//! Helpers shared by the tests.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use nibkeep_base::cbor::{self, Value as Cbor};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::{EncryptionKey, SigningKey};
use nibkeep_governance::{Listing, encryption_key_text};
use std::collections::BTreeSet;

use nibkeep_log::{
	Change, ChangeId, Draft, Kind, Log, LogError, Parcel, Payload, SpaceKeys, WrappedKey,
};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_testkit::TestRng;
use nibkeep_time::{Clock, Timestamp};

/// How far ahead of a device's wall clock a received change may be: an hour.
pub(crate) const DRIFT: u64 = 3_600_000;

/// A device with its own signing key and its own copy of a space's log.
pub(crate) struct Device {
	pub(crate) key: SigningKey,
	pub(crate) log: Log,
}

impl Device {
	pub(crate) fn new(rng: &mut TestRng, space: SpaceId) -> Self {
		Self {
			key: rng.signing_key(),
			log: Log::new(space, Clock::new(DRIFT)),
		}
	}

	/// Commits operations at the wall clock time `wall`.
	pub(crate) fn commit(
		&mut self,
		space_key: &SpaceKeys,
		operations: Vec<Operation>,
		wall: u64,
		rng: &mut TestRng,
	) -> Parcel {
		self.log
			.commit(&self.key, space_key, operations, wall, rng)
			.expect("commits")
	}

	/// Makes a change of `kind` that holds `operations`, building on `parents`, as a later
	/// version of NibKeep would, or a replica that turned against its user, which chooses its
	/// own parents; and takes it in.
	pub(crate) fn forge(
		&mut self,
		keys: &SpaceKeys,
		(kind, parents): (Kind, BTreeSet<ChangeId>),
		operations: Vec<Operation>,
		wall: u64,
		rng: &mut TestRng,
	) -> Parcel {
		let latest = parents
			.iter()
			.filter_map(|&parent| self.log.change(parent).infallible())
			.map(|change| change.timestamp().millis())
			.max()
			.expect("a parent");
		let payload = Payload::from(operations);
		let mut draft = Draft::new(parents, Timestamp::new(latest + 1, 0), payload);
		draft.kind = kind;
		let forged = Change::create(self.log.space(), keys, &self.key, &draft, rng);
		self.log
			.receive(forged.clone(), keys, wall)
			.expect("takes it in");
		forged
	}

	/// Makes a change as [`Device::forge`] does, building on this device's heads.
	pub(crate) fn forge_on_heads(
		&mut self,
		keys: &SpaceKeys,
		kind: Kind,
		operations: Vec<Operation>,
		wall: u64,
		rng: &mut TestRng,
	) -> Parcel {
		let heads = self.log.heads().infallible();
		self.forge(keys, (kind, heads), operations, wall, rng)
	}

	/// Receives every change and every wrapped record key of `other` that this device does
	/// not have yet.
	pub(crate) fn receive_from(&mut self, other: &Device, keys: &SpaceKeys, wall: u64) {
		self.receive_changes_from(other, keys, wall);
		self.receive_keys_from(other, keys);
	}

	/// Receives every change of `other` that this device does not have yet, in the order of
	/// their IDs, which has nothing to do with the order they were made in.
	pub(crate) fn receive_changes_from(&mut self, other: &Device, keys: &SpaceKeys, wall: u64) {
		let parcels: Vec<Parcel> = other
			.log
			.parcels()
			.infallible()
			.into_iter()
			.filter(|parcel| !self.log.contains(parcel.id()).infallible())
			.collect();
		for parcel in parcels {
			self.log
				.receive(parcel, keys, wall)
				.expect("accepts the change");
		}
	}

	/// Receives the wrapped record keys of `other`.
	pub(crate) fn receive_keys_from(&mut self, other: &Device, keys: &SpaceKeys) {
		let wrapped: Vec<(StatementId, WrappedKey)> = other
			.log
			.wrapped_keys()
			.map(|(statement, wrapped)| (statement, wrapped.clone()))
			.collect();
		for (statement, wrapped) in wrapped {
			self.log
				.receive_wrapped_key(statement, wrapped, keys)
				.expect("keeps the key");
		}
	}
}

pub(crate) fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

pub(crate) fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// Adds a value to a field with a new statement.
pub(crate) fn add(rng: &mut TestRng, entity: EntityId, name: &str, value: Value) -> Operation {
	assert(rng, entity, field(name), value)
}

/// Asserts a value of a field with a new statement.
pub(crate) fn assert(
	rng: &mut TestRng,
	entity: EntityId,
	field: FieldName,
	value: Value,
) -> Operation {
	Operation::assert(Statement::new(rng.statement_id(), entity, field, value))
}

/// The governing operations that make a member's record, with a new user ID and a role.
pub(crate) fn making_member(rng: &mut TestRng, record: EntityId, role: &str) -> Vec<Operation> {
	let user = rng.entity_id().to_string();
	vec![
		assert(rng, record, fields::USER_ID, Value::Text(user)),
		assert(rng, record, fields::ROLE, Value::Text(role.to_owned())),
	]
}

/// The governing operations that list a key in a space on its record, owned by `member`,
/// with its encryption key and the listing that the key signed, marked as an AI agent's if
/// `ai_agent` says so.
pub(crate) fn listing(
	rng: &mut TestRng,
	space: SpaceId,
	(key, encryption): (&SigningKey, &EncryptionKey),
	(member, ai_agent): (EntityId, bool),
) -> Vec<Operation> {
	let signed = Listing::sign(key, encryption, space, (member, ai_agent));
	let record = signed.record;
	let key = key.verifying_key().to_text();
	let encryption = encryption_key_text(encryption);
	let mut operations = vec![
		assert(rng, record, fields::SIGNING_KEY, Value::Text(key)),
		assert(rng, record, fields::OWNER, Value::Reference(member)),
		assert(rng, record, fields::ENCRYPTION_KEY, Value::Text(encryption)),
		assert(rng, record, fields::LISTING, Value::Text(signed.to_text())),
	];
	if ai_agent {
		operations.push(assert(rng, record, fields::AI_AGENT, Value::Boolean(true)));
	}
	operations
}

/// Two distinct items, one to read and one to change.
pub(crate) fn pair<T>(items: &mut [T], from: usize, to: usize) -> (&T, &mut T) {
	assert_ne!(from, to);
	if from < to {
		let (left, right) = items.split_at_mut(to);
		(&left[from], &mut right[0])
	} else {
		let (left, right) = items.split_at_mut(from);
		(&right[0], &mut left[to])
	}
}

/// `to` receives every change of `from`, parents first, as a sync sends them. Changes of
/// keys it knows no listing of are refused, as a sync drops them.
pub(crate) fn send(from: &Log, to: &mut Log, keys: &SpaceKeys, wall: u64) {
	let mut summaries = from.summaries().infallible();
	summaries.sort_by_key(|summary| (summary.depth, summary.id));
	for summary in summaries {
		let parcel = from
			.parcel(summary.id)
			.infallible()
			.expect("an accepted change");
		match to.receive(parcel, keys, wall) {
			Ok(_) | Err(LogError::UnknownAuthor) => {}
			Err(error) => panic!("takes the change: {error}"),
		}
	}
}

/// Whether a change counts in every log that holds it, which must agree.
pub(crate) fn counts_in<'a>(logs: impl IntoIterator<Item = &'a Log>, change: &Parcel) -> bool {
	let decided: Vec<bool> = logs
		.into_iter()
		.filter(|log| log.contains(change.id()).infallible())
		.map(|log| !log.is_set_aside(change.id()).infallible())
		.collect();
	assert!(!decided.is_empty(), "no device holds the change");
	assert!(
		decided.iter().all(|&counts| counts == decided[0]),
		"the devices disagree: {decided:?}"
	);
	decided[0]
}

/// Sets a field: a new statement that supersedes the field's current statements in `log`.
pub(crate) fn set(
	rng: &mut TestRng,
	log: &Log,
	entity: EntityId,
	name: &str,
	value: Value,
) -> Operation {
	let field = field(name);
	let current: Vec<StatementId> = log
		.state()
		.current(entity, &field)
		.infallible()
		.iter()
		.map(|statement| statement.id)
		.collect();
	Operation::replace(
		Statement::new(rng.statement_id(), entity, field, value),
		current,
	)
}

/// The values of a field, as text, in the order of their statement IDs.
pub(crate) fn texts(log: &Log, entity: EntityId, name: &str) -> Vec<String> {
	log.state()
		.current(entity, &field(name))
		.infallible()
		.iter()
		.filter_map(|statement| statement.value.as_text().map(str::to_owned))
		.collect()
}

/// Decodes a change into its CBOR items, for tests that tamper with it.
pub(crate) fn items(change: &Change) -> Vec<Cbor> {
	match cbor::decode(&cbor::to_vec(change)).expect("decodes") {
		Cbor::Array(items) => items,
		other => panic!("a change is an array, not {other:?}"),
	}
}
