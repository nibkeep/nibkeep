//! A device that lists itself by a recovery code in a space with members, after its member
//! lost their other devices: its listing counts where it carries the device's own listing,
//! as every listing there does, and a counting change it builds on, by a replica of that
//! member, names the code on that member's record; the device then has its member's role
//! at once ([technical design: recovery codes]).
//!
//! The family of these tests: the founder is the space's admin, their partner an editor and
//! the partner's sister a viewer; device 3 is a new one.
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use crate::common;

use crate::common::{Device, add, counts_in, making_member, pair, send, text};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{DecryptionKey, EncryptionKey, RecoveryCode};
use nibkeep_governance::{
	RecoveryListing, RecoveryStatement, encryption_key_text, list_by_recovery_code,
	recovery_record, replica_record, revoke_recovery_code, state_recovery_code,
};
use nibkeep_log::{Parcel, SpaceKeys};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{Operation, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};

const CAPTION: &str = "org.example.caption";

/// The founder, their partner and the partner's sister, by member.
const FOUNDER: usize = 0;
const PARTNER: usize = 1;
const SISTER: usize = 2;

/// The device that comes back with a code.
const NEW: usize = 3;

/// A shared space: devices 0 to 2 are the founder's, the partner's and the sister's, and
/// device 3, a new one, is of the member that [`Family::new`] names.
struct Family {
	rng: TestRng,
	id: SpaceId,
	keys: SpaceKeys,
	devices: Vec<Device>,
	encryptions: Vec<EncryptionKey>,
	/// The member that each device is a replica of.
	member_of: Vec<usize>,
	/// Each member's record.
	members: Vec<EntityId>,
	/// A photo, which the tests caption.
	photo: EntityId,
	now: u64,
}

impl Family {
	/// The space, with the partner an editor and the sister a viewer, each invited by the
	/// founder; device 3 is a new device of `new_of`'s, which holds what the others hold, as
	/// if from a backup, and is listed nowhere.
	fn new(seed: &str, new_of: usize) -> Self {
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let member_of = vec![FOUNDER, PARTNER, SISTER, new_of];
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
		let members = (0..3).map(|_| rng.entity_id()).collect();
		let photo = rng.entity_id();
		let mut family = Self {
			rng,
			id,
			keys,
			devices,
			encryptions,
			member_of,
			members,
			photo,
			now: millis_at("2026-11-14", 9, 0),
		};
		let mut founding = making_member(&mut family.rng, family.members[FOUNDER], "admin");
		founding.extend(family.listing(0));
		family.commit(0, founding);
		for (device, role) in [(1, "editor"), (2, "viewer")] {
			let member = family.members[family.member_of[device]];
			let mut operations = making_member(&mut family.rng, member, role);
			operations.extend(family.listing(device));
			family.commit(0, operations);
		}
		family.sync_all();
		family
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn assert(
		&mut self,
		entity: EntityId,
		field: nibkeep_model::FieldName,
		value: Value,
	) -> Operation {
		common::assert(&mut self.rng, entity, field, value)
	}

	/// A device's replica record, derived from the space and its key.
	fn record(&self, device: usize) -> EntityId {
		replica_record(self.id, &self.devices[device].key.verifying_key())
	}

	/// The governing operations that list a device's key on its record, owned by its member,
	/// with the listing that its key signed.
	fn listing(&mut self, device: usize) -> Vec<Operation> {
		let member = self.members[self.member_of[device]];
		let key = (&self.devices[device].key, &self.encryptions[device]);
		common::listing(&mut self.rng, self.id, key, (member, false))
	}

	/// The listing of device `device` that `code` signs, on its record, for `owner`.
	fn signed_by(&self, device: usize, code: &RecoveryCode, owner: usize) -> RecoveryListing {
		RecoveryListing::sign(
			code,
			(self.id, self.record(device), self.members[owner]),
			&self.devices[device].key.verifying_key(),
			&self.encryptions[device],
		)
	}

	/// The governing operations by which device `device` lists itself by `code` for its
	/// member: its own listing, and the listing that the code signs.
	fn by_code(&mut self, device: usize, code: &RecoveryCode) -> Vec<Operation> {
		let signed = self.signed_by(device, code, self.member_of[device]);
		let mut operations = self.listing(device);
		operations.push(list_by_recovery_code(&signed, &mut self.rng));
		operations
	}

	/// Device `by` names `code` for `member`, on that member's record for it.
	fn name_code(&mut self, by: usize, member: usize, code: &RecoveryCode) -> Parcel {
		let stated = RecoveryStatement::sign(code, self.members[member]);
		let operation = state_recovery_code(self.id, &stated, &mut self.rng);
		self.commit(by, vec![operation])
	}

	/// Device `by` revokes `code` on `member`'s record for it.
	fn revoke_code(&mut self, by: usize, member: usize, code: &RecoveryCode) -> Parcel {
		let key = code.signing_key().verifying_key();
		let record = recovery_record(self.id, self.members[member], &key);
		let operation = revoke_recovery_code(record, &key, &mut self.rng);
		self.commit(by, vec![operation])
	}

	/// Device `by` revokes device `revoked`'s replica, by its key.
	fn revoke(&mut self, by: usize, revoked: usize) -> Parcel {
		let record = self.record(revoked);
		let key = self.devices[revoked].key.verifying_key();
		let operation = self.assert(record, fields::REVOKED, Value::Text(key.to_text()));
		self.commit(by, vec![operation])
	}

	/// Device `by` gives `member` a role.
	fn give_role(&mut self, by: usize, member: usize, role: &str) -> Parcel {
		let record = self.members[member];
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(by, vec![operation])
	}

	/// Device `by` names its own record.
	fn name_itself(&mut self, by: usize, name: &str) -> Parcel {
		let record = self.record(by);
		let operation = self.assert(record, fields::NAME, text(name));
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

	/// Device `to` receives every change of device `from` that it takes, parents first, and
	/// every wrapped key, as a sync sends them.
	fn sync(&mut self, from: usize, to: usize) {
		let wall = self.tick();
		let (from, to) = pair(&mut self.devices, from, to);
		send(&from.log, &mut to.log, &self.keys, wall);
		to.receive_keys_from(from, &self.keys);
	}

	/// Every device receives what every other has.
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

	/// Whether a change counts on every device that holds it, which must agree.
	fn counts(&self, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.log), change)
	}

	/// Whether every device holds a change.
	fn everyone_holds(&self, change: &Parcel) -> bool {
		self.devices
			.iter()
			.all(|device| device.log.contains(change.id()).infallible())
	}
}

#[test]
fn a_members_new_device_lists_itself_by_their_code_with_their_role() {
	let mut family = Family::new("a viewer's device by a code", SISTER);
	let code = RecoveryCode::generate(&mut family.rng);
	let named = family.name_code(2, SISTER, &code);
	family.sync_all();
	assert!(family.counts(&named));
	// Her phone is lost; her new device lists itself by her code, and every other device
	// takes the listing although nothing listed its key.
	let listing = family.by_code(NEW, &code);
	let listed = family.commit(NEW, listing);
	family.sync_all();
	assert!(family.everyone_holds(&listed));
	assert!(family.counts(&listed));
	// It has her role, a viewer's: it names its own record, but captions nothing.
	let named = family.name_itself(NEW, "Her new tablet");
	let captioned = family.caption(NEW, "at the lake");
	family.sync_all();
	assert!(family.counts(&named));
	assert!(!family.counts(&captioned));
	// And it revokes her lost phone, as her own device.
	let revoked = family.revoke(NEW, 2);
	family.sync_all();
	assert!(family.counts(&revoked));
}

#[test]
fn an_admins_new_device_is_an_admin_at_once() {
	let mut family = Family::new("an admin's device by a code", FOUNDER);
	let code = RecoveryCode::generate(&mut family.rng);
	family.name_code(0, FOUNDER, &code);
	family.sync_all();
	let listing = family.by_code(NEW, &code);
	let listed = family.commit(NEW, listing);
	family.sync_all();
	assert!(family.counts(&listed));
	let made_admin = family.give_role(NEW, PARTNER, "admin");
	let revoked = family.revoke(NEW, 0);
	family.sync_all();
	assert!(family.counts(&made_admin));
	assert!(family.counts(&revoked));
}

#[test]
fn a_listing_by_a_code_needs_the_devices_own_listing_too() {
	let mut family = Family::new("by a code alone", SISTER);
	let code = RecoveryCode::generate(&mut family.rng);
	family.name_code(2, SISTER, &code);
	family.sync_all();
	// The key, its owner and its encryption key, and the code's listing, but not the
	// listing that the key itself signed.
	let record = family.record(NEW);
	let key = family.devices[NEW].key.verifying_key();
	let encryption = encryption_key_text(&family.encryptions[NEW]);
	let signed = family.signed_by(NEW, &code, SISTER);
	let member = family.members[SISTER];
	let operations = vec![
		family.assert(record, fields::SIGNING_KEY, Value::Text(key.to_text())),
		family.assert(record, fields::OWNER, Value::Reference(member)),
		family.assert(record, fields::ENCRYPTION_KEY, Value::Text(encryption)),
		list_by_recovery_code(&signed, &mut family.rng),
	];
	let listed = family.commit(NEW, operations);
	family.sync_all();
	assert!(!family.counts(&listed));
	// Nor does another device take it from a sync, as it lists nobody.
	assert!(!family.devices[0].log.contains(listed.id()).infallible());
}

#[test]
fn a_code_lists_only_for_the_member_whose_own_device_named_it() {
	let mut family = Family::new("another member's code", SISTER);
	let code = RecoveryCode::generate(&mut family.rng);
	// The founder, an admin, names a code for the sister: it names nothing, and lists
	// nobody.
	family.name_code(0, SISTER, &code);
	family.sync_all();
	let listing = family.by_code(NEW, &code);
	let listed = family.commit(NEW, listing);
	family.sync_all();
	assert!(!family.counts(&listed));
}

#[test]
fn a_code_lists_nobody_for_a_member_it_is_not_named_for() {
	let mut family = Family::new("a code for another member", PARTNER);
	let code = RecoveryCode::generate(&mut family.rng);
	// The sister names her code; whoever finds it lists a device as the partner's.
	family.name_code(2, SISTER, &code);
	family.sync_all();
	let listing = family.by_code(NEW, &code);
	let listed = family.commit(NEW, listing);
	family.sync_all();
	assert!(!family.counts(&listed));
}

#[test]
fn a_revoked_code_lists_nobody_in_a_space_with_members() {
	let mut family = Family::new("a revoked code with members", SISTER);
	let code = RecoveryCode::generate(&mut family.rng);
	family.name_code(2, SISTER, &code);
	family.sync_all();
	let revoked = family.revoke_code(2, SISTER, &code);
	family.sync_all();
	assert!(family.counts(&revoked));
	let listing = family.by_code(NEW, &code);
	let listed = family.commit(NEW, listing);
	family.sync_all();
	assert!(!family.counts(&listed));
}
