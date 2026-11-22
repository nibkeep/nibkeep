//! Whether a fact holds, or an event has happened, on a given day.

use nibkeep_time::{Day, Edtf, Presence, happened_by, holds_on};

fn day(text: &str) -> Day {
	text.parse().expect("valid day")
}

fn edtf(text: &str) -> Edtf {
	text.parse().expect("valid EDTF")
}

fn holds(valid_time: &str, on: &str) -> Presence {
	holds_on(Some(&edtf(valid_time)), day(on))
}

fn happened(date: &str, by: &str) -> Presence {
	happened_by(Some(&edtf(date)), day(by))
}

#[test]
fn an_event_in_the_1960s_is_uncertain_in_1965() {
	assert_eq!(happened("196X", "1959-12-31"), Presence::No);
	assert_eq!(happened("196X", "1960-01-01"), Presence::Uncertain);
	assert_eq!(happened("196X", "1965-06-15"), Presence::Uncertain);
	assert_eq!(happened("196X", "1969-12-31"), Presence::Yes);
	assert_eq!(happened("196X", "2000-01-01"), Presence::Yes);
}

#[test]
fn an_undated_fact_holds_at_every_point_in_time() {
	for on in ["Y-50000-01-01", "1066-10-14", "2026-10-11", "9999-12-31"] {
		assert_eq!(holds_on(None, day(on)), Presence::DateUnknown);
		assert_eq!(happened_by(None, day(on)), Presence::DateUnknown);
	}
}

#[test]
fn an_exact_date_is_certain() {
	assert_eq!(happened("1985-04-12", "1985-04-11"), Presence::No);
	assert_eq!(happened("1985-04-12", "1985-04-12"), Presence::Yes);
	assert_eq!(holds("1985-04-12", "1985-04-12"), Presence::Yes);
	assert_eq!(holds("1985-04-12", "1985-04-13"), Presence::No);
}

#[test]
fn a_plain_date_holds_throughout_its_range() {
	assert_eq!(holds("1985", "1985-01-01"), Presence::Yes);
	assert_eq!(holds("1985", "1985-12-31"), Presence::Yes);
	assert_eq!(holds("1985", "1986-01-01"), Presence::No);
	assert_eq!(holds("1985-02", "1985-02-28"), Presence::Yes);
	assert_eq!(holds("1984-02", "1984-02-29"), Presence::Yes);
	assert_eq!(holds("1985-02", "1985-03-01"), Presence::No);
}

#[test]
fn seasons_cover_three_months() {
	assert_eq!(holds("1975-22", "1975-05-31"), Presence::No);
	assert_eq!(holds("1975-22", "1975-06-01"), Presence::Yes);
	assert_eq!(holds("1975-22", "1975-08-31"), Presence::Yes);
	assert_eq!(holds("1975-22", "1975-09-01"), Presence::No);
	// Winter reaches into the next year.
	assert_eq!(holds("1975-24", "1976-02-29"), Presence::Yes);
	assert_eq!(holds("1975-24", "1976-03-01"), Presence::No);
}

#[test]
fn a_qualified_date_is_uncertain_over_a_wider_range() {
	// "About 1975" covers 1974 to 1976.
	assert_eq!(holds("1975~", "1973-12-31"), Presence::No);
	assert_eq!(holds("1975~", "1974-01-01"), Presence::Uncertain);
	assert_eq!(holds("1975~", "1975-06-01"), Presence::Uncertain);
	assert_eq!(holds("1975~", "1976-12-31"), Presence::Uncertain);
	assert_eq!(holds("1975~", "1977-01-01"), Presence::No);
	assert_eq!(happened("1975~", "1976-12-31"), Presence::Yes);
	// "Summer 1975, probably" widens by a season on each side.
	assert_eq!(holds("1975-22?", "1975-03-01"), Presence::Uncertain);
	assert_eq!(holds("1975-22?", "1975-02-28"), Presence::No);
	assert_eq!(holds("1975-22?", "1975-11-30"), Presence::Uncertain);
	// A month widens by a month, a day by a day.
	assert_eq!(holds("2004-06%", "2004-05-01"), Presence::Uncertain);
	assert_eq!(holds("2004-06%", "2004-07-31"), Presence::Uncertain);
	assert_eq!(holds("2004-06%", "2004-08-01"), Presence::No);
	assert_eq!(holds("2004-06-11?", "2004-06-12"), Presence::Uncertain);
	assert_eq!(holds("2004-06-11?", "2004-06-13"), Presence::No);
}

#[test]
fn unspecified_digits_are_uncertain() {
	assert_eq!(holds("1985-XX", "1985-07-01"), Presence::Uncertain);
	assert_eq!(holds("1985-04-XX", "1985-04-30"), Presence::Uncertain);
	assert_eq!(holds("1985-04-XX", "1985-05-01"), Presence::No);
}

#[test]
fn an_interval_is_certain_between_its_ends() {
	// From some time in 2001 to some time in 2023.
	assert_eq!(holds("2001/2023", "2000-12-31"), Presence::No);
	assert_eq!(holds("2001/2023", "2001-06-01"), Presence::Uncertain);
	assert_eq!(holds("2001/2023", "2001-12-31"), Presence::Yes);
	assert_eq!(holds("2001/2023", "2023-01-01"), Presence::Yes);
	assert_eq!(holds("2001/2023", "2023-06-01"), Presence::Uncertain);
	assert_eq!(holds("2001/2023", "2024-01-01"), Presence::No);
}

#[test]
fn open_and_unknown_interval_ends() {
	// Still true.
	assert_eq!(holds("2001-05-01/..", "2001-04-30"), Presence::No);
	assert_eq!(holds("2001-05-01/..", "9999-01-01"), Presence::Yes);
	// True since the beginning of time.
	assert_eq!(holds("../2001-05-01", "-9999-01-01"), Presence::Yes);
	assert_eq!(holds("../2001-05-01", "2001-05-02"), Presence::No);
	// Ended at an unknown time.
	assert_eq!(holds("2001-05-01/", "2001-04-30"), Presence::No);
	assert_eq!(holds("2001-05-01/", "2001-05-01"), Presence::Uncertain);
	assert_eq!(holds("2001-05-01/", "3000-01-01"), Presence::Uncertain);
	// Started at an unknown time.
	assert_eq!(holds("/2001-05-01", "1000-01-01"), Presence::Uncertain);
	assert_eq!(holds("/2001-05-01", "2001-05-02"), Presence::No);
}

#[test]
fn an_interval_event_has_happened_once_it_started() {
	assert_eq!(happened("2004-06/2004-08", "2004-05-31"), Presence::No);
	assert_eq!(
		happened("2004-06/2004-08", "2004-06-15"),
		Presence::Uncertain
	);
	assert_eq!(happened("2004-06/2004-08", "2004-06-30"), Presence::Yes);
	assert_eq!(happened("../2004", "1000-01-01"), Presence::Yes);
	assert_eq!(happened("/2004", "2003-06-01"), Presence::Uncertain);
	assert_eq!(happened("/2004", "2004-12-31"), Presence::Yes);
}

#[test]
fn a_date_with_a_time_means_the_date_as_written() {
	assert_eq!(
		happened("1985-04-12T23:20:30-04", "1985-04-11"),
		Presence::No
	);
	assert_eq!(
		happened("1985-04-12T23:20:30-04", "1985-04-12"),
		Presence::Yes
	);
	assert_eq!(holds("1985-04-12T23:20:30Z", "1985-04-12"), Presence::Yes);
	assert_eq!(holds("1985-04-12T23:20:30Z", "1985-04-13"), Presence::No);
}

#[test]
fn very_distant_years() {
	assert_eq!(happened("Y-170000002", "-9999-01-01"), Presence::Yes);
	assert_eq!(happened("Y170000002", "9999-12-31"), Presence::No);
	assert_eq!(holds("Y170000002", "Y170000002-06-01"), Presence::Yes);
}
