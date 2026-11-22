//! Where a date starts, as written, which orders the values of a field that changes over
//! time; and the date and time of a moment, as a device writes it.

use nibkeep_time::{Day, Edtf, Start};

fn day(text: &str) -> Day {
	text.parse().expect("valid day")
}

fn edtf(text: &str) -> Edtf {
	text.parse().expect("valid EDTF")
}

fn start(text: &str) -> Start {
	edtf(text).start()
}

#[test]
fn a_date_starts_on_the_first_day_it_names_before_qualifiers_widen_it() {
	assert_eq!(start("2010-06"), start("2010-06~"));
	assert_eq!(
		start("2010-06~"),
		Start::At {
			day: day("2010-06-01"),
			second: None
		}
	);
	assert_eq!(start("201X"), start("2010-01-01"));
	assert_eq!(start("1985-21"), start("1985-03-01"));
	// Approximately June 2010 starts after an exact day in May.
	assert!(start("2010-06~") > start("2010-05-20"));
}

#[test]
fn a_date_and_time_starts_at_its_time_as_written_whatever_its_offset() {
	assert!(start("2026-10-14T19:05:00+02") > start("2026-10-14T08:00:00Z"));
	assert!(start("2026-10-15T01:00:00+02") > start("2026-10-14T23:30:00-05"));
	assert_eq!(
		start("2026-10-14T19:05:00+02"),
		Start::At {
			day: day("2026-10-14"),
			second: Some(19 * 3600 + 5 * 60)
		}
	);
}

#[test]
fn a_date_alone_comes_before_the_times_of_its_day() {
	assert!(start("2026-10-14") < start("2026-10-14T00:00:00"));
	assert!(start("2026-10-14T23:59:59") < start("2026-10-15"));
}

#[test]
fn an_interval_starts_where_its_start_does() {
	assert_eq!(start("1990/2015"), start("1990"));
	assert_eq!(start("2004-06/.."), start("2004-06"));
	assert_eq!(start("../2015"), Start::BeginningOfTime);
	assert_eq!(start("/2015"), Start::BeginningOfTime);
	assert!(Start::BeginningOfTime < start("Y-170000002"));
}

#[test]
fn only_intervals_are_states() {
	assert!(edtf("1990/2015").is_interval());
	assert!(edtf("2019-03/..").is_interval());
	assert!(!edtf("2019-03").is_interval());
	assert!(!edtf("2026-10-14T19:05:00+02").is_interval());
}

#[test]
fn a_moment_is_written_with_the_offset_from_utc() {
	let on = day("2026-10-14");
	let seconds = 19 * 3600 + 5 * 60;
	let written = |offset: i32| Edtf::moment(on, seconds, offset).map(|moment| moment.to_string());
	assert_eq!(written(120).as_deref(), Some("2026-10-14T19:05:00+02"));
	assert_eq!(written(0).as_deref(), Some("2026-10-14T19:05:00Z"));
	assert_eq!(written(-300).as_deref(), Some("2026-10-14T19:05:00-05"));
	assert_eq!(written(330).as_deref(), Some("2026-10-14T19:05:00+05:30"));
	assert_eq!(written(-90).as_deref(), Some("2026-10-14T19:05:00-01:30"));
	// Offsets run from -12 to +14 hours, and a day has 86,400 seconds.
	assert_eq!(written(15 * 60), None);
	assert_eq!(written(-13 * 60), None);
	assert_eq!(Edtf::moment(on, 86_400, 0), None);
}

#[test]
fn a_day_alone_is_a_date() {
	assert_eq!(Edtf::day(day("2017-05-01")), edtf("2017-05-01"));
}

#[test]
fn the_local_day_of_a_moment_depends_on_the_offset() {
	// 2026-10-14T23:30:00Z is already the 15th two hours east of UTC.
	let millis = 1_792_020_600_000;
	assert_eq!(Day::from_unix_millis(millis, 0), Some(day("2026-10-14")));
	assert_eq!(Day::from_unix_millis(millis, 120), Some(day("2026-10-15")));
	assert_eq!(Day::from_unix_millis(millis, -300), Some(day("2026-10-14")));
	assert_eq!(Day::second_of_day(millis, 0), 23 * 3600 + 30 * 60);
	assert_eq!(Day::second_of_day(millis, 120), 3600 + 30 * 60);
}
