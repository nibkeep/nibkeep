//! Members through groups in a space ([technical design: groups]): an admin makes one a direct
//! member, with a record that names one of their keys, on which their replicas list
//! themselves once the person confirms; an admin keeps one out, or lets them back; and a
//! member through a group leaves one space and stays in the group. Keeping out is said on the
//! records in the space derived from the person's keys.
//!
//! [technical design: groups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{
	Listing, MemberFor, SpaceReplica, list_replica, make_member_for, replica_record,
};
use nibkeep_model::{Operation, Statement, Value, fields};

use super::Library;
use super::entities::display_name;
use crate::{MemberRole, NodeError};

impl Library {
	/// The member through a group, at any level, of a shared space that `reference` names:
	/// by name, whatever the case, or by the start of their record's ID in the group, at
	/// least four characters long. Returns the group and their record there.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoMember`] if none matches, [`NodeError::Ambiguous`] if several
	/// do, and an error if a space cannot be read.
	pub fn find_group_member(
		&self,
		space: SpaceId,
		reference: &str,
	) -> Result<(SpaceId, EntityId), NodeError> {
		self.check_shared(space)?;
		let lowered = reference.to_lowercase();
		let mut named = BTreeSet::new();
		let mut by_id = BTreeSet::new();
		for group in self.inner_groups(space)? {
			if !self.spaces.contains_key(&group) {
				continue;
			}
			let open = self.space(group)?;
			let state = open.stored.log().state();
			for (record, role) in open.stored.log().member_roles(&open.keys)? {
				if role == MemberRole::None {
					continue;
				}
				let name = display_name(state, &open.types, &self.languages, record)?;
				if name.is_some_and(|name| name.to_lowercase() == lowered) {
					named.insert((group, record));
				} else if lowered.len() >= 4 && record.to_string().starts_with(&lowered) {
					by_id.insert((group, record));
				}
			}
		}
		let found = if named.is_empty() { by_id } else { named };
		match found.len() {
			0 => Err(NodeError::NoMember(reference.to_owned())),
			1 => Ok(found
				.into_iter()
				.next()
				.unwrap_or_else(|| unreachable!("one found"))),
			_ => Err(NodeError::Ambiguous {
				reference: reference.to_owned(),
				candidates: found.into_iter().map(|(_, record)| record).collect(),
			}),
		}
	}

	/// The keys of a member's replicas in a group, but revoked ones and AI agents', which no
	/// group gives a space.
	fn member_keys(
		&self,
		group: SpaceId,
		member: EntityId,
	) -> Result<Vec<VerifyingKey>, NodeError> {
		let (_, owners) = self.users_and_owners(group)?;
		let open = self.space(group)?;
		let log = open.stored.log();
		let mut keys = Vec::new();
		for (key, owner) in owners {
			let Some(key) = VerifyingKey::from_bytes(&key) else {
				continue;
			};
			if owner == member
				&& !log.is_revoked(&key, &open.keys)?
				&& !log.is_ai_agent(&key, &open.keys)?
			{
				keys.push(key);
			}
		}
		Ok(keys)
	}

	/// Makes a member through a group a direct member of a shared space, as an admin of it,
	/// such as to make them an admin: a member's record with `role`, their user ID and name,
	/// and one of their keys, on which their replicas list themselves once they confirm
	/// ([`Library::confirm_direct`]). Returns the record.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin of the space,
	/// [`NodeError::NoMember`] if no group in the space, at any level, holds such a member,
	/// with a replica, and an error if a change cannot be saved.
	pub fn make_direct(
		&mut self,
		space: SpaceId,
		(group, member): (SpaceId, EntityId),
		role: MemberRole,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.observe_wall(wall_millis);
		self.check_admin(space, "make direct members")?;
		let missing = || NodeError::NoMember(member.to_string());
		if role == MemberRole::None || !self.inner_groups(space)?.contains(&group) {
			return Err(missing());
		}
		let key = *self
			.member_keys(group, member)?
			.first()
			.ok_or_else(missing)?;
		let (users, _) = self.users_and_owners(group)?;
		let user = *users.get(&member).ok_or_else(missing)?;
		let name = {
			let open = self.space(group)?;
			display_name(
				open.stored.log().state(),
				&open.types,
				&self.languages,
				member,
			)?
		};
		let record = EntityId::random(rng);
		let (governing, operations) = make_member_for(
			&MemberFor {
				member: record,
				user,
				user_name: name.as_deref(),
				role: role.as_text(),
				key: &key,
			},
			rng,
		);
		self.commit(space, governing, wall_millis, rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok(record)
	}

	/// The keys of this replica's user: its own, and those of the user's other replicas in
	/// the personal space, but revoked ones.
	fn user_keys(&self) -> Result<BTreeSet<[u8; 32]>, NodeError> {
		let personal = self.info.personal;
		let roster = self.roster(personal)?;
		let open = self.space(personal)?;
		let log = open.stored.log();
		let mut keys = BTreeSet::from([*self.replica.signing.verifying_key().as_bytes()]);
		for replica in roster.replicas() {
			for key in roster.keys_of(replica) {
				if !log.is_revoked(key, &open.keys)? {
					keys.insert(*key.as_bytes());
				}
			}
		}
		Ok(keys)
	}

	/// The records that an admin of a shared space made there for this replica's user, whose
	/// keys are `keys`, and that have a role, each with whether a key of the user is listed on
	/// it already.
	fn made_for_user(
		&self,
		space: SpaceId,
		keys: &BTreeSet<[u8; 32]>,
	) -> Result<Vec<(EntityId, bool)>, NodeError> {
		let (_, owners) = self.users_and_owners(space)?;
		let open = self.space(space)?;
		let log = open.stored.log();
		let roles = log.member_roles(&open.keys)?;
		let mut made = Vec::new();
		for governing in log.governing_statements(&open.keys)? {
			let Some(text) = governing.counting_text(&fields::MADE_FOR) else {
				continue;
			};
			let record = governing.statement.entity;
			let for_user =
				VerifyingKey::from_text(text).is_some_and(|key| keys.contains(key.as_bytes()));
			let has_role = roles
				.get(&record)
				.is_some_and(|role| *role != MemberRole::None);
			if for_user && has_role && !made.iter().any(|(found, _)| *found == record) {
				let listed = owners
					.iter()
					.any(|(key, owner)| *owner == record && keys.contains(key));
				made.push((record, listed));
			}
		}
		Ok(made)
	}

	/// Confirms, as a member through a group, the record that an admin of a shared space made
	/// for this replica's user there: lists this replica on it, which makes the user a direct
	/// member, with its role; the user's other replicas list themselves as they learn of it.
	/// Returns the record.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space,
	/// [`NodeError::AiAgentMayNot`] for an AI agent's replica, [`NodeError::NoDirectRecord`]
	/// if no admin made a record there for the user, and an error if a change cannot be saved.
	pub fn confirm_direct(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("confirm direct membership")?;
		let made = self.made_for_user(space, &self.user_keys()?)?;
		let Some(&(record, _)) = made.first() else {
			return Err(NodeError::NoDirectRecord);
		};
		if self.member_in(space)? == Some(record) {
			return Ok(record);
		}
		// A listing the log refused is never made again.
		if !self.listed_itself_on(space, record)? {
			self.list_on(space, record, wall_millis, rng)?;
		}
		if self.member_in(space)? != Some(record) {
			return Err(NodeError::DirectRecordRefused);
		}
		Ok(record)
	}

	/// Whether this replica listed itself on a member's record in a space, whether that
	/// listing counts or not.
	fn listed_itself_on(&self, space: SpaceId, member: EntityId) -> Result<bool, NodeError> {
		let own = self.replica.signing.verifying_key();
		let record = replica_record(space, &own);
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.governing_statements(&open.keys)?
			.iter()
			.any(|governing| {
				governing.author == own
					&& governing.statement.entity == record
					&& governing.statement.field == fields::LISTING
					&& governing
						.statement
						.value
						.as_text()
						.and_then(Listing::from_text)
						.is_some_and(|listing| listing.owner == member)
			}))
	}

	/// Lists this replica in a space on a member's record, with the listing its key signed,
	/// in a change that governs the space, and its records after it.
	fn list_on(
		&mut self,
		space: SpaceId,
		member: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let signing = self.replica.signing.verifying_key();
		let encryption = self.replica.decryption.encryption_key();
		let listing = Listing::sign(&self.replica.signing, &encryption, space, (member, false));
		let (device, app) = self.own_names()?;
		let endpoint = self.endpoint();
		let listed = list_replica(
			&SpaceReplica {
				record: listing.record,
				device_record: nibkeep_governance::device_record(space, &signing),
				member,
				signing: &signing,
				encryption: &encryption,
				listing: &listing.to_text(),
				endpoint: Some(&endpoint),
				device: &device,
				app: &app,
				ai_agent: false,
			},
			rng,
		);
		self.commit(space, listed.governing, wall_millis, rng)?;
		self.commit(space, listed.operations, wall_millis, rng)
	}

	/// After a sync: lists this replica, in each shared space where it is no direct member,
	/// on a record that an admin made there for its user and on which another replica of the
	/// user is listed already, as the user confirmed on another device.
	pub(crate) fn settle_direct(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		let keys = self.user_keys()?;
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		for space in spaces {
			if space == self.info.personal
				|| !self.has_members(space)?
				|| self.check_not_revoked(space).is_err()
				|| self.member_in(space)?.is_some()
			{
				continue;
			}
			let confirmed = self
				.made_for_user(space, &keys)?
				.into_iter()
				.find(|(_, listed)| *listed);
			if let Some((record, _)) = confirmed
				&& !self.listed_itself_on(space, record)?
			{
				self.list_on(space, record, wall_millis, rng)?;
			}
		}
		Ok(())
	}

	/// Keeps a member through a group out of a shared space, as an admin of it, naming them by
	/// their keys: they stay in the group, but no group gives them the space, which gets a
	/// new key, sealed to the replicas that remain.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin of the space,
	/// [`NodeError::NoMember`] if the group holds no such member with a replica, and an error
	/// if a change cannot be saved.
	pub fn keep_out(
		&mut self,
		space: SpaceId,
		(group, member): (SpaceId, EntityId),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.check_admin(space, "keep members out")?;
		let keys = self.keys_in_group(space, (group, member))?;
		self.set_kept_out(space, &keys, true, wall_millis, rng)?;
		self.after_departure(space, wall_millis, rng);
		Ok(())
	}

	/// Lets a member through a group whom a shared space keeps out back in, as an admin of
	/// it.
	///
	/// # Errors
	///
	/// As [`Library::keep_out`].
	pub fn let_in(
		&mut self,
		space: SpaceId,
		(group, member): (SpaceId, EntityId),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.check_admin(space, "let members back in")?;
		let keys = self.keys_in_group(space, (group, member))?;
		self.set_kept_out(space, &keys, false, wall_millis, rng)
	}

	/// The keys of a member's replicas in a group that is in a space, at any level.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoMember`] if the space holds no such group, or the group no such
	/// member with a replica.
	fn keys_in_group(
		&self,
		space: SpaceId,
		(group, member): (SpaceId, EntityId),
	) -> Result<Vec<VerifyingKey>, NodeError> {
		let keys = if self.inner_groups(space)?.contains(&group) {
			self.member_keys(group, member)?
		} else {
			Vec::new()
		};
		if keys.is_empty() {
			return Err(NodeError::NoMember(member.to_string()));
		}
		Ok(keys)
	}

	/// States, on the record in a space derived from each of `keys`, whether it keeps the
	/// key's person out, replacing what the records say now, in a change that governs the
	/// space.
	pub(crate) fn set_kept_out(
		&mut self,
		space: SpaceId,
		keys: &[VerifyingKey],
		keeps: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let operations = self.kept_out_operations(space, keys, keeps, rng)?;
		self.commit(space, operations, wall_millis, rng)
	}

	/// The operations that state, on the record in a space derived from each of `keys`,
	/// whether it keeps the key's person out, replacing what the records say now.
	pub(crate) fn kept_out_operations(
		&self,
		space: SpaceId,
		keys: &[VerifyingKey],
		keeps: bool,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut operations = Vec::new();
		for key in keys {
			let record = replica_record(space, key);
			let current: Vec<StatementId> = state
				.current(record, &fields::KEPT_OUT)?
				.iter()
				.map(|statement| statement.id)
				.collect();
			operations.push(Operation::replace(
				Statement::new(
					StatementId::random(rng),
					record,
					fields::KEPT_OUT,
					Value::Boolean(keeps),
				),
				current,
			));
		}
		Ok(operations)
	}

	/// The keys of `member`'s replicas in a shared space that groups there list, with the keys
	/// that the member's record was made for: a member removed from the space, or who leaves
	/// it, is kept out through groups too.
	pub(crate) fn keys_through_groups(
		&self,
		space: SpaceId,
		member: EntityId,
	) -> Result<Vec<VerifyingKey>, NodeError> {
		let (_, owners) = self.users_and_owners(space)?;
		let open = self.space(space)?;
		let log = open.stored.log();
		let made_for: BTreeSet<[u8; 32]> = log
			.governing_statements(&open.keys)?
			.iter()
			.filter(|governing| governing.statement.entity == member)
			.filter_map(|governing| {
				VerifyingKey::from_text(governing.counting_text(&fields::MADE_FOR)?)
			})
			.map(|key| *key.as_bytes())
			.collect();
		Ok(log
			.listed_through_groups(&open.keys)?
			.into_iter()
			.filter(|key| {
				owners.get(key.as_bytes()) == Some(&member) || made_for.contains(key.as_bytes())
			})
			.collect())
	}

	/// The member's record of this replica's user in a shared space: its own, or else the one
	/// that another replica of the user is listed on, as before this one lists itself there.
	pub(crate) fn user_member(&self, space: SpaceId) -> Result<Option<EntityId>, NodeError> {
		if let Some(member) = self.member_in(space)? {
			return Ok(Some(member));
		}
		let keys = self.user_keys()?;
		let (_, owners) = self.users_and_owners(space)?;
		Ok(owners
			.iter()
			.find(|(key, _)| keys.contains(*key))
			.map(|(_, owner)| *owner))
	}

	/// After someone left a space, or was removed or kept out: gives the space a new key, and
	/// a new secrets key where this replica's secrets are unlocked; or else the next sync
	/// does, as this replica's newest key is not safe.
	pub(crate) fn after_departure(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) {
		if let Ok(roster) = self.roster(space) {
			let _ = self.rotate(space, &[], &roster, wall_millis, rng);
		}
		let _ = self.settle_secrets(wall_millis, rng);
	}
}
