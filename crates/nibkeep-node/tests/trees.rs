//! Trees: folders whose files NibKeep tracks by their paths, with every version, as Git
//! tracks a working tree; commits of many files in one change, each with a revision; and
//! each replica's index of its copy ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::common::{library, now, this_day, values};
use nibkeep_base::EntityId;
use nibkeep_model::{Value, fields};
use nibkeep_node::{
	Change, Commit, Edit, Library, NodeError, TreeChange, TreePoint, resolved_path,
};
use nibkeep_std::{COMMITTED_IN, CONTENT, FILE, IN_TREE, MESSAGE, PARTS, REVISION, TREE_PATH};
use nibkeep_testkit::TestRng;

/// A folder of its own, as the file system resolves it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	resolved_path(&made).expect("resolves")
}

/// Writes a file, with its folders, and makes it look written an hour ago, so that what
/// reads it again only because it was written a moment ago does not.
fn write(root: &Path, path: &str, content: &str) -> PathBuf {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	age(&path);
	path
}

/// Makes a file look written an hour ago.
fn age(path: &Path) {
	let hour_ago = SystemTime::now() - Duration::from_secs(3_600);
	fs::File::options()
		.write(true)
		.open(path)
		.and_then(|file| file.set_modified(hour_ago))
		.expect("ages");
}

/// The types of an entity, as the library shows them.
fn types(library: &Library, entity: EntityId) -> Vec<nibkeep_model::TypeName> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity")
		.types
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// The changes, as `(path, change)`, for comparing.
fn changes(changes: &[TreeChange]) -> Vec<(String, Change)> {
	changes
		.iter()
		.map(|change| (change.path.clone(), change.change.clone()))
		.collect()
}

/// A library with a tree started in a folder of its own, which holds `files`.
fn with_tree(
	name: &str,
	files: &[(&str, &str)],
	rng: &mut TestRng,
) -> (Library, EntityId, PathBuf) {
	let (_, mut library) = library(name, rng);
	let root = folder(&format!("{name}, tree"));
	for (path, content) in files {
		write(&root, path, content);
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, Some("Start"), now() + 1_000, rng)
		.expect("starts");
	(library, started.tree, root)
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

#[test]
fn starting_a_tree_commits_its_files_in_parts_after_one_with_its_revision() {
	let mut rng = TestRng::new("tree started");
	let (_, mut library) = library("tree started", &mut rng);
	let root = folder("tree started, notes");
	write(&root, "notes.md", "# Notes\n");
	write(&root, "scripts/backup.sh", "#!/bin/sh\n");
	write(&root, ".github/workflows/ci.yml", "on: push\n");
	write(&root, ".env.example", "TOKEN=\n");
	#[cfg(unix)]
	{
		use std::os::unix::fs::{PermissionsExt, symlink};
		fs::set_permissions(
			root.join("scripts/backup.sh"),
			fs::Permissions::from_mode(0o755),
		)
		.expect("makes it executable");
		symlink("notes.md", root.join("latest.md")).expect("links");
	}
	let personal = library.personal_space();
	let preview = library
		.would_start_tree(personal, &root)
		.expect("would start");
	assert_eq!(preview.files, if cfg!(unix) { 5 } else { 4 });
	assert_eq!(preview.bytes, 8 + 10 + 9 + 7);
	let changes_before = library.status().expect("status").spaces[0].changes;
	let started = library
		.start_tree(personal, &root, Some("Start"), now() + 1_000, &mut rng)
		.expect("starts");
	assert_eq!(started.root, root);
	let mut expected = vec![
		(".env.example".to_owned(), Change::New),
		(".github/workflows/ci.yml".to_owned(), Change::New),
		("notes.md".to_owned(), Change::New),
		("scripts/backup.sh".to_owned(), Change::New),
	];
	if cfg!(unix) {
		expected.insert(2, ("latest.md".to_owned(), Change::New));
	}
	assert_eq!(changes(&started.committed.changes), expected);
	// The revision, the tree, then a part for each file.
	assert_eq!(started.committed.parts, 2 + expected.len());

	// The tree is named after its folder, and its root says where it is on this device.
	let trees = library.trees(personal).expect("lists");
	assert_eq!(trees.len(), 1);
	assert_eq!(trees[0].id, started.tree);
	assert_eq!(
		trees[0].name.as_deref(),
		root.file_name().and_then(|name| name.to_str())
	);
	assert_eq!(trees[0].root.as_deref(), Some(root.as_path()));
	let found = library
		.tree_at(personal, &root.join("scripts"))
		.expect("looks")
		.expect("finds the tree from a folder inside it");
	assert_eq!(found.id, started.tree);
	assert!(
		library
			.tree_at(personal, root.parent().expect("a parent"))
			.expect("looks")
			.is_none()
	);

	// Each file is a file of its own, with its path, and its revision names the tree and
	// holds the message.
	let notes = at_path(&library, started.tree, "notes.md");
	assert_eq!(types(&library, notes), [FILE]);
	assert_eq!(values(&library, notes, &TREE_PATH), [text("notes.md")]);
	assert_eq!(
		values(&library, notes, &IN_TREE),
		[Value::Reference(started.tree)]
	);
	assert_eq!(
		values(&library, notes, &fields::NAME),
		[Value::Name(nibkeep_model::Name::new("notes.md"))]
	);
	let revision = started.committed.revision.expect("a revision");
	assert_eq!(types(&library, revision), [REVISION]);
	assert_eq!(values(&library, revision, &MESSAGE), [text("Start")]);
	assert_eq!(
		values(&library, revision, &IN_TREE),
		[Value::Reference(started.tree)]
	);
	#[cfg(unix)]
	{
		use nibkeep_std::{EXECUTABLE, LINK_TARGET, SYMBOLIC_LINK};
		let script = at_path(&library, started.tree, "scripts/backup.sh");
		assert_eq!(
			values(&library, script, &EXECUTABLE),
			[Value::Boolean(true)]
		);
		assert_eq!(values(&library, notes, &EXECUTABLE), []);
		let link = at_path(&library, started.tree, "latest.md");
		assert_eq!(types(&library, link), [SYMBOLIC_LINK]);
		assert_eq!(values(&library, link, &LINK_TARGET), [text("notes.md")]);
		assert_eq!(values(&library, link, &CONTENT), []);
	}
	// A change for each part of the first commit, as the space has the types already, and
	// one that records the tree's folder on this device, which is no part of it.
	let changes_after = library.status().expect("status").spaces[0].changes;
	assert_eq!(
		changes_after - changes_before,
		started.committed.parts + 1,
		"{changes_before} → {changes_after}"
	);
	let status = library.tree_status(personal, started.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
}

#[test]
fn a_tree_is_never_inside_another_and_never_starts_in_a_git_repository() {
	let mut rng = TestRng::new("trees apart");
	let (mut library, _, root) = with_tree("trees apart", &[("a.txt", "a")], &mut rng);
	let personal = library.personal_space();
	let inside = root.join("inside");
	fs::create_dir(&inside).expect("makes a folder");
	assert!(matches!(
		library.start_tree(personal, &inside, None, now() + 2_000, &mut rng),
		Err(NodeError::TreeInTree(_))
	));
	assert!(matches!(
		library.would_start_tree(personal, root.parent().expect("a parent")),
		Err(NodeError::TreeInTree(_))
	));
	let repository = folder("trees apart, repository");
	fs::create_dir(repository.join(".git")).expect("makes a repository");
	assert!(matches!(
		library.would_start_tree(personal, &repository),
		Err(NodeError::GitRepositoryHere(_))
	));
}

#[test]
fn a_commit_records_new_files_versions_moves_and_deletions_in_a_part_each() {
	let mut rng = TestRng::new("tree commit");
	let files = [
		("a.txt", "Alpha\n"),
		("b.txt", "Beta\n"),
		("c.txt", "Gamma\n"),
		("docs/d.txt", "Delta\n"),
	];
	let (mut library, tree, root) = with_tree("tree commit", &files, &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	let b = at_path(&library, tree, "b.txt");
	let d = at_path(&library, tree, "docs/d.txt");

	write(&root, "a.txt", "Alpha, again\n");
	fs::remove_file(root.join("c.txt")).expect("deletes");
	fs::rename(root.join("b.txt"), root.join("docs/b.txt")).expect("moves");
	write(&root, "e.txt", "Epsilon\n");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			("a.txt".to_owned(), Change::Edited),
			("c.txt".to_owned(), Change::Deleted),
			(
				"docs/b.txt".to_owned(),
				Change::Moved {
					from: "b.txt".to_owned()
				}
			),
			("e.txt".to_owned(), Change::New),
		]
	);
	let changes_before = library.status().expect("status").spaces[0].changes;
	let committed = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Edit, move, delete")),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.changes, status.changes);
	assert_eq!(committed.parts, 5);
	assert_eq!(
		library.status().expect("status").spaces[0].changes,
		changes_before + 5,
		"a part for each, after one with the revision"
	);

	// A new version keeps the file; a move keeps it too, with its old path in history.
	assert_eq!(at_path(&library, tree, "a.txt"), a);
	assert_eq!(at_path(&library, tree, "docs/b.txt"), b);
	assert_eq!(at_path(&library, tree, "docs/d.txt"), d);
	let history = library
		.history(personal, b, Some(&TREE_PATH))
		.expect("reads")
		.expect("a history");
	let paths: Vec<&Value> = history.fields[0]
		.values
		.iter()
		.map(|past| &past.value)
		.collect();
	assert_eq!(paths, [&text("b.txt"), &text("docs/b.txt")]);
	let versions = library
		.history(personal, a, Some(&CONTENT))
		.expect("reads")
		.expect("a history");
	assert_eq!(versions.fields[0].values.len(), 2);
	let entries: Vec<String> = library
		.tree_entries(personal, tree)
		.expect("reads")
		.into_iter()
		.map(|entry| entry.path)
		.collect();
	assert_eq!(entries, ["a.txt", "docs/b.txt", "docs/d.txt", "e.txt"]);
	let deleted = library.deleted(personal).expect("lists");
	assert_eq!(deleted.len(), 1, "c.txt, which can be restored");
	nibkeep_testkit::assert_empty!(library.tree_status(personal, tree).expect("status").changes);
	// Nothing to commit commits nothing.
	let nothing = library
		.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng)
		.expect("commits nothing");
	assert_eq!(nothing.revision, None);
	assert_eq!(nothing.parts, 0);
}

#[test]
fn two_files_with_one_content_stay_two_files_and_adding_it_elsewhere_takes_neither() {
	let mut rng = TestRng::new("tree twins");
	let files = [("pkg/__init__.py", ""), ("pkg/sub/__init__.py", "")];
	let (mut library, tree, _) = with_tree("tree twins", &files, &mut rng);
	let personal = library.personal_space();
	let first = at_path(&library, tree, "pkg/__init__.py");
	let second = at_path(&library, tree, "pkg/sub/__init__.py");
	assert_ne!(first, second);
	assert_eq!(
		values(&library, first, &CONTENT),
		values(&library, second, &CONTENT)
	);
	let elsewhere = folder("tree twins, elsewhere");
	let empty = write(&elsewhere, "empty.txt", "");
	let added = library
		.add_file(personal, &empty, now() + 2_000, &mut rng)
		.expect("adds");
	assert!(!added.known, "a new file, not one of the tree's");
	assert_ne!(added.file, first);
	assert_ne!(added.file, second);
}

#[test]
fn files_outside_trees_never_reach_into_them() {
	let mut rng = TestRng::new("tree apart from files");
	let (mut library, tree, root) = with_tree(
		"tree apart from files",
		&[("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	// nib file add leaves the folders of trees to their commits.
	assert!(matches!(
		library.add_file(personal, &root.join("notes.md"), now() + 2_000, &mut rng),
		Err(NodeError::InTree(_))
	));
	// And no checkout writes a tree's file, which the tree's own commands write.
	let notes = at_path(&library, tree, "notes.md");
	let out = folder("tree apart from files, out");
	assert!(matches!(
		library.checkout(personal, notes, &out, now() + 3_000, &mut rng),
		Err(NodeError::TreeFile(_))
	));
}

#[test]
fn every_version_is_sealed_into_the_store() {
	let mut rng = TestRng::new("tree store");
	let (mut library, tree, root) = with_tree("tree store", &[("a.txt", "First\n")], &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	let Value::Blob(first) = values(&library, a, &CONTENT)[0] else {
		panic!("a blob");
	};
	write(&root, "a.txt", "Second\n");
	library
		.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng)
		.expect("commits");
	fs::remove_file(root.join("a.txt")).expect("deletes the copy");
	assert_eq!(
		library
			.file_content(personal, &first, 1_000)
			.expect("reads"),
		Some(b"First\n".to_vec())
	);
	let Value::Blob(second) = values(&library, a, &CONTENT)[0] else {
		panic!("a blob");
	};
	assert_eq!(
		library
			.file_content(personal, &second, 1_000)
			.expect("reads"),
		Some(b"Second\n".to_vec())
	);
}

#[test]
fn status_reads_only_what_changed_and_finds_the_same_once_its_index_is_lost() {
	let mut rng = TestRng::new("tree index");
	let files: Vec<(String, String)> = (0..20)
		.map(|n| (format!("file-{n:02}.txt"), format!("Content {n}\n")))
		.collect();
	let borrowed: Vec<(&str, &str)> = files
		.iter()
		.map(|(path, content)| (path.as_str(), content.as_str()))
		.collect();
	let (library, tree, root) = with_tree("tree index", &borrowed, &mut rng);
	let personal = library.personal_space();
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.read, 0, "nothing changed since the commit");
	nibkeep_testkit::assert_empty!(status.changes);

	write(&root, "file-03.txt", "Content three, longer\n");
	write(&root, "file-07.txt", "Content 7?\n");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.read, 2, "only the files whose size or time changed");
	let expected = [
		("file-03.txt".to_owned(), Change::Edited),
		("file-07.txt".to_owned(), Change::Edited),
	];
	assert_eq!(changes(&status.changes), expected);

	// A file whose time alone changed is read, and found unchanged.
	write(&root, "file-11.txt", "Content 11\n");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.read, 3);
	assert_eq!(changes(&status.changes), expected);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.read, 2, "what was found unchanged is not read again");

	// The index is derived: without it, every file the tree holds is read. Unchanged ones
	// are found unchanged, a new one new, and an edited one edited from a version this
	// replica does not know.
	let mut library = library;
	library
		.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng)
		.expect("commits");
	write(&root, "file-05.txt", "Content 5, edited\n");
	write(&root, "new.txt", "New\n");
	let replica = library.close();
	let index = replica.directory().join("trees.sqlite");
	assert!(index.exists(), "the index is in the replica's folder");
	fs::remove_file(&index).expect("loses the index");
	let library = replica.open().expect("opens");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.read, 20, "every file the tree holds, and no new one");
	let found = [
		("file-05.txt".to_owned(), Change::Unknown),
		("new.txt".to_owned(), Change::New),
	];
	assert_eq!(changes(&status.changes), found);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		status.read, 1,
		"what was found unchanged is in the index again"
	);
	assert_eq!(changes(&status.changes), found);
}

#[test]
fn a_copy_that_matches_no_version_is_committed_only_with_force() {
	let mut rng = TestRng::new("tree unknown");
	let (library, tree, root) = with_tree("tree unknown", &[("a.txt", "First\n")], &mut rng);
	let personal = library.personal_space();
	let replica = library.close();
	fs::remove_file(replica.directory().join("trees.sqlite")).expect("loses the index");
	let mut library = replica.open().expect("opens");
	write(&root, "a.txt", "Edited from what?\n");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[("a.txt".to_owned(), Change::Unknown)]
	);
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng),
		Err(NodeError::UnknownVersion(paths)) if paths == ["a.txt"]
	));
	library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_force(),
			now() + 3_000,
			&mut rng,
		)
		.expect("commits with force");
	nibkeep_testkit::assert_empty!(library.tree_status(personal, tree).expect("status").changes);
}

#[test]
fn edits_of_a_file_that_changed_elsewhere_are_committed_only_with_force() {
	let mut rng = TestRng::new("tree stale");
	let (mut library, tree, root) = with_tree("tree stale", &[("a.txt", "First\n")], &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	// Elsewhere, as by another device, the file moves.
	library
		.edit_entity(
			personal,
			a,
			vec![Edit::Set(TREE_PATH, vec![text("b.txt")])],
			&common::today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("moves it");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.behind, ["a.txt", "b.txt"]);
	write(&root, "a.txt", "Second\n");
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng),
		Err(NodeError::TreeStale(paths)) if paths == ["a.txt"]
	));
	library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_force(),
			now() + 4_000,
			&mut rng,
		)
		.expect("commits on top");
}

#[test]
fn a_commit_of_some_paths_leaves_the_rest() {
	let mut rng = TestRng::new("tree some paths");
	let files = [("a.txt", "a"), ("docs/b.txt", "b"), ("docs/c.txt", "c")];
	let (mut library, tree, root) = with_tree("tree some paths", &files, &mut rng);
	let personal = library.personal_space();
	write(&root, "a.txt", "aa");
	write(&root, "docs/b.txt", "bb");
	write(&root, "docs/c.txt", "cc");
	let committed = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_paths(vec!["docs".to_owned()]),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(
		changes(&committed.changes),
		[
			("docs/b.txt".to_owned(), Change::Edited),
			("docs/c.txt".to_owned(), Change::Edited),
		]
	);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[("a.txt".to_owned(), Change::Edited)]
	);
}

#[test]
fn a_commit_takes_a_part_for_each_file_after_one_with_its_revision() {
	let mut rng = TestRng::new("tree parts");
	let (_, mut library) = library("tree parts", &mut rng);
	let root = folder("tree parts, files");
	for n in 0..60 {
		write(&root, &format!("file-{n:02}.txt"), &format!("{n}\n"));
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts");
	for n in 0..60 {
		write(&root, &format!("file-{n:02}.txt"), &format!("{n} again\n"));
	}
	let committed = library
		.commit_tree(
			personal,
			started.tree,
			&Commit::new(Some("Everything")),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(
		committed.parts, 61,
		"the revision, then a part for each file"
	);
	assert_eq!(committed.changes.len(), 60);
	// One revision for the commit, with its message and how many parts it has.
	let revision = committed.revision.expect("a revision");
	assert_eq!(values(&library, revision, &PARTS), [Value::Integer(61)]);
	assert_eq!(values(&library, revision, &MESSAGE), [text("Everything")]);
	// Each part names it on the file it records, and history keeps the commit before,
	// which named its own on the tree too.
	let started_as = started.committed.revision.expect("a revision");
	assert_eq!(
		values(&library, started_as, &PARTS),
		[Value::Integer(62)],
		"the revision, the tree, and a part for each file"
	);
	assert_eq!(
		values(&library, started.tree, &COMMITTED_IN),
		[Value::Reference(started_as)]
	);
	for n in 0..60 {
		let file = at_path(&library, started.tree, &format!("file-{n:02}.txt"));
		assert_eq!(
			values(&library, file, &COMMITTED_IN),
			[Value::Reference(revision)]
		);
		let history = library
			.history(personal, file, Some(&COMMITTED_IN))
			.expect("reads")
			.expect("a history");
		let commits: Vec<&Value> = history.fields[0]
			.values
			.iter()
			.map(|past| &past.value)
			.collect();
		assert_eq!(
			commits,
			[&Value::Reference(started_as), &Value::Reference(revision)]
		);
	}
	let status = library.tree_status(personal, started.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.cut_short);
}

#[test]
fn moving_in_a_tree_moves_the_copy_and_the_file_at_once() {
	let mut rng = TestRng::new("tree move");
	let files = [("a.txt", "a"), ("docs/b.txt", "b"), ("docs/c.txt", "c")];
	let (mut library, tree, root) = with_tree("tree move", &files, &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	let b = at_path(&library, tree, "docs/b.txt");
	let moved = library
		.move_in_tree(
			personal,
			tree,
			("a.txt", "notes/a.md"),
			now() + 2_000,
			&mut rng,
		)
		.expect("moves");
	assert_eq!(
		changes(&moved.changes),
		[(
			"notes/a.md".to_owned(),
			Change::Moved {
				from: "a.txt".to_owned()
			}
		)]
	);
	assert!(!root.join("a.txt").exists());
	assert_eq!(
		fs::read_to_string(root.join("notes/a.md")).expect("reads"),
		"a"
	);
	assert_eq!(at_path(&library, tree, "notes/a.md"), a);
	assert_eq!(
		values(&library, a, &fields::NAME),
		[Value::Name(nibkeep_model::Name::new("a.md"))]
	);
	// A folder moves with every file in it.
	library
		.move_in_tree(personal, tree, ("docs", "papers"), now() + 3_000, &mut rng)
		.expect("moves a folder");
	assert_eq!(at_path(&library, tree, "papers/b.txt"), b);
	assert!(root.join("papers/c.txt").exists());
	assert!(!root.join("docs").exists());
	nibkeep_testkit::assert_empty!(library.tree_status(personal, tree).expect("status").changes);
	// Nothing is moved onto what is there, nor out of the tree, nor into .nib or a .git.
	assert!(matches!(
		library.move_in_tree(
			personal,
			tree,
			("papers/b.txt", "papers/c.txt"),
			now() + 4_000,
			&mut rng
		),
		Err(NodeError::PlaceTaken(_))
	));
	for unsafe_path in [
		"../b.txt",
		".nib/b.txt",
		".GIT/b.txt",
		"x/.git/b.txt",
		"/b.txt",
	] {
		assert!(
			matches!(
				library.move_in_tree(
					personal,
					tree,
					("papers/b.txt", unsafe_path),
					now() + 5_000,
					&mut rng
				),
				Err(NodeError::UnsafePath(_))
			),
			"{unsafe_path}"
		);
	}
	assert!(matches!(
		library.move_in_tree(
			personal,
			tree,
			("missing.txt", "here.txt"),
			now() + 6_000,
			&mut rng
		),
		Err(NodeError::NotInTree(_))
	));
}

#[test]
fn removing_deletes_unedited_copies_for_the_next_commit_to_record() {
	let mut rng = TestRng::new("tree remove");
	let files = [
		("a.txt", "a"),
		("docs/b.txt", "b"),
		("docs/deep/c.txt", "c"),
		("keep.txt", "k"),
		("stays.txt", "s"),
	];
	let (mut library, tree, root) = with_tree("tree remove", &files, &mut rng);
	let personal = library.personal_space();
	let changes_before = library.status().expect("status").spaces[0].changes;
	let removed = library
		.remove_in_tree(personal, tree, &["a.txt".to_owned()], false)
		.expect("removes");
	assert_eq!(removed.removed, ["a.txt"]);
	assert!(!root.join("a.txt").exists());
	// A folder goes with the tree's files in it, and the folders they leave empty; a file
	// the tree does not hold stays, and so does its folder.
	write(&root, "docs/new.txt", "not the tree's");
	let removed = library
		.remove_in_tree(personal, tree, &["docs".to_owned()], false)
		.expect("removes");
	assert_eq!(removed.removed, ["docs/b.txt", "docs/deep/c.txt"]);
	assert!(!root.join("docs/deep").exists());
	assert!(root.join("docs/new.txt").exists());
	assert_eq!(
		library.status().expect("status").spaces[0].changes,
		changes_before,
		"nothing recorded"
	);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			("a.txt".to_owned(), Change::Deleted),
			("docs/b.txt".to_owned(), Change::Deleted),
			("docs/deep/c.txt".to_owned(), Change::Deleted),
			("docs/new.txt".to_owned(), Change::New),
		]
	);
	// An edited copy stays, unless forced, as Git's rm keeps one, and so does every other
	// copy named with it.
	write(&root, "keep.txt", "edited");
	assert!(matches!(
		library.remove_in_tree(
			personal,
			tree,
			&["stays.txt".to_owned(), "keep.txt".to_owned()],
			false
		),
		Err(NodeError::EditedCopy(_))
	));
	assert!(root.join("keep.txt").exists());
	assert!(root.join("stays.txt").exists());
	let removed = library
		.remove_in_tree(personal, tree, &["keep.txt".to_owned()], true)
		.expect("removes");
	assert_eq!(removed.removed, ["keep.txt"]);
	assert!(!root.join("keep.txt").exists());
	// What the tree does not hold is not removed.
	assert!(matches!(
		library.remove_in_tree(personal, tree, &["docs/new.txt".to_owned()], false),
		Err(NodeError::NotInTree(_))
	));
	assert!(matches!(
		library.remove_in_tree(personal, tree, &["../a.txt".to_owned()], false),
		Err(NodeError::UnsafePath(_))
	));
	// The next commit records the deletions.
	let committed = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(Some("Removed")).with_paths(vec![
				"a.txt".to_owned(),
				"docs/b.txt".to_owned(),
				"docs/deep/c.txt".to_owned(),
				"keep.txt".to_owned(),
			]),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.changes.len(), 4);
	assert!(
		committed
			.changes
			.iter()
			.all(|change| change.change == Change::Deleted)
	);
}

#[cfg(unix)]
#[test]
fn a_move_never_goes_through_a_link() {
	let mut rng = TestRng::new("tree move through link");
	let (mut library, tree, root) =
		with_tree("tree move through link", &[("a.txt", "a")], &mut rng);
	let personal = library.personal_space();
	let outside = folder("tree move through link, outside");
	std::os::unix::fs::symlink(&outside, root.join("out")).expect("links");
	assert!(matches!(
		library.move_in_tree(
			personal,
			tree,
			("a.txt", "out/a.txt"),
			now() + 2_000,
			&mut rng
		),
		Err(NodeError::UnsafePath(_))
	));
	assert!(root.join("a.txt").exists());
	assert!(fs::read_dir(&outside).expect("reads").next().is_none());
}

#[test]
fn commits_leave_out_the_library_git_repositories_and_what_nibignore_names() {
	let mut rng = TestRng::new("tree left out");
	let (_, mut library) = library("tree left out", &mut rng);
	let root = folder("tree left out, files");
	write(&root, "notes.md", "# Notes\n");
	write(&root, ".nib/keys.cbor", "keys");
	write(&root, "project/.git/HEAD", "ref: refs/heads/main\n");
	write(&root, "project/src/main.rs", "fn main() {}\n");
	write(&root, "work/.git", "gitdir: ../somewhere\n");
	write(&root, "work/file.txt", "a worktree's\n");
	write(&root, ".nibignore", "target/\n*.log\n");
	write(&root, "target/debug/app", "binary");
	write(&root, "run.log", "log");
	write(&root, "docs/.nibignore", "!keep.log\n");
	write(&root, "docs/keep.log", "kept");
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, None, now() + 1_000, &mut rng)
		.expect("starts");
	let paths: Vec<String> = started
		.committed
		.changes
		.iter()
		.map(|change| change.path.clone())
		.collect();
	assert_eq!(
		paths,
		[".nibignore", "docs/.nibignore", "docs/keep.log", "notes.md"]
	);
}

#[test]
fn nibignore_leaves_out_what_git_would_and_nothing_else() {
	let Ok(version) = std::process::Command::new("git").arg("--version").output() else {
		eprintln!("Git is not installed: skipped");
		return;
	};
	assert!(version.status.success());
	let rules = [
		(
			"",
			"*.log\n!keep.log\nbuild/\n/root-only.txt\ndocs/**/*.tmp\n**/cache\na?c.txt\n[ab]*.bak\n\\#hash.txt\nspaced\\ \n",
		),
		("sub/", "*.md\n!README.md\n/local.txt\n"),
	];
	let files = [
		"keep.log",
		"other.log",
		"build/out.o",
		"src/build/out.o",
		"root-only.txt",
		"src/root-only.txt",
		"docs/a/b/c.tmp",
		"docs/c.tmp",
		"other/c.tmp",
		"x/cache/file",
		"cache/file",
		"abc.txt",
		"axc.txt",
		"abbc.txt",
		"a1.bak",
		"b2.bak",
		"c3.bak",
		"#hash.txt",
		"spaced ",
		"spaced",
		"sub/notes.md",
		"sub/README.md",
		"sub/local.txt",
		"sub/deeper/local.txt",
		"sub/deeper/other.md",
		".hidden/file",
		"plain.txt",
	];
	let tree_root = folder("nibignore like git, tree");
	let git_root = folder("nibignore like git, git");
	for (folder, rule) in rules {
		write(&tree_root, &format!("{folder}.nibignore"), rule);
		write(&git_root, &format!("{folder}.gitignore"), rule);
	}
	for path in files {
		write(&tree_root, path, "x");
		write(&git_root, path, "x");
	}
	let empty = git_root
		.parent()
		.expect("a parent")
		.join("nibignore like git, empty config");
	fs::write(&empty, "").expect("writes");
	let git = |arguments: &[&str]| {
		let output = std::process::Command::new("git")
			.args(arguments)
			.current_dir(&git_root)
			.env("GIT_CONFIG_GLOBAL", &empty)
			.env("GIT_CONFIG_NOSYSTEM", "1")
			.output()
			.expect("runs Git");
		assert!(output.status.success(), "{output:?}");
		String::from_utf8(output.stdout).expect("UTF-8")
	};
	git(&["init", "--quiet"]);
	let listed = git(&["ls-files", "--others", "--exclude-standard", "-z"]);
	let by_git: BTreeSet<String> = listed
		.split('\0')
		.filter(|path| !path.is_empty())
		.map(|path| path.replace(".gitignore", ".nibignore"))
		.collect();

	let mut rng = TestRng::new("nibignore like git");
	let (_, mut library) = library("nibignore like git", &mut rng);
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &tree_root, None, now() + 1_000, &mut rng)
		.expect("starts");
	let by_tree: BTreeSet<String> = started
		.committed
		.changes
		.iter()
		.map(|change| change.path.clone())
		.collect();
	assert_eq!(by_tree, by_git);
}

#[test]
fn tree_paths_never_leave_the_tree_nor_name_the_library_or_git() {
	use nibkeep_node::is_safe_tree_path;
	for safe in [
		"a.txt",
		"docs/a.txt",
		".github/workflows/ci.yml",
		".gitignore",
		".nibignore",
		"git/a",
		"nib",
		"a..b",
	] {
		assert!(is_safe_tree_path(safe), "{safe}");
	}
	for unsafe_path in [
		"",
		"/a",
		"a/",
		"a//b",
		"./a",
		"a/./b",
		"..",
		"../a",
		"a/../../b",
		"a\\b",
		"a\0b",
		".git",
		".git/config",
		"x/.git/hooks/post-checkout",
		".GIT/config",
		".Git/config",
		".git./config",
		".git /config",
		".git::$INDEX_ALLOCATION/config",
		"git~1/config",
		".g\u{200c}it/config",
		".nib",
		".nib/keys.cbor",
		".NIB/keys.cbor",
		"nib~1/keys.cbor",
	] {
		assert!(!is_safe_tree_path(unsafe_path), "{unsafe_path:?}");
	}
	// Only the composed form: what a Mac writes decomposed is recorded composed.
	assert!(is_safe_tree_path("caf\u{e9}.txt"));
	assert!(!is_safe_tree_path("cafe\u{301}.txt"));
}

#[test]
fn purging_one_file_of_a_commit_leaves_the_others_as_they_were_written() {
	let mut rng = TestRng::new("tree purge one");
	let files = [
		("a.txt", "Alpha\n"),
		("b.txt", "Leaked\n"),
		("c.txt", "Gamma\n"),
	];
	let (mut library, tree, _root) = with_tree("tree purge one", &files, &mut rng);
	let personal = library.personal_space();
	let (a, b, c) = (
		at_path(&library, tree, "a.txt"),
		at_path(&library, tree, "b.txt"),
		at_path(&library, tree, "c.txt"),
	);
	let history = |library: &Library, entity| {
		library
			.history(personal, entity, Some(&CONTENT))
			.expect("reads history")
	};
	let (a_before, c_before) = (history(&library, a), history(&library, c));
	let leaked = values(&library, b, &CONTENT)[0].clone();
	library
		.purge(
			personal,
			b,
			&CONTENT,
			Some(&leaked),
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	// Each file was a part of its own, so the purge stated none of the others again: their
	// history is as it was, and so is a view of a moment before the purge.
	assert_eq!(history(&library, a), a_before);
	assert_eq!(history(&library, c), c_before);
	for entity in [a, c] {
		let past = history(&library, entity).expect("a history");
		let restated = past
			.fields
			.iter()
			.flat_map(|field| &field.values)
			.any(|value| value.restated.is_some());
		assert!(!restated, "{past:?}");
	}
	let then = folder("tree purge one, then");
	fs::remove_dir(&then).expect("removes");
	library
		.write_tree(
			personal,
			tree,
			&then,
			TreePoint::AsOf(now() + 1_500),
			now() + 3_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(
		fs::read_to_string(then.join("a.txt")).expect("reads"),
		"Alpha\n"
	);
	assert_eq!(
		fs::read_to_string(then.join("c.txt")).expect("reads"),
		"Gamma\n"
	);
	// The commit is still one, with its message.
	let log = library
		.tree_log(personal, tree, None)
		.expect("reads the log");
	let start: Vec<_> = log
		.iter()
		.filter(|entry| entry.message.as_deref() == Some("Start"))
		.collect();
	assert_eq!(start.len(), 1, "{log:?}");
	assert_eq!(start[0].part, None);
	// The purge stated again the statement that counts b.txt's part, so the commit is whole
	// still, and a view of its revision has the other files.
	nibkeep_testkit::assert_empty!(
		library
			.tree_status(personal, tree)
			.expect("status")
			.cut_short
	);
	let at_revision = folder("tree purge one, at its revision");
	fs::remove_dir(&at_revision).expect("removes");
	let revision = start[0].revision.expect("a revision");
	library
		.write_tree(
			personal,
			tree,
			&at_revision,
			TreePoint::Revision(revision),
			now() + 3_000,
			&mut rng,
		)
		.expect("writes");
	for (path, content) in [("a.txt", "Alpha\n"), ("c.txt", "Gamma\n")] {
		assert_eq!(
			fs::read_to_string(at_revision.join(path)).expect("reads"),
			content
		);
	}
}

#[test]
fn a_purge_deletes_the_copies_not_edited_and_takes_the_content_out_of_the_index() {
	let mut rng = TestRng::new("tree purge");
	let files = [
		("a.txt", "Leaked token\n"),
		("b.txt", "Leaked token\n"),
		("c.txt", "Kept\n"),
	];
	let (mut library, tree, root) = with_tree("tree purge", &files, &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	write(&root, "b.txt", "Leaked token, and an edit\n");
	let leaked = values(&library, a, &CONTENT)[0].clone();
	let purge = library
		.purgeable(personal, a, &CONTENT, Some(&leaked))
		.expect("can purge");
	let copies: Vec<(PathBuf, bool)> = purge
		.tree_copies
		.iter()
		.map(|copy| (copy.path.clone(), copy.edited))
		.collect();
	assert_eq!(
		copies,
		[(root.join("a.txt"), false), (root.join("b.txt"), true)]
	);
	let purge = library
		.purge(
			personal,
			a,
			&CONTENT,
			Some(&leaked),
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	let deleted: Vec<bool> = purge.tree_copies.iter().map(|copy| copy.deleted).collect();
	assert_eq!(deleted, [true, false]);
	assert!(!root.join("a.txt").exists(), "the copy as it was committed");
	assert_eq!(
		fs::read_to_string(root.join("b.txt")).expect("reads"),
		"Leaked token, and an edit\n",
		"an edited copy stays"
	);
	// The index no longer says what a.txt held, so its copy is not taken as deleted. The
	// edited copy was edited from content that is gone, so the person commits it knowingly.
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[("b.txt".to_owned(), Change::Unknown)]
	);
	nibkeep_testkit::assert_empty!(status.behind);
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng),
		Err(NodeError::UnknownVersion(_))
	));
	library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_force(),
			now() + 4_000,
			&mut rng,
		)
		.expect("commits the edit");
	nibkeep_testkit::assert_empty!(library.tree_status(personal, tree).expect("status").changes);
	// A copy of the purged content, made anywhere in the tree, is never committed.
	write(&root, "again.txt", "Leaked token\n");
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	let committed = library
		.commit_tree(personal, tree, &Commit::new(None), now() + 5_000, &mut rng)
		.expect("commits nothing");
	nibkeep_testkit::assert_empty!(committed.changes);
}

#[test]
fn a_copy_with_none_of_its_files_is_not_taken_for_files_all_deleted() {
	let mut rng = TestRng::new("tree emptied");
	let files = [("a.txt", "a"), ("docs/b.txt", "b")];
	let (mut library, tree, root) = with_tree("tree emptied", &files, &mut rng);
	let personal = library.personal_space();
	// A folder that is gone, as when it moved, is no folder whose files were deleted.
	let moved = root.with_file_name("tree emptied, moved");
	let _ = fs::remove_dir_all(&moved);
	fs::rename(&root, &moved).expect("moves the folder");
	assert!(matches!(
		library.tree_status(personal, tree),
		Err(NodeError::TreeFolderGone(_))
	));
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng),
		Err(NodeError::TreeFolderGone(_))
	));
	// An empty folder in its place, as a drive that is not there leaves, is not emptied for
	// good unless the person says so.
	fs::create_dir(&root).expect("makes an empty folder");
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng),
		Err(NodeError::TreeEmptied(_))
	));
	let committed = library
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_force(),
			now() + 4_000,
			&mut rng,
		)
		.expect("deletes them all");
	assert_eq!(committed.changes.len(), 2);
	nibkeep_testkit::assert_empty!(library.tree_entries(personal, tree).expect("reads"));
}

#[test]
fn a_file_that_nibignore_names_after_it_was_committed_stays_in_the_tree() {
	let mut rng = TestRng::new("tree ignored later");
	let files = [("a.log", "first"), ("b.txt", "b")];
	let (library, tree, root) = with_tree("tree ignored later", &files, &mut rng);
	let personal = library.personal_space();
	write(&root, ".nibignore", "*.log\n");
	write(&root, "a.log", "second, longer");
	write(&root, "c.log", "never taken");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			(".nibignore".to_owned(), Change::New),
			("a.log".to_owned(), Change::Edited),
		]
	);
	fs::remove_file(root.join("a.log")).expect("deletes");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			(".nibignore".to_owned(), Change::New),
			("a.log".to_owned(), Change::Deleted),
		]
	);
}

/// Sets a folder's permissions, on Unix.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
	use std::os::unix::fs::PermissionsExt;
	fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("sets permissions");
}

#[cfg(unix)]
#[test]
fn a_purged_copy_that_cannot_be_deleted_yet_is_never_committed_again() {
	let mut rng = TestRng::new("tree purge held");
	let files = [("locked/token.txt", "Leaked token\n"), ("b.txt", "b")];
	let (mut library, tree, root) = with_tree("tree purge held", &files, &mut rng);
	let personal = library.personal_space();
	let token = at_path(&library, tree, "locked/token.txt");
	let leaked = values(&library, token, &CONTENT)[0].clone();
	// Its folder lets nothing be deleted from it.
	set_mode(&root.join("locked"), 0o555);
	let purge = library
		.purge(
			personal,
			token,
			&CONTENT,
			Some(&leaked),
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(purge.tree_copies.len(), 1);
	assert!(!purge.tree_copies[0].deleted);
	assert!(root.join("locked/token.txt").exists());
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	assert_eq!(status.skipped.len(), 1, "{:?}", status.skipped);
	let nothing = library
		.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng)
		.expect("commits nothing");
	nibkeep_testkit::assert_empty!(nothing.changes);
	// Once it can be, the next status deletes it.
	set_mode(&root.join("locked"), 0o755);
	let status = library.tree_status(personal, tree).expect("status");
	assert!(!root.join("locked/token.txt").exists());
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.skipped);
}

#[cfg(target_os = "linux")]
#[test]
fn a_decomposed_name_on_the_disk_is_its_composed_path_in_the_tree() {
	let mut rng = TestRng::new("tree decomposed");
	let files = [("cafe\u{301}/menu.txt", "Coffee\n"), ("b.txt", "b")];
	let (mut library, tree, root) = with_tree("tree decomposed", &files, &mut rng);
	let personal = library.personal_space();
	let menu = at_path(&library, tree, "caf\u{e9}/menu.txt");
	// A `.nibignore` that names it later leaves it in the tree, found by its composed path.
	write(&root, ".nibignore", "caf\u{e9}/\n");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[(".nibignore".to_owned(), Change::New)]
	);
	library
		.move_in_tree(
			personal,
			tree,
			("caf\u{e9}/menu.txt", "menu.txt"),
			now() + 2_000,
			&mut rng,
		)
		.expect("moves it");
	assert_eq!(at_path(&library, tree, "menu.txt"), menu);
	assert!(root.join("menu.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_read_is_not_taken_for_its_files_deleted() {
	let mut rng = TestRng::new("tree unreadable");
	let files = [("docs/a.txt", "a"), ("docs/b.txt", "b"), ("c.txt", "c")];
	let (mut library, tree, root) = with_tree("tree unreadable", &files, &mut rng);
	let personal = library.personal_space();
	set_mode(&root.join("docs"), 0o000);
	let status = library.tree_status(personal, tree);
	let committed =
		library.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng);
	set_mode(&root.join("docs"), 0o755);
	let status = status.expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	let skipped: Vec<&str> = status
		.skipped
		.iter()
		.map(|(path, _)| path.as_str())
		.collect();
	assert_eq!(skipped, ["docs", "docs/a.txt", "docs/b.txt"]);
	nibkeep_testkit::assert_empty!(committed.expect("commits").changes);
	assert_eq!(
		library.tree_entries(personal, tree).expect("reads").len(),
		3
	);
}

#[test]
fn a_copy_emptied_with_a_purged_file_in_the_tree_is_still_not_taken_for_files_all_deleted() {
	let mut rng = TestRng::new("tree emptied after purge");
	let files = [("a.txt", "Leaked\n"), ("b.txt", "b"), ("c.txt", "c")];
	let (mut library, tree, root) = with_tree("tree emptied after purge", &files, &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	let leaked = values(&library, a, &CONTENT)[0].clone();
	library
		.purge(
			personal,
			a,
			&CONTENT,
			Some(&leaked),
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	fs::remove_dir_all(&root).expect("empties it");
	fs::create_dir(&root).expect("leaves an empty folder");
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng),
		Err(NodeError::TreeEmptied(_))
	));
}

#[test]
fn no_tree_starts_in_the_library_s_own_folder_or_in_git_s() {
	let mut rng = TestRng::new("tree reserved folders");
	let (directory, library) = library("tree reserved folders", &mut rng);
	let personal = library.personal_space();
	let inside = directory.join("inside");
	fs::create_dir_all(&inside).expect("makes a folder");
	assert!(matches!(
		library.would_start_tree(personal, &inside),
		Err(NodeError::NotATreeFolder(_))
	));
	let hooks = folder("tree reserved folders, repository").join(".git/hooks");
	fs::create_dir_all(&hooks).expect("makes a repository");
	assert!(matches!(
		library.would_start_tree(personal, &hooks),
		Err(NodeError::NotATreeFolder(_))
	));
	// A tree whose folder holds the library leaves the library out, whatever its name.
	let holder = directory
		.parent()
		.expect("a parent")
		.join("tree reserved folders, holder");
	let _ = fs::remove_dir_all(&holder);
	fs::create_dir_all(&holder).expect("makes a folder");
	write(&holder, "notes.md", "# Notes\n");
	let replica = library.close();
	let moved = holder.join("data");
	fs::rename(&directory, &moved).expect("moves the library");
	drop(replica);
	let mut library =
		Library::open(&moved, &nibkeep_node::Unlocker::Passphrase("correct horse")).expect("opens");
	let started = library
		.start_tree(personal, &holder, None, now() + 1_000, &mut rng)
		.expect("starts");
	let paths: Vec<&str> = started
		.committed
		.changes
		.iter()
		.map(|change| change.path.as_str())
		.collect();
	assert_eq!(paths, ["notes.md"]);
}

#[cfg(unix)]
#[test]
fn a_link_named_on_the_command_line_is_the_link() {
	let mut rng = TestRng::new("tree link named");
	let files = [("releases/v2/app", "app")];
	let (mut library, tree, root) = with_tree("tree link named", &files, &mut rng);
	let personal = library.personal_space();
	std::os::unix::fs::symlink("releases/v2", root.join("current")).expect("links");
	library
		.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng)
		.expect("commits the link");
	assert_eq!(
		nibkeep_node::tree_path_at(&root, &root.join("current")).expect("a path"),
		"current"
	);
	library
		.move_in_tree(
			personal,
			tree,
			("current", "latest"),
			now() + 3_000,
			&mut rng,
		)
		.expect("moves the link");
	assert!(root.join("releases/v2/app").exists(), "its target stays");
	assert!(
		fs::symlink_metadata(root.join("latest"))
			.expect("the link")
			.file_type()
			.is_symlink()
	);
}

#[test]
fn a_move_never_lands_on_a_path_that_the_tree_holds() {
	let mut rng = TestRng::new("tree move onto held");
	let files = [("a.txt", "a"), ("b.txt", "b"), ("docs/c.txt", "c")];
	let (mut library, tree, root) = with_tree("tree move onto held", &files, &mut rng);
	let personal = library.personal_space();
	fs::remove_file(root.join("b.txt")).expect("deletes the copy, not yet committed");
	for onto in ["b.txt", "docs", "b.txt/x"] {
		assert!(
			matches!(
				library.move_in_tree(personal, tree, ("a.txt", onto), now() + 2_000, &mut rng),
				Err(NodeError::PlaceTaken(_))
			),
			"{onto}"
		);
	}
	assert!(root.join("a.txt").exists());
}

#[test]
fn a_folder_replaced_by_a_file_deletes_what_was_in_it() {
	let mut rng = TestRng::new("tree folder to file");
	let files = [("docs/a.txt", "a"), ("b.txt", "b")];
	let (library, tree, root) = with_tree("tree folder to file", &files, &mut rng);
	let personal = library.personal_space();
	fs::remove_dir_all(root.join("docs")).expect("deletes the folder");
	write(&root, "docs", "now a file");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(
		changes(&status.changes),
		[
			("docs".to_owned(), Change::New),
			("docs/a.txt".to_owned(), Change::Deleted),
		]
	);
}

#[test]
fn a_commit_cut_short_is_listed_until_the_next_commit() {
	let mut rng = TestRng::new("tree cut short");
	let (mut library, tree, root) = with_tree("tree cut short", &[("a.txt", "a")], &mut rng);
	let personal = library.personal_space();
	// The first part of three, as a crash after it leaves it.
	let first = library
		.create_entity(
			personal,
			&[REVISION],
			vec![
				(IN_TREE, Value::Reference(tree)),
				(PARTS, Value::Integer(3)),
			],
			now() + 2_000,
			&mut rng,
		)
		.expect("records a first part");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.cut_short.len(), 1);
	assert_eq!(status.cut_short[0].revision, first);
	assert_eq!(
		(status.cut_short[0].made, status.cut_short[0].parts),
		(1, 3)
	);
	write(&root, "a.txt", "aa");
	library
		.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng)
		.expect("commits the rest");
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.cut_short);
}

#[test]
fn a_root_counts_only_on_the_replica_that_recorded_it() {
	let mut rng = TestRng::new("tree root elsewhere");
	let (_, mut laptop) = library("tree root elsewhere, laptop", &mut rng);
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "tree root elsewhere, desk");
	fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	let personal = laptop.personal_space();
	let laptop_device = laptop.status().expect("status").device.id;
	// The desk, turned against its user, names a folder of the laptop's as a tree's.
	let home = folder("tree root elsewhere, home");
	write(&home, "secret.txt", "keys");
	desk.install_standard(
		personal,
		&[nibkeep_std::TREE, nibkeep_std::TREE_ROOT],
		now() + 1_000,
		&mut rng,
	)
	.expect("installs the types");
	let tree = desk
		.create_entity(
			personal,
			&[nibkeep_std::TREE],
			vec![],
			now() + 2_000,
			&mut rng,
		)
		.expect("makes a tree");
	desk.create_entity(
		personal,
		&[nibkeep_std::TREE_ROOT],
		vec![
			(nibkeep_std::ROOT_OF, Value::Reference(tree)),
			(nibkeep_std::ON_DEVICE, Value::Reference(laptop_device)),
			(nibkeep_std::PATH, Value::Text(home.display().to_string())),
		],
		now() + 3_000,
		&mut rng,
	)
	.expect("records the root");
	let (first, second) = common::sync_in_memory(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	first.expect("the desk synced");
	second.expect("the laptop synced");
	assert!(laptop.tree_at(personal, &home).expect("looks").is_none());
	laptop
		.add_file(personal, &home.join("secret.txt"), now() + 5_000, &mut rng)
		.expect("adds a file there, which is in no tree");
}

#[test]
fn a_root_outlives_a_purge_made_on_another_device_and_the_purge_reaches_its_copy() {
	let mut rng = TestRng::new("tree purged elsewhere");
	let files = [("token.txt", "Leaked token\n"), ("b.txt", "b")];
	let (mut laptop, tree, root) = with_tree("tree purged elsewhere, laptop", &files, &mut rng);
	let desk_directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"tree purged elsewhere, desk",
	);
	fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("the desk joined").library;
	let personal = laptop.personal_space();
	let token = at_path(&laptop, tree, "token.txt");
	let leaked = values(&laptop, token, &CONTENT)[0].clone();
	desk.purge(
		personal,
		token,
		&CONTENT,
		Some(&leaked),
		now() + 2_000,
		&mut rng,
	)
	.expect("the desk purges");
	let (first, second) = common::sync_in_memory(&mut desk, &mut laptop, now() + 3_000, (1, 2));
	first.expect("the desk synced");
	let synced = second.expect("the laptop synced");
	// The laptop's copy goes as the sync settles, and the tree is still its tree.
	assert!(!root.join("token.txt").exists(), "{synced:?}");
	let status = laptop
		.tree_status(personal, tree)
		.expect("still the laptop's tree");
	nibkeep_testkit::assert_empty!(status.changes);
	assert!(laptop.tree_at(personal, &root).expect("looks").is_some());
}

#[test]
fn a_purge_that_comes_while_the_drive_is_away_reaches_the_copy_when_it_is_back() {
	let mut rng = TestRng::new("tree purge drive away");
	let files = [("token.txt", "Leaked token\n"), ("b.txt", "b")];
	let (mut library, tree, root) = with_tree("tree purge drive away", &files, &mut rng);
	let personal = library.personal_space();
	let token = at_path(&library, tree, "token.txt");
	let leaked = values(&library, token, &CONTENT)[0].clone();
	let away = root.with_file_name("tree purge drive away, away");
	let _ = fs::remove_dir_all(&away);
	fs::rename(&root, &away).expect("the drive goes");
	fs::create_dir(&root).expect("an empty mount point");
	library
		.purge(
			personal,
			token,
			&CONTENT,
			Some(&leaked),
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	assert!(matches!(
		library.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng),
		Err(NodeError::TreeEmptied(_))
	));
	fs::remove_dir(&root).expect("the mount point");
	fs::rename(&away, &root).expect("the drive is back");
	let status = library.tree_status(personal, tree).expect("status");
	assert!(!root.join("token.txt").exists());
	nibkeep_testkit::assert_empty!(status.changes);
}

#[test]
fn a_folder_that_holds_a_checkout_does_not_move() {
	let mut rng = TestRng::new("tree move checkout");
	let files = [("docs/a.txt", "a")];
	let (mut library, tree, root) = with_tree("tree move checkout", &files, &mut rng);
	let personal = library.personal_space();
	let outside = folder("tree move checkout, outside");
	let report = write(&outside, "report.txt", "Report\n");
	let added = library
		.add_file(personal, &report, now() + 2_000, &mut rng)
		.expect("adds a file outside the tree");
	library
		.checkout(
			personal,
			added.file,
			&root.join("docs"),
			now() + 3_000,
			&mut rng,
		)
		.expect("checks it out into the tree's folder");
	nibkeep_testkit::assert_empty!(library.tree_status(personal, tree).expect("status").changes);
	assert!(matches!(
		library.move_in_tree(personal, tree, ("docs", "notes"), now() + 4_000, &mut rng),
		Err(NodeError::HoldsOtherCopies(_))
	));
	assert!(root.join("docs/report.txt").exists());
}

#[test]
fn a_copy_deleted_here_of_what_the_tree_deleted_too_needs_no_commit() {
	let mut rng = TestRng::new("tree deleted both");
	let files = [("a.txt", "a"), ("b.txt", "b")];
	let (mut library, tree, root) = with_tree("tree deleted both", &files, &mut rng);
	let personal = library.personal_space();
	let a = at_path(&library, tree, "a.txt");
	library
		.edit_entity(
			personal,
			a,
			vec![Edit::Delete],
			&common::today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("deletes it, as another device would");
	fs::remove_file(root.join("a.txt")).expect("deletes the copy");
	let status = library.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
	write(&root, "b.txt", "bb");
	library
		.commit_tree(personal, tree, &Commit::new(None), now() + 3_000, &mut rng)
		.expect("commits without force");
}

#[test]
fn a_folder_moves_with_every_file_that_competes_for_a_path_in_it() {
	let mut rng = TestRng::new("tree move competing");
	let files = [("docs/a.txt", "a")];
	let (mut library, tree, root) = with_tree("tree move competing", &files, &mut rng);
	let personal = library.personal_space();
	// Another device added a file at the same path, offline.
	let other = library
		.create_entity(
			personal,
			&[FILE],
			vec![
				(IN_TREE, Value::Reference(tree)),
				(TREE_PATH, text("docs/a.txt")),
			],
			now() + 2_000,
			&mut rng,
		)
		.expect("adds a competing file");
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.competing, ["docs/a.txt"]);
	library
		.move_in_tree(personal, tree, ("docs", "notes"), now() + 3_000, &mut rng)
		.expect("moves");
	assert_eq!(values(&library, other, &TREE_PATH), [text("notes/a.txt")]);
	assert!(root.join("notes/a.txt").exists());
}
