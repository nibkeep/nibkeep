//! Checkouts across devices: a purge of a file's content on one device destroys its sealed
//! copies on the others, and deletes their unchanged checkouts after their next sync, while
//! an edited one stays ([technical design: checkouts]).
//!
//! [technical design: checkouts]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#92-checkouts

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{TIMEOUT, library, now, pair_with, sync_in_memory};
use nibkeep_base::EntityId;
use nibkeep_node::{
	CopyState, Library, NetworkOptions, PairingCode, answer, resolved_path, sync_with_others,
};
use nibkeep_std::CONTENT;
use nibkeep_testkit::TestRng;

/// A folder of its own, as the file system resolves it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	resolved_path(&made).expect("resolves")
}

/// A library with one file, and a second replica of it, on the desk, which holds the file
/// too; the file is returned.
fn paired_with_a_file(name: &str, rng: &mut TestRng) -> (Library, Library, EntityId) {
	let (_, mut laptop) = library(&format!("{name} laptop"), rng);
	let files = folder(&format!("{name} files"));
	fs::write(files.join("notes.txt"), "Milk, eggs, bread.\n").expect("writes");
	let personal = laptop.personal_space();
	let added = laptop
		.add_file(personal, &files.join("notes.txt"), now() + 1_000, rng)
		.expect("adds");
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name} desk"));
	fs::remove_dir(&desk_directory).expect("removes");
	let code = PairingCode::generate(rng);
	let (paired, joined) = pair_with(&mut laptop, &desk_directory, &code, &code, rng);
	paired.expect("the laptop paired");
	let desk = joined.expect("the desk joined").library;
	(laptop, desk, added.file)
}

/// Whether a library holds a value of the file's content.
fn has_content(library: &Library, file: EntityId) -> bool {
	let personal = library.personal_space();
	let view = library
		.entity(personal, file, common::this_day())
		.expect("reads")
		.expect("the file");
	view.fields.iter().any(|field| field.field == CONTENT)
}

fn read(path: &Path) -> String {
	fs::read_to_string(path).expect("reads")
}

#[test]
fn a_purge_deletes_the_unchanged_checkouts_of_other_devices_when_they_sync() {
	let mut rng = TestRng::new("checkouts purged elsewhere");
	let (mut laptop, mut desk, file) = paired_with_a_file("checkouts purged", &mut rng);
	let personal = desk.personal_space();
	let out = folder("checkouts purged, out");
	let kept = folder("checkouts purged, kept");
	let checked_out = desk
		.checkout(personal, file, &out, now() + 100_000, &mut rng)
		.expect("checks out");
	assert!(checked_out.written);
	assert_eq!(checked_out.path, out.join("notes.txt"));
	assert_eq!(read(&checked_out.path), "Milk, eggs, bread.\n");
	desk.checkout(personal, file, &kept, now() + 101_000, &mut rng)
		.expect("checks out");
	fs::write(kept.join("notes.txt"), "Eggs.\n").expect("edits");
	let (first, second) = sync_in_memory(&mut desk, &mut laptop, now() + 110_000, (1, 2));
	first.expect("the desk synced");
	second.expect("the laptop synced");

	// The laptop sees the desk's checkouts, and purges their content with the file's and
	// the origin's; it has none of its own to delete.
	let purge = laptop
		.purgeable(personal, file, &CONTENT, None)
		.expect("can purge");
	assert_eq!(purge.statements.len(), 4, "{purge:?}");
	assert_eq!(purge.blobs, 1);
	assert!(purge.checkouts.is_empty(), "{purge:?}");
	let purge = laptop
		.purge(personal, file, &CONTENT, None, now() + 120_000, &mut rng)
		.expect("purges");
	nibkeep_testkit::assert_empty!(purge.checkouts);
	assert!(!has_content(&laptop, file));
	assert!(read(&out.join("notes.txt")).starts_with("Milk"));

	// The desk learns of the purge when it syncs, over the network, as nib sync does.
	let laptop_network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let desk_network = desk.network(NetworkOptions::LOCAL).expect("starts");
	desk_network.remember(
		laptop_network.endpoint(),
		&laptop_network.local_addresses(),
		None,
	);
	let laptop_directory = laptop.close();
	let mut answering = TestRng::new("answering");
	let synced = std::thread::scope(|scope| {
		let answered = scope.spawn(|| {
			answer(
				&laptop_directory,
				&laptop_network,
				Some(TIMEOUT),
				&|| now() + 130_000,
				&mut answering,
			)
		});
		let synced = sync_with_others(
			&mut desk,
			&desk_network,
			TIMEOUT,
			&|| now() + 130_000,
			&mut rng,
		)
		.expect("syncs");
		let answered = answered
			.join()
			.expect("the laptop ran")
			.expect("answers")
			.expect("a sync came");
		answered.synced.expect("the laptop synced");
		nibkeep_testkit::assert_empty!(answered.deleted);
		synced
	});
	assert_eq!(synced.len(), 1);
	synced[0].synced.as_ref().expect("synced");
	assert_eq!(synced[0].deleted, [out.join("notes.txt")]);
	assert!(!out.join("notes.txt").exists());
	assert_eq!(read(&kept.join("notes.txt")), "Eggs.\n");
	assert!(!has_content(&desk, file));
	let report = desk.verify(now() + 140_000).expect("verifies");
	assert_eq!(report.spaces[0].blobs, 0, "{report:?}");
	assert!(report.spaces[0].problems.is_empty(), "{report:?}");

	// What is left is the edited copy, whose record says so.
	let checkouts = desk.checkouts(personal).expect("lists");
	assert_eq!(checkouts.len(), 1, "{checkouts:?}");
	assert_eq!(checkouts[0].path, kept.join("notes.txt"));
	assert_eq!(checkouts[0].copy, CopyState::Edited);
	assert_eq!(checkouts[0].content, None);
}

#[test]
fn a_files_content_reads_from_its_copy_or_where_it_was_found() {
	let mut rng = TestRng::new("file content");
	let (laptop, desk, file) = paired_with_a_file("file content", &mut rng);
	let personal = laptop.personal_space();
	let view = laptop
		.entity(personal, file, common::this_day())
		.expect("reads")
		.expect("the file");
	let nibkeep_model::Value::Blob(content) = view.values(&CONTENT)[0].value else {
		panic!("a blob");
	};
	let notes = b"Milk, eggs, bread.\n".to_vec();
	// Where it was found, and from the copy that the desk was sent.
	assert_eq!(
		laptop
			.file_content(personal, &content, 1_000)
			.expect("reads"),
		Some(notes.clone())
	);
	assert_eq!(
		desk.file_content(personal, &content, 1_000).expect("reads"),
		Some(notes.clone())
	);
	// Too large, or unknown, it is not read.
	assert_eq!(
		laptop.file_content(personal, &content, 5).expect("reads"),
		None
	);
	let other = nibkeep_base::Hash::of(b"other");
	assert_eq!(
		laptop.file_content(personal, &other, 1_000).expect("reads"),
		None
	);
}

#[test]
fn a_file_synced_from_another_device_checks_out_there_and_its_edits_come_back() {
	let mut rng = TestRng::new("checkouts synced");
	let (mut laptop, mut desk, file) = paired_with_a_file("checkouts synced", &mut rng);
	let personal = desk.personal_space();
	let out = folder("checkouts synced, out");
	let checked_out = desk
		.checkout(personal, file, &out, now() + 100_000, &mut rng)
		.expect("checks out");
	fs::write(&checked_out.path, "Milk, eggs, bread, butter.\n").expect("edits");
	let version = desk
		.record_edits(
			personal,
			checked_out.checkout,
			false,
			now() + 101_000,
			&mut rng,
		)
		.expect("records")
		.version
		.expect("a new version");
	let ended = desk
		.end_checkout(personal, checked_out.checkout, now() + 102_000, &mut rng)
		.expect("ends");
	assert!(ended.deleted);
	assert!(!ended.sealed);
	assert!(!checked_out.path.exists());
	let (first, second) = sync_in_memory(&mut desk, &mut laptop, now() + 110_000, (1, 2));
	first.expect("the desk synced");
	second.expect("the laptop synced");

	// The laptop has the new version, sealed, and writes it out.
	let back = folder("checkouts synced, back");
	let checked_out = laptop
		.checkout(personal, file, &back, now() + 120_000, &mut rng)
		.expect("checks out");
	assert_eq!(checked_out.content, version);
	assert_eq!(read(&checked_out.path), "Milk, eggs, bread, butter.\n");
	// The desk's checkout ended, and the laptop's is the only one, on the laptop alone.
	nibkeep_testkit::assert_empty!(desk.checkouts(personal).expect("lists"));
	assert_eq!(laptop.checkouts(personal).expect("lists").len(), 1);
}

#[test]
fn the_device_that_purges_tells_unchanged_copies_by_their_hashes() {
	let mut rng = TestRng::new("checkouts purged here");
	let (_, mut library) = library("checkouts purged here", &mut rng);
	let personal = library.personal_space();
	let files = folder("checkouts purged here, files");
	fs::write(files.join("notes.txt"), "Milk, eggs, bread.\n").expect("writes");
	let file = library
		.add_file(personal, &files.join("notes.txt"), now(), &mut rng)
		.expect("adds")
		.file;
	let [touched, edited, away] =
		["touched", "edited", "away"].map(|name| folder(&format!("checkouts purged here, {name}")));
	fs::create_dir(away.join("sub")).expect("makes");
	for place in [&touched, &edited, &away.join("sub")] {
		library
			.checkout(personal, file, place, now() + 1_000, &mut rng)
			.expect("checks out");
	}
	let set_time = |path: &Path, time: std::time::SystemTime| {
		fs::File::options()
			.write(true)
			.open(path)
			.and_then(|file| file.set_modified(time))
			.expect("sets the time");
	};
	// Saved again unchanged, at another time: a copy of the content all the same.
	set_time(
		&touched.join("notes.txt"),
		std::time::SystemTime::UNIX_EPOCH,
	);
	// Edited in the second it was written, keeping its size: the person's work.
	let written = fs::metadata(edited.join("notes.txt"))
		.and_then(|metadata| metadata.modified())
		.expect("a time");
	fs::write(edited.join("notes.txt"), "Milk, eggs, jam!!.\n").expect("edits");
	set_time(&edited.join("notes.txt"), written);
	// On a drive that is not plugged in.
	fs::rename(away.join("sub"), away.join("unplugged")).expect("moves");

	let purge = library
		.purge(personal, file, &CONTENT, None, now() + 2_000, &mut rng)
		.expect("purges");
	let deleted: Vec<_> = purge
		.checkouts
		.iter()
		.filter(|checkout| checkout.deleted)
		.map(|checkout| checkout.path.clone())
		.collect();
	assert_eq!(deleted, [touched.join("notes.txt")]);
	assert!(!touched.join("notes.txt").exists());
	assert_eq!(read(&edited.join("notes.txt")), "Milk, eggs, jam!!.\n");

	// Later settling, without the hashes, keeps the edited copy still.
	let none = std::collections::BTreeMap::new();
	nibkeep_testkit::assert_empty!(
		library
			.settle_checkouts(&none, now() + 3_000, &mut rng)
			.expect("settles")
	);
	assert_eq!(read(&edited.join("notes.txt")), "Milk, eggs, jam!!.\n");
	// The copy out of reach kept its record, and goes once it is back.
	fs::rename(away.join("unplugged"), away.join("sub")).expect("moves");
	let settled = library
		.settle_checkouts(&none, now() + 4_000, &mut rng)
		.expect("settles");
	assert_eq!(settled, [away.join("sub").join("notes.txt")]);
	let left: Vec<_> = library
		.checkouts(personal)
		.expect("lists")
		.into_iter()
		.map(|checkout| (checkout.path, checkout.copy))
		.collect();
	assert_eq!(left, [(edited.join("notes.txt"), CopyState::Edited)]);
}
