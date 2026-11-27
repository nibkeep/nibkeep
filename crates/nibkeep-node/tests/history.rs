//! History in system time: every value an entity has had, when it was recorded, by whom and
//! when it was replaced; and an entity as the library had recorded it at a moment.

use crate::common::{library, now, this_day, today};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, History, Library};
use nibkeep_std::{AUTHOR, BOOK, PEN_NAME};
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

/// A library with a note titled "Milk", made at [`now`].
fn with_a_note(test: &str, rng: &mut TestRng) -> (Library, EntityId) {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	let note = TypeDef::new(NOTE, 1)
		.with_field(field("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(field("local.tag"), FieldDef::new(ValueKind::Text).many());
	library
		.define_type(personal, &note, now(), rng)
		.expect("defines");
	let entity = library
		.create_entity(
			personal,
			&[NOTE],
			vec![(field("local.title"), text("Milk"))],
			now(),
			rng,
		)
		.expect("creates");
	(library, entity)
}

/// Edits an entity of the personal space at a moment.
fn edit(library: &mut Library, entity: EntityId, edits: Vec<Edit>, at: u64, rng: &mut TestRng) {
	let personal = library.personal_space();
	library
		.edit_entity(personal, entity, edits, &today(), at, rng)
		.expect("edits");
}

/// The history of an entity's field: each value with the milliseconds it was recorded and
/// replaced at.
fn of_field(history: &History, name: &str) -> Vec<(Value, u64, Option<u64>)> {
	history
		.fields
		.iter()
		.find(|past| past.field == field(name))
		.map_or_default(|past| {
			past.values
				.iter()
				.map(|value| {
					(
						value.value.clone(),
						value.recorded.millis(),
						value.replaced.map(nibkeep_time::Timestamp::millis),
					)
				})
				.collect()
		})
}

#[test]
fn history_lists_every_value_with_when_and_who() {
	let mut rng = TestRng::new("history");
	let (mut library, note) = with_a_note("history", &mut rng);
	let title = field("local.title");
	edit(
		&mut library,
		note,
		vec![Edit::Set(title.clone(), vec![text("Oat milk")])],
		later(5),
		&mut rng,
	);
	edit(
		&mut library,
		note,
		vec![Edit::Add(field("local.tag"), text("shop"))],
		later(10),
		&mut rng,
	);
	let personal = library.personal_space();
	let history = library
		.history(personal, note, None)
		.expect("a space")
		.expect("the note");
	assert_eq!(history.id, note);
	assert_eq!(history.name.as_deref(), Some("Oat milk"));
	let names: Vec<String> = history
		.fields
		.iter()
		.map(|past| past.field.to_string())
		.collect();
	assert_eq!(names, ["local.tag", "local.title", "org.nibkeep.type"]);
	assert_eq!(
		of_field(&history, "local.title"),
		[
			(text("Milk"), now(), Some(later(5))),
			(text("Oat milk"), later(5), None)
		]
	);
	assert_eq!(
		of_field(&history, "local.tag"),
		[(text("shop"), later(10), None)]
	);
	let author = history.fields[1].values[0]
		.author
		.as_ref()
		.expect("an author");
	assert_eq!(author.user.name.as_deref(), Some("Ada"));

	// One field alone.
	let only = library
		.history(personal, note, Some(&title))
		.expect("a space")
		.expect("the note");
	assert_eq!(only.fields.len(), 1);
	assert_eq!(only.fields[0].field, title);
}

#[test]
fn deleting_and_restoring_are_in_the_history() {
	let mut rng = TestRng::new("history of deletion");
	let (mut library, note) = with_a_note("history of deletion", &mut rng);
	edit(&mut library, note, vec![Edit::Delete], later(5), &mut rng);
	edit(&mut library, note, vec![Edit::Restore], later(10), &mut rng);
	let history = library
		.history(library.personal_space(), note, None)
		.expect("a space")
		.expect("the note");
	assert_eq!(
		of_field(&history, fields::DELETED.as_str()),
		[(Value::Boolean(true), later(5), Some(later(10)))]
	);
}

#[test]
fn an_unknown_entity_has_no_history() {
	let mut rng = TestRng::new("no history");
	let (_, library) = library("no history", &mut rng);
	let unknown = EntityId::random(&mut rng);
	assert!(
		library
			.history(library.personal_space(), unknown, None)
			.expect("a space")
			.is_none()
	);
}

#[test]
fn an_entity_is_shown_as_recorded_at_a_moment() {
	let mut rng = TestRng::new("as of");
	let (mut library, note) = with_a_note("as of", &mut rng);
	let title = field("local.title");
	edit(
		&mut library,
		note,
		vec![Edit::Set(title.clone(), vec![text("Oat milk")])],
		later(5),
		&mut rng,
	);
	edit(&mut library, note, vec![Edit::Delete], later(10), &mut rng);
	let personal = library.personal_space();
	let as_of = |millis: u64| {
		library
			.entity_as_of(personal, note, this_day(), millis)
			.expect("a space")
	};
	let title_then = |millis: u64| {
		let view = as_of(millis).expect("recorded by then");
		let value = view
			.fields
			.iter()
			.find(|shown| shown.field == title)
			.map(|shown| shown.values[0].value.clone());
		(value, view.deleted)
	};
	assert_eq!(title_then(later(1)), (Some(text("Milk")), false));
	assert_eq!(title_then(later(7)), (Some(text("Oat milk")), false));
	assert_eq!(title_then(later(12)), (Some(text("Oat milk")), true));
	// Its name, as recorded then.
	let name_then = |millis: u64| library.name_as_of(personal, note, millis).expect("a space");
	assert_eq!(name_then(later(1)).as_deref(), Some("Milk"));
	assert_eq!(name_then(later(7)).as_deref(), Some("Oat milk"));
	// Before it was recorded, the library knew nothing of it.
	assert!(as_of(now() - 1).is_none());
	assert_eq!(name_then(now() - 1), None);
	// Now, as recorded by now, it is as it is.
	assert_eq!(
		as_of(later(60)),
		library.entity(personal, note, this_day()).expect("a space")
	);
}

#[test]
fn links_are_shown_as_recorded_at_a_moment() {
	let mut rng = TestRng::new("links as of");
	let (_, mut library) = library("links as of", &mut rng);
	let personal = library.personal_space();
	let name = |text: &str| Value::Name(Name::new(text));
	let corey = library
		.create_entity(
			personal,
			&[PEN_NAME],
			vec![(fields::NAME, name("James S. A. Corey"))],
			now(),
			&mut rng,
		)
		.expect("creates");
	library
		.create_entity(
			personal,
			&[BOOK],
			vec![
				(fields::NAME, name("Leviathan Wakes")),
				(AUTHOR, Value::Reference(corey)),
			],
			later(5),
			&mut rng,
		)
		.expect("creates");
	let links_then = |millis: u64| {
		library
			.links_to_as_of(personal, corey, this_day(), millis, 10)
			.expect("a space")
	};
	nibkeep_testkit::assert_empty!(links_then(later(1)));
	assert_eq!(links_then(later(6))[0].count, 1);
}
