//! A replica's listing in a space with members, which its own key signs, so that nobody
//! lists a key they do not hold, nor puts one on a record that is not that key's
//! ([technical design](../../../docs/Technical-design.md#13-sharing)).
//!
//! A replica's record in such a space has an ID derived from the space and the key, and the
//! listing names the record's owner and whether the replica is an AI agent's. In a space
//! other than the personal one, a governing change lists a key only with its listing on the
//! same record ([Log](../../../docs/spec/log.md#8-which-changes-count)).

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::{EncryptionKey, Signature, SigningKey, VerifyingKey};

/// The context of a listing's signature.
const LISTINGS: &str = "nibkeep listing";

/// The context from which a replica's record in a space with members is derived.
const RECORDS: &str = "nibkeep 2026-11 replica records";

/// The context from which the record of a replica's device in a space with members is
/// derived.
const DEVICES: &str = "nibkeep 2026-11 device records";

/// The context from which the record of a space's names in the personal space is derived.
const SPACE_NAMES: &str = "nibkeep 2026-11 space names";

/// The ID of the record of the replica whose verifying key is `key`, in `space`: the first 16
/// bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 replica records` from
/// the space's ID and the key, as a version 4 UUID.
#[must_use]
pub fn replica_record(space: SpaceId, key: &VerifyingKey) -> EntityId {
	derived(RECORDS, space, key)
}

/// The ID of the record of the device of the replica whose verifying key is `key`, in
/// `space`, derived as [`replica_record`] is, with the context `nibkeep 2026-11 device
/// records`: so every replica knows whose device a record is, and whose role allows
/// changing it.
#[must_use]
pub fn device_record(space: SpaceId, key: &VerifyingKey) -> EntityId {
	derived(DEVICES, space, key)
}

/// The ID of the record in the personal space `personal` that keeps the names of `space`,
/// derived as [`replica_record`] is from the two spaces' IDs, with the context
/// `nibkeep 2026-11 space names`: so that the devices that hold the space keep one record of
/// its names, which the others read.
#[must_use]
pub fn space_names_record(personal: SpaceId, space: SpaceId) -> EntityId {
	let mut material = Vec::with_capacity(32);
	material.extend_from_slice(personal.as_bytes());
	material.extend_from_slice(space.as_bytes());
	EntityId::derived(SPACE_NAMES, &material)
}

/// The first 16 bytes of the BLAKE3 key derived with `context` from the space's ID and the
/// key, as a version 4 UUID.
fn derived(context: &str, space: SpaceId, key: &VerifyingKey) -> EntityId {
	let mut material = Vec::with_capacity(48);
	material.extend_from_slice(space.as_bytes());
	material.extend_from_slice(key.as_bytes());
	EntityId::derived(context, &material)
}

/// A replica's listing in a space, signed by the replica's own key: the space, the record it
/// lists the key on, the record's owner, whether the replica is an AI agent's, and the hash
/// of its encryption key, so that nobody states another encryption key with it. In text,
/// the canonical encoding of `[space, record, owner, ai-agent, encryption, key, signature]`
/// in base64url without padding, where the signature, with the context `nibkeep listing`, is
/// of the canonical encoding of `[space, record, owner, ai-agent, encryption]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
	/// The space.
	pub space: SpaceId,
	/// The record that the key is listed on.
	pub record: EntityId,
	/// The record's owner: the member whose replica it is.
	pub owner: EntityId,
	/// Whether the replica is an AI agent's.
	pub ai_agent: bool,
	/// The hash of the replica's encryption key: [`encryption_hash`].
	pub encryption: Hash,
	/// The replica's verifying key.
	pub key: VerifyingKey,
	/// The key's signature of the rest.
	pub signature: Signature,
}

impl Listing {
	/// The listing of the replica whose signing key is `signing` and encryption key
	/// `encryption`, in `space`, on the record derived from them, owned by `owner`.
	#[must_use]
	pub fn sign(
		signing: &SigningKey,
		encryption: &EncryptionKey,
		space: SpaceId,
		(owner, ai_agent): (EntityId, bool),
	) -> Self {
		let key = signing.verifying_key();
		let record = replica_record(space, &key);
		let encryption = encryption_hash(&encryption.to_bytes());
		let signature = signing.sign(
			LISTINGS,
			&signed(space, record, owner, ai_agent, encryption),
		);
		Self {
			space,
			record,
			owner,
			ai_agent,
			encryption,
			key,
			signature,
		}
	}

	/// Whether the key signed it, and its record is the one derived from its space and key.
	#[must_use]
	pub fn verifies(&self) -> bool {
		self.record == replica_record(self.space, &self.key)
			&& self
				.key
				.verify(
					LISTINGS,
					&signed(
						self.space,
						self.record,
						self.owner,
						self.ai_agent,
						self.encryption,
					),
					&self.signature,
				)
				.is_ok()
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The listing that a text holds, if it holds one, whether it verifies or not.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// The hash that names an encryption key in a listing: the BLAKE3 hash of its bytes.
#[must_use]
pub fn encryption_hash(bytes: &[u8]) -> Hash {
	Hash::of(bytes)
}

/// What a listing's key signs.
fn signed(
	space: SpaceId,
	record: EntityId,
	owner: EntityId,
	ai_agent: bool,
	encryption: Hash,
) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![
		space.to_cbor(),
		record.to_cbor(),
		owner.to_cbor(),
		Cbor::Bool(ai_agent),
		encryption.to_cbor(),
	]))
}

impl ToCbor for Listing {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.record.to_cbor(),
			self.owner.to_cbor(),
			Cbor::Bool(self.ai_agent),
			self.encryption.to_cbor(),
			self.key.to_cbor(),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for Listing {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a listing: [space, record, owner, ai-agent, encryption, key, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, record, owner, ai_agent, encryption, key, signature] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let Cbor::Bool(ai_agent) = ai_agent else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			record: EntityId::from_cbor(record)?,
			owner: EntityId::from_cbor(owner)?,
			ai_agent: *ai_agent,
			encryption: Hash::from_cbor(encryption)?,
			key: VerifyingKey::from_cbor(key)?,
			signature: Signature::from_cbor(signature)?,
		})
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn a_listing_verifies_and_survives_its_text_form() {
		let mut rng = TestRng::new("a listing");
		let signing = SigningKey::generate(&mut rng);
		let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
		let listing = Listing::sign(
			&signing,
			&encryption,
			rng.space_id(),
			(rng.entity_id(), false),
		);
		assert!(listing.verifies());
		let again = Listing::from_text(&listing.to_text()).expect("reads");
		assert_eq!(again, listing);
		assert!(again.verifies());
	}

	#[test]
	fn a_listing_changed_after_signing_does_not_verify() {
		let mut rng = TestRng::new("a changed listing");
		let signing = SigningKey::generate(&mut rng);
		let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
		let listing = Listing::sign(
			&signing,
			&encryption,
			rng.space_id(),
			(rng.entity_id(), false),
		);
		let mut owner = listing.clone();
		owner.owner = rng.entity_id();
		assert!(!owner.verifies());
		let mut marked = listing.clone();
		marked.ai_agent = true;
		assert!(!marked.verifies());
		let mut another_key = listing.clone();
		another_key.encryption = Hash::of(b"another encryption key");
		assert!(!another_key.verifies());
		// Another key's signature, on a record derived from the first, does not verify.
		let other = SigningKey::generate(&mut rng);
		let mut borrowed =
			Listing::sign(&other, &encryption, listing.space, (listing.owner, false));
		borrowed.record = listing.record;
		assert!(!borrowed.verifies());
		// Nor does a key on another's record, whatever it signs.
		let mut moved = listing;
		moved.key = other.verifying_key();
		assert!(!moved.verifies());
	}

	#[test]
	fn records_differ_by_space_and_key() {
		let mut rng = TestRng::new("records");
		let key = SigningKey::generate(&mut rng).verifying_key();
		let other = SigningKey::generate(&mut rng).verifying_key();
		let space = rng.space_id();
		assert_eq!(replica_record(space, &key), replica_record(space, &key));
		assert_ne!(replica_record(space, &key), replica_record(space, &other));
		assert_ne!(
			replica_record(space, &key),
			replica_record(rng.space_id(), &key)
		);
	}
}
