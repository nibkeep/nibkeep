//! Pairing and syncing with the user's other replicas over the home network
//! ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md)): a [`Network`] runs this replica's endpoint, and
//! the functions here run one exchange each over it.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::PairingCode;
use nibkeep_keys::NewProtection;
use nibkeep_net::{Connection, NetError, Network, Options, Protocol};
use nibkeep_sync::{Reason, SyncError, Synced};

use crate::library::members::{Acceptor, Invitee, Inviter};
use crate::library::removal::{Removals, Removed};
use crate::library::sync::{AskedFor, Joining, Replica, Sponsor, Syncing};
use crate::library::taking::NewDevice;
use crate::role::Records;
use crate::{Closed, Library, MemberRole, NodeError};

/// An AI agent that pairing adds a replica of: its name, and the spaces besides the personal
/// one chosen for it.
type AiAgent<'a> = (&'a str, &'a [SpaceId]);

/// How long connecting to a replica found on the network may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a sponsor waits for the new replica's first sync, after pairing.
const FIRST_SYNC_TIMEOUT: Duration = Duration::from_mins(2);

/// How long a sponsor waits for the second sync of a new replica that asked for the secrets,
/// which follows the first at once.
const ASKING_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// How long pairing with a closed library waits for another program to close it, once a
/// new replica connects: the code may be spent, so it is worth a wait.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// How many tries a code takes ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#2-pairing)): the one in
/// which it matches spends it, and so does the last of those that fail before it matches,
/// whatever made them fail. Two pairings at once whose codes share their first two digits
/// may each reach the other's device first, and a person may mistype a code.
pub const CODE_TRIES: usize = 3;

/// How long a new replica that tried every device that announces the first two digits of its
/// code, and failed with each, waits for another before it gives up: one that announces them
/// too may not have been heard yet.
const ANOTHER_SPONSOR: Duration = Duration::from_secs(2);

/// What a device that shows a code is told of each try that failed before the code matched,
/// but the last, which ends the pairing or the invitation with its error instead: why it
/// failed, and how many tries the code has left.
pub type WrongTry<'a> = &'a dyn Fn(&NodeError, usize);

/// How a code that this device shows waits for the device that types it: for up to `timeout`
/// from when it is shown, telling `wrong_try` of each try that fails before it matches.
#[derive(Clone, Copy)]
pub struct Showing<'a> {
	/// How long the code works.
	pub timeout: Duration,
	/// What is told of each try that failed before the code matched, but the last.
	pub wrong_try: WrongTry<'a>,
}

impl Showing<'static> {
	/// A code that works for `timeout`, whose wrong tries nobody is told of.
	#[must_use]
	pub fn new(timeout: Duration) -> Self {
		Self {
			timeout,
			wrong_try: &tell_nobody,
		}
	}
}

impl fmt::Debug for Showing<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Showing")
			.field("timeout", &self.timeout)
			.finish_non_exhaustive()
	}
}

/// Tells nobody of a wrong try.
const fn tell_nobody(_: &NodeError, _: usize) {}

/// How pairing with a closed library waits: as `showing` says, until `stop` is set; whether
/// it gives the secrets to a new replica that asks for them, and what the new device is, a
/// computer or a phone, and which spaces it gets, as the person chose.
#[derive(Clone, Copy, Debug)]
pub struct Waiting<'a> {
	/// How long the code works, and what is told of its wrong tries.
	pub showing: Showing<'a>,
	/// Set, it stops waiting.
	pub stop: &'a AtomicBool,
	/// Whether it gives the secrets, which the closed library keeps.
	pub give_secrets: bool,
	/// What the new device is, and which spaces it gets.
	pub device: &'a NewDevice,
}

/// Why a try at a code failed.
enum TryFailed {
	/// Before the code matched: the code takes another try, unless this was its last.
	Unmatched(NodeError),
	/// Once it matched, or for a reason that ends the wait, such as the code's expiry: it is
	/// spent.
	Spent(NodeError),
}

impl From<NodeError> for TryFailed {
	fn from(error: NodeError) -> Self {
		Self::Spent(error)
	}
}

/// Takes the tries at a code that this device shows, as `showing` says: each calls `attempt`
/// with the time left, until one succeeds, the code is spent, or [`CODE_TRIES`] failed before
/// it matched.
fn take_tries<T>(
	showing: Showing<'_>,
	mut attempt: impl FnMut(Duration) -> Result<T, TryFailed>,
) -> Result<T, NodeError> {
	let deadline = Instant::now() + showing.timeout;
	let mut left = CODE_TRIES;
	loop {
		match attempt(deadline.saturating_duration_since(Instant::now())) {
			Ok(done) => return Ok(done),
			Err(TryFailed::Unmatched(error)) if left > 1 => {
				left -= 1;
				(showing.wrong_try)(&error, left);
			}
			Err(TryFailed::Unmatched(error) | TryFailed::Spent(error)) => return Err(error),
		}
	}
}

/// Calls `attempt` with each device that announces `announcement` on the network, one after
/// the other, as the device that typed a code, until one succeeds or its code is spent: two
/// pairings at once whose codes share their first two digits announce the same. It waits up
/// to `timeout` for the first, and [`ANOTHER_SPONSOR`] for each after one failed. A device
/// that answered that it is busy has matched another's code, which it spent, so it is not
/// tried again.
///
/// # Errors
///
/// Returns [`NodeError::NoSponsor`] if none was found, the error of the attempt that spent
/// the code, and otherwise the error of the attempts that failed: that the code did not
/// match, if one said so, as it is the one a person can act on.
fn try_each<T>(
	network: &Network,
	announcement: &str,
	timeout: Duration,
	mut attempt: impl FnMut([u8; 32]) -> Result<T, TryFailed>,
) -> Result<T, NodeError> {
	let deadline = Instant::now() + timeout;
	let mut tried = BTreeSet::new();
	let mut failed: Option<NodeError> = None;
	loop {
		let left = deadline.saturating_duration_since(Instant::now());
		let wait = if failed.is_some() {
			left.min(ANOTHER_SPONSOR)
		} else {
			left
		};
		let Ok(device) = network.find_announcing(announcement, &tried, wait) else {
			return Err(failed.unwrap_or(NodeError::NoSponsor));
		};
		match attempt(device) {
			Ok(done) => return Ok(done),
			Err(TryFailed::Spent(error)) => return Err(error),
			Err(TryFailed::Unmatched(error)) => {
				tried.insert(device);
				failed = Some(match failed {
					Some(wrong) if wrong.is_wrong_code() => wrong,
					_ => error,
				});
			}
		}
	}
}

/// How often pairing with a closed library checks whether to stop waiting.
const STOP_CHECK: Duration = Duration::from_millis(250);

impl Library {
	/// Starts this replica's endpoint, as `options` say.
	///
	/// # Errors
	///
	/// Returns an error if networking cannot start.
	pub fn network(&self, options: Options) -> Result<Network, NodeError> {
		start_network(&self.endpoint_secret(), options)
	}
}

/// Starts the endpoint whose key has this secret.
pub(crate) fn start_network(
	secret: &nibkeep_crypto::Key,
	options: Options,
) -> Result<Network, NodeError> {
	Ok(Network::start(secret.expose_bytes(), options)?)
}

/// A replica that pairing added.
#[derive(Debug)]
pub struct Paired {
	/// The new replica.
	pub replica: EntityId,
	/// The name of its device.
	pub device: String,
	/// The name of its app, or of the AI agent whose replica it is.
	pub app: String,
	/// Whether it is an AI agent's replica.
	pub ai_agent: bool,
	/// What it asked for besides the library: the secrets, and whether it was given them.
	pub asked_for: AskedFor,
	/// Its first sync, which it starts right after pairing.
	pub synced: Synced,
}

/// Pairs a new replica, as the sponsor: announces the code's first two digits on the
/// network, waits as `showing` says for a replica to pair with the code, records it once it
/// says it made its library, and answers its first sync. `network` must take connections.
/// The code takes [`CODE_TRIES`] tries: the one in which it matches spends it, and so does
/// the last of those that fail before it matches; `showing` is told of the others. If the
/// first sync fails, the new replica's records are deleted again, as the new replica
/// removes its library: the pairing failed. Changes are made at the time `now` gives when
/// they are made.
///
/// # Errors
///
/// Returns [`NodeError::PairingExpired`] if no replica paired in time, the error of the
/// last try if each failed before the code matched, such as [`NodeError::Sync`] with
/// [`SyncError::WrongCode`] if one typed another code, [`NodeError::PairingFailed`] if the
/// first sync failed, and other errors if the network or the library fails.
pub fn pair(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	pair_device(
		library,
		network,
		code,
		(&NewDevice::computer(), false),
		showing,
		now,
		rng,
	)
}

/// Pairs a new device of the person's, as [`pair`] does, as they chose: a computer, which
/// gets every space of this device's, or the spaces chosen, and takes those given to it
/// later; or a phone, which gets the personal space alone, or the spaces chosen, and asks
/// before it takes a space given to it later
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
/// With `give_secrets`, it gives the new device the secrets, as [`pair_giving_secrets`]
/// does.
///
/// # Errors
///
/// Returns the errors of [`pair`]; and, before the code is shown,
/// [`NodeError::SecretsLocked`] if the secrets are to be given and are not unlocked, and
/// [`NodeError::NoSpace`] or [`NodeError::NotListed`] for a space chosen that this device
/// does not hold, or is not listed in.
pub fn pair_device(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	(device, give_secrets): (&NewDevice, bool),
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	if give_secrets && !library.secrets_unlocked(library.personal_space()) {
		return Err(NodeError::SecretsLocked);
	}
	library.check_new_device(device)?;
	pair_as(
		library,
		network,
		(code, Pairing::Device(device), give_secrets),
		showing,
		now,
		rng,
	)
}

/// Pairs a new replica of an AI agent named `name`, as [`pair`] pairs one of the user's:
/// the change that lists its key marks it as an AI agent's, for good, and it is named after
/// the AI agent ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#14-ai-agents)).
///
/// # Errors
///
/// Returns the errors of [`pair`], and [`NodeError::AiAgentMayNot`] if this replica is an
/// AI agent's itself.
pub fn pair_ai_agent(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	name: &str,
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	pair_ai_agent_in(library, network, code, (name, &[]), showing, now, rng)
}

/// Pairs a new replica of an AI agent as [`pair_ai_agent`] does, and lists it in `spaces`
/// too, besides the personal one, as the person chose ([technical design: sharing]): its key
/// signs its listing in each as it joins, and it takes each from the personal space, which
/// gives it their first keys. A shared space must allow AI agents, which an admin's replica
/// says, if it does not yet, once the AI agent's replica has joined. The AI agent's replica gets no other space; it is paired anew
/// to change them.
///
/// [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing
///
/// # Errors
///
/// Returns the errors of [`pair_ai_agent`]; and, before the code is shown,
/// [`NodeError::NeverShared`] for a space without members, [`NodeError::NotListed`] for one
/// this replica is not listed in, and [`NodeError::AiAgentsNotAllowed`] for one that allows
/// no AI agents, where this replica's member is no admin.
pub fn pair_ai_agent_in(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	(name, spaces): (&str, &[SpaceId]),
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	pair_as(
		library,
		network,
		(code, Pairing::AiAgent((name, spaces)), false),
		showing,
		now,
		rng,
	)
}

/// Pairs a new replica as [`pair`] does, and gives it the secrets, if it asks for them: the
/// person chose to, and they are unlocked.
///
/// # Errors
///
/// Returns the errors of [`pair`], and [`NodeError::SecretsLocked`] if the secrets are not
/// unlocked, before the code is shown.
pub fn pair_giving_secrets(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	pair_device(
		library,
		network,
		code,
		(&NewDevice::computer(), true),
		showing,
		now,
		rng,
	)
}

/// Whom pairing adds: a device of the person's, as they chose, or an AI agent's replica,
/// with its name and the spaces chosen for it.
#[derive(Clone, Copy)]
enum Pairing<'a> {
	Device(&'a NewDevice),
	AiAgent(AiAgent<'a>),
}

fn pair_as(
	library: &mut Library,
	network: &Network,
	(code, pairing, give_secrets): (&PairingCode, Pairing<'_>, bool),
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	library.check_not_ai_agent("add devices")?;
	if let Pairing::AiAgent((name, spaces)) = pairing {
		if name.trim().is_empty() {
			return Err(NodeError::EmptyName);
		}
		// Before the code is shown: each space chosen can list the AI agent.
		library.check_ai_spaces(spaces)?;
	}
	network.announce(Some(&code.announcement()))?;
	let paired = take_tries(showing, |left| {
		let connection = accept_pairing(network, left, None)?;
		let pairing = (code, pairing, give_secrets);
		sponsor_connected(library, network, connection, pairing, now, rng)
	});
	network.announce(None)?;
	paired
}

/// Pairs a new replica as [`pair`] does, with the library closed while it waits for one, so
/// that other programs, and the app itself, can use the library in between: it is opened
/// once a new replica connects to pair, waiting a little for another program to close it,
/// and closed again after a try that failed before the code matched. It stops waiting, as
/// if the time were up, once `stop` is set, and gives the secrets as [`pair_giving_secrets`]
/// does if `waiting` says so. The caller checks first that this replica may add devices,
/// with [`Library::check_not_ai_agent`], before it shows the code.
///
/// # Errors
///
/// Returns the errors of [`pair`]; [`NodeError::SecretsLocked`] if the secrets are to be
/// given, and `closed` kept none, before the code is shown; and [`NodeError::Store`] with
/// [`StoreError::InUse`] if another program kept the library open after a new replica
/// connected.
///
/// [`StoreError::InUse`]: nibkeep_store::StoreError::InUse
pub fn pair_closed(
	closed: &Closed,
	network: &Network,
	code: &PairingCode,
	waiting: &Waiting<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, NodeError> {
	if waiting.give_secrets && !closed.secrets_kept() {
		return Err(NodeError::SecretsLocked);
	}
	network.announce(Some(&code.announcement()))?;
	let paired = take_tries(waiting.showing, |left| {
		let connection = accept_pairing(network, left, Some(waiting.stop))?;
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		library.check_new_device(waiting.device)?;
		let pairing = (code, Pairing::Device(waiting.device), waiting.give_secrets);
		sponsor_connected(&mut library, network, connection, pairing, now, rng)
	});
	network.announce(None)?;
	paired
}

/// Calls `open`, which opens a library, again while another program has the library open,
/// for up to `wait`: [`NodeError::in_use`] says so.
///
/// # Errors
///
/// Returns the error of the last call.
pub fn retry_in_use<T>(
	wait: Duration,
	mut open: impl FnMut() -> Result<T, NodeError>,
) -> Result<T, NodeError> {
	let deadline = Instant::now() + wait;
	loop {
		match open() {
			Err(error) if error.in_use() && Instant::now() < deadline => {
				std::thread::sleep(Duration::from_millis(50));
			}
			opened => return opened,
		}
	}
}

/// Waits up to `timeout` for a new replica to connect to pair, or until `stop` is set;
/// other connections are refused meanwhile.
fn accept_pairing(
	network: &Network,
	timeout: Duration,
	stop: Option<&AtomicBool>,
) -> Result<Connection, NodeError> {
	accept_first(network, Protocol::Pair, timeout, stop)?.ok_or(NodeError::PairingExpired)
}

/// Waits up to `timeout` for a connection of `protocol`, or until `stop` is set; other
/// connections are refused meanwhile. `None` if none came.
fn accept_first(
	network: &Network,
	protocol: Protocol,
	timeout: Duration,
	stop: Option<&AtomicBool>,
) -> Result<Option<Connection>, NodeError> {
	let deadline = Instant::now() + timeout;
	loop {
		let until = match stop {
			Some(_) => deadline.min(Instant::now() + STOP_CHECK),
			None => deadline,
		};
		let wanted = accept_until(network, until, |connection| {
			connection.protocol() == protocol
		})?;
		if let Some(connection) = wanted {
			return Ok(Some(connection));
		}
		if Instant::now() >= deadline || stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
			return Ok(None);
		}
	}
}

/// Pairs the new replica that connected, and answers its first sync.
fn sponsor_connected(
	library: &mut Library,
	network: &Network,
	mut connection: Connection,
	(code, pairing, give_secrets): (&PairingCode, Pairing<'_>, bool),
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Paired, TryFailed> {
	let joiner = connection.peer();
	let ai_agent = match pairing {
		Pairing::AiAgent(chosen) => Some(chosen),
		Pairing::Device(_) => None,
	};
	let mut sponsor = match pairing {
		Pairing::AiAgent(chosen) => Sponsor::ai_agent_in(library, chosen, now),
		Pairing::Device(device) if give_secrets => Sponsor::new(library, now)
			.giving_secrets()
			.for_device(device.clone()),
		Pairing::Device(device) => Sponsor::new(library, now).for_device(device.clone()),
	};
	let sponsored = nibkeep_sync::sponsor(&mut connection, &mut sponsor, code, rng);
	let (asked_for, matched) = (sponsor.asked_for(), sponsor.matched());
	connection.close();
	let sponsored = sponsored.map_err(|error| {
		if matched {
			TryFailed::Spent(error.into())
		} else {
			TryFailed::Unmatched(error.into())
		}
	})?;
	// The new replica syncs next, from the endpoint it paired from.
	let deadline = Instant::now() + FIRST_SYNC_TIMEOUT;
	let synced = match accept_until(network, deadline, |connection| {
		connection.protocol() == Protocol::Sync && connection.peer() == joiner
	}) {
		Ok(Some(mut connection)) => {
			let synced = nibkeep_sync::respond(&mut connection, &mut Syncing::new(library, now));
			connection.close();
			synced.map_err(NodeError::from)
		}
		Ok(None) => Err(NodeError::Network(NetError::Timeout)),
		Err(error) => Err(error),
	};
	match synced {
		Ok(synced) => {
			// The pairing is done. If settling the keys fails, the next sync tries again.
			let _ = library.settle_keys(now(), rng);
			// A new replica that asks for the secrets says so itself after its first sync, and
			// syncs once more; if it does not, the next sync tells.
			if sponsored.request.secrets.is_some() {
				let deadline = Instant::now() + ASKING_SYNC_TIMEOUT;
				if let Ok(Some(mut connection)) = accept_until(network, deadline, |connection| {
					connection.protocol() == Protocol::Sync && connection.peer() == joiner
				}) {
					let _ = nibkeep_sync::respond(&mut connection, &mut Syncing::new(library, now));
					connection.close();
					let _ = library.settle_keys(now(), rng);
				}
			}
			Ok(Paired {
				replica: sponsored.grant.replica,
				device: sponsored.request.device,
				app: ai_agent.map_or(sponsored.request.app, |(name, _)| name.to_owned()),
				ai_agent: ai_agent.is_some(),
				asked_for,
				synced,
			})
		}
		Err(error) => {
			library.forget_replica(&sponsored.grant, &sponsored.request.signing, now(), rng)?;
			Err(NodeError::PairingFailed(Box::new(error)).into())
		}
	}
}

/// Takes connections until one that `wanted` wants arrives, or `deadline` passes. The
/// others are refused.
fn accept_until(
	network: &Network,
	deadline: Instant,
	wanted: impl Fn(&Connection) -> bool,
) -> Result<Option<Connection>, NodeError> {
	loop {
		let left = deadline.saturating_duration_since(Instant::now());
		if left.is_zero() {
			return Ok(None);
		}
		let Some(mut connection) = network.accept(Some(left))? else {
			return Ok(None);
		};
		if wanted(&connection) {
			return Ok(Some(connection));
		}
		let _ = nibkeep_sync::refuse(&mut connection, Reason::Busy);
		connection.close();
	}
}

/// A library made by pairing with a sponsor, and its first sync.
pub struct Joined {
	/// The new library.
	pub library: Library,
	/// Its first sync.
	pub synced: Synced,
	/// How many blobs and changes its first sync refused, as the disk would have had less than 1 GiB
	/// free.
	pub refused: usize,
}

/// Makes a library in a directory that holds none by pairing a new replica with the
/// replica that shows `code` on the network, and syncs with it: it tries each that announces
/// the code's first two digits in turn, waiting up to `timeout` to find the first.
/// `network` is the new replica's, started with [`Joining::network`]. The keys are protected
/// as `protection` says. If the first sync fails, the library is removed again, as the
/// sponsor deletes the new replica's records: the pairing failed.
///
/// # Errors
///
/// Returns [`NodeError::NoSponsor`] if no replica announces the code in time,
/// [`NodeError::Sync`] with [`SyncError::WrongCode`] if the code matched with none,
/// [`NodeError::PairingFailed`] if the first sync failed, and other errors if the network
/// or the files fail.
#[expect(
	clippy::too_many_arguments,
	reason = "one exchange, with what it needs"
)]
pub fn join(
	directory: &Path,
	joining: Joining,
	network: &Network,
	code: &PairingCode,
	protection: &NewProtection<'_>,
	timeout: Duration,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Joined, NodeError> {
	// Before the code is spent. Copies of replicas made elsewhere stay, and the new one
	// goes beside them.
	match Library::replica_folder(directory) {
		Err(NodeError::NoLibrary | NodeError::Copied) => {}
		_ => return Err(NodeError::Exists),
	}
	joining.check_secrets_protection(protection)?;
	let mut joining = joining;
	joining.seal_secrets(rng)?;
	let asks_for_secrets = joining.asks_for_secrets();
	let request = joining.request();
	let announcement = code.announcement();
	let (sponsor, mut connection, grant) = try_each(network, &announcement, timeout, |sponsor| {
		let mut connection = network
			.connect(sponsor, Protocol::Pair, CONNECT_TIMEOUT)
			.map_err(|error| TryFailed::Unmatched(error.into()))?;
		match nibkeep_sync::join(&mut connection, &request, code, rng) {
			Ok(grant) => Ok((sponsor, connection, grant)),
			Err(error) => {
				// So that what this side said reaches the other.
				connection.close();
				Err(TryFailed::Unmatched(error.into()))
			}
		}
	})?;
	let mut library = match joining.make(directory, &grant, protection, rng) {
		Ok(library) => library,
		Err(error) => {
			let _ = nibkeep_sync::refuse(&mut connection, Reason::Failed);
			connection.close();
			return Err(error);
		}
	};
	// Its key signs its listing in the personal space, and an AI agent's in the spaces
	// chosen for it.
	let listings = library.pairing_listings(&grant);
	let synced = nibkeep_sync::joined(&mut connection, grant.clone(), listings)
		.map_err(NodeError::from)
		.and_then(|_| {
			connection.close();
			let mut connection = network.connect(sponsor, Protocol::Sync, CONNECT_TIMEOUT)?;
			let mut syncing = Syncing::new(&mut library, now).trusting(grant.sponsor);
			let synced = nibkeep_sync::initiate(&mut connection, &mut syncing);
			let refused = syncing.refused();
			// Saves what a sync that failed received.
			drop(syncing);
			connection.close();
			Ok((synced?, refused))
		});
	match synced {
		Ok((synced, refused)) => {
			// The pairing is done. Only its own key says that it asks for the secrets, and
			// with which key: it says so first, takes what was given, and syncs once more,
			// so that the sponsor knows. If it cannot say so, its compartment, which holds
			// nothing, is removed the next time its secrets are unlocked, and it asks anew.
			if asks_for_secrets {
				let _ = library.ask_as_joined(&grant.sponsor, now(), rng);
			}
			// If settling the keys fails, the next sync tries again. It takes the spaces
			// given to it as it paired, then says, as the person chose there, whether it
			// takes those given to it later; if it cannot say so, it takes them.
			let _ = library.settle_keys(now(), rng);
			let _ = library.take_paired_spaces(now(), rng);
			if grant.asks_first {
				let _ = library.set_takes_new_spaces(false, now(), rng);
			}
			if asks_for_secrets
				&& let Ok(mut connection) =
					network.connect(sponsor, Protocol::Sync, CONNECT_TIMEOUT)
			{
				let _ =
					nibkeep_sync::initiate(&mut connection, &mut Syncing::new(&mut library, now));
				connection.close();
				let _ = library.settle_keys(now(), rng);
			}
			Ok(Joined {
				library,
				synced,
				refused,
			})
		}
		Err(error) => {
			library.remove_unsynced();
			Err(NodeError::PairingFailed(Box::new(error)))
		}
	}
}

/// What says whether to add the one who asks to join a space.
type Confirm<'a> = &'a mut dyn FnMut(&Invitee) -> bool;

/// What an invitation is to: a shared space, the role it gives, and the person in the space
/// whom it names the new user, which the user agrees to after joining
/// ([`Library::link_member`]).
#[derive(Clone, Copy, Debug)]
pub struct Invitation {
	/// The space.
	pub space: SpaceId,
	/// The role it gives: viewer, editor or admin.
	pub role: MemberRole,
	/// The person in the space whom it names the new user, if any.
	pub person: Option<EntityId>,
}

/// A member that an invitation added.
#[derive(Debug)]
pub struct Invited {
	/// The member's new record in the space.
	pub member: EntityId,
	/// The user's name, as their library has it.
	pub user_name: String,
	/// The name of the device that joined.
	pub device: String,
	/// Its first sync of the space, which it starts right after the invitation.
	pub synced: Synced,
}

/// A space that this library joined by an invitation, and its first sync.
#[derive(Debug)]
pub struct JoinedSpace {
	/// The space.
	pub space: SpaceId,
	/// Its name, as the admin's device has it.
	pub name: String,
	/// The user's member record there.
	pub member: EntityId,
	/// The role it gave.
	pub role: MemberRole,
	/// The person in the space whom the admin names the user, by ID and name, if any: the
	/// link holds once the user agrees, with [`Library::link_member`].
	pub person: Option<(EntityId, String)>,
	/// The first sync of the space, with the admin's replica.
	pub synced: Synced,
	/// How many blobs and changes its first sync refused, as the disk would have had less
	/// than 1 GiB free.
	pub refused: usize,
}

/// Invites a user to a shared space, as a replica of an admin of it: announces the code's
/// first two digits on the network, waits as `showing` says for a replica to join with the
/// code, shows `confirm` who asks, and, once it says yes, adds them as a member with the
/// role given, lists the replica that joined, and answers its first sync of the space.
/// `network` must take connections. The code takes tries as in [`pair`]. If the first sync
/// fails, the joiner removes the space while the member stays, and a new invitation of the
/// same device makes it that member again; a device listed already comes back so too.
/// Changes are made at the time `now` gives when they are made.
///
/// # Errors
///
/// Returns [`NodeError::NeverShared`] for the personal space, [`NodeError::AdminsOnly`]
/// if this replica's member is no admin, [`NodeError::NoRole`] for the role none,
/// before the code is shown; [`NodeError::InvitationExpired`] if nobody joined in time, the
/// error of the last try if each failed before the code matched, such as
/// [`NodeError::Sync`] with [`SyncError::WrongCode`] if one typed another code,
/// [`NodeError::Declined`] if `confirm` said no, [`NodeError::Sync`] with
/// [`SyncError::Ended`] and [`Reason::HoldsSpace`] if the joiner holds the space already,
/// [`NodeError::InvitationFailed`] if the first sync failed, and other errors if the
/// network or the library fails.
///
/// [`Reason::HoldsSpace`]: nibkeep_sync::Reason::HoldsSpace
#[expect(
	clippy::too_many_arguments,
	reason = "one exchange, with what it needs"
)]
pub fn invite(
	library: &mut Library,
	network: &Network,
	invitation: &Invitation,
	code: &PairingCode,
	confirm: Confirm<'_>,
	showing: Showing<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Invited, NodeError> {
	if invitation.role == MemberRole::None {
		return Err(NodeError::NoRole);
	}
	library.check_admin(invitation.space, "invite")?;
	if let Some(person) = invitation.person {
		library.check_person(invitation.space, person)?;
	}
	network.announce(Some(&code.invitation_announcement()))?;
	let invited = take_tries(showing, |left| {
		let connection = accept_first(network, Protocol::Invite, left, None)?
			.ok_or(NodeError::InvitationExpired)?;
		invite_connected(
			library,
			network,
			connection,
			(invitation, code, &mut *confirm),
			now,
			rng,
		)
	});
	network.announce(None)?;
	invited
}

/// Invites a user to a shared space, as [`invite`] does, as a replica of an admin of it whose
/// library is closed: it checks the role before the code is shown, waits as `showing` says,
/// until `stop` is set, without the library open, so that other programs use it meanwhile,
/// and opens it once a replica connects, closing it again after a try that failed before the
/// code matched.
///
/// # Errors
///
/// Returns the errors of [`invite`], [`NodeError::InvitationExpired`] also once `stop` is
/// set, and [`NodeError::Store`] with [`StoreError::InUse`] if another program kept
/// the library to itself after a replica connected.
///
/// [`StoreError::InUse`]: nibkeep_store::StoreError::InUse
#[expect(
	clippy::too_many_arguments,
	reason = "one exchange, with what it needs"
)]
pub fn invite_closed(
	closed: &Closed,
	network: &Network,
	invitation: &Invitation,
	code: &PairingCode,
	confirm: Confirm<'_>,
	(showing, stop): (Showing<'_>, &AtomicBool),
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Invited, NodeError> {
	if invitation.role == MemberRole::None {
		return Err(NodeError::NoRole);
	}
	{
		let library = retry_in_use(OPEN_WAIT, || closed.open())?;
		library.check_admin(invitation.space, "invite")?;
		if let Some(person) = invitation.person {
			library.check_person(invitation.space, person)?;
		}
	}
	network.announce(Some(&code.invitation_announcement()))?;
	let invited = take_tries(showing, |left| {
		let connection = accept_first(network, Protocol::Invite, left, Some(stop))?
			.ok_or(NodeError::InvitationExpired)?;
		let mut library = retry_in_use(OPEN_WAIT, || closed.open())?;
		invite_connected(
			&mut library,
			network,
			connection,
			(invitation, code, &mut *confirm),
			now,
			rng,
		)
	});
	network.announce(None)?;
	invited
}

/// Invites the joiner that connected, and answers its first sync.
fn invite_connected(
	library: &mut Library,
	network: &Network,
	mut connection: Connection,
	(invitation, code, confirm): (&Invitation, &PairingCode, Confirm<'_>),
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Invited, TryFailed> {
	let joiner = connection.peer();
	let mut admin = Inviter::new(library, invitation, confirm, now);
	let invited = nibkeep_sync::invite(&mut connection, &mut admin, code, rng);
	let matched = admin.matched();
	connection.close();
	let invited = invited.map_err(|error| match error {
		SyncError::Declined => TryFailed::Spent(NodeError::Declined),
		error if matched => TryFailed::Spent(error.into()),
		error => TryFailed::Unmatched(error.into()),
	})?;
	// The joiner syncs the space next, from the endpoint it joined from.
	let started = now();
	let deadline = Instant::now() + FIRST_SYNC_TIMEOUT;
	let synced = match accept_until(network, deadline, |connection| {
		connection.protocol() == Protocol::Sync && connection.peer() == joiner
	}) {
		Ok(Some(mut connection)) => {
			let synced = nibkeep_sync::respond(&mut connection, &mut Syncing::new(library, now));
			connection.close();
			synced.map_err(NodeError::from)
		}
		Ok(None) => Err(NodeError::Network(NetError::Timeout)),
		Err(error) => Err(error),
	};
	match synced {
		Ok(synced) => {
			// The invitation is done. If settling the keys fails, the next sync tries again.
			let _ = library.settle_keys(now(), rng);
			let _ = library.settle_moves(&synced, started, now(), rng);
			Ok(Invited {
				member: invited.offer.member,
				user_name: invited.request.user_name,
				device: invited.request.device,
				synced,
			})
		}
		// The member stays, so that both sides agree whichever saw the sync fail: a joiner
		// that removed the space comes back as that member when invited again.
		Err(error) => Err(NodeError::InvitationFailed(Box::new(error)).into()),
	}
}

/// Joins a shared space by the invitation of the replica that shows `code` on the network,
/// and syncs the space with it: it tries each that announces the code's first two digits in
/// turn, waiting up to `timeout` to find the first. `network` is this replica's. If the
/// first sync fails, the space is removed again, and a new invitation makes this replica the
/// same member. The user's other replicas get the space through the personal space, after
/// their next syncs with this one.
///
/// # Errors
///
/// Returns [`NodeError::NoSponsor`] if no replica announces the code in time,
/// [`NodeError::Sync`] with [`SyncError::WrongCode`] if the code matched with none,
/// [`NodeError::HoldsSpace`] if this library holds the space already, [`NodeError::Sync`]
/// with [`SyncError::Ended`] and [`Reason::Declined`] if the person who invites declined,
/// [`NodeError::InvitationFailed`] if the first sync failed, and other errors if the
/// network or the files fail.
///
/// [`Reason::Declined`]: nibkeep_sync::Reason::Declined
pub fn join_space(
	library: &mut Library,
	network: &Network,
	code: &PairingCode,
	timeout: Duration,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<JoinedSpace, NodeError> {
	library.check_not_ai_agent("join spaces")?;
	library.check_not_revoked(library.personal_space())?;
	let request = library.invitation_request()?;
	let announcement = code.invitation_announcement();
	let (admin, invitation, back) = try_each(network, &announcement, timeout, |admin| {
		let mut connection = network
			.connect(admin, Protocol::Invite, CONNECT_TIMEOUT)
			.map_err(|error| TryFailed::Unmatched(error.into()))?;
		let mut acceptor = Acceptor::new(library);
		let accepted = nibkeep_sync::accept(&mut connection, &request, &mut acceptor, code, rng);
		let (held, taken, back) = (acceptor.held, acceptor.taken, acceptor.back);
		let matched = acceptor.matched;
		// So that what this side said reaches the other.
		connection.close();
		match accepted {
			Ok(invitation) => Ok((admin, invitation, back)),
			Err(SyncError::HoldsSpace) => Err(TryFailed::Spent(held.map_or(
				NodeError::Sync(SyncError::HoldsSpace),
				NodeError::HoldsSpace,
			))),
			// The admin's device took the code, and the person declined.
			Err(error @ SyncError::Ended(Reason::Declined)) => Err(TryFailed::Spent(error.into())),
			Err(error) => {
				if let Some(space) = taken {
					library.remove_space(space, rng);
				}
				// What this side refused, it says itself.
				let error = match error {
					SyncError::Holder(inner) => match inner.downcast::<NodeError>() {
						Ok(refused) => *refused,
						Err(inner) => NodeError::Sync(SyncError::Holder(inner)),
					},
					error => error.into(),
				};
				Err(if matched {
					TryFailed::Spent(error)
				} else {
					TryFailed::Unmatched(error)
				})
			}
		}
	})?;
	let space = invitation.offer.space;
	let started = now();
	let synced = network
		.connect(admin, Protocol::Sync, CONNECT_TIMEOUT)
		.map_err(NodeError::from)
		.and_then(|mut connection| {
			let mut syncing = Syncing::new(library, now).trusting(invitation.grant.admin);
			let synced = nibkeep_sync::initiate(&mut connection, &mut syncing);
			let refused = syncing.refused();
			// Saves what a sync that failed received.
			drop(syncing);
			connection.close();
			Ok((synced?, refused))
		});
	match synced {
		Ok((synced, refused)) => {
			// The invitation is done. The user's devices that take new spaces get it, and
			// the others ask for it; if settling the keys fails, the next sync tries again.
			let _ = library.settle_keys(now(), rng);
			let _ = library.give_new_space(space, &[], now(), rng);
			let _ = library.settle_space_names(now(), rng);
			library.end_bursts();
			let _ = library.settle_moves(&synced, started, now(), rng);
			Ok(JoinedSpace {
				space,
				name: invitation.offer.space_name,
				member: invitation.offer.member,
				role: MemberRole::from_text(&invitation.offer.role).unwrap_or(MemberRole::Viewer),
				person: invitation.offer.person,
				synced,
				refused,
			})
		}
		Err(error) => {
			// A replica that came back keeps the space it held, as it was.
			if back.is_none() {
				library.remove_space(space, rng);
			}
			Err(NodeError::InvitationFailed(Box::new(error)))
		}
	}
}

/// A sync with one of the user's other replicas.
#[derive(Debug)]
pub struct SyncedWith {
	/// The replica.
	pub replica: Replica,
	/// What the sync did, or why it failed; [`NodeError::NotFound`] if the replica was not
	/// found on the network.
	pub synced: Result<Synced, NodeError>,
	/// How many blobs and changes it refused, as the disk would have had less than 1 GiB free.
	pub refused: usize,
	/// The copies of checkouts on this device that it deleted, as it brought purges of
	/// their content.
	pub deleted: Vec<PathBuf>,
}

/// Syncs once with each of the user's other replicas that it finds on the network within
/// `timeout`, one after the other. `network` need not take connections. What is received
/// is checked against the time `now` gives when it arrives.
///
/// # Errors
///
/// Returns [`NodeError::NoOtherReplicas`] if the library has no other replica to sync
/// with, and an error if the library cannot be read or written; a sync that fails is in
/// its result.
pub fn sync_with_others(
	library: &mut Library,
	network: &Network,
	timeout: Duration,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Vec<SyncedWith>, NodeError> {
	library.record_own_endpoint(now(), rng)?;
	let peers = library.peers()?;
	if peers.is_empty() {
		return Err(NodeError::NoOtherReplicas);
	}
	let endpoints = peers.iter().filter_map(|peer| peer.endpoint).collect();
	let found = network.find(&endpoints, timeout);
	Ok(sync_with_found(library, network, peers, &found, now, rng))
}

/// Syncs once with each of `peers` whose endpoint is among those `found` on the network,
/// one after the other, as [`sync_with_others`] does once it found them: so that a program
/// can look for the others with the library closed, with [`Library::peers`] and
/// [`Network::find`]. A peer that was not found is in the result as
/// [`NodeError::NotFound`].
pub fn sync_with_found(
	library: &mut Library,
	network: &Network,
	peers: Vec<Replica>,
	found: &BTreeSet<[u8; 32]>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Vec<SyncedWith> {
	peers
		.into_iter()
		.map(|replica| sync_one(library, network, replica, found, (now, rng), None))
		.collect()
}

/// Syncs once with `replica`, if its endpoint is among those `found`, putting to it the
/// challenges that `removals` has for it, if a pass is given.
fn sync_one(
	library: &mut Library,
	network: &Network,
	replica: Replica,
	found: &BTreeSet<[u8; 32]>,
	(now, rng): (&dyn Fn() -> u64, &mut impl CryptoRng),
	removals: Option<&mut Removals>,
) -> SyncedWith {
	let Some(endpoint) = replica.endpoint.filter(|endpoint| found.contains(endpoint)) else {
		return SyncedWith {
			replica,
			synced: Err(NodeError::NotFound),
			refused: 0,
			deleted: Vec::new(),
		};
	};
	let mut refused = 0;
	let mut deleted = Vec::new();
	let started = now();
	let result = network
		.connect(endpoint, Protocol::Sync, CONNECT_TIMEOUT)
		.map_err(NodeError::from)
		.and_then(|mut connection| {
			// What the checkouts held before, to tell whether purges that arrive leave
			// their copies unchanged.
			let known = library.checkout_contents().unwrap_or_default();
			let mut syncing = Syncing::new(library, now);
			if let Some(removals) = removals {
				syncing = syncing.proving(removals);
			}
			let synced = nibkeep_sync::initiate(&mut connection, &mut syncing);
			refused = syncing.refused();
			// Saves what a sync that failed received.
			drop(syncing);
			connection.close();
			let synced = synced?;
			library.settle_keys(now(), rng)?;
			// What a restore that came brought back, and a wait that the marks changed.
			let _ = library.settle_waiting(now());
			// The sync is done; a later one settles what this one could not.
			deleted = library
				.settle_checkouts(&known, now(), rng)
				.unwrap_or_default();
			// What moves left behind, now that another device holds what moved.
			deleted.extend(
				library
					.settle_moves(&synced, started, now(), rng)
					.unwrap_or_default(),
			);
			Ok(synced)
		});
	SyncedWith {
		replica,
		synced: result,
		refused,
		deleted,
	}
}

/// A pass of removals, and the syncs that it ran for proofs.
#[derive(Debug)]
pub struct RemovalPass {
	/// What it removed, and what waits.
	pub removed: Removed,
	/// The syncs with the replicas asked to prove their copies.
	pub synced: Vec<SyncedWith>,
	/// How many thumbnails made twice it purged, those this replica made of contents that
	/// show another ([`Library::purge_thumbnails_made_twice`]).
	pub thumbnails: usize,
	/// How many parts of what purges destroyed went for good, as their wait ended
	/// ([`Library::destroy_due`]).
	pub destroyed: usize,
}

/// Removes, a little at a time, the copies that this replica no longer keeps, once other
/// disks have proven theirs ([technical design: the planner]): plans a pass
/// ([`Library::plan_removals`]) among the replicas whose endpoints were `found` on the
/// network, syncs with each that is to prove its copies, one after the other, putting the
/// challenges to it, and removes the copies whose proofs came in time
/// ([`Library::remove_proven`]), marked in `records` while they go; then purges the
/// thumbnails that it made twice ([`Library::purge_thumbnails_made_twice`]). First, it
/// destroys what purges destroyed whose wait has ended ([`Library::destroy_due`]).
///
/// [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner
///
/// # Errors
///
/// Returns an error if the library cannot be read, the records cannot be written, or a copy
/// cannot be removed; a sync that fails is in the result, and the copies it was to prove
/// stay.
pub fn remove_unkept(
	library: &mut Library,
	network: &Network,
	records: &Records,
	found: &BTreeSet<[u8; 32]>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<RemovalPass, NodeError> {
	// What purges destroyed goes for good once the wait for it ends, network or not.
	let destroyed = library.destroy_due(now());
	let mut removals = library.plan_removals(records, found, now(), rng)?;
	let mut synced = Vec::new();
	for replica in removals.provers().to_vec() {
		synced.push(sync_one(
			library,
			network,
			replica,
			found,
			(now, rng),
			Some(&mut removals),
		));
	}
	let mut removed = library.remove_proven(&removals, records, now(), rng)?;
	// A purge that fails waits for the next pass, and what the pass did stands.
	let thumbnails = library
		.purge_thumbnails_made_twice(now(), rng)
		.unwrap_or_else(|error| {
			removed.failed.push(error.to_string());
			0
		});
	let destroyed = destroyed.unwrap_or_else(|error| {
		removed.failed.push(error.to_string());
		0
	});
	Ok(RemovalPass {
		removed,
		synced,
		thumbnails,
		destroyed,
	})
}

/// A sync that another replica started.
#[derive(Debug)]
pub struct Answered {
	/// The other replica's endpoint ID.
	pub endpoint: [u8; 32],
	/// The other replica, as the library lists it after the sync, if it does.
	pub replica: Option<Replica>,
	/// What the sync did, or why it failed.
	pub synced: Result<Synced, NodeError>,
	/// How many blobs and changes it refused, as the disk would have had less than 1 GiB free.
	pub refused: usize,
	/// The copies of checkouts on this device that it deleted, as it brought purges of
	/// their content.
	pub deleted: Vec<PathBuf>,
}

/// Waits up to `timeout`, or for as long as it takes, for another replica to sync, and
/// answers it, on a `network` that takes connections: the library is opened for the sync
/// and closed after it, so that other programs can use it in between. A replica that
/// connects while another program has the library open is told to try again later. After a
/// sync, the keys of the spaces are settled, as [`Library::settle_keys`] says, and then the
/// checkouts, as [`Library::settle_checkouts`] says. `None` if none came in time.
///
/// # Errors
///
/// Returns an error if the network fails; a sync that fails is in its result.
pub fn answer(
	closed: &Closed,
	network: &Network,
	timeout: Option<Duration>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Option<Answered>, NodeError> {
	let Some(connection) = network.accept(timeout)? else {
		return Ok(None);
	};
	Ok(Some(answer_connection(Some(closed), connection, now, rng)))
}

/// Answers a connection that another replica opened, as [`answer`] does once it accepted
/// one: so that a program can wait for connections without holding its library, and answer
/// each with the library it holds then. Without a library, as when it is locked, the other
/// replica is told to try again later.
pub fn answer_connection(
	closed: Option<&Closed>,
	mut connection: Connection,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Answered {
	let endpoint = connection.peer();
	let Some(closed) = closed else {
		let _ = nibkeep_sync::refuse(&mut connection, Reason::Busy);
		connection.close();
		return Answered {
			endpoint,
			replica: None,
			synced: Err(NodeError::Locked),
			refused: 0,
			deleted: Vec::new(),
		};
	};
	if connection.protocol() != Protocol::Sync {
		let _ = nibkeep_sync::refuse(&mut connection, Reason::Busy);
		connection.close();
		return Answered {
			endpoint,
			replica: None,
			synced: Err(NodeError::Sync(SyncError::NotAReplica)),
			refused: 0,
			deleted: Vec::new(),
		};
	}
	let mut library = match closed.open() {
		Ok(library) => library,
		Err(error) => {
			let reason = if error.in_use() {
				Reason::Busy
			} else {
				Reason::Failed
			};
			let _ = nibkeep_sync::refuse(&mut connection, reason);
			connection.close();
			return Answered {
				endpoint,
				replica: None,
				synced: Err(error),
				refused: 0,
				deleted: Vec::new(),
			};
		}
	};
	// What the checkouts held before, to tell whether purges that arrive leave their copies
	// unchanged.
	let known = library.checkout_contents().unwrap_or_default();
	let started = now();
	let mut syncing = Syncing::new(&mut library, now);
	let synced = nibkeep_sync::respond(&mut connection, &mut syncing);
	let refused = syncing.refused();
	// Saves what a sync that failed received.
	drop(syncing);
	connection.close();
	let mut deleted = Vec::new();
	let synced = synced.map_err(NodeError::from).and_then(|synced| {
		library.settle_keys(now(), rng)?;
		// What a restore that came brought back, and a wait that the marks changed.
		let _ = library.settle_waiting(now());
		// The sync is done; a later one settles what this one could not.
		deleted = library
			.settle_checkouts(&known, now(), rng)
			.unwrap_or_default();
		// What moves left behind, now that another device holds what moved.
		deleted.extend(
			library
				.settle_moves(&synced, started, now(), rng)
				.unwrap_or_default(),
		);
		Ok(synced)
	});
	// The user's own, or another member's, as a shared space lists it.
	let replica = library
		.replicas()
		.unwrap_or_default()
		.into_iter()
		.chain(library.other_members_replicas())
		.find(|replica| replica.endpoint == Some(endpoint));
	drop(library);
	Answered {
		endpoint,
		replica,
		synced,
		refused,
		deleted,
	}
}
