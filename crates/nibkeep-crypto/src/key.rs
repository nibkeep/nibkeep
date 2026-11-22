//! Keys for encrypting envelopes.

use std::convert::Infallible;
use std::fmt;

use nibkeep_base::Hash;
use rand_core::CryptoRng;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The context of the key, derived from a change key or a record key, whose hash a
/// change's header holds.
const KEY_HASHES_CONTEXT: &str = "nibkeep 2026-10 key hashes";

/// The hash of a change key or a record key that a change's header holds, so that the key
/// a change was sealed with is the only one its replicas take: the hash of a key derived
/// from it, which tells nothing of the key itself. A space key's hash, which the statements
/// that give it name, is computed in the same way.
#[must_use]
pub fn key_hash(key: &Key) -> Hash {
	Hash::of(key.derive(KEY_HASHES_CONTEXT).expose_bytes())
}

/// A 256-bit key for encrypting envelopes, such as a space key or the key of a single
/// record.
///
/// The key is wiped from memory when it is dropped, and it is never printed.
#[derive(Clone)]
pub struct Key([u8; 32]);

impl Key {
	/// Returns a new random key.
	#[must_use]
	pub fn generate(rng: &mut (impl CryptoRng + ?Sized)) -> Self {
		// Filling the key in place leaves no copy of its bytes behind.
		let mut key = Self([0; 32]);
		rng.fill_bytes(&mut key.0);
		key
	}

	/// Returns the key with these bytes, such as a key read from storage. The caller should
	/// wipe its own copy of the bytes.
	#[must_use]
	pub const fn from_bytes(bytes: [u8; 32]) -> Self {
		Self(bytes)
	}

	/// Returns the key with these bytes, copied straight into the key so that no other copy
	/// is left behind, or `None` if there are not 32 of them.
	#[must_use]
	pub fn from_slice(bytes: &[u8]) -> Option<Self> {
		let bytes: &[u8; 32] = bytes.try_into().ok()?;
		let Ok(key) = Self::fill_with(|key| {
			key.copy_from_slice(bytes);
			Ok::<(), Infallible>(())
		});
		Some(key)
	}

	/// Whether two keys are the same, compared in constant time.
	#[must_use]
	pub fn same_as(&self, other: &Self) -> bool {
		// blake3::Hash compares in constant time.
		blake3::Hash::from_bytes(self.0) == blake3::Hash::from_bytes(other.0)
	}

	/// The key's bytes, for example to store it. Handle them with care.
	#[must_use]
	pub const fn expose_bytes(&self) -> &[u8; 32] {
		&self.0
	}

	/// Derives the key for one purpose, named by `context`, with BLAKE3 in key derivation
	/// mode. Contexts are fixed strings such as `nibkeep 2026-10 record keys`.
	#[must_use]
	pub fn derive(&self, context: &'static str) -> Self {
		Self(blake3::derive_key(context, &self.0))
	}

	/// A tag of `message`, which only holders of this key can make: BLAKE3 in keyed mode.
	/// A device tags what it stores with a key of its own, so that what it reads back is
	/// known to be what it wrote. The key is one derived for this purpose alone.
	#[must_use]
	pub fn tag(&self, message: &[u8]) -> [u8; 32] {
		*blake3::keyed_hash(&self.0, message).as_bytes()
	}

	/// Whether `tag` is this key's tag of `message`, compared in constant time.
	#[must_use]
	pub fn verify_tag(&self, message: &[u8], tag: &[u8]) -> bool {
		let Ok(tag) = <[u8; 32]>::try_from(tag) else {
			return false;
		};
		// blake3::Hash compares in constant time.
		blake3::keyed_hash(&self.0, message) == blake3::Hash::from_bytes(tag)
	}

	/// Returns a key whose bytes `fill` writes in place, so that no copy of them is left
	/// behind. If `fill` fails, the partly written key is wiped.
	pub(crate) fn fill_with<E>(
		fill: impl FnOnce(&mut [u8; 32]) -> Result<(), E>,
	) -> Result<Self, E> {
		let mut key = Self([0; 32]);
		fill(&mut key.0)?;
		Ok(key)
	}
}

impl Drop for Key {
	fn drop(&mut self) {
		self.0.zeroize();
	}
}

impl ZeroizeOnDrop for Key {}

impl fmt::Debug for Key {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("Key(..)")
	}
}
