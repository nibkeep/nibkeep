//! What reads who holds a space
//! ([technical design](../../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
//!
//! A device of the user's holds a space where its newest choice of it, in the personal space,
//! holds it, as every device states as it takes a space. So a revocation and a recovery name
//! the spaces that they cannot reach, and the devices that do; a backup's update names the
//! spaces that it did not write; a move takes along only the origins and checkouts of the
//! devices that hold the new space; and a pin names a file only for a device that holds its
//! space.

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_governance::{Choice, Roster};

use super::Library;
use super::taking::{SpaceHolder, by_name};
use crate::NodeError;

/// A space of the user's that this device does not hold, with the devices of theirs that do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldElsewhere {
	/// The space.
	pub space: SpaceId,
	/// Its name, as the devices that hold it keep it in the personal space.
	pub name: Option<String>,
	/// The other devices of the user's that hold it, as their choices say, but those revoked
	/// and those that the question leaves out: none where no other device holds it.
	pub holders: Vec<SpaceHolder>,
}

/// The newest choice of each replica of the user's for each space, by space and key, as
/// [`Library::newest_choices`] reads them.
type Newest = BTreeMap<(SpaceId, [u8; 32]), (EntityId, Choice)>;

impl Library {
	/// The spaces of the user's that a revocation of `replicas`, by their records in the
	/// personal space, cannot reach from this device: those that one of them holds, as its
	/// choices say, and this device does not. Each comes with the other devices of the user's
	/// that hold it, but `replicas`, which revoke them there after a sync that brings them the
	/// revocation; where none does, a replica revoked there stays in the user's place, as no
	/// device of theirs can revoke it there. By name, then by ID; the same after the
	/// revocation as before.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn out_of_reach(&self, replicas: &[EntityId]) -> Result<Vec<HeldElsewhere>, NodeError> {
		let (_, newest) = self.newest()?;
		let mut spaces = BTreeSet::new();
		for (record, choice) in newest.values() {
			if choice.holds
				&& replicas.contains(record)
				&& choice.space != self.info.personal
				&& !self.holds_here(choice.space)?
			{
				spaces.insert(choice.space);
			}
		}
		self.held_elsewhere_with(&spaces, replicas, &newest)
	}

	/// Of `spaces`, those that this device does not have, each with the devices of the user's
	/// that hold it, as [`HeldElsewhere`] says. By name, then by ID.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub(crate) fn spaces_held_elsewhere(
		&self,
		spaces: &[SpaceId],
	) -> Result<Vec<HeldElsewhere>, NodeError> {
		let elsewhere: BTreeSet<SpaceId> = spaces
			.iter()
			.copied()
			.filter(|space| *space != self.info.personal && !self.spaces.contains_key(space))
			.collect();
		if elsewhere.is_empty() {
			return Ok(Vec::new());
		}
		let (_, newest) = self.newest()?;
		self.held_elsewhere_with(&elsewhere, &[], &newest)
	}

	/// The replicas of the user's, and the newest choices that they state in the personal
	/// space, as [`Library::newest_choices`] reads them.
	fn newest(&self) -> Result<(Roster, Newest), NodeError> {
		let read = self.read_personal()?;
		let roster = self.roster(self.info.personal)?;
		let newest = Self::newest_choices(&read, &roster);
		Ok((roster, newest))
	}

	/// Each of `spaces` with its name and the devices that hold it by `newest`, but those of
	/// `left_out`. By name, then by ID.
	fn held_elsewhere_with(
		&self,
		spaces: &BTreeSet<SpaceId>,
		left_out: &[EntityId],
		newest: &Newest,
	) -> Result<Vec<HeldElsewhere>, NodeError> {
		let names = self.kept_space_names()?;
		let mut found = Vec::new();
		for &space in spaces {
			let mut holders = self.space_holders((space, false), newest)?;
			holders.retain(|holder| !left_out.contains(&holder.replica));
			let name = if self.spaces.contains_key(&space) {
				self.space_name(space)?
			} else {
				names.get(&space).cloned().flatten()
			};
			found.push(HeldElsewhere {
				space,
				name,
				holders,
			});
		}
		found.sort_by_key(|each| by_name(each.name.as_deref(), each.space));
		Ok(found)
	}

	/// Whether this replica holds `space`: it has it, and its choices do not drop it.
	pub(crate) fn holds_here(&self, space: SpaceId) -> Result<bool, NodeError> {
		let Some(open) = self.spaces.get(&space) else {
			return Ok(false);
		};
		let own = self.replica.signing.verifying_key();
		Ok(open.stored.log().holds_space(&own, &open.keys)?)
	}

	/// The devices of the user's that hold `space`, by their records in the personal space,
	/// as they show there: this one where it holds it, and each on which a replica that is not
	/// revoked holds it, as its choices say.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub(crate) fn holding_devices(&self, space: SpaceId) -> Result<BTreeSet<EntityId>, NodeError> {
		let personal = self.info.personal;
		let (roster, newest) = self.newest()?;
		let held = space == personal || self.holds_here(space)?;
		let records: BTreeSet<EntityId> = self
			.space_holders((space, held), &newest)?
			.into_iter()
			.map(|holder| holder.replica)
			.collect();
		let open = self.space(personal)?;
		let log = open.stored.log();
		let state = log.state();
		let mut devices = BTreeSet::new();
		for record in roster.replicas() {
			if !(space == personal || records.contains(&record)) {
				continue;
			}
			// One whose keys are all revoked holds nothing, as a record without keys.
			let mut revoked = true;
			for key in roster.keys_of(record) {
				revoked &= log.is_revoked(key, &open.keys)?;
			}
			if revoked {
				continue;
			}
			if let Some(device) =
				nibkeep_governance::replica(state, record)?.and_then(|on| on.device)
			{
				devices.insert(state.canonical(device)?);
			}
		}
		Ok(devices)
	}
}
