//! Links: `nib show` lists what links to an entity, and `nib new` notes a relationship
//! recorded before with the same participants. The standard vocabulary gives the types.

mod common;

use common::{Test, ids_hidden, library};

/// A library with the people behind a pen name, the pen name, and the first three Expanse
/// novels credited to it.
fn with_the_expanse(name: &str) -> Test {
	let mut test = library(name);
	for person in ["Daniel Abraham", "Ty Franck"] {
		test.nib(&["new", "person", &format!("name={person}")])
			.expect("adds a person");
	}
	test.nib(&[
		"new",
		"pen-name",
		"name=James S. A. Corey",
		"used-by=Daniel Abraham",
		"used-by=Ty Franck",
	])
	.expect("adds the pen name");
	for book in ["Leviathan Wakes", "Caliban's War", "Abaddon's Gate"] {
		test.nib(&[
			"new",
			"book",
			&format!("name={book}"),
			"author=James S. A. Corey",
		])
		.expect("adds a book");
	}
	test
}

#[test]
fn show_lists_what_links_to_an_entity() {
	let mut test = with_the_expanse("linked from");
	// Values added together, without dates, show in the order of their random IDs.
	let shown = ids_hidden(&test.nib(&["show", "James S. A. Corey"]).expect("shows"))
		.replace("Ty Franck; Daniel Abraham", "Daniel Abraham; Ty Franck");
	assert_eq!(
		shown,
		"\
James S. A. Corey
  id                <id>
  type              std.pen-name
  org.nibkeep.name  James S. A. Corey
  std.used-by       Daniel Abraham; Ty Franck
Linked from
  std.author-of  Abaddon's Gate; Caliban's War; Leviathan Wakes
"
	);
	assert_eq!(
		ids_hidden(&test.nib(&["show", "Daniel Abraham"]).expect("shows")),
		"\
Daniel Abraham
  id                <id>
  type              std.person
  org.nibkeep.name  Daniel Abraham
Linked from
  std.pen-names  James S. A. Corey
"
	);
	// A book links to its author, and nothing links back to it.
	assert!(
		!test
			.nib(&["show", "Leviathan Wakes"])
			.expect("shows")
			.contains("Linked from")
	);

	let shown = test.json(&["show", "James S. A. Corey"]);
	let linked = shown["linked_from"].as_array().expect("links");
	assert_eq!(linked.len(), 1);
	assert_eq!(linked[0]["field"], "std.author");
	assert_eq!(linked[0]["reverse"], "std.author-of");
	assert_eq!(linked[0]["role"], false);
	assert_eq!(linked[0]["count"], 3);
	assert_eq!(linked[0]["entities"][0]["name"], "Abaddon's Gate");
	assert_eq!(
		linked[0]["entities"][0]["types"],
		serde_json::json!(["std.book"])
	);
	assert_eq!(
		linked[0]["entities"][0]["id"],
		test.json(&["show", "Abaddon's Gate"])["id"]
	);
	assert_eq!(
		test.json(&["show", "Leviathan Wakes"])["linked_from"],
		serde_json::json!([])
	);
	assert_eq!(shown["recorded_before"], serde_json::json!([]));
}

#[test]
fn many_links_are_counted() {
	let mut test = library("many links");
	test.nib(&["new", "pen-name", "name=Prolific"])
		.expect("adds the pen name");
	for number in 1..=12 {
		test.nib(&[
			"new",
			"book",
			&format!("name=Book {number:02}"),
			"author=Prolific",
		])
		.expect("adds a book");
	}
	let shown = test.nib(&["show", "Prolific"]).expect("shows");
	let prolific = test.json(&["show", "Prolific"]);
	let short = &prolific["id"].as_str().expect("an ID")[..8];
	assert!(
		shown.ends_with(&format!(
			"Linked from\n  std.author-of  Book 01; Book 02; Book 03; Book 04; Book 05; Book 06; Book 07; Book 08; Book 09; Book 10; and 2 more: nib list --where std.author={short}\n"
		)),
		"{shown}"
	);
	let linked = &prolific["linked_from"][0];
	assert_eq!(linked["count"], 12);
	assert_eq!(linked["entities"].as_array().map(Vec::len), Some(10));
	// The command it names lists them all.
	let all = test
		.nib(&["list", "--where", &format!("std.author={short}")])
		.expect("lists");
	assert_eq!(all.lines().count(), 12, "{all}");
	assert!(all.lines().all(|line| line.contains("std.book")), "{all}");
}

/// A library with a character, an actor and a series, for portrayals.
fn with_a_cast(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["type", "add", "tv-series", "org.nibkeep.name:name"])
		.expect("adds the type");
	test.nib(&["new", "character", "name=Amos Burton"])
		.expect("adds the character");
	test.nib(&["new", "person", "name=Wes Chatham"])
		.expect("adds the actor");
	test.nib(&["new", "tv-series", "name=The Expanse"])
		.expect("adds the series");
	test
}

const PORTRAYAL: [&str; 5] = [
	"new",
	"portrayal",
	"portrayed=Amos Burton",
	"performer=Wes Chatham",
	"production=The Expanse",
];

#[test]
fn a_relationship_is_shown_with_its_other_participants() {
	let mut test = with_a_cast("participants");
	test.nib(&PORTRAYAL).expect("adds the portrayal");
	let shown = test.nib(&["show", "Wes Chatham"]).expect("shows");
	assert!(
		shown.ends_with(
			"Linked from\n  std.performer  std.portrayal with Amos Burton (std.portrayed), The Expanse (std.production)\n"
		),
		"{shown}"
	);
	let linked = &test.json(&["show", "Wes Chatham"])["linked_from"][0];
	assert_eq!(linked["role"], true);
	assert_eq!(linked["reverse"], serde_json::Value::Null);
	let participants = &linked["entities"][0]["participants"];
	assert_eq!(participants[0]["role"], "std.portrayed");
	assert_eq!(
		participants[0]["entities"][0],
		test.json(&["show", "Amos Burton"])["id"]
	);
	assert_eq!(participants[1]["role"], "std.production");
}

#[test]
fn a_relationship_recorded_before_is_noted() {
	let mut test = with_a_cast("recorded before");
	let first = test.nib(&PORTRAYAL).expect("adds the portrayal");
	assert!(!first.contains("before"), "{first}");
	let first_id = test.json(&["show", "Wes Chatham"])["linked_from"][0]["entities"][0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let second = test.nib(&PORTRAYAL).expect("adds it again");
	assert!(
		second.ends_with(&format!(
			"Recorded before with the same participants: {}\n",
			&first_id[..8]
		)),
		"{second}"
	);
	let again = test.json(&PORTRAYAL);
	assert_eq!(again["recorded_before"].as_array().map(Vec::len), Some(2));
	assert_eq!(again["recorded_before"][0].as_str().map(str::len), Some(36));
}

#[test]
fn a_search_finds_works_through_authors_and_pen_names() {
	let mut test = with_the_expanse("search through");
	assert_eq!(
		ids_hidden(&test.nib(&["find", "daniel", "abraham"]).expect("finds")),
		"\
<id>  Daniel Abraham     std.person
<id>  James S. A. Corey  std.pen-name  (via Daniel Abraham)
<id>  Abaddon's Gate     std.book  (via James S. A. Corey)
<id>  Caliban's War      std.book  (via James S. A. Corey)
<id>  Leviathan Wakes    std.book  (via James S. A. Corey)
"
	);
	let found = &test.json(&["find", "daniel", "abraham"])["entities"];
	assert_eq!(found[0]["via"], serde_json::Value::Null);
	assert_eq!(found[1]["via"], found[0]["id"]);
	assert_eq!(found[2]["via"], found[1]["id"]);
	// A list of entities is no search, and finds nothing through links.
	assert_eq!(
		test.json(&["list", "book"])[0]["via"],
		serde_json::Value::Null
	);
}

#[test]
fn the_links_left_out_are_listed_as_shown() {
	let mut test = library("links left out");
	test.nib(&["new", "pen-name", "name=Prolific"])
		.expect("adds the pen name");
	for number in 1..=11 {
		test.nib(&[
			"new",
			"book",
			&format!("name=Book {number:02}"),
			"author=Prolific",
		])
		.expect("adds a book");
	}
	let short = test.json(&["show", "Prolific"])["id"]
		.as_str()
		.expect("an ID")[..8]
		.to_owned();
	let shown = test
		.nib(&["show", "Prolific", "--on", "2026-10-12"])
		.expect("shows on a day");
	assert!(
		shown.contains(&format!(
			"and 1 more: nib list --where std.author={short} --on 2026-10-12\n"
		)),
		"{shown}"
	);
	let as_of = test
		.nib(&["show", "Prolific", "--as-of", "2026-10-12"])
		.expect("shows as recorded");
	assert!(
		as_of.contains("; and 1 more\n"),
		"no command lists them: {as_of}"
	);
	assert_eq!(
		test.nib(&[
			"list",
			"--where",
			&format!("std.author={short}"),
			"--on",
			"2026-10-12"
		])
		.expect("lists on the day")
		.lines()
		.count(),
		11
	);

	// A book merged into its author does not link to itself.
	test.nib(&["merge", "Book 01", "Prolific"]).expect("merges");
	let listed = test
		.nib(&["list", "--where", &format!("std.author={short}")])
		.expect("lists");
	assert_eq!(listed.lines().count(), 10, "{listed}");
	assert!(!listed.contains("std.pen-name"), "{listed}");
}
