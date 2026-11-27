//! Recovering a library from a recovery code and a library on this computer that no longer
//! unlocks, as after a forgotten passphrase or a lost keystore entry: what it holds comes
//! back, read as a copy is; in its own folder, the new replica takes its place, and the old
//! replica's folder is set aside beside it ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::common::devices::{Devices, bytes, on_disk};
use crate::common::{protection, this_day};
use nibkeep_base::EntityId;
use nibkeep_crypto::RecoveryCode;
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	Joining, Library, NodeError, Recovered, RecoveredFrom, RecoveredSecrets, Unlocker,
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

/// Recovers the library in `directory` from the source in `source` with `code`.
fn recover(
	devices: &mut Devices,
	directory: &Path,
	(source, code): (&Path, &RecoveryCode),
) -> Result<(Library, Recovered), NodeError> {
	let joining = Joining::new("laptop again", "nib", &mut devices.rng).expect("names");
	let at = devices.tick();
	Library::recover(
		directory,
		(source, code),
		joining,
		(&protection(), None),
		&|| at,
		&mut devices.rng,
	)
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

/// A NAS and a laptop, with a note and a file that the laptop wrote, and a code made on
/// the laptop; then the laptop's library no longer unlocks, and the NAS is lost: the
/// laptop's library is closed. Returns the devices, the laptop's library's folder, the
/// note, the file's content and the code.
fn laptop_locked_out(name: &str) -> (Devices, PathBuf, EntityId, nibkeep_base::Hash, RecoveryCode) {
	let mut devices = Devices::new(name, false);
	let laptop = devices.pair("laptop", None);
	let written = note(&mut devices, laptop, "the boiler's code is 4711");
	let (_, content) = devices.add(laptop, "recipe.txt", &bytes(3, 20_000));
	let code = make_code(&mut devices, laptop);
	devices.sync(0, laptop);
	let directory = devices.folders[laptop].clone();
	drop(devices.libraries.remove(laptop));
	devices.folders.remove(laptop);
	(devices, directory, written, content, code)
}

/// Every file under `folder`, with its size and when it was last changed.
fn files_in(folder: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
	let mut files = Vec::new();
	let mut folders = vec![folder.to_owned()];
	while let Some(next) = folders.pop() {
		for entry in fs::read_dir(&next).expect("lists").flatten() {
			let metadata = entry.metadata().expect("reads");
			if metadata.is_dir() {
				folders.push(entry.path());
			} else {
				files.push((
					entry.path(),
					metadata.len(),
					metadata.modified().expect("a time"),
				));
			}
		}
	}
	files.sort();
	files
}

/// The names in a library's folder of replicas.
fn replica_folders(directory: &Path) -> Vec<String> {
	let mut names: Vec<String> = fs::read_dir(directory.join("replicas"))
		.expect("lists")
		.flatten()
		.map(|entry| entry.file_name().to_string_lossy().into_owned())
		.collect();
	names.sort();
	names
}

/// An empty folder on a new computer's disk.
fn new_computer(name: &str) -> PathBuf {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, new"));
	on_disk(&directory, &format!("{name} new"));
	directory
}

#[test]
fn a_library_that_no_longer_unlocks_comes_back_in_its_own_folder() {
	let (mut devices, directory, written, content, code) = laptop_locked_out("recovered here");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	assert!(
		Library::copies_in(&directory).is_empty(),
		"the replica is here, no copy"
	);
	let (library, recovered) =
		recover(&mut devices, &directory, (&directory, &code)).expect("recovers");
	// The old replica's folder is set aside beside the new one, under a hidden name.
	let RecoveredFrom::Library {
		replica: Some(old_replica),
		replaced: true,
		set_aside: Some(set_aside),
		..
	} = &recovered.from
	else {
		panic!("set aside: {:?}", recovered.from);
	};
	assert!(!old.exists());
	assert!(set_aside.is_dir());
	assert_eq!(set_aside.parent(), old.parent());
	assert!(
		set_aside
			.file_name()
			.is_some_and(|name| name.to_string_lossy().starts_with('.')),
		"{}",
		set_aside.display()
	);
	assert_eq!(library.directory().parent(), old.parent());
	assert_eq!(
		recovered.revoked.len(),
		2,
		"the NAS and the laptop's old replica"
	);
	assert!(
		recovered
			.revoked
			.iter()
			.any(|replica| replica.id == *old_replica)
	);
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
	for replica in library.replicas().expect("lists") {
		assert_eq!(replica.revoked, !replica.this, "{replica:?}");
	}
	// The folder holds one library again, which opens under its new passphrase and writes;
	// the old replica is no copy beside it.
	let here = library.directory().to_owned();
	drop(library);
	assert!(!here.join("recovering").exists());
	assert_eq!(Library::replica_folder(&directory).expect("here"), here);
	assert_eq!(Library::copies_in(&directory), []);
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
}

#[test]
fn the_library_that_no_longer_unlocks_is_never_kept() {
	let (mut devices, directory, _, _, code) = laptop_locked_out("kept, locked out");
	let laptop = devices.libraries[0]
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.device_name.as_deref() == Some("laptop"))
		.expect("the laptop")
		.id;
	let new = new_computer("kept, locked out");
	let joining = Joining::new("laptop again", "nib", &mut devices.rng).expect("names");
	let at = devices.tick();
	let mut recovering = Library::start_recovery(
		&new,
		(&directory, &code),
		joining,
		&protection(),
		&|| at,
		&mut devices.rng,
	)
	.expect("starts recovering");
	// The laptop's replica, which the recovery reads, is revoked in any case: the NAS alone
	// may be kept.
	let others = recovering.others().expect("lists");
	assert_eq!(
		others
			.iter()
			.filter_map(|replica| replica.device_name.as_deref())
			.collect::<Vec<_>>(),
		["nas"]
	);
	let error = recovering.others_named("laptop").expect_err("not kept");
	assert!(error.to_string().contains("no longer unlocks"), "{error}");
	let error = recovering.keep(&[laptop]).expect_err("not kept");
	assert!(matches!(error, NodeError::WhichDevice(_)), "{error}");
	let nas = others[0].id;
	recovering.keep(&[nas]).expect("keeps the NAS");
	let at = devices.tick();
	let (library, recovered) = recovering
		.finish(None, &|| at, &mut devices.rng)
		.expect("recovers");
	let ids = |replicas: &[nibkeep_node::Replica]| -> Vec<EntityId> {
		replicas.iter().map(|replica| replica.id).collect()
	};
	assert_eq!(ids(&recovered.revoked), [laptop]);
	assert_eq!(ids(&recovered.kept), [nas]);
	for replica in library.replicas().expect("lists") {
		assert_eq!(replica.revoked, replica.id == laptop, "{replica:?}");
	}
}

#[test]
fn a_library_that_does_not_say_which_replica_it_is_keeps_no_device() {
	let (mut devices, directory, _, _, code) = laptop_locked_out("kept, unnamed");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	fs::remove_file(old.join("library.cbor")).expect("damages it");
	let new = new_computer("kept, unnamed");
	let joining = Joining::new("laptop again", "nib", &mut devices.rng).expect("names");
	let at = devices.tick();
	let mut recovering = Library::start_recovery(
		&new,
		(&directory, &code),
		joining,
		&protection(),
		&|| at,
		&mut devices.rng,
	)
	.expect("starts recovering");
	// Its replica may be any of the others, so none is kept, and every one is revoked.
	let others: Vec<EntityId> = recovering
		.others()
		.expect("lists")
		.iter()
		.map(|replica| replica.id)
		.collect();
	let error = recovering.keep(&others).expect_err("none kept");
	assert!(
		error.to_string().contains("no device can be kept"),
		"{error}"
	);
	let at = devices.tick();
	let (_, recovered) = recovering
		.finish(None, &|| at, &mut devices.rng)
		.expect("recovers");
	assert_eq!(recovered.kept, []);
	assert_eq!(recovered.revoked.len(), 2);
}

#[test]
fn a_library_in_another_folder_is_left_as_it_was() {
	let (mut devices, directory, written, _, code) = laptop_locked_out("recovered elsewhere");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	let before = files_in(&directory);
	let new = new_computer("recovered elsewhere");
	let (library, recovered) = recover(&mut devices, &new, (&directory, &code)).expect("recovers");
	assert!(
		matches!(
			recovered.from,
			RecoveredFrom::Library {
				replica: Some(_),
				replaced: false,
				set_aside: None,
				..
			}
		),
		"{:?}",
		recovered.from
	);
	assert_eq!(
		notes(&library, written),
		[Value::Text("the boiler's code is 4711".to_owned())]
	);
	assert_eq!(recovered.revoked.len(), 2);
	// Nothing of the old library was written to, moved or removed.
	assert_eq!(files_in(&directory), before);
	assert_eq!(Library::replica_folder(&directory).expect("here"), old);
	// Its replica's folder, named itself, is a source too.
	drop(library);
	let third =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "recovered elsewhere, third");
	on_disk(&third, "recovered elsewhere third");
	let (_, recovered) = recover(&mut devices, &third, (&old, &code)).expect("recovers");
	assert!(
		matches!(&recovered.from, RecoveredFrom::Library { folder, .. } if *folder == old),
		"{:?}",
		recovered.from
	);
	assert_eq!(files_in(&directory), before);
}

#[test]
fn a_copy_of_a_library_that_no_longer_unlocks_comes_back_beside_it() {
	let (mut devices, directory, written, _, code) = laptop_locked_out("recovered from a copy");
	// The laptop's library, restored from the system's backup onto a new computer, is a copy
	// there, which is never written to.
	let new = new_computer("recovered from a copy");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	let copy = new.join("replicas").join(old.file_name().expect("a name"));
	nibkeep_testkit::copy_folder(&old, &copy);
	assert!(matches!(
		Library::replica_folder(&new),
		Err(NodeError::Copied)
	));
	let before = files_in(&copy);
	let (library, recovered) = recover(&mut devices, &new, (&new, &code)).expect("recovers");
	assert!(
		matches!(
			recovered.from,
			RecoveredFrom::Library {
				replica: Some(_),
				replaced: false,
				set_aside: None,
				..
			}
		),
		"{:?}",
		recovered.from
	);
	assert_eq!(
		notes(&library, written),
		[Value::Text("the boiler's code is 4711".to_owned())]
	);
	drop(library);
	assert_eq!(files_in(&copy), before);
	let copies = Library::copies_in(&new);
	assert_eq!(copies.len(), 1);
	assert_eq!(copies[0].folder, copy);
}

#[test]
fn a_wrong_code_or_source_leaves_the_library_as_it_was() {
	let (mut devices, directory, _, _, code) = laptop_locked_out("recovered here, wrong");
	let before = replica_folders(&directory);
	let other = RecoveryCode::generate(&mut devices.rng);
	let error = recover(&mut devices, &directory, (&directory, &other))
		.err()
		.expect("another code");
	assert!(matches!(error, NodeError::CodeOpensNothing), "{error}");
	assert_eq!(replica_folders(&directory), before);
	// A folder that holds no library, from a new computer and from where the library is,
	// which stays as it was.
	let empty = new_computer("recovered here, wrong");
	let error = recover(&mut devices, &empty, (&empty, &code))
		.err()
		.expect("no library");
	assert!(matches!(error, NodeError::NoRecoverySource(_)), "{error}");
	let error = recover(&mut devices, &directory, (&empty, &code))
		.err()
		.expect("a library here");
	assert!(matches!(error, NodeError::Exists), "{error}");
	// Another library is where it would go: the NAS's.
	let nas = devices.nas.clone();
	let nas_before = replica_folders(&nas);
	let error = recover(&mut devices, &nas, (&directory, &code))
		.err()
		.expect("a library there");
	assert!(matches!(error, NodeError::Exists), "{error}");
	assert_eq!(replica_folders(&nas), nas_before);
	assert_eq!(replica_folders(&directory), before);
}

#[test]
fn a_recovery_in_its_own_folder_cut_short_starts_over_or_finishes() {
	let (mut devices, directory, written, _, code) = laptop_locked_out("recovered here, cut");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	let old_name = old.file_name().expect("a name").to_owned();
	let (library, recovered) =
		recover(&mut devices, &directory, (&directory, &code)).expect("recovers");
	let first = library.directory().to_owned();
	let RecoveredFrom::Library {
		set_aside: Some(set_aside),
		..
	} = recovered.from
	else {
		panic!("set aside");
	};
	drop(library);
	// Cut short once the new replica was whole, before the old one was set aside: it is
	// done, and the library opens, and sets the old one aside.
	fs::rename(&set_aside, &old).expect("puts it back");
	fs::write(first.join("recovering"), old_name.as_encoded_bytes()).expect("marks");
	fs::write(first.join("recovered"), b"").expect("says it is whole");
	assert_eq!(Library::replica_folder(&directory).expect("here"), first);
	assert_eq!(Library::copies_in(&directory), []);
	drop(Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens"));
	assert!(!first.join("recovering").exists());
	assert!(!first.join("recovered").exists());
	assert!(!old.exists());
	assert!(set_aside.is_dir());
	// Cut short before the new replica was whole: the library does not open, and the old
	// replica's folder does not count as a second one.
	fs::rename(&set_aside, &old).expect("puts it back");
	fs::write(first.join("recovering"), old_name.as_encoded_bytes()).expect("marks");
	let error = Library::open(&directory, &Unlocker::Passphrase("battery staple"))
		.err()
		.expect("does not open");
	assert!(matches!(error, NodeError::RecoveryCutShort), "{error}");
	assert_eq!(Library::copies_in(&directory), []);
	// While another recovery holds the mark, as one that still runs, both are left alone.
	let running = fs::File::options()
		.write(true)
		.open(first.join("recovering"))
		.expect("opens");
	running.try_lock().expect("locks");
	let error = recover(&mut devices, &directory, (&directory, &code))
		.err()
		.expect("one runs");
	assert!(matches!(error, NodeError::Exists), "{error}");
	assert!(first.exists());
	assert!(old.exists());
	drop(running);
	// Run again, it removes the replica it left, which nobody knew, and starts over from
	// the old one.
	let (library, recovered) =
		recover(&mut devices, &directory, (&directory, &code)).expect("recovers again");
	assert!(!first.exists());
	assert!(!old.exists());
	assert!(matches!(
		recovered.from,
		RecoveredFrom::Library {
			set_aside: Some(_),
			..
		}
	));
	assert_eq!(
		recovered.revoked.len(),
		2,
		"the NAS and the old replica alone"
	);
	assert_eq!(
		notes(&library, written),
		[Value::Text("the boiler's code is 4711".to_owned())]
	);
}

#[test]
fn an_old_replica_that_cannot_be_set_aside_yet_goes_as_the_library_opens() {
	let (mut devices, directory, written, _, code) = laptop_locked_out("recovered here, held");
	let old = Library::replica_folder(&directory).expect("the laptop's replica");
	let name = old
		.file_name()
		.expect("a name")
		.to_string_lossy()
		.into_owned();
	// Something is in the way of setting it aside, as a file another program holds open on
	// Windows would be.
	let blocker = old.with_file_name(format!(".replaced-{name}"));
	fs::create_dir(&blocker).expect("makes");
	fs::write(blocker.join("in the way"), b"").expect("writes");
	let (library, recovered) =
		recover(&mut devices, &directory, (&directory, &code)).expect("recovers all the same");
	assert!(
		matches!(
			recovered.from,
			RecoveredFrom::Library {
				replaced: true,
				set_aside: None,
				..
			}
		),
		"{:?}",
		recovered.from
	);
	let first = library.directory().to_owned();
	drop(library);
	// The new replica is the library, and the old one, in its place, is no second one, nor
	// a copy; it waits behind the mark.
	assert!(old.exists());
	assert_eq!(Library::replica_folder(&directory).expect("here"), first);
	assert_eq!(Library::copies_in(&directory), []);
	let library =
		Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	assert_eq!(
		notes(&library, written),
		[Value::Text("the boiler's code is 4711".to_owned())]
	);
	drop(library);
	assert!(old.exists());
	assert!(first.join("recovering").exists());
	// Once nothing is in the way, the library sets it aside as it opens.
	fs::remove_dir_all(&blocker).expect("removes");
	drop(Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens"));
	assert!(!old.exists());
	assert!(blocker.is_dir());
	assert!(!first.join("recovering").exists());
	assert!(!first.join("recovered").exists());
	assert_eq!(Library::replica_folder(&directory).expect("here"), first);
}
