//! Why keys could not be unlocked or saved.

use nibkeep_crypto::CryptoError;

/// Why keys could not be unlocked or saved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeysError {
	/// The passphrase does not open the compartment, or the file was changed.
	#[error("the passphrase is wrong, or the key file was changed")]
	WrongPassphrase,
	/// The keystore's key does not open the compartment, or the file was changed.
	#[error("the key in the keystore does not open the key file")]
	WrongKey,
	/// The compartment is protected another way than the unlocker offers.
	#[error("the key file is protected another way")]
	WrongUnlocker,
	/// The key file has no secrets compartment.
	#[error("the key file holds no secrets keys")]
	NoSecrets,
	/// The platform's keystore failed or does not have the key.
	#[error("the keystore failed: {0}")]
	Keystore(#[from] keyring_core::Error),
	/// The passphrase cannot be used.
	#[error(transparent)]
	Crypto(#[from] CryptoError),
	/// The key file is not well-formed.
	#[error("the key file is damaged: {0}")]
	Damaged(String),
	/// The file system failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),
}
