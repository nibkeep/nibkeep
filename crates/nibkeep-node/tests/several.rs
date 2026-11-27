//! Several programs on one replica at once ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#78-several-programs-on-one-replica)):
//! two libraries of the replica open in one program, as two syncs have it, and another
//! program that writes while this one does, or is killed meanwhile. Each writes on what the
//! others wrote, a field that one makes secret is sealed by all, and the replica stays
//! sound.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use nibkeep_crypto::PassphraseParams;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_log::LogError;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Library, NewLibrary, NewProtection, NodeError, Unlocker};
use nibkeep_store::StoreError;
use nibkeep_testkit::{TestRng, child, millis_at};

const PASSPHRASE: &str = "correct horse";

/// Set in a child, to the directory of its test.
const CHILD: &str = "NIBKEEP_SEVERAL_CHILD";

/// How many people each program adds side by side.
const SIDE_BY_SIDE: usize = 25;

fn now() -> u64 {
	millis_at("2026-10-31", 10, 0)
}

/// A new library, in the folder `library` of a directory of its own for the test.
fn library(name: &str) -> PathBuf {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name).join("library");
	Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid")),
		now(),
		&mut TestRng::new(name),
	)
	.expect("creates");
	directory
}

fn open(directory: &Path) -> Library {
	Library::open(directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens")
}

fn add_person(library: &mut Library, name: &str, rng: &mut TestRng) {
	let personal = library.personal_space();
	library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new(name)))],
			now(),
			rng,
		)
		.expect("adds");
}

/// The names of the people a search for `word` finds, sorted.
fn found(library: &Library, word: &str) -> Vec<String> {
	let mut names: Vec<String> = library
		.search(library.personal_space(), word)
		.expect("searches")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

/// Runs this test again as a child process, with [`CHILD`] set to `directory`.
fn start_child(directory: &Path) -> std::process::Child {
	Command::new(std::env::current_exe().expect("this test"))
		.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
		.env(CHILD, directory)
		.stdout(Stdio::null())
		.stderr(child::errors(directory))
		.spawn()
		.expect("starts the child")
}

/// Opens the library in `directory`, then starts the test `test` as a child, and waits until
/// the child has the library open too, as it notes: so neither writes before both have it
/// open, and each change commits at once. Changes that one made before the other opened it,
/// as before a child that starts slowly on Windows, would form a burst, whose transaction
/// holds the state file's write lock while that program waits for the other, which cannot
/// open the library meanwhile (docs/Known-limits.md).
fn open_with_child(directory: &Path) -> (Library, std::process::Child) {
	let library = open(directory);
	let test_directory = directory.parent().expect("the test's directory");
	let mut other = start_child(test_directory);
	child::wait_for_note(&mut other, test_directory);
	(library, other)
}

#[test]
fn two_libraries_of_one_replica_open_at_once_write_each_on_what_the_other_wrote() {
	let directory = library("two at once");
	let mut rng = TestRng::new("two at once");
	let mut one = open(&directory);
	let mut other = open(&directory);
	add_person(&mut one, "Grace Hopper", &mut rng);
	add_person(&mut other, "Ada Lovelace", &mut rng);
	// Each reads what the other wrote.
	assert_eq!(found(&one, "Lovelace"), ["Ada Lovelace"]);
	assert_eq!(found(&other, "Hopper"), ["Grace Hopper"]);
	assert!(one.catch_up().expect("catches up"));
	assert!(!one.catch_up().expect("catches up"), "once");
	add_person(&mut one, "Hedy Lamarr", &mut rng);
	let changes = one.status().expect("reads").spaces[0].changes;
	drop((one, other));
	let mut reopened = open(&directory);
	assert_eq!(reopened.status().expect("reads").spaces[0].changes, changes);
	assert_eq!(found(&reopened, "Hedy"), ["Hedy Lamarr"]);
	let report = reopened.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
}

#[test]
fn a_field_that_another_program_makes_secret_is_sealed_here_too() {
	let directory = library("made secret elsewhere");
	let mut rng = TestRng::new("made secret elsewhere");
	let login = TypeName::from_static("local.login");
	let pin: FieldName = "local.pin".parse().expect("a field name");
	let mut one = open(&directory);
	let personal = one.personal_space();
	let plain =
		TypeDef::new(login.clone(), 1).with_field(pin.clone(), FieldDef::new(ValueKind::Text));
	one.define_type(personal, &plain, now(), &mut rng)
		.expect("defines");
	// Another program, open beside it, makes the field secret.
	let mut other = open(&directory);
	let secret = TypeDef::new(login.clone(), 2)
		.with_field(pin.clone(), FieldDef::new(ValueKind::Text).secret());
	other
		.define_type(personal, &secret, now(), &mut rng)
		.expect("defines");
	// The first, which knew the field as ordinary, seals it from its next change: with the
	// secrets locked, it refuses the value rather than write it in plain.
	let value = vec![(pin.clone(), Value::Text("pin 7319-2846-5501".into()))];
	let refused = one.create_entity(
		personal,
		std::slice::from_ref(&login),
		value,
		now(),
		&mut rng,
	);
	assert!(
		matches!(
			refused,
			Err(NodeError::Store(StoreError::Log(LogError::SecretsLocked)))
		),
		"{refused:?}"
	);
	assert!(
		one.secret_fields(personal)
			.is_some_and(|secret| secret.contains(&pin))
	);
	drop((one, other));
	assert!(
		!nibkeep_testkit::holds_anywhere(&directory, b"7319-2846-5501"),
		"never in plain on the disk"
	);
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn another_program_writes_while_this_one_does() {
	if let Some(directory) = std::env::var_os(CHILD) {
		let directory = PathBuf::from(directory);
		let mut library = open(&directory.join("library"));
		child::note(&directory, &[0]);
		let mut rng = TestRng::new("the other program");
		for number in 0..SIDE_BY_SIDE {
			add_person(&mut library, &format!("Childperson {number}"), &mut rng);
		}
		std::process::exit(0);
	}
	let directory = library("another program");
	let mut rng = TestRng::new("this program");
	let test = directory.parent().expect("the test's directory");
	let (mut library, mut other) = open_with_child(&directory);
	for number in 0..SIDE_BY_SIDE {
		add_person(&mut library, &format!("Parentperson {number}"), &mut rng);
	}
	let status = other.wait().expect("waits for the other program");
	assert!(status.success(), "{}", child::errors_written(test));
	library.catch_up().expect("catches up");
	assert_eq!(found(&library, "Childperson").len(), SIDE_BY_SIDE);
	assert_eq!(found(&library, "Parentperson").len(), SIDE_BY_SIDE);
	let report = library.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
}

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn another_program_killed_while_this_one_writes_leaves_the_replica_sound() {
	if let Some(directory) = std::env::var_os(CHILD) {
		let directory = PathBuf::from(directory);
		let mut library = open(&directory.join("library"));
		let mut rng = TestRng::new("the killed program");
		let mut added = 0;
		child::note(&directory, &[added]);
		loop {
			add_person(&mut library, &format!("Killedperson {added}"), &mut rng);
			added += 1;
			child::note(&directory, &[added]);
		}
	}
	let directory = library("another program killed");
	let mut rng = TestRng::new("this program, which lives on");
	let test = directory.parent().expect("the test's directory");
	let (mut library, mut other) = open_with_child(&directory);
	std::thread::scope(|scope| {
		scope.spawn(|| {
			child::kill_after_progress(&mut other, test, 0, Duration::from_millis(100));
		});
		for number in 0..SIDE_BY_SIDE {
			add_person(&mut library, &format!("Liveperson {number}"), &mut rng);
		}
	});
	// It goes on writing once the other is gone, on what the other wrote.
	for number in SIDE_BY_SIDE..2 * SIDE_BY_SIDE {
		add_person(&mut library, &format!("Liveperson {number}"), &mut rng);
	}
	let [noted] = child::noted(test);
	library.catch_up().expect("catches up");
	let killed = found(&library, "Killedperson").len();
	assert!(
		killed == noted || killed == noted + 1,
		"{noted} noted, {killed} kept"
	);
	assert_eq!(found(&library, "Liveperson").len(), 2 * SIDE_BY_SIDE);
	drop(library);
	let mut reopened = open(&directory);
	let report = reopened.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
	assert_eq!(found(&reopened, "Killedperson").len(), killed);
}
