//! Random identifiers for entities, spaces and people.

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::cbor::{self, FromCbor, ToCbor, Value};

macro_rules! identifier {
	($(#[$doc:meta])* $name:ident) => {
		$(#[$doc])*
		///
		/// New identifiers are version 4 UUIDs. The text form is the lowercase hyphenated
		/// form of the UUID; in CBOR, an identifier is a UUID tag.
		#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
		pub struct $name(Uuid);

		impl $name {
			/// Makes a new identifier from 16 random bytes. Six of the bits are replaced
			/// by the version and variant of a version 4 UUID.
			#[must_use]
			pub fn from_random_bytes(bytes: [u8; 16]) -> Self {
				Self(uuid::Builder::from_random_bytes(bytes).into_uuid())
			}

			/// The identifier derived from `material` with `context`: the first 16 bytes of
			/// the BLAKE3 key derived with that context from it, as a version 4 UUID. Every
			/// replica that derives one from the same material gets the same identifier.
			#[must_use]
			pub fn derived(context: &str, material: &[u8]) -> Self {
				let key = blake3::derive_key(context, material);
				let mut bytes = [0; 16];
				bytes.copy_from_slice(&key[..16]);
				Self::from_random_bytes(bytes)
			}

			/// Makes a new identifier from 16 bytes of the random number generator.
			pub fn random(rng: &mut impl rand_core::CryptoRng) -> Self {
				let mut bytes = [0; 16];
				rng.fill_bytes(&mut bytes);
				Self::from_random_bytes(bytes)
			}

			/// Returns the identifier with these 16 bytes, as stored. Any UUID is accepted,
			/// because an identifier is opaque.
			#[must_use]
			pub fn from_bytes(bytes: [u8; 16]) -> Self {
				Self(Uuid::from_bytes(bytes))
			}

			/// The identifier's 16 bytes.
			#[must_use]
			pub fn as_bytes(&self) -> &[u8; 16] {
				self.0.as_bytes()
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				fmt::Display::fmt(&self.0.hyphenated(), f)
			}
		}

		impl fmt::Debug for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				write!(f, concat!(stringify!($name), "({})"), self.0.hyphenated())
			}
		}

		impl FromStr for $name {
			type Err = ParseIdError;

			/// Reads the text form. Other forms of UUIDs, such as uppercase ones or ones
			/// without hyphens, are rejected, so that every identifier has one text form.
			fn from_str(text: &str) -> Result<Self, Self::Err> {
				let uuid = Uuid::try_parse(text).map_err(|_| ParseIdError)?;
				let id = Self(uuid);
				if id.to_string() == text { Ok(id) } else { Err(ParseIdError) }
			}
		}

		impl ToCbor for $name {
			fn to_cbor(&self) -> Value {
				Value::Uuid(*self.as_bytes())
			}
		}

		impl FromCbor for $name {
			fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
				match value {
					Value::Uuid(bytes) => Ok(Self::from_bytes(*bytes)),
					_ => Err(cbor::Error::Expected("a UUID")),
				}
			}
		}
	};
}

identifier!(
	/// Identifies an entity: a person, a photo, a book, a list or anything else.
	EntityId
);

identifier!(
	/// Identifies a space, the unit of sharing, sync and encryption.
	SpaceId
);

identifier!(
	/// Identifies a person who uses NibKeep.
	PersonId
);

identifier!(
	/// Identifies a statement: one value of one field of an entity, with its qualifiers.
	StatementId
);

/// The error returned for text that is not an identifier in its text form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not an identifier in the form 00000000-0000-0000-0000-000000000000")]
pub struct ParseIdError;
