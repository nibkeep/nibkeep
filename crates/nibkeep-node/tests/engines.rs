//! Engines of different versions on one replica: each program records in the replica's
//! engines file the latest version of its stored files that its engine reads, and an engine
//! that finds them of a later version opens nothing, writes nothing, and asks for an update
//! (technical design: separate apps on one library).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{files, now};
use nibkeep_base::cbor::{self, Integer, Value};
use nibkeep_node::{
	DAY, ENGINE_VERSION, Library, NewLibrary, NewProtection, NodeError, PassphraseParams,
	REPLICA_VERSION, Unlocker, name_program,
};
use nibkeep_testkit::TestRng;

const PASSPHRASE: &str = "correct horse";

/// The name that every test here gives its program.
const PROGRAM: &str = "engines test";

fn unlocker() -> Unlocker<'static> {
	Unlocker::Passphrase(PASSPHRASE)
}

/// A new library in a directory of its own, closed again, made by this program.
fn library(name: &str) -> PathBuf {
	name_program(PROGRAM);
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("engines {name}"))
			.join(".nib");
	let mut rng = TestRng::new(name);
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid")),
		now(),
		&mut rng,
	)
	.expect("starts");
	let personal = library.personal_space();
	let mut library = library;
	library
		.create_entity(personal, &[], vec![], now() + 1_000, &mut rng)
		.expect("writes");
	drop(library);
	directory
}

/// The engines file of the library's replica.
fn engines_path(directory: &Path) -> PathBuf {
	Library::replica_folder(directory)
		.expect("a replica")
		.join("engines.cbor")
}

fn engines(directory: &Path) -> Value {
	cbor::decode(&fs::read(engines_path(directory)).expect("reads")).expect("decodes")
}

fn write_engines(directory: &Path, engines: &Value) {
	fs::write(engines_path(directory), cbor::encode(engines)).expect("writes");
}

fn integer(number: u64) -> Value {
	Value::Integer(Integer::from(number))
}

/// A program's entry: `[name, reads, engine, opened]`.
fn entry(name: &str, reads: u64, engine: &str, opened: u64) -> Value {
	Value::Array(vec![
		name.into(),
		integer(reads),
		engine.into(),
		integer(opened),
	])
}

/// The version and the entries of an engines file, and when each entry says its program last
/// opened the replica, apart.
fn read(engines: &Value) -> (u64, Vec<Value>) {
	let Value::Array(items) = engines else {
		panic!("an array: {engines:?}");
	};
	let Value::Integer(version) = &items[0] else {
		panic!("a version: {engines:?}");
	};
	let Value::Array(programs) = &items[1] else {
		panic!("the programs: {engines:?}");
	};
	(
		u64::try_from(*version).expect("a version"),
		programs.clone(),
	)
}

/// When an entry says its program last opened the replica.
fn opened(entry: &Value) -> u64 {
	let Value::Array(items) = entry else {
		panic!("an entry: {entry:?}");
	};
	let Value::Integer(opened) = items[3] else {
		panic!("a time: {entry:?}");
	};
	u64::try_from(opened).expect("a time")
}

/// The entry with its time left out, to compare.
fn untimed(entry: &Value) -> Value {
	let Value::Array(items) = entry else {
		panic!("an entry: {entry:?}");
	};
	let mut items = items.clone();
	items.remove(3);
	Value::Array(items)
}

/// The computer's wall clock, in milliseconds since the Unix epoch.
fn wall() -> u64 {
	u64::try_from(
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.expect("after the epoch")
			.as_millis(),
	)
	.expect("fits")
}

#[test]
fn a_new_replica_records_its_version_and_the_program_that_made_it() {
	let directory = library("new");
	let (version, programs) = read(&engines(&directory));
	assert_eq!(version, REPLICA_VERSION);
	assert_eq!(REPLICA_VERSION, 1);
	// Made at the time that the library was made at.
	assert_eq!(
		programs,
		[entry(PROGRAM, REPLICA_VERSION, ENGINE_VERSION, now())]
	);
}

#[test]
fn opening_records_the_program_at_most_once_a_day() {
	let directory = library("once a day");
	// Opened just now: opening again the same day writes nothing.
	let today = Value::Array(vec![
		integer(1),
		Value::Array(vec![entry(
			PROGRAM,
			REPLICA_VERSION,
			ENGINE_VERSION,
			wall(),
		)]),
	]);
	write_engines(&directory, &today);
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	assert_eq!(engines(&directory), today);
	// Opened two days ago: opening records the day.
	let earlier = wall() - 2 * DAY;
	write_engines(
		&directory,
		&Value::Array(vec![
			integer(1),
			Value::Array(vec![entry(
				PROGRAM,
				REPLICA_VERSION,
				ENGINE_VERSION,
				earlier,
			)]),
		]),
	);
	let before = wall();
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	let (_, programs) = read(&engines(&directory));
	assert_eq!(programs.len(), 1, "{programs:?}");
	assert!(opened(&programs[0]) >= before, "{programs:?}");
	// An entry from another engine of the program, as before an update, is replaced the
	// same day.
	write_engines(
		&directory,
		&Value::Array(vec![
			integer(1),
			Value::Array(vec![entry(PROGRAM, REPLICA_VERSION, "0.0.1", wall())]),
		]),
	);
	let closed = Library::open(&directory, &unlocker())
		.expect("opens")
		.close();
	let (_, programs) = read(&engines(&directory));
	assert_eq!(
		untimed(&programs[0]),
		untimed(&entry(PROGRAM, REPLICA_VERSION, ENGINE_VERSION, 0))
	);
	// Opening a closed library records it too.
	write_engines(
		&directory,
		&Value::Array(vec![
			integer(1),
			Value::Array(vec![entry(PROGRAM, REPLICA_VERSION, "0.0.2", wall())]),
		]),
	);
	drop(closed.open().expect("opens again"));
	let (_, programs) = read(&engines(&directory));
	assert_eq!(
		untimed(&programs[0]),
		untimed(&entry(PROGRAM, REPLICA_VERSION, ENGINE_VERSION, 0))
	);
}

#[test]
fn opening_keeps_the_other_programs_and_what_it_does_not_know() {
	let directory = library("others");
	// Another app, whose engine of a later version wrote an item after those it shares with
	// this one, and an item after the programs.
	let passwords = Value::Array(vec![
		"Passwords".into(),
		integer(1),
		"0.0.9".into(),
		integer(5),
		"a later item".into(),
	]);
	let editor = entry("Editor", 1, "0.0.8", 6);
	write_engines(
		&directory,
		&Value::Array(vec![
			integer(1),
			Value::Array(vec![passwords.clone(), editor.clone()]),
			"a later item".into(),
		]),
	);
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	let written = engines(&directory);
	let Value::Array(items) = &written else {
		panic!("an array");
	};
	assert_eq!(items.len(), 3, "{written:?}");
	assert_eq!(items[2], "a later item".into());
	let (version, programs) = read(&written);
	assert_eq!(version, 1);
	assert_eq!(programs.len(), 3, "{programs:?}");
	assert_eq!(programs[0], passwords, "kept as it was");
	assert_eq!(programs[1], editor, "kept as it was");
	assert_eq!(
		untimed(&programs[2]),
		untimed(&entry(PROGRAM, REPLICA_VERSION, ENGINE_VERSION, 0)),
		"added at the end"
	);
}

#[test]
fn a_replica_without_an_engines_file_is_of_the_first_version() {
	let directory = library("without");
	fs::remove_file(engines_path(&directory)).expect("removes");
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	let (version, programs) = read(&engines(&directory));
	assert_eq!(version, 1);
	assert_eq!(
		programs.iter().map(untimed).collect::<Vec<_>>(),
		[untimed(&entry(PROGRAM, REPLICA_VERSION, ENGINE_VERSION, 0))]
	);
}

#[test]
fn a_replica_of_a_later_version_is_neither_opened_nor_written() {
	let directory = library("later");
	let closed = Library::open(&directory, &unlocker())
		.expect("opens")
		.close();
	write_engines(
		&directory,
		&Value::Array(vec![
			integer(REPLICA_VERSION + 1),
			Value::Array(vec![entry(
				"Passwords",
				REPLICA_VERSION + 1,
				"9.0.0",
				wall(),
			)]),
		]),
	);
	let before = files(&directory);
	assert!(matches!(
		Library::open(&directory, &unlocker()),
		Err(NodeError::NewerFormats)
	));
	assert!(matches!(
		Library::protection(&directory),
		Err(NodeError::NewerFormats)
	));
	assert!(matches!(closed.open(), Err(NodeError::NewerFormats)));
	assert!(matches!(
		Library::drive_checked(&directory),
		Err(NodeError::NewerFormats)
	));
	assert_eq!(files(&directory), before, "nothing was written");
	assert!(
		NodeError::NewerFormats.to_string().contains("update"),
		"it asks for an update"
	);
	// A later version that keeps its keys elsewhere leaves a replica that is not taken for
	// one whose making was cut short, which an app may offer to remove.
	let folder = Library::replica_folder(&directory).expect("a replica");
	fs::remove_file(folder.join("keys.cbor")).expect("removes");
	assert!(matches!(
		Library::protection(&directory),
		Err(NodeError::NewerFormats)
	));
}

#[test]
fn a_program_that_cannot_record_itself_opens_all_the_same() {
	let directory = library("cannot record");
	let earlier = Value::Array(vec![
		integer(1),
		Value::Array(vec![entry(
			PROGRAM,
			REPLICA_VERSION,
			ENGINE_VERSION,
			wall() - 2 * DAY,
		)]),
	]);
	write_engines(&directory, &earlier);
	// As while another program holds what recording needs.
	let lock = Library::replica_folder(&directory)
		.expect("a replica")
		.join("engines.lock");
	let _ = fs::remove_file(&lock);
	fs::create_dir(&lock).expect("makes a folder in its place");
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	assert_eq!(engines(&directory), earlier, "recorded nothing");
	fs::remove_dir(&lock).expect("removes");
	drop(Library::open(&directory, &unlocker()).expect("opens"));
	let (_, programs) = read(&engines(&directory));
	assert!(
		opened(&programs[0]) > wall() - DAY,
		"recorded the next time"
	);
}

#[test]
fn a_damaged_engines_file_keeps_the_replica_closed() {
	let directory = library("damaged");
	for damaged in [
		b"not CBOR at all".to_vec(),
		cbor::encode(&Value::Array(vec![integer(0), Value::Array(vec![])])),
		cbor::encode(&Value::Array(vec![integer(1)])),
		cbor::encode(&Value::Array(vec![
			integer(1),
			Value::Array(vec![Value::Array(vec!["Passwords".into()])]),
		])),
	] {
		fs::write(engines_path(&directory), &damaged).expect("writes");
		let before = files(&directory);
		assert!(
			matches!(
				Library::open(&directory, &unlocker()),
				Err(NodeError::Damaged(_))
			),
			"{damaged:?}"
		);
		assert_eq!(files(&directory), before, "nothing was written");
	}
}

#[test]
fn a_log_file_of_a_later_version_asks_for_an_update_and_stays_as_it_was() {
	let directory = library("later log");
	let library = Library::open(&directory, &unlocker()).expect("opens");
	let personal = library.personal_space();
	drop(library);
	let log = Library::replica_folder(&directory)
		.expect("a replica")
		.join("spaces")
		.join(personal.to_string())
		.join("log.sqlite");
	let version = |set: Option<i64>| -> i64 {
		let connection = rusqlite::Connection::open(&log).expect("opens");
		if let Some(set) = set {
			connection
				.pragma_update(None, "user_version", set)
				.expect("sets the version");
		}
		connection
			.query_row("PRAGMA user_version", [], |row| row.get(0))
			.expect("reads the version")
	};
	let later = version(None) + 1;
	version(Some(later));
	assert!(matches!(
		Library::open(&directory, &unlocker()),
		Err(NodeError::NewerFormats)
	));
	assert_eq!(version(None), later, "the log file stays as it was");
}
