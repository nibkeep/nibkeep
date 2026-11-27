//! Checking a library: its log files, its blobs and its indexes.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{library, now};
use nibkeep_base::Hash;
use nibkeep_node::{Library, Problem, Unlocker};
use nibkeep_testkit::TestRng;

/// The files under a directory, recursively.
fn files(directory: &Path) -> Vec<PathBuf> {
	let mut files = Vec::new();
	let mut directories = vec![directory.to_owned()];
	while let Some(directory) = directories.pop() {
		let Ok(entries) = fs::read_dir(&directory) else {
			continue;
		};
		for entry in entries {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				directories.push(path);
			} else {
				files.push(path);
			}
		}
	}
	files
}

#[test]
fn a_sound_library_is_found_sound() {
	let mut rng = TestRng::new("sound");
	let (_, mut library) = library("sound", &mut rng);
	let report = library.verify(now()).expect("verifies");
	assert!(report.is_sound());
	assert_eq!(report.spaces.len(), 1);
	assert_eq!(report.spaces[0].name.as_deref(), Some("Personal"));
	assert_eq!(
		report.spaces[0].changes, 7,
		"what governs the space, the standard vocabulary, and a change for each entity"
	);
	assert_eq!(report.spaces[0].blobs, 0);
	nibkeep_testkit::assert_empty!(report.spaces[0].problems);
}

#[test]
fn a_damaged_blob_is_reported() {
	let mut rng = TestRng::new("damaged blob");
	let (directory, mut library) = library("damaged blob", &mut rng);
	let personal = library.personal_space();
	let content: Vec<u8> = (0..200_000_u32)
		.map(|index| u8::try_from(index % 251).expect("below 251"))
		.collect();
	let hash = library
		.import_blob(personal, content.as_slice(), now(), &mut rng)
		.expect("imports");
	assert_eq!(hash, Hash::of(&content));
	let report = library.verify(now()).expect("verifies");
	assert!(report.is_sound());
	assert_eq!(report.spaces[0].blobs, 1);

	let sealed = files(&nibkeep_store::blob_directory(&common::replica_folder(
		&directory,
	)))
	.into_iter()
	.find(|path| path.components().any(|part| part.as_os_str() == "blake3"))
	.expect("the sealed blob");
	let mut bytes = fs::read(&sealed).expect("reads");
	bytes[100_000] ^= 1;
	fs::write(&sealed, bytes).expect("damages");
	let report = library.verify(now()).expect("verifies");
	assert!(!report.is_sound());
	assert_eq!(report.spaces[0].problems, [Problem::DamagedBlob(hash)]);
}

/// File permissions that lock out the owner exist only on Unix.
#[cfg(unix)]
#[test]
fn files_that_cannot_be_read_are_reported_and_the_rest_checked() {
	use std::os::unix::fs::PermissionsExt;

	let mut rng = TestRng::new("unreadable");
	let (directory, mut library) = library("unreadable", &mut rng);
	let personal = library.personal_space();
	let first = library
		.import_blob(personal, b"a first scan".as_slice(), now(), &mut rng)
		.expect("imports");
	let second = library
		.import_blob(personal, b"a second scan".as_slice(), now(), &mut rng)
		.expect("imports");
	let sealed = files(&nibkeep_store::blob_directory(&common::replica_folder(
		&directory,
	)))
	.into_iter()
	.filter(|path| path.components().any(|part| part.as_os_str() == "blake3"))
	.collect::<Vec<_>>();
	assert_eq!(sealed.len(), 2);
	fs::set_permissions(&sealed[0], fs::Permissions::from_mode(0o000)).expect("locks");
	if fs::read(&sealed[0]).is_ok() {
		// Someone who may read every file, such as root, runs the tests.
		return;
	}
	let report = library.verify(now()).expect("verifies");
	fs::set_permissions(&sealed[0], fs::Permissions::from_mode(0o600)).expect("unlocks");
	assert_eq!(report.spaces[0].blobs, 2, "both are checked");
	let [Problem::UnreadableBlob(content, _)] = report.spaces[0].problems.as_slice() else {
		panic!("one unreadable blob: {:?}", report.spaces[0].problems);
	};
	assert!([first, second].contains(content));
	assert_eq!(report.problems(), 1);
}

#[test]
fn a_sealed_file_without_a_key_is_reported() {
	let mut rng = TestRng::new("keyless");
	let (directory, mut library) = library("keyless", &mut rng);
	let personal = library.personal_space();
	library
		.import_blob(personal, b"a scan".as_slice(), now(), &mut rng)
		.expect("imports");
	let sealed = files(&nibkeep_store::blob_directory(&common::replica_folder(
		&directory,
	)))
	.into_iter()
	.find(|path| path.components().any(|part| part.as_os_str() == "blake3"))
	.expect("the sealed blob");
	let lost = Hash::of(b"a key that was lost");
	let shown = lost.to_string();
	let digest = shown.trim_start_matches("blake3:");
	let blake3 = sealed
		.ancestors()
		.nth(2)
		.expect("the algorithm's directory")
		.to_owned();
	let target = blake3.join(&digest[..2]).join(digest);
	fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
	fs::copy(&sealed, &target).expect("copies");
	fs::write(blake3.parent().expect("the blobs").join("stray"), b"?").expect("writes");
	let report = library.verify(now()).expect("verifies");
	assert_eq!(report.spaces[0].blobs, 1);
	assert_eq!(
		report.spaces[0].problems,
		[
			Problem::KeylessBlob(lost),
			Problem::UnknownFile(blake3.parent().expect("the blobs").join("stray")),
		]
	);
}

/// Forges the signature of the last change of a library's personal space, with its ID
/// stored to match. With the replica's unlocked keys, the forger also writes the tag that
/// the replica stores the change with.
fn forge_last_change(directory: &Path, personal: nibkeep_base::SpaceId, unlocked: bool) {
	let log = nibkeep_store::log_path(&common::replica_folder(directory), personal);
	let connection = rusqlite::Connection::open(&log).expect("opens the log file");
	let (sequence, mut change, mut tag): (i64, Vec<u8>, Vec<u8>) = connection
		.query_row(
			"SELECT sequence, change, tag FROM changes ORDER BY sequence DESC LIMIT 1",
			[],
			|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
		)
		.expect("the last change");
	// The last byte of a change is the last byte of its signature.
	*change.last_mut().expect("bytes") ^= 1;
	// Its ID is the hash of all of it, and is stored with it.
	let id = Hash::of(&change).to_multihash();
	if unlocked {
		let key_file =
			nibkeep_keys::KeyFile::read(&common::replica_folder(directory).join("keys.cbor"))
				.expect("reads the keys");
		let compartment = key_file
			.unlock_replica(&Unlocker::Passphrase("correct horse"))
			.expect("unlocks");
		let replica = key_file.replica(&compartment).expect("opens");
		tag = replica
			.storage
			.derive("nibkeep 2026-10 stored changes")
			.tag(&change)
			.to_vec();
	}
	connection
		.execute(
			"UPDATE changes SET change = ?1, id = ?2, tag = ?3 WHERE sequence = ?4",
			rusqlite::params![change, id, tag, sequence],
		)
		.expect("forges the signature");
}

#[test]
fn a_change_that_this_device_did_not_store_is_found_when_read() {
	let mut rng = TestRng::new("forged change");
	let (directory, library) = library("forged change", &mut rng);
	let personal = library.personal_space();
	drop(library);
	forge_last_change(&directory, personal, false);
	// Whoever holds a key of the space, but not this device's unlocked keys, cannot pass
	// a change off as stored by this device: opening reads only what changed since the
	// state file, and reading the change, as checking the library does, finds it.
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	let report = library.verify(now()).expect("verifies");
	assert!(!report.is_sound());
	assert!(
		report.spaces[0].problems.iter().any(
			|problem| matches!(problem, Problem::DamagedChange(_, why) if why.contains("not stored by this device"))
		),
		"{:?}",
		report.spaces[0].problems
	);
}

#[test]
fn a_forged_signature_with_the_devices_own_tag_is_reported() {
	let mut rng = TestRng::new("forged signature");
	let (directory, library) = library("forged signature", &mut rng);
	let personal = library.personal_space();
	drop(library);
	forge_last_change(&directory, personal, true);
	// With this device's unlocked keys, the forger could sign as the device too; checking
	// still finds the signature that does not match.
	let mut library = Library::open(&directory, &Unlocker::Passphrase("correct horse"))
		.expect("opens despite the forged signature");
	let report = library.verify(now()).expect("verifies");
	assert!(matches!(
		report.spaces[0].problems.as_slice(),
		[Problem::ForgedChange(_)]
	));
}
