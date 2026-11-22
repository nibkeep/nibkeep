//! Why cryptographic operations fail.

/// Why a cryptographic operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CryptoError {
	/// The data could not be decrypted: the key is wrong, or the data or its associated
	/// data were changed.
	#[error("cannot decrypt: the key is wrong, or the data was changed")]
	Decryption,
	/// The signature does not match the verifying key, the context and the message.
	#[error("the signature is not valid")]
	InvalidSignature,
	/// A key cannot be derived from a passphrase with these inputs.
	#[error("cannot derive a key from the passphrase: {0}")]
	Passphrase(&'static str),
	/// There is not enough memory to derive a key with these passphrase parameters.
	/// Parameters that need less, such as
	/// [`PassphraseParams::LOW_MEMORY`](crate::PassphraseParams::LOW_MEMORY), may work.
	#[error("not enough memory to derive the key from the passphrase")]
	OutOfMemory,
	/// A message of SPAKE2 is not one of the other side's.
	#[error("the message of the key exchange is not valid")]
	Pake,
}
