//! Types in a library's spaces: defined, grown, and read back.

use crate::common::library;
use nibkeep_base::SpaceId;
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, ValueKind};
use nibkeep_node::{Library, NodeError, Unlocker};
use nibkeep_testkit::{TestRng, millis_at};

const BOOK: TypeName = TypeName::from_static("local.book");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn book() -> TypeDef {
	TypeDef::new(BOOK, 1)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(
			name("local.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		)
}

#[test]
fn a_type_defined_in_a_space_lasts() {
	let mut rng = TestRng::new("types last");
	let (directory, mut library) = library("types last", &mut rng);
	let personal = library.personal_space();
	let now = millis_at("2026-10-12", 11, 0);
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	let changes = library.status().expect("reads the status").spaces[0].changes;
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes,
		"nothing to add"
	);
	let with_year = book().with_field(name("local.year"), FieldDef::new(ValueKind::Date));
	let types = library
		.define_type(personal, &with_year, now, &mut rng)
		.expect("defines");
	assert_eq!(types.get(&BOOK), Some(&with_year));
	assert_eq!(
		library.types(personal).expect("a space").get(&BOOK),
		Some(&with_year)
	);
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		9,
		"seven that found the space, and two that define the type"
	);
	drop(library);

	let library = Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	let types = library.types(personal).expect("a space");
	assert_eq!(types.get(&BOOK), Some(&with_year));
	assert!(
		types.get(&nibkeep_governance::PERSON).is_some(),
		"and NibKeep's own"
	);
}

#[test]
fn a_newer_version_with_no_new_field_counts_at_once() {
	let mut rng = TestRng::new("newer version");
	let (_, mut library) = library("newer version", &mut rng);
	let personal = library.personal_space();
	let now = millis_at("2026-10-12", 11, 0);
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	let second = TypeDef::new(BOOK, 2)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(
			name("local.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		);
	let types = library
		.define_type(personal, &second, now, &mut rng)
		.expect("defines");
	assert_eq!(types.get(&BOOK), Some(&second));
	let changes = library.status().expect("reads the status").spaces[0].changes;
	library
		.define_type(personal, &second, now, &mut rng)
		.expect("defines");
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes,
		"the version is known already"
	);
}

#[test]
fn a_new_secret_field_is_secret_at_once() {
	let mut rng = TestRng::new("secret at once");
	let (_, mut library) = library("secret at once", &mut rng);
	let personal = library.personal_space();
	let password = name("local.password");
	let login = TypeDef::new(TypeName::from_static("local.login"), 1)
		.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret());
	library
		.define_type(personal, &login, millis_at("2026-10-12", 11, 0), &mut rng)
		.expect("defines");
	assert!(
		library
			.secret_fields(personal)
			.is_some_and(|secret| secret.contains(&password))
	);
}

#[test]
fn an_unknown_space_is_refused() {
	let mut rng = TestRng::new("unknown space");
	let (_, mut library) = library("unknown space", &mut rng);
	let elsewhere = SpaceId::random(&mut rng);
	assert!(matches!(
		library.types(elsewhere),
		Err(NodeError::NoSpace(space)) if space == elsewhere
	));
	assert!(matches!(
		library.define_type(elsewhere, &book(), 0, &mut rng),
		Err(NodeError::NoSpace(_))
	));
}

#[test]
fn a_definition_that_would_change_nothing_is_refused() {
	let mut rng = TestRng::new("refused");
	let (_, mut library) = library("type refused", &mut rng);
	let personal = library.personal_space();
	let now = millis_at("2026-10-12", 11, 0);
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	let changes = library.status().expect("reads the status").spaces[0].changes;
	let numbered =
		TypeDef::new(BOOK, 1).with_field(name("local.title"), FieldDef::new(ValueKind::Integer));
	assert!(matches!(
		library.define_type(personal, &numbered, now, &mut rng),
		Err(NodeError::Definition(
			nibkeep_model::DefinitionError::OtherKind { .. }
		))
	));
	assert_eq!(
		library.status().expect("reads the status").spaces[0].changes,
		changes
	);
}

#[test]
fn a_deleted_type_leaves_the_space_and_comes_back_whole() {
	let mut rng = TestRng::new("deleted type");
	let (directory, mut library) = library("deleted type", &mut rng);
	let personal = library.personal_space();
	let now = millis_at("2026-11-22", 11, 0);
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	// Another type that lists one of the book's fields keeps it.
	let essay = TypeDef::new(TypeName::from_static("local.essay"), 1)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text));
	library
		.define_type(personal, &essay, now, &mut rng)
		.expect("defines");
	library
		.delete_type(personal, &BOOK, now, &mut rng)
		.expect("deletes");
	let types = library.types(personal).expect("a space");
	assert_eq!(types.get(&BOOK), None);
	assert!(
		types
			.get(essay.name())
			.is_some_and(|kept| kept.field(&name("local.title")).is_some())
	);
	assert_eq!(library.deleted_types(personal).expect("lists"), [BOOK]);
	drop(library);

	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(library.types(personal).expect("a space").get(&BOOK), None);
	library
		.restore_type(personal, &BOOK, now, &mut rng)
		.expect("restores");
	assert_eq!(
		library.types(personal).expect("a space").get(&BOOK),
		Some(&book()),
		"with its fields"
	);
	assert_eq!(library.deleted_types(personal).expect("lists"), []);
}

#[test]
fn a_type_in_use_or_not_the_spaces_own_is_not_deleted() {
	let mut rng = TestRng::new("type kept");
	let (_, mut library) = library("type kept", &mut rng);
	let personal = library.personal_space();
	let now = millis_at("2026-11-22", 11, 0);
	library
		.define_type(personal, &book(), now, &mut rng)
		.expect("defines");
	let dune = library
		.create_entity(personal, &[BOOK], Vec::new(), now, &mut rng)
		.expect("makes");
	assert!(matches!(
		library.delete_type(personal, &BOOK, now, &mut rng),
		Err(NodeError::TypeInUse { kind, entities: 1 }) if kind == BOOK
	));
	library
		.edit_entity(
			personal,
			dune,
			vec![nibkeep_node::Edit::Delete],
			&nibkeep_node::Dating::on(nibkeep_time::Day::from_ymd(2026, 11, 22).expect("a day")),
			now,
			&mut rng,
		)
		.expect("deletes");
	library
		.delete_type(personal, &BOOK, now, &mut rng)
		.expect("deletes once no entity has it");
	// A standard type is the vocabulary's, and NibKeep's own are defined in code.
	for kind in [
		TypeName::from_static("std.book"),
		nibkeep_governance::PERSON,
	] {
		assert!(matches!(
			library.delete_type(personal, &kind, now, &mut rng),
			Err(NodeError::NotOwnType(refused)) if refused == kind
		));
	}
	// One that no definition of the space makes.
	let novel = TypeName::from_static("local.novel");
	assert!(matches!(
		library.delete_type(personal, &novel, now, &mut rng),
		Err(NodeError::NoTypeDefined(missing)) if missing == novel
	));
	assert!(matches!(
		library.restore_type(personal, &novel, now, &mut rng),
		Err(NodeError::NoTypeDefined(missing)) if missing == novel
	));
}
