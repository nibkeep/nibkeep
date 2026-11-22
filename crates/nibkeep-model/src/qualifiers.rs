//! Qualifiers, which say more about a value.

use std::collections::BTreeMap;

use nibkeep_base::StatementId;
use nibkeep_base::cbor::{self, FromCbor, Map, MapKey, ToCbor, Value as Cbor};
use nibkeep_time::Edtf;

use crate::{FieldName, Value, fields};

/// Qualifiers of a value: when it was true, where it came from, how the source wrote it,
/// how confident its author was, and any other qualifier that a type defines.
///
/// In CBOR, qualifiers are a map from field names to values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Qualifiers(BTreeMap<FieldName, Value>);

impl Qualifiers {
	/// Returns no qualifiers.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Returns the qualifiers with `field` set to `value`.
	#[must_use]
	pub fn with(mut self, field: FieldName, value: Value) -> Self {
		self.0.insert(field, value);
		self
	}

	/// Sets `field` to `value`, and returns the value it had.
	pub fn insert(&mut self, field: FieldName, value: Value) -> Option<Value> {
		self.0.insert(field, value)
	}

	/// Removes `field`, and returns the value it had.
	pub fn remove(&mut self, field: &FieldName) -> Option<Value> {
		self.0.remove(field)
	}

	/// The value of a qualifier.
	#[must_use]
	pub fn get(&self, field: &FieldName) -> Option<&Value> {
		self.0.get(field)
	}

	/// All qualifiers, ordered by field name.
	pub fn iter(&self) -> impl Iterator<Item = (&FieldName, &Value)> {
		self.0.iter()
	}

	/// The number of qualifiers.
	#[must_use]
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether there are no qualifiers.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// When the value was true, if the qualifier is a date.
	#[must_use]
	pub fn valid_time(&self) -> Option<&Edtf> {
		self.get(&fields::VALID_TIME).and_then(Value::as_date)
	}

	/// Whether the value has a valid time at all, readable or not. A valid time that this
	/// version cannot read, such as one of another kind, makes [`Qualifiers::valid_time`]
	/// `None` all the same.
	#[must_use]
	pub fn has_valid_time(&self) -> bool {
		self.get(&fields::VALID_TIME).is_some()
	}

	/// Where the information came from, if the qualifier is a reference or text.
	#[must_use]
	pub fn source(&self) -> Option<&Value> {
		self.get(&fields::SOURCE)
			.filter(|value| matches!(value, Value::Reference(_) | Value::Text(_)))
	}

	/// How the source wrote the value, if the qualifier is text.
	#[must_use]
	pub fn credited_as(&self) -> Option<&str> {
		self.get(&fields::CREDITED_AS).and_then(Value::as_text)
	}

	/// How likely the value is to be right, in percent, if the qualifier is an integer
	/// from 0 to 100.
	#[must_use]
	pub fn confidence(&self) -> Option<u8> {
		match self.get(&fields::CONFIDENCE) {
			Some(Value::Integer(percent)) => u8::try_from(*percent).ok().filter(|&p| p <= 100),
			_ => None,
		}
	}

	/// The language tag of the value, as written, if the qualifier is text.
	#[must_use]
	pub fn language(&self) -> Option<&str> {
		self.get(&fields::LANGUAGE).and_then(Value::as_text)
	}

	/// The first form of the value that this value is another form of, if the qualifier
	/// is a statement ID in text.
	#[must_use]
	pub fn alternative(&self) -> Option<StatementId> {
		self.get(&fields::ALTERNATIVE)
			.and_then(Value::as_text)
			.and_then(|id| id.parse().ok())
	}

	/// Whether the value is the one to show first.
	#[must_use]
	pub fn is_preferred(&self) -> bool {
		matches!(self.get(&fields::PREFERRED), Some(Value::Boolean(true)))
	}

	/// The qualifiers without the notes about the value, such as a move's
	/// ([`fields::is_note`]): what values that are compared compare.
	#[must_use]
	pub fn without_notes(&self) -> Self {
		Self(
			self.0
				.iter()
				.filter(|(field, _)| !fields::is_note(field))
				.map(|(field, value)| (field.clone(), value.clone()))
				.collect(),
		)
	}
}

impl ToCbor for Qualifiers {
	fn to_cbor(&self) -> Cbor {
		let mut map = Map::new();
		for (field, value) in &self.0 {
			map.insert(field.as_str(), value.to_cbor());
		}
		Cbor::Map(map)
	}
}

impl FromCbor for Qualifiers {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "qualifiers: a map from field names to values";
		let Cbor::Map(map) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let mut qualifiers = BTreeMap::new();
		for (key, value) in map {
			let MapKey::Text(field) = key else {
				return Err(cbor::Error::Expected(EXPECTED));
			};
			let field = field.parse().map_err(|_| cbor::Error::Expected(EXPECTED))?;
			qualifiers.insert(field, Value::from_cbor(value)?);
		}
		Ok(Self(qualifiers))
	}
}
