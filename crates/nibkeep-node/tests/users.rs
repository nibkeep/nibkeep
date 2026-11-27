//! Who made a change: the replica that signed it, the user it belongs to, and the person
//! linked to the user in the space.

use crate::common;

use crate::common::{library, now, this_day, today};
use nibkeep_base::{EntityId, StatementId};
use nibkeep_crypto::SigningKey;
use nibkeep_governance::{LINKED_PERSON, NAME};
use nibkeep_keys::KeyFile;
use nibkeep_log::SpaceKeys;
use nibkeep_model::{Name, Operation, Statement, TypeName, Value};
use nibkeep_node::{Edit, Library, Unlocker};
use nibkeep_store::{StoredLog, log_path};
use nibkeep_testkit::TestRng;

/// The statement that holds an entity's name.
fn name_statement(library: &Library, entity: EntityId) -> StatementId {
	let view = library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("an entity");
	view.fields
		.iter()
		.find(|field| field.field == NAME)
		.expect("a name")
		.values[0]
		.statement
}

/// The person that the library's user is linked to.
fn linked_person(library: &Library, user: EntityId) -> EntityId {
	let view = library
		.entity(library.personal_space(), user, this_day())
		.expect("reads")
		.expect("the user");
	view.fields
		.iter()
		.find(|field| field.field == LINKED_PERSON)
		.and_then(|field| field.values[0].value.as_reference())
		.expect("a linked person")
}

/// The name that history gives the author of a statement, which the library's status
/// gives its user too, and the person linked to the user.
fn named_as(library: &Library, statement: StatementId) -> (String, Option<EntityId>) {
	let author = library
		.author_of(library.personal_space(), statement)
		.expect("reads")
		.expect("an author");
	let name = author.user.name.expect("a name");
	let status = library.status().expect("reads the status");
	assert_eq!(status.user.name.as_deref(), Some(name.as_str()));
	(name, author.person)
}

#[test]
fn a_change_is_made_by_the_person_linked_to_its_user() {
	let mut rng = TestRng::new("authors");
	let (_directory, mut library) = library("authors", &mut rng);
	let personal = library.personal_space();
	let status = library.status().expect("reads the status");
	let person = linked_person(&library, status.user.id);
	let book = library
		.create_entity(
			personal,
			&[TypeName::from_static("local.book")],
			vec![(NAME, Value::Name(Name::new("Caliban's War")))],
			now(),
			&mut rng,
		)
		.expect("creates");
	let statement = name_statement(&library, book);

	let author = library
		.author_of(personal, statement)
		.expect("reads")
		.expect("an author");
	assert_eq!(author.user.id, status.user.id);
	assert_eq!(author.replica, status.replica.id);
	assert_eq!(
		named_as(&library, statement),
		("Ada".to_owned(), Some(person))
	);

	// The person's name is the one shown, unless the person is deleted, or the user is no
	// longer linked to the person.
	let edits = [
		(
			person,
			Edit::Set(NAME, vec![Value::Name(Name::new("Ada Lovelace"))]),
		),
		(person, Edit::Delete),
		(person, Edit::Restore),
		(status.user.id, Edit::Set(LINKED_PERSON, Vec::new())),
	];
	let shown = [
		("Ada Lovelace", Some(person)),
		("Ada", None),
		("Ada Lovelace", Some(person)),
		("Ada", None),
	];
	for (millis, ((entity, edit), (name, linked))) in (1..).zip(edits.into_iter().zip(shown)) {
		library
			.edit_entity(
				personal,
				entity,
				vec![edit],
				&today(),
				now() + millis * 1_000,
				&mut rng,
			)
			.expect("edits");
		assert_eq!(named_as(&library, statement), (name.to_owned(), linked));
	}

	// A statement the space does not hold has no author.
	assert!(
		library
			.author_of(personal, StatementId::from_bytes([7; 16]))
			.expect("reads")
			.is_none()
	);
}

#[test]
fn a_change_signed_by_a_key_of_no_replica_has_no_author() {
	let mut rng = TestRng::new("unknown authors");
	let (directory, library) = library("unknown authors", &mut rng);
	let personal = library.personal_space();
	drop(library);

	let key_file = KeyFile::read(&common::replica_folder(&directory).join("keys.cbor"))
		.expect("reads the keys");
	let compartment = key_file
		.unlock_replica(&Unlocker::Passphrase("correct horse"))
		.expect("unlocks");
	let replica = key_file.replica(&compartment).expect("opens");
	let keys = SpaceKeys::new(0, replica.spaces[&personal][&0].clone());
	let book = EntityId::random(&mut rng);
	let statement = StatementId::random(&mut rng);
	{
		let mut stored = StoredLog::open(
			&log_path(&common::replica_folder(&directory), personal),
			personal,
			&keys,
			&replica.storage,
			600_000,
			[],
		)
		.expect("opens the space");
		let stranger = SigningKey::generate(&mut rng);
		let operations = vec![Operation::assert(Statement::new(
			statement,
			book,
			NAME,
			Value::Name(Name::new("Leviathan Wakes")),
		))];
		stored
			.commit(&stranger, &keys, operations, now() + 1_000, &mut rng)
			.expect("commits");
	}

	let library = Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert!(
		library
			.author_of(personal, statement)
			.expect("reads")
			.is_none()
	);
}
