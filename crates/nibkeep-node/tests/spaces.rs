//! Spaces of one's own besides the personal one: the user makes one, it reaches every
//! replica of theirs but AI agents', also one paired later, new entities go to the space
//! chosen, and listing and search cover every space ([technical design: sharing]); and a replica
//! keeps one sealed copy of content that several of its spaces hold ([technical design: spaces and moves]).
//!
//! [technical design: sharing]: ../../../docs/Technical-design.md#13-sharing
//! [technical design: spaces and moves]: ../../../docs/Technical-design.md#131-spaces-and-moves

mod common;

use std::path::PathBuf;

use common::{library, now, pair_ai_agent_named, pair_named, sync_in_memory};
use nibkeep_model::{TypeName, Value};
use nibkeep_node::{Library, PairingCode, Unlocker};
use nibkeep_testkit::TestRng;

/// A new folder's place, where nothing is yet.
fn new_folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	std::fs::remove_dir(&made).expect("removes");
	made
}

/// A replica of `sponsor`'s library, paired on a device named `device`.
fn paired(sponsor: &mut Library, device: &str, rng: &mut TestRng) -> Library {
	let code = PairingCode::generate(rng);
	let directory = new_folder(device);
	let (paired, joined) = pair_named(sponsor, &directory, device, (&code, &code), rng);
	paired.expect("pairs");
	joined.expect("joins").library
}

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// The names of what a search finds in a space.
fn found(library: &Library, space: nibkeep_base::SpaceId, words: &str) -> Vec<String> {
	let mut names: Vec<String> = library
		.search(space, words)
		.expect("searches")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

fn note(
	library: &mut Library,
	space: nibkeep_base::SpaceId,
	name: &str,
	at: u64,
	rng: &mut TestRng,
) {
	library
		.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(
				"org.nibkeep.name".parse().expect("a field"),
				Value::Name(nibkeep_model::Name::new(name)),
			)],
			at,
			rng,
		)
		.expect("adds");
}

#[test]
fn a_space_a_user_makes_reaches_each_replica_of_theirs_also_one_paired_later() {
	let mut rng = TestRng::new("spaces reach every replica");
	let (_, mut laptop) = library("spaces, laptop", &mut rng);
	let mut desk = paired(&mut laptop, "desk", &mut rng);
	let mut at = sync_rounds(&mut laptop, &mut desk, now() + 100_000, 1);

	let household = laptop
		.create_space("Household", at, &mut rng)
		.expect("makes the space");
	assert_eq!(laptop.spaces(), vec![laptop.personal_space(), household]);
	assert_eq!(
		laptop.space_name(household).expect("names").as_deref(),
		Some("Household")
	);
	note(&mut laptop, household, "Boiler service", at + 500, &mut rng);

	// The desk learns of the space through the personal space, asks to be listed, is listed,
	// and then syncs it.
	at = sync_rounds(&mut laptop, &mut desk, at, 4);
	assert_eq!(desk.spaces(), vec![desk.personal_space(), household]);
	assert_eq!(
		desk.space_name(household).expect("names").as_deref(),
		Some("Household")
	);
	assert_eq!(found(&desk, household, "boiler"), ["Boiler service"]);
	// What the desk adds there reaches the laptop.
	note(&mut desk, household, "Window cleaner", at + 500, &mut rng);
	at = sync_rounds(&mut desk, &mut laptop, at + 1_000, 1);
	assert_eq!(found(&laptop, household, "window"), ["Window cleaner"]);

	// A phone paired later, from the desk, gets the space too.
	let mut phone = paired(&mut desk, "phone", &mut rng);
	at = sync_rounds(&mut desk, &mut phone, at + 70_000, 4);
	assert_eq!(phone.spaces(), vec![phone.personal_space(), household]);
	assert_eq!(found(&phone, household, "boiler"), ["Boiler service"]);
	note(&mut phone, household, "Gutters", at + 500, &mut rng);
	at = sync_rounds(&mut phone, &mut desk, at + 1_000, 1);
	sync_rounds(&mut desk, &mut laptop, at, 1);
	assert_eq!(found(&laptop, household, "gutters"), ["Gutters"]);
}

#[test]
fn a_space_that_another_program_makes_is_opened_by_the_one_that_syncs_and_reaches_the_others() {
	let mut rng = TestRng::new("spaces, another program");
	let (directory, mut laptop) = library("spaces, another program, laptop", &mut rng);
	let mut desk = paired(&mut laptop, "desk of two programs", &mut rng);
	let mut at = sync_rounds(&mut laptop, &mut desk, now() + 100_000, 1);
	// A program that keeps the library open, as the holder of the sync role does, opens it
	// anew: the changes that a program makes in a burst hold the state file's write lock
	// until it uses the library again, or closes it (docs/Known-limits.md).
	drop(laptop);
	let open = || Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	let mut laptop = open();

	// Another program of the replica, as nib is while an app or nibkeep-sync keeps the library
	// open, makes a space and adds to it, and ends.
	let mut other = open();
	let household = other
		.create_space("Household", at, &mut rng)
		.expect("makes the space");
	note(&mut other, household, "Boiler service", at + 500, &mut rng);
	drop(other);

	// The program that keeps the library open opens the space as it catches up, and writes
	// there too.
	assert!(laptop.catch_up().expect("catches up"));
	assert_eq!(laptop.spaces(), vec![laptop.personal_space(), household]);
	assert_eq!(
		laptop.space_name(household).expect("names").as_deref(),
		Some("Household")
	);
	assert_eq!(found(&laptop, household, "boiler"), ["Boiler service"]);
	note(&mut laptop, household, "Window cleaner", at + 600, &mut rng);

	// It syncs the space with the desk, which writes there too.
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 4);
	assert_eq!(desk.spaces(), vec![desk.personal_space(), household]);
	assert_eq!(found(&desk, household, "boiler"), ["Boiler service"]);
	assert_eq!(found(&desk, household, "window"), ["Window cleaner"]);
	note(&mut desk, household, "Gutters", at + 500, &mut rng);
	sync_rounds(&mut desk, &mut laptop, at + 1_000, 1);

	// Another program sees what came, and every program finds the replica sound.
	let mut other = open();
	assert_eq!(found(&other, household, "gutters"), ["Gutters"]);
	for library in [&mut laptop, &mut other, &mut desk] {
		let report = library.verify(at + 10_000).expect("verifies");
		for space in &report.spaces {
			assert!(space.problems.is_empty(), "{report:?}");
		}
	}
	drop((other, laptop));
	let reopened = open();
	assert_eq!(
		reopened.spaces(),
		vec![reopened.personal_space(), household]
	);
	assert_eq!(found(&reopened, household, "window"), ["Window cleaner"]);
}

#[test]
fn an_ai_agents_replica_gets_no_space_it_was_not_given() {
	let mut rng = TestRng::new("spaces and AI agents");
	let (_, mut laptop) = library("spaces, AI agent's laptop", &mut rng);
	let code = PairingCode::generate(&mut rng);
	let directory = new_folder("spaces, the AI agent");
	let (paired, joined) = pair_ai_agent_named(
		&mut laptop,
		&directory,
		("laptop", "Claude"),
		&code,
		&mut rng,
	);
	paired.expect("pairs");
	let mut agent = joined.expect("joins").library;
	let household = laptop
		.create_space("Household", now() + 100_000, &mut rng)
		.expect("makes the space");
	sync_rounds(&mut laptop, &mut agent, now() + 101_000, 4);
	assert_eq!(agent.spaces(), vec![agent.personal_space()]);
	assert!(laptop.spaces().contains(&household));
}

#[test]
fn new_entities_go_to_the_space_chosen_and_are_found_in_any_space() {
	let mut rng = TestRng::new("entities in spaces");
	let (_, mut laptop) = library("spaces, entities", &mut rng);
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	note(&mut laptop, personal, "Diary", now() + 2_000, &mut rng);
	note(
		&mut laptop,
		household,
		"Boiler service",
		now() + 3_000,
		&mut rng,
	);
	assert_eq!(found(&laptop, personal, "boiler"), Vec::<String>::new());
	assert_eq!(found(&laptop, household, "boiler"), ["Boiler service"]);
	assert_eq!(found(&laptop, household, "diary"), Vec::<String>::new());

	// Search across every space tells where each entity is.
	let everywhere = laptop.search_everywhere("boiler").expect("searches");
	assert_eq!(everywhere.len(), 1);
	assert_eq!(everywhere[0].0, household);
	// An entity is found by its name in whichever space holds it.
	let (space, boiler) = laptop.locate("Boiler service").expect("finds");
	assert_eq!(space, household);
	assert_eq!(
		laptop.name(household, boiler).expect("names").as_deref(),
		Some("Boiler service")
	);
	assert_eq!(laptop.locate("Diary").expect("finds").0, personal);

	// Spaces are found by name, and renamed.
	assert_eq!(laptop.find_space("household").expect("finds"), household);
	assert_eq!(laptop.find_space("Personal").expect("finds"), personal);
	laptop
		.rename_space(household, "Home", now() + 4_000, &mut rng)
		.expect("renames");
	assert_eq!(
		laptop.space_name(household).expect("names").as_deref(),
		Some("Home")
	);
	assert!(laptop.find_space("Household").is_err());
	assert!(
		laptop
			.rename_space(personal, "Mine", now() + 5_000, &mut rng)
			.is_err()
	);
	// The status lists both spaces.
	let status = laptop.status().expect("tells");
	let names: Vec<_> = status
		.spaces
		.iter()
		.map(|space| space.name.clone())
		.collect();
	assert_eq!(
		names,
		[Some("Personal".to_owned()), Some("Home".to_owned())]
	);
}

#[test]
fn a_space_survives_reopening_the_library() {
	let mut rng = TestRng::new("spaces reopened");
	let (directory, mut laptop) = library("spaces, reopened", &mut rng);
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	note(
		&mut laptop,
		household,
		"Boiler service",
		now() + 2_000,
		&mut rng,
	);
	drop(laptop);
	let mut laptop =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(laptop.spaces(), vec![laptop.personal_space(), household]);
	assert_eq!(found(&laptop, household, "boiler"), ["Boiler service"]);
	let report = laptop.verify(now() + 3_000).expect("verifies");
	assert!(
		report.spaces.iter().all(|space| space.problems.is_empty()),
		"{report:?}"
	);
}

#[test]
fn a_revoked_device_is_revoked_in_every_space_with_a_new_key() {
	let mut rng = TestRng::new("spaces and revocation");
	let (_, mut laptop) = library("spaces, revoking laptop", &mut rng);
	let mut desk = paired(&mut laptop, "revoked desk", &mut rng);
	let household = laptop
		.create_space("Household", now() + 100_000, &mut rng)
		.expect("makes the space");
	let mut at = sync_rounds(&mut laptop, &mut desk, now() + 101_000, 4);
	assert_eq!(found(&desk, household, "household"), ["Household"]);
	let (numbers, _) = laptop.space_key_numbers(household).expect("reads");
	assert_eq!(numbers, [0]);
	let desk_replica = desk.status().expect("tells").replica.id;
	at += 1_000;
	laptop
		.revoke(&[desk_replica], at, &mut rng)
		.expect("revokes the desk");
	// Household has a new key, which the desk does not get, and nothing it writes there
	// reaches the laptop.
	let (numbers, _) = laptop.space_key_numbers(household).expect("reads");
	assert_eq!(numbers.len(), 2, "{numbers:?}");
	note(
		&mut desk,
		household,
		"From the lost desk",
		at + 500,
		&mut rng,
	);
	let (first, _) = sync_in_memory(&mut desk, &mut laptop, at + 1_000, (0, 1));
	drop(first);
	assert_eq!(
		found(&laptop, household, "lost"),
		Vec::<String>::new(),
		"nothing the revoked desk wrote counts"
	);
}

#[test]
fn a_space_besides_the_personal_one_holds_secret_values_once_the_secrets_are_set_up() {
	let mut rng = TestRng::new("spaces without secrets");
	let (_, mut laptop) = library("spaces, no secrets", &mut rng);
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let written = laptop.create_entity(
		household,
		&[nibkeep_std::LOGIN],
		vec![(nibkeep_std::PASSWORD, Value::Text("hunter2".to_owned()))],
		now() + 2_000,
		&mut rng,
	);
	assert!(
		matches!(written, Err(nibkeep_node::NodeError::SetUpSecretsFirst)),
		"{written:?}"
	);
	// Once they are, the first secret value written there sets up the space's own.
	laptop
		.set_up_secrets(
			&nibkeep_node::NewProtection::Passphrase(
				"tr0ub4dor",
				nibkeep_crypto::PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 3_000,
			&mut rng,
		)
		.expect("sets up");
	laptop
		.create_entity(
			household,
			&[nibkeep_std::LOGIN],
			vec![(nibkeep_std::PASSWORD, Value::Text("hunter2".to_owned()))],
			now() + 4_000,
			&mut rng,
		)
		.expect("writes a secret value there");
	assert_eq!(
		laptop
			.secrets_key_numbers(household)
			.expect("a space")
			.0
			.len(),
		1
	);
}

/// A library with a space, a type of tasks with a link, a person, and a task about them.
fn with_task(
	name: &str,
	rng: &mut TestRng,
) -> (
	Library,
	nibkeep_base::SpaceId,
	nibkeep_base::EntityId,
	nibkeep_base::EntityId,
) {
	use nibkeep_model::{FieldDef, FieldName, TypeDef, ValueKind};
	let (_, mut laptop) = library(name, rng);
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, rng)
		.expect("makes the space");
	let about: FieldName = "local.about".parse().expect("a field");
	let task = TypeDef::new(TypeName::from_static("local.task"), 1)
		.with_field(about.clone(), FieldDef::new(ValueKind::Reference));
	laptop
		.define_type(personal, &task, now() + 2_000, rng)
		.expect("defines");
	note(&mut laptop, personal, "Boiler service", now() + 3_000, rng);
	let (_, boiler) = laptop.locate("Boiler service").expect("finds");
	let task = laptop
		.create_entity(
			personal,
			&[TypeName::from_static("local.task")],
			vec![
				(
					"org.nibkeep.name".parse().expect("a field"),
					Value::Name(nibkeep_model::Name::new("Call the plumber")),
				),
				(about, Value::Reference(boiler)),
			],
			now() + 4_000,
			rng,
		)
		.expect("adds");
	(laptop, household, boiler, task)
}

/// Moves an entity of one space to another, with its history.
fn move_bringing_history(
	library: &mut Library,
	(from, entity, to): (
		nibkeep_base::SpaceId,
		nibkeep_base::EntityId,
		nibkeep_base::SpaceId,
	),
	at: u64,
	rng: &mut TestRng,
) -> Result<nibkeep_base::EntityId, nibkeep_node::NodeError> {
	library.move_entity(
		from,
		entity,
		to,
		nibkeep_node::MoveHistory::Bring,
		nibkeep_node::Behind::Keep,
		nibkeep_node::MoveSecrets::Bring,
		at,
		rng,
	)
}

#[test]
fn a_move_keeps_the_links_and_leaves_the_history_where_it_was() {
	let mut rng = TestRng::new("moving");
	let (mut laptop, household, boiler, task) = with_task("moving", &mut rng);
	let personal = laptop.personal_space();
	let moved = move_bringing_history(
		&mut laptop,
		(personal, boiler, household),
		now() + 5_000,
		&mut rng,
	)
	.expect("moves");
	assert_ne!(moved, boiler);
	// The person is found in the household now, by its name and by its old ID.
	assert_eq!(
		laptop.locate("Boiler service").expect("finds"),
		(household, moved)
	);
	assert_eq!(
		laptop.locate(&boiler.to_string()).expect("finds"),
		(household, moved)
	);
	assert_eq!(
		laptop.moved_into(personal, boiler).expect("reads"),
		Some((household, moved))
	);
	// The task's link names it still.
	assert_eq!(
		laptop.name_anywhere(boiler).expect("names").as_deref(),
		Some("Boiler service")
	);
	// Its history before the move stays in the personal space.
	let history = laptop
		.history(personal, boiler, None)
		.expect("reads")
		.expect("a history");
	assert!(!history.fields.is_empty(), "{history:?}");
	// A move of an entity of a type the other space lacks brings the type along.
	let moved_task = move_bringing_history(
		&mut laptop,
		(personal, task, household),
		now() + 6_000,
		&mut rng,
	)
	.expect("moves the task");
	assert!(
		laptop
			.types(household)
			.expect("reads")
			.get(&TypeName::from_static("local.task"))
			.is_some()
	);
	assert_eq!(
		laptop
			.name(household, moved_task)
			.expect("names")
			.as_deref(),
		Some("Call the plumber")
	);
	// Moved once, it moves no further from where it was.
	assert!(matches!(
		move_bringing_history(
			&mut laptop,
			(personal, task, household),
			now() + 7_000,
			&mut rng
		),
		Err(nibkeep_node::NodeError::MovedAlready)
	));
	// With the personal space chosen, the old entity is found there, to show or purge.
	laptop.choose_space(Some(personal)).expect("chooses");
	assert_eq!(
		laptop.locate("Boiler service").expect("finds"),
		(personal, boiler)
	);
	laptop.choose_space(None).expect("chooses");
	// Moved back, it is found where it is now, by its name and by each old ID.
	let back = move_bringing_history(
		&mut laptop,
		(household, moved, personal),
		now() + 8_000,
		&mut rng,
	)
	.expect("moves back");
	assert_eq!(
		laptop.locate("Boiler service").expect("finds"),
		(personal, back)
	);
	assert_eq!(
		laptop.locate(&boiler.to_string()).expect("finds"),
		(personal, back)
	);
	// The library's own records stay where they are.
	let (_, device) = laptop.locate("laptop").expect("finds the device");
	assert!(matches!(
		move_bringing_history(
			&mut laptop,
			(personal, device, household),
			now() + 9_000,
			&mut rng
		),
		Err(nibkeep_node::NodeError::OwnRecord(_))
	));
}

/// The sealed blobs in the replica's blob directory, which its spaces share.
fn sealed_files(directory: &std::path::Path) -> usize {
	let blobs = nibkeep_store::blob_directory(&common::replica_folder(directory));
	let mut count = 0;
	let mut directories = vec![blobs];
	while let Some(directory) = directories.pop() {
		for entry in std::fs::read_dir(&directory)
			.into_iter()
			.flatten()
			.flatten()
		{
			if entry.path().is_dir() {
				directories.push(entry.path());
			} else {
				count += 1;
			}
		}
	}
	count
}

#[test]
fn a_moved_files_content_is_one_sealed_copy_that_a_purge_in_one_space_leaves_to_the_other() {
	let mut rng = TestRng::new("moving files");
	let (directory, mut laptop) = library("moving files", &mut rng);
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "moving files, the file");
	let path = folder.join("manual.txt");
	std::fs::write(&path, "Bleed the radiators in October.\n").expect("writes");
	let added = laptop
		.add_file(personal, &path, now() + 2_000, &mut rng)
		.expect("adds");
	let content = added.content;
	let moved = move_bringing_history(
		&mut laptop,
		(personal, added.file, household),
		now() + 3_000,
		&mut rng,
	)
	.expect("moves");
	assert!(
		laptop
			.file_content(household, &content, 1 << 20)
			.expect("reads")
			.is_some()
	);
	assert_eq!(sealed_files(&directory), 1, "one sealed copy for both");

	// A purge in the household says that the personal space keeps it, and leaves it there.
	let purgeable = laptop
		.purgeable(
			household,
			moved,
			&nibkeep_std::CONTENT,
			Some(&Value::Blob(content)),
		)
		.expect("says what goes");
	assert_eq!(purgeable.elsewhere, [personal]);
	laptop
		.purge(
			household,
			moved,
			&nibkeep_std::CONTENT,
			Some(&Value::Blob(content)),
			now() + 4_000,
			&mut rng,
		)
		.expect("purges");
	std::fs::remove_file(&path).expect("removes the original");
	assert!(
		laptop
			.file_content(household, &content, 1 << 20)
			.expect("reads")
			.is_none()
	);
	assert!(
		laptop
			.file_content(personal, &content, 1 << 20)
			.expect("reads")
			.is_some()
	);
	assert_eq!(sealed_files(&directory), 1);
	assert!(laptop.verify(now() + 5_000).expect("checks").is_sound());

	// Purged in the personal space too, it leaves the disk.
	let purgeable = laptop
		.purgeable(
			personal,
			added.file,
			&nibkeep_std::CONTENT,
			Some(&Value::Blob(content)),
		)
		.expect("says what goes");
	nibkeep_testkit::assert_empty!(purgeable.elsewhere);
	laptop
		.purge(
			personal,
			added.file,
			&nibkeep_std::CONTENT,
			Some(&Value::Blob(content)),
			now() + 6_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(sealed_files(&directory), 0);
}

#[test]
fn content_added_to_two_spaces_is_stored_once() {
	let mut rng = TestRng::new("content in two spaces");
	let (directory, mut laptop) = library("content in two spaces", &mut rng);
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let photo = vec![7_u8; 100_000];
	let content = laptop
		.import_blob(personal, photo.as_slice(), now() + 2_000, &mut rng)
		.expect("imports");
	assert_eq!(
		laptop
			.import_blob(household, photo.as_slice(), now() + 3_000, &mut rng)
			.expect("imports"),
		content
	);
	assert_eq!(sealed_files(&directory), 1);
	let report = laptop.verify(now() + 4_000).expect("checks");
	assert!(report.is_sound(), "{report:?}");
	assert_eq!(
		report
			.spaces
			.iter()
			.map(|space| space.blobs)
			.collect::<Vec<_>>(),
		[1, 1],
		"each space reads its copy"
	);

	// Opened again, both read it.
	drop(laptop);
	let laptop = Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	for space in [personal, household] {
		assert_eq!(
			laptop
				.file_content(space, &content, 1 << 20)
				.expect("reads")
				.expect("holds it")
				.len(),
			photo.len()
		);
	}
}

#[test]
fn a_copy_that_two_spaces_share_reaches_another_replica_once() {
	let mut rng = TestRng::new("a shared copy syncs");
	let (_, mut laptop) = library("a shared copy syncs, laptop", &mut rng);
	let desk_folder = new_folder("a shared copy syncs, desk");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_named(&mut laptop, &desk_folder, "desk", (&code, &code), &mut rng);
	paired.expect("pairs");
	let mut desk = joined.expect("joins").library;
	let household = laptop
		.create_space("Household", now() + 100_000, &mut rng)
		.expect("makes the space");
	let photo = vec![9_u8; 200_000];
	let personal = laptop.personal_space();
	let content = laptop
		.import_blob(personal, photo.as_slice(), now() + 101_000, &mut rng)
		.expect("imports");
	laptop
		.import_blob(household, photo.as_slice(), now() + 102_000, &mut rng)
		.expect("imports");
	sync_rounds(&mut laptop, &mut desk, now() + 110_000, 4);
	assert_eq!(desk.spaces(), vec![desk.personal_space(), household]);
	for space in [personal, household] {
		assert_eq!(
			desk.file_content(space, &content, 1 << 20)
				.expect("reads")
				.map(|bytes| bytes.len()),
			Some(photo.len())
		);
	}
	assert_eq!(sealed_files(&desk_folder), 1, "one copy on the desk too");
}

#[test]
fn a_move_takes_no_secret_values_while_the_secrets_are_locked() {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_keys::NewProtection;
	let mut rng = TestRng::new("moving secrets");
	let (_, mut laptop) = library("moving secrets", &mut rng);
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let secrets =
		NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"));
	laptop
		.set_up_secrets(&secrets, now() + 2_000, &mut rng)
		.expect("sets up the secrets");
	let login = laptop
		.create_entity(
			personal,
			&[nibkeep_std::LOGIN],
			vec![(nibkeep_std::PASSWORD, Value::Text("hunter2".to_owned()))],
			now() + 3_000,
			&mut rng,
		)
		.expect("adds");
	// Locked, as the secrets usually are, which entity a sealed value is of is unknown.
	laptop.lock_secrets();
	assert!(matches!(
		move_bringing_history(
			&mut laptop,
			(personal, login, household),
			now() + 4_000,
			&mut rng
		),
		Err(nibkeep_node::NodeError::MoveNeedsSecrets)
	));
}
