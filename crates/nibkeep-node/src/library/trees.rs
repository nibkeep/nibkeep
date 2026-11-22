//! Trees: folders whose files NibKeep tracks by their paths in them, with every version, as
//! Git tracks a working tree ([technical design: trees]). A tree is an entity of its own, with a
//! root on each device that holds a copy of it; each of its files is a file of its own,
//! whatever its content, with its path; a commit is a change for each of its parts, a first
//! that makes its revision, with its message, then one for each entity it records
//! ([technical design: one entity per change]); and each replica keeps the state of its copy in an index of its
//! own, which tells what to read.
//!
//! [technical design: trees]: ../../../../docs/Technical-design.md#15-trees
//! [technical design: one entity per change]: ../../../../docs/Technical-design.md#62-one-entity-per-change

mod commit;
mod compare;
mod diff;
mod git;
mod history;
mod index;
mod locate;
mod paths;
mod repositories;
mod secrecy;
mod update;
mod walk;
mod write;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements as _;
use nibkeep_model::{FieldName, Name, Statement, TypeName, Value, fields};
use nibkeep_std::{
	COMMITTED_IN, CONTENT, EXECUTABLE, FILE, GIT_REPOSITORY, IN_TREE, LINK_TARGET, ON_DEVICE,
	PARTS, PATH, REVISION, ROOT_OF, SECRET_CONTENT, SECRET_FILE, SYMBOLIC_LINK, TREE, TREE_PATH,
	TREE_ROOT,
};

use nibkeep_log::{ChangeId, Log};
use nibkeep_store::{LogFile, StateFile};

use self::commit::moving;
use self::index::{Entry, INDEX_CONTEXT, Index, SecretVersions, Version};
use self::locate::{Blocked, Listings, Located, located, located_in, place_for_new};
pub use self::repositories::{CloneFolder, GitRepository, RecordedRepository, ToClone};
use super::Library;
use super::entities::NewValue;
use crate::NodeError;

pub use self::diff::FileDiff;
pub use self::history::{BlameLine, TreeLogEntry, TreePoint};
pub use self::paths::is_safe_tree_path;
pub use self::update::Updated;
pub use self::write::{CopiesRemoved, Reverted, Written};

/// The file in the replica's folder that commits to its trees lock, so that two programs
/// never commit to one copy at once.
const LOCK: &str = "trees.lock";

/// The types that a tree's records need.
const TREE_TYPES: [TypeName; 7] = [
	TREE,
	TREE_ROOT,
	FILE,
	SECRET_FILE,
	SYMBOLIC_LINK,
	REVISION,
	GIT_REPOSITORY,
];

/// A tree of a space, and where its folder is on this device, if it has a root here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
	/// The tree.
	pub id: EntityId,
	/// Its name: that of its folder when it started.
	pub name: Option<String>,
	/// Its folder on this device, if it has one.
	pub root: Option<PathBuf>,
}

/// A file or a link of a tree, as the library has it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
	/// The file or link.
	pub entity: EntityId,
	/// Its path in the tree.
	pub path: String,
	/// What it holds.
	pub kind: TreeEntryKind,
}

/// What a file or a link of a tree holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEntryKind {
	/// A file: its content, unless a purge took it away, and whether it is executable.
	File {
		/// The hash of its content.
		content: Option<Hash>,
		/// Whether it is executable.
		executable: bool,
	},
	/// A symbolic link, and what it points to.
	Link {
		/// Its target, as it was written.
		target: String,
	},
	/// A secret file whose content this replica cannot read now: its secrets are locked, or
	/// not on this device ([technical design: secrets in trees]).
	///
	/// [technical design: secrets in trees]: ../../../docs/Technical-design.md#151-secrets-in-trees
	Sealed,
}

/// What a path of a tree's copy holds that the tree does not, as a commit records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeChange {
	/// The path; of a move, where it went.
	pub path: String,
	/// What changed.
	pub change: Change,
}

/// What changed at a path of a tree's copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
	/// A file or a link that the tree does not hold.
	New,
	/// A new version: other content, another target, or a file that became a link or the
	/// other way round.
	Edited,
	/// The same content, executable now or no longer.
	Executable(bool),
	/// Deleted from the copy.
	Deleted,
	/// Moved from another path, with the same content, as Git finds exact renames.
	Moved {
		/// Where it was.
		from: String,
	},
	/// Edited from a version that this replica does not know, as after its index was lost:
	/// a commit records it only with force.
	Unknown,
}

/// What a tree's copy on this device holds that the tree does not, and what the tree holds
/// that the copy does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeStatus {
	/// The tree.
	pub tree: EntityId,
	/// Its folder on this device.
	pub root: PathBuf,
	/// What a commit would record, by path.
	pub changes: Vec<TreeChange>,
	/// The paths that changed in the tree since the copy was written, as by another device,
	/// which the copy does not have yet: other versions, moves, additions and deletions.
	pub behind: Vec<String>,
	/// The paths that have competing values, as when two devices committed one file
	/// offline: several versions, several paths, or several files at one path.
	pub competing: Vec<String>,
	/// What the copy holds that a commit cannot take, by path, with why.
	pub skipped: Vec<(String, String)>,
	/// The commits cut short, as by a crash, before all their parts were made: the next
	/// commit takes in the rest.
	pub cut_short: Vec<CutShort>,
	/// How many files were read in full: those whose size or time changed.
	pub read: usize,
	/// The folders of the copy that hold Git repositories, by their paths in the tree: what
	/// `nib` asks Git about, for a commit to record.
	pub repositories: Vec<String>,
	/// The paths among the changes that are secret files, or would go in as new ones, which
	/// a commit seals for the space's secrets ([technical design: secrets in trees]).
	///
	/// [technical design: secrets in trees]: ../../../docs/Technical-design.md#151-secrets-in-trees
	pub secret: Vec<String>,
	/// The secret files of the tree that this copy lacks, as this replica cannot read them:
	/// its secrets are locked, or not on this device.
	pub sealed: Vec<String>,
}

/// The secret files of a tree, and its ordinary files that look secret
/// ([technical design: secrets in trees]).
///
/// [technical design: secrets in trees]: ../../../docs/Technical-design.md#151-secrets-in-trees
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeSecrets {
	/// The paths of its secret files, ordered.
	pub secret: Vec<String>,
	/// The paths of the secret files that this copy lacks, as this replica cannot read them.
	pub sealed: Vec<String>,
	/// The paths of its ordinary files that a built-in name or a `.nibsecret` file matches,
	/// as files committed before one did: they stay ordinary until made secret.
	pub looking_secret: Vec<String>,
}

/// Where a secret file of a tree is: the tree whose commit made it, which it belongs to for
/// good, and its path there ([technical design: secrets in trees]).
///
/// [technical design: secrets in trees]: ../../../docs/Technical-design.md#151-secrets-in-trees
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InTree {
	/// The tree.
	pub tree: EntityId,
	/// Its path in the tree, its last if it was deleted from the tree; of competing paths,
	/// the one in the statement with the smallest ID, as [`Library::tree_entries`] gives it.
	/// None if no path that a tree may hold.
	pub path: Option<String>,
}

/// What purging the versions of an ordinary file that a commit made secret takes, and what
/// it leaves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MadeSecret {
	/// The contents purged, which no other value holds as an ordinary file's, in the order
	/// the file had them.
	pub purged: Vec<Hash>,
	/// The contents left, which another value holds as an ordinary file's, as they were never
	/// secret.
	pub kept: Vec<Hash>,
}

/// A commit that was cut short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutShort {
	/// Its revision.
	pub revision: EntityId,
	/// How many parts it was to have.
	pub parts: u64,
	/// How many it has.
	pub made: u64,
}

/// What to commit to a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit<'a> {
	message: Option<&'a str>,
	paths: Vec<String>,
	force: bool,
	cut_after: Option<usize>,
	repositories: Option<Repositories<'a>>,
	secret: Vec<String>,
	ordinary: Vec<String>,
	leave_secrets: bool,
}

/// Where a commit learns what Git says of the repositories in a copy.
#[derive(Clone, Copy)]
enum Repositories<'a> {
	/// As given.
	Given(&'a [GitRepository]),
	/// From what reads them, given the folders found.
	Read(&'a dyn Fn(&[String]) -> Vec<GitRepository>),
}

impl std::fmt::Debug for Repositories<'_> {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Given(repositories) => {
				formatter.debug_tuple("Given").field(repositories).finish()
			}
			Self::Read(_) => formatter.write_str("Read"),
		}
	}
}

impl PartialEq for Repositories<'_> {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Given(a), Self::Given(b)) => a == b,
			(Self::Read(a), Self::Read(b)) => std::ptr::addr_eq(*a, *b),
			_ => false,
		}
	}
}

impl Eq for Repositories<'_> {}

impl<'a> Commit<'a> {
	/// Everything that changed, with a message or without.
	#[must_use]
	pub fn new(message: Option<&'a str>) -> Self {
		Self {
			message,
			paths: Vec::new(),
			force: false,
			cut_after: None,
			repositories: None,
			secret: Vec::new(),
			ordinary: Vec::new(),
			leave_secrets: false,
		}
	}

	/// Records the Git repositories in the copy as Git describes them: those found new, and
	/// what changed of those the tree records, which moved or are gone. Without it, a
	/// commit leaves repositories as they are.
	#[must_use]
	pub fn with_repositories(mut self, repositories: &'a [GitRepository]) -> Self {
		self.repositories = Some(Repositories::Given(repositories));
		self
	}

	/// Records the Git repositories in the copy as `read` describes the folders that the
	/// commit finds holding them, as with [`Commit::with_repositories`]: for a program that
	/// asks Git only once the folder is walked.
	#[must_use]
	pub fn with_repositories_read_by(
		mut self,
		read: &'a dyn Fn(&[String]) -> Vec<GitRepository>,
	) -> Self {
		self.repositories = Some(Repositories::Read(read));
		self
	}

	/// Only what changed at these paths of the tree, or in these folders of it; a move
	/// counts if either of its ends does.
	#[must_use]
	pub fn with_paths(mut self, paths: Vec<String>) -> Self {
		self.paths = paths;
		self
	}

	/// Records on top what changed from a version that is no longer the tree's, which
	/// history keeps, and what was edited from a version this replica does not know.
	#[must_use]
	pub fn with_force(mut self) -> Self {
		self.force = true;
		self
	}

	/// Takes in as secret files the new files at these paths, which no built-in name or
	/// `.nibsecret` file makes secret; and makes secret the ordinary files committed there:
	/// each becomes a new secret file, the old one deleted, whose versions
	/// [`Library::purge_made_secret`] purges once the person agrees.
	#[must_use]
	pub fn with_secret(mut self, paths: Vec<String>) -> Self {
		self.secret = paths;
		self
	}

	/// Takes in as ordinary files the new files at these paths, which would be secret.
	#[must_use]
	pub fn with_ordinary(mut self, paths: Vec<String>) -> Self {
		self.ordinary = paths;
		self
	}

	/// Commits the rest where a change of a secret file cannot be sealed, as the secrets are
	/// locked or not on this device, which leaves those changes for a later commit.
	#[must_use]
	pub fn leaving_secrets(mut self) -> Self {
		self.leave_secrets = true;
		self
	}

	/// Stops after this many parts, as a crash would: for trying what devices do with a
	/// commit cut short.
	#[must_use]
	pub fn cut_after(mut self, parts: usize) -> Self {
		self.cut_after = Some(parts);
		self
	}
}

/// What a commit recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Committed {
	/// The commit's revision, which its first part made; none if nothing changed.
	pub revision: Option<EntityId>,
	/// How many parts it made, a change for each: the first, with the revision, and one for
	/// each entity it recorded ([technical design: one entity per change]).
	///
	/// [technical design: one entity per change]: ../../../docs/Technical-design.md#62-one-entity-per-change
	pub parts: usize,
	/// What it recorded, by path.
	pub changes: Vec<TreeChange>,
	/// What it recorded of Git repositories, by path.
	pub repositories: Vec<TreeChange>,
	/// The paths of the secret files it recorded, new versions or new files, ordered.
	pub secret: Vec<String>,
	/// The paths of the changes of secret files that it left, as the secrets were locked or
	/// not on this device, which a later commit takes.
	pub left: Vec<String>,
	/// The paths of the new ordinary files that hold the content of a secret file of the
	/// tree, which the secrets do not protect there.
	pub unprotected: Vec<String>,
	/// The ordinary files it made secret, each now a new secret file at its path, whose old
	/// versions [`Library::purge_made_secret`] purges.
	pub made_secret: Vec<EntityId>,
}

/// How much a tree would take in if it started in a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartPreview {
	/// How many files and links.
	pub files: usize,
	/// How many bytes the files hold.
	pub bytes: u64,
	/// The folders that hold Git repositories, by their paths in the tree, which it records
	/// as references.
	pub repositories: Vec<String>,
}

/// What starting a tree did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeStarted {
	/// The tree.
	pub tree: EntityId,
	/// Its folder on this device, as the file system resolves it.
	pub root: PathBuf,
	/// Its first commit, of every file in the folder.
	pub committed: Committed,
}

/// A file or a link of a tree, as the state says.
#[derive(Clone, Debug)]
struct Current {
	entity: EntityId,
	/// Its version, or `None` for a file whose content a purge took away.
	version: Option<Version>,
}

/// The files and links of a tree, as the state says.
#[derive(Debug, Default)]
struct TreeState {
	by_path: BTreeMap<String, Current>,
	by_entity: BTreeMap<EntityId, String>,
	competing: BTreeSet<String>,
	/// The deleted files and links, by their last paths: what a file restored at its path
	/// is again.
	deleted: BTreeMap<String, Current>,
	/// The version of each file and link that is not deleted, its own, also of one that
	/// competes for its path with another.
	own: BTreeMap<EntityId, Option<Version>>,
	/// The versions of the files and links that have competing ones, as when two devices
	/// committed one file apart, each with its statements' order.
	versions: BTreeMap<EntityId, Vec<Version>>,
	/// The paths of the files and links whose paths compete, as when two devices moved one
	/// file apart: each is at its place at any of them.
	paths: BTreeMap<EntityId, Vec<String>>,
	/// The files and links deleted on one device while another gave them a new version,
	/// before either saw the other, with the keys of the deletions that compete with it, as
	/// competing values: they stay, among the competing, until a commit settles them,
	/// keeping them, or deleting them again.
	contested: BTreeMap<EntityId, Vec<String>>,
	/// The secret files of the tree, whose content only the space's secrets open.
	secret: BTreeSet<EntityId>,
	/// The secret files whose versions this replica cannot read now, as its secrets are
	/// locked or not on this device, and of which its copy holds nothing, by path.
	sealed: BTreeMap<String, EntityId>,
	/// Every path that a secret file of the tree has had: a new file there is secret too.
	secret_paths: BTreeSet<String>,
}

impl TreeState {
	/// Stands in, for each secret file that this replica cannot read now, the version that
	/// its copy here held when it was last written or committed, as the index says, so that
	/// the copy is neither written over nor deleted, and nothing of it is taken for a change,
	/// until the secrets open what the tree holds.
	fn fill_sealed(&mut self, entries: &BTreeMap<String, Entry>) {
		for (path, entity) in std::mem::take(&mut self.sealed) {
			let Some(entry) = entries.values().find(|entry| entry.entity == entity) else {
				self.sealed.insert(path, entity);
				continue;
			};
			let version = Some(entry.version.clone());
			self.own.insert(entity, version.clone());
			if self.by_path.contains_key(&path) {
				self.competing.insert(path.clone());
			} else {
				self.by_path
					.insert(path.clone(), Current { entity, version });
			}
			self.by_entity.insert(entity, path);
		}
	}

	/// Whether a file of the tree is a secret one.
	fn is_secret(&self, entity: EntityId) -> bool {
		self.secret.contains(&entity)
	}

	/// Whether the state holds `entity` at `path`, at `version`.
	fn holds(&self, path: &str, entity: EntityId, version: &Version) -> bool {
		self.at(entity, path) && self.has_version(entity, version)
	}

	/// Whether `entity` is at `path`: its path, or one of those that compete.
	fn at(&self, entity: EntityId, path: &str) -> bool {
		self.by_entity.get(&entity).is_some_and(|at| at == path)
			|| self
				.paths
				.get(&entity)
				.is_some_and(|paths| paths.iter().any(|at| at == path))
	}

	/// Whether the state holds `version` of `entity`, wherever it is: its own version, or
	/// one of those that compete.
	fn has_version(&self, entity: EntityId, version: &Version) -> bool {
		// Of a file whose versions compete, any of them: which one shows first follows the
		// statements' random IDs. An edit of such a file waits for an update all the same
		// (`compare::settle_competing`).
		match self.versions.get(&entity) {
			Some(all) => all.contains(version),
			None => self
				.own
				.get(&entity)
				.is_some_and(|own| own.as_ref() == Some(version)),
		}
	}
}

impl Library {
	/// Whether a tree could start in `folder`, and how much it would take in: for asking the
	/// person first, as [`Library::start_tree`] makes the folder's files the tree's copies.
	///
	/// # Errors
	///
	/// As [`Library::start_tree`], before it writes anything.
	pub fn would_start_tree(
		&self,
		space: SpaceId,
		folder: &Path,
	) -> Result<StartPreview, NodeError> {
		let root = self.checked_new_root(space, folder)?;
		let walked = walk::walk(&root, &BTreeSet::new(), &self.apart());
		Ok(StartPreview {
			files: walked.found.len(),
			bytes: walked.found.values().map(|found| found.size).sum(),
			repositories: walked.repositories,
		})
	}

	/// Starts a tree in `folder`: a tree named after the folder, its root on this device,
	/// and a first commit of every file in it, with `message`, in parts, as every commit is.
	/// The folder's files become the tree's copies, which a purge of
	/// their content deletes where they were not edited, so apps ask the person first.
	///
	/// # Errors
	///
	/// Returns [`NodeError::TreeInTree`] if the folder is in a tree's folder on this device,
	/// or holds one; [`NodeError::GitRepositoryHere`] if it holds a Git repository, which
	/// trees record as references; and another error if it is no folder, or reading it or
	/// saving the changes fails.
	pub fn start_tree(
		&mut self,
		space: SpaceId,
		folder: &Path,
		message: Option<&str>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<TreeStarted, NodeError> {
		self.start_tree_with(space, folder, &Commit::new(message), wall_millis, rng)
	}

	/// Starts a tree in `folder`, as [`Library::start_tree`] does, with the first commit's
	/// message and the Git repositories in the folder, as Git describes them, from `commit`.
	///
	/// # Errors
	///
	/// As [`Library::start_tree`].
	pub fn start_tree_with(
		&mut self,
		space: SpaceId,
		folder: &Path,
		commit: &Commit<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<TreeStarted, NodeError> {
		self.check_not_revoked(space)?;
		let _locked = self.lock_trees()?;
		let root = self.checked_new_root(space, folder)?;
		self.install_standard(space, &TREE_TYPES, wall_millis, rng)?;
		let name = root
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.filter(|name| !name.is_empty());
		let mut values = Vec::new();
		if let Some(name) = name {
			values.push(NewValue::new(fields::NAME, Value::Name(Name::new(&name))));
		}
		let (tree, founded) = self.new_entity(space, &[TREE], values, rng)?;
		let (root_entity, rooted) = self.new_entity(
			space,
			&[TREE_ROOT],
			vec![
				NewValue::new(ROOT_OF, Value::Reference(tree)),
				NewValue::new(ON_DEVICE, Value::Reference(self.info.device)),
				NewValue::new(PATH, Value::Text(root.to_string_lossy().into_owned())),
			],
			rng,
		)?;
		// The root is this device's own record, saved with the commit, which no other device
		// waits for.
		let founding = vec![(Some(tree), founded), (None, rooted)];
		let committed =
			self.commit_in(space, (tree, &root), commit, founding, (wall_millis, rng))?;
		self.index()?.set_root(tree, root_entity, &root)?;
		Ok(TreeStarted {
			tree,
			root,
			committed,
		})
	}

	/// The trees of a space that are not deleted, with their folders on this device, ordered
	/// by name.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn trees(&self, space: SpaceId) -> Result<Vec<Tree>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let roots = self.roots_here(space)?;
		let mut trees = Vec::new();
		for tree in super::entities::with_type(&open.stored, &TREE)? {
			let tree = state.canonical(tree)?;
			if state.is_deleted(tree)? || trees.iter().any(|known: &Tree| known.id == tree) {
				continue;
			}
			trees.push(Tree {
				id: tree,
				name: self.name(space, tree)?,
				root: roots.get(&tree).cloned(),
			});
		}
		trees.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
		Ok(trees)
	}

	/// The tree whose folder on this device holds `folder`, or is it, if there is one.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn tree_at(&self, space: SpaceId, folder: &Path) -> Result<Option<Tree>, NodeError> {
		let Some(folder) = resolved(folder) else {
			return Ok(None);
		};
		let roots = self.roots_here(space)?;
		let Some((&tree, root)) = roots.iter().find(|(_, root)| folder.starts_with(root)) else {
			return Ok(None);
		};
		Ok(Some(Tree {
			id: tree,
			name: self.name(space, tree)?,
			root: Some(root.clone()),
		}))
	}

	/// The files and links of a tree, as the library has them now, ordered by path. Of
	/// competing values, the one in the statement with the smallest ID.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn tree_entries(
		&self,
		space: SpaceId,
		tree: EntityId,
	) -> Result<Vec<TreeEntry>, NodeError> {
		let state = self.tree_state(space, tree)?;
		let mut entries: Vec<TreeEntry> = state
			.by_path
			.into_iter()
			.map(|(path, current)| TreeEntry {
				entity: current.entity,
				path,
				kind: match current.version {
					Some(Version::Link { target }) => TreeEntryKind::Link { target },
					Some(Version::File {
						content,
						executable,
					}) => TreeEntryKind::File {
						content: Some(content),
						executable,
					},
					None => TreeEntryKind::File {
						content: None,
						executable: false,
					},
				},
			})
			.chain(state.sealed.into_iter().map(|(path, entity)| TreeEntry {
				entity,
				path,
				kind: TreeEntryKind::Sealed,
			}))
			.collect();
		entries.sort_by(|a, b| a.path.cmp(&b.path));
		Ok(entries)
	}

	/// What the copy of a tree on this device holds that the tree does not, and what the
	/// tree holds that the copy does not. It reads only the files whose size or
	/// modification time changed since they were written or committed, and records what it
	/// found unchanged, so that it is not read again. It waits for a commit to the tree that
	/// runs, and deletes the copies of content that a purge took away, as
	/// [`Library::settle_checkouts`] does after a sync.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoTreeHere`] if the tree has no folder on this device,
	/// [`NodeError::TreeFolderGone`] if its folder is gone, and an error if the space or the
	/// index cannot be read.
	pub fn tree_status(&self, space: SpaceId, tree: EntityId) -> Result<TreeStatus, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let mut index = self.index()?;
		let purged = self.learn_purges(space, &index)?;
		let state = self.tree_state(space, tree)?;
		let apart = self.apart();
		settle_tree((tree, &root, &apart), &mut index, (&purged, &state))?;
		self.follow_cut_short(space, (tree, &root, &apart), &mut index, &state)?;
		// Where content was purged, new files are read too, as a commit reads them, so that
		// one holding it is not shown as a change to commit.
		let read_new = !purged.is_empty();
		let compared = self.compare(space, (tree, &root), &state, &index, (&purged, read_new))?;
		index.change(tree, &compared.forgotten, &compared.refreshed)?;
		self.note_secrets(space, (tree, &index), &state, &compared.refreshed)?;
		index.unmark(tree, &compared.unmarked)?;
		if compared.known {
			index.know(tree)?;
		}
		Ok(TreeStatus {
			tree,
			root,
			changes: compared
				.pending
				.iter()
				.map(|pending| TreeChange {
					path: pending.path.clone(),
					change: pending.change.clone(),
				})
				.collect(),
			secret: compared
				.pending
				.iter()
				.filter(|pending| pending.secret)
				.map(|pending| pending.path.clone())
				.collect(),
			sealed: state.sealed.keys().cloned().collect(),
			behind: compared.behind.into_iter().collect(),
			competing: state.competing.into_iter().collect(),
			skipped: compared
				.skipped
				.into_iter()
				.chain(compared.marked)
				.collect(),
			cut_short: self.cut_short(space, tree)?,
			read: compared.read,
			repositories: compared.repositories,
		})
	}

	/// Commits what changed in the copy of a tree on this device, or at the paths given: new
	/// files and links, new versions, moves, deletions and executable bits, in parts made one
	/// after another ([technical design: one entity per change]): a first that makes the commit's revision, with
	/// the message and how many parts there are, then a part for each entity it records,
	/// which names the revision. Every version is sealed into the store, whatever storage
	/// policies say. Returns nothing if nothing changed.
	///
	/// [technical design: one entity per change]: ../../../docs/Technical-design.md#62-one-entity-per-change
	///
	/// # Errors
	///
	/// Returns [`NodeError::TreeStale`] if a path changed here and in the tree since the
	/// copy was written, as by another device, [`NodeError::UnknownVersion`] if a copy was
	/// edited from a version this replica does not know, and [`NodeError::TreeEmptied`] if
	/// every file of the tree is gone from its copy, unless the commit is forced;
	/// [`NodeError::NoTreeHere`] if the tree has no folder on this device, and
	/// [`NodeError::TreeFolderGone`] if its folder is gone; and an error if a file cannot be
	/// read or the changes saved.
	pub fn commit_tree(
		&mut self,
		space: SpaceId,
		tree: EntityId,
		commit: &Commit<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Committed, NodeError> {
		self.check_not_revoked(space)?;
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		self.install_standard(space, &TREE_TYPES, wall_millis, rng)?;
		self.commit_in(space, (tree, &root), commit, Vec::new(), (wall_millis, rng))
	}

	/// The secret files of a tree, and its ordinary files that a built-in name, or a
	/// `.nibsecret` file of this device's copy, matches: files committed before one did,
	/// which stay ordinary until a commit makes them secret.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotATree`] if `tree` is none, and an error if the space, the
	/// index or the copy cannot be read.
	pub fn tree_secrets(&self, space: SpaceId, tree: EntityId) -> Result<TreeSecrets, NodeError> {
		let tree = self.check_tree(space, tree)?;
		let state = self.tree_state(space, tree)?;
		let rules = match self.root_here(space, tree) {
			Ok(root) => {
				let index = self.index()?;
				let left_out = self.left_out(space, tree, &root, &index)?;
				secrecy::Rules::of_copy(&walk::walk(&root, &left_out, &self.apart()).found)
			}
			Err(_) => secrecy::Rules::built_in(),
		};
		let mut listed = TreeSecrets {
			sealed: state.sealed.keys().cloned().collect(),
			..TreeSecrets::default()
		};
		for (path, current) in &state.by_path {
			if state.is_secret(current.entity) {
				listed.secret.push(path.clone());
			} else if matches!(current.version, Some(Version::File { .. })) && rules.is_secret(path)
			{
				listed.looking_secret.push(path.clone());
			}
		}
		listed.secret.extend(listed.sealed.iter().cloned());
		listed.secret.sort();
		listed.secret.dedup();
		Ok(listed)
	}

	/// The tree that a secret file belongs to, and its path there, if it is a tree's: the
	/// tree whose commit made it, whatever changes said later of its tree, as any replica of
	/// the space may say them, an AI agent's too. A secret file that no commit of a tree
	/// made, as a password record's, is no tree's, whatever its tree and path say. A file
	/// deleted from its tree is still the tree's. It needs neither the secrets nor a copy of
	/// the tree on this device.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn secret_file_in_tree(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<Option<InTree>, NodeError> {
		self.space(space)?.stored.reading(|stored| {
			let state = stored.log().state();
			let file = state.canonical(file)?;
			let Secrecy::Of(tree) = secret_file_of(state, file)? else {
				return Ok(None);
			};
			let mut statements = state.current(file, &TREE_PATH)?;
			statements.sort_by_key(|statement| statement.id);
			let path = statements
				.iter()
				.find_map(|statement| match &statement.value {
					Value::Text(path) => Some(path.clone()),
					_ => None,
				})
				.filter(|path| is_safe_tree_path(path));
			Ok(Some(InTree { tree, path }))
		})
	}

	/// What purging the versions of an ordinary file that a commit made secret would take:
	/// the contents its versions held, current or past, that no other value holds as an
	/// ordinary file's, which [`Library::purge_made_secret`] purges; and those that another
	/// holds, which were never secret, and stay. A file that is not deleted, as one still in
	/// its tree, takes nothing.
	///
	/// # Errors
	///
	/// The errors of [`Library::purgeable`].
	pub fn would_purge_made_secret(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<MadeSecret, NodeError> {
		let mut contents: Vec<(Option<nibkeep_time::Timestamp>, Hash)> = Vec::new();
		{
			let state = self.space(space)?.stored.log().state();
			if !state.is_deleted(state.canonical(file)?)? {
				return Ok(MadeSecret::default());
			}
			for id in state.storage().of_field(file, &CONTENT)? {
				if let Some(recorded) = state.storage().get(id)?
					&& let Value::Blob(content) = recorded.statement.value
					&& contents.iter().all(|(_, known)| *known != content)
				{
					contents.push((state.recorded(id)?, content));
				}
			}
		}
		contents.sort();
		let mut made = MadeSecret::default();
		for (_, content) in contents {
			let purge = self.purgeable(space, file, &CONTENT, Some(&Value::Blob(content)))?;
			if purge.also.is_empty() {
				made.purged.push(content);
			} else {
				made.kept.push(content);
			}
		}
		Ok(made)
	}

	/// Purges, for good and everywhere, the contents of the versions of an ordinary file that
	/// a commit made secret, as [`Library::would_purge_made_secret`] finds them, once the
	/// person agreed: the secret file at its path keeps its own sealed copy.
	///
	/// # Errors
	///
	/// The errors of [`Library::purge`].
	pub fn purge_made_secret(
		&mut self,
		space: SpaceId,
		file: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<MadeSecret, NodeError> {
		let made = self.would_purge_made_secret(space, file)?;
		for content in &made.purged {
			let value = Value::Blob(*content);
			self.purge(space, file, &CONTENT, Some(&value), wall_millis, rng)?;
		}
		Ok(made)
	}

	/// Moves a file or a link of a tree, or a folder with everything in it, on the disk and
	/// in the tree at once, as one commit: from the path `from` of the tree to `to`, which
	/// must be free. Edits not committed move with the copy, and stay to commit.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotInTree`] if the tree holds nothing at or under `from`,
	/// [`NodeError::UnsafePath`] if `to` is no path a tree may hold, would go through a link
	/// or names what cannot be written here, [`NodeError::PlaceTaken`] if something is at
	/// `to`, and an error if moving or saving fails.
	pub fn move_in_tree(
		&mut self,
		space: SpaceId,
		tree: EntityId,
		(from, to): (&str, &str),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Committed, NodeError> {
		self.check_not_revoked(space)?;
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		if !is_safe_tree_path(to) || !to.split('/').all(paths::fits_this_system) {
			return Err(NodeError::UnsafePath(to.to_owned()));
		}
		self.install_standard(space, &TREE_TYPES, wall_millis, rng)?;
		let state = self.tree_state(space, tree)?;
		let mut index = self.index()?;
		let entries = index.entries(tree)?;
		let under = |path: &str| path == from || path.starts_with(&format!("{from}/"));
		let moved_to = |path: &str| format!("{to}{}", &path[from.len()..]);
		// Every file and link there, those that compete for one path too.
		let moved: Vec<(String, String, EntityId)> = state
			.by_entity
			.iter()
			.filter(|(_, path)| under(path))
			.map(|(entity, path)| (path.clone(), moved_to(path), *entity))
			.collect();
		if moved.is_empty() {
			return Err(NodeError::NotInTree(from.to_owned()));
		}
		if to == from || to.starts_with(&format!("{from}/")) {
			return Err(NodeError::UnsafePath(to.to_owned()));
		}
		// The tree holds nothing at `to`, nor in it, nor a file on its way, even where its
		// copy is gone: one path holds one file.
		let clashes = |path: &String| {
			path == to || path.starts_with(&format!("{to}/")) || to.starts_with(&format!("{path}/"))
		};
		if state.by_path.keys().chain(entries.keys()).any(clashes) {
			return Err(NodeError::PlaceTaken(root.join(to)));
		}
		let apart = self.apart();
		let Located::Here(source) = located(&root, from, &apart) else {
			return Err(NodeError::NotInTree(from.to_owned()));
		};
		// Copies that NibKeep wrote there for other reasons, as checkouts, are recorded by
		// their paths, which a move would leave behind.
		if self
			.left_out(space, tree, &root, &index)?
			.iter()
			.any(|path| under(path))
		{
			return Err(NodeError::HoldsOtherCopies(from.to_owned()));
		}
		let blocked = |why| match why {
			Blocked::Taken => NodeError::PlaceTaken(root.join(to)),
			Blocked::Unsafe => NodeError::UnsafePath(to.to_owned()),
		};
		let target = place_for_new(&root, to, &apart).map_err(blocked)?;
		if let Some(parent) = target.parent() {
			write::make_folders(parent).map_err(NodeError::file(parent))?;
			// What made the folders may have raced a link into them.
			if place_for_new(&root, to, &apart).as_ref() != Ok(&target) {
				return Err(NodeError::UnsafePath(to.to_owned()));
			}
		}
		fs::rename(&source, &target).map_err(NodeError::file(&source))?;
		save_move(&source, &target);
		let mut parts: Vec<(EntityId, Vec<nibkeep_model::Operation>)> = Vec::new();
		let mut changes = Vec::new();
		let mut removed = Vec::new();
		let mut set = Vec::new();
		{
			let open = self.space(space)?;
			let space_state = open.stored.log().state();
			for (old, new, entity) in &moved {
				parts.push((*entity, moving(space_state, *entity, old, new, rng)?));
				changes.push(TreeChange {
					path: new.clone(),
					change: Change::Moved { from: old.clone() },
				});
			}
			// The index's entries move with the copies, whatever the tree holds.
			for (old, entry) in entries.iter().filter(|(path, _)| under(path)) {
				removed.push(old.clone());
				set.push((moved_to(old), entry.clone()));
			}
		}
		// A move is a commit in parts, a part for each file after one with its revision
		// (technical design: one entity per change), saved all or none, as the copy is moved already.
		let count = parts.len() + 1;
		let saved = self.batch_whole(space, |library| {
			let (revision, made) = library.revision_operations(space, tree, None, count, rng)?;
			library.commit(space, made, wall_millis, rng)?;
			for (entity, operations) in parts {
				let operations =
					library.part_operations(space, revision, entity, operations, rng)?;
				library.commit(space, operations, wall_millis, rng)?;
			}
			Ok(revision)
		});
		let revision = match saved {
			Ok(revision) => revision,
			Err(error) => {
				// The copy goes back where the tree has it.
				let _ = fs::rename(&target, &source);
				return Err(error);
			}
		};
		changes.sort_by(|a, b| a.path.cmp(&b.path));
		changes.dedup();
		index.change(tree, &removed, &set)?;
		Ok(Committed {
			revision: Some(revision),
			parts: count,
			changes,
			..Committed::default()
		})
	}
}

impl Library {
	/// The folder `folder` as the file system resolves it, if a tree may start there: a
	/// folder, in no tree's folder on this device and holding none, and holding no Git
	/// repository.
	fn checked_new_root(&self, space: SpaceId, folder: &Path) -> Result<PathBuf, NodeError> {
		let root = fs::canonicalize(folder).map_err(NodeError::file(folder))?;
		let root = super::files::plain(root);
		if !root.is_dir() {
			return Err(NodeError::file(&root)(io::ErrorKind::NotADirectory.into()));
		}
		// Never in the library's own folder, nor in any folder that a system reads as `.git`
		// or `.nib`.
		let in_reserved = root.components().any(|component| match component {
			std::path::Component::Normal(name) => paths::is_reserved(&name.to_string_lossy()),
			_ => false,
		});
		if in_reserved || self.apart().iter().any(|apart| root.starts_with(apart)) {
			return Err(NodeError::NotATreeFolder(root));
		}
		let _ = space;
		for &any in self.spaces.keys() {
			for other in self.roots_here(any)?.values() {
				if root.starts_with(other) || other.starts_with(&root) {
					return Err(NodeError::TreeInTree(root));
				}
			}
		}
		if fs::symlink_metadata(root.join(".git")).is_ok() || walk::is_bare(&root) {
			return Err(NodeError::GitRepositoryHere(root));
		}
		Ok(root)
	}

	/// The folders of the trees of a space on this device, by tree. A root counts only on
	/// the replica that recorded it, so that no replica names another's folders: as this
	/// replica's index lists it, with its folder, while the root and its tree are not deleted;
	/// or else, as after the index was lost, a root on this device whose values this replica
	/// wrote, all of them.
	fn roots_here(&self, space: SpaceId) -> Result<BTreeMap<EntityId, PathBuf>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let alive = |entity: EntityId| -> Result<bool, NodeError> {
			Ok(!state.is_deleted(state.canonical(entity)?)?)
		};
		let mut roots = BTreeMap::new();
		let recorded = if self.directory().join(index::INDEX).exists() {
			self.index()?.roots()?
		} else {
			BTreeMap::new()
		};
		for (tree, (root, folder)) in &recorded {
			let in_space = !state.types(*root)?.is_empty();
			if in_space && alive(*root)? && alive(*tree)? {
				roots.insert(state.canonical(*tree)?, folder.clone());
			}
		}
		let this_replica = self.replica.signing.verifying_key();
		for root in open
			.stored
			.find(&ON_DEVICE, &Value::Reference(self.info.device))?
		{
			let root = state.canonical(root)?;
			if state.is_deleted(root)? || !state.types(root)?.contains(&TREE_ROOT) {
				continue;
			}
			let mut values = BTreeMap::new();
			let mut own = true;
			for field in [&ROOT_OF, &ON_DEVICE, &PATH] {
				let mut current = state.current(root, field)?;
				current.sort_by_key(|statement| statement.id);
				for statement in &current {
					own &= open.stored.log().author_of(statement.id)? == Some(this_replica);
				}
				let first = current.first().map(|statement| statement.value.clone());
				values.insert(field.clone(), first);
			}
			let (true, Some(Some(Value::Reference(tree))), Some(Some(Value::Text(path)))) =
				(own, values.remove(&ROOT_OF), values.remove(&PATH))
			else {
				continue;
			};
			let tree = state.canonical(tree)?;
			if alive(tree)? {
				roots.entry(tree).or_insert_with(|| PathBuf::from(path));
			}
		}
		Ok(roots)
	}

	/// Whether `folder` is in a tree's folder on this device, of any space.
	pub(crate) fn in_any_tree(&self, folder: &Path) -> Result<bool, NodeError> {
		let Some(folder) = resolved(folder) else {
			return Ok(false);
		};
		for &space in self.spaces.keys() {
			if self
				.roots_here(space)?
				.values()
				.any(|root| folder.starts_with(root))
			{
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// A tree of a space, as the entity it is now; [`NodeError::NotATree`] if it is none.
	pub(super) fn check_tree(&self, space: SpaceId, tree: EntityId) -> Result<EntityId, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let tree = state.canonical(tree)?;
		if state.is_deleted(tree)? || !state.types(tree)?.contains(&TREE) {
			return Err(NodeError::NotATree(tree.to_string()));
		}
		Ok(tree)
	}

	/// The folder of a tree on this device, which must be there: a folder that is gone, as
	/// when it moved, is never taken for one whose files were all deleted.
	fn root_here(&self, space: SpaceId, tree: EntityId) -> Result<PathBuf, NodeError> {
		let tree = self.space(space)?.stored.log().state().canonical(tree)?;
		let root = self
			.roots_here(space)?
			.remove(&tree)
			.ok_or_else(|| NodeError::NoTreeHere(tree.to_string()))?;
		if !fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
			return Err(NodeError::TreeFolderGone(root));
		}
		Ok(root)
	}

	/// The index of this replica's copies of trees.
	fn index(&self) -> Result<Index, NodeError> {
		Index::open(self.directory(), &self.derived_key(INDEX_CONTEXT))
	}

	/// The index of this replica's copies of trees, if it has one, as a replica that holds
	/// no copy, such as an AI agent's, has not.
	fn index_if_any(&self) -> Result<Option<Index>, NodeError> {
		if fs::symlink_metadata(self.directory().join(index::INDEX)).is_err() {
			return Ok(None);
		}
		self.index().map(Some)
	}

	/// The name of the device whose replica made a statement, as the personal space names
	/// it, if it says.
	fn device_name_of(
		&self,
		space: SpaceId,
		statement: nibkeep_base::StatementId,
	) -> Option<String> {
		let named = || -> Result<Option<String>, NodeError> {
			let Some(author) = self.author_of(space, statement)? else {
				return Ok(None);
			};
			let personal = self.personal_space();
			let roster = self.space(personal)?.stored.log().state();
			let device = roster
				.current(author.replica, &nibkeep_governance::ON_DEVICE)?
				.into_iter()
				.find_map(|statement| statement.value.as_reference());
			match device {
				Some(device) => self.name(personal, device),
				None => Ok(None),
			}
		};
		named().ok().flatten()
	}

	/// Takes the lock of this replica's trees if no other program holds it, until what it
	/// returns is dropped; `None` if another holds it.
	fn try_lock_trees(&self) -> Result<Option<File>, NodeError> {
		let path = self.directory().join(LOCK);
		let file = File::options()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&path)
			.map_err(NodeError::file(&path))?;
		match file.try_lock() {
			Ok(()) => Ok(Some(file)),
			Err(std::fs::TryLockError::WouldBlock) => Ok(None),
			Err(std::fs::TryLockError::Error(error)) => Err(NodeError::file(&path)(error)),
		}
	}

	/// Takes the lock of commits to this replica's trees, until what it returns is dropped.
	fn lock_trees(&self) -> Result<File, NodeError> {
		let path = self.directory().join(LOCK);
		let file = File::options()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&path)
			.map_err(NodeError::file(&path))?;
		file.lock().map_err(NodeError::file(&path))?;
		Ok(file)
	}

	/// The files and links of a tree, as the state says: of competing values, the one in
	/// the statement with the smallest ID, and the path listed among the competing. A path
	/// that no tree may hold, as a faulty or hostile replica could record, is left out, so
	/// that nothing is ever written there.
	fn tree_state(&self, space: SpaceId, tree: EntityId) -> Result<TreeState, NodeError> {
		let open = self.space(space)?;
		let readable = self.secrets_unlocked(space);
		let mut read = open.stored.reading(|stored| {
			let log = stored.log();
			let state = log.state();
			let entities = tree_entities(state, tree)?;
			read_tree_state(state, tree, &entities, (Some(&Changes::of(log)), readable))
		})?;
		if !read.sealed.is_empty()
			&& let Some(index) = self.index_if_any()?
		{
			read.fill_sealed(&index.entries(tree)?);
		}
		Ok(read)
	}

	/// Records in the index which of the entries set are of secret files, with the sealed
	/// hashes of their content's secret blob keys, which the secrets tell while they are
	/// unlocked: what tells a purge of them while they are locked.
	fn note_secrets(
		&self,
		space: SpaceId,
		(tree, index): (EntityId, &Index),
		state: &TreeState,
		set: &[(String, Entry)],
	) -> Result<(), NodeError> {
		let versions = set.iter().filter_map(|(_, entry)| match entry.version {
			Version::File { content, .. } if state.is_secret(entry.entity) => {
				Some((entry.entity, content))
			}
			_ => None,
		});
		self.note_secret_versions(space, (tree, index), versions)
	}

	/// Records in the index versions of secret files, as [`Library::note_secrets`] does.
	fn note_secret_versions(
		&self,
		space: SpaceId,
		(tree, index): (EntityId, &Index),
		versions: impl IntoIterator<Item = (EntityId, Hash)>,
	) -> Result<(), NodeError> {
		let log = self.space(space)?.stored.log();
		for (entity, content) in versions {
			let sealed: BTreeSet<Hash> = log
				.blob_keys(&content)
				.filter(|(sealed, _)| log.is_secret_blob(sealed))
				.map(|(sealed, _)| *sealed)
				.collect();
			index.note_secret(tree, (entity, content), &sealed)?;
		}
		Ok(())
	}

	/// The paths that NibKeep wrote into a tree's folder for other reasons, which commits
	/// leave out: the copies of checkouts on this device, and those the index marks.
	fn left_out(
		&self,
		space: SpaceId,
		tree: EntityId,
		root: &Path,
		index: &Index,
	) -> Result<BTreeSet<String>, NodeError> {
		let mut left_out: BTreeSet<String> = index.marked(tree)?.into_keys().collect();
		for checkout in self.checkouts(space)? {
			if let Ok(relative) = checkout.path.strip_prefix(root)
				&& let Ok(path) = paths::tree_path_of(relative)
			{
				left_out.insert(path);
			}
		}
		Ok(left_out)
	}

	/// The folders that a tree never takes in: the library's own, whatever its name.
	fn apart(&self) -> Vec<PathBuf> {
		self.library_directory()
			.and_then(|directory| fs::canonicalize(directory).ok())
			.map(super::files::plain)
			.into_iter()
			.collect()
	}

	/// Whether the content that an entry of the index holds was purged. Of an ordinary file:
	/// no key of an ordinary blob opens it any more, and no value of the file, current or
	/// past, holds it. Of a secret file: a purge destroyed every secret blob key whose sealed
	/// hash its copy was written or committed with, which this replica tells with its secrets
	/// locked too; one whose sealed hashes it does not know, it cannot tell, and keeps.
	fn is_purged(
		&self,
		space: SpaceId,
		entry: &Entry,
		secret_versions: &SecretVersions,
	) -> Result<bool, NodeError> {
		let Version::File { content, .. } = entry.version else {
			return Ok(false);
		};
		let log = self.space(space)?.stored.log();
		if let Some(sealed) = secret_versions.get(&(entry.entity, content)) {
			if sealed.is_empty() {
				return Ok(false);
			}
			for sealed in sealed {
				if !nibkeep_log::History::is_blob_purged(log.history(), sealed)? {
					return Ok(false);
				}
			}
			return Ok(true);
		}
		let state = log.state();
		if secret_file_of(state, entry.entity)? != Secrecy::Ordinary {
			return Ok(false);
		}
		if log
			.blob_keys(&content)
			.any(|(sealed, _)| !log.is_secret_blob(sealed))
		{
			return Ok(false);
		}
		for entity in state.merged(state.canonical(entry.entity)?)? {
			for id in state.storage().of_field(entity, &CONTENT)? {
				if state
					.statement(id)?
					.is_some_and(|statement| statement.value == Value::Blob(content))
				{
					return Ok(false);
				}
			}
		}
		Ok(true)
	}

	/// How many blobs of a space were purged, for telling whether a purge came since its
	/// trees' copies were last settled.
	fn purges_of(&self, space: SpaceId) -> Result<usize, NodeError> {
		let log = self.space(space)?.stored.log();
		Ok(nibkeep_log::History::purged_blobs(log.history())?.len())
	}

	/// The contents that this replica learned were purged, having learned first, if a purge
	/// came since it last did, which of the contents of its trees' copies a purge took away.
	/// Purges name what they destroy by sealed hashes, so content is learned from the copies
	/// that held it, and from the purges made here.
	fn learn_purges(&self, space: SpaceId, index: &Index) -> Result<Purged, NodeError> {
		let purges = self.purges_of(space)?;
		let roots = self.roots_here(space)?;
		let mut secret_versions = SecretVersions::new();
		for tree in roots.keys() {
			secret_versions.extend(index.secret_versions(*tree)?);
		}
		if index.settled(space)? != Some(purges) {
			let mut learned = BTreeSet::new();
			let mut learned_secrets = BTreeSet::new();
			for tree in roots.keys() {
				for entry in index.entries(*tree)?.values() {
					if let Version::File { content, .. } = entry.version
						&& self.is_purged(space, entry, &secret_versions)?
					{
						if secret_versions.contains_key(&(entry.entity, content)) {
							learned_secrets.insert(content);
						} else {
							learned.insert(content);
						}
					}
				}
			}
			index.add_purged(&learned)?;
			index.add_purged_secrets(&learned_secrets)?;
			index.set_settled(space, purges)?;
		}
		Ok(Purged {
			ordinary: index.purged()?,
			secret: index.purged_secrets()?,
			secret_versions,
		})
	}

	/// Records contents that a purge made here took away, of ordinary fields or of secret
	/// ones, so that no commit seals them again as such, wherever a copy of them is.
	pub(crate) fn note_purged(
		&self,
		space: SpaceId,
		(contents, secret): (&BTreeSet<Hash>, bool),
	) -> Result<(), NodeError> {
		if contents.is_empty() || self.roots_here(space)?.is_empty() {
			return Ok(());
		}
		let _locked = self.lock_trees()?;
		let index = self.index()?;
		if secret {
			index.add_purged_secrets(contents)
		} else {
			index.add_purged(contents)
		}
	}

	/// The copies of trees' files on this device whose version, as the index says, is one
	/// of `contents`, of secret files if `secret` says so and else of ordinary ones, and which
	/// are there, each with whether it was edited since, as its hash tells: what a purge of
	/// the content deletes, and what it keeps. A purge of one kind leaves the other.
	pub(crate) fn tree_copies_holding(
		&self,
		space: SpaceId,
		contents: &BTreeSet<Hash>,
		secret: bool,
	) -> Result<Vec<(PathBuf, bool)>, NodeError> {
		let roots = self.roots_here(space)?;
		let mut holding = Vec::new();
		if roots.is_empty() || contents.is_empty() {
			return Ok(holding);
		}
		let index = self.index()?;
		for (tree, root) in roots {
			let secret_versions = index.secret_versions(tree)?;
			for (path, entry) in index.entries(tree)? {
				let Version::File { content, .. } = entry.version else {
					continue;
				};
				if !contents.contains(&content)
					|| secret_versions.contains_key(&(entry.entity, content)) != secret
				{
					continue;
				}
				let Located::Here(on_disk) = located(&root, &path, &self.apart()) else {
					continue;
				};
				if let Ok(at) = walk::look(&on_disk)
					&& at.kind == walk::FoundKind::File
				{
					let edited = walk::hash_found(&at).map_or(true, |found| found != content);
					holding.push((on_disk, edited));
				}
			}
		}
		holding.sort();
		Ok(holding)
	}

	/// Settles the copies of a space's trees on this device after purges, made here or
	/// received, as [`settle_tree`] does each tree: only when a purge came since
	/// they were last settled, so that a sync that brings none costs nothing here, and only
	/// when no other program works on them, as a later status or commit settles them too.
	/// Returns the copies it deleted.
	pub(crate) fn settle_trees_in(&self, space: SpaceId) -> Result<Vec<PathBuf>, NodeError> {
		let roots = self.roots_here(space)?;
		if roots.is_empty() {
			return Ok(Vec::new());
		}
		let mut index = self.index()?;
		if index.settled(space)? == Some(self.purges_of(space)?) {
			return Ok(Vec::new());
		}
		let Some(_locked) = self.try_lock_trees()? else {
			return Ok(Vec::new());
		};
		let purged = self.learn_purges(space, &index)?;
		let apart = self.apart();
		let mut deleted = Vec::new();
		for (tree, root) in roots {
			if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
				let state = self.tree_state(space, tree)?;
				let settling = (&purged, &state);
				let place = (tree, root.as_path(), apart.as_slice());
				deleted.extend(settle_tree(place, &mut index, settling)?);
			}
		}
		Ok(deleted)
	}

	/// The commits to a tree that were cut short: those with fewer parts here than their
	/// revisions say, after which the replica that made them made no other commit to the
	/// tree, which would have taken in the rest.
	fn cut_short(&self, space: SpaceId, tree: EntityId) -> Result<Vec<CutShort>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		// Each commit, by its revision: who made it, when, and how many parts it has.
		let mut commits = Vec::new();
		for revision in open.stored.find(&IN_TREE, &Value::Reference(tree))? {
			if !state.types(revision)?.contains(&REVISION) {
				continue;
			}
			let first = |field: &FieldName| -> Result<Option<Statement>, NodeError> {
				Ok(state
					.current(revision, field)?
					.into_iter()
					.min_by_key(|statement| statement.id)
					.map(std::borrow::Cow::into_owned))
			};
			let Some(in_tree) = first(&IN_TREE)? else {
				continue;
			};
			let parts = match first(&PARTS)?.map(|statement| statement.value) {
				Some(Value::Integer(parts)) => u64::try_from(parts).ok(),
				_ => None,
			};
			// By the full time of the change, which orders changes made in one millisecond
			// too.
			let made_at = state.recorded(in_tree.id)?;
			let made_by = open.stored.log().author_of(in_tree.id)?;
			commits.push((revision, made_by, made_at, parts));
		}
		// When each replica made its last commit: only a later commit of the same replica
		// takes in the rest, as another device's knows nothing of it.
		let mut last: Vec<(
			nibkeep_crypto::VerifyingKey,
			Option<nibkeep_time::Timestamp>,
		)> = Vec::new();
		for (_, made_by, made_at, _) in &commits {
			let Some(made_by) = made_by else {
				continue;
			};
			match last.iter_mut().find(|(by, _)| by == made_by) {
				Some((_, at)) => *at = (*at).max(*made_at),
				None => last.push((*made_by, *made_at)),
			}
		}
		let mut cut = Vec::new();
		for (revision, made_by, made_at, parts) in commits {
			let Some(parts) = parts else {
				continue;
			};
			let later = made_by
				.is_some_and(|made_by| last.iter().any(|(by, at)| *by == made_by && *at > made_at));
			if later {
				continue;
			}
			let here = history::parts_here(state, revision)?.len() + 1;
			let made = u64::try_from(here).unwrap_or(u64::MAX);
			if made < parts {
				cut.push(CutShort {
					revision,
					parts,
					made,
				});
			}
		}
		Ok(cut)
	}
}

/// Settles the copy of a tree after purges: deletes the copies whose version holds
/// content in `purged`, where they are as they were written or committed, as their
/// hashes tell, and takes that content out of the index, so that it no longer says what
/// a purged file was; the content stays in the index's list of what was purged, so that
/// no commit seals it again. An edited copy stays. A copy that cannot be reached, read or
/// deleted keeps its entry, and is settled later. The caller holds the lock of the
/// trees. Returns the copies deleted.
fn settle_tree(
	(tree, root, apart): (EntityId, &Path, &[PathBuf]),
	index: &mut Index,
	(purged, state): (&Purged, &TreeState),
) -> Result<Vec<PathBuf>, NodeError> {
	let mut deleted = Vec::new();
	let mut removed = Vec::new();
	if purged.is_empty() {
		return Ok(deleted);
	}
	let entries = index.entries(tree)?;
	// The copies to settle: those whose entries hold purged content, of their kind, and
	// those at the tree's paths whose content is gone, which have none, as when their
	// entries went while their drive was away; these are deleted only if they hold purged
	// content of their file's kind.
	let mut holding: Vec<(String, Option<Hash>, bool)> = entries
		.iter()
		.filter_map(|(path, entry)| match entry.version {
			Version::File { content, .. } if purged.holds(entry) => {
				Some((path.clone(), Some(content), purged.is_secret(entry)))
			}
			_ => None,
		})
		.collect();
	holding.extend(
		state
			.by_path
			.iter()
			.filter(|(path, current)| current.version.is_none() && !entries.contains_key(*path))
			.map(|(path, current)| (path.clone(), None, state.is_secret(current.entity))),
	);
	for (path, content, secret) in holding {
		let on_disk = match located(root, &path, apart) {
			Located::Here(on_disk) => on_disk,
			Located::Absent => {
				removed.push(path);
				continue;
			}
			Located::Unknown(_) => continue,
		};
		let Ok(at) = walk::look(&on_disk) else {
			continue;
		};
		if at.kind != walk::FoundKind::File {
			// Not the purged content: a link, the person's own.
			removed.push(path);
			continue;
		}
		match walk::hash_found(&at) {
			Ok(found)
				if content.map_or(purged.contains(&found, secret), |content| found == content) =>
			{
				// Deleted only if it is still the file that was read, as a save made
				// meanwhile puts another in its place.
				let unchanged = walk::look(&on_disk).is_ok_and(|now| {
					now.identity == at.identity
						&& now.size == at.size
						&& now.modified == at.modified
				});
				if unchanged && fs::remove_file(&on_disk).is_ok() {
					super::checkouts::sync_folder_of(&on_disk);
					deleted.push(on_disk);
					removed.push(path);
				}
			}
			Ok(_) => removed.push(path),
			Err(_) => {}
		}
	}
	index.change(tree, &removed, &[])?;
	Ok(deleted)
}

/// Saves the entries of the folders that a move took a copy from and put it into, before
/// what records the move, which a power cut must not keep without them.
fn save_move(source: &Path, target: &Path) {
	super::checkouts::sync_folder_of(source);
	super::checkouts::sync_folder_of(target);
}

impl Library {
	/// Puts the index in line with what an update, a revert or a commit did to the copy of a
	/// tree before something cut it short, as a crash, which it had not recorded yet, so that
	/// nothing it did looks like a change made here: the copies that moves left aside go
	/// back to their paths, where those are free; a copy that holds the version of a secret
	/// file that an update noted it was about to write is recorded as written, which with
	/// the secrets locked nothing else tells; a file whose version or path this replica's own
	/// commit set last, which the index has otherwise, takes the tree's, and its copy is read
	/// again; and an entry whose copy is gone from its path, of a file or link that the tree
	/// has at another path, where the copy holds the entry's version and the index has
	/// nothing, goes there, as the move it was. Otherwise a copy could look deleted at one
	/// path and new at the other, which a commit would record as another file, or edited
	/// from a version older than the tree's, which a commit would refuse. The caller holds
	/// the lock of the trees.
	fn follow_cut_short(
		&self,
		space: SpaceId,
		(tree, root, apart): (EntityId, &Path, &[PathBuf]),
		index: &mut Index,
		state: &TreeState,
	) -> Result<(), NodeError> {
		let entries = index.entries(tree)?;
		write::bring_back_aside(write::Place { root, apart }, entries.keys());
		// Before anything is read: a copy changed after this is read again.
		let read_at = walk::nanos(std::time::SystemTime::now()).unwrap_or(0);
		let listings = Listings::apart(apart);
		let at = |path: &str| located_in(&listings, root, path);
		let gone = |path: &str| at(path) == Located::Absent;
		let holding = |path: &str, (entity, version): (EntityId, &Version)| {
			let Located::Here(on_disk) = at(path) else {
				return None;
			};
			let found = walk::look(&on_disk).ok()?;
			let holds = write::copy_version(&found)?;
			write::same_version(&found, &holds, version).then(|| Entry {
				entity,
				version: version.clone(),
				size: found.size,
				modified: found.modified,
				read_at,
			})
		};
		let mut removed: Vec<String> = Vec::new();
		let mut set: BTreeMap<String, Entry> = BTreeMap::new();
		let writing = index.writing(tree)?;
		for (path, entity, version) in &writing {
			let recorded = entries
				.get(path)
				.is_some_and(|entry| entry.entity == *entity && entry.version == *version);
			if recorded {
				continue;
			}
			let Some(written) = holding(path, (*entity, version)) else {
				continue;
			};
			// The entry at a path that the file moved from, whose copy went.
			removed.extend(
				entries
					.iter()
					.filter(|(from, entry)| entry.entity == *entity && *from != path && gone(from))
					.map(|(from, _)| from.clone()),
			);
			set.insert(path.clone(), written);
		}
		// What a commit of this replica's made of a copy that changed since its entry: only
		// such a copy can hold what the commit recorded.
		let changed = |path: &str, entry: Option<&Entry>| match (at(path), entry) {
			(Located::Here(on_disk), Some(entry)) => {
				walk::look(&on_disk).is_ok_and(|found| !entry.untouched(found.size, found.modified))
			}
			(Located::Here(_), None) => index.knows(tree).unwrap_or(false),
			(Located::Absent, Some(_)) => true,
			_ => false,
		};
		for (path, entity, version) in self.committed_here(space, &entries, state, &changed)? {
			let from = entries
				.iter()
				.find(|(_, entry)| entry.entity == entity)
				.map(|(from, _)| from.clone());
			let moved = from.as_ref().filter(|from| **from != path);
			if set.contains_key(&path)
				|| moved.is_some_and(|from| !gone(from) || entries.contains_key(&path))
			{
				continue;
			}
			removed.extend(moved.cloned());
			// Read again, as what the copy holds now the index cannot tell.
			let entry = Entry {
				entity,
				version,
				size: 0,
				modified: None,
				read_at,
			};
			set.insert(path, entry);
		}
		for (path, entry) in &entries {
			let Some(now_at) = state.by_entity.get(&entry.entity) else {
				continue;
			};
			if now_at == path
				|| entries.contains_key(now_at)
				|| set.contains_key(now_at)
				|| removed.contains(path)
				|| !gone(path)
			{
				continue;
			}
			let Some(moved) = holding(now_at, (entry.entity, &entry.version)) else {
				continue;
			};
			removed.push(path.clone());
			set.insert(now_at.clone(), moved);
		}
		let set: Vec<(String, Entry)> = set.into_iter().collect();
		index.change(tree, &removed, &set)?;
		// Which of them are secret files, as the update or the commit would have noted.
		self.note_secrets(space, (tree, index), state, &set)?;
		if !writing.is_empty() {
			index.forget_writing(tree)?;
		}
		Ok(())
	}

	/// The files and links of a tree whose version or path the index has otherwise than the
	/// tree, or not at all, where the copy `changed` since its entry, of which a part of this
	/// replica's own commit set the version and path last, rather than any other change: each
	/// with its path and version in the tree. Files whose versions or paths compete are left
	/// out.
	fn committed_here(
		&self,
		space: SpaceId,
		entries: &BTreeMap<String, Entry>,
		state: &TreeState,
		changed: &dyn Fn(&str, Option<&Entry>) -> bool,
	) -> Result<Vec<(String, EntityId, Version)>, NodeError> {
		let indexed: BTreeMap<EntityId, (&String, &Entry)> = entries
			.iter()
			.map(|(path, entry)| (entry.entity, (path, entry)))
			.collect();
		let differing: Vec<(String, EntityId, Version)> = state
			.by_entity
			.iter()
			.filter(|(entity, path)| {
				!state.competing.contains(*path)
					&& !state.paths.contains_key(*entity)
					&& state
						.versions
						.get(*entity)
						.is_none_or(|versions| versions.len() < 2)
			})
			.filter_map(|(entity, path)| {
				let version = state.own.get(entity).cloned().flatten()?;
				let differs = match indexed.get(entity) {
					Some((at, entry)) => {
						(*at != path || entry.version != version) && changed(at, Some(entry))
					}
					None => changed(path, None),
				};
				differs.then(|| (path.clone(), *entity, version))
			})
			.collect();
		if differing.is_empty() {
			return Ok(differing);
		}
		let open = self.space(space)?;
		let log = open.stored.log();
		let state_now = log.state();
		let me = self.replica.signing.verifying_key();
		let change_of = |id| -> Result<Option<Hash>, NodeError> {
			Ok(state_now
				.storage()
				.get(id)?
				.and_then(|recorded| recorded.change))
		};
		let fields = [
			&TREE_PATH,
			&CONTENT,
			&SECRET_CONTENT,
			&LINK_TARGET,
			&EXECUTABLE,
		];
		let mut own = Vec::new();
		for (path, entity, version) in differing {
			// The newest of what makes its path and version, which a part of a commit made
			// with the commit's revision: not another change of this replica's.
			let mut last = None;
			for field in fields {
				for statement in state_now.current(entity, field)? {
					let at = state_now.recorded(statement.id)?;
					if last.as_ref().is_none_or(|(when, _)| at > *when) {
						last = Some((at, statement.id));
					}
				}
			}
			let Some((_, newest)) = last else {
				continue;
			};
			let change = change_of(newest)?;
			let mut in_commit = false;
			for part in state_now.current(entity, &COMMITTED_IN)? {
				in_commit |= change.is_some() && change_of(part.id)? == change;
			}
			if in_commit && log.author_of(newest)? == Some(me) {
				own.push((path, entity, version));
			}
		}
		Ok(own)
	}
}

/// What this replica learned was purged of the contents of its trees' copies: of ordinary
/// files and of secret files apart, as a purge of one kind leaves the same content in the
/// other ([technical design: secret files]).
///
/// [technical design: secret files]: ../../../../docs/Technical-design.md#95-secret-files
#[derive(Debug, Default)]
struct Purged {
	ordinary: BTreeSet<Hash>,
	secret: BTreeSet<Hash>,
	/// The versions of secret files that this replica's copies were written or committed at,
	/// which tell an entry of a secret file from one of an ordinary file.
	secret_versions: SecretVersions,
}

impl Purged {
	/// Whether nothing was purged.
	fn is_empty(&self) -> bool {
		self.ordinary.is_empty() && self.secret.is_empty()
	}

	/// Whether content was purged from fields of the kind given, secret or ordinary.
	fn contains(&self, content: &Hash, secret: bool) -> bool {
		if secret {
			self.secret.contains(content)
		} else {
			self.ordinary.contains(content)
		}
	}

	/// Whether an entry of the index is of a secret file's version.
	fn is_secret(&self, entry: &Entry) -> bool {
		matches!(entry.version, Version::File { content, .. }
			if self.secret_versions.contains_key(&(entry.entity, content)))
	}

	/// Whether the version that an entry of the index holds was purged.
	fn holds(&self, entry: &Entry) -> bool {
		match entry.version {
			Version::File { content, .. } => self.contains(&content, self.is_secret(entry)),
			Version::Link { .. } => false,
		}
	}
}

/// The path and version of an entity in a state, if the state holds it at a path; of a file
/// that competes for its path with another, the version is the path's.
fn version_of(state: &TreeState, entity: EntityId) -> Option<(String, Option<Version>)> {
	let path = state.by_entity.get(&entity)?;
	Some((path.clone(), state.own.get(&entity).cloned().flatten()))
}

/// The entities that are in a tree, deleted ones too, as the state the space has now
/// says: every state of the tree is read for them, as an entity never leaves its tree.
fn tree_entities<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	tree: EntityId,
) -> Result<Vec<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	let tree = state.canonical(tree)?;
	let mut entities = BTreeSet::new();
	// The value index leaves deleted entities out, so the statements that refer to the tree
	// are read instead, current or past.
	for id in state.storage().referring_to(tree)? {
		if let Some(recorded) = state.storage().get(id)?
			&& recorded.statement.field == IN_TREE
		{
			entities.insert(state.canonical(recorded.statement.entity)?);
		}
	}
	Ok(entities.into_iter().collect())
}

/// What the log tells of its changes, for telling a deletion that saw a version from one that
/// did not: the changes that each change builds on, found once for each, and whether a purge
/// took a change away.
pub(super) struct Changes<'a> {
	log: &'a Log<StateFile, LogFile>,
	built_on: RefCell<BTreeMap<Hash, BTreeSet<Hash>>>,
}

impl<'a> Changes<'a> {
	/// What `log` tells of its changes.
	pub(super) fn of(log: &'a Log<StateFile, LogFile>) -> Self {
		Self {
			log,
			built_on: RefCell::default(),
		}
	}

	/// Whether the change whose hash is `later` is the one whose hash is `earlier`, or
	/// builds on it.
	fn saw(&self, earlier: Hash, later: Hash) -> Result<bool, NodeError> {
		if earlier == later {
			return Ok(true);
		}
		if let Some(built_on) = self.built_on.borrow().get(&later) {
			return Ok(built_on.contains(&earlier));
		}
		let mut built_on = BTreeSet::new();
		let mut waiting = vec![ChangeId::from_hash(later)];
		while let Some(id) = waiting.pop() {
			let Some(change) = self.log.change(id)? else {
				continue;
			};
			for parent in change.parents() {
				if built_on.insert(*parent.hash()) {
					waiting.push(*parent);
				}
			}
		}
		let saw = built_on.contains(&earlier);
		self.built_on.borrow_mut().insert(later, built_on);
		Ok(saw)
	}

	/// Whether a purge took away the change whose hash is `change`, stating again what of it
	/// stays.
	fn purged(&self, change: Hash) -> Result<bool, NodeError> {
		let status = nibkeep_log::History::status(self.log.history(), ChangeId::from_hash(change))?;
		Ok(status == Some(nibkeep_log::Status::Ignored))
	}
}

/// The files and links among `entities`, as `state` says, as [`Library::tree_state`] reads
/// them; and the deleted ones, by their last paths. With `changes`, a file deleted while
/// another device gave it a new version, moved it or changed its executable bit, before
/// either saw the other, stays among the competing. Without `readable`, a secret file whose content shows not is sealed, rather
/// than purged.
fn read_tree_state<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	tree: EntityId,
	entities: &[EntityId],
	(changes, readable): (Option<&Changes<'_>>, bool),
) -> Result<TreeState, NodeError>
where
	NodeError: From<S::Error>,
{
	let tree = state.canonical(tree)?;
	let mut read = TreeState::default();
	// Of the deleted at each path, the one deleted last, with when.
	let mut deleted_at: BTreeMap<String, nibkeep_time::Timestamp> = BTreeMap::new();
	for &entity in entities {
		let Some((is_link, is_secret)) = kind_in_tree(state, entity, tree)? else {
			continue;
		};
		let values = |field: &FieldName| -> Result<Vec<Value>, NodeError> {
			let mut statements = state.current(entity, field)?;
			statements.sort_by_key(|statement| statement.id);
			Ok(statements
				.into_iter()
				.map(|statement| statement.value.clone())
				.collect())
		};
		let texts = |field: &FieldName| -> Result<Vec<String>, NodeError> {
			Ok(values(field)?
				.into_iter()
				.filter_map(|value| match value {
					Value::Text(text) => Some(text),
					_ => None,
				})
				.collect())
		};
		let paths = distinct(texts(&TREE_PATH)?);
		if is_secret {
			read.secret.insert(entity);
			read.secret_paths.extend(all_paths(state, entity)?);
		}
		let Some(path) = paths.first().cloned() else {
			continue;
		};
		if !is_safe_tree_path(&path) {
			continue;
		}
		let all: Vec<Version> = if is_link {
			texts(&LINK_TARGET)?
				.into_iter()
				.map(|target| Version::Link { target })
				.collect()
		} else {
			let executable = values(&EXECUTABLE)?.contains(&Value::Boolean(true));
			let field = if is_secret { &SECRET_CONTENT } else { &CONTENT };
			values(field)?
				.into_iter()
				.filter_map(|value| match value {
					Value::Blob(content) => Some(Version::File {
						content,
						executable,
					}),
					_ => None,
				})
				.collect()
		};
		let all = distinct(all);
		let competing = all.len() > 1;
		let version = all.first().cloned();
		let deleted = state.is_deleted(entity)?;
		if is_secret && version.is_none() && !deleted && !readable {
			// Sealed, not purged: which version the tree holds, the secrets alone tell.
			read.sealed.insert(path, entity);
			continue;
		}
		let unseen = match changes {
			// A file whose content was purged has no version to keep.
			Some(changes) if deleted && version.is_some() => {
				unseen_deletions(state, entity, changes)?
			}
			_ => None,
		};
		if let Some(deletions) = unseen {
			read.contested.insert(entity, deletions);
			read.competing.insert(path.clone());
		} else if deleted {
			note_deleted(
				state,
				(&mut read, &mut deleted_at),
				path,
				Current { entity, version },
			)?;
			continue;
		}
		// The paths of a file whose paths compete are all competing, its own among them.
		if competing {
			read.competing.insert(path.clone());
			read.versions.insert(entity, all);
		}
		read.own.insert(entity, version.clone());
		if paths.len() > 1 {
			read.competing.extend(paths.iter().cloned());
			read.paths.insert(entity, paths.clone());
		}
		if read.by_path.contains_key(&path) {
			read.competing.insert(path.clone());
		} else {
			read.by_path
				.insert(path.clone(), Current { entity, version });
		}
		read.by_entity.insert(entity, path);
	}
	Ok(read)
}

/// The values, each once, in their order: one value stated twice, as by two devices that
/// settled one competition apart, is one value, not two that compete.
fn distinct<T: PartialEq>(values: Vec<T>) -> Vec<T> {
	let mut kept = Vec::new();
	for value in values {
		if !kept.contains(&value) {
			kept.push(value);
		}
	}
	kept
}

/// Notes a deleted file or link at its last path, unless one deleted later was there.
fn note_deleted<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	(read, deleted_at): (
		&mut TreeState,
		&mut BTreeMap<String, nibkeep_time::Timestamp>,
	),
	path: String,
	deleted: Current,
) -> Result<(), NodeError>
where
	NodeError: From<S::Error>,
{
	let when = state
		.current(deleted.entity, &fields::DELETED)?
		.iter()
		.filter_map(|statement| state.recorded(statement.id).ok().flatten())
		.max();
	let later = match (when, deleted_at.get(&path)) {
		(Some(when), Some(known)) => when > *known,
		(Some(_) | None, None) => true,
		(None, Some(_)) => false,
	};
	if later {
		if let Some(when) = when {
			deleted_at.insert(path.clone(), when);
		}
		read.deleted.insert(path, deleted);
	}
	Ok(())
}

/// The keys of the deletions of a deleted file or link, if it has a version, a path or an
/// executable bit that none of them saw: one recorded by a change that no change that deleted
/// it builds on, as when one device deleted it while another gave it a new version, moved it
/// or took its bit away, before either saw the other. A version that a purge stated again
/// counts as seen, whichever change it came in.
fn unseen_deletions<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	entity: EntityId,
	changes: &Changes<'_>,
) -> Result<Option<Vec<String>>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut deletions = Vec::new();
	for statement in state.current(entity, &fields::DELETED)? {
		if statement.value != Value::Boolean(true) {
			continue;
		}
		// A deletion that came in no change saw everything.
		let Some(change) = state
			.storage()
			.get(statement.id)?
			.and_then(|recorded| recorded.change)
		else {
			return Ok(None);
		};
		deletions.push((deletion_key(statement.id), change));
	}
	if deletions.is_empty() {
		return Ok(None);
	}
	for field in [
		&CONTENT,
		&SECRET_CONTENT,
		&LINK_TARGET,
		&EXECUTABLE,
		&TREE_PATH,
	] {
		for statement in state.current(entity, field)? {
			let Some(recorded) = state.storage().get(statement.id)? else {
				continue;
			};
			let (Some(change), None) = (recorded.change, recorded.restated) else {
				continue;
			};
			if changes.purged(change)? {
				continue;
			}
			let mut seen = false;
			for (_, deletion) in &deletions {
				if changes.saw(change, *deletion)? {
					seen = true;
					break;
				}
			}
			if !seen {
				return Ok(Some(deletions.into_iter().map(|(key, _)| key).collect()));
			}
		}
	}
	Ok(None)
}

/// What an entity is in a tree, if it is a file or a link of it: whether it is a link, and
/// whether it is a secret file. A secret file is the tree's whose commit made it, for good,
/// whatever changes said later of its tree or its types, as any replica may say them; an
/// entity once in the tree whose tree is another now is the other's.
fn kind_in_tree<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	entity: EntityId,
	tree: EntityId,
) -> Result<Option<(bool, bool)>, NodeError>
where
	NodeError: From<S::Error>,
{
	match secret_file_of(state, entity)? {
		Secrecy::Of(of) if of == tree => return Ok(Some((false, true))),
		Secrecy::Of(_) | Secrecy::OfNone => return Ok(None),
		Secrecy::Ordinary => {}
	}
	let mut in_tree = false;
	for statement in state.current(entity, &IN_TREE)? {
		if let Value::Reference(of) = statement.value {
			in_tree |= state.canonical(of)? == tree;
		}
	}
	if !in_tree {
		return Ok(None);
	}
	let types = state.types(entity)?;
	let is_link = types.contains(&SYMBOLIC_LINK);
	Ok((is_link || types.contains(&FILE)).then_some((is_link, false)))
}

/// The earliest of some statements of an entity, by time and then ID, with its value and
/// the change it came in.
type Earliest = Option<(
	(Option<nibkeep_time::Timestamp>, nibkeep_base::StatementId),
	Value,
	Option<Hash>,
)>;

/// Whether an entity is a secret file, and of which tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Secrecy {
	/// It never was a secret file.
	Ordinary,
	/// A secret file of the tree whose commit made it.
	Of(EntityId),
	/// A secret file that no commit of a tree made, as a password record's, which a
	/// change took into a tree later: never a tree's.
	OfNone,
}

/// Whether an entity is a secret file, as its history says: one that any statement, current
/// or past, gave the type of secret files is one for good. It is the tree's that the first
/// statement of its tree names, if the change that made the entity stated it too, as a
/// commit does; else no tree's. So a file of a tree that a change made secret later stays
/// in its tree, and is never sealed as ordinary again.
fn secret_file_of<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	entity: EntityId,
) -> Result<Secrecy, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	// The earliest of some statements of the entity, with the change it came in.
	let earliest = |field: &FieldName,
	                keep: &dyn Fn(&Value) -> bool|
	 -> Result<Option<(Value, Option<Hash>)>, NodeError> {
		let mut first: Earliest = None;
		for id in storage.of_field(entity, field)? {
			let Some(recorded) = storage.get(id)? else {
				continue;
			};
			if !keep(&recorded.statement.value) {
				continue;
			}
			let at = (state.recorded(id)?, id);
			if first.as_ref().is_none_or(|(known, _, _)| at < *known) {
				first = Some((at, recorded.statement.value.clone(), recorded.change));
			}
		}
		Ok(first.map(|(_, value, change)| (value, change)))
	};
	let secret_type = SECRET_FILE;
	if earliest(&fields::TYPE, &|value| {
		value.as_text() == Some(secret_type.as_str())
	})?
	.is_none()
	{
		return Ok(Secrecy::Ordinary);
	}
	// The change that made the entity, which gave it its first type.
	let Some((_, made_in)) = earliest(&fields::TYPE, &|_| true)? else {
		return Ok(Secrecy::OfNone);
	};
	match earliest(&IN_TREE, &|value| matches!(value, Value::Reference(_)))? {
		Some((Value::Reference(tree), Some(change))) if made_in == Some(change) => {
			Ok(Secrecy::Of(state.canonical(tree)?))
		}
		_ => Ok(Secrecy::OfNone),
	}
}

/// Every path that a file of a tree had, current or past.
fn all_paths<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	entity: EntityId,
) -> Result<Vec<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	let mut paths = Vec::new();
	for id in storage.of_field(entity, &TREE_PATH)? {
		if let Some(recorded) = storage.get(id)?
			&& let Value::Text(path) = &recorded.statement.value
		{
			paths.push(path.clone());
		}
	}
	Ok(paths)
}

/// The key of a deletion of a file among its competing values, as written out: no version's
/// key is one.
fn deletion_key(statement: nibkeep_base::StatementId) -> String {
	format!("deleted {statement}")
}

/// The path in the tree at `root` of what is at `path`, a full path, whether or not it is
/// there: its folder as the file system resolves the part of it that is there, and the rest
/// as written, relative to the root; empty for the root itself.
///
/// # Errors
///
/// Returns [`NodeError::NotInTree`] if the path is not in the tree's folder, and
/// [`NodeError::UnsafePath`] if it is no path a tree may hold.
pub fn tree_path_at(root: &Path, path: &Path) -> Result<String, NodeError> {
	let shown = || path.display().to_string();
	// A link is named as itself, never as what it points to: its folder is resolved, and
	// its own name kept.
	let mut existing = path;
	let mut rest = Vec::new();
	if let (Some(std::path::Component::Normal(name)), Some(parent)) =
		(path.components().next_back(), path.parent())
	{
		rest.push(name);
		existing = parent;
	}
	let resolved = loop {
		if let Some(resolved) = resolved(existing) {
			break resolved;
		}
		rest.push(
			existing
				.file_name()
				.ok_or_else(|| NodeError::NotInTree(shown()))?,
		);
		existing = existing
			.parent()
			.ok_or_else(|| NodeError::NotInTree(shown()))?;
	};
	let mut full = resolved;
	full.extend(rest.iter().rev());
	let relative = full
		.strip_prefix(root)
		.map_err(|_| NodeError::NotInTree(shown()))?;
	if relative.as_os_str().is_empty() {
		return Ok(String::new());
	}
	paths::tree_path_of(relative).map_err(|_| NodeError::UnsafePath(shown()))
}

/// A folder as the file system resolves it, without Windows' prefix of long paths.
fn resolved(folder: &Path) -> Option<PathBuf> {
	fs::canonicalize(folder).ok().map(super::files::plain)
}
