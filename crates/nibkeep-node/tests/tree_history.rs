//! The history of trees: the log, diffs and blame, read from the library's history; going
//! back by restoring and reverting, which write the copy and record nothing; and writing a
//! tree into a folder of its own, as it is, at a revision or as of a moment
//! ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{library, now, today};
use nibkeep_base::EntityId;
use nibkeep_model::Value;
use nibkeep_node::{
	Change, Commit, Edit, Library, NodeError, TreeChange, TreePoint, resolved_path,
};
use nibkeep_std::{COMMITTED_IN, TREE_PATH};
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

/// Writes a file, with its folders, and makes it look written an hour ago.
fn write(root: &Path, path: &str, content: &str) -> PathBuf {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	let hour_ago = SystemTime::now() - Duration::from_secs(3_600);
	fs::File::options()
		.write(true)
		.open(&path)
		.and_then(|file| file.set_modified(hour_ago))
		.expect("ages");
	path
}

fn read(root: &Path, path: &str) -> String {
	fs::read_to_string(root.join(path)).expect("reads")
}

fn changes(changes: &[TreeChange]) -> Vec<(String, Change)> {
	changes
		.iter()
		.map(|change| (change.path.clone(), change.change.clone()))
		.collect()
}

/// The entity at a path of a tree, as the library has it now.
fn at_path(library: &Library, tree: EntityId, path: &str) -> EntityId {
	let personal = library.personal_space();
	library
		.tree_entries(personal, tree)
		.expect("reads")
		.into_iter()
		.find(|entry| entry.path == path)
		.unwrap_or_else(|| panic!("nothing at {path}"))
		.entity
}

/// A tree with three commits: the first, then an edit and a new file, then a move and a
/// deletion; the revisions are returned.
fn with_history(name: &str, rng: &mut TestRng) -> (Library, EntityId, PathBuf, Vec<EntityId>) {
	let (_, mut library) = library(name, rng);
	let root = folder(&format!("{name}, tree"));
	write(&root, "notes.md", "one\ntwo\nthree\nfour\n");
	write(&root, "old.txt", "Old\n");
	write(&root, "keep.txt", "Keep\n");
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, Some("Start"), now() + 1_000, rng)
		.expect("starts");
	let tree = started.tree;
	let mut revisions: Vec<EntityId> = started.committed.revision.into_iter().collect();
	write(&root, "notes.md", "one\nTWO\nthree\nfour\n");
	write(&root, "new.txt", "New\n");
	let second = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Second")),
			now() + 2_000,
			rng,
		)
		.expect("commits");
	revisions.extend(second.revision);
	fs::rename(root.join("keep.txt"), root.join("kept/keep.txt")).unwrap_or_else(|_| {
		fs::create_dir(root.join("kept")).expect("makes a folder");
		fs::rename(root.join("keep.txt"), root.join("kept/keep.txt")).expect("moves");
	});
	fs::remove_file(root.join("old.txt")).expect("deletes");
	write(&root, "notes.md", "one\nTWO\nthree\nFOUR\n");
	let third = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Third")),
			now() + 3_000,
			rng,
		)
		.expect("commits");
	revisions.extend(third.revision);
	(library, tree, root, revisions)
}

#[test]
fn the_log_lists_each_change_to_the_tree_newest_first_with_its_message() {
	let mut rng = TestRng::new("tree log");
	let (mut library, tree, _root, revisions) = with_history("tree log", &mut rng);
	let personal = library.personal_space();
	// A change made otherwise, as with nib edit, is listed without a revision.
	let new = at_path(&library, tree, "new.txt");
	library
		.edit_entity(
			personal,
			new,
			vec![Edit::Set(
				TREE_PATH,
				vec![Value::Text("renamed.txt".to_owned())],
			)],
			&today(),
			now() + 4_000,
			&mut rng,
		)
		.expect("moves it as nib edit would");
	let log = library.tree_log(personal, tree, None).expect("reads");
	let messages: Vec<Option<&str>> = log.iter().map(|entry| entry.message.as_deref()).collect();
	assert_eq!(
		messages,
		[None, Some("Third"), Some("Second"), Some("Start")]
	);
	assert_eq!(log[0].revision, None);
	assert_eq!(log[1].revision, Some(revisions[2]));
	assert_eq!(
		changes(&log[0].changes),
		[(
			"renamed.txt".to_owned(),
			Change::Moved {
				from: "new.txt".to_owned()
			}
		)]
	);
	assert_eq!(
		changes(&log[1].changes),
		[
			(
				"kept/keep.txt".to_owned(),
				Change::Moved {
					from: "keep.txt".to_owned()
				}
			),
			("notes.md".to_owned(), Change::Edited),
			("old.txt".to_owned(), Change::Deleted),
		]
	);
	assert_eq!(log[3].changes.len(), 3);
	let author = log[1].author.as_ref().expect("an author");
	assert_eq!(author.user.name.as_deref(), Some("Ada"));
	assert!(log.windows(2).all(|pair| pair[0].at >= pair[1].at));

	// The log of a path follows its file across moves.
	let kept = library
		.tree_log(personal, tree, Some("kept/keep.txt"))
		.expect("reads");
	let messages: Vec<Option<&str>> = kept.iter().map(|entry| entry.message.as_deref()).collect();
	assert_eq!(messages, [Some("Third"), Some("Start")]);
}

#[test]
fn a_diff_compares_the_copy_with_the_tree_or_two_points_of_it() {
	let mut rng = TestRng::new("tree diff");
	let (library, tree, root, revisions) = with_history("tree diff", &mut rng);
	let personal = library.personal_space();
	write(&root, "notes.md", "one\nTWO\nthree\nFOUR\nfive\n");
	fs::write(root.join("picture.bin"), [0_u8, 1, 2, 3]).expect("writes");
	let diffs = library.tree_diff(personal, tree, None).expect("diffs");
	let shown: Vec<(&str, &Change)> = diffs
		.iter()
		.map(|diff| (diff.path.as_str(), &diff.change))
		.collect();
	assert_eq!(
		shown,
		[("notes.md", &Change::Edited), ("picture.bin", &Change::New)]
	);
	let patch = diffs[0].patch.as_deref().expect("a text diff");
	assert!(patch.contains("+five"), "{patch}");
	assert!(diffs[1].binary, "a file with a zero byte is no text");

	// Between two revisions, as their authors had the tree.
	let diffs = library
		.tree_diff(
			personal,
			tree,
			Some((
				TreePoint::Revision(revisions[0]),
				TreePoint::Revision(revisions[2]),
			)),
		)
		.expect("diffs");
	let shown: Vec<(&str, &Change)> = diffs
		.iter()
		.map(|diff| (diff.path.as_str(), &diff.change))
		.collect();
	assert_eq!(
		shown,
		[
			(
				"kept/keep.txt",
				&Change::Moved {
					from: "keep.txt".to_owned()
				}
			),
			("new.txt", &Change::New),
			("notes.md", &Change::Edited),
			("old.txt", &Change::Deleted),
		]
	);
	let patch = diffs[2].patch.as_deref().expect("a text diff");
	assert!(
		patch.contains("-two") && patch.contains("+TWO") && patch.contains("+FOUR"),
		"{patch}"
	);
}

#[test]
fn a_revision_s_diff_shows_what_it_changed_and_nothing_else() {
	let mut rng = TestRng::new("tree revision diff");
	let (library, tree, _root, revisions) = with_history("tree revision diff", &mut rng);
	let personal = library.personal_space();
	let diffs = library
		.revision_diff(personal, revisions[2], false)
		.expect("diffs")
		.expect("a revision");
	let shown: Vec<(&str, &Change)> = diffs
		.iter()
		.map(|diff| (diff.path.as_str(), &diff.change))
		.collect();
	assert_eq!(
		shown,
		[
			(
				"kept/keep.txt",
				&Change::Moved {
					from: "keep.txt".to_owned()
				}
			),
			("notes.md", &Change::Edited),
			("old.txt", &Change::Deleted),
		]
	);
	// Its own edit alone, from the version the revision's author edited.
	let patch = diffs[1].patch.as_deref().expect("a text diff");
	assert!(
		patch.contains("-four") && patch.contains("+FOUR"),
		"{patch}"
	);
	assert!(!patch.contains("-two"), "{patch}");
	// The first revision adds every file.
	let first = library
		.revision_diff(personal, revisions[0], false)
		.expect("diffs")
		.expect("a revision");
	assert!(first.iter().all(|diff| diff.change == Change::New));
	assert_eq!(first.len(), 3);
	// Of an entity that is no revision, there is none.
	assert_eq!(
		library.revision_diff(personal, tree, false).expect("reads"),
		None
	);
}

#[test]
fn blame_names_the_revision_that_last_changed_each_line_across_moves() {
	let mut rng = TestRng::new("tree blame");
	let (mut library, tree, root, revisions) = with_history("tree blame", &mut rng);
	let personal = library.personal_space();
	library
		.move_in_tree(
			personal,
			tree,
			("notes.md", "docs/notes.md"),
			now() + 4_000,
			&mut rng,
		)
		.expect("moves");
	write(&root, "docs/notes.md", "one\nTWO\nthree\nFOUR\nfive\n");
	let fourth = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Fourth")),
			now() + 5_000,
			&mut rng,
		)
		.expect("commits");
	let blame = library
		.tree_blame(personal, tree, "docs/notes.md")
		.expect("blames");
	let lines: Vec<(&str, Option<EntityId>)> = blame
		.iter()
		.map(|line| (line.text.as_str(), line.revision))
		.collect();
	assert_eq!(
		lines,
		[
			("one", Some(revisions[0])),
			("TWO", Some(revisions[1])),
			("three", Some(revisions[0])),
			("FOUR", Some(revisions[2])),
			("five", Some(fourth.revision.expect("a revision"))),
		]
	);
	assert!(blame.iter().all(|line| line.author.is_some()));
}

#[test]
fn a_view_of_a_moment_between_a_commit_s_parts_shows_the_tree_before_it() {
	let mut rng = TestRng::new("tree moment apart");
	let (_, mut library) = library("tree moment apart", &mut rng);
	let root = folder("tree moment apart, files");
	for path in ["a.txt", "b.txt"] {
		write(&root, path, "before\n");
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts");
	for path in ["a.txt", "b.txt"] {
		write(&root, path, "after\n");
	}
	// The commit's revision and a.txt's part at its moment, and b.txt's part later, as when
	// its device took in a later change of another between batches of parts.
	let committed = library
		.commit_tree(
			personal,
			started.tree,
			&Commit::new(Some("Both")).cut_after(2),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.changes.len(), 1);
	assert_eq!(committed.changes[0].path, "a.txt");
	let revision = committed.revision.expect("a revision");
	let b = library
		.tree_entries(personal, started.tree)
		.expect("reads")
		.into_iter()
		.find(|entry| entry.path == "b.txt")
		.expect("b.txt")
		.entity;
	library
		.edit_entity(
			personal,
			b,
			vec![Edit::Set(COMMITTED_IN, vec![Value::Reference(revision)])],
			&today(),
			now() + 5_000,
			&mut rng,
		)
		.expect("records the last part");
	// Between its parts, the tree is as it was before the commit; after them, with it.
	for (moment, a) in [(now() + 3_000, "before\n"), (now() + 6_000, "after\n")] {
		let into = new_folder(&format!("tree moment apart, at {moment}"));
		library
			.write_tree(
				personal,
				started.tree,
				&into,
				TreePoint::AsOf(moment),
				now() + 7_000,
				&mut rng,
			)
			.expect("writes");
		assert_eq!(read(&into, "a.txt"), a, "at {moment}");
	}
}

#[test]
fn a_view_of_a_moment_shows_a_commit_in_parts_whole_or_not_at_all() {
	let mut rng = TestRng::new("tree moment");
	let (_, mut library) = library("tree moment", &mut rng);
	let root = folder("tree moment, files");
	let files = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
	for path in files {
		write(&root, path, "before\n");
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts");
	for path in files {
		write(&root, path, "after\n");
	}
	let committed = library
		.commit_tree(
			personal,
			started.tree,
			&Commit::new(Some("All")),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.parts, 1 + files.len(), "a part for each file");
	// The parts share the moment of the commit: a moment before it shows none of them,
	// and its moment shows all.
	for (moment, content) in [(now() + 1_999, "before\n"), (now() + 2_000, "after\n")] {
		let into = new_folder(&format!("tree moment, at {moment}"));
		library
			.write_tree(
				personal,
				started.tree,
				&into,
				TreePoint::AsOf(moment),
				now() + 3_000,
				&mut rng,
			)
			.expect("writes");
		for path in files {
			assert_eq!(read(&into, path), content, "{path} at {moment}");
		}
	}
	// The log shows the commit once, with its message and every file.
	let log = library
		.tree_log(personal, started.tree, None)
		.expect("reads the log");
	assert_eq!(log[0].revision, committed.revision);
	assert_eq!(log[0].message.as_deref(), Some("All"));
	assert_eq!(log[0].part, None, "every part is here");
	assert_eq!(log[0].changes.len(), files.len());
}

#[test]
fn a_tree_is_written_into_a_folder_of_its_own_as_it_is_or_was() {
	let mut rng = TestRng::new("tree write");
	let (library, tree, _root, revisions) = with_history("tree write", &mut rng);
	let personal = library.personal_space();
	let first = new_folder("tree write, first");
	let mut library = library;
	let written = library
		.write_tree(
			personal,
			tree,
			&first,
			TreePoint::Revision(revisions[0]),
			now() + 4_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(written.written, ["keep.txt", "notes.md", "old.txt"]);
	assert_eq!(read(&first, "notes.md"), "one\ntwo\nthree\nfour\n");
	// A picture of the past, not a copy to commit from.
	assert!(library.tree_at(personal, &first).expect("looks").is_none());
	let second = new_folder("tree write, second");
	library
		.write_tree(
			personal,
			tree,
			&second,
			TreePoint::AsOf(now() + 2_500),
			now() + 4_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(read(&second, "notes.md"), "one\nTWO\nthree\nfour\n");
	assert!(second.join("new.txt").exists());
	// Never over what is there, nor the tree as it is where this device has its copy.
	assert!(matches!(
		library.write_tree(
			personal,
			tree,
			&first,
			TreePoint::Revision(revisions[1]),
			now() + 5_000,
			&mut rng
		),
		Err(NodeError::FolderNotEmpty(_))
	));
	assert!(matches!(
		library.write_tree(
			personal,
			tree,
			&new_folder("tree write, now"),
			TreePoint::Now,
			now() + 5_000,
			&mut rng
		),
		Err(NodeError::TreeHereAlready(_))
	));
}

#[test]
fn a_tree_written_at_a_revision_holds_what_its_author_had_and_nothing_concurrent() {
	let mut rng = TestRng::new("tree write at revision");
	let (_, mut laptop) = library("tree write at revision, laptop", &mut rng);
	let root = folder("tree write at revision, tree");
	write(&root, "a.txt", "a\n");
	write(&root, "c.txt", "c\n");
	let personal = laptop.personal_space();
	let tree = laptop
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts")
		.tree;
	let desk_directory = new_folder("tree write at revision, desk");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	// Offline, the desk moves a.txt, before the laptop's next commit, which does not see it.
	let a = at_path(&desk, tree, "a.txt");
	desk.edit_entity(
		personal,
		a,
		vec![Edit::Set(TREE_PATH, vec![Value::Text("b.txt".to_owned())])],
		&today(),
		now() + 2_000,
		&mut rng,
	)
	.expect("moves it");
	write(&root, "c.txt", "c, edited\n");
	let commit = laptop
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Edit c")),
			now() + 3_000,
			&mut rng,
		)
		.expect("commits");
	let (first, second) = common::sync_in_memory(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	first.expect("the desk synced");
	second.expect("the laptop synced");
	let at = new_folder("tree write at revision, at");
	laptop
		.write_tree(
			personal,
			tree,
			&at,
			TreePoint::Revision(commit.revision.expect("a revision")),
			now() + 5_000,
			&mut rng,
		)
		.expect("writes");
	assert!(
		at.join("a.txt").exists(),
		"the desk's move is no part of it"
	);
	assert!(!at.join("b.txt").exists());
	assert_eq!(read(&at, "c.txt"), "c, edited\n");
	// As of a moment, the library's record includes what came from the desk by then: the
	// devices' clocks moved on to the pairing's.
	let as_of = new_folder("tree write at revision, as of");
	laptop
		.write_tree(
			personal,
			tree,
			&as_of,
			TreePoint::AsOf(now() + 120_000),
			now() + 5_000,
			&mut rng,
		)
		.expect("writes");
	assert!(as_of.join("b.txt").exists());
}

#[test]
fn a_device_without_a_copy_writes_the_tree_as_it_is_and_keeps_it() {
	let mut rng = TestRng::new("tree written elsewhere");
	let (_, mut laptop) = library("tree written elsewhere, laptop", &mut rng);
	let root = folder("tree written elsewhere, tree");
	write(&root, "notes.md", "# Notes\n");
	write(&root, "docs/plan.md", "Plan\n");
	let personal = laptop.personal_space();
	let tree = laptop
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts")
		.tree;
	let desk_directory = new_folder("tree written elsewhere, desk");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	let copy = new_folder("tree written elsewhere, copy");
	let written = desk
		.write_tree(
			personal,
			tree,
			&copy,
			TreePoint::Now,
			now() + 2_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(written.written, ["docs/plan.md", "notes.md"]);
	assert_eq!(read(&copy, "docs/plan.md"), "Plan\n");
	let here = desk
		.tree_at(personal, &copy)
		.expect("looks")
		.expect("the desk's copy");
	assert_eq!(here.id, tree);
	let status = desk.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
}

#[test]
fn restore_writes_an_older_version_or_a_deleted_file_into_the_copy_and_records_nothing() {
	let mut rng = TestRng::new("tree restore");
	let (mut library, tree, root, revisions) = with_history("tree restore", &mut rng);
	let personal = library.personal_space();
	let changes_before = library.status().expect("status").spaces[0].changes;
	// A file deleted from the tree comes back into the copy, to commit.
	let restored = library
		.restore_in_tree(personal, tree, "old.txt", TreePoint::Now)
		.expect("restores");
	assert_eq!(restored.written, ["old.txt"]);
	assert_eq!(read(&root, "old.txt"), "Old\n");
	// An older version, as a revision's author had it.
	library
		.restore_in_tree(
			personal,
			tree,
			"notes.md",
			TreePoint::Revision(revisions[0]),
		)
		.expect("restores");
	assert_eq!(read(&root, "notes.md"), "one\ntwo\nthree\nfour\n");
	assert_eq!(
		library.status().expect("status").spaces[0].changes,
		changes_before,
		"nothing recorded"
	);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			("notes.md".to_owned(), Change::Edited),
			("old.txt".to_owned(), Change::New),
		]
	);
	// A copy with edits not committed keeps them beside it, as Mercurial's revert does,
	// under a name of its own each time.
	write(&root, "kept/keep.txt", "Keep, edited\n");
	let restored = library
		.restore_in_tree(personal, tree, "kept/keep.txt", TreePoint::Now)
		.expect("restores");
	assert_eq!(restored.written, ["kept/keep.txt"]);
	assert_eq!(
		restored.aside,
		[("kept/keep.txt".to_owned(), "kept/keep.txt.orig".to_owned())]
	);
	assert_eq!(read(&root, "kept/keep.txt"), "Keep\n");
	assert_eq!(read(&root, "kept/keep.txt.orig"), "Keep, edited\n");
	write(&root, "kept/keep.txt", "Keep, edited again\n");
	let restored = library
		.restore_in_tree(personal, tree, "kept/keep.txt", TreePoint::Now)
		.expect("restores");
	assert_eq!(
		restored.aside,
		[(
			"kept/keep.txt".to_owned(),
			"kept/keep.txt.orig-2".to_owned()
		)]
	);
	assert_eq!(read(&root, "kept/keep.txt.orig"), "Keep, edited\n");
	assert_eq!(read(&root, "kept/keep.txt.orig-2"), "Keep, edited again\n");
	// No commit takes in the edits set aside: the status shows them until they are gone.
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			("notes.md".to_owned(), Change::Edited),
			("old.txt".to_owned(), Change::New),
		]
	);
	let shown: Vec<&str> = status
		.skipped
		.iter()
		.map(|(path, _)| path.as_str())
		.collect();
	assert_eq!(shown, ["kept/keep.txt.orig", "kept/keep.txt.orig-2"]);
	fs::remove_file(root.join("kept/keep.txt.orig")).expect("deletes");
	fs::remove_file(root.join("kept/keep.txt.orig-2")).expect("deletes");
	// What a revision's author had at a path is what is restored there.
	assert!(matches!(
		library.restore_in_tree(
			personal,
			tree,
			"kept/keep.txt",
			TreePoint::Revision(revisions[0])
		),
		Err(NodeError::NotInTree(_))
	));
	// Committing a deleted file restored brings back its file, with its history.
	let old_before = library
		.tree_log(personal, tree, Some("old.txt"))
		.expect("reads")
		.len();
	library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_paths(vec!["old.txt".to_owned()]),
			now() + 4_000,
			&mut rng,
		)
		.expect("commits");
	let old_after = library
		.tree_log(personal, tree, Some("old.txt"))
		.expect("reads")
		.len();
	assert_eq!(old_after, old_before + 1, "the same file, with its history");
}

#[test]
fn revert_undoes_a_revision_alone_keeping_what_later_revisions_changed() {
	let mut rng = TestRng::new("tree revert");
	let (library, tree, root, revisions) = with_history("tree revert", &mut rng);
	let personal = library.personal_space();
	let changes_before = library.status().expect("status").spaces[0].changes;
	let reverted = library
		.revert_in_tree(personal, tree, revisions[1])
		.expect("reverts the second revision");
	nibkeep_testkit::assert_empty!(reverted.conflicts);
	// Its edit of line two goes; the third revision's edit of line four stays.
	assert_eq!(read(&root, "notes.md"), "one\ntwo\nthree\nFOUR\n");
	assert!(!root.join("new.txt").exists(), "the file it added goes");
	assert_eq!(
		library.status().expect("status").spaces[0].changes,
		changes_before,
		"nothing recorded"
	);
	// Reverting the third: the moved file goes back, the deleted one comes back.
	write(&root, "notes.md", "one\nTWO\nthree\nFOUR\n");
	write(&root, "new.txt", "New\n");
	let reverted = library
		.revert_in_tree(personal, tree, revisions[2])
		.expect("reverts the third revision");
	nibkeep_testkit::assert_empty!(reverted.conflicts);
	assert!(root.join("keep.txt").exists());
	assert!(!root.join("kept/keep.txt").exists());
	assert_eq!(read(&root, "old.txt"), "Old\n");
	assert_eq!(read(&root, "notes.md"), "one\nTWO\nthree\nfour\n");
}

#[test]
fn revert_marks_where_later_edits_clash() {
	let mut rng = TestRng::new("tree revert clash");
	let (mut library, tree, root, revisions) = with_history("tree revert clash", &mut rng);
	let personal = library.personal_space();
	write(&root, "notes.md", "one\nTwo, again\nthree\nFOUR\n");
	library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Fourth")),
			now() + 4_000,
			&mut rng,
		)
		.expect("commits");
	let reverted = library
		.revert_in_tree(personal, tree, revisions[1])
		.expect("reverts");
	assert_eq!(reverted.conflicts, ["notes.md"]);
	let merged = read(&root, "notes.md");
	assert!(
		merged.contains("<<<<<<<") && merged.contains(">>>>>>>"),
		"{merged}"
	);
	assert!(
		merged.contains("Two, again") && merged.contains("two"),
		"{merged}"
	);
}

#[test]
fn what_a_change_says_is_never_written_outside_the_tree_or_into_git_or_the_library() {
	let mut rng = TestRng::new("tree write unsafe");
	let (_, mut library) = library("tree write unsafe", &mut rng);
	let root = folder("tree write unsafe, tree");
	write(&root, "a.txt", "a");
	write(&root, "b.txt", "b");
	write(&root, "c.txt", "c");
	let personal = library.personal_space();
	let tree = library
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts")
		.tree;
	// As a hostile replica could, paths that leave the tree or name Git's folder.
	for (path, to) in [
		("a.txt", "../escaped.txt"),
		("b.txt", ".git/hooks/post-checkout"),
		("c.txt", ".nib/keys.cbor"),
	] {
		let entity = at_path(&library, tree, path);
		library
			.edit_entity(
				personal,
				entity,
				vec![Edit::Set(TREE_PATH, vec![Value::Text(to.to_owned())])],
				&today(),
				now() + 2_000,
				&mut rng,
			)
			.expect("records the path");
	}
	let parent = new_folder("tree write unsafe, parent");
	fs::create_dir(&parent).expect("makes a folder");
	let copy = parent.join("copy");
	let written = library
		.write_tree(
			personal,
			tree,
			&copy,
			TreePoint::AsOf(now() + 3_000),
			now() + 3_000,
			&mut rng,
		)
		.expect("writes");
	nibkeep_testkit::assert_empty!(written.written);
	assert!(!parent.join("escaped.txt").exists());
	assert!(!copy.join(".git").exists());
	assert!(!copy.join(".nib").exists());
}

/// A tree with one commit of `files` and the library, as bytes; the first revision is
/// returned.
fn with_bytes(
	name: &str,
	files: &[(&str, &[u8])],
	rng: &mut TestRng,
) -> (Library, EntityId, PathBuf, EntityId) {
	let (_, mut library) = library(name, rng);
	let root = folder(&format!("{name}, tree"));
	for (path, bytes) in files {
		let path = root.join(path);
		fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
		fs::write(&path, bytes).expect("writes");
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, None, now() + 1_000, rng)
		.expect("starts");
	(
		library,
		started.tree,
		root,
		started.committed.revision.expect("a revision"),
	)
}

/// Commits what changed, at a moment.
fn commit(library: &mut Library, tree: EntityId, at: u64, rng: &mut TestRng) -> EntityId {
	let personal = library.personal_space();
	library
		.commit_tree(personal, tree, &Commit::new(None), at, rng)
		.expect("commits")
		.revision
		.expect("a revision")
}

#[test]
fn revert_keeps_text_in_any_encoding_byte_for_byte() {
	let mut rng = TestRng::new("tree revert encoding");
	// Latin-1: no zero byte, so text, but not UTF-8.
	let latin = b"caf\xe9 one\ntwo\nkept\nna\xefve four\n";
	let (mut library, tree, root, _) =
		with_bytes("tree revert encoding", &[("notes.txt", latin)], &mut rng);
	let personal = library.personal_space();
	fs::write(
		root.join("notes.txt"),
		b"caf\xe9 one\nTWO\nkept\nna\xefve four\n",
	)
	.expect("writes");
	let second = commit(&mut library, tree, now() + 2_000, &mut rng);
	fs::write(
		root.join("notes.txt"),
		b"caf\xe9 one\nTWO\nkept\nna\xefve FOUR\n",
	)
	.expect("writes");
	commit(&mut library, tree, now() + 3_000, &mut rng);
	let reverted = library
		.revert_in_tree(personal, tree, second)
		.expect("reverts");
	nibkeep_testkit::assert_empty!(reverted.conflicts);
	assert_eq!(
		fs::read(root.join("notes.txt")).expect("reads"),
		b"caf\xe9 one\ntwo\nkept\nna\xefve FOUR\n"
	);
}

#[test]
fn revert_never_writes_over_edits_not_committed() {
	let mut rng = TestRng::new("tree revert edited");
	let (library, tree, root, revisions) = with_history("tree revert edited", &mut rng);
	let personal = library.personal_space();
	write(&root, "notes.md", "one\nTWO\nthree\nFOUR\nmine\n");
	assert!(matches!(
		library.revert_in_tree(personal, tree, revisions[1]),
		Err(NodeError::EditedCopy(_))
	));
	assert_eq!(read(&root, "notes.md"), "one\nTWO\nthree\nFOUR\nmine\n");
}

#[cfg(unix)]
#[test]
fn revert_of_a_file_made_a_link_brings_the_file_back() {
	let mut rng = TestRng::new("tree revert link");
	let (mut library, tree, root, _) = with_bytes(
		"tree revert link",
		&[("x", b"a file\n"), ("target.txt", b"target\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	fs::remove_file(root.join("x")).expect("deletes");
	std::os::unix::fs::symlink("target.txt", root.join("x")).expect("links");
	let linked = commit(&mut library, tree, now() + 2_000, &mut rng);
	let reverted = library
		.revert_in_tree(personal, tree, linked)
		.expect("reverts");
	nibkeep_testkit::assert_empty!(reverted.conflicts);
	let metadata = fs::symlink_metadata(root.join("x")).expect("something at x");
	assert!(metadata.is_file(), "{reverted:?}");
	assert_eq!(read(&root, "x"), "a file\n");
}

#[cfg(unix)]
#[test]
fn revert_of_an_executable_bit_takes_it_back() {
	use std::os::unix::fs::PermissionsExt;
	let mut rng = TestRng::new("tree revert executable");
	let (mut library, tree, root, _) = with_bytes(
		"tree revert executable",
		&[("run.sh", b"#!/bin/sh\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");
	let chmod = commit(&mut library, tree, now() + 2_000, &mut rng);
	library
		.revert_in_tree(personal, tree, chmod)
		.expect("reverts");
	let mode = fs::metadata(root.join("run.sh"))
		.expect("reads")
		.permissions()
		.mode();
	assert_eq!(mode & 0o111, 0, "{mode:o}");
}

#[cfg(unix)]
#[test]
fn an_executable_bit_taken_away_shows_in_the_log_and_reverts() {
	use nibkeep_std::EXECUTABLE;
	use std::os::unix::fs::PermissionsExt;
	let mut rng = TestRng::new("tree log executable cleared");
	let (mut library, tree, root, _) = with_bytes(
		"tree log executable cleared",
		&[("run.sh", b"#!/bin/sh\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");
	commit(&mut library, tree, now() + 2_000, &mut rng);
	fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o644)).expect("chmod");
	let cleared = commit(&mut library, tree, now() + 3_000, &mut rng);
	// Taken away, the bit is stated false, rather than retracted with nothing in its place.
	let script = at_path(&library, tree, "run.sh");
	assert_eq!(
		common::values(&library, script, &EXECUTABLE),
		[Value::Boolean(false)]
	);
	let log = library.tree_log(personal, tree, None).expect("reads");
	assert_eq!(log[0].revision, Some(cleared));
	assert_eq!(
		changes(&log[0].changes),
		[("run.sh".to_owned(), Change::Executable(false))]
	);
	library
		.revert_in_tree(personal, tree, cleared)
		.expect("reverts");
	let mode = fs::metadata(root.join("run.sh"))
		.expect("reads")
		.permissions()
		.mode();
	assert_eq!(mode & 0o111, 0o111, "{mode:o}");
}

#[test]
fn what_revert_writes_beside_a_copy_is_shown_until_it_is_gone() {
	let mut rng = TestRng::new("tree revert beside");
	let (mut library, tree, root, _) =
		with_bytes("tree revert beside", &[("photo.bin", &[0, 1, 2])], &mut rng);
	let personal = library.personal_space();
	fs::write(root.join("photo.bin"), [0, 1, 2, 3]).expect("writes");
	let second = commit(&mut library, tree, now() + 2_000, &mut rng);
	fs::write(root.join("photo.bin"), [0, 1, 2, 3, 4]).expect("writes");
	commit(&mut library, tree, now() + 3_000, &mut rng);
	let reverted = library
		.revert_in_tree(personal, tree, second)
		.expect("reverts");
	assert_eq!(reverted.conflicts, ["photo.bin"]);
	let beside = format!("photo.bin.before-{}", &second.to_string()[..8]);
	assert_eq!(fs::read(root.join(&beside)).expect("reads"), [0, 1, 2]);
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	assert_eq!(status.skipped.len(), 1, "{:?}", status.skipped);
	fs::remove_file(root.join(&beside)).expect("deletes it");
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.skipped);
}

#[test]
fn edits_a_restore_keeps_never_take_a_path_of_the_tree() {
	let mut rng = TestRng::new("tree restore beside");
	let (library, tree, root, _) = with_bytes(
		"tree restore beside",
		&[
			(
				"docs/a.txt",
				b"a
",
			),
			(
				"docs/a.txt.orig",
				b"the tree's
",
			),
		],
		&mut rng,
	);
	let personal = library.personal_space();
	// The tree's own docs/a.txt.orig is not here, and docs/a.txt is edited.
	fs::remove_file(root.join("docs/a.txt.orig")).expect("deletes");
	fs::write(
		root.join("docs/a.txt"),
		"edited
",
	)
	.expect("writes");
	let restored = library
		.restore_in_tree(personal, tree, "docs", TreePoint::Now)
		.expect("restores");
	assert_eq!(
		restored.aside,
		[("docs/a.txt".to_owned(), "docs/a.txt.orig-2".to_owned())]
	);
	assert_eq!(
		read(&root, "docs/a.txt"),
		"a
"
	);
	assert_eq!(
		read(&root, "docs/a.txt.orig"),
		"the tree's
"
	);
	assert_eq!(
		read(&root, "docs/a.txt.orig-2"),
		"edited
"
	);
}

#[test]
fn restore_brings_back_the_file_deleted_last_at_a_path() {
	let mut rng = TestRng::new("tree restore latest");
	let (mut library, tree, root, _) = with_bytes(
		"tree restore latest",
		&[("notes.txt", b"first\n"), ("keep.txt", b"k")],
		&mut rng,
	);
	let personal = library.personal_space();
	fs::remove_file(root.join("notes.txt")).expect("deletes");
	commit(&mut library, tree, now() + 2_000, &mut rng);
	fs::write(root.join("notes.txt"), "second\n").expect("writes");
	commit(&mut library, tree, now() + 3_000, &mut rng);
	fs::remove_file(root.join("notes.txt")).expect("deletes");
	commit(&mut library, tree, now() + 4_000, &mut rng);
	library
		.restore_in_tree(personal, tree, "notes.txt", TreePoint::Now)
		.expect("restores");
	assert_eq!(read(&root, "notes.txt"), "second\n");
}

#[test]
fn what_is_sealed_for_the_secrets_is_never_written_in_plain() {
	let mut rng = TestRng::new("tree secret content");
	let (mut library, tree, root, _) = with_bytes(
		"tree secret content",
		&[("a.txt", b"a"), ("b.txt", b"b")],
		&mut rng,
	);
	let personal = library.personal_space();
	library
		.set_up_secrets(
			&nibkeep_node::NewProtection::Passphrase(
				"tr0ub4dor",
				nibkeep_node::PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 2_000,
			&mut rng,
		)
		.expect("sets up the secrets");
	let secret = library
		.add_secret_file(
			personal,
			"passport.pdf",
			b"s3ntinel".as_slice(),
			&[],
			now() + 3_000,
			&mut rng,
		)
		.expect("adds a secret file");
	// As a hostile replica could, a file of the tree names the secret content.
	let a = at_path(&library, tree, "a.txt");
	library
		.edit_entity(
			personal,
			a,
			vec![Edit::Set(
				nibkeep_std::CONTENT,
				vec![Value::Blob(secret.content)],
			)],
			&today(),
			now() + 4_000,
			&mut rng,
		)
		.expect("names it");
	fs::remove_file(root.join("a.txt")).expect("deletes the copy");
	let restored = library
		.restore_in_tree(personal, tree, "a.txt", TreePoint::Now)
		.expect("restores what it can");
	nibkeep_testkit::assert_empty!(restored.written);
	assert!(!root.join("a.txt").exists());
}

#[test]
fn a_file_whose_tree_changed_is_the_other_tree_s_alone() {
	let mut rng = TestRng::new("tree changed tree");
	let (mut library, tree, _root, _) = with_bytes(
		"tree changed tree",
		&[("a.txt", b"a"), ("b.txt", b"b")],
		&mut rng,
	);
	let personal = library.personal_space();
	let other = library
		.create_entity(
			personal,
			&[nibkeep_std::TREE],
			vec![],
			now() + 2_000,
			&mut rng,
		)
		.expect("makes another tree");
	let a = at_path(&library, tree, "a.txt");
	library
		.edit_entity(
			personal,
			a,
			vec![Edit::Set(
				nibkeep_std::IN_TREE,
				vec![Value::Reference(other)],
			)],
			&today(),
			now() + 3_000,
			&mut rng,
		)
		.expect("moves it to the other tree");
	let paths: Vec<String> = library
		.tree_entries(personal, tree)
		.expect("reads")
		.into_iter()
		.map(|entry| entry.path)
		.collect();
	assert_eq!(paths, ["b.txt"]);
	assert!(matches!(
		library.write_tree(
			personal,
			a,
			&new_folder("tree changed tree, not a tree"),
			TreePoint::Now,
			now() + 4_000,
			&mut rng
		),
		Err(NodeError::NotATree(_))
	));
}

#[test]
fn nibkeep_own_leftovers_are_left_out_and_names_like_them_are_not() {
	let mut rng = TestRng::new("tree leftovers");
	let (mut library, tree, root, _) = with_bytes(
		"tree leftovers",
		&[(".nib-settings.json", b"{}"), ("a.txt", b"a")],
		&mut rng,
	);
	let personal = library.personal_space();
	assert_eq!(
		library.tree_entries(personal, tree).expect("reads").len(),
		2
	);
	fs::write(
		root.join(".nib-tree-0123456789abcdef"),
		"a crash's leftover",
	)
	.expect("writes");
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	let _ = &mut library;
}

#[test]
fn revert_refuses_a_copy_behind_the_tree() {
	let mut rng = TestRng::new("tree revert behind");
	let (mut library, tree, _root, revisions) = with_history("tree revert behind", &mut rng);
	let personal = library.personal_space();
	// Elsewhere, as by another device, notes.md gets a newer version.
	let notes = at_path(&library, tree, "notes.md");
	let kept = at_path(&library, tree, "kept/keep.txt");
	let newer = library
		.entity(personal, kept, common::this_day())
		.expect("reads")
		.expect("an entity")
		.values(&nibkeep_std::CONTENT)[0]
		.value
		.clone();
	library
		.edit_entity(
			personal,
			notes,
			vec![Edit::Set(nibkeep_std::CONTENT, vec![newer])],
			&today(),
			now() + 4_000,
			&mut rng,
		)
		.expect("gives it a newer version");
	assert!(matches!(
		library.revert_in_tree(personal, tree, revisions[1]),
		Err(NodeError::TreeStale(paths)) if paths == ["notes.md"]
	));
}

#[test]
fn revert_reports_what_it_cannot_undo() {
	let mut rng = TestRng::new("tree revert reports");
	let (library, tree, root, revisions) = with_history("tree revert reports", &mut rng);
	let personal = library.personal_space();
	fs::remove_file(root.join("notes.md")).expect("deletes the copy");
	let reverted = library
		.revert_in_tree(personal, tree, revisions[1])
		.expect("reverts");
	let skipped: Vec<&str> = reverted
		.skipped
		.iter()
		.map(|(path, _)| path.as_str())
		.collect();
	assert_eq!(skipped, ["notes.md"]);
}

#[cfg(unix)]
#[test]
fn revert_keeps_an_executable_bit_that_a_later_revision_set_and_refuses_one_not_committed() {
	use std::os::unix::fs::PermissionsExt;
	let mut rng = TestRng::new("tree revert later bit");
	let (mut library, tree, root, _) =
		with_bytes("tree revert later bit", &[("run.sh", b"one\n")], &mut rng);
	let personal = library.personal_space();
	fs::write(root.join("run.sh"), "two\n").expect("writes");
	let edit = commit(&mut library, tree, now() + 2_000, &mut rng);
	let executable = fs::Permissions::from_mode(0o755);
	fs::set_permissions(root.join("run.sh"), executable).expect("chmod");
	commit(&mut library, tree, now() + 3_000, &mut rng);
	library
		.revert_in_tree(personal, tree, edit)
		.expect("reverts");
	assert_eq!(read(&root, "run.sh"), "one\n");
	let mode = fs::metadata(root.join("run.sh"))
		.expect("reads")
		.permissions()
		.mode();
	assert_ne!(mode & 0o100, 0, "the later bit stays: {mode:o}");
	// A bit not committed is an edit, which a restore keeps beside the copy, with its bit.
	commit(&mut library, tree, now() + 4_000, &mut rng);
	fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o644)).expect("chmod");
	let restored = library
		.restore_in_tree(personal, tree, "run.sh", TreePoint::Revision(edit))
		.expect("restores");
	assert_eq!(
		restored.aside,
		[("run.sh".to_owned(), "run.sh.orig".to_owned())]
	);
	assert_eq!(read(&root, "run.sh"), "two\n");
	let mode = fs::metadata(root.join("run.sh.orig"))
		.expect("reads")
		.permissions()
		.mode();
	assert_eq!(mode & 0o100, 0, "the edit's bit stays with it: {mode:o}");
}

#[cfg(unix)]
#[test]
fn a_file_brought_back_over_a_link_is_that_file_again() {
	let mut rng = TestRng::new("tree revert revives");
	let (mut library, tree, root, _) = with_bytes(
		"tree revert revives",
		&[("x", b"a file\n"), ("target.txt", b"target\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let file = at_path(&library, tree, "x");
	fs::remove_file(root.join("x")).expect("deletes");
	std::os::unix::fs::symlink("target.txt", root.join("x")).expect("links");
	let linked = commit(&mut library, tree, now() + 2_000, &mut rng);
	library
		.revert_in_tree(personal, tree, linked)
		.expect("reverts");
	commit(&mut library, tree, now() + 3_000, &mut rng);
	assert_eq!(at_path(&library, tree, "x"), file);
}

#[test]
fn revert_undoes_a_swap_of_two_files() {
	let mut rng = TestRng::new("tree revert swap");
	let (mut library, tree, root, _) = with_bytes(
		"tree revert swap",
		&[("a.txt", b"A\n"), ("b.txt", b"B\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	fs::rename(root.join("a.txt"), root.join("c.txt")).expect("moves");
	fs::rename(root.join("b.txt"), root.join("a.txt")).expect("moves");
	fs::rename(root.join("c.txt"), root.join("b.txt")).expect("moves");
	let swapped = commit(&mut library, tree, now() + 2_000, &mut rng);
	assert_eq!(read(&root, "a.txt"), "B\n");
	let reverted = library
		.revert_in_tree(personal, tree, swapped)
		.expect("reverts");
	nibkeep_testkit::assert_empty!(reverted.skipped);
	assert_eq!(read(&root, "a.txt"), "A\n");
	assert_eq!(read(&root, "b.txt"), "B\n");
}

#[test]
fn nothing_is_made_where_a_tree_may_not_be_written() {
	let mut rng = TestRng::new("tree write nowhere");
	let (mut library, tree, _root, revisions) = with_history("tree write nowhere", &mut rng);
	let personal = library.personal_space();
	let repository = folder("tree write nowhere, repository");
	fs::create_dir(repository.join(".git")).expect("makes a repository");
	let inside = repository.join(".git/x/y");
	assert!(matches!(
		library.write_tree(
			personal,
			tree,
			&inside,
			TreePoint::Revision(revisions[0]),
			now() + 4_000,
			&mut rng
		),
		Err(NodeError::NotATreeFolder(_))
	));
	assert!(!repository.join(".git/x").exists());
}
