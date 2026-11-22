//! Records encrypted in place: the pages of the state file, and the records of its
//! write-ahead log.

use aes_gcm::aead::{AeadInOut, KeyInit};
use rand_core::CryptoRng;
use xaes_256_gcm::Xaes256Gcm;

use crate::{CryptoError, Key};

/// The length of a record's nonce.
pub const PAGE_NONCE_LEN: usize = 24;

/// The length of a record's tag.
pub const PAGE_TAG_LEN: usize = 16;

/// How much longer a sealed record is than what it holds: its nonce before, and its tag
/// after.
pub const RECORD_OVERHEAD: usize = PAGE_NONCE_LEN + PAGE_TAG_LEN;

/// Encrypts records in place with XAES-256-GCM, each under a random nonce of its own: the
/// pages of the state file. A sealed record is its nonce, its ciphertext, and its tag, and
/// is as long as the record that held room for them.
///
/// XAES-256-GCM is AES-256-GCM with 24-byte nonces, which can be chosen at random for as
/// many records as a key will ever seal. It runs on the AES instructions of the processors
/// that NibKeep runs on.
pub struct PageCipher(Xaes256Gcm);

impl PageCipher {
	/// The cipher of `key`, a key of one state file.
	#[must_use]
	pub fn new(key: &Key) -> Self {
		Self(Xaes256Gcm::new(key.expose_bytes().into()))
	}

	/// Seals `record` in place. The record holds room for a nonce, then what it is to hold,
	/// then room for a tag; it is authenticated together with `associated_data`, which is
	/// not stored in it.
	///
	/// # Panics
	///
	/// Panics if `record` is shorter than [`RECORD_OVERHEAD`].
	pub fn seal(
		&self,
		rng: &mut (impl CryptoRng + ?Sized),
		associated_data: &[u8],
		record: &mut [u8],
	) {
		let (nonce, rest) = record
			.split_first_chunk_mut::<PAGE_NONCE_LEN>()
			.expect("room for a nonce");
		let (data, tag) = rest
			.split_last_chunk_mut::<PAGE_TAG_LEN>()
			.expect("room for a tag");
		rng.fill_bytes(nonce);
		let sealed_tag = self
			.0
			.encrypt_inout_detached((&*nonce).into(), associated_data, data.into())
			.expect("a record fits");
		tag.copy_from_slice(&sealed_tag);
	}

	/// Opens a sealed `record` in place, and returns what it holds.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Decryption`] if the record was not sealed with this key and
	/// this associated data, or was changed since, or is too short to be a record.
	pub fn open<'a>(
		&self,
		associated_data: &[u8],
		record: &'a mut [u8],
	) -> Result<&'a [u8], CryptoError> {
		let Some((nonce, rest)) = record.split_first_chunk_mut::<PAGE_NONCE_LEN>() else {
			return Err(CryptoError::Decryption);
		};
		let Some((data, tag)) = rest.split_last_chunk_mut::<PAGE_TAG_LEN>() else {
			return Err(CryptoError::Decryption);
		};
		match self.0.decrypt_inout_detached(
			(&*nonce).into(),
			associated_data,
			(&mut *data).into(),
			(&*tag).into(),
		) {
			Ok(()) => Ok(data),
			Err(_) => Err(CryptoError::Decryption),
		}
	}
}
