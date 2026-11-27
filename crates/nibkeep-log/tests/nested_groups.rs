//! Groups inside groups: a group is a member of a space or of another group, and a space
//! follows the groups at every level, whose changes take effect there at the space's own
//! anchors that name them ([technical design: groups]). Through a group inside a group, a
//! person has the lowest role on the path; a removal at any level takes effect in the space at
//! its first anchors; and groups that hold each other give nothing through the cycle.
//!
//! The logs of these tests: the family's space, the household's group in it, and the kids'
//! group in the household. The people: the mother, an admin of all three; her son, an editor
//! of the kids alone; the grandmother, a viewer of the family alone; and the father, a member
//! of the household.
//!
//! [technical design: groups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups

use crate::common;

use std::collections::BTreeMap;

use crate::common::{DRIFT, counts_in, pair, send, text, texts};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::{DecryptionKey, EncryptionKey, SigningKey};
use nibkeep_governance::{Anchor, GroupRef, Joining};
use nibkeep_log::{ChangeId, Log, Parcel, SpaceKeys};
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const CAPTION: &str = "org.example.caption";

const EDITOR: &str = "editor";
const VIEWER: &str = "viewer";
const NONE: &str = "none";

/// The logs: the family's space, the household's group, and the kids' group.
const FAMILY: usize = 0;
const HOUSEHOLD: usize = 1;
const KIDS: usize = 2;
const LOGS: usize = 3;

/// The people, each with a record in every log.
const MOTHER: usize = 0;
const SON: usize = 1;
const GRANDMA: usize = 2;
const FATHER: usize = 3;
const PEOPLE: usize = 4;

/// A device of one person's, with its keys and its copy of every log.
struct Device {
	key: SigningKey,
	encryption: EncryptionKey,
	person: usize,
	logs: Vec<Log>,
}

/// A space or a group: its ID, its keys, its first change, each person's record in it, and
/// an entity that anchors, agreements and withdrawals are stated on.
struct World {
	id: SpaceId,
	keys: SpaceKeys,
	root: Option<ChangeId>,
	members: Vec<EntityId>,
	entity: EntityId,
}

/// The three logs, with the devices that hold them. Device 0, the mother's, founds each.
struct Nested {
	rng: TestRng,
	now: u64,
	worlds: Vec<World>,
	devices: Vec<Device>,
	/// A photo of the family's, which the tests caption.
	photo: EntityId,
	/// Each group's record in a log, and the statement that adds it, by the log and the
	/// group.
	additions: BTreeMap<(usize, usize), (EntityId, StatementId)>,
	/// The groups that each log follows, at every level.
	following: [Vec<usize>; LOGS],
}

impl Nested {
	/// Devices of the people that `people` names, one for each; the first, the mother's,
	/// founds the three logs, as their admin. Nobody else is a member yet.
	fn new(seed: &str, people: &[usize]) -> Self {
		assert_eq!(people[0], MOTHER);
		let mut rng = TestRng::new(seed);
		let worlds: Vec<World> = (0..LOGS)
			.map(|_| World {
				id: rng.space_id(),
				keys: SpaceKeys::new(0, rng.key()),
				root: None,
				members: (0..PEOPLE).map(|_| rng.entity_id()).collect(),
				entity: rng.entity_id(),
			})
			.collect();
		let devices = people
			.iter()
			.map(|&person| Device {
				key: rng.signing_key(),
				encryption: DecryptionKey::generate(&mut rng).encryption_key(),
				person,
				logs: worlds
					.iter()
					.map(|world| {
						let mut log = Log::new(world.id, Clock::new(DRIFT));
						log.expect_governed();
						log
					})
					.collect(),
			})
			.collect();
		let photo = rng.entity_id();
		let mut nested = Self {
			rng,
			now: millis_at("2026-11-07", 9, 0),
			worlds,
			devices,
			photo,
			additions: BTreeMap::new(),
			following: [vec![HOUSEHOLD, KIDS], vec![KIDS], Vec::new()],
		};
		for log in 0..LOGS {
			let mut founding = nested.making_member(log, MOTHER, "admin");
			founding.extend(nested.listing(log, 0, false));
			let parcel = nested.commit(log, 0, founding);
			nested.worlds[log].root = Some(parcel.id());
		}
		nested
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn assert(&mut self, entity: EntityId, field: FieldName, value: Value) -> Operation {
		common::assert(&mut self.rng, entity, field, value)
	}

	/// The governing operations that make a person's record in a log, with a user ID and a
	/// role.
	fn making_member(&mut self, log: usize, person: usize, role: &str) -> Vec<Operation> {
		common::making_member(&mut self.rng, self.worlds[log].members[person], role)
	}

	/// The governing operations that list a device's key in a log, owned by its person's
	/// record, with the listing that its key signed; marked as an AI agent's with
	/// `ai_agent`.
	fn listing(&mut self, log: usize, device: usize, ai_agent: bool) -> Vec<Operation> {
		let (world, device) = (&self.worlds[log], &self.devices[device]);
		let member = (world.members[device.person], ai_agent);
		common::listing(
			&mut self.rng,
			world.id,
			(&device.key, &device.encryption),
			member,
		)
	}

	fn commit(&mut self, log: usize, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.worlds[log].keys.clone();
		let device = &mut self.devices[device];
		let key = device.key.clone();
		device.logs[log]
			.commit(&key, &keys, operations, wall, &mut self.rng)
			.expect("commits")
	}

	/// Device `by` invites device `device`'s person to a log with `role`, and the device then
	/// holds what `by` has.
	fn invite(&mut self, log: usize, by: usize, device: usize, role: &str) -> Parcel {
		let mut operations = self.making_member(log, self.devices[device].person, role);
		operations.extend(self.listing(log, device, false));
		let parcel = self.commit(log, by, operations);
		self.sync(log, by, device);
		parcel
	}

	/// Device `by` gives a person a role in a log.
	fn give_role(&mut self, log: usize, by: usize, person: usize, role: &str) -> Parcel {
		let record = self.worlds[log].members[person];
		self.give_role_to(log, by, record, role)
	}

	/// Device `by` gives a record of a log a role.
	fn give_role_to(&mut self, log: usize, by: usize, record: EntityId, role: &str) -> Parcel {
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(log, by, vec![operation])
	}

	/// Device `by` adds a group to a log with `role`: a record for the group, with the
	/// statement that adds it.
	fn add_group(&mut self, log: usize, group: usize, by: usize, role: &str) -> Parcel {
		let record = self.rng.entity_id();
		let world = &self.worlds[group];
		let reference = GroupRef {
			group: world.id,
			root: world.root.expect("founded"),
			key: nibkeep_crypto::key_hash(world.keys.space_keys().next().expect("a key").1),
		};
		let addition = self.rng.statement_id();
		let operations = vec![
			Operation::assert(Statement::new(
				addition,
				record,
				fields::GROUP,
				Value::Text(reference.to_text()),
			)),
			self.assert(record, fields::ROLE, Value::Text(role.to_owned())),
		];
		self.additions.insert((log, group), (record, addition));
		self.commit(log, by, operations)
	}

	/// A group's record in a log.
	fn record(&self, log: usize, group: usize) -> EntityId {
		self.additions[&(log, group)].0
	}

	/// The joining that an agreement for a group's addition to a log names.
	fn joining(&self, log: usize, group: usize) -> Joining {
		Joining {
			space: self.worlds[log].id,
			root: self.worlds[log].root.expect("founded"),
			addition: self.additions[&(log, group)].1,
		}
	}

	/// Device `by` agrees, in a group, to its addition to a log.
	fn agree(&mut self, group: usize, log: usize, by: usize) -> Parcel {
		let (entity, joining) = (self.worlds[group].entity, self.joining(log, group));
		let operation = self.assert(entity, fields::JOINED, Value::Text(joining.to_text()));
		self.commit(group, by, vec![operation])
	}

	/// Device `by` withdraws a group's agreement to its addition to a log.
	fn withdraw(&mut self, group: usize, log: usize, by: usize) -> Parcel {
		let (entity, joining) = (self.worlds[group].entity, self.joining(log, group));
		let operation = self.assert(entity, fields::WITHDRAWN, Value::Text(joining.to_text()));
		self.commit(group, by, vec![operation])
	}

	/// Device `by` anchors, in a log, a group's changes that it holds.
	fn anchor(&mut self, log: usize, group: usize, by: usize) -> Parcel {
		self.follow(by);
		let changes = self.devices[by].logs[group]
			.followed(&self.worlds[group].keys)
			.expect("reads the group")
			.heads();
		let anchor = Anchor {
			group: self.worlds[group].id,
			changes,
		};
		let entity = self.worlds[log].entity;
		let operation = self.assert(entity, fields::ANCHOR, Value::Text(anchor.to_text()));
		self.commit(log, by, vec![operation])
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> Parcel {
		let operation = Operation::assert(Statement::new(
			self.rng.statement_id(),
			self.photo,
			FieldName::from_static(CAPTION),
			text(caption),
		));
		self.commit(FAMILY, by, vec![operation])
	}

	/// Device `of` follows, in each log, the groups at every level below it, as its copies of
	/// their logs have them.
	fn follow(&mut self, of: usize) {
		for log in 0..LOGS {
			let keys = self.worlds[log].keys.clone();
			let device = &self.devices[of];
			let followed = self.following[log]
				.iter()
				.map(|&group| {
					device.logs[group]
						.followed(&self.worlds[group].keys)
						.expect("reads the group")
				})
				.collect();
			self.devices[of].logs[log]
				.follow(followed, &keys)
				.expect("follows the groups");
		}
	}

	/// Device `to` receives every change of device `from`'s copy of one log, parents first, as
	/// a sync sends them. Changes of keys it knows no listing of are refused, as a sync drops
	/// them.
	fn sync_one(&mut self, log: usize, from: usize, to: usize) {
		let wall = self.tick();
		let (sender, receiver) = pair(&mut self.devices, from, to);
		send(
			&sender.logs[log],
			&mut receiver.logs[log],
			&self.worlds[log].keys,
			wall,
		);
	}

	/// Device `to` receives what device `from` holds of a log, and, first, of the groups it
	/// follows, as a sync carries them, following them before it takes the log's changes.
	fn sync(&mut self, log: usize, from: usize, to: usize) {
		for group in self.following[log].clone() {
			self.sync_one(group, from, to);
		}
		self.follow(to);
		self.sync_one(log, from, to);
		self.follow(to);
	}

	/// Every device receives what every other has, of every log.
	fn sync_all(&mut self) {
		let count = self.devices.len();
		for _ in 0..2 {
			for from in 0..count {
				for to in 0..count {
					if from != to {
						for log in [KIDS, HOUSEHOLD, FAMILY] {
							self.sync(log, from, to);
						}
					}
				}
			}
		}
	}

	/// Whether a change of a log counts on every device that holds it, which must agree.
	fn counts_in(&self, log: usize, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.logs[log]), change)
	}

	/// Whether a change of the family's space counts.
	fn counts(&self, change: &Parcel) -> bool {
		self.counts_in(FAMILY, change)
	}

	/// The captions that a device shows, sorted.
	fn captions(&self, of: usize) -> Vec<String> {
		let mut captions = texts(&self.devices[of].logs[FAMILY], self.photo, CAPTION);
		captions.sort();
		captions
	}

	/// Whether the family's space ends a device's replica, as device `on` decides it.
	fn is_removed(&self, on: usize, device: usize) -> bool {
		self.devices[on].logs[FAMILY]
			.is_removed(
				&self.devices[device].key.verifying_key(),
				&self.worlds[FAMILY].keys,
			)
			.expect("reads the space")
	}

	/// The changes of the family's space that a new key must build on, as device `on` has
	/// them.
	fn removals(&self, on: usize) -> Vec<ChangeId> {
		self.devices[on].logs[FAMILY]
			.removals(&self.worlds[FAMILY].keys)
			.expect("reads the space")
	}
}

/// The kids, with the son as an editor, in the household with `kids_role`, and the household
/// in the family with `household_role`; the grandmother a viewer of the family if she has a
/// device; each group agreed by the mother, and every level anchored by her.
fn kids_in_household_in_family(
	seed: &str,
	people: &[usize],
	kids_role: &str,
	household_role: &str,
) -> Nested {
	let mut nested = Nested::new(seed, people);
	nested.invite(KIDS, 0, 1, EDITOR);
	if let Some(grandma) = people.iter().position(|&person| person == GRANDMA) {
		nested.invite(FAMILY, 0, grandma, VIEWER);
	}
	nested.add_group(FAMILY, HOUSEHOLD, 0, household_role);
	nested.agree(HOUSEHOLD, FAMILY, 0);
	nested.add_group(HOUSEHOLD, KIDS, 0, kids_role);
	nested.agree(KIDS, HOUSEHOLD, 0);
	nested.anchor(HOUSEHOLD, KIDS, 0);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.anchor(FAMILY, KIDS, 0);
	nested.sync_all();
	nested
}

#[test]
fn a_group_inside_a_group_gives_the_space_once_every_level_agrees_and_its_anchors_name_them() {
	let mut nested = Nested::new("every level", &[MOTHER, SON, GRANDMA]);
	nested.invite(KIDS, 0, 1, EDITOR);
	nested.invite(FAMILY, 0, 2, VIEWER);
	nested.add_group(FAMILY, HOUSEHOLD, 0, EDITOR);
	nested.agree(HOUSEHOLD, FAMILY, 0);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	// The household is in the family; the kids are in nothing yet.
	let alone = nested.caption(1, "the kids in nothing");
	nested.add_group(HOUSEHOLD, KIDS, 0, EDITOR);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	let unagreed = nested.caption(1, "before the kids agree");
	nested.agree(KIDS, HOUSEHOLD, 0);
	nested.anchor(HOUSEHOLD, KIDS, 0);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	// The household's own anchors name the kids' agreement, but the family's do not yet.
	let unanchored = nested.caption(1, "before the family anchors the kids");
	nested.sync_all();
	// The grandmother, a member of the family alone, anchors the kids there.
	let anchoring = nested.anchor(FAMILY, KIDS, 2);
	nested.sync_all();
	let late = nested.caption(1, "once the family's anchors name every level");
	nested.sync_all();
	assert!(nested.counts(&anchoring));
	assert!(nested.counts(&late));
	for early in [&alone, &unagreed, &unanchored] {
		assert!(!nested.counts(early));
	}
	assert_eq!(
		nested.captions(2),
		["once the family's anchors name every level"]
	);
	let listed = nested.devices[2].logs[FAMILY]
		.listed_through_groups(&nested.worlds[FAMILY].keys)
		.expect("reads the space");
	assert!(listed.contains(&nested.devices[1].key.verifying_key()));
}

#[test]
fn through_groups_inside_groups_a_person_has_the_lowest_role_on_the_path() {
	let mut nested = kids_in_household_in_family("lowest role", &[MOTHER, SON], VIEWER, EDITOR);
	let as_viewers = nested.caption(1, "the kids as viewers of the household");
	nested.sync_all();
	assert!(!nested.counts(&as_viewers));
	let kids = nested.record(HOUSEHOLD, KIDS);
	nested.give_role_to(HOUSEHOLD, 0, kids, EDITOR);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	let as_editors = nested.caption(1, "the kids as editors");
	nested.sync_all();
	assert!(nested.counts(&as_editors));
	// A group inside a group is never an admin either.
	let as_admins = nested.give_role_to(HOUSEHOLD, 0, kids, "admin");
	nested.sync_all();
	assert!(!nested.counts_in(HOUSEHOLD, &as_admins));
	// The household becomes a viewer of the family: the path is a viewer's again.
	let household = nested.record(FAMILY, HOUSEHOLD);
	nested.give_role_to(FAMILY, 0, household, VIEWER);
	nested.sync_all();
	let household_viewing = nested.caption(1, "the household as viewers of the family");
	nested.sync_all();
	assert!(!nested.counts(&household_viewing));
}

#[test]
fn a_removal_from_a_group_inside_a_group_takes_effect_in_the_space_at_its_first_anchors() {
	let mut nested = kids_in_household_in_family(
		"removed from the kids",
		&[MOTHER, SON, GRANDMA],
		EDITOR,
		EDITOR,
	);
	// The mother removes the son from the kids. Not knowing, he captions twice; the
	// grandmother gets the first caption, then the removal, and anchors it in the family.
	nested.give_role(KIDS, 0, SON, NONE);
	let first = nested.caption(1, "seen by the first anchor");
	nested.sync(FAMILY, 1, 2);
	let second = nested.caption(1, "not seen by it");
	assert!(
		nested.counts(&second),
		"a removal that no anchor names changes nothing yet"
	);
	assert!(!nested.is_removed(0, 1));
	nested.sync(KIDS, 0, 2);
	let first_anchor = nested.anchor(FAMILY, KIDS, 2);
	nested.sync_all();
	assert!(nested.counts(&first_anchor));
	assert!(nested.counts(&first), "the first anchor builds on it");
	assert!(!nested.counts(&second));
	let after = nested.caption(1, "after");
	nested.sync_all();
	assert!(!nested.counts(&after));
	// His replica is ended in the family, and a new key there builds on the first anchor.
	assert!(nested.is_removed(0, 1));
	assert!(nested.removals(0).contains(&first_anchor.id()));
	assert_eq!(nested.captions(2), ["seen by the first anchor"]);
}

#[test]
fn taking_a_group_out_of_a_group_takes_it_out_of_the_space_at_its_first_anchors() {
	let mut nested =
		kids_in_household_in_family("kids taken out", &[MOTHER, SON, GRANDMA], EDITOR, EDITOR);
	let kids = nested.record(HOUSEHOLD, KIDS);
	nested.give_role_to(HOUSEHOLD, 0, kids, NONE);
	let first = nested.caption(1, "seen by the first anchor");
	nested.sync(FAMILY, 1, 2);
	let second = nested.caption(1, "not seen by it");
	nested.sync(HOUSEHOLD, 0, 2);
	let first_anchor = nested.anchor(FAMILY, HOUSEHOLD, 2);
	nested.sync_all();
	assert!(nested.counts(&first_anchor));
	assert!(nested.counts(&first));
	assert!(!nested.counts(&second));
	assert!(nested.is_removed(0, 1));
	assert!(nested.removals(0).contains(&first_anchor.id()));
}

#[test]
fn a_group_that_withdraws_from_a_group_leaves_every_space_the_group_is_in_for_good() {
	let mut nested = kids_in_household_in_family("kids withdraw", &[MOTHER, SON], EDITOR, EDITOR);
	let before = nested.caption(1, "before the withdrawal");
	nested.sync_all();
	nested.withdraw(KIDS, HOUSEHOLD, 0);
	let anchoring = nested.anchor(FAMILY, KIDS, 0);
	nested.sync_all();
	let after = nested.caption(1, "after the withdrawal");
	nested.sync_all();
	assert!(nested.counts(&before));
	assert!(!nested.counts(&after));
	assert!(nested.is_removed(0, 1));
	assert!(nested.removals(0).contains(&anchoring.id()));
	// Agreeing again brings the kids back into nothing: a withdrawal is for good.
	nested.agree(KIDS, HOUSEHOLD, 0);
	nested.anchor(FAMILY, KIDS, 0);
	nested.sync_all();
	let again = nested.caption(1, "agreed again");
	nested.sync_all();
	assert!(!nested.counts(&again));
}

#[test]
fn groups_that_hold_each_other_give_nothing_through_the_cycle() {
	let mut nested = kids_in_household_in_family("a cycle", &[MOTHER, SON, FATHER], EDITOR, EDITOR);
	nested.invite(HOUSEHOLD, 0, 2, EDITOR);
	// The household joins the kids too, so that each holds the other.
	nested.following = [vec![HOUSEHOLD, KIDS], vec![KIDS], vec![HOUSEHOLD]];
	nested.add_group(KIDS, HOUSEHOLD, 0, EDITOR);
	nested.agree(HOUSEHOLD, KIDS, 0);
	nested.anchor(KIDS, HOUSEHOLD, 0);
	nested.anchor(HOUSEHOLD, KIDS, 0);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.anchor(FAMILY, KIDS, 0);
	nested.sync_all();
	let sons = nested.caption(1, "the son's");
	let fathers = nested.caption(2, "the father's");
	nested.sync_all();
	assert!(nested.counts(&sons));
	assert!(nested.counts(&fathers));
	// Taken out of the household, the kids no longer reach the family, as the household's
	// place in the kids gives nothing back.
	let kids = nested.record(HOUSEHOLD, KIDS);
	nested.give_role_to(HOUSEHOLD, 0, kids, NONE);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	let sons = nested.caption(1, "the son's, out of the household");
	let fathers = nested.caption(2, "the father's, still in it");
	nested.sync_all();
	assert!(!nested.counts(&sons));
	assert!(nested.counts(&fathers));
}

#[test]
fn every_device_decides_alike_after_random_changes_at_every_level_and_syncs() {
	for seed in 0..6 {
		let mut nested = kids_in_household_in_family(
			&format!("random {seed}"),
			&[MOTHER, SON, GRANDMA, FATHER],
			EDITOR,
			EDITOR,
		);
		nested.invite(HOUSEHOLD, 0, 3, EDITOR);
		nested.sync_all();
		let mut captions = Vec::new();
		for step in 0..24 {
			let pick = |nested: &mut Nested, count: usize| {
				usize::from(nested.rng.space_id().as_bytes()[0]) % count
			};
			let device = pick(&mut nested, 4);
			match pick(&mut nested, 7) {
				0 | 1 => captions.push(nested.caption(device, &format!("step {step}"))),
				2 => {
					// The mother gives the son a role in the kids, or the father one in the
					// household.
					let role = [NONE, VIEWER, EDITOR][pick(&mut nested, 3)];
					if pick(&mut nested, 2) == 0 {
						nested.give_role(KIDS, 0, SON, role);
					} else {
						nested.give_role(HOUSEHOLD, 0, FATHER, role);
					}
				}
				3 => {
					// She gives the kids a role in the household.
					let role = [NONE, VIEWER, EDITOR][pick(&mut nested, 3)];
					let kids = nested.record(HOUSEHOLD, KIDS);
					nested.give_role_to(HOUSEHOLD, 0, kids, role);
				}
				4 => {
					let (log, group) = [(FAMILY, HOUSEHOLD), (FAMILY, KIDS), (HOUSEHOLD, KIDS)]
						[pick(&mut nested, 3)];
					nested.anchor(log, group, device);
				}
				_ => {
					let other = (device + 1 + pick(&mut nested, 3)) % 4;
					let log = [FAMILY, HOUSEHOLD, KIDS][pick(&mut nested, 3)];
					nested.sync(log, device, other);
				}
			}
		}
		nested.sync_all();
		for caption in &captions {
			// Every device that holds it decides alike.
			nested.counts(caption);
		}
		let shown = nested.captions(0);
		for device in 1..4 {
			assert_eq!(nested.captions(device), shown, "seed {seed}");
		}
	}
}

#[test]
fn a_group_inside_a_group_whose_record_has_no_role_there_gives_nothing() {
	// The father, an editor of the household and an admin of the kids, puts the kids in the
	// household, which only an admin of the household may do, agrees as the kids' admin, and
	// anchors it all in the family.
	let mut nested = Nested::new("no role inside", &[MOTHER, SON, FATHER]);
	nested.invite(KIDS, 0, 1, EDITOR);
	nested.invite(KIDS, 0, 2, "admin");
	nested.invite(HOUSEHOLD, 0, 2, EDITOR);
	nested.add_group(FAMILY, HOUSEHOLD, 0, EDITOR);
	nested.agree(HOUSEHOLD, FAMILY, 0);
	nested.anchor(FAMILY, HOUSEHOLD, 0);
	nested.sync_all();
	let adding = nested.add_group(HOUSEHOLD, KIDS, 2, EDITOR);
	nested.agree(KIDS, HOUSEHOLD, 2);
	nested.anchor(HOUSEHOLD, KIDS, 2);
	nested.anchor(FAMILY, HOUSEHOLD, 2);
	nested.anchor(FAMILY, KIDS, 2);
	nested.sync_all();
	assert!(!nested.counts_in(HOUSEHOLD, &adding));
	let caption = nested.caption(1, "through a record with no role");
	nested.sync_all();
	assert!(!nested.counts(&caption));
	let listed = nested.devices[0].logs[FAMILY]
		.listed_through_groups(&nested.worlds[FAMILY].keys)
		.expect("reads the space");
	assert!(!listed.contains(&nested.devices[1].key.verifying_key()));
	assert!(listed.contains(&nested.devices[2].key.verifying_key()));
}
