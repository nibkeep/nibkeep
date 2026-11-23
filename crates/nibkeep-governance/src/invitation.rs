//! Inviting a user to a space, over a connection between the admin's replica and the
//! joiner's: what the joiner asks with, what the admin offers once the person confirms who
//! asks, and what it grants once the joiner signed its listing
//! ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#7-invitations), [technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{EncryptionKey, VerifyingKey};

use crate::{MAX_NAME_LEN, SpaceEntry};

/// What a joiner asks with: its user, by ID and name, its replica's keys, and the names of
/// its device and app. In CBOR, `[user, user name, signing key, encryption key, device, app]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitationRequest {
	/// The joiner's user.
	pub user: EntityId,
	/// The user's name.
	pub user_name: String,
	/// The joiner's verifying key.
	pub signing: VerifyingKey,
	/// The joiner's encryption key, which the space's first key is sealed to.
	pub encryption: EncryptionKey,
	/// The name of its device.
	pub device: String,
	/// The name of its app.
	pub app: String,
}

impl ToCbor for InvitationRequest {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![
			self.user.to_cbor(),
			self.user_name.to_cbor(),
			self.signing.to_cbor(),
			self.encryption.to_cbor(),
			self.device.to_cbor(),
			self.app.to_cbor(),
		])
	}
}

impl FromCbor for InvitationRequest {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a request: [user, user name, signing key, encryption key, device, app]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [user, user_name, signing, encryption, device, app] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			user: EntityId::from_cbor(user)?,
			user_name: name(user_name, EXPECTED)?,
			signing: VerifyingKey::from_cbor(signing)?,
			encryption: EncryptionKey::from_cbor(encryption)?,
			device: name(device, EXPECTED)?,
			app: name(app, EXPECTED)?,
		})
	}
}

/// A name that a message holds: not empty, and at most [`MAX_NAME_LEN`] bytes long.
fn name(value: &Value, expected: &'static str) -> Result<String, cbor::Error> {
	let name = String::from_cbor(value)?;
	if name.is_empty() || name.len() > MAX_NAME_LEN {
		return Err(cbor::Error::Expected(expected));
	}
	Ok(name)
}

/// What an admin offers a joiner once the person confirmed who asks: the space, by ID and
/// name, the ID of the member record it will make, the role it will give, and the person in
/// the space whom the admin names the user, if any, by ID and name. In CBOR,
/// `[space, space name, member, role]`, or `[space, space name, member, role, person,
/// person name]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
	/// The space.
	pub space: SpaceId,
	/// The space's name.
	pub space_name: String,
	/// The member record that the admin will make for the joiner's user.
	pub member: EntityId,
	/// The role it will give, as `org.nibkeep.role` names it.
	pub role: String,
	/// The person in the space whom the admin names the user, by ID and name, if any.
	pub person: Option<(EntityId, String)>,
}

impl ToCbor for Offer {
	fn to_cbor(&self) -> Value {
		let mut items = vec![
			self.space.to_cbor(),
			self.space_name.to_cbor(),
			self.member.to_cbor(),
			self.role.to_cbor(),
		];
		if let Some((person, name)) = &self.person {
			items.extend([person.to_cbor(), name.to_cbor()]);
		}
		Value::Array(items)
	}
}

impl FromCbor for Offer {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "an offer: [space, space name, member, role, person?, person name?]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let (space, space_name, member, role, person) = match items.as_slice() {
			[space, space_name, member, role] => (space, space_name, member, role, None),
			[space, space_name, member, role, person, person_name] => (
				space,
				space_name,
				member,
				role,
				Some((EntityId::from_cbor(person)?, name(person_name, EXPECTED)?)),
			),
			_ => return Err(cbor::Error::Expected(EXPECTED)),
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			space_name: name(space_name, EXPECTED)?,
			member: EntityId::from_cbor(member)?,
			role: name(role, EXPECTED)?,
			person,
		})
	}
}

/// What an admin grants a joiner whose listing it checked: the space's entry, with its first
/// key sealed to the joiner, and the admin's verifying key, which the joiner trusts for its
/// first sync of the space. In CBOR, `[entry, admin key]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitationGrant {
	/// The space, its member record for the joiner, its first change, and its first key
	/// sealed to the joiner's encryption key, as an entry of the personal space has them,
	/// with the joiner's record in the space as the replica it is sealed to.
	pub entry: SpaceEntry,
	/// The admin's verifying key.
	pub admin: VerifyingKey,
}

impl ToCbor for InvitationGrant {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![self.entry.to_cbor(), self.admin.to_cbor()])
	}
}

impl FromCbor for InvitationGrant {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a grant: [entry, admin key]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [entry, admin] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			entry: SpaceEntry::from_cbor(entry)?,
			admin: VerifyingKey::from_cbor(admin)?,
		})
	}
}
