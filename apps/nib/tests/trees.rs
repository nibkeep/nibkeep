//! Trees: folders whose files NibKeep keeps with every version, as Git keeps a working tree,
//! committed in one change each, with a revision, through commands named as Git, Mercurial,
//! Subversion, Fossil and Jujutsu name theirs ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::common::{Test, library};
use clap::Parser;
use nibkeep_cli::{Cli, CliError};
use nibkeep_node::NodeError;

/// Writes a file, with its folders, and makes it look written an hour ago, so that what
/// reads it again only because it was written a moment ago does not.
fn write(root: &Path, path: &str, content: &str) {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	let hour_ago = SystemTime::now() - Duration::from_secs(3_600);
	fs::File::options()
		.write(true)
		.open(&path)
		.and_then(|file| file.set_modified(hour_ago))
		.expect("ages");
}

/// The output with every short ID, eight hexadecimal digits, replaced by `<id>`.
fn ids_hidden(output: &str) -> String {
	output
		.split_inclusive(|c: char| c.is_whitespace() || c == ',')
		.map(|token| {
			let word = token.trim_end_matches(|c: char| c.is_whitespace() || c == ',');
			if word.len() == 8 && word.chars().all(|c| c.is_ascii_hexdigit()) {
				format!("<id>{}", &token[word.len()..])
			} else {
				token.to_owned()
			}
		})
		.collect()
}

/// A library whose folder holds a few files, and is a tree.
fn with_tree(name: &str) -> Test {
	let mut test = library(name);
	let here = test.here.clone();
	write(&here, "notes.md", "# Notes\n");
	write(&here, "docs/plan.md", "Plan\n");
	write(&here, ".nibignore", "*.log\n");
	write(&here, "build.log", "noise\n");
	test.nib(&["init", "--yes", "--message", "Start"])
		.expect("starts");
	test
}

#[test]
fn init_in_a_library_makes_its_folder_a_tree_once_the_person_agrees() {
	let mut test = library("tree init asks");
	let here = test.here.clone();
	write(&here, "notes.md", "# Notes\n");
	write(&here, "docs/plan.md", "Plan\n");

	// Without a terminal, nothing starts.
	assert!(matches!(
		test.nib(&["init"]),
		Err(CliError::UnconfirmedStart)
	));
	// Asked, the person says no.
	test.answers = Some(["n"].into());
	assert!(matches!(
		test.nib(&["init"]),
		Err(CliError::UnconfirmedStart)
	));
	assert_eq!(test.questions.len(), 1);
	let question = &test.questions[0];
	assert!(question.contains("2 files"), "{question}");
	assert!(question.contains("purge"), "{question}");
	// And then yes.
	test.answers = Some(["y"].into());
	let started = test.nib(&["init"]).expect("starts");
	let name = here
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a name");
	assert_eq!(
		ids_hidden(&started),
		format!(
			"Started the tree {name} in {}: 2 files, committed as revision <id>\n",
			here.display()
		)
	);
	assert!(matches!(
		test.nib(&["init", "--yes"]),
		Err(CliError::Node(NodeError::TreeInTree(_)))
	));
}

#[test]
fn init_makes_a_library_only_where_none_is_found() {
	// Outside every library, init given a folder or a message, which only a tree takes, says
	// how to make a library or join one.
	let mut test = Test::new("tree init outside");
	for arguments in [
		&["init", "."][..],
		&["init", "--message", "Start"],
		&["init", "--yes"],
	] {
		let refused = test.nib(arguments);
		assert!(
			matches!(refused, Err(CliError::NoLibraryForTree(_))),
			"{arguments:?}: {refused:?}"
		);
		let said = refused.expect_err("refused").to_string();
		assert!(said.contains("in your library's folder"), "{said}");
		assert!(said.contains("nib init alone"), "{said}");
		assert!(said.contains("nib init --join"), "{said}");
	}
	assert!(nibkeep_node::Library::replica_folder(&test.here.join(".nib")).is_err());
	// Init alone makes a library there, as git init makes a repository; again, in the
	// library's folder, it makes the folder a tree.
	test.answer(&["correct horse", "correct horse"]);
	let started = test.nib(&["init"]).expect("makes a library");
	assert!(
		started.starts_with("Ada Lovelace, on laptop, in "),
		"{started}"
	);
	write(&test.here, "notes.md", "# Notes\n");
	test.answer(&["correct horse"]);
	let tree = test.nib(&["init", "--yes"]).expect("makes a tree");
	assert!(tree.starts_with("Started the tree "), "{tree}");
	// The library is looked for from the folder to make a tree, as Git looks for a
	// repository.
	let mut test = library("tree init folder");
	let notes = test.here.join("notes");
	write(
		&notes, "notes.md", "# Notes
",
	);
	test.here =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "tree init elsewhere");
	let started = test
		.nib(&["init", notes.to_str().expect("UTF-8"), "--yes"])
		.expect("starts");
	assert!(started.contains("1 file"), "{started}");
	// What makes a library is never taken for a tree, nor the other way round.
	let parses = |arguments: &[&str]| {
		Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied())).is_ok()
	};
	assert!(parses(&["init", "--name", "Ada", "--passphrase"]));
	assert!(parses(&["init", "--passphrase", "--device", "laptop"]));
	assert!(parses(&["init", "notes", "--yes", "-m", "Start"]));
	assert!(!parses(&["init", "notes", "--passphrase"]));
	assert!(!parses(&["init", "--device", "laptop", "--yes"]));
	assert!(!parses(&["init", "--name", "Ada", "--message", "Start"]));
	assert!(!parses(&["init", "--join", "37-482-177", "notes"]));
}

/// A folder of its own beside the test's library, with a file in it.
fn beside(name: &str) -> std::path::PathBuf {
	let folder = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	write(&folder, "notes.md", "# Notes\n");
	folder
}

/// The library that the `.nib` file in a tree's folder names, as the file system resolves it.
fn named_in(folder: &Path) -> std::path::PathBuf {
	let text = fs::read_to_string(folder.join(".nib")).expect("reads the file");
	let named = text
		.strip_prefix("library: ")
		.and_then(|rest| rest.strip_suffix('\n'))
		.expect("one line naming the library");
	fs::canonicalize(named).expect("a folder")
}

#[test]
fn init_in_the_librarys_folder_makes_any_folder_a_tree_that_names_the_library() {
	let mut test = library("tree init beside");
	let library = test.here.clone();
	let project = beside("tree init beside, project");
	let name = project
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a name")
		.to_owned();

	// From the library's folder, as git worktree add makes a worktree of the repository there.
	let started = test
		.nib(&[
			"init",
			project.to_str().expect("UTF-8"),
			"--yes",
			"-m",
			"Start",
		])
		.expect("starts");
	assert_eq!(
		ids_hidden(&started),
		format!(
			"Started the tree {name} in {}: 1 file, committed as revision <id>\nWrote .nib into the tree's folder, which tells nib there which library to use\n",
			project.display()
		)
	);
	assert_eq!(
		named_in(&project),
		fs::canonicalize(&library).expect("a folder")
	);

	// In the tree's folder, and in the folders in it, nib finds the library by it, and never
	// takes the file into the tree.
	test.here.clone_from(&project);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
	write(&project, "docs/plan.md", "Plan\n");
	test.here = project.join("docs");
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"new       docs/plan.md\n"
	);
	let committed = test.nib(&["commit", "-m", "Plan"]).expect("commits");
	assert_eq!(
		ids_hidden(&committed),
		"Committed 1 change as revision <id>\nnew       docs/plan.md\n"
	);
	let whole = test.nib(&["status", "--library-wide"]).expect("status");
	assert!(whole.starts_with("Ada, on laptop, in "), "{whole}");

	// For programs, the file it wrote; and none for a tree in the library's folder, where nib
	// finds the library anyway.
	test.here.clone_from(&library);
	let other = beside("tree init beside, other");
	let shown = test.json(&["init", other.to_str().expect("UTF-8"), "--yes"]);
	assert_eq!(
		shown["pointer"],
		other.join(".nib").display().to_string(),
		"{shown}"
	);
	write(&library, "inside/notes.md", "# Notes\n");
	let shown = test.json(&["init", "inside", "--yes"]);
	assert_eq!(shown["pointer"], serde_json::Value::Null, "{shown}");
	assert!(!library.join("inside/.nib").exists());
}

#[test]
fn a_nib_file_counts_only_for_a_tree_of_its_library_in_that_folder() {
	let mut test = library("tree pointer checked");
	let library = test.here.clone();
	let project = beside("tree pointer checked, project");
	test.nib(&["init", project.to_str().expect("UTF-8"), "--yes"])
		.expect("starts");

	// A copy of the tree's folder, as in an archive from elsewhere, names the library, which
	// keeps no tree there: nib does not take it.
	let copy =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "tree pointer checked, copy");
	fs::copy(project.join(".nib"), copy.join(".nib")).expect("copies");
	test.here.clone_from(&copy);
	for arguments in [
		&["status"][..],
		&["status", "--library-wide"],
		&["device", "list"],
	] {
		let refused = test.nib(arguments);
		assert!(
			matches!(refused, Err(CliError::StrayPointer { .. })),
			"{arguments:?}: {refused:?}"
		);
	}

	// A file that names no library, or is not a line that names one, stops nib, which looks
	// no further.
	let bad = [
		format!("library: {}\n", copy.display()),
		"library: relative\n".to_owned(),
		"something else\n".to_owned(),
		String::new(),
	];
	for text in bad {
		fs::write(copy.join(".nib"), &text).expect("writes");
		let refused = test.nib(&["status"]);
		assert!(
			matches!(refused, Err(CliError::BadPointer { .. })),
			"{text:?}: {refused:?}"
		);
	}

	// Nor does nib leave NIBKEEP_CEILING for a library above it, as a build under development
	// must not.
	let inner = copy.join("inner");
	fs::create_dir_all(&inner).expect("makes a folder");
	fs::write(
		inner.join(".nib"),
		format!("library: {}\n", library.display()),
	)
	.expect("writes");
	test.here.clone_from(&inner);
	test.ceiling = Some(copy.clone());
	let refused = test.nib(&["status"]);
	assert!(
		matches!(refused, Err(CliError::BadPointer { .. })),
		"{refused:?}"
	);
	let said = refused.expect_err("refused").to_string();
	assert!(said.contains("NIBKEEP_CEILING"), "{said}");
}

#[test]
fn init_writes_a_lost_nib_file_again() {
	let mut test = library("tree pointer lost");
	let library = test.here.clone();
	let project = beside("tree pointer lost, project");
	let folder = project.to_str().expect("UTF-8").to_owned();
	test.nib(&["init", &folder, "--yes"]).expect("starts");
	let name = project
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a name")
		.to_owned();

	// Without the file, nib finds no library in the tree's folder.
	fs::remove_file(project.join(".nib")).expect("deletes");
	test.here.clone_from(&project);
	assert!(matches!(test.nib(&["status"]), Err(CliError::NotInLibrary)));

	// nib init in the library's folder writes it again, without asking, as the tree is there.
	test.here.clone_from(&library);
	assert_eq!(
		test.nib(&["init", &folder]).expect("writes it again"),
		format!(
			"Wrote .nib again into {}, the tree {name}'s folder, which tells nib there which library to use\n",
			project.display()
		)
	);
	assert_eq!(
		named_in(&project),
		fs::canonicalize(&library).expect("a folder")
	);
	// So it does for a file that names no library, as after the library's folder moved.
	fs::write(project.join(".nib"), "library: /nowhere\n").expect("writes");
	test.nib(&["init", &folder]).expect("writes it again");
	assert_eq!(
		named_in(&project),
		fs::canonicalize(&library).expect("a folder")
	);
	test.here.clone_from(&project);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);

	// For programs, as a tree that starts, with nothing committed.
	fs::remove_file(project.join(".nib")).expect("deletes");
	test.here.clone_from(&library);
	let shown = test.json(&["init", &folder]);
	assert_eq!(shown["revision"], serde_json::Value::Null, "{shown}");
	assert_eq!(shown["changes"], serde_json::json!([]), "{shown}");
	assert_eq!(
		shown["pointer"],
		project.join(".nib").display().to_string(),
		"{shown}"
	);

	// With the file in place, the folder is a tree already.
	assert!(matches!(
		test.nib(&["init", &folder, "--yes"]),
		Err(CliError::Node(NodeError::TreeInTree(_)))
	));
}

#[test]
fn init_makes_a_copy_of_a_trees_folder_a_tree_of_its_own() {
	let mut test = library("tree pointer copied");
	let library = test.here.clone();
	let project = beside("tree pointer copied, project");
	test.nib(&["init", project.to_str().expect("UTF-8"), "--yes"])
		.expect("starts");

	// A copy holds the tree's .nib file, which names the library that keeps no tree there:
	// init takes it all the same, and writes it anew.
	let copy = beside("tree pointer copied, copy");
	fs::copy(project.join(".nib"), copy.join(".nib")).expect("copies");
	let started = test
		.nib(&["init", copy.to_str().expect("UTF-8"), "--yes"])
		.expect("starts");
	assert!(started.starts_with("Started the tree "), "{started}");
	assert_eq!(
		named_in(&copy),
		fs::canonicalize(&library).expect("a folder")
	);
	test.here.clone_from(&copy);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
	// So in the copy itself, outside every library's folder.
	let again = beside("tree pointer copied, again");
	fs::copy(project.join(".nib"), again.join(".nib")).expect("copies");
	test.here.clone_from(&again);
	let started = test.nib(&["init", "--yes"]).expect("starts");
	assert!(started.starts_with("Started the tree "), "{started}");
}

#[test]
fn a_tree_in_the_librarys_folder_keeps_no_nib_file() {
	let mut test = library("tree pointer inside");
	let library = test.here.clone();
	let copied = library.join("copied");
	let file = copied.join(".nib");

	// A folder copied in with the .nib file of a tree elsewhere, which names no library here
	// and would take over the commands there: nib finds the library without it, so init takes
	// it away.
	write(&library, "copied/notes.md", "# Notes\n");
	fs::write(&file, "library: /nowhere\n").expect("writes");
	let started = test.nib(&["init", "copied", "--yes"]).expect("starts");
	assert!(
		started.starts_with("Started the tree copied in "),
		"{started}"
	);
	assert!(
		started.ends_with(
			"Took away .nib from the tree's folder, where nib finds the library without it\n"
		),
		"{started}"
	);
	assert!(!file.exists());
	test.here.clone_from(&copied);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);

	// One left there later, as by an older program, stops nib there; nib init in the library's
	// folder takes it away, without asking, as the tree is there.
	fs::write(&file, "library: /nowhere\n").expect("writes");
	assert!(matches!(
		test.nib(&["status"]),
		Err(CliError::BadPointer { .. })
	));
	test.here.clone_from(&library);
	assert_eq!(
		test.nib(&["init", "copied"]).expect("takes it away"),
		format!(
			"Took away .nib from {}, the tree copied's folder, where nib finds the library without it\n",
			copied.display()
		)
	);
	assert!(!file.exists());
	test.here.clone_from(&copied);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);

	// For programs, as a tree that starts, with nothing committed.
	fs::write(&file, format!("library: {}\n", library.display())).expect("writes");
	test.here.clone_from(&library);
	let shown = test.json(&["init", "copied"]);
	assert_eq!(shown["revision"], serde_json::Value::Null, "{shown}");
	assert_eq!(shown["pointer"], serde_json::Value::Null, "{shown}");
	assert_eq!(
		shown["pointer_taken_away"],
		file.display().to_string(),
		"{shown}"
	);
	// A link to a file, which nib would read as one, goes too; what it links to stays.
	#[cfg(unix)]
	{
		let target = beside("tree pointer inside, elsewhere").join("old.nib");
		fs::write(&target, "library: /nowhere\n").expect("writes");
		std::os::unix::fs::symlink(&target, &file).expect("links");
		test.nib(&["init", "copied"]).expect("takes it away");
		assert!(fs::symlink_metadata(&file).is_err());
		assert!(target.is_file());
	}
	// With nothing there to take away, the folder is a tree already.
	assert!(matches!(
		test.nib(&["init", "copied", "--yes"]),
		Err(CliError::Node(NodeError::TreeInTree(_)))
	));

	// A folder in its place, which holds no library, stays, as nib passes it by.
	write(&library, "kept/notes.md", "# Notes\n");
	fs::create_dir(library.join("kept/.nib")).expect("makes a folder");
	let shown = test.json(&["init", "kept", "--yes"]);
	assert_eq!(
		shown["pointer_taken_away"],
		serde_json::Value::Null,
		"{shown}"
	);
	assert!(library.join("kept/.nib").is_dir());
	test.here = library.join("kept");
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
}

#[test]
fn what_concerns_the_librarys_place_does_not_follow_a_nib_file() {
	let mut test = library("tree pointer place");
	let project = beside("tree pointer place, project");
	test.nib(&["init", project.to_str().expect("UTF-8"), "--yes"])
		.expect("starts");
	test.here.clone_from(&project);
	for arguments in [
		&["verify", "--drive-checked"][..],
		&["sync", "--at-login", "on"],
	] {
		let refused = test.nib(arguments);
		assert!(
			matches!(refused, Err(CliError::ThroughPointer(_))),
			"{arguments:?}: {refused:?}"
		);
	}
}

#[test]
fn init_starts_no_tree_where_its_nib_file_cannot_go() {
	let mut test = library("tree pointer in the way");
	let library = test.here.clone();
	let project = beside("tree pointer in the way, project");
	fs::create_dir(project.join(".nib")).expect("makes a folder");
	let folder = project.to_str().expect("UTF-8").to_owned();
	let refused = test.nib(&["init", &folder, "--yes"]);
	assert!(
		matches!(refused, Err(CliError::PointerInTheWay(_))),
		"{refused:?}"
	);
	// Nothing started, so once the way is clear, it starts.
	fs::remove_dir(project.join(".nib")).expect("deletes");
	test.nib(&["init", &folder, "--yes"]).expect("starts");

	// With --library, from a folder in no library, the same.
	let elsewhere = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"tree pointer in the way, elsewhere",
	);
	let other = beside("tree pointer in the way, other");
	test.here.clone_from(&elsewhere);
	let named = library.to_str().expect("UTF-8");
	test.nib(&[
		"--library",
		named,
		"init",
		other.to_str().expect("UTF-8"),
		"--yes",
	])
	.expect("starts");
	assert_eq!(
		named_in(&other),
		fs::canonicalize(&library).expect("a folder")
	);
	test.here.clone_from(&other);
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
}

#[test]
fn status_and_commit_show_and_record_what_changed() {
	let mut test = with_tree("tree status and commit");
	let here = test.here.clone();
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
	write(&here, "notes.md", "# Notes, edited\n");
	write(&here, "docs/new.md", "New\n");
	fs::remove_file(here.join("docs/plan.md")).expect("deletes");
	write(&here, "more.log", "left out\n");
	let status = test.nib(&["status"]).expect("status");
	assert_eq!(
		status,
		"deleted   docs/plan.md\nnew       docs/new.md\nedited    notes.md\n"
	);
	assert_eq!(test.nib(&["st"]).expect("status"), status);
	let shown = test.json(&["status"]);
	assert_eq!(shown["changes"][0]["path"], "docs/new.md");
	assert_eq!(shown["changes"][0]["change"], "new");
	assert_eq!(shown["changes"][1]["change"], "deleted");
	assert_eq!(shown["root"], here.display().to_string());
	// The library's status, from inside a tree too.
	let whole = test.nib(&["status", "--library-wide"]).expect("status");
	assert!(whole.starts_with("Ada, on laptop, in "), "{whole}");
	let whole = test.json(&["status", "--library-wide"]);
	assert_eq!(whole["user"]["name"], "Ada");
	assert!(whole.get("tree").is_none(), "{whole}");

	// From a folder inside the tree, a path is relative to it.
	test.here = here.join("docs");
	let committed = test
		.nib(&["commit", "new.md", "plan.md", "--message", "Docs"])
		.expect("commits");
	assert_eq!(
		ids_hidden(&committed),
		"Committed 2 changes as revision <id>\ndeleted   docs/plan.md\nnew       docs/new.md\n"
	);
	test.here = here.clone();
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"edited    notes.md\n"
	);
	let committed = test.json(&["commit"]);
	// The commit's revision, then the part that records the file.
	assert_eq!(committed["parts"], 2);
	assert!(committed["revision"].is_string());
	assert_eq!(committed["changes"][0]["path"], "notes.md");
	assert_eq!(
		test.nib(&["commit"]).expect("commits"),
		"Nothing to commit\n"
	);
	// As Mercurial and Subversion name it, and with Git's -a, which is what commit does.
	write(&here, "notes.md", "# Notes, again\n");
	let committed = test.nib(&["ci", "-a", "-m", "Again"]).expect("commits");
	assert!(committed.contains("edited    notes.md"), "{committed}");
	assert!(
		Cli::try_parse_from(["nib", "commit", "-a", "notes.md"]).is_err(),
		"-a means every change, so takes no paths"
	);
	// A revision is an entity, found by the words of its message.
	let found = test.nib(&["find", "Docs"]).expect("finds");
	assert!(found.contains("std.revision"), "{found}");
}

#[test]
fn a_move_moves_the_copy_and_the_file_at_once() {
	let mut test = with_tree("tree move");
	let here = test.here.clone();
	let moved = test
		.nib(&["mv", "notes.md", "docs/notes.md"])
		.expect("moves");
	assert_eq!(
		ids_hidden(&moved),
		"Moved notes.md to docs/notes.md, as revision <id>\n"
	);
	assert!(here.join("docs/notes.md").exists());
	assert!(!here.join("notes.md").exists());
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
	// As Mercurial names it.
	test.nib(&["rename", "docs/notes.md", "notes.md"])
		.expect("moves");
	assert!(here.join("notes.md").exists());
	assert!(matches!(
		test.nib(&["mv", "notes.md", ".nib/notes.md"]),
		Err(CliError::Node(NodeError::UnsafePath(_)))
	));
}

#[test]
fn rm_deletes_copies_and_the_next_commit_records_it() {
	let mut test = with_tree("tree rm");
	let here = test.here.clone();
	let removed = test.nib(&["rm", "notes.md"]).expect("removes");
	assert_eq!(
		removed,
		"removed   notes.md\nNothing is recorded: nib commit records it.\n"
	);
	assert!(!here.join("notes.md").exists());
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"deleted   notes.md\n"
	);
	// A folder, with Git's -r or without, as Mercurial's remove takes one.
	let removed = test.json(&["remove", "-r", "docs"]);
	assert_eq!(removed["removed"], serde_json::json!(["docs/plan.md"]));
	assert!(!here.join("docs").exists());
	// An edited copy stays, unless forced.
	test.nib(&["restore", "notes.md"]).expect("restores");
	write(&here, "notes.md", "# Notes, edited\n");
	assert!(matches!(
		test.nib(&["rm", "notes.md"]),
		Err(CliError::Node(NodeError::EditedCopy(_)))
	));
	test.nib(&["rm", "--force", "notes.md"]).expect("removes");
	assert!(!here.join("notes.md").exists());
	let committed = test.nib(&["commit", "-m", "Gone"]).expect("commits");
	assert!(committed.contains("deleted   notes.md"), "{committed}");
	assert!(committed.contains("deleted   docs/plan.md"), "{committed}");
}

#[test]
fn outside_a_tree_the_tree_commands_say_so() {
	let mut test = library("tree outside");
	assert!(matches!(test.nib(&["commit"]), Err(CliError::NotInTree(_))));
	// The library's status, as there is no tree's to show.
	let status = test.nib(&["status"]).expect("status");
	assert!(status.starts_with("Ada, on laptop, in "), "{status}");
	// Reading history names the other ways in.
	let refused = test.nib(&["log"]);
	assert!(
		matches!(refused, Err(CliError::NoTreeHere(_))),
		"{refused:?}"
	);
	let said = refused.expect_err("refused").to_string();
	assert!(said.contains("--tree"), "{said}");
	assert!(said.contains("nib history"), "{said}");
}

#[test]
fn what_nib_lacks_says_what_to_do_instead() {
	let mut test = with_tree("tree lacks");
	for (command, instead) in [
		(&["add", "."][..], "nib commit takes in new files"),
		(&["add", "-A"][..], "nib commit takes in new files"),
		(&["checkout", "main"][..], "nib clone"),
		(&["branch"][..], "no branches"),
		(&["tag", "v1"][..], "no branches"),
		(&["stash"][..], "no stash"),
		(&["reset", "--hard"][..], "nib restore"),
		(&["rebase", "main"][..], "no branches"),
		(&["fetch"][..], "nib sync"),
	] {
		let refused = test.nib(command);
		let Err(error @ CliError::NoSuchCommand { .. }) = refused else {
			panic!("{command:?}: {refused:?}");
		};
		let said = error.to_string();
		assert!(said.contains(instead), "{command:?}: {said}");
		assert!(
			said.starts_with(&format!("there is no {} command", command[0])),
			"{said}"
		);
	}
}

#[test]
fn files_outside_trees_leave_trees_alone() {
	let mut test = with_tree("tree and files");
	let here = test.here.clone();
	// In a folder, a tree's files are left out, as checkouts are; named, they fail.
	let docs = here.join("docs");
	let added = test.json(&["file", "add", docs.to_str().expect("UTF-8")]);
	assert_eq!(added["files"].as_array().map(Vec::len), Some(0));
	assert_eq!(added["left_out"], 1);
	let notes = here.join("notes.md");
	let named = test.nib_printing(&["file", "add", notes.to_str().expect("UTF-8")]);
	assert!(
		named.0.contains("in a tree; nib commit records it"),
		"{}",
		named.0
	);
	assert!(matches!(named.1, Err(CliError::FilesFailed(1))));
	assert!(matches!(
		test.nib(&[
			"file",
			"checkout",
			"plan.md",
			here.join("out.md").to_str().expect("UTF-8")
		]),
		Err(CliError::Node(NodeError::TreeFile(_)))
	));
}

/// A library whose folder is a tree, with a second commit that edits notes.md; the
/// revisions' IDs are returned, from the JSON of the commits.
fn with_two_commits(name: &str) -> (Test, Vec<String>) {
	let mut test = library(name);
	let here = test.here.clone();
	write(&here, "notes.md", "one\ntwo\n");
	write(&here, "old.txt", "Old\n");
	let started = test.json(&["init", "--yes", "--message", "Start"]);
	write(&here, "notes.md", "one\nTWO\n");
	fs::remove_file(here.join("old.txt")).expect("deletes");
	let second = test.json(&["commit", "--message", "Second"]);
	let revision =
		|shown: &serde_json::Value| shown["revision"].as_str().expect("a revision").to_owned();
	let revisions = vec![revision(&started), revision(&second)];
	(test, revisions)
}

#[test]
fn log_diff_and_blame_read_the_tree_s_history() {
	let (mut test, revisions) = with_two_commits("tree history commands");
	let log = test.nib(&["log"]).expect("logs");
	let lines: Vec<&str> = log.lines().collect();
	assert!(
		lines[0].starts_with("revision ") && lines[0].ends_with("Ada"),
		"{log}"
	);
	assert_eq!(lines[1], "    Second");
	assert_eq!(
		&lines[2..4],
		["  deleted   old.txt", "  edited    notes.md"]
	);
	assert!(log.contains("    Start"), "{log}");
	let notes_log = test.json(&["log", "notes.md"]);
	assert_eq!(notes_log.as_array().map(Vec::len), Some(2));

	let diff = test
		.nib(&["diff", &revisions[0], &revisions[1]])
		.expect("diffs");
	assert!(diff.contains("deleted   old.txt"), "{diff}");
	assert!(diff.contains("-two\n+TWO\n"), "{diff}");
	// As Mercurial, Fossil and Jujutsu name the two points.
	let named = test
		.nib(&["diff", "--from", &revisions[0], "--to", &revisions[1]])
		.expect("diffs");
	assert_eq!(named, diff);
	write(&test.here.clone(), "notes.md", "one\nTWO\nthree\n");
	let diff = test.nib(&["diff"]).expect("diffs the copy");
	assert!(diff.starts_with("edited    notes.md\n"), "{diff}");
	assert!(diff.contains("+three"), "{diff}");

	let blame = test.json(&["blame", "notes.md"]);
	assert_eq!(blame[0]["text"], "one");
	assert_eq!(blame[0]["revision"], revisions[0]);
	assert_eq!(blame[1]["revision"], revisions[1]);
	assert_eq!(test.json(&["annotate", "notes.md"]), blame);
}

#[test]
fn diff_takes_paths_after_its_revisions_as_git_diff_does() {
	let (mut test, revisions) = with_two_commits("tree diff paths");
	let here = test.here.clone();
	write(&here, "notes.md", "one\nTWO\nthree\n");
	write(&here, "docs/other.md", "Other\n");
	// A file's name is a path, though it also names the file's entity.
	let diff = test.nib(&["diff", "notes.md"]).expect("diffs a file");
	assert!(diff.starts_with("edited    notes.md\n"), "{diff}");
	assert!(diff.contains("+three"), "{diff}");
	assert!(!diff.contains("other.md"), "{diff}");
	let diff = test.nib(&["diff", "docs/"]).expect("diffs a folder");
	assert!(diff.starts_with("new       docs/other.md\n"), "{diff}");
	assert!(!diff.contains("notes.md"), "{diff}");
	// After the revisions, a file that the copy no longer holds too.
	let diff = test
		.nib(&["diff", &revisions[0], &revisions[1], "old.txt"])
		.expect("diffs");
	assert!(diff.starts_with("deleted   old.txt\n"), "{diff}");
	assert!(!diff.contains("notes.md"), "{diff}");
	// After --, also with --from.
	let diff = test
		.nib(&["diff", "--from", &revisions[0], "--", "notes.md"])
		.expect("diffs");
	assert!(diff.contains("-two\n+TWO\n"), "{diff}");
	assert!(!diff.contains("old.txt"), "{diff}");
	// What is neither a revision nor a path is refused, as what it is.
	let refused = test.nib(&["diff", "nothing.md"]).expect_err("refuses");
	assert_eq!(
		refused.to_string(),
		"nothing.md is no revision of the tree, nor a path in it"
	);
	// Two files of one name make no revision ambiguous.
	write(&here, "a/same.md", "A\n");
	write(&here, "b/same.md", "B\n");
	test.nib(&["commit", "--message", "Same"]).expect("commits");
	let refused = test.nib(&["diff", "same.md"]).expect_err("refuses");
	assert!(
		matches!(refused, CliError::NoRevisionOrPath(ref named) if named == "same.md"),
		"{refused:?}"
	);
	let refused = test
		.nib(&["diff", "--from", "notes.md"])
		.expect_err("refuses");
	assert_eq!(refused.to_string(), "the tree has no revision notes.md");
	let refused = test
		.nib(&["diff", &revisions[0], &revisions[1], &revisions[0]])
		.expect_err("refuses");
	assert_eq!(
		refused.to_string(),
		"nib diff compares two revisions at most, then takes paths"
	);
}

#[test]
fn show_of_a_revision_shows_what_it_changed() {
	let (mut test, revisions) = with_two_commits("tree show revision");
	let shown = test.nib(&["show", &revisions[1]]).expect("shows");
	assert!(shown.contains("Second"), "{shown}");
	assert!(shown.contains("\ndeleted   old.txt\n"), "{shown}");
	assert!(shown.contains("\nedited    notes.md\n"), "{shown}");
	assert!(shown.contains("-two\n+TWO\n"), "{shown}");
	let shown = test.json(&["show", &revisions[1]]);
	assert_eq!(shown["diff"][0]["path"], "notes.md");
	assert_eq!(shown["diff"][0]["change"], "edited");
	assert_eq!(shown["diff"][1]["path"], "old.txt");
	// Another entity shows no diff.
	let notes = test.json(&["show", "notes.md"]);
	assert!(notes.get("diff").is_none(), "{notes}");
}

#[test]
fn diff_and_show_take_how_many_lines_to_show_around_a_change() {
	let mut test = library("tree diff context");
	let here = test.here.clone();
	let lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
	write(&here, "long.md", &format!("{}\n", lines.join("\n")));
	let started = test.json(&["init", "--yes", "--message", "Start"]);
	let start = started["revision"].as_str().expect("a revision").to_owned();
	let mut edited = lines.clone();
	edited[9] = "LINE 10".to_owned();
	write(&here, "long.md", &format!("{}\n", edited.join("\n")));
	let patch = |diff: &serde_json::Value| diff[0]["patch"].as_str().expect("a patch").to_owned();

	// Three lines around a change unless asked, as Git shows.
	let three = patch(&test.json(&["diff"]));
	assert!(three.contains("@@ -7,7 +7,7 @@"), "{three}");
	assert!(
		three.contains(" line 7\n") && !three.contains(" line 6\n"),
		"{three}"
	);
	// None: the changed lines alone.
	let none = patch(&test.json(&["diff", "-U", "0"]));
	assert!(none.contains("@@ -10 +10 @@"), "{none}");
	assert!(!none.contains(" line 9\n"), "{none}");
	// More than the file holds: all of it.
	let all = patch(&test.json(&["diff", "--unified", "1000", "long.md"]));
	assert!(all.contains("@@ -1,20 +1,20 @@"), "{all}");
	assert!(
		all.contains(" line 1\n") && all.contains(" line 20\n"),
		"{all}"
	);

	// A revision shows the same way.
	let second = test.json(&["commit", "--message", "Shout"]);
	let revision = second["revision"].as_str().expect("a revision").to_owned();
	let shown = test.json(&["show", &revision, "-U", "1"]);
	let one = shown["diff"][0]["patch"].as_str().expect("a patch");
	assert!(one.contains("@@ -9,3 +9,3 @@"), "{one}");
	let printed = test
		.nib(&["show", &revision, "--unified", "0"])
		.expect("shows");
	assert!(printed.contains("-line 10\n+LINE 10\n"), "{printed}");
	assert!(!printed.contains(" line 9\n"), "{printed}");
	// Between two revisions, as for the copy.
	let between = test
		.nib(&["diff", "-U", "0", &start, &revision])
		.expect("diffs");
	assert!(between.contains("@@ -10 +10 @@"), "{between}");
}

/// Text without the escapes that color it.
fn uncolored(text: &str) -> String {
	[
		"\u{1b}[1m",
		"\u{1b}[31m",
		"\u{1b}[32m",
		"\u{1b}[36m",
		"\u{1b}[m",
	]
	.iter()
	.fold(text.to_owned(), |text, escape| text.replace(escape, ""))
}

#[test]
fn diff_and_show_color_their_lines_on_a_terminal_as_git_does() {
	let (mut test, revisions) = with_two_commits("tree diff colors");
	let between = ["diff", revisions[0].as_str(), revisions[1].as_str()];
	// For a program that reads what nib prints, no colors.
	let plain = test.nib(&between).expect("diffs");
	assert!(!plain.contains('\u{1b}'), "{plain:?}");

	// On a terminal, Git's: files and their headers bold, the hunks' lines cyan, removed
	// lines red and added ones green, each line ending its color.
	test.colors = true;
	let colored = test.nib(&between).expect("diffs");
	assert_eq!(
		colored,
		"\u{1b}[1medited    notes.md\u{1b}[m\n\
		 \u{1b}[1m--- a/notes.md\u{1b}[m\n\
		 \u{1b}[1m+++ b/notes.md\u{1b}[m\n\
		 \u{1b}[36m@@ -1,2 +1,2 @@\u{1b}[m\n \
		 one\n\
		 \u{1b}[31m-two\u{1b}[m\n\
		 \u{1b}[32m+TWO\u{1b}[m\n\
		 \u{1b}[1mdeleted   old.txt\u{1b}[m\n\
		 \u{1b}[1m--- a/old.txt\u{1b}[m\n\
		 \u{1b}[1m+++ b/old.txt\u{1b}[m\n\
		 \u{1b}[36m@@ -1 +0,0 @@\u{1b}[m\n\
		 \u{1b}[31m-Old\u{1b}[m\n"
	);
	assert_eq!(uncolored(&colored), plain);
	let never = test
		.nib(&["diff", "--color=never", &revisions[0], &revisions[1]])
		.expect("diffs");
	assert_eq!(never, plain);

	// Asked for, also for a program, as git diff --color is; alone, it takes no revision
	// for its value.
	test.colors = false;
	let asked = test
		.nib(&["diff", "--color", &revisions[0], &revisions[1]])
		.expect("diffs");
	assert_eq!(asked, colored);
	let always = test
		.nib(&["diff", "--color=always", &revisions[0], &revisions[1]])
		.expect("diffs");
	assert_eq!(always, colored);
	let auto = test
		.nib(&["diff", "--color=auto", &revisions[0], &revisions[1]])
		.expect("diffs");
	assert_eq!(auto, plain);
	// JSON stays as it is.
	let json = test.nib(&["--json", "diff", "--color", &revisions[0], &revisions[1]]);
	assert!(!json.expect("diffs").contains("\\u001b"));

	// A removed line that starts as a header does, or an added one, is colored as a line.
	let here = test.here.clone();
	write(&here, "notes.md", "-- one\nTWO\n");
	test.nib(&["commit", "--message", "Dashes"])
		.expect("commits");
	write(&here, "notes.md", "++ one\nTWO\n");
	let lookalike = test.nib(&["diff", "--color"]).expect("diffs");
	assert!(
		lookalike.contains(
			"\u{1b}[36m@@ -1,2 +1,2 @@\u{1b}[m\n\
			 \u{1b}[31m--- one\u{1b}[m\n\
			 \u{1b}[32m+++ one\u{1b}[m\n TWO\n"
		),
		"{lookalike:?}"
	);

	// Each hunk's header is cyan, a line keeps its carriage return inside its color, and the
	// note that a file ends without a line break stays plain.
	let lines: Vec<String> = (1..=12).map(|n| format!("line {n}\r")).collect();
	write(&here, "far.md", &lines.join("\n"));
	test.nib(&["commit", "--message", "Far"]).expect("commits");
	let mut edited = lines.clone();
	edited[0] = "LINE 1\r".to_owned();
	edited[11] = "LINE 12\r".to_owned();
	write(&here, "far.md", &edited.join("\n"));
	let far = test
		.nib(&["diff", "--color", "-U", "1", "far.md"])
		.expect("diffs");
	assert_eq!(
		far,
		"\u{1b}[1medited    far.md\u{1b}[m\n\
		 \u{1b}[1m--- a/far.md\u{1b}[m\n\
		 \u{1b}[1m+++ b/far.md\u{1b}[m\n\
		 \u{1b}[36m@@ -1,2 +1,2 @@\u{1b}[m\n\
		 \u{1b}[31m-line 1\r\u{1b}[m\n\
		 \u{1b}[32m+LINE 1\r\u{1b}[m\n \
		 line 2\r\n\
		 \u{1b}[36m@@ -11,2 +11,2 @@\u{1b}[m\n \
		 line 11\r\n\
		 \u{1b}[31m-line 12\r\u{1b}[m\n\
		 \\ No newline at end of file\n\
		 \u{1b}[32m+LINE 12\r\u{1b}[m\n\
		 \\ No newline at end of file\n",
		"{far:?}"
	);
	let plain = test.nib(&["diff", "-U", "1", "far.md"]).expect("diffs");
	assert_eq!(uncolored(&far), plain);

	// nib show colors the diff of a revision the same way.
	let shown = test
		.nib(&["show", "--color", &revisions[1]])
		.expect("shows");
	assert!(
		shown.contains("\u{1b}[31m-two\u{1b}[m\n\u{1b}[32m+TWO\u{1b}[m\n"),
		"{shown:?}"
	);
	test.colors = true;
	assert_eq!(test.nib(&["show", &revisions[1]]).expect("shows"), shown);
	let plain = test
		.nib(&["show", "--color=never", &revisions[1]])
		.expect("shows");
	assert_eq!(plain, uncolored(&shown));
}

#[test]
fn restore_and_revert_write_the_copy_and_record_nothing() {
	let (mut test, revisions) = with_two_commits("tree going back");
	let here = test.here.clone();
	let restored = test.nib(&["restore", "old.txt"]).expect("restores");
	assert_eq!(
		restored,
		"restored  old.txt\nNothing is recorded: nib commit records what you keep.\n"
	);
	assert_eq!(
		fs::read_to_string(here.join("old.txt")).expect("reads"),
		"Old\n"
	);
	fs::remove_file(here.join("old.txt")).expect("deletes it again");
	let reverted = test.nib(&["revert", &revisions[1]]).expect("reverts");
	assert!(reverted.contains("written   notes.md"), "{reverted}");
	assert!(reverted.contains("written   old.txt"), "{reverted}");
	assert_eq!(
		fs::read_to_string(here.join("notes.md")).expect("reads"),
		"one\ntwo\n"
	);
	let status = test.nib(&["status"]).expect("status");
	assert_eq!(status, "new       old.txt\nedited    notes.md\n");

	// Restoring over edits discards them from the copy, and keeps them beside it.
	let restored = test
		.nib(&["restore", "notes.md", "-r", &revisions[1]])
		.expect("restores");
	assert_eq!(
		restored,
		"restored  notes.md, your edits kept as notes.md.orig\nNothing is recorded: nib commit records what you keep.\n"
	);
	assert_eq!(
		fs::read_to_string(here.join("notes.md")).expect("reads"),
		"one\nTWO\n"
	);
	assert_eq!(
		fs::read_to_string(here.join("notes.md.orig")).expect("reads"),
		"one\ntwo\n"
	);
	let restored = test.json(&["restore", "old.txt", "--at", &revisions[0]]);
	assert_eq!(restored["written"], serde_json::json!([]));
	fs::remove_file(here.join("notes.md.orig")).expect("deletes");
	fs::remove_file(here.join("old.txt")).expect("deletes");

	// As Mercurial and Fossil name it; a path is for restore.
	test.nib(&["backout", &revisions[1]]).expect("reverts");
	let refused = test.nib(&["revert", "notes.md"]);
	assert!(
		matches!(refused, Err(CliError::RevertTakesRevision(_))),
		"{refused:?}"
	);
	let said = refused.expect_err("refused").to_string();
	assert!(said.contains("nib restore notes.md"), "{said}");
}

#[test]
fn a_tree_is_cloned_into_a_new_folder_at_a_revision() {
	let (mut test, revisions) = with_two_commits("tree cloned");
	let here = test.here.clone();
	let name = here
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a name")
		.to_owned();
	let picture = here
		.parent()
		.expect("a parent")
		.join("tree cloned, picture");
	let _ = fs::remove_dir_all(&picture);
	let written = test
		.nib(&[
			"clone",
			&name,
			picture.to_str().expect("UTF-8"),
			"-r",
			&revisions[0],
		])
		.expect("writes");
	assert_eq!(
		written,
		format!(
			"Wrote 2 files into {}, a picture of the past, which is no copy\n",
			picture.display()
		)
	);
	assert_eq!(
		fs::read_to_string(picture.join("notes.md")).expect("reads"),
		"one\ntwo\n"
	);
	assert!(matches!(
		test.nib(&["clone", &name, picture.to_str().expect("UTF-8")]),
		Err(CliError::Node(NodeError::TreeHereAlready(_)))
	));
	// Without a folder, a folder named after the tree, in the current one.
	let elsewhere =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "tree cloned, elsewhere");
	test.here.clone_from(&elsewhere);
	let library = here.to_str().expect("UTF-8");
	test.nib(&["--library", library, "clone", &name, "--at", &revisions[1]])
		.expect("writes");
	assert_eq!(
		fs::read_to_string(elsewhere.join(&name).join("notes.md")).expect("reads"),
		"one\nTWO\n"
	);
}

#[test]
fn update_brings_the_copy_to_the_tree_as_it_is() {
	let mut test = with_tree("tree update command");
	let here = test.here.clone();
	assert_eq!(
		test.nib(&["update"]).expect("updates"),
		"The copy here has the tree as it is\n"
	);
	// Elsewhere, as with nib edit on another device, a file moves.
	test.nib(&["edit", "plan.md", "std.tree-path=docs/later.md"])
		.expect("moves it in the tree");
	let updated = test.nib(&["up"]).expect("updates");
	assert_eq!(updated, "moved     docs/later.md, from docs/plan.md\n");
	assert!(here.join("docs/later.md").exists());
	assert_eq!(
		test.nib(&["status"]).expect("status"),
		"Nothing changed in the tree's copy here\n"
	);
}

#[test]
fn pull_syncs_then_updates_and_push_syncs() {
	let mut test = with_tree("tree pull");
	let here = test.here.clone();
	test.nib(&["edit", "plan.md", "std.tree-path=docs/later.md"])
		.expect("moves it in the tree");
	// A library on one device has nothing to sync with, which pull says once it updated.
	let (pulled, result) = test.nib_printing(&["pull", "--wait", "0"]);
	assert_eq!(pulled, "moved     docs/later.md, from docs/plan.md\n");
	assert!(
		matches!(result, Err(CliError::Node(NodeError::NoOtherReplicas))),
		"{result:?}"
	);
	assert!(here.join("docs/later.md").exists());
	let (pulled, result) = test.nib_printing(&["--json", "pull", "--wait", "0"]);
	assert!(result.is_err());
	let pulled: serde_json::Value = serde_json::from_str(&pulled).expect("JSON");
	assert_eq!(pulled["sync"], serde_json::Value::Null);
	assert_eq!(pulled["update"]["written"], serde_json::json!([]));
	assert!(matches!(
		test.nib(&["push", "--wait", "0"]),
		Err(CliError::Node(NodeError::NoOtherReplicas))
	));
}

/// Runs the test's Git in `folder`, and returns what it wrote.
fn git(test: &Test, folder: &Path, arguments: &[&str]) -> String {
	let output = test
		.git_command()
		.arg("-C")
		.arg(folder)
		.args(arguments)
		.output()
		.expect("runs Git");
	assert!(
		output.status.success(),
		"git {arguments:?}: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A library whose folder holds notes and a Git repository at `proj`, on `main`, whose
/// `origin` is an address with a token in it, which the test's Git takes to a bare
/// repository beside it that holds the first commit; then a tree.
fn with_repository(name: &str) -> Test {
	let mut test = with_repository_unstarted(name);
	test.nib(&["init", "--yes", "--message", "Start"])
		.expect("starts");
	test
}

/// The library of [`with_repository`], before its folder is a tree.
fn with_repository_unstarted(name: &str) -> Test {
	let test = library(name);
	let here = test.here.clone();
	write(&here, "notes.md", "# Notes\n");
	let remote =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, remote"));
	git(&test, &remote, &["init", "--quiet", "--bare"]);
	// What the person's Git would reach over the network, it reaches on the disk.
	let settings = format!(
		"{}[url \"{}\"]\n\tinsteadOf = https://example.invalid/proj.git\n\tinsteadOf = https://ada:secret-token@example.invalid/proj.git\n",
		fs::read_to_string(test.git_home.join("config")).expect("reads"),
		remote.display().to_string().replace('\\', "/")
	);
	fs::write(test.git_home.join("config"), settings).expect("writes");
	let proj = here.join("proj");
	fs::create_dir(&proj).expect("makes a folder");
	git(&test, &proj, &["init", "--quiet"]);
	write(&here, "proj/README.md", "Proj\n");
	git(&test, &proj, &["add", "README.md"]);
	git(&test, &proj, &["commit", "--quiet", "-m", "First"]);
	git(
		&test,
		&proj,
		&[
			"remote",
			"add",
			"origin",
			"https://ada:secret-token@example.invalid/proj.git",
		],
	);
	git(
		&test,
		&proj,
		&["push", "--quiet", "--set-upstream", "origin", "main"],
	);
	test
}

#[test]
fn a_tree_records_its_git_repositories_when_it_starts() {
	let mut test = with_repository_unstarted("tree git start");
	let started = test
		.nib(&["init", "--yes", "--message", "Start"])
		.expect("starts");
	assert!(
		started.contains("new       proj, a Git repository"),
		"{started}"
	);
	assert!(!started.contains("README"), "{started}");
	let status = test.nib(&["status"]).expect("status");
	assert_eq!(status, "Nothing changed in the tree's copy here\n");
	let listed = test.json(&["status", "--json"]);
	assert_eq!(listed["repositories"], serde_json::json!([]));
	assert_eq!(listed["only_here"], serde_json::json!([]));
}

#[test]
fn status_names_the_work_that_only_this_computer_holds() {
	let mut test = with_repository("tree git work here");
	let proj = test.here.join("proj");
	write(&test.here, "proj/more.md", "More\n");
	git(&test, &proj, &["add", "more.md"]);
	git(&test, &proj, &["commit", "--quiet", "-m", "Not pushed"]);
	write(&test.here, "proj/draft.md", "Draft\n");
	let status = test.nib(&["status"]).expect("status");
	assert!(
		status.contains(
			"only here proj, a Git repository: changes not committed, 1 commit not pushed"
		),
		"{status}"
	);
}

#[test]
fn a_commit_records_what_changed_of_a_repository_and_pin_pins_it() {
	let mut test = with_repository("tree git commit");
	let proj = test.here.join("proj");
	git(
		&test,
		&proj,
		&["remote", "add", "backup", "git@example.invalid:me/proj.git"],
	);
	let committed = test.nib(&["commit", "-m", "Backup"]).expect("commits");
	assert!(
		committed.contains("edited    proj, a Git repository's remotes, branch or pinned commit"),
		"{committed}"
	);
	// The repository's change counts, as the files' do.
	assert!(
		committed.starts_with("Committed 1 change as revision "),
		"{committed}"
	);
	let head = git(&test, &proj, &["rev-parse", "HEAD"]);
	let pinned = test.nib(&["commit", "--pin", "proj"]).expect("pins");
	assert_eq!(
		ids_hidden(&pinned),
		format!("Pinned proj at {}, as revision <id>\n", &head[..12])
	);
	let unpinned = test.nib(&["commit", "--unpin", "proj"]).expect("unpins");
	assert_eq!(ids_hidden(&unpinned), "Unpinned proj, as revision <id>\n");
	assert!(matches!(
		test.nib(&["commit", "--pin", "notes.md"]),
		Err(CliError::NoGitCommit(_) | CliError::Node(NodeError::NoRepository(_)))
	));
}

/// The repository `proj` of a test's tree gone, as on a computer that never had it: its
/// folder, and the index of copies with it.
fn as_never_cloned(test: &Test) {
	fs::remove_dir_all(test.here.join("proj")).expect("deletes");
	for replica in fs::read_dir(test.here.join(".nib/replicas")).expect("lists") {
		let index = replica.expect("reads").path().join("trees.sqlite");
		if index.exists() {
			fs::remove_file(index).expect("loses the index");
		}
	}
}

#[test]
fn update_clones_a_repository_this_copy_lacks_once_the_person_agrees() {
	let mut test = with_repository("tree git clone");
	as_never_cloned(&test);
	let listed = test.nib(&["update", "--no-clone"]).expect("updates");
	assert!(
		listed.contains(
			"to clone  proj, from https://example.invalid/proj.git (origin), recorded by"
		),
		"{listed}"
	);
	assert!(listed.contains("nib update --clone clones it"), "{listed}");
	assert!(!test.here.join("proj").exists());
	// Asked, the person says no, then yes.
	test.answers = Some(["n"].into_iter().collect());
	test.nib(&["update"]).expect("updates");
	assert!(!test.here.join("proj").exists());
	test.answers = Some(["y"].into_iter().collect());
	let cloned = test.nib(&["update"]).expect("updates");
	assert!(cloned.contains("cloned    proj"), "{cloned}");
	assert!(
		test.questions
			.iter()
			.any(|question| question.contains("proj from https://example.invalid/proj.git"))
	);
	let proj = test.here.join("proj");
	assert_eq!(
		fs::read_to_string(proj.join("README.md")).expect("reads"),
		"Proj\n"
	);
	assert_eq!(
		git(&test, &proj, &["config", "--get", "remote.origin.url"]),
		"https://example.invalid/proj.git"
	);
	assert_eq!(git(&test, &proj, &["branch", "--show-current"]), "main");
	// Not listed again.
	let again = test.nib(&["update"]).expect("updates");
	assert!(!again.contains("proj"), "{again}");
}

#[test]
fn a_branch_that_no_remote_has_is_cloned_as_the_remote_s_own() {
	let mut test = with_repository("tree git branch not pushed");
	let proj = test.here.join("proj");
	git(&test, &proj, &["switch", "--quiet", "--create", "feature"]);
	test.nib(&["commit", "-m", "Feature"]).expect("commits");
	as_never_cloned(&test);
	let cloned = test.nib(&["update", "--clone"]).expect("updates");
	assert!(cloned.contains("cloned    proj"), "{cloned}");
	assert_eq!(git(&test, &proj, &["branch", "--show-current"]), "main");
}

#[test]
fn a_clone_that_fails_leaves_nothing_behind() {
	let mut test = with_repository("tree git clone fails");
	let proj = test.here.join("proj");
	write(&test.here, "proj/local.md", "Only here\n");
	git(&test, &proj, &["add", "local.md"]);
	git(&test, &proj, &["commit", "--quiet", "-m", "Not pushed"]);
	test.nib(&["commit", "--pin", "proj"]).expect("pins");
	as_never_cloned(&test);
	let failed = test.nib(&["update", "--clone"]).expect("updates");
	assert!(failed.contains("could not clone proj"), "{failed}");
	assert!(!proj.exists());
}

#[test]
fn a_tree_keeps_its_env_as_a_secret_file_read_with_the_secrets_alone() {
	let mut test = library("tree secrets");
	let here = test.here.clone();
	write(&here, "notes.md", "# Notes\n");
	write(&here, ".env", "TOKEN=s3ntinel\n");
	// The library's first secret sets up its secrets, with a passphrase of their own.
	test.passphrases = ["secrets passphrase", "secrets passphrase"].into();
	let started = test.nib(&["init", "--yes"]).expect("starts");
	assert!(started.contains("secret    .env"), "{started}");
	assert!(started.contains("2 files"), "{started}");

	// A later edit is a secret file's, which needs the passphrase to commit.
	write(&here, ".env", "TOKEN=again\n");
	let status = test.nib(&["status"]).expect("status");
	assert!(status.contains("edited    .env"), "{status}");
	assert!(status.contains("secret    .env"), "{status}");
	assert!(matches!(
		test.nib(&["commit"]),
		Err(CliError::Node(NodeError::SecretsNeeded(_)))
	));
	let left = test
		.nib(&["commit", "--leave-secrets"])
		.expect("commits the rest");
	assert!(left.contains("left      .env"), "{left}");
	test.passphrases = ["secrets passphrase"].into();
	let committed = test.nib(&["commit"]).expect("commits");
	assert!(committed.contains("secret    .env"), "{committed}");

	// Its lines show only when asked for.
	write(&here, ".env", "TOKEN=third\n");
	let diff = test.nib(&["diff"]).expect("diffs");
	assert!(diff.contains("edited    .env"), "{diff}");
	assert!(!diff.contains("TOKEN"), "{diff}");
	test.passphrases = ["secrets passphrase"].into();
	let shown = test.nib(&["diff", "--secrets"]).expect("diffs");
	assert!(shown.contains("+TOKEN=third"), "{shown}");
	assert!(matches!(
		test.nib(&["blame", ".env"]),
		Err(CliError::Node(NodeError::SecretLines(_)))
	));

	// The status names the secret files, and the ordinary ones that look secret, when asked:
	// a file taken in as ordinary on purpose, such as a public certificate, is not named in
	// every status.
	write(&here, "public.pem", "public\n");
	test.nib(&["commit", "--ordinary", "public.pem", "--leave-secrets"])
		.expect("commits");
	let status = test.nib(&["status"]).expect("status");
	assert!(!status.contains("public.pem"), "{status}");
	let listed = test.nib(&["status", "--secrets"]).expect("lists");
	assert!(listed.contains("secret    .env"), "{listed}");
	assert!(
		listed.contains("ordinary  public.pem, which looks secret: nib commit --secret public.pem"),
		"{listed}"
	);
	let listed = test.json(&["status", "--secrets"]);
	assert_eq!(listed["secret"][0], ".env");
	assert_eq!(listed["looking_secret"][0], "public.pem");
}

#[test]
fn a_tree_of_another_space_takes_its_first_secret_file_as_the_personal_space_s_would() {
	let mut test = library("tree secrets in a space");
	let top = test.here.clone();
	test.nib(&["space", "new", "Rig"]).expect("makes");
	test.nib(&["space", "new", "Work"]).expect("makes");
	test.here = top.join("rig");
	write(&test.here, "notes.md", "# Notes\n");
	write(&test.here, ".nibsecret", "power-switch/wifi.env\n");
	test.nib(&["--space", "Rig", "init", "--yes"])
		.expect("starts");
	// The library's first secret sets up its secrets, and the space's with them.
	write(&test.here, "power-switch/wifi.env", "PASSWORD=s3ntinel\n");
	test.passphrases = ["secrets passphrase", "secrets passphrase"].into();
	let committed = test.nib(&["commit"]).expect("commits");
	assert!(
		committed.contains("secret    power-switch/wifi.env"),
		"{committed}"
	);
	// In a space that has none yet, the passphrase of the locked secrets is enough.
	test.here = top.join("work");
	write(&test.here, "notes.md", "# Notes\n");
	test.nib(&["--space", "Work", "init", "--yes"])
		.expect("starts");
	write(&test.here, ".env", "TOKEN=s3ntinel\n");
	test.passphrases = ["secrets passphrase"].into();
	let committed = test.nib(&["commit"]).expect("commits");
	assert!(committed.contains("secret    .env"), "{committed}");
}

#[test]
fn update_asks_before_it_moves_a_secret_file_where_another_device_moved_it() {
	let mut test = library("tree secret moved");
	let here = test.here.clone();
	write(&here, "notes.md", "# Notes\n");
	write(&here, ".env", "TOKEN=s3ntinel\n");
	test.passphrases = ["secrets passphrase", "secrets passphrase"].into();
	test.nib(&["init", "--yes"]).expect("starts");
	test.nib(&["edit", ".env", "std.tree-path=public/env.txt"])
		.expect("moves it, as another device may");
	let held = test.nib(&["update"]).expect("updates");
	assert!(held.contains("held      .env -> public/env.txt"), "{held}");
	assert!(here.join(".env").exists());
	// At a terminal, it asks for the secrets, and whether to write the move.
	test.answers = Some(["y"].into());
	test.passphrases = ["secrets passphrase"].into();
	let moved = test.nib(&["update"]).expect("updates");
	assert_eq!(test.questions.len(), 1, "{:?}", test.questions);
	assert!(
		moved.contains("moved     public/env.txt, from .env"),
		"{moved}"
	);
	assert!(here.join("public/env.txt").exists());
}
