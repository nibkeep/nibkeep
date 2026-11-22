//! Revoking replicas, the changes set aside for review, and the keys of the spaces: taking
//! those sealed to this replica, giving those it holds to the replicas that lack them, and
//! making new ones ([Identity](../../../../docs/spec/identity.md#4-revoking-replicas)).

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, Key, VerifyingKey, key_hash};
use nibkeep_governance::{Code, Role, Roster, SealedSpaceKey};
use nibkeep_log::ChangeId;
use nibkeep_time::Timestamp;

use super::Library;
use super::sync::Replica;
use crate::NodeError;

/// A change that was set aside: its author was revoked by a replica that did not have it,
/// or nobody listed its key; or, in a space with members, its author's member left, was
/// removed or lost a role before they knew of it, or their role never allowed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetAside {
	/// The change.
	pub change: ChangeId,
	/// The replica that made it, if the space names one with its key.
	pub replica: Option<EntityId>,
	/// In a space with members, the member whose replica made it, by the listings that the
	/// change builds on.
	pub member: Option<EntityId>,
	/// When it was made, as its author says, in milliseconds since 1970.
	pub millis: u64,
	/// How many operations it holds, if this replica can read it.
	pub operations: Option<usize>,
	/// Whether it governs the space, such as by revoking a replica or adding one: such a
	/// change is never accepted.
	pub governs: bool,
	/// Whether accepting it would make it count: an ordinary change that only a lowered role,
	/// a removal or a revocation set aside, and, in a space with members, one that this
	/// replica can read, so that an admin reviews what it does.
	pub acceptable: bool,
	/// The entities that it changes, if this replica can read it, each with its name: the one
	/// the space shows, or else the one that the change gives it.
	pub entities: Vec<(EntityId, Option<String>)>,
}

impl Library {
	/// The user's replicas that `target` names: the replica with that ID, or the replicas
	/// of the device with that ID or name, or the AI agent's replica of that name, revoked
	/// ones included. A device's name names no AI agent's replica. An ID may be given by its
	/// first eight characters or more, as lists show them.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichDevice`] if `target` names no device or replica of the
	/// user's, or several, and an error if the personal space cannot be read.
	pub fn replicas_named(&self, target: &str) -> Result<Vec<Replica>, NodeError> {
		named_among(self.replicas()?, target)
	}

	/// The replicas that revoking `target` revokes, as [`Library::replicas_named`] finds
	/// them, those whose every key is revoked already left out.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::replicas_named`], and [`NodeError::WhichDevice`] if
	/// `target` names this replica or its device.
	pub fn to_revoke(&self, target: &str) -> Result<Vec<Replica>, NodeError> {
		let mut chosen = self.replicas_named(target)?;
		if chosen.iter().any(|replica| replica.this) {
			return Err(NodeError::WhichDevice(
				"this device cannot revoke itself; revoke it from another of yours".to_owned(),
			));
		}
		chosen.retain(|replica| !replica.revoked);
		Ok(chosen)
	}

	/// The numbers of the keys of a space that this replica holds, in order, and the number
	/// of the one it writes with.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space.
	pub fn space_key_numbers(&self, space: SpaceId) -> Result<(Vec<u64>, u64), NodeError> {
		let open = self.space(space)?;
		Ok((open.keys.epochs().collect(), open.keys.newest().0))
	}

	/// Whether this replica knows that it was revoked. A revoked replica seldom does: every
	/// replica that knows of the revocation refuses to sync with it.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn is_revoked(&self) -> Result<bool, NodeError> {
		match self.check_not_revoked(self.info.personal) {
			Ok(()) => Ok(false),
			Err(NodeError::Revoked) => Ok(true),
			Err(error) => Err(error),
		}
	}

	/// Revokes replicas of the user, for good: no replica syncs with them any more, those of
	/// their changes that this replica does not have are set aside, and the personal space
	/// gets a new key, sealed to every replica that remains, so that they cannot read what is
	/// added later. Each revocation names the key it revokes: every key that the replica's
	/// record has, but this replica's own. The person confirms before the call. In every
	/// other space and group where they are listed, it revokes them too, and the devices
	/// that they added there and that the personal space does not list, which nothing tells
	/// from one that a thief paired on them: returns the spaces and groups where it revoked
	/// such a device.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichDevice`] if a replica is this one or not the user's,
	/// [`NodeError::Revoked`] if this replica was revoked itself, and an error if the keys or
	/// the change cannot be saved.
	pub fn revoke(
		&mut self,
		replicas: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<SpaceId>, NodeError> {
		let personal = self.info.personal;
		self.check_not_revoked(personal)?;
		self.check_not_ai_agent("revoke devices")?;
		let known: BTreeSet<EntityId> = self
			.replicas()?
			.into_iter()
			.filter(|replica| !replica.this)
			.map(|replica| replica.id)
			.collect();
		if let Some(unknown) = replicas.iter().find(|replica| !known.contains(replica)) {
			return Err(NodeError::WhichDevice(format!(
				"{unknown} is no other replica of yours"
			)));
		}
		let own = self.replica.signing.verifying_key();
		let roster = self.roster(personal)?;
		let open = self.space(personal)?;
		let log = open.stored.log();
		let mut revoking = Vec::new();
		for &replica in replicas {
			for key in roster.keys_of(replica) {
				if *key != own && !log.is_revoked(key, &open.keys)? {
					revoking.push((replica, *key));
				}
			}
		}
		self.record_own_endpoint(wall_millis, rng)?;
		let roster = self.roster(personal)?;
		self.rotate(personal, &revoking, &roster, wall_millis, rng)?;
		// And in every other space where they are listed, each with a new key. The personal
		// space's revocation is made: a space where this fails revokes after the next sync.
		let (elsewhere, _) = self.revoke_in_spaces_telling(wall_millis, rng);
		// A new secrets key, if the secrets are unlocked here and a revoked replica may have
		// held them; or else the first replica that unlocks them makes one.
		let _ = self.settle_secrets(wall_millis, rng);
		Ok(elsewhere)
	}

	/// Revokes one key of a replica of the user, whatever its record says, as
	/// [`Library::revoke`] does.
	pub(crate) fn revoke_key(
		&mut self,
		replica: EntityId,
		key: &VerifyingKey,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let roster = self.roster(personal)?;
		self.rotate(personal, &[(replica, *key)], &roster, wall_millis, rng)
	}

	/// The changes set aside in the personal space, waiting for review, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn set_aside(&self) -> Result<Vec<SetAside>, NodeError> {
		self.set_aside_in(self.info.personal)
	}

	/// The changes set aside in a space, waiting for review, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn set_aside_in(&self, space: SpaceId) -> Result<Vec<SetAside>, NodeError> {
		let open = self.space(space)?;
		let roster = open.stored.log().roster(&open.keys)?;
		let log = open.stored.log();
		let state = log.state();
		let members = space != self.info.personal && self.has_members(space)?;
		let mut set_aside = Vec::new();
		for change in log.set_aside_changes()? {
			let (Some(header), Some(review)) =
				(log.change(change)?, log.review(change, &open.keys)?)
			else {
				continue;
			};
			let replica = roster.replica_of(header.author());
			let payload = review.payload.as_ref();
			let mut entities: Vec<(EntityId, Option<String>)> = Vec::new();
			if let Some(payload) = payload {
				for operation in payload.all_operations() {
					let Some(entity) = operation.entity() else {
						continue;
					};
					if entities.iter().any(|(listed, _)| *listed == entity) {
						continue;
					}
					let name =
						super::entities::display_name(state, &open.types, &self.languages, entity)?
							.or_else(|| stated_name(payload, entity));
					entities.push((entity, name));
				}
			}
			set_aside.push(SetAside {
				change,
				replica,
				member: if members { review.member } else { None },
				millis: header.timestamp().millis(),
				operations: payload.map(|payload| payload.all_operations().count()),
				governs: header.kind() == nibkeep_log::Kind::Governing,
				// A replica's acceptance of its own change counts for nothing.
				acceptable: review.acceptable
					&& *header.author() != self.replica.signing.verifying_key(),
				entities,
			});
		}
		Ok(set_aside)
	}

	/// Accepts changes that were set aside in the personal space, as
	/// [`Library::accept_set_aside_in`] does there.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::accept_set_aside_in`].
	pub fn accept_set_aside(
		&mut self,
		changes: &[ChangeId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.accept_set_aside_in(self.info.personal, changes, wall_millis, rng)
	}

	/// Accepts changes that were set aside in a space, so that they count after all, with
	/// their own authors and times, in a change that governs the space: in a space with
	/// members, as an admin of it. Only those that accepting makes count are accepted, as
	/// [`SetAside::acceptable`] says: never one that governs the space, nor one of a key that
	/// no replica of the space has. Returns how many were accepted. The person confirms
	/// before the call, having seen what they do.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] for an AI agent's replica,
	/// [`NodeError::Revoked`] if this replica was revoked, [`NodeError::AdminsOnly`] in a
	/// space with members if its member is no admin there, and an error if the space cannot
	/// be read or the change saved.
	pub fn accept_set_aside_in(
		&mut self,
		space: SpaceId,
		changes: &[ChangeId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.observe_wall(wall_millis);
		if space == self.info.personal || !self.has_members(space)? {
			self.check_not_ai_agent("accept the changes of revoked devices")?;
		} else {
			self.check_settled_admin(space, "accept changes that were set aside")?;
		}
		let set_aside = self.set_aside_in(space)?;
		let operations: Vec<_> = set_aside
			.iter()
			.filter(|aside| changes.contains(&aside.change) && aside.acceptable)
			.filter_map(|aside| {
				let replica = aside.replica?;
				Some(nibkeep_governance::accept(
					replica,
					&aside.change.to_string(),
					rng,
				))
			})
			.collect();
		let accepted = operations.len();
		if accepted > 0 {
			self.commit(space, operations, wall_millis, rng)?;
			// What the accepted changes held may define types, or change the languages.
			self.read_again(space)?;
		}
		Ok(accepted)
	}

	/// Settles the keys of the spaces after a sync: takes the keys sealed to this replica,
	/// gives the keys it holds to the replicas that lack them, and makes a new key where its
	/// newest is not safe, as after two revocations at the same time; and states that the
	/// fields the types make secret are secret for good. A replica that is revoked, or that no
	/// counting change lists, does nothing, and an AI agent's only takes its keys: the rest
	/// governs the space, which it may not.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read, or the keys or the changes saved.
	pub fn settle_keys(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		// The spaces besides the personal one first, so that one taken now settles too; what
		// fails there is told once the rest is done, which never waits for it.
		let mut settled = self.settle_spaces(wall_millis, rng);
		// Then the groups: the spaces they give, and the keys and anchors that they need; and
		// the claims to the admin role that it saw.
		let grouped = self.settle_groups(wall_millis, rng);
		if settled.is_ok() {
			settled = grouped;
		}
		let claimed = self.settle_claims(wall_millis, rng);
		if settled.is_ok() {
			settled = claimed;
		}
		// The personal space first; another space's error stops neither it nor the others.
		for space in self.spaces() {
			let result = self.settle_space(space, wall_millis, rng);
			if space == self.info.personal {
				result?;
			} else if settled.is_ok() {
				settled = result;
			}
		}
		// The secrets keys given to it, while its secrets are unlocked. The sync is done; the
		// next one, or unlocking the secrets, tries again.
		let _ = self.settle_secrets(wall_millis, rng);
		// The recovery file, for the codes that the personal space names now; and the codes
		// that a newer one replaces, once every backup carries it.
		let saved = self.save_recovery_file();
		if settled.is_ok() {
			settled = saved;
		}
		let replaced = self
			.revoke_replaced_recovery_codes(false, wall_millis, rng)
			.map(|_| ());
		if settled.is_ok() {
			settled = replaced;
		}
		// The drops that this device makes, which the sync may have moved on.
		let dropped = self.settle_drops(wall_millis, rng);
		if settled.is_ok() {
			settled = dropped;
		}
		settled
	}

	/// Settles one space's keys after a sync, as [`Library::settle_keys`] says.
	fn settle_space(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let own = self.replica.signing.verifying_key();
		if !log.is_governed(&open.keys)?
			|| log.is_revoked(&own, &open.keys)?
			|| !log.is_listed(&own, &open.keys)?
		{
			return Ok(());
		}
		// An AI agent's replica governs nothing, and one whose member left or was removed
		// changes nothing there any more: each only takes the keys given it before.
		if log.is_ai_agent(&own, &open.keys)? || log.is_removed(&own, &open.keys)? {
			let roster = self.roster(space)?;
			self.take_keys(space, &roster, rng)?;
			self.choose_writing_key_with(space, &roster)?;
			return Ok(());
		}
		// Others can only give keys to a record with an encryption key.
		if space == self.info.personal {
			self.record_own_endpoint(wall_millis, rng)?;
		}
		self.mark_secret_fields(space, wall_millis, rng)?;
		// Giving keys out adds statements of keys, but no rotation.
		let roster = self.roster(space)?;
		self.take_keys(space, &roster, rng)?;
		self.give_keys_with(space, &roster, wall_millis, rng)?;
		if !self.choose_writing_key_with(space, &roster)? {
			self.rotate(space, &[], &roster, wall_millis, rng)?;
		}
		Ok(())
	}

	/// Who the replicas of a space are, as its counting governing changes say.
	pub(crate) fn roster(&self, space: SpaceId) -> Result<Roster, NodeError> {
		let open = self.space(space)?;
		let mut roster = open.stored.log().roster(&open.keys)?;
		// The replicas that groups in the space list, at every level, on their records there,
		// derived from the space and their keys, with the encryption keys that their records in
		// the group state.
		let listed = open.stored.log().listed_through_groups(&open.keys)?;
		if listed.is_empty() {
			return Ok(roster);
		}
		let groups: Vec<SpaceId> = self
			.inner_groups(space)?
			.into_iter()
			.filter(|group| self.spaces.contains_key(group))
			.collect();
		let mut group_rosters = Vec::new();
		for group in groups {
			let open = self.space(group)?;
			group_rosters.push((group, open.stored.log().roster(&open.keys)?));
		}
		for key in listed {
			let encryption = group_rosters.iter().find_map(|(group, group_roster)| {
				group_roster.encryption_of(nibkeep_governance::replica_record(*group, &key))
			});
			if let Some(encryption) = encryption {
				roster.add_listed(
					nibkeep_governance::replica_record(space, &key),
					key,
					encryption.clone(),
				);
			}
		}
		Ok(roster)
	}

	/// Chooses the key that each space writes with, as [`Library::choose_writing_key_with`]
	/// says.
	pub(crate) fn choose_writing_keys(&mut self) -> Result<(), NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		for space in spaces {
			let roster = self.roster(space)?;
			self.choose_writing_key_with(space, &roster)?;
		}
		Ok(())
	}

	/// Has a space write with the newest key that this replica holds and that is safe: whose
	/// rotation a member's replica made, by the changes it builds on, and builds on every
	/// counting revocation and every departure, so that whoever made it knew of each; the
	/// newest is the one whose rotation came last, by time and then by ID, key 0's being the
	/// space's first change. Returns whether one is safe; if none is, it writes with the
	/// newest, until a new key is made.
	fn choose_writing_key_with(
		&mut self,
		space: SpaceId,
		roster: &Roster,
	) -> Result<bool, NodeError> {
		// A group holds no data to protect: every change of it is wrapped with its first key,
		// which never changes, so that every replica of the spaces it is in reads its log
		// ([technical design](../../../../docs/Technical-design.md#138-groups)).
		if self.space(space)?.is_group()? {
			let open = self
				.spaces
				.get_mut(&space)
				.ok_or(NodeError::NoSpace(space))?;
			open.keys.write_with(Some(0));
			return Ok(true);
		}
		let open = self.space(space)?;
		let log = open.stored.log();
		let Some(root) = log.root()? else {
			return Ok(true);
		};
		let Some(first) = log.change(root)? else {
			return Ok(true);
		};
		let mut makers = Makers::of(open);
		let mut candidates: Vec<((Timestamp, ChangeId), u64)> = Vec::new();
		let mut counting: BTreeSet<ChangeId> = BTreeSet::new();
		for (epoch, key) in open.keys.space_keys() {
			if epoch == 0 {
				candidates.push(((first.timestamp(), root), epoch));
			} else if let Some(rotation) = roster.rotation(epoch, &key_hash(key))
				&& makers.made(rotation.change)?
			{
				candidates.push(((rotation.at, rotation.change), epoch));
				if rotation.counts {
					counting.insert(rotation.change);
				}
			}
		}
		candidates.sort_by(|a, b| b.cmp(a));
		let mut departures = log.revocations(&open.keys)?;
		departures.extend(log.removals(&open.keys)?);
		// A recovery code's revocation is a departure too: a key sealed to the code by one
		// who had not heard of it is not written with.
		departures.extend(
			roster
				.codes()
				.flat_map(|code| code.revoked_by.iter().copied()),
		);
		// The newest first: it is usually the safe one, and the walks are few.
		let mut writing = None;
		for &((_, rotation), epoch) in &candidates {
			if log.builds_on_all(rotation, &departures)? {
				writing = Some(epoch);
				break;
			}
		}
		let safe = writing.is_some();
		// Until a new key is made, the newest whose rotation counts, or key 0: never one made
		// by someone as they were removed, which they hold.
		let fallback = || {
			candidates
				.iter()
				.find(|((_, rotation), epoch)| *epoch == 0 || counting.contains(rotation))
				.map(|(_, epoch)| *epoch)
		};
		let Some(epoch) = writing.or_else(fallback) else {
			return Ok(true);
		};
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		open.keys.write_with(Some(epoch));
		Ok(safe)
	}

	/// Takes the keys of a space that statements seal to this replica, of numbers it lacks,
	/// where a member's replica made the statement and the key's rotation, by the changes
	/// each builds on, whether they count or not, and their hashes are those that the
	/// rotations name; saves them, and reads what they open. So it takes a key that a member
	/// made while being removed, with which others may have written. Where a counting
	/// statement names a number, only such statements give it, as the others give each key
	/// again where it counts; a number that only statements that do not count name is taken
	/// only if they agree on its key, so that someone who left cannot plant a key of their
	/// own under the number of another.
	fn take_keys(
		&mut self,
		space: SpaceId,
		roster: &Roster,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.own_record(space)?;
		let taken = self.keys_sealed_to(space, roster, (own, &self.replica.decryption))?;
		self.save_and_read_keys(space, &taken, rng)
	}

	/// Takes the keys of a space that statements seal to a recovery code, on its record
	/// `record`, as [`Library::take_keys`] takes those sealed to this replica, opening them
	/// with the code's decryption key: for a device that the code brings back. Returns how
	/// many it took.
	pub(crate) fn take_keys_of_code(
		&mut self,
		space: SpaceId,
		roster: &Roster,
		(record, decryption): (EntityId, &DecryptionKey),
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let taken = self.keys_sealed_to(space, roster, (record, decryption))?;
		self.save_and_read_keys(space, &taken, rng)?;
		Ok(taken.len())
	}

	/// The keys of a space that statements seal to the record `record`, of numbers this
	/// replica lacks, that open with `decryption`, as [`Library::take_keys`] says.
	fn keys_sealed_to(
		&self,
		space: SpaceId,
		roster: &Roster,
		(record, decryption): (EntityId, &DecryptionKey),
	) -> Result<Vec<(u64, Key)>, NodeError> {
		let open = self.space(space)?;
		let held: BTreeSet<u64> = open.keys.epochs().collect();
		let mut makers = Makers::of(open);
		// The statements that may give each number, with whether they and the rotation count.
		let mut candidates: std::collections::BTreeMap<u64, Vec<(&SealedSpaceKey, bool)>> =
			std::collections::BTreeMap::new();
		for gift in roster.given_to(record) {
			let sealed = &gift.sealed;
			if held.contains(&sealed.epoch) {
				continue;
			}
			let Some(rotation) = roster.rotation(sealed.epoch, &sealed.hash) else {
				continue;
			};
			if !makers.made(gift.change)? || !makers.made(rotation.change)? {
				continue;
			}
			candidates
				.entry(sealed.epoch)
				.or_default()
				.push((sealed, gift.counts));
		}
		let mut taken: Vec<(u64, Key)> = Vec::new();
		for (epoch, mut gifts) in candidates {
			if gifts.iter().any(|(_, counts)| *counts) {
				gifts.retain(|(_, counts)| *counts);
			} else if gifts
				.iter()
				.any(|(sealed, _)| sealed.hash != gifts[0].0.hash)
			{
				continue;
			}
			for (sealed, _) in gifts {
				if let Some(key) = sealed
					.open(decryption, space, record)
					.filter(|key| key_hash(key) == sealed.hash)
				{
					taken.push((epoch, key));
					break;
				}
			}
		}
		Ok(taken)
	}

	/// Saves the keys `taken` of a space, and reads what they open: also what a key saved
	/// before, by a run that stopped in between, opens.
	fn save_and_read_keys(
		&mut self,
		space: SpaceId,
		taken: &[(u64, Key)],
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if !taken.is_empty() {
			self.save_space_keys(space, taken, rng)?;
		}
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		open.stored.open_changes(&open.keys)?;
		open.stored.open_blob_keys(&open.keys)?;
		self.read_again(space)?;
		Ok(())
	}

	/// Gives the keys of a space that this replica holds, as [`Library::give_keys_with`]
	/// does.
	pub(crate) fn give_keys(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let roster = self.roster(space)?;
		self.give_keys_with(space, &roster, wall_millis, rng)
	}

	/// Gives the keys of a space that this replica holds, those whose rotations a member's
	/// replica made, by the changes they build on, whatever later set them aside, to every
	/// replica that remains, this one included, and every recovery code that stands, that has
	/// no counting statement of them, in one change that governs the space: so that what was written with a key stays readable
	/// to those who join later. Key 0 every replica holds already.
	fn give_keys_with(
		&mut self,
		space: SpaceId,
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let remaining = self.key_recipients(space, roster, &[])?;
		let open = self.space(space)?;
		let mut makers = Makers::of(open);
		let mut given: Vec<(u64, &Key, Hash)> = Vec::new();
		for (epoch, key) in open.keys.space_keys() {
			let hash = key_hash(key);
			if let Some(rotation) = roster.rotation(epoch, &hash)
				&& makers.made(rotation.change)?
			{
				given.push((epoch, key, hash));
			}
		}
		let mut operations = Vec::new();
		for (replica, encryption) in remaining {
			// Given only by a counting statement, so that a key set aside as it was made is
			// given again where it counts, and wins over any other of its number.
			let has: BTreeSet<(u64, Hash)> = roster
				.given_to(replica)
				.iter()
				.filter(|gift| gift.counts)
				.map(|gift| (gift.sealed.epoch, gift.sealed.hash))
				.collect();
			for &(epoch, key, hash) in &given {
				if has.contains(&(epoch, hash)) {
					continue;
				}
				let sealed =
					SealedSpaceKey::seal(&encryption, space, replica, (epoch, key, hash), rng);
				operations.push(nibkeep_governance::give_key(replica, &sealed, rng));
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// The replicas of a space that remain, but those of `revoking`, with their encryption
	/// keys: those whose keys are listed and none revoked, whose members did not leave and
	/// were not removed, nor AI agents' where the space allows none, and that have an
	/// encryption key.
	pub(super) fn remaining(
		&self,
		space: SpaceId,
		roster: &Roster,
		revoking: &[(EntityId, VerifyingKey)],
	) -> Result<Vec<(EntityId, EncryptionKey)>, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let mut remaining = Vec::new();
		for replica in roster.replicas() {
			if revoking.iter().any(|(revoked, _)| *revoked == replica) {
				continue;
			}
			let mut stays = true;
			for key in roster.keys_of(replica) {
				stays &= log.is_listed(key, &open.keys)?
					&& !log.is_revoked(key, &open.keys)?
					&& !log.is_removed(key, &open.keys)?;
			}
			if !stays {
				continue;
			}
			if let Some(encryption) = roster.encryption_of(replica) {
				remaining.push((replica, encryption.clone()));
			}
		}
		Ok(remaining)
	}

	/// Those that a key of a space is sealed to: the replicas that remain, as
	/// [`Library::remaining`] finds them, and the recovery codes that stand, each on its
	/// record with the encryption key that it signed, but those of `leaving`. A code is given
	/// a space's keys as a replica is, while it stands and, in a space with members, while
	/// its member has a role there: a member of the space itself, or of a group that gives
	/// their keys a role there
	/// ([technical design](../../../../docs/Technical-design.md#111-recovery-codes)).
	pub(super) fn key_recipients(
		&self,
		space: SpaceId,
		roster: &Roster,
		leaving: &[(EntityId, VerifyingKey)],
	) -> Result<Vec<(EntityId, EncryptionKey)>, NodeError> {
		let mut recipients = self.remaining(space, roster, leaving)?;
		recipients.extend(
			self.code_recipients(space, roster, leaving)?
				.into_iter()
				.map(|code| (code.record, code.statement.encryption.clone())),
		);
		Ok(recipients)
	}

	/// The recovery codes that a key of a space is sealed to, as [`Library::key_recipients`]
	/// says, but those of `leaving`.
	pub(super) fn code_recipients<'r>(
		&self,
		space: SpaceId,
		roster: &'r Roster,
		leaving: &[(EntityId, VerifyingKey)],
	) -> Result<Vec<&'r Code>, NodeError> {
		let standing: Vec<&Code> = roster
			.codes()
			.filter(|code| code.stands() && !leaving.iter().any(|(left, _)| *left == code.record))
			.collect();
		// Most spaces name no code, and the members' roles cost a walk of the groups.
		if standing.is_empty() || !self.has_members(space)? {
			return Ok(standing);
		}
		let open = self.space(space)?;
		let log = open.stored.log();
		let direct = log.member_roles(&open.keys)?;
		let through = log.group_member_roles(&open.keys)?;
		Ok(standing
			.into_iter()
			.filter(|code| {
				let member = &code.statement.member;
				direct
					.get(member)
					.or_else(|| through.get(member))
					.is_some_and(|role| *role != Role::None)
			})
			.collect())
	}

	/// Gives a space a new key, sealed to every replica that remains, this one included, and
	/// revokes the keys of `revoking` in the same change. The key's number is a random one
	/// that no key of the space has, so that keys made at the same time differ. The key is
	/// saved before the change that gives it out.
	pub(crate) fn rotate(
		&mut self,
		space: SpaceId,
		revoking: &[(EntityId, VerifyingKey)],
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.rotate_revoking(space, revoking, &[], roster, wall_millis, rng)
	}

	/// Gives a space a new key, as [`Library::rotate`] does, and revokes in the same change
	/// the replicas' keys of `revoking` and the recovery codes of `codes`, each by its record
	/// and its recovery key: none of them is given the new key.
	pub(crate) fn rotate_revoking(
		&mut self,
		space: SpaceId,
		revoking: &[(EntityId, VerifyingKey)],
		codes: &[(EntityId, VerifyingKey)],
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let open = self.space(space)?;
		// Every number the space knows: the keys held, and those that statements name.
		let mut known: BTreeSet<u64> = open.keys.epochs().collect();
		known.extend(roster.numbers());
		let epoch = loop {
			let mut random = [0; 8];
			rng.fill_bytes(&mut random);
			let epoch = u64::from_le_bytes(random);
			if !known.contains(&epoch) {
				break epoch;
			}
		};
		let key = Key::generate(rng);
		let hash = key_hash(&key);
		let mut operations = nibkeep_governance::revoke(revoking, rng);
		for (record, key) in codes {
			operations.push(nibkeep_governance::revoke_recovery_code(*record, key, rng));
		}
		let leaving: Vec<(EntityId, VerifyingKey)> =
			revoking.iter().chain(codes).copied().collect();
		for (replica, encryption) in self.key_recipients(space, roster, &leaving)? {
			let sealed =
				SealedSpaceKey::seal(&encryption, space, replica, (epoch, &key, hash), rng);
			operations.push(nibkeep_governance::give_key(replica, &sealed, rng));
		}
		self.save_space_keys(space, &[(epoch, key)], rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		let roster = self.roster(space)?;
		self.choose_writing_key_with(space, &roster)?;
		Ok(())
	}

	/// Saves keys of a space with the replica's other keys, in the key file as it is on the
	/// disk, and gives them to the space.
	fn save_space_keys(
		&mut self,
		space: SpaceId,
		keys: &[(u64, Key)],
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let compartment = self.compartment.clone();
		let mut saved = None;
		self.update_key_file(|key_file| {
			let mut replica = key_file.replica(&compartment)?;
			let epochs = replica.spaces.entry(space).or_default();
			for (epoch, key) in keys {
				epochs.insert(*epoch, key.clone());
			}
			key_file.set_replica(&replica, &compartment, rng)?;
			saved = Some(replica.spaces);
			Ok(())
		})?;
		if let Some(spaces) = saved {
			self.replica.spaces = spaces;
		}
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		for (epoch, key) in keys {
			open.keys.insert(*epoch, key.clone());
		}
		Ok(())
	}
}

/// Which of a space's changes that give keys out a member's replica made, by the changes
/// they build on, as found so far: a key, or a gift of one, counts only if one did
/// ([technical design](../../../../docs/Technical-design.md#13-sharing)).
pub(super) struct Makers<'a> {
	stored: &'a nibkeep_store::StoredLog,
	keys: &'a nibkeep_log::SpaceKeys,
	found: std::collections::BTreeMap<ChangeId, bool>,
}

impl<'a> Makers<'a> {
	pub(super) fn of(open: &'a super::Space) -> Self {
		Self {
			stored: &open.stored,
			keys: &open.keys,
			found: std::collections::BTreeMap::new(),
		}
	}

	/// Whether a member's replica made a change, by the changes it builds on.
	pub(super) fn made(&mut self, change: ChangeId) -> Result<bool, NodeError> {
		if let Some(&made) = self.found.get(&change) {
			return Ok(made);
		}
		let made = self.stored.log().author_was_a_member(change, self.keys)?;
		self.found.insert(change, made);
		Ok(made)
	}
}

/// The name that a change's payload gives an entity, in full, if it states one.
fn stated_name(payload: &nibkeep_log::Payload, entity: EntityId) -> Option<String> {
	payload
		.all_operations()
		.find_map(|operation| match operation {
			nibkeep_model::Operation::Assert { statement, .. }
				if statement.entity == entity && statement.field == nibkeep_model::fields::NAME =>
			{
				match &statement.value {
					nibkeep_model::Value::Name(name) => Some(name.full().to_owned()),
					other => other.as_text().map(str::to_owned),
				}
			}
			_ => None,
		})
}

/// The replicas of `replicas` that `target` names, as [`Library::replicas_named`] says.
pub(super) fn named_among(replicas: Vec<Replica>, target: &str) -> Result<Vec<Replica>, NodeError> {
	let is_target = |id: EntityId| {
		let id = id.to_string();
		id == target || (target.len() >= 8 && id.starts_with(target))
	};
	let mut named: BTreeSet<EntityId> = BTreeSet::new();
	for replica in &replicas {
		if is_target(replica.id) {
			named.insert(replica.id);
		}
		if let Some(device) = replica.device.filter(|&device| is_target(device)) {
			named.insert(device);
		}
	}
	if named.is_empty() {
		// An AI agent's replica is named by its own name, so that its device, which pairing
		// names after the computer it runs on, never makes a device's name ambiguous.
		named = replicas
			.iter()
			.filter(|replica| !replica.ai_agent && replica.device_name.as_deref() == Some(target))
			.filter_map(|replica| replica.device)
			.collect();
		named.extend(
			replicas
				.iter()
				.filter(|replica| replica.ai_agent && replica.name.as_deref() == Some(target))
				.map(|replica| replica.id),
		);
	}
	if named.len() > 1 {
		return Err(NodeError::WhichDevice(format!(
			"{target} names several devices or replicas; give more of the ID of the one you mean"
		)));
	}
	let Some(&named) = named.first() else {
		return Err(NodeError::WhichDevice(format!(
			"no device or replica of yours is {target}"
		)));
	};
	Ok(replicas
		.into_iter()
		.filter(|replica| replica.id == named || replica.device == Some(named))
		.collect())
}
