//! Ordered lists: position keys, the order of items, and moves that merge.

use nibkeep_model::storage::Infallibly;
use std::collections::BTreeSet;

use crate::common::{Device, add, set};
use nibkeep_base::EntityId;
use nibkeep_log::{SpaceKeys, list};
use nibkeep_model::{FieldDef, Value};
use nibkeep_testkit::{TestRng, millis_at};
use proptest::prelude::*;

fn start() -> u64 {
	millis_at("2026-10-10", 10, 0)
}

#[test]
fn position_keys_lie_between_their_neighbours() {
	let first = list::position_between(None, None).expect("a key");
	let after = list::position_between(Some(&first), None).expect("a key");
	let before = list::position_between(None, Some(&first)).expect("a key");
	assert!(before < first && first < after);
	for (low, high) in [
		("F", "G"),
		("F", "FV"),
		("Fz", "G"),
		("1", "2"),
		("zz", "zzV"),
	] {
		let middle = list::position_between(Some(low), Some(high)).expect("a key");
		assert!(
			low < middle.as_str() && middle.as_str() < high,
			"{low} < {middle} < {high}"
		);
		assert!(list::is_position(&middle));
	}
	// No key lies between a key and itself, or above a greater one, and invalid keys have
	// no neighbours.
	for (low, high) in [
		(Some("V"), Some("V")),
		(Some("W"), Some("V")),
		(Some("V0"), None),
		(Some(""), None),
		(Some("\u{e9}"), None),
		(None, Some("a b")),
	] {
		assert_eq!(list::position_between(low, high), None, "{low:?} {high:?}");
	}
}

proptest! {
	#[test]
	fn inserting_anywhere_keeps_keys_valid_and_ordered(
		places in prop::collection::vec(any::<prop::sample::Index>(), 1..200),
	) {
		let mut keys: Vec<String> = Vec::new();
		for place in places {
			let index = place.index(keys.len() + 1);
			let low = index.checked_sub(1).map(|before| keys[before].as_str());
			let high = keys.get(index).map(String::as_str);
			let key = list::position_between(low, high).expect("room between");
			prop_assert!(list::is_position(&key));
			keys.insert(index, key);
		}
		prop_assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
	}
}

/// A space with two devices that share a list of `count` items.
struct Shared {
	rng: TestRng,
	key: SpaceKeys,
	phone: Device,
	tablet: Device,
	list: EntityId,
	items: Vec<EntityId>,
}

fn shared_list(seed: &str, count: usize) -> Shared {
	let mut rng = TestRng::new(seed);
	let space = rng.space_id();
	let key = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let mut tablet = Device::new(&mut rng, space);
	let list = rng.entity_id();
	let mut items = Vec::new();
	for index in 0..count {
		let item = rng.entity_id();
		let (_, operations) =
			list::insert(phone.log.state(), list, item, index, &mut rng).infallible();
		phone.commit(&key, operations, start(), &mut rng);
		items.push(item);
	}
	tablet.receive_from(&phone, &key, start());
	Shared {
		rng,
		key,
		phone,
		tablet,
		list,
		items,
	}
}

/// The items of a list on a device, in order.
fn order(device: &Device, list: EntityId) -> Vec<EntityId> {
	list::members(device.log.state(), list)
		.infallible()
		.iter()
		.filter_map(|member| member.item)
		.collect()
}

/// The membership of an item.
fn membership(device: &Device, list: EntityId, item: EntityId) -> EntityId {
	list::members(device.log.state(), list)
		.infallible()
		.iter()
		.find(|member| member.item == Some(item))
		.expect("the item is in the list")
		.membership
}

/// Moves an item to `index` on a device.
fn move_item(shared: &mut Shared, on_tablet: bool, item: EntityId, index: usize, wall: u64) {
	let device = if on_tablet {
		&mut shared.tablet
	} else {
		&mut shared.phone
	};
	let membership = membership(device, shared.list, item);
	let operations = list::move_to(
		device.log.state(),
		shared.list,
		membership,
		index,
		&mut shared.rng,
	)
	.infallible();
	device.commit(&shared.key, operations, wall, &mut shared.rng);
}

#[test]
fn items_are_ordered_by_their_positions() {
	let mut shared = shared_list("order", 3);
	let [a, b, c] = shared.items[..] else {
		panic!("three items");
	};
	assert_eq!(order(&shared.phone, shared.list), [a, b, c]);
	let d = shared.rng.entity_id();
	let (_, operations) =
		list::insert(shared.phone.log.state(), shared.list, d, 1, &mut shared.rng).infallible();
	shared
		.phone
		.commit(&shared.key, operations, start() + 1_000, &mut shared.rng);
	assert_eq!(order(&shared.phone, shared.list), [a, d, b, c]);
}

#[test]
fn moving_an_item_changes_only_its_own_position() {
	let mut shared = shared_list("move", 3);
	let [a, b, c] = shared.items[..] else {
		panic!("three items");
	};
	let membership = membership(&shared.phone, shared.list, c);
	let operations = list::move_to(
		shared.phone.log.state(),
		shared.list,
		membership,
		0,
		&mut shared.rng,
	)
	.infallible();
	assert_eq!(operations.len(), 1);
	shared
		.phone
		.commit(&shared.key, operations, start() + 1_000, &mut shared.rng);
	assert_eq!(order(&shared.phone, shared.list), [c, a, b]);
}

#[test]
fn only_memberships_are_items() {
	let mut shared = shared_list("only memberships", 1);
	let impostor = shared.rng.entity_id();
	let reference = add(
		&mut shared.rng,
		impostor,
		"org.nibkeep.list",
		Value::Reference(shared.list),
	);
	shared.phone.commit(
		&shared.key,
		vec![reference],
		start() + 1_000,
		&mut shared.rng,
	);
	assert_eq!(order(&shared.phone, shared.list), shared.items);
}

#[test]
fn deleted_memberships_leave_the_list() {
	let mut shared = shared_list("delete", 3);
	let [a, b, c] = shared.items[..] else {
		panic!("three items");
	};
	let membership = membership(&shared.phone, shared.list, b);
	let delete = add(
		&mut shared.rng,
		membership,
		"org.nibkeep.deleted",
		Value::Boolean(true),
	);
	shared
		.phone
		.commit(&shared.key, vec![delete], start() + 1_000, &mut shared.rng);
	assert_eq!(order(&shared.phone, shared.list), [a, c]);
}

#[test]
fn items_placed_at_one_place_concurrently_make_room_for_items_moved_between_them() {
	let mut shared = shared_list("equal keys", 2);
	let [a, b] = shared.items[..] else {
		panic!("two items");
	};
	// Each device puts a new item between a and b, without seeing the other's.
	let x = shared.rng.entity_id();
	let (_, operations) =
		list::insert(shared.phone.log.state(), shared.list, x, 1, &mut shared.rng).infallible();
	shared
		.phone
		.commit(&shared.key, operations, start() + 1_000, &mut shared.rng);
	let y = shared.rng.entity_id();
	let (_, operations) = list::insert(
		shared.tablet.log.state(),
		shared.list,
		y,
		1,
		&mut shared.rng,
	)
	.infallible();
	shared
		.tablet
		.commit(&shared.key, operations, start() + 1_000, &mut shared.rng);
	shared
		.phone
		.receive_from(&shared.tablet, &shared.key, start() + 2_000);

	// Both got the same key, so their memberships' IDs order them.
	let members = list::members(shared.phone.log.state(), shared.list).infallible();
	assert_eq!(members[1].position, members[2].position);
	let between = [
		members[1].item.expect("an item"),
		members[2].item.expect("an item"),
	];
	assert_eq!(BTreeSet::from(between), BTreeSet::from([x, y]));

	// Moving b between them gives the second of them a new key as well.
	move_item(&mut shared, false, b, 2, start() + 3_000);
	assert_eq!(
		order(&shared.phone, shared.list),
		[a, between[0], b, between[1]]
	);
}

#[test]
fn a_moved_membership_keeps_its_other_fields() {
	let mut shared = shared_list("fields", 2);
	let [a, b] = shared.items[..] else {
		panic!("two items");
	};
	let membership = membership(&shared.phone, shared.list, b);
	let note = set(
		&mut shared.rng,
		&shared.phone.log,
		membership,
		"org.example.note",
		Value::Text("buy two".into()),
	);
	shared
		.phone
		.commit(&shared.key, vec![note], start() + 1_000, &mut shared.rng);
	move_item(&mut shared, false, b, 0, start() + 2_000);
	assert_eq!(order(&shared.phone, shared.list), [b, a]);
	assert_eq!(
		shared
			.phone
			.log
			.state()
			.current(membership, &"org.example.note".parse().expect("a name"))
			.infallible()
			.len(),
		1
	);
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(64))]

	#[test]
	fn concurrent_moves_neither_lose_nor_duplicate_items(
		moves in prop::collection::vec((any::<bool>(), 0..6_usize, 0..6_usize), 1..12),
	) {
		let mut shared = shared_list("concurrent moves", 6);
		let mut wall = start();
		for (on_tablet, which, index) in moves {
			wall += 1_000;
			let item = shared.items[which];
			move_item(&mut shared, on_tablet, item, index, wall);
		}
		shared.phone.receive_from(&shared.tablet, &shared.key, wall);
		shared.tablet.receive_from(&shared.phone, &shared.key, wall);

		let phone_order = order(&shared.phone, shared.list);
		prop_assert_eq!(&phone_order, &order(&shared.tablet, shared.list));
		prop_assert_eq!(phone_order.len(), 6);
		prop_assert_eq!(
			phone_order.iter().copied().collect::<BTreeSet<_>>(),
			shared.items.iter().copied().collect::<BTreeSet<_>>()
		);
	}
}

#[test]
fn the_membership_type_marks_its_roles() {
	let membership = list::membership_type();
	assert!(membership.field(&list::LIST).is_some_and(FieldDef::is_role));
	assert!(membership.field(&list::ITEM).is_some_and(FieldDef::is_role));
	assert!(
		!membership
			.field(&list::POSITION)
			.is_some_and(FieldDef::is_role)
	);
}
