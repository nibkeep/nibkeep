//! Power cuts: a library on LazyFS, a file system that keeps the data written to files in
//! memory until it is synced, and on command drops what is not, as a power cut does. It
//! carries out creating, renaming and deleting files at once, so the syncing of
//! directories is not tested here; real power cuts on a Raspberry Pi test that.
//!
//! A child process uses the library, and after every operation that returns, notes it on
//! the real disk. The test kills the child at a random moment and drops what LazyFS holds
//! unsynced. The library must then open, hold everything noted, and be found sound. Another
//! child receives changes from a replica on the real disk, in the batches that a sync saves
//! them in, and a third pair of children write at once.
//!
//! The test needs LazyFS (<https://github.com/dsrhaslab/lazyfs>), built, and its program in
//! `NIBKEEP_LAZYFS`; without it, it says so and passes. It runs on Linux, where FUSE is.

#![cfg(target_os = "linux")]

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::common::tree_rounds::{tree_child, tree_rounds};
use crate::common::tree_turns::TreePlaces;
use nibkeep_crypto::PassphraseParams;
use nibkeep_crypto::rand_core::TryRng;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{Name, Value};
use nibkeep_node::{Dating, Edit, Joining, Library, NewLibrary, NewProtection, Unlocker};
use nibkeep_testkit::lazyfs::{LazyFs, unmount};
use nibkeep_testkit::{TestRng, child, millis_at};

const PASSPHRASE: &str = "correct horse";

/// Set in the child, to the directory of the test.
const CHILD: &str = "NIBKEEP_POWER_LOSS_CHILD";

/// How many power cuts a run makes, unless `NIBKEEP_POWER_LOSS_ROUNDS` says how many.
const ROUNDS: u64 = 8;

/// How many people a batch of the child adds: a power cut keeps all of them or none.
const BATCH: usize = 3;

/// The content of the `number`th blob: several chunks, different for every number.
fn blob(number: usize) -> Vec<u8> {
	let mut content = u64::try_from(number).expect("small").to_le_bytes().to_vec();
	content.extend(
		(0..140_000)
			.map(|index| u8::try_from((index * 13 + number * 31) % 251).expect("below 251")),
	);
	content
}

/// How many changes the child's step after `changes` and `blobs` makes: none for a blob,
/// all of a batch, or one.
fn changes_of_next_step(changes: usize, blobs: usize) -> usize {
	let step = changes + blobs;
	if step % 5 == 4 {
		0
	} else if step % 7 == 6 {
		BATCH
	} else {
		1
	}
}

/// Uses the library until the process is killed: adds people, alone or in batches, gives
/// them more names, and imports blobs, noting the changes and blobs after each that
/// returns. Never returns.
fn use_until_killed(directory: &Path, library: &Path) -> ! {
	let mut library =
		Library::open(library, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	let personal = library.personal_space();
	let [mut changes, mut blobs] = child::noted(directory);
	// Every run starts after more was kept, so its randomness is its own.
	let mut rng = TestRng::new(&format!("power loss child {changes} {blobs}"));
	let mut people = Vec::new();
	loop {
		let wall = millis_at("2026-10-12", 10, 0) + u64::try_from(changes).expect("small");
		let step = changes + blobs;
		if step % 5 == 4 {
			library
				.import_blob(personal, blob(blobs).as_slice(), wall, &mut rng)
				.expect("imports");
			blobs += 1;
		} else if step % 7 == 6 {
			let added = library
				.batch(personal, |library| {
					(0..BATCH)
						.map(|number| {
							let name = Value::Name(Name::new(format!("person {step}.{number}")));
							library.create_entity(
								personal,
								&[PERSON],
								vec![(NAME, name)],
								wall,
								&mut rng,
							)
						})
						.collect::<Result<Vec<_>, _>>()
				})
				.expect("adds in a batch");
			people.extend(added);
			changes += BATCH;
		} else if step % 3 == 2
			&& let Some(&person) = people.last()
		{
			let name = Value::Name(Name::new(format!("also known as {step}")));
			library
				.edit_entity(
					personal,
					person,
					vec![Edit::Add(NAME, name)],
					&Dating::today(wall, 0).expect("a moment"),
					wall,
					&mut rng,
				)
				.expect("edits");
			changes += 1;
		} else {
			let name = Value::Name(Name::new(format!("person {step}")));
			people.push(
				library
					.create_entity(personal, &[PERSON], vec![(NAME, name)], wall, &mut rng)
					.expect("adds"),
			);
			changes += 1;
		}
		child::note(directory, &[changes, blobs]);
	}
}

#[test]
fn power_cuts_lose_nothing_committed() {
	if let Some(directory) = std::env::var_os(CHILD) {
		let directory = PathBuf::from(directory);
		use_until_killed(&directory, &directory.join("lazyfs/mount/library"));
	}
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	// A run that was interrupted may have left LazyFS mounted.
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("power-loss/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "power-loss");
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	let library = lazyfs.mount.join("library");

	// A new library survives a power cut right after it is made.
	let mut rng = TestRng::new("power loss");
	let protection =
		NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
	let created = Library::create(
		&library,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		millis_at("2026-10-12", 9, 0),
		&mut rng,
	)
	.expect("creates");
	let first = created.status().expect("reads the status").spaces[0].changes;
	drop(created);
	lazyfs.cut_power();
	let opened = Library::open(&library, &Unlocker::Passphrase(PASSPHRASE))
		.expect("opens after a power cut right after it was made");
	assert_eq!(
		opened.status().expect("reads the status").spaces[0].changes,
		first
	);
	drop(opened);
	child::note(&directory, &[first, 0]);

	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(ROUNDS, |rounds| rounds.parse().expect("a number"));
	for round in 0..rounds {
		let mut process = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(CHILD, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		// Up to half a second after the child's first progress, so that some cuts come
		// when the log has grown and SQLite moves its write-ahead log into the database.
		let delay = rng.try_next_u64().expect("infallible") % 500;
		child::kill_after_progress(
			&mut process,
			&directory,
			round,
			Duration::from_millis(delay),
		);
		lazyfs.cut_power();

		let [changes, blobs] = child::noted(&directory);
		let mut library = Library::open(&library, &Unlocker::Passphrase(PASSPHRASE))
			.unwrap_or_else(|error| panic!("round {round}: opens after the power cut: {error}"));
		let kept = library.status().expect("reads the status").spaces[0].changes;
		// The change or batch that was being noted may be kept too, but never part of a
		// batch.
		let next = changes_of_next_step(changes, blobs);
		assert!(
			kept == changes || kept == changes + next,
			"round {round}: {changes} changes noted, {kept} kept"
		);
		let report = library
			.verify(millis_at("2026-10-12", 11, 0))
			.expect("verifies");
		assert!(report.is_sound(), "round {round}: {report:?}");
		let kept_blobs = report.spaces[0].blobs;
		assert!(
			(blobs..=blobs + 1).contains(&kept_blobs),
			"round {round}: {blobs} blobs noted, {kept_blobs} kept"
		);
		// The next child starts from what was kept.
		child::note(&directory, &[kept, kept_blobs]);
	}
}

/// Set in each writer of the test with two writers, to its own directory beside the
/// library's.
const WRITER: &str = "NIBKEEP_POWER_LOSS_WRITER";

/// The writers of the test with two writers, each named after its directory.
const WRITERS: [&str; 2] = ["onewriter", "twowriter"];

/// Adds people named after the writer, one change each, noting each that returns, until the
/// process is killed, while another writer does the same. Never returns.
fn write_until_killed(directory: &Path, library: &Path) -> ! {
	let name = directory
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a writer's name")
		.to_owned();
	let mut library =
		Library::open(library, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	let personal = library.personal_space();
	let [mut added] = child::noted(directory);
	let mut rng = TestRng::new(&format!("{name} {added}"));
	loop {
		let wall = millis_at("2026-10-12", 10, 0) + u64::try_from(added).expect("small");
		let person = Value::Name(Name::new(format!("{name} {added}")));
		library
			.create_entity(personal, &[PERSON], vec![(NAME, person)], wall, &mut rng)
			.expect("adds");
		added += 1;
		child::note(directory, &[added]);
	}
}

#[test]
fn power_cuts_with_two_writers_lose_nothing_committed() {
	if let Some(directory) = std::env::var_os(WRITER) {
		let directory = PathBuf::from(directory);
		let library = directory
			.parent()
			.expect("the test's directory")
			.join("lazyfs/mount/library");
		write_until_killed(&directory, &library);
	}
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("two-writers/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "two-writers");
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	let library = lazyfs.mount.join("library");
	let mut rng = TestRng::new("two writers");
	let protection =
		NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
	drop(
		Library::create(
			&library,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			millis_at("2026-10-12", 9, 0),
			&mut rng,
		)
		.expect("creates"),
	);
	let writers = WRITERS.map(|name| {
		let writer = directory.join(name);
		std::fs::create_dir_all(&writer).expect("makes the writer's directory");
		child::note(&writer, &[0]);
		writer
	});
	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(ROUNDS, |rounds| rounds.parse().expect("a number"));
	for round in 0..rounds {
		let started = writers
			.each_ref()
			.map(|writer| child::noted::<1>(writer)[0]);
		let mut processes = writers.each_ref().map(|writer| {
			Command::new(std::env::current_exe().expect("this test"))
				.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
				.env(WRITER, writer)
				.stdout(Stdio::null())
				.stderr(child::errors(writer))
				.spawn()
				.expect("starts a writer")
		});
		// Both have written before the power goes, at a moment of their work.
		let waited = std::time::Instant::now();
		while writers
			.iter()
			.zip(started)
			.any(|(writer, started)| child::noted::<1>(writer)[0] == started)
		{
			for (process, writer) in processes.iter_mut().zip(&writers) {
				if let Some(status) = process.try_wait().expect("checks the writer") {
					let errors = child::errors_written(writer);
					panic!("round {round}: a writer ended by itself: {status}\n{errors}");
				}
			}
			assert!(
				waited.elapsed() < Duration::from_secs(120),
				"round {round}: a writer did nothing"
			);
			std::thread::sleep(Duration::from_millis(2));
		}
		let delay = rng.try_next_u64().expect("infallible") % 300;
		std::thread::sleep(Duration::from_millis(delay));
		for process in &mut processes {
			process.kill().expect("kills the writer");
			process.wait().expect("waits for the writer");
		}
		std::thread::sleep(Duration::from_millis(50));
		lazyfs.cut_power();

		let mut opened = Library::open(&library, &Unlocker::Passphrase(PASSPHRASE))
			.unwrap_or_else(|error| panic!("round {round}: opens after the power cut: {error}"));
		let personal = opened.personal_space();
		for (writer, name) in writers.iter().zip(WRITERS) {
			let [noted] = child::noted(writer);
			let kept = opened.search(personal, name).expect("searches").len();
			// The change that was being noted may be kept too.
			assert!(
				kept == noted || kept == noted + 1,
				"round {round}: {name} noted {noted}, {kept} kept"
			);
			child::note(writer, &[kept]);
		}
		let report = opened
			.verify(millis_at("2026-10-12", 11, 0))
			.expect("verifies");
		assert!(report.is_sound(), "round {round}: {report:?}");
	}
}

/// Set in the child of the test of receiving, to the directory of the test.
const RECEIVER: &str = "NIBKEEP_POWER_LOSS_RECEIVER";

/// How many people the replica on the real disk adds for each child of the test of
/// receiving, which the replica on LazyFS receives.
const ADDED: usize = 100;

/// Adds people to the replica on the real disk, notes that the replica on LazyFS is about to
/// receive them, which it does in batches, as a sync saves them, and notes how many changes
/// it holds once the sync returns. Then waits to be killed. Never returns.
fn receive_until_killed(directory: &Path) -> ! {
	let unlocker = Unlocker::Passphrase(PASSPHRASE);
	let mut source = Library::open(&directory.join("source"), &unlocker).expect("opens the source");
	let mut receiver = Library::open(&directory.join("lazyfs/mount/library"), &unlocker)
		.expect("opens the receiver");
	let personal = source.personal_space();
	let [noted, _] = child::noted(directory);
	// The source keeps what every child added, as no power cut reaches it: its count, unlike
	// the receiver's, tells each child's randomness and times apart.
	let made = source.status().expect("reads the status").spaces[0].changes;
	let mut rng = TestRng::new(&format!("power loss receiver {made}"));
	let wall = millis_at("2026-10-12", 10, 0) + u64::try_from(made).expect("small");
	source
		.batch(personal, |source| {
			for number in 0..ADDED {
				let name = Value::Name(Name::new(format!("person {made}.{number}")));
				source.create_entity(personal, &[PERSON], vec![(NAME, name)], wall, &mut rng)?;
			}
			Ok(())
		})
		.expect("adds people");
	// The power goes from here on, at a moment of the sync, or after it.
	child::note(directory, &[noted, 1]);
	let (initiated, responded) = common::sync_in_memory(&mut receiver, &mut source, wall, (1, 2));
	initiated.expect("receives");
	responded.expect("sends");
	let changes = receiver.status().expect("reads the status").spaces[0].changes;
	child::note(directory, &[changes, 2]);
	loop {
		std::thread::sleep(Duration::from_secs(1));
	}
}

#[test]
fn power_cuts_while_receiving_lose_nothing_saved() {
	if let Some(directory) = std::env::var_os(RECEIVER) {
		receive_until_killed(Path::new(&directory));
	}
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("receiving/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "receiving");
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	let library = lazyfs.mount.join("library");
	let mut rng = TestRng::new("receiving");
	let protection =
		NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
	// The source, on the real disk, pairs the replica on LazyFS.
	let started = millis_at("2026-10-12", 9, 0);
	let mut source = Library::create(
		&directory.join("source"),
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		started,
		&mut rng,
	)
	.expect("creates");
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let paired = source
		.pair_in_process(&library, joining, &protection, &|| started, &mut rng)
		.expect("pairs");
	drop((source, paired));
	lazyfs.cut_power();
	let opened = Library::open(&library, &Unlocker::Passphrase(PASSPHRASE))
		.expect("opens after a power cut right after it paired");
	let first = opened.status().expect("reads the status").spaces[0].changes;
	drop(opened);
	child::note(&directory, &[first, 0]);

	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(ROUNDS, |rounds| rounds.parse().expect("a number"));
	for round in 0..rounds {
		let mut process = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(RECEIVER, &directory)
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		// About as long as the sync takes, so that most cuts come while it receives.
		let delay = rng.try_next_u64().expect("infallible") % 1_500;
		child::kill_after_progress(
			&mut process,
			&directory,
			round,
			Duration::from_millis(delay),
		);
		lazyfs.cut_power();

		let [noted, _] = child::noted(&directory);
		let mut library = Library::open(&library, &Unlocker::Passphrase(PASSPHRASE))
			.unwrap_or_else(|error| panic!("round {round}: opens after the power cut: {error}"));
		let kept = library.status().expect("reads the status").spaces[0].changes;
		// The batches saved of a sync that was cut are kept too.
		assert!(
			kept >= noted,
			"round {round}: {noted} changes noted, {kept} kept"
		);
		// Verifying takes longer as the replica grows: every tenth round, and the last.
		if round % 10 == 0 || round + 1 == rounds {
			let report = library
				.verify(millis_at("2026-10-12", 11, 0))
				.expect("verifies");
			assert!(report.is_sound(), "round {round}: {report:?}");
		}
		child::note(&directory, &[kept, 0]);
	}
}

/// The name of the test of a tree with secret files on LazyFS, which its child runs.
const TREE_CUTS: &str = "power_cuts_in_a_tree_with_secrets_lose_nothing_committed";

/// A laptop's tree with secret files on LazyFS, with power cuts. The same with crashes alone,
/// which needs no LazyFS, runs on every system but iOS in `tree_crashes.rs`.
#[test]
fn power_cuts_in_a_tree_with_secrets_lose_nothing_committed() {
	tree_child();
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("tree-secrets/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "tree-secrets");
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	let laptop = lazyfs.mount.clone();
	let places = TreePlaces::new(&directory, &laptop);
	tree_rounds(TREE_CUTS, ROUNDS, &places, &laptop, || lazyfs.cut_power());
}

/// Set in the child of the test of deleting a dropped space, to the LazyFS mount, which
/// holds the laptop's library in `laptop`.
const DROP_CHILD: &str = "NIBKEEP_POWER_LOSS_DROP_CHILD";

/// Says that `folder` is the laptop's drive, in the test and in its child alike: the folder
/// on the real disk where its library is made, and LazyFS's mount, where each round copies
/// it, at the same path on the drive, so that the copy opens as the replica made there.
/// LazyFS tells no free space, so a library made on it would take no account of another
/// replica's.
fn laptop_drive(folder: &Path) {
	nibkeep_node::declare_drive(
		folder,
		nibkeep_node::Drive {
			serial: Some(b"power loss drop laptop".to_vec()),
			disk: Some(b"power loss drop laptop".to_vec()),
			..nibkeep_node::Drive::default()
		},
		None,
	);
}

/// Makes, in `laptop`, a library whose replica dropped Project, which a desk in `desk`, on
/// a disk of its own, holds and knows it dropped, so that its next settling deletes Project:
/// a second program held Project open meanwhile, which kept it from deleting it after the
/// syncs. Returns Project.
fn dropped_and_told(laptop_folder: &Path, desk_folder: &Path) -> nibkeep_base::SpaceId {
	let mut rng = TestRng::new("power loss drop");
	for folder in [laptop_folder, desk_folder] {
		std::fs::create_dir_all(folder).expect("makes the folder");
	}
	common::devices::on_disk(desk_folder, "power loss drop desk");
	let protection =
		NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
	// After the pairing, which happens now.
	let mut at = common::now();
	let mut laptop = Library::create(
		laptop_folder,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		at,
		&mut rng,
	)
	.expect("creates");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) =
		common::pair_named(&mut laptop, desk_folder, "desk", (&code, &code), &mut rng);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	at += 60_000;
	let project = laptop
		.create_space("Project", at, &mut rng)
		.expect("makes it");
	laptop
		.create_entity(
			project,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new("Plan")))],
			at + 100,
			&mut rng,
		)
		.expect("adds");
	// The accounts are issued anew a minute apart.
	at = common::sync_rounds(&mut desk, &mut laptop, at + 61_000, 1);
	laptop
		.install_policies(project, None, at + 500, &mut rng)
		.expect("installs them");
	let critical = laptop.find_policy(project, "Critical").expect("finds it");
	laptop
		.edit_policy(
			project,
			critical,
			&nibkeep_node::PolicyEdit {
				copies: Some(1),
				away: Some(0),
				..nibkeep_node::PolicyEdit::default()
			},
			at + 1_000,
			&mut rng,
		)
		.expect("edits it");
	for _ in 0..4 {
		at = common::sync_rounds(&mut desk, &mut laptop, at + 61_000, 1);
	}
	let dropping = laptop
		.drop_space(project, at + 1_000, &mut rng)
		.expect("drops");
	assert_eq!(
		dropping.stage,
		nibkeep_node::DropStage::Dropped,
		"{dropping:?}"
	);
	let keeping =
		Library::open(laptop_folder, &Unlocker::Passphrase(PASSPHRASE)).expect("opens again");
	for _ in 0..3 {
		at = common::sync_rounds(&mut desk, &mut laptop, at + 61_000, 1);
	}
	assert!(laptop.spaces().contains(&project));
	drop(keeping);
	project
}

/// Copies the folder `from` to `to`, syncing each file and folder, so that a power cut
/// keeps it whole.
fn copy_synced(from: &Path, to: &Path) {
	std::fs::create_dir_all(to).expect("makes the folder");
	for entry in std::fs::read_dir(from).expect("lists") {
		let entry = entry.expect("lists");
		let target = to.join(entry.file_name());
		if entry.file_type().expect("a type").is_dir() {
			copy_synced(&entry.path(), &target);
		} else {
			std::fs::copy(entry.path(), &target).expect("copies");
			std::fs::File::open(&target)
				.and_then(|file| file.sync_all())
				.expect("syncs");
		}
	}
	std::fs::File::open(to)
		.and_then(|folder| folder.sync_all())
		.expect("syncs the folder");
}

/// Opens the laptop's library, in `laptop` in the LazyFS mount `mount`, and asks again for
/// the drop of Project, which deletes it, noting the round and how far it went. Never
/// returns.
fn delete_until_killed(directory: &Path, mount: &Path, project: nibkeep_base::SpaceId) -> ! {
	laptop_drive(mount);
	let [round, _] = child::noted(directory);
	let mut library = Library::open(&mount.join("laptop"), &Unlocker::Passphrase(PASSPHRASE))
		.expect("opens the library");
	child::note(directory, &[round, 1]);
	let mut rng = TestRng::new(&format!("power loss drop child {round}"));
	let dropping = library
		.drop_space(project, common::now() + 3_600_000, &mut rng)
		.expect("drops");
	assert_eq!(dropping.stage, nibkeep_node::DropStage::Deleted);
	child::note(directory, &[round, 2]);
	loop {
		std::thread::sleep(Duration::from_secs(1));
	}
}

/// Deleting a dropped space on LazyFS, cut at a random moment: the space is then held whole,
/// as before, or gone whole, its folder and its key, and the library opens.
#[test]
fn power_cuts_while_deleting_a_dropped_space_leave_it_whole_or_gone() {
	if let (Some(directory), Some(mount), Some(project)) = (
		std::env::var_os(CHILD),
		std::env::var_os(DROP_CHILD),
		std::env::var("NIBKEEP_POWER_LOSS_DROPPED").ok(),
	) {
		let project = project.parse().expect("a space");
		delete_until_killed(&PathBuf::from(directory), &PathBuf::from(mount), project);
	}
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("drop-deleting/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "drop-deleting");
	let made = directory.join("made");
	std::fs::create_dir_all(&made).expect("makes the folder");
	laptop_drive(&made);
	let project = dropped_and_told(&made.join("laptop"), &directory.join("desk"));
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	laptop_drive(&lazyfs.mount);
	let laptop = lazyfs.mount.join("laptop");
	let mut rng = TestRng::new("power loss drop delays");
	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(ROUNDS, |rounds| rounds.parse().expect("a number"));
	let (mut whole, mut gone) = (0, 0);
	for round in 0..rounds {
		let _ = std::fs::remove_dir_all(&laptop);
		copy_synced(&made.join("laptop"), &laptop);
		child::note(&directory, &[usize::try_from(round).expect("small"), 0]);
		let mut process = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(CHILD, &directory)
			.env(DROP_CHILD, &lazyfs.mount)
			.env("NIBKEEP_POWER_LOSS_DROPPED", project.to_string())
			.stdout(Stdio::null())
			.stderr(child::errors(&directory))
			.spawn()
			.expect("starts the child");
		// Within the moment that deleting takes, once the child has opened the library.
		let delay = rng.try_next_u64().expect("infallible") % 150;
		child::kill_after_progress(
			&mut process,
			&directory,
			round,
			Duration::from_millis(delay),
		);
		lazyfs.cut_power();
		let [_, reached] = child::noted(&directory);
		let mut library = Library::open(&laptop, &Unlocker::Passphrase(PASSPHRASE))
			.unwrap_or_else(|error| panic!("round {round}: opens after the power cut: {error}"));
		let folder = common::replica_folder(&laptop)
			.join("spaces")
			.join(project.to_string());
		if library.spaces().contains(&project) {
			assert!(reached < 2, "round {round}: deleted, yet held");
			assert!(folder.join("log.sqlite").exists(), "round {round}");
			let report = library.verify(common::now() + 3_600_000).expect("verifies");
			assert!(report.is_sound(), "round {round}: {report:?}");
			whole += 1;
		} else {
			assert!(!folder.exists(), "round {round}: its folder is left");
			gone += 1;
		}
	}
	eprintln!("{whole} rounds held the space whole, {gone} had it gone");
	assert!(gone > 0, "no round deleted the space");
}
