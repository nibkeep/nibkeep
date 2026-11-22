//! Days of the proleptic Gregorian calendar.

use nibkeep_time::Day;
use proptest::prelude::*;

#[test]
fn day_zero_is_the_unix_epoch() {
	assert_eq!(Day::from_ymd(1970, 1, 1), Day::from_days_since_epoch(0));
	assert_eq!(
		Day::from_ymd(2000, 3, 1).map(Day::days_since_epoch),
		Some(11_017)
	);
	assert_eq!(
		Day::from_ymd(1969, 12, 31).map(Day::days_since_epoch),
		Some(-1)
	);
}

#[test]
fn invalid_dates_are_not_days() {
	assert_eq!(Day::from_ymd(1985, 2, 29), None);
	assert_eq!(Day::from_ymd(1985, 4, 31), None);
	assert_eq!(Day::from_ymd(1985, 13, 1), None);
	assert_eq!(Day::from_ymd(1985, 0, 1), None);
	assert_eq!(Day::from_ymd(1985, 1, 0), None);
	assert!(Day::from_ymd(2000, 2, 29).is_some());
	assert!(Day::from_ymd(-4, 2, 29).is_some());
	assert!(Day::from_ymd(1900, 2, 29).is_none());
}

#[test]
fn text_form() {
	let day = Day::from_ymd(1985, 4, 12).expect("valid");
	assert_eq!(day.to_string(), "1985-04-12");
	assert_eq!("1985-04-12".parse(), Ok(day));
	assert_eq!(
		Day::from_ymd(-44, 3, 15).expect("valid").to_string(),
		"-0044-03-15"
	);
	assert_eq!(
		Day::from_ymd(12_345, 1, 1).expect("valid").to_string(),
		"Y12345-01-01"
	);
	assert!("1985-4-12".parse::<Day>().is_err());
}

#[test]
fn years_have_at_most_15_digits() {
	let last = Day::from_ymd(999_999_999_999_999, 12, 31).expect("the last day");
	assert_eq!(last.to_string(), "Y999999999999999-12-31");
	assert_eq!("Y999999999999999-12-31".parse(), Ok(last));
	assert_eq!(
		Day::from_days_since_epoch(last.days_since_epoch() + 1),
		None
	);
	let first = Day::from_ymd(-999_999_999_999_999, 1, 1).expect("the first day");
	assert_eq!(
		Day::from_days_since_epoch(first.days_since_epoch() - 1),
		None
	);
	assert_eq!(Day::from_days_since_epoch(i64::MAX), None);
	assert_eq!(Day::from_days_since_epoch(i64::MIN), None);
	assert_eq!(Day::from_ymd(1_000_000_000_000_000, 1, 1), None);
}

#[test]
fn non_ascii_text_is_rejected() {
	assert!("1985-0\u{e9}-12".parse::<Day>().is_err());
	assert!("\u{e9}985-04-12".parse::<Day>().is_err());
}

proptest! {
	#[test]
	fn days_and_dates_convert_both_ways(days in -400_000_000_000i64..400_000_000_000) {
		let day = Day::from_days_since_epoch(days).expect("in range");
		let (year, month, day_of_month) = day.ymd();
		prop_assert_eq!(Day::from_ymd(year, month, day_of_month), Some(day));
		prop_assert_eq!(day.to_string().parse::<Day>(), Ok(day));
	}

	#[test]
	fn consecutive_days_are_consecutive_dates(days in -1_000_000i64..1_000_000) {
		let (year, month, day_of_month) = Day::from_days_since_epoch(days).expect("in range").ymd();
		let (next_year, next_month, next_day) =
			Day::from_days_since_epoch(days + 1).expect("in range").ymd();
		let same_month = (next_year, next_month, next_day) == (year, month, day_of_month + 1);
		let next_month_starts = next_day == 1
			&& ((next_year, next_month) == (year, month + 1) || (next_year, next_month, month) == (year + 1, 1, 12));
		prop_assert!(same_month || next_month_starts);
	}
}
