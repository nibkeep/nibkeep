//! The cryptographic building blocks of NibKeep: keys, envelopes, keys derived from
//! passphrases, signatures, messages sealed to a public key, keys agreed on from a short
//! code, and recovery codes.
//!
//! The formats are specified in `docs/spec/crypto.md`. The crate does no I/O: functions
//! that need randomness take a cryptographically secure random number generator from the
//! caller.

mod blob;
mod envelope;
mod error;
mod format;
mod key;
mod page;
mod pake;
mod passphrase;
mod recovery;
mod sealed;
mod signature;

pub use blob::{BLOB_CHUNK_LEN, BlobReader, SealedBlob, seal_blob};
pub use envelope::Envelope;
pub use error::CryptoError;
pub use key::{Key, key_hash};
pub use page::{PAGE_NONCE_LEN, PAGE_TAG_LEN, PageCipher, RECORD_OVERHEAD};
pub use pake::{Pake, PakeSide};
pub use passphrase::PassphraseParams;
pub use rand_core;
pub use recovery::{NotARecoveryCode, RecoveryCode};
pub use sealed::{DecryptionKey, EncryptionKey, SealedMessage};
pub use signature::{Signature, SigningKey, VerifyingKey};
pub use zeroize::Zeroizing;
