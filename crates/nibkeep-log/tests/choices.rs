//! Devices' choices of a space ([technical design: devices that choose their spaces]): a
//! device's choice is signed by its key and numbered, and of its choices the one with the
//! highest number stands, a drop winning a tie; a standing choice not to hold the space ends
//! the device there, as a removal does; only a newer choice of its own to hold it, carried by
//! a replica of its member, brings it back.
//!
//! The family of these tests: the founder, an admin, with a desktop (device 0) and a laptop
//! (device 1), and their partner, an editor, with one device (device 2).
//!
//! [technical design: devices that choose their spaces]: ../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces

mod common;

use common::{DRIFT, Device, add, counts_in, pair, send, text};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{DecryptionKey, EncryptionKey};
use nibkeep_governance::{Choice, replica_record};
use nibkeep_log::{Log, Parcel, SpaceKeys};
use nibkeep_model::{Operation, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const CAPTION: &str = "org.example.caption";

const ADMIN: &str = "admin";
const EDITOR: &str = "editor";

const FOUNDER: usize = 0;
const PARTNER: usize = 1;

const DESKTOP: usize = 0;
const LAPTOP: usize = 1;
const PARTNERS: usize = 2;

/// A shared space, as in the tests of roles, founded by device 0.
struct Family {
	rng: TestRng,
	id: SpaceId,
	keys: SpaceKeys,
	devices: Vec<Device>,
	encryptions: Vec<EncryptionKey>,
	member_of: Vec<usize>,
	members: Vec<EntityId>,
	photo: EntityId,
	now: u64,
}

impl Family {
	/// A space of the members that `member_of` names, one for each device, founded by device
	/// 0; with `roles`, its first change gives the founder the admin role, so that it has
	/// members; without, it has none, as a personal space has none.
	fn new(seed: &str, member_of: &[usize], roles: bool) -> Self {
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let mut devices: Vec<Device> = member_of
			.iter()
			.map(|_| Device::new(&mut rng, id))
			.collect();
		for device in &mut devices {
			device.log.expect_governed();
		}
		let encryptions = member_of
			.iter()
			.map(|_| DecryptionKey::generate(&mut rng).encryption_key())
			.collect();
		let count = member_of.iter().max().map_or(0, |last| last + 1);
		let members = (0..count).map(|_| rng.entity_id()).collect();
		let photo = rng.entity_id();
		let mut family = Self {
			rng,
			id,
			keys,
			devices,
			encryptions,
			member_of: member_of.to_vec(),
			members,
			photo,
			now: millis_at("2026-11-21", 9, 0),
		};
		let mut founding = if roles {
			common::making_member(&mut family.rng, family.members[member_of[0]], ADMIN)
		} else {
			Vec::new()
		};
		founding.extend(family.listing(0));
		family.commit(0, founding);
		family
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn listing(&mut self, device: usize) -> Vec<Operation> {
		let member = self.members[self.member_of[device]];
		let key = (&self.devices[device].key, &self.encryptions[device]);
		common::listing(&mut self.rng, self.id, key, (member, false))
	}

	fn record(&self, device: usize) -> EntityId {
		replica_record(self.id, &self.devices[device].key.verifying_key())
	}

	/// Device `by` lists device `device`, as a replica of a member that has a record.
	fn list(&mut self, by: usize, device: usize) -> Parcel {
		let operations = self.listing(device);
		self.commit(by, operations)
	}

	/// Device `by` invites device `device`'s member with `role`.
	fn invite(&mut self, by: usize, device: usize, role: &str) -> Parcel {
		let mut operations =
			common::making_member(&mut self.rng, self.members[self.member_of[device]], role);
		operations.extend(self.listing(device));
		self.commit(by, operations)
	}

	/// The choice numbered `number` of device `device`, signed by its key.
	fn choice(&self, device: usize, number: u64, holds: bool) -> Choice {
		Choice::sign(&self.devices[device].key, self.id, number, holds)
	}

	/// Device `by` states `choice` on the record it names.
	fn state(&mut self, by: usize, choice: &Choice) -> Parcel {
		self.state_on(by, choice.record, choice)
	}

	/// Device `by` states `choice` on `record`.
	fn state_on(&mut self, by: usize, record: EntityId, choice: &Choice) -> Parcel {
		let operation = common::assert(
			&mut self.rng,
			record,
			fields::SPACE_CHOICE,
			Value::Text(choice.to_text()),
		);
		self.commit(by, vec![operation])
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> Parcel {
		let operation = add(&mut self.rng, self.photo, CAPTION, text(caption));
		self.commit(by, vec![operation])
	}

	fn commit(&mut self, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.keys.clone();
		self.devices[device].commit(&keys, operations, wall, &mut self.rng)
	}

	fn sync(&mut self, from: usize, to: usize) {
		let wall = self.tick();
		let (from, to) = pair(&mut self.devices, from, to);
		send(&from.log, &mut to.log, &self.keys, wall);
		to.receive_keys_from(from, &self.keys);
	}

	fn sync_all(&mut self) {
		let count = self.devices.len();
		for _ in 0..2 {
			for from in 0..count {
				for to in 0..count {
					if from != to {
						self.sync(from, to);
					}
				}
			}
		}
	}

	fn counts(&self, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.log), change)
	}

	/// What ends device `device` there, as every device that holds a change of its key says,
	/// which must agree.
	fn ending(&self, device: usize) -> Vec<nibkeep_log::ChangeId> {
		let key = self.devices[device].key.verifying_key();
		let found: Vec<Vec<nibkeep_log::ChangeId>> = self
			.devices
			.iter()
			.map(|other| other.log.removals_of(&key, &self.keys).expect("reads"))
			.collect();
		assert!(
			found.iter().all(|ending| *ending == found[0]),
			"the devices disagree: {found:?}"
		);
		found[0].clone()
	}

	/// A copy of device `device`'s key, with what device `from` holds, as a thief's: the last
	/// device.
	fn copy_of(&mut self, device: usize, from: usize) -> usize {
		let mut log = Log::new(self.id, Clock::new(DRIFT));
		log.expect_governed();
		self.devices.push(Device {
			key: self.devices[device].key.clone(),
			log,
		});
		self.encryptions.push(self.encryptions[device].clone());
		self.member_of.push(self.member_of[device]);
		let copy = self.devices.len() - 1;
		self.sync(from, copy);
		copy
	}
}

/// The founder's desktop and laptop, and the partner's device, an editor's, which every
/// device knows of.
fn family(seed: &str) -> Family {
	let mut family = Family::new(seed, &[FOUNDER, FOUNDER, PARTNER], true);
	family.list(DESKTOP, LAPTOP);
	family.invite(DESKTOP, PARTNERS, EDITOR);
	family.sync_all();
	family
}

#[test]
fn a_standing_drop_ends_the_device() {
	let mut family = family("a standing drop");
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
	let choice = family.choice(LAPTOP, 1, false);
	let drop = family.state(LAPTOP, &choice);
	let after = family.caption(LAPTOP, "after its drop");
	family.sync_all();
	assert!(family.counts(&drop));
	assert_eq!(family.ending(LAPTOP), vec![drop.id()]);
	assert!(!family.counts(&after));
	assert_eq!(family.ending(DESKTOP), Vec::<nibkeep_log::ChangeId>::new());
}

#[test]
fn what_the_device_wrote_before_its_drop_counts() {
	let mut family = family("before a drop");
	let before = family.caption(LAPTOP, "before its drop");
	let choice = family.choice(LAPTOP, 1, false);
	family.state(LAPTOP, &choice);
	family.sync_all();
	assert!(family.counts(&before));
}

#[test]
fn a_change_made_without_knowing_of_the_drop_is_set_aside() {
	let mut family = family("unknowing of a drop");
	let thief = family.copy_of(LAPTOP, DESKTOP);
	let choice = family.choice(LAPTOP, 1, false);
	family.state(LAPTOP, &choice);
	let unknowing = family.caption(thief, "made without knowing of the drop");
	family.sync_all();
	assert!(!family.counts(&unknowing));
}

#[test]
fn the_highest_number_stands_whatever_builds_on_what() {
	// The desktop carries the laptop's third choice, to hold, while the laptop states its
	// second, to drop, neither knowing of the other: the third stands.
	let mut family = family("the highest number, holding");
	let hold = family.choice(LAPTOP, 3, true);
	family.state(DESKTOP, &hold);
	let drop = family.choice(LAPTOP, 2, false);
	family.state(LAPTOP, &drop);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
	// And the other way round: the third, to drop, stands over the second, to hold.
	let mut family = self::family("the highest number, dropping");
	let hold = family.choice(LAPTOP, 2, true);
	family.state(DESKTOP, &hold);
	let drop = family.choice(LAPTOP, 3, false);
	let dropping = family.state(LAPTOP, &drop);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), vec![dropping.id()]);
}

#[test]
fn of_two_choices_with_one_number_the_drop_stands() {
	let mut family = family("a tie");
	let hold = family.choice(LAPTOP, 1, true);
	family.state(DESKTOP, &hold);
	let drop = family.choice(LAPTOP, 1, false);
	let dropping = family.state(LAPTOP, &drop);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), vec![dropping.id()]);
}

#[test]
fn a_choice_carried_by_another_replica_of_the_member_counts() {
	let mut family = family("a carried choice");
	let choice = family.choice(LAPTOP, 1, false);
	let carried = family.state(DESKTOP, &choice);
	family.sync_all();
	assert!(family.counts(&carried));
	assert_eq!(family.ending(LAPTOP), vec![carried.id()]);
}

#[test]
fn a_choice_carried_by_another_member_counts_for_nothing() {
	let mut family = family("another member's choice");
	// The partner, an editor, carries the laptop's drop.
	let choice = family.choice(LAPTOP, 1, false);
	let by_partner = family.state(PARTNERS, &choice);
	family.sync_all();
	assert!(!family.counts(&by_partner));
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
	// Nor does the founder's, an admin's, carrying the partner's drop.
	let choice = family.choice(PARTNERS, 1, false);
	let by_admin = family.state(DESKTOP, &choice);
	family.sync_all();
	assert!(!family.counts(&by_admin));
	assert_eq!(family.ending(PARTNERS), Vec::<nibkeep_log::ChangeId>::new());
}

#[test]
fn a_forged_choice_counts_for_nothing() {
	let mut family = family("a forged choice");
	// The desktop signs a drop with its own key, and puts it on the laptop's record.
	let forged = family.choice(DESKTOP, 1, false);
	let record = family.record(LAPTOP);
	family.state_on(DESKTOP, record, &forged);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
	assert_eq!(family.ending(DESKTOP), Vec::<nibkeep_log::ChangeId>::new());
}

#[test]
fn a_dropped_device_comes_back_only_with_a_newer_choice_to_hold() {
	let mut family = family("coming back");
	let choice = family.choice(LAPTOP, 1, false);
	let drop = family.state(LAPTOP, &choice);
	family.sync_all();
	// Listed again with its listing alone, it stays ended.
	family.list(DESKTOP, LAPTOP);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), vec![drop.id()]);
	// With a choice to hold of the same number, too.
	let same = family.choice(LAPTOP, 1, true);
	family.state(DESKTOP, &same);
	family.sync_all();
	assert_eq!(family.ending(LAPTOP), vec![drop.id()]);
	let ended = family.caption(LAPTOP, "while ended");
	family.sync_all();
	assert!(!family.counts(&ended));
	// With a newer one, it is back.
	let newer = family.choice(LAPTOP, 2, true);
	let back = family.state(DESKTOP, &newer);
	family.sync_all();
	assert!(family.counts(&back));
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
	let after = family.caption(LAPTOP, "back");
	family.sync_all();
	assert!(family.counts(&after));
}

#[test]
fn a_dropped_devices_own_governing_changes_count_for_nothing() {
	let mut family = family("its own governing changes");
	let choice = family.choice(LAPTOP, 1, false);
	let drop = family.state(LAPTOP, &choice);
	family.sync_all();
	let own = family.choice(LAPTOP, 2, true);
	let itself = family.state(LAPTOP, &own);
	family.sync_all();
	assert!(!family.counts(&itself));
	assert_eq!(family.ending(LAPTOP), vec![drop.id()]);
}

#[test]
fn what_its_maker_made_with_or_after_its_drop_is_no_members() {
	let mut family = family("its maker's drop");
	let before = family.caption(LAPTOP, "before");
	let choice = family.choice(LAPTOP, 1, false);
	let drop = family.state(LAPTOP, &choice);
	let after = family.caption(LAPTOP, "after");
	family.sync_all();
	let later = family.caption(DESKTOP, "the desktop's, later");
	family.sync_all();
	let log = &family.devices[DESKTOP].log;
	let keys = &family.keys;
	let made = |parcel: &Parcel| log.author_was_a_member(parcel.id(), keys).expect("reads");
	assert!(made(&before));
	assert!(!made(&drop));
	assert!(!made(&after));
	assert!(made(&later));
}

#[test]
fn choices_count_for_nothing_where_the_space_has_no_members() {
	let mut family = Family::new("no members", &[FOUNDER, FOUNDER], false);
	family.list(DESKTOP, LAPTOP);
	family.sync_all();
	let choice = family.choice(LAPTOP, 1, false);
	family.state(LAPTOP, &choice);
	let after = family.caption(LAPTOP, "after");
	family.sync_all();
	assert!(family.counts(&after));
	assert_eq!(family.ending(LAPTOP), Vec::<nibkeep_log::ChangeId>::new());
}
