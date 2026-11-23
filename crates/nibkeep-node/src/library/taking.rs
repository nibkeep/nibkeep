//! Devices that choose their spaces: consent and taking
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
//!
//! A space reaches a device of the user's only with the person's consent: the personal
//! space's entry of the space, its first key sealed to the device, is given to the devices
//! the person chooses as they make or join a space, or pair a device, and to a device whose
//! request they approve on a device that holds the space. Each device says, as its own,
//! whether it takes a space given to it later or asks first: computers take them, phones
//! ask. A device that asks takes an entry only when the person says so there, and asks for a
//! space it has no entry of with a choice of its own to hold it. The devices that hold a
//! space keep its names in the personal space, so that every device shows each space of the
//! user's by name.

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{EncryptionKey, key_hash};
use nibkeep_governance::{
	Choice, OF_SPACE, SPACE_NAMES, SpaceEntry, give_space_entry, replica_record,
	space_names_record, state_choice,
};
use nibkeep_model::{Operation, Statement, Value, fields};

use super::Library;
use super::entities::display_name;
use super::spaces::Personal;
use crate::NodeError;

/// What the person chose for a new device of theirs, on the device that shows the code: a
/// computer takes every space of this device's, and the spaces given to it later; a phone
/// takes the personal space alone, and asks before it takes a space given to it later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewDevice {
	/// Whether it is a phone, which asks before it takes a space given to it later.
	pub phone: bool,
	/// The spaces besides the personal one that it gets, of those this device holds; `None`
	/// for the default: every space for a computer, none for a phone.
	pub spaces: Option<Vec<SpaceId>>,
}

impl NewDevice {
	/// A computer, which gets every space of this device's.
	#[must_use]
	pub fn computer() -> Self {
		Self::default()
	}

	/// A phone, which gets the personal space alone.
	#[must_use]
	pub fn phone() -> Self {
		Self {
			phone: true,
			spaces: None,
		}
	}

	/// The same, getting `spaces` besides the personal one instead.
	#[must_use]
	pub fn with_spaces(mut self, spaces: Vec<SpaceId>) -> Self {
		self.spaces = Some(spaces);
		self
	}
}

/// What taking a space did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taking {
	/// This device took the space, given to it before: it fills as it syncs.
	Took,
	/// This device asked for the space, which the person approves on a device that holds
	/// it.
	Asked,
}

/// A device of the user's that asks for a space this device holds, which the person
/// approves here with [`Library::allow_space`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceRequest {
	/// The space.
	pub space: SpaceId,
	/// The space's name.
	pub name: Option<String>,
	/// The replica that asks, by its record in the personal space.
	pub replica: EntityId,
	/// The name of its device.
	pub device: Option<String>,
	/// The name of its app.
	pub app: Option<String>,
	/// Whether it held the space before, and dropped it.
	pub back: bool,
}

/// A space of the user's, whether this device holds it or not, as the personal space
/// knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownSpace {
	/// The space.
	pub space: SpaceId,
	/// Its name.
	pub name: Option<String>,
	/// What this device did with it.
	pub here: Here,
	/// The devices of the user's that hold it, as their choices say, and this one.
	pub holders: Vec<SpaceHolder>,
	/// Whether this device drops it, and has not deleted it yet
	/// ([`Library::drop_space`]).
	pub dropping: bool,
}

impl KnownSpace {
	/// Whether this device holds it.
	#[must_use]
	pub fn held(&self) -> bool {
		self.here == Here::Held
	}
}

/// What a device did with a space of the user's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Here {
	/// It holds the space.
	Held,
	/// It was given the space, which it takes once the person says so there.
	Given,
	/// It asked for the space, and has not been given it yet.
	Asked,
	/// It declined or dropped the space, and has not asked for it again.
	Declined,
	/// None of these: it may ask for the space.
	Elsewhere,
}

/// A device of the user's that holds a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceHolder {
	/// The replica, by its record in the personal space.
	pub replica: EntityId,
	/// The name of its device.
	pub device: Option<String>,
	/// The name of its app.
	pub app: Option<String>,
	/// Whether it is this replica.
	pub this: bool,
}

impl Library {
	/// Whether this device takes the spaces given to it later, or asks first, as it said
	/// itself: a device that said nothing takes them.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn takes_new_spaces(&self) -> Result<bool, NodeError> {
		Ok(self.own_mark(&self.read_personal()?))
	}

	/// What this replica said itself of the spaces given to it later, as the personal space,
	/// `personal`, states it on its own record with its own key: whether it takes them.
	pub(super) fn own_mark(&self, personal: &Personal) -> bool {
		let own = self.replica.signing.verifying_key();
		personal
			.marks
			.iter()
			.rfind(|(record, author, _)| *record == self.info.replica && *author == own)
			.is_none_or(|(_, _, takes)| *takes)
	}

	/// What each replica of the user's said, itself, of the spaces given to it later: by its
	/// record in the personal space, whether it takes them; one that said nothing takes them.
	/// A statement counts only where one of the record's own keys made it, the last of those
	/// by the order of the changes; so that no other device can have it take what it was
	/// given.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn new_space_marks(&self) -> Result<BTreeMap<EntityId, bool>, NodeError> {
		let personal = self.read_personal()?;
		let roster = self.roster(self.info.personal)?;
		Ok(personal
			.marks
			.into_iter()
			.filter(|(record, author, _)| roster.keys_of(*record).contains(author))
			.map(|(record, _, takes)| (record, takes))
			.collect())
	}

	/// Takes the spaces given to this replica as it paired, every one but those it declined,
	/// whatever it says of the spaces given to it later: the person chose them on the device
	/// that paired it
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	pub(crate) fn take_paired_spaces(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let read = self.read_personal()?;
		self.take_space_entries_as(&read, true, wall_millis, rng)
	}

	/// Says, on this device, whether it takes the spaces given to it later, or asks first,
	/// as its own statement in the personal space, which no other device can make for it.
	///
	/// # Errors
	///
	/// Returns an error if this replica is an AI agent's or revoked, or the change cannot be
	/// saved.
	pub fn set_takes_new_spaces(
		&mut self,
		takes: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		self.check_not_revoked(personal)?;
		self.check_not_ai_agent("choose the spaces it holds")?;
		if self.takes_new_spaces()? == takes {
			return Ok(());
		}
		let operation = Operation::assert(Statement::new(
			StatementId::random(rng),
			self.info.replica,
			fields::NEW_SPACES,
			Value::Boolean(takes),
		));
		self.commit(personal, vec![operation], wall_millis, rng)
	}

	/// Gives a space that this device holds to the devices of the user's that the person
	/// chose, by their records in the personal space: its first key sealed to each, with the
	/// user's member there, in one change of the personal space. Each takes it as its own
	/// choice says: a computer at once, a phone once the person says so there. A device that
	/// has it already, or declined or dropped it, an AI agent's, and one revoked are passed
	/// over.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::Removed`] if
	/// this device may not give the space, and an error if the change cannot be saved.
	pub fn give_space(
		&mut self,
		space: SpaceId,
		replicas: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if space == self.info.personal {
			return Err(NodeError::NeverShared);
		}
		if self.member_in(space)?.is_none() || self.has_left(space)? {
			return Err(NodeError::Removed);
		}
		let read = self.read_personal()?;
		let roster = self.roster(self.info.personal)?;
		let given = read.given();
		let newest = Self::newest_choices(&read, &roster);
		let mut operations = Vec::new();
		for (replica, encryption) in self.remaining(self.info.personal, &roster, &[])? {
			if !replicas.contains(&replica)
				|| replica == self.info.replica
				|| given.contains(&(replica, space))
				|| self.is_ai_agent_record(&roster, replica)?
			{
				continue;
			}
			// One that declined or dropped it takes no entry: it asks again, itself.
			let declined = roster.keys_of(replica).iter().any(|key| {
				newest
					.get(&(space, *key.as_bytes()))
					.is_some_and(|(_, choice)| !choice.holds)
			});
			if declined {
				continue;
			}
			if let Some(operation) = self.entry_for(space, (replica, &encryption), rng)? {
				operations.push(operation);
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(self.info.personal, operations, wall_millis, rng)
	}

	/// Gives a space that this device made or joined, as the person chose: to the devices of
	/// the user's that take new spaces and to those of `also`, as [`Library::give_space`]
	/// does, after stating that this one holds it; and to the recovery codes that stand.
	pub(crate) fn give_new_space(
		&mut self,
		space: SpaceId,
		also: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.state_holding(&[space], wall_millis, rng)?;
		// Those that take them: every replica that said so, or said nothing.
		let marks = self.new_space_marks()?;
		let roster = self.roster(self.info.personal)?;
		let mut chosen: Vec<EntityId> = roster
			.replicas()
			.filter(|replica| marks.get(replica).copied().unwrap_or(true))
			.collect();
		chosen.extend_from_slice(also);
		self.give_space(space, &chosen, wall_millis, rng)?;
		self.give_space_entries(wall_millis, rng)
	}

	/// The entry that gives `space`, which this replica holds, to the replica `replica` of
	/// the user's, whose encryption key is `encryption`: the space's first key sealed to it,
	/// with the user's member there and the space's first change; none where this replica
	/// holds no first key or first change of it yet.
	pub(crate) fn entry_for(
		&self,
		space: SpaceId,
		(replica, encryption): (EntityId, &EncryptionKey),
		rng: &mut impl CryptoRng,
	) -> Result<Option<Operation>, NodeError> {
		let Some(member) = self.member_in(space)? else {
			return Ok(None);
		};
		let open = self.space(space)?;
		let Some(first) = open
			.keys
			.space_keys()
			.find(|(epoch, _)| *epoch == 0)
			.map(|(_, key)| key.clone())
		else {
			return Ok(None);
		};
		let Some(root) = open.stored.log().root()? else {
			return Ok(None);
		};
		let entry = SpaceEntry::seal(
			encryption,
			(space, member, *root.hash()),
			replica,
			(&first, key_hash(&first)),
			rng,
		);
		Ok(Some(give_space_entry(replica, &entry, rng)))
	}

	/// The entries that give a new replica of the user's, whose record pairing draws as
	/// `replica` and whose encryption key is `encryption`, the spaces that the person chose
	/// for it, of those this device holds: as [`NewDevice`] says, the spaces it may give,
	/// but groups, which reach a device as spaces do.
	pub(crate) fn entries_for_new(
		&self,
		(replica, encryption): (EntityId, &EncryptionKey),
		device: &NewDevice,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let chosen: Vec<SpaceId> = match &device.spaces {
			Some(spaces) => spaces.clone(),
			None if device.phone => Vec::new(),
			None => self.spaces(),
		};
		let mut entries = Vec::new();
		for space in chosen {
			// Not one it has dropped, whose drop it has stated, and still has.
			if space == self.info.personal || !self.holds_here(space)? || self.has_left(space)? {
				continue;
			}
			entries.extend(self.entry_for(space, (replica, encryption), rng)?);
		}
		Ok(entries)
	}

	/// Checks, before the code is shown, that this device can give a new device each space
	/// chosen for it: one it holds, besides the personal one, and is listed in.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpace`] for a space that this device does not hold, and
	/// [`NodeError::NotListed`] for one it is not listed in, or whose member left.
	pub fn check_new_device(&self, device: &NewDevice) -> Result<(), NodeError> {
		for &space in device.spaces.iter().flatten() {
			if space == self.info.personal {
				continue;
			}
			self.space(space)?;
			if self.member_in(space)?.is_none() || self.has_left(space)? {
				return Err(NodeError::NotListed);
			}
		}
		Ok(())
	}

	/// Whether a replica of the user's, by its record in the personal space, is an AI
	/// agent's, as a key of it is marked.
	fn is_ai_agent_record(
		&self,
		roster: &nibkeep_governance::Roster,
		replica: EntityId,
	) -> Result<bool, NodeError> {
		let open = self.space(self.info.personal)?;
		for key in roster.keys_of(replica) {
			if open.stored.log().is_ai_agent(key, &open.keys)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// This replica's newest choice of each space, of those that the personal space,
	/// `personal`, states on its own record: by space, its number and whether it holds the
	/// space.
	pub(super) fn own_choices(&self, personal: &Personal) -> BTreeMap<SpaceId, (u64, bool)> {
		let own = self.replica.signing.verifying_key();
		let mut choices: BTreeMap<SpaceId, (u64, bool)> = BTreeMap::new();
		for (record, text) in &personal.choices {
			if *record != self.info.replica {
				continue;
			}
			let Some(choice) = Choice::from_text(text) else {
				continue;
			};
			if choice.key != own || !choice.verifies() {
				continue;
			}
			let rank = |(number, holds): (u64, bool)| (number, !holds);
			let stated = (choice.number, choice.holds);
			if choices
				.get(&choice.space)
				.is_none_or(|kept| rank(stated) > rank(*kept))
			{
				choices.insert(choice.space, stated);
			}
		}
		choices
	}

	/// Whether this replica takes an entry of a space that it does not hold, by its newest
	/// choice of the space, `choice`: one that holds it, as when it asked; none, where it
	/// takes new spaces, `takes`; never one that declined or dropped it.
	pub(super) const fn takes_entry(choice: Option<(u64, bool)>, takes: bool) -> bool {
		match choice {
			Some((_, holds)) => holds,
			None => takes,
		}
	}

	/// States, in the personal space, that this replica holds each of `spaces` that its
	/// newest choice there does not say it holds already, as it takes them: each a choice
	/// numbered one above the highest of its own of that space, in one change.
	pub(crate) fn state_holding(
		&mut self,
		spaces: &[SpaceId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		let read = self.read_personal()?;
		let own = self.own_choices(&read);
		let mut operations = Vec::new();
		for &space in spaces {
			if space == self.info.personal || own.get(&space).is_some_and(|(_, holds)| *holds) {
				continue;
			}
			let number = self
				.highest_choice(space, &read)?
				.map_or(1, |number| number + 1);
			let choice = Choice::sign(&self.replica.signing, space, number, true);
			operations.push(state_choice(self.info.replica, &choice, rng));
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(self.info.personal, operations, wall_millis, rng)
	}

	/// Takes a space on this device, as the person says here: one given to it, through the
	/// personal space or a group, at once, stating that it holds it; one it drops, before its
	/// choice is stated, at once too, as it holds it still; one it was not given, or dropped,
	/// by asking for it with a choice of its own to hold it, which the person approves on a
	/// device that holds it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::PersonalSpaceHeld`] for the personal space,
	/// [`NodeError::HoldsSpace`] if this device holds the space already, and an error if this
	/// replica is an AI agent's or revoked, or a change cannot be saved.
	pub fn take_space(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Taking, NodeError> {
		let personal = self.info.personal;
		if space == personal {
			return Err(NodeError::PersonalSpaceHeld);
		}
		self.check_not_revoked(personal)?;
		self.check_not_ai_agent("choose the spaces it holds")?;
		let read = self.read_personal()?;
		let choice = self.own_choices(&read).get(&space).copied();
		// Dropped, not declined: it held the space before, so its drop may be there.
		let dropped = choice.is_some_and(|(_, holds)| !holds) && self.held_before(space, &read);
		let asked = choice.is_some_and(|(_, holds)| holds);
		if self.spaces.contains_key(&space) {
			// Dropped here before its choice was stated: taken back as it is.
			if self.take_back_drop(space)? {
				return Ok(Taking::Took);
			}
			// Held here and still listed: nothing to take. Dropped there: it asks again, once,
			// and drops it no longer.
			let open = self.space(space)?;
			let own = self.replica.signing.verifying_key();
			if open.stored.log().holds_space(&own, &open.keys)? && !dropped {
				return Err(NodeError::HoldsSpace(space));
			}
			if !asked {
				self.state_space_choice(space, true, wall_millis, rng)?;
			}
			self.end_drop(space)?;
			return Ok(Taking::Asked);
		}
		// Given before and never dropped: taken now. A device that dropped it asks first,
		// as only a device that holds it brings it back there.
		if !dropped && let Some((key, root)) = self.given_key(space, &read)? {
			self.add_space(space, &key, rng)?;
			if let Some(open) = self.spaces.get_mut(&space) {
				open.stored.expect_roots_too([root]);
			}
			self.state_holding(&[space], wall_millis, rng)?;
			return Ok(Taking::Took);
		}
		// Asked already, it asks no more.
		if !asked {
			self.state_space_choice(space, true, wall_millis, rng)?;
		}
		Ok(Taking::Asked)
	}

	/// Whether this replica held a space before, as one of its choices of the space that the
	/// personal space, `personal`, states says: a device that dropped a space held it, and
	/// one that declined it never did.
	fn held_before(&self, space: SpaceId, personal: &Personal) -> bool {
		let own = self.replica.signing.verifying_key();
		personal
			.choices
			.iter()
			.filter(|(record, _)| *record == self.info.replica)
			.filter_map(|(_, text)| Choice::from_text(text))
			.any(|choice| {
				choice.space == space && choice.key == own && choice.holds && choice.verifies()
			})
	}

	/// The first key of a space that an entry gives this replica, through the personal space,
	/// `personal`, or a group it holds, whose hash the entry names, with the space's first
	/// change that the entry names.
	fn given_key(
		&self,
		space: SpaceId,
		personal: &Personal,
	) -> Result<Option<(nibkeep_crypto::Key, nibkeep_log::ChangeId)>, NodeError> {
		for entry in self.own_entries(personal) {
			if entry.space != space {
				continue;
			}
			if let Some(key) = entry.open(&self.replica.decryption, self.info.replica)
				&& key_hash(&key) == entry.hash
			{
				return Ok(Some((key, nibkeep_log::ChangeId::from_hash(entry.root))));
			}
		}
		let own = self.replica.signing.verifying_key();
		for group in self.held_groups() {
			if self.check_not_revoked(group).is_err() {
				continue;
			}
			let record = replica_record(group, &own);
			for (entry, root) in self.agreed_entries(group, record)? {
				if entry.space != space {
					continue;
				}
				if let Some(key) = entry.open(&self.replica.decryption, record)
					&& key_hash(&key) == entry.hash
				{
					return Ok(Some((key, root)));
				}
			}
		}
		Ok(None)
	}

	/// The groups that this replica holds.
	pub(super) fn held_groups(&self) -> Vec<SpaceId> {
		self.spaces
			.keys()
			.copied()
			.filter(|&space| space != self.info.personal && self.is_group(space).unwrap_or(false))
			.collect()
	}

	/// The requests of the user's other devices for the spaces that this device holds and
	/// may give: a device's newest choice of the space, in the personal space, holds it, and
	/// it was never given the space, nor is listed there; or the choice that stands there is
	/// a drop that its newer choice outranks. Shown only on a device that holds the space.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn space_requests(&self) -> Result<Vec<SpaceRequest>, NodeError> {
		let personal = self.info.personal;
		let read = self.read_personal()?;
		let roster = self.roster(personal)?;
		let given = read.given();
		let mut requests = Vec::new();
		let personal_open = self.space(personal)?;
		let personal_log = personal_open.stored.log();
		// Whether this replica may give each space, found once for each.
		let mut may_give_each: BTreeMap<SpaceId, bool> = BTreeMap::new();
		for ((space, _), (record, choice)) in Self::newest_choices(&read, &roster) {
			if !choice.holds
				|| record == self.info.replica
				|| space == personal
				|| !self.spaces.contains_key(&space)
			{
				continue;
			}
			let may_give = if let Some(may_give) = may_give_each.get(&space) {
				*may_give
			} else {
				let may_give = self.changes_there(space)?;
				may_give_each.insert(space, may_give);
				may_give
			};
			if !may_give
				|| personal_log.is_revoked(&choice.key, &personal_open.keys)?
				|| personal_log.is_ai_agent(&choice.key, &personal_open.keys)?
			{
				continue;
			}
			let open = self.space(space)?;
			let log = open.stored.log();
			let back =
				if let Some((number, false)) = log.standing_choice(&choice.key, &open.keys)? {
					if number >= choice.number {
						continue;
					}
					true
				} else {
					if given.contains(&(record, space)) || log.is_listed(&choice.key, &open.keys)? {
						continue;
					}
					false
				};
			let (device, app, _) = self.replica_names(record)?;
			requests.push(SpaceRequest {
				space,
				name: self.space_name(space)?,
				replica: record,
				device,
				app,
				back,
			});
		}
		Ok(requests)
	}

	/// Approves, on this device, the request of the device of the user's whose record in the
	/// personal space is `replica` for `space`, as [`Library::space_requests`] shows it: gives
	/// it the space, or, where it dropped the space, lists it there again, with every key
	/// made meanwhile, and gives it the space too where it was never given it, as the device
	/// that made it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoRequest`] if that device asks for no such space here, and an
	/// error if a change cannot be saved.
	pub fn allow_space(
		&mut self,
		space: SpaceId,
		replica: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let request = self
			.space_requests()?
			.into_iter()
			.find(|request| request.space == space && request.replica == replica)
			.ok_or(NodeError::NoRequest)?;
		let given = self.read_personal()?.given().contains(&(replica, space));
		if request.back {
			self.list_again(space, replica, wall_millis, rng)?;
			// A device that deleted the space, as after a drop, takes it again by its entry,
			// which the device that made the space never had.
			if given {
				return Ok(());
			}
		}
		let roster = self.roster(self.info.personal)?;
		let encryption = roster
			.encryption_of(replica)
			.cloned()
			.ok_or(NodeError::NoRequest)?;
		let operation = self
			.entry_for(space, (replica, &encryption), rng)?
			.ok_or(NodeError::NoSpace(space))?;
		self.commit(self.info.personal, vec![operation], wall_millis, rng)
	}

	/// Brings the names of each space that this replica holds up to date in the personal
	/// space, on the record derived from the personal space and the space, so that every
	/// device of the user's shows it by name: in one change, for those whose names there
	/// differ from the space's own.
	pub(crate) fn settle_space_names(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let mut operations = Vec::new();
		for space in self.spaces() {
			// Only a space or group of the user's: not one whose member left, nor a group
			// whose log it keeps read only, to follow it in a space.
			if space == personal
				|| self.has_left(space)?
				|| self.own_role(space)? == crate::MemberRole::None
			{
				continue;
			}
			let open = self.space(space)?;
			let Some(entity) = open.own_entity()? else {
				continue;
			};
			let names: Vec<Value> = open
				.stored
				.log()
				.state()
				.current(entity, &fields::NAME)?
				.into_iter()
				.map(|statement| statement.value.clone())
				.collect();
			if names.is_empty() {
				continue;
			}
			let record = space_names_record(personal, space);
			let personal_open = self.space(personal)?;
			let state = personal_open.stored.log().state();
			let kept = state.current(record, &fields::NAME)?;
			// The same names, whatever their order or how many statements hold each, as two
			// devices that write them at once each write them once.
			let same = names
				.iter()
				.all(|name| kept.iter().any(|statement| statement.value == *name))
				&& kept
					.iter()
					.all(|statement| names.contains(&statement.value));
			if same {
				continue;
			}
			if !state.types(record)?.contains(&SPACE_NAMES) {
				for (field, value) in [
					(fields::TYPE, Value::Text(SPACE_NAMES.to_string())),
					(OF_SPACE, Value::Text(space.to_string())),
				] {
					operations.push(Operation::assert(Statement::new(
						StatementId::random(rng),
						record,
						field,
						value,
					)));
				}
			}
			let replaced: Vec<StatementId> = kept.iter().map(|statement| statement.id).collect();
			for (at, name) in names.into_iter().enumerate() {
				let statement =
					Statement::new(StatementId::random(rng), record, fields::NAME, name);
				operations.push(if at == 0 {
					Operation::replace(statement, replaced.clone())
				} else {
					Operation::assert(statement)
				});
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(personal, operations, wall_millis, rng)
	}

	/// The names that the personal space keeps of the user's spaces: by space, its name as
	/// this library's languages show it.
	pub(super) fn kept_space_names(&self) -> Result<BTreeMap<SpaceId, Option<String>>, NodeError> {
		let personal = self.info.personal;
		let open = self.space(personal)?;
		let state = open.stored.log().state();
		let mut names = BTreeMap::new();
		for record in open
			.stored
			.find(&fields::TYPE, &Value::Text(SPACE_NAMES.to_string()))?
		{
			if state.is_deleted(record)? {
				continue;
			}
			let Some(space) = state
				.current(record, &OF_SPACE)?
				.iter()
				.find_map(|statement| statement.value.as_text()?.parse::<SpaceId>().ok())
			else {
				continue;
			};
			// Only on the record derived for the space, which no other space shares.
			if record != space_names_record(personal, space) {
				continue;
			}
			names.insert(
				space,
				display_name(state, &open.types, &self.languages, record)?,
			);
		}
		Ok(names)
	}

	/// Every space of the user's but the personal one, whether this device holds it or not,
	/// as the personal space knows them: those it holds, those given to any device of the
	/// user's, those whose names it keeps, and those that a device chose; each with its
	/// name, what this device did with it, and the devices that hold it. A space that the
	/// user left is left out. By name, then by ID.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn known_spaces(&self) -> Result<Vec<KnownSpace>, NodeError> {
		let personal = self.info.personal;
		let read = self.read_personal()?;
		let roster = self.roster(personal)?;
		let names = self.kept_space_names()?;
		let own = self.own_choices(&read);
		let newest = Self::newest_choices(&read, &roster);
		// What the personal space knows of: the spaces given to any device or code, named,
		// or chosen.
		let mut spaces: BTreeSet<SpaceId> = read
			.entries
			.iter()
			.filter_map(|(_, text)| Some(SpaceEntry::from_text(text)?.space))
			.collect();
		spaces.extend(names.keys().copied());
		spaces.extend(newest.keys().map(|(space, _)| *space));
		// An AI agent's replica knows only those it holds, so that no AI agent reads the names
		// of the spaces not chosen for it.
		if self.is_ai_agent()? {
			spaces.clear();
		}
		// And those held here where this replica's user has a role, but no group whose log it
		// keeps read only, to follow it in a space.
		for &space in self.spaces.keys() {
			if !spaces.contains(&space) && self.own_role(space)? != crate::MemberRole::None {
				spaces.insert(space);
			}
		}
		spaces.remove(&personal);
		let given = self.given_here(&read)?;
		let mut known = Vec::new();
		for space in spaces {
			// On this device, and not dropped there; a space whose member left, or was
			// removed, is no longer the user's.
			let on_device = self.spaces.contains_key(&space);
			let held = self.holds_here(space)?;
			if held && self.has_left(space)? {
				continue;
			}
			let holders = self.space_holders((space, held), &newest)?;
			let name = if on_device {
				self.space_name(space)?
			} else {
				names.get(&space).cloned().flatten()
			};
			let here = match own.get(&space).copied() {
				_ if held => Here::Held,
				Some((_, false)) => Here::Declined,
				_ if given.contains(&space) => Here::Given,
				Some((_, true)) => Here::Asked,
				None => Here::Elsewhere,
			};
			known.push(KnownSpace {
				space,
				name,
				here,
				holders,
				dropping: on_device && self.is_dropping(space),
			});
		}
		known.sort_by_key(|known| by_name(known.name.as_deref(), known.space));
		Ok(known)
	}

	/// The spaces given to this replica, through the personal space, `personal`, or a group
	/// that it holds.
	fn given_here(&self, personal: &Personal) -> Result<BTreeSet<SpaceId>, NodeError> {
		let mut given: BTreeSet<SpaceId> = self
			.own_entries(personal)
			.into_iter()
			.map(|entry| entry.space)
			.collect();
		let own_key = self.replica.signing.verifying_key();
		for group in self.held_groups() {
			let record = replica_record(group, &own_key);
			given.extend(
				self.agreed_entries(group, record)?
					.into_iter()
					.map(|(entry, _)| entry.space),
			);
		}
		Ok(given)
	}

	/// The devices of the user's that hold `space`, as their newest choices, `newest`, say,
	/// but those revoked, and this one first where it holds it, `held`.
	pub(super) fn space_holders(
		&self,
		(space, held): (SpaceId, bool),
		newest: &BTreeMap<(SpaceId, [u8; 32]), (EntityId, Choice)>,
	) -> Result<Vec<SpaceHolder>, NodeError> {
		let personal_open = self.space(self.info.personal)?;
		let personal_log = personal_open.stored.log();
		let mut holders = Vec::new();
		if held {
			let (device, app, _) = self.replica_names(self.info.replica)?;
			holders.push(SpaceHolder {
				replica: self.info.replica,
				device,
				app,
				this: true,
			});
		}
		for ((chosen, _), (record, choice)) in newest {
			if *chosen != space
				|| !choice.holds
				|| *record == self.info.replica
				|| personal_log.is_revoked(&choice.key, &personal_open.keys)?
				|| holders.iter().any(|holder| holder.replica == *record)
			{
				continue;
			}
			let (device, app, _) = self.replica_names(*record)?;
			holders.push(SpaceHolder {
				replica: *record,
				device,
				app,
				this: false,
			});
		}
		Ok(holders)
	}

	/// Finds a space of the user's, held here or not, by its name, in any case, by its ID,
	/// or by the start of its ID, as [`Library::known_spaces`] knows them, and as
	/// [`Library::find_space`] finds those held here.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpaceNamed`] if no space matches, and
	/// [`NodeError::SpacesNamed`] if several do.
	pub fn find_known_space(&self, reference: &str) -> Result<SpaceId, NodeError> {
		match self.find_space(reference) {
			Err(NodeError::NoSpaceNamed(_)) => {}
			found => return found,
		}
		let lowered = reference.to_lowercase();
		let known = self.known_spaces()?;
		let by_name: Vec<SpaceId> = known
			.iter()
			.filter(|known| {
				known
					.name
					.as_ref()
					.is_some_and(|name| name.to_lowercase() == lowered)
			})
			.map(|known| known.space)
			.collect();
		let by_id: Vec<SpaceId> = known
			.iter()
			.filter(|known| lowered.len() >= 4 && known.space.to_string().starts_with(&lowered))
			.map(|known| known.space)
			.collect();
		let found = if by_name.is_empty() { by_id } else { by_name };
		match found.as_slice() {
			[] => Err(NodeError::NoSpaceNamed(reference.to_owned())),
			[space] => Ok(*space),
			_ => Err(NodeError::SpacesNamed(reference.to_owned())),
		}
	}
}

/// What orders spaces for people: by name, in any case, those without one last, then by ID.
pub(super) fn by_name(name: Option<&str>, space: SpaceId) -> (bool, Option<String>, SpaceId) {
	(name.is_none(), name.map(str::to_lowercase), space)
}
