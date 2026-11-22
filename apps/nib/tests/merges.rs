//! `nib merge`: entities that turn out to be the same, and merges undone.

mod common;

use clap::Parser as _;
use common::{Test, library};

/// A library with a person recorded twice, and a book credited to the second record.
fn with_duplicates(name: &str) -> Test {
	let mut test = library(name);
	for person in ["Daniel Abraham", "D. Abraham"] {
		test.nib(&["new", "person", &format!("name={person}")])
			.expect("adds a person");
	}
	test.nib(&["new", "book", "name=The Dragon's Path", "author=D. Abraham"])
		.expect("adds a book");
	test
}

/// The line of `nib show` that starts with `field`, trimmed.
fn line(output: &str, field: &str) -> Option<String> {
	output
		.lines()
		.find(|line| line.trim_start().starts_with(field))
		.map(|line| line.trim().to_owned())
}

#[test]
fn merge_shows_both_as_one() {
	let mut test = with_duplicates("merge");
	let main = test.json(&["show", "Daniel Abraham"])["id"].clone();
	let duplicate = test.json(&["show", "D. Abraham"])["id"].clone();
	let merged = test
		.nib(&["merge", "D. Abraham", "Daniel Abraham"])
		.expect("merges");
	// It shows the entity merged into, with both names, which compete.
	let names = line(&merged, "org.nibkeep.name").expect("names");
	assert!(
		names.contains("Daniel Abraham") && names.contains("D. Abraham"),
		"{names}"
	);
	assert!(names.ends_with("(competing)"), "{names}");
	let short = &duplicate.as_str().expect("an ID")[..8];
	assert_eq!(
		line(&merged, "merged").as_deref(),
		Some(format!("merged            D. Abraham ({short})").as_str()),
		"{merged}"
	);
	assert!(
		merged.contains("Linked from\n  std.author-of  The Dragon's Path\n"),
		"{merged}"
	);
	// Either name leads to it.
	assert_eq!(test.json(&["show", "D. Abraham"])["id"], main);
	let shown = test.json(&["show", "Daniel Abraham"]);
	assert_eq!(shown["merged"][0]["id"], duplicate);
	assert_eq!(shown["merged"][0]["name"], "D. Abraham");
	assert_eq!(
		test.json(&["show", "The Dragon's Path"])["merged"],
		serde_json::json!([])
	);
}

#[test]
fn merge_is_undone() {
	let mut test = with_duplicates("undo merge");
	let main = test.json(&["show", "Daniel Abraham"])["id"].clone();
	test.nib(&["merge", "D. Abraham", "Daniel Abraham"])
		.expect("merges");
	let undone = test
		.nib(&["merge", "--undo", "Daniel Abraham"])
		.expect("undoes");
	assert_eq!(
		line(&undone, "org.nibkeep.name").as_deref(),
		Some("org.nibkeep.name  D. Abraham")
	);
	assert_ne!(test.json(&["show", "D. Abraham"])["id"], main);
	assert_eq!(
		test.json(&["show", "Daniel Abraham"])["merged"],
		serde_json::json!([])
	);
}

#[test]
fn merge_undone_of_one_of_several_says_which() {
	let mut test = with_duplicates("undo one of several");
	test.nib(&["new", "person", "name=Abraham, Daniel"])
		.expect("adds a third");
	test.nib(&["merge", "D. Abraham", "Daniel Abraham"])
		.expect("merges");
	test.nib(&["merge", "Abraham, Daniel", "Daniel Abraham"])
		.expect("merges");
	let error = test
		.nib(&["merge", "--undo", "Daniel Abraham"])
		.expect_err("which")
		.to_string();
	assert!(
		error.starts_with("say which entity to set apart from Daniel Abraham: Abraham, Daniel ("),
		"{error}"
	);
	let undone = test
		.nib(&["merge", "--undo", "Daniel Abraham", "d. abraham"])
		.expect("undoes one");
	assert_eq!(
		line(&undone, "org.nibkeep.name").as_deref(),
		Some("org.nibkeep.name  D. Abraham")
	);
	assert_eq!(
		test.json(&["show", "Daniel Abraham"])["merged"]
			.as_array()
			.map(Vec::len),
		Some(1)
	);
}

#[test]
fn merges_that_cannot_be_are_explained() {
	let mut test = with_duplicates("merge mistakes");
	for (arguments, message) in [
		(
			&["merge", "Daniel Abraham", "Daniel Abraham"][..],
			"Daniel Abraham and Daniel Abraham are one entity already",
		),
		(
			&["merge", "--undo", "Daniel Abraham"][..],
			"no entity was merged into Daniel Abraham",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
	test.nib(&["merge", "D. Abraham", "Daniel Abraham"])
		.expect("merges");
	assert_eq!(
		test.nib(&["merge", "--undo", "Daniel Abraham", "Someone"])
			.expect_err("no such entity merged")
			.to_string(),
		"no entity merged into Daniel Abraham is Someone"
	);
	test.nib(&["edit", "The Dragon's Path", "--delete"])
		.expect("deletes");
	let error = test
		.nib(&["merge", "Daniel Abraham", "The Dragon's Path"])
		.expect_err("deleted");
	assert!(error.to_string().contains("The Dragon's Path"), "{error}");
	assert!(nibkeep_cli::Cli::try_parse_from(["nib", "merge", "D. Abraham"]).is_err());
}

#[test]
fn a_merged_entity_is_moved_by_merging_it_again() {
	let mut test = with_duplicates("merge again");
	let main = test.json(&["show", "Daniel Abraham"])["id"].clone();
	let book = test.json(&["show", "The Dragon's Path"])["id"].clone();
	test.nib(&["merge", "D. Abraham", "The Dragon's Path"])
		.expect("merges, by mistake");
	// Its own name names it, not the book it is merged into.
	test.nib(&["merge", "D. Abraham", "Daniel Abraham"])
		.expect("moves it");
	assert_eq!(test.json(&["show", "D. Abraham"])["id"], main);
	assert_eq!(test.json(&["show", "The Dragon's Path"])["id"], book);
	assert_eq!(
		test.json(&["show", "The Dragon's Path"])["merged"],
		serde_json::json!([])
	);
}

#[test]
fn ambiguous_members_of_a_merge_go_by_their_own_names() {
	let mut test = library("ambiguous members");
	let first = test.json(&["new", "person", "name=Le Guin"]);
	let second = test.json(&["new", "person", "name=Le Guin"]);
	test.nib(&["new", "person", "name=Ursula K. Le Guin"])
		.expect("adds a person");
	for member in [&first, &second] {
		let id = member["id"].as_str().expect("an ID");
		test.nib(&["merge", id, "Ursula K. Le Guin"])
			.expect("merges");
	}
	let error = test
		.nib(&["merge", "--undo", "Ursula K. Le Guin", "Le Guin"])
		.expect_err("either")
		.to_string();
	for member in [&first, &second] {
		let short = &member["id"].as_str().expect("an ID")[..8];
		assert!(
			error.contains(&format!("  {short}  Le Guin  std.person")),
			"{error}"
		);
	}
}
