//! Copies that wait before purging: an archive node keeps apart what a purge destroys for a
//! while, 30 days by default, during which its owner can stop the purge, which gives back what
//! it destroyed, or end the wait early ([technical design: copies that wait]).
//!
//! [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use nibkeep_base::{ChangeId, EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_log::{DeviceRole, Kept};

use super::Library;
use super::accounts::DeviceMarks;
use crate::NodeError;

/// A day, in milliseconds.
pub const DAY: u64 = 86_400_000;

/// How many days an archive node, or a backup, waits unless its owner says otherwise: 30.
pub const DEFAULT_WAIT_DAYS: u64 = 30;

/// The longest wait an owner may set, in days: 365.
pub const MOST_WAIT_DAYS: u64 = 365;

/// How many days a device waits before purging, as marked: an archive node its own number of
/// days, or 30 without one; an everyday device none.
#[must_use]
pub fn wait_days(marks: &DeviceMarks) -> u64 {
	match marks.role {
		DeviceRole::Archive => marks.waits.unwrap_or(DEFAULT_WAIT_DAYS).min(MOST_WAIT_DAYS),
		DeviceRole::Everyday => 0,
	}
}

/// How many days a backup waits before purging, as marked: its own number of days, or 30
/// without one.
#[must_use]
pub fn backup_wait_days(backup: &super::backups::Backup) -> u64 {
	backup
		.waits
		.unwrap_or(DEFAULT_WAIT_DAYS)
		.min(MOST_WAIT_DAYS)
}

/// The wait in force at a time, by a device's history of waits, in milliseconds: that of the
/// latest entry from that time or before, or none before the first.
pub(crate) fn wait_at(waits: &[(u64, u64)], at: u64) -> u64 {
	waits
		.iter()
		.rev()
		.find(|(since, _)| *since <= at)
		.map_or(0, |(_, wait)| *wait)
}

/// A device's history of waits brought up to date at `now` with the wait its marks ask for:
/// a longer wait holds at once, and a shorter one, or none, only once the wait before it has
/// passed, so that a device that turned against its user cannot shorten the wait and purge at
/// once. Returns `None` if nothing changes.
pub(crate) fn waits_marked(waits: &[(u64, u64)], marked: u64, now: u64) -> Option<Vec<(u64, u64)>> {
	match waits.last() {
		Some((_, last)) if *last == marked => return None,
		None if marked == 0 => return None,
		_ => {}
	}
	// What was to come no longer does: the marks say otherwise now.
	let mut waits: Vec<(u64, u64)> = waits
		.iter()
		.copied()
		.filter(|(since, _)| *since <= now)
		.collect();
	let before = wait_at(&waits, now);
	if marked >= before {
		waits.push((now, marked));
	} else {
		waits.push((now.saturating_add(before), marked));
	}
	Some(waits)
}

/// A purge whose destruction this replica keeps apart while it waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingPurge {
	/// The space.
	pub space: SpaceId,
	/// The purge.
	pub purge: ChangeId,
	/// When this replica took it in, in milliseconds since 1970.
	pub received: u64,
	/// When the wait ends, after which what it destroyed goes for good.
	pub ends: u64,
	/// How many changes it purged whose payloads and keys are kept.
	pub changes: usize,
	/// How many secret values it purged whose keys are kept.
	pub secrets: usize,
	/// How many sealed blobs it purged whose keys are kept.
	pub blobs: usize,
	/// How many bytes of sealed blobs are kept apart for it.
	pub bytes: u64,
	/// The backup it waits on, if it waits on a backup plugged in here rather than on this
	/// device.
	pub on: Option<WaitingOn>,
}

/// A backup that a purge waits on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingOn {
	/// The backup's record.
	pub backup: EntityId,
	/// Its name.
	pub name: String,
	/// Its folder, here.
	pub folder: PathBuf,
	/// How many items and files of what the purge destroyed it keeps.
	pub items: u64,
}

/// A copy that waits before destroying what a purge destroys, as a purge names them before
/// the person confirms: the person's archive nodes and backups, and other members'.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingCopy {
	/// Its name: the device's, or the backup's, if this replica knows it.
	pub name: Option<String>,
	/// Whether it is this device.
	pub this: bool,
	/// Whether it is a backup, whose wait counts from its next update.
	pub backup: bool,
	/// For another member's copy, that member's name, if known.
	pub member: Option<String>,
	/// Whether it is another member's.
	pub others: bool,
	/// How many days it waits.
	pub days: u64,
}

impl Library {
	/// How many days this device waits before purging, as marked now.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn own_wait_days(&self) -> Result<u64, NodeError> {
		Ok(wait_days(&self.device_marks(self.info.device)?))
	}

	/// The wait in force on this device at a time, in milliseconds, as its history of waits
	/// says.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub(crate) fn wait_in_force(&self, at: u64) -> Result<u64, NodeError> {
		let waits = self.space(self.info.personal)?.stored.waits()?;
		Ok(wait_at(&waits, at))
	}

	/// Brings this device's history of waits up to date with its marks at `now`, keeps what
	/// purges destroy in every space while a wait is in force, and returns the wait in force
	/// now, in milliseconds.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read or written.
	pub(crate) fn refresh_wait(&mut self, now: u64) -> Result<u64, NodeError> {
		let marked = self.own_wait_days()?.saturating_mul(DAY);
		let personal = self.info.personal;
		let open = self
			.spaces
			.get_mut(&personal)
			.ok_or(NodeError::NoSpace(personal))?;
		let waits = open.stored.waits()?;
		let waits = match waits_marked(&waits, marked, now) {
			Some(changed) => {
				open.stored.save_waits(changed.clone(), &open.keys)?;
				changed
			}
			None => waits,
		};
		let wait = wait_at(&waits, now);
		// A wait to come, after a shorter one, keeps too, as what it keeps may stay that long.
		let keeping = wait > 0;
		for open in self.spaces.values_mut() {
			open.stored.keep_destroyed(keeping);
		}
		Ok(wait)
	}

	/// Keeps what purges destroy in a space just opened, as the other spaces do, which the
	/// wait in force set ([`Library::refresh_wait`]).
	pub(crate) fn keep_in(&mut self, space: SpaceId) {
		let keeping = self
			.spaces
			.get(&self.info.personal)
			.is_some_and(|open| open.stored.keeps_destroyed());
		if let Some(open) = self.spaces.get_mut(&space) {
			open.stored.keep_destroyed(keeping);
		}
	}

	/// The purges whose destruction this replica keeps apart, by space and then by when it
	/// took them in, with when each wait ends.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn waiting_purges(&self) -> Result<Vec<WaitingPurge>, NodeError> {
		let waits = self.space(self.info.personal)?.stored.waits()?;
		let mut waiting = Vec::new();
		for (&space, open) in &self.spaces {
			let mut by_purge: BTreeMap<ChangeId, WaitingPurge> = BTreeMap::new();
			for row in open.stored.kept()? {
				let entry = by_purge.entry(row.purge).or_insert_with(|| WaitingPurge {
					space,
					purge: row.purge,
					received: row.received,
					ends: row.received.saturating_add(wait_at(&waits, row.received)),
					changes: 0,
					secrets: 0,
					blobs: 0,
					bytes: 0,
					on: None,
				});
				match &row.part {
					Kept::Change(_) => entry.changes += 1,
					Kept::RecordKeys(..) => entry.secrets += 1,
					Kept::BlobKeys(sealed, _) => {
						entry.blobs += 1;
						entry.bytes += open.stored.kept_blob_size(sealed).unwrap_or(0);
					}
				}
			}
			let mut purges: Vec<WaitingPurge> = by_purge.into_values().collect();
			purges.sort_by_key(|purge| (purge.received, purge.purge));
			waiting.extend(purges);
		}
		Ok(waiting)
	}

	/// The purges that wait on the backups `found` here before what they destroyed leaves
	/// them, as each backup's record says, by backup and then by when each came.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn waiting_purges_on(
		&self,
		found: &[(super::backups::Backup, PathBuf)],
	) -> Result<Vec<WaitingPurge>, NodeError> {
		let mut waiting = Vec::new();
		for (backup, folder) in found {
			let record = crate::backup::read_record(&crate::backup::InFileSystem::new(folder));
			let mut purges = Vec::new();
			for (&purge, &(seen, items)) in &record.purges {
				let mut space = None;
				for (&id, open) in &self.spaces {
					if open.stored.log().contains(purge)? {
						space = Some(id);
						break;
					}
				}
				let Some(space) = space else {
					continue;
				};
				purges.push(WaitingPurge {
					space,
					purge,
					received: seen,
					ends: seen.saturating_add(wait_at(&record.waits, seen)),
					changes: 0,
					secrets: 0,
					blobs: 0,
					bytes: 0,
					on: Some(WaitingOn {
						backup: backup.id,
						name: backup.name.clone(),
						folder: folder.clone(),
						items,
					}),
				});
			}
			purges.sort_by_key(|purge| (purge.received, purge.purge));
			waiting.extend(purges);
		}
		Ok(waiting)
	}

	/// The places where the purge that `name` names waits, on this device and on the backups
	/// `found` here: `name` is the start of its ID, as `nib purges` shows it; `on`, if given,
	/// names one backup, by its name or ID, or `here` this device.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichPurge`] if it names no purge that waits there, or several.
	pub fn find_waiting_purges(
		&self,
		name: &str,
		(found, on): (&[(super::backups::Backup, PathBuf)], Option<&str>),
	) -> Result<Vec<WaitingPurge>, NodeError> {
		let name = name.trim();
		let mut all = self.waiting_purges()?;
		all.extend(self.waiting_purges_on(found)?);
		let named: Vec<WaitingPurge> = all
			.into_iter()
			.filter(|waiting| !name.is_empty() && waiting.purge.to_string().starts_with(name))
			.filter(|waiting| match (on, &waiting.on) {
				(None, _) => true,
				(Some(on), None) => on.eq_ignore_ascii_case("here"),
				(Some(on), Some(backup)) => {
					backup.name.eq_ignore_ascii_case(on) || backup.backup.to_string() == on
				}
			})
			.collect();
		let purges: BTreeSet<ChangeId> = named.iter().map(|waiting| waiting.purge).collect();
		match purges.len() {
			1 => Ok(named),
			0 => Err(NodeError::WhichPurge(format!(
				"no purge that waits on this device, or on a backup plugged in here, starts with {name}"
			))),
			_ => Err(NodeError::WhichPurge(format!(
				"several purges that wait here start with {name}; give more of the ID"
			))),
		}
	}

	/// Stops a purge where it waits, as `waiting` lists the places of one purge: makes one
	/// change that restores what it destroyed and they keep, which every replica takes, and
	/// gives back what this replica kept, and from each backup, through an update of it, what
	/// the backup kept, so that what was purged counts again as it was. Returns the backups
	/// updated: one that another program holds is not, and gives back at its next update.
	///
	/// # Errors
	///
	/// Returns [`NodeError::CannotRestore`] if this member's role does not allow it: an admin
	/// may stop any purge, an editor one of the changes their member's replicas signed, and
	/// nothing is made; and another error if a space or a backup cannot be read or written.
	pub fn stop_purge(
		&mut self,
		waiting: &[WaitingPurge],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<super::backups::Backup>, NodeError> {
		let Some(first) = waiting.first() else {
			return Ok(Vec::new());
		};
		let (space, purge) = (first.space, first.purge);
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("stop a purge")?;
		// What each backup keeps of it comes back from the backup, through an update.
		let mut also = nibkeep_log::Restores::default();
		let mut from = Vec::new();
		for on in waiting.iter().filter_map(|waiting| waiting.on.as_ref()) {
			let backup = self.own_backup(on)?;
			let kept = self
				.backup_kept_of(&backup, &on.folder, (space, purge))?
				.ok_or_else(|| {
					NodeError::WhichPurge(format!(
						"another program updates the backup {} now; try again",
						on.name
					))
				})?;
			also.changes.extend(kept.changes);
			also.statements.extend(kept.statements);
			also.blobs.extend(kept.blobs);
			from.push((backup, on.folder.clone()));
		}
		let signing = &self.replica.signing;
		let code_types = &self.code_types;
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let restore =
			match open
				.stored
				.restore_kept(signing, &open.keys, (purge, &also), wall_millis, rng)
			{
				Err(nibkeep_store::StoreError::Log(nibkeep_log::LogError::CannotRestore)) => {
					return Err(NodeError::CannotRestore);
				}
				restore => restore?,
			};
		open.read_types(code_types)?;
		if restore.is_none() {
			return Err(NodeError::WhichPurge(format!(
				"nothing of the purge {purge} waits here"
			)));
		}
		if space == self.info.personal {
			self.refresh_languages()?;
		}
		let mut updated = Vec::new();
		for (backup, folder) in from {
			let now = || wall_millis;
			if self
				.update_backup(&backup, &folder, &now, &|| false)?
				.is_some()
			{
				updated.push(backup);
			}
		}
		Ok(updated)
	}

	/// The person's backup that a purge waits on.
	fn own_backup(&self, on: &WaitingOn) -> Result<super::backups::Backup, NodeError> {
		self.backups()?
			.into_iter()
			.find(|backup| backup.id == on.backup)
			.ok_or_else(|| NodeError::WhichBackup(format!("{} is no backup of yours", on.name)))
	}

	/// Ends the wait of a purge where it waits, as `waiting` lists the places of one purge:
	/// destroys what this replica kept of what it destroyed, and has each backup let it go
	/// at once, through an update of it. Returns how many parts went, and the backups updated.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichPurge`] if another program updates a backup now, and another
	/// error if a space or a backup cannot be written.
	pub fn end_wait(
		&mut self,
		waiting: &[WaitingPurge],
		wall_millis: u64,
	) -> Result<(usize, Vec<super::backups::Backup>), NodeError> {
		let mut gone = 0;
		let mut updated = Vec::new();
		for place in waiting {
			if let Some(on) = &place.on {
				let backup = self.own_backup(on)?;
				let now = || wall_millis;
				let ending = BTreeSet::from([place.purge]);
				let done = self
					.update_backup_ending(&backup, &on.folder, (&now, &|| false), &ending)?
					.ok_or_else(|| {
						NodeError::WhichPurge(format!(
							"another program updates the backup {} now; try again",
							on.name
						))
					})?;
				gone += done.purged;
				updated.push(backup);
			} else {
				let open = self
					.spaces
					.get_mut(&place.space)
					.ok_or(NodeError::NoSpace(place.space))?;
				gone += open
					.stored
					.destroy_kept(&BTreeSet::from([place.purge]), &open.keys)?;
			}
		}
		Ok((gone, updated))
	}

	/// Destroys what this replica keeps apart of the purges whose wait has ended at `now`, as
	/// the holder of the sync role does in its passes, after bringing the wait up to date
	/// with the marks. Returns how many parts went.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read or written.
	pub fn destroy_due(&mut self, now: u64) -> Result<usize, NodeError> {
		self.refresh_wait(now)?;
		let mut destroyed = 0;
		let due: Vec<(SpaceId, ChangeId)> = self
			.waiting_purges()?
			.into_iter()
			.filter(|waiting| waiting.ends <= now)
			.map(|waiting| (waiting.space, waiting.purge))
			.collect();
		let mut by_space: BTreeMap<SpaceId, BTreeSet<ChangeId>> = BTreeMap::new();
		for (space, purge) in due {
			by_space.entry(space).or_default().insert(purge);
		}
		for (space, purges) in by_space {
			let open = self
				.spaces
				.get_mut(&space)
				.ok_or(NodeError::NoSpace(space))?;
			destroyed += open.stored.destroy_kept(&purges, &open.keys)?;
		}
		Ok(destroyed)
	}

	/// After a sync: brings this device's wait up to date with marks that may have come, and
	/// gives back what a restore that came brought back, as [`Library::take_back_restored`]
	/// does.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read or written.
	pub fn settle_waiting(&mut self, now: u64) -> Result<usize, NodeError> {
		self.refresh_wait(now)?;
		self.take_back_restored(now, false)
	}

	/// Gives back what this replica keeps apart where a change that restores it came to
	/// count, as after a sync brought one, or at `now` once the library opened. Returns how
	/// many parts came back.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read or written.
	pub fn take_back_restored(&mut self, now: u64, opening: bool) -> Result<usize, NodeError> {
		let mut given = 0;
		let code_types = self.code_types.clone();
		for open in self.spaces.values_mut() {
			if opening || open.stored.restored_since() {
				let back = open.stored.take_back_kept(&open.keys, now)?;
				if back > 0 {
					// What came back may define types.
					open.read_types(&code_types)?;
				}
				given += back;
			}
		}
		Ok(given)
	}

	/// The copies that wait before destroying what a purge of `space` destroys: the person's
	/// archive nodes listed in that space, this device first, as their marks say; their
	/// backups of it; and other members' archive nodes and backups, as their accounts say.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn waiting_copies(&self, space: SpaceId) -> Result<Vec<WaitingCopy>, NodeError> {
		let copy = |name: Option<String>, days: u64| WaitingCopy {
			name,
			this: false,
			backup: false,
			member: None,
			others: false,
			days,
		};
		let mut own: BTreeMap<EntityId, WaitingCopy> = BTreeMap::new();
		let mut own_keys = BTreeSet::new();
		for replica in self.replicas()? {
			if let Some(key) = replica.signing {
				own_keys.insert(*key.as_bytes());
			}
			if replica.revoked || replica.deleted {
				continue;
			}
			let Some(device) = replica.device else {
				continue;
			};
			let days = wait_days(&replica.marks);
			if days == 0 || own.contains_key(&device) {
				continue;
			}
			let holds = match replica.signing {
				Some(key) => self.counts_in(space, &key)?,
				None => false,
			};
			if holds {
				let mut waiting = copy(replica.device_name.clone(), days);
				waiting.this = device == self.info.device;
				own.insert(device, waiting);
			}
		}
		let mut copies: Vec<WaitingCopy> = own.into_values().collect();
		copies.sort_by(|a, b| (!a.this, &a.name).cmp(&(!b.this, &b.name)));
		// The person's backups of the space, as marked.
		let mut known_backups = BTreeSet::new();
		for backup in self.backups()? {
			known_backups.insert(backup.id);
			if !self.backup_spaces(&backup)?.contains(&space) {
				continue;
			}
			let days = backup_wait_days(&backup);
			if days > 0 {
				let mut waiting = copy(Some(backup.name.clone()), days);
				waiting.backup = true;
				copies.push(waiting);
			}
		}
		// Other members' archive nodes, once for each disk, as their accounts say.
		let peers: BTreeMap<[u8; 32], crate::Replica> = self
			.peers()?
			.into_iter()
			.filter_map(|peer| peer.signing.map(|key| (*key.as_bytes(), peer)))
			.collect();
		let mut disks = BTreeSet::new();
		for (author, head) in self.other_heads(space)? {
			if own_keys.contains(&author)
				|| head.role != DeviceRole::Archive
				|| head.waits == 0
				|| !disks.insert(head.disk)
			{
				continue;
			}
			let peer = peers.get(&author);
			let mut waiting = copy(peer.and_then(|peer| peer.device_name.clone()), head.waits);
			waiting.member = peer.and_then(|peer| peer.member.clone());
			waiting.others = true;
			copies.push(waiting);
		}
		// And others' backups, as the latest update of each says.
		for (backup, account) in self.backup_accounts(space)? {
			if known_backups.contains(&backup)
				|| self.backup_forgotten(backup)?
				|| account.head.waits == 0
			{
				continue;
			}
			let mut waiting = copy(None, account.head.waits);
			waiting.backup = true;
			waiting.others = true;
			copies.push(waiting);
		}
		Ok(copies)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_longer_wait_holds_at_once_and_a_shorter_one_after_the_wait_before_it() {
		let day = DAY;
		// None marked, none kept.
		assert_eq!(waits_marked(&[], 0, 10), None);
		let waits = waits_marked(&[], 30 * day, 10).expect("a wait");
		assert_eq!(waits, [(10, 30 * day)]);
		assert_eq!(waits_marked(&waits, 30 * day, 20), None);
		// Longer at once.
		let longer = waits_marked(&waits, 60 * day, 20).expect("longer");
		assert_eq!(wait_at(&longer, 20), 60 * day);
		// Shorter, or none, after the wait before it.
		let none = waits_marked(&longer, 0, 30).expect("none");
		assert_eq!(wait_at(&none, 30), 60 * day);
		assert_eq!(wait_at(&none, 30 + 60 * day - 1), 60 * day);
		assert_eq!(wait_at(&none, 30 + 60 * day), 0);
		// Still none marked: what is to come stays as it was.
		assert_eq!(waits_marked(&none, 0, 40), None);
		// Marked again before it came, shorter than the wait in force: the wait holds, and
		// the new one comes after it.
		let again = waits_marked(&none, 30 * day, 50).expect("again");
		assert_eq!(wait_at(&again, 50), 60 * day);
		assert_eq!(wait_at(&again, 50 + 60 * day), 30 * day);
		assert_eq!(wait_at(&again, 50 + 365 * day), 30 * day);
	}
}
