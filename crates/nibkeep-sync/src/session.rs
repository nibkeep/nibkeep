//! A sync between two replicas, over a transport: who they are, which spaces they share,
//! and for each space, what one holds and the other lacks ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#3-syncing)).

use std::collections::BTreeSet;
use std::io::{self, Read};

use nibkeep_base::cbor::{self, Value};
use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_crypto::{Signature, VerifyingKey};
use nibkeep_governance::RecoveryListing;
use nibkeep_log::{ChangeId, Parcel, WrappedBlobKey, WrappedKey};

use crate::message::{Item, SyncMessage};
use crate::reconcile::Reconciling;
use crate::{CHALLENGES_AT_ONCE, Challenge, Holding, SyncError, Transport};

/// The context of the signature that proves who a side is.
const HELLO: &str = "nibkeep sync hello";

/// How many items one message asks for: their IDs take 2 MiB.
const WANTED_AT_ONCE: usize = 65_536;

/// How many bytes of a blob one message carries.
const BLOB_CHUNK: usize = 1024 * 1024;

/// The bytes of a sealed blob, as a replica reads them.
pub type BlobBytes<'a> = Box<dyn Read + 'a>;

/// A replica's data, as a sync reads and changes it. Each method works on one space.
pub trait Holder {
	/// Why reading or writing the data failed.
	type Error: std::error::Error + Send + Sync + 'static;

	/// The replica's verifying key.
	fn verifying_key(&self) -> VerifyingKey;

	/// Signs a message with the replica's signing key.
	fn sign(&self, context: &'static str, message: &[u8]) -> Signature;

	/// The spaces the replica holds in which `peer` is a replica that it syncs with, sorted
	/// by ID: also those in which [`Holder::introduced`] took a listing of `peer`'s.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn spaces_with(&self, peer: &VerifyingKey) -> Result<Vec<SpaceId>, Self::Error>;

	/// The listings of the replica's own key by a recovery code, which it sends in its hello
	/// as the initiator, so that a peer that does not know its key yet syncs with it: for the
	/// replica whose endpoint ID is `peer`, which it connected to, one for each space or group
	/// that lists it so and that it shares with that replica, which holds those IDs already;
	/// at most [`crate::MOST_INTRODUCTIONS`]. None by default.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn introductions(&self, peer: &[u8; 32]) -> Result<Vec<RecoveryListing>, Self::Error> {
		let _ = peer;
		Ok(Vec::new())
	}

	/// Takes the listings by a recovery code that `peer` sent in its hello, none if it sent
	/// none, before [`Holder::spaces_with`], for this sync alone: a space or group for which
	/// the replica holds a code that counts, which signed the listing of `peer`'s key there,
	/// it syncs with `peer` as with its replicas, which brings the change that lists `peer`.
	/// Takes none by default.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn introduced(
		&mut self,
		peer: &VerifyingKey,
		listings: &[RecoveryListing],
	) -> Result<(), Self::Error> {
		let _ = (peer, listings);
		Ok(())
	}

	/// Readies a space for its sync, before [`Holder::holdings`]: a replica brings its own
	/// account of what it holds up to date here. One that keeps no account has nothing to do.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read or written.
	fn prepare(&mut self, space: SpaceId) -> Result<(), Self::Error> {
		let _ = space;
		Ok(())
	}

	/// Everything a space holds, for reconciling.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn holdings(&self, space: SpaceId) -> Result<Vec<Holding>, Self::Error>;

	/// Whether to send `peer` an item of a space that it lacks: every item, but to a peer that
	/// the space no longer gives what is added, as a member's replica that was removed, only
	/// what it may still get. What is not sent is still reconciled, so that neither side asks
	/// for what the other holds already.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn sends(
		&self,
		space: SpaceId,
		peer: &VerifyingKey,
		holding: &Holding,
	) -> Result<bool, Self::Error> {
		let _ = (space, peer, holding);
		Ok(true)
	}

	/// The sealed blobs of a space that the replica asks `peer` for, at most `most` of them:
	/// those it keeps and lacks, of which `peer`'s account lists a copy, or all of them if it
	/// keeps no account of `peer` in the space; none of `asked`, which it asked `peer` for
	/// before in this sync of the space. Asked once what came of the space is saved.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn wanted_blobs(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		asked: &BTreeSet<Hash>,
		most: usize,
	) -> Result<Vec<Hash>, Self::Error>;

	/// Whether to send `peer` the sealed blobs of a space that it asks for: every one, but
	/// none to a peer that the space no longer gives what is added.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn sends_blobs(&self, space: SpaceId, peer: &VerifyingKey) -> Result<bool, Self::Error> {
		let _ = (space, peer);
		Ok(true)
	}

	/// The challenges to put to `peer` of a space's sealed blobs, which it is to prove that it
	/// holds now: of the copies that this replica is about to remove, for which a copy on the
	/// peer's disk is to stand, at most [`CHALLENGES_AT_ONCE`], as a sync puts no more to it
	/// in a space. None by default. Asked as the initiator, once the blobs asked for came.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn challenges(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
	) -> Result<Vec<Challenge>, Self::Error> {
		let _ = (space, peer);
		Ok(Vec::new())
	}

	/// Takes `peer`'s answers to the challenges put to it, in their order: `None` for a
	/// blob that it did not prove.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read or written.
	fn proved(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
		answers: &[Option<[u8; 32]>],
	) -> Result<(), Self::Error> {
		let _ = (space, peer, challenges, answers);
		Ok(())
	}

	/// Answers the challenges that `peer` puts to this replica, in their order: for each
	/// sealed blob of the space that it holds and is not removing, once it has recorded that
	/// it holds the blob for a while, the answer that [`Challenge::answer`] gives from its
	/// bytes; `None` for the others. Answers none by default.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read or written.
	fn prove(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
	) -> Result<Vec<Option<[u8; 32]>>, Self::Error> {
		let _ = (space, peer);
		Ok(vec![None; challenges.len()])
	}

	/// An accepted change with its sealed payload and wrapped keys, as far as the replica
	/// has them.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn parcel(&self, space: SpaceId, id: ChangeId) -> Result<Option<Parcel>, Self::Error>;

	/// A sealed blob that the replica holds, with its size in bytes.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn blob(
		&self,
		space: SpaceId,
		sealed: &Hash,
	) -> Result<Option<(u64, BlobBytes<'_>)>, Self::Error>;

	/// Takes a change from the other replica, `peer`, and returns whether it kept anything of
	/// it. One that the log rejects, or has already, or that the replica takes nothing like
	/// from that peer, is dropped, and is no error.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn receive_change(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		parcel: Parcel,
	) -> Result<bool, Self::Error>;

	/// Takes a wrapped record key from the other replica, and returns whether it kept it.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn receive_record_key(
		&mut self,
		space: SpaceId,
		statement: StatementId,
		key: WrappedKey,
	) -> Result<bool, Self::Error>;

	/// Takes a wrapped blob key from the other replica, and returns whether it kept it.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn receive_blob_key(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		key: WrappedBlobKey,
	) -> Result<bool, Self::Error>;

	/// Takes a sealed blob of `size` bytes from the other replica, and returns whether it
	/// kept it: only if its bytes have the sealed hash, and the replica wants it, such as
	/// one whose key it holds. It need not read all of `bytes`, and should read none of a
	/// blob it does not want.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written, or reading `bytes` fails.
	fn receive_blob(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		size: u64,
		bytes: &mut dyn Read,
	) -> Result<bool, Self::Error>;

	/// The encoding of a page of an account that the replica keeps, by the hash of its
	/// encoding; `None` if it no longer keeps it.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be read.
	fn account(&self, space: SpaceId, id: &Hash) -> Result<Option<Vec<u8>>, Self::Error> {
		let _ = (space, id);
		Ok(None)
	}

	/// Takes a page of an account from the other replica, and returns whether it kept it:
	/// only a page that its author signed, of a replica of the space, and later than the
	/// one of that page that it keeps.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn receive_account(&mut self, space: SpaceId, page: Vec<u8>) -> Result<bool, Self::Error> {
		let _ = (space, page);
		Ok(false)
	}

	/// Saves what was received of a space, after its last item.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn finish_space(&mut self, space: SpaceId) -> Result<(), Self::Error>;

	/// Saves what it holds unsaved of what it received, as the sync is about to wait for the
	/// other replica, or to send it items: a replica that saves received items together
	/// does not keep them waiting on the network. One that saves each item as it comes has
	/// nothing to do.
	///
	/// # Errors
	///
	/// Returns an error if the data cannot be written.
	fn pause(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}

/// How many things of a space went one way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
	/// Changes, with their payloads and keys.
	pub changes: usize,
	/// Wrapped record keys and wrapped blob keys.
	pub keys: usize,
	/// Sealed blobs.
	pub blobs: usize,
	/// The bytes of the sealed blobs.
	pub blob_bytes: u64,
}

impl Counts {
	/// Whether nothing went.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		*self == Self::default()
	}
}

/// What a space's sync sent and received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceSynced {
	/// The space.
	pub space: SpaceId,
	/// What this side sent.
	pub sent: Counts,
	/// What this side received.
	pub received: Counts,
}

/// What a sync did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Synced {
	/// The other replica's verifying key.
	pub peer: VerifyingKey,
	/// The spaces synced, in order.
	pub spaces: Vec<SpaceSynced>,
}

/// Syncs with the replica at the other end of `transport`, as the side that connected.
///
/// # Errors
///
/// Returns an error if the connection fails, the other side breaks the protocol or ends
/// the connection, is no replica that this one syncs with, or the data cannot be read or
/// written. What was received of a space before is kept.
pub fn initiate<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
) -> Result<Synced, SyncError> {
	let result = initiate_with(transport, holder);
	if let Err(error) = &result {
		end(transport, error);
	}
	result
}

/// Syncs with the replica at the other end of `transport`, as the side that was connected
/// to.
///
/// # Errors
///
/// As [`initiate`].
pub fn respond<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
) -> Result<Synced, SyncError> {
	let result = respond_with(transport, holder);
	if let Err(error) = &result {
		end(transport, error);
	}
	result
}

/// Tells the other side why this side ends the connection, unless the other side ended it
/// or the connection failed.
fn end(transport: &mut impl Transport, error: &SyncError) {
	if let Some(reason) = error.reason() {
		let _ = SyncMessage::End(reason).send(transport);
	}
}

/// What a side signs to prove who it is: `[binding, own endpoint, peer's endpoint]`.
fn hello_message(binding: &[u8; 32], own: &[u8; 32], peer: &[u8; 32]) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![
		Value::Bytes(binding.to_vec()),
		Value::Bytes(own.to_vec()),
		Value::Bytes(peer.to_vec()),
	]))
}

/// Sends this side's hello, with `listings`, the initiator's listings by a recovery code.
fn send_hello(
	transport: &mut impl Transport,
	holder: &impl Holder,
	listings: Vec<RecoveryListing>,
) -> Result<(), SyncError> {
	let message = hello_message(
		&transport.binding(),
		&transport.own_endpoint(),
		&transport.peer_endpoint(),
	);
	SyncMessage::Hello(
		holder.verifying_key(),
		holder.sign(HELLO, &message),
		listings,
	)
	.send(transport)
}

/// Waits for the other side's hello, and returns its key if its signature checks, with the
/// listings by a recovery code that it carries.
fn receive_hello(
	transport: &mut impl Transport,
) -> Result<(VerifyingKey, Vec<RecoveryListing>), SyncError> {
	let SyncMessage::Hello(key, signature, listings) = SyncMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a hello"));
	};
	let message = hello_message(
		&transport.binding(),
		&transport.peer_endpoint(),
		&transport.own_endpoint(),
	);
	key.verify(HELLO, &message, &signature)
		.map_err(|_| SyncError::protocol("a hello whose signature does not check"))?;
	Ok((key, listings))
}

fn initiate_with<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
) -> Result<Synced, SyncError> {
	let mut introductions = holder
		.introductions(&transport.peer_endpoint())
		.map_err(SyncError::holder)?;
	introductions.truncate(crate::MOST_INTRODUCTIONS);
	send_hello(transport, holder, introductions)?;
	let (peer, listings) = receive_hello(transport)?;
	// The responder knows the initiator by then, as it would not answer otherwise.
	if !listings.is_empty() {
		return Err(SyncError::protocol("a responder's hello with listings"));
	}
	let offered = holder.spaces_with(&peer).map_err(SyncError::holder)?;
	if offered.is_empty() {
		return Err(SyncError::NotAReplica);
	}
	SyncMessage::Spaces(offered.clone()).send(transport)?;
	let SyncMessage::Spaces(agreed) = SyncMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected the spaces"));
	};
	let offered: BTreeSet<SpaceId> = offered.into_iter().collect();
	if !agreed.iter().all(|space| offered.contains(space))
		|| !agreed.windows(2).all(|pair| pair[0] < pair[1])
	{
		return Err(SyncError::protocol("spaces that were not offered"));
	}
	let mut spaces = Vec::new();
	for space in agreed {
		spaces.push(sync_space(transport, holder, (space, &peer))?);
	}
	SyncMessage::Done.send(transport)?;
	match SyncMessage::receive(transport)? {
		SyncMessage::Done => Ok(Synced { peer, spaces }),
		_ => Err(SyncError::protocol("expected the end")),
	}
}

/// Syncs one space as the initiator.
fn sync_space<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(space, peer): (SpaceId, &VerifyingKey),
) -> Result<SpaceSynced, SyncError> {
	holder.prepare(space).map_err(SyncError::holder)?;
	let mut reconciling = Reconciling::new(holder.holdings(space).map_err(SyncError::holder)?)?;
	SyncMessage::Reconcile(space, reconciling.initiate()?).send(transport)?;
	let (mut have, mut need) = (Vec::new(), Vec::new());
	loop {
		let SyncMessage::Reconcile(answered, message) = SyncMessage::receive(transport)? else {
			return Err(SyncError::protocol("expected a message of negentropy"));
		};
		if answered != space {
			return Err(SyncError::protocol(
				"a message of negentropy of another space",
			));
		}
		match reconciling.answer(&message, &mut have, &mut need)? {
			Some(next) => SyncMessage::Reconcile(space, next).send(transport)?,
			None => break,
		}
	}
	let mut synced = SpaceSynced {
		space,
		sent: Counts::default(),
		received: Counts::default(),
	};
	for holding in reconciling.in_order(&have) {
		send_item(transport, holder, (space, peer), &holding, &mut synced.sent)?;
	}
	let none = BTreeSet::new();
	for wanted in need.chunks(WANTED_AT_ONCE) {
		// What came of the chunk before is saved before the other side sends more.
		holder.pause().map_err(SyncError::holder)?;
		SyncMessage::Wanted(space, wanted.to_vec()).send(transport)?;
		receive_until_sent(
			transport,
			holder,
			(space, peer),
			&none,
			&mut synced.received,
		)?;
	}
	// The initiator asks for the blobs it keeps, once what came is saved, so that it knows
	// what the space's files are now; then the responder asks.
	let mut asked = BTreeSet::new();
	loop {
		holder.pause().map_err(SyncError::holder)?;
		let wanted = holder
			.wanted_blobs(space, peer, &asked, WANTED_AT_ONCE)
			.map_err(SyncError::holder)?;
		if wanted.is_empty() {
			break;
		}
		let more = wanted.len() >= WANTED_AT_ONCE;
		asked.extend(wanted.iter().copied());
		SyncMessage::Asked(space, wanted).send(transport)?;
		receive_until_sent(
			transport,
			holder,
			(space, peer),
			&asked,
			&mut synced.received,
		)?;
		if !more {
			break;
		}
	}
	loop {
		SyncMessage::YourAsks(space).send(transport)?;
		let SyncMessage::Asked(of, wanted) = next_message(transport, holder)? else {
			return Err(SyncError::protocol("expected the blobs asked for"));
		};
		if of != space || wanted.len() > WANTED_AT_ONCE {
			return Err(SyncError::protocol("blobs asked for out of turn"));
		}
		send_blobs(transport, holder, (space, peer), &wanted, &mut synced.sent)?;
		SyncMessage::Sent(space).send(transport)?;
		if wanted.len() < WANTED_AT_ONCE {
			break;
		}
	}
	// Last, the responder proves the blobs that this side asks it to, as this side is about
	// to remove its own copies.
	let mut challenges = holder.challenges(space, peer).map_err(SyncError::holder)?;
	// One message of them in a space's sync, as the responder answers once.
	challenges.truncate(CHALLENGES_AT_ONCE);
	if !challenges.is_empty() {
		SyncMessage::Prove(space, challenges.clone()).send(transport)?;
		let SyncMessage::Proofs(of, answers) = next_message(transport, holder)? else {
			return Err(SyncError::protocol("expected the proofs"));
		};
		if of != space || answers.len() != challenges.len() {
			return Err(SyncError::protocol("proofs out of turn"));
		}
		holder
			.proved(space, peer, &challenges, &answers)
			.map_err(SyncError::holder)?;
	}
	holder.finish_space(space).map_err(SyncError::holder)?;
	Ok(synced)
}

/// Takes the items that the other side sends of a space until it says it sent what was
/// asked for. A blob is kept only if it is among `asked`.
fn receive_until_sent<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(space, peer): (SpaceId, &VerifyingKey),
	asked: &BTreeSet<Hash>,
	received: &mut Counts,
) -> Result<(), SyncError> {
	loop {
		match next_message(transport, holder)? {
			SyncMessage::Item(of, item) if of == space => {
				receive_item(transport, holder, (space, peer), item, asked, received)?;
			}
			SyncMessage::Sent(of) if of == space => return Ok(()),
			_ => return Err(SyncError::protocol("expected an item or the end of them")),
		}
	}
}

/// Sends the sealed blobs of a space that the other side asked for, those that this side
/// holds, unless it sends that peer no blobs.
fn send_blobs<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &H,
	(space, peer): (SpaceId, &VerifyingKey),
	wanted: &[Hash],
	sent: &mut Counts,
) -> Result<(), SyncError> {
	if !holder.sends_blobs(space, peer).map_err(SyncError::holder)? {
		return Ok(());
	}
	let mut seen = BTreeSet::new();
	for sealed in wanted {
		if seen.insert(*sealed) {
			send_blob(transport, holder, space, sealed, sent)?;
		}
	}
	Ok(())
}

/// Sends a sealed blob that this side holds, with its bytes; one that it no longer holds is
/// left out.
fn send_blob<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &H,
	space: SpaceId,
	sealed: &Hash,
	sent: &mut Counts,
) -> Result<(), SyncError> {
	let Some((size, mut reader)) = holder.blob(space, sealed).map_err(SyncError::holder)? else {
		return Ok(());
	};
	SyncMessage::Item(space, Item::Blob(*sealed, size)).send(transport)?;
	let mut left = size;
	let mut chunk = vec![0; BLOB_CHUNK];
	while left > 0 {
		let want = usize::try_from(left).map_or(BLOB_CHUNK, |left| left.min(BLOB_CHUNK));
		reader
			.read_exact(&mut chunk[..want])
			.map_err(SyncError::holder)?;
		transport.send(&crate::message::bytes_message(&chunk[..want]))?;
		left -= want as u64;
	}
	sent.blobs += 1;
	sent.blob_bytes += size;
	Ok(())
}

/// The space a responder is syncing.
struct Current {
	reconciling: Reconciling,
	synced: SpaceSynced,
	/// The sealed blobs it asked for in this sync of the space.
	asked: BTreeSet<Hash>,
	/// Whether it asked for blobs that the initiator has yet to say it sent.
	awaiting: bool,
	/// Whether it asked for all it lacks: fewer blobs than one message holds.
	asks_done: bool,
	/// Whether the initiator asked it for proofs, which it does once in a space's sync.
	proved: bool,
}

fn respond_with<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
) -> Result<Synced, SyncError> {
	let (peer, listings) = receive_hello(transport)?;
	holder
		.introduced(&peer, &listings)
		.map_err(SyncError::holder)?;
	let shared: BTreeSet<SpaceId> = holder
		.spaces_with(&peer)
		.map_err(SyncError::holder)?
		.into_iter()
		.collect();
	// A peer that is no replica learns nothing, not even who this replica is.
	if shared.is_empty() {
		return Err(SyncError::NotAReplica);
	}
	send_hello(transport, holder, Vec::new())?;
	let SyncMessage::Spaces(offered) = SyncMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected the spaces"));
	};
	let agreed: BTreeSet<SpaceId> = offered
		.into_iter()
		.filter(|space| shared.contains(space))
		.collect();
	SyncMessage::Spaces(agreed.iter().copied().collect()).send(transport)?;
	let mut synced = Synced {
		peer,
		spaces: Vec::new(),
	};
	let mut current: Option<Current> = None;
	let mut started: BTreeSet<SpaceId> = BTreeSet::new();
	loop {
		match next_message(transport, holder)? {
			SyncMessage::Reconcile(space, message) => {
				if current.as_ref().is_none_or(|now| now.synced.space != space) {
					if !agreed.contains(&space) || !started.insert(space) {
						return Err(SyncError::protocol("a space out of turn"));
					}
					finish(holder, current.take(), &mut synced)?;
					current = Some(begin_space(holder, space)?);
				}
				let now = current
					.as_mut()
					.unwrap_or_else(|| unreachable!("set above"));
				let answer = now.reconciling.respond(&message)?;
				SyncMessage::Reconcile(space, answer).send(transport)?;
			}
			SyncMessage::Item(space, item) => {
				let now = of_space(&mut current, space, "an item of a space out of turn")?;
				receive_item(
					transport,
					holder,
					(space, &peer),
					item,
					&now.asked,
					&mut now.synced.received,
				)?;
			}
			SyncMessage::Wanted(space, ids) => {
				let now = of_space(&mut current, space, "wanted items of a space out of turn")?;
				send_wanted(transport, holder, (now, &peer), &ids)?;
			}
			SyncMessage::Asked(space, wanted) => {
				let now = of_space(
					&mut current,
					space,
					"blobs asked for of a space out of turn",
				)?;
				answer_asked(transport, holder, (now, &peer), &wanted)?;
			}
			SyncMessage::YourAsks(space) => {
				let now = of_space(&mut current, space, "asks of a space out of turn")?;
				ask_as_responder(transport, holder, (now, &peer))?;
			}
			SyncMessage::Prove(space, challenges) => {
				// Only once both sides asked for their blobs, and once in a space's sync, so
				// that a peer makes this side read at most 64 MiB for each space.
				let Some(now) = current.as_mut().filter(|now| {
					now.synced.space == space && now.asks_done && !now.awaiting && !now.proved
				}) else {
					return Err(SyncError::protocol("proofs asked for out of turn"));
				};
				now.proved = true;
				answer_challenges(transport, holder, (space, &peer), &challenges)?;
			}
			// The initiator sent the blobs that this side asked for.
			SyncMessage::Sent(space)
				if current
					.as_ref()
					.is_some_and(|now| now.synced.space == space && now.awaiting) =>
			{
				if let Some(now) = current.as_mut() {
					now.awaiting = false;
				}
			}
			SyncMessage::Done => {
				finish(holder, current.take(), &mut synced)?;
				SyncMessage::Done.send(transport)?;
				return Ok(synced);
			}
			_ => return Err(SyncError::protocol("a message out of turn")),
		}
	}
}

/// The space that a responder syncs, if a message of `space` is of it; else the protocol
/// is broken, as `what` says.
fn of_space<'c>(
	current: &'c mut Option<Current>,
	space: SpaceId,
	what: &'static str,
) -> Result<&'c mut Current, SyncError> {
	current
		.as_mut()
		.filter(|now| now.synced.space == space)
		.ok_or_else(|| SyncError::protocol(what))
}

/// Readies a space for its sync as the responder: the replica's own account brought up to
/// date, and what the space holds put into negentropy's set.
fn begin_space<H: Holder>(holder: &mut H, space: SpaceId) -> Result<Current, SyncError> {
	holder.prepare(space).map_err(SyncError::holder)?;
	let holdings = holder.holdings(space).map_err(SyncError::holder)?;
	Ok(Current {
		reconciling: Reconciling::new(holdings)?,
		synced: SpaceSynced {
			space,
			sent: Counts::default(),
			received: Counts::default(),
		},
		asked: BTreeSet::new(),
		awaiting: false,
		asks_done: false,
		proved: false,
	})
}

/// Sends the items that the initiator asked a responder for, those it holds, then says it
/// sent them.
fn send_wanted<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(now, peer): (&mut Current, &VerifyingKey),
	ids: &[[u8; 32]],
) -> Result<(), SyncError> {
	let space = now.synced.space;
	// Sending may take long, and what came before is not to wait meanwhile.
	holder.pause().map_err(SyncError::holder)?;
	for holding in now.reconciling.in_order(ids) {
		send_item(
			transport,
			holder,
			(space, peer),
			&holding,
			&mut now.synced.sent,
		)?;
	}
	SyncMessage::Sent(space).send(transport)
}

/// Sends the sealed blobs that the initiator asked a responder for, those it holds, then
/// says it sent them.
fn answer_asked<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(now, peer): (&mut Current, &VerifyingKey),
	wanted: &[Hash],
) -> Result<(), SyncError> {
	if wanted.len() > WANTED_AT_ONCE {
		return Err(SyncError::protocol("too many blobs asked for at once"));
	}
	let space = now.synced.space;
	holder.pause().map_err(SyncError::holder)?;
	send_blobs(
		transport,
		holder,
		(space, peer),
		wanted,
		&mut now.synced.sent,
	)?;
	SyncMessage::Sent(space).send(transport)
}

/// Answers, as the responder, the challenges that the initiator put to it, once what came is
/// saved: one answer for each, in their order.
fn answer_challenges<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(space, peer): (SpaceId, &VerifyingKey),
	challenges: &[Challenge],
) -> Result<(), SyncError> {
	holder.pause().map_err(SyncError::holder)?;
	let mut answers = holder
		.prove(space, peer, challenges)
		.map_err(SyncError::holder)?;
	answers.resize(challenges.len(), None);
	SyncMessage::Proofs(space, answers).send(transport)
}

/// Asks, as the responder, for the sealed blobs of the space that it keeps and lacks, once
/// what came is saved, so that it knows what the space's files are now.
fn ask_as_responder<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(now, peer): (&mut Current, &VerifyingKey),
) -> Result<(), SyncError> {
	// The initiator asks again only while the asks before filled a message.
	if now.asks_done || now.awaiting {
		return Err(SyncError::protocol("asks of a space out of turn"));
	}
	let space = now.synced.space;
	holder.pause().map_err(SyncError::holder)?;
	let wanted = holder
		.wanted_blobs(space, peer, &now.asked, WANTED_AT_ONCE)
		.map_err(SyncError::holder)?;
	now.asked.extend(wanted.iter().copied());
	now.asks_done = wanted.len() < WANTED_AT_ONCE;
	now.awaiting = true;
	SyncMessage::Asked(space, wanted).send(transport)
}

/// The next message, once the holder has saved what it received, if the message has not
/// arrived yet.
fn next_message<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
) -> Result<SyncMessage, SyncError> {
	if !transport.has_message() {
		holder.pause().map_err(SyncError::holder)?;
	}
	SyncMessage::receive(transport)
}

/// Saves what a responder received of a space.
fn finish(
	holder: &mut impl Holder,
	current: Option<Current>,
	synced: &mut Synced,
) -> Result<(), SyncError> {
	if let Some(current) = current {
		holder
			.finish_space(current.synced.space)
			.map_err(SyncError::holder)?;
		synced.spaces.push(current.synced);
	}
	Ok(())
}

/// Sends an item that the other side lacks. One that this side no longer has, or does not
/// send that peer, is left out.
fn send_item<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &H,
	(space, peer): (SpaceId, &VerifyingKey),
	holding: &Holding,
	sent: &mut Counts,
) -> Result<(), SyncError> {
	if !holder
		.sends(space, peer, holding)
		.map_err(SyncError::holder)?
	{
		return Ok(());
	}
	match holding {
		Holding::Change { id, .. } => {
			if let Some(parcel) = holder.parcel(space, *id).map_err(SyncError::holder)? {
				SyncMessage::Item(space, Item::Change(Box::new(parcel))).send(transport)?;
				sent.changes += 1;
			}
		}
		Holding::RecordKey { statement, key } => {
			SyncMessage::Item(space, Item::RecordKey(*statement, key.clone())).send(transport)?;
			sent.keys += 1;
		}
		Holding::BlobKey { sealed, key } => {
			SyncMessage::Item(space, Item::BlobKey(*sealed, key.clone())).send(transport)?;
			sent.keys += 1;
		}
		Holding::Account { id, .. } => {
			if let Some(page) = holder.account(space, id).map_err(SyncError::holder)? {
				SyncMessage::Item(space, Item::Account(page)).send(transport)?;
			}
		}
	}
	Ok(())
}

/// Takes an item from the other side; a sealed blob only if it is among `asked`.
fn receive_item<T: Transport, H: Holder>(
	transport: &mut T,
	holder: &mut H,
	(space, peer): (SpaceId, &VerifyingKey),
	item: Item,
	asked: &BTreeSet<Hash>,
	received: &mut Counts,
) -> Result<(), SyncError> {
	match item {
		Item::Change(parcel) => {
			if holder
				.receive_change(space, peer, *parcel)
				.map_err(SyncError::holder)?
			{
				received.changes += 1;
			}
		}
		Item::RecordKey(statement, key) => {
			if holder
				.receive_record_key(space, statement, key)
				.map_err(SyncError::holder)?
			{
				received.keys += 1;
			}
		}
		Item::BlobKey(sealed, key) => {
			if holder
				.receive_blob_key(space, sealed, key)
				.map_err(SyncError::holder)?
			{
				received.keys += 1;
			}
		}
		Item::Blob(sealed, size) => {
			let mut incoming = Incoming {
				transport,
				left: size,
				buffer: Vec::new(),
				at: 0,
				error: None,
			};
			// A blob that was not asked for is read past, and kept nowhere: a replica keeps only
			// what it asks for.
			let kept = if asked.contains(&sealed) {
				holder.receive_blob(space, sealed, size, &mut incoming)
			} else {
				Ok(false)
			};
			// What the holder did not read is read all the same, to reach the next message.
			while incoming.error.is_none()
				&& (incoming.at < incoming.buffer.len() || incoming.left > 0)
			{
				incoming.at = incoming.buffer.len();
				let _ = incoming.fill();
			}
			if let Some(error) = incoming.error {
				return Err(error);
			}
			if kept.map_err(SyncError::holder)? {
				received.blobs += 1;
				received.blob_bytes += size;
			}
		}
		Item::Account(page) => {
			holder
				.receive_account(space, page)
				.map_err(SyncError::holder)?;
		}
	}
	Ok(())
}

/// The bytes of a blob, as they come in messages.
struct Incoming<'t, T> {
	transport: &'t mut T,
	/// The bytes still to come in messages.
	left: u64,
	/// The bytes of the last message, and how many of them were read.
	buffer: Vec<u8>,
	at: usize,
	/// What went wrong with the connection, which ends the sync.
	error: Option<SyncError>,
}

impl<T: Transport> Incoming<'_, T> {
	/// Waits for the next message of bytes, if more are to come.
	fn fill(&mut self) -> io::Result<()> {
		if self.left == 0 {
			return Ok(());
		}
		let received = SyncMessage::receive(self.transport).and_then(|message| match message {
			SyncMessage::Bytes(bytes) if !bytes.is_empty() && bytes.len() as u64 <= self.left => {
				Ok(bytes)
			}
			_ => Err(SyncError::protocol("expected a blob's bytes")),
		});
		match received {
			Ok(bytes) => {
				self.left -= bytes.len() as u64;
				self.buffer = bytes;
				self.at = 0;
				Ok(())
			}
			Err(error) => {
				self.error = Some(error);
				Err(io::Error::other("the connection failed"))
			}
		}
	}
}

impl<T: Transport> Read for Incoming<'_, T> {
	fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
		if self.error.is_some() {
			return Err(io::Error::other("the connection failed"));
		}
		if self.at == self.buffer.len() {
			self.fill()?;
			if self.at == self.buffer.len() {
				return Ok(0);
			}
		}
		let count = out.len().min(self.buffer.len() - self.at);
		out[..count].copy_from_slice(&self.buffer[self.at..self.at + count]);
		self.at += count;
		Ok(count)
	}
}
