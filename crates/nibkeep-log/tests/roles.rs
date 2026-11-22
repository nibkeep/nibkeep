//! Roles in a space with members: a change counts if its author's role at the change allows
//! it, as the changes it builds on give that role, lowered by those that lower it and that
//! it does not know of ([technical design: sharing]). Each operation names the one entity it
//! changes, so every replica decides from the change alone.
//!
//! The family of these tests: the founder is the space's admin, their partner an editor and
//! the partner's sister a viewer.
//!
//! [technical design: sharing]: ../../../docs/Technical-design.md#13-sharing

mod common;

use common::{Device, add, counts_in, pair, send, text, texts};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::RecoveryCode;
use nibkeep_crypto::{DecryptionKey, EncryptionKey};
use nibkeep_governance::{
	RecoveryEntry, RecoveryStatement, device_record, recovery_record, replica_record,
	revoke_recovery_code, state_recovery_code, state_recovery_entry,
};
use nibkeep_log::{ChangeId, LogError, Parcel, SpaceKeys};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};

const CAPTION: &str = "org.example.caption";

const ADMIN: &str = "admin";
const EDITOR: &str = "editor";
const VIEWER: &str = "viewer";

/// The founder, their partner and the partner's sister, by member.
const FOUNDER: usize = 0;
const PARTNER: usize = 1;
const SISTER: usize = 2;

fn start() -> u64 {
	millis_at("2026-11-05", 9, 0)
}

/// A shared space: devices, each a replica of one member's, and the members' records. The
/// founder's first device founds it.
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
	/// A space of the members that `member_of` names, one for each device, founded by device
	/// 0, whose member is the admin. Nobody else is a member yet.
	fn new(seed: &str, member_of: &[usize]) -> Self {
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
			now: start(),
		};
		let mut founding = family.making_member(member_of[0], ADMIN);
		founding.extend(family.listing(0));
		family.commit(0, founding);
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

	/// A device's device record, derived from the space and its key.
	fn device_record(&self, device: usize) -> EntityId {
		device_record(self.id, &self.devices[device].key.verifying_key())
	}

	/// The governing operations that make `member`'s record, with a user ID and a role.
	fn making_member(&mut self, member: usize, role: &str) -> Vec<Operation> {
		common::making_member(&mut self.rng, self.members[member], role)
	}

	/// The governing operations that list a device's key on its record, owned by its member,
	/// with the listing that its key signed.
	fn listing(&mut self, device: usize) -> Vec<Operation> {
		let member = self.members[self.member_of[device]];
		let key = (&self.devices[device].key, &self.encryptions[device]);
		common::listing(&mut self.rng, self.id, key, (member, false))
	}

	/// Device `by` invites device `device`'s member with `role`: one change that makes the
	/// member's record and lists the device that joins; then the device holds what `by`
	/// has.
	fn invite(&mut self, by: usize, device: usize, role: &str) -> Parcel {
		let mut operations = self.making_member(self.member_of[device], role);
		operations.extend(self.listing(device));
		let parcel = self.commit(by, operations);
		self.sync(by, device);
		parcel
	}

	/// Device `by` lists device `device`, as a replica of a member that has a record already.
	fn list(&mut self, by: usize, device: usize) -> Parcel {
		let operations = self.listing(device);
		self.commit(by, operations)
	}

	/// Device `by` gives `member` a role.
	fn give_role(&mut self, by: usize, member: usize, role: &str) -> Parcel {
		let record = self.members[member];
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(by, vec![operation])
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> (Parcel, StatementId) {
		let operation = add(&mut self.rng, self.photo, CAPTION, text(caption));
		let Operation::Assert { statement, .. } = &operation else {
			unreachable!("an assertion");
		};
		let id = statement.id;
		(self.commit(by, vec![operation]), id)
	}

	/// Device `by` names an entity, replacing the statements in `replacing`.
	fn name(
		&mut self,
		by: usize,
		entity: EntityId,
		name: &str,
		replacing: &[StatementId],
	) -> Parcel {
		let statement = Statement::new(self.rng.statement_id(), entity, fields::NAME, text(name));
		self.commit(
			by,
			vec![Operation::replace(statement, replacing.iter().copied())],
		)
	}

	fn commit(&mut self, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.keys.clone();
		self.devices[device].commit(&keys, operations, wall, &mut self.rng)
	}

	/// Device `to` receives every change of device `from`, parents first, and every wrapped
	/// key, as a sync sends them. Changes of keys it knows no listing of are refused, as a
	/// sync drops them.
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

	/// Whether a change counts on every device, which must agree.
	fn counts(&self, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.log), change)
	}

	/// The captions that device 0 shows, sorted.
	fn captions(&self) -> Vec<String> {
		let mut captions = texts(&self.devices[0].log, self.photo, CAPTION);
		captions.sort();
		captions
	}

	/// The names of an entity that device 0 shows, sorted.
	fn names(&self, entity: EntityId) -> Vec<String> {
		let mut names = texts(&self.devices[0].log, entity, "org.nibkeep.name");
		names.sort();
		names
	}
}

/// The family's space: the founder's device 0 makes the partner's device 1 an editor and the
/// sister's device 2 a viewer.
fn family(seed: &str) -> Family {
	let mut family = Family::new(seed, &[FOUNDER, PARTNER, SISTER]);
	family.invite(0, 1, EDITOR);
	family.invite(0, 2, VIEWER);
	family.sync_all();
	family
}

#[test]
fn a_viewers_changes_count_only_on_their_own_records() {
	let mut family = family("a viewer");
	let (captioned, caption) = family.caption(0, "Grandma at the lake");
	family.sync_all();
	// The sister's caption does not count, nor her taking away the founder's.
	let (hers, _) = family.caption(2, "At the lake, 1971");
	let photo = family.photo;
	let taking = family.commit(2, vec![Operation::retract(photo, [caption])]);
	// Naming her own phone and her own member's record counts.
	let (phone, member) = (family.record(2), family.members[SISTER]);
	let renamed = family.name(2, phone, "Her phone", &[]);
	let named = family.name(2, member, "Aunt Kate", &[]);
	// A retraction that names her own record, of the founder's caption, counts, and takes
	// nothing away: an operation supersedes only statements of the entity it names.
	let pretending = family.commit(2, vec![Operation::retract(phone, [caption])]);
	family.sync_all();
	assert!(family.counts(&captioned));
	assert!(!family.counts(&hers));
	assert!(!family.counts(&taking));
	assert!(family.counts(&renamed));
	assert!(family.counts(&named));
	assert!(family.counts(&pretending));
	assert_eq!(family.captions(), ["Grandma at the lake"]);
	assert_eq!(family.names(phone), ["Her phone"]);
}

#[test]
fn an_editors_changes_count_but_not_on_other_members_records() {
	let mut family = family("an editor");
	let (captioned, _) = family.caption(2, "never counts");
	let (founders, founders_caption) = family.caption(0, "Grandma at the lake");
	family.sync_all();
	assert!(!family.counts(&captioned));
	// The partner captions the photo, and replaces the founder's caption.
	let (partners, _) = family.caption(1, "At the lake, 1971");
	let photo = family.photo;
	let replacement = Statement::new(
		family.rng.statement_id(),
		photo,
		nibkeep_model::FieldName::from_static(CAPTION),
		text("Grandma and the lake"),
	);
	let replacing = family.commit(1, vec![Operation::replace(replacement, [founders_caption])]);
	// Naming the sister's phone, her device or her member does not count; naming the
	// partner's own does.
	let (sisters, sisters_device, sister) = (
		family.record(2),
		family.device_record(2),
		family.members[SISTER],
	);
	let on_her_phone = family.name(1, sisters, "Kate's phone", &[]);
	let on_her_device = family.name(1, sisters_device, "Kate's Galaxy", &[]);
	let on_her = family.name(1, sister, "Katie", &[]);
	let partners_phone = family.record(1);
	let own = family.name(1, partners_phone, "My phone", &[]);
	family.sync_all();
	assert!(family.counts(&founders));
	assert!(family.counts(&partners));
	assert!(family.counts(&replacing));
	assert!(!family.counts(&on_her_phone));
	assert!(!family.counts(&on_her_device));
	assert!(!family.counts(&on_her));
	assert!(family.counts(&own));
	assert_eq!(
		family.captions(),
		["At the lake, 1971", "Grandma and the lake"]
	);
}

#[test]
fn only_an_admin_gives_roles_and_makes_fields_secret_for_good() {
	let mut family = family("only admins");
	// The partner makes the sister an editor, and a field secret for good: neither counts.
	let promoting = family.give_role(1, SISTER, EDITOR);
	let partners = family.record(1);
	let secret = family.assert(
		partners,
		fields::SECRET_FIELD,
		Value::Text("org.example.pin".to_owned()),
	);
	let making_secret = family.commit(1, vec![secret]);
	family.sync_all();
	assert!(!family.counts(&promoting));
	assert!(!family.counts(&making_secret));
	let (sisters, _) = family.caption(2, "still a viewer");
	family.sync_all();
	assert!(!family.counts(&sisters));
	// The founder does both, and both count.
	let promoting = family.give_role(0, SISTER, EDITOR);
	let founders = family.record(0);
	let secret = family.assert(
		founders,
		fields::SECRET_FIELD,
		Value::Text("org.example.pin".to_owned()),
	);
	let making_secret = family.commit(0, vec![secret]);
	family.sync_all();
	assert!(family.counts(&promoting));
	assert!(family.counts(&making_secret));
	let (sisters, _) = family.caption(2, "an editor now");
	family.sync_all();
	assert!(family.counts(&sisters));
}

#[test]
fn only_an_admin_states_what_this_version_does_not_know() {
	// A later version may give a governing field a meaning, so this one leaves it to
	// admins, as an operation of a kind it does not know.
	let mut family = family("unknown governing fields");
	let later: nibkeep_model::FieldName = "org.nibkeep.from-a-later-version"
		.parse()
		.expect("a field name");
	let mut forged = Vec::new();
	for by in [1, 0] {
		let record = family.record(by);
		let statement = family.assert(record, later.clone(), Value::Text("later".to_owned()));
		let (wall, keys) = (family.tick(), family.keys.clone());
		forged.push(family.devices[by].forge_on_heads(
			&keys,
			nibkeep_log::Kind::Governing,
			vec![statement],
			wall,
			&mut family.rng,
		));
	}
	family.sync_all();
	assert!(!family.counts(&forged[0]));
	assert!(family.counts(&forged[1]));
}

#[test]
fn an_editor_purges_what_their_member_signed_and_only_an_admin_purges_content() {
	let mut family = family("purges by role");
	let (founders, founders_caption) = family.caption(0, "the founder's");
	let (partners, partners_caption) = family.caption(1, "the partner's");
	family.sync_all();
	assert!(family.counts(&founders) && family.counts(&partners));
	let keys = family.keys.clone();
	let (wall, rng) = (family.tick(), &mut family.rng);
	let device = &mut family.devices[1];
	let own = device
		.log
		.purge(&device.key, &keys, [partners_caption], wall, rng)
		.expect("purges its own");
	let wall = family.tick();
	let device = &mut family.devices[1];
	let others = device
		.log
		.purge(
			&device.key,
			&keys,
			[founders_caption],
			wall,
			&mut family.rng,
		)
		.expect("makes the purge");
	// Content: the partner holds a blob's key, and purges it.
	let content = Hash::of(b"a scanned print");
	let sealed = Hash::of(b"its sealed copy");
	let key = family.rng.key();
	let device = &mut family.devices[1];
	device
		.log
		.add_blob_key(content, sealed, key, &keys, &mut family.rng)
		.expect("adds the key");
	let wall = family.tick();
	let device = &mut family.devices[1];
	let blob = device
		.log
		.purge_blob(&device.key, &keys, &content, wall, &mut family.rng)
		.expect("makes the purge");
	family.sync_all();
	assert!(family.counts(&own), "an editor purges what they signed");
	assert!(!family.counts(&others), "nor what others signed");
	assert!(!family.counts(&blob), "nor content");
	assert_eq!(family.captions(), ["the founder's"]);
	assert!(!family.devices[0].log.is_blob_purged(&sealed).infallible());
}

#[test]
fn who_may_purge_something_may_restore_it_and_nobody_else() {
	let mut family = family("restores by role");
	let (founders, founders_caption) = family.caption(0, "the founder's");
	let (partners, partners_caption) = family.caption(1, "the partner's");
	family.sync_all();
	let keys = family.keys.clone();
	// The founder, an admin, purges both captions, and the partner's device keeps what that
	// destroys, as a device that waits does.
	family.devices[1].log.keep_destroyed(true);
	let wall = family.tick();
	let device = &mut family.devices[0];
	let purge = device
		.log
		.purge(
			&device.key,
			&keys,
			[founders_caption, partners_caption],
			wall,
			&mut family.rng,
		)
		.expect("purges both");
	family.sync_all();
	assert!(family.counts(&purge));
	nibkeep_testkit::assert_empty!(family.captions());
	let restoring = |changes: &[&Parcel]| nibkeep_log::Restores {
		changes: changes.iter().map(|parcel| parcel.id()).collect(),
		..nibkeep_log::Restores::default()
	};
	// The partner, an editor, may restore what their member signed, but not the founder's.
	let wall = family.tick();
	let device = &mut family.devices[1];
	let own = device
		.log
		.restore_purged(
			&device.key,
			&keys,
			restoring(&[&partners]),
			wall,
			&mut family.rng,
		)
		.expect("restores its own");
	let wall = family.tick();
	let device = &mut family.devices[1];
	let heads = device.log.heads().infallible();
	assert_eq!(
		device.log.restore_purged(
			&device.key,
			&keys,
			restoring(&[&founders]),
			wall,
			&mut family.rng,
		),
		Err(LogError::CannotRestore),
		"nor what others signed: a restore that would not count is never made"
	);
	assert_eq!(device.log.heads().infallible(), heads);
	family.sync_all();
	assert!(family.counts(&own), "an editor restores what they signed");
	for device in &family.devices {
		assert!(!device.log.is_change_purged(partners.id()).infallible());
		assert!(device.log.is_change_purged(founders.id()).infallible());
	}
}

#[test]
fn a_restore_set_aside_leaves_what_it_restored_purged_again() {
	let mut family = family("restore set aside");
	let (partners, partners_caption) = family.caption(1, "the partner's");
	family.sync_all();
	let keys = family.keys.clone();
	family.devices[1].log.keep_destroyed(true);
	let wall = family.tick();
	let device = &mut family.devices[0];
	device
		.log
		.purge(
			&device.key,
			&keys,
			[partners_caption],
			wall,
			&mut family.rng,
		)
		.expect("purges");
	family.sync_all();
	// The partner restores their own caption, while the founder, who has not seen it,
	// makes them a viewer.
	let wall = family.tick();
	let device = &mut family.devices[1];
	let restore = device
		.log
		.restore_purged(
			&device.key,
			&keys,
			nibkeep_log::Restores {
				changes: std::collections::BTreeSet::from([partners.id()]),
				..nibkeep_log::Restores::default()
			},
			wall,
			&mut family.rng,
		)
		.expect("restores");
	assert!(
		!family.devices[1]
			.log
			.is_change_purged(partners.id())
			.infallible()
	);
	family.give_role(0, 1, VIEWER);
	family.sync_all();
	assert!(
		!family.counts(&restore),
		"made without knowing of the lower role"
	);
	for device in &family.devices {
		assert!(device.log.is_change_purged(partners.id()).infallible());
	}
	nibkeep_testkit::assert_empty!(family.captions());
}

#[test]
fn a_purge_that_comes_to_count_late_leaves_what_a_restore_after_it_brought_back() {
	let mut family = family("late purge");
	let (partners, partners_caption) = family.caption(1, "the partner's");
	family.sync_all();
	// The founder makes the partner a viewer, while the partner, not knowing, purges their
	// own caption: set aside.
	family.give_role(0, PARTNER, VIEWER);
	let keys = family.keys.clone();
	let wall = family.tick();
	let device = &mut family.devices[1];
	let purge = device
		.log
		.purge(
			&device.key,
			&keys,
			[partners_caption],
			wall,
			&mut family.rng,
		)
		.expect("purges");
	family.sync_all();
	assert!(!family.counts(&purge));
	assert_eq!(family.captions(), ["the partner's"]);
	// The founder restores the caption, building on the purge; then accepts the purge.
	let wall = family.tick();
	let device = &mut family.devices[0];
	let restore = device
		.log
		.restore_purged(
			&device.key,
			&keys,
			nibkeep_log::Restores {
				changes: std::collections::BTreeSet::from([partners.id()]),
				..nibkeep_log::Restores::default()
			},
			wall,
			&mut family.rng,
		)
		.expect("restores");
	let record = family.record(0);
	let operation = family.assert(
		record,
		fields::ACCEPTED,
		Value::Text(purge.id().to_string()),
	);
	family.commit(0, vec![operation]);
	family.sync_all();
	assert!(family.counts(&restore) && family.counts(&purge));
	// The purge counts, but what the restore after it brought back stays, payload and all.
	for device in &family.devices {
		assert!(!device.log.is_change_purged(partners.id()).infallible());
		assert!(
			device
				.log
				.sealed_payload(partners.id())
				.infallible()
				.is_some()
		);
	}
	assert_eq!(family.captions(), ["the partner's"]);
}

#[test]
fn nobody_acts_on_a_role_they_did_not_know_they_had() {
	let mut family = family("unknown roles");
	// The founder makes the sister an editor; she captions before she hears of it.
	let promoting = family.give_role(0, SISTER, EDITOR);
	let (before, _) = family.caption(2, "before she knew");
	family.sync_all();
	assert!(family.counts(&promoting));
	assert!(!family.counts(&before));
	let (after, _) = family.caption(2, "once she knew");
	family.sync_all();
	assert!(family.counts(&after));
	assert_eq!(family.captions(), ["once she knew"]);
}

#[test]
fn what_a_member_wrote_without_knowing_of_a_lower_role_is_set_aside_until_an_admin_accepts_it() {
	let mut family = family("lower roles");
	let (known, _) = family.caption(1, "before");
	family.sync_all();
	// The founder makes the partner a viewer, while the partner captions, not knowing.
	let demoting = family.give_role(0, PARTNER, VIEWER);
	let (unknowing, _) = family.caption(1, "meanwhile");
	family.sync_all();
	assert!(family.counts(&known), "what the demotion builds on stays");
	assert!(family.counts(&demoting));
	assert!(!family.counts(&unknowing));
	// The sister, a viewer, accepts it: that does not count. The founder does.
	let accepting = |family: &mut Family, by: usize, change: ChangeId| {
		let record = family.record(by);
		let operation = family.assert(record, fields::ACCEPTED, Value::Text(change.to_string()));
		family.commit(by, vec![operation])
	};
	let by_sister = accepting(&mut family, 2, unknowing.id());
	family.sync_all();
	assert!(!family.counts(&by_sister));
	assert!(!family.counts(&unknowing));
	let by_founder = accepting(&mut family, 0, unknowing.id());
	family.sync_all();
	assert!(family.counts(&by_founder));
	assert!(family.counts(&unknowing));
	// Acceptance never lifts a role's own limits: the sister's caption stays set aside.
	let (sisters, _) = family.caption(2, "a viewer's");
	family.sync_all();
	let accepted = accepting(&mut family, 0, sisters.id());
	family.sync_all();
	assert!(family.counts(&accepted));
	assert!(!family.counts(&sisters));
	assert_eq!(family.captions(), ["before", "meanwhile"]);
}

#[test]
fn of_two_admins_who_demote_each_other_the_senior_stays() {
	let mut family = family("two admins");
	family.give_role(0, PARTNER, ADMIN);
	family.sync_all();
	// Offline, each makes the other an editor, and goes on as an admin.
	let by_founder = family.give_role(0, PARTNER, EDITOR);
	let by_partner = family.give_role(1, FOUNDER, EDITOR);
	let founders_sister = family.give_role(0, SISTER, EDITOR);
	let partners_sister = family.give_role(1, SISTER, VIEWER);
	family.sync_all();
	assert!(
		family.counts(&by_founder),
		"the founder became an admin first"
	);
	assert!(!family.counts(&by_partner));
	assert!(family.counts(&founders_sister));
	assert!(!family.counts(&partners_sister));
	let (sisters, _) = family.caption(2, "an editor");
	family.sync_all();
	assert!(family.counts(&sisters));
}

#[test]
fn no_change_leaves_the_space_without_a_direct_admin() {
	let mut family = family("never without an admin");
	// The only admin cannot step down while others remain.
	let stepping_down = family.give_role(0, FOUNDER, EDITOR);
	family.sync_all();
	assert!(!family.counts(&stepping_down));
	// With two admins, one steps down; of two who step down at once, the later does not
	// count.
	family.give_role(0, PARTNER, ADMIN);
	family.sync_all();
	let partner_steps_down = family.give_role(1, PARTNER, EDITOR);
	let founder_steps_down = family.give_role(0, FOUNDER, EDITOR);
	family.sync_all();
	assert!(family.counts(&partner_steps_down));
	assert!(!family.counts(&founder_steps_down));
	let (founders, _) = family.caption(0, "still an admin");
	let promoting = family.give_role(0, SISTER, EDITOR);
	family.sync_all();
	assert!(family.counts(&founders));
	assert!(family.counts(&promoting));
}

#[test]
fn only_a_members_own_replicas_list_and_revoke_its_replicas() {
	// The sister has a phone and a tablet.
	let mut family = Family::new("own replicas", &[FOUNDER, PARTNER, SISTER, SISTER]);
	family.invite(0, 1, EDITOR);
	family.invite(0, 2, VIEWER);
	family.sync_all();
	// The founder, an admin, lists the sister's tablet: that lists nothing.
	let by_founder = family.list(0, 3);
	family.sync_all();
	assert!(!family.counts(&by_founder));
	// Her phone lists it, and it counts.
	let by_phone = family.list(2, 3);
	family.sync_all();
	assert!(family.counts(&by_phone));
	let tablet = family.record(3);
	let named = family.name(3, tablet, "Her tablet", &[]);
	family.sync_all();
	assert!(family.counts(&named));
	// The founder revokes her tablet: that does not count. Her phone does, and it counts.
	let revoking = |family: &mut Family, by: usize| {
		let key = family.devices[3].key.verifying_key();
		let operation = family.assert(tablet, fields::REVOKED, Value::Text(key.to_text()));
		family.commit(by, vec![operation])
	};
	let by_founder = revoking(&mut family, 0);
	family.sync_all();
	assert!(!family.counts(&by_founder));
	let by_phone = revoking(&mut family, 2);
	family.sync_all();
	assert!(family.counts(&by_phone));
}

#[test]
fn only_a_members_own_replicas_state_and_revoke_its_recovery_codes() {
	let mut family = family("recovery codes");
	let code = RecoveryCode::generate(&mut family.rng);
	let recovery_key = code.signing_key().verifying_key();
	let stating = |family: &mut Family, by: usize, member: usize| {
		let stated = RecoveryStatement::sign(&code, family.members[member]);
		let operation = state_recovery_code(family.id, &stated, &mut family.rng);
		family.commit(by, vec![operation])
	};
	let revoking = |family: &mut Family, by: usize, member: usize| {
		let record = recovery_record(family.id, family.members[member], &recovery_key);
		let operation = revoke_recovery_code(record, &recovery_key, &mut family.rng);
		family.commit(by, vec![operation])
	};
	let standing = |family: &Family, member: usize| {
		let record = recovery_record(family.id, family.members[member], &recovery_key);
		let roster = family.devices[0].log.roster(&family.keys).unwrap();
		roster.code(record).map(nibkeep_governance::Code::stands)
	};
	// The founder, an admin, states a code for the sister: that names nothing.
	let by_founder = stating(&mut family, 0, SISTER);
	family.sync_all();
	assert!(!family.counts(&by_founder));
	assert_eq!(standing(&family, SISTER), None);
	// The sister, a viewer, states it herself, and it counts.
	let by_sister = stating(&mut family, 2, SISTER);
	family.sync_all();
	assert!(family.counts(&by_sister));
	assert_eq!(standing(&family, SISTER), Some(true));
	// Nor does she state it on a record that is not hers for it.
	let elsewhere = family.rng.entity_id();
	let text = Value::Text(RecoveryStatement::sign(&code, family.members[SISTER]).to_text());
	let operation = family.assert(elsewhere, fields::RECOVERY_KEY, text);
	let misplaced = family.commit(2, vec![operation]);
	family.sync_all();
	assert!(!family.counts(&misplaced));
	// The entry that the code signed is hers to state too, as her code is.
	let signed = RecoveryEntry::sign(
		&code,
		(
			family.id,
			family.members[SISTER],
			Hash::of(b"a first change"),
		),
		Hash::of(b"a first key"),
	);
	let entry = |family: &mut Family, by: usize| {
		let operation = state_recovery_entry(&signed, &mut family.rng);
		family.commit(by, vec![operation])
	};
	let by_founder = entry(&mut family, 0);
	family.sync_all();
	assert!(!family.counts(&by_founder));
	let by_sister = entry(&mut family, 2);
	family.sync_all();
	assert!(family.counts(&by_sister));
	// The partner copies her code onto a record of his own: that is his to do, and it
	// leaves hers as it was.
	let copied = stating(&mut family, 1, PARTNER);
	family.sync_all();
	assert!(family.counts(&copied));
	// The founder and the partner revoke the sister's code: neither counts.
	for by in [0, 1] {
		let by_other = revoking(&mut family, by, SISTER);
		family.sync_all();
		assert!(!family.counts(&by_other));
	}
	assert_eq!(standing(&family, SISTER), Some(true));
	// The partner revokes his copy, which leaves hers standing.
	let his = revoking(&mut family, 1, PARTNER);
	family.sync_all();
	assert!(family.counts(&his));
	assert_eq!(standing(&family, PARTNER), Some(false));
	assert_eq!(standing(&family, SISTER), Some(true));
	// She revokes her own.
	let hers = revoking(&mut family, 2, SISTER);
	family.sync_all();
	assert!(family.counts(&hers));
	assert_eq!(standing(&family, SISTER), Some(false));
}

#[test]
fn revoking_a_code_revokes_no_replicas_key() {
	// The partner names the sister's device key as a recovery key of his, and revokes it on
	// his record for it: her device stays.
	let mut family = family("no replica revoked");
	let sisters = family.devices[2].key.verifying_key();
	let record = recovery_record(family.id, family.members[PARTNER], &sisters);
	let operation = revoke_recovery_code(record, &sisters, &mut family.rng);
	let revocation = family.commit(1, vec![operation]);
	family.sync_all();
	assert!(family.counts(&revocation));
	let sisters_record = family.record(2);
	let named = family.name(2, sisters_record, "Her phone", &[]);
	family.sync_all();
	assert!(family.counts(&named));
	// The same statement as a replica's revocation counts for nobody but her own replicas.
	let operation = family.assert(record, fields::REVOKED, Value::Text(sisters.to_text()));
	let revocation = family.commit(1, vec![operation]);
	family.sync_all();
	assert!(!family.counts(&revocation));
}

#[test]
fn two_records_that_name_one_user_are_two_members() {
	let mut family = Family::new("one user twice", &[FOUNDER, PARTNER, SISTER]);
	family.invite(0, 1, EDITOR);
	// The sister's record names the partner's user ID, as a viewer: her device's rights
	// follow her record.
	let partners_user = family.devices[0]
		.log
		.state()
		.current(family.members[PARTNER], &fields::USER_ID)
		.infallible()
		.into_iter()
		.next()
		.expect("a user ID")
		.value
		.clone();
	let sister = family.members[SISTER];
	let mut operations = vec![
		family.assert(sister, fields::USER_ID, partners_user),
		family.assert(sister, fields::ROLE, Value::Text(VIEWER.to_owned())),
	];
	operations.extend(family.listing(2));
	family.commit(0, operations);
	family.sync_all();
	let (sisters, _) = family.caption(2, "a viewer's");
	let (partners, _) = family.caption(1, "an editor's");
	family.sync_all();
	assert!(!family.counts(&sisters));
	assert!(family.counts(&partners));
}

/// What each device decides, whatever order the changes reach it in.
#[test]
fn concurrent_changes_to_roles_end_the_same_in_any_order() {
	for seed in 0..6_u64 {
		let mut family = family(&format!("orders {seed}"));
		family.give_role(0, PARTNER, ADMIN);
		family.sync_all();
		// Offline: the partner makes the sister an editor and she captions as one; the
		// founder makes the partner a viewer.
		let promoting = family.give_role(1, SISTER, EDITOR);
		family.sync(1, 2);
		let (sisters, _) = family.caption(2, "as an editor");
		let demoting = family.give_role(0, PARTNER, VIEWER);
		let (partners, _) = family.caption(1, "as an admin");
		// The changes reach the others in an order of the seed's.
		let mut order: Vec<(usize, usize)> = vec![(2, 0), (1, 0), (0, 1), (0, 2), (2, 1), (1, 2)];
		order.rotate_left(usize::try_from(seed).expect("small"));
		for (from, to) in order {
			family.sync(from, to);
		}
		family.sync_all();
		assert!(family.counts(&demoting));
		assert!(
			!family.counts(&promoting),
			"made without knowing of the demotion"
		);
		assert!(!family.counts(&sisters), "a role nobody gave her");
		assert!(!family.counts(&partners));
	}
}

#[test]
fn what_a_governing_change_replaces_needs_the_role_its_entity_needs() {
	let mut family = family("governing replacements");
	let (_, caption) = family.caption(0, "Grandma at the lake");
	family.sync_all();
	// The sister gives a key, as any member may, on the photo, replacing its caption; and
	// states a role nobody knows on the founder's member, replacing nothing.
	let photo = family.photo;
	let gift = Statement::new(
		family.rng.statement_id(),
		photo,
		fields::SPACE_KEY,
		text("not a key"),
	);
	let replacing = family.commit(2, vec![Operation::replace(gift, [caption])]);
	let founder = family.members[FOUNDER];
	let unknown = family.assert(founder, fields::ROLE, text("queen"));
	let unknown_role = family.commit(2, vec![unknown]);
	family.sync_all();
	assert!(!family.counts(&replacing));
	assert!(!family.counts(&unknown_role));
	assert_eq!(family.captions(), ["Grandma at the lake"]);
}

#[test]
fn a_second_owner_on_a_listing_takes_no_replica_over() {
	let mut family = family("two owners");
	// The sister puts the partner's own listing of the partner's phone in a change of hers,
	// with her member as the owner first and the partner's after.
	let partners = family.listing(1);
	let sister = family.members[SISTER];
	let record = family.record(1);
	let mut taking = vec![family.assert(record, fields::OWNER, Value::Reference(sister))];
	taking.extend(partners);
	let taking = family.commit(2, taking);
	family.sync_all();
	assert!(!family.counts(&taking));
	// The partner's phone is still the partner's, an editor.
	let (partners, _) = family.caption(1, "still the partner's");
	family.sync_all();
	assert!(family.counts(&partners));
}

#[test]
fn an_invitation_lists_a_replica_only_for_a_member_it_makes() {
	// The founder's second device is listed by the founder as the partner's, on the
	// partner's record, which the change claims to make again.
	let mut family = Family::new("existing members", &[FOUNDER, PARTNER, PARTNER]);
	family.invite(0, 1, EDITOR);
	family.sync_all();
	let partner = family.members[PARTNER];
	let user = family.assert(partner, fields::USER_ID, text("someone else"));
	let mut claiming = vec![user];
	claiming.extend(family.listing(2));
	let claiming = family.commit(0, claiming);
	family.sync_all();
	assert!(!family.counts(&claiming));
}

#[test]
fn whose_records_entities_are_follows_what_a_change_builds_on() {
	for first in [0, 2] {
		let mut family = family(&format!("records known {first}"));
		// The partner names an entity; the founder, not knowing, makes it a member's record.
		let entity = family.rng.entity_id();
		let named = family.name(1, entity, "a box of prints", &[]);
		let mut making = family.making_member(0, VIEWER);
		for operation in &mut making {
			if let Operation::Assert { statement, .. } = operation {
				statement.entity = entity;
			}
		}
		family.commit(0, making);
		// The others take them in either order.
		if first == 0 {
			family.sync(0, 2);
			family.sync(1, 2);
		} else {
			family.sync(1, 2);
			family.sync(0, 2);
		}
		family.sync_all();
		assert!(
			family.counts(&named),
			"it was no member's record when named"
		);
	}
}

#[test]
fn an_operation_of_an_unknown_kind_needs_an_admin() {
	use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
	let mut family = family("unknown operations");
	let unknown: Operation =
		cbor::from_slice(&cbor::encode(&Cbor::Array(vec![9_u64.to_cbor()]))).expect("reads");
	let by_editor = family.commit(1, vec![unknown.clone()]);
	let by_admin = family.commit(0, vec![unknown]);
	family.sync_all();
	assert!(!family.counts(&by_editor));
	assert!(family.counts(&by_admin));
}

impl Family {
	/// The governing operations that list a device's key as an AI agent's, owned by its
	/// member, with the listing that its key signed, which says so too.
	fn ai_listing(&mut self, device: usize) -> Vec<Operation> {
		let member = self.members[self.member_of[device]];
		let key = (&self.devices[device].key, &self.encryptions[device]);
		common::listing(&mut self.rng, self.id, key, (member, true))
	}

	/// Device `by` says whether the space allows AI agents.
	fn allow_ai_agents(&mut self, by: usize, allow: bool) -> Parcel {
		let space = self.photo;
		let operation = self.assert(space, fields::AI_AGENTS, Value::Boolean(allow));
		self.commit(by, vec![operation])
	}
}

#[test]
fn an_ai_agents_listing_counts_only_where_the_admins_allow_ai_agents() {
	// The partner's AI agent is the fourth device.
	let mut family = Family::new("AI agents allowed", &[FOUNDER, PARTNER, SISTER, PARTNER]);
	family.invite(0, 1, EDITOR);
	family.sync_all();
	// Its listing counts for nothing while the space allows no AI agents.
	let operations = family.ai_listing(3);
	let listed = family.commit(1, operations);
	family.sync_all();
	assert!(!family.counts(&listed));
	// Only an admin allows them.
	let by_partner = family.allow_ai_agents(1, true);
	family.sync_all();
	assert!(!family.counts(&by_partner));
	let by_founder = family.allow_ai_agents(0, true);
	family.sync_all();
	assert!(family.counts(&by_founder));
	// Listed now, it counts, and what the AI agent writes counts as its user's.
	let operations = family.ai_listing(3);
	let listed = family.commit(1, operations);
	family.sync_all();
	assert!(family.counts(&listed));
	let (caption, _) = family.caption(3, "Written by an AI agent");
	family.sync_all();
	assert!(family.counts(&caption));
	// Once the admins stop allowing them, no AI agent is listed anew.
	let forbidding = family.allow_ai_agents(0, false);
	family.sync_all();
	assert!(family.counts(&forbidding));
	let operations = family.ai_listing(3);
	let again = family.commit(1, operations);
	family.sync_all();
	assert!(!family.counts(&again));
}

const NONE: &str = "none";

#[test]
fn a_member_who_leaves_or_is_removed_counts_no_more_but_for_what_they_did_without_knowing() {
	let mut family = family("leaving and removing");
	let (before, _) = family.caption(1, "before");
	family.sync_all();
	// The founder removes the partner, who captions meanwhile, not knowing.
	let removing = family.give_role(0, PARTNER, NONE);
	let (unknowing, _) = family.caption(1, "meanwhile");
	family.sync_all();
	assert!(family.counts(&removing));
	assert!(family.counts(&before), "what the removal builds on stays");
	assert!(
		!family.counts(&unknowing),
		"set aside, for an admin to review"
	);
	// Once the partner knows, nothing of theirs counts, not even on their own records.
	let (knowing, _) = family.caption(1, "after");
	let partner = family.members[PARTNER];
	let renaming = family.name(1, partner, "Partner", &[]);
	family.sync_all();
	assert!(!family.counts(&knowing));
	assert!(!family.counts(&renaming));
	assert_eq!(family.captions(), ["before"]);
	// The sister leaves, as any member may: that counts, and she changes nothing after.
	let leaving = family.give_role(2, SISTER, NONE);
	family.sync_all();
	assert!(family.counts(&leaving));
	let sister = family.members[SISTER];
	let renaming = family.name(2, sister, "Sister", &[]);
	family.sync_all();
	assert!(!family.counts(&renaming));
	// Both are removed, on every device, by the change that removed them.
	let keys = family.keys.clone();
	for device in &family.devices {
		let log = &device.log;
		for (gone, by) in [(1, &removing), (2, &leaving)] {
			let key = family.devices[gone].key.verifying_key();
			assert!(log.is_removed(&key, &keys).expect("reads"));
			assert_eq!(log.removals_of(&key, &keys).expect("reads"), vec![by.id()]);
		}
		let founder = family.devices[0].key.verifying_key();
		assert!(!log.is_removed(&founder, &keys).expect("reads"));
		assert_eq!(
			log.removals_of(&founder, &keys).expect("reads"),
			Vec::<ChangeId>::new()
		);
		let mut removals = vec![removing.id(), leaving.id()];
		removals.sort();
		assert_eq!(log.removals(&keys).expect("reads"), removals);
	}
}

#[test]
fn what_a_removed_admin_did_without_knowing_is_set_aside_members_they_added_included() {
	// The partner's cousin is the fourth member.
	let mut family = Family::new(
		"a removed admin's additions",
		&[FOUNDER, PARTNER, SISTER, 3],
	);
	family.invite(0, 1, ADMIN);
	family.invite(0, 2, VIEWER);
	family.sync_all();
	// The partner invites the cousin and promotes the sister, while the founder, the senior
	// admin, removes the partner.
	let adding = family.invite(1, 3, EDITOR);
	let promoting = family.give_role(1, SISTER, EDITOR);
	let removing = family.give_role(0, PARTNER, NONE);
	family.sync_all();
	assert!(family.counts(&removing));
	assert!(!family.counts(&adding));
	assert!(!family.counts(&promoting));
	let (cousins, _) = family.caption(3, "the cousin's");
	family.sync_all();
	assert!(!family.counts(&cousins));
}

#[test]
fn the_last_member_leaving_closes_the_space() {
	let mut family = Family::new("the last member", &[FOUNDER]);
	let leaving = family.give_role(0, FOUNDER, NONE);
	assert!(family.counts(&leaving), "nobody is left without an admin");
	let (after, _) = family.caption(0, "after");
	assert!(!family.counts(&after));
	let founder = family.devices[0].key.verifying_key();
	let keys = family.keys.clone();
	assert!(
		family.devices[0]
			.log
			.is_removed(&founder, &keys)
			.expect("reads")
	);
}

#[test]
fn once_the_admins_stop_allowing_ai_agents_an_ai_agents_changes_count_no_more() {
	// The partner's AI agent is the fourth device.
	let mut family = Family::new("AI agents stopped", &[FOUNDER, PARTNER, SISTER, PARTNER]);
	family.invite(0, 1, EDITOR);
	family.allow_ai_agents(0, true);
	family.sync_all();
	let operations = family.ai_listing(3);
	family.commit(1, operations);
	family.sync_all();
	let (before, _) = family.caption(3, "before");
	family.sync_all();
	// The founder stops allowing them while the AI agent captions, not knowing.
	let forbidding = family.allow_ai_agents(0, false);
	let (unknowing, _) = family.caption(3, "meanwhile");
	family.sync_all();
	assert!(family.counts(&before));
	assert!(
		!family.counts(&unknowing),
		"set aside, as a removal sets aside"
	);
	let (knowing, _) = family.caption(3, "after");
	family.sync_all();
	assert!(!family.counts(&knowing));
	let keys = family.keys.clone();
	let ai_agent = family.devices[3].key.verifying_key();
	let partner = family.devices[1].key.verifying_key();
	for device in &family.devices {
		assert!(device.log.is_removed(&ai_agent, &keys).expect("reads"));
		assert!(!device.log.is_removed(&partner, &keys).expect("reads"));
		assert_eq!(
			device.log.removals_of(&ai_agent, &keys).expect("reads"),
			vec![forbidding.id()]
		);
		assert_eq!(
			device.log.removals(&keys).expect("reads"),
			vec![forbidding.id()]
		);
	}
	// Allowed again, what it writes then counts again.
	family.allow_ai_agents(0, true);
	family.sync_all();
	let (again, _) = family.caption(3, "again");
	family.sync_all();
	assert!(family.counts(&again));
	assert!(
		!family.devices[0]
			.log
			.is_removed(&ai_agent, &keys)
			.expect("reads")
	);
	// Stopped once more, only that ends it: what it wrote while they were allowed again
	// builds on the first stop, and still counts.
	let forbidding_again = family.allow_ai_agents(0, false);
	family.sync_all();
	assert!(family.counts(&again));
	for device in &family.devices {
		assert_eq!(
			device.log.removals_of(&ai_agent, &keys).expect("reads"),
			vec![forbidding_again.id()]
		);
	}
}

#[test]
fn a_governing_change_is_a_members_by_what_it_builds_on_whatever_sets_it_aside_later() {
	let mut family = family("whose changes are a member's");
	// The partner gives a key while the founder removes them: the gift is set aside, but it
	// was a member's, by what it builds on.
	let removing = family.give_role(0, PARTNER, NONE);
	let record = family.record(1);
	let gift = family.assert(record, fields::SPACE_KEY, text("a key"));
	let unknowing = family.commit(1, vec![gift]);
	family.sync_all();
	assert!(family.counts(&removing));
	assert!(!family.counts(&unknowing));
	let keys = family.keys.clone();
	for device in &family.devices {
		assert!(
			device
				.log
				.author_was_a_member(unknowing.id(), &keys)
				.expect("reads")
		);
	}
	// One made after the partner knew of it was no member's.
	let gift = family.assert(record, fields::SPACE_KEY, text("another key"));
	let knowing = family.commit(1, vec![gift]);
	family.sync_all();
	assert!(!family.counts(&knowing));
	for device in &family.devices {
		assert!(
			!device
				.log
				.author_was_a_member(knowing.id(), &keys)
				.expect("reads")
		);
	}
	// What counts was a member's.
	assert!(
		family.devices[0]
			.log
			.author_was_a_member(removing.id(), &keys)
			.expect("reads")
	);
}

#[test]
fn an_editor_replaces_a_secret_value_whose_entity_only_those_with_the_secrets_see() {
	const PASSWORD: &str = "org.example.password";
	let mut family = family("secret values");
	let secrets = family.rng.key();
	family.keys.insert_secrets(0, secrets);
	for device in &mut family.devices {
		device.log.set_secret_fields([common::field(PASSWORD)]);
	}
	let login = family.rng.entity_id();
	let set = |family: &mut Family, by: usize, password: &str, replacing: &[StatementId]| {
		let statement = Statement::new(
			family.rng.statement_id(),
			login,
			common::field(PASSWORD),
			text(password),
		);
		let id = statement.id;
		let operation = Operation::replace(statement, replacing.iter().copied());
		(family.commit(by, vec![operation]), id)
	};
	let (first, first_id) = set(&mut family, 0, "hunter2", &[]);
	family.sync_all();
	// The editor replaces it: what it takes away is a retraction of secret statements, which
	// names no entity, as the statement's entity is sealed with it. The viewer's does not
	// count.
	let (editors, _) = set(&mut family, 1, "correct horse", &[first_id]);
	let (viewers, _) = set(&mut family, 2, "staple", &[first_id]);
	family.sync_all();
	assert!(family.counts(&first));
	assert!(family.counts(&editors));
	assert!(!family.counts(&viewers));
	assert_eq!(
		texts(&family.devices[0].log, login, PASSWORD),
		["correct horse"]
	);

	// The editor seals a statement under the ID of the founder's name, on the founder's own
	// record, which only an admin may change: a retraction of secret statements takes away
	// only the secret one, so the name stays, on every device.
	let founder = family.members[FOUNDER];
	let name = Statement::new(
		family.rng.statement_id(),
		founder,
		fields::NAME,
		text("Ada"),
	);
	let name_id = name.id;
	family.commit(0, vec![Operation::assert(name)]);
	family.sync_all();
	let reusing = Statement::new(name_id, login, common::field(PASSWORD), text("staple"));
	let sealing = family.commit(1, vec![Operation::assert(reusing)]);
	family.sync_all();
	let taking = family.commit(1, vec![Operation::retract_secret([name_id])]);
	family.sync_all();
	assert!(family.counts(&sealing));
	assert!(family.counts(&taking));
	for device in &family.devices {
		assert_eq!(texts(&device.log, founder, "org.nibkeep.name"), ["Ada"]);
	}
	// A retraction of the earlier form, which takes away a statement of any entity, needs an
	// admin, whatever it takes away.
	let earlier = family.commit(
		1,
		vec![Operation::Retract {
			entity: None,
			statements: [name_id].into(),
		}],
	);
	let (_, caption) = family.caption(0, "Grandma at the lake");
	family.sync_all();
	let taking = family.commit(
		1,
		vec![Operation::Retract {
			entity: None,
			statements: [caption].into(),
		}],
	);
	family.sync_all();
	assert!(!family.counts(&earlier));
	assert!(!family.counts(&taking));
	assert_eq!(family.captions(), ["Grandma at the lake"]);
	for device in &family.devices {
		assert_eq!(texts(&device.log, founder, "org.nibkeep.name"), ["Ada"]);
	}
}

#[test]
fn only_an_admin_takes_the_secrets_from_itself_which_sets_them_up() {
	let mut family = family("setting up the secrets");
	let (admins, editors) = (family.record(0), family.record(1));
	let stating = |family: &mut Family, by: usize, record: EntityId, from: EntityId| {
		let operation = family.assert(record, fields::SECRETS_FROM, Value::Reference(from));
		family.commit(by, vec![operation])
	};
	let editors_setup = stating(&mut family, 1, editors, editors);
	let asking = stating(&mut family, 1, editors, admins);
	let admins_setup = stating(&mut family, 0, admins, admins);
	family.sync_all();
	assert!(!family.counts(&editors_setup));
	assert!(family.counts(&asking));
	assert!(family.counts(&admins_setup));
}
