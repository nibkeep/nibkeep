//! Spaces of one's own besides the personal one: founding one, the personal space's entries
//! that give each of the user's replicas a space's first key, and listing a replica of the
//! user in a space, with the listing that its own key signed
//! ([Identity](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/identity.md#8-spaces-of-ones-own),
//! [technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, Key, SealedMessage, VerifyingKey};
use nibkeep_model::{FieldName, Name, Operation, Statement, Value, fields};

use crate::pairing::space_key_data;
use crate::{
	DEVICE, ENCRYPTION_KEY, GROUP, NAME, ON_DEVICE, OWNER, REPLICA, SIGNING_KEY, SPACE, USER,
	encryption_key_text, endpoint_text,
};

/// The context with which a space's first key is sealed to a replica in a space entry.
const FIRST_KEYS: &str = "nibkeep 2026-11 first keys";

/// The role of a space's creator, its first admin.
pub const ADMIN: &str = "admin";

/// What a group's first change states as its kind of space, in `org.nibkeep.space-kind`.
pub const GROUP_KIND: &str = "group";

/// A space of the user's, as a statement of [`fields::SPACE_ENTRY`] on a replica's record in
/// the personal space holds it: the space, the user's member record there, the ID of the
/// space's first change, and the space's first key sealed to the replica's encryption key.
/// In text, the canonical encoding of `[space, member, root, key hash, sealed key]` in
/// base64url without padding, the key sealed with the context `nibkeep 2026-11 first keys`
/// and the associated data `[space, 0, replica]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEntry {
	/// The space.
	pub space: SpaceId,
	/// The user's member record in the space, which the replica's listing there names as its
	/// owner.
	pub member: EntityId,
	/// The ID of the space's first change, as a hash: the only change that the replica takes
	/// as the space's first.
	pub root: Hash,
	/// The first key's hash, as the hash of a change key is computed.
	pub hash: Hash,
	/// The first key, sealed to the replica.
	pub sealed: SealedMessage,
}

impl SpaceEntry {
	/// Seals the first key `key` of `space`, whose hash is `hash`, to `recipient`, the
	/// encryption key of `replica`, with the user's member record `member` there.
	pub fn seal(
		recipient: &EncryptionKey,
		(space, member, root): (SpaceId, EntityId, Hash),
		replica: EntityId,
		(key, hash): (&Key, Hash),
		rng: &mut impl CryptoRng,
	) -> Self {
		let data = space_key_data(space, 0, replica);
		Self {
			space,
			member,
			root,
			hash,
			sealed: SealedMessage::seal_key(recipient, key, FIRST_KEYS, &data, rng),
		}
	}

	/// The first key, if it opens with `decryption`, the key of `replica` that it was sealed
	/// to. Whether its hash is the one named is for the caller to check.
	#[must_use]
	pub fn open(&self, decryption: &DecryptionKey, replica: EntityId) -> Option<Key> {
		let data = space_key_data(self.space, 0, replica);
		self.sealed.open_key(decryption, FIRST_KEYS, &data).ok()
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The entry that a text holds, if it holds one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

impl ToCbor for SpaceEntry {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.member.to_cbor(),
			self.root.to_cbor(),
			self.hash.to_cbor(),
			self.sealed.to_cbor(),
		])
	}
}

impl FromCbor for SpaceEntry {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a space entry: [space, member, root, key hash, sealed key]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, member, root, hash, sealed] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			member: EntityId::from_cbor(member)?,
			root: Hash::from_cbor(root)?,
			hash: Hash::from_cbor(hash)?,
			sealed: SealedMessage::from_cbor(sealed)?,
		})
	}
}

/// The statement that gives a replica a space of the user's, on its record in the personal
/// space.
pub fn give_space_entry(
	replica: EntityId,
	entry: &SpaceEntry,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert_one(
		replica,
		fields::SPACE_ENTRY,
		Value::Text(entry.to_text()),
		rng,
	)
}

/// The operation that states a device's choice of a space on `record`: its own record in
/// the personal space, or its record in the space, derived from the space and its key
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
pub fn state_choice(
	record: EntityId,
	choice: &crate::Choice,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert_one(
		record,
		fields::SPACE_CHOICE,
		Value::Text(choice.to_text()),
		rng,
	)
}

/// The statement by which a replica asks to be listed in a space, on its own record in the
/// personal space: the listing that its key signed, as text.
pub fn ask_to_be_listed(replica: EntityId, listing: &str, rng: &mut impl CryptoRng) -> Operation {
	assert_one(
		replica,
		fields::LISTING,
		Value::Text(listing.to_owned()),
		rng,
	)
}

/// A replica of the user's in a space: its record there, derived from the space and its key,
/// its keys, the listing its key signed, the member it belongs to, where it connects from,
/// and the names of its device and its app.
#[derive(Clone, Debug)]
pub struct SpaceReplica<'a> {
	/// Its record in the space.
	pub record: EntityId,
	/// Its device's record in the space, derived from the space and its key as its record
	/// is, so that every replica knows whose device it is.
	pub device_record: EntityId,
	/// The member it belongs to.
	pub member: EntityId,
	/// Its verifying key.
	pub signing: &'a VerifyingKey,
	/// Its encryption key.
	pub encryption: &'a EncryptionKey,
	/// The listing that its key signed, as text.
	pub listing: &'a str,
	/// Its endpoint ID, if known.
	pub endpoint: Option<&'a [u8; 32]>,
	/// The name of its device.
	pub device: &'a str,
	/// The name of its app.
	pub app: &'a str,
	/// Whether it is an AI agent's, as its listing says too.
	pub ai_agent: bool,
}

/// What listing a replica in a space creates.
#[derive(Clone, Debug)]
pub struct Listed {
	/// Its device's record in the space.
	pub device: EntityId,
	/// The operations of a change that governs the space: its key, its encryption key, its
	/// owner and its listing.
	pub governing: Vec<Operation>,
	/// The operations of a change after it: its type, name, device and endpoint, and its
	/// device with its type and name.
	pub operations: Vec<Operation>,
}

/// Lists a replica of the user in a space, as the identity specification says.
pub fn list_replica(replica: &SpaceReplica<'_>, rng: &mut impl CryptoRng) -> Listed {
	let device = replica.device_record;
	let record = replica.record;
	let mut governing = assert_all(
		vec![
			(record, SIGNING_KEY, Value::Text(replica.signing.to_text())),
			(
				record,
				ENCRYPTION_KEY,
				Value::Text(encryption_key_text(replica.encryption)),
			),
			(record, OWNER, Value::Reference(replica.member)),
			(
				record,
				fields::LISTING,
				Value::Text(replica.listing.to_owned()),
			),
		],
		rng,
	);
	if replica.ai_agent {
		governing.push(assert_one(
			record,
			fields::AI_AGENT,
			Value::Boolean(true),
			rng,
		));
	}
	let mut statements = vec![
		(record, fields::TYPE, Value::Text(REPLICA.to_string())),
		(record, NAME, Value::Name(Name::new(replica.app))),
		(record, ON_DEVICE, Value::Reference(device)),
		(device, fields::TYPE, Value::Text(DEVICE.to_string())),
		(device, NAME, Value::Name(Name::new(replica.device))),
	];
	if let Some(endpoint) = replica.endpoint {
		statements.push((
			record,
			fields::ENDPOINT,
			Value::Text(endpoint_text(endpoint)),
		));
	}
	Listed {
		device,
		governing,
		operations: assert_all(statements, rng),
	}
}

/// A space that a user founds besides the personal one.
#[derive(Clone, Debug)]
pub struct NewSpace<'a> {
	/// The space's name.
	pub name: &'a str,
	/// The user's ID, which the member record names.
	pub user: EntityId,
	/// The user's name.
	pub user_name: &'a str,
	/// The user's member record in the space.
	pub member: EntityId,
	/// The replica that founds it.
	pub replica: SpaceReplica<'a>,
	/// Whether it is a group: a space with members and no data of its own, whose own entity
	/// is of the type [`crate::GROUP`].
	pub group: bool,
}

/// What founding a space creates.
#[derive(Clone, Debug)]
pub struct FoundedSpace {
	/// The space's own entity.
	pub space: EntityId,
	/// The device of the replica that founded it.
	pub device: EntityId,
	/// The operations of the space's first change, which govern it: the user's member record
	/// with the user's ID and the role of admin, and the founding replica's listing.
	pub governing: Vec<Operation>,
	/// The operations of its second change: the member's type and name, the replica's and
	/// its device's records, and the space's own entity with its name.
	pub operations: Vec<Operation>,
}

/// The first two changes of a space that a user founds besides the personal one.
pub fn found_space(new: &NewSpace<'_>, rng: &mut impl CryptoRng) -> FoundedSpace {
	let space = EntityId::random(rng);
	let listed = list_replica(&new.replica, rng);
	let mut governing = assert_all(
		vec![
			(
				new.member,
				fields::USER_ID,
				Value::Text(new.user.to_string()),
			),
			(new.member, fields::ROLE, Value::Text(ADMIN.to_owned())),
		],
		rng,
	);
	governing.extend(listed.governing);
	// A group says so in its first change, which nobody but its founder writes.
	if new.group {
		governing.push(assert_one(
			space,
			fields::SPACE_KIND,
			Value::Text(GROUP_KIND.to_owned()),
			rng,
		));
	}
	let mut operations = assert_all(
		vec![
			(new.member, fields::TYPE, Value::Text(USER.to_string())),
			(new.member, NAME, Value::Name(Name::new(new.user_name))),
			(
				space,
				fields::TYPE,
				Value::Text(if new.group { GROUP } else { SPACE }.to_string()),
			),
			(space, NAME, Value::Name(Name::new(new.name))),
		],
		rng,
	);
	operations.extend(listed.operations);
	FoundedSpace {
		space,
		device: listed.device,
		governing,
		operations,
	}
}

/// A member that an admin adds to a space by an invitation: the user, by ID and name, the
/// member's new record, the role given, and the replica that joined.
#[derive(Clone, Debug)]
pub struct NewMember<'a> {
	/// The user's ID, which the member record names.
	pub user: EntityId,
	/// The user's name.
	pub user_name: &'a str,
	/// The member's record, new in the space.
	pub member: EntityId,
	/// The role given, as `org.nibkeep.role` names it.
	pub role: &'a str,
	/// The replica that joined.
	pub replica: SpaceReplica<'a>,
}

/// What adding a member creates.
#[derive(Clone, Debug)]
pub struct AddedMember {
	/// The device of the replica that joined.
	pub device: EntityId,
	/// The operations of a change that governs the space: the member's record with the
	/// user's ID and the role, and the replica's listing.
	pub governing: Vec<Operation>,
	/// The operations of the changes after it: the member's type and name, and the
	/// replica's and its device's records.
	pub operations: Vec<Operation>,
}

/// The changes by which an admin adds a member that an invitation brings, as the identity
/// specification says.
pub fn add_member(new: &NewMember<'_>, rng: &mut impl CryptoRng) -> AddedMember {
	let listed = list_replica(&new.replica, rng);
	let mut governing = assert_all(
		vec![
			(
				new.member,
				fields::USER_ID,
				Value::Text(new.user.to_string()),
			),
			(new.member, fields::ROLE, Value::Text(new.role.to_owned())),
		],
		rng,
	);
	governing.extend(listed.governing);
	let mut operations = assert_all(
		vec![
			(new.member, fields::TYPE, Value::Text(USER.to_string())),
			(new.member, NAME, Value::Name(Name::new(new.user_name))),
		],
		rng,
	);
	operations.extend(listed.operations);
	AddedMember {
		device: listed.device,
		governing,
		operations,
	}
}

/// A member's record that an admin makes in a space for a member through a group, who becomes
/// a direct member once their replicas list themselves on it.
#[derive(Clone, Debug)]
pub struct MemberFor<'a> {
	/// The new member's record.
	pub member: EntityId,
	/// The user's ID, as their record in the group names it.
	pub user: EntityId,
	/// Their name there, if they have one.
	pub user_name: Option<&'a str>,
	/// The role given, as `org.nibkeep.role` names it.
	pub role: &'a str,
	/// The key of one of their replicas in the group, which the record is made for.
	pub key: &'a VerifyingKey,
}

/// The changes by which an admin makes a member through a group a direct member, as the
/// identity specification says: the operations of a change that governs the space, the
/// record with the user's ID, the role and the key it is made for; and those of the change
/// after it, the record's type and name.
pub fn make_member_for(
	new: &MemberFor<'_>,
	rng: &mut impl CryptoRng,
) -> (Vec<Operation>, Vec<Operation>) {
	let governing = assert_all(
		vec![
			(
				new.member,
				fields::USER_ID,
				Value::Text(new.user.to_string()),
			),
			(new.member, fields::ROLE, Value::Text(new.role.to_owned())),
			(new.member, fields::MADE_FOR, Value::Text(new.key.to_text())),
		],
		rng,
	);
	let mut statements = vec![(new.member, fields::TYPE, Value::Text(USER.to_string()))];
	if let Some(name) = new.user_name {
		statements.push((new.member, NAME, Value::Name(Name::new(name))));
	}
	(governing, assert_all(statements, rng))
}

/// The statement that renames a space, on its own entity, replacing its names now,
/// `current`.
pub fn rename_space(
	space: EntityId,
	name: &str,
	current: impl IntoIterator<Item = StatementId>,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::replace(
		Statement::new(
			StatementId::random(rng),
			space,
			NAME,
			Value::Name(Name::new(name)),
		),
		current,
	)
}

fn assert_one(
	entity: EntityId,
	field: FieldName,
	value: Value,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		entity,
		field,
		value,
	))
}

fn assert_all(
	statements: Vec<(EntityId, FieldName, Value)>,
	rng: &mut impl CryptoRng,
) -> Vec<Operation> {
	statements
		.into_iter()
		.map(|(entity, field, value)| assert_one(entity, field, value, rng))
		.collect()
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::DecryptionKey;
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn a_space_entry_opens_for_its_replica_alone() {
		let mut rng = TestRng::new("space entry");
		let decryption = DecryptionKey::generate(&mut rng);
		let (space, member, replica) = (rng.space_id(), rng.entity_id(), rng.entity_id());
		let key = Key::generate(&mut rng);
		let hash = Hash::of(b"a key's hash");
		let entry = SpaceEntry::seal(
			&decryption.encryption_key(),
			(space, member, Hash::of(b"the first change")),
			replica,
			(&key, hash),
			&mut rng,
		);
		let again = SpaceEntry::from_text(&entry.to_text()).expect("reads");
		assert_eq!(again, entry);
		assert_eq!(
			again
				.open(&decryption, replica)
				.map(|key| *key.expose_bytes()),
			Some(*key.expose_bytes())
		);
		assert!(again.open(&decryption, rng.entity_id()).is_none());
		let other = DecryptionKey::generate(&mut rng);
		assert!(again.open(&other, replica).is_none());
	}
}
