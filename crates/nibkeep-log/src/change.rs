//! Changes: the operations that one replica made at one time, signed by the replica, with
//! their payload sealed under a key of the change's own.

use std::collections::{BTreeMap, BTreeSet};

pub use nibkeep_base::ChangeId;
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::small::SmallSet;
use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Envelope, Key, Signature, SigningKey, VerifyingKey, key_hash};
use nibkeep_time::Timestamp;

use crate::keys::{associated_data, change_keys_key};
use crate::{LogError, Payload, SpaceKeys};

/// The suite of the change format in version 1 of the specification.
const SUITE: u8 = 1;

/// The context of the signatures of changes.
const SIGNATURE_CONTEXT: &str = "nibkeep change";

/// What a change is for, which its header says, so that every replica can tell from
/// headers alone which changes may be purged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
	/// Any change that does not govern the space.
	#[default]
	Ordinary,
	/// A change that sets who may write to the space: the keys of replicas, and later
	/// members and roles. It holds nothing else, and is never purged.
	Governing,
}

impl ToCbor for Kind {
	fn to_cbor(&self) -> Cbor {
		match self {
			Self::Ordinary => 0u8.to_cbor(),
			Self::Governing => 1u8.to_cbor(),
		}
	}
}

impl FromCbor for Kind {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		match u8::from_cbor(value) {
			Ok(0) => Ok(Self::Ordinary),
			Ok(1) => Ok(Self::Governing),
			_ => Err(cbor::Error::Expected("a kind of change: 0 or 1")),
		}
	}
}

/// What a change purges: changes, whose keys and payloads every replica destroys; secret
/// statements, whose record keys it destroys; and blobs, by the sealed hashes of the blobs
/// whose keys it destroys.
///
/// In CBOR, purges are the array `[changes, statements, blobs]`, each sorted by its bytes
/// without duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Purges {
	/// The changes whose keys and payloads the change destroys.
	pub changes: BTreeSet<ChangeId>,
	/// The secret statements whose record keys the change destroys.
	pub statements: BTreeSet<StatementId>,
	/// The sealed hashes of the blobs whose keys the change destroys.
	pub blobs: BTreeSet<Hash>,
}

impl Purges {
	/// Whether the change purges nothing.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.changes.is_empty() && self.statements.is_empty() && self.blobs.is_empty()
	}
}

impl ToCbor for Purges {
	fn to_cbor(&self) -> Cbor {
		// Hashes of one algorithm, and so change IDs, sort by their digests, as their
		// bytes do.
		Cbor::Array(vec![
			Cbor::Array(self.changes.iter().map(ToCbor::to_cbor).collect()),
			Cbor::Array(self.statements.iter().map(ToCbor::to_cbor).collect()),
			Cbor::Array(self.blobs.iter().map(ToCbor::to_cbor).collect()),
		])
	}
}

impl FromCbor for Purges {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"purges: [changes, statements, blobs], each in ascending order without duplicates";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [changes, statements, blobs] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			changes: ascending(changes, EXPECTED)?,
			statements: ascending(statements, EXPECTED)?,
			blobs: ascending(blobs, EXPECTED)?,
		})
	}
}

/// What a change restores, after purges destroyed it: the same three lists as [`Purges`],
/// of changes whose payloads and keys come back, secret statements whose record keys come
/// back, and blobs whose keys come back, from replicas that kept them
/// ([technical design](../../../docs/Technical-design.md#911-copies-that-wait)).
pub type Restores = Purges;

/// Reads an array whose items are in ascending order, without duplicates.
pub(crate) fn ascending<T: FromCbor + Ord>(
	value: &Cbor,
	expected: &'static str,
) -> Result<BTreeSet<T>, cbor::Error> {
	let items: Vec<T> = Vec::from_cbor(value)?;
	if items.windows(2).any(|pair| pair[0] >= pair[1]) {
		return Err(cbor::Error::Expected(expected));
	}
	Ok(items.into_iter().collect())
}

/// A change's payload, sealed with the change's key: what [`Payload`] holds, encrypted.
/// The change's header holds its hash, so a sealed payload that does not belong to a
/// change is known at once. It is stored and travels beside its change, so that purging
/// can delete it while the change stays.
///
/// In CBOR, a sealed payload is an envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedPayload(Envelope);

impl SealedPayload {
	/// The hash of its encoding, which the header of its change holds.
	#[must_use]
	pub fn hash(&self) -> Hash {
		Hash::of(&cbor::to_vec(self))
	}

	/// How many bytes it holds, about what it takes on disk.
	#[must_use]
	pub fn byte_len(&self) -> usize {
		self.0.byte_len()
	}
}

impl ToCbor for SealedPayload {
	fn to_cbor(&self) -> Cbor {
		self.0.to_cbor()
	}
}

impl FromCbor for SealedPayload {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		Envelope::from_cbor(value).map(Self)
	}
}

/// A change's key, wrapped with a key derived from the space key of the change's epoch,
/// and tied to the change's ID. It is stored and travels beside its change, so that
/// purging can delete it while the change stays.
///
/// In CBOR, a wrapped change key is an envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedChangeKey(Envelope);

impl WrappedChangeKey {
	/// Wraps the key of a change with the space key of its epoch.
	fn wrap(
		space: SpaceId,
		change: ChangeId,
		change_key: &Key,
		space_key: &Key,
		rng: &mut impl CryptoRng,
	) -> Self {
		Self(Envelope::seal_key(
			&change_keys_key(space_key),
			change_key,
			&associated_data(space, &change),
			rng,
		))
	}

	/// Unwraps the key of `change`, if `keys` hold the space key of its epoch and the key
	/// is the one whose hash the change's header holds.
	pub(crate) fn unwrap(&self, change: &Change, keys: &SpaceKeys) -> Option<Key> {
		let space_key = keys.get(change.epoch())?;
		let key = self
			.0
			.open_key(
				&change_keys_key(space_key),
				&associated_data(change.space(), &change.id()),
			)
			.ok()?;
		(key_hash(&key) == change.header.key_hash).then_some(key)
	}
}

impl ToCbor for WrappedChangeKey {
	fn to_cbor(&self) -> Cbor {
		self.0.to_cbor()
	}
}

impl FromCbor for WrappedChangeKey {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		Envelope::from_cbor(value).map(Self)
	}
}

/// A change with its sealed payload and wrapped key, as far as a replica has them: the
/// three travel together, but may arrive apart, and the payload and key are deleted when
/// the change is purged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parcel {
	/// The change.
	pub change: Change,
	/// Its sealed payload.
	pub payload: Option<SealedPayload>,
	/// Its wrapped key: one, or a few on a replica that cannot tell yet which is good.
	pub keys: Vec<WrappedChangeKey>,
}

impl Parcel {
	/// The change's ID.
	#[must_use]
	pub const fn id(&self) -> ChangeId {
		self.change.id()
	}
}

/// What a new change is to hold, besides who makes it and with which key.
#[derive(Clone, Debug)]
pub struct Draft {
	/// The heads of the author's log.
	pub parents: BTreeSet<ChangeId>,
	/// The change's system time.
	pub timestamp: Timestamp,
	/// What the change is for.
	pub kind: Kind,
	/// What the change purges.
	pub purges: Purges,
	/// What the change restores, of what purges destroyed.
	pub restores: Restores,
	/// The hashes of the record keys of the statements sealed in the payload, by ID.
	pub record_keys: BTreeMap<StatementId, Hash>,
	/// The payload.
	pub payload: Payload,
}

impl Draft {
	/// An ordinary change that holds `payload` and purges nothing.
	#[must_use]
	pub fn new(
		parents: impl IntoIterator<Item = ChangeId>,
		timestamp: Timestamp,
		payload: Payload,
	) -> Self {
		Self {
			parents: parents.into_iter().collect(),
			timestamp,
			kind: Kind::Ordinary,
			purges: Purges::default(),
			restores: Restores::default(),
			record_keys: BTreeMap::new(),
			payload,
		}
	}
}

/// The operations that one replica made at one time, signed by the replica. The payload
/// that holds them is sealed with a key of the change's own, and kept beside the change,
/// whose header holds the hashes of both.
///
/// In CBOR, a change is the array `[1, header, signature]`. `docs/spec/log.md` describes
/// the format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
	header: Header,
	signature: Signature,
	id: ChangeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
	space: SpaceId,
	epoch: u64,
	author: VerifyingKey,
	/// Nearly always one or two: a small set.
	parents: SmallSet<ChangeId>,
	timestamp: Timestamp,
	kind: Kind,
	payload_hash: Hash,
	key_hash: Hash,
	/// The hashes of the record keys of the statements sealed in the payload, by ID.
	sealed: BTreeMap<StatementId, Hash>,
	purges: Purges,
	/// What it restores: in CBOR, an item after `purges` only if it restores anything.
	restores: Restores,
}

impl Change {
	/// Makes a change: seals the draft's payload with a new change key, wraps that key
	/// with the space's newest key, and signs the change as `author`.
	#[must_use]
	pub fn create(
		space: SpaceId,
		keys: &SpaceKeys,
		author: &SigningKey,
		draft: &Draft,
		rng: &mut impl CryptoRng,
	) -> Parcel {
		let (epoch, space_key) = keys.newest();
		let change_key = Key::generate(rng);
		let key_hash = key_hash(&change_key);
		let payload = SealedPayload(Envelope::seal(
			&change_key,
			&cbor::to_vec(&draft.payload),
			&associated_data(space, &key_hash),
			rng,
		));
		let header = Header {
			space,
			epoch,
			author: author.verifying_key(),
			parents: draft.parents.iter().copied().collect(),
			timestamp: draft.timestamp,
			kind: draft.kind,
			payload_hash: payload.hash(),
			key_hash,
			sealed: draft.record_keys.clone(),
			purges: draft.purges.clone(),
			restores: draft.restores.clone(),
		};
		let signature = author.sign(SIGNATURE_CONTEXT, &cbor::to_vec(&header));
		let change = Self::from_parts(header, signature);
		let key = WrappedChangeKey::wrap(space, change.id(), &change_key, space_key, rng);
		Parcel {
			change,
			payload: Some(payload),
			keys: vec![key],
		}
	}

	/// Puts a change together, with its ID: the hash of its encoding.
	fn from_parts(header: Header, signature: Signature) -> Self {
		let mut change = Self {
			header,
			signature,
			id: ChangeId::from_hash(Hash::of(&[])),
		};
		change.id = ChangeId::from_hash(Hash::of(&cbor::to_vec(&change)));
		change
	}

	/// The change's ID.
	#[must_use]
	pub const fn id(&self) -> ChangeId {
		self.id
	}

	/// The space the change belongs to.
	#[must_use]
	pub const fn space(&self) -> SpaceId {
		self.header.space
	}

	/// The number of the space key that wraps the change's key.
	#[must_use]
	pub const fn epoch(&self) -> u64 {
		self.header.epoch
	}

	/// The replica that made the change.
	#[must_use]
	pub const fn author(&self) -> &VerifyingKey {
		&self.header.author
	}

	/// The changes that the author's log ended with when the author made this one.
	#[must_use]
	pub const fn parents(&self) -> &SmallSet<ChangeId> {
		&self.header.parents
	}

	/// The change's system time.
	#[must_use]
	pub const fn timestamp(&self) -> Timestamp {
		self.header.timestamp
	}

	/// What the change is for.
	#[must_use]
	pub const fn kind(&self) -> Kind {
		self.header.kind
	}

	/// The hash of the change's sealed payload.
	#[must_use]
	pub const fn payload_hash(&self) -> &Hash {
		&self.header.payload_hash
	}

	/// The secret statements sealed in the change's payload, with the hashes of their record
	/// keys, by ID.
	#[must_use]
	pub const fn sealed(&self) -> &BTreeMap<StatementId, Hash> {
		&self.header.sealed
	}

	/// What the change purges.
	#[must_use]
	pub const fn purges(&self) -> &Purges {
		&self.header.purges
	}

	/// What the change restores, of what purges destroyed.
	#[must_use]
	pub const fn restores(&self) -> &Restores {
		&self.header.restores
	}

	/// Whether the change purges or restores anything, which the same roles may do.
	#[must_use]
	pub fn purges_or_restores(&self) -> bool {
		!self.header.purges.is_empty() || !self.header.restores.is_empty()
	}

	/// Checks the change's signature.
	///
	/// # Errors
	///
	/// Returns [`LogError::InvalidSignature`] if the signature is not valid for the author.
	pub fn verify(&self) -> Result<(), LogError> {
		self.header
			.author
			.verify(
				SIGNATURE_CONTEXT,
				&cbor::to_vec(&self.header),
				&self.signature,
			)
			.map_err(|_| LogError::InvalidSignature)
	}

	/// Decrypts the change's payload with the change's key, which `key` holds wrapped. Sealed
	/// statements stay sealed.
	///
	/// # Errors
	///
	/// Returns an error if the payload is not this change's, if `keys` lack the space key of
	/// the change's epoch, if the wrapped key is not this change's, or if the payload does
	/// not have the form of a payload, or seals other statements than the header says.
	pub fn open(
		&self,
		payload: &SealedPayload,
		key: &WrappedChangeKey,
		keys: &SpaceKeys,
	) -> Result<Payload, LogError> {
		if payload.hash() != self.header.payload_hash {
			return Err(LogError::OtherPayload);
		}
		self.open_checked(payload, key, keys)
	}

	/// Decrypts the change's payload, as [`Change::open`] does, for a payload already known
	/// to be the change's.
	pub(crate) fn open_checked(
		&self,
		payload: &SealedPayload,
		key: &WrappedChangeKey,
		keys: &SpaceKeys,
	) -> Result<Payload, LogError> {
		if keys.get(self.header.epoch).is_none() {
			return Err(LogError::UnknownEpoch(self.header.epoch));
		}
		let change_key = key.unwrap(self, keys).ok_or(LogError::OtherKey)?;
		let plaintext = payload
			.0
			.open(
				&change_key,
				&associated_data(self.header.space, &self.header.key_hash),
			)
			.map_err(|_| LogError::Decryption)?;
		let opened: Payload = cbor::from_slice(&plaintext).map_err(LogError::Operations)?;
		let mut sealed: Vec<StatementId> = opened.all_sealed().map(|(&id, _)| id).collect();
		sealed.sort_unstable();
		if !sealed.iter().eq(self.header.sealed.keys()) {
			return Err(LogError::Operations(cbor::Error::Expected(
				"the statements that the change's header says are sealed",
			)));
		}
		Ok(opened)
	}
}

/// The parents in their CBOR form: sorted by their bytes.
fn parents_cbor(parents: &SmallSet<ChangeId>) -> Cbor {
	let mut parents: Vec<Vec<u8>> = parents
		.iter()
		.map(|parent| parent.hash().to_multihash())
		.collect();
	parents.sort();
	Cbor::Array(parents.into_iter().map(Cbor::Bytes).collect())
}

impl ToCbor for Header {
	fn to_cbor(&self) -> Cbor {
		let mut items = vec![
			self.space.to_cbor(),
			self.epoch.to_cbor(),
			self.author.to_cbor(),
			parents_cbor(&self.parents),
			self.timestamp.to_cbor(),
			self.kind.to_cbor(),
			self.payload_hash.to_cbor(),
			self.key_hash.to_cbor(),
			write_by_statement(&self.sealed),
			self.purges.to_cbor(),
		];
		if !self.restores.is_empty() {
			items.push(self.restores.to_cbor());
		}
		Cbor::Array(items)
	}
}

impl FromCbor for Header {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a change header: [space, epoch, author, parents, timestamp, kind, payload hash, key hash, sealed, purges], and restores if it restores anything";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		// What it restores comes last, and only if it restores anything, so that a change
		// that does not has the form changes had before.
		let (items, restores) = match items.as_slice() {
			[header @ .., restores] if header.len() == 10 => {
				let restores = Restores::from_cbor(restores)?;
				if restores.is_empty() {
					return Err(cbor::Error::Expected(EXPECTED));
				}
				(header, restores)
			}
			items => (items, Restores::default()),
		};
		let [
			space,
			epoch,
			author,
			parents,
			timestamp,
			kind,
			payload_hash,
			key_hash,
			sealed,
			purges,
		] = items
		else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			epoch: u64::from_cbor(epoch)?,
			author: VerifyingKey::from_cbor(author)?,
			parents: read_parents(parents)?,
			timestamp: Timestamp::from_cbor(timestamp)?,
			kind: Kind::from_cbor(kind)?,
			payload_hash: Hash::from_cbor(payload_hash)?,
			key_hash: Hash::from_cbor(key_hash)?,
			sealed: read_by_statement(
				sealed,
				"sealed statements: [id, record key hash] in ascending order of their IDs",
			)?,
			purges: Purges::from_cbor(purges)?,
			restores,
		})
	}
}

/// Reads `[id, value]` pairs of statements, in ascending order of their IDs, without
/// duplicates: the sealed statements of a header or of a payload.
pub(crate) fn read_by_statement<T: FromCbor>(
	value: &Cbor,
	expected: &'static str,
) -> Result<BTreeMap<StatementId, T>, cbor::Error> {
	let Cbor::Array(items) = value else {
		return Err(cbor::Error::Expected(expected));
	};
	let mut read = BTreeMap::new();
	for item in items {
		let Cbor::Array(pair) = item else {
			return Err(cbor::Error::Expected(expected));
		};
		let [statement, value] = pair.as_slice() else {
			return Err(cbor::Error::Expected(expected));
		};
		let statement = StatementId::from_cbor(statement)?;
		if read
			.last_key_value()
			.is_some_and(|(&last, _)| last >= statement)
		{
			return Err(cbor::Error::Expected(expected));
		}
		read.insert(statement, T::from_cbor(value)?);
	}
	Ok(read)
}

/// Writes `[id, value]` pairs of statements, in ascending order of their IDs.
pub(crate) fn write_by_statement<T: ToCbor>(pairs: &BTreeMap<StatementId, T>) -> Cbor {
	Cbor::Array(
		pairs
			.iter()
			.map(|(statement, value)| Cbor::Array(vec![statement.to_cbor(), value.to_cbor()]))
			.collect(),
	)
}

/// Reads parents, which must be sorted by their bytes, without duplicates.
fn read_parents(value: &Cbor) -> Result<SmallSet<ChangeId>, cbor::Error> {
	const EXPECTED: &str = "parents: change IDs sorted by their bytes, without duplicates";
	let Cbor::Array(items) = value else {
		return Err(cbor::Error::Expected(EXPECTED));
	};
	let mut previous: Option<&[u8]> = None;
	let mut parents = SmallSet::new();
	for item in items {
		let Cbor::Bytes(bytes) = item else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		if previous.is_some_and(|previous| previous >= bytes.as_slice()) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		previous = Some(bytes);
		parents.insert(ChangeId::from_cbor(item)?);
	}
	Ok(parents)
}

impl ToCbor for Change {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			SUITE.to_cbor(),
			self.header.to_cbor(),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for Change {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a change: [1, header, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [suite, header, signature] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		if u8::from_cbor(suite) != Ok(SUITE) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		Ok(Self::from_parts(
			Header::from_cbor(header)?,
			Signature::from_cbor(signature)?,
		))
	}
}
