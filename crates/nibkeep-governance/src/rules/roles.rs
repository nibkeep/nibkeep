//! Roles in a space with members ([technical design: sharing]): which governing changes count by
//! what their authors' roles allow, never leaving the space without a direct admin; and, for
//! ordinary changes, the author's member and role at a change.
//!
//! A member's role at a change is what the counting governing changes it builds on give the
//! member, the lowest of those that no other of them builds on, lowered by each counting one
//! that gives the member a lower role and that it neither builds on nor is built on by:
//! nobody acts on a role they did not know they had, and what someone did without knowing
//! that they had lost one is set aside.
//!
//! [technical design: sharing]: ../../../../docs/Technical-design.md#13-sharing

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_model::{Operation, Statement, fields};
use nibkeep_time::Timestamp;

use super::claims::{ClaimEffect, Deciding, Records, WAIT_MILLIS, after, given_at};
use super::groups::{
	Ends, GroupMember, Knowledge, Lowering, Person, Reach, Routes, key_records, person_of,
};
use super::{Fact, KeyBytes, Label};
use crate::{RecoveryEntry, RecoveryStatement};
use nibkeep_base::ChangeId;

/// A member's role in a space with members, lowest first: `none` for a member who left or
/// was removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
	/// No longer a member.
	None,
	/// Sees everything, and changes only their own records.
	Viewer,
	/// Also changes everything but other members' records, and purges what they signed.
	Editor,
	/// Everything.
	Admin,
}

impl Role {
	/// The role that a statement of [`fields::ROLE`] names, if it names one.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		match text {
			"none" => Some(Self::None),
			"viewer" => Some(Self::Viewer),
			"editor" => Some(Self::Editor),
			"admin" => Some(Self::Admin),
			_ => None,
		}
	}

	/// The role as a statement of [`fields::ROLE`] names it.
	#[must_use]
	pub const fn as_text(self) -> &'static str {
		match self {
			Self::None => "none",
			Self::Viewer => "viewer",
			Self::Editor => "editor",
			Self::Admin => "admin",
		}
	}
}

impl std::fmt::Display for Role {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_text())
	}
}

/// What a statement or a retraction of a governing change touches, which its author's role
/// must allow, besides listings, revocations and roles, which have rules of their own.
#[derive(Clone, Copy, Debug)]
pub(super) enum Touch {
	/// A field of a replica's record that its member states: its key, encryption key, owner,
	/// listing and mark, and its secrets encryption key and source.
	Record(EntityId),
	/// What every member states: a key given to a replica, a space key or a secrets key; an
	/// anchor of a group's changes, and a group's first key; and, in a group, a request that
	/// it join a space.
	Gift,
	/// What only an admin states: acceptances, fields secret for good, members' user IDs, a
	/// user's spaces, and that a replica takes the secrets from itself, which sets them up.
	Admin,
	/// A retraction of statements of an entity, or of any, of the earlier form.
	Retraction(Option<EntityId>),
	/// A claim to the admin role, on a member's record, which only that member makes, as an
	/// editor.
	Claim(EntityId),
	/// A recovery code, or its revocation, on this record, naming this recovery key: only the
	/// replicas of the member whose record for the code it is state either.
	Code(EntityId, VerifyingKey),
	/// A device's choice of the space, on its record there: only the replicas of its member
	/// carry it, an admin of another member's not even.
	Choice(EntityId),
}

impl Touch {
	/// What an operation of a governing change touches: what its statement does, as
	/// [`Touch::of`] says, with what it replaces, which is changed as a retraction would
	/// change it; or what it retracts; or, for an operation of an unknown kind, whatever a
	/// later version may do with it, which is for an admin alone.
	pub(super) fn of_operation(operation: &Operation) -> Vec<Self> {
		match operation {
			Operation::Assert {
				statement,
				supersedes,
				any_entity,
			} => {
				let mut touches: Vec<Self> = Self::of(statement).into_iter().collect();
				if !supersedes.is_empty() {
					touches.push(Self::Retraction((!any_entity).then_some(statement.entity)));
				}
				touches
			}
			Operation::Retract { entity, .. } => vec![Self::Retraction(*entity)],
			// A governing change holds none, as no statement that governs is secret.
			Operation::RetractSecret { .. } | Operation::Unknown(_) => vec![Self::Admin],
		}
	}

	/// What a statement of a governing change touches, if not a role or a revocation.
	pub(super) fn of(statement: &Statement) -> Option<Self> {
		let field = &statement.field;
		let records = [
			fields::SIGNING_KEY,
			fields::ENCRYPTION_KEY,
			fields::OWNER,
			fields::LISTING,
			fields::AI_AGENT,
			fields::SECRETS_ENCRYPTION_KEY,
			fields::SECRETS_FROM,
			fields::RECOVERY_LISTING,
		];
		if *field == fields::ROLE || *field == fields::REVOKED {
			None
		} else if *field == fields::CLAIM {
			Some(Self::Claim(statement.entity))
		} else if *field == fields::RECOVERY_KEY
			|| *field == fields::RECOVERY_REVOKED
			|| *field == fields::RECOVERY_ENTRY
		{
			// A member's own replicas state their member's codes, revoke them, and state the
			// entries that they signed, on the records derived for that member; a text that
			// names no key is for a later version.
			let text = statement.value.as_text();
			let key = if *field == fields::RECOVERY_KEY {
				text.and_then(RecoveryStatement::from_text)
					.map(|stated| stated.key)
			} else if *field == fields::RECOVERY_ENTRY {
				text.and_then(RecoveryEntry::from_text)
					.map(|entry| entry.key)
			} else {
				text.and_then(VerifyingKey::from_text)
			};
			Some(key.map_or(Self::Admin, |key| Self::Code(statement.entity, key)))
		} else if *field == fields::SPACE_CHOICE {
			Some(Self::Choice(statement.entity))
		} else if *field == fields::SEEN {
			// Every member shows, on their own records, that they saw a claim or a rejection.
			Some(Self::Record(statement.entity))
		} else if *field == fields::KEPT_OUT {
			// Every member keeps their own keys out, leaving the space and staying in the
			// group; only an admin keeps others out, or lets anyone back.
			Some(if statement.value == nibkeep_model::Value::Boolean(true) {
				Self::Record(statement.entity)
			} else {
				Self::Admin
			})
		} else if *field == fields::SECRETS_FROM
			&& statement.value.as_reference() == Some(statement.entity)
		{
			// Taking the secrets from itself sets them up, which only an admin does.
			Some(Self::Admin)
		} else if records.contains(field) {
			Some(Self::Record(statement.entity))
		} else if *field == fields::SPACE_KEY
			|| *field == fields::SECRETS_KEY
			|| *field == fields::ANCHOR
			|| *field == fields::GROUP_KEY
			|| *field == fields::JOIN_REQUEST
		{
			// Every member gives keys, and anchors the groups' changes it knows.
			Some(Self::Gift)
		} else {
			Some(Self::Admin)
		}
	}
}

/// The member that a key belongs to at a governing change, as far as decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
	/// Not decided yet, as the listing that decides it is not.
	Undecided,
	/// No counting listing that the change builds on names one.
	Nobody,
	/// This member.
	Member(EntityId),
}

/// The person whose key signed a governing change, as far as decided.
#[derive(Clone, Debug)]
enum Who {
	/// Not decided yet, as a listing or a revocation that decides it is not.
	Undecided,
	/// No counting listing that the change builds on names the key, or it is revoked.
	Nobody,
	/// This person.
	Person(Person),
}

/// Whether what lowers a governing change's author's role, without its knowing, leaves it
/// as it was, as far as decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kept {
	/// Not decided yet.
	Undecided,
	/// Some path still allows it.
	Yes,
	/// Every path that allowed it is lowered.
	No,
}

/// What a governing change needs of its author's member's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
	/// Not decided yet, as a change it depends on is not.
	Undecided,
	/// Nobody's role allows it, such as listing another member's replica.
	Never,
	/// At least this role.
	At(Role),
}

/// Whether a governing change's author's role allows it, as far as decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
	/// Not decided yet.
	Undecided,
	/// No role of the author's allows it.
	Refused,
	/// The role of the author's member allows it, which needs at least `need`.
	Allowed {
		/// The author's member.
		member: EntityId,
		/// What the change needs of the member's role.
		need: Role,
	},
}

/// What deciding the governing changes of a space with members needs besides their
/// listings and revocations: which facts each builds on, and who became an admin when.
pub(super) struct Members<'a> {
	facts: &'a [Fact],
	/// For each fact, the facts it builds on, by index: all come before it.
	ancestors: &'a [BTreeSet<usize>],
	/// When each member became an admin: the order of the earliest change that made them
	/// one, whose author was an admin before it, back to the space's first change, whether
	/// those changes count or not.
	admin_since: BTreeMap<EntityId, (Timestamp, ChangeId)>,
	/// What the groups that the space follows give in it, if it follows any.
	reach: Option<&'a Reach>,
	/// The space.
	space: SpaceId,
	/// For every key that a fact revokes, the facts that revoke it.
	revoking: BTreeMap<KeyBytes, Vec<usize>>,
	/// Each fact's index, by its ID.
	index: BTreeMap<ChangeId, usize>,
	/// What each fact knows of the groups, and the keys that the groups in the space list
	/// there, found once for each fact, as they depend on the anchors taken to count alone.
	groups: RefCell<BTreeMap<usize, Rc<ThroughGroups>>>,
	/// Each fact's author's person, once decided, while the labels being decided are the
	/// same ([`Members::forget_decided`]).
	people: RefCell<BTreeMap<usize, Who>>,
	/// The facts that claim the admin role for each record.
	claims: BTreeMap<EntityId, Vec<usize>>,
	/// The keys whose choices of the space any fact states, so that deciding a fact by its
	/// author's choices costs nothing where nobody chose.
	chosen: BTreeSet<KeyBytes>,
}

/// What a fact knows of the groups, and the routes by which the groups in the space give keys
/// roles there.
struct ThroughGroups {
	knowledge: Knowledge,
	routes: Routes,
}

impl<'a> Members<'a> {
	/// For `facts` in their order, each of which builds on those `ancestors` names, with the
	/// member that the first listing of each key names, as [`super::listings`] finds it. A
	/// later listing that names another member, as when a member comes back, changes a key's
	/// member for seniority only if its author had become an admin before it.
	pub(super) fn new(
		facts: &'a [Fact],
		ancestors: &'a [BTreeSet<usize>],
		first_owner: &BTreeMap<KeyBytes, EntityId>,
		(space, reach): (SpaceId, Option<&'a Reach>),
	) -> Self {
		let mut admin_since: BTreeMap<EntityId, (Timestamp, ChangeId)> = BTreeMap::new();
		let mut owner = first_owner.clone();
		for fact in facts {
			// A claimant becomes an admin no earlier than 60 days after the claim, when it
			// may be settled: junior to every admin before, and earlier than those they make.
			for (record, _) in &fact.claims {
				let settled = after(fact.order.0, 2 * WAIT_MILLIS);
				admin_since.entry(*record).or_insert((settled, fact.id));
			}
			let author = if fact.root {
				fact.owner_of(&fact.author)
			} else {
				owner.get(&fact.author).copied()
			};
			let was_admin = fact.root
				|| author.is_some_and(|author| {
					admin_since
						.get(&author)
						.is_some_and(|&since| since < fact.order)
				});
			if !was_admin {
				continue;
			}
			for (member, role) in &fact.roles {
				if *role == Role::Admin {
					admin_since.entry(*member).or_insert(fact.order);
				}
			}
			for (index, (_, key)) in fact.lists.iter().enumerate() {
				if let Some(listed) = fact.owners[index] {
					owner.insert(*key, listed);
				}
			}
		}
		let mut revoking: BTreeMap<KeyBytes, Vec<usize>> = BTreeMap::new();
		for (at, fact) in facts.iter().enumerate() {
			for key in &fact.revokes {
				revoking.entry(*key).or_default().push(at);
			}
		}
		Self {
			facts,
			ancestors,
			admin_since,
			reach,
			space,
			revoking,
			index: facts
				.iter()
				.enumerate()
				.map(|(at, fact)| (fact.id, at))
				.collect(),
			groups: RefCell::default(),
			people: RefCell::default(),
			chosen: facts
				.iter()
				.flat_map(|fact| fact.choices().iter().map(|(key, ..)| *key))
				.collect(),
			claims: {
				let mut claims: BTreeMap<EntityId, Vec<usize>> = BTreeMap::new();
				for (at, fact) in facts.iter().enumerate() {
					for (record, _) in &fact.claims {
						claims.entry(*record).or_default().push(at);
					}
				}
				claims
			},
		}
	}

	/// When a member became an admin, for comparing two admins: a member with no such time,
	/// as one made one by a change whose author's member is unknown, comes after every other.
	fn seniority(&self, member: EntityId) -> (bool, Option<(Timestamp, ChangeId)>) {
		let since = self.admin_since.get(&member).copied();
		(since.is_none(), since)
	}

	/// Forgets the people found for facts, as the labels they were decided by are to be
	/// decided anew.
	pub(super) fn forget_decided(&self) {
		self.people.borrow_mut().clear();
	}

	/// What fact `at` knows of the groups, and the keys that the groups in the space list
	/// there: found once.
	fn through_groups(&self, at: usize) -> Option<Rc<ThroughGroups>> {
		let reach = self.reach?;
		if let Some(found) = self.groups.borrow().get(&at) {
			return Some(Rc::clone(found));
		}
		let known: Vec<ChangeId> = self.ancestors[at]
			.iter()
			.map(|&earlier| self.facts[earlier].id)
			.collect();
		let own = self.facts[at].id;
		let knowledge = reach.knowledge(&known, reach.is_anchor(&own).then_some(own));
		let made = |change: &ChangeId| self.builds_on_or_is(at, change);
		let groups = reach.groups_in(&knowledge, made);
		let routes = reach.routes(&knowledge, &groups);
		let found = Rc::new(ThroughGroups { knowledge, routes });
		self.groups.borrow_mut().insert(at, Rc::clone(&found));
		Some(found)
	}

	/// Whether a key joins records at fact `at`, as one revoked by no counting fact that `at`
	/// builds on, and by no group by what it knows, does; `None` while one of those facts is
	/// undecided.
	fn joins(
		&self,
		key: &KeyBytes,
		at: usize,
		labels: &[Label],
		knowledge: &Knowledge,
	) -> Option<bool> {
		if self
			.reach
			.is_some_and(|reach| reach.revoked(key, knowledge))
		{
			return Some(false);
		}
		let mut joins = true;
		for &revocation in self.revoking.get(key).into_iter().flatten() {
			if !self.ancestors[at].contains(&revocation) {
				continue;
			}
			match labels[revocation] {
				Label::Counts => joins = false,
				Label::Undecided => return None,
				Label::SetAside => {}
			}
		}
		Some(joins)
	}

	/// The person whose key `key` is at fact `at`: by the space's own counting listings
	/// among the facts it builds on, and, in a space that follows groups, by the groups'
	/// listings that it knows through its anchors and its own, joined where a key that is
	/// not revoked is listed on two records. A key that a group revokes, by what it knows,
	/// is nobody's.
	fn who(&self, key: &KeyBytes, at: usize, labels: &[Label]) -> Who {
		if let Some(found) = self.people.borrow().get(&at) {
			return found.clone();
		}
		let who = self.find_who(key, at, labels);
		if !matches!(who, Who::Undecided) {
			self.people.borrow_mut().insert(at, who.clone());
		}
		who
	}

	/// The person whose key `key` is at fact `at`, as [`Members::who`] finds it, without
	/// what it found before. Only the owners of the key and of those that a group lists
	/// matter, as a key that the space alone lists is on one record, and joins none.
	fn find_who(&self, key: &KeyBytes, at: usize, labels: &[Label]) -> Who {
		let Some(through) = self.through_groups(at) else {
			return match self.owner(key, at, labels) {
				Owner::Undecided => Who::Undecided,
				Owner::Nobody => Who::Nobody,
				Owner::Member(member) => Who::Person(Person {
					member,
					direct: BTreeSet::from([member]),
					keys: BTreeSet::from([*key]),
					members: Vec::new(),
				}),
			};
		};
		let knowledge = &through.knowledge;
		if self
			.reach
			.is_some_and(|reach| reach.revoked(key, knowledge))
		{
			return Who::Nobody;
		}
		let mut direct = BTreeMap::new();
		let mut joining = BTreeMap::new();
		for listed in std::iter::once(key).chain(through.routes.listed.keys()) {
			match self.owner(listed, at, labels) {
				Owner::Undecided => return Who::Undecided,
				Owner::Nobody => {}
				Owner::Member(member) => {
					direct.insert(*listed, member);
				}
			}
			match self.joins(listed, at, labels, knowledge) {
				None => return Who::Undecided,
				Some(joins) => {
					joining.insert(*listed, joins);
				}
			}
		}
		let joins = |listed: &KeyBytes| joining.get(listed).copied().unwrap_or(false);
		match person_of(key, &direct, &through.routes.listed, joins) {
			Some(person) => Who::Person(person),
			None => Who::Nobody,
		}
	}

	/// Whether a group in the space lists fact `at`'s author's key, by what it knows of the
	/// groups.
	pub(super) fn listed_through_groups(&self, at: usize) -> bool {
		self.through_groups(at)
			.is_some_and(|through| through.routes.listed.contains_key(&self.facts[at].author))
	}

	/// Whether fact `at` is the change `change` or builds on it.
	fn builds_on_or_is(&self, at: usize, change: &ChangeId) -> bool {
		self.facts[at].id == *change
			|| self.ancestors[at]
				.iter()
				.any(|&earlier| self.facts[earlier].id == *change)
	}

	/// A person's records at fact `at`: those of each of their records in the space, and
	/// the records there of their keys' replicas and devices. `None` while a listing they
	/// depend on is undecided.
	fn records_of_person(
		&self,
		person: &Person,
		at: usize,
		labels: &[Label],
	) -> Option<BTreeSet<EntityId>> {
		let mut records = BTreeSet::from([person.member]);
		for &record in &person.direct {
			records.extend(self.records(record, at, labels)?);
		}
		records.extend(key_records(self.space, &person.keys));
		Some(records)
	}

	/// The roles of a person's paths at fact `at`, by the facts it builds on: of each of
	/// their records in the space, and through each group in the space from which a route
	/// reaches one of their members, the lower of the group's role in the space and the best
	/// that those routes give, never an admin's. `None` while one is undecided.
	fn path_roles(&self, person: &Person, at: usize, labels: &[Label]) -> Option<Vec<Role>> {
		let mut roles = Vec::new();
		for &record in &person.direct {
			roles.push(self.record_role(record, at, labels)?);
		}
		if person.members.is_empty() {
			return Some(roles);
		}
		// A person kept out of the space has no path through a group.
		let kept_out = self.kept_out(person, at, labels)?;
		let through = self.through_groups(at)?;
		let routes = &through.routes;
		for (top, best) in routes.tops.iter().zip(routes.best(&person.members, &[])) {
			let Some(best) = best else {
				continue;
			};
			let group = self.role(top.record, at, labels)?;
			roles.push(if kept_out {
				Role::None
			} else {
				group.min(best).min(Role::Editor)
			});
		}
		Some(roles)
	}

	/// A direct record's role at fact `at`: what the counting facts it builds on give it, or
	/// an admin's by a claim that gives it the role there, while the new admin can only undo
	/// only for what undoes. `None` while one is undecided.
	fn record_role(&self, record: EntityId, at: usize, labels: &[Label]) -> Option<Role> {
		let role = self.role(record, at, labels)?;
		// A claim gives nothing to a member lowered below an editor, or removed, since.
		let Some(claims) = self.claims.get(&record) else {
			return Some(role);
		};
		if role == Role::Admin || role < Role::Editor {
			return Some(role);
		}
		let mut deciding = Deciding {
			facts: self.facts,
			ancestors: self.ancestors,
			counts: |fact: usize| match labels[fact] {
				Label::Counts => Some(true),
				Label::SetAside => Some(false),
				Label::Undecided => None,
			},
			person_of: |fact: usize| match self.who(&self.facts[fact].author, fact, labels) {
				Who::Undecided => None,
				Who::Nobody => Some(None),
				Who::Person(person) => Some(Some(person_records(&person))),
			},
		};
		let order = self.facts[at].order.0;
		for &claim in claims {
			if !self.ancestors[at].contains(&claim) || !(deciding.counts)(claim)? {
				continue;
			}
			let Some(claimant) = (deciding.person_of)(claim)? else {
				continue;
			};
			// The earliest change that `at` builds on and that shows another member saw it.
			let known = self.ancestors[at].iter().copied();
			let Some(&seer) = deciding.seers(claim, &claimant, known, 1)?.first() else {
				continue;
			};
			let Some(undoing_only) = given_at(self.facts[seer].order.0, order) else {
				continue;
			};
			// A withdrawal, or a rejection in time, voids it, by when it was first seen of all.
			let all = (claim + 1)..self.facts.len();
			let first = deciding
				.seers(claim, &claimant, all, 1)?
				.first()
				.map(|&first| self.facts[first].order.0);
			if deciding.void(claim, first)? {
				continue;
			}
			if !undoing_only || !self.undoes_not(at, labels)? {
				return Some(Role::Admin);
			}
		}
		Some(role)
	}

	/// Whether fact `at` does what a new admin by a claim cannot while they can only undo:
	/// an acceptance, a rejection of a claim, making an admin, or lowering a member whom the
	/// facts it builds on make an admin. `None` while one is undecided.
	fn undoes_not(&self, at: usize, labels: &[Label]) -> Option<bool> {
		let fact = &self.facts[at];
		if !fact.accepts.is_empty() || !fact.rejects.is_empty() {
			return Some(true);
		}
		for &(member, role) in &fact.roles {
			if role == Role::Admin || self.role(member, at, labels)? == Role::Admin {
				return Some(true);
			}
		}
		Some(false)
	}

	/// The records in the space, derived from the space and each key of a person, that keep
	/// them out; none where no fact keeps anyone out.
	fn keep_out_records(&self, person: &Person) -> BTreeSet<EntityId> {
		if !self.facts.iter().any(|fact| !fact.keeps_out.is_empty()) {
			return BTreeSet::new();
		}
		keep_out_records(self.space, &person.keys)
	}

	/// Whether a person is kept out of the space through groups at fact `at`: whether, for
	/// the record of one of their keys, of the counting facts that `at` builds on and that say
	/// whether it keeps them out, one of those that no other of them builds on does. `None`
	/// while one is undecided.
	fn kept_out(&self, person: &Person, at: usize, labels: &[Label]) -> Option<bool> {
		let records = self.keep_out_records(person);
		if records.is_empty() {
			return Some(false);
		}
		let mut sayers: BTreeMap<EntityId, Vec<(usize, bool)>> = BTreeMap::new();
		for &earlier in &self.ancestors[at] {
			for (record, keeps) in &self.facts[earlier].keeps_out {
				if !records.contains(record) {
					continue;
				}
				match labels[earlier] {
					Label::Undecided => return None,
					Label::SetAside => {}
					Label::Counts => sayers.entry(*record).or_default().push((earlier, *keeps)),
				}
			}
		}
		Some(sayers.values().any(|said| {
			said.iter().any(|(sayer, keeps)| {
				*keeps
					&& !said
						.iter()
						.any(|(other, _)| self.ancestors[*other].contains(sayer))
			})
		}))
	}

	/// The facts that keep a person out and that fact `at` neither builds on nor is built on
	/// by: they set aside what `at` did through a group, as a removal does.
	fn keeping_out(&self, person: &Person, at: usize) -> Vec<usize> {
		let records = self.keep_out_records(person);
		(0..self.facts.len())
			.filter(|&other| {
				other != at
					&& !self.ancestors[at].contains(&other)
					&& !self.ancestors[other].contains(&at)
					&& self.facts[other]
						.keeps_out
						.iter()
						.any(|(record, keeps)| *keeps && records.contains(record))
			})
			.collect()
	}

	/// Whether a counting fact that `at` builds on, by an admin, made `record` for a member
	/// through a group whose keys include one it names: the person then lists their replicas
	/// on it. `None` while one that would is undecided.
	fn made_for(
		&self,
		record: EntityId,
		person: &Person,
		at: usize,
		labels: &[Label],
	) -> Option<bool> {
		let mut undecided = false;
		for &earlier in &self.ancestors[at] {
			let fact = &self.facts[earlier];
			if !fact
				.made_for
				.iter()
				.any(|(made, key)| *made == record && person.keys.contains(key))
			{
				continue;
			}
			match labels[earlier] {
				Label::Counts => return Some(true),
				Label::Undecided => undecided = true,
				Label::SetAside => {}
			}
		}
		(!undecided).then_some(false)
	}

	/// The member that a key belongs to at fact `at`: the owner that the latest fact it
	/// builds on that lists the key names, if that one counts; or, for a key that no such
	/// fact lists and that lists itself at `at` by a recovery code, the owner that the code
	/// lists it for, whose role it has at once, as the rules count that listing apart.
	fn owner(&self, key: &KeyBytes, at: usize, labels: &[Label]) -> Owner {
		let latest = self.ancestors[at]
			.iter()
			.rev()
			.find(|&&listing| labels[listing] != Label::SetAside && self.facts[listing].lists(key));
		match latest {
			None => {
				let fact = &self.facts[at];
				match fact.by_code {
					Some(by_code) if fact.author == *key => Owner::Member(by_code.owner),
					_ => Owner::Nobody,
				}
			}
			Some(&listing) if labels[listing] == Label::Counts => self.facts[listing]
				.owner_of(key)
				.map_or(Owner::Nobody, Owner::Member),
			Some(_) => Owner::Undecided,
		}
	}

	/// The records of a member at fact `at`: the member's own, and those of the replicas,
	/// and their devices, that the counting facts it builds on, and `at` itself, list for the
	/// member. `None` while one of those listings is undecided.
	fn records(&self, member: EntityId, at: usize, labels: &[Label]) -> Option<BTreeSet<EntityId>> {
		let mut records = BTreeSet::from([member]);
		for &listing in self.ancestors[at].iter().chain([&at]) {
			let fact = &self.facts[listing];
			for (index, (record, _)) in fact.lists.iter().enumerate() {
				if fact.owners[index] != Some(member) {
					continue;
				}
				match labels[listing] {
					Label::SetAside if listing != at => {}
					Label::Undecided if listing != at => return None,
					_ => {
						records.insert(*record);
						records.insert(fact.devices[index]);
					}
				}
			}
		}
		Some(records)
	}

	/// The role that the counting facts that fact `at` builds on give a member: of those
	/// that no other of them builds on, the lowest. `None` while one is undecided.
	fn role(&self, member: EntityId, at: usize, labels: &[Label]) -> Option<Role> {
		let mut givers = Vec::new();
		for &giver in &self.ancestors[at] {
			let Some(role) = self.facts[giver].role_of(member) else {
				continue;
			};
			match labels[giver] {
				Label::Undecided => return None,
				Label::SetAside => {}
				Label::Counts => givers.push((giver, role)),
			}
		}
		Some(self.lowest_of_latest(&givers))
	}

	/// Of facts that give a member roles, the lowest role of those that no other builds on;
	/// none if there are none.
	fn lowest_of_latest(&self, givers: &[(usize, Role)]) -> Role {
		givers
			.iter()
			.filter(|(giver, _)| {
				!givers
					.iter()
					.any(|(other, _)| self.ancestors[*other].contains(giver))
			})
			.map(|(_, role)| *role)
			.min()
			.unwrap_or(Role::None)
	}

	/// Whether the space allows AI agents at fact `at`: whether, of the counting facts that it
	/// builds on that say so, and `at` itself, those that no other of them builds on all
	/// allow them; a space allows none until an admin says it does. `None` while one is
	/// undecided.
	fn allows_ai_agents(&self, at: usize, labels: &[Label]) -> Option<bool> {
		let mut sayers = Vec::new();
		for &earlier in &self.ancestors[at] {
			let Some(allows) = self.facts[earlier].allows_ai else {
				continue;
			};
			match labels[earlier] {
				Label::Undecided => return None,
				Label::SetAside => {}
				Label::Counts => sayers.push((earlier, allows)),
			}
		}
		if let Some(allows) = self.facts[at].allows_ai {
			sayers.push((at, allows));
		}
		let latest: Vec<bool> = sayers
			.iter()
			.filter(|(sayer, _)| {
				!sayers.iter().any(|(other, _)| {
					other != sayer && (*other == at || self.ancestors[*other].contains(sayer))
				})
			})
			.map(|(_, allows)| *allows)
			.collect();
		Some(!latest.is_empty() && latest.iter().all(|allows| *allows))
	}

	/// Whether the device whose key is `key` holds the space at fact `at`: whether, of its
	/// choices that the counting facts it builds on state, the one with the highest number, a
	/// drop winning a tie, holds it, or none does. `None` while a fact that states one is
	/// undecided.
	fn holds_at(&self, key: &KeyBytes, at: usize, labels: &[Label]) -> Option<bool> {
		if !self.chosen.contains(key) {
			return Some(true);
		}
		let mut choices = Vec::new();
		for &earlier in &self.ancestors[at] {
			let mut of_key = self.facts[earlier]
				.choices()
				.iter()
				.filter(|(chooser, ..)| chooser == key)
				.peekable();
			if of_key.peek().is_none() {
				continue;
			}
			match labels[earlier] {
				Label::Undecided => return None,
				Label::SetAside => {}
				Label::Counts => {
					choices.extend(of_key.map(|(_, number, holds)| (*number, *holds)));
				}
			}
		}
		Some(standing(choices).is_none_or(|(_, holds)| holds))
	}

	/// Whether no fact that fact `at` builds on, counting or not, makes `member` a member's
	/// record, by its user ID or a role.
	fn is_new(&self, member: EntityId, at: usize) -> bool {
		!self.ancestors[at].iter().any(|&earlier| {
			let fact = &self.facts[earlier];
			fact.users.contains(&member)
				|| fact.role_of(member).is_some()
				|| fact.makes_group_record(member)
		})
	}

	/// Whether a record is a group's, by fact `at` or a fact it builds on.
	fn is_group_record(&self, record: EntityId, at: usize) -> bool {
		self.facts[at].makes_group_record(record)
			|| self.ancestors[at]
				.iter()
				.any(|&earlier| self.facts[earlier].makes_group_record(record))
	}

	/// What fact `at` needs of its author's role, who is `person`.
	fn needs(&self, at: usize, person: &Person, labels: &[Label]) -> Need {
		let Some(records) = self.records_of_person(person, at, labels) else {
			return Need::Undecided;
		};
		let fact = &self.facts[at];
		let mut need = Role::Viewer;
		// An anchor that names a change that the replica holds of no group it follows cannot
		// count yet.
		if self
			.reach
			.is_some_and(|reach| reach.is_incomplete(&fact.id))
		{
			return Need::Never;
		}
		// Only a member's own replicas list its replicas, but for the one that an admin's
		// invitation lists, on the record of the member it makes, which no change it builds on
		// names as a member's.
		for owner in &fact.owners {
			match owner {
				Some(owner) if person.direct.contains(owner) => {}
				Some(owner) if fact.users.contains(owner) && self.is_new(*owner, at) => {
					need = Role::Admin;
				}
				// A member through a group lists their replicas on the record that an admin
				// made for them, which makes them a direct member.
				Some(owner) => match self.made_for(*owner, person, at, labels) {
					None => return Need::Undecided,
					Some(true) => {}
					Some(false) => return Need::Never,
				},
				None => return Need::Never,
			}
		}
		// Only a person's own replicas revoke their replicas, whichever path lists them.
		for key in &fact.revokes {
			if person.keys.contains(key) {
				continue;
			}
			match self.owner(key, at, labels) {
				Owner::Undecided => return Need::Undecided,
				Owner::Member(owner) if person.direct.contains(&owner) => {}
				_ => return Need::Never,
			}
		}
		// A group is never an admin of a space.
		for (given, role) in &fact.roles {
			if *role == Role::Admin && self.is_group_record(*given, at) {
				return Need::Never;
			}
		}
		// An AI agent's replica is listed only where the space allows AI agents.
		if !fact.marks.is_empty() {
			match self.allows_ai_agents(at, labels) {
				None => return Need::Undecided,
				Some(false) => return Need::Never,
				Some(true) => {}
			}
		}
		// Every member may leave; only admins give roles.
		for (given, role) in &fact.roles {
			if !(person.direct.contains(given) && *role == Role::None) {
				need = Role::Admin;
			}
		}
		for touch in &fact.touches {
			match touch {
				Touch::Record(entity) | Touch::Retraction(Some(entity))
					if records.contains(entity) => {}
				Touch::Gift => {}
				// Only a direct member claims, on their own record, as an editor.
				Touch::Claim(entity) if person.direct.contains(entity) => {
					need = need.max(Role::Editor);
				}
				// A device's choice is carried only by its member's replicas.
				Touch::Choice(entity) if records.contains(entity) => {}
				Touch::Claim(_) | Touch::Choice(_) => return Need::Never,
				// A code is its member's own: nobody states or revokes another member's, an
				// admin included. A member through a group alone has their record in the
				// group, which the code's record in the space is derived from.
				Touch::Code(record, key) => {
					let derived = |member: &EntityId| {
						crate::recovery_record(self.space, *member, key) == *record
					};
					let own = person.direct.iter().any(derived)
						|| person.members.iter().any(|member| derived(&member.member));
					if !own {
						return Need::Never;
					}
				}
				_ => need = Role::Admin,
			}
		}
		Need::At(need)
	}

	/// Whether fact `at`'s author's role, by the facts it builds on, allows it. The first
	/// change of the space is decided by its listing alone.
	pub(super) fn verdict(&self, at: usize, labels: &[Label]) -> Verdict {
		let fact = &self.facts[at];
		let person = match self.who(&fact.author, at, labels) {
			Who::Undecided => return Verdict::Undecided,
			Who::Nobody => return Verdict::Refused,
			Who::Person(person) => person,
		};
		// A device that dropped the space, by the facts it builds on, governs nothing there.
		match self.holds_at(&fact.author, at, labels) {
			None => return Verdict::Undecided,
			Some(false) => return Verdict::Refused,
			Some(true) => {}
		}
		let need = match self.needs(at, &person, labels) {
			Need::Undecided => return Verdict::Undecided,
			Need::Never => return Verdict::Refused,
			Need::At(need) => need,
		};
		match self.path_roles(&person, at, labels) {
			None => Verdict::Undecided,
			Some(roles) if roles.iter().any(|role| need <= *role) => Verdict::Allowed {
				member: person.member,
				need,
			},
			Some(_) => Verdict::Refused,
		}
	}

	/// Whether what would lower fact `at`'s author's role without its knowing, which its
	/// verdict allowed, leaves it as it was: whether some path that allowed it is lowered
	/// neither by a counting fact that gives a lower role and that `at` neither builds on nor
	/// is built on by, nor by a group change that it does not know and whose first anchors do
	/// not build on it. Of two admins who lower each other so, the one who became an admin
	/// later sets nothing aside.
	pub(super) fn kept(&self, at: usize, labels: &[Label]) -> Kept {
		let Verdict::Allowed { need, .. } = self.verdict(at, labels) else {
			return Kept::Yes;
		};
		let fact = &self.facts[at];
		let Who::Person(person) = self.who(&fact.author, at, labels) else {
			return Kept::Undecided;
		};
		let tri = |lowering: &[usize]| {
			if lowering.iter().any(|&other| labels[other] == Label::Counts) {
				Kept::No
			} else if lowering
				.iter()
				.all(|&other| labels[other] == Label::SetAside)
			{
				Kept::Yes
			} else {
				Kept::Undecided
			}
		};
		let mut paths: Vec<Kept> = Vec::new();
		for &record in &person.direct {
			if self
				.record_role(record, at, labels)
				.is_some_and(|role| need <= role)
			{
				paths.push(tri(&self.lowering_of(at, record, need, labels, true)));
			}
		}
		let (lowerings, through) = match (self.reach, self.through_groups(at)) {
			(Some(reach), Some(through)) => (
				reach.lowerings(
					&fact.author,
					&through.routes,
					&person.members,
					&through.knowledge,
				),
				Some(through),
			),
			_ => (Vec::new(), None),
		};
		let kept_by_first_anchors = |lowering: &Lowering| {
			self.reach.is_none_or(|reach| {
				reach.keeps(lowering, |anchor| {
					self.index
						.get(anchor)
						.is_some_and(|&anchor| self.ancestors[anchor].contains(&at))
				})
			})
		};
		let unkept: Vec<&Lowering> = lowerings
			.iter()
			.filter(|lowering| !kept_by_first_anchors(lowering))
			.collect();
		let keeping_out = self.keeping_out(&person, at);
		// A person kept out by what the fact builds on has no path through a group.
		let through = match self.kept_out(&person, at, labels) {
			Some(false) => through.filter(|_| !person.members.is_empty()),
			Some(true) => None,
			None => {
				paths.push(Kept::Undecided);
				None
			}
		};
		if let Some(through) = through {
			let routes = &through.routes;
			let by = routes.best(&person.members, &[]);
			let lowered = routes.best(&person.members, &unkept);
			for (top, (by, lowered)) in routes.tops.iter().zip(by.into_iter().zip(lowered)) {
				let Some(by) = by else {
					continue;
				};
				let Some(group) = self.role(top.record, at, labels) else {
					paths.push(Kept::Undecided);
					continue;
				};
				if group.min(by).min(Role::Editor) < need {
					continue;
				}
				// A route that allowed it and that nothing in the groups lowers below what it
				// needs keeps it, unless the space lowers the group's record.
				paths.push(if lowered.is_none_or(|lowered| lowered < need) {
					Kept::No
				} else {
					let mut lowering = self.lowering_of(at, top.record, need, labels, false);
					lowering.extend(&keeping_out);
					tri(&lowering)
				});
			}
		}
		let revoked = unkept.iter().any(|lowering| lowering.at.is_none());
		if revoked || paths.iter().all(|kept| *kept == Kept::No) {
			Kept::No
		} else if paths.contains(&Kept::Yes) {
			Kept::Yes
		} else {
			Kept::Undecided
		}
	}

	/// The facts that would set fact `at` aside by lowering a record's role, which its author's
	/// path through it needs, below `need`: those that give the record a lower role, and that
	/// `at` neither builds on nor is built on by. With `senior`, of two admins who lower each
	/// other so, the one who became an admin later sets nothing aside.
	fn lowering_of(
		&self,
		at: usize,
		member: EntityId,
		need: Role,
		labels: &[Label],
		senior: bool,
	) -> Vec<usize> {
		let mut lowering = Vec::new();
		for (other, fact) in self.facts.iter().enumerate() {
			if other == at
				|| self.ancestors[at].contains(&other)
				|| self.ancestors[other].contains(&at)
				|| fact.role_of(member).is_none_or(|role| role >= need)
			{
				continue;
			}
			// Of two that lower each other's authors, the junior admin's sets nothing aside.
			if senior
				&& let Verdict::Allowed {
					member: theirs,
					need: they_need,
				} = self.verdict(other, labels)
				&& self.facts[at]
					.role_of(theirs)
					.is_some_and(|role| role < they_need)
				&& self.seniority(theirs) > self.seniority(member)
			{
				continue;
			}
			lowering.push(other);
		}
		lowering
	}

	/// Of the counting facts, the first in their order that would leave the space with
	/// members and no direct admin, by the facts it builds on and itself; or else, if all
	/// of them together leave none, the latest that took the role from an admin. `None` if
	/// none does.
	pub(super) fn leaving_no_admin(&self, labels: &[Label]) -> Option<usize> {
		let counting = |index: &usize| labels[*index] == Label::Counts;
		for at in (0..self.facts.len()).filter(counting) {
			let fact = &self.facts[at];
			if fact.roles.is_empty() {
				continue;
			}
			let mut roles = self.roles_among(self.ancestors[at].iter().copied().filter(counting));
			for (member, _) in &fact.roles {
				if let Some(role) = fact.role_of(*member) {
					roles.insert(*member, role);
				}
			}
			if without_admin(&roles) {
				return Some(at);
			}
		}
		if !without_admin(&self.roles_among((0..self.facts.len()).filter(counting))) {
			return None;
		}
		(0..self.facts.len()).rev().filter(counting).find(|&at| {
			self.facts[at].roles.iter().any(|(member, role)| {
				*role < Role::Admin && self.role(*member, at, labels) == Some(Role::Admin)
			})
		})
	}

	/// The role of each member that some of `facts` give one to, as [`Members::role`] reckons
	/// it among them.
	fn roles_among(&self, facts: impl Iterator<Item = usize>) -> BTreeMap<EntityId, Role> {
		let mut givers: BTreeMap<EntityId, Vec<(usize, Role)>> = BTreeMap::new();
		for giver in facts {
			for (member, _) in &self.facts[giver].roles {
				if let Some(role) = self.facts[giver].role_of(*member) {
					let of_member = givers.entry(*member).or_default();
					if !of_member.contains(&(giver, role)) {
						of_member.push((giver, role));
					}
				}
			}
		}
		givers
			.into_iter()
			.map(|(member, givers)| (member, self.lowest_of_latest(&givers)))
			.collect()
	}
}

/// Whether roles leave a space with members but no admin.
fn without_admin(roles: &BTreeMap<EntityId, Role>) -> bool {
	roles.values().any(|role| *role != Role::None)
		&& !roles.values().any(|role| *role == Role::Admin)
}

/// What the counting governing changes of a space with members say of members, keys and
/// roles, with which governing changes each builds on: what deciding an ordinary change by
/// its author's role needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
	/// The signed listings of counting changes, in their order: the change, the key, its
	/// owner, its record and its device's record.
	listings: Vec<(ChangeId, KeyBytes, EntityId, EntityId, EntityId)>,
	/// The roles that counting changes give, in their order: the change, the member and
	/// the role.
	roles: Vec<(ChangeId, EntityId, Role)>,
	/// For each governing change that the replica can read, those it builds on.
	ancestors: BTreeMap<ChangeId, BTreeSet<ChangeId>>,
	/// For each governing change that the replica can read, counting or not, the records
	/// of members and of replicas and their devices that it names: by user IDs, roles and
	/// listings.
	records: BTreeMap<ChangeId, BTreeSet<EntityId>>,
	/// Whether counting changes say that the space allows AI agents, in their order.
	ai_allowed: Vec<(ChangeId, bool)>,
	/// For each governing change that the replica can read, counting or not, the keys it
	/// lists.
	listed_keys: BTreeMap<ChangeId, Vec<KeyBytes>>,
	/// The keys that counting changes revoke, with the changes, in their order.
	revocations: Vec<(ChangeId, KeyBytes)>,
	/// What counting changes say of keeping people out, in their order: the change, the
	/// record derived from the space and a key, and whether it keeps the key's person out.
	keep_outs: Vec<(ChangeId, EntityId, bool)>,
	/// What each counting claim to the admin role gives.
	claims: Vec<ClaimEffect>,
	/// The devices' choices of the space that counting changes state, in their order: the
	/// change, the key, the number and whether it holds the space.
	choices: Vec<(ChangeId, KeyBytes, u64, bool)>,
	/// The space, once known.
	space: Option<SpaceId>,
	/// What the groups that the space follows give in it, if it follows any.
	reach: Option<Reach>,
}

/// A key's person at a change, with their records and roles: what deciding an ordinary
/// change by its author's role needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Standing {
	/// Their member: the space's own record that lists their key, if one does; or else the
	/// smallest of their direct records, or else of their records in groups.
	pub member: EntityId,
	/// Their records in the space itself.
	pub direct: BTreeSet<EntityId>,
	/// The keys on their records that groups list too, and the key asked about.
	pub keys: BTreeSet<KeyBytes>,
	/// Their records: those of their records in the space, and the records there of their
	/// keys' replicas and devices.
	pub records: BTreeSet<EntityId>,
	/// What the change knows of the groups.
	pub knowledge: Knowledge,
	/// Their role by what the change builds on: the highest over every path.
	pub by_ancestors: Role,
	/// That, lowered by what lowers each path without the change's knowing.
	pub lowered: Role,
}

impl Standing {
	/// Whether it is the same person's as `other`: the same member, or records or keys in
	/// common, which join records into one person.
	#[must_use]
	pub fn is_same_person(&self, other: &Self) -> bool {
		self.member == other.member
			|| !self.direct.is_disjoint(&other.direct)
			|| !self.keys.is_disjoint(&other.keys)
	}
}

impl View {
	/// What the counting ones of `facts` say, where each builds on those `ancestors` names.
	pub(super) fn of(
		facts: &[Fact],
		ancestors: &[BTreeSet<usize>],
		counting: &BTreeSet<ChangeId>,
	) -> Self {
		let mut view = Self {
			ancestors: facts
				.iter()
				.zip(ancestors)
				.map(|(fact, of)| (fact.id, of.iter().map(|&index| facts[index].id).collect()))
				.collect(),
			..Self::default()
		};
		for fact in facts {
			let records = view.records.entry(fact.id).or_default();
			records.extend(fact.lists.iter().map(|(record, _)| *record));
			records.extend(fact.devices.iter().copied());
			records.extend(fact.roles.iter().map(|(member, _)| *member));
			records.extend(fact.users.iter().copied());
			records.extend(fact.group_records.iter().map(|(record, ..)| *record));
			view.listed_keys
				.insert(fact.id, fact.lists.iter().map(|(_, key)| *key).collect());
		}
		for fact in facts.iter().filter(|fact| counting.contains(&fact.id)) {
			for (index, (record, key)) in fact.lists.iter().enumerate() {
				if let Some(owner) = fact.owners[index] {
					view.listings
						.push((fact.id, *key, owner, *record, fact.devices[index]));
				}
			}
			for (member, role) in &fact.roles {
				view.roles.push((fact.id, *member, *role));
			}
			if let Some(allows) = fact.allows_ai {
				view.ai_allowed.push((fact.id, allows));
			}
			view.revocations
				.extend(fact.revokes.iter().map(|key| (fact.id, *key)));
			view.keep_outs.extend(
				fact.keeps_out
					.iter()
					.map(|(record, keeps)| (fact.id, *record, *keeps)),
			);
			view.choices.extend(
				fact.choices()
					.iter()
					.map(|(key, number, holds)| (fact.id, *key, *number, *holds)),
			);
		}
		view
	}

	/// The same, with what each counting claim to the admin role among `facts` gives, once
	/// the view knows the groups the space follows, as whose a change is depends on them.
	#[must_use]
	pub fn with_claims(
		mut self,
		facts: &[Fact],
		ancestors: &[BTreeSet<usize>],
		counting: &BTreeSet<ChangeId>,
	) -> Self {
		if facts.iter().all(|fact| fact.claims.is_empty()) {
			return self;
		}
		let empty = BTreeSet::new();
		let claims = Deciding {
			facts,
			ancestors,
			counts: |fact: usize| Some(counting.contains(&facts[fact].id)),
			person_of: |fact: usize| {
				let known = self.ancestors.get(&facts[fact].id).unwrap_or(&empty);
				Some(
					self.standing(&facts[fact].author, known, NOW, |_| true)
						.map(|standing| {
							standing
								.direct
								.iter()
								.copied()
								.chain([standing.member])
								.collect()
						}),
				)
			},
		}
		.effects()
		.unwrap_or_default();
		self.claims = claims;
		self
	}

	/// What each counting claim to the admin role gives.
	#[must_use]
	pub fn claims(&self) -> &[ClaimEffect] {
		&self.claims
	}

	/// Whether a claim makes a direct record an admin's at a change timestamped `at` that
	/// builds on the governing changes `known`: `Some(true)` while the new admin can only
	/// undo.
	fn claim_gives(
		&self,
		record: EntityId,
		known: &BTreeSet<ChangeId>,
		at: Timestamp,
	) -> Option<bool> {
		self.claims
			.iter()
			.filter(|effect| effect.claimant == record && known.contains(&effect.claim))
			.filter_map(|effect| effect.gives_at(at, |change| known.contains(change)))
			.min()
	}

	/// The counting changes among `among` that keep out the person whose keys are `keys`:
	/// for the record of each key, of the changes that say whether it keeps them out, those
	/// that no other of them builds on and that keep them out.
	fn keeping_out_among(
		&self,
		keys: &BTreeSet<KeyBytes>,
		mut among: impl FnMut(&ChangeId) -> bool,
	) -> Vec<ChangeId> {
		let Some(space) = self.space.filter(|_| !self.keep_outs.is_empty()) else {
			return Vec::new();
		};
		let records = keep_out_records(space, keys);
		let mut keeping = Vec::new();
		for record in &records {
			let said: Vec<(ChangeId, bool)> = self
				.keep_outs
				.iter()
				.filter(|(change, of, _)| of == record && among(change))
				.map(|(change, _, keeps)| (*change, *keeps))
				.collect();
			keeping.extend(
				self.latest(&said)
					.filter(|(_, keeps)| *keeps)
					.map(|(change, _)| change),
			);
		}
		keeping
	}

	/// Whether a counting change that keeps out the person whose keys are `keys` is unknown to
	/// a change that builds on the governing changes `known`, and `concurrent` says does not
	/// build on it either: it sets aside what the change did through a group.
	fn kept_out_unknowing(
		&self,
		keys: &BTreeSet<KeyBytes>,
		known: &BTreeSet<ChangeId>,
		mut concurrent: impl FnMut(ChangeId) -> bool,
	) -> bool {
		let Some(space) = self.space.filter(|_| !self.keep_outs.is_empty()) else {
			return false;
		};
		let records = keep_out_records(space, keys);
		self.keep_outs.iter().any(|(change, record, keeps)| {
			*keeps && records.contains(record) && !known.contains(change) && concurrent(*change)
		})
	}

	/// The counting changes that keep out a key's person, whom nothing else gives a role now:
	/// those that end the key, which a new key is written with only if its making builds on.
	#[must_use]
	pub fn keep_outs_ending(&self, key: &KeyBytes) -> Vec<ChangeId> {
		if self.keep_outs.is_empty() {
			return Vec::new();
		}
		let all: BTreeSet<ChangeId> = self.ancestors.keys().copied().collect();
		match self.standing(key, &all, NOW, |_| true) {
			Some(standing) if standing.by_ancestors == Role::None => {
				self.keeping_out_among(&standing.keys, |_| true)
			}
			_ => Vec::new(),
		}
	}

	/// Whether a key's person has a role now, by every governing change, over any path.
	#[must_use]
	pub fn has_role_now(&self, key: &KeyBytes) -> bool {
		self.key_role_now(key) > Role::None
	}

	/// The role that a key's changes have now, by every governing change: through its
	/// listing's member, directly or through groups; none for a key that no counting listing
	/// names.
	#[must_use]
	pub fn key_role_now(&self, key: &KeyBytes) -> Role {
		let all: BTreeSet<ChangeId> = self.ancestors.keys().copied().collect();
		self.standing(key, &all, NOW, |_| true)
			.map_or(Role::None, |standing| standing.by_ancestors)
	}

	/// The same, for a space that follows groups, which `reach` says what they give in.
	#[must_use]
	pub fn with_reach(mut self, space: SpaceId, reach: Option<Reach>) -> Self {
		self.space = Some(space);
		self.reach = reach;
		self
	}

	/// What the groups that the space follows give in it, if it follows any.
	#[must_use]
	pub fn reach(&self) -> Option<&Reach> {
		self.reach.as_ref()
	}

	/// The keys that the counting listings among the governing changes `known` list, each
	/// with the owner its latest such listing names.
	#[must_use]
	pub fn listed_among(&self, known: &BTreeSet<ChangeId>) -> BTreeMap<KeyBytes, EntityId> {
		let mut listed = BTreeMap::new();
		for (change, key, owner, ..) in &self.listings {
			if known.contains(change) {
				listed.insert(*key, *owner);
			}
		}
		listed
	}

	/// The keys that the governing changes `known` list, counting or not.
	pub fn listed_any_among<'s>(
		&'s self,
		known: &'s BTreeSet<ChangeId>,
	) -> impl Iterator<Item = KeyBytes> + 's {
		known
			.iter()
			.filter_map(|change| self.listed_keys.get(change))
			.flatten()
			.copied()
	}

	/// The roles that counting changes give a member, with the changes.
	#[must_use]
	pub fn roles_of(&self, member: EntityId) -> Vec<(ChangeId, Role)> {
		self.roles
			.iter()
			.filter(|(_, given, _)| *given == member)
			.map(|(change, _, role)| (*change, *role))
			.collect()
	}

	/// A direct record's role at a change that builds on the governing changes `known`, at
	/// `at`, as [`View::role`] gives it, or an admin's by a claim that gives it the role
	/// there, but for a purge while the new admin can only undo.
	fn direct_role(
		&self,
		record: EntityId,
		known: &BTreeSet<ChangeId>,
		at: Moment,
		concurrent: impl FnMut(ChangeId) -> bool,
	) -> (Role, Role) {
		let (by_ancestors, lowered) = self.role(record, known, concurrent);
		// A claim gives nothing to a member lowered below an editor, or removed, since; and
		// one lowered so without the change's knowing loses it there too.
		if by_ancestors < Role::Editor {
			return (by_ancestors, lowered);
		}
		let (timestamp, purges) = at;
		let claimed = match self.claim_gives(record, known, timestamp) {
			Some(false) => true,
			Some(true) => !purges,
			None => false,
		};
		if !claimed {
			return (by_ancestors, lowered);
		}
		let lowered = if lowered < Role::Editor {
			lowered
		} else {
			Role::Admin
		};
		(Role::Admin, lowered)
	}

	/// A key's person at a change that builds on the governing changes `known`, with their
	/// records, their role by what it builds on, and that lowered by what it does not know:
	/// the counting changes that lower a path's record and that do not build on it either,
	/// and the group changes it does not know, unless one of their first anchors builds on
	/// it, where `had(change)` says whether a governing change builds on it. `None` if no
	/// counting listing among them names the key, or a group revokes it by what the change
	/// knows.
	pub fn standing(
		&self,
		key: &KeyBytes,
		known: &BTreeSet<ChangeId>,
		at: Moment,
		mut had: impl FnMut(ChangeId) -> bool,
	) -> Option<Standing> {
		let Some(reach) = &self.reach else {
			let member = self.owner(key, known)?;
			let (by_ancestors, lowered) = self.direct_role(member, known, at, |giver| !had(giver));
			return Some(Standing {
				member,
				direct: BTreeSet::from([member]),
				keys: BTreeSet::from([*key]),
				records: self.records_of(member, known),
				knowledge: Knowledge::new(),
				by_ancestors,
				lowered,
			});
		};
		let knowledge = reach.knowledge(known, None);
		if reach.revoked(key, &knowledge) {
			return None;
		}
		let groups = reach.groups_in(&knowledge, |change| known.contains(change));
		let routes = reach.routes(&knowledge, &groups);
		let through = &routes.listed;
		// Only the owners of the key and of those that a group lists matter, as a key that
		// the space alone lists is on one record, and joins none.
		let listed = self.listed_among(known);
		let direct: BTreeMap<KeyBytes, EntityId> = std::iter::once(key)
			.chain(through.keys())
			.filter_map(|key| listed.get(key).map(|owner| (*key, *owner)))
			.collect();
		let joins = |key: &KeyBytes| {
			!reach.revoked(key, &knowledge)
				&& !self
					.revocations
					.iter()
					.any(|(change, revoked)| revoked == key && known.contains(change))
		};
		let person = person_of(key, &direct, through, joins)?;
		let mut records = BTreeSet::from([person.member]);
		let mut by_ancestors = Role::None;
		let mut lowered = Role::None;
		for &record in &person.direct {
			records.extend(self.records_of(record, known));
			let (by, low) = self.direct_role(record, known, at, |giver| !had(giver));
			by_ancestors = by_ancestors.max(by);
			lowered = lowered.max(low);
		}
		let lowerings = reach.lowerings(key, &routes, &person.members, &knowledge);
		let unkept: Vec<&Lowering> = lowerings
			.iter()
			.filter(|lowering| !reach.keeps(lowering, |anchor| had(*anchor)))
			.collect();
		let revoked = unkept.iter().any(|lowering| lowering.at.is_none());
		// A person kept out has no path through a group; and one kept out unknowingly, none
		// for what they did then.
		let kept_out = !person.members.is_empty()
			&& !self
				.keeping_out_among(&person.keys, |change| known.contains(change))
				.is_empty();
		let kept_out_unknowing = !person.members.is_empty()
			&& self.kept_out_unknowing(&person.keys, known, |change| !had(change));
		if !kept_out && !person.members.is_empty() {
			let by = routes.best(&person.members, &[]);
			let low = routes.best(&person.members, &unkept);
			for (top, (by, low)) in routes.tops.iter().zip(by.into_iter().zip(low)) {
				let (Some(best), Some(in_group)) = (by, low) else {
					continue;
				};
				let (group_by, group_low) = self.role(top.record, known, |giver| !had(giver));
				let by = group_by.min(best).min(Role::Editor);
				let low = if kept_out_unknowing {
					Role::None
				} else {
					group_low.min(in_group).min(Role::Editor)
				};
				by_ancestors = by_ancestors.max(by);
				lowered = lowered.max(low);
			}
		}
		if revoked {
			lowered = Role::None;
		}
		if let Some(space) = self.space {
			records.extend(key_records(space, &person.keys));
		}
		Some(Standing {
			member: person.member,
			direct: person.direct,
			keys: person.keys,
			records,
			knowledge,
			by_ancestors,
			lowered,
		})
	}

	/// Whether an entity is the record of a member, or of a listed replica or its device,
	/// by the governing changes `known`, counting or not, or, in a space that follows
	/// groups, of a replica that a group change it knows lists, or its device, by what it
	/// knows of the groups, `knowledge`.
	#[must_use]
	pub fn is_any_record(
		&self,
		entity: &EntityId,
		known: &BTreeSet<ChangeId>,
		knowledge: &Knowledge,
	) -> bool {
		self.is_record(entity, known)
			|| self
				.reach
				.as_ref()
				.is_some_and(|reach| reach.is_listed_record(entity, knowledge))
	}

	/// The keys that groups list in the space now, by every group change that its counting
	/// anchors name: those of the groups in it whose records have a role there, but AI
	/// agents' keys and revoked ones. A replica syncs with them.
	#[must_use]
	pub fn listed_through_groups_now(&self) -> BTreeSet<KeyBytes> {
		self.group_members_now().into_keys().collect()
	}

	/// The keys that the groups in the space, and those inside them, list now, each with its
	/// members there, by every group change that its counting anchors name, through the groups
	/// in it whose records have a role there.
	fn group_members_now(&self) -> BTreeMap<KeyBytes, Vec<GroupMember>> {
		let Some(reach) = &self.reach else {
			return BTreeMap::new();
		};
		let knowledge = reach.knowledge(reach.counting(), None);
		let groups: Vec<_> = reach
			.groups_in(&knowledge, |_| true)
			.into_iter()
			.filter(|(_, record, _)| self.role_now(*record).is_some_and(|role| role > Role::None))
			.collect();
		reach.routes(&knowledge, &groups).listed
	}

	/// The members of the groups that the space follows, at any level, whose keys those
	/// groups give a role in the space now, each with the highest role that one of their keys
	/// has now, as [`View::key_role_now`] says, directly or through groups: so that what is
	/// given to a member through a group alone, such as their recovery codes, is given while
	/// they are in the space.
	#[must_use]
	pub fn group_member_roles_now(&self) -> BTreeMap<EntityId, Role> {
		let mut roles: BTreeMap<EntityId, Role> = BTreeMap::new();
		for (key, members) in self.group_members_now() {
			let role = self.key_role_now(&key);
			for member in members {
				let highest = roles.entry(member.member).or_insert(Role::None);
				*highest = (*highest).max(role);
			}
		}
		roles
	}

	/// Each group that the space follows, at any level, that lists a key now, with the key's
	/// member there, but for AI agents' keys.
	#[must_use]
	pub fn group_members_of(&self, key: &KeyBytes) -> Vec<(SpaceId, EntityId)> {
		self.reach
			.as_ref()
			.map_or_default(|reach| reach.members_of_key(key))
	}

	/// For a key that a group the space follows lists, at any level, the group and the key's
	/// member there: of the groups that routes reach now, the first, as a walk level by level
	/// reaches them, whose member has a role there, or else the first group's by ID that lists
	/// it. `None` for a key that no group lists.
	#[must_use]
	pub fn group_member_of(&self, key: &KeyBytes) -> Option<(SpaceId, EntityId)> {
		let reach = self.reach.as_ref()?;
		self.group_members_now()
			.remove(key)
			.unwrap_or_default()
			.iter()
			.find(|member| member.role > Role::None)
			.map(|member| (member.group, member.member))
			.or_else(|| reach.members_of_key(key).first().copied())
	}

	/// The governing changes that the replica can read.
	pub fn facts(&self) -> impl Iterator<Item = &ChangeId> {
		self.ancestors.keys()
	}

	/// Whether a change is a governing change that the replica can read.
	#[must_use]
	pub fn is_fact(&self, change: &ChangeId) -> bool {
		self.ancestors.contains_key(change)
	}

	/// The governing changes that a governing change builds on.
	#[must_use]
	pub fn ancestors_of(&self, change: &ChangeId) -> Option<&BTreeSet<ChangeId>> {
		self.ancestors.get(change)
	}

	/// The member that a key belongs to at a change that builds on the governing changes
	/// `known`: the owner that the latest counting listing among them names.
	#[must_use]
	pub fn owner(&self, key: &KeyBytes, known: &BTreeSet<ChangeId>) -> Option<EntityId> {
		self.listings
			.iter()
			.rev()
			.find(|(change, listed, ..)| listed == key && known.contains(change))
			.map(|(_, _, owner, ..)| *owner)
	}

	/// A member's records at a change that builds on the governing changes `known`: the
	/// member's own, and those of the replicas, and their devices, that counting listings
	/// among them list for the member.
	#[must_use]
	pub fn records_of(&self, member: EntityId, known: &BTreeSet<ChangeId>) -> BTreeSet<EntityId> {
		let mut records = BTreeSet::from([member]);
		for (change, _, owner, record, device) in &self.listings {
			if *owner == member && known.contains(change) {
				records.insert(*record);
				records.insert(*device);
			}
		}
		records
	}

	/// Whether an entity is the record of a member, or of a listed replica or its device, by
	/// the governing changes `known`, counting or not: so that whose an entity is does not
	/// depend on what a change does not build on.
	#[must_use]
	pub fn is_record(&self, entity: &EntityId, known: &BTreeSet<ChangeId>) -> bool {
		known.iter().any(|change| {
			self.records
				.get(change)
				.is_some_and(|records| records.contains(entity))
		})
	}

	/// Each member that the counting governing changes give a role, with the role they have
	/// by all of them: the lowest of those that no other of them builds on.
	#[must_use]
	pub fn roles_now(&self) -> BTreeMap<EntityId, Role> {
		let all: BTreeSet<ChangeId> = self.ancestors.keys().copied().collect();
		let members: BTreeSet<EntityId> = self.roles.iter().map(|(_, member, _)| *member).collect();
		members
			.into_iter()
			.map(|member| (member, self.role(member, &all, |_| false).0))
			.collect()
	}

	/// A member's role by all the counting governing changes, if one gives them a role: the
	/// lowest of those that no other of them builds on.
	#[must_use]
	pub fn role_now(&self, member: EntityId) -> Option<Role> {
		if !self.roles.iter().any(|(_, given, _)| *given == member) {
			return None;
		}
		let all: BTreeSet<ChangeId> = self.ancestors.keys().copied().collect();
		Some(self.role(member, &all, |_| false).0)
	}

	/// Whether the space allows AI agents now: whether, of the counting changes that say so,
	/// those that no other of them builds on all allow them.
	#[must_use]
	pub fn allows_ai_agents(&self) -> bool {
		let latest: Vec<bool> = self
			.ai_allowed
			.iter()
			.filter(|(change, _)| {
				!self.ai_allowed.iter().any(|(other, _)| {
					self.ancestors
						.get(other)
						.is_some_and(|ancestors| ancestors.contains(change))
				})
			})
			.map(|(_, allows)| *allows)
			.collect();
		!latest.is_empty() && latest.iter().all(|allows| *allows)
	}

	/// Whether an AI agent's change that builds on the governing changes `known` may count by
	/// whether the space allows AI agents: whether, of the counting changes among them that
	/// say so, those that no other of them builds on all allow them; and that, lowered by the
	/// counting ones that allow none, that it does not build on, and that `concurrent` says do
	/// not build on it either, as a removal sets aside what was done without knowing of it.
	pub fn ai_allowed(
		&self,
		known: &BTreeSet<ChangeId>,
		mut concurrent: impl FnMut(ChangeId) -> bool,
	) -> (bool, bool) {
		let among: Vec<(ChangeId, bool)> = self
			.ai_allowed
			.iter()
			.filter(|(change, _)| known.contains(change))
			.copied()
			.collect();
		let latest: Vec<bool> = among
			.iter()
			.filter(|(change, _)| {
				!among.iter().any(|(other, _)| {
					self.ancestors
						.get(other)
						.is_some_and(|ancestors| ancestors.contains(change))
				})
			})
			.map(|(_, allows)| *allows)
			.collect();
		let by_ancestors = !latest.is_empty() && latest.iter().all(|allows| *allows);
		let mut lowered = by_ancestors;
		for (change, allows) in &self.ai_allowed {
			if lowered && !allows && !known.contains(change) && concurrent(*change) {
				lowered = false;
			}
		}
		(by_ancestors, lowered)
	}

	/// Whether the counting changes that say whether the space allows AI agents differ from
	/// those of `before`.
	#[must_use]
	pub fn ai_allowed_differs(&self, before: &Self) -> bool {
		self.ai_allowed != before.ai_allowed
	}

	/// The member that a key belongs to now: the owner that its latest counting listing, in
	/// the order of the log, names.
	#[must_use]
	pub fn owner_now(&self, key: &KeyBytes) -> Option<EntityId> {
		self.listings
			.iter()
			.rev()
			.find(|(_, listed, ..)| listed == key)
			.map(|(_, _, owner, ..)| *owner)
	}

	/// The counting changes that give a member the role none, of those that give them a role
	/// and that no other of those builds on: that removed them, or by which they left, and
	/// that no later role undid.
	#[must_use]
	pub fn removals_of_member(&self, member: EntityId) -> Vec<ChangeId> {
		let givers: Vec<(ChangeId, Role)> = self
			.roles
			.iter()
			.filter(|(_, given, _)| *given == member)
			.map(|(change, _, role)| (*change, *role))
			.collect();
		self.latest(&givers)
			.filter(|(_, role)| *role == Role::None)
			.map(|(change, _)| change)
			.collect()
	}

	/// Of changes that each say something, those that no other of them builds on.
	fn latest<'s, T: Copy + 's>(
		&'s self,
		sayers: &'s [(ChangeId, T)],
	) -> impl Iterator<Item = (ChangeId, T)> + 's {
		sayers
			.iter()
			.filter(|(change, _)| {
				!sayers.iter().any(|(other, _)| {
					self.ancestors
						.get(other)
						.is_some_and(|ancestors| ancestors.contains(change))
				})
			})
			.copied()
	}

	/// Every role that counting changes give, with the change and the member.
	pub fn all_roles(&self) -> impl Iterator<Item = (ChangeId, EntityId, Role)> + '_ {
		self.roles.iter().copied()
	}

	/// The first anchors of every group change that ends paths through the groups in the
	/// space: a new key is written with only if its making builds on each.
	#[must_use]
	pub fn departure_anchors(&self) -> BTreeSet<ChangeId> {
		let Some(reach) = &self.reach else {
			return BTreeSet::new();
		};
		reach
			.departures()
			.iter()
			.flat_map(|departure| reach.first_anchors(&departure.group, &departure.change))
			.collect()
	}

	/// For a key that a group in the space lists, at any level, by what its counting anchors
	/// name, and that has no path in the space now: what ended its paths, the first anchors of
	/// the group changes that removed its member, took a group on its paths out of the group
	/// it was in, revoked it or withdrew a group on its paths, and the space's own changes
	/// that took its groups out. Empty for any other key.
	#[must_use]
	pub fn group_removals_of(&self, key: &KeyBytes) -> Vec<ChangeId> {
		let Some(reach) = &self.reach else {
			return Vec::new();
		};
		let members = reach.members_of_key(key);
		if members.is_empty() {
			return Vec::new();
		}
		let all: BTreeSet<ChangeId> = self.ancestors.keys().copied().collect();
		let standing = self.standing(key, &all, NOW, |_| true);
		if standing
			.as_ref()
			.is_some_and(|standing| standing.by_ancestors > Role::None)
		{
			return Vec::new();
		}
		let passes = reach.passes_to(members.iter().map(|(group, _)| *group));
		let mut removals = BTreeSet::new();
		for departure in reach.departures() {
			let ends = match departure.ends {
				Ends::Member(member) => {
					members.contains(&(departure.group, member))
						|| passes.records.contains(&(departure.group, member))
				}
				Ends::Key(revoked) => revoked == *key,
				Ends::Withdrawal(from) => passes.joinings.contains(&(departure.group, from)),
			};
			if ends {
				removals.extend(reach.first_anchors(&departure.group, &departure.change));
			}
		}
		// Keeping the person out ends their paths too.
		if let Some(standing) = &standing {
			removals.extend(self.keeping_out_among(&standing.keys, |_| true));
		}
		for group in &passes.tops {
			for record in reach.records_of_group(group) {
				if self.role_now(record) == Some(Role::None) {
					removals.extend(self.removals_of_member(record));
				}
			}
		}
		removals.into_iter().collect()
	}

	/// The counting changes that give anyone the role none.
	pub fn all_removals(&self) -> impl Iterator<Item = ChangeId> + '_ {
		self.roles
			.iter()
			.filter(|(_, _, role)| *role == Role::None)
			.map(|(change, ..)| *change)
	}

	/// The counting changes that say that the space allows no AI agents.
	pub fn disallowing(&self) -> impl Iterator<Item = ChangeId> + '_ {
		self.ai_allowed
			.iter()
			.filter(|(_, allows)| !allows)
			.map(|(change, _)| *change)
	}

	/// The counting changes that say that the space allows no AI agents, of those that say
	/// whether it does and that no other of those builds on: those by which it allows none
	/// now.
	#[must_use]
	pub fn disallowing_now(&self) -> Vec<ChangeId> {
		self.latest(&self.ai_allowed)
			.filter(|(_, allows)| !allows)
			.map(|(change, _)| change)
			.collect()
	}

	/// The choices of `key` that counting changes state, each with its change.
	fn choices_of<'s>(
		&'s self,
		key: &'s KeyBytes,
	) -> impl Iterator<Item = (ChangeId, u64, bool)> + 's {
		self.choices
			.iter()
			.filter(move |(_, chooser, ..)| chooser == key)
			.map(|(change, _, number, holds)| (*change, *number, *holds))
	}

	/// Whether the device whose key is `key` holds the space now: whether, of its choices
	/// that counting changes state, the one with the highest number, a drop winning a tie,
	/// holds it, or none states one
	/// ([technical design](../../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[must_use]
	pub fn holds_now(&self, key: &KeyBytes) -> bool {
		self.standing_choice(key).is_none_or(|(_, holds)| holds)
	}

	/// The choice of the device whose key is `key` that stands, of those that counting
	/// changes state: its number and whether it holds the space; none if none states one.
	#[must_use]
	pub fn standing_choice(&self, key: &KeyBytes) -> Option<(u64, bool)> {
		standing(
			self.choices_of(key)
				.map(|(_, number, holds)| (number, holds)),
		)
	}

	/// While the device whose key is `key` does not hold the space, the counting changes that
	/// state its choice that stands, by which it dropped the space; empty while it holds it.
	/// Ordered by ID.
	#[must_use]
	pub fn drops_of(&self, key: &KeyBytes) -> Vec<ChangeId> {
		let Some((number, false)) = self.standing_choice(key) else {
			return Vec::new();
		};
		let drops: BTreeSet<ChangeId> = self
			.choices_of(key)
			.filter(|(_, stated, holds)| *stated == number && !holds)
			.map(|(change, ..)| change)
			.collect();
		drops.into_iter().collect()
	}

	/// Every counting change that states a device's choice not to hold the space.
	pub fn all_drops(&self) -> impl Iterator<Item = ChangeId> + '_ {
		self.choices
			.iter()
			.filter(|(.., holds)| !holds)
			.map(|(change, ..)| *change)
	}

	/// Whether a change by the device whose key is `key`, which builds on the governing
	/// changes `known`, may count by the device's choices: whether, of those that counting
	/// changes among `known` state, the one that stands holds the space, or none does; and
	/// that, lowered by any counting choice not to hold it that outranks that one, is not
	/// among `known`, and that `concurrent` says does not build on the change either, as a
	/// removal sets aside what was done without knowing of it.
	pub fn holds(
		&self,
		key: &KeyBytes,
		known: &BTreeSet<ChangeId>,
		mut concurrent: impl FnMut(ChangeId) -> bool,
	) -> (bool, bool) {
		let stood = standing(
			self.choices_of(key)
				.filter(|(change, ..)| known.contains(change))
				.map(|(_, number, holds)| (number, holds)),
		);
		let by_ancestors = stood.is_none_or(|(_, holds)| holds);
		let mut lowered = by_ancestors;
		for (change, number, holds) in self.choices_of(key) {
			if lowered
				&& !holds
				&& !known.contains(&change)
				&& outranks((number, holds), stood)
				&& concurrent(change)
			{
				lowered = false;
			}
		}
		(by_ancestors, lowered)
	}

	/// Whether the roles that counting changes give, what they say of keeping people out, or
	/// what claims give, differ from those of `before`.
	#[must_use]
	pub fn roles_differ(&self, before: &Self) -> bool {
		let summaries =
			|view: &Self| -> Vec<_> { view.claims.iter().map(ClaimEffect::summary).collect() };
		self.roles != before.roles
			|| self.keep_outs != before.keep_outs
			|| summaries(self) != summaries(before)
	}

	/// Whether a listing that counted in `before` counts no longer.
	#[must_use]
	pub fn lost_listings(&self, before: &Self) -> bool {
		before
			.listings
			.iter()
			.any(|listing| !self.listings.contains(listing))
	}

	/// A member's role at a change that builds on the governing changes `known`: what the
	/// counting ones among them give, and that lowered by the counting ones that give the
	/// member a role, that it does not build on, and that `concurrent` says do not build on
	/// it either.
	pub fn role(
		&self,
		member: EntityId,
		known: &BTreeSet<ChangeId>,
		mut concurrent: impl FnMut(ChangeId) -> bool,
	) -> (Role, Role) {
		let givers: Vec<(ChangeId, Role)> = self
			.roles
			.iter()
			.filter(|(change, given, _)| *given == member && known.contains(change))
			.map(|(change, _, role)| (*change, *role))
			.collect();
		let by_ancestors = givers
			.iter()
			.filter(|(giver, _)| {
				!givers.iter().any(|(other, _)| {
					self.ancestors
						.get(other)
						.is_some_and(|ancestors| ancestors.contains(giver))
				})
			})
			.map(|(_, role)| *role)
			.min()
			.unwrap_or(Role::None);
		let mut lowered = by_ancestors;
		for (change, given, role) in &self.roles {
			if *given == member && *role < lowered && !known.contains(change) && concurrent(*change)
			{
				lowered = *role;
			}
		}
		(by_ancestors, lowered)
	}

	/// The keys whose changes may count less than before: those of members whose roles
	/// changed, and those whose listings did.
	#[must_use]
	pub fn losing(&self, before: &Self) -> BTreeSet<KeyBytes> {
		let roles_of = |view: &Self, member: EntityId| -> Vec<(ChangeId, Role)> {
			view.roles
				.iter()
				.filter(|(_, given, _)| *given == member)
				.map(|(change, _, role)| (*change, *role))
				.collect()
		};
		let mut members: BTreeSet<EntityId> = BTreeSet::new();
		for (_, member, _) in self.roles.iter().chain(&before.roles) {
			if roles_of(self, *member) != roles_of(before, *member) {
				members.insert(*member);
			}
		}
		// Those whose claims give otherwise.
		let summaries =
			|view: &Self| -> Vec<_> { view.claims.iter().map(ClaimEffect::summary).collect() };
		let (now, then) = (summaries(self), summaries(before));
		for summary in now.iter().chain(&then) {
			if !now.contains(summary) || !then.contains(summary) {
				members.insert(summary.1);
			}
		}
		let mut losing = BTreeSet::new();
		for listing in self.listings.iter().chain(&before.listings) {
			let (_, key, owner, ..) = listing;
			if members.contains(owner)
				|| !self.listings.contains(listing)
				|| !before.listings.contains(listing)
			{
				losing.insert(*key);
			}
		}
		// Those whose choices of the space counting changes state otherwise.
		for choice in self.choices.iter().chain(&before.choices) {
			if !self.choices.contains(choice) || !before.choices.contains(choice) {
				losing.insert(choice.1);
			}
		}
		losing
	}
}

/// Of a key's choices of a space, each its number and whether it holds the space, the one
/// that stands: the one with the highest number, a drop winning a tie.
fn standing(choices: impl IntoIterator<Item = (u64, bool)>) -> Option<(u64, bool)> {
	choices
		.into_iter()
		.max_by_key(|(number, holds)| (*number, !*holds))
}

/// Whether a choice outranks the one that stands, `None` for none: by its number, a drop
/// winning a tie.
fn outranks((number, holds): (u64, bool), standing: Option<(u64, bool)>) -> bool {
	standing.is_none_or(|(stood, stood_holds)| (number, !holds) > (stood, !stood_holds))
}

/// The records in a space, derived from the space and each of `keys`, that keep a person out
/// through groups.
fn keep_out_records(space: SpaceId, keys: &BTreeSet<KeyBytes>) -> BTreeSet<EntityId> {
	keys.iter()
		.filter_map(nibkeep_crypto::VerifyingKey::from_bytes)
		.map(|key| crate::replica_record(space, &key))
		.collect()
}

/// When a change was made, and whether it purges anything: what a claim to the admin role
/// gives depends on both.
pub(super) type Moment = (Timestamp, bool);

/// The moment for what holds by the records alone, which purges nothing: before any claim to
/// the admin role takes effect, so that no claim keeps a member who was removed.
pub(super) const NOW: Moment = (Timestamp::new(0, 0), false);

/// The records of a person, for telling whether a change is another member's: their member's
/// record, and their records in the space.
fn person_records(person: &Person) -> Records {
	person
		.direct
		.iter()
		.copied()
		.chain([person.member])
		.collect()
}
