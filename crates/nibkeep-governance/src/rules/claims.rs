//! Claims to the admin role ([technical design: claims]): a direct member who can edit claims
//! the role with a governing change, on their own record. Another member's replica that
//! receives the claim shows that it saw it with a governing change that builds on it, which
//! shows it whether it counts or not, so that seeing depends on who is who alone. At one
//! of the claimant's changes, the claim takes effect 30 days after the earliest such change
//! of another member that the change builds on, if the change is timestamped then or later:
//! so the wait starts when someone else saw the claim, and no claim dated back shortens it.
//! An admin's rejection voids the claim, and what the claimant did as an admin, if another
//! member saw it within 30 days after the claim took effect, however it is dated, the claim's
//! being first seen by the earliest such change of all. In those 30 days the new admin can
//! only undo.
//!
//! [technical design: claims]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#139-claims-to-the-admin-role

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, StatementId};
use nibkeep_time::Timestamp;

use super::Fact;
use nibkeep_base::ChangeId;

/// How long a claim waits after it was seen, and how long after that the new admin can only
/// undo: 30 days, in milliseconds.
pub(super) const WAIT_MILLIS: u64 = 30 * 86_400_000;

/// What a claim to the admin role gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimEffect {
	/// The change that made the claim.
	pub claim: ChangeId,
	/// When the claim was made, by the claimant's timestamp.
	pub made: Timestamp,
	/// The claimant's record.
	pub claimant: EntityId,
	/// The governing changes of other members that build on it, counting or not, in their
	/// order, with their timestamps: each shows that its member saw it.
	pub seers: Vec<(ChangeId, Timestamp)>,
	/// Whether an admin's rejection, seen in time, voids it.
	pub void: bool,
}

impl ClaimEffect {
	/// What decides anything of it: the claim, its claimant, when it was first seen, and
	/// whether it counts for nothing. Another member who sees it later decides nothing, as no
	/// change that came before builds on them.
	pub fn summary(&self) -> (ChangeId, EntityId, Option<Timestamp>, bool) {
		(self.claim, self.claimant, self.seen(), self.void)
	}

	/// When another member first saw it, if one did.
	pub fn seen(&self) -> Option<Timestamp> {
		self.seers.first().map(|(_, seen)| *seen)
	}

	/// When it takes effect: 30 days after it was first seen.
	pub fn effective(&self) -> Option<Timestamp> {
		self.seen().map(|seen| after(seen, WAIT_MILLIS))
	}

	/// When it is settled: 30 days after it took effect.
	pub fn settled(&self) -> Option<Timestamp> {
		self.seen().map(|seen| after(seen, 2 * WAIT_MILLIS))
	}

	/// Whether it makes the claimant an admin at a change timestamped `at`, which builds on
	/// the governing changes that `knows(change)` says it does: if it is not void, the change
	/// knows it, and is timestamped 30 days or more after the earliest change that it knows
	/// saw it. `Some(true)` while the new admin can only undo.
	pub fn gives_at(
		&self,
		at: Timestamp,
		mut knows: impl FnMut(&ChangeId) -> bool,
	) -> Option<bool> {
		if self.void || !knows(&self.claim) {
			return None;
		}
		let &(_, seen) = self.seers.iter().find(|(seer, _)| knows(seer))?;
		given_at(seen, at)
	}
}

/// Whether a claim seen at `seen`, by what a change timestamped `at` knows, makes the
/// claimant an admin there: `Some(true)` while the new admin can only undo.
pub(super) fn given_at(seen: Timestamp, at: Timestamp) -> Option<bool> {
	if at < after(seen, WAIT_MILLIS) {
		return None;
	}
	Some(at < after(seen, 2 * WAIT_MILLIS))
}

/// A timestamp `millis` milliseconds after `at`.
pub(super) fn after(at: Timestamp, millis: u64) -> Timestamp {
	Timestamp::new(at.millis().saturating_add(millis), 0)
}

/// How to decide claims among facts, each building on those that `ancestors` names: whether a
/// fact counts, and the records of its author's person, each `None` while undecided, and
/// the person's records `None` for an author who is nobody.
pub(super) struct Deciding<'a, C, P> {
	pub(super) facts: &'a [Fact],
	pub(super) ancestors: &'a [BTreeSet<usize>],
	pub(super) counts: C,
	pub(super) person_of: P,
}

/// The records of a person: their member's record, and their records in the space.
pub(super) type Records = BTreeSet<EntityId>;

impl<C, P> Deciding<'_, C, P>
where
	C: FnMut(usize) -> Option<bool>,
	P: FnMut(usize) -> Option<Option<Records>>,
{
	/// What each claim among the facts, in their order, gives. `None` while what decides one
	/// is undecided.
	pub(super) fn effects(&mut self) -> Option<Vec<ClaimEffect>> {
		let mut effects = Vec::new();
		for at in 0..self.facts.len() {
			if self.facts[at].claims.is_empty() || !(self.counts)(at)? {
				continue;
			}
			let Some(claimant) = (self.person_of)(at)? else {
				continue;
			};
			for (record, _) in self.facts[at].claims.clone() {
				let seers: Vec<(ChangeId, Timestamp)> = self
					.seers(at, &claimant, (at + 1)..self.facts.len(), usize::MAX)?
					.into_iter()
					.map(|seer| (self.facts[seer].id, self.facts[seer].order.0))
					.collect();
				let seen = seers.first().map(|(_, seen)| *seen);
				let void = self.void(at, seen)?;
				effects.push(ClaimEffect {
					claim: self.facts[at].id,
					made: self.facts[at].order.0,
					claimant: record,
					seers,
					void,
				});
			}
		}
		Some(effects)
	}

	/// Whether the claim that fact `claim` made, first seen at `seen`, counts for nothing:
	/// as a counting change retracts its statement, which withdraws it, or as an admin's
	/// rejection, which another member saw within 30 days after the claim took effect,
	/// however it is dated, voids it. `None` while one is undecided.
	pub(super) fn void(&mut self, claim: usize, seen: Option<Timestamp>) -> Option<bool> {
		let id = self.facts[claim].id;
		let statements: Vec<StatementId> = self.facts[claim]
			.claims
			.iter()
			.map(|(_, statement)| *statement)
			.collect();
		let deadline = seen.map(|seen| after(seen, 2 * WAIT_MILLIS));
		for later in 0..self.facts.len() {
			let fact = &self.facts[later];
			let withdraws = fact
				.retracts
				.iter()
				.any(|statement| statements.contains(statement));
			let rejects = fact.rejects.contains(&id);
			if !(withdraws || rejects) || !(self.counts)(later)? {
				continue;
			}
			if withdraws {
				return Some(true);
			}
			let Some(rejector) = (self.person_of)(later)? else {
				continue;
			};
			let first = self
				.seers(later, &rejector, (later + 1)..self.facts.len(), 1)?
				.first()
				.map(|&seer| self.facts[seer].order.0);
			if first.is_some_and(|first| deadline.is_none_or(|deadline| first <= deadline)) {
				return Some(true);
			}
		}
		Some(false)
	}

	/// Of the facts `among`, in their order, those that build on fact `made` and whose authors
	/// are people other than the one whose records are `maker`, whether they count or not, at
	/// most `limit`: `None` while whose one of those before the last found is is undecided.
	pub(super) fn seers(
		&mut self,
		made: usize,
		maker: &Records,
		among: impl IntoIterator<Item = usize>,
		limit: usize,
	) -> Option<Vec<usize>> {
		let mut seers = Vec::new();
		for later in among {
			if later <= made || !self.ancestors[later].contains(&made) {
				continue;
			}
			if let Some(theirs) = (self.person_of)(later)?
				&& theirs.is_disjoint(maker)
			{
				seers.push(later);
				if seers.len() == limit {
					break;
				}
			}
		}
		Some(seers)
	}
}
