//! Canonical CBOR: a strict subset of CBOR (RFC 8949) in which every value has exactly one
//! encoding.
//!
//! [`encode`] always produces the canonical encoding, and [`decode`] accepts nothing else,
//! so `encode(&decode(bytes)?) == bytes` for every input that [`decode`] accepts. Rust
//! types take part through [`ToCbor`] and [`FromCbor`]. The rules are specified in
//! `docs/spec/encoding.md`.

mod convert;
mod decode;
mod encode;
mod value;

pub use convert::{Bytes, Error, FromCbor, ToCbor, from_slice, to_vec};
pub use decode::{DecodeError, DecodeErrorKind, MAX_DEPTH, decode, decode_first};
pub use encode::{encode, encode_into, encoded_len};
pub use value::{Integer, IntegerOutOfRange, Map, MapKey, Value};

/// The tag of a decimal fraction.
const TAG_DECIMAL: u64 = 4;
/// The tag of a UUID.
const TAG_UUID: u64 = 37;

/// The major types of CBOR. Major type 7, the simple values, has no constant: it is
/// whatever the first byte's top three bits leave over.
mod major {
	pub(super) const UNSIGNED: u8 = 0;
	pub(super) const NEGATIVE: u8 = 1;
	pub(super) const BYTES: u8 = 2;
	pub(super) const TEXT: u8 = 3;
	pub(super) const ARRAY: u8 = 4;
	pub(super) const MAP: u8 = 5;
	pub(super) const TAG: u8 = 6;
}

/// The bytes of the simple values.
mod simple {
	pub(super) const FALSE: u8 = 0xf4;
	pub(super) const TRUE: u8 = 0xf5;
	pub(super) const NULL: u8 = 0xf6;
}

/// The additional information in the first byte of a header whose argument is `argument`,
/// in the shortest form: the argument itself below 24, otherwise the size of the argument
/// that follows (24 for one byte up to 27 for eight).
fn additional_information(argument: u64) -> u8 {
	match argument {
		0..=23 => u8::try_from(argument).expect("below 24"),
		24..=0xff => 24,
		0x100..=0xffff => 25,
		0x1_0000..=0xffff_ffff => 26,
		_ => 27,
	}
}

/// The first byte of a header in the shortest form.
fn initial_byte(major: u8, argument: u64) -> u8 {
	(major << 5) | additional_information(argument)
}

/// The length of a byte string, text string, array or map as a header argument.
fn length(len: usize) -> u64 {
	u64::try_from(len).expect("lengths fit in 64 bits")
}
