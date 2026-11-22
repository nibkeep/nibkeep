//! A backup in a folder that Android's folder picker gives, which the engine reaches through
//! its interface of files: the picker gives files but no paths, never replaces a file in one
//! step, and moves a file only under its own name ([technical design: backups]).
//!
//! [technical design: backups]: ../../../docs/Technical-design.md#910-backups

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::devices::{Devices, bytes};
use nibkeep_node::{
	Backup, BackupDrive, BackupFolder, BackupUpdate, NewBackup, NodeError, PickerRules,
	backup_named_in,
};
use nibkeep_std::CONTENT;

/// Four weeks and a day, in milliseconds: after it, an update reads a blob file again.
const AFTER_A_ROUND: u64 = 29 * 24 * 60 * 60 * 1000;

/// A folder as Android's picker gives one, on a USB drive, which keeps the picker's rules.
fn picked(root: &Path) -> PickerRules {
	PickerRules::new(
		root,
		BackupDrive {
			path: Some("Backups/Blue".to_owned()),
			disk: Some(b"file system\0\x9c\xb1\x16\x16".to_vec()),
			remote: false,
			unjournaled: true,
		},
	)
}

/// An empty folder for a backup, on a drive that a test pretends to plug into a phone.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, picked"))
		.join("Backups")
		.join("Blue")
}

/// Makes a backup of every space in a picked folder, from library `device`.
fn add(devices: &mut Devices, device: usize, picked: &PickerRules) -> Backup {
	let at = devices.tick();
	let new = NewBackup {
		name: "Blue stick".to_owned(),
		waits: Some(0),
		..NewBackup::default()
	};
	devices.libraries[device]
		.add_backup_in(picked, &new, at, &mut devices.rng)
		.expect("adds")
		.backup
}

/// Updates the backup in a picked folder from library `device`, at the devices' time.
fn update(
	devices: &mut Devices,
	device: usize,
	backup: &Backup,
	picked: &PickerRules,
) -> BackupUpdate {
	let at = devices.tick();
	devices.libraries[device]
		.update_backup_in(backup, picked, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another")
}

/// The files under a folder, by their paths from it.
fn files_under(folder: &Path) -> Vec<String> {
	let mut found = Vec::new();
	let mut folders = vec![folder.to_owned()];
	while let Some(at) = folders.pop() {
		for entry in fs::read_dir(&at).expect("reads").flatten() {
			let path = entry.path();
			if path.is_dir() {
				folders.push(path);
			} else {
				let relative = path.strip_prefix(folder).expect("under it");
				found.push(relative.to_string_lossy().replace('\\', "/"));
			}
		}
	}
	found.sort();
	found
}

#[test]
fn a_picked_folder_is_a_backup_that_a_computer_updates_in_turn() {
	let mut devices = Devices::new("picked backup", false);
	let laptop = devices.pair("laptop", None);
	let root = folder("picked backup");
	let picked = picked(&root);
	let backup = add(&mut devices, 0, &picked);
	// Recorded by its path on the drive, which a computer finds under each mount.
	assert_eq!(backup.folder, "Backups/Blue");
	assert_eq!(backup_named_in(&picked), Some(backup.id));
	devices.sync(0, laptop);
	let (_, small) = devices.add(0, "recipe.txt", &bytes(1, 40_000));
	let (_, large) = devices.add(0, "steps.mov", &bytes(2, 3_000_000));
	let first = update(&mut devices, 0, &backup, &picked);
	assert_eq!((first.update, first.written.blobs), (1, 2), "{first:?}");
	// Every file was moved into place under its own name, never renamed.
	for (file, into) in &picked.moves() {
		assert!(file.starts_with("tmp/"), "{file} into {into}");
	}
	let state = devices.libraries[0]
		.backup_state_in(&backup, &picked, devices.at)
		.expect("reads");
	assert_eq!((state.update, state.held), (1, 2), "{state:?}");
	// A computer reads what the phone wrote, at the folder's path, and goes on counting.
	let at = devices.tick();
	let taken = devices.libraries[laptop]
		.update_backup(&backup, &root, &|| at, &|| false)
		.expect("updates")
		.expect("not held");
	assert_eq!((taken.update, taken.taken.blobs), (2, 2), "{taken:?}");
	assert!(devices.holds(laptop, &small) && devices.holds(laptop, &large));
	// And the phone after it.
	let (file, _) = devices.add(laptop, "note.txt", &bytes(3, 100));
	let again = update(&mut devices, laptop, &backup, &picked);
	assert_eq!(again.update, 3, "{again:?}");
	let taken = update(&mut devices, 0, &backup, &picked);
	assert_eq!(taken.update, 4, "{taken:?}");
	assert!(taken.taken.changes >= 1, "{taken:?}");
	// A purge reaches it: packs written anew, the old ones deleted, a blob file gone.
	let space = devices.personal();
	let at = devices.tick();
	devices.libraries[0]
		.purge(space, file, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	let purged = update(&mut devices, 0, &backup, &picked);
	assert!(purged.purged >= 1, "{purged:?}");
	// Nothing is left half written.
	let left = files_under(&root);
	assert!(
		left.iter().all(|file| !file.starts_with("tmp/")),
		"{left:?}"
	);
}

#[test]
fn a_damaged_copy_in_a_picked_folder_is_written_again_in_its_place() {
	let mut devices = Devices::new("picked damaged", false);
	let root = folder("picked damaged");
	let picked = picked(&root);
	let backup = add(&mut devices, 0, &picked);
	devices.add(0, "large.bin", &bytes(4, 2_000_000));
	update(&mut devices, 0, &backup, &picked);
	let [blob] = files_under(&root)
		.into_iter()
		.filter(|file| file.starts_with("blobs/"))
		.collect::<Vec<_>>()
		.try_into()
		.expect("one blob file");
	let path = root.join(&blob);
	let mut damaged = fs::read(&path).expect("reads");
	damaged[1_000] ^= 0xFF;
	fs::write(&path, &damaged).expect("writes");
	devices.at += AFTER_A_ROUND;
	let checked = update(&mut devices, 0, &backup, &picked);
	assert_eq!((checked.damaged, checked.repaired), (1, 1), "{checked:?}");
	let intact = update(&mut devices, 0, &backup, &picked);
	assert_eq!(intact.damaged, 0, "{intact:?}");
}

#[test]
fn a_record_replaced_halfway_is_put_in_place_by_the_next_update() {
	let mut devices = Devices::new("picked halfway", false);
	let root = folder("picked halfway");
	let picked = picked(&root);
	let backup = add(&mut devices, 0, &picked);
	devices.add(0, "large.bin", &bytes(5, 2_000_000));
	update(&mut devices, 0, &backup, &picked);
	let second = update(&mut devices, 0, &backup, &picked);
	assert_eq!(second.update, 2);
	// The picker deleted the old record, and the app ended before the new one was moved in:
	// it waits in `tmp/`, and the mark of an update stays.
	fs::create_dir_all(root.join("tmp")).expect("makes");
	fs::rename(
		root.join("record.cbor"),
		root.join("tmp").join("record.cbor"),
	)
	.expect("moves");
	fs::write(root.join("in-use"), b"").expect("writes");
	let state = devices.libraries[0]
		.backup_state_in(&backup, &picked, devices.at)
		.expect("reads");
	assert_eq!(state.update, 0, "no record in place: {state:?}");
	let after = update(&mut devices, 0, &backup, &picked);
	// The record was found, so the numbers go on, and only the unclean end costs checks.
	assert_eq!(after.update, 3, "{after:?}");
	assert!(after.unclean, "{after:?}");
	assert_eq!(
		after.checked, 1,
		"read back, as the drive keeps no journal: {after:?}"
	);
	assert!(root.join("record.cbor").exists());
	assert!(!root.join("tmp").join("record.cbor").exists());
	// A record lost outright, as to a drive pulled out, leaves the numbers going on, after
	// updates that wrote no pack too.
	fs::remove_file(root.join("record.cbor")).expect("removes");
	let lost = update(&mut devices, 0, &backup, &picked);
	assert_eq!(lost.update, 4, "{lost:?}");
}

#[test]
fn a_picked_folder_becomes_a_backup_only_if_empty_and_held_by_one_program() {
	let mut devices = Devices::new("picked empty", false);
	let root = folder("picked empty");
	let picked = picked(&root);
	fs::write(root.join("holiday.jpg"), b"a photo").expect("writes");
	let at = devices.tick();
	let refused = devices.libraries[0].add_backup_in(
		&picked,
		&NewBackup {
			name: "Blue stick".to_owned(),
			..NewBackup::default()
		},
		at,
		&mut devices.rng,
	);
	assert!(
		matches!(refused, Err(NodeError::BackupFolder(_))),
		"{refused:?}"
	);
	assert_eq!(backup_named_in(&picked), None);
	fs::remove_file(root.join("holiday.jpg")).expect("removes");
	let backup = add(&mut devices, 0, &picked);
	let lock = picked.open_lock("lock").expect("opens");
	lock.lock().expect("locks");
	let at = devices.tick();
	let held = devices.libraries[0]
		.update_backup_in(&backup, &picked, &|| at, &|| false)
		.expect("reads");
	assert!(held.is_none());
	drop(lock);
	update(&mut devices, 0, &backup, &picked);
}
