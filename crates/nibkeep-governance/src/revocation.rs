//! Revoking replicas, the space keys that a space's statements seal to its replicas, the
//! secrets keys sealed to the replicas that hold the secrets, and accepting a revoked
//! replica's changes that were set aside
//! ([Identity](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/identity.md#4-revoking-replicas)).

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, Key, SealedMessage, VerifyingKey};
use nibkeep_model::{Operation, Statement, Value, fields};

use crate::pairing::{SPACE_KEYS, space_key_data};

/// The context with which secrets keys are sealed to replicas' secrets encryption keys.
const SECRETS_KEYS: &str = "nibkeep 2026-10 secrets keys";

/// A space key sealed to a replica, as a statement of [`fields::SPACE_KEY`] on the replica's
/// record holds it: the key's number, its hash, and the key sealed to the replica's
/// encryption key, as pairing seals it. In text, the canonical encoding of
/// `[epoch, key hash, sealed key]` in base64url without padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedSpaceKey {
	/// The key's number.
	pub epoch: u64,
	/// The key's hash, as the hash of a change key is computed.
	pub hash: Hash,
	/// The key, sealed to the replica.
	pub sealed: SealedMessage,
}

impl SealedSpaceKey {
	/// Seals a key of `space`, whose number is `epoch` and hash `hash`, to `recipient`, the
	/// encryption key of `replica`.
	pub fn seal(
		recipient: &EncryptionKey,
		space: SpaceId,
		replica: EntityId,
		(epoch, key, hash): (u64, &Key, Hash),
		rng: &mut impl CryptoRng,
	) -> Self {
		let data = space_key_data(space, epoch, replica);
		Self {
			epoch,
			hash,
			sealed: SealedMessage::seal_key(recipient, key, SPACE_KEYS, &data, rng),
		}
	}

	/// The key, if it opens with `decryption`, the key of `replica` that it was sealed to.
	/// Whether its hash is the one named is for the caller to check.
	#[must_use]
	pub fn open(
		&self,
		decryption: &DecryptionKey,
		space: SpaceId,
		replica: EntityId,
	) -> Option<Key> {
		let data = space_key_data(space, self.epoch, replica);
		self.sealed.open_key(decryption, SPACE_KEYS, &data).ok()
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The sealed key that a text holds, if it holds one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

impl ToCbor for SealedSpaceKey {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.epoch.to_cbor(),
			self.hash.to_cbor(),
			self.sealed.to_cbor(),
		])
	}
}

impl FromCbor for SealedSpaceKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a sealed space key: [epoch, key hash, sealed key]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [epoch, hash, sealed] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			epoch: u64::from_cbor(epoch)?,
			hash: Hash::from_cbor(hash)?,
			sealed: SealedMessage::from_cbor(sealed)?,
		})
	}
}

/// The hash that names a secrets encryption key, which a sealed secrets key names as its
/// recipient: the hash of its bytes.
#[must_use]
pub fn secrets_recipient(key: &EncryptionKey) -> Hash {
	Hash::of(&key.to_bytes())
}

/// A secrets key of a space sealed to a replica, as a statement of [`fields::SECRETS_KEY`]
/// on the replica's record holds it: the key's number, its hash, the key sealed to the
/// replica's secrets encryption key, the hash of that key, so that it is told apart from a
/// key sealed to one that the replica had before, and a link that proves its giver held a
/// secrets key ([`SecretsLink`]). In text, the canonical encoding of
/// `[epoch, key hash, sealed key, recipient, link]` in base64url without padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedSecretsKey {
	/// The key's number.
	pub epoch: u64,
	/// The key's hash, as the hash of a change key is computed.
	pub hash: Hash,
	/// The key, sealed to the replica's secrets encryption key.
	pub sealed: SealedMessage,
	/// The hash of the secrets encryption key it was sealed to: [`secrets_recipient`].
	pub recipient: Hash,
	/// The proof that its giver held a secrets key.
	pub link: SecretsLink,
}

/// The context of the key, derived from a secrets key, that tags the gifts it links.
const SECRETS_LINKS: &str = "nibkeep 2026-11 secrets links";

/// A proof, in a gift of a secrets key, that its giver held a secrets key: a tag of the gift,
/// made with a key derived from that key, which names it by its space and number. Only a
/// replica that holds the key can make it, and only one that holds it can check it: a
/// replica that holds the key takes the gift from any replica, as only one that held the
/// secrets can have given it
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)). In CBOR,
/// `[space, epoch, tag]`; the tag is BLAKE3 in keyed mode of the canonical encoding of
/// `[space, epoch, key hash, replica, recipient]`, those of the gift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretsLink {
	/// The space of the secrets key that made it.
	pub space: SpaceId,
	/// The number of the secrets key that made it.
	pub epoch: u64,
	/// The tag.
	pub tag: [u8; 32],
}

impl SealedSecretsKey {
	/// Seals a secrets key of `space`, whose number is `epoch` and hash `hash`, to
	/// `recipient`, the secrets encryption key of `replica`, linked with `link_key`, the
	/// secrets key of `link_space` whose number is `link_epoch`, which the giver holds.
	pub fn seal(
		recipient: &EncryptionKey,
		space: SpaceId,
		replica: EntityId,
		(epoch, key, hash): (u64, &Key, Hash),
		(link_space, link_epoch, link_key): (SpaceId, u64, &Key),
		rng: &mut impl CryptoRng,
	) -> Self {
		let data = space_key_data(space, epoch, replica);
		let recipient_hash = secrets_recipient(recipient);
		let tag = link_key.derive(SECRETS_LINKS).tag(&linked_data(
			space,
			replica,
			(epoch, hash, recipient_hash),
		));
		Self {
			epoch,
			hash,
			sealed: SealedMessage::seal_key(recipient, key, SECRETS_KEYS, &data, rng),
			recipient: recipient_hash,
			link: SecretsLink {
				space: link_space,
				epoch: link_epoch,
				tag,
			},
		}
	}

	/// Whether its link was made with `key`, the secrets key that the link names, for this
	/// gift of a secrets key of `space` to `replica`.
	#[must_use]
	pub fn link_holds(&self, space: SpaceId, replica: EntityId, key: &Key) -> bool {
		let data = linked_data(space, replica, (self.epoch, self.hash, self.recipient));
		key.derive(SECRETS_LINKS).verify_tag(&data, &self.link.tag)
	}

	/// The key, if it opens with `decryption`, the secrets decryption key of `replica` that it
	/// was sealed to. Whether its hash is the one named is for the caller to check.
	#[must_use]
	pub fn open(
		&self,
		decryption: &DecryptionKey,
		space: SpaceId,
		replica: EntityId,
	) -> Option<Key> {
		let data = space_key_data(space, self.epoch, replica);
		self.sealed.open_key(decryption, SECRETS_KEYS, &data).ok()
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The sealed key that a text holds, if it holds one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// What a link tags: `[space, epoch, key hash, replica, recipient]`.
fn linked_data(
	space: SpaceId,
	replica: EntityId,
	(epoch, hash, recipient): (u64, Hash, Hash),
) -> Vec<u8> {
	cbor::encode(&Cbor::Array(vec![
		space.to_cbor(),
		epoch.to_cbor(),
		hash.to_cbor(),
		replica.to_cbor(),
		recipient.to_cbor(),
	]))
}

impl ToCbor for SealedSecretsKey {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.epoch.to_cbor(),
			self.hash.to_cbor(),
			self.sealed.to_cbor(),
			self.recipient.to_cbor(),
			Cbor::Array(vec![
				self.link.space.to_cbor(),
				self.link.epoch.to_cbor(),
				Cbor::Bytes(self.link.tag.to_vec()),
			]),
		])
	}
}

impl FromCbor for SealedSecretsKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a sealed secrets key: [epoch, key hash, sealed key, recipient, link]";
		const LINK: &str = "a link: [space, epoch, tag]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [epoch, hash, sealed, recipient, Cbor::Array(link)] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [link_space, link_epoch, Cbor::Bytes(tag)] = link.as_slice() else {
			return Err(cbor::Error::Expected(LINK));
		};
		let link = SecretsLink {
			space: SpaceId::from_cbor(link_space)?,
			epoch: u64::from_cbor(link_epoch)?,
			tag: tag
				.as_slice()
				.try_into()
				.map_err(|_| cbor::Error::Expected(LINK))?,
		};
		Ok(Self {
			epoch: u64::from_cbor(epoch)?,
			hash: Hash::from_cbor(hash)?,
			sealed: SealedMessage::from_cbor(sealed)?,
			recipient: Hash::from_cbor(recipient)?,
			link,
		})
	}
}

/// The operations that revoke replicas' keys: each replica's record gets
/// [`fields::REVOKED`] with the key it revokes, as text. They govern the space, and a
/// revocation is final.
pub fn revoke(keys: &[(EntityId, VerifyingKey)], rng: &mut impl CryptoRng) -> Vec<Operation> {
	keys.iter()
		.map(|(replica, key)| assert(*replica, fields::REVOKED, Value::Text(key.to_text()), rng))
		.collect()
}

/// The operation that gives a replica a key of the space, sealed to it.
pub fn give_key(replica: EntityId, key: &SealedSpaceKey, rng: &mut impl CryptoRng) -> Operation {
	assert(replica, fields::SPACE_KEY, Value::Text(key.to_text()), rng)
}

/// The operation that states a replica's secrets encryption key, which secrets keys are
/// sealed to.
pub fn state_secrets_key(
	replica: EntityId,
	key: &EncryptionKey,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert(
		replica,
		fields::SECRETS_ENCRYPTION_KEY,
		Value::Text(crate::encryption_key_text(key)),
		rng,
	)
}

/// The operation that states the replica that a replica takes the secrets from: the one
/// that the person chose, or itself.
pub fn state_secrets_from(
	replica: EntityId,
	from: EntityId,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert(replica, fields::SECRETS_FROM, Value::Reference(from), rng)
}

/// The operation that gives a replica a secrets key of the space, sealed to its secrets
/// encryption key.
pub fn give_secrets_key(
	replica: EntityId,
	key: &SealedSecretsKey,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert(
		replica,
		fields::SECRETS_KEY,
		Value::Text(key.to_text()),
		rng,
	)
}

/// The operation that accepts a change that was set aside, known by its ID in text, on the
/// record of the replica that made it: the change counts after all.
pub fn accept(replica: EntityId, change: &str, rng: &mut impl CryptoRng) -> Operation {
	assert(
		replica,
		fields::ACCEPTED,
		Value::Text(change.to_owned()),
		rng,
	)
}

/// The operation that asserts a value of a replica's field.
fn assert(
	replica: EntityId,
	field: nibkeep_model::FieldName,
	value: Value,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		replica,
		field,
		value,
	))
}
