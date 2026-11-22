//! The key file: two compartments, each opened by a passphrase or the platform's keystore.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Envelope, Key, PassphraseParams};
use zeroize::Zeroizing;

use crate::{KeysError, ReplicaKeys, SecretsKeys};

/// The suite of the key file format in version 1 of the specification.
const SUITE: u8 = 1;

/// The name of the replica compartment, in its associated data.
const REPLICA: &str = "nibkeep key file: replica";

/// The name of the secrets compartment, in its associated data.
const SECRETS: &str = "nibkeep key file: secrets";

/// Counts writes, so that concurrent writes use temporary files of their own.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// How a compartment's key is found.
#[derive(Clone, PartialEq, Eq)]
pub enum Protection {
	/// Derived from a passphrase with Argon2id.
	Passphrase {
		/// The cost of the derivation.
		params: PassphraseParams,
		/// Random bytes, new for every compartment key.
		salt: [u8; 16],
	},
	/// Kept in the platform's keystore, under a service and an account name. Every
	/// compartment key has an account name of its own.
	Keystore {
		/// The service name.
		service: String,
		/// The account name.
		account: String,
	},
}

impl fmt::Debug for Protection {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Passphrase { params, .. } => f
				.debug_struct("Passphrase")
				.field("params", params)
				.finish_non_exhaustive(),
			Self::Keystore { service, account } => f
				.debug_struct("Keystore")
				.field("service", service)
				.field("account", account)
				.finish(),
		}
	}
}

/// How to protect a compartment that is being written.
#[derive(Clone, Copy)]
pub enum NewProtection<'a> {
	/// With a passphrase, at this cost.
	Passphrase(&'a str, PassphraseParams),
	/// With a new random key, kept in the platform's keystore under this service name and
	/// an account name of its own.
	Keystore {
		/// The service name.
		service: &'a str,
	},
}

impl fmt::Debug for NewProtection<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Passphrase(_, params) => f.debug_tuple("Passphrase").field(params).finish(),
			Self::Keystore { service } => f
				.debug_struct("Keystore")
				.field("service", service)
				.finish(),
		}
	}
}

/// What unlocks a compartment.
#[derive(Clone, Copy)]
pub enum Unlocker<'a> {
	/// The passphrase.
	Passphrase(&'a str),
	/// The key in the platform's keystore, which the platform may release only after its
	/// own confirmation.
	Keystore,
}

impl fmt::Debug for Unlocker<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			Self::Passphrase(_) => "Passphrase(..)",
			Self::Keystore => "Keystore",
		})
	}
}

/// The key of an unlocked compartment: kept while the compartment stays unlocked, so that
/// changing its keys needs no new passphrase or confirmation. It is wiped when dropped and
/// never printed.
#[derive(Clone)]
pub struct CompartmentKey(Key);

impl fmt::Debug for CompartmentKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("CompartmentKey(..)")
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Compartment {
	protection: Protection,
	envelope: Envelope,
}

/// A secrets compartment sealed apart from any key file, as deriving a key from a passphrase,
/// or a keystore's prompt, takes a while, and put in the key file as it is on the disk
/// afterwards, with [`KeyFile::put_secrets`]. One dropped without being put deletes its
/// keystore key, which no file will need.
#[derive(Debug)]
pub struct SealedSecrets(Option<Compartment>);

impl SealedSecrets {
	/// Seals `keys` in a new secrets compartment, protected as `protection` says. Returns
	/// it, and its key.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase cannot be used or the keystore fails, as when the
	/// person turns its prompt down.
	pub fn seal(
		keys: &SecretsKeys,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(Self, CompartmentKey), KeysError> {
		let (compartment, key) = Compartment::seal(&keys.encode(), SECRETS, protection, rng)?;
		Ok((Self(Some(compartment)), key))
	}

	/// Replaces the keys it holds, with its key, which asks nothing again.
	///
	/// # Errors
	///
	/// Returns an error if `key` is not its key.
	pub fn update(
		&mut self,
		keys: &SecretsKeys,
		key: &CompartmentKey,
		rng: &mut impl CryptoRng,
	) -> Result<(), KeysError> {
		self.0
			.as_mut()
			.ok_or(KeysError::NoSecrets)?
			.reseal(&keys.encode(), SECRETS, key, rng)
	}

	/// How it is protected.
	#[must_use]
	pub fn protection(&self) -> Option<&Protection> {
		self.0.as_ref().map(|compartment| &compartment.protection)
	}
}

impl Drop for SealedSecrets {
	fn drop(&mut self) {
		if let Some(Compartment {
			protection: Protection::Keystore { service, account },
			..
		}) = self.0.take()
		{
			let _ = keyring_core::Entry::new(&service, &account)
				.and_then(|entry| entry.delete_credential());
		}
	}
}

/// The key file of a replica: the replica compartment, with the replica's keys and the
/// space keys, and, on replicas that hold secrets, the secrets compartment. The two open
/// separately.
///
/// In CBOR, a key file is the array `[1, replica]` or `[1, replica, secrets]`.
/// `docs/spec/keys.md` describes the format.
#[derive(Clone, Debug)]
pub struct KeyFile {
	replica: Compartment,
	secrets: Option<Compartment>,
	/// Keystore entries of replaced compartments, deleted once the file that no longer
	/// needs them is written.
	obsolete: Vec<(String, String)>,
}

impl PartialEq for KeyFile {
	fn eq(&self, other: &Self) -> bool {
		self.replica == other.replica && self.secrets == other.secrets
	}
}

impl Eq for KeyFile {}

impl KeyFile {
	/// Makes a key file with the replica's keys, protected as `protection` says. Returns the
	/// file and the key of its replica compartment, which is unlocked.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase cannot be used or the keystore fails.
	pub fn create(
		replica: &ReplicaKeys,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(Self, CompartmentKey), KeysError> {
		let (compartment, key) = Compartment::seal(&replica.encode(), REPLICA, protection, rng)?;
		let file = Self {
			replica: compartment,
			secrets: None,
			obsolete: Vec::new(),
		};
		Ok((file, key))
	}

	/// How the replica compartment is protected.
	#[must_use]
	pub fn replica_protection(&self) -> &Protection {
		&self.replica.protection
	}

	/// How the secrets compartment is protected, if there is one.
	#[must_use]
	pub fn secrets_protection(&self) -> Option<&Protection> {
		self.secrets.as_ref().map(|secrets| &secrets.protection)
	}

	/// Unlocks the replica compartment.
	///
	/// # Errors
	///
	/// Returns an error if `unlocker` does not open it.
	pub fn unlock_replica(&self, unlocker: &Unlocker<'_>) -> Result<CompartmentKey, KeysError> {
		self.replica.unlock(REPLICA, unlocker)
	}

	/// The replica's keys.
	///
	/// # Errors
	///
	/// Returns an error if `key` is not the replica compartment's key.
	pub fn replica(&self, key: &CompartmentKey) -> Result<ReplicaKeys, KeysError> {
		ReplicaKeys::decode(&self.replica.open(REPLICA, key)?)
	}

	/// Replaces the replica's keys, for example after the replica joins a space.
	///
	/// # Errors
	///
	/// Returns an error if `key` is not the replica compartment's key.
	pub fn set_replica(
		&mut self,
		keys: &ReplicaKeys,
		key: &CompartmentKey,
		rng: &mut impl CryptoRng,
	) -> Result<(), KeysError> {
		self.replica.reseal(&keys.encode(), REPLICA, key, rng)
	}

	/// Protects the replica compartment another way, for example with a new passphrase.
	/// Returns the compartment's new key. A keystore entry that the old protection used is
	/// deleted once the file is written.
	///
	/// # Errors
	///
	/// Returns an error if `key` is not the replica compartment's key, or the new protection
	/// cannot be set up.
	pub fn protect_replica(
		&mut self,
		key: &CompartmentKey,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<CompartmentKey, KeysError> {
		let contents = self.replica.open(REPLICA, key)?;
		let (compartment, new_key) = Compartment::seal(&contents, REPLICA, protection, rng)?;
		let old = std::mem::replace(&mut self.replica, compartment);
		self.retire(old);
		Ok(new_key)
	}

	/// Sets the secrets compartment, protected as `protection` says, replacing any there
	/// was: when the replica is chosen to hold secrets, or the person changes how they are
	/// protected. Returns the compartment's key.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase cannot be used or the keystore fails.
	pub fn set_secrets(
		&mut self,
		keys: &SecretsKeys,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<CompartmentKey, KeysError> {
		let (compartment, key) = Compartment::seal(&keys.encode(), SECRETS, protection, rng)?;
		if let Some(old) = self.secrets.replace(compartment) {
			self.retire(old);
		}
		Ok(key)
	}

	/// Unlocks the secrets compartment. Unlock it each time the person confirms, and drop
	/// the key and the secrets keys after a timeout.
	///
	/// # Errors
	///
	/// Returns an error if the file has no secrets compartment, or `unlocker` does not open
	/// it.
	pub fn unlock_secrets(&self, unlocker: &Unlocker<'_>) -> Result<CompartmentKey, KeysError> {
		self.secrets
			.as_ref()
			.ok_or(KeysError::NoSecrets)?
			.unlock(SECRETS, unlocker)
	}

	/// The secrets keys.
	///
	/// # Errors
	///
	/// Returns an error if the file has no secrets compartment, or `key` is not its key.
	pub fn secrets(&self, key: &CompartmentKey) -> Result<SecretsKeys, KeysError> {
		let secrets = self.secrets.as_ref().ok_or(KeysError::NoSecrets)?;
		SecretsKeys::decode(&secrets.open(SECRETS, key)?)
	}

	/// Whether the keystore lost the key of the secrets compartment, which then opens never
	/// again: Android destroys keys that need the person's confirmation once the screen lock
	/// is removed. Asks the keystore for the entry's attributes: a store whose keys need a
	/// confirmation should answer without one, as an app's on Android does.
	///
	/// # Errors
	///
	/// Returns an error if the keystore fails.
	pub fn secrets_key_lost(&self) -> Result<bool, KeysError> {
		let Some(Protection::Keystore { service, account }) = self.secrets_protection() else {
			return Ok(false);
		};
		match keyring_core::Entry::new(service, account)?.get_attributes() {
			Ok(_) => Ok(false),
			Err(keyring_core::Error::NoEntry) => Ok(true),
			Err(error) => Err(error.into()),
		}
	}

	/// Replaces the secrets keys.
	///
	/// # Errors
	///
	/// Returns an error if the file has no secrets compartment, or `key` is not its key.
	pub fn update_secrets(
		&mut self,
		keys: &SecretsKeys,
		key: &CompartmentKey,
		rng: &mut impl CryptoRng,
	) -> Result<(), KeysError> {
		self.secrets
			.as_mut()
			.ok_or(KeysError::NoSecrets)?
			.reseal(&keys.encode(), SECRETS, key, rng)
	}

	/// Reads a key file.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be read or is not a key file.
	pub fn read(path: &Path) -> Result<Self, KeysError> {
		let bytes = fs::read(path)?;
		cbor::from_slice(&bytes).map_err(|error| KeysError::Damaged(error.to_string()))
	}

	/// Writes the key file, replacing it as a whole: a new file is written next to it and
	/// renamed over it, so that a crash leaves either the old file or the new one. On
	/// Unix, only the owner may read it. Then deletes the keystore keys that only replaced
	/// compartments needed; a key that cannot be deleted now is tried again at the next
	/// write.
	///
	/// # Errors
	///
	/// Returns an error if the new file cannot be written; the old one is then unchanged.
	pub fn write(&mut self, path: &Path) -> Result<(), KeysError> {
		let name = path
			.file_name()
			.map_or_else(|| "keys".into(), |name| name.to_string_lossy());
		let temporary = path.with_file_name(format!(
			".{name}.{}.{}.tmp",
			std::process::id(),
			WRITES.fetch_add(1, Ordering::Relaxed)
		));
		let written =
			write_new(&temporary, &cbor::to_vec(self)).and_then(|()| fs::rename(&temporary, path));
		if let Err(error) = written {
			let _ = fs::remove_file(&temporary);
			return Err(error.into());
		}
		// The rename is in place; syncing the directory makes it survive a power cut where
		// the file system allows it.
		#[cfg(unix)]
		{
			let directory = path
				.parent()
				.filter(|directory| !directory.as_os_str().is_empty())
				.unwrap_or_else(|| Path::new("."));
			let _ = fs::File::open(directory).and_then(|directory| directory.sync_all());
		}
		self.obsolete.retain(|(service, account)| {
			let deleted = keyring_core::Entry::new(service, account)
				.and_then(|entry| entry.delete_credential());
			matches!(deleted, Err(error) if !matches!(error, keyring_core::Error::NoEntry))
		});
		Ok(())
	}

	/// Deletes the keystore keys that the file's compartments use, for a file that is
	/// thrown away, such as one whose library was never finished. A file protected by
	/// passphrases uses none.
	pub fn discard(self) {
		let entries = [Some(self.replica), self.secrets]
			.into_iter()
			.flatten()
			.filter_map(|compartment| match compartment.protection {
				Protection::Keystore { service, account } => Some((service, account)),
				Protection::Passphrase { .. } => None,
			})
			.chain(self.obsolete);
		for (service, account) in entries {
			let _ = keyring_core::Entry::new(&service, &account)
				.and_then(|entry| entry.delete_credential());
		}
	}

	/// Puts in the file a secrets compartment sealed apart from it, in place of any it had,
	/// whose keystore key is deleted once the file is written.
	pub fn put_secrets(&mut self, mut sealed: SealedSecrets) {
		if let Some(new) = sealed.0.take()
			&& let Some(old) = self.secrets.replace(new)
		{
			self.retire(old);
		}
	}

	/// Removes the secrets compartment from a file that is written next, such as one that a
	/// new replica made to receive the secrets, which it was not given; its keystore key, if
	/// it has one, is deleted once the file is written without it.
	pub fn remove_secrets(&mut self) {
		if let Some(old) = self.secrets.take() {
			self.retire(old);
		}
	}

	/// Removes the secrets compartment, and deletes its keystore key if it has one: for a
	/// compartment that no written file holds, such as one whose file could not be written.
	pub fn discard_secrets(&mut self) {
		if let Some(Compartment {
			protection: Protection::Keystore { service, account },
			..
		}) = self.secrets.take()
		{
			let _ = keyring_core::Entry::new(&service, &account)
				.and_then(|entry| entry.delete_credential());
		}
	}

	/// Puts back `before`, the file as it was before a change that could not be written,
	/// such as a new protection, and deletes the keystore keys that only the change made:
	/// those of compartments protected otherwise in `before`. Keystore keys still to delete
	/// that `before` does not use stay to delete, such as those taken over from an earlier
	/// copy of the file.
	pub fn restore(&mut self, before: Self) {
		let uses = |(service, account): &(String, String)| {
			[Some(&before.replica), before.secrets.as_ref()]
				.into_iter()
				.flatten()
				.any(|compartment| {
					matches!(&compartment.protection, Protection::Keystore { service: s, account: a }
						if s == service && a == account)
				})
		};
		let mut pending: Vec<(String, String)> = std::mem::take(&mut self.obsolete)
			.into_iter()
			.filter(|entry| !uses(entry))
			.collect();
		let changed = |now: Option<&Compartment>, then: Option<&Compartment>| match now {
			Some(Compartment {
				protection: Protection::Keystore { service, account },
				..
			}) if now.map(|now| &now.protection) != then.map(|then| &then.protection) => {
				Some((service.clone(), account.clone()))
			}
			_ => None,
		};
		let made = [
			changed(Some(&self.replica), Some(&before.replica)),
			changed(self.secrets.as_ref(), before.secrets.as_ref()),
		];
		for (service, account) in made.into_iter().flatten() {
			let _ = keyring_core::Entry::new(&service, &account)
				.and_then(|entry| entry.delete_credential());
		}
		*self = before;
		pending.retain(|entry| !self.obsolete.contains(entry));
		self.obsolete.append(&mut pending);
	}

	/// Takes over the keystore keys that `older`, an earlier copy of this file, has still to
	/// delete, so that writing this one deletes them.
	pub fn take_obsolete(&mut self, older: &mut Self) {
		self.obsolete.append(&mut older.obsolete);
	}

	/// Remembers the keystore entry of a replaced compartment, to delete it once the file is
	/// written.
	fn retire(&mut self, old: Compartment) {
		if let Protection::Keystore { service, account } = old.protection {
			self.obsolete.push((service, account));
		}
	}
}

/// Creates a new file, readable only by its owner on Unix, and writes and syncs `bytes`.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let mut options = fs::OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	let mut file = options.open(path)?;
	file.write_all(bytes)?;
	file.sync_all()
}

impl Compartment {
	/// Encrypts contents with a new compartment key, protected as `protection` says.
	fn seal(
		contents: &[u8],
		name: &str,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(Self, CompartmentKey), KeysError> {
		let (protection, key) = match *protection {
			NewProtection::Passphrase(passphrase, params) => {
				let mut salt = [0; 16];
				rng.fill_bytes(&mut salt);
				let key = Key::from_passphrase(passphrase, &salt, &params)?;
				(Protection::Passphrase { params, salt }, key)
			}
			NewProtection::Keystore { service } => {
				// A new account name, so that no key that a written file needs is replaced.
				let mut suffix = [0; 8];
				rng.fill_bytes(&mut suffix);
				let account = format!("nibkeep-{:016x}", u64::from_be_bytes(suffix));
				let key = Key::generate(rng);
				keyring_core::Entry::new(service, &account)?.set_secret(key.expose_bytes())?;
				let protection = Protection::Keystore {
					service: service.to_owned(),
					account,
				};
				(protection, key)
			}
		};
		let envelope = Envelope::seal(&key, contents, &associated_data(name, &protection), rng);
		Ok((
			Self {
				protection,
				envelope,
			},
			CompartmentKey(key),
		))
	}

	/// Gets the compartment key from the passphrase or the keystore, and checks that it
	/// opens the compartment.
	fn unlock(&self, name: &str, unlocker: &Unlocker<'_>) -> Result<CompartmentKey, KeysError> {
		let key = match (&self.protection, unlocker) {
			(Protection::Passphrase { params, salt }, Unlocker::Passphrase(passphrase)) => {
				Key::from_passphrase(passphrase, salt, params)?
			}
			(Protection::Keystore { service, account }, Unlocker::Keystore) => {
				let secret =
					Zeroizing::new(keyring_core::Entry::new(service, account)?.get_secret()?);
				Key::from_slice(&secret).ok_or(KeysError::WrongKey)?
			}
			_ => return Err(KeysError::WrongUnlocker),
		};
		let key = CompartmentKey(key);
		self.open(name, &key)?;
		Ok(key)
	}

	/// Decrypts the compartment's contents.
	fn open(&self, name: &str, key: &CompartmentKey) -> Result<Zeroizing<Vec<u8>>, KeysError> {
		self.envelope
			.open(&key.0, &associated_data(name, &self.protection))
			.map_err(|_| match self.protection {
				Protection::Passphrase { .. } => KeysError::WrongPassphrase,
				Protection::Keystore { .. } => KeysError::WrongKey,
			})
	}

	/// Encrypts new contents with the compartment's key, after checking the key.
	fn reseal(
		&mut self,
		contents: &[u8],
		name: &str,
		key: &CompartmentKey,
		rng: &mut impl CryptoRng,
	) -> Result<(), KeysError> {
		self.open(name, key)?;
		self.envelope = Envelope::seal(
			&key.0,
			contents,
			&associated_data(name, &self.protection),
			rng,
		);
		Ok(())
	}
}

/// The associated data of a compartment: its name and its protection, so that neither can
/// be changed without the change being noticed.
fn associated_data(name: &str, protection: &Protection) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![name.to_cbor(), protection.to_cbor()]))
}

impl ToCbor for KeyFile {
	fn to_cbor(&self) -> Value {
		let mut items = vec![SUITE.to_cbor(), self.replica.to_cbor()];
		if let Some(secrets) = &self.secrets {
			items.push(secrets.to_cbor());
		}
		Value::Array(items)
	}
}

impl FromCbor for KeyFile {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a key file: [1, replica] or [1, replica, secrets]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let (suite, replica, secrets) = match items.as_slice() {
			[suite, replica] => (suite, replica, None),
			[suite, replica, secrets] => (suite, replica, Some(secrets)),
			_ => return Err(cbor::Error::Expected(EXPECTED)),
		};
		if u8::from_cbor(suite) != Ok(SUITE) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		Ok(Self {
			replica: Compartment::from_cbor(replica)?,
			secrets: secrets.map(Compartment::from_cbor).transpose()?,
			obsolete: Vec::new(),
		})
	}
}

impl ToCbor for Protection {
	fn to_cbor(&self) -> Value {
		match self {
			Self::Passphrase { params, salt } => Value::Array(vec![
				0_u8.to_cbor(),
				params.to_cbor(),
				Value::Bytes(salt.to_vec()),
			]),
			Self::Keystore { service, account } => {
				Value::Array(vec![1_u8.to_cbor(), service.to_cbor(), account.to_cbor()])
			}
		}
	}
}

impl FromCbor for Protection {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a protection: [0, parameters, salt] or [1, service, account]";
		let error = || cbor::Error::Expected(EXPECTED);
		let Value::Array(items) = value else {
			return Err(error());
		};
		match items.as_slice() {
			[kind, params, Value::Bytes(salt)] if u8::from_cbor(kind) == Ok(0) => {
				Ok(Self::Passphrase {
					params: PassphraseParams::from_cbor(params)?,
					salt: salt.as_slice().try_into().map_err(|_| error())?,
				})
			}
			[kind, service, account] if u8::from_cbor(kind) == Ok(1) => Ok(Self::Keystore {
				service: String::from_cbor(service)?,
				account: String::from_cbor(account)?,
			}),
			_ => Err(error()),
		}
	}
}

impl ToCbor for Compartment {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![self.protection.to_cbor(), self.envelope.to_cbor()])
	}
}

impl FromCbor for Compartment {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a compartment: [protection, envelope]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [protection, envelope] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			protection: Protection::from_cbor(protection)?,
			envelope: Envelope::from_cbor(envelope)?,
		})
	}
}
