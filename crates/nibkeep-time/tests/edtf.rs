//! EDTF values parse and print back unchanged, and invalid ones are rejected.

use nibkeep_base::cbor;
use nibkeep_time::Edtf;

/// The examples for levels 0 and 1 in the EDTF specification, plus a few more.
const VALID: &[&str] = &[
	// Level 0: dates.
	"1985-04-12",
	"1985-04",
	"1985",
	// Level 0: dates and times.
	"1985-04-12T23:20:30",
	"1985-04-12T23:20:30Z",
	"1985-04-12T23:20:30-04",
	"1985-04-12T23:20:30+04:30",
	// Level 0: intervals.
	"1964/2008",
	"2004-06/2006-08",
	"2004-02-01/2005-02-08",
	"2004-02-01/2005-02",
	"2004-02-01/2005",
	"2005/2006-02",
	// Level 1: years with more than four digits.
	"Y170000002",
	"Y-170000002",
	// Level 1: seasons.
	"2001-21",
	"2001-22",
	"2001-23",
	"2001-24",
	// Level 1: qualifiers.
	"1984?",
	"2004-06~",
	"2004-06-11%",
	// Level 1: unspecified digits.
	"201X",
	"20XX",
	"2004-XX",
	"1985-04-XX",
	"1985-XX-XX",
	// Level 1: open and unknown interval ends.
	"1985-04-12/..",
	"1985-04/..",
	"1985/..",
	"../1985-04-12",
	"../1985-04",
	"../1985",
	"1985-04-12/",
	"1985-04/",
	"1985/",
	"/1985-04-12",
	"/1985-04",
	"/1985",
	// Level 1: negative years.
	"-1985",
	// Qualified interval ends.
	"1984~/2004-06",
	"1984?/2004%",
	// Time zones with whole hours, with minutes, and at the limits.
	"1985-04-12T23:20:30+14",
	"1985-04-12T23:20:30-12",
	"1985-04-12T23:20:30+05:45",
	"1985-04-12T23:20:30-09:30",
	// Leap days.
	"2000-02-29",
	"2024-02-29",
	"0000",
];

const INVALID: &[&str] = &[
	"",
	" 1985",
	"1985 ",
	"85",
	"19850",
	"1985-4",
	"1985-04-1",
	"1985-13",
	"1985-00",
	"1985-04-31",
	"1985-02-29",
	"1900-02-29",
	"1985-04-00",
	"Y1985",
	"Y-1985",
	"-0000",
	"Y1234567890123456",
	"2001-25",
	"2001-21-01",
	"201X-05",
	"2XXX",
	"-201X",
	"1985-XX-12",
	"1984??",
	"1984?~",
	"1985-04-12T24:00:00",
	"1985-04-12T23:60:00",
	"1985-04-12T23:20:60",
	"1985-04-12T23:20",
	"1985-04-12T23:20:30+0430",
	"1985-04-12T23:20:30+15:00",
	"1985-04-12T23:20:30~",
	"1985-04T23:20:30",
	"1985/1985-04-12T23:20:30",
	"../..",
	"../",
	"/..",
	"/",
	"..",
	"2005/2004",
	"2005/2004-12",
	"1985//1986",
	// The start of an interval is checked as written, not as widened by a qualifier.
	"1985~/1984",
	"1990?/1989",
	// Every time zone has one form, and real offsets range from -12:00 to +14:00.
	"1985-04-12T23:20:30+00",
	"1985-04-12T23:20:30-00",
	"1985-04-12T23:20:30+00:00",
	"1985-04-12T23:20:30+04:00",
	"1985-04-12T23:20:30+14:30",
	"1985-04-12T23:20:30-12:30",
	"1985-04-12T23:20:30-13",
	"1985-04-12T23:20:30+04:60",
	// Only ASCII.
	"1985-04-12T1\u{e9}:00:00",
	"1985-04-\u{e9}",
	"\u{e9}",
	// Interval ends are dates without times.
	"1985-04-12T23:20:30Z/1986",
	"1985/1986-04-12T23:20:30",
	// Only four-digit years without a sign have unspecified digits.
	"-198X",
	"-19X",
	"Y1XX",
	"Y-1XXXX",
];

#[test]
fn valid_values_print_back_unchanged() {
	for &text in VALID {
		let edtf: Edtf = text
			.parse()
			.unwrap_or_else(|error| panic!("{text} was rejected: {error}"));
		assert_eq!(edtf.to_string(), text);
	}
}

#[test]
fn invalid_values_are_rejected() {
	for &text in INVALID {
		assert!(text.parse::<Edtf>().is_err(), "{text:?} was accepted");
	}
}

#[test]
fn values_are_encoded_as_text() {
	let edtf: Edtf = "1975-22?".parse().expect("valid");
	let encoded = cbor::to_vec(&edtf);
	assert_eq!(cbor::from_slice::<Edtf>(&encoded), Ok(edtf));
	assert_eq!(encoded, cbor::to_vec(&"1975-22?".to_owned()));
	assert!(cbor::from_slice::<Edtf>(&cbor::to_vec(&"1975-13".to_owned())).is_err());
}

#[test]
fn errors_explain_the_problem() {
	let reason = |text: &str| text.parse::<Edtf>().expect_err("invalid").to_string();
	assert!(reason("1985-04-12T23:20:30Z/1986").contains("dates without times"));
	assert!(reason("-198X").contains("without a sign"));
	assert!(reason("1985-04-12T23:20:30+00").contains("UTC is written Z"));
	assert!(reason("1985-04-12T23:20:30+04:00").contains("whole hours"));
}

#[test]
fn a_date_and_time_is_written_again_from_its_local_seconds_in_the_form_devices_write() {
	for text in [
		"2019-12-24T18:30:00+01",
		"2019-12-24T18:30:00",
		"2019-12-24T17:30:00Z",
		"1969-07-20T20:17:40-05:45",
	] {
		let date: Edtf = text.parse().expect("a date and time");
		let (local, offset) = date.local_seconds().expect("a date and time");
		assert_eq!(
			Edtf::from_local_seconds(local, offset),
			Some(date),
			"{text}"
		);
	}
	assert_eq!(Edtf::from_local_seconds(0, Some(15 * 60)), None);
}
