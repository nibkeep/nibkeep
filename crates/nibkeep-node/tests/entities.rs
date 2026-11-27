//! Entities in a library's spaces: created, shown, listed, and found by reference.

use crate::common;

use crate::common::{library, now, this_day};
use nibkeep_base::EntityId;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Library, NewLibrary, NodeError};
use nibkeep_testkit::TestRng;

const BOOK: TypeName = TypeName::from_static("local.book");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

/// A library with the type of books.
fn with_books(test: &str, rng: &mut TestRng) -> Library {
	let (_, mut library) = library(test, rng);
	let book = TypeDef::new(BOOK, 1)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(
			name("local.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		)
		.with_field(name("local.year"), FieldDef::new(ValueKind::Date));
	let personal = library.personal_space();
	library
		.define_type(personal, &book, now(), rng)
		.expect("defines");
	library
}

fn new_book(library: &mut Library, title: &str, rng: &mut TestRng) -> EntityId {
	let personal = library.personal_space();
	library
		.create_entity(
			personal,
			&[BOOK],
			vec![(name("local.title"), Value::Text(title.to_owned()))],
			now(),
			rng,
		)
		.expect("creates")
}

#[test]
fn an_entity_is_created_and_shown() {
	let mut rng = TestRng::new("created and shown");
	let mut library = with_books("created and shown", &mut rng);
	let personal = library.personal_space();
	let author = library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new("Daniel Abraham")))],
			now(),
			&mut rng,
		)
		.expect("creates");
	let book = library
		.create_entity(
			personal,
			&[BOOK],
			vec![
				(
					name("local.title"),
					Value::Text("Leviathan Wakes".to_owned()),
				),
				(name("local.author"), Value::Reference(author)),
				(
					name("local.year"),
					Value::Date("2011".parse().expect("a date")),
				),
			],
			now(),
			&mut rng,
		)
		.expect("creates");
	let view = library
		.entity(personal, book, this_day())
		.expect("a space")
		.expect("the book");
	assert_eq!(view.id, book);
	assert_eq!(view.name.as_deref(), Some("Leviathan Wakes"), "its title");
	assert_eq!(view.types, [BOOK]);
	assert!(!view.deleted);
	let fields: Vec<(String, Vec<Value>, bool)> = view
		.fields
		.iter()
		.map(|field| {
			let values = field
				.values
				.iter()
				.map(|shown| shown.value.clone())
				.collect();
			(field.field.to_string(), values, field.competing)
		})
		.collect();
	assert_eq!(
		fields,
		[
			(
				"local.author".to_owned(),
				vec![Value::Reference(author)],
				false
			),
			(
				"local.title".to_owned(),
				vec![Value::Text("Leviathan Wakes".to_owned())],
				false
			),
			(
				"local.year".to_owned(),
				vec![Value::Date("2011".parse().expect("a date"))],
				false
			),
		]
	);
	let author_view = library
		.entity(personal, author, this_day())
		.expect("a space")
		.expect("the author");
	assert_eq!(
		author_view.name.as_deref(),
		Some("Daniel Abraham"),
		"its name"
	);
	assert!(
		library
			.entity(personal, EntityId::random(&mut rng), this_day())
			.expect("a space")
			.is_none()
	);
}

#[test]
fn entities_are_listed_by_type_and_name() {
	let mut rng = TestRng::new("listed");
	let mut library = with_books("listed", &mut rng);
	let personal = library.personal_space();
	let leviathan = new_book(&mut library, "Leviathan Wakes", &mut rng);
	let caliban = new_book(&mut library, "Caliban's War", &mut rng);
	let books = library.entities(personal, Some(&BOOK)).expect("a space");
	let names: Vec<Option<&str>> = books.iter().map(|book| book.name.as_deref()).collect();
	assert_eq!(names, [Some("Caliban's War"), Some("Leviathan Wakes")]);
	assert_eq!(books[0].id, caliban);
	assert_eq!(books[1].id, leviathan);
	assert_eq!(books[0].types, [BOOK]);
	let all = library.entities(personal, None).expect("a space");
	let all_names: Vec<Option<&str>> = all.iter().map(|entity| entity.name.as_deref()).collect();
	// The user and the person linked to the user, both named Ada.
	assert!(all_names.starts_with(&[
		Some("Ada"),
		Some("Ada"),
		Some("Caliban's War"),
		Some("laptop")
	]));
	assert!(all.iter().any(|entity| entity.id == leviathan));
}

#[test]
fn entities_are_found_by_id_by_prefix_or_by_name() {
	let mut rng = TestRng::new("found");
	let mut library = with_books("found", &mut rng);
	let personal = library.personal_space();
	let book = new_book(&mut library, "Leviathan Wakes", &mut rng);
	let id = book.to_string();
	assert_eq!(library.find_entity(personal, &id).expect("by ID"), book);
	assert_eq!(
		library.find_entity(personal, &id[..8]).expect("by prefix"),
		book
	);
	assert_eq!(
		library
			.find_entity(personal, "leviathan wakes")
			.expect("by name"),
		book
	);
	assert!(matches!(
		library.find_entity(personal, "Caliban's War"),
		Err(NodeError::NoEntity(reference)) if reference == "Caliban's War"
	));
	let first = new_book(&mut library, "Twins", &mut rng);
	let second = new_book(&mut library, "Twins", &mut rng);
	match library.find_entity(personal, "twins") {
		Err(NodeError::Ambiguous {
			reference,
			candidates,
		}) => {
			assert_eq!(reference, "twins");
			assert_eq!(candidates.len(), 2);
			assert!(candidates.contains(&first) && candidates.contains(&second));
		}
		other => panic!("ambiguous, not {other:?}"),
	}
}

#[test]
fn entities_are_found_by_the_name_they_have_now_in_any_field_that_names_them() {
	for secret_name in [false, true] {
		let test = format!("found by name {secret_name}");
		let mut rng = TestRng::new(&test);
		let mut library = with_books(&test, &mut rng);
		// A field that names entities and is secret, whose values search cannot find, has
		// them found by name otherwise.
		if secret_name {
			let agent = TypeDef::new(TypeName::from_static("local.agent"), 1).with_field(
				name("local.alias.name"),
				FieldDef::new(ValueKind::Text).secret(),
			);
			let personal = library.personal_space();
			library
				.define_type(personal, &agent, now(), &mut rng)
				.expect("defines");
		}
		found_by_name(&mut library, &mut rng);
	}
}

fn found_by_name(library: &mut Library, rng: &mut TestRng) {
	use nibkeep_node::Edit;

	let personal = library.personal_space();
	let book = new_book(library, "Leviathan Wakes", rng);
	assert_eq!(
		library
			.find_entity(personal, "LEVIATHAN WAKES")
			.expect("by title"),
		book
	);
	// A name that was replaced finds the entity no more.
	library
		.edit_entity(
			personal,
			book,
			vec![Edit::Set(
				name("local.title"),
				vec![Value::Text("Caliban's War".to_owned())],
			)],
			&common::today(),
			now(),
			rng,
		)
		.expect("renames");
	assert!(matches!(
		library.find_entity(personal, "Leviathan Wakes"),
		Err(NodeError::NoEntity(_))
	));
	assert_eq!(
		library
			.find_entity(personal, "caliban's war")
			.expect("by its new title"),
		book
	);
	// A name with no words in it is found as well.
	let unnamed = new_book(library, "?!", rng);
	assert_eq!(
		library.find_entity(personal, "?!").expect("by symbols"),
		unnamed
	);
	// A person's name, written in full. The user linked to the person has the same name,
	// and is found by ID only.
	let ada = library.find_entity(personal, "ada").expect("the person");
	let view = library
		.entity(personal, ada, this_day())
		.expect("a space")
		.expect("the person");
	assert_eq!(view.name.as_deref(), Some("Ada"));
	assert_eq!(view.types, [PERSON]);
	let user = library.status().expect("reads the status").user.id;
	assert_eq!(
		library
			.find_entity(personal, &user.to_string())
			.expect("by ID"),
		user
	);
	// A deleted entity is found by name only to be shown or restored.
	library
		.edit_entity(
			personal,
			unnamed,
			vec![Edit::Delete],
			&common::today(),
			now(),
			rng,
		)
		.expect("deletes");
	assert!(matches!(
		library.find_entity(personal, "?!"),
		Err(NodeError::NoEntity(_))
	));
	assert_eq!(
		library
			.find_entity_or_deleted(personal, "?!")
			.expect("deleted"),
		unnamed
	);
}

#[test]
fn what_entities_cannot_hold_is_refused() {
	use nibkeep_model::fields;
	use nibkeep_model::schema::TYPE_DEFINITION;

	let mut rng = TestRng::new("refused");
	let mut library = with_books("refused", &mut rng);
	let personal = library.personal_space();
	let changes = library.status().expect("reads the status").spaces[0].changes;
	assert!(matches!(
		library.create_entity(personal, &[TYPE_DEFINITION], Vec::new(), now(), &mut rng),
		Err(NodeError::OwnType(_))
	));
	assert!(matches!(
		library.create_entity(
			personal,
			&[BOOK],
			vec![(fields::DELETED, Value::Boolean(true))],
			now(),
			&mut rng
		),
		Err(NodeError::OwnField(field)) if field == fields::DELETED
	));
	let title = name("local.title");
	assert!(matches!(
		library.create_entity(
			personal,
			&[BOOK],
			vec![
				(title.clone(), Value::Text("Leviathan Wakes".to_owned())),
				(title.clone(), Value::Text("Leviathan wakes".to_owned())),
			],
			now(),
			&mut rng
		),
		Err(NodeError::OneValue(field)) if field == title
	));
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes,
		"nothing was committed"
	);
	// Fields with many values take several.
	let author = name("local.author");
	let abraham = new_book(&mut library, "Daniel Abraham", &mut rng);
	let franck = new_book(&mut library, "Ty Franck", &mut rng);
	library
		.create_entity(
			personal,
			&[BOOK],
			vec![
				(author.clone(), Value::Reference(abraham)),
				(author, Value::Reference(franck)),
			],
			now(),
			&mut rng,
		)
		.expect("creates");
}

#[test]
fn a_library_needs_names() {
	let mut rng = TestRng::new("empty names");
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "empty names");
	let directory = directory.join("library");
	let protection = nibkeep_node::NewProtection::Passphrase(
		"correct horse",
		nibkeep_crypto::PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	for (user, device, app) in [
		("", "laptop", "nib"),
		("Ada", "", "nib"),
		("Ada", "laptop", ""),
	] {
		assert!(matches!(
			Library::create(
				&directory,
				&NewLibrary::new(user, device, app),
				&protection,
				now(),
				&mut rng
			),
			Err(NodeError::EmptyName)
		));
		assert!(!directory.exists(), "nothing is made");
	}
}

#[test]
fn a_name_with_parts_is_removed_by_its_full_form() {
	let mut rng = TestRng::new("names removed");
	let (_, mut library) = library("names removed", &mut rng);
	let personal = library.personal_space();
	let name = Name::new("Ursula K. Le Guin")
		.with_part("given", "Ursula")
		.with_part("family", "Le Guin");
	let author = library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(name))],
			now(),
			&mut rng,
		)
		.expect("adds");
	library
		.edit_entity(
			personal,
			author,
			vec![nibkeep_node::Edit::Remove(
				NAME,
				Value::Name(Name::new("Ursula K. Le Guin")),
			)],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("removes the name");
	assert_eq!(library.name(personal, author).expect("a space"), None);
}

#[test]
fn a_replica_is_created_with_its_key_in_a_change_of_its_own() {
	let mut rng = TestRng::new("a replica");
	let (_, mut library) = library("a replica", &mut rng);
	let personal = library.personal_space();
	let replica = library
		.create_entity(
			personal,
			&[nibkeep_governance::REPLICA],
			vec![
				(NAME, Value::Name(Name::new("tablet"))),
				(
					nibkeep_governance::SIGNING_KEY,
					Value::Text("a verifying key".to_owned()),
				),
			],
			now(),
			&mut rng,
		)
		.expect("creates the replica");
	let view = library
		.entity(personal, replica, this_day())
		.expect("a space")
		.expect("the replica");
	assert_eq!(view.name.as_deref(), Some("tablet"));
	assert!(
		view.fields
			.iter()
			.any(|field| field.field == nibkeep_governance::SIGNING_KEY)
	);
}

/// Listing reads what names and types entities at once, and must say what showing each
/// entity says: with entities merged, deleted, without a title, and named in several ways.
#[test]
fn a_list_names_and_types_entities_as_showing_them_does() {
	let mut rng = TestRng::new("listed as shown");
	let mut library = with_books("listed as shown", &mut rng);
	let personal = library.personal_space();
	let leviathan = new_book(&mut library, "Leviathan Wakes", &mut rng);
	let again = new_book(&mut library, "Leviathan Wakes, again", &mut rng);
	let caliban = new_book(&mut library, "Caliban's War", &mut rng);
	let untitled = library
		.create_entity(personal, &[BOOK], vec![], now(), &mut rng)
		.expect("creates");
	let author = library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new("Daniel Abraham")))],
			now(),
			&mut rng,
		)
		.expect("creates");
	library
		.merge_entities(personal, again, leviathan, now(), &mut rng)
		.expect("merges");
	library
		.edit_entity(
			personal,
			caliban,
			vec![nibkeep_node::Edit::Delete],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	let listed = library.entities(personal, None).expect("lists");
	let ids: Vec<EntityId> = listed.iter().map(|entity| entity.id).collect();
	for entity in [leviathan, untitled, author] {
		assert!(ids.contains(&entity), "{entity} is listed");
	}
	for entity in [again, caliban] {
		assert!(!ids.contains(&entity), "{entity} is not listed");
	}
	for entity in &listed {
		let shown = library
			.entity(personal, entity.id, this_day())
			.expect("shows")
			.expect("exists");
		assert_eq!(entity.name, shown.name, "the name of {}", entity.id);
		assert_eq!(entity.types, shown.types, "the types of {}", entity.id);
	}
	let deleted = library.deleted(personal).expect("lists the deleted");
	let deleted_ids: Vec<EntityId> = deleted.iter().map(|entity| entity.id).collect();
	assert_eq!(deleted_ids, [caliban]);
	assert_eq!(deleted[0].name.as_deref(), Some("Caliban's War"));
	assert_eq!(deleted[0].types, [BOOK]);
	// Counted without naming, as the lists list: those merged as one, the deleted apart,
	// and no definition of a type.
	let census = library.census(personal).expect("counts");
	assert_eq!(census.things, listed.len());
	assert_eq!(census.deleted, 1);
	let mut types = std::collections::BTreeMap::new();
	for entity in &listed {
		for kind in &entity.types {
			*types.entry(kind.clone()).or_insert(0) += 1;
		}
	}
	assert_eq!(census.types, types);
	assert_eq!(
		census.types.get(&BOOK),
		Some(&2),
		"the merged book and the untitled one"
	);
}

#[test]
fn a_state_mark_moves_with_every_change_and_only_then() {
	let mut rng = TestRng::new("state mark");
	let mut library = with_books("state mark", &mut rng);
	let personal = library.personal_space();
	let before = library.state_mark(personal).expect("a mark");
	assert_eq!(library.state_mark(personal).expect("a mark"), before);
	let book = new_book(&mut library, "Leviathan Wakes", &mut rng);
	let after = library.state_mark(personal).expect("a mark");
	assert_ne!(after, before, "a new entity moves it");
	let library = library.close().open().expect("opens again");
	assert_eq!(
		library.state_mark(personal).expect("a mark"),
		after,
		"opening again leaves it"
	);
	let mut library = library;
	library
		.edit_entity(
			personal,
			book,
			vec![nibkeep_node::Edit::Delete],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	assert_ne!(library.state_mark(personal).expect("a mark"), after);
	// The user and the device, as the status names them.
	let status = library.status().expect("a status");
	let (user, device) = library.user_and_device().expect("named");
	assert_eq!((user, device), (status.user, status.device));
}

#[test]
fn dated_values_are_the_current_values_with_when_they_held() {
	let mut rng = TestRng::new("dated values");
	let mut library = with_books("dated values", &mut rng);
	let personal = library.personal_space();
	let book = new_book(&mut library, "Leviathan Wakes", &mut rng);
	let title = name("local.title");
	let on = |year: &str| {
		nibkeep_node::Dating::on(this_day()).with_valid_time(year.parse().expect("a year"))
	};
	for (year, text) in [("2011", "Leviathan Wakes"), ("2012", "Caliban's War")] {
		library
			.edit_entity(
				personal,
				book,
				vec![nibkeep_node::Edit::Add(
					title.clone(),
					Value::Text(text.to_owned()),
				)],
				&on(year),
				now(),
				&mut rng,
			)
			.expect("adds");
	}
	let dated = library.dated_values(personal, book, &title).expect("reads");
	let mut held: Vec<(String, String)> = dated
		.iter()
		.map(|dated| (dated.valid_time.to_string(), format!("{:?}", dated.value)))
		.collect();
	held.sort();
	// The first title, written without a date, is no dated value.
	assert_eq!(
		held,
		[
			("2011".to_owned(), "Text(\"Leviathan Wakes\")".to_owned()),
			("2012".to_owned(), "Text(\"Caliban's War\")".to_owned()),
		]
	);
	// Read with the other fields, those without dated values left out.
	let all = library
		.all_dated_values(personal, book, |_| true)
		.expect("reads all");
	assert_eq!(
		all.iter().map(|(field, _)| field).collect::<Vec<_>>(),
		[&title]
	);
	assert_eq!(all[0].1, dated);
	// A correction sets the value shown today, the latest, which keeps its date.
	library
		.edit_entity(
			personal,
			book,
			vec![nibkeep_node::Edit::Set(
				title.clone(),
				vec![Value::Text("Caliban's War, corrected".to_owned())],
			)],
			&common::today(),
			now(),
			&mut rng,
		)
		.expect("corrects");
	let texts: Vec<String> = library
		.dated_values(personal, book, &title)
		.expect("reads")
		.iter()
		.map(|dated| format!("{} {:?}", dated.valid_time, dated.value))
		.collect();
	assert_eq!(texts.len(), 2, "{texts:?}");
	assert!(
		texts.contains(&"2012 Text(\"Caliban's War, corrected\")".to_owned()),
		"{texts:?}"
	);
}
