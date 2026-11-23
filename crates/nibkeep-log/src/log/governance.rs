//! Which changes count, as the rules of [`nibkeep_governance::rules`] decide from the
//! governing changes that the replica can read
//! ([Log](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/log.md#8-which-changes-count)): what the log reads for them
//! from its history, and how it takes in or sets aside the changes whose decision changed.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_governance::rules::{
	Fact, Followed, Governance, Header, Reach, Role, Standing, View, decide,
};
use nibkeep_governance::{Claim, GoverningStatement, RecoveryListing, Roster, SpaceGroup};
use nibkeep_model::storage::Statements;
use nibkeep_model::{Operation, Statement, fields};
use nibkeep_time::Timestamp;

use super::{Log, PurgedBy, fits_an_ai_agent, holds_only_governing, read_payload};
use crate::history::{Accepted, History, Status};
use crate::{Change, ChangeId, Kind, LogError, Payload, Purges, Restores, SpaceKeys};

impl Header for Change {
	fn id(&self) -> ChangeId {
		Change::id(self)
	}

	fn author(&self) -> &VerifyingKey {
		Change::author(self)
	}

	fn timestamp(&self) -> Timestamp {
		Change::timestamp(self)
	}

	fn is_root(&self) -> bool {
		self.parents().is_empty()
	}

	fn space(&self) -> SpaceId {
		Change::space(self)
	}

	fn purges_or_seals(&self) -> bool {
		// A new kind of change that destroys or hides content must count here too, and so does
		// one that brings back what purges destroyed.
		self.purges_or_restores() || !self.sealed().is_empty()
	}
}

/// Whether the space's first change governs it, the governing changes that the replica can
/// read, in their order, and their statuses.
type Facts = (bool, Vec<Fact>, BTreeMap<ChangeId, Status>);

/// The latest governing changes that each change builds on, which no other of them builds
/// on, with the governing changes they were found among.
#[derive(Debug, Default)]
pub(super) struct Frontiers {
	among: BTreeSet<ChangeId>,
	latest: BTreeMap<ChangeId, Vec<ChangeId>>,
}

/// What a governing change says: what decides which changes count, its author, and its
/// statements.
#[derive(Clone, Debug)]
pub(super) struct ReadGoverning {
	fact: Fact,
	author: VerifyingKey,
	statements: Vec<Statement>,
}

/// What an admin reviews of a change that was set aside, as [`Log::review`] gives it.
#[derive(Clone, Debug)]
pub struct Review {
	/// In a space with members, the member whose replica made it, by the listings that it
	/// builds on.
	pub member: Option<EntityId>,
	/// Its payload, if the replica can read it.
	pub payload: Option<Payload>,
	/// Whether accepting it would make it count.
	pub acceptable: bool,
}

/// The ancestors of revocations, as far down as needed, so that each revocation's history is
/// walked once.
struct Ancestors {
	/// How deep the walks go: as deep as the shallowest change they are asked about.
	floor: u64,
	/// For each revocation, how deep its walk went, and the changes it found.
	walked: BTreeMap<ChangeId, (u64, BTreeSet<ChangeId>)>,
}
impl<S: Statements, H: History<Error = S::Error>> Log<S, H> {
	/// What the governing changes that count say, read from them and their statuses unless
	/// known already.
	pub(super) fn governance(
		&self,
		keys: &SpaceKeys,
	) -> Result<std::cell::Ref<'_, Governance>, LogError> {
		if self.governance.borrow().is_none() {
			let (governed, facts, statuses) = self.read_facts(keys)?;
			let counting = facts
				.iter()
				.filter(|fact| statuses.get(&fact.id()) == Some(&Status::Applied))
				.map(Fact::id)
				.collect();
			let ancestry = self.fact_ancestry(&facts)?;
			*self.governance.borrow_mut() = Some(Governance::of(
				(self.space, governed),
				&facts,
				ancestry.as_deref(),
				&counting,
				&self.following,
			));
		}
		Ok(std::cell::Ref::map(
			self.governance.borrow(),
			|governance| {
				governance
					.as_ref()
					.unwrap_or_else(|| unreachable!("read just now"))
			},
		))
	}

	/// What a space needs of this log, a group's, to follow it: the governing changes that the
	/// replica can read, in their order, and those each builds on.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn followed(&self, keys: &SpaceKeys) -> Result<Followed, LogError> {
		let (_, facts, _) = self.read_facts(keys)?;
		let ancestry = self
			.fact_ancestry(&facts)?
			.unwrap_or_else(|| vec![BTreeSet::new(); facts.len()]);
		Ok(Followed::new(self.space, facts, ancestry))
	}

	/// Follows the groups in this space, as the replica's copies of their logs say
	/// ([`Log::followed`]): their changes take effect here at the anchors that name them. The
	/// groups it followed before and leaves out are no longer followed. Decides again which
	/// changes count, if the groups' changes are not those it followed before.
	///
	/// The log keeps what it follows in memory alone: a program follows the groups again each
	/// time it opens the space, before it takes or makes changes there, and again whenever a
	/// group's log changes, as a space that follows none decides without them.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read or written.
	pub fn follow(&mut self, groups: Vec<Followed>, keys: &SpaceKeys) -> Result<(), LogError> {
		// A space never follows itself, as groups that hold each other would have it.
		let following: BTreeMap<SpaceId, Followed> = groups
			.into_iter()
			.filter(|followed| followed.group() != self.space)
			.map(|followed| (followed.group(), followed))
			.collect();
		let same = following.len() == self.following.len()
			&& following
				.iter()
				.zip(&self.following)
				.all(|((group, new), (old_group, old))| {
					group == old_group && new.same_changes(old)
				});
		if same {
			return Ok(());
		}
		// What it decided before, by the groups it followed then.
		self.governance(keys)?;
		self.following = following;
		self.govern(keys)
	}

	/// For each of `facts`, in their order, the facts it builds on, by index, if the space
	/// has members, whose first change gives a role. The history is walked once: each change
	/// below a fact is walked down to the facts it builds on through no other, and what it
	/// found is kept for the walks that reach it again.
	fn fact_ancestry(&self, facts: &[Fact]) -> Result<Option<Vec<BTreeSet<usize>>>, LogError> {
		if !facts.iter().any(Fact::founds_members) {
			return Ok(None);
		}
		let index: BTreeMap<ChangeId, usize> = facts
			.iter()
			.enumerate()
			.map(|(at, fact)| (fact.id(), at))
			.collect();
		let mut nearest: BTreeMap<ChangeId, BTreeSet<usize>> = BTreeMap::new();
		let mut ancestry: Vec<BTreeSet<usize>> = Vec::with_capacity(facts.len());
		for fact in facts {
			let mut found = BTreeSet::new();
			for parent in self.parents_of(fact.id())? {
				for earlier in self.nearest_facts(parent, &index, &mut nearest)? {
					found.insert(earlier);
					// A change's ancestors are earlier than it, so theirs are found already.
					if let Some(theirs) = ancestry.get(earlier) {
						found.extend(theirs.iter().copied());
					}
				}
			}
			ancestry.push(found);
		}
		Ok(Some(ancestry))
	}

	/// The facts, by their indexes in `index`, that a change is or builds on through no other
	/// fact, as `nearest` keeps them for the changes walked before.
	fn nearest_facts(
		&self,
		id: ChangeId,
		index: &BTreeMap<ChangeId, usize>,
		nearest: &mut BTreeMap<ChangeId, BTreeSet<usize>>,
	) -> Result<BTreeSet<usize>, LogError> {
		if let Some(&at) = index.get(&id) {
			return Ok(BTreeSet::from([at]));
		}
		let mut to_visit = vec![(id, false)];
		while let Some((at, expanded)) = to_visit.pop() {
			if nearest.contains_key(&at) {
				continue;
			}
			let parents = self.parents_of(at)?;
			if !expanded {
				to_visit.push((at, true));
				to_visit.extend(
					parents
						.iter()
						.filter(|parent| {
							!index.contains_key(parent) && !nearest.contains_key(parent)
						})
						.map(|parent| (*parent, false)),
				);
				continue;
			}
			let mut found = BTreeSet::new();
			for parent in &parents {
				match index.get(parent) {
					Some(&fact) => {
						found.insert(fact);
					}
					None => found.extend(nearest.get(parent).into_iter().flatten().copied()),
				}
			}
			nearest.insert(at, found);
		}
		Ok(nearest.get(&id).cloned().unwrap_or_default())
	}

	/// The parents of an accepted change.
	fn parents_of(&self, id: ChangeId) -> Result<Vec<ChangeId>, LogError> {
		Ok(self
			.read(self.history.change(id))?
			.map_or_default(|change| change.parents().iter().copied().collect()))
	}

	/// The governing changes, of those that `view` knows, that a change that is not one of
	/// them builds on.
	pub(super) fn known_by(
		&self,
		change: &Change,
		view: &View,
	) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut known = BTreeSet::new();
		for parent in change.parents() {
			for fact in self.frontier(*parent, view)? {
				known.insert(fact);
				if let Some(ancestors) = view.ancestors_of(&fact) {
					known.extend(ancestors.iter().copied());
				}
			}
		}
		Ok(known)
	}

	/// The latest governing changes that a change is or builds on, which no other of them
	/// builds on: found once for each change, from its parents', while the governing changes
	/// that `view` knows are the same.
	fn frontier(&self, id: ChangeId, view: &View) -> Result<Vec<ChangeId>, LogError> {
		if view.is_fact(&id) {
			return Ok(vec![id]);
		}
		{
			let mut frontiers = self.frontiers.borrow_mut();
			if !frontiers.among.iter().eq(view.facts()) {
				frontiers.among = view.facts().copied().collect();
				frontiers.latest.clear();
			}
			if let Some(latest) = frontiers.latest.get(&id) {
				return Ok(latest.clone());
			}
		}
		let mut to_visit = vec![(id, false)];
		while let Some((at, expanded)) = to_visit.pop() {
			if self.frontiers.borrow().latest.contains_key(&at) {
				continue;
			}
			let parents = self.parents_of(at)?;
			if !expanded {
				to_visit.push((at, true));
				let frontiers = self.frontiers.borrow();
				to_visit.extend(
					parents
						.iter()
						.filter(|parent| {
							!view.is_fact(parent) && !frontiers.latest.contains_key(parent)
						})
						.map(|parent| (*parent, false)),
				);
				continue;
			}
			let mut found = BTreeSet::new();
			{
				let frontiers = self.frontiers.borrow();
				for parent in &parents {
					if view.is_fact(parent) {
						found.insert(*parent);
					} else if let Some(theirs) = frontiers.latest.get(parent) {
						found.extend(theirs.iter().copied());
					}
				}
			}
			let latest = found
				.iter()
				.filter(|fact| {
					!found.iter().any(|other| {
						view.ancestors_of(other)
							.is_some_and(|ancestors| ancestors.contains(fact))
					})
				})
				.copied()
				.collect();
			self.frontiers.borrow_mut().latest.insert(at, latest);
		}
		Ok(self
			.frontiers
			.borrow()
			.latest
			.get(&id)
			.cloned()
			.unwrap_or_default())
	}

	/// Whether an ordinary change counts by what the counting governing changes say: as
	/// [`Governance::counts`] decides, and, in a space with members, as
	/// [`Log::counts_by_role`] does, from its payload too if given. `had(other)` says whether
	/// a governing change builds on it.
	pub(super) fn counts_now(
		&self,
		change: &Change,
		payload: Option<&Payload>,
		governance: &Governance,
		had: impl FnMut(ChangeId) -> bool,
	) -> Result<bool, LogError> {
		match governance.view() {
			Some(view) if governance.governed() => {
				self.counts_by_role(change, payload, (governance, view), had)
			}
			_ => Ok(governance.counts(change, had)),
		}
	}

	/// Whether an ordinary change counts in a space with members, whose `view` the
	/// governance holds: if its author's member's role at it allows what its header purges,
	/// and what its payload changes, if it can be read; and if its author is listed and not
	/// revoked, or an admin accepted it, which lifts only what a lower role, a removal or a
	/// revocation that it did not know of set aside. `had(other)` says whether a governing
	/// change builds on it.
	pub(super) fn counts_by_role(
		&self,
		change: &Change,
		payload: Option<&Payload>,
		(governance, view): (&Governance, &View),
		mut had: impl FnMut(ChangeId) -> bool,
	) -> Result<bool, LogError> {
		let author = change.author().as_bytes();
		// What an AI agent's change purges or seals is never accepted.
		if governance.is_ai_agent(author) && change.purges_or_seals() {
			return Ok(false);
		}
		let known = self.known_by(change, view)?;
		let moment = (change.timestamp(), change.purges_or_restores());
		let Some(standing) = view.standing(author, &known, moment, &mut had) else {
			return Ok(false);
		};
		let need = self.need_of(change, payload, &standing, (view, &known))?;
		let (by_ancestors, lowered) = (standing.by_ancestors, standing.lowered);
		// An AI agent's change counts only where the space allows AI agents at it, and stops
		// counting as a removal does once the admins stop allowing them.
		let (ai_by_ancestors, ai_lowered) = if governance.is_ai_agent(author) {
			view.ai_allowed(&known, |sayer| !had(sayer))
		} else {
			(true, true)
		};
		// A device's change counts only where it holds the space at it, and stops counting
		// as a removal does once a choice to drop it outranks the one it knew.
		let (holds_by_ancestors, holds_lowered) = view.holds(author, &known, |sayer| !had(sayer));
		if governance.is_accepted(change, &mut had)
			&& need <= by_ancestors
			&& ai_by_ancestors
			&& holds_by_ancestors
		{
			return Ok(true);
		}
		let listed = governance.is_listed(author) || governance.listed_in_groups().contains(author);
		Ok(listed
			&& governance.unrevoked(change, had)
			&& need <= lowered
			&& ai_lowered
			&& holds_lowered)
	}

	/// What an admin reviews of a change that was set aside, if the replica has it: the member
	/// whose replica made it, in a space with members, by the counting listings among the
	/// governing changes it builds on; its payload, if the replica can read it; and whether
	/// accepting it would make it count: an ordinary change, which no rule but those that
	/// acceptance lifts sets aside. In a space with members, it must be readable here, so that
	/// an admin reviews what it does, and the role that its author's member had by the
	/// governing changes it builds on must allow it, as an AI agent's must have been allowed
	/// there: acceptance lifts only what a lowered role, a removal or a revocation set aside.
	/// What an AI agent's change purges or seals is never accepted, nor what writes a value
	/// plainly that its author knew to be secret.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn review(&self, id: ChangeId, keys: &SpaceKeys) -> Result<Option<Review>, LogError> {
		let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Ok(None);
		};
		let change = &accepted.change;
		let payload = read_payload(&accepted, keys).ok().flatten();
		let governance = self.governance(keys)?;
		let author = change.author().as_bytes();
		let known = match governance.view() {
			Some(view) => Some((view, self.known_by(change, view)?)),
			None => None,
		};
		let standing = known.as_ref().and_then(|(view, known)| {
			let moment = (change.timestamp(), change.purges_or_restores());
			view.standing(author, known, moment, |_| true)
		});
		let member = standing.as_ref().map(|standing| standing.member);
		let ai_agent = governance.is_ai_agent(author);
		let acceptable = if change.kind() == Kind::Governing {
			false
		} else if !governance.governed() {
			true
		} else if ai_agent && change.purges_or_seals() {
			false
		} else if let Some(payload) = &payload
			&& ((ai_agent && !fits_an_ai_agent(payload))
				|| self.writes_secret_plainly(change, payload, &governance)?)
		{
			false
		} else {
			match (&known, &standing, &payload) {
				(None, ..) => true,
				(Some((view, known)), Some(standing), Some(payload)) => {
					let need = self.need_of(change, Some(payload), standing, (view, known))?;
					let by_ancestors = standing.by_ancestors;
					let (allowed, _) = if ai_agent {
						view.ai_allowed(known, |_| false)
					} else {
						(true, true)
					};
					let (holds, _) = view.holds(author, known, |_| false);
					need <= by_ancestors && allowed && holds
				}
				_ => false,
			}
		};
		Ok(Some(Review {
			member,
			payload,
			acceptable,
		}))
	}

	/// Whether every one of `others` is `change` or one of its ancestors: the history is
	/// walked once, down to the shallowest of them.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn builds_on_all(&self, change: ChangeId, others: &[ChangeId]) -> Result<bool, LogError> {
		let left: BTreeSet<ChangeId> = others
			.iter()
			.copied()
			.filter(|other| *other != change)
			.collect();
		let mut floor = u64::MAX;
		for &other in &left {
			match self.read(self.history.depth(other))? {
				Some(depth) => floor = floor.min(depth),
				None => return Ok(false),
			}
		}
		if left.is_empty() {
			return Ok(true);
		}
		let found = self.ancestors_down_to(change, floor)?;
		Ok(left.iter().all(|other| found.contains(other)))
	}

	/// In a space with members, the counting governing changes that end a key's replica there,
	/// if they do: the latest that give its member the role none, by which they left or were
	/// removed, while that is the member's role now; and, for an AI agent's key, the latest
	/// that say the space allows no AI agents, while it allows none. Ordered by ID; empty
	/// elsewhere.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn removals_of(
		&self,
		key: &VerifyingKey,
		keys: &SpaceKeys,
	) -> Result<Vec<ChangeId>, LogError> {
		let governance = self.governance(keys)?;
		let Some(view) = governance.view().filter(|_| governance.governed()) else {
			return Ok(Vec::new());
		};
		let mut removals = BTreeSet::new();
		// A member removed from the space whom a group still gives a role is not ended.
		if let Some(member) = view.owner_now(key.as_bytes())
			&& view.role_now(member) == Some(Role::None)
			&& !view.has_role_now(key.as_bytes())
		{
			removals.extend(view.removals_of_member(member));
		}
		if governance.is_ai_agent(key.as_bytes()) && !view.allows_ai_agents() {
			removals.extend(view.disallowing_now());
		}
		// A key that only groups list, whose paths through them have all ended.
		removals.extend(view.group_removals_of(key.as_bytes()));
		// A device whose standing choice drops the space.
		removals.extend(view.drops_of(key.as_bytes()));
		Ok(removals.into_iter().collect())
	}

	/// Whether a key's replica is ended in a space with members, as
	/// [`Log::removals_of`] says: its member left or was removed, or it is an AI agent's and
	/// the space allows none. It changes nothing there after, and no replica gives it keys.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_removed(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(!self.removals_of(key, keys)?.is_empty())
	}

	/// In a space with members, every counting governing change that ends replicas there:
	/// that gives a member the role none, and, where a counting listing is an AI agent's, that
	/// says the space allows no AI agents. A key is written with only if its making builds on
	/// each ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)). Ordered by
	/// ID; empty elsewhere.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn removals(&self, keys: &SpaceKeys) -> Result<Vec<ChangeId>, LogError> {
		let governance = self.governance(keys)?;
		let Some(view) = governance.view().filter(|_| governance.governed()) else {
			return Ok(Vec::new());
		};
		let mut removals: BTreeSet<ChangeId> = view.all_removals().collect();
		if governance.lists_ai_agents() {
			removals.extend(view.disallowing());
		}
		removals.extend(view.all_drops());
		// What ends paths through a group takes effect at its first anchors, and keeping
		// out a person whom nothing else gives a role at once.
		removals.extend(view.departure_anchors());
		for key in governance.listed_in_groups() {
			removals.extend(view.keep_outs_ending(key));
		}
		Ok(removals.into_iter().collect())
	}

	/// The groups that the space's governing changes add, each with its record there, the
	/// group and its first change, the statement that adds it, its role now, and whether it
	/// is in the space: whether, by every group change that the counting anchors name, the
	/// group agrees to that addition and has not withdrawn. Empty in a space without members.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn groups(&self, keys: &SpaceKeys) -> Result<Vec<SpaceGroup>, LogError> {
		let governance = self.governance(keys)?;
		let Some(view) = governance.view() else {
			return Ok(Vec::new());
		};
		let Some(reach) = view.reach() else {
			return Ok(Vec::new());
		};
		let knowledge = reach.knowledge(reach.counting(), None);
		let agreed: BTreeSet<EntityId> = reach
			.groups_in(&knowledge, |_| true)
			.into_iter()
			.map(|(_, record, _)| record)
			.collect();
		Ok(reach
			.group_records()
			.into_iter()
			.map(|(record, group, addition)| SpaceGroup {
				record,
				group,
				addition,
				role: view.role_now(record),
				agreed: agreed.contains(&record),
			})
			.collect())
	}

	/// The keys that the groups in the space list now, by every group change that its
	/// counting anchors name, but AI agents' and revoked ones: replicas that count as listed
	/// there, which a replica syncs with and gives keys to.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn listed_through_groups(&self, keys: &SpaceKeys) -> Result<Vec<VerifyingKey>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.map_or_default(View::listed_through_groups_now)
			.iter()
			.filter_map(VerifyingKey::from_bytes)
			.collect())
	}

	/// The claims to the admin role that count in a space with members, in their order, with
	/// when another member first saw each, when each takes effect and is settled, and whether
	/// a rejection voids it. Empty elsewhere.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn claims(&self, keys: &SpaceKeys) -> Result<Vec<Claim>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance.view().map_or_default(|view| {
			view.claims()
				.iter()
				.map(|effect| Claim {
					claim: effect.claim,
					claimant: effect.claimant,
					made: effect.made,
					seen: effect.seen(),
					effective: effect.effective(),
					settled: effect.settled(),
					rejected: effect.void,
				})
				.collect()
		}))
	}

	/// The role that a key's changes have in a space with members now, as its counting
	/// governing changes give it: through its listing's member, directly or through groups;
	/// [`Role::None`] for a key that no counting listing names, or whose member left or was
	/// removed. `None` for a space without members. Neither a claim, which takes effect by
	/// the wall clock, nor a revocation is taken into account.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn key_role(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<Option<Role>, LogError> {
		let governance = self.governance(keys)?;
		if !governance.members() {
			return Ok(None);
		}
		Ok(Some(governance.view().map_or(Role::None, |view| {
			view.key_role_now(key.as_bytes())
		})))
	}

	/// For a key that a group the space follows lists, at any level, the group and the key's
	/// member there, by which history names the key's changes: the first path's that gives it
	/// a role now, by every group change that the counting anchors name, or else the first
	/// group's by ID that lists it. `None` for any other key.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn group_member_of(
		&self,
		key: &VerifyingKey,
		keys: &SpaceKeys,
	) -> Result<Option<(SpaceId, EntityId)>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.and_then(|view| view.group_member_of(key.as_bytes())))
	}

	/// The members of the groups that the space follows, at any level, whose keys those
	/// groups give a role in the space now, each with the highest role that one of their keys
	/// has now; empty for a space that follows no group.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn group_member_roles(
		&self,
		keys: &SpaceKeys,
	) -> Result<BTreeMap<EntityId, Role>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.map_or_default(View::group_member_roles_now))
	}

	/// Each group that the space follows, at any level, that lists a key now, with the key's
	/// member there, but for AI agents' keys.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn group_members_of(
		&self,
		key: &VerifyingKey,
		keys: &SpaceKeys,
	) -> Result<Vec<(SpaceId, EntityId)>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.map_or_default(|view| view.group_members_of(key.as_bytes())))
	}

	/// Each group's changes that the space's counting anchors name, with every one they
	/// build on: a replica that changed a group, or holds a change of it that none of these
	/// holds, anchors it.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn anchored(
		&self,
		keys: &SpaceKeys,
	) -> Result<BTreeMap<SpaceId, BTreeSet<ChangeId>>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.and_then(View::reach)
			.map_or_default(Reach::anchored))
	}

	/// Whether a change's author was, by the governing changes it builds on, a replica that a
	/// counting change listed and none revoked, and in a space with members a replica of a
	/// member whose role there was not none: as a change that counts was. A key made by such a
	/// replica is given out, whatever later sets its making aside, so that what was written
	/// with it stays readable ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).
	/// An AI agent's replica makes no key.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn author_was_a_member(&self, id: ChangeId, keys: &SpaceKeys) -> Result<bool, LogError> {
		let Some(change) = self.read(self.history.change(id))?.map(Cow::into_owned) else {
			return Ok(false);
		};
		let governance = self.governance(keys)?;
		let author = change.author().as_bytes();
		// The governing changes it builds on, found once, where they are needed.
		let mut known = None;
		// A device that dropped the space, by the change itself or what it builds on, makes
		// nothing given out there: a key it made as it dropped the space is never safe.
		if let Some(view) = governance.view().filter(|_| governance.governed())
			&& view.standing_choice(author).is_some()
		{
			let mut with_itself = self.known_by(&change, view)?;
			with_itself.insert(id);
			if !view.holds(author, &with_itself, |_| false).0 {
				return Ok(false);
			}
			with_itself.remove(&id);
			known = Some(with_itself);
		}
		if self.read(self.history.status(id))? == Some(Status::Applied) {
			return Ok(true);
		}
		if !governance.governed() {
			return Ok(true);
		}
		if governance.is_ai_agent(author) {
			return Ok(false);
		}
		for &revocation in governance.revocations_of(author).into_iter().flatten() {
			if self.had(revocation, id)? {
				return Ok(false);
			}
		}
		if let Some(view) = governance.view() {
			let known = match known {
				Some(known) => known,
				None => self.known_by(&change, view)?,
			};
			let moment = (change.timestamp(), change.purges_or_restores());
			return Ok(view
				.standing(author, &known, moment, |_| true)
				.is_some_and(|standing| standing.by_ancestors != Role::None));
		}
		for &listing in governance.listings_of(author).into_iter().flatten() {
			// A change that lists its own author counts only as the space's first.
			if listing != id && self.had(listing, id)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// What an ordinary change by a replica of `member` needs of the member's role: an
	/// editor's to purge the changes, and the statements sealed in changes, that the
	/// member's replicas signed, and an admin's to purge anything else and content; an
	/// editor's to change any entity, a viewer's for the member's own records, and an
	/// admin's for those of other members, their replicas and their devices, as each
	/// operation names its entity, and for one that names none, of an unknown kind or of the
	/// earlier form; and an editor's for secret statements, whose entities only those who can
	/// open them see, and for retractions of secret statements, which take away nothing
	/// else.
	fn need_of(
		&self,
		change: &Change,
		payload: Option<&Payload>,
		standing: &Standing,
		(view, known): (&View, &BTreeSet<ChangeId>),
	) -> Result<Role, LogError> {
		let mut need = Role::Viewer;
		// Who may purge something may restore it, and nobody else.
		let mut signers = Vec::new();
		for named in [change.purges(), change.restores()] {
			if !named.blobs.is_empty() {
				return Ok(Role::Admin);
			}
			for &purged in &named.changes {
				signers.push(Some(purged));
			}
			for &statement in &named.statements {
				signers.push(self.read(self.history.holder(statement))?);
			}
		}
		for signer in signers {
			let signer = match signer {
				Some(signer) => self.read(self.history.change(signer))?.map(Cow::into_owned),
				None => None,
			};
			let Some(signer) = signer else {
				return Ok(Role::Admin);
			};
			let theirs = self.known_by(&signer, view)?;
			let moment = (signer.timestamp(), signer.purges_or_restores());
			let signers = view.standing(signer.author().as_bytes(), &theirs, moment, |_| true);
			if !signers.is_some_and(|signers| signers.is_same_person(standing)) {
				return Ok(Role::Admin);
			}
			need = need.max(Role::Editor);
		}
		let Some(payload) = payload else {
			return Ok(need);
		};
		let records = &standing.records;
		let restated = payload.restated.values();
		let operations = payload
			.operations
			.iter()
			.chain(restated.clone().flat_map(|restated| &restated.operations));
		for operation in operations {
			// What takes away secret statements names no entity, as theirs are sealed with
			// them: an editor's, as for a secret statement itself.
			if let Operation::RetractSecret { .. } = operation {
				need = need.max(Role::Editor);
				continue;
			}
			// What an operation of an unknown kind, or of the earlier form, changes is not
			// told by an entity it names.
			if operation.supersedes_any_entity() {
				return Ok(Role::Admin);
			}
			match operation.entity() {
				Some(entity) if records.contains(&entity) => {}
				Some(entity) if !view.is_any_record(&entity, known, &standing.knowledge) => {
					need = need.max(Role::Editor);
				}
				_ => return Ok(Role::Admin),
			}
		}
		if !payload.sealed.is_empty()
			|| restated
				.into_iter()
				.any(|restated| !restated.sealed.is_empty())
		{
			need = need.max(Role::Editor);
		}
		Ok(need)
	}

	/// Whether the space's first change governs it, the governing changes that the replica
	/// can read and that have the form a governing change must, in their order, and their
	/// statuses. What a change says is read once, and kept.
	fn read_facts(&self, keys: &SpaceKeys) -> Result<Facts, LogError> {
		let mut governed = false;
		let mut facts = Vec::new();
		let mut statuses = BTreeMap::new();
		for id in self.read(self.history.governing())? {
			let Some((status, read)) = self.governing_change(id, keys, &mut governed)? else {
				continue;
			};
			facts.push(read.fact);
			statuses.insert(id, status);
		}
		facts.sort_by_key(Fact::order);
		// In a space whose first change governs it, as every space of NibKeep, only the
		// listings that the listed keys signed count for anything.
		if governed {
			for fact in &mut facts {
				fact.keep_signed();
			}
		}
		Ok((governed, facts, statuses))
	}

	/// A governing change's status and what it says, if the replica can read it and it has
	/// the form it must; `governed` notes whether it is a change without parents. A change
	/// read before is not read again, but for its status.
	fn governing_change(
		&self,
		id: ChangeId,
		keys: &SpaceKeys,
		governed: &mut bool,
	) -> Result<Option<(Status, ReadGoverning)>, LogError> {
		let Some(status) = self.read(self.history.status(id))? else {
			return Ok(None);
		};
		let cached = self.facts.borrow().get(&id).cloned();
		let read = match cached {
			Some(Some(read)) => {
				*governed |= read.fact.is_root();
				Some(read)
			}
			Some(None) => {
				*governed |= self
					.read(self.history.change(id))?
					.is_some_and(|change| change.parents().is_empty());
				None
			}
			None => {
				let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
					return Ok(None);
				};
				*governed |= accepted.change.parents().is_empty();
				self.read_governing(&accepted, keys)
			}
		};
		if status == Status::Ignored {
			return Ok(None);
		}
		Ok(read.map(|read| (status, read)))
	}

	/// What a governing change says, if the replica can read it and it has the form a
	/// governing change must: statements of fields that govern and retractions alone, and
	/// nothing stated again. Whatever order changes arrive in, every replica reads the
	/// same of it.
	fn read_governing(&self, accepted: &Accepted, keys: &SpaceKeys) -> Option<ReadGoverning> {
		let id = accepted.change.id();
		if let Some(read) = self.facts.borrow().get(&id) {
			return read.clone();
		}
		let Ok(Some(payload)) = read_payload(accepted, keys) else {
			// Not readable yet: it is read again when it may be.
			return None;
		};
		let read =
			(holds_only_governing(&payload.operations) && payload.restated.is_empty()).then(|| {
				ReadGoverning {
					fact: Fact::read(&accepted.change, &payload.operations),
					author: *accepted.change.author(),
					statements: payload
						.operations
						.iter()
						.filter_map(|operation| match operation {
							Operation::Assert { statement, .. } => Some(statement.clone()),
							_ => None,
						})
						.collect(),
				}
			});
		self.facts.borrow_mut().insert(id, read.clone());
		read
	}

	/// The statements of the governing changes that the replica can read, in the order of
	/// their changes, by timestamp and then by ID, each with its change, whether that counts,
	/// and its author: what says who the replicas are. Retractions are left out, as
	/// revoking is the only way out.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn governing_statements(
		&self,
		keys: &SpaceKeys,
	) -> Result<Vec<GoverningStatement>, LogError> {
		let signed = self.governance(keys)?.governed();
		let mut statements = Vec::new();
		let mut governed = false;
		for id in self.read(self.history.governing())? {
			let Some((status, read)) = self.governing_change(id, keys, &mut governed)? else {
				continue;
			};
			let counts = status == Status::Applied;
			let (author, timestamp) = (read.author, read.fact.order().0);
			let fact = &read.fact;
			statements.extend(read.statements.iter().map(|statement| {
				let lists = statement.field == fields::SIGNING_KEY
					&& statement
						.value
						.as_text()
						.and_then(VerifyingKey::from_text)
						.is_some_and(|key| fact.lists_on(statement.entity, key.as_bytes(), signed));
				GoverningStatement {
					change: id,
					author,
					timestamp,
					counts,
					statement: statement.clone(),
					lists,
				}
			}));
		}
		statements.sort_by_key(|statement| (statement.timestamp, statement.change));
		Ok(statements)
	}

	/// The listings of `key` by a recovery code, in the counting governing changes that list
	/// it and that it made itself, as a device that recovered with a code lists itself: what
	/// it shows a peer that does not know it yet
	/// ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#3-syncing)).
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn listings_by_code(
		&self,
		key: &VerifyingKey,
		keys: &SpaceKeys,
	) -> Result<Vec<RecoveryListing>, LogError> {
		let governance = self.governance(keys)?;
		let mut listings = Vec::new();
		for &id in governance.listings_of(key.as_bytes()).into_iter().flatten() {
			let mut governed = false;
			let Some((_, read)) = self.governing_change(id, keys, &mut governed)? else {
				continue;
			};
			if read.author != *key {
				continue;
			}
			listings.extend(read.statements.iter().filter_map(|statement| {
				(statement.field == fields::RECOVERY_LISTING)
					.then(|| statement.value.as_text())
					.flatten()
					.and_then(RecoveryListing::from_text)
					.filter(|listing| {
						listing.key == *key
							&& listing.space == self.space
							&& listing.record == statement.entity
					})
			}));
		}
		Ok(listings)
	}

	/// Who the replicas of the space are, as its governing changes that count say.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn roster(&self, keys: &SpaceKeys) -> Result<Roster, LogError> {
		Ok(Roster::new(self.space, &self.governing_statements(keys)?))
	}

	/// Decides again which changes count, after a governing change could be read, and takes
	/// in or sets aside every change whose decision changed. A change that counted and is set
	/// aside leaves the state stale; what it purged is no longer purged, unless a change that
	/// counts purges it too.
	pub(super) fn govern(&mut self, keys: &SpaceKeys) -> Result<(), LogError> {
		let before = self.governance(keys)?.clone();
		let (governed, facts, statuses) = self.read_facts(keys)?;
		let ancestry = self.fact_ancestry(&facts)?;
		let counting = if governed {
			decide(
				&facts,
				ancestry.as_deref(),
				(self.space, Some(&self.following)),
				|ancestor, of| self.had(ancestor, of),
			)?
		} else {
			facts.iter().map(Fact::id).collect()
		};
		let after = Governance::of(
			(self.space, governed),
			&facts,
			ancestry.as_deref(),
			&counting,
			&self.following,
		);
		*self.governance.borrow_mut() = Some(after.clone());
		let unchanged = before == after;
		for fact in &facts {
			match (counting.contains(&fact.id()), statuses[&fact.id()]) {
				(true, Status::Pending | Status::SetAside) => {
					self.set_status(fact.id(), Status::Pending)?;
					self.complete_now(fact.id(), keys)?;
				}
				(false, Status::Applied) => {
					self.set_status(fact.id(), Status::SetAside)?;
					self.stale = true;
				}
				(false, Status::Pending) => self.set_status(fact.id(), Status::SetAside)?,
				_ => {}
			}
		}
		// Whether an ordinary change counts depends on what the counting governing changes
		// say alone.
		if !unchanged {
			self.decide_again(&before, &after, keys)?;
		}
		if self.stale
			&& let Some(fresh) = self.state.storage().ordinary().empty_like()
		{
			self.rebuild(fresh, keys)?;
		}
		Ok(())
	}

	/// Decides again which ordinary changes count, of those that `after` may decide
	/// otherwise than `before`: those set aside, those accepted or no longer, and those of the
	/// keys that may count less, or of every key when the space came to be governed.
	fn decide_again(
		&mut self,
		before: &Governance,
		after: &Governance,
		keys: &SpaceKeys,
	) -> Result<(), LogError> {
		let mut candidates: BTreeSet<ChangeId> =
			self.read(self.history.set_aside())?.into_iter().collect();
		candidates.extend(before.accepted().symmetric_difference(&after.accepted()));
		let losing = after.losing(before);
		// In a space that follows groups, what they give, or a role, may change the standing
		// of any key, of a member through a group or of one joined to them: every change is
		// decided again.
		let roles_changed = match (before.view(), after.view()) {
			(Some(before), Some(after)) => after.roles_differ(before),
			_ => false,
		};
		let groups_changed = match (before.reach(), after.reach()) {
			(None, None) => false,
			(Some(earlier), Some(later)) => {
				!losing.is_empty() || roles_changed || !earlier.gives_same(later)
			}
			_ => true,
		};
		let newly_governed = before.governed() != after.governed() || groups_changed;
		// In a space with members, a purge's signer's member is found through the signer's
		// listing: once one stops counting, every purge is decided again.
		let purges_again = after
			.view()
			.is_some_and(|view| view.lost_listings(before.view().unwrap_or(&View::default())));
		// A change that makes a field secret for good, which counts now: the ordinary changes
		// that it does not build on may have written the field in plain since, or at the same
		// time, and then no longer count. What it builds on stays as it was.
		let mut built_on: Vec<BTreeSet<ChangeId>> = Vec::new();
		for (field, marks) in after.secret_fields() {
			for &mark in marks {
				if !before
					.made_secret(field)
					.is_some_and(|had| had.contains(&mark))
				{
					built_on.push(self.ancestors_of(mark)?);
				}
			}
		}
		if newly_governed || !losing.is_empty() || !built_on.is_empty() || purges_again {
			for id in self.read(self.history.ids())? {
				let Some(change) = self.read(self.history.change(id))? else {
					continue;
				};
				let unseen = change.kind() == Kind::Ordinary
					&& built_on.iter().any(|ancestors| !ancestors.contains(&id));
				if newly_governed
					|| losing.contains(change.author().as_bytes())
					|| (purges_again && change.purges_or_restores())
					|| unseen
				{
					candidates.insert(id);
				}
			}
		}
		// Each revocation's history is walked once, down to the shallowest candidate.
		let mut floor = u64::MAX;
		for &id in &candidates {
			if let Some(depth) = self.read(self.history.depth(id))? {
				floor = floor.min(depth);
			}
		}
		let mut ancestors = Ancestors {
			floor,
			walked: BTreeMap::new(),
		};
		let mut recount = false;
		let read_payloads = !built_on.is_empty();
		for id in candidates {
			recount |= self.decide_change(id, (after, read_payloads), &mut ancestors, keys)?;
		}
		// Changes that come to count, or no longer, as what restores comes to count too, are
		// counted again until none does.
		while recount {
			recount = false;
			for id in self.recount_purges(keys)? {
				recount |= self.decide_change(id, (after, read_payloads), &mut ancestors, keys)?;
			}
		}
		Ok(())
	}

	/// Decides again whether an accepted ordinary change counts, and takes it in or sets it
	/// aside; from what its payload writes too if it was set aside, or, with
	/// `read_payloads`, as a field became secret for good. Returns whether it was set aside after its header was acted on, so
	/// that what it purged is to be counted again.
	fn decide_change(
		&mut self,
		id: ChangeId,
		(governance, read_payloads): (&Governance, bool),
		ancestors: &mut Ancestors,
		keys: &SpaceKeys,
	) -> Result<bool, LogError> {
		let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Ok(false);
		};
		if accepted.change.kind() == Kind::Governing {
			return Ok(false);
		}
		let depth = accepted.depth;
		// In a space with members, what its author's role allows of what it writes, if it can
		// be read.
		let payload = if governance.view().is_some() {
			read_payload(&accepted, keys).ok().flatten()
		} else {
			None
		};
		let mut failed = None;
		let mut counts = self.counts_now(
			&accepted.change,
			payload.as_ref(),
			governance,
			|other| match self.descends(other, id, depth, ancestors) {
				Ok(had) => had,
				Err(error) => {
					failed = Some(error);
					false
				}
			},
		)?;
		if let Some(error) = failed {
			return Err(error);
		}
		// An AI agent's change that writes what an AI agent may not is set aside, once its
		// payload can be read, and so is one that writes plainly what its author knew to be
		// secret; one that cannot be read yet is decided when it is completed, as is one set
		// aside that comes to count.
		let ai_agent = governance.is_ai_agent(accepted.change.author().as_bytes());
		let payload = match payload {
			Some(payload) => Some(payload),
			None if counts
				&& (ai_agent || read_payloads || accepted.status == Status::SetAside) =>
			{
				read_payload(&accepted, keys).ok().flatten()
			}
			None => None,
		};
		if counts
			&& (ai_agent || read_payloads || accepted.status == Status::SetAside)
			&& let Some(payload) = payload
		{
			counts = (!ai_agent || fits_an_ai_agent(&payload))
				&& !self.writes_secret_plainly(&accepted.change, &payload, governance)?;
		}
		match (counts, accepted.status) {
			(true, Status::SetAside) => {
				let status = if self.read(self.history.is_change_purged(id))? {
					Status::Ignored
				} else {
					Status::Pending
				};
				self.set_status(id, status)?;
				let recount = self.take_in_header(&accepted.change, false)?;
				self.hold_sealed(&accepted.change)?;
				self.complete(id, keys)?;
				Ok(recount)
			}
			(false, Status::SetAside) | (true, _) => Ok(false),
			(false, status) => {
				self.set_status(id, Status::SetAside)?;
				if status == Status::Applied {
					self.stale = true;
				}
				let header = &accepted.change;
				Ok(header.purges_or_seals())
			}
		}
	}

	/// Whether `change`, at `depth`, is `revocation` or one of its ancestors.
	fn descends(
		&self,
		revocation: ChangeId,
		change: ChangeId,
		depth: u64,
		ancestors: &mut Ancestors,
	) -> Result<bool, LogError> {
		if revocation == change {
			return Ok(true);
		}
		if let Some((walked, found)) = ancestors.walked.get(&revocation)
			&& *walked <= depth
		{
			return Ok(found.contains(&change));
		}
		let depth = depth.min(ancestors.floor);
		let found = self.ancestors_down_to(revocation, depth)?;
		let had = found.contains(&change);
		ancestors.walked.insert(revocation, (depth, found));
		Ok(had)
	}

	/// The ancestors of a change that are at least `depth` deep.
	pub(super) fn ancestors_down_to(
		&self,
		of: ChangeId,
		depth: u64,
	) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut found = BTreeSet::new();
		let mut to_visit: Vec<ChangeId> = self
			.read(self.history.change(of))?
			.map_or_default(|change| change.parents().iter().copied().collect());
		while let Some(id) = to_visit.pop() {
			if found.contains(&id) {
				continue;
			}
			let Some(visiting) = self.read(self.history.depth(id))? else {
				continue;
			};
			if visiting < depth {
				continue;
			}
			found.insert(id);
			if let Some(change) = self.read(self.history.change(id))? {
				to_visit.extend(change.parents().iter().copied());
			}
		}
		Ok(found)
	}

	/// The changes `of`, and every accepted change that one of them builds on.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn with_ancestors(&self, of: &[ChangeId]) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut found: BTreeSet<ChangeId> = BTreeSet::new();
		for &change in of {
			if found.contains(&change) || self.read(self.history.change(change))?.is_none() {
				continue;
			}
			found.insert(change);
			found.extend(self.ancestors_down_to(change, 0)?);
		}
		Ok(found)
	}

	/// Whether `ancestor` is `of` or one of its ancestors.
	pub(super) fn had(&self, ancestor: ChangeId, of: ChangeId) -> Result<bool, LogError> {
		if ancestor == of {
			return Ok(true);
		}
		match self.read(self.history.change(of))? {
			Some(change) => self.is_ancestor(ancestor, &change),
			None => Ok(false),
		}
	}

	/// Counts again what purges that count destroyed, after a change whose header was acted
	/// on was set aside as its payload was read, or one that restores came to count, and
	/// decides again the changes that this brings back, until none comes to count or stops.
	pub(super) fn recount_purges_and_decide(&mut self, keys: &SpaceKeys) -> Result<(), LogError> {
		let governance = self.governance(keys)?.clone();
		let mut ancestors = Ancestors {
			floor: u64::MAX,
			walked: BTreeMap::new(),
		};
		let mut recount = true;
		while recount {
			recount = false;
			for id in self.recount_purges(keys)? {
				recount |= self.decide_change(id, (&governance, false), &mut ancestors, keys)?;
			}
		}
		Ok(())
	}

	/// Which purge keeps each purged item purged, of the changes, secret statements and sealed
	/// blobs that counting purges destroyed: the first that purges it and that no counting
	/// change that restores it builds on. A copy that waits counts its wait by it
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait)).
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn purged_by(&self) -> Result<PurgedBy, LogError> {
		// A space that purged nothing needs no look through its history.
		if self.read(self.history.purged_changes())?.is_empty()
			&& self.read(self.history.purged_statements())?.is_empty()
			&& self.read(self.history.purged_blobs())?.is_empty()
		{
			return Ok(PurgedBy::default());
		}
		let mut purging: Vec<(ChangeId, Purges)> = Vec::new();
		let mut restoring: Vec<(ChangeId, Restores)> = Vec::new();
		for id in self.read(self.history.ids())? {
			let (Some(change), Some(status)) = (
				self.read(self.history.change(id))?.map(Cow::into_owned),
				self.read(self.history.status(id))?,
			) else {
				continue;
			};
			if status == Status::SetAside {
				continue;
			}
			if !change.purges().is_empty() {
				purging.push((id, change.purges().clone()));
			}
			if !change.restores().is_empty() {
				restoring.push((id, change.restores().clone()));
			}
		}
		let (changes, statements, blobs) = self.still_purged(&purging, &restoring)?;
		Ok(PurgedBy {
			changes,
			statements,
			blobs,
		})
	}

	/// Counts again what the changes that count purge, after one that purged was set aside,
	/// or one that restores came to count or stopped: what a counting change purges stays
	/// purged unless a counting change that builds on it restores it, what none of them
	/// purges so is no longer purged, and which change holds each sealed statement is
	/// decided again among those that count. What comes to be purged again, as after a
	/// change that restored it was set aside, is destroyed again. Returns the changes that
	/// are no longer purged, which may count or not.
	fn recount_purges(&mut self, keys: &SpaceKeys) -> Result<Vec<ChangeId>, LogError> {
		let mut purging: Vec<(ChangeId, Purges)> = Vec::new();
		let mut all_restoring = BTreeSet::new();
		let mut restoring: Vec<(ChangeId, Restores)> = Vec::new();
		let mut sealing: Vec<(ChangeId, bool, Timestamp, Vec<StatementId>)> = Vec::new();
		for id in self.read(self.history.ids())? {
			// The header and the status are enough: the payload is not read.
			let (Some(change), Some(status)) = (
				self.read(self.history.change(id))?.map(Cow::into_owned),
				self.read(self.history.status(id))?,
			) else {
				continue;
			};
			let set_aside = status == Status::SetAside;
			if !change.restores().is_empty() {
				all_restoring.insert(id);
			}
			if !change.sealed().is_empty() {
				sealing.push((
					id,
					set_aside,
					change.timestamp(),
					change.sealed().keys().copied().collect(),
				));
			}
			if set_aside {
				continue;
			}
			if !change.purges().is_empty() {
				purging.push((id, change.purges().clone()));
			}
			if !change.restores().is_empty() {
				restoring.push((id, change.restores().clone()));
			}
		}
		*self.restoring.get_mut() = Some(all_restoring);
		let (changes, statements, blobs) = self.still_purged(&purging, &restoring)?;
		let mut unpurged = Vec::new();
		for id in self.read(self.history.purged_changes())? {
			if !changes.contains_key(&id) {
				let forgot = self.history.unpurge_change(id);
				self.read(forgot)?;
				unpurged.push(id);
				self.destroyed.restored.changes.insert(id);
			}
		}
		for statement in self.read(self.history.purged_statements())? {
			if !statements.contains_key(&statement) {
				let forgot = self.history.unpurge_statement(statement);
				self.read(forgot)?;
				self.destroyed.restored.statements.insert(statement);
			}
		}
		for sealed in self.read(self.history.purged_blobs())? {
			if !blobs.contains_key(&sealed) {
				let forgot = self.history.unpurge_blob(sealed);
				self.read(forgot)?;
				self.destroyed.restored.blobs.insert(sealed);
			}
		}
		// What is purged again, as a change that restored it no longer counts, is destroyed
		// again.
		for (&change, &by) in &changes {
			if !self.read(self.history.is_change_purged(change))? {
				self.destroy_change(change, by)?;
				self.destroyed.restored.changes.remove(&change);
			}
		}
		for (&statement, &by) in &statements {
			if !self.read(self.history.is_statement_purged(statement))? {
				self.destroy_statement(statement, by)?;
				self.destroyed.restored.statements.remove(&statement);
			}
		}
		for (&sealed, &by) in &blobs {
			if !self.read(self.history.is_blob_purged(&sealed))? {
				self.destroy_blob_key(sealed, by)?;
				self.destroyed.restored.blobs.remove(&sealed);
			}
		}
		// Of the changes that seal a statement, whether they count or not, one that counts and
		// was not purged comes first, then the earliest, then the smallest, as when they
		// arrive.
		let mut holders: BTreeMap<StatementId, (bool, Timestamp, ChangeId)> = BTreeMap::new();
		for (id, set_aside, timestamp, sealed) in sealing {
			let order = (set_aside || changes.contains_key(&id), timestamp, id);
			for statement in sealed {
				let best = holders.entry(statement).or_insert(order);
				if order < *best {
					*best = order;
				}
			}
		}
		for (statement, (_, _, holder)) in holders {
			if self.read(self.history.holder(statement))? != Some(holder) {
				let held = self.history.set_holder(statement, holder);
				self.read(held)?;
				self.state.storage_mut().forget(statement);
				self.open(statement, keys, &mut BTreeMap::new())?;
			}
		}
		Ok(unpurged)
	}

	/// What the counting changes that purge, `purging`, keep purged, each item by the first
	/// purge that purges it so: all they name but what a counting change that restores it,
	/// of `restoring`, restores after the purge, building on it.
	#[expect(
		clippy::type_complexity,
		reason = "three maps, as the history keeps them"
	)]
	fn still_purged(
		&self,
		purging: &[(ChangeId, Purges)],
		restoring: &[(ChangeId, Restores)],
	) -> Result<
		(
			BTreeMap<ChangeId, ChangeId>,
			BTreeMap<StatementId, ChangeId>,
			BTreeMap<Hash, ChangeId>,
		),
		LogError,
	> {
		let mut changes: BTreeMap<ChangeId, ChangeId> = BTreeMap::new();
		let mut statements: BTreeMap<StatementId, ChangeId> = BTreeMap::new();
		let mut blobs: BTreeMap<Hash, ChangeId> = BTreeMap::new();
		// Whether a counting change that builds on the purge restores the item.
		for (purge, purges) in purging {
			for &change in &purges.changes {
				if !changes.contains_key(&change)
					&& !self.restored_after(*purge, restoring, &|named| {
						named.changes.contains(&change)
					})? {
					changes.insert(change, *purge);
				}
			}
			for &statement in &purges.statements {
				if !statements.contains_key(&statement)
					&& !self.restored_after(*purge, restoring, &|named| {
						named.statements.contains(&statement)
					})? {
					statements.insert(statement, *purge);
				}
			}
			for &sealed in &purges.blobs {
				if !blobs.contains_key(&sealed)
					&& !self
						.restored_after(*purge, restoring, &|named| named.blobs.contains(&sealed))?
				{
					blobs.insert(sealed, *purge);
				}
			}
		}
		Ok((changes, statements, blobs))
	}

	/// Changes the status of an accepted change, keeping what the log has of it.
	pub(super) fn set_status(&mut self, id: ChangeId, status: Status) -> Result<(), LogError> {
		let Some(mut accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Ok(());
		};
		if accepted.status == status {
			return Ok(());
		}
		accepted.status = status;
		self.store(&accepted)
	}
}
