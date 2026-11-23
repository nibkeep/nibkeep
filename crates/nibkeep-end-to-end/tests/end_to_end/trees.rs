//! Trees on two computers ([technical design: trees]): a folder of files and Git repositories
//! that one computer starts and the other writes, with its repositories cloned from where
//! they come from; and commits made apart on both, which compete until one settles them.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use nibkeep_end_to_end::{Device, EndToEnd, Ran, str};

use crate::common::{paired, synced};

/// Writes a file, with the folders it is in.
fn write(root: &Path, path: &str, content: &[u8]) {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
}

/// Every file in a folder and the folders in it, by its path, with its content; but what
/// Git and NibKeep keep of their own.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
	let mut files = BTreeMap::new();
	let mut folders = vec![root.to_owned()];
	while let Some(folder) = folders.pop() {
		for entry in fs::read_dir(&folder).expect("lists") {
			let path = entry.expect("an entry").path();
			let name = path.file_name().expect("a name");
			if name == ".git" || name == ".nib" {
				continue;
			}
			if path.is_dir() {
				folders.push(path);
				continue;
			}
			let relative = path
				.strip_prefix(root)
				.expect("in the folder")
				.to_string_lossy()
				.replace('\\', "/");
			files.insert(relative, fs::read(&path).expect("reads"));
		}
	}
	files
}

/// Runs nib in the tree's copy on a device.
fn in_tree(device: &Device, tree: &Path, arguments: &[&str]) -> Ran {
	let mut command = device.command(arguments);
	command.current_dir(tree);
	device.run(command)
}

/// What the device's Git prints, trimmed.
fn git(device: &Device, folder: &Path, arguments: &[&str]) -> String {
	device.git(folder, arguments).ok().stdout.trim().to_owned()
}

/// A bare repository in the test's folder, which the devices' Git reaches at
/// `https://example.invalid/NAME.git`, with or without a token in the address, as it would
/// reach a server.
fn server(test: &EndToEnd, devices: &[&Device], name: &str) {
	let repository = test.folder().join("server").join(format!("{name}.git"));
	fs::create_dir_all(&repository).expect("makes a folder");
	let first = devices.first().expect("a device that reaches the server");
	first.git(&repository, &["init", "--quiet", "--bare"]).ok();
	// As Git's settings write a path: with slashes on Windows too, and quotes and
	// backslashes escaped.
	let path = repository.to_string_lossy();
	let path = if cfg!(windows) {
		path.replace('\\', "/")
	} else {
		path.replace('\\', "\\\\")
	};
	let at = path.replace('"', "\\\"");
	for device in devices {
		device.git_config(&format!(
			"[url \"{at}\"]\n\tinsteadOf = https://example.invalid/{name}.git\n\tinsteadOf = https://ada:secret-token@example.invalid/{name}.git\n"
		));
	}
}

/// A Git repository at `folder` on `main`, with a first commit, pushed to its server through
/// an address with a token in it.
fn repository(device: &Device, folder: &Path, name: &str) {
	write(folder, "README.md", format!("{name}\n").as_bytes());
	device.git(folder, &["init", "--quiet"]).ok();
	device.git(folder, &["add", "README.md"]).ok();
	device
		.git(folder, &["commit", "--quiet", "-m", "First"])
		.ok();
	let address = format!("https://ada:secret-token@example.invalid/{name}.git");
	device
		.git(folder, &["remote", "add", "origin", &address])
		.ok();
	device
		.git(
			folder,
			&["push", "--quiet", "--set-upstream", "origin", "main"],
		)
		.ok();
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_tree_and_its_git_repositories_reach_the_other_computer() {
	let test = EndToEnd::new("tree written elsewhere");
	// Recorded by no device, as the test checks as it ends.
	test.secret("secret-token");
	let (laptop, desktop) = paired(&test);
	server(&test, &[&laptop, &desktop], "proj");
	server(&test, &[&laptop, &desktop], "tools");

	// The laptop starts a tree of notes and a repository.
	let notes = laptop.library().join("notes");
	write(&notes, "notes.md", b"# Notes\n");
	write(&notes, "docs/plan.md", b"Plan\n");
	repository(&laptop, &notes.join("proj"), "proj");
	laptop
		.nib(&["init", "notes", "--yes", "--message", "Start"])
		.ok()
		.stdout_eq(str![[r#"
Started the tree notes in [ROOT]/laptop/library/notes: 2 files, committed as revision [SHORT_ID]
new       proj, a Git repository

"#]]);
	synced(&laptop, &desktop).ok();

	// The desktop writes it, and clones the repository from its server, by the address
	// without the token, which no device records.
	desktop
		.nib(&["clone", "notes", "notes", "--clone"])
		.ok()
		.stdout_eq(str![[r#"
Wrote 2 files into [ROOT]/desktop/library/notes, which is this device's copy of it
cloned    proj, from https://example.invalid/proj.git (origin), recorded by laptop

"#]]);
	let on_desktop = desktop.library().join("notes");
	assert_eq!(files(&on_desktop), files(&notes));
	assert_eq!(
		git(&desktop, &on_desktop.join("proj"), &["rev-parse", "HEAD"]),
		git(&laptop, &notes.join("proj"), &["rev-parse", "HEAD"])
	);
	assert_eq!(
		git(
			&desktop,
			&on_desktop.join("proj"),
			&["config", "--get", "remote.origin.url"]
		),
		"https://example.invalid/proj.git"
	);
	in_tree(&desktop, &on_desktop, &["status"])
		.ok()
		.stdout_eq(str![[r#"
Nothing changed in the tree's copy here

"#]]);

	// A repository that the laptop adds later is listed on the desktop, and cloned once the
	// person agrees.
	repository(&laptop, &notes.join("tools"), "tools");
	in_tree(&laptop, &notes, &["commit", "--message", "Tools"])
		.ok()
		.stdout_eq(str![[r#"
Committed 1 change as revision [SHORT_ID]
new       tools, a Git repository

"#]]);
	synced(&laptop, &desktop).ok();
	in_tree(&desktop, &on_desktop, &["update", "--no-clone"])
		.ok()
		.stdout_eq(str![[r#"
to clone  tools, from https://example.invalid/tools.git (origin), recorded by laptop: nib update --clone clones it

"#]]);
	in_tree(&desktop, &on_desktop, &["update", "--clone"])
		.ok()
		.stdout_eq(str![[r#"
cloned    tools, from https://example.invalid/tools.git (origin), recorded by laptop

"#]]);
	assert_eq!(files(&on_desktop), files(&notes));
	in_tree(&desktop, &on_desktop, &["status"])
		.ok()
		.stdout_eq(str![[r#"
Nothing changed in the tree's copy here

"#]]);
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_tree_outside_the_librarys_folder_finds_the_library_on_both_computers() {
	let test = EndToEnd::new("tree outside the library");
	let (laptop, desktop) = paired(&test);

	// The laptop makes a folder outside the library's folder a tree, from the library's
	// folder, and works in it without naming the library.
	let notes = laptop.folder().join("projects").join("notes");
	write(&notes, "notes.md", b"# Notes\n");
	laptop
		.nib(&[
			"init",
			notes.to_str().expect("UTF-8"),
			"--yes",
			"--message",
			"Start",
		])
		.ok()
		.stdout_eq(str![[r#"
Started the tree notes in [ROOT]/laptop/projects/notes: 1 file, committed as revision [SHORT_ID]
Wrote .nib into the tree's folder, which tells nib there which library to use

"#]]);
	write(&notes, "plan.md", b"Plan\n");
	in_tree(&laptop, &notes, &["commit", "--message", "Plan"])
		.ok()
		.stdout_eq(str![[r#"
Committed 1 change as revision [SHORT_ID]
new       plan.md

"#]]);
	synced(&laptop, &desktop).ok();

	// The desktop writes it outside its library's folder too.
	let on_desktop = desktop.folder().join("projects").join("notes");
	desktop
		.nib(&["clone", "notes", on_desktop.to_str().expect("UTF-8")])
		.ok()
		.stdout_eq(str![[r#"
Wrote 2 files into [ROOT]/desktop/projects/notes, which is this device's copy of it
Wrote .nib into the tree's folder, which tells nib there which library to use

"#]]);
	assert_eq!(files(&on_desktop), files(&notes));
	in_tree(&desktop, &on_desktop, &["status"])
		.ok()
		.stdout_eq(str![[r#"
Nothing changed in the tree's copy here

"#]]);
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_tree_written_into_a_folder_that_holds_files_keeps_them() {
	let test = EndToEnd::new("tree written over files");
	let (laptop, desktop) = paired(&test);
	let notes = laptop.library().join("notes");
	write(&notes, "notes.md", b"# Notes\n");
	write(&notes, "plan.md", b"Plan\n");
	write(&notes, "todo.md", b"Todo\n");
	laptop
		.nib(&["init", "notes", "--yes", "--message", "Start"])
		.ok()
		.stdout_eq(str![[r#"
Started the tree notes in [ROOT]/laptop/library/notes: 3 files, committed as revision [SHORT_ID]

"#]]);
	synced(&laptop, &desktop).ok();

	// The desktop has a folder of its own there already: one file as the tree has it, one
	// that differs, and one the tree does not have.
	let on_desktop = desktop.library().join("notes");
	write(&on_desktop, "notes.md", b"# Notes\n");
	write(&on_desktop, "plan.md", b"My plan\n");
	write(&on_desktop, "mine.md", b"Mine\n");
	desktop
		.nib(&["clone", "notes", "notes"])
		.ok()
		.stdout_eq(str![[r#"
Wrote 1 file into [ROOT]/desktop/library/notes, which is this device's copy of it
1 file there held the tree's version already
differs   plan.md: kept as an edit; nib diff shows it, nib restore writes the tree's version
1 file there is not the tree's: the next commit adds it, unless .nibignore leaves it out

"#]]);
	in_tree(&desktop, &on_desktop, &["status"])
		.ok()
		.stdout_eq(str![[r#"
new       mine.md
edited    plan.md

"#]]);
	in_tree(&desktop, &on_desktop, &["restore", "plan.md"]).ok();
	assert_eq!(
		fs::read_to_string(on_desktop.join("plan.md")).expect("reads"),
		"Plan\n"
	);
	assert_eq!(
		fs::read_to_string(on_desktop.join("plan.md.orig")).expect("reads"),
		"My plan\n"
	);
}

/// Both computers edit apart, and commit: other lines of one file, one line of another, and
/// a photo; the laptop adds a file, and edits one that the desktop deletes.
fn commit_apart(laptop: &Device, notes: &Path, desktop: &Device, on_desktop: &Path) {
	write(notes, "notes.md", b"ONE, laptop\ntwo\nthree\nfour\n");
	write(notes, "plan.md", b"Plan: run\n");
	write(notes, "photo.bin", &[0, 1, 2, 3]);
	write(notes, "new.md", b"New on the laptop\n");
	write(notes, "old.md", b"Old, and kept\n");
	in_tree(laptop, notes, &["commit", "--message", "Laptop"])
		.ok()
		.stdout_eq(str![[r#"
Committed 5 changes as revision [SHORT_ID]
new       new.md
edited    notes.md
edited    old.md
edited    photo.bin
edited    plan.md

"#]]);
	write(on_desktop, "notes.md", b"one\ntwo\nthree\nFOUR, desktop\n");
	write(on_desktop, "plan.md", b"Plan: swim today\n");
	write(on_desktop, "photo.bin", &[0, 9]);
	fs::remove_file(on_desktop.join("old.md")).expect("deletes");
	in_tree(desktop, on_desktop, &["commit", "--message", "Desktop"])
		.ok()
		.stdout_eq(str![[r#"
Committed 4 changes as revision [SHORT_ID]
deleted   old.md
edited    notes.md
edited    photo.bin
edited    plan.md

"#]]);
}

/// Synced, the two commits compete; the desktop's update merges what it can, puts the rest
/// side by side, and brings back the file it deleted, which the laptop edited; and the
/// desktop settles them: one plan, the laptop's photo, and the rest as merged and brought
/// back.
fn settle_on_desktop(laptop: &Device, desktop: &Device, on_desktop: &Path) {
	synced(laptop, desktop).ok();
	let status = in_tree(desktop, on_desktop, &["--json", "status"])
		.ok()
		.json();
	assert_eq!(
		status["competing"],
		serde_json::json!(["notes.md", "old.md", "photo.bin", "plan.md"])
	);
	in_tree(desktop, on_desktop, &["update"])
		.ok()
		.stdout_eq(str![[r#"
written   new.md
written   old.md
merged    notes.md, to commit
conflict  photo.bin
conflict  plan.md
competing old.md: deleted on one device while changed on another; a commit keeps it, unless it is deleted first

"#]]);
	assert_eq!(
		fs::read_to_string(on_desktop.join("notes.md")).expect("reads"),
		"ONE, laptop\ntwo\nthree\nFOUR, desktop\n"
	);
	let plan = fs::read_to_string(on_desktop.join("plan.md")).expect("reads");
	assert!(
		plan.contains("<<<<<<<") && plan.contains("Plan: run") && plan.contains("Plan: swim today"),
		"{plan}"
	);
	assert_eq!(
		fs::read(on_desktop.join("photo.bin.conflict-laptop")).expect("beside"),
		[0, 1, 2, 3]
	);

	write(on_desktop, "plan.md", b"Plan: run, then swim\n");
	fs::rename(
		on_desktop.join("photo.bin.conflict-laptop"),
		on_desktop.join("photo.bin"),
	)
	.expect("renames");
	in_tree(desktop, on_desktop, &["commit", "--message", "Settled"])
		.ok()
		.stdout_eq(str![[r#"
Committed 4 changes as revision [SHORT_ID]
edited    notes.md
edited    old.md
edited    photo.bin
edited    plan.md

"#]]);
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn commits_made_apart_compete_until_one_computer_settles_them() {
	let test = EndToEnd::new("tree commits apart");
	let (laptop, desktop) = paired(&test);
	let notes = laptop.library().join("notes");
	write(&notes, "notes.md", b"one\ntwo\nthree\nfour\n");
	write(&notes, "plan.md", b"Plan: walk\n");
	write(&notes, "photo.bin", &[0, 1, 2]);
	write(&notes, "old.md", b"Old\n");
	laptop
		.nib(&["init", "notes", "--yes", "--message", "Start"])
		.ok();
	synced(&laptop, &desktop).ok();
	desktop.nib(&["clone", "notes", "notes"]).ok();
	let on_desktop = desktop.library().join("notes");

	commit_apart(&laptop, &notes, &desktop, &on_desktop);

	settle_on_desktop(&laptop, &desktop, &on_desktop);

	// The laptop takes what settled it, and both copies are the same, with nothing left to
	// commit or settle.
	synced(&laptop, &desktop).ok();
	in_tree(&laptop, &notes, &["update"])
		.ok()
		.stdout_eq(str![[r#"
written   notes.md
written   plan.md

"#]]);
	assert_eq!(files(&notes), files(&on_desktop));
	for (device, tree) in [(&laptop, &notes), (&desktop, &on_desktop)] {
		in_tree(device, tree, &["status"]).ok().stdout_eq(str![[r#"
Nothing changed in the tree's copy here

"#]]);
	}
	in_tree(&laptop, &notes, &["log"]).ok().stdout_eq(str![[r#"
revision [SHORT_ID]  [TIME]  Ada
    Settled
  edited    notes.md
  edited    old.md
  edited    photo.bin
  edited    plan.md

revision [SHORT_ID]  [TIME]  Ada
    Desktop
  deleted   old.md
  edited    notes.md
  edited    photo.bin
  edited    plan.md

revision [SHORT_ID]  [TIME]  Ada
    Laptop
  new       new.md
  edited    notes.md
  edited    old.md
  edited    photo.bin
  edited    plan.md

revision [SHORT_ID]  [TIME]  Ada
    Start
  new       notes.md
  new       old.md
  new       photo.bin
  new       plan.md

"#]]);
}
