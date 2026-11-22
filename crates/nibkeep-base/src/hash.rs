//! Content hashes that name their algorithm.

use std::fmt;
use std::io;
use std::str::FromStr;

use crate::cbor::{self, FromCbor, ToCbor, Value};

/// The ID of a change: the BLAKE3 hash of its encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChangeId(Hash);

impl ChangeId {
	/// The hash.
	#[must_use]
	pub const fn hash(&self) -> &Hash {
		&self.0
	}

	/// The ID of the change whose hash this is.
	#[must_use]
	pub const fn from_hash(hash: Hash) -> Self {
		Self(hash)
	}
}

impl fmt::Display for ChangeId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl FromStr for ChangeId {
	type Err = <Hash as std::str::FromStr>::Err;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		text.parse().map(Self)
	}
}

impl ToCbor for ChangeId {
	fn to_cbor(&self) -> Value {
		self.0.to_cbor()
	}
}

impl FromCbor for ChangeId {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		Hash::from_cbor(value).map(Self)
	}
}

/// The algorithms that produce hashes ([`Hash`](struct@Hash)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum HashAlgorithm {
	/// BLAKE3 with a 32-byte digest.
	Blake3,
}

impl HashAlgorithm {
	/// The name used in the text form of a hash, such as `blake3`.
	#[must_use]
	pub const fn name(self) -> &'static str {
		match self {
			Self::Blake3 => "blake3",
		}
	}

	/// The algorithm's code in the multihash table.
	const fn multihash_code(self) -> u64 {
		match self {
			Self::Blake3 => 0x1e,
		}
	}

	/// The length of the algorithm's digest in bytes.
	const fn digest_len(self) -> usize {
		match self {
			Self::Blake3 => 32,
		}
	}

	fn from_name(name: &str) -> Option<Self> {
		(name == Self::Blake3.name()).then_some(Self::Blake3)
	}

	fn from_multihash_code(code: u64) -> Option<Self> {
		(code == Self::Blake3.multihash_code()).then_some(Self::Blake3)
	}
}

/// The longest digest of any algorithm, in bytes.
const MAX_DIGEST_LEN: usize = 64;

/// The hash of some content, together with the algorithm that produced it.
///
/// Its text form is the algorithm's name, a colon and the digest in lowercase hexadecimal,
/// such as `blake3:af1349b9…`. In CBOR, it is a byte string holding a multihash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash {
	algorithm: HashAlgorithm,
	/// The digest, followed by zeros up to [`MAX_DIGEST_LEN`]. Keeping the unused bytes
	/// zero makes the derived comparisons depend on the algorithm and the digest only.
	digest: [u8; MAX_DIGEST_LEN],
}

impl Hash {
	/// Hashes `data` with BLAKE3.
	#[must_use]
	pub fn of(data: &[u8]) -> Self {
		Self::from_blake3(&blake3::hash(data))
	}

	/// The algorithm that produced the hash.
	#[must_use]
	pub fn algorithm(&self) -> HashAlgorithm {
		self.algorithm
	}

	/// The digest. Its length depends on the algorithm.
	#[must_use]
	pub fn digest(&self) -> &[u8] {
		&self.digest[..self.algorithm.digest_len()]
	}

	/// The binary form: a multihash, that is the algorithm's code and the digest's length
	/// as unsigned varints, followed by the digest.
	#[must_use]
	pub fn to_multihash(&self) -> Vec<u8> {
		let digest = self.digest();
		let mut out = Vec::with_capacity(2 + digest.len());
		write_varint(&mut out, self.algorithm.multihash_code());
		write_varint(&mut out, digest.len() as u64);
		out.extend_from_slice(digest);
		out
	}

	/// Reads the binary form.
	///
	/// # Errors
	///
	/// Returns an error if `bytes` is not a multihash, if its algorithm is unknown, or if
	/// the digest does not have the algorithm's length.
	pub fn from_multihash(bytes: &[u8]) -> Result<Self, ParseHashError> {
		let mut rest = bytes;
		let code = read_varint(&mut rest)?;
		let len = read_varint(&mut rest)?;
		let algorithm =
			HashAlgorithm::from_multihash_code(code).ok_or(ParseHashError::UnknownAlgorithm)?;
		if usize::try_from(len).ok() != Some(algorithm.digest_len()) {
			return Err(ParseHashError::WrongLength);
		}
		if rest.len() != algorithm.digest_len() {
			return Err(ParseHashError::Malformed);
		}
		Ok(Self::new(algorithm, rest))
	}

	fn from_blake3(hash: &blake3::Hash) -> Self {
		Self::new(HashAlgorithm::Blake3, hash.as_bytes())
	}

	/// Builds a hash from a digest that has the algorithm's length.
	fn new(algorithm: HashAlgorithm, digest: &[u8]) -> Self {
		debug_assert_eq!(digest.len(), algorithm.digest_len());
		let mut padded = [0; MAX_DIGEST_LEN];
		padded[..digest.len()].copy_from_slice(digest);
		Self {
			algorithm,
			digest: padded,
		}
	}
}

impl fmt::Display for Hash {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}:", self.algorithm.name())?;
		self.digest()
			.iter()
			.try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}

impl fmt::Debug for Hash {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Hash({self})")
	}
}

impl FromStr for Hash {
	type Err = ParseHashError;

	/// Reads the text form. Only lowercase hexadecimal is accepted, so that every hash has
	/// exactly one text form.
	fn from_str(text: &str) -> Result<Self, Self::Err> {
		let (name, hex) = text.split_once(':').ok_or(ParseHashError::Malformed)?;
		let algorithm = HashAlgorithm::from_name(name).ok_or(ParseHashError::UnknownAlgorithm)?;
		if hex.len() != 2 * algorithm.digest_len() {
			return Err(ParseHashError::WrongLength);
		}
		let mut digest = [0; MAX_DIGEST_LEN];
		for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().chunks(2)) {
			*byte = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
		}
		Ok(Self { algorithm, digest })
	}
}

impl ToCbor for Hash {
	fn to_cbor(&self) -> Value {
		Value::Bytes(self.to_multihash())
	}
}

impl FromCbor for Hash {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		match value {
			Value::Bytes(bytes) => {
				Self::from_multihash(bytes).map_err(|_| cbor::Error::Expected("a multihash"))
			}
			_ => Err(cbor::Error::Expected("a multihash")),
		}
	}
}

/// Why a hash could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseHashError {
	/// The algorithm is not one that NibKeep knows.
	#[error("unknown hash algorithm")]
	UnknownAlgorithm,
	/// The digest does not have the algorithm's length.
	#[error("digest has the wrong length for its algorithm")]
	WrongLength,
	/// The input is not a hash in the expected form.
	#[error("not a hash in the expected form")]
	Malformed,
}

/// Computes a [`Hash`](struct@Hash) incrementally, for content that arrives in pieces.
#[derive(Clone, Debug, Default)]
pub struct Hasher(blake3::Hasher);

impl Hasher {
	/// Returns a hasher that has seen no content yet.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Adds content.
	pub fn update(&mut self, data: &[u8]) -> &mut Self {
		self.0.update(data);
		self
	}

	/// Returns the hash of all the content added so far.
	#[must_use]
	pub fn finalize(&self) -> Hash {
		Hash::from_blake3(&self.0.finalize())
	}
}

impl io::Write for Hasher {
	fn write(&mut self, data: &[u8]) -> io::Result<usize> {
		self.update(data);
		Ok(data.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

fn hex_digit(digit: u8) -> Result<u8, ParseHashError> {
	match digit {
		b'0'..=b'9' => Ok(digit - b'0'),
		b'a'..=b'f' => Ok(digit - b'a' + 10),
		_ => Err(ParseHashError::Malformed),
	}
}

/// Writes an unsigned varint: seven bits per byte, least significant first, with the high
/// bit set on every byte but the last.
fn write_varint(out: &mut Vec<u8>, mut value: u64) {
	loop {
		let low = u8::try_from(value & 0x7f).expect("seven bits");
		value >>= 7;
		if value == 0 {
			out.push(low);
			return;
		}
		out.push(low | 0x80);
	}
}

/// Reads an unsigned varint in its minimal form, as the multiformats specification
/// requires, and at most nine bytes long.
fn read_varint(bytes: &mut &[u8]) -> Result<u64, ParseHashError> {
	let mut value = 0u64;
	for (index, &byte) in bytes.iter().enumerate().take(9) {
		value |= u64::from(byte & 0x7f) << (7 * index);
		if byte & 0x80 == 0 {
			if byte == 0 && index > 0 {
				return Err(ParseHashError::Malformed);
			}
			*bytes = &bytes[index + 1..];
			return Ok(value);
		}
	}
	Err(ParseHashError::Malformed)
}
