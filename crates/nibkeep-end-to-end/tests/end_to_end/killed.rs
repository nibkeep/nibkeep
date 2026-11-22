//! A command killed while it writes, as a crash or the system's task manager ends a program:
//! the library opens after, sound, with every file that the command said it added, and the
//! command run again finishes the work.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use nibkeep_end_to_end::{Device, EndToEnd};
use nibkeep_testkit::TestRng;

/// How many files are added: enough that a debug build takes seconds to add them all.
const FILES: usize = 200;

/// The names of the files that a command said it added, by what it printed, such as
/// `added  1a2b3c4d  file   notes/note 001.txt`. It says so once the file is saved.
fn added(printed: &str) -> Vec<String> {
	printed
		.lines()
		.filter_map(|line| {
			let rest = line.strip_prefix("added")?.trim_start();
			let (_id, rest) = rest.split_once(' ')?;
			let (_kind, path) = rest.trim_start().split_once(' ')?;
			Path::new(path.trim_start())
				.file_name()
				.map(|name| name.to_string_lossy().into_owned())
		})
		.collect()
}

/// The names of the files that the library holds, and how many entities hold them.
fn listed(laptop: &Device) -> (BTreeSet<String>, usize) {
	let listed = laptop.nib(&["--json", "list", "file"]).ok().json();
	let files = listed.as_array().expect("a list");
	let names = files
		.iter()
		.map(|file| file["name"].as_str().expect("a name").to_owned())
		.collect();
	(names, files.len())
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn adding_files_keeps_what_it_said_it_added_when_killed() {
	let test = EndToEnd::new("killed");
	let laptop = test.device("laptop");
	laptop
		.nib(&["init", "--name", "Ada", "--languages", "en"])
		.ok();
	let notes = laptop.library().join("notes");
	fs::create_dir(&notes).expect("makes a folder");
	let mut rng = TestRng::new("killed");
	for number in 0..FILES {
		fs::write(
			notes.join(format!("note {number:03}.txt")),
			rng.bytes::<16384>(),
		)
		.expect("writes");
	}

	// Killed once it said it added its first file, and then halfway through what was left;
	// each time, the library opens, is sound, and holds every file it said it added.
	let mut said = BTreeSet::new();
	for round in 0..2 {
		let (_, entities) = listed(&laptop);
		assert!(entities <= FILES, "{entities} entities for {FILES} files");
		let left = FILES - entities;
		let after = if round == 0 { 1 } else { left / 2 };
		if after == 0 {
			break;
		}
		let mut adding = laptop.start(&["file", "add", "notes"]);
		adding.wait_until(&format!("{after} files added"), |printed| {
			(added(printed).len() >= after).then_some(())
		});
		said.extend(added(&adding.kill().stdout));
		laptop.nib(&["verify"]).ok();
		let (names, _) = listed(&laptop);
		let lost: Vec<_> = said.difference(&names).collect();
		assert!(lost.is_empty(), "said it added, but lost: {lost:?}");
	}

	// Run again, it adds what is left, and every file once.
	laptop.nib(&["file", "add", "notes"]).ok();
	let (names, entities) = listed(&laptop);
	assert_eq!(names.len(), FILES, "{names:?}");
	assert_eq!(entities, FILES, "one entity a file");
}
