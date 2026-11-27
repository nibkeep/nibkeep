//! `nib policy`: the policies of the personal space, their rules, and what `nib file add`
//! does with them ([technical design: storage policies]).
//!
//! [technical design: storage policies]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#93-storage-policies

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{Test, ids_hidden, library};
use nibkeep_cli::CliError;

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::write(&path, bytes).expect("writes");
	path
}

/// A library whose device has a downloads folder.
fn with_downloads(name: &str) -> (Test, PathBuf) {
	let mut test = library(name);
	let downloads = folder(&format!("{name} downloads"));
	test.downloads = Some(downloads.clone());
	(
		test,
		nibkeep_node::resolved_path(&downloads).expect("resolves"),
	)
}

#[test]
fn the_default_policies_are_listed() {
	let (mut test, downloads) = with_downloads("policies listed");
	assert_eq!(
		test.nib(&["policy", "list"]).expect("lists"),
		format!(
			"\
Policies:
  Critical     3 copies, 1 outside the home, in the store
  Replaceable  1 copy, kept where it is
  Standard     2 copies, 1 on an archive node, in the store
Rules, in the order in which they choose:
  1. a file with std.camera-make: Standard
  2. a file found in {} on laptop: Replaceable
  3. any other file: Standard, asking above 1 GiB
",
			downloads.display()
		)
	);
	let listed = test.json(&["policy", "list"]);
	assert_eq!(listed["policies"].as_array().map(Vec::len), Some(3));
	assert_eq!(listed["rules"][2]["asks_above"], 1_u64 << 30);
	assert_eq!(listed["rules"][0]["has"], "std.camera-make");
	assert_eq!(listed["policies"][1]["needs"]["kept"], "in-place");
}

#[test]
fn policies_are_added_changed_and_shown() {
	let mut test = library("policies changed");
	assert_eq!(
		test.nib(&["policy", "new", "Films", "--copies", "2", "--in-place"])
			.expect("adds"),
		"Added Films: 2 copies, kept where it is\n"
	);
	assert_eq!(
		test.nib(&["policy", "edit", "films", "--archived", "1", "--store"])
			.expect("changes"),
		"Changed Films: 2 copies, 1 on an archive node, in the store\n"
	);
	test.nib(&["policy", "rule", "new", "Films", "--media-type", "video/"])
		.expect("adds a rule");
	assert_eq!(
		ids_hidden(&test.nib(&["policy", "show", "Films"]).expect("shows")),
		"\
<id>  Films
  2 copies, 1 on an archive node, in the store
  2. a file of media type video/: Films
"
	);
	assert!(matches!(
		test.nib(&["policy", "show", "Nothing"]),
		Err(CliError::Node(nibkeep_node::NodeError::NoEntity(_)))
	));
}

#[test]
fn rules_are_added_moved_and_removed() {
	let mut test = library("policies rules");
	// Before the rule for any other file, by default.
	assert_eq!(
		test.nib(&[
			"policy",
			"rule",
			"new",
			"Replaceable",
			"--type",
			"photo",
			"--larger-than",
			"500MB",
		])
		.expect("adds"),
		"Added rule 2. a file of type std.photo, larger than 500 MB: Replaceable\n"
	);
	assert_eq!(
		test.nib(&["policy", "rule", "move", "2", "--last"])
			.expect("moves"),
		"Moved rule 3. a file of type std.photo, larger than 500 MB: Replaceable\n"
	);
	assert_eq!(
		test.nib(&["policy", "rule", "move", "3", "--before", "1"])
			.expect("moves"),
		"Moved rule 1. a file of type std.photo, larger than 500 MB: Replaceable\n"
	);
	assert!(
		test.nib(&["policy", "rule", "remove", "1"])
			.expect("removes")
			.starts_with("Removed rule ")
	);
	assert_eq!(
		test.nib(&["policy", "list"])
			.expect("lists")
			.lines()
			.skip(5)
			.collect::<Vec<_>>(),
		[
			"  1. a file with std.camera-make: Standard",
			"  2. any other file: Standard, asking above 1 GiB",
		]
	);
	assert!(matches!(
		test.nib(&["policy", "rule", "move", "1"]),
		Err(CliError::WhereTo)
	));
	assert!(matches!(
		test.nib(&["policy", "rule", "remove", "7"]),
		Err(CliError::NoRule(rule)) if rule == "7"
	));
}

#[test]
fn a_download_is_kept_where_it_is_and_checked() {
	let (mut test, downloads) = with_downloads("policies downloads");
	let film = write(&downloads, "film.mkv", b"a film, said to be one");
	let film_path = film.to_string_lossy().into_owned();
	let added = test.nib(&["file", "add", &film_path]).expect("adds");
	let lines: Vec<&str> = added.lines().collect();
	assert!(
		lines[0].starts_with("added ")
			&& lines[0].ends_with(&format!("{film_path}, kept where it is")),
		"{added}"
	);
	assert_eq!(
		lines[1],
		"Added 1 file, kept 1 where it was found, as its policy says"
	);
	assert_eq!(
		test.nib(&["policy", "why", "film.mkv"]).expect("tells"),
		format!(
			"\
Replaceable, by rule 2. a file found in {} on laptop: Replaceable
  1 copy, kept where it is
",
			downloads.display()
		)
	);
	assert_eq!(
		test.nib(&["policy", "check"]).expect("checks"),
		"\
Replaceable: 1 file, every one has its copies; it needs 1 copy, kept where it is
The space's changes: on 1 disk, none outside the home; Critical asks for 3 copies, 1 outside the home, in the store
Disks:
  this disk: nib on laptop (this one)
"
	);
	let verified = test.nib(&["verify"]).expect("verifies");
	assert!(
		verified.contains("  1 file kept where it was found, unchanged\n"),
		"{verified}"
	);
	// The film is on this device's disk, where it was found.
	assert_eq!(
		test.nib(&["file", "where", "film.mkv"]).expect("tells"),
		"film.mkv: on 1 disk\n  this disk: nib on laptop (this one): a plain copy, unchanged, checked 2026-10-12 10:00\n"
	);
	let copies = test.json(&["file", "where", "film.mkv"]);
	assert_eq!(copies["copies"][0]["held"], "in place");
	assert_eq!(copies["copies"][0]["disk"]["here"], true);
	assert_eq!(copies["copies"][0]["counts"], true);

	// Its own policy wins, until it is cleared.
	assert_eq!(
		test.nib(&["policy", "set", "film.mkv", "Critical"])
			.expect("sets"),
		"Critical, its own\n  3 copies, 1 outside the home, in the store\n"
	);
	let check = test.nib(&["policy", "check"]).expect("checks");
	assert!(
		check.contains(
			"Critical: 1 file, each lacks copies; it needs 3 copies, 1 outside the home, in the store\n  1 on too few disks; 1 with too few outside the home\n  Kept only where they were found, though the policy keeps them in the store; nib file add stores them: film.mkv\n"
		),
		"{check}"
	);
	assert!(
		test.nib(&["policy", "set", "film.mkv", "--clear"])
			.expect("clears")
			.starts_with("Replaceable, by rule 2.")
	);

	// Changed by another app, it no longer counts.
	fs::write(&film, b"another film altogether, longer").expect("writes");
	let check = test.json(&["policy", "check"]);
	assert_eq!(
		check["policies"][0]["none_here"].as_array().map(Vec::len),
		Some(1)
	);
	let verified = test.nib(&["verify"]).expect("verifies, sound");
	assert!(
		verified.contains(&format!(
			"  {film_path} changed, went or cannot be read since it was added, and does not count as a copy\n"
		)),
		"{verified}"
	);
}

#[test]
fn a_large_file_is_asked_about_once_for_its_folder() {
	let mut test = library("policies asks");
	test.nib(&[
		"policy",
		"rule",
		"new",
		"Standard",
		"--asks-above",
		"10B",
		"--first",
	])
	.expect("adds");
	let films = folder("policies asks films");
	let first = write(&films, "first.mkv", b"a film, said to be one");
	let second = write(&films, "second.mkv", b"another film, also long");
	test.answers = Some(VecDeque::from(["y"]));
	let added = test
		.nib(&["file", "add", &films.to_string_lossy()])
		.expect("adds");
	assert_eq!(test.questions.len(), 1, "{:?}", test.questions);
	assert!(
		test.questions[0].contains("first.mkv is 22 bytes. Can you get it again if it is lost?"),
		"{}",
		test.questions[0]
	);
	assert_eq!(
		added
			.lines()
			.filter(|line| line.ends_with(", kept where it is"))
			.count(),
		2,
		"{added}"
	);
	assert!(
		test.nib(&["policy", "list"])
			.expect("lists")
			.contains("  1. a file found in ")
	);

	// Without a terminal, nothing is asked, and the file goes into the store.
	let others = folder("policies asks others");
	let third = write(&others, "third.mkv", b"a third film, long too");
	test.answers = None;
	let added = test
		.nib(&["file", "add", &third.to_string_lossy()])
		.expect("adds");
	assert!(
		added.contains(
			"1 large file went into the store without asking whether it can be got again"
		),
		"{added}"
	);
	let fourth = write(&others, "fourth.mkv", b"a fourth film, long too");
	let added = test.json(&["file", "add", &fourth.to_string_lossy()]);
	assert_eq!(added["files"][0]["kept"], "store");
	assert_eq!(added["files"][0]["unasked"], true);
	let _ = (first, second);
}

#[test]
fn nibkeep_own_fields_and_types_are_refused() {
	let mut test = library("policies refused");
	test.nib(&["policy", "list"]).expect("installs");
	assert!(matches!(
		test.nib(&["new", "org.nibkeep.storage-policy"]),
		Err(CliError::Node(nibkeep_node::NodeError::OwnType(_)))
	));
	assert!(matches!(
		test.nib(&["edit", "Critical", "--delete"]),
		Err(CliError::Node(nibkeep_node::NodeError::OwnType(_)))
	));
	assert_eq!(
		test.nib(&["policy", "remove", "Critical"])
			.expect("removes"),
		"Removed Critical\n"
	);
	assert!(
		!test
			.nib(&["policy", "list"])
			.expect("lists")
			.contains("Critical")
	);
}

#[test]
fn a_copy_counts_while_it_was_checked_in_the_last_eight_weeks() {
	let (mut test, downloads) = with_downloads("policies eight weeks");
	let film = write(&downloads, "film.mkv", b"a film, kept for a while");
	test.nib(&["file", "add", &film.to_string_lossy()])
		.expect("adds");
	// Nothing checks the film for eight weeks, as no program syncs this device.
	test.now += 57 * 86_400_000;
	let checked = test.nib(&["policy", "check"]).expect("checks");
	assert!(
		checked.contains(
			"Replaceable: 1 file, each lacks copies; it needs 1 copy, kept where it is\n  1 on too few disks\n  1 has a copy not checked in 8 weeks, which does not count\n"
		),
		"{checked}"
	);
	let place = test.nib(&["file", "where", "film.mkv"]).expect("tells");
	assert!(
		place.ends_with(
			": a plain copy, unchanged, checked 2026-10-12 10:00, so it does not count\n"
		),
		"{place}"
	);
	assert!(
		test.nib(&["status"])
			.expect("describes")
			.contains("Checks: none yet. The program that syncs this device checks its copies: nib sync --serve, or syncing from login.\n")
	);
	// Checking the library checks it, and it counts again.
	test.nib(&["verify"]).expect("verifies");
	assert!(
		test.nib(&["policy", "check"])
			.expect("checks")
			.starts_with("Replaceable: 1 file, every one has its copies;")
	);
}
