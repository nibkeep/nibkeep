//! Writing trees: a tree into a folder of its own, as it is, at a revision or as of a
//! moment; an older version or a deleted file into the copy; and what undoes a revision,
//! merged three ways with what came later. Writing never follows a link, never writes
//! outside the tree's folder or into `.nib`, a `.git` or the library's own folder, and never
//! loses a copy's edits that are not committed: a restore keeps them beside the copy, and
//! nothing else writes over them ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::Value;
use nibkeep_std::{ON_DEVICE, PATH, ROOT_OF, TREE_ROOT};

use super::history::{READ_LIMIT, TreePoint};
use super::index::{Entry, Version};
use super::locate::{Blocked, Located, located, place_for_new};
use super::paths::{fits_this_system, is_reserved};
use super::secrecy::Rules;
use super::walk::{Found, FoundKind, hash_found, look, nanos, open_found, walk};
use super::{TREE_TYPES, TreeState, version_of};
use crate::NodeError;
use crate::library::Library;
use crate::library::checkouts::{
	copy_checked, put_in_place, remove_leftover, sync_folder_of, temporary_for,
};
use crate::library::entities::NewValue;

/// What writing a tree, or part of one, wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
	/// The paths written, in order.
	pub written: Vec<String>,
	/// The paths not written, with why: a name that this system cannot write, something in
	/// the way, or content that a purge took away.
	pub skipped: Vec<(String, String)>,
	/// The secret files not written, as this replica cannot read them: its secrets are
	/// locked, or not on this device
	/// ([technical design: secrets in trees](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#151-secrets-in-trees)).
	pub sealed: Vec<String>,
	/// The copies with edits not committed that a restore moved beside their paths before it
	/// wrote there, each path with where its edits are now, as Mercurial's revert keeps them:
	/// `NAME.orig`, or the first free of `NAME.orig-2` and on.
	pub aside: Vec<(String, String)>,
	/// Of a tree written as it is into a folder that held files: the paths where a copy held
	/// the tree's version already, which counts as written.
	pub found: Vec<String>,
	/// The paths where a copy held an older version of the tree's file, which counts as
	/// behind: an update writes the tree's version.
	pub older: Vec<String>,
	/// The paths where a copy held something else, which stays, as an edit of the tree's
	/// version: a commit records it, and a restore writes the tree's version.
	pub differing: Vec<String>,
	/// What the folder holds that the tree does not, left out what `.nibignore` files name:
	/// it stays, and the next commit takes it in.
	pub others: Vec<String>,
}

/// What removing files from a tree's copy did, as Git's rm does: the copies deleted, for the
/// next commit to record as deleted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopiesRemoved {
	/// The paths whose copies it deleted, or that were gone already, in order.
	pub removed: Vec<String>,
	/// The paths whose copies stay, with why: one that changed meanwhile, that cannot be
	/// reached, or that was never written here.
	pub skipped: Vec<(String, String)>,
}

/// What undoing a revision wrote into the copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reverted {
	/// The paths written: versions from before the revision, or merged with later ones.
	pub written: Vec<String>,
	/// The paths deleted: files and links that the revision added.
	pub deleted: Vec<String>,
	/// The moves undone, from where to where.
	pub moved: Vec<(String, String)>,
	/// The paths where what came later clashes with undoing the revision: text with Git's
	/// conflict markers, or another file with the version from before written beside it.
	pub conflicts: Vec<String>,
	/// The paths left as they are, with why, whose part of the revision is not undone.
	pub skipped: Vec<(String, String)>,
}

/// What is written at a path: a version, or bytes made here, such as a merge; of a secret
/// file, read with the secrets alone, and written for its owner alone.
pub(super) enum Content<'a> {
	Version(&'a Version),
	Bytes(&'a [u8], bool),
	Secret(&'a Version),
	SecretBytes(&'a [u8], bool),
}

impl<'a> Content<'a> {
	/// A version of a file, secret or not.
	pub(super) fn of(version: &'a Version, secret: bool) -> Self {
		if secret {
			Self::Secret(version)
		} else {
			Self::Version(version)
		}
	}

	/// Bytes made here of a file, secret or not, executable or not.
	pub(super) fn bytes(bytes: &'a [u8], executable: bool, secret: bool) -> Self {
		if secret {
			Self::SecretBytes(bytes, executable)
		} else {
			Self::Bytes(bytes, executable)
		}
	}
}

/// Where writing goes: the tree's folder, and the folders that no path of it reaches.
#[derive(Clone, Copy)]
pub(super) struct Place<'a> {
	pub(super) root: &'a Path,
	pub(super) apart: &'a [PathBuf],
}

/// How a copy that a folder held before a tree was written into it is taken in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kept {
	/// It holds the tree's version.
	Same,
	/// It holds an older version of the tree's file.
	Older,
	/// It holds something else.
	Differing,
}

/// What writing a tree into a folder that held files does at one of the tree's paths.
enum Taken {
	/// Nothing is there that a copy could be, and the tree's file is written if it can be.
	Nothing,
	/// A copy is taken in as it is, with its entry in the index.
	Copy(Kept, Entry),
	/// What is there stays, left out of commits, with why, until the person renames or
	/// deletes it: one that cannot be read, and so cannot be told from the tree's file, or one
	/// that would be secret by the tree's rules where the tree's file is not.
	Marked(String),
}

impl Library {
	/// Writes a tree into `folder` at a point of its history: as it is, which becomes this
	/// device's copy of it, with its root recorded; or at a revision, or as of a moment, into
	/// an empty or new folder, which is a picture of the past and no copy, never over one.
	///
	/// A copy goes into a folder that holds files too, and never over one of them: a file at
	/// a path of the tree that holds the tree's version counts as written; one that holds an
	/// older version of its file, whose content the store holds, as behind, which an update
	/// brings up to date; and one that holds anything else stays, as an edit of the tree's
	/// version. What the tree does not have stays too, for the next commit to take in
	/// ([`Written`]).
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotATree`] if `tree` is none, [`NodeError::FolderNotEmpty`] if
	/// a picture of the past is asked for in a folder that holds anything,
	/// [`NodeError::TreeHereAlready`] if the tree as it is is asked
	/// for and this device has a copy of it already, [`NodeError::TreeInTree`] or
	/// [`NodeError::NotATreeFolder`] if the folder is in a tree's, the library's or Git's,
	/// [`NodeError::OtherLibraryHere`] if it holds another library,
	/// [`NodeError::HoldsOtherCopies`] if a checkout of this device has its copy at a path of
	/// the tree there, and an error if writing or saving fails.
	pub fn write_tree(
		&mut self,
		space: SpaceId,
		tree: EntityId,
		folder: &Path,
		at: TreePoint,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Written, NodeError> {
		let tree = self.check_tree(space, tree)?;
		// Taken before the copy is looked for, so that two writes of the tree as it is never
		// both make one.
		let locked = if at == TreePoint::Now {
			self.check_not_revoked(space)?;
			let locked = self.lock_trees()?;
			if let Some(root) = self.roots_here(space)?.remove(&tree) {
				return Err(NodeError::TreeHereAlready(root));
			}
			Some(locked)
		} else {
			None
		};
		// The place is checked before anything is made there.
		self.check_new_place(folder)?;
		let held = holds_anything(folder)?;
		// A picture of the past goes only where nothing is, as it is no copy that could take
		// in what was there.
		if held && locked.is_none() {
			return Err(NodeError::FolderNotEmpty(folder.to_path_buf()));
		}
		if held {
			self.check_no_other_library(folder)?;
		}
		let root = self.checked_new_root(space, folder)?;
		let state = self.tree_state_at(space, tree, at)?;
		// Of an empty folder too, as a checkout whose copy was deleted is still recorded.
		self.check_no_checkout_at(&root, &state)?;
		let apart = self.apart();
		let place = Place {
			root: &root,
			apart: &apart,
		};
		let (mut written, entries, marked) = self.write_files(space, place, &state, held)?;
		if locked.is_some() {
			self.install_standard(space, &TREE_TYPES, wall_millis, rng)?;
			let (root_entity, operations) = self.new_entity(
				space,
				&[TREE_ROOT],
				vec![
					NewValue::new(ROOT_OF, Value::Reference(tree)),
					NewValue::new(ON_DEVICE, Value::Reference(self.info.device)),
					NewValue::new(PATH, Value::Text(root.to_string_lossy().into_owned())),
				],
				rng,
			)?;
			self.commit(space, operations, wall_millis, rng)?;
			let mut index = self.index()?;
			index.change(tree, &[], &entries)?;
			for (path, why) in &marked {
				index.mark(tree, path, why)?;
			}
			self.note_secrets(space, (tree, &index), &state, &entries)?;
			index.set_root(tree, root_entity, &root)?;
			index.know(tree)?;
			// The copy holds the parts of commits not yet whole, which updates then keep.
			let cut: Vec<EntityId> = self
				.cut_short(space, tree)?
				.into_iter()
				.map(|cut| cut.revision)
				.collect();
			index.take_parts(tree, &cut)?;
			if held {
				written.others = self.others_in(space, tree, &root, &state)?;
			}
		}
		Ok(written)
	}

	/// Writes the files of a tree's `state` into its folder, as [`Library::write_tree`] says,
	/// taking in what the folder held, if it `held` anything: what was written, the index's
	/// entries of the copies, and the paths to mark, with why.
	#[expect(
		clippy::type_complexity,
		reason = "three results that only write_tree takes"
	)]
	fn write_files(
		&self,
		space: SpaceId,
		place: Place<'_>,
		state: &TreeState,
		held: bool,
	) -> Result<(Written, Vec<(String, Entry)>, Vec<(String, String)>), NodeError> {
		let read_at = nanos(SystemTime::now()).unwrap_or(0);
		let mut written = Written {
			sealed: state.sealed.keys().cloned().collect(),
			..Written::default()
		};
		let mut entries = Vec::new();
		let mut marked = Vec::new();
		// What makes a file secret in the folder as it was: the built-in names, and its own
		// `.nibsecret` files.
		let rules = if held {
			Rules::of_copy(&walk(place.root, &BTreeSet::new(), place.apart).found)
		} else {
			Rules::built_in()
		};
		for (path, current) in &state.by_path {
			let Some(version) = &current.version else {
				let why = "its content was purged".to_owned();
				written.skipped.push((path.clone(), why));
				continue;
			};
			let taken = if held {
				let secret = state.is_secret(current.entity);
				self.take_found(space, place, (path, current.entity, version), read_at)?
					.unless_secret(!secret && rules.is_secret(path))
			} else {
				Taken::Nothing
			};
			match taken {
				Taken::Nothing => {}
				Taken::Copy(kept, entry) => {
					match kept {
						Kept::Same => written.found.push(path.clone()),
						Kept::Older => written.older.push(path.clone()),
						Kept::Differing => written.differing.push(path.clone()),
					}
					entries.push((path.clone(), entry));
					continue;
				}
				Taken::Marked(why) => {
					written.skipped.push((path.clone(), why.clone()));
					marked.push((path.clone(), why));
					continue;
				}
			}
			let content = Content::of(version, state.is_secret(current.entity));
			match self.write_new(space, place, path, &content) {
				Ok(found) => {
					written.written.push(path.clone());
					entries.push((
						path.clone(),
						Entry {
							entity: current.entity,
							version: version.clone(),
							size: found.size,
							modified: found.modified,
							read_at,
						},
					));
				}
				Err(why) => written.skipped.push((path.clone(), why)),
			}
		}
		Ok((written, entries, marked))
	}

	/// What a folder that held files before a tree was written into it holds at the tree's
	/// `path`, which the copy takes in as it is, with its entry in the index: the tree's
	/// version, as written; an older version of its file, as behind; or anything else, as an
	/// edit of the tree's version, whose entry never says the copy is unchanged, so that it
	/// is always read. A file that cannot be read is marked, and stays out of commits.
	/// [`Taken::Nothing`] where nothing is there, or nothing that a write would take for a
	/// copy, such as a folder, which writing then names as in the way.
	fn take_found(
		&self,
		space: SpaceId,
		place: Place<'_>,
		(path, entity, version): (&str, EntityId, &Version),
		read_at: i64,
	) -> Result<Taken, NodeError> {
		let Located::Here(on_disk) = located(place.root, path, place.apart) else {
			return Ok(Taken::Nothing);
		};
		let Ok(found) = look(&on_disk) else {
			return Ok(Taken::Nothing);
		};
		let copy = match &found.kind {
			FoundKind::File => {
				let Ok(content) = hash_found(&found) else {
					return Ok(Taken::Marked(UNREADABLE.to_owned()));
				};
				let executable = found.executable.unwrap_or(matches!(
					version,
					Version::File {
						executable: true,
						..
					}
				));
				Version::File {
					content,
					executable,
				}
			}
			FoundKind::Link(target) => Version::Link {
				target: target.clone(),
			},
		};
		let entry = |version: Version, modified: Option<i64>| Entry {
			entity,
			version,
			size: found.size,
			modified,
			read_at,
		};
		if &copy == version {
			return Ok(Taken::Copy(Kept::Same, entry(copy, found.modified)));
		}
		if !same_content(&copy, version)
			&& let Some(past) = self.past_version(space, entity, &copy)?
		{
			return Ok(Taken::Copy(Kept::Older, entry(past, found.modified)));
		}
		Ok(Taken::Copy(Kept::Differing, entry(version.clone(), None)))
	}

	/// Refuses a folder to write a tree into where a checkout of this device, of any space,
	/// has its copy at a path of the tree's `state`: the checkout and the tree would both
	/// keep that file, and an update or a sync write over what the other wrote.
	fn check_no_checkout_at(&self, root: &Path, state: &TreeState) -> Result<(), NodeError> {
		for &space in self.spaces.keys() {
			for checkout in self.checkouts(space)? {
				let at_a_path = checkout
					.path
					.strip_prefix(root)
					.ok()
					.and_then(|relative| super::paths::tree_path_of(relative).ok())
					.is_some_and(|path| {
						state.by_path.contains_key(&path) || state.sealed.contains_key(&path)
					});
				if at_a_path {
					return Err(NodeError::HoldsOtherCopies(root.display().to_string()));
				}
			}
		}
		Ok(())
	}

	/// Refuses a folder to write a tree into that holds a library other than this one, whose
	/// `.nib` folder would take over the commands there.
	fn check_no_other_library(&self, folder: &Path) -> Result<(), NodeError> {
		let data = folder.join(crate::pointer::LIBRARY_DATA);
		if !fs::symlink_metadata(&data).is_ok_and(|metadata| metadata.is_dir()) {
			return Ok(());
		}
		let own = fs::canonicalize(&data)
			.map(crate::library::files::plain)
			.is_ok_and(|data| self.apart().contains(&data));
		if own {
			return Ok(());
		}
		Err(NodeError::OtherLibraryHere(folder.to_path_buf()))
	}

	/// What a tree's folder holds that the tree does not, as a commit would find it: the
	/// files and links of no path of the tree, and the Git repositories it does not record.
	fn others_in(
		&self,
		space: SpaceId,
		tree: EntityId,
		root: &Path,
		state: &TreeState,
	) -> Result<Vec<String>, NodeError> {
		let index = self.index()?;
		let left_out = self.left_out(space, tree, root, &index)?;
		let walked = walk(root, &left_out, &self.apart());
		let recorded: BTreeSet<String> = self
			.recorded_repositories(space, tree)?
			.into_iter()
			.map(|repository| repository.path)
			.collect();
		let mut others: Vec<String> = walked
			.found
			.into_keys()
			.filter(|path| !state.by_path.contains_key(path) && !state.sealed.contains_key(path))
			.chain(
				walked
					.repositories
					.into_iter()
					.filter(|path| !recorded.contains(path)),
			)
			.collect();
		others.sort();
		Ok(others)
	}

	/// Writes into the copy of a tree on this device the file or link at `path`, or what is
	/// in the folder at `path`, as it was at a point of the tree's history; as it is, a file
	/// deleted from the tree comes back too. It records nothing: committing it does, after
	/// the person checked it. A copy with edits that are not committed, its executable bit
	/// too, is moved beside its path first, and marked, so that no commit takes it in
	/// ([`Written::aside`]).
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotInTree`] if the tree held nothing there then,
	/// [`NodeError::EditedCopy`] if a copy to write over cannot be read, and an error if
	/// the index cannot be read or written.
	pub fn restore_in_tree(
		&self,
		space: SpaceId,
		tree: EntityId,
		path: &str,
		at: TreePoint,
	) -> Result<Written, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let index = self.index()?;
		let entries = index.entries(tree)?;
		let now = self.tree_state(space, tree)?;
		let then = self.tree_state_at(space, tree, at)?;
		let apart = self.apart();
		let place = Place {
			root: &root,
			apart: &apart,
		};
		let under = |candidate: &str| {
			path.is_empty() || candidate == path || candidate.starts_with(&format!("{path}/"))
		};
		let take = |(candidate, current): (&String, &super::Current)| {
			let secret = then.is_secret(current.entity);
			(candidate.clone(), current.version.clone(), secret)
		};
		let mut chosen: Vec<(String, Option<Version>, bool)> = then
			.by_path
			.iter()
			.filter(|(candidate, _)| under(candidate))
			.map(take)
			.collect();
		if chosen.is_empty() && at == TreePoint::Now {
			chosen = then
				.deleted
				.iter()
				.filter(|(candidate, _)| under(candidate))
				.map(take)
				.collect();
		}
		let sealed: Vec<String> = then
			.sealed
			.keys()
			.filter(|candidate| under(candidate))
			.cloned()
			.collect();
		if chosen.is_empty() && sealed.is_empty() {
			return Err(NodeError::NotInTree(path.to_owned()));
		}
		// Every copy is looked at before anything is written.
		let mut plan = Vec::new();
		let mut written = Written {
			sealed,
			..Written::default()
		};
		for (candidate, version, secret) in chosen {
			let Some(version) = version else {
				let why = "its content was purged".to_owned();
				written.skipped.push((candidate, why));
				continue;
			};
			if secret && !self.secrets_unlocked(space) {
				written.sealed.push(candidate);
				continue;
			}
			match located(&root, &candidate, &apart) {
				Located::Absent => plan.push((candidate, version, secret, None)),
				Located::Unknown(why) => written.skipped.push((candidate, why)),
				Located::Here(on_disk) => {
					let found =
						look(&on_disk).map_err(|_| NodeError::EditedCopy(candidate.clone()))?;
					let holds = copy_version(&found);
					if holds.as_ref() == Some(&version) {
						continue;
					}
					let recorded = recorded_version(&entries, &now, &candidate);
					let unedited = holds
						.as_ref()
						.zip(recorded)
						.is_some_and(|(holds, recorded)| same_version(&found, holds, recorded));
					plan.push((candidate, version, secret, Some((found, !unedited))));
				}
			}
		}
		written.sealed.sort();
		for (candidate, version, secret, there) in plan {
			let content = Content::of(&version, secret);
			let done = match &there {
				None => self.write_new(space, place, &candidate, &content).map(drop),
				Some((found, false)) => self.write_over(space, found, &content),
				Some((found, true)) => {
					// Never beside it at a path of the tree's, then or now.
					let held = |name: &str| {
						now.by_path.contains_key(name)
							|| then.by_path.contains_key(name)
							|| entries.contains_key(name)
					};
					let edited = (candidate.as_str(), found);
					let at = (&index, tree, place);
					self.write_keeping_edits(space, at, (edited, &held), &content)?
						.map(|beside| written.aside.push((candidate.clone(), beside)))
				}
			};
			match done {
				Ok(()) => written.written.push(candidate),
				Err(why) => written.skipped.push((candidate, why)),
			}
		}
		Ok(written)
	}

	/// Writes `content` at `path`, whose copy `found` has edits not committed, once the copy
	/// is moved beside it ([`set_aside`]); where writing fails, the copy goes back. Returns
	/// where the edits are, or why nothing was written.
	fn write_keeping_edits(
		&self,
		space: SpaceId,
		(index, tree, place): (&super::index::Index, EntityId, Place<'_>),
		((path, found), held): ((&str, &Found), Held<'_>),
		content: &Content<'_>,
	) -> Result<Result<String, String>, NodeError> {
		let beside = match set_aside(index, (tree, place), (path, held), found)? {
			Ok(beside) => beside,
			Err(why) => return Ok(Err(why)),
		};
		match self.write_new(space, place, path, content) {
			Ok(_) => Ok(Ok(beside)),
			Err(why) => {
				bring_back(index, (tree, place), (path, &beside), found)?;
				Ok(Err(why))
			}
		}
	}

	/// Deletes from the copy of a tree on this device the file or link at each of `paths`,
	/// or the tree's files and links in the folder there, the whole copy for an empty path,
	/// and the folders they leave empty, as Git's rm does: what the next commit records as
	/// deleted. It records nothing. A copy with edits that are not committed stays, unless
	/// `force`, and so does whatever the tree does not hold; every copy is looked at before
	/// any is deleted.
	///
	/// # Errors
	///
	/// Returns [`NodeError::UnsafePath`] if a path leaves the tree, [`NodeError::NotInTree`]
	/// if the tree holds nothing at one, [`NodeError::EditedCopy`] if a copy has edits not
	/// committed, or cannot be read, and an error if the index cannot be read.
	pub fn remove_in_tree(
		&self,
		space: SpaceId,
		tree: EntityId,
		paths: &[String],
		force: bool,
	) -> Result<CopiesRemoved, NodeError> {
		if let Some(path) = paths
			.iter()
			.find(|path| !path.is_empty() && !super::is_safe_tree_path(path))
		{
			return Err(NodeError::UnsafePath(path.clone()));
		}
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let entries = self.index()?.entries(tree)?;
		let now = self.tree_state(space, tree)?;
		let apart = self.apart();
		let under = |path: &str, candidate: &str| {
			path.is_empty() || candidate == path || candidate.starts_with(&format!("{path}/"))
		};
		if let Some(path) = paths
			.iter()
			.find(|path| !now.by_path.keys().any(|candidate| under(path, candidate)))
		{
			return Err(NodeError::NotInTree(path.clone()));
		}
		let chosen: Vec<&String> = now
			.by_path
			.keys()
			.filter(|candidate| paths.iter().any(|path| under(path, candidate)))
			.collect();
		let mut removed = CopiesRemoved::default();
		let mut plan = Vec::new();
		for candidate in chosen {
			match located(&root, candidate, &apart) {
				Located::Absent if entries.contains_key(candidate) => {
					removed.removed.push(candidate.clone());
				}
				Located::Absent => {
					let why = "its copy was never written here".to_owned();
					removed.skipped.push((candidate.clone(), why));
				}
				Located::Unknown(why) => removed.skipped.push((candidate.clone(), why)),
				Located::Here(on_disk) => {
					let found =
						look(&on_disk).map_err(|_| NodeError::EditedCopy(candidate.clone()))?;
					let unedited = copy_version(&found)
						.zip(recorded_version(&entries, &now, candidate))
						.is_some_and(|(holds, recorded)| same_version(&found, &holds, recorded));
					if !unedited && !force {
						return Err(NodeError::EditedCopy(candidate.clone()));
					}
					plan.push((candidate.clone(), found));
				}
			}
		}
		for (candidate, found) in plan {
			// Deleted only if it is still the copy read, as a save made meanwhile replaces it.
			if !still_the_copy(&found) {
				let why = "it changed meanwhile".to_owned();
				removed.skipped.push((candidate, why));
				continue;
			}
			match fs::remove_file(&found.on_disk) {
				Ok(()) => {
					sync_folder_of(&found.on_disk);
					remove_emptied(&root, &found.on_disk);
					removed.removed.push(candidate);
				}
				Err(error) => removed.skipped.push((candidate, error.to_string())),
			}
		}
		removed.removed.sort();
		Ok(removed)
	}

	/// Writes into the copy of a tree on this device what undoes a revision, and only that,
	/// as `git revert --no-commit` does: each file it changed goes back to its version
	/// before it, merged three ways with what later revisions changed; files it moved go
	/// back, files it deleted come back, and files it added go. It records nothing. What it
	/// cannot undo, it lists with why.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoRevision`] if the revision is not the tree's,
	/// [`NodeError::EditedCopy`] if a copy it would change has edits not committed,
	/// [`NodeError::TreeStale`] if a copy it would change is behind the tree, which an update
	/// brings first, and an error if the space cannot be read or writing fails.
	pub fn revert_in_tree(
		&self,
		space: SpaceId,
		tree: EntityId,
		revision: EntityId,
	) -> Result<Reverted, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let index = self.index()?;
		let entries = index.entries(tree)?;
		let changes = self.revision_changes(space, tree, revision)?;
		let Some(&last) = changes.last() else {
			return Err(NodeError::NoRevision(revision.to_string()));
		};
		let parts: BTreeSet<_> = changes.iter().copied().collect();
		let (after, before) = self.tree_states_of_changes(space, tree, last, &parts)?;
		let now = self.tree_state(space, tree)?;
		let apart = self.apart();
		let place = Place {
			root: &root,
			apart: &apart,
		};
		let mut reverted = Reverted::default();
		let plan = undoing(&before, &after, &now, &mut reverted);
		// Every copy it changes holds what the tree holds now, edits committed, executable
		// bits too: it never writes over edits, as `git revert` refuses to, nor undoes a
		// revision on a copy that is behind, which would leave later revisions out.
		let mut behind = Vec::new();
		for path in plan.touched() {
			let Located::Here(on_disk) = located(&root, path, &apart) else {
				continue;
			};
			let Ok(found) = look(&on_disk) else {
				return Err(NodeError::EditedCopy(path.to_owned()));
			};
			let holds = copy_version(&found);
			let committed = holds
				.as_ref()
				.zip(entries.get(path).map(|entry| &entry.version))
				.is_some_and(|(holds, entry)| same_version(&found, holds, entry));
			let current = now
				.by_path
				.get(path)
				.and_then(|current| current.version.as_ref());
			let up_to_date = holds
				.as_ref()
				.zip(current)
				.is_some_and(|(holds, current)| same_version(&found, holds, current));
			if !up_to_date && committed {
				behind.push(path.to_owned());
			} else if !up_to_date {
				return Err(NodeError::EditedCopy(path.to_owned()));
			}
		}
		if !behind.is_empty() {
			return Err(NodeError::TreeStale(behind));
		}
		// First what is in the way goes, then what moved goes back, then what was deleted
		// comes back, then edits are undone: the order in which no step meets another's.
		for (path, added) in &plan.additions {
			undo_addition(place, (path, added.as_ref()), &mut reverted);
		}
		let at = undo_moves(place, &plan.moves, &mut reverted);
		for (path, version) in &plan.deletions {
			let Some(version) = version else {
				let why = "its content was purged".to_owned();
				reverted.skipped.push((path.clone(), why));
				continue;
			};
			let content = Content::of(version, plan.secret.contains(path));
			match self.write_new(space, place, path, &content) {
				Ok(_) => reverted.written.push(path.clone()),
				Err(why) => reverted.skipped.push((path.clone(), why)),
			}
		}
		let short = &revision.to_string()[..8];
		for (path, versions) in &plan.edits {
			let secret = plan.secret.contains(path);
			let path = at.get(path).unwrap_or(path);
			if let Some(beside) = self.undo_edit(
				space,
				(place, short),
				(path, secret),
				versions,
				&mut reverted,
			) {
				index.mark(
					tree,
					&beside,
					"NibKeep wrote it, the version from before a reverted revision",
				)?;
			}
		}
		Ok(reverted)
	}

	/// Refuses a folder to write a tree into, before anything is made there: in a folder
	/// that some system reads as `.git` or `.nib`, in the library's own, or in or around a
	/// tree's folder on this device, of any space.
	fn check_new_place(&self, folder: &Path) -> Result<(), NodeError> {
		// Where it is, as the file system resolves the part of it that is there.
		let mut existing = folder;
		let mut rest = Vec::new();
		let resolved = loop {
			if let Ok(resolved) = fs::canonicalize(existing) {
				break crate::library::files::plain(resolved);
			}
			let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
				return Err(NodeError::NotATreeFolder(folder.to_path_buf()));
			};
			rest.push(name);
			existing = parent;
		};
		let mut full = resolved;
		full.extend(rest.iter().rev());
		let reserved = full.components().any(|component| match component {
			std::path::Component::Normal(name) => is_reserved(&name.to_string_lossy()),
			_ => false,
		});
		if reserved || self.apart().iter().any(|apart| full.starts_with(apart)) {
			return Err(NodeError::NotATreeFolder(full));
		}
		for &space in self.spaces.keys() {
			for other in self.roots_here(space)?.values() {
				if full.starts_with(other) || other.starts_with(&full) {
					return Err(NodeError::TreeInTree(full));
				}
			}
		}
		Ok(())
	}
}

/// What undoing a revision does, decided before anything is written: the copies of what it
/// added to delete, with the versions it added; the moves to undo; the files it deleted to
/// write back; and the edits to undo, each with its version before and after the revision,
/// by the path where the copy is now.
#[derive(Debug, Default)]
struct Undoing {
	additions: Vec<(String, Option<Version>)>,
	moves: Vec<(String, String)>,
	deletions: Vec<(String, Option<Version>)>,
	edits: Vec<(String, (Version, Version))>,
	/// The paths of deletions and edits that are of secret files.
	secret: BTreeSet<String>,
}

impl Undoing {
	/// The paths whose copies it changes, as they are now.
	fn touched(&self) -> impl Iterator<Item = &String> {
		self.additions
			.iter()
			.map(|(path, _)| path)
			.chain(self.moves.iter().map(|(from, _)| from))
			.chain(self.edits.iter().map(|(path, _)| path))
	}
}

/// What undoing a revision does, from the tree before it, after it, and now; a file whose
/// path another holds now too, as when two devices added one path, is listed in `reverted`
/// as skipped, as which copy is its own is not known.
fn undoing(
	before: &TreeState,
	after: &TreeState,
	now: &TreeState,
	reverted: &mut Reverted,
) -> Undoing {
	let mut plan = Undoing::default();
	let entities: BTreeSet<EntityId> = before
		.by_entity
		.keys()
		.chain(after.by_entity.keys())
		.copied()
		.collect();
	for entity in entities {
		let is = version_of(now, entity);
		if let Some((path, _)) = &is
			&& now
				.by_path
				.get(path)
				.is_some_and(|current| current.entity != entity)
		{
			let why = "another file competes for its path; update the copy first".to_owned();
			reverted.skipped.push((path.clone(), why));
			continue;
		}
		match (version_of(before, entity), version_of(after, entity)) {
			(None, Some((_, added))) => {
				if let Some((path, _)) = is {
					plan.additions.push((path, added));
				}
			}
			(Some((path, version)), None) => {
				if is.is_none() {
					if before.is_secret(entity) {
						plan.secret.insert(path.clone());
					}
					plan.deletions.push((path, version));
				}
			}
			(Some((was_path, was)), Some((became_path, became))) => {
				if was_path == became_path && was == became {
					// The revision did not change it.
					continue;
				}
				let Some((path, _)) = is else {
					let why = "it was deleted since".to_owned();
					reverted.skipped.push((became_path, why));
					continue;
				};
				if was_path != became_path && path == became_path {
					plan.moves.push((path.clone(), was_path));
				}
				if let (Some(was), Some(became)) = (was, became)
					&& was != became
				{
					if after.is_secret(entity) {
						plan.secret.insert(path.clone());
					}
					plan.edits.push((path, (was, became)));
				}
			}
			(None, None) => {}
		}
	}
	plan
}

/// Undoes a revision's addition of a file: deletes its copy, if it is as the revision left
/// it; one that later revisions changed stays, as a conflict.
fn undo_addition(
	place: Place<'_>,
	(path, added): (&String, Option<&Version>),
	reverted: &mut Reverted,
) {
	let on_disk = match located(place.root, path, place.apart) {
		Located::Here(on_disk) => on_disk,
		Located::Absent => return,
		Located::Unknown(why) => {
			reverted.skipped.push((path.clone(), why));
			return;
		}
	};
	let unchanged = look(&on_disk).is_ok_and(|found| {
		copy_version(&found)
			.zip(added)
			.is_some_and(|(holds, added)| same_version(&found, &holds, added))
	});
	if !unchanged {
		reverted.conflicts.push(path.clone());
		return;
	}
	match fs::remove_file(&on_disk) {
		Ok(()) => {
			sync_folder_of(&on_disk);
			reverted.deleted.push(path.clone());
		}
		Err(error) => reverted.skipped.push((path.clone(), error.to_string())),
	}
}

/// Undoes moves, all at once, as [`undo_moves_at`] does them.
fn undo_moves(
	place: Place<'_>,
	moves: &[(String, String)],
	reverted: &mut Reverted,
) -> BTreeMap<String, String> {
	let at = undo_moves_at(place, moves, &mut reverted.skipped);
	reverted
		.moved
		.extend(at.iter().map(|(from, to)| (from.clone(), to.clone())));
	at
}

/// Moves copies, all at once: each goes to a name of its own beside it first, then to where
/// it goes, so that swaps, chains and changes of case alone never meet each other. Returns
/// where each copy went, by where it was; what could not move is in `skipped`, with why.
/// Where a move puts a copy aside while the others are moved: a temporary name of its own,
/// never one that writing a file at its path uses and clears.
pub(super) fn aside_for(source: &Path) -> PathBuf {
	let name = source.file_name().map(std::ffi::OsStr::as_encoded_bytes);
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"moved aside\0");
	hasher.update(name.unwrap_or_default());
	let digest = hasher.finalize().to_hex();
	source.with_file_name(format!(".nib-tree-{}", &digest[..16]))
}

/// Puts back the copies that a move cut short left aside, where their paths are free.
pub(super) fn bring_back_aside<'a>(place: Place<'_>, paths: impl Iterator<Item = &'a String>) {
	for path in paths {
		if located(place.root, path, place.apart) != Located::Absent {
			continue;
		}
		let source = place.root.join(path);
		let aside = aside_for(&source);
		if fs::symlink_metadata(&aside).is_ok()
			&& fs::symlink_metadata(&source).is_err()
			&& fs::rename(&aside, &source).is_ok()
		{
			sync_folder_of(&source);
		}
	}
}

/// Removes the folders above `deleted`, below the tree's folder `root`, that its deletion
/// left empty, from the innermost, each saved into the folder that held it.
fn remove_emptied(root: &Path, deleted: &Path) {
	for folder in deleted.ancestors().skip(1) {
		if folder == root || !folder.starts_with(root) || fs::remove_dir(folder).is_err() {
			break;
		}
		sync_folder_of(folder);
	}
}

/// Whether the tree holds a path, then or now, which no copy set aside may take.
type Held<'a> = &'a dyn Fn(&str) -> bool;

/// Whether the copy that `found` is is still as it was read: the same file, of the same size
/// and time, as a save made meanwhile replaces it.
pub(super) fn still_the_copy(found: &Found) -> bool {
	look(&found.on_disk).is_ok_and(|now| {
		now.identity == found.identity && now.size == found.size && now.modified == found.modified
	})
}

/// Moves the copy that `found` is, with edits not committed, beside its path `path`, before
/// a restore writes there: as `NAME.orig`, or the first free of `NAME.orig-2` to
/// `NAME.orig-99` that is no path `held` by the tree, marked first, so that no commit takes
/// it in, whatever stops the restore; and only if it is still the copy read. Returns where it
/// is now, or why it stays.
fn set_aside(
	index: &super::index::Index,
	(tree, place): (EntityId, Place<'_>),
	(path, held): (&str, Held<'_>),
	found: &Found,
) -> Result<Result<String, String>, NodeError> {
	let Some(beside) = (1..=99)
		.map(|number| match number {
			1 => format!("{path}.orig"),
			_ => format!("{path}.orig-{number}"),
		})
		.find(|name| !held(name) && located(place.root, name, place.apart) == Located::Absent)
	else {
		return Ok(Err(format!(
			"{path}.orig and the names after it are taken, so its edits stay"
		)));
	};
	index.mark(
		tree,
		&beside,
		"NibKeep wrote it, the edits a restore took from the copy",
	)?;
	let moved = if still_the_copy(found) {
		move_into_place(place, &found.on_disk, &beside)
	} else {
		Err("it changed meanwhile".to_owned())
	};
	if let Err(why) = moved {
		index.unmark(tree, std::slice::from_ref(&beside))?;
		return Ok(Err(why));
	}
	Ok(Ok(beside))
}

/// Puts back at `path` the edits that [`set_aside`] moved to `beside`, where nothing came
/// meanwhile, once writing the version there failed, and takes the mark away.
fn bring_back(
	index: &super::index::Index,
	(tree, place): (EntityId, Place<'_>),
	(path, beside): (&str, &str),
	found: &Found,
) -> Result<(), NodeError> {
	if located(place.root, path, place.apart) == Located::Absent
		&& fs::rename(place.root.join(beside), &found.on_disk).is_ok()
	{
		sync_folder_of(&found.on_disk);
		index.unmark(tree, &[beside.to_owned()])?;
	}
	Ok(())
}

pub(super) fn undo_moves_at(
	place: Place<'_>,
	moves: &[(String, String)],
	skipped: &mut Vec<(String, String)>,
) -> BTreeMap<String, String> {
	let mut aside = Vec::new();
	for (from, to) in moves {
		let source = match located(place.root, from, place.apart) {
			Located::Here(source) => source,
			Located::Absent => {
				skipped.push((from.clone(), "its copy is not here".to_owned()));
				continue;
			}
			Located::Unknown(why) => {
				skipped.push((from.clone(), why));
				continue;
			}
		};
		let temporary = aside_for(&source);
		if fs::symlink_metadata(&temporary).is_ok() || fs::rename(&source, &temporary).is_err() {
			skipped.push((from.clone(), "it could not be moved".to_owned()));
			continue;
		}
		aside.push((from, to, source, temporary));
	}
	let mut at = BTreeMap::new();
	for (from, to, source, temporary) in aside {
		match move_into_place(place, &temporary, to) {
			Ok(()) => {
				// Saved before the index records the move, which a power cut must not keep
				// without it.
				sync_folder_of(&source);
				at.insert(from.clone(), to.clone());
			}
			Err(why) => {
				// Back where it was, as nothing took its place meanwhile.
				let _ = fs::rename(&temporary, &source);
				skipped.push((from.clone(), why));
			}
		}
	}
	at
}

impl Library {
	/// Undoes a revision's edit of the file whose copy is at `path`: writes its version from
	/// before, or that version merged three ways with what later revisions changed. Returns
	/// the path it wrote the version from before at, beside the copy, if the merge could not.
	fn undo_edit(
		&self,
		space: SpaceId,
		(place, short): (Place<'_>, &str),
		(path, secret): (&str, bool),
		(before, after): &(Version, Version),
		reverted: &mut Reverted,
	) -> Option<String> {
		if secret && !self.secrets_unlocked(space) {
			let why = "a secret file, which is written only with the secrets unlocked".to_owned();
			reverted.skipped.push((path.to_owned(), why));
			return None;
		}
		let on_disk = match located(place.root, path, place.apart) {
			Located::Here(on_disk) => on_disk,
			Located::Absent => {
				let why = "its copy is not here".to_owned();
				reverted.skipped.push((path.to_owned(), why));
				return None;
			}
			Located::Unknown(why) => {
				reverted.skipped.push((path.to_owned(), why));
				return None;
			}
		};
		let found = match look(&on_disk) {
			Ok(found) => found,
			Err(why) => {
				reverted.skipped.push((path.to_owned(), why));
				return None;
			}
		};
		match self.merge_back(space, &found, (before, after), secret) {
			Ok(Merged::Unchanged) => {}
			Ok(Merged::Clean(content)) => {
				match self.write_over(space, &found, &content.as_content(secret)) {
					Ok(()) => reverted.written.push(path.to_owned()),
					Err(why) => reverted.skipped.push((path.to_owned(), why)),
				}
			}
			Ok(Merged::Clashed(content)) => {
				match self.write_over(space, &found, &content.as_content(secret)) {
					Ok(()) => reverted.conflicts.push(path.to_owned()),
					Err(why) => reverted.skipped.push((path.to_owned(), why)),
				}
			}
			Ok(Merged::Beside) => {
				let beside = format!("{path}.before-{short}");
				match self.write_new(space, place, &beside, &Content::of(before, secret)) {
					Ok(_) => {
						reverted.conflicts.push(path.to_owned());
						return Some(beside);
					}
					Err(why) => reverted.skipped.push((path.to_owned(), why)),
				}
			}
			Err(why) => reverted.skipped.push((path.to_owned(), why)),
		}
		None
	}

	/// What undoing an edit makes of a copy: the version from before, if the copy holds what
	/// the revision made; or else that version merged three ways with what the copy holds,
	/// byte by byte, so that text in any encoding stays as it is. The executable bit merges
	/// three ways too: what later revisions set stays.
	fn merge_back(
		&self,
		space: SpaceId,
		found: &Found,
		(before, after): (&Version, &Version),
		secret: bool,
	) -> Result<Merged, String> {
		let holds = copy_version(found).ok_or("it cannot be read")?;
		let (
			Version::File {
				content: was,
				executable: was_executable,
			},
			Version::File {
				content: became,
				executable: became_executable,
			},
		) = (before, after)
		else {
			// Links: the target from before, if the copy holds the revision's.
			return Ok(if holds == *before {
				Merged::Unchanged
			} else if same_content(&holds, after) {
				Merged::Clean(Owned::Version(before.clone()))
			} else {
				Merged::Beside
			});
		};
		let holds_executable = found.executable.unwrap_or(*became_executable);
		let executable = if holds_executable == *became_executable {
			*was_executable
		} else {
			holds_executable
		};
		let wanted = Version::File {
			content: *was,
			executable,
		};
		if holds == wanted {
			return Ok(Merged::Unchanged);
		}
		if same_content(&holds, after) {
			return Ok(Merged::Clean(Owned::Version(wanted)));
		}
		if found.kind != FoundKind::File {
			return Ok(Merged::Beside);
		}
		let was_bytes = self.version_bytes(space, was, secret).ok().flatten();
		let became_bytes = self.version_bytes(space, became, secret).ok().flatten();
		let mut ours = Vec::new();
		let read = open_found(found)
			.and_then(|file| {
				io::Read::read_to_end(&mut io::Read::take(file, READ_LIMIT + 1), &mut ours)
			})
			.map_err(|error| error.to_string())?;
		let (Some(was_bytes), Some(became_bytes), true) =
			(was_bytes, became_bytes, read as u64 <= READ_LIMIT)
		else {
			// Too large to merge, or a version this device does not hold.
			return Ok(Merged::Beside);
		};
		let text = [&was_bytes, &became_bytes, &ours]
			.iter()
			.all(|bytes| super::diff::is_text(bytes));
		if !text {
			return Ok(Merged::Beside);
		}
		Ok(match diffy::merge_bytes(&became_bytes, &ours, &was_bytes) {
			Ok(merged) => Merged::Clean(Owned::Bytes(merged, executable)),
			Err(clashed) => Merged::Clashed(Owned::Bytes(clashed, executable)),
		})
	}

	/// Writes `content` at the path `path` of the tree at the place's folder, where nothing
	/// is: never through a link, outside the folder, or into `.nib`, a `.git` or the
	/// library's own folder. Returns what is there then, or why nothing was written.
	pub(super) fn write_new(
		&self,
		space: SpaceId,
		place: Place<'_>,
		path: &str,
		content: &Content<'_>,
	) -> Result<Found, String> {
		if !path.split('/').all(fits_this_system) {
			return Err("this system cannot write its name".to_owned());
		}
		let blocked = |why| match why {
			Blocked::Taken => "something is in its place".to_owned(),
			Blocked::Unsafe => "its way goes through a link, or into the library".to_owned(),
		};
		let target = place_for_new(place.root, path, place.apart).map_err(blocked)?;
		if let Some(parent) = target.parent() {
			make_folders(parent).map_err(|error| error.to_string())?;
			// What made the folders may have raced a link into them.
			if place_for_new(place.root, path, place.apart).as_ref() != Ok(&target) {
				return Err("its way goes through a link".to_owned());
			}
		}
		let temporary = temporary_for(&target, "tree");
		remove_leftover(&temporary);
		let made = self
			.write_temporary(space, content, &temporary)
			.and_then(|()| put_in_place(&temporary, &target).map_err(|error| error.to_string()));
		if made.is_err() {
			remove_leftover(&temporary);
		}
		made?;
		look(&target)
	}

	/// Writes `content` over the copy that `found` is, if it is still that copy, as it was
	/// read: written beside it first, then put in its place at once, so that the copy stays
	/// as it is if writing fails.
	pub(super) fn write_over(
		&self,
		space: SpaceId,
		found: &Found,
		content: &Content<'_>,
	) -> Result<(), String> {
		let target = &found.on_disk;
		let temporary = temporary_for(target, "tree");
		remove_leftover(&temporary);
		let placed = self
			.write_temporary(space, content, &temporary)
			.and_then(|()| {
				if !still_the_copy(found) {
					return Err("it changed meanwhile".to_owned());
				}
				fs::rename(&temporary, target).map_err(|error| error.to_string())
			});
		if placed.is_err() {
			remove_leftover(&temporary);
		} else {
			sync_folder_of(target);
		}
		placed
	}

	/// Writes `content` at `temporary`, which nothing is at: a link, or a file saved to the
	/// disk, executable as it says.
	fn write_temporary(
		&self,
		space: SpaceId,
		content: &Content<'_>,
		temporary: &Path,
	) -> Result<(), String> {
		match content {
			Content::Version(Version::Link { target })
			| Content::Secret(Version::Link { target }) => make_link(target, temporary),
			Content::Version(Version::File {
				content,
				executable,
			}) => self
				.blob_into(space, content, temporary)
				.and_then(|()| set_executable(temporary, *executable)),
			Content::Secret(Version::File {
				content,
				executable,
			}) => self
				.secret_blob_into(space, content, temporary)
				.and_then(|()| set_executable(temporary, *executable)),
			Content::Bytes(bytes, executable) => bytes_into(bytes, temporary, false)
				.and_then(|()| set_executable(temporary, *executable)),
			Content::SecretBytes(bytes, executable) => bytes_into(bytes, temporary, true)
				.and_then(|()| set_executable(temporary, *executable)),
		}
	}

	/// Writes the content of a secret file with the hash `content`, from the store, into the
	/// new file `temporary`, which only its owner may read on Unix, checked against its hash,
	/// and saves it to the disk: only with the secrets unlocked, and only content sealed for
	/// the secrets.
	fn secret_blob_into(
		&self,
		space: SpaceId,
		content: &nibkeep_base::Hash,
		temporary: &Path,
	) -> Result<(), String> {
		if !self.secrets_unlocked(space) {
			return Err(
				"a secret file, which is written only with the secrets unlocked".to_owned(),
			);
		}
		let open = self.space(space).map_err(|error| error.to_string())?;
		let gone = || "this device holds no copy of its content".to_owned();
		if !open.stored.has_secret_blob_key(content) || !open.stored.holds_blob(content) {
			return Err(gone());
		}
		let mut reader = open.stored.open_blob(content).map_err(|_| gone())?;
		let mut file = new_file(temporary, true).map_err(|error| error.to_string())?;
		let matches =
			copy_checked(&mut reader, &mut file, content).map_err(|error| error.to_string())?;
		if !matches {
			return Err("its content does not match its hash".to_owned());
		}
		file.sync_all().map_err(|error| error.to_string())
	}

	/// The content with the hash `content` of a version, as this device holds it, if no larger
	/// than a merge reads: of a secret file only with the secrets unlocked, and only content
	/// sealed for the secrets; of another, only content sealed for every replica.
	pub(super) fn version_bytes(
		&self,
		space: SpaceId,
		content: &nibkeep_base::Hash,
		secret: bool,
	) -> Result<Option<Vec<u8>>, NodeError> {
		if !secret {
			return self.content_bytes(space, content);
		}
		if !self.secrets_unlocked(space) {
			return Ok(None);
		}
		let open = self.space(space)?;
		if !open.stored.has_secret_blob_key(content) || !open.stored.holds_blob(content) {
			return Ok(None);
		}
		let reader = open.stored.open_blob(content)?;
		let mut bytes = Vec::new();
		let read = io::Read::read_to_end(&mut io::Read::take(reader, READ_LIMIT + 1), &mut bytes)
			.map_err(|_| NodeError::Store(nibkeep_store::StoreError::DamagedBlob))?;
		if read as u64 > READ_LIMIT || nibkeep_base::Hash::of(&bytes) != *content {
			return Ok(None);
		}
		Ok(Some(bytes))
	}

	/// Writes the content with the hash `content`, from the store, into the new file
	/// `temporary`, checked against its hash, and saves it to the disk.
	fn blob_into(
		&self,
		space: SpaceId,
		content: &nibkeep_base::Hash,
		temporary: &Path,
	) -> Result<(), String> {
		let open = self.space(space).map_err(|error| error.to_string())?;
		// Only content sealed for every replica: a secret file's copy of the same content is
		// sealed for the secrets, and never written in plain.
		if !open.stored.holds_ordinary_blob(content) {
			return Err("this device holds no copy of its content".to_owned());
		}
		let mut reader = open
			.stored
			.open_blob(content)
			.map_err(|_| "this device holds no copy of its content".to_owned())?;
		let mut file = File::create_new(temporary).map_err(|error| error.to_string())?;
		let matches =
			copy_checked(&mut reader, &mut file, content).map_err(|error| error.to_string())?;
		if !matches {
			return Err("its content does not match its hash".to_owned());
		}
		file.sync_all().map_err(|error| error.to_string())
	}
}

/// Makes `folder` and the folders above it that are missing, each saved into the folder that
/// holds it, so that a power cut never keeps a copy recorded in a folder that it loses.
pub(super) fn make_folders(folder: &Path) -> io::Result<()> {
	let missing: Vec<&Path> = folder
		.ancestors()
		.take_while(|at| fs::symlink_metadata(at).is_err())
		.collect();
	fs::create_dir_all(folder)?;
	for made in missing.iter().rev() {
		sync_folder_of(made);
	}
	Ok(())
}

/// Moves what is at `temporary`, beside a copy, to the path `to` of the tree, which must be
/// free: never through a link, into the library, nor onto something.
fn move_into_place(place: Place<'_>, temporary: &Path, to: &str) -> Result<(), String> {
	let blocked = |_| "something is in its place".to_owned();
	let target = place_for_new(place.root, to, place.apart).map_err(blocked)?;
	if let Some(parent) = target.parent() {
		make_folders(parent).map_err(|error| error.to_string())?;
	}
	// What made the folders may have raced a link into them, or something into its place.
	if place_for_new(place.root, to, place.apart).as_ref() != Ok(&target) {
		return Err("something is in its place".to_owned());
	}
	fs::rename(temporary, &target).map_err(|error| error.to_string())?;
	sync_folder_of(&target);
	Ok(())
}

/// What undoing an edit makes of a copy.
enum Merged {
	/// The copy holds the version from before already.
	Unchanged,
	/// What to write, cleanly.
	Clean(Owned),
	/// What to write, with Git's conflict markers where edits clash.
	Clashed(Owned),
	/// The copy stays as it is, and the version from before goes beside it.
	Beside,
}

/// Content to write that this module owns.
enum Owned {
	Version(Version),
	Bytes(Vec<u8>, bool),
}

impl Owned {
	fn as_content(&self, secret: bool) -> Content<'_> {
		match self {
			Self::Version(version) => Content::of(version, secret),
			Self::Bytes(bytes, executable) => Content::bytes(bytes, *executable, secret),
		}
	}
}

/// The version that this copy last held as the index says, or else, as the tree holds it.
fn recorded_version<'a>(
	entries: &'a BTreeMap<String, Entry>,
	now: &'a TreeState,
	path: &str,
) -> Option<&'a Version> {
	entries.get(path).map(|entry| &entry.version).or_else(|| {
		now.by_path
			.get(path)
			.and_then(|current| current.version.as_ref())
	})
}

/// The version that a copy holds, as reading it tells; `None` if it cannot be read.
pub(super) fn copy_version(found: &Found) -> Option<Version> {
	match &found.kind {
		FoundKind::Link(target) => Some(Version::Link {
			target: target.clone(),
		}),
		FoundKind::File => hash_found(found).ok().map(|content| Version::File {
			content,
			executable: found.executable.unwrap_or(false),
		}),
	}
}

/// Whether a copy holds `recorded`: the same content or target, and where the system keeps
/// executable bits, the same bit.
pub(super) fn same_version(found: &Found, holds: &Version, recorded: &Version) -> bool {
	if found.executable.is_some() {
		holds == recorded
	} else {
		same_content(holds, recorded)
	}
}

/// Why a file that a folder held at a tree's path stays out of commits: it cannot be read.
const UNREADABLE: &str =
	"it was there and could not be read; it stays out of commits until you rename or delete it";

/// Why a file that a folder held at a tree's path stays out of commits: the tree's rules
/// would make it secret, where the tree's file is not.
const WOULD_BE_SECRET: &str = "it was there, and would be a secret file where the tree's is not; it stays out of commits until you rename or delete it";

impl Taken {
	/// What is taken, but a copy that holds neither the tree's version nor an older one,
	/// where the tree's rules would make it secret, `would_be_secret`: it is marked instead,
	/// so that no commit records it as a version of the tree's ordinary file.
	fn unless_secret(self, would_be_secret: bool) -> Self {
		match self {
			Taken::Copy(Kept::Differing, _) if would_be_secret => {
				Taken::Marked(WOULD_BE_SECRET.to_owned())
			}
			taken => taken,
		}
	}
}

/// Whether a folder to write a tree into holds anything; one that is not there is made.
fn holds_anything(folder: &Path) -> Result<bool, NodeError> {
	match fs::read_dir(folder) {
		Ok(mut entries) => Ok(entries.next().is_some()),
		Err(error) if error.kind() == io::ErrorKind::NotFound => {
			fs::create_dir_all(folder).map_err(NodeError::file(folder))?;
			Ok(false)
		}
		Err(error) => Err(NodeError::file(folder)(error)),
	}
}

/// Whether two versions hold the same content or target, whatever their executable bits.
fn same_content(a: &Version, b: &Version) -> bool {
	super::compare::same_content(a, b)
}

/// Writes bytes into the new file `temporary`, which only its owner may read on Unix if
/// `private` says so, and saves it to the disk.
fn bytes_into(bytes: &[u8], temporary: &Path, private: bool) -> Result<(), String> {
	let mut file = new_file(temporary, private).map_err(|error| error.to_string())?;
	file.write_all(bytes)
		.and_then(|()| file.sync_all())
		.map_err(|error| error.to_string())
}

/// Makes the new file `temporary`, which only its owner may read on Unix if `private` says
/// so.
fn new_file(temporary: &Path, private: bool) -> io::Result<File> {
	let mut options = File::options();
	options.write(true).create_new(true);
	#[cfg(unix)]
	if private {
		std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	}
	#[cfg(not(unix))]
	let _ = private;
	options.open(temporary)
}

/// Makes a file executable, or not, where the system keeps the bit.
#[cfg_attr(
	not(unix),
	expect(clippy::unnecessary_wraps, reason = "it can fail only on Unix")
)]
fn set_executable(path: &Path, executable: bool) -> Result<(), String> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = fs::metadata(path)
			.map_err(|error| error.to_string())?
			.permissions()
			.mode();
		let mode = if executable {
			mode | ((mode & 0o444) >> 2)
		} else {
			mode & !0o111
		};
		fs::set_permissions(path, fs::Permissions::from_mode(mode))
			.map_err(|error| error.to_string())
	}
	#[cfg(not(unix))]
	{
		let _ = (path, executable);
		Ok(())
	}
}

/// Makes a symbolic link at `at` to `target`, as it was written; where the system cannot,
/// as Windows without leave, says so.
fn make_link(target: &str, at: &Path) -> Result<(), String> {
	#[cfg(unix)]
	{
		std::os::unix::fs::symlink(target, at).map_err(|error| error.to_string())
	}
	#[cfg(windows)]
	{
		std::os::windows::fs::symlink_file(target.replace('/', "\\"), at)
			.map_err(|error| format!("this system made no link: {error}"))
	}
	#[cfg(not(any(unix, windows)))]
	{
		let _ = (target, at);
		Err("this system makes no links".to_owned())
	}
}
