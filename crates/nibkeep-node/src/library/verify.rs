//! Checking a library: its log files and its blobs, and the files kept where they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_log::ChangeId;
use nibkeep_model::{Value, fields};
use nibkeep_std::{CONTENT, FILE};
use nibkeep_store::{SealedCheck, StoreError, StoredLog};

use super::export::blake3_of;
use super::{Library, policies};
use crate::NodeError;

/// What checking a library found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
	/// Each space, the personal space first.
	pub spaces: Vec<SpaceReport>,
}

/// What checking a space found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceReport {
	/// The space.
	pub id: SpaceId,
	/// Its name, if it has one.
	pub name: Option<String>,
	/// How many changes were checked, accepted or waiting: their signatures now, the rest
	/// when the library was opened.
	pub changes: usize,
	/// How many blobs were read in full.
	pub blobs: usize,
	/// How many sealed files have keys that the replica cannot open yet, such as keys of a
	/// space key it has not received, or of secret files while the secrets are locked. They
	/// wait for their keys, and are not read.
	pub waiting: usize,
	/// How many files that this device keeps where they were found, and not in the store,
	/// were read there in full and still hold their content.
	pub in_place: usize,
	/// The places where this device found a file it keeps only there, which no longer hold
	/// its content, or cannot be read: changed, gone or locked away, so that they no longer
	/// count as copies. Nothing is wrong with the library for them.
	pub changed: Vec<PathBuf>,
	/// What is wrong, if anything.
	pub problems: Vec<Problem>,
}

/// Something wrong with a space.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Problem {
	/// SQLite's check of the log file found it damaged, or could not run.
	Database(String),
	/// A stored change's signature is not valid: the log file was damaged or tampered
	/// with. Opening the library trusts the signatures checked when changes arrived.
	ForgedChange(ChangeId),
	/// A stored change cannot be read, as the error says, such as one that this replica did
	/// not store: the log file was damaged or tampered with.
	DamagedChange(ChangeId, String),
	/// The replica's copy of a blob is damaged: it is moved aside, and a sync brings it again
	/// from a replica that holds it intact.
	DamagedBlob(Hash),
	/// The replica's copy of a blob is intact, but its key does not open it to its content:
	/// the key is wrong.
	WrongBlobKey(Hash),
	/// The replica's copy of a blob cannot be read, as the error says.
	UnreadableBlob(Hash, String),
	/// Some stored keys of a sealed blob were bad, and opening the library removed them.
	BadBlobKeys(Hash),
	/// No key of any space of the replica opens a sealed blob, and no purge explains why:
	/// unless another replica sends one, its content is lost. The replica's spaces share
	/// their blob directory, so the personal space's report names these.
	KeylessBlob(Hash),
	/// A file in the replica's blob directory is not a sealed blob, which the personal
	/// space's report names.
	UnknownFile(PathBuf),
	/// A check could not be finished, as the error says.
	Unchecked(String),
}

impl Report {
	/// How many problems checking found.
	#[must_use]
	pub fn problems(&self) -> usize {
		self.spaces.iter().map(|space| space.problems.len()).sum()
	}

	/// Whether nothing is wrong.
	#[must_use]
	pub fn is_sound(&self) -> bool {
		self.problems() == 0
	}
}

impl Library {
	/// Checks the library: SQLite's check of every log file, the signature of every
	/// change, and every blob that the replica can read, read in full against its content
	/// hash. The rest of every change was checked again, and bad stored keys removed, when
	/// the library was opened; the report names the keys removed. Blobs found intact are
	/// recorded as checked at `wall_millis`; a damaged copy is moved aside, so that a sync
	/// brings it again, as the regular check does ([technical design: checking and repair]).
	/// Files that this device keeps where they were found are read there and checked against
	/// their hashes too, and those whose size and modification time are unchanged recorded as
	/// checked. What one check cannot finish is a problem in the report, and the other checks
	/// go on.
	///
	/// [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair
	///
	/// # Errors
	///
	/// Returns an error if a batch of changes is open, whose changes are not saved yet, or
	/// if the library has a space it did not open, which it never should.
	pub fn verify(&mut self, wall_millis: u64) -> Result<Report, NodeError> {
		if self.spaces.values().any(|open| open.stored.in_batch()) {
			return Err(StoreError::InBatch.into());
		}
		let mut ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
		ids.sort_by_key(|&id| (id != self.info.personal, id));
		// What each space explains of the blob directory that they share.
		let mut known = BTreeMap::new();
		for (&id, open) in &self.spaces {
			known.insert(id, open.stored.known_blobs());
		}
		let mut spaces = Vec::new();
		for id in ids {
			let (name, unnamed) = match self.space(id)?.name(&self.languages) {
				Ok(name) => (name, None),
				Err(error) => (None, Some(Problem::Unchecked(error.to_string()))),
			};
			let open = self.spaces.get_mut(&id).ok_or(NodeError::NoSpace(id))?;
			let mut report = SpaceReport {
				id,
				name,
				changes: open.stored.log().kept()?,
				blobs: 0,
				waiting: 0,
				in_place: 0,
				changed: Vec::new(),
				problems: unnamed.into_iter().collect(),
			};
			// Without what another space explains, the files that no space explains are not
			// told apart; the space's own checks go on.
			let mut elsewhere = BTreeSet::new();
			let mut strays = id == self.info.personal;
			for (_, other) in known.iter().filter(|(other, _)| **other != id) {
				match other {
					Ok(other) => elsewhere.extend(other),
					Err(error) => {
						if strays {
							report.problems.push(Problem::Unchecked(error.to_string()));
						}
						strays = false;
					}
				}
			}
			check_space(
				&mut open.stored,
				(&elsewhere, strays),
				wall_millis,
				&mut report,
			);
			let open = self.spaces.get_mut(&id).ok_or(NodeError::NoSpace(id))?;
			let read = check_in_place(&open.stored, self.info.device, &mut report);
			// Every unchanged plain copy is recorded as checked, held or not, those read
			// above without reading them again.
			if let Err(error) = self.check_plain_now(id, &read, wall_millis) {
				report.problems.push(Problem::Unchecked(error.to_string()));
			}
			spaces.push(report);
		}
		// What was moved aside is recorded in every space that keeps a key for it.
		self.note_aside(wall_millis);
		Ok(Report { spaces })
	}
}

/// Checks a space's log file and blobs. The sealed hashes that the replica's other spaces
/// explain are `elsewhere`; the files that no space explains, and those that are no sealed
/// blobs, are named only where `strays` says, as all spaces share the blob directory.
fn check_space(
	stored: &mut StoredLog,
	(elsewhere, strays): (&BTreeSet<Hash>, bool),
	wall_millis: u64,
	report: &mut SpaceReport,
) {
	match stored.integrity_problems() {
		Ok(found) => report
			.problems
			.extend(found.into_iter().map(Problem::Database)),
		Err(error) => report.problems.push(Problem::Database(error.to_string())),
	}
	match stored.log().forged() {
		Ok(forged) => report
			.problems
			.extend(forged.into_iter().map(|(id, unreadable)| match unreadable {
				None => Problem::ForgedChange(id),
				Some(error) => Problem::DamagedChange(id, error.to_string()),
			})),
		Err(error) => report.problems.push(Problem::Unchecked(error.to_string())),
	}
	report.problems.extend(
		stored
			.blob_keys_dropped_at_open()
			.iter()
			.copied()
			.map(Problem::BadBlobKeys),
	);
	let inventory = match stored.blob_inventory(elsewhere) {
		Ok(inventory) => inventory,
		Err(error) => {
			report.problems.push(Problem::Unchecked(error.to_string()));
			return;
		}
	};
	report.waiting = inventory.waiting;
	if strays {
		report
			.problems
			.extend(inventory.keyless.into_iter().map(Problem::KeylessBlob));
		report
			.problems
			.extend(inventory.unknown.into_iter().map(Problem::UnknownFile));
	}
	let mut damaged = BTreeSet::new();
	for content in inventory.held {
		report.blobs += 1;
		match stored.verify_blob(&content, wall_millis) {
			Ok(()) => {}
			Err(StoreError::DamagedBlob) => {
				damaged.insert(content);
				report.problems.push(damaged_or_wrong_key(stored, &content));
			}
			Err(error @ (StoreError::Io(_) | StoreError::MissingBlob)) => report
				.problems
				.push(Problem::UnreadableBlob(content, error.to_string())),
			Err(error) => {
				report.problems.push(Problem::Unchecked(error.to_string()));
				return;
			}
		}
	}
	// The copies kept aside, moved by an earlier check or by this one in another space,
	// that still read damaged: each one put back, as intact after all, is no problem.
	for sealed in stored.aside_blobs() {
		let content = stored.log().content_of_sealed(&sealed).unwrap_or(sealed);
		if damaged.contains(&content) {
			continue;
		}
		match stored.recheck_aside_blob(&sealed, &|| false) {
			Ok(SealedCheck::Intact(_) | SealedCheck::Gone) => {}
			Ok(_) => {
				damaged.insert(content);
				report.problems.push(Problem::DamagedBlob(content));
			}
			Err(error) => report
				.problems
				.push(Problem::UnreadableBlob(content, error.to_string())),
		}
	}
}

/// What a copy of `content` that did not open to its content is: damaged, where a copy's
/// bytes differ from its sealed hash, which it then moves aside; held under a wrong key,
/// where every copy's bytes are intact; or else unreadable, as when a copy could not be read
/// again or moved.
fn damaged_or_wrong_key(stored: &StoredLog, content: &Hash) -> Problem {
	let sealed: Vec<Hash> = stored
		.log()
		.blob_keys(content)
		.map(|(sealed, _)| *sealed)
		.filter(|sealed| stored.holds_sealed(sealed))
		.collect();
	let mut moved = false;
	let mut failed = None;
	for each in sealed {
		match stored.check_sealed_blob(&each, &|| false) {
			Ok(SealedCheck::Damaged) => match stored.set_aside_blob(&each) {
				Ok(true) => moved = true,
				Ok(false) => {}
				Err(error) => {
					failed = Some(format!("damaged, but it could not be moved aside: {error}"));
				}
			},
			Ok(_) => {}
			Err(error) => failed = Some(error.to_string()),
		}
	}
	match (moved, failed) {
		(true, _) => Problem::DamagedBlob(*content),
		(false, Some(failed)) => Problem::UnreadableBlob(*content, failed),
		(false, None) => Problem::WrongBlobKey(*content),
	}
}

/// Checks the files of a space that this device keeps only where it found them: reads each
/// place on `device` that an origin names, and tells whether it still holds the content.
/// Returns the hash of each place it read.
fn check_in_place(
	stored: &StoredLog,
	device: EntityId,
	report: &mut SpaceReport,
) -> BTreeMap<String, Hash> {
	let mut read = BTreeMap::new();
	let files = match stored.find(&fields::TYPE, &Value::Text(FILE.to_string())) {
		Ok(files) => files,
		Err(error) => {
			report.problems.push(Problem::Unchecked(error.to_string()));
			return read;
		}
	};
	let mut checked = BTreeSet::new();
	for file in files {
		match kept_in_place(stored, device, file, &mut checked) {
			Ok(Some((content, places))) => {
				let mut held = false;
				for place in places {
					let found = blake3_of(Path::new(&place));
					if found.as_ref().is_ok_and(|found| *found == content) {
						held = true;
					} else {
						report.changed.push(PathBuf::from(&place));
					}
					if let Ok(found) = found {
						read.insert(place, found);
					}
				}
				report.in_place += usize::from(held);
			}
			Ok(None) => {}
			Err(error) => {
				report.problems.push(Problem::Unchecked(error.to_string()));
				return read;
			}
		}
	}
	read
}

/// The content of a file that this device, `device`, keeps only where it found it, and the
/// places there, unless the file is deleted, its content is in the store, or it was checked
/// already, as `checked` notes.
fn kept_in_place(
	stored: &StoredLog,
	device: EntityId,
	file: EntityId,
	checked: &mut BTreeSet<EntityId>,
) -> Result<Option<(Hash, Vec<String>)>, NodeError> {
	let state = stored.log().state();
	let file = state.canonical(file)?;
	if !checked.insert(file) || state.is_deleted(file)? {
		return Ok(None);
	}
	let content =
		state
			.current(file, &CONTENT)?
			.iter()
			.find_map(|statement| match statement.value {
				Value::Blob(hash) => Some(hash),
				_ => None,
			});
	let Some(content) = content.filter(|content| !stored.holds_ordinary_blob(content)) else {
		return Ok(None);
	};
	let places = policies::origins_of(state, file)?
		.into_iter()
		.filter(|(_, on, _)| *on == device)
		.map(|(_, _, place)| place)
		.collect();
	Ok(Some((content, places)))
}
