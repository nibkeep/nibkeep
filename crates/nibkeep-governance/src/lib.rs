//! Who a NibKeep library and its spaces belong to, and whose changes count in each space:
//! the governing statements that users, replicas, members, groups and claims are written
//! with; the [`rules`] that decide from them which changes count; and the [`Roster`] of each
//! space's replicas and their keys. A replica adds another by pairing with it.
//!
//! The formats are specified in `docs/spec/identity.md` and `docs/spec/log.md`, and pairing
//! in `docs/spec/sync.md`.

mod choice;
mod groups;
mod invitation;
mod listing;
mod pairing;
mod recovery;
mod revocation;
mod roster;
pub mod rules;
mod spaces;

pub use choice::Choice;
pub use groups::{
	Anchor, GroupKey, GroupRef, GroupRequest, Joining, add_group, agree_to_join, anchor,
	ask_to_join, withdraw_from,
};
pub use invitation::{InvitationGrant, InvitationRequest, Offer};
pub use listing::{Listing, device_record, encryption_hash, replica_record, space_names_record};
pub use recovery::{
	RecoveryEntry, RecoveryListing, RecoveryStatement, list_by_recovery_code, recovery_record,
	revoke_recovery_code, state_recovery_code, state_recovery_entry,
};

pub use revocation::{
	SealedSecretsKey, SealedSpaceKey, SecretsLink, accept, give_key, give_secrets_key, revoke,
	secrets_recipient, state_secrets_from, state_secrets_key,
};
pub use roster::{Code, Roster, Rotation, SecretsGift, SpaceGift};
pub use rules::{Claim, Followed, GoverningStatement, Role, SpaceGroup};

pub use spaces::{
	ADMIN, AddedMember, FoundedSpace, GROUP_KIND, Listed, MemberFor, NewMember, NewSpace,
	SpaceEntry, SpaceReplica, add_member, ask_to_be_listed, found_space, give_space_entry,
	list_replica, make_member_for, rename_space, state_choice,
};

pub use pairing::{
	Grant, INVITATION_PREFIX, JoinRequest, MAX_NAME_LEN, NotACode, PAIRING_PREFIX, Pairing,
	PairingCode, PairingKey, SealedSpaceKeys, Side, open_space_keys, seal_space_keys,
};

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{EncryptionKey, Key, SigningKey, VerifyingKey};
use nibkeep_model::storage::Statements;
use nibkeep_model::{
	FieldDef, FieldName, Languages, Name, Operation, State, Statement, TypeDef, TypeName, Value,
	ValueKind, fields,
};

/// The type of users: those who use NibKeep.
pub const USER: TypeName = TypeName::from_static("org.nibkeep.user");

/// The type of people: those described in the data, which the standard vocabulary defines
/// (`nibkeep-std`), as people record them; NibKeep names one for each user.
pub const PERSON: TypeName = TypeName::from_static("std.person");

/// The type of devices: phones and computers, which hold replicas.
pub const DEVICE: TypeName = TypeName::from_static("org.nibkeep.device");

/// The type of replicas: copies of a library, each with keys of its own, which sign the
/// changes they make.
pub const REPLICA: TypeName = TypeName::from_static("org.nibkeep.replica");

/// The type of a space's own entity.
pub const SPACE: TypeName = TypeName::from_static("org.nibkeep.space");

/// The type of a group's own entity, a space with members and no data of its own
/// ([technical design](../../../docs/Technical-design.md#138-groups)).
pub const GROUP: TypeName = TypeName::from_static("org.nibkeep.group");

/// The type of the record in the personal space that keeps the names of a space of the
/// user's, derived from the personal space and the space, so that every device of the user
/// shows the space by name, those that do not hold it too
/// ([technical design](../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
pub const SPACE_NAMES: TypeName = TypeName::from_static("org.nibkeep.space-names");

/// The space whose names a record of the type [`SPACE_NAMES`] keeps: its ID, as text.
pub const OF_SPACE: FieldName = FieldName::from_static("org.nibkeep.of-space");

/// The type of a recovery code's record: a member's printed code, which works as one more
/// device that lives on paper
/// ([technical design](../../../docs/Technical-design.md#111-recovery-codes)).
pub const RECOVERY_CODE: TypeName = TypeName::from_static("org.nibkeep.recovery-code");

/// A recovery code of a member's, on the code's record, and its revocation there; and a
/// replica's listing that a code signed, on the replica's record.
pub use nibkeep_model::fields::{RECOVERY_ENTRY, RECOVERY_KEY, RECOVERY_LISTING, RECOVERY_REVOKED};

/// The names of entities; a replica's Ed25519 verifying key as text, see
/// [`VerifyingKey::to_text`]; its X-Wing encryption key, which keys are sealed to; its
/// endpoint ID, which its other replicas connect to; and the user a replica belongs to.
pub use nibkeep_model::fields::{ENCRYPTION_KEY, ENDPOINT, NAME, OWNER, SIGNING_KEY};

/// Whether a replica is revoked; the space keys sealed to it; which of its changes that
/// were set aside are accepted; whether it is an AI agent's; and the fields that are secret
/// in a space for good.
pub use nibkeep_model::fields::{ACCEPTED, AI_AGENT, REVOKED, SECRET_FIELD, SPACE_KEY};

/// The key that a replica's secrets compartment opens what is sealed to, and the secrets
/// keys sealed to it ([technical design](../../../docs/Technical-design.md#12-secrets)).
pub use nibkeep_model::fields::{SECRETS_ENCRYPTION_KEY, SECRETS_FROM, SECRETS_KEY};

/// A member's role and the user a member's record is, in a space other than the personal
/// one; a replica's listing there, signed by its own key; and the user's spaces, each with
/// its first key sealed to a replica, in the personal space
/// ([technical design](../../../docs/Technical-design.md#13-sharing)).
pub use nibkeep_model::fields::{LISTING, ROLE, SPACE_ENTRY, USER_ID};

/// The person that a user is, in the space that says so.
pub const LINKED_PERSON: FieldName = FieldName::from_static("org.nibkeep.linked-person");

/// The languages and scripts that a user reads, ranked, as language ranges in text, such as
/// `en, *-Latn, ru`. A device's list, and a replica's, come before the user's, for what is
/// shown there: see [`Languages`].
pub const LANGUAGES: FieldName = FieldName::from_static("org.nibkeep.languages");

/// The device that a replica is on.
pub const ON_DEVICE: FieldName = FieldName::from_static("org.nibkeep.on-device");

/// A device's role, as its user marked it in the personal space: `archive` for an archive
/// node; any other device is an everyday device
/// ([technical design](../../../docs/Technical-design.md#96-where-the-copies-are)).
pub const DEVICE_ROLE: FieldName = FieldName::from_static("org.nibkeep.device-role");

/// Whether a device is kept away from home, as its user marked it in the personal space.
pub const AWAY: FieldName = FieldName::from_static("org.nibkeep.away");

/// How many bytes of files an everyday device keeps, as its user set it in the personal
/// space ([technical design](../../../docs/Technical-design.md#99-the-planner)).
pub const KEEPS_UP_TO: FieldName = FieldName::from_static("org.nibkeep.keeps-up-to");

/// How many days a device that is an archive node, or a backup, waits before it destroys what
/// a purge destroys, as its user set it in the personal space: 0 for none; without it, 30
/// ([technical design](../../../docs/Technical-design.md#911-copies-that-wait)).
pub const WAITS: FieldName = FieldName::from_static("org.nibkeep.waits");

/// A file, in any space of the user's, that a device keeps whatever its size, as its user
/// pinned it in the personal space: one value for each.
pub const PINNED: FieldName = FieldName::from_static("org.nibkeep.pinned");

/// The name of a user's personal space.
pub const PERSONAL: &str = "Personal";

/// The types of users, devices, replicas, spaces, groups and recovery codes.
#[must_use]
pub fn types() -> [TypeDef; 7] {
	let name = name_field();
	[
		TypeDef::new(USER, 1)
			.with_field(NAME, name.clone())
			.with_field(LINKED_PERSON, FieldDef::new(ValueKind::Reference))
			.with_field(LANGUAGES, languages())
			.with_field(USER_ID, text("User"))
			.with_field(ROLE, text("Role")),
		TypeDef::new(DEVICE, 1)
			.with_field(NAME, name.clone())
			.with_field(LANGUAGES, languages())
			.with_field(DEVICE_ROLE, text("Role").last_writer_wins())
			.with_field(
				AWAY,
				labelled(FieldDef::new(ValueKind::Boolean), "Away from home").last_writer_wins(),
			)
			.with_field(
				KEEPS_UP_TO,
				labelled(FieldDef::new(ValueKind::Integer), "Keeps up to").last_writer_wins(),
			)
			.with_field(
				WAITS,
				labelled(
					FieldDef::new(ValueKind::Integer),
					"Waits before purging (days)",
				)
				.last_writer_wins(),
			)
			.with_field(
				PINNED,
				labelled(FieldDef::new(ValueKind::Reference), "Pinned file").many(),
			),
		TypeDef::new(REPLICA, 1)
			.with_field(NAME, name.clone())
			.with_field(LANGUAGES, languages())
			.with_field(SIGNING_KEY, text("Verifying key"))
			.with_field(ENCRYPTION_KEY, text("Public encryption key"))
			.with_field(ENDPOINT, text("Endpoint ID").last_writer_wins())
			.with_field(OWNER, FieldDef::new(ValueKind::Reference).role())
			.with_field(ON_DEVICE, FieldDef::new(ValueKind::Reference))
			.with_field(REVOKED, text("Revoked key"))
			.with_field(SPACE_KEY, text("Sealed space key"))
			.with_field(ACCEPTED, text("Accepted change"))
			.with_field(
				AI_AGENT,
				labelled(FieldDef::new(ValueKind::Boolean), "AI agent"),
			)
			.with_field(SECRET_FIELD, text("Field made secret").many())
			.with_field(
				SECRETS_ENCRYPTION_KEY,
				text("Public secrets encryption key"),
			)
			.with_field(SECRETS_FROM, FieldDef::new(ValueKind::Reference))
			.with_field(SECRETS_KEY, text("Sealed secrets key"))
			.with_field(LISTING, text("Signed listing").many())
			.with_field(SPACE_ENTRY, text("Space with its sealed first key").many())
			.with_field(RECOVERY_LISTING, text("Listing signed by a recovery code"))
			.with_field(
				fields::SPACE_CHOICE,
				text("Signed choice of a space").many(),
			)
			.with_field(
				fields::NEW_SPACES,
				labelled(FieldDef::new(ValueKind::Boolean), "Takes new spaces"),
			),
		TypeDef::new(SPACE, 1).with_field(NAME, name.clone()),
		TypeDef::new(SPACE_NAMES, 1)
			.with_field(NAME, name.clone())
			.with_field(OF_SPACE, text("Space")),
		TypeDef::new(GROUP, 1).with_field(NAME, name.clone()),
		TypeDef::new(RECOVERY_CODE, 1)
			.with_field(NAME, name)
			.with_field(RECOVERY_KEY, text("Recovery key with its encryption keys"))
			.with_field(RECOVERY_REVOKED, text("Revoked recovery key"))
			.with_field(
				RECOVERY_ENTRY,
				text("Personal space's entry signed by the code"),
			)
			.with_field(SPACE_KEY, text("Sealed space key"))
			.with_field(SECRETS_KEY, text("Sealed secrets key"))
			.with_field(SPACE_ENTRY, text("Space with its sealed first key").many()),
	]
}

/// The name of an entity, which every entity can have, labelled in English.
fn name_field() -> FieldDef {
	labelled(FieldDef::new(ValueKind::Name), "Name")
}

/// A field labelled in English. The labels of a replica's keys say what they are: the
/// public halves, which every replica reads, or keys sealed to the replica, never the
/// secrets themselves.
fn labelled(field: FieldDef, label: &str) -> FieldDef {
	let english = nibkeep_model::Tag::parse("en").expect("a language tag");
	field.with_label(Some(&english), label)
}

/// A field of text, labelled in English.
fn text(label: &str) -> FieldDef {
	labelled(FieldDef::new(ValueKind::Text), label)
}

/// The field of ranked languages: text, whose last writer wins.
fn languages() -> FieldDef {
	FieldDef::new(ValueKind::Text).last_writer_wins()
}

/// What founding a personal space creates.
#[derive(Clone, Debug)]
pub struct Founding {
	/// The user.
	pub user: EntityId,
	/// The person linked to the user.
	pub person: EntityId,
	/// The device.
	pub device: EntityId,
	/// The replica on the device, which founds the space.
	pub replica: EntityId,
	/// The space's own entity.
	pub space: EntityId,
	/// The operations of the space's first change, which govern the space: the replica's
	/// verifying key, its encryption key, its owner and the listing that its key signed. A
	/// change that sets them holds nothing else.
	pub governing: Vec<Operation>,
	/// The operations of the space's second change: everything else.
	pub operations: Vec<Operation>,
}

/// The first two changes of the user's personal space, `personal`: the replica's
/// verifying key, its encryption key, its owner and the listing that its key signed, on its
/// record, derived from the space and its key, as in every space; then the user with a name
/// and the languages they read, unless there are none, linked to a person with a name of
/// their own, such as the user's full name beside an account's name, the device with its
/// name, on its record derived alike, the replica with its name, which is the app's, on the
/// device, and the space's own entity, named [`PERSONAL`].
pub fn found(
	personal: SpaceId,
	(user_name, person_name): (&str, &str),
	(device_name, replica_name): (&str, &str),
	languages: &Languages,
	(signing, encryption): (&SigningKey, &EncryptionKey),
	rng: &mut impl CryptoRng,
) -> Founding {
	let user = EntityId::random(rng);
	let person = EntityId::random(rng);
	let replica_key = signing.verifying_key();
	let replica = replica_record(personal, &replica_key);
	let device = device_record(personal, &replica_key);
	let space = EntityId::random(rng);
	let listing = Listing::sign(signing, encryption, personal, (user, false));
	let mut assert = |statements: Vec<(EntityId, FieldName, Value)>| -> Vec<Operation> {
		statements
			.into_iter()
			.map(|(entity, field, value)| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					entity,
					field,
					value,
				))
			})
			.collect()
	};
	let governing = assert(vec![
		(replica, SIGNING_KEY, Value::Text(replica_key.to_text())),
		(
			replica,
			ENCRYPTION_KEY,
			Value::Text(encryption_key_text(encryption)),
		),
		(replica, OWNER, Value::Reference(user)),
		(replica, fields::LISTING, Value::Text(listing.to_text())),
	]);
	let mut statements = vec![
		(user, fields::TYPE, Value::Text(USER.to_string())),
		(user, NAME, Value::Name(Name::new(user_name))),
		(user, LINKED_PERSON, Value::Reference(person)),
		(person, fields::TYPE, Value::Text(PERSON.to_string())),
		(person, NAME, Value::Name(Name::new(person_name))),
		(device, fields::TYPE, Value::Text(DEVICE.to_string())),
		(device, NAME, Value::Name(Name::new(device_name))),
		(replica, fields::TYPE, Value::Text(REPLICA.to_string())),
		(replica, NAME, Value::Name(Name::new(replica_name))),
		(replica, ON_DEVICE, Value::Reference(device)),
		(space, fields::TYPE, Value::Text(SPACE.to_string())),
		(space, NAME, Value::Name(Name::new(PERSONAL))),
	];
	if !languages.is_empty() {
		statements.push((user, LANGUAGES, Value::Text(languages.to_string())));
	}
	let operations = assert(statements);
	Founding {
		user,
		person,
		device,
		replica,
		space,
		governing,
		operations,
	}
}

/// The text form of an encryption key: its 1216 bytes in base64url without padding.
#[must_use]
pub fn encryption_key_text(key: &EncryptionKey) -> String {
	Base64UrlUnpadded::encode_string(&key.to_bytes())
}

/// The encryption key of its text form, or `None` if the text is not one.
#[must_use]
pub fn encryption_key_of_text(text: &str) -> Option<EncryptionKey> {
	EncryptionKey::from_bytes(&Base64UrlUnpadded::decode_vec(text).ok()?)
}

/// The text form of an endpoint ID: its 32 bytes in base64url without padding.
#[must_use]
pub fn endpoint_text(endpoint: &[u8; 32]) -> String {
	Base64UrlUnpadded::encode_string(endpoint)
}

/// The endpoint ID of its text form, or `None` if the text is not one.
#[must_use]
pub fn endpoint_of_text(text: &str) -> Option<[u8; 32]> {
	Base64UrlUnpadded::decode_vec(text).ok()?.try_into().ok()
}

/// The 32-byte secret of a replica's endpoint key, derived from its signing key, so that
/// the replica needs no other key to connect, and a new signing key brings a new endpoint.
#[must_use]
pub fn endpoint_secret(signing: &SigningKey) -> Key {
	Key::from_bytes(*signing.expose_bytes()).derive("nibkeep 2026-10 endpoint key")
}

/// A replica that another adds to the user's personal space: its keys, where it connects
/// from, the names of its device and its app, and whether it is an AI agent's.
#[derive(Clone, Debug)]
pub struct NewReplica<'a> {
	/// The personal space, which its records are derived from.
	pub space: SpaceId,
	/// The user it belongs to.
	pub user: EntityId,
	/// Its verifying key.
	pub signing: &'a VerifyingKey,
	/// Its encryption key.
	pub encryption: &'a EncryptionKey,
	/// Its endpoint ID.
	pub endpoint: &'a [u8; 32],
	/// The name of its device.
	pub device: &'a str,
	/// The name of its app.
	pub app: &'a str,
	/// The keys of the personal space that it is granted, sealed to it.
	pub keys: &'a [SealedSpaceKey],
	/// Whether it is an AI agent's, which the change that lists its key says, for good.
	pub ai_agent: bool,
	/// The secrets keys that the replica that adds it gives it, sealed to the secrets
	/// encryption key it asked with, if it asked and the person chose to give them. They
	/// count once the new replica states, itself, that it asks for them with that key.
	pub given: &'a [SealedSecretsKey],
	/// The listing that its key signed, in text, if it is known yet: a new replica that
	/// pairs signs it once the grant tells it who it is, and its sponsor adds it then.
	pub listing: Option<&'a str>,
}

/// What adding a replica creates.
#[derive(Clone, Debug)]
pub struct AddedReplica {
	/// The new replica's device.
	pub device: EntityId,
	/// The new replica.
	pub replica: EntityId,
	/// The operations of a change that governs the space: the replica's verifying key, its
	/// encryption key, its owner, the space keys it is granted, whether it is an AI agent's,
	/// and the secrets keys it is given.
	pub governing: Vec<Operation>,
	/// The operations of a change after it: the replica's type, name, device and endpoint,
	/// and the device with its type and name.
	pub operations: Vec<Operation>,
}

/// Adds a replica on a new device: the changes that the replica that pairs with it makes,
/// as the identity specification says, on its records derived from the personal space and
/// its key, which the keys it is granted are sealed for.
pub fn add_replica(new: &NewReplica<'_>, rng: &mut impl CryptoRng) -> AddedReplica {
	let replica = replica_record(new.space, new.signing);
	let device = device_record(new.space, new.signing);
	let mut assert = |statements: Vec<(EntityId, FieldName, Value)>| -> Vec<Operation> {
		statements
			.into_iter()
			.map(|(entity, field, value)| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					entity,
					field,
					value,
				))
			})
			.collect()
	};
	let mut statements = vec![
		(replica, SIGNING_KEY, Value::Text(new.signing.to_text())),
		(
			replica,
			ENCRYPTION_KEY,
			Value::Text(encryption_key_text(new.encryption)),
		),
		(replica, OWNER, Value::Reference(new.user)),
	];
	statements.extend(
		new.keys
			.iter()
			.map(|key| (replica, SPACE_KEY, Value::Text(key.to_text()))),
	);
	if new.ai_agent {
		statements.push((replica, AI_AGENT, Value::Boolean(true)));
	}
	if let Some(listing) = new.listing {
		statements.push((replica, fields::LISTING, Value::Text(listing.to_owned())));
	}
	let mut governing = assert(statements);
	let operations = assert(vec![
		(replica, fields::TYPE, Value::Text(REPLICA.to_string())),
		(replica, NAME, Value::Name(Name::new(new.app))),
		(replica, ON_DEVICE, Value::Reference(device)),
		(replica, ENDPOINT, Value::Text(endpoint_text(new.endpoint))),
		(device, fields::TYPE, Value::Text(DEVICE.to_string())),
		(device, NAME, Value::Name(Name::new(new.device))),
	]);
	governing.extend(
		new.given
			.iter()
			.map(|given| give_secrets_key(replica, given, rng)),
	);
	AddedReplica {
		device,
		replica,
		governing,
		operations,
	}
}

/// An AI agent's key that a replica keeps for an AI client on its own device, with no folder
/// of its own ([technical design: AI agents]): its keys, the device it is on, its name and
/// the listing that it signed.
///
/// [technical design: AI agents]: ../../../docs/Technical-design.md#14-ai-agents
#[derive(Clone, Debug)]
pub struct NewAgentKey<'a> {
	/// The personal space, which its record is derived from.
	pub space: SpaceId,
	/// The user it belongs to.
	pub user: EntityId,
	/// Its verifying key.
	pub signing: &'a VerifyingKey,
	/// Its encryption key.
	pub encryption: &'a EncryptionKey,
	/// The device of the replica that keeps it.
	pub device: EntityId,
	/// Its name: the AI client's.
	pub name: &'a str,
	/// The listing that its key signed, in text.
	pub listing: &'a str,
}

/// Adds an AI agent's key on the device of the replica that keeps it: the changes that the
/// replica makes, as for a paired AI agent's replica, but with no endpoint, as nothing
/// connects to it, and no device record of its own. Returns its record, the operations of a
/// change that governs the space, and those of a change after it.
pub fn add_agent_key(
	new: &NewAgentKey<'_>,
	rng: &mut impl CryptoRng,
) -> (EntityId, Vec<Operation>, Vec<Operation>) {
	let record = replica_record(new.space, new.signing);
	let mut assert = |statements: Vec<(FieldName, Value)>| -> Vec<Operation> {
		statements
			.into_iter()
			.map(|(field, value)| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					record,
					field,
					value,
				))
			})
			.collect()
	};
	let governing = assert(vec![
		(SIGNING_KEY, Value::Text(new.signing.to_text())),
		(
			ENCRYPTION_KEY,
			Value::Text(encryption_key_text(new.encryption)),
		),
		(OWNER, Value::Reference(new.user)),
		(AI_AGENT, Value::Boolean(true)),
		(fields::LISTING, Value::Text(new.listing.to_owned())),
	]);
	let operations = assert(vec![
		(fields::TYPE, Value::Text(REPLICA.to_string())),
		(NAME, Value::Name(Name::new(new.name))),
		(ON_DEVICE, Value::Reference(new.device)),
	]);
	(record, governing, operations)
}

/// A replica's record, as a space has it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaRecord {
	/// The replica.
	pub id: EntityId,
	/// Its verifying key, if it has one that can be read; of several, the first by
	/// statement ID.
	pub signing: Option<VerifyingKey>,
	/// Whether it has an encryption key that can be read.
	pub has_encryption_key: bool,
	/// Its endpoint ID, if it has one that can be read.
	pub endpoint: Option<[u8; 32]>,
	/// The user it belongs to.
	pub owner: Option<EntityId>,
	/// The device it is on.
	pub device: Option<EntityId>,
	/// Whether it is deleted.
	pub deleted: bool,
}

/// The record of a replica, as `state` has it now, or `None` if the entity is no replica.
///
/// # Errors
///
/// Returns an error if the state's storage cannot be read.
pub fn replica<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<ReplicaRecord>, S::Error> {
	if !state.types(entity)?.contains(&REPLICA) {
		return Ok(None);
	}
	// Current statements come in the order of their IDs.
	let first_text = |field: &FieldName| -> Result<Vec<String>, S::Error> {
		Ok(state
			.current(entity, field)?
			.iter()
			.filter_map(|statement| statement.value.as_text().map(str::to_owned))
			.collect())
	};
	let reference = |field: &FieldName| -> Result<Option<EntityId>, S::Error> {
		Ok(state
			.current(entity, field)?
			.iter()
			.find_map(|statement| statement.value.as_reference()))
	};
	let deleted = state
		.current(entity, &fields::DELETED)?
		.iter()
		.any(|statement| statement.value == Value::Boolean(true));
	Ok(Some(ReplicaRecord {
		id: entity,
		signing: first_text(&SIGNING_KEY)?
			.iter()
			.find_map(|text| VerifyingKey::from_text(text)),
		has_encryption_key: first_text(&ENCRYPTION_KEY)?
			.iter()
			.any(|text| encryption_key_of_text(text).is_some()),
		endpoint: first_text(&ENDPOINT)?
			.iter()
			.find_map(|text| endpoint_of_text(text)),
		owner: reference(&OWNER)?,
		device: reference(&ON_DEVICE)?,
		deleted,
	}))
}

/// The name of an entity, written in full: of its current names, the one in the statement
/// with the smallest ID.
///
/// # Errors
///
/// Returns an error if the state's storage cannot be read.
pub fn name<S: Statements>(state: &State<S>, entity: EntityId) -> Result<Option<String>, S::Error> {
	Ok(state
		.current(entity, &NAME)?
		.iter()
		.find_map(|statement| statement.value.as_name())
		.map(|name| name.full().to_owned()))
}
