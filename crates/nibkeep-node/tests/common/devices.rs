//! A user's devices in one process, each with a library on a disk of its own, which sync in
//! memory: for the tests of what each replica keeps and removes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_node::{
	DeviceMarks, DeviceRole, Drive, Library, NewLibrary, NewProtection, PairingCode,
	PassphraseParams, Removals, Removed, declare_drive,
};
use nibkeep_testkit::TestRng;

use super::{now, pair_named, sync_in_memory, sync_in_memory_proving};

/// Says that a folder is on the disk named `disk`.
pub(crate) fn on_disk(folder: &Path, disk: &str) {
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
pub(crate) struct Devices {
	pub(crate) libraries: Vec<Library>,
	/// The NAS's library's folder.
	pub(crate) nas: PathBuf,
	/// Each library's folder, in the order of the libraries.
	pub(crate) folders: Vec<PathBuf>,
	pub(crate) rng: TestRng,
	pub(crate) at: u64,
	name: String,
}

impl Devices {
	/// A NAS, with a library on a disk of its own, which it marks as an archive node if
	/// `archive` says so.
	pub(crate) fn new(name: &str, archive: bool) -> Self {
		let mut rng = TestRng::new(name);
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		on_disk(&directory, &format!("{name} nas"));
		let library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "nas", "nib"),
			&NewProtection::Passphrase(
				"correct horse",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now(),
			&mut rng,
		)
		.expect("creates");
		let mut devices = Self {
			libraries: vec![library],
			folders: vec![directory.clone()],
			nas: directory,
			rng,
			at: now() + 100_000,
			name: name.to_owned(),
		};
		if archive {
			devices.mark(0, DeviceRole::Archive, None);
		}
		devices
	}

	/// Pairs a new device named `device`, with its library on a disk of its own, with the
	/// NAS, and gives it a size if `keeps` gives one.
	pub(crate) fn pair(&mut self, device: &str, keeps: Option<u64>) -> usize {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, {device}", self.name),
		);
		on_disk(&directory, &format!("{} {device}", self.name));
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
		self.folders.push(directory);
		let added = self.libraries.len() - 1;
		if keeps.is_some() {
			self.mark(added, DeviceRole::Everyday, keeps);
		}
		added
	}

	pub(crate) fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	pub(crate) fn sync(&mut self, a: usize, b: usize) {
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

	pub(crate) fn personal(&self) -> SpaceId {
		self.libraries[0].personal_space()
	}

	/// Adds a file of `bytes` on device `device`; returns it and its content.
	pub(crate) fn add(&mut self, device: usize, name: &str, bytes: &[u8]) -> (EntityId, Hash) {
		let folder = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, files {device} {name}", self.name),
		);
		let path = folder.join(name);
		fs::write(&path, bytes).expect("writes");
		let at = self.tick();
		let space = self.personal();
		let added = self.libraries[device]
			.add_file(space, &path, at, &mut self.rng)
			.expect("adds");
		(added.file, added.content)
	}

	/// The device that library `library` is on.
	pub(crate) fn device_of(&self, library: usize) -> EntityId {
		self.libraries[library]
			.replicas()
			.expect("lists")
			.into_iter()
			.find(|replica| replica.this)
			.and_then(|replica| replica.device)
			.expect("a device")
	}

	/// Marks the device that library `marked` is on, from that library.
	pub(crate) fn mark(&mut self, marked: usize, role: DeviceRole, keeps: Option<u64>) {
		let device = self.device_of(marked);
		let at = self.tick();
		self.libraries[marked]
			.set_device_marks(
				device,
				DeviceMarks {
					role,
					away: false,
					keeps,
					waits: None,
				},
				at,
				&mut self.rng,
			)
			.expect("marks");
	}

	/// Whether device `device` holds the content `content` in its store.
	pub(crate) fn holds(&self, device: usize, content: &Hash) -> bool {
		self.libraries[device]
			.file_content(self.personal(), content, u64::MAX)
			.expect("reads")
			.is_some()
	}
}

/// A file of `len` bytes, different for each `seed`.
pub(crate) fn bytes(seed: u8, len: usize) -> Vec<u8> {
	(0..len)
		.map(|index| seed.wrapping_add(u8::try_from(index % 241).expect("small")))
		.collect()
}

impl Devices {
	/// Moves the clock on by `millis`.
	pub(crate) fn advance(&mut self, millis: u64) {
		self.at += millis;
	}

	/// Has every device record its endpoint, so that each finds the others "on the network",
	/// and syncs each with the NAS, twice, so that every device knows of every other.
	pub(crate) fn record_endpoints(&mut self) {
		for device in 0..self.libraries.len() {
			let at = self.tick();
			self.libraries[device]
				.record_own_endpoint(at, &mut self.rng)
				.expect("records");
		}
		for _ in 0..2 {
			for device in 1..self.libraries.len() {
				self.sync(device, 0);
			}
		}
	}

	/// The endpoints of every device but `device`, as if each were found on the network.
	pub(crate) fn others_found(&self, device: usize) -> BTreeSet<[u8; 32]> {
		self.libraries
			.iter()
			.enumerate()
			.filter(|(other, _)| *other != device)
			.map(|(_, library)| library.endpoint())
			.collect()
	}

	/// Plans a pass of removals on `device`, with every other device found.
	pub(crate) fn plan_removals(&mut self, device: usize) -> Removals {
		let at = self.tick();
		let found = self.others_found(device);
		let records = self.libraries[device]
			.sync_records(TestRng::new(&format!("records of {device} at {at}")))
			.expect("opens");
		self.libraries[device]
			.plan_removals(&records, &found, at, &mut self.rng)
			.expect("plans")
	}

	/// Syncs `device` with each device that a pass of its removals asks to prove its copies,
	/// putting the challenges to it.
	pub(crate) fn prove(&mut self, device: usize, removals: &mut Removals) {
		let provers: Vec<usize> = removals
			.provers()
			.iter()
			.map(|replica| {
				self.libraries
					.iter()
					.position(|library| Some(library.endpoint()) == replica.endpoint)
					.expect("a device")
			})
			.collect();
		for prover in provers {
			let at = self.tick();
			let (first, second) = self.two(device, prover);
			let (first, second) =
				sync_in_memory_proving(first, second, at, (device, prover), Some(&mut *removals));
			first.expect("syncs");
			second.expect("syncs");
		}
	}

	/// Removes on `device` what a pass proved.
	pub(crate) fn remove_proven(&mut self, device: usize, removals: &Removals) -> Removed {
		let at = self.tick();
		let records = self.libraries[device]
			.sync_records(TestRng::new(&format!("records of {device} at {at}")))
			.expect("opens");
		self.libraries[device]
			.remove_proven(removals, &records, at, &mut self.rng)
			.expect("removes")
	}

	/// Runs a pass of removals on `device`: plans it, has the others prove their copies, and
	/// removes what they proved.
	pub(crate) fn remove(&mut self, device: usize) -> Removed {
		let mut removals = self.plan_removals(device);
		self.prove(device, &mut removals);
		self.remove_proven(device, &removals)
	}

	/// The sealed blobs that `device` holds for others now, as it proved them.
	pub(crate) fn held_for_others(&self, device: usize) -> BTreeSet<Hash> {
		self.libraries[device]
			.sync_records(TestRng::new(&format!("holds of {device}")))
			.expect("opens")
			.holds(self.at)
			.expect("reads")
	}

	/// The libraries of two devices, at once.
	fn two(&mut self, a: usize, b: usize) -> (&mut Library, &mut Library) {
		if a < b {
			let (left, right) = self.libraries.split_at_mut(b);
			(&mut left[a], &mut right[0])
		} else {
			let (left, right) = self.libraries.split_at_mut(a);
			(&mut right[0], &mut left[b])
		}
	}
}

/// Flips a byte in the middle of every blob file under `blobs`.
pub(crate) fn damage_blobs(blobs: &Path) {
	for file in blob_files(blobs) {
		let mut data = fs::read(&file).expect("reads");
		let middle = data.len() / 2;
		data[middle] ^= 1;
		fs::write(&file, data).expect("writes");
	}
}

/// The blob files under `blobs`.
pub(crate) fn blob_files(blobs: &Path) -> Vec<PathBuf> {
	let mut files = Vec::new();
	let mut folders = vec![blobs.to_owned()];
	while let Some(folder) = folders.pop() {
		for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
			let path = entry.path();
			if path.is_dir() {
				folders.push(path);
			} else {
				files.push(path);
			}
		}
	}
	files.sort();
	files
}
