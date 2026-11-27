//! Scenario S14 of the design guidelines, films and memories: a film ripped from a disc is
//! registered where it is and kept as a single copy; a large video of a child's first steps
//! gets two copies, one of them on the NAS; and a changed password reaches three devices,
//! one of them outside the home ([design: S14], [technical design: storage policies]). Until
//! relays come, the device outside the home is one kept away from home, which syncs when it
//! comes back, as the Plan says.
//!
//! [design: S14]: https://github.com/nibkeep/nibkeep/blob/main/docs/Design.md#5-scenarios
//! [technical design: storage policies]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#93-storage-policies

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{now, pair_joining, sync_in_memory, this_day, today};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{Name, Value, fields};
use nibkeep_node::{
	AddedFile, CopyHeld, DeviceMarks, DeviceRole, Drive, Edit, Joining, Kept, Library, NewLibrary,
	NewProtection, NewRule, PairingCode, PassphraseParams, Place, declare_drive,
};
use nibkeep_std::{LOGIN, PASSWORD, USERNAME};
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::Video;

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

fn secrets(passphrase: &'static str) -> NewProtection<'static> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// A household's devices, each with a library on a disk of its own and the secrets.
struct Home {
	libraries: Vec<Library>,
	rng: TestRng,
	at: u64,
}

impl Home {
	/// A laptop, with the secrets set up.
	fn new() -> Self {
		let mut rng = TestRng::new("s14");
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "s14");
		on_disk(&directory, "s14 laptop");
		let mut library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&secrets("correct horse"),
			now(),
			&mut rng,
		)
		.expect("creates");
		library
			.set_up_secrets(&secrets("tr0ub4dor"), now() + 500, &mut rng)
			.expect("sets up");
		Self {
			libraries: vec![library],
			rng,
			at: now() + 100_000,
		}
	}

	/// Pairs a device named `device` with the laptop, giving it the secrets.
	fn pair(&mut self, device: &'static str) -> usize {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("s14, {device}"),
		);
		on_disk(&directory, &format!("s14 {device}"));
		let code = PairingCode::generate(&mut self.rng);
		let joining = Joining::new(device, "nib", &mut self.rng)
			.expect("names")
			.asking_for_secrets(secrets(device), &mut self.rng);
		let (paired, joined) = pair_joining(
			&mut self.libraries[0],
			&directory,
			(joining, true),
			(&code, &code),
			&mut self.rng,
		);
		paired.expect("paired");
		self.libraries.push(joined.expect("joined").library);
		let new = self.libraries.len() - 1;
		self.sync(0, new);
		new
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

	/// Marks the device of library `marked`, from the laptop.
	fn mark(&mut self, marked: usize, marks: DeviceMarks) {
		let device = self.libraries[marked]
			.replicas()
			.expect("lists")
			.into_iter()
			.find(|replica| replica.this)
			.and_then(|replica| replica.device)
			.expect("a device");
		let at = self.tick();
		self.libraries[0]
			.set_device_marks(device, marks, at, &mut self.rng)
			.expect("marks");
	}

	/// Adds a file on the laptop, in a folder of its own.
	fn add(&mut self, path: &Path, bytes: &[u8]) -> AddedFile {
		fs::create_dir_all(path.parent().expect("a folder")).expect("makes the folder");
		fs::write(path, bytes).expect("writes");
		let at = self.tick();
		let space = self.personal();
		self.libraries[0]
			.add_file(space, path, at, &mut self.rng)
			.expect("adds")
	}

	/// The password that library `device` shows of a login.
	fn password(&self, device: usize, login: EntityId) -> Vec<Value> {
		self.libraries[device]
			.entity(self.personal(), login, this_day())
			.expect("reads")
			.expect("the login")
			.fields
			.into_iter()
			.filter(|shown| shown.field == PASSWORD)
			.flat_map(|shown| shown.values.into_iter().map(|value| value.value))
			.collect()
	}
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// The household: the laptop, a NAS marked as an archive node, and a phone kept away from
/// home, with the default policies.
fn household() -> (Home, usize, usize) {
	let mut home = Home::new();
	let space = home.personal();
	let nas = home.pair("nas");
	let phone = home.pair("phone");
	home.mark(
		nas,
		DeviceMarks {
			role: DeviceRole::Archive,
			..DeviceMarks::default()
		},
	);
	home.mark(
		phone,
		DeviceMarks {
			away: true,
			..DeviceMarks::default()
		},
	);
	let at = home.tick();
	home.libraries[0]
		.install_policies(space, None, at, &mut home.rng)
		.expect("installs");
	(home, nas, phone)
}

#[test]
fn s14_films_and_memories() {
	let (mut home, nas, phone) = household();
	let space = home.personal();
	let files: PathBuf =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "s14, files");

	// The film's folder holds what can be got again, as the person answered when NibKeep
	// asked of the first film: its rule chooses Replaceable, one copy, kept in place.
	let replaceable = home.libraries[0]
		.find_policy(space, "Replaceable")
		.expect("Replaceable");
	fs::create_dir_all(files.join("Films")).expect("makes the folder");
	let at = home.tick();
	home.libraries[0]
		.add_rule(
			space,
			&NewRule {
				folder: Some(files.join("Films")),
				..NewRule::new(replaceable)
			},
			Place::First,
			at,
			&mut home.rng,
		)
		.expect("adds the rule");
	// Bytes standing for 40 GB.
	let film = home.add(&files.join("Films/The film.mkv"), &vec![7; 64_000]);
	assert_eq!(film.kept, Kept::InPlace, "registered where it is");
	assert_eq!(
		home.libraries[0]
			.policy_of(space, film.file)
			.expect("chooses")
			.policy,
		Some(replaceable)
	);

	// The first steps, in 4K: Standard, as any other file, so two copies, one archived.
	let video = Video {
		created: Some(1_623_000_000),
		duration_millis: 95_000,
		size: (3840, 2160),
	};
	let first_steps = home.add(&files.join("Videos/first steps.mp4"), &video.mp4());
	assert_eq!(first_steps.kept, Kept::Store);
	let standard = home.libraries[0]
		.find_policy(space, "Standard")
		.expect("Standard");
	assert_eq!(
		home.libraries[0]
			.policy_of(space, first_steps.file)
			.expect("chooses")
			.policy,
		Some(standard)
	);

	// The NAS takes what it keeps, and the accounts pass round.
	for _ in 0..2 {
		home.sync(0, nas);
		home.sync(nas, phone);
	}
	assert!(
		home.libraries[nas]
			.copies_of(space, first_steps.file, home.at)
			.expect("counts")
			.iter()
			.any(|copy| copy.disk.here && matches!(copy.held, CopyHeld::Sealed { .. })),
		"the NAS holds the video"
	);
	let copies = home.libraries[0]
		.copies_of(space, first_steps.file, home.at)
		.expect("counts");
	assert_eq!(copies.iter().filter(|copy| copy.counts).count(), 2);
	assert!(copies.iter().any(|copy| copy.counts && copy.disk.archive));
	// The film is on the laptop's disk alone, where it was found, and nowhere sealed.
	let copies = home.libraries[0]
		.copies_of(space, film.file, home.at)
		.expect("counts");
	assert_eq!(copies.len(), 1);
	assert!(matches!(copies[0].held, CopyHeld::InPlace { .. }));
	let check = home.libraries[0]
		.check_policies(space, home.at)
		.expect("checks");
	assert_eq!(
		check
			.policies
			.iter()
			.map(|policy| policy.short)
			.sum::<usize>(),
		0,
		"{check:?}"
	);
	a_changed_password_reaches_every_device(&mut home, (nas, phone));
}

/// Checks that a password changed on the laptop reaches the NAS, and the phone away from
/// home once it syncs, and that the space's changes are on three disks, one of them away.
fn a_changed_password_reaches_every_device(home: &mut Home, (nas, phone): (usize, usize)) {
	let space = home.personal();
	let at = home.tick();
	let login = home.libraries[0]
		.create_entity(
			space,
			&[LOGIN],
			vec![
				(fields::NAME, Value::Name(Name::new("Films at home"))),
				(USERNAME, text("ada@example.com")),
				(PASSWORD, text("first-s3cret")),
			],
			at,
			&mut home.rng,
		)
		.expect("creates");
	home.sync(0, nas);
	home.sync(nas, phone);
	let at = home.tick();
	home.libraries[0]
		.edit_entity(
			space,
			login,
			vec![Edit::Set(PASSWORD, vec![text("second-s3cret")])],
			&today(),
			at,
			&mut home.rng,
		)
		.expect("changes the password");
	home.sync(0, nas);
	home.sync(nas, phone);
	home.sync(0, phone);
	for device in [0, nas, phone] {
		assert_eq!(
			home.password(device, login),
			[text("second-s3cret")],
			"device {device}"
		);
	}
	let changes = home.libraries[0]
		.check_policies(space, home.at)
		.expect("checks")
		.changes;
	assert_eq!((changes.disks, changes.away), (3, 1), "{changes:?}");
}
