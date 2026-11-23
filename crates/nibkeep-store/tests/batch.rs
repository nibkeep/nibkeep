//! Batches of changes, saved together ([technical design: saving in batches]): a batch saves exactly what
//! saving its changes one at a time saves, a change refused in it leaves the others, and a
//! batch that does not end, or whose files fail, saves none of its changes.
//!
//! [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches

use std::fs;
use std::path::PathBuf;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::SigningKey;
use nibkeep_log::{Log, LogError, SpaceKeys};
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoreError, StoredLog, log_path};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const DRIFT: u64 = 3_600_000;

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn note() -> FieldName {
	field("org.example.note")
}

fn password() -> FieldName {
	field("org.nibkeep.password")
}

/// A space on disk, and what writes to it.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	author: SigningKey,
	path: PathBuf,
	id: SpaceId,
	entity: EntityId,
	/// The time of the next change.
	wall: u64,
}

impl Space {
	/// A space whose randomness comes from `seed`, in a directory of its own: two spaces of
	/// one seed make the same changes.
	fn new(directory: &str, seed: &str) -> Self {
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		let author = rng.signing_key();
		let entity = rng.entity_id();
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), directory);
		Self {
			rng,
			keys,
			author,
			path: log_path(&directory, id),
			id,
			entity,
			wall: millis_at("2026-10-23", 12, 0),
		}
	}

	fn open(&self) -> StoredLog {
		StoredLog::open(
			&self.path,
			self.id,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[password()],
		)
		.expect("opens")
	}

	fn state_file(&self) -> PathBuf {
		self.path.with_file_name("state.sqlite")
	}

	/// Commits a note, or a secret value, with `keys`.
	fn try_commit(
		&mut self,
		stored: &mut StoredLog,
		keys: &SpaceKeys,
		value: usize,
		secret: bool,
	) -> Result<(), StoreError> {
		let field = if secret { password() } else { note() };
		let statement = Statement::new(
			self.rng.statement_id(),
			self.entity,
			field,
			Value::Text(format!("value {value}")),
		);
		self.wall += 1_000;
		stored
			.commit(
				&self.author,
				keys,
				vec![Operation::assert(statement)],
				self.wall,
				&mut self.rng,
			)
			.map(|_| ())
	}

	fn commit(&mut self, stored: &mut StoredLog, value: usize, secret: bool) {
		let keys = self.keys.clone();
		self.try_commit(stored, &keys, value, secret)
			.expect("commits");
	}
}

/// The current statements of a stored log's state.
fn current(stored: &StoredLog) -> Vec<Statement> {
	stored
		.log()
		.state()
		.statements()
		.expect("reads")
		.into_iter()
		.map(std::borrow::Cow::into_owned)
		.collect()
}

fn changes(stored: &StoredLog) -> usize {
	stored.log().changes().expect("reads").len()
}

/// The space's keys without its secrets key.
fn locked(keys: &SpaceKeys) -> SpaceKeys {
	SpaceKeys::new(0, keys.newest().1.clone())
}

#[test]
fn a_batch_saves_exactly_what_single_changes_save() {
	let mut single = Space::new("batch single", "batch or not");
	let mut batched = Space::new("batch batched", "batch or not");
	let mut one_by_one = single.open();
	let mut together = batched.open();
	together.begin_batch(&batched.keys).expect("begins a batch");
	assert!(together.in_batch());
	for value in 0..6 {
		single.commit(&mut one_by_one, value, value % 3 == 0);
		batched.commit(&mut together, value, value % 3 == 0);
	}
	together.end_batch().expect("ends the batch");
	assert!(!together.in_batch());
	assert_eq!(current(&together), current(&one_by_one));
	drop((one_by_one, together));

	let (one_by_one, together) = (single.open(), batched.open());
	assert_eq!(changes(&together), 6);
	assert_eq!(current(&together), current(&one_by_one));
	assert_eq!(
		together.log().parcels().expect("reads"),
		one_by_one.log().parcels().expect("reads"),
		"the same changes"
	);
	assert_eq!(
		together.search("value").expect("searches"),
		one_by_one.search("value").expect("searches")
	);
}

#[test]
fn changes_in_a_batch_are_read_before_it_ends() {
	let mut space = Space::new("batch read", "batch read");
	let mut stored = space.open();
	stored.begin_batch(&space.keys).expect("begins a batch");
	space.commit(&mut stored, 1, false);
	space.commit(&mut stored, 2, false);
	assert_eq!(changes(&stored), 2);
	assert_eq!(current(&stored).len(), 2);
	assert_eq!(stored.search("value 1").expect("searches").len(), 1);
	stored.end_batch().expect("ends the batch");
}

#[test]
fn a_batch_that_does_not_end_saves_none_of_its_changes() {
	let mut space = Space::new("batch dropped", "batch dropped");
	let mut stored = space.open();
	space.commit(&mut stored, 0, false);
	let before = current(&stored);
	stored.begin_batch(&space.keys).expect("begins a batch");
	for value in 1..4 {
		space.commit(&mut stored, value, value == 2);
	}
	drop(stored);

	let mut stored = space.open();
	assert_eq!(changes(&stored), 1);
	assert_eq!(current(&stored), before);
	nibkeep_testkit::assert_empty!(stored.search("value 3").expect("searches"));
	// The space goes on from where it was.
	space.commit(&mut stored, 4, false);
	assert_eq!(changes(&stored), 2);
}

#[test]
fn a_refused_change_leaves_the_rest_of_its_batch() {
	let mut space = Space::new("batch refused", "batch refused");
	let mut stored = space.open();
	stored.begin_batch(&space.keys).expect("begins a batch");
	space.commit(&mut stored, 1, false);
	// A secret value without the secrets key is refused.
	let locked = locked(&space.keys);
	assert!(matches!(
		space.try_commit(&mut stored, &locked, 2, true),
		Err(StoreError::Log(LogError::SecretsLocked))
	));
	assert!(stored.in_batch(), "the batch goes on");
	space.commit(&mut stored, 3, false);
	stored.end_batch().expect("ends the batch");
	let kept = current(&stored);
	assert_eq!(kept.len(), 2);
	drop(stored);

	let stored = space.open();
	assert_eq!(changes(&stored), 2);
	assert_eq!(current(&stored), kept);
}

#[test]
fn what_cannot_go_into_a_batch_is_refused() {
	let mut space = Space::new("batch refuses", "batch refuses");
	let mut stored = space.open();
	space.commit(&mut stored, 0, false);
	let statement = current(&stored)[0].id;
	stored.begin_batch(&space.keys).expect("begins a batch");
	space.commit(&mut stored, 1, false);
	assert!(matches!(
		stored.begin_batch(&space.keys),
		Err(StoreError::InBatch)
	));
	assert!(matches!(
		stored.purge(
			&space.author,
			&space.keys,
			&[statement],
			space.wall + 500,
			&mut space.rng
		),
		Err(StoreError::InBatch)
	));
	assert!(matches!(
		stored.import_blob(&space.keys, &b"a blob"[..], space.wall, &mut space.rng),
		Err(StoreError::InBatch)
	));
	assert!(matches!(
		stored.unlock_secrets(&space.keys),
		Err(StoreError::InBatch)
	));
	// A change from another device, which goes into the batch, as a sync saves it.
	let mut other = Log::new(space.id, Clock::new(DRIFT));
	let parcel = other
		.commit(
			&space.rng.signing_key(),
			&space.keys,
			vec![Operation::assert(Statement::new(
				space.rng.statement_id(),
				space.entity,
				note(),
				Value::Text("from elsewhere".into()),
			))],
			space.wall,
			&mut space.rng,
		)
		.expect("commits");
	stored
		.receive(&parcel, &space.keys, space.wall)
		.expect("receives in the batch");
	// The batch is as it was, and what was refused works once it ends.
	space.commit(&mut stored, 2, false);
	stored.end_batch().expect("ends the batch");
	assert_eq!(changes(&stored), 4);
	assert!(stored.end_batch().is_err(), "no batch is open");
	stored
		.purge(
			&space.author,
			&space.keys,
			&[statement],
			space.wall + 500,
			&mut space.rng,
		)
		.expect("purges");
	assert_eq!(changes(&stored), 5);
}

#[test]
fn the_fields_can_change_in_a_batch() {
	let mut space = Space::new("batch fields", "batch fields");
	let mut stored = space.open();
	space.commit(&mut stored, 1, false);
	stored.begin_batch(&space.keys).expect("begins a batch");
	// The note becomes a field whose last writer wins, which builds the indexes again.
	stored
		.set_fields([password()], [note()])
		.expect("changes the fields");
	space.commit(&mut stored, 2, false);
	stored.end_batch().expect("ends the batch");
	assert_eq!(stored.search("value").expect("searches").len(), 1);
	drop(stored);

	let stored = space.open();
	assert_eq!(changes(&stored), 2);
	assert_eq!(stored.last_writer_wins_fields().len(), 1);
	assert_eq!(stored.search("value").expect("searches").len(), 1);
}

/// Changes every byte of a state file after its first page, which opening reads: the
/// other pages no longer authenticate when they are read.
fn damage_after_the_first_page(space: &Space) {
	let path = space.state_file();
	let mut bytes = fs::read(&path).expect("reads the state file");
	assert!(bytes.len() > 16_384, "a state file of several pages");
	for byte in &mut bytes[8_192..] {
		*byte ^= 0x40;
	}
	fs::write(&path, bytes).expect("writes the state file");
}

#[test]
fn a_state_file_found_damaged_in_a_batch_loses_the_batch() {
	let mut space = Space::new("batch damaged", "batch damaged");
	let mut stored = space.open();
	for value in 0..40 {
		space.commit(&mut stored, value, false);
	}
	let before = current(&stored);
	drop(stored);
	let mut stored = space.open();
	stored.begin_batch(&space.keys).expect("begins a batch");
	// Damage found while the batch is open, as a disk may damage what it holds.
	damage_after_the_first_page(&space);
	let mut failed = None;
	for value in 40..80 {
		if let Err(error) = space.try_commit(&mut stored, &space.keys.clone(), value, false) {
			failed = Some(error);
			break;
		}
	}
	assert!(
		matches!(failed, Some(StoreError::State(_))),
		"the damage is found: {failed:?}"
	);
	assert!(matches!(
		space.try_commit(&mut stored, &space.keys.clone(), 80, false),
		Err(StoreError::BatchLost)
	));
	assert!(matches!(stored.end_batch(), Err(StoreError::BatchLost)));
	assert!(!stored.in_batch());
	assert_eq!(changes(&stored), 40, "none of the batch is kept");
	assert_eq!(current(&stored), before);
	// The state file was made again, and the space goes on.
	space.commit(&mut stored, 81, false);
	drop(stored);

	let stored = space.open();
	assert_eq!(changes(&stored), 41);
	assert_eq!(current(&stored).len(), 41);
}
