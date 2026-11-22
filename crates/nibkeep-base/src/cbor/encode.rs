//! Writing values in their canonical encoding.

use super::{MapKey, TAG_DECIMAL, TAG_UUID, Value, initial_byte, length, major, simple};

/// Encodes a value canonically.
///
/// Values nested more deeply than [`MAX_DEPTH`](super::MAX_DEPTH) levels are encoded, but
/// [`decode`](super::decode) rejects them.
#[must_use]
pub fn encode(value: &Value) -> Vec<u8> {
	let mut out = Vec::with_capacity(encoded_len(value));
	write_value(&mut out, value);
	out
}

/// Appends the canonical encoding of a value to `out`. With room for
/// [`encoded_len`] more bytes reserved beforehand, `out` is not reallocated, so no copy of
/// the encoding is left in memory that was freed: useful for secrets.
pub fn encode_into(value: &Value, out: &mut Vec<u8>) {
	write_value(out, value);
}

/// The length of the canonical encoding of a value.
#[must_use]
pub fn encoded_len(value: &Value) -> usize {
	match value {
		Value::Integer(integer) => header_len(integer.header().1),
		Value::Bytes(bytes) => header_len(length(bytes.len())) + bytes.len(),
		Value::Text(text) => header_len(length(text.len())) + text.len(),
		Value::Array(items) => {
			header_len(length(items.len())) + items.iter().map(encoded_len).sum::<usize>()
		}
		Value::Map(map) => {
			let entries: usize = map
				.iter()
				.map(|(key, value)| {
					let key = match key {
						MapKey::Integer(integer) => header_len(integer.header().1),
						MapKey::Text(text) => header_len(length(text.len())) + text.len(),
					};
					key + encoded_len(value)
				})
				.sum();
			header_len(length(map.len())) + entries
		}
		Value::Uuid(bytes) => header_len(TAG_UUID) + header_len(length(bytes.len())) + bytes.len(),
		Value::Decimal { exponent, mantissa } => {
			header_len(TAG_DECIMAL)
				+ header_len(2)
				+ header_len(exponent.header().1)
				+ header_len(mantissa.header().1)
		}
		Value::Bool(_) | Value::Null => 1,
	}
}

/// The length of a header in the shortest form.
fn header_len(argument: u64) -> usize {
	match argument {
		0..=23 => 1,
		24..=0xff => 2,
		0x100..=0xffff => 3,
		0x1_0000..=0xffff_ffff => 5,
		_ => 9,
	}
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
	match value {
		Value::Integer(integer) => {
			let (major, argument) = integer.header();
			write_header(out, major, argument);
		}
		Value::Bytes(bytes) => {
			write_header(out, major::BYTES, length(bytes.len()));
			out.extend_from_slice(bytes);
		}
		Value::Text(text) => write_text(out, text),
		Value::Array(items) => {
			write_header(out, major::ARRAY, length(items.len()));
			for item in items {
				write_value(out, item);
			}
		}
		Value::Map(map) => {
			write_header(out, major::MAP, length(map.len()));
			for (key, value) in map {
				match key {
					MapKey::Integer(integer) => write_value(out, &Value::Integer(*integer)),
					MapKey::Text(text) => write_text(out, text),
				}
				write_value(out, value);
			}
		}
		Value::Uuid(bytes) => {
			write_header(out, major::TAG, TAG_UUID);
			write_header(out, major::BYTES, length(bytes.len()));
			out.extend_from_slice(bytes);
		}
		Value::Decimal { exponent, mantissa } => {
			write_header(out, major::TAG, TAG_DECIMAL);
			write_header(out, major::ARRAY, 2);
			write_value(out, &Value::Integer(*exponent));
			write_value(out, &Value::Integer(*mantissa));
		}
		Value::Bool(false) => out.push(simple::FALSE),
		Value::Bool(true) => out.push(simple::TRUE),
		Value::Null => out.push(simple::NULL),
	}
}

fn write_text(out: &mut Vec<u8>, text: &str) {
	write_header(out, major::TEXT, length(text.len()));
	out.extend_from_slice(text.as_bytes());
}

/// Writes a header in the shortest form: the first byte, then the argument in as few
/// bytes as it needs, unless it fits in the first byte.
fn write_header(out: &mut Vec<u8>, major: u8, argument: u64) {
	out.push(initial_byte(major, argument));
	if argument < 24 {
		return;
	}
	if let Ok(argument) = u8::try_from(argument) {
		out.push(argument);
	} else if let Ok(argument) = u16::try_from(argument) {
		out.extend_from_slice(&argument.to_be_bytes());
	} else if let Ok(argument) = u32::try_from(argument) {
		out.extend_from_slice(&argument.to_be_bytes());
	} else {
		out.extend_from_slice(&argument.to_be_bytes());
	}
}
