//! Merging entities that turn out to be the same, and undoing a merge (`docs/spec/model.md`,
//! section 7).

use crate::common;

use crate::common::{library, now, this_day};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, Value, ValueKind, fields};
use nibkeep_node::{Library, NodeError};
use nibkeep_std::{AUTHOR, BOOK};
use nibkeep_testkit::TestRng;

const PERSON: nibkeep_model::TypeName = nibkeep_governance::PERSON;

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn name(text: &str) -> Value {
	Value::Name(Name::new(text))
}

/// A library with a person, the same person recorded again under another name with a note,
/// and a book credited to the second.
struct Duplicates {
	library: Library,
	main: EntityId,
	duplicate: EntityId,
	book: EntityId,
}

fn duplicates(test: &str, rng: &mut TestRng) -> Duplicates {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	library
		.define_type(
			personal,
			&TypeDef::new(PERSON, 1)
				.with_field(field("local.note"), FieldDef::new(ValueKind::Text).many()),
			now(),
			rng,
		)
		.expect("defines");
	let mut add = |library: &mut Library, values: Vec<(FieldName, Value)>, kind| {
		library
			.create_entity(personal, &[kind], values, now(), rng)
			.expect("creates")
	};
	let main = add(
		&mut library,
		vec![(fields::NAME, name("Daniel Abraham"))],
		PERSON,
	);
	let duplicate = add(
		&mut library,
		vec![
			(fields::NAME, name("D. Abraham")),
			(
				field("local.note"),
				Value::Text("writes fantasy".to_owned()),
			),
		],
		PERSON,
	);
	let book = add(
		&mut library,
		vec![
			(fields::NAME, name("The Dragon's Path")),
			(AUTHOR, Value::Reference(duplicate)),
		],
		BOOK,
	);
	Duplicates {
		library,
		main,
		duplicate,
		book,
	}
}

#[test]
fn a_merged_entity_shows_under_the_one_it_was_merged_into() {
	let mut rng = TestRng::new("merge");
	let Duplicates {
		mut library,
		main,
		duplicate,
		book,
	} = duplicates("merge", &mut rng);
	let personal = library.personal_space();
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");

	// The entity shows what both say: two names compete until a person chooses one.
	let view = library
		.entity(personal, main, this_day())
		.expect("a space")
		.expect("the person");
	let names = view
		.fields
		.iter()
		.find(|shown| shown.field == fields::NAME)
		.expect("names");
	assert!(names.competing);
	assert_eq!(names.values.len(), 2);
	assert!(
		view.fields
			.iter()
			.any(|shown| shown.field == field("local.note"))
	);
	// The duplicate leads to it, by ID and by name.
	assert_eq!(
		library
			.entity(personal, duplicate, this_day())
			.expect("a space")
			.map(|view| view.id),
		Some(main)
	);
	assert_eq!(
		library.find_entity(personal, "D. Abraham").expect("finds"),
		main
	);
	// What referred to the duplicate links to it.
	let links = library
		.links_to(personal, main, this_day(), 10)
		.expect("a space");
	assert_eq!(links[0].entities[0].id, book);
	// A search finds it once.
	let found = library.search(personal, "abraham").expect("a space");
	assert_eq!(found.iter().filter(|listed| listed.id == main).count(), 1);
	assert!(found.iter().all(|listed| listed.id != duplicate));
	// It lists the entity merged into it, by that entity's own name.
	let merged = library.merged(personal, main).expect("a space");
	assert_eq!(merged.len(), 1);
	assert_eq!(merged[0].id, duplicate);
	assert_eq!(merged[0].name.as_deref(), Some("D. Abraham"));
	nibkeep_testkit::assert_empty!(library.merged(personal, book).expect("a space"));
}

#[test]
fn a_merge_is_undone() {
	let mut rng = TestRng::new("undo merge");
	let Duplicates {
		mut library,
		main,
		duplicate,
		book,
	} = duplicates("undo merge", &mut rng);
	let personal = library.personal_space();
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");
	library
		.undo_merge(personal, duplicate, now(), &mut rng)
		.expect("undoes");
	assert_eq!(
		library
			.entity(personal, duplicate, this_day())
			.expect("a space")
			.map(|view| view.id),
		Some(duplicate)
	);
	nibkeep_testkit::assert_empty!(
		library
			.links_to(personal, main, this_day(), 10)
			.expect("a space")
	);
	assert_eq!(
		library
			.links_to(personal, duplicate, this_day(), 10)
			.expect("a space")[0]
			.entities[0]
			.id,
		book
	);
	nibkeep_testkit::assert_empty!(library.merged(personal, main).expect("a space"));
}

#[test]
fn merging_again_moves_the_entity_and_merges_join() {
	let mut rng = TestRng::new("merge again");
	let Duplicates {
		mut library,
		main,
		duplicate,
		book,
	} = duplicates("merge again", &mut rng);
	let personal = library.personal_space();
	let canonical = |library: &Library, entity| {
		library
			.entity(personal, entity, this_day())
			.expect("a space")
			.map(|view| view.id)
	};
	library
		.merge_entities(personal, duplicate, book, now(), &mut rng)
		.expect("merges, by mistake");
	// Merging it again moves it: one merge replaces the other.
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges again");
	assert_eq!(canonical(&library, duplicate), Some(main));
	assert_eq!(canonical(&library, book), Some(book));
	// Merging the entity that others were merged into takes them along.
	let third = library
		.create_entity(
			personal,
			&[PERSON],
			vec![(fields::NAME, name("Abraham, Daniel"))],
			now(),
			&mut rng,
		)
		.expect("creates");
	library
		.merge_entities(personal, main, third, now(), &mut rng)
		.expect("merges");
	assert_eq!(canonical(&library, duplicate), Some(third));
	let merged: Vec<EntityId> = library
		.merged(personal, third)
		.expect("a space")
		.iter()
		.map(|listed| listed.id)
		.collect();
	assert_eq!(merged.len(), 2);
	assert!(merged.contains(&main) && merged.contains(&duplicate));
}

#[test]
fn merges_that_cannot_be_are_refused() {
	let mut rng = TestRng::new("bad merges");
	let Duplicates {
		mut library,
		main,
		duplicate,
		..
	} = duplicates("bad merges", &mut rng);
	let personal = library.personal_space();
	let refused = |library: &mut Library, from, into, rng: &mut TestRng| {
		library
			.merge_entities(personal, from, into, now(), rng)
			.expect_err("refused")
	};
	assert!(matches!(
		refused(&mut library, main, main, &mut rng),
		NodeError::SameEntity(_)
	));
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");
	// They are one already, whichever way round.
	for (from, into) in [(main, duplicate), (duplicate, main)] {
		assert!(matches!(
			refused(&mut library, from, into, &mut rng),
			NodeError::SameEntity(_)
		));
	}
	let unknown = EntityId::random(&mut rng);
	assert!(matches!(
		refused(&mut library, unknown, main, &mut rng),
		NodeError::NoEntity(_)
	));
	assert!(matches!(
		refused(&mut library, main, unknown, &mut rng),
		NodeError::NoEntity(_)
	));
	// Types are defined by entities, which are not merged.
	let definition = library
		.entities(personal, Some(&nibkeep_model::schema::TYPE_DEFINITION))
		.expect("a space");
	let definition = definition.first().expect("a type definition").id;
	assert!(matches!(
		refused(&mut library, definition, main, &mut rng),
		NodeError::DefinesTypes(_)
	));
	// Only a merged entity's merge is undone.
	assert!(matches!(
		library.undo_merge(personal, main, now(), &mut rng),
		Err(NodeError::NotMerged(_))
	));
}

#[test]
fn a_merged_entity_is_found_by_either_name_after_opening_again() {
	let mut rng = TestRng::new("merge reopened");
	let (directory, mut library) = library("merge reopened", &mut rng);
	let personal = library.personal_space();
	let add = |library: &mut Library, text: &str, rng: &mut TestRng| {
		library
			.create_entity(
				personal,
				&[PERSON],
				vec![(fields::NAME, name(text))],
				now(),
				rng,
			)
			.expect("creates")
	};
	let main = add(&mut library, "Daniel Abraham", &mut rng);
	let duplicate = add(&mut library, "D. Abraham", &mut rng);
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");
	for text in ["Daniel Abraham", "D. Abraham"] {
		assert_eq!(
			library.find_entity_or_deleted(personal, text).expect(text),
			main
		);
	}
	drop(library);
	let library = Library::open(
		&directory,
		&nibkeep_node::Unlocker::Passphrase("correct horse"),
	)
	.expect("opens");
	for text in ["Daniel Abraham", "D. Abraham"] {
		assert_eq!(
			library.find_entity_or_deleted(personal, text).expect(text),
			main
		);
	}
}

#[test]
fn a_merged_entity_is_found_among_those_merged_by_its_own_names() {
	let mut rng = TestRng::new("find merged");
	let Duplicates {
		mut library,
		main,
		duplicate,
		..
	} = duplicates("find merged", &mut rng);
	let personal = library.personal_space();
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");
	for reference in [
		"D. Abraham",
		"d. abraham",
		&duplicate.to_string(),
		&duplicate.to_string()[..8],
	] {
		assert_eq!(
			library
				.find_merged(personal, main, reference)
				.expect(reference),
			duplicate
		);
	}
	// The entity merged into is not among those merged into it, nor is a short prefix.
	for reference in ["Daniel Abraham", "Someone", &duplicate.to_string()[..2]] {
		assert!(matches!(
			library.find_merged(personal, main, reference),
			Err(NodeError::NotMerged(_))
		));
	}
}

#[test]
fn merged_entities_named_in_other_fields_are_found_by_those_names() {
	let mut rng = TestRng::new("other naming fields");
	let (_, mut library) = library("other naming fields", &mut rng);
	let personal = library.personal_space();
	let note = nibkeep_model::TypeName::from_static("local.note");
	library
		.define_type(
			personal,
			&TypeDef::new(note.clone(), 1)
				.with_field(field("local.title"), FieldDef::new(ValueKind::Text)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let titled = library
		.create_entity(
			personal,
			&[note],
			vec![(
				field("local.title"),
				Value::Text("Notes on Abraham".to_owned()),
			)],
			now(),
			&mut rng,
		)
		.expect("creates");
	let named = library
		.create_entity(
			personal,
			&[PERSON],
			vec![(fields::NAME, name("Daniel Abraham"))],
			now(),
			&mut rng,
		)
		.expect("creates");
	library
		.merge_entities(personal, titled, named, now(), &mut rng)
		.expect("merges");
	for text in ["Daniel Abraham", "Notes on Abraham"] {
		assert_eq!(library.find_entity(personal, text).expect(text), named);
	}
}

#[test]
fn records_of_who_uses_the_library_and_deleted_entities_are_not_merged() {
	let mut rng = TestRng::new("own records");
	let Duplicates {
		mut library,
		main,
		duplicate,
		..
	} = duplicates("own records", &mut rng);
	let personal = library.personal_space();
	let user = library.status().expect("a status").user.id;
	assert!(matches!(
		library.merge_entities(personal, user, main, now(), &mut rng),
		Err(NodeError::OwnRecord(_))
	));
	assert!(matches!(
		library.merge_entities(personal, main, user, now(), &mut rng),
		Err(NodeError::OwnRecord(_))
	));
	library
		.edit_entity(
			personal,
			duplicate,
			vec![nibkeep_node::Edit::Delete],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	assert!(matches!(
		library.merge_entities(personal, duplicate, main, now(), &mut rng),
		Err(NodeError::Deleted(_))
	));
	assert!(matches!(
		library.merge_entities(personal, main, duplicate, now(), &mut rng),
		Err(NodeError::Deleted(_))
	));
}

#[test]
fn entities_merged_by_a_moment_are_listed_as_of_it() {
	let mut rng = TestRng::new("merged as of");
	let Duplicates {
		mut library,
		main,
		duplicate,
		..
	} = duplicates("merged as of", &mut rng);
	let personal = library.personal_space();
	let later = now() + 60_000;
	library
		.merge_entities(personal, duplicate, main, later, &mut rng)
		.expect("merges");
	nibkeep_testkit::assert_empty!(
		library
			.merged_as_of(personal, main, now())
			.expect("a space")
	);
	assert_eq!(
		library
			.merged_as_of(personal, main, later)
			.expect("a space")[0]
			.id,
		duplicate
	);
}

/// Each edit of a write sees what the edits before it did, also to the values of an entity
/// merged into the one edited, whose statements each change names apart.
#[test]
fn an_edit_sees_what_the_edit_before_it_took_from_a_merged_entity() {
	let mut rng = TestRng::new("edits of merged entities");
	let Duplicates {
		mut library,
		main,
		duplicate,
		..
	} = duplicates("edits of merged entities", &mut rng);
	let personal = library.personal_space();
	library
		.merge_entities(personal, duplicate, main, now(), &mut rng)
		.expect("merges");
	let note = || {
		nibkeep_node::Edit::Remove(
			field("local.note"),
			Value::Text("writes fantasy".to_owned()),
		)
	};
	assert!(matches!(
		library.edit_entity(
			personal,
			main,
			vec![note(), note()],
			&common::today(),
			now(),
			&mut rng
		),
		Err(NodeError::NoValue(_))
	));
	library
		.edit_entity(
			personal,
			main,
			vec![note()],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("removes the duplicate's note");
	let shown = library
		.entity(personal, main, this_day())
		.expect("reads")
		.expect("an entity");
	assert!(
		!shown
			.fields
			.iter()
			.any(|field| field.field.as_str() == "local.note"),
		"{shown:?}"
	);
}
