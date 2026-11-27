//! The storage of the log's state: changes taken back without applying them again, and a
//! storage that fails.

use crate::common::{DRIFT, Device, add, text, texts};
use nibkeep_base::{EntityId, StatementId};
use nibkeep_log::{Log, LogError, Received, SpaceKeys};
use nibkeep_model::storage::{Memory, Origin, Recorded, Statements};
use nibkeep_model::{FieldName, Statement};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::{Clock, Timestamp};

fn start() -> u64 {
	millis_at("2026-10-20", 9, 0)
}

#[test]
fn changes_taken_back_without_applying_leave_the_storage_as_it_is() {
	let mut rng = TestRng::new("not applying");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let (ada, grace) = (rng.entity_id(), rng.entity_id());
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&keys, vec![first], start(), &mut rng);
	let second = add(&mut rng, grace, "org.nibkeep.name", text("Grace"));
	let second = phone.commit(&keys, vec![second], start() + 1, &mut rng);

	let mut log = Log::new(space, Clock::new(DRIFT));
	log.restore(first, &keys, true)
		.expect("takes the change back");
	assert!(log.take_applied().is_empty(), "covered by the storage");
	log.restore(second, &keys, false)
		.expect("takes the change back");
	assert_eq!(log.take_applied().len(), 1);
	assert!(
		texts(&log, ada, "org.nibkeep.name").is_empty(),
		"not applied again"
	);
	assert_eq!(texts(&log, grace, "org.nibkeep.name"), ["Grace"]);
	assert_eq!(
		log.heads().expect("reads"),
		phone.log.heads().expect("reads")
	);
}

#[test]
fn a_waiting_change_is_applied_as_its_own_storage_says_when_a_later_one_completes_it() {
	let mut rng = TestRng::new("completed later");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let (ada, grace, alan) = (rng.entity_id(), rng.entity_id(), rng.entity_id());
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&keys, vec![first], start(), &mut rng);
	let second = add(&mut rng, grace, "org.nibkeep.name", text("Grace"));
	let second = phone.commit(&keys, vec![second], start() + 1, &mut rng);
	let third = add(&mut rng, alan, "org.nibkeep.name", text("Alan"));
	let third = phone.commit(&keys, vec![third], start() + 2, &mut rng);

	// The storage holds the third change's operations, but not the second's, which waited
	// for the first when the storage was last written.
	let mut log = Log::new(space, Clock::new(DRIFT));
	assert_eq!(log.restore(third, &keys, true), Ok(Received::Waiting));
	assert_eq!(log.restore(second, &keys, false), Ok(Received::Waiting));
	assert!(matches!(
		log.restore(first, &keys, false),
		Ok(Received::Accepted(accepted)) if accepted.len() == 3
	));
	assert_eq!(texts(&log, ada, "org.nibkeep.name"), ["Ada"]);
	assert_eq!(texts(&log, grace, "org.nibkeep.name"), ["Grace"]);
	assert!(
		texts(&log, alan, "org.nibkeep.name").is_empty(),
		"not applied again"
	);
	assert_eq!(log.take_applied().len(), 2);
}

/// A storage in memory that fails once it has taken a number of writes.
#[derive(Debug)]
struct Failing {
	memory: Memory,
	writes_left: usize,
}

impl Failing {
	fn write(&mut self) -> Result<(), &'static str> {
		self.writes_left = self.writes_left.checked_sub(1).ok_or("the disk is full")?;
		Ok(())
	}
}

impl Statements for Failing {
	type Error = &'static str;

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, Self::Error> {
		Ok(self.memory.get(id).unwrap_or_else(|never| match never {}))
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, Self::Error> {
		Ok(self.memory.all().unwrap_or_else(|never| match never {}))
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<nibkeep_base::Hash>, Self::Error> {
		Ok(self
			.memory
			.asserted_again(id)
			.unwrap_or_else(|never| match never {}))
	}

	fn is_superseded(&self, id: StatementId) -> Result<bool, Self::Error> {
		Ok(self
			.memory
			.is_superseded(id)
			.unwrap_or_else(|never| match never {}))
	}

	fn of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<StatementId>, Self::Error> {
		Ok(self
			.memory
			.of_field(entity, field)
			.unwrap_or_else(|never| match never {}))
	}

	fn of_entity(
		&self,
		entity: EntityId,
	) -> Result<Vec<(FieldName, Vec<StatementId>)>, Self::Error> {
		Ok(self
			.memory
			.of_entity(entity)
			.unwrap_or_else(|never| match never {}))
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, Self::Error> {
		Ok(self
			.memory
			.referring_to(entity)
			.unwrap_or_else(|never| match never {}))
	}

	fn entities(&self) -> Result<Vec<EntityId>, Self::Error> {
		Ok(self
			.memory
			.entities()
			.unwrap_or_else(|never| match never {}))
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, Self::Error> {
		Ok(self
			.memory
			.superseded_at(id)
			.unwrap_or_else(|never| match never {}))
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, Self::Error> {
		Ok(self
			.memory
			.superseded_at_of(id, entity, secret)
			.unwrap_or_else(|never| match never {}))
	}

	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), Self::Error> {
		self.write()?;
		let Ok(()) = self.memory.put(statement, origin);
		Ok(())
	}

	fn assert_again(
		&mut self,
		id: StatementId,
		change: nibkeep_base::Hash,
	) -> Result<(), Self::Error> {
		self.write()?;
		let Ok(()) = self.memory.assert_again(id, change);
		Ok(())
	}

	fn supersede(
		&mut self,
		scope: nibkeep_model::storage::Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), Self::Error> {
		self.write()?;
		let Ok(()) = self.memory.supersede(scope, ids, at);
		Ok(())
	}
}

#[test]
fn a_log_whose_storage_fails_refuses_changes_until_built_again() {
	let mut rng = TestRng::new("failing");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let mut changes = Vec::new();
	for n in 0..3 {
		let entity = rng.entity_id();
		let operation = add(&mut rng, entity, "org.nibkeep.name", text(&format!("{n}")));
		changes.push(phone.commit(&keys, vec![operation], start() + n, &mut rng));
	}

	// Each change writes its statement and what it supersedes: two writes.
	let failing = Failing {
		memory: Memory::default(),
		writes_left: 3,
	};
	let mut log = Log::with_storage(space, Clock::new(DRIFT), failing);
	let wall = start() + 10;
	assert!(matches!(
		log.receive(changes[0].clone(), &keys, wall),
		Ok(Received::Accepted(_))
	));
	assert_eq!(
		log.receive(changes[1].clone(), &keys, wall),
		Err(LogError::Storage)
	);
	assert_eq!(log.storage_error(), Some("the disk is full"));
	assert_eq!(log.storage_error(), None, "told once");
	assert_eq!(
		log.receive(changes[2].clone(), &keys, wall),
		Err(LogError::Storage)
	);
	let operation = add(
		&mut rng,
		EntityId::from_bytes([1; 16]),
		"org.nibkeep.name",
		text("x"),
	);
	assert!(matches!(
		log.commit(&phone.key, &keys, vec![operation], wall, &mut rng),
		Err(LogError::Storage)
	));
	assert!(
		!log.contains(changes[2].id()).expect("reads"),
		"nothing is taken after the failure"
	);
	// A fresh storage does not mend the log: only emptying it and taking the changes back
	// again does.
	let fresh = Failing {
		memory: Memory::default(),
		writes_left: 100,
	};
	assert_eq!(log.rebuild(fresh, &keys), Err(LogError::Storage));
}

#[test]
fn a_change_refused_while_taken_back_is_applied_when_it_comes_again() {
	let mut rng = TestRng::new("refused while taken back");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let (ada, grace) = (rng.entity_id(), rng.entity_id());
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&keys, vec![first], start(), &mut rng);
	let second = add(&mut rng, grace, "org.nibkeep.name", text("Grace"));
	let second = phone.commit(&keys, vec![second], start() + 1, &mut rng);

	// Taken back as covered, the second change is refused: too many changes wait.
	let mut log = Log::new(space, Clock::new(DRIFT));
	log.limit_waiting(0);
	assert_eq!(
		log.restore(second.clone(), &keys, true),
		Err(LogError::TooManyWaiting)
	);
	// When it comes again, from another device, its operations are applied.
	log.receive(first, &keys, start() + 10).expect("accepts");
	log.receive(second, &keys, start() + 10).expect("accepts");
	assert_eq!(texts(&log, grace, "org.nibkeep.name"), ["Grace"]);
}
