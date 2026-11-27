//! `nib history` and `nib show --as-of`: the record over time, in system time.

use crate::common::{Test, ids_hidden, library};
use clap::Parser as _;
use nibkeep_testkit::millis_at;

/// A library with a note titled "Milk", made at 10:00 on 2026-10-12, then retitled at 10:05
/// and tagged at 10:10.
fn with_a_note(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["type", "add", "note", "title:text", "tag:text:many"])
		.expect("adds the type");
	test.nib(&["new", "note", "title=Milk"])
		.expect("adds the note");
	test.now = millis_at("2026-10-12", 10, 5);
	test.nib(&["edit", "Milk", "title=Oat milk"])
		.expect("retitles it");
	test.now = millis_at("2026-10-12", 10, 10);
	test.nib(&["edit", "Oat milk", "tag+=shop"])
		.expect("tags it");
	test
}

#[test]
fn history_shows_every_value_with_when_and_who() {
	let mut test = with_a_note("history");
	assert_eq!(
		test.nib(&["history", "Oat milk"]).expect("shows"),
		"\
Oat milk
  local.tag
    2026-10-12 10:10  Ada  shop
  local.title
    2026-10-12 10:00  Ada  Milk  (replaced 2026-10-12 10:05)
    2026-10-12 10:05  Ada  Oat milk
  org.nibkeep.type
    2026-10-12 10:00  Ada  local.note
"
	);
	// A field the entity has had, of its types or of its own record.
	assert!(
		test.nib(&["history", "Oat milk", "type"])
			.expect("shows")
			.contains("org.nibkeep.type")
	);
	assert_eq!(
		test.nib(&["history", "Oat milk", "isbn"])
			.expect_err("never had it")
			.to_string(),
		"the entity never had a field isbn"
	);
	assert_eq!(
		test.nib(&["history", "Oat milk", "title"]).expect("shows"),
		"\
Oat milk
  local.title
    2026-10-12 10:00  Ada  Milk  (replaced 2026-10-12 10:05)
    2026-10-12 10:05  Ada  Oat milk
"
	);

	let history = test.json(&["history", "Oat milk"]);
	assert_eq!(history["name"], "Oat milk");
	assert_eq!(history["id"], test.json(&["show", "Oat milk"])["id"]);
	let title = &history["fields"][1];
	assert_eq!(title["name"], "local.title");
	let first = &title["values"][0];
	assert_eq!(first["value"], "Milk");
	assert_eq!(first["kind"], "text");
	assert_eq!(first["recorded"], "2026-10-12T10:00:00Z");
	assert_eq!(first["replaced"], "2026-10-12T10:05:00Z");
	assert_eq!(first["restated"], serde_json::Value::Null);
	assert_eq!(first["author"]["name"], "Ada");
	assert!(first["statement"].is_string());
	assert_eq!(title["values"][1]["replaced"], serde_json::Value::Null);
}

#[test]
fn history_shows_when_an_entity_was_deleted_and_restored() {
	let mut test = with_a_note("history of deletion");
	test.now = millis_at("2026-10-12", 11, 0);
	test.nib(&["edit", "Oat milk", "--delete"])
		.expect("deletes");
	test.now = millis_at("2026-10-12", 12, 0);
	test.nib(&["edit", "Oat milk", "--restore"])
		.expect("restores");
	let history = test.nib(&["history", "Oat milk"]).expect("shows");
	assert!(
		history.contains(
			"  org.nibkeep.deleted\n    2026-10-12 11:00  Ada  yes  (replaced 2026-10-12 12:00)\n"
		),
		"{history}"
	);
}

#[test]
fn show_as_of_shows_the_record_at_a_moment() {
	let mut test = with_a_note("as of");
	test.now = millis_at("2026-10-12", 11, 0);
	test.nib(&["edit", "Oat milk", "--delete"])
		.expect("deletes");
	let title_then = |test: &mut Test, when: &str| {
		let shown = test
			.nib(&["show", "Oat milk", "--as-of", when])
			.expect("shows");
		let title = shown
			.lines()
			.find(|line| line.trim_start().starts_with("local.title"))
			.map(|line| line.trim().to_owned());
		(title, shown.contains("deleted"))
	};
	assert_eq!(
		title_then(&mut test, "2026-10-12T10:03"),
		(Some("local.title  Milk".to_owned()), false)
	);
	assert_eq!(
		title_then(&mut test, "2026-10-12T10:07"),
		(Some("local.title  Oat milk".to_owned()), false)
	);
	// A moment with its offset from UTC.
	assert_eq!(
		title_then(&mut test, "2026-10-12T12:03+02:00"),
		(Some("local.title  Milk".to_owned()), false)
	);
	// A day alone means its end.
	assert_eq!(
		title_then(&mut test, "2026-10-12"),
		(Some("local.title  Oat milk".to_owned()), true)
	);
	assert_eq!(
		ids_hidden(
			&test
				.nib(&["show", "Oat milk", "--as-of", "2026-10-12T10:07"])
				.expect("shows")
		),
		"\
Oat milk
  id           <id>
  type         local.note
  local.title  Oat milk
"
	);
	let shown = test.json(&["show", "Oat milk", "--as-of", "2026-10-12T10:03"]);
	assert_eq!(shown["fields"][0]["values"][0]["value"], "Milk");

	assert_eq!(
		test.nib(&["show", "Oat milk", "--as-of", "2026-10-11"])
			.expect_err("too early")
			.to_string(),
		"the library had no record of Oat milk by 2026-10-11 23:59"
	);
	assert_eq!(
		test.nib(&["show", "Oat milk", "--as-of", "1960-01-01T00:00Z"])
			.expect_err("before 1970")
			.to_string(),
		"the library had no record of Oat milk by 1970"
	);
	// A lowercase separator still has a time.
	assert_eq!(
		title_then(&mut test, "2026-10-12t10:03"),
		(Some("local.title  Milk".to_owned()), false)
	);
	let error =
		nibkeep_cli::Cli::try_parse_from(["nib", "show", "Oat milk", "--as-of", "yesterday"])
			.expect_err("no moment")
			.to_string();
	assert!(
		error.contains(
			"a day such as 2026-10-10, or a moment such as 2026-10-10T20:00, with its offset such as +02:00 if not in local time"
		),
		"{error}"
	);
}

#[test]
fn show_as_of_names_what_it_refers_to_as_it_was_named_then() {
	let mut test = library("names then");
	test.nib(&["new", "pen-name", "name=J. S. A. Corey"])
		.expect("adds the pen name");
	test.nib(&[
		"new",
		"book",
		"name=Leviathan Wakes",
		"author=J. S. A. Corey",
	])
	.expect("adds the book");
	test.now = millis_at("2026-10-12", 10, 5);
	test.nib(&["edit", "J. S. A. Corey", "name=James S. A. Corey"])
		.expect("renames the pen name");
	let author = |shown: &str| {
		shown
			.lines()
			.find(|line| line.trim_start().starts_with("std.author"))
			.map(|line| line.trim().to_owned())
	};
	let then = test
		.nib(&["show", "Leviathan Wakes", "--as-of", "2026-10-12T10:03"])
		.expect("shows");
	assert_eq!(
		author(&then).as_deref(),
		Some("std.author        J. S. A. Corey")
	);
	let now = test.nib(&["show", "Leviathan Wakes"]).expect("shows");
	assert_eq!(
		author(&now).as_deref(),
		Some("std.author        James S. A. Corey")
	);
}

#[test]
fn a_move_carries_the_history_with_the_movers_note() {
	let mut test = with_a_note("history carried");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.now = millis_at("2026-10-12", 11, 0);
	test.nib(&["move", "Oat milk", "--to", "Household"])
		.expect("moves");
	assert_eq!(
		test.nib(&["history", "Oat milk", "title"]).expect("shows"),
		"\
Oat milk
  local.title
    2026-10-12 11:00  Ada  Milk  (carried from Personal, recorded there 2026-10-12 10:00 by Ada, replaced there 2026-10-12 10:05)
    2026-10-12 11:00  Ada  Oat milk  (carried from Personal, recorded there 2026-10-12 10:05 by Ada)
"
	);
	let history = test.json(&["history", "Oat milk", "title"]);
	let carried = &history["fields"][0]["values"][0]["carried"];
	assert_eq!(carried["from_name"], "Personal", "{history}");
	assert_eq!(carried["author"]["name"], "Ada");
	// Replaced there since, a value carried current says so.
	test.now = millis_at("2026-10-12", 11, 5);
	test.nib(&["edit", "Oat milk", "title=Almond milk"])
		.expect("retitles it");
	let shown = test
		.nib(&["history", "Almond milk", "title"])
		.expect("shows");
	assert!(
		shown.contains("Oat milk  (carried from Personal, recorded there 2026-10-12 10:05 by Ada, replaced 2026-10-12 11:05)"),
		"{shown}"
	);
	// Moved without its history, a value has no note.
	test.nib(&["new", "note", "title=Eggs"]).expect("adds");
	test.nib(&["move", "Eggs", "--to", "Household", "--without-history"])
		.expect("moves");
	let history = test.json(&["history", "Eggs", "title"]);
	assert!(
		history["fields"][0]["values"][0]["carried"].is_null(),
		"{history}"
	);
}
