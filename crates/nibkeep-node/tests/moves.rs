//! Moves between spaces of one's own that bring the entity's history, as the mover's account
//! of it, and that take folders and lists with what they hold, through every level
//! ([technical design: spaces and moves]).
//!
//! [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves

mod common;

use common::{library, now, this_day, today};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_log::list::MEMBERSHIP;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, Library, MoveHistory, NodeError};
use nibkeep_std::{FOLDER, IN_FOLDER};
use nibkeep_testkit::TestRng;

const NOTE: TypeName = TypeName::from_static("local.note");

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// `minutes` after [`now`].
fn later(minutes: u64) -> u64 {
	now() + minutes * 60_000
}

/// A library with a household space, and notes with a title, tags and folders in the
/// personal space.
fn with_notes(test: &str, rng: &mut TestRng) -> (Library, SpaceId, SpaceId) {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	let household = library
		.create_space("Household", now(), rng)
		.expect("makes the space");
	// In folders as the standard vocabulary has it.
	let in_folder = nibkeep_std::types()
		.into_iter()
		.find(|definition| *definition.name() == FOLDER)
		.and_then(|folder| folder.field(&IN_FOLDER).cloned())
		.expect("the standard folder field");
	let note = TypeDef::new(NOTE, 1)
		.with_field(field("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(field("local.tag"), FieldDef::new(ValueKind::Text).many())
		.with_field(IN_FOLDER, in_folder);
	library
		.define_type(personal, &note, now(), rng)
		.expect("defines");
	(library, personal, household)
}

/// A note of a space, titled `title`, in the folders given.
fn note(
	library: &mut Library,
	space: SpaceId,
	title: &str,
	folders: &[EntityId],
	at: u64,
	rng: &mut TestRng,
) -> EntityId {
	let mut values = vec![(field("local.title"), text(title))];
	values.extend(
		folders
			.iter()
			.map(|folder| (IN_FOLDER, Value::Reference(*folder))),
	);
	library
		.create_entity(space, &[NOTE], values, at, rng)
		.expect("creates")
}

/// A folder of a space, named `name`, in the folders given.
fn folder(
	library: &mut Library,
	space: SpaceId,
	name: &str,
	folders: &[EntityId],
	at: u64,
	rng: &mut TestRng,
) -> EntityId {
	let mut values = vec![(fields::NAME, Value::Name(Name::new(name)))];
	values.extend(
		folders
			.iter()
			.map(|folder| (IN_FOLDER, Value::Reference(*folder))),
	);
	library
		.create_entity(space, &[FOLDER], values, at, rng)
		.expect("creates")
}

/// A membership that puts `item` in the ordered list `list`, at `position`.
fn member(
	library: &mut Library,
	space: SpaceId,
	(list, item, position): (EntityId, EntityId, &str),
	at: u64,
	rng: &mut TestRng,
) -> EntityId {
	library
		.create_entity(
			space,
			&[MEMBERSHIP],
			vec![
				(fields::LIST, Value::Reference(list)),
				(fields::ITEM, Value::Reference(item)),
				(fields::POSITION, text(position)),
			],
			at,
			rng,
		)
		.expect("adds to the list")
}

/// Makes one edit to an entity of a space, at a moment.
fn edit(
	library: &mut Library,
	(space, entity): (SpaceId, EntityId),
	edit: Edit,
	at: u64,
	rng: &mut TestRng,
) {
	library
		.edit_entity(space, entity, vec![edit], &today(), at, rng)
		.expect("edits");
}

/// Moves an entity of one space to another.
fn moved(
	library: &mut Library,
	(from, entity, to): (SpaceId, EntityId, SpaceId),
	history: MoveHistory,
	at: u64,
	rng: &mut TestRng,
) -> EntityId {
	library
		.move_entity(
			from,
			entity,
			to,
			history,
			nibkeep_node::Behind::Keep,
			nibkeep_node::MoveSecrets::Bring,
			at,
			rng,
		)
		.expect("moves")
}

/// The name of an author's user.
fn user_name(author: Option<&nibkeep_node::Author>) -> Option<&str> {
	author.and_then(|author| author.user.name.as_deref())
}

/// The current values of an entity's field, as a space shows them today.
fn values(library: &Library, space: SpaceId, entity: EntityId, name: &FieldName) -> Vec<Value> {
	library
		.entity(space, entity, this_day())
		.expect("reads")
		.expect("an entity")
		.fields
		.iter()
		.filter(|shown| shown.field == *name)
		.flat_map(|shown| shown.values.iter().map(|value| value.value.clone()))
		.collect()
}

/// The entity that a moved one is now, in `space`.
fn now_in(library: &Library, from: SpaceId, entity: EntityId, space: SpaceId) -> EntityId {
	let (there, moved) = library
		.moved_into(from, entity)
		.expect("reads")
		.expect("moved");
	assert_eq!(there, space);
	moved
}

#[test]
fn a_move_brings_the_history_as_the_movers_account_of_it() {
	let mut rng = TestRng::new("a move brings history");
	let (mut laptop, personal, household) = with_notes("a move brings history", &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(1), &mut rng);
	edit(
		&mut laptop,
		(personal, milk),
		Edit::Set(field("local.title"), vec![text("Milk and eggs")]),
		later(2),
		&mut rng,
	);
	edit(
		&mut laptop,
		(personal, milk),
		Edit::Add(field("local.tag"), text("shopping")),
		later(3),
		&mut rng,
	);
	let moved = moved(
		&mut laptop,
		(personal, milk, household),
		MoveHistory::Bring,
		later(10),
		&mut rng,
	);

	// It shows what it showed, once each.
	assert_eq!(
		values(&laptop, household, moved, &field("local.title")),
		[text("Milk and eggs")]
	);
	assert_eq!(
		values(&laptop, household, moved, &field("local.tag")),
		[text("shopping")]
	);
	// Its history is there, carried by the mover at the move, with when and by whom the
	// personal space recorded each value.
	let history = laptop
		.history(household, moved, Some(&field("local.title")))
		.expect("reads")
		.expect("a history");
	let titles = &history.fields[0].values;
	assert_eq!(
		titles
			.iter()
			.map(|value| value.value.clone())
			.collect::<Vec<_>>(),
		[text("Milk"), text("Milk and eggs")]
	);
	for value in titles {
		assert_eq!(value.recorded.millis(), later(10), "recorded at the move");
		assert_eq!(user_name(value.author.as_ref()), Some("Ada"));
		let carried = value.carried.as_ref().expect("carried");
		assert_eq!(carried.from, Some(personal));
		assert_eq!(user_name(carried.author.as_ref()), Some("Ada"));
	}
	let carried: Vec<(Option<u64>, Option<u64>)> = titles
		.iter()
		.map(|value| {
			let carried = value.carried.as_ref().expect("carried");
			(carried.recorded, carried.replaced)
		})
		.collect();
	assert_eq!(
		carried,
		[(Some(later(1)), Some(later(2))), (Some(later(2)), None)]
	);
	assert!(titles[0].replaced.is_some(), "the first title was replaced");
	assert!(titles[1].replaced.is_none());
	// It shows from the move on: before, the household knew nothing of it, and the personal
	// space still holds it as it was.
	assert!(
		laptop
			.entity_as_of(household, moved, this_day(), later(5))
			.expect("reads")
			.is_none()
	);
	assert_eq!(
		laptop
			.history(personal, milk, Some(&field("local.title")))
			.expect("reads")
			.expect("a history")
			.fields[0]
			.values
			.len(),
		2
	);
	// A value added there after is no note, and the same value written again is one value.
	edit(
		&mut laptop,
		(household, moved),
		Edit::Add(field("local.tag"), text("shopping")),
		later(11),
		&mut rng,
	);
	assert_eq!(
		values(&laptop, household, moved, &field("local.tag")),
		[text("shopping")]
	);
}

#[test]
fn a_move_without_history_brings_the_current_values_alone() {
	let mut rng = TestRng::new("a move without history");
	let (mut laptop, personal, household) = with_notes("a move without history", &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(1), &mut rng);
	edit(
		&mut laptop,
		(personal, milk),
		Edit::Set(field("local.title"), vec![text("Milk and eggs")]),
		later(2),
		&mut rng,
	);
	let moved = moved(
		&mut laptop,
		(personal, milk, household),
		MoveHistory::Leave,
		later(10),
		&mut rng,
	);
	let history = laptop
		.history(household, moved, Some(&field("local.title")))
		.expect("reads")
		.expect("a history");
	let titles = &history.fields[0].values;
	assert_eq!(titles.len(), 1);
	assert_eq!(titles[0].value, text("Milk and eggs"));
	assert!(titles[0].carried.is_none());
}

#[test]
fn competing_values_stay_competing() {
	let mut rng = TestRng::new("competing values move");
	let (mut laptop, personal, household) = with_notes("competing values move", &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(1), &mut rng);
	edit(
		&mut laptop,
		(personal, milk),
		Edit::Add(field("local.title"), text("Eggs")),
		later(2),
		&mut rng,
	);
	let before = laptop
		.entity(personal, milk, this_day())
		.expect("reads")
		.expect("an entity");
	let moved = moved(
		&mut laptop,
		(personal, milk, household),
		MoveHistory::Bring,
		later(10),
		&mut rng,
	);
	let after = laptop
		.entity(household, moved, this_day())
		.expect("reads")
		.expect("an entity");
	let titles = |view: &nibkeep_node::EntityView| {
		view.fields
			.iter()
			.find(|shown| shown.field == field("local.title"))
			.map(|shown| {
				(
					shown.competing,
					shown
						.values
						.iter()
						.map(|value| value.value.clone())
						.collect::<Vec<_>>(),
				)
			})
	};
	assert_eq!(titles(&after), titles(&before));
	assert!(titles(&after).is_some_and(|(competing, _)| competing));
}

#[test]
fn a_folder_moves_with_what_it_holds_through_every_level() {
	let mut rng = TestRng::new("a folder moves");
	let (mut laptop, personal, household) = with_notes("a folder moves", &mut rng);
	let house = folder(&mut laptop, personal, "House", &[], later(1), &mut rng);
	let garden = folder(
		&mut laptop,
		personal,
		"Garden",
		&[house],
		later(1),
		&mut rng,
	);
	let errands = folder(&mut laptop, personal, "Errands", &[], later(1), &mut rng);
	let boiler = note(
		&mut laptop,
		personal,
		"Boiler",
		&[house],
		later(2),
		&mut rng,
	);
	let hedge = note(
		&mut laptop,
		personal,
		"Hedge",
		&[garden, errands],
		later(2),
		&mut rng,
	);
	note(
		&mut laptop,
		personal,
		"Stamps",
		&[errands],
		later(2),
		&mut rng,
	);

	let preview = laptop
		.move_preview(personal, house, household)
		.expect("previews");
	assert_eq!(preview.entities.len(), 4, "{preview:?}");
	assert_eq!(preview.entities[0], house);
	for entity in [garden, boiler, hedge] {
		assert!(preview.entities.contains(&entity));
	}
	assert_eq!(preview.staying, [errands]);

	let moved = moved(
		&mut laptop,
		(personal, house, household),
		MoveHistory::Bring,
		later(10),
		&mut rng,
	);
	let (garden_now, boiler_now, hedge_now) = (
		now_in(&laptop, personal, garden, household),
		now_in(&laptop, personal, boiler, household),
		now_in(&laptop, personal, hedge, household),
	);
	assert_eq!(
		values(&laptop, household, garden_now, &IN_FOLDER),
		[Value::Reference(moved)]
	);
	assert_eq!(
		values(&laptop, household, boiler_now, &IN_FOLDER),
		[Value::Reference(moved)]
	);
	// The hedge is in the garden there, and in the errands, which stay, through its mark.
	assert_eq!(
		values(&laptop, household, hedge_now, &IN_FOLDER),
		[Value::Reference(garden_now)]
	);
	assert!(values(&laptop, personal, hedge, &IN_FOLDER).contains(&Value::Reference(errands)));
	assert!(
		laptop
			.moved_into(personal, errands)
			.expect("reads")
			.is_none()
	);
	// Nothing that stayed links to the hedge: a folder is what its members name.
	let links = laptop
		.links_to_anywhere(household, hedge_now, this_day(), 10)
		.expect("reads");
	assert!(
		links.iter().all(|links| links.field != IN_FOLDER),
		"no folder there holds it but the garden, which it names"
	);
}

#[test]
fn a_list_moves_with_its_memberships_and_their_items() {
	let mut rng = TestRng::new("a list moves");
	let (mut laptop, personal, household) = with_notes("a list moves", &mut rng);
	let shopping = note(&mut laptop, personal, "Shopping", &[], later(1), &mut rng);
	let favourites = note(&mut laptop, personal, "Favourites", &[], later(1), &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(1), &mut rng);
	let eggs = note(&mut laptop, personal, "Eggs", &[], later(1), &mut rng);
	member(
		&mut laptop,
		personal,
		(shopping, milk, "b"),
		later(2),
		&mut rng,
	);
	member(
		&mut laptop,
		personal,
		(shopping, eggs, "a"),
		later(2),
		&mut rng,
	);
	let kept = member(
		&mut laptop,
		personal,
		(favourites, milk, "a"),
		later(2),
		&mut rng,
	);

	let preview = laptop
		.move_preview(personal, shopping, household)
		.expect("previews");
	assert_eq!(
		preview.entities.len(),
		5,
		"the list, two memberships, two items"
	);
	assert_eq!(preview.staying, [favourites]);
	let moved = moved(
		&mut laptop,
		(personal, shopping, household),
		MoveHistory::Bring,
		later(10),
		&mut rng,
	);
	let (milk_now, eggs_now) = (
		now_in(&laptop, personal, milk, household),
		now_in(&laptop, personal, eggs, household),
	);
	// The memberships there put the items there in the list there, in their order.
	let members = laptop.list_members(household, moved).expect("lists");
	assert_eq!(
		members.iter().map(|member| member.item).collect::<Vec<_>>(),
		[Some(eggs_now), Some(milk_now)]
	);
	// The favourites stay, with their membership, which finds the milk through its mark.
	assert!(laptop.moved_into(personal, kept).expect("reads").is_none());
	assert_eq!(
		laptop
			.list_members(personal, favourites)
			.expect("lists")
			.iter()
			.map(|member| member.item)
			.collect::<Vec<_>>(),
		[Some(milk)]
	);
	// On the milk there, the household shows its own membership, and the favourites' that
	// led to the milk that was.
	let linking: Vec<EntityId> = laptop
		.links_to_anywhere(household, milk_now, this_day(), 10)
		.expect("reads")
		.iter()
		.flat_map(|links| links.entities.iter().map(|linking| linking.id))
		.collect();
	assert_eq!(linking.len(), 2, "{linking:?}");
	assert!(linking.contains(&kept));
	assert_eq!(
		laptop
			.links_to(household, milk_now, this_day(), 10)
			.expect("reads")
			.iter()
			.map(|links| links.count)
			.sum::<usize>(),
		1,
		"in the household alone, its own"
	);
}

#[test]
fn a_folder_that_holds_what_cannot_move_now_does_not_move() {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_keys::NewProtection;
	let mut rng = TestRng::new("a folder that cannot move");
	let (mut laptop, personal, household) = with_notes("a folder that cannot move", &mut rng);
	let secrets =
		NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"));
	laptop
		.set_up_secrets(&secrets, later(1), &mut rng)
		.expect("sets up the secrets");
	let banking = folder(&mut laptop, personal, "Banking", &[], later(2), &mut rng);
	let bills = note(
		&mut laptop,
		personal,
		"Bills",
		&[banking],
		later(2),
		&mut rng,
	);
	laptop
		.create_entity(
			personal,
			&[nibkeep_std::LOGIN],
			vec![
				(nibkeep_std::PASSWORD, text("hunter2")),
				(IN_FOLDER, Value::Reference(banking)),
			],
			later(3),
			&mut rng,
		)
		.expect("adds");
	laptop.lock_secrets();
	assert!(matches!(
		laptop.move_entity(
			personal,
			banking,
			household,
			MoveHistory::Bring,
			nibkeep_node::Behind::Keep,
			nibkeep_node::MoveSecrets::Bring,
			later(4),
			&mut rng
		),
		Err(NodeError::MoveNeedsSecrets)
	));
	// Nothing moved, not even what could.
	for entity in [banking, bills] {
		assert!(
			laptop
				.moved_into(personal, entity)
				.expect("reads")
				.is_none()
		);
	}
}

#[test]
fn what_moved_already_is_linked_where_it_went_and_a_membership_moves_with_its_list() {
	let mut rng = TestRng::new("what moved already");
	let (mut laptop, personal, household) = with_notes("what moved already", &mut rng);
	let house = folder(&mut laptop, personal, "House", &[], later(1), &mut rng);
	let house_now = moved(
		&mut laptop,
		(personal, house, household),
		MoveHistory::Bring,
		later(2),
		&mut rng,
	);
	// A note that still names the house that was, as one written on a device that had not
	// seen the move: moved after it, it is in the house there.
	let boiler = note(
		&mut laptop,
		personal,
		"Boiler",
		&[house],
		later(3),
		&mut rng,
	);
	let preview = laptop
		.move_preview(personal, boiler, household)
		.expect("previews");
	assert_eq!(preview.entities, [boiler]);
	nibkeep_testkit::assert_empty!(preview.staying, "the house is no longer here");
	let boiler_now = moved(
		&mut laptop,
		(personal, boiler, household),
		MoveHistory::Bring,
		later(4),
		&mut rng,
	);
	assert_eq!(
		values(&laptop, household, boiler_now, &IN_FOLDER),
		[Value::Reference(house_now)]
	);
	// A membership alone is no entity to move: it moves with its list.
	let list = note(&mut laptop, personal, "Shopping", &[], later(5), &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(5), &mut rng);
	let membership = member(&mut laptop, personal, (list, milk, "a"), later(5), &mut rng);
	assert!(matches!(
		laptop.move_preview(personal, membership, household),
		Err(NodeError::OwnRecord(_))
	));
	assert!(matches!(
		laptop.move_entity(
			personal,
			membership,
			household,
			MoveHistory::Bring,
			nibkeep_node::Behind::Keep,
			nibkeep_node::MoveSecrets::Bring,
			later(6),
			&mut rng
		),
		Err(NodeError::OwnRecord(_))
	));
}

#[test]
fn links_from_before_a_move_are_counted_beyond_those_shown() {
	let mut rng = TestRng::new("links counted");
	let (mut laptop, personal, household) = with_notes("links counted", &mut rng);
	let shopping = note(&mut laptop, personal, "Shopping", &[], later(1), &mut rng);
	let favourites = note(&mut laptop, personal, "Favourites", &[], later(1), &mut rng);
	let milk = note(&mut laptop, personal, "Milk", &[], later(1), &mut rng);
	// Twelve lists that stay hold the milk, and the shopping list that moves with it.
	for index in 0..12 {
		let list = note(
			&mut laptop,
			personal,
			&format!("List {index:02}"),
			&[],
			later(2),
			&mut rng,
		);
		member(&mut laptop, personal, (list, milk, "a"), later(2), &mut rng);
	}
	member(
		&mut laptop,
		personal,
		(shopping, milk, "a"),
		later(2),
		&mut rng,
	);
	member(
		&mut laptop,
		personal,
		(favourites, milk, "b"),
		later(2),
		&mut rng,
	);
	moved(
		&mut laptop,
		(personal, shopping, household),
		MoveHistory::Bring,
		later(10),
		&mut rng,
	);
	let milk_now = now_in(&laptop, personal, milk, household);
	let links = laptop
		.links_to_anywhere(household, milk_now, this_day(), 5)
		.expect("reads");
	let items: Vec<_> = links
		.iter()
		.filter(|links| links.field == fields::ITEM)
		.collect();
	assert_eq!(items.len(), 1, "{links:?}");
	assert_eq!(
		items[0].count, 14,
		"the household's membership and the thirteen that stay"
	);
	assert_eq!(items[0].entities.len(), 5);
}
