//! Signatures with Ed25519, over a context and a message.

use std::cell::RefCell;
use std::fmt;

use ed25519_dalek::Signer;
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::CryptoError;
use crate::format::{self, Hex};

/// A key that signs, kept secret by the device that made it. It is wiped from memory when
/// it is dropped, and never printed.
#[derive(Clone)]
pub struct SigningKey(ed25519_dalek::SigningKey);

impl SigningKey {
	/// Returns a new random signing key.
	#[must_use]
	pub fn generate(rng: &mut impl CryptoRng) -> Self {
		let mut bytes = Zeroizing::new([0; 32]);
		rng.fill_bytes(&mut bytes[..]);
		Self::from_bytes(&bytes)
	}

	/// Returns the signing key with these bytes, the secret key of RFC 8032.
	#[must_use]
	pub fn from_bytes(bytes: &[u8; 32]) -> Self {
		Self(ed25519_dalek::SigningKey::from_bytes(bytes))
	}

	/// The key's bytes, for example to store it. Handle them with care.
	#[must_use]
	pub fn expose_bytes(&self) -> &[u8; 32] {
		self.0.as_bytes()
	}

	/// The key that checks this key's signatures.
	#[must_use]
	pub fn verifying_key(&self) -> VerifyingKey {
		VerifyingKey(self.0.verifying_key())
	}

	/// Signs `message` for the purpose named by `context`, a fixed string such as
	/// `nibkeep change`. The signature covers the canonical CBOR encoding of the array
	/// `[context, message]`, so it cannot be passed off as one made for another purpose.
	#[must_use]
	pub fn sign(&self, context: &'static str, message: &[u8]) -> Signature {
		Signature(self.0.sign(&signed_bytes(context, message)).to_bytes())
	}
}

impl fmt::Debug for SigningKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("SigningKey")
			.field("verifying_key", &self.verifying_key())
			.finish_non_exhaustive()
	}
}

/// A key that checks the signatures of a [`SigningKey`], and can be shared with anyone.
///
/// In CBOR, a verifying key is the array `[1, key]`, with the 32-byte public key of
/// RFC 8032. Keys of small order are rejected.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VerifyingKey(ed25519_dalek::VerifyingKey);

impl VerifyingKey {
	/// The 32-byte public key of RFC 8032.
	#[must_use]
	pub fn as_bytes(&self) -> &[u8; 32] {
		self.0.as_bytes()
	}

	/// The key with these 32 bytes, or `None` if they are not a key that NibKeep accepts:
	/// points of small order and encodings that are not canonical are refused.
	#[must_use]
	pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
		Self::decode(bytes)
	}

	/// The key's text form, as the records of replicas hold it: its 32 bytes in base64url
	/// without padding.
	#[must_use]
	pub fn to_text(&self) -> String {
		<base64ct::Base64UrlUnpadded as base64ct::Encoding>::encode_string(self.as_bytes())
	}

	/// The key whose text form this is, or `None` if the text is not one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes: [u8; 32] = <base64ct::Base64UrlUnpadded as base64ct::Encoding>::decode_vec(text)
			.ok()?
			.try_into()
			.ok()?;
		Self::from_bytes(&bytes)
	}

	/// Checks that `signature` signs `message` for the purpose named by `context`.
	///
	/// Verification is strict, so that every implementation accepts the same signatures:
	/// it rejects signatures whose scalar is not reduced or whose point is of small order,
	/// and checks the equation without the cofactor.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::InvalidSignature`] if the signature is not valid.
	pub fn verify(
		&self,
		context: &'static str,
		message: &[u8],
		signature: &Signature,
	) -> Result<(), CryptoError> {
		let signature = ed25519_dalek::Signature::from_bytes(&signature.0);
		self.0
			.verify_strict(&signed_bytes(context, message), &signature)
			.map_err(|_| CryptoError::InvalidSignature)
	}
}

impl fmt::Debug for VerifyingKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "VerifyingKey({})", Hex(self.as_bytes()))
	}
}

impl ToCbor for VerifyingKey {
	fn to_cbor(&self) -> Value {
		format::write_bytes(&[self.as_bytes()])
	}
}

impl FromCbor for VerifyingKey {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a verifying key: [1, Ed25519 public key]";
		let error = || cbor::Error::Expected(EXPECTED);
		let [bytes] = format::read_bytes(value, EXPECTED)?;
		let bytes: &[u8; 32] = bytes.try_into().map_err(|_| error())?;
		VerifyingKey::decode(bytes).ok_or_else(error)
	}
}

/// How many verifying keys each thread remembers having decoded.
const DECODED_KEYS: usize = 16;

thread_local! {
	/// Keys decoded lately, the one used last at the end. A log holds the keys of a few
	/// devices again and again, and decoding one takes two inversions on the curve, which
	/// would take a large share of the time to open a space.
	static DECODED: RefCell<Vec<([u8; 32], VerifyingKey)>> =
		const { RefCell::new(Vec::new()) };
}

impl VerifyingKey {
	/// Decodes a public key, or `None` if it is not one that NibKeep accepts. Like strict
	/// implementations such as libsodium, it rejects points of small order and encodings
	/// that are not canonical, so that every implementation accepts the same keys and every
	/// key has one encoding. Only keys that are accepted are remembered.
	fn decode(bytes: &[u8; 32]) -> Option<Self> {
		// While the thread ends, what it remembered may be gone already: then the key is
		// decoded in full.
		let remembered = DECODED
			.try_with(|decoded| {
				let mut decoded = decoded.borrow_mut();
				let position = decoded.iter().position(|(known, _)| known == bytes)?;
				let entry = decoded.remove(position);
				decoded.push(entry);
				Some(entry.1)
			})
			.ok()
			.flatten();
		if remembered.is_some() {
			return remembered;
		}
		let key = Self::decode_in_full(bytes)?;
		// Nothing is lost when the thread can no longer remember.
		let _ = DECODED.try_with(|decoded| {
			let mut decoded = decoded.borrow_mut();
			if decoded.len() == DECODED_KEYS {
				decoded.remove(0);
			}
			decoded.push((*bytes, key));
		});
		Some(key)
	}

	fn decode_in_full(bytes: &[u8; 32]) -> Option<Self> {
		let key = ed25519_dalek::VerifyingKey::from_bytes(bytes).ok()?;
		if key.is_weak() || key.to_edwards().compress().as_bytes() != bytes {
			return None;
		}
		Some(Self(key))
	}
}

/// A signature made by a [`SigningKey`].
///
/// In CBOR, a signature is the array `[1, signature]`, with the 64-byte Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; 64]);

impl Signature {
	/// The 64-byte Ed25519 signature.
	#[must_use]
	pub const fn as_bytes(&self) -> &[u8; 64] {
		&self.0
	}
}

impl fmt::Debug for Signature {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Signature({})", Hex(&self.0))
	}
}

impl ToCbor for Signature {
	fn to_cbor(&self) -> Value {
		format::write_bytes(&[&self.0])
	}
}

impl FromCbor for Signature {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a signature: [1, Ed25519 signature]";
		let [bytes] = format::read_bytes(value, EXPECTED)?;
		bytes
			.try_into()
			.map(Self)
			.map_err(|_| cbor::Error::Expected(EXPECTED))
	}
}

/// The bytes a signature covers: the canonical CBOR encoding of `[context, message]`.
fn signed_bytes(context: &str, message: &[u8]) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![
		Value::Text(context.to_owned()),
		Value::Bytes(message.to_vec()),
	]))
}
