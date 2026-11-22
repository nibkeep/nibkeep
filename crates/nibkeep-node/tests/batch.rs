//! Changes saved in batches ([technical design: saving in batches]): a batch saves its changes when its
//! closure returns, keeps those that were not refused whether the closure succeeds or
//! fails, and saves nothing if the closure panics.
//!
//! [technical design: saving in batches]: ../../../docs/Technical-design.md#77-saving-in-batches

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

mod common;

use common::{library, now, this_day, today};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, Library, NodeError, Unlocker};
use nibkeep_store::{StoreError, log_path};
use nibkeep_testkit::TestRng;

fn person(
	library: &mut Library,
	space: SpaceId,
	name: &str,
	rng: &mut TestRng,
) -> Result<EntityId, NodeError> {
	library.create_entity(
		space,
		&[PERSON],
		vec![(NAME, Value::Name(Name::new(name)))],
		now(),
		rng,
	)
}

/// The names of the people of the personal space, sorted.
fn people(library: &Library) -> Vec<String> {
	let mut names: Vec<String> = library
		.entities(library.personal_space(), Some(&PERSON))
		.expect("lists")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

fn changes(library: &Library) -> usize {
	library.status().expect("reads the status").spaces[0].changes
}

fn reopen(directory: &Path, library: Library) -> Library {
	drop(library);
	Library::open(directory, &Unlocker::Passphrase("correct horse")).expect("opens")
}

#[test]
fn a_batch_saves_its_changes_when_it_ends() {
	let mut rng = TestRng::new("batch saves");
	let (directory, mut library) = library("batch saves", &mut rng);
	let personal = library.personal_space();
	let (before, people_before) = (changes(&library), people(&library));
	let made = library
		.batch(personal, |library| {
			let first = person(library, personal, "Ann", &mut rng)?;
			person(library, personal, "Bob", &mut rng)?;
			library.edit_entity(
				personal,
				first,
				vec![Edit::Set(NAME, vec![Value::Name(Name::new("Annie"))])],
				&today(),
				now(),
				&mut rng,
			)?;
			// What the batch made is read before it ends.
			assert_eq!(changes(library), before + 3);
			Ok(first)
		})
		.expect("saves the batch");
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before + 3);
	let mut expected = people_before;
	// Ann shows the name she was given last, which replaced her first.
	expected.extend(["Annie".to_owned(), "Bob".to_owned()]);
	expected.sort();
	assert_eq!(people(&library), expected);
	assert!(
		library
			.entity(personal, made, this_day())
			.expect("shows")
			.is_some()
	);
}

#[test]
fn what_a_failing_closure_made_is_saved() {
	let mut rng = TestRng::new("batch fails");
	let (directory, mut library) = library("batch fails", &mut rng);
	let personal = library.personal_space();
	let before = changes(&library);
	let result: Result<(), NodeError> = library.batch(personal, |library| {
		person(library, personal, "Ann", &mut rng)?;
		person(library, personal, "Bob", &mut rng)?;
		Err(NodeError::EmptyName)
	});
	assert!(matches!(result, Err(NodeError::EmptyName)), "{result:?}");
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before + 2);
}

#[test]
fn a_refused_change_leaves_the_others_of_its_batch() {
	let mut rng = TestRng::new("batch refused");
	let (directory, mut library) = library("batch refused", &mut rng);
	let personal = library.personal_space();
	let before = changes(&library);
	library
		.batch(personal, |library| {
			person(library, personal, "Ann", &mut rng)?;
			// A field of NibKeep's own, which only its own operations set.
			let own = library.create_entity(
				personal,
				&[PERSON],
				vec![(fields::DELETED, Value::Boolean(true))],
				now(),
				&mut rng,
			);
			assert!(matches!(own, Err(NodeError::OwnField(_))), "{own:?}");
			let missing = library.edit_entity(
				personal,
				rng.entity_id(),
				vec![Edit::Add(NAME, Value::Name(Name::new("Nobody")))],
				&today(),
				now(),
				&mut rng,
			);
			assert!(
				matches!(missing, Err(NodeError::NoEntity(_))),
				"{missing:?}"
			);
			person(library, personal, "Bob", &mut rng)?;
			Ok(())
		})
		.expect("saves the batch");
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before + 2);
}

#[test]
fn a_panic_in_a_batch_saves_none_of_it() {
	let mut rng = TestRng::new("batch panics");
	let (directory, mut library) = library("batch panics", &mut rng);
	let personal = library.personal_space();
	let (before, people_before) = (changes(&library), people(&library));
	let panicked = catch_unwind(AssertUnwindSafe(|| {
		library.batch(personal, |library| -> Result<(), NodeError> {
			person(library, personal, "Ann", &mut rng)?;
			panic!("a bug in the closure");
		})
	}));
	assert!(panicked.is_err());
	assert_eq!(changes(&library), before, "none of the batch is kept");
	assert_eq!(people(&library), people_before);
	// The library goes on, saving one change at a time.
	person(&mut library, personal, "Bob", &mut rng).expect("adds");
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before + 1);
	assert!(!people(&library).contains(&"Ann".to_owned()));
}

#[test]
fn types_defined_in_a_batch_hold_in_it() {
	let mut rng = TestRng::new("batch types");
	let (directory, mut library) = library("batch types", &mut rng);
	let personal = library.personal_space();
	let shelf: FieldName = "local.shelf".parse().expect("a field name");
	let book = TypeDef::new(TypeName::from_static("local.book"), 1).with_field(
		shelf.clone(),
		FieldDef::new(ValueKind::Text).last_writer_wins(),
	);
	library
		.batch(personal, |library| {
			library.define_type(personal, &book, now(), &mut rng)?;
			library.create_entity(
				personal,
				&[book.name().clone()],
				vec![(shelf.clone(), Value::Text("top".to_owned()))],
				now(),
				&mut rng,
			)?;
			Ok(())
		})
		.expect("saves the batch");
	let library = reopen(&directory, library);
	let types = library.types(personal).expect("reads the types");
	assert!(types.get(book.name()).is_some());
	assert!(types.last_writer_wins_fields().contains(&shelf));
	assert_eq!(library.search(personal, "top").expect("searches").len(), 1);
}

#[test]
fn what_writes_outside_a_change_is_refused_in_a_batch() {
	let mut rng = TestRng::new("batch refuses");
	let (directory, mut library) = library("batch refuses", &mut rng);
	let personal = library.personal_space();
	let before = changes(&library);
	library
		.batch(personal, |library| {
			let blob = library.import_blob(personal, &b"a photo"[..], now(), &mut rng);
			assert!(
				matches!(blob, Err(NodeError::Store(StoreError::InBatch))),
				"{blob:?}"
			);
			let nested = library.batch(personal, |_| Ok(()));
			assert!(
				matches!(nested, Err(NodeError::Store(StoreError::InBatch))),
				"{nested:?}"
			);
			let verified = library.verify(now());
			assert!(
				matches!(verified, Err(NodeError::Store(StoreError::InBatch))),
				"{verified:?}"
			);
			person(library, personal, "Ann", &mut rng)?;
			Ok(())
		})
		.expect("saves the batch");
	library
		.import_blob(personal, &b"a photo"[..], now(), &mut rng)
		.expect("imports after the batch");
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before + 1);
}

/// Changes every byte of a space's state file after its first page, which opening reads:
/// the other pages no longer authenticate when they are read.
fn damage_state_file(directory: &Path, space: SpaceId) {
	let path = log_path(&common::replica_folder(directory), space).with_file_name("state.sqlite");
	let mut bytes = fs::read(&path).expect("reads the state file");
	for byte in &mut bytes[8_192..] {
		*byte ^= 0x40;
	}
	fs::write(&path, bytes).expect("writes the state file");
}

#[test]
fn a_batch_that_the_files_fail_leaves_nothing_of_it() {
	let mut rng = TestRng::new("batch lost");
	let (directory, mut library) = library("batch lost", &mut rng);
	let personal = library.personal_space();
	for number in 0..40 {
		person(
			&mut library,
			personal,
			&format!("person {number}"),
			&mut rng,
		)
		.expect("adds");
	}
	let mut library = reopen(&directory, library);
	let (before, people_before) = (changes(&library), people(&library));
	let book = TypeDef::new(TypeName::from_static("local.book"), 1).with_field(
		"local.title".parse().expect("a field name"),
		FieldDef::new(ValueKind::Text),
	);
	let result = library.batch(personal, |library| {
		library.define_type(personal, &book, now(), &mut rng)?;
		// Damage found while the batch is open, as a disk may damage what it holds.
		damage_state_file(&directory, personal);
		for number in 0..40 {
			person(
				library,
				personal,
				&format!("in the batch {number}"),
				&mut rng,
			)?;
		}
		Ok(())
	});
	assert!(
		matches!(result, Err(NodeError::Store(StoreError::State(_)))),
		"{result:?}"
	);
	// The type that the batch defined went with it.
	assert!(
		library
			.types(personal)
			.expect("reads")
			.get(book.name())
			.is_none()
	);
	assert_eq!(changes(&library), before);
	assert_eq!(people(&library), people_before);
	let library = reopen(&directory, library);
	assert_eq!(changes(&library), before);
	assert!(
		library
			.types(personal)
			.expect("reads")
			.get(book.name())
			.is_none()
	);
}
