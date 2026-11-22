//! Helpers for the CBOR forms, which all start with the number of their suite.

use std::fmt;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};

/// The suite of every format in version 1 of the specification.
pub(crate) const SUITE: u8 = 1;

/// The length of the authentication tag of XChaCha20-Poly1305 and ChaCha20-Poly1305.
pub(crate) const TAG_LEN: usize = 16;

/// Writes the CBOR array `[1, items...]`.
pub(crate) fn write(items: impl IntoIterator<Item = Value>) -> Value {
	Value::Array(std::iter::once(SUITE.to_cbor()).chain(items).collect())
}

/// Writes the CBOR array `[1, items...]`, with the items as byte strings.
pub(crate) fn write_bytes(items: &[&[u8]]) -> Value {
	write(items.iter().map(|item| Value::Bytes(item.to_vec())))
}

/// Reads the CBOR array `[1, items...]` with `N` items, or fails with `expected`.
pub(crate) fn read<'a, const N: usize>(
	value: &'a Value,
	expected: &'static str,
) -> Result<&'a [Value; N], cbor::Error> {
	let error = || cbor::Error::Expected(expected);
	let Value::Array(items) = value else {
		return Err(error());
	};
	let Some((suite, items)) = items.split_first() else {
		return Err(error());
	};
	if u8::from_cbor(suite) != Ok(SUITE) {
		return Err(error());
	}
	items.try_into().map_err(|_| error())
}

/// Reads the CBOR array `[1, items...]` with `N` byte strings, or fails with `expected`.
pub(crate) fn read_bytes<'a, const N: usize>(
	value: &'a Value,
	expected: &'static str,
) -> Result<[&'a [u8]; N], cbor::Error> {
	let items: &[Value; N] = read(value, expected)?;
	let empty: &'a [u8] = &[];
	let mut fields = [empty; N];
	for (field, item) in fields.iter_mut().zip(items) {
		let Value::Bytes(bytes) = item else {
			return Err(cbor::Error::Expected(expected));
		};
		*field = bytes;
	}
	Ok(fields)
}

/// Prints bytes in hexadecimal.
pub(crate) struct Hex<'a>(pub(crate) &'a [u8]);

impl fmt::Display for Hex<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}
