//! Spaces of one's own besides the personal one: making, naming and finding them, finding
//! entities in any of them, and how a space reaches every replica of the user but AI
//! agents': the personal space gives each its first key, each asks to be listed with a
//! listing its own key signed, and a replica listed there lists it
//! ([Identity](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/identity.md#8-spaces-of-ones-own),
//! [technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Key, VerifyingKey, key_hash};
use nibkeep_governance::{
	Choice, Code, LISTING, Listing, NewSpace, RecoveryListing, Roster, SPACE, SPACE_ENTRY,
	SpaceEntry, SpaceReplica, ask_to_be_listed, found_space, list_replica, replica_record,
	state_choice, state_recovery_code,
};
use nibkeep_model::{Operation, fields};
use nibkeep_store::log_path;

use super::entities::{Listed, display_name};
use super::{Library, Space, code_types};
use crate::NodeError;

/// What the personal space says of the user's spaces: the entries that give them, the
/// listings that ask to be listed in them, and the devices' choices of them, each as
/// `(record, text)`; and what the devices say of the spaces given to them later, each as
/// `(record, author, takes)`, in the order of the changes.
#[derive(Default)]
pub(super) struct Personal {
	pub(super) entries: Vec<(EntityId, String)>,
	pub(super) listings: Vec<(EntityId, String)>,
	pub(super) choices: Vec<(EntityId, String)>,
	pub(super) marks: Vec<(EntityId, VerifyingKey, bool)>,
}

impl Personal {
	/// The spaces that the entries give each replica or code, as `(record, space)`.
	pub(super) fn given(&self) -> BTreeSet<(EntityId, SpaceId)> {
		self.entries
			.iter()
			.filter_map(|(record, text)| Some((*record, SpaceEntry::from_text(text)?.space)))
			.collect()
	}
}

/// The names of a replica's device and app, and its endpoint, as the personal space has
/// them.
type ReplicaNames = (Option<String>, Option<String>, Option<[u8; 32]>);

impl Library {
	/// The spaces this replica holds: the personal space first, then the others by ID.
	#[must_use]
	pub fn spaces(&self) -> Vec<SpaceId> {
		let personal = self.info.personal;
		std::iter::once(personal)
			.chain(
				self.spaces
					.keys()
					.copied()
					.filter(|&space| space != personal),
			)
			.collect()
	}

	/// Chooses the space that new entities go to for as long as the library is open, and
	/// the only one that [`Library::locate`] looks in; with none, new entities go to the
	/// personal space, and every space is looked in.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpace`] if this library holds no such space.
	pub fn choose_space(&mut self, space: Option<SpaceId>) -> Result<(), NodeError> {
		if let Some(space) = space {
			self.space(space)?;
		}
		self.chosen = space;
		Ok(())
	}

	/// This replica's record in a space: in a space with members, the one derived from the
	/// space and its key; in the personal space, the one the library file names.
	pub(crate) fn own_record(&self, space: SpaceId) -> Result<EntityId, NodeError> {
		if self.has_members(space)? {
			Ok(replica_record(space, &self.replica.signing.verifying_key()))
		} else {
			Ok(self.info.replica)
		}
	}

	/// Whether a space has members, as every space but the personal one does.
	pub(crate) fn has_members(&self, space: SpaceId) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		Ok(space != self.info.personal && open.stored.log().has_members(&open.keys)?)
	}

	/// The spaces that may hold secret values: every space, the personal one first.
	pub(crate) fn secret_spaces(&self) -> Result<Vec<SpaceId>, NodeError> {
		self.space(self.info.personal)?;
		Ok(self.spaces())
	}

	/// The space chosen for this session, if one is.
	#[must_use]
	pub const fn chosen_space(&self) -> Option<SpaceId> {
		self.chosen
	}

	/// The space that holds an entity, of those this replica holds: the first, the personal
	/// space first, where the entity has a statement, or was merged into another.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn space_of(&self, entity: EntityId) -> Result<Option<SpaceId>, NodeError> {
		for space in self.spaces() {
			let state = self.space(space)?.stored.log().state();
			let canonical = state.canonical(entity)?;
			if canonical != entity || !state.fields(entity)?.is_empty() {
				return Ok(Some(space));
			}
		}
		Ok(None)
	}

	/// The space that new entities go to: the one chosen, or else the personal space.
	#[must_use]
	pub fn default_space(&self) -> SpaceId {
		self.chosen.unwrap_or(self.info.personal)
	}

	/// A space's name, as this library's languages show it, if it has one: the name kept
	/// for it in the personal space where its own has not come yet, as on a device that took
	/// the space and has not synced it since.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn space_name(&self, space: SpaceId) -> Result<Option<String>, NodeError> {
		if let Some(name) = self.space(space)?.name(&self.languages)? {
			return Ok(Some(name));
		}
		if space == self.info.personal {
			return Ok(None);
		}
		Ok(self.kept_space_names()?.remove(&space).flatten())
	}

	/// Finds a space by its name, in any case, by its ID, or by the start of its ID.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpaceNamed`] if no space matches, and
	/// [`NodeError::Ambiguous`]-like [`NodeError::SpacesNamed`] if several do.
	pub fn find_space(&self, reference: &str) -> Result<SpaceId, NodeError> {
		let lowered = reference.to_lowercase();
		let mut by_name = Vec::new();
		let mut by_id = Vec::new();
		for space in self.spaces() {
			if self
				.space_name(space)?
				.is_some_and(|name| name.to_lowercase() == lowered)
			{
				by_name.push(space);
			}
			if lowered.len() >= 4 && space.to_string().starts_with(&lowered) {
				by_id.push(space);
			}
		}
		let found = if by_name.is_empty() { by_id } else { by_name };
		match found.as_slice() {
			[] => Err(NodeError::NoSpaceNamed(reference.to_owned())),
			[space] => Ok(*space),
			_ => Err(NodeError::SpacesNamed(reference.to_owned())),
		}
	}

	/// Makes a space besides the personal one, named `name`: this replica founds it, with
	/// the user as its member and first admin, states that it holds it, and lists it in the
	/// personal space with its first key sealed to each of the user's devices that take new
	/// spaces, as computers do, so that they hold it too, and returns its ID
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	///
	/// # Errors
	///
	/// Returns an error if the name is empty, this replica is an AI agent's or revoked, or
	/// the keys or the changes cannot be saved.
	pub fn create_space(
		&mut self,
		name: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<SpaceId, NodeError> {
		self.make_space(name, (false, &[]), wall_millis, rng)
	}

	/// Makes a space as [`Library::create_space`] does, and gives it too to the devices of
	/// the user's that the person chose, `also`, by their records in the personal space,
	/// besides those that take new spaces
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	///
	/// # Errors
	///
	/// The errors of [`Library::create_space`].
	pub fn create_space_on(
		&mut self,
		name: &str,
		also: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<SpaceId, NodeError> {
		self.make_space(name, (false, also), wall_millis, rng)
	}

	/// Makes a group, named `name`: a space with members and no data of its own, which can be
	/// a member of spaces, as an editor or a viewer, so that whoever joins the group joins
	/// each of them ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#138-groups)).
	/// This replica founds it, with the user as its first admin, as [`Library::create_space`]
	/// founds a space, and returns its ID.
	///
	/// # Errors
	///
	/// The errors of [`Library::create_space`].
	pub fn create_group(
		&mut self,
		name: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<SpaceId, NodeError> {
		self.make_space(name, (true, &[]), wall_millis, rng)
	}

	/// Makes a group as [`Library::create_group`] does, and gives it too to the devices of
	/// the user's that the person chose, as [`Library::create_space_on`] does.
	///
	/// # Errors
	///
	/// The errors of [`Library::create_space`].
	pub fn create_group_on(
		&mut self,
		name: &str,
		also: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<SpaceId, NodeError> {
		self.make_space(name, (true, also), wall_millis, rng)
	}

	/// Makes a space, or with `group` a group, as [`Library::create_space`] says, giving it
	/// to the devices that take new spaces and to those of `also`.
	fn make_space(
		&mut self,
		name: &str,
		(group, also): (bool, &[EntityId]),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<SpaceId, NodeError> {
		if name.trim().is_empty() {
			return Err(NodeError::EmptyName);
		}
		let personal = self.info.personal;
		self.check_not_revoked(personal)?;
		if self.is_ai_agent()? {
			return Err(NodeError::AiAgentMayNot("make spaces"));
		}
		let space = SpaceId::random(rng);
		let first = Key::generate(rng);
		let member = EntityId::random(rng);
		let signing = &self.replica.signing;
		let encryption = self.replica.decryption.encryption_key();
		let listing = Listing::sign(signing, &encryption, space, (member, false));
		let user_name = self.own_name(self.info.user)?.unwrap_or_default();
		let (device_name, app_name) = self.own_names()?;
		let endpoint = self.endpoint();
		let founded = found_space(
			&NewSpace {
				name,
				user: self.info.user,
				user_name: &user_name,
				member,
				replica: SpaceReplica {
					record: listing.record,
					device_record: nibkeep_governance::device_record(
						space,
						&signing.verifying_key(),
					),
					member,
					signing: &signing.verifying_key(),
					encryption: &encryption,
					listing: &listing.to_text(),
					endpoint: Some(&endpoint),
					device: &device_name,
					app: &app_name,
					ai_agent: false,
				},
				group,
			},
			rng,
		);
		self.add_space(space, &first, rng)?;
		if let Err(error) = self.found(space, founded, listing.record, wall_millis, rng) {
			// A space whose first change was not made would be no space at all.
			if self
				.space(space)
				.is_ok_and(|open| open.stored.log().root().ok().flatten().is_none())
			{
				self.remove_space(space, rng);
			}
			return Err(error);
		}
		// The devices that take new spaces get it now, and those that the person chose; one
		// that misses it, as the change fails, asks for it. Each knows it by name, or after
		// the next sync.
		let _ = self.give_new_space(space, also, wall_millis, rng);
		let _ = self.settle_space_names(wall_millis, rng);
		self.end_bursts();
		Ok(space)
	}

	/// Commits the changes of a burst in each space, as closing the library does: after
	/// several changes made at once, so that another program can open the library at once.
	pub(crate) fn end_bursts(&mut self) {
		for open in self.spaces.values_mut() {
			open.stored.end_burst();
		}
	}

	/// Commits a new space's first changes: the one that governs it, then the rest, each
	/// about one thing.
	fn found(
		&mut self,
		space: SpaceId,
		founded: nibkeep_governance::FoundedSpace,
		record: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let code_types = code_types();
		let mut operations = founded.operations;
		for definition in nibkeep_std::types() {
			operations.extend(code_types.define(&definition, rng));
		}
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let secret = nibkeep_std::secret_fields().iter().cloned();
		let mut governing = founded.governing;
		governing.extend(open.secret_marks(&code_types, (record, secret), rng)?);
		for operations in [governing, operations] {
			open.commit(
				&self.replica.signing,
				&code_types,
				operations,
				wall_millis,
				rng,
			)?;
		}
		self.read_again(space)
	}

	/// Renames a space besides the personal one, whose name stays.
	///
	/// # Errors
	///
	/// Returns an error if the space is the personal one or this library has none such, the
	/// name is empty, or the change cannot be saved.
	pub fn rename_space(
		&mut self,
		space: SpaceId,
		name: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if name.trim().is_empty() {
			return Err(NodeError::EmptyName);
		}
		if space == self.info.personal {
			return Err(NodeError::PersonalSpace);
		}
		let open = self.space(space)?;
		let Some(entity) = open.own_entity()? else {
			return Err(NodeError::Damaged(format!(
				"the space {space} has no record"
			)));
		};
		let current: Vec<_> = open
			.stored
			.log()
			.state()
			.current(entity, &fields::NAME)?
			.iter()
			.map(|statement| statement.id)
			.collect();
		let operation = nibkeep_governance::rename_space(entity, name, current, rng);
		self.commit(space, vec![operation], wall_millis, rng)?;
		// The other devices show the new name, or after the next sync.
		let _ = self.settle_space_names(wall_millis, rng);
		Ok(())
	}

	/// Searches every space for what holds the words, as [`Library::search`] does in one:
	/// what each finds, with the space, the personal space first.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn search_everywhere(&self, query: &str) -> Result<Vec<(SpaceId, Listed)>, NodeError> {
		let mut found = Vec::new();
		for space in self.spaces() {
			found.extend(
				self.search(space, query)?
					.into_iter()
					.map(|listed| (space, listed)),
			);
		}
		Ok(found)
	}

	/// Finds an entity in whichever space holds it, or in the space chosen, if one is, by its
	/// ID, the start of its ID, or its name, as [`Library::find_entity`] does in one space.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if no space has it, and [`NodeError::Ambiguous`] if
	/// several entities match, in one space or in several.
	pub fn locate(&self, reference: &str) -> Result<(SpaceId, EntityId), NodeError> {
		self.locate_with(reference, |library, space| {
			library.find_entity(space, reference)
		})
	}

	/// Finds an entity as [`Library::locate`] does, except that a name that finds no entity
	/// that is not deleted finds deleted ones, as [`Library::find_entity_or_deleted`] does.
	///
	/// # Errors
	///
	/// As [`Library::locate`].
	pub fn locate_or_deleted(&self, reference: &str) -> Result<(SpaceId, EntityId), NodeError> {
		self.locate_with(reference, |library, space| {
			library.find_entity_or_deleted(space, reference)
		})
	}

	fn locate_with(
		&self,
		reference: &str,
		find: impl Fn(&Self, SpaceId) -> Result<EntityId, NodeError>,
	) -> Result<(SpaceId, EntityId), NodeError> {
		let mut found = Vec::new();
		let mut candidates = Vec::new();
		let spaces = match self.chosen {
			Some(chosen) => vec![chosen],
			None => self.spaces(),
		};
		for space in spaces {
			// Of several that a name finds in one space, those moved away, where this replica
			// holds where they went, are found there instead.
			let result = match find(self, space) {
				Err(NodeError::Ambiguous {
					reference: named,
					candidates: more,
				}) if self.chosen.is_none() => {
					let mut staying = Vec::new();
					for candidate in more {
						if self.moved_into(space, candidate)?.is_none() {
							staying.push(candidate);
						}
					}
					match staying.as_slice() {
						[] => Err(NodeError::NoEntity(named)),
						[only] => Ok(*only),
						_ => Err(NodeError::Ambiguous {
							reference: named,
							candidates: staying,
						}),
					}
				}
				other => other,
			};
			match result {
				// An entity moved into a space this replica holds is found there, by its ID;
				// by a name, it is found as it is named there, if it is. With a space chosen,
				// the entity there is found, as it stays to be shown or purged.
				Ok(entity) => {
					let found_here = match self.moved_into(space, entity)? {
						Some(moved) if self.chosen.is_none() => {
							if !reference.parse::<EntityId>().is_ok_and(|id| id == entity) {
								continue;
							}
							moved
						}
						_ => (space, entity),
					};
					if !found.contains(&found_here) {
						found.push(found_here);
					}
				}
				Err(NodeError::NoEntity(_)) => {}
				Err(NodeError::Ambiguous {
					candidates: more, ..
				}) => candidates.extend(more),
				Err(error) => return Err(error),
			}
		}
		if candidates.is_empty() && found.len() == 1 {
			return Ok(found[0]);
		}
		// A user, device, replica or space by its name is the personal space's, where other
		// spaces hold records of them too, by the same names.
		if candidates.is_empty() && self.chosen.is_none() {
			let personal: Vec<_> = found
				.iter()
				.filter(|(space, _)| *space == self.info.personal)
				.collect();
			let elsewhere_records = found
				.iter()
				.filter(|(space, _)| *space != self.info.personal)
				.try_fold(true, |all, &(space, entity)| {
					Ok::<_, NodeError>(all && self.is_library_record(space, entity)?)
				})?;
			if let [only] = personal.as_slice()
				&& elsewhere_records
				&& self.is_library_record(only.0, only.1)?
			{
				return Ok(**only);
			}
		}
		if candidates.is_empty() && found.is_empty() {
			return Err(NodeError::NoEntity(reference.to_owned()));
		}
		candidates.extend(found.iter().map(|(_, entity)| *entity));
		Err(NodeError::Ambiguous {
			reference: reference.to_owned(),
			candidates,
		})
	}

	/// Whether an entity is one of the library's own records of who and where: a user, a
	/// device, a replica or a space.
	fn is_library_record(&self, space: SpaceId, entity: EntityId) -> Result<bool, NodeError> {
		let records = [
			nibkeep_governance::USER,
			nibkeep_governance::DEVICE,
			nibkeep_governance::REPLICA,
			SPACE,
		];
		Ok(self
			.own_types(space, entity)?
			.iter()
			.any(|kind| records.contains(kind)))
	}

	/// Settles the spaces besides the personal one after a sync: it takes the spaces the
	/// personal space gives it, as its choices and its mark for new spaces say, asks to be
	/// listed in those it is not listed in, lists the replicas of its user that asked in
	/// those it is listed in, carries its user's devices' drops there, gives the spaces it
	/// holds to the recovery codes that lack them, and brings their names up to date in the
	/// personal space. An AI agent's replica only takes the spaces given it, those chosen
	/// for it, and asks, lists and gives nothing.
	pub(crate) fn settle_spaces(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let own = self.replica.signing.verifying_key();
		let ai_agent = {
			let open = self.space(personal)?;
			let log = open.stored.log();
			if !log.is_listed(&own, &open.keys)? || log.is_revoked(&own, &open.keys)? {
				return Ok(());
			}
			log.is_ai_agent(&own, &open.keys)?
		};
		// An AI agent's replica only takes the spaces chosen for it, as the replica that
		// paired it gave them, and lists, asks and gives nothing.
		if ai_agent {
			let read = self.read_personal()?;
			return self.take_space_entries(&read, wall_millis, rng);
		}
		// What the personal space says is read once: what the steps add to it, the requests
		// of this replica, is nothing the later steps take.
		let read = self.read_personal()?;
		if read.entries.is_empty() && self.spaces.len() == 1 {
			return Ok(());
		}
		let roster = self.roster(personal)?;
		// Each step goes on whatever another's error; the first is told at the end.
		let results = [
			self.take_space_entries(&read, wall_millis, rng),
			self.ask_to_be_listed(&read, wall_millis, rng),
			self.revoke_in_spaces(wall_millis, rng),
			self.carry_codes_of(&roster, wall_millis, rng),
			self.list_who_asked(&read, &roster, wall_millis, rng),
			self.carry_choices(&read, &roster, wall_millis, rng),
			self.give_space_entries_with(&read, &roster, wall_millis, rng),
			self.settle_space_names(wall_millis, rng),
		];
		results.into_iter().collect()
	}

	/// Revokes, in each space besides the personal one that this replica is listed in, the
	/// keys of its member's replicas that the personal space revokes, each space with a new
	/// key, as a revocation there does: a lost device is revoked in every space of its user.
	/// So are the devices that a revoked one added there and that the personal space does
	/// not list, as [`Library::revoke_in_spaces_telling`] says.
	pub(crate) fn revoke_in_spaces(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.revoke_in_spaces_telling(wall_millis, rng).1
	}

	/// Revokes in each space as [`Library::revoke_in_spaces`] says, with the replicas of this
	/// replica's member there that the personal space does not list, and that a replica it
	/// revokes added, there or in another space or group, or one so added in turn, once every
	/// other device of the person's that remains has told this one of all it held after it
	/// learned of the revocations: the personal space no longer counts what a revoked
	/// replica listed that its revocation does not build on, so nothing tells such a device
	/// from one that a thief paired on the lost one, and no device of the person's could name
	/// it to revoke it. One that a replica the personal space lists and does not revoke added
	/// too stays, as do those that it added in turn; so does one added by itself alone, as on
	/// a record made for it, as nothing tells whose it is. Returns the spaces and groups
	/// where it revoked such a replica, and the first error, as a space where it fails is
	/// left until the next sync.
	pub(crate) fn revoke_in_spaces_telling(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> (Vec<SpaceId>, Result<(), NodeError>) {
		let personal = self.info.personal;
		// What cannot be read now is decided after a later sync, and told.
		let (ended, mut first_error) = match self.ended_by_revocations() {
			Ok(ended) => (ended, Ok(())),
			Err(error) => (BTreeSet::new(), Err(error)),
		};
		let mut elsewhere = Vec::new();
		for space in self.spaces() {
			if space == personal {
				continue;
			}
			match self.revoke_in_space(space, &ended, wall_millis, rng) {
				Ok(true) => elsewhere.push(space),
				Ok(false) => {}
				Err(error) => {
					if first_error.is_ok() {
						first_error = Err(error);
					}
				}
			}
		}
		(elsewhere, first_error)
	}

	/// The keys, of this replica's member in a space or group of its own and of no replica
	/// that the personal space lists, that a replica it revokes added, as
	/// [`Library::revoke_in_spaces_telling`] says: none while the personal space revokes
	/// nothing, while no space lists such a key, and while another device of the person's,
	/// but an AI agent's, has not told yet of all it held after it learned of every
	/// revocation, as its latest account in the personal space says.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	fn ended_by_revocations(&self) -> Result<BTreeSet<[u8; 32]>, NodeError> {
		let personal = self.info.personal;
		let (mut standing, mut revoked) = (BTreeSet::new(), BTreeSet::new());
		// The keys of each other replica that remains, but AI agents', which add no device.
		let mut others: Vec<Vec<VerifyingKey>> = Vec::new();
		{
			let roster = self.roster(personal)?;
			let open = self.space(personal)?;
			let log = open.stored.log();
			for record in roster.replicas() {
				let keys = roster.keys_of(record);
				let (mut stands, mut ai_agent) = (false, false);
				for key in keys {
					if log.is_revoked(key, &open.keys)? {
						revoked.insert(*key.as_bytes());
					} else {
						standing.insert(*key.as_bytes());
						stands = true;
					}
					ai_agent |= log.is_ai_agent(key, &open.keys)?;
				}
				if stands && !ai_agent && record != self.info.replica {
					others.push(keys.to_vec());
				}
			}
		}
		if revoked.is_empty() {
			return Ok(BTreeSet::new());
		}
		let known = |key: &[u8; 32]| standing.contains(key) || revoked.contains(key);
		let (listers, waiting) = self.own_listers(&known)?;
		if !waiting {
			return Ok(BTreeSet::new());
		}
		if !self.told_since_revoked(&others, &revoked)? {
			return Ok(BTreeSet::new());
		}
		Ok(ended_by_revoked(&listers, &standing, &revoked))
	}

	/// In each space and group of this replica's but the personal space, the keys that added
	/// each key of this replica's member there, by the counting changes that list it, or, for
	/// a key listed by a recovery code, the replica that named the code there; and whether a
	/// key that `known` leaves out, of no replica that the personal space lists, is listed
	/// there and not revoked.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	fn own_listers(&self, known: &dyn Fn(&[u8; 32]) -> bool) -> Result<(Listers, bool), NodeError> {
		let personal = self.info.personal;
		let mut listers = Listers::new();
		let mut waiting = false;
		for space in self.spaces() {
			if space == personal {
				continue;
			}
			let Some(member) = self.member_in(space)? else {
				continue;
			};
			let roster = self.roster(space)?;
			let open = self.space(space)?;
			let log = open.stored.log();
			let mut added: Vec<(VerifyingKey, [u8; 32])> = Vec::new();
			for governing in log.governing_statements(&open.keys)? {
				if !governing.counts {
					continue;
				}
				let text = governing.statement.value.as_text();
				if governing.lists
					&& governing.statement.field == fields::SIGNING_KEY
					&& let Some(key) = text.and_then(VerifyingKey::from_text)
				{
					added.push((key, *governing.author.as_bytes()));
				} else if governing.statement.field == fields::RECOVERY_LISTING
					&& let Some(listing) = text.and_then(RecoveryListing::from_text)
					&& listing.space == space
					&& listing.verifies()
					&& let Some(code) = roster.code(listing.code_record())
				{
					// Listed by a code: added by whoever named the code there.
					added.push((listing.key, *code.author.as_bytes()));
				}
			}
			for (key, by) in added {
				if self.listing_owner(space, &key)? != Some(member) {
					continue;
				}
				listers.entry(*key.as_bytes()).or_default().insert(by);
				waiting |= !known(key.as_bytes()) && !log.is_revoked(&key, &open.keys)?;
			}
		}
		Ok((listers, waiting))
	}

	/// Whether each of `others`, the keys of each other replica of the person's that remains,
	/// has told this one of all it held after it learned of the revocations of the keys of
	/// `revoked`: whether its latest account in the personal space names heads that this
	/// replica holds, and that build on each of those revocations.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	fn told_since_revoked(
		&self,
		others: &[Vec<VerifyingKey>],
		revoked: &BTreeSet<[u8; 32]>,
	) -> Result<bool, NodeError> {
		let personal = self.info.personal;
		let open = self.space(personal)?;
		let log = open.stored.log();
		let mut revocations: BTreeSet<nibkeep_log::ChangeId> = BTreeSet::new();
		for bytes in revoked {
			if let Some(key) = VerifyingKey::from_bytes(bytes) {
				revocations.extend(log.revocations_of(Some(&key), &open.keys)?);
			}
		}
		let heads = self.other_heads(personal)?;
		for keys in others {
			let mut told = false;
			for (author, head) in &heads {
				if told || !keys.iter().any(|key| key.as_bytes() == author) {
					continue;
				}
				// Held here and applied, as a change whose payload is still to come lists
				// nothing yet.
				let mut all = true;
				for &held in &head.heads {
					all = all && log.is_complete(held)?;
				}
				for &revocation in &revocations {
					if !all {
						break;
					}
					let mut known = false;
					for &held in &head.heads {
						if log.builds_on_all(held, &[revocation])? {
							known = true;
							break;
						}
					}
					all = known;
				}
				told = all;
			}
			if !told {
				return Ok(false);
			}
		}
		Ok(true)
	}

	/// Carries the person's recovery codes into each space besides the personal one that this
	/// replica is listed in, a group too: it names there, on its member's record for each,
	/// the codes that stand in the personal space and that the space lacks, and revokes
	/// there, with a new key, those that the personal space revokes. Only a member's own
	/// replicas name or revoke their codes, so each user's replicas carry their own
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	pub(crate) fn carry_codes(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let roster = self.roster(self.info.personal)?;
		self.carry_codes_of(&roster, wall_millis, rng)
	}

	/// Carries the codes that the personal space's roster `roster` names, as
	/// [`Library::carry_codes`] does.
	fn carry_codes_of(
		&mut self,
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let codes: Vec<Code> = roster.codes().cloned().collect();
		if codes.is_empty() {
			return Ok(());
		}
		let mut first_error = Ok(());
		for space in self.spaces() {
			if space == personal {
				continue;
			}
			let carried = self.carry_codes_into(space, &codes, wall_millis, rng);
			if first_error.is_ok() {
				first_error = carried;
			}
		}
		first_error
	}

	/// Carries the codes into one space, as [`Library::carry_codes`] does.
	fn carry_codes_into(
		&mut self,
		space: SpaceId,
		codes: &[Code],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		// Only where this replica is listed for a member, of the space itself or of a group
		// that gives it a role there, whose record the code's record there is derived from,
		// and who did not leave.
		let Some(member) = self.code_member_in(space)? else {
			return Ok(());
		};
		if self.has_left(space)? {
			return Ok(());
		}
		let roster = self.roster(space)?;
		// The codes to name there, on that member's record, unless another record of the
		// member's that gives them a role names it already, as one named before its member
		// was made direct; and those to revoke, on each record of the member's that names
		// them. The member's records are read only where a code needs them.
		let mut own_members = None;
		let mut naming = Vec::new();
		let mut revoking = Vec::new();
		for code in codes {
			let stated = code.statement.for_member(member);
			let record = stated.record(space);
			if code.stands() && roster.code(record).is_some() {
				continue;
			}
			if own_members.is_none() {
				own_members = Some(self.own_code_members(space)?);
			}
			let own_members = own_members.as_deref().unwrap_or_default();
			if code.stands() {
				let named_elsewhere = own_members.iter().any(|&own| {
					roster
						.code(code.statement.for_member(own).record(space))
						.is_some_and(Code::stands)
				});
				if !named_elsewhere {
					naming.push((record, stated));
				}
				continue;
			}
			for &own in own_members {
				let record = code.statement.for_member(own).record(space);
				if roster.code(record).is_some_and(Code::stands)
					&& !revoking.contains(&(record, code.statement.key))
				{
					revoking.push((record, code.statement.key));
				}
			}
		}
		if naming.is_empty() && revoking.is_empty() {
			return Ok(());
		}
		// But what this replica stated there before, whether it counts or not: it names and
		// revokes each code once, so that a statement set aside is not made at every sync.
		let own = self.replica.signing.verifying_key();
		let stated_before: BTreeSet<(EntityId, bool)> = {
			let open = self.space(space)?;
			open.stored
				.log()
				.governing_statements(&open.keys)?
				.into_iter()
				.filter(|governing| governing.author == own)
				.filter_map(|governing| {
					let field = &governing.statement.field;
					(*field == fields::RECOVERY_KEY || *field == fields::RECOVERY_REVOKED).then(
						|| {
							(
								governing.statement.entity,
								*field == fields::RECOVERY_REVOKED,
							)
						},
					)
				})
				.collect()
		};
		let naming: Vec<Operation> = naming
			.into_iter()
			.filter(|(record, _)| !stated_before.contains(&(*record, false)))
			.map(|(_, stated)| state_recovery_code(space, &stated, rng))
			.collect();
		revoking.retain(|(record, _)| !stated_before.contains(&(*record, true)));
		if !naming.is_empty() {
			self.commit(space, naming, wall_millis, rng)?;
			// The keys given out there so far, as a replica that lacks them is given them.
			self.give_keys(space, wall_millis, rng)?;
		}
		// Each record in a change of its own, with a new key: a revocation that the log sets
		// aside on one record, as on a record in a group that a removal from the group took
		// the member's path through, leaves those on the others standing.
		for revoked in revoking {
			let roster = self.roster(space)?;
			self.rotate_revoking(space, &[], &[revoked], &roster, wall_millis, rng)?;
		}
		Ok(())
	}

	/// This replica's member in a space, for the codes it names there: its member in the
	/// space itself; or else, for a member through a group alone that has a role there, its
	/// member in the group that the space's rules name for its key, as history names it.
	fn code_member_in(&self, space: SpaceId) -> Result<Option<EntityId>, NodeError> {
		if let Some(member) = self.member_in(space)? {
			return Ok(Some(member));
		}
		if self.own_role(space)? == crate::MemberRole::None {
			return Ok(None);
		}
		let open = self.space(space)?;
		let own = self.replica.signing.verifying_key();
		Ok(open
			.stored
			.log()
			.group_member_of(&own, &open.keys)?
			.map(|(_, member)| member))
	}

	/// Each record of this replica's member in a space that its codes there may be named
	/// on: its member in the space itself, and its member in each group that the space
	/// follows, lists its key, and gives that member a role there now.
	pub(crate) fn own_code_members(&self, space: SpaceId) -> Result<Vec<EntityId>, NodeError> {
		let mut members: Vec<EntityId> = self.member_in(space)?.into_iter().collect();
		let open = self.space(space)?;
		let log = open.stored.log();
		let roles = log.group_member_roles(&open.keys)?;
		let own = self.replica.signing.verifying_key();
		for (_, member) in log.group_members_of(&own, &open.keys)? {
			let has_role = roles
				.get(&member)
				.is_some_and(|role| *role != crate::MemberRole::None);
			if has_role && !members.contains(&member) {
				members.push(member);
			}
		}
		Ok(members)
	}

	/// Revokes in one space, as [`Library::revoke_in_spaces_telling`] does, with the keys of
	/// `ended` too, which [`Library::ended_by_revocations`] gives. Returns whether it revoked
	/// one of those.
	fn revoke_in_space(
		&mut self,
		space: SpaceId,
		ended: &BTreeSet<[u8; 32]>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let personal = self.info.personal;
		let Some(member) = self.member_in(space)? else {
			return Ok(false);
		};
		// A replica whose member left changes nothing there.
		if self.has_left(space)? {
			return Ok(false);
		}
		let roster = self.roster(space)?;
		let mut revoking = Vec::new();
		let mut unknown = false;
		{
			let open = self.space(space)?;
			let log = open.stored.log();
			let personal_open = self.space(personal)?;
			let personal_log = personal_open.stored.log();
			for record in roster.replicas() {
				for key in roster.keys_of(record) {
					let added_by_revoked = ended.contains(key.as_bytes());
					if log.is_revoked(key, &open.keys)?
						|| !(added_by_revoked
							|| personal_log.is_revoked(key, &personal_open.keys)?)
						|| self.listing_owner(space, key)? != Some(member)
					{
						continue;
					}
					unknown |= added_by_revoked;
					revoking.push((record, *key));
				}
			}
		}
		if !revoking.is_empty() {
			self.rotate(space, &revoking, &roster, wall_millis, rng)?;
		}
		Ok(unknown)
	}

	/// The owner that a key's latest counting listing in a space with members names.
	fn listing_owner(
		&self,
		space: SpaceId,
		key: &nibkeep_crypto::VerifyingKey,
	) -> Result<Option<EntityId>, NodeError> {
		let open = self.space(space)?;
		let record = replica_record(space, key);
		Ok(open
			.stored
			.log()
			.governing_statements(&open.keys)?
			.into_iter()
			.filter(|governing| governing.statement.entity == record)
			.filter_map(|governing| Listing::from_text(governing.counting_text(&LISTING)?))
			// The latest, as a device that came back is listed again for its new member.
			.rfind(|listing| listing.key == *key && listing.verifies())
			.map(|listing| listing.owner))
	}

	/// What the counting governing changes of the personal space say of the user's spaces:
	/// the entries that give them, and the listings that ask to be listed in them, each as
	/// `(record, text)`. Read once for each settling, as reading them goes through every
	/// governing change.
	pub(super) fn read_personal(&self) -> Result<Personal, NodeError> {
		let open = self.space(self.info.personal)?;
		let mut personal = Personal::default();
		for governing in open.stored.log().governing_statements(&open.keys)? {
			let entity = governing.statement.entity;
			if let Some(text) = governing.counting_text(&SPACE_ENTRY) {
				personal.entries.push((entity, text.to_owned()));
			} else if let Some(text) = governing.counting_text(&LISTING) {
				personal.listings.push((entity, text.to_owned()));
			} else if let Some(text) = governing.counting_text(&fields::SPACE_CHOICE) {
				personal.choices.push((entity, text.to_owned()));
			} else if governing.counts
				&& governing.statement.field == fields::NEW_SPACES
				&& let nibkeep_model::Value::Boolean(takes) = governing.statement.value
			{
				personal.marks.push((entity, governing.author, takes));
			}
		}
		Ok(personal)
	}

	/// The spaces that the personal space gives this replica, and the member it is in each.
	pub(super) fn own_entries(&self, personal: &Personal) -> Vec<SpaceEntry> {
		let own = self.info.replica;
		personal
			.entries
			.iter()
			.filter(|(record, _)| *record == own)
			.filter_map(|(_, text)| SpaceEntry::from_text(text))
			.collect()
	}

	/// Takes the spaces that the personal space gives this replica and that it lacks, as
	/// [`Library::takes_entry`] says: where its newest choice of the space holds it, as when
	/// it asked, or it chose none and takes new spaces, never where it declined or dropped
	/// the space; opens each first key, keeps it if its hash is the one named, makes the
	/// space, empty until it syncs it, and states, in one change, that it holds those it
	/// took. An AI agent's replica takes every space given it, those chosen for it.
	fn take_space_entries(
		&mut self,
		personal: &Personal,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let takes_new = self.own_mark(personal);
		self.take_space_entries_as(personal, takes_new, wall_millis, rng)
	}

	/// Takes the spaces given to this replica that it lacks, as
	/// [`Library::take_space_entries`] does, where it chose none of them as `takes_new`
	/// says: as its mark says, or, after pairing, every one, as the person chose them there.
	pub(super) fn take_space_entries_as(
		&mut self,
		personal: &Personal,
		takes_new: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let ai_agent = self.is_ai_agent()?;
		let own = self.own_choices(personal);
		let takes_new = ai_agent || takes_new;
		let mut taken = Vec::new();
		for entry in self.own_entries(personal) {
			if self.spaces.contains_key(&entry.space)
				|| !(ai_agent || Self::takes_entry(own.get(&entry.space).copied(), takes_new))
			{
				continue;
			}
			let Some(key) = entry.open(&self.replica.decryption, self.info.replica) else {
				continue;
			};
			if key_hash(&key) == entry.hash {
				self.add_space(entry.space, &key, rng)?;
				taken.push(entry.space);
			}
		}
		self.expect_roots_of(personal)?;
		if ai_agent || taken.is_empty() {
			return Ok(());
		}
		self.state_holding(&taken, wall_millis, rng)
	}

	/// Has each space that this replica holds, but has not taken the first change of yet,
	/// take as its first change only one that an entry of the personal space names: so that
	/// another replica cannot have it take one of its own making.
	pub(crate) fn expect_space_roots(&mut self) -> Result<(), NodeError> {
		// Nothing to read where every space has its first change, as is usual.
		let mut waiting = false;
		for open in self.spaces.values() {
			waiting |= open.stored.log().root()?.is_none();
		}
		if !waiting {
			return Ok(());
		}
		let read = self.read_personal()?;
		self.expect_roots_of(&read)
	}

	/// Pins the first change of each space that lacks one to the root that the entry which
	/// gave this replica its first key names: the first entry, in the order of the personal
	/// space's statements, with that key's hash, as the entry taken was. Besides any that a
	/// group names, so that the order in which each source is read does not matter.
	fn expect_roots_of(&mut self, personal: &Personal) -> Result<(), NodeError> {
		let entries = self.own_entries(personal);
		for (&space, open) in &mut self.spaces {
			if open.stored.log().root()?.is_some() {
				continue;
			}
			let Some(hash) = open.first_key_hash() else {
				continue;
			};
			if let Some(entry) = entries
				.iter()
				.find(|entry| entry.space == space && entry.hash == hash)
			{
				open.stored
					.expect_roots_too([nibkeep_log::ChangeId::from_hash(entry.root)]);
			}
		}
		Ok(())
	}

	/// Asks to be listed in each space this replica holds but is not listed in, and has not
	/// asked to be listed in yet, with the listing its key signed, owned by the member that
	/// the space's entry names.
	fn ask_to_be_listed(
		&mut self,
		personal: &Personal,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.replica.signing.verifying_key();
		let asked: BTreeSet<SpaceId> = personal
			.listings
			.iter()
			.filter(|(record, _)| *record == self.info.replica)
			.filter_map(|(_, text)| Listing::from_text(text))
			.filter(|listing| listing.key == own)
			.map(|listing| listing.space)
			.collect();
		let mut operations = Vec::new();
		for entry in self.own_entries(personal) {
			if asked.contains(&entry.space)
				|| operations.iter().any(|(space, _)| *space == entry.space)
			{
				continue;
			}
			let Ok(open) = self.space(entry.space) else {
				continue;
			};
			if open.stored.log().is_listed(&own, &open.keys)?
				&& open.stored.log().is_governed(&open.keys)?
			{
				continue;
			}
			let listing = Listing::sign(
				&self.replica.signing,
				&self.replica.decryption.encryption_key(),
				entry.space,
				(entry.member, false),
			);
			operations.push((
				entry.space,
				ask_to_be_listed(self.info.replica, &listing.to_text(), rng),
			));
		}
		if operations.is_empty() {
			return Ok(());
		}
		let personal = self.info.personal;
		let operations = operations
			.into_iter()
			.map(|(_, operation)| operation)
			.collect();
		self.commit(personal, operations, wall_millis, rng)
	}

	/// Lists, in each space this replica is listed in, the replicas of its user that asked to
	/// be: those whose records in the personal space hold a listing of their own key for the
	/// space, owned by this replica's member there, that is not listed yet, and that are not
	/// revoked or AI agents', nor declined or dropped the space by their newest choice of it
	/// there.
	fn list_who_asked(
		&mut self,
		asks: &Personal,
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let own = self.replica.signing.verifying_key();
		let newest = Self::newest_choices(asks, roster);
		for space in self.spaces() {
			if space == personal {
				continue;
			}
			let Some(member) = self.member_in(space)? else {
				continue;
			};
			// Nobody lists a replica of a member who left.
			if self.has_left(space)? {
				continue;
			}
			let mut operations = Vec::new();
			let mut listed_now = BTreeSet::new();
			for (asker, text) in &asks.listings {
				let Some(listing) = Listing::from_text(text) else {
					continue;
				};
				// The cheap checks first: a listing is checked only if it may be listed.
				if listing.space != space
					|| listing.owner != member
					|| listing.ai_agent
					|| listing.key == own
					|| !roster.keys_of(*asker).contains(&listing.key)
					|| listed_now.contains(listing.key.as_bytes())
					|| newest
						.get(&(space, *listing.key.as_bytes()))
						.is_some_and(|(_, choice)| !choice.holds)
				{
					continue;
				}
				{
					let open = self.space(space)?;
					if open.stored.log().is_listed(&listing.key, &open.keys)? {
						continue;
					}
					let personal_open = self.space(personal)?;
					let log = personal_open.stored.log();
					if log.is_revoked(&listing.key, &personal_open.keys)?
						|| log.is_ai_agent(&listing.key, &personal_open.keys)?
						|| !listing.verifies()
					{
						continue;
					}
				}
				// The encryption key that the listing signed, or the listing would not count.
				let Some(encryption) = roster.encryption_of(*asker).cloned() else {
					continue;
				};
				if nibkeep_governance::encryption_hash(&encryption.to_bytes()) != listing.encryption
				{
					continue;
				}
				let (device, app, endpoint) = self.replica_names(*asker)?;
				let listed = list_replica(
					&SpaceReplica {
						record: listing.record,
						device_record: nibkeep_governance::device_record(space, &listing.key),
						member,
						signing: &listing.key,
						encryption: &encryption,
						listing: text,
						endpoint: endpoint.as_ref(),
						device: device.as_deref().unwrap_or_default(),
						app: app.as_deref().unwrap_or_default(),
						ai_agent: false,
					},
					rng,
				);
				listed_now.insert(*listing.key.as_bytes());
				operations.push(listed);
			}
			if operations.is_empty() {
				continue;
			}
			// The listings govern the space, and go in a change of their own first.
			let operations: Vec<Operation> = operations
				.into_iter()
				.flat_map(|listed| listed.governing.into_iter().chain(listed.operations))
				.collect();
			self.commit(space, operations, wall_millis, rng)?;
			self.give_keys(space, wall_millis, rng)?;
		}
		Ok(())
	}

	/// Gives each space this replica holds and is listed in, other than the personal one, to
	/// every recovery code that stands in the personal space and has no entry of it yet: its
	/// first key, sealed to that code, with the user's member there, in one change of the
	/// personal space. A code is given every space of the user's, as a device that it brings
	/// back takes those it holds; a device of the user's is given a space only with the
	/// person's consent ([`Library::give_space`]).
	pub(crate) fn give_space_entries(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.read_personal()?;
		let roster = self.roster(self.info.personal)?;
		self.give_space_entries_with(&personal, &roster, wall_millis, rng)
	}

	/// Gives the spaces as [`Library::give_space_entries`] does, from what the personal
	/// space says, read before.
	fn give_space_entries_with(
		&mut self,
		read: &Personal,
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let given = read.given();
		// The recovery codes that stand, each on its record, so that a device that a code
		// brings back finds the user's spaces.
		let remaining: Vec<(EntityId, nibkeep_crypto::EncryptionKey)> = roster
			.codes()
			.filter(|code| code.stands())
			.map(|code| (code.record, code.statement.encryption.clone()))
			.collect();
		if remaining.is_empty() {
			return Ok(());
		}
		let mut operations = Vec::new();
		for space in self.spaces() {
			// A space that the user left reaches none of their codes that lack it.
			if space == personal || self.member_in(space)?.is_none() || self.has_left(space)? {
				continue;
			}
			for (code, encryption) in &remaining {
				if !given.contains(&(*code, space)) {
					operations.extend(self.entry_for(space, (*code, encryption), rng)?);
				}
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(personal, operations, wall_millis, rng)
	}

	/// States this replica's choice of a space: whether it holds it, signed by its key, with a
	/// number one above the highest of its choices of that space that the personal space and
	/// the space state; on its own record in the personal space, where its user's other
	/// devices read it, and, where this replica holds the space, by its choices too, and is
	/// listed there, on its record there, where a choice not to hold it ends this replica,
	/// as a removal does
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	///
	/// # Errors
	///
	/// Returns [`NodeError::PersonalSpaceHeld`] for the personal space, and an error if this
	/// replica is an AI agent's, or a change cannot be saved.
	pub fn state_space_choice(
		&mut self,
		space: SpaceId,
		holds: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Choice, NodeError> {
		let personal = self.info.personal;
		if space == personal {
			return Err(NodeError::PersonalSpaceHeld);
		}
		let own = self.replica.signing.verifying_key();
		{
			let open = self.space(personal)?;
			if open.stored.log().is_ai_agent(&own, &open.keys)? {
				return Err(NodeError::AiAgentMayNot("choose the spaces it holds"));
			}
		}
		let number = self
			.highest_choice(space, &self.read_personal()?)?
			.map_or(1, |number| number + 1);
		let choice = Choice::sign(&self.replica.signing, space, number, holds);
		let operation = state_choice(self.info.replica, &choice, rng);
		self.commit(personal, vec![operation], wall_millis, rng)?;
		// Where it dropped the space, it changes nothing there: a device of its user carries
		// a choice to hold it there, once the person approves.
		if self.spaces.contains_key(&space) && self.changes_there(space)? {
			let operation = state_choice(choice.record, &choice, rng);
			self.commit(space, vec![operation], wall_millis, rng)?;
		}
		Ok(choice)
	}

	/// The number of the highest of this replica's choices of a space that the personal space
	/// states, `personal`, or that the space states, where this replica holds it.
	pub(super) fn highest_choice(
		&self,
		space: SpaceId,
		personal: &Personal,
	) -> Result<Option<u64>, NodeError> {
		let own = self.replica.signing.verifying_key();
		let mut highest = personal
			.choices
			.iter()
			.filter(|(record, _)| *record == self.info.replica)
			.filter_map(|(_, text)| Choice::from_text(text))
			.filter(|choice| choice.space == space && choice.key == own && choice.verifies())
			.map(|choice| choice.number)
			.max();
		if self.spaces.contains_key(&space) {
			let open = self.space(space)?;
			if let Some((number, _)) = open.stored.log().standing_choice(&own, &open.keys)? {
				highest = highest.max(Some(number));
			}
		}
		Ok(highest)
	}

	/// Of the choices that the personal space, `personal`, states for the replicas of its user
	/// that `roster` lists, each on that replica's own record, signed by its key of its own,
	/// and that verify, the newest of each replica's for each space: by space and key, the
	/// replica's record and the choice with the highest number, a drop winning a tie.
	pub(super) fn newest_choices(
		personal: &Personal,
		roster: &Roster,
	) -> BTreeMap<(SpaceId, [u8; 32]), (EntityId, Choice)> {
		let mut newest: BTreeMap<(SpaceId, [u8; 32]), (EntityId, Choice)> = BTreeMap::new();
		for (record, text) in &personal.choices {
			let Some(choice) = Choice::from_text(text) else {
				continue;
			};
			if !roster.keys_of(*record).contains(&choice.key) || !choice.verifies() {
				continue;
			}
			let rank = |choice: &Choice| (choice.number, !choice.holds);
			let slot = (choice.space, *choice.key.as_bytes());
			if newest
				.get(&slot)
				.is_none_or(|(_, kept)| rank(&choice) > rank(kept))
			{
				newest.insert(slot, (*record, choice));
			}
		}
		newest
	}

	/// Whether this replica may change a space with members by its role and choices: it is
	/// listed there, not revoked, holds the space by its choices, and its member did not
	/// leave.
	pub(super) fn changes_there(&self, space: SpaceId) -> Result<bool, NodeError> {
		if self.member_in(space)?.is_none() || self.has_left(space)? {
			return Ok(false);
		}
		let open = self.space(space)?;
		let own = self.replica.signing.verifying_key();
		Ok(open.stored.log().holds_space(&own, &open.keys)?)
	}

	/// Whether this replica may carry its user's devices' choices into a space: it changes
	/// it as [`Library::changes_there`] says, or it is a member there through a group alone,
	/// with a role, holds the space by its choices, and is not revoked; so that a device's
	/// decline of a group's space reaches the space through a device of its user that holds
	/// it.
	fn carries_there(&self, space: SpaceId) -> Result<bool, NodeError> {
		if self.changes_there(space)? {
			return Ok(true);
		}
		if self.member_in(space)?.is_some()
			|| self.has_left(space)?
			|| self.own_role(space)? == crate::MemberRole::None
			|| self.check_not_revoked(space).is_err()
		{
			return Ok(false);
		}
		let open = self.space(space)?;
		let own = self.replica.signing.verifying_key();
		Ok(open.stored.log().holds_space(&own, &open.keys)?)
	}

	/// Carries into each space that this replica holds and may change each newest choice of
	/// a replica of its user that the personal space, `personal`, states, where it is a drop
	/// that outranks the choice of that replica's that stands there, while that replica is
	/// listed there: so a device's drop reaches the space through any of its user's devices
	/// that holds it, also one stated while that device held nothing of the space, and never
	/// one that a newer choice to hold replaced. A space where this fails is left until the
	/// next sync, and the first error is told at the end.
	fn carry_choices(
		&mut self,
		personal: &Personal,
		roster: &Roster,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal_space = self.info.personal;
		// The newest drops of the replicas that the personal space neither revokes nor
		// counts as AI agents', by space.
		let mut drops: BTreeMap<SpaceId, Vec<Choice>> = BTreeMap::new();
		{
			let open = self.space(personal_space)?;
			let log = open.stored.log();
			for ((space, _), (_, choice)) in Self::newest_choices(personal, roster) {
				if choice.holds
					|| log.is_revoked(&choice.key, &open.keys)?
					|| log.is_ai_agent(&choice.key, &open.keys)?
				{
					continue;
				}
				drops.entry(space).or_default().push(choice);
			}
		}
		let mut first = Ok(());
		for (space, choices) in drops {
			let carried = self.carry_into(space, &choices, wall_millis, rng);
			if first.is_ok() {
				first = carried;
			}
		}
		first
	}

	/// Carries `choices`, newest drops of replicas of the user, into `space`, as
	/// [`Library::carry_choices`] says.
	fn carry_into(
		&mut self,
		space: SpaceId,
		choices: &[Choice],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if space == self.info.personal
			|| !self.spaces.contains_key(&space)
			|| !self.carries_there(space)?
		{
			return Ok(());
		}
		let mut operations = Vec::new();
		{
			let open = self.space(space)?;
			let log = open.stored.log();
			for choice in choices {
				if !log.is_listed(&choice.key, &open.keys)?
					|| log.is_revoked(&choice.key, &open.keys)?
				{
					continue;
				}
				let stood = log.standing_choice(&choice.key, &open.keys)?;
				if stood.is_some_and(|(number, _)| number >= choice.number) {
					continue;
				}
				operations.push(state_choice(choice.record, choice, rng));
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Lists again, in `space`, the replica of this replica's user whose record in the
	/// personal space is `replica`, after it dropped the space, once it asked for the space
	/// again: carries there its newest choice of the space that the personal space states,
	/// where that is one to hold it that outranks the one that stands there, which brings it
	/// back, and gives it the space's keys. Step 2 calls it once the person approves on this
	/// device.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoNewerChoice`] if that replica's newest choice of the space is
	/// none, a drop, or one that does not outrank the one that stands there;
	/// [`NodeError::Removed`] if this replica may not change the space; and an error if a
	/// change cannot be saved.
	pub fn list_again(
		&mut self,
		space: SpaceId,
		replica: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if space == self.info.personal || !self.changes_there(space)? {
			return Err(NodeError::Removed);
		}
		let personal = self.read_personal()?;
		let roster = self.roster(self.info.personal)?;
		let newest = Self::newest_choices(&personal, &roster)
			.into_iter()
			.filter(|((chosen, _), (record, _))| *chosen == space && *record == replica)
			.map(|(_, (_, choice))| choice)
			.max_by_key(|choice| (choice.number, !choice.holds))
			.filter(|choice| choice.holds)
			.ok_or(NodeError::NoNewerChoice)?;
		{
			let open = self.space(space)?;
			let stood = open.stored.log().standing_choice(&newest.key, &open.keys)?;
			if stood.is_none_or(|(number, holds)| holds || number >= newest.number) {
				return Err(NodeError::NoNewerChoice);
			}
		}
		let operation = state_choice(newest.record, &newest, rng);
		self.commit(space, vec![operation], wall_millis, rng)?;
		self.give_keys(space, wall_millis, rng)
	}

	/// This replica's member in a space with members, as its counting listing there names
	/// it, if it is listed.
	pub(crate) fn member_in(&self, space: SpaceId) -> Result<Option<EntityId>, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let own = self.replica.signing.verifying_key();
		if !log.has_members(&open.keys)?
			|| !log.is_listed(&own, &open.keys)?
			|| log.is_revoked(&own, &open.keys)?
		{
			return Ok(None);
		}
		self.listing_owner(space, &own)
	}

	/// The names of the device and the app of a replica of the user, and its endpoint, as
	/// the personal space has them.
	pub(super) fn replica_names(&self, replica: EntityId) -> Result<ReplicaNames, NodeError> {
		let open = self.space(self.info.personal)?;
		let state = open.stored.log().state();
		let record = nibkeep_governance::replica(state, replica)?;
		let device = match record.as_ref().and_then(|record| record.device) {
			Some(device) => display_name(state, &open.types, &self.languages, device)?,
			None => None,
		};
		let app = display_name(state, &open.types, &self.languages, replica)?;
		Ok((device, app, record.and_then(|record| record.endpoint)))
	}

	/// The names of this replica's device and app.
	pub(crate) fn own_names(&self) -> Result<(String, String), NodeError> {
		let (device, app, _) = self.replica_names(self.info.replica)?;
		Ok((device.unwrap_or_default(), app.unwrap_or_default()))
	}

	/// An entity's name in the personal space.
	pub(crate) fn own_name(&self, entity: EntityId) -> Result<Option<String>, NodeError> {
		let open = self.space(self.info.personal)?;
		display_name(
			open.stored.log().state(),
			&open.types,
			&self.languages,
			entity,
		)
	}

	/// Adds a space this replica now holds, with its first key: opens the space, empty
	/// until it syncs it or is founded, and then saves the key with the others, under the key
	/// file's lock. The log comes first, as a key without its log would keep the library
	/// from opening; a log without its key is passed over, and opened again when the key
	/// comes.
	pub(crate) fn add_space(
		&mut self,
		space: SpaceId,
		first: &Key,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let log = log_path(&self.directory, space);
		if let Some(folder) = log.parent() {
			fs::create_dir_all(folder).map_err(NodeError::file(folder))?;
		}
		let epochs = std::collections::BTreeMap::from([(0, first.clone())]);
		let opened = Space::open(
			&log,
			space,
			&epochs,
			&self.replica.storage,
			&self.code_types,
		)?;
		let compartment = self.compartment.clone();
		let mut saved = None;
		self.update_key_file(|key_file| {
			let mut replica = key_file.replica(&compartment)?;
			replica
				.spaces
				.entry(space)
				.or_default()
				.entry(0)
				.or_insert_with(|| first.clone());
			key_file.set_replica(&replica, &compartment, rng)?;
			saved = Some(replica.spaces);
			Ok(())
		})?;
		if let Some(spaces) = saved {
			self.replica.spaces = spaces;
		}
		self.spaces.insert(space, opened);
		self.keep_in(space);
		self.choose_writing_keys()?;
		Ok(())
	}

	/// Takes away a space this replica added and could not found: its key, then its folder.
	pub(crate) fn remove_space(&mut self, space: SpaceId, rng: &mut impl CryptoRng) {
		self.spaces.remove(&space);
		let compartment = self.compartment.clone();
		let mut saved = None;
		let removed = self.update_key_file(|key_file| {
			let mut replica = key_file.replica(&compartment)?;
			replica.spaces.remove(&space);
			key_file.set_replica(&replica, &compartment, rng)?;
			saved = Some(replica.spaces);
			Ok(())
		});
		if let Some(spaces) = saved {
			self.replica.spaces = spaces;
		}
		if removed.is_ok()
			&& let Some(folder) = log_path(&self.directory, space).parent()
		{
			let _ = fs::remove_dir_all(folder);
		}
	}

	/// Opens the spaces that another program of this replica added since this one opened
	/// the library, as their folders show, from the key file.
	pub(crate) fn catch_up_spaces(&mut self) -> Result<bool, NodeError> {
		let folder = self.directory.join("spaces");
		let Ok(entries) = fs::read_dir(&folder) else {
			return Ok(false);
		};
		let new: Vec<SpaceId> = entries
			.filter_map(Result::ok)
			.filter_map(|entry| entry.file_name().to_str()?.parse::<SpaceId>().ok())
			.filter(|space| !self.spaces.contains_key(space))
			.collect();
		if new.is_empty() {
			return Ok(false);
		}
		let replica = nibkeep_keys::KeyFile::read(&self.directory.join(super::KEYS))?
			.replica(&self.compartment)?;
		let mut opened = false;
		for space in new {
			let (Some(epochs), log) =
				(replica.spaces.get(&space), log_path(&self.directory, space))
			else {
				continue;
			};
			if !log.exists() {
				continue;
			}
			let open = Space::open(&log, space, epochs, &replica.storage, &self.code_types)?;
			self.spaces.insert(space, open);
			self.keep_in(space);
			opened = true;
		}
		if opened {
			self.replica.spaces = replica.spaces;
			self.choose_writing_keys()?;
		}
		Ok(opened)
	}
}

/// The keys that added each key, by the bytes of each.
type Listers = BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>;

/// The keys that a revocation ends of those that the personal space does not list, as
/// [`Library::revoke_in_spaces_telling`] says: each that a key of `revoked`, or one of these,
/// listed, as `listers` gives the keys that listed each, but those that a key of `standing`
/// listed, or one so kept in turn. A key listed by itself alone is in neither.
fn ended_by_revoked(
	listers: &Listers,
	standing: &BTreeSet<[u8; 32]>,
	revoked: &BTreeSet<[u8; 32]>,
) -> BTreeSet<[u8; 32]> {
	// Each grows from its seeds by the keys they listed, until it takes no more.
	let grown = |seeds: &BTreeSet<[u8; 32]>, but: &BTreeSet<[u8; 32]>| {
		let mut grown: BTreeSet<[u8; 32]> = BTreeSet::new();
		loop {
			let before = grown.len();
			for (key, by) in listers {
				if !seeds.contains(key)
					&& !but.contains(key)
					&& by.iter().any(|lister| {
						lister != key && (seeds.contains(lister) || grown.contains(lister))
					}) {
					grown.insert(*key);
				}
			}
			if grown.len() == before {
				return grown;
			}
		}
	};
	let mut known: BTreeSet<[u8; 32]> = standing.union(revoked).copied().collect();
	let kept = grown(standing, &known);
	known.extend(kept);
	grown(revoked, &known)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn keys(names: &[u8]) -> BTreeSet<[u8; 32]> {
		names.iter().map(|&name| [name; 32]).collect()
	}

	fn listed(pairs: &[(u8, &[u8])]) -> BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> {
		pairs
			.iter()
			.map(|&(key, by)| ([key; 32], keys(by)))
			.collect()
	}

	#[test]
	fn a_revocation_ends_what_revoked_devices_added_but_what_devices_at_hand_added_too() {
		// 1 is this device, 2 a laptop at hand, 3 a lost desk, revoked, that the personal
		// space lists; the others it does not list.
		let (standing, revoked) = (keys(&[1, 2]), keys(&[3]));
		let listers = listed(&[
			// Listed by the lost desk, and in turn by what it listed.
			(4, &[3]),
			(5, &[4]),
			// Listed by the kept laptop, and by the lost desk too, and what it listed.
			(6, &[2, 3]),
			(7, &[6]),
			// Listed by itself alone, as on a record made for it, and by itself and the desk.
			(8, &[8]),
			(9, &[9, 3]),
			// The keys that the personal space lists stay as they are.
			(2, &[1]),
			(3, &[2]),
		]);
		assert_eq!(
			ended_by_revoked(&listers, &standing, &revoked),
			keys(&[4, 5, 9])
		);
	}
}
