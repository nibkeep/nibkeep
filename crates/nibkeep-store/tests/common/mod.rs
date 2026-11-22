//! What the crash test and the power-loss test write to an encrypted database, and what
//! they check of it after: one workload, so that both test the same. Several writers can
//! share one database, each with a table of its own, and one table that they all add to
//! and delete from.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use std::path::Path;

use nibkeep_store::EncryptedDatabase;
use nibkeep_testkit::{TestRng, child};

/// The most rows that one transaction writes.
const MOST_ROWS: i64 = 7;

/// Opens the database of the tests, with randomness seeded by `seed`.
pub(crate) fn open(path: &Path, seed: &str) -> EncryptedDatabase {
	EncryptedDatabase::open(
		path,
		&TestRng::new("encrypted workload key").key(),
		TestRng::new(seed),
	)
	.unwrap_or_else(|error| panic!("opens the database: {error}"))
}

/// Makes the tables of the writer `name`, and the one that all writers share.
pub(crate) fn make_tables(database: &EncryptedDatabase, name: &str) {
	database
		.connection()
		.execute_batch(&format!(
			"CREATE TABLE IF NOT EXISTS rows_{name} (n INTEGER PRIMARY KEY, value BLOB NOT NULL);
			 CREATE TABLE IF NOT EXISTS others (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;"
		))
		.expect("makes the tables");
}

/// The rows of the writer `name`, and the last of them: they are numbered from 1.
fn rows(database: &EncryptedDatabase, name: &str) -> (i64, i64) {
	database
		.connection()
		.query_row(
			&format!("SELECT count(*), coalesce(max(n), 0) FROM rows_{name}"),
			[],
			|row| Ok((row.get(0)?, row.get(1)?)),
		)
		.expect("counts")
}

/// Writes one transaction as the writer `name`, after the row `last`: one to seven rows of
/// its own, and rows of the shared table added and deleted, so that pages are freed and
/// used again. Returns the last row written.
pub(crate) fn write_once(database: &mut EncryptedDatabase, name: &str, last: i64) -> i64 {
	let transaction = database.connection_mut().transaction().expect("begins");
	let mut next = last + 1;
	for _ in 0..=next % MOST_ROWS {
		transaction
			.execute(
				&format!("INSERT INTO rows_{name} (n, value) VALUES (?1, randomblob(700))"),
				[next],
			)
			.expect("inserts");
		transaction
			.execute(
				"INSERT INTO others (key, value) VALUES (randomblob(16), randomblob(300))",
				[],
			)
			.expect("inserts another");
		next += 1;
	}
	transaction
		.execute(
			"DELETE FROM others WHERE key IN (SELECT key FROM others LIMIT 2)",
			[],
		)
		.expect("deletes");
	transaction.commit().expect("commits");
	next - 1
}

/// Writes transactions as the writer `name` to the database at `path` until the process is
/// killed, noting each that commits in `directory`. Never returns.
pub(crate) fn write_until_killed(directory: &Path, path: &Path, name: &str) -> ! {
	let [noted] = child::noted(directory);
	// Every run starts after more was kept, so its randomness is its own.
	let mut database = open(path, &format!("encrypted workload {name} {noted}"));
	make_tables(&database, name);
	let mut last = rows(&database, name).1;
	loop {
		last = write_once(&mut database, name, last);
		child::note(directory, &[usize::try_from(last).expect("positive")]);
	}
}

/// Checks the rows of the writer `name` after a crash in round `round`: they are whole
/// transactions, and hold every row noted, and perhaps the transaction being noted when the
/// writer died. Returns how many rows there are.
pub(crate) fn check(database: &EncryptedDatabase, name: &str, noted: usize, round: u64) -> usize {
	let (count, last) = rows(database, name);
	assert_eq!(
		count, last,
		"round {round}: the rows of {name} are whole transactions"
	);
	let noted = i64::try_from(noted).expect("small");
	assert!(
		(noted..=noted + MOST_ROWS).contains(&last),
		"round {round}: {noted} rows of {name} noted, {last} kept"
	);
	usize::try_from(last).expect("positive")
}

/// Checks the rows of the writer `name` while it may still write: they are whole
/// transactions, and hold at least the rows noted before. Returns how many there are.
pub(crate) fn check_while_writing(
	database: &EncryptedDatabase,
	name: &str,
	noted: usize,
	round: u64,
) -> usize {
	let (count, last) = rows(database, name);
	assert_eq!(
		count, last,
		"round {round}: the rows of {name} are whole transactions"
	);
	let last = usize::try_from(last).expect("positive");
	assert!(
		last >= noted,
		"round {round}: {noted} rows of {name} noted, {last} seen"
	);
	last
}

/// Checks that the database is sound.
pub(crate) fn check_sound(database: &EncryptedDatabase, round: u64) {
	let integrity: String = database
		.connection()
		.query_row("PRAGMA integrity_check", [], |row| row.get(0))
		.expect("checks");
	assert_eq!(integrity, "ok", "round {round}");
}
