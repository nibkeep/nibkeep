//! Groups inside groups ([technical design: groups]): a group is a member of a space or of
//! another group, and its members get every space that the groups it is in are in, by the
//! lowest role on the way, on every device; a removal at any level takes them out, with a new
//! key; and no group holds a group that holds it.
//!
//! The people: Mona, an admin of the family's space, the household's group and the kids'
//! group; Sam, her son, an editor of the kids alone; and Gran, a viewer of the family alone.
//!
//! [technical design: groups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups

mod common;

use common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{Name, TypeName, Value};
use nibkeep_node::{Library, MemberRole, NodeError};
use nibkeep_testkit::TestRng;

/// Syncs `a` and `b` back and forth once, at `at`; returns the time after.
fn sync_once(a: &mut Library, b: &mut Library, at: u64) -> u64 {
	let at = at + 1_000;
	let (first, second) = sync_in_memory(a, b, at, (0, 1));
	first.unwrap_or_else(|error| panic!("the first side syncs: {error}"));
	second.unwrap_or_else(|error| panic!("the second side syncs: {error}"));
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

/// Who history says wrote the name of an entity last.
fn named_by(library: &Library, (space, entity): (SpaceId, EntityId)) -> Option<String> {
	let field: nibkeep_model::FieldName = "org.nibkeep.name".parse().expect("a field");
	let history = library
		.history(space, entity, Some(&field))
		.expect("reads")
		.expect("an entity");
	history.fields[0]
		.values
		.iter()
		.max_by_key(|value| value.recorded)
		.and_then(|value| value.author.as_ref())
		.and_then(|author| author.user.name.clone())
}

/// The family's space, the household's group and the kids' group, all made by Mona, with
/// Sam in the kids as an editor and Gran in the family as a viewer, each synced with Mona.
struct Families {
	mona: Library,
	sam: Library,
	gran: Library,
	family: SpaceId,
	household: SpaceId,
	kids: SpaceId,
	at: u64,
}

impl Families {
	fn new(rng: &mut TestRng) -> Self {
		let mut mona = user_library("Mona", "laptop", rng);
		let mut sam = user_library("Sam", "phone", rng);
		let mut gran = user_library("Gran", "tablet", rng);
		let mut at = now() + 100_000;
		let household = mona.create_group("Household", at, rng).expect("makes it");
		let kids = mona.create_group("Kids", at + 100, rng).expect("makes it");
		let family = mona
			.create_space("Family", at + 200, rng)
			.expect("makes it");
		note(&mut mona, family, "Wedding album", at + 300, rng).expect("adds");
		at += 2_000;
		invite_member(&mut mona, (kids, MemberRole::Editor), &mut sam, at);
		at += 2_000;
		invite_member(&mut mona, (family, MemberRole::Viewer), &mut gran, at);
		at = sync_once(&mut mona, &mut sam, at + 1_000);
		at = sync_once(&mut mona, &mut gran, at + 1_000);
		Self {
			mona,
			sam,
			gran,
			family,
			household,
			kids,
			at: at + 1_000,
		}
	}

	/// Mona and each of the others sync, in turn, `rounds` times.
	fn sync_all(&mut self, rounds: u64) {
		for _ in 0..rounds {
			self.at = sync_once(&mut self.mona, &mut self.sam, self.at);
			self.at = sync_once(&mut self.mona, &mut self.gran, self.at);
		}
	}

	/// Mona adds the household to the family and the kids to the household, each with
	/// `role`, as an admin of all three, which agrees at once; then everyone syncs.
	fn nest(&mut self, household_role: MemberRole, kids_role: MemberRole, rng: &mut TestRng) {
		let (family, household, kids) = (self.family, self.household, self.kids);
		let at = self.at;
		self.mona
			.add_group(family, household, household_role, at, rng)
			.expect("adds the household to the family");
		self.mona
			.add_group(household, kids, kids_role, at + 100, rng)
			.expect("adds the kids to the household");
		self.at += 1_000;
		self.sync_all(5);
	}
}

#[test]
fn a_group_inside_a_group_gives_its_members_every_space_the_outer_group_is_in() {
	let mut rng = TestRng::new("a group inside a group");
	let mut families = Families::new(&mut rng);
	let (family, household, kids) = (families.family, families.household, families.kids);
	assert!(!families.sam.spaces().contains(&family));
	families.nest(MemberRole::Editor, MemberRole::Editor, &mut rng);
	// Sam gets the household through the kids, and the family through the household.
	assert!(families.sam.spaces().contains(&household));
	assert!(families.sam.spaces().contains(&family));
	assert_eq!(found(&families.sam, family, "album"), ["Wedding album"]);
	let at = families.at;
	let photos = note(&mut families.sam, family, "School photos", at, &mut rng).expect("Sam adds");
	families.at += 1_000;
	families.sync_all(2);
	// Every device counts it: Gran's too, though she is in the family alone; and its history
	// names Sam, as the kids name him.
	for library in [&families.mona, &families.sam, &families.gran] {
		assert_eq!(found(library, family, "photos"), ["School photos"]);
		assert_eq!(named_by(library, (family, photos)).as_deref(), Some("Sam"));
	}
	// The family shows the household, and the kids inside it, with their members.
	let groups = families
		.gran
		.space_groups(family)
		.expect("lists the groups");
	assert_eq!(groups.len(), 1);
	assert_eq!(groups[0].group, household);
	assert_eq!(groups[0].groups.len(), 1);
	let inner = &groups[0].groups[0];
	assert_eq!(inner.group, kids);
	assert_eq!(inner.name.as_deref(), Some("Kids"));
	assert_eq!(inner.role, MemberRole::Editor);
	assert!(inner.agreed);
	let mut members: Vec<(Option<String>, MemberRole)> = inner
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
}

#[test]
fn through_a_viewers_group_inside_a_group_a_member_only_views() {
	let mut rng = TestRng::new("a viewers' group inside a group");
	let mut families = Families::new(&mut rng);
	let family = families.family;
	families.nest(MemberRole::Editor, MemberRole::Viewer, &mut rng);
	assert_eq!(found(&families.sam, family, "album"), ["Wedding album"]);
	let at = families.at;
	note(&mut families.sam, family, "School photos", at, &mut rng).expect("writes locally");
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(
		found(&families.gran, family, "photos"),
		Vec::<String>::new()
	);
}

#[test]
fn removing_someone_from_a_group_inside_a_group_takes_them_out_of_every_space_with_a_new_key() {
	let mut rng = TestRng::new("a removal from a group inside a group");
	let mut families = Families::new(&mut rng);
	let (family, kids) = (families.family, families.kids);
	families.nest(MemberRole::Editor, MemberRole::Editor, &mut rng);
	let key_before = families.mona.space_key_numbers(family).expect("reads").1;
	let sam = families.mona.find_member(kids, "Sam").expect("a member");
	let at = families.at;
	families
		.mona
		.remove_member(kids, sam, at, &mut rng)
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
	for library in [&families.mona, &families.gran] {
		assert_eq!(found(library, family, "removal"), Vec::<String>::new());
	}
}

#[test]
fn taking_a_group_out_of_a_group_takes_its_members_out_of_every_space_with_a_new_key() {
	let mut rng = TestRng::new("a group taken out of a group");
	let mut families = Families::new(&mut rng);
	let (family, household, kids) = (families.family, families.household, families.kids);
	families.nest(MemberRole::Editor, MemberRole::Editor, &mut rng);
	let key_before = families.mona.space_key_numbers(family).expect("reads").1;
	let at = families.at;
	families
		.mona
		.remove_group(household, kids, at, &mut rng)
		.expect("takes the kids out");
	families.at += 1_000;
	families.sync_all(2);
	let key_after = families.mona.space_key_numbers(family).expect("reads").1;
	assert!(key_after > key_before, "a new key after the departure");
	let (held, _) = families.sam.space_key_numbers(family).expect("reads");
	assert!(!held.contains(&key_after));
	let at = families.at;
	let _ = note(&mut families.sam, family, "Taken out", at, &mut rng);
	families.at += 1_000;
	families.sync_all(2);
	assert_eq!(found(&families.gran, family, "taken"), Vec::<String>::new());
}

#[test]
fn no_group_holds_a_group_that_holds_it() {
	let mut rng = TestRng::new("no cycles");
	let mut families = Families::new(&mut rng);
	let (household, kids) = (families.household, families.kids);
	let at = families.at;
	families
		.mona
		.add_group(household, kids, MemberRole::Editor, at, &mut rng)
		.expect("adds the kids to the household");
	assert!(matches!(
		families
			.mona
			.add_group(kids, household, MemberRole::Editor, at + 100, &mut rng),
		Err(NodeError::GroupCycle)
	));
	assert!(matches!(
		families
			.mona
			.add_group(kids, kids, MemberRole::Editor, at + 200, &mut rng),
		Err(NodeError::GroupCycle)
	));
}

#[test]
fn a_group_added_again_shows_its_name_and_members_on_each_record() {
	let mut rng = TestRng::new("a group added again");
	let mut families = Families::new(&mut rng);
	let (family, household) = (families.family, families.household);
	let at = families.at;
	families
		.mona
		.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the household");
	families
		.mona
		.remove_group(family, household, at + 100, &mut rng)
		.expect("takes it out");
	families
		.mona
		.add_group(family, household, MemberRole::Viewer, at + 200, &mut rng)
		.expect("adds it again");
	let groups = families
		.mona
		.space_groups(family)
		.expect("lists the groups");
	assert_eq!(groups.len(), 2);
	for group in &groups {
		assert_eq!(group.name.as_deref(), Some("Household"));
		assert_eq!(group.members.len(), 1, "Mona alone");
	}
}
