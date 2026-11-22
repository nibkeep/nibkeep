//! A laptop and a desk taking turns in a tree with secret files, and the checks of the
//! laptop's library and copy after a crash or a power cut. Shared by the crash test, which
//! runs on every system but iOS, the power-cut test with LazyFS, and the power-cut example,
//! which cuts a real device's power, so it needs nothing of the tests' own build.

use std::fs;
use std::path::{Path, PathBuf};

use nibkeep_base::Hash;
use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	Commit, Joining, Library, NewLibrary, NewProtection, PairingCode, TreeEntryKind, TreePoint,
	Unlocker,
};
use nibkeep_testkit::{TestRng, millis_at};

use super::{pair_joining, sync_in_memory};

/// The laptop's passphrase, and its secrets'; the desk's, which asked for the secrets as it
/// joined.
pub(crate) const LAPTOP_PASSPHRASE: &str = "correct horse";
pub(crate) const SECRETS: &str = "tr0ub4dor";
pub(crate) const DESK_PASSPHRASE: &str = "battery staple";
pub(crate) const DESK_SECRETS: &str = "desk secrets";

/// The laptop's own files of the tree, which it edits and commits: a secret file and an
/// ordinary one. The desk edits, moves, deletes and adds the others.
pub(crate) const OWN: [&str; 2] = [".env", "notes.md"];
/// The desk's secret key, which moves between these two paths.
pub(crate) const KEY_PATHS: [&str; 2] = ["deploy/server.key", "deploy/keys/server.key"];
/// The desk's certificate, which it deletes and adds again.
pub(crate) const PEM: &str = "deploy/api.pem";
/// The desk's other secret files, which it edits at every step, so that an update writes
/// several.
pub(crate) const KEYS: [&str; 4] = [
	"config/keys/a.key",
	"config/keys/b.key",
	"config/keys/c.key",
	"config/keys/d.key",
];
pub(crate) const README: &str = "README.md";

/// What every secret file holds, which must never be in a library's files in plain.
pub(crate) const TREE_SENTINEL: &str = "s3ntinel";

/// Where the laptop and the desk keep what.
pub(crate) struct TreePlaces {
	/// The desk's library and copy, and the notes or marks of whatever drives the turns: on
	/// the real disk in the tests, which cut the power under the laptop alone.
	pub(crate) directory: PathBuf,
	/// The laptop's library.
	pub(crate) laptop: PathBuf,
	/// The laptop's copy of the tree.
	pub(crate) on_laptop: PathBuf,
	/// Where the laptop's edits are written before they are put in the tree, as an editor
	/// that saves with care does, on the same file system.
	pub(crate) laptop_edits: PathBuf,
	/// The desk's library.
	pub(crate) desk: PathBuf,
	/// The desk's copy of the tree.
	pub(crate) on_desk: PathBuf,
	pub(crate) desk_edits: PathBuf,
}

impl TreePlaces {
	/// The places in `directory`, and the laptop's in `laptop`, which may be the same.
	pub(crate) fn new(directory: &Path, laptop: &Path) -> Self {
		Self {
			directory: directory.to_path_buf(),
			laptop: laptop.join("library"),
			on_laptop: laptop.join("tree"),
			laptop_edits: laptop.join("edits"),
			desk: directory.join("desk"),
			on_desk: directory.join("desk-tree"),
			desk_edits: directory.join("desk-edits"),
		}
	}
}

/// The content of the laptop's `.env` at its `number`th commit.
pub(crate) fn env_content(number: usize) -> String {
	format!("TOKEN={TREE_SENTINEL}-{number}\n")
}

/// The tree's entry of the laptop's `.env` at its `number`th commit.
pub(crate) fn env_entry(number: usize) -> TreeEntryKind {
	TreeEntryKind::File {
		content: Some(Hash::of(env_content(number).as_bytes())),
		executable: false,
	}
}

/// The content of a secret file of the desk's at its `step`th step: tens of kilobytes,
/// different at every step and for every path.
pub(crate) fn key_content(path: &str, step: usize) -> String {
	use std::fmt::Write as _;
	let mut content = format!("KEY {step}\n-----BEGIN PRIVATE KEY {TREE_SENTINEL}-----\n");
	let salt: u64 = path.bytes().map(u64::from).sum();
	let step = u64::try_from(step).expect("small");
	for line in 1..=600_u64 {
		let number = line
			.wrapping_mul(step + salt * 7_919 + 7)
			.wrapping_mul(2_654_435_761);
		writeln!(content, "{number:064x}").expect("writes to a string");
	}
	content
}

/// The number of a key's or `.env`'s content, from its first line.
pub(crate) fn number_in(content: &str) -> usize {
	content
		.lines()
		.next()
		.and_then(|line| line.rsplit([' ', '-']).next())
		.and_then(|number| number.parse().ok())
		.unwrap_or_else(|| panic!("no number in {content:?}"))
}

/// Saves a folder's entries to the disk: its files' and folders' names, as made, renamed or
/// deleted. Only on Unix, as the engine does: Rust's standard library opens no folder as a
/// file on Windows. A crash loses nothing the system holds, and the tests cut the power only
/// on Linux.
pub(crate) fn save_folder(folder: &Path) {
	#[cfg(unix)]
	fs::File::open(folder)
		.and_then(|folder| folder.sync_all())
		.expect("saves the folder");
	#[cfg(not(unix))]
	let _ = folder;
}

/// Writes a file of a tree as an editor that saves with care does: beside the tree first,
/// saved to the disk, then put in place, and the folder saved.
pub(crate) fn save_into(root: &Path, edits: &Path, path: &str, content: &str) {
	use std::io::Write as _;
	fs::create_dir_all(edits).expect("makes the edits' folder");
	let temporary = edits.join("saving");
	let mut file = fs::File::create(&temporary).expect("makes the file");
	file.write_all(content.as_bytes()).expect("writes");
	file.sync_all().expect("saves");
	let target = root.join(path);
	let folder = target.parent().expect("a folder");
	fs::create_dir_all(folder).expect("makes folders");
	fs::rename(&temporary, &target).expect("puts in place");
	save_folder(folder);
}

/// Which of the key's paths a copy holds it at, and its number there.
pub(crate) fn key_in(root: &Path) -> Option<(&'static str, usize)> {
	KEY_PATHS.iter().find_map(|path| {
		fs::read_to_string(root.join(path))
			.ok()
			.map(|content| (*path, number_in(&content)))
	})
}

/// The step the desk has made: the number its copy's README holds.
pub(crate) fn desk_step(on_desk: &Path) -> usize {
	let readme = fs::read_to_string(on_desk.join(README)).expect("reads the README");
	number_in(&readme)
}

/// The bits of a file's mode that let others than its owner in: none, on a system without
/// Unix's modes.
fn open_to_others(path: &Path) -> u32 {
	#[cfg(unix)]
	{
		let metadata = fs::metadata(path).expect("reads its mode");
		std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077
	}
	#[cfg(not(unix))]
	{
		let _ = path;
		0
	}
}

/// Every file in a folder, and under it, whose name starts with `start`.
fn named_like(folder: &Path, start: &str) -> Vec<PathBuf> {
	let mut found = Vec::new();
	let mut to_visit = vec![folder.to_path_buf()];
	while let Some(next) = to_visit.pop() {
		for entry in fs::read_dir(&next).expect("lists a folder") {
			let entry = entry.expect("reads an entry");
			if entry.file_type().expect("its type").is_dir() {
				to_visit.push(entry.path());
			} else if entry.file_name().to_string_lossy().starts_with(start) {
				found.push(entry.path());
			}
		}
	}
	found
}

/// The tree's entry of the laptop's `.env`.
pub(crate) fn env_in_tree(laptop: &Library) -> TreeEntryKind {
	let personal = laptop.personal_space();
	let tree = laptop.trees(personal).expect("lists trees")[0].id;
	laptop
		.tree_entries(personal, tree)
		.expect("reads the tree")
		.into_iter()
		.find(|entry| entry.path == OWN[0])
		.expect("the tree holds .env")
		.kind
}

/// Opens the laptop's library with its secrets unlocked.
pub(crate) fn open_laptop(places: &TreePlaces) -> Library {
	let mut laptop = Library::open(&places.laptop, &Unlocker::Passphrase(LAPTOP_PASSPHRASE))
		.expect("opens the laptop's library");
	laptop
		.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		.expect("unlocks the laptop's secrets");
	laptop
}

/// Opens the desk's library with its secrets unlocked.
pub(crate) fn open_desk(places: &TreePlaces) -> Library {
	let mut desk = Library::open(&places.desk, &Unlocker::Passphrase(DESK_PASSPHRASE))
		.expect("opens the desk's library");
	desk.unlock_secrets(&Unlocker::Passphrase(DESK_SECRETS))
		.expect("unlocks the desk's secrets");
	desk
}

/// The desk's next step: a new README, and new versions of its keys; and the server's key
/// moved, at every fourth step, or the certificate deleted or added again, at every fourth
/// step after that, or the server's key edited.
fn desk_edits(places: &TreePlaces, step: usize) {
	let on_desk = &places.on_desk;
	save_into(
		on_desk,
		&places.desk_edits,
		README,
		&format!("readme {step}\n"),
	);
	for path in KEYS {
		save_into(on_desk, &places.desk_edits, path, &key_content(path, step));
	}
	let (at, _) = key_in(on_desk).expect("the desk holds the key");
	match step % 4 {
		0 => {
			let to = on_desk.join(KEY_PATHS[usize::from(at == KEY_PATHS[0])]);
			fs::create_dir_all(to.parent().expect("a folder")).expect("makes folders");
			fs::rename(on_desk.join(at), to).expect("moves the key");
		}
		2 => {
			let pem = on_desk.join(PEM);
			if pem.exists() {
				fs::remove_file(pem).expect("deletes the certificate");
			} else {
				save_into(on_desk, &places.desk_edits, PEM, &key_content(PEM, step));
			}
		}
		_ => save_into(
			on_desk,
			&places.desk_edits,
			at,
			&key_content(KEY_PATHS[0], step),
		),
	}
}

/// Takes turns until the process is killed or the power cut: the desk edits, moves, deletes
/// or adds secret files and commits, and the laptop receives the commit and updates its copy,
/// agreeing to what waits for it; then the laptop commits new versions of its own files, a
/// secret one and an ordinary one. Starting from `noted`, gives `note` the laptop's commits,
/// the key its copy holds and the syncs after each that returns. Never returns.
pub(crate) fn take_turns(
	places: &TreePlaces,
	noted: [usize; 3],
	rng: &mut TestRng,
	note: &mut dyn FnMut([usize; 3]),
) -> ! {
	let mut laptop = open_laptop(places);
	let mut desk = open_desk(places);
	let personal = laptop.personal_space();
	let tree = laptop.trees(personal).expect("lists trees")[0].id;
	let [mut own, mut key, mut syncs] = noted;
	let mut step = desk_step(&places.on_desk);
	// The desk's last edits, if a kill came before its commit made them all: committed first,
	// as a person commits again after a crash, so that each step is a commit of its own.
	// Otherwise the next step could move the key in the same commit as its edit, which takes
	// it for another file, deleted at one path and new at the other (as Known limits says),
	// and a kill in the laptop's update between the two leaves its copy with neither.
	let pending = desk
		.tree_status(personal, tree)
		.expect("reads the desk's status");
	if !pending.changes.is_empty() {
		let wall = millis_at("2026-10-12", 10, 0)
			+ 1_000 * u64::try_from(own + step).expect("small")
			- 750;
		desk.commit_tree(personal, tree, &Commit::new(Some("Desk")), wall, rng)
			.expect("the desk commits what it left");
	}
	loop {
		let wall =
			millis_at("2026-10-12", 10, 0) + 1_000 * u64::try_from(own + step).expect("small");
		step += 1;
		desk_edits(places, step);
		desk.commit_tree(personal, tree, &Commit::new(Some("Desk")), wall, rng)
			.expect("the desk commits");
		let (initiated, responded) = sync_in_memory(&mut laptop, &mut desk, wall, (1, 2));
		initiated.expect("the laptop receives");
		responded.expect("the desk sends");
		syncs += 1;
		// The tests cut every other round soon after this: in the update or the commit after it.
		note([own, key, syncs]);
		let updated = laptop
			.update_tree_agreeing(personal, tree, false)
			.expect("the laptop updates its copy");
		assert!(
			updated.skipped.is_empty() && updated.kept.is_empty(),
			"the update left something: {updated:?}"
		);
		key = key_in(&places.on_laptop)
			.expect("the laptop holds the key")
			.1;
		note([own, key, syncs]);

		let next = own + 1;
		let edits = &places.laptop_edits;
		save_into(&places.on_laptop, edits, OWN[0], &env_content(next));
		save_into(&places.on_laptop, edits, OWN[1], &format!("notes {next}\n"));
		laptop
			.commit_tree(personal, tree, &Commit::new(Some("Own")), wall + 500, rng)
			.expect("the laptop commits");
		own = next;
		note([own, key, syncs]);
	}
}

/// Makes the laptop's library, with its secrets and the tree, and the desk's, which joins it
/// with the secrets and writes its copy of the tree.
pub(crate) fn make_tree_libraries(places: &TreePlaces, rng: &mut TestRng) {
	let started = millis_at("2026-10-12", 9, 0);
	let quick = |passphrase| {
		NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
	};
	let mut laptop = Library::create(
		&places.laptop,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&quick(LAPTOP_PASSPHRASE),
		started,
		rng,
	)
	.expect("creates");
	laptop
		.set_up_secrets(&quick(SECRETS), started + 500, rng)
		.expect("sets up the secrets");
	fs::create_dir_all(&places.on_laptop).expect("makes the tree's folder");
	let (on_laptop, edits) = (&places.on_laptop, &places.laptop_edits);
	save_into(on_laptop, edits, OWN[0], &env_content(0));
	save_into(on_laptop, edits, OWN[1], "notes 0\n");
	save_into(on_laptop, edits, README, "readme 0\n");
	save_into(
		on_laptop,
		edits,
		KEY_PATHS[0],
		&key_content(KEY_PATHS[0], 0),
	);
	for path in KEYS {
		save_into(on_laptop, edits, path, &key_content(path, 0));
	}
	let personal = laptop.personal_space();
	let tree = laptop
		.start_tree(personal, on_laptop, Some("Start"), started + 1_000, rng)
		.expect("starts the tree")
		.tree;
	let code = PairingCode::generate(rng);
	let joining = Joining::new("desk", "nib", rng)
		.expect("names")
		.asking_for_secrets(quick(DESK_SECRETS), rng);
	let (paired, joined) = pair_joining(
		&mut laptop,
		&places.desk,
		(joining, true),
		(&code, &code),
		rng,
	);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	desk.write_tree(
		personal,
		tree,
		&places.on_desk,
		TreePoint::Now,
		started + 70_000,
		rng,
	)
	.expect("the desk writes its copy");
	assert_eq!(key_in(&places.on_desk), Some((KEY_PATHS[0], 0)));
}

/// Checks that each of the desk's secret files in the laptop's copy is a whole version that
/// the desk wrote, no newer than its copy's, readable by its owner alone where NibKeep wrote
/// it, and the key at one of its paths only; returns the key's number.
fn check_desk_files(round: u64, places: &TreePlaces) -> usize {
	let desk_now = desk_step(&places.on_desk);
	let (at, key) = key_in(&places.on_laptop).expect("the laptop holds the key");
	let other = KEY_PATHS[usize::from(at == KEY_PATHS[0])];
	assert!(
		!places.on_laptop.join(other).exists(),
		"round {round}: the key at both {at} and {other}"
	);
	for path in KEYS.into_iter().chain([at, PEM]) {
		let on_disk = places.on_laptop.join(path);
		let Ok(content) = fs::read_to_string(&on_disk) else {
			assert_eq!(path, PEM, "round {round}: {path} gone");
			continue;
		};
		let number = number_in(&content);
		let salted = if KEY_PATHS.contains(&path) {
			KEY_PATHS[0]
		} else {
			path
		};
		assert!(
			number <= desk_now && content == key_content(salted, number),
			"round {round}: {path} holds no whole version of the desk's: {number}, the desk at {desk_now}"
		);
		// What NibKeep wrote: the laptop's first versions are the person's own.
		if number > 0 {
			assert_eq!(
				open_to_others(&on_disk),
				0,
				"round {round}: {path} readable by others"
			);
		}
	}
	key
}

/// Checks the laptop's library and copy after a crash or a power cut, against the commits
/// and the key last noted: the library opens, the tree keeps every commit noted, its copy
/// holds whole versions, a key at least as new as the one noted, and nothing that a commit
/// would take for a change of the desk's files, with the secrets unlocked or locked; then an
/// update brings the copy to the tree, and leaves nothing beside its files. With `verify`,
/// verifies the library too, and looks for secrets in plain in its files. Returns how many of
/// the laptop's commits the tree kept, and the key its copy held.
pub(crate) fn check_tree_after(
	round: u64,
	places: &TreePlaces,
	(own, key): (usize, usize),
	verify: bool,
) -> (usize, usize) {
	let only_own = |status: &nibkeep_node::TreeStatus, when: &str| {
		for change in &status.changes {
			assert!(
				OWN.contains(&change.path.as_str()),
				"round {round}, {when}: a change at {}: {status:?}",
				change.path
			);
		}
	};
	// As the library opens after the cut, with the secrets locked, before anything else
	// takes in what the cut left.
	let mut laptop = Library::open(&places.laptop, &Unlocker::Passphrase(LAPTOP_PASSPHRASE))
		.expect("opens the laptop's library");
	let personal = laptop.personal_space();
	let tree = laptop.trees(personal).expect("lists trees")[0].id;
	let locked = laptop
		.tree_status(personal, tree)
		.expect("reads the status, locked");
	only_own(&locked, "after the cut, with the secrets locked");
	laptop
		.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		.expect("unlocks the secrets");
	let env = fs::read_to_string(places.on_laptop.join(OWN[0])).expect("reads the .env");
	let written = number_in(&env);
	assert!(
		written == own || written == own + 1,
		"round {round}: {own} commits noted, the copy holds .env {written}"
	);
	let in_tree = env_in_tree(&laptop);
	let kept = if in_tree == env_entry(own) {
		own
	} else {
		assert!(
			written == own + 1 && in_tree == env_entry(own + 1),
			"round {round}: {own} commits noted, the copy holds .env {written}, the tree \
			 {in_tree:?}"
		);
		own + 1
	};
	let held = check_desk_files(round, places);
	assert!(
		held >= key,
		"round {round}: key {key} noted, the copy holds {held}"
	);
	let status = laptop
		.tree_status(personal, tree)
		.expect("reads the status");
	only_own(&status, "after the cut");

	let updated = laptop
		.update_tree_agreeing(personal, tree, false)
		.expect("updates the copy");
	// A sync cut short may have saved a commit and not yet its content, which the next sync
	// brings.
	let lacking = |(_, why): &(String, String)| why == "this device holds no copy of its content";
	assert!(
		updated.skipped.iter().all(lacking) && updated.kept.is_empty(),
		"round {round}: the update left something: {updated:?}"
	);
	check_desk_files(round, places);
	let left = named_like(&places.on_laptop, ".nib-");
	assert!(left.is_empty(), "round {round}: left in the copy: {left:?}");
	let status = laptop
		.tree_status(personal, tree)
		.expect("reads the status");
	only_own(&status, "after updating");
	if updated.waiting.is_empty() && updated.skipped.is_empty() {
		nibkeep_testkit::assert_empty!(status.behind, "round {round}");
	}
	if verify {
		let report = laptop
			.verify(millis_at("2026-10-12", 11, 0))
			.expect("verifies");
		assert!(report.is_sound(), "round {round}: {report:?}");
		assert!(
			!nibkeep_testkit::holds_anywhere(&places.laptop, TREE_SENTINEL.as_bytes()),
			"round {round}: a secret in plain in the library"
		);
	}
	(kept, held)
}
