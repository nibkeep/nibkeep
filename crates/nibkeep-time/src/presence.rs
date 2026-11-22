//! The questions of time travel: does a state hold on a day, has an event happened by it?

use crate::day::Day;
use crate::edtf::{Edtf, Endpoint, Kind};

/// The answer to a question of time travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
	/// Certainly.
	Yes,
	/// Possibly: the dates are too fuzzy to tell.
	Uncertain,
	/// Certainly not.
	No,
	/// There is no date, so the fact is shown at every point in time, marked "date
	/// unknown".
	DateUnknown,
}

/// A point on the time line, including the beginning and end of time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Bound {
	BeginningOfTime,
	Day(Day),
	EndOfTime,
}

/// Whether a state that is true during `valid_time` holds on `day`.
///
/// A single date counts as the whole of its range. For an interval, the state starts
/// somewhere within the start date's range and ends somewhere within the end date's
/// range. `docs/spec/time.md` describes the rules.
#[must_use]
pub fn holds_on(valid_time: Option<&Edtf>, day: Day) -> Presence {
	let Some(valid_time) = valid_time else {
		return Presence::DateUnknown;
	};
	match &valid_time.0 {
		Kind::Date(date) => {
			let (first, last) = date.range();
			if day < first || day > last {
				Presence::No
			} else if date.is_definite() {
				Presence::Yes
			} else {
				Presence::Uncertain
			}
		}
		Kind::DateTime(date_time) => {
			if day == date_time.day() {
				Presence::Yes
			} else {
				Presence::No
			}
		}
		Kind::Interval(start, end) => {
			let (earliest_start, latest_start) = endpoint_bounds(start, Bound::BeginningOfTime);
			let (earliest_end, latest_end) = endpoint_bounds(end, Bound::EndOfTime);
			let day = Bound::Day(day);
			if latest_start <= day && day <= earliest_end {
				Presence::Yes
			} else if earliest_start <= day && day <= latest_end {
				Presence::Uncertain
			} else {
				Presence::No
			}
		}
	}
}

/// Whether an event dated `date` has happened by the end of `day`. An event dated with an
/// interval has happened once it has started.
#[must_use]
pub fn happened_by(date: Option<&Edtf>, day: Day) -> Presence {
	let Some(date) = date else {
		return Presence::DateUnknown;
	};
	let (earliest, latest) = match &date.0 {
		Kind::Date(date) => bounds(date.range()),
		Kind::DateTime(date_time) => bounds((date_time.day(), date_time.day())),
		Kind::Interval(start, end) => match start {
			// An event with an unknown start began at some time before its end.
			Endpoint::Unknown => (
				Bound::BeginningOfTime,
				endpoint_bounds(end, Bound::EndOfTime).1,
			),
			_ => endpoint_bounds(start, Bound::BeginningOfTime),
		},
	};
	let day = Bound::Day(day);
	if latest <= day {
		Presence::Yes
	} else if earliest <= day {
		Presence::Uncertain
	} else {
		Presence::No
	}
}

fn bounds((first, last): (Day, Day)) -> (Bound, Bound) {
	(Bound::Day(first), Bound::Day(last))
}

/// The earliest and latest point an interval's end could be at. `open` is where an open
/// end lies: the beginning of time for a start, the end of time for an end. An unknown
/// end could lie anywhere.
fn endpoint_bounds(endpoint: &Endpoint, open: Bound) -> (Bound, Bound) {
	match endpoint {
		Endpoint::Date(date) => bounds(date.range()),
		Endpoint::Open => (open, open),
		Endpoint::Unknown => (Bound::BeginningOfTime, Bound::EndOfTime),
	}
}
