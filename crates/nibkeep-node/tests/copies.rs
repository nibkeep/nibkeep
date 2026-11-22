//! Copies of a replica: what backups, programs that sync folders, and people copy is never
//! opened as the replica it was, nor written to; a replica that moves with its drive, or on
//! it, stays itself; and a device that joins beside a copy takes in what only the copy
//! held (technical design: copies of a replica).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{files, now, pair_with, sync_in_memory};
use nibkeep_base::EntityId;
use nibkeep_node::{
	Drive, Library, NewLibrary, NewProtection, NodeError, PairingCode, PassphraseParams, Unlocker,
	declare_drive,
};
use nibkeep_std::CONTENT;
use nibkeep_testkit::{TestRng, copy_folder};

const PASSPHRASE: &str = "correct horse";

fn protection() -> NewProtection<'static> {
	NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"))
}

fn unlocker() -> Unlocker<'static> {
	Unlocker::Passphrase(PASSPHRASE)
}

/// A folder for one test.
fn root(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("copies {name}"))
}

/// Declares that `mount` is a drive with the serial `serial`, which keeps the IDs of folders
/// if `ids` says so, and which the system runs from if `system` says so, on `computer`.
fn on(mount: &Path, serial: &str, ids: bool, system: bool, computer: &str) {
	fs::create_dir_all(mount).expect("makes the folder");
	declare_drive(
		mount,
		Drive {
			serial: Some(serial.as_bytes().to_vec()),
			folder_ids: ids,
			system,
			..Drive::default()
		},
		Some(computer.as_bytes().to_vec()),
	);
}

/// Starts a library in `directory`.
fn start(directory: &Path, rng: &mut TestRng) -> Library {
	Library::create(
		directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection(),
		now(),
		rng,
	)
	.expect("starts")
}

#[test]
fn a_copy_on_another_drive_is_never_opened_nor_written() {
	let mut rng = TestRng::new("copy on another drive");
	let root = root("on another drive");
	let (original, restored) = (root.join("original"), root.join("restored"));
	on(&original, "old disk", true, false, "desktop");
	on(&restored, "new disk", true, false, "desktop");
	drop(start(&original.join(".nib"), &mut rng));
	copy_folder(&original, &restored);
	let before = files(&restored);
	let copied = restored.join(".nib");
	assert!(
		Library::exists(&copied),
		"a folder of copies holds a library"
	);
	assert!(matches!(
		Library::open(&copied, &unlocker()),
		Err(NodeError::Copied)
	));
	assert!(matches!(
		Library::protection(&copied),
		Err(NodeError::Copied)
	));
	assert_eq!(Library::copies_in(&copied).len(), 1);
	assert_eq!(files(&restored), before, "nothing of the copy was written");
	Library::open(&original.join(".nib"), &unlocker()).expect("the original opens");
}

#[test]
fn a_stick_moves_between_computers_and_a_copy_of_it_does_not() {
	let mut rng = TestRng::new("stick");
	let root = root("stick");
	let stick = root.join("stick");
	// FAT and exFAT keep no IDs of folders: the path tells the folder.
	on(&stick, "stick", false, false, "desktop");
	drop(start(&stick.join(".nib"), &mut rng));
	on(&stick, "stick", false, false, "laptop");
	Library::open(&stick.join(".nib"), &unlocker()).expect("opens on the laptop");
	let other = root.join("other stick");
	on(&other, "other stick", false, false, "laptop");
	copy_folder(&stick, &other);
	assert!(matches!(
		Library::open(&other.join(".nib"), &unlocker()),
		Err(NodeError::Copied)
	));
}

#[test]
fn on_the_systems_drive_a_computer_cloned_from_one_image_has_a_copy() {
	let mut rng = TestRng::new("cloned");
	let home = root("cloned").join("home");
	on(&home, "image", true, true, "desktop");
	drop(start(&home.join(".nib"), &mut rng));
	on(&home, "image", true, true, "clone");
	assert!(matches!(
		Library::open(&home.join(".nib"), &unlocker()),
		Err(NodeError::Copied)
	));
	// The same drive, put into another computer as a drive of its data, is the replica.
	on(&home, "image", true, false, "new computer");
	Library::open(&home.join(".nib"), &unlocker()).expect("opens as a data drive");
}

#[test]
fn a_copy_beside_itself_is_a_copy_and_a_folder_moved_on_its_drive_is_not() {
	let mut rng = TestRng::new("beside");
	let root = root("beside");
	on(&root, "disk", true, false, "desktop");
	drop(start(&root.join("library").join(".nib"), &mut rng));
	copy_folder(&root.join("library"), &root.join("backup"));
	let before = files(&root.join("backup"));
	assert!(matches!(
		Library::open(&root.join("backup").join(".nib"), &unlocker()),
		Err(NodeError::Copied)
	));
	assert_eq!(files(&root.join("backup")), before);
	fs::rename(root.join("library"), root.join("moved")).expect("moves");
	Library::open(&root.join("moved").join(".nib"), &unlocker()).expect("the moved one opens");
}

#[test]
fn two_computers_that_sync_their_nib_each_keep_their_own() {
	let mut rng = TestRng::new("synced");
	let root = root("synced");
	let (desktop, laptop) = (root.join("desktop"), root.join("laptop"));
	on(&desktop, "desktop disk", true, true, "desktop");
	on(&laptop, "laptop disk", true, true, "laptop");
	let (on_desktop, on_laptop) = (desktop.join(".nib"), laptop.join(".nib"));
	drop(start(&on_desktop, &mut rng));
	drop(start(&on_laptop, &mut rng));
	// A program that syncs every dotfolder brings each one's replica to the other.
	let desktop_replica = Library::replica_folder(&on_desktop).expect("a replica");
	let laptop_replica = Library::replica_folder(&on_laptop).expect("a replica");
	let replicas = |nib: &Path, replica: &Path| {
		nib.join("replicas")
			.join(replica.file_name().expect("a name"))
	};
	copy_folder(&desktop_replica, &replicas(&on_laptop, &desktop_replica));
	copy_folder(&laptop_replica, &replicas(&on_desktop, &laptop_replica));
	let copies_before = (
		files(&replicas(&on_laptop, &desktop_replica)),
		files(&replicas(&on_desktop, &laptop_replica)),
	);
	for (nib, rng) in [
		(&on_desktop, &mut rng),
		(&on_laptop, &mut TestRng::new("laptop")),
	] {
		let mut library = Library::open(nib, &unlocker()).expect("opens its own");
		let personal = library.personal_space();
		library
			.create_entity(personal, &[], vec![], now() + 1_000, rng)
			.expect("writes");
		assert_eq!(library.copies().len(), 1, "the other's is a copy");
	}
	assert_eq!(
		(
			files(&replicas(&on_laptop, &desktop_replica)),
			files(&replicas(&on_desktop, &laptop_replica)),
		),
		copies_before,
		"neither wrote the other's"
	);
}

#[test]
fn a_new_library_is_not_started_among_copies() {
	let mut rng = TestRng::new("among copies");
	let root = root("among copies");
	let (original, copied) = (root.join("original"), root.join("copied"));
	on(&original, "one", true, false, "desktop");
	on(&copied, "two", true, false, "desktop");
	drop(start(&original.join(".nib"), &mut rng));
	copy_folder(&original, &copied);
	assert!(matches!(
		Library::create(
			&copied.join(".nib"),
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection(),
			now(),
			&mut rng,
		),
		Err(NodeError::Copied)
	));
}

/// Whether a library holds a file with its content.
fn has_content(library: &Library, file: EntityId) -> bool {
	library
		.entity(library.personal_space(), file, common::this_day())
		.expect("reads")
		.is_some_and(|view| view.fields.iter().any(|field| field.field == CONTENT))
}

#[test]
fn a_device_that_joins_beside_a_copy_takes_in_what_only_the_copy_held() {
	let mut rng = TestRng::new("taken in");
	let root = root("taken in");
	let (laptop_folder, desk_folder, new_folder) = (
		root.join("laptop"),
		root.join("desk"),
		root.join("new laptop"),
	);
	on(&laptop_folder, "laptop disk", true, false, "laptop");
	on(&desk_folder, "desk disk", true, false, "desk");
	on(&new_folder, "new disk", true, false, "new laptop");
	let mut laptop = start(&laptop_folder.join(".nib"), &mut rng);
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(
		&mut laptop,
		&desk_folder.join(".nib"),
		&code,
		&code,
		&mut rng,
	);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	// What the laptop adds after, the desk never gets: the laptop is lost, and only its
	// backup holds it.
	let notes = root.join("notes.txt");
	fs::write(&notes, "Milk, eggs, bread.\n").expect("writes");
	let personal = laptop.personal_space();
	let added = laptop
		.add_file(personal, &notes, now() + 1_000, &mut rng)
		.expect("adds")
		.file;
	drop(laptop);
	copy_folder(&laptop_folder, &new_folder);
	let copied = files(&new_folder.join(".nib"));
	let nib = new_folder.join(".nib");
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::Copied)
	));
	// The desk adds the new laptop beside the copy, which takes in what the copy held.
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut desk, &nib, &code, &code, &mut rng);
	paired.expect("the desk paired");
	let mut new = joined.expect("the new laptop joined").library;
	assert!(!has_content(&new, added), "the desk never had it");
	let taken = new.take_in_copies(&|| now() + 120_000).expect("takes in");
	assert_eq!(taken.copies, 1);
	assert!(taken.items > 0);
	assert!(has_content(&new, added), "the copy held it");
	// Nothing of the copy was written, and it is still a copy.
	for (path, bytes) in copied {
		assert_eq!(fs::read(&path).ok(), Some(bytes), "{}", path.display());
	}
	assert_eq!(Library::copies_in(&nib).len(), 1);
	// It reaches the desk at the next sync.
	let (first, second) = sync_in_memory(&mut new, &mut desk, now() + 180_000, (1, 2));
	first.expect("syncs");
	second.expect("syncs");
	assert!(has_content(&desk, added));
	// Taking in again brings nothing new.
	assert_eq!(
		new.take_in_copies(&|| now() + 240_000)
			.expect("takes in")
			.items,
		0
	);
}

#[test]
fn a_network_drive_holds_no_replica() {
	let mut rng = TestRng::new("network drive");
	let share = root("network drive").join("share");
	fs::create_dir_all(&share).expect("makes the folder");
	let remote = |remote| Drive {
		remote,
		..Drive::default()
	};
	declare_drive(&share, remote(true), None);
	assert!(matches!(
		Library::create(
			&share.join(".nib"),
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection(),
			now(),
			&mut rng,
		),
		Err(NodeError::RemoteDrive)
	));
	declare_drive(&share, remote(false), None);
	drop(start(&share.join(".nib"), &mut rng));
	declare_drive(&share, remote(true), None);
	assert!(matches!(
		Library::open(&share.join(".nib"), &unlocker()),
		Err(NodeError::RemoteDrive)
	));
}

#[test]
fn a_folder_refused_for_a_library_is_left_as_it_was() {
	let mut rng = TestRng::new("refused folder");
	let root = root("refused folder");
	// As a folder of OneDrive or iCloud Drive, which would take whatever is made in it to the
	// cloud, and a network drive.
	let cloud = root.join("cloud");
	let share = root.join("share");
	for (folder, drive) in [
		(
			&cloud,
			Drive {
				cloud: true,
				..Drive::default()
			},
		),
		(
			&share,
			Drive {
				remote: true,
				..Drive::default()
			},
		),
	] {
		fs::create_dir_all(folder).expect("makes the folder");
		declare_drive(folder, drive, None);
		let library = folder.join("Passwords");
		let directory = library.join(".nib");
		assert!(matches!(
			Library::create(
				&directory,
				&NewLibrary::new("Ada", "laptop", "nib"),
				&protection(),
				now(),
				&mut rng,
			),
			Err(NodeError::CloudFolder | NodeError::RemoteDrive)
		));
		assert!(matches!(
			Library::allow_fat(&directory),
			Err(NodeError::CloudFolder | NodeError::RemoteDrive)
		));
		assert!(!library.exists(), "{}", folder.display());
	}
	// A drive without a journal, where the person did not allow a library.
	let stick = root.join("stick");
	fs::create_dir_all(&stick).expect("makes the folder");
	declare_drive(
		&stick,
		Drive {
			unjournaled: true,
			..Drive::default()
		},
		None,
	);
	assert!(matches!(
		Library::create(
			&stick.join("Passwords").join(".nib"),
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection(),
			now(),
			&mut rng,
		),
		Err(NodeError::NoJournal)
	));
	assert!(!stick.join("Passwords").exists());
}

#[test]
fn a_copy_in_a_folder_that_a_program_takes_to_the_cloud_makes_no_device_there() {
	let mut rng = TestRng::new("copy in the cloud");
	let root = root("copy in the cloud");
	let (old, cloud) = (root.join("old"), root.join("cloud"));
	on(&old, "old disk", true, false, "old laptop");
	drop(start(&old.join(".nib"), &mut rng));
	copy_folder(&old, &cloud);
	declare_drive(
		&cloud,
		Drive {
			serial: Some(b"new disk".to_vec()),
			folder_ids: true,
			cloud: true,
			..Drive::default()
		},
		Some(b"new laptop".to_vec()),
	);
	let nib = cloud.join(".nib");
	let replicas = nib.join("replicas");
	let (before, changed) = (
		files(&cloud),
		fs::metadata(&replicas).and_then(|folder| folder.modified()),
	);
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	let joining = nibkeep_node::Joining::new("new laptop", "nib", &mut rng).expect("names");
	assert!(matches!(
		Library::add_from_copy(
			&nib,
			(&copy_folder, &unlocker()),
			joining,
			&protection(),
			None,
			&|| now() + 60_000,
			&mut rng,
		),
		Err(NodeError::CloudFolder)
	));
	// Nothing was made there, not even for a while.
	assert_eq!(files(&cloud), before);
	assert_eq!(
		fs::metadata(&replicas)
			.and_then(|folder| folder.modified())
			.ok(),
		changed.ok()
	);
}

#[test]
fn two_replicas_that_are_both_here_open_neither() {
	let mut rng = TestRng::new("two here");
	let root = root("two here");
	on(&root, "stick", false, false, "desktop");
	drop(start(&root.join("one").join(".nib"), &mut rng));
	drop(start(&root.join("two").join(".nib"), &mut rng));
	// The second's folder moved into the first's `.nib`: where it was is empty now, so it is
	// here too.
	let second = Library::replica_folder(&root.join("two").join(".nib")).expect("a replica");
	let into = root
		.join("one")
		.join(".nib")
		.join("replicas")
		.join(second.file_name().expect("a name"));
	fs::rename(&second, &into).expect("moves");
	assert!(matches!(
		Library::open(&root.join("one").join(".nib"), &unlocker()),
		Err(NodeError::SeveralHere(folders)) if folders.len() == 2
	));
}

#[test]
fn an_unfinished_replica_is_this_computers_and_one_without_a_place_file_a_copy() {
	let mut rng = TestRng::new("unfinished");
	let root = root("unfinished");
	on(&root, "disk", true, false, "desktop");
	let nib = root.join("library").join(".nib");
	drop(start(&nib, &mut rng));
	let replica = Library::replica_folder(&nib).expect("a replica");
	fs::remove_file(replica.join("library.cbor")).expect("removes");
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::Unfinished)
	));
	// Without its place file, it may be a copy that a program is still bringing in.
	fs::remove_file(replica.join("place.cbor")).expect("removes");
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::Copied)
	));
}

#[test]
fn copies_that_change_while_a_replica_is_in_use_tell_of_a_program_that_syncs_the_folder() {
	let mut rng = TestRng::new("told");
	let root = root("told");
	let (desktop, laptop) = (root.join("desktop"), root.join("laptop"));
	on(&desktop, "desktop disk", true, true, "desktop");
	on(&laptop, "laptop disk", true, true, "laptop");
	let (on_desktop, on_laptop) = (desktop.join(".nib"), laptop.join(".nib"));
	drop(start(&on_desktop, &mut rng));
	let mut on_the_laptop = start(&on_laptop, &mut TestRng::new("laptop"));
	let laptop_replica = Library::replica_folder(&on_laptop).expect("a replica");
	let brought = on_desktop
		.join("replicas")
		.join(laptop_replica.file_name().expect("a name"));
	copy_folder(&laptop_replica, &brought);
	let desktop_library = Library::open(&on_desktop, &unlocker()).expect("opens");
	assert!(
		!desktop_library.told_of_syncing().expect("looks"),
		"a copy alone tells nothing"
	);
	// The laptop writes, and the program brings it.
	let personal = on_the_laptop.personal_space();
	on_the_laptop
		.create_entity(personal, &[], vec![], now() + 1_000, &mut rng)
		.expect("writes");
	drop(on_the_laptop);
	fs::remove_dir_all(&brought).expect("removes");
	copy_folder(&laptop_replica, &brought);
	assert!(desktop_library.told_of_syncing().expect("looks"), "told");
	assert!(
		!desktop_library.told_of_syncing().expect("looks"),
		"told once"
	);
}

#[test]
fn a_copys_own_keys_add_this_computer_when_no_other_device_can() {
	let mut rng = TestRng::new("own keys");
	let root = root("own keys");
	let (old, new) = (root.join("old disk"), root.join("new disk"));
	on(&old, "old disk", true, false, "old laptop");
	on(&new, "new disk", true, false, "new laptop");
	let mut only = start(&old.join(".nib"), &mut rng);
	let notes = root.join("notes.txt");
	fs::write(&notes, "Milk, eggs, bread.\n").expect("writes");
	let personal = only.personal_space();
	let added = only
		.add_file(personal, &notes, now() + 1_000, &mut rng)
		.expect("adds")
		.file;
	let old_replica = only.status().expect("reads").replica.id;
	drop(only);
	// The only laptop is lost; its backup is restored onto a new disk.
	copy_folder(&old, &new);
	let nib = new.join(".nib");
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	let copied = files(&copy_folder);
	let joining = nibkeep_node::Joining::new("new laptop", "nib", &mut rng).expect("names");
	let library = Library::add_from_copy(
		&nib,
		(&copy_folder, &unlocker()),
		joining,
		&protection(),
		None,
		&|| now() + 60_000,
		&mut rng,
	)
	.expect("adds this computer");
	assert!(
		has_content(&library, added),
		"it took in what the copy held"
	);
	let replicas = library.replicas().expect("lists");
	let old_one = replicas
		.iter()
		.find(|replica| replica.id == old_replica)
		.expect("the old laptop's");
	assert!(old_one.revoked, "the old laptop is revoked");
	assert!(
		replicas
			.iter()
			.any(|replica| replica.this && !replica.revoked)
	);
	// The copy is as it was, and the new device opens as the library here.
	assert_eq!(files(&copy_folder), copied);
	drop(library);
	let mut opened = Library::open(&nib, &unlocker()).expect("opens");
	assert!(has_content(&opened, added));
	opened
		.create_entity(personal, &[], vec![], now() + 120_000, &mut rng)
		.expect("writes as a device of its own");
}

#[test]
fn a_copy_of_a_later_version_gives_nothing_and_adds_no_device() {
	let mut rng = TestRng::new("later copy");
	let root = root("later copy");
	let (laptop_folder, desk_folder, new_folder) = (
		root.join("laptop"),
		root.join("desk"),
		root.join("new laptop"),
	);
	on(&laptop_folder, "laptop disk", true, false, "laptop");
	on(&desk_folder, "desk disk", true, false, "desk");
	on(&new_folder, "new disk", true, false, "new laptop");
	let mut laptop = start(&laptop_folder.join(".nib"), &mut rng);
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(
		&mut laptop,
		&desk_folder.join(".nib"),
		&code,
		&code,
		&mut rng,
	);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	let notes = root.join("notes.txt");
	fs::write(&notes, "Milk, eggs, bread.\n").expect("writes");
	let personal = laptop.personal_space();
	let added = laptop
		.add_file(personal, &notes, now() + 1_000, &mut rng)
		.expect("adds")
		.file;
	drop(laptop);
	copy_folder(&laptop_folder, &new_folder);
	let nib = new_folder.join(".nib");
	// A later version of NibKeep opened the laptop's library before it was copied.
	let copy = Library::copies_in(&nib).remove(0).folder;
	fs::write(
		copy.join("engines.cbor"),
		nibkeep_base::cbor::encode(&nibkeep_base::cbor::Value::Array(vec![
			nibkeep_base::cbor::Value::Integer((nibkeep_node::REPLICA_VERSION + 1).into()),
			nibkeep_base::cbor::Value::Array(vec![]),
		])),
	)
	.expect("writes");
	assert!(matches!(
		Library::copies_in(&nib).remove(0).protections(),
		Err(NodeError::NewerFormats)
	));
	let joining = nibkeep_node::Joining::new("new laptop", "nib", &mut rng).expect("names");
	assert!(matches!(
		Library::add_from_copy(
			&nib,
			(&copy, &unlocker()),
			joining,
			&protection(),
			None,
			&|| now() + 60_000,
			&mut rng,
		),
		Err(NodeError::NewerFormats)
	));
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut desk, &nib, &code, &code, &mut rng);
	paired.expect("the desk paired");
	let mut new = joined.expect("the new laptop joined").library;
	let taken = new.take_in_copies(&|| now() + 120_000).expect("takes in");
	assert_eq!((taken.copies, taken.items), (0, 0), "it was passed over");
	assert!(!has_content(&new, added));
}

#[test]
fn a_new_device_takes_from_a_copy_the_secrets_that_only_the_copy_held() {
	use nibkeep_model::Value;
	use nibkeep_std::{LOGIN, PASSWORD};
	let mut rng = TestRng::new("secrets of a copy");
	let root = root("secrets of a copy");
	let (laptop_folder, desk_folder, new_folder) = (
		root.join("laptop"),
		root.join("desk"),
		root.join("new laptop"),
	);
	on(&laptop_folder, "laptop disk", true, false, "laptop");
	on(&desk_folder, "desk disk", true, false, "desk");
	on(&new_folder, "new disk", true, false, "new laptop");
	let secrets =
		NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"));
	let mut laptop = start(&laptop_folder.join(".nib"), &mut rng);
	laptop
		.set_up_secrets(&secrets, now() + 500, &mut rng)
		.expect("sets up the secrets");
	let personal = laptop.personal_space();
	let login = laptop
		.create_entity(
			personal,
			&[LOGIN],
			vec![(PASSWORD, Value::Text("hunter2".to_owned()))],
			now() + 1_000,
			&mut rng,
		)
		.expect("adds a login");
	// The desk never asks for the secrets: only the laptop holds them.
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(
		&mut laptop,
		&desk_folder.join(".nib"),
		&code,
		&code,
		&mut rng,
	);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	drop(laptop);
	copy_folder(&laptop_folder, &new_folder);
	let nib = new_folder.join(".nib");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut desk, &nib, &code, &code, &mut rng);
	paired.expect("the desk paired");
	let mut new = joined.expect("joined").library;
	new.take_in_copies(&|| now() + 60_000).expect("takes in");
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	// The library's passphrase does not open the copy's secrets.
	assert!(
		new.take_secrets_from_copy(
			&copy_folder,
			&unlocker(),
			&secrets,
			now() + 70_000,
			&mut rng
		)
		.is_err()
	);
	let taken = new
		.take_secrets_from_copy(
			&copy_folder,
			&Unlocker::Passphrase("tr0ub4dor"),
			&secrets,
			now() + 70_000,
			&mut rng,
		)
		.expect("takes the secrets");
	assert!(taken > 0);
	let shown = new
		.entity(personal, login, common::this_day())
		.expect("reads")
		.expect("the login");
	let password = shown
		.fields
		.iter()
		.find(|shown| shown.field == PASSWORD)
		.expect("a password");
	assert_eq!(password.values[0].value, Value::Text("hunter2".to_owned()));
	assert_eq!(
		new.secrets_state().expect("reads"),
		nibkeep_node::SecretsState::Here
	);
}

#[cfg(unix)]
#[test]
fn a_phones_folder_declared_through_a_link_before_it_exists_is_the_same_once_it_does() {
	let mut rng = TestRng::new("phone");
	let root = root("phone");
	// As Android's /data/user/0 is a link to /data/data.
	let real = root.join("data");
	fs::create_dir_all(&real).expect("makes the folder");
	std::os::unix::fs::symlink(&real, root.join("user")).expect("links");
	let nib = root.join("user").join("app").join("library").join(".nib");
	nibkeep_node::declare_phone(&nib, b"phone".to_vec());
	drop(start(&nib, &mut rng));
	// The app starts again, and declares its folder, which exists now.
	nibkeep_node::declare_phone(&nib, b"phone".to_vec());
	Library::open(&nib, &unlocker()).expect("the phone's own library opens");
	// Another phone, with its ID, has a copy.
	nibkeep_node::declare_phone(&nib, b"another phone".to_vec());
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::Copied)
	));
	// A phone that gives no ID makes no library.
	let other = root.join("user").join("other").join(".nib");
	nibkeep_node::declare_phone(&other, Vec::new());
	assert!(matches!(
		Library::create(
			&other,
			&NewLibrary::new("Ada", "phone", "app"),
			&protection(),
			now(),
			&mut rng,
		),
		Err(NodeError::NoPhoneId)
	));
}

#[test]
fn a_mistyped_passphrase_of_a_copys_secrets_leaves_nothing_made() {
	let mut rng = TestRng::new("mistyped");
	let root = root("mistyped");
	let (old, new) = (root.join("old disk"), root.join("new disk"));
	on(&old, "old disk", true, false, "old laptop");
	on(&new, "new disk", true, false, "new laptop");
	let secrets =
		NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"));
	let mut only = start(&old.join(".nib"), &mut rng);
	only.set_up_secrets(&secrets, now() + 500, &mut rng)
		.expect("sets up the secrets");
	drop(only);
	copy_folder(&old, &new);
	let nib = new.join(".nib");
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	let attempt = |typed: &'static str, rng: &mut TestRng| {
		let joining = nibkeep_node::Joining::new("new laptop", "nib", rng).expect("names");
		Library::add_from_copy(
			&nib,
			(&copy_folder, &unlocker()),
			joining,
			&protection(),
			Some((&Unlocker::Passphrase(typed), &secrets)),
			&|| now() + 60_000,
			rng,
		)
	};
	assert!(attempt("tr0ub4door", &mut rng).is_err());
	assert!(
		matches!(Library::replica_folder(&nib), Err(NodeError::Copied)),
		"nothing was made"
	);
	let library = attempt("tr0ub4dor", &mut rng).expect("adds this computer");
	assert_eq!(
		library.secrets_state().expect("reads"),
		nibkeep_node::SecretsState::Here
	);
}

#[test]
fn a_folder_of_dropbox_settings_above_a_library_tells_nothing() {
	let mut rng = TestRng::new("dropbox settings");
	let home = root("dropbox settings");
	on(&home, "disk", true, false, "desktop");
	// Dropbox keeps its settings in a home folder's `.dropbox`, a folder; the root of the
	// folder it syncs holds a file of that name.
	fs::create_dir_all(home.join(".dropbox")).expect("makes the folder");
	let library = start(&home.join("library").join(".nib"), &mut rng);
	assert!(!library.told_of_syncing().expect("looks"));
	drop(library);
	let synced = home.join("Dropbox");
	fs::create_dir_all(&synced).expect("makes the folder");
	fs::write(synced.join(".dropbox"), b"").expect("writes");
	let library = start(&synced.join("library").join(".nib"), &mut rng);
	assert!(library.told_of_syncing().expect("looks"));
}

#[test]
fn several_replicas_here_are_named() {
	let mut rng = TestRng::new("named");
	let root = root("named");
	on(&root, "stick", false, false, "desktop");
	drop(start(&root.join("one").join(".nib"), &mut rng));
	drop(start(&root.join("two").join(".nib"), &mut rng));
	let second = Library::replica_folder(&root.join("two").join(".nib")).expect("a replica");
	let into = root
		.join("one")
		.join(".nib")
		.join("replicas")
		.join(second.file_name().expect("a name"));
	fs::rename(&second, &into).expect("moves");
	let said = Library::open(&root.join("one").join(".nib"), &unlocker())
		.err()
		.expect("several")
		.to_string();
	assert!(said.contains(&into.display().to_string()), "{said}");
}

/// Declares that `mount` is a stick formatted FAT or exFAT, which keeps no journal.
fn on_fat(mount: &Path, computer: &str) {
	fs::create_dir_all(mount).expect("makes the folder");
	declare_drive(
		mount,
		Drive {
			serial: Some(b"fat stick".to_vec()),
			unjournaled: true,
			..Drive::default()
		},
		Some(computer.as_bytes().to_vec()),
	);
}

#[test]
fn a_drive_without_a_journal_holds_a_library_only_as_the_person_asks() {
	let mut rng = TestRng::new("fat");
	let stick = root("fat").join("stick");
	on_fat(&stick, "desktop");
	let nib = stick.join(".nib");
	assert!(matches!(
		Library::create(
			&nib,
			&NewLibrary::new("Ada", "stick", "nib"),
			&protection(),
			now(),
			&mut rng,
		),
		Err(NodeError::NoJournal)
	));
	Library::allow_fat(&nib).expect("allows");
	let library = start(&nib, &mut rng);
	let replica = Library::replica_folder(&nib).expect("a replica");
	assert!(replica.join("in-use").exists(), "open, it says so");
	drop(library);
	assert!(!replica.join("in-use").exists(), "closed, it says nothing");
}

#[test]
fn a_drive_removed_while_its_library_was_open_is_checked_before_the_library_opens() {
	let mut rng = TestRng::new("removed");
	let stick = root("removed").join("stick");
	on_fat(&stick, "desktop");
	let nib = stick.join(".nib");
	Library::allow_fat(&nib).expect("allows");
	drop(start(&nib, &mut rng));
	let replica = Library::replica_folder(&nib).expect("a replica");
	// As a stick pulled out, or a power cut, leaves it: the mark, and no program.
	fs::write(replica.join("in-use"), b"").expect("writes");
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::UncleanRemoval)
	));
	// Still so, until the person says the drive was checked.
	assert!(matches!(
		Library::open(&nib, &unlocker()),
		Err(NodeError::UncleanRemoval)
	));
	Library::drive_checked(&nib).expect("says so");
	let mut library = Library::open(&nib, &unlocker()).expect("opens");
	let report = library.verify(now()).expect("verifies");
	assert!(report.spaces.iter().all(|space| space.problems.is_empty()));
}

#[test]
fn a_device_made_from_a_copy_keeps_its_other_spaces() {
	use nibkeep_model::{Name, TypeName, Value};
	let mut rng = TestRng::new("copy with spaces");
	let root = root("copy with spaces");
	let (old, new) = (root.join("old disk"), root.join("new disk"));
	on(&old, "old disk", true, false, "old laptop");
	on(&new, "new disk", true, false, "new laptop");
	let mut only = start(&old.join(".nib"), &mut rng);
	let household = only
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	only.create_entity(
		household,
		&[TypeName::from_static("std.person")],
		vec![(
			"org.nibkeep.name".parse().expect("a field"),
			Value::Name(Name::new("Boiler service")),
		)],
		now() + 2_000,
		&mut rng,
	)
	.expect("adds");
	drop(only);
	copy_folder(&old, &new);
	let nib = new.join(".nib");
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	let joining = nibkeep_node::Joining::new("new laptop", "nib", &mut rng).expect("names");
	let mut library = Library::add_from_copy(
		&nib,
		(&copy_folder, &unlocker()),
		joining,
		&protection(),
		None,
		&|| now() + 60_000,
		&mut rng,
	)
	.expect("adds this computer");
	assert!(
		library.spaces().contains(&household),
		"{:?}",
		library.spaces()
	);
	let found = library.search(household, "boiler").expect("searches");
	assert_eq!(found.len(), 1, "it took in the space's entities");
	// It writes there as a device of its own.
	library
		.create_entity(household, &[], vec![], now() + 120_000, &mut rng)
		.expect("writes in the space");
}

#[test]
fn a_device_made_from_a_copy_takes_the_spaces_the_copy_holds_and_asks_as_it_did() {
	let mut rng = TestRng::new("copy holds");
	let root = root("copy holds");
	let (old, new) = (root.join("old disk"), root.join("new disk"));
	on(&old, "old disk", true, false, "old laptop");
	on(&new, "new disk", true, false, "new laptop");
	let mut only = start(&old.join(".nib"), &mut rng);
	let household = only
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes it");
	let garage = only
		.create_space("Garage", now() + 2_000, &mut rng)
		.expect("makes it");
	// It dropped the garage, whose files it had not deleted yet, and asks before it takes a
	// space given to it later.
	only.state_space_choice(garage, false, now() + 3_000, &mut rng)
		.expect("drops it");
	only.set_takes_new_spaces(false, now() + 4_000, &mut rng)
		.expect("says so");
	drop(only);
	copy_folder(&old, &new);
	let nib = new.join(".nib");
	let copy_folder = Library::copies_in(&nib).remove(0).folder;
	let joining = nibkeep_node::Joining::new("new laptop", "nib", &mut rng).expect("names");
	let library = Library::add_from_copy(
		&nib,
		(&copy_folder, &unlocker()),
		joining,
		&protection(),
		None,
		&|| now() + 60_000,
		&mut rng,
	)
	.expect("adds this computer");
	assert_eq!(library.spaces(), [library.personal_space(), household]);
	assert!(!library.takes_new_spaces().expect("reads"));
}
