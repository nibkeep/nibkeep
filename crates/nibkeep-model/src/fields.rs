//! The fields that NibKeep defines: facts about entities themselves, the standard
//! qualifiers, and the fields of the entities that define types.

use crate::FieldName;

/// The types of an entity: text values that name types.
pub const TYPE: FieldName = FieldName::from_static("org.nibkeep.type");

/// The names of entities such as users, people, replicas and spaces.
pub const NAME: FieldName = FieldName::from_static("org.nibkeep.name");

/// A replica's Ed25519 verifying key, as text.
pub const SIGNING_KEY: FieldName = FieldName::from_static("org.nibkeep.signing-key");

/// A replica's X-Wing encryption key, which keys are sealed to, as text.
pub const ENCRYPTION_KEY: FieldName = FieldName::from_static("org.nibkeep.encryption-key");

/// The user a replica belongs to: a reference, and a role.
pub const OWNER: FieldName = FieldName::from_static("org.nibkeep.owner");

/// A key of a replica that is revoked, as text, in the form of [`SIGNING_KEY`]: the key's
/// changes that not every replica which revoked it had are set aside, and it is synced with
/// no more.
pub const REVOKED: FieldName = FieldName::from_static("org.nibkeep.revoked");

/// A key of the space sealed to a replica, as text: the canonical encoding of
/// `[epoch, key hash, sealed key]` in base64url.
pub const SPACE_KEY: FieldName = FieldName::from_static("org.nibkeep.space-key");

/// A replica's secrets encryption key, which secrets keys are sealed to, as text: an X-Wing
/// encryption key whose decryption key is in the replica's secrets compartment. A replica
/// states it to ask for the secrets, or once it holds them.
pub const SECRETS_ENCRYPTION_KEY: FieldName =
	FieldName::from_static("org.nibkeep.secrets-encryption-key");

/// The replica that a replica takes the secrets from, a reference: the one that the person
/// chose on it, or itself, for a replica that set up the secrets. It takes secrets keys from
/// no other.
pub const SECRETS_FROM: FieldName = FieldName::from_static("org.nibkeep.secrets-from");

/// A secrets key of the space sealed to a replica's secrets encryption key, as text: the
/// canonical encoding of `[epoch, key hash, sealed key, recipient]` in base64url, where
/// `recipient` is the hash of the secrets encryption key it was sealed to.
pub const SECRETS_KEY: FieldName = FieldName::from_static("org.nibkeep.secrets-key");

/// The ID of a change that was set aside and is accepted, as text: it counts after all.
pub const ACCEPTED: FieldName = FieldName::from_static("org.nibkeep.accepted");

/// `true` on the record of a replica that is an AI agent's, in the change that lists its
/// key: its changes count only if they write what an AI agent may ([`ai_agent_may_write`]),
/// and purge, seal and govern nothing.
pub const AI_AGENT: FieldName = FieldName::from_static("org.nibkeep.ai-agent");

/// Whether a space with members allows AI agents, `true` or `false`, in a change that governs
/// it, which only an admin's counts: an AI agent's listing counts only where the latest such
/// statements that its change builds on allow them
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).
pub const AI_AGENTS: FieldName = FieldName::from_static("org.nibkeep.ai-agents");

/// The name of a field whose values are secret in the space for good, as text: no later
/// definition of types makes it ordinary again.
pub const SECRET_FIELD: FieldName = FieldName::from_static("org.nibkeep.secret-field");

/// A member's role in a space other than the personal one, on the member's record, as
/// text: `admin`, `editor`, `viewer`, or `none` for a member who left or was removed
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).
pub const ROLE: FieldName = FieldName::from_static("org.nibkeep.role");

/// The ID of the user whom a member's record in a space other than the personal one is, as
/// text, fixed when the record is made.
pub const USER_ID: FieldName = FieldName::from_static("org.nibkeep.user-id");

/// A replica's listing in a space other than the personal one, signed by its own key, as
/// text: on the replica's record there it lists the key, and on its record in the personal
/// space it asks to be listed in that space.
pub const LISTING: FieldName = FieldName::from_static("org.nibkeep.listing");

/// A space of the user's, on a replica's record in the personal space, as text: the space,
/// the user's member record there, and the space's first key sealed to the replica.
pub const SPACE_ENTRY: FieldName = FieldName::from_static("org.nibkeep.space-entry");

/// The group that a member's record in a space is, as text: the group and its first change,
/// fixed when the record is made, by the statement that adds the group
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups)).
pub const GROUP: FieldName = FieldName::from_static("org.nibkeep.group");

/// An anchor, in a space that a group is in, as text: the group's changes that take effect in
/// the space at the change that states it.
pub const ANCHOR: FieldName = FieldName::from_static("org.nibkeep.anchor");

/// A group's agreement to join a space, in the group, as text: the space, its first change,
/// and the statement there that adds the group.
pub const JOINED: FieldName = FieldName::from_static("org.nibkeep.joined");

/// The withdrawal of a group's agreement to join a space, in the group, as text, as an
/// agreement names it: the group leaves that space for good.
pub const WITHDRAWN: FieldName = FieldName::from_static("org.nibkeep.withdrawn");

/// A member's request, in a group, that the group join a space, as text: what an agreement
/// would name, and the space's name sealed to the group's admins.
pub const JOIN_REQUEST: FieldName = FieldName::from_static("org.nibkeep.join-request");

/// A group's first key, in a space it is in, as text, in a change that the space's key
/// wraps: the space's replicas read the group's log with it.
pub const GROUP_KEY: FieldName = FieldName::from_static("org.nibkeep.group-key");

/// What kind of space a space is, as text, in its first change: `group` for a group, a
/// space with members and no data of its own.
pub const SPACE_KIND: FieldName = FieldName::from_static("org.nibkeep.space-kind");

/// The verifying key of a member through a group, as text, on a member's record that an
/// admin made in a space for them, so that their replicas list themselves on it once the
/// person confirms: it makes them a direct member.
pub const MADE_FOR: FieldName = FieldName::from_static("org.nibkeep.made-for");

/// Whether the person whose key a record in a space is derived from is kept out of the
/// space through groups, a Boolean: `true` keeps them out, as an admin does or as they leave
/// the space and stay in the group, and `false`, which only an admin states, lets them back.
pub const KEPT_OUT: FieldName = FieldName::from_static("org.nibkeep.kept-out");

/// A member's claim to the admin role, `true`, on their own record in a space: it takes effect
/// 30 days after a replica of another member first shows that it saw it, unless an admin
/// rejects it in time.
pub const CLAIM: FieldName = FieldName::from_static("org.nibkeep.claim");

/// An admin's rejection of a claim to the admin role, as text: the ID of the change that made
/// the claim.
pub const REJECTED: FieldName = FieldName::from_static("org.nibkeep.rejected");

/// That a replica saw a claim or a rejection, as text: the ID of the change that made it, on
/// the replica's record, in a change that builds on it.
pub const SEEN: FieldName = FieldName::from_static("org.nibkeep.seen");

/// A recovery code of a member's, as text, on the code's record: its recovery key and the
/// encryption keys that keys are sealed to, which the recovery key signed.
pub const RECOVERY_KEY: FieldName = FieldName::from_static("org.nibkeep.recovery-key");

/// The recovery key of a member's recovery code that is revoked, as text, on the code's
/// record. It is no revocation of a replica's key, which [`REVOKED`] names: a code's key is
/// never listed, and what revokes one says nothing of any replica.
pub const RECOVERY_REVOKED: FieldName = FieldName::from_static("org.nibkeep.recovery-revoked");

/// A replica's listing signed by a recovery code of its member's, as text, on the replica's
/// record: by it, a new device lists its own key, which nothing else listed, after its user
/// lost their other devices.
pub const RECOVERY_LISTING: FieldName = FieldName::from_static("org.nibkeep.recovery-listing");

/// The personal space's entry for a recovery code, as the code signed it once, as it was
/// named, as text, on the code's record in the personal space: what a recovery file's entry
/// for the code says of the space, so that no device writes another for the code
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
pub const RECOVERY_ENTRY: FieldName = FieldName::from_static("org.nibkeep.recovery-entry");

/// A device's choice of a space, as text, signed by its key and numbered: on its own record
/// in the personal space, and on its record in the space, derived from the space and its key.
/// Of a key's choices of a space, the one with the highest number stands, and one that does
/// not hold the space ends the device there
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
pub const SPACE_CHOICE: FieldName = FieldName::from_static("org.nibkeep.space-choice");

/// Whether a device takes the spaces given to it later, `true`, or asks first, `false`: on
/// its own record in the personal space, counting only where its own key states it, so that
/// no other device can have it take what it was given; a device that states none takes them
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
pub const NEW_SPACES: FieldName = FieldName::from_static("org.nibkeep.new-spaces");

/// The fields that govern a space, by saying who may write to it: the keys of replicas,
/// whom they belong to, which are revoked, the space keys sealed to them, which of their
/// set-aside changes are accepted, which are AI agents', and whether a space allows them,
/// which fields are secret for good,
/// who holds the secrets, members and their roles, replicas' signed listings, the user's
/// spaces with their first keys, and groups: those in a space, their anchors, the spaces
/// they agree to join, the records made for their members, and those kept out; and claims
/// to the admin role, their rejections, and that replicas saw them; and members' recovery
/// codes, which of those are revoked, the personal space's entries that they signed, and
/// listings that a code signed; and devices' choices of spaces, and whether they take new
/// ones. Changes that set them hold nothing else, and are never purged, so that every
/// replica reads them alike.
pub const GOVERNING: [FieldName; 34] = [
	SIGNING_KEY,
	ENCRYPTION_KEY,
	OWNER,
	REVOKED,
	SPACE_KEY,
	ACCEPTED,
	AI_AGENT,
	AI_AGENTS,
	SECRET_FIELD,
	SECRETS_ENCRYPTION_KEY,
	SECRETS_FROM,
	SECRETS_KEY,
	ROLE,
	USER_ID,
	LISTING,
	SPACE_ENTRY,
	GROUP,
	ANCHOR,
	JOINED,
	WITHDRAWN,
	JOIN_REQUEST,
	GROUP_KEY,
	SPACE_KIND,
	MADE_FOR,
	KEPT_OUT,
	CLAIM,
	REJECTED,
	SEEN,
	RECOVERY_KEY,
	RECOVERY_REVOKED,
	RECOVERY_LISTING,
	RECOVERY_ENTRY,
	SPACE_CHOICE,
	NEW_SPACES,
];

/// Whether a field governs a space: see [`GOVERNING`].
#[must_use]
pub fn is_governing(field: &FieldName) -> bool {
	GOVERNING.contains(field)
}

/// The fields of NibKeep's own that this version knows, besides those that govern a space
/// and those that [`is_reserved`] names: those of this module, and those that other crates
/// define, of users, devices, backups and storage policies. A test finds every field of
/// NibKeep's own that the programs define, and checks that it is here. Once the formats are
/// at version 1, the fields known so are fixed: one added later is never known so, but kept
/// in a governing change as from a later version (`docs/spec/log.md`, section 1).
const OTHERS_KNOWN: [FieldName; 30] = [
	NAME,
	POSITION,
	ENDPOINT,
	LIST,
	ITEM,
	// Of users and devices, in `nibkeep-governance`.
	FieldName::from_static("org.nibkeep.linked-person"),
	FieldName::from_static("org.nibkeep.languages"),
	FieldName::from_static("org.nibkeep.on-device"),
	FieldName::from_static("org.nibkeep.device-role"),
	FieldName::from_static("org.nibkeep.away"),
	FieldName::from_static("org.nibkeep.keeps-up-to"),
	FieldName::from_static("org.nibkeep.waits"),
	FieldName::from_static("org.nibkeep.pinned"),
	FieldName::from_static("org.nibkeep.of-space"),
	// Of backups and storage policies, in `nibkeep-node`.
	FieldName::from_static("org.nibkeep.backup-folder"),
	FieldName::from_static("org.nibkeep.backup-space"),
	FieldName::from_static("org.nibkeep.backup-policy"),
	FieldName::from_static("org.nibkeep.policy"),
	FieldName::from_static("org.nibkeep.policy-copies"),
	FieldName::from_static("org.nibkeep.policy-away"),
	FieldName::from_static("org.nibkeep.policy-archived"),
	FieldName::from_static("org.nibkeep.policy-kept"),
	FieldName::from_static("org.nibkeep.rule-policy"),
	FieldName::from_static("org.nibkeep.rule-type"),
	FieldName::from_static("org.nibkeep.rule-has"),
	FieldName::from_static("org.nibkeep.rule-folder"),
	FieldName::from_static("org.nibkeep.rule-device"),
	FieldName::from_static("org.nibkeep.rule-media-type"),
	FieldName::from_static("org.nibkeep.rule-larger-than"),
	FieldName::from_static("org.nibkeep.rule-asks-above"),
];

/// Whether this version knows a field of NibKeep's own: one that governs a space, one that
/// [`is_reserved`] names, or another that it defines. A governing change may state a field
/// whose name starts with `org.nibkeep.` and that this version does not know, which a later
/// version may govern with: the statement is kept, with no effect here
/// (`docs/spec/log.md`, section 1).
#[must_use]
pub fn is_known(field: &FieldName) -> bool {
	is_governing(field) || is_reserved(field) || OTHERS_KNOWN.contains(field)
}

/// Whether a governing change may state a field that this version does not govern with:
/// one of NibKeep's own that it does not know, from a later version.
#[must_use]
pub fn is_later_governing(field: &FieldName) -> bool {
	field.is_nibkeep() && !is_known(field)
}

/// The position key of a membership in an ordered list, as text.
pub const POSITION: FieldName = FieldName::from_static("org.nibkeep.position");

/// A replica's endpoint ID, which its other replicas connect to, as text.
pub const ENDPOINT: FieldName = FieldName::from_static("org.nibkeep.endpoint");

/// The list that a membership of an ordered list belongs to: a reference.
pub const LIST: FieldName = FieldName::from_static("org.nibkeep.list");

/// The item that a membership puts in its ordered list: a reference.
pub const ITEM: FieldName = FieldName::from_static("org.nibkeep.item");

/// The fields of NibKeep's own that an AI agent may write, as every entity has them: its
/// name, its types, whether it is deleted, what it is merged into, and the fields of
/// ordered lists. The others are the library's own records, such as replicas, type
/// definitions and storage policies.
const AI_AGENTS_FIELDS: [FieldName; 7] = [NAME, TYPE, DELETED, MERGED_INTO, LIST, ITEM, POSITION];

/// The types of NibKeep's own that an AI agent may give an entity: a membership's of an
/// ordered list.
const AI_AGENTS_TYPES: [&str; 1] = ["org.nibkeep.membership"];

/// Whether an AI agent may assert a statement: one of a field that is not NibKeep's own, or
/// of one that every entity has, its name, its types, whether it is deleted, what it is
/// merged into, and the fields of ordered lists; and of the field of types, only a type
/// that is not NibKeep's own, or a membership's. Names are NibKeep's own when
/// they start with `org.nibkeep.`. Never one with a move's note, which only a move writes,
/// as history shows it as the account of who recorded the value where it came from, nor
/// one with a migration's, for the same reason.
#[must_use]
pub fn ai_agent_may_write(statement: &crate::Statement) -> bool {
	if statement
		.qualifiers
		.iter()
		.any(|(qualifier, _)| is_note(qualifier))
	{
		return false;
	}
	ai_agent_may_take_away(statement)
}

/// Whether an AI agent may take a statement away, by replacing or retracting it, as its
/// field goes: one of a field that it may assert, and of the field of types, of a type that
/// it may give. A move's or a migration's note does not count, as it marks a value that the
/// AI agent may still change. Only an AI agent's own replica refuses the others, before it
/// commits: every replica decides from the payload alone whether an AI agent's change
/// counts, and a retraction names only the statements it takes away.
#[must_use]
pub fn ai_agent_may_take_away(statement: &crate::Statement) -> bool {
	let field = &statement.field;
	if !field.is_nibkeep() {
		return true;
	}
	if !AI_AGENTS_FIELDS.contains(field) {
		return false;
	}
	if *field != TYPE {
		return true;
	}
	statement.value.as_text().is_some_and(ai_agents_type)
}

/// Whether an entity of these types is one of the library's own records: of a type of
/// NibKeep's own, but a membership's of an ordered list, such as a replica, a type
/// definition or a storage policy. An AI agent's own replica takes none of their
/// statements away, and deletes, restores and merges none of them.
#[must_use]
pub fn is_library_record<'a>(types: impl IntoIterator<Item = &'a crate::TypeName>) -> bool {
	types.into_iter().any(|name| !ai_agents_type(name.as_str()))
}

/// Whether an AI agent may give an entity a type: one that is not NibKeep's own, or a
/// membership's.
fn ai_agents_type(name: &str) -> bool {
	!name.starts_with(NIBKEEP_PREFIX) || AI_AGENTS_TYPES.contains(&name)
}

/// How the names of NibKeep's own fields and types start.
const NIBKEEP_PREFIX: &str = "org.nibkeep.";

/// The fields whose text is not words that people search for, but keys, positions and
/// the names of fields.
const NOT_WORDS: [FieldName; 29] = [
	SIGNING_KEY,
	ENCRYPTION_KEY,
	ENDPOINT,
	POSITION,
	REVOKED,
	SPACE_KEY,
	ACCEPTED,
	SECRET_FIELD,
	SECRETS_ENCRYPTION_KEY,
	SECRETS_KEY,
	ROLE,
	USER_ID,
	LISTING,
	SPACE_ENTRY,
	GROUP,
	ANCHOR,
	JOINED,
	WITHDRAWN,
	JOIN_REQUEST,
	GROUP_KEY,
	SPACE_KIND,
	MADE_FOR,
	REJECTED,
	SEEN,
	RECOVERY_KEY,
	RECOVERY_REVOKED,
	RECOVERY_LISTING,
	RECOVERY_ENTRY,
	SPACE_CHOICE,
];

/// `true` when the entity is deleted: hidden, but kept, and restored by retracting the
/// statement.
pub const DELETED: FieldName = FieldName::from_static("org.nibkeep.deleted");

/// A reference to the entity that this one was merged into.
pub const MERGED_INTO: FieldName = FieldName::from_static("org.nibkeep.merged-into");

/// The entity in another space that an entity was moved into, a reference: links to it lead
/// there, for those who can read that space, and the old space keeps the history it had
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing) and
/// [spaces and moves](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves)).
pub const MOVED_INTO: FieldName = FieldName::from_static("org.nibkeep.moved-into");

/// `true` on an entity that a move left behind, whose mover asked that what stays of it be
/// purged: the old space's admins purge what the mover could not
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves)).
pub const PURGE_ASKED: FieldName = FieldName::from_static("org.nibkeep.purge-asked");

/// Qualifier: when the value was true, as a date.
pub const VALID_TIME: FieldName = FieldName::from_static("org.nibkeep.valid-time");

/// Qualifier: where the information came from, as a reference or text.
pub const SOURCE: FieldName = FieldName::from_static("org.nibkeep.source");

/// Qualifier: how the source wrote the value, as text.
pub const CREDITED_AS: FieldName = FieldName::from_static("org.nibkeep.credited-as");

/// Qualifier: how likely the value is to be right, as an integer percentage.
pub const CONFIDENCE: FieldName = FieldName::from_static("org.nibkeep.confidence");

/// Qualifier: `true` for the value to show first when a field has several.
pub const PREFERRED: FieldName = FieldName::from_static("org.nibkeep.preferred");

/// Qualifier: the language and script of the value, as a language tag in text.
pub const LANGUAGE: FieldName = FieldName::from_static("org.nibkeep.language");

/// Qualifier: the statement ID, as text, of the first form of the value that this value is
/// another form of, in another language or script.
pub const ALTERNATIVE: FieldName = FieldName::from_static("org.nibkeep.alternative");

/// Qualifier of a value that a move carried from another space, the mover's note of where
/// from: the ID of that space, as text ([technical design: spaces and moves]). The notes of a move are not
/// proof, and do not count when values are compared ([`is_note`]).
///
/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
pub const CARRIED_FROM: FieldName = FieldName::from_static("org.nibkeep.carried-from");

/// Qualifier of a value that a move carried, the mover's note of who recorded it in the
/// space it came from: the signing key of their replica, as text, in the form of
/// [`SIGNING_KEY`].
pub const CARRIED_BY: FieldName = FieldName::from_static("org.nibkeep.carried-by");

/// Qualifier of a value that a move carried, the mover's note of when the space it came
/// from recorded it: milliseconds since 1970, as an integer.
pub const CARRIED_AT: FieldName = FieldName::from_static("org.nibkeep.carried-at");

/// Qualifier of a past value that a move carried, the mover's note of when the space it came
/// from recorded that it was replaced: milliseconds since 1970, as an integer.
pub const CARRIED_REPLACED: FieldName = FieldName::from_static("org.nibkeep.carried-replaced");

/// Qualifier of a value that a migration wrote, the migrating device's note of where it
/// came from: the migration's name and the IDs of the statements it replaced, as text in the
/// form that [`crate::migration::note`] writes ([technical design: migrations]). Like a
/// move's notes, it is no proof, and does not count when values are compared ([`is_note`]).
///
/// [technical design: migrations]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#57-migrations
pub const MIGRATED_FROM: FieldName = FieldName::from_static("org.nibkeep.migrated-from");

/// The qualifiers that are notes about a value rather than part of it: those of a move, and
/// a migration's.
const NOTES: [FieldName; 5] = [
	CARRIED_FROM,
	CARRIED_BY,
	CARRIED_AT,
	CARRIED_REPLACED,
	MIGRATED_FROM,
];

/// Whether a qualifier is a note about a value rather than part of it, which values that
/// are compared, as for repeats and competing values, leave out: a move's notes, and a
/// migration's.
#[must_use]
pub fn is_note(field: &FieldName) -> bool {
	NOTES.contains(field)
}

/// The name of the type that a type definition defines, as text.
pub const TYPE_NAME: FieldName = FieldName::from_static("org.nibkeep.type-name");

/// The version of the type that a type definition defines, as an integer: 1 if left out.
pub const TYPE_VERSION: FieldName = FieldName::from_static("org.nibkeep.type-version");

/// The type that a field definition belongs to: a reference to the type's definition, and
/// a role.
pub const FIELD_OF: FieldName = FieldName::from_static("org.nibkeep.field-of");

/// The name of the field that a field definition defines, as text.
pub const FIELD_NAME: FieldName = FieldName::from_static("org.nibkeep.field-name");

/// The kind of the field's values, as the integer that stands for it.
pub const FIELD_KIND: FieldName = FieldName::from_static("org.nibkeep.field-kind");

/// How many values the field has, as the integer that stands for it: one if left out.
pub const FIELD_MULTIPLICITY: FieldName = FieldName::from_static("org.nibkeep.field-multiplicity");

/// `true` when the field's values are secret.
pub const FIELD_SECRET: FieldName = FieldName::from_static("org.nibkeep.field-secret");

/// `true` when the field is a role in a relationship.
pub const FIELD_ROLE: FieldName = FieldName::from_static("org.nibkeep.field-role");

/// The unit of a field of quantities, in UCUM.
pub const FIELD_UNIT: FieldName = FieldName::from_static("org.nibkeep.field-unit");

/// `true` when the entity that holds the field stands for the entities it refers to.
pub const FIELD_STANDS_FOR: FieldName = FieldName::from_static("org.nibkeep.field-stands-for");

/// `true` when a search that finds an entity the field refers to also finds the entity that
/// holds it.
pub const FIELD_SEARCH_THROUGH: FieldName =
	FieldName::from_static("org.nibkeep.field-search-through");

/// `true` when the field's values are codes, text in no language.
pub const FIELD_CODE: FieldName = FieldName::from_static("org.nibkeep.field-code");

/// The name that a field goes by when a link is shown from the entity it refers to, as
/// text.
pub const FIELD_REVERSE: FieldName = FieldName::from_static("org.nibkeep.field-reverse");

/// What apps show for the type that a type definition defines, instead of its name: text,
/// with the qualifier [`LANGUAGE`] for the language it is in, one value for each language.
pub const TYPE_LABEL: FieldName = FieldName::from_static("org.nibkeep.type-label");

/// What apps show for the field that a field definition defines, instead of its name, as
/// [`TYPE_LABEL`] for a type.
pub const FIELD_LABEL: FieldName = FieldName::from_static("org.nibkeep.field-label");

/// How a type names an entity of it that has no name of its own, in a language, as the
/// statement's language qualifier says: a pattern of the entity's fields
/// ([`crate::TypeDef::with_title`]). Text, one value for each language.
pub const TYPE_TITLE: FieldName = FieldName::from_static("org.nibkeep.type-title");

/// The fields that NibKeep defines for its own structure: the facts about entities
/// themselves, the qualifiers, and the fields of type definitions.
const RESERVED: [FieldName; 33] = [
	TYPE,
	DELETED,
	MERGED_INTO,
	MOVED_INTO,
	PURGE_ASKED,
	VALID_TIME,
	SOURCE,
	CREDITED_AS,
	CONFIDENCE,
	PREFERRED,
	LANGUAGE,
	ALTERNATIVE,
	CARRIED_FROM,
	CARRIED_BY,
	CARRIED_AT,
	CARRIED_REPLACED,
	MIGRATED_FROM,
	TYPE_NAME,
	TYPE_VERSION,
	FIELD_OF,
	FIELD_NAME,
	FIELD_KIND,
	FIELD_MULTIPLICITY,
	FIELD_SECRET,
	FIELD_ROLE,
	FIELD_UNIT,
	FIELD_STANDS_FOR,
	FIELD_SEARCH_THROUGH,
	FIELD_CODE,
	FIELD_REVERSE,
	TYPE_LABEL,
	TYPE_TITLE,
	FIELD_LABEL,
];

/// Whether a field's values can name an entity: `org.nibkeep.name`, and the fields whose
/// names end in `.name` or `.title`. An entity is shown by the name in the first of them
/// that has one, and found by it.
#[must_use]
pub fn can_name(field: &FieldName) -> bool {
	*field == NAME
		|| NAMING_SUFFIXES
			.iter()
			.any(|suffix| has_suffix(field, suffix))
}

/// The last parts of the names of fields that can name an entity, besides
/// `org.nibkeep.name`, in the order in which they are looked at for the name to show.
pub const NAMING_SUFFIXES: [&str; 2] = ["name", "title"];

/// Whether the last part of a field's name, after its last dot, is `suffix`.
#[must_use]
pub fn has_suffix(field: &FieldName, suffix: &str) -> bool {
	field
		.as_str()
		.rsplit_once('.')
		.is_some_and(|(_, last)| last == suffix)
}

/// The name that a value of a field holds, lowercased, by which the entity is found: of a
/// field that can name an entity, a name written in full, or text.
#[must_use]
pub fn name_key(field: &FieldName, value: &crate::Value) -> Option<String> {
	if can_name(field) {
		value.as_name_text().map(str::to_lowercase)
	} else {
		None
	}
}

/// Whether search finds the text values of a field by their words: those of every field
/// but those that NibKeep reserves for its own structure, and those that hold keys and
/// positions. Every field is found by its values.
#[must_use]
pub fn has_words(field: &FieldName) -> bool {
	!is_reserved(field) && !NOT_WORDS.contains(field)
}

/// Whether NibKeep defines the field for its own structure, so that the types of a space
/// cannot change how it is defined, for example to make it secret.
#[must_use]
pub fn is_reserved(field: &FieldName) -> bool {
	RESERVED.contains(field)
}
