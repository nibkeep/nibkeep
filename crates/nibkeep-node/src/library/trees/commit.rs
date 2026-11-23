//! Committing to a tree: what changed in its copy, sealed into the store and recorded in
//! parts made one after another: a first that makes the commit's revision, with its message,
//! then a part for each entity it records, which names the revision ([trees] and
//! [one entity per change] in the technical design).
//!
//! [trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees
//! [one entity per change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#62-one-entity-per-change

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Name, Operation, State, Statement, Value, fields};
use nibkeep_std::{
	COMMITTED_IN, CONTENT, EXECUTABLE, FILE, IN_TREE, LINK_TARGET, MEDIA_TYPE, MESSAGE, PARTS,
	REVISION, SECRET_CONTENT, SECRET_FILE, SIZE, SYMBOLIC_LINK, TREE_PATH,
};

use super::compare::{Pending, is_executable};
use super::index::{Entry, Index, Version};
use super::locate::{Located, located};
use super::repositories::{Plan, Scope};
use super::walk::{Found, FoundKind, look, nanos, open_found};
use super::{Change, Commit, Committed, Purged, TreeChange, TreeState};
use crate::NodeError;
use crate::library::Library;
use crate::library::entities::NewValue;
use crate::library::files::HEAD;
use crate::library::policies;

/// How many parts of a commit go in one batch of changes, which holds up the space's other
/// writers until it is saved.
const PARTS_IN_A_BATCH: usize = 1_000;

/// A part of a commit after its first, which makes the revision: what it records of one
/// entity, and what the index takes in once it is saved.
struct Part {
	/// The entity it records, on which it names the commit's revision, which counts it; none
	/// for a change saved with the commit that is no part of it, as the record of the tree's
	/// folder on this device, which no other device waits for.
	entity: Option<EntityId>,
	/// What it records; nothing for a change that only the index takes in, as of a path
	/// alone.
	operations: Vec<Operation>,
	indexing: Indexing,
}

/// What the index takes in once a part is saved, and what the commit says it recorded.
#[derive(Default)]
struct Indexing {
	removed: Vec<String>,
	set: Vec<(String, Entry)>,
	/// The entities whose versions that competed the part recorded over.
	entities: Vec<EntityId>,
	/// Whether the part ends what the commit records of Git repositories.
	holds: bool,
	/// What it recorded of a file or a link, by path.
	change: Option<TreeChange>,
	/// The version of a secret file it recorded, whose sealed hashes the index keeps.
	secret: Option<(EntityId, Hash)>,
}

/// A change to record, with its content sealed, the entity it leaves at its path, and its
/// operations.
struct Recorded {
	pending: Pending,
	sealed: Option<Sealed>,
	entity: Option<EntityId>,
	operations: Vec<Operation>,
	/// The ordinary file that it made a secret one at its path, if it did.
	made_secret: Option<EntityId>,
}

impl Library {
	/// Commits what changed in a tree's copy at `root`, as [`Library::commit_tree`] does,
	/// with the records that start the tree in `founding`, an entity's in each, first after
	/// the revision: as a part, with the entity it records, or as a change saved with the
	/// commit, once the lock is held and the types installed.
	pub(super) fn commit_in(
		&mut self,
		space: SpaceId,
		(tree, root): (EntityId, &Path),
		commit: &Commit<'_>,
		founding: Vec<(Option<EntityId>, Vec<Operation>)>,
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<Committed, NodeError> {
		let mut index = self.index()?;
		let purged = self.learn_purges(space, &index)?;
		let state = self.tree_state(space, tree)?;
		let apart = self.apart();
		super::settle_tree((tree, root, &apart), &mut index, (&purged, &state))?;
		self.follow_cut_short(space, (tree, root, &apart), &mut index, &state)?;
		// Where content was purged, new files are read too, so that none holding it is
		// sealed again.
		let read_new = !purged.is_empty();
		let mut compared =
			self.compare(space, (tree, root), &state, &index, (&purged, read_new))?;
		index.change(tree, &compared.forgotten, &compared.refreshed)?;
		self.note_secrets(space, (tree, &index), &state, &compared.refreshed)?;
		index.unmark(tree, &compared.unmarked)?;
		let mut repositories = self.repositories_to_commit(
			space,
			(tree, root, &index),
			(&compared.repositories, commit),
			rng,
		)?;
		// What a commit records of repositories goes in its last part, so that no device
		// takes it before the commit is whole.
		let recording_repositories = std::mem::take(&mut repositories.operations);
		let whole = compared.known && commit.paths.is_empty();
		if emptied(&compared, &repositories) && !commit.force {
			return Err(NodeError::TreeEmptied(root.to_path_buf()));
		}
		let mut pending = std::mem::take(&mut compared.pending);
		self.add_made_secret(root, (&state, commit), &mut pending);
		let chosen = chosen(pending, commit)?;
		let unlocked = self.secrets_unlocked(space);
		let (chosen, left) = with_secrets(chosen, (&state, commit), unlocked)?;
		// The index holds the whole copy once nothing of it was left unknown: a commit of
		// some paths leaves the rest as it found them.
		let whole =
			whole || (commit.paths.is_empty() && commit.force && compared.skipped.is_empty());
		if chosen.is_empty() && founding.is_empty() && recording_repositories.is_empty() {
			if whole {
				index.know(tree)?;
			}
			// Repositories found where the tree records them, which this copy now holds.
			hold(&index, tree, &repositories)?;
			return Ok(Committed {
				left,
				..Committed::default()
			});
		}
		// What each change records, sealed into the store first.
		let recorded =
			self.seal_and_record(space, tree, chosen, (&state, &purged), (wall_millis, rng))?;
		let unprotected = unprotected(&recorded, &state, &purged);
		let made_secret: Vec<EntityId> = recorded
			.iter()
			.filter_map(|recorded| recorded.made_secret)
			.collect();
		let records = (founding, recorded, recording_repositories);
		let parts = self.commit_parts(space, tree, records, (&state, compared.read_at))?;
		let count = parts
			.iter()
			.filter(|part| part.entity.is_some() && !part.operations.is_empty())
			.count()
			+ 1;
		if count == 1 {
			// Nothing to record: every copy changed while it was read, or what changed only
			// the index takes in, as a path whose file the tree no longer holds.
			let indexing = parts.into_iter().map(|part| part.indexing);
			let mut nothing = Committed::default();
			let secrets = take_in(&mut index, tree, indexing, &mut nothing)?.1;
			self.note_secret_versions(space, (tree, &index), secrets)?;
			hold(&index, tree, &repositories)?;
			if whole {
				index.know(tree)?;
			}
			return Ok(Committed {
				left,
				..Committed::default()
			});
		}
		let mut committed = Committed {
			repositories: repositories.changes.clone(),
			left,
			unprotected,
			made_secret,
			..Committed::default()
		};
		let saving = (tree, commit, count);
		let all = self.save_parts(
			space,
			saving,
			parts,
			(&mut index, &repositories, &mut committed),
			(wall_millis, rng),
		)?;
		if all && whole {
			index.know(tree)?;
		}
		Ok(committed)
	}

	/// The parts of a commit, but its first, which makes the revision: each about one entity
	/// ([technical design: one entity per change]), the tree's founding first, when it starts, with its folder
	/// here beside; then each file and link; and last what it records of Git repositories,
	/// so that no device takes them before the commit is whole.
	///
	/// [technical design: one entity per change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#62-one-entity-per-change
	#[allow(clippy::type_complexity, reason = "what a commit records, read once")]
	fn commit_parts(
		&self,
		space: SpaceId,
		tree: EntityId,
		(founding, recorded, repositories): (
			Vec<(Option<EntityId>, Vec<Operation>)>,
			Vec<Recorded>,
			Vec<(EntityId, Vec<Operation>)>,
		),
		(state, read_at): (&TreeState, i64),
	) -> Result<Vec<Part>, NodeError> {
		let mut parts: Vec<Part> = founding
			.into_iter()
			.map(|(entity, operations)| Part {
				entity,
				operations,
				indexing: Indexing::default(),
			})
			.collect();
		for one in recorded {
			parts.extend(self.file_parts(space, tree, one, state, read_at)?);
		}
		let last = repositories.len();
		for (number, (entity, operations)) in repositories.into_iter().enumerate() {
			parts.push(Part {
				entity: Some(entity),
				operations,
				indexing: Indexing {
					holds: number + 1 == last,
					..Indexing::default()
				},
			});
		}
		Ok(parts)
	}

	/// Saves a commit of `count` parts: its revision first, then `parts`, in batches of
	/// [`PARTS_IN_A_BATCH`], the index taking in each batch once it is saved, and the Git
	/// repositories this copy holds once the commit is whole. Returns whether every part was
	/// saved, rather than cut short as [`Commit::cut_after`] asks.
	fn save_parts(
		&mut self,
		space: SpaceId,
		(tree, commit, count): (EntityId, &Commit<'_>, usize),
		parts: Vec<Part>,
		(index, repositories, committed): (&mut Index, &Plan, &mut Committed),
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<bool, NodeError> {
		let cut_here = |made: usize| commit.cut_after.is_some_and(|after| made >= after);
		if cut_here(0) {
			return Ok(false);
		}
		let mut parts = parts.into_iter().peekable();
		let mut revision = None;
		let mut held = false;
		while revision.is_none() || parts.peek().is_some() {
			// What the index takes in once the batch is saved, and not before: an index ahead
			// of the log would take a version that the log never got for the copy's.
			let mut indexing: Vec<Indexing> = Vec::new();
			let mut made = committed.parts;
			let cut = self.batch(space, |library| {
				if revision.is_none() {
					let (made_now, operations) =
						library.revision_operations(space, tree, commit.message, count, rng)?;
					library.commit(space, operations, wall_millis, rng)?;
					revision = Some(made_now);
					made = 1;
				}
				let Some(revision) = revision else {
					return Ok(false);
				};
				for part in parts.by_ref().take(PARTS_IN_A_BATCH) {
					match (part.entity, part.operations.is_empty()) {
						(_, true) => {}
						(Some(entity), false) => {
							if cut_here(made) {
								return Ok(true);
							}
							let operations = library.part_operations(
								space,
								revision,
								entity,
								part.operations,
								rng,
							)?;
							library.commit(space, operations, wall_millis, rng)?;
							made += 1;
						}
						(None, false) => {
							library.commit(space, part.operations, wall_millis, rng)?;
						}
					}
					indexing.push(part.indexing);
				}
				Ok(cut_here(made) && parts.peek().is_some())
			})?;
			committed.revision = revision;
			committed.parts = made;
			let (holds, secrets) = take_in(index, tree, indexing, committed)?;
			self.note_secret_versions(space, (tree, index), secrets)?;
			if holds {
				hold(index, tree, repositories)?;
				held = true;
			}
			if cut {
				return Ok(false);
			}
		}
		if !held {
			hold(index, tree, repositories)?;
		}
		Ok(true)
	}

	/// The parts that record a change of a file or a link: one for each entity it records,
	/// as a file that became a link is another entity, with what the index takes in going
	/// with the last; or a part that records nothing, with what the index takes in, if the
	/// change records nothing in the tree.
	fn file_parts(
		&self,
		space: SpaceId,
		tree: EntityId,
		mut one: Recorded,
		state: &TreeState,
		read_at: i64,
	) -> Result<Vec<Part>, NodeError> {
		let mut indexing = Indexing::default();
		// Only new content supersedes the versions that competed; any change of a file whose
		// deletions competed settles them.
		if one.pending.change == Change::Edited
			|| one
				.pending
				.entity
				.is_some_and(|entity| state.contested.contains_key(&entity))
		{
			indexing.entities.extend(one.pending.entity);
		}
		if let Change::Moved { from } = &one.pending.change {
			indexing.removed.push(from.clone());
		}
		match index_entry(&one, state, read_at) {
			Some(entry) => {
				if one.pending.secret
					&& let Version::File { content, .. } = entry.version
				{
					indexing.secret = Some((entry.entity, content));
				}
				indexing.set.push((one.pending.path.clone(), entry));
			}
			None => indexing.removed.push(one.pending.path.clone()),
		}
		indexing.change = Some(TreeChange {
			path: one.pending.path.clone(),
			change: one.pending.change.clone(),
		});
		// What cannot be told of an entity, which tree's records never hold, goes with the
		// one the change leaves at its path.
		let fallback = one.entity.or(one.pending.entity).unwrap_or(tree);
		let operations = std::mem::take(&mut one.operations);
		let mut parts: Vec<Part> = self
			.space(space)?
			.by_entity(operations)?
			.into_iter()
			.map(|(entity, operations)| Part {
				entity: Some(entity.unwrap_or(fallback)),
				operations,
				indexing: Indexing::default(),
			})
			.collect();
		match parts.last_mut() {
			Some(last) => last.indexing = indexing,
			None => parts.push(Part {
				entity: None,
				operations: Vec::new(),
				indexing,
			}),
		}
		Ok(parts)
	}

	/// What a part of the commit of `revision` saves: what it records of `entity`, and that
	/// the commit recorded it, which counts the part.
	pub(super) fn part_operations(
		&self,
		space: SpaceId,
		revision: EntityId,
		entity: EntityId,
		mut operations: Vec<Operation>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let committed_in = Value::Reference(revision);
		operations.push(replacing(state, entity, COMMITTED_IN, committed_in, rng)?);
		Ok(operations)
	}

	/// Seals the copies of the changes chosen into the store, and returns what each records;
	/// a copy that changed or went since it was looked at, or holds purged content, is left
	/// for the next commit.
	fn seal_and_record(
		&mut self,
		space: SpaceId,
		tree: EntityId,
		chosen: Vec<Pending>,
		(state, purged): (&TreeState, &Purged),
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<Vec<Recorded>, NodeError> {
		let mut recorded = Vec::with_capacity(chosen.len());
		for pending in chosen {
			// An ordinary file made secret is sealed anew, also where its copy is unchanged.
			let made_secret = pending.secret
				&& pending.entity.is_some_and(|entity| {
					state.by_entity.contains_key(&entity) && !state.is_secret(entity)
				});
			let sealed = match (&pending.found, &pending.change) {
				(Some(at), Change::New | Change::Edited | Change::Unknown)
					if at.kind == FoundKind::File =>
				{
					let secret = pending.secret;
					match self.seal_copy(space, at, secret, (wall_millis, rng)) {
						Ok(sealed) if !purged.contains(&sealed.hash, secret) => Some(sealed),
						// A copy that changed or went since it was looked at, as an editor's
						// files do, is left for the next commit, and so is purged content.
						Ok(_) | Err(NodeError::File { .. }) => continue,
						Err(error) => return Err(error),
					}
				}
				_ => None,
			};
			let (entity, operations) =
				self.recording(space, tree, &pending, sealed.as_ref(), state, rng)?;
			let made_secret = made_secret.then_some(pending.entity).flatten();
			recorded.push(Recorded {
				pending,
				sealed,
				entity,
				operations,
				made_secret,
			});
		}
		Ok(recorded)
	}

	/// Adds, for each path that the commit makes secret where the tree holds an ordinary file
	/// whose copy is unchanged, the change that makes it a secret file; a changed copy has its
	/// change already.
	fn add_made_secret(
		&self,
		root: &Path,
		(state, commit): (&TreeState, &Commit<'_>),
		pending: &mut Vec<Pending>,
	) {
		let apart = self.apart();
		for path in &commit.secret {
			let Some(current) = state.by_path.get(path) else {
				continue;
			};
			if state.is_secret(current.entity)
				|| !matches!(current.version, Some(Version::File { .. }))
				|| pending.iter().any(|pending| pending.path == *path)
			{
				continue;
			}
			let Located::Here(on_disk) = located(root, path, &apart) else {
				continue;
			};
			let Ok(at) = look(&on_disk) else {
				continue;
			};
			if at.kind != FoundKind::File {
				continue;
			}
			pending.push(Pending {
				path: path.clone(),
				change: Change::Edited,
				entity: Some(current.entity),
				found: Some(at),
				hash: None,
				base: current.version.clone(),
				stale: false,
				secret: true,
			});
		}
		pending.sort_by(|a, b| a.path.cmp(&b.path));
	}

	/// What a commit records of the Git repositories `found` in a tree's copy, of those it
	/// chose, if `nib` read what Git says of them; nothing otherwise.
	fn repositories_to_commit(
		&self,
		space: SpaceId,
		(tree, root, index): (EntityId, &Path, &super::index::Index),
		(found, commit): (&[String], &Commit<'_>),
		rng: &mut impl CryptoRng,
	) -> Result<Plan, NodeError> {
		let read;
		let described = match commit.repositories {
			None => return Ok(Plan::default()),
			Some(super::Repositories::Given(given)) => given,
			Some(super::Repositories::Read(reading)) => {
				read = reading(found);
				read.as_slice()
			}
		};
		let apart = self.apart();
		let scope = Scope {
			root,
			apart: &apart,
			paths: &commit.paths,
		};
		self.plan_repositories(space, (tree, index, &scope), (found, described), rng)
	}

	/// Seals the copy that `at` found into the store, opened as it was found, never through
	/// a link, and returns what it held: its hash, size, time and media type, as read. Its
	/// time, and when it was read, are taken before it is read, so that a change made while
	/// it is read shows in the next status.
	fn seal_copy(
		&mut self,
		space: SpaceId,
		at: &Found,
		secret: bool,
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<Sealed, NodeError> {
		let read_at = nanos(SystemTime::now()).unwrap_or(0);
		let file = open_found(at).map_err(NodeError::file(&at.on_disk))?;
		let metadata = file.metadata().map_err(NodeError::file(&at.on_disk))?;
		let mut reading = Headed {
			inner: file,
			head: Vec::new(),
			size: 0,
		};
		let hash = if secret {
			self.seal_blob(space, (&mut reading, true), wall_millis, rng)?
		} else {
			self.import_blob(space, &mut reading, wall_millis, rng)?
		};
		Ok(Sealed {
			hash,
			size: reading.size,
			modified: metadata.modified().ok().and_then(nanos),
			read_at,
			media_type: infer::get(&reading.head).map(|kind| kind.mime_type().to_owned()),
			executable_here: at.executable,
		})
	}

	/// The operations that record a change of a tree's copy, its content sealed already,
	/// and the entity it leaves at its path.
	fn recording(
		&self,
		space: SpaceId,
		tree: EntityId,
		pending: &Pending,
		sealed: Option<&Sealed>,
		state: &TreeState,
		rng: &mut impl CryptoRng,
	) -> Result<(Option<EntityId>, Vec<Operation>), NodeError> {
		let space_state = self.space(space)?.stored.log().state();
		// The entity it changes, if it is still in the tree.
		let existing = pending
			.entity
			.filter(|entity| state.by_entity.contains_key(entity));
		let existing_is_link = existing
			.map(|entity| -> Result<bool, NodeError> {
				Ok(space_state.types(entity)?.contains(&SYMBOLIC_LINK))
			})
			.transpose()?;
		let found_is_link = pending
			.found
			.as_ref()
			.is_some_and(|at| matches!(at.kind, FoundKind::Link(_)));
		let secret = pending.secret;
		let mut operations = Vec::new();
		let mut left = existing;
		match (&pending.change, &pending.found, existing) {
			(Change::Deleted, _, Some(entity)) => operations.push(deleting(entity, rng)),
			(Change::Moved { .. }, Some(at), Some(entity)) => {
				// From where the tree has it, as a copy written beside has a name of its own.
				let from = state.by_entity.get(&entity).map_or("", String::as_str);
				operations.extend(moving(space_state, entity, from, &pending.path, rng)?);
				let was = is_executable(state.own.get(&entity).and_then(Option::as_ref));
				if let Some(executable) = at.executable
					&& executable != was
				{
					operations.extend(set_executable(space_state, entity, executable, rng)?);
				}
			}
			(Change::Executable(executable), _, Some(entity)) => {
				operations.extend(set_executable(space_state, entity, *executable, rng)?);
			}
			(Change::Deleted | Change::Moved { .. } | Change::Executable(_), _, _)
			| (_, None, _) => {}
			(_, Some(at), Some(entity))
				if existing_is_link == Some(found_is_link) && state.is_secret(entity) == secret =>
			{
				let version = (at, sealed, pending);
				operations.extend(new_version(space_state, entity, version, state, rng)?);
			}
			(_, Some(at), existing)
				if state.deleted.get(&pending.path).is_some_and(|deleted| {
					state.is_secret(deleted.entity) == secret
						&& space_state
							.types(deleted.entity)
							.is_ok_and(|types| types.contains(&SYMBOLIC_LINK) == found_is_link)
				}) =>
			{
				// A file that became a link, or the other way round, where the file of that
				// kind was deleted before: that file again, with its history.
				if let Some(entity) = existing {
					operations.push(deleting(entity, rng));
				}
				let deleted = state.deleted[&pending.path].entity;
				operations.extend(clearing(space_state, deleted, &fields::DELETED)?);
				let version = (at, sealed, pending);
				operations.extend(new_version(space_state, deleted, version, state, rng)?);
				left = Some(deleted);
			}
			(_, Some(at), existing) => {
				// A file that became a link, or the other way round, is another entity; so is
				// one whose entity is gone from the tree.
				if let Some(entity) = existing {
					operations.push(deleting(entity, rng));
				}
				let kind = match at.kind {
					FoundKind::Link(_) => SYMBOLIC_LINK,
					FoundKind::File if secret => SECRET_FILE,
					FoundKind::File => FILE,
				};
				let values = new_values(tree, pending, (at, sealed), state);
				let (made_entity, made) = self.new_entity(space, &[kind], values, rng)?;
				operations.extend(made);
				left = Some(made_entity);
			}
		}
		if pending.change == Change::Deleted {
			left = None;
		}
		// A file deleted elsewhere while this copy's version came, which stays: its deletion,
		// which competed with it, goes.
		if let Some(entity) = left
			&& state.contested.contains_key(&entity)
		{
			operations.extend(clearing(space_state, entity, &fields::DELETED)?);
		}
		Ok((left, operations))
	}

	/// A new revision of a tree, for a commit's first part: with a message, and how many
	/// parts the commit has, the first included.
	pub(super) fn revision_operations(
		&self,
		space: SpaceId,
		tree: EntityId,
		message: Option<&str>,
		parts: usize,
		rng: &mut impl CryptoRng,
	) -> Result<(EntityId, Vec<Operation>), NodeError> {
		let mut values = vec![NewValue::new(IN_TREE, Value::Reference(tree))];
		if let Some(message) = message.filter(|message| !message.is_empty()) {
			values.push(NewValue::new(MESSAGE, Value::Text(message.to_owned())));
		}
		values.push(NewValue::new(PARTS, Value::Integer(integer(parts))));
		self.new_entity(space, &[REVISION], values, rng)
	}
}

/// What a commit takes of what changed: everything, or what is at or under the paths it
/// gives, a move counting if either of its ends is. Unless it is forced, it refuses changes
/// to what changed in the tree meanwhile, and copies edited from a version this replica does
/// not know.
/// Whether a copy with none of the tree's files or repositories left is to delete some: more
/// likely a drive that is not there, or a folder emptied by mistake, than all deleted on
/// purpose.
fn emptied(compared: &super::compare::Compared, repositories: &Plan) -> bool {
	compared.present == 0
		&& compared.repositories.is_empty()
		&& (compared
			.pending
			.iter()
			.any(|pending| pending.change == Change::Deleted)
			|| repositories
				.changes
				.iter()
				.any(|change| change.change == Change::Deleted))
}

/// Takes into the index what parts saved record, in one transaction, as one for each part
/// would synchronize the index's file as many times, and into `committed` what they recorded.
/// Returns whether one of them ended what the commit records of Git repositories.
fn take_in(
	index: &mut Index,
	tree: EntityId,
	indexing: impl IntoIterator<Item = Indexing>,
	committed: &mut Committed,
) -> Result<(bool, Vec<(EntityId, Hash)>), NodeError> {
	let (mut removed, mut set, mut entities) = (Vec::new(), Vec::new(), Vec::new());
	let mut holds = false;
	let mut secrets = Vec::new();
	for indexed in indexing {
		removed.extend(indexed.removed);
		set.extend(indexed.set);
		entities.extend(indexed.entities);
		holds |= indexed.holds;
		if let (Some(secret), Some(change)) = (indexed.secret, &indexed.change) {
			secrets.push(secret);
			committed.secret.push(change.path.clone());
		}
		committed.changes.extend(indexed.change);
	}
	index.change(tree, &removed, &set)?;
	// The versions that competed, which these parts recorded over.
	index.forget_written_out(tree, &entities)?;
	committed.secret.sort();
	committed.secret.dedup();
	Ok((holds, secrets))
}

/// The commit's changes with what is secret settled, as the tree's rules and the commit's
/// choices say, and the paths of those of secret files left, as the secrets are locked or
/// not on this device: refused unless the commit leaves them.
fn with_secrets(
	chosen: Vec<Pending>,
	(state, commit): (&TreeState, &Commit<'_>),
	unlocked: bool,
) -> Result<(Vec<Pending>, Vec<String>), NodeError> {
	let mut kept = Vec::with_capacity(chosen.len());
	let mut needing = Vec::new();
	for mut pending in chosen {
		pending.secret = chosen_secrecy(&pending, state, commit);
		if pending.secret && !unlocked {
			needing.push(pending.path.clone());
		} else {
			kept.push(pending);
		}
	}
	if !needing.is_empty() && !commit.leave_secrets {
		return Err(NodeError::SecretsNeeded(needing));
	}
	Ok((kept, needing))
}

/// Whether a change is of a secret file, or takes in one, with what the commit chooses: a
/// new file at a path it names as ordinary is ordinary, and one at a path it names as
/// secret is secret, as is an ordinary file there, which it makes a secret one.
fn chosen_secrecy(pending: &Pending, state: &TreeState, commit: &Commit<'_>) -> bool {
	let is_link = pending
		.found
		.as_ref()
		.is_some_and(|found| matches!(found.kind, FoundKind::Link(_)));
	if is_link || pending.change == Change::Deleted {
		return pending.secret && !is_link;
	}
	let named = |paths: &[String]| paths.contains(&pending.path);
	match pending
		.entity
		.filter(|entity| state.by_entity.contains_key(entity))
	{
		Some(entity) => state.is_secret(entity) || named(&commit.secret),
		None if named(&commit.ordinary) => false,
		None => pending.secret || named(&commit.secret),
	}
}

/// The paths of the new ordinary files that a commit sealed whose content a secret file of
/// the tree holds, as this replica knows it, not empty: copies the secrets do not protect.
fn unprotected(recorded: &[Recorded], state: &TreeState, purged: &Purged) -> Vec<String> {
	let mut secret: BTreeSet<Hash> = purged
		.secret_versions
		.keys()
		.map(|(_, content)| *content)
		.collect();
	for (entity, version) in &state.own {
		if state.is_secret(*entity)
			&& let Some(Version::File { content, .. }) = version
		{
			secret.insert(*content);
		}
	}
	for one in recorded {
		if one.pending.secret
			&& let Some(sealed) = &one.sealed
		{
			secret.insert(sealed.hash);
		}
	}
	let empty = Hash::of(b"");
	let mut paths: Vec<String> = recorded
		.iter()
		.filter(|one| !one.pending.secret && one.pending.change == Change::New)
		.filter(|one| {
			one.sealed
				.as_ref()
				.is_some_and(|sealed| sealed.hash != empty && secret.contains(&sealed.hash))
		})
		.map(|one| one.pending.path.clone())
		.collect();
	paths.sort();
	paths
}

/// Records in the index which repositories this copy holds, as a commit's plan says.
fn hold(index: &super::index::Index, tree: EntityId, plan: &Plan) -> Result<(), NodeError> {
	index.forget_repositories(tree, &plan.forget)?;
	for (path, entity, record) in &plan.hold {
		index.hold_repository(tree, path, (*entity, record))?;
	}
	Ok(())
}

fn chosen(pending: Vec<Pending>, commit: &Commit<'_>) -> Result<Vec<Pending>, NodeError> {
	let chosen: Vec<Pending> = pending
		.into_iter()
		.filter(|pending| {
			commit.paths.is_empty()
				|| commit.paths.iter().any(|given| {
					let given = given.trim_end_matches('/');
					let under = |path: &str| {
						given.is_empty() || path == given || path.starts_with(&format!("{given}/"))
					};
					under(&pending.path)
						|| matches!(&pending.change, Change::Moved { from } if under(from))
				})
		})
		.collect();
	if !commit.force {
		let paths_where = |refused: fn(&Pending) -> bool| -> Vec<String> {
			chosen
				.iter()
				.filter(|pending| refused(pending))
				.map(|pending| pending.path.clone())
				.collect()
		};
		let changed_elsewhere = paths_where(|pending| pending.stale);
		if !changed_elsewhere.is_empty() {
			return Err(NodeError::TreeStale(changed_elsewhere));
		}
		let unknown = paths_where(|pending| pending.change == Change::Unknown);
		if !unknown.is_empty() {
			return Err(NodeError::UnknownVersion(unknown));
		}
	}
	Ok(chosen)
}

/// What the index holds of a path once its change is recorded, or `None` if the path holds
/// nothing of the tree's any more.
fn index_entry(recorded: &Recorded, state: &TreeState, read_at: i64) -> Option<Entry> {
	let pending = &recorded.pending;
	let (Some(at), Some(entity)) = (&pending.found, recorded.entity) else {
		return None;
	};
	let version = match (&at.kind, &recorded.sealed) {
		(FoundKind::Link(target), _) => Version::Link {
			target: target.clone(),
		},
		(FoundKind::File, Some(sealed)) => Version::File {
			content: sealed.hash,
			executable: sealed.executable(at, state, pending),
		},
		(FoundKind::File, None) => match (&pending.hash, &pending.base) {
			(Some(content), _) | (None, Some(Version::File { content, .. })) => Version::File {
				content: *content,
				executable: at
					.executable
					.unwrap_or(is_executable(pending.base.as_ref())),
			},
			_ => return None,
		},
	};
	let (size, modified, read_at) = recorded
		.sealed
		.as_ref()
		.map_or((at.size, at.modified, read_at), |sealed| {
			(sealed.size, sealed.modified, sealed.read_at)
		});
	Some(Entry {
		entity,
		version,
		size,
		modified,
		read_at,
	})
}

/// The values of a new file or link of a tree.
fn new_values(
	tree: EntityId,
	pending: &Pending,
	(at, sealed): (&Found, Option<&Sealed>),
	state: &TreeState,
) -> Vec<NewValue> {
	let mut values = vec![
		NewValue::new(
			fields::NAME,
			Value::Name(Name::new(file_name(&pending.path))),
		),
		NewValue::new(IN_TREE, Value::Reference(tree)),
		NewValue::new(TREE_PATH, Value::Text(pending.path.clone())),
	];
	let content = if pending.secret {
		SECRET_CONTENT
	} else {
		CONTENT
	};
	match (&at.kind, sealed) {
		(FoundKind::Link(target), _) => {
			values.push(NewValue::new(LINK_TARGET, Value::Text(target.clone())));
		}
		(FoundKind::File, Some(sealed)) => {
			values.push(NewValue::new(content, Value::Blob(sealed.hash)));
			values.push(NewValue::new(SIZE, Value::Integer(integer(sealed.size))));
			if let Some(media_type) = &sealed.media_type {
				values.push(NewValue::new(MEDIA_TYPE, Value::Text(media_type.clone())));
			}
			if sealed.executable(at, state, pending) {
				values.push(NewValue::new(EXECUTABLE, Value::Boolean(true)));
			}
		}
		(FoundKind::File, None) => {}
	}
	values
}

/// The operations that record a new version of what is at a path, on top of what the tree
/// holds, at the path where the copy has it.
fn new_version<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	(at, sealed, pending): (&Found, Option<&Sealed>, &Pending),
	tree_state: &TreeState,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut operations = Vec::new();
	match (&at.kind, sealed) {
		(FoundKind::Link(target), _) => operations.push(replacing(
			state,
			entity,
			LINK_TARGET,
			Value::Text(target.clone()),
			rng,
		)?),
		(FoundKind::File, Some(sealed)) => {
			let content = Value::Blob(sealed.hash);
			if pending.secret {
				operations.push(replacing(state, entity, SECRET_CONTENT, content, rng)?);
				// Nothing ordinary stands for a secret file's content, whatever a change said.
				operations.extend(clearing(state, entity, &CONTENT)?);
			} else {
				operations.push(replacing(state, entity, CONTENT, content, rng)?);
			}
			let size = Value::Integer(integer(sealed.size));
			operations.push(replacing(state, entity, SIZE, size, rng)?);
			match &sealed.media_type {
				Some(media_type) => {
					let media_type = Value::Text(media_type.clone());
					operations.push(replacing(state, entity, MEDIA_TYPE, media_type, rng)?);
				}
				None => operations.extend(clearing(state, entity, &MEDIA_TYPE)?),
			}
			let executable = sealed.executable(at, tree_state, pending);
			operations.extend(set_executable(state, entity, executable, rng)?);
		}
		(FoundKind::File, None) => {}
	}
	if let Some(from) = tree_state.by_entity.get(&entity)
		&& (*from != pending.path || tree_state.competing.contains(&pending.path))
	{
		// Also where its path competes with another: the path of this copy supersedes them.
		operations.extend(moving(state, entity, from, &pending.path, rng)?);
	}
	Ok(operations)
}

/// What sealing a copy into the store found.
#[derive(Clone, Debug)]
struct Sealed {
	hash: Hash,
	size: u64,
	modified: Option<i64>,
	/// When it was read, before it was.
	read_at: i64,
	media_type: Option<String>,
	/// Whether the copy is executable, where the system says.
	executable_here: Option<bool>,
}

impl Sealed {
	/// Whether the version is executable: as the copy is, or where the system keeps no
	/// executable bit, as the tree has it.
	fn executable(&self, at: &Found, state: &TreeState, pending: &Pending) -> bool {
		self.executable_here.or(at.executable).unwrap_or_else(|| {
			is_executable(
				pending
					.entity
					.and_then(|entity| state.by_entity.get(&entity))
					.and_then(|path| state.by_path.get(path))
					.and_then(|current| current.version.as_ref()),
			)
		})
	}
}

/// A reader that keeps the first bytes it reads, which tell a media type, and counts them.
struct Headed<R> {
	inner: R,
	head: Vec<u8>,
	size: u64,
}

impl<R: Read> Read for Headed<R> {
	fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		let read = self.inner.read(buffer)?;
		let room = HEAD.saturating_sub(self.head.len());
		self.head.extend_from_slice(&buffer[..read.min(room)]);
		self.size += read as u64;
		Ok(read)
	}
}

/// The operation that deletes an entity, which can be restored.
pub(super) fn deleting(entity: EntityId, rng: &mut impl CryptoRng) -> Operation {
	policies::assert(entity, fields::DELETED, Value::Boolean(true), rng)
}

/// The operations that move a file or a link of a tree from `from` to `to`: its path, and
/// its name if its file name changed.
pub(super) fn moving<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	from: &str,
	to: &str,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let path = Value::Text(to.to_owned());
	let mut operations = vec![replacing(state, entity, TREE_PATH, path, rng)?];
	if file_name(from) != file_name(to) {
		let name = Value::Name(Name::new(file_name(to)));
		operations.push(replacing(state, entity, fields::NAME, name, rng)?);
	}
	Ok(operations)
}

/// The operations that make a file executable, or not. A new file states its bit only if it
/// is executable; a bit taken away is stated false, rather than retracted, so that the log
/// shows it, and a deletion that did not see it competes with it, as with a new version.
fn set_executable<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	executable: bool,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let current = state.current(entity, &EXECUTABLE)?;
	let is = current
		.iter()
		.any(|statement| statement.value == Value::Boolean(true));
	if is == executable && current.len() <= 1 {
		return Ok(Vec::new());
	}
	let replaced: Vec<StatementId> = current.iter().map(|statement| statement.id).collect();
	let value = Value::Boolean(executable);
	let statement = Statement::new(StatementId::random(rng), entity, EXECUTABLE, value);
	Ok(vec![Operation::replace(statement, replaced)])
}

/// The operation that sets a field of an entity to `value`, superseding its current values.
pub(super) fn replacing<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: FieldName,
	value: Value,
	rng: &mut impl CryptoRng,
) -> Result<Operation, NodeError>
where
	NodeError: From<S::Error>,
{
	let current: Vec<StatementId> = state
		.current(entity, &field)?
		.iter()
		.map(|statement| statement.id)
		.collect();
	let statement = Statement::new(StatementId::random(rng), entity, field, value);
	Ok(Operation::replace(statement, current))
}

/// The operations that clear a field of an entity.
pub(super) fn clearing<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let current: Vec<StatementId> = state
		.current(entity, field)?
		.iter()
		.map(|statement| statement.id)
		.collect();
	Ok(if current.is_empty() {
		Vec::new()
	} else {
		vec![Operation::retract(entity, current)]
	})
}

/// A count or a size as a value.
fn integer(value: impl TryInto<i64>) -> i64 {
	value.try_into().unwrap_or(i64::MAX)
}

/// The last name of a tree's path.
fn file_name(path: &str) -> &str {
	path.rsplit('/').next().unwrap_or(path)
}
