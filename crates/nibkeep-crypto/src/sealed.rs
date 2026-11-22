//! Messages sealed to a public key with HPKE, using the post-quantum hybrid X-Wing.

use std::fmt;

use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::XWing;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::envelope::key_from_plaintext;
use crate::format::{self, Hex, TAG_LEN};
use crate::{CryptoError, Key};

type PrivateKey = <XWing as Kem>::PrivateKey;
type PublicKey = <XWing as Kem>::PublicKey;
type EncappedKey = <XWing as Kem>::EncappedKey;

/// A key that opens the messages sealed to its [`EncryptionKey`]: a 32-byte X-Wing seed.
/// It is wiped from memory when it is dropped, and never printed.
#[derive(Clone)]
pub struct DecryptionKey(PrivateKey);

impl DecryptionKey {
	/// Returns a new random decryption key.
	#[must_use]
	pub fn generate(rng: &mut impl CryptoRng) -> Self {
		let mut seed = Zeroizing::new([0; 32]);
		rng.fill_bytes(&mut seed[..]);
		Self::from_seed(&seed)
	}

	/// Returns the decryption key with this seed, such as one read from storage or from a
	/// recovery code.
	#[must_use]
	pub fn from_seed(seed: &[u8; 32]) -> Self {
		let Ok(key) = PrivateKey::from_bytes(seed) else {
			unreachable!("every 32 bytes are an X-Wing seed");
		};
		Self(key)
	}

	/// The key's seed, for example to store it. Handle it with care.
	#[must_use]
	pub fn expose_seed(&self) -> Zeroizing<[u8; 32]> {
		// Writing into the buffer that wipes itself leaves no other copy behind.
		let mut seed = Zeroizing::new([0; 32]);
		self.0.write_exact(&mut seed[..]);
		seed
	}

	/// The public key that messages for this key are sealed to.
	#[must_use]
	pub fn encryption_key(&self) -> EncryptionKey {
		EncryptionKey(XWing::sk_to_pk(&self.0))
	}
}

impl fmt::Debug for DecryptionKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("DecryptionKey(..)")
	}
}

/// A public key that messages are sealed to, belonging to a [`DecryptionKey`].
///
/// In CBOR, an encryption key is the array `[1, key]`, with the 1216-byte X-Wing public
/// key.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptionKey(PublicKey);

impl EncryptionKey {
	/// The 1216 bytes of the X-Wing public key.
	#[must_use]
	pub fn to_bytes(&self) -> Vec<u8> {
		self.0.to_bytes().to_vec()
	}

	/// The key with these bytes, or `None` if they are not an X-Wing public key.
	#[must_use]
	pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
		PublicKey::from_bytes(bytes).ok().map(Self)
	}
}

impl fmt::Debug for EncryptionKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "EncryptionKey({}..)", Hex(&self.0.to_bytes()[..8]))
	}
}

impl ToCbor for EncryptionKey {
	fn to_cbor(&self) -> Value {
		format::write_bytes(&[&self.0.to_bytes()])
	}
}

impl FromCbor for EncryptionKey {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "an encryption key: [1, X-Wing public key]";
		let [bytes] = format::read_bytes(value, EXPECTED)?;
		PublicKey::from_bytes(bytes)
			.map(Self)
			.map_err(|_| cbor::Error::Expected(EXPECTED))
	}
}

/// A message sealed to an [`EncryptionKey`] with HPKE in base mode, using X-Wing,
/// HKDF-SHA256 and ChaCha20-Poly1305.
///
/// Opening it needs the [`DecryptionKey`], and the same context and associated data that it
/// was sealed with. In CBOR, a sealed message is the array `[1, enc, ciphertext]`, with
/// the encapsulated key and the ciphertext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedMessage {
	encapsulated_key: Vec<u8>,
	ciphertext: Vec<u8>,
}

impl SealedMessage {
	/// Seals `plaintext` to `recipient`. The `context` names the purpose, such as
	/// `nibkeep invitation`, and becomes HPKE's `info`; the `associated_data` is
	/// authenticated but not stored in the message.
	///
	/// # Panics
	///
	/// Panics if `plaintext` is longer than ChaCha20-Poly1305 allows, about 256 GiB.
	#[must_use]
	pub fn seal(
		recipient: &EncryptionKey,
		plaintext: &[u8],
		context: &'static str,
		associated_data: &[u8],
		rng: &mut impl CryptoRng,
	) -> Self {
		let (encapsulated_key, ciphertext) =
			hpke::single_shot_seal_with_rng::<ChaCha20Poly1305, HkdfSha256, XWing>(
				&OpModeS::Base,
				&recipient.0,
				context.as_bytes(),
				plaintext,
				associated_data,
				rng,
			)
			.expect("the plaintext fits in a sealed message");
		Self {
			encapsulated_key: encapsulated_key.to_bytes().to_vec(),
			ciphertext,
		}
	}

	/// Opens a message sealed to the encryption key of `recipient`.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Decryption`] if the message was sealed to another key or with
	/// another context or associated data, or if it was changed.
	pub fn open(
		&self,
		recipient: &DecryptionKey,
		context: &'static str,
		associated_data: &[u8],
	) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
		let encapsulated_key =
			EncappedKey::from_bytes(&self.encapsulated_key).map_err(|_| CryptoError::Decryption)?;
		hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, XWing>(
			&OpModeR::Base,
			&recipient.0,
			&encapsulated_key,
			context.as_bytes(),
			&self.ciphertext,
			associated_data,
		)
		.map(Zeroizing::new)
		.map_err(|_| CryptoError::Decryption)
	}

	/// Seals `key` to `recipient`: a sealed message whose plaintext is the key.
	#[must_use]
	pub fn seal_key(
		recipient: &EncryptionKey,
		key: &Key,
		context: &'static str,
		associated_data: &[u8],
		rng: &mut impl CryptoRng,
	) -> Self {
		Self::seal(recipient, key.expose_bytes(), context, associated_data, rng)
	}

	/// Opens a key sealed with [`SealedMessage::seal_key`].
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Decryption`] if the message does not open, or does not hold a
	/// key.
	pub fn open_key(
		&self,
		recipient: &DecryptionKey,
		context: &'static str,
		associated_data: &[u8],
	) -> Result<Key, CryptoError> {
		key_from_plaintext(&self.open(recipient, context, associated_data)?)
	}
}

impl ToCbor for SealedMessage {
	fn to_cbor(&self) -> Value {
		format::write_bytes(&[&self.encapsulated_key, &self.ciphertext])
	}
}

impl FromCbor for SealedMessage {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a sealed message: [1, encapsulated key, ciphertext]";
		let [encapsulated_key, ciphertext] = format::read_bytes(value, EXPECTED)?;
		if EncappedKey::from_bytes(encapsulated_key).is_err() || ciphertext.len() < TAG_LEN {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		Ok(Self {
			encapsulated_key: encapsulated_key.to_vec(),
			ciphertext: ciphertext.to_vec(),
		})
	}
}
