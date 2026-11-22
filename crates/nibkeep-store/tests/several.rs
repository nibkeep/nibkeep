//! Several stored logs of one space at once, as several programs, or two syncs in one, have
//! it open ([technical design](../../../docs/Technical-design.md#78-several-programs-on-one-replica)): each catches
//! up with what the others wrote before it writes; a purge waits for the others to close
//! the space, which makes its state file anew alone; and only a stored log that has the
//! space alone puts it in order after a crash.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::SigningKey;
use nibkeep_log::SpaceKeys;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoreError, StoredLog, log_path};
use nibkeep_testkit::{TestRng, millis_at};

const DRIFT: u64 = 3_600_000;

fn start() -> u64 {
	millis_at("2026-10-31", 10, 0)
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn password() -> FieldName {
	field("org.nibkeep.password")
}

/// A space on one device, which several stored logs open.
struct OneSpace {
	rng: TestRng,
	keys: SpaceKeys,
	author: SigningKey,
	path: PathBuf,
	id: SpaceId,
	login: EntityId,
}

impl OneSpace {
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let space = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		Self {
			author: rng.signing_key(),
			login: rng.entity_id(),
			path: log_path(&directory, space),
			rng,
			keys,
			id: space,
		}
	}

	fn open_with(&self, secret: &[FieldName]) -> Result<StoredLog, StoreError> {
		StoredLog::open(
			&self.path,
			self.id,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			secret.iter().cloned(),
		)
	}

	fn open(&self) -> StoredLog {
		self.open_with(&[password()]).expect("opens")
	}

	/// Commits a value of the login's in `field`; returns the statement's ID.
	fn add(&mut self, stored: &mut StoredLog, field: FieldName, text: &str) -> StatementId {
		let id = self.rng.statement_id();
		let statement = Statement::new(id, self.login, field, Value::Text(text.into()));
		stored
			.commit(
				&self.author,
				&self.keys,
				vec![Operation::assert(statement)],
				start(),
				&mut self.rng,
			)
			.expect("commits");
		id
	}
}

/// The texts of the login's values in a stored log's state, sorted.
fn texts(stored: &StoredLog, field: &FieldName) -> Vec<String> {
	let mut texts: Vec<String> = stored
		.log()
		.state()
		.statements()
		.expect("reads")
		.into_iter()
		.filter(|statement| statement.field == *field)
		.filter_map(|statement| statement.value.as_text().map(str::to_owned))
		.collect();
	texts.sort();
	texts
}

#[test]
fn two_stored_logs_of_one_space_write_in_turn_each_on_what_the_other_wrote() {
	let mut space = OneSpace::new("two at once");
	let note = field("org.example.note");
	let mut one = space.open();
	let mut other = space.open();
	space.add(&mut one, note.clone(), "from one");
	// The other caught up before it wrote: its change builds on the first, not beside it.
	space.add(&mut other, note.clone(), "from the other");
	assert_eq!(other.log().heads().expect("reads").len(), 1);
	assert_eq!(texts(&other, &note), ["from one", "from the other"]);
	// The state file is shared, and the first sees what the other wrote; catching up, it
	// knows its change too.
	assert_eq!(texts(&one, &note), ["from one", "from the other"]);
	let heads = other.log().heads().expect("reads");
	assert!(one.catch_up(&space.keys).expect("catches up"));
	assert!(!one.catch_up(&space.keys).expect("catches up"), "once");
	assert_eq!(one.log().heads().expect("reads"), heads);
	space.add(&mut one, note.clone(), "from one again");
	drop((one, other));
	let reopened = space.open();
	assert_eq!(reopened.log().changes().expect("reads").len(), 3);
	assert_eq!(reopened.log().heads().expect("reads").len(), 1);
	assert_eq!(
		texts(&reopened, &note),
		["from one", "from one again", "from the other"]
	);
}

#[test]
fn a_change_waits_for_the_batch_of_another_and_builds_on_it() {
	let mut space = OneSpace::new("waits for a batch");
	let note = field("org.example.note");
	let mut one = space.open();
	let mut other = space.open();
	one.begin_batch(&space.keys).expect("begins");
	space.add(&mut one, note.clone(), "in the batch");
	let (keys, author, login) = (space.keys.clone(), space.author.clone(), space.login);
	let waiting = std::thread::spawn(move || {
		let mut rng = TestRng::new("the other thread");
		let statement = Statement::new(
			rng.statement_id(),
			login,
			field("org.example.note"),
			Value::Text("after the batch".into()),
		);
		other
			.commit(
				&author,
				&keys,
				vec![Operation::assert(statement)],
				start(),
				&mut rng,
			)
			.expect("commits once the batch is saved");
		other
	});
	std::thread::sleep(Duration::from_millis(300));
	one.end_batch().expect("ends");
	let other = waiting.join().expect("the other thread");
	assert_eq!(other.log().heads().expect("reads").len(), 1);
	assert_eq!(texts(&other, &note), ["after the batch", "in the batch"]);
}

#[test]
fn a_field_made_secret_by_another_is_sealed_here_from_then_on() {
	let mut space = OneSpace::new("made secret elsewhere");
	let pin = field("org.example.pin");
	let mut one = space.open();
	let mut other = space.open();
	// The other's types make the field secret.
	other
		.set_fields([password(), pin.clone()], [])
		.expect("makes it secret");
	let id = space.rng.statement_id();
	let statement = Statement::new(id, space.login, pin.clone(), Value::Text("1234".into()));
	let parcel = one
		.commit(
			&space.author,
			&space.keys,
			vec![Operation::assert(statement)],
			start(),
			&mut space.rng,
		)
		.expect("commits");
	assert!(parcel.change.sealed().contains_key(&id), "sealed");
	assert!(one.secret_fields().contains(&pin));
	// Without the secrets, the value cannot be read.
	let mut locked = space.keys.clone();
	locked.forget_secrets();
	drop((one, other));
	let without = StoredLog::open(
		&space.path,
		space.id,
		&locked,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[password(), pin.clone()],
	)
	.expect("opens");
	assert!(texts(&without, &pin).is_empty(), "only the secrets open it");
}

#[test]
fn a_purge_waits_for_the_others_to_close_the_space_which_they_are_told_to() {
	let mut space = OneSpace::new("a purge waits");
	let mut one = space.open();
	let mut other = space.open();
	let note = field("org.example.note");
	// Not a secret, whose record key alone goes: the change that holds it loses its payload,
	// which the state file holds, and is made anew without it.
	let purged = space.add(&mut one, note.clone(), "my address");
	let (keys, author) = (space.keys.clone(), space.author.clone());
	let purging = std::thread::spawn(move || {
		let mut rng = TestRng::new("the purging thread");
		one.purge(&author, &keys, &[purged], start() + 1_000, &mut rng)
			.expect("purges");
		one
	});
	// The other is told to close the space at its next change, and does.
	let mark = space.path.with_file_name("remake");
	let mut refused = None;
	for _ in 0..100 {
		std::thread::sleep(Duration::from_millis(20));
		let id = space.rng.statement_id();
		let statement = Statement::new(id, space.login, note.clone(), Value::Text("x".into()));
		match other.commit(
			&space.author,
			&space.keys,
			vec![Operation::assert(statement)],
			start(),
			&mut space.rng,
		) {
			Err(error @ StoreError::InUse) => {
				refused = Some(error);
				break;
			}
			result => assert!(result.is_ok(), "{result:?}"),
		}
	}
	assert!(refused.is_some(), "told to close the space");
	assert!(mark.exists());
	drop(other);
	let one = purging.join().expect("the purging thread");
	assert!(!mark.exists(), "the state file was made anew");
	assert!(!texts(&one, &note).contains(&"my address".to_owned()));
	drop(one);
	let reopened = space.open();
	assert!(!texts(&reopened, &note).contains(&"my address".to_owned()));
}

#[test]
fn a_space_whose_state_file_waits_to_be_made_anew_opens_only_alone() {
	let space = OneSpace::new("waits to be made anew");
	let one = space.open();
	let mark = space.path.with_file_name("remake");
	fs::write(&mark, b"").expect("marks");
	assert!(matches!(
		space.open_with(&[password()]),
		Err(StoreError::InUse)
	));
	drop(one);
	let alone = space.open();
	assert!(!mark.exists(), "opened alone, it made the state file anew");
	drop(alone);
}

#[test]
fn files_being_written_are_left_while_another_has_the_space_open() {
	let space = OneSpace::new("files being written");
	let one = space.open();
	let staging = space.path.with_file_name("tmp");
	fs::create_dir_all(&staging).expect("makes the folder");
	let written = staging.join("1-1.tmp");
	fs::write(&written, b"half a blob").expect("writes");
	let other = space.open();
	assert!(written.exists(), "the other may be writing it");
	drop((one, other));
	let alone = space.open();
	assert!(!written.exists(), "alone, it was a crash's");
	drop(alone);
}

#[test]
fn a_purge_that_waits_in_vain_is_finished_once_the_others_close_the_space() {
	let mut space = OneSpace::new("a purge waits in vain");
	let mut one = space.open();
	let other = space.open();
	let note = field("org.example.note");
	let purged = space.add(&mut one, note.clone(), "my address");
	// The other keeps the space open, making no change, for longer than the purge waits:
	// the purge is saved all the same, and the mark stays.
	one.purge(
		&space.author,
		&space.keys,
		&[purged],
		start() + 1_000,
		&mut space.rng,
	)
	.expect("purges");
	let mark = space.path.with_file_name("remake");
	assert!(mark.exists());
	let id = space.rng.statement_id();
	let statement = Statement::new(id, space.login, note.clone(), Value::Text("x".into()));
	let refused = one.commit(
		&space.author,
		&space.keys,
		vec![Operation::assert(statement.clone())],
		start() + 2_000,
		&mut space.rng,
	);
	assert!(matches!(refused, Err(StoreError::InUse)), "{refused:?}");
	// Once the other has closed it, the next change makes the state file anew, alone.
	drop(other);
	one.commit(
		&space.author,
		&space.keys,
		vec![Operation::assert(statement)],
		start() + 2_000,
		&mut space.rng,
	)
	.expect("commits");
	assert!(!mark.exists());
	assert_eq!(texts(&one, &note), ["x"]);
	drop(one);
	assert_eq!(texts(&space.open(), &note), ["x"]);
}
