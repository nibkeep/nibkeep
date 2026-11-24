//! The history of a tree, read from the library's history and storing nothing: the log of
//! its changes, newest first, with who made them and their revisions' messages; blame, line
//! by line; and the tree as it is, as a revision's author had it, or as the library had
//! recorded it by a moment ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_log::ChangeId;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, State, Value, fields};
use nibkeep_std::{
	COMMITTED_IN, CONTENT, EXECUTABLE, IN_TREE, LINK_TARGET, MESSAGE, PARTS, REVISION,
	SECRET_CONTENT, SIZE, TREE_PATH,
};
use nibkeep_time::Timestamp;

use super::{Change, Changes, TreeChange, TreeState, read_tree_state, tree_entities};
use crate::NodeError;
use crate::library::{Author, Library, Space};

/// How large a file's content may be for a diff or blame to read it whole: 64 MiB.
pub(super) const READ_LIMIT: u64 = 64 * 1024 * 1024;

/// A point of a tree's history to read it at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreePoint {
	/// As the library has it now.
	Now,
	/// As a revision's author had it: the state of its commit's parts and of the changes
	/// they build on, so that changes made meanwhile on other devices do not slip in.
	Revision(EntityId),
	/// As the library had recorded it by a moment, in milliseconds since 1970; or, if a
	/// commit's parts were recorded both by it and after it, as it had by the moment before
	/// that commit, which a view of a moment never shows in part.
	AsOf(u64),
}

/// A change to a tree, as its log lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeLogEntry {
	/// The change, by its ID: of a commit, its first part's, which made its revision.
	pub change: String,
	/// When it was made, in milliseconds since 1970.
	pub at: u64,
	/// Who made it, as far as the space knows.
	pub author: Option<Author>,
	/// The commit's revision, if it was a commit.
	pub revision: Option<EntityId>,
	/// The revision's message, if it has one.
	pub message: Option<String>,
	/// Of a commit whose parts have not all arrived: how many have, and how many it has.
	pub part: Option<(u64, u64)>,
	/// What it did to the tree's files and links, by path.
	pub changes: Vec<TreeChange>,
}

/// A line of a file of a tree, with what last changed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
	/// The line, without its end.
	pub text: String,
	/// The revision that last changed it, if a commit did.
	pub revision: Option<EntityId>,
	/// Who did, as far as the space knows.
	pub author: Option<Author>,
	/// When, in milliseconds since 1970.
	pub at: u64,
}

/// What one change said of one entity of a tree.
#[derive(Clone, Debug, Default)]
struct Said {
	fields: BTreeMap<FieldName, Value>,
}

/// A revision of a tree, as its statements say.
#[derive(Clone, Debug)]
struct Revision {
	message: Option<String>,
	/// How many parts its commit has, its first included.
	parts: Option<u64>,
}

/// A part of a commit that is here, or its first part, by the statement that counts it.
#[derive(Clone, Copy, Debug)]
pub(super) struct PartHere {
	statement: StatementId,
	at: Timestamp,
	/// The change that made it, or the purge that stated it again.
	change: Option<Hash>,
	restated: bool,
}

impl Library {
	/// The tree as it is at a point of its history: as the library has it now, as a
	/// revision's author had it, or as the library had recorded it by a moment.
	pub(super) fn tree_state_at(
		&self,
		space: SpaceId,
		tree: EntityId,
		point: TreePoint,
	) -> Result<TreeState, NodeError> {
		let tree = self.check_tree(space, tree)?;
		match point {
			TreePoint::Now => self.tree_state(space, tree),
			TreePoint::AsOf(millis) => {
				let open = self.space(space)?;
				open.stored.reading(|stored| {
					let state = stored.log().state();
					let entities = tree_entities(state, tree)?;
					let millis = before_commits_under_way(stored, state, tree, millis)?;
					let log = stored.log();
					let changes = Changes::of(log);
					let readable = self.secrets_unlocked(space);
					let then = log.state_as_of(millis);
					read_tree_state(&then, tree, &entities, (Some(&changes), readable))
				})
			}
			TreePoint::Revision(revision) => {
				let changes = self.revision_changes(space, tree, revision)?;
				let last = *changes
					.last()
					.ok_or_else(|| NodeError::from(NoRevision(revision)))?;
				Ok(self
					.tree_states_of_changes(space, tree, last, &BTreeSet::new())?
					.0)
			}
		}
	}

	/// The tree as the changes `last` builds on, and `last` itself, leave it, and as they
	/// leave it without those of `without`: replayed from their payloads, in one pass, as the
	/// author of `last` had it. Only what the space counts is replayed: changes it set aside,
	/// as a revoked replica's, or whose payloads it cannot apply, add nothing, and of a purged
	/// change, what its purge stated again.
	pub(super) fn tree_states_of_changes(
		&self,
		space: SpaceId,
		tree: EntityId,
		last: ChangeId,
		without: &BTreeSet<ChangeId>,
	) -> Result<(TreeState, TreeState), NodeError> {
		self.tree_states_from(space, tree, &[last], without)
	}

	/// The tree as the changes that `heads` are or build on leave it, and as they leave it
	/// without those of `without`, as [`Library::tree_states_of_changes`] replays them.
	pub(super) fn tree_states_from(
		&self,
		space: SpaceId,
		tree: EntityId,
		heads: &[ChangeId],
		without: &BTreeSet<ChangeId>,
	) -> Result<(TreeState, TreeState), NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let entities: BTreeSet<EntityId> = open
			.stored
			.reading(|stored| tree_entities(stored.log().state(), tree))?
			.into_iter()
			.collect();
		// The changes it builds on, in the order of their times, which follow the order in
		// which they build on each other.
		let mut ancestors = Vec::new();
		let mut seen = BTreeSet::new();
		let mut waiting = heads.to_vec();
		while let Some(id) = waiting.pop() {
			if !seen.insert(id) {
				continue;
			}
			let Some(change) = log.change(id)? else {
				continue;
			};
			waiting.extend(change.parents().iter().copied());
			ancestors.push((change.timestamp(), id));
		}
		ancestors.sort();
		let mut restated: Option<BTreeMap<ChangeId, Vec<Operation>>> = None;
		let mut with: State = State::new();
		let mut apart: State = State::new();
		for (at, id) in ancestors {
			let status = nibkeep_log::History::status(log.history(), id)?;
			let operations = match status {
				Some(nibkeep_log::Status::Applied) => {
					log.payload(id, &open.keys)?.map_or_default(|payload| {
						let mut operations = payload.operations;
						operations.extend(opened(log, payload.sealed.keys()));
						operations
					})
				}
				// A change that a purge took away: what the purge stated again of it.
				Some(nibkeep_log::Status::Ignored) => {
					let restated = match &mut restated {
						Some(restated) => restated,
						None => restated.insert(restated_by_purges(open)?),
					};
					restated.get(&id).cloned().unwrap_or_default()
				}
				_ => continue,
			};
			for operation in &operations {
				let about_tree = match operation {
					Operation::Assert { statement, .. } => entities.contains(&statement.entity),
					Operation::Retract { .. } | Operation::RetractSecret { .. } => true,
					Operation::Unknown(_) => false,
				};
				if about_tree {
					let Ok(()) = with.apply_in(operation, at, Some(*id.hash()));
					if !without.contains(&id) {
						let Ok(()) = apart.apply_in(operation, at, Some(*id.hash()));
					}
				}
			}
		}
		let entities: Vec<EntityId> = entities.into_iter().collect();
		let changes = Changes::of(log);
		let readable = self.secrets_unlocked(space);
		Ok((
			read_tree_state(&with, tree, &entities, (Some(&changes), readable))?,
			read_tree_state(&apart, tree, &entities, (Some(&changes), readable))?,
		))
	}

	/// The tree as the library has it now, without the changes of `without`, such as the
	/// parts of a commit whose other parts have not arrived.
	pub(super) fn tree_state_without(
		&self,
		space: SpaceId,
		tree: EntityId,
		without: &BTreeSet<ChangeId>,
	) -> Result<TreeState, NodeError> {
		let heads: Vec<ChangeId> = self
			.space(space)?
			.stored
			.log()
			.heads()?
			.into_iter()
			.collect();
		Ok(self.tree_states_from(space, tree, &heads, without)?.1)
	}

	/// The tree that a revision is of, if `revision` is one.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn tree_of_revision(
		&self,
		space: SpaceId,
		revision: EntityId,
	) -> Result<Option<EntityId>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			let revision = state.canonical(revision)?;
			if !state.types(revision)?.contains(&REVISION) {
				return Ok(None);
			}
			for statement in state.current(revision, &IN_TREE)? {
				if let Value::Reference(tree) = statement.value {
					return Ok::<_, NodeError>(Some(state.canonical(tree)?));
				}
			}
			Ok(None)
		})
	}

	/// The changes of a revision's commit that are here, in order: of its first part, which
	/// made the revision, and of each other part. Of a part that a purge stated again, the
	/// change it came from, which the purge took away.
	pub(super) fn revision_changes(
		&self,
		space: SpaceId,
		tree: EntityId,
		revision: EntityId,
	) -> Result<Vec<ChangeId>, NodeError> {
		let open = self.space(space)?;
		let parts = open.stored.reading(|stored| {
			let state = stored.log().state();
			let revision = state.canonical(revision)?;
			if !is_revision_of(state, revision, tree)? {
				return Err(NoRevision(revision).into());
			}
			let mut parts: Vec<PartHere> = first_part(state, revision)?.into_iter().collect();
			parts.extend(parts_here(state, revision)?);
			Ok::<_, NodeError>(parts)
		})?;
		let mut restated = None;
		let mut changes = Vec::new();
		for part in parts {
			let change = if part.restated {
				let restated = match &mut restated {
					Some(restated) => restated,
					None => restated.insert(restated_by_purges(open)?),
				};
				restated
					.iter()
					.find(|(_, operations)| {
						operations.iter().any(|operation| {
							matches!(operation, Operation::Assert { statement, .. }
								if statement.id == part.statement)
						})
					})
					.map(|(source, _)| *source)
			} else {
				part.change.map(ChangeId::from_hash)
			};
			changes.extend(change.map(|change| (part.at, change)));
		}
		changes.sort();
		changes.dedup();
		Ok(changes.into_iter().map(|(_, change)| change).collect())
	}

	/// The log of a tree, newest first: each change that touched its files and links, with
	/// who made it, its revision and the revision's message, and what it did, by path. With
	/// `path`, only the changes to the file or link there, also when it was elsewhere then.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotInTree`] if the tree holds nothing at `path`, and an error if
	/// the space cannot be read.
	pub fn tree_log(
		&self,
		space: SpaceId,
		tree: EntityId,
		path: Option<&str>,
	) -> Result<Vec<TreeLogEntry>, NodeError> {
		let tree = self.check_tree(space, tree)?;
		let chosen = match path {
			Some(path) => {
				let state = self.tree_state(space, tree)?;
				let current = state
					.by_path
					.get(path)
					.or_else(|| state.deleted.get(path))
					.ok_or_else(|| NodeError::NotInTree(path.to_owned()))?;
				Some(current.entity)
			}
			None => None,
		};
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			let entities = tree_entities(state, tree)?;
			let revisions = revisions_of(stored, state, tree)?;
			let told = told_by_changes(state, tree, &entities, &revisions)?;
			let Commits {
				by_change: by_change_revision,
				made,
			} = commits(state, &revisions, &told.parts)?;
			let paths = &told.paths;
			let what_changed = |about: &BTreeMap<EntityId, Said>, at: Timestamp| {
				about
					.iter()
					.filter_map(|(entity, said)| {
						let path_then = |before: bool| {
							paths.get(entity).and_then(|list| {
								list.iter()
									.rev()
									.find(|(when, _)| if before { *when < at } else { *when <= at })
									.map(|(_, path)| path.clone())
							})
						};
						what_it_did(said, path_then(false), path_then(true))
					})
					.collect::<Vec<TreeChange>>()
			};
			// Each commit once, however many parts it has (technical design: one entity per change), and each other
			// change once: what each part said goes into its commit's entry.
			let mut grouped: Vec<Grouped> = Vec::new();
			let mut at_entry: BTreeMap<Result<EntityId, Hash>, usize> = BTreeMap::new();
			for ((change, recorded), about) in told.by_change {
				let about_chosen = chosen.is_none_or(|chosen| about.contains_key(&chosen));
				let commit = by_change_revision.get(&(change, recorded)).copied();
				let changes = what_changed(&about, recorded);
				match at_entry.entry(commit.ok_or(change)) {
					std::collections::btree_map::Entry::Occupied(entry) => {
						let entry = &mut grouped[*entry.get()];
						entry.changes.extend(changes);
						entry.chosen |= about_chosen;
					}
					std::collections::btree_map::Entry::Vacant(entry) => {
						entry.insert(grouped.len());
						grouped.push(Grouped {
							change,
							commit,
							changes,
							chosen: about_chosen,
						});
					}
				}
			}
			// Only the entries kept are read further: who made each, and when.
			let mut entries: Vec<(Timestamp, TreeLogEntry)> = Vec::new();
			for grouped in grouped.into_iter().filter(|grouped| grouped.chosen) {
				let reading = (open, stored, state);
				if let Some(entry) = self.log_entry(reading, grouped, (&revisions, &made))? {
					entries.push(entry);
				}
			}
			// Newest first, by the changes' full times, which order changes made in one
			// millisecond too.
			entries.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.change.cmp(&a.1.change)));
			Ok(entries.into_iter().map(|(_, entry)| entry).collect())
		})
	}

	/// The entry of the log for what `grouped` holds, named, of a commit, by its first part,
	/// which made its revision with the message, and with how many of its parts are here if
	/// not all are; `None` if its change is not in the log.
	#[allow(clippy::type_complexity, reason = "what the log read, passed along")]
	fn log_entry<S: Statements>(
		&self,
		(open, stored, state): (&Space, &nibkeep_store::StoredLog, &State<S>),
		grouped: Grouped,
		(revisions, made): (&BTreeMap<EntityId, Revision>, &BTreeMap<EntityId, u64>),
	) -> Result<Option<(Timestamp, TreeLogEntry)>, NodeError>
	where
		NodeError: From<S::Error>,
	{
		let mut changes = grouped.changes;
		changes.sort_by(|a, b| a.path.cmp(&b.path));
		changes.dedup();
		let first = match grouped.commit {
			Some(commit) => type_change(state, commit)?,
			None => None,
		};
		let change = first.unwrap_or_else(|| ChangeId::from_hash(grouped.change));
		let Some(header) = stored.log().change(change)? else {
			return Ok(None);
		};
		let revision = grouped.commit.and_then(|commit| revisions.get(&commit));
		// Of a commit whose parts have not all arrived, how many have.
		let part = grouped.commit.and_then(|commit| {
			let made = made.get(&commit).copied().unwrap_or(0);
			let of = revision.and_then(|revision| revision.parts).unwrap_or(made);
			(made < of).then_some((made, of))
		});
		let at = header.timestamp();
		Ok(Some((
			at,
			TreeLogEntry {
				change: change.to_string(),
				at: at.millis(),
				author: self.author_with_key(open, stored, header.author())?,
				revision: grouped.commit,
				message: revision.and_then(|revision| revision.message.clone()),
				part,
				changes,
			},
		)))
	}

	/// Blame for the file at `path` of a tree: for each line of its current version, the
	/// revision and the author that last changed it, through every version it had, whatever
	/// its path then.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotInTree`] if the tree holds no file at `path`,
	/// [`NodeError::NotText`] if a version is no text, [`NodeError::NoCopyHere`] if this
	/// device holds no copy of one, and an error if the space cannot be read.
	pub fn tree_blame(
		&self,
		space: SpaceId,
		tree: EntityId,
		path: &str,
	) -> Result<Vec<BlameLine>, NodeError> {
		self.tree_blame_with(space, tree, path, false)
	}

	/// Blame for the file at `path` of a tree, as [`Library::tree_blame`] gives it, and of a
	/// secret file too if `secrets` says so, with the secrets unlocked.
	///
	/// # Errors
	///
	/// The errors of [`Library::tree_blame`]; [`NodeError::SecretLines`] if the file is a
	/// secret one and `secrets` does not ask for it, and [`NodeError::SecretsLocked`] if it
	/// does while the secrets are locked.
	pub fn tree_blame_with(
		&self,
		space: SpaceId,
		tree: EntityId,
		path: &str,
		secrets: bool,
	) -> Result<Vec<BlameLine>, NodeError> {
		let tree = self.check_tree(space, tree)?;
		let state = self.tree_state(space, tree)?;
		let entity = match (state.by_path.get(path), state.sealed.get(path)) {
			(Some(current), _) => current.entity,
			(None, Some(_)) if !secrets => return Err(NodeError::SecretLines(path.to_owned())),
			(None, Some(_)) => return Err(NodeError::SecretsLocked),
			(None, None) => return Err(NodeError::NotInTree(path.to_owned())),
		};
		let secret = state.is_secret(entity);
		if secret && !secrets {
			return Err(NodeError::SecretLines(path.to_owned()));
		}
		if secret && !self.secrets_unlocked(space) {
			return Err(NodeError::SecretsLocked);
		}
		let field = if secret { SECRET_CONTENT } else { CONTENT };
		let open = self.space(space)?;
		// Its versions, in the order they were recorded, each with the change that made it.
		let versions: Vec<(Hash, Hash, Timestamp)> = open.stored.reading(|stored| {
			let state = stored.log().state();
			let storage = state.storage();
			let mut versions = Vec::new();
			for id in storage.of_field(entity, &field)? {
				if let Some(recorded) = storage.get(id)?
					&& let (Value::Blob(content), Some(change)) =
						(&recorded.statement.value, recorded.change)
				{
					versions.push((recorded.at, id, *content, change));
				}
			}
			versions.sort();
			Ok::<_, NodeError>(
				versions
					.into_iter()
					.map(|(at, _, content, change)| (content, change, at))
					.collect(),
			)
		})?;
		let mut lines: Vec<(String, Hash, u64)> = Vec::new();
		let versions: Vec<(Hash, Hash, u64)> = versions
			.into_iter()
			.map(|(content, change, at)| (content, change, at.millis()))
			.collect();
		let mut previous: Option<String> = None;
		for (content, change, at) in versions {
			let bytes = self
				.version_bytes(space, &content, secret)?
				.ok_or_else(|| NodeError::NoCopyHere(path.to_owned()))?;
			if !super::diff::is_text(&bytes) {
				return Err(NodeError::NotText(path.to_owned()));
			}
			let text = String::from_utf8_lossy(&bytes).into_owned();
			lines = match &previous {
				None => split_lines(&text)
					.into_iter()
					.map(|line| (line, change, at))
					.collect(),
				Some(before) => carried(before, &text, &lines, (change, at)),
			};
			previous = Some(text);
		}
		// A line names the commit that recorded its version, as the part that did names it.
		let revisions = open.stored.reading(|stored| {
			let storage = stored.log().state().storage();
			let mut by_change = BTreeMap::new();
			for id in storage.of_field(entity, &COMMITTED_IN)? {
				if let Some(recorded) = storage.get(id)?
					&& let (Value::Reference(revision), Some(change)) =
						(&recorded.statement.value, recorded.change)
				{
					by_change.insert(change, *revision);
				}
			}
			Ok::<_, NodeError>(by_change)
		})?;
		let mut authors: BTreeMap<Hash, Option<Author>> = BTreeMap::new();
		let mut blamed = Vec::with_capacity(lines.len());
		for (text, change, at) in lines {
			let author = if let Some(author) = authors.get(&change) {
				author.clone()
			} else {
				let author = open.stored.reading(|stored| {
					match stored.log().change(ChangeId::from_hash(change))? {
						Some(header) => self.author_with_key(open, stored, header.author()),
						None => Ok(None),
					}
				})?;
				authors.insert(change, author.clone());
				author
			};
			blamed.push(BlameLine {
				text,
				revision: revisions.get(&change).copied(),
				author,
				at,
			});
		}
		Ok(blamed)
	}
}

/// What the log lists as one entry: a commit, by its revision, or another change.
struct Grouped {
	/// The change it was first found by.
	change: Hash,
	commit: Option<EntityId>,
	/// What it did to the tree's files and links, by path.
	changes: Vec<TreeChange>,
	/// Whether it is about the file or link that the log was asked for, if any.
	chosen: bool,
}

/// The commits of a tree's changes, as its log groups them.
struct Commits {
	/// The commit of each change, by its revision: of each part of a change, as of a purge,
	/// which states again parts of several changes, by the time each was recorded.
	by_change: BTreeMap<(Hash, Timestamp), EntityId>,
	/// How many parts of each commit are here.
	made: BTreeMap<EntityId, u64>,
}

/// The commits of a tree's changes, of its `revisions`: each first part, which made its
/// revision, and each of `parts`, which names it on what it recorded.
fn commits<S: Statements>(
	state: &State<S>,
	revisions: &BTreeMap<EntityId, Revision>,
	parts: &[(EntityId, Option<(Hash, Timestamp)>)],
) -> Result<Commits, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut commits = Commits {
		by_change: BTreeMap::new(),
		made: BTreeMap::new(),
	};
	let mut first_parts = Vec::with_capacity(revisions.len());
	for &revision in revisions.keys() {
		if let Some(part) = first_part(state, revision)? {
			first_parts.push((revision, part.change.map(|change| (change, part.at))));
		}
	}
	for &(revision, change) in first_parts.iter().chain(parts) {
		*commits.made.entry(revision).or_default() += 1;
		if let Some(change) = change {
			commits.by_change.insert(change, revision);
		}
	}
	Ok(commits)
}

/// What the changes to a tree's files and links told, as its log reads them.
struct Told {
	/// What each change said of each file and link, by change and when it was recorded: a
	/// purge states parts of several changes again, each with the time of the change it came
	/// from.
	by_change: BTreeMap<(Hash, Timestamp), BTreeMap<EntityId, Said>>,
	/// Each file's and link's paths over time, in order.
	paths: BTreeMap<EntityId, Vec<(Timestamp, String)>>,
	/// The parts of commits that recorded them, or the tree, but their first parts: the
	/// revision each names, and its change, with when it was recorded.
	parts: Vec<(EntityId, Option<(Hash, Timestamp)>)>,
}

/// What each change said of each file and link among `entities`, revisions aside; each one's
/// paths over time; and the parts of commits that recorded them or `tree`, which name their
/// revisions on them.
fn told_by_changes<S: Statements>(
	state: &State<S>,
	tree: EntityId,
	entities: &[EntityId],
	revisions: &BTreeMap<EntityId, Revision>,
) -> Result<Told, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	let mut told = Told {
		by_change: BTreeMap::new(),
		paths: BTreeMap::new(),
		parts: Vec::new(),
	};
	let mut part = |recorded: &nibkeep_model::storage::Recorded<'_>| {
		if recorded.statement.field == COMMITTED_IN
			&& let Value::Reference(revision) = recorded.statement.value
		{
			let change = recorded.change.map(|change| (change, recorded.at));
			told.parts.push((revision, change));
		}
	};
	for id in storage.of_field(tree, &COMMITTED_IN)? {
		if let Some(recorded) = storage.get(id)? {
			part(&recorded);
		}
	}
	for &entity in entities {
		if revisions.contains_key(&entity) {
			continue;
		}
		for (field, ids) in storage.of_entity(entity)? {
			for id in ids {
				let Some(recorded) = storage.get(id)? else {
					continue;
				};
				part(&recorded);
				if field == TREE_PATH
					&& let Value::Text(path) = &recorded.statement.value
				{
					told.paths
						.entry(entity)
						.or_default()
						.push((recorded.at, path.clone()));
				}
				let Some(change) = recorded.change else {
					continue;
				};
				told.by_change
					.entry((change, recorded.at))
					.or_default()
					.entry(entity)
					.or_default()
					.fields
					.insert(field.clone(), recorded.statement.value.clone());
			}
		}
	}
	for list in told.paths.values_mut() {
		list.sort();
	}
	Ok(told)
}

/// A revision that the tree does not hold.
struct NoRevision(EntityId);

impl From<NoRevision> for NodeError {
	fn from(NoRevision(revision): NoRevision) -> Self {
		Self::NoRevision(revision.to_string())
	}
}

/// What a change that said `said` of a file or link did to it, at the path it had then, or
/// before it for a move.
fn what_it_did(said: &Said, now: Option<String>, before: Option<String>) -> Option<TreeChange> {
	let has = |field: &FieldName| said.fields.contains_key(field);
	let path = now?;
	let change = if has(&fields::TYPE) {
		Change::New
	} else if said.fields.get(&fields::DELETED) == Some(&Value::Boolean(true)) {
		Change::Deleted
	} else if has(&TREE_PATH) {
		match before {
			Some(from) if from != path => Change::Moved { from },
			_ => Change::Edited,
		}
	} else if has(&CONTENT) || has(&SECRET_CONTENT) || has(&SIZE) || has(&LINK_TARGET) {
		// A secret file's new version is sealed, and shows by its size, which is ordinary.
		Change::Edited
	} else if let Some(Value::Boolean(executable)) = said.fields.get(&EXECUTABLE) {
		Change::Executable(*executable)
	} else {
		return None;
	};
	Some(TreeChange { path, change })
}

/// The revisions of a tree, by ID.
fn revisions_of<S: Statements>(
	stored: &nibkeep_store::StoredLog,
	state: &State<S>,
	tree: EntityId,
) -> Result<BTreeMap<EntityId, Revision>, NodeError>
where
	NodeError: From<S::Error>,
{
	let tree = state.canonical(tree)?;
	let mut revisions = BTreeMap::new();
	for entity in stored.find(&IN_TREE, &Value::Reference(tree))? {
		let entity = state.canonical(entity)?;
		if !state.types(entity)?.contains(&REVISION) {
			continue;
		}
		let first_value = |field: &FieldName| -> Result<Option<Value>, NodeError> {
			let mut current = state.current(entity, field)?;
			current.sort_by_key(|statement| statement.id);
			Ok(current.first().map(|statement| statement.value.clone()))
		};
		let message = match first_value(&MESSAGE)? {
			Some(Value::Text(message)) => Some(message),
			_ => None,
		};
		let parts = match first_value(&PARTS)? {
			Some(Value::Integer(parts)) => u64::try_from(parts).ok(),
			_ => None,
		};
		revisions.insert(entity, Revision { message, parts });
	}
	Ok(revisions)
}

/// The first part of a revision's commit, which made the revision: by the first statement
/// of its type.
fn first_part<S: Statements>(
	state: &State<S>,
	revision: EntityId,
) -> Result<Option<PartHere>, NodeError>
where
	NodeError: From<S::Error>,
{
	let Some(id) = state
		.storage()
		.of_field(revision, &fields::TYPE)?
		.into_iter()
		.min()
	else {
		return Ok(None);
	};
	Ok(state.storage().get(id)?.map(|recorded| PartHere {
		statement: id,
		at: recorded.at,
		change: recorded.change,
		restated: recorded.restated.is_some(),
	}))
}

/// The parts of a revision's commit that are here, but its first: each names the revision on
/// the entity it recorded ([technical design: one entity per change]), a statement that history keeps when a later
/// commit records the entity again, and that a purge of the entity's other values states
/// again.
///
/// [technical design: one entity per change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#62-one-entity-per-change
pub(super) fn parts_here<S: Statements>(
	state: &State<S>,
	revision: EntityId,
) -> Result<Vec<PartHere>, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	let mut parts = Vec::new();
	for id in storage.referring_to(revision)? {
		if let Some(recorded) = storage.get(id)?
			&& recorded.statement.field == COMMITTED_IN
		{
			parts.push(PartHere {
				statement: id,
				at: recorded.at,
				change: recorded.change,
				restated: recorded.restated.is_some(),
			});
		}
	}
	Ok(parts)
}

/// The moment to read a tree as of, for `millis`: that moment, or, if a commit's parts were
/// recorded both by it and after it, the moment before the earliest such commit's first, so
/// that a view of a moment never shows a commit in part ([technical design: one entity per change]). A commit's
/// parts are made at its moment, but its device takes in what others wrote between batches
/// of them, and its clock moves past any later change it takes in.
///
/// [technical design: one entity per change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#62-one-entity-per-change
fn before_commits_under_way<S: Statements>(
	stored: &nibkeep_store::StoredLog,
	state: &State<S>,
	tree: EntityId,
	millis: u64,
) -> Result<u64, NodeError>
where
	NodeError: From<S::Error>,
{
	// When each part of each commit was recorded.
	let mut commits: Vec<Vec<u64>> = Vec::new();
	for &revision in revisions_of(stored, state, tree)?.keys() {
		let parts: Vec<u64> = first_part(state, revision)?
			.into_iter()
			.chain(parts_here(state, revision)?)
			.map(|part| part.at.millis())
			.collect();
		commits.push(parts);
	}
	let mut moment = millis;
	loop {
		let under_way = commits
			.iter()
			.filter(|parts| {
				parts.iter().any(|&at| at <= moment) && parts.iter().any(|&at| at > moment)
			})
			.filter_map(|parts| parts.iter().min().copied())
			.min();
		match under_way {
			Some(first) if first > 0 => moment = first - 1,
			Some(_) => return Ok(0),
			None => return Ok(moment),
		}
	}
}

/// Whether `revision` is a revision of `tree`.
fn is_revision_of<S: Statements>(
	state: &State<S>,
	revision: EntityId,
	tree: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	let tree = state.canonical(tree)?;
	if !state.types(revision)?.contains(&REVISION) {
		return Ok(false);
	}
	for statement in state.current(revision, &IN_TREE)? {
		if let Value::Reference(of) = statement.value
			&& state.canonical(of)? == tree
		{
			return Ok(true);
		}
	}
	Ok(false)
}

/// The change that made an entity, by the first statement of its type.
fn type_change<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<ChangeId>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(first_part(state, entity)?.and_then(|part| part.change.map(ChangeId::from_hash)))
}

/// What the purges of a space stated again of the changes they purged, by change.
fn restated_by_purges(open: &Space) -> Result<BTreeMap<ChangeId, Vec<Operation>>, NodeError> {
	let log = open.stored.log();
	let mut restated: BTreeMap<ChangeId, Vec<Operation>> = BTreeMap::new();
	for change in log.changes()? {
		if change.purges().is_empty() {
			continue;
		}
		if let Ok(Some(payload)) = log.payload(change.id(), &open.keys) {
			for (of, again) in payload.restated {
				let sealed = opened(log, again.sealed.keys());
				let operations = restated.entry(of).or_default();
				operations.extend(again.operations);
				operations.extend(sealed);
			}
		}
	}
	Ok(restated)
}

/// The secret statements of `sealed` that this replica opened, as its secrets are unlocked,
/// asserted: what a replay of a change's payload adds to its operations.
fn opened<'a>(
	log: &nibkeep_log::Log<nibkeep_store::StateFile, nibkeep_store::LogFile>,
	sealed: impl Iterator<Item = &'a StatementId>,
) -> Vec<Operation> {
	let storage = log.state().storage();
	sealed
		.filter_map(|id| storage.get(*id).ok().flatten())
		.map(|recorded| Operation::assert(recorded.statement.into_owned()))
		.collect()
}

/// The lines of a text, without their ends.
pub(super) fn split_lines(text: &str) -> Vec<String> {
	text.lines().map(str::to_owned).collect()
}

/// The lines of `after`, each with what last changed it: those that `before` held carry
/// what they had, and the others what made `after`.
fn carried(
	before: &str,
	after: &str,
	blamed: &[(String, Hash, u64)],
	made: (Hash, u64),
) -> Vec<(String, Hash, u64)> {
	let patch = diffy::DiffOptions::new()
		.set_context_len(usize::MAX)
		.create_patch(before, after);
	let Some(hunk) = patch.hunks().first() else {
		return blamed.to_vec();
	};
	let mut old = blamed.iter();
	let mut lines = Vec::new();
	for line in hunk.lines() {
		match line {
			diffy::Line::Context(text) => {
				let (_, change, at) =
					old.next()
						.cloned()
						.unwrap_or((String::new(), made.0, made.1));
				lines.push((trimmed(text), change, at));
			}
			diffy::Line::Delete(_) => {
				old.next();
			}
			diffy::Line::Insert(text) => lines.push((trimmed(text), made.0, made.1)),
		}
	}
	lines
}

/// A line without its end.
fn trimmed(line: &str) -> String {
	line.trim_end_matches(['\n', '\r']).to_owned()
}
