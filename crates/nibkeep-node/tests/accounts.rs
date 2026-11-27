//! Where the copies are: each replica's account of what it holds, passed on by syncs, and
//! copies counted by disk across devices ([technical design: where the copies are]).
//!
//! [technical design: where the copies are]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#96-where-the-copies-are

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{now, pair_named, sync_in_memory};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_node::{
	CopyHeld, DeviceMarks, DeviceRole, Drive, Library, NewLibrary, NewProtection, NewRule,
	PairingCode, PassphraseParams, Place, PolicyCheck, Syncing, declare_drive,
};
use nibkeep_sync::Holder as _;
use nibkeep_testkit::TestRng;

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

/// A user's devices, each with a library on a disk of its own unless said otherwise.
struct Devices {
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	rng: TestRng,
	at: u64,
	name: String,
}

impl Devices {
	/// A laptop with a library on the disk `disk`.
	fn new(name: &str, disk: &str) -> Self {
		let mut rng = TestRng::new(name);
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		on_disk(&directory, disk);
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

	/// Pairs a new device named `device`, with its library on `disk`, with device `sponsor`.
	fn pair(&mut self, sponsor: usize, device: &str, disk: &str) -> usize {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, {device}", self.name),
		);
		on_disk(&directory, disk);
		let code = PairingCode::generate(&mut self.rng);
		let (paired, joined) = pair_named(
			&mut self.libraries[sponsor],
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

	/// Adds a file of `bytes` on device `device`.
	fn add_file(&mut self, device: usize, name: &str, bytes: &[u8]) -> EntityId {
		self.add_file_at(device, name, bytes).0
	}

	/// Adds a file of `bytes` on device `device`, and says where it is.
	fn add_file_at(&mut self, device: usize, name: &str, bytes: &[u8]) -> (EntityId, PathBuf) {
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

	fn check(&self, device: usize) -> PolicyCheck {
		self.libraries[device]
			.check_policies(self.personal(), self.at)
			.expect("checks")
	}

	/// The device that library `device` is on, as its library names it.
	fn device_of(&self, device: usize) -> EntityId {
		self.libraries[device]
			.replicas()
			.expect("lists")
			.into_iter()
			.find(|replica| replica.this)
			.and_then(|replica| replica.device)
			.expect("a device")
	}

	/// Marks the device that library `marked` is on, from library `from`.
	fn mark(&mut self, from: usize, marked: usize, marks: DeviceMarks) {
		let device = self.device_of(marked);
		let at = self.tick();
		self.libraries[from]
			.set_device_marks(device, marks, at, &mut self.rng)
			.expect("marks");
	}

	/// How many disks hold each file, as device `device` counts, by file.
	fn disks_of(&self, device: usize, file: EntityId) -> usize {
		self.libraries[device]
			.copies_of(self.personal(), file, self.at)
			.expect("counts")
			.len()
	}
}

/// How many files of each policy lack copies, all together.
fn short(check: &PolicyCheck) -> usize {
	check.policies.iter().map(|policy| policy.short).sum()
}

#[test]
fn copies_count_on_each_disk_that_holds_them_once_its_account_came() {
	let mut devices = Devices::new("accounts, two disks", "laptop's drive");
	let file = devices.add_file(0, "first steps.mp4", &vec![7; 50_000]);
	// Alone, the laptop holds the one copy.
	assert_eq!(devices.disks_of(0, file), 1);
	let desk = devices.pair(0, "desk", "desk's drive");
	// The desk holds the file once pairing brought it, and says so at its next sync.
	devices.sync(desk, 0);
	assert_eq!(devices.disks_of(0, file), 2);
	assert_eq!(devices.disks_of(desk, file), 2);
	let check = devices.check(0);
	assert_eq!(check.disks.len(), 2);
	assert_eq!(check.disks.iter().filter(|disk| disk.here).count(), 1);
	// Standard wants one of its two copies on an archive node, which no device is yet.
	let standard = check
		.policies
		.iter()
		.find(|policy| policy.name.as_deref() == Some("Standard"))
		.expect("Standard");
	assert_eq!(
		(standard.files, standard.few_copies, standard.few_archived),
		(1, 0, 1)
	);
	assert_eq!(standard.lacking, [file]);
	let copies = devices.libraries[0]
		.copies_of(devices.libraries[0].personal_space(), file, now())
		.expect("copies");
	let lacks = standard.needs.lacks_of(&copies);
	assert!(!lacks.copies && !lacks.away && lacks.archived && lacks.any());
	// Once the desk is an archive node, and says so, nothing lacks copies.
	devices.mark(
		0,
		desk,
		DeviceMarks {
			role: DeviceRole::Archive,
			away: false,
			keeps: None,
			waits: None,
		},
	);
	devices.sync(0, desk);
	devices.sync(desk, 0);
	assert_eq!(short(&devices.check(0)), 0);
	let check = devices.check(0);
	let archive = check
		.disks
		.iter()
		.find(|disk| !disk.here)
		.expect("the desk's disk");
	assert!(archive.archive && !archive.away);
	assert!(
		archive
			.replicas
			.iter()
			.any(|replica| replica.device.as_deref() == Some("desk"))
	);
}

#[test]
fn replicas_on_one_disk_count_as_one_copy() {
	let mut devices = Devices::new("accounts, one disk", "the family computer's drive");
	let file = devices.add_file(0, "scan.pdf", &vec![3; 20_000]);
	let other = devices.pair(0, "family computer", "the family computer's drive");
	devices.sync(other, 0);
	assert_eq!(devices.disks_of(0, file), 1);
	assert_eq!(devices.check(0).disks.len(), 1);
	assert_eq!(devices.check(0).disks[0].replicas.len(), 2);
}

#[test]
fn accounts_pass_through_replicas_that_never_meet() {
	let mut devices = Devices::new("accounts, passed on", "laptop's drive");
	let file = devices.add_file(0, "photo.jpg", &vec![5; 30_000]);
	let desk = devices.pair(0, "desk", "desk's drive");
	let phone = devices.pair(desk, "phone", "phone's storage");
	// The phone holds the file, and tells the desk; the desk tells the laptop.
	devices.sync(phone, desk);
	devices.sync(desk, 0);
	assert_eq!(devices.disks_of(0, file), 3);
	// And the laptop's account reaches the phone through the desk.
	devices.sync(0, desk);
	devices.sync(desk, phone);
	assert_eq!(devices.disks_of(phone, file), 3);
}

#[test]
fn a_revoked_devices_copies_stop_counting() {
	let mut devices = Devices::new("accounts, revoked", "laptop's drive");
	let file = devices.add_file(0, "letter.pdf", &vec![9; 10_000]);
	let phone = devices.pair(0, "phone", "phone's storage");
	devices.sync(phone, 0);
	assert_eq!(devices.disks_of(0, file), 2);
	let replica = devices.libraries[0]
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.device_name.as_deref() == Some("phone"))
		.expect("the phone")
		.id;
	let at = devices.tick();
	devices.libraries[0]
		.revoke(&[replica], at, &mut devices.rng)
		.expect("revokes");
	assert_eq!(devices.disks_of(0, file), 1);
}

#[test]
fn a_page_that_its_replica_did_not_sign_or_that_is_older_is_refused() {
	let mut devices = Devices::new("accounts, forged", "laptop's drive");
	let file = devices.add_file(0, "deed.pdf", &vec![1; 10_000]);
	let desk = devices.pair(0, "desk", "desk's drive");
	devices.sync(desk, 0);
	let space = devices.personal();
	let now = move || now() + 1_000_000;
	// A page signed by a key that the space does not list counts for nothing.
	let stranger = nibkeep_crypto::SigningKey::generate(&mut devices.rng);
	let keys = nibkeep_log::SpaceKeys::new(0, nibkeep_crypto::Key::generate(&mut devices.rng));
	let page = nibkeep_log::AccountPage::issue(
		space,
		&keys,
		&stranger,
		(
			now(),
			&nibkeep_log::PageBody::Head(nibkeep_log::Head {
				disk: nibkeep_base::Hash::of(b"a disk of nobody's"),
				role: DeviceRole::Archive,
				away: true,
				heads: Vec::new(),
				damaged: 0,
				unrepaired: 0,
				waits: 0,
			}),
		),
		&mut devices.rng,
	);
	let kept = Syncing::new(&mut devices.libraries[0], &now)
		.receive_account(space, page.to_bytes())
		.expect("takes");
	assert!(!kept);
	// Nor does a page whose bytes were changed, so that its signature no longer checks.
	let mut changed = page.to_bytes();
	let last = changed.len() - 1;
	changed[last] ^= 1;
	let kept = Syncing::new(&mut devices.libraries[0], &now)
		.receive_account(space, changed)
		.expect("takes");
	assert!(!kept);
	assert_eq!(devices.disks_of(0, file), 2);
	assert_eq!(devices.check(0).disks.len(), 2);
	// The desk's head, as the laptop keeps it now.
	let desk_key = devices.libraries[0]
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.device_name.as_deref() == Some("desk"))
		.and_then(|replica| replica.signing)
		.expect("the desk's key");
	let desk_head = |library: &mut Library| -> Vec<u8> {
		let syncing = Syncing::new(library, &now);
		syncing
			.holdings(space)
			.expect("holds")
			.into_iter()
			.filter_map(|holding| match holding {
				nibkeep_sync::Holding::Account { id, .. } => {
					syncing.account(space, &id).expect("reads")
				}
				_ => None,
			})
			.find(|bytes| {
				nibkeep_log::AccountPage::from_bytes(bytes).is_ok_and(|page| {
					*page.author() == desk_key && page.kind() == nibkeep_log::PageKind::Head
				})
			})
			.expect("the desk's head")
	};
	let older = desk_head(&mut devices.libraries[0]);
	// Once the desk is an archive node, and its newer head came, the older one sent again
	// is refused, and the newer one counts.
	devices.mark(
		0,
		desk,
		DeviceMarks {
			role: DeviceRole::Archive,
			away: false,
			keeps: None,
			waits: None,
		},
	);
	devices.sync(0, desk);
	devices.sync(desk, 0);
	assert_ne!(desk_head(&mut devices.libraries[0]), older);
	let kept = Syncing::new(&mut devices.libraries[0], &now)
		.receive_account(space, older)
		.expect("takes");
	assert!(!kept);
	assert!(devices.check(0).disks.iter().any(|disk| disk.archive));
}

#[test]
fn pages_are_issued_again_only_when_what_they_tell_changes() {
	let mut devices = Devices::new("accounts, issued once", "laptop's drive");
	devices.add_file(0, "song.flac", &vec![4; 10_000]);
	let desk = devices.pair(0, "desk", "desk's drive");
	devices.sync(desk, 0);
	let issued = |devices: &Devices, device: usize| -> Vec<Option<u64>> {
		devices
			.check(device)
			.disks
			.iter()
			.map(|disk| disk.issued)
			.collect()
	};
	let before = issued(&devices, 0);
	devices.sync(desk, 0);
	devices.sync(0, desk);
	assert_eq!(issued(&devices, 0), before);
	// A new file changes what both hold, once they synced.
	devices.add_file(desk, "another.flac", &vec![6; 10_000]);
	devices.sync(desk, 0);
	devices.sync(0, desk);
	assert_ne!(issued(&devices, desk), before);
}

#[test]
fn changes_held_here_alone_are_counted() {
	let mut devices = Devices::new("accounts, changes", "laptop's drive");
	devices.add_file(0, "a.txt", b"one");
	let desk = devices.pair(0, "desk", "desk's drive");
	devices.sync(desk, 0);
	let changes = devices.check(0).changes;
	assert_eq!((changes.disks, changes.here_alone), (2, 0));
	// Changes the desk has not received yet are on the laptop's disk alone.
	devices.add_file(0, "b.txt", b"two");
	devices.add_file(0, "c.txt", b"three");
	let changes = devices.check(0).changes;
	assert_eq!(changes.disks, 1);
	assert!(changes.here_alone >= 2);
	assert!(!changes.here_alone_more);
	devices.sync(0, desk);
	devices.sync(desk, 0);
	let changes = devices.check(0).changes;
	assert_eq!((changes.disks, changes.here_alone), (2, 0));
	// The space's changes count against Critical: three disks, one of them away.
	assert_eq!((changes.needs.copies, changes.needs.away), (3, 1));
	devices.mark(
		0,
		desk,
		DeviceMarks {
			role: DeviceRole::Everyday,
			away: true,
			keeps: None,
			waits: None,
		},
	);
	devices.sync(0, desk);
	devices.sync(desk, 0);
	assert_eq!(devices.check(0).changes.away, 1);
}

#[test]
fn an_original_kept_in_place_counts_on_the_disk_of_its_device() {
	let mut devices = Devices::new("accounts, in place", "laptop's drive");
	let space = devices.personal();
	let at = devices.tick();
	devices.libraries[0]
		.install_policies(space, None, at, &mut devices.rng)
		.expect("installs");
	let replaceable = devices.libraries[0]
		.find_policy(space, "Replaceable")
		.expect("Replaceable");
	let at = devices.tick();
	devices.libraries[0]
		.add_rule(
			space,
			&NewRule::new(replaceable),
			Place::First,
			at,
			&mut devices.rng,
		)
		.expect("adds a rule");
	let (film, path) = devices.add_file_at(0, "film.mkv", &vec![8; 40_000]);
	let desk = devices.pair(0, "desk", "desk's drive");
	devices.sync(0, desk);
	// The desk holds no copy: the film is on the laptop's disk alone, where it was found.
	let copies = devices.libraries[desk]
		.copies_of(space, film, devices.at)
		.expect("counts");
	assert_eq!(copies.len(), 1);
	assert!(matches!(
		copies[0].held,
		CopyHeld::InPlace { checked: Some(_) }
	));
	assert!(copies[0].counts);
	assert!(!copies[0].disk.here);
	assert_eq!(short(&devices.check(desk)), 0);
	// Changed where it was found, it is no copy any more, once the laptop says so.
	let original = devices.libraries[0]
		.copies_of(space, film, devices.at)
		.expect("counts");
	assert!(matches!(original[0].held, CopyHeld::InPlace { .. }));
	fs::write(&path, vec![9; 41_000]).expect("changes the film");
	assert_eq!(devices.disks_of(0, film), 0);
	// Nothing in the space changed, so the laptop's account stands for a minute.
	devices.sync(0, desk);
	assert_eq!(devices.disks_of(desk, film), 1);
	devices.at += 60_000;
	devices.sync(0, desk);
	assert_eq!(devices.disks_of(desk, film), 0);
}

#[test]
fn a_page_is_refused_from_a_peer_for_a_space_it_is_not_of() {
	let mut devices = Devices::new("accounts, other space", "laptop's drive");
	let space = devices.personal();
	let at = devices.tick();
	let other = devices.libraries[0]
		.create_space("Other", at, &mut devices.rng)
		.expect("makes a space");
	let desk = devices.pair(0, "desk", "desk's drive");
	devices.sync(desk, 0);
	// A page of the personal space offered as the other space's is not the other's.
	let stored = devices.libraries[desk]
		.check_policies(space, devices.at)
		.expect("checks");
	assert_eq!(stored.disks.len(), 2);
	let now = move || now() + 2_000_000;
	let signing = nibkeep_crypto::SigningKey::generate(&mut devices.rng);
	let keys = nibkeep_log::SpaceKeys::new(0, nibkeep_crypto::Key::generate(&mut devices.rng));
	let page = nibkeep_log::AccountPage::issue(
		space,
		&keys,
		&signing,
		(now(), &nibkeep_log::PageBody::Blobs(0, Vec::new())),
		&mut devices.rng,
	);
	let kept = Syncing::new(&mut devices.libraries[0], &now)
		.receive_account(other, page.to_bytes())
		.expect("takes");
	assert!(!kept);
}
