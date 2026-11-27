//! Recovering a library on a new device from a recovery code and a backup, after its person
//! lost every device: the personal space comes back, the lost devices are revoked, and a
//! device that turns up later learns of it from the backup ([technical design: recovery
//! codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::devices::{Devices, bytes, on_disk};
use crate::common::{protection, this_day};
use nibkeep_base::EntityId;
use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_crypto::{RecoveryCode, key_hash};
use nibkeep_governance::{RecoveryStatement, SpaceEntry, recovery_record, state_recovery_code};
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	Backup, Joining, Library, NewBackup, NodeError, RECOVERY_FILE, Recovered, RecoveredSecrets,
	Recovering, RecoveryFile, Unlocker,
};

const NOTE: &str = "local.note";

/// Makes a code on library `device`, typed back.
fn make_code(devices: &mut Devices, device: usize) -> RecoveryCode {
	let code = RecoveryCode::generate(&mut devices.rng);
	let at = devices.tick();
	devices.libraries[device]
		.add_recovery_code(&code, false, at, &mut devices.rng)
		.expect("names the code");
	code
}

/// A backup of every space, made on library 0 and updated there.
fn backup(devices: &mut Devices, name: &str) -> (Backup, PathBuf) {
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
	update(devices, 0, &backup, &folder);
	(backup, folder)
}

fn update(devices: &mut Devices, device: usize, backup: &Backup, folder: &Path) {
	let at = devices.tick();
	devices.libraries[device]
		.update_backup(backup, folder, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another");
}

/// An empty folder on a new computer's disk.
fn new_computer(name: &str) -> PathBuf {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, new"));
	on_disk(&directory, &format!("{name} new"));
	directory
}

/// Recovers the library in `directory` from the backup in `folder` with `code`.
fn recover(
	devices: &mut Devices,
	directory: &Path,
	(folder, code): (&Path, &RecoveryCode),
) -> Result<(Library, Recovered), NodeError> {
	let joining = Joining::new("new laptop", "nib", &mut devices.rng).expect("names");
	let at = devices.tick();
	Library::recover(
		directory,
		(folder, code),
		joining,
		(&protection(), None),
		&|| at,
		&mut devices.rng,
	)
}

/// Starts recovering the library in `directory` from the backup in `folder` with `code`.
fn start_recovery<'a>(
	devices: &mut Devices,
	directory: &Path,
	(folder, code): (&'a Path, &'a RecoveryCode),
) -> Recovering<'a> {
	let joining = Joining::new("new laptop", "nib", &mut devices.rng).expect("names");
	let at = devices.tick();
	Library::start_recovery(
		directory,
		(folder, code),
		joining,
		&protection(),
		&|| at,
		&mut devices.rng,
	)
	.expect("starts recovering")
}

/// The names of the devices of `replicas`, in order.
fn device_names(replicas: &[nibkeep_node::Replica]) -> Vec<String> {
	let mut names: Vec<String> = replicas
		.iter()
		.filter_map(|replica| replica.device_name.clone())
		.collect();
	names.sort();
	names
}

/// A note in the personal space, written on library `device`.
fn note(devices: &mut Devices, device: usize, text: &str) -> EntityId {
	let at = devices.tick();
	let space = devices.personal();
	devices.libraries[device]
		.create_entity(
			space,
			&[],
			vec![(field(), Value::Text(text.to_owned()))],
			at,
			&mut devices.rng,
		)
		.expect("writes")
}

fn field() -> FieldName {
	NOTE.parse().expect("a field name")
}

fn notes(library: &Library, entity: EntityId) -> Vec<Value> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.map_or_default(|entity| {
			entity
				.values(&field())
				.iter()
				.map(|shown| shown.value.clone())
				.collect()
		})
}

/// Whether library `library`'s own replica is revoked, as it knows.
fn revoked_here(library: &Library) -> bool {
	library
		.replicas()
		.expect("lists")
		.into_iter()
		.any(|replica| replica.this && replica.revoked)
}

#[test]
fn a_library_comes_back_from_its_code_and_a_backup() {
	let mut devices = Devices::new("recovered", false);
	let laptop = devices.pair("laptop", None);
	devices.sync(0, laptop);
	let written = note(&mut devices, 0, "the boiler's code is 4711");
	let (_, content) = devices.add(0, "recipe.txt", &bytes(3, 20_000));
	let code = make_code(&mut devices, 0);
	let (_, folder) = backup(&mut devices, "recovered");
	devices.sync(0, laptop);
	// Both devices are lost; a new one comes back with the code and the backup.
	let directory = new_computer("recovered");
	let (library, recovered) =
		recover(&mut devices, &directory, (&folder, &code)).expect("recovers");
	assert_eq!(
		recovered.from,
		nibkeep_node::RecoveredFrom::Backup {
			name: Some("Blue stick".to_owned()),
			updated: true
		}
	);
	assert_eq!(recovered.revoked.len(), 2, "the NAS and the laptop");
	assert_eq!(recovered.secrets, RecoveredSecrets::NoneHeld);
	assert_eq!(
		notes(&library, written),
		[Value::Text("the boiler's code is 4711".to_owned())]
	);
	assert!(
		library
			.file_content(library.personal_space(), &content, u64::MAX)
			.expect("reads")
			.is_some(),
		"the file came back"
	);
	let replicas = library.replicas().expect("lists");
	assert_eq!(replicas.len(), 3);
	for replica in &replicas {
		assert_eq!(replica.revoked, !replica.this, "{replica:?}");
	}
	// It is a library like any other: it opens again, and writes.
	let folder_here = library.directory().to_owned();
	drop(library);
	assert!(!folder_here.join("recovering").exists());
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	let at = devices.tick();
	let space = library.personal_space();
	library
		.create_entity(
			space,
			&[],
			vec![(field(), Value::Text("back".to_owned()))],
			at,
			&mut devices.rng,
		)
		.expect("writes");
	// The backup carries the new device: the laptop, found again and plugged in, learns from
	// it that it was revoked.
	let backup = devices.libraries[laptop]
		.backups()
		.expect("lists")
		.remove(0);
	assert!(!revoked_here(&devices.libraries[laptop]));
	update(&mut devices, laptop, &backup, &folder);
	assert!(revoked_here(&devices.libraries[laptop]));
}

#[test]
fn a_recovery_keeps_the_devices_the_person_still_has() {
	let mut devices = Devices::new("kept", false);
	let laptop = devices.pair("laptop", None);
	let phone = devices.pair("phone", None);
	devices.sync(0, laptop);
	devices.sync(0, phone);
	let code = make_code(&mut devices, 0);
	let (_, folder) = backup(&mut devices, "kept");
	devices.sync(0, phone);
	// The NAS and the laptop are lost, and the phone is at hand.
	let directory = new_computer("kept");
	let mut recovering = start_recovery(&mut devices, &directory, (&folder, &code));
	let others = recovering.others().expect("lists");
	assert_eq!(device_names(&others), ["laptop", "nas", "phone"]);
	let kept: Vec<EntityId> = recovering
		.others_named("phone")
		.expect("names it")
		.iter()
		.map(|replica| replica.id)
		.collect();
	assert_eq!(kept.len(), 1);
	// What names none of them keeps nothing.
	let error = recovering.others_named("tablet").expect_err("none");
	assert!(matches!(error, NodeError::WhichDevice(_)), "{error}");
	let error = recovering
		.keep(&[EntityId::random(&mut devices.rng)])
		.expect_err("none");
	assert!(matches!(error, NodeError::WhichDevice(_)), "{error}");
	recovering.keep(&kept).expect("keeps");
	let at = devices.tick();
	let (library, recovered) = recovering
		.finish(None, &|| at, &mut devices.rng)
		.expect("recovers");
	assert_eq!(
		recovered
			.kept
			.iter()
			.map(|replica| replica.id)
			.collect::<Vec<_>>(),
		kept
	);
	assert_eq!(device_names(&recovered.revoked), ["laptop", "nas"]);
	for replica in library.replicas().expect("lists") {
		let lost = !replica.this && !kept.contains(&replica.id);
		assert_eq!(replica.revoked, lost, "{replica:?}");
	}
	// The new device writes with a key that the lost devices never held.
	let personal = library.personal_space();
	let (_, writing) = library.space_key_numbers(personal).expect("reads");
	let (lost, _) = devices.libraries[0]
		.space_key_numbers(personal)
		.expect("reads");
	assert!(!lost.contains(&writing), "{lost:?} {writing}");
	// The phone learns of the new device from the backup, and stays the person's: the two
	// sync, and each reads what the other writes.
	let stick = devices.libraries[phone].backups().expect("lists").remove(0);
	update(&mut devices, phone, &stick, &folder);
	assert!(!revoked_here(&devices.libraries[phone]));
	devices.libraries.push(library);
	let new = devices.libraries.len() - 1;
	let from_new = note(&mut devices, new, "written on the new laptop");
	let from_phone = note(&mut devices, phone, "written on the phone");
	devices.sync(new, phone);
	assert_eq!(
		notes(&devices.libraries[phone], from_new),
		[Value::Text("written on the new laptop".to_owned())]
	);
	assert_eq!(
		notes(&devices.libraries[new], from_phone),
		[Value::Text("written on the phone".to_owned())]
	);
	let (held, _) = devices.libraries[phone]
		.space_key_numbers(personal)
		.expect("reads");
	assert!(held.contains(&writing), "{held:?} {writing}");
}

/// The NAS and a phone that synced with it, a code made on the NAS, and a backup, after which
/// `before_loss` runs, with the phone's library and the code; then the NAS is lost, and a new
/// device recovers, keeping the phone. Returns the devices, and the phone's and the new
/// device's libraries.
fn recovered_keeping_the_phone(
	name: &str,
	before_loss: impl FnOnce(&mut Devices, usize, &RecoveryCode),
) -> (Devices, usize, usize) {
	let mut devices = Devices::new(name, false);
	let phone = devices.pair("phone", None);
	devices.sync(0, phone);
	let code = make_code(&mut devices, 0);
	let (_, folder) = backup(&mut devices, name);
	before_loss(&mut devices, phone, &code);
	let directory = new_computer(name);
	let mut recovering = start_recovery(&mut devices, &directory, (&folder, &code));
	let kept: Vec<EntityId> = recovering
		.others_named("phone")
		.expect("names it")
		.iter()
		.map(|replica| replica.id)
		.collect();
	recovering.keep(&kept).expect("keeps");
	let at = devices.tick();
	let (library, _) = recovering
		.finish(None, &|| at, &mut devices.rng)
		.expect("recovers");
	devices.libraries.push(library);
	let new = devices.libraries.len() - 1;
	(devices, phone, new)
}

/// Syncs the new device with the phone, which it connects to, and checks that the phone
/// refuses it.
fn refused_by_the_phone(devices: &mut Devices, phone: usize, new: usize) {
	let at = devices.tick();
	let (left, right) = devices.libraries.split_at_mut(new);
	let (initiated, responded) =
		common::sync_in_memory(&mut right[0], &mut left[phone], at, (new, phone));
	assert!(
		matches!(
			responded,
			Err(NodeError::Sync(nibkeep_sync::SyncError::NotAReplica))
		),
		"{responded:?}"
	);
	assert!(initiated.is_err());
}

#[test]
fn a_kept_device_takes_the_new_one_from_a_sync() {
	let (mut devices, phone, new) =
		recovered_keeping_the_phone("introduced", |devices, phone, _| devices.sync(0, phone));
	let personal = devices.personal();
	let (_, writing) = devices.libraries[new]
		.space_key_numbers(personal)
		.expect("reads");
	let from_new = note(&mut devices, new, "written on the new laptop");
	// The new device introduces itself in its first sync with the phone, which took no
	// backup: the phone takes its listing, learns that the NAS is revoked, and takes the new
	// key, sealed to it.
	devices.sync(new, phone);
	let replicas = devices.libraries[phone].replicas().expect("lists");
	let named = |name: &str| {
		replicas
			.iter()
			.find(|replica| replica.device_name.as_deref() == Some(name))
			.unwrap_or_else(|| panic!("{name}"))
	};
	assert!(named("nas").revoked);
	assert!(!named("new laptop").revoked);
	assert_eq!(
		notes(&devices.libraries[phone], from_new),
		[Value::Text("written on the new laptop".to_owned())]
	);
	let (held, _) = devices.libraries[phone]
		.space_key_numbers(personal)
		.expect("reads");
	assert!(held.contains(&writing), "{held:?} {writing}");
	// From then on, the phone knows it, and connects to it too.
	let from_phone = note(&mut devices, phone, "written on the phone");
	devices.sync(phone, new);
	assert_eq!(
		notes(&devices.libraries[new], from_phone),
		[Value::Text("written on the phone".to_owned())]
	);
	// Once the phone revokes it, its listing by the code introduces it no more.
	let id = named("new laptop").id;
	let at = devices.tick();
	devices.libraries[phone]
		.revoke(&[id], at, &mut devices.rng)
		.expect("revokes");
	refused_by_the_phone(&mut devices, phone, new);
}

#[test]
fn a_device_that_does_not_hold_the_code_takes_nothing_from_the_new_one() {
	let (mut devices, phone, new) = recovered_keeping_the_phone("not introduced", |_, _, _| {});
	// The phone never took the code's statement, so the new device's listing by it says
	// nothing to it: it refuses the new device.
	refused_by_the_phone(&mut devices, phone, new);
	assert!(
		devices.libraries[phone]
			.replicas()
			.expect("lists")
			.iter()
			.all(|replica| replica.device_name.as_deref() != Some("new laptop"))
	);
	// Once it takes the code's statement, from the NAS found again, the new device's
	// listing counts with it, and the two sync.
	devices.sync(0, phone);
	devices.sync(new, phone);
	assert!(
		devices.libraries[phone]
			.replicas()
			.expect("lists")
			.iter()
			.any(
				|replica| replica.device_name.as_deref() == Some("new laptop") && !replica.revoked
			)
	);
}

#[test]
fn a_code_revoked_since_the_backup_introduces_nobody() {
	let (mut devices, phone, new) =
		recovered_keeping_the_phone("revoked since", |devices, phone, code| {
			let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
			let at = devices.tick();
			devices.libraries[0]
				.revoke_recovery_code(&name, at, &mut devices.rng)
				.expect("revokes");
			devices.sync(0, phone);
		});
	// The backup, not updated since, still held the code; the phone knows it is revoked.
	refused_by_the_phone(&mut devices, phone, new);
}

#[test]
fn a_wrong_code_or_folder_writes_nothing() {
	let mut devices = Devices::new("wrong code", false);
	let code = make_code(&mut devices, 0);
	let (_, folder) = backup(&mut devices, "wrong code");
	let directory = new_computer("wrong code");
	let other = RecoveryCode::generate(&mut devices.rng);
	let error = recover(&mut devices, &directory, (&folder, &other))
		.err()
		.expect("another code");
	assert!(matches!(error, NodeError::CodeOpensNothing), "{error}");
	// A folder that is no backup, such as the backup's own parent.
	let parent = folder.parent().expect("a parent").to_owned();
	let error = recover(&mut devices, &directory, (&parent, &code))
		.err()
		.expect("no backup");
	assert!(matches!(error, NodeError::NoRecoverySource(_)), "{error}");
	assert!(matches!(
		Library::replica_folder(&directory),
		Err(NodeError::NoLibrary)
	));
	// The right code still works.
	recover(&mut devices, &directory, (&folder, &code)).expect("recovers");
}

#[test]
fn a_recovery_file_that_names_what_the_code_never_signed_brings_nothing_back() {
	let mut devices = Devices::new("forged recovery file", false);
	let laptop = devices.pair("laptop", None);
	devices.sync(0, laptop);
	let code = make_code(&mut devices, 0);
	let (backup, folder) = backup(&mut devices, "forged recovery file");
	devices.sync(0, laptop);
	let person = devices.libraries[0].user_and_device().expect("reads").0.id;
	// The laptop turns against its person. It names the code in the personal space for a
	// member it made up, as the code's signature of its keys allows, updates the backup, and
	// writes the backup's recovery file with an entry for that member, sealing the personal
	// space's first key, which it holds, to the code anew.
	let personal = devices.personal();
	let made_up = EntityId::random(&mut devices.rng);
	let copy = RecoveryStatement::sign(&code, person).for_member(made_up);
	let operation = state_recovery_code(personal, &copy, &mut devices.rng);
	let directory = devices.folders[laptop].clone();
	drop(devices.libraries.remove(laptop));
	let unlocker = Unlocker::Passphrase("battery staple");
	let at = devices.tick();
	common::forge(
		&directory,
		&unlocker,
		(personal, vec![operation]),
		(None, at),
		&mut devices.rng,
	);
	let reopened = Library::open(&directory, &unlocker).expect("opens");
	devices.libraries.insert(laptop, reopened);
	update(&mut devices, laptop, &backup, &folder);
	let genuine = RecoveryFile::read(&folder.join(RECOVERY_FILE))
		.expect("reads")
		.expect("a file");
	let opened = genuine.open(&code).expect("opens");
	let (first, root) = (opened.key, *opened.root.hash());
	let record = recovery_record(personal, made_up, &code.signing_key().verifying_key());
	let entry = SpaceEntry::seal(
		&code.decryption_key().encryption_key(),
		(personal, made_up, root),
		record,
		(&first, key_hash(&first)),
		&mut devices.rng,
	);
	// It cannot sign for the code: the genuine entry's signature is the best it has.
	let signature = genuine_signature(&folder);
	let codes = Cbor::Array(vec![Cbor::Array(vec![
		record.to_cbor(),
		entry.to_cbor(),
		signature,
	])]);
	let check = nibkeep_base::Hash::of(&cbor::encode(&codes));
	let forged = Cbor::Array(vec![1u8.to_cbor(), codes, check.to_cbor()]);
	fs::write(folder.join(RECOVERY_FILE), cbor::encode(&forged)).expect("writes");
	// Every device is lost. The recovery takes nothing that the code did not sign, and says
	// that the file was changed, rather than bring back a library of a stranger's.
	let directory = new_computer("forged recovery file");
	let error = recover(&mut devices, &directory, (&folder, &code))
		.err()
		.expect("refused");
	assert!(matches!(error, NodeError::RecoveryFileChanged), "{error}");
	assert!(matches!(
		Library::replica_folder(&directory),
		Err(NodeError::NoLibrary)
	));
	// The next update by a device of the person's writes the file anew, as it names what the
	// codes that stand signed, and the code brings back the person's library.
	update(&mut devices, 0, &backup, &folder);
	let (library, _) = recover(&mut devices, &directory, (&folder, &code)).expect("recovers");
	assert_eq!(library.user_and_device().expect("reads").0.id, person);
}

/// The CBOR of the code's signature in the recovery file in a backup's `folder`, which holds
/// one code's entry.
fn genuine_signature(folder: &Path) -> Cbor {
	let bytes = fs::read(folder.join(RECOVERY_FILE)).expect("reads");
	let Cbor::Array(file) = cbor::from_slice::<Cbor>(&bytes).expect("CBOR") else {
		panic!("an array");
	};
	let Cbor::Array(codes) = &file[1] else {
		panic!("the codes");
	};
	let Cbor::Array(entry) = &codes[0] else {
		panic!("an entry");
	};
	entry[2].clone()
}

#[test]
fn a_revoked_code_brings_nothing_back() {
	let mut devices = Devices::new("revoked code", false);
	let code = make_code(&mut devices, 0);
	let (backup, folder) = backup(&mut devices, "revoked code");
	let file = fs::read(folder.join(RECOVERY_FILE)).expect("reads");
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	let at = devices.tick();
	devices.libraries[0]
		.revoke_recovery_code(&name, at, &mut devices.rng)
		.expect("revokes");
	update(&mut devices, 0, &backup, &folder);
	let directory = new_computer("revoked code");
	// The backup holds no recovery file any more, as no code stands.
	let error = recover(&mut devices, &directory, (&folder, &code))
		.err()
		.expect("revoked");
	assert!(matches!(error, NodeError::NoRecoverySource(_)), "{error}");
	// Nor does an old file, put back, bring back a device: the packs hold the revocation.
	fs::write(folder.join(RECOVERY_FILE), file).expect("writes");
	let error = recover(&mut devices, &directory, (&folder, &code))
		.err()
		.expect("revoked");
	assert!(matches!(error, NodeError::RecoveryCodeRevoked), "{error}");
	assert!(matches!(
		Library::replica_folder(&directory),
		Err(NodeError::NoLibrary)
	));
}

#[test]
fn a_recovery_cut_short_starts_over() {
	let mut devices = Devices::new("cut short", false);
	let code = make_code(&mut devices, 0);
	let (_, folder) = backup(&mut devices, "cut short");
	let directory = new_computer("cut short");
	let (library, _) = recover(&mut devices, &directory, (&folder, &code)).expect("recovers");
	let first = library.directory().to_owned();
	let first_replica = library
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.expect("this one")
		.id;
	drop(library);
	// As if it had crashed before it was done, after it updated the backup.
	fs::write(first.join("recovering"), b"").expect("marks");
	let error = Library::open(&directory, &Unlocker::Passphrase("battery staple"))
		.err()
		.expect("does not open");
	assert!(matches!(error, NodeError::RecoveryCutShort), "{error}");
	// While another recovery holds the mark, as one that still runs, it is left alone.
	let running = fs::File::options()
		.write(true)
		.open(first.join("recovering"))
		.expect("opens");
	running.try_lock().expect("locks");
	let error = recover(&mut devices, &directory, (&folder, &code))
		.err()
		.expect("one runs");
	assert!(matches!(error, NodeError::Exists), "{error}");
	assert!(first.exists());
	drop(running);
	// Run again, it starts over: the replica it left on the backup is no device to keep, and
	// it revokes it.
	let mut recovering = start_recovery(&mut devices, &directory, (&folder, &code));
	assert!(!first.exists());
	assert!(
		recovering
			.others()
			.expect("lists")
			.iter()
			.all(|replica| replica.id != first_replica)
	);
	let error = recovering
		.others_named("new laptop")
		.expect_err("cut short");
	assert!(error.to_string().contains("cut short"), "{error}");
	let error = recovering.keep(&[first_replica]).expect_err("cut short");
	assert!(matches!(error, NodeError::WhichDevice(_)), "{error}");
	let at = devices.tick();
	let (library, recovered) = recovering
		.finish(None, &|| at, &mut devices.rng)
		.expect("recovers again");
	assert!(
		recovered
			.revoked
			.iter()
			.any(|replica| replica.id == first_replica)
	);
	let replicas = library.replicas().expect("lists");
	for replica in &replicas {
		assert_eq!(replica.revoked, !replica.this, "{replica:?}");
	}
}

#[test]
fn a_recovery_file_left_waiting_as_a_phones_picker_leaves_it_still_brings_the_library_back() {
	let mut devices = Devices::new("recovered from waiting", false);
	let code = make_code(&mut devices, 0);
	let (backup, folder) = backup(&mut devices, "recovered from waiting");
	let (there, waiting) = (
		folder.join(RECOVERY_FILE),
		folder.join("tmp").join(RECOVERY_FILE),
	);
	// Android's picker deleted the old file, and the app ended before the new one moved in.
	let halfway = || {
		fs::create_dir_all(folder.join("tmp")).expect("makes");
		fs::rename(&there, &waiting).expect("moves");
	};
	halfway();
	// The next update puts it in place.
	update(&mut devices, 0, &backup, &folder);
	assert!(there.exists() && !waiting.exists());
	// And a recovery before it reads the one waiting.
	halfway();
	let directory = new_computer("recovered from waiting");
	recover(&mut devices, &directory, (&folder, &code)).expect("recovers");
}
