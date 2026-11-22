//! The Extended Date/Time Format, levels 0 and 1.

use std::fmt;
use std::str::FromStr;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};

use crate::day::{Day, days_in_month, parse_year, two_digits, write_year};

/// A date, a date and time, or an interval, in the Extended Date/Time Format (EDTF,
/// ISO 8601-2), levels 0 and 1.
///
/// Values are read with [`str::parse`] and printed back exactly as they were read. In
/// CBOR, a value is its text. `docs/spec/time.md` lists what is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edtf(pub(crate) Kind);

/// Where a date starts, as written, before qualifiers widen it: what orders the values of
/// a field that changes over time (`docs/spec/model.md`, section 6).
///
/// The beginning of time comes first; then days in order, and within a day the date alone
/// before the times of that day, which are ordered by their time as written, whatever
/// their offset from UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Start {
	/// An interval whose start is open or unknown.
	BeginningOfTime,
	/// A day, and for a date and time, its second of the day as written.
	At {
		/// The first day the date names.
		day: Day,
		/// The second of the day, from 0 to 86,399, if the date has a time.
		second: Option<u32>,
	},
}

/// Where one end of a date could be: on a day within a range, or at an end of an interval
/// written open (`..`) or unknown (empty).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
	/// Open: the interval reaches back to the beginning of time, or on into the future.
	Open,
	/// Unknown: the end exists, but nobody knows when it was.
	Unknown,
	/// On one of the days from `earliest` to `latest`, both included.
	Within {
		/// The earliest day it could be on.
		earliest: Day,
		/// The latest day it could be on.
		latest: Day,
	},
}

impl Edge {
	/// Whether the edge is on one day, known to the day.
	#[must_use]
	pub fn is_one_day(self) -> bool {
		matches!(self, Self::Within { earliest, latest } if earliest == latest)
	}
}

impl Edtf {
	/// The days that the start and the end could be on. A date starts and ends anywhere in
	/// its range, widened by its qualifier (`docs/spec/time.md`, 1.2): `2011-03` anywhere in
	/// March. A date and time is on its day as written. An interval has the edges of its
	/// two ends.
	#[must_use]
	pub fn edges(&self) -> (Edge, Edge) {
		let within = |(earliest, latest): (Day, Day)| Edge::Within { earliest, latest };
		let endpoint = |endpoint: &Endpoint| match endpoint {
			Endpoint::Date(date) => within(date.range()),
			Endpoint::Open => Edge::Open,
			Endpoint::Unknown => Edge::Unknown,
		};
		match &self.0 {
			Kind::Date(date) => (within(date.range()), within(date.range())),
			Kind::DateTime(date_time) => {
				let day = within((date_time.day(), date_time.day()));
				(day, day)
			}
			Kind::Interval(start, end) => (endpoint(start), endpoint(end)),
		}
	}

	/// Where the date starts, as written: a date on the first day it names, before a
	/// qualifier widens it, so that `2010-06~` starts on 2010-06-01; a date and time at its
	/// time as written; an interval where its start does, and at the beginning of time if
	/// its start is open or unknown.
	#[must_use]
	pub fn start(&self) -> Start {
		match &self.0 {
			Kind::Date(date) => Start::At {
				day: date.span().0,
				second: None,
			},
			Kind::DateTime(date_time) => Start::At {
				day: date_time.date,
				second: Some(date_time.second_of_day()),
			},
			Kind::Interval(start, _) => match start.date() {
				Some(date) => Start::At {
					day: date.span().0,
					second: None,
				},
				None => Start::BeginningOfTime,
			},
		}
	}

	/// Whether the date is an interval: the period of a state, rather than when an event
	/// happened.
	#[must_use]
	pub fn is_interval(&self) -> bool {
		matches!(self.0, Kind::Interval(..))
	}

	/// The date of a day, such as `2017-05-01`.
	#[must_use]
	pub fn day(day: Day) -> Self {
		let (year, month, day) = day.ymd();
		Self(Kind::Date(Date {
			year: Year {
				value: year,
				unspecified: 0,
			},
			month: Month::Known(month),
			day: DayOfMonth::Known(day),
			qualifier: None,
		}))
	}

	/// A date and time's seconds since 1970-01-01T00:00:00 as written, whatever its time
	/// zone, and its offset from UTC in minutes, if it has one: `None` for a date alone or an
	/// interval.
	#[must_use]
	pub fn local_seconds(&self) -> Option<(i64, Option<i32>)> {
		let Kind::DateTime(date_time) = &self.0 else {
			return None;
		};
		let local =
			date_time.date.days_since_epoch() * 86_400 + i64::from(date_time.second_of_day());
		Some((local, date_time.offset.map(Offset::minutes)))
	}

	/// The date and time written as `local` seconds since 1970-01-01T00:00:00, whatever its
	/// time zone, with its offset from UTC in minutes if it has one, as [`Edtf::local_seconds`]
	/// gives them, in the one form that devices write: `2019-12-24T18:30:00+01`. Returns
	/// `None` for an offset outside the range from −12 to +14 hours, or a day out of range.
	#[must_use]
	pub fn from_local_seconds(local: i64, offset_minutes: Option<i32>) -> Option<Self> {
		let day = Day::from_days_since_epoch(local.div_euclid(86_400))?;
		let second = u32::try_from(local.rem_euclid(86_400)).ok()?;
		if let Some(offset) = offset_minutes {
			return Self::moment(day, second, offset);
		}
		let Kind::DateTime(date_time) = Self::moment(day, second, 0)?.0 else {
			return None;
		};
		Some(Self(Kind::DateTime(DateTime {
			offset: None,
			..date_time
		})))
	}

	/// The same date and time with an offset from UTC of `offset_minutes`, for one written
	/// without one; `None` for one that has one, or an offset outside the range.
	#[must_use]
	pub fn with_offset(&self, offset_minutes: i32) -> Option<Self> {
		let Kind::DateTime(date_time) = &self.0 else {
			return None;
		};
		if date_time.offset.is_some() {
			return None;
		}
		Some(Self(Kind::DateTime(DateTime {
			offset: Some(Offset::from_minutes(offset_minutes)?),
			..*date_time
		})))
	}

	/// The date and time of a moment, as a device writes it: its local day, the second of
	/// that day, and its offset from UTC in minutes, such as `2026-10-14T19:05:00+02`.
	/// Returns `None` if the second is not one of a day or the offset is outside the range
	/// from −12 to +14 hours.
	#[must_use]
	pub fn moment(day: Day, second_of_day: u32, offset_minutes: i32) -> Option<Self> {
		if second_of_day >= 86_400 {
			return None;
		}
		let offset = Offset::from_minutes(offset_minutes)?;
		let part = |value: u32| u8::try_from(value).ok();
		Some(Self(Kind::DateTime(DateTime {
			date: day,
			hour: part(second_of_day / 3600)?,
			minute: part(second_of_day / 60 % 60)?,
			second: part(second_of_day % 60)?,
			offset: Some(offset),
		})))
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
	Date(Date),
	DateTime(DateTime),
	Interval(Endpoint, Endpoint),
}

/// One end of an interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Endpoint {
	Date(Date),
	/// `..`: the interval reaches back to the beginning of time, or on into the future.
	Open,
	/// Empty: the end exists but nobody knows when it was.
	Unknown,
}

/// A date, possibly with unspecified digits, a season and a qualifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Date {
	year: Year,
	month: Month,
	day: DayOfMonth,
	qualifier: Option<Qualifier>,
}

/// A year. With unspecified digits, `value` holds the specified leading digits: `201X` is
/// 201 with one unspecified digit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Year {
	value: i64,
	unspecified: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Month {
	None,
	Known(u8),
	Season(Season),
	Unspecified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DayOfMonth {
	None,
	Known(u8),
	Unspecified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Season {
	Spring,
	Summer,
	Autumn,
	Winter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Qualifier {
	/// `?`
	Uncertain,
	/// `~`
	Approximate,
	/// `%`
	Both,
}

/// A date with a time of day, to the second, and an optional time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DateTime {
	date: Day,
	hour: u8,
	minute: u8,
	second: u8,
	offset: Option<Offset>,
}

/// A time zone: `Z`, or an offset from UTC written as `+04` or `+04:30`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Offset {
	Utc,
	Hours {
		negative: bool,
		hours: u8,
	},
	HoursMinutes {
		negative: bool,
		hours: u8,
		minutes: u8,
	},
}

/// The unit by which a qualified date widens its range on each side.
#[derive(Clone, Copy, Debug)]
enum Unit {
	Years(i64),
	Months(i64),
	Days(i64),
}

impl Date {
	/// The first and last day the date could mean, widened by one unit of its precision on
	/// each side if it is qualified.
	pub(crate) fn range(&self) -> (Day, Day) {
		let (first, last) = self.span();
		if self.qualifier.is_some() {
			widen(first, last, self.unit())
		} else {
			(first, last)
		}
	}

	/// Whether the date means exactly its range: no qualifier and no unspecified digits.
	pub(crate) fn is_definite(&self) -> bool {
		self.qualifier.is_none()
			&& self.year.unspecified == 0
			&& self.month != Month::Unspecified
			&& self.day != DayOfMonth::Unspecified
	}

	/// The first and last day the date covers, before any widening.
	pub(crate) fn span(&self) -> (Day, Day) {
		let year = self.year.value;
		match (self.month, self.day) {
			(Month::None, _) => {
				let size = 10_i64.pow(u32::from(self.year.unspecified));
				let first = year * size;
				(
					Day::first_of_month(first, 1),
					Day::last_of_month(first + size - 1, 12),
				)
			}
			(Month::Unspecified, _) => (Day::first_of_month(year, 1), Day::last_of_month(year, 12)),
			(Month::Season(season), _) => {
				let first_month = season.first_month();
				let (last_year, last_month) = add_months(year, first_month, 2);
				(
					Day::first_of_month(year, first_month),
					Day::last_of_month(last_year, last_month),
				)
			}
			(Month::Known(month), DayOfMonth::Known(day)) => {
				let day = Day::civil(year, month, day);
				(day, day)
			}
			(Month::Known(month), _) => (
				Day::first_of_month(year, month),
				Day::last_of_month(year, month),
			),
		}
	}

	/// The unit of the date's precision.
	fn unit(&self) -> Unit {
		match (self.month, self.day) {
			(Month::None, _) => Unit::Years(10_i64.pow(u32::from(self.year.unspecified))),
			(Month::Unspecified, _) => Unit::Years(1),
			(Month::Season(_), _) => Unit::Months(3),
			(Month::Known(_), DayOfMonth::Known(_)) => Unit::Days(1),
			(Month::Known(_), _) => Unit::Months(1),
		}
	}
}

impl Season {
	/// The first of the season's three months. Winter's last month is in the following
	/// year.
	fn first_month(self) -> u8 {
		match self {
			Self::Spring => 3,
			Self::Summer => 6,
			Self::Autumn => 9,
			Self::Winter => 12,
		}
	}

	fn code(self) -> u8 {
		match self {
			Self::Spring => 21,
			Self::Summer => 22,
			Self::Autumn => 23,
			Self::Winter => 24,
		}
	}

	fn from_code(code: u8) -> Option<Self> {
		match code {
			21 => Some(Self::Spring),
			22 => Some(Self::Summer),
			23 => Some(Self::Autumn),
			24 => Some(Self::Winter),
			_ => None,
		}
	}
}

impl Offset {
	/// The offset in minutes from UTC.
	fn minutes(self) -> i32 {
		match self {
			Self::Utc => 0,
			Self::Hours { negative, hours } => {
				let minutes = i32::from(hours) * 60;
				if negative { -minutes } else { minutes }
			}
			Self::HoursMinutes {
				negative,
				hours,
				minutes,
			} => {
				let minutes = i32::from(hours) * 60 + i32::from(minutes);
				if negative { -minutes } else { minutes }
			}
		}
	}

	/// The offset of `minutes` minutes from UTC, in its one written form: `Z` for UTC, whole
	/// hours alone, and otherwise hours and minutes. Returns `None` outside the range from
	/// −12 to +14 hours.
	fn from_minutes(minutes: i32) -> Option<Self> {
		if minutes == 0 {
			return Some(Self::Utc);
		}
		let negative = minutes < 0;
		let total = minutes.unsigned_abs();
		let limit = if negative { 12 * 60 } else { 14 * 60 };
		if total > limit {
			return None;
		}
		let hours = u8::try_from(total / 60).ok()?;
		let minutes = u8::try_from(total % 60).ok()?;
		Some(if minutes == 0 {
			Self::Hours { negative, hours }
		} else {
			Self::HoursMinutes {
				negative,
				hours,
				minutes,
			}
		})
	}
}

impl DateTime {
	/// The date, as written, whatever the time zone.
	pub(crate) fn day(&self) -> Day {
		self.date
	}

	/// The second of the day, as written, whatever the time zone.
	fn second_of_day(&self) -> u32 {
		u32::from(self.hour) * 3600 + u32::from(self.minute) * 60 + u32::from(self.second)
	}
}

impl Endpoint {
	/// The end's date, unless it is open or unknown.
	pub(crate) fn date(&self) -> Option<&Date> {
		match self {
			Self::Date(date) => Some(date),
			Self::Open | Self::Unknown => None,
		}
	}
}

/// Widens a range by `unit` on each side.
fn widen(first: Day, last: Day, unit: Unit) -> (Day, Day) {
	match unit {
		Unit::Days(days) => (first.add_days(-days), last.add_days(days)),
		Unit::Months(months) => {
			let (first_year, first_month, _) = first.ymd();
			let (last_year, last_month, _) = last.ymd();
			let (start_year, start_month) = add_months(first_year, first_month, -months);
			let (end_year, end_month) = add_months(last_year, last_month, months);
			(
				Day::first_of_month(start_year, start_month),
				Day::last_of_month(end_year, end_month),
			)
		}
		Unit::Years(years) => {
			let (first_year, _, _) = first.ymd();
			let (last_year, _, _) = last.ymd();
			(
				Day::first_of_month(first_year - years, 1),
				Day::last_of_month(last_year + years, 12),
			)
		}
	}
}

/// The month `months` months after `month` of `year`, or before it if `months` is
/// negative.
fn add_months(year: i64, month: u8, months: i64) -> (i64, u8) {
	let index = year * 12 + i64::from(month) - 1 + months;
	let month = u8::try_from(index.rem_euclid(12) + 1).expect("a month is between 1 and 12");
	(index.div_euclid(12), month)
}

impl fmt::Display for Edtf {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match &self.0 {
			Kind::Date(date) => date.fmt(f),
			Kind::DateTime(date_time) => date_time.fmt(f),
			Kind::Interval(start, end) => write!(f, "{start}/{end}"),
		}
	}
}

impl fmt::Display for Endpoint {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Date(date) => date.fmt(f),
			Self::Open => f.write_str(".."),
			Self::Unknown => Ok(()),
		}
	}
}

impl fmt::Display for Date {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.year.unspecified == 0 {
			write_year(f, self.year.value)?;
		} else {
			let digits = usize::from(4 - self.year.unspecified);
			write!(
				f,
				"{:0digits$}{}",
				self.year.value,
				"X".repeat(usize::from(self.year.unspecified))
			)?;
		}
		match self.month {
			Month::None => {}
			Month::Known(month) => write!(f, "-{month:02}")?,
			Month::Season(season) => write!(f, "-{}", season.code())?,
			Month::Unspecified => f.write_str("-XX")?,
		}
		match self.day {
			DayOfMonth::None => {}
			DayOfMonth::Known(day) => write!(f, "-{day:02}")?,
			DayOfMonth::Unspecified => f.write_str("-XX")?,
		}
		match self.qualifier {
			None => Ok(()),
			Some(Qualifier::Uncertain) => f.write_str("?"),
			Some(Qualifier::Approximate) => f.write_str("~"),
			Some(Qualifier::Both) => f.write_str("%"),
		}
	}
}

impl fmt::Display for DateTime {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{}T{:02}:{:02}:{:02}",
			self.date, self.hour, self.minute, self.second
		)?;
		match self.offset {
			None => Ok(()),
			Some(Offset::Utc) => f.write_str("Z"),
			Some(Offset::Hours { negative, hours }) => {
				write!(f, "{}{hours:02}", if negative { '-' } else { '+' })
			}
			Some(Offset::HoursMinutes {
				negative,
				hours,
				minutes,
			}) => write!(
				f,
				"{}{hours:02}:{minutes:02}",
				if negative { '-' } else { '+' }
			),
		}
	}
}

impl FromStr for Edtf {
	type Err = ParseEdtfError;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		// Every valid value is ASCII, and checking this first lets the parser below index
		// the text by bytes.
		if !text.is_ascii() {
			return Err(error("only ASCII characters are allowed"));
		}
		if let Some((start, end)) = text.split_once('/') {
			return parse_interval(start, end).map(Self);
		}
		if let Some((date, time)) = text.split_once('T') {
			return parse_date_time(date, time).map(|date_time| Self(Kind::DateTime(date_time)));
		}
		parse_date(text).map(|date| Self(Kind::Date(date)))
	}
}

impl ToCbor for Edtf {
	fn to_cbor(&self) -> Value {
		Value::Text(self.to_string())
	}
}

impl FromCbor for Edtf {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		match value {
			Value::Text(text) => text
				.parse()
				.map_err(|_| cbor::Error::Expected("an EDTF date")),
			_ => Err(cbor::Error::Expected("an EDTF date")),
		}
	}
}

/// Why text is not an EDTF value that NibKeep accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a valid date: {reason}")]
pub struct ParseEdtfError {
	reason: &'static str,
}

fn error(reason: &'static str) -> ParseEdtfError {
	ParseEdtfError { reason }
}

fn parse_interval(start: &str, end: &str) -> Result<Kind, ParseEdtfError> {
	let start = parse_endpoint(start)?;
	let end = parse_endpoint(end)?;
	match (start.date(), end.date()) {
		(None, None) => Err(error("an interval needs at least one date")),
		(Some(start_date), Some(end_date)) if start_date.span().0 > end_date.span().1 => {
			Err(error("an interval cannot end before it starts"))
		}
		_ => Ok(Kind::Interval(start, end)),
	}
}

fn parse_endpoint(text: &str) -> Result<Endpoint, ParseEdtfError> {
	match text {
		"" => Ok(Endpoint::Unknown),
		".." => Ok(Endpoint::Open),
		_ if text.contains('T') => Err(error("the ends of an interval are dates without times")),
		_ => parse_date(text).map(Endpoint::Date),
	}
}

fn parse_date(text: &str) -> Result<Date, ParseEdtfError> {
	let (text, qualifier) = match text.as_bytes().last() {
		Some(b'?') => (&text[..text.len() - 1], Some(Qualifier::Uncertain)),
		Some(b'~') => (&text[..text.len() - 1], Some(Qualifier::Approximate)),
		Some(b'%') => (&text[..text.len() - 1], Some(Qualifier::Both)),
		_ => (text, None),
	};
	let (year, rest) = parse_year_with_unspecified(text)?;
	let mut parts = rest.split('-');
	if parts.next() != Some("") {
		return Err(error("expected a dash after the year"));
	}
	let month = match parts.next() {
		None => Month::None,
		Some(_) if year.unspecified > 0 => {
			return Err(error("a year with unspecified digits stands alone"));
		}
		Some("XX") => Month::Unspecified,
		Some(month) => {
			let code = two_digits(month).ok_or_else(|| error("a month has two digits"))?;
			match (code, Season::from_code(code)) {
				(1..=12, _) => Month::Known(code),
				(_, Some(season)) => Month::Season(season),
				_ => return Err(error("no such month or season")),
			}
		}
	};
	let day = match (parts.next(), month) {
		(None, _) => DayOfMonth::None,
		(Some(_), Month::Season(_)) => return Err(error("a season has no days")),
		(Some("XX"), _) => DayOfMonth::Unspecified,
		(Some(_), Month::Unspecified) => {
			return Err(error("an unspecified month needs an unspecified day"));
		}
		(Some(day), Month::Known(month)) => {
			let day = two_digits(day).ok_or_else(|| error("a day has two digits"))?;
			if !(1..=days_in_month(year.value, month)).contains(&day) {
				return Err(error("no such day in that month"));
			}
			DayOfMonth::Known(day)
		}
		(Some(_), Month::None) => unreachable!("a day only follows a month"),
	};
	if parts.next().is_some() {
		return Err(error("unexpected text after the day"));
	}
	Ok(Date {
		year,
		month,
		day,
		qualifier,
	})
}

/// Reads a year, which may end in one or two unspecified digits (`201X`, `20XX`). Only
/// four-digit years without a sign can have unspecified digits.
fn parse_year_with_unspecified(text: &str) -> Result<(Year, &str), ParseEdtfError> {
	let four = text.get(..4).unwrap_or(text);
	let unspecified = four.bytes().rev().take_while(|&b| b == b'X').count();
	if four.len() == 4 && unspecified > 0 && four.as_bytes()[0].is_ascii_digit() {
		let specified = &four[..4 - unspecified];
		if unspecified > 2 || !specified.bytes().all(|b| b.is_ascii_digit()) {
			return Err(error(
				"only the last one or two digits of a year can be unspecified",
			));
		}
		let value = specified.parse().expect("only digits");
		let unspecified = u8::try_from(unspecified).expect("at most two");
		return Ok((Year { value, unspecified }, &text[4..]));
	}
	if text.contains('X') && (text.starts_with('-') || text.starts_with('Y')) {
		return Err(error(
			"only four-digit years without a sign can have unspecified digits",
		));
	}
	let (value, rest) = parse_year(text).ok_or_else(|| error("not a year"))?;
	Ok((
		Year {
			value,
			unspecified: 0,
		},
		rest,
	))
}

/// Reads a date and time that were split at the `T`. The text is ASCII.
fn parse_date_time(date: &str, time: &str) -> Result<DateTime, ParseEdtfError> {
	let date: Day = date
		.parse()
		.map_err(|_| error("a date with a time needs a full date"))?;
	let clock = time.get(..8).ok_or_else(|| error("a time is hh:mm:ss"))?;
	let field = |range: std::ops::Range<usize>, max: u8| {
		two_digits(&clock[range])
			.filter(|&value| value <= max)
			.ok_or_else(|| error("a time is hh:mm:ss"))
	};
	if &clock[2..3] != ":" || &clock[5..6] != ":" {
		return Err(error("a time is hh:mm:ss"));
	}
	let (hour, minute, second) = (field(0..2, 23)?, field(3..5, 59)?, field(6..8, 59)?);
	let offset = parse_offset(&time[8..])?;
	Ok(DateTime {
		date,
		hour,
		minute,
		second,
		offset,
	})
}

/// Reads a time zone. Each offset has one form: `Z` for UTC, `+hh` or `-hh` for whole
/// hours, and `+hh:mm` or `-hh:mm` otherwise. Offsets range from −12:00 to +14:00.
fn parse_offset(text: &str) -> Result<Option<Offset>, ParseEdtfError> {
	let invalid = || error("a time zone is Z, +hh or +hh:mm, from -12:00 to +14:00");
	let (negative, rest) = match text.as_bytes().first() {
		None => return Ok(None),
		Some(b'Z') if text.len() == 1 => return Ok(Some(Offset::Utc)),
		Some(b'+') => (false, &text[1..]),
		Some(b'-') => (true, &text[1..]),
		_ => return Err(invalid()),
	};
	let hours = rest.get(..2).and_then(two_digits).ok_or_else(invalid)?;
	let minutes = match &rest[2..] {
		"" => 0,
		minutes => match minutes.strip_prefix(':').and_then(two_digits) {
			Some(0) => return Err(error("whole hours are written +hh or -hh")),
			Some(minutes @ 1..=59) => minutes,
			_ => return Err(invalid()),
		},
	};
	let total = i32::from(hours) * 60 + i32::from(minutes);
	if total == 0 {
		return Err(error("UTC is written Z"));
	}
	Offset::from_minutes(if negative { -total } else { total })
		.map(Some)
		.ok_or_else(invalid)
}
