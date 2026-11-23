//! Changes of a burst, which the state file takes in together ([technical design: the state file]): they
//! are read at once, commit when the burst ends or the space closes, and are not read again
//! when the space is opened; a change refused in a burst leaves the others; a change made
//! on its own commits the state file at once. The time that tells them apart is a clock of
//! the tests' own, which moves only when they move it, so that every machine sees the same
//! bursts.
//!
//! [technical design: the state file]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#75-the-state-file

use std::path::PathBuf;
use std::time::Duration;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::SigningKey;
use nibkeep_log::{LogError, SpaceKeys};
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoreError, StoredLog, Ticks, log_path};
use nibkeep_testkit::{ManualClock, TestRng, millis_at};

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
	/// The clock that tells bursts apart.
	clock: ManualClock,
}

impl Space {
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let id = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		let author = rng.signing_key();
		let entity = rng.entity_id();
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		Self {
			rng,
			keys,
			author,
			path: log_path(&directory, id),
			id,
			entity,
			wall: millis_at("2026-10-24", 12, 0),
			clock: ManualClock::new(),
		}
	}

	fn open(&self) -> StoredLog {
		let mut stored = StoredLog::open(
			&self.path,
			self.id,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[password()],
		)
		.expect("opens");
		let clock = self.clock.clone();
		stored.set_ticks(Ticks::from_fn(move || clock.now()));
		stored
	}

	/// Lets this much time go by.
	fn wait(&self, millis: u64) {
		self.clock.advance(Duration::from_millis(millis));
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

	fn commit(&mut self, stored: &mut StoredLog, value: usize) {
		let keys = self.keys.clone();
		self.try_commit(stored, &keys, value, false)
			.expect("commits");
	}
}

/// How many current statements a stored log's state has.
fn current(stored: &StoredLog) -> usize {
	stored.log().state().statements().expect("reads").len()
}

fn changes(stored: &StoredLog) -> usize {
	stored.log().changes().expect("reads").len()
}

/// Commits `values` as notes in a row, a millisecond apart: a burst.
fn burst(space: &mut Space, stored: &mut StoredLog, values: std::ops::Range<usize>) {
	for value in values {
		space.wait(1);
		space.commit(stored, value);
	}
}

/// Opens the space again, and checks that it holds `expected` changes, every one of them in
/// the state, and that opening read none of them again from the log file: the state file
/// had them all when the space closed.
fn reopened(space: &Space, expected: usize) -> StoredLog {
	let stored = space.open();
	assert_eq!(
		stored.log().history().changes_read(),
		0,
		"the state file was committed before the space closed"
	);
	assert_eq!(changes(&stored), expected);
	assert_eq!(current(&stored), expected);
	stored
}

#[test]
fn changes_of_a_burst_are_read_at_once_and_committed_when_the_space_closes() {
	let mut space = Space::new("burst closes");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..30);
	assert_eq!(current(&stored), 30, "read at once");
	assert_eq!(stored.search("value").expect("searches").len(), 1);
	// The first change, which no change came before, committed alone; the others wait in
	// one transaction of the state file.
	assert_eq!(stored.pending_changes(), 29);
	drop(stored);
	let mut stored = reopened(&space, 30);

	// And again, after more changes.
	for value in 30..40 {
		space.commit(&mut stored, value);
	}
	drop(stored);
	reopened(&space, 40);
}

#[test]
fn a_change_refused_in_a_burst_leaves_the_others() {
	let mut space = Space::new("burst refused");
	let mut stored = space.open();
	for value in 0..5 {
		space.commit(&mut stored, value);
	}
	// A secret value without the secrets key is refused.
	let locked = SpaceKeys::new(0, space.keys.newest().1.clone());
	assert!(matches!(
		space.try_commit(&mut stored, &locked, 5, true),
		Err(StoreError::Log(LogError::SecretsLocked))
	));
	assert_eq!(current(&stored), 5, "the burst goes on");
	for value in 6..11 {
		space.commit(&mut stored, value);
	}
	assert_eq!(changes(&stored), 10);
	drop(stored);
	reopened(&space, 10);
}

#[test]
fn a_change_made_on_its_own_commits_the_state_file_at_once() {
	let mut space = Space::new("alone commits");
	let mut stored = space.open();
	space.commit(&mut stored, 0);
	assert_eq!(stored.pending_changes(), 0, "the first change of the space");
	space.wait(150);
	space.commit(&mut stored, 1);
	assert_eq!(stored.pending_changes(), 0, "a change later than a burst's");
	drop(stored);
	reopened(&space, 2);
}

#[test]
fn a_burst_that_stopped_commits_when_the_space_is_used_again() {
	let mut space = Space::new("burst stopped");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..5);
	assert_eq!(stored.pending_changes(), 4);
	space.wait(1_100);
	// Reading the log is using the space.
	assert_eq!(changes(&stored), 5);
	assert_eq!(stored.pending_changes(), 0, "committed after a second");
	drop(stored);
	reopened(&space, 5);
}

#[test]
fn ending_a_burst_commits_it_at_once_for_another_program() {
	let mut space = Space::new("burst ended");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..3);
	assert_eq!(stored.pending_changes(), 2);
	stored.end_burst();
	assert_eq!(stored.pending_changes(), 0, "committed when ended");
	// Another program opens the space at once, and reads every change.
	reopened(&space, 3);
	assert_eq!(changes(&stored), 3);
}

#[test]
fn a_read_keeps_a_burst_pending_and_settles_one_that_stopped() {
	let mut space = Space::new("read settles");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..3);
	stored
		.reading(|stored| stored.log().state().statements().map(|_| ()))
		.expect("reads");
	assert_eq!(
		stored.pending_changes(),
		2,
		"a read within a second keeps the burst"
	);
	space.wait(1_100);
	stored
		.reading(|stored| {
			assert_eq!(stored.pending_changes(), 0, "committed before the read");
			stored.log().state().statements().map(|_| ())
		})
		.expect("reads");
	assert_eq!(changes(&stored), 3);
	drop(stored);
	reopened(&space, 3);
}

#[test]
fn the_fields_can_change_in_a_burst() {
	let mut space = Space::new("burst fields");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..3);
	assert_eq!(stored.pending_changes(), 2);
	// The note becomes a field whose last writer wins, which builds the indexes again, in the
	// transaction of the burst if one is open.
	stored
		.set_fields([password()], [note()])
		.expect("changes the fields");
	assert_eq!(stored.search("value").expect("searches").len(), 1);
	for value in 3..6 {
		space.commit(&mut stored, value);
	}
	assert_eq!(stored.search("value 5").expect("searches").len(), 1);
	assert_eq!(stored.search("value 4").expect("searches").len(), 0);
	drop(stored);
	let stored = reopened(&space, 6);
	assert_eq!(stored.last_writer_wins_fields().len(), 1);
	assert_eq!(stored.search("value 5").expect("searches").len(), 1);
}

/// Changes every byte of the state file after its first page, which opening reads, and of
/// its write-ahead log after its first record, which holds the pages of the transaction
/// that is open: the pages no longer authenticate when they are read.
fn damage(space: &Space) {
	let mut damaged = 0;
	for (name, keep) in [("state.sqlite", 8_192), ("state.sqlite-wal", 4_096)] {
		let path = space.path.with_file_name(name);
		let mut bytes = std::fs::read(&path).expect("reads the file");
		for byte in bytes.iter_mut().skip(keep) {
			*byte ^= 0x40;
			damaged += 1;
		}
		std::fs::write(&path, bytes).expect("writes the file");
	}
	assert!(damaged > 65_536, "pages to damage");
}

#[test]
fn a_state_file_found_damaged_in_a_burst_keeps_every_change_saved() {
	let mut space = Space::new("burst damaged");
	let mut stored = space.open();
	// Long notes, 6.4 MB in all, saved before the burst and read by no connection since:
	// reading them reads the disk, where the damage is, whatever SQLite keeps in memory.
	let long = "word ".repeat(64_000);
	for value in 0..20 {
		let statement = Statement::new(
			space.rng.statement_id(),
			space.entity,
			note(),
			Value::Text(format!("{long}{value}")),
		);
		space.wait(60_000);
		space.wall += 60_000;
		stored
			.commit(
				&space.author,
				&space.keys,
				vec![Operation::assert(statement)],
				space.wall,
				&mut space.rng,
			)
			.expect("commits");
	}
	drop(stored);
	let mut stored = space.open();
	burst(&mut space, &mut stored, 20..120);
	let pending = stored.pending_changes();
	assert!(pending > 0, "a burst is under way");
	damage(&space);
	// A read finds the damage, which ends the transaction that held the changes of the
	// burst; the log file holds them all the same, and the state file is made again from it.
	let mut found = matches!(stored.log().state().statements(), Err(StoreError::State(_)));
	for value in 120..130 {
		match space.try_commit(&mut stored, &space.keys.clone(), value, false) {
			Ok(()) => {}
			Err(StoreError::State(_)) => found = true,
			Err(error) => panic!("commits: {error}"),
		}
	}
	assert!(found, "the damage is found, with {pending} changes pending");
	let saved = changes(&stored);
	assert!(saved >= 120);
	assert_eq!(current(&stored), saved);
	drop(stored);
	reopened(&space, saved);
}

#[test]
fn blobs_saved_in_a_burst_wait_with_it() {
	let mut space = Space::new("burst blobs");
	let mut stored = space.open();
	let mut blobs = Vec::new();
	// As adding files does: a blob, then the change that records it, for each file.
	for value in 0..10 {
		space.wait(1);
		let keys = space.keys.clone();
		let content = format!("blob {value}");
		blobs.push(
			stored
				.import_blob(&keys, content.as_bytes(), space.wall, &mut space.rng)
				.expect("imports"),
		);
		space.commit(&mut stored, value);
	}
	assert_eq!(
		stored.pending_changes(),
		9,
		"a blob commits no burst, and is no change"
	);
	drop(stored);
	let stored = reopened(&space, 10);
	for blob in &blobs {
		assert!(stored.holds_blob(blob));
	}
}

#[test]
fn a_blob_saved_after_a_burst_waited_commits_it() {
	let mut space = Space::new("burst blob commits");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..3);
	assert_eq!(stored.pending_changes(), 2);
	space.wait(1_100);
	let keys = space.keys.clone();
	stored
		.import_blob(&keys, &b"a blob"[..], space.wall, &mut space.rng)
		.expect("imports");
	assert_eq!(stored.pending_changes(), 0, "saving is using the space");
	drop(stored);
	reopened(&space, 3);
}

#[test]
fn a_burst_commits_at_a_thousand_changes() {
	let mut space = Space::new("burst thousand");
	let mut stored = space.open();
	// The first change commits alone; the next thousand, with the clock standing still,
	// fill a burst, which commits with the last of them.
	for value in 0..1_001 {
		space.commit(&mut stored, value);
	}
	assert_eq!(stored.pending_changes(), 0);
	burst(&mut space, &mut stored, 1_001..1_003);
	assert_eq!(stored.pending_changes(), 2);
	drop(stored);
	reopened(&space, 1_003);
}

#[test]
fn a_burst_that_goes_on_commits_after_a_second() {
	let mut space = Space::new("burst goes on");
	let mut stored = space.open();
	space.commit(&mut stored, 0);
	// Each change within a tenth of a second of the one before, for longer than a second
	// after the first change that waits, at 90 ms.
	for value in 1..=12 {
		space.wait(90);
		space.commit(&mut stored, value);
		assert_eq!(
			stored.pending_changes(),
			value,
			"pending after change {value}"
		);
	}
	space.wait(90);
	space.commit(&mut stored, 13);
	assert_eq!(
		stored.pending_changes(),
		0,
		"a second after the first of them"
	);
	drop(stored);
	reopened(&space, 14);
}

#[test]
fn a_new_clock_begins_anew() {
	let mut space = Space::new("burst new clock");
	let mut stored = space.open();
	burst(&mut space, &mut stored, 0..3);
	assert_eq!(stored.pending_changes(), 2);
	// Another clock, whose times are not compared with the first one's.
	let other = ManualClock::new();
	let clock = other.clone();
	stored.set_ticks(Ticks::from_fn(move || clock.now()));
	space.commit(&mut stored, 3);
	assert_eq!(
		stored.pending_changes(),
		0,
		"the next change commits the burst"
	);
	other.advance(Duration::from_millis(1));
	space.commit(&mut stored, 4);
	assert_eq!(
		stored.pending_changes(),
		1,
		"and bursts form by the new clock"
	);
	drop(stored);
	reopened(&space, 5);
}
