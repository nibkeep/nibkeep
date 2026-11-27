//! `nib file checkout` and `nib file checkin`: plain copies of files, written on request,
//! whose edits become new versions of the file, and which a purge of the file's content
//! deletes ([technical design: checkouts]).
//!
//! [technical design: checkouts]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#92-checkouts

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{Test, library};
use nibkeep_cli::CliError;
use nibkeep_node::NodeError;

/// A folder of its own, as the file system resolves it, so that it reads as `nib` shows it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	nibkeep_node::resolved_path(&made).expect("resolves")
}

fn text(path: &Path) -> &str {
	path.to_str().expect("UTF-8")
}

const NOTES: &str = "Milk, eggs, bread.\n";

/// A library with one file, `notes.txt`, added from a folder of its own, which is returned.
fn with_notes(name: &str) -> (Test, PathBuf) {
	let mut test = library(name);
	let files = folder(&format!("{name}, files"));
	fs::write(files.join("notes.txt"), NOTES).expect("writes");
	test.nib(&["file", "add", text(&files.join("notes.txt"))])
		.expect("adds");
	(test, files)
}

/// The lines of the output with the short ID after their first word replaced by `<id>`.
fn short_ids_hidden(output: &str) -> String {
	output
		.lines()
		.map(|line| {
			let word = line.find(' ').unwrap_or(line.len());
			let start = word + (line[word..].len() - line[word..].trim_start().len());
			match line.get(start..start + 8) {
				Some(id)
					if id.chars().all(|c| c.is_ascii_hexdigit())
						&& line[start + 8..].starts_with(' ') =>
				{
					format!("{}<id>{}\n", &line[..start], &line[start + 8..])
				}
				_ => format!("{line}\n"),
			}
		})
		.collect()
}

/// The values of a field, from the JSON of `nib show` or `nib history`.
fn values_of(shown: &serde_json::Value, field: &str) -> Vec<serde_json::Value> {
	shown["fields"]
		.as_array()
		.expect("fields")
		.iter()
		.filter(|shown| shown["name"] == field)
		.flat_map(|shown| shown["values"].as_array().cloned().unwrap_or_default())
		.map(|value| value["value"].clone())
		.collect()
}

/// The names of the files in a folder, sorted.
fn names_in(folder: &Path) -> Vec<String> {
	let mut names: Vec<String> = fs::read_dir(folder)
		.expect("reads")
		.map(|entry| {
			entry
				.expect("an entry")
				.file_name()
				.to_string_lossy()
				.into_owned()
		})
		.collect();
	names.sort();
	names
}

/// The sealed blobs that a library's store holds: the files in its spaces' `blobs`, but
/// those being written.
fn sealed_blobs(library: &Path) -> usize {
	fn count(folder: &Path, in_blobs: bool) -> usize {
		let Ok(entries) = fs::read_dir(folder) else {
			return 0;
		};
		entries
			.map(|entry| entry.expect("an entry").path())
			.map(|path| {
				let name = path
					.file_name()
					.map(|name| name.to_string_lossy().into_owned());
				match (path.is_dir(), name.as_deref()) {
					(true, Some("tmp")) if in_blobs => 0,
					(true, name) => count(&path, in_blobs || name == Some("blobs")),
					(false, _) => usize::from(in_blobs),
				}
			})
			.sum()
	}
	count(&library.join(".nib"), false)
}

#[test]
fn a_checkout_writes_a_plain_copy_once_and_replaces_nothing() {
	let (mut test, _) = with_notes("checkout written");
	let out = folder("checkout written, out");
	// What a checkout cut short by a crash left behind, named after the copy's name.
	let digest = nibkeep_base::Hash::of(b"notes.txt").to_string();
	let leftover = format!(".nib-checkout-{}", &digest["blake3:".len()..][..16]);
	fs::write(out.join(&leftover), "Milk").expect("writes");

	let said = test
		.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	let copy = out.join("notes.txt");
	assert_eq!(
		said,
		format!("Checked out notes.txt to {}\n", copy.display())
	);
	assert_eq!(fs::read_to_string(&copy).expect("reads"), NOTES);
	assert_eq!(names_in(&out), ["notes.txt"]);

	let again = test
		.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	assert_eq!(
		again,
		format!(
			"notes.txt is checked out to {} already, unchanged\n",
			copy.display()
		)
	);

	// To a path of its own, and to the current folder.
	let named = out.join("shopping.txt");
	test.nib(&["file", "checkout", "notes.txt", text(&named)])
		.expect("checks out");
	assert_eq!(fs::read_to_string(&named).expect("reads"), NOTES);
	test.nib(&["file", "checkout", "notes.txt"])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(test.here.join("notes.txt")).expect("reads"),
		NOTES
	);

	// A file there already stays as it is.
	let taken = out.join("taken.txt");
	fs::write(&taken, "Someone else's.\n").expect("writes");
	assert!(matches!(
		test.nib(&["file", "checkout", "notes.txt", text(&taken)]),
		Err(CliError::Node(NodeError::PlaceTaken(_)))
	));
	assert_eq!(
		fs::read_to_string(&taken).expect("reads"),
		"Someone else's.\n"
	);

	let status = test.nib(&["status"]).expect("shows");
	assert!(
		status.ends_with("Checkouts on this device: 3\n"),
		"{status}"
	);
	let status = test.json(&["status"]);
	let checkouts = status["checkouts"].as_array().expect("an array");
	assert_eq!(checkouts.len(), 3);
	assert!(
		checkouts
			.iter()
			.all(|checkout| checkout["copy"] == "unchanged")
	);

	// A search for a checkout's words finds the file, once.
	let found = &test.json(&["find", "shopping"])["entities"];
	assert_eq!(found.as_array().map(Vec::len), Some(1));
	assert_eq!(found[0]["name"], "notes.txt");
	assert!(test.nib(&["verify"]).is_ok());
}

#[test]
fn an_unchanged_copy_checks_in_by_being_deleted() {
	let (mut test, _) = with_notes("checkin unchanged");
	let out = folder("checkin unchanged, out");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	let copy = out.join("notes.txt");

	let said = test
		.nib(&["file", "checkin", text(&copy)])
		.expect("checks in");
	assert_eq!(
		said,
		format!(
			"Deleted the copy of notes.txt at {}, which was unchanged\n",
			copy.display()
		)
	);
	assert!(!copy.exists());
	let status = test.nib(&["status"]).expect("shows");
	assert!(!status.contains("Checkouts"), "{status}");
	assert!(matches!(
		test.nib(&["file", "checkin", "notes.txt"]),
		Err(CliError::Node(NodeError::NotCheckedOut(_)))
	));
}

#[test]
fn an_edited_copy_checks_in_as_a_new_version() {
	let (mut test, _) = with_notes("checkin edited");
	let out = folder("checkin edited, out");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	let copy = out.join("notes.txt");
	fs::write(&copy, "Milk, eggs, bread, butter.\n").expect("edits");

	let status = test.nib(&["status"]).expect("shows");
	assert!(
		short_ids_hidden(&status).ends_with(&format!(
			"Checkouts on this device: 1, 1 edited\nedited  <id>  {}\n",
			copy.display()
		)),
		"{status}"
	);

	// Found by the file's name, as it has one checkout.
	let said = test
		.nib(&["file", "checkin", "notes.txt"])
		.expect("checks in");
	assert_eq!(
		said,
		format!(
			"Recorded the edits of {} as a new version of notes.txt, and deleted the copy\n",
			copy.display()
		)
	);
	assert!(!copy.exists());
	let shown = test.json(&["show", "notes.txt"]);
	assert_eq!(values_of(&shown, "std.size"), [27]);
	let history = test.json(&["history", "notes.txt", "std.content"]);
	assert_eq!(values_of(&history, "std.content").len(), 2, "{history}");

	// The new version is what a checkout writes now; with --keep, the copy stays.
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(&copy).expect("reads"),
		"Milk, eggs, bread, butter.\n"
	);
	fs::write(&copy, "Milk, eggs, bread, butter, jam.\n").expect("edits");
	let kept = test
		.nib(&["file", "checkin", text(&copy), "--keep"])
		.expect("checks in");
	assert_eq!(
		kept,
		format!(
			"Recorded the edits of {} as a new version of notes.txt, and kept the copy\n",
			copy.display()
		)
	);
	assert!(copy.exists());
	let status = test.nib(&["status"]).expect("shows");
	assert!(
		status.ends_with("Checkouts on this device: 1\n"),
		"{status}"
	);
	let kept = test.json(&["file", "checkin", text(&copy), "--keep"]);
	assert_eq!(kept["copy"], "unchanged");
	assert_eq!(kept["version"], serde_json::Value::Null);
	assert!(copy.exists());
	assert!(test.nib(&["verify"]).is_ok());
}

#[test]
fn an_edit_of_an_older_version_is_refused_unless_forced() {
	let (mut test, _) = with_notes("checkin stale");
	let first = folder("checkin stale, first");
	let second = folder("checkin stale, second");
	test.nib(&["file", "checkout", "notes.txt", text(&first)])
		.expect("checks out");
	test.nib(&["file", "checkout", "notes.txt", text(&second)])
		.expect("checks out");
	assert!(matches!(
		test.nib(&["file", "checkin", "notes.txt"]),
		Err(CliError::Node(NodeError::WhichCheckout { .. }))
	));
	fs::write(first.join("notes.txt"), "Milk.\n").expect("edits");
	fs::write(second.join("notes.txt"), "Eggs.\n").expect("edits");
	test.nib(&["file", "checkin", text(&first.join("notes.txt"))])
		.expect("checks in");

	assert!(matches!(
		test.nib(&["file", "checkin", text(&second.join("notes.txt"))]),
		Err(CliError::Stale(_))
	));
	assert!(second.join("notes.txt").exists());
	test.nib(&[
		"file",
		"checkin",
		text(&second.join("notes.txt")),
		"--force",
	])
	.expect("checks in");
	assert!(!second.join("notes.txt").exists());
	let shown = test.json(&["show", "notes.txt"]);
	assert_eq!(values_of(&shown, "std.size"), [6]);
	let history = test.json(&["history", "notes.txt", "std.content"]);
	assert_eq!(values_of(&history, "std.content").len(), 3, "{history}");
}

#[test]
fn a_copy_that_is_gone_ends_its_checkout_or_is_written_again() {
	let (mut test, _) = with_notes("checkout gone");
	let out = folder("checkout gone, out");
	let copy = out.join("notes.txt");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	fs::remove_file(&copy).expect("removes");
	let status = test.nib(&["status"]).expect("shows");
	assert!(
		short_ids_hidden(&status).ends_with(&format!(
			"Checkouts on this device: 1, 1 gone\ngone    <id>  {}\n",
			copy.display()
		)),
		"{status}"
	);

	// Written again, with a record in place of the one before.
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	assert_eq!(fs::read_to_string(&copy).expect("reads"), NOTES);
	let status = test.nib(&["status"]).expect("shows");
	assert!(
		status.ends_with("Checkouts on this device: 1\n"),
		"{status}"
	);

	fs::remove_file(&copy).expect("removes");
	let said = test
		.nib(&["file", "checkin", text(&copy)])
		.expect("checks in");
	assert_eq!(
		said,
		format!(
			"The copy of notes.txt at {} was gone; its checkout ended\n",
			copy.display()
		)
	);
	let status = test.nib(&["status"]).expect("shows");
	assert!(!status.contains("Checkouts"), "{status}");
}

#[test]
fn file_add_takes_no_checkout() {
	let (mut test, _) = with_notes("checkout added");
	let out = folder("checkout added, out");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	let copy = out.join("notes.txt");

	let (said, result) = test.nib_printing(&["file", "add", text(&copy)]);
	assert!(matches!(result, Err(CliError::FilesFailed(1))));
	assert_eq!(
		said,
		format!(
			"failed {}: a checkout; nib file checkin records its edits\nAdded 0 files\n",
			copy.display()
		)
	);
	let said = test.nib(&["file", "add", text(&out)]).expect("adds");
	assert_eq!(
		said,
		"Added 0 files, left out 1 item of folders: links, hidden files and folders, checkouts, and the library's own\n"
	);
	let origins = test.json(&["list", "std.origin"]);
	assert_eq!(origins.as_array().map(Vec::len), Some(1));
}

#[test]
fn purging_a_files_content_destroys_it_and_deletes_its_unchanged_checkouts() {
	let (mut test, files) = with_notes("checkout purged");
	let first = folder("checkout purged, first");
	let second = folder("checkout purged, second");
	test.nib(&["file", "checkout", "notes.txt", text(&first)])
		.expect("checks out");
	test.nib(&["file", "checkout", "notes.txt", text(&second)])
		.expect("checks out");
	fs::write(second.join("notes.txt"), "Eggs.\n").expect("edits");
	assert_eq!(sealed_blobs(&test.here), 1);

	let (said, result) = test.nib_printing(&["purge", "notes.txt", "std.content="]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert_eq!(
		said,
		format!(
			"std.content: 4 values, from 4 changes, and 1 stored copy, would be purged for good\ncheckout  {}, would be deleted\ncheckout  {}, edited, would be kept\noriginal  {}, on laptop, would be kept\n",
			first.join("notes.txt").display(),
			second.join("notes.txt").display(),
			files.join("notes.txt").display(),
		)
	);
	assert!(first.join("notes.txt").exists());

	let said = test
		.nib(&["purge", "notes.txt", "std.content=", "--yes"])
		.expect("purges");
	assert_eq!(
		said,
		format!(
			"std.content: 4 values, from 4 changes, and 1 stored copy, purged for good\ncheckout  {}, deleted\ncheckout  {}, edited, kept\noriginal  {}, on laptop, kept\n",
			first.join("notes.txt").display(),
			second.join("notes.txt").display(),
			files.join("notes.txt").display(),
		)
	);
	assert!(!first.join("notes.txt").exists());
	assert_eq!(
		fs::read_to_string(second.join("notes.txt")).expect("reads"),
		"Eggs.\n"
	);
	assert_eq!(
		fs::read_to_string(files.join("notes.txt")).expect("reads"),
		NOTES
	);
	assert_eq!(sealed_blobs(&test.here), 0);
	let shown = test.json(&["show", "notes.txt"]);
	assert!(values_of(&shown, "std.content").is_empty(), "{shown}");
	let status = test.nib(&["status"]).expect("shows");
	assert!(
		short_ids_hidden(&status).ends_with(&format!(
			"Checkouts on this device: 1, 1 edited\nedited  <id>  {}\n",
			second.join("notes.txt").display()
		)),
		"{status}"
	);
	let export = folder("checkout purged, export");
	let exported = test.json(&["export", text(&export.join("export"))]);
	assert_eq!(exported["files"], 0, "{exported}");
	assert!(test.nib(&["verify"]).is_ok());

	// The edited copy holds the person's work, which checking in records.
	test.nib(&["file", "checkin", text(&second.join("notes.txt"))])
		.expect("checks in");
	let shown = test.json(&["show", "notes.txt"]);
	assert_eq!(values_of(&shown, "std.size"), [6]);
}

#[test]
fn a_file_kept_where_it_is_checks_out_from_its_original() {
	let mut test = library("checkout in place");
	let downloads = folder("checkout in place, downloads");
	test.downloads = Some(downloads.clone());
	let film = downloads.join("film.mkv");
	fs::write(&film, "a film, said to be one").expect("writes");
	let added = test.nib(&["file", "add", text(&film)]).expect("adds");
	assert!(added.contains("kept where it is"), "{added}");
	assert_eq!(sealed_blobs(&test.here), 0);
	let out = folder("checkout in place, out");
	let copy = out.join("film.mkv");

	test.nib(&["file", "checkout", "film.mkv", text(&out)])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(&copy).expect("reads"),
		"a film, said to be one"
	);
	// The original holds the content still, so the copy goes without sealing it.
	let checked_in = test.json(&["file", "checkin", text(&copy)]);
	assert_eq!(checked_in["copy"], "unchanged");
	assert_eq!(checked_in["sealed"], false);
	assert_eq!(sealed_blobs(&test.here), 0);

	// Once the original changes, the copy is the device's last, and goes into the store.
	test.nib(&["file", "checkout", "film.mkv", text(&out)])
		.expect("checks out");
	fs::write(&film, "another film").expect("writes");
	let checked_in = test.json(&["file", "checkin", text(&copy)]);
	assert_eq!(checked_in["sealed"], true);
	assert!(!copy.exists());
	assert_eq!(sealed_blobs(&test.here), 1);
	test.nib(&["file", "checkout", "film.mkv", text(&out)])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(&copy).expect("reads"),
		"a film, said to be one"
	);

	// Without a copy on the device, there is nothing to check out.
	let mut other = library("checkout nowhere");
	other.downloads = Some(downloads.clone());
	other.nib(&["file", "add", text(&film)]).expect("adds");
	fs::write(&film, "a third film").expect("writes");
	assert!(matches!(
		other.nib(&["file", "checkout", "film.mkv", text(&out.join("third.mkv"))]),
		Err(CliError::Node(NodeError::NoCopyHere(_)))
	));
	assert!(!out.join("third.mkv").exists());
}

#[test]
fn an_edit_with_the_size_and_time_recorded_is_still_recorded() {
	let (mut test, _) = with_notes("checkin same second");
	let out = folder("checkin same second, out");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	let copy = out.join("notes.txt");
	// An edit of the same size, in the second the copy was written.
	let written = fs::metadata(&copy)
		.and_then(|metadata| metadata.modified())
		.expect("a time");
	fs::write(&copy, "Milk, eggs, jam!!.\n").expect("edits");
	fs::File::options()
		.write(true)
		.open(&copy)
		.and_then(|file| file.set_modified(written))
		.expect("sets the time");

	let said = test
		.nib(&["file", "checkin", text(&copy)])
		.expect("checks in");
	assert_eq!(
		said,
		format!(
			"Recorded the edits of {} as a new version of notes.txt, and deleted the copy\n",
			copy.display()
		)
	);
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(&copy).expect("reads"),
		"Milk, eggs, jam!!.\n"
	);
}

#[test]
fn a_purge_of_content_takes_every_value_that_holds_it_and_names_them() {
	let (mut test, files) = with_notes("purge every holder");
	let out = folder("purge every holder, out");
	// The file gets a second version, so that the first is a past value of it.
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	fs::write(out.join("notes.txt"), "Eggs.\n").expect("edits");
	test.nib(&["file", "checkin", text(&out.join("notes.txt"))])
		.expect("checks in");
	// Another file with the first version's content, and a book that refers to it.
	fs::write(files.join("copy.txt"), NOTES).expect("writes");
	let added = test
		.nib(&["file", "add", text(&files.join("copy.txt"))])
		.expect("adds");
	assert!(added.starts_with("again"), "{added}");
	fs::write(files.join("other.txt"), "Bread.\n").expect("writes");
	test.nib(&["file", "add", text(&files.join("other.txt"))])
		.expect("adds");
	test.nib(&["type", "add", "book", "title:text", "cover:blob"])
		.expect("adds the type");
	let first = nibkeep_base::Hash::of(NOTES.as_bytes()).to_string();
	test.nib(&["new", "book", "title=Shopping", &format!("cover={first}")])
		.expect("adds the book");

	let purged = test.json(&["purge", "other.txt", "std.content=", "--yes"]);
	assert_eq!(purged["also"].as_array().map(Vec::len), Some(0), "{purged}");
	let (said, _) = test.nib_printing(&["purge", "notes.txt", &format!("std.content={first}")]);
	assert!(
		short_ids_hidden(&said)
			.contains("also      <id>  Shopping, which holds the same content\n"),
		"{said}"
	);
	test.nib(&[
		"purge",
		"notes.txt",
		&format!("std.content={first}"),
		"--yes",
	])
	.expect("purges");
	let book = test.json(&["show", "Shopping"]);
	assert!(values_of(&book, "local.cover").is_empty(), "{book}");
	let history = test.json(&["history", "notes.txt", "std.content"]);
	assert_eq!(values_of(&history, "std.content").len(), 1, "{history}");
	let exported = test.json(&[
		"export",
		text(&folder("purge every holder, export").join("export")),
	]);
	assert_eq!(exported["files"], 1, "{exported}");
	assert!(test.nib(&["verify"]).is_ok());
}

#[test]
fn an_original_of_an_older_version_is_that_file_found_again() {
	let (mut test, files) = with_notes("original of an older version");
	let out = folder("original of an older version, out");
	test.nib(&["file", "checkout", "notes.txt", text(&out)])
		.expect("checks out");
	fs::write(out.join("notes.txt"), "Milk, eggs, bread, butter.\n").expect("edits");
	test.nib(&["file", "checkin", text(&out.join("notes.txt"))])
		.expect("checks in");

	// Unchanged where it was found: known, as what was found there.
	let again = test.json(&["file", "add", text(&files.join("notes.txt"))]);
	assert_eq!(again["files"][0]["known"], true, "{again}");
	assert_eq!(
		again["files"][0]["origin"],
		serde_json::Value::Null,
		"{again}"
	);
	assert_eq!(
		again["files"][0]["content"],
		nibkeep_base::Hash::of(NOTES.as_bytes()).to_string()
	);
	// Touched, it is read again, and found to be the file's first version.
	let original = files.join("notes.txt");
	fs::File::options()
		.write(true)
		.open(&original)
		.and_then(|file| file.set_modified(std::time::SystemTime::UNIX_EPOCH))
		.expect("sets the time");
	let said = test.nib(&["file", "add", text(&original)]).expect("adds");
	assert!(said.starts_with("again"), "{said}");
	let listed = test.json(&["list", "std.file"]);
	assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");
}

#[test]
fn a_copy_whose_folder_is_gone_can_be_named_by_its_path() {
	let (mut test, _) = with_notes("checkout folder gone");
	let out = folder("checkout folder gone, out");
	fs::create_dir(out.join("sub")).expect("makes");
	test.nib(&["file", "checkout", "notes.txt", text(&out.join("sub"))])
		.expect("checks out");
	fs::remove_dir_all(out.join("sub")).expect("removes");
	let copy = out.join("sub").join("notes.txt");
	let said = test
		.nib(&["file", "checkin", text(&copy)])
		.expect("checks in");
	assert_eq!(
		said,
		format!(
			"The copy of notes.txt at {} was gone; its checkout ended\n",
			copy.display()
		)
	);
}

#[test]
fn a_place_written_as_a_folder_is_one() {
	let (mut test, _) = with_notes("checkout into a new folder");
	let out = folder("checkout into a new folder, out");
	let place = format!("{}{}", text(&out.join("new")), std::path::MAIN_SEPARATOR);
	assert!(matches!(
		test.nib(&["file", "checkout", "notes.txt", &place]),
		Err(CliError::Node(NodeError::File { .. }))
	));
	assert_eq!(names_in(&out), Vec::<String>::new());
}

#[test]
fn a_file_with_a_name_as_long_as_names_go_checks_out() {
	let mut test = library("checkout long name");
	let files = folder("checkout long name, files");
	let name = format!("{}.txt", "n".repeat(246));
	fs::write(files.join(&name), NOTES).expect("writes");
	test.nib(&["file", "add", text(&files.join(&name))])
		.expect("adds");
	let out = folder("checkout long name, out");
	test.nib(&["file", "checkout", &name, text(&out)])
		.expect("checks out");
	assert_eq!(names_in(&out), [name]);
}

#[test]
fn a_file_is_pinned_to_a_device_and_unpinned() {
	let (mut test, _) = with_notes("checkouts, pinned");
	assert_eq!(
		test.nib(&["file", "pin", "notes.txt"]).expect("pins"),
		"Pinned notes.txt to laptop, which keeps it whatever its size\n"
	);
	let json = test.json(&["file", "pin", "notes.txt", "--device", "laptop"]);
	assert_eq!(
		(&json["pinned"], &json["device_name"]),
		(&true.into(), &"laptop".into())
	);
	assert_eq!(
		test.nib(&["file", "unpin", "notes.txt"]).expect("unpins"),
		"Unpinned notes.txt from laptop\n"
	);
	// Only files are pinned, and only to a device of yours.
	assert!(matches!(
		test.nib(&["file", "pin", "laptop"]),
		Err(CliError::Node(
			NodeError::NotAFile(_) | NodeError::NoEntity(_)
		))
	));
	assert!(matches!(
		test.nib(&["file", "pin", "notes.txt", "--device", "toaster"]),
		Err(CliError::Node(NodeError::WhichDevice(_)))
	));
}
