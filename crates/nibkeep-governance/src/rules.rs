//! Which changes count, in a space whose first change governs it: those of the keys that its
//! governing changes list, and of a revoked key only those that every change which revokes
//! it builds on; of an AI agent's key, only ordinary changes that purge and seal nothing
//! and write what an AI agent may ([Log](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/log.md#8-which-changes-count)).
//! The others are set aside: kept and passed on, but neither applied nor acted on.

use std::collections::{BTreeMap, BTreeSet};

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::{ChangeId, EntityId, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_time::Timestamp;

use crate::{Anchor, Choice, GroupRef, Joining, Listing, RecoveryListing, RecoveryStatement};

pub use groups::{Followed, Reach};
use roles::{Kept, Members, Touch, Verdict};
pub use roles::{Role, Standing, View};

mod claims;
mod groups;
mod roles;

/// How many times the anchors taken to count are decided again, at most, until those that
/// count are those taken to: a space follows few groups, whose changes are few.
const ANCHOR_ROUNDS: usize = 8;

/// What the rules read of a change's header, which the log's changes give. The rules never
/// see a change itself: whatever a change can destroy or hide must show in
/// [`Header::purges_or_seals`], as no AI agent's change may do it.
pub trait Header {
	/// The change's ID.
	fn id(&self) -> ChangeId;
	/// The key that signed it.
	fn author(&self) -> &VerifyingKey;
	/// When it was made, by its author's clock.
	fn timestamp(&self) -> Timestamp;
	/// Whether it is the space's first change, with no parents.
	fn is_root(&self) -> bool;
	/// The space it is a change of.
	fn space(&self) -> SpaceId;
	/// Whether it purges or seals anything, or does anything else that destroys or hides
	/// content.
	fn purges_or_seals(&self) -> bool;
}

/// A replica's verifying key, by its bytes.
pub type KeyBytes = [u8; 32];

/// What a governing change says that decides which changes count.
#[derive(Clone, Debug)]
pub struct Fact {
	/// The change.
	id: ChangeId,
	author: KeyBytes,
	/// Its place among the governing changes: by timestamp, and then by ID.
	order: (Timestamp, ChangeId),
	/// Whether it is the space's first change.
	root: bool,
	/// The keys it lists, by the replica they are listed for.
	lists: Vec<(EntityId, KeyBytes)>,
	/// For each key it lists, whether the same change holds the key's own listing of
	/// it, which a space with members requires ([`crate::Listing`]).
	signed: Vec<bool>,
	/// For each of [`Fact::lists`], the member that the change states as the record's
	/// owner, if it states one.
	owners: Vec<Option<EntityId>>,
	/// For each of [`Fact::lists`], the record of the key's device, derived from the space
	/// and the key.
	devices: Vec<EntityId>,
	/// Whether it gives a member a role: the first change of a space with members does.
	gives_role: bool,
	/// The roles it gives members, by their records.
	roles: Vec<(EntityId, Role)>,
	/// The members whose records it makes, by stating their user IDs.
	users: Vec<EntityId>,
	/// What else its statements and retractions touch, which the author's role must allow
	/// in a space with members.
	touches: Vec<Touch>,
	/// The keys it revokes, which each revocation names.
	revokes: Vec<KeyBytes>,
	/// The ordinary changes it accepts.
	accepts: Vec<ChangeId>,
	/// The replicas it marks as AI agents'.
	marks: Vec<EntityId>,
	/// Whether it says that the space allows AI agents, if it says so: the last such
	/// statement of the change.
	allows_ai: Option<bool>,
	/// The fields it makes secret for good, but NibKeep's own, which no replica seals, if it
	/// is wrapped with key 0, which every replica holds: every replica that builds on it
	/// could read it.
	secret: Vec<FieldName>,
	/// The records of groups it makes, in a space: each record, its group, and the statement
	/// that adds the group.
	group_records: Vec<(EntityId, GroupRef, StatementId)>,
	/// The anchors it states, in a space that groups are in.
	anchors: Vec<Anchor>,
	/// The agreements to join spaces it states, in a group.
	joinings: Vec<Joining>,
	/// The withdrawals of agreements it states, in a group.
	withdrawals: Vec<Joining>,
	/// The records it makes for members through groups, each with the key it names.
	made_for: Vec<(EntityId, KeyBytes)>,
	/// What it says of keeping people out: each record, derived from the space and a key,
	/// with whether it keeps the key's person out.
	keeps_out: Vec<(EntityId, bool)>,
	/// The records whose members it says claim the admin role, each with the statement.
	claims: Vec<(EntityId, StatementId)>,
	/// The statements it retracts or replaces, which withdraw a claim.
	retracts: Vec<StatementId>,
	/// The claims to the admin role that it rejects, by the changes that made them.
	rejects: Vec<ChangeId>,
	/// The records of the recovery codes that it names, with signatures that verify, each
	/// on the record derived from the space, the member that its statement names, and its
	/// recovery key.
	codes: Vec<EntityId>,
	/// The recovery codes that it revokes: each code's record, with the recovery key that
	/// the revocation names.
	code_revokes: Vec<(EntityId, KeyBytes)>,
	/// The listing of its own author by a recovery code, if it lists its author so.
	by_code: Option<ByCode>,
	/// The devices' choices of the space that it states, each on the record it names, for
	/// the change's space, signed by the key it names: the key, the number and whether it
	/// holds the space.
	choices: Vec<(KeyBytes, u64, bool)>,
}

/// A change's listing of its own author by a recovery code
/// ([Identity](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/identity.md#11-recovery-codes)): the record and the owner
/// that it names, and the code's record for that owner, which a counting change that it
/// builds on must name.
#[derive(Clone, Copy, Debug)]
struct ByCode {
	/// The record that it lists its author on.
	record: EntityId,
	/// The record's owner, the code's member.
	owner: EntityId,
	/// The code's record, derived from the space, the owner and the recovery key.
	code: EntityId,
	/// The code's recovery key, which a revocation of the code names.
	recovery_key: KeyBytes,
}

impl Fact {
	/// What a governing change says before its payload is read: who made it, when, and
	/// whether it is the space's first.
	pub fn of_change(change: &impl Header) -> Self {
		Self {
			id: change.id(),
			author: *change.author().as_bytes(),
			order: (change.timestamp(), change.id()),
			root: change.is_root(),
			lists: Vec::new(),
			signed: Vec::new(),
			owners: Vec::new(),
			devices: Vec::new(),
			gives_role: false,
			roles: Vec::new(),
			users: Vec::new(),
			touches: Vec::new(),
			revokes: Vec::new(),
			accepts: Vec::new(),
			marks: Vec::new(),
			allows_ai: None,
			secret: Vec::new(),
			group_records: Vec::new(),
			anchors: Vec::new(),
			joinings: Vec::new(),
			withdrawals: Vec::new(),
			made_for: Vec::new(),
			keeps_out: Vec::new(),
			claims: Vec::new(),
			rejects: Vec::new(),
			retracts: Vec::new(),
			codes: Vec::new(),
			code_revokes: Vec::new(),
			by_code: None,
			choices: Vec::new(),
		}
	}

	/// What a governing change's payload says.
	pub fn read(change: &impl Header, operations: &[Operation]) -> Self {
		let mut fact = Self::of_change(change);
		// The owners and the hashes of encryption keys that the change states, and the
		// listings that its keys signed.
		let mut owners: Vec<(EntityId, EntityId)> = Vec::new();
		let mut encryptions: Vec<(EntityId, nibkeep_base::Hash)> = Vec::new();
		let mut listings: Vec<Listing> = Vec::new();
		let mut by_codes: Vec<RecoveryListing> = Vec::new();
		for operation in operations {
			fact.touches.extend(Touch::of_operation(operation));
			match operation {
				Operation::Retract { statements, .. } => fact.retracts.extend(statements),
				Operation::Assert { supersedes, .. } => fact.retracts.extend(supersedes),
				_ => {}
			}
			let Operation::Assert { statement, .. } = operation else {
				continue;
			};
			if statement.field == fields::AI_AGENT && statement.value == Value::Boolean(true) {
				fact.marks.push(statement.entity);
			}
			if statement.field == fields::AI_AGENTS {
				match statement.value {
					Value::Boolean(allows) => fact.allows_ai = Some(allows),
					// A value that this version does not know allows nothing.
					_ => fact.allows_ai = Some(false),
				}
			}
			if statement.field == fields::ROLE {
				fact.gives_role = true;
				match statement.value.as_text().and_then(Role::from_text) {
					Some(role) => fact.roles.push((statement.entity, role)),
					// A role that this version does not know is for an admin alone.
					None => fact.touches.push(Touch::Admin),
				}
			}
			if statement.field == fields::USER_ID {
				fact.users.push(statement.entity);
			}
			if statement.field == fields::KEPT_OUT {
				// A value that this version does not know keeps nobody out.
				let keeps = statement.value == Value::Boolean(true);
				fact.keeps_out.push((statement.entity, keeps));
				continue;
			}
			if statement.field == fields::CLAIM && statement.value == Value::Boolean(true) {
				fact.claims.push((statement.entity, statement.id));
				continue;
			}
			if statement.field == fields::OWNER
				&& let Some(owner) = statement.value.as_reference()
			{
				owners.push((statement.entity, owner));
			}
			let Value::Text(text) = &statement.value else {
				continue;
			};
			let field = &statement.field;
			if *field == fields::SIGNING_KEY || *field == fields::REVOKED {
				let Some(key) = VerifyingKey::from_text(text) else {
					continue;
				};
				if *field == fields::SIGNING_KEY {
					fact.lists.push((statement.entity, *key.as_bytes()));
					fact.devices
						.push(crate::device_record(change.space(), &key));
				} else {
					fact.revokes.push(*key.as_bytes());
				}
			} else if *field == fields::ENCRYPTION_KEY
				&& let Ok(bytes) = Base64UrlUnpadded::decode_vec(text)
			{
				encryptions.push((statement.entity, crate::encryption_hash(&bytes)));
			} else if *field == fields::LISTING
				&& let Some(listing) = Listing::from_text(text)
				&& listing.record == statement.entity
				&& listing.space == change.space()
				&& listing.verifies()
			{
				listings.push(listing);
			} else if fact.read_choice(change, statement, text) {
			} else if *field == fields::ACCEPTED
				&& let Ok(change) = text.parse()
			{
				fact.accepts.push(change);
			} else if fact.read_group_statement(statement, text)
				|| fact.read_code_statement(change, statement, text, &mut by_codes)
			{
			} else if *field == fields::SECRET_FIELD
				&& let Ok(secret) = text.parse::<FieldName>()
				&& !secret.is_nibkeep()
			{
				fact.secret.push(secret);
			}
		}
		fact.settle_listings(&owners, &encryptions, &listings);
		fact.settle_by_code(&owners, &encryptions, &by_codes);
		fact
	}

	/// Notes a device's choice of the space that a statement states, with its text, if it is
	/// on the record that it names, for the change's space, and verifies. Returns whether the
	/// statement is of a choice, whether it counts or not.
	fn read_choice(&mut self, change: &impl Header, statement: &Statement, text: &str) -> bool {
		if statement.field != fields::SPACE_CHOICE {
			return false;
		}
		if let Some(choice) = Choice::from_text(text)
			&& choice.record == statement.entity
			&& choice.space == change.space()
			&& choice.verifies()
		{
			self.choices
				.push((*choice.key.as_bytes(), choice.number, choice.holds));
		}
		true
	}

	/// Notes what a statement says of recovery codes, with its text: a code that it names,
	/// with a signature that verifies and on the code's own record; a revocation of a code;
	/// or a listing of the change's own author on its record, signed by a code, for the
	/// change's space, which goes to `by_codes`. Returns whether it says one of these.
	fn read_code_statement(
		&mut self,
		change: &impl Header,
		statement: &Statement,
		text: &str,
		by_codes: &mut Vec<RecoveryListing>,
	) -> bool {
		let field = &statement.field;
		if *field == fields::RECOVERY_KEY {
			if let Some(code) = RecoveryStatement::from_text(text)
				&& code.record(change.space()) == statement.entity
				&& code.verifies()
			{
				self.codes.push(statement.entity);
			}
		} else if *field == fields::RECOVERY_REVOKED {
			if let Some(key) = VerifyingKey::from_text(text) {
				self.code_revokes.push((statement.entity, *key.as_bytes()));
			}
		} else if *field == fields::RECOVERY_LISTING {
			if let Some(listing) = RecoveryListing::from_text(text)
				&& listing.record == statement.entity
				&& listing.space == change.space()
				&& listing.key == *change.author()
				&& listing.verifies()
			{
				by_codes.push(listing);
			}
		} else {
			return false;
		}
		true
	}

	/// Notes the listing of the change's own author by a recovery code, of `by_codes`: one
	/// on a record that the change lists its author on, with the owner and the encryption key
	/// that the change states for the record, and no other, and no mark of an AI agent's
	/// replica, as a code lists none.
	fn settle_by_code(
		&mut self,
		owners: &[(EntityId, EntityId)],
		encryptions: &[(EntityId, nibkeep_base::Hash)],
		by_codes: &[RecoveryListing],
	) {
		self.by_code = by_codes
			.iter()
			.find(|listing| {
				self.lists_on(listing.record, &self.author, false)
					&& only_stated(
						owners,
						encryptions,
						listing.record,
						listing.owner,
						listing.encryption,
					)
					&& !self.marks.contains(&listing.record)
			})
			.map(|listing| ByCode {
				record: listing.record,
				owner: listing.owner,
				code: listing.code_record(),
				recovery_key: *listing.recovery_key.as_bytes(),
			});
	}

	/// Notes what a statement says of groups and members, with its text: a group's record in
	/// a space, an anchor, a group's agreement to join a space or its withdrawal, a record made
	/// for a member through a group, or a rejection of a claim to the admin role. Returns
	/// whether it says one of these.
	fn read_group_statement(&mut self, statement: &Statement, text: &str) -> bool {
		let field = &statement.field;
		if *field == fields::REJECTED
			&& let Ok(claim) = text.parse()
		{
			self.rejects.push(claim);
		} else if *field == fields::MADE_FOR
			&& let Some(key) = VerifyingKey::from_text(text)
		{
			self.made_for.push((statement.entity, *key.as_bytes()));
		} else if *field == fields::GROUP
			&& let Some(group) = GroupRef::from_text(text)
		{
			self.group_records
				.push((statement.entity, group, statement.id));
		} else if *field == fields::ANCHOR
			&& let Some(anchor) = Anchor::from_text(text)
		{
			self.anchors.push(anchor);
		} else if *field == fields::JOINED
			&& let Some(joining) = Joining::from_text(text)
		{
			self.joinings.push(joining);
		} else if *field == fields::WITHDRAWN
			&& let Some(joining) = Joining::from_text(text)
		{
			self.withdrawals.push(joining);
		} else {
			return false;
		}
		true
	}

	/// Notes, for each key it lists, whether the change holds the listing that the key
	/// signed, on the same record, with the owner, the mark and the encryption key that the
	/// change states, as `owners`, `encryptions` and `listings` hold them; and the owner it
	/// states for the record.
	fn settle_listings(
		&mut self,
		owners: &[(EntityId, EntityId)],
		encryptions: &[(EntityId, nibkeep_base::Hash)],
		listings: &[Listing],
	) {
		// The change states one owner and one encryption key for the record, those that the
		// listing signed: no other may take the key over, or have keys sealed to it.
		let only = |record: EntityId, owner: EntityId, encryption: nibkeep_base::Hash| {
			only_stated(owners, encryptions, record, owner, encryption)
		};
		let signed: Vec<Option<&Listing>> = self
			.lists
			.iter()
			.map(|(replica, key)| {
				listings.iter().find(|listing| {
					listing.record == *replica
						&& listing.key.as_bytes() == key
						&& only(*replica, listing.owner, listing.encryption)
						&& listing.ai_agent == self.marks.contains(replica)
				})
			})
			.collect();
		self.signed = signed.iter().map(Option::is_some).collect();
		self.owners = self
			.lists
			.iter()
			.zip(&signed)
			.map(|((replica, _), listing)| match listing {
				Some(listing) => Some(listing.owner),
				None => owners
					.iter()
					.find(|(record, _)| record == replica)
					.map(|(_, owner)| *owner),
			})
			.collect();
	}

	/// The change.
	#[must_use]
	pub fn id(&self) -> ChangeId {
		self.id
	}

	/// Its place among the governing changes: by timestamp, and then by ID.
	#[must_use]
	pub fn order(&self) -> (Timestamp, ChangeId) {
		self.order
	}

	/// Whether it is the space's first change.
	#[must_use]
	pub fn is_root(&self) -> bool {
		self.root
	}

	/// The devices' choices of the space that it states, which verify on the records they
	/// name: each key, its choice's number and whether it holds the space.
	#[must_use]
	pub fn choices(&self) -> &[(KeyBytes, u64, bool)] {
		&self.choices
	}

	/// Whether it is the first change of a space with members, which gives a role.
	#[must_use]
	pub fn founds_members(&self) -> bool {
		self.root && self.gives_role
	}

	/// Whether it lists `key` on `replica`'s record: where listings must be `signed`, as in
	/// every governed space, only if it holds the listing that the key signed.
	#[must_use]
	pub fn lists_on(&self, replica: EntityId, key: &KeyBytes, signed: bool) -> bool {
		self.lists
			.iter()
			.zip(&self.signed)
			.any(|((on, listed), holds)| *on == replica && listed == key && (*holds || !signed))
	}

	/// Keeps only the listings that the listed keys signed, as a governed space counts no
	/// other, and only the marks of AI agents that such listings carry; and a listing of its
	/// author by a recovery code only where the change holds the author's own listing on the
	/// same record, for the same owner, as every listing there does.
	pub fn keep_signed(&mut self) {
		if let Some(by_code) = self.by_code {
			let signed = self.lists.iter().enumerate().any(|(index, listed)| {
				*listed == (by_code.record, self.author)
					&& self.signed[index]
					&& self.owners[index] == Some(by_code.owner)
			});
			if !signed {
				self.by_code = None;
			}
		}
		let mut kept = Vec::new();
		let mut owners = Vec::new();
		let mut devices = Vec::new();
		for (index, listed) in self.lists.iter().enumerate() {
			if self.signed[index] {
				kept.push(*listed);
				owners.push(self.owners[index]);
				devices.push(self.devices[index]);
			}
		}
		self.marks
			.retain(|replica| kept.iter().any(|(listed, _)| listed == replica));
		self.signed = vec![true; kept.len()];
		self.lists = kept;
		self.owners = owners;
		self.devices = devices;
	}

	/// Whether it lists a key.
	fn lists(&self, key: &KeyBytes) -> bool {
		self.lists.iter().any(|(_, listed)| listed == key)
	}

	/// The owner that it states for a key it lists, if it lists it and states one.
	fn owner_of(&self, key: &KeyBytes) -> Option<EntityId> {
		self.lists
			.iter()
			.position(|(_, listed)| listed == key)
			.and_then(|index| self.owners[index])
	}

	/// Whether it makes a record of a group.
	fn makes_group_record(&self, record: EntityId) -> bool {
		self.group_records.iter().any(|(made, ..)| *made == record)
	}

	/// The role it gives a member, if it gives one: the lowest, if it gives several.
	fn role_of(&self, member: EntityId) -> Option<Role> {
		self.roles
			.iter()
			.filter(|(given, _)| *given == member)
			.map(|(_, role)| *role)
			.min()
	}
}

/// Whether a change states, for `record`, an owner and an encryption key, as `owners` and
/// `encryptions` hold them, and only `owner` and the encryption key whose hash is
/// `encryption`: no other may take a listed key over, or have keys sealed to it.
fn only_stated(
	owners: &[(EntityId, EntityId)],
	encryptions: &[(EntityId, nibkeep_base::Hash)],
	record: EntityId,
	owner: EntityId,
	encryption: nibkeep_base::Hash,
) -> bool {
	let mut stated_owners = owners.iter().filter(|(of, _)| *of == record);
	let mut stated_keys = encryptions.iter().filter(|(of, _)| *of == record);
	stated_owners.clone().next().is_some()
		&& stated_owners.all(|(_, stated)| *stated == owner)
		&& stated_keys.clone().next().is_some()
		&& stated_keys.all(|(_, stated)| *stated == encryption)
}

/// What the governing changes that count say: whose changes count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Governance {
	/// Whether the space's first change governs it. If not, every change counts.
	governed: bool,
	/// Whether it is a space with members, whose first change gives a role, where only the
	/// listings that the listed keys signed count.
	members: bool,
	/// The keys that counting governing changes list.
	listed: BTreeSet<KeyBytes>,
	/// The keys that the counting listings of the groups it follows list, at any level, but
	/// AI agents': a change of such a key may count, as its author's standing at the change
	/// decides, also once its paths have ended.
	listed_in_groups: BTreeSet<KeyBytes>,
	/// For each listed key, the counting changes that list it.
	listed_by: BTreeMap<KeyBytes, BTreeSet<ChangeId>>,
	/// For each revoked key, the counting changes that revoke it.
	revoked: BTreeMap<KeyBytes, BTreeSet<ChangeId>>,
	/// The revoked keys that would stay if they revoked back the keys that revoked them, as
	/// of two that revoke each other.
	may_revoke_back: BTreeSet<KeyBytes>,
	/// The ordinary changes that counting governing changes accept, each with the changes
	/// that accept it and their authors.
	accepted: BTreeMap<ChangeId, Vec<(ChangeId, KeyBytes)>>,
	/// The keys that any governing change the replica can read lists, counting or not, but
	/// a key that lists itself, other than in the space's first change or by a recovery code
	/// that such a change names, and an AI agent's: those whose changes the replica takes, to
	/// count or to set aside.
	known: BTreeSet<KeyBytes>,
	/// The records of the recovery codes that any governing change the replica can read
	/// names, counting or not, but an AI agent's: a key that lists itself by one of them is
	/// known.
	named_codes: BTreeSet<EntityId>,
	/// The keys that are AI agents', as the listings that gave them their time mark them.
	ai_agents: BTreeSet<KeyBytes>,
	/// The counting changes that make each field secret for good.
	secret: BTreeMap<FieldName, BTreeSet<ChangeId>>,
	/// In a space with members, what deciding ordinary changes by their authors' roles
	/// needs.
	view: Option<View>,
}

impl Governance {
	/// In a space with members, each member that the counting governing changes give a
	/// role, with the role they have by all of them.
	pub fn roles_now(&self) -> BTreeMap<nibkeep_base::EntityId, Role> {
		self.view.as_ref().map_or_default(View::roles_now)
	}

	/// What the groups that the space follows give in it, if it follows any.
	pub fn reach(&self) -> Option<&Reach> {
		self.view.as_ref().and_then(View::reach)
	}

	/// In a space with members, whether it allows AI agents now.
	pub fn allows_ai_agents(&self) -> bool {
		self.view.as_ref().is_some_and(View::allows_ai_agents)
	}

	/// What the counting ones of `facts` say, where, in a space with members, `ancestors`
	/// says which facts each builds on, and `following` holds the groups it follows.
	#[must_use]
	pub fn of(
		(space, governed): (SpaceId, bool),
		facts: &[Fact],
		ancestors: Option<&[BTreeSet<usize>]>,
		counting: &BTreeSet<ChangeId>,
		following: &BTreeMap<SpaceId, Followed>,
	) -> Self {
		let members = facts.iter().any(Fact::founds_members);
		let reach = ancestors.filter(|_| members).and_then(|ancestors| {
			let mut reach = Reach::new(space, facts, ancestors, following)?;
			let anchors = facts
				.iter()
				.filter(|fact| counting.contains(&fact.id) && reach.is_anchor(&fact.id))
				.map(|fact| fact.id)
				.collect();
			reach.assume(anchors, following, &mut BTreeMap::new());
			Some(reach)
		});
		let Listings {
			first_listed,
			ai_agents,
			through_code,
			..
		} = listings(facts);
		let mut governance = Self {
			governed,
			members,
			ai_agents,
			..Self::default()
		};
		if let Some(reach) = &reach {
			governance.known.extend(reach.known_keys().iter().copied());
			governance.listed_in_groups = reach.listed_in_groups();
		}
		governance.view = ancestors.filter(|_| members).map(|ancestors| {
			View::of(facts, ancestors, counting)
				.with_reach(space, reach)
				.with_claims(facts, ancestors, counting)
		});
		if let Some(view) = &governance.view {
			governance.listed.extend(view.listed_through_groups_now());
		}
		for fact in facts {
			if governance.ai_agents.contains(&fact.author) {
				continue;
			}
			governance.known.extend(
				fact.lists
					.iter()
					.map(|(_, key)| *key)
					.filter(|key| fact.root || *key != fact.author),
			);
			governance.named_codes.extend(fact.codes.iter().copied());
		}
		// A key that lists itself by a code that a change names, so that a device listed by
		// a code introduces itself, and strangers still fill nobody's log.
		for fact in facts {
			if fact
				.by_code
				.is_some_and(|by_code| governance.named_codes.contains(&by_code.code))
			{
				governance.known.insert(fact.author);
			}
		}
		for fact in facts.iter().filter(|fact| counting.contains(&fact.id)) {
			for (_, key) in &fact.lists {
				governance.listed.insert(*key);
				governance
					.listed_by
					.entry(*key)
					.or_default()
					.insert(fact.id);
			}
			for key in &fact.revokes {
				governance.revoked.entry(*key).or_default().insert(fact.id);
			}
			for accepted in &fact.accepts {
				governance
					.accepted
					.entry(*accepted)
					.or_default()
					.push((fact.id, fact.author));
			}
			for field in &fact.secret {
				governance
					.secret
					.entry(field.clone())
					.or_default()
					.insert(fact.id);
			}
		}
		// Of two keys that revoke each other, the one listed through a code, or else the one
		// listed later, sets nothing aside, as `attackers` decides. In a space with members,
		// only a member's own replicas revoke its replicas.
		let rank = |key: &KeyBytes| (through_code.contains(key), first_listed.get(key).copied());
		let authors: BTreeMap<ChangeId, &KeyBytes> =
			facts.iter().map(|fact| (fact.id, &fact.author)).collect();
		for (key, revocations) in &governance.revoked {
			let outranked = |revocation: &ChangeId| {
				authors
					.get(revocation)
					.is_some_and(|author| rank(author) > rank(key))
			};
			if revocations.iter().all(outranked) {
				governance.may_revoke_back.insert(*key);
			}
		}
		governance
	}

	/// Whether a revoked key would stay if it revoked back the keys that revoked it: whether
	/// it ranks above each, as of two that revoke each other. False for a key that is not
	/// revoked.
	#[must_use]
	pub fn may_revoke_back(&self, key: &KeyBytes) -> bool {
		self.may_revoke_back.contains(key)
	}

	/// The counting changes that make a field secret for good, if any does.
	#[must_use]
	pub fn made_secret(&self, field: &FieldName) -> Option<&BTreeSet<ChangeId>> {
		self.secret.get(field)
	}

	/// Whether an ordinary change counts, where `had(other)` says whether a counting
	/// revocation of its author, or a counting change that accepts it, builds on it. Only a
	/// change that another key's replica had can be accepted, so that a replica cannot
	/// accept, before it is revoked, what it keeps back to send later.
	pub fn counts(&self, change: &impl Header, mut had: impl FnMut(ChangeId) -> bool) -> bool {
		if !self.governed {
			return true;
		}
		let author = change.author().as_bytes();
		// What an AI agent's change purges or seals is never accepted.
		if self.ai_agents.contains(author) && change.purges_or_seals() {
			return false;
		}
		self.is_accepted(change, &mut had)
			|| (self.listed.contains(author) && self.unrevoked(change, had))
	}

	/// Whether a counting change of another key accepts a change, where `had(acceptance)`
	/// says whether the acceptance builds on it.
	pub fn is_accepted(&self, change: &impl Header, mut had: impl FnMut(ChangeId) -> bool) -> bool {
		let author = change.author().as_bytes();
		self.accepted.get(&change.id()).is_some_and(|acceptors| {
			acceptors
				.iter()
				.any(|(acceptor, by)| by != author && had(*acceptor))
		})
	}

	/// Whether every counting revocation of a change's author is the change or builds on it,
	/// as `had(revocation)` says.
	pub fn unrevoked(&self, change: &impl Header, mut had: impl FnMut(ChangeId) -> bool) -> bool {
		self.revoked
			.get(change.author().as_bytes())
			.is_none_or(|revocations| {
				revocations
					.iter()
					.all(|&revocation| revocation == change.id() || had(revocation))
			})
	}

	/// Whether the space's first change governs it. If not, every change counts.
	#[must_use]
	pub fn governed(&self) -> bool {
		self.governed
	}

	/// Whether it is a space with members, whose first change gives a role.
	#[must_use]
	pub fn members(&self) -> bool {
		self.members
	}

	/// In a space with members, what deciding ordinary changes by their authors' roles needs.
	#[must_use]
	pub fn view(&self) -> Option<&View> {
		self.view.as_ref()
	}

	/// The keys that the groups the space follows list, at any level, but AI agents'.
	#[must_use]
	pub fn listed_in_groups(&self) -> &BTreeSet<KeyBytes> {
		&self.listed_in_groups
	}

	/// The counting changes that list a key.
	#[must_use]
	pub fn listings_of(&self, key: &KeyBytes) -> Option<&BTreeSet<ChangeId>> {
		self.listed_by.get(key)
	}

	/// The ordinary changes that counting governing changes accept.
	#[must_use]
	pub fn accepted(&self) -> BTreeSet<ChangeId> {
		self.accepted.keys().copied().collect()
	}

	/// The counting changes that make each field secret for good.
	#[must_use]
	pub fn secret_fields(&self) -> &BTreeMap<FieldName, BTreeSet<ChangeId>> {
		&self.secret
	}

	/// Whether a counting governing change lists an AI agent's key.
	#[must_use]
	pub fn lists_ai_agents(&self) -> bool {
		self.ai_agents.iter().any(|key| self.listed.contains(key))
	}

	/// The counting changes that revoke a key.
	#[must_use]
	pub fn revocations_of(&self, key: &KeyBytes) -> Option<&BTreeSet<ChangeId>> {
		self.revoked.get(key)
	}

	/// Every counting change that revokes a key.
	#[must_use]
	pub fn revocations(&self) -> BTreeSet<ChangeId> {
		self.revoked.values().flatten().copied().collect()
	}

	/// Whether a counting governing change lists a key.
	#[must_use]
	pub fn is_listed(&self, key: &KeyBytes) -> bool {
		self.listed.contains(key)
	}

	/// Whether a key is an AI agent's, as the listing that gave it its time marks it.
	#[must_use]
	pub fn is_ai_agent(&self, key: &KeyBytes) -> bool {
		self.governed && self.ai_agents.contains(key)
	}

	/// Whether any governing change that the replica can read lists a key, as
	/// `Governance::known` says, or the space's first change does not govern it.
	#[must_use]
	pub fn is_known(&self, key: &KeyBytes) -> bool {
		!self.governed || self.known.contains(key)
	}

	/// Whether a governing change that lists its own author would be taken from another
	/// replica although nothing lists its author yet: whether `fact`, what it says, lists its
	/// author by a recovery code that a governing change the replica can read names, counting
	/// or not, but an AI agent's.
	#[must_use]
	pub fn takes_listing_by_code(&self, fact: &Fact) -> bool {
		self.governed
			&& fact
				.by_code
				.is_some_and(|by_code| self.named_codes.contains(&by_code.code))
	}

	/// The keys whose changes may count less than before: those no longer listed, those
	/// that are AI agents' and were not, and those with a revocation that counts now and did
	/// not before.
	#[must_use]
	pub fn losing(&self, before: &Self) -> BTreeSet<KeyBytes> {
		let mut losing: BTreeSet<KeyBytes> =
			before.listed.difference(&self.listed).copied().collect();
		losing.extend(self.ai_agents.difference(&before.ai_agents));
		if let Some(view) = &self.view {
			let none = View::default();
			let earlier = before.view.as_ref().unwrap_or(&none);
			losing.extend(view.losing(earlier));
			// An AI agent's change counts only while the space allows AI agents.
			if view.ai_allowed_differs(earlier) {
				losing.extend(self.ai_agents.iter().copied());
			}
		}
		for (key, revocations) in &self.revoked {
			let earlier = before.revoked.get(key);
			if revocations
				.iter()
				.any(|revocation| !earlier.is_some_and(|earlier| earlier.contains(revocation)))
			{
				losing.insert(*key);
			}
		}
		losing
	}
}

/// Whether a governing change counts, as far as decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Label {
	Undecided,
	Counts,
	SetAside,
}

/// When each key was first listed, the member its first listing named, and which keys are
/// AI agents', from `facts` in their order, whether they count or not, so that what decides
/// it depends on no change that it decides. A listing gives a key its time only if its author had one before it, back to the
/// space's first change, which lists its own author: a key that nobody listed, or that lists
/// itself, cannot make a key older by a listing it dates early. A record made for a member
/// through a group gives the key it names its time and the record as its member the same
/// way, as that member's replica lists itself there. A key is an AI agent's if the listing
/// that gave it its time marks the replica it lists the key for as one; an AI agent's key
/// lists nothing.
fn listings(facts: &[Fact]) -> Listings {
	let mut first_listed: BTreeMap<KeyBytes, (Timestamp, ChangeId)> = BTreeMap::new();
	let mut first_owner: BTreeMap<KeyBytes, EntityId> = BTreeMap::new();
	let mut ai_agents = BTreeSet::new();
	let mut through_code = BTreeSet::new();
	// The codes that changes before, by keys listed before them, name.
	let mut named: BTreeSet<EntityId> = BTreeSet::new();
	for fact in facts {
		let mut author_listed = if fact.root {
			fact.lists(&fact.author)
		} else {
			first_listed
				.get(&fact.author)
				.is_some_and(|&listed| listed < fact.order)
		};
		// A key that lists itself by a code that such a change names counts as added then,
		// and as listed through a code, as does every other key that the change lists.
		if !author_listed
			&& !first_listed.contains_key(&fact.author)
			&& let Some(by_code) = fact.by_code
			&& named.contains(&by_code.code)
		{
			first_listed.insert(fact.author, fact.order);
			first_owner.insert(fact.author, by_code.owner);
			through_code.insert(fact.author);
			author_listed = true;
		}
		if !author_listed || ai_agents.contains(&fact.author) {
			continue;
		}
		named.extend(fact.codes.iter().copied());
		// A key listed by one listed through a code is listed through a code too.
		let lister_through_code = through_code.contains(&fact.author);
		for (index, (replica, key)) in fact.lists.iter().enumerate() {
			if (fact.root || *key != fact.author) && !first_listed.contains_key(key) {
				first_listed.insert(*key, fact.order);
				if let Some(owner) = fact.owners[index] {
					first_owner.insert(*key, owner);
				}
				if fact.marks.contains(replica) {
					ai_agents.insert(*key);
				}
				if lister_through_code {
					through_code.insert(*key);
				}
			}
		}
		for (record, key) in &fact.made_for {
			if !first_listed.contains_key(key) {
				first_listed.insert(*key, fact.order);
				first_owner.insert(*key, *record);
				if lister_through_code {
					through_code.insert(*key);
				}
			}
		}
	}
	Listings {
		first_listed,
		first_owner,
		ai_agents,
		through_code,
	}
}

/// When each key was first listed, the owner its first listing named, which keys are AI
/// agents', and which were listed through a recovery code, as [`listings`] finds them.
struct Listings {
	first_listed: BTreeMap<KeyBytes, (Timestamp, ChangeId)>,
	first_owner: BTreeMap<KeyBytes, EntityId>,
	ai_agents: BTreeSet<KeyBytes>,
	/// The keys that a code listed, or that a key listed through a code listed, back along
	/// who listed whom: of two replicas that revoke each other, one listed so loses to one
	/// that was not.
	through_code: BTreeSet<KeyBytes>,
}

/// Decides which governing changes count, of `facts` in their order, in `space`. `had(a, b)`
/// says whether `a` is `b` or one of its ancestors. A governing change is never accepted:
/// only ordinary ones are. An AI agent's governing change never counts. In a space with
/// members, where `ancestors` says which facts each builds on, a change counts only if its
/// author's role allows it, and none counts that leaves the space without a direct admin;
/// and, where it follows the groups `groups`, by what they give at every level.
///
/// # Errors
///
/// Returns what `had` returns when it fails.
pub fn decide<E>(
	facts: &[Fact],
	ancestors: Option<&[BTreeSet<usize>]>,
	(space, groups): (SpaceId, Option<&BTreeMap<SpaceId, Followed>>),
	mut had: impl FnMut(ChangeId, ChangeId) -> Result<bool, E>,
) -> Result<BTreeSet<ChangeId>, E> {
	// When each key was first listed: of two replicas that revoke each other, the one listed
	// first stays.
	let Listings {
		first_listed,
		first_owner,
		ai_agents,
		through_code,
	} = listings(facts);
	// In a space that follows groups, the anchors that count decide what the groups give,
	// which decides which anchors count: those taken to count are decided again until they
	// are those that do.
	let mut reach = match (ancestors, groups) {
		(Some(ancestors), Some(following)) => {
			Reach::new(space, facts, ancestors, following).map(|reach| (reach, following))
		}
		_ => None,
	};
	// What each group's rules decide among the changes that the anchors name, found once.
	let mut decided = BTreeMap::new();
	if let Some((reach, following)) = &mut reach {
		let anchors = reach.complete_anchors();
		reach.assume(anchors, following, &mut decided);
	}
	// Once the rounds run out, fewer anchors are taken each time, which ends, and those not
	// taken are set aside: so the anchors that count are always those taken.
	let mut not_taken: BTreeSet<usize> = BTreeSet::new();
	let attackers = attackers(facts, (&first_listed, &through_code), &mut had)?;
	let (vouchers, code_attackers) = by_code(facts, &mut had)?;
	// The changes before each that list its author: the facts come in their order.
	let listings: Vec<Vec<usize>> = facts
		.iter()
		.enumerate()
		.map(|(index, fact)| {
			(0..index)
				.filter(|&earlier| facts[earlier].lists(&fact.author))
				.collect()
		})
		.collect();
	let mut round = 0;
	loop {
		round += 1;
		let members = ancestors.map(|ancestors| {
			Members::new(
				facts,
				ancestors,
				&first_owner,
				(space, reach.as_ref().map(|(reach, _)| reach)),
			)
		});
		// Changes that would leave the space without a direct admin are set aside, one at a
		// time, and the rest decided again.
		let mut refused: BTreeSet<usize> = not_taken.clone();
		let labels = loop {
			let labels = label(
				facts,
				(&ai_agents, &refused),
				(&listings, &attackers),
				(&vouchers, &code_attackers),
				members.as_ref(),
			);
			if let Some(members) = &members
				&& let Some(leaving) = members.leaving_no_admin(&labels)
				&& refused.insert(leaving)
			{
				continue;
			}
			break labels;
		};
		let counting: BTreeSet<ChangeId> = facts
			.iter()
			.zip(&labels)
			.filter(|(_, label)| **label == Label::Counts)
			.map(|(fact, _)| fact.id)
			.collect();
		if let Some((reach, following)) = &mut reach
			&& let Some(taken) = next_anchors(facts, reach, &counting, round, &mut not_taken)
		{
			reach.assume(taken, following, &mut decided);
			continue;
		}
		return Ok(counting);
	}
}

/// For each of `facts`, the revocations of its author that do not build on it, which set it
/// aside; of two that revoke each other's authors, only the one by the key listed earlier,
/// as `first_listed` says, unless that key was listed through a code, as `through_code` says,
/// and the other's was not. `had(a, b)` says whether `a` is `b` or one of its ancestors.
fn attackers<E>(
	facts: &[Fact],
	(first_listed, through_code): (
		&BTreeMap<KeyBytes, (Timestamp, ChangeId)>,
		&BTreeSet<KeyBytes>,
	),
	had: &mut impl FnMut(ChangeId, ChangeId) -> Result<bool, E>,
) -> Result<Vec<BTreeSet<usize>>, E> {
	// The changes that revoke each key.
	let mut revoking: BTreeMap<KeyBytes, BTreeSet<usize>> = BTreeMap::new();
	for (index, fact) in facts.iter().enumerate() {
		for key in &fact.revokes {
			revoking.entry(*key).or_default().insert(index);
		}
	}
	// Each change is set aside by the revocations of its author that do not build on it.
	let mut attackers: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); facts.len()];
	for (index, fact) in facts.iter().enumerate() {
		for &revocation in revoking.get(&fact.author).into_iter().flatten() {
			if revocation != index && !had(fact.id, facts[revocation].id)? {
				attackers[index].insert(revocation);
			}
		}
	}
	// Of two that revoke each other's authors, the one by the key listed through a code, or
	// else the one by the key listed later, sets nothing aside: a device in hand beats a code.
	let mutual: Vec<(usize, usize)> = attackers
		.iter()
		.enumerate()
		.flat_map(|(index, by)| by.iter().map(move |&by| (index, by)))
		.filter(|&(index, by)| attackers[by].contains(&index))
		.collect();
	for (index, by) in mutual {
		let seniority = |at: usize| {
			let author = &facts[at].author;
			(
				through_code.contains(author),
				first_listed.get(author).copied(),
			)
		};
		if seniority(by) > seniority(index) {
			attackers[index].remove(&by);
		}
	}
	Ok(attackers)
}

/// For each of `facts` that lists its author by a recovery code: the facts before it that
/// name the code and that it builds on, which vouch for it, and those that revoke the code
/// without building on it, which set it aside. `had(a, b)` says whether `a` is `b` or one of
/// its ancestors.
#[expect(clippy::type_complexity, reason = "two lists, one for each fact")]
fn by_code<E>(
	facts: &[Fact],
	had: &mut impl FnMut(ChangeId, ChangeId) -> Result<bool, E>,
) -> Result<(Vec<Vec<usize>>, Vec<Vec<usize>>), E> {
	let mut vouchers: Vec<Vec<usize>> = vec![Vec::new(); facts.len()];
	let mut code_attackers: Vec<Vec<usize>> = vec![Vec::new(); facts.len()];
	for (index, fact) in facts.iter().enumerate() {
		let Some(by_code) = fact.by_code else {
			continue;
		};
		for (at, other) in facts.iter().enumerate() {
			if at < index && other.codes.contains(&by_code.code) && had(other.id, fact.id)? {
				vouchers[index].push(at);
			}
			if at != index
				&& other
					.code_revokes
					.contains(&(by_code.code, by_code.recovery_key))
				&& !had(fact.id, other.id)?
			{
				code_attackers[index].push(at);
			}
		}
	}
	Ok((vouchers, code_attackers))
}

/// The anchors to take to count next, after a round of deciding a space's changes, with
/// `reach` taking those it took, of which those in `counting` count: `None` if they are
/// those taken. Before [`ANCHOR_ROUNDS`] rounds, those that counted; after, those that
/// counted and were taken, which are fewer each time, with the others put in `not_taken`, to
/// be set aside.
fn next_anchors(
	facts: &[Fact],
	reach: &Reach,
	counting: &BTreeSet<ChangeId>,
	round: usize,
	not_taken: &mut BTreeSet<usize>,
) -> Option<BTreeSet<ChangeId>> {
	let anchors: BTreeSet<ChangeId> = counting
		.iter()
		.filter(|change| reach.is_anchor(change))
		.copied()
		.collect();
	if anchors == *reach.counting() {
		return None;
	}
	if round < ANCHOR_ROUNDS {
		return Some(anchors);
	}
	let fewer: BTreeSet<ChangeId> = anchors.intersection(reach.counting()).copied().collect();
	*not_taken = facts
		.iter()
		.enumerate()
		.filter(|(_, fact)| reach.is_anchor(&fact.id) && !fewer.contains(&fact.id))
		.map(|(at, _)| at)
		.collect();
	Some(fewer)
}

/// Labels each of `facts`, as far as the rules decide: those of AI agents, and those
/// `refused`, are set aside; another counts if a counting change before it lists its author,
/// or it lists its author by a recovery code that a counting change it builds on names and
/// no counting revocation of the code sets aside, no counting revocation sets it aside, and,
/// in a space with members, its author's role allows it and no counting change lowers that
/// role without knowing of it. `listings` says which facts before each list its author, and
/// `attackers` which revocations set it aside; `vouchers` which facts it builds on name the
/// code it lists its author by, and `code_attackers` which revocations of that code do not
/// build on it.
fn label(
	facts: &[Fact],
	(ai_agents, refused): (&BTreeSet<KeyBytes>, &BTreeSet<usize>),
	(listings, attackers): (&[Vec<usize>], &[BTreeSet<usize>]),
	(vouchers, code_attackers): (&[Vec<usize>], &[Vec<usize>]),
	members: Option<&Members<'_>>,
) -> Vec<Label> {
	if let Some(members) = members {
		members.forget_decided();
	}
	let mut labels: Vec<Label> = facts
		.iter()
		.enumerate()
		.map(|(index, fact)| {
			if ai_agents.contains(&fact.author) || refused.contains(&index) {
				Label::SetAside
			} else {
				Label::Undecided
			}
		})
		.collect();
	let mut changed = true;
	while changed {
		changed = false;
		for index in 0..facts.len() {
			if labels[index] != Label::Undecided {
				continue;
			}
			let fact = &facts[index];
			// A key that a group in the space lists, by what the change knows, is listed.
			let lists_itself = (fact.root && fact.lists(&fact.author))
				|| members.is_some_and(|members| members.listed_through_groups(index));
			// A listing by a code counts once a counting change that it builds on names the
			// code, and every counting revocation of the code builds on it.
			let by_code = vouchers[index]
				.iter()
				.any(|&at| labels[at] == Label::Counts)
				&& code_attackers[index]
					.iter()
					.all(|&at| labels[at] == Label::SetAside);
			let may_be_by_code = vouchers[index]
				.iter()
				.any(|&at| labels[at] != Label::SetAside)
				&& !code_attackers[index]
					.iter()
					.any(|&at| labels[at] == Label::Counts);
			let listed = lists_itself
				|| by_code
				|| listings[index]
					.iter()
					.any(|&at| labels[at] == Label::Counts);
			let may_be_listed = lists_itself
				|| may_be_by_code
				|| listings[index]
					.iter()
					.any(|&at| labels[at] != Label::SetAside);
			let mut revoked = attackers[index]
				.iter()
				.any(|&at| labels[at] == Label::Counts);
			let mut not_revoked = attackers[index]
				.iter()
				.all(|&at| labels[at] == Label::SetAside);
			// In a space with members, what the author's role allows, and the changes that
			// lower that role without knowing of this one, which set it aside as revocations
			// do. The first change is decided by its listing alone.
			let (mut allowed, mut refused) = (true, false);
			if let Some(members) = members
				&& !fact.root
			{
				match members.verdict(index, &labels) {
					Verdict::Undecided => allowed = false,
					Verdict::Refused => (allowed, refused) = (false, true),
					Verdict::Allowed { .. } => match members.kept(index, &labels) {
						Kept::Yes => {}
						Kept::No => (revoked, not_revoked) = (true, false),
						Kept::Undecided => not_revoked = false,
					},
				}
			}
			let label = if listed && not_revoked && allowed {
				Label::Counts
			} else if !may_be_listed || revoked || refused {
				Label::SetAside
			} else {
				continue;
			};
			labels[index] = label;
			changed = true;
		}
	}
	labels
}

/// A group that a space's governing changes add, as `Log::groups` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceGroup {
	/// The group's record in the space, a member's record there.
	pub record: EntityId,
	/// The group and its first change.
	pub group: GroupRef,
	/// The statement that adds the group, which its agreement names.
	pub addition: StatementId,
	/// The record's role now, if a counting change gives it one.
	pub role: Option<Role>,
	/// Whether the group agrees to the addition, and has not withdrawn, by every group change
	/// that the counting anchors name.
	pub agreed: bool,
}

/// A claim to the admin role in a space, as `Log::claims` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
	/// The change that made it.
	pub claim: ChangeId,
	/// The claimant's record.
	pub claimant: EntityId,
	/// When the claimant made it, by their timestamp.
	pub made: Timestamp,
	/// When a replica of another member first saw it, if one did.
	pub seen: Option<Timestamp>,
	/// When it takes effect, 30 days after it was first seen.
	pub effective: Option<Timestamp>,
	/// When it is settled, 30 days after that: until then the new admin can only undo, and an
	/// admin's rejection that another member sees voids it.
	pub settled: Option<Timestamp>,
	/// Whether an admin's rejection, seen in time, voids it.
	pub rejected: bool,
}

/// A statement of a governing change, as `Log::governing_statements` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoverningStatement {
	/// The change.
	pub change: ChangeId,
	/// The change's author.
	pub author: VerifyingKey,
	/// The change's timestamp.
	pub timestamp: Timestamp,
	/// Whether the change counts.
	pub counts: bool,
	/// The statement.
	pub statement: Statement,
	/// For a statement of a verifying key, whether it lists the key: in a space with
	/// members, only if the same change holds the listing that the key signed.
	pub lists: bool,
}

impl GoverningStatement {
	/// The statement's text, if its change counts and it states `field`.
	#[must_use]
	pub fn counting_text(&self, field: &FieldName) -> Option<&str> {
		(self.counts && self.statement.field == *field)
			.then(|| self.statement.value.as_text())
			.flatten()
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_base::Hash;
	use nibkeep_crypto::SigningKey;
	use nibkeep_testkit::TestRng;

	use super::*;
	use crate::Choice;

	/// A governing change's header, for reading what its payload says.
	struct Governing {
		id: ChangeId,
		author: VerifyingKey,
		space: SpaceId,
	}

	impl Header for Governing {
		fn id(&self) -> ChangeId {
			self.id
		}
		fn author(&self) -> &VerifyingKey {
			&self.author
		}
		fn timestamp(&self) -> Timestamp {
			Timestamp::new(1_000, 0)
		}
		fn is_root(&self) -> bool {
			false
		}
		fn space(&self) -> SpaceId {
			self.space
		}
		fn purges_or_seals(&self) -> bool {
			false
		}
	}

	fn stating(rng: &mut TestRng, entity: EntityId, choice: &Choice) -> Operation {
		Operation::assert(Statement::new(
			rng.statement_id(),
			entity,
			fields::SPACE_CHOICE,
			Value::Text(choice.to_text()),
		))
	}

	#[test]
	fn a_fact_reads_only_choices_that_verify_on_their_own_record_and_space() {
		let mut rng = TestRng::new("a fact's choices");
		let space = rng.space_id();
		let device = SigningKey::generate(&mut rng);
		let author = SigningKey::generate(&mut rng);
		let good = Choice::sign(&device, space, 2, false);
		let elsewhere = Choice::sign(&device, rng.space_id(), 3, false);
		let mut tampered = Choice::sign(&device, space, 4, true);
		tampered.holds = false;
		let another_record = rng.entity_id();
		let operations = vec![
			stating(&mut rng, good.record, &good),
			// On another record than the one it names.
			stating(
				&mut rng,
				another_record,
				&Choice::sign(&device, space, 5, true),
			),
			// For another space, on the record it names.
			stating(&mut rng, elsewhere.record, &elsewhere),
			stating(&mut rng, tampered.record, &tampered),
		];
		let header = Governing {
			id: ChangeId::from_hash(Hash::of(b"a change")),
			author: author.verifying_key(),
			space,
		};
		let fact = Fact::read(&header, &operations);
		assert_eq!(
			fact.choices(),
			&[(*device.verifying_key().as_bytes(), 2, false)]
		);
	}
}
