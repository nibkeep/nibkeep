//! Groups in spaces ([technical design: groups]): a group is a space with members and no data
//! of its own; an admin of a space who is a member of a group adds it, an admin of the group
//! agrees, and the group's members then get the space and count there, by the lower of the
//! two roles, on every device, also on a device of a member of the space alone; a removal
//! from the group takes them out of the space, which gets a new key.
//!
//! The people: Mona, an admin of the household's group and of the family's space; Sam, her
//! son, an editor in the household; Gran, a viewer of the family; and Fred, an admin of the
//! family and an editor in the household.
//!
//! [technical design: groups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups

use crate::common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{Name, TypeName, Value};
use nibkeep_node::{Library, MemberRole, NodeError};
use nibkeep_testkit::TestRng;

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// Adds an entity named `name` to `space`.
fn note(
	library: &mut Library,
	space: SpaceId,
	name: &str,
	at: u64,
	rng: &mut TestRng,
) -> Result<EntityId, NodeError> {
	library.create_entity(
		space,
		&[TypeName::from_static("std.person")],
		vec![(
			"org.nibkeep.name".parse().expect("a field"),
			Value::Name(Name::new(name)),
		)],
		at,
		rng,
	)
}

/// The names of what a search finds in a space.
fn found(library: &Library, space: SpaceId, words: &str) -> Vec<String> {
	let mut names: Vec<String> = library
		.search(space, words)
		.expect("searches")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

/// The family's space and the household's group, both made by Mona, with Sam in the
/// household as an editor and Gran in the family as a viewer, each synced with Mona.
struct Families {
	mona: Library,
	sam: Library,
	gran: Library,
	household: SpaceId,
	family: SpaceId,
	at: u64,
}

impl Families {
	fn new(rng: &mut TestRng) -> Self {
		let mut mona = user_library("Mona", "laptop", rng);
		let mut sam = user_library("Sam", "phone", rng);
		let mut gran = user_library("Gran", "tablet", rng);
		let mut at = now() + 100_000;
		let household = mona.create_group("Household", at, rng).expect("makes it");
		let family = mona
			.create_space("Family", at + 100, rng)
			.expect("makes it");
		note(&mut mona, family, "Wedding album", at + 200, rng).expect("adds");
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut sam, at);
		at += 2_000;
		invite_member(&mut mona, (family, MemberRole::Viewer), &mut gran, at);
		at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
		at = sync_rounds(&mut mona, &mut gran, at + 1_000, 1);
		Self {
			mona,
			sam,
			gran,
			household,
			family,
			at: at + 1_000,
		}
	}

	/// Mona and each of the others sync, in turn, `rounds` times.
	fn sync_all(&mut self, rounds: u64) {
		for _ in 0..rounds {
			self.at = sync_rounds(&mut self.mona, &mut self.sam, self.at, 1);
			self.at = sync_rounds(&mut self.mona, &mut self.gran, self.at, 1);
		}
	}
}

#[test]
fn a_groups_members_get_the_spaces_it_is_in_and_count_there() {
	let mut rng = TestRng::new("a group in a space");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	// Sam holds the household, not the family.
	assert!(families.sam.is_group(household).expect("reads"));
	assert!(!families.sam.spaces().contains(&family));
	// Mona, an admin of both, adds the household to the family as editors.
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the group");
	families.at += 1_000;
	families.sync_all(3);
	// Sam gets the family through the household, and adds to it, as an editor.
	assert!(families.sam.spaces().contains(&family));
	assert_eq!(
		families.sam.own_role(family).expect("reads"),
		MemberRole::Editor
	);
	assert_eq!(
		families.mona.own_role(family).expect("reads"),
		MemberRole::Admin
	);
	assert_eq!(
		families
			.sam
			.own_role(families.sam.personal_space())
			.expect("reads"),
		MemberRole::Admin
	);
	assert_eq!(found(&families.sam, family, "album"), ["Wedding album"]);
	let at = families.at;
	note(&mut families.sam, family, "School photos", at, &mut rng).expect("Sam adds");
	families.at += 1_000;
	families.sync_all(2);
	// Every device counts it: Gran's too, though she is in the family alone.
	for library in [&families.mona, &families.sam, &families.gran] {
		assert_eq!(found(library, family, "photos"), ["School photos"]);
	}
	let groups = families
		.gran
		.space_groups(family)
		.expect("lists the groups");
	assert_eq!(groups.len(), 1);
	assert_eq!(groups[0].group, household);
	assert_eq!(groups[0].role, MemberRole::Editor);
	assert!(groups[0].agreed);
	let mut members: Vec<(Option<String>, MemberRole)> = groups[0]
		.members
		.iter()
		.map(|member| (member.name.clone(), member.role))
		.collect();
	members.sort();
	assert_eq!(
		members,
		[
			(Some("Mona".to_owned()), MemberRole::Admin),
			(Some("Sam".to_owned()), MemberRole::Editor)
		]
	);
	// The group itself is no member of the family's own.
	let names: Vec<String> = families
		.mona
		.members(family)
		.expect("lists")
		.into_iter()
		.filter_map(|member| member.name)
		.collect();
	assert!(!names.iter().any(|name| name == "Household"));
}

#[test]
fn through_a_viewers_group_a_member_only_views() {
	let mut rng = TestRng::new("a viewers' group");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Viewer, at, &mut rng)
		.expect("adds the group");
	families.at += 1_000;
	families.sync_all(3);
	assert_eq!(found(&families.sam, family, "album"), ["Wedding album"]);
	assert_eq!(
		families.sam.own_role(family).expect("reads"),
		MemberRole::Viewer
	);
	let at = families.at;
	note(&mut families.sam, family, "School photos", at, &mut rng).expect("writes locally");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.mona, family, "photos"),
		Vec::<String>::new()
	);
	assert_eq!(
		found(&families.gran, family, "photos"),
		Vec::<String>::new()
	);
}

#[test]
fn a_group_joins_a_space_only_once_an_admin_of_the_group_agrees() {
	let mut rng = TestRng::new("an agreement");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	// Fred, an admin of the family and an editor in the household, adds the household.
	let mut fred = user_library("Fred", "phone", &mut rng);
	let at = families.at;
	invite_member(
		&mut families.mona,
		(family, MemberRole::Admin),
		&mut fred,
		at,
	);
	invite_member(
		&mut families.mona,
		(household, MemberRole::Editor),
		&mut fred,
		at + 2_000,
	);
	let mut at = sync_rounds(&mut families.mona, &mut fred, at + 4_000, 1);
	fred.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the group");
	at = sync_rounds(&mut fred, &mut families.mona, at + 1_000, 1);
	families.at = at + 1_000;
	families.sync_all(2);
	// Nothing reaches Sam until Mona, the household's admin, agrees.
	assert!(!families.sam.spaces().contains(&family));
	let requests = families
		.mona
		.join_requests(household)
		.expect("lists the requests");
	assert_eq!(requests.len(), 1);
	assert_eq!(requests[0].space, family);
	assert_eq!(requests[0].name.as_deref(), Some("Family"));
	assert!(!requests[0].agreed);
	// Sam, an editor in the household, cannot agree; Mona can.
	let at = families.at;
	assert!(matches!(
		families.sam.agree_to_join(household, family, at, &mut rng),
		Err(NodeError::AdminsOnly(_))
	));
	families
		.mona
		.agree_to_join(household, family, at, &mut rng)
		.expect("agrees");
	families.at += 1_000;
	families.sync_all(3);
	assert!(families.sam.spaces().contains(&family));
	assert_eq!(found(&families.sam, family, "album"), ["Wedding album"]);
}

#[test]
fn removing_someone_from_a_group_takes_them_out_of_its_spaces_with_a_new_key() {
	let mut rng = TestRng::new("a removal from a group");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the group");
	families.at += 1_000;
	families.sync_all(3);
	let key_before = families.mona.space_key_numbers(family).expect("reads").1;
	// Mona removes Sam from the household: the family gets a new key, which Sam never gets,
	// and what Sam writes after counts for nothing.
	let sam = families
		.mona
		.find_member(household, "Sam")
		.expect("a member");
	let at = families.at;
	families
		.mona
		.remove_member(household, sam, at, &mut rng)
		.expect("removes");
	families.at += 1_000;
	families.sync_all(2);
	let key_after = families.mona.space_key_numbers(family).expect("reads").1;
	assert!(key_after > key_before, "a new key after the departure");
	let (held, _) = families.sam.space_key_numbers(family).expect("reads");
	assert!(!held.contains(&key_after));
	let at = families.at;
	let _ = note(&mut families.sam, family, "After the removal", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.mona, family, "removal"),
		Vec::<String>::new()
	);
	assert_eq!(
		found(&families.gran, family, "removal"),
		Vec::<String>::new()
	);
	// Notes added after reach Gran, not Sam.
	let at = families.at;
	note(
		&mut families.mona,
		family,
		"Christmas card list",
		at,
		&mut rng,
	)
	.expect("adds");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "christmas"),
		["Christmas card list"]
	);
	assert_eq!(
		found(&families.sam, family, "christmas"),
		Vec::<String>::new()
	);
}

#[test]
fn withdrawing_a_group_takes_its_members_out_of_the_space() {
	let mut rng = TestRng::new("a withdrawal");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the group");
	families.at += 1_000;
	families.sync_all(3);
	let at = families.at;
	families
		.mona
		.withdraw_group(household, family, at, &mut rng)
		.expect("withdraws");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	let _ = note(
		&mut families.sam,
		family,
		"After the withdrawal",
		at,
		&mut rng,
	);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.mona, family, "withdrawal"),
		Vec::<String>::new()
	);
	let groups = families.mona.space_groups(family).expect("lists");
	assert!(!groups[0].agreed);
}

#[test]
fn a_group_holds_no_entities_and_is_never_an_admin() {
	let mut rng = TestRng::new("no entities");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	assert!(matches!(
		note(&mut families.mona, household, "A note", at, &mut rng),
		Err(NodeError::GroupHoldsNoData)
	));
	assert!(matches!(
		families
			.mona
			.add_group(family, household, MemberRole::Admin, at, &mut rng),
		Err(NodeError::GroupAsAdmin)
	));
	// Gran, a viewer of the family and no member of the household, adds nothing.
	assert!(
		families
			.gran
			.add_group(family, household, MemberRole::Editor, at, &mut rng)
			.is_err()
	);
}

#[test]
fn an_entity_typed_as_a_group_makes_no_space_a_group() {
	let mut rng = TestRng::new("no group by type");
	let mut families = Families::new(&mut rng);
	let family = families.family;
	let at = families.at;
	families
		.mona
		.create_entity(
			family,
			&[TypeName::from_static("org.nibkeep.group")],
			vec![(
				"org.nibkeep.name".parse().expect("a field"),
				Value::Name(Name::new("Not a group")),
			)],
			at,
			&mut rng,
		)
		.expect("adds");
	assert!(!families.mona.is_group(family).expect("reads"));
	note(
		&mut families.mona,
		family,
		"Still a space",
		at + 100,
		&mut rng,
	)
	.expect("a space holds entities");
}

#[test]
fn a_groups_record_is_never_made_an_admin_and_an_admin_takes_the_group_out() {
	let mut rng = TestRng::new("a group's record");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	let record = families
		.mona
		.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the group");
	assert!(matches!(
		families
			.mona
			.give_role(family, record, MemberRole::Admin, at + 100, &mut rng),
		Err(NodeError::GroupAsAdmin)
	));
	families.at += 1_000;
	families.sync_all(3);
	assert!(families.sam.spaces().contains(&family));
	let at = families.at;
	families
		.mona
		.remove_group(family, household, at, &mut rng)
		.expect("takes it out");
	families.at += 1_000;
	families.sync_all(2);
	let groups = families.mona.space_groups(family).expect("lists");
	assert_eq!(groups[0].role, MemberRole::None);
	let at = families.at;
	let _ = note(
		&mut families.sam,
		family,
		"After it was taken out",
		at,
		&mut rng,
	);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(found(&families.mona, family, "taken"), Vec::<String>::new());
}

/// The family with the household in it as editors, synced.
fn household_in_family(rng: &mut TestRng) -> Families {
	let mut families = Families::new(rng);
	let (household, family) = (families.household, families.family);
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Editor, at, rng)
		.expect("adds the group");
	families.at += 1_000;
	families.sync_all(3);
	families
}

/// The role that a space's members, as a library lists them, give the member named `name`.
fn role_of(library: &Library, space: SpaceId, name: &str) -> Option<MemberRole> {
	library
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|member| member.name.as_deref() == Some(name))
		.map(|member| member.role)
}

#[test]
fn a_member_through_a_group_made_direct_confirms_and_takes_the_role() {
	let mut rng = TestRng::new("made direct");
	let mut families = household_in_family(&mut rng);
	let family = families.family;
	// Through the household, Sam is an editor: he cannot give roles.
	assert!(matches!(
		families.sam.check_admin(family, "give roles"),
		Err(NodeError::AdminsOnly(_))
	));
	assert!(matches!(
		families.sam.confirm_direct(family, families.at, &mut rng),
		Err(NodeError::NoDirectRecord)
	));
	let sam = families
		.mona
		.find_group_member(family, "sam")
		.expect("finds Sam through the household");
	let at = families.at;
	families
		.mona
		.make_direct(family, sam, MemberRole::Admin, at, &mut rng)
		.expect("makes a record for Sam");
	families.at += 1_000;
	families.sync_all(1);
	let at = families.at;
	families
		.sam
		.confirm_direct(family, at, &mut rng)
		.expect("Sam confirms");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		role_of(&families.mona, family, "Sam"),
		Some(MemberRole::Admin)
	);
	families
		.sam
		.check_admin(family, "give roles")
		.expect("Sam is an admin");
	let gran = families.sam.find_member(family, "Gran").expect("a member");
	let at = families.at;
	families
		.sam
		.give_role(family, gran, MemberRole::Editor, at, &mut rng)
		.expect("Sam gives Gran a role");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		role_of(&families.mona, family, "Gran"),
		Some(MemberRole::Editor)
	);
}

#[test]
fn a_member_kept_out_stays_in_the_group_until_an_admin_lets_them_back() {
	let mut rng = TestRng::new("kept out");
	let mut families = household_in_family(&mut rng);
	let (household, family) = (families.household, families.family);
	let key_before = families.mona.space_key_numbers(family).expect("reads").1;
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam");
	let at = families.at;
	assert!(matches!(
		families.gran.keep_out(family, sam, at, &mut rng),
		Err(NodeError::AdminsOnly(_))
	));
	families
		.mona
		.keep_out(family, sam, at, &mut rng)
		.expect("keeps Sam out");
	families.at += 1_000;
	families.sync_all(2);
	let key_after = families.mona.space_key_numbers(family).expect("reads").1;
	assert!(key_after > key_before, "a new key after keeping him out");
	let (held, _) = families.sam.space_key_numbers(family).expect("reads");
	assert!(!held.contains(&key_after));
	let at = families.at;
	let _ = note(&mut families.sam, family, "Kept out", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(found(&families.gran, family, "kept"), Vec::<String>::new());
	// He stays in the household.
	assert_eq!(
		role_of(&families.mona, household, "Sam"),
		Some(MemberRole::Editor)
	);
	// Let back in, he gets the new key, and writes again.
	let at = families.at;
	families
		.mona
		.let_in(family, sam, at, &mut rng)
		.expect("lets Sam back in");
	families.at += 1_000;
	families.sync_all(3);
	let (held, _) = families.sam.space_key_numbers(family).expect("reads");
	assert!(held.contains(&key_after));
	let at = families.at;
	note(&mut families.sam, family, "Back in", at, &mut rng).expect("Sam adds");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(found(&families.gran, family, "back"), ["Back in"]);
}

#[test]
fn a_member_through_a_group_leaves_one_space_and_stays_in_the_group() {
	let mut rng = TestRng::new("leaving one space");
	let mut families = household_in_family(&mut rng);
	let (household, family) = (families.household, families.family);
	assert!(!families.sam.check_leave(family).expect("may leave"));
	let at = families.at;
	families
		.sam
		.leave(family, at, &mut rng)
		.expect("Sam leaves");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	let _ = note(&mut families.sam, family, "After leaving", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "leaving"),
		Vec::<String>::new()
	);
	assert_eq!(
		role_of(&families.mona, household, "Sam"),
		Some(MemberRole::Editor)
	);
}

#[test]
fn removing_a_direct_member_who_is_in_through_a_group_keeps_them_out() {
	let mut rng = TestRng::new("removed and kept out");
	let mut families = household_in_family(&mut rng);
	let family = families.family;
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam");
	let at = families.at;
	let record = families
		.mona
		.make_direct(family, sam, MemberRole::Editor, at, &mut rng)
		.expect("makes a record for Sam");
	families.at += 1_000;
	families.sync_all(1);
	let at = families.at;
	families
		.sam
		.confirm_direct(family, at, &mut rng)
		.expect("Sam confirms");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	families
		.mona
		.remove_member(family, record, at, &mut rng)
		.expect("removes Sam");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	let _ = note(&mut families.sam, family, "Removed", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "removed"),
		Vec::<String>::new()
	);
}

#[test]
fn a_shared_group_with_one_admin_suggests_a_second() {
	let mut rng = TestRng::new("a second admin");
	let mut families = Families::new(&mut rng);
	let household = families.household;
	assert!(
		families
			.mona
			.suggests_second_admin(household)
			.expect("reads")
	);
	let sam = families
		.mona
		.find_member(household, "Sam")
		.expect("a member");
	let at = families.at;
	families
		.mona
		.give_role(household, sam, MemberRole::Admin, at, &mut rng)
		.expect("makes Sam an admin");
	assert!(
		!families
			.mona
			.suggests_second_admin(household)
			.expect("reads")
	);
}

/// Mona makes Sam, in the family through the household, a direct member with `role`, and Sam
/// confirms; returns his record.
fn make_sam_direct(families: &mut Families, role: MemberRole, rng: &mut TestRng) -> EntityId {
	let family = families.family;
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam");
	let at = families.at;
	let record = families
		.mona
		.make_direct(family, sam, role, at, rng)
		.expect("makes a record for Sam");
	families.at += 1_000;
	families.sync_all(1);
	let at = families.at;
	families
		.sam
		.confirm_direct(family, at, rng)
		.expect("Sam confirms");
	families.at += 1_000;
	families.sync_all(2);
	record
}

#[test]
fn a_direct_member_in_through_a_group_too_leaves_the_space_by_both() {
	let mut rng = TestRng::new("leaving by both");
	let mut families = household_in_family(&mut rng);
	let (household, family) = (families.household, families.family);
	make_sam_direct(&mut families, MemberRole::Editor, &mut rng);
	let at = families.at;
	families
		.sam
		.leave(family, at, &mut rng)
		.expect("Sam leaves");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	let _ = note(&mut families.sam, family, "After leaving", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "leaving"),
		Vec::<String>::new()
	);
	assert_eq!(
		role_of(&families.mona, household, "Sam"),
		Some(MemberRole::Editor)
	);
}

#[test]
fn a_removed_direct_member_let_back_in_is_in_through_the_group_again() {
	let mut rng = TestRng::new("removed, let back in");
	let mut families = household_in_family(&mut rng);
	let family = families.family;
	let record = make_sam_direct(&mut families, MemberRole::Editor, &mut rng);
	let at = families.at;
	families
		.mona
		.remove_member(family, record, at, &mut rng)
		.expect("removes Sam");
	families.at += 1_000;
	families.sync_all(2);
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam in the household");
	let at = families.at;
	families
		.mona
		.let_in(family, sam, at, &mut rng)
		.expect("lets Sam back in");
	families.at += 1_000;
	families.sync_all(3);
	let at = families.at;
	note(
		&mut families.sam,
		family,
		"Back through the household",
		at,
		&mut rng,
	)
	.expect("Sam adds");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "household"),
		["Back through the household"]
	);
}

#[test]
fn removing_a_record_made_for_a_member_before_they_confirm_keeps_them_out() {
	let mut rng = TestRng::new("removed before confirming");
	let mut families = household_in_family(&mut rng);
	let family = families.family;
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam");
	let at = families.at;
	let record = families
		.mona
		.make_direct(family, sam, MemberRole::Admin, at, &mut rng)
		.expect("makes a record for Sam");
	families
		.mona
		.remove_member(family, record, at + 100, &mut rng)
		.expect("removes it");
	families.at += 1_000;
	families.sync_all(2);
	let at = families.at;
	let _ = note(&mut families.sam, family, "Still here?", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(found(&families.gran, family, "still"), Vec::<String>::new());
}

/// How many changes each of the family's libraries keeps.
fn kept(families: &Families) -> [usize; 3] {
	[&families.mona, &families.sam, &families.gran]
		.map(|library| library.changes_kept().expect("counts"))
}

#[test]
fn syncing_with_a_group_in_a_space_settles() {
	let mut rng = TestRng::new("a group in a space settles");
	let mut families = household_in_family(&mut rng);
	let (household, family) = (families.household, families.family);
	families.sync_all(2);
	// Sam, an editor in the household, holds the family, and gives it to nobody: an entry
	// counts only from an admin of the group.
	assert!(families.sam.spaces().contains(&family));
	assert_eq!(
		families.sam.own_role(household).expect("reads"),
		MemberRole::Editor
	);
	let before = kept(&families);
	families.sync_all(3);
	families.at = sync_rounds(&mut families.sam, &mut families.gran, families.at, 2);
	assert_eq!(
		kept(&families),
		before,
		"a sync with nothing new adds no change"
	);
}
