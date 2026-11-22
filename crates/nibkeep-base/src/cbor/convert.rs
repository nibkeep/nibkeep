//! Converting between Rust types and canonical CBOR.

use super::{DecodeError, Integer, Value, decode, encode};

/// A Rust type that can be written as canonical CBOR.
pub trait ToCbor {
	/// Returns the value to encode.
	fn to_cbor(&self) -> Value;
}

/// A Rust type that can be read from canonical CBOR.
pub trait FromCbor: Sized {
	/// Reads the Rust value from a decoded value.
	///
	/// # Errors
	///
	/// Returns an error if `value` does not have the shape the type expects.
	fn from_cbor(value: &Value) -> Result<Self, Error>;
}

/// Encodes a Rust value canonically.
#[must_use]
pub fn to_vec<T: ToCbor + ?Sized>(value: &T) -> Vec<u8> {
	encode(&value.to_cbor())
}

/// Decodes a Rust value from its canonical encoding.
///
/// # Errors
///
/// Returns an error if `bytes` is not a canonical encoding, or if the value it encodes
/// does not have the shape that `T` expects.
pub fn from_slice<T: FromCbor>(bytes: &[u8]) -> Result<T, Error> {
	T::from_cbor(&decode(bytes)?)
}

/// Why a Rust value could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The input is not a canonical encoding.
	#[error(transparent)]
	Decode(#[from] DecodeError),
	/// The value does not have the expected shape.
	#[error("expected {0}")]
	Expected(&'static str),
}

/// A byte string. A plain `Vec<u8>` is an array of integers instead.
///
/// There is deliberately no conversion for `Option`: writing `None` as `null` would make
/// `Some(None)` and `None` indistinguishable. Structures leave out absent fields instead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl ToCbor for Bytes {
	fn to_cbor(&self) -> Value {
		Value::Bytes(self.0.clone())
	}
}

impl FromCbor for Bytes {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		match value {
			Value::Bytes(bytes) => Ok(Self(bytes.clone())),
			_ => Err(Error::Expected("a byte string")),
		}
	}
}

impl ToCbor for Value {
	fn to_cbor(&self) -> Value {
		self.clone()
	}
}

impl FromCbor for Value {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		Ok(value.clone())
	}
}

impl ToCbor for bool {
	fn to_cbor(&self) -> Value {
		Value::Bool(*self)
	}
}

impl FromCbor for bool {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		match value {
			Value::Bool(flag) => Ok(*flag),
			_ => Err(Error::Expected("true or false")),
		}
	}
}

impl ToCbor for String {
	fn to_cbor(&self) -> Value {
		Value::Text(self.clone())
	}
}

impl ToCbor for str {
	fn to_cbor(&self) -> Value {
		Value::Text(self.to_owned())
	}
}

impl FromCbor for String {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		match value {
			Value::Text(text) => Ok(text.clone()),
			_ => Err(Error::Expected("text")),
		}
	}
}

impl ToCbor for Integer {
	fn to_cbor(&self) -> Value {
		Value::Integer(*self)
	}
}

impl FromCbor for Integer {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		match value {
			Value::Integer(integer) => Ok(*integer),
			_ => Err(Error::Expected("an integer")),
		}
	}
}

macro_rules! cbor_integer {
	($($integer:ty),*) => {
		$(
			impl ToCbor for $integer {
				fn to_cbor(&self) -> Value {
					Value::Integer(Integer::from(*self))
				}
			}

			impl FromCbor for $integer {
				fn from_cbor(value: &Value) -> Result<Self, Error> {
					let integer = Integer::from_cbor(value)?;
					<$integer>::try_from(integer).map_err(|_| {
						Error::Expected(concat!("an integer that fits in ", stringify!($integer)))
					})
				}
			}
		)*
	};
}

cbor_integer!(u8, u16, u32, u64, i8, i16, i32, i64);

impl<T: ToCbor> ToCbor for Vec<T> {
	fn to_cbor(&self) -> Value {
		Value::Array(self.iter().map(ToCbor::to_cbor).collect())
	}
}

impl<T: FromCbor> FromCbor for Vec<T> {
	fn from_cbor(value: &Value) -> Result<Self, Error> {
		match value {
			Value::Array(items) => items.iter().map(T::from_cbor).collect(),
			_ => Err(Error::Expected("an array")),
		}
	}
}
