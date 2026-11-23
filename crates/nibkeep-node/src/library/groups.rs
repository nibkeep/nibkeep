//! Groups in spaces ([technical design: groups]): a group is a space with members and no data
//! of its own. An admin of a space who is a member of a group adds it there, as an editor or
//! a viewer, and asks in the group that it join; an admin of the group agrees. Every replica
//! of the space then keeps the group's log, follows it, and anchors its changes, which take
//! effect in the space at those anchors; the group's members' replicas get the space from
//! a replica of an admin of the group that holds it, through entries in the group. A group
//! may be in another group the same way: the spaces the outer group is in follow and anchor
//! both, and the inner group's members get them through the outer group's entries.
//!
//! [technical design: groups]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{EncryptionKey, key_hash};
use nibkeep_governance::{
	Anchor, GroupKey, GroupRef, GroupRequest, Joining, SpaceEntry, give_space_entry, replica_record,
};
use nibkeep_log::ChangeId;
use nibkeep_model::{FieldName, Operation, fields};

use super::Library;
use crate::{MemberRole, NodeError};

/// A group that a space's governing changes add, as a program shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInSpace {
	/// The group's record in the space.
	pub record: EntityId,
	/// The group.
	pub group: SpaceId,
	/// The group's name, where this replica holds its log.
	pub name: Option<String>,
	/// The group's role in the space now: `none` once an admin took it out.
	pub role: MemberRole,
	/// Whether the group agreed to join, and has not withdrawn, as the space knows it.
	pub agreed: bool,
	/// The group's members, with their roles in the group, where this replica holds its log.
	pub members: Vec<GroupMember>,
	/// The groups inside the group, as its log says, where this replica holds it.
	pub groups: Vec<GroupInSpace>,
}

/// A member of a group, with their role in the group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
	/// Their record in the group.
	pub record: EntityId,
	/// Their name there.
	pub name: Option<String>,
	/// Their role in the group.
	pub role: MemberRole,
}

/// A request, in a group, that the group join a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupJoinRequest {
	/// The space.
	pub space: SpaceId,
	/// The space's name, where it is sealed to this replica, as to the group's admins.
	pub name: Option<String>,
	/// Whether an admin of the group agreed.
	pub agreed: bool,
	/// Whether an admin withdrew that agreement, for good.
	pub withdrawn: bool,
}

impl Library {
	/// Whether a space this library holds is a group.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpace`] if the library holds no such space, and an error if it
	/// cannot be read.
	pub fn is_group(&self, space: SpaceId) -> Result<bool, NodeError> {
		self.space(space)?.is_group()
	}

	/// Refuses entities of its own in a group, which every member of the spaces it is in
	/// reads.
	pub(crate) fn check_not_group(&self, space: SpaceId) -> Result<(), NodeError> {
		if self.is_group(space)? {
			return Err(NodeError::GroupHoldsNoData);
		}
		Ok(())
	}

	/// Adds a group to a space, as an admin of the space who is a member of the group, with
	/// `role`, an editor's or a viewer's: a record of the group in the space, and a request in
	/// the group that it join, with the space's name sealed to the group's admins. As an admin
	/// of the group too, this replica agrees at once, and anchors the agreement in the space,
	/// which gives the space the group's first key. Returns the group's record in the space.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin of the space,
	/// [`NodeError::GroupAsAdmin`] for the role of admin, [`NodeError::NotAGroup`] if the
	/// library holds no such group, [`NodeError::NotInGroup`] if this replica is no member of
	/// it, [`NodeError::GroupCycle`] if the space is the group or a group inside it, at any
	/// level, and an error if a change cannot be saved.
	pub fn add_group(
		&mut self,
		space: SpaceId,
		group: SpaceId,
		role: MemberRole,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.observe_wall(wall_millis);
		self.check_admin(space, "add groups")?;
		if !matches!(role, MemberRole::Editor | MemberRole::Viewer) {
			return Err(NodeError::GroupAsAdmin);
		}
		if !self.spaces.contains_key(&group) || !self.is_group(group)? {
			return Err(NodeError::NotAGroup(group));
		}
		if space == group || self.inner_groups(group)?.contains(&space) {
			return Err(NodeError::GroupCycle);
		}
		self.check_not_revoked(group)
			.map_err(|_| NodeError::NotInGroup)?;
		let member = self.member_in(group)?.ok_or(NodeError::NotInGroup)?;
		// Everything that may fail is read before anything is written.
		let (group_root, space_root) = (self.root_of(group)?, self.root_of(space)?);
		let first_key = self
			.first_key(group)?
			.ok_or_else(|| NodeError::Damaged(format!("the group {group} has no first key")))?;
		let name = self.space_name(space)?.unwrap_or_default();
		let admins = self.admins_replicas(group)?;
		let entity = self.group_entity(group)?;
		let open = self.space(group)?;
		let agrees =
			open.stored.log().member_roles(&open.keys)?.get(&member) == Some(&MemberRole::Admin);
		let record = EntityId::random(rng);
		let (operations, addition) = nibkeep_governance::add_group(
			record,
			GroupRef {
				group,
				root: group_root,
				key: key_hash(&first_key),
			},
			role.as_text(),
			rng,
		);
		self.commit(space, operations, wall_millis, rng)?;
		let joining = Joining {
			space,
			root: space_root,
			addition,
		};
		let request = GroupRequest::new(joining, &name, &admins, rng);
		let mut operations = vec![nibkeep_governance::ask_to_join(entity, &request, rng)];
		if agrees {
			operations.push(nibkeep_governance::agree_to_join(entity, &joining, rng));
		}
		self.commit(group, operations, wall_millis, rng)?;
		Ok(record)
	}

	/// Takes a group out of a space, as an admin of the space, with the role none on its
	/// record there: its members leave the space, unless they are members there some other
	/// way, and it joins again only by a new addition.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin of the space,
	/// [`NodeError::NotAGroup`] if the space adds no such group, and an error if the change
	/// cannot be saved.
	pub fn remove_group(
		&mut self,
		space: SpaceId,
		group: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.check_admin(space, "take groups out")?;
		let open = self.space(space)?;
		let records: Vec<EntityId> = open
			.stored
			.log()
			.groups(&open.keys)?
			.into_iter()
			.filter(|found| {
				found.group.group == group
					&& found.role.is_some_and(|role| role != MemberRole::None)
			})
			.map(|found| found.record)
			.collect();
		if records.is_empty() {
			return Err(NodeError::NotAGroup(group));
		}
		for record in records {
			self.set_role(space, record, MemberRole::None, wall_millis, rng)?;
		}
		Ok(())
	}

	/// The requests in a group that it join spaces, with the spaces' names where they are
	/// sealed to this replica, and whether an admin agreed or withdrew.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotAGroup`] if the space is no group, and an error if it cannot be
	/// read.
	pub fn join_requests(&self, group: SpaceId) -> Result<Vec<GroupJoinRequest>, NodeError> {
		if !self.is_group(group)? {
			return Err(NodeError::NotAGroup(group));
		}
		let own = replica_record(group, &self.replica.signing.verifying_key());
		let (agreements, withdrawals) = self.joinings(group)?;
		// Of the joinings of each space, whether one stands, agreed and not withdrawn, and
		// whether one was withdrawn.
		let mut standing: BTreeMap<SpaceId, (bool, bool)> = BTreeMap::new();
		let mut requests: BTreeMap<SpaceId, GroupJoinRequest> = BTreeMap::new();
		let open = self.space(group)?;
		for governing in open.stored.log().governing_statements(&open.keys)? {
			let Some(request) = governing
				.counting_text(&fields::JOIN_REQUEST)
				.and_then(GroupRequest::from_text)
			else {
				continue;
			};
			let joining = request.joining;
			let found = requests.entry(joining.space).or_insert(GroupJoinRequest {
				space: joining.space,
				name: None,
				agreed: false,
				withdrawn: false,
			});
			if found.name.is_none() {
				found.name = request.name_for(own, &self.replica.decryption);
			}
			let state = standing.entry(joining.space).or_default();
			let withdrawn = withdrawals.contains(&joining);
			state.0 |= agreements.contains(&joining) && !withdrawn;
			state.1 |= withdrawn;
		}
		for (space, (stands, withdrawn)) in standing {
			if let Some(request) = requests.get_mut(&space) {
				request.agreed = stands;
				request.withdrawn = withdrawn && !stands;
			}
		}
		Ok(requests.into_values().collect())
	}

	/// Agrees, as an admin of a group, to the requests that it join a space, and anchors the
	/// agreement there if this replica holds the space.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin of the group,
	/// [`NodeError::NoGroupRequest`] if no request names the space, and an error if a change
	/// cannot be saved.
	pub fn agree_to_join(
		&mut self,
		group: SpaceId,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.state_joinings(group, space, false, wall_millis, rng)
	}

	/// Withdraws, as an admin of a group, its agreement to join a space, for good: the group
	/// leaves the space as a removal does, and joins it again only by a new addition and a
	/// new agreement.
	///
	/// # Errors
	///
	/// As [`Library::agree_to_join`].
	pub fn withdraw_group(
		&mut self,
		group: SpaceId,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.state_joinings(group, space, true, wall_millis, rng)
	}

	/// Agrees to, or with `withdraw` withdraws, every joining of a space that the group's
	/// requests name and that is not stated so already.
	fn state_joinings(
		&mut self,
		group: SpaceId,
		space: SpaceId,
		withdraw: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if !self.is_group(group)? {
			return Err(NodeError::NotAGroup(group));
		}
		self.check_admin(group, if withdraw { "withdraw" } else { "agree" })?;
		let (agreements, withdrawals) = self.joinings(group)?;
		let mut joinings = BTreeSet::new();
		let open = self.space(group)?;
		for governing in open.stored.log().governing_statements(&open.keys)? {
			if let Some(request) = governing
				.counting_text(&fields::JOIN_REQUEST)
				.and_then(GroupRequest::from_text)
				&& request.joining.space == space
			{
				joinings.insert(request.joining);
			}
		}
		if joinings.is_empty() {
			return Err(NodeError::NoGroupRequest);
		}
		let entity = self.group_entity(group)?;
		let operations: Vec<_> = joinings
			.iter()
			.filter(|joining| {
				if withdraw {
					agreements.contains(joining) && !withdrawals.contains(joining)
				} else {
					!agreements.contains(joining) && !withdrawals.contains(joining)
				}
			})
			.map(|joining| {
				if withdraw {
					nibkeep_governance::withdraw_from(entity, joining, rng)
				} else {
					nibkeep_governance::agree_to_join(entity, joining, rng)
				}
			})
			.collect();
		if !operations.is_empty() {
			self.commit(group, operations, wall_millis, rng)?;
		}
		Ok(())
	}

	/// The groups that a space's governing changes add, with their roles, whether they agreed,
	/// and, where this replica holds a group's log, its name and members.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, and an error if a space
	/// cannot be read.
	pub fn space_groups(&self, space: SpaceId) -> Result<Vec<GroupInSpace>, NodeError> {
		self.check_shared(space)?;
		self.groups_of(space, &mut BTreeSet::from([space]))
	}

	/// The groups that a space's or a group's governing changes add, as
	/// [`Library::space_groups`] gives them, with those inside each, at every level, but the
	/// groups on the way there, `path`, which hold it.
	fn groups_of(
		&self,
		space: SpaceId,
		path: &mut BTreeSet<SpaceId>,
	) -> Result<Vec<GroupInSpace>, NodeError> {
		let open = self.space(space)?;
		let mut groups = Vec::new();
		for found in open.stored.log().groups(&open.keys)? {
			let group = found.group.group;
			let (name, members, inner) = if self.spaces.contains_key(&group) {
				let inner = if path.insert(group) {
					let inner = self.groups_of(group, path)?;
					path.remove(&group);
					inner
				} else {
					Vec::new()
				};
				(self.space_name(group)?, self.group_members(group)?, inner)
			} else {
				(None, Vec::new(), Vec::new())
			};
			groups.push(GroupInSpace {
				record: found.record,
				group,
				name,
				role: found.role.unwrap_or(MemberRole::None),
				agreed: found.agreed,
				members,
				groups: inner,
			});
		}
		Ok(groups)
	}

	/// The groups that a space's governing changes add, and, at every level, those that the
	/// governing changes of each of them whose log this replica holds add, but the space
	/// itself: what the space follows.
	pub(crate) fn inner_groups(&self, space: SpaceId) -> Result<BTreeSet<SpaceId>, NodeError> {
		let mut found = BTreeSet::new();
		let mut to_visit = vec![space];
		while let Some(at) = to_visit.pop() {
			let Some(open) = self.spaces.get(&at) else {
				continue;
			};
			for group in open.stored.log().groups(&open.keys)? {
				let group = group.group.group;
				if group != space && found.insert(group) {
					to_visit.push(group);
				}
			}
		}
		Ok(found)
	}

	/// A group's members, with their roles and names there, but the groups inside it.
	fn group_members(&self, group: SpaceId) -> Result<Vec<GroupMember>, NodeError> {
		let open = self.space(group)?;
		let state = open.stored.log().state();
		let groups: BTreeSet<EntityId> = open
			.stored
			.log()
			.groups(&open.keys)?
			.iter()
			.map(|found| found.record)
			.collect();
		let mut members = Vec::new();
		for (record, role) in open.stored.log().member_roles(&open.keys)? {
			if groups.contains(&record) {
				continue;
			}
			members.push(GroupMember {
				record,
				name: super::entities::display_name(state, &open.types, &self.languages, record)?,
				role,
			});
		}
		Ok(members)
	}

	/// Settles the groups after a sync: takes the first keys of the groups that the spaces it
	/// holds give, and the spaces that the groups it is in give it, follows each space's
	/// groups, anchors the group changes that no anchor names yet, and gives the spaces it
	/// holds to the members of the groups in them that lack them.
	pub(crate) fn settle_groups(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		// Each step goes on whatever another's error; the first is told at the end.
		let results = [
			self.take_group_keys(rng),
			self.take_group_entries(wall_millis, rng),
			self.follow_all_groups(),
			self.anchor_all_groups(wall_millis, rng),
			self.give_group_entries(wall_millis, rng),
			self.settle_direct(wall_millis, rng),
		];
		results.into_iter().collect()
	}

	/// Follows the groups of every space it holds, as its copies of their logs say: done when
	/// the library opens, before it takes or makes changes, and after each sync.
	pub(crate) fn follow_all_groups(&mut self) -> Result<(), NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		for space in spaces {
			if space != self.info.personal {
				self.follow_groups(space)?;
			}
		}
		Ok(())
	}

	/// Follows the groups that a space's governing changes add, and those inside them at every
	/// level, as this replica's copies of their logs say; a group whose log it lacks is
	/// followed with nothing.
	fn follow_groups(&mut self, space: SpaceId) -> Result<(), NodeError> {
		let groups = self.inner_groups(space)?;
		if groups.is_empty() {
			return Ok(());
		}
		let mut followed = Vec::new();
		for group in groups {
			if let Some(open) = self.spaces.get(&group) {
				followed.push(open.stored.log().followed(&open.keys)?);
			}
		}
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let keys = open.keys.clone();
		open.stored.follow(&followed, &keys)?;
		self.read_again(space)
	}

	/// After a change to a group that this replica made: follows it in each space it holds
	/// that the group is in, at any level, and anchors the change there.
	pub(crate) fn after_group_change(
		&mut self,
		group: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		for space in spaces {
			if space == self.info.personal || space == group {
				continue;
			}
			if self.inner_groups(space)?.contains(&group) {
				self.follow_groups(space)?;
				self.anchor_groups(space, wall_millis, rng)?;
			}
		}
		Ok(())
	}

	/// Anchors, in every space it holds, the group changes that no anchor names yet.
	fn anchor_all_groups(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		let mut first_error = Ok(());
		for space in spaces {
			if space == self.info.personal {
				continue;
			}
			let anchored = self.anchor_groups(space, wall_millis, rng);
			if first_error.is_ok() {
				first_error = anchored;
			}
		}
		first_error
	}

	/// Anchors, in a space or a group this replica is in, the changes of each group that
	/// reaches it, at any level, whose log it holds, and whose governing changes that no other
	/// builds on no counting anchor names yet, with the group's first key where no counting
	/// change gives it: so the group's changes take effect there. It anchors no group that
	/// holds the space at some level, which gives nothing through the cycle, and whose
	/// anchors would answer each other's for ever.
	fn anchor_groups(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.check_not_revoked(space).is_err() {
			return Ok(());
		}
		let operations = self.anchoring(space, rng)?;
		if !operations.is_empty() {
			self.commit(space, operations, wall_millis, rng)?;
		}
		Ok(())
	}

	/// The operations that anchor in a space the groups that reach it, as
	/// [`Library::anchor_groups`] says, whether or not this replica is listed there yet: a
	/// replica that a group lists, and that the space lists through no anchor yet, is listed
	/// there by its own anchor, as one that a recovery code brings back is.
	pub(crate) fn anchoring(
		&self,
		space: SpaceId,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		// A space it has nothing of yet anchors nothing.
		let Some(space_root) = self.space(space)?.stored.log().root()? else {
			return Ok(Vec::new());
		};
		let open = self.space(space)?;
		let anchored = open.stored.log().anchored(&open.keys)?;
		let given = self.given_group_keys(space)?;
		// Only a group can be inside a group that it holds.
		let cycles = self.is_group(space)?;
		let mut operations = Vec::new();
		for (record, found) in self.reaching_groups(space, space_root)? {
			let group = found.group;
			if cycles && self.inner_groups(group)?.contains(&space) {
				continue;
			}
			let open_group = self.space(group)?;
			let heads = open_group.stored.log().followed(&open_group.keys)?.heads();
			let known = anchored.get(&group);
			if heads
				.iter()
				.all(|head| known.is_some_and(|known| known.contains(head)))
			{
				continue;
			}
			let key = if given.contains(&found) {
				None
			} else {
				open_group
					.keys
					.space_keys()
					.find(|(epoch, _)| *epoch == 0)
					.map(|(_, key)| GroupKey {
						group: found,
						key: key.clone(),
					})
			};
			operations.extend(nibkeep_governance::anchor(
				record,
				&Anchor {
					group,
					changes: heads,
				},
				key.as_ref(),
				rng,
			));
		}
		Ok(operations)
	}

	/// The groups that reach a space, as this replica's copies of their logs say: each group
	/// that the space adds with a role, whose log it holds, naming that log's first change,
	/// and that agreed to that addition, once only, also once it withdrew, as an anchor of a
	/// withdrawal is needed too; and, the same way, each group inside one of those that has not
	/// withdrawn, at every level. Each comes with its record in the space, or that of the group
	/// in the space it reaches it through.
	pub(crate) fn reaching_groups(
		&self,
		space: SpaceId,
		root: ChangeId,
	) -> Result<Vec<(EntityId, GroupRef)>, NodeError> {
		let mut reaching = Vec::new();
		let mut seen = BTreeSet::from([space]);
		let mut joinings: BTreeMap<SpaceId, (BTreeSet<Joining>, BTreeSet<Joining>)> =
			BTreeMap::new();
		let mut to_visit: Vec<(SpaceId, ChangeId, Option<EntityId>)> = vec![(space, root, None)];
		while let Some((container, container_root, through)) = to_visit.pop() {
			let open = self.space(container)?;
			for found in open.stored.log().groups(&open.keys)? {
				let group = found.group.group;
				if seen.contains(&group)
					|| !matches!(found.role, Some(MemberRole::Editor | MemberRole::Viewer))
				{
					continue;
				}
				let Some(open_group) = self.spaces.get(&group) else {
					continue;
				};
				if open_group.stored.log().root()? != Some(found.group.root) {
					continue;
				}
				if let std::collections::btree_map::Entry::Vacant(entry) = joinings.entry(group) {
					entry.insert(self.joinings(group)?);
				}
				let joining = Joining {
					space: container,
					root: container_root,
					addition: found.addition,
				};
				let Some((agreed, withdrawn)) = joinings.get(&group) else {
					continue;
				};
				if !agreed.contains(&joining) {
					continue;
				}
				seen.insert(group);
				let record = through.unwrap_or(found.record);
				reaching.push((record, found.group));
				if !withdrawn.contains(&joining) {
					to_visit.push((group, found.group.root, Some(record)));
				}
			}
		}
		Ok(reaching)
	}

	/// The groups, with their first changes, whose first keys a counting governing change of a
	/// space gives.
	fn given_group_keys(&self, space: SpaceId) -> Result<BTreeSet<GroupRef>, NodeError> {
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.governing_statements(&open.keys)?
			.into_iter()
			.filter_map(|governing| {
				GroupKey::from_text(governing.counting_text(&fields::GROUP_KEY)?)
			})
			.map(|key| key.group)
			.collect())
	}

	/// Takes the first key of each group that a space it holds gives the key of, and whose log
	/// it lacks: only the key whose hash a record of the group names, in a space or a group it
	/// holds, as the spaces a group is in give the keys of the groups inside it too; keeps the
	/// group's log, empty until it syncs it, taking as its first change only the one that the
	/// record names.
	pub(crate) fn take_group_keys(&mut self, rng: &mut impl CryptoRng) -> Result<(), NodeError> {
		let mut records: BTreeSet<GroupRef> = BTreeSet::new();
		for (&space, open) in &self.spaces {
			if space != self.info.personal {
				records.extend(
					open.stored
						.log()
						.groups(&open.keys)?
						.iter()
						.map(|group| group.group),
				);
			}
		}
		if records.is_empty() {
			return self.expect_group_roots();
		}
		let mut found: BTreeMap<GroupRef, (bool, nibkeep_crypto::Key)> = BTreeMap::new();
		for (&space, open) in &self.spaces {
			if space == self.info.personal {
				continue;
			}
			let log = open.stored.log();
			for governing in log.governing_statements(&open.keys)? {
				if governing.statement.field != fields::GROUP_KEY {
					continue;
				}
				let Some(key) = governing
					.statement
					.value
					.as_text()
					.and_then(GroupKey::from_text)
				else {
					continue;
				};
				// Only the key whose hash the record of the group names.
				if !records.contains(&key.group)
					|| key_hash(&key.key) != key.group.key
					|| self.spaces.contains_key(&key.group.group)
				{
					continue;
				}
				found
					.entry(key.group)
					.or_insert((governing.counts, key.key));
			}
		}
		for (group, (_, key)) in found {
			self.add_space(group.group, &key, rng)?;
			if let Some(open) = self.spaces.get_mut(&group.group) {
				open.stored.expect_roots([group.root]);
			}
		}
		self.expect_group_roots()
	}

	/// Has each group's log that has no first change yet take as its first change only one
	/// that a space's record of the group names, besides any that another source names.
	pub(crate) fn expect_group_roots(&mut self) -> Result<(), NodeError> {
		let mut roots: BTreeMap<SpaceId, BTreeSet<ChangeId>> = BTreeMap::new();
		for open in self.spaces.values() {
			for found in open.stored.log().groups(&open.keys)? {
				roots
					.entry(found.group.group)
					.or_default()
					.insert(found.group.root);
			}
		}
		for (group, roots) in roots {
			if let Some(open) = self.spaces.get_mut(&group)
				&& open.stored.log().root()?.is_none()
			{
				open.stored.expect_roots_too(roots);
			}
		}
		Ok(())
	}

	/// Gives, in each group this replica's member is an admin of, the spaces it holds that the
	/// group is in to the group's members' replicas that lack them, but AI agents', and to
	/// their recovery codes that stand: an entry for each, sealed to the replica or the code,
	/// with the space's first key, naming its first change.
	/// Only for a space whose record of the group has a role, and that the group agreed to
	/// join and has not withdrawn from. Only as an admin, as an entry counts from no other
	/// member: another's would be set aside, and made again after every sync.
	fn give_group_entries(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let groups: Vec<SpaceId> = self
			.spaces
			.keys()
			.copied()
			.filter(|&space| self.is_group(space).unwrap_or(false))
			.collect();
		for group in groups {
			if self.check_admin(group, "give a space").is_err() {
				continue;
			}
			let (agreements, withdrawals) = self.joinings(group)?;
			let (given, remaining) = self.group_recipients(group)?;
			let mut operations = Vec::new();
			let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
			for space in spaces {
				if space == self.info.personal
					|| space == group
					|| self.check_not_revoked(space).is_err()
				{
					continue;
				}
				let open = self.space(space)?;
				let in_space = open
					.stored
					.log()
					.groups(&open.keys)?
					.into_iter()
					.find(|found| {
						found.group.group == group
							&& found.agreed
							&& matches!(found.role, Some(MemberRole::Editor | MemberRole::Viewer))
					});
				let Some(found) = in_space else { continue };
				let Some(root) = self.space(space)?.stored.log().root()? else {
					continue;
				};
				let joining = Joining {
					space,
					root,
					addition: found.addition,
				};
				if !agreements.contains(&joining) || withdrawals.contains(&joining) {
					continue;
				}
				let Some(first) = self
					.space(space)?
					.keys
					.space_keys()
					.find(|(epoch, _)| *epoch == 0)
					.map(|(_, key)| key.clone())
				else {
					continue;
				};
				let hash = key_hash(&first);
				for (record, member, encryption) in &remaining {
					if given.contains(&(*record, space)) {
						continue;
					}
					let entry = SpaceEntry::seal(
						encryption,
						(space, *member, *root.hash()),
						*record,
						(&first, hash),
						rng,
					);
					operations.push(give_space_entry(*record, &entry, rng));
				}
			}
			if !operations.is_empty() {
				self.commit(group, operations, wall_millis, rng)?;
			}
		}
		Ok(())
	}

	/// The spaces that a group's counting entries give each replica or code, as
	/// `(record, space)`, and the group's members' replicas that remain, but AI agents' and
	/// this one, and their recovery codes that the group's keys are given to: each record,
	/// its member and its encryption key.
	#[expect(
		clippy::type_complexity,
		reason = "a pair of what is given and to whom, each read once"
	)]
	fn group_recipients(
		&self,
		group: SpaceId,
	) -> Result<
		(
			BTreeSet<(EntityId, SpaceId)>,
			Vec<(EntityId, EntityId, EncryptionKey)>,
		),
		NodeError,
	> {
		let own = self.replica.signing.verifying_key();
		let roster = self.roster(group)?;
		let open = self.space(group)?;
		let log = open.stored.log();
		let given = log
			.governing_statements(&open.keys)?
			.into_iter()
			.filter_map(|governing| {
				let entry = SpaceEntry::from_text(governing.counting_text(&fields::SPACE_ENTRY)?)?;
				Some((governing.statement.entity, entry.space))
			})
			.collect();
		// The members of keys that groups inside the group list are in those groups.
		let (_, mut owners) = self.users_and_owners(group)?;
		for inner in self.inner_groups(group)? {
			if self.spaces.contains_key(&inner) {
				for (key, owner) in self.users_and_owners(inner)?.1 {
					owners.entry(key).or_insert(owner);
				}
			}
		}
		let mut remaining = Vec::new();
		for record in roster.replicas() {
			let Some(key) = roster.keys_of(record).first() else {
				continue;
			};
			if *key == own
				|| log.is_revoked(key, &open.keys)?
				|| log.is_removed(key, &open.keys)?
				|| log.is_ai_agent(key, &open.keys)?
			{
				continue;
			}
			if let (Some(owner), Some(encryption)) =
				(owners.get(key.as_bytes()), roster.encryption_of(record))
			{
				remaining.push((record, *owner, encryption.clone()));
			}
		}
		// A code is given the group's spaces as its member's replicas are, so that a device
		// that it brings back finds them: while it stands and its member is in the group.
		for code in self.code_recipients(group, &roster, &[])? {
			remaining.push((
				code.record,
				code.statement.member,
				code.statement.encryption.clone(),
			));
		}
		Ok((given, remaining))
	}

	/// Takes the spaces that the groups this replica is in give it, on its record there, and
	/// that it lacks, as [`Library::agreed_entries`] lists them, as spaces made elsewhere: as
	/// [`Library::takes_entry`] says, by its newest choice of each and its mark for new
	/// spaces; opens each first key, keeps it if its hash is the one named, and keeps the
	/// space, empty until it syncs it, taking as its first change only the one named; and
	/// states, in one change, that it holds those it took.
	fn take_group_entries(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.replica.signing.verifying_key();
		let groups: Vec<SpaceId> = self
			.spaces
			.keys()
			.copied()
			.filter(|&space| self.is_group(space).unwrap_or(false))
			.collect();
		if groups.is_empty() {
			return Ok(());
		}
		let read = self.read_personal()?;
		let choices = self.own_choices(&read);
		let takes_new = self.own_mark(&read);
		let mut taking = Vec::new();
		for group in groups {
			if self.check_not_revoked(group).is_err() {
				continue;
			}
			let record = replica_record(group, &own);
			for (entry, root) in self.agreed_entries(group, record)? {
				if self.spaces.contains_key(&entry.space)
					|| !Self::takes_entry(choices.get(&entry.space).copied(), takes_new)
				{
					continue;
				}
				if let Some(key) = entry.open(&self.replica.decryption, record)
					&& key_hash(&key) == entry.hash
				{
					taking.push((entry.space, root, key));
				}
			}
		}
		let mut taken = Vec::new();
		for (space, root, key) in taking {
			if self.spaces.contains_key(&space) {
				continue;
			}
			self.add_space(space, &key, rng)?;
			if let Some(open) = self.spaces.get_mut(&space) {
				open.stored.expect_roots([root]);
			}
			taken.push(space);
		}
		if taken.is_empty() {
			return Ok(());
		}
		self.state_holding(&taken, wall_millis, rng)
	}

	/// The entries that a group gives on `record`, this replica's record there, each with the
	/// first change it names: only for a space that the group agreed to join, naming that
	/// first change, and has not withdrawn from.
	pub(crate) fn agreed_entries(
		&self,
		group: SpaceId,
		record: EntityId,
	) -> Result<Vec<(SpaceEntry, ChangeId)>, NodeError> {
		let (agreements, withdrawals) = self.joinings(group)?;
		let open = self.space(group)?;
		let mut entries = Vec::new();
		for governing in open.stored.log().governing_statements(&open.keys)? {
			let Some(entry) = governing
				.counting_text(&fields::SPACE_ENTRY)
				.filter(|_| governing.statement.entity == record)
				.and_then(SpaceEntry::from_text)
			else {
				continue;
			};
			let root = ChangeId::from_hash(entry.root);
			let agreed = agreements.iter().any(|joining| {
				joining.space == entry.space
					&& joining.root == root
					&& !withdrawals.contains(joining)
			});
			if agreed {
				entries.push((entry, root));
			}
		}
		Ok(entries)
	}

	/// Has each space that this replica holds but has no first change of take as its first
	/// change only one that the groups name: a group's log, as
	/// [`Library::expect_group_roots`] says, and a space that a group gave it, as
	/// [`Library::expect_group_entry_roots`] says; each whatever the other's error. The log
	/// expects a first change only while the library is open, so this is done when it opens,
	/// and when it opens a space that another program took. Where a group cannot be read,
	/// each such space that nothing named a first change for takes none until the library
	/// opens again, rather than any.
	pub(crate) fn expect_roots_from_groups(&mut self) -> Result<(), NodeError> {
		let logs = self.expect_group_roots();
		let entries = self.expect_group_entry_roots();
		if logs.is_err() || entries.is_err() {
			for (&space, open) in &mut self.spaces {
				if space != self.info.personal {
					open.stored.expect_roots_too([]);
				}
			}
		}
		logs.and(entries)
	}

	/// Has each space that this replica holds but has no first change of take as its first
	/// change only one that an entry a group gave it names, as [`Library::agreed_entries`]
	/// lists them, with the hash of the first key it keeps, besides any that another source
	/// names: so that a device of the group cannot have it take one of its own making. A
	/// group it cannot read is passed over, and the first error told once the others are
	/// read.
	fn expect_group_entry_roots(&mut self) -> Result<(), NodeError> {
		let mut waiting: BTreeMap<SpaceId, Hash> = BTreeMap::new();
		for (&space, open) in &self.spaces {
			if space != self.info.personal
				&& matches!(open.stored.log().root(), Ok(None))
				&& let Some(hash) = open.first_key_hash()
			{
				waiting.insert(space, hash);
			}
		}
		// Nothing to read where every space has its first change, as is usual.
		if waiting.is_empty() {
			return Ok(());
		}
		let own = self.replica.signing.verifying_key();
		let mut roots: BTreeMap<SpaceId, BTreeSet<ChangeId>> = BTreeMap::new();
		let mut failed = None;
		for (&group, open) in &self.spaces {
			let entries = open.is_group().and_then(|is_group| {
				if is_group {
					self.agreed_entries(group, replica_record(group, &own))
				} else {
					Ok(Vec::new())
				}
			});
			match entries {
				Ok(entries) => {
					for (entry, root) in entries {
						if waiting.get(&entry.space) == Some(&entry.hash) {
							roots.entry(entry.space).or_default().insert(root);
						}
					}
				}
				Err(error) => {
					failed.get_or_insert(error);
				}
			}
		}
		for (space, roots) in roots {
			if let Some(open) = self.spaces.get_mut(&space) {
				open.stored.expect_roots_too(roots);
			}
		}
		failed.map_or(Ok(()), Err)
	}

	/// The joinings that a group's counting governing changes agree to, and those they
	/// withdraw.
	fn joinings(
		&self,
		group: SpaceId,
	) -> Result<(BTreeSet<Joining>, BTreeSet<Joining>), NodeError> {
		let open = self.space(group)?;
		let mut agreements = BTreeSet::new();
		let mut withdrawals = BTreeSet::new();
		for governing in open.stored.log().governing_statements(&open.keys)? {
			let joining =
				|field: &FieldName| governing.counting_text(field).and_then(Joining::from_text);
			if let Some(joining) = joining(&fields::JOINED) {
				agreements.insert(joining);
			} else if let Some(joining) = joining(&fields::WITHDRAWN) {
				withdrawals.insert(joining);
			}
		}
		Ok((agreements, withdrawals))
	}

	/// The records and encryption keys of the replicas of a group's admins, but revoked ones.
	fn admins_replicas(&self, group: SpaceId) -> Result<Vec<(EntityId, EncryptionKey)>, NodeError> {
		let roster = self.roster(group)?;
		let (_, owners) = self.users_and_owners(group)?;
		let open = self.space(group)?;
		let log = open.stored.log();
		let roles = log.member_roles(&open.keys)?;
		let mut admins = Vec::new();
		for record in roster.replicas() {
			let Some(key) = roster.keys_of(record).first() else {
				continue;
			};
			let admin = owners
				.get(key.as_bytes())
				.is_some_and(|owner| roles.get(owner) == Some(&MemberRole::Admin));
			if admin
				&& !log.is_revoked(key, &open.keys)?
				&& let Some(encryption) = roster.encryption_of(record)
			{
				admins.push((record, encryption.clone()));
			}
		}
		Ok(admins)
	}

	/// A group's own entity, which its requests and agreements are stated on.
	fn group_entity(&self, group: SpaceId) -> Result<EntityId, NodeError> {
		self.space(group)?
			.own_entity()?
			.ok_or_else(|| NodeError::Damaged(format!("the group {group} has no record")))
	}

	/// A space's first key, if this replica holds it.
	fn first_key(&self, space: SpaceId) -> Result<Option<nibkeep_crypto::Key>, NodeError> {
		Ok(self
			.space(space)?
			.keys
			.space_keys()
			.find(|(epoch, _)| *epoch == 0)
			.map(|(_, key)| key.clone()))
	}

	/// A space's first change.
	fn root_of(&self, space: SpaceId) -> Result<ChangeId, NodeError> {
		self.space(space)?
			.stored
			.log()
			.root()?
			.ok_or_else(|| NodeError::Damaged(format!("the space {space} has no first change")))
	}
}
