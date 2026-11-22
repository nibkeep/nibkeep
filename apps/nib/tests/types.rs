//! `nib type`: types and their fields.

mod common;

use common::Test;
use serde_json::json;

/// A library to work in, unlocked by the mock keystore.
fn library(name: &str) -> Test {
	let mut test = Test::with_keystore(name);
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	test
}

const BOOK: &str = "\
local.book, version 1
  local.author  reference, many, role
  local.title   text
";

#[test]
fn types_are_added_and_shown() {
	let mut test = library("types are added");
	assert_eq!(
		test.nib(&[
			"type",
			"add",
			"book",
			"title:text",
			"author:reference:many:role"
		])
		.expect("adds"),
		BOOK
	);
	assert_eq!(test.nib(&["type", "show", "book"]).expect("shows"), BOOK);
	assert_eq!(
		test.nib(&["type", "show", "local.book"]).expect("shows"),
		BOOK
	);
	assert_eq!(
		test.nib(&["type", "add", "book", "year:date", "isbn:text:lww"])
			.expect("adds fields"),
		"\
local.book, version 2
  local.author  reference, many, role
  local.isbn    text, last writer wins
  local.title   text
  local.year    date
"
	);
	assert_eq!(
		test.nib(&["type", "add", "login", "password:text:secret"])
			.expect("adds"),
		"local.login, version 1\n  local.password  text, secret\n"
	);
	assert_eq!(
		test.nib(&["type", "add", "movie"]).expect("adds"),
		"local.movie, version 1\n"
	);
}

#[test]
fn types_are_listed() {
	let mut test = library("types are listed");
	test.nib(&["type", "add", "book", "title:text"])
		.expect("adds");
	assert_eq!(
		test.nib(&["type", "list"]).expect("lists"),
		"\
local.book                    1 field
org.nibkeep.backup            6 fields
org.nibkeep.device            7 fields
org.nibkeep.field-definition  12 fields
org.nibkeep.group             1 field
org.nibkeep.recovery-code     7 fields
org.nibkeep.replica           20 fields
org.nibkeep.space             1 field
org.nibkeep.space-names       2 fields
org.nibkeep.storage-policy    5 fields
org.nibkeep.storage-rule      9 fields
org.nibkeep.type-definition   4 fields
org.nibkeep.user              5 fields
std.audiobook                 3 fields   Audiobook
std.book                      9 fields   Book
std.character                 9 fields   Character
std.checkout                  6 fields   Checkout
std.edition                   8 fields   Edition
std.episode                   17 fields  Episode
std.file                      13 fields  File
std.film                      16 fields  Film
std.folder                    3 fields   Folder
std.game                      16 fields  Game
std.git-repository            8 fields   Git repository
std.login                     8 fields   Login
std.organization              5 fields   Organization
std.origin                    10 fields  Where it was found
std.pen-name                  2 fields   Pen name
std.person                    13 fields  Person
std.photo                     8 fields   Photo
std.play                      8 fields   Play
std.portrayal                 4 fields   Portrayal
std.revision                  3 fields   Revision
std.season                    7 fields   Season
std.secret-file               10 fields  Secret file
std.series                    16 fields  Series
std.source                    2 fields   Source
std.symbolic-link             5 fields   Symbolic link
std.thumbnail                 7 fields   Thumbnail
std.tree                      2 fields   Tree
std.tree-root                 3 fields   Tree's folder
std.video                     9 fields   Video
"
	);
}

#[test]
fn types_have_a_json_form() {
	let mut test = library("types in JSON");
	test.nib(&[
		"type",
		"add",
		"book",
		"title:text",
		"author:reference:many:role",
	])
	.expect("adds");
	let shown = test.json(&["type", "show", "book"]);
	assert_eq!(
		shown,
		json!({
			"name": "local.book",
			"label": null,
			"labels": {},
			"version": 1,
			"fields": [
				{ "name": "local.author", "label": null, "labels": {}, "kind": "reference", "multiplicity": "many", "secret": false, "role": true, "stands_for": false, "search_through": false, "code": false, "unit": null, "reverse": null },
				{ "name": "local.title", "label": null, "labels": {}, "kind": "text", "multiplicity": "one", "secret": false, "role": false, "stands_for": false, "search_through": false, "code": false, "unit": null, "reverse": null },
			],
		})
	);
	let listed = test.json(&["type", "list"]);
	assert_eq!(
		listed[0],
		json!({ "name": "local.book", "label": null, "labels": {}, "version": 1, "fields": 2 })
	);
	assert_eq!(listed.as_array().map(Vec::len), Some(42));
	let grown = test.json(&["type", "add", "book", "isbn:text:lww"]);
	assert_eq!(grown["version"], 2);
	assert_eq!(grown["fields"][1]["name"], "local.isbn");
	assert_eq!(grown["fields"][1]["multiplicity"], "lww");
}

#[test]
fn mistakes_are_explained() {
	let mut test = library("type mistakes");
	test.nib(&["type", "add", "book", "title:text"])
		.expect("adds");
	for (arguments, message) in [
		(
			&["type", "show", "novel"][..],
			"there is no type local.novel; define it with nib type add",
		),
		(
			&["type", "add", "book", "title"][..],
			"a field is name:kind, with flags such as :many, :lww, :secret or :role after it, not title",
		),
		(
			&["type", "add", "book", "title:words"][..],
			"there is no kind of value called words; the kinds are text, integer, boolean, date, reference, blob, decimal, quantity and name",
		),
		(
			&["type", "add", "book", "title:text:often"][..],
			"there is no flag often for a field; the flags are many, lww, secret, role, stands-for, search-through, code, unit= and reverse=",
		),
		(&["type", "add", "Book!"][..], "Book! is not a valid name"),
		(
			&["type", "add", "book", "tags:text:many:lww"][..],
			"a field has either many values or the last one, not both",
		),
		(
			&["type", "add", "book", "notes:text:secret:secret"][..],
			"the flag secret is given twice",
		),
		(
			&["type", "add", "book", "title:text:"][..],
			"a field is name:kind, with flags such as :many, :lww, :secret or :role after it, not title:text:",
		),
		(
			&["type", "add", "book", "title:integer"][..],
			"local.title has values of another kind already, and a field's kind never changes",
		),
		(
			&["type", "add", "org.nibkeep.type-definition"][..],
			"org.nibkeep.type-definition is defined by NibKeep, and no space can define it",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
}

/// Standard names come from the vocabulary alone, so that a field that a later version adds
/// to a standard type never meets one of another kind; a standard type takes fields of the
/// person's own, and a type lists a standard field as the vocabulary defines it.
#[test]
fn standard_names_come_from_the_vocabulary_alone() {
	let mut test = library("standard names");
	for (arguments, message) in [
		(
			&["type", "add", "std.award"][..],
			"std.award is a standard name, which only the standard vocabulary gives; choose another type name, such as one starting with local.",
		),
		(
			&["type", "add", "photo", "std.depicts:reference"][..],
			"std.depicts is a standard name, which only the standard vocabulary gives; choose another field name, such as one starting with local.",
		),
		(
			&[
				"type",
				"add",
				"photo",
				"depicts:reference:reverse=std.portraits",
			][..],
			"std.portraits is a standard name, which only the standard vocabulary gives; choose another field name, such as one starting with local.",
		),
		// A reverse name is no field, and a standard field keeps its kind.
		(
			&["type", "add", "essay", "std.author-of:text"][..],
			"std.author-of is a standard name, which only the standard vocabulary gives; choose another field name, such as one starting with local.",
		),
		(
			&["type", "add", "essay", "std.tree-path:integer"][..],
			"std.tree-path has values of another kind already, and a field's kind never changes",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
	let shown = test
		.nib(&["type", "add", "std.photo", "depicts:reference:many"])
		.expect("a standard type takes a field of the person's own");
	assert!(
		shown.lines().any(|line| {
			let words: Vec<&str> = line.split_whitespace().collect();
			words == ["local.depicts", "reference,", "many"]
		}),
		"{shown}"
	);
	let shown = test
		.nib(&[
			"type",
			"add",
			"essay",
			"std.author:reference:many:search-through:reverse=std.author-of",
		])
		.expect("a standard field");
	assert!(shown.contains("std.author"), "{shown}");
}

#[test]
fn links_have_a_reverse_name_and_can_stand_for_what_they_refer_to() {
	let mut test = library("links");
	let pen_name = "\
local.pen-name, version 1
  local.used-by  reference, many, stands for, searched through, reverse local.pen-name
";
	assert_eq!(
		test.nib(&[
			"type",
			"add",
			"pen-name",
			"used-by:reference:many:stands-for:search-through:reverse=pen-name"
		])
		.expect("adds"),
		pen_name
	);
	assert_eq!(
		test.nib(&["type", "show", "pen-name"]).expect("shows"),
		pen_name
	);
	let shown = test.json(&["type", "show", "pen-name"]);
	assert_eq!(shown["fields"][0]["stands_for"], true);
	assert_eq!(shown["fields"][0]["search_through"], true);
	assert_eq!(shown["fields"][0]["reverse"], "local.pen-name");
	for (arguments, message) in [
		(
			&["type", "add", "book", "title:text:stands-for"][..],
			"only a reference can stand for what it refers to, be searched through, or have a reverse name",
		),
		(
			&["type", "add", "book", "title:text:search-through"][..],
			"only a reference can stand for what it refers to, be searched through, or have a reverse name",
		),
		(
			&["type", "add", "book", "title:text:reverse=titled"][..],
			"only a reference can stand for what it refers to, be searched through, or have a reverse name",
		),
		(
			&["type", "add", "book", "author:reference:reverse="][..],
			"a reverse name is a field's name, such as reverse=author-of",
		),
		(
			&["type", "add", "book", "author:reference:reverse=Author!"][..],
			"Author! is not a valid name",
		),
		(
			&[
				"type",
				"add",
				"book",
				"author:reference:reverse=wrote:reverse=penned",
			][..],
			"the flag reverse is given twice",
		),
		(
			&[
				"type",
				"add",
				"pen-name",
				"used-by:reference:many:stands-for:search-through:reverse=alias",
			][..],
			"local.used-by goes by local.pen-name from its other end already",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
}

#[test]
fn types_and_fields_are_shown_with_their_labels() {
	let mut test = library("labels");
	assert_eq!(
		test.nib(&["type", "show", "std.login"]).expect("shows"),
		"\
std.login (Login), version 1
  org.nibkeep.name   name                                                            Name
  std.in-folder      reference, many, searched through, reverse std.folder-contents  Folder
  std.one-time-code  text, secret, code                                              One-time codes
  std.password       text, secret, code                                              Password
  std.recovery-code  text, many, secret, code                                        Recovery codes
  std.secret-notes   text, secret                                                    Secret notes
  std.username       text, code                                                      User name
  std.website        text, many, code                                                Web site
"
	);
	let shown = test.json(&["type", "show", "std.login"]);
	assert_eq!(shown["label"], "Login");
	assert_eq!(shown["labels"], json!({ "en": "Login" }));
	assert_eq!(shown["fields"][3]["name"], "std.password");
	assert_eq!(shown["fields"][3]["label"], "Password");
}
