//! A backup is a folder that the person's computers fill with the sealed changes, keys and
//! blobs of chosen spaces, and take in from: it holds nothing readable without the space's
//! keys, each file is written whole, a purge reaches it, and its copies are checked
//! ([technical design: backups]).
//!
//! [technical design: backups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#910-backups

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::devices::{Devices, bytes};
use nibkeep_base::{EntityId, Hash};
use nibkeep_node::{
	Backup, BackupEdit, BackupUpdate, DAY, Drive, NewBackup, NodeError, declare_drive,
};
use nibkeep_std::CONTENT;

/// Four weeks and a day, in milliseconds: after it, an update reads a blob file again.
const AFTER_A_ROUND: u64 = 29 * 24 * 60 * 60 * 1000;

/// An empty folder for a backup.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, backup"))
		.join("Backup")
}

/// Makes a backup of every space, from library `device`.
fn add(devices: &mut Devices, device: usize, folder: &Path, new: &NewBackup) -> Backup {
	let at = devices.tick();
	devices.libraries[device]
		.add_backup(folder, new, at, &mut devices.rng)
		.expect("adds")
		.backup
}

/// A backup named "Blue stick" of every space.
fn blue() -> NewBackup {
	NewBackup {
		name: "Blue stick".to_owned(),
		..NewBackup::default()
	}
}

/// Updates the backup from library `device`, at the devices' time.
fn update(devices: &mut Devices, device: usize, backup: &Backup, folder: &Path) -> BackupUpdate {
	let at = devices.tick();
	devices.libraries[device]
		.update_backup(backup, folder, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another")
}

/// Every file under a folder, with its bytes.
fn files_under(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
	let mut found = Vec::new();
	let mut folders = vec![folder.to_owned()];
	while let Some(at) = folders.pop() {
		for entry in fs::read_dir(&at).expect("reads").flatten() {
			let path = entry.path();
			if path.is_dir() {
				folders.push(path);
			} else {
				let bytes = fs::read(&path).expect("reads");
				found.push((path, bytes));
			}
		}
	}
	found
}

/// The packs of the backup.
fn packs(folder: &Path) -> Vec<PathBuf> {
	files_under(&folder.join("spaces"))
		.into_iter()
		.map(|(path, _)| path)
		.collect()
}

/// The backup's blob files.
fn blob_files(folder: &Path) -> Vec<PathBuf> {
	let blobs = folder.join("blobs");
	if !blobs.exists() {
		return Vec::new();
	}
	files_under(&blobs)
		.into_iter()
		.map(|(path, _)| path)
		.collect()
}

/// Whether `bytes` holds `part`.
fn holds(bytes: &[u8], part: &[u8]) -> bool {
	bytes.windows(part.len()).any(|window| window == part)
}

/// A file's bytes that are easy to find: a phrase repeated.
fn findable(phrase: &str, len: usize) -> Vec<u8> {
	phrase.bytes().cycle().take(len).collect()
}

#[test]
fn a_backup_holds_nothing_readable_and_another_device_takes_in_what_it_lacks() {
	let mut devices = Devices::new("backup takes in", false);
	let laptop = devices.pair("laptop", None);
	let folder = folder("backup takes in");
	let backup = add(&mut devices, 0, &folder, &blue());
	// The laptop learns of the backup, and of everything until now.
	devices.sync(0, laptop);
	let recipe = findable("Grandma's apple pie: three apples. ", 40_000);
	let video = findable("first steps on the beach. ", 3_000_000);
	let (_, small) = devices.add(0, "recipe.txt", &recipe);
	let (_, large) = devices.add(0, "steps.mov", &video);
	let written = update(&mut devices, 0, &backup, &folder);
	assert!(written.written.changes >= 2, "{written:?}");
	assert_eq!(written.written.blobs, 2, "{written:?}");
	// Nothing on the backup reads without the space's keys: no key file, no plain bytes.
	for (path, bytes) in files_under(&folder) {
		assert!(!holds(&bytes, &recipe[..64]), "{}", path.display());
		assert!(!holds(&bytes, &video[..64]), "{}", path.display());
		assert_ne!(
			path.file_name().and_then(|name| name.to_str()),
			Some("keys.cbor")
		);
	}
	// The large file has a file of its own, the small one is in a pack.
	assert_eq!(blob_files(&folder).len(), 1);
	// The laptop, which never synced since, takes the changes and files from the backup.
	assert!(!devices.holds(laptop, &small));
	let taken = update(&mut devices, laptop, &backup, &folder);
	assert!(taken.taken.changes >= 2, "{taken:?}");
	assert_eq!(taken.taken.blobs, 2, "{taken:?}");
	assert!(devices.holds(laptop, &small));
	assert!(devices.holds(laptop, &large));
	// And has nothing to give back.
	assert_eq!(taken.written.blobs, 0, "{taken:?}");
}

#[test]
fn an_update_writes_only_what_is_new_and_merges_small_packs() {
	let mut devices = Devices::new("backup writes what is new", false);
	let folder = folder("backup writes what is new");
	let backup = add(&mut devices, 0, &folder, &blue());
	let first = update(&mut devices, 0, &backup, &folder);
	assert!(first.written.changes > 0);
	let again = update(&mut devices, 0, &backup, &folder);
	assert_eq!(
		again.written,
		nibkeep_node::BackupMoved::default(),
		"{again:?}"
	);
	assert_eq!(
		again.taken,
		nibkeep_node::BackupMoved::default(),
		"{again:?}"
	);
	// Each update with something new writes a small pack; past 16, they are merged.
	for n in 0..20_u8 {
		devices.add(0, &format!("note {n}.txt"), &bytes(n, 100));
		update(&mut devices, 0, &backup, &folder);
	}
	let count = packs(&folder).len();
	assert!(count <= 17, "{count} packs");
	// Nothing was lost by merging: every file is still there, and nothing is to write.
	let last = update(&mut devices, 0, &backup, &folder);
	assert_eq!(
		last.written,
		nibkeep_node::BackupMoved::default(),
		"{last:?}"
	);
	assert_eq!(last.held, 20, "{last:?}");
}

#[test]
fn a_purge_reaches_the_backup() {
	let mut devices = Devices::new("backup purged", false);
	let folder = folder("backup purged");
	// A backup that does not wait before purging.
	let at_once = NewBackup {
		waits: Some(0),
		..blue()
	};
	let backup = add(&mut devices, 0, &folder, &at_once);
	let (small_file, small) = devices.add(0, "small.txt", &findable("small ", 50_000));
	let (large_file, large) = devices.add(0, "large.bin", &findable("large ", 2_000_000));
	let first = update(&mut devices, 0, &backup, &folder);
	assert_eq!(first.held, 2, "{first:?}");
	let space = devices.personal();
	for file in [small_file, large_file] {
		let at = devices.tick();
		devices.libraries[0]
			.purge(space, file, &CONTENT, None, at, &mut devices.rng)
			.expect("purges");
	}
	assert!(!devices.holds(0, &small) && !devices.holds(0, &large));
	let purged = update(&mut devices, 0, &backup, &folder);
	assert!(purged.purged >= 4, "two keys and two blobs: {purged:?}");
	assert_eq!(purged.held, 0, "{purged:?}");
	assert_eq!(blob_files(&folder), Vec::<PathBuf>::new());
	// A device that never heard of the purge takes in nothing of the purged files.
	let laptop = devices.pair("laptop", None);
	assert!(!devices.holds(laptop, &small));
	let taken = update(&mut devices, laptop, &backup, &folder);
	assert_eq!(taken.taken.blobs, 0, "{taken:?}");
}

#[test]
fn a_drive_pulled_out_loses_at_most_the_file_being_written() {
	let mut devices = Devices::new("backup pulled out", false);
	let folder = folder("backup pulled out");
	let stick = folder.parent().expect("a parent").to_owned();
	declare_drive(
		&stick,
		Drive {
			serial: Some(b"stick".to_vec()),
			unjournaled: true,
			..Drive::default()
		},
		None,
	);
	let backup = add(&mut devices, 0, &folder, &blue());
	devices.add(0, "large.bin", &bytes(3, 2_000_000));
	let first = update(&mut devices, 0, &backup, &folder);
	assert!(!first.unclean);
	assert_eq!(first.checked, 0, "written just now: {first:?}");
	// Pulled out while it wrote a file: the mark stays, and a file half written.
	fs::write(folder.join("in-use"), b"").expect("writes");
	fs::create_dir_all(folder.join("tmp")).expect("makes");
	fs::write(folder.join("tmp").join("half"), b"half a pa").expect("writes");
	let after = update(&mut devices, 0, &backup, &folder);
	assert!(after.unclean, "{after:?}");
	// The half-written file is gone, and every copy is read back before it counts.
	assert!(!folder.join("tmp").join("half").exists());
	assert_eq!(after.checked, 1, "{after:?}");
	assert!(!folder.join("in-use").exists());
}

#[test]
fn a_damaged_copy_is_written_again_and_a_damaged_pack_set_aside() {
	let mut devices = Devices::new("backup damaged", false);
	let folder = folder("backup damaged");
	let backup = add(&mut devices, 0, &folder, &blue());
	devices.add(0, "large.bin", &bytes(4, 2_000_000));
	update(&mut devices, 0, &backup, &folder);
	let [blob] = blob_files(&folder).try_into().expect("one blob file");
	let mut damaged = fs::read(&blob).expect("reads");
	damaged[1_000] ^= 0xFF;
	fs::write(&blob, &damaged).expect("writes");
	// Checked again a round later: found damaged, and written again from the replica's copy.
	devices.at += AFTER_A_ROUND;
	let checked = update(&mut devices, 0, &backup, &folder);
	assert_eq!((checked.damaged, checked.repaired), (1, 1), "{checked:?}");
	let sealed: Hash = format!(
		"blake3:{}",
		blob.file_name()
			.and_then(|name| name.to_str())
			.expect("a name")
	)
	.parse()
	.expect("a sealed hash");
	assert_eq!(Hash::of(&fs::read(&blob).expect("reads")), sealed);
	// A pack cut short is set aside, and what it held written again.
	for pack in packs(&folder) {
		let bytes = fs::read(&pack).expect("reads");
		fs::write(&pack, &bytes[..bytes.len() / 2]).expect("writes");
	}
	let after = update(&mut devices, 0, &backup, &folder);
	assert!(after.set_aside >= 1, "{after:?}");
	assert!(after.written.changes > 0, "{after:?}");
	assert!(folder.join("damaged").exists());
	// A pack that is what its name says but does not read here, as a later version's, is
	// left as it is.
	let later = b"a pack of a later version".to_vec();
	let space_folder = packs(&folder)[0]
		.parent()
		.expect("a space's folder")
		.to_owned();
	let digest = Hash::of(&later).to_string();
	let unread = space_folder.join(format!(
		"{}.pack",
		digest.strip_prefix("blake3:").expect("BLAKE3")
	));
	fs::write(&unread, &later).expect("writes");
	let left = update(&mut devices, 0, &backup, &folder);
	assert_eq!(left.set_aside, 0, "{left:?}");
	assert!(unread.exists());
}

#[test]
fn a_backup_held_by_another_program_is_left_alone() {
	let mut devices = Devices::new("backup held", false);
	let folder = folder("backup held");
	let backup = add(&mut devices, 0, &folder, &blue());
	let lock = fs::File::options()
		.write(true)
		.create(true)
		.truncate(false)
		.open(folder.join("lock"))
		.expect("opens");
	lock.lock().expect("locks");
	let at = devices.tick();
	let held = devices.libraries[0]
		.update_backup(&backup, &folder, &|| at, &|| false)
		.expect("reads");
	assert!(held.is_none());
	drop(lock);
	update(&mut devices, 0, &backup, &folder);
}

#[test]
fn a_backup_of_chosen_policies_keeps_only_their_files() {
	let mut devices = Devices::new("backup of policies", false);
	let space = devices.personal();
	let (passport, scan) = devices.add(0, "passport.pdf", &findable("passport ", 20_000));
	let (_, holiday) = devices.add(0, "holiday.jpg.bin", &findable("holiday ", 20_000));
	let critical = devices.libraries[0]
		.find_policy(space, "Critical")
		.expect("Critical");
	let at = devices.tick();
	devices.libraries[0]
		.set_policy(space, passport, Some(critical), at, &mut devices.rng)
		.expect("sets");
	let folder = folder("backup of policies");
	let backup = add(
		&mut devices,
		0,
		&folder,
		&NewBackup {
			name: "Office stick".to_owned(),
			policies: vec![critical],
			away: true,
			..NewBackup::default()
		},
	);
	assert!(backup.away);
	let written = update(&mut devices, 0, &backup, &folder);
	assert_eq!((written.written.blobs, written.held), (1, 1), "{written:?}");
	// Another device takes in the passport from it, and not the holiday.
	let laptop = devices.pair("laptop", None);
	let _ = (scan, holiday, laptop);
}

#[test]
fn a_backup_is_made_only_in_an_empty_folder_outside_a_library() {
	let mut devices = Devices::new("backup refused", false);
	let at = devices.tick();
	let library = devices.folders[0].clone();
	let refused =
		devices.libraries[0].add_backup(&library.join("inside"), &blue(), at, &mut devices.rng);
	assert!(
		matches!(refused, Err(NodeError::BackupFolder(_))),
		"{refused:?}"
	);
	let full = folder("backup refused");
	fs::create_dir_all(&full).expect("makes");
	fs::write(full.join("photo.jpg"), b"mine").expect("writes");
	let refused = devices.libraries[0].add_backup(&full, &blue(), at, &mut devices.rng);
	assert!(
		matches!(refused, Err(NodeError::BackupFolder(_))),
		"{refused:?}"
	);
	assert_eq!(devices.libraries[0].backups().expect("lists").len(), 0);
	// A folder that names another backup is not this one.
	let first = folder("backup refused, first");
	let made = add(&mut devices, 0, &first, &blue());
	let second = folder("backup refused, second");
	let other = add(
		&mut devices,
		0,
		&second,
		&NewBackup {
			name: "Red stick".to_owned(),
			..NewBackup::default()
		},
	);
	let at = devices.tick();
	let wrong = devices.libraries[0].update_backup(&other, &first, &|| at, &|| false);
	assert!(
		matches!(wrong, Err(NodeError::NotTheBackup { .. })),
		"{wrong:?}"
	);
	// Each is found where its folder names it: the one remembered at the first folder, and
	// the other at its own, under the drive it is on, not where it was wrongly remembered.
	let found = devices.libraries[0]
		.find_backups(&[(made.id, first.clone()), (other.id, first.clone())].into())
		.expect("finds");
	let found: Vec<(EntityId, PathBuf)> = found
		.into_iter()
		.map(|(backup, at)| (backup.id, fs::canonicalize(at).expect("there")))
		.collect();
	assert!(found.contains(&(made.id, fs::canonicalize(&first).expect("there"))));
	assert!(!found.contains(&(other.id, fs::canonicalize(&first).expect("there"))));
	// Removed, it is forgotten, and its folder stays.
	let at = devices.tick();
	devices.libraries[0]
		.remove_backup(made.id, at, &mut devices.rng)
		.expect("removes");
	assert_eq!(devices.libraries[0].backups().expect("lists").len(), 1);
	assert!(first.join("backup.cbor").exists());
}

#[test]
fn a_space_taken_on_keeps_the_files_of_the_same_policies() {
	let mut devices = Devices::new("backup takes on a space", false);
	let space = devices.personal();
	let at = devices.tick();
	devices.libraries[0]
		.install_policies(space, None, at, &mut devices.rng)
		.expect("installs");
	let critical = devices.libraries[0]
		.find_policy(space, "Critical")
		.expect("Critical");
	let folder = folder("backup takes on a space");
	let backup = add(
		&mut devices,
		0,
		&folder,
		&NewBackup {
			name: "Office stick".to_owned(),
			spaces: vec![space],
			policies: vec![critical],
			..NewBackup::default()
		},
	);
	let at = devices.tick();
	let family = devices.libraries[0]
		.create_space("Family", at, &mut devices.rng)
		.expect("makes");
	let at = devices.tick();
	devices.libraries[0]
		.edit_backup(
			backup.id,
			&BackupEdit {
				spaces: Some(vec![space, family]),
				..BackupEdit::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("edits");
	let edited = devices.libraries[0]
		.find_backup("Office stick")
		.expect("finds");
	assert_eq!(edited.spaces.len(), 2);
	assert_eq!(
		edited.policies.len(),
		2,
		"Critical of each space: {edited:?}"
	);
	assert!(edited.policies.contains(&critical));
}

/// How long a replica waits before it issues its account again while its state stays the
/// same, in milliseconds, and a little more.
const ACCOUNTS_AGAIN: u64 = 61_000;

#[test]
fn a_backup_counts_as_a_disk_on_every_device_but_makes_no_archive_node() {
	let mut devices = Devices::new("backup counts", false);
	let laptop = devices.pair("laptop", None);
	let (file, content) = devices.add(0, "steps.mov", &bytes(5, 2_000_000));
	let folder = folder("backup counts");
	let backup = add(&mut devices, 0, &folder, &blue());
	update(&mut devices, 0, &backup, &folder);
	let space = devices.personal();
	let at = devices.tick();
	let copies = devices.libraries[0]
		.copies_of(space, file, at)
		.expect("counts");
	let on_backup = copies
		.iter()
		.find(|copy| !copy.disk.backups.is_empty())
		.expect("the backup counts");
	assert!(on_backup.counts && on_backup.disk.archive, "{on_backup:?}");
	assert_eq!(
		on_backup.disk.backups[0].name.as_deref(),
		Some("Blue stick")
	);
	assert_eq!(copies.len(), 2, "{copies:?}");
	// Its account travels with syncs: the laptop counts it too, and keeps every file still,
	// as the space has no archive node.
	devices.sync(laptop, 0);
	let at = devices.tick();
	let copies = devices.libraries[laptop]
		.copies_of(space, file, at)
		.expect("counts");
	assert!(
		copies
			.iter()
			.any(|copy| copy.disk.backups.iter().any(|on| on.backup == backup.id)),
		"{copies:?}"
	);
	assert!(devices.holds(laptop, &content));
	// Removed, it counts no longer.
	let at = devices.tick();
	devices.libraries[0]
		.remove_backup(backup.id, at, &mut devices.rng)
		.expect("removes");
	let copies = devices.libraries[0]
		.copies_of(space, file, at)
		.expect("counts");
	assert!(
		copies.iter().all(|copy| copy.disk.backups.is_empty()),
		"{copies:?}"
	);
}

/// A NAS that is an archive node, which added a file of 2 MB, and a laptop that holds it
/// sealed and keeps nothing from now on, and knows the NAS's account. Returns the devices,
/// the laptop, the file and its content.
fn nas_and_laptop(name: &str) -> (Devices, usize, EntityId, Hash) {
	let mut devices = Devices::new(name, true);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let (file, content) = devices.add(0, "steps.mov", &bytes(6, 2_000_000));
	devices.sync(laptop, 0);
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(laptop, 0);
	assert!(devices.holds(laptop, &content));
	devices.mark(laptop, nibkeep_node::DeviceRole::Everyday, Some(0));
	(devices, laptop, file, content)
}

#[test]
#[cfg_attr(
	target_os = "ios",
	ignore = "iOS lists no mounts to look for a backup on: backups from a phone are on Android alone"
)]
fn a_backup_plugged_in_stands_for_a_copy_when_a_device_removes_its_own() {
	let (mut devices, laptop, _, content) = nas_and_laptop("backup stands");
	// Standard asks for two copies: the NAS's alone is not enough.
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 0, "{removed:?}");
	assert!(devices.holds(laptop, &content));
	// With a backup plugged into the laptop, which reads it itself, it is.
	let folder = folder("backup stands");
	let backup = add(&mut devices, laptop, &folder, &blue());
	update(&mut devices, laptop, &backup, &folder);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 1, "{removed:?}");
	assert!(!devices.holds(laptop, &content));
	assert!(devices.holds(0, &content));
}

#[test]
#[cfg_attr(
	target_os = "ios",
	ignore = "iOS lists no mounts to look for a backup on: backups from a phone are on Android alone"
)]
fn a_backup_kept_away_lets_a_critical_file_leave_an_everyday_device() {
	let (mut devices, laptop, file, content) = nas_and_laptop("backup away");
	let desk = devices.pair("desk", None);
	devices.record_endpoints();
	let space = devices.personal();
	let critical = devices.libraries[0]
		.find_policy(space, "Critical")
		.expect("Critical");
	let at = devices.tick();
	devices.libraries[0]
		.set_policy(space, file, Some(critical), at, &mut devices.rng)
		.expect("sets");
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(desk, 0);
	devices.sync(laptop, 0);
	assert!(devices.holds(desk, &content));
	// Critical asks for a copy away from home, which no device on the home network is.
	let folder = folder("backup away");
	let home = add(&mut devices, laptop, &folder, &blue());
	update(&mut devices, laptop, &home, &folder);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 0, "{removed:?}");
	// Marked as kept away, the backup plugged in stands for it.
	let at = devices.tick();
	devices.libraries[laptop]
		.edit_backup(
			home.id,
			&BackupEdit {
				away: Some(true),
				..BackupEdit::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("marks");
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 1, "{removed:?}");
	assert!(!devices.holds(laptop, &content));
}

#[test]
fn a_backup_waits_before_what_a_purge_destroyed_leaves_it() {
	let mut devices = Devices::new("backup waits", false);
	let folder = folder("backup waits");
	let backup = add(&mut devices, 0, &folder, &blue());
	let (small_file, small) = devices.add(0, "small.txt", &findable("small ", 50_000));
	let (large_file, large) = devices.add(0, "large.bin", &findable("large ", 2_000_000));
	assert_eq!(update(&mut devices, 0, &backup, &folder).held, 2);
	let space = devices.personal();
	for file in [small_file, large_file] {
		let at = devices.tick();
		devices.libraries[0]
			.purge(space, file, &CONTENT, None, at, &mut devices.rng)
			.expect("purges");
	}
	// The next update keeps what the purges destroyed: the device that purged no longer
	// holds it, and the backup counts two purges that wait.
	let kept = update(&mut devices, 0, &backup, &folder);
	assert_eq!(kept.waiting, 2, "{kept:?}");
	assert_eq!(kept.purged, 0, "{kept:?}");
	assert_eq!(kept.held, 2, "{kept:?}");
	assert!(!devices.holds(0, &small) && !devices.holds(0, &large));
	let found = [(backup.clone(), folder.clone())];
	let waiting = devices.libraries[0]
		.waiting_purges_on(&found)
		.expect("lists");
	assert_eq!(waiting.len(), 2, "{waiting:?}");
	assert!(
		waiting
			.iter()
			.all(|purge| purge.ends == purge.received + DAY * 30)
	);
	assert!(
		waiting
			.iter()
			.all(|purge| purge.on.as_ref().is_some_and(|on| on.items > 0))
	);
	// An update a month later lets them go.
	let later = waiting[0].ends;
	let gone = devices.libraries[0]
		.update_backup(&backup, &folder, &|| later, &|| false)
		.expect("updates")
		.expect("not held");
	assert_eq!(gone.waiting, 0, "{gone:?}");
	assert_eq!(gone.held, 0, "{gone:?}");
	nibkeep_testkit::assert_empty!(blob_files(&folder));
	nibkeep_testkit::assert_empty!(
		devices.libraries[0]
			.waiting_purges_on(&found)
			.expect("lists")
	);
}

#[test]
fn a_purge_that_waits_on_a_backup_is_stopped_or_its_wait_ended_where_it_is_plugged_in() {
	let mut devices = Devices::new("backup stops", false);
	let folder = folder("backup stops");
	let backup = add(&mut devices, 0, &folder, &blue());
	let (photo, content) = devices.add(0, "photo.jpg", &findable("a photo ", 2_000_000));
	update(&mut devices, 0, &backup, &folder);
	let space = devices.personal();
	let at = devices.tick();
	let purge = devices.libraries[0]
		.purgeable(space, photo, &CONTENT, None)
		.expect("says");
	// The purge says first that the backup waits, from its next update.
	assert!(
		purge.waiting.iter().any(|copy| copy.backup
			&& copy.name.as_deref() == Some("Blue stick")
			&& copy.days == 30),
		"{:?}",
		purge.waiting
	);
	devices.libraries[0]
		.purge(space, photo, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	update(&mut devices, 0, &backup, &folder);
	assert!(!devices.holds(0, &content));
	let found = [(backup.clone(), folder.clone())];
	let waiting = devices.libraries[0]
		.find_waiting_purges(
			&devices.libraries[0]
				.waiting_purges_on(&found)
				.expect("lists")[0]
				.purge
				.to_string(),
			(&found, Some("blue stick")),
		)
		.expect("finds it");
	// Stopped, the photo comes back from the backup.
	let at = devices.tick();
	devices.libraries[0]
		.stop_purge(&waiting, at, &mut devices.rng)
		.expect("stops it");
	assert!(devices.holds(0, &content), "back from the backup");
	nibkeep_testkit::assert_empty!(
		devices.libraries[0]
			.waiting_purges_on(&found)
			.expect("lists")
	);
	// Purged again, its wait ended on the backup, it is gone from it.
	let at = devices.tick();
	devices.libraries[0]
		.purge(space, photo, &CONTENT, None, at, &mut devices.rng)
		.expect("purges again");
	update(&mut devices, 0, &backup, &folder);
	let waiting = devices.libraries[0]
		.waiting_purges_on(&found)
		.expect("lists");
	assert_eq!(waiting.len(), 1);
	let at = devices.tick();
	let ended = devices.libraries[0].end_wait(&waiting, at).expect("ends").0;
	assert!(ended > 0);
	nibkeep_testkit::assert_empty!(blob_files(&folder));
	nibkeep_testkit::assert_empty!(
		devices.libraries[0]
			.waiting_purges_on(&found)
			.expect("lists")
	);
}

#[test]
fn a_purge_that_waits_on_an_archive_node_and_its_backup_is_ended_in_both_at_once() {
	let mut devices = Devices::new("backup and node wait", true);
	let folder = folder("backup and node wait");
	let backup = add(&mut devices, 0, &folder, &blue());
	let (photo, content) = devices.add(0, "photo.jpg", &findable("a photo ", 2_000_000));
	update(&mut devices, 0, &backup, &folder);
	let space = devices.personal();
	let at = devices.tick();
	devices.libraries[0]
		.purge(space, photo, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	update(&mut devices, 0, &backup, &folder);
	assert!(!devices.holds(0, &content));
	let found = [(backup.clone(), folder.clone())];
	let id = devices.libraries[0].waiting_purges().expect("lists")[0]
		.purge
		.to_string();
	let places = devices.libraries[0]
		.find_waiting_purges(&id, (&found, None))
		.expect("finds it");
	assert_eq!(places.len(), 2, "on the NAS and on the backup: {places:?}");
	let here = devices.libraries[0]
		.find_waiting_purges(&id, (&found, Some("here")))
		.expect("finds it");
	assert_eq!(here.len(), 1);
	assert!(here[0].on.is_none());
	let at = devices.tick();
	let (gone, updated) = devices.libraries[0].end_wait(&places, at).expect("ends");
	assert!(gone > 0);
	assert_eq!(updated.len(), 1);
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));
	nibkeep_testkit::assert_empty!(
		devices.libraries[0]
			.waiting_purges_on(&found)
			.expect("lists")
	);
	nibkeep_testkit::assert_empty!(blob_files(&folder));
}
