//! Replicas of a space in memory, and connections between them over channels.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_crypto::{Key, Signature, SigningKey, VerifyingKey, seal_blob};
use nibkeep_log::{ChangeId, Log, Parcel, Received, SpaceKeys, WrappedBlobKey, WrappedKey};
use nibkeep_model::storage::Infallibly;
use nibkeep_sync::{Challenge, Holder, Holding, SyncError, Synced, Transport, TransportError};
use nibkeep_testkit::TestRng;
use nibkeep_time::Clock;

/// How far ahead of a replica's wall clock a received change may be: an hour.
pub(crate) const DRIFT: u64 = 3_600_000;

/// A replica of one space, in memory, with the blobs it holds.
pub(crate) struct Replica {
	pub(crate) key: SigningKey,
	pub(crate) endpoint: [u8; 32],
	pub(crate) space: SpaceId,
	pub(crate) keys: SpaceKeys,
	pub(crate) log: Log,
	pub(crate) blobs: BTreeMap<Hash, Vec<u8>>,
	/// The verifying keys of the replicas it syncs with.
	pub(crate) peers: BTreeSet<[u8; 32]>,
	/// The wall clock, for what it receives.
	pub(crate) wall: u64,
	/// How many items it received since the sync last had it save them, as a replica that
	/// saves what it receives together would hold them unsaved.
	pub(crate) unsaved: Arc<AtomicUsize>,
	/// The sealed blobs it does not keep, and so never asks for.
	pub(crate) declines: BTreeSet<Hash>,
	/// The verifying keys of the replicas it sends no blobs to.
	pub(crate) no_blobs_to: BTreeSet<[u8; 32]>,
	/// How many sealed blobs it asked for in each ask, in order.
	pub(crate) asks: Vec<usize>,
	/// The challenges it puts to each peer, by its verifying key.
	pub(crate) challenges: BTreeMap<[u8; 32], Vec<Challenge>>,
	/// The answers it was given, with their challenges, in order.
	pub(crate) proofs: Vec<(Challenge, Option<[u8; 32]>)>,
	/// The sealed blobs it removes now, which it proves to nobody.
	pub(crate) removing: BTreeSet<Hash>,
	/// The sealed blobs it holds for others, as it proved them.
	pub(crate) held: BTreeSet<Hash>,
	/// The listings of its own key by a recovery code, which it sends as the initiator.
	pub(crate) introductions: Vec<nibkeep_governance::RecoveryListing>,
	/// The recovery keys of the codes that stand in its space, by which it takes a peer's
	/// listing of itself.
	pub(crate) codes: BTreeSet<[u8; 32]>,
	/// The peer that introduced itself in this sync by such a listing, if one did.
	pub(crate) introduced: Option<[u8; 32]>,
}

impl Replica {
	/// Notes an item received, unsaved until the sync pauses or finishes the space.
	fn received(&self) {
		self.unsaved.fetch_add(1, Ordering::Relaxed);
	}
}

impl Replica {
	pub(crate) fn new(rng: &mut TestRng, space: SpaceId, keys: &SpaceKeys) -> Self {
		Self {
			key: rng.signing_key(),
			endpoint: rng.bytes(),
			space,
			keys: keys.clone(),
			log: Log::new(space, Clock::new(DRIFT)),
			blobs: BTreeMap::new(),
			peers: BTreeSet::new(),
			wall: 0,
			unsaved: Arc::new(AtomicUsize::new(0)),
			declines: BTreeSet::new(),
			no_blobs_to: BTreeSet::new(),
			asks: Vec::new(),
			challenges: BTreeMap::new(),
			proofs: Vec::new(),
			removing: BTreeSet::new(),
			held: BTreeSet::new(),
			introductions: Vec::new(),
			codes: BTreeSet::new(),
			introduced: None,
		}
	}

	/// Seals a blob of `len` bytes and keeps it, with its key; returns its sealed hash.
	pub(crate) fn add_blob(&mut self, rng: &mut TestRng, len: usize) -> Hash {
		let content: Vec<u8> = (0..len)
			.map(|index| u8::try_from(index % 251).expect("below 251"))
			.collect();
		let blob_key = Key::generate(rng);
		let mut sealed = Vec::new();
		let made = seal_blob(&blob_key, content.as_slice(), &mut sealed, rng).expect("seals");
		self.log
			.add_blob_key(made.content, made.sealed, blob_key, &self.keys, rng)
			.expect("adds the key");
		self.blobs.insert(made.sealed, sealed);
		made.sealed
	}
}

impl Holder for Replica {
	type Error = Infallible;

	fn verifying_key(&self) -> VerifyingKey {
		self.key.verifying_key()
	}

	fn sign(&self, context: &'static str, message: &[u8]) -> Signature {
		self.key.sign(context, message)
	}

	fn spaces_with(&self, peer: &VerifyingKey) -> Result<Vec<SpaceId>, Infallible> {
		Ok(
			if self.peers.contains(peer.as_bytes()) || self.introduced == Some(*peer.as_bytes()) {
				vec![self.space]
			} else {
				Vec::new()
			},
		)
	}

	fn introductions(
		&self,
		_: &[u8; 32],
	) -> Result<Vec<nibkeep_governance::RecoveryListing>, Infallible> {
		Ok(self.introductions.clone())
	}

	fn introduced(
		&mut self,
		peer: &VerifyingKey,
		listings: &[nibkeep_governance::RecoveryListing],
	) -> Result<(), Infallible> {
		// A listing of the peer's own key, in this space, by a code that stands here, for
		// this sync alone.
		self.introduced = listings
			.iter()
			.any(|listing| {
				listing.key == *peer
					&& listing.space == self.space
					&& listing.verifies()
					&& self.codes.contains(listing.recovery_key.as_bytes())
			})
			.then_some(*peer.as_bytes());
		Ok(())
	}

	fn holdings(&self, space: SpaceId) -> Result<Vec<Holding>, Infallible> {
		assert_eq!(space, self.space);
		let mut holdings: Vec<Holding> = self
			.log
			.summaries()
			.infallible()
			.into_iter()
			.map(|summary| Holding::Change {
				id: summary.id,
				millis: summary.millis,
				depth: summary.depth,
				complete: summary.complete,
			})
			.collect();
		holdings.extend(
			self.log
				.wrapped_keys()
				.map(|(statement, key)| Holding::RecordKey {
					statement,
					key: key.clone(),
				}),
		);
		holdings.extend(
			self.log
				.wrapped_blob_keys()
				.map(|(sealed, key)| Holding::BlobKey {
					sealed: *sealed,
					key: key.clone(),
				}),
		);
		Ok(holdings)
	}

	fn wanted_blobs(
		&mut self,
		space: SpaceId,
		_: &VerifyingKey,
		asked: &BTreeSet<Hash>,
		most: usize,
	) -> Result<Vec<Hash>, Infallible> {
		assert_eq!(space, self.space);
		// Saved before it asks, as a sync asks once what came is saved.
		assert_eq!(self.unsaved.load(Ordering::Relaxed), 0);
		let wanted: Vec<Hash> = self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.filter(|sealed| {
				!self.blobs.contains_key(sealed)
					&& !self.declines.contains(sealed)
					&& !asked.contains(sealed)
			})
			.collect::<BTreeSet<Hash>>()
			.into_iter()
			.take(most)
			.collect();
		self.asks.push(wanted.len());
		Ok(wanted)
	}

	fn sends_blobs(&self, _: SpaceId, peer: &VerifyingKey) -> Result<bool, Infallible> {
		Ok(!self.no_blobs_to.contains(peer.as_bytes()))
	}

	fn challenges(
		&mut self,
		_: SpaceId,
		peer: &VerifyingKey,
	) -> Result<Vec<Challenge>, Infallible> {
		// Saved before it asks, as a sync asks once what came is saved.
		assert_eq!(self.unsaved.load(Ordering::Relaxed), 0);
		Ok(self.challenges.remove(peer.as_bytes()).unwrap_or_default())
	}

	fn proved(
		&mut self,
		_: SpaceId,
		_: &VerifyingKey,
		challenges: &[Challenge],
		answers: &[Option<[u8; 32]>],
	) -> Result<(), Infallible> {
		self.proofs
			.extend(challenges.iter().cloned().zip(answers.iter().copied()));
		Ok(())
	}

	fn prove(
		&mut self,
		_: SpaceId,
		_: &VerifyingKey,
		challenges: &[Challenge],
	) -> Result<Vec<Option<[u8; 32]>>, Infallible> {
		assert_eq!(self.unsaved.load(Ordering::Relaxed), 0);
		Ok(challenges
			.iter()
			.map(|challenge| {
				if self.removing.contains(&challenge.sealed) {
					return None;
				}
				let bytes = self.blobs.get(&challenge.sealed)?;
				// Held before it answers.
				self.held.insert(challenge.sealed);
				challenge.answer_from(bytes)
			})
			.collect())
	}

	fn parcel(&self, _: SpaceId, id: ChangeId) -> Result<Option<Parcel>, Infallible> {
		Ok(self.log.parcel(id).infallible())
	}

	fn blob(
		&self,
		_: SpaceId,
		sealed: &Hash,
	) -> Result<Option<(u64, nibkeep_sync::BlobBytes<'_>)>, Infallible> {
		Ok(self.blobs.get(sealed).map(|bytes| {
			let reader: Box<dyn Read + '_> = Box::new(bytes.as_slice());
			(bytes.len() as u64, reader)
		}))
	}

	fn receive_change(
		&mut self,
		_: SpaceId,
		_: &VerifyingKey,
		parcel: Parcel,
	) -> Result<bool, Infallible> {
		self.received();
		// What the log rejects is dropped, as a replica does.
		Ok(matches!(
			self.log.receive(parcel, &self.keys, self.wall),
			Ok(Received::Accepted(_) | Received::Waiting | Received::Completed)
		))
	}

	fn receive_record_key(
		&mut self,
		_: SpaceId,
		statement: StatementId,
		key: WrappedKey,
	) -> Result<bool, Infallible> {
		self.received();
		let before = self.log.wrapped_keys_of(statement).len();
		let _ = self.log.receive_wrapped_key(statement, key, &self.keys);
		Ok(self.log.wrapped_keys_of(statement).len() > before)
	}

	fn receive_blob_key(
		&mut self,
		_: SpaceId,
		sealed: Hash,
		key: WrappedBlobKey,
	) -> Result<bool, Infallible> {
		self.received();
		let before = self.log.wrapped_blob_keys_of(&sealed).to_vec();
		let _ = self.log.receive_blob_key(sealed, key, &self.keys);
		Ok(self.log.wrapped_blob_keys_of(&sealed) != before.as_slice())
	}

	fn receive_blob(
		&mut self,
		_: SpaceId,
		sealed: Hash,
		size: u64,
		bytes: &mut dyn Read,
	) -> Result<bool, Infallible> {
		// A blob takes long to come, and is saved apart, with what came before.
		self.unsaved.store(0, Ordering::Relaxed);
		// A blob without its key is not wanted.
		if self.log.wrapped_blob_keys_of(&sealed).is_empty() {
			return Ok(false);
		}
		let mut received = Vec::new();
		if bytes.read_to_end(&mut received).is_ok()
			&& received.len() as u64 == size
			&& Hash::of(&received) == sealed
		{
			self.blobs.insert(sealed, received);
			return Ok(true);
		}
		Ok(false)
	}

	fn finish_space(&mut self, _: SpaceId) -> Result<(), Infallible> {
		self.unsaved.store(0, Ordering::Relaxed);
		Ok(())
	}

	fn pause(&mut self) -> Result<(), Infallible> {
		self.unsaved.store(0, Ordering::Relaxed);
		Ok(())
	}
}

/// One end of a connection over channels, which can be cut after a number of messages.
pub(crate) struct Pipe {
	to: Option<mpsc::Sender<Vec<u8>>>,
	from: mpsc::Receiver<Vec<u8>>,
	own: [u8; 32],
	peer: [u8; 32],
	binding: [u8; 32],
	/// How many more messages this end may send, if it is to be cut.
	budget: Option<usize>,
	/// A message taken from `from` to tell whether one had arrived, and not yet received.
	early: Option<Vec<u8>>,
	/// What its side's holder holds unsaved, and how many times this end waited for a
	/// message, or sent one, meanwhile: never, in a sync that succeeds.
	watched: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
	/// How many messages it received.
	received: usize,
}

impl Pipe {
	/// Whether the next message comes late, after its side looked whether it had come and
	/// before it waits for it, as over a network: every third, on a watched end.
	const fn late(&self) -> bool {
		self.watched.is_some() && self.received % 3 == 2
	}

	/// Notes a wait or a send while its side's holder holds what it received unsaved.
	fn note_unsaved(&self) {
		if let Some((unsaved, noted)) = &self.watched
			&& unsaved.load(Ordering::Relaxed) > 0
		{
			noted.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Transport for Pipe {
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
		self.note_unsaved();
		if self.budget == Some(0) {
			// The other end sees the connection close.
			self.to = None;
		}
		if let Some(budget) = &mut self.budget {
			*budget = budget.saturating_sub(1);
		}
		let to = self
			.to
			.as_ref()
			.ok_or_else(|| TransportError("cut".to_owned()))?;
		to.send(message.to_vec())
			.map_err(|_| TransportError("closed".to_owned()))
	}

	fn receive(&mut self) -> Result<Vec<u8>, TransportError> {
		if self.late() {
			self.note_unsaved();
		}
		self.received += 1;
		if let Some(message) = self.early.take() {
			return Ok(message);
		}
		match self.from.try_recv() {
			Ok(message) => return Ok(message),
			Err(mpsc::TryRecvError::Empty) => self.note_unsaved(),
			Err(mpsc::TryRecvError::Disconnected) => {}
		}
		self.from
			.recv()
			.map_err(|_| TransportError("closed".to_owned()))
	}

	fn has_message(&mut self) -> bool {
		if self.late() {
			return false;
		}
		if self.early.is_some() {
			return true;
		}
		match self.from.try_recv() {
			Ok(message) => {
				self.early = Some(message);
				true
			}
			Err(mpsc::TryRecvError::Empty) => false,
			Err(mpsc::TryRecvError::Disconnected) => true,
		}
	}

	fn binding(&self) -> [u8; 32] {
		self.binding
	}

	fn own_endpoint(&self) -> [u8; 32] {
		self.own
	}

	fn peer_endpoint(&self) -> [u8; 32] {
		self.peer
	}
}

/// A connection between two endpoints: the end of `a`, which may be cut after `budget`
/// messages, and the end of `b`.
pub(crate) fn pipes(
	a: [u8; 32],
	b: [u8; 32],
	binding: [u8; 32],
	budget: Option<usize>,
) -> (Pipe, Pipe) {
	let (a_end, mut b_end) = pipes_bound(a, b, binding, binding);
	b_end.budget = None;
	(Pipe { budget, ..a_end }, b_end)
}

/// A connection whose two ends see different bindings, as two connections that an attacker
/// joins would.
pub(crate) fn pipes_bound(
	a: [u8; 32],
	b: [u8; 32],
	binding_a: [u8; 32],
	binding_b: [u8; 32],
) -> (Pipe, Pipe) {
	let (to_b, from_a) = mpsc::channel();
	let (to_a, from_b) = mpsc::channel();
	(
		Pipe {
			to: Some(to_b),
			from: from_b,
			own: a,
			peer: b,
			binding: binding_a,
			budget: None,
			early: None,
			watched: None,
			received: 0,
		},
		Pipe {
			to: Some(to_a),
			from: from_a,
			own: b,
			peer: a,
			binding: binding_b,
			budget: None,
			early: None,
			watched: None,
			received: 0,
		},
	)
}

/// Syncs two replicas: `a` connects to `b`, and its end is cut after `budget` messages if
/// one is given. Returns what each side's sync gave.
pub(crate) fn sync(
	a: &mut Replica,
	b: &mut Replica,
	binding: [u8; 32],
	budget: Option<usize>,
) -> (Result<Synced, SyncError>, Result<Synced, SyncError>) {
	let (a_end, b_end) = pipes(a.endpoint, b.endpoint, binding, budget);
	sync_over(a, b, a_end, b_end)
}

/// Syncs two replicas over the two ends of a connection: `a` connects to `b`. In a sync that
/// succeeds, neither side waits for the other, or sends to it, while it holds what it
/// received unsaved.
pub(crate) fn sync_over(
	a: &mut Replica,
	b: &mut Replica,
	mut a_end: Pipe,
	mut b_end: Pipe,
) -> (Result<Synced, SyncError>, Result<Synced, SyncError>) {
	// What a sync that failed received was saved as it ended, as a replica's is.
	a.unsaved.store(0, Ordering::Relaxed);
	b.unsaved.store(0, Ordering::Relaxed);
	let noted = Arc::new(AtomicUsize::new(0));
	a_end.watched = Some((a.unsaved.clone(), noted.clone()));
	b_end.watched = Some((b.unsaved.clone(), noted.clone()));
	let synced = std::thread::scope(|scope| {
		let initiator = scope.spawn(|| {
			let synced = nibkeep_sync::initiate(&mut a_end, a);
			drop(a_end);
			synced
		});
		let responded = nibkeep_sync::respond(&mut b_end, b);
		drop(b_end);
		(initiator.join().expect("the initiator ran"), responded)
	});
	if synced.0.is_ok() && synced.1.is_ok() {
		assert_eq!(
			noted.load(Ordering::Relaxed),
			0,
			"a side waited or sent while it held what it received unsaved"
		);
	}
	synced
}
