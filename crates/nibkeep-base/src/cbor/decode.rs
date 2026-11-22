//! Reading values, accepting only their canonical encoding.

use std::cmp::Ordering;
use std::fmt;

use super::{Integer, Map, MapKey, TAG_DECIMAL, TAG_UUID, Value, major, simple};

/// The deepest nesting of arrays and maps that [`decode`] accepts.
pub const MAX_DEPTH: usize = 64;

/// Decodes a value from its canonical encoding.
///
/// # Errors
///
/// Returns an error if `bytes` is not exactly the canonical encoding of one value: for
/// example if a number is not in its shortest form, map keys are out of order, the input
/// ends early or continues after the value, or it uses a part of CBOR that canonical CBOR
/// leaves out, such as floating-point numbers.
pub fn decode(bytes: &[u8]) -> Result<Value, DecodeError> {
	let mut decoder = Decoder { bytes, position: 0 };
	let value = decoder.value(0)?;
	if decoder.position == bytes.len() {
		Ok(value)
	} else {
		Err(DecodeError::new(
			decoder.position,
			DecodeErrorKind::TrailingBytes,
		))
	}
}

/// Decodes the first value of a sequence of values ([RFC 8742]), each in its canonical
/// encoding, and returns it with the length of its encoding, where the next value starts.
///
/// [RFC 8742]: https://www.rfc-editor.org/rfc/rfc8742
///
/// # Errors
///
/// Returns an error if `bytes` does not start with the canonical encoding of a value.
pub fn decode_first(bytes: &[u8]) -> Result<(Value, usize), DecodeError> {
	let mut decoder = Decoder { bytes, position: 0 };
	let value = decoder.value(0)?;
	Ok((value, decoder.position))
}

/// Why input was rejected, and where.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at byte {offset}")]
pub struct DecodeError {
	offset: usize,
	kind: DecodeErrorKind,
}

impl DecodeError {
	fn new(offset: usize, kind: DecodeErrorKind) -> Self {
		Self { offset, kind }
	}

	/// The position in the input of the data item that was rejected.
	#[must_use]
	pub fn offset(&self) -> usize {
		self.offset
	}

	/// Why the input was rejected.
	#[must_use]
	pub fn kind(&self) -> &DecodeErrorKind {
		&self.kind
	}
}

/// The reasons why input is rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeErrorKind {
	/// The input ends in the middle of a data item, or a length is longer than the rest of
	/// the input.
	UnexpectedEnd,
	/// There are bytes after the end of the value.
	TrailingBytes,
	/// A number, length or tag is not in its shortest form.
	NotShortest,
	/// A byte string, text string, array or map has an indefinite length.
	IndefiniteLength,
	/// A header uses reserved additional information (28 to 30).
	Reserved,
	/// A floating-point number.
	Float,
	/// A simple value other than `false`, `true` and `null`.
	SimpleValue,
	/// A text string is not valid UTF-8.
	InvalidUtf8,
	/// A map key is neither an integer nor a text string.
	InvalidMapKey,
	/// A map key is not greater than the key before it.
	UnsortedKeys,
	/// A map key appears twice.
	DuplicateKey,
	/// A tag other than the decimal fraction (4) and the UUID (37).
	UnsupportedTag,
	/// A tag's content does not have the shape the tag requires.
	InvalidTagContent,
	/// Arrays and maps are nested more than [`MAX_DEPTH`] levels deep.
	TooDeep,
}

impl fmt::Display for DecodeErrorKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			Self::UnexpectedEnd => "unexpected end of input",
			Self::TrailingBytes => "bytes after the end of the value",
			Self::NotShortest => "number not in its shortest form",
			Self::IndefiniteLength => "indefinite length",
			Self::Reserved => "reserved additional information",
			Self::Float => "floating-point number",
			Self::SimpleValue => "simple value other than false, true and null",
			Self::InvalidUtf8 => "text that is not valid UTF-8",
			Self::InvalidMapKey => "map key that is neither an integer nor text",
			Self::UnsortedKeys => "map key out of order",
			Self::DuplicateKey => "duplicate map key",
			Self::UnsupportedTag => "unsupported tag",
			Self::InvalidTagContent => "invalid tag content",
			Self::TooDeep => "nesting too deep",
		})
	}
}

struct Decoder<'a> {
	bytes: &'a [u8],
	position: usize,
}

impl<'a> Decoder<'a> {
	/// Reads the data item at the current position. `depth` is the number of arrays and
	/// maps around it.
	fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
		let start = self.position;
		let initial = self.byte()?;
		let (major, info) = (initial >> 5, initial & 0x1f);
		match major {
			major::UNSIGNED => {
				let argument = self.argument(info, start)?;
				Ok(Value::Integer(Integer::from(argument)))
			}
			major::NEGATIVE => {
				let argument = self.argument(info, start)?;
				let value = Integer::new(-1 - i128::from(argument)).expect("at least -2^64");
				Ok(Value::Integer(value))
			}
			major::BYTES => {
				let argument = self.argument(info, start)?;
				Ok(Value::Bytes(self.take(argument, start)?.to_vec()))
			}
			major::TEXT => {
				let argument = self.argument(info, start)?;
				let bytes = self.take(argument, start)?;
				let text = std::str::from_utf8(bytes)
					.map_err(|_| DecodeError::new(start, DecodeErrorKind::InvalidUtf8))?;
				Ok(Value::Text(text.to_owned()))
			}
			major::ARRAY => {
				let argument = self.argument(info, start)?;
				Self::enter(depth, start)?;
				let count = self.count(argument, start)?;
				let mut items = Vec::with_capacity(count);
				for _ in 0..count {
					items.push(self.value(depth + 1)?);
				}
				Ok(Value::Array(items))
			}
			major::MAP => {
				let argument = self.argument(info, start)?;
				Self::enter(depth, start)?;
				let count = self.count(argument, start)?;
				self.map(count, depth)
			}
			major::TAG => {
				let argument = self.argument(info, start)?;
				self.tag(argument, start)
			}
			_ => Self::simple(initial, start),
		}
	}

	fn map(&mut self, count: usize, depth: usize) -> Result<Value, DecodeError> {
		let mut entries: Vec<(MapKey, Value)> = Vec::with_capacity(count);
		for _ in 0..count {
			let key_start = self.position;
			let key = self.map_key()?;
			if let Some((previous, _)) = entries.last() {
				match previous.cmp(&key) {
					Ordering::Less => {}
					Ordering::Equal => {
						return Err(DecodeError::new(key_start, DecodeErrorKind::DuplicateKey));
					}
					Ordering::Greater => {
						return Err(DecodeError::new(key_start, DecodeErrorKind::UnsortedKeys));
					}
				}
			}
			let value = self.value(depth + 1)?;
			entries.push((key, value));
		}
		Ok(Value::Map(Map::from_sorted(entries)))
	}

	/// Reads a map key, which must be an integer or a text string. Anything else is
	/// rejected before it is read, so a key can never start a nested structure.
	fn map_key(&mut self) -> Result<MapKey, DecodeError> {
		let start = self.position;
		let major = self.peek()? >> 5;
		if !matches!(major, major::UNSIGNED | major::NEGATIVE | major::TEXT) {
			return Err(DecodeError::new(start, DecodeErrorKind::InvalidMapKey));
		}
		match self.value(0)? {
			Value::Integer(integer) => Ok(MapKey::Integer(integer)),
			Value::Text(text) => Ok(MapKey::Text(text)),
			_ => unreachable!("major types 0, 1 and 3 decode to integers or text"),
		}
	}

	/// Reads the content of the tag whose header started at `start`. The content of each
	/// allowed tag has a fixed shape, so it is read directly rather than as a nested value:
	/// tags never nest, and they do not count towards [`MAX_DEPTH`].
	fn tag(&mut self, number: u64, start: usize) -> Result<Value, DecodeError> {
		match number {
			TAG_DECIMAL => {
				let content = self.position;
				let invalid = || DecodeError::new(content, DecodeErrorKind::InvalidTagContent);
				let (major, count) = self.header()?;
				if major != major::ARRAY || count != 2 {
					return Err(invalid());
				}
				let exponent = self.integer().ok_or_else(invalid)??;
				let mantissa = self.integer().ok_or_else(invalid)??;
				Ok(Value::Decimal { exponent, mantissa })
			}
			TAG_UUID => {
				let content = self.position;
				let invalid = || DecodeError::new(content, DecodeErrorKind::InvalidTagContent);
				let (major, len) = self.header()?;
				if major != major::BYTES || len != 16 {
					return Err(invalid());
				}
				let bytes = self.take(len, content)?;
				Ok(Value::Uuid(bytes.try_into().expect("took 16 bytes")))
			}
			_ => Err(DecodeError::new(start, DecodeErrorKind::UnsupportedTag)),
		}
	}

	/// Reads an integer, or returns `None` without consuming anything if the next data
	/// item is not an integer.
	fn integer(&mut self) -> Option<Result<Integer, DecodeError>> {
		let major = match self.peek() {
			Ok(initial) => initial >> 5,
			Err(error) => return Some(Err(error)),
		};
		if !matches!(major, major::UNSIGNED | major::NEGATIVE) {
			return None;
		}
		Some(self.value(0).map(|value| match value {
			Value::Integer(integer) => integer,
			_ => unreachable!("major types 0 and 1 decode to integers"),
		}))
	}

	/// Reads a header of major types 0 to 6 and returns its major type and argument.
	fn header(&mut self) -> Result<(u8, u64), DecodeError> {
		let start = self.position;
		let initial = self.byte()?;
		let (major, info) = (initial >> 5, initial & 0x1f);
		if major > major::TAG {
			return Ok((major, 0));
		}
		Ok((major, self.argument(info, start)?))
	}

	fn simple(initial: u8, start: usize) -> Result<Value, DecodeError> {
		match initial {
			simple::FALSE => Ok(Value::Bool(false)),
			simple::TRUE => Ok(Value::Bool(true)),
			simple::NULL => Ok(Value::Null),
			0xf9..=0xfb => Err(DecodeError::new(start, DecodeErrorKind::Float)),
			0xfc..=0xfe => Err(DecodeError::new(start, DecodeErrorKind::Reserved)),
			_ => Err(DecodeError::new(start, DecodeErrorKind::SimpleValue)),
		}
	}

	/// Fails if an array or map at `depth` would be nested too deeply.
	fn enter(depth: usize, start: usize) -> Result<(), DecodeError> {
		if depth < MAX_DEPTH {
			Ok(())
		} else {
			Err(DecodeError::new(start, DecodeErrorKind::TooDeep))
		}
	}

	/// Reads the argument of a header whose first byte had the additional information
	/// `info`, and checks that it is in its shortest form.
	fn argument(&mut self, info: u8, start: usize) -> Result<u64, DecodeError> {
		let not_shortest = || DecodeError::new(start, DecodeErrorKind::NotShortest);
		match info {
			0..=23 => Ok(u64::from(info)),
			24 => {
				let argument = u64::from(self.byte()?);
				if argument < 24 {
					Err(not_shortest())
				} else {
					Ok(argument)
				}
			}
			25 => {
				let argument = u64::from(u16::from_be_bytes(self.array()?));
				if argument <= 0xff {
					Err(not_shortest())
				} else {
					Ok(argument)
				}
			}
			26 => {
				let argument = u64::from(u32::from_be_bytes(self.array()?));
				if argument <= 0xffff {
					Err(not_shortest())
				} else {
					Ok(argument)
				}
			}
			27 => {
				let argument = u64::from_be_bytes(self.array()?);
				if argument <= 0xffff_ffff {
					Err(not_shortest())
				} else {
					Ok(argument)
				}
			}
			28..=30 => Err(DecodeError::new(start, DecodeErrorKind::Reserved)),
			_ => Err(DecodeError::new(start, DecodeErrorKind::IndefiniteLength)),
		}
	}

	/// Checks that `count` items can follow: each takes at least one byte, so a count
	/// larger than the rest of the input cannot be right. This also keeps the decoder from
	/// reserving memory for a count that the input could never fill.
	fn count(&self, count: u64, start: usize) -> Result<usize, DecodeError> {
		usize::try_from(count)
			.ok()
			.filter(|&count| count <= self.remaining())
			.ok_or_else(|| DecodeError::new(start, DecodeErrorKind::UnexpectedEnd))
	}

	/// Takes the next `len` bytes.
	fn take(&mut self, len: u64, start: usize) -> Result<&'a [u8], DecodeError> {
		let len = self.count(len, start)?;
		let bytes = &self.bytes[self.position..self.position + len];
		self.position += len;
		Ok(bytes)
	}

	fn peek(&self) -> Result<u8, DecodeError> {
		self.bytes
			.get(self.position)
			.copied()
			.ok_or_else(|| DecodeError::new(self.position, DecodeErrorKind::UnexpectedEnd))
	}

	fn byte(&mut self) -> Result<u8, DecodeError> {
		let byte = *self
			.bytes
			.get(self.position)
			.ok_or_else(|| DecodeError::new(self.position, DecodeErrorKind::UnexpectedEnd))?;
		self.position += 1;
		Ok(byte)
	}

	fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
		let start = self.position;
		let bytes = self.take(N as u64, start)?;
		Ok(bytes.try_into().expect("took exactly N bytes"))
	}

	fn remaining(&self) -> usize {
		self.bytes.len() - self.position
	}
}
