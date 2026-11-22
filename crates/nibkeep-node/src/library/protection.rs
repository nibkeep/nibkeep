//! Protecting a library's keys and its secrets anew, after they were set up: with a new
//! passphrase, or with a new key in the platform's keystore
//! ([Key file](../../../../docs/spec/keys.md)). The library's passphrase and its secrets'
//! stay apart, so that unlocking the library never unlocks its secrets.

use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_keys::{KeysError, NewProtection, SealedSecrets};

use super::Library;
use super::secrets::Compartment;
use crate::NodeError;

impl Library {
	/// Protects the library's keys another way: with a new passphrase, or with a new key in
	/// the platform's keystore. The keystore key of the old protection, if it had one, is
	/// deleted once the key file is written. The library stays open, and closes and opens
	/// again with the new protection.
	///
	/// # Errors
	///
	/// Returns [`NodeError::LibraryPassphrase`] if the passphrase unlocks the library's
	/// secrets; an error if the passphrase cannot be used or the keystore fails, or if the
	/// key file cannot be read or written, in which case nothing changed.
	pub fn protect(
		&mut self,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.opens_the_other(protection, Compartment::Secrets)? {
			return Err(NodeError::LibraryPassphrase);
		}
		let compartment = self.compartment.clone();
		let mut protected = None;
		self.update_key_file(|key_file| {
			protected = Some(key_file.protect_replica(&compartment, protection, rng)?);
			Ok(())
		})?;
		if let Some(compartment) = protected {
			self.compartment = compartment;
		}
		Ok(())
	}

	/// Protects the library's secrets another way: with a new passphrase, or with a new key
	/// in the platform's keystore, while they are unlocked, which they stay. They are sealed
	/// before the key file is locked, as deriving a key from a passphrase, or a keystore's
	/// prompt, takes a while; secrets keys that another program gave meanwhile are kept. The
	/// keystore key of the old protection, if it had one, is deleted once the key file is
	/// written.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsLocked`] if the secrets are locked; an error of the keys,
	/// [`KeysError::NoSecrets`], if there are none; [`NodeError::LibraryPassphrase`] if the
	/// passphrase is the library's; an error if this replica is an AI agent's; an error if
	/// the passphrase cannot be used or the keystore fails, or if the key file cannot be read
	/// or written, in which case nothing changed.
	pub fn protect_secrets(
		&mut self,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		let Some(secrets) = self.secrets_compartment.clone() else {
			return Err(if self.secrets_protection()?.is_some() {
				NodeError::SecretsLocked
			} else {
				NodeError::Keys(KeysError::NoSecrets)
			});
		};
		if self.opens_the_other(protection, Compartment::Replica)? {
			return Err(NodeError::LibraryPassphrase);
		}
		let keys = self.key_file_on_disk()?.secrets(&secrets)?;
		let (mut sealed, protected) = SealedSecrets::seal(&keys, protection, rng)?;
		self.update_key_file(|key_file| {
			// The secrets keys as the file on the disk holds them now.
			let keys = key_file.secrets(&secrets)?;
			sealed.update(&keys, &protected, rng)?;
			key_file.put_secrets(sealed);
			Ok(())
		})?;
		self.secrets_compartment = Some(protected);
		Ok(())
	}

	/// Forgets the secrets compartment, whose key the keystore lost, as Android destroys
	/// keys that need the person's confirmation once the screen lock is removed: it opens
	/// never again. The library can then ask another replica for the secrets, as a new one
	/// does; where none holds them, they are lost, and new ones can be set up.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsKeyKept`] if the secrets can still be unlocked: the
	/// keystore has their key, or a passphrase protects them; an error of the keys,
	/// [`KeysError::NoSecrets`], if there are none; an error if the keystore fails, or the
	/// key file cannot be read or written, in which case nothing changed.
	pub fn forget_lost_secrets(&mut self) -> Result<(), NodeError> {
		self.update_key_file(|key_file| {
			if key_file.secrets_protection().is_none() {
				return Err(NodeError::Keys(KeysError::NoSecrets));
			}
			if !key_file.secrets_key_lost()? {
				return Err(NodeError::SecretsKeyKept);
			}
			key_file.remove_secrets();
			Ok(())
		})?;
		self.lock_secrets();
		Ok(())
	}
}
