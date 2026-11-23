//! The messages of pairing and of syncing, in canonical CBOR: arrays whose first item is the
//! message's kind ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md)).

use nibkeep_base::cbor::{self, Bytes, FromCbor, ToCbor, Value};
use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_crypto::{Signature, VerifyingKey};
use nibkeep_governance::{
	Grant, InvitationGrant, InvitationRequest, JoinRequest, Offer, RecoveryListing,
};
use nibkeep_log::{Change, Parcel, SealedPayload, WrappedBlobKey, WrappedChangeKey, WrappedKey};

use crate::{
	CHALLENGES_AT_ONCE, Challenge, MOST_INTRODUCTIONS, MOST_RANGES, Range, Reason, SyncError,
	Transport,
};

/// A message of pairing.
#[derive(Debug)]
pub(crate) enum PairMessage {
	/// `[0, reason]`: the sender ends the connection.
	End(Reason),
	/// `[1, message]`: the joiner's message of SPAKE2; `[1, message, tag]`, the sponsor's,
	/// with the tag of its message, which proves that it has the code.
	Pake(Vec<u8>, Option<[u8; 32]>),
	/// `[2, request, tag]`: what the joiner asks for.
	Request(Box<JoinRequest>, [u8; 32]),
	/// `[3, grant, tag]`: what the sponsor grants.
	Grant(Box<Grant>, [u8; 32]),
	/// `[4]`: the joiner made its library; `[4, listings]` for an AI agent's replica, with its
	/// listings in the spaces chosen for it, as text, in the grant's order.
	Joined(Vec<String>),
}

/// A message of an invitation to a space.
#[derive(Debug)]
pub(crate) enum InviteMessage {
	/// `[0, reason]`: the sender ends the connection.
	End(Reason),
	/// `[1, message]`: the joiner's message of SPAKE2; `[1, message, tag]`, the admin's,
	/// with the tag of its message, which proves that it has the code.
	Pake(Vec<u8>, Option<[u8; 32]>),
	/// `[2, request, tag]`: who the joiner is.
	Request(Box<InvitationRequest>, [u8; 32]),
	/// `[3, offer, tag]`: what the admin offers, once the person confirmed who asks.
	Offer(Box<Offer>, [u8; 32]),
	/// `[4, listing, tag]`: the joiner's listing in the space, which its key signed, as text.
	Listing(String, [u8; 32]),
	/// `[5, grant, tag]`: the space's first key, sealed to the joiner.
	Grant(Box<InvitationGrant>, [u8; 32]),
	/// `[6]`: the joiner holds the space now.
	Joined,
}

/// A thing a space holds, as it travels.
#[derive(Debug)]
pub(crate) enum Item {
	/// `[4, space, 0, change, payload, keys]`.
	Change(Box<Parcel>),
	/// `[4, space, 1, statement, wrapped key]`.
	RecordKey(StatementId, WrappedKey),
	/// `[4, space, 2, sealed hash, wrapped blob key]`.
	BlobKey(Hash, WrappedBlobKey),
	/// `[4, space, 3, sealed hash, size]`, followed by the blob's bytes: sent only when
	/// asked for.
	Blob(Hash, u64),
	/// `[4, space, 4, page]`: a page of a replica's account of what it holds, as its
	/// encoding.
	Account(Vec<u8>),
}

/// A message of syncing.
#[derive(Debug)]
pub(crate) enum SyncMessage {
	/// `[0, reason]`: the sender ends the connection.
	End(Reason),
	/// `[1, key, signature]`: who the sender is; or `[1, key, signature, listings]`, from an
	/// initiator listed by a recovery code, which introduces itself so to a peer that does
	/// not know its key yet.
	Hello(VerifyingKey, Signature, Vec<RecoveryListing>),
	/// `[2, spaces]`: the spaces to sync.
	Spaces(Vec<SpaceId>),
	/// `[3, space, message]`: a message of negentropy.
	Reconcile(SpaceId, Vec<u8>),
	/// `[4, space, …]`: a thing the space holds.
	Item(SpaceId, Item),
	/// `[5, space, wanted]`: the IDs of the items the sender lacks.
	Wanted(SpaceId, Vec<[u8; 32]>),
	/// `[6, space]`: the sender sent what was wanted.
	Sent(SpaceId),
	/// `[7, bytes]`: some of a blob's bytes.
	Bytes(Vec<u8>),
	/// `[8]`: the sync is done.
	Done,
	/// `[9, space, sealed hashes]`: the sealed blobs that the sender asks for.
	Asked(SpaceId, Vec<Hash>),
	/// `[10, space]`: the initiator asks which sealed blobs the responder asks for.
	YourAsks(SpaceId),
	/// `[11, space, challenges]`: the initiator asks the responder to prove that it holds
	/// these sealed blobs now.
	Prove(SpaceId, Vec<Challenge>),
	/// `[12, space, answers]`: the responder's answers, in the challenges' order, each null
	/// for a blob that it does not prove.
	Proofs(SpaceId, Vec<Option<[u8; 32]>>),
}

/// A message's items, if it is an array that starts with an unsigned integer: its kind.
fn kind_and_items(bytes: &[u8]) -> Result<(u64, Vec<Value>), SyncError> {
	let value = cbor::decode(bytes).map_err(|error| SyncError::protocol(error.to_string()))?;
	kind_and_items_of(value)
}

/// The same, of a message already decoded.
fn kind_and_items_of(value: Value) -> Result<(u64, Vec<Value>), SyncError> {
	let Value::Array(mut items) = value else {
		return Err(SyncError::protocol("a message that is not an array"));
	};
	if items.is_empty() {
		return Err(SyncError::protocol("an empty message"));
	}
	let kind = u64::from_cbor(&items.remove(0)).map_err(protocol)?;
	Ok((kind, items))
}

#[expect(clippy::needless_pass_by_value, reason = "for map_err")]
fn protocol(error: cbor::Error) -> SyncError {
	SyncError::protocol(error.to_string())
}

fn bytes_of(value: &Value) -> Result<Vec<u8>, SyncError> {
	Ok(Bytes::from_cbor(value).map_err(protocol)?.0)
}

fn tag_of(value: &Value) -> Result<[u8; 32], SyncError> {
	bytes_of(value)?
		.try_into()
		.map_err(|_| SyncError::protocol("a tag that is not 32 bytes"))
}

fn reason_of(value: &Value) -> Result<Reason, SyncError> {
	let number = u8::from_cbor(value).map_err(protocol)?;
	Reason::of_number(number).ok_or_else(|| SyncError::protocol("an unknown reason"))
}

/// The message that ends a connection, `[0, reason]`, in either protocol.
pub(crate) fn end_message(reason: Reason) -> Vec<u8> {
	message(0, vec![reason.number().to_cbor()])
}

/// The message `[7, bytes]` that carries some of a blob's bytes, written straight from them:
/// the array's head, the kind, and the byte string's head, in canonical CBOR.
pub(crate) fn bytes_message(bytes: &[u8]) -> Vec<u8> {
	let mut message = Vec::with_capacity(bytes.len() + 11);
	message.extend([0x82, 0x07]);
	let length = bytes.len() as u64;
	match length {
		0..24 => message.push(0x40 | u8::try_from(length).unwrap_or(0)),
		24..0x100 => message.extend([0x58, u8::try_from(length).unwrap_or(0)]),
		0x100..0x1_0000 => {
			message.push(0x59);
			message.extend(u16::try_from(length).unwrap_or(0).to_be_bytes());
		}
		0x1_0000..0x1_0000_0000 => {
			message.push(0x5a);
			message.extend(u32::try_from(length).unwrap_or(0).to_be_bytes());
		}
		_ => {
			message.push(0x5b);
			message.extend(length.to_be_bytes());
		}
	}
	message.extend_from_slice(bytes);
	message
}

fn message(kind: u8, mut items: Vec<Value>) -> Vec<u8> {
	items.insert(0, kind.to_cbor());
	cbor::encode(&Value::Array(items))
}

/// `[1, message]`, or `[1, message, tag]`.
fn pake_message(bytes: &[u8], tag: Option<&[u8; 32]>) -> Vec<u8> {
	let mut items = vec![Value::Bytes(bytes.to_vec())];
	items.extend(tag.map(|tag| Value::Bytes(tag.to_vec())));
	message(1, items)
}

fn wrong_form(kind: u64) -> SyncError {
	SyncError::protocol(format!("a message of kind {kind} of the wrong form"))
}

impl PairMessage {
	pub(crate) fn encode(&self) -> Vec<u8> {
		match self {
			Self::End(reason) => end_message(*reason),
			Self::Pake(bytes, tag) => pake_message(bytes, tag.as_ref()),
			Self::Request(request, tag) => {
				message(2, vec![request.to_cbor(), Value::Bytes(tag.to_vec())])
			}
			Self::Grant(grant, tag) => {
				message(3, vec![grant.to_cbor(), Value::Bytes(tag.to_vec())])
			}
			Self::Joined(listings) if listings.is_empty() => message(4, Vec::new()),
			Self::Joined(listings) => message(
				4,
				vec![Value::Array(listings.iter().map(ToCbor::to_cbor).collect())],
			),
		}
	}

	pub(crate) fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
		let (kind, items) = kind_and_items(bytes)?;
		Ok(match (kind, items.as_slice()) {
			(0, [reason]) => Self::End(reason_of(reason)?),
			(1, [bytes]) => Self::Pake(bytes_of(bytes)?, None),
			(1, [bytes, tag]) => Self::Pake(bytes_of(bytes)?, Some(tag_of(tag)?)),
			(2, [request, tag]) => Self::Request(
				Box::new(JoinRequest::from_cbor(request).map_err(protocol)?),
				tag_of(tag)?,
			),
			(3, [grant, tag]) => Self::Grant(
				Box::new(Grant::from_cbor(grant).map_err(protocol)?),
				tag_of(tag)?,
			),
			(4, []) => Self::Joined(Vec::new()),
			(4, [Value::Array(listings)]) if !listings.is_empty() => Self::Joined(
				listings
					.iter()
					.map(String::from_cbor)
					.collect::<Result<_, _>>()
					.map_err(protocol)?,
			),
			_ => return Err(wrong_form(kind)),
		})
	}

	/// Sends the message.
	pub(crate) fn send(&self, transport: &mut impl Transport) -> Result<(), SyncError> {
		Ok(transport.send(&self.encode())?)
	}

	/// Waits for the next message; one that ends the connection is an error.
	pub(crate) fn receive(transport: &mut impl Transport) -> Result<Self, SyncError> {
		match Self::decode(&transport.receive()?)? {
			Self::End(reason) => Err(SyncError::Ended(reason)),
			message => Ok(message),
		}
	}
}

impl InviteMessage {
	pub(crate) fn encode(&self) -> Vec<u8> {
		let tagged = |kind, value: Value, tag: &[u8; 32]| {
			message(kind, vec![value, Value::Bytes(tag.to_vec())])
		};
		match self {
			Self::End(reason) => end_message(*reason),
			Self::Pake(bytes, tag) => pake_message(bytes, tag.as_ref()),
			Self::Request(request, tag) => tagged(2, request.to_cbor(), tag),
			Self::Offer(offer, tag) => tagged(3, offer.to_cbor(), tag),
			Self::Listing(listing, tag) => tagged(4, listing.to_cbor(), tag),
			Self::Grant(grant, tag) => tagged(5, grant.to_cbor(), tag),
			Self::Joined => message(6, Vec::new()),
		}
	}

	pub(crate) fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
		let (kind, items) = kind_and_items(bytes)?;
		Ok(match (kind, items.as_slice()) {
			(0, [reason]) => Self::End(reason_of(reason)?),
			(1, [bytes]) => Self::Pake(bytes_of(bytes)?, None),
			(1, [bytes, tag]) => Self::Pake(bytes_of(bytes)?, Some(tag_of(tag)?)),
			(2, [request, tag]) => Self::Request(
				Box::new(InvitationRequest::from_cbor(request).map_err(protocol)?),
				tag_of(tag)?,
			),
			(3, [offer, tag]) => Self::Offer(
				Box::new(Offer::from_cbor(offer).map_err(protocol)?),
				tag_of(tag)?,
			),
			(4, [listing, tag]) => {
				Self::Listing(String::from_cbor(listing).map_err(protocol)?, tag_of(tag)?)
			}
			(5, [grant, tag]) => Self::Grant(
				Box::new(InvitationGrant::from_cbor(grant).map_err(protocol)?),
				tag_of(tag)?,
			),
			(6, []) => Self::Joined,
			_ => return Err(wrong_form(kind)),
		})
	}

	/// Sends the message.
	pub(crate) fn send(&self, transport: &mut impl Transport) -> Result<(), SyncError> {
		Ok(transport.send(&self.encode())?)
	}

	/// Waits for the next message; one that ends the connection is an error.
	pub(crate) fn receive(transport: &mut impl Transport) -> Result<Self, SyncError> {
		match Self::decode(&transport.receive()?)? {
			Self::End(reason) => Err(SyncError::Ended(reason)),
			message => Ok(message),
		}
	}
}

impl SyncMessage {
	pub(crate) fn encode(&self) -> Vec<u8> {
		match self {
			Self::End(reason) => end_message(*reason),
			Self::Hello(key, signature, listings) => {
				let mut items = vec![key.to_cbor(), signature.to_cbor()];
				if !listings.is_empty() {
					items.push(Value::Array(listings.iter().map(ToCbor::to_cbor).collect()));
				}
				message(1, items)
			}
			Self::Spaces(spaces) => message(
				2,
				vec![Value::Array(spaces.iter().map(ToCbor::to_cbor).collect())],
			),
			Self::Reconcile(space, bytes) => {
				message(3, vec![space.to_cbor(), Value::Bytes(bytes.clone())])
			}
			Self::Item(space, item) => {
				let mut items = vec![space.to_cbor()];
				match item {
					Item::Change(parcel) => items.extend([
						0_u8.to_cbor(),
						parcel.change.to_cbor(),
						parcel.payload.as_ref().map_or(Value::Null, ToCbor::to_cbor),
						Value::Array(parcel.keys.iter().map(ToCbor::to_cbor).collect()),
					]),
					Item::RecordKey(statement, key) => {
						items.extend([1_u8.to_cbor(), statement.to_cbor(), key.to_cbor()]);
					}
					Item::BlobKey(sealed, key) => {
						items.extend([2_u8.to_cbor(), sealed.to_cbor(), key.to_cbor()]);
					}
					Item::Blob(sealed, size) => {
						items.extend([3_u8.to_cbor(), sealed.to_cbor(), size.to_cbor()]);
					}
					Item::Account(page) => {
						items.extend([4_u8.to_cbor(), Value::Bytes(page.clone())]);
					}
				}
				message(4, items)
			}
			Self::Wanted(space, ids) => message(
				5,
				vec![
					space.to_cbor(),
					Value::Array(ids.iter().map(|id| Value::Bytes(id.to_vec())).collect()),
				],
			),
			Self::Sent(space) => message(6, vec![space.to_cbor()]),
			Self::Bytes(bytes) => bytes_message(bytes),
			Self::Done => message(8, Vec::new()),
			Self::Asked(space, sealed) => message(
				9,
				vec![
					space.to_cbor(),
					Value::Array(sealed.iter().map(ToCbor::to_cbor).collect()),
				],
			),
			Self::YourAsks(space) => message(10, vec![space.to_cbor()]),
			Self::Prove(space, challenges) => message(
				11,
				vec![
					space.to_cbor(),
					Value::Array(challenges.iter().map(challenge_cbor).collect()),
				],
			),
			Self::Proofs(space, answers) => message(
				12,
				vec![
					space.to_cbor(),
					Value::Array(
						answers
							.iter()
							.map(|answer| {
								answer.map_or(Value::Null, |hash| Value::Bytes(hash.to_vec()))
							})
							.collect(),
					),
				],
			),
		}
	}

	pub(crate) fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
		Self::of_items(kind_and_items(bytes)?)
	}

	/// A message from its value, decoded already.
	pub(crate) fn from_value(value: Value) -> Result<Self, SyncError> {
		Self::of_items(kind_and_items_of(value)?)
	}

	fn of_items((kind, mut items): (u64, Vec<Value>)) -> Result<Self, SyncError> {
		// A blob's bytes, the most of what comes, are taken as they are, not copied.
		if kind == 7
			&& let [Value::Bytes(_)] = items.as_slice()
			&& let Some(Value::Bytes(bytes)) = items.pop()
		{
			return Ok(Self::Bytes(bytes));
		}
		let space = |value: &Value| SpaceId::from_cbor(value).map_err(protocol);
		Ok(match (kind, items.as_slice()) {
			(0, [reason]) => Self::End(reason_of(reason)?),
			(1, [key, signature, rest @ ..]) => {
				// Listings, when there are any: from 1 to [`MOST_INTRODUCTIONS`].
				let listings = match rest {
					[] => Vec::new(),
					[Value::Array(listings)]
						if (1..=MOST_INTRODUCTIONS).contains(&listings.len()) =>
					{
						listings
							.iter()
							.map(|listing| RecoveryListing::from_cbor(listing).map_err(protocol))
							.collect::<Result<Vec<_>, SyncError>>()?
					}
					_ => return Err(wrong_form(1)),
				};
				Self::Hello(
					VerifyingKey::from_cbor(key).map_err(protocol)?,
					Signature::from_cbor(signature).map_err(protocol)?,
					listings,
				)
			}
			(2, [Value::Array(spaces)]) => Self::Spaces(
				spaces
					.iter()
					.map(space)
					.collect::<Result<Vec<_>, SyncError>>()?,
			),
			(3, [id, bytes]) => Self::Reconcile(space(id)?, bytes_of(bytes)?),
			(4, [id, rest @ ..]) => Self::Item(space(id)?, item_of(rest)?),
			(5, [id, Value::Array(ids)]) => Self::Wanted(
				space(id)?,
				ids.iter()
					.map(tag_of)
					.collect::<Result<Vec<_>, SyncError>>()?,
			),
			(6, [id]) => Self::Sent(space(id)?),
			(8, []) => Self::Done,
			(9, [id, Value::Array(sealed)]) => Self::Asked(
				space(id)?,
				sealed
					.iter()
					.map(|sealed| Hash::from_cbor(sealed).map_err(protocol))
					.collect::<Result<Vec<_>, SyncError>>()?,
			),
			(10, [id]) => Self::YourAsks(space(id)?),
			(11, [id, Value::Array(challenges)]) if challenges.len() <= CHALLENGES_AT_ONCE => {
				Self::Prove(
					space(id)?,
					challenges
						.iter()
						.map(challenge_of)
						.collect::<Result<Vec<_>, SyncError>>()?,
				)
			}
			(12, [id, Value::Array(answers)]) if answers.len() <= CHALLENGES_AT_ONCE => {
				Self::Proofs(
					space(id)?,
					answers
						.iter()
						.map(|answer| match answer {
							Value::Null => Ok(None),
							answer => tag_of(answer).map(Some),
						})
						.collect::<Result<Vec<_>, SyncError>>()?,
				)
			}
			_ => return Err(wrong_form(kind)),
		})
	}

	/// Sends the message.
	pub(crate) fn send(&self, transport: &mut impl Transport) -> Result<(), SyncError> {
		Ok(transport.send(&self.encode())?)
	}

	/// Waits for the next message; one that ends the connection is an error.
	pub(crate) fn receive(transport: &mut impl Transport) -> Result<Self, SyncError> {
		match Self::decode(&transport.receive()?)? {
			Self::End(reason) => Err(SyncError::Ended(reason)),
			message => Ok(message),
		}
	}
}

/// A challenge, as it travels: `[sealed hash, key, [[offset, length], …]]`, or for a plain
/// one `[content hash, key, [[offset, length], …], true]`.
fn challenge_cbor(challenge: &Challenge) -> Value {
	let mut items = vec![
		challenge.sealed.to_cbor(),
		Value::Bytes(challenge.key.to_vec()),
		Value::Array(
			challenge
				.ranges
				.iter()
				.map(|range| Value::Array(vec![range.offset.to_cbor(), range.len.to_cbor()]))
				.collect(),
		),
	];
	if challenge.plain {
		items.push(Value::Bool(true));
	}
	Value::Array(items)
}

/// A challenge from its form as it travels; one that a prover would not read, as one with
/// too many or too long ranges, breaks the protocol.
fn challenge_of(value: &Value) -> Result<Challenge, SyncError> {
	let Value::Array(items) = value else {
		return Err(wrong_form(11));
	};
	let (sealed, key, ranges, plain) = match items.as_slice() {
		[sealed, key, Value::Array(ranges)] => (sealed, key, ranges, false),
		[sealed, key, Value::Array(ranges), Value::Bool(true)] => (sealed, key, ranges, true),
		_ => return Err(wrong_form(11)),
	};
	if ranges.len() > MOST_RANGES {
		return Err(SyncError::protocol("a challenge of too many ranges"));
	}
	let challenge = Challenge {
		sealed: Hash::from_cbor(sealed).map_err(protocol)?,
		key: tag_of(key)?,
		ranges: ranges
			.iter()
			.map(|range| match range {
				Value::Array(pair) => match pair.as_slice() {
					[offset, len] => Ok(Range {
						offset: u64::from_cbor(offset).map_err(protocol)?,
						len: u32::from_cbor(len).map_err(protocol)?,
					}),
					_ => Err(wrong_form(11)),
				},
				_ => Err(wrong_form(11)),
			})
			.collect::<Result<Vec<_>, SyncError>>()?,
		plain,
	};
	if !challenge.is_fair() {
		return Err(SyncError::protocol("a challenge that no prover reads"));
	}
	Ok(challenge)
}

/// An item from the rest of `[4, space, …]`.
fn item_of(items: &[Value]) -> Result<Item, SyncError> {
	let Some((kind, rest)) = items.split_first() else {
		return Err(wrong_form(4));
	};
	Ok(match (u8::from_cbor(kind).map_err(protocol)?, rest) {
		(0, [change, payload, Value::Array(keys)]) => Item::Change(Box::new(Parcel {
			change: Change::from_cbor(change).map_err(protocol)?,
			payload: match payload {
				Value::Null => None,
				payload => Some(SealedPayload::from_cbor(payload).map_err(protocol)?),
			},
			keys: keys
				.iter()
				.map(|key| WrappedChangeKey::from_cbor(key).map_err(protocol))
				.collect::<Result<Vec<_>, SyncError>>()?,
		})),
		(1, [statement, key]) => Item::RecordKey(
			StatementId::from_cbor(statement).map_err(protocol)?,
			WrappedKey::from_cbor(key).map_err(protocol)?,
		),
		(2, [sealed, key]) => Item::BlobKey(
			Hash::from_cbor(sealed).map_err(protocol)?,
			WrappedBlobKey::from_cbor(key).map_err(protocol)?,
		),
		(3, [sealed, size]) => Item::Blob(
			Hash::from_cbor(sealed).map_err(protocol)?,
			u64::from_cbor(size).map_err(protocol)?,
		),
		(4, [page]) => Item::Account(bytes_of(page)?),
		_ => return Err(wrong_form(4)),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A challenge of `ranges` ranges of `len` bytes each.
	fn prove(ranges: usize, len: u32) -> Vec<u8> {
		SyncMessage::Prove(
			SpaceId::from_bytes([1; 16]),
			vec![Challenge {
				sealed: Hash::of(b"sealed"),
				key: [2; 32],
				ranges: vec![Range { offset: 5, len }; ranges],
				plain: false,
			}],
		)
		.encode()
	}

	/// A hello with `count` listings by a recovery code.
	fn hello(count: usize) -> Vec<u8> {
		let mut rng = nibkeep_testkit::TestRng::new("hello");
		let signing = rng.signing_key();
		let code = nibkeep_crypto::RecoveryCode::generate(&mut rng);
		let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
		let listing = RecoveryListing::sign(
			&code,
			(rng.space_id(), rng.entity_id(), rng.entity_id()),
			&signing.verifying_key(),
			&encryption,
		);
		SyncMessage::Hello(
			signing.verifying_key(),
			signing.sign("test", b"hello"),
			vec![listing; count],
		)
		.encode()
	}

	#[test]
	fn a_hello_carries_listings_by_a_code_but_not_too_many() {
		for count in [0, 1, MOST_INTRODUCTIONS] {
			let Ok(SyncMessage::Hello(_, _, listings)) = SyncMessage::decode(&hello(count)) else {
				panic!("a hello with {count} listings");
			};
			assert_eq!(listings.len(), count);
		}
		assert!(SyncMessage::decode(&hello(MOST_INTRODUCTIONS + 1)).is_err());
		// No listings are written as none at all, never as an empty array.
		let Ok(Value::Array(mut items)) = cbor::decode(&hello(1)) else {
			panic!("an array");
		};
		items[3] = Value::Array(Vec::new());
		assert!(SyncMessage::decode(&cbor::encode(&Value::Array(items))).is_err());
	}

	#[test]
	fn a_challenge_that_reads_too_much_breaks_the_protocol() {
		assert!(SyncMessage::decode(&prove(MOST_RANGES, crate::MOST_RANGE_LEN)).is_ok());
		assert!(SyncMessage::decode(&prove(MOST_RANGES + 1, 10)).is_err());
		assert!(SyncMessage::decode(&prove(1, crate::MOST_RANGE_LEN + 1)).is_err());
		assert!(SyncMessage::decode(&prove(1, 0)).is_err());
		assert!(SyncMessage::decode(&prove(0, 10)).is_err());
		let space = SpaceId::from_bytes([1; 16]);
		let challenge = Challenge {
			sealed: Hash::of(b"sealed"),
			key: [2; 32],
			ranges: vec![Range {
				offset: u64::MAX,
				len: 2,
			}],
			plain: false,
		};
		assert!(SyncMessage::decode(&SyncMessage::Prove(space, vec![challenge]).encode()).is_err());
		let plain = Challenge {
			sealed: Hash::of(b"content"),
			key: [3; 32],
			ranges: vec![Range { offset: 0, len: 9 }],
			plain: true,
		};
		let Ok(SyncMessage::Prove(_, read)) =
			SyncMessage::decode(&SyncMessage::Prove(space, vec![plain.clone()]).encode())
		else {
			panic!("reads a plain challenge");
		};
		assert_eq!(read, vec![plain]);
		let many = vec![None; CHALLENGES_AT_ONCE + 1];
		assert!(SyncMessage::decode(&SyncMessage::Proofs(space, many).encode()).is_err());
	}

	#[test]
	fn a_blobs_bytes_are_written_as_canonical_cbor_writes_them() {
		for length in [0, 1, 23, 24, 255, 256, 65_535, 65_536, 1024 * 1024] {
			let bytes = vec![7_u8; length];
			let canonical = cbor::encode(&Value::Array(vec![
				7_u8.to_cbor(),
				Value::Bytes(bytes.clone()),
			]));
			assert_eq!(bytes_message(&bytes), canonical, "{length}");
			let Ok(SyncMessage::Bytes(read)) = SyncMessage::decode(&canonical) else {
				panic!("reads back, {length}");
			};
			assert_eq!(read, bytes);
		}
	}
}
