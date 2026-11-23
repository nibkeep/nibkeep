//! Comparing the copy of a tree on this device with the tree and with the index: what a
//! commit would record, what changed in the tree since the copy was written, and which
//! copies are a version the tree knows ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_model::Value;
use nibkeep_std::CONTENT;

use super::index::{Conflict, Entry, Index, Version};
use super::locate::{Listings, Located, located_in};
use super::secrecy::Rules;
use super::walk::{Found, FoundKind, Walked, hash_found, look, nanos, walk};
use super::{Change, Current, Purged, TreeState};
use crate::NodeError;
use crate::library::Library;

/// Looks at the paths the tree or the index holds that the walk did not reach, as where a
/// `.nibignore` came to name them, name by name as their folders list them; returns the paths
/// that cannot be told, which are neither changes nor gone.
fn look_tracked(
	tracked: BTreeSet<&String>,
	(root, listings): (&Path, &Listings),
	(entries, purged, left_out): (&BTreeMap<String, Entry>, &Purged, &BTreeSet<String>),
	(found, compared): (&mut BTreeMap<String, Found>, &mut Compared),
) -> BTreeSet<String> {
	let mut unreachable = BTreeSet::new();
	for path in tracked {
		// Copies whose content a purge took away, which could not be deleted yet, are
		// neither changes to commit nor the tree's to write.
		let held_purged = entries.get(path).is_some_and(|entry| purged.holds(entry));
		if held_purged {
			let why = "its content was purged; it is deleted once it can be".to_owned();
			compared.skipped.push((path.clone(), why));
			unreachable.insert(path.clone());
			continue;
		}
		if found.contains_key(path) || left_out.contains(path) {
			continue;
		}
		match located_in(listings, root, path) {
			Located::Here(on_disk) => match look(&on_disk) {
				Ok(at) => {
					found.insert(path.clone(), at);
				}
				Err(why) => {
					compared.skipped.push((path.clone(), why));
					unreachable.insert(path.clone());
				}
			},
			Located::Absent => {}
			// What cannot be read is not taken as gone.
			Located::Unknown(why) => {
				compared.skipped.push((path.clone(), why));
				unreachable.insert(path.clone());
			}
		}
	}
	unreachable
}

/// Shows what NibKeep wrote beside copies, until it is gone or renamed, when its mark goes.
fn show_marked(
	marked: BTreeMap<String, String>,
	(root, listings): (&Path, &Listings),
	conflicts: &BTreeMap<String, Conflict>,
	compared: &mut Compared,
) {
	for (path, why) in marked {
		if located_in(listings, root, &path) == Located::Absent {
			// The copy of another device's file is forgotten only once a commit settled
			// that file.
			if !conflicts.contains_key(&path) {
				compared.unmarked.push(path);
			}
		} else {
			let why = format!("{why}; rename it to keep it in the tree");
			compared.marked.push((path, why));
		}
	}
}

/// What is at each of some paths.
type AtPaths<T> = Vec<(String, T)>;

/// A change that a commit would record, with what it needs.
#[derive(Clone, Debug)]
pub(super) struct Pending {
	pub(super) path: String,
	pub(super) change: Change,
	/// The file or link it changes, as the index or the state names it.
	pub(super) entity: Option<EntityId>,
	/// What the copy holds there, if anything.
	pub(super) found: Option<Found>,
	/// The hash of the copy's content, if it was read.
	pub(super) hash: Option<Hash>,
	/// The version that the change is from, if it is from one.
	pub(super) base: Option<Version>,
	/// Whether what it changes changed in the tree since the copy was written.
	pub(super) stale: bool,
	/// Whether it is of a secret file, or would go in as one, as the tree's rules say
	/// without what a commit chooses.
	pub(super) secret: bool,
}

/// What comparing a tree's copy with the tree and the index found.
#[derive(Debug, Default)]
pub(super) struct Compared {
	pub(super) pending: Vec<Pending>,
	pub(super) behind: BTreeSet<String>,
	pub(super) skipped: Vec<(String, String)>,
	pub(super) read: usize,
	/// The entries to write to the index: copies found as a version of the tree.
	pub(super) refreshed: Vec<(String, Entry)>,
	/// Whether the index holds the whole copy, so that a path it lacks was never written
	/// here.
	pub(super) known: bool,
	/// How many of the tree's paths the copy holds, or may hold where it cannot be read:
	/// none, when its drive is not there.
	pub(super) present: usize,
	/// When the comparison began, before it read anything.
	pub(super) read_at: i64,
	/// The paths whose entries the index no longer needs: copies deleted here of what the
	/// tree deleted or moved too.
	pub(super) forgotten: Vec<String>,
	/// The marked paths whose copies are gone, which the index no longer needs to mark.
	pub(super) unmarked: Vec<String>,
	/// What NibKeep wrote beside copies, which commits leave out, with why: shown as what a
	/// commit cannot take, but nothing left unknown.
	pub(super) marked: Vec<(String, String)>,
	/// The folders that hold Git repositories, by path in the tree.
	pub(super) repositories: Vec<String>,
	/// What makes the copy's new files secret: the built-in names, and its `.nibsecret`
	/// files.
	pub(super) rules: Rules,
}

/// What a path is compared with: the index's entries, the tree's state, and whether the
/// index holds the whole copy.
struct Against<'a> {
	entries: &'a BTreeMap<String, Entry>,
	state: &'a TreeState,
	known: bool,
	read_at: i64,
}

impl Library {
	/// Compares the copy of a tree at `root` with the tree's state and the index: what a
	/// commit would record, what changed in the tree meanwhile, and the index's entries to
	/// write for copies found as a version of the tree.
	///
	/// No copy that holds content in `purged`, of the kind it would be, is ever a change to
	/// commit. With `read_new`, every new file is read, as a commit must to tell whether it
	/// holds purged content.
	pub(super) fn compare(
		&self,
		space: SpaceId,
		(tree, root): (EntityId, &Path),
		state: &TreeState,
		index: &Index,
		(purged, read_new): (&Purged, bool),
	) -> Result<Compared, NodeError> {
		// Before anything is read: a copy changed after this is read again.
		let read_at = nanos(SystemTime::now()).unwrap_or(0);
		let entries = index.entries(tree)?;
		let left_out = self.left_out(space, tree, root, index)?;
		let Walked {
			mut found,
			skipped,
			repositories,
		} = walk(root, &left_out, &self.apart());
		let mut compared = Compared {
			skipped,
			repositories,
			known: index.knows(tree)?,
			read_at,
			rules: Rules::of_copy(&found),
			..Compared::default()
		};
		// A path the tree holds stays in it whatever `.nibignore` came to say, so it is looked
		// at where the walk did not go: name by name, as its folders list them.
		let tracked: BTreeSet<&String> = entries.keys().chain(state.by_path.keys()).collect();
		let listings = Listings::apart(&self.apart());
		let conflicts = index.conflicts(tree)?;
		show_marked(
			index.marked(tree)?,
			(root, &listings),
			&conflicts,
			&mut compared,
		);
		let unreachable = look_tracked(
			tracked,
			(root, &listings),
			(&entries, purged, &left_out),
			(&mut found, &mut compared),
		);
		compared.present = state
			.by_path
			.keys()
			.filter(|path| found.contains_key(*path) || unreachable.contains(*path))
			.count();
		let paths: BTreeSet<String> = found
			.keys()
			.chain(entries.keys())
			.chain(state.by_path.keys())
			.filter(|path| !unreachable.contains(*path))
			.cloned()
			.collect();
		let against = Against {
			entries: &entries,
			state,
			known: compared.known,
			read_at,
		};
		let mut new = Vec::new();
		let mut deleted = Vec::new();
		for path in paths {
			let entry = entries.get(&path);
			let Some(at) = found.get(&path) else {
				// A file whose content a purge took away has nothing to write.
				let to_write = state
					.by_path
					.get(&path)
					.is_some_and(|current| current.version.is_some());
				if let Some(entry) = entry {
					deleted.push((path, entry.clone()));
				} else if to_write
					&& !state
						.by_path
						.get(&path)
						.is_some_and(|current| state.contested.contains_key(&current.entity))
				{
					// A file deleted here while another device gave it a new version is listed
					// among the competing.
					compared.behind.insert(path);
				}
				continue;
			};
			if entry.is_none() && !state.by_path.contains_key(&path) {
				new.push((path, at.clone()));
				continue;
			}
			self.compare_path(space, path, at, &against, &mut compared)?;
		}
		pair_moves((new, deleted), state, read_new, &mut compared);
		settle_conflicts(
			&conflicts,
			(root, &listings),
			(state, &entries),
			&mut compared,
		);
		let written = index.written_out(tree)?;
		settle_competing(state, (&entries, &written), &found, &mut compared);
		settle_secrets_and_purges(state, purged, &mut compared);
		Ok(compared)
	}

	/// Compares the copy at a path that the index or the tree holds.
	fn compare_path(
		&self,
		space: SpaceId,
		path: String,
		at: &Found,
		against: &Against<'_>,
		compared: &mut Compared,
	) -> Result<(), NodeError> {
		let entry = against.entries.get(&path);
		// This copy's file, where another competes for its path, else what the tree holds.
		let own = entry
			.filter(|entry| against.state.at(entry.entity, &path))
			.map(|entry| Current {
				entity: entry.entity,
				version: against.state.own.get(&entry.entity).cloned().flatten(),
			});
		let current = own.as_ref().or_else(|| against.state.by_path.get(&path));
		// The executable bit that the copy has, or where the system keeps none, the one
		// recorded.
		let recorded = entry
			.map(|entry| &entry.version)
			.or_else(|| current.and_then(|current| current.version.as_ref()));
		let executable = at.executable.unwrap_or(is_executable(recorded));
		let untouched = entry.is_some_and(|entry| {
			same_kind(&entry.version, &at.kind) && entry.untouched(at.size, at.modified)
		});
		let known = entry
			.filter(|_| untouched)
			.and_then(|entry| match entry.version {
				Version::File { content, .. } => Some(content),
				Version::Link { .. } => None,
			});
		let Some((copy, hash)) = copy_of(&path, at, (known, executable), compared) else {
			return Ok(());
		};
		let refreshed = |entity: EntityId, version: Version| Entry {
			entity,
			version,
			size: at.size,
			modified: at.modified,
			read_at: against.read_at,
		};
		let pending =
			|change: Change, (entity, base): (EntityId, Option<&Version>), stale: bool| Pending {
				path: path.clone(),
				change,
				entity: Some(entity),
				found: Some(at.clone()),
				hash,
				base: base.cloned(),
				stale,
				secret: false,
			};
		if let Some(entry) = entry {
			let from = (entry.entity, Some(&entry.version));
			let in_state = against.state.holds(&path, entry.entity, &entry.version);
			if copy == entry.version {
				if !in_state {
					compared.behind.insert(path.clone());
				}
				if !untouched {
					compared
						.refreshed
						.push((path, refreshed(entry.entity, copy)));
				}
			} else if let Some(current) =
				current.filter(|current| current.version.as_ref() == Some(&copy))
			{
				// The copy holds the tree's version, as a commit or a write here left it
				// before the index was told.
				compared
					.refreshed
					.push((path, refreshed(current.entity, copy)));
			} else if same_content(&copy, &entry.version) {
				let change = Change::Executable(executable);
				compared.pending.push(pending(change, from, !in_state));
			} else {
				compared
					.pending
					.push(pending(Change::Edited, from, !in_state));
			}
			return Ok(());
		}
		// The index has nothing here, and the tree has something.
		let Some(current) = current else {
			return Ok(());
		};
		let from = (current.entity, current.version.as_ref());
		if current.version.is_none() {
			// The content it was edited from was purged: which version it was, nobody knows.
			compared.pending.push(pending(Change::Unknown, from, false));
		} else if current.version.as_ref() == Some(&copy) {
			compared
				.refreshed
				.push((path, refreshed(current.entity, copy)));
		} else if against.known {
			// Never written here: added here and elsewhere, as two devices that add one path
			// give it two files.
			compared.pending.push(pending(Change::New, from, true));
		} else if let Some(past) = self.past_version(space, current.entity, &copy)? {
			// The copy holds an older version: the tree changed since it was written.
			compared.behind.insert(path.clone());
			compared
				.refreshed
				.push((path, refreshed(current.entity, past)));
		} else if current
			.version
			.as_ref()
			.is_some_and(|version| same_content(&copy, version))
		{
			let change = Change::Executable(executable);
			compared.pending.push(pending(change, from, false));
		} else {
			compared.pending.push(pending(Change::Unknown, from, false));
		}
		Ok(())
	}

	/// The version of a file of a space that a copy holds, if it is one the file had, and
	/// one whose content the store holds: its content then, with the copy's executable bit.
	/// A value with no content behind it, as any replica of the space may state, an AI
	/// agent's too, makes no copy an older version, which an update would write over beyond
	/// recall.
	pub(super) fn past_version(
		&self,
		space: SpaceId,
		entity: EntityId,
		copy: &Version,
	) -> Result<Option<Version>, NodeError> {
		let Version::File { content, .. } = copy else {
			return Ok(None);
		};
		let Some(history) = self.history(space, entity, Some(&CONTENT))? else {
			return Ok(None);
		};
		let had = history
			.fields
			.iter()
			.flat_map(|field| &field.values)
			.any(|past| past.value == Value::Blob(*content));
		let held = had && self.space(space)?.stored.holds_blob(content);
		Ok(held.then(|| copy.clone()))
	}
}

/// The version that the copy at a path holds, and its hash if it was read: of a file whose
/// content the index knows, as it says, and of another, as reading it tells; `None` if it
/// cannot be read, which is recorded.
fn copy_of(
	path: &str,
	at: &Found,
	(known, executable): (Option<Hash>, bool),
	compared: &mut Compared,
) -> Option<(Version, Option<Hash>)> {
	match (&at.kind, known) {
		(FoundKind::Link(target), _) => Some((
			Version::Link {
				target: target.clone(),
			},
			None,
		)),
		(FoundKind::File, Some(content)) => Some((
			Version::File {
				content,
				executable,
			},
			None,
		)),
		(FoundKind::File, None) => {
			compared.read += 1;
			match hash_found(at) {
				Ok(content) => Some((
					Version::File {
						content,
						executable,
					},
					Some(content),
				)),
				Err(error) => {
					compared.skipped.push((path.to_owned(), error.to_string()));
					None
				}
			}
		}
	}
}

/// Settles the files of other devices that competed for paths, whose copies were written
/// beside, once those copies are gone, while each still competes for the path it was
/// written beside: a new file that holds the file's version is that file, moved there, as
/// when the person renamed the copy; and if none does, the file is deleted. A copy written
/// before a newer version came settles nothing: the next update writes that version. The
/// record of a copy whose file no longer competes for that path is forgotten.
fn settle_conflicts(
	conflicts: &BTreeMap<String, Conflict>,
	(root, listings): (&Path, &Listings),
	(state, entries): (&TreeState, &BTreeMap<String, Entry>),
	compared: &mut Compared,
) {
	// The files with copies of their own here, as the index has them or this comparison
	// found them, as when the person renamed a copy beside onto its path.
	let placed: BTreeSet<EntityId> = entries
		.values()
		.map(|entry| entry.entity)
		.chain(compared.refreshed.iter().map(|(_, entry)| entry.entity))
		.collect();
	for (beside, conflict) in conflicts {
		if located_in(listings, root, beside) != Located::Absent {
			continue;
		}
		if !still_competes(state, &placed, conflict) {
			compared.unmarked.push(beside.clone());
			continue;
		}
		let (Some(own), Some(written)) = (
			state.own.get(&conflict.entity).cloned().flatten(),
			&conflict.written,
		) else {
			continue;
		};
		if !same_content(&own, written) {
			continue;
		}
		let mut renamed = None;
		for pending in &mut compared.pending {
			if pending.change != Change::New {
				continue;
			}
			let Some(found) = &pending.found else {
				continue;
			};
			if pending.hash.is_none() && found.kind == FoundKind::File {
				pending.hash = hash_found(found).ok();
			}
			let holds = match &found.kind {
				FoundKind::Link(target) => Some(Version::Link {
					target: target.clone(),
				}),
				FoundKind::File => pending.hash.map(|content| Version::File {
					content,
					executable: false,
				}),
			};
			if holds.is_some_and(|holds| same_content(&holds, &own)) {
				renamed = Some(pending.path.clone());
				break;
			}
		}
		// What this copy holds at the path, not yet committed, is a file of its own once the
		// other leaves the path.
		for pending in &mut compared.pending {
			if pending.path == conflict.of_path
				&& pending.change == Change::New
				&& pending.entity == Some(conflict.entity)
			{
				pending.entity = None;
				pending.base = None;
				pending.stale = false;
			}
		}
		let renamed = renamed.and_then(|path| {
			compared
				.pending
				.iter_mut()
				.find(|pending| pending.path == path)
		});
		if let Some(pending) = renamed {
			pending.change = Change::Moved {
				from: beside.clone(),
			};
			pending.entity = Some(conflict.entity);
			pending.base = Some(own);
		} else {
			compared.pending.push(Pending {
				path: beside.clone(),
				change: Change::Deleted,
				entity: Some(conflict.entity),
				found: None,
				hash: None,
				base: Some(own),
				stale: false,
				secret: false,
			});
		}
	}
	compared.pending.sort_by(|a, b| a.path.cmp(&b.path));
}

/// Whether the file of a copy written beside a path still holds that path, with no copy of
/// its own in this one: what the copy beside settles. Whatever holds the path here, another
/// file of the tree or one not committed yet, keeps it.
pub(super) fn still_competes(
	state: &TreeState,
	placed: &BTreeSet<EntityId>,
	conflict: &Conflict,
) -> bool {
	state.at(conflict.entity, &conflict.of_path) && !placed.contains(&conflict.entity)
}

/// Whether an update wrote out into a file's copy every value of it that competes: every
/// version, and of a file deleted on one device while another gave it a new version, that
/// version and every deletion. A version or a deletion that came since is not written out.
pub(super) fn written_out(
	state: &TreeState,
	entity: EntityId,
	written: &BTreeMap<EntityId, BTreeSet<String>>,
) -> bool {
	let has = |key: &str| written.get(&entity).is_some_and(|keys| keys.contains(key));
	if let Some(deletions) = state.contested.get(&entity) {
		let own = state.own.get(&entity).cloned().flatten();
		if !deletions.iter().all(|key| has(key)) || own.is_some_and(|own| !has(&own.key())) {
			return false;
		}
	}
	let Some(versions) = state.versions.get(&entity) else {
		return true;
	};
	versions.iter().all(|version| has(&version.key()))
}

/// Makes the copies of files whose own values compete, versions or paths, changes to
/// commit, as committing them settles the competition: their values supersede the others.
/// A change of a file whose versions compete is refused without force until an update wrote
/// them all out into the copy. Files that compete with another file for one path settle
/// through the copy beside instead.
fn settle_competing(
	state: &TreeState,
	(entries, written): (
		&BTreeMap<String, Entry>,
		&BTreeMap<EntityId, BTreeSet<String>>,
	),
	found: &BTreeMap<String, Found>,
	compared: &mut Compared,
) {
	for path in &state.competing {
		if compared.pending.iter().any(|pending| pending.path == *path) {
			continue;
		}
		let held = state
			.by_entity
			.values()
			.filter(|held| *held == path)
			.count();
		if held > 1 {
			continue;
		}
		let entry = entries.get(path);
		let (Some(at), Some(entity)) = (
			found.get(path),
			entry
				.map(|entry| entry.entity)
				.or_else(|| state.by_path.get(path).map(|current| current.entity)),
		) else {
			continue;
		};
		// Until an update wrote out its competing versions, a copy nobody touched is no
		// change: committing it would drop them unseen.
		if !state.at(entity, path) || !written_out(state, entity, written) {
			continue;
		}
		let own = state.own.get(&entity).cloned().flatten();
		// A copy untouched since it was written holds the version it was written at.
		let known = entry
			.filter(|entry| entry.untouched(at.size, at.modified))
			.and_then(|entry| match entry.version {
				Version::File { content, .. } => Some(content),
				Version::Link { .. } => None,
			});
		let hash = match at.kind {
			FoundKind::File => known.or_else(|| hash_found(at).ok()),
			FoundKind::Link(_) => None,
		};
		compared.pending.push(Pending {
			path: path.clone(),
			change: Change::Edited,
			entity: Some(entity),
			found: Some(at.clone()),
			hash,
			base: own,
			stale: false,
			secret: false,
		});
	}
	// Whatever found them, changes of this copy's files whose versions compete wait for an
	// update; the settling of a copy beside does not, as no update writes those out.
	let here: BTreeSet<EntityId> = entries.values().map(|entry| entry.entity).collect();
	for pending in &mut compared.pending {
		if let Some(entity) = pending.entity
			&& here.contains(&entity)
			&& matches!(
				pending.change,
				Change::Edited | Change::Executable(_) | Change::Deleted | Change::Moved { .. }
			)
			&& !written_out(state, entity, written)
		{
			pending.stale = true;
		}
	}
	compared.pending.sort_by(|a, b| a.path.cmp(&b.path));
}

/// Pairs what is new in a copy with what was deleted from it that held the same content,
/// or the same link, as moves; the rest stays new or deleted. Only a new file of the size of
/// a deleted one is read.
fn pair_moves(
	(new, deleted): (AtPaths<Found>, AtPaths<Entry>),
	state: &TreeState,
	read_new: bool,
	compared: &mut Compared,
) {
	// A copy deleted here of what the tree no longer holds, deleted there too, or moved
	// where this comparison found its copy, needs nothing committed.
	let found_elsewhere: BTreeSet<EntityId> = compared
		.refreshed
		.iter()
		.map(|(_, entry)| entry.entity)
		.collect();
	let (mut deleted, agreed): (AtPaths<Entry>, AtPaths<Entry>) =
		deleted.into_iter().partition(|(_, entry)| {
			state.by_entity.contains_key(&entry.entity) && !found_elsewhere.contains(&entry.entity)
		});
	compared
		.forgotten
		.extend(agreed.into_iter().map(|(path, _)| path));
	let sizes: BTreeSet<u64> = deleted.iter().map(|(_, entry)| entry.size).collect();
	for (path, at) in new {
		let mut hash = None;
		let copy = match &at.kind {
			FoundKind::Link(target) => Some(Version::Link {
				target: target.clone(),
			}),
			FoundKind::File if read_new || sizes.contains(&at.size) => {
				compared.read += 1;
				match hash_found(&at) {
					Ok(read) => {
						hash = Some(read);
						Some(Version::File {
							content: read,
							executable: at.executable.unwrap_or(false),
						})
					}
					Err(error) => {
						compared.skipped.push((path, error.to_string()));
						continue;
					}
				}
			}
			FoundKind::File => None,
		};
		let moved_from = copy.as_ref().and_then(|copy| {
			deleted
				.iter()
				.position(|(_, entry)| same_content(copy, &entry.version))
		});
		let Some(position) = moved_from else {
			compared.pending.push(Pending {
				path,
				change: Change::New,
				entity: None,
				found: Some(at),
				hash,
				base: None,
				stale: false,
				secret: false,
			});
			continue;
		};
		let (from, entry) = deleted.remove(position);
		// A version that came since was not seen; a path that changed takes this one.
		let changed_elsewhere = !state.has_version(entry.entity, &entry.version);
		compared.pending.push(Pending {
			path,
			change: Change::Moved { from },
			entity: Some(entry.entity),
			found: Some(at),
			hash,
			base: Some(entry.version),
			stale: changed_elsewhere,
			secret: false,
		});
	}
	for (path, entry) in deleted {
		let changed_elsewhere = !state.has_version(entry.entity, &entry.version);
		compared.pending.push(Pending {
			path,
			change: Change::Deleted,
			entity: Some(entry.entity),
			found: None,
			hash: None,
			base: Some(entry.version),
			stale: changed_elsewhere,
			secret: false,
		});
	}
}

/// Settles which changes are of secret files, leaves out those that hold purged content of
/// their kind, which is never committed again, from whichever copy, and tells whether the
/// index holds the whole copy again: an index lost, and rebuilt, does once every copy is a
/// version it knows.
fn settle_secrets_and_purges(state: &TreeState, purged: &Purged, compared: &mut Compared) {
	for pending in &mut compared.pending {
		pending.secret = secrecy(pending, state, &compared.rules);
	}
	let (kept, refused): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut compared.pending)
		.into_iter()
		.partition(|pending| {
			pending
				.hash
				.is_none_or(|hash| !purged.contains(&hash, pending.secret))
		});
	compared.pending = kept;
	for pending in refused {
		let why = "it holds content that was purged".to_owned();
		compared.skipped.push((pending.path, why));
	}
	compared.pending.sort_by(|a, b| a.path.cmp(&b.path));
	compared.known |= compared
		.pending
		.iter()
		.all(|pending| pending.change != Change::Unknown);
}

/// Whether a change is of a secret file, or would take in one, as the tree's rules say: a
/// file still in the tree is what its first commit made it; a new file is secret if a
/// built-in name or a `.nibsecret` file matches its path, or a secret file of the tree had
/// it. A link is never secret.
pub(super) fn secrecy(pending: &Pending, state: &TreeState, rules: &Rules) -> bool {
	if pending
		.found
		.as_ref()
		.is_some_and(|found| matches!(found.kind, FoundKind::Link(_)))
	{
		return false;
	}
	match pending
		.entity
		.filter(|entity| state.by_entity.contains_key(entity))
	{
		Some(entity) => state.is_secret(entity),
		None => {
			pending.change != Change::Deleted
				&& (rules.is_secret(&pending.path) || state.secret_paths.contains(&pending.path))
		}
	}
}

/// Whether a version is that of an executable file.
pub(super) fn is_executable(version: Option<&Version>) -> bool {
	matches!(
		version,
		Some(Version::File {
			executable: true,
			..
		})
	)
}

/// Whether two versions hold the same content, or the same target, whatever their
/// executable bits.
pub(super) fn same_content(a: &Version, b: &Version) -> bool {
	match (a, b) {
		(Version::File { content: a, .. }, Version::File { content: b, .. }) => a == b,
		(Version::Link { target: a }, Version::Link { target: b }) => a == b,
		_ => false,
	}
}

/// Whether a version and what is on the disk are both files or both links.
fn same_kind(version: &Version, found: &FoundKind) -> bool {
	matches!(
		(version, found),
		(Version::File { .. }, FoundKind::File) | (Version::Link { .. }, FoundKind::Link(_))
	)
}
