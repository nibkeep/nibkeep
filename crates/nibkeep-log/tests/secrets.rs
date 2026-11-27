//! Secret fields: two layers of encryption, locking, and purging.

use crate::common;

use nibkeep_model::storage::Infallibly;
use std::collections::BTreeSet;

use crate::common::{DRIFT, Device, field, set, text};
use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, StatementId};
use nibkeep_crypto::Envelope;
use nibkeep_log::{Change, Draft, LogError, Payload, SpaceKeys, WrappedKey};
use nibkeep_model::{Operation, Statement, Value};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Timestamp;

const PASSWORD: &str = "org.nibkeep.password";
const USERNAME: &str = "org.nibkeep.username";

fn start() -> u64 {
	millis_at("2026-10-10", 10, 0)
}

/// A space with a secrets key, two devices that hold it, and an archive node that does
/// not.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	archive_keys: SpaceKeys,
	phone: Device,
	laptop: Device,
	archive: Device,
	login: EntityId,
}

fn space(seed: &str) -> Space {
	let mut rng = TestRng::new(seed);
	let space = rng.space_id();
	let space_key = rng.key();
	let archive_keys = SpaceKeys::new(0, space_key.clone());
	let mut keys = SpaceKeys::new(0, space_key);
	keys.insert_secrets(0, rng.key());
	let mut devices = [(); 3].map(|()| Device::new(&mut rng, space));
	for device in &mut devices {
		device.log.set_secret_fields([field(PASSWORD)]);
	}
	let [phone, laptop, archive] = devices;
	let login = rng.entity_id();
	Space {
		rng,
		keys,
		archive_keys,
		phone,
		laptop,
		archive,
		login,
	}
}

/// The current values of a field, as text.
fn values(device: &Device, entity: EntityId, name: &str) -> Vec<String> {
	common::texts(&device.log, entity, name)
}

/// Sets the login's password on the phone. Returns the new statement's ID.
fn set_password(space: &mut Space, password: &str, wall: u64) -> StatementId {
	let operation = set(
		&mut space.rng,
		&space.phone.log,
		space.login,
		PASSWORD,
		text(password),
	);
	let Operation::Assert { statement, .. } = &operation else {
		unreachable!("setting asserts");
	};
	let id = statement.id;
	space
		.phone
		.commit(&space.keys, vec![operation], wall, &mut space.rng);
	id
}

/// The phone creates a login with a username and a password.
fn create_login(space: &mut Space) -> StatementId {
	let username = set(
		&mut space.rng,
		&space.phone.log,
		space.login,
		USERNAME,
		text("ada"),
	);
	space
		.phone
		.commit(&space.keys, vec![username], start(), &mut space.rng);
	set_password(space, "hunter2", start() + 1_000)
}

#[test]
fn secret_values_need_the_secrets_key_and_their_record_key() {
	let mut space = space("reading");
	create_login(&mut space);
	assert_eq!(values(&space.phone, space.login, PASSWORD), ["hunter2"]);
	let wall = start() + 2_000;

	// With the changes alone, the laptop sees the username but not the password.
	space
		.laptop
		.receive_changes_from(&space.phone, &space.keys, wall);
	assert_eq!(values(&space.laptop, space.login, USERNAME), ["ada"]);
	assert!(
		values(&space.laptop, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);
	// The wrapped record key opens it.
	space.laptop.receive_keys_from(&space.phone, &space.keys);
	assert_eq!(values(&space.laptop, space.login, PASSWORD), ["hunter2"]);

	// The archive node stores everything but cannot read the password.
	space
		.archive
		.receive_from(&space.phone, &space.archive_keys, wall);
	assert_eq!(values(&space.archive, space.login, USERNAME), ["ada"]);
	assert!(
		values(&space.archive, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);

	// A new device that gets everything from the archive alone can.
	let mut tablet = Device::new(&mut space.rng, space.phone.log.space());
	tablet.log.set_secret_fields([field(PASSWORD)]);
	tablet.receive_from(&space.archive, &space.keys, wall);
	assert_eq!(values(&tablet, space.login, PASSWORD), ["hunter2"]);
	assert_eq!(tablet.log.state(), space.phone.log.state());
}

#[test]
fn the_ordinary_layer_never_holds_a_secret_value() {
	let mut space = space("layers");
	let password = create_login(&mut space);
	let changes: Vec<_> = space
		.phone
		.log
		.changes()
		.infallible()
		.iter()
		.map(Change::id)
		.collect();
	for change in changes {
		let payload = space
			.phone
			.log
			.payload(change, &space.archive_keys)
			.expect("opens with the space key")
			.expect("a payload");
		for operation in &payload.operations {
			if let Operation::Assert { statement, .. } = operation {
				assert_ne!(statement.field, field(PASSWORD));
			}
		}
		for statement in payload.sealed.keys() {
			assert_eq!(*statement, password);
		}
	}
}

/// What takes a secret value away, replacing it or not, is a retraction of secret
/// statements, which names no entity, as the value's is sealed with it; and it takes the
/// value away for every device that holds it.
#[test]
fn what_takes_a_secret_value_away_names_no_entity() {
	let mut space = space("taking away");
	let first = create_login(&mut space);
	let second = set_password(&mut space, "correct horse", start() + 2_000);
	let removing = Operation::retract(space.login, [second]);
	space
		.phone
		.commit(&space.keys, vec![removing], start() + 3_000, &mut space.rng);
	let mut taken = Vec::new();
	for change in space.phone.log.changes().infallible() {
		let payload = space
			.phone
			.log
			.payload(change.id(), &space.archive_keys)
			.expect("opens with the space key")
			.expect("a payload");
		for operation in payload.operations {
			match operation {
				Operation::RetractSecret { statements } => taken.push(statements),
				Operation::Assert { statement, .. } => {
					assert_eq!(statement.field, field(USERNAME));
				}
				other => panic!("names the login: {other:?}"),
			}
		}
	}
	taken.sort();
	let mut expected = vec![BTreeSet::from([first]), BTreeSet::from([second])];
	expected.sort();
	assert_eq!(taken, expected);
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 4_000);
	assert_eq!(
		values(&space.laptop, space.login, PASSWORD),
		Vec::<String>::new()
	);
	assert_eq!(values(&space.laptop, space.login, USERNAME), ["ada"]);
}

/// A change takes away, as secret statements, those this replica knows to be of the entity
/// it names: opened, or sealed in the same change. One of another entity stays in a
/// retraction from the entity named, where it is not superseded, as before.
#[test]
fn a_retraction_takes_away_as_secret_only_statements_of_the_entity_it_names() {
	let mut space = space("of the entity named");
	let first = create_login(&mut space);
	let other = space.rng.entity_id();
	let payload_of = |space: &Space, change: &nibkeep_log::Parcel| {
		space
			.phone
			.log
			.payload(change.change.id(), &space.archive_keys)
			.expect("opens with the space key")
			.expect("a payload")
	};
	let elsewhere = space.phone.commit(
		&space.keys,
		vec![Operation::retract(other, [first])],
		start() + 2_000,
		&mut space.rng,
	);
	assert_eq!(
		payload_of(&space, &elsewhere).operations,
		[Operation::retract(other, [first])]
	);
	assert_eq!(values(&space.phone, space.login, PASSWORD), ["hunter2"]);
	// Two passwords in one change, the second replacing the first and the one before.
	let password = |space: &mut Space, text_of: &str| {
		Statement::new(
			space.rng.statement_id(),
			space.login,
			field(PASSWORD),
			text(text_of),
		)
	};
	let second = password(&mut space, "staple");
	let third = password(&mut space, "correct horse");
	let second_id = second.id;
	let both = space.phone.commit(
		&space.keys,
		vec![
			Operation::replace(second, [first]),
			Operation::replace(third, [second_id]),
		],
		start() + 3_000,
		&mut space.rng,
	);
	assert_eq!(
		payload_of(&space, &both).operations,
		[Operation::retract_secret([first, second_id])]
	);
	assert_eq!(
		values(&space.phone, space.login, PASSWORD),
		["correct horse"]
	);
}

#[test]
fn locking_forgets_secret_values_until_unlocked() {
	let mut space = space("locking");
	create_login(&mut space);
	let password = space
		.phone
		.log
		.state()
		.current(space.login, &field(PASSWORD))
		.infallible()[0]
		.id;
	space.phone.log.lock_secrets();
	assert!(
		values(&space.phone, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);
	assert_eq!(values(&space.phone, space.login, USERNAME), ["ada"]);
	// Nothing about the secret statement is left, not even its author.
	assert_eq!(space.phone.log.author_of(password).infallible(), None);
	space
		.phone
		.log
		.unlock_secrets(&space.keys)
		.expect("unlocks");
	assert_eq!(values(&space.phone, space.login, PASSWORD), ["hunter2"]);
	assert_eq!(
		space.phone.log.author_of(password).infallible(),
		Some(space.phone.key.verifying_key())
	);
}

#[test]
fn what_a_secret_value_replaced_stays_replaced_for_every_device() {
	let mut space = space("replaced");
	// Before the field became secret, the password was an ordinary value.
	space.phone.log.set_secret_fields([]);
	let ordinary = set(
		&mut space.rng,
		&space.phone.log,
		space.login,
		PASSWORD,
		text("old"),
	);
	let Operation::Assert { statement, .. } = &ordinary else {
		unreachable!("setting asserts");
	};
	let old = statement.id;
	space
		.phone
		.commit(&space.keys, vec![ordinary], start(), &mut space.rng);
	space.phone.log.set_secret_fields([field(PASSWORD)]);
	set_password(&mut space, "new", start() + 1_000);

	// The archive cannot read the new password, but knows the old one is replaced, and so
	// does the phone while its secrets are locked.
	space
		.archive
		.receive_from(&space.phone, &space.archive_keys, start() + 2_000);
	assert!(!space.archive.log.state().is_current(old).infallible());
	space.phone.log.lock_secrets();
	assert!(!space.phone.log.state().is_current(old).infallible());
	assert!(
		values(&space.phone, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);
}

#[test]
fn a_change_that_purges_what_it_does_not_build_on_is_rejected() {
	let mut space = space("bogus purge");
	create_login(&mut space);
	let not_sealed = space.rng.statement_id();
	let parents = space.phone.log.heads().infallible().clone();
	let latest = parents
		.iter()
		.filter_map(|&parent| space.phone.log.change(parent).infallible())
		.map(|change| change.timestamp())
		.max()
		.expect("a head");
	let mut draft = Draft::new(
		parents,
		Timestamp::new(latest.millis() + 1, 0),
		Payload::default(),
	);
	draft.purges.statements = BTreeSet::from([not_sealed]);
	let bogus = Change::create(
		space.phone.log.space(),
		&space.keys,
		&space.phone.key,
		&draft,
		&mut space.rng,
	);
	space
		.laptop
		.receive_from(&space.phone, &space.keys, start() + 5_000);
	assert_eq!(
		space
			.laptop
			.log
			.receive(bogus, &space.keys, start() + 5_000),
		Err(LogError::InvalidPurge)
	);
	assert!(!space.laptop.log.is_purged(not_sealed).infallible());
}

#[test]
fn a_bad_wrapped_key_does_not_block_a_good_one() {
	let mut space = space("bad keys");
	let password = create_login(&mut space);
	let wall = start() + 2_000;
	space
		.laptop
		.receive_changes_from(&space.phone, &space.keys, wall);
	// A faulty device sends garbage for the password's wrapped key first.
	let garbage = Envelope::seal(&space.rng.key(), b"garbage", b"", &mut space.rng);
	let garbage: WrappedKey = cbor::from_slice(&cbor::encode(&Cbor::Array(vec![
		0_u64.to_cbor(),
		garbage.to_cbor(),
	])))
	.expect("well-formed");
	space
		.laptop
		.log
		.receive_wrapped_key(password, garbage.clone(), &space.keys)
		.expect("keeps the key");
	assert!(
		values(&space.laptop, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);
	space.laptop.receive_keys_from(&space.phone, &space.keys);
	assert_eq!(values(&space.laptop, space.login, PASSWORD), ["hunter2"]);
	// The garbage, which the laptop could check, is not passed on.
	assert!(
		space
			.laptop
			.log
			.wrapped_keys()
			.all(|(_, key)| key != &garbage)
	);
}

#[test]
fn writing_a_secret_value_needs_the_secrets_key() {
	let mut space = space("writing");
	create_login(&mut space);
	let operation = set(
		&mut space.rng,
		&space.archive.log,
		space.login,
		PASSWORD,
		text("guess"),
	);
	let heads = space.archive.log.heads().infallible().clone();
	assert_eq!(
		space.archive.log.commit(
			&space.archive.key,
			&space.archive_keys,
			vec![operation],
			start(),
			&mut space.rng
		),
		Err(LogError::SecretsLocked)
	);
	assert_eq!(space.archive.log.heads().infallible(), heads);
}

#[test]
fn a_purged_value_is_gone_everywhere_and_what_it_replaced_stays_replaced() {
	let mut space = space("purging");
	let old = create_login(&mut space);
	let new = set_password(&mut space, "correct horse", start() + 2_000);
	let wall = start() + 3_000;
	space.laptop.receive_from(&space.phone, &space.keys, wall);
	space
		.archive
		.receive_from(&space.phone, &space.archive_keys, wall);
	assert_eq!(
		values(&space.laptop, space.login, PASSWORD),
		["correct horse"]
	);
	// Before the purge, the laptop keeps a copy of the new password's wrapped key.
	let mut stale_copy = Device::new(&mut space.rng, space.phone.log.space());
	stale_copy.receive_from(&space.laptop, &space.archive_keys, wall);

	// The new password was pasted into the wrong field: it is purged.
	let purge = space
		.phone
		.log
		.purge(&space.phone.key, &space.keys, [new], wall, &mut space.rng)
		.expect("purges");
	for (device, keys) in [
		(&mut space.laptop, &space.keys),
		(&mut space.archive, &space.archive_keys),
	] {
		device
			.log
			.receive(purge.clone(), keys, wall)
			.expect("accepts the purge");
		assert!(device.log.is_purged(new).infallible());
		assert!(
			device
				.log
				.wrapped_keys()
				.all(|(statement, _)| statement != new)
		);
	}

	// The password is gone, and the one it replaced did not come back, now or in the past.
	for device in [&space.phone, &space.laptop] {
		assert!(
			values(device, space.login, PASSWORD).is_empty(),
			"no secret value is shown"
		);
		assert!(device.log.state().statement(new).infallible().is_none());
		assert!(!device.log.state().is_current(old).infallible());
		let between = device.log.state_as_of(start() + 2_500);
		assert!(!between.is_current(old).infallible());
		assert!(between.statement(new).infallible().is_none());
	}
	// A wrapped key that arrives afterwards from an old copy is ignored.
	space.laptop.receive_keys_from(&stale_copy, &space.keys);
	assert!(
		values(&space.laptop, space.login, PASSWORD).is_empty(),
		"no secret value is shown"
	);
	assert!(space.laptop.log.is_purged(new).infallible());
	assert_eq!(space.laptop.log.state(), space.phone.log.state());
}

#[test]
fn history_keeps_old_secret_values_until_they_are_purged() {
	let mut space = space("history");
	create_login(&mut space);
	set_password(&mut space, "correct horse", start() + DRIFT / 2);
	let before = space.phone.log.state_as_of(start() + 1_000);
	let field = field(PASSWORD);
	let earlier: Vec<Value> = before
		.current(space.login, &field)
		.infallible()
		.iter()
		.map(|statement| statement.value.clone())
		.collect();
	assert_eq!(earlier, [text("hunter2")]);
}

#[test]
fn only_what_the_device_holds_can_be_purged() {
	let mut space = space("unknown");
	create_login(&mut space);
	let unknown = space.rng.statement_id();
	assert_eq!(
		space.phone.log.purge(
			&space.phone.key,
			&space.keys,
			[unknown],
			start() + 5_000,
			&mut space.rng
		),
		Err(LogError::CannotPurge)
	);
}

#[test]
fn a_secret_value_is_sealed_with_the_secrets_key_chosen_not_the_highest_number() {
	let mut space = space("the secrets key chosen");
	let space_key = space.keys.get(0).expect("key 0").clone();
	// A key of a higher number, which the replica holds but does not seal with, as one
	// that a revoked device made.
	space.keys.insert_secrets(u64::MAX, space.rng.key());
	space.keys.write_secrets_with(Some(0));
	assert_eq!(space.keys.newest_secrets().map(|(epoch, _)| epoch), Some(0));
	create_login(&mut space);
	// The laptop holds key 0 alone, and opens the password.
	let mut laptop_keys = SpaceKeys::new(0, space_key);
	laptop_keys.insert_secrets(0, space.keys.secrets(0).expect("key 0").clone());
	let wall = start() + 2_000;
	space
		.laptop
		.receive_changes_from(&space.phone, &laptop_keys, wall);
	space.laptop.receive_keys_from(&space.phone, &laptop_keys);
	assert_eq!(values(&space.laptop, space.login, PASSWORD), ["hunter2"]);
	// A number that the replica does not hold chooses the highest.
	space.keys.write_secrets_with(Some(5));
	assert_eq!(
		space.keys.newest_secrets().map(|(epoch, _)| epoch),
		Some(u64::MAX)
	);
}
