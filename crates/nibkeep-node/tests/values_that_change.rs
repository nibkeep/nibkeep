//! Values that change over time, through the engine: a change in the world, a correction,
//! and a correction in the past (`docs/spec/model.md`, section 6).

mod common;

use common::{library, now, today};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Dating, Edit, Library};
use nibkeep_testkit::TestRng;
use nibkeep_time::{Day, Edtf, Presence};

const PERSON: TypeName = TypeName::from_static("local.person");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

fn edtf(text: &str) -> Edtf {
	text.parse().expect("a date")
}

/// A library with a person whose employer has one value at a time and phones many.
fn with_a_person(test: &str, rng: &mut TestRng) -> (Library, EntityId) {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	let person = TypeDef::new(PERSON, 1)
		.with_field(name("local.employer"), FieldDef::new(ValueKind::Text))
		.with_field(name("local.phone"), FieldDef::new(ValueKind::Text).many());
	library
		.define_type(personal, &person, now(), rng)
		.expect("defines");
	let entity = library
		.create_entity(personal, &[PERSON], Vec::new(), now(), rng)
		.expect("creates");
	(library, entity)
}

fn edit(
	library: &mut Library,
	entity: EntityId,
	edits: Vec<Edit>,
	dating: &Dating,
	rng: &mut TestRng,
) {
	let personal = library.personal_space();
	library
		.edit_entity(personal, entity, edits, dating, now(), rng)
		.expect("edits");
}

/// The values a field shows on `on`, each with its valid time.
fn shown(
	library: &Library,
	entity: EntityId,
	field: &str,
	on: Day,
) -> Vec<(Value, Option<String>)> {
	let view = library
		.entity(library.personal_space(), entity, on)
		.expect("a space")
		.expect("the entity");
	view.fields
		.iter()
		.find(|view| view.field == name(field))
		.map_or_else(Vec::new, |view| {
			view.values
				.iter()
				.map(|shown| {
					(
						shown.value.clone(),
						shown.valid_time.as_ref().map(ToString::to_string),
					)
				})
				.collect()
		})
}

fn dated(value: &str, valid_time: &str) -> (Value, Option<String>) {
	(text(value), Some(valid_time.to_owned()))
}

/// A person who worked at Acme from 1990 to 2015, and at Globex since March 2019.
fn employed(test: &str, rng: &mut TestRng) -> (Library, EntityId) {
	let (mut library, person) = with_a_person(test, rng);
	let employer = || name("local.employer");
	edit(
		&mut library,
		person,
		vec![Edit::Add(employer(), text("Acme"))],
		&today().with_valid_time(edtf("1990/2015")),
		rng,
	);
	edit(
		&mut library,
		person,
		vec![Edit::Add(employer(), text("Globex"))],
		&today().with_valid_time(edtf("2019-03")),
		rng,
	);
	(library, person)
}

#[test]
fn a_change_in_the_world_adds_a_dated_value_and_ends_the_one_before() {
	let mut rng = TestRng::new("a change in the world");
	let (library, person) = employed("a change in the world", &mut rng);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2000-01-01"), [dated("Acme", "1990/2015")]);
	assert_eq!(on("2016-06-01"), []);
	assert_eq!(on("2026-10-12"), [dated("Globex", "2019-03")]);
}

#[test]
fn a_correction_replaces_what_shows_and_keeps_its_date() {
	let mut rng = TestRng::new("a correction");
	let (mut library, person) = employed("a correction", &mut rng);
	edit(
		&mut library,
		person,
		vec![Edit::Set(name("local.employer"), vec![text("Globex Inc")])],
		&today(),
		&mut rng,
	);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2026-10-12"), [dated("Globex Inc", "2019-03")]);
	// The past stays as it was.
	assert_eq!(on("2000-01-01"), [dated("Acme", "1990/2015")]);
}

#[test]
fn a_correction_in_the_past_replaces_only_that_value() {
	let mut rng = TestRng::new("a correction in the past");
	let (mut library, person) = employed("a correction in the past", &mut rng);
	edit(
		&mut library,
		person,
		vec![Edit::Set(name("local.employer"), vec![text("ACME")])],
		&Dating::on(day("2000-01-01")),
		&mut rng,
	);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2000-01-01"), [dated("ACME", "1990/2015")]);
	assert_eq!(on("2026-10-12"), [dated("Globex", "2019-03")]);
}

#[test]
fn a_value_set_on_a_day_that_shows_nothing_starts_on_that_day() {
	let mut rng = TestRng::new("set on an empty day");
	let (mut library, person) = employed("set on an empty day", &mut rng);
	edit(
		&mut library,
		person,
		vec![Edit::Set(name("local.employer"), vec![text("freelance")])],
		&Dating::on(day("2017-05-01")),
		&mut rng,
	);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2017-06-01"), [dated("freelance", "2017-05-01")]);
	assert_eq!(on("2026-10-12"), [dated("Globex", "2019-03")]);
}

#[test]
fn a_value_added_without_a_date_to_a_dated_field_starts_now() {
	let mut rng = TestRng::new("added now");
	let (mut library, person) = employed("added now", &mut rng);
	edit(
		&mut library,
		person,
		vec![Edit::Add(name("local.employer"), text("Initech"))],
		&today(),
		&mut rng,
	);
	// 10:00 UTC is noon two hours east of it.
	assert_eq!(
		shown(&library, person, "local.employer", day("2026-10-12")),
		[dated("Initech", "2026-10-12T12:00:00+02")]
	);
}

#[test]
fn a_field_with_many_values_keeps_what_belongs_to_other_days() {
	let mut rng = TestRng::new("many values over time");
	let (mut library, person) = with_a_person("many values over time", &mut rng);
	let phone = || name("local.phone");
	edit(
		&mut library,
		person,
		vec![Edit::Add(phone(), text("home"))],
		&today().with_valid_time(edtf("1985/2003")),
		&mut rng,
	);
	edit(
		&mut library,
		person,
		vec![Edit::Add(phone(), text("mobile"))],
		&today().with_valid_time(edtf("2003-05")),
		&mut rng,
	);
	edit(
		&mut library,
		person,
		vec![Edit::Set(phone(), vec![text("work")])],
		&today(),
		&mut rng,
	);
	// Setting replaced what showed today, the mobile phone, and kept the home phone of the
	// past. A field with many values gives an undated value no date, so it shows on every
	// day, after the dated ones.
	let on = |date| shown(&library, person, "local.phone", day(date));
	assert_eq!(on("2026-10-12"), [(text("work"), None)]);
	assert_eq!(
		on("1990-01-01"),
		[dated("home", "1985/2003"), (text("work"), None)]
	);
}

#[test]
fn removing_a_value_removes_it_on_the_day_shown_only() {
	let mut rng = TestRng::new("removed on a day");
	let (mut library, person) = with_a_person("removed on a day", &mut rng);
	let employer = || name("local.employer");
	for period in ["1990/2015", "2019-03"] {
		edit(
			&mut library,
			person,
			vec![Edit::Add(employer(), text("Acme"))],
			&today().with_valid_time(edtf(period)),
			&mut rng,
		);
	}
	edit(
		&mut library,
		person,
		vec![Edit::Remove(employer(), text("Acme"))],
		&Dating::on(day("2000-01-01")),
		&mut rng,
	);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2000-01-01"), []);
	assert_eq!(on("2026-10-12"), [dated("Acme", "2019-03")]);
}

#[test]
fn a_value_added_on_another_day_starts_on_that_day() {
	let mut rng = TestRng::new("added on a day");
	let (mut library, person) = with_a_person("added on a day", &mut rng);
	let employer = || name("local.employer");
	edit(
		&mut library,
		person,
		vec![Edit::Add(employer(), text("Beta"))],
		&today(),
		&mut rng,
	);
	edit(
		&mut library,
		person,
		vec![Edit::Add(employer(), text("Acme"))],
		&Dating::on(day("2020-01-01")),
		&mut rng,
	);
	let on = |date| shown(&library, person, "local.employer", day(date));
	assert_eq!(on("2019-01-01"), [(text("Beta"), None)]);
	assert_eq!(on("2026-10-12"), [dated("Acme", "2020-01-01")]);
}

#[test]
fn the_name_of_an_entity_is_its_name_on_the_day_shown() {
	let mut rng = TestRng::new("names on a day");
	let (mut library, person) = with_a_person("names on a day", &mut rng);
	let personal = library.personal_space();
	let name_field = || "org.nibkeep.name".parse::<FieldName>().expect("a field");
	for (value, period) in [("Anna Smith", "1950-03-02"), ("Anna Jones", "1975-06-14")] {
		edit(
			&mut library,
			person,
			vec![Edit::Add(name_field(), text(value))],
			&today().with_valid_time(edtf(period)),
			&mut rng,
		);
	}
	let name_on = |date| {
		library
			.entity(personal, person, day(date))
			.expect("a space")
			.expect("the entity")
			.name
	};
	assert_eq!(name_on("1960-01-01").as_deref(), Some("Anna Smith"));
	assert_eq!(name_on("2026-10-12").as_deref(), Some("Anna Jones"));
}

#[test]
fn setting_a_field_to_what_it_shows_changes_nothing_even_with_repeats() {
	let mut rng = TestRng::new("repeats");
	let (mut library, person) = with_a_person("repeats", &mut rng);
	let employer = || name("local.employer");
	for _ in 0..2 {
		edit(
			&mut library,
			person,
			vec![Edit::Add(employer(), text("Acme"))],
			&today().with_valid_time(edtf("2019-03")),
			&mut rng,
		);
	}
	let changes = |library: &Library| library.status().expect("reads the status").spaces[0].changes;
	let before = changes(&library);
	edit(
		&mut library,
		person,
		vec![Edit::Set(employer(), vec![text("Acme")])],
		&today(),
		&mut rng,
	);
	assert_eq!(changes(&library), before);
	assert_eq!(
		shown(&library, person, "local.employer", day("2026-10-12")),
		[dated("Acme", "2019-03")]
	);
}

#[test]
fn a_value_that_may_not_have_happened_yet_is_shown_as_uncertain() {
	let mut rng = TestRng::new("uncertain");
	let (mut library, person) = with_a_person("uncertain", &mut rng);
	edit(
		&mut library,
		person,
		vec![Edit::Add(name("local.employer"), text("Acme"))],
		&today().with_valid_time(edtf("2010-06~")),
		&mut rng,
	);
	let presence = |date| {
		let view = library
			.entity(library.personal_space(), person, day(date))
			.expect("a space")
			.expect("the entity");
		view.fields
			.iter()
			.find(|field| field.field == name("local.employer"))
			.map(|field| field.values[0].presence)
	};
	assert_eq!(presence("2010-06-10"), Some(Presence::Uncertain));
	assert_eq!(presence("2011-01-01"), Some(Presence::Yes));
}
