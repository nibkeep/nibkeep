//! The log file of a space: an SQLite database.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_crypto::Key;
use nibkeep_log::{
	Accepted, Change, ChangeId, History, Kept, Kind, Parcel, SealedPayload, Status, Summary,
	WrappedBlobKey, WrappedChangeKey, WrappedKey,
};
use nibkeep_time::Timestamp;
use rusqlite::config::DbConfig;
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};

use crate::{Mark, StoreError};

/// The application ID that marks a log file: `NIBK`.
pub(crate) const APPLICATION_ID: i64 = 0x4E49_424B;

/// The version of the file format.
const VERSION: i64 = 1;

/// The tables of a log file.
///
/// A change's row holds the change and the tag this replica stored it with, its sealed
/// payload as far as the replica has it, and, once it is accepted, its depth, what became of
/// its payload (`status`: 0 pending, 1 applied, 2 adding nothing, 3 set aside), its time,
/// whether it purges changes, and its kind (0 ordinary, 1 governing the space). A waiting
/// change has no status. A row that changes moves to the end, so that the rows after a state
/// file's mark are those that changed since.
///
/// The replicas' accounts of what they hold are indexed by the hash of each page, by which a
/// sync asks for it.
const TABLES: &str = "
	CREATE TABLE changes (
		sequence INTEGER PRIMARY KEY,
		id BLOB NOT NULL UNIQUE,
		change BLOB NOT NULL,
		tag BLOB NOT NULL UNIQUE,
		payload BLOB,
		depth INTEGER,
		status INTEGER,
		millis INTEGER,
		counter INTEGER,
		purging INTEGER,
		kind INTEGER
	);
	CREATE INDEX changes_pending ON changes (id) WHERE status = 0;
	CREATE INDEX changes_purging ON changes (id) WHERE status = 1 AND purging = 1;
	CREATE INDEX changes_waiting ON changes (sequence) WHERE status IS NULL;
	CREATE INDEX changes_by_time ON changes (millis, counter) WHERE status IS NOT NULL;
	CREATE INDEX changes_governing ON changes (id) WHERE kind = 1 AND status IS NOT NULL;
	CREATE INDEX changes_set_aside ON changes (id) WHERE status = 3;
	CREATE TABLE change_keys (
		change BLOB NOT NULL,
		wrapped_key BLOB NOT NULL,
		PRIMARY KEY (change, wrapped_key)
	) WITHOUT ROWID;
	CREATE TABLE heads (
		change BLOB PRIMARY KEY NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE holders (
		statement BLOB PRIMARY KEY NOT NULL,
		change BLOB NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE purged (
		kind INTEGER NOT NULL,
		id BLOB NOT NULL,
		PRIMARY KEY (kind, id)
	) WITHOUT ROWID;
	CREATE TABLE wrapped_keys (
		statement BLOB NOT NULL,
		wrapped_key BLOB NOT NULL,
		PRIMARY KEY (statement, wrapped_key)
	) WITHOUT ROWID;
	CREATE TABLE blob_keys (
		sealed BLOB NOT NULL,
		wrapped_key BLOB NOT NULL,
		PRIMARY KEY (sealed, wrapped_key)
	) WITHOUT ROWID;
	CREATE TABLE verified_blobs (
		sealed BLOB PRIMARY KEY,
		verified INTEGER NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE accounts (
		author BLOB NOT NULL,
		page INTEGER NOT NULL,
		issued INTEGER NOT NULL,
		id BLOB NOT NULL,
		bytes BLOB NOT NULL,
		PRIMARY KEY (author, page)
	) WITHOUT ROWID;
	CREATE INDEX accounts_by_id ON accounts (id);
	CREATE TABLE backup_accounts (
		author BLOB NOT NULL,
		backup BLOB NOT NULL,
		page INTEGER NOT NULL,
		issued INTEGER NOT NULL,
		id BLOB NOT NULL,
		bytes BLOB NOT NULL,
		PRIMARY KEY (author, backup, page)
	) WITHOUT ROWID;
	CREATE INDEX backup_accounts_by_id ON backup_accounts (id);
	CREATE TABLE damaged_blobs (
		sealed BLOB PRIMARY KEY,
		found INTEGER NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE place_checks (
		path TEXT NOT NULL,
		content BLOB NOT NULL,
		checked INTEGER NOT NULL,
		held INTEGER NOT NULL,
		PRIMARY KEY (path, content)
	) WITHOUT ROWID;
	CREATE TABLE opened (
		file BLOB PRIMARY KEY,
		opened INTEGER NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE kept (
		kind INTEGER NOT NULL,
		id BLOB NOT NULL,
		purge BLOB NOT NULL,
		part BLOB NOT NULL,
		received INTEGER NOT NULL,
		PRIMARY KEY (kind, id)
	) WITHOUT ROWID;
	CREATE INDEX kept_by_purge ON kept (purge);
	CREATE TABLE waits (
		since INTEGER PRIMARY KEY,
		wait INTEGER NOT NULL
	);
";

/// A check of a plain copy on this device, an original kept where it was found or the copy of
/// a checkout, read in full against the content it should hold ([technical design: checking
/// and repair]).
///
/// [technical design: checking and repair]: ../../../docs/Technical-design.md#97-checking-and-repair
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceCheck {
	/// The copy's full path.
	pub path: String,
	/// The content it should hold.
	pub content: Hash,
	/// When it was read, in milliseconds since 1970.
	pub checked: u64,
	/// Whether it held the content.
	pub held: bool,
}

/// A page of a replica's account of what it holds in the space, as the log file keeps it:
/// the latest of each page of each replica ([technical design: where the copies are]).
///
/// [technical design: where the copies are]: ../../../docs/Technical-design.md#96-where-the-copies-are
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPage {
	/// The verifying key of the replica that issued it.
	pub author: [u8; 32],
	/// The backup whose account it is a page of, by its record's ID; `None` for the author's
	/// own account.
	pub backup: Option<[u8; 16]>,
	/// Which page it is, by its number.
	pub page: u16,
	/// When it was issued, in milliseconds since 1970.
	pub issued: u64,
	/// The hash of its encoding.
	pub id: Hash,
	/// Its encoding.
	pub bytes: Vec<u8>,
}

/// A page of a replica's account, as the log file keeps it, without its encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPageId {
	/// The verifying key of the replica that issued it.
	pub author: [u8; 32],
	/// The backup whose account it is a page of, by its record's ID; `None` for the author's
	/// own account.
	pub backup: Option<[u8; 16]>,
	/// Which page it is, by its number.
	pub page: u16,
	/// When it was issued, in milliseconds since 1970.
	pub issued: u64,
	/// The hash of its encoding.
	pub id: Hash,
}

/// What the table `purged` holds: purged changes, secret statements and sealed blobs.
const PURGED_CHANGE: i64 = 0;
const PURGED_STATEMENT: i64 = 1;
const PURGED_BLOB: i64 = 2;

/// The context of the key that tags stored changes, derived from the replica's storage key.
const TAG_CONTEXT: &str = "nibkeep 2026-10 stored changes";

/// How long to wait for another connection that holds the database.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What one save writes to the log file, besides what the log writes to it as its history.
#[derive(Default)]
pub(crate) struct Writes {
	/// Waiting changes to add, with their sealed payloads and wrapped keys as far as the
	/// log has them. Accepted changes are in the file already.
	pub(crate) changes: Vec<Parcel>,
	/// Waiting changes whose sealed payloads or wrapped keys changed; the log saves what it
	/// has of accepted ones itself.
	pub(crate) completed: Vec<Parcel>,
	/// Changes to remove: waiting changes that were rejected for good.
	pub(crate) rejected: Vec<ChangeId>,
	/// The wrapped keys that statements now have, replacing those stored; none for purged
	/// statements.
	pub(crate) wrapped_keys: Vec<(StatementId, Vec<WrappedKey>)>,
	/// The wrapped keys that sealed blobs now have, replacing those stored; none for purged
	/// blobs.
	pub(crate) blob_keys: Vec<(Hash, Vec<WrappedBlobKey>)>,
	/// When the replica last checked sealed blobs it holds, in milliseconds since 1970.
	pub(crate) verified: Vec<(Hash, u64)>,
	/// Sealed blobs that the replica no longer holds, whose checks are forgotten.
	pub(crate) unheld: Vec<Hash>,
	/// Sealed blobs that a check found damaged, with when.
	pub(crate) damaged: Vec<(Hash, u64)>,
	/// Checks of plain copies on this device.
	pub(crate) places: Vec<PlaceCheck>,
	/// Files opened on this device, by their IDs, with when, in milliseconds since 1970.
	pub(crate) opened: Vec<(EntityId, u64)>,
	/// What purges destroyed that this replica keeps apart while it waits, by the purge that
	/// destroyed each part, with when it took the purge in.
	pub(crate) kept: Vec<KeptRow>,
	/// Parts kept apart that go: given back, or destroyed as their wait ended.
	pub(crate) unkept: Vec<Kept>,
	/// How long this device waits before purging, from each time on, in milliseconds,
	/// replacing what was stored: kept in the personal space's log file.
	pub(crate) waits: Option<Vec<(u64, u64)>>,
}

/// A part of what a purge destroyed, which a replica that waits keeps apart in its log file
/// ([technical design: copies that wait]).
///
/// [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptRow {
	/// The purge that destroyed it.
	pub purge: ChangeId,
	/// What it was.
	pub part: Kept,
	/// When the replica took the purge in, in milliseconds since 1970.
	pub received: u64,
}

/// How the `kept` table names a part: its kind, 0 for a change, 1 for a secret statement's
/// record keys, 2 for a sealed blob's keys, and the bytes of what it is of.
fn kept_key(part: &Kept) -> (i64, Vec<u8>) {
	match part {
		Kept::Change(parcel) => (0, parcel.id().hash().to_multihash()),
		Kept::RecordKeys(statement, _) => (1, statement.as_bytes().to_vec()),
		Kept::BlobKeys(sealed, _) => (2, sealed.to_multihash()),
	}
}

/// A kept part in CBOR: a change as `[change, sealed payload or null, wrapped keys]`, and
/// keys as the array of the wrapped keys.
fn kept_part(part: &Kept) -> Vec<u8> {
	let value = match part {
		Kept::Change(parcel) => Cbor::Array(vec![
			parcel.change.to_cbor(),
			parcel.payload.as_ref().map_or(Cbor::Null, ToCbor::to_cbor),
			Cbor::Array(parcel.keys.iter().map(ToCbor::to_cbor).collect()),
		]),
		Kept::RecordKeys(_, keys) => Cbor::Array(keys.iter().map(ToCbor::to_cbor).collect()),
		Kept::BlobKeys(_, keys) => Cbor::Array(keys.iter().map(ToCbor::to_cbor).collect()),
	};
	cbor::encode(&value)
}

/// Reads a kept part from its row.
fn read_kept(kind: i64, id: &[u8], part: &[u8]) -> Result<Kept, StoreError> {
	let damaged = || StoreError::Damaged("a part kept from a purge does not read".to_owned());
	let value = cbor::decode(part).map_err(|_| damaged())?;
	let read = match kind {
		0 => {
			let Cbor::Array(items) = &value else {
				return Err(damaged());
			};
			let [change, payload, keys] = items.as_slice() else {
				return Err(damaged());
			};
			let change = Change::from_cbor(change).map_err(|_| damaged())?;
			if change.id() != change_id(id)? {
				return Err(damaged());
			}
			let payload = match payload {
				Cbor::Null => None,
				payload => Some(SealedPayload::from_cbor(payload).map_err(|_| damaged())?),
			};
			let keys = Vec::<WrappedChangeKey>::from_cbor(keys).map_err(|_| damaged())?;
			Kept::Change(Box::new(Parcel {
				change,
				payload,
				keys,
			}))
		}
		1 => Kept::RecordKeys(
			statement_id(id)?,
			Vec::<WrappedKey>::from_cbor(&value).map_err(|_| damaged())?,
		),
		2 => Kept::BlobKeys(
			hash(id)?,
			Vec::<WrappedBlobKey>::from_cbor(&value).map_err(|_| damaged())?,
		),
		_ => return Err(damaged()),
	};
	Ok(read)
}

impl Writes {
	pub(crate) fn is_empty(&self) -> bool {
		self.changes.is_empty()
			&& self.completed.is_empty()
			&& self.rejected.is_empty()
			&& self.wrapped_keys.is_empty()
			&& self.blob_keys.is_empty()
			&& self.verified.is_empty()
			&& self.unheld.is_empty()
			&& self.damaged.is_empty()
			&& self.places.is_empty()
			&& self.opened.is_empty()
			&& self.kept.is_empty()
			&& self.unkept.is_empty()
			&& self.waits.is_none()
	}
}

/// The log file of a space, which holds its history: an SQLite database.
///
/// It is written in transactions, which the stored log begins and commits,
/// so that what one call to the log changes is saved whole. Every change read from it is
/// checked against its stored ID and the tag this replica stored it with.
pub struct LogFile {
	connection: Connection,
	/// The key that tags the changes this replica stores, so that it knows on reading them
	/// that it wrote them, and need not check their signatures again.
	tags: Key,
	/// How many changes were read, and checked, since the file was opened.
	read: Cell<usize>,
	/// Whether the transaction that has begun wrote anything.
	wrote: Cell<bool>,
}

impl std::fmt::Debug for LogFile {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("LogFile").finish_non_exhaustive()
	}
}

impl LogFile {
	/// Opens the log file, creating it if it does not exist, with the replica's storage key.
	/// A file that is not a log file of this version is refused before anything is written
	/// to it.
	pub(crate) fn open(path: &Path, storage: &Key) -> Result<Self, StoreError> {
		let mut connection = Connection::open(path).map_err(database)?;
		connection.busy_timeout(BUSY_TIMEOUT).map_err(database)?;
		steady_plans(&connection).map_err(database)?;
		initialize(&mut connection)?;
		let mode: String = connection
			.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
			.map_err(database)?;
		if !mode.eq_ignore_ascii_case("wal") {
			return Err(StoreError::Unsupported("write-ahead logging"));
		}
		synchronize_fully(&connection).map_err(database)?;
		connection
			.pragma_update(None, "secure_delete", true)
			.map_err(database)?;
		Ok(Self {
			connection,
			tags: storage.derive(TAG_CONTEXT),
			read: Cell::new(0),
			wrote: Cell::new(false),
		})
	}

	/// Begins a transaction, which holds the database's write lock until it ends, waiting
	/// for it while another program holds it.
	pub(crate) fn begin(&mut self) -> Result<(), StoreError> {
		if self.in_transaction() {
			return Err(StoreError::Transaction);
		}
		self.wrote.set(false);
		self.connection
			.execute_batch("BEGIN IMMEDIATE")
			.map_err(database)
	}

	/// Begins a transaction as [`LogFile::begin`] does, unless another program holds the
	/// write lock: whether it began.
	pub(crate) fn begin_now(&mut self) -> Result<bool, StoreError> {
		self.connection
			.busy_timeout(Duration::ZERO)
			.map_err(database)?;
		let began = self.begin();
		if let Err(error) = self.connection.busy_timeout(BUSY_TIMEOUT) {
			if began.is_ok() {
				let _ = self.rollback();
			}
			return Err(database(error));
		}
		match began {
			Ok(()) => Ok(true),
			Err(error) if error.busy() => Ok(false),
			Err(error) => Err(error),
		}
	}

	/// A number that changes whenever another connection, of this program or another, commits
	/// a change to the file, as [`data_version`] reads it.
	pub(crate) fn data_version(&self) -> Result<i64, StoreError> {
		data_version(&self.connection).map_err(database)
	}

	/// Whether the transaction that has begun wrote anything.
	pub(crate) fn wrote(&self) -> bool {
		self.in_transaction() && self.wrote.get()
	}

	/// Whether a transaction has begun and not ended.
	pub(crate) fn in_transaction(&self) -> bool {
		!self.connection.is_autocommit()
	}

	/// Commits the transaction.
	pub(crate) fn commit(&mut self) -> Result<(), StoreError> {
		self.connection.execute_batch("COMMIT").map_err(database)
	}

	/// Leaves out what was written since the transaction began, and ends it.
	pub(crate) fn rollback(&mut self) -> Result<(), StoreError> {
		self.connection.execute_batch("ROLLBACK").map_err(database)
	}

	/// Marks where a change of a batch begins, in the transaction that has begun, so that a
	/// change that is refused can be left out alone.
	pub(crate) fn savepoint(&mut self) -> Result<(), StoreError> {
		self.wrote.set(false);
		self.connection
			.execute_batch("SAVEPOINT change")
			.map_err(database)
	}

	/// Keeps what was written since the savepoint, in the transaction.
	pub(crate) fn release(&mut self) -> Result<(), StoreError> {
		self.connection
			.execute_batch("RELEASE change")
			.map_err(database)
	}

	/// Leaves out what was written since the savepoint, and keeps the transaction.
	pub(crate) fn rollback_to_savepoint(&mut self) -> Result<(), StoreError> {
		self.connection
			.execute_batch("ROLLBACK TO change; RELEASE change")
			.map_err(database)
	}

	/// Runs a statement that writes, inside a transaction.
	fn write(&self, sql: &str, parameters: impl rusqlite::Params) -> Result<usize, StoreError> {
		if !self.in_transaction() {
			return Err(StoreError::Transaction);
		}
		self.wrote.set(true);
		self.connection
			.prepare_cached(sql)
			.and_then(|mut statement| statement.execute(parameters))
			.map_err(database)
	}

	/// How many changes were read, and checked against their tags, since the file was
	/// opened: opening a space reads only those that changed since its state file last
	/// caught up.
	#[must_use]
	pub fn changes_read(&self) -> usize {
		self.read.get()
	}

	/// Reads a change from its columns `change` and `tag`, checked against them and its ID.
	fn checked(&self, id: &[u8], encoded: &[u8], tag: &[u8]) -> Result<Change, StoreError> {
		self.read.set(self.read.get() + 1);
		if !self.tags.verify_tag(encoded, tag) {
			return Err(StoreError::Damaged(
				"a stored change was not stored by this device".to_owned(),
			));
		}
		let change: Change = cbor::from_slice(encoded)
			.map_err(|error| StoreError::Damaged(format!("a stored change: {error}")))?;
		if change.id().hash().to_multihash() != id {
			return Err(StoreError::Damaged(
				"a stored change does not have its ID".to_owned(),
			));
		}
		Ok(change)
	}

	/// The wrapped keys stored for a change, known by the bytes of its ID.
	fn keys_of(&self, change: &[u8]) -> Result<Vec<WrappedChangeKey>, StoreError> {
		let mut query = self
			.connection
			.prepare_cached("SELECT wrapped_key FROM change_keys WHERE change = ?1")
			.map_err(database)?;
		let rows = query
			.query_map([change], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut keys = Vec::new();
		for row in rows {
			let encoded = row.map_err(database)?;
			keys.push(
				cbor::from_slice(&encoded).map_err(|error| {
					StoreError::Damaged(format!("a stored change key: {error}"))
				})?,
			);
		}
		Ok(keys)
	}

	/// The changes waiting for their parents, in the order the replica got them, with their
	/// sealed payloads and wrapped keys.
	pub(crate) fn waiting(&self) -> Result<Vec<Parcel>, StoreError> {
		let mut query = self
			.connection
			.prepare(
				"SELECT id, change, tag, payload FROM changes WHERE status IS NULL
					ORDER BY sequence",
			)
			.map_err(database)?;
		let rows = query
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, Vec<u8>>(2)?,
					row.get::<_, Option<Vec<u8>>>(3)?,
				))
			})
			.map_err(database)?;
		let mut waiting = Vec::new();
		for row in rows {
			let (id, encoded, tag, payload) = row.map_err(database)?;
			waiting.push(Parcel {
				change: self.checked(&id, &encoded, &tag)?,
				payload: payload_from(payload)?,
				keys: self.keys_of(&id)?,
			});
		}
		Ok(waiting)
	}

	/// The applied changes in the rows after `sequence`, in their order.
	pub(crate) fn applied_since(&self, sequence: i64) -> Result<Vec<ChangeId>, StoreError> {
		self.ids(
			"SELECT id FROM changes WHERE sequence > ?1 AND status = 1 ORDER BY sequence",
			[sequence],
		)
	}

	/// The pending changes in the rows after `sequence`, in their order.
	pub(crate) fn pending_since(&self, sequence: i64) -> Result<Vec<ChangeId>, StoreError> {
		self.ids(
			"SELECT id FROM changes WHERE sequence > ?1 AND status = 0 ORDER BY sequence",
			[sequence],
		)
	}

	/// The sequence of the row with this tag, if one has it.
	pub(crate) fn sequence_of(&self, tag: &[u8]) -> Result<Option<i64>, StoreError> {
		self.connection
			.query_row(
				"SELECT sequence FROM changes WHERE tag = ?1",
				[tag],
				|row| row.get(0),
			)
			.optional()
			.map_err(database)
	}

	/// The latest time of an accepted change, from which the space's clock resumes.
	pub(crate) fn latest(&self) -> Result<Option<Timestamp>, StoreError> {
		let latest: Option<(i64, u32)> = self
			.connection
			.query_row(
				"SELECT millis, counter FROM changes WHERE status IS NOT NULL
					ORDER BY millis DESC, counter DESC LIMIT 1",
				[],
				|row| Ok((row.get(0)?, row.get(1)?)),
			)
			.optional()
			.map_err(database)?;
		Ok(latest.map(|(millis, counter)| Timestamp::new(millis.cast_unsigned(), counter)))
	}

	/// Where the last change in the file is, if it holds any.
	pub(crate) fn last(&self) -> Result<Option<Mark>, StoreError> {
		self.connection
			.query_row(
				"SELECT sequence, tag FROM changes ORDER BY sequence DESC LIMIT 1",
				[],
				|row| {
					Ok(Mark {
						sequence: row.get(0)?,
						tag: row.get(1)?,
					})
				},
			)
			.optional()
			.map_err(database)
	}

	/// The wrapped keys.
	pub(crate) fn wrapped_keys(&self) -> Result<Vec<(StatementId, WrappedKey)>, StoreError> {
		let mut query = self
			.connection
			.prepare("SELECT statement, wrapped_key FROM wrapped_keys")
			.map_err(database)?;
		let rows = query
			.query_map([], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
			})
			.map_err(database)?;
		let mut keys = Vec::new();
		for row in rows {
			let (statement, encoded) = row.map_err(database)?;
			let wrapped: WrappedKey = cbor::from_slice(&encoded)
				.map_err(|error| StoreError::Damaged(format!("a stored key: {error}")))?;
			keys.push((statement_id(&statement)?, wrapped));
		}
		Ok(keys)
	}

	/// The wrapped blob keys, by sealed hash.
	pub(crate) fn blob_keys(&self) -> Result<Vec<(Hash, WrappedBlobKey)>, StoreError> {
		let mut query = self
			.connection
			.prepare("SELECT sealed, wrapped_key FROM blob_keys")
			.map_err(database)?;
		let rows = query
			.query_map([], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
			})
			.map_err(database)?;
		let mut keys = Vec::new();
		for row in rows {
			let (sealed, encoded) = row.map_err(database)?;
			let wrapped: WrappedBlobKey = cbor::from_slice(&encoded)
				.map_err(|error| StoreError::Damaged(format!("a stored blob key: {error}")))?;
			keys.push((hash(&sealed)?, wrapped));
		}
		Ok(keys)
	}

	/// When the replica last checked a sealed blob, in milliseconds since 1970.
	pub(crate) fn verified(&self, sealed: &Hash) -> Result<Option<u64>, StoreError> {
		let verified: Option<i64> = self
			.connection
			.query_row(
				"SELECT verified FROM verified_blobs WHERE sealed = ?1",
				params![sealed.to_multihash()],
				|row| row.get(0),
			)
			.optional()
			.map_err(database)?;
		Ok(verified.and_then(|verified| u64::try_from(verified).ok()))
	}

	/// When the replica last checked each sealed blob that it checked, in milliseconds since
	/// 1970.
	pub(crate) fn checks(&self) -> Result<BTreeMap<Hash, u64>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT sealed, verified FROM verified_blobs")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
			})
			.map_err(database)?;
		let mut checks = BTreeMap::new();
		for row in rows {
			let (sealed, verified) = row.map_err(database)?;
			checks.insert(hash(&sealed)?, u64::try_from(verified).unwrap_or(0));
		}
		Ok(checks)
	}

	/// The sealed blobs that the replica's checks found damaged, with when each was last
	/// found so, in milliseconds since 1970.
	pub(crate) fn damaged_blobs(&self) -> Result<BTreeMap<Hash, u64>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT sealed, found FROM damaged_blobs")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
			})
			.map_err(database)?;
		let mut damaged = BTreeMap::new();
		for row in rows {
			let (sealed, found) = row.map_err(database)?;
			damaged.insert(hash(&sealed)?, u64::try_from(found).unwrap_or(0));
		}
		Ok(damaged)
	}

	/// The latest check of each plain copy on this device, by its path and the content it
	/// should hold.
	pub(crate) fn place_checks(&self) -> Result<Vec<PlaceCheck>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT path, content, checked, held FROM place_checks")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, String>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, bool>(3)?,
				))
			})
			.map_err(database)?;
		let mut checks = Vec::new();
		for row in rows {
			let (path, content, checked, held) = row.map_err(database)?;
			checks.push(PlaceCheck {
				path,
				content: hash(&content)?,
				checked: u64::try_from(checked).unwrap_or(0),
				held,
			});
		}
		Ok(checks)
	}

	/// When each file was last opened on this device, by its ID, in milliseconds since 1970.
	pub(crate) fn opened(&self) -> Result<BTreeMap<EntityId, u64>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT file, opened FROM opened")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
			})
			.map_err(database)?;
		let mut opened = BTreeMap::new();
		for row in rows {
			let (file, at) = row.map_err(database)?;
			if let Ok(bytes) = <[u8; 16]>::try_from(file.as_slice()) {
				opened.insert(EntityId::from_bytes(bytes), u64::try_from(at).unwrap_or(0));
			}
		}
		Ok(opened)
	}

	/// The pages of the replicas' accounts that the log file keeps: the latest of each.
	pub(crate) fn account_pages(&self) -> Result<Vec<StoredPage>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT author, page, issued, id, bytes FROM accounts")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, i64>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, Vec<u8>>(3)?,
					row.get::<_, Vec<u8>>(4)?,
				))
			})
			.map_err(database)?;
		let mut pages = Vec::new();
		for row in rows {
			let (author, page, issued, id, bytes) = row.map_err(database)?;
			let damaged = || StoreError::Damaged("a stored page of an account".to_owned());
			pages.push(StoredPage {
				author: author.try_into().map_err(|_| damaged())?,
				backup: None,
				page: u16::try_from(page).map_err(|_| damaged())?,
				issued: u64::try_from(issued).map_err(|_| damaged())?,
				id: hash(&id)?,
				bytes,
			});
		}
		Ok(pages)
	}

	/// The pages of the replicas' accounts that the log file keeps, without their
	/// encodings.
	pub(crate) fn account_page_ids(&self) -> Result<Vec<StoredPageId>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT author, page, issued, id FROM accounts")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, i64>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, Vec<u8>>(3)?,
				))
			})
			.map_err(database)?;
		let mut pages = Vec::new();
		for row in rows {
			let (author, page, issued, id) = row.map_err(database)?;
			let damaged = || StoreError::Damaged("a stored page of an account".to_owned());
			pages.push(StoredPageId {
				author: author.try_into().map_err(|_| damaged())?,
				backup: None,
				page: u16::try_from(page).map_err(|_| damaged())?,
				issued: u64::try_from(issued).map_err(|_| damaged())?,
				id: hash(&id)?,
			});
		}
		Ok(pages)
	}

	/// The encoding of the page of an account whose hash is `id`, if the log file keeps it.
	pub(crate) fn account_page(&self, id: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
		let own = self
			.connection
			.query_row(
				"SELECT bytes FROM accounts WHERE id = ?1",
				params![id.to_multihash()],
				|row| row.get(0),
			)
			.optional()
			.map_err(database)?;
		if own.is_some() {
			return Ok(own);
		}
		self.connection
			.query_row(
				"SELECT bytes FROM backup_accounts WHERE id = ?1",
				params![id.to_multihash()],
				|row| row.get(0),
			)
			.optional()
			.map_err(database)
	}

	/// The pages of the accounts of backups that the log file keeps: the latest of each page
	/// of each backup that each replica issued.
	pub(crate) fn backup_account_pages(&self) -> Result<Vec<StoredPage>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT author, backup, page, issued, id, bytes FROM backup_accounts")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, i64>(3)?,
					row.get::<_, Vec<u8>>(4)?,
					row.get::<_, Vec<u8>>(5)?,
				))
			})
			.map_err(database)?;
		let mut pages = Vec::new();
		for row in rows {
			let (author, backup, page, issued, id, bytes) = row.map_err(database)?;
			let damaged = || StoreError::Damaged("a stored page of a backup's account".to_owned());
			pages.push(StoredPage {
				author: author.try_into().map_err(|_| damaged())?,
				backup: Some(backup.try_into().map_err(|_| damaged())?),
				page: u16::try_from(page).map_err(|_| damaged())?,
				issued: u64::try_from(issued).map_err(|_| damaged())?,
				id: hash(&id)?,
				bytes,
			});
		}
		Ok(pages)
	}

	/// The pages of the accounts of backups that the log file keeps, without their
	/// encodings.
	pub(crate) fn backup_account_page_ids(&self) -> Result<Vec<StoredPageId>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT author, backup, page, issued, id FROM backup_accounts")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, i64>(3)?,
					row.get::<_, Vec<u8>>(4)?,
				))
			})
			.map_err(database)?;
		let mut pages = Vec::new();
		for row in rows {
			let (author, backup, page, issued, id) = row.map_err(database)?;
			let damaged = || StoreError::Damaged("a stored page of a backup's account".to_owned());
			pages.push(StoredPageId {
				author: author.try_into().map_err(|_| damaged())?,
				backup: Some(backup.try_into().map_err(|_| damaged())?),
				page: u16::try_from(page).map_err(|_| damaged())?,
				issued: u64::try_from(issued).map_err(|_| damaged())?,
				id: hash(&id)?,
			});
		}
		Ok(pages)
	}

	/// Keeps each of these pages that is later than the page of its replica and number that
	/// the log file keeps, or that it keeps none of: issued later, or at the same time with
	/// a larger hash. Returns how many it kept.
	pub(crate) fn save_account_pages(&mut self, pages: &[StoredPage]) -> Result<usize, StoreError> {
		if pages.is_empty() {
			return Ok(0);
		}
		let own = !self.in_transaction();
		if own {
			self.begin()?;
		}
		let saved = pages.iter().try_fold(0, |kept, page| {
			let issued = i64::try_from(page.issued).unwrap_or(i64::MAX);
			let written = match &page.backup {
				None => self.write(
					"INSERT INTO accounts (author, page, issued, id, bytes)
						VALUES (?1, ?2, ?3, ?4, ?5)
						ON CONFLICT (author, page) DO UPDATE
						SET issued = excluded.issued, id = excluded.id, bytes = excluded.bytes
						WHERE (excluded.issued, excluded.id) > (accounts.issued, accounts.id)",
					params![
						page.author.as_slice(),
						page.page,
						issued,
						page.id.to_multihash(),
						page.bytes
					],
				)?,
				Some(backup) => self.write(
					"INSERT INTO backup_accounts (author, backup, page, issued, id, bytes)
						VALUES (?1, ?2, ?3, ?4, ?5, ?6)
						ON CONFLICT (author, backup, page) DO UPDATE
						SET issued = excluded.issued, id = excluded.id, bytes = excluded.bytes
						WHERE (excluded.issued, excluded.id)
							> (backup_accounts.issued, backup_accounts.id)",
					params![
						page.author.as_slice(),
						backup.as_slice(),
						page.page,
						issued,
						page.id.to_multihash(),
						page.bytes
					],
				)?,
			};
			Ok::<_, StoreError>(kept + written)
		});
		if own {
			match saved {
				Ok(_) => self.commit()?,
				Err(error) => {
					let _ = self.rollback();
					return Err(error);
				}
			}
		}
		saved
	}

	/// Writes what one operation changed besides the history: in the transaction that has
	/// begun, or else in one of its own.
	pub(crate) fn save(&mut self, writes: &Writes) -> Result<(), StoreError> {
		if writes.is_empty() {
			return Ok(());
		}
		let own = !self.in_transaction();
		if own {
			self.begin()?;
		}
		let saved = self.save_in_transaction(writes);
		if own {
			match saved {
				Ok(()) => self.commit()?,
				Err(error) => {
					let _ = self.rollback();
					return Err(error);
				}
			}
		}
		saved
	}

	fn save_in_transaction(&self, writes: &Writes) -> Result<(), StoreError> {
		for parcel in &writes.changes {
			let encoded = cbor::to_vec(&parcel.change);
			let tag = self.tags.tag(&encoded);
			let id = parcel.id().hash().to_multihash();
			let inserted = self.write(
				"INSERT OR IGNORE INTO changes (id, change, tag, payload) VALUES (?1, ?2, ?3, ?4)",
				params![
					id,
					encoded,
					tag.as_slice(),
					parcel.payload.as_ref().map(cbor::to_vec)
				],
			)?;
			if inserted > 0 {
				self.replace_change_keys(&id, &parcel.keys)?;
			}
		}
		for parcel in &writes.completed {
			let id = parcel.id().hash().to_multihash();
			self.write(
				"UPDATE changes SET payload = ?2,
					sequence = (SELECT max(sequence) + 1 FROM changes)
					WHERE id = ?1",
				params![id, parcel.payload.as_ref().map(cbor::to_vec)],
			)?;
			self.replace_change_keys(&id, &parcel.keys)?;
		}
		for id in &writes.rejected {
			let id = id.hash().to_multihash();
			self.write("DELETE FROM changes WHERE id = ?1", params![id])?;
			self.replace_change_keys(&id, &[])?;
		}
		for (statement, wrapped_keys) in &writes.wrapped_keys {
			let statement = statement.as_bytes().as_slice();
			self.write(
				"DELETE FROM wrapped_keys WHERE statement = ?1",
				params![statement],
			)?;
			for wrapped in wrapped_keys {
				self.write(
					"INSERT INTO wrapped_keys (statement, wrapped_key) VALUES (?1, ?2)",
					params![statement, cbor::to_vec(wrapped)],
				)?;
			}
		}
		for (sealed, wrapped_keys) in &writes.blob_keys {
			let sealed = sealed.to_multihash();
			self.write("DELETE FROM blob_keys WHERE sealed = ?1", params![sealed])?;
			for wrapped in wrapped_keys {
				self.write(
					"INSERT INTO blob_keys (sealed, wrapped_key) VALUES (?1, ?2)",
					params![sealed, cbor::to_vec(wrapped)],
				)?;
			}
		}
		self.save_records_of_device(writes)
	}

	/// Saves what this device alone knows: its checks, and when it opened each file.
	fn save_records_of_device(&self, writes: &Writes) -> Result<(), StoreError> {
		for (sealed, verified) in &writes.verified {
			self.write(
				"INSERT OR REPLACE INTO verified_blobs (sealed, verified) VALUES (?1, ?2)",
				params![
					sealed.to_multihash(),
					i64::try_from(*verified).unwrap_or(i64::MAX)
				],
			)?;
		}
		for sealed in &writes.unheld {
			self.write(
				"DELETE FROM verified_blobs WHERE sealed = ?1",
				params![sealed.to_multihash()],
			)?;
			self.write(
				"DELETE FROM damaged_blobs WHERE sealed = ?1",
				params![sealed.to_multihash()],
			)?;
		}
		for (sealed, found) in &writes.damaged {
			self.write(
				"INSERT OR REPLACE INTO damaged_blobs (sealed, found) VALUES (?1, ?2)",
				params![
					sealed.to_multihash(),
					i64::try_from(*found).unwrap_or(i64::MAX)
				],
			)?;
		}
		for check in &writes.places {
			self.write(
				"INSERT OR REPLACE INTO place_checks (path, content, checked, held)
					VALUES (?1, ?2, ?3, ?4)",
				params![
					check.path,
					check.content.to_multihash(),
					i64::try_from(check.checked).unwrap_or(i64::MAX),
					check.held
				],
			)?;
		}
		for (file, at) in &writes.opened {
			self.write(
				"INSERT OR REPLACE INTO opened (file, opened) VALUES (?1, ?2)",
				params![
					file.as_bytes().as_slice(),
					i64::try_from(*at).unwrap_or(i64::MAX)
				],
			)?;
		}
		for row in &writes.kept {
			let (kind, id) = kept_key(&row.part);
			// Kept once: by the first purge that destroyed it.
			self.write(
				"INSERT OR IGNORE INTO kept (kind, id, purge, part, received)
					VALUES (?1, ?2, ?3, ?4, ?5)",
				params![
					kind,
					id,
					row.purge.hash().to_multihash(),
					kept_part(&row.part),
					i64::try_from(row.received).unwrap_or(i64::MAX)
				],
			)?;
		}
		self.forget_kept(&writes.unkept)?;
		if let Some(waits) = &writes.waits {
			self.write("DELETE FROM waits", [])?;
			for (since, wait) in waits {
				self.write(
					"INSERT OR REPLACE INTO waits (since, wait) VALUES (?1, ?2)",
					params![
						i64::try_from(*since).unwrap_or(i64::MAX),
						i64::try_from(*wait).unwrap_or(i64::MAX)
					],
				)?;
			}
		}
		Ok(())
	}

	/// How long this device waits before purging, from each time on, in milliseconds, in the
	/// order of those times.
	pub(crate) fn waits(&self) -> Result<Vec<(u64, u64)>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT since, wait FROM waits ORDER BY since")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
			.map_err(database)?;
		let mut waits = Vec::new();
		for row in rows {
			let (since, wait) = row.map_err(database)?;
			waits.push((
				u64::try_from(since).unwrap_or(0),
				u64::try_from(wait).unwrap_or(0),
			));
		}
		Ok(waits)
	}

	/// What this replica keeps apart of what purges destroyed, as it waits, with when it took
	/// each purge in.
	pub(crate) fn kept(&self) -> Result<Vec<KeptRow>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT kind, id, purge, part, received FROM kept")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, i64>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, Vec<u8>>(2)?,
					row.get::<_, Vec<u8>>(3)?,
					row.get::<_, i64>(4)?,
				))
			})
			.map_err(database)?;
		let mut kept = Vec::new();
		for row in rows {
			let (kind, id, purge, part, received) = row.map_err(database)?;
			kept.push(KeptRow {
				purge: change_id(&purge)?,
				part: read_kept(kind, &id, &part)?,
				received: u64::try_from(received).unwrap_or(0),
			});
		}
		Ok(kept)
	}

	/// The sealed hashes of the blobs whose keys this replica keeps apart.
	pub(crate) fn kept_blobs(&self) -> Result<BTreeSet<Hash>, StoreError> {
		let mut statement = self
			.connection
			.prepare_cached("SELECT id FROM kept WHERE kind = 2")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut kept = BTreeSet::new();
		for row in rows {
			kept.insert(hash(&row.map_err(database)?)?);
		}
		Ok(kept)
	}

	/// Whether this replica keeps anything apart of what purges destroyed.
	pub(crate) fn keeps_any(&self) -> Result<bool, StoreError> {
		self.connection
			.prepare_cached("SELECT 1 FROM kept LIMIT 1")
			.and_then(|mut statement| statement.exists([]))
			.map_err(database)
	}

	/// Forgets these kept parts, in the transaction that has begun.
	pub(crate) fn forget_kept(&self, parts: &[Kept]) -> Result<(), StoreError> {
		for part in parts {
			let (kind, id) = kept_key(part);
			self.write(
				"DELETE FROM kept WHERE kind = ?1 AND id = ?2",
				params![kind, id],
			)?;
		}
		Ok(())
	}

	/// Replaces the wrapped keys stored for a change, known by the bytes of its ID.
	fn replace_change_keys(
		&self,
		change: &[u8],
		keys: &[WrappedChangeKey],
	) -> Result<(), StoreError> {
		self.write("DELETE FROM change_keys WHERE change = ?1", params![change])?;
		for key in keys {
			self.write(
				"INSERT OR IGNORE INTO change_keys (change, wrapped_key) VALUES (?1, ?2)",
				params![change, cbor::to_vec(key)],
			)?;
		}
		Ok(())
	}

	/// What SQLite's own check of the database finds wrong: nothing if it is sound.
	pub(crate) fn integrity_problems(&self) -> Result<Vec<String>, StoreError> {
		let mut query = self
			.connection
			.prepare("PRAGMA integrity_check")
			.map_err(database)?;
		let rows = query
			.query_map([], |row| row.get::<_, String>(0))
			.map_err(database)?;
		let mut problems = Vec::new();
		for row in rows {
			let row = row.map_err(database)?;
			if row != "ok" {
				problems.push(row);
			}
		}
		Ok(problems)
	}

	/// Moves everything in the write-ahead log into the database file and truncates the
	/// log, so that no older copy of deleted data remains. Returns whether it got that
	/// far; another connection that reads the database can hold it up.
	pub(crate) fn truncate_wal(&self) -> Result<bool, StoreError> {
		let busy: i64 = self
			.connection
			.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
			.map_err(database)?;
		Ok(busy == 0)
	}

	/// The changes that a query of their IDs finds.
	fn ids(
		&self,
		sql: &str,
		parameters: impl rusqlite::Params,
	) -> Result<Vec<ChangeId>, StoreError> {
		let mut query = self.connection.prepare_cached(sql).map_err(database)?;
		let rows = query
			.query_map(parameters, |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut ids = Vec::new();
		for row in rows {
			ids.push(change_id(&row.map_err(database)?)?);
		}
		Ok(ids)
	}

	/// Whether a query with one parameter finds a row.
	fn exists(&self, sql: &str, parameter: impl rusqlite::ToSql) -> Result<bool, StoreError> {
		self.connection
			.prepare_cached(sql)
			.and_then(|mut statement| statement.exists([parameter]))
			.map_err(database)
	}

	/// Moves a change's row to the end of the file.
	fn move_to_end(&self, id: &[u8]) -> Result<(), StoreError> {
		self.write(
			"UPDATE changes SET sequence = (SELECT max(sequence) + 1 FROM changes) WHERE id = ?1",
			params![id],
		)?;
		Ok(())
	}
}

impl History for LogFile {
	type Error = StoreError;

	fn get(&self, id: ChangeId) -> Result<Option<Cow<'_, Accepted>>, StoreError> {
		let id = id.hash().to_multihash();
		let row = self
			.connection
			.prepare_cached(
				"SELECT change, tag, payload, depth, status FROM changes
					WHERE id = ?1 AND status IS NOT NULL",
			)
			.and_then(|mut statement| {
				statement
					.query_row([&id], |row| {
						Ok((
							row.get::<_, Vec<u8>>(0)?,
							row.get::<_, Vec<u8>>(1)?,
							row.get::<_, Option<Vec<u8>>>(2)?,
							row.get::<_, i64>(3)?,
							row.get::<_, i64>(4)?,
						))
					})
					.optional()
			})
			.map_err(database)?;
		let Some((encoded, tag, payload, depth, status)) = row else {
			return Ok(None);
		};
		Ok(Some(Cow::Owned(Accepted {
			change: self.checked(&id, &encoded, &tag)?,
			depth: u64::try_from(depth)
				.map_err(|_| StoreError::Damaged("a stored depth is negative".to_owned()))?,
			payload: payload_from(payload)?,
			keys: self.keys_of(&id)?,
			status: status_from(status)?,
		})))
	}

	fn change(&self, id: ChangeId) -> Result<Option<Cow<'_, Change>>, StoreError> {
		let id = id.hash().to_multihash();
		let row = self
			.connection
			.prepare_cached("SELECT change, tag FROM changes WHERE id = ?1 AND status IS NOT NULL")
			.and_then(|mut statement| {
				statement
					.query_row([&id], |row| {
						Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
					})
					.optional()
			})
			.map_err(database)?;
		row.map(|(encoded, tag)| Ok(Cow::Owned(self.checked(&id, &encoded, &tag)?)))
			.transpose()
	}

	fn status(&self, id: ChangeId) -> Result<Option<Status>, StoreError> {
		let status: Option<i64> = self
			.connection
			.prepare_cached("SELECT status FROM changes WHERE id = ?1 AND status IS NOT NULL")
			.and_then(|mut query| {
				query
					.query_row([id.hash().to_multihash()], |row| row.get(0))
					.optional()
			})
			.map_err(database)?;
		status.map(status_from).transpose()
	}

	fn depth(&self, id: ChangeId) -> Result<Option<u64>, StoreError> {
		let depth: Option<i64> = self
			.connection
			.prepare_cached("SELECT depth FROM changes WHERE id = ?1 AND status IS NOT NULL")
			.and_then(|mut statement| {
				statement
					.query_row([id.hash().to_multihash()], |row| row.get(0))
					.optional()
			})
			.map_err(database)?;
		depth
			.map(|depth| {
				u64::try_from(depth)
					.map_err(|_| StoreError::Damaged("a stored depth is negative".to_owned()))
			})
			.transpose()
	}

	fn contains(&self, id: ChangeId) -> Result<bool, StoreError> {
		self.exists(
			"SELECT 1 FROM changes WHERE id = ?1 AND status IS NOT NULL",
			id.hash().to_multihash(),
		)
	}

	fn ids(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM changes WHERE status IS NOT NULL ORDER BY id",
			[],
		)
	}

	fn summaries(&self) -> Result<Vec<Summary>, StoreError> {
		let mut query = self
			.connection
			.prepare_cached(
				"SELECT id, millis, depth, payload IS NOT NULL,
					EXISTS (SELECT 1 FROM change_keys WHERE change_keys.change = changes.id)
				FROM changes WHERE status IS NOT NULL ORDER BY id",
			)
			.map_err(database)?;
		let rows = query
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, i64>(1)?,
					row.get::<_, i64>(2)?,
					row.get::<_, bool>(3)?,
					row.get::<_, bool>(4)?,
				))
			})
			.map_err(database)?;
		let mut summaries = Vec::new();
		for row in rows {
			let (id, millis, depth, payload, key) = row.map_err(database)?;
			let number = |value: i64| {
				u64::try_from(value)
					.map_err(|_| StoreError::Damaged("a stored change's numbers".to_owned()))
			};
			summaries.push(Summary {
				id: change_id(&id)?,
				millis: number(millis)?,
				depth: number(depth)?,
				complete: payload && key,
			});
		}
		Ok(summaries)
	}

	fn count(&self) -> Result<usize, StoreError> {
		let count: i64 = self
			.connection
			.query_row(
				"SELECT count(*) FROM changes WHERE status IS NOT NULL",
				[],
				|row| row.get(0),
			)
			.map_err(database)?;
		usize::try_from(count).map_err(|_| StoreError::Damaged("a count".to_owned()))
	}

	fn heads(&self) -> Result<BTreeSet<ChangeId>, StoreError> {
		Ok(LogFile::ids(self, "SELECT change FROM heads", [])?
			.into_iter()
			.collect())
	}

	fn pending(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM changes WHERE status = 0 ORDER BY id",
			[],
		)
	}

	fn governing(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM changes WHERE kind = 1 AND status IS NOT NULL ORDER BY id",
			[],
		)
	}

	fn set_aside(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM changes WHERE status = 3 ORDER BY id",
			[],
		)
	}

	fn purging(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM changes WHERE status = 1 AND purging = 1 ORDER BY id",
			[],
		)
	}

	fn holder(&self, statement: StatementId) -> Result<Option<ChangeId>, StoreError> {
		let holder: Option<Vec<u8>> = self
			.connection
			.prepare_cached("SELECT change FROM holders WHERE statement = ?1")
			.and_then(|mut query| {
				query
					.query_row([statement.as_bytes().as_slice()], |row| row.get(0))
					.optional()
			})
			.map_err(database)?;
		holder.map(|holder| change_id(&holder)).transpose()
	}

	fn sealed(&self) -> Result<Vec<StatementId>, StoreError> {
		let mut query = self
			.connection
			.prepare_cached("SELECT statement FROM holders ORDER BY statement")
			.map_err(database)?;
		let rows = query
			.query_map([], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut sealed = Vec::new();
		for row in rows {
			sealed.push(statement_id(&row.map_err(database)?)?);
		}
		Ok(sealed)
	}

	fn purged_changes(&self) -> Result<Vec<ChangeId>, StoreError> {
		LogFile::ids(
			self,
			"SELECT id FROM purged WHERE kind = ?1 ORDER BY id",
			[PURGED_CHANGE],
		)
	}

	fn is_change_purged(&self, id: ChangeId) -> Result<bool, StoreError> {
		self.exists(
			&format!("SELECT 1 FROM purged WHERE kind = {PURGED_CHANGE} AND id = ?1"),
			id.hash().to_multihash(),
		)
	}

	fn is_statement_purged(&self, statement: StatementId) -> Result<bool, StoreError> {
		self.exists(
			&format!("SELECT 1 FROM purged WHERE kind = {PURGED_STATEMENT} AND id = ?1"),
			statement.as_bytes().as_slice(),
		)
	}

	fn purged_statements(&self) -> Result<Vec<StatementId>, StoreError> {
		let mut query = self
			.connection
			.prepare_cached("SELECT id FROM purged WHERE kind = ?1 ORDER BY id")
			.map_err(database)?;
		let rows = query
			.query_map([PURGED_STATEMENT], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut statements = Vec::new();
		for row in rows {
			statements.push(statement_id(&row.map_err(database)?)?);
		}
		Ok(statements)
	}

	fn purged_blobs(&self) -> Result<Vec<Hash>, StoreError> {
		let mut query = self
			.connection
			.prepare_cached("SELECT id FROM purged WHERE kind = ?1 ORDER BY id")
			.map_err(database)?;
		let rows = query
			.query_map([PURGED_BLOB], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut blobs = Vec::new();
		for row in rows {
			blobs.push(hash(&row.map_err(database)?)?);
		}
		Ok(blobs)
	}

	fn is_blob_purged(&self, sealed: &Hash) -> Result<bool, StoreError> {
		self.exists(
			&format!("SELECT 1 FROM purged WHERE kind = {PURGED_BLOB} AND id = ?1"),
			sealed.to_multihash(),
		)
	}

	fn add(&mut self, accepted: &Accepted) -> Result<(), StoreError> {
		let change = &accepted.change;
		let encoded = cbor::to_vec(change);
		let tag = self.tags.tag(&encoded);
		let id = change.id().hash().to_multihash();
		let timestamp = change.timestamp();
		let columns = params![
			id,
			encoded,
			tag.as_slice(),
			accepted.payload.as_ref().map(cbor::to_vec),
			i64::try_from(accepted.depth).unwrap_or(i64::MAX),
			status_to(accepted.status),
			timestamp.millis().cast_signed(),
			timestamp.counter(),
			!change.purges().changes.is_empty(),
			kind_to(change.kind()),
		];
		// A change that waited is in the file already, without a status: it moves to the
		// end, never taking a sequence that a state file's mark may name.
		let updated = self.write(
			"UPDATE changes SET change = ?2, tag = ?3, payload = ?4, depth = ?5, status = ?6,
				millis = ?7, counter = ?8, purging = ?9, kind = ?10,
				sequence = (SELECT max(sequence) + 1 FROM changes)
				WHERE id = ?1",
			columns,
		)?;
		if updated == 0 {
			self.write(
				"INSERT INTO changes
					(id, change, tag, payload, depth, status, millis, counter, purging, kind)
					VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
				columns,
			)?;
		}
		self.replace_change_keys(&id, &accepted.keys)?;
		for parent in change.parents() {
			self.write(
				"DELETE FROM heads WHERE change = ?1",
				params![parent.hash().to_multihash()],
			)?;
		}
		self.write(
			"INSERT OR IGNORE INTO heads (change) VALUES (?1)",
			params![id],
		)?;
		Ok(())
	}

	fn update(
		&mut self,
		id: ChangeId,
		payload: Option<&SealedPayload>,
		keys: &[WrappedChangeKey],
		status: Status,
	) -> Result<(), StoreError> {
		let id = id.hash().to_multihash();
		self.write(
			"UPDATE changes SET payload = ?2, status = ?3 WHERE id = ?1",
			params![id, payload.map(cbor::to_vec), status_to(status)],
		)?;
		self.replace_change_keys(&id, keys)?;
		self.move_to_end(&id)
	}

	fn set_holder(&mut self, statement: StatementId, change: ChangeId) -> Result<(), StoreError> {
		self.write(
			"INSERT OR REPLACE INTO holders (statement, change) VALUES (?1, ?2)",
			params![
				statement.as_bytes().as_slice(),
				change.hash().to_multihash()
			],
		)?;
		Ok(())
	}

	fn purge_change(&mut self, id: ChangeId) -> Result<(), StoreError> {
		let id = id.hash().to_multihash();
		self.write(
			"INSERT OR IGNORE INTO purged (kind, id) VALUES (?1, ?2)",
			params![PURGED_CHANGE, id],
		)?;
		self.write(
			"UPDATE changes SET payload = NULL, status = CASE status WHEN 3 THEN 3 ELSE 2 END
				WHERE id = ?1",
			params![id],
		)?;
		self.replace_change_keys(&id, &[])?;
		self.move_to_end(&id)
	}

	fn purge_statement(&mut self, statement: StatementId) -> Result<(), StoreError> {
		self.write(
			"INSERT OR IGNORE INTO purged (kind, id) VALUES (?1, ?2)",
			params![PURGED_STATEMENT, statement.as_bytes().as_slice()],
		)?;
		Ok(())
	}

	fn purge_blob(&mut self, sealed: Hash) -> Result<(), StoreError> {
		self.write(
			"INSERT OR IGNORE INTO purged (kind, id) VALUES (?1, ?2)",
			params![PURGED_BLOB, sealed.to_multihash()],
		)?;
		Ok(())
	}

	fn unpurge_change(&mut self, id: ChangeId) -> Result<(), StoreError> {
		let id = id.hash().to_multihash();
		self.write(
			"DELETE FROM purged WHERE kind = ?1 AND id = ?2",
			params![PURGED_CHANGE, id],
		)?;
		self.write(
			"UPDATE changes SET status = 0 WHERE id = ?1 AND status = 2",
			params![id],
		)?;
		self.move_to_end(&id)
	}

	fn unpurge_statement(&mut self, statement: StatementId) -> Result<(), StoreError> {
		self.write(
			"DELETE FROM purged WHERE kind = ?1 AND id = ?2",
			params![PURGED_STATEMENT, statement.as_bytes().as_slice()],
		)?;
		Ok(())
	}

	fn unpurge_blob(&mut self, sealed: Hash) -> Result<(), StoreError> {
		self.write(
			"DELETE FROM purged WHERE kind = ?1 AND id = ?2",
			params![PURGED_BLOB, sealed.to_multihash()],
		)?;
		Ok(())
	}

	fn clear(&mut self) -> Result<(), StoreError> {
		// The file is the history: what it holds stays.
		Ok(())
	}
}

/// A change's kind, as its column holds it.
const fn kind_to(kind: Kind) -> i64 {
	match kind {
		Kind::Ordinary => 0,
		Kind::Governing => 1,
	}
}

/// A sealed payload from its column.
pub(crate) fn payload_from(payload: Option<Vec<u8>>) -> Result<Option<SealedPayload>, StoreError> {
	payload
		.map(|payload| cbor::from_slice(&payload))
		.transpose()
		.map_err(|error| StoreError::Damaged(format!("a stored payload: {error}")))
}

/// A change's ID from its bytes, a multihash.
pub(crate) fn change_id(bytes: &[u8]) -> Result<ChangeId, StoreError> {
	ChangeId::from_cbor(&cbor::Value::Bytes(bytes.to_vec()))
		.map_err(|error| StoreError::Damaged(format!("a stored change ID: {error}")))
}

/// Of these sealed blobs, those that the log file `log`, of another space of the replica,
/// holds a wrapped key for now, read through a connection of its own that only reads.
pub(crate) fn blob_keys_in(log: &Path, sealed: &[Hash]) -> rusqlite::Result<Vec<Hash>> {
	let connection = Connection::open_with_flags(
		log,
		rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
	)?;
	connection.busy_timeout(BUSY_TIMEOUT)?;
	steady_plans(&connection)?;
	let mut query = connection.prepare("SELECT 1 FROM blob_keys WHERE sealed = ?1 LIMIT 1")?;
	let mut found = Vec::new();
	for sealed in sealed {
		let row: Option<i64> = query
			.query_row(params![sealed.to_multihash()], |row| row.get(0))
			.optional()?;
		if row.is_some() {
			found.push(*sealed);
		}
	}
	Ok(found)
}

/// Of these sealed blobs, those whose keys the log file at `log` keeps apart, as a purge
/// destroyed them while its replica waits.
pub(crate) fn kept_blobs_in(log: &Path, sealed: &[Hash]) -> rusqlite::Result<Vec<Hash>> {
	let connection = Connection::open_with_flags(
		log,
		rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
	)?;
	connection.busy_timeout(BUSY_TIMEOUT)?;
	let exists: bool = connection
		.prepare("SELECT 1 FROM sqlite_schema WHERE name = 'kept'")?
		.exists([])?;
	if !exists {
		return Ok(Vec::new());
	}
	let mut query = connection.prepare("SELECT 1 FROM kept WHERE kind = 2 AND id = ?1")?;
	let mut found = Vec::new();
	for sealed in sealed {
		if query.exists(params![sealed.to_multihash()])? {
			found.push(*sealed);
		}
	}
	Ok(found)
}

/// A statement's ID from its 16 bytes.
pub(crate) fn statement_id(bytes: &[u8]) -> Result<StatementId, StoreError> {
	let bytes: [u8; 16] = bytes.try_into().map_err(|_| {
		StoreError::Damaged("a stored statement ID has the wrong length".to_owned())
	})?;
	Ok(StatementId::from_bytes(bytes))
}

/// A hash from its bytes, a multihash.
pub(crate) fn hash(bytes: &[u8]) -> Result<Hash, StoreError> {
	Hash::from_multihash(bytes)
		.map_err(|error| StoreError::Damaged(format!("a stored hash: {error}")))
}

/// What became of a payload, as its column holds it.
const fn status_to(status: Status) -> i64 {
	match status {
		Status::Pending => 0,
		Status::Applied => 1,
		Status::Ignored => 2,
		Status::SetAside => 3,
	}
}

fn status_from(status: i64) -> Result<Status, StoreError> {
	match status {
		0 => Ok(Status::Pending),
		1 => Ok(Status::Applied),
		2 => Ok(Status::Ignored),
		3 => Ok(Status::SetAside),
		_ => Err(StoreError::Damaged("a stored status".to_owned())),
	}
}

/// Checks that the file is a log file of this version, or makes an empty file one. Writes
/// nothing to a file that is neither.
fn initialize(connection: &mut Connection) -> Result<(), StoreError> {
	let transaction = connection
		.transaction_with_behavior(TransactionBehavior::Immediate)
		.map_err(database)?;
	let application_id: i64 = transaction
		.query_row("PRAGMA application_id", [], |row| row.get(0))
		.map_err(database)?;
	let version: i64 = transaction
		.query_row("PRAGMA user_version", [], |row| row.get(0))
		.map_err(database)?;
	match (application_id, version) {
		(APPLICATION_ID, VERSION) => {}
		(0, 0) => {
			let objects: i64 = transaction
				.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
				.map_err(database)?;
			if objects > 0 {
				return Err(StoreError::NotALog);
			}
			transaction
				.execute_batch(&format!(
					"PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {VERSION}; {TABLES}"
				))
				.map_err(database)?;
		}
		// Only a newer program reads it, and nothing is written to it.
		(APPLICATION_ID, version) if version > VERSION => return Err(StoreError::Newer),
		_ => return Err(StoreError::NotALog),
	}
	transaction.commit().map_err(database)
}

/// Has every commit and checkpoint reach the disk before it returns: full synchronization,
/// and on Apple's systems, where a plain sync leaves data in the drive's cache, `F_FULLFSYNC`.
pub(crate) fn synchronize_fully(connection: &Connection) -> rusqlite::Result<()> {
	for (pragma, value) in [
		("synchronous", "FULL"),
		("fullfsync", "ON"),
		("checkpoint_fullfsync", "ON"),
	] {
		connection.pragma_update(None, pragma, value)?;
	}
	Ok(())
}

/// Has SQLite plan each statement once, whatever values are bound to it: its query planner
/// stability guarantee. Without it, SQLite 3.54 prepares a statement again whenever a
/// number compared with a column changes between 0 or 1 and any other, which made the
/// state file's reads eight times slower; no statement of NibKeep needs a plan made for
/// its values.
pub(crate) fn steady_plans(connection: &Connection) -> rusqlite::Result<()> {
	connection
		.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG, true)
		.map(|_| ())
}

/// Turns a database error into a store error: a file that is not a database is not a log.
/// SQLite's count of the changes that other connections, of this program or another,
/// committed to a database since this connection opened it: when it moves, another wrote.
pub(crate) fn data_version(connection: &Connection) -> rusqlite::Result<i64> {
	connection.query_row("PRAGMA data_version", [], |row| row.get(0))
}

pub(crate) fn database(error: rusqlite::Error) -> StoreError {
	match error.sqlite_error_code() {
		Some(ErrorCode::NotADatabase) => StoreError::NotALog,
		_ => StoreError::Database(error),
	}
}
