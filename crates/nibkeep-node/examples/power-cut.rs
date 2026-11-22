//! The device's side of real power cuts: writes to a library, or to a tree with secret files,
//! until the power goes, and afterwards checks what was kept. `scripts/power-cut.py` drives
//! it over SSH.
//!
//!     power-cut write DIR   adds people, renames them and imports blobs, printing
//!                           `done CHANGES BLOBS` after every operation that returns
//!     power-cut check DIR   prints `kept CHANGES BLOBS` and `sound`, or what is wrong;
//!                           on a drive without a journal, `not closed` after a cut
//!     power-cut check --drive-checked DIR
//!                           the same, once the drive was checked after such a cut
//!     power-cut write-tree DIR
//!                           in DIR, a laptop's library and copy of a tree with secret
//!                           files, and a desk's: the desk edits, moves, deletes and adds
//!                           secret files and commits, the laptop receives it in a sync,
//!                           updates its copy, and commits its own; prints `done COMMITS KEY`,
//!                           the laptop's commits and the number of the key its copy holds,
//!                           after each update and commit that returns
//!     power-cut check-tree DIR ROUND COMMITS KEY
//!                           checks both libraries and the laptop's copy against the last
//!                           report, then updates the copy; prints `kept COMMITS KEY` and
//!                           `sound`, or panics saying what is wrong; on a drive without a
//!                           journal, `not closed` after a cut
//!     power-cut check-tree --drive-checked DIR ROUND COMMITS KEY
//!                           the same, once the drive was checked after such a cut
//!
//! The libraries are protected by passphrases that only tests use, and hold no one's data.
//! The tree's turns and checks are those of the power-cut tests with LazyFS, which creates,
//! renames and deletes files at once: here a real power cut tests the saving of folders too,
//! and cuts the desk as well.

// Shared with the tests.
#[path = "../tests/common/in_process.rs"]
mod in_process;
#[path = "../tests/common/tree_turns.rs"]
mod tree_turns;

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use in_process::{pair_joining, sync_in_memory};
use nibkeep_crypto::PassphraseParams;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{Name, Value};
use nibkeep_node::{Dating, Edit, Library, NewLibrary, NewProtection, NodeError, Unlocker};
use nibkeep_testkit::{TestRng, millis_at};
use tree_turns::{
	OWN, TREE_SENTINEL, TreePlaces, check_tree_after, env_entry, env_in_tree, key_in,
	make_tree_libraries, number_in, open_desk, open_laptop, save_folder, take_turns,
};

const PASSPHRASE: &str = "correct horse";

fn now() -> u64 {
	let elapsed = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("after 1970");
	u64::try_from(elapsed.as_millis()).expect("a time this side of the far future")
}

/// The content of the `number`th blob: several chunks, different for every number.
fn blob(number: usize) -> Vec<u8> {
	let mut content = u64::try_from(number).expect("small").to_le_bytes().to_vec();
	content.extend(
		(0..140_000)
			.map(|index| u8::try_from((index * 13 + number * 31) % 251).expect("below 251")),
	);
	content
}

fn open(directory: &Path) -> Library {
	Library::open(directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library")
}

fn write(directory: &Path) -> ! {
	let mut rng = TestRng::new(&format!("power cut {}", now()));
	if !Library::exists(directory) {
		// On a USB stick formatted FAT or exFAT too, as a person may choose (technical design:
		// drives without a journal).
		Library::allow_fat(directory).expect("allows a drive without a journal");
		let protection =
			NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
		Library::create(
			directory,
			&NewLibrary::new("Ada", "raspberry pi", "power-cut"),
			&protection,
			now(),
			&mut rng,
		)
		.expect("creates the library");
	}
	let mut library = open(directory);
	let personal = library.personal_space();
	let found = library.verify(now()).expect("verifies");
	let mut changes = found.spaces[0].changes;
	let mut blobs = found.spaces[0].blobs;
	let mut people = Vec::new();
	let mut out = std::io::stdout();
	loop {
		let step = changes + blobs;
		// A blob now and then: every check reads them all.
		if step % 20 == 19 {
			library
				.import_blob(personal, blob(blobs).as_slice(), now(), &mut rng)
				.expect("imports");
			blobs += 1;
		} else if step % 3 == 2
			&& let Some(&person) = people.last()
		{
			let name = Value::Name(Name::new(format!("also known as {step}")));
			library
				.edit_entity(
					personal,
					person,
					vec![Edit::Add(NAME, name)],
					&Dating::today(now(), 0).expect("a moment"),
					now(),
					&mut rng,
				)
				.expect("edits");
			changes += 1;
		} else {
			let name = Value::Name(Name::new(format!("person {step}")));
			people.push(
				library
					.create_entity(personal, &[PERSON], vec![(NAME, name)], now(), &mut rng)
					.expect("adds"),
			);
			changes += 1;
		}
		// Only now is it done, as far as the driver can tell.
		writeln!(out, "done {changes} {blobs}")
			.and_then(|()| out.flush())
			.expect("reports");
	}
}

fn check(directory: &Path, drive_checked: bool) -> ExitCode {
	if drive_checked {
		Library::drive_checked(directory).expect("says that the drive was checked");
	}
	let mut library = match Library::open(directory, &Unlocker::Passphrase(PASSPHRASE)) {
		Ok(library) => library,
		// On a drive without a journal, the cut left the replica open: the drive is checked
		// before it opens.
		Err(NodeError::UncleanRemoval) => {
			println!("not closed");
			return ExitCode::FAILURE;
		}
		Err(error) => panic!("opens the library: {error:?}"),
	};
	let changes = library.status().expect("reads the status").spaces[0].changes;
	let report = library.verify(now()).expect("verifies");
	println!("kept {changes} {}", report.spaces[0].blobs);
	if report.is_sound() {
		println!("sound");
		ExitCode::SUCCESS
	} else {
		println!("problems {:?}", report.spaces[0].problems);
		ExitCode::FAILURE
	}
}

/// In a tree's directory while its libraries and copies are made, and renamed `made` once
/// they are, before the first turn: a directory with `making` holds what a failed start of
/// this program left, which it makes again; one with neither must be empty.
const MAKING: &str = "making";
const MADE: &str = "made";

/// The tree's places: all in `directory`, whose power goes with the laptop's.
fn tree_places(directory: &Path) -> TreePlaces {
	TreePlaces::new(directory, directory)
}

/// Makes the tree's libraries and copies in `places.directory`, unless they are made.
fn make_tree(places: &TreePlaces, rng: &mut TestRng) {
	let directory = &places.directory;
	if directory.join(MADE).exists() {
		return;
	}
	// Deleting only what a start of this program left, never what someone keeps in a
	// directory given by mistake.
	if directory.join(MAKING).exists() {
		std::fs::remove_dir_all(directory).expect("deletes what a failed start left");
	} else {
		match std::fs::read_dir(directory) {
			Ok(mut entries) => assert!(
				entries.next().is_none(),
				"{} holds files already: give a new or empty directory",
				directory.display()
			),
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => panic!("cannot read {}: {error}", directory.display()),
		}
	}
	std::fs::create_dir_all(directory).expect("makes the directory");
	std::fs::File::create(directory.join(MAKING))
		.and_then(|making| making.sync_all())
		.expect("marks the libraries as being made");
	save_folder(directory);
	// On a USB stick formatted FAT or exFAT too, as with `write`.
	for library in [&places.laptop, &places.desk] {
		Library::allow_fat(library).expect("allows a drive without a journal");
	}
	make_tree_libraries(places, rng);
	std::fs::rename(directory.join(MAKING), directory.join(MADE))
		.expect("marks the libraries made");
	save_folder(directory);
}

fn write_tree(directory: &Path) -> ! {
	let places = tree_places(directory);
	let mut rng = TestRng::new(&format!("power cut tree {}", now()));
	make_tree(&places, &mut rng);
	// Where the last check left them: the tree at the copy's `.env`, or at the version
	// before, which the copy holds edited.
	let in_tree = env_in_tree(&open_laptop(&places));
	let env = std::fs::read_to_string(places.on_laptop.join(OWN[0])).expect("reads the .env");
	let written = number_in(&env);
	let own = if in_tree == env_entry(written) {
		written
	} else {
		let before = written
			.checked_sub(1)
			.filter(|&before| in_tree == env_entry(before));
		before.unwrap_or_else(|| {
			panic!("the copy holds .env {written}, and the tree neither it nor the one before")
		})
	};
	let (_, key) = key_in(&places.on_laptop).expect("the laptop holds the key");
	let mut out = std::io::stdout();
	take_turns(&places, [own, key, 0], &mut rng, &mut |[own, key, _]| {
		// Only now is it done, as far as the driver can tell.
		writeln!(out, "done {own} {key}")
			.and_then(|()| out.flush())
			.expect("reports");
	})
}

fn check_tree(
	directory: &Path,
	round: u64,
	reported: (usize, usize),
	drive_checked: bool,
) -> ExitCode {
	let places = tree_places(directory);
	let libraries = [("laptop", &places.laptop), ("desk", &places.desk)];
	if drive_checked {
		for (_, library) in libraries {
			Library::drive_checked(library).expect("says that the drive was checked");
		}
	} else {
		// On a drive without a journal, the cut left both replicas open, as the turns keep
		// both libraries open: the drive is checked before they open.
		let left_open: Vec<&str> = libraries
			.iter()
			.filter(|(_, library)| match Library::protection(library) {
				Ok(_) => false,
				Err(NodeError::UncleanRemoval) => true,
				Err(error) => panic!("reads the library in {}: {error:?}", library.display()),
			})
			.map(|(name, _)| *name)
			.collect();
		match left_open.as_slice() {
			[] => {}
			[_, _] => {
				println!("not closed");
				return ExitCode::FAILURE;
			}
			only => {
				println!("not closed: the {} library alone", only.join(", "));
				return ExitCode::FAILURE;
			}
		}
	}
	let (kept, held) = check_tree_after(round, &places, reported, true);
	// The desk's power went too.
	let mut desk = open_desk(&places);
	let personal = desk.personal_space();
	let tree = desk.trees(personal).expect("lists trees")[0].id;
	desk.tree_status(personal, tree)
		.expect("reads the desk's status");
	let report = desk
		.verify(millis_at("2026-10-12", 11, 0))
		.expect("verifies the desk's library");
	assert!(report.is_sound(), "the desk's library: {report:?}");
	assert!(
		!nibkeep_testkit::holds_anywhere(&places.desk, TREE_SENTINEL.as_bytes()),
		"a secret in plain in the desk's library"
	);
	println!("kept {kept} {held}");
	println!("sound");
	ExitCode::SUCCESS
}

fn main() -> ExitCode {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	let number = |text: &str| text.parse::<usize>().expect("a number");
	match arguments.as_slice() {
		[command, directory] if command == "write" => write(Path::new(directory)),
		[command, directory] if command == "check" => check(Path::new(directory), false),
		[command, flag, directory] if command == "check" && flag == "--drive-checked" => {
			check(Path::new(directory), true)
		}
		[command, directory] if command == "write-tree" => write_tree(Path::new(directory)),
		[command, directory, round, commits, key] if command == "check-tree" => check_tree(
			Path::new(directory),
			round.parse().expect("a round"),
			(number(commits), number(key)),
			false,
		),
		[command, flag, directory, round, commits, key]
			if command == "check-tree" && flag == "--drive-checked" =>
		{
			check_tree(
				Path::new(directory),
				round.parse().expect("a round"),
				(number(commits), number(key)),
				true,
			)
		}
		_ => {
			eprintln!(
				"usage: power-cut write DIR | power-cut check [--drive-checked] DIR\n       \
				 power-cut write-tree DIR | power-cut check-tree [--drive-checked] DIR ROUND COMMITS KEY"
			);
			ExitCode::FAILURE
		}
	}
}
