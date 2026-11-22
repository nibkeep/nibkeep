//! The members of a shared space and their roles, as its counting governing changes say,
//! and the two sides of an invitation to it: the admin's, which confirms who asks and adds
//! the member, and the joiner's, which signs its listing and takes the space
//! ([Identity](../../../../docs/spec/identity.md#9-members-and-invitations),
//! [technical design](../../../../docs/Technical-design.md#13-sharing)).

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{VerifyingKey, key_hash};
use nibkeep_governance::{
	InvitationGrant, InvitationRequest, Listing, NewMember, Offer, Role as MemberRole, SpaceEntry,
	SpaceReplica, add_member, encryption_hash, replica_record,
};
use nibkeep_model::storage::Statements;
use nibkeep_model::{Operation, Statement, Value, fields};
use nibkeep_sync::{Accepting, Inviting};

use super::Library;
use super::entities::display_name;
use crate::NodeError;

/// A member of a shared space, as its counting governing changes say: a user, with a role,
/// and the replicas listed for them there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceMember {
	/// The member's record in the space.
	pub record: EntityId,
	/// The ID of the user whom the member is, as the record states it.
	pub user: Option<EntityId>,
	/// The member's name in the space.
	pub name: Option<String>,
	/// The member's role now: [`MemberRole::None`] for one who left or was removed.
	pub role: MemberRole,
	/// Whether the member is the one this replica is listed for.
	pub this_user: bool,
	/// The member's replicas there, by record.
	pub replicas: Vec<MemberReplica>,
	/// The member's link to a person in the space.
	pub person: PersonLink,
}

/// A member's link to a person in a shared space: it holds when an admin's statement and the
/// member's own name the same person; an admin's own statement counts for both while they are an
/// admin
/// ([users and people](../../../../docs/Design.md#610-users-people-and-devices)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersonLink {
	/// The person the member is linked to, if the two agree, and the person is not deleted.
	pub holds: Option<EntityId>,
	/// The person that an admin's latest statement names, if one states it.
	pub by_admin: Option<EntityId>,
	/// The person that the member's own latest statement names, if they state it.
	pub by_member: Option<EntityId>,
	/// Whether the two agree on a person who is deleted, which holds no link.
	pub deleted: bool,
}

/// A replica that a shared space lists for a member.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "each says something apart of the replica, which lists show apart"
)]
pub struct MemberReplica {
	/// Its record in the space.
	pub record: EntityId,
	/// The name of its app.
	pub app: Option<String>,
	/// The name of its device.
	pub device: Option<String>,
	/// Its endpoint ID, which the others connect to, if its record has one.
	pub endpoint: Option<[u8; 32]>,
	/// Its verifying key.
	pub signing: VerifyingKey,
	/// Whether the space revoked it.
	pub revoked: bool,
	/// Whether it dropped the space: its choice that stands there does not hold it
	/// ([technical design](../../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	pub dropped: bool,
	/// Whether it is an AI agent's, which every member sees.
	pub ai_agent: bool,
	/// Whether it is this replica.
	pub this: bool,
	/// Whether it holds the space's secrets, as given to it by the replica it asked, or as
	/// it set them up.
	pub holds_secrets: bool,
	/// Whether it asks another replica for the space's secrets, which has not given them.
	pub asks_for_secrets: bool,
	/// The replica that it asked for the space's secrets, if it asked one.
	pub secrets_from: Option<EntityId>,
}

/// Who asks to join a space, as the admin's device shows them before the person confirms:
/// the user, by ID and name, the names of the device and app that ask, the members whose
/// records name the same user ID, and whether the user was a member before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitee {
	/// The user's ID.
	pub user: EntityId,
	/// The user's name, as their library has it.
	pub user_name: String,
	/// The name of the device that asks.
	pub device: String,
	/// The name of its app.
	pub app: String,
	/// The members, with a role, whose records name the same user ID: a new member is
	/// another all the same.
	pub same_user: Vec<SpaceMember>,
	/// Whether a member record of the same user has the role none: the user was a member,
	/// left or was removed, and comes back as a new member.
	pub was_member: bool,
}

impl Library {
	/// The members of a shared space, as its counting governing changes say, with their
	/// roles now and their replicas, sorted by name and record.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, and an error if the space
	/// cannot be read.
	pub fn members(&self, space: SpaceId) -> Result<Vec<SpaceMember>, NodeError> {
		self.check_shared(space)?;
		let own = self.replica.signing.verifying_key();
		let this_member = self.member_in(space)?;
		let open = self.space(space)?;
		let log = open.stored.log();
		let state = log.state();
		let roles = log.member_roles(&open.keys)?;
		let (users, owners) = self.users_and_owners(space)?;
		let roster = self.roster(space)?;
		let holders = roster.secrets_holders();
		let mut replicas: BTreeMap<EntityId, Vec<MemberReplica>> = BTreeMap::new();
		for record in roster.replicas() {
			let Some(key) = roster.keys_of(record).first().copied() else {
				continue;
			};
			let Some(owner) = owners.get(key.as_bytes()) else {
				continue;
			};
			let found = nibkeep_governance::replica(state, record)?;
			let device = found.as_ref().and_then(|found| found.device);
			replicas.entry(*owner).or_default().push(MemberReplica {
				record,
				app: display_name(state, &open.types, &self.languages, record)?,
				device: match device {
					Some(device) => display_name(state, &open.types, &self.languages, device)?,
					None => None,
				},
				endpoint: found.and_then(|found| found.endpoint),
				signing: key,
				revoked: log.is_revoked(&key, &open.keys)?,
				dropped: !log.holds_space(&key, &open.keys)?,
				ai_agent: log.is_ai_agent(&key, &open.keys)?,
				this: key == own,
				holds_secrets: holders.contains(&record),
				asks_for_secrets: roster.secrets_encryption_of(record).is_some()
					&& roster
						.secrets_from(record)
						.is_some_and(|from| from != record)
					&& !holders.contains(&record),
				secrets_from: roster.secrets_from(record).filter(|from| *from != record),
			});
		}
		// The groups in it have records of their own there, which no user is.
		let groups: BTreeSet<EntityId> = log
			.groups(&open.keys)?
			.iter()
			.map(|found| found.record)
			.collect();
		let mut members = Vec::new();
		for (&record, &role) in &roles {
			if groups.contains(&record) {
				continue;
			}
			members.push(SpaceMember {
				record,
				user: users.get(&record).copied(),
				name: display_name(state, &open.types, &self.languages, record)?,
				role,
				this_user: this_member == Some(record),
				replicas: replicas.remove(&record).unwrap_or_default(),
				person: person_link((&open.stored, state), (&owners, &roles), record)?,
			});
		}
		members.sort_by(|a, b| (&a.name, a.record).cmp(&(&b.name, b.record)));
		Ok(members)
	}

	/// The member of a shared space that `reference` names: by name, whatever the case, or
	/// by the start of the record's ID, at least four characters long; of several, the one
	/// with a role, if one alone has one.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoMember`] if none matches, [`NodeError::Ambiguous`] if several
	/// do, and the errors of [`Library::members`].
	pub fn find_member(&self, space: SpaceId, reference: &str) -> Result<EntityId, NodeError> {
		let found = self.find_members(space, reference)?;
		if let [member] = found.as_slice() {
			return Ok(*member);
		}
		// One who came back has a record of the past too: the one with a role is meant.
		let roles = {
			let open = self.space(space)?;
			open.stored.log().member_roles(&open.keys)?
		};
		let current: Vec<EntityId> = found
			.iter()
			.copied()
			.filter(|member| {
				roles
					.get(member)
					.is_some_and(|role| *role != MemberRole::None)
			})
			.collect();
		match current.as_slice() {
			[member] => Ok(*member),
			_ => Err(NodeError::Ambiguous {
				reference: reference.to_owned(),
				candidates: found,
			}),
		}
	}

	/// Every member of a shared space that `reference` names, as [`Library::find_member`]
	/// finds them, those who left or were removed included: one who came back has a record
	/// of the past too.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoMember`] if none matches, and the errors of
	/// [`Library::members`].
	pub fn find_members(
		&self,
		space: SpaceId,
		reference: &str,
	) -> Result<Vec<EntityId>, NodeError> {
		let lowered = reference.to_lowercase();
		let members = self.members(space)?;
		let by_name: Vec<EntityId> = members
			.iter()
			.filter(|member| {
				member
					.name
					.as_ref()
					.is_some_and(|name| name.to_lowercase() == lowered)
			})
			.map(|member| member.record)
			.collect();
		let found = if by_name.is_empty() && lowered.len() >= 4 {
			members
				.iter()
				.filter(|member| member.record.to_string().starts_with(&lowered))
				.map(|member| member.record)
				.collect()
		} else {
			by_name
		};
		if found.is_empty() {
			return Err(NodeError::NoMember(reference.to_owned()));
		}
		Ok(found)
	}

	/// Gives a member of a shared space a role, as an admin of it: viewer, editor or admin;
	/// the role none removes them, as [`Library::remove_member`] does.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::AdminsOnly`]
	/// if this replica's member is no admin, [`NodeError::NoMember`] if the space has no such
	/// member, [`NodeError::LastAdmin`] if the space would have no admin left, and an error if
	/// the change cannot be saved.
	pub fn give_role(
		&mut self,
		space: SpaceId,
		member: EntityId,
		role: MemberRole,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.check_shared(space)?;
		if role == MemberRole::None {
			return self.remove_member(space, member, wall_millis, rng);
		}
		self.check_admin(space, "give roles")?;
		let roles = {
			let open = self.space(space)?;
			open.stored.log().member_roles(&open.keys)?
		};
		// Making an admin, or lowering one, needs an admin for good.
		if role == MemberRole::Admin || roles.get(&member) == Some(&MemberRole::Admin) {
			self.check_settled_admin(space, "make or lower admins")?;
		}
		match roles.get(&member) {
			None | Some(MemberRole::None) => return Err(NodeError::NoMember(member.to_string())),
			Some(now) if *now == role => return Ok(()),
			Some(_) => {}
		}
		// A group's record in the space is never an admin's.
		if role == MemberRole::Admin {
			let open = self.space(space)?;
			let group = open
				.stored
				.log()
				.groups(&open.keys)?
				.iter()
				.any(|found| found.record == member);
			if group {
				return Err(NodeError::GroupAsAdmin);
			}
		}
		let admins = roles
			.iter()
			.filter(|(other, given)| **given == MemberRole::Admin && **other != member)
			.count();
		if role != MemberRole::Admin && admins == 0 {
			return Err(NodeError::LastAdmin);
		}
		self.set_role(space, member, role, wall_millis, rng)
	}

	/// Removes a member of a shared space, as an admin of it, or, for this replica's own
	/// member, leaves, as [`Library::leave`] does: gives them the role none, which ends every
	/// replica of theirs there, and gives the space a new key, sealed to the replicas that
	/// remain, so that they read nothing added after. What they did without knowing of it is
	/// set aside on every device, for an admin to review.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::AdminsOnly`]
	/// if this replica's member is no admin, [`NodeError::NoMember`] if the space has no such
	/// member, or they left already, [`NodeError::LastAdmin`] if the space would have no admin
	/// left, and an error if the changes cannot be saved.
	pub fn remove_member(
		&mut self,
		space: SpaceId,
		member: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		if self.check_remove(space, member)? {
			return self.leave(space, wall_millis, rng).map(|_| ());
		}
		// A member who is in the space through a group too is kept out of it, in the same
		// change.
		let through = self.keys_through_groups(space, member)?;
		let mut operations = vec![self.role_operation(space, member, MemberRole::None, rng)?];
		operations.extend(self.kept_out_operations(space, &through, true, rng)?);
		self.commit(space, operations, wall_millis, rng)?;
		// The removal is saved: if the new key fails, the next sync makes one, as this
		// replica's newest key is not safe; and a new secrets key, if the secrets are unlocked
		// here, or else the first replica that unlocks them makes one.
		self.after_departure(space, wall_millis, rng);
		Ok(())
	}

	/// Leaves a shared space: gives this replica's own member the role none, which ends every
	/// replica of the user there, and keeps the user out of it through groups, where they are
	/// in it through one, staying in the group. They keep what they hold, read only, and get
	/// nothing added after; the members who remain give the space a new key as they learn of
	/// it. Returns whether it closed the space, as the last member there.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::Removed`] if
	/// the member left or was removed already, [`NodeError::NotListed`] if this replica is no
	/// member's there, [`NodeError::AiAgentMayNot`] for an AI agent's replica,
	/// [`NodeError::LastAdmin`] if the member is the only admin while others remain, who
	/// makes another an admin first, and an error if the change cannot be saved.
	pub fn leave(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let closes = self.check_leave(space)?;
		// The role none and keeping out go in one change, as this replica changes nothing
		// there after either.
		let key = self.replica.signing.verifying_key();
		let mut operations = Vec::new();
		let mut through = Vec::new();
		if let Some(own) = self.user_member(space)? {
			operations.push(self.role_operation(space, own, MemberRole::None, rng)?);
			through = self.keys_through_groups(space, own)?;
		}
		// A member through a group keeps themselves out, by this replica's key too.
		if !through.contains(&key) && self.is_listed_through_groups(space, &key)? {
			through.push(key);
		}
		operations.extend(self.kept_out_operations(space, &through, true, rng)?);
		self.commit(space, operations, wall_millis, rng)?;
		Ok(closes)
	}

	/// Whether groups in a space list a key, at any level.
	fn is_listed_through_groups(
		&self,
		space: SpaceId,
		key: &nibkeep_crypto::VerifyingKey,
	) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.listed_through_groups(&open.keys)?
			.contains(key))
	}

	/// Checks that this replica's member may leave a shared space now, as before saying what
	/// leaving would do, and returns whether leaving would close it, as its last member.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::leave`] but those of saving.
	pub fn check_leave(&self, space: SpaceId) -> Result<bool, NodeError> {
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("leave spaces")?;
		let Some(own) = self.user_member(space)? else {
			// A member through a group alone leaves the space and stays in the group.
			let key = self.replica.signing.verifying_key();
			if self.is_listed_through_groups(space, &key)? {
				return Ok(false);
			}
			return Err(NodeError::NotListed);
		};
		let roles = {
			let open = self.space(space)?;
			open.stored.log().member_roles(&open.keys)?
		};
		let others: Vec<MemberRole> = roles
			.iter()
			.filter(|(member, role)| **member != own && **role != MemberRole::None)
			.map(|(_, role)| *role)
			.collect();
		if roles.get(&own) == Some(&MemberRole::Admin)
			&& !others.is_empty()
			&& !others.contains(&MemberRole::Admin)
		{
			return Err(NodeError::LastAdmin);
		}
		Ok(others.is_empty())
	}

	/// Checks that this replica's member may remove `member` from a shared space now, as
	/// before saying what removing would do; returns whether `member` is this replica's own,
	/// whose removal is leaving, as [`Library::check_leave`] checks.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::remove_member`] but those of saving.
	pub fn check_remove(&self, space: SpaceId, member: EntityId) -> Result<bool, NodeError> {
		self.check_shared(space)?;
		if self.member_in(space)? == Some(member) {
			self.check_leave(space)?;
			return Ok(true);
		}
		self.check_admin(space, "remove members")?;
		let roles = {
			let open = self.space(space)?;
			open.stored.log().member_roles(&open.keys)?
		};
		if roles.get(&member) == Some(&MemberRole::Admin) {
			self.check_settled_admin(space, "remove admins")?;
		}
		// The admin who removes stays: the space keeps an admin.
		match roles.get(&member) {
			None | Some(MemberRole::None) => Err(NodeError::NoMember(member.to_string())),
			Some(_) => Ok(false),
		}
	}

	/// Commits a member's role, replacing the statements of it now, in a change that
	/// governs the space.
	pub(crate) fn set_role(
		&mut self,
		space: SpaceId,
		member: EntityId,
		role: MemberRole,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let operation = self.role_operation(space, member, role, rng)?;
		self.commit(space, vec![operation], wall_millis, rng)
	}

	/// The operation that states a member's role, replacing the statements of it now.
	pub(crate) fn role_operation(
		&self,
		space: SpaceId,
		member: EntityId,
		role: MemberRole,
		rng: &mut impl CryptoRng,
	) -> Result<Operation, NodeError> {
		let current: Vec<StatementId> = self
			.space(space)?
			.stored
			.log()
			.state()
			.current(member, &fields::ROLE)?
			.iter()
			.map(|statement| statement.id)
			.collect();
		Ok(Operation::replace(
			Statement::new(
				StatementId::random(rng),
				member,
				fields::ROLE,
				Value::Text(role.as_text().to_owned()),
			),
			current,
		))
	}

	/// Whether inviting someone to a space should suggest making them a second admin: a
	/// shared space with a single admin does, and the personal space never.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn suggests_second_admin(&self, space: SpaceId) -> Result<bool, NodeError> {
		if space == self.info.personal || !self.has_members(space)? {
			return Ok(false);
		}
		let open = self.space(space)?;
		let roles = open.stored.log().member_roles(&open.keys)?;
		Ok(roles
			.values()
			.filter(|role| **role == MemberRole::Admin)
			.count()
			== 1)
	}

	/// What this replica asks with when it joins a space by an invitation: its user, by ID
	/// and name, its keys, and the names of its device and app.
	pub(crate) fn invitation_request(&self) -> Result<InvitationRequest, NodeError> {
		let (device, app) = self.own_names()?;
		let user_name = self.own_name(self.info.user)?.unwrap_or_default();
		for name in [&user_name, &device, &app] {
			if name.len() > nibkeep_governance::MAX_NAME_LEN {
				return Err(NodeError::LongName(nibkeep_governance::MAX_NAME_LEN));
			}
			if name.trim().is_empty() {
				return Err(NodeError::EmptyName);
			}
		}
		Ok(InvitationRequest {
			user: self.info.user,
			user_name,
			signing: self.replica.signing.verifying_key(),
			encryption: self.replica.decryption.encryption_key(),
			device,
			app,
		})
	}

	/// Whether a space allows AI agents now: the personal space does, and a shared space where
	/// its admins said so last ([technical design](../../../../docs/Technical-design.md#13-sharing)).
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn ai_agents_allowed(&self, space: SpaceId) -> Result<bool, NodeError> {
		if space == self.info.personal || !self.has_members(space)? {
			return Ok(true);
		}
		let open = self.space(space)?;
		Ok(open.stored.log().allows_ai_agents(&open.keys)?)
	}

	/// Says, as an admin of a shared space, whether it allows AI agents: an AI agent's replica
	/// is listed there only while it does, and what it writes counts only then. Stopping them
	/// ends the AI agents' replicas listed there, as a removal does, and gives the space a new
	/// key that they do not get; allowing them again brings those back.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::AdminsOnly`]
	/// if this replica's member is no admin there, and an error if the change cannot be
	/// saved.
	pub fn allow_ai_agents(
		&mut self,
		space: SpaceId,
		allow: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_admin(space, "allow AI agents")?;
		let allowed = self.ai_agents_allowed(space)?;
		let open = self.space(space)?;
		let entity = open
			.own_entity()?
			.ok_or_else(|| NodeError::Damaged(format!("the space {space} has no record")))?;
		let current: Vec<StatementId> = open
			.stored
			.log()
			.state()
			.current(entity, &fields::AI_AGENTS)?
			.iter()
			.map(|statement| statement.id)
			.collect();
		let operation = Operation::replace(
			Statement::new(
				StatementId::random(rng),
				entity,
				fields::AI_AGENTS,
				Value::Boolean(allow),
			),
			current,
		);
		self.commit(space, vec![operation], wall_millis, rng)?;
		if allow || !allowed {
			return Ok(());
		}
		// What is written next is sealed with a key that the AI agents it ended do not hold;
		// if making it fails, the next sync makes one.
		let roster = self.roster(space)?;
		let open = self.space(space)?;
		let mut ended = false;
		for replica in roster.replicas() {
			for key in roster.keys_of(replica) {
				ended |= open.stored.log().is_ai_agent(key, &open.keys)?
					&& open.stored.log().is_listed(key, &open.keys)?;
			}
		}
		if ended {
			let _ = self.rotate(space, &[], &roster, wall_millis, rng);
		}
		Ok(())
	}

	/// Checks, before an AI agent's replica is paired, that each of `spaces` can list it: a
	/// shared space that this replica is listed in, which allows AI agents, or where this
	/// replica's member is an admin, who allows them once the AI agent has joined.
	pub(crate) fn check_ai_spaces(&self, spaces: &[SpaceId]) -> Result<(), NodeError> {
		for &space in spaces {
			self.check_shared(space)?;
			self.check_not_revoked(space)?;
			self.member_in(space)?.ok_or(NodeError::NotListed)?;
			if self.ai_agents_allowed(space)? {
				continue;
			}
			match self.check_admin(space, "allow AI agents") {
				Ok(_) => {}
				Err(NodeError::AdminsOnly(_)) => {
					let name = self.space_name(space)?.unwrap_or_else(|| space.to_string());
					return Err(NodeError::AiAgentsNotAllowed(name));
				}
				Err(error) => return Err(error),
			}
		}
		Ok(())
	}

	/// The listings that this replica's key signs as it pairs: in the personal space, for the
	/// record and the user that a grant names, as an AI agent's if the grant says so; then, as
	/// an AI agent's, in the spaces that the grant chose for it, in their order.
	pub(crate) fn pairing_listings(&self, grant: &nibkeep_governance::Grant) -> Vec<String> {
		let encryption = self.replica.decryption.encryption_key();
		let own = Listing::sign(
			&self.replica.signing,
			&encryption,
			grant.space,
			(grant.user, grant.ai_agent),
		);
		std::iter::once(own.to_text())
			.chain(grant.chosen.iter().map(|&(space, member)| {
				Listing::sign(&self.replica.signing, &encryption, space, (member, true)).to_text()
			}))
			.collect()
	}

	/// What listing an AI agent's replica that pairing added in the spaces chosen for it
	/// takes, once the listings that its key signed check, before anything is committed: the
	/// personal space's entries that give it each space's first key, and the operations that
	/// list it in each space, owned by the user's member, marked as an AI agent's.
	pub(crate) fn ai_agent_listings(
		&self,
		(request, endpoint, grant, name): (
			&nibkeep_governance::JoinRequest,
			&[u8; 32],
			&nibkeep_governance::Grant,
			&str,
		),
		listings: &[String],
		rng: &mut impl CryptoRng,
	) -> Result<AiAgentListings, NodeError> {
		let chosen: Vec<SpaceId> = grant.chosen.iter().map(|(space, _)| *space).collect();
		self.check_ai_spaces(&chosen)?;
		let encryption = encryption_hash(&request.encryption.to_bytes());
		let mut prepared = AiAgentListings::default();
		for (&(space, member), text) in grant.chosen.iter().zip(listings) {
			let listing = Listing::from_text(text)
				.filter(|listing| {
					listing.space == space
						&& listing.owner == member
						&& listing.ai_agent
						&& listing.key == request.signing
						&& listing.encryption == encryption
						&& listing.verifies()
				})
				.ok_or_else(|| {
					NodeError::Damaged("the AI agent's listing is not the one asked for".to_owned())
				})?;
			let open = self.space(space)?;
			let first = open
				.keys
				.space_keys()
				.find(|(epoch, _)| *epoch == 0)
				.map(|(_, key)| key.clone())
				.ok_or(NodeError::NoSpace(space))?;
			let root = open.stored.log().root()?.ok_or(NodeError::NoSpace(space))?;
			prepared.entries.push(nibkeep_governance::give_space_entry(
				grant.replica,
				&SpaceEntry::seal(
					&request.encryption,
					(space, member, *root.hash()),
					grant.replica,
					(&first, key_hash(&first)),
					rng,
				),
				rng,
			));
			let listed = nibkeep_governance::list_replica(
				&SpaceReplica {
					record: listing.record,
					device_record: nibkeep_governance::device_record(space, &listing.key),
					member,
					signing: &listing.key,
					encryption: &request.encryption,
					listing: text,
					endpoint: Some(endpoint),
					device: &request.device,
					app: name,
					ai_agent: true,
				},
				rng,
			);
			let mut operations = listed.governing;
			operations.extend(listed.operations);
			prepared.spaces.push((space, operations));
		}
		Ok(prepared)
	}

	/// Lists an AI agent's replica in the spaces chosen for it, as
	/// [`Library::ai_agent_listings`] prepared: allows AI agents first where a space does not
	/// yet, as an admin there, then lists it, and gives it the space's keys.
	pub(crate) fn list_ai_agent(
		&mut self,
		spaces: Vec<(SpaceId, Vec<Operation>)>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		for (space, operations) in spaces {
			if !self.ai_agents_allowed(space)? {
				self.allow_ai_agents(space, true, wall_millis, rng)?;
			}
			self.commit(space, operations, wall_millis, rng)?;
			self.give_keys(space, wall_millis, rng)?;
		}
		Ok(())
	}

	/// Checks that a shared space holds a person to name a member by: an entity there, not
	/// deleted.
	pub(crate) fn check_person(&self, space: SpaceId, person: EntityId) -> Result<(), NodeError> {
		let state = self.space(space)?.stored.log().state();
		if !state.types(person)?.contains(&nibkeep_governance::PERSON)
			|| state.is_deleted(person)?
		{
			return Err(NodeError::NoEntity(person.to_string()));
		}
		Ok(())
	}

	/// The person in a shared space that `reference` names, as [`Library::find_entity`]
	/// finds entities, but of the type `std.person` alone: so that neither a member's
	/// record, which has the user's name, nor another entity is taken for a person.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if no person matches, [`NodeError::Ambiguous`] if
	/// several do, and an error if the space cannot be read.
	pub fn find_person(&self, space: SpaceId, reference: &str) -> Result<EntityId, NodeError> {
		let is_person = |entity: EntityId| -> Result<bool, NodeError> {
			let state = self.space(space)?.stored.log().state();
			Ok(state.types(entity)?.contains(&nibkeep_governance::PERSON)
				&& !state.is_deleted(entity)?)
		};
		let candidates = match self.find_entity(space, reference) {
			Ok(entity) => vec![entity],
			Err(NodeError::Ambiguous { candidates, .. }) => candidates,
			Err(error) => return Err(error),
		};
		let mut persons = Vec::new();
		for candidate in candidates {
			if is_person(candidate)? {
				persons.push(candidate);
			}
		}
		match persons.as_slice() {
			[] => Err(NodeError::NoEntity(reference.to_owned())),
			[person] => Ok(*person),
			_ => Err(NodeError::Ambiguous {
				reference: reference.to_owned(),
				candidates: persons,
			}),
		}
	}

	/// Checks that a space is a shared one, with members.
	pub(super) fn check_shared(&self, space: SpaceId) -> Result<(), NodeError> {
		if space == self.info.personal || !self.has_members(space)? {
			return Err(NodeError::NeverShared);
		}
		Ok(())
	}

	/// Checks that this replica's member is an admin of a shared space now, as before
	/// showing a code that could not be used, and returns the member.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::Revoked`] if
	/// this replica is revoked there, [`NodeError::AiAgentMayNot`] for an AI agent's replica,
	/// [`NodeError::AdminsOnly`] if its member is no admin, and an error if the space cannot
	/// be read.
	pub fn check_admin(&self, space: SpaceId, what: &'static str) -> Result<EntityId, NodeError> {
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		if self.is_ai_agent()? {
			return Err(NodeError::AiAgentMayNot(what));
		}
		let member = self.member_in(space)?.ok_or(NodeError::AdminsOnly(what))?;
		let open = self.space(space)?;
		let roles = open.stored.log().member_roles(&open.keys)?;
		// A claim that took effect makes its claimant an admin too.
		if roles.get(&member) != Some(&MemberRole::Admin)
			&& self.claim_now(space, member)?.is_none()
		{
			return Err(NodeError::AdminsOnly(what));
		}
		Ok(member)
	}

	/// Checks, as [`Library::check_admin`] does, that this replica's member is an admin of a
	/// shared space now, other than one whose claim to the role took effect less than 30 days
	/// ago, who can only undo: no purge, acceptance, rejection of claims, new admin, or
	/// lowering an admin.
	///
	/// # Errors
	///
	/// The errors of [`Library::check_admin`].
	pub(crate) fn check_settled_admin(
		&self,
		space: SpaceId,
		what: &'static str,
	) -> Result<EntityId, NodeError> {
		let member = self.check_admin(space, what)?;
		if self.claim_now(space, member)? == Some(super::claims::ClaimNow::Undoing) {
			let open = self.space(space)?;
			let roles = open.stored.log().member_roles(&open.keys)?;
			if roles.get(&member) != Some(&MemberRole::Admin) {
				return Err(NodeError::AdminsOnly(what));
			}
		}
		Ok(member)
	}

	/// The member that a key's counting listing in a shared space names, if one lists it.
	fn listed_member(
		&self,
		space: SpaceId,
		key: &VerifyingKey,
	) -> Result<Option<EntityId>, NodeError> {
		Ok(self.users_and_owners(space)?.1.get(key.as_bytes()).copied())
	}

	/// What the counting governing changes of a shared space say of who is who, as
	/// [`users_and_owners`] says.
	pub(crate) fn users_and_owners(&self, space: SpaceId) -> Result<UsersAndOwners, NodeError> {
		let open = self.space(space)?;
		users_and_owners(&open.stored, &open.keys)
	}

	/// Links a member of a shared space to a person in it, or, without one, unlinks them: as
	/// the member, for themselves, or as an admin, for another member. The link holds once an
	/// admin's statement and the member's own agree; an admin's own statement counts for both
	/// while they are an admin.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::NotListed`]
	/// if this replica is no member's there, [`NodeError::AdminsOnly`] for another member if
	/// this replica's member is no admin, [`NodeError::NoMember`] if the space has no such
	/// member, [`NodeError::NoEntity`] if it holds no such person, and an error if the change
	/// cannot be saved.
	pub fn link_member(
		&mut self,
		space: SpaceId,
		member: EntityId,
		person: Option<EntityId>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		let own = self.member_in(space)?.ok_or(NodeError::NotListed)?;
		if member != own {
			self.check_admin(space, "link another member to a person")?;
		}
		let open = self.space(space)?;
		let log = open.stored.log();
		let roles = log.member_roles(&open.keys)?;
		if roles
			.get(&member)
			.is_none_or(|role| *role == MemberRole::None)
		{
			return Err(NodeError::NoMember(member.to_string()));
		}
		if let Some(person) = person {
			self.check_person(space, person)?;
		}
		let open = self.space(space)?;
		// Each side replaces only its own statements, so that the other's stays to agree with.
		let (_, owners) = users_and_owners(&open.stored, &open.keys)?;
		let by_member = member == own;
		let ours: Vec<StatementId> =
			link_statements((&open.stored, open.stored.log().state()), &owners, member)?
				.into_iter()
				.filter(|stated| stated.by_member == by_member)
				.map(|stated| stated.id)
				.collect();
		let operation = match person {
			Some(person) => Operation::replace(
				Statement::new(
					StatementId::random(rng),
					member,
					nibkeep_governance::LINKED_PERSON,
					Value::Reference(person),
				),
				ours,
			),
			None if ours.is_empty() => return Ok(()),
			None => Operation::retract(member, ours),
		};
		self.commit(space, vec![operation], wall_millis, rng)
	}

	/// The replicas of other members of the shared spaces that this replica is listed in,
	/// which it syncs those spaces with: listed there, not revoked, and with an endpoint on
	/// their records, each once, with the first such space.
	pub(crate) fn other_members_replicas(&self) -> Vec<super::sync::Replica> {
		let own = self.replica.signing.verifying_key();
		let mut found: BTreeMap<[u8; 32], super::sync::Replica> = BTreeMap::new();
		for space in self.spaces() {
			// A space that cannot be read keeps nobody from syncing the others.
			if space == self.info.personal
				|| !self.has_members(space).unwrap_or(false)
				|| self.check_not_revoked(space).is_err()
			{
				continue;
			}
			let Ok(members) = self.members(space) else {
				continue;
			};
			let member = self.member_in(space).ok().flatten();
			// The members of the groups in it too, at every level, whose logs it holds.
			let mut others = members;
			for group in self.inner_groups(space).unwrap_or_default() {
				if self.spaces.contains_key(&group)
					&& let Ok(members) = self.members(group)
				{
					others.extend(members);
				}
			}
			for other in others {
				if Some(other.record) == member {
					continue;
				}
				for replica in other.replicas {
					if replica.revoked
						|| replica.signing == own
						|| replica.endpoint.is_none()
						|| found.contains_key(replica.signing.as_bytes())
					{
						continue;
					}
					found.insert(
						*replica.signing.as_bytes(),
						super::sync::Replica::of_member((space, other.name.as_deref()), &replica),
					);
				}
			}
		}
		found.into_values().collect()
	}
}

/// What listing an AI agent's replica in the spaces chosen for it takes: the personal
/// space's entries, and the operations of each space.
#[derive(Default)]
pub(crate) struct AiAgentListings {
	pub(crate) entries: Vec<Operation>,
	pub(crate) spaces: Vec<(SpaceId, Vec<Operation>)>,
}

/// Each member record's user ID, and each listed key's owner.
type UsersAndOwners = (BTreeMap<EntityId, EntityId>, BTreeMap<[u8; 32], EntityId>);

/// What the counting governing changes of a shared space say of who is who: each member
/// record's user ID, the first stated, and each listed key's owner, by the listings that its
/// key signed.
pub(crate) fn users_and_owners(
	stored: &nibkeep_store::StoredLog,
	keys: &nibkeep_log::SpaceKeys,
) -> Result<UsersAndOwners, NodeError> {
	let mut users = BTreeMap::new();
	let mut owners = BTreeMap::new();
	for governing in stored.log().governing_statements(keys)? {
		let entity = governing.statement.entity;
		if let Some(text) = governing.counting_text(&fields::USER_ID) {
			if let Ok(user) = text.parse::<EntityId>() {
				users.entry(entity).or_insert(user);
			}
		} else if let Some(listing) = governing
			.counting_text(&fields::LISTING)
			.and_then(Listing::from_text)
			&& listing.record == entity
			&& listing.verifies()
		{
			owners.insert(*listing.key.as_bytes(), listing.owner);
		}
	}
	Ok((users, owners))
}

/// A current statement of a member's link to a person: the person, when it was recorded,
/// and whether one of the member's own replicas signed it, or else an admin's, as only those
/// count on a member's record.
struct LinkStated {
	id: StatementId,
	person: EntityId,
	at: nibkeep_time::Timestamp,
	by_member: bool,
}

/// The statements of a member's link to a person that `state` holds now, with who signed
/// each, as the log has their changes.
fn link_statements<S: Statements>(
	(stored, state): (&nibkeep_store::StoredLog, &nibkeep_model::State<S>),
	owners: &BTreeMap<[u8; 32], EntityId>,
	member: EntityId,
) -> Result<Vec<LinkStated>, NodeError>
where
	NodeError: From<S::Error>,
{
	let log = stored.log();
	let mut stated = Vec::new();
	for statement in state.current(member, &nibkeep_governance::LINKED_PERSON)? {
		let Some(person) = statement.value.as_reference() else {
			continue;
		};
		let Some(recorded) = state.storage().get(statement.id)? else {
			continue;
		};
		let Some(change) = recorded.change else {
			continue;
		};
		let Some(change) = log.change(nibkeep_log::ChangeId::from_hash(change))? else {
			continue;
		};
		stated.push(LinkStated {
			id: statement.id,
			person,
			at: recorded.at,
			by_member: owners.get(change.author().as_bytes()) == Some(&member),
		});
	}
	Ok(stated)
}

/// A member's link to a person in a shared space, as `state` has it, with each listed key's
/// owner and each member's role now.
pub(crate) fn person_link<S: Statements>(
	(stored, state): (&nibkeep_store::StoredLog, &nibkeep_model::State<S>),
	(owners, roles): (
		&BTreeMap<[u8; 32], EntityId>,
		&BTreeMap<EntityId, MemberRole>,
	),
	member: EntityId,
) -> Result<PersonLink, NodeError>
where
	NodeError: From<S::Error>,
{
	let stated = link_statements((stored, state), owners, member)?;
	let latest = |by_member: bool| {
		stated
			.iter()
			.filter(|stated| stated.by_member == by_member)
			.max_by_key(|stated| (stated.at, stated.id))
			.map(|stated| stated.person)
	};
	let (by_member, by_admin) = (latest(true), latest(false));
	let is_admin = roles.get(&member) == Some(&MemberRole::Admin);
	let agreed = by_member.filter(|person| is_admin || by_admin == Some(*person));
	let deleted = match agreed {
		Some(person) => state.is_deleted(person)?,
		None => false,
	};
	Ok(PersonLink {
		holds: agreed.filter(|_| !deleted),
		by_admin,
		by_member,
		deleted,
	})
}

/// What an admin's replica asks and offers as it invites someone to a space.
pub(crate) struct Inviter<'a> {
	library: &'a mut Library,
	space: SpaceId,
	role: MemberRole,
	person: Option<EntityId>,
	confirm: &'a mut dyn FnMut(&Invitee) -> bool,
	now: &'a dyn Fn() -> u64,
	/// Who asked, from where, and what was offered.
	asked: Option<(InvitationRequest, [u8; 32], Offer)>,
	/// The joiner's listing, once it checked.
	listing: Option<(Listing, String)>,
	/// Whether the code matched: the joiner's request came with the tag of the code's key.
	matched: bool,
}

impl<'a> Inviter<'a> {
	pub(crate) fn new(
		library: &'a mut Library,
		invitation: &crate::Invitation,
		confirm: &'a mut dyn FnMut(&Invitee) -> bool,
		now: &'a dyn Fn() -> u64,
	) -> Self {
		Self {
			library,
			space: invitation.space,
			role: invitation.role,
			person: invitation.person,
			confirm,
			now,
			asked: None,
			listing: None,
			matched: false,
		}
	}

	/// Whether the code matched: the person was asked about the joiner, or would have been,
	/// had reading the members not failed. An invitation that failed before then leaves the
	/// code for another try.
	pub(crate) const fn matched(&self) -> bool {
		self.matched
	}
}

impl Inviting for Inviter<'_> {
	type Error = NodeError;

	fn offer<R: CryptoRng>(
		&mut self,
		request: &InvitationRequest,
		endpoint: &[u8; 32],
		rng: &mut R,
	) -> Result<Option<Offer>, NodeError> {
		// An invitation asks for an offer only once the code matched.
		self.matched = true;
		let members = self.library.members(self.space)?;
		let of_user = |member: &&SpaceMember| member.user == Some(request.user);
		let invitee = Invitee {
			user: request.user,
			user_name: request.user_name.clone(),
			device: request.device.clone(),
			app: request.app.clone(),
			same_user: members
				.iter()
				.filter(of_user)
				.filter(|member| member.role != MemberRole::None)
				.cloned()
				.collect(),
			was_member: members
				.iter()
				.filter(of_user)
				.any(|member| member.role == MemberRole::None),
		};
		if !(self.confirm)(&invitee) {
			return Ok(None);
		}
		// A device invited before, whose first sync failed, is still listed, for the member
		// that invitation made: it comes back as that member. One whose member left or was
		// removed comes back as a new member, as anyone who comes back does.
		let listed = self.library.listed_member(self.space, &request.signing)?;
		let of_listed =
			listed.and_then(|listed| members.iter().find(|member| member.record == listed));
		let member = match of_listed {
			Some(listed) if listed.role == MemberRole::None => EntityId::random(rng),
			Some(listed) if listed.user == Some(request.user) => listed.record,
			Some(_) => {
				return Err(NodeError::Damaged(
					"the device that asks is listed in the space for another user".to_owned(),
				));
			}
			None if listed.is_some() => {
				return Err(NodeError::Damaged(
					"the device that asks is listed in the space for no member".to_owned(),
				));
			}
			None => EntityId::random(rng),
		};
		let name = self
			.library
			.space_name(self.space)?
			.unwrap_or_else(|| self.space.to_string());
		let person = match self.person {
			Some(person) => {
				let name = self
					.library
					.name(self.space, person)?
					.unwrap_or_else(|| person.to_string());
				Some((person, truncated(&name, nibkeep_governance::MAX_NAME_LEN)))
			}
			None => None,
		};
		let offer = Offer {
			space: self.space,
			space_name: truncated(&name, nibkeep_governance::MAX_NAME_LEN),
			member,
			role: self.role.as_text().to_owned(),
			person,
		};
		self.asked = Some((request.clone(), *endpoint, offer.clone()));
		Ok(Some(offer))
	}

	fn grant<R: CryptoRng>(
		&mut self,
		text: &str,
		rng: &mut R,
	) -> Result<InvitationGrant, NodeError> {
		let (request, _, offer) = self
			.asked
			.as_ref()
			.ok_or_else(|| NodeError::Damaged("a listing came before the offer".to_owned()))?;
		let space = self.space;
		let listing = Listing::from_text(text)
			.filter(|listing| {
				listing.space == space
					&& listing.owner == offer.member
					&& !listing.ai_agent
					&& listing.key == request.signing
					&& listing.encryption == encryption_hash(&request.encryption.to_bytes())
					&& listing.verifies()
			})
			.ok_or_else(|| {
				NodeError::Damaged("the joiner's listing is not the one offered".to_owned())
			})?;
		let open = self.library.space(space)?;
		let log = open.stored.log();
		// A key that the space revoked is never listed anew.
		if log.is_revoked(&listing.key, &open.keys)? {
			return Err(NodeError::Damaged(
				"the joiner's key is revoked in the space".to_owned(),
			));
		}
		let first = open
			.keys
			.space_keys()
			.find(|(epoch, _)| *epoch == 0)
			.map(|(_, key)| key.clone())
			.ok_or(NodeError::NoSpace(space))?;
		let root = log.root()?.ok_or(NodeError::NoSpace(space))?;
		let entry = SpaceEntry::seal(
			&request.encryption,
			(space, offer.member, *root.hash()),
			listing.record,
			(&first, key_hash(&first)),
			rng,
		);
		self.listing = Some((listing, text.to_owned()));
		Ok(InvitationGrant {
			entry,
			admin: self.library.replica.signing.verifying_key(),
		})
	}

	fn record<R: CryptoRng>(&mut self, rng: &mut R) -> Result<(), NodeError> {
		let (Some((request, endpoint, offer)), Some((listing, text))) =
			(&self.asked, &self.listing)
		else {
			return Err(NodeError::Damaged(
				"the joiner joined before it was granted the space".to_owned(),
			));
		};
		// A device invited before is listed already, for this member: it gets the role anew.
		if self.library.listed_member(self.space, &listing.key)? == Some(offer.member) {
			let wall_millis = (self.now)();
			let roles = {
				let open = self.library.space(self.space)?;
				open.stored.log().member_roles(&open.keys)?
			};
			if roles.get(&offer.member) != Some(&self.role) {
				self.library
					.set_role(self.space, offer.member, self.role, wall_millis, rng)?;
			}
			let _ = self.library.give_keys(self.space, wall_millis, rng);
			// The joiner holds the space now: a person deleted meanwhile keeps nobody out.
			if let Some(person) = self.person {
				let _ = self.library.link_member(
					self.space,
					offer.member,
					Some(person),
					wall_millis,
					rng,
				);
			}
			return Ok(());
		}
		let added = add_member(
			&NewMember {
				user: request.user,
				user_name: &request.user_name,
				member: offer.member,
				role: self.role.as_text(),
				replica: SpaceReplica {
					record: listing.record,
					device_record: nibkeep_governance::device_record(self.space, &listing.key),
					member: offer.member,
					signing: &listing.key,
					encryption: &request.encryption,
					listing: text,
					endpoint: Some(endpoint),
					device: &request.device,
					app: &request.app,
					ai_agent: false,
				},
			},
			rng,
		);
		let mut operations = added.governing;
		operations.extend(added.operations);
		// The admin's side of the member's link to the person named, which the member's own
		// statement completes.
		if let Some(person) = self.person {
			operations.push(Operation::assert(Statement::new(
				StatementId::random(rng),
				offer.member,
				nibkeep_governance::LINKED_PERSON,
				Value::Reference(person),
			)));
		}
		let wall_millis = (self.now)();
		self.library
			.commit(self.space, operations, wall_millis, rng)?;
		// The space's later keys reach the new replica through the records; if giving them
		// fails, the next sync gives them.
		let _ = self.library.give_keys(self.space, wall_millis, rng);
		Ok(())
	}
}

/// The longest start of `text` that is at most `max` bytes long, cut between characters.
fn truncated(text: &str, max: usize) -> String {
	let mut end = text.len().min(max);
	while !text.is_char_boundary(end) {
		end -= 1;
	}
	text[..end].to_owned()
}

/// What a joiner's replica signs and takes as it joins a space by an invitation.
pub(crate) struct Acceptor<'a> {
	library: &'a mut Library,
	/// The space it holds already, if the offer named one.
	pub(crate) held: Option<SpaceId>,
	/// The space it took.
	pub(crate) taken: Option<SpaceId>,
	/// The space it holds already, which its member left or was removed from, that it comes
	/// back to as a new member.
	pub(crate) back: Option<SpaceId>,
	/// Whether the code matched: the admin's offer came with the tag of the code's key.
	pub(crate) matched: bool,
}

impl<'a> Acceptor<'a> {
	pub(crate) const fn new(library: &'a mut Library) -> Self {
		Self {
			library,
			held: None,
			taken: None,
			back: None,
			matched: false,
		}
	}
}

impl Accepting for Acceptor<'_> {
	type Error = NodeError;

	fn listing(&mut self, offer: &Offer) -> Result<Option<String>, NodeError> {
		// An invitation asks for the listing only once the code matched.
		self.matched = true;
		if MemberRole::from_text(&offer.role).is_none_or(|role| role == MemberRole::None) {
			return Err(NodeError::Damaged(format!(
				"the invitation offers a role that is none: {}",
				offer.role
			)));
		}
		if self.library.spaces.contains_key(&offer.space) {
			// A replica whose member left or was removed comes back as a new member, keeping
			// what it holds.
			if !self.library.has_left(offer.space)? {
				self.held = Some(offer.space);
				return Ok(None);
			}
			// An admin's device that has not learned of the leaving yet offers the member who
			// left, which would leave this replica removed.
			if self.library.member_in(offer.space)? == Some(offer.member) {
				return Err(NodeError::InviterBehind);
			}
			self.back = Some(offer.space);
		}
		Ok(Some(
			Listing::sign(
				&self.library.replica.signing,
				&self.library.replica.decryption.encryption_key(),
				offer.space,
				(offer.member, false),
			)
			.to_text(),
		))
	}

	fn take<R: CryptoRng>(
		&mut self,
		offer: &Offer,
		grant: &InvitationGrant,
		rng: &mut R,
	) -> Result<(), NodeError> {
		let entry = &grant.entry;
		if entry.space != offer.space || entry.member != offer.member {
			return Err(NodeError::Damaged(
				"the grant is of another space than the offer".to_owned(),
			));
		}
		let record = replica_record(offer.space, &self.library.replica.signing.verifying_key());
		let key = entry
			.open(&self.library.replica.decryption, record)
			.filter(|key| key_hash(key) == entry.hash)
			.ok_or_else(|| NodeError::Damaged("the space's first key does not open".to_owned()))?;
		if self.back == Some(offer.space) {
			// It holds the space and its first key already.
			return Ok(());
		}
		self.library.add_space(offer.space, &key, rng)?;
		self.taken = Some(offer.space);
		if let Some(open) = self.library.spaces.get_mut(&offer.space) {
			open.stored
				.expect_roots([nibkeep_log::ChangeId::from_hash(entry.root)]);
		}
		Ok(())
	}
}
