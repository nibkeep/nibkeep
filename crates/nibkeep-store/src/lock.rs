//! The lock of a space, in its file `lock`: every stored log that has the space open holds it
//! shared, and one holds it alone only while it must run alone, as when it makes the state
//! file anew, or puts the space in order after a crash ([technical design: several programs on one replica]). It is a lock
//! of the open file, as Rust's `File::lock` takes them, `flock` on Unix and `LockFileEx` on
//! Windows, so that two stored logs of one space in one program exclude each other as two
//! programs do. The system lets go of it when the program ends, even by a crash.
//!
//! Neither system converts a shared lock to one held alone in place, Windows not at all and
//! `flock` not atomically, so a stored log lets go of its lock before it takes it the other
//! way. Meanwhile it holds the file `gate` locked: so does every other that changes its
//! lock, so that none takes the lock alone while another has let go of its own on the way,
//! and still has the space open.
//!
//! [technical design: several programs on one replica]: ../../../docs/Technical-design.md#78-several-programs-on-one-replica

use std::cell::Cell;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::StoreError;

/// How long opening waits for another stored log that holds the space alone, as while it
/// opens it and puts it in order, which takes a moment: as long as SQLite waits for a lock.
const OPEN_WAIT: Duration = Duration::from_secs(5);

/// How often opening tries again meanwhile.
const OPEN_TRY: Duration = Duration::from_millis(10);

/// A space's lock, held shared or alone.
#[derive(Debug)]
pub(crate) struct SpaceLock {
	file: File,
	/// The gate, which a stored log locks while it changes its lock.
	gate: File,
	/// Whether it is held alone now.
	alone: Cell<bool>,
}

fn open(path: &Path) -> Result<File, StoreError> {
	Ok(OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(path)?)
}

/// Holds a gate locked until it goes.
struct Gated<'a>(&'a File);

impl Drop for Gated<'_> {
	fn drop(&mut self) {
		let _ = self.0.unlock();
	}
}

impl SpaceLock {
	/// Takes the lock of a space, `lock` in its folder, shared, waiting up to [`OPEN_WAIT`]
	/// for another stored log that holds it alone; `gate` beside it is the gate.
	///
	/// # Errors
	///
	/// Returns [`StoreError::InUse`] if another stored log holds it alone longer, as while it
	/// makes the state file of a large space anew, and an error if the files cannot be
	/// opened or the file system takes no locks.
	pub(crate) fn shared(path: &Path) -> Result<Self, StoreError> {
		let file = open(path)?;
		let gate = open(&path.with_file_name("gate"))?;
		let waited = Instant::now();
		loop {
			match file.try_lock_shared() {
				Ok(()) => {
					return Ok(Self {
						file,
						gate,
						alone: Cell::new(false),
					});
				}
				Err(TryLockError::WouldBlock) if waited.elapsed() < OPEN_WAIT => {
					std::thread::sleep(OPEN_TRY);
				}
				Err(TryLockError::WouldBlock) => return Err(StoreError::InUse),
				Err(TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => {
					return Err(StoreError::Unsupported("file locks"));
				}
				Err(TryLockError::Error(error)) => return Err(error.into()),
			}
		}
	}

	/// Locks the gate, waiting for another stored log that changes its lock: a moment.
	fn gated(&self) -> Option<Gated<'_>> {
		self.gate.lock().ok().map(|()| Gated(&self.gate))
	}

	/// Holds the lock alone, if no other stored log, here or in another program, has the
	/// space open: whether it does. If another has, the lock stays shared.
	pub(crate) fn alone(&self) -> bool {
		if self.alone.get() {
			return true;
		}
		let Some(_gated) = self.gated() else {
			return false;
		};
		let _ = self.file.unlock();
		if self.file.try_lock().is_ok() {
			self.alone.set(true);
			return true;
		}
		// No other takes it alone meanwhile: it would need the gate.
		let _ = self.file.lock_shared();
		false
	}

	/// Whether another stored log, here or in another program, has the space open now.
	pub(crate) fn shared_with_others(&self) -> bool {
		if self.alone.get() {
			return false;
		}
		let Some(_gated) = self.gated() else {
			return true;
		};
		let _ = self.file.unlock();
		let alone = self.file.try_lock().is_ok();
		if alone {
			let _ = self.file.unlock();
		}
		let _ = self.file.lock_shared();
		!alone
	}

	/// Whether it is held alone now.
	pub(crate) fn is_alone(&self) -> bool {
		self.alone.get()
	}

	/// Shares the lock again, if it was held alone.
	pub(crate) fn share(&self) {
		if self.alone.get() {
			let _gated = self.gated();
			let _ = self.file.unlock();
			let _ = self.file.lock_shared();
			self.alone.set(false);
		}
	}
}
