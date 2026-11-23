//! Diffs of a tree: the copy on this device against the tree, or two points of the tree's
//! history, file by file, with unified diffs of text and only which versions differ for the
//! rest ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::BTreeSet;
use std::io::Read;

use nibkeep_base::{EntityId, Hash, SpaceId};

use super::history::{READ_LIMIT, TreePoint};
use super::index::Version;
use super::walk::open_found;
use super::{Change, TreeState, version_of};
use crate::NodeError;
use crate::library::Library;

/// How many of a file's first bytes tell whether it is text: as many as Git reads.
const TEXT_HEAD: usize = 8_000;

/// How one file or link of a tree differs between two points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
	/// Its path at the later point, or where it was, if it is gone then.
	pub path: String,
	/// What changed.
	pub change: Change,
	/// The unified diff of its text, if both versions are text that this device holds and
	/// they differ.
	pub patch: Option<String>,
	/// Whether a version is no text, so that only which versions differ is told.
	pub binary: bool,
	/// Whether it is a secret file, whose lines a diff shows only when asked for, with the
	/// secrets unlocked
	/// ([technical design: secrets in trees](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#151-secrets-in-trees)).
	pub secret: bool,
}

/// What one side of a diff holds at a path.
enum Side<'a> {
	/// Nothing.
	Nothing,
	/// A version that the library holds.
	Version(&'a Version),
	/// The copy on the disk, as it was found.
	Copy(&'a super::walk::Found),
}

impl Library {
	/// How a tree differs: with `between` given, from its first point to its second, as
	/// each is read ([`TreePoint`]); without, the copy on this device from the tree as the
	/// library has it now, what a commit would record. Files are paired by what they are,
	/// so that a move shows as one.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoRevision`] if a revision is not the tree's,
	/// [`NodeError::NoTreeHere`] if the copy is asked for and the tree has none here, and an
	/// error if the space or the copy cannot be read.
	pub fn tree_diff(
		&self,
		space: SpaceId,
		tree: EntityId,
		between: Option<(TreePoint, TreePoint)>,
	) -> Result<Vec<FileDiff>, NodeError> {
		self.tree_diff_with(space, tree, between, false)
	}

	/// How a tree differs, as [`Library::tree_diff`] tells it, with the lines of secret files
	/// too if `secrets` says so, which this device reads with the secrets unlocked; without,
	/// a secret file shows only that it changed.
	///
	/// # Errors
	///
	/// As [`Library::tree_diff`].
	pub fn tree_diff_with(
		&self,
		space: SpaceId,
		tree: EntityId,
		between: Option<(TreePoint, TreePoint)>,
		secrets: bool,
	) -> Result<Vec<FileDiff>, NodeError> {
		let mut diffs = match between {
			Some((from, to)) => {
				let before = self.tree_state_at(space, tree, from)?;
				let after = self.tree_state_at(space, tree, to)?;
				self.diff_states(space, (&before, &after), secrets)?
			}
			None => self.diff_copy(space, tree, secrets)?,
		};
		diffs.sort_by(|a, b| a.path.cmp(&b.path));
		Ok(diffs)
	}

	/// What a revision changed, as `git show` shows a commit: from the tree as its author
	/// had it without its commit's parts to the tree with them, so that changes made
	/// meanwhile on other devices do not slip in. `None` if `revision` is no revision of a
	/// tree.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn revision_diff(
		&self,
		space: SpaceId,
		revision: EntityId,
		secrets: bool,
	) -> Result<Option<Vec<FileDiff>>, NodeError> {
		let Some(tree) = self.tree_of_revision(space, revision)? else {
			return Ok(None);
		};
		let changes = self.revision_changes(space, tree, revision)?;
		let Some(&last) = changes.last() else {
			return Ok(None);
		};
		let parts: BTreeSet<_> = changes.iter().copied().collect();
		let (after, before) = self.tree_states_of_changes(space, tree, last, &parts)?;
		let mut diffs = self.diff_states(space, (&before, &after), secrets)?;
		diffs.sort_by(|a, b| a.path.cmp(&b.path));
		Ok(Some(diffs))
	}

	/// How the tree differs between two of its states.
	fn diff_states(
		&self,
		space: SpaceId,
		(before, after): (&TreeState, &TreeState),
		secrets: bool,
	) -> Result<Vec<FileDiff>, NodeError> {
		let entities: BTreeSet<EntityId> = before
			.by_entity
			.keys()
			.chain(after.by_entity.keys())
			.copied()
			.collect();
		let mut diffs = Vec::new();
		for entity in entities {
			let (path, change, old, new) =
				match (version_of(before, entity), version_of(after, entity)) {
					(None, Some((path, new))) => (path, Change::New, None, new),
					(Some((path, old)), None) => (path, Change::Deleted, old, None),
					(Some((from, old)), Some((path, new))) => {
						let change = if from != path {
							Change::Moved { from }
						} else if old == new {
							continue;
						} else {
							Change::Edited
						};
						(path, change, old, new)
					}
					(None, None) => continue,
				};
			let secret = before.is_secret(entity) || after.is_secret(entity);
			// With the secrets locked, a secret file's versions are known nowhere but in the
			// copy, which stands in for now alone: it is left out.
			if secret && !self.secrets_unlocked(space) {
				continue;
			}
			let old = old.as_ref().map_or(Side::Nothing, Side::Version);
			let new = new.as_ref().map_or(Side::Nothing, Side::Version);
			let shown = (secret, secrets);
			diffs.push(self.file_diff(space, (path, change), (&old, &new), shown)?);
		}
		Ok(diffs)
	}

	/// How the copy of a tree on this device differs from the tree as the library has it.
	fn diff_copy(
		&self,
		space: SpaceId,
		tree: EntityId,
		secrets: bool,
	) -> Result<Vec<FileDiff>, NodeError> {
		let _locked = self.lock_trees()?;
		let root = self.root_here(space, tree)?;
		let index = self.index()?;
		let purged = self.learn_purges(space, &index)?;
		let state = self.tree_state(space, tree)?;
		let compared = self.compare(space, (tree, &root), &state, &index, (&purged, false))?;
		let mut diffs = Vec::new();
		for pending in &compared.pending {
			let base = pending.base.clone().or_else(|| {
				pending
					.entity
					.and_then(|entity| state.by_entity.get(&entity))
					.and_then(|path| state.by_path.get(path))
					.and_then(|current| current.version.clone())
			});
			let old = match (&pending.change, &base) {
				(Change::New, _) | (_, None) => Side::Nothing,
				(_, Some(version)) => Side::Version(version),
			};
			let new = match &pending.found {
				Some(found) => Side::Copy(found),
				None => Side::Nothing,
			};
			let path = pending.path.clone();
			let at = (path, pending.change.clone());
			let shown = (pending.secret, secrets);
			diffs.push(self.file_diff(space, at, (&old, &new), shown)?);
		}
		Ok(diffs)
	}

	/// How one file differs between two sides; of a secret file, only that it changed unless
	/// `shown` asks for its lines.
	fn file_diff(
		&self,
		space: SpaceId,
		(path, change): (String, Change),
		(old, new): (&Side<'_>, &Side<'_>),
		(secret, shown): (bool, bool),
	) -> Result<FileDiff, NodeError> {
		if secret && !shown {
			return Ok(FileDiff {
				path,
				change,
				patch: None,
				binary: false,
				secret,
			});
		}
		let old_bytes = self.side_bytes(space, old, secret)?;
		let new_bytes = self.side_bytes(space, new, secret)?;
		let binary = [&old_bytes, &new_bytes]
			.iter()
			.any(|bytes| bytes.as_ref().is_some_and(|bytes| !is_text(bytes)));
		let unified = match (&old_bytes, &new_bytes) {
			(Some(old_bytes), Some(new_bytes)) if !binary && old_bytes != new_bytes => {
				let from = match &change {
					Change::Moved { from } => from.as_str(),
					_ => path.as_str(),
				};
				let old_text = String::from_utf8_lossy(old_bytes);
				let new_text = String::from_utf8_lossy(new_bytes);
				let made = diffy::DiffOptions::new()
					.set_original_filename(format!("a/{from}"))
					.set_modified_filename(format!("b/{path}"))
					.create_patch(&old_text, &new_text)
					.to_string();
				Some(made)
			}
			_ => None,
		};
		Ok(FileDiff {
			path,
			change,
			patch: unified,
			binary,
			secret,
		})
	}

	/// What a side holds, as bytes: nothing as empty, a link as its target, a version as
	/// this device holds its content, if it does and it is no larger than a diff reads, and
	/// the copy as it is. A secret file's version is read only with the secrets unlocked.
	fn side_bytes(
		&self,
		space: SpaceId,
		side: &Side<'_>,
		secret: bool,
	) -> Result<Option<Vec<u8>>, NodeError> {
		Ok(match side {
			Side::Nothing => Some(Vec::new()),
			Side::Version(Version::Link { target }) => Some(target.clone().into_bytes()),
			Side::Version(Version::File { content, .. }) => {
				self.version_bytes(space, content, secret)?
			}
			Side::Copy(found) => match &found.kind {
				super::walk::FoundKind::Link(target) => Some(target.clone().into_bytes()),
				super::walk::FoundKind::File => {
					let mut bytes = Vec::new();
					open_found(found)
						.and_then(|file| file.take(READ_LIMIT + 1).read_to_end(&mut bytes))
						.ok()
						.filter(|_| bytes.len() as u64 <= READ_LIMIT)
						.map(|_| bytes)
				}
			},
		})
	}

	/// The content with the hash `content`, as this device holds it, if no larger than a
	/// diff reads.
	pub(super) fn content_bytes(
		&self,
		space: SpaceId,
		content: &Hash,
	) -> Result<Option<Vec<u8>>, NodeError> {
		self.file_content(space, content, READ_LIMIT)
	}
}

/// Whether content is text: its first 8,000 bytes hold no zero byte, as Git decides.
pub(super) fn is_text(bytes: &[u8]) -> bool {
	!bytes[..bytes.len().min(TEXT_HEAD)].contains(&0)
}

#[cfg(test)]
mod tests {
	use super::is_text;

	#[test]
	fn text_is_what_holds_no_zero_byte_in_its_first_eight_thousand() {
		assert!(is_text(b"notes\n"));
		assert!(is_text(&[0xff, 0xfe, b'a']));
		assert!(!is_text(&[b'a', 0, b'b']));
		let mut late = vec![b'a'; 8_000];
		late.push(0);
		assert!(is_text(&late));
	}
}
