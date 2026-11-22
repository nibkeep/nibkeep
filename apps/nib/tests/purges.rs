//! `nib purges`: on an archive node, a purge waits before what it destroyed goes for good,
//! and can be stopped, which brings it back, or its wait ended ([technical design: copies
//! that wait]).
//!
//! [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::library;
use nibkeep_cli::CliError;
use nibkeep_node::NodeError;

const NOTES: &str = "Milk.\nBread.\n";

/// An empty folder for one test.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn text(path: &Path) -> &str {
	path.to_str().expect("a path in UTF-8")
}

#[test]
fn a_purge_on_an_archive_node_waits_and_is_stopped_or_ended() {
	let mut test = library("purges wait");
	let files = folder("purges wait, files");
	fs::write(files.join("notes.txt"), NOTES).expect("writes");
	test.nib(&["file", "add", text(&files.join("notes.txt"))])
		.expect("adds");
	assert_eq!(
		test.nib(&["purges"]).expect("lists"),
		"This device destroys what a purge destroys at once: only an archive node waits (nib device mark --archive).\nNo purge waits here.\n"
	);
	test.nib(&["device", "mark", "laptop", "--archive"])
		.expect("marks");

	// The purge says first that this device waits.
	let (said, result) = test.nib_printing(&["purge", "notes.txt", "std.content="]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert!(
		said.contains("waits     on this device, an archive node, for 30 days after it learns of the purge\nUntil then, its owner can stop the purge there with nib purges"),
		"{said}"
	);
	let purged = test.json(&["purge", "notes.txt", "std.content=", "--yes"]);
	assert_eq!(purged["waiting"][0]["days"], 30);
	assert!(
		test.nib(&[
			"file",
			"checkout",
			"notes.txt",
			text(&folder("purges wait, gone"))
		])
		.is_err()
	);

	let listed = test.nib(&["purges"]).expect("lists");
	assert!(
		listed.starts_with("This device waits 30 days before it destroys what a purge destroys.\n"),
		"{listed}"
	);
	assert!(listed.contains(", waits until "), "{listed}");
	assert!(listed.contains("1 stored file"), "{listed}");
	let json = test.json(&["purges"]);
	assert_eq!(json["waits_days"], 30);
	let purge = json["purges"][0]["purge"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let short = &purge[..12];

	// Stopping it, once confirmed, brings the file back.
	let (said, result) = test.nib_printing(&["purges", "stop", short]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert!(said.starts_with("Would stop the purge "), "{said}");
	let said = test
		.nib(&["purges", "stop", short, "--yes"])
		.expect("stops");
	assert!(said.starts_with("Stopped the purge "), "{said}");
	let back = folder("purges wait, back");
	test.nib(&["file", "checkout", "notes.txt", text(&back)])
		.expect("checks out");
	assert_eq!(
		fs::read_to_string(back.join("notes.txt")).expect("reads"),
		NOTES
	);
	let listed = test.json(&["purges"]);
	nibkeep_testkit::assert_empty!(listed["purges"].as_array().expect("a list"));

	// Purged again, its wait ended, it is gone for good.
	test.nib(&["purge", "notes.txt", "std.content=", "--yes"])
		.expect("purges again");
	let purge = test.json(&["purges"])["purges"][0]["purge"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let (said, result) = test.nib_printing(&["purges", "end", &purge]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert!(said.starts_with("Would destroy now "), "{said}");
	let said = test.nib(&["purges", "end", &purge, "--yes"]).expect("ends");
	assert!(said.starts_with("Ended the wait of the purge "), "{said}");
	assert!(matches!(
		test.nib(&["purges", "stop", &purge, "--yes"]),
		Err(CliError::Node(NodeError::WhichPurge(_)))
	));
}
