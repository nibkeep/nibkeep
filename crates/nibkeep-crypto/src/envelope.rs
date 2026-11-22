//! Envelopes: data encrypted with a key.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::format::{self, TAG_LEN};
use crate::{CryptoError, Key};

const NONCE_LEN: usize = 24;

/// Data encrypted with a [`Key`], with XChaCha20-Poly1305 and a random nonce.
///
/// Opening an envelope needs the key and the same associated data that it was sealed
/// with. In CBOR, an envelope is the array `[1, nonce, ciphertext]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
	nonce: [u8; NONCE_LEN],
	ciphertext: Vec<u8>,
}

impl Envelope {
	/// Encrypts `plaintext` with `key`, and authenticates it together with
	/// `associated_data`, which is not stored in the envelope.
	///
	/// # Panics
	///
	/// Panics if `plaintext` is longer than XChaCha20-Poly1305 allows, about 256 GiB.
	#[must_use]
	pub fn seal(
		key: &Key,
		plaintext: &[u8],
		associated_data: &[u8],
		rng: &mut impl CryptoRng,
	) -> Self {
		let mut nonce = [0; NONCE_LEN];
		rng.fill_bytes(&mut nonce);
		let payload = Payload {
			msg: plaintext,
			aad: associated_data,
		};
		let ciphertext = cipher(key)
			.encrypt(&XNonce::from(nonce), payload)
			.expect("the plaintext fits in an envelope");
		Self { nonce, ciphertext }
	}

	/// Decrypts the envelope.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Decryption`] if the key or the associated data is not the one
	/// the envelope was sealed with, or if the envelope was changed.
	pub fn open(
		&self,
		key: &Key,
		associated_data: &[u8],
	) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
		let payload = Payload {
			msg: &self.ciphertext,
			aad: associated_data,
		};
		cipher(key)
			.decrypt(&XNonce::from(self.nonce), payload)
			.map(Zeroizing::new)
			.map_err(|_| CryptoError::Decryption)
	}

	/// Wraps `key` with `wrapping_key`: seals an envelope whose plaintext is the key.
	#[must_use]
	pub fn seal_key(
		wrapping_key: &Key,
		key: &Key,
		associated_data: &[u8],
		rng: &mut impl CryptoRng,
	) -> Self {
		Self::seal(wrapping_key, key.expose_bytes(), associated_data, rng)
	}

	/// Unwraps a key wrapped with [`Envelope::seal_key`].
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Decryption`] if the envelope does not open, or does not hold
	/// a key.
	pub fn open_key(&self, wrapping_key: &Key, associated_data: &[u8]) -> Result<Key, CryptoError> {
		key_from_plaintext(&self.open(wrapping_key, associated_data)?)
	}
}

impl Envelope {
	/// How many bytes it holds: its nonce and its ciphertext.
	#[must_use]
	pub fn byte_len(&self) -> usize {
		NONCE_LEN + self.ciphertext.len()
	}
}

impl ToCbor for Envelope {
	fn to_cbor(&self) -> Value {
		format::write_bytes(&[&self.nonce, &self.ciphertext])
	}
}

impl FromCbor for Envelope {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "an envelope: [1, nonce, ciphertext]";
		let [nonce, ciphertext] = format::read_bytes(value, EXPECTED)?;
		let nonce = nonce
			.try_into()
			.map_err(|_| cbor::Error::Expected(EXPECTED))?;
		if ciphertext.len() < TAG_LEN {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		Ok(Self {
			nonce,
			ciphertext: ciphertext.to_vec(),
		})
	}
}

/// Reads a key from decrypted bytes, which must be exactly the 32 bytes of a key.
pub(crate) fn key_from_plaintext(plaintext: &[u8]) -> Result<Key, CryptoError> {
	let plaintext: &[u8; 32] = plaintext.try_into().map_err(|_| CryptoError::Decryption)?;
	Key::fill_with(|bytes| {
		bytes.copy_from_slice(plaintext);
		Ok(())
	})
}

fn cipher(key: &Key) -> XChaCha20Poly1305 {
	XChaCha20Poly1305::new(key.expose_bytes().into())
}
