//! Trees on several devices: a tree written on a device that joined the library, and
//! `update`, which brings a copy to the tree as other devices left it, with competing values
//! written out: text merged three ways, anything else side by side ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::common::{library, now, sync_in_memory};
use nibkeep_base::EntityId;
use nibkeep_node::{
	Change, Commit, Library, NodeError, PairingCode, TreePoint, Written, resolved_path,
};
use nibkeep_testkit::TestRng;

/// A folder of its own, as the file system resolves it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	resolved_path(&made).expect("resolves")
}

/// A new folder's place, where nothing is yet.
fn new_folder(name: &str) -> PathBuf {
	let made = folder(name);
	fs::remove_dir(&made).expect("removes");
	made
}

/// Writes a file and makes it look written an hour ago.
fn write(root: &Path, path: &str, content: &[u8]) {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	age(&path, 3_600);
}

/// Makes a file look written some seconds ago.
fn age(path: &Path, seconds: u64) {
	let then = SystemTime::now() - Duration::from_secs(seconds);
	fs::File::options()
		.write(true)
		.open(path)
		.and_then(|file| file.set_modified(then))
		.expect("ages");
}

fn read(root: &Path, path: &str) -> String {
	fs::read_to_string(root.join(path)).expect("reads")
}

/// Two devices with copies of one tree: the laptop, which started it with `files`, and the
/// desk, which joined the library and wrote the tree.
struct Two {
	laptop: Library,
	desk: Library,
	tree: EntityId,
	on_laptop: PathBuf,
	on_desk: PathBuf,
	at: u64,
	rng: TestRng,
}

impl Two {
	fn new(name: &str, files: &[(&str, &[u8])]) -> Self {
		let mut rng = TestRng::new(name);
		let (_, mut laptop) = library(&format!("{name}, laptop"), &mut rng);
		let on_laptop = folder(&format!("{name}, on laptop"));
		for (path, content) in files {
			write(&on_laptop, path, content);
		}
		let personal = laptop.personal_space();
		let tree = laptop
			.start_tree(personal, &on_laptop, Some("Start"), now() + 1_000, &mut rng)
			.expect("starts")
			.tree;
		let desk_directory = new_folder(&format!("{name}, desk"));
		let code = PairingCode::generate(&mut rng);
		let (paired, joined) =
			common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
		paired.expect("the laptop paired");
		let mut desk = joined.expect("the desk joined").library;
		let on_desk = new_folder(&format!("{name}, on desk"));
		desk.write_tree(
			personal,
			tree,
			&on_desk,
			TreePoint::Now,
			now() + 70_000,
			&mut rng,
		)
		.expect("the desk writes its copy");
		Self {
			laptop,
			desk,
			tree,
			on_laptop,
			on_desk,
			at: now() + 80_000,
			rng,
		}
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn commit_on_laptop(&mut self, message: &str) {
		let at = self.tick();
		let personal = self.laptop.personal_space();
		self.laptop
			.commit_tree(
				personal,
				self.tree,
				&Commit::new(Some(message)),
				at,
				&mut self.rng,
			)
			.expect("the laptop commits");
	}

	fn commit_on_desk(&mut self, message: &str) {
		let at = self.tick();
		let personal = self.desk.personal_space();
		self.desk
			.commit_tree(
				personal,
				self.tree,
				&Commit::new(Some(message)),
				at,
				&mut self.rng,
			)
			.expect("the desk commits");
	}

	fn sync(&mut self) {
		let at = self.tick();
		let (first, second) = sync_in_memory(&mut self.laptop, &mut self.desk, at, (1, 2));
		first.expect("the laptop synced");
		second.expect("the desk synced");
	}
}

/// The laptop, with a tree of `files` whose `later` versions it committed next, its copy, and
/// a desk that joined the library and has no copy of the tree yet.
fn joined(
	name: &str,
	files: &[(&str, &[u8])],
	later: &[(&str, &[u8])],
) -> (Library, PathBuf, Library, EntityId, TestRng) {
	let mut rng = TestRng::new(name);
	let (_, mut laptop) = library(&format!("{name}, laptop"), &mut rng);
	let on_laptop = folder(&format!("{name}, on laptop"));
	for (path, content) in files {
		write(&on_laptop, path, content);
	}
	let personal = laptop.personal_space();
	let tree = laptop
		.start_tree(personal, &on_laptop, Some("Start"), now() + 1_000, &mut rng)
		.expect("starts")
		.tree;
	if !later.is_empty() {
		for (path, content) in later {
			write(&on_laptop, path, content);
		}
		commit(&mut laptop, tree, now() + 2_000, &mut rng);
	}
	let desk_directory = new_folder(&format!("{name}, desk"));
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let desk = joined.expect("the desk joined").library;
	(laptop, on_laptop, desk, tree, rng)
}

#[test]
fn a_tree_written_into_a_folder_that_holds_files_keeps_every_one_of_them() {
	let name = "tree written over files";
	let (_laptop, _, mut desk, tree, mut rng) = joined(
		name,
		&[
			(".nibignore", b"local.md\n"),
			("new.txt", b"new\n"),
			("notes.md", b"notes\n"),
			("old.txt", b"old, first\n"),
			("same.txt", b"same\n"),
		],
		&[("old.txt", b"old, the second version\n")],
	);
	let on_desk = folder(&format!("{name}, on desk"));
	write(&on_desk, "same.txt", b"same\n");
	write(&on_desk, "old.txt", b"old, first\n");
	write(&on_desk, "notes.md", b"my own notes\n");
	write(&on_desk, "mine.txt", b"mine\n");
	write(&on_desk, "local.md", b"left out\n");
	let personal = desk.personal_space();
	let written = desk
		.write_tree(
			personal,
			tree,
			&on_desk,
			TreePoint::Now,
			now() + 70_000,
			&mut rng,
		)
		.expect("writes over what is there");
	assert_eq!(written.written, [".nibignore", "new.txt"]);
	assert_eq!(written.found, ["same.txt"]);
	assert_eq!(written.older, ["old.txt"]);
	assert_eq!(written.differing, ["notes.md"]);
	// What .nibignore leaves out is not the tree's, nor one of its new files.
	assert_eq!(written.others, ["mine.txt"]);
	nibkeep_testkit::assert_empty!(written.skipped);
	// Nothing that was there changed.
	assert_eq!(read(&on_desk, "notes.md"), "my own notes\n");
	assert_eq!(read(&on_desk, "old.txt"), "old, first\n");
	assert_eq!(read(&on_desk, "mine.txt"), "mine\n");
	assert_eq!(read(&on_desk, "local.md"), "left out\n");
	// A file that differed is an edit of the tree's version, and one the tree does not have
	// is new; the older version is behind, and an update writes the tree's.
	let status = desk.tree_status(personal, tree).expect("status");
	let changes: Vec<(&str, &Change)> = status
		.changes
		.iter()
		.map(|change| (change.path.as_str(), &change.change))
		.collect();
	assert_eq!(
		changes,
		[("mine.txt", &Change::New), ("notes.md", &Change::Edited)]
	);
	assert_eq!(status.behind, ["old.txt"]);
	nibkeep_testkit::assert_empty!(status.competing);
	let updated = desk.update_tree(personal, tree, false).expect("updates");
	assert_eq!(updated.written, ["old.txt"]);
	assert_eq!(read(&on_desk, "old.txt"), "old, the second version\n");
	assert_eq!(read(&on_desk, "notes.md"), "my own notes\n");
}

#[test]
fn a_file_that_differed_where_the_tree_was_written_is_committed_as_its_next_version() {
	let name = "tree written over a file, committed";
	let (mut laptop, on_laptop, mut desk, tree, mut rng) =
		joined(name, &[("notes.md", b"notes\n")], &[]);
	let on_desk = folder(&format!("{name}, on desk"));
	write(&on_desk, "notes.md", b"my own notes\n");
	let personal = desk.personal_space();
	desk.write_tree(
		personal,
		tree,
		&on_desk,
		TreePoint::Now,
		now() + 70_000,
		&mut rng,
	)
	.expect("writes over what is there");
	commit(&mut desk, tree, now() + 80_000, &mut rng);
	sync_two(&mut laptop, &mut desk, now() + 90_000);
	let updated = laptop
		.update_tree(personal, tree, false)
		.expect("the laptop updates");
	assert_eq!(updated.written, ["notes.md"]);
	assert_eq!(read(&on_laptop, "notes.md"), "my own notes\n");
	// One file, with a new version: nothing competes, on either device.
	for library in [&laptop, &desk] {
		let status = library.tree_status(personal, tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn a_restore_of_a_file_that_differed_where_the_tree_was_written_keeps_it_beside() {
	let name = "tree written over a file, restored";
	let (_laptop, _, mut desk, tree, mut rng) = joined(name, &[("notes.md", b"notes\n")], &[]);
	let on_desk = folder(&format!("{name}, on desk"));
	write(&on_desk, "notes.md", b"my own notes\n");
	let personal = desk.personal_space();
	desk.write_tree(
		personal,
		tree,
		&on_desk,
		TreePoint::Now,
		now() + 70_000,
		&mut rng,
	)
	.expect("writes over what is there");
	let restored = desk
		.restore_in_tree(personal, tree, "notes.md", TreePoint::Now)
		.expect("restores");
	assert_eq!(
		restored.aside,
		[("notes.md".to_owned(), "notes.md.orig".to_owned())]
	);
	assert_eq!(read(&on_desk, "notes.md"), "notes\n");
	assert_eq!(read(&on_desk, "notes.md.orig"), "my own notes\n");
}

/// The desk's copy of a tree of `files`, written into a folder that `prepare` filled first,
/// with what writing it did.
fn written_over(
	name: &str,
	files: &[(&str, &[u8])],
	prepare: impl FnOnce(&Path),
) -> (
	Library,
	PathBuf,
	Library,
	PathBuf,
	EntityId,
	Result<Written, NodeError>,
) {
	let (laptop, on_laptop, mut desk, tree, mut rng) = joined(name, files, &[]);
	let on_desk = folder(&format!("{name}, on desk"));
	prepare(&on_desk);
	let personal = desk.personal_space();
	let written = desk.write_tree(
		personal,
		tree,
		&on_desk,
		TreePoint::Now,
		now() + 70_000,
		&mut rng,
	);
	(laptop, on_laptop, desk, on_desk, tree, written)
}

#[test]
fn a_file_that_would_be_secret_where_the_tree_s_is_not_stays_out_of_commits() {
	let name = "tree written over a file secret here";
	let (mut laptop, on_laptop, mut desk, on_desk, tree, written) =
		written_over(name, &[("notes.md", b"notes\n")], |on_desk| {
			write(on_desk, ".nibsecret", b"notes.md\n");
			write(on_desk, "notes.md", b"my secret notes\n");
		});
	let written = written.expect("writes over what is there");
	nibkeep_testkit::assert_empty!(written.differing);
	assert_eq!(written.skipped.len(), 1, "{written:?}");
	assert_eq!(written.skipped[0].0, "notes.md");
	assert_eq!(read(&on_desk, "notes.md"), "my secret notes\n");
	let personal = desk.personal_space();
	let mut rng = TestRng::new(name);
	commit(&mut desk, tree, now() + 80_000, &mut rng);
	sync_two(&mut laptop, &mut desk, now() + 90_000);
	laptop
		.update_tree(personal, tree, false)
		.expect("the laptop updates");
	// The notes stayed here: the laptop has the tree's, and nothing competes with them.
	assert_eq!(read(&on_laptop, "notes.md"), "notes\n");
	let status = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_read_where_the_tree_was_written_stays_out_of_commits() {
	use std::os::unix::fs::PermissionsExt as _;
	let name = "tree written over an unreadable file";
	let (_laptop, _, mut desk, on_desk, tree, written) =
		written_over(name, &[("notes.md", b"notes\n")], |on_desk| {
			write(on_desk, "notes.md", b"mine\n");
			let locked = fs::Permissions::from_mode(0o000);
			fs::set_permissions(on_desk.join("notes.md"), locked).expect("locks");
		});
	let unlocked = fs::Permissions::from_mode(0o644);
	fs::set_permissions(on_desk.join("notes.md"), unlocked).expect("unlocks");
	let written = written.expect("writes over what is there");
	if written.differing == ["notes.md"] {
		// Read all the same, as by a user whom no permission stops.
		return;
	}
	assert_eq!(written.skipped.len(), 1, "{written:?}");
	assert_eq!(written.skipped[0].0, "notes.md");
	// Readable now, it is still no new file of its own, which a commit would add.
	let personal = desk.personal_space();
	let status = desk.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.competing);
	let mut rng = TestRng::new(name);
	commit(&mut desk, tree, now() + 80_000, &mut rng);
	assert_eq!(read(&on_desk, "notes.md"), "mine\n");
}

#[cfg(unix)]
#[test]
fn an_executable_bit_or_a_link_where_the_tree_was_written_is_an_edit() {
	use std::os::unix::fs::PermissionsExt as _;
	let name = "tree written over a bit and a link";
	let (_laptop, _, desk, on_desk, tree, written) = written_over(
		name,
		&[("run.sh", b"echo run\n"), ("notes.md", b"notes\n")],
		|on_desk| {
			write(on_desk, "run.sh", b"echo run\n");
			let executable = fs::Permissions::from_mode(0o755);
			fs::set_permissions(on_desk.join("run.sh"), executable).expect("sets the bit");
			write(on_desk, "elsewhere.md", b"elsewhere\n");
			std::os::unix::fs::symlink("elsewhere.md", on_desk.join("notes.md")).expect("links");
		},
	);
	let written = written.expect("writes over what is there");
	assert_eq!(written.differing, ["notes.md", "run.sh"]);
	nibkeep_testkit::assert_empty!(written.older);
	let personal = desk.personal_space();
	let status = desk.tree_status(personal, tree).expect("status");
	let changes: Vec<(&str, &Change)> = status
		.changes
		.iter()
		.map(|change| (change.path.as_str(), &change.change))
		.collect();
	assert_eq!(
		changes,
		[
			("elsewhere.md", &Change::New),
			("notes.md", &Change::Edited),
			("run.sh", &Change::Executable(true)),
		]
	);
	// An update leaves both as they are.
	desk.update_tree(personal, tree, false).expect("updates");
	assert!(on_desk.join("notes.md").is_symlink());
	assert_eq!(read(&on_desk, "elsewhere.md"), "elsewhere\n");
}

#[test]
fn a_tree_is_not_written_into_the_folder_of_another_library() {
	let name = "tree written over another library";
	let (_laptop, _, _desk, on_desk, _tree, written) =
		written_over(name, &[("notes.md", b"notes\n")], |on_desk| {
			fs::create_dir_all(on_desk.join(".nib").join("replicas")).expect("makes one");
		});
	assert!(
		matches!(written, Err(NodeError::OtherLibraryHere(_))),
		"{written:?}"
	);
	assert!(!on_desk.join("notes.md").exists());
}

#[test]
fn a_past_value_with_no_content_behind_it_makes_no_copy_an_older_version() {
	use nibkeep_node::Edit;
	let name = "tree written over a value with no content";
	let (mut laptop, _, mut desk, tree, mut rng) =
		joined(name, &[("config.yml", b"the tree's\n")], &[]);
	let personal = desk.personal_space();
	// Another device states a content it holds nothing of, the desk's file's, and takes it
	// back, as any replica of the space may write, an AI agent's too.
	let file = laptop.tree_entries(personal, tree).expect("lists")[0].entity;
	for (content, at) in [(&b"the desk's\n"[..], 60_000), (b"the tree's\n", 61_000)] {
		let value = nibkeep_model::Value::Blob(nibkeep_base::Hash::of(content));
		laptop
			.edit_entity(
				personal,
				file,
				vec![Edit::Set(nibkeep_std::CONTENT, vec![value])],
				&common::today(),
				now() + at,
				&mut rng,
			)
			.expect("edits");
	}
	sync_two(&mut laptop, &mut desk, now() + 62_000);
	let on_desk = folder(&format!("{name}, on desk"));
	write(&on_desk, "config.yml", b"the desk's\n");
	let written = desk
		.write_tree(
			personal,
			tree,
			&on_desk,
			TreePoint::Now,
			now() + 70_000,
			&mut rng,
		)
		.expect("writes over what is there");
	nibkeep_testkit::assert_empty!(written.older);
	assert_eq!(written.differing, ["config.yml"]);
	// So no update writes over it, which nothing could bring back.
	desk.update_tree(personal, tree, false).expect("updates");
	assert_eq!(read(&on_desk, "config.yml"), "the desk's\n");
}

#[test]
fn a_tree_is_not_written_where_a_checkout_has_its_copy_at_one_of_its_paths() {
	let name = "tree written over a checkout";
	let (_laptop, _, mut desk, tree, mut rng) = joined(name, &[("list.txt", b"Milk.\n")], &[]);
	let personal = desk.personal_space();
	let files = folder(&format!("{name}, files"));
	write(&files, "list.txt", b"Milk.\n");
	let file = desk
		.add_file(personal, &files.join("list.txt"), now() + 60_000, &mut rng)
		.expect("adds")
		.file;
	let on_desk = folder(&format!("{name}, on desk"));
	desk.checkout(personal, file, &on_desk, now() + 61_000, &mut rng)
		.expect("checks out");
	let refused = desk.write_tree(
		personal,
		tree,
		&on_desk,
		TreePoint::Now,
		now() + 70_000,
		&mut rng,
	);
	assert!(
		matches!(refused, Err(NodeError::HoldsOtherCopies(_))),
		"{refused:?}"
	);
	assert!(desk.tree_at(personal, &on_desk).expect("looks").is_none());
	// A checkout elsewhere in the folder is left out of commits, and no hindrance.
	let other = folder(&format!("{name}, other"));
	fs::create_dir(other.join("kept")).expect("makes a folder");
	desk.checkout(
		personal,
		file,
		&other.join("kept"),
		now() + 71_000,
		&mut rng,
	)
	.expect("checks out");
	let written = desk
		.write_tree(
			personal,
			tree,
			&other,
			TreePoint::Now,
			now() + 72_000,
			&mut rng,
		)
		.expect("writes beside the checkout");
	assert_eq!(written.written, ["list.txt"]);
	nibkeep_testkit::assert_empty!(written.others);
}

#[test]
fn update_brings_what_another_device_committed() {
	let mut two = Two::new(
		"tree update",
		&[
			("notes.md", b"one\n"),
			("old.txt", b"old\n"),
			("move/me.txt", b"me\n"),
		],
	);
	write(&two.on_laptop, "notes.md", b"one, edited\n");
	write(&two.on_laptop, "new.txt", b"new\n");
	fs::remove_file(two.on_laptop.join("old.txt")).expect("deletes");
	fs::create_dir(two.on_laptop.join("moved")).expect("makes a folder");
	fs::rename(
		two.on_laptop.join("move/me.txt"),
		two.on_laptop.join("moved/me.txt"),
	)
	.expect("moves");
	two.commit_on_laptop("Changes");
	two.sync();
	let personal = two.desk.personal_space();
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	assert!(!status.behind.is_empty(), "{status:?}");
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.written, ["new.txt", "notes.md"]);
	assert_eq!(updated.deleted, ["old.txt"]);
	assert_eq!(
		updated.moved,
		[("move/me.txt".to_owned(), "moved/me.txt".to_owned())]
	);
	assert_eq!(read(&two.on_desk, "notes.md"), "one, edited\n");
	assert!(!two.on_desk.join("old.txt").exists());
	assert_eq!(read(&two.on_desk, "moved/me.txt"), "me\n");
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
}

#[test]
fn an_edit_here_is_merged_with_a_newer_version_from_elsewhere() {
	let mut two = Two::new(
		"tree update merge",
		&[("notes.md", b"one\ntwo\nthree\nfour\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\nfour\n");
	two.commit_on_laptop("Laptop");
	two.sync();
	write(&two.on_desk, "notes.md", b"one\ntwo\nthree\nFOUR\n");
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.merged, ["notes.md"]);
	assert_eq!(read(&two.on_desk, "notes.md"), "ONE\ntwo\nthree\nFOUR\n");
	// What it merged is an edit of the newer version, to commit, not a conflict.
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.laptop.personal_space();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "notes.md"), "ONE\ntwo\nthree\nFOUR\n");
}

#[test]
fn an_edited_copy_never_loses_its_edits_whatever_its_size_and_time() {
	let mut two = Two::new(
		"tree update keeps edits",
		&[("notes.md", b"aaaa\nkeep\nbbbb\n")],
	);
	// The desk's edit keeps the copy's size and time, so that they tell nothing.
	let on_desk = two.on_desk.join("notes.md");
	let before = fs::metadata(&on_desk)
		.expect("reads")
		.modified()
		.expect("a time");
	fs::write(&on_desk, b"aaaa\nkeep\nBBBB\n").expect("edits");
	fs::File::options()
		.write(true)
		.open(&on_desk)
		.and_then(|file| file.set_modified(before))
		.expect("keeps its time");
	write(&two.on_laptop, "notes.md", b"AAAA\nkeep\nbbbb\n");
	two.commit_on_laptop("Laptop");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "notes.md"), "AAAA\nkeep\nBBBB\n");
}

#[test]
fn clashing_edits_are_marked_in_text_and_set_side_by_side_otherwise() {
	let mut two = Two::new(
		"tree update clash",
		&[("notes.md", b"one\n"), ("photo.bin", &[0, 1, 2])],
	);
	write(&two.on_laptop, "notes.md", b"laptop\n");
	write(&two.on_laptop, "photo.bin", &[0, 1, 2, 3]);
	two.commit_on_laptop("Laptop");
	two.sync();
	write(&two.on_desk, "notes.md", b"desk\n");
	write(&two.on_desk, "photo.bin", &[0, 9]);
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.conflicts, ["notes.md", "photo.bin"]);
	let merged = read(&two.on_desk, "notes.md");
	assert!(
		merged.contains("<<<<<<<") && merged.contains("laptop") && merged.contains("desk"),
		"{merged}"
	);
	assert_eq!(
		fs::read(two.on_desk.join("photo.bin")).expect("reads"),
		[0, 9]
	);
	assert_eq!(
		fs::read(two.on_desk.join("photo.bin.conflict-laptop")).expect("the other beside"),
		[0, 1, 2, 3]
	);
	// The other version beside is no file of the tree.
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	let changed: Vec<&str> = status
		.changes
		.iter()
		.map(|change| change.path.as_str())
		.collect();
	assert_eq!(changed, ["notes.md", "photo.bin"]);
}

#[test]
fn commits_made_apart_on_two_devices_compete_until_a_commit_settles_them() {
	let mut two = Two::new(
		"tree update competing",
		&[("notes.md", b"one\ntwo\nthree\nfour\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\nfour\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one\ntwo\nthree\nFOUR\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	assert_eq!(status.competing, ["notes.md"]);
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "notes.md"), "ONE\ntwo\nthree\nFOUR\n");
	two.commit_on_desk("Settled");
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
	nibkeep_testkit::assert_empty!(status.changes);
}

#[test]
fn competing_versions_are_listed_as_competing_alone_on_both_devices() {
	// Which version shows first follows the statements' random IDs. Each device holds one
	// of them, so on one device or the other, its own is not first: neither counts its copy
	// as behind, as both list it as competing.
	let mut two = Two::new(
		"tree status competing alone",
		&[("notes.md", b"one\n"), ("photo.bin", &[0])],
	);
	write(&two.on_laptop, "notes.md", b"laptop\n");
	write(&two.on_laptop, "photo.bin", &[1]);
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"desk\n");
	write(&two.on_desk, "photo.bin", &[2]);
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.laptop.personal_space();
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		assert_eq!(status.competing, ["notes.md", "photo.bin"]);
		nibkeep_testkit::assert_empty!(status.behind);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn two_devices_that_add_one_path_give_it_two_files_until_one_goes() {
	let mut two = Two::new("tree update two adds", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "plan.md", b"laptop's plan\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "plan.md", b"desk's plan\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	assert_eq!(status.competing, ["plan.md"]);
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.conflicts, ["plan.md"]);
	let mine = read(&two.on_desk, "plan.md");
	let beside = read(&two.on_desk, "plan.md.conflict-laptop");
	assert_eq!(mine, "desk's plan\n");
	assert_eq!(beside, "laptop's plan\n");
	// Keeping the other under a name of its own settles it: it moves there.
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("laptop-plan.md"),
	)
	.expect("renames");
	two.commit_on_desk("Both kept");
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
	let paths: Vec<String> = two
		.desk
		.tree_entries(personal, two.tree)
		.expect("reads")
		.into_iter()
		.map(|entry| entry.path)
		.collect();
	assert_eq!(paths, ["a.txt", "laptop-plan.md", "plan.md"]);
}

#[test]
fn a_file_deleted_elsewhere_while_edited_here_keeps_its_copy() {
	let mut two = Two::new(
		"tree update deleted edited",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n")],
	);
	fs::remove_file(two.on_laptop.join("a.txt")).expect("deletes");
	two.commit_on_laptop("Laptop deletes");
	two.sync();
	write(&two.on_desk, "a.txt", b"a, edited\n");
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	let kept: Vec<&str> = updated.kept.iter().map(|(path, _)| path.as_str()).collect();
	assert_eq!(kept, ["a.txt"]);
	assert_eq!(read(&two.on_desk, "a.txt"), "a, edited\n");
}

/// Two devices that committed apart: the laptop a new version of `a.txt`, the desk its
/// deletion; synced, before either saw the other.
fn deleted_apart_from_a_new_version(name: &str) -> Two {
	let mut two = Two::new(name, &[("a.txt", b"a\n"), ("b.txt", b"b\n")]);
	write(&two.on_laptop, "a.txt", b"a, edited on the laptop\n");
	two.commit_on_laptop("Laptop edits");
	fs::remove_file(two.on_desk.join("a.txt")).expect("deletes");
	two.commit_on_desk("Desk deletes");
	two.sync();
	two
}

#[test]
fn a_file_deleted_on_one_device_while_another_gave_it_a_new_version_stays_on_both() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, stays");
	let personal = two.laptop.personal_space();
	// Both list it, and neither as behind.
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		assert_eq!(status.competing, ["a.txt"]);
		nibkeep_testkit::assert_empty!(status.behind);
	}
	// The laptop keeps its copy; the desk gets the new version back. Both list it.
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.contested, ["a.txt"]);
	nibkeep_testkit::assert_empty!(updated.deleted);
	nibkeep_testkit::assert_empty!(updated.written);
	assert_eq!(read(&two.on_laptop, "a.txt"), "a, edited on the laptop\n");
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.written, ["a.txt"]);
	assert_eq!(updated.contested, ["a.txt"]);
	assert_eq!(read(&two.on_desk, "a.txt"), "a, edited on the laptop\n");
	// A commit settles it: the file stays, on both, with nothing left to settle.
	two.commit_on_desk("Desk keeps it");
	two.sync();
	for (library, root) in [(&two.laptop, &two.on_laptop), (&two.desk, &two.on_desk)] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
		let updated = library
			.update_tree(personal, two.tree, false)
			.expect("updates");
		nibkeep_testkit::assert_empty!(updated.deleted);
		assert_eq!(read(root, "a.txt"), "a, edited on the laptop\n");
	}
}

#[cfg(unix)]
#[test]
fn an_executable_bit_taken_away_while_another_device_deleted_the_file_competes() {
	use std::os::unix::fs::PermissionsExt as _;
	let mut two = Two::new("tree deleted apart, bit taken away", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "run.sh", b"echo run\n");
	fs::set_permissions(
		two.on_laptop.join("run.sh"),
		fs::Permissions::from_mode(0o755),
	)
	.expect("makes it executable");
	two.commit_on_laptop("Laptop adds a script");
	two.sync();
	let personal = two.laptop.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	// Apart: the laptop takes the bit away, the desk deletes the file.
	fs::set_permissions(
		two.on_laptop.join("run.sh"),
		fs::Permissions::from_mode(0o644),
	)
	.expect("takes the bit away");
	two.commit_on_laptop("Laptop takes the bit away");
	fs::remove_file(two.on_desk.join("run.sh")).expect("deletes");
	two.commit_on_desk("Desk deletes");
	two.sync();
	// The deletion did not see the bit taken away, so the two compete, as for a new version.
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		assert_eq!(status.competing, ["run.sh"]);
		nibkeep_testkit::assert_empty!(status.behind);
	}
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.contested, ["run.sh"]);
	nibkeep_testkit::assert_empty!(updated.deleted);
	assert!(two.on_laptop.join("run.sh").exists());
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.written, ["run.sh"]);
	assert_eq!(updated.contested, ["run.sh"]);
	let mode = fs::metadata(two.on_desk.join("run.sh"))
		.expect("written back")
		.permissions()
		.mode();
	assert_eq!(mode & 0o111, 0, "{mode:o}");
	// A commit settles it: the file stays, on both, not executable, with nothing to settle.
	two.commit_on_desk("Desk keeps it");
	two.sync();
	for (library, root) in [(&two.laptop, &two.on_laptop), (&two.desk, &two.on_desk)] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
		let updated = library
			.update_tree(personal, two.tree, false)
			.expect("updates");
		nibkeep_testkit::assert_empty!(updated.deleted);
		let mode = fs::metadata(root.join("run.sh"))
			.expect("kept")
			.permissions()
			.mode();
		assert_eq!(mode & 0o111, 0, "{mode:o}");
	}
}

#[test]
fn a_file_moved_on_one_device_while_another_deleted_it_competes() {
	let mut two = Two::new(
		"tree deleted apart, moved",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n")],
	);
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("moved.txt")).expect("moves");
	two.commit_on_laptop("Laptop moves");
	fs::remove_file(two.on_desk.join("a.txt")).expect("deletes");
	two.commit_on_desk("Desk deletes");
	two.sync();
	let personal = two.laptop.personal_space();
	// The deletion did not see the move, so the two compete, as for a new version.
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		assert_eq!(status.competing, ["moved.txt"]);
	}
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.contested, ["moved.txt"]);
	nibkeep_testkit::assert_empty!(updated.deleted);
	assert_eq!(read(&two.on_laptop, "moved.txt"), "a\n");
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.written, ["moved.txt"]);
	assert_eq!(read(&two.on_desk, "moved.txt"), "a\n");
	two.commit_on_desk("Desk keeps it");
	two.sync();
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn a_file_deleted_again_once_its_new_version_was_seen_goes_everywhere() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, deleted again");
	let personal = two.laptop.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	// The desk has seen the laptop's version, and deletes the file again.
	fs::remove_file(two.on_desk.join("a.txt")).expect("deletes");
	two.commit_on_desk("Desk deletes again");
	two.sync();
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.deleted, ["a.txt"]);
	assert!(!two.on_laptop.join("a.txt").exists());
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn a_commit_before_an_update_leaves_a_deletion_and_a_new_version_competing() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, unseen");
	let personal = two.laptop.personal_space();
	// Unseen, the deletion is not committed over, nor the new version deleted.
	two.commit_on_laptop("Laptop, before an update");
	two.commit_on_desk("Desk, before an update");
	two.sync();
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		assert_eq!(status.competing, ["a.txt"]);
	}
	// An edit on the laptop waits for an update, as an edit of competing versions does.
	write(&two.on_laptop, "a.txt", b"a, edited again\n");
	let at = two.tick();
	assert!(matches!(
		two.laptop
			.commit_tree(personal, two.tree, &Commit::new(None), at, &mut two.rng),
		Err(nibkeep_node::NodeError::TreeStale(_))
	));
}

#[test]
fn a_contested_copy_deleted_here_before_an_update_is_deleted_by_the_next_commit() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, then here too");
	let personal = two.laptop.personal_space();
	// The laptop agrees with the deletion before it updates.
	fs::remove_file(two.on_laptop.join("a.txt")).expect("deletes");
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_laptop.join("a.txt").exists());
	two.commit_on_laptop("Laptop deletes it too");
	two.sync();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("a.txt").exists());
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn a_contested_copy_moved_here_before_an_update_is_kept_where_it_went() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, moved here");
	let personal = two.laptop.personal_space();
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("c.txt")).expect("moves");
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.commit_on_laptop("Laptop moves it");
	two.sync();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "c.txt"), "a, edited on the laptop\n");
	assert!(!two.on_desk.join("a.txt").exists());
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

#[test]
fn two_devices_that_settle_one_contest_apart_keep_one_version() {
	let mut two = deleted_apart_from_a_new_version("tree deleted apart, settled twice");
	let personal = two.laptop.personal_space();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.commit_on_laptop("Laptop keeps it");
	two.commit_on_desk("Desk keeps it");
	two.sync();
	for library in [&two.laptop, &two.desk] {
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
		nibkeep_testkit::assert_empty!(status.changes);
	}
}

/// A third device of the library, the phone, paired with the laptop, with a copy of the tree
/// written as the laptop has it now.
fn phone_of(two: &mut Two, name: &str) -> (Library, PathBuf) {
	let directory = new_folder(&format!("{name}, phone"));
	let code = PairingCode::generate(&mut two.rng);
	let (paired, joined) = common::pair_named(
		&mut two.laptop,
		&directory,
		"phone",
		(&code, &code),
		&mut two.rng,
	);
	paired.expect("the laptop paired");
	let mut phone = joined.expect("the phone joined").library;
	let on_phone = new_folder(&format!("{name}, on phone"));
	let at = two.tick();
	let personal = phone.personal_space();
	phone
		.write_tree(
			personal,
			two.tree,
			&on_phone,
			TreePoint::Now,
			at,
			&mut two.rng,
		)
		.expect("the phone writes its copy");
	(phone, on_phone)
}

/// Syncs two libraries.
fn sync_two(first: &mut Library, second: &mut Library, at: u64) {
	let (one, other) = sync_in_memory(first, second, at, (1, 2));
	one.expect("the first synced");
	other.expect("the second synced");
}

/// Commits a library's copy of the tree.
fn commit(library: &mut Library, tree: EntityId, at: u64, rng: &mut TestRng) {
	let personal = library.personal_space();
	library
		.commit_tree(personal, tree, &Commit::new(None), at, rng)
		.expect("commits");
}

#[test]
fn a_newer_version_that_arrives_after_an_update_saw_the_deletion_waits_for_another() {
	let name = "tree deleted apart, newer version";
	let mut two = Two::new(name, &[("a.txt", b"a\n"), ("b.txt", b"b\n")]);
	let (mut phone, on_phone) = phone_of(&mut two, name);
	let personal = two.laptop.personal_space();
	// The laptop gives a.txt a version, which the phone takes, and gives it a newer one;
	// meanwhile the desk deletes it, having seen neither.
	write(&two.on_laptop, "a.txt", b"a, laptop\n");
	two.commit_on_laptop("Laptop");
	let at = two.tick();
	sync_two(&mut two.laptop, &mut phone, at);
	phone
		.update_tree(personal, two.tree, false)
		.expect("updates");
	write(&on_phone, "a.txt", b"a, laptop, then phone\n");
	let at = two.tick();
	commit(&mut phone, two.tree, at, &mut two.rng);
	fs::remove_file(two.on_desk.join("a.txt")).expect("deletes");
	two.commit_on_desk("Desk deletes");
	// The laptop sees the deletion against its version, and updates.
	two.sync();
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.contested, ["a.txt"]);
	// The phone's newer version arrives: the laptop's copy is behind, no change to commit,
	// until an update writes the newer version, which a commit keeps.
	let at = two.tick();
	sync_two(&mut two.laptop, &mut phone, at);
	let status = two.laptop.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	assert_eq!(status.competing, ["a.txt"]);
	assert_eq!(status.behind, ["a.txt"]);
	two.commit_on_laptop("Nothing to settle yet");
	assert!(
		two.laptop
			.tree_status(personal, two.tree)
			.expect("status")
			.competing
			.contains(&"a.txt".to_owned())
	);
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "a.txt"), "a, laptop, then phone\n");
	two.commit_on_laptop("Keeps it");
	let at = two.tick();
	sync_two(&mut two.laptop, &mut phone, at);
	two.sync();
	for (library, root) in [
		(&two.laptop, &two.on_laptop),
		(&two.desk, &two.on_desk),
		(&phone, &on_phone),
	] {
		library
			.update_tree(personal, two.tree, false)
			.expect("updates");
		assert_eq!(read(root, "a.txt"), "a, laptop, then phone\n");
		let status = library.tree_status(personal, two.tree).expect("status");
		nibkeep_testkit::assert_empty!(status.competing);
	}
}

#[test]
fn versions_made_apart_come_back_merged_where_the_file_was_deleted() {
	let name = "tree deleted apart, two versions";
	let mut two = Two::new(name, &[("a.txt", b"one\ntwo\nthree\n"), ("b.txt", b"b\n")]);
	let (mut phone, on_phone) = phone_of(&mut two, name);
	let personal = two.laptop.personal_space();
	// The laptop and the phone change other lines apart; the desk deletes the file.
	write(&two.on_laptop, "a.txt", b"ONE\ntwo\nthree\n");
	two.commit_on_laptop("Laptop");
	write(&on_phone, "a.txt", b"one\ntwo\nTHREE\n");
	let at = two.tick();
	commit(&mut phone, two.tree, at, &mut two.rng);
	fs::remove_file(two.on_desk.join("a.txt")).expect("deletes");
	two.commit_on_desk("Desk deletes");
	let at = two.tick();
	sync_two(&mut two.laptop, &mut phone, at);
	two.sync();
	// Where it was deleted, both versions come back, merged; neither edit is lost.
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.contested, ["a.txt"]);
	assert_eq!(read(&two.on_desk, "a.txt"), "ONE\ntwo\nTHREE\n");
	two.commit_on_desk("Desk keeps both");
	two.sync();
	let at = two.tick();
	sync_two(&mut two.laptop, &mut phone, at);
	for (library, root) in [(&two.laptop, &two.on_laptop), (&phone, &on_phone)] {
		library
			.update_tree(personal, two.tree, false)
			.expect("updates");
		assert_eq!(read(root, "a.txt"), "ONE\ntwo\nTHREE\n");
	}
}

#[test]
fn a_commit_in_several_parts_is_written_elsewhere_only_whole() {
	let mut two = Two::new(
		"tree update parts",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n"), ("c.txt", b"c\n")],
	);
	for path in ["a.txt", "b.txt", "c.txt"] {
		write(&two.on_laptop, path, format!("{path}, again\n").as_bytes());
	}
	let at = two.tick();
	let personal = two.laptop.personal_space();
	// As a crash would, the commit stops after its revision and its first file.
	two.laptop
		.commit_tree(
			personal,
			two.tree,
			&Commit::new(Some("Parts")).cut_after(2),
			at,
			&mut two.rng,
		)
		.expect("commits a part");
	two.sync();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.written);
	assert_eq!(updated.waiting.len(), 1);
	assert_eq!(read(&two.on_desk, "a.txt"), "a\n");
	let updated = two
		.desk
		.update_tree(personal, two.tree, true)
		.expect("updates in part");
	assert_eq!(updated.written, ["a.txt"]);
}

#[test]
fn a_file_moved_and_edited_elsewhere_is_moved_and_written_here() {
	let mut two = Two::new(
		"tree update move edit",
		&[("a.txt", b"one\n"), ("b.txt", b"b\n")],
	);
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("c.txt")).expect("moves");
	write(&two.on_laptop, "c.txt", b"one, edited\n");
	two.commit_on_laptop("Move and edit");
	two.sync();
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.conflicts);
	assert_eq!(read(&two.on_desk, "c.txt"), "one, edited\n");
	assert!(!two.on_desk.join("c.txt.conflict-laptop").exists());
}

#[test]
fn a_swap_elsewhere_is_a_swap_here() {
	let mut two = Two::new("tree update swap", &[("a.txt", b"A\n"), ("b.txt", b"B\n")]);
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("c.txt")).expect("moves");
	fs::rename(two.on_laptop.join("b.txt"), two.on_laptop.join("a.txt")).expect("moves");
	fs::rename(two.on_laptop.join("c.txt"), two.on_laptop.join("b.txt")).expect("moves");
	two.commit_on_laptop("Swap");
	two.sync();
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.conflicts);
	assert_eq!(read(&two.on_desk, "a.txt"), "B\n");
	assert_eq!(read(&two.on_desk, "b.txt"), "A\n");
}

#[test]
fn update_leaves_deletions_and_moves_made_here_and_not_committed() {
	let mut two = Two::new(
		"tree update keeps local",
		&[("a.txt", b"a\n"), ("old.txt", b"old\n"), ("c.txt", b"c\n")],
	);
	write(&two.on_laptop, "c.txt", b"c, edited\n");
	two.commit_on_laptop("Laptop");
	two.sync();
	fs::remove_file(two.on_desk.join("old.txt")).expect("deletes here");
	fs::rename(two.on_desk.join("a.txt"), two.on_desk.join("renamed.txt")).expect("moves here");
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("old.txt").exists());
	assert!(!two.on_desk.join("a.txt").exists());
	assert_eq!(read(&two.on_desk, "c.txt"), "c, edited\n");
}

#[test]
fn competing_versions_are_committed_over_only_once_written_out() {
	let mut two = Two::new(
		"tree commit competing",
		&[("notes.md", b"one\ntwo\nthree\nfour\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\nfour\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one\ntwo\nthree\nFOUR\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	// Untouched, the copy is no change: nothing is recorded over the laptop's version.
	let at = two.tick();
	let committed = two
		.desk
		.commit_tree(personal, two.tree, &Commit::new(None), at, &mut two.rng)
		.expect("commits");
	nibkeep_testkit::assert_empty!(committed.changes);
	// Edited before an update, it is refused.
	write(&two.on_desk, "notes.md", b"one\ntwo\nthree\nFOUR!\n");
	let at = two.tick();
	assert!(matches!(
		two.desk
			.commit_tree(personal, two.tree, &Commit::new(None), at, &mut two.rng),
		Err(nibkeep_node::NodeError::TreeStale(_))
	));
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "notes.md"), "ONE\ntwo\nthree\nFOUR!\n");
	two.commit_on_desk("Merged");
}

#[test]
fn a_status_between_renaming_the_other_file_and_committing_keeps_what_settles_it() {
	let mut two = Two::new("tree conflict status", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "plan.md", b"laptop's plan\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "plan.md", b"desk's plan\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("laptop-plan.md"),
	)
	.expect("renames");
	two.desk.tree_status(personal, two.tree).expect("status");
	two.commit_on_desk("Both kept");
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
}

#[test]
fn competing_versions_merge_from_the_version_both_started_from() {
	let mut two = Two::new(
		"tree common base",
		&[("notes.md", b"one\nkeep\ntwo\nkeep\nthree\n")],
	);
	// The desk commits twice, the laptop once, apart.
	write(&two.on_desk, "notes.md", b"ONE\nkeep\ntwo\nkeep\nthree\n");
	two.commit_on_desk("Desk, first");
	write(&two.on_laptop, "notes.md", b"one\nkeep\nTWO\nkeep\nthree\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"ONE\nkeep\ntwo\nkeep\nTHREE\n");
	two.commit_on_desk("Desk, second");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(
		read(&two.on_desk, "notes.md"),
		"ONE\nkeep\nTWO\nkeep\nTHREE\n"
	);
}

#[test]
fn a_device_s_own_commit_cut_short_stays_in_its_copy() {
	let mut two = Two::new(
		"tree own cut short",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "a.txt", b"a, again\n");
	write(&two.on_laptop, "b.txt", b"b, again\n");
	let at = two.tick();
	let personal = two.laptop.personal_space();
	two.laptop
		.commit_tree(
			personal,
			two.tree,
			&Commit::new(None).cut_after(2),
			at,
			&mut two.rng,
		)
		.expect("commits a part");
	let updated = two
		.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.waiting);
	assert_eq!(read(&two.on_laptop, "a.txt"), "a, again\n");
}

#[test]
fn a_copy_beside_gets_the_other_device_s_newer_version() {
	let mut two = Two::new("tree beside refreshed", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "plan.md", b"laptop's plan\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "plan.md", b"desk's plan\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(
		read(&two.on_desk, "plan.md.conflict-laptop"),
		"laptop's plan\n"
	);
	write(&two.on_laptop, "plan.md", b"laptop's plan, again\n");
	two.commit_on_laptop("Laptop, again");
	two.sync();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.skipped);
	assert_eq!(
		read(&two.on_desk, "plan.md.conflict-laptop"),
		"laptop's plan, again\n"
	);
	assert_eq!(read(&two.on_desk, "plan.md"), "desk's plan\n");
}

impl Two {
	/// Both devices add `plan.md` apart, sync, and update: each copy keeps its own, with the
	/// other device's beside it.
	fn both_add_plan(&mut self) {
		write(&self.on_laptop, "plan.md", b"laptop's plan\n");
		self.commit_on_laptop("Laptop's plan");
		write(&self.on_desk, "plan.md", b"desk's plan\n");
		self.commit_on_desk("Desk's plan");
		self.sync();
		let personal = self.desk.personal_space();
		self.desk
			.update_tree(personal, self.tree, false)
			.expect("the desk updates");
		self.laptop
			.update_tree(personal, self.tree, false)
			.expect("the laptop updates");
	}

	/// The desk's index of copies, lost.
	fn with_desk_index_lost(self) -> Self {
		let Self {
			laptop,
			desk,
			tree,
			on_laptop,
			on_desk,
			at,
			rng,
		} = self;
		let replica = desk.close();
		fs::remove_file(replica.directory().join("trees.sqlite")).expect("loses the index");
		let desk = replica.open().expect("opens");
		Self {
			laptop,
			desk,
			tree,
			on_laptop,
			on_desk,
			at,
			rng,
		}
	}
}

#[test]
fn a_copy_beside_whose_file_found_a_place_of_its_own_goes_and_settles_nothing() {
	let mut two = Two::new("tree beside obsolete", &[("a.txt", b"a\n")]);
	two.both_add_plan();
	assert_eq!(
		read(&two.on_laptop, "plan.md.conflict-desk"),
		"desk's plan\n"
	);
	// The desk keeps both: the laptop's file, renamed.
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("laptop-plan.md"),
	)
	.expect("renames");
	two.commit_on_desk("Keep both");
	two.sync();
	let personal = two.laptop.personal_space();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "plan.md"), "desk's plan\n");
	assert_eq!(read(&two.on_laptop, "laptop-plan.md"), "laptop's plan\n");
	assert!(!two.on_laptop.join("plan.md.conflict-desk").exists());
	let status = two.laptop.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.competing);
}

#[test]
fn a_second_update_merges_nothing_again() {
	let mut two = Two::new("tree update twice", &[("notes.md", b"one\n")]);
	write(&two.on_laptop, "notes.md", b"laptop\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"desk\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	let first = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(first.conflicts, ["notes.md"]);
	let marked = read(&two.on_desk, "notes.md");
	assert!(marked.contains("<<<<<<<"), "{marked}");
	let second = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates again");
	nibkeep_testkit::assert_empty!(second.conflicts);
	assert_eq!(read(&two.on_desk, "notes.md"), marked);
}

#[test]
fn after_the_index_is_lost_an_update_puts_no_copy_of_a_file_beside_itself() {
	let mut two = Two::new(
		"tree update index lost",
		&[("notes.md", b"one\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "b.txt", b"b, edited\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one, edited here\n");
	two.sync();
	let mut two = two.with_desk_index_lost();
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.conflicts);
	assert!(
		updated.kept.iter().any(|(path, _)| path == "b.txt"),
		"{updated:?}"
	);
	assert_eq!(read(&two.on_desk, "notes.md"), "one, edited here\n");
	assert!(!two.on_desk.join("notes.md.conflict-laptop").exists());
	assert!(!two.on_desk.join("b.txt.conflict-laptop").exists());
	let _ = two.tick();
}

#[test]
fn a_file_moved_here_and_edited_elsewhere_is_not_written_back() {
	let mut two = Two::new(
		"tree moved here edited there",
		&[("a.txt", b"one\n"), ("c.txt", b"c\n")],
	);
	write(&two.on_laptop, "a.txt", b"one, edited\n");
	two.commit_on_laptop("Edit");
	two.sync();
	fs::rename(two.on_desk.join("a.txt"), two.on_desk.join("b.txt")).expect("moves here");
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("a.txt").exists());
	assert!(
		updated.kept.iter().any(|(path, _)| path == "a.txt"),
		"{updated:?}"
	);
	assert_eq!(read(&two.on_desk, "b.txt"), "one\n");
}

#[test]
fn a_file_moved_apart_on_two_devices_is_committed_without_force() {
	let mut two = Two::new("tree paths compete", &[("a.txt", b"a\n")]);
	fs::create_dir(two.on_laptop.join("x")).expect("makes a folder");
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("x/a.txt")).expect("moves");
	two.commit_on_laptop("To x");
	fs::create_dir(two.on_desk.join("y")).expect("makes a folder");
	fs::rename(two.on_desk.join("a.txt"), two.on_desk.join("y/a.txt")).expect("moves");
	two.commit_on_desk("To y");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.commit_on_desk("Settle");
	two.sync();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	let status = two.laptop.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
	nibkeep_testkit::assert_empty!(status.changes);
}

#[test]
fn a_copy_beside_renamed_before_a_newer_version_was_written_is_a_new_file() {
	let mut two = Two::new("tree beside renamed stale", &[("a.txt", b"a\n")]);
	two.both_add_plan();
	write(&two.on_laptop, "plan.md", b"laptop's plan, again\n");
	two.commit_on_laptop("Laptop, again");
	two.sync();
	// The desk renames the copy beside before it updates.
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("keep.md"),
	)
	.expect("renames");
	two.commit_on_desk("Keep it");
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "keep.md"), "laptop's plan\n");
	assert_eq!(
		read(&two.on_desk, "plan.md.conflict-laptop"),
		"laptop's plan, again\n"
	);
	assert_eq!(read(&two.on_desk, "plan.md"), "desk's plan\n");
}

#[test]
fn the_place_of_a_copy_beside_whose_file_is_gone_is_free_again() {
	let mut two = Two::new("tree beside gone", &[("a.txt", b"a\n")]);
	two.both_add_plan();
	fs::remove_file(two.on_laptop.join("plan.md")).expect("deletes");
	two.commit_on_laptop("No plan");
	two.sync();
	fs::remove_file(two.on_desk.join("plan.md.conflict-laptop")).expect("deletes the copy");
	let personal = two.desk.personal_space();
	two.desk.tree_status(personal, two.tree).expect("status");
	write(
		&two.on_desk,
		"plan.md.conflict-laptop",
		b"a file of my own\n",
	);
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	assert!(
		status
			.changes
			.iter()
			.any(|change| change.path == "plan.md.conflict-laptop"),
		"{status:?}"
	);
}

#[test]
fn an_edit_of_a_file_whose_path_another_holds_too_is_committed_without_force() {
	let mut two = Two::new("tree two at one path edit", &[("a.txt", b"a\n")]);
	two.both_add_plan();
	write(&two.on_desk, "plan.md", b"desk's plan, edited\n");
	two.commit_on_desk("Edit");
	write(&two.on_laptop, "plan.md", b"laptop's plan, edited\n");
	two.commit_on_laptop("Edit");
}

#[test]
fn parts_taken_with_partial_stay_after_a_plain_update() {
	let mut two = Two::new("tree partial kept", &[("a.txt", b"a\n"), ("b.txt", b"b\n")]);
	write(&two.on_laptop, "a.txt", b"a, again\n");
	write(&two.on_laptop, "b.txt", b"b, again\n");
	let at = two.tick();
	let personal = two.laptop.personal_space();
	two.laptop
		.commit_tree(
			personal,
			two.tree,
			&Commit::new(None).cut_after(2),
			at,
			&mut two.rng,
		)
		.expect("commits a part");
	two.sync();
	let partial = two
		.desk
		.update_tree(personal, two.tree, true)
		.expect("updates");
	let written: Vec<String> = partial.written.clone();
	assert_eq!(written.len(), 1, "{partial:?}");
	let content = read(&two.on_desk, &written[0]);
	let plain = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(plain.written);
	assert_eq!(read(&two.on_desk, &written[0]), content);
}

#[cfg(unix)]
#[test]
fn a_copy_beside_of_an_executable_file_gets_newer_versions() {
	use std::os::unix::fs::PermissionsExt as _;
	let mut two = Two::new("tree beside executable", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "run.sh", b"echo laptop\n");
	fs::set_permissions(
		two.on_laptop.join("run.sh"),
		fs::Permissions::from_mode(0o755),
	)
	.expect("makes it executable");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "run.sh", b"echo desk\n");
	two.commit_on_desk("Desk");
	two.sync();
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	write(&two.on_laptop, "run.sh", b"echo laptop, again\n");
	two.commit_on_laptop("Laptop, again");
	two.sync();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.skipped);
	assert_eq!(
		read(&two.on_desk, "run.sh.conflict-laptop"),
		"echo laptop, again\n"
	);
}

#[test]
fn a_file_added_elsewhere_where_one_not_committed_is_here_stays_beside_it() {
	let mut two = Two::new("tree beside local file", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "plan.md", b"laptop's plan\n");
	two.commit_on_laptop("Laptop's plan");
	two.sync();
	write(&two.on_desk, "plan.md", b"desk's plan, not committed\n");
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(
		read(&two.on_desk, "plan.md.conflict-laptop"),
		"laptop's plan\n"
	);
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates again");
	assert_eq!(
		read(&two.on_desk, "plan.md.conflict-laptop"),
		"laptop's plan\n"
	);
	// Keeping both: the laptop's file renamed, the desk's committed as a file of its own.
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("laptop-plan.md"),
	)
	.expect("renames");
	two.commit_on_desk("Both");
	two.sync();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "laptop-plan.md"), "laptop's plan\n");
	assert_eq!(
		read(&two.on_laptop, "plan.md"),
		"desk's plan, not committed\n"
	);
}

#[test]
fn an_update_before_the_commit_does_not_undo_what_the_person_did_with_a_copy_beside() {
	let mut two = Two::new("tree beside settled then update", &[("a.txt", b"a\n")]);
	two.both_add_plan();
	fs::remove_file(two.on_desk.join("plan.md.conflict-laptop")).expect("deletes the copy");
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("plan.md.conflict-laptop").exists());
	two.commit_on_desk("Only mine");
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.competing);
}

#[test]
fn a_deletion_of_a_file_whose_versions_compete_waits_for_an_update() {
	let mut two = Two::new(
		"tree delete competing",
		&[("notes.md", b"one\ntwo\nthree\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one\ntwo\nTHREE\n");
	two.commit_on_desk("Desk");
	two.sync();
	fs::remove_file(two.on_desk.join("notes.md")).expect("deletes");
	let personal = two.desk.personal_space();
	let at = two.tick();
	assert!(matches!(
		two.desk
			.commit_tree(personal, two.tree, &Commit::new(None), at, &mut two.rng),
		Err(nibkeep_node::NodeError::TreeStale(_))
	));
}

#[test]
fn a_file_deleted_here_and_moved_elsewhere_is_not_written_back_after_a_status() {
	let mut two = Two::new(
		"tree deleted here moved there",
		&[("x.txt", b"x\n"), ("b.txt", b"b\n")],
	);
	fs::rename(two.on_laptop.join("x.txt"), two.on_laptop.join("y.txt")).expect("moves");
	two.commit_on_laptop("Move");
	two.sync();
	fs::remove_file(two.on_desk.join("x.txt")).expect("deletes here");
	let personal = two.desk.personal_space();
	two.desk.tree_status(personal, two.tree).expect("status");
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("y.txt").exists());
	assert!(!two.on_desk.join("x.txt").exists());
}

#[test]
fn another_device_s_commit_stays_left_out_after_a_commit_here() {
	let mut two = Two::new(
		"tree cut short then commit here",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "a.txt", b"a, again\n");
	write(&two.on_laptop, "b.txt", b"b, again\n");
	let at = two.tick();
	let personal = two.laptop.personal_space();
	two.laptop
		.commit_tree(
			personal,
			two.tree,
			&Commit::new(None).cut_after(2),
			at,
			&mut two.rng,
		)
		.expect("commits a part");
	two.sync();
	write(&two.on_desk, "c.txt", b"c\n");
	two.commit_on_desk("Desk");
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(updated.waiting.len(), 1, "{updated:?}");
	assert_eq!(read(&two.on_desk, "a.txt"), "a\n");
	assert_eq!(read(&two.on_desk, "b.txt"), "b\n");
}

#[test]
fn a_tree_written_with_parts_of_a_commit_keeps_them_after_an_update() {
	let name = "tree write keeps parts";
	let mut rng = TestRng::new(name);
	let (_, mut laptop) = library(&format!("{name}, laptop"), &mut rng);
	let on_laptop = folder(&format!("{name}, on laptop"));
	write(&on_laptop, "a.txt", b"a\n");
	write(&on_laptop, "b.txt", b"b\n");
	let personal = laptop.personal_space();
	let tree = laptop
		.start_tree(personal, &on_laptop, Some("Start"), now() + 1_000, &mut rng)
		.expect("starts")
		.tree;
	write(&on_laptop, "a.txt", b"a, again\n");
	write(&on_laptop, "g.txt", b"g\n");
	laptop
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).cut_after(2),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits a part");
	let desk_directory = new_folder(&format!("{name}, desk"));
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	let on_desk = new_folder(&format!("{name}, on desk"));
	desk.write_tree(
		personal,
		tree,
		&on_desk,
		TreePoint::Now,
		now() + 70_000,
		&mut rng,
	)
	.expect("the desk writes its copy");
	let written: Vec<String> = ["a.txt", "b.txt", "g.txt"]
		.iter()
		.filter(|path| on_desk.join(path).exists())
		.map(|path| read(&on_desk, path))
		.collect();
	desk.update_tree(personal, tree, false).expect("updates");
	let after: Vec<String> = ["a.txt", "b.txt", "g.txt"]
		.iter()
		.filter(|path| on_desk.join(path).exists())
		.map(|path| read(&on_desk, path))
		.collect();
	assert_eq!(after, written);
}

#[test]
fn a_competing_file_with_no_copy_here_is_written_with_its_versions_merged() {
	let mut two = Two::new(
		"tree competing no copy",
		&[("notes.md", b"one\ntwo\nthree\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one\ntwo\nTHREE\n");
	two.commit_on_desk("Desk");
	two.sync();
	fs::remove_file(two.on_desk.join("notes.md")).expect("deletes");
	let replica_lost = two.with_desk_index_lost();
	let mut two = replica_lost;
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "notes.md"), "ONE\ntwo\nTHREE\n");
	let at = two.tick();
	two.desk
		.commit_tree(personal, two.tree, &Commit::new(None), at, &mut two.rng)
		.expect("commits without force");
}

#[test]
fn a_file_moved_apart_stays_where_this_device_moved_it() {
	let mut two = Two::new(
		"tree moved apart stays",
		&[("a.txt", b"a\n"), ("b.txt", b"b\n")],
	);
	fs::create_dir(two.on_laptop.join("x")).expect("makes a folder");
	fs::rename(two.on_laptop.join("a.txt"), two.on_laptop.join("x/a.txt")).expect("moves");
	two.commit_on_laptop("To x");
	fs::create_dir(two.on_desk.join("y")).expect("makes a folder");
	fs::rename(two.on_desk.join("a.txt"), two.on_desk.join("y/a.txt")).expect("moves");
	two.commit_on_desk("To y");
	two.sync();
	let personal = two.desk.personal_space();
	let updated = two
		.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "y/a.txt"), "a\n");
	assert!(!two.on_desk.join("x/a.txt").exists());
	assert!(
		updated.kept.iter().any(|(path, _)| path == "y/a.txt"),
		"{updated:?}"
	);
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "x/a.txt"), "a\n");
}

#[test]
fn a_copy_beside_renamed_onto_its_path_keeps_the_other_device_s_file() {
	let mut two = Two::new("tree take theirs", &[("a.txt", b"a\n")]);
	write(&two.on_laptop, "plan.md", b"laptop's plan\n");
	two.commit_on_laptop("Laptop's plan");
	two.sync();
	write(&two.on_desk, "plan.md", b"desk's draft, not committed\n");
	let personal = two.desk.personal_space();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	// Theirs it is.
	fs::rename(
		two.on_desk.join("plan.md.conflict-laptop"),
		two.on_desk.join("plan.md"),
	)
	.expect("renames");
	two.commit_on_desk("Take theirs");
	two.sync();
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_desk, "plan.md"), "laptop's plan\n");
	assert_eq!(read(&two.on_laptop, "plan.md"), "laptop's plan\n");
}

#[test]
fn a_move_made_on_both_devices_deletes_nothing() {
	let mut two = Two::new("tree same move", &[("x.txt", b"x\n"), ("b.txt", b"b\n")]);
	fs::rename(two.on_laptop.join("x.txt"), two.on_laptop.join("y.txt")).expect("moves");
	two.commit_on_laptop("Move");
	two.sync();
	fs::rename(two.on_desk.join("x.txt"), two.on_desk.join("y.txt")).expect("moves here too");
	two.commit_on_desk("Move too");
	two.sync();
	let personal = two.laptop.personal_space();
	two.laptop
		.update_tree(personal, two.tree, false)
		.expect("updates");
	two.desk
		.update_tree(personal, two.tree, false)
		.expect("updates");
	assert_eq!(read(&two.on_laptop, "y.txt"), "x\n");
	assert_eq!(read(&two.on_desk, "y.txt"), "x\n");
}

#[test]
fn an_untouched_copy_whose_versions_compete_blocks_no_commit() {
	let mut two = Two::new(
		"tree competing untouched",
		&[("notes.md", b"one\ntwo\nthree\n"), ("b.txt", b"b\n")],
	);
	write(&two.on_laptop, "notes.md", b"ONE\ntwo\nthree\n");
	two.commit_on_laptop("Laptop");
	write(&two.on_desk, "notes.md", b"one\ntwo\nTHREE\n");
	two.commit_on_desk("Desk");
	two.sync();
	write(&two.on_desk, "b.txt", b"b, edited\n");
	two.commit_on_desk("Only b");
	let personal = two.desk.personal_space();
	let status = two.desk.tree_status(personal, two.tree).expect("status");
	assert_eq!(status.competing, ["notes.md"]);
}
