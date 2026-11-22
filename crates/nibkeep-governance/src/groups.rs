//! Groups ([technical design: groups]): a group is a space with members and no data of its
//! own, whose own entity is of the type [`crate::GROUP`]. An admin of a space who is a member
//! of a group adds the group there, with a record that names it, and asks in the group that
//! it join; an admin of the group agrees; and an anchor in the space names the agreement,
//! giving the space the group's first key, so that its replicas can read the group's log.
//!
//! A space follows the governing changes of each group in it, which take effect there at
//! the space's governing changes that name them, its *anchors*
//! ([Log](../../../docs/spec/log.md#9-groups)). Each value that spaces and groups state of
//! each other is written in text as the canonical encoding of its items in base64url without
//! padding, as a [`crate::Listing`] is.
//!
//! [technical design: groups]: ../../../docs/Technical-design.md#138-groups

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, Key, SealedMessage};
use nibkeep_model::{Operation, Statement, Value, fields};

/// The context with which a space's name is sealed to a group's admins in a request.
const SPACE_NAMES: &str = "nibkeep 2026-11 space names";

/// The group that a member's record in a space is, as `org.nibkeep.group` states it there
/// when an admin adds the group: the group's ID, the ID of its first change, so that the
/// space's replicas take no other log for it, and the hash of its first key, computed as the
/// hash of a change key is, so that they take no other key for it. `[group, root, key]`.
/// The statement that states it is the group's *addition*, which the group's agreement
/// names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupRef {
	/// The group.
	pub group: SpaceId,
	/// The group's first change.
	pub root: ChangeId,
	/// The hash of the group's first key.
	pub key: Hash,
}

/// The changes of a group that a space's governing change names, as `org.nibkeep.anchor`
/// states it: the group's changes take effect in the space at the anchors that name them,
/// which then build on them. `[group, changes]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
	/// The group.
	pub group: SpaceId,
	/// The group's changes that it names: its governing changes that no other of them builds
	/// on, as the replica that writes it holds them.
	pub changes: Vec<ChangeId>,
}

/// What a group's agreement to join a space names, as `org.nibkeep.joined` states it in the
/// group, and its withdrawal, as `org.nibkeep.withdrawn` does: the space, the space's first
/// change, and the statement there that adds the group. `[space, root, addition]`. An
/// agreement for another first change, or another addition, brings the group into no space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Joining {
	/// The space.
	pub space: SpaceId,
	/// The space's first change.
	pub root: ChangeId,
	/// The statement in the space that adds the group.
	pub addition: StatementId,
}

/// Text forms of the values above.
macro_rules! text_form {
	($name:ident) => {
		impl $name {
			/// Its text form.
			#[must_use]
			pub fn to_text(&self) -> String {
				Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
			}

			/// The value that a text holds, if it holds one.
			#[must_use]
			pub fn from_text(text: &str) -> Option<Self> {
				let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
				cbor::from_slice(&bytes).ok()
			}
		}
	};
}

text_form!(GroupRef);
text_form!(Anchor);
text_form!(Joining);

impl ToCbor for GroupRef {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.group.to_cbor(),
			self.root.to_cbor(),
			self.key.to_cbor(),
		])
	}
}

impl FromCbor for GroupRef {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a group: [group, root, key]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [group, root, key] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			group: SpaceId::from_cbor(group)?,
			root: ChangeId::from_cbor(root)?,
			key: Hash::from_cbor(key)?,
		})
	}
}

impl ToCbor for Anchor {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![self.group.to_cbor(), self.changes.to_cbor()])
	}
}

impl FromCbor for Anchor {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "an anchor: [group, changes]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [group, changes] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			group: SpaceId::from_cbor(group)?,
			changes: Vec::from_cbor(changes)?,
		})
	}
}

impl ToCbor for Joining {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.root.to_cbor(),
			self.addition.to_cbor(),
		])
	}
}

impl FromCbor for Joining {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a joining: [space, root, addition]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, root, addition] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			root: ChangeId::from_cbor(root)?,
			addition: StatementId::from_cbor(addition)?,
		})
	}
}

/// A group's first key, as `org.nibkeep.group-key` gives it to a space that the group is
/// in, in the change that anchors the group's agreement, which the space's key wraps:
/// `[group, root, key]`, the key's 32 bytes, whose hash a record of the group in the space
/// names. Every replica of the space then reads the
/// group's log, whose governing changes that key wraps.
#[derive(Clone)]
pub struct GroupKey {
	/// The group and its first change.
	pub group: GroupRef,
	/// The group's first key.
	pub key: Key,
}

impl std::fmt::Debug for GroupKey {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("GroupKey")
			.field("group", &self.group)
			.finish_non_exhaustive()
	}
}

impl GroupKey {
	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The group key that a text holds, if it holds one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

impl ToCbor for GroupKey {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.group.group.to_cbor(),
			self.group.root.to_cbor(),
			Cbor::Bytes(self.key.expose_bytes().to_vec()),
		])
	}
}

impl FromCbor for GroupKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a group key: [group, root, key]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [group, root, Cbor::Bytes(key)] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let key = Key::from_slice(key).ok_or(cbor::Error::Expected(EXPECTED))?;
		Ok(Self {
			// The group as a record of it names it: its hash is the key's own.
			group: GroupRef {
				group: SpaceId::from_cbor(group)?,
				root: ChangeId::from_cbor(root)?,
				key: nibkeep_crypto::key_hash(&key),
			},
			key,
		})
	}
}

/// A member's request, in a group, as `org.nibkeep.join-request` states it, that the group
/// join a space: the joining that an agreement would name, and the space's name sealed to
/// each replica of the group's admins, so that no other reader of the group learns it.
/// `[space, root, addition, [[replica, sealed name], ...]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupRequest {
	/// The space, its first change, and the statement there that adds the group.
	pub joining: Joining,
	/// The space's name, sealed to each of the group's admins' replicas, by their records in
	/// the group.
	pub names: Vec<(EntityId, SealedMessage)>,
}

impl GroupRequest {
	/// The request that `joining` names, with the space's name sealed to each of `admins`,
	/// replicas' records in the group with their encryption keys.
	pub fn new(
		joining: Joining,
		name: &str,
		admins: &[(EntityId, EncryptionKey)],
		rng: &mut impl CryptoRng,
	) -> Self {
		let names = admins
			.iter()
			.map(|(record, key)| {
				let data = name_data(joining.space, *record);
				let sealed = SealedMessage::seal(key, name.as_bytes(), SPACE_NAMES, &data, rng);
				(*record, sealed)
			})
			.collect();
		Self { joining, names }
	}

	/// The space's name, if one is sealed to `record`'s replica and opens with its
	/// decryption key.
	#[must_use]
	pub fn name_for(&self, record: EntityId, decryption: &DecryptionKey) -> Option<String> {
		let data = name_data(self.joining.space, record);
		self.names
			.iter()
			.filter(|(to, _)| *to == record)
			.find_map(|(_, sealed)| sealed.open(decryption, SPACE_NAMES, &data).ok())
			.and_then(|name| String::from_utf8(name.to_vec()).ok())
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The request that a text holds, if it holds one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// What a sealed name is bound to: the space and the record of the replica it is sealed to.
fn name_data(space: SpaceId, record: EntityId) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![space.to_cbor(), record.to_cbor()]))
}

impl ToCbor for GroupRequest {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.joining.space.to_cbor(),
			self.joining.root.to_cbor(),
			self.joining.addition.to_cbor(),
			Cbor::Array(
				self.names
					.iter()
					.map(|(record, sealed)| Cbor::Array(vec![record.to_cbor(), sealed.to_cbor()]))
					.collect(),
			),
		])
	}
}

impl FromCbor for GroupRequest {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a group request: [space, root, addition, names]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, root, addition, Cbor::Array(names)] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let names = names
			.iter()
			.map(|name| {
				let Cbor::Array(pair) = name else {
					return Err(cbor::Error::Expected(EXPECTED));
				};
				let [record, sealed] = pair.as_slice() else {
					return Err(cbor::Error::Expected(EXPECTED));
				};
				Ok((
					EntityId::from_cbor(record)?,
					SealedMessage::from_cbor(sealed)?,
				))
			})
			.collect::<Result<_, _>>()?;
		Ok(Self {
			joining: Joining {
				space: SpaceId::from_cbor(space)?,
				root: ChangeId::from_cbor(root)?,
				addition: StatementId::from_cbor(addition)?,
			},
			names,
		})
	}
}

/// The governing operations that add a group to a space with `role`, on a new record: the
/// group, with its first change, and the role. Returns them with the statement that adds the
/// group, which the group's agreement names.
pub fn add_group(
	record: EntityId,
	group: GroupRef,
	role: &str,
	rng: &mut impl CryptoRng,
) -> (Vec<Operation>, StatementId) {
	let addition = StatementId::random(rng);
	let operations = vec![
		Operation::assert(Statement::new(
			addition,
			record,
			fields::GROUP,
			Value::Text(group.to_text()),
		)),
		assert_one(record, fields::ROLE, Value::Text(role.to_owned()), rng),
	];
	(operations, addition)
}

/// The statement, in a group, of a member's request that it join a space, on the group's own
/// entity.
pub fn ask_to_join(
	entity: EntityId,
	request: &GroupRequest,
	rng: &mut impl CryptoRng,
) -> Operation {
	assert_one(
		entity,
		fields::JOIN_REQUEST,
		Value::Text(request.to_text()),
		rng,
	)
}

/// The statement, in a group, of an admin's agreement to join a space, on the group's own
/// entity.
pub fn agree_to_join(entity: EntityId, joining: &Joining, rng: &mut impl CryptoRng) -> Operation {
	assert_one(entity, fields::JOINED, Value::Text(joining.to_text()), rng)
}

/// The statement, in a group, of an admin's withdrawal of its agreement to join a space, for
/// good, on the group's own entity.
pub fn withdraw_from(entity: EntityId, joining: &Joining, rng: &mut impl CryptoRng) -> Operation {
	assert_one(
		entity,
		fields::WITHDRAWN,
		Value::Text(joining.to_text()),
		rng,
	)
}

/// The statements, in a space, of an anchor of a group's changes, on the group's record
/// there, with the group's first key if `key` gives it.
pub fn anchor(
	record: EntityId,
	anchor: &Anchor,
	key: Option<&GroupKey>,
	rng: &mut impl CryptoRng,
) -> Vec<Operation> {
	let mut operations = vec![assert_one(
		record,
		fields::ANCHOR,
		Value::Text(anchor.to_text()),
		rng,
	)];
	if let Some(key) = key {
		operations.push(assert_one(
			record,
			fields::GROUP_KEY,
			Value::Text(key.to_text()),
			rng,
		));
	}
	operations
}

fn assert_one(
	entity: EntityId,
	field: nibkeep_model::FieldName,
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

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn a_group_key_and_a_request_survive_their_text_forms() {
		let mut rng = TestRng::new("group texts");
		let first = Key::generate(&mut rng);
		let group = GroupRef {
			group: rng.space_id(),
			root: ChangeId::from_hash(Hash::of(b"the group's first change")),
			key: nibkeep_crypto::key_hash(&first),
		};
		let key = GroupKey { group, key: first };
		let again = GroupKey::from_text(&key.to_text()).expect("reads");
		assert_eq!(again.group, group);
		assert!(again.key.same_as(&key.key));
		let admin = DecryptionKey::generate(&mut rng);
		let other = DecryptionKey::generate(&mut rng);
		let (record, other_record) = (rng.entity_id(), rng.entity_id());
		let joining = Joining {
			space: rng.space_id(),
			root: group.root,
			addition: rng.statement_id(),
		};
		let request = GroupRequest::new(
			joining,
			"Family",
			&[(record, admin.encryption_key())],
			&mut rng,
		);
		let again = GroupRequest::from_text(&request.to_text()).expect("reads");
		assert_eq!(again, request);
		assert_eq!(again.name_for(record, &admin).as_deref(), Some("Family"));
		assert_eq!(again.name_for(other_record, &other), None);
		assert_eq!(again.name_for(record, &other), None);
	}

	#[test]
	fn each_value_survives_its_text_form() {
		let mut rng = TestRng::new("group values");
		let root = ChangeId::from_hash(Hash::of(b"a first change"));
		let group = GroupRef {
			group: rng.space_id(),
			root,
			key: Hash::of(b"a first key"),
		};
		assert_eq!(GroupRef::from_text(&group.to_text()), Some(group));
		let anchor = Anchor {
			group: group.group,
			changes: vec![root, ChangeId::from_hash(Hash::of(b"another"))],
		};
		assert_eq!(Anchor::from_text(&anchor.to_text()), Some(anchor));
		let joining = Joining {
			space: rng.space_id(),
			root,
			addition: rng.statement_id(),
		};
		assert_eq!(Joining::from_text(&joining.to_text()), Some(joining));
		assert_eq!(Joining::from_text(&group.to_text()), None);
		assert_eq!(GroupRef::from_text("not base64!"), None);
	}
}
