//! Devices that choose their spaces: dropping a space
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
//!
//! A device drops a space as the person asks there. A drop that would leave the space below
//! its policies, or on no other device of the person's that remains, is refused, and says
//! what is missing; so is one while a tree's copy, or an edited checkout, of the space is on
//! the device. Otherwise the space's folder takes a mark, `dropping`, and the drop waits:
//! for the device's own changes there to be on other disks, for as many disks as the
//! space's Critical policy asks for to hold all of the space's changes, for the copies
//! elsewhere of the blobs it holds to be proven, as the holder of the sync role removes them
//! a pass at a time, the planner keeping none of the space's blobs while the mark is there,
//! and for another replica that remains to hold the space's secrets, where this one holds
//! them. Then it states its choice not to hold the space, and once a device of the person's
//! that holds the space tells, by its account in the personal space, that it holds that
//! choice, it deletes the space: its unedited checkouts, the blobs that no other space of the
//! replica keeps, its key, and its folder, the mark last, so that a deletion that a power cut
//! stops is finished as the library opens. A space that the person left is dropped the same
//! way, but at once, without waiting for copies, which are no longer theirs to keep.

use std::fs;
use std::path::PathBuf;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::Choice;
use nibkeep_log::ChangeId;
use nibkeep_model::fields;
use nibkeep_model::storage::Statements;

use nibkeep_store::log_path;

use super::Library;
use super::accounts::Copies;
use super::checkouts::sync_folder_of;
use super::spaces::Personal;
use crate::{Needs, NodeError};

/// The name of the mark, in a space's folder, of a drop that this device makes.
const MARK: &str = "dropping";

/// What the mark of a space that the person left holds.
const LEFT: &[u8] = b"left";

/// How far a drop of a space on this device has come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropping {
	/// The space.
	pub space: SpaceId,
	/// How far it has come.
	pub stage: DropStage,
	/// What it waits for before it states its choice, while it waits.
	pub waits: Vec<DropWait>,
}

/// How far a drop has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropStage {
	/// This device holds the space still, and waits for what the drop's waits say.
	Waiting,
	/// This device stated its choice not to hold the space, and deletes it once a device of
	/// the person's that holds the space holds that choice.
	Dropped,
	/// The space is gone from this device.
	Deleted,
}

/// What a drop waits for before this device states its choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropWait {
	/// The newest of the space's changes that this disk alone holds, as no other disk's
	/// account says it holds them yet, and whether there may be more of them.
	OwnChanges {
		/// How many.
		changes: usize,
		/// Whether there may be more, beyond those looked at.
		more: bool,
	},
	/// Fewer disks besides this one hold all of the space's changes than the space's
	/// Critical policy asks for: how many do, how many of them are away from home, and how
	/// many are archive nodes'.
	ChangeCopies {
		/// The other disks that hold every change.
		disks: usize,
		/// How many of them are away from home.
		away: usize,
		/// How many of them are archive nodes'.
		archived: usize,
		/// What the Critical policy asks.
		needs: Needs,
	},
	/// The sealed blobs of the space that this device holds and no other space of the
	/// replica keeps: each goes once its copies elsewhere are proven, as the holder of the
	/// sync role removes copies, a pass at a time.
	Files(usize),
	/// This device holds the space's secrets, and no other replica that remains in the
	/// space does.
	Secrets,
}

/// Why a drop of a space is refused now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropRefusal {
	/// A tree of the space has its folder on this device, which the person deals with first.
	Tree(PathBuf),
	/// A checkout of the space was edited since it was written, which the person deals with
	/// first.
	EditedCheckout(PathBuf),
	/// No other device of the person's that remains holds the space, so none could approve
	/// a request there, nor carry a revocation into it.
	OnlyDevice,
	/// The space's changes would be on fewer disks than its Critical policy asks for, as
	/// the accounts of the other disks say: how many other disks there are, how many of them
	/// are away from home, and how many are archive nodes'.
	ChangeCopies {
		/// The other disks.
		disks: usize,
		/// How many of them are away from home.
		away: usize,
		/// How many of them are archive nodes'.
		archived: usize,
		/// What the Critical policy asks.
		needs: Needs,
	},
	/// Files whose copies on other disks would not meet their policy, as those disks'
	/// accounts say.
	Files {
		/// The policy's name, if it has one.
		policy: Option<String>,
		/// What it asks.
		needs: Needs,
		/// The files.
		files: Vec<EntityId>,
	},
	/// Blobs held here, besides the files' current contents too, as their older versions,
	/// thumbnails and pictures, of which other disks' accounts show fewer copies than they
	/// need: how many, which the device could then never remove.
	Blobs(usize),
}

/// The reasons a drop is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropRefusals(pub Vec<DropRefusal>);

impl std::fmt::Display for DropRefusals {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let mut first = true;
		for refusal in &self.0 {
			if !first {
				f.write_str("; ")?;
			}
			first = false;
			match refusal {
				DropRefusal::Tree(path) => {
					write!(f, "a tree's folder is here, at {}", path.display())?;
				}
				DropRefusal::EditedCheckout(path) => {
					write!(f, "a checkout was edited, at {}", path.display())?;
				}
				DropRefusal::OnlyDevice => f.write_str("no other device of yours holds it")?,
				DropRefusal::ChangeCopies { disks, needs, .. } => {
					write!(
						f,
						"its changes would be on {disks} other disks, where its Critical policy \
						 asks for {}",
						needs.copies
					)?;
					write_marks(f, needs)?;
				}
				DropRefusal::Files {
					policy,
					needs,
					files,
				} => {
					let count = if files.len() == 1 {
						"1 file".to_owned()
					} else {
						format!("{} files", files.len())
					};
					write!(
						f,
						"{count} would have fewer copies than {} asks: {}",
						policy.as_deref().unwrap_or("their policy"),
						needs.copies
					)?;
					write_marks(f, needs)?;
				}
				DropRefusal::Blobs(blobs) => {
					write!(
						f,
						"{} held here, of files' older versions, thumbnails or pictures among \
						 them, would have fewer copies elsewhere than they need",
						if *blobs == 1 {
							"1 blob".to_owned()
						} else {
							format!("{blobs} blobs")
						}
					)?;
				}
			}
		}
		Ok(())
	}
}

/// Writes how many copies away from home and on archive nodes `needs` asks for, where it
/// asks for any.
fn write_marks(f: &mut std::fmt::Formatter<'_>, needs: &Needs) -> std::fmt::Result {
	if needs.away > 0 {
		write!(f, ", {} away from home", needs.away)?;
	}
	if needs.archived > 0 {
		write!(f, ", {} on archive nodes", needs.archived)?;
	}
	Ok(())
}

impl Library {
	/// Drops a space on this device, as the person asks here: refuses as [`DropRefusal`]
	/// says; otherwise marks it, and moves the drop on as far as it can now, as
	/// [`Library::drop_progress`] then tells. The holder of the sync role goes on with it, a
	/// pass of removals at a time, and the syncs after, until the space is deleted. A drop
	/// asked for again tells how far it has come.
	///
	/// # Errors
	///
	/// Returns [`NodeError::PersonalSpaceHeld`] for the personal space,
	/// [`NodeError::NoSpace`] for a space that this device does not hold,
	/// [`NodeError::DropRefused`] with the reasons, and an error if this replica is an AI
	/// agent's or revoked, or the library cannot be read or changed.
	pub fn drop_space(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Dropping, NodeError> {
		if space == self.info.personal {
			return Err(NodeError::PersonalSpaceHeld);
		}
		self.check_not_revoked(self.info.personal)?;
		self.check_not_ai_agent("choose the spaces it holds")?;
		// Asked again, also once its choice no longer holds the space.
		if self.is_dropping(space) {
			return self.settle_drop(space, wall_millis, rng);
		}
		{
			let open = self.space(space)?;
			let own = self.replica.signing.verifying_key();
			if !open.stored.log().holds_space(&own, &open.keys)? {
				return Err(NodeError::NoSpace(space));
			}
		}
		// A member who left, or was removed, holds the space read only: no longer theirs.
		let left = self.has_left(space)?;
		let refused = self.drop_refusals(space, left, wall_millis)?;
		if !refused.is_empty() {
			return Err(NodeError::DropRefused(DropRefusals(refused)));
		}
		let mark = self.drop_mark(space);
		fs::write(&mark, if left { LEFT } else { b"" }).map_err(NodeError::file(&mark))?;
		sync_folder_of(&mark);
		self.settle_drop(space, wall_millis, rng)
	}

	/// How far a drop of a space on this device has come, and what it waits for; `None` if
	/// this device does not drop it, as when it never did, took it back, or deleted it.
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read.
	pub fn drop_progress(&self, space: SpaceId) -> Result<Option<Dropping>, NodeError> {
		if !self.spaces.contains_key(&space) || !self.is_dropping(space) {
			return Ok(None);
		}
		let read = self.read_personal()?;
		if self.drop_stated(space, &read) {
			return Ok(Some(Dropping {
				space,
				stage: DropStage::Dropped,
				waits: Vec::new(),
			}));
		}
		let waits = if self.dropping_left(space) {
			Vec::new()
		} else {
			self.drop_waits(space, self.wall.get())?
		};
		Ok(Some(Dropping {
			space,
			stage: DropStage::Waiting,
			waits,
		}))
	}

	/// Whether this device drops a space: its folder holds the mark.
	pub(crate) fn is_dropping(&self, space: SpaceId) -> bool {
		self.spaces.contains_key(&space) && self.drop_mark(space).exists()
	}

	/// Takes back a space that this device drops, before it stated its choice: takes the
	/// mark away. Whether it did.
	pub(crate) fn take_back_drop(&mut self, space: SpaceId) -> Result<bool, NodeError> {
		if !self.is_dropping(space) || self.drop_stated(space, &self.read_personal()?) {
			return Ok(false);
		}
		let mark = self.drop_mark(space);
		fs::remove_file(&mark).map_err(NodeError::file(&mark))?;
		sync_folder_of(&mark);
		Ok(true)
	}

	/// Ends a drop of a space that this device made, whatever its stage, as when it asks for
	/// the space again: takes the mark away.
	pub(crate) fn end_drop(&self, space: SpaceId) -> Result<(), NodeError> {
		if !self.is_dropping(space) {
			return Ok(());
		}
		let mark = self.drop_mark(space);
		fs::remove_file(&mark).map_err(NodeError::file(&mark))?;
		sync_folder_of(&mark);
		Ok(())
	}

	/// Moves on each drop that this device makes, after a sync or a pass of removals: states
	/// the choice of each whose waits are over, and deletes each whose choice a device of the
	/// person's that holds the space holds; then, while the secrets are unlocked, deletes
	/// those of the spaces deleted while they were locked. Each goes on whatever another's
	/// error; the first is told at the end.
	pub(crate) fn settle_drops(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let dropping: Vec<SpaceId> = self
			.spaces
			.keys()
			.copied()
			.filter(|&space| self.is_dropping(space))
			.collect();
		let mut first = Ok(());
		for space in dropping {
			let settled = self.settle_drop(space, wall_millis, rng).map(|_| ());
			if first.is_ok() {
				first = settled;
			}
		}
		// Tried again after the next sync, which it never fails.
		let _ = self.forget_secrets_of_dropped(rng);
		first
	}

	/// Moves a drop on as far as it can now, as [`Library::settle_drops`] says.
	fn settle_drop(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Dropping, NodeError> {
		let left = self.dropping_left(space);
		let read = self.read_personal()?;
		if !self.drop_stated(space, &read) {
			let waits = if left {
				Vec::new()
			} else {
				self.drop_waits(space, wall_millis)?
			};
			if !waits.is_empty() {
				return Ok(Dropping {
					space,
					stage: DropStage::Waiting,
					waits,
				});
			}
			self.state_space_choice(space, false, wall_millis, rng)?;
		}
		let read = self.read_personal()?;
		if (left || self.holder_holds_drop(space, &read)?) && self.delete_dropped(space, rng)? {
			return Ok(Dropping {
				space,
				stage: DropStage::Deleted,
				waits: Vec::new(),
			});
		}
		Ok(Dropping {
			space,
			stage: DropStage::Dropped,
			waits: Vec::new(),
		})
	}

	/// The mark of a drop of a space, in its folder.
	fn drop_mark(&self, space: SpaceId) -> PathBuf {
		space_folder(&self.directory, space).join(MARK)
	}

	/// Whether a drop is of a space that the person left, as its mark says.
	fn dropping_left(&self, space: SpaceId) -> bool {
		fs::read(self.drop_mark(space)).is_ok_and(|held| held == LEFT)
	}

	/// Whether this replica's newest choice of a space, in the personal space, `personal`,
	/// drops it: its drop is stated.
	fn drop_stated(&self, space: SpaceId, personal: &Personal) -> bool {
		self.own_choices(personal)
			.get(&space)
			.is_some_and(|(_, holds)| !holds)
	}

	/// Why a drop of a space would be refused now, as [`DropRefusal`] says; a space that
	/// the person left, `left`, is refused only for its trees and edited checkouts.
	fn drop_refusals(
		&self,
		space: SpaceId,
		left: bool,
		wall_millis: u64,
	) -> Result<Vec<DropRefusal>, NodeError> {
		let mut refused = Vec::new();
		for tree in self.trees(space)? {
			if let Some(root) = tree.root {
				refused.push(DropRefusal::Tree(root));
			}
		}
		for checkout in self.checkouts(space)? {
			if checkout.copy == super::checkouts::CopyState::Edited {
				refused.push(DropRefusal::EditedCheckout(checkout.path));
			}
		}
		if left {
			return Ok(refused);
		}
		if self.other_holders(space)?.is_empty() {
			refused.push(DropRefusal::OnlyDevice);
		}
		let copies = self.copies_by_disk(space, wall_millis)?;
		let needs = self.critical_needs(space)?;
		let others: std::collections::BTreeSet<usize> = (0..copies.disks.len())
			.filter(|&disk| disk != Copies::OWN)
			.collect();
		let (away, archived) = copies.marked(&others);
		if needs.lacks(others.len(), away, archived).any() {
			refused.push(DropRefusal::ChangeCopies {
				disks: others.len(),
				away,
				archived,
				needs,
			});
		}
		for count in self.policies_without_this_disk(space, wall_millis)? {
			if !count.lacking.is_empty() {
				refused.push(DropRefusal::Files {
					policy: count.name,
					needs: count.needs,
					files: count.lacking,
				});
			}
		}
		let short = self.blobs_short_elsewhere(space, &copies)?;
		if short > 0 {
			refused.push(DropRefusal::Blobs(short));
		}
		Ok(refused)
	}

	/// What a drop of a space waits for now, as [`DropWait`] says.
	fn drop_waits(&self, space: SpaceId, wall_millis: u64) -> Result<Vec<DropWait>, NodeError> {
		let mut waits = Vec::new();
		let copies = self.copies_by_disk(space, wall_millis)?;
		let needs = self.critical_needs(space)?;
		let changes = self.change_copies(space, &copies, needs)?;
		if changes.here_alone > 0 {
			waits.push(DropWait::OwnChanges {
				changes: changes.here_alone,
				more: changes.here_alone_more,
			});
		}
		let mut holding = self.disks_holding_all(space, &copies)?;
		holding.remove(&Copies::OWN);
		let (away, archived) = copies.marked(&holding);
		if needs.lacks(holding.len(), away, archived).any() {
			waits.push(DropWait::ChangeCopies {
				disks: holding.len(),
				away,
				archived,
				needs,
			});
		}
		let stored = &self.space(space)?.stored;
		let held = stored.sealed_blobs()?;
		let elsewhere = stored.keyed_elsewhere(&held);
		let files = held
			.iter()
			.filter(|sealed| !elsewhere.contains(sealed))
			.count();
		if files > 0 {
			waits.push(DropWait::Files(files));
		}
		if self.alone_holds_secrets(space)? {
			waits.push(DropWait::Secrets);
		}
		Ok(waits)
	}

	/// The keys of the other devices of the person's that hold a space and remain: by their
	/// newest choices in the personal space, or with none, as a device with no choice holds
	/// the space; neither revoked there nor AI agents'; and in the space, listed, neither
	/// revoked nor ended.
	fn other_holders(&self, space: SpaceId) -> Result<Vec<[u8; 32]>, NodeError> {
		let personal = self.info.personal;
		let read = self.read_personal()?;
		let roster = self.roster(personal)?;
		let newest = Self::newest_choices(&read, &roster);
		let personal_open = self.space(personal)?;
		let personal_log = personal_open.stored.log();
		let open = self.space(space)?;
		let log = open.stored.log();
		let mut holders = Vec::new();
		for replica in roster.replicas() {
			if replica == self.info.replica {
				continue;
			}
			for key in roster.keys_of(replica) {
				let holds = newest
					.get(&(space, *key.as_bytes()))
					.is_none_or(|(_, choice)| choice.holds);
				if !holds
					|| holders.contains(key.as_bytes())
					|| personal_log.is_revoked(key, &personal_open.keys)?
					|| personal_log.is_ai_agent(key, &personal_open.keys)?
					|| !log.is_listed(key, &open.keys)?
					|| log.is_revoked(key, &open.keys)?
					|| log.is_removed(key, &open.keys)?
				{
					continue;
				}
				holders.push(*key.as_bytes());
			}
		}
		Ok(holders)
	}

	/// Whether a device of the person's that holds a space and remains holds this replica's
	/// stated drop of it: its latest account in the personal space names heads that this
	/// replica holds whole, and one of which builds on the change that stated the drop.
	fn holder_holds_drop(&self, space: SpaceId, personal: &Personal) -> Result<bool, NodeError> {
		let Some(stated) = self.drop_change(space, personal)? else {
			return Ok(false);
		};
		let holders = self.other_holders(space)?;
		let open = self.space(self.info.personal)?;
		let log = open.stored.log();
		for (author, head) in self.other_heads(self.info.personal)? {
			if !holders.contains(&author) {
				continue;
			}
			for &held in &head.heads {
				if log.is_complete(held)? && log.builds_on_all(held, &[stated])? {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	/// The change of the personal space that stated this replica's newest choice of a space,
	/// where that choice drops it.
	fn drop_change(
		&self,
		space: SpaceId,
		personal: &Personal,
	) -> Result<Option<ChangeId>, NodeError> {
		let Some((number, false)) = self.own_choices(personal).get(&space).copied() else {
			return Ok(None);
		};
		let own = self.replica.signing.verifying_key();
		let open = self.space(self.info.personal)?;
		let state = open.stored.log().state();
		for statement in state.current(self.info.replica, &fields::SPACE_CHOICE)? {
			let Some(choice) = statement.value.as_text().and_then(Choice::from_text) else {
				continue;
			};
			if choice.space != space
				|| choice.key != own
				|| choice.number != number
				|| choice.holds
				|| !choice.verifies()
			{
				continue;
			}
			if let Some(recorded) = state.storage().get(statement.id)?
				&& let Some(change) = recorded.change
			{
				return Ok(Some(ChangeId::from_hash(change)));
			}
		}
		Ok(None)
	}

	/// Whether this replica holds a space's secrets, and no other replica that remains in
	/// the space does, as the space's records say.
	fn alone_holds_secrets(&self, space: SpaceId) -> Result<bool, NodeError> {
		let roster = self.roster(space)?;
		let own = self.own_record(space)?;
		let holders = roster.secrets_holders();
		if !holders.contains(&own) {
			return Ok(false);
		}
		for holder in holders {
			if holder != own && self.is_remaining(space, &roster, holder)? {
				return Ok(false);
			}
		}
		Ok(true)
	}

	/// Deletes a space that this device dropped, once it has the space alone, as no other
	/// program has it open: its unedited checkouts, the blobs that no other space of the
	/// replica keeps, its secrets keys where they are unlocked, its key, and then its folder,
	/// the mark last. Whether it did; it tries again after the next sync.
	fn delete_dropped(
		&mut self,
		space: SpaceId,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		if !self.space(space)?.stored.hold_alone() {
			return Ok(false);
		}
		// Until its key goes, a step that fails lets the others open the space again, and the
		// next sync tries again.
		if let Err(error) = self.release_dropped(space, rng) {
			if let Ok(open) = self.space(space) {
				open.stored.share_again();
			}
			return Err(error);
		}
		// The key first: a key without its log would keep the library from opening, and a
		// folder without its key, with the mark, is deleted as the library opens.
		let compartment = self.compartment.clone();
		let mut saved = None;
		let removed = self.update_key_file(|key_file| {
			let mut replica = key_file.replica(&compartment)?;
			replica.spaces.remove(&space);
			key_file.set_replica(&replica, &compartment, rng)?;
			saved = Some(replica.spaces);
			Ok(())
		});
		if let Err(error) = removed {
			if let Ok(open) = self.space(space) {
				open.stored.share_again();
			}
			return Err(error);
		}
		if let Some(spaces) = saved {
			self.replica.spaces = spaces;
		}
		self.spaces.remove(&space);
		self.accounts_refreshed.borrow_mut().remove(&space);
		if self.chosen == Some(space) {
			self.chosen = None;
		}
		delete_space_folder(&self.directory, space);
		self.choose_writing_keys()?;
		Ok(true)
	}

	/// Deletes what a dropped space leaves beside its folder and key: its unedited checkouts,
	/// a checkout edited since staying, the person's own file now; the blobs that no other
	/// space of the replica keeps; and its secrets keys, where they are unlocked.
	fn release_dropped(
		&mut self,
		space: SpaceId,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.delete_unedited_checkouts(space)?;
		self.space(space)?.stored.release_all_blobs()?;
		if let Some(compartment) = self.secrets_compartment.clone() {
			self.update_key_file(|key_file| {
				let mut keys = key_file.secrets(&compartment)?;
				if keys.spaces.remove(&space).is_some() {
					key_file.update_secrets(&keys, &compartment, rng)?;
				}
				Ok(())
			})?;
		}
		Ok(())
	}

	/// Deletes the secrets keys of the spaces that this replica dropped and deleted, as a
	/// deletion leaves them while the secrets are locked: once they are unlocked.
	pub(crate) fn forget_secrets_of_dropped(
		&mut self,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(compartment) = self.secrets_compartment.clone() else {
			return Ok(());
		};
		let read = self.read_personal()?;
		let dropped: Vec<SpaceId> = self
			.own_choices(&read)
			.into_iter()
			.filter(|(space, (_, holds))| !holds && !self.spaces.contains_key(space))
			.map(|(space, _)| space)
			.collect();
		if dropped.is_empty() {
			return Ok(());
		}
		self.update_key_file(|key_file| {
			let mut keys = key_file.secrets(&compartment)?;
			let before = keys.spaces.len();
			keys.spaces.retain(|space, _| !dropped.contains(space));
			if keys.spaces.len() != before {
				key_file.update_secrets(&keys, &compartment, rng)?;
			}
			Ok(())
		})
	}

	/// Finishes each deletion of a dropped space that a power cut stopped once the space's
	/// key had gone: deletes each folder of a space that the key file holds no key of, and
	/// that holds the mark of a drop.
	pub(crate) fn finish_cut_short_drops(&self) {
		let Ok(entries) = fs::read_dir(self.directory.join("spaces")) else {
			return;
		};
		for entry in entries.filter_map(Result::ok) {
			let Some(space) = entry
				.file_name()
				.to_str()
				.and_then(|name| name.parse::<SpaceId>().ok())
			else {
				continue;
			};
			if !self.replica.spaces.contains_key(&space) && entry.path().join(MARK).exists() {
				delete_space_folder(&self.directory, space);
			}
		}
	}
}

/// A space's folder in the replica's folder `directory`.
fn space_folder(directory: &std::path::Path, space: SpaceId) -> PathBuf {
	log_path(directory, space).parent().map_or_else(
		|| directory.join("spaces").join(space.to_string()),
		ToOwned::to_owned,
	)
}

/// Deletes a dropped space's folder in the replica's folder `directory`: everything in it
/// but the mark, then the mark, then the folder, so that a folder that holds anything holds
/// the mark until the end. What cannot be deleted now waits for the library's next opening.
fn delete_space_folder(directory: &std::path::Path, space: SpaceId) {
	let folder = space_folder(directory, space);
	let Ok(entries) = fs::read_dir(&folder) else {
		return;
	};
	let mut all = true;
	for entry in entries.filter_map(Result::ok) {
		if entry.file_name() == MARK {
			continue;
		}
		let path = entry.path();
		let deleted = if path.is_dir() {
			fs::remove_dir_all(&path)
		} else {
			fs::remove_file(&path)
		};
		all &= deleted.is_ok();
	}
	if !all {
		return;
	}
	sync_folder_of(&folder.join(MARK));
	if fs::remove_file(folder.join(MARK)).is_ok() && fs::remove_dir(&folder).is_ok() {
		sync_folder_of(&folder);
	}
}
