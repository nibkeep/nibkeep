//! Checkouts: plain copies of files that NibKeep writes on request and records, whose edits
//! become new versions of their files, and which a purge of their content deletes
//! ([technical design: checkouts]).
//!
//! [technical design: checkouts]: ../../../../docs/Technical-design.md#92-checkouts

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::{FieldName, Operation, Value, fields};
use nibkeep_std::{
	CHECKED_OUT_ON, CHECKOUT, CHECKOUT_OF, CONTENT, FILE, IN_TREE, MEDIA_TYPE, MODIFIED, PATH,
	SECRET_FILE, SIZE,
};

use super::entities::NewValue;
use super::export::blake3_of;
use super::files::{HEAD, Sniffing, moment, plain};
use super::{Library, policies};
use crate::NodeError;

/// Whether a checkout's copy is as it was written, or as its edits were recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyState {
	/// It has the content its checkout records.
	Unchanged,
	/// It was edited since, and its edits are not recorded.
	Edited,
	/// No file is where it was written.
	Gone,
}

impl CopyState {
	/// The state in a word, as JSON writes it.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Unchanged => "unchanged",
			Self::Edited => "edited",
			Self::Gone => "gone",
		}
	}
}

/// A checkout on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
	/// The checkout's record.
	pub id: EntityId,
	/// The file it is a copy of.
	pub file: EntityId,
	/// Where its copy is.
	pub path: PathBuf,
	/// The content its copy held when it was written, or when its edits were recorded; `None`
	/// once a purge took it away.
	pub content: Option<Hash>,
	/// Whether its copy is unchanged since.
	pub copy: CopyState,
}

/// What checking out a file did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedOut {
	/// The checkout's record.
	pub checkout: EntityId,
	/// The file.
	pub file: EntityId,
	/// Where its copy is.
	pub path: PathBuf,
	/// The content written.
	pub content: Hash,
	/// Whether it was written now, or was there already, unchanged.
	pub written: bool,
}

/// What recording the edits of a checkout's copy did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditsRecorded {
	/// Whether the copy was unchanged, edited or gone, as its hash told.
	pub copy: CopyState,
	/// The content hash of the new version that its edits made, if it was edited.
	pub version: Option<Hash>,
}

/// What ending a checkout did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
	/// Where its copy was.
	pub path: PathBuf,
	/// Whether its copy was deleted, rather than gone already.
	pub deleted: bool,
	/// Whether its copy was sealed into the store first, as no other copy on this device held
	/// its content.
	pub sealed: bool,
}

/// What a checkout records, as the state says.
#[derive(Clone, Debug)]
struct Record {
	id: EntityId,
	file: EntityId,
	path: PathBuf,
	content: Option<Hash>,
	size: Option<u64>,
	modified: Option<Value>,
}

impl Record {
	/// The checkout, with whether its copy is unchanged.
	fn checkout(&self) -> Checkout {
		Checkout {
			id: self.id,
			file: self.file,
			path: self.path.clone(),
			content: self.content,
			copy: self.copy_state(),
		}
	}

	/// Whether the copy is unchanged, as a status shows it: with the size and modification
	/// time recorded; edited if its size is not the one recorded; and otherwise as its hash
	/// tells, so that a copy of the same size whose time alone changed is read. A copy that
	/// cannot be read counts as edited, so that it is never taken for one that can be
	/// deleted.
	fn copy_state(&self) -> CopyState {
		let size = match fs::symlink_metadata(&self.path) {
			Ok(metadata) if metadata.is_file() => metadata.len(),
			_ => return CopyState::Gone,
		};
		if self.is_untouched() {
			return CopyState::Unchanged;
		}
		if self.size.is_some_and(|recorded| recorded != size) {
			return CopyState::Edited;
		}
		match (blake3_of(&self.path), self.content) {
			(Ok(content), Some(recorded)) if content == recorded => CopyState::Unchanged,
			_ => CopyState::Edited,
		}
	}

	/// Whether the copy is unchanged, as its hash tells, for what deletes it or records its
	/// edits: modification times are recorded to the second, so an edit made in the second
	/// the copy was written, of the same size, has the time and size recorded. A copy whose
	/// content a purge took away has only its size and modification time to tell.
	fn verified_state(&self) -> CopyState {
		match fs::symlink_metadata(&self.path) {
			Ok(metadata) if metadata.is_file() => {}
			_ => return CopyState::Gone,
		}
		let unchanged = match self.content {
			Some(recorded) => blake3_of(&self.path).is_ok_and(|content| content == recorded),
			None => self.is_untouched(),
		};
		if unchanged {
			CopyState::Unchanged
		} else {
			CopyState::Edited
		}
	}

	/// Whether the copy has the size and modification time recorded, as `nib file add` tells
	/// an unchanged file: all that tells, once a purge took the content's hash away.
	fn is_untouched(&self) -> bool {
		let Ok(metadata) = fs::symlink_metadata(&self.path) else {
			return false;
		};
		let modified = metadata.modified().ok().and_then(moment).map(Value::Date);
		metadata.is_file()
			&& self.size == Some(metadata.len())
			&& modified.is_some()
			&& modified == self.modified
	}
}

impl Library {
	/// Checks out a file of a space: writes a plain copy of its current content at `place`,
	/// and records it, as a checkout on this device. `place` is a folder, where the copy
	/// takes the file's name, or the path of a new file. The content comes from the store,
	/// or else from an original on this device whose hash still matches. A checkout there
	/// already, unchanged, is left as it is; one whose copy is gone is written again.
	///
	/// The copy is written beside its place, under a hidden name, checked against the
	/// content's hash and synced; then the checkout is recorded, and only then is the copy
	/// put in place, without replacing anything.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoContent`] if the file has no content, [`NodeError::NotAFileName`]
	/// if `place` is a folder and the file's name is no name a file can have,
	/// [`NodeError::PlaceTaken`] if something is at the copy's place, and
	/// [`NodeError::NoCopyHere`] if this device holds no copy of the content, which it then
	/// keeps whatever its size for a while, so that a sync with a device that holds it
	/// fetches it ([`Library::record_opened`]); and another error if the copy cannot be
	/// written, or the change saved.
	pub fn checkout(
		&mut self,
		space: SpaceId,
		file: EntityId,
		place: &Path,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<CheckedOut, NodeError> {
		// Before a copy is written that could not be recorded.
		self.check_not_revoked(space)?;
		let (file, name, content) = self.content_of(space, file)?;
		let path = place_for(place, name.as_deref())?;
		let mut replaced = None;
		if let Some(record) = self.record_at(space, &path)? {
			match record.copy_state() {
				CopyState::Unchanged if record.file == file && record.content == Some(content) => {
					// Opened all the same; a record that cannot be saved costs only that.
					let _ = self.record_opened(space, file, wall_millis);
					return Ok(CheckedOut {
						checkout: record.id,
						file,
						path,
						content,
						written: false,
					});
				}
				CopyState::Gone => replaced = Some(record.id),
				_ => return Err(NodeError::PlaceTaken(path)),
			}
		}
		if fs::symlink_metadata(&path).is_ok() {
			return Err(NodeError::PlaceTaken(path));
		}
		let temporary = temporary_for(&path, "checkout");
		remove_leftover(&temporary);
		let (size, modified) =
			match self.write_copy(space, (file, name.as_deref()), &content, &temporary) {
				Ok(written) => written,
				Err(error) => {
					remove_leftover(&temporary);
					// Opened where this device lacks the content, so that a sync fetches it.
					if matches!(error, NodeError::NoCopyHere(_)) {
						self.record_opened(space, file, wall_millis)?;
					}
					return Err(error);
				}
			};
		let recorded = self.record_checkout(
			space,
			(file, &path, content),
			(size, modified),
			replaced,
			(wall_millis, rng),
		);
		let checkout = match recorded {
			Ok(checkout) => checkout,
			Err(error) => {
				remove_leftover(&temporary);
				return Err(error);
			}
		};
		if let Err(error) = put_in_place(&temporary, &path) {
			remove_leftover(&temporary);
			// The record names a copy that is not there, so it ends; if that fails, it shows
			// as gone.
			let _ = self.commit(space, vec![deleting(checkout, rng)], wall_millis, rng);
			return Err(if error.kind() == io::ErrorKind::AlreadyExists {
				NodeError::PlaceTaken(path)
			} else {
				NodeError::file(&path)(error)
			});
		}
		// Opened; a record that cannot be saved costs only that, the copy being written.
		let _ = self.record_opened(space, file, wall_millis);
		Ok(CheckedOut {
			checkout,
			file,
			path,
			content,
			written: true,
		})
	}

	/// Writes a plain copy of a file's current content for a viewer to show, in a folder of
	/// the program's own, and returns where: read-only, in a folder named after the content,
	/// under the file's name, so that the viewer knows its kind. It is no checkout: nothing
	/// records it, no edit of it is taken, and the program removes it later
	/// ([`remove_view_copies`]). A copy written before is given again. The file is recorded
	/// as opened ([`Library::record_opened`]).
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoContent`] if the file has no content,
	/// [`NodeError::SecretFile`] for a secret file, and [`NodeError::NoCopyHere`] if this
	/// device holds no copy of the content, which it then keeps whatever its size for a
	/// while, so that a sync with a device that holds it fetches it; and another error if
	/// the copy cannot be written.
	pub fn view_copy(
		&mut self,
		space: SpaceId,
		file: EntityId,
		folder: &Path,
		wall_millis: u64,
	) -> Result<PathBuf, NodeError> {
		let (file, name, content) = self.content_to_view(space, file)?;
		// Named with the digest alone, as Windows takes no colon in a name.
		let digest = super::hex(&content.digest()[..16]);
		let of_content = folder.join(format!("{VIEW_PREFIX}{digest}"));
		fs::create_dir_all(&of_content).map_err(NodeError::file(&of_content))?;
		let path = place_for(&of_content, Some(&view_name(name.as_deref())))?;
		let written = || fs::symlink_metadata(&path).is_ok_and(|found| found.is_file());
		if !written() {
			let temporary = temporary_for(&path, "view");
			remove_read_only(&temporary);
			if let Err(error) =
				self.write_copy(space, (file, name.as_deref()), &content, &temporary)
			{
				remove_leftover(&temporary);
				let _ = fs::remove_dir(&of_content);
				if matches!(error, NodeError::NoCopyHere(_)) {
					self.record_opened(space, file, wall_millis)?;
				}
				return Err(error);
			}
			// Read-only once in place: Windows would not delete a read-only temporary.
			match put_in_place(&temporary, &path) {
				Ok(()) => {
					let read_only = fs::metadata(&path).and_then(|found| {
						let mut permissions = found.permissions();
						permissions.set_readonly(true);
						fs::set_permissions(&path, permissions)
					});
					read_only.map_err(NodeError::file(&path))?;
				}
				// Another call wrote it meanwhile.
				Err(error) if error.kind() == io::ErrorKind::AlreadyExists && written() => {
					remove_read_only(&temporary);
				}
				Err(error) => {
					remove_read_only(&temporary);
					return Err(NodeError::file(&path)(error));
				}
			}
		}
		// Opened; a record that cannot be saved costs only that, the copy being written.
		let _ = self.record_opened(space, file, wall_millis);
		Ok(path)
	}

	/// The file of a space to view, as the entity it is now, its name, and its current
	/// content: any file but a secret one, a tree's too.
	fn content_to_view(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<(EntityId, Option<String>, Hash), NodeError> {
		let state = self.space(space)?.stored.log().state();
		let file = state.canonical(file)?;
		let name = state
			.current(file, &fields::NAME)?
			.iter()
			.min_by_key(|statement| statement.id)
			.and_then(|statement| statement.value.as_name_text().map(str::to_owned));
		let described = || name.clone().unwrap_or_else(|| file.to_string());
		if !state.is_deleted(file)? && state.types(file)?.contains(&SECRET_FILE) {
			return Err(NodeError::SecretFile(described()));
		}
		if state.is_deleted(file)? || !state.types(file)?.contains(&FILE) {
			return Err(NodeError::NoContent(described()));
		}
		let content = self
			.current_content(space, file)?
			.ok_or_else(|| NodeError::NoContent(described()))?;
		Ok((file, name, content))
	}

	/// The checkouts on this device of a space, that are not deleted, ordered by path, with
	/// whether each copy is unchanged.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn checkouts(&self, space: SpaceId) -> Result<Vec<Checkout>, NodeError> {
		Ok(self.records(space)?.iter().map(Record::checkout).collect())
	}

	/// The checkout on this device of a space whose copy is at `path`, as the file system
	/// resolves its folder, if there is one.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn checkout_at(&self, space: SpaceId, path: &Path) -> Result<Option<Checkout>, NodeError> {
		let Some(path) = resolved_place(path) else {
			return Ok(None);
		};
		Ok(self.record_at(space, &path)?.as_ref().map(Record::checkout))
	}

	/// The checkouts on this device of a file of a space, ordered by path.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn checkouts_of(&self, space: SpaceId, file: EntityId) -> Result<Vec<Checkout>, NodeError> {
		let file = self.space(space)?.stored.log().state().canonical(file)?;
		Ok(self
			.records(space)?
			.iter()
			.filter(|record| record.file == file)
			.map(Record::checkout)
			.collect())
	}

	/// Records the edits of a checkout's copy as a new version of its file: the copy is
	/// sealed into the store, whatever the file's policy says, and becomes the file's
	/// content, with its size and media type, and the checkout's, in one change; history
	/// keeps the version before. Returns whether the copy was edited, as its hash tells, and
	/// the new version's content hash; a copy that is unchanged or gone records nothing.
	/// Unless `force`, edits of a version that is no longer the file's are refused.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotCheckedOut`] if the checkout is not one on this device that is
	/// not deleted, [`NodeError::Stale`] if the file got a new version since the copy was
	/// written and `force` is false, and another error if the copy cannot be read, or changed
	/// while it was, or the change cannot be saved.
	pub fn record_edits(
		&mut self,
		space: SpaceId,
		checkout: EntityId,
		force: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EditsRecorded, NodeError> {
		let record = self.record(space, checkout)?;
		let copy = record.verified_state();
		if copy != CopyState::Edited {
			return Ok(EditsRecorded {
				copy,
				version: None,
			});
		}
		let current = self.current_content(space, record.file)?;
		if current != record.content && !force {
			return Err(NodeError::Stale(record.path));
		}
		let path = &record.path;
		// An edit made while the copy is read would not be what is recorded.
		let before = fs::metadata(path).map_err(NodeError::file(path))?;
		let mut head = Vec::with_capacity(HEAD);
		File::open(path)
			.and_then(|file| file.take(HEAD as u64).read_to_end(&mut head))
			.map_err(NodeError::file(path))?;
		let media_type = infer::get(&head).map(|kind| kind.mime_type().to_owned());
		let mut reading = Sniffing {
			inner: File::open(path).map_err(NodeError::file(path))?,
			size: 0,
			failed: None,
		};
		let content = self.import_blob(space, &mut reading, wall_millis, rng)?;
		let after = fs::metadata(path).map_err(NodeError::file(path))?;
		if after.len() != reading.size
			|| before.len() != reading.size
			|| after.modified().ok() != before.modified().ok()
		{
			return Err(NodeError::file(path)(io::Error::other(
				"it changed while it was read",
			)));
		}
		let size = Value::Integer(i64::try_from(reading.size).unwrap_or(i64::MAX));
		let modified = before.modified().ok().and_then(moment).map(Value::Date);
		let mut operations = self.replacing(
			space,
			record.file,
			vec![(CONTENT, Value::Blob(content)), (SIZE, size.clone())],
			rng,
		)?;
		operations.extend(self.replacing_or_clearing(
			space,
			record.file,
			MEDIA_TYPE,
			media_type.map(Value::Text),
			rng,
		)?);
		operations.extend(self.replacing(
			space,
			record.id,
			vec![(CONTENT, Value::Blob(content)), (SIZE, size)],
			rng,
		)?);
		operations.extend(self.replacing_or_clearing(space, record.id, MODIFIED, modified, rng)?);
		self.commit(space, operations, wall_millis, rng)?;
		Ok(EditsRecorded {
			copy,
			version: Some(content),
		})
	}

	/// Ends a checkout: deletes its copy, which must be unchanged, and its record. A copy
	/// whose content no other copy on this device holds, neither the store nor an original
	/// whose hash still matches, is sealed into the store first. A copy that is gone only
	/// ends its record.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotCheckedOut`] if the checkout is not one on this device that is
	/// not deleted, [`NodeError::Unrecorded`] if its copy was edited, as
	/// [`Library::record_edits`] records, and another error if the copy cannot be read or
	/// deleted, or the change cannot be saved.
	pub fn end_checkout(
		&mut self,
		space: SpaceId,
		checkout: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Ended, NodeError> {
		let record = self.record(space, checkout)?;
		let path = record.path.clone();
		let mut ended = Ended {
			path: path.clone(),
			deleted: false,
			sealed: false,
		};
		match record.verified_state() {
			CopyState::Edited => return Err(NodeError::Unrecorded(path)),
			CopyState::Gone => {}
			CopyState::Unchanged => {
				if let Some(content) = record.content
					&& !self.held_elsewhere(space, &content, &record)?
				{
					let copy = File::open(&path).map_err(NodeError::file(&path))?;
					let sealed = self.import_blob(space, copy, wall_millis, rng)?;
					if sealed != content {
						return Err(NodeError::Unrecorded(path));
					}
					ended.sealed = true;
				}
				fs::remove_file(&path).map_err(NodeError::file(&path))?;
				sync_folder_of(&path);
				ended.deleted = true;
			}
		}
		remove_leftover(&temporary_for(&path, "checkout"));
		self.commit(space, vec![deleting(record.id, rng)], wall_millis, rng)?;
		Ok(ended)
	}

	/// Deletes, in every space, the copies of this device's checkouts whose content a purge
	/// took away, where they are unchanged, and ends their checkouts, and those whose copies
	/// are gone. `known` holds the contents of this device's checkouts before the purges
	/// arrived, as [`Library::checkout_contents`] gave them, by which their hashes tell
	/// whether they are unchanged; without it, the size and modification time recorded tell.
	/// An edited copy holds the person's work, and stays; its record loses its modification
	/// time, so that no later settling takes it for unchanged. A copy whose folder cannot be
	/// reached, as on a drive that is not plugged in, waits for a later settling. Returns the
	/// copies deleted. If the change cannot be saved, as on a replica that can no longer
	/// write, a later settling makes it again.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn settle_checkouts(
		&mut self,
		known: &BTreeMap<EntityId, Hash>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<PathBuf>, NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		let mut deleted = Vec::new();
		for space in spaces {
			deleted.extend(self.settle_checkouts_in(space, known, wall_millis, rng)?);
		}
		Ok(deleted)
	}

	/// The contents of this device's checkouts, in every space, by checkout, for
	/// [`Library::settle_checkouts`] to tell later whether their copies are unchanged.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn checkout_contents(&self) -> Result<BTreeMap<EntityId, Hash>, NodeError> {
		let mut contents = BTreeMap::new();
		for &space in self.spaces.keys() {
			contents.extend(self.checkout_contents_in(space)?);
		}
		Ok(contents)
	}

	/// The contents of this device's checkouts of one space, by checkout.
	pub(crate) fn checkout_contents_in(
		&self,
		space: SpaceId,
	) -> Result<BTreeMap<EntityId, Hash>, NodeError> {
		Ok(self
			.records(space)?
			.into_iter()
			.filter_map(|record| Some((record.id, record.content?)))
			.collect())
	}

	/// Settles the checkouts of one space, as [`Library::settle_checkouts`] does.
	pub(crate) fn settle_checkouts_in(
		&mut self,
		space: SpaceId,
		known: &BTreeMap<EntityId, Hash>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<PathBuf>, NodeError> {
		let mut deleted = Vec::new();
		let mut operations = Vec::new();
		for record in self.records(space)? {
			if record.content.is_some() || !record.path.parent().is_some_and(Path::is_dir) {
				continue;
			}
			let present = match fs::symlink_metadata(&record.path) {
				Ok(metadata) => metadata.is_file(),
				Err(error) if error.kind() == io::ErrorKind::NotFound => false,
				Err(_) => continue,
			};
			if present {
				let unchanged = match known.get(&record.id) {
					Some(content) => blake3_of(&record.path).is_ok_and(|found| found == *content),
					None => record.is_untouched(),
				};
				if !unchanged {
					operations
						.extend(self.replacing_or_clearing(space, record.id, MODIFIED, None, rng)?);
					continue;
				}
				if fs::remove_file(&record.path).is_err() {
					continue;
				}
				sync_folder_of(&record.path);
				deleted.push(record.path.clone());
			}
			remove_leftover(&temporary_for(&record.path, "checkout"));
			operations.push(deleting(record.id, rng));
		}
		if !operations.is_empty() {
			let _ = self.commit(space, operations, wall_millis, rng);
		}
		// The copies of trees' files are settled the same way (technical design: trees); a later
		// settling ends what this one could not.
		deleted.extend(self.settle_trees_in(space).unwrap_or_default());
		Ok(deleted)
	}

	/// Deletes the copies of a space's checkouts on this device that are unchanged, as their
	/// hashes tell, as the space leaves the device when it drops it; an edited copy stays,
	/// the person's own file now. Nothing is recorded, as the space goes.
	pub(crate) fn delete_unedited_checkouts(&self, space: SpaceId) -> Result<(), NodeError> {
		for record in self.records(space)? {
			if record.verified_state() != CopyState::Unchanged {
				continue;
			}
			if fs::remove_file(&record.path).is_ok() {
				sync_folder_of(&record.path);
			}
			remove_leftover(&temporary_for(&record.path, "checkout"));
		}
		Ok(())
	}

	/// The checkouts on this device of a space whose content is one of `contents`, and whose
	/// copies are there, each with whether its copy was edited, as its hash tells: what a
	/// purge of the content deletes, and what it keeps.
	pub(crate) fn checkouts_holding(
		&self,
		space: SpaceId,
		contents: &BTreeSet<Hash>,
	) -> Result<Vec<(PathBuf, bool)>, NodeError> {
		let mut holding = Vec::new();
		for record in self.records(space)? {
			if record
				.content
				.is_some_and(|content| contents.contains(&content))
			{
				match record.verified_state() {
					CopyState::Gone => {}
					copy => holding.push((record.path, copy == CopyState::Edited)),
				}
			}
		}
		Ok(holding)
	}

	/// The copies of this device's checkouts of a space, each with the content it should
	/// hold, and whether it has the size and modification time recorded.
	pub(crate) fn checkout_places_here(
		&self,
		space: SpaceId,
	) -> Result<Vec<super::checking::PlainPlace>, NodeError> {
		Ok(self
			.records(space)?
			.into_iter()
			.filter_map(|record| {
				let content = record.content?;
				let path = record.path.to_string_lossy().into_owned();
				let untouched = record.is_untouched();
				Some(super::checking::PlainPlace {
					file: record.file,
					seen: super::checking::seen_at(&path).filter(|_| untouched),
					path,
					content,
					untouched,
					size: record.size.unwrap_or(0),
				})
			})
			.collect())
	}

	/// Whether the file at `path`, given as it was, is a checkout's copy on this device, by
	/// its path as the file system resolves it.
	pub(crate) fn is_checkout(&self, space: SpaceId, path: &str) -> Result<bool, NodeError> {
		// In any space, as one that moved with its file is recorded where it went.
		let path = Path::new(path);
		if self.record_at(space, path)?.is_some() {
			return Ok(true);
		}
		for &other in self.spaces.keys() {
			if other != space && self.record_at(other, path)?.is_some() {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// The file of a space to check out, as the entity it is now, its name, and its current
	/// content.
	fn content_of(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<(EntityId, Option<String>, Hash), NodeError> {
		let found = self.content_to_view(space, file)?;
		// A tree's own commands write the copies of its files (technical design: trees).
		let state = self.space(space)?.stored.log().state();
		if !state.current(found.0, &IN_TREE)?.is_empty() {
			return Err(NodeError::TreeFile(
				found.1.unwrap_or_else(|| found.0.to_string()),
			));
		}
		Ok(found)
	}

	/// The current content of a file of a space: of competing values, the one in the
	/// statement with the smallest ID.
	fn current_content(&self, space: SpaceId, file: EntityId) -> Result<Option<Hash>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let file = state.canonical(file)?;
		Ok(state
			.current(file, &CONTENT)?
			.iter()
			.filter(|statement| matches!(statement.value, Value::Blob(_)))
			.min_by_key(|statement| statement.id)
			.and_then(|statement| match statement.value {
				Value::Blob(content) => Some(content),
				_ => None,
			}))
	}

	/// Writes the content of a file, named `name`, to `temporary`, a new file: from the
	/// store, or else from an original on this device whose hash matches. Checks the content
	/// hash, syncs the file, and returns its size and modification time.
	fn write_copy(
		&self,
		space: SpaceId,
		(file, name): (EntityId, Option<&str>),
		content: &Hash,
		temporary: &Path,
	) -> Result<(u64, Option<Value>), NodeError> {
		let open = self.space(space)?;
		let mut copy = File::create_new(temporary).map_err(NodeError::file(temporary))?;
		let mut written = false;
		// A sealed copy that does not open leaves the originals to try; a secret file's copy
		// of the same content is no file's.
		if open.stored.holds_ordinary_blob(content)
			&& let Ok(mut reader) = open.stored.open_blob(content)
		{
			written = copy_checked(&mut reader, &mut copy, content)
				.map_err(NodeError::file(temporary))?;
		}
		if !written {
			for original in self.originals_here(space, content)? {
				let Ok(mut reading) = File::open(&original) else {
					continue;
				};
				copy.set_len(0)
					.and_then(|()| copy.rewind())
					.map_err(NodeError::file(temporary))?;
				if copy_checked(&mut reading, &mut copy, content)
					.map_err(NodeError::file(temporary))?
				{
					written = true;
					break;
				}
			}
		}
		if !written {
			let name = name.map_or_else(|| file.to_string(), str::to_owned);
			return Err(NodeError::NoCopyHere(name));
		}
		copy.sync_all().map_err(NodeError::file(temporary))?;
		let metadata = copy.metadata().map_err(NodeError::file(temporary))?;
		let modified = metadata.modified().ok().and_then(moment).map(Value::Date);
		Ok((metadata.len(), modified))
	}

	/// Records a checkout of a file at `path`, of `content`, whose copy has this size and
	/// modification time, and ends the checkout `replaced`, whose copy was there before, in
	/// one change. Returns the new checkout.
	fn record_checkout(
		&mut self,
		space: SpaceId,
		(file, path, content): (EntityId, &Path, Hash),
		(size, modified): (u64, Option<Value>),
		replaced: Option<EntityId>,
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<EntityId, NodeError> {
		self.install_standard(space, &[CHECKOUT], wall_millis, rng)?;
		let mut values = vec![
			NewValue::new(CHECKOUT_OF, Value::Reference(file)),
			NewValue::new(CHECKED_OUT_ON, Value::Reference(self.info.device)),
			NewValue::new(PATH, Value::Text(path.to_string_lossy().into_owned())),
			NewValue::new(CONTENT, Value::Blob(content)),
			NewValue::new(
				SIZE,
				Value::Integer(i64::try_from(size).unwrap_or(i64::MAX)),
			),
		];
		values.extend(modified.map(|modified| NewValue::new(MODIFIED, modified)));
		let (checkout, mut operations) = self.new_entity(space, &[CHECKOUT], values, rng)?;
		if let Some(replaced) = replaced {
			operations.push(deleting(replaced, rng));
		}
		self.commit(space, operations, wall_millis, rng)?;
		// The copy was checked against the content as it was written: its first check.
		self.record_first_check(
			space,
			(path.to_string_lossy().into_owned(), content),
			wall_millis,
		);
		Ok(checkout)
	}

	/// The operations that set a field of an entity to `value`, replacing its current values,
	/// or, without one, retract them.
	pub(crate) fn replacing_or_clearing(
		&self,
		space: SpaceId,
		entity: EntityId,
		field: FieldName,
		value: Option<Value>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		if let Some(value) = value {
			return self.replacing(space, entity, vec![(field, value)], rng);
		}
		let state = self.space(space)?.stored.log().state();
		let current: Vec<StatementId> = state
			.current(entity, &field)?
			.iter()
			.map(|statement| statement.id)
			.collect();
		Ok(if current.is_empty() {
			Vec::new()
		} else {
			vec![Operation::retract(entity, current)]
		})
	}

	/// Whether a copy on this device other than the checkout's holds `content`: the store,
	/// or an original whose hash still matches.
	fn held_elsewhere(
		&self,
		space: SpaceId,
		content: &Hash,
		record: &Record,
	) -> Result<bool, NodeError> {
		if self.space(space)?.stored.holds_ordinary_blob(content) {
			return Ok(true);
		}
		for original in self.originals_here(space, content)? {
			if original == record.path {
				continue;
			}
			let same_size = fs::metadata(&original)
				.is_ok_and(|metadata| record.size.is_none_or(|size| metadata.len() == size));
			if same_size && blake3_of(&original).is_ok_and(|hash| hash == *content) {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// The content of a file of a space, if it is no larger than `limit` bytes: from the copy
	/// that this replica holds, or else from a place on this device where it was found,
	/// checked against its hash, as a checkout writes it, for an app to show, such as a
	/// picture. `None` if neither has it as it was added, or it is larger. Content that only
	/// the secrets open is never read here.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn file_content(
		&self,
		space: SpaceId,
		content: &Hash,
		limit: u64,
	) -> Result<Option<Vec<u8>>, NodeError> {
		let open = self.space(space)?;
		if open.stored.holds_ordinary_blob(content)
			&& let Ok(reader) = open.stored.open_blob(content)
			&& let Some(bytes) = read_checked(reader, content, limit)
		{
			return Ok(Some(bytes));
		}
		for original in self.originals_here(space, content)? {
			if let Ok(file) = File::open(&original)
				&& let Some(bytes) = read_checked(file, content, limit)
			{
				return Ok(Some(bytes));
			}
		}
		Ok(None)
	}

	/// The paths on this device where files of a space whose content is `content` were
	/// found, each once.
	pub(crate) fn originals_here(
		&self,
		space: SpaceId,
		content: &Hash,
	) -> Result<Vec<PathBuf>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut places = BTreeSet::new();
		for file in open.stored.find(&CONTENT, &Value::Blob(*content))? {
			if !state.types(file)?.contains(&FILE) {
				continue;
			}
			for (_, device, place) in policies::origins_of(state, file)? {
				if device == self.info.device {
					places.insert(PathBuf::from(place));
				}
			}
		}
		Ok(places.into_iter().collect())
	}

	/// The checkout on this device of a space, that is not deleted, whose copy is at `path`,
	/// resolved already.
	fn record_at(&self, space: SpaceId, path: &Path) -> Result<Option<Record>, NodeError> {
		let open = self.space(space)?;
		let text = Value::Text(path.to_string_lossy().into_owned());
		for checkout in open.stored.find(&PATH, &text)? {
			if let Some(record) = self.record_of(space, checkout)?
				&& record.path == path
			{
				return Ok(Some(record));
			}
		}
		Ok(None)
	}

	/// The checkout of a space with the ID `checkout`, if it is one on this device, that is
	/// not deleted.
	fn record(&self, space: SpaceId, checkout: EntityId) -> Result<Record, NodeError> {
		self.record_of(space, checkout)?
			.ok_or_else(|| NodeError::NotCheckedOut(checkout.to_string()))
	}

	/// The checkouts on this device of a space that are not deleted, ordered by path.
	fn records(&self, space: SpaceId) -> Result<Vec<Record>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut seen = BTreeSet::new();
		let mut records = Vec::new();
		for checkout in open
			.stored
			.find(&CHECKED_OUT_ON, &Value::Reference(self.info.device))?
		{
			if seen.insert(state.canonical(checkout)?)
				&& let Some(record) = self.record_of(space, checkout)?
			{
				records.push(record);
			}
		}
		records.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
		Ok(records)
	}

	/// What a checkout of a space records, if it is one on this device that is neither
	/// deleted nor moved to another space with its file, where it is recorded then.
	fn record_of(&self, space: SpaceId, checkout: EntityId) -> Result<Option<Record>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let checkout = state.canonical(checkout)?;
		if !super::policies::stands_here(state, checkout)?
			|| !state.types(checkout)?.contains(&CHECKOUT)
		{
			return Ok(None);
		}
		let first = |field: &FieldName| -> Result<Option<Value>, NodeError> {
			Ok(state
				.current(checkout, field)?
				.iter()
				.min_by_key(|statement| statement.id)
				.map(|statement| statement.value.clone()))
		};
		let (Some(Value::Reference(on)), Some(Value::Reference(file)), Some(Value::Text(path))) =
			(first(&CHECKED_OUT_ON)?, first(&CHECKOUT_OF)?, first(&PATH)?)
		else {
			return Ok(None);
		};
		if state.canonical(on)? != self.info.device {
			return Ok(None);
		}
		Ok(Some(Record {
			id: checkout,
			file: state.canonical(file)?,
			path: PathBuf::from(path),
			content: match first(&CONTENT)? {
				Some(Value::Blob(content)) => Some(content),
				_ => None,
			},
			size: match first(&SIZE)? {
				Some(Value::Integer(size)) => u64::try_from(size).ok(),
				_ => None,
			},
			modified: first(&MODIFIED)?,
		}))
	}
}

/// The operation that deletes an entity.
fn deleting(entity: EntityId, rng: &mut impl CryptoRng) -> Operation {
	policies::assert(entity, fields::DELETED, Value::Boolean(true), rng)
}

/// Where the copy of a file named `name` goes, when it is checked out at `place`, as the
/// file system resolves the folder: in `place`, if it is a folder or is written as one,
/// ending in a separator, under the file's name, and otherwise at `place` itself.
pub(super) fn place_for(place: &Path, name: Option<&str>) -> Result<PathBuf, NodeError> {
	let as_folder = place.is_dir()
		|| place
			.as_os_str()
			.to_string_lossy()
			.ends_with(['/', std::path::MAIN_SEPARATOR]);
	let (folder, file_name) = if as_folder {
		let name = name
			.filter(|name| is_file_name(name))
			.ok_or_else(|| NodeError::NotAFileName(name.unwrap_or_default().to_owned()))?;
		(place, OsString::from(name))
	} else {
		let file_name = place
			.file_name()
			.ok_or_else(|| NodeError::NotAFileName(place.display().to_string()))?;
		let folder = match place.parent() {
			Some(parent) if !parent.as_os_str().is_empty() => parent,
			_ => Path::new("."),
		};
		(folder, file_name.to_owned())
	};
	let folder = fs::canonicalize(folder).map_err(NodeError::file(folder))?;
	Ok(plain(folder).join(file_name))
}

/// Where a copy at `path` is: its folder as the file system resolves the part of it that
/// exists, and the rest as written, so that a copy whose folder is gone can still be named.
fn resolved_place(path: &Path) -> Option<PathBuf> {
	let file_name = path.file_name()?;
	let mut existing = match path.parent() {
		Some(parent) if !parent.as_os_str().is_empty() => parent,
		_ => Path::new("."),
	};
	let mut rest = Vec::new();
	loop {
		if let Ok(resolved) = fs::canonicalize(existing) {
			let mut resolved = plain(resolved);
			resolved.extend(rest.iter().rev());
			return Some(resolved.join(file_name));
		}
		rest.push(existing.file_name()?);
		existing = existing
			.parent()
			.filter(|parent| !parent.as_os_str().is_empty())?;
	}
}

/// What the folders of copies to view are named with, before a part of the content's hash.
const VIEW_PREFIX: &str = "view-";

/// The name of a file's copy to view: the file's name, without what a file system such as
/// Windows' takes in no name, so that a file named on another system opens all the same,
/// and `file` for a file without a name that a file can have.
fn view_name(name: Option<&str>) -> String {
	let cleaned: String = name
		.unwrap_or_default()
		.chars()
		.map(|char| {
			let refused = matches!(char, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*');
			if refused || char.is_control() {
				'_'
			} else {
				char
			}
		})
		.collect();
	let cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start();
	if !is_file_name(cleaned) {
		return "file".to_owned();
	}
	// Windows keeps some names for its devices, with any extension.
	let stem = cleaned.split('.').next().unwrap_or_default().trim_end();
	let stem = stem.to_ascii_uppercase();
	let numbered = |prefix: &str| {
		stem.strip_prefix(prefix).is_some_and(|rest| {
			matches!(
				rest,
				"0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
			)
		})
	};
	if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
	{
		format!("_{cleaned}")
	} else {
		cleaned.to_owned()
	}
}

/// Deletes a file that may be read-only, as Windows deletes no such file.
fn remove_read_only(path: &Path) {
	if let Ok(found) = fs::symlink_metadata(path) {
		let mut permissions = found.permissions();
		if permissions.readonly() && !found.file_type().is_symlink() {
			#[expect(
				clippy::permissions_set_readonly_false,
				reason = "the file is deleted next"
			)]
			permissions.set_readonly(false);
			let _ = fs::set_permissions(path, permissions);
		}
	}
	let _ = fs::remove_file(path);
}

/// Removes the copies written for viewers in a program's folder ([`Library::view_copy`]),
/// as a program does when it starts: the folders named as those copies' are, with the files
/// in them, and nothing else. Returns how many copies it removed.
#[must_use]
pub fn remove_view_copies(folder: &Path) -> usize {
	let mut removed = 0;
	let Ok(entries) = fs::read_dir(folder) else {
		return 0;
	};
	for entry in entries.flatten() {
		let named = entry
			.file_name()
			.to_str()
			.is_some_and(|name| name.starts_with(VIEW_PREFIX));
		if !named || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
			continue;
		}
		let of_content = entry.path();
		for copy in fs::read_dir(&of_content).into_iter().flatten().flatten() {
			if copy.file_type().is_ok_and(|kind| !kind.is_dir()) {
				let leftover = copy.file_name().to_string_lossy().starts_with(".nib-view-");
				remove_read_only(&copy.path());
				if !leftover && fs::symlink_metadata(copy.path()).is_err() {
					removed += 1;
				}
			}
		}
		let _ = fs::remove_dir(&of_content);
	}
	removed
}

/// Whether a file's name can name a file in a folder: not empty, not a folder of its own,
/// and with no separator in it.
fn is_file_name(name: &str) -> bool {
	!name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// The hidden file beside the place of a copy that the copy is written to first, named
/// after the copy's name in short, so that a name as long as a file's can be is not made
/// too long: `.nib-`, what copy it is, such as `checkout`, and a digest of the name.
pub(super) fn temporary_for(path: &Path, what: &str) -> PathBuf {
	let name = path.file_name().map(std::ffi::OsStr::as_encoded_bytes);
	let digest = blake3::hash(name.unwrap_or_default()).to_hex();
	path.with_file_name(format!(".nib-{what}-{}", &digest[..16]))
}

/// Deletes what a checkout cut short left behind, if anything.
pub(super) fn remove_leftover(temporary: &Path) {
	let _ = fs::remove_file(temporary);
}

/// Puts the copy written to `temporary` at `path`, unless something is there, and saves the
/// folder's entries to the disk. A file system without hard links gets the copy renamed,
/// after a check that nothing is there.
pub(super) fn put_in_place(temporary: &Path, path: &Path) -> io::Result<()> {
	match fs::hard_link(temporary, path) {
		Ok(()) => remove_leftover(temporary),
		Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
		Err(_) => {
			if fs::symlink_metadata(path).is_ok() {
				return Err(io::ErrorKind::AlreadyExists.into());
			}
			fs::rename(temporary, path)?;
		}
	}
	sync_folder_of(path);
	Ok(())
}

/// Saves the entries of the folder that holds `path` to the disk, where the system allows
/// it.
pub(super) fn sync_folder_of(path: &Path) {
	#[cfg(unix)]
	if let Some(folder) = path.parent()
		&& let Ok(opened) = File::open(folder)
	{
		let _ = opened.sync_all();
	}
	#[cfg(not(unix))]
	let _ = path;
}

/// All that a reader reads, if it is no more than `limit` bytes and has the hash `content`.
fn read_checked(reader: impl Read, content: &Hash, limit: u64) -> Option<Vec<u8>> {
	let mut bytes = Vec::new();
	reader
		.take(limit.saturating_add(1))
		.read_to_end(&mut bytes)
		.ok()?;
	(bytes.len() as u64 <= limit && Hash::of(&bytes) == *content).then_some(bytes)
}

/// Copies `from` to `to`, and tells whether what was copied has the hash `content`. What
/// passes through memory is wiped, as it may be a secret file's.
pub(super) fn copy_checked(
	from: &mut impl Read,
	to: &mut File,
	content: &Hash,
) -> io::Result<bool> {
	let mut hasher = nibkeep_base::Hasher::new();
	let mut buffer = nibkeep_crypto::Zeroizing::new(vec![0; 64 * 1024]);
	loop {
		let read = match from.read(&mut buffer) {
			Ok(0) => break,
			Ok(read) => read,
			Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
			// What cannot be read is no copy of the content.
			Err(_) => return Ok(false),
		};
		to.write_all(&buffer[..read])?;
		hasher.write_all(&buffer[..read])?;
	}
	Ok(hasher.finalize() == *content)
}

#[cfg(test)]
mod tests {
	use super::view_name;

	#[test]
	fn a_copy_to_view_is_named_as_every_file_system_takes() {
		assert_eq!(view_name(Some("contract.pdf")), "contract.pdf");
		assert_eq!(
			view_name(Some("What? 3:2 <final>.txt")),
			"What_ 3_2 _final_.txt"
		);
		assert_eq!(view_name(Some("a/b\\c")), "a_b_c");
		assert_eq!(view_name(Some("notes. ")), "notes");
		assert_eq!(view_name(Some("tab\there")), "tab_here");
		assert_eq!(view_name(Some("con.pdf")), "_con.pdf");
		assert_eq!(view_name(Some("LPT1")), "_LPT1");
		assert_eq!(view_name(Some("console.txt")), "console.txt");
		for unnamed in [None, Some(""), Some(".."), Some(" . ")] {
			assert_eq!(view_name(unnamed), "file");
		}
	}
}
