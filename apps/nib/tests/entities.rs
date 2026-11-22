//! `nib new`, `nib show` and `nib list`: entities and their values.

mod common;

use clap::CommandFactory;
use common::{Test, ids_hidden, library};
use nibkeep_cli::Cli;

/// A library with the type of books and an author.
fn with_books(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&[
		"type",
		"add",
		"book",
		"title:text",
		"author:reference:many:role",
		"year:date",
		"pages:integer",
		"read:boolean",
	])
	.expect("adds the type");
	test.nib(&["new", "person", "name=Daniel Abraham"])
		.expect("adds the author");
	test
}

#[test]
fn an_entity_is_added_and_shown() {
	let mut test = with_books("added and shown");
	let added = test
		.nib(&[
			"new",
			"book",
			"title=Leviathan Wakes",
			"author=Daniel Abraham",
			"year=2011",
			"pages=592",
			"read=yes",
		])
		.expect("adds");
	let expected = "\
Leviathan Wakes
  id            <id>
  type          local.book
  local.author  Daniel Abraham
  local.pages   592
  local.read    yes
  local.title   Leviathan Wakes
  local.year    2011
";
	assert_eq!(ids_hidden(&added), expected);
	assert_eq!(
		ids_hidden(&test.nib(&["show", "leviathan wakes"]).expect("shows")),
		expected
	);
	assert_eq!(
		ids_hidden(&test.nib(&["show", "Daniel Abraham"]).expect("shows")),
		"Daniel Abraham\n  id                <id>\n  type              std.person\n  org.nibkeep.name  Daniel Abraham\nLinked from\n  local.author  Leviathan Wakes\n"
	);

	let shown = test.json(&["show", "Leviathan Wakes"]);
	assert_eq!(shown["name"], "Leviathan Wakes");
	assert_eq!(shown["types"], serde_json::json!(["local.book"]));
	assert_eq!(shown["deleted"], false);
	let fields = shown["fields"].as_array().expect("fields");
	assert_eq!(fields.len(), 5);
	assert_eq!(fields[0]["name"], "local.author");
	assert_eq!(fields[0]["values"][0]["kind"], "reference");
	let author = test.json(&["show", "Daniel Abraham"]);
	assert_eq!(fields[0]["values"][0]["value"], author["id"]);
	assert_eq!(fields[1]["values"][0]["value"], 592);
	assert_eq!(fields[2]["values"][0]["value"], true);
	assert_eq!(fields[4]["values"][0]["value"], "2011");
	assert!(fields[3]["values"][0]["statement"].is_string());
	let id = shown["id"].as_str().expect("an ID");
	assert_eq!(
		test.json(&["show", &id[..6]])["name"],
		"Leviathan Wakes",
		"by a prefix of the ID"
	);
}

#[test]
fn entities_are_listed() {
	let mut test = with_books("listed");
	test.nib(&["new", "book", "title=Leviathan Wakes"])
		.expect("adds");
	test.nib(&["new", "book", "title=Caliban's War"])
		.expect("adds");
	assert_eq!(
		ids_hidden(&test.nib(&["list", "book"]).expect("lists")),
		"<id>  Caliban's War    local.book\n<id>  Leviathan Wakes  local.book\n"
	);
	let all = ids_hidden(&test.nib(&["list"]).expect("lists"));
	// The user and the person linked to the user, in the order of their IDs.
	let first: Vec<&str> = all.lines().take(3).collect();
	assert!(first.contains(&"<id>  Ada              std.person"));
	assert!(first.contains(&"<id>  Ada              org.nibkeep.user"));
	assert_eq!(first[2], "<id>  Caliban's War    local.book");
	assert!(all.contains("<id>  Daniel Abraham   std.person\n"));
	let listed = test.json(&["list", "book"]);
	assert_eq!(listed[0]["name"], "Caliban's War");
	assert_eq!(listed[0]["types"], serde_json::json!(["local.book"]));
	assert!(listed[1]["id"].is_string());
}

#[test]
fn values_are_read_as_their_fields_say() {
	let mut test = with_books("values");
	for (arguments, message) in [
		(
			&["new", "book", "pages=many"][..],
			"local.pages takes a whole number",
		),
		(
			&["new", "book", "read=perhaps"][..],
			"local.read takes yes or no",
		),
		(
			&["new", "book", "year=next week"][..],
			"local.year takes a date, such as 2011, 2011-06 or 1975~",
		),
		(
			&["new", "book", "author=Ty Franck"][..],
			"there is no entity Ty Franck",
		),
		(
			&["new", "book", "title"][..],
			"a value is field=value, not title",
		),
		(
			&["new", "book", "mood=tense"][..],
			"local.book has no field mood; give its full name, such as local.mood, for a field that no type defines",
		),
		(
			&["new", "boko", "title=Leviathan Wakes"][..],
			"there is no type local.boko; define it with nib type add",
		),
		(
			&[
				"new",
				"book",
				"title=Leviathan Wakes",
				"title=Leviathan wakes",
			][..],
			"local.title takes one value",
		),
		(
			&["new", "book", "org.nibkeep.deleted=yes"][..],
			"org.nibkeep.deleted is one of NibKeep's own fields, which only its own commands set",
		),
		(
			&["new", "type-definition"][..],
			"org.nibkeep.type-definition is defined by NibKeep, and only its own commands make entities of it",
		),
		(
			&["show", "Caliban's War"][..],
			"there is no entity Caliban's War",
		),
		(
			&["list", "boko"][..],
			"there is no type local.boko; define it with nib type add",
		),
	] {
		assert_eq!(test.nib(arguments).expect_err(message).to_string(), message);
	}
	// A field that no type defines takes text, given by its full name.
	let added = test
		.nib(&["new", "book", "title=Leviathan Wakes", "local.mood=tense"])
		.expect("adds");
	assert!(added.contains("  local.mood   tense\n"), "{added}");
}

#[test]
fn a_name_and_a_prefix_of_an_id_can_be_ambiguous() {
	let mut test = with_books("ambiguous");
	let first = test.json(&["new", "book", "title=Leviathan Wakes"]);
	let prefix = first["id"].as_str().expect("an ID")[..8].to_owned();
	let second = test.json(&["new", "book", &format!("title={prefix}")]);
	let mut lines = [
		format!("  {prefix}  Leviathan Wakes  local.book"),
		format!(
			"  {}  {prefix}         local.book",
			&second["id"].as_str().expect("an ID")[..8]
		),
	];
	lines.sort();
	assert_eq!(
		test.nib(&["show", &prefix])
			.expect_err("ambiguous")
			.to_string(),
		format!(
			"{prefix} could be any of 2 entities; give the ID, or its start:\n{}",
			lines.join("\n")
		)
	);
	assert_eq!(
		test.json(&["show", first["id"].as_str().expect("an ID")])["name"],
		"Leviathan Wakes",
		"the whole ID is never ambiguous"
	);
}

#[test]
fn an_ambiguous_name_lists_the_entities_it_could_be() {
	let mut test = with_books("ambiguous name");
	test.nib(&["type", "add", "edition", "title:text"])
		.expect("adds a type");
	let book = test.json(&["new", "book", "title=Leviathan Wakes"]);
	let edition = test.json(&["new", "edition", "title=Leviathan Wakes"]);
	let error = test
		.nib(&["edit", "Leviathan Wakes", "year=2011"])
		.expect_err("ambiguous")
		.to_string();
	let short = |entity: &serde_json::Value| entity["id"].as_str().expect("an ID")[..8].to_owned();
	assert!(
		error.starts_with(
			"Leviathan Wakes could be any of 2 entities; give the ID, or its start:\n"
		),
		"{error}"
	);
	assert!(
		error.contains(&format!("  {}  Leviathan Wakes  local.book", short(&book))),
		"{error}"
	);
	assert!(
		error.contains(&format!(
			"  {}  Leviathan Wakes  local.edition",
			short(&edition)
		)),
		"{error}"
	);
	// A reference in a value is told apart the same way.
	let error = test
		.nib(&[
			"new",
			"book",
			"title=Caliban's War",
			"author=Leviathan Wakes",
		])
		.expect_err("ambiguous")
		.to_string();
	assert!(error.contains("local.edition"), "{error}");
	test.nib(&["edit", &short(&book), "year=2011"])
		.expect("the start of an ID tells them apart");

	for _ in 0..11 {
		test.nib(&["new", "book", "title=Untitled"]).expect("adds");
	}
	let error = test
		.nib(&["show", "Untitled"])
		.expect_err("ambiguous")
		.to_string();
	assert_eq!(error.lines().count(), 12, "{error}");
	assert!(error.ends_with("\n  and 1 more"), "{error}");
}

#[test]
fn an_entity_has_several_types_given_and_taken_away() {
	let mut test = with_books("several types");
	test.nib(&[
		"type",
		"add",
		"audiobook",
		"narrator:reference:many",
		"minutes:integer",
	])
	.expect("adds a type");
	test.nib(&["new", "person", "name=Jefferson Mays"])
		.expect("adds a narrator");
	let added = test
		.nib(&[
			"new",
			"book,audiobook",
			"title=Leviathan Wakes",
			"narrator=Jefferson Mays",
		])
		.expect("adds an entity of two types");
	assert!(
		added.contains("  type            local.audiobook, local.book\n"),
		"{added}"
	);
	assert_eq!(
		ids_hidden(&test.nib(&["list", "audiobook"]).expect("lists")),
		"<id>  Leviathan Wakes  local.audiobook, local.book\n"
	);

	test.nib(&["new", "book", "title=Caliban's War"])
		.expect("adds a book");
	let edited = test
		.nib(&["edit", "Caliban's War", "type+=audiobook", "minutes=1260"])
		.expect("gives a type, and sets its field");
	assert!(edited.contains("local.audiobook, local.book"), "{edited}");
	assert!(edited.contains("  local.minutes  1260\n"), "{edited}");
	let edited = test
		.nib(&["edit", "Caliban's War", "type-=book"])
		.expect("takes a type away");
	assert!(
		edited.contains("  type           local.audiobook\n"),
		"{edited}"
	);
	assert!(
		edited.contains("  local.title    Caliban's War\n"),
		"the values stay: {edited}"
	);
	assert_eq!(
		test.nib(&["edit", "Caliban's War", "type-=book"])
			.expect_err("not a book")
			.to_string(),
		"the entity has no type local.book"
	);
	assert_eq!(
		test.nib(&["edit", "Caliban's War", "type-=audiobook"])
			.expect_err("the only type")
			.to_string(),
		"local.audiobook is the entity's only type; give it another before taking this one away"
	);
	assert_eq!(
		test.nib(&["edit", "Caliban's War", "type=book"])
			.expect_err("types are not set")
			.to_string(),
		"an entity's types are given with type+=TYPE and taken away with type-=TYPE"
	);
	assert_eq!(
		test.nib(&["edit", "Caliban's War", "type+=org.nibkeep.user"])
			.expect_err("an own type")
			.to_string(),
		"org.nibkeep.user is defined by NibKeep, and only its own commands make entities of it"
	);
	test.nib(&["edit", "Caliban's War", "type+=book", "type-=audiobook"])
		.expect("swaps the types");
	assert_eq!(
		ids_hidden(&test.nib(&["list", "audiobook"]).expect("lists")),
		"<id>  Leviathan Wakes  local.audiobook, local.book\n"
	);
}

/// A library with two series of books, the books with numbers in them and dates.
fn with_series(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["type", "add", "series", "org.nibkeep.name:name"])
		.expect("adds a type");
	test.nib(&[
		"type",
		"add",
		"book",
		"title:text",
		"series:reference",
		"number:decimal",
		"published:date",
	])
	.expect("adds a type");
	test.nib(&["new", "series", "name=The Expanse"])
		.expect("adds a series");
	test.nib(&["new", "series", "name=Dune Chronicles"])
		.expect("adds a series");
	test.nib(&["new", "person", "name=Frank Herbert"])
		.expect("adds a person");
	for (title, series, number, published) in [
		("Caliban's War", "The Expanse", "2", "2012-06-26"),
		("Drive", "The Expanse", "0.1", "2012-11-27"),
		("Leviathan Wakes", "The Expanse", "1", "2011-06-15"),
		("Gods of Risk", "The Expanse", "2.5", "2012"),
		("Memory's Legion", "The Expanse", "", "2022-03-15"),
		("Dune", "Dune Chronicles", "1", "1965"),
	] {
		let mut values = vec![
			format!("title={title}"),
			format!("series={series}"),
			format!("published={published}"),
		];
		if !number.is_empty() {
			values.push(format!("number={number}"));
		}
		let mut arguments = vec!["new", "book"];
		arguments.extend(values.iter().map(String::as_str));
		test.nib(&arguments).expect("adds a book");
	}
	test
}

/// The names in a list that `nib list` printed.
fn names(listed: &str) -> Vec<String> {
	listed
		.lines()
		.map(|line| line[10..].split("  ").next().expect("a name").to_owned())
		.collect()
}

/// The arguments of the example in the help of a command of `nib`, between the backticks
/// after "such as", split where a shell would split them, with only double quotes.
fn example(command: &str) -> Vec<String> {
	let cli = Cli::command();
	let help = cli
		.find_subcommand(command)
		.and_then(|command| command.get_long_about().or(command.get_about()))
		.expect("the command's help")
		.to_string();
	let example = help
		.split("such as `")
		.nth(1)
		.and_then(|rest| rest.split('`').next())
		.expect("an example");
	let (mut words, mut word, mut quoted) = (Vec::new(), String::new(), false);
	for character in example.chars() {
		match character {
			'"' => quoted = !quoted,
			' ' if !quoted => words.extend((!word.is_empty()).then(|| std::mem::take(&mut word))),
			_ => word.push(character),
		}
	}
	words.extend((!word.is_empty()).then_some(word));
	assert_eq!(words.first().map(String::as_str), Some("nib"), "{example}");
	words.remove(0);
	words
}

#[test]
fn the_examples_in_the_help_work_with_the_standard_types() {
	let mut test = library("help's examples");
	let new = example("new");
	test.nib(&new.iter().map(String::as_str).collect::<Vec<_>>())
		.expect("adds the book of the example");
	test.nib(&["new", "series", "name=The Expanse"])
		.expect("adds a series");
	test.nib(&[
		"edit",
		"Leviathan Wakes",
		"in-series=The Expanse",
		"number-in-series=1",
	])
	.expect("puts the book in the series");
	for (name, number) in [("Caliban's War", "2"), ("Drive", "0.1")] {
		test.nib(&[
			"new",
			"book",
			&format!("name={name}"),
			"in-series=The Expanse",
			&format!("number-in-series={number}"),
		])
		.expect("adds a book of the series");
	}
	test.nib(&["new", "book", "name=Dune"])
		.expect("adds a book of no series");
	let list = example("list");
	let listed = test
		.nib(&list.iter().map(String::as_str).collect::<Vec<_>>())
		.expect("lists the books of the example");
	assert_eq!(
		names(&listed),
		["Drive", "Leviathan Wakes", "Caliban's War"],
		"{listed}"
	);
}

#[test]
fn entities_are_listed_in_the_order_of_a_field() {
	let mut test = with_series("listed in order");
	let expanse = test
		.nib(&[
			"list",
			"book",
			"--where",
			"series=The Expanse",
			"--sort",
			"number",
		])
		.expect("lists");
	assert_eq!(
		names(&expanse),
		[
			"Drive",
			"Leviathan Wakes",
			"Caliban's War",
			"Gods of Risk",
			"Memory's Legion"
		],
		"by number, the book without one last"
	);
	let by_date = test
		.nib(&["list", "book", "--sort", "published"])
		.expect("lists");
	assert_eq!(
		names(&by_date),
		[
			"Dune",
			"Leviathan Wakes",
			"Gods of Risk",
			"Caliban's War",
			"Drive",
			"Memory's Legion"
		],
		"by when they start: 2012 on its first day"
	);
	let by_series = test
		.nib(&["list", "--sort", "series"])
		.expect("lists everything");
	let order = names(&by_series);
	assert_eq!(&order[..2], ["Dune", "Caliban's War"], "{by_series}");
	assert!(
		by_series
			.lines()
			.take(6)
			.all(|line| line.ends_with("local.book")),
		"entities without a series come last, by name: {by_series}"
	);
	assert_eq!(order[6], "Ada", "{by_series}");
}

#[test]
fn entities_are_listed_where_their_fields_show_values() {
	let mut test = with_series("listed where");
	assert_eq!(
		names(
			&test
				.nib(&[
					"list",
					"--where",
					"series=The Expanse",
					"--where",
					"number=1"
				])
				.expect("lists")
		),
		["Leviathan Wakes"],
		"every condition"
	);
	assert_eq!(
		names(
			&test
				.nib(&["list", "--where", "org.nibkeep.name=Dune Chronicles"])
				.expect("lists by a name")
		),
		["Dune Chronicles"]
	);
	let json = test.json(&["list", "book", "--where", "published=1965"]);
	assert_eq!(json.as_array().map(Vec::len), Some(1));
	assert_eq!(json[0]["name"], "Dune");

	// A reference to an entity merged into another meets a condition of either.
	test.nib(&["new", "series", "name=The Expanse series"])
		.expect("adds a second series");
	test.nib(&[
		"new",
		"book",
		"title=Strange Dogs",
		"series=The Expanse series",
	])
	.expect("adds a book of it");
	test.nib(&["merge", "The Expanse series", "The Expanse"])
		.expect("merges the series");
	let merged = test
		.nib(&["list", "book", "--where", "series=The Expanse"])
		.expect("lists");
	assert!(merged.contains("Strange Dogs"), "{merged}");
	assert_eq!(merged.lines().count(), 6, "{merged}");

	test.nib(&["edit", "Drive", "--delete"]).expect("deletes");
	let without = test
		.nib(&["list", "book", "--where", "series=The Expanse"])
		.expect("lists");
	assert!(
		!without.contains("Drive"),
		"deleted entities are not listed: {without}"
	);
	assert_eq!(
		test.nib(&["list", "--where", "series"])
			.expect_err("no value")
			.to_string(),
		"a value is field=value, not series"
	);
}

#[test]
fn types_and_a_field_named_type_are_told_apart() {
	let mut test = library("a field named type");
	test.nib(&["type", "add", "car", "type:text"])
		.expect("adds a type with a field named type");
	test.nib(&["type", "add", "vehicle", "wheels:integer"])
		.expect("adds a type");
	test.nib(&["new", "car", "type=sedan"]).expect("adds a car");
	let edited = test
		.nib(&["edit", "sedan", "type=coupe"])
		.expect_err("a car has no name");
	assert!(edited.to_string().contains("no entity sedan"), "{edited}");
	let car = test.json(&["list", "car"])[0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let edited = test
		.nib(&["edit", &car, "type=coupe"])
		.expect("sets the field");
	assert!(edited.contains("  local.type  coupe\n"), "{edited}");
	let edited = test
		.nib(&["edit", &car, "org.nibkeep.type+=vehicle", "wheels=4"])
		.expect("gives a type");
	assert!(edited.contains("local.car, local.vehicle"), "{edited}");

	// A type given in the edit has its field named type.
	test.nib(&["new", "vehicle", "wheels=3"])
		.expect("adds a tricycle");
	let tricycle = test.json(&["list", "--where", "wheels=3"])[0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let edited = test
		.nib(&["edit", &tricycle, "type+=car", "type=trike"])
		.expect("gives a type, and sets its field named type");
	assert!(edited.contains("  local.type    trike\n"), "{edited}");
	// Two fields named type, neither your own, are ambiguous, not the types.
	test.nib(&["type", "add", "org.example.toy", "org.example.type:text"])
		.expect("adds a type with a field named type");
	test.nib(&["type", "add", "org.example.game", "org.other.type:text"])
		.expect("adds a type with another");
	let toy = test.json(&["new", "org.example.toy,org.example.game"])["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	assert_eq!(
		test.nib(&["edit", &toy, "type=puzzle"])
			.expect_err("ambiguous")
			.to_string(),
		"type could be any of the fields org.example.type, org.other.type"
	);

	// An entity trades its only type, whichever comes first.
	test.nib(&["new", "vehicle", "wheels=2"])
		.expect("adds a bicycle");
	let bicycle = test.json(&["list", "--where", "wheels=2"])[0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let traded = test
		.nib(&["edit", &bicycle, "type-=vehicle", "type+=car"])
		.expect("trades its type");
	assert!(traded.contains("  type          local.car\n"), "{traded}");
}

#[test]
fn nibkeep_own_records_are_not_made_or_retyped() {
	let mut test = library("own records");
	assert_eq!(
		test.nib(&["new", "user,person", "name=Mallory"])
			.expect_err("a user record")
			.to_string(),
		"org.nibkeep.user is defined by NibKeep, and only its own commands make entities of it"
	);
	let user = test.json(&["status"])["user"]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	assert_eq!(
		test.nib(&["edit", &user, "type+=person"])
			.expect_err("the user's types")
			.to_string(),
		"org.nibkeep.user is defined by NibKeep, and only its own commands make entities of it"
	);
}

#[test]
fn numbers_meet_conditions_by_size_and_quantities_sort_by_unit() {
	let mut test = library("numbers");
	test.nib(&[
		"type",
		"add",
		"parcel",
		"title:text",
		"price:decimal",
		"weight:quantity:unit=kg",
	])
	.expect("adds a type");
	for (title, price, weight) in [
		("a", "12.50", "2 kg"),
		("b", "12.5", "1 kg"),
		("c", "3", "500 g"),
		("d", "7", "0.250 kg"),
	] {
		test.nib(&[
			"new",
			"parcel",
			&format!("title={title}"),
			&format!("price={price}"),
			&format!("weight={weight}"),
		])
		.expect("adds a parcel");
	}
	let listed = |test: &mut Test, arguments: &[&str]| -> Vec<String> {
		test.json(arguments)
			.as_array()
			.expect("a list")
			.iter()
			.map(|entity| entity["name"].as_str().unwrap_or_default().to_owned())
			.collect()
	};
	let mut cheap = listed(&mut test, &["list", "--where", "price=12.5"]);
	cheap.sort();
	assert_eq!(cheap, ["a", "b"], "12.5 and 12.50 are one price");
	assert_eq!(listed(&mut test, &["list", "--where", "price=3.0"]), ["c"]);
	assert_eq!(
		listed(&mut test, &["list", "--where", "weight=0.25 kg"]),
		["d"]
	);
	assert_eq!(
		listed(&mut test, &["list", "parcel", "--sort", "weight"]),
		["d", "b", "a", "c"],
		"kilograms by size, then grams, which are not converted"
	);
}

#[test]
fn entities_are_listed_as_they_were_on_a_day() {
	let mut test = library("listed on a day");
	test.nib(&["type", "add", "company", "org.nibkeep.name:name"])
		.expect("adds a type");
	test.nib(&["new", "company", "name=Old Mill"])
		.expect("adds a company");
	test.nib(&["edit", "Old Mill", "name+=New Mill", "--date", "2010"])
		.expect("renames it in 2010");
	assert_eq!(
		names(&test.nib(&["list", "company"]).expect("lists")),
		["New Mill"]
	);
	assert_eq!(
		names(
			&test
				.nib(&["list", "company", "--on", "2000-01-01"])
				.expect("lists on a day")
		),
		["Old Mill"]
	);
	assert_eq!(
		names(
			&test
				.nib(&["list", "--where", "name=Old Mill", "--on", "2000-01-01"])
				.expect("lists where it was named so")
		),
		["Old Mill"]
	);
	assert_eq!(
		test.nib(&["list", "--where", "name=Old Mill"])
			.expect("lists where it is named so")
			.lines()
			.count(),
		0
	);
}

#[test]
fn a_list_of_one_type_takes_the_fields_of_the_others() {
	let mut test = with_series("fields of other types");
	test.nib(&["type", "add", "audiobook", "narrator:reference:many"])
		.expect("adds a type");
	test.nib(&["new", "person", "name=Jefferson Mays"])
		.expect("adds a narrator");
	for (title, published) in [
		("Caliban's War", "2015-02-12"),
		("Leviathan Wakes", "2015-01-15"),
	] {
		let edited = test
			.nib(&["edit", title, "type+=audiobook", "narrator=Jefferson Mays"])
			.expect("makes it an audiobook too");
		assert!(edited.contains("local.audiobook, local.book"), "{edited}");
		test.nib(&["edit", title, &format!("published={published}")])
			.expect("dates it");
	}
	// The audiobook type has no title or date, but its entities are books too.
	assert_eq!(
		names(
			&test
				.nib(&["list", "audiobook", "--sort", "published"])
				.expect("sorts by a field of another type")
		),
		["Leviathan Wakes", "Caliban's War"]
	);
	assert_eq!(
		names(
			&test
				.nib(&["list", "audiobook", "--where", "title=Caliban's War"])
				.expect("a condition on a field of another type")
		),
		["Caliban's War"]
	);
	assert_eq!(
		test.nib(&["list", "audiobook", "--sort", "pages"])
			.expect_err("no type has it")
			.to_string(),
		"local.audiobook has no field pages; give its full name, such as local.pages, for a field that no type defines"
	);
	// Only the types that the entities have count: a series' name is no field of books.
	assert_eq!(
		test.nib(&["list", "book", "--where", "name=Dune Chronicles"])
			.expect_err("books have no name field")
			.to_string(),
		"local.book has no field name; give its full name, such as local.name, for a field that no type defines"
	);
}

#[test]
fn a_list_of_one_type_takes_the_fields_its_entities_have() {
	let mut test = library("fields its entities have");
	test.nib(&[
		"type",
		"add",
		"org.example.edition",
		"org.example.title:text",
		"org.example.published:date",
	])
	.expect("adds a type");
	test.nib(&["type", "add", "audiobook", "narrator:text"])
		.expect("adds a type");
	// A type of your own with a field of the same name, which no audiobook has.
	test.nib(&["type", "add", "film", "published:date"])
		.expect("adds a type");
	for (title, published) in [("Later", "2020"), ("Earlier", "2010")] {
		test.nib(&[
			"new",
			"org.example.edition,audiobook",
			&format!("org.example.title={title}"),
			&format!("org.example.published={published}"),
		])
		.expect("adds an audiobook");
	}
	let sorted = test
		.nib(&["list", "audiobook", "--sort", "published"])
		.expect("sorts by the editions' field, not the films'");
	assert_eq!(names(&sorted), ["Earlier", "Later"], "{sorted}");

	// Two fields of the same name that the entities have are ambiguous.
	test.nib(&[
		"type",
		"add",
		"org.other.release",
		"org.other.published:date",
	])
	.expect("adds a type");
	test.nib(&[
		"new",
		"org.other.release,audiobook",
		"org.other.published=2015",
	])
	.expect("adds another audiobook");
	assert_eq!(
		test.nib(&["list", "audiobook", "--sort", "published"])
			.expect_err("ambiguous")
			.to_string(),
		"published could be any of the fields org.example.published, org.other.published"
	);
}
