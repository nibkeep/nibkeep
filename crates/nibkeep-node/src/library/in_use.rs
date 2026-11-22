//! The guard of a replica on a drive without a journal against a removal without unmounting,
//! as the technical design's section on drives without a journal decides: a file that says that the replica is open, and locks that
//! tell whether any program has it open. A program that opens the replica, finds the file
//! and no other program with it open, knows that the drive went away while it was open.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::NodeError;
use crate::place;

/// The file that says that the replica is open.
const MARK: &str = "in-use";

/// The file that every program holds a shared lock on while it has the replica open.
const LOCK: &str = "in-use.lock";

/// The file that a program locks alone while it looks at the others, so that two programs
/// that open or close the replica at once do not take each other for gone.
const GATE: &str = "in-use.gate";

/// A replica held open on a drive without a journal: the shared lock, given up when it goes,
/// and the mark removed then if no other program has the replica open.
#[derive(Debug)]
pub(crate) struct InUse {
	folder: PathBuf,
	lock: File,
}

fn open(path: &Path) -> Result<File, NodeError> {
	OpenOptions::new()
		.create(true)
		.truncate(false)
		.read(true)
		.write(true)
		.open(path)
		.map_err(NodeError::file(path))
}

/// Locks the gate of a replica's folder, until the file it returns goes.
fn gate(folder: &Path) -> Result<File, NodeError> {
	let path = folder.join(GATE);
	let gate = open(&path)?;
	gate.lock().map_err(NodeError::file(&path))?;
	Ok(gate)
}

/// Syncs a folder, where the system allows it, so that a file made or removed in it lasts.
fn sync_folder(folder: &Path) {
	#[cfg(unix)]
	let _ = File::open(folder).and_then(|folder| folder.sync_all());
	#[cfg(not(unix))]
	let _ = folder;
}

impl InUse {
	/// Holds the replica in `folder` open, if its drive keeps no journal; `None` on others.
	///
	/// # Errors
	///
	/// Returns [`NodeError::UncleanRemoval`] if the mark is there while no program has the
	/// replica open, and an error if the files cannot be written.
	pub(crate) fn take(folder: &Path) -> Result<Option<Self>, NodeError> {
		if !place::whereabouts(folder).drive.unjournaled {
			return Ok(None);
		}
		let gate = gate(folder)?;
		let path = folder.join(LOCK);
		let lock = open(&path)?;
		let mark = folder.join(MARK);
		match lock.try_lock() {
			// No other program has the replica open.
			Ok(()) => {
				if mark.exists() {
					return Err(NodeError::UncleanRemoval);
				}
				File::create(&mark)
					.and_then(|made| made.sync_all())
					.map_err(NodeError::file(&mark))?;
				sync_folder(folder);
				lock.unlock().map_err(NodeError::file(&path))?;
			}
			Err(TryLockError::WouldBlock) => {}
			Err(TryLockError::Error(error)) => return Err(NodeError::file(&path)(error)),
		}
		lock.lock_shared().map_err(NodeError::file(&path))?;
		drop(gate);
		Ok(Some(Self {
			folder: folder.to_owned(),
			lock,
		}))
	}
}

impl Drop for InUse {
	fn drop(&mut self) {
		let Ok(_gate) = gate(&self.folder) else {
			return;
		};
		let _ = self.lock.unlock();
		// The last program to close the replica removes the mark.
		if self.lock.try_lock().is_ok() {
			let _ = fs::remove_file(self.folder.join(MARK));
			sync_folder(&self.folder);
			let _ = self.lock.unlock();
		}
	}
}

/// Whether the drive of the replica in `folder` was removed, or lost its power, while the
/// replica was open: the mark is there, and no program has the replica open.
///
/// # Errors
///
/// Returns an error if the files cannot be written.
pub(crate) fn removed(folder: &Path) -> Result<bool, NodeError> {
	if !place::whereabouts(folder).drive.unjournaled || !folder.join(MARK).exists() {
		return Ok(false);
	}
	let _gate = gate(folder)?;
	let path = folder.join(LOCK);
	let lock = open(&path)?;
	match lock.try_lock() {
		Ok(()) => {
			let removed = folder.join(MARK).exists();
			lock.unlock().map_err(NodeError::file(&path))?;
			Ok(removed)
		}
		Err(TryLockError::WouldBlock) => Ok(false),
		Err(TryLockError::Error(error)) => Err(NodeError::file(&path)(error)),
	}
}

/// Says that the drive of the replica in `folder` was checked after a removal without
/// unmounting: removes the mark, unless a program has the replica open, which it then is.
///
/// # Errors
///
/// Returns an error if the files cannot be written.
pub(crate) fn checked(folder: &Path) -> Result<(), NodeError> {
	if !place::whereabouts(folder).drive.unjournaled {
		return Ok(());
	}
	let _gate = gate(folder)?;
	let path = folder.join(LOCK);
	let lock = open(&path)?;
	if lock.try_lock().is_ok() {
		let mark = folder.join(MARK);
		if mark.exists() {
			fs::remove_file(&mark).map_err(NodeError::file(&mark))?;
			sync_folder(folder);
		}
		lock.unlock().map_err(NodeError::file(&path))?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::place::{Drive, declare_drive};

	/// A folder on a drive without a journal, for one test.
	fn on_fat(name: &str) -> PathBuf {
		let folder =
			std::env::temp_dir().join(format!("nibkeep-in-use-{name}-{}", std::process::id()));
		let _ = fs::remove_dir_all(&folder);
		fs::create_dir_all(&folder).expect("makes the folder");
		declare_drive(
			&folder,
			Drive {
				unjournaled: true,
				..Drive::default()
			},
			None,
		);
		folder
	}

	#[test]
	fn programs_that_have_a_replica_open_at_once_leave_it_marked_until_the_last_goes() {
		let folder = on_fat("two");
		// Each holds a file of its own, as two programs do.
		let first = InUse::take(&folder).expect("takes").expect("guards");
		let second = InUse::take(&folder)
			.expect("takes beside the first")
			.expect("guards");
		assert!(folder.join(MARK).exists());
		drop(first);
		assert!(folder.join(MARK).exists(), "the second has it open");
		// A third that comes meanwhile takes the second for open, not gone.
		let third = InUse::take(&folder).expect("takes").expect("guards");
		drop(second);
		drop(third);
		assert!(!folder.join(MARK).exists(), "the last removed it");
		// Saying that the drive was checked while a program has the replica open leaves the
		// mark that it holds.
		let open = InUse::take(&folder).expect("takes").expect("guards");
		checked(&folder).expect("says so");
		assert!(folder.join(MARK).exists());
		drop(open);
		fs::remove_dir_all(&folder).expect("removes");
	}

	#[test]
	fn a_mark_without_a_program_is_a_removal_until_the_drive_is_checked() {
		let folder = on_fat("removed");
		fs::write(folder.join(MARK), b"").expect("writes");
		assert!(matches!(
			InUse::take(&folder),
			Err(NodeError::UncleanRemoval)
		));
		assert!(removed(&folder).expect("tells"));
		checked(&folder).expect("says so");
		assert!(!removed(&folder).expect("tells"));
		let open = InUse::take(&folder).expect("takes").expect("guards");
		assert!(!removed(&folder).expect("tells"), "open, not removed");
		drop(open);
		fs::remove_dir_all(&folder).expect("removes");
	}
}
