//! Deleting a type that a space defines, and restoring it: its definitions are deleted, as
//! any entity is, hidden and kept, so that the space's types leave it out, with its fields
//! where no other type lists them, until it is restored whole
//! ([data model](../../../../docs/spec/model.md#8-types)).

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::schema::TYPE_DEFINITION;
use nibkeep_model::storage::Statements;
use nibkeep_model::{Operation, State, Statement, TypeName, Value, fields};

use super::{Library, entities};
use crate::NodeError;

impl Library {
	/// Deletes a type that a space defines: each of its definitions that is not deleted,
	/// in one change. The space's types then leave it out, and the fields that only it
	/// lists; [`Library::restore_type`] brings it back with them. Only a type of the space's
	/// own is deleted, not one of the standard vocabulary, which comes back as it is
	/// installed, nor one of NibKeep's, which is defined in code; and only once no entity
	/// that is not deleted has it, so that no entity is left with a type that no
	/// definition makes.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotOwnType`] for a standard type or one defined in code;
	/// [`NodeError::NoTypeDefined`] if the space has no definition of it that is not
	/// deleted; [`NodeError::TypeInUse`] if entities still have it;
	/// [`NodeError::AiAgentMayNot`] if this replica is an AI agent's; and another error if
	/// the library has no such space, or the change cannot be saved.
	pub fn delete_type(
		&mut self,
		space: SpaceId,
		kind: &TypeName,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("delete a type")?;
		self.check_own_type(kind)?;
		let operations = {
			let open = self.space(space)?;
			let state = open.stored.log().state();
			let definitions = self.definitions(space, kind, false)?;
			if definitions.is_empty() {
				return Err(NodeError::NoTypeDefined(kind.clone()));
			}
			let mut having = BTreeSet::new();
			for entity in entities::with_type(&open.stored, kind)? {
				let entity = state.canonical(entity)?;
				if !state.is_deleted(entity)? {
					having.insert(entity);
				}
			}
			if !having.is_empty() {
				return Err(NodeError::TypeInUse {
					kind: kind.clone(),
					entities: having.len(),
				});
			}
			definitions
				.into_iter()
				.map(|definition| {
					let statement = Statement::new(
						StatementId::random(rng),
						definition,
						fields::DELETED,
						Value::Boolean(true),
					);
					Operation::replace(statement, Vec::new())
				})
				.collect::<Vec<_>>()
		};
		self.commit(space, operations, wall_millis, rng)
	}

	/// The types of a space's own that it deleted: those whose definitions are all deleted,
	/// which [`Library::restore_type`] brings back, ordered by name.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn deleted_types(&self, space: SpaceId) -> Result<Vec<TypeName>, NodeError> {
		let types = &self.space(space)?.types;
		let deleted: BTreeSet<TypeName> = deleted_definitions(self, space)?
			.into_values()
			.filter(|kind| types.get(kind).is_none())
			.collect();
		Ok(deleted.into_iter().collect())
	}

	/// Restores a type that a space deleted: each of its deleted definitions, in one change,
	/// so that it comes back with the fields it had.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotOwnType`] for a standard type or one defined in code;
	/// [`NodeError::NoTypeDefined`] if the space has no deleted definition of it;
	/// [`NodeError::AiAgentMayNot`] if this replica is an AI agent's; and another error if
	/// the library has no such space, or the change cannot be saved.
	pub fn restore_type(
		&mut self,
		space: SpaceId,
		kind: &TypeName,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("restore a type")?;
		self.check_own_type(kind)?;
		let operations = {
			let state = self.space(space)?.stored.log().state();
			let mut operations = Vec::new();
			for definition in self.definitions(space, kind, true)? {
				let deletions: Vec<StatementId> = state
					.current(definition, &fields::DELETED)?
					.iter()
					.map(|statement| statement.id)
					.collect();
				if !deletions.is_empty() {
					operations.push(Operation::retract(definition, deletions));
				}
			}
			operations
		};
		if operations.is_empty() {
			return Err(NodeError::NoTypeDefined(kind.clone()));
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Refuses a type that is not the space's own to delete or restore: a standard one, or
	/// one that NibKeep or an app defines in code.
	fn check_own_type(&self, kind: &TypeName) -> Result<(), NodeError> {
		if kind.as_str().starts_with("std.")
			|| kind.is_nibkeep()
			|| self.code_types.get(kind).is_some()
		{
			return Err(NodeError::NotOwnType(kind.clone()));
		}
		Ok(())
	}

	/// The definitions of a type in a space, those deleted if `deleted`, else the others.
	fn definitions(
		&self,
		space: SpaceId,
		kind: &TypeName,
		deleted: bool,
	) -> Result<Vec<EntityId>, NodeError> {
		if deleted {
			return Ok(deleted_definitions(self, space)?
				.into_iter()
				.filter(|(_, named)| named == kind)
				.map(|(definition, _)| definition)
				.collect());
		}
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut found = BTreeSet::new();
		for entity in entities::with_type(&open.stored, &TYPE_DEFINITION)? {
			let entity = state.canonical(entity)?;
			if !state.is_deleted(entity)? && type_named_in(state, entity)?.as_ref() == Some(kind) {
				found.insert(entity);
			}
		}
		Ok(found.into_iter().collect())
	}
}

/// The deleted entities of a space that define types, with the types they name. The index
/// of values leaves deleted entities out, so the space's entities are read, in one
/// transaction, as the deleted entities are listed.
fn deleted_definitions(
	library: &Library,
	space: SpaceId,
) -> Result<BTreeMap<EntityId, TypeName>, NodeError> {
	library.space(space)?.stored.reading(|stored| {
		let state = stored.log().state();
		let mut found = BTreeMap::new();
		for entity in state.storage().entities()? {
			let entity = state.canonical(entity)?;
			if !found.contains_key(&entity)
				&& state.is_deleted(entity)?
				&& state.types(entity)?.contains(&TYPE_DEFINITION)
				&& let Some(kind) = type_named_in(state, entity)?
			{
				found.insert(entity, kind);
			}
		}
		Ok(found)
	})
}

/// The type that a definition names, if this version can read its name.
fn type_named_in<S: Statements>(
	state: &State<S>,
	definition: EntityId,
) -> Result<Option<TypeName>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(definition, &fields::TYPE_NAME)?
		.iter()
		.find_map(|statement| statement.value.as_text()?.parse().ok()))
}
