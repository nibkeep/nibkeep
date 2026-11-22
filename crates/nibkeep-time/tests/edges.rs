//! The days that each end of a date could be on, for drawing it on a time line.

use nibkeep_time::{Day, Edge, Edtf};

fn day(text: &str) -> Day {
	text.parse().expect("valid day")
}

fn edtf(text: &str) -> Edtf {
	text.parse().expect("valid EDTF")
}

fn within(earliest: &str, latest: &str) -> Edge {
	Edge::Within {
		earliest: day(earliest),
		latest: day(latest),
	}
}

#[test]
fn a_date_starts_and_ends_anywhere_in_its_range() {
	let march = within("2011-03-01", "2011-03-31");
	assert_eq!(edtf("2011-03").edges(), (march, march));
	assert_eq!(
		edtf("2004~").edges().0,
		within("2003-01-01", "2005-12-31"),
		"a qualifier widens it"
	);
	assert_eq!(edtf("198X").edges().1, within("1980-01-01", "1989-12-31"));
	let day_of = within("2017-08-11", "2017-08-11");
	assert_eq!(edtf("2017-08-11T18:03:38").edges(), (day_of, day_of));
}

#[test]
fn an_interval_has_the_ends_it_was_written_with() {
	assert_eq!(
		edtf("2008/2015~").edges(),
		(
			within("2008-01-01", "2008-12-31"),
			within("2014-01-01", "2016-12-31")
		)
	);
	assert_eq!(edtf("1985/..").edges().1, Edge::Open);
	assert_eq!(edtf("1985/").edges().1, Edge::Unknown);
	assert_eq!(edtf("../1985").edges().0, Edge::Open);
	assert_eq!(edtf("/1985").edges().0, Edge::Unknown);
}

#[test]
fn an_edge_is_exact_when_it_is_one_day() {
	assert!(edtf("2009-04-14").edges().0.is_one_day());
	assert!(edtf("2017-08-11T18:03:38").edges().0.is_one_day());
	for vague in ["2011-03", "2013", "2009-04-14~", "2009-04-XX"] {
		assert!(!edtf(vague).edges().0.is_one_day(), "{vague}");
	}
	assert!(!Edge::Open.is_one_day());
	assert!(!Edge::Unknown.is_one_day());
}
