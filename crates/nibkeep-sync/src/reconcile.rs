//! What a space holds, as items of negentropy's set, each with a timestamp and an ID of 32
//! bytes ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#3-syncing)).

use std::collections::BTreeMap;

use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nibkeep_base::cbor::{self, ToCbor, Value};
use nibkeep_base::{Hash, StatementId};
use nibkeep_log::{ChangeId, WrappedBlobKey, WrappedKey};

use crate::SyncError;

/// How long a message of negentropy may be: messages that would be longer are split, at the
/// cost of more round trips.
const FRAME_LIMIT: u64 = 4 * 1024 * 1024;

/// A thing that a space holds, as a replica tells it to reconciliation. Sealed blobs are not
/// among them: each side asks for those it keeps ([`crate::Holder::wanted_blobs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holding {
	/// An accepted change.
	Change {
		/// The change.
		id: ChangeId,
		/// The milliseconds of its timestamp.
		millis: u64,
		/// Its depth: deeper than its parents, so changes sent in the order of their depths
		/// come after their parents.
		depth: u64,
		/// Whether the replica holds its sealed payload and a wrapped key.
		complete: bool,
	},
	/// A wrapped record key of a secret statement.
	RecordKey {
		/// The statement.
		statement: StatementId,
		/// The wrapped key.
		key: WrappedKey,
	},
	/// A wrapped blob key.
	BlobKey {
		/// The sealed hash of its blob.
		sealed: Hash,
		/// The wrapped key.
		key: WrappedBlobKey,
	},
	/// A page of a replica's account of what it holds in the space, the latest that the
	/// replica keeps of that page.
	Account {
		/// The hash of its encoding.
		id: Hash,
		/// When it was issued, in milliseconds since 1970.
		issued: u64,
	},
}

impl Holding {
	/// The item's timestamp: a change's milliseconds, a page's time of issue, and 0 for the
	/// rest.
	const fn timestamp(&self) -> u64 {
		match self {
			Self::Change { millis, .. } => *millis,
			Self::Account { issued, .. } => *issued,
			_ => 0,
		}
	}

	/// In which order items are sent: changes by depth, then wrapped record keys, wrapped
	/// blob keys, and pages of accounts.
	fn order(&self) -> (u8, u64) {
		match self {
			Self::Change { depth, .. } => (0, *depth),
			Self::RecordKey { .. } => (1, 0),
			Self::BlobKey { .. } => (2, 0),
			Self::Account { .. } => (4, 0),
		}
	}
}

/// The ID of an item: the BLAKE3 hash of the canonical encoding of what it is.
#[must_use]
pub fn item_id(holding: &Holding) -> [u8; 32] {
	let described = match holding {
		Holding::Change { id, complete, .. } => {
			Value::Array(vec![0_u8.to_cbor(), id.to_cbor(), Value::Bool(*complete)])
		}
		Holding::RecordKey { statement, key } => {
			Value::Array(vec![1_u8.to_cbor(), statement.to_cbor(), key.to_cbor()])
		}
		Holding::BlobKey { sealed, key } => {
			Value::Array(vec![2_u8.to_cbor(), sealed.to_cbor(), key.to_cbor()])
		}
		Holding::Account { id, .. } => Value::Array(vec![4_u8.to_cbor(), id.to_cbor()]),
	};
	let digest = Hash::of(&cbor::encode(&described));
	digest
		.digest()
		.try_into()
		.unwrap_or_else(|_| unreachable!("BLAKE3 digests are 32 bytes"))
}

/// What a space holds, by item ID, and negentropy over it.
pub(crate) struct Reconciling {
	negentropy: Negentropy<'static, NegentropyStorageVector>,
	held: BTreeMap<[u8; 32], Holding>,
}

impl Reconciling {
	/// Puts everything a space holds into negentropy's set.
	pub(crate) fn new(holdings: Vec<Holding>) -> Result<Self, SyncError> {
		let mut held = BTreeMap::new();
		for holding in holdings {
			held.entry(item_id(&holding)).or_insert(holding);
		}
		let mut storage = NegentropyStorageVector::with_capacity(held.len());
		for (id, holding) in &held {
			storage
				.insert(holding.timestamp(), Id::from_byte_array(*id))
				.map_err(negentropy_error)?;
		}
		storage.seal().map_err(negentropy_error)?;
		let negentropy = Negentropy::owned(storage, FRAME_LIMIT).map_err(negentropy_error)?;
		Ok(Self { negentropy, held })
	}

	/// The initiator's first message.
	pub(crate) fn initiate(&mut self) -> Result<Vec<u8>, SyncError> {
		self.negentropy.initiate().map_err(negentropy_error)
	}

	/// The initiator's answer to a message, adding the IDs that only this side holds to
	/// `have` and those only the other side holds to `need`; `None` once nothing differs
	/// any more.
	pub(crate) fn answer(
		&mut self,
		message: &[u8],
		have: &mut Vec<[u8; 32]>,
		need: &mut Vec<[u8; 32]>,
	) -> Result<Option<Vec<u8>>, SyncError> {
		let (mut haves, mut needs) = (Vec::new(), Vec::new());
		let answer = self
			.negentropy
			.reconcile_with_ids(message, &mut haves, &mut needs)
			.map_err(negentropy_error)?;
		have.extend(haves.iter().map(|id| **id));
		need.extend(needs.iter().map(|id| **id));
		Ok(answer)
	}

	/// The responder's answer to a message.
	pub(crate) fn respond(&mut self, message: &[u8]) -> Result<Vec<u8>, SyncError> {
		self.negentropy.reconcile(message).map_err(negentropy_error)
	}

	/// What this side holds of these IDs, in the order they are to be sent; IDs it does not
	/// hold are left out.
	pub(crate) fn in_order(&self, ids: &[[u8; 32]]) -> Vec<Holding> {
		let mut holdings: Vec<(&[u8; 32], &Holding)> = ids
			.iter()
			.filter_map(|id| self.held.get_key_value(id))
			.collect();
		holdings.sort_by(|(a_id, a), (b_id, b)| a.order().cmp(&b.order()).then(a_id.cmp(b_id)));
		holdings.dedup_by(|(a, _), (b, _)| a == b);
		holdings
			.into_iter()
			.map(|(_, holding)| holding.clone())
			.collect()
	}
}

#[expect(clippy::needless_pass_by_value, reason = "for map_err")]
fn negentropy_error(error: negentropy::Error) -> SyncError {
	SyncError::protocol(format!("negentropy: {error}"))
}
