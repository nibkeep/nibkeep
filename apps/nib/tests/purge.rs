//! `nib purge`: values removed for good, once confirmed.

mod common;

use common::library;
use nibkeep_cli::CliError;

#[test]
fn a_purge_says_what_it_would_do_and_does_it_once_confirmed() {
	let mut test = library("purge");
	test.nib(&["type", "add", "book", "title:text", "note:text"])
		.expect("adds the type");
	test.nib(&["new", "book", "title=Leviathan wakes", "note=lent to Grace"])
		.expect("adds the book");
	test.nib(&["edit", "Leviathan wakes", "title=Leviathan Wakes"])
		.expect("edits");

	let (said, result) = test.nib_printing(&["purge", "Leviathan Wakes", "title=Leviathan wakes"]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert_eq!(
		said,
		"local.title: 1 value, from 1 change, would be purged for good\n"
	);
	let shown = test.nib(&["show", "Leviathan Wakes"]).expect("shows");
	assert!(shown.contains("local.title  Leviathan Wakes"), "{shown}");

	let purged = test
		.nib(&["purge", "Leviathan Wakes", "title=Leviathan wakes", "--yes"])
		.expect("purges");
	assert_eq!(
		purged,
		"local.title: 1 value, from 1 change, purged for good\n"
	);
	let shown = test.nib(&["show", "Leviathan Wakes"]).expect("shows");
	assert!(shown.contains("local.title  Leviathan Wakes"), "{shown}");
	assert!(shown.contains("local.note   lent to Grace"), "{shown}");
	assert!(matches!(
		test.nib(&["purge", "Leviathan Wakes", "title=Leviathan wakes", "--yes"]),
		Err(CliError::Node(nibkeep_node::NodeError::NoValue(_)))
	));

	// Every value the field has held.
	let all = test.json(&["purge", "Leviathan Wakes", "title=", "--yes"]);
	assert_eq!(all["values"], 1);
	assert_eq!(all["purged"], true);
	assert!(test.nib(&["verify"]).is_ok());
}
