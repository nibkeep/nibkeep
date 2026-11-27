//! The recovery file: each replica of a person with a recovery code keeps the personal
//! space's first key, sealed to each code that stands, in its folder, and each backup holds
//! the same file, with the personal space's changes and keys, so that a code and a backup
//! together open the library; `nib recovery list` names the backups that carry each code
//! ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::devices::{Devices, bytes};
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{Backup, BackupUpdate, NewBackup, RECOVERY_FILE, RecoveryFile};

/// Makes a code on library `device`, typed back.
fn make_code(devices: &mut Devices, device: usize) -> RecoveryCode {
	let code = RecoveryCode::generate(&mut devices.rng);
	let at = devices.tick();
	devices.libraries[device]
		.add_recovery_code(&code, false, at, &mut devices.rng)
		.expect("names the code");
	code
}

/// Revokes a code on library `device`.
fn revoke(devices: &mut Devices, device: usize, code: &RecoveryCode) {
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	let at = devices.tick();
	devices.libraries[device]
		.revoke_recovery_code(&name, at, &mut devices.rng)
		.expect("revokes");
}

/// The recovery file in a folder, if it holds one.
fn file_in(folder: &Path) -> Option<RecoveryFile> {
	RecoveryFile::read(&folder.join(RECOVERY_FILE)).expect("reads")
}

/// Whether the recovery file in a folder opens with `code` to the personal space's first key.
fn opens(devices: &Devices, folder: &Path, code: &RecoveryCode) -> bool {
	let user = devices.libraries[0].status().expect("a status").user.id;
	file_in(folder)
		.and_then(|file| file.open(code).ok())
		.is_some_and(|opened| opened.space == devices.personal() && opened.member == user)
}

/// The folder of library `device`'s replica.
fn replica(devices: &Devices, device: usize) -> PathBuf {
	devices.libraries[device].directory().to_owned()
}

/// An empty folder for a backup.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, backup"))
		.join("Backup")
}

/// Updates the backup from library `device`, at the devices' time.
fn update(devices: &mut Devices, device: usize, backup: &Backup, folder: &Path) -> BackupUpdate {
	let at = devices.tick();
	devices.libraries[device]
		.update_backup(backup, folder, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another")
}

/// The names of the backups that carry the code `code` and of those still to update, as
/// library `device` lists the person's codes.
fn backups_of(devices: &Devices, device: usize, code: &RecoveryCode) -> (Vec<String>, Vec<String>) {
	let key = code.signing_key().verifying_key();
	let listed = devices.libraries[device]
		.recovery_codes()
		.expect("lists")
		.into_iter()
		.find(|named| named.key == key)
		.expect("the code is listed");
	(listed.carried_by, listed.to_update)
}

#[test]
fn each_replica_keeps_the_personal_spaces_first_key_sealed_to_each_code_that_stands() {
	let mut devices = Devices::new("recovery file in replicas", false);
	let laptop = devices.pair("laptop", None);
	let nas = replica(&devices, 0);
	assert!(file_in(&nas).is_none(), "no code, no file");
	let first = make_code(&mut devices, 0);
	assert!(opens(&devices, &nas, &first));
	// It opens with no other code.
	let other = RecoveryCode::generate(&mut devices.rng);
	assert!(!opens(&devices, &nas, &other));
	// A second code: both stand, until a device replaces the first.
	let second = make_code(&mut devices, 0);
	assert!(opens(&devices, &nas, &first) && opens(&devices, &nas, &second));
	// A sync replaces it, as no backup is still to carry the second: the files open with the
	// second alone, on the NAS and on the laptop, which writes its own as it learns of them.
	devices.sync(0, laptop);
	let on_laptop = replica(&devices, laptop);
	assert!(!opens(&devices, &nas, &first) && opens(&devices, &nas, &second));
	assert!(!opens(&devices, &on_laptop, &first) && opens(&devices, &on_laptop, &second));
	// Syncing again leaves the files as they are.
	let before = (
		fs::read(nas.join(RECOVERY_FILE)).expect("reads"),
		fs::read(on_laptop.join(RECOVERY_FILE)).expect("reads"),
	);
	devices.sync(0, laptop);
	let after = (
		fs::read(nas.join(RECOVERY_FILE)).expect("reads"),
		fs::read(on_laptop.join(RECOVERY_FILE)).expect("reads"),
	);
	assert!(before == after, "the files were written anew");
	// With no code left, there is no file.
	revoke(&mut devices, 0, &second);
	assert!(file_in(&nas).is_none());
	devices.sync(0, laptop);
	assert!(file_in(&on_laptop).is_none());
}

#[test]
fn a_backup_carries_the_recovery_file_and_the_personal_spaces_changes_but_not_its_files() {
	let mut devices = Devices::new("recovery file in a backup", false);
	let at = devices.tick();
	let household = devices.libraries[0]
		.create_space("Household", at, &mut devices.rng)
		.expect("makes it");
	let recipe = bytes(7, 20_000);
	devices.add(0, "recipe.txt", &recipe);
	let folder = folder("recovery file in a backup");
	let personal_packs = folder.join("spaces").join(devices.personal().to_string());
	let at = devices.tick();
	let backup = devices.libraries[0]
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				spaces: vec![household],
				..NewBackup::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("adds")
		.backup;
	// Without a code, the backup holds neither the file nor the personal space.
	update(&mut devices, 0, &backup, &folder);
	assert!(file_in(&folder).is_none());
	assert!(!personal_packs.exists());
	// A code is still to reach it.
	let first = make_code(&mut devices, 0);
	assert_eq!(
		backups_of(&devices, 0, &first),
		(vec![], vec!["Blue stick".to_owned()])
	);
	// Updated, it holds the file, which opens with the code, and the personal space's
	// changes and keys, but not its files, which the backup was not chosen to keep.
	let updated = update(&mut devices, 0, &backup, &folder);
	assert!(opens(&devices, &folder, &first));
	assert!(personal_packs.exists());
	assert_eq!(updated.written.blobs, 0, "{updated:?}");
	assert_eq!(
		backups_of(&devices, 0, &first),
		(vec!["Blue stick".to_owned()], vec![])
	);
	// A code made after is still to reach it, until the next update.
	let second = make_code(&mut devices, 0);
	assert_eq!(
		backups_of(&devices, 0, &second),
		(vec![], vec!["Blue stick".to_owned()])
	);
	// The update that makes the backup carry it revokes the first, which it replaces: the
	// backup opens with the first until its next update.
	update(&mut devices, 0, &backup, &folder);
	assert!(opens(&devices, &folder, &first) && opens(&devices, &folder, &second));
	update(&mut devices, 0, &backup, &folder);
	assert!(!opens(&devices, &folder, &first) && opens(&devices, &folder, &second));
	// Another device learns which backups carry the codes by syncing.
	let laptop = devices.pair("laptop", None);
	devices.sync(0, laptop);
	assert_eq!(
		backups_of(&devices, laptop, &second),
		(vec!["Blue stick".to_owned()], vec![])
	);
	// The backup still holds nothing readable without a key from elsewhere.
	for path in [folder.join("keys.cbor"), folder.join("library.cbor")] {
		assert!(!path.exists(), "{}", path.display());
	}
}

#[test]
fn an_update_with_nothing_new_leaves_the_backups_recovery_file_as_it_is() {
	let mut devices = Devices::new("recovery file kept in a backup", false);
	let folder = folder("recovery file kept in a backup");
	let at = devices.tick();
	let backup = devices.libraries[0]
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				..NewBackup::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("adds")
		.backup;
	make_code(&mut devices, 0);
	let laptop = devices.pair("laptop", None);
	devices.sync(0, laptop);
	update(&mut devices, 0, &backup, &folder);
	let before = fs::read(folder.join(RECOVERY_FILE)).expect("reads");
	// Another computer of the person updates it too: the file stays as written.
	update(&mut devices, laptop, &backup, &folder);
	update(&mut devices, 0, &backup, &folder);
	assert!(fs::read(folder.join(RECOVERY_FILE)).expect("reads") == before);
}

/// Changes a byte in the middle of the recovery file in a folder, as a damaged drive does.
fn damage(folder: &Path) {
	let path = folder.join(RECOVERY_FILE);
	let mut bytes = fs::read(&path).expect("reads");
	let middle = bytes.len() / 2;
	bytes[middle] ^= 1;
	fs::write(&path, bytes).expect("writes");
}

#[test]
fn a_damaged_recovery_file_is_written_again() {
	let mut devices = Devices::new("recovery file damaged", false);
	let laptop = devices.pair("laptop", None);
	let folder = folder("recovery file damaged");
	let at = devices.tick();
	let backup = devices.libraries[0]
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				..NewBackup::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("adds")
		.backup;
	let code = make_code(&mut devices, 0);
	update(&mut devices, 0, &backup, &folder);
	// On the backup, the next update writes it again.
	damage(&folder);
	assert!(RecoveryFile::read(&folder.join(RECOVERY_FILE)).is_err());
	update(&mut devices, 0, &backup, &folder);
	assert!(opens(&devices, &folder, &code));
	// In a replica's folder, the next sync does.
	let nas = replica(&devices, 0);
	damage(&nas);
	devices.sync(0, laptop);
	assert!(opens(&devices, &nas, &code));
}

#[test]
fn a_computer_that_has_not_heard_of_a_code_keeps_it_on_a_backup() {
	let mut devices = Devices::new("recovery file behind", false);
	let laptop = devices.pair("laptop", None);
	let at = devices.tick();
	let household = devices.libraries[0]
		.create_space("Household", at, &mut devices.rng)
		.expect("makes it");
	let folder = folder("recovery file behind");
	let at = devices.tick();
	let backup = devices.libraries[0]
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				spaces: vec![household],
				..NewBackup::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("adds")
		.backup;
	devices.sync(0, laptop);
	let code = make_code(&mut devices, 0);
	update(&mut devices, 0, &backup, &folder);
	// The laptop, which has not synced since, updates it: it takes the code in from the
	// backup's personal space, which it keeps, and the code stays on it.
	update(&mut devices, laptop, &backup, &folder);
	assert!(opens(&devices, &folder, &code));
	assert_eq!(
		backups_of(&devices, laptop, &code),
		(vec!["Blue stick".to_owned()], vec![])
	);
	// Once the laptop hears of the code's revocation, its update takes it off.
	revoke(&mut devices, 0, &code);
	devices.sync(0, laptop);
	update(&mut devices, laptop, &backup, &folder);
	assert!(file_in(&folder).is_none());
}
