//! The sync role, held in the background ([technical design](../../../docs/Technical-design.md#105-the-sync-role)),
//! as apps hold it while a library is unlocked: a thread that takes the role, or waits
//! for the program that holds it to let go, and syncs the replica for every program on it
//! until stopped. And asking the holder, whichever program it is, to sync now.

use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nibkeep_base::EntityId;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_net::Options;

use crate::{Closed, Held, Holding, NodeError, Pace, Records, Role, hold};

/// How long the thread waits after holding the role failed, as when the library could not
/// be opened, before it tries again.
const RETRY: Duration = Duration::from_secs(10);

/// How often an ask looks whether the holder answered.
const LOOK: Duration = Duration::from_millis(100);

/// How a program holds the sync role in the background: as [`Holding`] says, with what it
/// keeps of its own.
#[derive(Clone, Debug)]
pub struct BackgroundOptions {
	/// The program, as the records name it, such as `nibkeep-sync`.
	pub program: String,
	/// How its endpoint starts.
	pub network: Options,
	/// Where other replicas are, found other than on the network, as in tests.
	pub known: Vec<([u8; 32], Vec<SocketAddr>)>,
	/// How long it waits after new changes before it syncs them.
	pub quiet: Duration,
	/// How long it waits at most after another replica appears before it connects.
	pub appear_wait: Duration,
	/// How it paces its checks of the replica's copies.
	pub check: Pace,
}

impl BackgroundOptions {
	/// For `program`, with its endpoint as `network` says, the waits of
	/// [`Holding::QUIET`] and [`Holding::APPEAR_WAIT`], and checks at [`Pace::DEFAULT`].
	#[must_use]
	pub fn new(program: &str, network: Options) -> Self {
		Self {
			program: program.to_owned(),
			network,
			known: Vec::new(),
			quiet: Holding::QUIET,
			appear_wait: Holding::APPEAR_WAIT,
			check: Pace::DEFAULT,
		}
	}
}

/// What the thread that holds the sync role in the background does now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackgroundState {
	/// Whether it holds the role; if not, another program does, or did a moment ago, which
	/// it waits for to let go.
	pub holding: bool,
	/// Why holding the role failed last, until its endpoint starts again: it tries again a
	/// while later, but after an error of the keys, as when another program protected the
	/// library anew, which ends it.
	pub failed: Option<String>,
}

/// A thread that holds the sync role of a replica in the background, or waits for it, until
/// this is stopped or dropped.
pub struct Background {
	stop: Arc<AtomicBool>,
	state: Arc<Mutex<BackgroundState>>,
	directory: PathBuf,
	thread: Option<JoinHandle<()>>,
}

fn locked(state: &Mutex<BackgroundState>) -> MutexGuard<'_, BackgroundState> {
	state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Background {
	/// Starts a thread that takes the sync role of `closed`'s replica, as soon as no other
	/// program holds it, and holds it as [`hold`] does, as `options` say, until stopped:
	/// again after holding it failed, a while later. `now` is the wall clock, and `rng`
	/// makes randomness for each thread.
	pub fn start<R: CryptoRng + Send + 'static>(
		closed: Closed,
		options: BackgroundOptions,
		now: impl Fn() -> u64 + Send + Sync + 'static,
		rng: impl Fn() -> R + Send + Sync + 'static,
	) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		let state = Arc::new(Mutex::new(BackgroundState::default()));
		let directory = closed.directory().to_owned();
		let thread = {
			let (stop, state) = (Arc::clone(&stop), Arc::clone(&state));
			std::thread::spawn(move || run(&closed, &options, (&now, &rng), &stop, &state))
		};
		Self {
			stop,
			state,
			directory,
			thread: Some(thread),
		}
	}

	/// What it does now.
	#[must_use]
	pub fn state(&self) -> BackgroundState {
		locked(&self.state).clone()
	}

	/// The folder of the replica whose role it holds.
	#[must_use]
	pub fn directory(&self) -> &Path {
		&self.directory
	}

	/// Whether its thread ended, as after an error of the keys, or once stopped.
	#[must_use]
	pub fn ended(&self) -> bool {
		self.thread.as_ref().is_none_or(JoinHandle::is_finished)
	}

	/// Stops it, and waits until it let go of the role: once the syncs under way end.
	pub fn wait(mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

impl Drop for Background {
	/// Stops it without waiting: the thread lets go of the role once the syncs under way
	/// end, and a program that waits for the role, such as another of these on the same
	/// replica, takes it then.
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
	}
}

/// The thread of a [`Background`].
fn run<R: CryptoRng + Send + 'static>(
	closed: &Closed,
	options: &BackgroundOptions,
	(now, rng): (&(dyn Fn() -> u64 + Sync), &(dyn Fn() -> R + Sync)),
	stop: &AtomicBool,
	state: &Mutex<BackgroundState>,
) {
	let failed = |error: &NodeError| locked(state).failed = Some(error.to_string());
	while !stop.load(Ordering::SeqCst) {
		let role = match Role::wait(closed, stop) {
			Ok(Some(role)) => role,
			Ok(None) => return,
			Err(error) => {
				failed(&error);
				pause(stop);
				continue;
			}
		};
		locked(state).holding = true;
		let holding = Holding {
			program: &options.program,
			network: options.network,
			known: options.known.clone(),
			quiet: options.quiet,
			appear_wait: options.appear_wait,
			check: options.check,
			stop,
			now,
		};
		let held = catch_unwind(AssertUnwindSafe(|| {
			hold(closed, role, &holding, rng, &mut |held| {
				if matches!(held, Held::Started { .. }) {
					locked(state).failed = None;
				}
			})
		}))
		.unwrap_or(Err(NodeError::Panicked));
		locked(state).holding = false;
		match held {
			Ok(()) => {}
			// Another program protected the library anew: its keys here are old.
			Err(error @ NodeError::Keys(_)) => {
				failed(&error);
				return;
			}
			Err(error) => {
				failed(&error);
				pause(stop);
			}
		}
	}
}

/// Waits for [`RETRY`], or less if stopped.
fn pause(stop: &AtomicBool) {
	let paused = Instant::now();
	while paused.elapsed() < RETRY && !stop.load(Ordering::SeqCst) {
		std::thread::sleep(LOOK);
	}
}

/// Asks the program that holds the sync role of the replica, whichever it is, to sync with
/// one other replica, or with all it finds, and waits until it did, for at most `wait`:
/// waits first for a program to take the role, if none holds it, and for one that just
/// took it to look for the others on the network, which it connects to at once in its
/// first while ([`Holding::APPEAR_WAIT`]). Returns when it asked, after which the holder
/// recorded each sync of the answer ([`crate::ReplicaRecord::synced`]); or `None` if no
/// program answered in time. `now` is the wall clock, as the holder's is.
///
/// # Errors
///
/// Returns an error if the records cannot be read or written.
pub fn ask_to_sync(
	records: &Records,
	replica: Option<EntityId>,
	now: &dyn Fn() -> u64,
	wait: Duration,
) -> Result<Option<u64>, NodeError> {
	ask_to_sync_each(records, &[replica], now, wait)
}

/// Asks as [`ask_to_sync`] does, for a sync with each of several replicas, as when a file is
/// fetched from the devices that hold it, and waits until the holder answered them all.
///
/// # Errors
///
/// Returns an error if the records cannot be read or written.
pub fn ask_to_sync_each(
	records: &Records,
	replicas: &[Option<EntityId>],
	now: &dyn Fn() -> u64,
	wait: Duration,
) -> Result<Option<u64>, NodeError> {
	let started = Instant::now();
	let holder = loop {
		if let Some(holder) = records.holder()? {
			break holder;
		}
		if started.elapsed() > wait {
			return Ok(None);
		}
		std::thread::sleep(LOOK);
	};
	let settled = holder
		.since
		.saturating_add(u64::try_from(Holding::APPEAR_WAIT.as_millis()).unwrap_or(u64::MAX));
	if let Some(early) = settled.checked_sub(now()) {
		std::thread::sleep(Duration::from_millis(early).min(wait));
	}
	let asked = now();
	let asks = replicas
		.iter()
		.map(|replica| records.ask(*replica, asked))
		.collect::<Result<Vec<_>, _>>()?;
	for ask in asks {
		while records.answered(ask)?.is_none() {
			if started.elapsed() > wait {
				return Ok(None);
			}
			std::thread::sleep(LOOK);
		}
	}
	Ok(Some(asked))
}
