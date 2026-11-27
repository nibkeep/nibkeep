//! Purging what a move leaves behind ([technical design: spaces and moves]): once the new entity counts and
//! another device holds it, the mover purges what their role lets them in the old space, and
//! a mark asks the old space's admins for the rest; a move that keeps what it leaves behind
//! purges nothing.
//!
//! [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves

use crate::common;

use crate::common::{invite_member, now, pair_named, sync_in_memory, user_library};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{FieldName, Name, TypeName, Value, fields};
use nibkeep_node::{Behind, Edit, Library, MemberRole, MoveHistory, PairingCode};
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

fn name_field() -> FieldName {
	"org.nibkeep.name".parse().expect("a field")
}

/// Adds a person named `name` to `space`.
fn note(library: &mut Library, space: SpaceId, name: &str, at: u64, rng: &mut TestRng) -> EntityId {
	library
		.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(name_field(), Value::Name(Name::new(name)))],
			at,
			rng,
		)
		.expect("adds")
}

/// Renames an entity.
fn rename(
	library: &mut Library,
	(space, entity): (SpaceId, EntityId),
	name: &str,
	at: u64,
	rng: &mut TestRng,
) {
	library
		.edit_entity(
			space,
			entity,
			vec![Edit::Set(name_field(), vec![Value::Name(Name::new(name))])],
			&common::today(),
			at,
			rng,
		)
		.expect("saves the change");
}

/// The names an entity has had in a space, current or past, as its history shows them.
fn names(library: &Library, (space, entity): (SpaceId, EntityId)) -> Vec<String> {
	let Some(history) = library
		.history(space, entity, Some(&name_field()))
		.expect("reads")
	else {
		return Vec::new();
	};
	let mut names: Vec<String> = history
		.fields
		.iter()
		.flat_map(|field| &field.values)
		.filter_map(|value| match &value.value {
			Value::Name(name) => Some(name.full().to_owned()),
			_ => None,
		})
		.collect();
	names.sort();
	names
}

/// Whether an entity's record in a space still holds its marks of a move.
fn marked(library: &Library, (space, entity): (SpaceId, EntityId)) -> bool {
	library
		.history(space, entity, Some(&fields::MOVED_INTO))
		.expect("reads")
		.is_some_and(|history| !history.fields.is_empty())
}

#[test]
fn a_move_purges_what_it_leaves_once_another_device_holds_the_new_entity() {
	let mut rng = TestRng::new("purged behind");
	let mut laptop = user_library("Ada", "laptop", &mut rng);
	let mut desk = {
		let code = PairingCode::generate(&mut rng);
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&common::unique_folder("purged behind, desk"),
		);
		std::fs::remove_dir(&directory).expect("removes");
		let (paired, joined) =
			pair_named(&mut laptop, &directory, "desk", (&code, &code), &mut rng);
		paired.expect("pairs");
		joined.expect("joins").library
	};
	let personal = laptop.personal_space();
	let mut at = sync_rounds(&mut laptop, &mut desk, now() + 70_000, 1);
	let household = laptop
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let boiler = note(&mut laptop, personal, "Boiler service", at + 100, &mut rng);
	rename(
		&mut laptop,
		(personal, boiler),
		"Boiler and radiators",
		at + 200,
		&mut rng,
	);
	// The desk holds it too, as it was before the move.
	at = sync_rounds(&mut laptop, &mut desk, at + 200, 1);

	let moved = laptop
		.move_entity(
			personal,
			boiler,
			household,
			MoveHistory::Bring,
			Behind::Purge,
			nibkeep_node::MoveSecrets::Bring,
			at + 300,
			&mut rng,
		)
		.expect("moves");
	// Until another device holds the new entity, the old one keeps what it had.
	assert_eq!(
		names(&laptop, (personal, boiler)),
		["Boiler and radiators", "Boiler service"]
	);
	// Meanwhile the desk, which has not heard of the move, merges a duplicate into the old
	// entity: what the move did not take is never purged.
	let duplicate = note(&mut desk, personal, "Boiler check", at + 400, &mut rng);
	desk.merge_entities(personal, duplicate, boiler, at + 500, &mut rng)
		.expect("merges");
	// A sync that brings the desk the personal space alone purges nothing yet.
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 1);
	assert_eq!(names(&laptop, (personal, boiler)).len(), 3);
	// Once the desk holds the new space, and the new entity in it, the laptop purges.
	at = sync_rounds(&mut laptop, &mut desk, at, 4);
	assert_eq!(names(&desk, (household, moved)).len(), 2);
	assert_eq!(names(&laptop, (personal, boiler)), ["Boiler check"]);
	assert!(marked(&laptop, (personal, boiler)), "the mark stays");
	// The purge reaches the desk, and a lookup still leads to the new entity.
	sync_rounds(&mut laptop, &mut desk, at, 1);
	assert_eq!(names(&desk, (personal, boiler)), ["Boiler check"]);
	assert_eq!(
		desk.locate(&boiler.to_string()).expect("finds"),
		(household, moved)
	);
	assert_eq!(names(&laptop, (household, moved)).len(), 2);
}

#[test]
fn a_move_that_keeps_what_it_leaves_purges_nothing() {
	let mut rng = TestRng::new("kept behind");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	invite_member(
		&mut ada,
		(household, MemberRole::Editor),
		&mut bea,
		at + 100,
	);
	let personal = ada.personal_space();
	let boiler = note(&mut ada, personal, "Boiler service", at + 200, &mut rng);
	ada.move_entity(
		personal,
		boiler,
		household,
		MoveHistory::Bring,
		Behind::Keep,
		nibkeep_node::MoveSecrets::Bring,
		at + 300,
		&mut rng,
	)
	.expect("moves");
	sync_rounds(&mut ada, &mut bea, at + 1_000, 2);
	assert_eq!(names(&ada, (personal, boiler)), ["Boiler service"]);
	assert_eq!(ada.purge_requests(personal).expect("lists"), Vec::new());
}

#[test]
fn an_editor_purges_only_their_own_changes_and_an_admin_is_asked_for_the_rest() {
	let mut rng = TestRng::new("purged by role");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let mut at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let family = ada
		.create_space("Family", at + 10, &mut rng)
		.expect("makes it");
	invite_member(
		&mut ada,
		(household, MemberRole::Editor),
		&mut bea,
		at + 100,
	);
	invite_member(&mut ada, (family, MemberRole::Editor), &mut bea, at + 200);
	let boiler = note(&mut ada, household, "Boiler service", at + 300, &mut rng);
	at = sync_rounds(&mut ada, &mut bea, at + 1_000, 1);
	rename(
		&mut bea,
		(household, boiler),
		"Boiler and radiators",
		at,
		&mut rng,
	);

	// Bea moves it, and is told that the members who stop seeing it are none, and that she
	// purges her own changes alone.
	let preview = bea
		.move_preview(household, boiler, family)
		.expect("previews");
	assert_eq!(preview.losing, Vec::<String>::new());
	assert!(!preview.purges_all);
	let moved = bea
		.move_entity(
			household,
			boiler,
			family,
			MoveHistory::Bring,
			Behind::Purge,
			nibkeep_node::MoveSecrets::Bring,
			at + 100,
			&mut rng,
		)
		.expect("moves");
	at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	// Bea's renaming is purged, on Ada's laptop too after the next sync; Ada's name stays,
	// for an admin to purge.
	assert_eq!(names(&bea, (household, boiler)), ["Boiler service"]);
	at = sync_rounds(&mut bea, &mut ada, at, 1);
	assert_eq!(names(&ada, (household, boiler)), ["Boiler service"]);
	let asked = ada.purge_requests(household).expect("lists");
	assert_eq!(asked.len(), 1, "{asked:?}");
	assert_eq!(asked[0].entity, boiler);
	assert_eq!(asked[0].moved_into, (family, moved));
	assert_eq!(
		bea.purge_requests(household).expect("lists"),
		Vec::new(),
		"for admins only"
	);
	ada.purge_behind(household, boiler, at + 2_000, &mut rng)
		.expect("purges");
	assert_eq!(names(&ada, (household, boiler)), Vec::<String>::new());
	assert!(marked(&ada, (household, boiler)), "the mark stays");
	assert_eq!(ada.purge_requests(household).expect("lists"), Vec::new());
	assert_eq!(names(&ada, (family, moved)).len(), 2);
}

#[test]
fn a_move_names_the_members_who_will_no_longer_see_it() {
	let mut rng = TestRng::new("who loses it");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let mine = ada
		.create_space("Mine", at + 10, &mut rng)
		.expect("makes it");
	invite_member(
		&mut ada,
		(household, MemberRole::Viewer),
		&mut bea,
		at + 100,
	);
	let boiler = note(&mut ada, household, "Boiler service", at + 200, &mut rng);
	let preview = ada.move_preview(household, boiler, mine).expect("previews");
	assert_eq!(preview.losing, ["Bea"]);
	assert_eq!(preview.gaining, Vec::<String>::new());
	assert!(preview.purges_all);
	// Out of the personal space, nobody else saw it.
	let personal = ada.personal_space();
	let note = note(&mut ada, personal, "Diary", at + 300, &mut rng);
	let preview = ada
		.move_preview(personal, note, household)
		.expect("previews");
	assert_eq!(preview.losing, Vec::<String>::new());
	// Bea sees it for the first time, with its history.
	assert_eq!(preview.gaining, ["Bea"]);
	assert!(preview.purges_all);
}

#[test]
fn what_moved_must_have_the_same_values_before_an_admin_purges_the_rest() {
	let mut rng = TestRng::new("differs");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let personal = ada.personal_space();
	let boiler = note(&mut ada, personal, "Boiler service", at + 100, &mut rng);
	let moved = ada
		.move_entity(
			personal,
			boiler,
			household,
			MoveHistory::Leave,
			Behind::Purge,
			nibkeep_node::MoveSecrets::Bring,
			at + 200,
			&mut rng,
		)
		.expect("moves");
	// Its type and its name.
	assert_eq!(ada.behind(personal, boiler).expect("reads"), Some(2));
	// Renamed where it is now, it no longer shows what stays: nothing is purged.
	rename(
		&mut ada,
		(household, moved),
		"Boiler and radiators",
		at + 300,
		&mut rng,
	);
	let refused = ada.purge_behind(personal, boiler, at + 400, &mut rng);
	assert!(
		matches!(refused, Err(nibkeep_node::NodeError::MovedDiffers(_))),
		"{refused:?}"
	);
	assert_eq!(names(&ada, (personal, boiler)), ["Boiler service"]);
}
