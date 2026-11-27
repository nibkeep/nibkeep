//! Languages and scripts (`docs/spec/model.md`, section 11): language tags on values, forms
//! of one value, and the form and order that a reader's ranked languages choose.

use crate::common::{at, entity, field, statement, statement_id, text};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{
	FieldDef, Languages, Operation, Qualifiers, Resolution, State, Tag, Value, ValueKind, fields,
};
use nibkeep_time::Day;

const FIELD: &str = "org.example.field";

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

fn languages(ranges: &str) -> Languages {
	ranges.parse().expect("ranges")
}

fn tag(text: &str) -> Tag {
	Tag::parse(text).expect("a tag")
}

/// Statement `id` with `value`, in `language` if given, as another form of statement
/// `form_of` if given, and dated `time` if given.
fn form(
	id: u16,
	value: &str,
	language: Option<&str>,
	form_of: Option<u16>,
	time: Option<&str>,
) -> Operation {
	let mut qualifiers = Qualifiers::new();
	if let Some(language) = language {
		qualifiers.insert(fields::LANGUAGE, text(language));
	}
	if let Some(first) = form_of {
		qualifiers.insert(
			fields::ALTERNATIVE,
			Value::Text(statement_id(first).to_string()),
		);
	}
	if let Some(time) = time {
		qualifiers.insert(
			fields::VALID_TIME,
			Value::Date(time.parse().expect("a valid time")),
		);
	}
	Operation::assert(statement(id, 1, FIELD, text(value)).with_qualifiers(qualifiers))
}

fn apply(operations: &[Operation]) -> State {
	let mut state = State::new();
	for (n, operation) in (1..).zip(operations) {
		state.apply(operation, at(n)).infallible();
	}
	state
}

fn one() -> FieldDef {
	FieldDef::new(ValueKind::Text)
}

fn many() -> FieldDef {
	FieldDef::new(ValueKind::Text).many()
}

/// What the field shows on `on` to a reader of `ranges`, and whether its values compete.
fn shown(state: &State, definition: &FieldDef, on: &str, ranges: &str) -> (Vec<String>, bool) {
	let texts = |statements: Vec<std::borrow::Cow<'_, nibkeep_model::Statement>>| {
		statements
			.iter()
			.map(|statement| statement.value.as_text().expect("text").to_owned())
			.collect()
	};
	match state
		.resolve_for(
			entity(1),
			&field(FIELD),
			Some(definition),
			day(on),
			&languages(ranges),
		)
		.infallible()
	{
		Resolution::Values(statements) => (texts(statements), false),
		Resolution::Competing(statements) => (texts(statements), true),
	}
}

/// The one value the field shows on `on` to a reader of `ranges`.
fn the_one(state: &State, definition: &FieldDef, on: &str, ranges: &str) -> String {
	let (values, competing) = shown(state, definition, on, ranges);
	assert!(!competing, "{values:?} compete");
	let [value] = values.as_slice() else {
		panic!("one value, not {values:?}");
	};
	value.clone()
}

#[test]
fn tags_are_read_without_regard_to_case_and_others_refused() {
	assert_eq!(tag("UK-latn").to_string(), "uk-Latn");
	assert_eq!(
		tag("uk-Latn-t-uk")
			.source()
			.map(ToString::to_string)
			.as_deref(),
		Some("uk")
	);
	assert!(!tag("uk-Latn-t-uk").is_machine());
	assert!(tag("en-t-ru-t0-und").is_machine());
	assert_eq!(
		tag("en-t-ru-t0-und")
			.source()
			.map(ToString::to_string)
			.as_deref(),
		Some("ru")
	);
	assert!(tag("en").source().is_none());
	assert!(tag("en").is_known());
	assert!(!tag("und").is_known(), "the tag of an unknown language");
	for text in ["", "not a tag", "x-private", "en_US"] {
		assert!(Tag::parse(text).is_none(), "{text}");
	}
}

#[test]
fn ranges_match_by_language_and_likely_script() {
	let ranked = languages("en, *-Latn, ru");
	assert_eq!(ranked.to_string(), "en, *-Latn, ru");
	assert_eq!(ranked.len(), 3);
	let rank = |text: &str| ranked.rank(Tag::parse(text).as_ref());
	assert_eq!(rank("en"), 0);
	assert_eq!(rank("en-GB"), 0, "a range matches a tag with more subtags");
	assert_eq!(rank("EN-latn-us"), 0);
	assert_eq!(rank("sv"), 1, "Swedish is written in the Latin script");
	assert_eq!(rank("uk-Latn-t-uk"), 1);
	assert_eq!(rank("ru"), 2);
	assert_eq!(rank("uk"), 3, "Ukrainian is written in Cyrillic");
	assert_eq!(
		rank("en-Cyrl"),
		3,
		"English in Cyrillic is not what `en` asks for"
	);
	assert_eq!(ranked.rank(None), 3, "no tag matches after every range");
	assert_eq!(rank("und"), 3);

	// A language without a script is matched with its likely script.
	let ukrainian = languages("uk");
	assert_eq!(ukrainian.rank(Some(&tag("uk-Cyrl-UA"))), 0);
	assert_eq!(ukrainian.rank(Some(&tag("uk-Latn"))), 1);
	assert_eq!(languages("en-GB").rank(Some(&tag("en"))), 1);
	assert_eq!(languages("*").rank(Some(&tag("ja"))), 0);

	assert!(Languages::default().is_empty());
	assert_eq!(languages("").len(), 0);
	assert_eq!(languages(" en ,ru ").to_string(), "en, ru");
	for text in ["en, not a range", "en,,ru", "x-private"] {
		assert!(text.parse::<Languages>().is_err(), "{text}");
	}
}

/// A name in Ukrainian, its passport transliteration, the same name in Russian, and its
/// transliteration from Russian: four forms of one value.
fn four_forms() -> Vec<Operation> {
	vec![
		form(1, "Олена Коваль", Some("uk"), None, None),
		form(2, "Olena Koval", Some("uk-Latn-t-uk"), Some(1), None),
		form(3, "Елена Коваль", Some("ru"), Some(1), None),
		form(4, "Elena Koval", Some("ru-Latn-t-ru"), Some(1), None),
	]
}

#[test]
fn forms_of_a_value_never_compete_and_the_reader_sees_the_best() {
	let state = apply(&four_forms());
	let name = |ranges: &str| the_one(&state, &one(), "2026-10-22", ranges);
	assert_eq!(
		name(""),
		"Олена Коваль",
		"the first form, for a reader who ranks none"
	);
	assert_eq!(name("uk"), "Олена Коваль");
	assert_eq!(name("ru, uk"), "Елена Коваль");
	// Both transliterations are in the Latin script: the one made from a ranked language
	// comes first, and of two, the one made from the language ranked earlier.
	assert_eq!(name("en, *-Latn, uk, ru"), "Olena Koval");
	assert_eq!(name("en, *-Latn, ru, uk"), "Elena Koval");
	assert_eq!(name("en, *-Latn, ru"), "Elena Koval");
	assert_eq!(
		name("en, *-Latn"),
		"Olena Koval",
		"of equals, the smallest ID"
	);
	assert_eq!(name("sv"), "Олена Коваль", "nothing matches: what there is");
	// In a field with many values, they are one value all the same.
	assert_eq!(
		shown(&state, &many(), "2026-10-22", "en, *-Latn, uk"),
		(vec!["Olena Koval".to_owned()], false)
	);

	let forms = state
		.forms(entity(1), &field(FIELD), statement_id(3))
		.infallible();
	assert_eq!(
		forms.iter().map(|form| form.id).collect::<Vec<_>>(),
		(1..=4).map(statement_id).collect::<Vec<_>>()
	);
	nibkeep_testkit::assert_empty!(
		state
			.forms(entity(1), &field(FIELD), statement_id(9))
			.infallible()
	);
}

#[test]
fn a_form_marked_preferred_comes_before_others_that_match_as_well() {
	let mut operations = four_forms();
	operations.push(Operation::assert(
		statement(5, 1, FIELD, text("Olena Kovalj")).with_qualifiers(
			Qualifiers::new()
				.with(fields::LANGUAGE, text("uk-Latn-t-uk"))
				.with(
					fields::ALTERNATIVE,
					Value::Text(statement_id(1).to_string()),
				)
				.with(fields::PREFERRED, Value::Boolean(true)),
		),
	));
	let state = apply(&operations);
	assert_eq!(
		the_one(&state, &one(), "2026-10-22", "en, *-Latn, uk"),
		"Olena Kovalj"
	);
	assert_eq!(
		the_one(&state, &one(), "2026-10-22", "uk"),
		"Олена Коваль",
		"a better match comes before a preferred form"
	);
}

#[test]
fn of_forms_that_match_as_well_the_first_comes_before_a_smaller_id() {
	// The Russian form was added to the English one, and drew a smaller ID.
	let state = apply(&[
		form(5, "Elena Koval", Some("en"), None, None),
		form(2, "Елена Коваль", Some("ru"), Some(5), None),
	]);
	let name = |ranges: &str| the_one(&state, &one(), "2026-10-22", ranges);
	assert_eq!(name(""), "Elena Koval", "for a reader who ranks none");
	assert_eq!(name("sv"), "Elena Koval", "nothing matches");
	assert_eq!(name("ru"), "Елена Коваль");
}

#[test]
fn a_machine_translation_never_replaces_what_a_reader_can_read() {
	let state = apply(&[
		form(1, "Заметки", Some("ru"), None, None),
		form(2, "Notes", Some("en-t-ru-t0-und"), Some(1), None),
	]);
	let note = |ranges: &str| the_one(&state, &one(), "2026-10-22", ranges);
	assert_eq!(note("en, ru"), "Заметки", "the reader reads Russian");
	assert_eq!(note("en, sv"), "Notes", "the reader reads no Russian");
	assert_eq!(note(""), "Заметки");

	// What a person wrote in an unknown language may be one the reader reads.
	let state = apply(&[
		form(1, "Заметки", None, None, None),
		form(2, "Notes", Some("en-t-ru-t0-und"), Some(1), None),
	]);
	assert_eq!(the_one(&state, &one(), "2026-10-22", "en"), "Заметки");

	// A translation that a person made is what a person wrote.
	let state = apply(&[
		form(1, "Заметки", Some("ru"), None, None),
		form(2, "Notes", Some("en-t-ru"), Some(1), None),
	]);
	assert_eq!(the_one(&state, &one(), "2026-10-22", "en, ru"), "Notes");
}

#[test]
fn values_of_a_field_with_many_come_in_the_readers_languages_first() {
	let state = apply(&[
		form(1, "red", Some("en"), None, None),
		form(2, "красный", Some("ru"), None, None),
		form(3, "röd", Some("sv"), None, None),
	]);
	let values = |ranges: &str| shown(&state, &many(), "2026-10-22", ranges).0;
	assert_eq!(values("ru, en"), ["красный", "red", "röd"]);
	assert_eq!(values("sv, ru"), ["röd", "красный", "red"]);
	// Without ranges, those that started latest come first, then by ID.
	assert_eq!(values(""), ["red", "красный", "röd"]);

	// The value marked preferred comes first all the same.
	let mut operations = vec![
		form(1, "red", Some("en"), None, None),
		form(2, "красный", Some("ru"), None, None),
	];
	operations.push(Operation::assert(
		statement(3, 1, FIELD, text("blue"))
			.with_qualifiers(Qualifiers::new().with(fields::PREFERRED, Value::Boolean(true))),
	));
	let state = apply(&operations);
	assert_eq!(
		shown(&state, &many(), "2026-10-22", "ru").0,
		["blue", "красный", "red"]
	);
}

#[test]
fn forms_take_one_place_in_time_and_keep_their_group_when_corrected() {
	let mut operations = vec![
		form(1, "Alpha", Some("uk"), None, Some("2000")),
		form(2, "Alpha-lat", Some("uk-Latn-t-uk"), Some(1), Some("2000")),
		form(3, "Beta", Some("uk"), None, Some("2010")),
	];
	let state = apply(&operations);
	assert_eq!(the_one(&state, &one(), "2005-01-01", "uk"), "Alpha");
	assert_eq!(the_one(&state, &one(), "2005-01-01", "*-Latn"), "Alpha-lat");
	assert_eq!(the_one(&state, &one(), "2015-01-01", "*-Latn"), "Beta");

	// Correcting the transliteration names the first form, as the one it corrects did.
	operations.push(Operation::replace(
		statement(5, 1, FIELD, text("Alpha-latin")).with_qualifiers(
			Qualifiers::new()
				.with(fields::LANGUAGE, text("uk-Latn-t-uk"))
				.with(
					fields::ALTERNATIVE,
					Value::Text(statement_id(1).to_string()),
				)
				.with(
					fields::VALID_TIME,
					Value::Date("2000".parse().expect("a date")),
				),
		),
		[statement_id(2)],
	));
	let state = apply(&operations);
	assert_eq!(
		the_one(&state, &one(), "2005-01-01", "*-Latn"),
		"Alpha-latin"
	);
	assert_eq!(
		state
			.forms(entity(1), &field(FIELD), statement_id(1))
			.infallible()
			.len(),
		2
	);

	// Correcting the first form keeps the group too, with the first form's ID.
	operations.push(Operation::replace(
		statement(6, 1, FIELD, text("Alfa")).with_qualifiers(
			Qualifiers::new()
				.with(fields::LANGUAGE, text("uk"))
				.with(
					fields::ALTERNATIVE,
					Value::Text(statement_id(1).to_string()),
				)
				.with(
					fields::VALID_TIME,
					Value::Date("2000".parse().expect("a date")),
				),
		),
		[statement_id(1)],
	));
	let state = apply(&operations);
	assert_eq!(the_one(&state, &one(), "2005-01-01", "uk"), "Alfa");
	assert_eq!(
		the_one(&state, &one(), "2005-01-01", "*-Latn"),
		"Alpha-latin"
	);
}

#[test]
fn forms_add_up_whatever_order_they_arrive_in() {
	let operations = four_forms();
	let reversed: Vec<Operation> = operations.iter().rev().cloned().collect();
	let (forward, backward) = (apply(&operations), apply(&reversed));
	for ranges in ["", "uk", "ru", "en, *-Latn, uk", "en, *-Latn, ru"] {
		assert_eq!(
			shown(&forward, &one(), "2026-10-22", ranges),
			shown(&backward, &one(), "2026-10-22", ranges),
			"{ranges}"
		);
	}
}

#[test]
fn a_form_that_names_a_repeated_value_joins_it() {
	// Two replicas wrote the same name; a third added a form of the second's statement.
	let state = apply(&[
		form(1, "Anna", None, None, None),
		form(2, "Anna", None, None, None),
		form(3, "Анна", Some("ru"), Some(2), None),
	]);
	assert_eq!(the_one(&state, &one(), "2026-10-22", "ru"), "Анна");
	assert_eq!(the_one(&state, &one(), "2026-10-22", "en"), "Anna");
	assert_eq!(
		state
			.forms(entity(1), &field(FIELD), statement_id(2))
			.infallible()
			.len(),
		2,
		"Anna, once, and Анна"
	);
	assert_eq!(
		state
			.form_groups(entity(1), &field(FIELD))
			.infallible()
			.len(),
		1
	);
}

#[test]
fn a_field_whose_last_writer_wins_shows_the_last_value_in_the_readers_form() {
	let lww = FieldDef::new(ValueKind::Text).last_writer_wins();
	let state = apply(&[
		form(1, "Hello", Some("en"), None, None),
		form(2, "Привет", Some("ru"), Some(1), None),
	]);
	assert_eq!(the_one(&state, &lww, "2026-10-22", "en"), "Hello");
	assert_eq!(the_one(&state, &lww, "2026-10-22", "ru"), "Привет");
	let state = apply(&[
		form(1, "Hello", Some("en"), None, None),
		form(2, "Привет", Some("ru"), Some(1), None),
		form(3, "Goodbye", Some("en"), None, None),
	]);
	assert_eq!(the_one(&state, &lww, "2026-10-22", "ru"), "Goodbye");
}
