//! Updating a tree's copy: bringing it to the tree as other devices left it, with new
//! versions, moves, additions and deletions written, and competing values written out:
//! text merged three ways, from the version both started from, with Git's conflict markers
//! where edits clash, and anything else left as it is, with the other version beside it as
//! `NAME.conflict-DEVICE`. An edited copy never loses its edits, nor are changes made here
//! and not committed undone ([technical design: trees]).
//!
//! [technical design: trees]: ../../../../../docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_log::ChangeId;
use nibkeep_model::storage::Statements as _;
use nibkeep_model::{Operation, Value};
use nibkeep_std::{CONTENT, LINK_TARGET, SECRET_CONTENT};

use super::compare::{same_content, still_competes};
use super::history::READ_LIMIT;
use super::index::{Conflict, Entry, Index, Version};
use super::locate::{Located, located};
use super::secrecy::{NIBSECRET, lines};
use super::walk::{Found, FoundKind, look, nanos, open_found};
use super::write::{Content, Place, copy_version, same_version, undo_moves_at};
use super::{CutShort, TreeState, settle_tree, version_of};
use crate::NodeError;
use crate::library::Library;
use crate::library::checkouts::sync_folder_of;

/// What updating a tree's copy did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Updated {
	/// The paths written: new versions, and files added elsewhere.
	pub written: Vec<String>,
	/// The copies moved, from where to where.
	pub moved: Vec<(String, String)>,
	/// The copies deleted, of files deleted elsewhere.
	pub deleted: Vec<String>,
	/// The copies edited here, or whose versions competed, merged cleanly: what a commit
	/// records next.
	pub merged: Vec<String>,
	/// The paths where versions clash: text with Git's conflict markers, or another file with
	/// the other version beside it, as `NAME.conflict-DEVICE`.
	pub conflicts: Vec<String>,
	/// The copies kept as they are, with why, such as one edited here of a file deleted
	/// elsewhere.
	pub kept: Vec<(String, String)>,
	/// The files deleted on one device while another gave them a new version, before either
	/// saw the other, whose copies hold that version now: a commit keeps them, unless they
	/// are deleted first.
	pub contested: Vec<String>,
	/// The paths not written, with why.
	pub skipped: Vec<(String, String)>,
	/// The commits of other devices whose parts have not all arrived, which are left out
	/// until they have.
	pub waiting: Vec<CutShort>,
	/// What changed elsewhere that waits for the person to agree before it is written: moves
	/// of secret files, as `FROM -> TO`, which could put a plain copy where something
	/// publishes it, and `.nibsecret` files that lose lines, by path, which could make new
	/// files ordinary ([technical design: secrets in trees]).
	///
	/// [technical design: secrets in trees]: ../../../docs/Technical-design.md#151-secrets-in-trees
	pub held: Vec<String>,
	/// The secret files not written, as this replica cannot read them: its secrets are
	/// locked, or not on this device.
	pub sealed: Vec<String>,
}

/// A version to write at a path where a copy may be.
#[derive(Clone)]
struct Arriving {
	path: String,
	entity: EntityId,
	/// The version the copy was written or committed at, if this copy had the file.
	base: Option<Version>,
	/// The version to write.
	version: Version,
	/// All the versions of the file, of one that competes with itself, this one among them.
	competing: Vec<Version>,
	/// The competing versions that an update wrote out into the copy already, by their
	/// [`Version::key`].
	written: BTreeSet<String>,
	/// The keys of the deletions that compete with this version, where one device deleted
	/// the file while another gave it the version, before either saw the other.
	deletions: Vec<String>,
	/// Whether it is a secret file's.
	secret: bool,
}

impl Arriving {
	/// Whether nothing came that the copy lacks: no newer version, and no competing one
	/// that was not written out into it.
	fn nothing_new(&self) -> bool {
		let versions = if self.competing.is_empty() {
			self.base.as_ref() == Some(&self.version)
		} else {
			self.competing
				.iter()
				.all(|version| self.written.contains(&version.key()))
		};
		versions && self.deletions.iter().all(|key| self.written.contains(key))
	}

	/// Whether other values compete with this version: versions, or deletions.
	fn competes(&self) -> bool {
		!self.competing.is_empty() || !self.deletions.is_empty()
	}

	/// Notes that the copy holds every value of the file that competes now, if any do, as an
	/// update wrote them out into it, which a commit then settles; and the file among the
	/// contested, if deletions compete with its version.
	fn wrote_out(&self, updated: &mut Updated, changes: &mut Changes) {
		if !self.competes() {
			return;
		}
		let mut keys: BTreeSet<String> = self.competing.iter().map(Version::key).collect();
		keys.insert(self.version.key());
		keys.extend(self.deletions.iter().cloned());
		changes.written_out.push((self.entity, keys));
		if !self.deletions.is_empty() {
			updated.contested.push(self.path.clone());
		}
	}
}

/// What an update works with: the tree and its copy, and the index.
struct Here<'a> {
	space: SpaceId,
	tree: EntityId,
	place: Place<'a>,
	index: &'a Index,
	/// Whether the index holds the whole copy: else a copy that it has no entry of may be
	/// any version of its file.
	whole: bool,
	/// The copies of other files written beside paths, by path.
	conflicts: BTreeMap<String, Conflict>,
	read_at: i64,
}

/// What an update changes in the index as it goes, written once it is done.
#[derive(Default)]
struct Changes {
	set: Vec<(String, Entry)>,
	removed: Vec<String>,
	/// The competing versions written out into each copy, by their keys.
	written_out: Vec<(EntityId, BTreeSet<String>)>,
	/// Whether a copy was left whose version the index does not know.
	unknown: bool,
}

impl Library {
	/// Brings the copy of a tree on this device to the tree as the library has it, as other
	/// devices left it: writes new versions, moves, additions and deletions, merges edits made
	/// here with newer versions, and writes out competing values. What was deleted or moved
	/// here and not committed stays so. A commit of another device in several parts is left
	/// out until all its parts have arrived, unless `partial`, after which this copy keeps
	/// what it took. Nothing is recorded: committing what it wrote settles what competed.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoTreeHere`] if the tree has no folder on this device,
	/// [`NodeError::TreeEmptied`] if the copy holds none of its files, as a drive that is not
	/// there leaves it, and an error if the space or the index cannot be read.
	pub fn update_tree(
		&self,
		space: SpaceId,
		tree: EntityId,
		partial: bool,
	) -> Result<Updated, NodeError> {
		self.update_tree_with(space, tree, (partial, false), None)
	}

	/// Brings the copy of a tree on this device to the tree, as [`Library::update_tree`]
	/// does, with what waits for the person's agreement too: moves of secret files, and
	/// `.nibsecret` files that lose lines, which [`Updated::held`] lists.
	///
	/// # Errors
	///
	/// As [`Library::update_tree`].
	pub fn update_tree_agreeing(
		&self,
		space: SpaceId,
		tree: EntityId,
		partial: bool,
	) -> Result<Updated, NodeError> {
		self.update_tree_with(space, tree, (partial, true), None)
	}

	/// Updates the copy of a tree as [`Library::update_tree_agreeing`] does, but stops once
	/// it has written `written` copies, after its moves and deletions, before it records
	/// anything, as a crash would: for trying what an update cut short leaves.
	///
	/// # Errors
	///
	/// As [`Library::update_tree`].
	pub fn update_tree_cut_after(
		&self,
		space: SpaceId,
		tree: EntityId,
		written: usize,
	) -> Result<Updated, NodeError> {
		self.update_tree_with(space, tree, (false, true), Some(written))
	}

	/// Brings the copy of a tree on this device to the tree, with what waits for the
	/// person's agreement if `agreed` says so; stops before it records anything, once it has
	/// written as many copies as `cut` says, if it says.
	fn update_tree_with(
		&self,
		space: SpaceId,
		tree: EntityId,
		(partial, agreed): (bool, bool),
		cut: Option<usize>,
	) -> Result<Updated, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let mut index = self.index()?;
		let purged = self.learn_purges(space, &index)?;
		let apart = self.apart();
		let place = Place {
			root: &root,
			apart: &apart,
		};
		let now = self.tree_state(space, tree)?;
		settle_tree((tree, &root, &apart), &mut index, (&purged, &now))?;
		self.follow_cut_short(space, (tree, &root, &apart), &mut index, &now)?;
		let entries = index.entries(tree)?;
		// A copy with none of its files is more likely a drive that is not there than one
		// to write the whole tree into.
		if !entries.is_empty()
			&& entries
				.keys()
				.all(|path| located(&root, path, &apart) == Located::Absent)
		{
			return Err(NodeError::TreeEmptied(root));
		}
		let mut updated = Updated::default();
		let in_full: BTreeSet<EntityId> = now.by_entity.keys().copied().collect();
		// What this copy lacks of the secret files, as the tree is now, with the copies it
		// holds standing in.
		let sealed: Vec<String> = now.sealed.keys().cloned().collect();
		let (state, taken) =
			self.state_to_update(space, tree, (now, partial), (&index, &mut updated))?;
		let here = Here {
			space,
			tree,
			place,
			index: &index,
			whole: index.knows(tree)?,
			conflicts: index.conflicts(tree)?,
			read_at: nanos(SystemTime::now()).unwrap_or(0),
		};
		let by_entity: BTreeMap<EntityId, (String, Entry)> = entries
			.iter()
			.map(|(path, entry)| (entry.entity, (path.clone(), entry.clone())))
			.collect();
		let mut changes = Changes::default();
		let written = index.written_out(tree)?;
		let (mut deleted, mut moves, mut arriving) =
			plan_update((&state, &in_full), &by_entity, &written);
		updated.sealed = sealed;
		if !agreed {
			self.hold_back(
				space,
				(&state, &entries),
				(&mut deleted, &mut moves, &mut arriving),
				&mut updated,
			)?;
		}
		tell_moved_apart(&state, &by_entity, &mut updated);
		let noted = note_writing(&index, tree, &arriving)?;
		for (path, entry) in &deleted {
			// Deleted elsewhere: the copy goes, unless it was edited here.
			delete_copy(place, (path, entry), &mut updated, &mut changes.removed);
		}
		// Moves, all at once; then what arrives at each path, the index as the deletions
		// and moves left it.
		let failed = carry_out_moves(place, (&entries, &moves), (&mut updated, &mut changes));
		let held = held_paths(&entries, &changes);
		self.arrive_all(
			&here,
			(&arriving, &failed, &held),
			cut,
			(&mut updated, &mut changes),
		);
		if cut.is_some() {
			return Ok(updated);
		}
		let whole = here.whole;
		index.change(tree, &changes.removed, &changes.set)?;
		self.note_secrets(space, (tree, &index), &state, &changes.set)?;
		for (entity, keys) in &changes.written_out {
			index.write_out(tree, *entity, keys)?;
		}
		clear_obsolete((tree, place, &index), &state, &mut updated)?;
		self.settle_repositories(space, (tree, &index), &mut updated)?;
		if partial {
			index.take_parts(tree, &taken)?;
		}
		// A lost index holds the whole copy again only once every copy was looked at.
		if whole || (!changes.unknown && updated.skipped.is_empty()) {
			index.know(tree)?;
		}
		if noted {
			index.forget_writing(tree)?;
		}
		updated.written.sort();
		updated.merged.sort();
		updated.contested.sort();
		updated.conflicts.sort();
		updated.conflicts.dedup();
		Ok(updated)
	}

	/// Writes what arrives at each path, or beside it, of the arrivals that the deletions and
	/// moves left, all but those whose moves `failed`; stops once it has written as many
	/// copies as `cut` says, if it says.
	fn arrive_all(
		&self,
		here: &Here<'_>,
		(arriving, failed, held): (
			&[Arriving],
			&BTreeSet<EntityId>,
			&BTreeMap<String, EntityId>,
		),
		cut: Option<usize>,
		(updated, changes): (&mut Updated, &mut Changes),
	) {
		for arrival in arriving {
			if cut.is_some_and(|cut| updated.written.len() >= cut) {
				return;
			}
			if failed.contains(&arrival.entity) {
				let why = "it could not be moved where the tree has it".to_owned();
				updated.skipped.push((arrival.path.clone(), why));
				continue;
			}
			if goes_beside(held, here, arrival) {
				self.write_beside(here, arrival, updated);
				continue;
			}
			// One file that fails leaves the others to update.
			if let Err(error) = self.arrive(here, arrival, (updated, changes)) {
				updated
					.skipped
					.push((arrival.path.clone(), error.to_string()));
			}
		}
	}

	/// The tree to bring the copy to: as the library has it, without the commits of other
	/// devices whose parts have not all arrived, unless `partial`, which go in `updated`.
	/// This replica's own commits cut short stay in, as its copy holds what they recorded,
	/// and so do those whose parts this copy took. Returns the state, and the commits cut
	/// short, by their revisions.
	fn state_to_update(
		&self,
		space: SpaceId,
		tree: EntityId,
		(now, partial): (TreeState, bool),
		(index, updated): (&Index, &mut Updated),
	) -> Result<(TreeState, Vec<EntityId>), NodeError> {
		let cut = self.cut_short(space, tree)?;
		let revisions = cut.iter().map(|cut| cut.revision).collect();
		if partial {
			return Ok((now, revisions));
		}
		let this_replica = self.replica.signing.verifying_key();
		let taken = index.taken_parts(tree)?;
		let mut without = BTreeSet::new();
		for cut in cut {
			if taken.contains(&cut.revision) {
				continue;
			}
			let changes = self.revision_changes(space, tree, cut.revision)?;
			let log = self.space(space)?.stored.log();
			let own = match changes.first() {
				Some(first) => log
					.change(*first)?
					.is_some_and(|change| *change.author() == this_replica),
				None => true,
			};
			if !own {
				without.extend(changes);
				updated.waiting.push(cut);
			}
		}
		if without.is_empty() {
			return Ok((now, revisions));
		}
		Ok((self.tree_state_without(space, tree, &without)?, revisions))
	}

	/// Writes a version that arrived at its path: where no copy is, as it is, unless the copy
	/// was deleted or moved here, which stays so; over a copy that holds the version it was
	/// written at; or merged three ways with a copy edited here, and with versions that
	/// compete with it. A copy that the index has no entry of, once the index was lost,
	/// stays as it is.
	fn arrive(
		&self,
		here: &Here<'_>,
		arrival: &Arriving,
		(updated, changes): (&mut Updated, &mut Changes),
	) -> Result<(), NodeError> {
		let path = &arrival.path;
		let nothing_new = arrival.nothing_new();
		let entry = |found: &Found, version: &Version, known: bool| {
			entry_for(arrival.entity, (found, version, known), here.read_at)
		};
		let on_disk = match located(here.place.root, path, here.place.apart) {
			Located::Here(on_disk) => on_disk,
			// Deleted or moved here, and not committed: it stays so.
			// Deleted or moved here from the version that deletions elsewhere compete with:
			// seen, so that a commit deletes or moves it.
			Located::Absent
				if !arrival.deletions.is_empty()
					&& arrival.competing.is_empty()
					&& arrival.base.as_ref() == Some(&arrival.version) =>
			{
				arrival.wrote_out(updated, changes);
				return Ok(());
			}
			Located::Absent if arrival.base.is_some() => {
				if !nothing_new {
					let why = "deleted or moved here, and changed elsewhere: restore it, or \
					           commit with force to delete it"
						.to_owned();
					updated.kept.push((path.clone(), why));
				}
				return Ok(());
			}
			Located::Absent => {
				let content = Content::of(&arrival.version, arrival.secret);
				match self.write_new(here.space, here.place, path, &content) {
					Ok(found) if arrival.competing.is_empty() => {
						updated.written.push(path.clone());
						let written = entry(&found, &arrival.version, true);
						changes.set.push((path.clone(), written));
						arrival.wrote_out(updated, changes);
					}
					// The versions that compete with it, merged into it.
					Ok(found) => {
						updated.written.push(path.clone());
						let written = Arriving {
							base: Some(arrival.version.clone()),
							..arrival.clone()
						};
						return self.arrive_at_copy(
							here,
							&written,
							&found.on_disk,
							(updated, changes),
						);
					}
					Err(why) => updated.skipped.push((path.clone(), why)),
				}
				return Ok(());
			}
			Located::Unknown(why) => {
				updated.skipped.push((path.clone(), why));
				return Ok(());
			}
		};
		if nothing_new {
			return Ok(());
		}
		self.arrive_at_copy(here, arrival, &on_disk, (updated, changes))
	}

	/// Writes a version that arrived where a copy of its file is: over it, if it holds the
	/// version it was written at, or merged with it; or, once the index was lost, leaves it.
	fn arrive_at_copy(
		&self,
		here: &Here<'_>,
		arrival: &Arriving,
		on_disk: &Path,
		(updated, changes): (&mut Updated, &mut Changes),
	) -> Result<(), NodeError> {
		let path = &arrival.path;
		let entry = |found: &Found, version: &Version, known: bool| {
			entry_for(arrival.entity, (found, version, known), here.read_at)
		};
		// Read whatever its size and time say, as an edit may keep both.
		let found = look(on_disk).ok();
		let Some((found, holds)) =
			found.and_then(|found| copy_version(&found).map(|holds| (found, holds)))
		else {
			updated
				.skipped
				.push((path.clone(), "it cannot be read".to_owned()));
			return Ok(());
		};
		if arrival.base.is_none() {
			// Only a lost index leaves a copy here that it has no entry of.
			if same_version(&found, &holds, &arrival.version) {
				changes
					.set
					.push((path.clone(), entry(&found, &arrival.version, true)));
			} else {
				changes.unknown = true;
				let why = "which version it holds is not known, as the index of copies was \
				           lost: commit it, or restore it"
					.to_owned();
				updated.kept.push((path.clone(), why));
			}
			return Ok(());
		}
		// The other versions to bring in: those that compete and were not written out, or
		// the one that arrived.
		let candidates: Vec<&Version> = if arrival.competing.is_empty() {
			vec![&arrival.version]
		} else {
			arrival
				.competing
				.iter()
				.filter(|version| !arrival.written.contains(&version.key()))
				.collect()
		};
		let others: Vec<&Version> = candidates
			.into_iter()
			.filter(|other| !same_version(&found, &holds, other))
			.filter(|other| arrival.base.as_ref() != Some(*other))
			.collect();
		let unedited = arrival
			.base
			.as_ref()
			.is_some_and(|base| same_version(&found, &holds, base));
		if others.is_empty() {
			let known = arrival.competing.is_empty() && unedited;
			changes
				.set
				.push((path.clone(), entry(&found, &arrival.version, known)));
			arrival.wrote_out(updated, changes);
			return Ok(());
		}
		if unedited && arrival.competing.is_empty() {
			let content = Content::of(&arrival.version, arrival.secret);
			match self.write_over(here.space, &found, &content) {
				Ok(()) => {
					updated.written.push(path.clone());
					if let Ok(now) = look(on_disk) {
						let written = entry(&now, &arrival.version, true);
						changes.set.push((path.clone(), written));
						arrival.wrote_out(updated, changes);
					}
				}
				Err(why) => updated.skipped.push((path.clone(), why)),
			}
			return Ok(());
		}
		let Some(clashed) = self.merge_arrival(here, (arrival, &found), others, updated)? else {
			return Ok(());
		};
		if clashed {
			updated.conflicts.push(path.clone());
		} else {
			updated.merged.push(path.clone());
		}
		// The copy now builds on the version that arrived; what it holds is to commit.
		if let Ok(now) = look(on_disk) {
			changes
				.set
				.push((path.clone(), entry(&now, &arrival.version, false)));
			arrival.wrote_out(updated, changes);
		}
		Ok(())
	}

	/// Merges into the copy that `found` is, edited here or competing, each of `others` in
	/// turn, from the version both started from: text three ways, with Git's conflict
	/// markers where edits clash, and what is no text beside it, marked before it is
	/// written. Returns whether anything clashed, or `None` if the copy, or a version
	/// beside it, could not be written, which leaves the copy's base as it was.
	fn merge_arrival(
		&self,
		here: &Here<'_>,
		(arrival, found): (&Arriving, &Found),
		others: Vec<&Version>,
		updated: &mut Updated,
	) -> Result<Option<bool>, NodeError> {
		let path = &arrival.path;
		let space = here.space;
		// A newer version builds on the one this copy was written at; competing versions
		// build on the last one they all superseded.
		let base = if arrival.competing.is_empty() {
			arrival.base.clone()
		} else {
			// Its content, with the executable bit this copy was written with.
			let written = matches!(
				arrival.base,
				Some(Version::File {
					executable: true,
					..
				})
			);
			self.common_base(space, arrival.entity)?
				.map(|base| match base {
					Version::File { content, .. } => Version::File {
						content,
						executable: written,
					},
					link @ Version::Link { .. } => link,
				})
		};
		let mut ours = match &found.kind {
			FoundKind::File => {
				let mut bytes = Vec::new();
				let read = open_found(found)
					.and_then(|file| {
						io::Read::read_to_end(&mut io::Read::take(file, READ_LIMIT + 1), &mut bytes)
					})
					.map_err(NodeError::file(&found.on_disk))?;
				(read as u64 <= READ_LIMIT).then_some(bytes)
			}
			FoundKind::Link(_) => None,
		};
		let secret = arrival.secret;
		let base_bytes = match &base {
			Some(Version::File { content, .. }) => self.version_bytes(space, content, secret)?,
			Some(Version::Link { .. }) => None,
			None => Some(Vec::new()),
		};
		let mut clashed = false;
		let mut merged_any = false;
		for other in others {
			let other_bytes = match other {
				Version::File { content, .. } => self.version_bytes(space, content, secret)?,
				Version::Link { .. } => None,
			};
			let text = match (&ours, &base_bytes, &other_bytes) {
				(Some(ours), Some(base), Some(other)) => [ours, base, other]
					.iter()
					.all(|bytes| super::diff::is_text(bytes))
					.then(|| (ours.clone(), base.clone(), other.clone())),
				_ => None,
			};
			if let Some((mine, from, theirs)) = text {
				match diffy::merge_bytes(&from, &mine, &theirs) {
					Ok(merged) => ours = Some(merged),
					Err(marked) => {
						ours = Some(marked);
						clashed = true;
					}
				}
				merged_any = true;
				continue;
			}
			let device = self.device_of(space, arrival.entity, other);
			let Some((beside, there)) = place_beside(here, (path, &device), other) else {
				let why = format!("{path}.conflict-{device} and the names after it are taken");
				updated.skipped.push((path.clone(), why));
				return Ok(None);
			};
			clashed = true;
			if there {
				continue;
			}
			// Marked first, so that no commit takes it in, whatever stops the update.
			here.index.mark(
				here.tree,
				&beside,
				"NibKeep wrote it, the other version of a file, from another device",
			)?;
			if let Err(why) =
				self.write_new(space, here.place, &beside, &Content::of(other, secret))
			{
				here.index
					.unmark(here.tree, std::slice::from_ref(&beside))?;
				updated.skipped.push((path.clone(), why));
				return Ok(None);
			}
		}
		if merged_any && let Some(bytes) = &ours {
			let executable = merged_executable(found, base.as_ref(), &arrival.version);
			let merged = Content::bytes(bytes, executable, secret);
			if let Err(why) = self.write_over(space, found, &merged) {
				updated.skipped.push((path.clone(), why));
				return Ok(None);
			}
		}
		Ok(Some(clashed))
	}

	/// Writes beside its path the version of another file that competes for it, as two
	/// devices that add one path give it two files, as `NAME.conflict-DEVICE`, marked first,
	/// and then with the file and the version written: what the person does with it settles
	/// that file at the next commit. A copy beside written before gets a newer version
	/// unless the person changed it.
	fn write_beside(&self, here: &Here<'_>, arrival: &Arriving, updated: &mut Updated) {
		let device = self.device_of(here.space, arrival.entity, &arrival.version);
		// The name of this file's copy beside, or a free one: never another's, which may be
		// a settlement still to commit.
		let named = (1..=99)
			.map(|number| match number {
				1 => format!("{}.conflict-{device}", arrival.path),
				_ => format!("{}.conflict-{device}-{number}", arrival.path),
			})
			.find(|beside| match here.conflicts.get(beside) {
				Some(conflict) => conflict.entity == arrival.entity,
				None => located(here.place.root, beside, here.place.apart) == Located::Absent,
			});
		let Some(beside) = named else {
			let why = format!(
				"{}.conflict-{device} and the names after it are taken",
				arrival.path
			);
			updated.skipped.push((arrival.path.clone(), why));
			return;
		};
		let content = Content::of(&arrival.version, arrival.secret);
		let written = match located(here.place.root, &beside, here.place.apart) {
			// Renamed or deleted, as written: settled here, which the next commit records.
			Located::Absent
				if here.conflicts.get(&beside).is_some_and(|conflict| {
					conflict.entity == arrival.entity
						&& conflict
							.written
							.as_ref()
							.is_some_and(|written| same_content(written, &arrival.version))
				}) =>
			{
				let why = "the other file at this path, settled here: commit to record it";
				updated.kept.push((beside, why.to_owned()));
				return;
			}
			Located::Absent => {
				let why = "NibKeep wrote it, the file of another device at the same path";
				match here.index.mark(here.tree, &beside, why) {
					Ok(()) => match self.write_new(here.space, here.place, &beside, &content) {
						Ok(_) => Ok(()),
						Err(why) => {
							let _ = here.index.unmark(here.tree, std::slice::from_ref(&beside));
							Err(why)
						}
					},
					Err(error) => Err(error.to_string()),
				}
			}
			Located::Here(on_disk) => match look(&on_disk) {
				Ok(found) => {
					let holds = copy_version(&found);
					let before = here
						.conflicts
						.get(&beside)
						.filter(|conflict| conflict.entity == arrival.entity)
						.and_then(|conflict| conflict.written.clone());
					let holding = |version: &Version| {
						holds
							.as_ref()
							.is_some_and(|holds| same_content(holds, version))
					};
					if holding(&arrival.version) {
						Ok(())
					} else if before.as_ref().is_some_and(holding) {
						self.write_over(here.space, &found, &content)
					} else {
						Err(format!("{beside} is in the way"))
					}
				}
				Err(why) => Err(why),
			},
			Located::Unknown(why) => Err(why),
		};
		let marked = written.and_then(|()| {
			here.index
				.mark_conflict(
					here.tree,
					(&beside, &arrival.path),
					(arrival.entity, &arrival.version),
				)
				.map_err(|error| error.to_string())
		});
		match marked {
			Ok(()) => updated.conflicts.push(arrival.path.clone()),
			Err(why) => updated.skipped.push((arrival.path.clone(), why)),
		}
	}

	/// Settles the Git repositories that this copy holds and that changed elsewhere: one
	/// deleted elsewhere keeps its folder, with the work it may hold, marked so that commits
	/// leave it out until the person deletes it or renames it to keep it; one moved
	/// elsewhere stays, listed, for the person to move.
	fn settle_repositories(
		&self,
		space: SpaceId,
		(tree, index): (EntityId, &Index),
		updated: &mut Updated,
	) -> Result<(), NodeError> {
		let recorded: BTreeMap<EntityId, String> = self
			.recorded_repositories(space, tree)?
			.into_iter()
			.map(|recorded| (recorded.entity, recorded.path))
			.collect();
		let mut gone = Vec::new();
		for (path, (entity, _)) in index.repositories(tree)? {
			match recorded.get(&entity) {
				Some(now) if *now == path => {}
				Some(now) => {
					let why =
						format!("a Git repository moved elsewhere to {now}: move its folder there");
					updated.kept.push((path, why));
				}
				None => {
					let why = "a Git repository deleted from the tree elsewhere, whose folder stays \
					           with its work until you delete it";
					index.mark(tree, &path, why)?;
					updated.kept.push((path.clone(), why.to_owned()));
					gone.push(path);
				}
			}
		}
		index.forget_repositories(tree, &gone)
	}

	/// The name of the device whose replica made a version of a file, for the name of a copy
	/// written beside another, with what no file name holds left out; `elsewhere` if the
	/// space does not say.
	fn device_of(&self, space: SpaceId, entity: EntityId, version: &Version) -> String {
		let (fields, value) = match version {
			Version::File { content, .. } => {
				(&[CONTENT, SECRET_CONTENT][..], Value::Blob(*content))
			}
			Version::Link { target } => (&[LINK_TARGET][..], Value::Text(target.clone())),
		};
		let name = {
			let open = self.space(space).ok();
			let statement = open.and_then(|open| {
				let state = open.stored.log().state();
				fields.iter().find_map(|field| {
					state
						.current(entity, field)
						.ok()?
						.into_iter()
						.find(|statement| statement.value == value)
						.map(|statement| statement.id)
				})
			});
			statement
				.and_then(|statement| self.device_name_of(space, statement))
				.unwrap_or_else(|| "elsewhere".to_owned())
		};
		let kept: String = name
			.chars()
			.take(40)
			.map(|character| {
				if character.is_alphanumeric() || matches!(character, '-' | '_') {
					character
				} else {
					'-'
				}
			})
			.collect();
		if kept.trim_matches('-').is_empty() {
			"elsewhere".to_owned()
		} else {
			kept
		}
	}

	/// The version of a file that its competing versions all started from: of the versions
	/// that each superseded, directly or through the versions before it, the one recorded
	/// last, as their payloads say.
	fn common_base(&self, space: SpaceId, entity: EntityId) -> Result<Option<Version>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let mut competing = Vec::new();
		for field in [CONTENT, SECRET_CONTENT, LINK_TARGET] {
			for id in storage.of_field(entity, &field)? {
				if state.is_current(id)? {
					competing.push(id);
				}
			}
		}
		let mut common: Option<BTreeSet<StatementId>> = None;
		for id in competing {
			let before = self.superseded_through(space, id)?;
			common = Some(match common {
				Some(common) => common.intersection(&before).copied().collect(),
				None => before,
			});
		}
		let mut last: Option<(nibkeep_time::Timestamp, Version)> = None;
		for id in common.unwrap_or_default() {
			let Some(recorded) = storage.get(id)? else {
				continue;
			};
			let version = match recorded.statement.value.clone() {
				Value::Blob(content) => Version::File {
					content,
					executable: false,
				},
				Value::Text(target) => Version::Link { target },
				_ => continue,
			};
			if last.as_ref().is_none_or(|(at, _)| recorded.at > *at) {
				last = Some((recorded.at, version));
			}
		}
		Ok(last.map(|(_, version)| version))
	}

	/// The statements that a statement superseded, directly or through those it superseded
	/// in turn, as the payloads of their changes say.
	fn superseded_through(
		&self,
		space: SpaceId,
		statement: StatementId,
	) -> Result<BTreeSet<StatementId>, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let mut seen = BTreeSet::new();
		let mut waiting = vec![statement];
		while let Some(id) = waiting.pop() {
			let change = log
				.state()
				.storage()
				.get(id)?
				.and_then(|recorded| recorded.change);
			let Some(change) = change else {
				continue;
			};
			let Some(payload) = log.payload(ChangeId::from_hash(change), &open.keys)? else {
				continue;
			};
			for operation in &payload.operations {
				if let Operation::Assert {
					statement,
					supersedes,
					..
				} = operation
					&& statement.id == id
				{
					for before in supersedes {
						if seen.insert(*before) {
							waiting.push(*before);
						}
					}
				}
			}
		}
		Ok(seen)
	}

	/// Holds back, until the person agrees, what changed elsewhere that could expose a
	/// secret: a move of a secret file, which could put its plain copy where a web server or
	/// a sync program publishes it; and a `.nibsecret` file that loses lines, by an edit, a
	/// move or a deletion, which could make new files ordinary. Any replica of the space can
	/// make such changes, an AI agent's too. What it holds back stays as the copy has it.
	#[allow(
		clippy::type_complexity,
		reason = "the three lists of an update, changed in place"
	)]
	fn hold_back(
		&self,
		space: SpaceId,
		(state, entries): (&TreeState, &BTreeMap<String, Entry>),
		(deleted, moves, arriving): (
			&mut Vec<(String, Entry)>,
			&mut Vec<(String, String)>,
			&mut Vec<Arriving>,
		),
		updated: &mut Updated,
	) -> Result<(), NodeError> {
		let is_nibsecret =
			|path: &str| path == NIBSECRET || path.ends_with(&format!("/{NIBSECRET}"));
		let mut held: BTreeSet<EntityId> = BTreeSet::new();
		moves.retain(|(from, to)| {
			let Some(entry) = entries.get(from) else {
				return true;
			};
			if state.is_secret(entry.entity) || is_nibsecret(from) {
				updated.held.push(format!("{from} -> {to}"));
				held.insert(entry.entity);
				return false;
			}
			true
		});
		deleted.retain(|(path, entry)| {
			if is_nibsecret(path) {
				updated.held.push(path.clone());
				held.insert(entry.entity);
				return false;
			}
			true
		});
		for arrival in arriving.iter() {
			if held.contains(&arrival.entity) || !is_nibsecret(&arrival.path) {
				continue;
			}
			let (
				Some(Version::File { content: was, .. }),
				Version::File {
					content: becomes, ..
				},
			) = (&arrival.base, &arrival.version)
			else {
				continue;
			};
			if was == becomes {
				continue;
			}
			let was = self.content_bytes(space, was)?.unwrap_or_default();
			let becomes = self.content_bytes(space, becomes)?.unwrap_or_default();
			let was = String::from_utf8_lossy(&was);
			let becomes = String::from_utf8_lossy(&becomes);
			if !lines(&was).is_subset(&lines(&becomes)) {
				updated.held.push(arrival.path.clone());
				held.insert(arrival.entity);
			}
		}
		arriving.retain(|arrival| !held.contains(&arrival.entity));
		updated.held.sort();
		Ok(())
	}
}

/// Which file holds each path of the copy, as the index holds it with what an update's
/// deletions and moves changed.
fn held_paths(entries: &BTreeMap<String, Entry>, changes: &Changes) -> BTreeMap<String, EntityId> {
	let mut held: BTreeMap<String, EntityId> = entries
		.iter()
		.filter(|(path, _)| !changes.removed.contains(path))
		.map(|(path, entry)| (path.clone(), entry.entity))
		.collect();
	held.extend(
		changes
			.set
			.iter()
			.map(|(path, entry)| (path.clone(), entry.entity)),
	);
	held
}

/// Lists the copies of files moved apart on two devices, which stay where they are here,
/// for a commit to keep them there.
fn tell_moved_apart(
	state: &TreeState,
	by_entity: &BTreeMap<EntityId, (String, Entry)>,
	updated: &mut Updated,
) {
	for (entity, paths) in &state.paths {
		if let Some((here_at, _)) = by_entity.get(entity)
			&& paths.contains(here_at)
		{
			let elsewhere: Vec<&str> = paths
				.iter()
				.filter(|path| *path != here_at)
				.map(String::as_str)
				.collect();
			let why = format!(
				"moved apart, also to {}: commit to keep it here",
				elsewhere.join(", ")
			);
			updated.kept.push((here_at.clone(), why));
		}
	}
}

/// Where to write beside `path` another version, `other`, of its file, made on `device`:
/// `NAME.conflict-DEVICE`, or with `-2` and on after it where those names are taken by
/// something else; and whether the version is there already, as an update that stopped
/// left it.
fn place_beside(
	here: &Here<'_>,
	(path, device): (&str, &str),
	other: &Version,
) -> Option<(String, bool)> {
	for number in 1..=99 {
		let beside = if number == 1 {
			format!("{path}.conflict-{device}")
		} else {
			format!("{path}.conflict-{device}-{number}")
		};
		match located(here.place.root, &beside, here.place.apart) {
			Located::Absent => return Some((beside, false)),
			Located::Here(on_disk) => {
				let holds = look(&on_disk).ok().and_then(|found| copy_version(&found));
				if holds.is_some_and(|holds| same_content(&holds, other)) {
					return Some((beside, true));
				}
			}
			Located::Unknown(_) => {}
		}
	}
	None
}

/// The index's entry of a copy of `entity` found holding `version`, or built on it, if not
/// `known`: then its time is not kept, so that the next status reads it, and finds the edit
/// to commit.
fn entry_for(
	entity: EntityId,
	(found, version, known): (&Found, &Version, bool),
	read_at: i64,
) -> Entry {
	Entry {
		entity,
		version: version.clone(),
		size: found.size,
		modified: if known { found.modified } else { None },
		read_at,
	}
}

/// The executable bit of a merge: the copy's, if this copy changed it from the version both
/// started from, and else the one that arrived.
fn merged_executable(found: &Found, base: Option<&Version>, arrived: &Version) -> bool {
	let is = |version: Option<&Version>| {
		matches!(
			version,
			Some(Version::File {
				executable: true,
				..
			})
		)
	};
	match found.executable {
		Some(here) if here != is(base) => here,
		_ => is(Some(arrived)),
	}
}

/// Deletes the copy at `path` of a file deleted elsewhere, if it holds the version it was
/// written or committed at, and is still the file read; an edited copy stays, and is
/// listed.
fn delete_copy(
	place: Place<'_>,
	(path, entry): (&String, &Entry),
	updated: &mut Updated,
	removed: &mut Vec<String>,
) {
	let on_disk = match located(place.root, path, place.apart) {
		Located::Here(on_disk) => on_disk,
		Located::Absent => {
			removed.push(path.clone());
			return;
		}
		Located::Unknown(why) => {
			updated.skipped.push((path.clone(), why));
			return;
		}
	};
	let Ok(found) = look(&on_disk) else {
		updated
			.skipped
			.push((path.clone(), "it cannot be read".to_owned()));
		return;
	};
	let unedited =
		copy_version(&found).is_some_and(|holds| same_version(&found, &holds, &entry.version));
	if !unedited {
		let why = "deleted elsewhere, and edited here".to_owned();
		updated.kept.push((path.clone(), why));
		return;
	}
	// Deleted only if it is still the file read, as a save made meanwhile replaces it.
	if !super::write::still_the_copy(&found) {
		updated
			.kept
			.push((path.clone(), "it changed meanwhile".to_owned()));
		return;
	}
	match fs::remove_file(&on_disk) {
		Ok(()) => {
			sync_folder_of(&on_disk);
			updated.deleted.push(path.clone());
			removed.push(path.clone());
		}
		Err(error) => updated.skipped.push((path.clone(), error.to_string())),
	}
}

/// Moves the copies of files moved elsewhere, all at once, so that swaps and chains
/// work, and returns the files whose copies could not be moved.
fn carry_out_moves(
	place: Place<'_>,
	(entries, moves): (&BTreeMap<String, Entry>, &[(String, String)]),
	(updated, changes): (&mut Updated, &mut Changes),
) -> BTreeSet<EntityId> {
	let at = undo_moves_at(place, moves, &mut updated.skipped);
	let mut failed = BTreeSet::new();
	for (from, to) in moves {
		let Some(entry) = entries.get(from) else {
			continue;
		};
		if at.get(from) == Some(to) {
			updated.moved.push((from.clone(), to.clone()));
			changes.removed.push(from.clone());
			changes.set.push((to.clone(), entry.clone()));
		} else {
			failed.insert(entry.entity);
		}
	}
	failed
}

/// Notes in the index the versions of secret files that an update is about to write: with
/// the secrets locked, only this tells a copy written here, before a crash cut the update
/// short, from one edited here. Returns whether it noted any.
fn note_writing(index: &Index, tree: EntityId, arriving: &[Arriving]) -> Result<bool, NodeError> {
	let secrets: Vec<(String, EntityId, Version)> = arriving
		.iter()
		.filter(|arrival| arrival.secret && !arrival.nothing_new())
		.map(|arrival| {
			(
				arrival.path.clone(),
				arrival.entity,
				arrival.version.clone(),
			)
		})
		.collect();
	if secrets.is_empty() {
		return Ok(false);
	}
	index.about_to_write(tree, &secrets)?;
	Ok(true)
}

/// What changed elsewhere, file by file: the copies of files deleted elsewhere, with their
/// entries; the moves; and the versions that arrive at each path.
#[allow(clippy::type_complexity, reason = "three lists, made once")]
fn plan_update(
	(state, in_full): (&TreeState, &BTreeSet<EntityId>),
	by_entity: &BTreeMap<EntityId, (String, Entry)>,
	written: &BTreeMap<EntityId, BTreeSet<String>>,
) -> (Vec<(String, Entry)>, Vec<(String, String)>, Vec<Arriving>) {
	let mut deleted = Vec::new();
	let mut moves = Vec::new();
	let mut arriving = Vec::new();
	let entities: BTreeSet<EntityId> = state
		.by_entity
		.keys()
		.chain(by_entity.keys())
		.copied()
		.collect();
	for entity in entities {
		match (by_entity.get(&entity), version_of(state, entity)) {
			// Only left out, with a commit not yet whole: neither deleted nor written.
			(Some(_), None) if in_full.contains(&entity) => {}
			(Some((path, entry)), None) => deleted.push((path.clone(), entry.clone())),
			(ours, Some((path, version))) => {
				// A file whose content was purged has nothing to write: settling deletes it.
				let Some(version) = version else {
					continue;
				};
				// A file moved apart stays at the path of its copy, which a commit keeps.
				let path = match ours {
					Some((from, _)) if state.at(entity, from) => from.clone(),
					_ => path,
				};
				if let Some((from, _)) = ours
					&& *from != path
				{
					moves.push((from.clone(), path.clone()));
				}
				arriving.push(Arriving {
					path,
					entity,
					base: ours.map(|(_, entry)| entry.version.clone()),
					version,
					competing: state.versions.get(&entity).cloned().unwrap_or_default(),
					written: written.get(&entity).cloned().unwrap_or_default(),
					deletions: state.contested.get(&entity).cloned().unwrap_or_default(),
					secret: state.is_secret(entity),
				});
			}
			(None, None) => {}
		}
	}
	(deleted, moves, arriving)
}

/// Whether the version that arrives at a path goes beside what is there: where another file
/// of this copy holds the path, as the deletions and moves of this update left the index,
/// or, where the index holds the whole copy, a copy that is none of the tree's is there,
/// which is never written over.
fn goes_beside(held: &BTreeMap<String, EntityId>, here: &Here<'_>, arrival: &Arriving) -> bool {
	match held.get(&arrival.path) {
		Some(entity) => *entity != arrival.entity,
		None if arrival.base.is_some() || !here.whole => false,
		None => match located(here.place.root, &arrival.path, here.place.apart) {
			Located::Here(on_disk) => look(&on_disk)
				.ok()
				.and_then(|found| copy_version(&found).map(|holds| (found, holds)))
				.is_none_or(|(found, holds)| !same_version(&found, &holds, &arrival.version)),
			Located::Absent | Located::Unknown(_) => false,
		},
	}
}

/// Clears the copies written beside paths whose files no longer compete for them, as the
/// person settled it elsewhere: one gone, or as it was written, which the tree holds, goes,
/// and its mark with it; one the person changed stays marked, as their own.
fn clear_obsolete(
	(tree, place, index): (EntityId, Place<'_>, &Index),
	state: &TreeState,
	updated: &mut Updated,
) -> Result<(), NodeError> {
	let placed: BTreeSet<EntityId> = index
		.entries(tree)?
		.values()
		.map(|entry| entry.entity)
		.collect();
	for (beside, conflict) in index.conflicts(tree)? {
		if still_competes(state, &placed, &conflict) {
			continue;
		}
		let on_disk = match located(place.root, &beside, place.apart) {
			Located::Here(on_disk) => on_disk,
			Located::Absent => {
				index.unmark(tree, std::slice::from_ref(&beside))?;
				continue;
			}
			Located::Unknown(_) => continue,
		};
		let found = look(&on_disk).ok();
		let as_written = found.as_ref().is_some_and(|found| {
			copy_version(found)
				.zip(conflict.written.as_ref())
				.is_some_and(|(holds, written)| same_content(&holds, written))
		});
		let still = found.is_some_and(|found| {
			look(&on_disk).is_ok_and(|now| {
				now.identity == found.identity
					&& now.size == found.size
					&& now.modified == found.modified
			})
		});
		if as_written && still && fs::remove_file(&on_disk).is_ok() {
			sync_folder_of(&on_disk);
			index.unmark(tree, std::slice::from_ref(&beside))?;
			updated.deleted.push(beside);
		} else {
			index.forget_conflict(tree, &beside)?;
		}
	}
	Ok(())
}
