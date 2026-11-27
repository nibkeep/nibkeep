//! Purging values for good through the library.

use crate::common;

use crate::common::{library, now, this_day, today};
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Edit, Library, NodeError, Unlocker};
use nibkeep_testkit::TestRng;

const BOOK: TypeName = TypeName::from_static("local.book");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// The values of a field that an entity shows today.
fn shown(library: &Library, entity: nibkeep_base::EntityId, field: &str) -> Vec<Value> {
	let view = library
		.entity(library.personal_space(), entity, this_day())
		.expect("a space")
		.expect("the entity");
	view.fields
		.iter()
		.filter(|shown| shown.field == name(field))
		.flat_map(|shown| shown.values.iter().map(|value| value.value.clone()))
		.collect()
}

#[test]
fn a_value_and_its_past_are_purged_and_the_rest_stays() {
	let mut rng = TestRng::new("purge a value");
	let (directory, mut library) = library("purge a value", &mut rng);
	let personal = library.personal_space();
	let book = TypeDef::new(BOOK, 1)
		.with_field(name("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(name("local.note"), FieldDef::new(ValueKind::Text));
	library
		.define_type(personal, &book, now(), &mut rng)
		.expect("defines");
	let entity = library
		.create_entity(
			personal,
			&[BOOK],
			vec![
				(name("local.title"), text("Leviathan wakes")),
				(name("local.note"), text("lent to Grace")),
			],
			now() + 1_000,
			&mut rng,
		)
		.expect("creates");
	library
		.edit_entity(
			personal,
			entity,
			vec![Edit::Set(
				name("local.title"),
				vec![text("Leviathan Wakes")],
			)],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("edits");

	let misspelt = Some(text("Leviathan wakes"));
	let purge = library
		.purgeable(personal, entity, &name("local.title"), misspelt.as_ref())
		.expect("finds it");
	assert_eq!(
		(purge.statements.len(), purge.changes, purge.secret),
		(1, 1, 0)
	);
	library
		.purge(
			personal,
			entity,
			&name("local.title"),
			misspelt.as_ref(),
			now() + 3_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(
		shown(&library, entity, "local.title"),
		[text("Leviathan Wakes")]
	);
	assert_eq!(
		shown(&library, entity, "local.note"),
		[text("lent to Grace")]
	);
	assert!(matches!(
		library.purgeable(personal, entity, &name("local.title"), misspelt.as_ref()),
		Err(NodeError::NoValue(_))
	));
	drop(library);

	let mut reopened =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(
		shown(&reopened, entity, "local.title"),
		[text("Leviathan Wakes")]
	);
	assert_eq!(
		shown(&reopened, entity, "local.note"),
		[text("lent to Grace")]
	);
	assert!(reopened.verify(now() + 4_000).expect("verifies").is_sound());
}

#[test]
fn a_replica_key_cannot_be_purged() {
	let mut rng = TestRng::new("purge a replica key");
	let (_, mut library) = library("purge a replica key", &mut rng);
	let personal = library.personal_space();
	let replica = library.status().expect("a status").replica.id;
	// Saying what it would purge already refuses it.
	assert!(matches!(
		library.purgeable(personal, replica, &nibkeep_governance::SIGNING_KEY, None),
		Err(NodeError::Log(nibkeep_log::LogError::CannotPurge))
	));
	assert!(matches!(
		library.purge(
			personal,
			replica,
			&nibkeep_governance::SIGNING_KEY,
			None,
			now() + 1_000,
			&mut rng
		),
		Err(NodeError::Log(nibkeep_log::LogError::CannotPurge))
	));
}

#[test]
fn secret_values_need_the_secrets_unlocked_to_be_purged() {
	let mut rng = TestRng::new("purge a secret");
	let (_, mut library) = library("purge a secret", &mut rng);
	let personal = library.personal_space();
	let login = TypeDef::new(TypeName::from_static("local.login"), 1)
		.with_field(name("local.site"), FieldDef::new(ValueKind::Text))
		.with_field(
			name("local.password"),
			FieldDef::new(ValueKind::Text).secret(),
		);
	library
		.define_type(personal, &login, now(), &mut rng)
		.expect("defines");
	let entity = library
		.create_entity(
			personal,
			&[TypeName::from_static("local.login")],
			vec![(name("local.site"), text("library.example"))],
			now() + 1_000,
			&mut rng,
		)
		.expect("creates");
	// A space without secret values has nothing that only unlocking shows.
	let password = name("local.password");
	assert!(matches!(
		library.purgeable(personal, entity, &password, None),
		Err(NodeError::NoValue(_))
	));
	library
		.set_up_secrets(
			&nibkeep_node::NewProtection::Passphrase(
				"tr0ub4dor",
				nibkeep_crypto::PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 500,
			&mut rng,
		)
		.expect("sets up");
	library
		.edit_entity(
			personal,
			entity,
			vec![nibkeep_node::Edit::Set(
				password.clone(),
				vec![text("hunter2")],
			)],
			&common::today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("sets");
	library.lock_secrets();
	assert!(matches!(
		library.purgeable(personal, entity, &password, None),
		Err(NodeError::SecretsLocked)
	));
	library
		.unlock_secrets(&nibkeep_node::Unlocker::Passphrase("tr0ub4dor"))
		.expect("unlocks");
	let purge = library
		.purgeable(personal, entity, &password, None)
		.expect("shows what it would purge");
	assert_eq!((purge.statements.len(), purge.secret), (1, 1));
}
