//! Values of fields.

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash};
use nibkeep_time::Edtf;

use crate::kinds::{Decimal, Name, Quantity};

/// A value of a field: a literal, or a reference to another entity.
///
/// In CBOR, a value is the array `[kind, payload]`. Values that this version of NibKeep
/// cannot read are kept as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
	/// Text.
	Text(String),
	/// A whole number.
	Integer(i64),
	/// Yes or no.
	Boolean(bool),
	/// A date, a date and time, or an interval, with any precision and uncertainty.
	Date(Edtf),
	/// Another entity.
	Reference(EntityId),
	/// The content of a file, by its hash.
	Blob(Hash),
	/// A decimal number, with the precision it was written with.
	Decimal(Decimal),
	/// A decimal number with its unit.
	Quantity(Quantity),
	/// The name of an entity, in its parts.
	Name(Name),
	/// A value that this version of NibKeep cannot read, kept as it is.
	Unknown(UnknownValue),
}

/// A value that this version of NibKeep cannot read: one of a kind it does not know, or
/// one whose payload it cannot parse, such as a date in a newer format.
///
/// Only decoding makes one, so it never equals a value that this version can read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownValue {
	kind: u64,
	payload: Cbor,
}

impl UnknownValue {
	/// The kind of the value. It can be a known kind, if only the payload is unreadable.
	#[must_use]
	pub const fn kind(&self) -> ValueKind {
		ValueKind::from_code(self.kind)
	}

	/// The payload, in CBOR.
	#[must_use]
	pub const fn payload(&self) -> &Cbor {
		&self.payload
	}
}

/// The kind of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueKind {
	/// Text.
	Text,
	/// A whole number.
	Integer,
	/// Yes or no.
	Boolean,
	/// A date.
	Date,
	/// Another entity.
	Reference,
	/// The content of a file.
	Blob,
	/// A decimal number.
	Decimal,
	/// A decimal number with its unit.
	Quantity,
	/// The name of an entity.
	Name,
	/// A kind that this version of NibKeep does not know.
	Other(OtherKind),
}

/// A kind of value that this version of NibKeep does not know. Only
/// [`ValueKind::from_code`] makes one, so its number is never that of a known kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OtherKind(u64);

impl OtherKind {
	/// The number that stands for the kind in CBOR.
	#[must_use]
	pub const fn code(self) -> u64 {
		self.0
	}
}

impl ValueKind {
	/// The number that stands for the kind in CBOR.
	#[must_use]
	pub const fn code(self) -> u64 {
		match self {
			Self::Text => 0,
			Self::Integer => 1,
			Self::Boolean => 2,
			Self::Date => 3,
			Self::Reference => 4,
			Self::Blob => 5,
			Self::Decimal => 6,
			Self::Quantity => 7,
			Self::Name => 8,
			Self::Other(other) => other.0,
		}
	}

	/// The kind that a number stands for.
	#[must_use]
	pub const fn from_code(code: u64) -> Self {
		match code {
			0 => Self::Text,
			1 => Self::Integer,
			2 => Self::Boolean,
			3 => Self::Date,
			4 => Self::Reference,
			5 => Self::Blob,
			6 => Self::Decimal,
			7 => Self::Quantity,
			8 => Self::Name,
			other => Self::Other(OtherKind(other)),
		}
	}
}

impl Value {
	/// The kind of the value.
	#[must_use]
	pub fn kind(&self) -> ValueKind {
		match self {
			Self::Text(_) => ValueKind::Text,
			Self::Integer(_) => ValueKind::Integer,
			Self::Boolean(_) => ValueKind::Boolean,
			Self::Date(_) => ValueKind::Date,
			Self::Reference(_) => ValueKind::Reference,
			Self::Blob(_) => ValueKind::Blob,
			Self::Decimal(_) => ValueKind::Decimal,
			Self::Quantity(_) => ValueKind::Quantity,
			Self::Name(_) => ValueKind::Name,
			Self::Unknown(unknown) => unknown.kind(),
		}
	}

	/// The text, if the value is text.
	#[must_use]
	pub fn as_text(&self) -> Option<&str> {
		match self {
			Self::Text(text) => Some(text),
			_ => None,
		}
	}

	/// The date, if the value is a date.
	#[must_use]
	pub fn as_date(&self) -> Option<&Edtf> {
		match self {
			Self::Date(date) => Some(date),
			_ => None,
		}
	}

	/// The text that search finds the value by: text itself, and every part of a name.
	#[must_use]
	pub fn searchable_texts(&self) -> Vec<&str> {
		match self {
			Self::Text(text) => vec![text.as_str()],
			Self::Name(name) => name.parts().map(|(_, text)| text).collect(),
			_ => Vec::new(),
		}
	}

	/// The text to show for the value where a name is expected: a name written in full, or
	/// text.
	#[must_use]
	pub fn as_name_text(&self) -> Option<&str> {
		match self {
			Self::Name(name) => Some(name.full()),
			Self::Text(text) => Some(text),
			_ => None,
		}
	}

	/// The name, if the value is a name.
	#[must_use]
	pub fn as_name(&self) -> Option<&Name> {
		match self {
			Self::Name(name) => Some(name),
			_ => None,
		}
	}

	/// The entity referred to, if the value is a reference.
	#[must_use]
	pub fn as_reference(&self) -> Option<EntityId> {
		match self {
			Self::Reference(entity) => Some(*entity),
			_ => None,
		}
	}
}

impl ToCbor for Value {
	fn to_cbor(&self) -> Cbor {
		let payload = match self {
			Self::Text(text) => text.to_cbor(),
			Self::Integer(integer) => integer.to_cbor(),
			Self::Boolean(boolean) => boolean.to_cbor(),
			Self::Date(date) => date.to_cbor(),
			Self::Reference(entity) => entity.to_cbor(),
			Self::Blob(hash) => hash.to_cbor(),
			Self::Decimal(decimal) => decimal.to_cbor(),
			Self::Quantity(quantity) => quantity.to_cbor(),
			Self::Name(name) => name.to_cbor(),
			Self::Unknown(unknown) => unknown.payload.clone(),
		};
		Cbor::Array(vec![self.kind().code().to_cbor(), payload])
	}
}

impl FromCbor for Value {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a value: [kind, payload]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [kind, payload] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let code = u64::from_cbor(kind).map_err(|_| cbor::Error::Expected(EXPECTED))?;
		let known = match ValueKind::from_code(code) {
			ValueKind::Text => String::from_cbor(payload).ok().map(Self::Text),
			ValueKind::Integer => i64::from_cbor(payload).ok().map(Self::Integer),
			ValueKind::Boolean => bool::from_cbor(payload).ok().map(Self::Boolean),
			ValueKind::Date => Edtf::from_cbor(payload).ok().map(Self::Date),
			ValueKind::Reference => EntityId::from_cbor(payload).ok().map(Self::Reference),
			ValueKind::Blob => Hash::from_cbor(payload).ok().map(Self::Blob),
			ValueKind::Decimal => Decimal::from_cbor(payload).map(Self::Decimal),
			ValueKind::Quantity => Quantity::from_cbor(payload).map(Self::Quantity),
			ValueKind::Name => Name::from_cbor(payload).map(Self::Name),
			ValueKind::Other(_) => None,
		};
		// What this version cannot read is kept, so that it is never lost.
		Ok(known.unwrap_or_else(|| {
			Self::Unknown(UnknownValue {
				kind: code,
				payload: payload.clone(),
			})
		}))
	}
}
