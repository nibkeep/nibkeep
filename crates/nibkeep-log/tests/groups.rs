//! Groups in a space: a group is a space with members and no data of its own, and a space
//! follows the governing changes of each group in it, which take effect there at the space's
//! governing changes that name them, its anchors ([technical design: groups]). A group is a member
//! only once admins of both agree, and never an admin; through a group, a person has the
//! lower of the two roles, and over every path the highest; a removal from a group takes
//! effect in a space at its first anchors.
//!
//! The people of these tests: the mother, an admin of the household's group and of the
//! family's space; her son, an editor in the household; the grandmother, a viewer of the
//! family; the father, the household's second admin; and a cousin, whom the father adds.
//!
//! [technical design: groups]: ../../../docs/Technical-design.md#138-groups

mod common;

use common::{DRIFT, counts_in, pair, send, text, texts};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::RecoveryCode;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, SigningKey};
use nibkeep_governance::{
	Anchor, GroupRef, Joining, RecoveryListing, RecoveryStatement, list_by_recovery_code,
	replica_record, state_recovery_code,
};
use nibkeep_log::{ChangeId, Log, LogError, Parcel, SpaceKeys};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const CAPTION: &str = "org.example.caption";

const ADMIN: &str = "admin";
const EDITOR: &str = "editor";
const VIEWER: &str = "viewer";
const NONE: &str = "none";

/// The people, each with a record in the space and one in the group.
const MOTHER: usize = 0;
const SON: usize = 1;
const GRANDMA: usize = 2;
const FATHER: usize = 3;
const COUSIN: usize = 4;
const PEOPLE: usize = 5;

/// A log of each: the family's space, and the household's group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
	Space,
	Group,
}

/// A device of one person's, with its keys and its copies of both logs.
struct Device {
	key: SigningKey,
	encryption: EncryptionKey,
	person: usize,
	space: Log,
	group: Log,
}

impl Device {
	fn log(&self, which: Which) -> &Log {
		match which {
			Which::Space => &self.space,
			Which::Group => &self.group,
		}
	}

	fn log_mut(&mut self, which: Which) -> &mut Log {
		match which {
			Which::Space => &mut self.space,
			Which::Group => &mut self.group,
		}
	}
}

/// A space or a group: its ID, its keys, its first change, and each person's record in it.
struct World {
	id: SpaceId,
	keys: SpaceKeys,
	root: Option<ChangeId>,
	members: Vec<EntityId>,
}

/// The family's space and the household's group, with the devices that hold them. Device 0,
/// the mother's, founds both.
struct Families {
	rng: TestRng,
	now: u64,
	space: World,
	group: World,
	devices: Vec<Device>,
	/// A photo, which the tests caption.
	photo: EntityId,
	/// An entity that the group's agreements and withdrawals are stated on.
	agreements: EntityId,
	/// The household's record in the space, and the statement that adds it, once added.
	addition: Option<(EntityId, StatementId)>,
}

impl Families {
	/// Devices of the people that `people` names, one for each; the first, the mother's,
	/// founds the space and the group, as their admin. Nobody else is a member yet.
	fn new(seed: &str, people: &[usize]) -> Self {
		assert_eq!(people[0], MOTHER);
		let mut rng = TestRng::new(seed);
		let world = |rng: &mut TestRng| World {
			id: rng.space_id(),
			keys: SpaceKeys::new(0, rng.key()),
			root: None,
			members: (0..PEOPLE).map(|_| rng.entity_id()).collect(),
		};
		let space = world(&mut rng);
		let group = world(&mut rng);
		let devices = people
			.iter()
			.map(|&person| {
				let mut space_log = Log::new(space.id, Clock::new(DRIFT));
				space_log.expect_governed();
				let mut group_log = Log::new(group.id, Clock::new(DRIFT));
				group_log.expect_governed();
				Device {
					key: rng.signing_key(),
					encryption: DecryptionKey::generate(&mut rng).encryption_key(),
					person,
					space: space_log,
					group: group_log,
				}
			})
			.collect();
		let photo = rng.entity_id();
		let agreements = rng.entity_id();
		let mut families = Self {
			rng,
			now: millis_at("2026-11-07", 9, 0),
			space,
			group,
			devices,
			photo,
			agreements,
			addition: None,
		};
		for which in [Which::Space, Which::Group] {
			let mut founding = families.making_member(which, MOTHER, ADMIN);
			founding.extend(families.listing(which, 0));
			let parcel = families.commit(which, 0, founding);
			families.world_mut(which).root = Some(parcel.id());
		}
		families
	}

	fn world(&self, which: Which) -> &World {
		match which {
			Which::Space => &self.space,
			Which::Group => &self.group,
		}
	}

	fn world_mut(&mut self, which: Which) -> &mut World {
		match which {
			Which::Space => &mut self.space,
			Which::Group => &mut self.group,
		}
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn assert(&mut self, entity: EntityId, field: FieldName, value: Value) -> Operation {
		common::assert(&mut self.rng, entity, field, value)
	}

	/// The governing operations that make a person's record, with a user ID and a role.
	fn making_member(&mut self, which: Which, person: usize, role: &str) -> Vec<Operation> {
		let record = self.world(which).members[person];
		common::making_member(&mut self.rng, record, role)
	}

	/// The governing operations that list a device's key, owned by its person's record, with
	/// the listing that its key signed; marked as an AI agent's with `ai_agent`.
	fn signed_listing(&mut self, which: Which, device: usize, ai_agent: bool) -> Vec<Operation> {
		let (space, members) = (self.world(which).id, &self.world(which).members);
		let device = &self.devices[device];
		let member = (members[device.person], ai_agent);
		common::listing(
			&mut self.rng,
			space,
			(&device.key, &device.encryption),
			member,
		)
	}

	fn listing(&mut self, which: Which, device: usize) -> Vec<Operation> {
		self.signed_listing(which, device, false)
	}

	fn commit(&mut self, which: Which, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.world(which).keys.clone();
		let device = &mut self.devices[device];
		let key = device.key.clone();
		device
			.log_mut(which)
			.commit(&key, &keys, operations, wall, &mut self.rng)
			.expect("commits")
	}

	/// Device `by` invites device `device`'s person with `role`, and the device then holds
	/// what `by` has.
	fn invite(&mut self, which: Which, by: usize, device: usize, role: &str) -> Parcel {
		let mut operations = self.making_member(which, self.devices[device].person, role);
		operations.extend(self.listing(which, device));
		let parcel = self.commit(which, by, operations);
		self.sync(which, by, device);
		parcel
	}

	/// Device `by` lists device `device`, a replica of a person who has a record already.
	fn list(&mut self, which: Which, by: usize, device: usize) -> Parcel {
		let operations = self.listing(which, device);
		self.commit(which, by, operations)
	}

	/// Device `by` gives a person a role.
	fn give_role(&mut self, which: Which, by: usize, person: usize, role: &str) -> Parcel {
		let record = self.world(which).members[person];
		self.give_role_to(which, by, record, role)
	}

	/// Device `by` gives a record a role.
	fn give_role_to(&mut self, which: Which, by: usize, record: EntityId, role: &str) -> Parcel {
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(which, by, vec![operation])
	}

	/// Device `by` revokes device `device`'s key.
	fn revoke(&mut self, which: Which, by: usize, device: usize) -> Parcel {
		let key = self.devices[device].key.verifying_key();
		let record = replica_record(self.world(which).id, &key);
		let operation = self.assert(record, fields::REVOKED, Value::Text(key.to_text()));
		self.commit(which, by, vec![operation])
	}

	/// Device `by` adds the household to the family's space with `role`: a record for the
	/// group, with the statement that adds it.
	fn add_group(&mut self, by: usize, role: &str) -> Parcel {
		let root = self.group.root.expect("founded");
		self.add_group_rooted(by, role, root)
	}

	/// The same, naming `root` as the group's first change.
	fn add_group_rooted(&mut self, by: usize, role: &str, root: ChangeId) -> Parcel {
		let record = self.rng.entity_id();
		let group = GroupRef {
			group: self.group.id,
			root,
			key: nibkeep_crypto::key_hash(self.group.keys.space_keys().next().expect("a key").1),
		};
		let addition = self.rng.statement_id();
		let operations = vec![
			Operation::assert(Statement::new(
				addition,
				record,
				fields::GROUP,
				Value::Text(group.to_text()),
			)),
			self.assert(record, fields::ROLE, Value::Text(role.to_owned())),
		];
		self.addition = Some((record, addition));
		self.commit(Which::Space, by, operations)
	}

	/// The household's record in the space.
	fn group_record(&self) -> EntityId {
		self.addition.expect("added").0
	}

	/// The joining that an agreement for the household's addition names.
	fn joining(&self) -> Joining {
		Joining {
			space: self.space.id,
			root: self.space.root.expect("founded"),
			addition: self.addition.expect("added").1,
		}
	}

	/// Device `by` agrees, in the household, to a joining.
	fn agree_to(&mut self, by: usize, joining: Joining) -> Parcel {
		let entity = self.agreements;
		let operation = self.assert(entity, fields::JOINED, Value::Text(joining.to_text()));
		self.commit(Which::Group, by, vec![operation])
	}

	/// Device `by` agrees, in the household, to its addition to the family.
	fn agree(&mut self, by: usize) -> Parcel {
		let joining = self.joining();
		self.agree_to(by, joining)
	}

	/// Device `by` withdraws the household's agreement to its addition to the family.
	fn withdraw(&mut self, by: usize) -> Parcel {
		let (entity, joining) = (self.agreements, self.joining());
		let operation = self.assert(entity, fields::WITHDRAWN, Value::Text(joining.to_text()));
		self.commit(Which::Group, by, vec![operation])
	}

	/// The household's governing changes that no other of them builds on, as device `of`
	/// holds them.
	fn group_heads(&self, of: usize) -> Vec<ChangeId> {
		self.devices[of]
			.group
			.followed(&self.group.keys)
			.expect("reads the group")
			.heads()
	}

	/// Device `by` anchors the household's changes that it holds in the family's space.
	fn anchor(&mut self, by: usize) -> Parcel {
		self.follow(by);
		let changes = self.group_heads(by);
		self.anchor_naming(by, changes)
	}

	/// Device `by` anchors these changes of the household in the family's space.
	fn anchor_naming(&mut self, by: usize, changes: Vec<ChangeId>) -> Parcel {
		let anchor = Anchor {
			group: self.group.id,
			changes,
		};
		let entity = self.group_record();
		let operation = self.assert(entity, fields::ANCHOR, Value::Text(anchor.to_text()));
		self.commit(Which::Space, by, vec![operation])
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> Parcel {
		let operation = Operation::assert(Statement::new(
			self.rng.statement_id(),
			self.photo,
			FieldName::from_static(CAPTION),
			text(caption),
		));
		self.commit(Which::Space, by, vec![operation])
	}

	/// Device `by` purges a statement of the space.
	fn purge(&mut self, by: usize, statement: StatementId) -> Parcel {
		let (wall, keys) = (self.tick(), self.space.keys.clone());
		let device = &mut self.devices[by];
		let key = device.key.clone();
		device
			.space
			.purge(&key, &keys, [statement], wall, &mut self.rng)
			.expect("makes the purge")
	}

	/// Device `by` names an entity of the space.
	fn name(&mut self, by: usize, entity: EntityId, name: &str) -> Parcel {
		let operation = self.assert(entity, fields::NAME, text(name));
		self.commit(Which::Space, by, vec![operation])
	}

	/// A device's replica record in the space, derived from the space and its key.
	fn record_in_space(&self, device: usize) -> EntityId {
		replica_record(self.space.id, &self.devices[device].key.verifying_key())
	}

	/// Device `of` follows the household in the family's space, as its copy of the group's log
	/// has it.
	fn follow(&mut self, of: usize) {
		let (group_keys, space_keys) = (self.group.keys.clone(), self.space.keys.clone());
		let device = &mut self.devices[of];
		let followed = device.group.followed(&group_keys).expect("reads the group");
		device
			.space
			.follow(vec![followed], &space_keys)
			.expect("follows the group");
	}

	/// Device `to` receives every change of device `from`'s copy of a log, parents first, and
	/// every wrapped key, as a sync sends them; a space's sync carries its groups' logs too,
	/// so a device follows the group before it takes the space's changes. Changes of keys it
	/// knows no listing of are refused, as a sync drops them.
	fn sync(&mut self, which: Which, from: usize, to: usize) {
		if which == Which::Space {
			self.sync(Which::Group, from, to);
		}
		let wall = self.tick();
		let keys = self.world(which).keys.clone();
		let receiver = to;
		let (from, to) = pair(&mut self.devices, from, to);
		send(from.log(which), to.log_mut(which), &keys, wall);
		if which == Which::Group {
			self.follow(receiver);
		}
	}

	/// Every device receives what every other has, of both logs.
	fn sync_all(&mut self) {
		let count = self.devices.len();
		for _ in 0..2 {
			for from in 0..count {
				for to in 0..count {
					if from != to {
						self.sync(Which::Space, from, to);
					}
				}
			}
		}
	}

	/// Whether a change of the space counts on every device that holds it, which must agree.
	fn counts(&self, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.space), change)
	}

	/// The captions that a device shows, sorted.
	fn captions(&self, of: usize) -> Vec<String> {
		let mut captions = texts(&self.devices[of].space, self.photo, CAPTION);
		captions.sort();
		captions
	}
}

/// The household, with the son as an editor, and the family, with the grandmother as a
/// viewer, and the household in the family with `role`, agreed and anchored by the mother.
fn household_in_family(seed: &str, people: &[usize], role: &str) -> Families {
	let mut families = Families::new(seed, people);
	families.invite(Which::Group, 0, 1, EDITOR);
	if people.get(2) == Some(&GRANDMA) {
		families.invite(Which::Space, 0, 2, VIEWER);
	}
	families.add_group(0, role);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	families
}

#[test]
fn a_group_counts_in_a_space_once_both_sides_agree_and_an_anchor_names_the_agreement() {
	let mut families = Families::new("agreeing", &[MOTHER, SON, GRANDMA]);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.invite(Which::Space, 0, 2, VIEWER);
	// The mother adds the household to the family, as editors: the son's caption does not
	// count yet.
	let adding = families.add_group(0, EDITOR);
	families.sync_all();
	let early = families.caption(1, "before the agreement");
	families.sync_all();
	assert!(families.counts(&adding));
	assert!(!families.counts(&early));
	// She agrees in the household: still not, until an anchor in the family names it.
	families.agree(0);
	families.sync_all();
	let unanchored = families.caption(1, "agreed, not anchored");
	families.sync_all();
	assert!(!families.counts(&unanchored));
	let anchoring = families.anchor(0);
	families.sync_all();
	let late = families.caption(1, "after the anchor");
	families.sync_all();
	assert!(families.counts(&anchoring));
	assert!(families.counts(&late));
	assert!(!families.counts(&early) && !families.counts(&unanchored));
	assert_eq!(families.captions(2), ["after the anchor"]);
}

#[test]
fn through_a_group_a_person_has_the_lower_role_and_over_every_path_the_highest() {
	// The mother's laptop, the fourth device, is listed in the household alone.
	let mut families = Families::new("paths", &[MOTHER, SON, GRANDMA, MOTHER]);
	families.list(Which::Group, 0, 3);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.invite(Which::Space, 0, 2, VIEWER);
	families.add_group(0, VIEWER);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	// Through the household, the son is a viewer of the family: his caption does not count,
	// and naming his own phone's record there does.
	let caption = families.caption(1, "a viewer's");
	let phone = families.record_in_space(1);
	let naming = families.name(1, phone, "The son's phone");
	families.sync_all();
	assert!(!families.counts(&caption));
	assert!(families.counts(&naming));
	// The laptop is one member with the mother's phone, joined by its key, which both list:
	// an admin, so making the grandmother an editor from it counts.
	let promoting = families.give_role(Which::Space, 3, GRANDMA, EDITOR);
	families.sync_all();
	assert!(families.counts(&promoting));
	// Made editors, the household gives the son an editor's role.
	let record = families.group_record();
	families.give_role_to(Which::Space, 0, record, EDITOR);
	families.sync_all();
	let editing = families.caption(1, "an editor's");
	families.sync_all();
	assert!(families.counts(&editing));
	// A viewer in the household, he is a viewer of the family again: the lower of the two.
	families.give_role(Which::Group, 0, SON, VIEWER);
	families.sync_all();
	families.anchor(0);
	families.sync_all();
	let viewing = families.caption(1, "a viewer's again");
	families.sync_all();
	assert!(!families.counts(&viewing));
	assert_eq!(families.captions(0), ["an editor's"]);
}

#[test]
fn a_group_is_never_an_admin_of_a_space() {
	let mut families = household_in_family("never an admin", &[MOTHER, SON, GRANDMA], EDITOR);
	let record = families.group_record();
	let promoting = families.give_role_to(Which::Space, 0, record, ADMIN);
	families.sync_all();
	assert!(!families.counts(&promoting));
	// The son is still an editor through it, and gives no roles.
	let caption = families.caption(1, "an editor's");
	let giving = families.give_role(Which::Space, 1, GRANDMA, EDITOR);
	families.sync_all();
	assert!(families.counts(&caption));
	assert!(!families.counts(&giving));
	// Nor does a group added as an admin count at all.
	let mut families = Families::new("added as an admin", &[MOTHER, SON]);
	families.invite(Which::Group, 0, 1, EDITOR);
	let adding = families.add_group(0, ADMIN);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "never counts");
	families.sync_all();
	assert!(!families.counts(&adding));
	assert!(!families.counts(&caption));
}

#[test]
fn an_agreement_counts_only_for_the_addition_and_the_first_change_it_names() {
	let mut families = Families::new("agreements", &[MOTHER, SON]);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.add_group(0, EDITOR);
	let joining = families.joining();
	let elsewhere = Joining {
		addition: families.rng.statement_id(),
		..joining
	};
	families.agree_to(0, elsewhere);
	let another_space = Joining {
		root: families.group.root.expect("founded"),
		..joining
	};
	families.agree_to(0, another_space);
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "not agreed to");
	families.sync_all();
	assert!(!families.counts(&caption));
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "agreed to");
	families.sync_all();
	assert!(families.counts(&caption));
}

#[test]
fn only_a_spaces_admin_adds_a_group_and_only_the_groups_admin_agrees() {
	let mut families = Families::new("admins of both", &[MOTHER, SON, GRANDMA]);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.invite(Which::Space, 0, 2, VIEWER);
	families.sync_all();
	// The grandmother, a viewer, adds the household: that does not count.
	let by_grandma = families.add_group(2, EDITOR);
	families.sync_all();
	assert!(!families.counts(&by_grandma));
	// The mother adds it, and the son, an editor in the household, agrees: that does not
	// count either.
	families.add_group(0, EDITOR);
	families.sync_all();
	let by_son = families.agree(1);
	families.sync_all();
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "not agreed to by an admin");
	families.sync_all();
	assert!(!families.counts(&caption));
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "agreed to by an admin");
	families.sync_all();
	assert!(families.counts(&caption));
	assert!(
		families.devices[0]
			.group
			.is_set_aside(by_son.id())
			.infallible()
	);
}

#[test]
fn a_key_listed_through_a_group_counts_once_an_anchor_names_its_listing() {
	// The son's tablet is the fourth device.
	let mut families = household_in_family("listings", &[MOTHER, SON, GRANDMA, SON], EDITOR);
	families.list(Which::Group, 1, 3);
	families.sync_all();
	let early = families.caption(3, "before an anchor names the tablet");
	families.sync_all();
	assert!(!families.counts(&early));
	// The grandmother, a member of the family alone, anchors the household's changes.
	families.anchor(2);
	families.sync_all();
	let late = families.caption(3, "once one does");
	families.sync_all();
	assert!(families.counts(&late));
	assert!(!families.counts(&early));
}

#[test]
fn a_device_that_a_group_lists_by_a_code_counts_in_the_space_by_its_own_anchor() {
	// The son lost his phone; his new tablet, the fourth device, comes back with his code.
	let mut families = household_in_family(
		"by a code through a group",
		&[MOTHER, SON, GRANDMA, SON],
		EDITOR,
	);
	let code = RecoveryCode::generate(&mut families.rng);
	let (group, son) = (families.group.id, families.group.members[SON]);
	let stated = RecoveryStatement::sign(&code, son);
	let naming = state_recovery_code(group, &stated, &mut families.rng);
	families.commit(Which::Group, 1, vec![naming]);
	families.sync_all();
	// The tablet lists itself in the household by the code, and anchors that in the family,
	// where nothing lists it yet: it counts there, with his role through the household.
	let tablet = &families.devices[3];
	let key = tablet.key.verifying_key();
	let signed = RecoveryListing::sign(
		&code,
		(group, replica_record(group, &key), son),
		&key,
		&tablet.encryption,
	);
	let mut listing = families.listing(Which::Group, 3);
	listing.push(list_by_recovery_code(&signed, &mut families.rng));
	families.commit(Which::Group, 3, listing);
	let anchor = families.anchor(3);
	let caption = families.caption(3, "from the new tablet");
	families.sync_all();
	assert!(families.counts(&anchor));
	assert!(families.counts(&caption));
}

#[test]
fn a_removal_from_a_group_takes_effect_in_a_space_at_its_first_anchors() {
	let mut families = household_in_family("first anchors", &[MOTHER, SON, GRANDMA], EDITOR);
	// The mother removes the son from the household. Not knowing, he captions twice; the
	// grandmother gets the first caption, then the removal, and anchors it.
	families.give_role(Which::Group, 0, SON, NONE);
	let first = families.caption(1, "seen by the first anchor");
	families.sync(Which::Space, 1, 2);
	let second = families.caption(1, "not seen by it");
	assert!(
		families.counts(&second),
		"a removal that no anchor names changes nothing yet"
	);
	families.sync(Which::Group, 0, 2);
	let first_anchor = families.anchor(2);
	families.sync_all();
	assert!(families.counts(&first_anchor));
	assert!(families.counts(&first), "the first anchor builds on it");
	assert!(!families.counts(&second));
	// A later anchor that builds on the second caption keeps nothing of it.
	families.anchor(0);
	families.sync_all();
	assert!(!families.counts(&second));
	// Knowing of it, what he writes counts for nothing.
	let after = families.caption(1, "after");
	families.sync_all();
	assert!(!families.counts(&after));
	assert_eq!(families.captions(0), ["seen by the first anchor"]);
}

#[test]
fn a_removed_members_own_anchor_keeps_nothing() {
	let mut families = household_in_family("own anchors", &[MOTHER, SON, GRANDMA], EDITOR);
	families.give_role(Which::Group, 0, SON, NONE);
	// The grandmother anchors the removal first.
	families.sync(Which::Group, 0, 2);
	families.anchor(2);
	// The son reads his removal in the household, captions, and anchors the removal himself.
	families.sync(Which::Group, 0, 1);
	let caption = families.caption(1, "on purpose");
	let own = families.anchor(1);
	families.sync_all();
	assert!(!families.counts(&own));
	assert!(!families.counts(&caption));
}

#[test]
fn what_a_group_sets_aside_once_the_space_knows_why_gave_nothing() {
	let mut families = Families::new("gave nothing", &[MOTHER, SON, GRANDMA, FATHER, COUSIN]);
	families.invite(Which::Group, 0, 3, ADMIN);
	families.invite(Which::Space, 0, 2, VIEWER);
	families.add_group(0, EDITOR);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	// The father adds the cousin to the household while the mother, the senior admin,
	// removes him.
	let adding_cousin = families.invite(Which::Group, 3, 4, EDITOR);
	let removing_father = families.give_role(Which::Group, 0, FATHER, NONE);
	// The grandmother learns of the cousin first, and anchors that; the cousin captions.
	families.sync(Which::Group, 3, 2);
	families.anchor(2);
	families.sync(Which::Space, 2, 4);
	let caption = families.caption(4, "the cousin's");
	families.sync(Which::Space, 4, 2);
	assert!(families.counts(&caption), "while the space knows no reason");
	// The mother's removal of the father reaches the grandmother, who anchors it after the
	// caption: the cousin's addition gave nothing, though that anchor builds on the caption.
	families.sync(Which::Group, 0, 2);
	families.anchor(2);
	families.sync_all();
	assert!(!families.counts(&caption));
	for device in &families.devices {
		assert!(device.group.is_set_aside(adding_cousin.id()).infallible());
		assert!(!device.group.is_set_aside(removing_father.id()).infallible());
	}
}

#[test]
fn a_removal_that_a_group_sets_aside_took_nothing() {
	let mut families = Families::new("took nothing", &[MOTHER, SON, GRANDMA, FATHER]);
	families.invite(Which::Group, 0, 3, ADMIN);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.invite(Which::Space, 0, 2, VIEWER);
	families.add_group(0, EDITOR);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	// The father removes the son while the mother, the senior admin, removes the father.
	families.give_role(Which::Group, 3, SON, NONE);
	families.give_role(Which::Group, 0, FATHER, NONE);
	// The grandmother anchors the son's removal first; the son captions meanwhile.
	let caption = families.caption(1, "the son's");
	families.sync(Which::Group, 3, 2);
	families.anchor(2);
	families.sync(Which::Space, 1, 2);
	families.sync(Which::Space, 2, 1);
	assert!(!families.counts(&caption));
	// The father's removal reaches the space: the son's removal took nothing.
	families.sync(Which::Group, 0, 2);
	families.anchor(2);
	families.sync_all();
	assert!(families.counts(&caption));
}

#[test]
fn withdrawing_an_agreement_takes_a_group_out_of_a_space_for_good() {
	let mut families = household_in_family("withdrawals", &[MOTHER, SON], EDITOR);
	let before = families.caption(1, "before the withdrawal");
	families.sync_all();
	assert!(families.counts(&before));
	families.withdraw(0);
	families.anchor(0);
	families.sync_all();
	let withdrawn = families.caption(1, "withdrawn");
	families.sync_all();
	assert!(!families.counts(&withdrawn));
	assert!(
		families.counts(&before),
		"the withdrawal's first anchor builds on it"
	);
	// Agreeing to the same addition again brings nothing back.
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let again = families.caption(1, "agreed again");
	families.sync_all();
	assert!(!families.counts(&again));
	// A new addition, with a new agreement, does.
	families.add_group(0, EDITOR);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let added = families.caption(1, "added anew");
	families.sync_all();
	assert!(families.counts(&added));
	assert!(families.counts(&before));
}

#[test]
fn a_key_revoked_in_a_group_is_ended_in_the_space_on_every_path() {
	// The mother's laptop, the third device, is listed in both.
	let mut families = Families::new("revoked keys", &[MOTHER, SON, MOTHER]);
	families.list(Which::Space, 0, 2);
	families.list(Which::Group, 0, 2);
	families.invite(Which::Group, 0, 1, EDITOR);
	families.add_group(0, EDITOR);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let before = families.caption(2, "before");
	families.sync_all();
	assert!(families.counts(&before));
	// Her phone revokes the laptop in the household and anchors it, while the laptop
	// captions.
	families.revoke(Which::Group, 0, 2);
	let unknowing = families.caption(2, "not knowing");
	families.anchor(0);
	families.sync_all();
	assert!(!families.counts(&unknowing));
	let knowing = families.caption(2, "knowing");
	families.sync_all();
	assert!(!families.counts(&knowing));
	assert!(families.counts(&before));
}

#[test]
fn an_ai_agents_key_counts_only_where_it_is_listed_itself() {
	// The son's AI agent is the third device.
	let mut families = household_in_family("AI agents", &[MOTHER, SON, SON], EDITOR);
	for which in [Which::Space, Which::Group] {
		let entity = families.agreements;
		let allowing = families.assert(entity, fields::AI_AGENTS, Value::Boolean(true));
		families.commit(which, 0, vec![allowing]);
	}
	families.sync_all();
	let operations = families.signed_listing(Which::Group, 2, true);
	let listed = families.commit(Which::Group, 1, operations);
	families.sync_all();
	families.anchor(0);
	families.sync_all();
	assert!(
		!families.devices[0]
			.group
			.is_set_aside(listed.id())
			.infallible()
	);
	let caption = families.caption(2, "an AI agent's");
	families.sync_all();
	assert!(!families.counts(&caption));
}

#[test]
fn a_spaces_admin_takes_a_group_out_with_the_role_none() {
	let mut families = household_in_family("taken out", &[MOTHER, SON], EDITOR);
	let before = families.caption(1, "before");
	families.sync_all();
	let record = families.group_record();
	families.give_role_to(Which::Space, 0, record, NONE);
	let unknowing = families.caption(1, "not knowing");
	families.sync_all();
	assert!(!families.counts(&unknowing));
	let knowing = families.caption(1, "knowing");
	families.sync_all();
	assert!(!families.counts(&knowing));
	assert!(
		families.counts(&before),
		"taking the group out builds on it"
	);
}

#[test]
fn the_last_direct_admin_does_not_leave_while_a_group_remains() {
	let mut families = household_in_family("the last admin", &[MOTHER, SON], EDITOR);
	let leaving = families.give_role(Which::Space, 0, MOTHER, NONE);
	families.sync_all();
	assert!(!families.counts(&leaving));
}

#[test]
fn every_device_decides_alike_whatever_order_the_logs_arrive_in() {
	// The grandmother's tablet, the fourth device, learns of everything last, the space's
	// changes before the group's.
	let mut families = household_in_family("any order", &[MOTHER, SON, GRANDMA, GRANDMA], EDITOR);
	families.list(Which::Space, 2, 3);
	families.give_role(Which::Group, 0, SON, NONE);
	let first = families.caption(1, "kept");
	families.sync(Which::Space, 1, 2);
	let second = families.caption(1, "set aside");
	families.sync(Which::Group, 0, 2);
	families.anchor(2);
	for device in 0..3 {
		for other in 0..3 {
			if device != other {
				families.sync(Which::Space, device, other);
			}
		}
	}
	// The tablet takes the space's changes with no group to follow, then the group's.
	let wall = families.tick();
	let keys = families.space.keys.clone();
	let parcels = families.devices[0].space.parcels().infallible();
	for parcel in parcels.into_iter().rev() {
		match families.devices[3].space.receive(parcel, &keys, wall) {
			Ok(_) | Err(LogError::UnknownAuthor) => {}
			Err(error) => panic!("takes the change: {error}"),
		}
	}
	families.sync_all();
	assert!(families.counts(&first));
	assert!(!families.counts(&second));
	assert_eq!(families.captions(3), families.captions(0));
}

#[test]
fn every_device_decides_alike_after_random_changes_and_syncs() {
	for seed in 0..6 {
		let mut families = Families::new(
			&format!("random {seed}"),
			&[MOTHER, SON, GRANDMA, FATHER, SON],
		);
		families.invite(Which::Group, 0, 3, ADMIN);
		families.invite(Which::Group, 0, 1, EDITOR);
		families.list(Which::Group, 1, 4);
		families.invite(Which::Space, 0, 2, VIEWER);
		families.add_group(0, EDITOR);
		families.agree(0);
		families.anchor(0);
		families.sync_all();
		let mut captions = Vec::new();
		for step in 0..24 {
			let pick = |families: &mut Families, count: usize| {
				usize::from(families.rng.space_id().as_bytes()[0]) % count
			};
			let device = pick(&mut families, 5);
			match pick(&mut families, 7) {
				0 | 1 => captions.push(families.caption(device, &format!("step {step}"))),
				6 => {
					// The mother keeps the son or the father out of the family, or lets
					// him back.
					let kept = [1, 3][pick(&mut families, 2)];
					let keeps = pick(&mut families, 2) == 0;
					families.keep_out(0, kept, keeps);
				}
				2 => {
					// An admin of the household gives the son or the father a role.
					let by = [0, 3][pick(&mut families, 2)];
					let person = [SON, FATHER][pick(&mut families, 2)];
					let role = [NONE, VIEWER, EDITOR][pick(&mut families, 3)];
					families.give_role(Which::Group, by, person, role);
				}
				3 => {
					families.anchor(device);
				}
				_ => {
					let other = (device + 1 + pick(&mut families, 4)) % 5;
					let which = [Which::Space, Which::Group][pick(&mut families, 2)];
					families.sync(which, device, other);
				}
			}
		}
		families.sync_all();
		for caption in &captions {
			// Every device that holds it decides alike.
			families.counts(caption);
		}
		let shown = families.captions(0);
		for device in 1..5 {
			assert_eq!(families.captions(device), shown, "seed {seed}");
		}
	}
}

#[test]
fn a_revocation_in_a_space_that_follows_a_group_counts() {
	// The mother's laptop, the fourth device, is listed in the family alone.
	let mut families = household_in_family(
		"revoked in the space",
		&[MOTHER, SON, GRANDMA, MOTHER],
		EDITOR,
	);
	families.list(Which::Space, 0, 3);
	families.sync_all();
	let revoking = families.revoke(Which::Space, 0, 3);
	families.sync_all();
	let laptops = families.caption(3, "the laptop's");
	let sons = families.caption(1, "the son's");
	let promoting = families.give_role(Which::Space, 0, GRANDMA, EDITOR);
	families.sync_all();
	assert!(families.counts(&revoking));
	assert!(!families.counts(&laptops));
	assert!(families.counts(&sons));
	assert!(families.counts(&promoting));
}

#[test]
fn a_record_that_names_another_first_change_brings_no_group_in() {
	let mut families = Families::new("another first change", &[MOTHER, SON]);
	families.invite(Which::Group, 0, 1, EDITOR);
	let other = families.space.root.expect("founded");
	families.add_group_rooted(0, EDITOR, other);
	families.agree(0);
	families.anchor(0);
	families.sync_all();
	let caption = families.caption(1, "through another log");
	families.sync_all();
	assert!(!families.counts(&caption));
}

#[test]
fn a_member_through_a_group_purges_only_what_they_signed() {
	let mut families = household_in_family("purges", &[MOTHER, SON], EDITOR);
	let sons = families.caption(1, "the son's");
	let mothers = families.caption(0, "the mother's");
	families.sync_all();
	let statement = |parcel: &Parcel, families: &Families| {
		let payload = families.devices[0]
			.space
			.payload(parcel.id(), &families.space.keys)
			.expect("reads")
			.expect("holds it");
		let Operation::Assert { statement, .. } = &payload.operations[0] else {
			unreachable!("an assertion");
		};
		statement.id
	};
	let (own, others) = (statement(&sons, &families), statement(&mothers, &families));
	let purging_own = families.purge(1, own);
	let purging_others = families.purge(1, others);
	families.sync_all();
	assert!(families.counts(&purging_own));
	assert!(!families.counts(&purging_others));
	assert_eq!(families.captions(0), ["the mother's"]);
}

impl Families {
	/// Device `by` makes, in the family, a member's record for device `device`'s person, a
	/// member through the household, with `role` and the device's key: the record, and the
	/// change.
	fn make_for(&mut self, by: usize, device: usize, role: &str) -> (EntityId, Parcel) {
		let record = self.rng.entity_id();
		let user = self.rng.entity_id().to_string();
		let key = self.devices[device].key.verifying_key().to_text();
		let operations = vec![
			self.assert(record, fields::USER_ID, Value::Text(user)),
			self.assert(record, fields::ROLE, Value::Text(role.to_owned())),
			self.assert(record, fields::MADE_FOR, Value::Text(key)),
		];
		let parcel = self.commit(Which::Space, by, operations);
		(record, parcel)
	}

	/// Device `device` lists itself in the family on `record`, with the listing its key signed.
	fn list_on(&mut self, device: usize, record: EntityId) -> Parcel {
		let key = (&self.devices[device].key, &self.devices[device].encryption);
		let operations = common::listing(&mut self.rng, self.space.id, key, (record, false));
		self.commit(Which::Space, device, operations)
	}

	/// Device `by` keeps device `device`'s person out of the family, or with `keeps` false
	/// lets them back.
	fn keep_out(&mut self, by: usize, device: usize, keeps: bool) -> Parcel {
		let record = self.record_in_space(device);
		let operation = self.assert(record, fields::KEPT_OUT, Value::Boolean(keeps));
		self.commit(Which::Space, by, vec![operation])
	}

	/// Whether the family ends a device's replica, as the mother's device decides it.
	fn is_removed(&self, device: usize) -> bool {
		self.devices[0]
			.space
			.is_removed(&self.devices[device].key.verifying_key(), &self.space.keys)
			.expect("reads the space")
	}
}

#[test]
fn a_member_through_a_group_made_direct_lists_their_replicas_and_takes_the_role() {
	let mut families = household_in_family("made direct", &[MOTHER, SON, GRANDMA, SON], EDITOR);
	families.list(Which::Group, 1, 3);
	families.sync_all();
	families.anchor(0);
	families.sync_all();
	// Through the household, the son is an editor: an admin's change of his does not count.
	let early = families.give_role(Which::Space, 1, GRANDMA, EDITOR);
	families.sync_all();
	assert!(!families.counts(&early));
	// The grandmother cannot make a record for him; the mother can, as an admin.
	let (_, by_grandma) = families.make_for(2, 1, ADMIN);
	let (record, making) = families.make_for(0, 1, ADMIN);
	families.sync_all();
	assert!(!families.counts(&by_grandma));
	assert!(families.counts(&making));
	// The grandmother's device cannot list itself on it; his phone, then his tablet, can.
	let grandmas = families.list_on(2, record);
	let phones = families.list_on(1, record);
	families.sync_all();
	let tablets = families.list_on(3, record);
	families.sync_all();
	assert!(!families.counts(&grandmas));
	assert!(families.counts(&phones));
	assert!(families.counts(&tablets));
	// He is an admin of the family now, directly.
	let promoting = families.give_role(Which::Space, 3, GRANDMA, EDITOR);
	families.sync_all();
	assert!(families.counts(&promoting));
}

#[test]
fn a_member_kept_out_of_a_space_stays_in_the_group_until_an_admin_lets_them_back() {
	// The son's tablet, the fourth device, is listed in the household on his record there.
	let mut families = household_in_family("kept out", &[MOTHER, SON, GRANDMA, SON], EDITOR);
	families.list(Which::Group, 1, 3);
	families.sync_all();
	families.anchor(0);
	families.sync_all();
	let before = families.caption(1, "before");
	families.sync_all();
	// The grandmother, a viewer, cannot keep him out; the mother can, naming his phone, while
	// his tablet captions without knowing.
	let by_grandma = families.keep_out(2, 1, true);
	let keeping = families.keep_out(0, 1, true);
	let unknowing = families.caption(3, "not knowing");
	families.sync_all();
	assert!(!families.counts(&by_grandma));
	assert!(families.counts(&keeping));
	assert!(families.counts(&before));
	assert!(!families.counts(&unknowing));
	let knowing = families.caption(3, "knowing");
	families.sync_all();
	assert!(!families.counts(&knowing));
	// Both his devices are ended in the family, which needs a new key; he stays in the
	// household.
	assert!(families.is_removed(1) && families.is_removed(3));
	let removals = families.devices[0]
		.space
		.removals(&families.space.keys)
		.expect("reads");
	assert!(removals.contains(&keeping.id()));
	let roles = families.devices[0]
		.group
		.member_roles(&families.group.keys)
		.expect("reads");
	assert_eq!(roles[&families.group.members[SON]].as_text(), EDITOR);
	// He cannot let himself back; the mother can.
	let by_himself = families.keep_out(1, 1, false);
	families.sync_all();
	assert!(!families.counts(&by_himself));
	families.keep_out(0, 1, false);
	families.sync_all();
	let back = families.caption(3, "back");
	families.sync_all();
	assert!(families.counts(&back));
	assert!(!families.is_removed(1));
}

#[test]
fn a_member_through_a_group_leaves_one_space_and_stays_in_the_group() {
	let mut families = household_in_family("leaving one space", &[MOTHER, SON], EDITOR);
	let leaving = families.keep_out(1, 1, true);
	families.sync_all();
	let after = families.caption(1, "after leaving");
	families.sync_all();
	assert!(families.counts(&leaving));
	assert!(!families.counts(&after));
	assert!(families.is_removed(1));
}

#[test]
fn an_admin_appointed_by_a_member_made_direct_is_junior_to_the_admins_before() {
	let mut families = household_in_family("appointed after", &[MOTHER, SON, GRANDMA], EDITOR);
	let (record, _) = families.make_for(0, 1, ADMIN);
	families.sync_all();
	families.list_on(1, record);
	families.sync_all();
	// The son, an admin now, makes the grandmother one; she and the mother then demote each
	// other, each not knowing: the mother, the senior, stays.
	families.give_role(Which::Space, 1, GRANDMA, ADMIN);
	families.sync_all();
	let by_mother = families.give_role(Which::Space, 0, GRANDMA, EDITOR);
	let by_grandma = families.give_role(Which::Space, 2, MOTHER, EDITOR);
	families.sync_all();
	assert!(families.counts(&by_mother));
	assert!(!families.counts(&by_grandma));
}

#[test]
fn keeping_oneself_out_needs_no_new_key_while_one_stays_a_member() {
	let mut families = household_in_family("no new key", &[MOTHER, SON, GRANDMA], EDITOR);
	// The grandmother, a direct viewer in no group, keeps herself out, which keeps her out of
	// nothing.
	let pointless = families.keep_out(2, 2, true);
	families.sync_all();
	assert!(families.counts(&pointless));
	assert!(!families.is_removed(2));
	let removals = families.devices[0]
		.space
		.removals(&families.space.keys)
		.expect("reads");
	assert!(!removals.contains(&pointless.id()));
}
