//! The Git repositories in a tree: references, recorded by their remotes, branches, the
//! remote that a branch follows, and the commit only if the person pinned it, as `nib` reads
//! them from the person's own Git; never file by file. The engine knows nothing of Git: it
//! records what `nib` gives it, with no credential, and says what a clone may take from what
//! other devices recorded ([technical design: trees]).
//!
//! Each copy keeps what Git said of each repository when it last recorded or cloned it, so
//! that a commit records only what changed here since: devices whose clones differ, as one
//! that took fewer remotes, never undo each other's records.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Name, Operation, State, Value, fields};
use nibkeep_std::{
	GIT_BRANCH, GIT_COMMIT, GIT_REMOTE, GIT_REPOSITORY, GIT_UPSTREAM, IN_TREE, TREE_PATH,
};

use super::commit::{clearing, deleting, moving, replacing};
use super::git::{cloneable, is_commit, is_safe_name, recordable_address};
use super::index::Index;
use super::locate::{Blocked, Listings, Located, holds_git, located, place_for_new};
use super::walk::walk;
use super::{Change, Committed, TREE_TYPES, TreeChange, is_safe_tree_path, tree_entities};
use crate::NodeError;
use crate::library::Library;
use crate::library::entities::NewValue;
use crate::library::policies;

/// What Git says of a repository in a tree's copy: what `nib` reads from the person's Git,
/// and gives a commit to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepository {
	/// The folder that holds it, as a path in the tree.
	pub path: String,
	/// Its remotes, each by its name and address, credentials and all: a commit takes them
	/// out.
	pub remotes: Vec<(String, String)>,
	/// The branch it has checked out, if any.
	pub branch: Option<String>,
	/// The name of the remote that the branch follows, if it follows one.
	pub upstream: Option<String>,
	/// The commit it has checked out, in full, which a commit records if it is pinned.
	pub commit: Option<String>,
}

/// A Git repository that a tree records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRepository {
	/// Its entity.
	pub entity: EntityId,
	/// Its folder, as a path in the tree.
	pub path: String,
	/// Its remotes, each by its name and address, ordered by name.
	pub remotes: Vec<(String, String)>,
	/// Its branch.
	pub branch: Option<String>,
	/// The remote that its branch follows.
	pub upstream: Option<String>,
	/// The commit it is pinned at, if the person pinned it.
	pub pinned: Option<String>,
	/// Whether this device's copy of the tree holds it.
	pub here: bool,
}

/// A repository that this device's copy of a tree does not hold: what a clone would take,
/// and where it comes from, or why it may not be cloned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToClone {
	/// Its entity, which [`Library::prepare_clone`] and [`Library::repository_cloned`] take.
	pub entity: EntityId,
	/// Its folder, as a path in the tree.
	pub path: String,
	/// The remote to clone from: of those a clone may take, the one its branch follows, or
	/// else `origin`, or else the first by name.
	pub remote: String,
	/// That remote's address.
	pub address: String,
	/// The other remotes to add, those a clone may take.
	pub others: Vec<(String, String)>,
	/// The branch to check out.
	pub branch: Option<String>,
	/// The commit to check out the branch at, if it is pinned.
	pub pinned: Option<String>,
	/// The device whose replica recorded the address, as the space names it.
	pub recorded_by: String,
	/// Why it may not be cloned, if it may not.
	pub refused: Option<String>,
}

/// The folder that a clone goes into, made empty, with the folders made for it: what to
/// remove, from the innermost, if the clone fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneFolder {
	/// The folder, empty.
	pub folder: PathBuf,
	/// The folders made to hold it, from the outermost, none of which was there.
	pub made: Vec<PathBuf>,
}

/// A repository as the space records it, with the statements that say so.
#[derive(Clone, Debug)]
pub(super) struct Recorded {
	pub(super) entity: EntityId,
	pub(super) path: String,
	/// Its remotes, by name, each with every address recorded for it and its statement,
	/// competing values too, the smallest statement's first: the one a clone takes.
	remotes: BTreeMap<String, Vec<(String, StatementId)>>,
	branch: Option<String>,
	upstream: Option<String>,
	pinned: Option<String>,
	/// Every statement that says what it is, where, or what a clone takes: who made them
	/// tells whether a clone may trust them.
	statements: Vec<StatementId>,
}

/// What Git said of a repository when this copy last recorded or cloned it: what a commit
/// tells what changed here from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Snapshot {
	remotes: BTreeMap<String, String>,
	branch: Option<String>,
	upstream: Option<String>,
	head: Option<String>,
}

impl Snapshot {
	/// What Git says now.
	fn of(repository: &GitRepository) -> Self {
		let remotes = recordable_remotes(repository);
		// Git takes an address there too: only a remote's name is recorded.
		let upstream = repository
			.upstream
			.clone()
			.filter(|upstream| remotes.contains_key(upstream));
		Self {
			remotes,
			branch: repository.branch.clone(),
			upstream,
			head: repository.commit.clone(),
		}
	}

	/// As the index keeps it: a line for each remote, then the branch, upstream and commit,
	/// none of which holds a line break.
	fn to_record(&self) -> String {
		let mut lines: Vec<String> = self
			.remotes
			.iter()
			.map(|(name, address)| format!("remote {name} {address}"))
			.collect();
		for (key, value) in [
			("branch", &self.branch),
			("upstream", &self.upstream),
			("head", &self.head),
		] {
			if let Some(value) = value.as_ref().filter(|value| !value.contains('\n')) {
				lines.push(format!("{key} {value}"));
			}
		}
		lines.join("\n")
	}

	/// As the index keeps it.
	fn from_record(record: &str) -> Self {
		let mut snapshot = Self::default();
		for line in record.lines() {
			let Some((key, rest)) = line.split_once(' ') else {
				continue;
			};
			match key {
				"remote" => {
					if let Some((name, address)) = rest.split_once(' ') {
						snapshot.remotes.insert(name.to_owned(), address.to_owned());
					}
				}
				"branch" => snapshot.branch = Some(rest.to_owned()),
				"upstream" => snapshot.upstream = Some(rest.to_owned()),
				"head" => snapshot.head = Some(rest.to_owned()),
				_ => {}
			}
		}
		snapshot
	}
}

/// What a commit records of a tree's repositories, and what it changes in the index.
#[derive(Default)]
pub(super) struct Plan {
	/// What it records, by the repository each is about.
	pub(super) operations: Vec<(EntityId, Vec<Operation>)>,
	pub(super) changes: Vec<TreeChange>,
	/// The repositories this copy holds after it, by path, with what Git said of each.
	pub(super) hold: Vec<(String, EntityId, String)>,
	/// The paths of repositories it no longer holds.
	pub(super) forget: Vec<String>,
}

/// Where a commit's plan of repositories looks: the copy's folder, the folders apart, and
/// the paths chosen, all of them if empty.
pub(super) struct Scope<'a> {
	pub(super) root: &'a Path,
	pub(super) apart: &'a [PathBuf],
	pub(super) paths: &'a [String],
}

/// The repositories that a tree records, by their entities, in the order of their paths.
pub(super) fn read_repositories<S: Statements>(
	state: &State<S>,
	tree: EntityId,
	entities: &[EntityId],
) -> Result<Vec<Recorded>, NodeError>
where
	NodeError: From<S::Error>,
{
	let tree = state.canonical(tree)?;
	let mut recorded = Vec::new();
	for &entity in entities {
		if !state.types(entity)?.contains(&GIT_REPOSITORY) || state.is_deleted(entity)? {
			continue;
		}
		let mut in_tree = false;
		for statement in state.current(entity, &IN_TREE)? {
			if let Value::Reference(of) = statement.value {
				in_tree |= state.canonical(of)? == tree;
			}
		}
		if !in_tree {
			continue;
		}
		let first = |field: &FieldName| -> Result<Option<String>, NodeError> {
			let mut statements = state.current(entity, field)?;
			statements.sort_by_key(|statement| statement.id);
			Ok(statements
				.into_iter()
				.find_map(|statement| match &statement.value {
					Value::Text(text) => Some(text.clone()),
					_ => None,
				}))
		};
		let Some(path) = first(&TREE_PATH)? else {
			continue;
		};
		let mut remotes: BTreeMap<String, Vec<(String, StatementId)>> = BTreeMap::new();
		let mut current = state.current(entity, &GIT_REMOTE)?;
		current.sort_by_key(|statement| statement.id);
		for statement in current {
			if let Value::Text(text) = &statement.value
				&& let Some((name, address)) = text.split_once(' ')
			{
				remotes
					.entry(name.to_owned())
					.or_default()
					.push((address.to_owned(), statement.id));
			}
		}
		let mut statements = Vec::new();
		for field in [
			IN_TREE,
			TREE_PATH,
			GIT_REMOTE,
			GIT_BRANCH,
			GIT_UPSTREAM,
			GIT_COMMIT,
		] {
			statements.extend(
				state
					.current(entity, &field)?
					.iter()
					.map(|statement| statement.id),
			);
		}
		recorded.push(Recorded {
			entity,
			path,
			remotes,
			branch: first(&GIT_BRANCH)?,
			upstream: first(&GIT_UPSTREAM)?,
			pinned: first(&GIT_COMMIT)?,
			statements,
		});
	}
	recorded.sort_by(|a, b| a.path.cmp(&b.path));
	Ok(recorded)
}

/// The remotes of what Git says, as they are recorded: with no credential, by name, and
/// none whose name could not be told from its address, nor any with a control character.
fn recordable_remotes(repository: &GitRepository) -> BTreeMap<String, String> {
	let plain = |text: &str| !text.chars().any(char::is_control);
	repository
		.remotes
		.iter()
		.filter(|(name, address)| {
			!name.is_empty() && !name.contains(char::is_whitespace) && plain(name) && plain(address)
		})
		.map(|(name, address)| (name.clone(), recordable_address(address)))
		.filter(|(_, address)| !address.contains(char::is_whitespace))
		.collect()
}

/// Adds to `plan` what records what changed of a repository here since `before`: remotes
/// added, changed or removed here, the branch and upstream if they changed here, and the
/// commit of a pinned one if it moved here. What changed elsewhere stays.
fn changed<S: Statements>(
	state: &State<S>,
	(recorded, repository, before): (&Recorded, &GitRepository, &Snapshot),
	plan: &mut Plan,
	rng: &mut impl CryptoRng,
) -> Result<(), NodeError>
where
	NodeError: From<S::Error>,
{
	let entity = recorded.entity;
	let now = Snapshot::of(repository);
	let mut operations = Vec::new();
	// Only the remotes added, changed or removed here: other devices' stay, competing
	// values too, unless this one records the name again.
	let mut retracted = Vec::new();
	for (name, address) in &now.remotes {
		if before.remotes.get(name) == Some(address) {
			continue;
		}
		let values = recorded.remotes.get(name).map_or_default(Vec::as_slice);
		retracted.extend(
			values
				.iter()
				.filter(|(other, _)| other != address)
				.map(|(_, statement)| *statement),
		);
		if !values.iter().any(|(other, _)| other == address) {
			let value = Value::Text(format!("{name} {address}"));
			operations.push(policies::assert(entity, GIT_REMOTE, value, rng));
		}
	}
	for name in before.remotes.keys() {
		if !now.remotes.contains_key(name)
			&& let Some(values) = recorded.remotes.get(name)
		{
			retracted.extend(values.iter().map(|(_, statement)| *statement));
		}
	}
	if !retracted.is_empty() {
		operations.push(Operation::retract(entity, retracted));
	}
	for (field, here, then, recorded) in [
		(GIT_BRANCH, &now.branch, &before.branch, &recorded.branch),
		(
			GIT_UPSTREAM,
			&now.upstream,
			&before.upstream,
			&recorded.upstream,
		),
	] {
		if here != then && here != recorded {
			match here {
				Some(value) => {
					let value = Value::Text(value.clone());
					operations.push(replacing(state, entity, field, value, rng)?);
				}
				None => operations.extend(clearing(state, entity, &field)?),
			}
		}
	}
	if recorded.pinned.is_some()
		&& let Some(commit) = now.head.as_ref().filter(|commit| is_commit(commit))
		&& before.head.as_ref() != Some(commit)
		&& recorded.pinned.as_ref() != Some(commit)
	{
		let value = Value::Text(commit.clone());
		operations.push(replacing(state, entity, GIT_COMMIT, value, rng)?);
	}
	if !operations.is_empty() {
		plan.operations.push((entity, operations));
		plan.changes.push(TreeChange {
			path: repository.path.clone(),
			change: Change::Edited,
		});
	}
	Ok(())
}

/// Whether a path of a tree is inside a folder that holds a Git repository here, or that
/// the tree records as one, or one the index marks: a clone never goes into another.
fn inside_repository(
	root: &Path,
	path: &str,
	(recorded, marked): (&[Recorded], &BTreeMap<String, String>),
) -> bool {
	folders_of(path).any(|folder| {
		marked.contains_key(folder)
			|| recorded.iter().any(|other| other.path == folder)
			|| fs::symlink_metadata(root.join(folder).join(".git")).is_ok()
	})
}

/// Whether the repository this copy held at `path` is surely gone from here, as the disk
/// says: no folder there, or one with no `.git` any longer. A folder that cannot be
/// read says nothing.
fn gone_here(scope: &Scope<'_>, path: &str) -> bool {
	match located(scope.root, path, scope.apart) {
		Located::Absent => true,
		Located::Here(folder) => {
			fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir())
				&& matches!(
					fs::symlink_metadata(folder.join(".git")),
					Err(error) if error.kind() == std::io::ErrorKind::NotFound
				)
		}
		Located::Unknown(_) => false,
	}
}

/// The last name of a tree's path.
fn last_name(path: &str) -> &str {
	path.rsplit('/').next().unwrap_or(path)
}

/// The folders that hold `path` in a tree, from the outermost.
fn folders_of(path: &str) -> impl Iterator<Item = &str> {
	path.match_indices('/').map(move |(at, _)| &path[..at])
}

/// Whether `path` is one of `paths`, or in a folder that one of them names.
fn within(path: &str, paths: &[String]) -> bool {
	paths.is_empty()
		|| paths.iter().any(|given| {
			given.is_empty() || path == given || path.starts_with(&format!("{given}/"))
		})
}

impl Library {
	/// The repositories that a tree records, in the order of their paths, each with whether
	/// this device's copy holds it.
	///
	/// # Errors
	///
	/// Returns an error if the space or the index cannot be read.
	pub fn tree_repositories(
		&self,
		space: SpaceId,
		tree: EntityId,
	) -> Result<Vec<RecordedRepository>, NodeError> {
		let held: BTreeSet<EntityId> = match self.index_if_any()? {
			Some(index) => index
				.repositories(tree)?
				.into_values()
				.map(|(entity, _)| entity)
				.collect(),
			None => BTreeSet::new(),
		};
		Ok(self
			.recorded_repositories(space, tree)?
			.into_iter()
			.map(|recorded| RecordedRepository {
				entity: recorded.entity,
				here: held.contains(&recorded.entity),
				path: recorded.path,
				remotes: recorded
					.remotes
					.into_iter()
					.filter_map(|(name, values)| {
						values
							.into_iter()
							.next()
							.map(|(address, _)| (name, address))
					})
					.collect(),
				branch: recorded.branch,
				upstream: recorded.upstream,
				pinned: recorded.pinned,
			})
			.collect())
	}

	/// The folders of the copy of a tree on this device that hold Git repositories, by
	/// their paths in the tree: what `nib` asks Git about before a commit. The folder is
	/// walked, and nothing is read.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoTreeHere`] if the tree has no folder on this device, and an
	/// error if the space or the index cannot be read.
	pub fn tree_repository_folders(
		&self,
		space: SpaceId,
		tree: EntityId,
	) -> Result<Vec<String>, NodeError> {
		let root = self.root_here(space, tree)?;
		let index = self.index()?;
		let left_out = self.left_out(space, tree, &root, &index)?;
		Ok(walk(&root, &left_out, &self.apart()).repositories)
	}

	/// The repositories that a tree records, as the space says.
	pub(super) fn recorded_repositories(
		&self,
		space: SpaceId,
		tree: EntityId,
	) -> Result<Vec<Recorded>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			let entities = tree_entities(state, tree)?;
			read_repositories(state, tree, &entities)
		})
	}

	/// What a commit would record of a tree's repositories, for a status: of those `found`
	/// in the copy, what Git says of them, in `described`, as [`Library::commit_tree`]
	/// records it with [`Commit::with_repositories`](super::Commit::with_repositories).
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoTreeHere`] if the tree has no folder on this device, and an
	/// error if the space or the index cannot be read.
	pub fn repository_changes(
		&self,
		space: SpaceId,
		tree: EntityId,
		(found, described): (&[String], &[GitRepository]),
	) -> Result<Vec<TreeChange>, NodeError> {
		let root = self.root_here(space, tree)?;
		let apart = self.apart();
		let index = self.index()?;
		let scope = Scope {
			root: &root,
			apart: &apart,
			paths: &[],
		};
		// The statements it would make are thrown away: any randomness will do.
		let mut rng = UnwrapErr(SysRng);
		let plan =
			self.plan_repositories(space, (tree, &index, &scope), (found, described), &mut rng)?;
		Ok(plan.changes)
	}

	/// What a commit records of a tree's repositories: of those `found` in the copy, what
	/// Git says of them, in `described`, against what the tree records and what this copy
	/// last recorded or cloned of each. A repository found that Git did not describe stays
	/// as it is. One this copy held whose folder is surely gone, or no longer holds a
	/// repository, moved, if one found new has the remotes it had, or else is deleted; a
	/// move counts if either of its ends is chosen, as a file's does. One found where this
	/// copy held a repository that moved or was deleted elsewhere is left for the person.
	pub(super) fn plan_repositories(
		&self,
		space: SpaceId,
		(tree, index, scope): (EntityId, &Index, &Scope<'_>),
		(found, described): (&[String], &[GitRepository]),
		rng: &mut impl CryptoRng,
	) -> Result<Plan, NodeError> {
		let recorded = self.recorded_repositories(space, tree)?;
		let by_path: BTreeMap<&str, &Recorded> = recorded
			.iter()
			.map(|recorded| (recorded.path.as_str(), recorded))
			.collect();
		let by_entity: BTreeMap<EntityId, &Recorded> = recorded
			.iter()
			.map(|recorded| (recorded.entity, recorded))
			.collect();
		let held = index.repositories(tree)?;
		let found: BTreeSet<&str> = found.iter().map(String::as_str).collect();
		let mut plan = Plan::default();
		let mut new = Vec::new();
		let open = self.space(space)?;
		let state = open.stored.log().state();
		for repository in described {
			let path = repository.path.as_str();
			if !found.contains(path) || !is_safe_tree_path(path) {
				continue;
			}
			let now = Snapshot::of(repository).to_record();
			if let Some((entity, record)) = held.get(path) {
				// Moved or deleted elsewhere: the person settles it, not a commit.
				if let Some(recorded) = by_entity
					.get(entity)
					.filter(|recorded| recorded.path == path)
					&& within(path, scope.paths)
				{
					let before = Snapshot::from_record(record);
					changed(state, (recorded, repository, &before), &mut plan, rng)?;
					plan.hold.push((path.to_owned(), *entity, now));
				}
				continue;
			}
			match by_path.get(path) {
				// Where the tree records it, as a clone made outside NibKeep or a lost index
				// leaves it: held from now on, and nothing recorded.
				Some(recorded) => plan.hold.push((path.to_owned(), recorded.entity, now)),
				None => new.push(repository),
			}
		}
		for (path, (entity, record)) in &held {
			if found.contains(path.as_str()) || !gone_here(scope, path) {
				continue;
			}
			let recorded = by_entity
				.get(entity)
				.filter(|recorded| recorded.path == *path);
			let before = Snapshot::from_record(record);
			let moved = recorded.and_then(|_| {
				new.iter().position(|repository| {
					!before.remotes.is_empty() && recordable_remotes(repository) == before.remotes
				})
			});
			let chosen = |other: &str| within(path, scope.paths) || within(other, scope.paths);
			match (recorded, moved) {
				(Some(recorded), Some(position)) if chosen(&new[position].path) => {
					let repository = new.remove(position);
					plan.forget.push(path.clone());
					let mut operations = moving(state, *entity, path, &repository.path, rng)?;
					let mut edits = Plan::default();
					changed(state, (recorded, repository, &before), &mut edits, rng)?;
					operations.extend(edits.operations.into_iter().flat_map(|(_, edits)| edits));
					plan.operations.push((*entity, operations));
					let now = Snapshot::of(repository).to_record();
					plan.hold.push((repository.path.clone(), *entity, now));
					plan.changes.push(TreeChange {
						path: repository.path.clone(),
						change: Change::Moved { from: path.clone() },
					});
				}
				// A move to a place not chosen waits for a commit that chooses it.
				(Some(_), Some(position)) => {
					new.remove(position);
				}
				(Some(_), None) if within(path, scope.paths) => {
					plan.forget.push(path.clone());
					plan.operations
						.push((*entity, vec![deleting(*entity, rng)]));
					plan.changes.push(TreeChange {
						path: path.clone(),
						change: Change::Deleted,
					});
				}
				(None, _) if within(path, scope.paths) => plan.forget.push(path.clone()),
				(Some(_) | None, _) => {}
			}
		}
		for repository in new {
			if !within(&repository.path, scope.paths) {
				continue;
			}
			let (entity, operations) = self.new_repository(space, tree, repository, rng)?;
			plan.operations.push((entity, operations));
			let now = Snapshot::of(repository).to_record();
			plan.hold.push((repository.path.clone(), entity, now));
			plan.changes.push(TreeChange {
				path: repository.path.clone(),
				change: Change::New,
			});
		}
		plan.changes.sort_by(|a, b| a.path.cmp(&b.path));
		Ok(plan)
	}

	/// The operations that record a repository found new.
	fn new_repository(
		&self,
		space: SpaceId,
		tree: EntityId,
		repository: &GitRepository,
		rng: &mut impl CryptoRng,
	) -> Result<(EntityId, Vec<Operation>), NodeError> {
		let mut values = vec![
			NewValue::new(
				fields::NAME,
				Value::Name(Name::new(last_name(&repository.path))),
			),
			NewValue::new(IN_TREE, Value::Reference(tree)),
			NewValue::new(TREE_PATH, Value::Text(repository.path.clone())),
		];
		let said = Snapshot::of(repository);
		for (name, address) in &said.remotes {
			values.push(NewValue::new(
				GIT_REMOTE,
				Value::Text(format!("{name} {address}")),
			));
		}
		if let Some(branch) = said.branch {
			values.push(NewValue::new(GIT_BRANCH, Value::Text(branch)));
		}
		if let Some(upstream) = said.upstream {
			values.push(NewValue::new(GIT_UPSTREAM, Value::Text(upstream)));
		}
		self.new_entity(space, &[GIT_REPOSITORY], values, rng)
	}

	/// Pins the repository at `path` of a tree at `commit`, a full commit as Git names it,
	/// or with `None` takes its pin away, as a revision of its own: a pinned repository is
	/// cloned at that commit, and commits keep it as Git says it is.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoRepository`] if the tree records no repository at `path`,
	/// [`NodeError::NotACommit`] if `commit` is no full commit, and an error if saving
	/// fails.
	pub fn pin_repository(
		&mut self,
		space: SpaceId,
		tree: EntityId,
		path: &str,
		commit: Option<&str>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Committed, NodeError> {
		self.check_not_revoked(space)?;
		let _locked = self.lock_trees()?;
		let recorded = self
			.recorded_repositories(space, tree)?
			.into_iter()
			.find(|recorded| recorded.path == path)
			.ok_or_else(|| NodeError::NoRepository(path.to_owned()))?;
		if let Some(commit) = commit
			&& !is_commit(commit)
		{
			return Err(NodeError::NotACommit(commit.to_owned()));
		}
		if recorded.pinned.as_deref() == commit {
			return Ok(Committed::default());
		}
		self.install_standard(space, &TREE_TYPES, wall_millis, rng)?;
		let operations = {
			let state = self.space(space)?.stored.log().state();
			match commit {
				Some(commit) => {
					let value = Value::Text(commit.to_owned());
					vec![replacing(state, recorded.entity, GIT_COMMIT, value, rng)?]
				}
				None => clearing(state, recorded.entity, &GIT_COMMIT)?,
			}
		};
		let message = match commit {
			Some(commit) => format!("Pin {path} at {}", &commit[..12]),
			None => format!("Unpin {path}"),
		};
		// A commit of two parts, saved all or none: its revision, and the pin (technical design:
		// one entity per change).
		let revision = self.batch_whole(space, |library| {
			let (revision, made) =
				library.revision_operations(space, tree, Some(&message), 2, rng)?;
			library.commit(space, made, wall_millis, rng)?;
			let entity = recorded.entity;
			let operations = library.part_operations(space, revision, entity, operations, rng)?;
			library.commit(space, operations, wall_millis, rng)?;
			Ok(revision)
		})?;
		Ok(Committed {
			revision: Some(revision),
			parts: 2,
			changes: Vec::new(),
			repositories: vec![TreeChange {
				path: path.to_owned(),
				change: Change::Edited,
			}],
			..Committed::default()
		})
	}

	/// The repositories that a tree records and this device's copy does not hold, and that
	/// are not at their paths: what writing or updating the copy would clone, each with the
	/// remote it comes from and the device that recorded it, or why it may not be cloned, as
	/// when something that holds no Git repository is at its path. One deleted here, which
	/// the index says the copy held, is not listed, nor one whose folder holds a repository.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoTreeHere`] if the tree has no folder on this device, and an
	/// error if the space or the index cannot be read.
	pub fn repositories_to_clone(
		&self,
		space: SpaceId,
		tree: EntityId,
	) -> Result<Vec<ToClone>, NodeError> {
		let root = self.root_here(space, tree)?;
		let apart = self.apart();
		let index = self.index()?;
		let held: BTreeSet<EntityId> = index
			.repositories(tree)?
			.into_values()
			.map(|(entity, _)| entity)
			.collect();
		let marked = index.marked(tree)?;
		let recorded = self.recorded_repositories(space, tree)?;
		let mut to_clone = Vec::new();
		for repository in &recorded {
			if held.contains(&repository.entity) || marked.contains_key(&repository.path) {
				continue;
			}
			// A repository there is left as it is. Anything else there is too, but listed with
			// why it may not be cloned, as one inside another repository, or that cannot be
			// looked at, is.
			let there = match is_safe_tree_path(&repository.path)
				.then(|| located(&root, &repository.path, &apart))
			{
				Some(Located::Here(folder)) => Some(holds_git(&Listings::apart(&apart), &folder)),
				_ => None,
			};
			if there == Some(true) {
				continue;
			}
			let nested = inside_repository(&root, &repository.path, (&recorded, &marked));
			let mut listed = self.to_clone(space, repository, nested);
			if there == Some(false) && listed.refused.is_none() {
				let why = "something that holds no Git repository is at its path";
				listed.refused = Some(why.to_owned());
			}
			to_clone.push(listed);
		}
		Ok(to_clone)
	}

	/// Whether a person's replica made a statement: one that the space lists and has not
	/// revoked, and no AI agent's. One it cannot tell is not trusted.
	fn made_by_person(&self, space: SpaceId, statement: StatementId) -> bool {
		let Ok(open) = self.space(space) else {
			return false;
		};
		open.stored
			.reading(|stored| -> Result<bool, NodeError> {
				let log = stored.log();
				let Some(key) = log.author_of(statement)? else {
					return Ok(false);
				};
				Ok(log.is_listed(&key, &open.keys)?
					&& !log.is_revoked(&key, &open.keys)?
					&& !log.is_ai_agent(&key, &open.keys)?)
			})
			.unwrap_or(false)
	}

	/// What a clone of a recorded repository takes, or why it may not: never what an AI
	/// agent's replica, or one the space does not know, recorded of it.
	fn to_clone(&self, space: SpaceId, recorded: &Recorded, nested: bool) -> ToClone {
		let trusted = recorded
			.statements
			.iter()
			.all(|statement| self.made_by_person(space, *statement));
		let may_take = |name: &str, address: &str| is_safe_name(name) && cloneable(address).is_ok();
		// The address a clone takes of a remote: of the statement with the smallest ID.
		let first = |name: &str| {
			recorded
				.remotes
				.get(name)
				.and_then(|values| values.first())
				.map(|(address, statement)| (address.clone(), *statement))
		};
		// Of the remotes a clone may take: the one the branch follows, or else `origin`, or
		// else the first by name.
		let preferred: Vec<&String> = recorded
			.upstream
			.iter()
			.filter(|upstream| recorded.remotes.contains_key(*upstream))
			.chain(
				recorded
					.remotes
					.get_key_value("origin")
					.map(|(name, _)| name),
			)
			.chain(recorded.remotes.keys())
			.collect();
		let chosen = preferred
			.iter()
			.find(|name| first(name).is_some_and(|(address, _)| may_take(name, &address)))
			.or_else(|| preferred.first())
			.map(|name| (*name).clone());
		let (remote, address, statement) = match chosen
			.as_ref()
			.and_then(|name| first(name).map(|first| (name, first)))
		{
			Some((name, (address, statement))) => (name.clone(), address.clone(), Some(statement)),
			None => (String::new(), String::new(), None),
		};
		let refused = if statement.is_none() {
			Some("it has no remote".to_owned())
		} else if !is_safe_tree_path(&recorded.path) || recorded.path.contains(char::is_control) {
			Some("its path is not one a tree may hold".to_owned())
		} else if !trusted {
			Some(
				"an AI agent's replica, or one this library does not know, recorded part of it"
					.to_owned(),
			)
		} else if nested {
			Some("it is inside another Git repository".to_owned())
		} else if let Err(why) = cloneable(&address) {
			Some(why.to_owned())
		} else if !is_safe_name(&remote) {
			Some("its remote's name could be read as an option".to_owned())
		} else if recorded
			.branch
			.as_ref()
			.is_some_and(|branch| !is_safe_name(branch))
		{
			Some("its branch's name could be read as an option".to_owned())
		} else if recorded
			.pinned
			.as_ref()
			.is_some_and(|pinned| !is_commit(pinned))
		{
			Some("its pinned commit is not one".to_owned())
		} else {
			None
		};
		let others = recorded
			.remotes
			.keys()
			.filter(|name| **name != remote)
			.filter_map(|name| first(name).map(|(other, _)| (name.clone(), other)))
			.filter(|(name, other)| may_take(name, other))
			.collect();
		let recorded_by = statement
			.and_then(|statement| self.device_name_of(space, statement))
			.unwrap_or_else(|| "elsewhere".to_owned());
		ToClone {
			entity: recorded.entity,
			path: recorded.path.clone(),
			remote,
			address,
			others,
			branch: recorded.branch.clone(),
			pinned: recorded.pinned.clone(),
			recorded_by,
			refused,
		}
	}

	/// Makes the folder that a clone goes into, empty, once its place is checked as a file's
	/// is, parents and all: never through a link, outside the tree's folder, into `.nib`, a
	/// `.git` or the library's own folder, nor inside another Git repository. What the tree
	/// records must still be what `to` says, as the person agreed to it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::RepositoryChanged`] if the record is no longer what `to` says,
	/// or a clone may not take it now, [`NodeError::UnsafePath`] if its place is no safe
	/// one, [`NodeError::PlaceTaken`] if something is there, and an error if a folder cannot
	/// be made.
	pub fn prepare_clone(
		&self,
		space: SpaceId,
		tree: EntityId,
		to: &ToClone,
	) -> Result<CloneFolder, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let apart = self.apart();
		let all = self.recorded_repositories(space, tree)?;
		let marked = self.index()?.marked(tree)?;
		let changed = || NodeError::RepositoryChanged(to.path.clone());
		let recorded = all
			.iter()
			.find(|recorded| recorded.entity == to.entity)
			.ok_or_else(changed)?;
		let path = recorded.path.as_str();
		let nested = inside_repository(&root, path, (&all, &marked));
		// As it was asked about, and still one a clone may take.
		let now = self.to_clone(space, recorded, nested);
		if now != *to || now.refused.is_some() {
			return Err(changed());
		}
		if !is_safe_tree_path(path) || path.contains(char::is_control) {
			return Err(NodeError::UnsafePath(path.to_owned()));
		}
		let blocked = |why| match why {
			Blocked::Taken => NodeError::PlaceTaken(root.join(path)),
			Blocked::Unsafe => NodeError::UnsafePath(path.to_owned()),
		};
		let target = place_for_new(&root, path, &apart).map_err(blocked)?;
		let mut made: Vec<PathBuf> = target
			.ancestors()
			.skip(1)
			.take_while(|folder| folder.starts_with(&root) && *folder != root)
			.filter(|folder| fs::symlink_metadata(folder).is_err())
			.map(Path::to_path_buf)
			.collect();
		made.reverse();
		if let Some(parent) = target.parent() {
			fs::create_dir_all(parent).map_err(NodeError::file(parent))?;
		}
		// What made the folders may have raced a link into them.
		if place_for_new(&root, path, &apart).map_err(blocked)? != target {
			return Err(NodeError::UnsafePath(path.to_owned()));
		}
		fs::create_dir(&target).map_err(|error| match error.kind() {
			std::io::ErrorKind::AlreadyExists => NodeError::PlaceTaken(target.clone()),
			_ => NodeError::file(&target)(error),
		})?;
		Ok(CloneFolder {
			folder: target,
			made,
		})
	}

	/// Notes that this device's copy of a tree holds the repository that `to` names, as a
	/// clone made it at its path, with what Git says of the clone, `described`: no longer
	/// one to clone, and one whose folder gone later is deleted.
	///
	/// # Errors
	///
	/// Returns [`NodeError::RepositoryChanged`] if the tree no longer records it at that
	/// path, as when another device moved or deleted it meanwhile, and an error if the index
	/// cannot be written.
	pub fn repository_cloned(
		&self,
		space: SpaceId,
		tree: EntityId,
		(to, described): (&ToClone, &GitRepository),
	) -> Result<(), NodeError> {
		let _locked = self.lock_trees()?;
		let still = self
			.recorded_repositories(space, tree)?
			.into_iter()
			.any(|recorded| recorded.entity == to.entity && recorded.path == to.path);
		if !still {
			return Err(NodeError::RepositoryChanged(to.path.clone()));
		}
		let record = Snapshot::of(described).to_record();
		self.index()?
			.hold_repository(tree, &to.path, (to.entity, &record))
	}
}

#[cfg(test)]
mod tests {
	use super::{GitRepository, Snapshot, folders_of};

	#[test]
	fn what_git_said_is_kept_and_read_back_as_it_was() {
		let said = Snapshot::of(&GitRepository {
			path: "proj".to_owned(),
			remotes: vec![
				(
					"origin".to_owned(),
					"https://u:t@example.com/p.git".to_owned(),
				),
				(
					"bad name".to_owned(),
					"https://example.com/x.git".to_owned(),
				),
				("lines".to_owned(), "https://example.com/\ny.git".to_owned()),
			],
			branch: Some("main".to_owned()),
			upstream: Some("origin".to_owned()),
			commit: Some("a".repeat(40)),
		});
		assert_eq!(said.remotes.len(), 1);
		assert_eq!(said.remotes["origin"], "https://example.com/p.git");
		assert_eq!(Snapshot::from_record(&said.to_record()), said);
	}

	#[test]
	fn a_path_s_folders_are_listed_from_the_outermost() {
		assert_eq!(folders_of("a/b/c").collect::<Vec<_>>(), ["a", "a/b"]);
		assert_eq!(folders_of("c").count(), 0);
	}
}
