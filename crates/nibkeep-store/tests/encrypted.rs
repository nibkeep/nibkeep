//! Encrypted databases: SQLite through the store's VFS, which encrypts every page of the
//! file and every record of its write-ahead log ([technical design: the state file]).
//!
//! [technical design: the state file]: ../../../docs/Technical-design.md#75-the-state-file

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use nibkeep_crypto::Key;
use nibkeep_crypto::rand_core::Rng;
use nibkeep_store::{EncryptedDatabase, StoreError};
use nibkeep_testkit::TestRng;
use proptest::prelude::*;
use rusqlite::{Connection, params};

/// A value that must never reach the disk as it is.
const MARKER: &[u8] = b"a value to keep secret, 0123456789";

/// The size of the file's header, and of a page as stored: its nonce, the page and its tag.
const HEADER: usize = 96;
const RECORD: usize = 24 + 4096 + 16;

/// SQLite's extended codes that the tests expect.
const NOTADB: i32 = 26;
const IOERR_DATA: i32 = 0x0a | (32 << 8);

fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn key(seed: &str) -> Key {
	TestRng::new(seed).key()
}

fn open(path: &Path, key: &Key) -> Result<EncryptedDatabase, StoreError> {
	EncryptedDatabase::open(path, key, TestRng::new(&format!("{}", path.display())))
}

fn wal(path: &Path) -> PathBuf {
	let mut name = path.as_os_str().to_owned();
	name.push("-wal");
	PathBuf::from(name)
}

fn contains(path: &Path, needle: &[u8]) -> bool {
	fs::read(path).is_ok_and(|data| data.windows(needle.len()).any(|window| window == needle))
}

fn code(error: &StoreError) -> Option<i32> {
	match error {
		StoreError::Database(rusqlite::Error::SqliteFailure(error, _)) => Some(error.extended_code),
		_ => None,
	}
}

fn rusqlite_code(error: &rusqlite::Error) -> Option<i32> {
	match error {
		rusqlite::Error::SqliteFailure(error, _) => Some(error.extended_code),
		_ => None,
	}
}

/// A table of rows, each holding the marker, in a database that keeps everything in its
/// write-ahead log until asked.
fn fill(database: &EncryptedDatabase, rows: usize) {
	let connection = database.connection();
	connection
		.execute_batch(
			"CREATE TABLE IF NOT EXISTS rows (n INTEGER PRIMARY KEY, value BLOB NOT NULL);
			 PRAGMA wal_autocheckpoint = 0;",
		)
		.expect("makes the table");
	for _ in 0..rows {
		connection
			.execute(
				"INSERT INTO rows (value) VALUES (?1)",
				params![MARKER.repeat(10)],
			)
			.expect("inserts");
	}
}

fn count(database: &EncryptedDatabase) -> rusqlite::Result<i64> {
	database
		.connection()
		.query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
}

fn checkpoint(database: &EncryptedDatabase) {
	let busy: i64 = database
		.connection()
		.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
		.expect("checkpoints");
	assert_eq!(busy, 0);
}

#[test]
fn what_is_written_reads_back_after_reopening() {
	let directory = directory("reads back");
	let path = directory.join("state.sqlite");
	let key = key("state key");
	{
		let database = open(&path, &key).expect("creates");
		fill(&database, 100);
	}
	let database = open(&path, &key).expect("opens again");
	assert_eq!(count(&database).expect("counts"), 100);
	let integrity: String = database
		.connection()
		.query_row("PRAGMA integrity_check", [], |row| row.get(0))
		.expect("checks");
	assert_eq!(integrity, "ok");
}

#[test]
fn nothing_reaches_the_disk_in_the_clear() {
	let directory = directory("in the clear");
	let path = directory.join("state.sqlite");
	let database = open(&path, &key("state key")).expect("creates");
	fill(&database, 200);
	assert!(fs::metadata(wal(&path)).expect("a log").len() > 100_000);
	let shared_memory = directory.join("state.sqlite-shm");
	assert!(shared_memory.exists(), "SQLite's shared memory, while open");
	for file in [&path, &wal(&path), &shared_memory] {
		assert!(
			!contains(file, MARKER),
			"{} holds the marker",
			file.display()
		);
		assert!(
			!contains(file, b"SQLite format 3"),
			"{} holds SQLite's header",
			file.display()
		);
	}
	checkpoint(&database);
	assert!(!contains(&path, MARKER));
	drop(database);
	let mut names: Vec<_> = fs::read_dir(&directory)
		.expect("lists")
		.map(|entry| entry.expect("an entry").file_name())
		.collect();
	names.sort();
	assert_eq!(
		names,
		["state.sqlite"],
		"no journal, shared memory or log is left"
	);
}

#[test]
fn each_database_has_a_key_of_its_own() {
	let directory = directory("own keys");
	let key = key("one key for both");
	let (first, second) = (directory.join("first"), directory.join("second"));
	for path in [&first, &second] {
		let database = open(path, &key).expect("creates");
		fill(&database, 10);
		checkpoint(&database);
	}
	let original = fs::read(&second).expect("reads");
	let from_first = fs::read(&first).expect("reads");
	// A page of the first, in its place in the second, does not open there.
	let mut moved = original.clone();
	moved[HEADER..HEADER + RECORD].copy_from_slice(&from_first[HEADER..HEADER + RECORD]);
	fs::write(&second, &moved).expect("writes");
	assert!(
		open(&second, &key)
			.and_then(|database| count(&database).map_err(StoreError::Database))
			.is_err()
	);
	// Nor do the second's pages under the first's header.
	let mut header = original.clone();
	header[..HEADER].copy_from_slice(&from_first[..HEADER]);
	fs::write(&second, &header).expect("writes");
	assert!(
		open(&second, &key)
			.and_then(|database| count(&database).map_err(StoreError::Database))
			.is_err()
	);
	fs::write(&second, &original).expect("writes");
	assert_eq!(
		count(&open(&second, &key).expect("opens")).expect("counts"),
		10
	);
}

#[test]
fn another_key_and_stock_sqlite_cannot_read_it() {
	let directory = directory("other keys");
	let path = directory.join("state.sqlite");
	fill(&open(&path, &key("state key")).expect("creates"), 10);

	let error = open(&path, &key("another key")).expect_err("the wrong key");
	assert_eq!(code(&error), Some(NOTADB));
	let error = Connection::open(&path)
		.and_then(|connection| {
			connection.query_row("SELECT count(*) FROM rows", [], |row| row.get::<_, i64>(0))
		})
		.expect_err("stock SQLite");
	assert_eq!(rusqlite_code(&error), Some(NOTADB));
	// The file is untouched, and opens with its key.
	assert_eq!(
		count(&open(&path, &key("state key")).expect("opens")).expect("counts"),
		10
	);
}

#[test]
fn a_changed_or_moved_page_is_found() {
	let directory = directory("changed pages");
	let path = directory.join("state.sqlite");
	let key = key("state key");
	{
		let database = open(&path, &key).expect("creates");
		fill(&database, 100);
		checkpoint(&database);
	}
	let original = fs::read(&path).expect("reads");
	let pages = (original.len() - HEADER) / RECORD;
	assert!(pages > 4);
	let read_all = |data: &[u8]| {
		let copy = directory.join("copy.sqlite");
		fs::write(&copy, data).expect("writes");
		let result = open(&copy, &key).and_then(|database| {
			let total: i64 = database
				.connection()
				.query_row("SELECT sum(length(value)) FROM rows", [], |row| row.get(0))
				.map_err(StoreError::Database)?;
			Ok(total)
		});
		fs::remove_file(&copy).expect("removes");
		result
	};
	assert!(read_all(&original).is_ok());
	for page in [0, 1, pages / 2, pages - 1] {
		for at in [0, 24, 24 + 2000, RECORD - 1] {
			let mut changed = original.clone();
			changed[HEADER + page * RECORD + at] ^= 1;
			let error = read_all(&changed).expect_err("a changed page");
			assert_eq!(code(&error), Some(IOERR_DATA), "page {page}, byte {at}");
		}
	}
	let mut swapped = original.clone();
	let (first, second) = (HEADER + 2 * RECORD, HEADER + 3 * RECORD);
	let page = swapped[first..first + RECORD].to_vec();
	swapped.copy_within(second..second + RECORD, first);
	swapped[second..second + RECORD].copy_from_slice(&page);
	assert_eq!(
		code(&read_all(&swapped).expect_err("swapped pages")),
		Some(IOERR_DATA)
	);
	// Every byte of the header counts: the magic, the size of a page, the reserved bytes,
	// and its seal.
	for at in [0, 17, 21, 30, HEADER - 1] {
		let mut header = original.clone();
		header[at] ^= 1;
		assert_eq!(
			code(&read_all(&header).expect_err("a changed header")),
			Some(NOTADB),
			"byte {at} of the header"
		);
	}
}

/// The rows that a copy of the file and its write-ahead log holds, the log changed by
/// `change`.
fn rows_with_log(path: &Path, key: &Key, change: impl FnOnce(&mut Vec<u8>)) -> i64 {
	let directory = path.with_file_name("copies");
	let _ = fs::remove_dir_all(&directory);
	fs::create_dir_all(&directory).expect("makes the directory");
	let copy = directory.join("state.sqlite");
	fs::copy(path, &copy).expect("copies the file");
	let mut log = fs::read(wal(path)).expect("reads the log");
	change(&mut log);
	fs::write(wal(&copy), log).expect("writes the log");
	let database = open(&copy, key).expect("opens the copy");
	let integrity: String = database
		.connection()
		.query_row("PRAGMA integrity_check", [], |row| row.get(0))
		.expect("checks");
	assert_eq!(integrity, "ok");
	count(&database).expect("counts")
}

#[test]
fn a_log_cut_short_or_ending_in_garbage_ends_there() {
	let directory = directory("torn log");
	let path = directory.join("state.sqlite");
	let key = key("state key");
	let database = open(&path, &key).expect("creates");
	fill(&database, 50);
	// The last transaction writes the table's pages: cutting into it loses it alone.
	let frame = 24 + 4096 + 2 * 40;
	assert_eq!(rows_with_log(&path, &key, |_| {}), 50);
	assert_eq!(
		rows_with_log(&path, &key, |log| log.truncate(log.len() - 1000)),
		49
	);
	assert_eq!(
		rows_with_log(&path, &key, |log| log.truncate(log.len() - RECORD - 10)),
		49
	);
	assert_eq!(
		rows_with_log(&path, &key, |log| {
			let end = log.len();
			log[end - frame..].fill(0x5a);
		}),
		49
	);
	assert_eq!(
		rows_with_log(&path, &key, |log| log.extend_from_slice(&[0x5a; 5000])),
		50
	);
	drop(database);
}

#[test]
fn a_library_that_moves_still_opens() {
	let directory = directory("moves");
	let key = key("state key");
	let before = directory.join("before");
	fs::create_dir_all(&before).expect("makes the directory");
	fill(
		&open(&before.join("state.sqlite"), &key).expect("creates"),
		20,
	);
	let after = directory.join("after");
	fs::rename(&before, &after).expect("moves");
	assert_eq!(
		count(&open(&after.join("state.sqlite"), &key).expect("opens")).expect("counts"),
		20
	);
}

#[test]
fn a_database_is_open_under_one_key_and_another_beside_it_under_its_own() {
	let directory = directory("open once");
	let path = directory.join("state.sqlite");
	let database = open(&path, &key("state key")).expect("creates");
	// Opened again in this program, under the same key, it is the same database; under
	// another key, it is refused.
	let again = open(&path, &key("state key")).expect("opens again");
	assert!(matches!(
		open(&path, &key("another key")),
		Err(StoreError::InUse)
	));
	// The same file named another way is the same file.
	let roundabout = directory.join(".").join("state.sqlite");
	assert!(matches!(
		open(&roundabout, &key("another key")),
		Err(StoreError::InUse)
	));
	fill(&again, 2);
	drop(again);
	// A purge writes a new file beside the old one, under a new key.
	let new = open(&directory.join("state.sqlite.new"), &key("new key")).expect("creates");
	fill(&new, 5);
	fill(&database, 7);
	drop(database);
	assert_eq!(
		count(&open(&path, &key("state key")).expect("opens")).expect("counts"),
		9
	);
	assert_eq!(count(&new).expect("counts"), 5);
}

#[test]
fn files_other_than_the_database_and_its_log_are_refused() {
	let directory = directory("refused");
	let path = directory.join("state.sqlite");
	let database = open(&path, &key("state key")).expect("creates");
	fill(&database, 3);
	// Leaving the write-ahead log needs a journal on disk, at once or at the next write.
	let connection = database.connection();
	let refused = connection
		.pragma_update(None, "journal_mode", "DELETE")
		.and_then(|()| connection.execute("INSERT INTO rows (value) VALUES (x'00')", []));
	assert!(refused.is_err(), "a journal on disk is refused");
	assert!(!directory.join("state.sqlite-journal").exists());
	assert_eq!(count(&database).expect("still counts"), 3);
	// A database attached to it has no key, and is never made.
	let attached = directory.join("attached.sqlite");
	let attach = format!(
		"ATTACH '{}' AS attached; CREATE TABLE attached.t (n);",
		attached.display()
	);
	assert!(
		EncryptedDatabase::open(
			&directory.join("other.sqlite"),
			&key("k"),
			TestRng::new("r")
		)
		.and_then(|database| {
			database
				.connection()
				.execute_batch(&attach)
				.map_err(StoreError::Database)
		})
		.is_err(),
		"an attached database has no key"
	);
	assert!(!attached.exists(), "the attached database was not made");
}

/// What one step of the property test does.
#[derive(Clone, Debug)]
enum Step {
	/// Inserts or replaces the row `n` with `length` bytes.
	Put { n: u8, length: u16 },
	/// Deletes the row `n`.
	Delete { n: u8 },
	/// Moves the write-ahead log into the file.
	Checkpoint,
	/// Closes the database and opens it again.
	Reopen,
}

fn step() -> impl Strategy<Value = Step> {
	prop_oneof![
		6 => (any::<u8>(), 0..9000u16).prop_map(|(n, length)| Step::Put { n, length }),
		2 => any::<u8>().prop_map(|n| Step::Delete { n }),
		1 => Just(Step::Checkpoint),
		1 => Just(Step::Reopen),
	]
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(32))]

	/// After any steps, the database holds what a map of the same steps holds.
	#[test]
	fn the_database_holds_what_was_written(steps in proptest::collection::vec(step(), 1..60)) {
		let directory = directory("property");
		let path = directory.join("state.sqlite");
		let key = key("state key");
		let mut model: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
		let mut database = open(&path, &key).expect("creates");
		database
			.connection()
			.execute_batch("CREATE TABLE rows (n INTEGER PRIMARY KEY, value BLOB NOT NULL)")
			.expect("makes the table");
		let mut rng = TestRng::new("values");
		for step in steps {
			match step {
				Step::Put { n, length } => {
					let mut value = vec![0; usize::from(length)];
					rng.fill_bytes(&mut value);
					database
						.connection()
						.execute("INSERT OR REPLACE INTO rows VALUES (?1, ?2)", params![n, value])
						.expect("puts");
					model.insert(n, value);
				}
				Step::Delete { n } => {
					database
						.connection()
						.execute("DELETE FROM rows WHERE n = ?1", params![n])
						.expect("deletes");
					model.remove(&n);
				}
				Step::Checkpoint => checkpoint(&database),
				Step::Reopen => {
					drop(database);
					database = open(&path, &key).expect("opens again");
				}
			}
		}
		let mut query = database
			.connection()
			.prepare("SELECT n, value FROM rows ORDER BY n")
			.expect("prepares");
		let stored: BTreeMap<u8, Vec<u8>> = query
			.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
			.expect("reads")
			.collect::<Result<_, _>>()
			.expect("reads every row");
		prop_assert_eq!(stored, model);
	}
}

#[test]
fn a_database_named_another_way_is_the_same_database() {
	let directory = directory("in use");
	fs::create_dir_all(directory.join("sub")).expect("makes a directory");
	let path = directory.join("state.sqlite");
	let database = open(&path, &key("state key")).expect("creates");
	fill(&database, 1);
	// Under another key, a name that leads to it is refused; under its own, it is the same
	// database, which another connection reads.
	#[cfg(unix)]
	{
		// A link to the directory, and a link to the file itself.
		std::os::unix::fs::symlink(&directory, directory.join("link")).expect("links");
		std::os::unix::fs::symlink(&path, directory.join("alias.sqlite")).expect("links");
	}
	let names = [
		directory.join("sub").join("..").join("state.sqlite"),
		#[cfg(unix)]
		directory.join("link").join("state.sqlite"),
		#[cfg(unix)]
		directory.join("alias.sqlite"),
	];
	for name in &names {
		assert!(
			matches!(open(name, &key("another key")), Err(StoreError::InUse)),
			"{}",
			name.display()
		);
		let same = open(name, &key("state key")).expect("opens");
		assert_eq!(count(&same).expect("counts"), 1, "{}", name.display());
	}
	drop(database);
	assert!(open(&names[0], &key("state key")).is_ok());
}

#[test]
fn a_file_cut_short_while_it_was_made_is_refused_until_made_again() {
	let directory = directory("cut while made");
	let path = directory.join("state.sqlite");
	// From 2 bytes: SQLite's own VFS reports a file of one byte as empty, working around a
	// bug of a file system, and so it is.
	for length in [2, 30, HEADER] {
		fs::write(&path, vec![0x5a; length]).expect("writes");
		// Another process may be writing the header when this one opens the file: a file of no
		// more than its header reads as empty, and its header is checked when it is first
		// written, under SQLite's lock. The VFS changes nothing: making it again is for
		// whoever has the space to itself.
		let refused = open(&path, &key("state key")).and_then(|database| {
			database
				.connection()
				.execute_batch("CREATE TABLE rows (n INTEGER PRIMARY KEY)")
				.map_err(StoreError::Database)
		});
		assert_eq!(
			code(&refused.expect_err("not one that the key opens")),
			Some(NOTADB)
		);
		assert_eq!(
			fs::read(&path).expect("reads"),
			vec![0x5a; length],
			"untouched"
		);
		fs::remove_file(&path).expect("removes");
		let database = open(&path, &key("state key")).expect("made again");
		fill(&database, 2);
		drop(database);
		assert_eq!(
			count(&open(&path, &key("state key")).expect("opens")).expect("counts"),
			2
		);
		fs::remove_file(&path).expect("removes");
	}
}

#[test]
fn a_damaged_frame_ends_the_log_while_recovering_and_is_an_error_after() {
	let directory = directory("damaged frame");
	let path = directory.join("state.sqlite");
	let key = key("state key");
	// Open, so that everything stays in the write-ahead log.
	let database = open(&path, &key).expect("creates");
	fill(&database, 60);
	let frame = 24 + 4096 + 2 * 40;
	let frames = (fs::read(wal(&path)).expect("a log").len() - 72) / frame;
	assert!(frames > 4);

	// Damaged before it is recovered, the log ends at the frame, with fewer rows.
	let rows = rows_with_log(&path, &key, |log| {
		log[72 + (frames / 2) * frame + 64 + 100] ^= 1;
	});
	assert!(rows < 60, "{rows} rows");

	// Damaged after: reading the page is an error.
	let copies = directory.join("after");
	fs::create_dir_all(&copies).expect("makes the directory");
	let copy = copies.join("state.sqlite");
	fs::copy(&path, &copy).expect("copies the file");
	fs::copy(wal(&path), wal(&copy)).expect("copies the log");
	let opened = open(&copy, &key).expect("opens the copy");
	opened
		.connection()
		.pragma_update(None, "cache_size", 1)
		.expect("a small cache");
	let mut log = fs::read(wal(&copy)).expect("reads the log");
	for start in (72..=log.len() - frame).step_by(frame) {
		log[start + 64 + 100] ^= 1;
	}
	fs::write(wal(&copy), log).expect("damages the log");
	let error = opened
		.connection()
		.query_row("SELECT sum(length(value)) FROM rows", [], |row| {
			row.get::<_, i64>(0)
		})
		.expect_err("damaged pages");
	assert_eq!(rusqlite_code(&error), Some(IOERR_DATA));
	drop(database);
}
