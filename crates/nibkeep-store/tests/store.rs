//! The log of a space on disk: restarts, purges, and files that are damaged or foreign.

use std::fs;
use std::path::{Path, PathBuf};

use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, StatementId};
use nibkeep_crypto::{Envelope, Key, SigningKey};
use nibkeep_log::{
	Change, Draft, History, Log, LogError, Parcel, Payload, Received, SpaceKeys, WrappedKey,
};
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoreError, StoredLog, log_path};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const DRIFT: u64 = 3_600_000;

fn start() -> u64 {
	millis_at("2026-10-10", 10, 0)
}

/// An empty directory for one test.
fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn password() -> FieldName {
	field("org.nibkeep.password")
}

/// A device with a space key and the space's secrets key.
struct Setup {
	rng: TestRng,
	keys: SpaceKeys,
	author: SigningKey,
	path: PathBuf,
	space: nibkeep_base::SpaceId,
	login: EntityId,
}

fn setup(name: &str) -> Setup {
	let mut rng = TestRng::new(name);
	let space = rng.space_id();
	let mut keys = SpaceKeys::new(0, rng.key());
	keys.insert_secrets(0, rng.key());
	let author = rng.signing_key();
	let login = rng.entity_id();
	let path = log_path(&directory(name), space);
	Setup {
		rng,
		keys,
		author,
		path,
		space,
		login,
	}
}

fn open(setup: &Setup) -> Result<StoredLog, StoreError> {
	StoredLog::open(
		&setup.path,
		setup.space,
		&setup.keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[password()],
	)
}

fn add(rng: &mut TestRng, entity: EntityId, field: FieldName, value: Value) -> Operation {
	Operation::assert(Statement::new(rng.statement_id(), entity, field, value))
}

/// Commits a login with a username and a secret password. Returns the password's ID.
fn commit_login(setup: &mut Setup, stored: &mut StoredLog) -> StatementId {
	let username = add(
		&mut setup.rng,
		setup.login,
		field("org.nibkeep.username"),
		Value::Text("ada".into()),
	);
	let password = add(
		&mut setup.rng,
		setup.login,
		password(),
		Value::Text("hunter2".into()),
	);
	let Operation::Assert { statement, .. } = &password else {
		unreachable!("an assertion");
	};
	let id = statement.id;
	stored
		.commit(
			&setup.author,
			&setup.keys,
			vec![username, password],
			start(),
			&mut setup.rng,
		)
		.expect("commits");
	id
}

/// The current statements of a log's state, for comparing them.
fn current<S: Statements, H: History<Error = S::Error>>(log: &Log<S, H>) -> Vec<Statement> {
	log.state()
		.statements()
		.expect("reads")
		.into_iter()
		.map(std::borrow::Cow::into_owned)
		.collect()
}

#[test]
fn after_a_restart_exactly_what_was_committed_is_there() {
	let mut setup = setup("restart");
	let mut stored = open(&setup).expect("opens");
	commit_login(&mut setup, &mut stored);
	let note = add(
		&mut setup.rng,
		setup.login,
		field("org.example.note"),
		Value::Text("work".into()),
	);
	stored
		.commit(
			&setup.author,
			&setup.keys,
			vec![note],
			start() + 1_000,
			&mut setup.rng,
		)
		.expect("commits");
	let state = current(stored.log());
	let heads = stored.log().heads().expect("reads").clone();
	drop(stored);

	let reopened = open(&setup).expect("opens again");
	assert_eq!(current(reopened.log()), state);
	assert_eq!(reopened.log().heads().expect("reads"), heads);
	assert_eq!(reopened.log().changes().expect("reads").len(), 2);
}

#[test]
fn waiting_changes_survive_a_restart() {
	let mut setup = setup("waiting");
	// Another device makes two changes; this one gets the second first.
	let mut other = Log::new(setup.space, Clock::new(DRIFT));
	let author = setup.rng.signing_key();
	let operation = add(
		&mut setup.rng,
		setup.login,
		field("org.example.note"),
		Value::Integer(1),
	);
	let first = other
		.commit(
			&author,
			&setup.keys,
			vec![operation],
			start(),
			&mut setup.rng,
		)
		.expect("commits");
	let operation = add(
		&mut setup.rng,
		setup.login,
		field("org.example.note"),
		Value::Integer(2),
	);
	let second = other
		.commit(
			&author,
			&setup.keys,
			vec![operation],
			start() + 1_000,
			&mut setup.rng,
		)
		.expect("commits");
	let mut stored = open(&setup).expect("opens");
	assert_eq!(
		stored
			.receive(&second, &setup.keys, start() + 2_000)
			.expect("keeps it"),
		Received::Waiting
	);
	drop(stored);

	let mut stored = open(&setup).expect("opens again");
	assert!(stored.log().contains(second.id()).expect("reads"));
	assert_eq!(
		stored
			.receive(&first, &setup.keys, start() + 2_000)
			.expect("accepts it"),
		Received::Accepted(vec![first.id(), second.id()])
	);
	drop(stored);
	assert_eq!(current(open(&setup).expect("opens").log()), current(&other));
}

/// Whether any of the files of the log contain `bytes`.
fn on_disk(path: &Path, bytes: &[u8]) -> bool {
	let directory = path.parent().expect("a directory");
	fs::read_dir(directory).expect("lists files").any(|entry| {
		let content = fs::read(entry.expect("an entry").path()).unwrap_or_default();
		content.windows(bytes.len()).any(|window| window == bytes)
	})
}

#[test]
fn a_purge_removes_the_key_from_the_disk() {
	let mut setup = setup("purge");
	let mut stored = open(&setup).expect("opens");
	let password = commit_login(&mut setup, &mut stored);
	let wrapped = cbor::to_vec(&stored.log().wrapped_keys_of(password)[0]);
	assert!(on_disk(&setup.path, &wrapped), "the key is stored");

	stored
		.purge(
			&setup.author,
			&setup.keys,
			&[password],
			start() + 1_000,
			&mut setup.rng,
		)
		.expect("purges");
	assert!(
		!on_disk(&setup.path, &wrapped),
		"the key is gone from the disk"
	);
	drop(stored);
	let reopened = open(&setup).expect("opens again");
	assert!(reopened.log().is_purged(password).expect("reads"));
	assert_eq!(reopened.log().wrapped_keys_of(password), []);
}

#[test]
fn the_store_refuses_files_it_did_not_make() {
	let setup = setup("foreign");
	fs::create_dir_all(setup.path.parent().expect("a directory")).expect("creates");

	// Another program's database.
	let other = rusqlite::Connection::open(&setup.path).expect("opens");
	other
		.execute_batch("CREATE TABLE notes (text TEXT);")
		.expect("creates a table");
	drop(other);
	assert!(matches!(open(&setup), Err(StoreError::NotALog)));

	// A log of a later version, which only a newer program reads, and which stays as it
	// was.
	fs::remove_file(&setup.path).expect("removes");
	let newer = rusqlite::Connection::open(&setup.path).expect("opens");
	newer
		.execute_batch("PRAGMA application_id = 1313423947; PRAGMA user_version = 1000;")
		.expect("marks the file");
	drop(newer);
	assert!(matches!(open(&setup), Err(StoreError::Newer)));
	let tables: i64 = rusqlite::Connection::open(&setup.path)
		.expect("opens")
		.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
		.expect("counts");
	assert_eq!(tables, 0, "nothing was written");

	// Not a database at all.
	fs::write(&setup.path, b"not a database, just some text in a file").expect("writes");
	assert!(matches!(open(&setup), Err(StoreError::NotALog)));
}

#[test]
fn dropping_the_stored_log_closes_the_file_and_releases_the_space() {
	let mut setup = setup("closed for exit");
	let mut stored = open(&setup).expect("opens");
	commit_login(&mut setup, &mut stored);
	let wal = setup.path.with_file_name("log.sqlite-wal");
	assert!(
		wal.exists(),
		"the write-ahead log is there while the file is open"
	);
	drop(stored);
	// SQLite removes the write-ahead log when the last connection closes.
	assert!(!wal.exists(), "the file was closed");
	let reopened = open(&setup).expect("opens again: the space was released");
	assert_eq!(reopened.log().changes().expect("reads").len(), 1);
}

#[test]
fn a_log_file_of_an_earlier_version_is_refused_untouched() {
	let setup = setup("earlier version");
	fs::create_dir_all(setup.path.parent().expect("a directory")).expect("creates");
	let old = rusqlite::Connection::open(&setup.path).expect("opens");
	old.execute_batch(
		"PRAGMA application_id = 1313423947; PRAGMA user_version = 0;
		CREATE TABLE changes (sequence INTEGER PRIMARY KEY, id BLOB NOT NULL UNIQUE, change BLOB NOT NULL);",
	)
	.expect("writes a log file of version 0");
	drop(old);
	assert!(matches!(open(&setup), Err(StoreError::NotALog)));
	let columns: i64 = rusqlite::Connection::open(&setup.path)
		.expect("opens")
		.query_row(
			"SELECT count(*) FROM pragma_table_info('changes')",
			[],
			|row| row.get(0),
		)
		.expect("counts");
	assert_eq!(columns, 3, "nothing was written");
}

#[test]
fn a_log_file_opens_only_with_the_storage_key_that_wrote_it() {
	let mut setup = setup("other storage key");
	let mut stored = open(&setup).expect("opens");
	commit_login(&mut setup, &mut stored);
	drop(stored);
	// Another device's storage key: the tags of the changes do not check.
	let other = Key::from_bytes([0x33; 32]);
	assert!(matches!(
		StoredLog::open(
			&setup.path,
			setup.space,
			&setup.keys,
			&other,
			DRIFT,
			[password()]
		),
		Err(StoreError::Damaged(_))
	));
	assert_eq!(
		open(&setup)
			.expect("opens")
			.log()
			.changes()
			.expect("reads")
			.len(),
		1
	);
}

#[test]
fn a_tampered_change_is_detected() {
	let mut setup = setup("tampered");
	let mut stored = open(&setup).expect("opens");
	commit_login(&mut setup, &mut stored);
	drop(stored);

	let connection = rusqlite::Connection::open(&setup.path).expect("opens");
	let mut change: Vec<u8> = connection
		.query_row("SELECT change FROM changes", [], |row| row.get(0))
		.expect("a change");
	let middle = change.len() / 2;
	change[middle] ^= 1;
	connection
		.execute("UPDATE changes SET change = ?1", [change])
		.expect("changes the change");
	drop(connection);
	assert!(matches!(open(&setup), Err(StoreError::Damaged(_))));
}

#[test]
fn the_clock_resumes_after_the_latest_change() {
	let mut setup = setup("clock");
	let mut stored = open(&setup).expect("opens");
	commit_login(&mut setup, &mut stored);
	let latest = stored
		.log()
		.changes()
		.expect("reads")
		.iter()
		.map(Change::timestamp)
		.max();
	drop(stored);

	// The wall clock went back by a minute while the device was off.
	let mut stored = open(&setup).expect("opens again");
	let note = add(
		&mut setup.rng,
		setup.login,
		field("org.example.note"),
		Value::Integer(1),
	);
	let change = stored
		.commit(
			&setup.author,
			&setup.keys,
			vec![note],
			start() - 60_000,
			&mut setup.rng,
		)
		.expect("commits");
	assert!(Some(change.change.timestamp()) > latest);
}

/// The keys without the secrets key: a device whose secrets are locked.
fn locked(keys: &SpaceKeys) -> SpaceKeys {
	let mut keys = keys.clone();
	keys.forget_secrets();
	keys
}

/// A wrapped key that opens with nothing.
fn garbage(rng: &mut TestRng) -> WrappedKey {
	let envelope = Envelope::seal(&rng.key(), b"garbage", b"", rng);
	cbor::from_slice(&cbor::encode(&Cbor::Array(vec![
		0_u64.to_cbor(),
		envelope.to_cbor(),
	])))
	.expect("well-formed")
}

#[test]
fn bad_wrapped_keys_leave_the_disk_with_the_log() {
	let mut setup = setup("bad keys");
	let mut writer = open(&setup).expect("opens");
	let secret = commit_login(&mut setup, &mut writer);
	let good = writer.log().wrapped_keys_of(secret)[0].clone();
	let changes: Vec<Parcel> = writer.log().parcels().expect("reads");
	drop(writer);

	// A device whose secrets are locked gets the changes and, first, some garbage.
	let other = Setup {
		path: log_path(&directory("bad keys, other device"), setup.space),
		..setup
	};
	let mut setup = other;
	let locked_keys = locked(&setup.keys);
	let mut stored = StoredLog::open(
		&setup.path,
		setup.space,
		&locked_keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[password()],
	)
	.expect("opens");
	for change in &changes {
		stored
			.receive(change, &locked_keys, start() + 1_000)
			.expect("accepts");
	}
	for _ in 0..3 {
		let bad = garbage(&mut setup.rng);
		stored
			.receive_wrapped_key(secret, bad, &locked_keys)
			.expect("keeps the key");
	}
	stored
		.receive_wrapped_key(secret, good.clone(), &locked_keys)
		.expect("keeps the key");
	assert_eq!(stored.log().wrapped_keys_of(secret).len(), 4);

	// Unlocked, the device drops the garbage, from the disk too.
	stored.unlock_secrets(&setup.keys).expect("unlocks");
	assert_eq!(
		stored.log().wrapped_keys_of(secret),
		std::slice::from_ref(&good)
	);
	drop(stored);
	let reopened = open(&setup).expect("opens again");
	assert_eq!(reopened.log().wrapped_keys_of(secret), [good]);
	let values: Vec<Value> = reopened
		.log()
		.state()
		.current(setup.login, &password())
		.expect("reads")
		.into_iter()
		.map(|statement| statement.into_owned().value)
		.collect();
	assert_eq!(values, [Value::Text("hunter2".into())]);
}

#[test]
fn a_waiting_change_that_is_rejected_leaves_the_disk() {
	let mut setup = setup("rejected");
	let mut other = Log::new(setup.space, Clock::new(DRIFT));
	let author = setup.rng.signing_key();
	let operation = add(
		&mut setup.rng,
		setup.login,
		field("org.example.note"),
		Value::Integer(1),
	);
	let parent = other
		.commit(
			&author,
			&setup.keys,
			vec![operation],
			start(),
			&mut setup.rng,
		)
		.expect("commits");
	// A faulty change that is not later than its parent, and a change that builds on it.
	let faulty = Change::create(
		setup.space,
		&setup.keys,
		&author,
		&Draft::new([parent.id()], parent.change.timestamp(), Payload::default()),
		&mut setup.rng,
	);
	let child = Change::create(
		setup.space,
		&setup.keys,
		&author,
		&Draft::new(
			[faulty.id()],
			nibkeep_time::Timestamp::new(start() + 1_000, 0),
			Payload::default(),
		),
		&mut setup.rng,
	);
	let mut stored = open(&setup).expect("opens");
	let wall = start() + 2_000;
	assert_eq!(
		stored.receive(&child, &setup.keys, wall).expect("keeps it"),
		Received::Waiting
	);
	stored
		.receive(&parent, &setup.keys, wall)
		.expect("accepts it");
	assert!(matches!(
		stored.receive(&faulty, &setup.keys, wall),
		Err(StoreError::Log(LogError::NotAfterParents))
	));
	drop(stored);

	let reopened = open(&setup).expect("opens again");
	assert!(!reopened.log().contains(child.id()).expect("reads"));
	assert_eq!(reopened.log().changes().expect("reads").len(), 1);
}

#[test]
fn a_foreign_database_is_left_as_it_was() {
	let setup = setup("untouched");
	fs::create_dir_all(setup.path.parent().expect("a directory")).expect("creates");
	let other = rusqlite::Connection::open(&setup.path).expect("opens");
	other
		.execute_batch("CREATE TABLE notes (text TEXT); INSERT INTO notes VALUES ('mine');")
		.expect("writes");
	drop(other);
	let before = fs::read(&setup.path).expect("reads");
	assert!(matches!(open(&setup), Err(StoreError::NotALog)));
	assert_eq!(fs::read(&setup.path).expect("reads"), before);
	let wal = setup.path.with_extension("sqlite-wal");
	assert!(!wal.exists(), "no write-ahead log was created");
}

#[test]
fn a_purge_that_is_set_aside_deletes_nothing() {
	let mut setup = setup("set aside purge");
	let mut laptop = open(&setup).expect("opens");
	laptop.expect_governed();
	let phone_key = setup.rng.signing_key();
	let space = setup.space;
	let phone_record = nibkeep_governance::replica_record(space, &phone_key.verifying_key());
	let owner = setup.rng.entity_id();
	// Each key with its own listing, on its record derived from the space and the key.
	let listing = |rng: &mut TestRng, key: &SigningKey| -> Vec<Operation> {
		let encryption = nibkeep_crypto::DecryptionKey::generate(rng).encryption_key();
		let signed = nibkeep_governance::Listing::sign(key, &encryption, space, (owner, false));
		let record = signed.record;
		vec![
			add(
				rng,
				record,
				nibkeep_model::fields::SIGNING_KEY,
				Value::Text(key.verifying_key().to_text()),
			),
			add(
				rng,
				record,
				nibkeep_model::fields::OWNER,
				Value::Reference(owner),
			),
			add(
				rng,
				record,
				nibkeep_model::fields::ENCRYPTION_KEY,
				Value::Text(nibkeep_governance::encryption_key_text(&encryption)),
			),
			add(
				rng,
				record,
				nibkeep_model::fields::LISTING,
				Value::Text(signed.to_text()),
			),
		]
	};
	let author = setup.author.clone();
	for key in [&author, &phone_key] {
		let listing = listing(&mut setup.rng, key);
		laptop
			.commit(&setup.author, &setup.keys, listing, start(), &mut setup.rng)
			.expect("lists");
	}
	let content = laptop
		.import_blob(&setup.keys, &b"a photo"[..], start(), &mut setup.rng)
		.expect("imports");
	// The phone gets everything, and purges the photo.
	let mut phone = Log::new(setup.space, Clock::new(DRIFT));
	for parcel in laptop.log().parcels().expect("reads") {
		phone
			.receive(parcel, &setup.keys, start() + 10_000)
			.expect("takes");
	}
	let blob_keys: Vec<_> = laptop
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, key)| (*sealed, key.clone()))
		.collect();
	for (sealed, key) in blob_keys {
		phone
			.receive_blob_key(sealed, key, &setup.keys)
			.expect("keeps the key");
	}
	let purge = phone
		.purge_blob(
			&phone_key,
			&setup.keys,
			&content,
			start() + 20_000,
			&mut setup.rng,
		)
		.expect("purges");
	// The laptop revokes the phone before the purge reaches it.
	let revoking = add(
		&mut setup.rng,
		phone_record,
		nibkeep_model::fields::REVOKED,
		Value::Text(phone_key.verifying_key().to_text()),
	);
	laptop
		.commit(
			&setup.author,
			&setup.keys,
			vec![revoking],
			start() + 30_000,
			&mut setup.rng,
		)
		.expect("revokes");
	laptop
		.receive(&purge, &setup.keys, start() + 40_000)
		.expect("takes the purge");
	assert!(laptop.log().is_set_aside(purge.id()).expect("reads"));
	assert!(laptop.holds_blob(&content), "the photo stays");
	drop(laptop);
	let reopened = open(&setup).expect("opens again");
	assert!(reopened.holds_blob(&content), "and stays on the disk");
}
