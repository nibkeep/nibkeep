//! The values that canonical CBOR can encode.

use std::cmp::Ordering;
use std::collections::{BTreeMap, btree_map};

use super::{initial_byte, length, major};

/// An integer in the range CBOR represents directly: −2^64 to 2^64 − 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Integer(i128);

impl Integer {
	/// The smallest integer, −2^64.
	pub const MIN: Self = Self(-(1 << 64));
	/// The largest integer, 2^64 − 1.
	pub const MAX: Self = Self((1 << 64) - 1);

	/// Returns the integer, or `None` if `value` is outside the range CBOR represents.
	#[must_use]
	pub const fn new(value: i128) -> Option<Self> {
		if value >= Self::MIN.0 && value <= Self::MAX.0 {
			Some(Self(value))
		} else {
			None
		}
	}

	/// Returns the value.
	#[must_use]
	pub const fn get(self) -> i128 {
		self.0
	}

	/// The major type and argument of the integer's header: major type 0 with the value
	/// itself, or major type 1 with −1 − value.
	pub(super) fn header(self) -> (u8, u64) {
		if self.0 >= 0 {
			(
				major::UNSIGNED,
				u64::try_from(self.0).expect("at most 2^64 - 1"),
			)
		} else {
			(
				major::NEGATIVE,
				u64::try_from(-1 - self.0).expect("at least -2^64"),
			)
		}
	}
}

macro_rules! integer_from {
	($($from:ty),*) => {
		$(
			impl From<$from> for Integer {
				fn from(value: $from) -> Self {
					Self(i128::from(value))
				}
			}
		)*
	};
}

integer_from!(u8, u16, u32, u64, i8, i16, i32, i64);

/// The error returned when an [`Integer`] does not fit the requested type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("integer out of range")]
pub struct IntegerOutOfRange;

macro_rules! integer_into {
	($($into:ty),*) => {
		$(
			impl TryFrom<Integer> for $into {
				type Error = IntegerOutOfRange;

				fn try_from(value: Integer) -> Result<Self, Self::Error> {
					<$into>::try_from(value.0).map_err(|_| IntegerOutOfRange)
				}
			}
		)*
	};
}

integer_into!(u8, u16, u32, u64, i8, i16, i32, i64, usize);

/// A value that canonical CBOR can encode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
	/// An integer: major types 0 and 1.
	Integer(Integer),
	/// A byte string: major type 2.
	Bytes(Vec<u8>),
	/// A text string: major type 3.
	Text(String),
	/// An array: major type 4.
	Array(Vec<Value>),
	/// A map with integer or text keys: major type 5.
	Map(Map),
	/// A UUID: tag 37 with a 16-byte byte string.
	Uuid([u8; 16]),
	/// A decimal fraction, `mantissa × 10^exponent`: tag 4 with an array of the two
	/// integers. The precision is part of the value, so 1.5 and 1.50 are different values.
	Decimal {
		/// The power of ten.
		exponent: Integer,
		/// The digits.
		mantissa: Integer,
	},
	/// `true` or `false`.
	Bool(bool),
	/// `null`.
	Null,
}

impl From<Integer> for Value {
	fn from(value: Integer) -> Self {
		Self::Integer(value)
	}
}

impl From<String> for Value {
	fn from(value: String) -> Self {
		Self::Text(value)
	}
}

impl From<&str> for Value {
	fn from(value: &str) -> Self {
		Self::Text(value.to_owned())
	}
}

impl From<bool> for Value {
	fn from(value: bool) -> Self {
		Self::Bool(value)
	}
}

impl From<Map> for Value {
	fn from(value: Map) -> Self {
		Self::Map(value)
	}
}

impl From<Vec<Value>> for Value {
	fn from(value: Vec<Value>) -> Self {
		Self::Array(value)
	}
}

/// The key of a map entry: an integer or a text string.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MapKey {
	/// An integer key.
	Integer(Integer),
	/// A text key.
	Text(String),
}

impl MapKey {
	/// The parts of the key's encoding, in the order that decides the canonical order of
	/// keys: the first byte, the argument of the header and the payload. Comparing these
	/// gives the same result as comparing the encodings byte by byte, because two
	/// encodings with the same first byte write their arguments with the same width.
	fn order(&self) -> (u8, u64, &[u8]) {
		match self {
			Self::Integer(integer) => {
				let (major, argument) = integer.header();
				(initial_byte(major, argument), argument, &[])
			}
			Self::Text(text) => {
				let argument = length(text.len());
				(
					initial_byte(major::TEXT, argument),
					argument,
					text.as_bytes(),
				)
			}
		}
	}
}

impl Ord for MapKey {
	/// The canonical order: the bytewise order of the keys' encodings.
	fn cmp(&self, other: &Self) -> Ordering {
		self.order().cmp(&other.order())
	}
}

impl PartialOrd for MapKey {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl From<Integer> for MapKey {
	fn from(value: Integer) -> Self {
		Self::Integer(value)
	}
}

impl From<i64> for MapKey {
	fn from(value: i64) -> Self {
		Self::Integer(Integer::from(value))
	}
}

impl From<u64> for MapKey {
	fn from(value: u64) -> Self {
		Self::Integer(Integer::from(value))
	}
}

impl From<i32> for MapKey {
	fn from(value: i32) -> Self {
		Self::Integer(Integer::from(value))
	}
}

impl From<String> for MapKey {
	fn from(value: String) -> Self {
		Self::Text(value)
	}
}

impl From<&str> for MapKey {
	fn from(value: &str) -> Self {
		Self::Text(value.to_owned())
	}
}

/// A map whose entries are kept in canonical order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Map(BTreeMap<MapKey, Value>);

impl Map {
	/// Returns an empty map.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Inserts an entry and returns the value it replaced, if any.
	pub fn insert(&mut self, key: impl Into<MapKey>, value: impl Into<Value>) -> Option<Value> {
		self.0.insert(key.into(), value.into())
	}

	/// Returns the value for `key`.
	#[must_use]
	pub fn get(&self, key: &MapKey) -> Option<&Value> {
		self.0.get(key)
	}

	/// Removes the entry for `key` and returns its value.
	pub fn remove(&mut self, key: &MapKey) -> Option<Value> {
		self.0.remove(key)
	}

	/// The number of entries.
	#[must_use]
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether the map has no entries.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// The entries in canonical order.
	pub fn iter(&self) -> btree_map::Iter<'_, MapKey, Value> {
		self.0.iter()
	}

	/// Builds a map from entries whose keys are already unique and in canonical order.
	pub(super) fn from_sorted(entries: Vec<(MapKey, Value)>) -> Self {
		debug_assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
		Self(entries.into_iter().collect())
	}
}

impl FromIterator<(MapKey, Value)> for Map {
	fn from_iter<I: IntoIterator<Item = (MapKey, Value)>>(entries: I) -> Self {
		Self(entries.into_iter().collect())
	}
}

impl IntoIterator for Map {
	type Item = (MapKey, Value);
	type IntoIter = btree_map::IntoIter<MapKey, Value>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl<'a> IntoIterator for &'a Map {
	type Item = (&'a MapKey, &'a Value);
	type IntoIter = btree_map::Iter<'a, MapKey, Value>;

	fn into_iter(self) -> Self::IntoIter {
		self.0.iter()
	}
}
