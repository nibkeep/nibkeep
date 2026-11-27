//! `nib file add`: files and folders added, content stored once, and photos that say when
//! and where they were taken.

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::library;
use nibkeep_cli::CliError;
use nibkeep_testkit::media::Photo;

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn christmas() -> Vec<u8> {
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken: "2019:12:24 18:30:00",
		offset: Some("+01:00"),
		size: (4032, 3024),
		orientation: None,
		place: Some(([59, 19, 48], [18, 4, 12])),
	}
	.jpeg()
}

/// The output with the short ID after the first word of each line replaced by `<id>`.
fn short_ids_hidden(output: &str) -> String {
	output
		.lines()
		.map(|line| match line.split_once("  ") {
			Some((what, rest)) if rest.len() > 8 && rest.as_bytes()[8] == b' ' => {
				format!("{what}  <id>{}\n", &rest[8..])
			}
			_ => format!("{line}\n"),
		})
		.collect()
}

fn text(path: &Path) -> &str {
	path.to_str().expect("UTF-8")
}

#[test]
fn a_folder_is_added_file_by_file_and_its_copies_are_found_again() {
	let mut test = library("files added");
	let files = folder("files added");
	fs::create_dir_all(files.join("2019")).expect("makes");
	fs::create_dir_all(files.join(".thumbnails")).expect("makes");
	fs::write(files.join("2019").join("IMG_2041.JPG"), christmas()).expect("writes");
	fs::write(files.join("2019").join("IMG_2041 (1).JPG"), christmas()).expect("writes");
	fs::write(files.join("notes.txt"), "Milk, eggs, bread.\n").expect("writes");
	fs::write(files.join(".thumbnails").join("small.jpg"), "not added").expect("writes");
	let said = test.nib(&["file", "add", text(&files)]).expect("adds");
	// Made again part by part, as the paths shown are written with the system's separator.
	let shown =
		|path: &str| text(&files.join(Path::new(path).iter().collect::<PathBuf>())).to_owned();
	// The copy comes first by name, so the original is found again, as the copy.
	assert_eq!(
		short_ids_hidden(&said),
		format!(
			"added  <id>  photo  {}\nagain  <id>  photo  {}, as IMG_2041 (1).JPG\nadded  <id>  file   {}\nAdded 2 files, found 1 again in other places, could not read 1 picture for thumbnails, left out 1 item of folders: links, hidden files and folders, checkouts, and the library's own\n",
			shown("2019/IMG_2041 (1).JPG"),
			shown("2019/IMG_2041.JPG"),
			shown("notes.txt"),
		)
	);
	// Adding the folder again adds nothing.
	let again = test.nib(&["file", "add", text(&files)]).expect("adds");
	assert!(
		again.ends_with(
			"Added 0 files, knew 3 already, left out 1 item of folders: links, hidden files and folders, checkouts, and the library's own\n"
		),
		"{again}"
	);

	let shown = test.nib(&["show", "IMG_2041 (1).JPG"]).expect("shows");
	assert!(shown.contains("std.photo"), "{shown}");
	assert!(shown.contains("std.camera-model  iPhone 12 Pro"), "{shown}");
	assert!(shown.contains("2019-12-24T18:30:00+01"), "{shown}");
	assert!(shown.contains("std.origins"), "{shown}");
}

#[test]
fn the_files_of_the_library_s_folder_are_added_and_its_own_never() {
	let mut test = library("files of the library");
	let files = test.here.clone();
	fs::create_dir_all(files.join("Covers")).expect("makes");
	fs::write(files.join("notes.txt"), "Milk.\n").expect("writes");
	fs::write(files.join("Covers").join("Dune.jpg"), christmas()).expect("writes");
	let said = test.nib(&["file", "add", text(&files)]).expect("adds");
	assert!(
		said.ends_with(
			"Added 2 files, could not read 1 picture for thumbnails, left out 1 item of folders: links, hidden files and folders, checkouts, and the library's own\n"
		),
		"{said}"
	);
	// What the library keeps, given, or reached by `..`, fails.
	let own = files.join(".nib");
	for given in [
		own.clone(),
		files.join("Covers").join("..").join(".nib"),
		nibkeep_node::Library::replica_folder(&own)
			.expect("a replica")
			.join("keys.cbor"),
	] {
		let (said, result) = test.nib_printing(&["file", "add", text(&given)]);
		assert!(
			matches!(result, Err(CliError::FilesFailed(1))),
			"{result:?}"
		);
		assert!(said.contains("the library's own folder holds it"), "{said}");
	}
}

#[cfg(unix)]
#[test]
fn the_library_s_own_folder_is_left_out_wherever_a_link_puts_it() {
	let mut test = library("the library's own folder, linked");
	// What the library keeps is in a folder with a plain name, which `.nib` links to.
	let elsewhere = folder("the library's own folder, linked");
	fs::rename(test.here.join(".nib"), elsewhere.join("data")).expect("moves");
	std::os::unix::fs::symlink(elsewhere.join("data"), test.here.join(".nib")).expect("links");
	fs::write(elsewhere.join("notes.txt"), "Milk.\n").expect("writes");
	let said = test.nib(&["file", "add", text(&elsewhere)]).expect("adds");
	assert!(
		said.ends_with("Added 1 file, left out 1 item of folders: links, hidden files and folders, checkouts, and the library's own\n"),
		"{said}"
	);
}

#[test]
fn files_that_cannot_be_added_are_reported_and_the_others_added() {
	let mut test = library("files failed");
	let files = folder("files failed");
	fs::write(files.join("notes.txt"), "Milk, eggs, bread.\n").expect("writes");
	let missing = files.join("missing.txt");
	let (said, result) = test.nib_printing(&[
		"file",
		"add",
		text(&missing),
		text(&files.join("notes.txt")),
	]);
	assert!(
		matches!(result, Err(CliError::FilesFailed(1))),
		"{result:?}"
	);
	assert!(
		said.starts_with(&format!("failed {}: ", missing.display())),
		"{said}"
	);
	assert!(said.contains("added "), "{said}");
	assert!(said.contains("Added 1 file"), "{said}");
	// A path given that is neither a file nor a folder.
	#[cfg(unix)]
	{
		let fifo = files.join("pipe");
		let made = std::process::Command::new("mkfifo")
			.arg(&fifo)
			.status()
			.expect("runs mkfifo");
		assert!(made.success());
		let (said, result) = test.nib_printing(&["file", "add", text(&fifo)]);
		assert!(
			matches!(result, Err(CliError::FilesFailed(1))),
			"{result:?}"
		);
		assert!(said.contains("neither a file nor a folder"), "{said}");
		fs::remove_file(&fifo).expect("removes");
	}

	let json = test.json(&["file", "add", text(&files)]);
	assert_eq!(json["files"][0]["known"], true);
	assert_eq!(json["files"][0]["origin"], serde_json::Value::Null);
	assert_eq!(json["files"][0]["types"][0], "std.file");
	assert!(
		json["files"][0]["content"]
			.as_str()
			.is_some_and(|hash| hash.starts_with("blake3:"))
	);
}
