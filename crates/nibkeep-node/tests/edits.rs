//! Edits of entities: values set, added, removed and cleared, deletion and restoring, and
//! search.

use crate::common::{library, now, this_day, today};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, Library, NodeError};
use nibkeep_testkit::TestRng;

const BOOK: TypeName = TypeName::from_static("local.book");
const AUDIOBOOK: TypeName = TypeName::from_static("local.audiobook");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// A library with a book, and the book.
fn with_a_book(test: &str, rng: &mut TestRng) -> (Library, EntityId) {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	let book = TypeDef::new(BOOK, 1)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(name("local.tag"), FieldDef::new(ValueKind::Text).many());
	library
		.define_type(personal, &book, now(), rng)
		.expect("defines");
	let entity = library
		.create_entity(
			personal,
			&[BOOK],
			vec![(name("local.title"), text("Leviathan Wakes"))],
			now(),
			rng,
		)
		.expect("creates");
	(library, entity)
}

/// The values of a field of an entity, and whether they compete.
fn values(library: &Library, entity: EntityId, field: &str) -> (Vec<Value>, bool) {
	let personal = library.personal_space();
	let view = library
		.entity(personal, entity, this_day())
		.expect("a space")
		.expect("the entity");
	view.fields
		.iter()
		.find(|view| view.field == name(field))
		.map_or((Vec::new(), false), |view| {
			let values = view
				.values
				.iter()
				.map(|shown| shown.value.clone())
				.collect();
			(values, view.competing)
		})
}

#[test]
fn values_are_set_added_removed_and_cleared() {
	let mut rng = TestRng::new("values edited");
	let (mut library, book) = with_a_book("values edited", &mut rng);
	let personal = library.personal_space();
	let mut edit = |library: &mut Library, edits: Vec<Edit>| {
		library
			.edit_entity(personal, book, edits, &today(), now(), &mut rng)
			.expect("edits");
	};
	edit(
		&mut library,
		vec![
			Edit::Set(name("local.title"), vec![text("Leviathan wakes")]),
			Edit::Set(name("local.tag"), vec![text("space"), text("noir")]),
		],
	);
	assert_eq!(
		values(&library, book, "local.title"),
		(vec![text("Leviathan wakes")], false)
	);
	assert_eq!(values(&library, book, "local.tag").0.len(), 2);
	edit(
		&mut library,
		vec![Edit::Add(name("local.tag"), text("opera"))],
	);
	assert_eq!(values(&library, book, "local.tag").0.len(), 3);
	edit(
		&mut library,
		vec![Edit::Remove(name("local.tag"), text("noir"))],
	);
	let (tags, _) = values(&library, book, "local.tag");
	assert_eq!(tags.len(), 2);
	assert!(!tags.contains(&text("noir")));
	edit(&mut library, vec![Edit::Set(name("local.tag"), Vec::new())]);
	assert_eq!(values(&library, book, "local.tag").0, Vec::<Value>::new());

	// A second value of a field with one value competes, until a person chooses.
	edit(
		&mut library,
		vec![Edit::Add(name("local.title"), text("Leviathan Wakes"))],
	);
	let (titles, competing) = values(&library, book, "local.title");
	assert_eq!(titles.len(), 2);
	assert!(competing);
	edit(
		&mut library,
		vec![Edit::Set(
			name("local.title"),
			vec![text("Leviathan Wakes")],
		)],
	);
	assert_eq!(
		values(&library, book, "local.title"),
		(vec![text("Leviathan Wakes")], false)
	);
}

#[test]
fn entities_are_deleted_and_restored() {
	let mut rng = TestRng::new("deleted");
	let (mut library, book) = with_a_book("deleted", &mut rng);
	let personal = library.personal_space();
	let found = |library: &Library| {
		library
			.entities(personal, Some(&BOOK))
			.expect("a space")
			.len()
	};
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	assert!(
		library
			.entity(personal, book, this_day())
			.expect("a space")
			.expect("kept")
			.deleted
	);
	assert_eq!(found(&library), 0);
	let deleted = library.deleted(personal).expect("a space");
	assert_eq!(
		(deleted.len(), deleted[0].id, deleted[0].name.as_deref()),
		(1, book, Some("Leviathan Wakes"))
	);
	assert_eq!(
		library
			.find_entity_or_deleted(personal, "Leviathan Wakes")
			.expect("still found by name, to restore it"),
		book
	);
	assert!(
		matches!(
			library.find_entity(personal, "Leviathan Wakes"),
			Err(NodeError::NoEntity(_))
		),
		"references never point to a deleted entity by name"
	);
	let changes = library.status().expect("reads the status").spaces[0].changes;
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes again");
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes,
		"nothing to do"
	);
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Restore],
			&today(),
			now(),
			&mut rng,
		)
		.expect("restores");
	assert_eq!(found(&library), 1);
	nibkeep_testkit::assert_empty!(library.deleted(personal).expect("a space"));
}

#[test]
fn edits_that_cannot_be_made_are_refused() {
	let mut rng = TestRng::new("refused edits");
	let (mut library, book) = with_a_book("refused edits", &mut rng);
	let personal = library.personal_space();
	let changes = library.status().expect("reads the status").spaces[0].changes;
	let mut refused = |edits: Vec<Edit>| {
		library
			.edit_entity(personal, book, edits, &today(), now(), &mut rng)
			.expect_err("refused")
	};
	assert!(matches!(
		refused(vec![Edit::Set(fields::DELETED, vec![Value::Boolean(true)])]),
		NodeError::OwnField(_)
	));
	assert!(matches!(
		refused(vec![Edit::Set(
			name("local.title"),
			vec![text("a"), text("b")]
		)]),
		NodeError::OneValue(_)
	));
	assert!(matches!(
		refused(vec![Edit::Remove(name("local.tag"), text("never there"))]),
		NodeError::NoValue(_)
	));
	let nobody = EntityId::random(&mut rng);
	assert!(matches!(
		library.edit_entity(
			personal,
			nobody,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng
		),
		Err(NodeError::NoEntity(_))
	));
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes
	);
}

#[test]
fn search_finds_entities_by_their_words() {
	let mut rng = TestRng::new("search");
	let (mut library, book) = with_a_book("search", &mut rng);
	let personal = library.personal_space();
	let found = library.search(personal, "leviathan").expect("a space");
	assert_eq!(found.len(), 1);
	assert_eq!(found[0].id, book);
	assert_eq!(found[0].name.as_deref(), Some("Leviathan Wakes"));
	assert_eq!(found[0].types, [BOOK]);
	nibkeep_testkit::assert_empty!(library.search(personal, "caliban").expect("a space"));
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	nibkeep_testkit::assert_empty!(library.search(personal, "leviathan").expect("a space"));
}

#[test]
fn search_finds_names_but_not_keys() {
	let mut rng = TestRng::new("names but not keys");
	let (library, _) = with_a_book("names but not keys", &mut rng);
	let personal = library.personal_space();
	let status = library.status().expect("reads the status");
	let found = |query: &str| -> Vec<EntityId> {
		library
			.search(personal, query)
			.expect("a space")
			.into_iter()
			.map(|found| found.id)
			.collect()
	};
	assert_eq!(found("laptop"), [status.device.id]);
	assert_eq!(found("nib"), [status.replica.id]);
	let view = library
		.entity(personal, status.replica.id, this_day())
		.expect("a space")
		.expect("the replica");
	let key = view
		.fields
		.iter()
		.find(|field| field.field == nibkeep_governance::SIGNING_KEY)
		.and_then(|field| field.values.first())
		.and_then(|shown| shown.value.as_text())
		.expect("the replica's key")
		.to_owned();
	nibkeep_testkit::assert_empty!(found(&key[..6]), "a replica's key is not searched");
}

#[test]
fn each_edit_sees_the_ones_before_it() {
	let mut rng = TestRng::new("in order");
	let (mut library, book) = with_a_book("in order", &mut rng);
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			book,
			vec![
				Edit::Add(name("local.tag"), text("x")),
				Edit::Remove(name("local.tag"), text("x")),
			],
			&today(),
			now(),
			&mut rng,
		)
		.expect("adds and removes");
	nibkeep_testkit::assert_empty!(values(&library, book, "local.tag").0);
	let deleted = |library: &Library| {
		library
			.entity(personal, book, this_day())
			.expect("a space")
			.expect("kept")
			.deleted
	};
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Delete, Edit::Restore],
			&today(),
			now(),
			&mut rng,
		)
		.expect("edits");
	assert!(!deleted(&library), "restored last");
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Restore, Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("edits");
	assert!(deleted(&library), "deleted last");
}

#[test]
fn setting_the_values_a_field_has_changes_nothing() {
	let mut rng = TestRng::new("same values");
	let (mut library, book) = with_a_book("same values", &mut rng);
	let personal = library.personal_space();
	let changes = library.status().expect("reads the status").spaces[0].changes;
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Set(
				name("local.title"),
				vec![text("Leviathan Wakes")],
			)],
			&today(),
			now(),
			&mut rng,
		)
		.expect("edits");
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes
	);
}

#[test]
fn removing_the_last_value_written_clears_the_field() {
	let mut rng = TestRng::new("last writer wins");
	let (mut library, book) = with_a_book("last writer wins", &mut rng);
	let personal = library.personal_space();
	let status = TypeDef::new(BOOK, 1).with_field(
		name("local.status"),
		FieldDef::new(ValueKind::Text).last_writer_wins(),
	);
	library
		.define_type(personal, &status, now(), &mut rng)
		.expect("defines");
	for value in ["draft", "final"] {
		library
			.edit_entity(
				personal,
				book,
				vec![Edit::Add(name("local.status"), text(value))],
				&today(),
				now(),
				&mut rng,
			)
			.expect("adds");
	}
	assert_eq!(values(&library, book, "local.status").0, [text("final")]);
	let found =
		|library: &Library, word: &str| library.search(personal, word).expect("a space").len();
	assert_eq!(found(&library, "final"), 1);
	assert_eq!(
		found(&library, "draft"),
		0,
		"search finds only what the field shows"
	);
	assert!(matches!(
		library.edit_entity(
			personal,
			book,
			vec![Edit::Remove(name("local.status"), text("draft"))],
			&today(),
			now(),
			&mut rng
		),
		Err(NodeError::NoValue(_))
	));
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Remove(name("local.status"), text("final"))],
			&today(),
			now(),
			&mut rng,
		)
		.expect("removes");
	nibkeep_testkit::assert_empty!(
		values(&library, book, "local.status").0,
		"draft does not come back"
	);
}

#[test]
fn definitions_of_types_are_not_edited_as_entities() {
	use nibkeep_model::schema::FIELD_DEFINITION;

	let mut rng = TestRng::new("definitions");
	let (mut library, _) = with_a_book("definitions", &mut rng);
	let personal = library.personal_space();
	let definition = library
		.entities(personal, Some(&FIELD_DEFINITION))
		.expect("a space")
		.first()
		.expect("a field's definition")
		.id;
	assert!(matches!(
		library.edit_entity(
			personal,
			definition,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng
		),
		Err(NodeError::OwnType(_))
	));
}

#[test]
fn types_are_given_and_taken_away() {
	let mut rng = TestRng::new("types edited");
	let (mut library, book) = with_a_book("types edited", &mut rng);
	let personal = library.personal_space();
	let types = |library: &Library| {
		library
			.entity(personal, book, this_day())
			.expect("a space")
			.expect("the book")
			.types
	};
	let mut edit = |library: &mut Library, edits: Vec<Edit>, at: u64| {
		library.edit_entity(personal, book, edits, &today(), now() + at, &mut rng)
	};
	let changes = |library: &Library| library.status().expect("a status").spaces[0].changes;

	edit(&mut library, vec![Edit::AddType(AUDIOBOOK)], 1_000).expect("gives a type");
	assert_eq!(types(&library), [AUDIOBOOK, BOOK]);
	let before = changes(&library);
	edit(&mut library, vec![Edit::AddType(AUDIOBOOK)], 2_000).expect("gives it again");
	assert_eq!(changes(&library), before, "a type it has changes nothing");
	assert_eq!(
		library.entities(personal, Some(&AUDIOBOOK)).expect("lists")[0].id,
		book,
		"it is listed with the type"
	);

	edit(&mut library, vec![Edit::RemoveType(BOOK)], 3_000).expect("takes a type away");
	assert_eq!(types(&library), [AUDIOBOOK]);
	assert_eq!(
		values(&library, book, "local.title").0,
		[text("Leviathan Wakes")],
		"the values stay"
	);
	assert_eq!(
		library.entities(personal, Some(&BOOK)).expect("lists"),
		[],
		"it is no longer listed as a book"
	);
	assert!(matches!(
		edit(&mut library, vec![Edit::RemoveType(BOOK)], 4_000),
		Err(NodeError::NotOfType(name)) if name == BOOK
	));
	assert!(matches!(
		edit(&mut library, vec![Edit::RemoveType(AUDIOBOOK)], 5_000),
		Err(NodeError::OnlyType(name)) if name == AUDIOBOOK
	));
	// A type given and the other taken away in one edit.
	edit(
		&mut library,
		vec![Edit::AddType(BOOK), Edit::RemoveType(AUDIOBOOK)],
		6_000,
	)
	.expect("swaps the types");
	assert_eq!(types(&library), [BOOK]);

	for own in [
		nibkeep_model::schema::TYPE_DEFINITION,
		nibkeep_governance::USER,
		nibkeep_governance::REPLICA,
	] {
		assert!(
			matches!(
				edit(&mut library, vec![Edit::AddType(own.clone())], 7_000),
				Err(NodeError::OwnType(refused)) if refused == own
			),
			"{own}"
		);
	}
	let user = library.status().expect("a status").user.id;
	for retyped in [
		Edit::RemoveType(nibkeep_governance::USER),
		Edit::AddType(BOOK),
	] {
		assert!(matches!(
			library.edit_entity(
				personal,
				user,
				vec![retyped],
				&today(),
				now() + 8_000,
				&mut rng
			),
			Err(NodeError::OwnType(_))
		));
	}
}
