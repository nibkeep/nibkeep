//! Claims to the admin role ([technical design: claims]): when the admins of a space are gone,
//! a direct member who can edit claims the role. Every other member's replica shows that it
//! saw the claim, as soon as it holds it; the claim takes effect 30 days after another
//! member first saw it, unless an admin rejects it in time, and for 30 days after that the new
//! admin can only undo. Once the claim is settled, the claimant's replica states the role on
//! its own record.
//!
//! [technical design: claims]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#139-claims-to-the-admin-role

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::replica_record;
use nibkeep_log::ChangeId;
use nibkeep_model::{Operation, Statement, Value, fields};

use super::Library;
use super::entities::display_name;
use crate::{MemberRole, NodeError};

/// A claim to the admin role in a space, as a program shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimInSpace {
	/// The claimant's record.
	pub claimant: EntityId,
	/// Their name there.
	pub name: Option<String>,
	/// When they made it, in milliseconds since the Unix epoch, by their clock.
	pub made: u64,
	/// When another member's device first saw it, if one did.
	pub seen: Option<u64>,
	/// When it takes effect, 30 days after it was first seen.
	pub effective: Option<u64>,
	/// When it is settled, 30 days after that.
	pub settled: Option<u64>,
	/// Whether an admin's rejection, seen in time, voids it.
	pub rejected: bool,
}

/// What a claim gives a member now, by the wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimNow {
	/// The member is an admin by it, and can only undo.
	Undoing,
	/// The member is an admin by it, for good.
	Settled,
}

impl Library {
	/// The claims to the admin role in a shared space, with their claimants' names.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, and an error if the space
	/// cannot be read.
	pub fn claims(&self, space: SpaceId) -> Result<Vec<ClaimInSpace>, NodeError> {
		self.check_shared(space)?;
		let open = self.space(space)?;
		let log = open.stored.log();
		let state = log.state();
		let mut claims = Vec::new();
		for claim in log.claims(&open.keys)? {
			claims.push(ClaimInSpace {
				claimant: claim.claimant,
				name: display_name(state, &open.types, &self.languages, claim.claimant)?,
				made: claim.made.millis(),
				seen: claim.seen.map(nibkeep_time::Timestamp::millis),
				effective: claim.effective.map(nibkeep_time::Timestamp::millis),
				settled: claim.settled.map(nibkeep_time::Timestamp::millis),
				rejected: claim.rejected,
			});
		}
		Ok(claims)
	}

	/// What a claim gives a member of a shared space now, by the latest wall clock time the
	/// library was given, if it makes them an admin.
	pub(crate) fn claim_now(
		&self,
		space: SpaceId,
		member: EntityId,
	) -> Result<Option<ClaimNow>, NodeError> {
		let now = self.wall.get();
		let open = self.space(space)?;
		let mut best = None;
		for claim in open.stored.log().claims(&open.keys)? {
			if claim.claimant != member || claim.rejected {
				continue;
			}
			match (claim.effective, claim.settled) {
				(_, Some(settled)) if settled.millis() <= now => {
					return Ok(Some(ClaimNow::Settled));
				}
				(Some(effective), _) if effective.millis() <= now => best = Some(ClaimNow::Undoing),
				_ => {}
			}
		}
		Ok(best)
	}

	/// Claims the admin role of a shared space, as a direct member who is an editor there,
	/// when its admins are gone: every member sees the claim, which takes effect 30 days after
	/// another member's device first sees it, unless an admin rejects it, and for 30 days
	/// after that this user can only undo.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::Revoked`] if
	/// this replica is revoked there, [`NodeError::AiAgentMayNot`] for an AI agent's replica,
	/// [`NodeError::ClaimNeedsDirect`] if this replica's user is no direct member there,
	/// [`NodeError::ClaimNeedsEditor`] if they are no editor, and an error if the change
	/// cannot be saved.
	pub fn claim_admin(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.check_claim(space)?;
		let operation = Operation::assert(Statement::new(
			StatementId::random(rng),
			own,
			fields::CLAIM,
			Value::Boolean(true),
		));
		self.commit(space, vec![operation], wall_millis, rng)
	}

	/// Checks that this replica's user may claim the admin role of a shared space now, as
	/// before saying what claiming would do, and returns their member.
	///
	/// # Errors
	///
	/// The errors of [`Library::claim_admin`] but those of saving.
	pub fn check_claim(&self, space: SpaceId) -> Result<EntityId, NodeError> {
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("claim the admin role")?;
		let own = self.member_in(space)?.ok_or(NodeError::ClaimNeedsDirect)?;
		let role = {
			let open = self.space(space)?;
			open.stored
				.log()
				.member_roles(&open.keys)?
				.get(&own)
				.copied()
		};
		if role != Some(MemberRole::Editor) {
			return Err(NodeError::ClaimNeedsEditor);
		}
		Ok(own)
	}

	/// Rejects, as an admin of a shared space, the claims to the admin role that a member
	/// made there: if another member's device sees the rejection within 30 days after a
	/// claim took effect, the claim counts for nothing, nor what the member did as an admin.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AdminsOnly`] if this replica's member is no admin there, for
	/// good, [`NodeError::NoClaim`] if the member made no claim there that stands and is not
	/// settled, which no rejection undoes, and an error if the change cannot be saved.
	pub fn reject_claim(
		&mut self,
		space: SpaceId,
		member: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.check_settled_admin(space, "reject claims")?;
		let now = self.wall.get();
		let claims: Vec<ChangeId> = {
			let open = self.space(space)?;
			open.stored
				.log()
				.claims(&open.keys)?
				.into_iter()
				.filter(|claim| {
					claim.claimant == member
						&& !claim.rejected
						&& claim.settled.is_none_or(|settled| settled.millis() > now)
				})
				.map(|claim| claim.claim)
				.collect()
		};
		if claims.is_empty() {
			return Err(NodeError::NoClaim(member.to_string()));
		}
		let operations = claims
			.iter()
			.map(|claim| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					member,
					fields::REJECTED,
					Value::Text(claim.to_string()),
				))
			})
			.collect();
		self.commit(space, operations, wall_millis, rng)
	}

	/// After a sync: shows, in each shared space this replica is in, that it saw each claim
	/// and each rejection of another member's, as soon as it holds one; and states the admin
	/// role on its own member's record once a claim of theirs is settled.
	pub(crate) fn settle_claims(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		// One space's error stops none of the others; the first is told at the end.
		let mut first_error = Ok(());
		for space in spaces {
			if space == self.info.personal
				|| !self.has_members(space).unwrap_or(false)
				|| self.check_not_revoked(space).is_err()
			{
				continue;
			}
			let settled = self
				.see_claims(space, wall_millis, rng)
				.and_then(|()| self.take_settled_claim(space, wall_millis, rng));
			if first_error.is_ok() {
				first_error = settled;
			}
		}
		first_error
	}

	/// Shows that this replica saw each claim and rejection in a space that another member
	/// made, and that it has not shown it saw yet.
	fn see_claims(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own_key = self.replica.signing.verifying_key();
		{
			let open = self.space(space)?;
			let log = open.stored.log();
			if log.claims(&open.keys)?.is_empty()
				|| !log.is_listed(&own_key, &open.keys)?
				|| log.is_removed(&own_key, &open.keys)?
			{
				return Ok(());
			}
		}
		let own = self.user_member(space)?;
		let (_, owners) = self.users_and_owners(space)?;
		let record = replica_record(space, &own_key);
		let mut seen = BTreeSet::new();
		let mut unseen = BTreeSet::new();
		{
			let open = self.space(space)?;
			let log = open.stored.log();
			for governing in log.governing_statements(&open.keys)? {
				let field = &governing.statement.field;
				if *field == fields::SEEN && governing.author == own_key {
					seen.extend(governing.statement.value.as_text().map(str::to_owned));
				} else if governing.counts
					&& (*field == fields::CLAIM || *field == fields::REJECTED)
					&& governing.author != own_key
				{
					// A member's own claim, or their own rejection, they do not see.
					let by_own = if *field == fields::CLAIM {
						Some(governing.statement.entity) == own
					} else {
						own.is_some() && owners.get(governing.author.as_bytes()).copied() == own
					};
					if !by_own {
						unseen.insert(governing.change.to_string());
					}
				}
			}
		}
		unseen.retain(|change| !seen.contains(change));
		if unseen.is_empty() {
			return Ok(());
		}
		let operations = unseen
			.into_iter()
			.map(|change| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					record,
					fields::SEEN,
					Value::Text(change),
				))
			})
			.collect();
		self.commit(space, operations, wall_millis, rng)
	}

	/// States the admin role on this replica's member's record in a space, once a claim of
	/// theirs is settled and the record does not say so yet: so the space keeps an admin by
	/// its records, which the rules of never being without one go by.
	fn take_settled_claim(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(own) = self.member_in(space)? else {
			return Ok(());
		};
		let role = {
			let open = self.space(space)?;
			open.stored
				.log()
				.member_roles(&open.keys)?
				.get(&own)
				.copied()
		};
		// A claimant lowered or removed since takes nothing.
		if role != Some(MemberRole::Editor)
			|| self.claim_now(space, own)? != Some(ClaimNow::Settled)
		{
			return Ok(());
		}
		self.set_role(space, own, MemberRole::Admin, wall_millis, rng)
	}
}
