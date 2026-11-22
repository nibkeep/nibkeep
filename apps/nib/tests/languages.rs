//! Languages in `nib`: the ranked languages of the user, the device and the app, values in a
//! language, and other forms of a value.

mod common;

use clap::Parser;
use common::{Test, ids_hidden};

/// A library started with the mock keystore, whose user reads `ranges`.
fn library(name: &str, ranges: &str) -> Test {
	let mut test = Test::with_keystore(name);
	test.nib(&["init", "--name", "Ada", "--languages", ranges])
		.expect("starts");
	test
}

#[test]
fn the_most_specific_list_of_languages_counts() {
	let mut test = library("languages", "en, ru");
	assert_eq!(
		test.nib(&["languages"]).expect("shows them"),
		"In use: en, ru\nYours: en, ru\nThis device's: none\nThis app's: none\n"
	);
	assert_eq!(
		test.nib(&["languages", "en", "--device"])
			.expect("sets them"),
		"In use: en\nYours: en, ru\nThis device's: en\nThis app's: none\n"
	);
	assert_eq!(
		test.nib(&["languages", "FR, en", "--app"])
			.expect("sets them"),
		"In use: fr, en\nYours: en, ru\nThis device's: en\nThis app's: fr, en\n"
	);
	let shown = test.json(&["languages"]);
	assert_eq!(shown["in_use"], "fr, en");
	assert_eq!(shown["user"], "en, ru");
	assert_eq!(shown["device"], "en");
	assert_eq!(shown["replica"], "fr, en");
	test.nib(&["languages", "--clear", "--app"])
		.expect("clears them");
	test.nib(&["languages", "--clear", "--device"])
		.expect("clears them");
	assert_eq!(
		test.nib(&["languages", "sv"]).expect("sets them"),
		"In use: sv\nYours: sv\nThis device's: none\nThis app's: none\n"
	);
	assert_eq!(
		test.nib(&["languages", "en, not a range"])
			.expect_err("not ranges")
			.to_string(),
		"not a range is not a language range, such as en, uk-Latn or *-Latn"
	);
	assert!(
		nibkeep_cli::Cli::try_parse_from(["nib", "languages", "en", "--clear"]).is_err(),
		"a list is set or cleared, not both"
	);
}

#[test]
fn a_library_starts_with_the_systems_languages() {
	let mut test = Test::with_keystore("system languages");
	test.languages = vec![
		"en_US.UTF-8".to_owned(),
		"en_GB".to_owned(),
		"sr_RS@latin".to_owned(),
		"ru".to_owned(),
		"C".to_owned(),
	];
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	assert_eq!(
		test.nib(&["languages"]).expect("shows them").lines().nth(1),
		Some("Yours: en, sr-Latn, ru")
	);

	let mut test = Test::with_keystore("no system languages");
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	assert_eq!(
		test.nib(&["languages"]).expect("shows them").lines().next(),
		Some("In use: none")
	);
}

#[test]
fn a_name_in_several_forms_shows_the_readers() {
	let mut test = library("forms", "en, *-Latn, uk");
	test.nib(&["new", "person", "name=Олена Коваль", "--language", "uk"])
		.expect("adds");
	let shown = test
		.nib(&[
			"edit",
			"Олена Коваль",
			"name+=Olena Koval",
			"--language",
			"uk-Latn-t-uk",
			"--form",
		])
		.expect("adds a form");
	assert_eq!(
		ids_hidden(&shown),
		"Olena Koval\n  id                <id>\n  type              std.person\n  org.nibkeep.name  Olena Koval (uk-Latn-t-uk) / Олена Коваль (uk)\n"
	);
	test.nib(&[
		"edit",
		"olena koval",
		"name+=Елена Коваль",
		"--language",
		"ru",
		"--form-of",
		"Олена Коваль",
	])
	.expect("adds a form of the value named by any of its forms");
	let found = ids_hidden(&test.nib(&["find", "Елена"]).expect("finds"));
	assert_eq!(found, "<id>  Olena Koval  std.person\n");
	let shown = test.json(&["show", "Елена Коваль"]);
	let name = &shown["fields"][0]["values"][0];
	assert_eq!(name["language"], "uk-Latn-t-uk");
	assert_eq!(name["other_forms"], 2);
	let mut forms: Vec<String> = name["forms"]
		.as_array()
		.expect("the other forms")
		.iter()
		.map(|form| format!("{} {}", form["value"]["full"], form["language"]))
		.collect();
	forms.sort();
	assert_eq!(
		forms,
		["\"Елена Коваль\" \"ru\"", "\"Олена Коваль\" \"uk\""]
	);
	let line = test
		.nib(&["show", "Olena Koval"])
		.expect("shows")
		.lines()
		.find(|line| line.contains("org.nibkeep.name"))
		.expect("the name")
		.to_owned();
	assert!(line.contains("Olena Koval (uk-Latn-t-uk) / "), "{line}");
	assert!(
		line.contains("Олена Коваль (uk)") && line.contains("Елена Коваль (ru)"),
		"{line}"
	);

	test.nib(&["languages", "uk", "--app"]).expect("sets them");
	assert_eq!(
		test.nib(&["show", "Olena Koval"])
			.expect("shows")
			.lines()
			.next(),
		Some("Олена Коваль")
	);
}

#[test]
fn a_language_tags_text_names_and_files_only() {
	let mut test = library("tagged", "en");
	test.nib(&["type", "add", "book", "title:text", "year:integer"])
		.expect("defines");
	test.nib(&[
		"new",
		"book",
		"title=Leviathan Wakes",
		"year=2011",
		"--language",
		"en",
	])
	.expect("adds");
	let shown = test.json(&["show", "Leviathan Wakes"]);
	let field = |name: &str| {
		shown["fields"]
			.as_array()
			.expect("fields")
			.iter()
			.find(|field| field["name"] == name)
			.cloned()
			.expect("the field")
	};
	assert_eq!(field("local.title")["values"][0]["language"], "en");
	assert_eq!(
		field("local.year")["values"][0]["language"],
		serde_json::Value::Null
	);
}

#[test]
fn codes_are_in_no_language() {
	let mut test = library("codes", "en");
	assert_eq!(
		test.nib(&["type", "add", "record", "title:text", "isbn:text:many:code"])
			.expect("defines"),
		"local.record, version 1\n  local.isbn   text, many, code\n  local.title  text\n"
	);
	assert_eq!(
		test.nib(&["type", "add", "record", "pages:integer:code"])
			.expect_err("not text")
			.to_string(),
		"only text can hold codes, which are in no language"
	);
	// The standard vocabulary's language and ISBNs are codes.
	test.nib(&[
		"new",
		"book",
		"name=Ostatnie życzenie",
		"language=pl",
		"--language",
		"pl",
	])
	.expect("adds a book in Polish");
	let shown = test
		.nib(&["show", "Ostatnie życzenie"])
		.expect("shows the book");
	assert!(
		shown.contains("  org.nibkeep.name  Ostatnie życzenie (pl)\n"),
		"{shown}"
	);
	assert!(shown.contains("  std.language      pl\n"), "{shown}");
	test.nib(&[
		"new",
		"edition",
		"name=Ostatnie życzenie (superNOWA, 1993)",
		"isbn=0-00-000000-0",
		"--language",
		"pl",
	])
	.expect("adds an edition");
	let shown = test.json(&["show", "Ostatnie życzenie (superNOWA, 1993)"]);
	let isbn = shown["fields"]
		.as_array()
		.expect("fields")
		.iter()
		.find(|field| field["name"] == "std.isbn")
		.expect("the ISBN");
	assert_eq!(isbn["values"][0]["language"], serde_json::Value::Null);
	test.nib(&["new", "record", "title=Tytuł", "isbn=1", "--language", "pl"])
		.expect("adds a record");
	test.nib(&["edit", "Tytuł", "isbn+=2", "--language", "pl"])
		.expect("adds a code");
	let shown = test.nib(&["show", "Tytuł"]).expect("shows");
	assert!(
		shown.contains("  local.isbn   1; 2\n") || shown.contains("  local.isbn   2; 1\n"),
		"{shown}"
	);
	assert!(shown.contains("  local.title  Tytuł (pl)\n"), "{shown}");
}

#[test]
fn mistakes_with_languages_and_forms_are_explained() {
	let mut test = library("language mistakes", "en");
	test.nib(&["type", "add", "thing", "title:text", "tag:text:many"])
		.expect("defines");
	test.nib(&["new", "thing", "title=Ball", "tag=red", "tag=round"])
		.expect("adds");
	assert_eq!(
		test.nib(&["edit", "Ball", "tag+=красный", "--language", "ru", "--form"])
			.expect_err("which value")
			.to_string(),
		"local.tag shows several values; say which one the new form is of"
	);
	assert_eq!(
		test.nib(&["edit", "Ball", "tag=blue", "--form"])
			.expect_err("nothing added")
			.to_string(),
		"--form adds and removes forms with field+=value and field-=value; there are none"
	);
	let error =
		nibkeep_cli::Cli::try_parse_from(["nib", "new", "thing", "tag=x", "--language", "en_US"])
			.expect_err("not a tag");
	assert!(
		error
			.to_string()
			.contains("a language tag is written as in BCP 47, such as en, uk or uk-Latn"),
		"{error}"
	);
	let error =
		nibkeep_cli::Cli::try_parse_from(["nib", "init", "--name", "Ada", "--languages", "en_US"])
			.expect_err("not a range");
	assert!(
		error.to_string().contains("en_US is not a language range"),
		"{error}"
	);
}

#[test]
fn a_form_is_removed_with_form_and_a_value_without() {
	let mut test = library("remove forms", "en");
	test.nib(&["type", "add", "thing", "title:text", "tag:text:many"])
		.expect("defines");
	test.nib(&[
		"new",
		"thing",
		"title=Ball",
		"tag=cat",
		"tag=dog",
		"--language",
		"en",
	])
	.expect("adds");
	test.nib(&[
		"edit",
		"Ball",
		"tag+=кот",
		"--language",
		"ru",
		"--form-of",
		"cat",
	])
	.expect("adds a form");
	let shown = test
		.nib(&["edit", "Ball", "tag-=кот", "--form"])
		.expect("removes a form");
	assert!(
		shown.contains("cat (en); dog (en)") || shown.contains("dog (en); cat (en)"),
		"{shown}"
	);
	test.nib(&[
		"edit",
		"Ball",
		"tag+=кот",
		"--language",
		"ru",
		"--form-of",
		"cat",
	])
	.expect("adds it again");
	let shown = test
		.nib(&["edit", "Ball", "tag-=кот"])
		.expect("removes the value");
	assert!(shown.contains("local.tag    dog (en)\n"), "{shown}");
}
