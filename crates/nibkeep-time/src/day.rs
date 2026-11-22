//! Days of the proleptic Gregorian calendar.

use std::fmt;
use std::str::FromStr;

/// The largest year NibKeep handles. Years have at most 15 digits, which keeps every
/// computation on days far from overflowing.
pub(crate) const MAX_YEAR: i64 = 999_999_999_999_999;

/// A day of the proleptic Gregorian calendar, which extends the Gregorian calendar to
/// years before its introduction. Year 0 is 1 BC.
///
/// The text form is the year, month and day, such as `1985-04-12`. Years have four
/// digits, a `-` before negative years, and a `Y` before years with more than four
/// digits, as in EDTF: `-0044-03-15`, `Y12345-01-01`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i64);

impl Day {
	/// Returns the day, or `None` if there is no such date or the year has more than 15
	/// digits.
	#[must_use]
	pub fn from_ymd(year: i64, month: u8, day: u8) -> Option<Self> {
		let valid = (-MAX_YEAR..=MAX_YEAR).contains(&year)
			&& (1..=12).contains(&month)
			&& (1..=days_in_month(year, month)).contains(&day);
		valid.then(|| Self::civil(year, month, day))
	}

	/// Returns the day `days` days after 1970-01-01, or before it if `days` is negative,
	/// or `None` if its year would have more than 15 digits.
	#[must_use]
	pub fn from_days_since_epoch(days: i64) -> Option<Self> {
		let first = days_from_civil(-MAX_YEAR, 1, 1);
		let last = days_from_civil(MAX_YEAR, 12, 31);
		(first..=last).contains(&days).then_some(Self(days))
	}

	/// The local day of a moment, given in milliseconds since 1970 (UTC), at an offset from
	/// UTC in minutes. Returns `None` if its year would have more than 15 digits.
	#[must_use]
	pub fn from_unix_millis(millis: u64, offset_minutes: i32) -> Option<Self> {
		Self::from_days_since_epoch(local_seconds(millis, offset_minutes).div_euclid(86_400))
	}

	/// The second of the local day of a moment, given in milliseconds since 1970 (UTC), at
	/// an offset from UTC in minutes: from 0 to 86,399.
	///
	/// # Panics
	///
	/// Never: the remainder of a division by 86,400 always fits.
	#[must_use]
	pub fn second_of_day(millis: u64, offset_minutes: i32) -> u32 {
		u32::try_from(local_seconds(millis, offset_minutes).rem_euclid(86_400))
			.expect("a second of a day is below 86,400")
	}

	/// The number of days since 1970-01-01, negative for earlier days.
	#[must_use]
	pub const fn days_since_epoch(self) -> i64 {
		self.0
	}

	/// The year, month and day.
	#[must_use]
	pub fn ymd(self) -> (i64, u8, u8) {
		civil_from_days(self.0)
	}

	/// The day of a valid date, with any year.
	pub(crate) fn civil(year: i64, month: u8, day: u8) -> Self {
		Self(days_from_civil(year, month, day))
	}

	/// The first day of a month.
	pub(crate) fn first_of_month(year: i64, month: u8) -> Self {
		Self::civil(year, month, 1)
	}

	/// The last day of a month.
	pub(crate) fn last_of_month(year: i64, month: u8) -> Self {
		Self::civil(year, month, days_in_month(year, month))
	}

	/// The day `days` days later, or earlier if `days` is negative. Internal computations
	/// may step a little outside the range of [`Day::from_days_since_epoch`], which is far
	/// from overflowing.
	pub(crate) fn add_days(self, days: i64) -> Self {
		Self(self.0 + days)
	}
}

impl fmt::Display for Day {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let (year, month, day) = self.ymd();
		write_year(f, year)?;
		write!(f, "-{month:02}-{day:02}")
	}
}

impl FromStr for Day {
	type Err = ParseDayError;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		if !text.is_ascii() {
			return Err(ParseDayError);
		}
		let (year, rest) = parse_year(text).ok_or(ParseDayError)?;
		let (month, day) = rest
			.strip_prefix('-')
			.and_then(|rest| rest.split_once('-'))
			.ok_or(ParseDayError)?;
		Day::from_ymd(
			year,
			two_digits(month).ok_or(ParseDayError)?,
			two_digits(day).ok_or(ParseDayError)?,
		)
		.ok_or(ParseDayError)
	}
}

/// The error returned for text that is not a day in its text form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a day in the form 1985-04-12")]
pub struct ParseDayError;

/// Whether a year of the proleptic Gregorian calendar is a leap year.
pub(crate) fn is_leap_year(year: i64) -> bool {
	year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// The number of days in a month, or 0 if `month` is not a month.
pub(crate) fn days_in_month(year: i64, month: u8) -> u8 {
	match month {
		1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
		4 | 6 | 9 | 11 => 30,
		2 if is_leap_year(year) => 29,
		2 => 28,
		_ => 0,
	}
}

/// Writes a year as EDTF does: four digits, a `-` before negative years, and a `Y` before
/// years with more than four digits.
pub(crate) fn write_year(f: &mut fmt::Formatter<'_>, year: i64) -> fmt::Result {
	if year.unsigned_abs() > 9999 {
		write!(f, "Y{year}")
	} else if year < 0 {
		write!(f, "-{:04}", year.unsigned_abs())
	} else {
		write!(f, "{year:04}")
	}
}

/// Reads a year in the form [`write_year`] writes, and returns it with the rest of the
/// text.
pub(crate) fn parse_year(text: &str) -> Option<(i64, &str)> {
	if let Some(rest) = text.strip_prefix('Y') {
		let (negative, rest) = rest
			.strip_prefix('-')
			.map_or((false, rest), |rest| (true, rest));
		let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
		if !(5..=15).contains(&digits) || rest.starts_with('0') {
			return None;
		}
		let value: i64 = rest[..digits].parse().ok()?;
		Some((if negative { -value } else { value }, &rest[digits..]))
	} else {
		let (negative, rest) = text
			.strip_prefix('-')
			.map_or((false, text), |rest| (true, rest));
		let digits = rest
			.get(..4)
			.filter(|digits| digits.bytes().all(|b| b.is_ascii_digit()))?;
		let value: i64 = digits.parse().ok()?;
		if negative && value == 0 {
			return None;
		}
		Some((if negative { -value } else { value }, &rest[4..]))
	}
}

/// Reads exactly two digits.
pub(crate) fn two_digits(text: &str) -> Option<u8> {
	if text.len() == 2 && text.bytes().all(|b| b.is_ascii_digit()) {
		text.parse().ok()
	} else {
		None
	}
}

/// The number of days from 1970-01-01 to a date, after Howard Hinnant's algorithm, which
/// counts years from March so that the leap day comes last.
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
	let year = if month <= 2 { year - 1 } else { year };
	let era = year.div_euclid(400);
	let year_of_era = year.rem_euclid(400);
	let month_from_march = (i64::from(month) + 9) % 12;
	let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
	let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
	era * 146_097 + day_of_era - 719_468
}

/// The date of the day `days` days after 1970-01-01: the inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u8, u8) {
	let days = days + 719_468;
	let era = days.div_euclid(146_097);
	let day_of_era = days.rem_euclid(146_097);
	let year_of_era =
		(day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_from_march = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
	let month = if month_from_march < 10 {
		month_from_march + 3
	} else {
		month_from_march - 9
	};
	let year = year_of_era + era * 400 + i64::from(month <= 2);
	(
		year,
		u8::try_from(month).expect("a month is between 1 and 12"),
		u8::try_from(day).expect("a day is between 1 and 31"),
	)
}

/// Seconds since 1970 in local time: a moment in milliseconds since 1970 (UTC), shifted by
/// an offset from UTC in minutes.
fn local_seconds(millis: u64, offset_minutes: i32) -> i64 {
	i64::try_from(millis / 1000).unwrap_or(i64::MAX) + i64::from(offset_minutes) * 60
}
