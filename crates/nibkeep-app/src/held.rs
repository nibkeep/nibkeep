//! The library while it is unlocked: held here between the app's calls, closed, and opened
//! for each, so that other programs, such as `nib` and `nib sync --serve`, can use it in
//! between; with the key of its secrets while they are unlocked, in an app that reads them.
//!
//! Locking never waits for a call that holds the library: the call lets go of it, or of its
//! secrets, when it ends ([`Held`]).

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

use nibkeep_node::{Closed, Library, NodeError};

use crate::{Failure, joining, pairing, recovery, syncing};

/// The library while it is unlocked.
static LIBRARY: Mutex<Option<Closed>> = Mutex::new(None);

/// How many times the library, and its secrets, were locked.
static LOCKS: AtomicU64 = AtomicU64::new(0);
static SECRETS_LOCKS: AtomicU64 = AtomicU64::new(0);

/// How long a call waits for another program that has the library to itself, as while it
/// makes a space's state file anew after a purge; tests wait less.
pub const WAIT_FOR_OTHERS: Duration = if cfg!(any(test, feature = "testing")) {
	Duration::from_secs(1)
} else {
	Duration::from_secs(3)
};

/// The library's lock, held for a call, and how many times the library and its secrets
/// were locked when the call began: if they were locked meanwhile, the call lets go of them
/// when it ends. It is the library, closed, or `None` while it is locked.
pub struct Held {
	guard: MutexGuard<'static, Option<Closed>>,
	mark: Mark,
}

impl Held {
	/// Whether the library was not locked since the call began.
	#[must_use]
	pub fn unchanged(&self) -> bool {
		LOCKS.load(Ordering::SeqCst) == self.mark.locks
	}
}

impl Deref for Held {
	type Target = Option<Closed>;

	fn deref(&self) -> &Self::Target {
		&self.guard
	}
}

impl DerefMut for Held {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.guard
	}
}

impl Drop for Held {
	fn drop(&mut self) {
		if !self.unchanged() {
			*self.guard = None;
		} else if SECRETS_LOCKS.load(Ordering::SeqCst) != self.mark.secrets_locks
			&& let Some(closed) = self.guard.as_mut()
		{
			closed.lock_secrets();
		}
	}
}

/// How many times the library and its secrets were locked by a moment: work that must not
/// hold the library while it waits, as for the person to confirm, checks afterwards that
/// neither was locked meanwhile.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
	locks: u64,
	secrets_locks: u64,
}

impl Mark {
	/// The locks so far.
	fn now() -> Self {
		Self {
			locks: LOCKS.load(Ordering::SeqCst),
			secrets_locks: SECRETS_LOCKS.load(Ordering::SeqCst),
		}
	}

	/// Whether the library and its secrets stayed unlocked since the mark: if the library
	/// was locked, [`Failure::Locked`], and if the secrets were, [`Failure::SecretsLocked`].
	///
	/// # Errors
	///
	/// As said.
	pub fn check(&self) -> Result<(), Failure> {
		if LOCKS.load(Ordering::SeqCst) != self.locks {
			return Err(Failure::Locked);
		}
		if SECRETS_LOCKS.load(Ordering::SeqCst) != self.secrets_locks {
			return Err(Failure::SecretsLocked);
		}
		Ok(())
	}
}

/// Holds the library for a call, waiting for the call that holds it.
pub fn hold() -> Held {
	let mark = Mark::now();
	Held {
		guard: LIBRARY.lock().unwrap_or_else(PoisonError::into_inner),
		mark,
	}
}

/// The library's lock, unless a call holds it.
fn try_hold() -> Option<MutexGuard<'static, Option<Closed>>> {
	match LIBRARY.try_lock() {
		Ok(guard) => Some(guard),
		Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
		Err(TryLockError::WouldBlock) => None,
	}
}

/// Whether no call holds the library now.
#[must_use]
pub fn idle() -> bool {
	try_hold().is_some()
}

/// The directory of the library unlocked, and a [`Mark`] of the locks so far, read while
/// it is held for a moment.
///
/// # Errors
///
/// [`Failure::Locked`] if the library is locked.
pub fn marked() -> Result<(PathBuf, Mark), Failure> {
	let held = hold();
	let Some(closed) = held.as_ref() else {
		return Err(Failure::Locked);
	};
	Ok((closed.directory().to_owned(), held.mark))
}

/// Does `work` with the library unlocked, opened for it and closed after.
///
/// # Errors
///
/// [`Failure::Locked`] if the library is locked, also where another program protected its
/// keys anew, which locks it until the person unlocks it again; [`Failure::Busy`] if
/// another program has it to itself; and what `work` fails with.
pub fn with_open<T>(work: impl FnOnce(&mut Library) -> Result<T, Failure>) -> Result<T, Failure> {
	let mut held = hold();
	let Some(closed) = held.take() else {
		return Err(Failure::Locked);
	};
	let mut library = match waiting(|| closed.open()) {
		Ok(library) => library,
		Err(Failure::Node(NodeError::Keys(_))) => {
			drop(held);
			lock();
			return Err(Failure::Locked);
		}
		Err(failure) => {
			*held = Some(closed);
			return Err(failure);
		}
	};
	let done = work(&mut library);
	*held = Some(library.close());
	done
}

/// Opens a library, waiting a little for another program that has it open.
///
/// # Errors
///
/// [`Failure::Busy`] if the other program keeps it longer than [`WAIT_FOR_OTHERS`], and
/// what opening fails with.
pub fn waiting(open: impl FnMut() -> Result<Library, NodeError>) -> Result<Library, Failure> {
	match nibkeep_node::retry_in_use(WAIT_FOR_OTHERS, open) {
		Err(error) if error.in_use() => Err(Failure::Busy),
		opened => Ok(opened?),
	}
}

/// Keeps a library that the app just unlocked, made or joined, closed, for the calls that
/// follow: what the library held before did on the network stops first.
pub fn keep(library: Library) {
	let_go();
	*hold() = Some(library.close());
}

/// Locks the library at once: forgets it, or has the call that holds it forget it when it
/// ends; stops what it did on the network, as [`let_go`] says; and forgets the recovery code
/// shown last, which was never named.
pub fn lock() {
	LOCKS.fetch_add(1, Ordering::SeqCst);
	if let Some(mut held) = try_hold() {
		*held = None;
	}
	let_go();
	recovery::forget_shown();
}

/// Locks the secrets at once, as [`lock`] locks the library, and stops adding a device that
/// would be given them.
pub fn lock_secrets() {
	SECRETS_LOCKS.fetch_add(1, Ordering::SeqCst);
	if let Some(mut held) = try_hold()
		&& let Some(closed) = held.as_mut()
	{
		closed.lock_secrets();
	}
	pairing::stop_giving();
}

/// Stops what the library that the app held did on the network, as it goes: syncing,
/// adding a device and joining a space; and what the app itself stops and forgets
/// ([`crate::App::let_go`]).
pub fn let_go() {
	syncing::stop_serving();
	pairing::stop_adding();
	joining::stop();
	if let Some(app) = crate::APP.get() {
		(app.let_go)();
	}
}
