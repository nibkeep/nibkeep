//! Groups in a space with members ([technical design: groups]): the space follows the governing
//! changes of each group in it, which take effect there at its *anchors*, its own governing
//! changes that name them.
//!
//! A change of the space *knows* the group's changes that the counting anchors it builds on
//! name, with their ancestors. What a group gives at a change is decided by the group's own
//! rules from what the change knows, with each of those group changes counting as the
//! group's rules decide among every group change that the space's counting anchors name: so
//! a group change that the space has learned to be set aside gave nothing, and a removal set
//! aside took nothing. A group change that lowers what the group gives, and that a change
//! does not know, takes effect at its *first anchors*, the counting anchors that name it and
//! build on no other that does: the change is set aside unless one of them builds on it.
//!
//! Groups inside groups reach the space too, through the groups they are in: the space follows
//! the groups at every level, and knows each through its own anchors alone. Each group's
//! changes are decided by its own rules among those that the space's anchors name, as for a
//! group in the space; what the groups inside a group give counts in the space through the
//! space's own anchors of them, by routes that pass each group once, so that groups that
//! hold each other give nothing through the cycle. Every route counts, however many groups
//! share the groups inside them: what the routes give is found from the records they pass,
//! without listing them.
//!
//! [technical design: groups]: ../../../../docs/Technical-design.md#138-groups

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use nibkeep_base::{EntityId, SpaceId, StatementId};

use super::roles::{Role, View};
use super::{Fact, KeyBytes, decide, listings};
use crate::{GroupRef, Joining};
use nibkeep_base::ChangeId;

/// What a space needs of a group's log to follow it: the group's governing changes that the
/// replica can read, in their order, and the governing changes that each builds on. A
/// replica takes it from its copy of the group's log (`Log::followed`) and gives it
/// to the space's log (`Log::follow`), which decides the group's changes by the
/// group's rules from those that its anchors name.
#[derive(Clone, Debug)]
pub struct Followed {
	group: SpaceId,
	facts: Vec<Fact>,
	/// For each fact, the facts it builds on, by index.
	ancestry: Vec<BTreeSet<usize>>,
	/// Each fact's index.
	index: BTreeMap<ChangeId, usize>,
}

impl Followed {
	/// What a group's log says, from its governing changes in their order and the facts that
	/// each builds on.
	#[must_use]
	pub fn new(group: SpaceId, facts: Vec<Fact>, ancestry: Vec<BTreeSet<usize>>) -> Self {
		let index = facts
			.iter()
			.enumerate()
			.map(|(at, fact)| (fact.id, at))
			.collect();
		Self {
			group,
			facts,
			ancestry,
			index,
		}
	}

	/// The group.
	#[must_use]
	pub const fn group(&self) -> SpaceId {
		self.group
	}

	/// The group's first change, if the replica holds it.
	#[must_use]
	pub fn root(&self) -> Option<ChangeId> {
		self.facts.iter().find(|fact| fact.root).map(|fact| fact.id)
	}

	/// The group's governing changes that no other of them builds on: what an anchor names.
	#[must_use]
	pub fn heads(&self) -> Vec<ChangeId> {
		let built_on: BTreeSet<usize> = self.ancestry.iter().flatten().copied().collect();
		(0..self.facts.len())
			.filter(|at| !built_on.contains(at))
			.map(|at| self.facts[at].id)
			.collect()
	}

	/// Whether it holds the same governing changes as `other`.
	#[must_use]
	pub fn same_changes(&self, other: &Self) -> bool {
		self.group == other.group
			&& self
				.facts
				.iter()
				.map(|fact| fact.id)
				.eq(other.facts.iter().map(|fact| fact.id))
	}

	/// The governing changes `named`, with every one that they build on; `None` if one of
	/// them is no governing change that the replica holds of the group.
	fn closure(&self, named: &[ChangeId]) -> Option<BTreeSet<ChangeId>> {
		let mut known = BTreeSet::new();
		for change in named {
			let &at = self.index.get(change)?;
			known.insert(*change);
			known.extend(
				self.ancestry[at]
					.iter()
					.map(|&earlier| self.facts[earlier].id),
			);
		}
		Some(known)
	}

	/// What the group's rules decide among the governing changes `known`, which hold every
	/// one that each of them builds on. The groups inside it are left out: within a space,
	/// what they give counts through the space's own anchors of them, and a group's changes
	/// by its members through them, which are never an admin's, matter to the group alone.
	fn decide_among(&self, known: &BTreeSet<ChangeId>) -> Decided {
		let kept: Vec<usize> = (0..self.facts.len())
			.filter(|&at| known.contains(&self.facts[at].id))
			.collect();
		let renumbered: BTreeMap<usize, usize> = kept
			.iter()
			.enumerate()
			.map(|(new, &old)| (old, new))
			.collect();
		let facts: Vec<Fact> = kept.iter().map(|&at| self.facts[at].clone()).collect();
		let ancestry: Vec<BTreeSet<usize>> = kept
			.iter()
			.map(|&at| {
				self.ancestry[at]
					.iter()
					.filter_map(|earlier| renumbered.get(earlier).copied())
					.collect()
			})
			.collect();
		let ids: Vec<ChangeId> = facts.iter().map(|fact| fact.id).collect();
		let position: BTreeMap<ChangeId, usize> =
			ids.iter().enumerate().map(|(at, id)| (*id, at)).collect();
		let Ok(counting) = decide::<Infallible>(
			&facts,
			Some(&ancestry),
			(self.group, None),
			|ancestor, of| {
				Ok(ancestor == of
					|| match (position.get(&ancestor), position.get(&of)) {
						(Some(ancestor), Some(of)) => ancestry[*of].contains(ancestor),
						_ => false,
					})
			},
		);
		let view = View::of(&facts, &ancestry, &counting).with_claims(&facts, &ancestry, &counting);
		let ai_agents = listings(&facts).ai_agents;
		let mut decided = Decided {
			view,
			ai_agents,
			..Decided::default()
		};
		for fact in facts.iter().filter(|fact| counting.contains(&fact.id)) {
			for joining in &fact.joinings {
				decided.agreements.push((fact.id, *joining));
			}
			for joining in &fact.withdrawals {
				decided.withdrawals.push((fact.id, *joining));
			}
			for key in &fact.revokes {
				decided.revocations.push((fact.id, *key));
			}
		}
		decided
	}

	/// The keys that its governing changes list, counting or not, but AI agents': those of
	/// the group's replicas whose changes the space takes, to count or to set aside.
	#[must_use]
	pub fn known_keys(&self) -> BTreeSet<KeyBytes> {
		let ai_agents = listings(&self.facts).ai_agents;
		self.facts
			.iter()
			.flat_map(|fact| fact.lists.iter().map(|(_, key)| *key))
			.filter(|key| !ai_agents.contains(key))
			.collect()
	}
}

/// What a group's rules decide among the group's changes that a space's counting anchors
/// name, with their ancestors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decided {
	/// What the counting ones among them say of members, keys and roles.
	view: View,
	/// The agreements to join spaces that count, with their changes.
	agreements: Vec<(ChangeId, Joining)>,
	/// The withdrawals of agreements that count, with their changes.
	withdrawals: Vec<(ChangeId, Joining)>,
	/// The revocations that count, with their changes.
	revocations: Vec<(ChangeId, KeyBytes)>,
	/// The keys that are AI agents' in the group, which count in a space only where they are
	/// listed themselves.
	ai_agents: BTreeSet<KeyBytes>,
}

impl Decided {
	/// Whether the group, by the group changes `known`, agrees to join a space with an
	/// addition, and has not withdrawn that agreement, which is for good.
	fn agrees(&self, joining: &Joining, known: &BTreeSet<ChangeId>) -> bool {
		let stated = |list: &[(ChangeId, Joining)]| {
			list.iter()
				.any(|(change, stated)| stated == joining && known.contains(change))
		};
		stated(&self.agreements) && !stated(&self.withdrawals)
	}

	/// Whether a counting revocation among the group changes `known` revokes a key.
	fn revokes(&self, key: &KeyBytes, known: &BTreeSet<ChangeId>) -> bool {
		self.revocations
			.iter()
			.any(|(change, revoked)| revoked == key && known.contains(change))
	}
}

/// What a space's change knows of each group: the group's changes, with their ancestors.
pub(super) type Knowledge = BTreeMap<SpaceId, BTreeSet<ChangeId>>;

/// A key's member in a group that the routes from a space reach at a change, with the member's
/// role there by the group changes that the change knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GroupMember {
	/// The group that lists the key.
	pub group: SpaceId,
	/// The key's member there.
	pub member: EntityId,
	/// The member's role there.
	pub role: Role,
}

/// A group in the space at a change, where routes start: the group, its record in the space,
/// whose role there no route through it passes, and the statement that adds it there, which
/// its agreement names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Top {
	pub group: SpaceId,
	pub record: EntityId,
	pub addition: StatementId,
}

/// A group inside a group that a route may pass: the group it is inside, the group, its record
/// there, and the statement that adds it there, which its agreement names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hop {
	container: SpaceId,
	group: SpaceId,
	record: EntityId,
	addition: StatementId,
}

/// The routes from a space into the groups in it and inside them, at every level, at a change:
/// the groups in the space, and each record of a group inside a group that a route reaches,
/// with its role there. They are held by what they pass rather than one by one, as groups
/// that share the groups inside them multiply the routes: a route passes each group once, and
/// never the space, and whatever a walk through these records gives, a route gives too, the
/// walk without its cycles.
#[derive(Clone, Debug, Default)]
pub(super) struct Routes {
	/// The groups in the space.
	pub tops: Vec<Top>,
	/// Each record of a group inside a group that a route reaches, with its role there, which
	/// is never none.
	hops: Vec<(Hop, Role)>,
	/// The records of [`Routes::hops`], by index, by the group inside.
	into: BTreeMap<SpaceId, Vec<usize>>,
	/// The keys that the groups that the routes reach list, but AI agents' keys and revoked
	/// ones there, each with its members, by the order in which a walk, level by level,
	/// reaches the groups.
	pub listed: BTreeMap<KeyBytes, Vec<GroupMember>>,
}

impl Routes {
	fn new(
		tops: Vec<Top>,
		hops: Vec<(Hop, Role)>,
		listed: BTreeMap<KeyBytes, Vec<GroupMember>>,
	) -> Self {
		let mut into: BTreeMap<SpaceId, Vec<usize>> = BTreeMap::new();
		for (at, (hop, _)) in hops.iter().enumerate() {
			into.entry(hop.group).or_default().push(at);
		}
		Self {
			tops,
			hops,
			into,
			listed,
		}
	}

	/// The groups from which a walk through the records that `open(index)` takes reaches one
	/// of the groups `ends`, those included.
	fn reaching(
		&self,
		ends: impl IntoIterator<Item = SpaceId>,
		mut open: impl FnMut(usize) -> bool,
	) -> BTreeSet<SpaceId> {
		let mut reaching: BTreeSet<SpaceId> = ends.into_iter().collect();
		let mut to_visit: Vec<SpaceId> = reaching.iter().copied().collect();
		while let Some(group) = to_visit.pop() {
			for &at in self.into.get(&group).into_iter().flatten() {
				let container = self.hops[at].0.container;
				if open(at) && reaching.insert(container) {
					to_visit.push(container);
				}
			}
		}
		reaching
	}

	/// For each of [`Routes::tops`], the best that a route from it gives one of `members`: the
	/// highest, over the routes that reach one of them, of the lowest of the roles of the
	/// records it passes, in the groups they are in, and of the member's; each as the group
	/// changes `lowered` lower it. `None` for a group from which no route reaches one of them.
	/// `lowered` must come from [`Reach::lowerings`] for these routes and these members, as
	/// what they lower is named by its index in each.
	#[must_use]
	pub(super) fn best(&self, members: &[GroupMember], lowered: &[&Lowering]) -> Vec<Option<Role>> {
		let mut hops: Vec<Role> = self.hops.iter().map(|(_, role)| *role).collect();
		let mut ends: Vec<Role> = members.iter().map(|member| member.role).collect();
		let mut tops = vec![Role::Admin; self.tops.len()];
		for lowering in lowered {
			if let Some((place, role)) = lowering.at {
				let lowest = match place {
					Place::Top(at) => tops.get_mut(at),
					Place::Hop(at) => hops.get_mut(at),
					Place::Member(at) => ends.get_mut(at),
				}
				.expect("a lowering of these routes and members");
				*lowest = (*lowest).min(role);
			}
		}
		// For each role, from the lowest, the groups from which a route reaches one of the
		// members with at least that role, through records with at least that role.
		let mut best: BTreeMap<SpaceId, Role> = BTreeMap::new();
		for level in [Role::None, Role::Viewer, Role::Editor, Role::Admin] {
			let reached = members
				.iter()
				.zip(&ends)
				.filter(|(_, role)| **role >= level)
				.map(|(member, _)| member.group);
			let reaching = self.reaching(reached, |at| hops[at] >= level);
			if reaching.is_empty() {
				break;
			}
			for group in reaching {
				best.insert(group, level);
			}
		}
		self.tops
			.iter()
			.zip(tops)
			.map(|(top, cap)| best.get(&top.group).map(|role| (*role).min(cap)))
			.collect()
	}
}

/// What the routes from a space to a group pass through, whatever roles and agreements say,
/// as [`Reach::passes_to`] finds them.
#[derive(Clone, Debug, Default)]
pub(super) struct Passes {
	/// The groups in the space that they go through.
	pub tops: BTreeSet<SpaceId>,
	/// The records of groups inside groups, each with the group it is in.
	pub records: BTreeSet<(SpaceId, EntityId)>,
	/// Each group on them, with the space or group it joined.
	pub joinings: BTreeSet<(SpaceId, SpaceId)>,
}

/// A group change that would lower what a group gives a key at a change that does not know
/// it: the group, the change, and what it lowers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lowering {
	pub group: SpaceId,
	pub change: ChangeId,
	/// What it lowers on the routes, with the role it leaves; or `None` for every route, as a
	/// revocation of the key does.
	pub at: Option<(Place, Role)>,
}

/// What a group change lowers on the routes, by its index in the routes and members that
/// [`Reach::lowerings`] was given: a group in the space, whose agreement it withdraws; a
/// record of a group inside a group, which it gives a lower role, or whose group's agreement
/// it withdraws; or a key's member, which it gives a lower role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
	Top(usize),
	Hop(usize),
	Member(usize),
}

/// A group change that ends paths through a group: the group, the change, and what it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Departure {
	pub group: SpaceId,
	pub change: ChangeId,
	pub ends: Ends,
}

/// What a group change ends: a member's paths through the group, or those of a group inside
/// it; a key on every path; or every path through the group into a space or a group it had
/// joined, as a withdrawal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ends {
	Member(EntityId),
	Key(KeyBytes),
	Withdrawal(SpaceId),
}

/// What the groups that a space follows give in it, for one decision: what each governing
/// change of the space knows of them through its own anchors, the space's records of groups,
/// and, for the anchors taken to count, what each group's rules decide among the group
/// changes they name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
	space: SpaceId,
	/// The space's first change.
	root: Option<ChangeId>,
	/// For each governing change of the space that anchors, what it knows of each group by
	/// its own anchors: the changes it names, with their ancestors; `None` if it names a
	/// change that the replica holds of no group it follows, so that it cannot count yet.
	knows: BTreeMap<ChangeId, Option<Knowledge>>,
	/// The space's records of groups, by the governing changes that make them, counting or
	/// not, in their order: the group, the statement that adds it, and the change.
	records: Records,
	/// For each governing change of the space, those it builds on.
	ancestors: BTreeMap<ChangeId, BTreeSet<ChangeId>>,
	/// The anchors taken to count.
	counting: BTreeSet<ChangeId>,
	/// For each group, the changes that the counting anchors name, with their ancestors, and
	/// what its rules decide among them.
	groups: BTreeMap<SpaceId, (BTreeSet<ChangeId>, Decided)>,
	/// The keys of the replicas of each group that a record in the space names, whose
	/// changes the space takes: those its governing changes list, counting or not, but AI
	/// agents'.
	known_keys: BTreeSet<KeyBytes>,
	/// The first change of each group it follows, if the replica holds it: a record of the
	/// group names it, so that no other log is taken for the group.
	roots: BTreeMap<SpaceId, Option<ChangeId>>,
	/// The records that the groups it follows make of groups inside them, by the group, in
	/// the order of its changes, counting or not: the group inside, the statement that adds
	/// it, and the change.
	inner_records: BTreeMap<SpaceId, Records>,
}

/// Records of groups in a space or a group, each with the changes that make it, counting or
/// not, in their order: the group, the statement that adds it, and the change.
type Records = BTreeMap<EntityId, Vec<(GroupRef, StatementId, ChangeId)>>;

/// What each group's rules decide among the changes that anchors name, by the group and those
/// changes, kept while a space's changes are decided.
pub(super) type DecidedGroups = BTreeMap<(SpaceId, BTreeSet<ChangeId>), Decided>;

impl Reach {
	/// What the space's governing changes `facts`, each building on those `ancestors` names,
	/// say of the groups that `following` holds, at every level, with no anchor taken to count
	/// yet. `None` if they follow no group and make no record of one.
	#[must_use]
	pub fn new(
		space: SpaceId,
		facts: &[Fact],
		ancestors: &[BTreeSet<usize>],
		following: &BTreeMap<SpaceId, Followed>,
	) -> Option<Self> {
		let anchors = facts.iter().any(|fact| !fact.anchors.is_empty());
		let records = facts.iter().any(|fact| !fact.group_records.is_empty());
		if following.is_empty() && !anchors && !records {
			return None;
		}
		let mut reach = Self {
			space,
			root: facts.iter().find(|fact| fact.root).map(|fact| fact.id),
			knows: BTreeMap::new(),
			records: BTreeMap::new(),
			ancestors: BTreeMap::new(),
			counting: BTreeSet::new(),
			groups: BTreeMap::new(),
			known_keys: BTreeSet::new(),
			roots: following
				.iter()
				.map(|(group, followed)| (*group, followed.root()))
				.collect(),
			inner_records: BTreeMap::new(),
		};
		for (at, fact) in facts.iter().enumerate() {
			reach.ancestors.insert(
				fact.id,
				ancestors[at]
					.iter()
					.map(|&earlier| facts[earlier].id)
					.collect(),
			);
			for (record, group, addition) in &fact.group_records {
				reach
					.records
					.entry(*record)
					.or_default()
					.push((*group, *addition, fact.id));
			}
			if fact.anchors.is_empty() {
				continue;
			}
			let mut knows: Option<Knowledge> = Some(BTreeMap::new());
			for anchor in &fact.anchors {
				let named = following
					.get(&anchor.group)
					.and_then(|followed| followed.closure(&anchor.changes));
				match (named, &mut knows) {
					(Some(named), Some(knows)) => {
						knows.entry(anchor.group).or_default().extend(named);
					}
					_ => knows = None,
				}
			}
			reach.knows.insert(fact.id, knows);
		}
		reach.follow_inner(following);
		Some(reach)
	}

	/// Notes the records that the groups it follows make of groups inside them, and the keys
	/// of the groups that records name at every level, from the space's own, whose changes
	/// the space takes.
	fn follow_inner(&mut self, following: &BTreeMap<SpaceId, Followed>) {
		for (group, followed) in following {
			let mut records = Records::new();
			for fact in &followed.facts {
				for (record, inner, addition) in &fact.group_records {
					records
						.entry(*record)
						.or_default()
						.push((*inner, *addition, fact.id));
				}
			}
			if !records.is_empty() {
				self.inner_records.insert(*group, records);
			}
		}
		// The groups that records name at every level, from the space's own.
		let mut named: BTreeSet<SpaceId> = self
			.records
			.values()
			.flatten()
			.map(|(group, ..)| group.group)
			.collect();
		let mut to_visit: Vec<SpaceId> = named.iter().copied().collect();
		while let Some(group) = to_visit.pop() {
			for makings in self
				.inner_records
				.get(&group)
				.into_iter()
				.flat_map(BTreeMap::values)
			{
				for (inner, ..) in makings {
					if named.insert(inner.group) {
						to_visit.push(inner.group);
					}
				}
			}
		}
		self.known_keys = following
			.iter()
			.filter(|(group, _)| named.contains(group))
			.flat_map(|(_, followed)| followed.known_keys())
			.collect();
	}

	/// The anchors that can count: those whose every named change the replica holds of a
	/// group it follows.
	#[must_use]
	pub fn complete_anchors(&self) -> BTreeSet<ChangeId> {
		self.knows
			.iter()
			.filter(|(_, knows)| knows.is_some())
			.map(|(anchor, _)| *anchor)
			.collect()
	}

	/// Whether a governing change of the space anchors, and names a change that the replica
	/// holds of no group it follows, so that it cannot count yet.
	pub fn is_incomplete(&self, change: &ChangeId) -> bool {
		self.knows.get(change).is_some_and(Option::is_none)
	}

	/// Whether a governing change of the space anchors a group.
	#[must_use]
	pub fn is_anchor(&self, change: &ChangeId) -> bool {
		self.knows.contains_key(change)
	}

	/// Takes `counting` to be the anchors that count, and decides each group's changes by its
	/// rules among those that they name.
	pub fn assume(
		&mut self,
		counting: BTreeSet<ChangeId>,
		following: &BTreeMap<SpaceId, Followed>,
		decided: &mut DecidedGroups,
	) {
		let mut named: BTreeMap<SpaceId, BTreeSet<ChangeId>> = BTreeMap::new();
		for anchor in &counting {
			if let Some(Some(knows)) = self.knows.get(anchor) {
				for (group, changes) in knows {
					named.entry(*group).or_default().extend(changes);
				}
			}
		}
		self.groups = named
			.into_iter()
			.filter_map(|(group, known)| {
				let followed = following.get(&group)?;
				let found = decided
					.entry((group, known.clone()))
					.or_insert_with(|| followed.decide_among(&known))
					.clone();
				Some((group, (known, found)))
			})
			.collect();
		self.counting = counting;
	}

	/// Whether it gives what `other` gives: the same anchors count, naming the same changes of
	/// the same groups, decided alike, and the same records of groups.
	#[must_use]
	pub fn gives_same(&self, other: &Self) -> bool {
		let named = |reach: &Self| -> Vec<(ChangeId, Option<Knowledge>)> {
			reach
				.counting
				.iter()
				.map(|anchor| (*anchor, reach.knows.get(anchor).cloned().flatten()))
				.collect()
		};
		self.counting == other.counting
			&& self.groups == other.groups
			&& self.records == other.records
			&& named(self) == named(other)
	}

	/// The anchors taken to count.
	#[must_use]
	pub fn counting(&self) -> &BTreeSet<ChangeId> {
		&self.counting
	}

	/// The keys of the groups' replicas whose changes the space takes.
	#[must_use]
	pub fn known_keys(&self) -> &BTreeSet<KeyBytes> {
		&self.known_keys
	}

	/// Whether a key is an AI agent's in a group: it counts in the space only where it is
	/// listed itself.
	fn is_ai_agent_in(&self, group: &SpaceId, key: &KeyBytes) -> bool {
		self.groups
			.get(group)
			.is_some_and(|(_, decided)| decided.ai_agents.contains(key))
	}

	/// What a change knows of the groups, if it builds on the governing changes `known` of the
	/// space, and is `own` itself if it is one: what the counting anchors among them name, and
	/// what `own` names if it anchors.
	pub fn knowledge<'k>(
		&self,
		known: impl IntoIterator<Item = &'k ChangeId>,
		own: Option<ChangeId>,
	) -> Knowledge {
		let mut knowledge = Knowledge::new();
		let mut add = |anchor: &ChangeId| {
			if let Some(Some(knows)) = self.knows.get(anchor) {
				for (group, changes) in knows {
					knowledge.entry(*group).or_default().extend(changes);
				}
			}
		};
		for anchor in known {
			if self.counting.contains(anchor) {
				add(anchor);
			}
		}
		if let Some(own) = own {
			add(&own);
		}
		knowledge
	}

	/// The groups in the space at a change whose knowledge is `knowledge`, with their records
	/// there, if the change builds on a governing change that makes the record, as
	/// `made(change)` says: each group whose record a known change makes, by the first such
	/// change, and that agrees, by the group changes the change knows, to that record's
	/// addition and the space's first change.
	pub fn groups_in(
		&self,
		knowledge: &Knowledge,
		mut made: impl FnMut(&ChangeId) -> bool,
	) -> Vec<(SpaceId, EntityId, StatementId)> {
		let Some(root) = self.root else {
			return Vec::new();
		};
		let mut groups = Vec::new();
		for (record, makings) in &self.records {
			let Some((group, addition, _)) = makings.iter().find(|(.., change)| made(change))
			else {
				continue;
			};
			// A record names the group's first change, so that no other log is taken for it.
			if self.roots.get(&group.group).copied().flatten() != Some(group.root) {
				continue;
			}
			let (Some(known), Some((_, decided))) =
				(knowledge.get(&group.group), self.groups.get(&group.group))
			else {
				continue;
			};
			let joining = Joining {
				space: self.space,
				root,
				addition: *addition,
			};
			if decided.agrees(&joining, known) {
				groups.push((group.group, *record, *addition));
			}
		}
		groups
	}

	/// Walks from the groups `from`, into the groups inside them, level by level, by the
	/// records that each group reached makes of groups inside it, as `step` takes each, with
	/// the changes that make it: the group inside, the statement that adds it, and the role its
	/// record gives. Returns the groups reached, in the order it reaches them, and each record
	/// it takes, with its role, but those of the space and of a group inside itself, which no
	/// route passes.
	fn walk(
		&self,
		from: impl IntoIterator<Item = SpaceId>,
		mut step: impl FnMut(
			SpaceId,
			EntityId,
			&[(GroupRef, StatementId, ChangeId)],
		) -> Option<(SpaceId, StatementId, Role)>,
	) -> (Vec<SpaceId>, Vec<(Hop, Role)>) {
		let mut order: Vec<SpaceId> = Vec::new();
		let mut reached: BTreeSet<SpaceId> = BTreeSet::new();
		for group in from {
			if reached.insert(group) {
				order.push(group);
			}
		}
		let mut hops = Vec::new();
		let mut at = 0;
		while let Some(&container) = order.get(at) {
			at += 1;
			for (record, makings) in self.inner_records.get(&container).into_iter().flatten() {
				let Some((group, addition, role)) = step(container, *record, makings) else {
					continue;
				};
				if group == self.space || group == container {
					continue;
				}
				let hop = Hop {
					container,
					group,
					record: *record,
					addition,
				};
				hops.push((hop, role));
				if reached.insert(group) {
					order.push(group);
				}
			}
		}
		(order, hops)
	}

	/// The routes from the space into the groups `groups` that are in it at a change whose
	/// knowledge is `knowledge`, and into the groups inside them at every level: each group
	/// inside a group that a route reaches, whose record there a group change that the change
	/// knows makes, by the first such change, that names the first change of the group's log
	/// that the replica follows, that agrees, by the group changes it knows, to that addition
	/// and the first change of the group it is inside, and whose record has a role there, by
	/// those changes. With them, each key that a counting listing among the group changes the
	/// change knows lists, in a group they reach, the latest, with its member there and that
	/// member's role, but AI agents' keys and those that a counting revocation among them
	/// revokes.
	#[must_use]
	pub(super) fn routes(
		&self,
		knowledge: &Knowledge,
		groups: &[(SpaceId, EntityId, StatementId)],
	) -> Routes {
		let tops: Vec<Top> = groups
			.iter()
			.map(|&(group, record, addition)| Top {
				group,
				record,
				addition,
			})
			.collect();
		let (order, hops) = self.walk(
			tops.iter().map(|top| top.group),
			|container, record, makings| {
				let known = knowledge.get(&container)?;
				let (_, decided) = self.groups.get(&container)?;
				let root = self.roots.get(&container).copied().flatten()?;
				let (inner, addition, _) =
					makings.iter().find(|(.., change)| known.contains(change))?;
				if self.roots.get(&inner.group).copied().flatten() != Some(inner.root) {
					return None;
				}
				let (group_known, (_, group_decided)) =
					(knowledge.get(&inner.group)?, self.groups.get(&inner.group)?);
				let joining = Joining {
					space: container,
					root,
					addition: *addition,
				};
				if !group_decided.agrees(&joining, group_known) {
					return None;
				}
				// A group whose record has no role in the group it is in gives nothing there.
				let role = decided.view.role(record, known, |_| false).0;
				(role > Role::None).then_some((inner.group, *addition, role))
			},
		);
		let mut listed: BTreeMap<KeyBytes, Vec<GroupMember>> = BTreeMap::new();
		for group in order {
			let (Some(known), Some((_, decided))) =
				(knowledge.get(&group), self.groups.get(&group))
			else {
				continue;
			};
			for (key, member) in decided.view.listed_among(known) {
				if self.is_ai_agent_in(&group, &key) || decided.revokes(&key, known) {
					continue;
				}
				let role = decided.view.role(member, known, |_| false).0;
				listed.entry(key).or_default().push(GroupMember {
					group,
					member,
					role,
				});
			}
		}
		Routes::new(tops, hops, listed)
	}

	/// Whether a counting revocation among the group changes that a change knows, of any
	/// group, revokes a key: it is ended in the space on every path.
	#[must_use]
	pub fn revoked(&self, key: &KeyBytes, knowledge: &Knowledge) -> bool {
		knowledge.iter().any(|(group, known)| {
			self.groups
				.get(group)
				.is_some_and(|(_, decided)| decided.revokes(key, known))
		})
	}

	/// The group changes that the counting anchors name and a change whose knowledge is
	/// `knowledge` does not know, which would lower what `routes` give `key`'s `members` there:
	/// those that give one of the members, or a group inside a group on a route to one of
	/// them, a lower role; those that withdraw a group on such a route from the space or the
	/// group it is in; and those that revoke the key, which lower every route.
	#[must_use]
	pub(super) fn lowerings(
		&self,
		key: &KeyBytes,
		routes: &Routes,
		members: &[GroupMember],
		knowledge: &Knowledge,
	) -> Vec<Lowering> {
		let empty = BTreeSet::new();
		let known_of = |group: &SpaceId| knowledge.get(group).unwrap_or(&empty);
		let mut lowerings = Vec::new();
		for (group, (_, decided)) in &self.groups {
			let known = known_of(group);
			for (change, revoked) in &decided.revocations {
				if revoked == key && !known.contains(change) {
					lowerings.push(Lowering {
						group: *group,
						change: *change,
						at: None,
					});
				}
			}
		}
		// Only the records on routes to the members.
		let reaching = routes.reaching(members.iter().map(|member| member.group), |_| true);
		let hops = || {
			routes
				.hops
				.iter()
				.enumerate()
				.filter(|(_, (hop, _))| reaching.contains(&hop.group))
		};
		let records = members
			.iter()
			.enumerate()
			.map(|(at, member)| (Place::Member(at), member.group, member.member, member.role))
			.chain(
				hops().map(|(at, (hop, role))| (Place::Hop(at), hop.container, hop.record, *role)),
			);
		for (place, group, record, given) in records {
			let Some((named, decided)) = self.groups.get(&group) else {
				continue;
			};
			let known = known_of(&group);
			for (change, role) in decided.view.roles_of(record) {
				if role < given && named.contains(&change) && !known.contains(&change) {
					lowerings.push(Lowering {
						group,
						change,
						at: Some((place, role)),
					});
				}
			}
		}
		let tops = routes.tops.iter().enumerate().filter_map(|(at, top)| {
			let root = self.root.filter(|_| reaching.contains(&top.group))?;
			let joining = Joining {
				space: self.space,
				root,
				addition: top.addition,
			};
			Some((Place::Top(at), top.group, joining))
		});
		let hops = hops().filter_map(|(at, (hop, _))| {
			let root = self.roots.get(&hop.container).copied().flatten()?;
			let joining = Joining {
				space: hop.container,
				root,
				addition: hop.addition,
			};
			Some((Place::Hop(at), hop.group, joining))
		});
		for (place, group, joining) in tops.chain(hops) {
			let Some((_, decided)) = self.groups.get(&group) else {
				continue;
			};
			let known = known_of(&group);
			for (change, withdrawn) in &decided.withdrawals {
				if *withdrawn == joining && !known.contains(change) {
					lowerings.push(Lowering {
						group,
						change: *change,
						at: Some((place, Role::None)),
					});
				}
			}
		}
		lowerings
	}

	/// Whether a group change that a change does not know leaves it as it was: whether one of
	/// the change's first anchors builds on the change, as `builds_on(anchor)` says. One that
	/// no counting anchor names changes nothing yet.
	pub fn keeps(&self, lowering: &Lowering, builds_on: impl FnMut(&ChangeId) -> bool) -> bool {
		let first = self.first_anchors(&lowering.group, &lowering.change);
		first.is_empty() || first.iter().any(builds_on)
	}

	/// A group change's first anchors: the counting anchors that name it and build on no other
	/// counting anchor that does.
	#[must_use]
	pub fn first_anchors(&self, group: &SpaceId, change: &ChangeId) -> Vec<ChangeId> {
		let naming: Vec<ChangeId> = self
			.counting
			.iter()
			.filter(|anchor| {
				matches!(self.knows.get(anchor), Some(Some(knows))
					if knows.get(group).is_some_and(|known| known.contains(change)))
			})
			.copied()
			.collect();
		naming
			.iter()
			.filter(|anchor| {
				!naming.iter().any(|other| {
					self.ancestors
						.get(anchor)
						.is_some_and(|ancestors| ancestors.contains(other))
				})
			})
			.copied()
			.collect()
	}

	/// The counting group changes, among those that the counting anchors name, that end paths
	/// through the groups in the space, at every level: those that give a member, or a group
	/// inside the group, the role none, those that revoke a key, and those that withdraw a
	/// group from the space or from a group that the space follows.
	#[must_use]
	pub fn departures(&self) -> Vec<Departure> {
		let mut departures = Vec::new();
		for (group, (_, decided)) in &self.groups {
			for (change, member, role) in decided.view.all_roles() {
				if role == Role::None {
					departures.push(Departure {
						group: *group,
						change,
						ends: Ends::Member(member),
					});
				}
			}
			for (change, key) in &decided.revocations {
				departures.push(Departure {
					group: *group,
					change: *change,
					ends: Ends::Key(*key),
				});
			}
			for (change, joining) in &decided.withdrawals {
				let from_space = joining.space == self.space && Some(joining.root) == self.root;
				let from_group =
					self.roots.get(&joining.space).copied().flatten() == Some(joining.root);
				if from_space || from_group {
					departures.push(Departure {
						group: *group,
						change: *change,
						ends: Ends::Withdrawal(joining.space),
					});
				}
			}
		}
		departures
	}

	/// The members, in the groups that the counting anchors name, that a key's latest counting
	/// listing there names, but for AI agents' keys: each group with the key's member there.
	#[must_use]
	pub fn members_of_key(&self, key: &KeyBytes) -> Vec<(SpaceId, EntityId)> {
		self.groups
			.iter()
			.filter(|(_, (_, decided))| !decided.ai_agents.contains(key))
			.filter_map(|(group, (known, decided))| {
				decided
					.view
					.listed_among(known)
					.get(key)
					.map(|member| (*group, *member))
			})
			.collect()
	}

	/// What the routes from the space to the groups `targets` pass through, by every record of a
	/// group that the space's governing changes make, and every one that the group changes that
	/// the counting anchors name make, naming the first change of the group's log that the
	/// replica follows, whatever roles and agreements say: the groups in the space, the records
	/// of groups inside groups, and each group with what it joined. A route to a group passes
	/// it only at its end; where other groups hold each other, it takes the records of the
	/// cycle that a walk to the group may pass too, as finding only those on routes that pass
	/// each group once is too costly for some shapes.
	#[must_use]
	pub(super) fn passes_to(&self, targets: impl IntoIterator<Item = SpaceId>) -> Passes {
		let tops: Vec<Top> = self
			.records
			.iter()
			.filter_map(|(record, makings)| {
				let (group, addition, _) = makings.first()?;
				Some(Top {
					group: group.group,
					record: *record,
					addition: *addition,
				})
			})
			.collect();
		let (_, hops) = self.walk(tops.iter().map(|top| top.group), |container, _, makings| {
			let (named, _) = self.groups.get(&container)?;
			let (inner, addition, _) =
				makings.iter().find(|(.., change)| named.contains(change))?;
			(self.roots.get(&inner.group).copied().flatten() == Some(inner.root)).then_some((
				inner.group,
				*addition,
				Role::Admin,
			))
		});
		let routes = Routes::new(tops, hops, BTreeMap::new());
		let mut passes = Passes::default();
		for target in targets {
			// The groups from which a walk reaches the target, and those that a walk from the
			// groups in the space reaches without passing it.
			let reaching = routes.reaching([target], |at| routes.hops[at].0.container != target);
			let mut reached: BTreeSet<SpaceId> = routes.tops.iter().map(|top| top.group).collect();
			let mut to_visit: Vec<SpaceId> = reached
				.iter()
				.copied()
				.filter(|group| *group != target)
				.collect();
			while let Some(group) = to_visit.pop() {
				for (hop, _) in routes.hops.iter().filter(|(hop, _)| hop.container == group) {
					if reached.insert(hop.group) && hop.group != target {
						to_visit.push(hop.group);
					}
				}
			}
			for top in routes
				.tops
				.iter()
				.filter(|top| reaching.contains(&top.group))
			{
				passes.tops.insert(top.group);
				passes.joinings.insert((top.group, self.space));
			}
			let on_routes = routes.hops.iter().filter(|(hop, _)| {
				hop.container != target
					&& reached.contains(&hop.container)
					&& reaching.contains(&hop.group)
			});
			for (hop, _) in on_routes {
				passes.records.insert((hop.container, hop.record));
				passes.joinings.insert((hop.group, hop.container));
			}
		}
		passes
	}

	/// The keys that the counting listings among the group changes that the counting anchors
	/// name list, in any group, but AI agents' keys there.
	#[must_use]
	pub fn listed_in_groups(&self) -> BTreeSet<KeyBytes> {
		self.groups
			.values()
			.flat_map(|(known, decided)| {
				decided
					.view
					.listed_among(known)
					.into_keys()
					.filter(|key| !decided.ai_agents.contains(key))
			})
			.collect()
	}

	/// The space's records of each group that a key is listed in, by every change that makes
	/// one.
	#[must_use]
	pub fn records_of_group(&self, group: &SpaceId) -> Vec<EntityId> {
		self.records
			.iter()
			.filter(|(_, makings)| makings.iter().any(|(made, ..)| made.group == *group))
			.map(|(record, _)| *record)
			.collect()
	}

	/// The space's records of groups, each with the group and the statement that adds it, by
	/// the first change that makes it.
	#[must_use]
	pub fn group_records(&self) -> Vec<(EntityId, GroupRef, StatementId)> {
		self.records
			.iter()
			.filter_map(|(record, makings)| {
				makings
					.first()
					.map(|(group, addition, _)| (*record, *group, *addition))
			})
			.collect()
	}

	/// Each group's changes that the counting anchors name, with their ancestors.
	#[must_use]
	pub fn anchored(&self) -> BTreeMap<SpaceId, BTreeSet<ChangeId>> {
		self.groups
			.iter()
			.map(|(group, (known, _))| (*group, known.clone()))
			.collect()
	}

	/// Whether an entity is the record, in the space, of a replica or its device whose key a
	/// group change that a change knows lists, counting or not: such records are a member's,
	/// whose role allows changing them.
	#[must_use]
	pub fn is_listed_record(&self, entity: &EntityId, knowledge: &Knowledge) -> bool {
		knowledge.iter().any(|(group, known)| {
			self.groups.get(group).is_some_and(|(_, decided)| {
				let keys: Vec<KeyBytes> = decided.view.listed_any_among(known).collect();
				key_records(self.space, &keys).any(|record| record == *entity)
			})
		})
	}
}

/// The records in a space, derived from the space and each key, of the keys' replicas and
/// their devices.
pub(super) fn key_records<'k>(
	space: SpaceId,
	keys: impl IntoIterator<Item = &'k KeyBytes>,
) -> impl Iterator<Item = EntityId> {
	keys.into_iter()
		.filter_map(nibkeep_crypto::VerifyingKey::from_bytes)
		.flat_map(move |key| {
			[
				crate::replica_record(space, &key),
				crate::device_record(space, &key),
			]
		})
}

/// The records of one person at a change: their records in the space and in the groups in
/// it, joined where a key, not revoked, is listed on two of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Person {
	/// Their member: the space's own record that lists the key asked about, if one does; or
	/// else the smallest of their direct records, or else of their records in groups.
	pub member: EntityId,
	/// Their records in the space itself.
	pub direct: BTreeSet<EntityId>,
	/// The keys on their records.
	pub keys: BTreeSet<KeyBytes>,
	/// Their members in the groups that routes from the space reach.
	pub members: Vec<GroupMember>,
}

/// The person whose key `key` is at a change, from the space's own listings there, `direct`,
/// each key's member, and the keys' members in the groups that routes reach, `through`, with
/// `joins(key)` saying whether a key joins records, as one that is not revoked does. `None`
/// if no listing names the key.
pub(super) fn person_of(
	key: &KeyBytes,
	direct: &BTreeMap<KeyBytes, EntityId>,
	through: &BTreeMap<KeyBytes, Vec<GroupMember>>,
	mut joins: impl FnMut(&KeyBytes) -> bool,
) -> Option<Person> {
	/// A record: one of the space's own, or a member's in a group.
	#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
	enum Node {
		Direct(EntityId),
		InGroup(SpaceId, EntityId),
	}
	let nodes_of = |key: &KeyBytes| -> Vec<Node> {
		let mut nodes: Vec<Node> = direct
			.get(key)
			.map(|&record| Node::Direct(record))
			.into_iter()
			.collect();
		nodes.extend(
			through
				.get(key)
				.into_iter()
				.flatten()
				.map(|member| Node::InGroup(member.group, member.member)),
		);
		nodes
	};
	let start = nodes_of(key);
	if start.is_empty() {
		return None;
	}
	let keys: BTreeSet<KeyBytes> = direct.keys().chain(through.keys()).copied().collect();
	// The records reached from the key's own, through keys listed on two of them.
	let mut reached: BTreeSet<Node> = start.iter().copied().collect();
	let mut person_keys = BTreeSet::from([*key]);
	let mut changed = true;
	while changed {
		changed = false;
		for other in &keys {
			if person_keys.contains(other) {
				continue;
			}
			let nodes = nodes_of(other);
			if nodes.iter().any(|node| reached.contains(node)) {
				// A key on one of their records is theirs; only one not revoked joins
				// the others it is listed on.
				person_keys.insert(*other);
				changed = true;
				if joins(other) {
					reached.extend(nodes);
				}
			}
		}
	}
	let direct_records: BTreeSet<EntityId> = reached
		.iter()
		.filter_map(|node| match node {
			Node::Direct(record) => Some(*record),
			Node::InGroup(..) => None,
		})
		.collect();
	let member = direct
		.get(key)
		.copied()
		.or_else(|| direct_records.iter().next().copied())
		.or_else(|| {
			reached.iter().find_map(|node| match node {
				Node::InGroup(_, member) => Some(*member),
				Node::Direct(_) => None,
			})
		})?;
	let members = person_keys
		.iter()
		.flat_map(|key| through.get(key).into_iter().flatten())
		.filter(|member| reached.contains(&Node::InGroup(member.group, member.member)))
		.fold(Vec::new(), |mut members: Vec<GroupMember>, member| {
			if !members.contains(member) {
				members.push(*member);
			}
			members
		});
	Some(Person {
		member,
		direct: direct_records,
		keys: person_keys,
		members,
	})
}

#[cfg(test)]
mod tests {
	use nibkeep_base::Hash;
	use nibkeep_testkit::TestRng;

	use super::*;

	const ROLES: [Role; 4] = [Role::None, Role::Viewer, Role::Editor, Role::Admin];

	/// Routes from the groups `tops` in a space, through the records of groups inside groups
	/// that `hops` names, each `(container, group, role)`.
	fn routes(rng: &mut TestRng, tops: &[SpaceId], hops: &[(SpaceId, SpaceId, Role)]) -> Routes {
		let tops = tops
			.iter()
			.map(|&group| Top {
				group,
				record: rng.entity_id(),
				addition: rng.statement_id(),
			})
			.collect();
		let hops = hops
			.iter()
			.map(|&(container, group, role)| {
				let hop = Hop {
					container,
					group,
					record: rng.entity_id(),
					addition: rng.statement_id(),
				};
				(hop, role)
			})
			.collect();
		Routes::new(tops, hops, BTreeMap::new())
	}

	fn member(rng: &mut TestRng, group: SpaceId, role: Role) -> GroupMember {
		GroupMember {
			group,
			member: rng.entity_id(),
			role,
		}
	}

	fn lowering(rng: &mut TestRng, at: Place, role: Role) -> Lowering {
		Lowering {
			group: rng.space_id(),
			change: ChangeId::from_hash(Hash::of(&rng.bytes::<8>())),
			at: Some((at, role)),
		}
	}

	fn pick(rng: &mut TestRng, count: usize) -> usize {
		usize::from(rng.bytes::<1>()[0]) % count
	}

	/// What [`Routes::best`] finds, by listing every route that passes each group once.
	fn best_by_listing(
		routes: &Routes,
		members: &[GroupMember],
		lowered: &[&Lowering],
	) -> Vec<Option<Role>> {
		fn visit(
			at: SpaceId,
			passed: &mut Vec<SpaceId>,
			lowest: Role,
			(routes, hops, ends, members): (&Routes, &[Role], &[Role], &[GroupMember]),
			best: &mut Option<Role>,
		) {
			for (member, end) in members.iter().zip(ends) {
				if member.group == at {
					let given = lowest.min(*end);
					*best = Some(best.map_or(given, |best| best.max(given)));
				}
			}
			for (index, (hop, _)) in routes.hops.iter().enumerate() {
				if hop.container == at && !passed.contains(&hop.group) {
					passed.push(hop.group);
					let lower = lowest.min(hops[index]);
					visit(
						hop.group,
						passed,
						lower,
						(routes, hops, ends, members),
						best,
					);
					passed.pop();
				}
			}
		}
		let mut hops: Vec<Role> = routes.hops.iter().map(|(_, role)| *role).collect();
		let mut ends: Vec<Role> = members.iter().map(|member| member.role).collect();
		let mut tops = vec![Role::Admin; routes.tops.len()];
		for lowering in lowered {
			let (place, role) = lowering.at.expect("a lowering of a place");
			let lowest = match place {
				Place::Top(at) => &mut tops[at],
				Place::Hop(at) => &mut hops[at],
				Place::Member(at) => &mut ends[at],
			};
			*lowest = (*lowest).min(role);
		}
		routes
			.tops
			.iter()
			.zip(tops)
			.map(|(top, cap)| {
				let mut best = None;
				let mut passed = vec![top.group];
				let given = (routes, hops.as_slice(), ends.as_slice(), members);
				visit(top.group, &mut passed, cap, given, &mut best);
				best
			})
			.collect()
	}

	#[test]
	fn the_best_route_gives_the_lowest_role_on_it() {
		let mut rng = TestRng::new("best routes");
		let [a, b, left, right, kids] = [(); 5].map(|()| rng.space_id());
		// A diamond below `a`: an editor's through the left, a viewer's through the right; and
		// `b` reaches the kids only through a viewer's record.
		let routes = routes(
			&mut rng,
			&[a, b],
			&[
				(a, left, Role::Editor),
				(a, right, Role::Editor),
				(left, kids, Role::Editor),
				(right, kids, Role::Viewer),
				(b, right, Role::Editor),
			],
		);
		let son = member(&mut rng, kids, Role::Editor);
		assert_eq!(
			routes.best(&[son], &[]),
			[Some(Role::Editor), Some(Role::Viewer)]
		);
		// The member's own role caps every route.
		let viewer = member(&mut rng, kids, Role::Viewer);
		assert_eq!(
			routes.best(&[viewer], &[]),
			[Some(Role::Viewer), Some(Role::Viewer)]
		);
		// Lowering the left side leaves the right's viewer.
		let left_kids = lowering(&mut rng, Place::Hop(2), Role::None);
		assert_eq!(
			routes.best(&[son], &[&left_kids]),
			[Some(Role::Viewer), Some(Role::Viewer)]
		);
		// A withdrawal of `a` from the space lowers every route from it.
		let withdrawn = lowering(&mut rng, Place::Top(0), Role::None);
		assert_eq!(
			routes.best(&[son], &[&withdrawn]),
			[Some(Role::None), Some(Role::Viewer)]
		);
		// Lowering the member lowers every route to them.
		let lowered = lowering(&mut rng, Place::Member(0), Role::Viewer);
		assert_eq!(
			routes.best(&[son], &[&lowered]),
			[Some(Role::Viewer), Some(Role::Viewer)]
		);
		// A group that no route reaches gets nothing.
		let nowhere = rng.space_id();
		let elsewhere = member(&mut rng, nowhere, Role::Editor);
		assert_eq!(routes.best(&[elsewhere], &[]), [None, None]);
	}

	#[test]
	fn a_cycle_gives_no_route_more_than_it_gives_without_it() {
		let mut rng = TestRng::new("cycles");
		let [a, x, y, kids] = [(); 4].map(|()| rng.space_id());
		// `x` and `y` hold each other, editors both ways; only `y` holds the kids, through a
		// viewer's record, and `x` reaches `y` only through a viewer's.
		let routes = routes(
			&mut rng,
			&[a],
			&[
				(a, x, Role::Editor),
				(x, y, Role::Viewer),
				(y, x, Role::Editor),
				(y, kids, Role::Editor),
			],
		);
		let son = member(&mut rng, kids, Role::Editor);
		assert_eq!(routes.best(&[son], &[]), [Some(Role::Viewer)]);
	}

	#[test]
	fn the_best_route_is_the_best_of_every_route_listed() {
		let mut rng = TestRng::new("every route");
		for _ in 0..2_000 {
			// A few groups, some in the space, holding each other at random: shared, repeated,
			// in cycles, and in themselves.
			let groups: Vec<SpaceId> = (0..2 + pick(&mut rng, 5)).map(|_| rng.space_id()).collect();
			let tops: Vec<SpaceId> = (0..=pick(&mut rng, 3))
				.map(|_| groups[pick(&mut rng, groups.len())])
				.collect();
			let hops: Vec<(SpaceId, SpaceId, Role)> = (0..pick(&mut rng, 12))
				.map(|_| {
					let container = groups[pick(&mut rng, groups.len())];
					let group = groups[pick(&mut rng, groups.len())];
					(container, group, ROLES[1 + pick(&mut rng, 3)])
				})
				.collect();
			let routes = routes(&mut rng, &tops, &hops);
			let members: Vec<GroupMember> = (0..=pick(&mut rng, 3))
				.map(|_| {
					let group = groups[pick(&mut rng, groups.len())];
					let role = ROLES[pick(&mut rng, 4)];
					member(&mut rng, group, role)
				})
				.collect();
			let mut places: Vec<Place> = (0..tops.len()).map(Place::Top).collect();
			places.extend((0..hops.len()).map(Place::Hop));
			places.extend((0..members.len()).map(Place::Member));
			let lowerings: Vec<Lowering> = (0..pick(&mut rng, 4))
				.map(|_| {
					let place = places[pick(&mut rng, places.len())];
					let role = ROLES[pick(&mut rng, 3)];
					lowering(&mut rng, place, role)
				})
				.collect();
			let lowered: Vec<&Lowering> = lowerings.iter().collect();
			assert_eq!(
				routes.best(&members, &lowered),
				best_by_listing(&routes, &members, &lowered),
				"{hops:?} {members:?} {lowerings:?}"
			);
		}
	}
}
