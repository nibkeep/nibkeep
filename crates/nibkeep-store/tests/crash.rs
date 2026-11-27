//! A crash in the middle of a write leaves the last committed state.
//!
//! Each test runs itself as a child process that writes until it is killed, and then
//! checks what the child left behind: changes in the log, blobs with their keys, or rows in
//! an encrypted database.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::common;

use nibkeep_base::Hash;
use nibkeep_log::SpaceKeys;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoredLog, log_path};
use nibkeep_testkit::{TestRng, child, millis_at};

const DRIFT: u64 = 3_600_000;

/// Set in the child process, to the directory it writes to.
const CHILD: &str = "NIBKEEP_CRASH_TEST_DIRECTORY";

/// Set in the child process that imports blobs, to the directory it writes to.
const BLOBS_CHILD: &str = "NIBKEEP_CRASH_TEST_BLOBS_DIRECTORY";

/// Set in the child process that writes an encrypted database, to the directory it writes
/// to.
const ENCRYPTED_CHILD: &str = "NIBKEEP_CRASH_TEST_ENCRYPTED_DIRECTORY";

/// What the parent and the child agree on: the space, its key, and the author.
fn space() -> (nibkeep_base::SpaceId, SpaceKeys, nibkeep_crypto::SigningKey) {
	let mut rng = TestRng::new("crash test");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	(space, keys, rng.signing_key())
}

/// Commits changes until the process is killed. Never returns.
fn write_until_killed(directory: &Path) -> ! {
	let (space, keys, author) = space();
	let mut stored = StoredLog::open(
		&log_path(directory, space),
		space,
		&keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[],
	)
	.expect("opens the log");
	let mut committed = stored.log().changes().expect("reads").len();
	// A new seed for every run, so that statement IDs never repeat.
	let mut rng = TestRng::new(&format!("crash child {committed}"));
	let entity = rng.entity_id();
	let field: FieldName = "org.example.counter".parse().expect("a field name");
	loop {
		let count = i64::try_from(committed).expect("small");
		let operation = Operation::assert(Statement::new(
			rng.statement_id(),
			entity,
			field.clone(),
			Value::Integer(count),
		));
		let wall = millis_at("2026-10-10", 10, 0) + u64::try_from(committed).expect("small");
		stored
			.commit(&author, &keys, vec![operation], wall, &mut rng)
			.expect("commits");
		committed += 1;
		child::note(directory, &[committed]);
	}
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn a_crash_while_writing_leaves_the_last_committed_state() {
	if let Some(directory) = std::env::var_os(CHILD) {
		write_until_killed(Path::new(&directory));
	}
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "crash");
	let (space, keys, _) = space();

	for round in 0..6 {
		let mut child = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		child::kill_after_progress(
			&mut child,
			&directory,
			round,
			Duration::from_millis(25 * round),
		);

		// The log opens, every change in it checks out, and it holds every change that was
		// committed, plus perhaps the one that was being noted when the child died.
		let stored = StoredLog::open(
			&log_path(&directory, space),
			space,
			&keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[],
		)
		.expect("opens after the crash");
		let [committed] = child::noted(&directory);
		let stored_count = stored.log().changes().expect("reads").len();
		assert!(
			(committed..=committed + 1).contains(&stored_count),
			"round {round}: {committed} committed, {stored_count} stored"
		);
	}
}

/// Set in the child process that saves batches, to the directory it writes to.
const BATCHES_CHILD: &str = "NIBKEEP_CRASH_TEST_BATCHES_DIRECTORY";

/// How many changes each batch of the child holds.
const BATCH: usize = 5;

/// Commits changes in batches until the process is killed, noting how many were saved
/// after each batch ends. Never returns.
fn save_batches_until_killed(directory: &Path) -> ! {
	let (space, keys, author) = space();
	let mut stored = StoredLog::open(
		&log_path(directory, space),
		space,
		&keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[],
	)
	.expect("opens the log");
	let mut committed = stored.log().changes().expect("reads").len();
	let mut rng = TestRng::new(&format!("crash child in batches {committed}"));
	let entity = rng.entity_id();
	let field: FieldName = "org.example.counter".parse().expect("a field name");
	loop {
		stored.begin_batch(&keys).expect("begins a batch");
		for _ in 0..BATCH {
			let count = i64::try_from(committed).expect("small");
			let operation = Operation::assert(Statement::new(
				rng.statement_id(),
				entity,
				field.clone(),
				Value::Integer(count),
			));
			let wall = millis_at("2026-10-23", 10, 0) + u64::try_from(committed).expect("small");
			stored
				.commit(&author, &keys, vec![operation], wall, &mut rng)
				.expect("commits");
			committed += 1;
		}
		stored.end_batch().expect("ends the batch");
		child::note(directory, &[committed]);
	}
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn a_crash_while_saving_batches_keeps_whole_batches() {
	if let Some(directory) = std::env::var_os(BATCHES_CHILD) {
		save_batches_until_killed(Path::new(&directory));
	}
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "crash in batches");
	let (space, keys, _) = space();

	for round in 0..6 {
		let mut child = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(BATCHES_CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		child::kill_after_progress(
			&mut child,
			&directory,
			round,
			Duration::from_millis(40 * round),
		);

		// The log holds every batch that ended, plus perhaps the one that was being noted
		// when the child died, and never part of a batch; the state follows it.
		let stored = StoredLog::open(
			&log_path(&directory, space),
			space,
			&keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[],
		)
		.expect("opens after the crash");
		let [committed] = child::noted(&directory);
		let stored_count = stored.log().changes().expect("reads").len();
		assert!(
			stored_count == committed || stored_count == committed + BATCH,
			"round {round}: {committed} committed, {stored_count} stored"
		);
		let statements = stored.log().state().statements().expect("reads").len();
		assert_eq!(statements, stored_count, "round {round}");
	}
}

/// The content of the blob that the child imports as its `number`th: several chunks, and
/// different for every number.
fn blob_content(number: usize) -> Vec<u8> {
	let mut content = u64::try_from(number).expect("small").to_le_bytes().to_vec();
	content.extend(
		(0..150_000).map(|index| u8::try_from((index * 31 + number * 7) % 251).expect("below 251")),
	);
	content
}

/// Imports blobs until the process is killed. Never returns.
fn import_until_killed(directory: &Path) -> ! {
	let (space, keys, _) = space();
	let mut stored = StoredLog::open(
		&log_path(directory, space),
		space,
		&keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[],
	)
	.expect("opens the space");
	// Every blob has its own content, so the store's keys count the blobs imported.
	let mut imported = stored
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, _)| *sealed)
		.collect::<BTreeSet<_>>()
		.len();
	let mut rng = TestRng::new(&format!("crash child importing {imported}"));
	loop {
		let wall = millis_at("2026-10-12", 10, 0) + u64::try_from(imported).expect("small");
		stored
			.import_blob(&keys, blob_content(imported).as_slice(), wall, &mut rng)
			.expect("imports");
		imported += 1;
		child::note(directory, &[imported]);
	}
}

/// The sealed hashes of the blob files in place, and the other files under the blob
/// directory.
fn blob_files(blobs: &Path) -> (BTreeSet<Hash>, Vec<PathBuf>) {
	let mut sealed = BTreeSet::new();
	let mut others = Vec::new();
	let mut directories = vec![blobs.to_owned()];
	while let Some(directory) = directories.pop() {
		let Ok(entries) = fs::read_dir(&directory) else {
			continue;
		};
		for entry in entries {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				directories.push(path);
				continue;
			}
			let relative = path.strip_prefix(blobs).expect("inside");
			let parts: Vec<&str> = relative
				.iter()
				.map(|part| part.to_str().expect("a name"))
				.collect();
			match parts.as_slice() {
				["blake3", _, digest] => {
					sealed.insert(format!("blake3:{digest}").parse().expect("a sealed hash"));
				}
				_ => others.push(relative.to_owned()),
			}
		}
	}
	(sealed, others)
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn a_crash_while_importing_leaves_every_blob_whole() {
	if let Some(directory) = std::env::var_os(BLOBS_CHILD) {
		import_until_killed(Path::new(&directory));
	}
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "crash while importing");
	let (space, keys, _) = space();
	let path = log_path(&directory, space);

	for round in 0..6 {
		let mut child = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(BLOBS_CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		child::kill_after_progress(
			&mut child,
			&directory,
			round,
			Duration::from_millis(25 * round),
		);

		let stored = StoredLog::open(
			&path,
			space,
			&keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[],
		)
		.expect("opens after the crash");
		let [imported] = child::noted(&directory);
		// Every blob that was imported is there and whole, and perhaps the one that was
		// being noted when the child died.
		for number in 0..=imported {
			let content = blob_content(number);
			let hash = Hash::of(&content);
			if number == imported && !stored.holds_blob(&hash) {
				continue;
			}
			let mut read = Vec::new();
			stored
				.open_blob(&hash)
				.expect("holds the blob")
				.read_to_end(&mut read)
				.expect("reads the blob");
			assert!(read == content, "round {round}: blob {number} is whole");
		}
		// Every blob in place has its key, every key its blob, and nothing else is left, of
		// what was being written either.
		let (sealed, mut others) = blob_files(&nibkeep_store::blob_directory(&directory));
		others.extend(blob_files(&path.with_file_name("tmp")).1);
		let keyed: BTreeSet<Hash> = stored
			.log()
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect();
		assert_eq!(sealed, keyed, "round {round}");
		nibkeep_testkit::assert_empty!(others, "round {round}: {others:?}");
		assert!(
			(imported..=imported + 1).contains(&sealed.len()),
			"round {round}"
		);
	}
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn a_crash_while_writing_an_encrypted_database_loses_nothing_committed() {
	if let Some(directory) = std::env::var_os(ENCRYPTED_CHILD) {
		let directory = Path::new(&directory);
		common::write_until_killed(directory, &directory.join("state.sqlite"), "child");
	}
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "crash while encrypting");
	for round in 0..12 {
		let mut child = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(ENCRYPTED_CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		// Up to a second of writing, so that some crashes come while SQLite moves its
		// write-ahead log into the database.
		child::kill_after_progress(
			&mut child,
			&directory,
			round,
			Duration::from_millis(80 * round),
		);
		let [noted] = child::noted(&directory);
		let database = common::open(
			&directory.join("state.sqlite"),
			&format!("crash test reading {round}"),
		);
		common::check_sound(&database, round);
		common::check(&database, "child", noted, round);
	}
}

/// Set in the child process that writes beside the test's own process, to the directory of
/// its notes, whose parent holds the database.
const SHARED_CHILD: &str = "NIBKEEP_CRASH_TEST_SHARED_DIRECTORY";

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn two_processes_write_an_encrypted_database_at_once_and_a_crash_harms_neither() {
	if let Some(notebook) = std::env::var_os(SHARED_CHILD) {
		let notebook = Path::new(&notebook);
		let database = notebook.parent().expect("a parent").join("state.sqlite");
		common::write_until_killed(notebook, &database, "child");
	}
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "two processes encrypting");
	let notebook = directory.join("child");
	fs::create_dir_all(&notebook).expect("makes the directory");
	let path = directory.join("state.sqlite");
	let mut database = common::open(&path, "the test's own writer");
	common::make_tables(&database, "parent");
	common::make_tables(&database, "child");
	let mut last = 0;
	for round in 0..4 {
		let mut child = child::Guard(
			Command::new(std::env::current_exe().expect("this test"))
				.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
				.env(SHARED_CHILD, &notebook)
				.stdout(Stdio::null())
				.stderr(child::errors(&notebook))
				.spawn()
				.expect("starts the child"),
		);
		// Writing and reading while the child writes, until both have written a good deal
		// at once: the child's rows grow, and are always whole transactions, as this process
		// sees them.
		let started = std::time::Instant::now();
		let [before] = child::noted(&notebook);
		let mut seen = 0;
		let mut step = 0;
		while step < 150 || child::noted::<1>(&notebook)[0] < before + 300 {
			assert!(
				started.elapsed() < Duration::from_secs(60),
				"round {round}: the child wrote too little beside this process"
			);
			last = common::write_once(&mut database, "parent", last);
			if step % 10 == 0 {
				let [noted] = child::noted(&notebook);
				let kept = common::check_while_writing(&database, "child", noted, round);
				assert!(kept >= seen, "round {round}: the child's rows only grow");
				seen = kept;
			}
			step += 1;
		}
		// The child dies in the middle of its work, while this process goes on.
		child.kill().expect("kills the child");
		child.wait().expect("waits for the child");
		for _ in 0..20 {
			last = common::write_once(&mut database, "parent", last);
		}
		let [noted] = child::noted(&notebook);
		common::check_sound(&database, round);
		common::check(&database, "child", noted, round);
		common::check(
			&database,
			"parent",
			usize::try_from(last).expect("positive"),
			round,
		);
	}
	drop(database);
	let reopened = common::open(&path, "reading after");
	common::check_sound(&reopened, 4);
	common::check(
		&reopened,
		"parent",
		usize::try_from(last).expect("positive"),
		4,
	);
}

/// Set in the child process that purges, to the directory it writes to.
const PURGE_CHILD: &str = "NIBKEEP_CRASH_TEST_PURGE_DIRECTORY";

/// Commits changes of two values and purges one of them, until the process is killed.
/// Never returns.
fn purge_until_killed(directory: &Path) -> ! {
	let (space, keys, author) = space();
	let mut stored = StoredLog::open(
		&log_path(directory, space),
		space,
		&keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[],
	)
	.expect("opens the log");
	let mut done = stored.log().changes().expect("reads").len();
	let mut rng = TestRng::new(&format!("purging child {done}"));
	let field: FieldName = "org.example.note".parse().expect("a field name");
	loop {
		let entity = rng.entity_id();
		let [kept, purged] = [0, 1]
			.map(|n| Statement::new(rng.statement_id(), entity, field.clone(), Value::Integer(n)));
		let wall = millis_at("2026-10-10", 10, 0) + u64::try_from(done).expect("small") * 10;
		stored
			.commit(
				&author,
				&keys,
				vec![Operation::assert(kept), Operation::assert(purged.clone())],
				wall,
				&mut rng,
			)
			.expect("commits");
		stored
			.purge(&author, &keys, &[purged.id], wall + 1, &mut rng)
			.expect("purges");
		done += 2;
		child::note(directory, &[done]);
	}
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn a_crash_while_purging_leaves_nothing_purged_in_either_file() {
	if let Some(directory) = std::env::var_os(PURGE_CHILD) {
		purge_until_killed(Path::new(&directory));
	}
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "crash purge");
	let (space, keys, _) = space();
	let path = log_path(&directory, space);
	for round in 0..8 {
		let mut child = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(PURGE_CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		child::kill_after_progress(
			&mut child,
			&directory,
			round,
			Duration::from_millis(40 * round),
		);
		let stored = StoredLog::open(
			&path,
			space,
			&keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[],
		)
		.expect("opens after the crash");
		let purged: BTreeSet<Hash> = stored
			.log()
			.purged_changes()
			.expect("reads")
			.into_iter()
			.map(|change| *change.hash())
			.collect();
		assert!(
			!purged.is_empty() || round == 0,
			"round {round}: purges were made"
		);
		// The state holds nothing of a purged change.
		for statement in stored.log().state().statements().expect("reads") {
			let recorded = stored
				.log()
				.state()
				.storage()
				.get(statement.id)
				.expect("reads")
				.expect("the statement");
			assert!(
				recorded
					.change
					.is_none_or(|change| !purged.contains(&change)),
				"round {round}: the state holds a purged change's statement"
			);
		}
		drop(stored);
		// The log file holds no payload or key of a purged change.
		let connection = rusqlite::Connection::open(&path).expect("opens the log file");
		for change in &purged {
			let id = change.to_multihash();
			let payload: Option<Vec<u8>> = connection
				.query_row("SELECT payload FROM changes WHERE id = ?1", [&id], |row| {
					row.get(0)
				})
				.expect("the change's row");
			assert!(
				payload.is_none(),
				"round {round}: a purged payload is stored"
			);
			let keys: i64 = connection
				.query_row(
					"SELECT count(*) FROM change_keys WHERE change = ?1",
					[&id],
					|row| row.get(0),
				)
				.expect("counts");
			assert_eq!(keys, 0, "round {round}: a purged key is stored");
		}
	}
}
