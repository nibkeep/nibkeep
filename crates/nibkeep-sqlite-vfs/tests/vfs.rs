//! SQLite through a VFS in safe Rust: a plain one, on ordinary files, that notes what it is
//! asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nibkeep_sqlite_vfs::{Error, File, FileKind, RawFile, RegisterError, SyncMode, Vfs, register};
use nibkeep_testkit::empty_directory;
use rusqlite::{Connection, OpenFlags};

/// What the VFS was asked to open, and how it should misbehave.
#[derive(Default)]
struct Notes {
	opened: Mutex<Vec<(PathBuf, FileKind)>>,
	/// The database reads as a file that ends at its start: its size is 0, and its reads
	/// come up short.
	short_reads: AtomicBool,
	/// Reads of the database find damage.
	damaged: AtomicBool,
	/// Reads of the database panic.
	panics: AtomicBool,
}

/// A VFS that passes SQLite's bytes to the raw file as they are.
struct PlainVfs(Arc<Notes>);

struct PlainFile {
	kind: FileKind,
	notes: Arc<Notes>,
}

impl Vfs for PlainVfs {
	type File = PlainFile;

	fn accepts(&self, path: &Path, kind: FileKind) -> Result<(), Error> {
		self.0
			.opened
			.lock()
			.expect("not poisoned")
			.push((path.to_owned(), kind));
		// Journals and temporary files are refused, before they are made.
		match kind {
			FileKind::Database | FileKind::WriteAheadLog => Ok(()),
			FileKind::Journal | FileKind::Other => Err(Error::CannotOpen),
		}
	}

	fn open(
		&self,
		_path: &Path,
		kind: FileKind,
		raw: Option<&mut RawFile>,
	) -> Result<PlainFile, Error> {
		raw.ok_or(Error::CannotOpen)?;
		Ok(PlainFile {
			kind,
			notes: Arc::clone(&self.0),
		})
	}
}

/// The raw file that every file of the plain VFS has.
fn raw(raw: Option<&mut RawFile>) -> Result<&mut RawFile, Error> {
	raw.ok_or(Error::Read)
}

impl File for PlainFile {
	fn read(
		&mut self,
		raw: Option<&mut RawFile>,
		offset: u64,
		buffer: &mut [u8],
	) -> Result<usize, Error> {
		if self.kind == FileKind::Database {
			assert!(
				!self.notes.panics.load(Ordering::Relaxed),
				"a VFS that panics"
			);
			if self.notes.damaged.load(Ordering::Relaxed) {
				return Err(Error::Damaged);
			}
			if self.notes.short_reads.load(Ordering::Relaxed) {
				buffer.fill(0xff);
				return Ok(0);
			}
		}
		let raw = self::raw(raw)?;
		if raw.read(offset, buffer)? {
			return Ok(buffer.len());
		}
		let size = raw.size()?;
		Ok(usize::try_from(size.saturating_sub(offset)).map_or(0, |left| left.min(buffer.len())))
	}

	fn write(&mut self, raw: Option<&mut RawFile>, offset: u64, data: &[u8]) -> Result<(), Error> {
		self::raw(raw)?.write(offset, data)
	}

	fn truncate(&mut self, raw: Option<&mut RawFile>, size: u64) -> Result<(), Error> {
		self::raw(raw)?.truncate(size)
	}

	fn sync(&mut self, raw: Option<&mut RawFile>, sync: SyncMode) -> Result<(), Error> {
		self::raw(raw)?.sync(sync)
	}

	fn size(&mut self, raw: Option<&mut RawFile>) -> Result<u64, Error> {
		if self.kind == FileKind::Database && self.notes.short_reads.load(Ordering::Relaxed) {
			return Ok(0);
		}
		self::raw(raw)?.size()
	}
}

/// Registers a plain VFS under a name of its own, and returns the name and its notes.
fn plain_vfs(name: &str) -> (String, Arc<Notes>) {
	let notes = Arc::new(Notes::default());
	let name = format!("plain-{name}");
	register(&name, PlainVfs(Arc::clone(&notes))).expect("registers");
	(name, notes)
}

/// Opens a database through the VFS as the store does: with temporary data in memory, a
/// write-ahead log, and a wait for the locks that another connection holds.
fn open(path: &Path, vfs: &str) -> rusqlite::Result<Connection> {
	let connection = Connection::open_with_flags_and_vfs(path, OpenFlags::default(), vfs)?;
	connection.busy_timeout(std::time::Duration::from_secs(5))?;
	connection.pragma_update(None, "temp_store", "MEMORY")?;
	// Only a new file switches to the write-ahead log: switching needs the database alone.
	let mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
	if mode != "wal" {
		connection.query_row("PRAGMA journal_mode = MEMORY", [], |_| Ok(()))?;
	}
	let mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
	assert_eq!(mode, "wal");
	Ok(connection)
}

fn directory(name: &str) -> PathBuf {
	empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn code(error: &rusqlite::Error) -> Option<i32> {
	match error {
		rusqlite::Error::SqliteFailure(error, _) => Some(error.extended_code),
		_ => None,
	}
}

#[test]
fn sqlite_reads_and_writes_through_the_vfs() {
	let directory = directory("reads and writes");
	let path = directory.join("data.sqlite");
	let (vfs, notes) = plain_vfs("reads and writes");
	{
		let connection = open(&path, &vfs).expect("opens");
		connection
			.execute_batch(
				"CREATE TABLE rows (n INTEGER PRIMARY KEY, value BLOB NOT NULL);
				 INSERT INTO rows (value) VALUES (randomblob(5000)), (randomblob(10));",
			)
			.expect("writes");
		assert!(path.with_file_name("data.sqlite-wal").exists());
	}
	let connection = open(&path, &vfs).expect("opens again");
	let count: i64 = connection
		.query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
		.expect("reads");
	assert_eq!(count, 2);
	let integrity: String = connection
		.query_row("PRAGMA integrity_check", [], |row| row.get(0))
		.expect("checks");
	assert_eq!(integrity, "ok");
	// Stock SQLite reads the same file: the VFS only carried SQLite's bytes.
	drop(connection);
	let count: i64 = Connection::open(&path)
		.and_then(|c| c.query_row("SELECT count(*) FROM rows", [], |row| row.get(0)))
		.expect("stock SQLite reads it");
	assert_eq!(count, 2);

	let opened = notes.opened.lock().expect("not poisoned");
	let kinds: Vec<FileKind> = opened.iter().map(|(_, kind)| *kind).collect();
	assert!(kinds.contains(&FileKind::Database));
	assert!(kinds.contains(&FileKind::WriteAheadLog));
	assert!(opened.iter().all(|(path, _)| path.is_absolute()));
}

#[test]
fn two_connections_use_one_database_at_once() {
	let directory = directory("two connections");
	let path = directory.join("data.sqlite");
	let (vfs, _) = plain_vfs("two connections");
	let writer = open(&path, &vfs).expect("opens");
	writer
		.execute_batch("CREATE TABLE rows (n INTEGER PRIMARY KEY, value BLOB NOT NULL)")
		.expect("makes the table");
	let reader = open(&path, &vfs).expect("opens again");
	let count = |connection: &Connection| -> i64 {
		connection
			.query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
			.expect("counts")
	};
	// The reader sees what was committed, and keeps its snapshot while the writer goes on.
	writer
		.execute("INSERT INTO rows (value) VALUES (randomblob(5000))", [])
		.expect("writes");
	assert_eq!(count(&reader), 1);
	reader.execute_batch("BEGIN").expect("begins a read");
	assert_eq!(count(&reader), 1);
	for _ in 0..20 {
		writer
			.execute("INSERT INTO rows (value) VALUES (randomblob(5000))", [])
			.expect("writes while the other reads");
	}
	assert_eq!(count(&reader), 1, "the reader's snapshot");
	reader.execute_batch("COMMIT").expect("ends the read");
	assert_eq!(count(&reader), 21);
	assert!(directory.join("data.sqlite-shm").exists(), "shared memory");
	// Writers take turns.
	writer
		.execute_batch("BEGIN IMMEDIATE")
		.expect("takes the write lock");
	reader
		.busy_timeout(std::time::Duration::from_millis(50))
		.expect("a short wait");
	let error = reader
		.execute("INSERT INTO rows (value) VALUES (x'00')", [])
		.expect_err("the other holds the write lock");
	assert_eq!(code(&error).map(|code| code & 0xff), Some(codes::BUSY));
	writer.execute_batch("COMMIT").expect("lets go");
	reader
		.execute("INSERT INTO rows (value) VALUES (x'00')", [])
		.expect("its turn");
}

#[test]
fn a_relative_path_reaches_the_vfs_absolute() {
	let directory = directory("relative path");
	let (vfs, notes) = plain_vfs("relative path");
	// From the working directory: `..` up to where the two paths part, then down.
	let working = std::env::current_dir().expect("a working directory");
	let shared = working
		.components()
		.zip(directory.components())
		.take_while(|(one, other)| one == other)
		.count();
	if shared == 0 {
		// On another drive, as on Windows, there is no relative path.
		return;
	}
	let mut relative = PathBuf::new();
	for _ in working.components().skip(shared) {
		relative.push("..");
	}
	relative.extend(directory.components().skip(shared));
	let relative = relative.join("data.sqlite");
	assert!(relative.is_relative());
	open(&relative, &vfs).expect("opens");
	let opened = notes.opened.lock().expect("not poisoned");
	assert!(opened.iter().all(|(path, _)| path.is_absolute()));
}

#[test]
fn files_that_the_vfs_refuses_are_not_written() {
	let directory = directory("refused");
	let path = directory.join("data.sqlite");
	let (vfs, _) = plain_vfs("refused");
	let connection = Connection::open_with_flags_and_vfs(&path, OpenFlags::default(), vfs.as_str())
		.expect("opens");
	// A rollback journal on disk is refused, so the first write fails.
	let error = connection
		.execute_batch("CREATE TABLE rows (n INTEGER PRIMARY KEY)")
		.expect_err("needs a journal");
	assert_eq!(code(&error), Some(codes::CANTOPEN));
	assert!(!directory.join("data.sqlite-journal").exists());
}

#[test]
fn a_short_read_reads_as_zeros_and_damage_as_an_error() {
	let directory = directory("short and damaged");
	let path = directory.join("data.sqlite");
	let (vfs, notes) = plain_vfs("short and damaged");
	let connection = open(&path, &vfs).expect("opens");
	connection
		.execute_batch(
			"CREATE TABLE rows (n INTEGER PRIMARY KEY, value BLOB NOT NULL);
			 INSERT INTO rows (value) VALUES (randomblob(100));
			 PRAGMA wal_checkpoint(TRUNCATE);",
		)
		.expect("writes");
	drop(connection);

	// A database that ends at its start reads as empty, whatever the VFS left in the
	// buffer.
	notes.short_reads.store(true, Ordering::Relaxed);
	let connection = open(&path, &vfs).expect("opens");
	let error = connection
		.query_row("SELECT count(*) FROM rows", [], |row| row.get::<_, i64>(0))
		.expect_err("no table in an empty database");
	assert!(error.to_string().contains("no such table"), "{error}");
	drop(connection);
	notes.short_reads.store(false, Ordering::Relaxed);

	notes.damaged.store(true, Ordering::Relaxed);
	let error = open(&path, &vfs)
		.and_then(|c| c.query_row("SELECT count(*) FROM rows", [], |row| row.get::<_, i64>(0)))
		.expect_err("damaged");
	assert_eq!(code(&error), Some(codes::IOERR_DATA));
}

#[test]
fn a_panic_in_the_vfs_is_an_error_to_sqlite() {
	let directory = directory("panics");
	let path = directory.join("data.sqlite");
	let (vfs, notes) = plain_vfs("panics");
	let connection = open(&path, &vfs).expect("opens");
	connection
		.execute_batch(
			"CREATE TABLE rows (n INTEGER PRIMARY KEY);
			 INSERT INTO rows DEFAULT VALUES;
			 PRAGMA wal_checkpoint(TRUNCATE);",
		)
		.expect("writes");
	drop(connection);
	notes.panics.store(true, Ordering::Relaxed);
	let error = open(&path, &vfs)
		.and_then(|c| c.query_row("SELECT count(*) FROM rows", [], |row| row.get::<_, i64>(0)))
		.expect_err("the VFS panicked");
	assert_eq!(code(&error), Some(codes::IOERR));
}

#[test]
fn a_name_is_registered_once() {
	plain_vfs("once");
	assert!(matches!(
		register("plain-once", PlainVfs(Arc::default())),
		Err(RegisterError::Name)
	));
	assert!(matches!(
		register("with\0zero", PlainVfs(Arc::default())),
		Err(RegisterError::Name)
	));
}

/// SQLite's codes that the tests expect.
mod codes {
	/// `SQLITE_IOERR`: a call into the VFS failed.
	pub(crate) const IOERR: i32 = 10;
	/// `SQLITE_BUSY`: another connection holds a lock.
	pub(crate) const BUSY: i32 = 5;
	/// `SQLITE_CANTOPEN`: a file could not be opened.
	pub(crate) const CANTOPEN: i32 = 14;
	/// `SQLITE_IOERR_DATA`: what was read does not check out.
	pub(crate) const IOERR_DATA: i32 = IOERR | (32 << 8);
}
