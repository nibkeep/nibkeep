//! The state file: a space's state kept on disk in an encrypted database, read as needed
//! ([technical design: the state file]).
//!
//! [technical design: the state file]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#75-the-state-file

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_crypto::Key;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::{Origin, Recorded, Scope, Statements};
use nibkeep_model::{FieldName, Qualifiers, Statement, Value};
use nibkeep_time::Timestamp;
use rusqlite::{Row, params};

use crate::index::{Indexed, Postings};
use crate::{EncryptedDatabase, StoreError};

/// The version of the state file's format, which SQLite keeps as the database's
/// `user_version`. A new file has version 0 until its tables are made.
const FORMAT: i64 = 1;

/// How much of the state file SQLite keeps in memory, in KiB, taken as it is used: SQLite's
/// default of 2,000 KiB holds the index of statement IDs of some 20,000 statements, and
/// building the indexes of the search anew, as a purge or a field whose last writer wins
/// does, looks each statement up by ID and reads that index from the file over and over
/// once it has outgrown the cache. With 32 MiB, building them anew for 10,007 entities took
/// a third of the time, listing them a fifth less, and writing changes was as quick as
/// before, since a change's pages are written once at its commit whatever the cache holds.
const CACHE_KIB: i64 = 32 * 1024;

/// How much of the state file SQLite may keep in memory while a transaction writes it, in
/// KiB: the pages a transaction changed stay in the cache until it ends. Before entities were
/// numbered, a batch of 1,000 changes to entities spread through a file of 112 MB filled 32
/// MiB with them, and SQLite then read every other page from the file again each time it was
/// needed, 2.4 million reads in an import of 49,638 entities; with 128 MiB, it read 508. With
/// entities numbered, importing 119,645 entities and credits on a Windows laptop took 76
/// seconds with 128 MiB and 124 with 32, and the importing process peaked at 260 MB either
/// way. The cache is taken only as it is used, and shrinks back to [`CACHE_KIB`] when the
/// transaction ends.
const WRITING_CACHE_KIB: i64 = 128 * 1024;

/// How many pages the write-ahead log of the state file holds before a commit copies them
/// into the database, SQLite's checkpoint: ten times SQLite's default, so that a page that
/// many batches change is copied, and the database synchronized, a tenth as often. The
/// import above took 94 seconds with SQLite's default.
const CHECKPOINT_PAGES: i64 = 10_000;

/// The tables and indexes of the state file.
///
/// Statements are numbered as they come, and the indexes of the search refer to them by
/// those numbers, which are small and in the order of writing, where IDs are 16 random
/// bytes: rows go to the end of their tables rather than to random places, and the indexes
/// stay small; a statement's ID finds its number through one index. Entities are numbered
/// as the file first holds anything of them, and statements and indexes refer to them by
/// those numbers, so that the statements of entities written together are kept together in
/// the indexes too, rather than each in a random place. A statement's value and
/// qualifiers are kept in canonical CBOR, beside the columns that find it: its entity and
/// field, the entity its value refers to, if any, and when an operation about its entity
/// first superseded it, if one did. An operation that superseded a statement that had not
/// arrived, or that was held of another entity, or one of the earlier form, which names no
/// entity, is noted apart too, with the entity it named, so that a statement that arrives
/// later, or again with another entity, is superseded as the operations about its entity
/// say. So is every retraction of secret statements, which supersedes none of the
/// statements the file holds, all ordinary ones, and only secret statements opened beside
/// it: its entity is the single byte 1, which no entity's 16 bytes are. A change that
/// asserted a statement besides the one it is recorded by, as two devices that run one
/// migration at once both do, is noted too, by the statement's ID and the change's
/// multihash, so that a purge of the statement takes it as well.
const SCHEMA: &str = "
	CREATE TABLE entities (
		n INTEGER PRIMARY KEY,
		id BLOB NOT NULL UNIQUE
	);
	CREATE TABLE statements (
		n INTEGER PRIMARY KEY,
		id BLOB NOT NULL UNIQUE,
		entity INTEGER NOT NULL,
		field TEXT NOT NULL,
		reference INTEGER,
		recorded_millis INTEGER NOT NULL,
		recorded_counter INTEGER NOT NULL,
		restated_millis INTEGER,
		restated_counter INTEGER,
		origin BLOB,
		superseded_millis INTEGER,
		superseded_counter INTEGER,
		content BLOB NOT NULL
	);
	CREATE INDEX statements_by_field ON statements (entity, field, id);
	CREATE INDEX statements_by_reference ON statements (reference, field, id)
		WHERE reference IS NOT NULL;
	CREATE INDEX statements_in_field ON statements (field, entity, id);
	CREATE TABLE asserted_again (
		id BLOB NOT NULL,
		change BLOB NOT NULL,
		PRIMARY KEY (id, change)
	) WITHOUT ROWID;
	CREATE TABLE supersessions (
		id BLOB NOT NULL,
		entity BLOB NOT NULL,
		superseded_millis INTEGER NOT NULL,
		superseded_counter INTEGER NOT NULL,
		PRIMARY KEY (id, entity)
	) WITHOUT ROWID;
	CREATE TABLE mark (
		only INTEGER PRIMARY KEY NOT NULL CHECK (only = 0),
		sequence INTEGER NOT NULL,
		tag BLOB NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE indexed (
		statement INTEGER PRIMARY KEY,
		entity INTEGER NOT NULL,
		shown INTEGER NOT NULL,
		deleted INTEGER NOT NULL,
		field TEXT NOT NULL,
		value BLOB NOT NULL,
		words TEXT NOT NULL,
		name TEXT
	);
	CREATE INDEX indexed_by_value ON indexed (field, value);
	CREATE INDEX indexed_by_shown ON indexed (shown);
	CREATE INDEX indexed_by_name ON indexed (name) WHERE name IS NOT NULL;
	CREATE TABLE words (
		word TEXT NOT NULL,
		statement INTEGER NOT NULL,
		PRIMARY KEY (word, statement)
	) WITHOUT ROWID;
	CREATE TABLE applied (
		change BLOB PRIMARY KEY NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE indexed_fields (
		only INTEGER PRIMARY KEY NOT NULL CHECK (only = 0),
		secret TEXT NOT NULL,
		last_writer_wins TEXT NOT NULL
	) WITHOUT ROWID;
";

/// The columns that a recorded statement is read from, in the order [`recorded`] reads
/// them, from the tables of [`FROM_RECORDED`].
const RECORDED: &str = "s.id, e.id, s.field, s.content, s.recorded_millis, s.recorded_counter,
	s.restated_millis, s.restated_counter, s.origin";

/// The tables that a recorded statement is read from: the statements as `s`, and their
/// entities as `e`, looked up by number for each statement found. A number that no entity
/// has reads as null, which is damage, rather than leaving the statement out.
const FROM_RECORDED: &str = "FROM statements s LEFT JOIN entities e ON e.n = s.entity";

/// The columns that what is indexed of a statement is read from, in the order
/// [`indexed_row`] reads them, from the tables of [`FROM_INDEXED`].
const INDEXED: &str = "e.id, shown.id, i.deleted, i.field, i.value, i.words, i.name";

/// The tables that what is indexed of a statement is read from, named as [`INDEXED`] names
/// them, with the statement as `s`, and the entities it is of and shows under looked up by
/// number for each row found, as [`FROM_RECORDED`] looks them up.
const FROM_INDEXED: &str = "FROM indexed i JOIN statements s ON s.n = i.statement
	LEFT JOIN entities e ON e.n = i.entity
	LEFT JOIN entities shown ON shown.n = i.shown";

/// How many numbers of entities a state file keeps in memory once it has looked them up:
/// some 3 MB. Looking an entity's number up in the file made the queries of an entity's
/// fields, which search asks for each entity it finds, a quarter slower.
const NUMBERS_KEPT: usize = 1 << 16;

/// How far into the log file a state file goes: the sequence of the last row of the log
/// file that it covers, and that row's tag, which tells it from a later row that took its
/// sequence after it was deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
	/// The row's sequence.
	pub sequence: i64,
	/// The row's tag.
	pub tag: Vec<u8>,
}

/// A space's state on disk: the storage of a [`State`](nibkeep_model::State) in an
/// encrypted database of its own.
///
/// It is written only in transactions, which [`StateFile::begin`] and
/// [`StateFile::commit`] make, so that every operation is applied whole, and many, such as
/// the operations of many changes, are synchronized together. The first error in a
/// transaction ends it, leaving out what it wrote, so that no failure keeps the database's
/// write lock from other processes.
#[derive(Debug)]
pub struct StateFile {
	/// The database, unless it was closed to be made again, and could not be.
	database: Option<EncryptedDatabase>,
	path: PathBuf,
	/// Whether the file turned out damaged: it is then deleted when it is dropped, so that
	/// it is made again, unless it is made again before.
	damaged: Cell<bool>,
	/// Whether the transaction that has begun wrote anything.
	wrote: Cell<bool>,
	/// Whether the transaction that has begun only brings up to date what the file derives
	/// from itself, such as the indexes, as one that [`StateFile::begin_derived`] begins:
	/// if it fails, the file is as it was, and lacks nothing of the log file.
	derived: Cell<bool>,
	/// Whether a transaction that wrote, and not only what the file derives from itself,
	/// ended in an error, so that the file lacks what the log file holds: nothing is read or
	/// written until a transaction begins anew, which building the log again from the files
	/// does.
	lost: Cell<bool>,
	/// The numbers of entities looked up lately, which never change once committed. Those of
	/// a transaction, or a part of one, that is left out are forgotten with it, as it may
	/// have numbered them; and all of them once there are [`NUMBERS_KEPT`].
	numbers: RefCell<HashMap<EntityId, i64>>,
}

impl Drop for StateFile {
	fn drop(&mut self) {
		if self.damaged.get() {
			self.database = None;
			let _ = remove_files(&self.path);
		}
	}
}

impl StateFile {
	/// Opens the state file at `path` with `key`, making it if it does not exist. `rng`
	/// makes the file's own key and the nonces of what is written, so it must be a
	/// cryptographically secure generator, such as the operating system's.
	///
	/// # Errors
	///
	/// Returns [`StoreError::State`] if the file has an earlier format version, is damaged
	/// or is not one that `key` opens, [`StoreError::Newer`] if it has a later one, and the
	/// other errors of [`EncryptedDatabase::open`].
	pub fn open(
		path: &Path,
		key: &Key,
		rng: impl CryptoRng + Send + 'static,
	) -> Result<Self, StoreError> {
		Ok(Self {
			database: Some(open_database(path, key, rng)?),
			path: path.to_owned(),
			damaged: Cell::new(false),
			wrote: Cell::new(false),
			derived: Cell::new(false),
			lost: Cell::new(false),
			numbers: RefCell::default(),
		})
	}

	/// Opens the state file at `path` with `key`, as [`StateFile::open`] does, and makes it
	/// again, empty, if it cannot be used: if it is damaged, of an earlier format, or not one
	/// that `key` opens, such as one cut short while it was made. `rng` makes a generator
	/// for each try. Only a process that has the space to itself may do this.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Newer`] for a file of a later format, which stays as it is, and
	/// an error if the file cannot be opened or made again.
	pub fn open_or_make<R: CryptoRng + Send + 'static>(
		path: &Path,
		key: &Key,
		rng: impl Fn() -> R,
	) -> Result<Self, StoreError> {
		match Self::open(path, key, rng()) {
			Err(StoreError::State(_)) => {
				remove_files(path)?;
				Self::open(path, key, rng())
			}
			opened => opened,
		}
	}

	/// Deletes the state file and makes it again, empty, for example when it turned out
	/// to be damaged, or not to match the log file. Only a process that has the space to
	/// itself may do this.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be deleted or made again; the state file is
	/// then unusable until it is opened again.
	pub fn remake(
		&mut self,
		key: &Key,
		rng: impl CryptoRng + Send + 'static,
	) -> Result<(), StoreError> {
		// Closed before its files are deleted.
		self.database = None;
		remove_files(&self.path)?;
		self.damaged.set(false);
		self.wrote.set(false);
		self.derived.set(false);
		self.lost.set(false);
		self.numbers.borrow_mut().clear();
		self.database = Some(open_database(&self.path, key, rng)?);
		Ok(())
	}

	/// A number that changes whenever another connection, of this program or another, commits
	/// a change to the file: SQLite's `data_version`.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn data_version(&self) -> Result<i64, StoreError> {
		crate::file::data_version(self.connection()?).map_err(database_error)
	}

	/// Keeps the file when it is dropped, though it was found damaged: another program has
	/// the space open, and only one that has it alone may delete the file.
	pub(crate) fn keep(&self) {
		self.damaged.set(false);
	}

	/// Whether reading or writing found the file damaged since it was opened or made
	/// again: it is then made again before it is used for anything but reading, or when it
	/// is dropped.
	#[must_use]
	pub fn is_damaged(&self) -> bool {
		self.damaged.get()
	}

	/// The connection to the database.
	fn connection(&self) -> Result<&rusqlite::Connection, StoreError> {
		if self.lost.get() {
			return Err(StoreError::State(
				"a transaction that wrote ended in an error: the state file is built again \
				 from the log before the next change"
					.to_owned(),
			));
		}
		self.database
			.as_ref()
			.map(EncryptedDatabase::connection)
			.ok_or(StoreError::Broken)
	}

	/// How far into the log file the state file goes, if it covers any of it.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or is damaged.
	pub fn mark(&self) -> Result<Option<Mark>, StoreError> {
		Ok(self
			.query("SELECT sequence, tag FROM mark", [], |row| {
				Ok(Mark {
					sequence: row.get(0).map_err(database_error)?,
					tag: row.get(1).map_err(database_error)?,
				})
			})?
			.pop())
	}

	/// Records how far into the log file the state file goes, in the transaction that
	/// takes in the changes up to there.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Transaction`] outside a transaction, and an error if the
	/// database fails.
	pub fn set_mark(&mut self, mark: &Mark) -> Result<(), StoreError> {
		self.write(
			"INSERT OR REPLACE INTO mark (only, sequence, tag) VALUES (0, ?1, ?2)",
			params![mark.sequence, mark.tag],
		)
	}

	/// The hashes of the changes whose operations the state file holds.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or is damaged.
	pub fn applied(&self) -> Result<BTreeSet<Hash>, StoreError> {
		Ok(self
			.query("SELECT change FROM applied", [], |row| {
				let bytes: Vec<u8> = row.get(0).map_err(database_error)?;
				Hash::from_multihash(&bytes).map_err(|_| damaged("a change hash"))
			})?
			.into_iter()
			.collect())
	}

	/// Whether the state file holds the operations of the change whose hash is `change`.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or is damaged.
	pub fn has_applied(&self, change: &Hash) -> Result<bool, StoreError> {
		Ok(self.count(
			"SELECT count(*) FROM applied WHERE change = ?1",
			params![change.to_multihash()],
		)? > 0)
	}

	/// Records that the state file holds the operations of the change whose hash is
	/// `change`, in the transaction that applies them.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Transaction`] outside a transaction, and an error if the
	/// database fails.
	pub fn note_applied(&self, change: &Hash) -> Result<(), StoreError> {
		self.write(
			"INSERT OR IGNORE INTO applied (change) VALUES (?1)",
			params![change.to_multihash()],
		)
	}

	/// Begins a transaction, which holds the database's write lock until it ends, so that
	/// what it reads stays as it is.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or if a transaction has begun already.
	pub fn begin(&mut self) -> Result<(), StoreError> {
		self.begin_writing(false)
	}

	/// Begins a transaction, as [`StateFile::begin`] does, that only brings up to date what
	/// the file derives from itself, such as the indexes: if it fails, the file is as it was,
	/// and lacks nothing of the log file, so it can still be read.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or if a transaction has begun already.
	pub(crate) fn begin_derived(&mut self) -> Result<(), StoreError> {
		self.begin_writing(true)
	}

	/// Begins a transaction that writes, and notes whether it only writes what the file
	/// derives from itself. Only one that may take in what the log holds lets a file that
	/// lacks some of it be read again: what the file derives from itself cannot make up for
	/// it.
	fn begin_writing(&mut self, derived: bool) -> Result<(), StoreError> {
		if self.in_transaction() {
			return Err(StoreError::Transaction);
		}
		if !derived {
			self.lost.set(false);
		}
		self.wrote.set(false);
		self.derived.set(derived);
		self.cache(WRITING_CACHE_KIB);
		let begun = self.execute("BEGIN IMMEDIATE");
		if begun.is_err() {
			self.cache(CACHE_KIB);
		}
		begun
	}

	/// Lets SQLite keep up to `kib` KiB of the file in memory, freeing what is beyond. A
	/// cache of another size costs memory or time, never data, so a failure is let pass.
	fn cache(&self, kib: i64) {
		if let Ok(connection) = self.connection() {
			let _ = connection.pragma_update(None, "cache_size", -kib);
		}
	}

	/// Notes that the transaction ended in an error, here or in SQLite: if it wrote, and not
	/// only what the file derives from itself, the file lacks what it took in, and the next
	/// transaction has to begin anew.
	fn note_ended(&self) {
		if self.wrote.get() && !self.derived.get() {
			self.lost.set(true);
		}
		self.wrote.set(false);
		self.derived.set(false);
	}

	/// Whether a transaction has begun and not ended.
	#[must_use]
	pub fn in_transaction(&self) -> bool {
		self.database
			.as_ref()
			.is_some_and(|database| !database.connection().is_autocommit())
	}

	/// Commits the transaction, and synchronizes the database.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or if no transaction has begun.
	pub fn commit(&self) -> Result<(), StoreError> {
		// An error ends the transaction too, as `ending` notes.
		let committed = self.execute("COMMIT");
		if committed.is_ok() {
			self.wrote.set(false);
			self.derived.set(false);
			self.cache(CACHE_KIB);
		}
		committed
	}

	/// Runs `read` in one transaction that only reads, so that its queries take the
	/// database's lock once, not each on its own, and all see it as it was when the first
	/// began. In a transaction begun already, `read` just runs.
	///
	/// # Errors
	///
	/// Returns the error of `read`, or an error if the database fails.
	pub(crate) fn reading<T, E: From<StoreError>>(
		&self,
		read: impl FnOnce() -> Result<T, E>,
	) -> Result<T, E> {
		if self.in_transaction() {
			return read();
		}
		self.execute("BEGIN DEFERRED")?;
		let result = read();
		// An error of the database may have ended the transaction already.
		if self.in_transaction() {
			let ended = self.execute("COMMIT");
			if result.is_ok() {
				ended?;
			}
		}
		result
	}

	/// Leaves out what was written since the transaction began, and ends it.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or if no transaction has begun.
	pub fn rollback(&mut self) -> Result<(), StoreError> {
		self.wrote.set(false);
		self.derived.set(false);
		self.numbers.borrow_mut().clear();
		let rolled_back = self.execute("ROLLBACK");
		self.cache(CACHE_KIB);
		rolled_back
	}

	/// Marks where a part of the transaction that has begun starts, so that it can be left
	/// out alone.
	pub(crate) fn savepoint(&mut self, which: Savepoint) -> Result<(), StoreError> {
		self.writing()?;
		self.execute(which.begin())
	}

	/// Keeps what was written since the savepoint, in the transaction.
	pub(crate) fn release(&mut self, which: Savepoint) -> Result<(), StoreError> {
		self.execute(which.release())
	}

	/// Leaves out what was written since the savepoint, and keeps the transaction.
	pub(crate) fn rollback_to_savepoint(&mut self, which: Savepoint) -> Result<(), StoreError> {
		self.numbers.borrow_mut().clear();
		self.execute(which.rollback())
	}

	fn execute(&self, sql: &str) -> Result<(), StoreError> {
		self.connection()?
			.execute_batch(sql)
			.map_err(|error| self.ending(database_error(error)))
	}

	/// Ends the transaction, if one has begun, after an error in it, notes damage, and
	/// returns the error.
	fn ending(&self, error: StoreError) -> StoreError {
		// A file that lacks what the log file holds refuses to be read, which is no damage.
		if matches!(error, StoreError::State(_)) && !self.lost.get() {
			self.damaged.set(true);
		}
		if self.in_transaction()
			&& let Ok(connection) = self.connection()
		{
			// The error that ended the transaction is the one to report; rolling back fails
			// only if SQLite has already ended it.
			let _ = connection.execute_batch("ROLLBACK");
		}
		// Before the file may be noted as lacking what it took in, which keeps it from being
		// used.
		self.cache(CACHE_KIB);
		self.numbers.borrow_mut().clear();
		// Whether the transaction ended here or SQLite ended it.
		self.note_ended();
		error
	}

	/// Checks that a write is in a transaction.
	fn writing(&self) -> Result<(), StoreError> {
		if self.in_transaction() {
			Ok(())
		} else {
			Err(StoreError::Transaction)
		}
	}

	/// Whether a query with `parameters` finds a row.
	fn exists(&self, sql: &str, parameters: impl rusqlite::Params) -> Result<bool, StoreError> {
		self.connection()?
			.prepare_cached(sql)
			.and_then(|mut statement| statement.exists(parameters))
			.map_err(|error| self.ending(database_error(error)))
	}

	/// Runs a query with `parameters`, and collects what `read` makes of each row.
	fn query<T>(
		&self,
		sql: &str,
		parameters: impl rusqlite::Params,
		read: impl Fn(&Row<'_>) -> Result<T, StoreError>,
	) -> Result<Vec<T>, StoreError> {
		let rows = || {
			let mut statement = self
				.connection()?
				.prepare_cached(sql)
				.map_err(database_error)?;
			let mut rows = statement.query(parameters).map_err(database_error)?;
			let mut found = Vec::new();
			while let Some(row) = rows.next().map_err(database_error)? {
				found.push(read(row)?);
			}
			Ok(found)
		};
		rows().map_err(|error| self.ending(error))
	}

	/// Runs a statement that writes, with `parameters`, and tells how many rows it changed.
	fn write_counting(
		&self,
		sql: &str,
		parameters: impl rusqlite::Params,
	) -> Result<usize, StoreError> {
		self.writing()?;
		self.wrote.set(true);
		self.connection()?
			.prepare_cached(sql)
			.and_then(|mut statement| statement.execute(parameters))
			.map_err(|error| self.ending(database_error(error)))
	}

	/// Runs a statement that writes, with `parameters`.
	fn write(&self, sql: &str, parameters: impl rusqlite::Params) -> Result<(), StoreError> {
		self.write_counting(sql, parameters).map(|_| ())
	}

	/// Counts the rows of a query with `parameters`.
	fn count(&self, sql: &str, parameters: impl rusqlite::Params) -> Result<usize, StoreError> {
		let counts = self.query(sql, parameters, |row| {
			let count: i64 = row.get(0).map_err(database_error)?;
			usize::try_from(count).map_err(|_| damaged("a count is out of range"))
		})?;
		Ok(counts.into_iter().next().unwrap_or(0))
	}

	/// The number of the entity with this ID, which it gets when the file first holds
	/// anything of it, in a transaction that writes.
	fn entity_number(&self, entity: EntityId) -> Result<i64, StoreError> {
		if let Some(number) = self.held_number(entity)? {
			return Ok(number);
		}
		self.write("INSERT INTO entities (id) VALUES (?1)", [entity.as_bytes()])?;
		let number = self.connection()?.last_insert_rowid();
		self.keep_number(entity, number);
		Ok(number)
	}

	/// The number of the entity with this ID, if the file holds anything of it: as a
	/// parameter, `None` is null, which a query finds nothing equal to.
	fn held_number(&self, entity: EntityId) -> Result<Option<i64>, StoreError> {
		if let Some(&number) = self.numbers.borrow().get(&entity) {
			return Ok(Some(number));
		}
		let held = self
			.query(
				"SELECT n FROM entities WHERE id = ?1",
				[entity.as_bytes()],
				|row| row.get(0).map_err(database_error),
			)?
			.pop();
		if let Some(number) = held {
			self.keep_number(entity, number);
		}
		Ok(held)
	}

	fn keep_number(&self, entity: EntityId, number: i64) {
		let mut numbers = self.numbers.borrow_mut();
		if numbers.len() >= NUMBERS_KEPT {
			numbers.clear();
		}
		numbers.insert(entity, number);
	}

	/// The number of the statement with this ID, if the state file holds one.
	pub(crate) fn statement_number(&self, id: StatementId) -> Result<Option<i64>, StoreError> {
		Ok(self
			.query(
				"SELECT n FROM statements WHERE id = ?1",
				[id.as_bytes()],
				|row| row.get(0).map_err(database_error),
			)?
			.pop())
	}
}

/// The savepoints of a transaction of the state file: the change being made, in a batch or
/// in a burst of changes, and the indexes built again, which nests in it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Savepoint {
	Change,
	Indexes,
}

impl Savepoint {
	const fn begin(self) -> &'static str {
		match self {
			Self::Change => "SAVEPOINT change",
			Self::Indexes => "SAVEPOINT indexes",
		}
	}

	const fn release(self) -> &'static str {
		match self {
			Self::Change => "RELEASE change",
			Self::Indexes => "RELEASE indexes",
		}
	}

	const fn rollback(self) -> &'static str {
		match self {
			Self::Change => "ROLLBACK TO change; RELEASE change",
			Self::Indexes => "ROLLBACK TO indexes; RELEASE indexes",
		}
	}
}

/// SQLite's extended code for a read that found the data damaged, which the VFS gives when
/// a page does not authenticate.
const IOERR_DATA: i32 = rusqlite::ffi::SQLITE_IOERR_DATA;

/// What a failure of the database means for the state file: that it is damaged or not
/// one that its key opens, which building it again answers, or that the database failed.
fn database_error(error: rusqlite::Error) -> StoreError {
	match &error {
		rusqlite::Error::SqliteFailure(failure, _)
			if matches!(
				failure.code,
				rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
			) || failure.extended_code == IOERR_DATA =>
		{
			damaged("SQLite found it damaged, or its key does not open it")
		}
		rusqlite::Error::InvalidColumnType(..)
		| rusqlite::Error::FromSqlConversionFailure(..)
		| rusqlite::Error::IntegralValueOutOfRange(..) => {
			damaged("a column holds a value of another type")
		}
		_ => StoreError::Database(error),
	}
}

/// Opens the encrypted database of a state file, making its tables if it is new.
fn open_database(
	path: &Path,
	key: &Key,
	rng: impl CryptoRng + Send + 'static,
) -> Result<EncryptedDatabase, StoreError> {
	// A file cut short within its first page reads as empty, and SQLite cannot make it again
	// over what is left: it is damaged.
	let torn = |error: rusqlite::Error| {
		let cut = error.sqlite_error_code() == Some(rusqlite::ErrorCode::CannotOpen)
			&& std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0);
		if cut {
			damaged("SQLite cannot open what is left of it")
		} else {
			database_error(error)
		}
	};
	let mut database = EncryptedDatabase::open(path, key, rng).map_err(|error| match error {
		StoreError::Database(error) => torn(error),
		// Pages of another size, for example.
		StoreError::Damaged(_) => damaged("its pages are not as the VFS stores them"),
		error => error,
	})?;
	let transaction = database
		.connection_mut()
		.transaction()
		.map_err(database_error)?;
	let version: i64 = transaction
		.pragma_query_value(None, "user_version", |row| row.get(0))
		.map_err(database_error)?;
	match version {
		FORMAT => {}
		0 => {
			let tables: i64 = transaction
				.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
				.map_err(database_error)?;
			if tables > 0 {
				return Err(damaged("it has tables but no format version"));
			}
			transaction.execute_batch(SCHEMA).map_err(database_error)?;
			transaction
				.pragma_update(None, "user_version", FORMAT)
				.map_err(database_error)?;
		}
		// Only a newer program reads it, which may share the replica: it is never made anew.
		version if version > FORMAT => return Err(StoreError::Newer),
		_ => {
			return Err(StoreError::State(format!(
				"it has format version {version}, not {FORMAT}"
			)));
		}
	}
	transaction.commit().map_err(database_error)?;
	// A commit that a power cut loses is taken in again from the log file, so the state
	// file need not wait for the disk at every commit: the write-ahead log keeps it
	// consistent, possibly behind.
	database
		.connection()
		.pragma_update(None, "synchronous", "NORMAL")
		.map_err(database_error)?;
	database
		.connection()
		.pragma_update(None, "cache_size", -CACHE_KIB)
		.map_err(database_error)?;
	database
		.connection()
		.pragma_update(None, "wal_autocheckpoint", CHECKPOINT_PAGES)
		.map_err(database_error)?;
	Ok(database)
}

/// Deletes a state file and its write-ahead log and shared memory, if they are there.
fn remove_files(path: &Path) -> Result<(), StoreError> {
	for suffix in ["", "-wal", "-shm"] {
		let mut name = path.as_os_str().to_owned();
		name.push(suffix);
		match std::fs::remove_file(PathBuf::from(name)) {
			Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
			_ => {}
		}
	}
	Ok(())
}

/// A statement ID from a column.
fn statement_id(row: &Row<'_>, column: usize) -> Result<StatementId, StoreError> {
	Ok(StatementId::from_bytes(sixteen(row, column)?))
}

/// An entity ID from a column.
fn entity_id(row: &Row<'_>, column: usize) -> Result<EntityId, StoreError> {
	Ok(EntityId::from_bytes(sixteen(row, column)?))
}

fn sixteen(row: &Row<'_>, column: usize) -> Result<[u8; 16], StoreError> {
	let bytes: Vec<u8> = row.get(column).map_err(database_error)?;
	bytes
		.try_into()
		.map_err(|_| damaged("an ID is not 16 bytes"))
}

/// A field name from a column.
fn field_name(row: &Row<'_>, column: usize) -> Result<FieldName, StoreError> {
	let name: String = row.get(column).map_err(database_error)?;
	name.parse()
		.map_err(|_| damaged("a field name is not valid"))
}

/// A statement's value and qualifiers as stored: the CBOR array `[value, qualifiers]`.
fn content(statement: &Statement) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![
		statement.value.to_cbor(),
		statement.qualifiers.to_cbor(),
	]))
}

/// A recorded statement from the columns [`RECORDED`], starting at `column`.
fn recorded(row: &Row<'_>, column: usize) -> Result<Recorded<'static>, StoreError> {
	let id = statement_id(row, column)?;
	let entity = entity_id(row, column + 1)?;
	let field = field_name(row, column + 2)?;
	let bytes: Vec<u8> = row.get(column + 3).map_err(database_error)?;
	let unreadable = || damaged("a statement cannot be read");
	let Cbor::Array(items) = cbor::from_slice::<Cbor>(&bytes).map_err(|_| unreadable())? else {
		return Err(unreadable());
	};
	let [value, qualifiers] = items.as_slice() else {
		return Err(unreadable());
	};
	let value = Value::from_cbor(value).map_err(|_| unreadable())?;
	let qualifiers = Qualifiers::from_cbor(qualifiers).map_err(|_| unreadable())?;
	// The milliseconds are kept as the bits of a signed number, which SQLite's integers are.
	let millis: i64 = row.get(column + 4).map_err(database_error)?;
	let counter: u32 = row.get(column + 5).map_err(database_error)?;
	let restated_millis: Option<i64> = row.get(column + 6).map_err(database_error)?;
	let restated_counter: Option<u32> = row.get(column + 7).map_err(database_error)?;
	let restated = match (restated_millis, restated_counter) {
		(Some(millis), Some(counter)) => Some(Timestamp::new(millis.cast_unsigned(), counter)),
		(None, None) => None,
		_ => return Err(damaged("a statement is restated at half a time")),
	};
	let origin: Option<Vec<u8>> = row.get(column + 8).map_err(database_error)?;
	let change = match origin {
		Some(bytes) => {
			Some(Hash::from_multihash(&bytes).map_err(|_| damaged("a hash is not valid"))?)
		}
		None => None,
	};
	Ok(Recorded {
		statement: Cow::Owned(Statement::new(id, entity, field, value).with_qualifiers(qualifiers)),
		at: Timestamp::new(millis.cast_unsigned(), counter),
		restated,
		change,
	})
}

/// Milliseconds as SQLite keeps them where it compares them: their bits with the highest
/// flipped, so that SQLite's signed integers order as the milliseconds do.
const fn ordered(millis: u64) -> i64 {
	(millis ^ (1 << 63)).cast_signed()
}

/// The milliseconds that [`ordered`] made.
const fn from_ordered(stored: i64) -> u64 {
	stored.cast_unsigned() ^ (1 << 63)
}

/// A time an operation superseded a statement at, from two columns, if they are not null.
fn superseded_time(row: &Row<'_>, column: usize) -> Result<Option<Timestamp>, StoreError> {
	let millis: Option<i64> = row.get(column).map_err(database_error)?;
	let counter: Option<u32> = row.get(column + 1).map_err(database_error)?;
	match (millis, counter) {
		(Some(millis), Some(counter)) => Ok(Some(Timestamp::new(from_ordered(millis), counter))),
		(None, None) => Ok(None),
		_ => Err(damaged("a statement is superseded at half a time")),
	}
}

fn damaged(what: &str) -> StoreError {
	StoreError::State(format!("it is damaged: {what}"))
}

impl Statements for StateFile {
	type Error = StoreError;

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, StoreError> {
		Ok(self
			.query(
				&format!("SELECT {RECORDED} {FROM_RECORDED} WHERE s.id = ?1"),
				[id.as_bytes()],
				|row| recorded(row, 0),
			)?
			.pop())
	}

	fn contains(&self, id: StatementId) -> Result<bool, StoreError> {
		self.exists("SELECT 1 FROM statements WHERE id = ?1", [id.as_bytes()])
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<Hash>, StoreError> {
		let mut changes = self.query(
			"SELECT change FROM asserted_again WHERE id = ?1",
			[id.as_bytes()],
			|row| {
				let bytes: Vec<u8> = row.get(0).map_err(database_error)?;
				Hash::from_multihash(&bytes).map_err(|_| damaged("a hash is not valid"))
			},
		)?;
		changes.sort();
		Ok(changes)
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, StoreError> {
		self.query(
			&format!("SELECT {RECORDED} {FROM_RECORDED} ORDER BY s.id"),
			[],
			|row| recorded(row, 0),
		)
	}

	fn is_superseded(&self, id: StatementId) -> Result<bool, StoreError> {
		// One query, for a statement held or one superseded before it arrived.
		self.exists(
			"SELECT 1 FROM statements WHERE id = ?1 AND superseded_millis IS NOT NULL
				UNION ALL
				SELECT 1 FROM supersessions WHERE id = ?1
					AND NOT EXISTS (SELECT 1 FROM statements WHERE id = ?1)",
			[id.as_bytes()],
		)
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, StoreError> {
		let held = self.query(
			"SELECT superseded_millis, superseded_counter FROM statements WHERE id = ?1",
			[id.as_bytes()],
			|row| superseded_time(row, 0),
		)?;
		if let Some(superseded) = held.into_iter().next() {
			return Ok(superseded);
		}
		// Not arrived yet: superseded by any operation, if at all.
		Ok(self
			.query(
				"SELECT superseded_millis, superseded_counter FROM supersessions WHERE id = ?1
					ORDER BY superseded_millis, superseded_counter LIMIT 1",
				[id.as_bytes()],
				|row| superseded_time(row, 0),
			)?
			.pop()
			.flatten())
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, StoreError> {
		Ok(self
			.query(
				"SELECT superseded_millis, superseded_counter FROM supersessions
					WHERE id = ?1 AND (entity = X'' OR entity = ?2 OR (?3 AND entity = ?4))
					ORDER BY superseded_millis, superseded_counter LIMIT 1",
				params![id.as_bytes(), entity.as_bytes(), secret, SECRET_SCOPE],
				|row| superseded_time(row, 0),
			)?
			.pop()
			.flatten())
	}

	fn of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<StatementId>, StoreError> {
		self.query(
			"SELECT id FROM statements WHERE entity = ?1 AND field = ?2 ORDER BY id",
			params![self.held_number(entity)?, field.as_str()],
			|row| statement_id(row, 0),
		)
	}

	fn of_entity(
		&self,
		entity: EntityId,
	) -> Result<Vec<(FieldName, Vec<StatementId>)>, StoreError> {
		let rows = self.query(
			"SELECT field, id FROM statements WHERE entity = ?1 ORDER BY field, id",
			[self.held_number(entity)?],
			|row| {
				let field: String = row.get(0).map_err(database_error)?;
				Ok((field, statement_id(row, 1)?))
			},
		)?;
		let mut fields: Vec<(FieldName, Vec<StatementId>)> = Vec::new();
		for (field, id) in rows {
			match fields.last_mut() {
				Some((last, ids)) if last.as_str() == field => ids.push(id),
				_ => {
					let name = field
						.parse()
						.map_err(|_| self.ending(damaged("a field name is not valid")))?;
					fields.push((name, vec![id]));
				}
			}
		}
		Ok(fields)
	}

	fn current_fields(&self, entity: EntityId) -> Result<Vec<FieldName>, StoreError> {
		self.query(
			"SELECT DISTINCT field FROM statements
				WHERE entity = ?1 AND superseded_millis IS NULL
				ORDER BY field",
			[self.held_number(entity)?],
			|row| field_name(row, 0),
		)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, StoreError> {
		self.query(
			"SELECT id FROM statements WHERE reference = ?1 ORDER BY id",
			[self.held_number(entity)?],
			|row| statement_id(row, 0),
		)
	}

	fn current_of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, StoreError> {
		self.query(
			&format!(
				"SELECT {RECORDED} {FROM_RECORDED}
					WHERE s.entity = ?1 AND s.field = ?2 AND s.superseded_millis IS NULL
					ORDER BY s.id"
			),
			params![self.held_number(entity)?, field.as_str()],
			|row| recorded(row, 0),
		)
	}

	fn current_referring_to(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, StoreError> {
		self.query(
			&format!(
				"SELECT {RECORDED} {FROM_RECORDED}
					WHERE s.reference = ?1 AND s.field = ?2 AND s.superseded_millis IS NULL
					ORDER BY s.id"
			),
			params![self.held_number(entity)?, field.as_str()],
			|row| recorded(row, 0),
		)
	}

	fn current_in_field(&self, field: &FieldName) -> Result<Vec<Recorded<'_>>, StoreError> {
		self.query(
			&format!(
				"SELECT {RECORDED} {FROM_RECORDED}
					WHERE s.field = ?1 AND s.superseded_millis IS NULL
					ORDER BY s.id"
			),
			[field.as_str()],
			|row| recorded(row, 0),
		)
	}

	fn current_in_range(
		&self,
		first: EntityId,
		last: EntityId,
		fields: &[FieldName],
	) -> Result<Vec<Recorded<'_>>, StoreError> {
		if fields.is_empty() {
			return Ok(Vec::new());
		}
		// The entities, found by the index of their IDs, then their rows, found by the index
		// of entities and fields, and of them those of the fields asked for.
		let asked: Vec<String> = (3..fields.len() + 3).map(|n| format!("?{n}")).collect();
		let mut parameters: Vec<rusqlite::types::Value> = vec![
			first.as_bytes().to_vec().into(),
			last.as_bytes().to_vec().into(),
		];
		parameters.extend(fields.iter().map(|field| field.as_str().to_owned().into()));
		self.query(
			&format!(
				"SELECT {RECORDED} FROM entities e CROSS JOIN statements s ON s.entity = e.n
					WHERE e.id BETWEEN ?1 AND ?2 AND s.field IN ({})
						AND s.superseded_millis IS NULL
					ORDER BY s.id",
				asked.join(", ")
			),
			rusqlite::params_from_iter(parameters),
			|row| recorded(row, 0),
		)
	}

	fn current_statements(&self) -> Result<Vec<Recorded<'_>>, StoreError> {
		self.query(
			&format!(
				"SELECT {RECORDED} {FROM_RECORDED} WHERE s.superseded_millis IS NULL ORDER BY s.id"
			),
			[],
			|row| recorded(row, 0),
		)
	}

	fn entities(&self) -> Result<Vec<EntityId>, StoreError> {
		self.query(
			"SELECT id FROM entities e
				WHERE EXISTS (
					SELECT 1 FROM statements WHERE entity = e.n AND superseded_millis IS NULL
				)
				ORDER BY id",
			[],
			|row| entity_id(row, 0),
		)
	}

	fn field_names(&self) -> Result<Vec<FieldName>, StoreError> {
		// Each field after the last by one look into the index of fields, rather than a
		// walk through every statement.
		self.query(
			"WITH RECURSIVE fields(name) AS (
				SELECT min(field) FROM statements
				UNION ALL
				SELECT (SELECT min(field) FROM statements WHERE field > name) FROM fields
					WHERE name IS NOT NULL
			)
			SELECT name FROM fields WHERE name IS NOT NULL ORDER BY name",
			[],
			|row| field_name(row, 0),
		)
	}

	fn described(&self) -> Result<Vec<EntityId>, StoreError> {
		self.query(
			"SELECT id FROM entities e
				WHERE EXISTS (SELECT 1 FROM statements WHERE entity = e.n)
				ORDER BY id",
			[],
			|row| entity_id(row, 0),
		)
	}

	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), StoreError> {
		self.writing()?;
		let entity = self.entity_number(statement.entity)?;
		let reference = match statement.value.as_reference() {
			Some(reference) => Some(self.entity_number(reference)?),
			None => None,
		};
		let Origin {
			at,
			restated,
			change,
		} = origin;
		// A statement is superseded from the earliest operation about its entity, or of the
		// earlier form, that superseded it, before or after it arrived; one asserted anew
		// keeps the time it has, and one that comes again with another entity takes the time
		// of the operations kept apart about that one.
		self.write(
			"INSERT INTO statements
				(id, entity, field, reference, recorded_millis, recorded_counter,
					restated_millis, restated_counter, origin,
					superseded_millis, superseded_counter, content)
				VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
					(SELECT superseded_millis FROM supersessions
						WHERE id = ?1 AND (entity = X'' OR entity = ?11)
						ORDER BY superseded_millis, superseded_counter LIMIT 1),
					(SELECT superseded_counter FROM supersessions
						WHERE id = ?1 AND (entity = X'' OR entity = ?11)
						ORDER BY superseded_millis, superseded_counter LIMIT 1),
					?10)
				ON CONFLICT (id) DO UPDATE SET
					entity = excluded.entity,
					field = excluded.field,
					reference = excluded.reference,
					recorded_millis = excluded.recorded_millis,
					recorded_counter = excluded.recorded_counter,
					restated_millis = excluded.restated_millis,
					restated_counter = excluded.restated_counter,
					origin = excluded.origin,
					superseded_millis = CASE WHEN entity = excluded.entity
						THEN superseded_millis ELSE excluded.superseded_millis END,
					superseded_counter = CASE WHEN entity = excluded.entity
						THEN superseded_counter ELSE excluded.superseded_counter END,
					content = excluded.content",
			params![
				statement.id.as_bytes(),
				entity,
				statement.field.as_str(),
				reference,
				at.millis().cast_signed(),
				at.counter(),
				restated.map(|restated| restated.millis().cast_signed()),
				restated.map(Timestamp::counter),
				change.map(|change| change.to_multihash()),
				content(statement),
				statement.entity.as_bytes(),
			],
		)
	}

	fn assert_again(&mut self, id: StatementId, change: Hash) -> Result<(), StoreError> {
		self.write(
			"INSERT OR IGNORE INTO asserted_again (id, change) VALUES (?1, ?2)",
			params![id.as_bytes(), change.to_multihash()],
		)
	}

	fn supersede(
		&mut self,
		scope: Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), StoreError> {
		self.writing()?;
		// An empty blob stands for an operation of the earlier form, which names no entity.
		let named: &[u8] = match &scope {
			Scope::Entity(entity) => entity.as_bytes(),
			Scope::Any => &[],
			Scope::Secret => SECRET_SCOPE,
		};
		// Kept apart whatever the file holds: one of the earlier form, which holds for any
		// statement, and a retraction of secret statements, which holds for none it holds.
		let apart = !matches!(scope, Scope::Entity(_));
		for id in ids {
			// A statement held of that entity is superseded from the earliest time.
			let updated = if scope == Scope::Secret {
				0
			} else {
				self.write_counting(
					"UPDATE statements SET superseded_millis = ?2, superseded_counter = ?3
						WHERE id = ?1
							AND (?4 = X'' OR entity = (SELECT n FROM entities WHERE id = ?4))
							AND (superseded_millis IS NULL
								OR (superseded_millis, superseded_counter) > (?2, ?3))",
					params![id.as_bytes(), ordered(at.millis()), at.counter(), named],
				)?
			};
			// Kept apart, with the earliest time of each scope, unless it holds for a
			// statement held of that entity alone: one that has not arrived, and one held of
			// another entity, which a statement of this one with its ID may replace.
			if updated == 0 || apart {
				self.write(
					"INSERT INTO supersessions (id, entity, superseded_millis, superseded_counter)
						SELECT ?1, ?4, ?2, ?3
							WHERE ?5 OR NOT EXISTS (
								SELECT 1 FROM statements
									WHERE id = ?1
										AND entity = (SELECT n FROM entities WHERE id = ?4))
						ON CONFLICT (id, entity) DO UPDATE SET
							superseded_millis = excluded.superseded_millis,
							superseded_counter = excluded.superseded_counter
						WHERE (excluded.superseded_millis, excluded.superseded_counter)
							< (superseded_millis, superseded_counter)",
					params![
						id.as_bytes(),
						ordered(at.millis()),
						at.counter(),
						named,
						apart
					],
				)?;
			}
		}
		Ok(())
	}
}

/// What the table `supersessions` holds as the entity of a retraction of secret statements:
/// one byte, which no entity's 16 bytes are, nor the empty blob of the earlier form.
const SECRET_SCOPE: &[u8] = &[1];

/// The fields that a space treats specially: those that are secret, and those whose last
/// writer wins.
pub(crate) type SpecialFields = (BTreeSet<FieldName>, BTreeSet<FieldName>);

/// The fields an index was built with, as text: their names, one on each line.
fn field_list(fields: &BTreeSet<FieldName>) -> String {
	fields
		.iter()
		.map(FieldName::as_str)
		.collect::<Vec<_>>()
		.join("\n")
}

/// The fields of a list that [`field_list`] wrote.
fn parse_field_list(list: &str) -> Result<BTreeSet<FieldName>, StoreError> {
	list.lines()
		.map(|name| {
			name.parse()
				.map_err(|_| damaged("a field name is not valid"))
		})
		.collect()
}

/// The words of a statement as stored: separated by spaces, which no word holds.
fn parse_words(words: &str) -> Vec<String> {
	words
		.split(' ')
		.filter(|word| !word.is_empty())
		.map(str::to_owned)
		.collect()
}

/// The first text after every text that starts with `prefix`: the upper end of a range of
/// words that start with it, as SQLite compares text by its bytes. No word holds the
/// character that ends it, which is not one.
fn after_prefix(prefix: &str) -> String {
	format!("{prefix}\u{10FFFF}")
}

/// What is indexed of a statement, from the columns [`INDEXED`], starting at `column`.
fn indexed_row(row: &Row<'_>, column: usize) -> Result<Indexed, StoreError> {
	let deleted: bool = row.get(column + 2).map_err(database_error)?;
	let value: Vec<u8> = row.get(column + 4).map_err(database_error)?;
	let words: String = row.get(column + 5).map_err(database_error)?;
	let name: Option<String> = row.get(column + 6).map_err(database_error)?;
	Ok(Indexed {
		entity: entity_id(row, column)?,
		shown: entity_id(row, column + 1)?,
		deleted,
		words: parse_words(&words),
		name,
		value: (
			field_name(row, column + 3)?,
			Hash::from_multihash(&value).map_err(|_| damaged("a hash is not valid"))?,
		),
	})
}

impl StateFile {
	/// The secret fields and the fields whose last writer wins that the indexes were built
	/// with, if they were built.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or is damaged.
	pub(crate) fn indexed_fields(&self) -> Result<Option<SpecialFields>, StoreError> {
		let lists = self.query(
			"SELECT secret, last_writer_wins FROM indexed_fields",
			[],
			|row| {
				let secret: String = row.get(0).map_err(database_error)?;
				let last: String = row.get(1).map_err(database_error)?;
				Ok((secret, last))
			},
		)?;
		match lists.into_iter().next() {
			Some((secret, last)) => {
				Ok(Some((parse_field_list(&secret)?, parse_field_list(&last)?)))
			}
			None => Ok(None),
		}
	}

	/// Records the fields the indexes were built with, in the transaction that built them.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Transaction`] outside a transaction, and an error if the
	/// database fails.
	pub(crate) fn set_indexed_fields(
		&self,
		secret: &BTreeSet<FieldName>,
		last_writer_wins: &BTreeSet<FieldName>,
	) -> Result<(), StoreError> {
		self.write(
			"INSERT OR REPLACE INTO indexed_fields (only, secret, last_writer_wins)
				VALUES (0, ?1, ?2)",
			params![field_list(secret), field_list(last_writer_wins)],
		)
	}

	/// Forgets which fields the indexes were built with, in a transaction that changes the
	/// state without them: they are then built again.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Transaction`] outside a transaction, and an error if the
	/// database fails.
	pub(crate) fn forget_indexed_fields(&self) -> Result<(), StoreError> {
		self.write("DELETE FROM indexed_fields", [])
	}
}

impl Postings for StateFile {
	type Error = StoreError;
	/// A statement's number in the file, so that a search reads only the indexes, never the
	/// rows of the statements, which hold their values.
	type Key = i64;

	fn indexed(&self, id: StatementId) -> Result<Option<Indexed>, StoreError> {
		Ok(self
			.query(
				&format!("SELECT {INDEXED} {FROM_INDEXED} WHERE s.id = ?1"),
				[id.as_bytes()],
				|row| indexed_row(row, 0),
			)?
			.pop())
	}

	fn insert(&self, id: StatementId, indexed: &Indexed) -> Result<(), StoreError> {
		let Some(statement) = self.statement_number(id)? else {
			// A statement the state file does not hold, such as a secret opened in memory, is
			// not indexed: searches find what the file holds.
			return Ok(());
		};
		// A statement indexed already is indexed anew, its old words gone with its old row: a
		// faulty replica may give an ID to another text.
		self.remove_number(statement)?;
		let (entity, shown) = (
			self.entity_number(indexed.entity)?,
			self.entity_number(indexed.shown)?,
		);
		self.write(
			"INSERT INTO indexed
				(statement, entity, shown, deleted, field, value, words, name)
				VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
			params![
				statement,
				entity,
				shown,
				indexed.deleted,
				indexed.value.0.as_str(),
				indexed.value.1.to_multihash(),
				indexed.words.join(" "),
				indexed.name,
			],
		)?;
		for word in &indexed.words {
			self.write(
				"INSERT OR IGNORE INTO words (word, statement) VALUES (?1, ?2)",
				params![word, statement],
			)?;
		}
		Ok(())
	}

	fn remove(&self, id: StatementId) -> Result<(), StoreError> {
		match self.statement_number(id)? {
			Some(statement) => self.remove_number(statement),
			None => Ok(()),
		}
	}
	fn clear(&self) -> Result<(), StoreError> {
		self.write("DELETE FROM words", [])?;
		self.write("DELETE FROM indexed", [])
	}
	fn with_word(&self, word: &str) -> Result<Vec<(i64, EntityId)>, StoreError> {
		self.query(
			"SELECT w.statement, shown.id FROM words w
				JOIN indexed i ON i.statement = w.statement
				LEFT JOIN entities shown ON shown.n = i.shown
				WHERE w.word = ?1 AND i.deleted = 0",
			[word],
			|row| Ok((row.get(0).map_err(database_error)?, entity_id(row, 1)?)),
		)
	}

	fn word_count(&self, word: &str) -> Result<usize, StoreError> {
		self.count("SELECT count(*) FROM words WHERE word = ?1", [word])
	}

	fn with_prefix(&self, prefix: &str) -> Result<Vec<(i64, EntityId)>, StoreError> {
		self.query(
			"SELECT DISTINCT w.statement, shown.id FROM words w
				JOIN indexed i ON i.statement = w.statement
				LEFT JOIN entities shown ON shown.n = i.shown
				WHERE w.word >= ?1 AND w.word < ?2 AND i.deleted = 0",
			params![prefix, after_prefix(prefix)],
			|row| Ok((row.get(0).map_err(database_error)?, entity_id(row, 1)?)),
		)
	}

	fn prefix_count(&self, prefix: &str, at_most: usize) -> Result<usize, StoreError> {
		self.count(
			"SELECT count(*) FROM
				(SELECT 1 FROM words WHERE word >= ?1 AND word < ?2 LIMIT ?3)",
			params![
				prefix,
				after_prefix(prefix),
				i64::try_from(at_most).unwrap_or(i64::MAX)
			],
		)
	}

	fn shown_with_prefix(&self, entity: EntityId, prefix: &str) -> Result<Vec<i64>, StoreError> {
		let shown = self.query(
			"SELECT statement, words FROM indexed WHERE shown = ?1",
			[self.held_number(entity)?],
			|row| {
				let words: String = row.get(1).map_err(database_error)?;
				Ok((row.get::<_, i64>(0).map_err(database_error)?, words))
			},
		)?;
		Ok(shown
			.into_iter()
			.filter(|(_, words)| {
				words
					.split(' ')
					.any(|word| !word.is_empty() && word.starts_with(prefix))
			})
			.map(|(statement, _)| statement)
			.collect())
	}

	fn with_value(&self, field: &FieldName, value: &Hash) -> Result<Vec<EntityId>, StoreError> {
		self.query(
			"SELECT shown.id FROM indexed i LEFT JOIN entities shown ON shown.n = i.shown
				WHERE i.field = ?1 AND i.value = ?2 AND i.deleted = 0",
			params![field.as_str(), value.to_multihash()],
			|row| entity_id(row, 0),
		)
	}

	fn with_name(&self, lowered: &str) -> Result<Vec<EntityId>, StoreError> {
		self.query(
			"SELECT DISTINCT shown.id FROM indexed i LEFT JOIN entities shown ON shown.n = i.shown
				WHERE i.name = ?1
				ORDER BY shown.id",
			[lowered],
			|row| entity_id(row, 0),
		)
	}

	fn of_shown(&self, entity: EntityId) -> Result<Vec<(StatementId, Indexed)>, StoreError> {
		self.query(
			&format!("SELECT s.id, {INDEXED} {FROM_INDEXED} WHERE i.shown = ?1"),
			[self.held_number(entity)?],
			|row| Ok((statement_id(row, 0)?, indexed_row(row, 1)?)),
		)
	}

	fn set_deleted(&self, entity: EntityId, deleted: bool) -> Result<(), StoreError> {
		self.write(
			"UPDATE indexed SET deleted = ?2 WHERE shown = ?1",
			params![self.held_number(entity)?, deleted],
		)
	}

	fn remove_field(&self, field: &FieldName) -> Result<(), StoreError> {
		self.write(
			"DELETE FROM words
				WHERE statement IN (SELECT statement FROM indexed WHERE field = ?1)",
			[field.as_str()],
		)?;
		self.write("DELETE FROM indexed WHERE field = ?1", [field.as_str()])
	}
}

impl StateFile {
	/// Takes the statement with this number out of the indexes, if it is in them.
	fn remove_number(&self, statement: i64) -> Result<(), StoreError> {
		let words = self.query(
			"SELECT words FROM indexed WHERE statement = ?1",
			[statement],
			|row| {
				let words: String = row.get(0).map_err(database_error)?;
				Ok(parse_words(&words))
			},
		)?;
		let Some(words) = words.into_iter().next() else {
			return Ok(());
		};
		for word in words {
			self.write(
				"DELETE FROM words WHERE word = ?1 AND statement = ?2",
				params![word, statement],
			)?;
		}
		self.write("DELETE FROM indexed WHERE statement = ?1", [statement])
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use nibkeep_testkit::TestRng;

	use super::*;

	/// A state file in a directory of its own, and the directory.
	fn opened(name: &str) -> (PathBuf, StateFile) {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-state-file-{name}-{}", std::process::id()),
		);
		let key = TestRng::new(name).key();
		let state = StateFile::open(&directory.join("state.sqlite"), &key, TestRng::new(name))
			.expect("opens");
		(directory, state)
	}

	/// An entity ID that starts with `first`.
	fn entity(first: u8) -> EntityId {
		let mut bytes = [0; 16];
		bytes[0] = first;
		EntityId::from_random_bytes(bytes)
	}

	#[test]
	fn entities_are_numbered_as_they_come_and_listed_by_id() {
		let (directory, mut state) = opened("numbered");
		let name: FieldName = "org.nibkeep.name".parse().expect("a field name");
		state.begin().expect("begins");
		// The IDs come in descending order, so that their numbers go the other way.
		for (n, first) in [0xc0_u8, 0x80, 0x40].into_iter().enumerate() {
			let id = StatementId::from_random_bytes([u8::try_from(n).expect("small"); 16]);
			let statement = Statement::new(id, entity(first), name.clone(), Value::Integer(1));
			state
				.put(&statement, Origin::new(Timestamp::new(1, 0), None))
				.expect("puts");
		}
		// A reference numbers the entity it refers to too.
		let referring = Statement::new(
			StatementId::from_random_bytes([9; 16]),
			entity(0x80),
			"org.example.link".parse().expect("a field name"),
			Value::Reference(entity(0x20)),
		);
		state
			.put(&referring, Origin::new(Timestamp::new(2, 0), None))
			.expect("puts");
		state.commit().expect("commits");
		let numbered = state
			.query("SELECT id FROM entities ORDER BY n", [], |row| {
				entity_id(row, 0)
			})
			.expect("reads");
		assert_eq!(
			numbered,
			[entity(0xc0), entity(0x80), entity(0x40), entity(0x20)]
		);
		assert_eq!(
			state.entities().expect("lists"),
			[entity(0x40), entity(0x80), entity(0xc0)]
		);
		assert_eq!(
			state.referring_to(entity(0x20)).expect("finds"),
			[referring.id]
		);
		let range = state
			.current_in_range(entity(0x40), entity(0x80), &[name])
			.expect("reads a range");
		let in_range: Vec<EntityId> = range
			.iter()
			.map(|recorded| recorded.statement.entity)
			.collect();
		assert_eq!(
			in_range,
			[entity(0x80), entity(0x40)],
			"ordered by statement ID"
		);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn a_number_left_out_with_a_change_is_forgotten() {
		let (directory, mut state) = opened("forgotten");
		let name: FieldName = "org.nibkeep.name".parse().expect("a field name");
		let at = Origin::new(Timestamp::new(1, 0), None);
		state.begin().expect("begins");
		state
			.savepoint(Savepoint::Change)
			.expect("marks the change");
		let left_out = Statement::new(
			StatementId::from_random_bytes([1; 16]),
			entity(0x10),
			name.clone(),
			Value::Integer(1),
		);
		state.put(&left_out, at).expect("puts");
		state
			.rollback_to_savepoint(Savepoint::Change)
			.expect("leaves the change out");
		// The next entity takes the number that the one left out had.
		let kept = Statement::new(
			StatementId::from_random_bytes([2; 16]),
			entity(0x20),
			name.clone(),
			Value::Integer(2),
		);
		state.put(&kept, at).expect("puts");
		state.commit().expect("commits");
		let numbers = state
			.query("SELECT n, id FROM entities", [], |row| {
				let number: i64 = row.get(0).map_err(database_error)?;
				Ok((number, entity_id(row, 1)?))
			})
			.expect("reads");
		assert_eq!(numbers, [(1, entity(0x20))]);
		assert_eq!(state.of_field(entity(0x10), &name).expect("reads"), []);
		assert_eq!(
			state.of_field(entity(0x20), &name).expect("reads"),
			[kept.id]
		);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn entities_found_by_name_are_listed_by_id() {
		let (directory, mut state) = opened("names");
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let value = Hash::of(b"ada");
		state.begin().expect("begins");
		// They come in descending order, and show under themselves or another.
		for (n, (first, shown)) in [(0xc0_u8, 0xc0_u8), (0x80, 0x10), (0x40, 0x40)]
			.into_iter()
			.enumerate()
		{
			let id = StatementId::from_random_bytes([u8::try_from(n).expect("small"); 16]);
			let statement = Statement::new(
				id,
				entity(first),
				title.clone(),
				Value::Text("Ada".to_owned()),
			);
			state
				.put(&statement, Origin::new(Timestamp::new(1, 0), None))
				.expect("puts");
			let indexed = Indexed {
				entity: entity(first),
				shown: entity(shown),
				deleted: false,
				words: vec!["ada".to_owned()],
				name: Some("ada".to_owned()),
				value: (title.clone(), value),
			};
			state.insert(id, &indexed).expect("indexes");
		}
		state.commit().expect("commits");
		assert_eq!(
			state.with_name("ada").expect("finds"),
			[entity(0x10), entity(0x40), entity(0xc0)]
		);
		let mut by_value = state.with_value(&title, &value).expect("finds");
		by_value.sort();
		assert_eq!(by_value, [entity(0x10), entity(0x40), entity(0xc0)]);
		let shown: Vec<EntityId> = state
			.of_shown(entity(0x10))
			.expect("finds")
			.into_iter()
			.map(|(_, indexed)| indexed.entity)
			.collect();
		assert_eq!(shown, [entity(0x80)]);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn a_statement_is_planned_once_whatever_numbers_are_bound_to_it() {
		let (directory, state) = opened("planned once");
		let connection = state.connection().expect("a connection");
		let mut query = connection
			.prepare_cached(
				"SELECT id FROM statements WHERE entity = ?1 AND field = ?2 ORDER BY id",
			)
			.expect("prepares");
		for entity in [1_i64, 5, 0, 7, 1, 2] {
			query
				.query_map(params![entity, "org.example.note"], |row| {
					row.get::<_, Vec<u8>>(0)
				})
				.expect("reads")
				.for_each(drop);
		}
		assert_eq!(query.get_status(rusqlite::StatementStatus::RePrepare), 0);
		drop(query);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn the_cache_grows_while_the_file_is_written_and_shrinks_after() {
		let (directory, mut state) = opened("cache");
		let cache = |state: &StateFile| -> i64 {
			state
				.connection()
				.expect("a connection")
				.query_row("PRAGMA cache_size", [], |row| row.get(0))
				.expect("reads the cache size")
		};
		assert_eq!(cache(&state), -CACHE_KIB);
		state.begin().expect("begins");
		assert_eq!(cache(&state), -WRITING_CACHE_KIB);
		state.commit().expect("commits");
		assert_eq!(cache(&state), -CACHE_KIB);
		state.begin_derived().expect("begins");
		assert_eq!(cache(&state), -WRITING_CACHE_KIB);
		state.rollback().expect("rolls back");
		assert_eq!(cache(&state), -CACHE_KIB);
		state.begin_derived().expect("begins");
		assert!(state.write("INSERT INTO nowhere VALUES (1)", []).is_err());
		assert_eq!(cache(&state), -CACHE_KIB, "after an error too");
		// Reading alone keeps the cache as it is.
		let during = state
			.reading(|| Ok::<_, StoreError>(cache(&state)))
			.expect("reads");
		assert_eq!(during, -CACHE_KIB);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn a_failed_transaction_of_what_the_file_derives_leaves_it_readable() {
		let (directory, mut state) = opened("failed derived");
		state.begin_derived().expect("begins");
		assert!(state.write("INSERT INTO nowhere VALUES (1)", []).is_err());
		assert!(!state.in_transaction(), "the error ended the transaction");
		assert_eq!(state.mark().expect("still reads"), None);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn a_failed_transaction_that_wrote_refuses_reads_until_the_next() {
		let (directory, mut state) = opened("failed wrote");
		state.begin().expect("begins");
		assert!(state.write("INSERT INTO nowhere VALUES (1)", []).is_err());
		assert!(
			state.mark().is_err(),
			"the file may lack what the log file holds"
		);
		assert!(!state.is_damaged(), "refusing to be read is no damage");
		// Bringing the indexes up to date cannot make up for what the file lacks.
		assert!(state.begin_derived().is_err());
		assert!(state.mark().is_err());
		state.begin().expect("begins anew");
		assert_eq!(state.mark().expect("reads again"), None);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn a_transaction_that_sqlite_ended_is_noted_as_lost() {
		let (directory, mut state) = opened("ended by sqlite");
		state.begin().expect("begins");
		let mark = Mark {
			sequence: 1,
			tag: vec![0; 16],
		};
		state.set_mark(&mark).expect("marks");
		// As SQLite rolls back on its own after some errors, such as a full disk.
		state
			.connection()
			.expect("a connection")
			.execute_batch("ROLLBACK")
			.expect("rolls back");
		// The error that SQLite answered with comes to the file after the transaction ended.
		assert!(state.count("SELECT count(*) FROM nowhere", []).is_err());
		assert!(
			state.mark().is_err(),
			"the file lacks what the transaction took in"
		);
		drop(state);
		let _ = fs::remove_dir_all(directory);
	}
}
