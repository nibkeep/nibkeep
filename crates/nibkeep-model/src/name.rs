//! Names of types and fields.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};

const MAX_LEN: usize = 128;

/// Whether `name` is a valid name: two or more segments separated by dots, each made of
/// lowercase ASCII letters, digits and hyphens without a hyphen at either end, and at
/// most 128 bytes in all.
const fn is_valid(name: &str) -> bool {
	let bytes = name.as_bytes();
	if bytes.len() > MAX_LEN {
		return false;
	}
	let mut segments = 0;
	let mut start = 0;
	let mut index = 0;
	while index <= bytes.len() {
		if index == bytes.len() || bytes[index] == b'.' {
			if index == start || bytes[start] == b'-' || bytes[index - 1] == b'-' {
				return false;
			}
			segments += 1;
			start = index + 1;
		} else if !(bytes[index].is_ascii_lowercase()
			|| bytes[index].is_ascii_digit()
			|| bytes[index] == b'-')
		{
			return false;
		}
		index += 1;
	}
	segments >= 2
}

/// The most names that the table of shared names holds. Spaces use a few hundred. When a
/// new name finds the table full, the table starts again, empty: names already parsed keep
/// their texts, and the names in use come back as they are parsed again. So the table
/// never grows without end, and names from a space with countless of them, such as junk
/// from a misbehaving replica, cost a refill but never end sharing for other spaces.
const SHARED: usize = 4096;

/// The text of a name: fixed when the program was compiled, or shared by every copy.
#[derive(Clone)]
enum Text {
	Static(&'static str),
	Shared(Arc<str>),
}

impl Text {
	fn as_str(&self) -> &str {
		match self {
			Self::Static(text) => text,
			Self::Shared(text) => text,
		}
	}

	/// The text of a parsed name, shared with the names parsed from the same text before
	/// it. Names are few and parsed again for every statement read, so sharing them saves
	/// an allocation each time.
	fn shared(text: &str) -> Self {
		static NAMES: OnceLock<Mutex<HashSet<Arc<str>>>> = OnceLock::new();
		// A panic while the table was locked leaves it consistent: it only holds names.
		let mut names = NAMES
			.get_or_init(Mutex::default)
			.lock()
			.unwrap_or_else(PoisonError::into_inner);
		if let Some(name) = names.get(text) {
			return Self::Shared(Arc::clone(name));
		}
		if names.len() >= SHARED {
			names.clear();
		}
		let name: Arc<str> = Arc::from(text);
		names.insert(Arc::clone(&name));
		Self::Shared(name)
	}
}

macro_rules! name {
	($(#[$doc:meta])* $name:ident) => {
		$(#[$doc])*
		///
		/// A name has two or more segments separated by dots, made of lowercase ASCII
		/// letters, digits and hyphens, and starts with the reversed internet domain of
		/// whoever defines it. In CBOR, a name is a text string.
		///
		/// Copies share the text, and so do names parsed from the same text. Names compare,
		/// order and hash by their text.
		#[derive(Clone)]
		pub struct $name(Text);

		impl $name {
			/// Returns the name for text that is fixed when the program is compiled.
			///
			/// # Panics
			///
			/// Panics if `name` is not a valid name. In a constant, the program does not
			/// compile.
			#[must_use]
			pub const fn from_static(name: &'static str) -> Self {
				assert!(is_valid(name), "not a valid name");
				Self(Text::Static(name))
			}

			/// The name as text.
			#[must_use]
			pub fn as_str(&self) -> &str {
				self.0.as_str()
			}

			/// Whether NibKeep defines the name: whether it starts with `org.nibkeep.`.
			#[must_use]
			pub fn is_nibkeep(&self) -> bool {
				self.as_str().starts_with("org.nibkeep.")
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.write_str(self.as_str())
			}
		}

		impl fmt::Debug for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.debug_tuple(stringify!($name)).field(&self.as_str()).finish()
			}
		}

		impl PartialEq for $name {
			fn eq(&self, other: &Self) -> bool {
				self.as_str() == other.as_str()
			}
		}

		impl Eq for $name {}

		impl PartialOrd for $name {
			fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
				Some(self.cmp(other))
			}
		}

		impl Ord for $name {
			fn cmp(&self, other: &Self) -> Ordering {
				self.as_str().cmp(other.as_str())
			}
		}

		impl Hash for $name {
			fn hash<H: Hasher>(&self, state: &mut H) {
				self.as_str().hash(state);
			}
		}

		impl FromStr for $name {
			type Err = ParseNameError;

			fn from_str(text: &str) -> Result<Self, Self::Err> {
				if is_valid(text) {
					Ok(Self(Text::shared(text)))
				} else {
					Err(ParseNameError)
				}
			}
		}

		impl ToCbor for $name {
			fn to_cbor(&self) -> Value {
				Value::Text(self.as_str().to_owned())
			}
		}

		impl FromCbor for $name {
			fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
				const EXPECTED: &str = "a name such as org.nibkeep.name";
				match value {
					Value::Text(text) => text.parse().map_err(|_| cbor::Error::Expected(EXPECTED)),
					_ => Err(cbor::Error::Expected(EXPECTED)),
				}
			}
		}
	};
}

name!(
	/// The name of a type, such as `std.book`.
	TypeName
);

name!(
	/// The name of a field, such as `org.nibkeep.name`.
	FieldName
);

name!(
	/// The name of a migration, such as `std.editions`, from which the IDs of what it writes
	/// are derived ([`crate::migration`]). A migration never changes once released: one that
	/// does something else takes another name.
	MigrationName
);

/// The error returned for text that is not a valid name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a name such as org.nibkeep.name")]
pub struct ParseNameError;
