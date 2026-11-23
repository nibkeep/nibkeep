//! Pairing a new replica with one that holds the library, over a transport
//! ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#2-pairing)).

use nibkeep_base::cbor::{ToCbor, Value};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{Grant, JoinRequest, Pairing, PairingCode, Side};

use crate::message::PairMessage;
use crate::{Reason, SyncError, Transport};

/// The library's side of pairing: what grants a new replica the space keys, and records it
/// once it has joined.
pub trait Sponsoring {
	/// Why granting or recording the replica failed.
	type Error: std::error::Error + Send + Sync + 'static;

	/// What to grant the replica that `request` asks for, connected from `endpoint`, with
	/// the space keys sealed to it. Nothing is recorded yet.
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read or written.
	fn grant<R: CryptoRng>(
		&mut self,
		request: &JoinRequest,
		endpoint: &[u8; 32],
		rng: &mut R,
	) -> Result<Grant, Self::Error>;

	/// Records the replica that the last grant named, now that it has made its library, with
	/// the listings that its key signed: in the personal space first, then, for an AI agent's
	/// replica, in each space that the grant chose for it.
	///
	/// # Errors
	///
	/// Returns an error if the replica cannot be recorded, or a listing is not the one the
	/// grant asked for.
	fn record<R: CryptoRng>(&mut self, listings: &[String], rng: &mut R)
	-> Result<(), Self::Error>;
}

/// What a sponsor did: whom it added, and what it granted.
#[derive(Clone, Debug)]
pub struct Sponsored {
	/// The joiner's request.
	pub request: JoinRequest,
	/// What the joiner was granted.
	pub grant: Grant,
}

/// What a joiner was granted, once it has made its library.
#[derive(Clone, Debug)]
pub struct Joined {
	/// The grant.
	pub grant: Grant,
}

/// Pairs with the joiner at the other end of `transport`, as the sponsor, with the code it
/// showed. The code is spent whatever happens.
///
/// # Errors
///
/// Returns [`SyncError::WrongCode`] if the joiner typed another code, on either side, and
/// other errors if the connection fails, the joiner breaks the protocol or does not join,
/// or the replica cannot be recorded; in all of these, nothing is recorded.
pub fn sponsor<T: Transport, S: Sponsoring>(
	transport: &mut T,
	sponsoring: &mut S,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Sponsored, SyncError> {
	let result = sponsor_with(transport, sponsoring, code, rng);
	end(transport, result.as_ref().err());
	result.map_err(SyncError::either_side)
}

fn sponsor_with<T: Transport, S: Sponsoring>(
	transport: &mut T,
	sponsoring: &mut S,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Sponsored, SyncError> {
	let PairMessage::Pake(theirs, None) = PairMessage::receive(transport)? else {
		return Err(SyncError::protocol(
			"expected the joiner's message of SPAKE2",
		));
	};
	let (pairing, ours) = Pairing::start(Side::Sponsor, code, rng);
	let key = pairing
		.finish(&theirs, &transport.binding())
		.map_err(|_| SyncError::protocol("a message of SPAKE2 that is not the joiner's"))?;
	// The sponsor proves the code first, so that the joiner tells nothing of itself to a
	// device that does not have it.
	let proof = key.tag(Side::Sponsor, &Value::Bytes(ours.clone()));
	PairMessage::Pake(ours, Some(proof)).send(transport)?;
	let PairMessage::Request(request, tag) = PairMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a request"));
	};
	if !key.checks(Side::Joiner, &request.to_cbor(), &tag) {
		return Err(SyncError::WrongCode);
	}
	let grant = sponsoring
		.grant(&request, &transport.peer_endpoint(), rng)
		.map_err(SyncError::holder)?;
	let tag = key.tag(Side::Sponsor, &grant.to_cbor());
	PairMessage::Grant(Box::new(grant.clone()), tag).send(transport)?;
	// A joiner that could not make its library is not recorded.
	let PairMessage::Joined(listings) = PairMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected the joiner to say it joined"));
	};
	if listings.len() != grant.chosen.len() + 1 {
		return Err(SyncError::protocol(
			"a listing in the personal space, then one for each space chosen, and no other",
		));
	}
	sponsoring
		.record(&listings, rng)
		.map_err(SyncError::holder)?;
	Ok(Sponsored {
		request: *request,
		grant,
	})
}

/// Pairs with the sponsor at the other end of `transport`, as the joiner, with the code the
/// person typed, and returns what the sponsor granted. The joiner then makes its library
/// and says so with [`joined`].
///
/// # Errors
///
/// Returns [`SyncError::WrongCode`] if the code did not match, on either side, and other
/// errors if the connection fails or the sponsor breaks the protocol.
pub fn join<T: Transport>(
	transport: &mut T,
	request: &JoinRequest,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Grant, SyncError> {
	let result = join_with(transport, request, code, rng);
	end(transport, result.as_ref().err());
	result.map_err(SyncError::either_side)
}

fn join_with<T: Transport>(
	transport: &mut T,
	request: &JoinRequest,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Grant, SyncError> {
	let (pairing, ours) = Pairing::start(Side::Joiner, code, rng);
	PairMessage::Pake(ours, None).send(transport)?;
	let PairMessage::Pake(theirs, Some(proof)) = PairMessage::receive(transport)? else {
		return Err(SyncError::protocol(
			"expected the sponsor's message of SPAKE2, with its tag",
		));
	};
	let key = pairing
		.finish(&theirs, &transport.binding())
		.map_err(|_| SyncError::protocol("a message of SPAKE2 that is not the sponsor's"))?;
	// Nothing of the joiner goes to a sponsor that has not proved the code.
	if !key.checks(Side::Sponsor, &Value::Bytes(theirs), &proof) {
		return Err(SyncError::WrongCode);
	}
	let tag = key.tag(Side::Joiner, &request.to_cbor());
	PairMessage::Request(Box::new(request.clone()), tag).send(transport)?;
	let PairMessage::Grant(grant, tag) = PairMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a grant"));
	};
	// The code matched, as the sponsor proved: a wrong tag now breaks the protocol.
	if !key.checks(Side::Sponsor, &grant.to_cbor(), &tag) {
		return Err(SyncError::protocol("a grant that is not the sponsor's"));
	}
	Ok(*grant)
}

/// Tells the sponsor that the joiner made its library: the end of pairing, with the listings
/// that its key signed, in the personal space first, then, for an AI agent's replica, in
/// the spaces that the grant chose for it, in their order.
///
/// # Errors
///
/// Returns an error if the connection fails.
pub fn joined<T: Transport>(
	transport: &mut T,
	grant: Grant,
	listings: Vec<String>,
) -> Result<Joined, SyncError> {
	PairMessage::Joined(listings).send(transport)?;
	Ok(Joined { grant })
}

/// Ends a pairing or a sync before it starts, telling the other side why, as a replica that
/// cannot take it now does.
///
/// # Errors
///
/// Returns an error if the connection fails.
pub fn refuse<T: Transport>(transport: &mut T, reason: Reason) -> Result<(), SyncError> {
	Ok(transport.send(&crate::message::end_message(reason))?)
}

/// Tells the other side why pairing ended, unless it ended it or the connection failed.
fn end(transport: &mut impl Transport, error: Option<&SyncError>) {
	if let Some(reason) = error.and_then(SyncError::reason) {
		let _ = PairMessage::End(reason).send(transport);
	}
}
