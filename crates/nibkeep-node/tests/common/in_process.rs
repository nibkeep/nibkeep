//! Two libraries in one process: pairing them over connections on localhost, and syncing
//! them in memory. Shared by the tests and the power-cut example, which runs on a device of
//! its own, so it needs nothing of the tests' own build.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	Joining, Library, NetworkOptions, NewProtection, NodeError, PairingCode, SyncError, Synced,
	Syncing, join,
};
use nibkeep_sync::{Transport, TransportError};
use nibkeep_testkit::{TestRng, millis_at};

/// How long a test waits for the other side over the network.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// How a library made by pairing protects its keys: a quick passphrase.
pub(crate) fn protection() -> NewProtection<'static> {
	NewProtection::Passphrase(
		"battery staple",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	)
}

/// Pairs `joining` in `directory` with `sponsor`, which shows the code `shown`, that the new
/// replica types as `typed`, over connections on localhost; with `give`, the sponsor gives
/// the secrets, if the new replica asks for them.
pub(crate) fn pair_joining(
	sponsor: &mut Library,
	directory: &Path,
	(joining, give): (Joining, bool),
	(shown, typed): (&PairingCode, &PairingCode),
	rng: &mut TestRng,
) -> (
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
) {
	pair_joining_as(
		sponsor,
		directory,
		(joining, give, &nibkeep_node::NewDevice::computer()),
		(shown, typed),
		rng,
	)
}

/// Pairs `joining` as [`pair_joining`] does, as the person chose on the sponsor: a computer
/// or a phone, with the spaces chosen for it.
pub(crate) fn pair_joining_as(
	sponsor: &mut Library,
	directory: &Path,
	(joining, give, new_device): (Joining, bool, &nibkeep_node::NewDevice),
	(shown, typed): (&PairingCode, &PairingCode),
	rng: &mut TestRng,
) -> (
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
) {
	let device = joining.request().device;
	let sponsor_network = sponsor.network(NetworkOptions::LOCAL).expect("starts");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		sponsor_network.endpoint(),
		&sponsor_network.local_addresses(),
		Some(&shown.announcement()),
	);
	// Each pairing draws apart, also of two devices of one name.
	let mut sponsor_rng = TestRng::new(&format!("sponsor of {device} in {}", directory.display()));
	std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			nibkeep_node::pair_device(
				sponsor,
				&sponsor_network,
				shown,
				(new_device, give),
				nibkeep_node::Showing::new(TIMEOUT),
				&|| now() + 60_000,
				&mut sponsor_rng,
			)
		});
		let joined = join(
			directory,
			joining,
			&joiner_network,
			typed,
			&protection(),
			TIMEOUT,
			&|| now() + 60_000,
			rng,
		);
		(paired.join().expect("the sponsor ran"), joined)
	})
}

/// The time of the first change.
pub(crate) fn now() -> u64 {
	millis_at("2026-10-12", 10, 0)
}

/// One end of a connection in memory.
pub(crate) struct Pipe {
	pub(crate) to: mpsc::Sender<Vec<u8>>,
	pub(crate) from: mpsc::Receiver<Vec<u8>>,
	/// A message taken from `from` to tell whether one had arrived, and not yet received.
	pub(crate) early: Option<Vec<u8>>,
	pub(crate) own: [u8; 32],
	pub(crate) peer: [u8; 32],
}

impl Transport for Pipe {
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
		self.to
			.send(message.to_vec())
			.map_err(|_| TransportError("closed".to_owned()))
	}

	fn receive(&mut self) -> Result<Vec<u8>, TransportError> {
		match self.early.take() {
			Some(message) => Ok(message),
			None => self
				.from
				.recv()
				.map_err(|_| TransportError("closed".to_owned())),
		}
	}

	fn has_message(&mut self) -> bool {
		if self.early.is_some() {
			return true;
		}
		match self.from.try_recv() {
			Ok(message) => {
				self.early = Some(message);
				true
			}
			Err(mpsc::TryRecvError::Empty) => false,
			// Receiving fails at once.
			Err(mpsc::TryRecvError::Disconnected) => true,
		}
	}

	fn binding(&self) -> [u8; 32] {
		[9; 32]
	}

	fn own_endpoint(&self) -> [u8; 32] {
		self.own
	}

	fn peer_endpoint(&self) -> [u8; 32] {
		self.peer
	}
}

/// Syncs two libraries in memory, as `nib sync` does over the network, at the time `at`:
/// `first` connects to `second`; then each settles its keys and its checkouts, with random
/// numbers of its own, which `sides` names. Returns what each side's sync gave.
pub(crate) fn sync_in_memory(
	first: &mut Library,
	second: &mut Library,
	at: u64,
	sides: (usize, usize),
) -> (Result<Synced, NodeError>, Result<Synced, NodeError>) {
	sync_in_memory_proving(first, second, at, sides, None)
}

/// The same, with `first` putting to `second` the challenges that a pass of removals has for
/// it, if one is given.
pub(crate) fn sync_in_memory_proving(
	first: &mut Library,
	second: &mut Library,
	at: u64,
	sides: (usize, usize),
	removals: Option<&mut nibkeep_node::Removals>,
) -> (Result<Synced, NodeError>, Result<Synced, NodeError>) {
	let (mut first_end, mut second_end) = pipes(first.endpoint(), second.endpoint());
	let now = move || at;
	let known = (
		first.checkout_contents().expect("reads"),
		second.checkout_contents().expect("reads"),
	);
	let (initiated, responded) = std::thread::scope(|scope| {
		let initiated = scope.spawn(|| {
			let mut syncing = Syncing::new(first, &now);
			if let Some(removals) = removals {
				syncing = syncing.proving(removals);
			}
			let synced = nibkeep_sync::initiate(&mut first_end, &mut syncing);
			drop(syncing);
			drop(first_end);
			synced
		});
		let responded = nibkeep_sync::respond(&mut second_end, &mut Syncing::new(second, &now));
		drop(second_end);
		(initiated.join().expect("the initiator ran"), responded)
	});
	let settled = |library: &mut Library,
	               synced: Result<Synced, SyncError>,
	               (side, known): (usize, &std::collections::BTreeMap<_, _>)| {
		let synced = synced?;
		settle(library, &synced, known, (at, side))?;
		Ok(synced)
	};
	(
		settled(first, initiated, (sides.0, &known.0)),
		settled(second, responded, (sides.1, &known.1)),
	)
}

/// The two ends of a connection in memory between the endpoints `a` and `b`.
pub(crate) fn pipes(a: [u8; 32], b: [u8; 32]) -> (Pipe, Pipe) {
	let (to_b, from_a) = mpsc::channel();
	let (to_a, from_b) = mpsc::channel();
	let a_end = Pipe {
		to: to_b,
		from: from_b,
		early: None,
		own: a,
		peer: b,
	};
	let b_end = Pipe {
		to: to_a,
		from: from_a,
		early: None,
		own: b,
		peer: a,
	};
	(a_end, b_end)
}

/// What a library does after a sync at `at`, as `nib sync` does, with random numbers of
/// its own, which `side` names: settles its keys, the changes that wait, its checkouts and
/// the moves it was behind on. `known` is what its checkouts held before the sync.
pub(crate) fn settle(
	library: &mut Library,
	synced: &Synced,
	known: &std::collections::BTreeMap<nibkeep_base::EntityId, nibkeep_base::Hash>,
	(at, side): (u64, usize),
) -> Result<(), NodeError> {
	let mut rng = TestRng::new(&format!("settling {side} at {at}"));
	library.settle_keys(at, &mut rng)?;
	library.settle_waiting(at)?;
	library.settle_checkouts(known, at, &mut rng)?;
	library.settle_moves(synced, at, at, &mut rng)?;
	Ok(())
}
