//! A new recovery code replaces those before it: they are revoked once every backup of the
//! person carries the new one, or at once if the person says so, and stand until then, as a
//! backup not updated since opens with the old code alone. Revoking a device names the codes
//! that its replicas made ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

mod common;

use std::path::{Path, PathBuf};

use common::devices::Devices;
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{Backup, NewBackup, RecoveryCodeInfo};

/// Makes a code on library `device`, typed back.
fn make_code(devices: &mut Devices, device: usize) -> RecoveryCode {
	let code = RecoveryCode::generate(&mut devices.rng);
	let at = devices.tick();
	devices.libraries[device]
		.add_recovery_code(&code, false, at, &mut devices.rng)
		.expect("names the code");
	code
}

/// The person's code `code`, as library `device` lists it.
fn listed(devices: &Devices, device: usize, code: &RecoveryCode) -> RecoveryCodeInfo {
	let key = code.signing_key().verifying_key();
	devices.libraries[device]
		.recovery_codes()
		.expect("lists")
		.into_iter()
		.find(|named| named.key == key)
		.expect("the code is listed")
}

/// The name of a code.
fn name(code: &RecoveryCode) -> String {
	RecoveryCode::name_of(&code.signing_key().verifying_key())
}

/// Revokes, on library `device`, the codes that the newest replaces, as the device does
/// after a sync, or at once with `at_once`; returns their names.
fn replace(devices: &mut Devices, device: usize, at_once: bool) -> Vec<String> {
	let at = devices.tick();
	devices.libraries[device]
		.revoke_replaced_recovery_codes(at_once, at, &mut devices.rng)
		.expect("replaces")
		.into_iter()
		.map(|code| code.name)
		.collect()
}

/// A backup named "Blue stick" of every space, made on library 0.
fn blue(devices: &mut Devices, name: &str) -> (Backup, PathBuf) {
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, backup"))
			.join("Backup");
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
	(backup, folder)
}

/// Updates the backup from library `device`, at the devices' time.
fn update(devices: &mut Devices, device: usize, backup: &Backup, folder: &Path) {
	let at = devices.tick();
	devices.libraries[device]
		.update_backup(backup, folder, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another");
}

#[test]
fn a_new_code_replaces_the_one_before_once_every_backup_carries_it() {
	let mut devices = Devices::new("a code replaced", false);
	let laptop = devices.pair("laptop", None);
	let (backup, folder) = blue(&mut devices, "a code replaced");
	let old = make_code(&mut devices, 0);
	update(&mut devices, 0, &backup, &folder);
	assert_eq!(listed(&devices, 0, &old).replaced_by, None);
	// A new code: the old one stands, replaced by it, as the backup opens with the old alone.
	let new = make_code(&mut devices, 0);
	assert_eq!(replace(&mut devices, 0, false), [] as [String; 0]);
	let before = listed(&devices, 0, &old);
	assert!(!before.revoked);
	assert_eq!(before.replaced_by, Some(name(&new)));
	assert_eq!(listed(&devices, 0, &new).replaced_by, None);
	// The update that makes the backup carry the new code revokes the old one.
	update(&mut devices, 0, &backup, &folder);
	assert!(listed(&devices, 0, &old).revoked);
	assert!(!listed(&devices, 0, &new).revoked);
	// Another device learns of it by syncing, and revokes nothing more.
	devices.sync(0, laptop);
	assert!(listed(&devices, laptop, &old).revoked);
	assert_eq!(replace(&mut devices, laptop, false), [] as [String; 0]);
}

#[test]
fn a_device_that_learns_that_every_backup_carries_the_new_code_revokes_the_old() {
	let mut devices = Devices::new("a code replaced by a sync", false);
	let laptop = devices.pair("laptop", None);
	let (backup, folder) = blue(&mut devices, "a code replaced by a sync");
	let old = make_code(&mut devices, 0);
	devices.sync(0, laptop);
	let new = make_code(&mut devices, laptop);
	// The NAS learns of the new code, and updates the backup, which then carries it: the old
	// one goes there; and the laptop, which syncs after, takes the revocation.
	devices.sync(0, laptop);
	update(&mut devices, 0, &backup, &folder);
	assert!(listed(&devices, 0, &old).revoked);
	devices.sync(0, laptop);
	assert!(listed(&devices, laptop, &old).revoked);
	assert!(!listed(&devices, laptop, &new).revoked);
}

#[test]
fn with_no_backup_a_new_code_replaces_the_one_before_at_once() {
	let mut devices = Devices::new("a code replaced, no backup", false);
	let old = make_code(&mut devices, 0);
	let new = make_code(&mut devices, 0);
	assert_eq!(replace(&mut devices, 0, false), [name(&old)]);
	assert!(listed(&devices, 0, &old).revoked);
	assert!(!listed(&devices, 0, &new).revoked);
}

#[test]
fn a_new_code_replaces_the_one_before_at_once_if_the_person_says_so() {
	let mut devices = Devices::new("a code replaced at once", false);
	let (backup, folder) = blue(&mut devices, "a code replaced at once");
	let old = make_code(&mut devices, 0);
	update(&mut devices, 0, &backup, &folder);
	let new = make_code(&mut devices, 0);
	assert_eq!(replace(&mut devices, 0, false), [] as [String; 0]);
	assert_eq!(replace(&mut devices, 0, true), [name(&old)]);
	assert!(listed(&devices, 0, &old).revoked);
	assert!(!listed(&devices, 0, &new).revoked);
	// With one code alone, there is nothing to replace.
	assert_eq!(replace(&mut devices, 0, true), [] as [String; 0]);
}

#[test]
fn revoking_a_device_names_the_codes_that_it_made() {
	let mut devices = Devices::new("a device's codes", false);
	let laptop = devices.pair("laptop", None);
	let mine = make_code(&mut devices, 0);
	devices.sync(0, laptop);
	let its = make_code(&mut devices, laptop);
	devices.sync(0, laptop);
	let replicas: Vec<_> = devices.libraries[0]
		.to_revoke("laptop")
		.expect("finds it")
		.into_iter()
		.map(|replica| replica.id)
		.collect();
	let made: Vec<String> = devices.libraries[0]
		.recovery_codes_made_by(&replicas)
		.expect("lists")
		.into_iter()
		.map(|code| code.name)
		.collect();
	assert_eq!(made, [name(&its)]);
	// The NAS's own code, which the laptop's replaced, as there is no backup, is revoked,
	// and named no longer.
	assert!(listed(&devices, 0, &mine).revoked);
	let own = devices.libraries[0].status().expect("a status").replica.id;
	assert_eq!(
		devices.libraries[0]
			.recovery_codes_made_by(&[own])
			.expect("lists"),
		[]
	);
}
