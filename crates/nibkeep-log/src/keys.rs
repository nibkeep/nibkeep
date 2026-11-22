//! The keys of a space: the space keys that encrypt changes, the secrets keys, the wrapped
//! record keys of secret values, and the wrapped keys of blobs.

use std::collections::BTreeMap;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Envelope, Key, Zeroizing};
use zeroize::Zeroize;

/// The context of the key, derived from a space key, that wraps change keys.
const CHANGE_KEYS_CONTEXT: &str = "nibkeep 2026-10 change keys";

/// The context of the key, derived from a secrets key, that wraps record keys.
const RECORD_KEYS_CONTEXT: &str = "nibkeep 2026-10 record keys";

/// The context of the key, derived from a space key, that wraps blob keys.
const BLOB_KEYS_CONTEXT: &str = "nibkeep 2026-10 blob keys";

/// The context of the key, derived from a secrets key, that wraps the keys of blobs of
/// secret fields, inside the space key.
const SECRET_BLOB_KEYS_CONTEXT: &str = "nibkeep 2026-10 secret blob keys";

/// The keys of a space that a replica has, by their numbers: 0 for a space's first key, and
/// a random one for each key that a rotation brings, so that two rotations made at once
/// differ; the numbers do not tell which key is newer. New changes use the newest key whose
/// rotation counts, which the replica chooses; older changes keep the key they were made
/// with.
///
/// Besides the space keys, a replica that the person chose holds the space's secrets keys
/// while its secrets are unlocked.
#[derive(Clone, Debug)]
pub struct SpaceKeys {
	keys: BTreeMap<u64, Key>,
	secrets: BTreeMap<u64, Key>,
	/// The key new changes and blob keys are wrapped with, if not the newest.
	writing: Option<u64>,
	/// The secrets key new secret values are sealed with, if not the one of the highest
	/// number.
	secrets_writing: Option<u64>,
}

impl SpaceKeys {
	/// Returns the keys of a space with this one space key so far, and no secrets key.
	#[must_use]
	pub fn new(epoch: u64, key: Key) -> Self {
		Self {
			keys: BTreeMap::from([(epoch, key)]),
			secrets: BTreeMap::new(),
			writing: None,
			secrets_writing: None,
		}
	}

	/// Wraps new changes and blob keys with the key of this number from now on, or with the
	/// one of the highest number if `None` or if there is no key of this number. A replica
	/// writes with the newest key whose rotation counts, which a key it got may not be, and
	/// which the numbers, random ones, do not tell.
	pub fn write_with(&mut self, epoch: Option<u64>) {
		self.writing = epoch;
	}

	/// The same keys, wrapping new changes with the key of this number, if there is one.
	#[must_use]
	pub fn writing_with(&self, epoch: u64) -> Self {
		let mut keys = self.clone();
		keys.writing = Some(epoch);
		keys
	}

	/// The numbers of the space keys, in order.
	pub fn epochs(&self) -> impl Iterator<Item = u64> + '_ {
		self.keys.keys().copied()
	}

	/// The space keys with their numbers, in order.
	pub fn space_keys(&self) -> impl Iterator<Item = (u64, &Key)> {
		self.keys.iter().map(|(&epoch, key)| (epoch, key))
	}

	/// Adds a space key, after the space's key has changed.
	pub fn insert(&mut self, epoch: u64, key: Key) {
		self.keys.insert(epoch, key);
	}

	/// The space key with this number.
	#[must_use]
	pub fn get(&self, epoch: u64) -> Option<&Key> {
		self.keys.get(&epoch)
	}

	/// The space key that new changes and blob keys are wrapped with, with its number: the
	/// one [`SpaceKeys::write_with`] chose, or else the one of the highest number.
	#[must_use]
	pub fn newest(&self) -> (u64, &Key) {
		if let Some(epoch) = self.writing
			&& let Some(key) = self.keys.get(&epoch)
		{
			return (epoch, key);
		}
		let Some((&epoch, key)) = self.keys.last_key_value() else {
			unreachable!("there is always at least one space key");
		};
		(epoch, key)
	}

	/// Adds a secrets key, once the person has confirmed, on a replica chosen to hold the
	/// space's secrets.
	pub fn insert_secrets(&mut self, epoch: u64, key: Key) {
		self.secrets.insert(epoch, key);
	}

	/// Forgets the secrets keys, when the replica locks its secrets.
	pub fn forget_secrets(&mut self) {
		self.secrets.clear();
	}

	/// The secrets key with this number.
	#[must_use]
	pub fn secrets(&self, epoch: u64) -> Option<&Key> {
		self.secrets.get(&epoch)
	}

	/// The secrets keys, with their numbers, while the secrets are unlocked.
	pub fn secrets_keys(&self) -> impl Iterator<Item = (u64, &Key)> {
		self.secrets.iter().map(|(&epoch, key)| (epoch, key))
	}

	/// Seals new secret values with the secrets key of this number from now on, or with the
	/// one of the highest number if `None` or if there is no key of this number. A replica
	/// seals with the newest secrets key whose rotation knew of every departure, which the
	/// numbers, random ones, do not tell.
	pub fn write_secrets_with(&mut self, epoch: Option<u64>) {
		self.secrets_writing = epoch;
	}

	/// The secrets key that new secret values are sealed with, with its number, if the
	/// replica holds one: the one [`SpaceKeys::write_secrets_with`] chose, or else the one of
	/// the highest number.
	#[must_use]
	pub fn newest_secrets(&self) -> Option<(u64, &Key)> {
		if let Some(epoch) = self.secrets_writing
			&& let Some(key) = self.secrets.get(&epoch)
		{
			return Some((epoch, key));
		}
		self.secrets
			.last_key_value()
			.map(|(&epoch, key)| (epoch, key))
	}
}

/// The key that wraps change keys, derived from a space key.
pub(crate) fn change_keys_key(space_key: &Key) -> Key {
	space_key.derive(CHANGE_KEYS_CONTEXT)
}

/// The associated data that ties an envelope to its place: the space, and what the
/// envelope belongs to, such as the ID of a secret statement or the sealed hash of a blob.
pub(crate) fn associated_data(space: SpaceId, owner: &impl ToCbor) -> Vec<u8> {
	cbor::encode(&Cbor::Array(vec![space.to_cbor(), owner.to_cbor()]))
}

/// A wrapped key in CBOR: `[epoch, envelope]`, where `epoch` is the number of the key that
/// wraps it.
fn wrapped_to_cbor(epoch: u64, envelope: &Envelope) -> Cbor {
	Cbor::Array(vec![epoch.to_cbor(), envelope.to_cbor()])
}

/// Reads a wrapped key in CBOR: `[epoch, envelope]`.
fn wrapped_from_cbor(value: &Cbor, expected: &'static str) -> Result<(u64, Envelope), cbor::Error> {
	let Cbor::Array(items) = value else {
		return Err(cbor::Error::Expected(expected));
	};
	let [epoch, envelope] = items.as_slice() else {
		return Err(cbor::Error::Expected(expected));
	};
	Ok((u64::from_cbor(epoch)?, Envelope::from_cbor(envelope)?))
}

/// The record key of a secret statement, wrapped with the space's secrets key.
///
/// In CBOR, a wrapped key is the array `[epoch, envelope]`, where `epoch` is the number of
/// the secrets key. Wrapped keys are stored apart from changes, so that purging can
/// destroy them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedKey {
	epoch: u64,
	envelope: Envelope,
}

impl WrappedKey {
	/// Wraps the record key of `statement` with the newest secrets key.
	pub(crate) fn wrap(
		space: SpaceId,
		statement: StatementId,
		record_key: &Key,
		(epoch, secrets_key): (u64, &Key),
		rng: &mut impl CryptoRng,
	) -> Self {
		Self {
			epoch,
			envelope: Envelope::seal_key(
				&secrets_key.derive(RECORD_KEYS_CONTEXT),
				record_key,
				&associated_data(space, &statement),
				rng,
			),
		}
	}

	/// Unwraps the record key of `statement`, if `keys` hold the secrets key it needs.
	pub(crate) fn unwrap(
		&self,
		space: SpaceId,
		statement: StatementId,
		keys: &SpaceKeys,
	) -> Option<Key> {
		let secrets_key = keys.secrets(self.epoch)?;
		self.envelope
			.open_key(
				&secrets_key.derive(RECORD_KEYS_CONTEXT),
				&associated_data(space, &statement),
			)
			.ok()
	}

	/// The number of the secrets key that wraps the record key.
	#[must_use]
	pub const fn epoch(&self) -> u64 {
		self.epoch
	}
}

impl ToCbor for WrappedKey {
	fn to_cbor(&self) -> Cbor {
		wrapped_to_cbor(self.epoch, &self.envelope)
	}
}

impl FromCbor for WrappedKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		let (epoch, envelope) = wrapped_from_cbor(value, "a wrapped key: [epoch, envelope]")?;
		Ok(Self { epoch, envelope })
	}
}

/// The key of a blob, wrapped with a space key together with the blob's content hash, so
/// that the content hash stays as secret as the content. The key of a blob of a secret
/// field is wrapped with a secrets key first, so that only a replica that holds both opens
/// it.
///
/// In CBOR, a wrapped blob key is the array `[epoch, envelope]`, where `epoch` is the
/// number of the space key. The envelope holds `[content hash, blob key]`, or, for a
/// secret blob key, `[secrets epoch, envelope]`, whose envelope holds them, wrapped with
/// the secrets key of that number. It is known by the sealed hash of its blob, and stored
/// apart from changes, so that purging can destroy it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedBlobKey {
	epoch: u64,
	envelope: Envelope,
}

impl WrappedBlobKey {
	/// Wraps the key of the blob with the newest space key.
	pub(crate) fn wrap(
		space: SpaceId,
		sealed: &Hash,
		content: &Hash,
		blob_key: &Key,
		keys: &SpaceKeys,
		rng: &mut impl CryptoRng,
	) -> Self {
		Self::wrap_plaintext(
			space,
			sealed,
			&blob_key_plaintext(content, blob_key),
			keys,
			rng,
		)
	}

	/// Wraps the key of a blob of a secret field with the newest secrets key, then with the
	/// newest space key; `None` if `keys` hold no secrets key.
	pub(crate) fn wrap_secret(
		space: SpaceId,
		sealed: &Hash,
		content: &Hash,
		blob_key: &Key,
		keys: &SpaceKeys,
		rng: &mut impl CryptoRng,
	) -> Option<Self> {
		let (secrets_epoch, secrets_key) = keys.newest_secrets()?;
		let inner = Envelope::seal(
			&secrets_key.derive(SECRET_BLOB_KEYS_CONTEXT),
			&blob_key_plaintext(content, blob_key),
			&associated_data(space, sealed),
			rng,
		);
		let plaintext = cbor::encode(&wrapped_to_cbor(secrets_epoch, &inner));
		Some(Self::wrap_plaintext(space, sealed, &plaintext, keys, rng))
	}

	/// Wraps what a wrapped blob key holds with the newest space key.
	fn wrap_plaintext(
		space: SpaceId,
		sealed: &Hash,
		plaintext: &[u8],
		keys: &SpaceKeys,
		rng: &mut impl CryptoRng,
	) -> Self {
		let (epoch, space_key) = keys.newest();
		Self {
			epoch,
			envelope: Envelope::seal(
				&space_key.derive(BLOB_KEYS_CONTEXT),
				plaintext,
				&associated_data(space, sealed),
				rng,
			),
		}
	}

	/// Unwraps the content hash and the key of the blob with the sealed hash `sealed`, if
	/// `keys` hold the space key it needs, and for a secret blob key the secrets key too:
	/// `None` if the replica lacks the space key, or the key does not open.
	pub(crate) fn unwrap(
		&self,
		space: SpaceId,
		sealed: &Hash,
		keys: &SpaceKeys,
	) -> Option<Unwrapped> {
		let space_key = keys.get(self.epoch)?;
		let plaintext = self
			.envelope
			.open(
				&space_key.derive(BLOB_KEYS_CONTEXT),
				&associated_data(space, sealed),
			)
			.ok()?;
		let (secrets_epoch, inner) = match read_outer_plaintext(&plaintext)? {
			Outer::Ordinary(content, key) => {
				return Some(Unwrapped::Key {
					content,
					key,
					secret: false,
				});
			}
			Outer::Secret(secrets_epoch, inner) => (secrets_epoch, inner),
		};
		let Some(secrets_key) = keys.secrets(secrets_epoch) else {
			return Some(Unwrapped::Secret);
		};
		let plaintext = inner
			.open(
				&secrets_key.derive(SECRET_BLOB_KEYS_CONTEXT),
				&associated_data(space, sealed),
			)
			.ok()?;
		let (content, key) = read_blob_key_plaintext(&plaintext)?;
		Some(Unwrapped::Key {
			content,
			key,
			secret: true,
		})
	}

	/// The number of the space key that wraps the blob key.
	#[must_use]
	pub const fn epoch(&self) -> u64 {
		self.epoch
	}
}

impl ToCbor for WrappedBlobKey {
	fn to_cbor(&self) -> Cbor {
		wrapped_to_cbor(self.epoch, &self.envelope)
	}
}

impl FromCbor for WrappedBlobKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		let (epoch, envelope) = wrapped_from_cbor(value, "a wrapped blob key: [epoch, envelope]")?;
		Ok(Self { epoch, envelope })
	}
}

/// What a wrapped blob key gave when it was opened.
pub(crate) enum Unwrapped {
	/// The content hash of the blob and its key, and whether the key is a secret blob key,
	/// which the replica forgets when it locks its secrets.
	Key {
		content: Hash,
		key: Key,
		secret: bool,
	},
	/// A secret blob key, whose inner envelope needs a secrets key that the replica lacks,
	/// so that it can be neither opened nor checked yet.
	Secret,
}

/// What a wrapped blob key holds: `[content hash, blob key]` in CBOR, encoded without
/// leaving copies of the key behind.
fn blob_key_plaintext(content: &Hash, blob_key: &Key) -> Zeroizing<Vec<u8>> {
	let mut value = Cbor::Array(vec![
		content.to_cbor(),
		Cbor::Bytes(blob_key.expose_bytes().to_vec()),
	]);
	let mut plaintext = Zeroizing::new(Vec::with_capacity(cbor::encoded_len(&value)));
	cbor::encode_into(&value, &mut plaintext);
	wipe(&mut value);
	plaintext
}

/// What the outer envelope of a wrapped blob key holds.
enum Outer {
	/// The content hash and the key of a blob.
	Ordinary(Hash, Key),
	/// The number of a secrets key, and the envelope it opens.
	Secret(u64, Envelope),
}

/// Reads what the outer envelope of a wrapped blob key holds, decoded once, and wipes the
/// copies of the key it made: `[content hash, blob key]`, or `[secrets epoch, envelope]`.
fn read_outer_plaintext(plaintext: &[u8]) -> Option<Outer> {
	let mut value = cbor::decode(plaintext).ok()?;
	let read = match &value {
		Cbor::Array(items) => match items.as_slice() {
			[content, Cbor::Bytes(key)] => Hash::from_cbor(content)
				.ok()
				.zip(Key::from_slice(key))
				.map(|(content, key)| Outer::Ordinary(content, key))
				.or_else(|| {
					wrapped_from_cbor(&value, "a secret blob key: [secrets epoch, envelope]")
						.ok()
						.map(|(epoch, inner)| Outer::Secret(epoch, inner))
				}),
			_ => wrapped_from_cbor(&value, "a secret blob key: [secrets epoch, envelope]")
				.ok()
				.map(|(epoch, inner)| Outer::Secret(epoch, inner)),
		},
		_ => None,
	};
	wipe(&mut value);
	read
}

/// Reads what a wrapped blob key holds, and wipes the copies of the key it made.
fn read_blob_key_plaintext(plaintext: &[u8]) -> Option<(Hash, Key)> {
	let mut value = cbor::decode(plaintext).ok()?;
	let read = match &value {
		Cbor::Array(items) => match items.as_slice() {
			[content, Cbor::Bytes(key)] => Hash::from_cbor(content).ok().zip(Key::from_slice(key)),
			_ => None,
		},
		_ => None,
	};
	wipe(&mut value);
	read
}

/// Overwrites the byte strings of a value, where keys are.
fn wipe(value: &mut Cbor) {
	match value {
		Cbor::Bytes(bytes) => bytes.zeroize(),
		Cbor::Array(items) => items.iter_mut().for_each(wipe),
		_ => {}
	}
}
