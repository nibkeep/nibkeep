//! Inviting a user to a space, between a replica of an admin of the space and the joiner's,
//! over a transport ([Sync](../../../docs/spec/sync.md#7-invitations)).

use nibkeep_base::cbor::{ToCbor, Value};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{
	InvitationGrant, InvitationRequest, Offer, Pairing, PairingCode, PairingKey, Side,
};

use crate::message::InviteMessage;
use crate::{SyncError, Transport};

/// The admin's side of an invitation: what it offers the one who asks, once the person
/// confirms, what it grants once the joiner signed its listing, and how it records the new
/// member.
pub trait Inviting {
	/// Why offering, granting or recording failed.
	type Error: std::error::Error + Send + Sync + 'static;

	/// What to offer the joiner that `request` names, connected from `endpoint`, once the
	/// person confirms who asks; `None` if they decline. Nothing is recorded yet.
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read.
	fn offer<R: CryptoRng>(
		&mut self,
		request: &InvitationRequest,
		endpoint: &[u8; 32],
		rng: &mut R,
	) -> Result<Option<Offer>, Self::Error>;

	/// What to grant the joiner whose key signed `listing`, its listing in the space as the
	/// offer said, as text.
	///
	/// # Errors
	///
	/// Returns an error if the listing is not the joiner's as the request and the offer
	/// say, or the library cannot be read.
	fn grant<R: CryptoRng>(
		&mut self,
		listing: &str,
		rng: &mut R,
	) -> Result<InvitationGrant, Self::Error>;

	/// Records the new member and its replica, now that the joiner holds the space.
	///
	/// # Errors
	///
	/// Returns an error if they cannot be recorded.
	fn record<R: CryptoRng>(&mut self, rng: &mut R) -> Result<(), Self::Error>;
}

/// The joiner's side of an invitation: its listing for what the admin offers, and how it
/// takes the space.
pub trait Accepting {
	/// Why signing the listing or taking the space failed.
	type Error: std::error::Error + Send + Sync + 'static;

	/// The joiner's listing in the space that `offer` names, signed by its key, as text;
	/// `None` if its library holds the space already.
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read.
	fn listing(&mut self, offer: &Offer) -> Result<Option<String>, Self::Error>;

	/// Takes the space that `grant` gives, as `offer` named it.
	///
	/// # Errors
	///
	/// Returns an error if the first key does not open, or the space cannot be made.
	fn take<R: CryptoRng>(
		&mut self,
		offer: &Offer,
		grant: &InvitationGrant,
		rng: &mut R,
	) -> Result<(), Self::Error>;
}

/// What an invitation did, on either side: who asked, what was offered and what granted.
#[derive(Clone, Debug)]
pub struct Invitation {
	/// The joiner's request.
	pub request: InvitationRequest,
	/// What the admin offered.
	pub offer: Offer,
	/// What it granted.
	pub grant: InvitationGrant,
}

/// Invites the joiner at the other end of `transport` to a space, as the admin's replica,
/// with the code it showed. The code is spent whatever happens.
///
/// # Errors
///
/// Returns [`SyncError::WrongCode`] if the joiner typed another code, on either side,
/// [`SyncError::Declined`] if the person declined who asks, [`SyncError::Ended`] with
/// [`Reason::HoldsSpace`] if the joiner holds the space already, and other errors if the
/// connection fails, the joiner breaks the protocol or does not join, or the member cannot
/// be recorded; in all of these, nothing is recorded.
///
/// [`Reason::HoldsSpace`]: crate::Reason::HoldsSpace
pub fn invite<T: Transport, I: Inviting>(
	transport: &mut T,
	inviting: &mut I,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Invitation, SyncError> {
	let result = invite_with(transport, inviting, code, rng);
	end(transport, result.as_ref().err());
	result.map_err(SyncError::either_side)
}

fn invite_with<T: Transport, I: Inviting>(
	transport: &mut T,
	inviting: &mut I,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Invitation, SyncError> {
	let InviteMessage::Pake(theirs, None) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol(
			"expected the joiner's message of SPAKE2",
		));
	};
	let (pairing, ours) = Pairing::start(Side::Sponsor, code, rng);
	let key = agree(pairing, &theirs, transport)?;
	// The admin's replica proves the code first, so that the joiner says who it is only to
	// a device that has it.
	let proof = key.tag(Side::Sponsor, &Value::Bytes(ours.clone()));
	InviteMessage::Pake(ours, Some(proof)).send(transport)?;
	let InviteMessage::Request(request, tag) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a request"));
	};
	if !key.checks(Side::Joiner, &request.to_cbor(), &tag) {
		return Err(SyncError::WrongCode);
	}
	let offer = inviting
		.offer(&request, &transport.peer_endpoint(), rng)
		.map_err(SyncError::holder)?
		.ok_or(SyncError::Declined)?;
	let tag = key.tag(Side::Sponsor, &offer.to_cbor());
	InviteMessage::Offer(Box::new(offer.clone()), tag).send(transport)?;
	let InviteMessage::Listing(listing, tag) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a listing"));
	};
	if !key.checks(Side::Joiner, &listing.to_cbor(), &tag) {
		return Err(SyncError::protocol("a listing that is not the joiner's"));
	}
	let grant = inviting.grant(&listing, rng).map_err(SyncError::holder)?;
	let tag = key.tag(Side::Sponsor, &grant.to_cbor());
	InviteMessage::Grant(Box::new(grant.clone()), tag).send(transport)?;
	// A joiner that could not take the space is not recorded.
	let InviteMessage::Joined = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected the joiner to say it joined"));
	};
	inviting.record(rng).map_err(SyncError::holder)?;
	Ok(Invitation {
		request: *request,
		offer,
		grant,
	})
}

/// Joins a space by the invitation of the admin's replica at the other end of `transport`,
/// with the code that the person typed and `request`, which says who asks, and takes the
/// space as `accepting` does.
///
/// # Errors
///
/// Returns [`SyncError::WrongCode`] if the code did not match, on either side,
/// [`SyncError::HoldsSpace`] if the library holds the space already, [`SyncError::Ended`]
/// with [`Reason::Declined`] if the person who invites declined, and other errors if the
/// connection fails, the admin breaks the protocol, or the space cannot be taken.
///
/// [`Reason::Declined`]: crate::Reason::Declined
pub fn accept<T: Transport, A: Accepting>(
	transport: &mut T,
	request: &InvitationRequest,
	accepting: &mut A,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Invitation, SyncError> {
	let result = accept_with(transport, request, accepting, code, rng);
	end(transport, result.as_ref().err());
	result.map_err(SyncError::either_side)
}

fn accept_with<T: Transport, A: Accepting>(
	transport: &mut T,
	request: &InvitationRequest,
	accepting: &mut A,
	code: &PairingCode,
	rng: &mut impl CryptoRng,
) -> Result<Invitation, SyncError> {
	let (pairing, ours) = Pairing::start(Side::Joiner, code, rng);
	InviteMessage::Pake(ours, None).send(transport)?;
	let InviteMessage::Pake(theirs, Some(proof)) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol(
			"expected the admin's message of SPAKE2, with its tag",
		));
	};
	let key = agree(pairing, &theirs, transport)?;
	// Nothing of the joiner goes to a device that has not proved the code.
	if !key.checks(Side::Sponsor, &Value::Bytes(theirs), &proof) {
		return Err(SyncError::WrongCode);
	}
	let tag = key.tag(Side::Joiner, &request.to_cbor());
	InviteMessage::Request(Box::new(request.clone()), tag).send(transport)?;
	let InviteMessage::Offer(offer, tag) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected an offer"));
	};
	// The code matched, as the admin's replica proved: a wrong tag now breaks the protocol.
	if !key.checks(Side::Sponsor, &offer.to_cbor(), &tag) {
		return Err(SyncError::protocol("an offer that is not the admin's"));
	}
	let listing = accepting
		.listing(&offer)
		.map_err(SyncError::holder)?
		.ok_or(SyncError::HoldsSpace)?;
	let tag = key.tag(Side::Joiner, &listing.to_cbor());
	InviteMessage::Listing(listing, tag).send(transport)?;
	let InviteMessage::Grant(grant, tag) = InviteMessage::receive(transport)? else {
		return Err(SyncError::protocol("expected a grant"));
	};
	if !key.checks(Side::Sponsor, &grant.to_cbor(), &tag) {
		return Err(SyncError::protocol("a grant that is not the admin's"));
	}
	accepting
		.take(&offer, &grant, rng)
		.map_err(SyncError::holder)?;
	InviteMessage::Joined.send(transport)?;
	Ok(Invitation {
		request: request.clone(),
		offer: *offer,
		grant: *grant,
	})
}

/// Finishes SPAKE2 with the other side's message, with the context of invitations.
fn agree(
	pairing: Pairing,
	theirs: &[u8],
	transport: &impl Transport,
) -> Result<PairingKey, SyncError> {
	pairing
		.finish_invitation(theirs, &transport.binding())
		.map_err(|_| SyncError::protocol("a message of SPAKE2 that is not the other side's"))
}

/// Tells the other side why the invitation ended, unless it ended it or the connection
/// failed.
fn end(transport: &mut impl Transport, error: Option<&SyncError>) {
	if let Some(reason) = error.and_then(SyncError::reason) {
		let _ = InviteMessage::End(reason).send(transport);
	}
}
