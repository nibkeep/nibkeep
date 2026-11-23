//! Checking and repair: the regular check reads every copy once in a round, a damaged sealed
//! blob comes again from another replica, and a damaged original is reported, never written
//! over ([technical design: checking and repair]).
//!
//! [technical design: checking and repair]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#97-checking-and-repair

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{now, pair_named, replica_folder, sync_in_memory};
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_node::{
	COUNTS_FOR_MILLIS, CheckRun, CopyHeld, Damaged, Drive, Library, NewLibrary, NewProtection,
	NewRule, Pace, PairingCode, PassphraseParams, Place, ROUND_MILLIS, Records, declare_drive,
};
use nibkeep_testkit::TestRng;

/// A pace that never rests, for tests.
const QUICK: Pace = Pace {
	wait: Duration::ZERO,
	rest: 0,
	run_for: Duration::from_secs(60),
};

/// Says that a folder is on the disk named `disk`.
fn on_disk(folder: &Path, disk: &str) {
	declare_drive(
		folder,
		Drive {
			serial: Some(format!("{disk} file system").into_bytes()),
			folder_ids: true,
			disk: Some(disk.as_bytes().to_vec()),
			..Drive::default()
		},
		None,
	);
}

/// A user's devices, each with a library on a disk of its own.
struct Devices {
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	rng: TestRng,
	at: u64,
	name: String,
}

impl Devices {
	/// A laptop with a library on a disk of its own.
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		on_disk(&directory, &format!("{name}, laptop"));
		let library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&NewProtection::Passphrase(
				"correct horse",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now(),
			&mut rng,
		)
		.expect("creates");
		Self {
			libraries: vec![library],
			directories: vec![directory],
			rng,
			at: now() + 100_000,
			name: name.to_owned(),
		}
	}

	/// Pairs a new device named `device`, on a disk of its own, with device 0.
	fn pair(&mut self, device: &str) -> usize {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, {device}", self.name),
		);
		on_disk(&directory, &format!("{}, {device}", self.name));
		let code = PairingCode::generate(&mut self.rng);
		let (paired, joined) = pair_named(
			&mut self.libraries[0],
			&directory,
			device,
			(&code, &code),
			&mut self.rng,
		);
		paired.expect("paired");
		self.libraries.push(joined.expect("joined").library);
		self.directories.push(directory);
		self.libraries.len() - 1
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn sync(&mut self, a: usize, b: usize) {
		let at = self.tick();
		let (first, second) = if a < b {
			let (left, right) = self.libraries.split_at_mut(b);
			(&mut left[a], &mut right[0])
		} else {
			let (left, right) = self.libraries.split_at_mut(a);
			(&mut right[0], &mut left[b])
		};
		let (first, second) = sync_in_memory(first, second, at, (a, b));
		first.expect("syncs");
		second.expect("syncs");
	}

	fn personal(&self) -> SpaceId {
		self.libraries[0].personal_space()
	}

	/// Adds a file of `bytes` on device `device`, and says where it was found.
	fn add_file(&mut self, device: usize, name: &str, bytes: &[u8]) -> (EntityId, PathBuf) {
		let folder = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, files {device} {name}", self.name),
		);
		let path = folder.join(name);
		fs::write(&path, bytes).expect("writes");
		let at = self.tick();
		let space = self.personal();
		let file = self.libraries[device]
			.add_file(space, &path, at, &mut self.rng)
			.expect("adds")
			.file;
		(file, path)
	}

	/// Keeps the files that device 0 adds from now on where they are found.
	fn keep_in_place(&mut self) {
		let space = self.personal();
		let at = self.tick();
		self.libraries[0]
			.install_policies(space, None, at, &mut self.rng)
			.expect("installs");
		let replaceable = self.libraries[0]
			.find_policy(space, "Replaceable")
			.expect("Replaceable");
		let at = self.tick();
		self.libraries[0]
			.add_rule(
				space,
				&NewRule::new(replaceable),
				Place::First,
				at,
				&mut self.rng,
			)
			.expect("adds a rule");
	}

	fn records(&self, device: usize) -> Records {
		self.libraries[device]
			.sync_records(TestRng::new("records"))
			.expect("records")
	}

	/// Runs the regular check on device `device` at `at`.
	fn check_at(&mut self, device: usize, at: u64) -> CheckRun {
		let records = self.records(device);
		self.libraries[device]
			.check_copies(&records, &|| at, QUICK, &|| false)
			.expect("checks")
	}

	/// Runs a whole round of the regular check on device `device`: at the end of the round
	/// under way, or of one that begins a round's time ago, so that everything is due.
	fn check_all(&mut self, device: usize) -> CheckRun {
		let at = self.tick();
		let (began, mut run) = match self.records(device).round().expect("reads") {
			Some(round) => (round.began, CheckRun::default()),
			None => (at - ROUND_MILLIS, self.check_at(device, at - ROUND_MILLIS)),
		};
		let done = self.check_at(device, at.max(began + ROUND_MILLIS));
		run.blobs += done.blobs;
		run.plain += done.plain;
		run.damaged += done.damaged;
		run
	}

	/// The files of the sealed blobs that device `device` holds, in the order of their
	/// sealed hashes.
	fn blob_files(&self, device: usize) -> Vec<(Hash, PathBuf)> {
		let mut files = Vec::new();
		let replica = replica_folder(&self.directories[device]);
		let root = nibkeep_store::blob_directory(&replica);
		for byte in 0..=255_u8 {
			for (sealed, _) in
				nibkeep_store::blobs_starting_with(&replica, &Hash::of(b""), byte).expect("lists")
			{
				let digest = sealed.digest().iter().fold(String::new(), |mut hex, byte| {
					let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
					hex
				});
				let path = root
					.join(sealed.algorithm().name())
					.join(&digest[..2])
					.join(&digest);
				files.push((sealed, path));
			}
		}
		files
	}

	/// How many disks hold copies of a file that count, as device `device` counts at `at`.
	fn counting(&self, device: usize, file: EntityId, at: u64) -> usize {
		self.libraries[device]
			.copies_of(self.personal(), file, at)
			.expect("counts")
			.iter()
			.filter(|copy| copy.counts)
			.count()
	}
}

/// Flips one byte in the middle of a file, keeping its size and modification time.
fn damage(path: &Path) {
	let modified = fs::metadata(path)
		.and_then(|metadata| metadata.modified())
		.expect("reads");
	let mut bytes = fs::read(path).expect("reads");
	let middle = bytes.len() / 2;
	bytes[middle] ^= 0x20;
	fs::write(path, &bytes).expect("writes");
	fs::File::options()
		.write(true)
		.open(path)
		.and_then(|file| file.set_modified(modified))
		.expect("keeps the time");
}

#[test]
fn a_damaged_blob_is_moved_aside_and_comes_again_checked_from_another_replica() {
	let mut devices = Devices::new("checking, comes again");
	let (file, _) = devices.add_file(0, "first steps.mp4", &vec![7; 70_000]);
	let desk = devices.pair("desk");
	devices.sync(desk, 0);
	let blobs = devices.blob_files(desk);
	assert_eq!(blobs.len(), 1, "the desk received the film");
	let (sealed, path) = blobs[0].clone();
	// Nothing is wrong yet: a whole round reads the film and finds it intact.
	let run = devices.check_all(desk);
	assert_eq!((run.blobs, run.damaged), (1, 0));
	// A bit flips on the desk's disk; the next round finds it, and moves the copy aside.
	damage(&path);
	let run = devices.check_all(desk);
	assert_eq!((run.blobs, run.damaged), (1, 1));
	assert!(!path.exists(), "moved aside");
	let status = devices.libraries[desk]
		.check_status(&devices.records(desk), devices.at)
		.expect("reads");
	assert_eq!(status.damaged.len(), 1);
	assert_eq!(status.damaged[0].files, [file]);
	assert_eq!(
		status.damaged[0].copy,
		Damaged::Sealed {
			sealed,
			replaced: false
		}
	);
	// The desk no longer counts as holding the film, and says so.
	let at = devices.tick();
	assert_eq!(devices.counting(desk, file, at), 1, "the laptop's alone");
	// The next sync brings it again from the laptop, checked before it is put in place.
	devices.sync(desk, 0);
	assert!(path.exists(), "back in place");
	assert_eq!(
		blake3::hash(&fs::read(&path).expect("reads")).as_bytes(),
		sealed.digest()
	);
	let status = devices.libraries[desk]
		.check_status(&devices.records(desk), devices.at)
		.expect("reads");
	assert_eq!(
		status.damaged[0].copy,
		Damaged::Sealed {
			sealed,
			replaced: true
		}
	);
	let at = devices.tick();
	assert_eq!(devices.counting(desk, file, at), 2);
	// The laptop learns of the damage the desk found, at the desk's next syncs.
	devices.at += 60_000;
	devices.sync(desk, 0);
	let at = devices.tick();
	let check = devices.libraries[0]
		.check_policies(devices.personal(), at)
		.expect("checks");
	let desks = check
		.disks
		.iter()
		.find(|disk| !disk.here)
		.expect("the desk's disk");
	assert_eq!((desks.damaged, desks.unrepaired), (1, 0));
}

#[test]
fn a_copy_damaged_on_another_replica_too_is_refused_and_an_intact_one_taken() {
	let mut devices = Devices::new("checking, damaged twice");
	devices.add_file(0, "wedding.mov", &vec![3; 90_000]);
	let desk = devices.pair("desk");
	let nas = devices.pair("nas");
	devices.sync(desk, 0);
	devices.sync(nas, 0);
	let (sealed, desks) = devices.blob_files(desk)[0].clone();
	let (_, laptops) = devices.blob_files(0)[0].clone();
	// Both the desk's copy and the laptop's are damaged; only the desk has checked yet.
	damage(&desks);
	damage(&laptops);
	assert_eq!(devices.check_all(desk).damaged, 1);
	// The laptop still offers its copy, which the desk refuses.
	devices.sync(desk, 0);
	assert!(!desks.exists(), "the laptop's damaged copy is refused");
	// The NAS's copy is intact, and taken.
	devices.sync(desk, nas);
	assert!(desks.exists());
	assert_eq!(
		blake3::hash(&fs::read(&desks).expect("reads")).as_bytes(),
		sealed.digest()
	);
}

#[test]
fn an_original_found_damaged_is_reported_and_left_as_it_is() {
	let mut devices = Devices::new("checking, originals");
	devices.keep_in_place();
	let (photo, damaged_path) = devices.add_file(0, "photo.jpg", &vec![5; 30_000]);
	let (notes, edited_path) = devices.add_file(0, "notes.txt", &vec![6; 20_000]);
	let at = devices.tick();
	assert_eq!(devices.counting(0, photo, at), 1);
	// One original's bytes change under the same size and time: damage. The other is
	// edited, as another app would: no damage, but no copy any more either.
	damage(&damaged_path);
	let damaged_bytes = fs::read(&damaged_path).expect("reads");
	fs::write(&edited_path, vec![6; 20_001]).expect("edits");
	let run = devices.check_all(0);
	assert_eq!(
		(run.plain, run.damaged),
		(1, 1),
		"the edited one is not read"
	);
	// The damaged one is reported, stops counting, and stays as it is.
	assert_eq!(fs::read(&damaged_path).expect("reads"), damaged_bytes);
	let status = devices.libraries[0]
		.check_status(&devices.records(0), devices.at)
		.expect("reads");
	assert_eq!(status.damaged.len(), 1);
	assert_eq!(status.damaged[0].files, [photo]);
	assert_eq!(
		status.damaged[0].copy,
		Damaged::Plain {
			path: damaged_path.to_string_lossy().into_owned()
		}
	);
	let at = devices.tick();
	assert_eq!(devices.counting(0, photo, at), 0);
	assert_eq!(devices.counting(0, notes, at), 0);
	// The laptop's account says how many it found damaged.
	let check = devices.libraries[0]
		.check_policies(devices.personal(), at)
		.expect("checks");
	assert_eq!((check.disks[0].damaged, check.disks[0].unrepaired), (1, 1));
}

#[test]
fn a_round_reads_a_share_at_a_time_in_the_order_of_sealed_hashes() {
	let mut devices = Devices::new("checking, a round");
	let mut files = Vec::new();
	for n in 0..8_u8 {
		files.push(
			devices
				.add_file(0, &format!("scan {n}.png"), &[n; 10_000])
				.0,
		);
	}
	let sealed: Vec<Hash> = devices
		.blob_files(0)
		.into_iter()
		.map(|(sealed, _)| sealed)
		.collect();
	assert_eq!(sealed.len(), 8);
	// A round begins: nothing is due yet.
	let began = devices.tick();
	assert_eq!(devices.check_at(0, began).blobs, 0);
	// Half way through it, half the bytes are read, the smallest sealed hashes first.
	let half = began + ROUND_MILLIS / 2;
	assert_eq!(devices.check_at(0, half).blobs, 4);
	let round = devices.records(0).round().expect("reads").expect("a round");
	assert_eq!(round.blobs.after, Some(sealed[3].to_multihash()));
	let checked_then = files
		.iter()
		.filter(|file| {
			devices.libraries[0]
				.copies_of(devices.personal(), **file, half)
				.expect("counts")
				.iter()
				.any(|copy| {
					copy.held
						== CopyHeld::Sealed {
							checked: Some(half),
						}
				})
		})
		.count();
	assert_eq!(checked_then, 4);
	// A device that was off catches up once the round's time is up, and a new round
	// begins once every copy was read.
	let late = began + ROUND_MILLIS + 5 * 86_400_000;
	assert_eq!(devices.check_at(0, late).blobs, 4);
	let round = devices.records(0).round().expect("reads").expect("a round");
	assert_eq!(round.began, late);
	assert!(!round.blobs.done);
}

#[test]
fn copies_not_checked_in_eight_weeks_do_not_count() {
	let mut devices = Devices::new("checking, eight weeks");
	let (file, _) = devices.add_file(0, "letters.pdf", &vec![2; 15_000]);
	let desk = devices.pair("desk");
	devices.sync(desk, 0);
	devices.at += 60_000;
	devices.sync(desk, 0);
	let at = devices.tick();
	assert_eq!(devices.counting(0, file, at), 2);
	// Eight weeks on, neither copy was checked since: neither counts.
	let later = at + COUNTS_FOR_MILLIS + 1_000;
	assert_eq!(devices.counting(0, file, later), 0);
	let check = devices.libraries[0]
		.check_policies(devices.personal(), later)
		.expect("checks");
	let unchecked: usize = check.policies.iter().map(|count| count.unchecked).sum();
	assert_eq!(unchecked, 1);
	let copies = devices.libraries[0]
		.copies_of(devices.personal(), file, later)
		.expect("counts");
	assert!(
		copies
			.iter()
			.all(|copy| !copy.counts && matches!(copy.held, CopyHeld::Sealed { checked: Some(_) }))
	);
	// The laptop checks its own copy, which counts again; the desk's counts once the desk
	// has checked it and said so.
	devices.at = later;
	devices.check_all(0);
	assert_eq!(devices.counting(0, file, devices.at), 1);
	devices.check_all(desk);
	devices.sync(desk, 0);
	assert_eq!(devices.counting(0, file, devices.at), 2);
}

#[test]
fn a_copy_moved_aside_without_a_record_is_recorded_at_the_next_check() {
	let mut devices = Devices::new("checking, moved without a record");
	let (file, _) = devices.add_file(0, "map.tif", &vec![1; 25_000]);
	let (sealed, path) = devices.blob_files(0)[0].clone();
	// As if a crash came between moving the copy aside and recording it.
	let digest = path.file_name().expect("a name").to_owned();
	let aside = replica_folder(&devices.directories[0])
		.join("damaged")
		.join(sealed.algorithm().name());
	fs::create_dir_all(&aside).expect("makes the folder");
	damage(&path);
	fs::rename(&path, aside.join(digest)).expect("moves");
	let at = devices.tick();
	devices.check_at(0, at);
	let status = devices.libraries[0]
		.check_status(&devices.records(0), devices.at)
		.expect("reads");
	assert_eq!(status.damaged.len(), 1);
	assert_eq!(status.damaged[0].files, [file]);
	assert_eq!(status.damaged[0].found, at);
	assert_eq!(
		status.damaged[0].copy,
		Damaged::Sealed {
			sealed,
			replaced: false
		}
	);
}
