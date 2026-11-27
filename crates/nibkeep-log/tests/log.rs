//! Changes: signed, linked by hashes, accepted on their headers, and applied in any order
//! once their payloads and keys are there.

use crate::common;

use nibkeep_model::storage::Infallibly;
use std::collections::BTreeSet;

use crate::common::{DRIFT, Device, add, field, items, set, text, texts};
use nibkeep_base::Hash;
use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::small::SmallSet;
use nibkeep_log::{Change, Draft, Kind, LogError, Parcel, Payload, Purges, Received, SpaceKeys};
use nibkeep_model::{Operation, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::{ClockError, Timestamp};
use proptest::prelude::*;

/// Saturday morning.
fn start() -> u64 {
	millis_at("2026-10-10", 10, 0)
}

/// A change of `operations` that `device` signs, with these parents and timestamp, whatever
/// its log holds.
fn create(
	device: &Device,
	keys: &SpaceKeys,
	parents: impl IntoIterator<Item = nibkeep_log::ChangeId>,
	timestamp: Timestamp,
	operations: Vec<Operation>,
	rng: &mut TestRng,
) -> Parcel {
	let draft = Draft::new(parents, timestamp, Payload::from(operations));
	Change::create(device.log.space(), keys, &device.key, &draft, rng)
}

/// The change alone, without its payload and key.
fn alone(parcel: &Parcel) -> Parcel {
	Parcel {
		payload: None,
		keys: Vec::new(),
		..parcel.clone()
	}
}

/// A change put together from its CBOR items, with the payload and key of `parcel`.
fn rebuilt(parcel: &Parcel, items: Vec<Cbor>) -> Parcel {
	Parcel {
		change: cbor::from_slice(&cbor::encode(&Cbor::Array(items))).expect("well-formed"),
		..parcel.clone()
	}
}

/// A space with its key and two devices.
fn setup(seed: &str) -> (TestRng, SpaceKeys, Device, Device) {
	let mut rng = TestRng::new(seed);
	let space = rng.space_id();
	let key = SpaceKeys::new(0, rng.key());
	let first = Device::new(&mut rng, space);
	let second = Device::new(&mut rng, space);
	(rng, key, first, second)
}

#[test]
fn a_committed_change_is_applied_and_becomes_the_head() {
	let (mut rng, key, mut phone, _) = setup("commit");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);

	assert_eq!(texts(&phone.log, ada, "org.nibkeep.name"), ["Ada"]);
	assert_eq!(
		phone.log.heads().infallible(),
		BTreeSet::from([change.id()])
	);
	let change = &change.change;
	assert!(change.parents().is_empty());
	assert_eq!(change.timestamp(), Timestamp::new(start(), 0));
	assert_eq!(change.author(), &phone.key.verifying_key());
	assert_eq!(change.kind(), Kind::Ordinary);
	let statement = phone
		.log
		.state()
		.current(ada, &field("org.nibkeep.name"))
		.infallible()[0]
		.id;
	assert_eq!(
		phone.log.author_of(statement).infallible(),
		Some(phone.key.verifying_key())
	);
}

#[test]
fn changes_round_trip_through_cbor_with_their_id() {
	let (mut rng, key, mut phone, _) = setup("cbor");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone
		.commit(&key, vec![operation], start(), &mut rng)
		.change;
	let encoded = cbor::to_vec(&change);
	assert_eq!(change.id().hash(), &Hash::of(&encoded));
	let decoded: Change = cbor::from_slice(&encoded).expect("decodes");
	assert_eq!(decoded.id(), change.id());
	assert_eq!(decoded, change);
}

#[test]
fn a_change_names_the_heads_its_author_had_seen() {
	let (mut rng, key, mut phone, mut laptop) = setup("heads");
	let ada = rng.entity_id();
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&key, vec![first], start(), &mut rng);
	laptop.receive_from(&phone, &key, start());

	// Both continue without seeing each other's change.
	let on_laptop = add(&mut rng, ada, "org.example.note", text("from the laptop"));
	let on_laptop = laptop.commit(&key, vec![on_laptop], start() + 1_000, &mut rng);
	let on_phone = add(&mut rng, ada, "org.example.note", text("from the phone"));
	let on_phone = phone.commit(&key, vec![on_phone], start() + 2_000, &mut rng);
	assert_eq!(on_laptop.change.parents(), &SmallSet::from([first.id()]));
	assert_eq!(on_phone.change.parents(), &SmallSet::from([first.id()]));

	// After syncing, the phone's next change follows both.
	phone.receive_from(&laptop, &key, start() + 3_000);
	assert_eq!(
		phone.log.heads().infallible(),
		BTreeSet::from([on_laptop.id(), on_phone.id()])
	);
	let next = add(&mut rng, ada, "org.example.note", text("after both"));
	let next = phone.commit(&key, vec![next], start() + 4_000, &mut rng);
	assert_eq!(
		next.change.parents(),
		&SmallSet::from([on_laptop.id(), on_phone.id()])
	);
	assert!(next.change.timestamp() > on_laptop.change.timestamp());
}

#[test]
fn concurrent_writes_keep_both_values() {
	let (mut rng, key, mut phone, mut laptop) = setup("concurrent");
	let grandma = rng.entity_id();
	let first = add(&mut rng, grandma, "org.nibkeep.name", text("Grandma"));
	phone.commit(&key, vec![first], start(), &mut rng);
	laptop.receive_from(&phone, &key, start());

	// Offline, each device renames her.
	let rename = set(
		&mut rng,
		&phone.log,
		grandma,
		"org.nibkeep.name",
		text("Granny"),
	);
	phone.commit(&key, vec![rename], start() + 1_000, &mut rng);
	let rename = set(
		&mut rng,
		&laptop.log,
		grandma,
		"org.nibkeep.name",
		text("Gran"),
	);
	laptop.commit(&key, vec![rename], start() + 1_000, &mut rng);

	phone.receive_from(&laptop, &key, start() + 2_000);
	laptop.receive_from(&phone, &key, start() + 2_000);
	let mut names = texts(&phone.log, grandma, "org.nibkeep.name");
	names.sort();
	assert_eq!(names, ["Gran", "Granny"]);
	assert_eq!(phone.log.state(), laptop.log.state());
}

#[test]
fn a_change_waits_for_its_parents() {
	let (mut rng, key, mut phone, mut laptop) = setup("waiting");
	let ada = rng.entity_id();
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&key, vec![first], start(), &mut rng);
	let second = set(
		&mut rng,
		&phone.log,
		ada,
		"org.nibkeep.name",
		text("Ada King"),
	);
	let second = phone.commit(&key, vec![second], start() + 1_000, &mut rng);

	assert_eq!(
		laptop.log.receive(second.clone(), &key, start() + 2_000),
		Ok(Received::Waiting)
	);
	assert!(laptop.log.contains(second.id()).infallible());
	assert_eq!(
		laptop.log.missing().infallible(),
		BTreeSet::from([first.id()])
	);
	nibkeep_testkit::assert_empty!(texts(&laptop.log, ada, "org.nibkeep.name"));

	assert_eq!(
		laptop.log.receive(first.clone(), &key, start() + 2_000),
		Ok(Received::Accepted(vec![first.id(), second.id()]))
	);
	assert!(laptop.log.missing().infallible().is_empty());
	assert_eq!(texts(&laptop.log, ada, "org.nibkeep.name"), ["Ada King"]);
	assert_eq!(laptop.log.state(), phone.log.state());
	assert_eq!(
		laptop.log.receive(first, &key, start() + 2_000),
		Ok(Received::Duplicate)
	);
}

#[test]
fn a_change_with_a_bad_signature_is_rejected() {
	let (mut rng, key, mut phone, mut laptop) = setup("signature");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);

	// A changed timestamp, and a change that names another device as its author.
	let mut later = items(&change.change);
	let Cbor::Array(header) = &mut later[1] else {
		panic!("the header is an array");
	};
	header[4] = Timestamp::new(start() + 1, 0).to_cbor();
	let mut other_author = items(&change.change);
	let Cbor::Array(header) = &mut other_author[1] else {
		panic!("the header is an array");
	};
	header[2] = laptop.key.verifying_key().to_cbor();
	for tampered in [later, other_author] {
		let tampered = rebuilt(&change, tampered);
		assert_eq!(
			laptop.log.receive(tampered, &key, start()),
			Err(LogError::InvalidSignature)
		);
	}
	nibkeep_testkit::assert_empty!(laptop.log.changes().infallible());
}

#[test]
fn restoring_trusts_the_signatures_checked_on_arrival_and_finds_forged_ones() {
	let (mut rng, key, mut phone, mut laptop) = setup("restored signatures");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);
	let good = change.change.clone();
	// The same change, with another signature: its header and payload still match.
	let mut forged = items(&change.change);
	// A signature is the array [1, bytes].
	let Cbor::Array(signature) = &mut forged[2] else {
		panic!("the signature is an array");
	};
	let Cbor::Bytes(signature) = &mut signature[1] else {
		panic!("the signature's bytes");
	};
	signature[0] ^= 1;
	let forged = rebuilt(&change, forged);
	assert_eq!(
		laptop.log.receive(forged.clone(), &key, start()),
		Err(LogError::InvalidSignature)
	);

	// From its own storage, a device does not check the signature again; checking finds
	// it. Its key is wrapped for another change ID, so it opens nothing.
	assert!(matches!(
		laptop.log.restore(forged.clone(), &key, false),
		Ok(Received::Accepted(_))
	));
	assert_eq!(laptop.log.forged().infallible(), [(forged.id(), None)]);
	nibkeep_testkit::assert_empty!(texts(&laptop.log, ada, "org.nibkeep.name"));
	nibkeep_testkit::assert_empty!(laptop.log.change_keys_of(forged.id()).infallible());
	assert_eq!(phone.log.change(good.id()).infallible(), Some(good.clone()));
	nibkeep_testkit::assert_empty!(phone.log.forged().infallible());
}

#[test]
fn a_change_whose_header_was_changed_on_the_disk_opens_nothing() {
	let (mut rng, key, mut phone, mut laptop) = setup("restored headers");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);
	// A change whose author was changed on the disk: its ID is another, so its key, tied
	// to its ID, no longer opens.
	let mut other_author = items(&change.change);
	let Cbor::Array(header) = &mut other_author[1] else {
		panic!("the header is an array");
	};
	header[2] = laptop.key.verifying_key().to_cbor();
	let tampered = rebuilt(&change, other_author);
	assert!(laptop.log.restore(tampered.clone(), &key, false).is_ok());
	assert!(!laptop.log.is_complete(tampered.id()).infallible());
	nibkeep_testkit::assert_empty!(texts(&laptop.log, ada, "org.nibkeep.name"));
	assert_eq!(laptop.log.forged().infallible(), [(tampered.id(), None)]);
}

#[test]
fn a_payload_cannot_be_moved_into_another_change() {
	let (mut rng, key, mut phone, _) = setup("binding");
	let ada = rng.entity_id();
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&key, vec![first], start(), &mut rng);
	let second = add(&mut rng, ada, "org.nibkeep.name", text("Lovelace"));
	let second = phone.commit(&key, vec![second], start() + 1_000, &mut rng);

	// The second change's header naming the first change's payload, properly signed.
	let mut moved = items(&second.change);
	let Cbor::Array(first_header) = &items(&first.change)[1] else {
		panic!("the header is an array");
	};
	let Cbor::Array(header) = &mut moved[1] else {
		panic!("the header is an array");
	};
	header[6] = first_header[6].clone();
	let header = moved[1].clone();
	moved[2] = phone
		.key
		.sign("nibkeep change", &cbor::encode(&header))
		.to_cbor();
	let mut moved = rebuilt(&second, moved);
	moved.payload.clone_from(&first.payload);

	// Neither the second change's key nor the first's opens it.
	let mut laptop = Device::new(&mut rng, phone.log.space());
	laptop
		.log
		.receive(alone(&first), &key, start() + 2_000)
		.expect("accepts the first change");
	assert!(
		laptop
			.log
			.receive(moved.clone(), &key, start() + 2_000)
			.is_ok()
	);
	assert_eq!(
		laptop
			.log
			.receive_change_key(moved.id(), first.keys[0].clone(), &key),
		Err(LogError::OtherKey)
	);
	// The second change's key is tied to the second change's ID, so the moved change has
	// none.
	nibkeep_testkit::assert_empty!(laptop.log.change_keys_of(moved.id()).infallible());
	assert!(!laptop.log.is_complete(moved.id()).infallible());
	nibkeep_testkit::assert_empty!(texts(&laptop.log, ada, "org.nibkeep.name"));
}

#[test]
fn a_change_is_accepted_on_its_header_and_applied_once_its_payload_and_key_are_there() {
	let (mut rng, key, mut phone, mut laptop) = setup("parts");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);
	let payload = change.payload.clone().expect("a payload");
	let wrapped = change.keys[0].clone();

	assert_eq!(
		laptop.log.receive(alone(&change), &key, start()),
		Ok(Received::Accepted(vec![change.id()]))
	);
	assert_eq!(
		laptop.log.heads().infallible(),
		BTreeSet::from([change.id()])
	);
	assert!(!laptop.log.is_complete(change.id()).infallible());
	assert_eq!(
		laptop
			.log
			.receive_change_key(change.id(), wrapped.clone(), &key),
		Ok(true)
	);
	assert_eq!(
		laptop.log.receive_change_key(change.id(), wrapped, &key),
		Ok(false)
	);
	nibkeep_testkit::assert_empty!(texts(&laptop.log, ada, "org.nibkeep.name"));
	assert_eq!(
		laptop
			.log
			.receive_payload(change.id(), payload.clone(), &key),
		Ok(true)
	);
	assert_eq!(texts(&laptop.log, ada, "org.nibkeep.name"), ["Ada"]);
	assert!(laptop.log.is_complete(change.id()).infallible());
	assert_eq!(laptop.log.take_applied().len(), 1);
	assert_eq!(
		laptop.log.receive_payload(change.id(), payload, &key),
		Ok(false)
	);
	assert_eq!(laptop.log.parcel(change.id()).infallible(), Some(change));
}

#[test]
fn a_payload_or_key_that_is_not_the_changes_is_dropped_without_hiding_the_change() {
	let (mut rng, key, mut phone, mut laptop) = setup("mismatch");
	let ada = rng.entity_id();
	let first = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&key, vec![first], start(), &mut rng);
	let second = add(&mut rng, ada, "org.example.note", text("second"));
	let second = phone.commit(&key, vec![second], start() + 1_000, &mut rng);

	// The first change arrives with the second's payload and key: they are dropped.
	let mixed = Parcel {
		change: first.change.clone(),
		payload: second.payload.clone(),
		keys: second.keys.clone(),
	};
	assert_eq!(
		laptop.log.receive(mixed, &key, start() + 2_000),
		Ok(Received::Accepted(vec![first.id()]))
	);
	assert!(laptop.log.sealed_payload(first.id()).infallible().is_none());
	nibkeep_testkit::assert_empty!(laptop.log.change_keys_of(first.id()).infallible());
	assert_eq!(
		laptop
			.log
			.receive_payload(first.id(), second.payload.clone().expect("a payload"), &key),
		Err(LogError::OtherPayload)
	);
	assert_eq!(
		laptop
			.log
			.receive_change_key(first.id(), second.keys[0].clone(), &key),
		Err(LogError::OtherKey)
	);
	// The right ones still count, whenever they come: here the key, and then the payload
	// with the change again.
	assert_eq!(
		laptop
			.log
			.receive_change_key(first.id(), first.keys[0].clone(), &key),
		Ok(true)
	);
	assert_eq!(
		laptop.log.receive(first.clone(), &key, start() + 2_000),
		Ok(Received::Completed)
	);
	assert_eq!(texts(&laptop.log, ada, "org.nibkeep.name"), ["Ada"]);
	assert_eq!(
		laptop.log.take_applied().len(),
		1,
		"what the parts that came with it applied"
	);
	// Parts of a change the device does not have are refused.
	assert_eq!(
		laptop.log.receive_payload(
			second.id(),
			second.payload.clone().expect("a payload"),
			&key
		),
		Err(LogError::UnknownChange)
	);
}

#[test]
fn changes_for_another_space_or_key_add_nothing() {
	let (mut rng, keys, mut phone, _) = setup("keys");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&keys, vec![operation], start(), &mut rng);

	let other_space = rng.space_id();
	let mut elsewhere = Device::new(&mut rng, other_space);
	assert_eq!(
		elsewhere.log.receive(change.clone(), &keys, start()),
		Err(LogError::OtherSpace)
	);
	// A device with another key of the same number drops the change's key.
	let mut stranger = Device::new(&mut rng, phone.log.space());
	let other_key = SpaceKeys::new(0, rng.key());
	assert!(
		stranger
			.log
			.receive(change.clone(), &other_key, start())
			.is_ok()
	);
	nibkeep_testkit::assert_empty!(stranger.log.change_keys_of(change.id()).infallible());
	nibkeep_testkit::assert_empty!(texts(&stranger.log, ada, "org.nibkeep.name"));
	// A device without the key of the change's epoch keeps the change's key, and reads the
	// change once it gets that space key.
	let mut newcomer = Device::new(&mut rng, phone.log.space());
	let mut later_keys = SpaceKeys::new(1, rng.key());
	assert!(
		newcomer
			.log
			.receive(change.clone(), &later_keys, start())
			.is_ok()
	);
	assert_eq!(
		newcomer.log.change_keys_of(change.id()).infallible(),
		change.keys.as_slice()
	);
	nibkeep_testkit::assert_empty!(texts(&newcomer.log, ada, "org.nibkeep.name"));
	assert_eq!(
		newcomer.log.payload(change.id(), &later_keys),
		Err(LogError::UnknownEpoch(0))
	);
	later_keys.insert(0, keys.get(0).expect("key 0").clone());
	newcomer.log.open_changes(&later_keys).expect("opens");
	assert_eq!(texts(&newcomer.log, ada, "org.nibkeep.name"), ["Ada"]);
}

#[test]
fn changes_made_with_older_keys_are_still_accepted() {
	let (mut rng, mut keys, mut phone, mut laptop) = setup("epochs");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let before = phone.commit(&keys, vec![operation], start(), &mut rng);
	// The space gets a new key; new changes use it.
	keys.insert(1, rng.key());
	let operation = add(&mut rng, ada, "org.example.note", text("after the new key"));
	let after = phone.commit(&keys, vec![operation], start() + 1_000, &mut rng);
	assert_eq!((before.change.epoch(), after.change.epoch()), (0, 1));
	laptop.receive_from(&phone, &keys, start() + 2_000);
	assert_eq!(laptop.log.state(), phone.log.state());
}

#[test]
fn a_rejected_change_takes_the_changes_waiting_for_it_along() {
	let (mut rng, keys, mut phone, mut laptop) = setup("rejected");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let parent = phone.commit(&keys, vec![operation], start(), &mut rng);
	// A faulty change that is not later than its parent, and a change that builds on it.
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("faulty"));
	let faulty = create(
		&phone,
		&keys,
		[parent.id()],
		parent.change.timestamp(),
		vec![operation],
		&mut rng,
	);
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("child"));
	let child = create(
		&phone,
		&keys,
		[faulty.id()],
		Timestamp::new(start() + DRIFT / 2, 0),
		vec![operation],
		&mut rng,
	);

	// They arrive in the wrong order.
	for change in [&child, &faulty] {
		assert_eq!(
			laptop.log.receive(change.clone(), &keys, start()),
			Ok(Received::Waiting)
		);
	}
	assert_eq!(
		laptop.log.receive(parent.clone(), &keys, start()),
		Ok(Received::Accepted(vec![parent.id()]))
	);
	assert_eq!(
		laptop.log.rejected().get(&faulty.id()),
		Some(&LogError::NotAfterParents)
	);
	assert_eq!(
		laptop.log.rejected().get(&child.id()),
		Some(&LogError::RejectedParent)
	);
	assert!(laptop.log.missing().infallible().is_empty());
	assert!(!laptop.log.contains(child.id()).infallible());
	// The clock never took the rejected changes into account.
	assert!(laptop.log.clock().last() < child.change.timestamp());
	// Sent again, they are rejected again.
	assert_eq!(
		laptop.log.receive(child, &keys, start()),
		Err(LogError::RejectedParent)
	);
}

#[test]
fn the_number_of_waiting_changes_is_limited() {
	let (mut rng, keys, mut phone, mut laptop) = setup("limit");
	let ada = rng.entity_id();
	let mut changes = Vec::new();
	for n in 0..3 {
		let operation = add(&mut rng, ada, "org.example.note", Value::Integer(n));
		let millis = start() + u64::try_from(n).expect("small") * 1_000;
		changes.push(phone.commit(&keys, vec![operation], millis, &mut rng));
	}
	laptop.log.limit_waiting(1);
	let wall = start() + 5_000;
	assert_eq!(
		laptop.log.receive(changes[2].clone(), &keys, wall),
		Ok(Received::Waiting)
	);
	assert_eq!(
		laptop.log.receive(changes[1].clone(), &keys, wall),
		Err(LogError::TooManyWaiting)
	);
	assert_eq!(
		laptop.log.receive(changes[0].clone(), &keys, wall),
		Ok(Received::Accepted(vec![changes[0].id()]))
	);
	assert_eq!(
		laptop.log.receive(changes[1].clone(), &keys, wall),
		Ok(Received::Accepted(vec![changes[1].id(), changes[2].id()]))
	);
}

#[test]
fn a_change_from_too_far_in_the_future_is_rejected() {
	let (mut rng, key, mut phone, mut laptop) = setup("future");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let change = phone.commit(&key, vec![operation], start(), &mut rng);
	// The laptop's clock is two hours behind.
	let behind = start() - 2 * DRIFT;
	assert!(matches!(
		laptop.log.receive(change.clone(), &key, behind),
		Err(LogError::Clock(ClockError::TooFarAhead { .. }))
	));
	assert!(!laptop.log.contains(change.id()).infallible());
	// Once its clock is right, it accepts the change.
	assert!(laptop.log.receive(change, &key, start()).is_ok());
}

#[test]
fn a_change_that_is_not_later_than_its_parents_is_rejected() {
	let (mut rng, key, mut phone, mut laptop) = setup("causality");
	let ada = rng.entity_id();
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Ada"));
	let first = phone.commit(&key, vec![operation], start(), &mut rng);
	let operation = add(&mut rng, ada, "org.nibkeep.name", text("Lovelace"));
	let early = create(
		&phone,
		&key,
		[first.id()],
		first.change.timestamp(),
		vec![operation],
		&mut rng,
	);
	laptop.receive_from(&phone, &key, start());
	assert_eq!(
		laptop.log.receive(early, &key, start()),
		Err(LogError::NotAfterParents)
	);
}

/// A statement of a field that governs the space: a device's key.
fn device_key(rng: &mut TestRng, device: nibkeep_base::EntityId) -> Operation {
	Operation::assert(nibkeep_model::Statement::new(
		rng.statement_id(),
		device,
		fields::SIGNING_KEY,
		text("a key"),
	))
}

#[test]
fn statements_that_govern_the_space_make_a_change_of_their_own() {
	let (mut rng, keys, mut phone, mut laptop) = setup("governing");
	let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
	let owner = rng.entity_id();
	// The space's first change governs it, and lists the phone with its own listing, so
	// that its changes count.
	let listing = common::listing(
		&mut rng,
		phone.log.space(),
		(&phone.key, &encryption),
		(owner, false),
	);
	let device = nibkeep_governance::replica_record(phone.log.space(), &phone.key.verifying_key());
	let name = add(&mut rng, device, "org.nibkeep.name", text("phone"));
	let key = listing[0].clone();
	assert_eq!(
		phone.log.commit(
			&phone.key,
			&keys,
			vec![key.clone(), name.clone()],
			start(),
			&mut rng
		),
		Err(LogError::MixedGoverning)
	);
	let key_id = match &key {
		Operation::Assert { statement, .. } => statement.id,
		_ => unreachable!("an assertion"),
	};
	let governing = phone.commit(&keys, listing, start(), &mut rng);
	assert_eq!(governing.change.kind(), Kind::Governing);
	let ordinary = phone.commit(&keys, vec![name.clone()], start() + 1_000, &mut rng);
	assert_eq!(ordinary.change.kind(), Kind::Ordinary);
	// Retracting a device's key, or replacing it with another value, governs the space too.
	let retract = Operation::retract(device, [key_id]);
	assert_eq!(
		phone.log.commit(
			&phone.key,
			&keys,
			vec![retract.clone(), name],
			start() + 1_500,
			&mut rng
		),
		Err(LogError::MixedGoverning)
	);
	let revoked = phone.commit(&keys, vec![retract], start() + 2_000, &mut rng);
	assert_eq!(revoked.change.kind(), Kind::Governing);
	laptop.receive_from(&phone, &keys, start() + 3_000);
	assert_eq!(laptop.log.state(), phone.log.state());
}

#[test]
fn a_payload_that_does_not_fit_its_kind_of_change_adds_nothing() {
	let (mut rng, keys, phone, mut laptop) = setup("kinds");
	let device = rng.entity_id();
	// An ordinary change that sets a device's key, and a governing change that names it.
	let key = device_key(&mut rng, device);
	let sets_a_key = create(
		&phone,
		&keys,
		[],
		Timestamp::new(start(), 0),
		vec![key],
		&mut rng,
	);
	let name = add(&mut rng, device, "org.nibkeep.name", text("phone"));
	let mut draft = Draft::new(
		[sets_a_key.id()],
		Timestamp::new(start() + 1_000, 0),
		Payload::from(vec![name]),
	);
	draft.kind = Kind::Governing;
	let names = Change::create(phone.log.space(), &keys, &phone.key, &draft, &mut rng);
	for parcel in [&sets_a_key, &names] {
		assert!(
			laptop
				.log
				.receive(parcel.clone(), &keys, start() + 2_000)
				.is_ok()
		);
		assert!(laptop.log.is_complete(parcel.id()).infallible());
	}
	assert_eq!(
		laptop.log.heads().infallible(),
		BTreeSet::from([names.id()])
	);
	nibkeep_testkit::assert_empty!(laptop.log.state().statements().infallible());
}

#[test]
fn a_change_purges_only_ordinary_changes_it_builds_on() {
	let (mut rng, keys, mut phone, mut laptop) = setup("purging changes");
	let device = rng.entity_id();
	// An ordinary first change, so that the space does not decide whose changes count.
	let first = add(&mut rng, device, "org.example.note", text("first"));
	phone.commit(&keys, vec![first], start() - 1_000, &mut rng);
	let governing = device_key(&mut rng, device);
	let governing = phone.commit(&keys, vec![governing], start(), &mut rng);
	let note = add(&mut rng, device, "org.example.note", text("a note"));
	let ordinary = phone.commit(&keys, vec![note], start() + 1_000, &mut rng);
	let elsewhere = add(&mut rng, device, "org.example.note", text("elsewhere"));
	let concurrent = create(
		&laptop,
		&keys,
		[governing.id()],
		Timestamp::new(start() + 1_500, 0),
		vec![elsewhere],
		&mut rng,
	);
	laptop.receive_from(&phone, &keys, start() + 2_000);
	laptop
		.log
		.receive(concurrent.clone(), &keys, start() + 2_000)
		.expect("accepts the concurrent change");

	let purging = |purged: nibkeep_log::ChangeId, rng: &mut TestRng| {
		let mut draft = Draft::new(
			[ordinary.id()],
			Timestamp::new(start() + 3_000, 0),
			Payload::default(),
		);
		draft.purges = Purges {
			changes: BTreeSet::from([purged]),
			..Purges::default()
		};
		Change::create(phone.log.space(), &keys, &phone.key, &draft, rng)
	};
	let not_an_ancestor = purging(concurrent.id(), &mut rng);
	let governs = purging(governing.id(), &mut rng);
	let fine = purging(ordinary.id(), &mut rng);
	for rejected in [not_an_ancestor, governs] {
		assert_eq!(
			laptop.log.receive(rejected, &keys, start() + 4_000),
			Err(LogError::InvalidPurge)
		);
	}
	assert!(laptop.log.receive(fine, &keys, start() + 4_000).is_ok());

	// A governing change purges nothing.
	let mut draft = Draft::new(
		[ordinary.id()],
		Timestamp::new(start() + 3_000, 0),
		Payload::default(),
	);
	draft.kind = Kind::Governing;
	draft.purges.changes.insert(ordinary.id());
	let governs_and_purges = Change::create(phone.log.space(), &keys, &phone.key, &draft, &mut rng);
	assert_eq!(
		laptop
			.log
			.receive(governs_and_purges, &keys, start() + 4_000),
		Err(LogError::MixedGoverning)
	);
}

#[test]
fn any_earlier_state_can_be_reconstructed() {
	let (mut rng, key, mut phone, _) = setup("history");
	let ada = rng.entity_id();
	let name = "org.nibkeep.name";
	for (n, value) in ["Ada Byron", "Ada King", "Ada Lovelace"]
		.into_iter()
		.enumerate()
	{
		let operation = set(&mut rng, &phone.log, ada, name, text(value));
		let n = u64::try_from(n).expect("small");
		phone.commit(&key, vec![operation], start() + n * 60_000, &mut rng);
	}
	let as_of = |millis: u64| -> Vec<Value> {
		phone
			.log
			.state_as_of(millis)
			.current(ada, &field(name))
			.infallible()
			.iter()
			.map(|statement| statement.value.clone())
			.collect()
	};
	nibkeep_testkit::assert_empty!(as_of(start() - 1));
	assert_eq!(as_of(start()), [text("Ada Byron")]);
	assert_eq!(as_of(start() + 90_000), [text("Ada King")]);
	assert_eq!(as_of(start() + 120_000), [text("Ada Lovelace")]);
	assert_eq!(
		phone.log.state_as_of(u64::MAX).statements().infallible(),
		phone.log.state().statements().infallible()
	);
}

/// An operation that one of three devices makes, then the devices it syncs with.
#[derive(Clone, Debug)]
struct Step {
	device: usize,
	value: u8,
	replace: bool,
	sync_from: Option<usize>,
}

fn step() -> impl Strategy<Value = Step> {
	(
		0..3_usize,
		any::<u8>(),
		any::<bool>(),
		prop::option::of(0..3_usize),
	)
		.prop_map(|(device, value, replace, sync_from)| Step {
			device,
			value,
			replace,
			sync_from,
		})
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(64))]

	#[test]
	fn replicas_converge_whatever_the_order_of_changes(steps in prop::collection::vec(step(), 1..16)) {
		let (mut rng, key, first, second) = setup("convergence");
		let third = Device::new(&mut rng, first.log.space());
		let mut devices = [first, second, third];
		let entity = rng.entity_id();
		let mut wall = start();
		for step in steps {
			wall += 1_000;
			let log = &devices[step.device].log;
			let value = Value::Integer(i64::from(step.value));
			let operation = if step.replace {
				set(&mut rng, log, entity, "org.example.x", value)
			} else {
				add(&mut rng, entity, "org.example.x", value)
			};
			devices[step.device].commit(&key, vec![operation], wall, &mut rng);
			if let Some(source) = step.sync_from.filter(|&source| source != step.device) {
				let (to, from) = pick(&mut devices, step.device, source);
				to.receive_from(from, &key, wall);
			}
		}
		// Everyone receives everything, in the order of change IDs.
		for to in 0..3 {
			for from in 0..3 {
				if to != from {
					let (to, from) = pick(&mut devices, to, from);
					to.receive_from(from, &key, wall);
				}
			}
		}
		prop_assert_eq!(devices[0].log.state(), devices[1].log.state());
		prop_assert_eq!(devices[1].log.state(), devices[2].log.state());
		prop_assert_eq!(devices[0].log.heads().infallible(), devices[2].log.heads().infallible());
	}
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
	fn changes_payloads_and_keys_in_any_order_give_one_state(
		steps in prop::collection::vec((any::<u8>(), any::<bool>()), 1..8),
		shuffle in prop::collection::vec(any::<u64>(), 24),
	) {
		let (mut rng, keys, mut phone, mut laptop) = setup("parts in any order");
		let entity = rng.entity_id();
		let mut wall = start();
		for (value, replace) in steps {
			wall += 1_000;
			let value = Value::Integer(i64::from(value));
			let operation = if replace {
				set(&mut rng, &phone.log, entity, "org.example.x", value)
			} else {
				add(&mut rng, entity, "org.example.x", value)
			};
			phone.commit(&keys, vec![operation], wall, &mut rng);
		}
		let mut parts = Vec::new();
		for parcel in phone.log.parcels().infallible() {
			let id = parcel.id();
			parts.push(Part::Payload(id, parcel.payload.clone().expect("a payload")));
			parts.push(Part::Key(id, parcel.keys[0].clone()));
			parts.push(Part::Header(Box::new(alone(&parcel))));
		}
		// Shuffled by the generated numbers; parts of changes that the laptop does not hold
		// yet are sent again in the next round.
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
				match &part {
					Part::Header(parcel) => {
						laptop.log.receive((**parcel).clone(), &keys, wall).expect("accepts");
					}
					Part::Payload(id, payload) => {
						if laptop.log.receive_payload(*id, payload.clone(), &keys)
							== Err(LogError::UnknownChange)
						{
							later.push(part);
						}
					}
					Part::Key(id, key) => {
						if laptop.log.receive_change_key(*id, key.clone(), &keys)
							== Err(LogError::UnknownChange)
						{
							later.push(part);
						}
					}
				}
			}
			pending = later;
		}
		prop_assert_eq!(laptop.log.state(), phone.log.state());
		prop_assert_eq!(laptop.log.heads().infallible(), phone.log.heads().infallible());
	}
}

/// Borrows one device mutably and another immutably.
fn pick(devices: &mut [Device; 3], to: usize, from: usize) -> (&mut Device, &Device) {
	assert_ne!(to, from);
	if to < from {
		let (left, right) = devices.split_at_mut(from);
		(&mut left[to], &right[0])
	} else {
		let (left, right) = devices.split_at_mut(to);
		(&mut right[0], &left[from])
	}
}

#[test]
fn ancestry_follows_the_parents_however_deep_the_history() {
	let (mut rng, keys, mut phone, mut laptop) = setup("deep ancestry");
	let ada = rng.entity_id();
	let first = add(&mut rng, ada, "org.example.note", text("first"));
	let first = phone.commit(&keys, vec![first], start(), &mut rng);
	laptop.receive_from(&phone, &keys, start());
	// The phone's branch grows long; the laptop's, concurrent, a little.
	for n in 1..40 {
		let note = add(&mut rng, ada, "org.example.note", Value::Integer(n));
		phone.commit(
			&keys,
			vec![note],
			start() + 1_000 * u64::try_from(n).expect("small"),
			&mut rng,
		);
	}
	let elsewhere = add(&mut rng, ada, "org.example.note", text("elsewhere"));
	let elsewhere = laptop.commit(&keys, vec![elsewhere], start() + 500, &mut rng);
	phone.receive_from(&laptop, &keys, start() + 50_000);

	let purging = |purged: nibkeep_log::ChangeId, rng: &mut TestRng| {
		let heads = phone.log.heads().infallible();
		let mut draft = Draft::new(
			heads,
			Timestamp::new(start() + 60_000, 0),
			Payload::default(),
		);
		draft.purges.changes.insert(purged);
		Change::create(phone.log.space(), &keys, &phone.key, &draft, rng)
	};
	// Both are ancestors of a change made after them.
	let reaches_far_back = purging(first.id(), &mut rng);
	let reaches_the_other_branch = purging(elsewhere.id(), &mut rng);
	for parcel in [reaches_far_back, reaches_the_other_branch] {
		let mut fresh = Device::new(&mut rng, phone.log.space());
		fresh.receive_from(&phone, &keys, start() + 60_000);
		assert!(fresh.log.receive(parcel, &keys, start() + 60_000).is_ok());
	}

	// A change on the laptop's branch alone does not build on the phone's long branch.
	let phone_head = *phone
		.log
		.heads()
		.infallible()
		.iter()
		.find(|&&head| head != elsewhere.id())
		.expect("the phone's head");
	let mut draft = Draft::new(
		[elsewhere.id()],
		Timestamp::new(start() + 70_000, 0),
		Payload::default(),
	);
	draft.purges.changes.insert(phone_head);
	let not_an_ancestor = Change::create(phone.log.space(), &keys, &laptop.key, &draft, &mut rng);
	assert_eq!(
		phone.log.receive(not_an_ancestor, &keys, start() + 70_000),
		Err(LogError::InvalidPurge)
	);
}
