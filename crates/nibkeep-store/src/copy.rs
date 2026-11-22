//! Reading what a copy of a replica holds, which another computer may have written, as
//! untrusted as what another replica sends: its log file, copied with its write-ahead log
//! into a private folder and read there with SQLite's defensive settings, and its sealed
//! blobs (technical design: copies of a replica). Nothing of the copy is written to.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nibkeep_base::{Hash, StatementId};
use nibkeep_log::{Parcel, WrappedBlobKey, WrappedKey};
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags};

use crate::StoreError;
use crate::blobs::{blob_directory, place_in, replica_of_log};
use crate::file::{APPLICATION_ID, change_id, database, hash, payload_from, statement_id};

/// The tables that a log file has, which a copy's must have, as tables.
const TABLES: &[&str] = &["changes", "change_keys", "wrapped_keys", "blob_keys"];

/// How many times the copy's files are copied again while they keep changing.
const TRIES: usize = 5;

/// What a copy's log holds, item by item.
#[derive(Debug)]
pub enum CopiedItem {
	/// A change, with its sealed payload and wrapped keys if the copy holds them, boxed, as
	/// it is much larger than the other items.
	Change(Box<Parcel>),
	/// A wrapped key of a secret statement.
	RecordKey(StatementId, WrappedKey),
	/// A wrapped key of a blob, by the blob's sealed hash.
	BlobKey(Hash, WrappedBlobKey),
	/// A sealed blob, by its sealed hash, in the copy's file, which is read and never
	/// written.
	Blob(Hash, PathBuf),
}

/// Reads what a copy of a replica holds in one space, whose log file is `log`, item by item:
/// the changes in the order the copy got them, which builds before what builds on it, then
/// the keys of statements and blobs, then the sealed blobs. The log file and its write-ahead
/// log are copied together into `private`, a folder of the caller's own, again if they
/// changed meanwhile, and read there, never trusting their schema. Neither the change's tags,
/// which only the copy's keys check, nor anything else is trusted: `take` checks each item,
/// as it checks what another replica sends.
///
/// # Errors
///
/// Returns an error if the log file cannot be copied, is not a log file, or fails SQLite's
/// check of its integrity; or what `take` returns.
pub fn take_copied_log<E: From<StoreError>>(
	log: &Path,
	private: &Path,
	take: &mut dyn FnMut(CopiedItem) -> Result<(), E>,
) -> Result<(), E> {
	let copied = snapshot(log, private)?;
	let connection = open_untrusted(&copied)?;
	read_items(&connection, log, take)
}

/// Reads the items of a copy's log, opened as [`open_untrusted`] opens it.
fn read_items<E: From<StoreError>>(
	connection: &Connection,
	log: &Path,
	take: &mut dyn FnMut(CopiedItem) -> Result<(), E>,
) -> Result<(), E> {
	let mut changes = connection
		.prepare("SELECT id, change, payload FROM changes ORDER BY sequence")
		.map_err(database)?;
	let mut keys = connection
		.prepare("SELECT wrapped_key FROM change_keys WHERE change = ?1")
		.map_err(database)?;
	let mut rows = changes.query([]).map_err(database)?;
	while let Some(row) = rows.next().map_err(database)? {
		let id: Vec<u8> = row.get(0).map_err(database)?;
		let encoded: Vec<u8> = row.get(1).map_err(database)?;
		let payload: Option<Vec<u8>> = row.get(2).map_err(database)?;
		let Ok(change) = nibkeep_base::cbor::from_slice::<nibkeep_log::Change>(&encoded) else {
			continue;
		};
		if change_id(&id).ok() != Some(change.id()) {
			continue;
		}
		let wrapped: Vec<Vec<u8>> = keys
			.query_map([&id], |row| row.get(0))
			.map_err(database)?
			.collect::<Result<_, _>>()
			.map_err(database)?;
		take(CopiedItem::Change(Box::new(Parcel {
			change,
			payload: payload_from(payload).ok().flatten(),
			keys: wrapped
				.iter()
				.filter_map(|key| nibkeep_base::cbor::from_slice(key).ok())
				.collect(),
		})))?;
	}
	drop(rows);
	let mut statements = connection
		.prepare("SELECT statement, wrapped_key FROM wrapped_keys")
		.map_err(database)?;
	let mut rows = statements.query([]).map_err(database)?;
	while let Some(row) = rows.next().map_err(database)? {
		let statement: Vec<u8> = row.get(0).map_err(database)?;
		let encoded: Vec<u8> = row.get(1).map_err(database)?;
		if let (Ok(statement), Ok(key)) = (
			statement_id(&statement),
			nibkeep_base::cbor::from_slice(&encoded),
		) {
			take(CopiedItem::RecordKey(statement, key))?;
		}
	}
	drop(rows);
	let mut blob_keys = connection
		.prepare("SELECT sealed, wrapped_key FROM blob_keys")
		.map_err(database)?;
	let mut rows = blob_keys.query([]).map_err(database)?;
	while let Some(row) = rows.next().map_err(database)? {
		let sealed: Vec<u8> = row.get(0).map_err(database)?;
		let encoded: Vec<u8> = row.get(1).map_err(database)?;
		if let (Ok(sealed), Ok(key)) = (hash(&sealed), nibkeep_base::cbor::from_slice(&encoded)) {
			take(CopiedItem::BlobKey(sealed, key))?;
		}
	}
	drop(rows);
	// The copy's sealed blobs of the space: those it keeps keys for, in its replica's blob
	// directory.
	let directory = blob_directory(replica_of_log(log));
	let mut keyed = connection
		.prepare("SELECT DISTINCT sealed FROM blob_keys ORDER BY sealed")
		.map_err(database)?;
	let mut rows = keyed.query([]).map_err(database)?;
	while let Some(row) = rows.next().map_err(database)? {
		let sealed: Vec<u8> = row.get(0).map_err(database)?;
		let Ok(sealed) = hash(&sealed) else {
			continue;
		};
		let path = place_in(&directory, &sealed);
		if path.is_file() {
			take(CopiedItem::Blob(sealed, path))?;
		}
	}
	Ok(())
}

/// What tells that a file changed: its length and when it was last written.
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
	let metadata = fs::metadata(path).ok()?;
	Some((metadata.len(), metadata.modified().ok()?))
}

/// Copies a log file with its write-ahead log into `private`, again while either changes
/// meanwhile, a few times at most: a program that syncs the copy's folder may be writing
/// them. Returns the private copy of the log file.
fn snapshot(log: &Path, private: &Path) -> Result<PathBuf, StoreError> {
	let wal = with_suffix(log, "-wal");
	let copied = private.join("log.sqlite");
	let copied_wal = with_suffix(&copied, "-wal");
	fs::create_dir_all(private)?;
	for _ in 0..TRIES {
		let before = (stamp(log), stamp(&wal));
		for stale in [&copied, &copied_wal, &with_suffix(&copied, "-shm")] {
			let _ = fs::remove_file(stale);
		}
		fs::copy(log, &copied)?;
		if wal.exists() {
			fs::copy(&wal, &copied_wal)?;
		}
		if (stamp(log), stamp(&wal)) == before {
			break;
		}
	}
	Ok(copied)
}

/// A path with a suffix after its file name, as SQLite names a database's other files.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
	let mut name = path.as_os_str().to_owned();
	name.push(suffix);
	PathBuf::from(name)
}

/// Opens a database that another computer may have written, so that nothing in it runs:
/// SQLite's defensive mode, no triggers or views, a schema it does not trust, and a check
/// of its integrity and of its tables before anything is read.
fn open_untrusted(path: &Path) -> Result<Connection, StoreError> {
	let connection = Connection::open_with_flags(
		path,
		OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
	)
	.map_err(database)?;
	for (setting, on) in [
		(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
		(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
		(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
		(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
		(DbConfig::SQLITE_DBCONFIG_ENABLE_QPSG, true),
	] {
		connection.set_db_config(setting, on).map_err(database)?;
	}
	let application_id: i64 = connection
		.query_row("PRAGMA application_id", [], |row| row.get(0))
		.map_err(database)?;
	if application_id != APPLICATION_ID {
		return Err(StoreError::Damaged(
			"the copy's log file is not a log file".to_owned(),
		));
	}
	let check: String = connection
		.query_row("PRAGMA quick_check", [], |row| row.get(0))
		.map_err(database)?;
	if check != "ok" {
		return Err(StoreError::Damaged(format!(
			"the copy's log file is damaged: {check}"
		)));
	}
	for table in TABLES {
		let kind: Option<String> = connection
			.query_row(
				"SELECT type FROM sqlite_schema WHERE name = ?1",
				[table],
				|row| row.get(0),
			)
			.ok();
		if kind.as_deref() != Some("table") {
			return Err(StoreError::Damaged(format!(
				"the copy's log file has no table {table}"
			)));
		}
	}
	// A log file has tables and indexes only: a trigger or a view would run when the file
	// is used.
	let others: i64 = connection
		.query_row(
			"SELECT count(*) FROM sqlite_schema WHERE type NOT IN ('table', 'index')",
			[],
			|row| row.get(0),
		)
		.map_err(database)?;
	if others > 0 {
		return Err(StoreError::Damaged(
			"the copy's log file has triggers or views, which no log file has".to_owned(),
		));
	}
	Ok(connection)
}
