//! A recovery code in a space: the statement that names it, on a record of its member's,
//! with the encryption keys that keys are sealed to, which the code itself signed
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
//!
//! A code is no replica: it signs no change, and no key of it is listed. Its member's own
//! replicas state it, and seal the space's keys to it as they do to each other.

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{EncryptionKey, RecoveryCode, Signature, VerifyingKey};
use nibkeep_model::{Operation, Statement, Value, fields};

use crate::encryption_hash;

/// The context of a recovery key's signature of its code's encryption keys.
const KEYS: &str = "nibkeep recovery key";

/// The context from which a code's record in a space is derived.
const RECORDS: &str = "nibkeep 2026-11 recovery records";

/// The ID of the record, in `space`, of the code whose recovery key is `key`, for `member`:
/// the user in a personal space, and the member's record in a space with members. It is the
/// first 16 bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 recovery
/// records` from the space's ID, the member's ID and the key, as a version 4 UUID. Each
/// member has a record of their own for a code, so that no member's statement of a code
/// crowds out another's.
#[must_use]
pub fn recovery_record(space: SpaceId, member: EntityId, key: &VerifyingKey) -> EntityId {
	let mut material = Vec::with_capacity(64);
	material.extend_from_slice(space.as_bytes());
	material.extend_from_slice(member.as_bytes());
	material.extend_from_slice(key.as_bytes());
	EntityId::derived(RECORDS, &material)
}

/// A recovery code as a space names it: the member it is of, its recovery key, and the two
/// encryption keys that the recovery key signed once, as the code was made, so that keys
/// are sealed only to encryption keys that the code itself named. In text, the canonical
/// encoding of `[member, key, encryption, secrets encryption, signature]` in base64url
/// without padding, where the signature, with the context `nibkeep recovery key`, is of the
/// canonical encoding of the two encryption keys' hashes, `[encryption, secrets
/// encryption]`. The signature names no space and no member, as it binds only keys to a
/// key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryStatement {
	/// The member whose code it is.
	pub member: EntityId,
	/// The code's recovery key.
	pub key: VerifyingKey,
	/// The encryption key that space keys are sealed to.
	pub encryption: EncryptionKey,
	/// The encryption key that secrets keys are sealed to.
	pub secrets_encryption: EncryptionKey,
	/// The recovery key's signature of the two encryption keys.
	pub signature: Signature,
}

impl RecoveryStatement {
	/// The statement of `code` for `member`, signed with the code's recovery key.
	#[must_use]
	pub fn sign(code: &RecoveryCode, member: EntityId) -> Self {
		let signing = code.signing_key();
		let encryption = code.decryption_key().encryption_key();
		let secrets_encryption = code.secrets_decryption_key().encryption_key();
		let signature = signing.sign(KEYS, &signed(&encryption, &secrets_encryption));
		Self {
			member,
			key: signing.verifying_key(),
			encryption,
			secrets_encryption,
			signature,
		}
	}

	/// The same code's statement for another member: the user's record in another space.
	#[must_use]
	pub fn for_member(&self, member: EntityId) -> Self {
		Self {
			member,
			..self.clone()
		}
	}

	/// Whether the recovery key signed the two encryption keys.
	#[must_use]
	pub fn verifies(&self) -> bool {
		self.key
			.verify(
				KEYS,
				&signed(&self.encryption, &self.secrets_encryption),
				&self.signature,
			)
			.is_ok()
	}

	/// The code's record in `space`: [`recovery_record`].
	#[must_use]
	pub fn record(&self, space: SpaceId) -> EntityId {
		recovery_record(space, self.member, &self.key)
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The statement that a text holds, if it holds one, whether it verifies or not.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// What a recovery key signs: the hashes of its code's two encryption keys.
fn signed(encryption: &EncryptionKey, secrets_encryption: &EncryptionKey) -> Vec<u8> {
	let hashes: [Hash; 2] = [
		encryption_hash(&encryption.to_bytes()),
		encryption_hash(&secrets_encryption.to_bytes()),
	];
	cbor::to_vec(&Cbor::Array(hashes.iter().map(ToCbor::to_cbor).collect()))
}

impl ToCbor for RecoveryStatement {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.member.to_cbor(),
			self.key.to_cbor(),
			self.encryption.to_cbor(),
			self.secrets_encryption.to_cbor(),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for RecoveryStatement {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a recovery code: [member, key, encryption, secrets encryption, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [member, key, encryption, secrets_encryption, signature] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			member: EntityId::from_cbor(member)?,
			key: VerifyingKey::from_cbor(key)?,
			encryption: EncryptionKey::from_cbor(encryption)?,
			secrets_encryption: EncryptionKey::from_cbor(secrets_encryption)?,
			signature: Signature::from_cbor(signature)?,
		})
	}
}

/// The operation that states a recovery code in `space`, on its record there. It governs the
/// space.
pub fn state_recovery_code(
	space: SpaceId,
	statement: &RecoveryStatement,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		statement.record(space),
		fields::RECOVERY_KEY,
		Value::Text(statement.to_text()),
	))
}

/// The personal space's entry for a recovery code, as the code signed it once, as it was
/// named: the space, the member the code is named for there, the ID of the space's first
/// change and its first key's hash, which a recovery file's entry for the code names. A
/// recovery takes what an entry names only where the code signed it, so a device that writes
/// a recovery file, as one updating a backup, can make it name no other member, first change
/// or first key. In text, the canonical encoding of `[space, member, root, key hash, recovery
/// key, signature]` in base64url without padding, where the signature, with the context
/// `nibkeep recovery entry`, is the recovery key's of the canonical encoding of `[space,
/// member, root, key hash]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEntry {
	/// The personal space.
	pub space: SpaceId,
	/// The member that the code is named for there: the user.
	pub member: EntityId,
	/// The ID of the space's first change, as a hash.
	pub root: Hash,
	/// The first key's hash.
	pub hash: Hash,
	/// The code's recovery key.
	pub key: VerifyingKey,
	/// The recovery key's signature of the rest.
	pub signature: Signature,
}

/// The context of a recovery key's signature of the personal space's entry for its code.
const ENTRIES: &str = "nibkeep recovery entry";

impl RecoveryEntry {
	/// The entry of `space`, whose first change is `root` and first key's hash `hash`, for
	/// `code` named for `member`, signed with the code's recovery key.
	#[must_use]
	pub fn sign(
		code: &RecoveryCode,
		(space, member, root): (SpaceId, EntityId, Hash),
		hash: Hash,
	) -> Self {
		let signing = code.signing_key();
		let signature = signing.sign(ENTRIES, &entry_signed((space, member, root), hash));
		Self {
			space,
			member,
			root,
			hash,
			key: signing.verifying_key(),
			signature,
		}
	}

	/// Whether the recovery key signed it.
	#[must_use]
	pub fn verifies(&self) -> bool {
		self.key
			.verify(
				ENTRIES,
				&entry_signed((self.space, self.member, self.root), self.hash),
				&self.signature,
			)
			.is_ok()
	}

	/// The code's record in the space, for the member: [`recovery_record`].
	#[must_use]
	pub fn record(&self) -> EntityId {
		recovery_record(self.space, self.member, &self.key)
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The entry that a text holds, if it holds one, whether it verifies or not.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// What a recovery key signs of the personal space's entry for its code.
fn entry_signed((space, member, root): (SpaceId, EntityId, Hash), hash: Hash) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![
		space.to_cbor(),
		member.to_cbor(),
		root.to_cbor(),
		hash.to_cbor(),
	]))
}

impl ToCbor for RecoveryEntry {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.member.to_cbor(),
			self.root.to_cbor(),
			self.hash.to_cbor(),
			self.key.to_cbor(),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for RecoveryEntry {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a recovery entry: [space, member, root, key hash, recovery key, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, member, root, hash, key, signature] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			member: EntityId::from_cbor(member)?,
			root: Hash::from_cbor(root)?,
			hash: Hash::from_cbor(hash)?,
			key: VerifyingKey::from_cbor(key)?,
			signature: Signature::from_cbor(signature)?,
		})
	}
}

/// The operation that states the personal space's entry for a recovery code, signed by the
/// code, on the code's record there. It governs the space, in the change that names the code.
pub fn state_recovery_entry(entry: &RecoveryEntry, rng: &mut impl CryptoRng) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		entry.record(),
		fields::RECOVERY_ENTRY,
		Value::Text(entry.to_text()),
	))
}

/// The operation that revokes a recovery code on its record `record`, naming its recovery
/// key. It governs the space, and a revocation is final.
pub fn revoke_recovery_code(
	record: EntityId,
	key: &VerifyingKey,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		record,
		fields::RECOVERY_REVOKED,
		Value::Text(key.to_text()),
	))
}

/// A new replica's listing, signed by a recovery code of its owner's: by it, a device lists
/// its own key, which nothing else listed, after its user lost their other devices. It names
/// the space, the record it lists the key on, the record's owner, the hash of the replica's
/// encryption key and its verifying key, so that the code's signature says nothing of
/// another space, record, owner or key. In text, the canonical encoding of `[space, record,
/// owner, encryption, key, recovery key, signature]` in base64url without padding, where the
/// signature, with the context `nibkeep recovery listing`, is the recovery key's of the
/// canonical encoding of `[space, record, owner, encryption, key]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryListing {
	/// The space.
	pub space: SpaceId,
	/// The record that the key is listed on.
	pub record: EntityId,
	/// The record's owner: the member whose code it is.
	pub owner: EntityId,
	/// The hash of the replica's encryption key: [`encryption_hash`].
	pub encryption: Hash,
	/// The replica's verifying key.
	pub key: VerifyingKey,
	/// The code's recovery key.
	pub recovery_key: VerifyingKey,
	/// The recovery key's signature of the rest.
	pub signature: Signature,
}

/// The context of a recovery key's signature of a replica's listing.
const LISTINGS: &str = "nibkeep recovery listing";

impl RecoveryListing {
	/// The listing, signed with `code`, of the replica whose verifying key is `key` and
	/// encryption key `encryption`, on the record `record` of `space`, owned by `owner`.
	#[must_use]
	pub fn sign(
		code: &RecoveryCode,
		(space, record, owner): (SpaceId, EntityId, EntityId),
		key: &VerifyingKey,
		encryption: &EncryptionKey,
	) -> Self {
		let signing = code.signing_key();
		let encryption = encryption_hash(&encryption.to_bytes());
		let signature = signing.sign(
			LISTINGS,
			&listing_signed((space, record, owner), encryption, key),
		);
		Self {
			space,
			record,
			owner,
			encryption,
			key: *key,
			recovery_key: signing.verifying_key(),
			signature,
		}
	}

	/// Whether the recovery key signed it.
	#[must_use]
	pub fn verifies(&self) -> bool {
		self.recovery_key
			.verify(
				LISTINGS,
				&listing_signed(
					(self.space, self.record, self.owner),
					self.encryption,
					&self.key,
				),
				&self.signature,
			)
			.is_ok()
	}

	/// The code's record in the space, for the listing's owner: [`recovery_record`].
	#[must_use]
	pub fn code_record(&self) -> EntityId {
		recovery_record(self.space, self.owner, &self.recovery_key)
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

/// What a recovery key signs of a listing.
fn listing_signed(
	(space, record, owner): (SpaceId, EntityId, EntityId),
	encryption: Hash,
	key: &VerifyingKey,
) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![
		space.to_cbor(),
		record.to_cbor(),
		owner.to_cbor(),
		encryption.to_cbor(),
		key.to_cbor(),
	]))
}

impl ToCbor for RecoveryListing {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.record.to_cbor(),
			self.owner.to_cbor(),
			self.encryption.to_cbor(),
			self.key.to_cbor(),
			self.recovery_key.to_cbor(),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for RecoveryListing {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a recovery listing: [space, record, owner, encryption, key, recovery key, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [
			space,
			record,
			owner,
			encryption,
			key,
			recovery_key,
			signature,
		] = items.as_slice()
		else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			record: EntityId::from_cbor(record)?,
			owner: EntityId::from_cbor(owner)?,
			encryption: Hash::from_cbor(encryption)?,
			key: VerifyingKey::from_cbor(key)?,
			recovery_key: VerifyingKey::from_cbor(recovery_key)?,
			signature: Signature::from_cbor(signature)?,
		})
	}
}

/// The operation that states a recovery listing, on the record it lists the key on. It
/// governs the space, in the change that lists the key, by the key itself.
pub fn list_by_recovery_code(listing: &RecoveryListing, rng: &mut impl CryptoRng) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		listing.record,
		fields::RECOVERY_LISTING,
		Value::Text(listing.to_text()),
	))
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::SigningKey;
	use nibkeep_testkit::TestRng;

	use super::*;

	fn ids(rng: &mut TestRng) -> (SpaceId, EntityId) {
		(SpaceId::random(rng), EntityId::random(rng))
	}

	#[test]
	fn a_statement_verifies_and_survives_its_text_form() {
		let mut rng = TestRng::new("a recovery statement");
		let code = RecoveryCode::generate(&mut rng);
		let (_, member) = ids(&mut rng);
		let statement = RecoveryStatement::sign(&code, member);
		assert!(statement.verifies());
		assert_eq!(statement.key, code.signing_key().verifying_key());
		assert_eq!(
			RecoveryStatement::from_text(&statement.to_text()),
			Some(statement)
		);
		assert_eq!(RecoveryStatement::from_text("not one"), None);
	}

	#[test]
	fn no_other_encryption_key_passes_for_the_codes() {
		let mut rng = TestRng::new("another encryption key");
		let code = RecoveryCode::generate(&mut rng);
		let other = RecoveryCode::generate(&mut rng);
		let (_, member) = ids(&mut rng);
		let statement = RecoveryStatement::sign(&code, member);
		let other = RecoveryStatement::sign(&other, member);
		// A replica that puts a key of its own in a code's statement has nothing sealed to it.
		let mut planted = statement.clone();
		planted.encryption = other.encryption.clone();
		assert!(!planted.verifies());
		let mut planted = statement.clone();
		planted.secrets_encryption = other.secrets_encryption.clone();
		assert!(!planted.verifies());
		// Nor do the two keys stand in for each other.
		let mut swapped = statement.clone();
		std::mem::swap(&mut swapped.encryption, &mut swapped.secrets_encryption);
		assert!(!swapped.verifies());
		// Another code's signature says nothing of this one's keys.
		let mut signed_by_other = statement;
		signed_by_other.signature = other.signature;
		assert!(!signed_by_other.verifies());
	}

	#[test]
	fn the_signature_names_no_member_so_one_code_serves_every_space() {
		let mut rng = TestRng::new("every space");
		let code = RecoveryCode::generate(&mut rng);
		let (personal, user) = ids(&mut rng);
		let (family, member) = ids(&mut rng);
		let statement = RecoveryStatement::sign(&code, user);
		let carried = statement.for_member(member);
		assert!(carried.verifies());
		assert_eq!(carried.signature, statement.signature);
		// Each space and member has a record of its own for the code.
		let records = [
			statement.record(personal),
			statement.record(family),
			carried.record(personal),
			carried.record(family),
		];
		for (index, record) in records.iter().enumerate() {
			assert!(!records[index + 1..].contains(record));
		}
		assert_eq!(
			carried.record(family),
			recovery_record(family, member, &code.signing_key().verifying_key())
		);
	}

	#[test]
	fn a_listing_by_a_code_verifies_for_what_it_names_alone() {
		let mut rng = TestRng::new("a recovery listing");
		let code = RecoveryCode::generate(&mut rng);
		let (space, owner) = ids(&mut rng);
		let record = EntityId::random(&mut rng);
		let key = SigningKey::generate(&mut rng).verifying_key();
		let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
		let listing = RecoveryListing::sign(&code, (space, record, owner), &key, &encryption);
		assert!(listing.verifies());
		assert_eq!(listing.recovery_key, code.signing_key().verifying_key());
		assert_eq!(
			listing.code_record(),
			RecoveryStatement::sign(&code, owner).record(space)
		);
		assert_eq!(
			RecoveryListing::from_text(&listing.to_text()),
			Some(listing.clone())
		);
		// The code's signature says nothing of another space, record, owner or key.
		let other_key = SigningKey::generate(&mut rng).verifying_key();
		let other_space = SpaceId::random(&mut rng);
		let other_entity = EntityId::random(&mut rng);
		let changed: [&dyn Fn(&mut RecoveryListing); 5] = [
			&|listing| listing.space = other_space,
			&|listing| listing.record = other_entity,
			&|listing| listing.owner = other_entity,
			&|listing| listing.key = other_key,
			&|listing| listing.encryption = Hash::of(b"another key"),
		];
		for change in changed {
			let mut forged = listing.clone();
			change(&mut forged);
			assert!(!forged.verifies());
		}
		// Nor does another code's key pass for the code's.
		let mut forged = listing;
		forged.recovery_key = RecoveryCode::generate(&mut rng)
			.signing_key()
			.verifying_key();
		assert!(!forged.verifies());
	}

	#[test]
	fn the_personal_spaces_entry_verifies_for_what_the_code_signed_alone() {
		let mut rng = TestRng::new("a recovery entry");
		let code = RecoveryCode::generate(&mut rng);
		let (space, member) = ids(&mut rng);
		let root = Hash::of(b"the first change");
		let hash = Hash::of(b"the first key");
		let entry = RecoveryEntry::sign(&code, (space, member, root), hash);
		assert!(entry.verifies());
		assert_eq!(
			entry.record(),
			RecoveryStatement::sign(&code, member).record(space)
		);
		assert_eq!(
			RecoveryEntry::from_text(&entry.to_text()),
			Some(entry.clone())
		);
		assert_eq!(RecoveryEntry::from_text("not one"), None);
		// A device that writes a recovery file can name no other space, member, first change
		// or first key, as the code signed what the entry names.
		let other_space = SpaceId::random(&mut rng);
		let other_member = EntityId::random(&mut rng);
		let changed: [&dyn Fn(&mut RecoveryEntry); 4] = [
			&|entry| entry.space = other_space,
			&|entry| entry.member = other_member,
			&|entry| entry.root = Hash::of(b"another first change"),
			&|entry| entry.hash = Hash::of(b"another first key"),
		];
		for change in changed {
			let mut forged = entry.clone();
			change(&mut forged);
			assert!(!forged.verifies());
		}
		// Nor does another code's key pass for the code's, nor another signature of its own.
		let other = RecoveryCode::generate(&mut rng);
		let mut forged = entry.clone();
		forged.key = other.signing_key().verifying_key();
		assert!(!forged.verifies());
		let mut forged = entry.clone();
		forged.signature = RecoveryStatement::sign(&code, member).signature;
		assert!(!forged.verifies());
		let Operation::Assert {
			statement: asserted,
			..
		} = state_recovery_entry(&entry, &mut rng)
		else {
			panic!("not an assertion");
		};
		assert_eq!(asserted.entity, entry.record());
		assert_eq!(asserted.field, fields::RECOVERY_ENTRY);
	}

	#[test]
	fn stating_a_code_asserts_it_on_its_record() {
		let mut rng = TestRng::new("stating a code");
		let code = RecoveryCode::generate(&mut rng);
		let (space, member) = ids(&mut rng);
		let statement = RecoveryStatement::sign(&code, member);
		let Operation::Assert {
			statement: asserted,
			supersedes,
			..
		} = state_recovery_code(space, &statement, &mut rng)
		else {
			panic!("not an assertion");
		};
		assert!(supersedes.is_empty());
		assert_eq!(asserted.entity, statement.record(space));
		assert_eq!(asserted.field, fields::RECOVERY_KEY);
		assert_eq!(
			asserted
				.value
				.as_text()
				.and_then(RecoveryStatement::from_text),
			Some(statement)
		);
	}
}
