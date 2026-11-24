//! The secrets keys of a space once the secrets are set up: the one that seals new secret
//! values, a new one after a replica that may hold the secrets departs, and giving them to
//! every replica known to hold the secrets, which takes a key from any replica if the gift
//! carries a link made with a secrets key that it holds already
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)). A replica that never
//! held the secrets cannot make such a link, so it neither gives a key of its own making to
//! seal with, nor is given one by the others.

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{EncryptionKey, Key, RecoveryCode, VerifyingKey, key_hash};
use nibkeep_governance::{Roster, SealedSecretsKey, recovery_record};
use nibkeep_keys::{SecretsKeys, SpaceKeyMap};
use nibkeep_log::ChangeId;
use nibkeep_time::Timestamp;

use super::Library;
use super::revocation::Makers;
use crate::NodeError;

/// A secrets key that a statement seals to this replica's record in a space, which it takes
/// if the key opens with its hash: one that the replica it takes the secrets from gave, or
/// one whose link a secrets key it holds checks, made by a member's replica.
pub(super) struct Given {
	pub(super) space: SpaceId,
	/// The replica's record that it is given to.
	pub(super) replica: EntityId,
	pub(super) sealed: SealedSecretsKey,
	/// Given by the replica it takes the secrets from, in a counting change signed by that
	/// replica's key of its own, as [technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#12-secrets)
	/// says.
	pub(super) asked: bool,
	/// Whether a member's replica made the change that gives it and the key's rotation, by
	/// the changes they build on, whatever later set them aside.
	pub(super) made: bool,
}

/// A replica known to hold the secrets of a space, which this one gives the keys it lacks.
struct Holder {
	replica: EntityId,
	encryption: EncryptionKey,
	/// The keys given to it, by number and hash, as far as this replica can tell.
	has: BTreeSet<(u64, Hash)>,
	/// For a replica of this user's, known to hold the personal space's secrets, the newest
	/// key of the personal space given to it that this replica holds, by its space and
	/// number, which links what no key of the space itself can.
	personal: Option<(SpaceId, u64, Key)>,
}

/// This user's replicas known to hold the personal space's secrets, by their personal
/// records, each with the newest personal key given to it that this replica holds, which
/// links a gift of another space's key to it; with the personal space's roster, and the
/// spaces that each holds by its newest choice there, as `(space, record)`. Read once for
/// every space that the secrets settle in.
pub(super) struct PersonalHolders {
	roster: Roster,
	links: std::collections::BTreeMap<EntityId, (SpaceId, u64, Key)>,
	holding: BTreeSet<(SpaceId, EntityId)>,
}

/// A secrets key that this replica holds and gives out: its rotation a member's replica
/// made, by the changes it builds on.
struct GivenOut {
	rotation: (Timestamp, ChangeId),
	epoch: u64,
	key: Key,
	hash: Hash,
}

/// Takes into `keys` the secrets keys of `given` that open with their secrets decryption key
/// and whose hashes are those stated: those that the replica asked gave, and those whose
/// links a key of `keys` checks, as one taken here may. Returns how many it took.
pub(super) fn take_given(given: &[Given], keys: &mut SecretsKeys) -> usize {
	let mut taken = 0;
	loop {
		let mut opened = Vec::new();
		for gift in given {
			let held = |space: SpaceId, epoch: u64| {
				keys.spaces.get(&space).and_then(|held| held.get(&epoch))
			};
			if held(gift.space, gift.sealed.epoch).is_some() {
				continue;
			}
			let link = &gift.sealed.link;
			let linked = || {
				gift.made
					&& held(link.space, link.epoch)
						.is_some_and(|key| gift.sealed.link_holds(gift.space, gift.replica, key))
			};
			if !gift.asked && !linked() {
				continue;
			}
			if let Some(key) = gift
				.sealed
				.open(&keys.decryption, gift.space, gift.replica)
				.filter(|key| key_hash(key) == gift.sealed.hash)
			{
				opened.push((gift.space, gift.sealed.epoch, key));
			}
		}
		let before = taken;
		for (space, epoch, key) in opened {
			if let std::collections::btree_map::Entry::Vacant(vacant) =
				keys.spaces.entry(space).or_default().entry(epoch)
			{
				vacant.insert(key);
				taken += 1;
			}
		}
		if taken == before {
			return taken;
		}
	}
}

/// The keys of `held` that `given` gives, by number and hash: those that the replica asked
/// gave, and those whose links a key kept checks, as one kept here may; so a copy of a
/// replica keeps what that replica would have taken.
pub(super) fn keep_given(given: &[Given], held: &SpaceKeyMap) -> SpaceKeyMap {
	let mut kept = SpaceKeyMap::new();
	loop {
		let mut more = Vec::new();
		for gift in given {
			if kept
				.get(&gift.space)
				.is_some_and(|kept| kept.contains_key(&gift.sealed.epoch))
			{
				continue;
			}
			let Some(key) = held
				.get(&gift.space)
				.and_then(|held| held.get(&gift.sealed.epoch))
				.filter(|key| key_hash(key) == gift.sealed.hash)
			else {
				continue;
			};
			let link = &gift.sealed.link;
			let linked = || {
				gift.made
					&& kept
						.get(&link.space)
						.and_then(|kept| kept.get(&link.epoch))
						.is_some_and(|key| gift.sealed.link_holds(gift.space, gift.replica, key))
			};
			if gift.asked || linked() {
				more.push((gift.space, gift.sealed.epoch, key.clone()));
			}
		}
		if more.is_empty() {
			return kept;
		}
		for (space, epoch, key) in more {
			kept.entry(space).or_default().insert(epoch, key);
		}
	}
}

/// A random number for a new secrets key, which no key of `known` has, so that two keys
/// made at the same time differ.
pub(super) fn fresh_number(known: &BTreeSet<u64>, rng: &mut impl CryptoRng) -> u64 {
	loop {
		let epoch = rng.next_u64();
		if !known.contains(&epoch) {
			return epoch;
		}
	}
}

impl Library {
	/// The numbers of the secrets keys of a space that this replica holds while its secrets
	/// are unlocked, in order, and the number of the one it seals new secret values with.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space.
	pub fn secrets_key_numbers(
		&self,
		space: SpaceId,
	) -> Result<(Vec<u64>, Option<u64>), NodeError> {
		let open = self.space(space)?;
		Ok((
			open.keys.secrets_keys().map(|(epoch, _)| epoch).collect(),
			open.keys.newest_secrets().map(|(epoch, _)| epoch),
		))
	}

	/// The secrets keys that statements of `rosters` seal to the record of the replica whose
	/// key is `whose`, or else of this replica, to the secrets encryption key whose hash is
	/// `recipient`, or else to the one that each space states for it, with whether that
	/// replica may take each.
	pub(super) fn secrets_given(
		&self,
		rosters: &std::collections::BTreeMap<SpaceId, Roster>,
		whose: Option<&VerifyingKey>,
		recipient: Option<Hash>,
	) -> Result<Vec<Given>, NodeError> {
		let mut given = Vec::new();
		for (&space, roster) in rosters {
			let own = match whose {
				Some(key) => match roster.replica_of(key) {
					Some(record) => record,
					None => continue,
				},
				None => self.own_record(space)?,
			};
			let Some(recipient) = recipient.or_else(|| {
				roster
					.secrets_encryption_of(own)
					.map(nibkeep_governance::secrets_recipient)
			}) else {
				continue;
			};
			let asked_key = roster
				.secrets_from(own)
				.and_then(|from| roster.own_key_of(from));
			let mut makers = Makers::of(self.space(space)?);
			for gift in roster.secrets_gifts(own) {
				if gift.sealed.recipient != recipient {
					continue;
				}
				let asked = gift.counts && asked_key == Some(&gift.author);
				let made = !asked && self.linked_and_made(space, roster, &mut makers, gift)?;
				given.push(Given {
					space,
					replica: own,
					sealed: gift.sealed.clone(),
					asked,
					made,
				});
			}
		}
		Ok(given)
	}

	/// The secrets keys that statements of `rosters` seal to the recovery code `code`, on
	/// each of its records, to the secrets encryption key that it signed, with whether a
	/// replica that recovers with the code may take each: those that the replica which made
	/// the code gave it on the person's record of the code in the personal space, signed by
	/// that replica's key of its own, as a replica takes those of the replica it asked; and
	/// those whose links a key taken checks.
	/// A code asks nobody: the replica that made it gave it the secrets, as its person chose
	/// then ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	pub(super) fn code_secrets_given(
		&self,
		rosters: &std::collections::BTreeMap<SpaceId, Roster>,
		code: &RecoveryCode,
	) -> Result<Vec<Given>, NodeError> {
		let key = code.signing_key().verifying_key();
		let recipient =
			nibkeep_governance::secrets_recipient(&code.secrets_decryption_key().encryption_key());
		// The code's record for the person in the personal space, whose first statement is
		// its maker's: a statement copied onto a record for another member, as its signature
		// allows, makes nobody its maker.
		let own = recovery_record(self.info.personal, self.info.user, &key);
		let mut given = Vec::new();
		for (&space, roster) in rosters {
			let mut makers = Makers::of(self.space(space)?);
			for named in roster.codes().filter(|named| named.statement.key == key) {
				let maker = space == self.info.personal
					&& named.record == own
					&& roster
						.replica_of(&named.author)
						.is_some_and(|maker| roster.own_key_of(maker) == Some(&named.author));
				for gift in roster.secrets_gifts(named.record) {
					if gift.sealed.recipient != recipient {
						continue;
					}
					let asked = maker && gift.counts && gift.author == named.author;
					let made = !asked && self.linked_and_made(space, roster, &mut makers, gift)?;
					given.push(Given {
						space,
						replica: named.record,
						sealed: gift.sealed.clone(),
						asked,
						made,
					});
				}
			}
		}
		Ok(given)
	}

	/// The secrets keys that the recovery code `code` opens, of those that statements of
	/// `rosters` seal to it, by space and number: those that a replica which recovers with the
	/// code takes, as [`Library::code_secrets_given`] says.
	pub(super) fn secrets_code_opens(
		&self,
		rosters: &std::collections::BTreeMap<SpaceId, Roster>,
		code: &RecoveryCode,
	) -> Result<SpaceKeyMap, NodeError> {
		let given = self.code_secrets_given(rosters, code)?;
		let mut keys = SecretsKeys {
			spaces: SpaceKeyMap::new(),
			decryption: code.secrets_decryption_key(),
		};
		take_given(&given, &mut keys);
		Ok(keys.spaces)
	}

	/// Whether a gift of a secrets key in `space` carries a link, which proves the giver
	/// held a key of this space, or of the personal space, which only this user's replicas
	/// hold, as another space's proves nothing here; and whether a member's replica made the
	/// change that gives it and the key's rotation, by the changes they build on.
	fn linked_and_made(
		&self,
		space: SpaceId,
		roster: &Roster,
		makers: &mut Makers<'_>,
		gift: &nibkeep_governance::SecretsGift,
	) -> Result<bool, NodeError> {
		let link = &gift.sealed.link;
		Ok((link.space == space || link.space == self.info.personal)
			&& makers.made(gift.change)?
			&& match roster.secrets_rotation(gift.sealed.epoch, &gift.sealed.hash) {
				Some(rotation) => makers.made(rotation.change)?,
				None => false,
			})
	}

	/// The departures after which a space's secrets need a new key: every counting change
	/// that revokes a replica that a statement gave a secrets key, or that ends it in a space
	/// with members, as a member's leaving or removal does; and every one that revokes a
	/// recovery code that a statement gave one.
	fn secrets_departures(
		&self,
		space: SpaceId,
		roster: &Roster,
	) -> Result<Vec<ChangeId>, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let mut departures = BTreeSet::new();
		for replica in roster.replicas() {
			if !roster.may_hold_secrets(replica) {
				continue;
			}
			for key in roster.keys_of(replica) {
				departures.extend(log.revocations_of(Some(key), &open.keys)?);
				departures.extend(log.removals_of(key, &open.keys)?);
			}
		}
		// And every counting revocation of a recovery code that a statement gave one.
		for code in roster.codes() {
			if roster.may_hold_secrets(code.record) {
				departures.extend(code.revoked_by.iter().copied());
			}
		}
		Ok(departures.into_iter().collect())
	}

	/// Has a space seal new secret values with the newest secrets key that this replica
	/// holds and that is safe: whose rotation a member's replica made, by the changes it
	/// builds on, and builds on every departure after which the secrets need a new key, so
	/// that whoever made it knew of each; the newest is the one whose rotation came last, by
	/// time and then by ID. Until one is safe, it seals with the newest whose rotation counts.
	/// Returns whether one is safe, or none needs to be, as no replica that held the secrets
	/// departed and no key it holds has a rotation, as one set up before records said so.
	pub(super) fn choose_secrets_key_with(
		&mut self,
		space: SpaceId,
		roster: &Roster,
	) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		if open.keys.newest_secrets().is_none() {
			return Ok(true);
		}
		let log = open.stored.log();
		let mut makers = Makers::of(open);
		let mut candidates: Vec<((Timestamp, ChangeId), u64)> = Vec::new();
		let mut counting: BTreeSet<ChangeId> = BTreeSet::new();
		// Keys that no statement names, as one set up before the records said so.
		let mut unstated = Vec::new();
		for (epoch, key) in open.keys.secrets_keys() {
			match roster.secrets_rotation(epoch, &key_hash(key)) {
				Some(rotation) if makers.made(rotation.change)? => {
					candidates.push(((rotation.at, rotation.change), epoch));
					if rotation.counts {
						counting.insert(rotation.change);
					}
				}
				Some(_) => {}
				None => unstated.push(epoch),
			}
		}
		candidates.sort_by(|a, b| b.cmp(a));
		let departures = self.secrets_departures(space, roster)?;
		let mut writing = None;
		for &((_, rotation), epoch) in &candidates {
			if log.builds_on_all(rotation, &departures)? {
				writing = Some(epoch);
				break;
			}
		}
		let safe = writing.is_some() || (candidates.is_empty() && departures.is_empty());
		// Never by the number alone, which is a random one: a key that no statement names
		// before one that someone not a member made.
		let writing = writing
			.or_else(|| {
				candidates
					.iter()
					.find(|((_, rotation), _)| counting.contains(rotation))
					.map(|(_, epoch)| *epoch)
			})
			.or_else(|| unstated.first().copied())
			.or_else(|| candidates.first().map(|(_, epoch)| *epoch));
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		open.keys.write_secrets_with(writing);
		Ok(safe)
	}

	/// While the secrets are unlocked, after they settle: in each space whose secrets this
	/// replica holds, and where it remains, has a key of its own and is no AI agent's, makes
	/// a new secrets key if none is safe, and gives the keys it holds to the replicas known to
	/// hold the secrets that lack them.
	pub(super) fn settle_secrets_keys(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.personal_holders()?;
		for space in self.secret_spaces()? {
			let Some(roster) = self.giving_roster(space)? else {
				continue;
			};
			if !self.choose_secrets_key_with(space, &roster)? {
				self.new_secrets_key(space, (&roster, personal.as_ref()), wall_millis, rng)?;
			}
			let roster = self.roster(space)?;
			self.give_secrets_to_holders(space, (&roster, personal.as_ref()), wall_millis, rng)?;
		}
		Ok(())
	}

	/// The roster of a space whose secrets this replica gives out: one whose secrets it holds
	/// unlocked, where it remains, has a key of its own and is no AI agent's.
	fn giving_roster(&self, space: SpaceId) -> Result<Option<Roster>, NodeError> {
		if !self.secrets_unlocked(space) {
			return Ok(None);
		}
		let own_key = self.replica.signing.verifying_key();
		let open = self.space(space)?;
		let log = open.stored.log();
		if log.is_revoked(&own_key, &open.keys)?
			|| !log.is_listed(&own_key, &open.keys)?
			|| log.is_ai_agent(&own_key, &open.keys)?
			|| log.is_removed(&own_key, &open.keys)?
		{
			return Ok(None);
		}
		let roster = self.roster(space)?;
		if roster.owner_of(&own_key) != Some(self.own_record(space)?) {
			return Ok(None);
		}
		Ok(Some(roster))
	}

	/// Gives `replica`, a replica of this user's that this one has just given the personal
	/// space's secrets, the keys it lacks of the other spaces whose secrets this replica gives
	/// out, each linked with the personal space's key given to it: what
	/// [`Library::settle_secrets_keys`] would give it the next time the secrets settle, by the
	/// same rule, so that one gift of the person's brings the secrets of every space that both
	/// hold. Returns how many keys it gave. A space where that fails is left to the next
	/// settling, and the others are given theirs.
	pub(super) fn give_other_spaces_secrets(
		&mut self,
		replica: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> usize {
		let Ok(Some(personal)) = self.personal_holders() else {
			return 0;
		};
		// Its record in another space is the one whose key of its own is its own here.
		let Some(its_key) = personal.roster.own_key_of(replica).copied() else {
			return 0;
		};
		let Ok(spaces) = self.secret_spaces() else {
			return 0;
		};
		let mut given = 0;
		for space in spaces {
			if space == self.info.personal {
				continue;
			}
			given += self
				.give_space_secrets_linked(space, (&personal, &its_key), wall_millis, rng)
				.unwrap_or(0);
		}
		given
	}

	/// Gives the replica whose key of its own is `its_key`, in `space`, the keys it lacks of
	/// those this replica gives out there, as [`Library::give_other_spaces_secrets`] says.
	/// Returns how many keys it gave.
	fn give_space_secrets_linked(
		&mut self,
		space: SpaceId,
		(personal, its_key): (&PersonalHolders, &VerifyingKey),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let Some(roster) = self.giving_roster(space)? else {
			return Ok(0);
		};
		let own = self.own_record(space)?;
		let given_out = self.given_out(space, &roster)?;
		let mut operations = Vec::new();
		for holder in self.known_holders(space, &roster, own, Some(personal))? {
			if holder.personal.is_none() || roster.own_key_of(holder.replica) != Some(its_key) {
				continue;
			}
			for key in &given_out {
				if !holder.has.contains(&(key.epoch, key.hash)) {
					let gift = (key.epoch, &key.key, key.hash);
					operations.push(linked_gift(space, &holder, gift, &given_out, rng));
				}
			}
		}
		let given = operations.len();
		if given > 0 {
			self.commit(space, operations, wall_millis, rng)?;
		}
		Ok(given)
	}

	/// This user's replicas known to hold the personal space's secrets, as
	/// [`PersonalHolders`] says, while this replica's are unlocked.
	fn personal_holders(&self) -> Result<Option<PersonalHolders>, NodeError> {
		let personal = self.info.personal;
		if !self.secrets_unlocked(personal) {
			return Ok(None);
		}
		let roster = self.roster(personal)?;
		let given = self.given_out(personal, &roster)?;
		let mut links = std::collections::BTreeMap::new();
		for holder in self.known_holders(personal, &roster, self.info.replica, None)? {
			if let Some(key) = given
				.iter()
				.find(|key| holder.has.contains(&(key.epoch, key.hash)))
			{
				links.insert(holder.replica, (personal, key.epoch, key.key.clone()));
			}
		}
		let read = self.read_personal()?;
		let holding = Self::newest_choices(&read, &roster)
			.into_iter()
			.filter(|(_, (_, choice))| choice.holds)
			.map(|((space, _), (record, _))| (space, record))
			.collect();
		Ok(Some(PersonalHolders {
			roster,
			links,
			holding,
		}))
	}

	/// The replicas of a space, but this one's record `own`, known to hold the secrets: those
	/// that remain, are no AI agent's, have a secrets encryption key, and that this replica
	/// gave a secrets key, or that a counting gift to whose key carries a link that a key it
	/// holds checks, or that it takes the secrets from. A replica that only says it holds the
	/// secrets, with a key of its own making, is none of these.
	fn known_holders(
		&self,
		space: SpaceId,
		roster: &Roster,
		own: EntityId,
		personal: Option<&PersonalHolders>,
	) -> Result<Vec<Holder>, NodeError> {
		// In another space, this user's replicas known to hold the personal space's secrets
		// too: the person chose them already.
		let personal = personal.filter(|_| space != self.info.personal);
		let remaining = self.remaining(space, roster, &[])?;
		let open = self.space(space)?;
		let log = open.stored.log();
		let own_key = self.replica.signing.verifying_key();
		let asked = roster.secrets_from(own);
		let mut holders = Vec::new();
		'replicas: for (replica, _) in remaining {
			if replica == own {
				continue;
			}
			for key in roster.keys_of(replica) {
				if log.is_ai_agent(key, &open.keys)? {
					continue 'replicas;
				}
			}
			let Some(encryption) = roster.secrets_encryption_of(replica) else {
				continue;
			};
			let recipient = nibkeep_governance::secrets_recipient(encryption);
			let giver = roster
				.secrets_from(replica)
				.and_then(|from| roster.own_key_of(from));
			let mut known = asked == Some(replica);
			let mut has = BTreeSet::new();
			for gift in roster.secrets_gifts(replica) {
				let ours = gift.author == own_key;
				// Its own gifts whether they count or not, which giving again would not change.
				if gift.sealed.recipient != recipient || !(gift.counts || ours) {
					continue;
				}
				let link = &gift.sealed.link;
				let linked = (link.space == space || link.space == self.info.personal)
					&& self.space(link.space).is_ok_and(|linked| {
						linked
							.keys
							.secrets(link.epoch)
							.is_some_and(|key| gift.sealed.link_holds(space, replica, key))
					});
				known |= linked || ours;
				if linked || ours || giver == Some(&gift.author) {
					has.insert((gift.sealed.epoch, gift.sealed.hash));
				}
			}
			// Only a replica that holds the space, by its newest choice of it in the personal
			// space: one that declined it, or never took it, as a phone through a group, is
			// given none of its secrets by its user's devices.
			let personal_link = personal.and_then(|personal| {
				roster.keys_of(replica).iter().find_map(|key| {
					personal
						.roster
						.replica_of(key)
						.filter(|record| personal.holding.contains(&(space, *record)))
						.and_then(|record| personal.links.get(&record))
				})
			});
			known |= personal_link.is_some();
			if known {
				holders.push(Holder {
					replica,
					encryption: encryption.clone(),
					has,
					personal: personal_link.cloned(),
				});
			}
		}
		holders.extend(self.code_holders(space, roster, personal)?);
		Ok(holders)
	}

	/// The recovery codes of a space known to hold its secrets, as
	/// [`Library::known_holders`] knows replicas to: those that stand, whose members have a
	/// role, and that this replica gave a secrets key, or that a counting gift to whose
	/// secrets encryption key carries a link that a key it holds checks; and, in a space other
	/// than the personal one, the codes of this user's that are known to hold the personal
	/// space's secrets. A code asks nobody: the replica that made it gave it the secrets, as
	/// its person chose then
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	fn code_holders(
		&self,
		space: SpaceId,
		roster: &Roster,
		personal: Option<&PersonalHolders>,
	) -> Result<Vec<Holder>, NodeError> {
		let own_key = self.replica.signing.verifying_key();
		let standing: BTreeSet<EntityId> = self
			.key_recipients(space, roster, &[])?
			.into_iter()
			.map(|(record, _)| record)
			.collect();
		let mut holders = Vec::new();
		for code in roster.codes() {
			if !standing.contains(&code.record) {
				continue;
			}
			let encryption = &code.statement.secrets_encryption;
			let recipient = nibkeep_governance::secrets_recipient(encryption);
			let mut known = false;
			let mut has = BTreeSet::new();
			for gift in roster.secrets_gifts(code.record) {
				let ours = gift.author == own_key;
				if gift.sealed.recipient != recipient || !(gift.counts || ours) {
					continue;
				}
				let link = &gift.sealed.link;
				let linked = (link.space == space || link.space == self.info.personal)
					&& self.space(link.space).is_ok_and(|linked| {
						linked
							.keys
							.secrets(link.epoch)
							.is_some_and(|key| gift.sealed.link_holds(space, code.record, key))
					});
				if linked || ours {
					known = true;
					has.insert((gift.sealed.epoch, gift.sealed.hash));
				}
			}
			// The same code, in the personal space, by its recovery key.
			let personal_link = personal.and_then(|personal| {
				personal
					.roster
					.codes()
					.filter(|named| named.statement.key == code.statement.key)
					.find_map(|named| personal.links.get(&named.record))
			});
			known |= personal_link.is_some();
			if known {
				holders.push(Holder {
					replica: code.record,
					encryption: encryption.clone(),
					has,
					personal: personal_link.cloned(),
				});
			}
		}
		Ok(holders)
	}

	/// Gives a recovery code of the person's the secrets keys of the personal space that this
	/// replica holds and gives out, and that the code has not been given, each sealed to the
	/// secrets encryption key that the code signed, in one change that governs the space;
	/// the first linked with itself, so that every holder knows that the code holds them, and
	/// gives it the keys made later. The person chose so, with the secrets unlocked.
	pub(super) fn give_code_secrets(
		&mut self,
		record: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		if !self.secrets_unlocked(personal) {
			return Err(NodeError::SecretsLocked);
		}
		let roster = self.roster(personal)?;
		let Some(code) = roster.code(record) else {
			return Err(NodeError::NoEntity(record.to_string()));
		};
		let encryption = code.statement.secrets_encryption.clone();
		let recipient = nibkeep_governance::secrets_recipient(&encryption);
		let has: BTreeSet<(u64, Hash)> = roster
			.secrets_gifts(record)
			.iter()
			.filter(|gift| gift.sealed.recipient == recipient)
			.map(|gift| (gift.sealed.epoch, gift.sealed.hash))
			.collect();
		let holder = Holder {
			replica: record,
			encryption,
			has,
			personal: None,
		};
		let given = self.given_out(personal, &roster)?;
		let operations: Vec<nibkeep_model::Operation> = given
			.iter()
			.filter(|key| !holder.has.contains(&(key.epoch, key.hash)))
			.map(|key| {
				linked_gift(
					personal,
					&holder,
					(key.epoch, &key.key, key.hash),
					&given,
					rng,
				)
			})
			.collect();
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(personal, operations, wall_millis, rng)
	}

	/// The secrets keys of a space that this replica holds and gives out, the newest first,
	/// by their rotations.
	fn given_out(&self, space: SpaceId, roster: &Roster) -> Result<Vec<GivenOut>, NodeError> {
		let open = self.space(space)?;
		let mut makers = Makers::of(open);
		let mut given = Vec::new();
		for (epoch, key) in open.keys.secrets_keys() {
			let hash = key_hash(key);
			if let Some(rotation) = roster.secrets_rotation(epoch, &hash)
				&& makers.made(rotation.change)?
			{
				given.push(GivenOut {
					rotation: (rotation.at, rotation.change),
					epoch,
					key: key.clone(),
					hash,
				});
			}
		}
		given.sort_by_key(|key| std::cmp::Reverse(key.rotation));
		Ok(given)
	}

	/// Gives the secrets keys of a space that this replica holds and gives out to the
	/// replicas known to hold the secrets that lack them, in one change that governs the
	/// space: each linked with the newest key given to that replica that this one holds, so
	/// that it takes the key from this one, or else with the key itself, so that the others
	/// know this one held it.
	fn give_secrets_to_holders(
		&mut self,
		space: SpaceId,
		(roster, personal): (&Roster, Option<&PersonalHolders>),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.own_record(space)?;
		let holders = self.known_holders(space, roster, own, personal)?;
		let given = self.given_out(space, roster)?;
		let mut operations = Vec::new();
		for holder in &holders {
			for key in &given {
				if !holder.has.contains(&(key.epoch, key.hash)) {
					let gift = (key.epoch, &key.key, key.hash);
					operations.push(linked_gift(space, holder, gift, &given, rng));
				}
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Sets up the secrets of a shared space whose secrets no remaining replica holds, as the
	/// first secret value written there does, on an admin's replica that holds the secrets
	/// compartment open: a secrets key of a random number, saved in the compartment first,
	/// then, in one change that governs the space, this replica's secrets encryption key,
	/// that it takes the secrets from itself, and the key given to itself, linked with the
	/// personal space's key it seals with, so that this user's other replicas take it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SpaceSecretsElsewhere`] if another replica that remains holds the
	/// space's secrets, [`NodeError::SetUpSecretsFirst`] if this replica has no secrets
	/// compartment, [`NodeError::SecretsLocked`] if it is locked, [`NodeError::AdminsOnly`]
	/// if this replica's member is no admin of the space, [`NodeError::ForeignKey`] if its
	/// record lists a key it did not make, and an error if the key file or the change cannot
	/// be saved.
	pub(super) fn set_up_space_secrets(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		let compartment = self.open_compartment()?;
		// Another program may have saved the space's keys since: they are opened.
		let held = self.key_file_on_disk()?.secrets(&compartment)?;
		if held.spaces.get(&space).is_some_and(|keys| !keys.is_empty()) {
			return self.open_secrets(&held);
		}
		// Linked with a key of the personal space, so that this user's other replicas take
		// it: one that asked for the secrets, and was not given them yet, sets up none.
		if !self.secrets_unlocked(self.info.personal) {
			return Err(NodeError::SecretsElsewhere);
		}
		let roster = self.roster(space)?;
		let own = self.own_record(space)?;
		for holder in roster.secrets_holders() {
			if holder != own && self.is_remaining(space, &roster, holder)? {
				return Err(NodeError::SpaceSecretsElsewhere);
			}
		}
		self.check_admin(space, "set up the secrets of a shared space")?;
		if roster.owner_of(&self.replica.signing.verifying_key()) != Some(own) {
			return Err(NodeError::ForeignKey);
		}
		let mut known = roster.secrets_numbers();
		known.extend(
			held.spaces
				.get(&space)
				.into_iter()
				.flatten()
				.map(|(epoch, _)| *epoch),
		);
		let epoch = fresh_number(&known, rng);
		let key = Key::generate(rng);
		let hash = key_hash(&key);
		self.update_key_file(|key_file| {
			let mut keys = key_file.secrets(&compartment)?;
			keys.spaces
				.entry(space)
				.or_default()
				.insert(epoch, key.clone());
			key_file.update_secrets(&keys, &compartment, rng)?;
			Ok(())
		})?;
		self.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?
			.keys
			.insert_secrets(epoch, key.clone());
		let encryption = held.decryption.encryption_key();
		let mut operations = Vec::new();
		if roster.secrets_encryption_of(own) != Some(&encryption) {
			operations.push(nibkeep_governance::state_secrets_key(own, &encryption, rng));
		}
		operations.push(nibkeep_governance::state_secrets_from(own, own, rng));
		let personal = self.info.personal;
		let link = self
			.space(personal)?
			.keys
			.newest_secrets()
			.map(|(epoch, key)| (personal, epoch, key.clone()));
		let (link_space, link_epoch, link_key) = link.unwrap_or((space, epoch, key.clone()));
		let mine = SealedSecretsKey::seal(
			&encryption,
			space,
			own,
			(epoch, &key, hash),
			(link_space, link_epoch, &link_key),
			rng,
		);
		operations.push(nibkeep_governance::give_secrets_key(own, &mine, rng));
		self.commit(space, operations, wall_millis, rng)?;
		let roster = self.roster(space)?;
		self.choose_secrets_key_with(space, &roster)?;
		// This user's other replicas that hold the secrets get it at once.
		let personal = self.personal_holders()?;
		self.give_secrets_to_holders(space, (&roster, personal.as_ref()), wall_millis, rng)
	}

	/// Gives a space a new secrets key, whose number is a random one that no secrets key of
	/// the space has: saved in the secrets compartment first, then given to this replica,
	/// linked with the key it sealed with until now, and to the replicas known to hold the
	/// secrets, as [`Library::give_secrets_to_holders`] links them, in one change that
	/// governs the space. New secret values are sealed with it.
	fn new_secrets_key(
		&mut self,
		space: SpaceId,
		(roster, personal): (&Roster, Option<&PersonalHolders>),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(compartment) = self.secrets_compartment.clone() else {
			return Ok(());
		};
		let own = self.own_record(space)?;
		let Some(own_encryption) = roster.secrets_encryption_of(own).cloned() else {
			return Ok(());
		};
		let open = self.space(space)?;
		let mut known: BTreeSet<u64> = open.keys.secrets_keys().map(|(epoch, _)| epoch).collect();
		known.extend(roster.secrets_numbers());
		let epoch = fresh_number(&known, rng);
		let previous = open
			.keys
			.newest_secrets()
			.map(|(epoch, key)| (epoch, key.clone()));
		let key = Key::generate(rng);
		let hash = key_hash(&key);
		// Saved before it is given, as space keys are.
		self.update_key_file(|key_file| {
			let mut keys = key_file.secrets(&compartment)?;
			keys.spaces
				.entry(space)
				.or_default()
				.insert(epoch, key.clone());
			key_file.update_secrets(&keys, &compartment, rng)?;
			Ok(())
		})?;
		self.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?
			.keys
			.insert_secrets(epoch, key.clone());
		let (link_epoch, link_key) = previous
			.as_ref()
			.map_or((epoch, &key), |(epoch, key)| (*epoch, key));
		let mine = SealedSecretsKey::seal(
			&own_encryption,
			space,
			own,
			(epoch, &key, hash),
			(space, link_epoch, link_key),
			rng,
		);
		let mut operations = vec![nibkeep_governance::give_secrets_key(own, &mine, rng)];
		let given = self.given_out(space, roster)?;
		for holder in &self.known_holders(space, roster, own, personal)? {
			operations.push(linked_gift(space, holder, (epoch, &key, hash), &given, rng));
		}
		self.commit(space, operations, wall_millis, rng)?;
		let roster = self.roster(space)?;
		self.choose_secrets_key_with(space, &roster)?;
		Ok(())
	}
}

/// The operation that gives `holder` of `space` the secrets key `key`, by its number, the
/// key and its hash, linked with the newest of `given` that `holder` was given, or else with
/// the personal space's key that `holder` names, or else with `key` itself.
fn linked_gift(
	space: SpaceId,
	holder: &Holder,
	(epoch, key, hash): (u64, &Key, Hash),
	given: &[GivenOut],
	rng: &mut impl CryptoRng,
) -> nibkeep_model::Operation {
	let link = given
		.iter()
		.find(|link| holder.has.contains(&(link.epoch, link.hash)))
		.map(|link| (space, link.epoch, &link.key))
		.or_else(|| {
			holder
				.personal
				.as_ref()
				.map(|(space, epoch, key)| (*space, *epoch, key))
		})
		.unwrap_or((space, epoch, key));
	let sealed = SealedSecretsKey::seal(
		&holder.encryption,
		space,
		holder.replica,
		(epoch, key, hash),
		link,
		rng,
	);
	nibkeep_governance::give_secrets_key(holder.replica, &sealed, rng)
}
