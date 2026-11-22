//! Groups that share the groups inside them: the routes from a space into its groups multiply,
//! and every one counts ([technical design: groups]). A key in a group that a hundred routes
//! reach has its role through the best of them; and whatever groups hold, share or repeat,
//! every device decides alike.
//!
//! The logs of these tests: a space, log 0, and groups, each followed by the space. The first
//! device, its person's, founds every log as its admin.
//!
//! [technical design: groups]: ../../../docs/Technical-design.md#138-groups

mod common;

use common::{DRIFT, counts_in, pair, send, text, texts};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::{DecryptionKey, EncryptionKey, SigningKey};
use nibkeep_governance::{Anchor, GroupRef, Joining};
use nibkeep_log::{ChangeId, Log, Parcel, SpaceKeys};
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const CAPTION: &str = "org.example.caption";

const SPACE: usize = 0;

const ROLES: [&str; 3] = ["none", "viewer", "editor"];

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

/// A group's record in a log, and the statement that adds it there.
#[derive(Clone, Copy)]
struct GroupRecord {
	log: usize,
	group: usize,
	record: EntityId,
	addition: StatementId,
}

/// The space and its groups, with the devices that hold them, one for each person.
struct Groups {
	rng: TestRng,
	now: u64,
	worlds: Vec<World>,
	devices: Vec<Device>,
	/// A photo of the space's, which the tests caption.
	photo: EntityId,
	/// Every record of a group in a log, in the order they were made.
	records: Vec<GroupRecord>,
}

impl Groups {
	/// A space and `groups` groups, with a device for each of `people` people; the first
	/// founds every log, as its admin. Nobody else is a member yet.
	fn new(seed: &str, groups: usize, people: usize) -> Self {
		let mut rng = TestRng::new(seed);
		let worlds: Vec<World> = (0..=groups)
			.map(|_| World {
				id: rng.space_id(),
				keys: SpaceKeys::new(0, rng.key()),
				root: None,
				members: (0..people).map(|_| rng.entity_id()).collect(),
				entity: rng.entity_id(),
			})
			.collect();
		let devices = (0..people)
			.map(|person| Device {
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
		let mut this = Self {
			rng,
			now: millis_at("2026-11-16", 9, 0),
			worlds,
			devices,
			photo,
			records: Vec::new(),
		};
		for log in 0..=groups {
			let mut founding = this.making_member(log, 0, "admin");
			founding.extend(this.listing(log, 0));
			let parcel = this.commit(log, 0, founding);
			this.worlds[log].root = Some(parcel.id());
		}
		this
	}

	fn logs(&self) -> usize {
		self.worlds.len()
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn assert(&mut self, entity: EntityId, field: FieldName, value: Value) -> Operation {
		common::assert(&mut self.rng, entity, field, value)
	}

	fn making_member(&mut self, log: usize, person: usize, role: &str) -> Vec<Operation> {
		common::making_member(&mut self.rng, self.worlds[log].members[person], role)
	}

	fn listing(&mut self, log: usize, device: usize) -> Vec<Operation> {
		let (world, device) = (&self.worlds[log], &self.devices[device]);
		let member = (world.members[device.person], false);
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

	/// The first device makes device `device`'s person a member of a log with `role`, and
	/// the device then holds what the first has of it.
	fn invite(&mut self, log: usize, device: usize, role: &str) {
		let mut operations = self.making_member(log, self.devices[device].person, role);
		operations.extend(self.listing(log, device));
		self.commit(log, 0, operations);
		self.sync(log, 0, device);
	}

	/// The first device gives a record of a log a role.
	fn give_role(&mut self, log: usize, record: EntityId, role: &str) -> Parcel {
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(log, 0, vec![operation])
	}

	/// The first device adds a group to a log with `role`, by a new record; the group agrees
	/// with `agree`.
	fn add_group(&mut self, log: usize, group: usize, role: &str, agree: bool) -> GroupRecord {
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
		self.commit(log, 0, operations);
		let made = GroupRecord {
			log,
			group,
			record,
			addition,
		};
		self.records.push(made);
		if agree {
			self.agree(made, fields::JOINED);
		}
		made
	}

	/// The first device states, in a record's group, its agreement to the addition, or its
	/// withdrawal, as `field` says.
	fn agree(&mut self, record: GroupRecord, field: FieldName) {
		let joining = Joining {
			space: self.worlds[record.log].id,
			root: self.worlds[record.log].root.expect("founded"),
			addition: record.addition,
		};
		let entity = self.worlds[record.group].entity;
		let operation = self.assert(entity, field, Value::Text(joining.to_text()));
		self.commit(record.group, 0, vec![operation]);
	}

	/// Device `by` anchors, in the space, a group's changes that it holds.
	fn anchor(&mut self, group: usize, by: usize) -> Parcel {
		self.follow(by);
		let changes = self.devices[by].logs[group]
			.followed(&self.worlds[group].keys)
			.expect("reads the group")
			.heads();
		let anchor = Anchor {
			group: self.worlds[group].id,
			changes,
		};
		let entity = self.worlds[SPACE].entity;
		let operation = self.assert(entity, fields::ANCHOR, Value::Text(anchor.to_text()));
		self.commit(SPACE, by, vec![operation])
	}

	/// The first device anchors every group in the space.
	fn anchor_all(&mut self) {
		for group in 1..self.logs() {
			self.anchor(group, 0);
		}
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> Parcel {
		let operation = Operation::assert(Statement::new(
			self.rng.statement_id(),
			self.photo,
			FieldName::from_static(CAPTION),
			text(caption),
		));
		self.commit(SPACE, by, vec![operation])
	}

	/// Device `of`'s copy of the space follows every group, as its copies of their logs have
	/// them.
	fn follow(&mut self, of: usize) {
		let keys = self.worlds[SPACE].keys.clone();
		let device = &self.devices[of];
		let followed = (1..self.logs())
			.map(|group| {
				device.logs[group]
					.followed(&self.worlds[group].keys)
					.expect("reads the group")
			})
			.collect();
		self.devices[of].logs[SPACE]
			.follow(followed, &keys)
			.expect("follows the groups");
	}

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

	/// Device `to` receives what device `from` holds of a log; for the space, of every group
	/// first, following them before and after it takes the space's changes.
	fn sync(&mut self, log: usize, from: usize, to: usize) {
		if log == SPACE {
			for group in 1..self.logs() {
				self.sync_one(group, from, to);
			}
			self.follow(to);
		}
		self.sync_one(log, from, to);
		if log == SPACE {
			self.follow(to);
		}
	}

	fn sync_all(&mut self) {
		let count = self.devices.len();
		for _ in 0..2 {
			for from in 0..count {
				for to in 0..count {
					if from != to {
						self.sync(SPACE, from, to);
					}
				}
			}
		}
	}

	/// Whether a change of the space counts on every device that holds it, which must agree.
	fn counts(&self, change: &Parcel) -> bool {
		counts_in(
			self.devices.iter().map(|device| &device.logs[SPACE]),
			change,
		)
	}

	fn captions(&self, of: usize) -> Vec<String> {
		let mut captions = texts(&self.devices[of].logs[SPACE], self.photo, CAPTION);
		captions.sort();
		captions
	}

	fn pick(&mut self, count: usize) -> usize {
		usize::from(self.rng.space_id().as_bytes()[0]) % count
	}
}

/// A ladder of diamonds below a group in the space: each rung, two groups, both holding the
/// next, so that the routes to the last double at every rung, as the groups of a large family
/// share their children's.
fn diamonds(seed: &str, rungs: usize, people: usize) -> Groups {
	let mut groups = Groups::new(seed, 1 + 3 * rungs, people);
	groups.add_group(SPACE, 1, "editor", true);
	let mut top = 1;
	for rung in 0..rungs {
		let (left, right, below) = (2 + 3 * rung, 3 + 3 * rung, 4 + 3 * rung);
		for side in [left, right] {
			groups.add_group(top, side, "editor", true);
			groups.add_group(side, below, "editor", true);
		}
		top = below;
	}
	groups
}

#[test]
fn a_key_in_a_group_that_many_routes_reach_counts_through_the_best_of_them() {
	// Seven rungs: 128 routes to the last group, and more to those above, with the son an
	// editor of the last.
	let rungs = 7;
	let mut groups = diamonds("many routes", rungs, 2);
	let last = 1 + 3 * rungs;
	groups.invite(last, 1, "editor");
	groups.anchor_all();
	groups.sync_all();
	let caption = groups.caption(1, "from the last rung");
	groups.sync_all();
	assert!(groups.counts(&caption));
	// Every route through the left side of the second rung is a viewer's now; those through
	// the right still give an editor's.
	let left = groups.records[6];
	assert_eq!((left.log, left.group), (5, 7));
	groups.give_role(left.log, left.record, "viewer");
	groups.anchor_all();
	groups.sync_all();
	let still = groups.caption(1, "through the right");
	groups.sync_all();
	assert!(groups.counts(&still));
	// The right side's record too: no route gives more than a viewer's.
	let right = groups.records[8];
	assert_eq!((right.log, right.group), (6, 7));
	groups.give_role(right.log, right.record, "viewer");
	groups.anchor_all();
	groups.sync_all();
	let viewer = groups.caption(1, "a viewer's");
	groups.sync_all();
	assert!(!groups.counts(&viewer));
}

#[test]
fn a_key_that_a_million_routes_reach_counts() {
	// Twenty rungs: over a million routes to the last group, which no replica lists.
	let rungs = 20;
	let mut groups = diamonds("a million routes", rungs, 2);
	let last = 1 + 3 * rungs;
	groups.invite(last, 1, "editor");
	groups.anchor_all();
	groups.sync_all();
	let caption = groups.caption(1, "from the last rung");
	groups.sync(SPACE, 1, 0);
	assert!(groups.counts(&caption));
}

#[test]
fn every_device_decides_alike_with_groups_that_share_repeat_and_hold_each_other() {
	for seed in 0..12 {
		let mut groups = Groups::new(&format!("shared {seed}"), 3 + seed % 4, 4);
		let logs = groups.logs();
		// Each person but the first is a member of two groups, and of the space only through
		// them.
		for device in 1..4 {
			for _ in 0..2 {
				let group = 1 + groups.pick(logs - 1);
				let role = ROLES[1 + groups.pick(2)];
				groups.invite(group, device, role);
			}
		}
		// Groups in the space and inside each other, some twice, some in a cycle.
		for _ in 0..10 {
			let log = groups.pick(logs);
			let group = 1 + groups.pick(logs - 1);
			if group != log {
				let role = ROLES[1 + groups.pick(2)];
				let agree = groups.pick(6) != 0;
				groups.add_group(log, group, role, agree);
			}
		}
		groups.anchor_all();
		groups.sync_all();
		let mut captions = Vec::new();
		for step in 0..40 {
			let device = groups.pick(4);
			match groups.pick(10) {
				0..=2 => captions.push(groups.caption(device, &format!("step {step}"))),
				3 if !groups.records.is_empty() => {
					// A record of a group gets another role, or its group withdraws or agrees
					// again.
					let count = groups.records.len();
					let at = groups.pick(count);
					let record = groups.records[at];
					match groups.pick(6) {
						0 => groups.agree(record, fields::WITHDRAWN),
						1 => groups.agree(record, fields::JOINED),
						_ => {
							let role = ROLES[groups.pick(3)];
							groups.give_role(record.log, record.record, role);
						}
					}
				}
				4 => {
					let (group, person) = (1 + groups.pick(logs - 1), 1 + groups.pick(3));
					let record = groups.worlds[group].members[person];
					let role = ROLES[groups.pick(3)];
					groups.give_role(group, record, role);
				}
				5 => {
					let group = 1 + groups.pick(logs - 1);
					groups.anchor(group, device);
				}
				6 => {
					let (log, group) = (groups.pick(logs), 1 + groups.pick(logs - 1));
					if group != log {
						let role = ROLES[1 + groups.pick(2)];
						groups.add_group(log, group, role, true);
					}
				}
				_ => {
					let other = (device + 1 + groups.pick(3)) % 4;
					let log = groups.pick(logs);
					groups.sync(log, device, other);
				}
			}
		}
		groups.sync_all();
		for caption in &captions {
			groups.counts(caption);
		}
		let shown = groups.captions(0);
		for device in 1..4 {
			assert_eq!(groups.captions(device), shown, "seed {seed}");
		}
	}
}
