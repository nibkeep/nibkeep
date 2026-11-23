//! Purging any value: the changes that asserted it lose their keys and payloads, and the
//! purge states again everything else they held ([technical design: purging]).
//!
//! [technical design: purging]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#64-purging

mod common;

use common::{DRIFT, Device, add, field, set, text, texts};
use nibkeep_base::{EntityId, StatementId};
use nibkeep_log::{Kind, Log, LogError, Parcel, Received, SpaceKeys};
use nibkeep_model::storage::{Infallibly, Statements};
use nibkeep_model::{Operation, Value};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;
use proptest::prelude::*;

fn start() -> u64 {
	millis_at("2026-10-21", 9, 0)
}

const NAME: &str = "org.nibkeep.name";
const NOTE: &str = "org.example.note";
const PASSWORD: &str = "org.nibkeep.password";

/// A space with its keys, and a phone and a laptop that hold its secrets fields.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	phone: Device,
	laptop: Device,
	ada: EntityId,
}

fn space(seed: &str) -> Space {
	let mut rng = TestRng::new(seed);
	let id = rng.space_id();
	let mut keys = SpaceKeys::new(0, rng.key());
	keys.insert_secrets(0, rng.key());
	let mut phone = Device::new(&mut rng, id);
	let mut laptop = Device::new(&mut rng, id);
	for device in [&mut phone, &mut laptop] {
		device.log.set_secret_fields([field(PASSWORD)]);
	}
	let ada = rng.entity_id();
	Space {
		rng,
		keys,
		phone,
		laptop,
		ada,
	}
}

/// The ID of the statement an operation asserts.
fn asserted(operation: &Operation) -> StatementId {
	match operation {
		Operation::Assert { statement, .. } => statement.id,
		_ => panic!("an assertion"),
	}
}

/// The values of a field as of a time, as text.
fn texts_as_of(log: &Log, millis: u64, entity: EntityId, name: &str) -> Vec<String> {
	log.state_as_of(millis)
		.current(entity, &field(name))
		.infallible()
		.iter()
		.filter_map(|statement| statement.value.as_text().map(str::to_owned))
		.collect()
}

/// An address typed into a change that also holds a note, edited once: the first change
/// asserts the address and a note, the second replaces the address.
struct Edited {
	first: Parcel,
	note: StatementId,
	old_address: StatementId,
	new_address: StatementId,
}

fn edited(space: &mut Space) -> Edited {
	let ada = space.ada;
	let address = add(
		&mut space.rng,
		ada,
		"org.example.address",
		text("1 Old Road"),
	);
	let note = add(&mut space.rng, ada, NOTE, text("met at the library"));
	let (old_address, note_id) = (asserted(&address), asserted(&note));
	let first = space
		.phone
		.commit(&space.keys, vec![address, note], start(), &mut space.rng);
	let moved = set(
		&mut space.rng,
		&space.phone.log,
		ada,
		"org.example.address",
		text("2 New Street"),
	);
	let new_address = asserted(&moved);
	space
		.phone
		.commit(&space.keys, vec![moved], start() + 60_000, &mut space.rng);
	Edited {
		first,
		note: note_id,
		old_address,
		new_address,
	}
}

#[test]
fn a_purge_destroys_the_change_and_states_the_rest_again() {
	let mut space = space("restating");
	let edited = edited(&mut space);
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 90_000);
	let purge_millis = start() + 120_000;
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			purge_millis,
			&mut space.rng,
		)
		.expect("purges");
	assert_eq!(purge.change.purges().changes.len(), 1);
	assert!(purge.change.purges().changes.contains(&edited.first.id()));
	assert_eq!(purge.change.kind(), Kind::Ordinary);
	space
		.laptop
		.log
		.receive(purge, &space.keys, purge_millis)
		.expect("accepts the purge");

	for device in [&space.phone, &space.laptop] {
		let log = &device.log;
		assert!(log.is_change_purged(edited.first.id()).infallible());
		assert!(log.sealed_payload(edited.first.id()).infallible().is_none());
		nibkeep_testkit::assert_empty!(log.change_keys_of(edited.first.id()).infallible());
		// The purged value is gone; the current value and the note stay.
		assert_eq!(log.state().statement(edited.old_address).infallible(), None);
		assert_eq!(
			texts(log, space.ada, "org.example.address"),
			["2 New Street"]
		);
		assert_eq!(texts(log, space.ada, NOTE), ["met at the library"]);
		// The note counts from its first change, and shows from the purge on, written by
		// the device that purged.
		let recorded = log
			.state()
			.storage()
			.get(edited.note)
			.infallible()
			.expect("the note");
		assert_eq!(recorded.at, edited.first.change.timestamp());
		assert_eq!(
			recorded.restated.map(nibkeep_time::Timestamp::millis),
			Some(purge_millis)
		);
		assert_eq!(
			log.author_of(edited.note).infallible(),
			Some(space.phone.key.verifying_key())
		);
		// Between the change and its purge, the space shows neither.
		nibkeep_testkit::assert_empty!(texts_as_of(log, start() + 30_000, space.ada, NOTE));
		nibkeep_testkit::assert_empty!(texts_as_of(
			log,
			start() + 30_000,
			space.ada,
			"org.example.address"
		));
		assert_eq!(
			texts_as_of(log, purge_millis, space.ada, NOTE),
			["met at the library"]
		);
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn a_purged_value_never_comes_back() {
	let mut space = space("never again");
	let edited = edited(&mut space);
	space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges");
	// The purged change, its payload and its key, sent again, add nothing.
	assert_eq!(
		space
			.phone
			.log
			.receive(edited.first.clone(), &space.keys, start() + 130_000),
		Ok(Received::Duplicate)
	);
	assert_eq!(
		space.phone.log.receive_payload(
			edited.first.id(),
			edited.first.payload.clone().expect("a payload"),
			&space.keys
		),
		Ok(false)
	);
	assert_eq!(
		space
			.phone
			.log
			.state()
			.statement(edited.old_address)
			.infallible(),
		None
	);
	nibkeep_testkit::assert_empty!(
		space
			.phone
			.log
			.change_keys_of(edited.first.id())
			.infallible()
	);

	// A device that gets everything, in any order, never shows it either.
	let parcels: Vec<Parcel> = space.phone.log.parcels().infallible();
	for parcel in parcels.iter().rev() {
		space
			.laptop
			.log
			.receive(parcel.clone(), &space.keys, start() + 130_000)
			.expect("takes the change");
	}
	space
		.laptop
		.log
		.receive(edited.first.clone(), &space.keys, start() + 130_000)
		.expect("takes the change again");
	assert_eq!(
		space
			.laptop
			.log
			.state()
			.statement(edited.old_address)
			.infallible(),
		None
	);
	assert_eq!(space.laptop.log.state(), space.phone.log.state());
}

#[test]
fn a_device_that_applied_the_change_builds_its_state_again() {
	let mut space = space("again");
	let edited = edited(&mut space);
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 90_000);
	assert_eq!(
		space
			.laptop
			.log
			.state()
			.statement(edited.old_address)
			.infallible()
			.map(|statement| statement.id),
		Some(edited.old_address)
	);
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address, edited.new_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges both addresses");
	assert_eq!(purge.change.purges().changes.len(), 2);
	space
		.laptop
		.log
		.receive(purge, &space.keys, start() + 120_000)
		.expect("accepts the purge");
	assert!(!space.laptop.log.take_stale(), "built again in memory");
	nibkeep_testkit::assert_empty!(texts(&space.laptop.log, space.ada, "org.example.address"));
	assert_eq!(
		texts(&space.laptop.log, space.ada, NOTE),
		["met at the library"]
	);
	assert_eq!(space.laptop.log.state(), space.phone.log.state());
}

#[test]
fn a_secret_held_by_a_purged_change_is_stated_again_sealed() {
	let mut space = space("secrets restated");
	let ada = space.ada;
	let name = add(&mut space.rng, ada, NAME, text("Ada"));
	let password = add(&mut space.rng, ada, PASSWORD, text("hunter2"));
	let name_id = asserted(&name);
	space
		.phone
		.commit(&space.keys, vec![name, password], start(), &mut space.rng);
	space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[name_id],
			start() + 60_000,
			&mut space.rng,
		)
		.expect("purges the name");
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 90_000);
	for device in [&space.phone, &space.laptop] {
		nibkeep_testkit::assert_empty!(texts(&device.log, ada, NAME));
		assert_eq!(texts(&device.log, ada, PASSWORD), ["hunter2"]);
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn what_governs_the_space_cannot_be_purged() {
	let mut space = space("governing");
	let device = space.rng.entity_id();
	let key = Operation::assert(nibkeep_model::Statement::new(
		space.rng.statement_id(),
		device,
		nibkeep_model::fields::SIGNING_KEY,
		text("a key"),
	));
	let key_id = asserted(&key);
	space
		.phone
		.commit(&space.keys, vec![key], start(), &mut space.rng);
	assert_eq!(
		space.phone.log.purge(
			&space.phone.key,
			&space.keys,
			[key_id],
			start() + 60_000,
			&mut space.rng
		),
		Err(LogError::CannotPurge)
	);
}

/// What happens to the space: a value written, replacing one or not, or purged.
#[derive(Clone, Debug)]
enum Step {
	Write {
		field: usize,
		value: u8,
		replace: bool,
	},
	Purge(usize),
}

fn step() -> impl Strategy<Value = Step> {
	prop_oneof![
		4 => (0..3_usize, any::<u8>(), any::<bool>())
			.prop_map(|(field, value, replace)| Step::Write { field, value, replace }),
		1 => any::<usize>().prop_map(Step::Purge),
	]
}

/// One part of a change, as it travels.
#[derive(Clone, Debug)]
enum Part {
	Header(Box<Parcel>),
	Payload(nibkeep_log::ChangeId, nibkeep_log::SealedPayload),
	Key(nibkeep_log::ChangeId, nibkeep_log::WrappedChangeKey),
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(64))]

	#[test]
	fn changes_payloads_keys_and_purges_in_any_order_give_one_state(
		steps in prop::collection::vec(step(), 1..12),
		shuffle in prop::collection::vec(any::<u64>(), 40),
		applied_before in any::<bool>(),
	) {
		let mut space = space("purges in any order");
		let fields = ["org.example.a", "org.example.b", "org.example.c"];
		let mut written: Vec<StatementId> = Vec::new();
		let mut wall = start();
		let mut current_after_purge = Vec::new();
		for step in steps {
			wall += 1_000;
			match step {
				Step::Write { field: which, value, replace } => {
					let value = Value::Integer(i64::from(value));
					let operation = if replace {
						set(&mut space.rng, &space.phone.log, space.ada, fields[which], value)
					} else {
						add(&mut space.rng, space.ada, fields[which], value)
					};
					written.push(asserted(&operation));
					space.phone.commit(&space.keys, vec![operation], wall, &mut space.rng);
				}
				Step::Purge(which) => {
					if written.is_empty() {
						continue;
					}
					// The laptop may have applied the change before its purge arrives.
					if applied_before {
						space.laptop.receive_from(&space.phone, &space.keys, wall);
					}
					let purged = written.remove(which % written.len());
					let before: Vec<Vec<String>> = fields
						.iter()
						.map(|name| current_except(&space.phone.log, space.ada, name, purged))
						.collect();
					space
						.phone
						.log
						.purge(&space.phone.key, &space.keys, [purged], wall, &mut space.rng)
						.expect("purges");
					// Purging changes no other field's current values.
					let after: Vec<Vec<String>> = fields
						.iter()
						.map(|name| current_except(&space.phone.log, space.ada, name, purged))
						.collect();
					prop_assert_eq!(&before, &after);
					current_after_purge.push(purged);
				}
			}
		}
		let mut parts = Vec::new();
		for parcel in space.phone.log.parcels().infallible() {
			let id = parcel.id();
			if let Some(payload) = parcel.payload.clone() {
				parts.push(Part::Payload(id, payload));
			}
			if let Some(key) = parcel.keys.first() {
				parts.push(Part::Key(id, key.clone()));
			}
			parts.push(Part::Header(Box::new(Parcel {
				payload: None,
				keys: Vec::new(),
				..parcel
			})));
		}
		let mut ranked: Vec<(u64, Part)> = parts
			.into_iter()
			.enumerate()
			.map(|(index, part)| (shuffle[index % shuffle.len()], part))
			.collect();
		ranked.sort_by_key(|(rank, _)| *rank);
		let mut pending: Vec<Part> = ranked.into_iter().map(|(_, part)| part).collect();
		while !pending.is_empty() {
			let mut later = Vec::new();
			for part in pending {
				let unknown = match &part {
					Part::Header(parcel) => {
						if space.laptop.log.receive((**parcel).clone(), &space.keys, wall)
							== Ok(Received::Waiting)
						{
							continue;
						}
						false
					}
					Part::Payload(id, payload) => {
						space.laptop.log.receive_payload(*id, payload.clone(), &space.keys)
							== Err(LogError::UnknownChange)
					}
					Part::Key(id, key) => {
						space.laptop.log.receive_change_key(*id, key.clone(), &space.keys)
							== Err(LogError::UnknownChange)
					}
				};
				if unknown {
					later.push(part);
				}
			}
			pending = later;
		}
		prop_assert_eq!(space.laptop.log.state(), space.phone.log.state());
		for purged in current_after_purge {
			prop_assert_eq!(space.laptop.log.state().statement(purged).infallible(), None);
		}
	}
}

/// The current values of a field, as text, without a statement.
fn current_except(log: &Log, entity: EntityId, name: &str, except: StatementId) -> Vec<String> {
	log.state()
		.current(entity, &field(name))
		.infallible()
		.iter()
		.filter(|statement| statement.id != except)
		.map(|statement| format!("{:?}", statement.value))
		.collect()
}

#[test]
fn a_log_with_storage_it_cannot_make_anew_is_left_stale_for_its_owner() {
	let mut space = space("stale");
	let edited = edited(&mut space);
	// A log whose storage is not in memory: here, one wrapped so that it cannot be made
	// anew.
	let mut log = Log::with_storage(
		space.phone.log.space(),
		Clock::new(DRIFT),
		Opaque(nibkeep_model::storage::Memory::default()),
	);
	for parcel in space.phone.log.parcels().infallible() {
		log.receive(parcel, &space.keys, start() + 90_000)
			.expect("takes the change");
	}
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges");
	log.receive(purge, &space.keys, start() + 120_000)
		.expect("accepts the purge");
	assert!(log.take_stale());
	assert!(!log.take_stale(), "told once");
	log.rebuild(
		Opaque(nibkeep_model::storage::Memory::default()),
		&space.keys,
	)
	.expect("builds again");
	assert_eq!(
		log.state().statement(edited.old_address).expect("reads"),
		None
	);
	assert_eq!(
		log.state().statements().expect("reads").len(),
		space.phone.log.state().statements().infallible().len()
	);
}

/// Storage in memory that does not offer to be made anew, as a file does not.
#[derive(Debug)]
struct Opaque(nibkeep_model::storage::Memory);

impl Statements for Opaque {
	type Error = std::convert::Infallible;

	fn get(
		&self,
		id: StatementId,
	) -> Result<Option<nibkeep_model::storage::Recorded<'_>>, Self::Error> {
		self.0.get(id)
	}

	fn all(&self) -> Result<Vec<nibkeep_model::storage::Recorded<'_>>, Self::Error> {
		self.0.all()
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<nibkeep_base::Hash>, Self::Error> {
		self.0.asserted_again(id)
	}

	fn superseded_at(
		&self,
		id: StatementId,
	) -> Result<Option<nibkeep_time::Timestamp>, Self::Error> {
		self.0.superseded_at(id)
	}

	fn of_field(
		&self,
		entity: EntityId,
		field: &nibkeep_model::FieldName,
	) -> Result<Vec<StatementId>, Self::Error> {
		self.0.of_field(entity, field)
	}

	fn of_entity(
		&self,
		entity: EntityId,
	) -> Result<Vec<(nibkeep_model::FieldName, Vec<StatementId>)>, Self::Error> {
		self.0.of_entity(entity)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, Self::Error> {
		self.0.referring_to(entity)
	}

	fn entities(&self) -> Result<Vec<EntityId>, Self::Error> {
		self.0.entities()
	}

	fn put(
		&mut self,
		statement: &nibkeep_model::Statement,
		origin: nibkeep_model::storage::Origin,
	) -> Result<(), Self::Error> {
		self.0.put(statement, origin)
	}

	fn assert_again(
		&mut self,
		id: StatementId,
		change: nibkeep_base::Hash,
	) -> Result<(), Self::Error> {
		self.0.assert_again(id, change)
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<nibkeep_time::Timestamp>, Self::Error> {
		self.0.superseded_at_of(id, entity, secret)
	}

	fn supersede(
		&mut self,
		scope: nibkeep_model::storage::Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: nibkeep_time::Timestamp,
	) -> Result<(), Self::Error> {
		self.0.supersede(scope, ids, at)
	}
}

#[test]
fn values_that_concurrent_purges_state_again_go_with_a_further_purge() {
	let mut space = space("concurrent purges");
	let ada = space.ada;
	let address = add(
		&mut space.rng,
		ada,
		"org.example.address",
		text("1 Old Road"),
	);
	let phone_number = add(&mut space.rng, ada, "org.example.phone", text("555 0100"));
	let ids = [asserted(&address), asserted(&phone_number)];
	space.phone.commit(
		&space.keys,
		vec![address, phone_number],
		start(),
		&mut space.rng,
	);
	let [address, phone_number] = ids;
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 1_000);
	// Each device purges one value, before it sees the other's purge: each purge states
	// the other value again.
	let wall = start() + 2_000;
	let on_phone = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[address],
			wall,
			&mut space.rng,
		)
		.expect("purges");
	let on_laptop = space
		.laptop
		.log
		.purge(
			&space.laptop.key,
			&space.keys,
			[phone_number],
			wall,
			&mut space.rng,
		)
		.expect("purges");
	space
		.phone
		.log
		.receive(on_laptop, &space.keys, wall)
		.expect("accepts");
	space
		.laptop
		.log
		.receive(on_phone, &space.keys, wall)
		.expect("accepts");
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
	for id in ids {
		assert!(space.phone.log.state().statement(id).infallible().is_some());
	}
	// A further purge finds every change that states them again.
	let changes = space
		.phone
		.log
		.changes_to_purge(ids, &space.keys)
		.expect("finds them");
	assert_eq!(changes.len(), 2);
	let further = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			ids,
			wall + 1_000,
			&mut space.rng,
		)
		.expect("purges again");
	space
		.laptop
		.log
		.receive(further, &space.keys, wall + 1_000)
		.expect("accepts");
	for device in [&space.phone, &space.laptop] {
		for id in ids {
			assert_eq!(device.log.state().statement(id).infallible(), None);
		}
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn a_value_that_two_purges_state_again_is_purged_from_both() {
	let mut space = space("stated again twice");
	let ada = space.ada;
	let values: Vec<Operation> = ["1 Old Road", "555 0100", "met at the library"]
		.into_iter()
		.map(|value| add(&mut space.rng, ada, NOTE, text(value)))
		.collect();
	let [address, phone_number, note] = [0, 1, 2].map(|index| asserted(&values[index]));
	space
		.phone
		.commit(&space.keys, values, start(), &mut space.rng);
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 1_000);
	let wall = start() + 2_000;
	let on_phone = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[address],
			wall,
			&mut space.rng,
		)
		.expect("purges");
	let on_laptop = space
		.laptop
		.log
		.purge(
			&space.laptop.key,
			&space.keys,
			[phone_number],
			wall,
			&mut space.rng,
		)
		.expect("purges");
	space
		.phone
		.log
		.receive(on_laptop, &space.keys, wall)
		.expect("accepts");
	// Both purges state the note again; purging it purges both.
	assert_eq!(
		space
			.phone
			.log
			.changes_to_purge([note], &space.keys)
			.expect("finds them")
			.len(),
		2
	);
	space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[note],
			wall + 1_000,
			&mut space.rng,
		)
		.expect("purges the note");
	assert_eq!(space.phone.log.state().statement(note).infallible(), None);
	space
		.laptop
		.log
		.receive(on_phone, &space.keys, wall)
		.expect("accepts");
	space
		.laptop
		.receive_from(&space.phone, &space.keys, wall + 1_000);
	assert_eq!(space.laptop.log.state().statement(note).infallible(), None);
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn a_statement_that_two_changes_assert_is_purged_from_both() {
	// Two devices that run one migration at once write the same statement, under the ID
	// that it derives, each in a change of its own.
	let mut space = space("asserted twice");
	let ada = space.ada;
	let note = add(&mut space.rng, ada, NOTE, text("met at the library"));
	let id = asserted(&note);
	space
		.phone
		.commit(&space.keys, vec![note.clone()], start(), &mut space.rng);
	space
		.laptop
		.commit(&space.keys, vec![note], start() + 1_000, &mut space.rng);
	let wall = start() + 2_000;
	space.phone.receive_from(&space.laptop, &space.keys, wall);
	space.laptop.receive_from(&space.phone, &space.keys, wall);
	for device in [&space.phone, &space.laptop] {
		assert_eq!(texts(&device.log, ada, NOTE), ["met at the library"]);
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
	// Each device finds both changes, whichever it applied first.
	for device in [&space.phone, &space.laptop] {
		assert_eq!(
			device
				.log
				.changes_to_purge([id], &space.keys)
				.expect("finds them")
				.len(),
			2
		);
	}
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[id],
			wall + 1_000,
			&mut space.rng,
		)
		.expect("purges");
	space
		.laptop
		.log
		.receive(purge, &space.keys, wall + 1_000)
		.expect("accepts");
	for device in [&space.phone, &space.laptop] {
		assert_eq!(device.log.state().statement(id).infallible(), None);
		nibkeep_testkit::assert_empty!(texts(&device.log, ada, NOTE));
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn a_secret_that_two_changes_seal_opens_and_is_purged_from_both() {
	// Two devices that hold the secrets migrate one secret value at once: each seals the
	// same statement with a record key of its own.
	let mut space = space("sealed twice");
	let ada = space.ada;
	let password = add(&mut space.rng, ada, PASSWORD, text("hunter2"));
	let id = asserted(&password);
	space
		.phone
		.commit(&space.keys, vec![password.clone()], start(), &mut space.rng);
	space
		.laptop
		.commit(&space.keys, vec![password], start() + 1_000, &mut space.rng);
	let wall = start() + 2_000;
	space.phone.receive_from(&space.laptop, &space.keys, wall);
	space.laptop.receive_from(&space.phone, &space.keys, wall);
	for device in [&space.phone, &space.laptop] {
		assert_eq!(texts(&device.log, ada, PASSWORD), ["hunter2"]);
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
	let purge = space
		.laptop
		.log
		.purge(
			&space.laptop.key,
			&space.keys,
			[id],
			wall + 1_000,
			&mut space.rng,
		)
		.expect("purges");
	space
		.phone
		.log
		.receive(purge, &space.keys, wall + 1_000)
		.expect("accepts");
	for device in [&space.phone, &space.laptop] {
		nibkeep_testkit::assert_empty!(texts(&device.log, ada, PASSWORD));
		assert_eq!(device.log.wrapped_keys().count(), 0);
	}
	assert_eq!(space.phone.log.state(), space.laptop.log.state());
}

#[test]
fn a_secret_sealed_twice_opens_whichever_change_and_key_come_first() {
	// A third device takes in the later sealing first, then both record keys, then the
	// earlier sealing, which comes to hold the statement: it keeps the key of each.
	let mut space = space("sealed twice, keys first");
	let ada = space.ada;
	let password = add(&mut space.rng, ada, PASSWORD, text("hunter2"));
	space
		.phone
		.commit(&space.keys, vec![password.clone()], start(), &mut space.rng);
	space
		.laptop
		.commit(&space.keys, vec![password], start() + 1_000, &mut space.rng);
	let mut desktop = common::Device::new(&mut space.rng, space.phone.log.space());
	desktop.log.set_secret_fields([field(PASSWORD)]);
	let wall = start() + 2_000;
	desktop.receive_changes_from(&space.laptop, &space.keys, wall);
	desktop.receive_keys_from(&space.phone, &space.keys);
	desktop.receive_keys_from(&space.laptop, &space.keys);
	desktop.receive_changes_from(&space.phone, &space.keys, wall);
	assert_eq!(texts(&desktop.log, ada, PASSWORD), ["hunter2"]);
	desktop.log.lock_secrets();
	desktop.log.unlock_secrets(&space.keys).expect("unlocks");
	assert_eq!(texts(&desktop.log, ada, PASSWORD), ["hunter2"]);
	assert_eq!(desktop.log.wrapped_keys().count(), 2);
}
