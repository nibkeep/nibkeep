//! The sync role ([technical design](../../../docs/Technical-design.md#105-the-sync-role)): one program
//! on each replica syncs it for all, the one that holds the lock on `sync.lock` in the
//! replica's folder. The holder runs the replica's endpoint, answers the replicas that
//! connect, and connects to them as changes come and replicas appear. The others ask it
//! through `sync.sqlite`, an encrypted database beside it, where it records what it does.
//! A program that pairs borrows the network, by locking `sync.lent`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use nibkeep_base::{EntityId, Hash};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_net::{NetError, Network, Options};
use nibkeep_store::EncryptedDatabase;
use rusqlite::{OptionalExtension, params};

use crate::library::backups::{Backup, BackupUpdate};
use crate::library::checking::{CheckRun, Pace, Part, Round};
use crate::network::{
	Answered, RemovalPass, SyncedWith, answer_connection, remove_unkept, retry_in_use,
	sync_with_found,
};
use crate::{Closed, Library, NodeError};

/// The file whose lock is the sync role.
const ROLE: &str = "sync.lock";

/// The file that a program locks while it borrows the network to pair.
const LENT: &str = "sync.lent";

/// The database through which programs ask the holder, and read what it did.
const RECORDS: &str = "sync.sqlite";

/// The context of the key of the records, derived from the replica's storage key.
const RECORDS_CONTEXT: &str = "nibkeep 2026-10 sync records";

/// How long a step of the holder waits for a connection, between looking at the rest.
const STEP: Duration = Duration::from_millis(250);

/// How long a program that takes the sync role tries, as another may hold its lock for a
/// moment only to see whether a program holds the role.
const LOOKING: Duration = Duration::from_millis(50);

/// How long a program waits to open the library, for a program that has it to itself.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// How long a program that borrows the network tries to lock `sync.lent`, which the holder
/// looks at for a moment now and then, before it takes it that another borrows it.
const LENT_TRY: Duration = Duration::from_secs(2);

/// How soon a pass of removals that had room for no more is followed by another.
const REMOVE_SOON: Duration = Duration::from_secs(10);

/// How long a program that borrows the network waits for the holder to stop its endpoint,
/// once the syncs under way end.
const LEND_WAIT: Duration = Duration::from_mins(1);

/// The sync role of a replica, held: the lock on its `sync.lock`, which the system lets go
/// of when the program ends, even by a crash.
#[derive(Debug)]
pub struct Role {
	_lock: File,
}

fn open_lock(path: &Path) -> Result<File, NodeError> {
	OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(path)
		.map_err(NodeError::file(path))
}

/// Takes the lock on a file alone, unless another holds it: whether it did.
fn try_alone(file: &File, path: &Path) -> Result<bool, NodeError> {
	match file.try_lock() {
		Ok(()) => Ok(true),
		Err(TryLockError::WouldBlock) => Ok(false),
		Err(TryLockError::Error(error)) => Err(NodeError::file(path)(error)),
	}
}

impl Role {
	/// Takes the sync role of a closed library's replica, unless another program, or
	/// another part of this one, holds it.
	///
	/// # Errors
	///
	/// Returns an error if the lock's file cannot be opened or locked.
	pub fn take(closed: &Closed) -> Result<Option<Self>, NodeError> {
		Self::take_at(closed.directory())
	}

	/// Takes the sync role of the replica in `directory`, as [`Role::take`] does.
	fn take_at(directory: &Path) -> Result<Option<Self>, NodeError> {
		let path = directory.join(ROLE);
		let file = open_lock(&path)?;
		let tried = Instant::now();
		loop {
			if try_alone(&file, &path)? {
				return Ok(Some(Self { _lock: file }));
			}
			if tried.elapsed() >= LOOKING {
				return Ok(None);
			}
			std::thread::sleep(Duration::from_millis(5));
		}
	}

	/// Whether a program holds the sync role of the replica in `directory`: its lock, taken
	/// for a moment, tells.
	fn held_at(directory: &Path) -> Result<bool, NodeError> {
		let path = directory.join(ROLE);
		let file = open_lock(&path)?;
		Ok(!try_alone(&file, &path)?)
	}

	/// Waits for the sync role, taking it as soon as the program that holds it lets go, or
	/// ends, until `stop` is set.
	///
	/// # Errors
	///
	/// Returns an error if the lock's file cannot be opened or locked.
	pub fn wait(closed: &Closed, stop: &AtomicBool) -> Result<Option<Self>, NodeError> {
		loop {
			if let Some(role) = Self::take(closed)? {
				return Ok(Some(role));
			}
			if stop.load(Ordering::SeqCst) {
				return Ok(None);
			}
			std::thread::sleep(STEP);
		}
	}
}

/// What the holder of the sync role records of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HolderRecord {
	/// The program that holds the role, as it names itself.
	pub program: String,
	/// The UDP port its endpoint takes connections on, or `None` while it runs none, as
	/// while another program borrows the network to pair.
	pub port: Option<u16>,
	/// Since when it holds the role.
	pub since: u64,
}

/// What the holder of the sync role records of another replica.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplicaRecord {
	/// Whether it is on the network now.
	pub present: bool,
	/// When it was last seen on the network.
	pub seen: Option<u64>,
	/// When it last synced, or tried to.
	pub synced: Option<u64>,
	/// How many changes that sync brought here.
	pub received: usize,
	/// How many it sent there.
	pub sent: usize,
	/// Why it failed, if it did.
	pub error: Option<String>,
}

/// The records of the sync role, in `sync.sqlite` beside its lock, encrypted under a key of
/// the replica's: who holds the role, what other programs ask it, and what it did.
pub struct Records {
	database: EncryptedDatabase,
	/// The replica's folder, where the role's lock is.
	directory: PathBuf,
}

const TABLES: &str = "
CREATE TABLE IF NOT EXISTS holder (
	one INTEGER PRIMARY KEY CHECK (one = 1),
	program TEXT NOT NULL,
	port INTEGER,
	since INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS asks (
	n INTEGER PRIMARY KEY,
	replica BLOB,
	asked INTEGER NOT NULL,
	done INTEGER
);
CREATE TABLE IF NOT EXISTS replicas (
	replica BLOB PRIMARY KEY,
	present INTEGER NOT NULL DEFAULT 0,
	seen INTEGER,
	synced INTEGER,
	received INTEGER NOT NULL DEFAULT 0,
	sent INTEGER NOT NULL DEFAULT 0,
	error TEXT
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS checking (
	one INTEGER PRIMARY KEY CHECK (one = 1),
	began INTEGER NOT NULL,
	blobs_total INTEGER NOT NULL,
	blobs_read INTEGER NOT NULL,
	blobs_after BLOB,
	blobs_done INTEGER NOT NULL,
	plain_total INTEGER NOT NULL,
	plain_read INTEGER NOT NULL,
	plain_after BLOB,
	plain_done INTEGER NOT NULL,
	last INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS holds (
	sealed BLOB PRIMARY KEY,
	until INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS removing (
	sealed BLOB PRIMARY KEY,
	until INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS unkept (
	sealed BLOB PRIMARY KEY,
	since INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS backups (
	backup BLOB PRIMARY KEY,
	folder TEXT NOT NULL,
	updated INTEGER,
	kept INTEGER
) WITHOUT ROWID;
";

/// How long a replica holds a sealed blob that it proved to another, so that it does not
/// remove it meanwhile, and how long a mark of a blob it is removing stands at most:
/// 10 minutes ([technical design: the planner]).
///
/// [technical design: the planner]: ../../../docs/Technical-design.md#99-the-planner
pub(crate) const HOLD_FOR: u64 = 600_000;

fn database(error: rusqlite::Error) -> NodeError {
	NodeError::Store(nibkeep_store::StoreError::Database(error))
}

fn millis(at: u64) -> i64 {
	i64::try_from(at).unwrap_or(i64::MAX)
}

fn unsigned(value: Option<i64>) -> Option<u64> {
	value.and_then(|value| u64::try_from(value).ok())
}

impl Library {
	/// Takes the sync role of the library's replica, as [`Role::take`] does: for a program
	/// that has it open.
	///
	/// # Errors
	///
	/// Returns an error if the lock's file cannot be opened or locked.
	pub fn take_role(&self) -> Result<Option<Role>, NodeError> {
		Role::take_at(self.directory())
	}

	/// The records of the replica's sync role, which programs read and write through an
	/// encrypted database beside its lock; `rng` makes the database's own key and nonces.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be opened or made.
	pub fn sync_records(&self, rng: impl CryptoRng + Send + 'static) -> Result<Records, NodeError> {
		let key = self.derived_key(RECORDS_CONTEXT);
		let opened = EncryptedDatabase::open(&self.directory().join(RECORDS), &key, rng)?;
		opened
			.connection()
			.execute_batch(TABLES)
			.map_err(database)?;
		Ok(Records {
			database: opened,
			directory: self.directory().to_path_buf(),
		})
	}
}

impl Records {
	/// Who holds the role, as it recorded, if any program does: a record that a program
	/// left as it ended, as by a crash or as an app quits, says nothing once its lock is
	/// free.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read, or the lock's file opened.
	pub fn holder(&self) -> Result<Option<HolderRecord>, NodeError> {
		if !Role::held_at(&self.directory)? {
			return Ok(None);
		}
		self.database
			.connection()
			.query_row(
				"SELECT program, port, since FROM holder WHERE one = 1",
				[],
				|row| {
					Ok(HolderRecord {
						program: row.get(0)?,
						port: row
							.get::<_, Option<i64>>(1)?
							.and_then(|port| u16::try_from(port).ok()),
						since: unsigned(row.get(2)?).unwrap_or(0),
					})
				},
			)
			.optional()
			.map_err(database)
	}

	fn set_holder(&self, program: &str, port: Option<u16>, since: u64) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT INTO holder (one, program, port, since) VALUES (1, ?1, ?2, ?3)
				 ON CONFLICT (one) DO UPDATE SET program = ?1, port = ?2, since = ?3",
				params![program, port, millis(since)],
			)
			.map(drop)
			.map_err(database)
	}

	/// Clears who holds the role, and who is on the network, which only a holder that runs
	/// knows: as a holder ends, and as another takes the role after one that ended
	/// without clearing them, as by a crash.
	fn clear_holder(&self) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute_batch("DELETE FROM holder; UPDATE replicas SET present = 0;")
			.map_err(database)
	}

	/// Asks the holder to sync with one replica, or with all it finds: returns the ask,
	/// whose answer [`Records::answered`] tells.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be written.
	pub fn ask(&self, replica: Option<EntityId>, at: u64) -> Result<i64, NodeError> {
		let connection = self.database.connection();
		connection
			.execute(
				"INSERT INTO asks (replica, asked) VALUES (?1, ?2)",
				params![
					replica.map(|replica| replica.as_bytes().to_vec()),
					millis(at)
				],
			)
			.map_err(database)?;
		Ok(connection.last_insert_rowid())
	}

	/// Where the round of checks under way stands, if one began ([technical design: checking
	/// and repair]).
	///
	/// [technical design: checking and repair]: ../../../docs/Technical-design.md#97-checking-and-repair
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read.
	pub fn round(&self) -> Result<Option<Round>, NodeError> {
		Ok(self.round_with_last()?.map(|(round, _)| round))
	}

	/// Where the round of checks under way stands, with when a check last ran.
	pub(crate) fn round_with_last(&self) -> Result<Option<(Round, Option<u64>)>, NodeError> {
		let part = |row: &rusqlite::Row<'_>, at: usize| -> rusqlite::Result<Part> {
			Ok(Part {
				total: unsigned(row.get(at)?).unwrap_or(0),
				read: unsigned(row.get(at + 1)?).unwrap_or(0),
				after: row.get(at + 2)?,
				done: row.get(at + 3)?,
			})
		};
		self.database
			.connection()
			.query_row(
				"SELECT began, blobs_total, blobs_read, blobs_after, blobs_done,
					plain_total, plain_read, plain_after, plain_done, last
				 FROM checking WHERE one = 1",
				[],
				|row| {
					Ok((
						Round {
							began: unsigned(row.get(0)?).unwrap_or(0),
							blobs: part(row, 1)?,
							plain: part(row, 5)?,
						},
						unsigned(row.get(9)?),
					))
				},
			)
			.optional()
			.map_err(database)
	}

	/// Saves where the round of checks stands, as a check that ran until `last` left it.
	pub(crate) fn save_round(&self, round: &Round, last: u64) -> Result<(), NodeError> {
		let size = |bytes: u64| i64::try_from(bytes).unwrap_or(i64::MAX);
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO checking (one, began, blobs_total, blobs_read, blobs_after,
					blobs_done, plain_total, plain_read, plain_after, plain_done, last)
				 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
				params![
					millis(round.began),
					size(round.blobs.total),
					size(round.blobs.read),
					round.blobs.after,
					round.blobs.done,
					size(round.plain.total),
					size(round.plain.read),
					round.plain.after,
					round.plain.done,
					millis(last),
				],
			)
			.map(drop)
			.map_err(database)
	}

	/// When the holder answered an ask, if it did: once the sync with those of the replicas
	/// asked for that it found on the network ended, whose records then say how it went.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read.
	pub fn answered(&self, ask: i64) -> Result<Option<u64>, NodeError> {
		Ok(unsigned(
			self.database
				.connection()
				.query_row("SELECT done FROM asks WHERE n = ?1", [ask], |row| {
					row.get(0)
				})
				.optional()
				.map_err(database)?
				.flatten(),
		))
	}

	/// The asks not answered yet: each, and the replica it names, or `None` for all.
	fn open_asks(&self) -> Result<Vec<(i64, Option<EntityId>)>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare_cached("SELECT n, replica FROM asks WHERE done IS NULL ORDER BY n")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				let replica: Option<Vec<u8>> = row.get(1)?;
				Ok((row.get(0)?, replica))
			})
			.map_err(database)?;
		let mut asks = Vec::new();
		for row in rows {
			let (ask, replica) = row.map_err(database)?;
			let replica = replica
				.and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
				.map(EntityId::from_bytes);
			asks.push((ask, replica));
		}
		Ok(asks)
	}

	fn answer(&self, ask: i64, at: u64) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"UPDATE asks SET done = ?2 WHERE n = ?1",
				params![ask, millis(at)],
			)
			.map(drop)
			.map_err(database)
	}

	/// What the holder recorded of each other replica. Only a holder that runs knows which
	/// are on the network: once its lock is free, none is, whatever it left recorded.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read, or the lock's file opened.
	pub fn replicas(&self) -> Result<BTreeMap<EntityId, ReplicaRecord>, NodeError> {
		let held = Role::held_at(&self.directory)?;
		let connection = self.database.connection();
		let mut statement = connection
			.prepare_cached(
				"SELECT replica, present, seen, synced, received, sent, error FROM replicas",
			)
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				let replica: Vec<u8> = row.get(0)?;
				let present: i64 = row.get(1)?;
				let received: i64 = row.get(4)?;
				let sent: i64 = row.get(5)?;
				Ok((
					replica,
					ReplicaRecord {
						present: held && present != 0,
						seen: unsigned(row.get(2)?),
						synced: unsigned(row.get(3)?),
						received: usize::try_from(received).unwrap_or(0),
						sent: usize::try_from(sent).unwrap_or(0),
						error: row.get(6)?,
					},
				))
			})
			.map_err(database)?;
		let mut records = BTreeMap::new();
		for row in rows {
			let (replica, record) = row.map_err(database)?;
			if let Ok(replica) = <[u8; 16]>::try_from(replica) {
				records.insert(EntityId::from_bytes(replica), record);
			}
		}
		Ok(records)
	}

	/// What the holder recorded of each replica that it synced with at `asked` or later, by
	/// the wall clock, as in answer to an ask made then: a sync recorded before was not the
	/// one asked for.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read.
	pub fn synced_since(&self, asked: u64) -> Result<BTreeMap<EntityId, ReplicaRecord>, NodeError> {
		let mut records = self.replicas()?;
		records.retain(|_, record| record.synced.is_some_and(|at| at >= asked));
		Ok(records)
	}

	fn record_presence(&self, replica: EntityId, present: bool, at: u64) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT INTO replicas (replica, present, seen) VALUES (?1, ?2, ?3)
				 ON CONFLICT (replica) DO UPDATE SET present = ?2,
				 seen = CASE WHEN ?2 THEN ?3 ELSE seen END",
				params![replica.as_bytes().to_vec(), present, millis(at)],
			)
			.map(drop)
			.map_err(database)
	}

	fn record_sync(
		&self,
		replica: EntityId,
		at: u64,
		result: Result<(usize, usize), String>,
	) -> Result<(), NodeError> {
		let (received, sent, error) = match result {
			Ok((received, sent)) => (received, sent, None),
			Err(error) => (0, 0, Some(error)),
		};
		self.database
			.connection()
			.execute(
				"INSERT INTO replicas (replica, present, seen, synced, received, sent, error)
				 VALUES (?1, 1, ?2, ?2, ?3, ?4, ?5)
				 ON CONFLICT (replica) DO UPDATE SET present = 1, seen = ?2, synced = ?2,
				 received = ?3, sent = ?4, error = ?5",
				params![
					replica.as_bytes().to_vec(),
					millis(at),
					i64::try_from(received).unwrap_or(i64::MAX),
					i64::try_from(sent).unwrap_or(i64::MAX),
					error
				],
			)
			.map(drop)
			.map_err(database)
	}

	/// Holds these sealed blobs for another replica that is about to remove its copies, as
	/// this one proves them at `at`: it removes none of them for [`HOLD_FOR`]. Returns those
	/// held, which are all but those it is removing itself, which it proves to nobody.
	pub(crate) fn hold(&self, sealed: &[Hash], at: u64) -> Result<BTreeSet<Hash>, NodeError> {
		self.marking(sealed, at, ("holds", "removing"))
	}

	/// Marks these sealed blobs as being removed at `at`, so that none of them is proved to
	/// another replica meanwhile. Returns those marked, which are all but those it holds for
	/// another replica, which it does not remove.
	pub(crate) fn begin_removing(
		&self,
		sealed: &[Hash],
		at: u64,
	) -> Result<BTreeSet<Hash>, NodeError> {
		self.marking(sealed, at, ("removing", "holds"))
	}

	/// Clears the marks of blobs that it was removing, once they are gone or stay after all.
	pub(crate) fn end_removing(&self, sealed: &[Hash]) -> Result<(), NodeError> {
		let connection = self.database.connection();
		let transaction = rusqlite::Transaction::new_unchecked(
			connection,
			rusqlite::TransactionBehavior::Immediate,
		)
		.map_err(database)?;
		{
			let mut delete = transaction
				.prepare_cached("DELETE FROM removing WHERE sealed = ?1")
				.map_err(database)?;
			for each in sealed {
				delete.execute([each.to_multihash()]).map_err(database)?;
			}
		}
		transaction.commit().map_err(database)
	}

	/// Since when this replica no longer keeps each of these sealed blobs that it holds, as
	/// its passes of removals first found it so, `at` for those found now: a device that waits
	/// before purging removes none before its wait has passed since then
	/// ([technical design: copies that wait]).
	///
	/// [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait
	pub(crate) fn unkept_since(
		&self,
		sealed: &[Hash],
		at: u64,
	) -> Result<BTreeMap<Hash, u64>, NodeError> {
		let connection = self.database.connection();
		let transaction = rusqlite::Transaction::new_unchecked(
			connection,
			rusqlite::TransactionBehavior::Immediate,
		)
		.map_err(database)?;
		let mut since = BTreeMap::new();
		{
			let mut insert = transaction
				.prepare_cached("INSERT OR IGNORE INTO unkept (sealed, since) VALUES (?1, ?2)")
				.map_err(database)?;
			let mut select = transaction
				.prepare_cached("SELECT since FROM unkept WHERE sealed = ?1")
				.map_err(database)?;
			for each in sealed {
				insert
					.execute(rusqlite::params![each.to_multihash(), millis(at)])
					.map_err(database)?;
				let found: i64 = select
					.query_row([each.to_multihash()], |row| row.get(0))
					.map_err(database)?;
				since.insert(*each, u64::try_from(found).unwrap_or(0));
			}
		}
		transaction.commit().map_err(database)?;
		Ok(since)
	}

	/// Forgets since when blobs were no longer kept, but for those in `still`: a blob kept
	/// again, or gone, starts anew if it is no longer kept later.
	pub(crate) fn forget_unkept_but(&self, still: &BTreeSet<Hash>) -> Result<(), NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare_cached("SELECT sealed FROM unkept")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut gone = Vec::new();
		for row in rows {
			let bytes = row.map_err(database)?;
			if Hash::from_multihash(&bytes).map_or(true, |sealed| !still.contains(&sealed)) {
				gone.push(bytes);
			}
		}
		for bytes in gone {
			connection
				.execute("DELETE FROM unkept WHERE sealed = ?1", [bytes])
				.map_err(database)?;
		}
		Ok(())
	}

	/// The sealed blobs that this replica holds for others at `at`, as it proved them.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read.
	pub fn holds(&self, at: u64) -> Result<BTreeSet<Hash>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare_cached("SELECT sealed FROM holds WHERE until > ?1")
			.map_err(database)?;
		let rows = statement
			.query_map([millis(at)], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database)?;
		let mut held = BTreeSet::new();
		for row in rows {
			if let Ok(sealed) = Hash::from_multihash(&row.map_err(database)?) {
				held.insert(sealed);
			}
		}
		Ok(held)
	}

	/// Marks these sealed blobs in the table `(marked, against)`'s first, until [`HOLD_FOR`]
	/// after `at`, but those that its second marks now, in one transaction that no other
	/// program's marks come between; marks that ended go. Returns those marked.
	fn marking(
		&self,
		sealed: &[Hash],
		at: u64,
		(marked, against): (&str, &str),
	) -> Result<BTreeSet<Hash>, NodeError> {
		let connection = self.database.connection();
		let transaction = rusqlite::Transaction::new_unchecked(
			connection,
			rusqlite::TransactionBehavior::Immediate,
		)
		.map_err(database)?;
		let now = millis(at);
		let until = millis(at.saturating_add(HOLD_FOR));
		let mut done = BTreeSet::new();
		{
			transaction
				.execute(&format!("DELETE FROM {marked} WHERE until <= ?1"), [now])
				.map_err(database)?;
			transaction
				.execute(&format!("DELETE FROM {against} WHERE until <= ?1"), [now])
				.map_err(database)?;
			let mut barred = transaction
				.prepare_cached(&format!("SELECT 1 FROM {against} WHERE sealed = ?1"))
				.map_err(database)?;
			let mut mark = transaction
				.prepare_cached(&format!(
					"INSERT INTO {marked} (sealed, until) VALUES (?1, ?2)
					 ON CONFLICT (sealed) DO UPDATE SET until = max(until, ?2)"
				))
				.map_err(database)?;
			for each in sealed {
				let bytes = each.to_multihash();
				if barred.exists([&bytes]).map_err(database)? {
					continue;
				}
				mark.execute(params![bytes, until]).map_err(database)?;
				done.insert(*each);
			}
		}
		transaction.commit().map_err(database)?;
		Ok(done)
	}

	/// The person's backups that the holder found on this computer: where it last found
	/// each, and when it last updated it here, or tried to.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be read.
	pub fn backup_folders(&self) -> Result<BTreeMap<EntityId, FoundBackup>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare_cached("SELECT backup, folder, updated, kept FROM backups")
			.map_err(database)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, String>(1)?,
					row.get::<_, Option<i64>>(2)?,
					row.get::<_, Option<i64>>(3)?,
				))
			})
			.map_err(database)?;
		let mut found = BTreeMap::new();
		for row in rows {
			let (backup, folder, updated, kept) = row.map_err(database)?;
			if let Ok(bytes) = <[u8; 16]>::try_from(backup.as_slice()) {
				found.insert(
					EntityId::from_bytes(bytes),
					FoundBackup {
						folder: PathBuf::from(folder),
						updated: unsigned(updated),
						kept: unsigned(kept),
					},
				);
			}
		}
		Ok(found)
	}

	/// Records where the holder found a backup, and, if it updated it now or tried to,
	/// when, and how many changes the replica kept then.
	///
	/// # Errors
	///
	/// Returns an error if the database cannot be written.
	pub fn record_backup(
		&self,
		backup: EntityId,
		folder: &Path,
		updated: Option<(u64, usize)>,
	) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT INTO backups (backup, folder, updated, kept) VALUES (?1, ?2, ?3, ?4)
				 ON CONFLICT (backup) DO UPDATE
				 SET folder = excluded.folder,
					updated = coalesce(excluded.updated, updated),
					kept = coalesce(excluded.kept, kept)",
				params![
					backup.as_bytes().as_slice(),
					folder.to_string_lossy(),
					updated.map(|(at, _)| millis(at)),
					updated.map(|(_, kept)| i64::try_from(kept).unwrap_or(i64::MAX))
				],
			)
			.map_err(database)?;
		Ok(())
	}

	/// SQLite's count of what other connections committed, which moves when a program asks.
	fn version(&self) -> Result<i64, NodeError> {
		self.database
			.connection()
			.query_row("PRAGMA data_version", [], |row| row.get(0))
			.map_err(database)
	}
}

/// The network, borrowed from the holder of the sync role to pair, until this goes: the
/// lock on `sync.lent`, and the role itself if no program held it.
#[derive(Debug)]
pub struct Borrowed {
	_lent: File,
	_role: Option<Role>,
}

/// Borrows the network of a library's replica, to pair: locks `sync.lent`, which tells the
/// holder of the sync role to stop its endpoint, and waits until it records that it runs
/// none, or `stop` is set; or, if no program holds the role, takes it, so that none starts
/// an endpoint meanwhile. The endpoint starts again once the borrowed network goes.
///
/// # Errors
///
/// Returns [`NodeError::NetworkBusy`] if another program borrows it already, or the holder
/// keeps its endpoint; [`NodeError::PairingExpired`] once `stop` is set, as pairing does;
/// and an error if the files cannot be read or written.
pub fn borrow_network(
	library: &Library,
	stop: &AtomicBool,
	rng: impl CryptoRng + Send + 'static,
) -> Result<Borrowed, NodeError> {
	let path = library.directory().join(LENT);
	let lent = open_lock(&path)?;
	// The holder looks at the lock now and then, for a moment.
	let tried = Instant::now();
	while !try_alone(&lent, &path)? {
		if tried.elapsed() > LENT_TRY {
			return Err(NodeError::NetworkBusy);
		}
		std::thread::sleep(Duration::from_millis(20));
	}
	if let Some(role) = library.take_role()? {
		return Ok(Borrowed {
			_lent: lent,
			_role: Some(role),
		});
	}
	let records = library.sync_records(rng)?;
	let waited = Instant::now();
	loop {
		match records.holder()? {
			Some(holder) if holder.port.is_some() => {}
			// It stopped its endpoint, or ended without a record, as by a crash.
			_ => {
				return Ok(Borrowed {
					_lent: lent,
					_role: None,
				});
			}
		}
		if let Some(role) = library.take_role()? {
			return Ok(Borrowed {
				_lent: lent,
				_role: Some(role),
			});
		}
		if waited.elapsed() > LEND_WAIT {
			return Err(NodeError::NetworkBusy);
		}
		if stop.load(Ordering::SeqCst) {
			return Err(NodeError::PairingExpired);
		}
		std::thread::sleep(STEP);
	}
}

/// Whether a program borrows the network of the replica in `closed`.
fn lent(closed: &Closed) -> Result<bool, NodeError> {
	let path = closed.directory().join(LENT);
	let file = open_lock(&path)?;
	match file.try_lock_shared() {
		Ok(()) => {
			let _ = file.unlock();
			Ok(false)
		}
		Err(TryLockError::WouldBlock) => Ok(true),
		Err(TryLockError::Error(error)) => Err(NodeError::file(&path)(error)),
	}
}

/// How the holder of the sync role runs.
pub struct Holding<'a> {
	/// The program that holds the role, as the records name it, such as `nib sync --serve`.
	pub program: &'a str,
	/// How its endpoint starts: it takes connections on the first free port from
	/// [`Options::port`], within ten, then on any.
	pub network: Options,
	/// Where other replicas are, found other than on the network, as in tests.
	pub known: Vec<([u8; 32], Vec<SocketAddr>)>,
	/// How long it waits after new changes before it syncs them, so that the changes of a
	/// burst travel together.
	pub quiet: Duration,
	/// How long it waits at most after another replica appears before it connects, unless
	/// that one connects meanwhile: a random wait up to this, so that two holders that see
	/// each other at once seldom connect to each other at once.
	pub appear_wait: Duration,
	/// How it paces its checks of the replica's copies.
	pub check: Pace,
	/// Set, it lets go of the role, once the syncs under way end.
	pub stop: &'a AtomicBool,
	/// The wall clock.
	pub now: &'a (dyn Fn() -> u64 + Sync),
}

impl Holding<'_> {
	/// How long the holder waits after new changes, by default: a few seconds.
	pub const QUIET: Duration = Duration::from_secs(3);

	/// How long it waits at most after another replica appears, by default.
	pub const APPEAR_WAIT: Duration = Duration::from_secs(5);
}

/// What the holder of the sync role did, for the program to show.
#[derive(Debug)]
pub enum Held {
	/// Its endpoint started, on this port, or another than the one asked for, with why.
	Started {
		/// The port it takes connections on.
		port: Option<u16>,
		/// The port asked for, and why it was not had, if it took another.
		refused: Option<(u16, String)>,
	},
	/// It stopped its endpoint, as another program borrows the network.
	Lent,
	/// It answered a sync that another replica started.
	Answered(Answered),
	/// It synced with another replica, which it connected to.
	Synced(SyncedWith),
	/// It checked a share of the replica's copies.
	Checked(CheckRun),
	/// It ran a pass of removals of the copies that the replica no longer keeps, or why it
	/// could not.
	Removed(Result<RemovalPass, NodeError>),
	/// It updated one of the person's backups that it found, or tried to.
	BackedUp(BackedUp),
}

/// An update of a backup that the holder of the sync role found.
#[derive(Debug)]
pub struct BackedUp {
	/// The backup.
	pub backup: Backup,
	/// Where it found it.
	pub folder: PathBuf,
	/// What the update did, or why it failed.
	pub updated: Result<BackupUpdate, NodeError>,
}

/// How often the holder looks for the person's backups: every 30 seconds.
const BACKUPS_LOOK: Duration = Duration::from_secs(30);

/// How long after its last update here the holder updates a backup that stays found, once
/// new changes came: an hour.
const BACKUP_AGAIN: u64 = 3_600_000;

/// How long after its last update here the holder updates a backup that stays found, if no
/// new change came, so that its checks go on: a day.
const BACKUP_IDLE: u64 = 86_400_000;

/// Where the holder of the sync role found one of the person's backups on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundBackup {
	/// The folder where it last found it.
	pub folder: PathBuf,
	/// When it last updated it here, or tried to.
	pub updated: Option<u64>,
	/// How many changes the replica kept then.
	pub kept: Option<u64>,
}

/// What a look for backups found, and the updates it ran.
struct BackupLook {
	/// The backups found.
	present: BTreeSet<EntityId>,
	/// The updates run.
	updates: Vec<BackedUp>,
}

/// A sync that ended, on the thread that ran it, with how many changes the replica kept
/// as it began.
enum Finished {
	/// What was answered, or `None` if answering panicked.
	Answered(Option<Box<Answered>>, usize),
	/// The syncs with the replicas it connected to, or why none could start, and the asks
	/// they answer.
	Synced(Result<Vec<SyncedWith>, NodeError>, usize, Vec<i64>),
	/// A run of the checks of the replica's copies, or why it could not run.
	Checked(Result<CheckRun, NodeError>),
	/// A pass of removals, or why it could not run, with how many changes the replica kept
	/// as it began.
	Removed(Result<RemovalPass, NodeError>, usize),
	/// A look for the person's backups, with the updates it ran, or why it could not run.
	BackedUp(Result<BackupLook, NodeError>),
}

/// Holds the sync role of a closed library's replica, and syncs it, as the technical design's section on the sync role
/// says, until `holding.stop` is set: runs its endpoint, on which it answers every replica
/// that connects, each sync on a thread of its own, with a library of its own; and connects
/// to the others, one at a time, as it takes the role, when one appears on the network,
/// a while after new changes came, made here or received, and when another program asks.
/// Each change sets off at most one sync with each other replica: one it synced with since
/// the change came is not synced with again. It runs no endpoint while another program
/// borrows the network. Beside the syncs, on a thread of its own, it checks a share of the
/// replica's copies whenever one is due, as `holding.check` paces it, and after a check that
/// found damage syncs with every replica it finds ([`Library::check_copies`]). `rng` makes
/// randomness for each thread. `told` hears what it does.
///
/// # Errors
///
/// Returns an error if the library cannot be opened, the records cannot be written, or the
/// network cannot start; a sync that fails is in what `told` hears.
pub fn hold<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	role: Role,
	holding: &Holding<'_>,
	rng: &(dyn Fn() -> R + Sync),
	told: &mut dyn FnMut(Held),
) -> Result<(), NodeError> {
	// Closed at once, so that what it records commits now: the library kept open only reads,
	// and never holds a burst of changes, which would hold up the other programs' changes.
	retry_in_use(OPEN_WAIT, || closed.open())?.record_own_endpoint((holding.now)(), &mut rng())?;
	let watch = retry_in_use(OPEN_WAIT, || closed.open())?;
	let records = watch.sync_records(rng())?;
	let since = (holding.now)();
	let mut state = Holding::state(watch, records, holding, since)?;
	state.records.clear_holder()?;
	state.records.set_holder(holding.program, None, since)?;
	// Set as the holder ends, even by an error, so that a check under way ends too.
	let ending_flag = AtomicBool::new(false);
	let ending = &ending_flag;
	let held = std::thread::scope(|scope| {
		let held = (|| -> Result<(), NodeError> {
			let (finished, done) = mpsc::channel::<Finished>();
			loop {
				let stopping = holding.stop.load(Ordering::SeqCst);
				if stopping && state.running == 0 && !state.checking && !state.backing_up {
					return Ok(());
				}
				// A look for the person's backups, which updates those found.
				if let Some(present) = state.backups_due(stopping) {
					let finished = finished.clone();
					scope.spawn(move || {
						let look = run_backups(closed, holding, ending, &present, rng);
						let _ = finished.send(Finished::BackedUp(look));
					});
				}
				// A share of the replica's copies to check, when one is due.
				if !stopping && !state.checking && Instant::now() >= state.check_at {
					state.checking = true;
					let finished = finished.clone();
					scope.spawn(move || {
						let checked = run_checks(closed, holding, ending, rng);
						let _ = finished.send(Finished::Checked(checked));
					});
				}
				let lend = state.tend_network(closed, holding, stopping, told)?;
				state.look(holding)?;
				// A sync to start, one at a time, while the network is not lent.
				if let Some(network) = state
					.network
					.clone()
					.filter(|_| !lend && !stopping && !state.connecting)
					&& let Some((due, asked)) = state.due()
				{
					state.connecting = true;
					state.running += 1;
					let (finished, kept) = (finished.clone(), state.holder.kept);
					let peers = state.holder.peers.clone();
					scope.spawn(move || {
						let found = due.iter().copied().collect();
						let synced = run_syncs(closed, &network, (peers, &found), holding.now, rng);
						let _ = finished.send(Finished::Synced(synced, kept, asked));
					});
				}
				// A pass of removals, when one is due and no sync to start is, as a sync of
				// its own: the replicas asked to prove their copies are connected to.
				if let Some(network) = state
					.network
					.clone()
					.filter(|_| !lend && !stopping && !state.connecting)
					&& Instant::now() >= state.remove_at
				{
					state.connecting = true;
					state.running += 1;
					let found: BTreeSet<[u8; 32]> = state.holder.present.keys().copied().collect();
					let (finished, kept) = (finished.clone(), state.holder.kept);
					scope.spawn(move || {
						let pass = run_removals(closed, &network, &found, holding.now, rng);
						let _ = finished.send(Finished::Removed(pass, kept));
					});
				}
				// A sync that another replica starts.
				match state.network.clone().filter(|_| !lend && !stopping) {
					Some(network) => {
						if let Some(connection) = network.accept(Some(STEP))? {
							state.running += 1;
							let finished = finished.clone();
							let kept = state.holder.kept;
							scope.spawn(move || {
								let answered = catch_unwind(AssertUnwindSafe(|| {
									answer_connection(
										Some(closed),
										connection,
										holding.now,
										&mut rng(),
									)
								}))
								.ok()
								.map(Box::new);
								let _ = finished.send(Finished::Answered(answered, kept));
							});
						}
					}
					None => std::thread::sleep(STEP),
				}
				while let Ok(ended) = done.try_recv() {
					state.ended(ended, holding, told)?;
				}
			}
		})();
		ending.store(true, Ordering::SeqCst);
		held
	});
	let cleared = state.records.clear_holder();
	drop(role);
	held.and(cleared)
}

/// Syncs with each of `peers` that was `found` on the network, one after the other, on a
/// library of its own ([`sync_with_found`]).
fn run_syncs<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	network: &Network,
	(peers, found): (Vec<crate::Replica>, &BTreeSet<[u8; 32]>),
	now: &(dyn Fn() -> u64 + Sync),
	rng: &(dyn Fn() -> R + Sync),
) -> Result<Vec<SyncedWith>, NodeError> {
	catch_unwind(AssertUnwindSafe(|| {
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		Ok(sync_with_found(
			&mut library,
			network,
			peers,
			found,
			now,
			&mut rng(),
		))
	}))
	.unwrap_or(Err(NodeError::Panicked))
}

/// Runs a share of the checks of the replica's copies, on a library of its own, until the
/// holder stops or `ending` is set ([`Library::check_copies`]).
fn run_checks<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	holding: &Holding<'_>,
	ending: &AtomicBool,
	rng: &(dyn Fn() -> R + Sync),
) -> Result<CheckRun, NodeError> {
	catch_unwind(AssertUnwindSafe(|| {
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		let records = library.sync_records(rng())?;
		let stop = || holding.stop.load(Ordering::SeqCst) || ending.load(Ordering::SeqCst);
		library.check_copies(&records, holding.now, holding.check, &stop)
	}))
	.unwrap_or(Err(NodeError::Panicked))
}

/// Looks for the person's backups on this computer, on a library of its own, and updates
/// each found that the last look, whose finds are `present`, did not find, or that was last
/// updated here an hour ago or more, until the holder stops or `ending` is set
/// ([`Library::update_backup`]).
fn run_backups<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	holding: &Holding<'_>,
	ending: &AtomicBool,
	present: &BTreeSet<EntityId>,
	rng: &(dyn Fn() -> R + Sync),
) -> Result<BackupLook, NodeError> {
	catch_unwind(AssertUnwindSafe(|| {
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		let records = library.sync_records(rng())?;
		let known = records.backup_folders()?;
		let remembered = known
			.iter()
			.map(|(backup, found)| (*backup, found.folder.clone()))
			.collect();
		let stop = || holding.stop.load(Ordering::SeqCst) || ending.load(Ordering::SeqCst);
		let mut look = BackupLook {
			present: BTreeSet::new(),
			updates: Vec::new(),
		};
		for (backup, folder) in library.find_backups(&remembered)? {
			look.present.insert(backup.id);
			let now = (holding.now)();
			let kept = library.changes_kept()?;
			let last = known.get(&backup.id);
			// Found anew, as when plugged in; an hour after its last update here once new
			// changes came, or a day after if none did. One that failed waits as long.
			let due = !present.contains(&backup.id)
				|| last.is_none_or(|last| {
					last.updated.is_none_or(|at| {
						let since = now.saturating_sub(at);
						since >= BACKUP_IDLE
							|| (since >= BACKUP_AGAIN && last.kept != u64::try_from(kept).ok())
					})
				});
			if !due || stop() {
				records.record_backup(backup.id, &folder, None)?;
				continue;
			}
			let updated = library.update_backup(&backup, &folder, holding.now, &stop);
			if !matches!(updated, Ok(None)) {
				records.record_backup(backup.id, &folder, Some(((holding.now)(), kept)))?;
			}
			match updated {
				// Another program updates it: the next look tries again.
				Ok(None) => {
					look.present.remove(&backup.id);
				}
				Ok(Some(update)) => look.updates.push(BackedUp {
					backup,
					folder,
					updated: Ok(update),
				}),
				Err(error) => look.updates.push(BackedUp {
					backup,
					folder,
					updated: Err(error),
				}),
			}
		}
		Ok(look)
	}))
	.unwrap_or(Err(NodeError::Panicked))
}

/// Runs a pass of removals of the copies that the replica no longer keeps, on a library of
/// its own, with the replicas `found` on the network ([`remove_unkept`]).
fn run_removals<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	network: &Network,
	found: &BTreeSet<[u8; 32]>,
	now: &(dyn Fn() -> u64 + Sync),
	rng: &(dyn Fn() -> R + Sync),
) -> Result<RemovalPass, NodeError> {
	catch_unwind(AssertUnwindSafe(|| {
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		let records = library.sync_records(rng())?;
		remove_unkept(&mut library, network, &records, found, now, &mut rng())
	}))
	.unwrap_or(Err(NodeError::Panicked))
}

/// The holder of the sync role as it runs.
struct State {
	/// A library of the replica's own, which tells when changes come into it.
	watch: Library,
	records: Records,
	holder: Holder,
	/// Its endpoint, while it runs one.
	network: Option<Arc<Network>>,
	/// Since when it holds the role.
	since: u64,
	/// How many syncs run, on threads of their own.
	running: usize,
	/// Whether it connects to replicas now: one sync at a time does.
	connecting: bool,
	/// The records' count of what other connections committed, as last read.
	version: i64,
	/// The asks to answer, and those that a sync under way answers.
	asks: Vec<(i64, Option<EntityId>)>,
	answering: BTreeSet<i64>,
	/// Whether a check of the replica's copies runs, on a thread of its own.
	checking: bool,
	/// When to look next whether checks are due.
	check_at: Instant,
	/// When to run the next pass of removals.
	remove_at: Instant,
	/// Whether a look for the person's backups runs, on a thread of its own.
	backing_up: bool,
	/// When to look for backups next.
	backups_at: Instant,
	/// The backups that the last look found.
	backups_present: BTreeSet<EntityId>,
}

impl Holding<'_> {
	fn state(
		watch: Library,
		records: Records,
		holding: &Self,
		since: u64,
	) -> Result<State, NodeError> {
		Ok(State {
			holder: Holder::new(&watch, holding)?,
			version: records.version()?,
			asks: records.open_asks()?,
			watch,
			records,
			network: None,
			since,
			running: 0,
			connecting: false,
			answering: BTreeSet::new(),
			checking: false,
			check_at: Instant::now() + holding.check.wait,
			remove_at: Instant::now() + holding.check.wait,
			backing_up: false,
			backups_at: Instant::now(),
			backups_present: BTreeSet::new(),
		})
	}
}

impl State {
	/// Whether a look for the person's backups is due, not while the holder stops: if it
	/// is, the backups that the last look found, as it begins.
	fn backups_due(&mut self, stopping: bool) -> Option<BTreeSet<EntityId>> {
		if stopping || self.backing_up || Instant::now() < self.backups_at {
			return None;
		}
		self.backing_up = true;
		Some(self.backups_present.clone())
	}

	/// Stops the endpoint while another program borrows the network, or the role is let
	/// go, once the syncs under way end, and starts it again after. Returns whether the
	/// network is lent.
	fn tend_network(
		&mut self,
		closed: &Closed,
		holding: &Holding<'_>,
		stopping: bool,
		told: &mut dyn FnMut(Held),
	) -> Result<bool, NodeError> {
		let lend = lent(closed)?;
		if (lend || stopping) && self.network.is_some() && self.running == 0 {
			self.network = None;
			self.records.set_holder(holding.program, None, self.since)?;
			if lend {
				told(Held::Lent);
			}
		}
		if !lend && !stopping && self.network.is_none() {
			let started = Arc::new(self.watch.network(holding.network)?);
			for (peer, addresses) in &holding.known {
				started.remember(*peer, addresses, None);
			}
			self.records
				.set_holder(holding.program, started.port(), self.since)?;
			// A program that borrowed the network meanwhile may have read the record of
			// none, from before: the endpoint goes again before it takes a connection.
			if lent(closed)? {
				drop(started);
				self.records.set_holder(holding.program, None, self.since)?;
				return Ok(true);
			}
			told(Held::Started {
				port: started.port(),
				refused: started
					.port_refused()
					.map(|(port, why)| (port, why.to_owned())),
			});
			self.network = Some(started);
			self.holder.taken(Instant::now());
		}
		Ok(lend)
	}

	/// Looks at what other programs ask, what came into the replica, and who is on the
	/// network.
	fn look(&mut self, holding: &Holding<'_>) -> Result<(), NodeError> {
		let moved = self.records.version()?;
		if moved != self.version {
			self.version = moved;
			self.asks = self.records.open_asks()?;
			let answering = &self.answering;
			self.asks.retain(|(ask, _)| !answering.contains(ask));
		}
		if self.watch.catch_up()? {
			self.holder.peers = self.watch.peers()?;
			self.holder
				.counted(self.watch.changes_kept()?, Instant::now());
		}
		if let Some(network) = &self.network {
			let now = (holding.now)();
			for (replica, present) in self.holder.look(network, Instant::now()) {
				self.records.record_presence(replica, present, now)?;
			}
		}
		Ok(())
	}

	/// The replicas to connect to now, and the asks that connecting answers, if there are
	/// any: those asked for, or else those that [`Holder::due`] says.
	fn due(&mut self) -> Option<(Vec<[u8; 32]>, Vec<i64>)> {
		let asked: Option<BTreeSet<EntityId>> = if self.asks.is_empty() {
			None
		} else if self.asks.iter().any(|(_, replica)| replica.is_none()) {
			Some(self.holder.peers.iter().map(|peer| peer.id).collect())
		} else {
			Some(
				self.asks
					.iter()
					.filter_map(|(_, replica)| *replica)
					.collect(),
			)
		};
		let due = self.holder.due(asked.as_ref(), Instant::now());
		if due.is_empty() && self.asks.is_empty() {
			return None;
		}
		let asked: Vec<i64> = self.asks.drain(..).map(|(ask, _)| ask).collect();
		self.answering.extend(asked.iter().copied());
		Some((due, asked))
	}

	/// Notes a sync that ended: records it, tells the program, and answers the asks it
	/// answers.
	fn ended(
		&mut self,
		ended: Finished,
		holding: &Holding<'_>,
		told: &mut dyn FnMut(Held),
	) -> Result<(), NodeError> {
		if let Finished::BackedUp(look) = ended {
			self.backing_up = false;
			self.backups_at = Instant::now() + BACKUPS_LOOK;
			if let Ok(look) = look {
				self.backups_present = look.present;
				for update in look.updates {
					told(Held::BackedUp(update));
				}
			}
			return Ok(());
		}
		if let Finished::Checked(checked) = ended {
			self.checking = false;
			// After a run that ended at its time, the next waits out the rest it owes.
			let wait = match &checked {
				Ok(run) if run.more => run.rest,
				_ => holding.check.wait,
			};
			self.check_at = Instant::now() + wait;
			if let Ok(run) = checked {
				told(Held::Checked(run));
			}
			return Ok(());
		}
		self.running -= 1;
		let now = (holding.now)();
		if let Finished::Removed(pass, kept) = ended {
			self.connecting = false;
			// A pass that had room for no more is followed by another soon.
			let soon = matches!(&pass, Ok(pass) if pass.removed.more && pass.removed.copies > 0);
			self.remove_at = Instant::now()
				+ if soon {
					REMOVE_SOON
				} else {
					holding.check.wait
				};
			if let Ok(pass) = &pass {
				for with in pass.synced.iter().filter(|with| tried(with)) {
					if let Some(endpoint) = with.replica.endpoint {
						let counted = counts(&with.synced);
						self.holder.synced(endpoint, &counted, kept);
						self.records.record_sync(with.replica.id, now, counted)?;
					}
				}
			}
			told(Held::Removed(pass));
			return Ok(());
		}
		// What the sync brought counts as having come before its end.
		if self.watch.catch_up()? {
			self.holder.peers = self.watch.peers()?;
		}
		self.holder
			.counted(self.watch.changes_kept()?, Instant::now());
		match ended {
			Finished::Answered(None, _)
			| Finished::Checked(_)
			| Finished::Removed(..)
			| Finished::BackedUp(_) => {}
			Finished::Answered(Some(answered), kept) => {
				if let Some(replica) = &answered.replica {
					let synced = counts(&answered.synced);
					self.holder.synced(answered.endpoint, &synced, kept);
					self.records.record_sync(replica.id, now, synced)?;
				}
				told(Held::Answered(*answered));
			}
			Finished::Synced(Err(_), _, asked) => {
				// None could start, as the library could not be opened: the asks wait for
				// the next sync, as they are read again.
				self.connecting = false;
				for ask in asked {
					self.answering.remove(&ask);
				}
				self.version = self.version.wrapping_sub(1);
			}
			Finished::Synced(Ok(synced), kept, asked) => {
				self.connecting = false;
				for with in synced {
					if !tried(&with) {
						continue;
					}
					if let Some(endpoint) = with.replica.endpoint {
						let counted = counts(&with.synced);
						self.holder.synced(endpoint, &counted, kept);
						self.records.record_sync(with.replica.id, now, counted)?;
					}
					told(Held::Synced(with));
				}
				for ask in asked {
					self.answering.remove(&ask);
					self.records.answer(ask, now)?;
				}
			}
		}
		Ok(())
	}
}

/// Whether a sync with another replica was tried: those not found are not told of; nor those
/// that refused, as a program there only connects, as one that syncs once connects to this
/// one itself: no sync failed.
#[must_use]
pub fn tried(with: &SyncedWith) -> bool {
	!matches!(
		with.synced,
		Err(NodeError::NotFound | NodeError::Network(NetError::Refused))
	)
}

/// What a sync brought and sent, or why it failed.
fn counts(synced: &Result<nibkeep_sync::Synced, NodeError>) -> Result<(usize, usize), String> {
	match synced {
		Ok(synced) => Ok(synced
			.spaces
			.iter()
			.fold((0, 0), |(received, sent), space| {
				(received + space.received.changes, sent + space.sent.changes)
			})),
		Err(error) => Err(error.to_string()),
	}
}

/// What the holder keeps track of, to tell when to connect to whom.
struct Holder {
	/// The user's other replicas.
	peers: Vec<crate::Replica>,
	/// How many changes the replica keeps, as last counted.
	kept: usize,
	/// When the count last grew, if it did since the replicas behind were synced with.
	grew: Option<Instant>,
	/// How many changes the replica kept after its last sync with each other replica, by
	/// endpoint: a replica behind the count now has not seen every change.
	synced: BTreeMap<[u8; 32], usize>,
	/// The replicas on the network, by endpoint, and when each appeared, until the holder
	/// synced with it, or it connected.
	present: BTreeMap<[u8; 32], Option<Instant>>,
	/// When the holder took the role, until it synced with all it found then.
	taken: Option<Instant>,
	quiet: Duration,
	appear_wait: Duration,
	/// A source of jitter for the wait after a replica appears.
	jitter: u64,
}

impl Holder {
	fn new(library: &Library, holding: &Holding<'_>) -> Result<Self, NodeError> {
		Ok(Self {
			peers: library.peers()?,
			kept: library.changes_kept()?,
			grew: None,
			synced: BTreeMap::new(),
			present: BTreeMap::new(),
			taken: None,
			quiet: holding.quiet,
			appear_wait: holding.appear_wait,
			jitter: u64::from_le_bytes(library.endpoint()[..8].try_into().unwrap_or_default()),
		})
	}

	/// Notes that the holder took the role, and started its endpoint: the replicas it finds
	/// in the next while, as discovery finds them, it connects to at once.
	fn taken(&mut self, now: Instant) {
		self.taken = Some(now);
	}

	/// Notes how many changes the replica keeps now. Fewer than before, as when changes
	/// that waited are rejected, no other replica has seen more.
	fn counted(&mut self, kept: usize, now: Instant) {
		if kept > self.kept {
			self.grew = Some(now);
		}
		if kept < self.kept {
			for seen in self.synced.values_mut() {
				*seen = (*seen).min(kept);
			}
		}
		self.kept = kept;
	}

	/// Notes a sync with the replica at `endpoint`, which began when the replica kept `kept`
	/// changes: once it is done, the other has seen those, and what it sent. A change made
	/// here meanwhile is not among them; nor one that another sync of the same batch
	/// brought, which costs the other replica one more sync, that finds nothing new. The
	/// replica is on the network, and is not connected to as one that appears.
	fn synced(&mut self, endpoint: [u8; 32], synced: &Result<(usize, usize), String>, kept: usize) {
		if let Ok((received, _)) = synced {
			let seen = self.synced.entry(endpoint).or_default();
			*seen = (*seen).max(kept + received);
		}
		self.present.insert(endpoint, None);
	}

	/// Looks who is on the network: returns the replicas that appeared, and those that
	/// went, since it last looked.
	fn look(&mut self, network: &Network, now: Instant) -> Vec<(EntityId, bool)> {
		let endpoints: BTreeSet<[u8; 32]> =
			self.peers.iter().filter_map(|peer| peer.endpoint).collect();
		let found = network.find(&endpoints, Duration::ZERO);
		let mut changed = Vec::new();
		for peer in &self.peers {
			let Some(endpoint) = peer.endpoint else {
				continue;
			};
			let here = found.contains(&endpoint);
			match (here, self.present.contains_key(&endpoint)) {
				(true, false) => {
					self.jitter = self.jitter.rotate_left(13) ^ 0x9e37_79b9_7f4a_7c15;
					// At once in the while after the role is taken.
					let just_taken = self
						.taken
						.is_some_and(|taken| now < taken + self.appear_wait);
					let wait = if just_taken {
						Duration::ZERO
					} else {
						self.appear_wait.mul_f64(
							f64::from(u32::try_from(self.jitter % 1_000).unwrap_or(0)) / 1_000.0,
						)
					};
					self.present.insert(endpoint, Some(now + wait));
					changed.push((peer.id, true));
				}
				(false, true) => {
					self.present.remove(&endpoint);
					changed.push((peer.id, false));
				}
				_ => {}
			}
		}
		changed
	}

	/// The endpoints to sync with now, among those on the network: those asked for; one
	/// that appeared, once its wait is over, which is none in the while after the role is
	/// taken; and once the changes have been quiet a while, those that have not seen every
	/// change.
	fn due(&mut self, asked: Option<&BTreeSet<EntityId>>, now: Instant) -> Vec<[u8; 32]> {
		let mut due = BTreeSet::new();
		let quiet = self.grew.is_some_and(|grew| now >= grew + self.quiet);
		for peer in &self.peers {
			let Some(endpoint) = peer.endpoint else {
				continue;
			};
			let Some(appeared) = self.present.get(&endpoint).copied() else {
				continue;
			};
			let behind = self
				.synced
				.get(&endpoint)
				.is_none_or(|kept| *kept < self.kept);
			if asked.is_some_and(|asked| asked.contains(&peer.id))
				|| appeared.is_some_and(|at| now >= at)
				|| (quiet && behind)
			{
				due.insert(endpoint);
			}
		}
		if quiet {
			self.grew = None;
		}
		for endpoint in &due {
			if let Some(appeared) = self.present.get_mut(endpoint) {
				*appeared = None;
			}
		}
		due.into_iter().collect()
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_testkit::{TestRng, millis_at};

	use super::*;
	use crate::{NewLibrary, NewProtection};

	/// A directory that goes when the test ends, also when it fails.
	struct Removed(PathBuf);

	impl Drop for Removed {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.0);
		}
	}

	/// A library in a directory of its own, protected by a quick passphrase.
	fn library(name: &str) -> (Removed, Library) {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-node-role-{name}-{}", std::process::id()),
		);
		let library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&NewProtection::Passphrase(
				"correct horse",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			millis_at("2026-11-01", 10, 0),
			&mut TestRng::new(name),
		)
		.expect("creates");
		(Removed(directory), library)
	}

	#[test]
	fn a_holder_that_ended_without_clearing_its_records_holds_nothing() {
		let (_removed, library) = library("left-behind");
		let records = library
			.sync_records(TestRng::new("records"))
			.expect("records");
		// As an app that quit, or crashed, leaves it.
		records
			.set_holder("Passwords", Some(53300), millis_at("2026-11-01", 10, 1))
			.expect("records");
		let phone = EntityId::from_bytes([7; 16]);
		records
			.record_presence(phone, true, millis_at("2026-11-01", 10, 2))
			.expect("records");
		assert_eq!(records.holder().expect("reads"), None);
		let left = &records.replicas().expect("reads")[&phone];
		assert!(!left.present, "nobody knows who is on the network");
		assert_eq!(left.seen, Some(millis_at("2026-11-01", 10, 2)));
		let _role = library.take_role().expect("takes").expect("free");
		assert_eq!(
			records
				.holder()
				.expect("reads")
				.map(|holder| holder.program),
			Some("Passwords".to_owned()),
			"held, the record says who holds it"
		);
		assert!(records.replicas().expect("reads")[&phone].present);
	}

	#[test]
	fn a_blob_held_for_another_is_not_removed_and_one_being_removed_is_not_proved() {
		let (_removed, library) = library("holds");
		let records = library
			.sync_records(TestRng::new("records"))
			.expect("records");
		// And two programs at once, each with its own connection.
		let other = library
			.sync_records(TestRng::new("other records"))
			.expect("records");
		let (held, removing, free) = (Hash::of(b"held"), Hash::of(b"removing"), Hash::of(b"free"));
		let at = millis_at("2026-11-01", 10, 0);
		assert_eq!(records.hold(&[held], at).expect("holds"), [held].into());
		assert_eq!(
			other.begin_removing(&[removing], at).expect("marks"),
			[removing].into()
		);
		// Neither is had the other way while the other stands.
		assert_eq!(
			other.begin_removing(&[held, free], at + 1).expect("marks"),
			[free].into()
		);
		assert!(records.hold(&[removing], at + 1).expect("holds").is_empty());
		assert_eq!(records.holds(at + 1).expect("reads"), [held].into());
		// A removal that ends lets its blob be proved again.
		other.end_removing(&[removing, free]).expect("ends");
		assert_eq!(
			records.hold(&[removing], at + 2).expect("holds"),
			[removing].into()
		);
		// A hold ends after 10 minutes.
		let later = at + 2 + HOLD_FOR;
		assert!(records.holds(later).expect("reads").is_empty());
		assert_eq!(
			other
				.begin_removing(&[held, removing], later)
				.expect("marks"),
			[held, removing].into()
		);
	}

	#[test]
	fn borrowing_the_network_stops_waiting_for_the_holder_once_told() {
		let (_removed, library) = library("borrow-stops");
		let _role = library.take_role().expect("takes").expect("free");
		let records = library
			.sync_records(TestRng::new("records"))
			.expect("records");
		// A holder that keeps its endpoint, as one busy syncing.
		records
			.set_holder(
				"nib sync --serve",
				Some(53300),
				millis_at("2026-11-01", 10, 1),
			)
			.expect("records");
		let started = Instant::now();
		let borrowed = borrow_network(&library, &AtomicBool::new(true), TestRng::new("borrows"));
		assert!(matches!(borrowed, Err(NodeError::PairingExpired)));
		assert!(started.elapsed() < LEND_WAIT / 2, "stopped at once");
	}
}
