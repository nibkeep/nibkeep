//! Real data, as `scripts/fetch-real-data.py` writes it, imported into a library.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

use nibkeep_base::EntityId;
use nibkeep_generate::{ImportError, Imported, Languages, PASSPHRASE, RECIPE, RealData};
use nibkeep_governance::PERSON;
use nibkeep_model::{FieldName, Quantity, Value};
use nibkeep_node::{EntityView, Library, Unlocker};
use nibkeep_time::Day;

const ENTITIES: &str = r#"{"id": "Q1", "kind": "film", "names": [{"lang": "en", "text": "Harry Potter and the Philosopher's Stone"}, {"lang": "ru", "text": "Гарри Поттер и философский камень"}], "dates": {"published": ["2001-11-04", "2001-11-16"]}, "numbers": {"duration_minutes": [152]}, "links": {"series": ["Q2"], "based_on": ["Q3"]}}
{"id": "Q2", "kind": "series", "names": [{"lang": "mul", "text": "Harry Potter"}, {"lang": "en", "text": "Harry Potter series", "alias": true}]}
{"id": "Q3", "kind": "book", "names": [{"lang": "ru", "text": "Гарри Поттер и философский камень"}]}
{"id": "Q4", "kind": "person", "names": [{"lang": "en", "text": "Daniel Radcliffe"}], "dates": {"born": ["1989-07-23"]}, "texts": {"birth_name": [{"lang": "ru", "text": "Дэниел Джейкоб Рэдклифф"}, {"lang": "en", "text": "Daniel Jacob Radcliffe"}, {"lang": "en", "text": "Daniel Radcliffe"}, {"lang": "ru", "text": "Дэниел Рэдклифф"}]}}
{"id": "Q5", "kind": "character", "names": [{"lang": "en", "text": "Harry Potter"}], "links": {"father": ["Q6"], "appears_in": ["Q1"]}}
{"id": "Q6", "kind": "character", "names": [{"lang": "en", "text": "James Potter"}], "links": {"child": ["Q5"]}}
{"id": "mb:g", "kind": "audiobook", "names": [{"lang": "sv", "text": "Harry Potter och De vises sten"}], "dates": {"published": ["2004"]}, "links": {"reads": ["Q3"]}}
{"id": "mb:a", "kind": "person", "names": [{"text": "Krister Henriksson"}]}
"#;

const CREDITS: &str = r#"{"work": "Q1", "person": "Q4", "role": "cast", "character": "Q5"}
{"work": "mb:g", "person": "mb:a", "role": "narrator"}
"#;

/// A directory of real data with these files, and a manifest that names it.
fn data(name: &str, entities: &str, credits: &str) -> PathBuf {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	let data = directory.join("data");
	fs::create_dir(&data).expect("makes the data's directory");
	fs::write(data.join("entities.jsonl"), entities).expect("writes");
	fs::write(data.join("credits.jsonl"), credits).expect("writes");
	fs::write(data.join("manifest.json"), r#"{"data": "0123456789ab"}"#).expect("writes");
	data
}

fn library_beside(data: &Path) -> PathBuf {
	data.parent().expect("a parent").join("library")
}

/// Imports the data beside it.
fn import(data: &Path) -> (Library, Imported) {
	RealData::read(data, Languages::First)
		.expect("reads")
		.import(&library_beside(data))
}

/// Why the data cannot be read; and that it is refused before a library is made.
fn refused(data: &Path) -> ImportError {
	let error = RealData::read(data, Languages::First)
		.err()
		.expect("refuses");
	assert!(!library_beside(data).exists());
	error
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn values<'a>(view: &'a EntityView, name: &str) -> Vec<&'a Value> {
	view.fields
		.iter()
		.filter(|shown| shown.field == field(name))
		.flat_map(|shown| shown.values.iter().map(|value| &value.value))
		.collect()
}

fn today() -> Day {
	"2026-10-15".parse().expect("a day")
}

#[test]
fn real_data_becomes_entities_links_and_credits() {
	let data = data("real data", ENTITIES, CREDITS);
	assert_eq!(
		RealData::read(&data, Languages::First)
			.expect("reads")
			.name(),
		"0123456789ab"
	);
	let (library, imported) = import(&data);
	assert_eq!((imported.entities, imported.credits), (8, 2));
	drop(library);

	let mut library = Library::open(&library_beside(&data), &Unlocker::Passphrase(PASSPHRASE))
		.expect("opens the library");
	let personal = library.personal_space();
	assert_eq!(
		library.status().expect("reads the status").spaces[0].entities,
		5 + 8 + 1,
		"the user, the person, the device, the replica and the space, the entities and the \
		 portrayal"
	);
	let id = |key: &str| imported.ids[key];
	let show = |entity: EntityId| {
		library
			.entity(personal, entity, today())
			.expect("a space")
			.expect("an entity")
	};

	let film = show(id("Q1"));
	assert_eq!(
		film.name.as_deref(),
		Some("Harry Potter and the Philosopher's Stone")
	);
	assert!(film.types.contains(&nibkeep_std::FILM));
	// The standard vocabulary's fields: of one value, the data's first.
	assert_eq!(
		values(&film, "std.published"),
		[&Value::Date("2001-11-04".parse().expect("a date"))]
	);
	assert_eq!(values(&film, "std.duration"), [&quantity("152 min")]);
	assert_eq!(
		values(&film, "std.in-series"),
		[&Value::Reference(id("Q2"))]
	);
	assert_eq!(values(&film, "std.based-on"), [&Value::Reference(id("Q3"))]);
	assert_eq!(
		show(id("Q2")).name.as_deref(),
		Some("Harry Potter"),
		"named in no language of the reader's, but named"
	);

	// One language: the first name of each, and of texts in several languages, those in
	// the language that comes first, as there may be several values in it.
	let radcliffe = show(id("Q4"));
	assert!(radcliffe.types.contains(&PERSON));
	assert_eq!(
		values(&radcliffe, "std.birth-name"),
		[&Value::Text("Daniel Jacob Radcliffe".to_owned())]
	);
	assert_eq!(
		show(id("Q3")).name.as_deref(),
		Some("Гарри Поттер и философский камень")
	);

	// Links that go round: a father and his child.
	assert_eq!(
		values(&show(id("Q5")), "std.father"),
		[&Value::Reference(id("Q6"))]
	);
	assert_eq!(
		values(&show(id("Q6")), "std.child"),
		[&Value::Reference(id("Q5"))]
	);

	for (query, found) in [
		("radcliffe", "Q4"),
		("философский", "Q3"),
		("henriksson", "mb:a"),
	] {
		let results = library.search(personal, query).expect("searches");
		assert!(
			results.iter().any(|listed| listed.id == id(found)),
			"{query}"
		);
	}
	let report = library.verify(imported.finished_at).expect("verifies");
	assert!(report.spaces.iter().all(|space| space.problems.is_empty()));
}

/// A credit with the character played is a portrayal, and any other a value of the work's
/// field for the role.
#[test]
fn credits_become_portrayals_or_fields_of_works() {
	let data = data("credits", ENTITIES, CREDITS);
	let (library, imported) = import(&data);
	let personal = library.personal_space();
	let id = |key: &str| imported.ids[key];
	let show = |entity: EntityId| {
		library
			.entity(personal, entity, today())
			.expect("a space")
			.expect("an entity")
	};
	// A credit with the character played is a portrayal, which its type's title names.
	let portrayals = library
		.entities(personal, Some(&nibkeep_std::PORTRAYAL))
		.expect("a space");
	assert_eq!(portrayals.len(), 1);
	let cast = show(portrayals[0].id);
	assert_eq!(
		values(&cast, "std.production"),
		[&Value::Reference(id("Q1"))]
	);
	assert_eq!(
		values(&cast, "std.performer"),
		[&Value::Reference(id("Q4"))]
	);
	assert_eq!(
		values(&cast, "std.portrayed"),
		[&Value::Reference(id("Q5"))]
	);
	// Any other credit is a value of the work's field for the role; an audiobook is an
	// edition of the book it reads.
	let audiobook = show(id("mb:g"));
	assert!(audiobook.types.contains(&nibkeep_std::AUDIOBOOK));
	assert!(audiobook.types.contains(&nibkeep_std::EDITION));
	assert_eq!(
		values(&audiobook, "std.narrator"),
		[&Value::Reference(id("mb:a"))]
	);
	assert_eq!(
		values(&audiobook, "std.edition-of"),
		[&Value::Reference(id("Q3"))]
	);
}

/// The data with another line of entities.
fn with_entity(line: &str) -> String {
	format!("{ENTITIES}{line}\n")
}

#[test]
fn a_link_to_what_the_data_does_not_hold_is_refused_with_its_line() {
	let entities = with_entity(
		r#"{"id": "Q7", "kind": "film", "names": [{"text": "Elsewhere"}], "links": {"series": ["Q99"]}}"#,
	);
	let error = refused(&data("a dangling link", &entities, CREDITS));
	assert!(
		matches!(&error, ImportError::Data { file, line: 9, .. } if file == "entities.jsonl"),
		"{error}"
	);
	assert!(error.to_string().contains("Q99"), "{error}");
}

#[test]
fn a_credit_of_what_the_data_does_not_hold_is_refused_with_its_line() {
	let credits = format!(
		"{CREDITS}{}\n",
		r#"{"work": "Q1", "person": "Q98", "role": "cast"}"#
	);
	let error = refused(&data("a dangling credit", ENTITIES, &credits));
	assert!(
		matches!(&error, ImportError::Data { file, line: 3, .. } if file == "credits.jsonl"),
		"{error}"
	);
}

#[test]
fn a_character_for_a_role_that_plays_none_is_refused_with_its_line() {
	let credits = format!(
		"{CREDITS}{}\n",
		r#"{"work": "Q1", "person": "Q4", "role": "director", "character": "Q5"}"#
	);
	let error = refused(&data("a director's character", ENTITIES, &credits));
	assert!(
		matches!(&error, ImportError::Data { file, line: 3, .. } if file == "credits.jsonl"),
		"{error}"
	);
}

#[test]
fn an_id_twice_is_refused() {
	let entities = with_entity(r#"{"id": "Q1", "kind": "film", "names": [{"text": "Again"}]}"#);
	let error = refused(&data("an id twice", &entities, CREDITS));
	assert!(
		matches!(&error, ImportError::Data { line: 9, .. }),
		"{error}"
	);
}

#[test]
fn a_field_with_values_of_two_kinds_is_refused() {
	for (name, line) in [
		(
			"a field of two kinds",
			r#"{"id": "Q7", "kind": "game", "names": [{"text": "A game"}], "numbers": {"published": [2001]}}"#,
		),
		(
			"a field of the vocabulary, of another kind",
			r#"{"id": "Q7", "kind": "person", "names": [{"text": "Someone"}], "numbers": {"born": [2001]}}"#,
		),
	] {
		let error = refused(&data(name, &with_entity(line), CREDITS));
		assert!(
			matches!(&error, ImportError::Data { line: 9, .. }),
			"{name}: {error}"
		);
	}
}

#[test]
fn data_without_a_name_is_refused() {
	let data = data("no name", ENTITIES, CREDITS);
	fs::write(data.join("manifest.json"), "{}").expect("writes");
	let error = refused(&data);
	assert!(
		matches!(&error, ImportError::Data { file, .. } if file == "manifest.json"),
		"{error}"
	);
}

#[test]
fn a_line_that_is_not_json_is_refused_with_its_line() {
	let error = refused(&data("not json", ENTITIES, "{\"work\": \"Q1\"\n"));
	assert!(
		matches!(&error, ImportError::Data { file, line: 1, .. } if file == "credits.jsonl"),
		"{error}"
	);
}

/// Entities with values that held for a time, or were measured at a moment.
const DATED: &str = r#"{"id": "Q2283", "kind": "group", "names": [{"text": "Microsoft"}], "dated": {"chief_executive": [{"link": "Q5284", "valid": "1975-04/2000-01"}, {"link": "Q181162", "valid": "2000-01/2014-02-04"}, {"link": "Q19837", "valid": "2014-02-04/.."}], "employees": [{"number": 118000, "valid": "2015-06-30"}, {"number": 221000, "valid": "2022"}]}}
{"id": "Q5284", "kind": "person", "names": [{"text": "Bill Gates"}]}
{"id": "Q181162", "kind": "person", "names": [{"text": "Steve Ballmer"}]}
{"id": "Q19837", "kind": "person", "names": [{"text": "Satya Nadella"}], "links": {"employer": ["Q2283"]}}
{"id": "sim:bruno", "kind": "animal", "names": [{"text": "Bruno"}], "dated": {"weight": [{"quantity": "4.4 kg", "valid": "2016-05-15T08:22:00+02"}, {"quantity": "4.63 kg", "valid": "2016-05-17"}, {"quantity": "23.1 kg", "valid": "2019-03-12"}]}}
{"id": "sim:leo", "kind": "person", "names": [{"text": "Leo"}], "dated": {"height": [{"quantity": "94.5 cm", "valid": "2016-01-05"}, {"quantity": "40.25 [in_i]", "valid": "2016-07-12"}]}}
"#;

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

fn quantity(text: &str) -> Value {
	Value::Quantity(text.parse::<Quantity>().expect("a quantity"))
}

#[test]
fn dated_values_hold_when_they_held() {
	let data = data("dated values", DATED, "");
	let (mut library, imported) = import(&data);
	let personal = library.personal_space();
	let id = |key: &str| imported.ids[key];
	let on = |key: &str, when: &str, name: &str| {
		let view = library
			.entity(personal, id(key), day(when))
			.expect("a space")
			.expect("an entity");
		values(&view, name).into_iter().cloned().collect::<Vec<_>>()
	};

	// Links that held for a period, each shown while it held.
	let chief = "local.chief-executive";
	assert_eq!(
		on("Q2283", "1990-06-01", chief),
		[Value::Reference(id("Q5284"))]
	);
	assert_eq!(
		on("Q2283", "2005-01-01", chief),
		[Value::Reference(id("Q181162"))]
	);
	assert_eq!(
		on("Q2283", "2026-01-01", chief),
		[Value::Reference(id("Q19837"))]
	);

	// Measurements hold one at a time, each until the next.
	let employees = "local.employees";
	assert_eq!(on("Q2283", "2010-01-01", employees), []);
	assert_eq!(
		on("Q2283", "2016-01-01", employees),
		[Value::Integer(118_000)]
	);
	assert_eq!(
		on("Q2283", "2023-01-01", employees),
		[Value::Integer(221_000)]
	);
	assert_eq!(
		on("sim:bruno", "2016-05-16", "local.weight"),
		[quantity("4.4 kg")]
	);
	assert_eq!(
		on("sim:bruno", "2018-01-01", "local.weight"),
		[quantity("4.63 kg")]
	);
	assert_eq!(
		on("sim:bruno", "2020-01-01", "local.weight"),
		[quantity("23.1 kg")]
	);
	let weights = library
		.history(personal, id("sim:bruno"), Some(&field("local.weight")))
		.expect("a space")
		.expect("a history");
	assert_eq!(weights.fields[0].values.len(), 3);
	assert_eq!(
		weights.fields[0].values[0].valid_time,
		Some("2016-05-15T08:22:00+02".parse().expect("a moment"))
	);
	// Each in the unit it was measured in.
	assert_eq!(
		on("sim:leo", "2016-03-01", "local.height"),
		[quantity("94.5 cm")]
	);
	assert_eq!(
		on("sim:leo", "2016-08-01", "local.height"),
		[quantity("40.25 [in_i]")]
	);

	// Undated links are there on any day.
	assert_eq!(
		on("Q19837", "1950-01-01", "std.employer"),
		[Value::Reference(id("Q2283"))]
	);
	let report = library.verify(imported.finished_at).expect("verifies");
	assert!(report.spaces.iter().all(|space| space.problems.is_empty()));
}

#[test]
fn a_dated_value_that_is_not_one_is_refused_with_its_line() {
	for (name, dated) in [
		("no valid time", r#"{"weight": [{"quantity": "4.4 kg"}]}"#),
		(
			"a valid time that is not a date",
			r#"{"weight": [{"quantity": "4.4 kg", "valid": "soon"}]}"#,
		),
		(
			"a quantity without a unit",
			r#"{"weight": [{"quantity": "4.4", "valid": "2016"}]}"#,
		),
		(
			"neither a link, a number nor a quantity",
			r#"{"weight": [{"text": "heavy", "valid": "2016"}]}"#,
		),
		(
			"a link and a number",
			r#"{"owner": [{"link": "Q1", "number": 5, "valid": "2016"}]}"#,
		),
		(
			"a link to what the data does not hold",
			r#"{"owner": [{"link": "Q99", "valid": "2016/.."}]}"#,
		),
	] {
		let line = format!(
			r#"{{"id": "Q7", "kind": "animal", "names": [{{"text": "Rex"}}], "dated": {dated}}}"#
		);
		let error = refused(&data(name, &with_entity(&line), CREDITS));
		assert!(
			matches!(&error, ImportError::Data { file, line: 9, .. } if file == "entities.jsonl"),
			"{name}: {error}"
		);
	}
}

#[test]
fn a_measurement_with_values_without_dates_is_refused() {
	let entities = format!(
		"{ENTITIES}{}
{}
",
		r#"{"id": "Q7", "kind": "group", "names": [{"text": "A studio"}], "numbers": {"employees": [5]}}"#,
		r#"{"id": "Q8", "kind": "group", "names": [{"text": "Another"}], "dated": {"employees": [{"number": 7, "valid": "2016"}]}}"#,
	);
	let error = refused(&data("a measurement without dates", &entities, CREDITS));
	assert!(
		matches!(&error, ImportError::Data { line: 9, .. }),
		"{error}"
	);
	assert!(error.to_string().contains("local.employees"), "{error}");
}

#[test]
fn languages_are_named_as_the_benchmarks_record_them() {
	assert_eq!(Languages::First.as_str(), "first");
}

#[test]
fn every_language_becomes_forms_and_aliases() {
	assert_eq!(Languages::named("all"), Some(Languages::All));
	assert_eq!(Languages::named("first"), Some(Languages::First));
	assert_eq!(Languages::named("some"), None);
	let data = data("every language", ENTITIES, CREDITS);
	let (library, imported) = RealData::read(&data, Languages::All)
		.expect("reads")
		.import(&library_beside(&data));
	let personal = library.personal_space();
	let show = |key: &str| {
		library
			.entity(personal, imported.ids[key], today())
			.expect("a space")
			.expect("an entity")
	};
	let name = |view: &EntityView| {
		view.fields
			.iter()
			.find(|shown| shown.field == nibkeep_governance::NAME)
			.map(|shown| shown.values[0].clone())
			.expect("a name")
	};

	// Labels in two languages: one name, in two forms, shown in English.
	let film = show("Q1");
	assert_eq!(
		film.name.as_deref(),
		Some("Harry Potter and the Philosopher's Stone")
	);
	assert_eq!(name(&film).language.as_deref(), Some("en"));
	assert_eq!(name(&film).other_forms, 1);
	let found = library
		.search(personal, "философский")
		.expect("searches")
		.into_iter()
		.map(|found| found.id)
		.collect::<Vec<_>>();
	assert!(found.contains(&imported.ids["Q1"]), "by its Russian form");

	// An alias is another name, in its language.
	let series = show("Q2");
	assert_eq!(series.name.as_deref(), Some("Harry Potter"));
	assert_eq!(name(&series).language.as_deref(), Some("mul"));
	let alias = series
		.fields
		.iter()
		.find(|shown| shown.field == field("local.alias"))
		.expect("an alias");
	assert_eq!(
		alias.values[0].value,
		Value::Text("Harry Potter series".to_owned())
	);
	assert_eq!(alias.values[0].language.as_deref(), Some("en"));

	// A field of one value takes the first text in each language, as forms of it.
	let actor = show("Q4");
	let birth_names = actor
		.fields
		.iter()
		.find(|shown| shown.field == field("std.birth-name"))
		.expect("birth names");
	assert_eq!(birth_names.values.len(), 1);
	assert_eq!(birth_names.values[0].language.as_deref(), Some("en"));
	assert_eq!(birth_names.values[0].other_forms, 1);

	// A name without a language has no tag.
	assert_eq!(name(&show("mb:a")).language, None);
}

/// Wikidata repeats aliases in many languages: each is kept once, in the first language
/// that gives it, and none that a form of the name says already.
#[test]
fn an_alias_is_kept_once() {
	let entities = r#"{"id": "Q9", "kind": "group", "names": [{"lang": "en", "text": "Syfy"}, {"lang": "en", "text": "Sci-Fi Channel", "alias": true}, {"lang": "sv", "text": "Sci-Fi Channel", "alias": true}, {"lang": "ru", "text": "Syfy", "alias": true}, {"lang": "ru", "text": "SyFy", "alias": true}]}
"#;
	let data = data("aliases", entities, "");
	let (library, imported) = RealData::read(&data, Languages::All)
		.expect("reads")
		.import(&library_beside(&data));
	let view = library
		.entity(library.personal_space(), imported.ids["Q9"], today())
		.expect("a space")
		.expect("an entity");
	assert!(view.types.contains(&nibkeep_std::ORGANIZATION));
	let mut aliases: Vec<(String, Option<String>)> = view
		.fields
		.iter()
		.filter(|shown| shown.field == field("local.alias"))
		.flat_map(|shown| &shown.values)
		.map(|value| (format!("{:?}", value.value), value.language.clone()))
		.collect();
	aliases.sort();
	assert_eq!(
		aliases,
		[
			(
				r#"Text("Sci-Fi Channel")"#.to_owned(),
				Some("en".to_owned())
			),
			(r#"Text("SyFy")"#.to_owned(), Some("ru".to_owned())),
		]
	);
}

/// The importer's recipe, and what it made of the sample with that recipe.
const RECIPE_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/recipe.txt");

/// A value as text, a reference by the name of what it refers to. Not as `Debug` writes
/// it, which may change with no change to what was imported.
fn as_text(value: &Value, names: &HashMap<EntityId, String>) -> String {
	match value {
		Value::Text(text) => format!("{text:?}"),
		Value::Name(name) => format!("name {:?}", name.full()),
		Value::Boolean(yes) => yes.to_string(),
		Value::Integer(number) => number.to_string(),
		Value::Date(date) => date.to_string(),
		Value::Decimal(decimal) => decimal.to_string(),
		Value::Quantity(quantity) => quantity.to_string(),
		Value::Reference(id) => format!("->{}", names.get(id).map_or("?", String::as_str)),
		other => format!("{other:?}"),
	}
}

/// What the importer makes of a sample of every kind of data, with one name for each
/// entity, as the benchmarks import it: each entity of the data and each credit, on three
/// days, with its types and the values it shows then, each with when it held.
fn imported_as_text() -> String {
	let data = data(
		"the recipe",
		&format!("{ENTITIES}{DATED}"),
		&format!(
			"{CREDITS}{}\n",
			r#"{"work": "Q1", "person": "Q5284", "role": "producer"}"#
		),
	);
	let (library, imported) = import(&data);
	let personal = library.personal_space();
	let mut entities: Vec<EntityId> = imported.ids.values().copied().collect();
	entities.extend(
		library
			.entities(personal, Some(&nibkeep_std::PORTRAYAL))
			.expect("a space")
			.iter()
			.map(|listed| listed.id),
	);
	let show = |id: EntityId, when: Day| {
		library
			.entity(personal, id, when)
			.expect("a space")
			.expect("an entity")
	};
	let names: HashMap<EntityId, String> = entities
		.iter()
		.map(|&id| (id, show(id, today()).name.unwrap_or_default()))
		.collect();
	let mut lines = Vec::new();
	for when in ["1990-06-01", "2016-06-01", "2026-10-15"] {
		for &id in &entities {
			let view = show(id, day(when));
			let types: Vec<String> = view.types.iter().map(ToString::to_string).collect();
			let mut line = format!(
				"{when} {} [{}]",
				view.name.as_deref().unwrap_or("-"),
				types.join(", ")
			);
			for field in &view.fields {
				for shown in &field.values {
					write!(line, " {}={}", field.field, as_text(&shown.value, &names))
						.expect("writes to a string");
					if let Some(valid) = &shown.valid_time {
						write!(line, "@{valid}").expect("writes to a string");
					}
				}
			}
			lines.push(line);
		}
	}
	lines.sort();
	lines.join("\n") + "\n"
}

/// A new recipe starts a new series of the benchmarks, and a change to what the importer
/// imports must start one. `NIBKEEP_BLESS=1` writes the file again, once `RECIPE` is raised.
#[test]
fn a_change_to_what_the_importer_imports_comes_with_a_new_recipe() {
	let now = imported_as_text();
	// Git may have given the file Windows' line endings.
	let written = fs::read_to_string(RECIPE_FILE)
		.unwrap_or_default()
		.replace("\r\n", "\n");
	let (first, before) = written.split_once('\n').unwrap_or_default();
	let recorded: Option<u32> = first
		.strip_prefix("recipe ")
		.and_then(|number| number.parse().ok());
	if before == now {
		assert_eq!(
			recorded,
			Some(RECIPE),
			"RECIPE changed, but what the importer imports did not: put it back"
		);
		return;
	}
	assert!(
		env::var_os("NIBKEEP_BLESS").is_some_and(|bless| bless == "1"),
		"what the importer imports changed, which starts a new series of the benchmarks: \
		 raise RECIPE in src/real.rs, then write tests/recipe.txt again with NIBKEEP_BLESS=1 \
		 cargo test -p nibkeep-generate --test real"
	);
	assert!(
		recorded.is_none_or(|recorded| RECIPE > recorded),
		"what the importer imports changed: raise RECIPE in src/real.rs before writing the file"
	);
	fs::write(RECIPE_FILE, format!("recipe {RECIPE}\n{now}")).expect("writes the recipe's file");
}
