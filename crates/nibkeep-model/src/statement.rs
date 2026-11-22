//! Statements, and the operations that change which of them are current.

use std::collections::BTreeSet;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, StatementId};

use crate::{FieldName, Qualifiers, Value};

/// One value of one field of an entity, with its qualifiers.
///
/// In CBOR, a statement is the array `[id, entity, field, value, qualifiers]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
	/// The statement's own ID, random and new for every statement.
	pub id: StatementId,
	/// The entity the statement is about.
	pub entity: EntityId,
	/// The field.
	pub field: FieldName,
	/// The value.
	pub value: Value,
	/// What else is known about the value.
	pub qualifiers: Qualifiers,
}

impl Statement {
	/// Returns the statement that `entity`'s `field` has `value`, without qualifiers.
	#[must_use]
	pub fn new(id: StatementId, entity: EntityId, field: FieldName, value: Value) -> Self {
		Self {
			id,
			entity,
			field,
			value,
			qualifiers: Qualifiers::new(),
		}
	}

	/// Returns the statement with these qualifiers.
	#[must_use]
	pub fn with_qualifiers(mut self, qualifiers: Qualifiers) -> Self {
		self.qualifiers = qualifiers;
		self
	}
}

impl ToCbor for Statement {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.id.to_cbor(),
			self.entity.to_cbor(),
			self.field.to_cbor(),
			self.value.to_cbor(),
			self.qualifiers.to_cbor(),
		])
	}
}

impl FromCbor for Statement {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a statement: [id, entity, field, value, qualifiers]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [id, entity, field, value, qualifiers] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			id: StatementId::from_cbor(id)?,
			entity: EntityId::from_cbor(entity)?,
			field: FieldName::from_cbor(field)?,
			value: Value::from_cbor(value)?,
			qualifiers: Qualifiers::from_cbor(qualifiers)?,
		})
	}
}

/// A change to the statements of a space, about one entity, which it names: it supersedes
/// only statements of that entity, so that what it changes can be told from it alone; or a
/// retraction of secret statements, whose entities are sealed with them, which supersedes
/// only secret statements.
///
/// In CBOR, an assertion is the array `[3, statement, superseded]`, a retraction is
/// `[2, entity, superseded]` and a retraction of secret statements `[4, superseded]`, where
/// `superseded` is an array of statement IDs in ascending order. Operations written before
/// operations named their entity supersede statements of any entity: an assertion
/// `[0, statement, superseded]`, and a retraction `[1, superseded]`, which names none.
/// Operations of kinds that this version of NibKeep does not know are kept as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
	/// Adds a statement, and supersedes the statements of its entity that the writer
	/// replaced.
	Assert {
		/// The new statement.
		statement: Statement,
		/// The statements it replaces, of its entity, of any field: one of another entity is
		/// not superseded, but by an assertion of the earlier form.
		supersedes: BTreeSet<StatementId>,
		/// Whether it supersedes statements of any entity, as an assertion of the earlier
		/// form, `[0, statement, superseded]`, does. Programs write only assertions that do
		/// not.
		any_entity: bool,
	},
	/// Supersedes statements of an entity without replacing them.
	Retract {
		/// The entity whose statements it removes: one of another entity is not superseded.
		/// `None` only in a retraction written before operations named their entity, which
		/// supersedes statements of any entity.
		entity: Option<EntityId>,
		/// The statements to remove.
		statements: BTreeSet<StatementId>,
	},
	/// Supersedes secret statements, of any entity, as their entities are sealed with them,
	/// without replacing them: an ordinary statement with one of their IDs, which only a
	/// faulty replica writes, is not superseded.
	RetractSecret {
		/// The secret statements to remove.
		statements: BTreeSet<StatementId>,
	},
	/// An operation of a kind that this version of NibKeep does not know. It is kept, and
	/// has no effect here.
	Unknown(UnknownOperation),
}

/// An operation of a kind that this version of NibKeep does not know, kept as it is. Only
/// decoding makes one, so its kind is never that of a known operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownOperation(Cbor);

impl UnknownOperation {
	/// The whole operation, in CBOR.
	#[must_use]
	pub const fn as_cbor(&self) -> &Cbor {
		&self.0
	}
}

impl Operation {
	/// Adds a statement without replacing any: another value of a field with many values,
	/// or a competing claim.
	#[must_use]
	pub fn assert(statement: Statement) -> Self {
		Self::replace(statement, [])
	}

	/// Adds a statement that replaces the statements in `supersedes`: setting a field
	/// replaces all of its statements that the writer can see.
	#[must_use]
	pub fn replace(
		statement: Statement,
		supersedes: impl IntoIterator<Item = StatementId>,
	) -> Self {
		Self::Assert {
			statement,
			supersedes: supersedes.into_iter().collect(),
			any_entity: false,
		}
	}

	/// Removes statements of an entity.
	#[must_use]
	pub fn retract(entity: EntityId, statements: impl IntoIterator<Item = StatementId>) -> Self {
		Self::Retract {
			entity: Some(entity),
			statements: statements.into_iter().collect(),
		}
	}

	/// Removes secret statements, whose entities are sealed with them.
	#[must_use]
	pub fn retract_secret(statements: impl IntoIterator<Item = StatementId>) -> Self {
		Self::RetractSecret {
			statements: statements.into_iter().collect(),
		}
	}

	/// The entity it changes, if it names one: an assertion's statement's, and a
	/// retraction's, but one of the earlier form, or of secret statements. An assertion of
	/// the earlier form may change others too, by what it supersedes.
	#[must_use]
	pub const fn entity(&self) -> Option<EntityId> {
		match self {
			Self::Assert { statement, .. } => Some(statement.entity),
			Self::Retract { entity, .. } => *entity,
			Self::RetractSecret { .. } | Self::Unknown(_) => None,
		}
	}

	/// Whether it supersedes statements of any entity, as operations of the earlier form do
	/// when they supersede any.
	#[must_use]
	pub fn supersedes_any_entity(&self) -> bool {
		match self {
			Self::Assert {
				supersedes,
				any_entity,
				..
			} => *any_entity && !supersedes.is_empty(),
			Self::Retract { entity, statements } => entity.is_none() && !statements.is_empty(),
			Self::RetractSecret { .. } | Self::Unknown(_) => false,
		}
	}

	/// The statements it supersedes: for a retraction of secret statements, only secret
	/// statements of these IDs.
	#[must_use]
	pub fn superseded(&self) -> Option<&BTreeSet<StatementId>> {
		match self {
			Self::Assert { supersedes, .. } => Some(supersedes),
			Self::Retract { statements, .. } | Self::RetractSecret { statements } => {
				Some(statements)
			}
			Self::Unknown(_) => None,
		}
	}
}

impl ToCbor for Operation {
	fn to_cbor(&self) -> Cbor {
		let ids =
			|ids: &BTreeSet<StatementId>| Cbor::Array(ids.iter().map(ToCbor::to_cbor).collect());
		match self {
			Self::Assert {
				statement,
				supersedes,
				any_entity,
			} => Cbor::Array(vec![
				if *any_entity { 0_u8 } else { 3_u8 }.to_cbor(),
				statement.to_cbor(),
				ids(supersedes),
			]),
			Self::Retract {
				entity: Some(entity),
				statements,
			} => Cbor::Array(vec![2_u8.to_cbor(), entity.to_cbor(), ids(statements)]),
			Self::Retract {
				entity: None,
				statements,
			} => Cbor::Array(vec![1_u8.to_cbor(), ids(statements)]),
			Self::RetractSecret { statements } => {
				Cbor::Array(vec![4_u8.to_cbor(), ids(statements)])
			}
			Self::Unknown(unknown) => unknown.0.clone(),
		}
	}
}

impl FromCbor for Operation {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "an operation: [3, statement, superseded], [2, entity, superseded], \
			 [4, superseded], [0, statement, superseded] or [1, superseded]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let kind = items
			.first()
			.and_then(|kind| u64::from_cbor(kind).ok())
			.ok_or(cbor::Error::Expected(EXPECTED))?;
		match (kind, items.as_slice()) {
			(kind @ (0 | 3), [_, statement, superseded]) => Ok(Self::Assert {
				statement: Statement::from_cbor(statement)?,
				supersedes: read_ids(superseded)?,
				any_entity: kind == 0,
			}),
			(1, [_, superseded]) => Ok(Self::Retract {
				entity: None,
				statements: read_ids(superseded)?,
			}),
			(2, [_, entity, superseded]) => Ok(Self::Retract {
				entity: Some(EntityId::from_cbor(entity)?),
				statements: read_ids(superseded)?,
			}),
			(4, [_, superseded]) => Ok(Self::RetractSecret {
				statements: read_ids(superseded)?,
			}),
			// New forms of known operations come with new kinds, so a known kind in
			// another form is an error.
			(0..=4, _) => Err(cbor::Error::Expected(EXPECTED)),
			_ => Ok(Self::Unknown(UnknownOperation(value.clone()))),
		}
	}
}

/// Reads an array of statement IDs in strictly ascending order.
fn read_ids(value: &Cbor) -> Result<BTreeSet<StatementId>, cbor::Error> {
	const EXPECTED: &str = "statement IDs in ascending order, without duplicates";
	let ids: Vec<StatementId> = Vec::from_cbor(value)?;
	if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
		return Err(cbor::Error::Expected(EXPECTED));
	}
	Ok(ids.into_iter().collect())
}
