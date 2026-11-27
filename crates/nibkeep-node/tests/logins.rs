//! Logins and folders, as a password manager keeps them: a login is found by its name,
//! sites and user name without the secrets, and what signs in stays sealed (S11).

use crate::common::{library, now, this_day};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{Value, fields};
use nibkeep_node::{Library, NewProtection, Unlocker};
use nibkeep_std::{FOLDER, IN_FOLDER, LOGIN, PASSWORD, SECRET_NOTES, USERNAME, WEBSITE};
use nibkeep_testkit::TestRng;

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

fn name(text: &str) -> Value {
	Value::Name(nibkeep_model::Name::new(text))
}

const SECRETS: Unlocker<'static> = Unlocker::Passphrase("tr0ub4dor");

/// The texts of a field that an entity shows.
fn shown(
	library: &Library,
	entity: nibkeep_base::EntityId,
	field: &nibkeep_model::FieldName,
) -> Vec<Value> {
	let view = library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity");
	view.fields
		.iter()
		.filter(|shown| shown.field == *field)
		.flat_map(|shown| shown.values.iter().map(|value| value.value.clone()))
		.collect()
}

#[test]
fn a_new_library_knows_logins_and_folders_and_their_secrets() {
	let mut rng = TestRng::new("logins known");
	let (_, mut library) = library("logins known", &mut rng);
	let personal = library.personal_space();
	let types = library.types(personal).expect("a space");
	assert!(types.get(&LOGIN).is_some());
	assert!(types.get(&FOLDER).is_some());
	let secret = library.secret_fields(personal).expect("a space");
	for field in [PASSWORD, SECRET_NOTES] {
		assert!(secret.contains(&field), "{field}");
	}
	assert!(!secret.contains(&WEBSITE));
	// The vocabulary is there already, so defining it again adds nothing.
	let before = library.status().expect("reads");
	for definition in nibkeep_std::types() {
		library
			.define_type(personal, &definition, now(), &mut rng)
			.expect("defines");
	}
	assert_eq!(library.status().expect("reads"), before);
}

#[test]
fn a_login_is_found_without_the_secrets_which_open_its_password() {
	let mut rng = TestRng::new("is there an account");
	let (directory, mut library) = library("is there an account", &mut rng);
	let personal = library.personal_space();
	library
		.set_up_secrets(
			&NewProtection::Passphrase(
				"tr0ub4dor",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 500,
			&mut rng,
		)
		.expect("sets up");
	let folder = library
		.create_entity(
			personal,
			&[FOLDER],
			vec![(fields::NAME, name("Streaming"))],
			now(),
			&mut rng,
		)
		.expect("creates");
	let login = library
		.create_entity(
			personal,
			&[LOGIN],
			vec![
				(fields::NAME, name("Films at home")),
				(WEBSITE, text("https://www.example-tv.com/sign-in")),
				(USERNAME, text("someone@example.com")),
				(PASSWORD, text("hunter2-s3ntinel")),
				(SECRET_NOTES, text("first pet: s3ntinel-rex")),
				(IN_FOLDER, Value::Reference(folder)),
			],
			now(),
			&mut rng,
		)
		.expect("creates");
	library.lock_secrets();
	drop(library);
	for sentinel in [&b"hunter2-s3ntinel"[..], b"s3ntinel-rex"] {
		assert!(
			!nibkeep_testkit::holds_anywhere(&directory, sentinel),
			"stored sealed"
		);
	}

	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	let found = |library: &Library, query: &str| -> Vec<nibkeep_base::EntityId> {
		library
			.search(personal, query)
			.expect("searches")
			.into_iter()
			.map(|listed| listed.id)
			.collect()
	};
	// By its name, its site, and its user name.
	for query in ["films", "example-tv", "someone@example.com"] {
		assert_eq!(found(&library, query), [login], "{query}");
	}
	// By its folder, after the folder itself.
	assert_eq!(found(&library, "streaming"), [folder, login]);
	// Never by what is secret.
	assert_eq!(found(&library, "hunter2"), []);
	assert_eq!(shown(&library, login, &PASSWORD), []);
	assert_eq!(shown(&library, login, &SECRET_NOTES), []);
	assert_eq!(
		shown(&library, login, &USERNAME),
		[text("someone@example.com")]
	);
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(
		shown(&library, login, &PASSWORD),
		[text("hunter2-s3ntinel")]
	);
}
