//! The values of kinds beyond the plain ones: decimals, quantities and names
//! (`docs/spec/model.md`, section 2).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use nibkeep_base::cbor::{Integer, Map, MapKey, Value as Cbor};

/// The largest power of ten that a readable decimal has, either way: its text and JSON
/// forms write every digit, so a larger one is kept unread.
const MAX_EXPONENT: i64 = 1000;

/// A decimal number, `mantissa × 10^exponent`, with the precision it was written with:
/// 1.5 and 1.50 are different decimals.
///
/// Its text form, which is also its form in JSON, is the digits with the decimal point
/// where the exponent puts it, such as `12.99` or `1.50`, and for a positive exponent the
/// mantissa, `e` and the exponent, such as `15e2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
	mantissa: i128,
	exponent: i64,
}

impl Decimal {
	/// The decimal `mantissa × 10^exponent`, or `None` if the mantissa is outside the
	/// integers of CBOR or the exponent outside −1000 to 1000.
	#[must_use]
	pub fn new(mantissa: i128, exponent: i64) -> Option<Self> {
		(Integer::new(mantissa).is_some() && (-MAX_EXPONENT..=MAX_EXPONENT).contains(&exponent))
			.then_some(Self { mantissa, exponent })
	}

	/// The digits.
	#[must_use]
	pub const fn mantissa(self) -> i128 {
		self.mantissa
	}

	/// The power of ten.
	#[must_use]
	pub const fn exponent(self) -> i64 {
		self.exponent
	}

	/// The decimal in CBOR: tag 4 with the exponent and the mantissa.
	pub(crate) fn to_cbor(self) -> Cbor {
		Cbor::Decimal {
			exponent: Integer::new(i128::from(self.exponent)).expect("within ±1000"),
			mantissa: Integer::new(self.mantissa).expect("checked when made"),
		}
	}

	/// Reads a decimal from CBOR, if it is one that this version can read.
	pub(crate) fn from_cbor(value: &Cbor) -> Option<Self> {
		let Cbor::Decimal { exponent, mantissa } = value else {
			return None;
		};
		Self::new(mantissa.get(), i64::try_from(exponent.get()).ok()?)
	}
}

impl fmt::Display for Decimal {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.exponent > 0 {
			return write!(f, "{}e{}", self.mantissa, self.exponent);
		}
		let digits = self.mantissa.unsigned_abs().to_string();
		let sign = if self.mantissa < 0 { "-" } else { "" };
		let places = usize::try_from(-self.exponent).expect("within 1000");
		if places == 0 {
			return write!(f, "{sign}{digits}");
		}
		let padded = format!("{digits:0>width$}", width = places + 1);
		let (whole, fraction) = padded.split_at(padded.len() - places);
		write!(f, "{sign}{whole}.{fraction}")
	}
}

/// Why text is not a decimal, a quantity or a name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseValueError(&'static str);

impl FromStr for Decimal {
	type Err = ParseValueError;

	/// Reads the text form: an optional `-`, digits without leading zeros, and either a
	/// decimal point with digits after it, or `e` and a positive exponent.
	fn from_str(text: &str) -> Result<Self, Self::Err> {
		const INVALID: ParseValueError =
			ParseValueError("a decimal is written such as 12.99, -0.5, 1500 or 15e2");
		let (negative, unsigned) = match text.strip_prefix('-') {
			Some(rest) => (true, rest),
			None => (false, text),
		};
		let digits =
			|part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
		let no_leading_zero = |part: &str| part == "0" || !part.starts_with('0');
		let (mantissa, exponent) = if let Some((whole, fraction)) = unsigned.split_once('.') {
			if !digits(whole) || !digits(fraction) || !no_leading_zero(whole) {
				return Err(INVALID);
			}
			let exponent = -i64::try_from(fraction.len()).map_err(|_| INVALID)?;
			(format!("{whole}{fraction}"), exponent)
		} else if let Some((whole, exponent)) = unsigned.split_once('e') {
			if !digits(whole)
				|| !digits(exponent)
				|| !no_leading_zero(whole)
				|| exponent.starts_with('0')
			{
				return Err(INVALID);
			}
			(whole.to_owned(), exponent.parse().map_err(|_| INVALID)?)
		} else {
			if !digits(unsigned) || !no_leading_zero(unsigned) {
				return Err(INVALID);
			}
			(unsigned.to_owned(), 0)
		};
		let magnitude: i128 = mantissa.parse().map_err(|_| INVALID)?;
		let mantissa = if negative { -magnitude } else { magnitude };
		if negative && mantissa == 0 {
			return Err(INVALID);
		}
		Self::new(mantissa, exponent).ok_or(INVALID)
	}
}

/// A decimal with its unit, such as 72.3 kg, written in the case-sensitive codes of UCUM,
/// such as `kg`, `cm` or `[lb_av]`. The model stores the unit as it was entered; apps
/// convert it to show it.
///
/// Its text form is the number, a space and the unit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Quantity {
	number: Decimal,
	unit: String,
}

impl Quantity {
	/// A quantity, or `None` if the unit is not one, as [`Quantity::is_unit`] says.
	#[must_use]
	pub fn new(number: Decimal, unit: impl Into<String>) -> Option<Self> {
		let unit = unit.into();
		Self::is_unit(&unit).then_some(Self { number, unit })
	}

	/// Whether text can be a unit: not empty, and without white space. Whether it is one of
	/// UCUM is for apps to tell, which convert units.
	#[must_use]
	pub fn is_unit(unit: &str) -> bool {
		!unit.is_empty() && !unit.chars().any(char::is_whitespace)
	}

	/// The number.
	#[must_use]
	pub const fn number(&self) -> Decimal {
		self.number
	}

	/// The unit, as written in UCUM.
	#[must_use]
	pub fn unit(&self) -> &str {
		&self.unit
	}

	/// The quantity in CBOR: the array `[number, unit]`.
	pub(crate) fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![self.number.to_cbor(), Cbor::from(self.unit.as_str())])
	}

	/// Reads a quantity from CBOR, if it is one that this version can read.
	pub(crate) fn from_cbor(value: &Cbor) -> Option<Self> {
		let Cbor::Array(items) = value else {
			return None;
		};
		let [number, Cbor::Text(unit)] = items.as_slice() else {
			return None;
		};
		Self::new(Decimal::from_cbor(number)?, unit.as_str())
	}
}

impl fmt::Display for Quantity {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{} {}", self.number, self.unit)
	}
}

impl FromStr for Quantity {
	type Err = ParseValueError;

	/// Reads the text form: a decimal, one space, and a unit without white space.
	fn from_str(text: &str) -> Result<Self, Self::Err> {
		const INVALID: ParseValueError = ParseValueError(
			"a quantity is written as a number, a space and a unit, such as 72.3 kg",
		);
		let (number, unit) = text.split_once(' ').ok_or(INVALID)?;
		Self::new(number.parse().map_err(|_| INVALID)?, unit).ok_or(INVALID)
	}
}

/// The name of an entity, such as a person, a city or a company, in its parts. `full`, the
/// name written in full in the order its culture writes it, is always there, and apps show
/// it; a person's name can also have `given`, `family`, `additional` for middle names and
/// patronymics, `prefix` and `suffix`. Parts from newer versions are kept.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Name {
	parts: BTreeMap<String, String>,
}

impl Name {
	/// The part that every name has.
	pub const FULL: &'static str = "full";

	/// A name written in full, without other parts.
	///
	/// # Panics
	///
	/// Panics if `full` is empty: every name is written in full.
	#[must_use]
	pub fn new(full: impl Into<String>) -> Self {
		let full = full.into();
		assert!(!full.is_empty(), "a name is not empty");
		Self {
			parts: BTreeMap::from([(Self::FULL.to_owned(), full)]),
		}
	}

	/// The same name with another part, such as `given` or `family`.
	///
	/// # Panics
	///
	/// Panics if the part is `full` and the text empty: every name is written in full.
	#[must_use]
	pub fn with_part(mut self, part: impl Into<String>, text: impl Into<String>) -> Self {
		let (part, text) = (part.into(), text.into());
		assert!(
			part != Self::FULL || !text.is_empty(),
			"a name is not empty"
		);
		self.parts.insert(part, text);
		self
	}

	/// The name written in full.
	#[must_use]
	pub fn full(&self) -> &str {
		&self.parts[Self::FULL]
	}

	/// A part of the name, if it has it.
	#[must_use]
	pub fn part(&self, part: &str) -> Option<&str> {
		self.parts.get(part).map(String::as_str)
	}

	/// Every part, ordered by the part's name.
	pub fn parts(&self) -> impl Iterator<Item = (&str, &str)> {
		self.parts
			.iter()
			.map(|(part, text)| (part.as_str(), text.as_str()))
	}

	/// The name in CBOR: a map from part names to text.
	pub(crate) fn to_cbor(&self) -> Cbor {
		let mut map = Map::new();
		for (part, text) in &self.parts {
			map.insert(part.as_str(), Cbor::from(text.as_str()));
		}
		Cbor::Map(map)
	}

	/// Reads a name from CBOR, if it is one that this version can read: a map of text parts
	/// with a part `full` that is not empty.
	pub(crate) fn from_cbor(value: &Cbor) -> Option<Self> {
		let Cbor::Map(map) = value else {
			return None;
		};
		let mut parts = BTreeMap::new();
		for (key, text) in map {
			let (MapKey::Text(part), Cbor::Text(text)) = (key, text) else {
				return None;
			};
			parts.insert(part.clone(), text.clone());
		}
		parts
			.get(Self::FULL)
			.is_some_and(|full| !full.is_empty())
			.then_some(Self { parts })
	}
}
