//! What a field shows on a day (`docs/spec/model.md`, section 6): in a field with one value,
//! each value until the next one starts; in a field with many, each by its own valid time.

use crate::common::{at, entity, field, statement, text};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{
	FieldDef, Operation, Qualifiers, Resolution, State, Statement, Value, ValueKind, fields,
};
use nibkeep_time::Day;

const FIELD: &str = "org.example.field";

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

fn valid(time: &str) -> Qualifiers {
	Qualifiers::new().with(
		fields::VALID_TIME,
		Value::Date(time.parse().expect("a valid time")),
	)
}

/// Asserts statement `id` with `value`, dated `time`, or undated without one.
fn value(id: u16, value: &str, time: Option<&str>) -> Operation {
	let statement = statement(id, 1, FIELD, text(value));
	Operation::assert(match time {
		Some(time) => statement.with_qualifiers(valid(time)),
		None => statement,
	})
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

/// What the field shows on `on`, and whether its values compete.
fn shown(state: &State, definition: Option<&FieldDef>, on: &str) -> (Vec<Value>, bool) {
	let values = |statements: Vec<std::borrow::Cow<'_, Statement>>| {
		statements
			.into_iter()
			.map(|statement| statement.value.clone())
			.collect()
	};
	match state
		.resolve_on(entity(1), &field(FIELD), definition, day(on))
		.infallible()
	{
		Resolution::Values(statements) => (values(statements), false),
		Resolution::Competing(statements) => (values(statements), true),
	}
}

fn texts(values: &[&str]) -> Vec<Value> {
	values.iter().map(|value| text(value)).collect()
}

#[test]
fn each_value_shows_until_the_next_one_starts_or_its_own_period_ends() {
	let state = apply(&[
		value(1, "Acme", Some("1990/2015")),
		value(2, "Globex", Some("2019-03")),
	]);
	let on = |date| shown(&state, Some(&one()), date).0;
	assert_eq!(on("1985-01-01"), texts(&[]));
	assert_eq!(on("2000-01-01"), texts(&["Acme"]));
	// Acme's own period ended in 2015, which leaves a gap until Globex.
	assert_eq!(on("2016-06-01"), texts(&[]));
	assert_eq!(on("2019-02-28"), texts(&[]));
	assert_eq!(on("2019-03-01"), texts(&["Globex"]));
	assert_eq!(on("2026-10-14"), texts(&["Globex"]));
}

#[test]
fn a_change_needs_no_end_date() {
	let state = apply(&[
		value(1, "Anna Smith", Some("1950-03-02")),
		value(2, "Anna Jones", Some("1975-06-14")),
	]);
	let on = |date| shown(&state, Some(&one()), date).0;
	assert_eq!(on("1960-01-01"), texts(&["Anna Smith"]));
	assert_eq!(on("1975-06-13"), texts(&["Anna Smith"]));
	assert_eq!(on("1975-06-14"), texts(&["Anna Jones"]));
	assert_eq!(on("2000-01-01"), texts(&["Anna Jones"]));
}

#[test]
fn an_undated_value_counts_as_earlier_than_every_dated_one() {
	let state = apply(&[value(1, "Acme", None), value(2, "Globex", Some("2019-03"))]);
	let on = |date| shown(&state, Some(&one()), date).0;
	assert_eq!(on("2010-01-01"), texts(&["Acme"]));
	assert_eq!(on("2020-01-01"), texts(&["Globex"]));
}

#[test]
fn readings_of_one_day_follow_their_times() {
	let state = apply(&[
		value(1, "12.0", Some("2026-10-14")),
		value(2, "12.4", Some("2026-10-14T19:05:00+02")),
		value(3, "12.1", Some("2026-10-14T08:00:00+02")),
	]);
	assert_eq!(
		shown(&state, Some(&one()), "2026-10-14"),
		(texts(&["12.4"]), false)
	);
}

#[test]
fn values_that_start_at_the_same_time_compete() {
	let state = apply(&[
		value(1, "12.1", Some("2026-10-14")),
		value(2, "12.4", Some("2026-10-14")),
	]);
	assert_eq!(
		shown(&state, Some(&one()), "2026-10-14"),
		(texts(&["12.1", "12.4"]), true)
	);
}

#[test]
fn the_same_value_recorded_twice_shows_once() {
	let state = apply(&[
		value(1, "Globex", Some("2019-03")),
		value(2, "Globex", Some("2019-03")),
		value(3, "Chess", None),
		value(4, "Chess", None),
	]);
	assert_eq!(
		shown(&state, Some(&one()), "2020-01-01"),
		(texts(&["Globex"]), false)
	);
	assert_eq!(
		shown(&state, Some(&many()), "2020-01-01"),
		(texts(&["Globex", "Chess"]), false)
	);
}

#[test]
fn a_valid_time_that_cannot_be_read_stays_out_of_the_order() {
	let unreadable = statement(2, 1, FIELD, text("Initech"))
		.with_qualifiers(Qualifiers::new().with(fields::VALID_TIME, text("someday")));
	let state = apply(&[
		value(1, "Globex", Some("2019-03")),
		Operation::assert(unreadable),
	]);
	// It is shown on every day, beside the value that holds, and stops nothing.
	assert_eq!(
		shown(&state, Some(&one()), "2010-01-01"),
		(texts(&["Initech"]), false)
	);
	assert_eq!(
		shown(&state, Some(&one()), "2020-01-01"),
		(texts(&["Globex", "Initech"]), true)
	);
}

#[test]
fn in_a_field_with_many_values_states_hold_for_their_period_and_events_from_their_day() {
	let state = apply(&[
		value(1, "member of the band", Some("1975/1981")),
		value(2, "award", Some("2004")),
		value(3, "nickname", None),
	]);
	let on = |date| shown(&state, Some(&many()), date).0;
	assert_eq!(on("1978-01-01"), texts(&["member of the band", "nickname"]));
	assert_eq!(on("1990-01-01"), texts(&["nickname"]));
	// An event stays: the award shows from 2004 on, the latest started first.
	assert_eq!(on("2010-01-01"), texts(&["award", "nickname"]));
}

#[test]
fn the_preferred_value_shows_first_and_otherwise_the_latest() {
	let home = statement(1, 1, FIELD, text("home")).with_qualifiers(valid("1985-04"));
	let mobile = statement(2, 1, FIELD, text("mobile")).with_qualifiers(valid("2003-05"));
	let state = apply(&[
		Operation::assert(home.clone()),
		Operation::assert(mobile.clone()),
	]);
	assert_eq!(
		shown(&state, Some(&many()), "2026-10-14").0,
		texts(&["mobile", "home"])
	);
	let preferred =
		home.with_qualifiers(valid("1985-04").with(fields::PREFERRED, Value::Boolean(true)));
	let state = apply(&[Operation::assert(preferred), Operation::assert(mobile)]);
	assert_eq!(
		shown(&state, Some(&many()), "2026-10-14").0,
		texts(&["home", "mobile"])
	);
}

#[test]
fn a_field_that_no_type_defines_shows_its_values_as_one_with_many_values_does() {
	let state = apply(&[
		value(1, "Acme", Some("1990/2015")),
		value(2, "Globex", Some("2019-03")),
	]);
	// Each value holds for its own valid time: Acme's period, and Globex from its day on.
	assert_eq!(shown(&state, None, "2000-01-01").0, texts(&["Acme"]));
	assert_eq!(shown(&state, None, "2020-01-01").0, texts(&["Globex"]));
}

#[test]
fn a_field_whose_last_writer_wins_ignores_valid_time() {
	let state = apply(&[
		value(1, "first", Some("2030")),
		value(2, "second", Some("1990")),
	]);
	let latest = FieldDef::new(ValueKind::Text).last_writer_wins();
	assert_eq!(
		shown(&state, Some(&latest), "2000-01-01"),
		(texts(&["second"]), false)
	);
}
