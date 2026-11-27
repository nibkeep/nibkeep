//! `nib edit` and `nib find`: changing entities, and finding them by their words.

use crate::common::{Test, ids_hidden, library};

/// A library with a book.
fn with_a_book(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&[
		"type",
		"add",
		"book",
		"title:text",
		"tag:text:many",
		"year:date",
	])
	.expect("adds the type");
	test.nib(&["new", "book", "title=Leviathan Wakes", "tag=space"])
		.expect("adds the book");
	test
}

#[test]
fn edits_set_add_remove_and_clear_values() {
	let mut test = with_a_book("edits");
	let edited = test
		.nib(&[
			"edit",
			"Leviathan Wakes",
			"year=2011",
			"tag+=noir",
			"tag-=space",
		])
		.expect("edits");
	assert_eq!(
		ids_hidden(&edited),
		"\
Leviathan Wakes
  id           <id>
  type         local.book
  local.tag    noir
  local.title  Leviathan Wakes
  local.year   2011
"
	);
	let edited = test
		.nib(&["edit", "Leviathan Wakes", "tag=", "year=2012"])
		.expect("edits");
	assert!(!edited.contains("local.tag"), "{edited}");
	assert!(edited.contains("  local.year   2012\n"), "{edited}");
	let tags = test
		.nib(&["edit", "Leviathan Wakes", "tag=space", "tag=opera"])
		.expect("sets several values");
	assert!(tags.contains("local.tag    "), "{tags}");
	assert_eq!(
		test.json(&["show", "Leviathan Wakes"])["fields"][0]["values"]
			.as_array()
			.map(Vec::len),
		Some(2)
	);
}

#[test]
fn a_second_value_competes_until_one_is_chosen() {
	let mut test = with_a_book("competing");
	let competing = test
		.nib(&["edit", "Leviathan Wakes", "title+=Leviathan wakes"])
		.expect("adds a competing title");
	let line = competing
		.lines()
		.find(|line| line.trim_start().starts_with("local.title"))
		.expect("the title");
	assert!(line.ends_with("(competing)"), "{line}");
	assert!(
		line.contains("Leviathan Wakes") && line.contains("Leviathan wakes"),
		"{line}"
	);
	let chosen = test
		.nib(&["edit", "leviathan wakes", "title=Leviathan Wakes"])
		.expect("chooses");
	assert!(
		chosen.contains("  local.title  Leviathan Wakes\n"),
		"{chosen}"
	);
}

#[test]
fn entities_are_deleted_and_restored() {
	let mut test = with_a_book("deleted");
	let deleted = test
		.nib(&["edit", "Leviathan Wakes", "--delete"])
		.expect("deletes");
	assert!(deleted.contains("  deleted      yes\n"), "{deleted}");
	assert_eq!(test.nib(&["list", "book"]).expect("lists"), "");
	assert_eq!(test.nib(&["find", "leviathan"]).expect("finds"), "");
	let restored = test
		.nib(&["edit", "Leviathan Wakes", "--restore"])
		.expect("restores");
	assert!(!restored.contains("deleted"), "{restored}");
	assert_eq!(
		ids_hidden(&test.nib(&["list", "book"]).expect("lists")),
		"<id>  Leviathan Wakes  local.book\n"
	);
}

#[test]
fn find_lists_what_matches_every_word() {
	let mut test = with_a_book("find");
	test.nib(&["new", "book", "title=Caliban's War", "tag=space"])
		.expect("adds");
	assert_eq!(
		ids_hidden(&test.nib(&["find", "leviathan"]).expect("finds")),
		"<id>  Leviathan Wakes  local.book\n"
	);
	assert_eq!(
		ids_hidden(&test.nib(&["find", "space", "cal"]).expect("finds")),
		"<id>  Caliban's War  local.book\n"
	);
	let found = test.json(&["find", "space"]);
	assert_eq!(found["entities"].as_array().map(Vec::len), Some(2));
	assert_eq!(
		test.json(&["find", "nothing"]),
		serde_json::json!({"entities": [], "more": 0, "at_least": false, "not_looked_in": []})
	);
}

#[test]
fn find_lists_the_first_fifty_and_counts_the_rest() {
	let mut test = with_a_book("find, the first fifty");
	for number in 1..=51 {
		test.nib(&["new", "book", &format!("title=Leviathan {number}")])
			.expect("adds");
	}
	let found = test.nib(&["find", "leviathan"]).expect("finds");
	let lines: Vec<&str> = found.lines().collect();
	assert_eq!(lines.len(), 51, "{found}");
	assert_eq!(
		lines[50], "At least 2 more: add words to narrow it, or list them all with --all.",
		"those found by their words, but not through links, which were not looked for"
	);
	let found = test.json(&["find", "leviathan"]);
	assert_eq!(found["entities"].as_array().map(Vec::len), Some(50));
	assert_eq!(
		(&found["more"], &found["at_least"]),
		(&2.into(), &true.into())
	);
	let all = test.json(&["find", "leviathan", "--all"]);
	assert_eq!(all["entities"].as_array().map(Vec::len), Some(52));
	assert_eq!((&all["more"], &all["at_least"]), (&0.into(), &false.into()));
	let entities = |found: &serde_json::Value| found["entities"].as_array().cloned();
	assert_eq!(
		entities(&all).map(|all| all[..50].to_vec()),
		entities(&found),
		"the same first fifty"
	);
	assert_eq!(
		test.nib(&["find", "leviathan", "--all"])
			.expect("finds")
			.lines()
			.count(),
		52
	);
}

#[test]
fn mistakes_in_edits_are_explained() {
	let mut test = with_a_book("edit mistakes");
	for (arguments, message) in [
		(
			&["edit", "Leviathan Wakes", "tag=noir", "tag="][..],
			"local.tag is both set and cleared",
		),
		(
			&["edit", "Leviathan Wakes", "title=a", "title=b"][..],
			"local.title takes one value",
		),
		(
			&["edit", "Leviathan Wakes", "tag-=never"][..],
			"local.tag has no such value",
		),
		(
			&["edit", "Caliban's War", "title=x"][..],
			"there is no entity Caliban's War",
		),
		(
			&["edit", "Leviathan Wakes", "year+=soon"][..],
			"local.year takes a date, such as 2011, 2011-06 or 1975~",
		),
		(
			&["edit", "Leviathan Wakes", "org.nibkeep.type=other"][..],
			"an entity's types are given with type+=TYPE and taken away with type-=TYPE",
		),
		(
			&["edit", "Leviathan Wakes", "org.nibkeep.deleted=yes"][..],
			"org.nibkeep.deleted is one of NibKeep's own fields, which only its own commands set",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
}

#[test]
fn edits_need_something_to_change() {
	use clap::Parser;

	assert!(nibkeep_cli::Cli::try_parse_from(["nib", "edit", "Leviathan Wakes"]).is_err());
	let mut test = with_a_book("edit something");
	test.nib(&["edit", "Leviathan Wakes", "tag=", "tag="])
		.expect("clears twice");
	assert_eq!(
		test.nib(&["edit", "Leviathan Wakes", "tag+="])
			.expect_err("an empty value")
			.to_string(),
		"a value is field=value, not tag+="
	);
	test.nib(&["new", "person", "name=Ada Lovelace"])
		.expect("adds a person");
	test.nib(&["edit", "Ada Lovelace", "--delete"])
		.expect("deletes");
	test.nib(&["type", "add", "book", "author:reference:many"])
		.expect("adds a field");
	assert_eq!(
		test.nib(&["edit", "Leviathan Wakes", "author+=Ada Lovelace"])
			.expect_err("a deleted entity")
			.to_string(),
		"there is no entity Ada Lovelace"
	);
}
