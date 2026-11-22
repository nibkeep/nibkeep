//! A space's state in its state file: it always answers as the state built again from the
//! log file, whatever happened to the state file, and holds no secret value ([the technical design's
//! section on the state file]).
//!
//! [technical design: the state file]: ../../../docs/Technical-design.md#75-the-state-file

use std::borrow::Cow;
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::SigningKey;
use nibkeep_log::{History, Log, LogError, Parcel, SpaceKeys};
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_store::{StoreError, StoredLog, Ticks, log_path};
use nibkeep_testkit::{ManualClock, TestRng, millis_at};
use nibkeep_time::Clock;
use proptest::prelude::*;

const DRIFT: u64 = 3_600_000;

fn start() -> u64 {
	millis_at("2026-10-20", 12, 0)
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
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
	entities: Vec<EntityId>,
	/// The time of the next change.
	wall: u64,
	/// Another device, which writes to the space too.
	other: (SigningKey, Log),
	/// The payloads and keys of the other device's changes that this device has not
	/// received yet, each alone in a parcel with its change.
	pending: Vec<Parcel>,
	/// The clock that tells bursts of changes apart, which only the steps move, so that a
	/// case forms the same bursts on every machine.
	clock: ManualClock,
	/// Whether the test damaged the state file since it was last made: only then may a read
	/// find it damaged.
	tampered: Cell<bool>,
	/// Whether the state file kept by [`Step::Keep`] was damaged.
	kept_tampered: bool,
}

impl Space {
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let id = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		let author = rng.signing_key();
		let entities = (0..3).map(|_| rng.entity_id()).collect();
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		let other = (rng.signing_key(), Log::new(id, Clock::new(DRIFT)));
		Self {
			other,
			pending: Vec::new(),
			rng,
			keys,
			author,
			path: log_path(&directory, id),
			id,
			entities,
			wall: start(),
			clock: ManualClock::new(),
			tampered: Cell::new(false),
			kept_tampered: false,
		}
	}

	fn open(&self) -> Result<StoredLog, StoreError> {
		let mut stored = StoredLog::open(
			&self.path,
			self.id,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[password()],
		)?;
		let clock = self.clock.clone();
		stored.set_ticks(Ticks::from_fn(move || clock.now()));
		Ok(stored)
	}

	fn state_file(&self) -> PathBuf {
		self.path.with_file_name("state.sqlite")
	}

	/// Commits a value of an entity, secret or not, replacing what the field showed.
	fn commit(&mut self, stored: &mut StoredLog, entity: usize, value: u16, secret: bool) {
		self.try_commit(stored, entity, value, secret)
			.expect("commits, also when it finds the state file damaged");
	}

	/// Commits as [`Space::commit`] does, and tells whether it failed.
	fn try_commit(
		&mut self,
		stored: &mut StoredLog,
		entity: usize,
		value: u16,
		secret: bool,
	) -> Result<(), StoreError> {
		let entity = self.entities[entity % self.entities.len()];
		let field = if secret {
			password()
		} else {
			field("org.example.note")
		};
		// Damage that opening did not read may be found here; the commit then makes the
		// state file again.
		let current: Vec<_> = match stored.log().state().current(entity, &field) {
			Ok(current) => current.iter().map(|statement| statement.id).collect(),
			Err(StoreError::State(_)) => Vec::new(),
			Err(error) => panic!("reads: {error}"),
		};
		let statement = Statement::new(
			self.rng.statement_id(),
			entity,
			field,
			Value::Text(format!("value {value}")),
		);
		self.wall += 1_000;
		stored
			.commit(
				&self.author,
				&self.keys,
				vec![Operation::replace(statement, current)],
				self.wall,
				&mut self.rng,
			)
			.map(|_| ())
	}

	/// Commits values in a batch, and ends it, or else closes the space with the batch open.
	/// A batch that finds the state file damaged fails whole. Returns the space, open.
	fn batch(
		&mut self,
		mut stored: StoredLog,
		values: &[(usize, u16, bool)],
		ends: bool,
	) -> Result<StoredLog, TestCaseError> {
		let before = stored.log().changes().expect("reads").len();
		// Bringing the indexes up to date may find damage that opening did not read; the
		// batch then begins once the state file is made again.
		if let Err(error) = stored.begin_batch(&self.keys) {
			assert!(
				matches!(error, StoreError::State(_)) && self.tampered.get(),
				"begins a batch: {error}"
			);
			stored
				.begin_batch(&self.keys)
				.expect("begins a batch once the state file is made again");
		}
		let mut lost = false;
		for &(entity, value, secret) in values {
			match self.try_commit(&mut stored, entity, value, secret) {
				Ok(()) => prop_assert!(!lost, "a change after the batch was lost"),
				Err(StoreError::State(_) | StoreError::BatchLost) => lost = true,
				Err(error) => panic!("commits in a batch: {error}"),
			}
		}
		if !ends {
			drop(stored);
			let stored = self.open().expect("opens again");
			prop_assert_eq!(stored.log().changes().expect("reads").len(), before);
			return Ok(stored);
		}
		match stored.end_batch() {
			Ok(()) => {
				prop_assert!(!lost, "a lost batch ended");
				prop_assert_eq!(
					stored.log().changes().expect("reads").len(),
					before + values.len()
				);
			}
			Err(StoreError::BatchLost) => {
				prop_assert!(lost, "a batch lost at its end");
				prop_assert_eq!(stored.log().changes().expect("reads").len(), before);
			}
			Err(error) => panic!("ends the batch: {error}"),
		}
		Ok(stored)
	}
}

impl Space {
	/// The other device writes a value, and this device receives the change alone; its
	/// payload and key wait to be delivered.
	fn arrive(&mut self, stored: &mut StoredLog, entity: usize, value: u16) {
		let entity = self.entities[entity % self.entities.len()];
		let statement = Statement::new(
			self.rng.statement_id(),
			entity,
			field("org.example.note"),
			Value::Text(format!("value {value}")),
		);
		self.wall += 1_000;
		let (author, log) = &mut self.other;
		let parcel = log
			.commit(
				author,
				&self.keys,
				vec![Operation::assert(statement)],
				self.wall,
				&mut self.rng,
			)
			.expect("commits");
		let alone = Parcel {
			payload: None,
			keys: Vec::new(),
			..parcel.clone()
		};
		self.receive(stored, &alone);
		self.pending.push(Parcel {
			keys: Vec::new(),
			..parcel.clone()
		});
		self.pending.push(Parcel {
			payload: None,
			..parcel
		});
	}

	/// Purges one of the statements that the state shows, if it shows any.
	fn purge(&mut self, stored: &mut StoredLog, which: usize) {
		let statements = match current(stored.log()) {
			Ok(statements) => statements,
			// Damage that opening did not read; the purge then finds it too.
			Err(StoreError::State(_)) => return,
			Err(error) => panic!("reads: {error}"),
		};
		if statements.is_empty() {
			return;
		}
		let statement = statements[which % statements.len()].id;
		self.wall += 1_000;
		stored
			.purge(
				&self.author,
				&self.keys,
				&[statement],
				self.wall,
				&mut self.rng,
			)
			.expect("purges, also when it finds the state file damaged");
		assert_eq!(
			stored.log().state().statement(statement).expect("reads"),
			None
		);
	}

	/// Delivers one of the payloads and keys that wait. A device whose log file lost the
	/// change gets it again with the part.
	fn deliver(&mut self, stored: &mut StoredLog, which: usize) {
		if self.pending.is_empty() {
			return;
		}
		let part = self.pending.remove(which % self.pending.len());
		let id = part.id();
		let delivered = match (&part.payload, part.keys.first()) {
			(Some(payload), _) => stored.receive_payload(id, payload, &self.keys),
			(None, Some(key)) => stored.receive_change_key(id, key, &self.keys),
			(None, None) => Ok(()),
		};
		match delivered {
			Err(StoreError::Log(LogError::UnknownChange)) => self.receive(stored, &part),
			Err(StoreError::State(_)) => {
				// The state file turned out damaged, and was made again; the part is sent
				// again.
				self.pending.push(part);
			}
			delivered => delivered.expect("keeps the part"),
		}
	}

	/// The other device writes values, whole, and purges one of the statements it shows, if
	/// told to and it shows any; this device receives them in a batch, as a sync does, which
	/// ends unless told not to, and then closes the space with the batch open. A batch that
	/// finds the state file damaged fails whole. Returns the space, open.
	fn receive_batch(
		&mut self,
		mut stored: StoredLog,
		(values, purge): (&[(usize, u16)], Option<usize>),
		ends: bool,
	) -> Result<StoredLog, TestCaseError> {
		let mut parcels = Vec::new();
		for &(entity, value) in values {
			let entity = self.entities[entity % self.entities.len()];
			let statement = Statement::new(
				self.rng.statement_id(),
				entity,
				field("org.example.note"),
				Value::Text(format!("value {value}")),
			);
			self.wall += 1_000;
			let (author, log) = &mut self.other;
			parcels.push(
				log.commit(
					author,
					&self.keys,
					vec![Operation::assert(statement)],
					self.wall,
					&mut self.rng,
				)
				.expect("commits"),
			);
		}
		let (author, log) = &mut self.other;
		let shown = current(log).expect("reads");
		if let Some(which) = purge
			&& !shown.is_empty()
		{
			self.wall += 1_000;
			let statement = shown[which % shown.len()].id;
			parcels.push(
				log.purge(author, &self.keys, [statement], self.wall, &mut self.rng)
					.expect("purges"),
			);
		}
		// What the other device made before, which a log file put back lacks, comes first.
		let mut missing: Vec<Parcel> = log
			.parcels()
			.expect("reads")
			.into_iter()
			.filter(|other| !stored.log().contains(other.id()).expect("reads"))
			.filter(|other| !parcels.iter().any(|parcel| parcel.id() == other.id()))
			.collect();
		missing.extend(parcels);
		self.begin(&mut stored);
		let mut before = stored.log().changes().expect("reads").len();
		let mut lost = false;
		for parcel in &missing {
			match stored.receive(parcel, &self.keys, self.wall) {
				Ok(_) => prop_assert!(!lost, "a change after the batch was lost"),
				Err(StoreError::State(_) | StoreError::BatchLost) => lost = true,
				// One that waiting changes wait for comes alone, as a sync receives it.
				Err(StoreError::InBatch) => {
					match stored.end_batch() {
						Ok(()) => prop_assert!(!lost, "a lost batch ended"),
						Err(StoreError::BatchLost) => prop_assert!(lost, "a batch lost at its end"),
						Err(error) => panic!("ends the batch: {error}"),
					}
					lost = false;
					match stored.receive(parcel, &self.keys, self.wall) {
						Ok(_) | Err(StoreError::State(_)) => {}
						Err(error) => panic!("receives alone: {error}"),
					}
					self.begin(&mut stored);
					before = stored.log().changes().expect("reads").len();
				}
				Err(error) => panic!("receives in a batch: {error}"),
			}
		}
		if !ends {
			drop(stored);
			let stored = self.open().expect("opens again");
			prop_assert_eq!(stored.log().changes().expect("reads").len(), before);
			return Ok(stored);
		}
		match stored.end_batch() {
			Ok(()) => prop_assert!(!lost, "a lost batch ended"),
			Err(StoreError::BatchLost) => prop_assert!(lost, "a batch lost at its end"),
			Err(error) => panic!("ends the batch: {error}"),
		}
		Ok(stored)
	}

	/// Begins a batch. Bringing the indexes up to date may find damage that opening did not
	/// read; the batch then begins once the state file is made again.
	fn begin(&self, stored: &mut StoredLog) {
		if let Err(error) = stored.begin_batch(&self.keys) {
			assert!(
				matches!(error, StoreError::State(_)) && self.tampered.get(),
				"begins a batch: {error}"
			);
			stored
				.begin_batch(&self.keys)
				.expect("begins a batch once the state file is made again");
		}
	}

	/// Receives a change that the other device made, with what parts come with it. Its
	/// parents may be gone from a log file put back: they come again first.
	fn receive(&self, stored: &mut StoredLog, parcel: &Parcel) {
		let missing: Vec<Parcel> = self
			.other
			.1
			.parcels()
			.expect("reads")
			.into_iter()
			.filter(|other| !stored.log().contains(other.id()).expect("reads"))
			.map(|other| Parcel {
				payload: None,
				keys: Vec::new(),
				..other
			})
			.collect();
		for change in missing.iter().chain([parcel]) {
			match stored.receive(change, &self.keys, self.wall) {
				Ok(_) | Err(StoreError::State(_)) => {}
				Err(error) => panic!("receives: {error}"),
			}
		}
	}
}

/// The current statements of a log's state.
fn current<S: Statements, H: History<Error = S::Error>>(
	log: &Log<S, H>,
) -> Result<Vec<Statement>, S::Error> {
	Ok(log
		.state()
		.statements()?
		.into_iter()
		.map(Cow::into_owned)
		.collect())
}

/// The state that the log's changes build in memory, with the secrets that `keys` open.
fn rebuilt(stored: &StoredLog, space: &Space) -> Vec<Statement> {
	let mut log = Log::new(space.id, Clock::new(DRIFT));
	log.set_secret_fields([password()]);
	for parcel in stored.log().parcels().expect("reads") {
		log.restore(parcel, &space.keys, false)
			.expect("takes the change");
	}
	for (statement, wrapped) in stored.log().wrapped_keys() {
		log.receive_wrapped_key(statement, wrapped.clone(), &space.keys)
			.expect("keeps the key");
	}
	current(&log).expect("reads")
}

/// What `read` finds in the state, which may find damage in a page that opening did not
/// read, but only if the test damaged the state file since it was last made. The store must
/// then have noted the damage, which deletes the file as the space closes; and the space,
/// opened again, must have made the file again, in which every read succeeds.
fn reading<T>(
	space: &Space,
	stored: StoredLog,
	read: impl Fn(&StoredLog) -> Result<T, StoreError>,
) -> (StoredLog, T) {
	match read(&stored) {
		Ok(found) => (stored, found),
		Err(StoreError::State(error)) if space.tampered.get() => {
			assert!(
				stored.log().state().storage().ordinary().is_damaged(),
				"the damage is noted: {error}"
			);
			drop(stored);
			let stored = space.open().expect("opens again");
			space.tampered.set(false);
			let found = read(&stored).expect("reads the state file made again");
			(stored, found)
		}
		Err(error) => panic!("reads: {error}"),
	}
}

/// The IDs of every statement that the state file records, current or not.
fn recorded(stored: &StoredLog) -> Result<Vec<StatementId>, StoreError> {
	let recorded = stored.log().state().storage().ordinary().all()?;
	Ok(recorded
		.iter()
		.map(|recorded| recorded.statement.id)
		.collect())
}

/// Checks that the stored state answers as the state built again from the log, and holds
/// no secret statement in its state file, through reads that may find damage as
/// [`reading`] says.
fn check(space: &Space, stored: StoredLog) -> Result<StoredLog, TestCaseError> {
	// A damaged state file answers right, or not at all.
	let (stored, shown) = reading(space, stored, |stored| current(stored.log()));
	prop_assert_eq!(shown, rebuilt(&stored, space));
	// Every statement it records, which may be in pages that the state shown is not.
	let (stored, recorded) = reading(space, stored, recorded);
	// Opened again or not, it answers as the log.
	prop_assert_eq!(
		current(stored.log()).expect("reads"),
		rebuilt(&stored, space)
	);
	for id in recorded {
		// Sealed or not, as the log file says.
		prop_assert!(
			!stored.log().is_sealed(id).expect("reads"),
			"a secret statement in the state file"
		);
	}
	Ok(stored)
}

/// The results of searches, through reads that may find damage as [`reading`] says.
fn search_all(
	space: &Space,
	stored: StoredLog,
	queries: &[&str],
) -> (StoredLog, Vec<Vec<nibkeep_store::Found>>) {
	reading(space, stored, |stored| {
		queries.iter().map(|query| stored.search(query)).collect()
	})
}

/// What happens to a space.
#[derive(Clone, Debug)]
enum Step {
	/// Commits a value of an entity, secret or not.
	Commit(usize, u16, bool),
	/// Commits values in a batch, and ends it unless told not to.
	Batch(Vec<(usize, u16, bool)>, bool),
	/// Receives a change from another device, without its payload and key.
	Arrive(usize, u16),
	/// Receives changes from another device in a batch, the last of which purges if told
	/// to, and ends the batch unless told not to.
	Receive(Vec<(usize, u16)>, Option<usize>, bool),
	/// Receives one of the payloads and keys that wait.
	Deliver(usize),
	/// Purges one of the statements, secret or not.
	Purge(usize),
	/// Lets this many milliseconds go by: changes less than a tenth of a second apart form
	/// a burst, and a burst that waited a second commits.
	Wait(u16),
	/// Closes the space and opens it again.
	Reopen,
	/// Keeps a copy of the log file and the state file, closed.
	Keep,
	/// Puts back the state file that was kept: it is behind the log.
	StateBehind,
	/// Puts back the log file that was kept: the state file is ahead of it.
	LogBehind,
	/// Deletes the state file.
	Delete,
	/// Cuts the state file short.
	Cut(usize),
	/// Changes a byte of the state file.
	Flip(usize),
}

fn step() -> impl Strategy<Value = Step> {
	prop_oneof![
		6 => (0..3_usize, any::<u16>(), any::<bool>())
			.prop_map(|(entity, value, secret)| Step::Commit(entity, value, secret)),
		2 => (
			prop::collection::vec((0..3_usize, any::<u16>(), any::<bool>()), 1..5),
			any::<bool>(),
		)
			.prop_map(|(values, ends)| Step::Batch(values, ends)),
		2 => (0..3_usize, any::<u16>()).prop_map(|(entity, value)| Step::Arrive(entity, value)),
		2 => (
			prop::collection::vec((0..3_usize, any::<u16>()), 0..4),
			prop::option::of(any::<usize>()),
			any::<bool>(),
		)
			.prop_map(|(values, purge, ends)| Step::Receive(values, purge, ends)),
		3 => any::<usize>().prop_map(Step::Deliver),
		1 => any::<usize>().prop_map(Step::Purge),
		3 => prop_oneof![Just(10_u16), Just(150), Just(1_200)].prop_map(Step::Wait),
		1 => Just(Step::Reopen),
		1 => Just(Step::Keep),
		1 => Just(Step::StateBehind),
		1 => Just(Step::LogBehind),
		1 => Just(Step::Delete),
		1 => (0..20_000_usize).prop_map(Step::Cut),
		1 => any::<usize>().prop_map(Step::Flip),
	]
}

/// Copies a file, or deletes the copy if there is none.
fn copy(from: &Path, to: &Path) {
	if from.exists() {
		fs::copy(from, to).expect("copies");
	} else if to.exists() {
		fs::remove_file(to).expect("deletes");
	}
}

/// Each test case gets a space of its own.
static CASES: AtomicUsize = AtomicUsize::new(0);

proptest! {
	#![proptest_config(ProptestConfig::with_cases(48))]

	#[test]
	fn the_state_file_answers_as_the_log_whatever_happens_to_it(
		steps in prop::collection::vec(step(), 1..24),
	) {
		let case = CASES.fetch_add(1, Ordering::Relaxed);
		let mut space = Space::new(&format!("whatever happens {case}"));
		let kept_log = space.path.with_file_name("kept-log");
		let kept_state = space.path.with_file_name("kept-state");
		let mut stored = Some(space.open().expect("opens"));
		for step in steps {
			match step {
				Step::Commit(entity, value, secret) => {
					let open = stored.as_mut().expect("open");
					space.commit(open, entity, value, secret);
					continue;
				}
				Step::Batch(values, ends) => {
					let open = stored.take().expect("open");
					stored = Some(space.batch(open, &values, ends)?);
					continue;
				}
				Step::Arrive(entity, value) => {
					let open = stored.as_mut().expect("open");
					space.arrive(open, entity, value);
					continue;
				}
				Step::Receive(values, purge, ends) => {
					let open = stored.take().expect("open");
					stored = Some(space.receive_batch(open, (&values, purge), ends)?);
					continue;
				}
				Step::Deliver(which) => {
					let open = stored.as_mut().expect("open");
					space.deliver(open, which);
					continue;
				}
				Step::Purge(which) => {
					let open = stored.as_mut().expect("open");
					space.purge(open, which);
					continue;
				}
				Step::Wait(millis) => {
					space.clock.advance(Duration::from_millis(millis.into()));
					continue;
				}
				_ => {}
			}
			// Everything else happens to closed files.
			if let Some(open) = stored.take() {
				drop(open);
			}
			let state = space.state_file();
			match step {
				Step::Commit(..)
				| Step::Batch(..)
				| Step::Arrive(..)
				| Step::Receive(..)
				| Step::Deliver(_)
				| Step::Purge(_)
				| Step::Wait(_)
				| Step::Reopen => {}
				Step::Keep => {
					copy(&space.path, &kept_log);
					copy(&state, &kept_state);
					space.kept_tampered = space.tampered.get();
				}
				Step::StateBehind => {
					if kept_state.exists() {
						copy(&kept_state, &state);
						space.tampered.set(space.kept_tampered);
					}
				}
				Step::LogBehind => {
					if kept_log.exists() {
						copy(&kept_log, &space.path);
					}
				}
				Step::Delete => {
					let _ = fs::remove_file(&state);
					space.tampered.set(false);
				}
				Step::Cut(length) => {
					if let Ok(file) = fs::OpenOptions::new().write(true).open(&state) {
						let size = file.metadata().expect("its size").len();
						file.set_len(size.min(length as u64)).expect("cuts");
						space.tampered.set(true);
					}
				}
				Step::Flip(at) => {
					if let Ok(mut bytes) = fs::read(&state)
						&& !bytes.is_empty()
					{
						let at = at % bytes.len();
						bytes[at] ^= 0x40;
						fs::write(&state, bytes).expect("writes");
						space.tampered.set(true);
					}
				}
			}
			let opened = space.open().expect("opens whatever happened to the state file");
			stored = Some(check(&space, opened)?);
		}
		let opened = stored.take().expect("open");
		let opened = check(&space, opened)?;
		drop(opened);
		let opened = check(&space, space.open().expect("opens again"))?;

		// The indexes, kept up to date, answer as indexes built anew.
		let queries = ["value", "va", "1", "value 2", "hunter"];
		let (opened, kept) = search_all(&space, opened, &queries);
		drop(opened);
		fs::remove_file(space.state_file()).expect("deletes the state file");
		let rebuilt = space.open().expect("opens again");
		for (query, kept) in queries.iter().zip(kept) {
			prop_assert_eq!(rebuilt.search(query).expect("searches"), kept, "{}", query);
		}
	}
}

/// The size of the state file's pages, each of which authenticates on its own.
const PAGE: usize = 4096;

#[test]
fn damage_in_any_page_after_a_purge_is_found_by_whichever_read_reaches_it() {
	let mut space = Space::new("damage after a purge");
	let mut stored = space.open().expect("opens");
	// A value, the value that replaces it, and a purge of the second: the state file keeps
	// rows of the first that only some reads reach.
	space.commit(&mut stored, 0, 1, false);
	space.commit(&mut stored, 0, 2, false);
	space.purge(&mut stored, 0);
	drop(stored);
	let state = space.state_file();
	let sound = fs::read(&state).expect("reads");
	for at in (100..sound.len()).step_by(PAGE) {
		let mut damaged = sound.clone();
		damaged[at] ^= 0x40;
		fs::write(&state, &damaged).expect("writes");
		space.tampered.set(true);
		let opened = space
			.open()
			.expect("opens whatever happened to the state file");
		let opened = check(&space, opened).expect("answers as the log");
		drop(opened);
	}
}

#[test]
fn opening_takes_back_what_the_state_file_holds_without_writing_it() {
	let mut space = Space::new("without writing");
	let mut stored = space.open().expect("opens");
	for value in 0..20 {
		space.commit(&mut stored, usize::from(value), value, value % 3 == 0);
	}
	drop(stored);
	let state = space.state_file();
	let before = fs::read(&state).expect("reads");
	drop(space.open().expect("opens again"));
	assert!(
		fs::read(&state).expect("reads") == before,
		"opening wrote to a state file that covers the log"
	);
}

#[test]
fn a_state_file_of_another_history_of_the_same_length_is_made_again() {
	let mut space = Space::new("another history");
	let state = space.state_file();
	let (kept_log, other_state) = (
		space.path.with_file_name("kept-log"),
		space.path.with_file_name("other-state"),
	);
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 0, 1, false);
	drop(stored);
	fs::copy(&space.path, &kept_log).expect("copies");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 1, 2, false);
	drop(stored);
	fs::copy(&state, &other_state).expect("copies");

	// The log goes back, and takes another change in the place of the second.
	fs::copy(&kept_log, &space.path).expect("copies");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 2, 3, false);
	drop(stored);
	// The state file of the other history ends at a row of the same sequence.
	fs::copy(&other_state, &state).expect("copies");
	check(&space, space.open().expect("opens")).expect("answers as the log");
}

#[test]
fn a_change_that_fails_leaves_the_others_of_its_batch() {
	let mut space = Space::new("fails in a batch");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 0, 1, false);
	// A secret value without the secrets key: the change fails before it is saved.
	let locked = SpaceKeys::new(0, space.keys.newest().1.clone());
	let statement = Statement::new(
		space.rng.statement_id(),
		space.entities[1],
		password(),
		Value::Text("hunter2".into()),
	);
	assert!(
		stored
			.commit(
				&space.author,
				&locked,
				vec![Operation::assert(statement)],
				space.wall + 500,
				&mut space.rng,
			)
			.is_err()
	);
	space.commit(&mut stored, 2, 3, false);
	let before = current(stored.log()).expect("reads");
	assert_eq!(before.len(), 2);
	drop(stored);
	let reopened = check(&space, space.open().expect("opens again")).expect("answers as the log");
	assert_eq!(current(reopened.log()).expect("reads"), before);
}

/// The first bytes of a state file, which hold its own key: another file has others.
fn header(space: &Space) -> Vec<u8> {
	let bytes = fs::read(space.state_file()).expect("reads the state file");
	bytes[..96].to_vec()
}

#[test]
fn a_part_that_arrives_late_needs_no_new_state_file_when_the_last_commit_is_lost() {
	let mut space = Space::new("late part");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 0, 1, false);
	// The other device's change comes last, alone: the state file's mark is at it.
	space.arrive(&mut stored, 1, 2);
	drop(stored);
	let state = space.state_file();
	let kept = state.with_file_name("kept-state");
	fs::copy(&state, &kept).expect("copies");
	let made = header(&space);

	// Its payload and key arrive, which moves it to the end of the log file, and the state
	// file's commit is lost, as a power cut can lose it.
	let mut stored = space.open().expect("opens");
	space.deliver(&mut stored, 0);
	space.deliver(&mut stored, 0);
	assert_eq!(current(stored.log()).expect("reads").len(), 2);
	drop(stored);
	fs::copy(&kept, &state).expect("copies");

	let stored = check(&space, space.open().expect("opens")).expect("answers as the log");
	assert_eq!(current(stored.log()).expect("reads").len(), 2);
	drop(stored);
	assert_eq!(header(&space), made, "the same state file");
}

#[test]
fn changes_read_with_a_space_key_that_arrives_later_reach_the_state_file() {
	let mut space = Space::new("later key");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 0, 1, false);
	drop(stored);
	// A device that has only a later key of the space gets the change.
	let parcels: Vec<Parcel> = space.open().expect("opens").log().parcels().expect("reads");
	let other = Space::new("later key, other device");
	let later = SpaceKeys::new(1, other.keys.newest().1.clone());
	let path = space.path.with_file_name("other").join("log.sqlite");
	let open = |keys: &SpaceKeys| {
		StoredLog::open(
			&path,
			space.id,
			keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[password()],
		)
		.expect("opens")
	};
	let mut stored = open(&later);
	for parcel in &parcels {
		stored.receive(parcel, &later, space.wall).expect("accepts");
	}
	assert_eq!(current(stored.log()).expect("reads").len(), 0);
	let mut both = later.clone();
	both.insert(0, space.keys.get(0).expect("key 0").clone());
	stored.open_changes(&both).expect("opens the changes");
	assert_eq!(current(stored.log()).expect("reads").len(), 1);
	assert_eq!(stored.search("value").expect("searches").len(), 1);
	drop(stored);
	let reopened = open(&both);
	assert_eq!(current(reopened.log()).expect("reads").len(), 1);
	assert_eq!(reopened.search("value").expect("searches").len(), 1);
	assert_eq!(reopened.log().history().changes_read(), 0);
	// The log file says so too: nothing waits for its payload.
	for parcel in &parcels {
		assert!(reopened.log().is_complete(parcel.id()).expect("reads"));
	}
}

/// Whether `needle` is anywhere in the bytes of a file or of its write-ahead log.
fn on_disk(path: &Path, needle: &[u8]) -> bool {
	let mut wal = path.as_os_str().to_owned();
	wal.push("-wal");
	[path.to_path_buf(), PathBuf::from(wal)]
		.iter()
		.filter_map(|path| fs::read(path).ok())
		.any(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
}

#[test]
fn after_a_purge_neither_file_holds_what_was_purged() {
	let mut space = Space::new("purged from both files");
	let mut stored = space.open().expect("opens");
	let entity = space.entities[0];
	let address = Statement::new(
		space.rng.statement_id(),
		entity,
		field("org.example.address"),
		Value::Text("1 Old Road".into()),
	);
	let note = Statement::new(
		space.rng.statement_id(),
		entity,
		field("org.example.note"),
		Value::Text("met at the library".into()),
	);
	let first = stored
		.commit(
			&space.author,
			&space.keys,
			vec![
				Operation::assert(address.clone()),
				Operation::assert(note.clone()),
			],
			space.wall,
			&mut space.rng,
		)
		.expect("commits");
	space.commit(&mut stored, 1, 2, false);
	let sealed = nibkeep_base::cbor::to_vec(first.payload.as_ref().expect("a payload"));
	let wrapped = nibkeep_base::cbor::to_vec(&first.keys[0]);
	assert!(on_disk(&space.path, &sealed), "the payload is stored");
	let made = header(&space);

	space.wall += 1_000;
	let purge = stored
		.purge(
			&space.author,
			&space.keys,
			&[address.id],
			space.wall,
			&mut space.rng,
		)
		.expect("purges");
	assert!(purge.change.purges().changes.contains(&first.id()));
	assert!(stored.is_shredded());
	assert!(!on_disk(&space.path, &sealed), "the payload is gone");
	assert!(!on_disk(&space.path, &wrapped), "the key is gone");
	assert_ne!(header(&space), made, "the state file is written anew");
	assert_eq!(
		stored.log().state().statement(address.id).expect("reads"),
		None
	);
	assert!(
		stored
			.log()
			.state()
			.statement(note.id)
			.expect("reads")
			.is_some()
	);
	assert_eq!(stored.search("old").expect("searches").len(), 0);
	assert_eq!(stored.search("library").expect("searches").len(), 1);
	let stored = check(&space, stored).expect("answers as the log");
	drop(stored);
	let reopened = check(&space, space.open().expect("opens again")).expect("answers as the log");
	assert_eq!(
		reopened.log().state().statement(address.id).expect("reads"),
		None
	);
	assert!(reopened.log().is_change_purged(first.id()).expect("reads"));
}

#[test]
fn a_purge_from_another_device_rewrites_both_files() {
	let mut space = Space::new("purge received");
	let mut stored = space.open().expect("opens");
	space.arrive(&mut stored, 0, 1);
	while !space.pending.is_empty() {
		space.deliver(&mut stored, 0);
	}
	let (author, other) = &mut space.other;
	let first = other
		.parcels()
		.expect("reads")
		.into_iter()
		.next()
		.expect("a change");
	let statement = other
		.state()
		.statements()
		.expect("reads")
		.first()
		.map(|statement| statement.id)
		.expect("a statement");
	space.wall += 1_000;
	let purge = other
		.purge(author, &space.keys, [statement], space.wall, &mut space.rng)
		.expect("purges");
	let sealed = nibkeep_base::cbor::to_vec(first.payload.as_ref().expect("a payload"));
	assert!(on_disk(&space.path, &sealed));
	let made = header(&space);
	stored
		.receive(&purge, &space.keys, space.wall)
		.expect("accepts the purge");
	assert!(!on_disk(&space.path, &sealed));
	assert_ne!(header(&space), made);
	assert_eq!(
		stored.log().state().statement(statement).expect("reads"),
		None
	);
	let stored = check(&space, stored).expect("answers as the log");
	drop(stored);
}

#[test]
fn a_purge_received_in_a_batch_rewrites_both_files_once_the_batch_ends() {
	let mut space = Space::new("purge received in a batch");
	let mut stored = space.open().expect("opens");
	space.arrive(&mut stored, 0, 1);
	while !space.pending.is_empty() {
		space.deliver(&mut stored, 0);
	}
	let (author, other) = &mut space.other;
	let first = other
		.parcels()
		.expect("reads")
		.into_iter()
		.next()
		.expect("a change");
	let statement = current(other).expect("reads")[0].id;
	space.wall += 1_000;
	let purge = other
		.purge(author, &space.keys, [statement], space.wall, &mut space.rng)
		.expect("purges");
	space.wall += 1_000;
	let after = other
		.commit(
			author,
			&space.keys,
			vec![Operation::assert(Statement::new(
				space.rng.statement_id(),
				space.entities[1],
				field("org.example.note"),
				Value::Text("after the purge".to_owned()),
			))],
			space.wall,
			&mut space.rng,
		)
		.expect("commits");
	let sealed = nibkeep_base::cbor::to_vec(first.payload.as_ref().expect("a payload"));
	let made = header(&space);
	stored.begin_batch(&space.keys).expect("begins a batch");
	stored
		.receive(&purge, &space.keys, space.wall)
		.expect("accepts the purge");
	assert!(stored.batch_purged());
	stored
		.receive(&after, &space.keys, space.wall)
		.expect("accepts the change after it");
	// Nothing leaves the disk before the purge is saved.
	assert!(on_disk(&space.path, &sealed));
	assert_eq!(header(&space), made);
	stored.end_batch().expect("ends the batch");
	assert!(!on_disk(&space.path, &sealed));
	assert_ne!(header(&space), made);
	assert_eq!(
		stored.log().state().statement(statement).expect("reads"),
		None
	);
	let stored = check(&space, stored).expect("answers as the log");
	assert_eq!(stored.log().changes().expect("reads").len(), 3);
}

#[test]
fn a_change_that_governs_is_received_outside_a_batch() {
	let space = Space::new("governs, received in a batch");
	let mut stored = space.open().expect("opens");
	let mut rng = TestRng::new("a governing change");
	let mut other: Log = Log::new(space.id, Clock::new(DRIFT));
	let governing = other
		.commit(
			&space.other.0,
			&space.keys,
			vec![Operation::assert(Statement::new(
				rng.statement_id(),
				rng.entity_id(),
				nibkeep_model::fields::ROLE,
				Value::Text("admin".to_owned()),
			))],
			start(),
			&mut rng,
		)
		.expect("commits");
	assert_eq!(governing.change.kind(), nibkeep_log::Kind::Governing);
	stored.begin_batch(&space.keys).expect("begins a batch");
	assert!(matches!(
		stored.receive(&governing, &space.keys, start()),
		Err(StoreError::InBatch)
	));
	stored.end_batch().expect("ends the batch");
	stored
		.receive(&governing, &space.keys, start())
		.expect("receives it alone");
}

#[test]
fn a_state_file_that_holds_what_was_purged_is_built_again() {
	let mut space = Space::new("purged, state file put back");
	let mut stored = space.open().expect("opens");
	space.commit(&mut stored, 0, 1, false);
	let statement = current(stored.log()).expect("reads")[0].id;
	drop(stored);
	// The state file from before the purge comes back, as after a crash that kept it from
	// being written anew.
	let state = space.state_file();
	let kept = state.with_file_name("kept-state");
	fs::copy(&state, &kept).expect("copies");
	let mut stored = space.open().expect("opens");
	space.wall += 1_000;
	stored
		.purge(
			&space.author,
			&space.keys,
			&[statement],
			space.wall,
			&mut space.rng,
		)
		.expect("purges");
	drop(stored);
	fs::copy(&kept, &state).expect("copies");
	let reopened = check(&space, space.open().expect("opens")).expect("answers as the log");
	assert_eq!(
		reopened.log().state().statement(statement).expect("reads"),
		None
	);
}

#[test]
fn opening_reads_only_the_changes_after_the_state_file() {
	let mut space = Space::new("reads only what changed");
	let mut stored = space.open().expect("opens");
	for value in 0..30 {
		space.commit(&mut stored, usize::from(value % 3), value, false);
	}
	drop(stored);
	let reopened = space.open().expect("opens again");
	// Secret values, opened from the changes that hold them, would be read too.
	assert_eq!(
		reopened.log().history().changes_read(),
		0,
		"nothing changed since"
	);
	assert_eq!(current(reopened.log()).expect("reads").len(), 3);
	drop(reopened);

	// The state file loses its last commits, as a power cut may make it: opening reads
	// the changes it lacks, and only them.
	let state = space.state_file();
	let kept = state.with_file_name("kept-state");
	fs::copy(&state, &kept).expect("copies");
	let mut stored = space.open().expect("opens");
	for value in 30..34 {
		space.commit(&mut stored, usize::from(value % 3), value, false);
	}
	drop(stored);
	fs::copy(&kept, &state).expect("copies");
	let reopened = space.open().expect("opens");
	let read = reopened.log().history().changes_read();
	assert!((4..30).contains(&read), "{read} changes read");
	check(&space, reopened).expect("answers as the log");
}
