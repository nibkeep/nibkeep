//! What a change holds: ordinary operations, sealed secret statements, and what it states
//! again of the changes it purges.

use std::collections::BTreeMap;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Envelope, Key};
use nibkeep_model::{Operation, Statement};

use crate::ChangeId;
use crate::change::{read_by_statement, write_by_statement};
use crate::keys::associated_data;

/// The content of a change, which the change's header does not hold.
///
/// In CBOR, a payload is the array `[operations, sealed, restated]`, where `restated` is
/// an array of `[change, operations, sealed]`, in ascending order of the changes' IDs.
/// `docs/spec/log.md` describes the format.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Payload {
	/// The operations that everyone with the space key can read.
	pub operations: Vec<Operation>,
	/// Secret statements, by ID, each encrypted with its record key. What they supersede
	/// is retracted in `operations`, so that every replica applies it.
	pub sealed: BTreeMap<StatementId, Envelope>,
	/// What the change states again of earlier changes, by the change each came from: the
	/// rest of the changes it purges.
	pub restated: BTreeMap<ChangeId, Restated>,
}

/// What a purge states again of one earlier change: its operations and sealed statements
/// but those purged, with their statement IDs. They count from the time of the change they
/// came from, and show from the purge on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restated {
	/// The operations.
	pub operations: Vec<Operation>,
	/// The secret statements, sealed as they were.
	pub sealed: BTreeMap<StatementId, Envelope>,
}

impl Restated {
	/// Whether it states nothing again.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.operations.is_empty() && self.sealed.is_empty()
	}
}

impl Payload {
	/// Every operation of the payload, its own and those it states again.
	pub fn all_operations(&self) -> impl Iterator<Item = &Operation> {
		self.operations.iter().chain(
			self.restated
				.values()
				.flat_map(|restated| restated.operations.iter()),
		)
	}

	/// Every statement the payload seals, its own and those it states again.
	pub fn all_sealed(&self) -> impl Iterator<Item = (&StatementId, &Envelope)> {
		self.sealed.iter().chain(
			self.restated
				.values()
				.flat_map(|restated| restated.sealed.iter()),
		)
	}
}

impl From<Vec<Operation>> for Payload {
	fn from(operations: Vec<Operation>) -> Self {
		Self {
			operations,
			..Self::default()
		}
	}
}

/// Encrypts a secret statement with its record key.
pub(crate) fn seal(
	space: SpaceId,
	statement: &Statement,
	record_key: &Key,
	rng: &mut impl CryptoRng,
) -> Envelope {
	Envelope::seal(
		record_key,
		&cbor::to_vec(statement),
		&associated_data(space, &statement.id),
		rng,
	)
}

/// Decrypts a sealed statement with its record key. Returns `None` if it does not open,
/// or is not the statement with this ID.
pub(crate) fn open_sealed(
	space: SpaceId,
	id: StatementId,
	envelope: &Envelope,
	record_key: &Key,
) -> Option<Statement> {
	let plaintext = envelope
		.open(record_key, &associated_data(space, &id))
		.ok()?;
	let statement: Statement = cbor::from_slice(&plaintext).ok()?;
	(statement.id == id).then_some(statement)
}

impl ToCbor for Payload {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			Cbor::Array(self.operations.iter().map(ToCbor::to_cbor).collect()),
			write_by_statement(&self.sealed),
			Cbor::Array(
				self.restated
					.iter()
					.map(|(change, restated)| {
						Cbor::Array(vec![
							change.to_cbor(),
							Cbor::Array(restated.operations.iter().map(ToCbor::to_cbor).collect()),
							write_by_statement(&restated.sealed),
						])
					})
					.collect(),
			),
		])
	}
}

impl FromCbor for Payload {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a payload: [operations, sealed, restated], with sealed statements and restated changes in ascending order of their IDs";
		let error = || cbor::Error::Expected(EXPECTED);
		let Cbor::Array(items) = value else {
			return Err(error());
		};
		let [operations, sealed, restated] = items.as_slice() else {
			return Err(error());
		};
		let Cbor::Array(restated) = restated else {
			return Err(error());
		};
		let mut restated_by_change: BTreeMap<ChangeId, Restated> = BTreeMap::new();
		for entry in restated {
			let Cbor::Array(entry) = entry else {
				return Err(error());
			};
			let [change, operations, sealed] = entry.as_slice() else {
				return Err(error());
			};
			let change = ChangeId::from_cbor(change)?;
			// Change IDs of one algorithm sort by their digests, as their bytes do.
			if restated_by_change
				.last_key_value()
				.is_some_and(|(&last, _)| last >= change)
			{
				return Err(error());
			}
			restated_by_change.insert(
				change,
				Restated {
					operations: Vec::from_cbor(operations)?,
					sealed: read_by_statement(sealed, EXPECTED)?,
				},
			);
		}
		Ok(Self {
			operations: Vec::from_cbor(operations)?,
			sealed: read_by_statement(sealed, EXPECTED)?,
			restated: restated_by_change,
		})
	}
}
