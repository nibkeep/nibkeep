//! Merging entities that turn out to be the same, and undoing a merge (`docs/spec/model.md`,
//! section 7).

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, State, Statement, TypeName, Value, fields};

use super::Library;
use super::entities::{Listed, SHORTEST_PREFIX, defines_types};
use super::export::OWN_RECORDS;
use crate::NodeError;

impl Library {
	/// Merges an entity of a space into another that it turns out to be the same as: from
	/// then on, the entity it is merged into shows what both say, and references to either
	/// lead to it. The entities merged into `from` go with it. A merge of `from` into another
	/// entity is replaced. A merge is undone by [`Library::undo_merge`].
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the space no such entities; if
	/// they are one entity already, merged or not; if either is deleted, defines a type or
	/// a field, or is one of NibKeep's own records of a user, device, replica or space; or if
	/// the change cannot be saved.
	pub fn merge_entities(
		&mut self,
		space: SpaceId,
		from: EntityId,
		into: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let operations = {
			let state = self.space(space)?.stored.log().state();
			for entity in [from, into] {
				if !exists(state, entity)? {
					return Err(NodeError::NoEntity(entity.to_string()));
				}
				if defines_types(state, entity)? {
					return Err(NodeError::DefinesTypes(entity.to_string()));
				}
				let types = state.types(entity)?;
				if OWN_RECORDS.iter().any(|own| types.contains(own)) {
					return Err(NodeError::OwnRecord(entity.to_string()));
				}
				if state.is_deleted(entity)? {
					return Err(NodeError::Deleted(entity.to_string()));
				}
			}
			if state.canonical(from)? == state.canonical(into)? {
				return Err(NodeError::SameEntity(from.to_string()));
			}
			let replaced = own_merges(state, from)?;
			let statement = Statement::new(
				StatementId::random(rng),
				from,
				fields::MERGED_INTO,
				Value::Reference(into),
			);
			vec![Operation::replace(statement, replaced)]
		};
		self.commit(space, operations, wall_millis, rng)
	}

	/// Undoes the merge of an entity of a space into another: it shows what it says again,
	/// apart from the entity it was merged into, with the entities merged into it.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, if the entity was not merged into
	/// another, or if the change cannot be saved.
	pub fn undo_merge(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let merges = own_merges(self.space(space)?.stored.log().state(), entity)?;
		if merges.is_empty() {
			return Err(NodeError::NotMerged(entity.to_string()));
		}
		self.commit(
			space,
			vec![Operation::retract(entity, merges)],
			wall_millis,
			rng,
		)
	}

	/// The other entities merged with an entity of a space, by their own names, ordered by
	/// name without regard to case, unnamed ones last: those that a merge undone would set
	/// apart.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn merged(&self, space: SpaceId, entity: EntityId) -> Result<Vec<Listed>, NodeError> {
		merged(self.space(space)?.stored.log().state(), entity)
	}

	/// The other entities merged with an entity of a space, as the library had recorded them
	/// at the end of the millisecond `millis`, as [`Library::merged`] gives them.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn merged_as_of(
		&self,
		space: SpaceId,
		entity: EntityId,
		millis: u64,
	) -> Result<Vec<Listed>, NodeError> {
		let open = self.space(space)?;
		open.stored
			.reading(|stored| merged(&stored.log().state_as_of(millis), entity))
	}

	/// The types that an entity's own statements give it, apart from those of the entities
	/// merged with it: what it is, set apart from a merge.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn own_types(&self, space: SpaceId, entity: EntityId) -> Result<Vec<TypeName>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let mut types: Vec<TypeName> = state
			.storage()
			.current_of_field(entity, &fields::TYPE)?
			.iter()
			.filter_map(|recorded| recorded.statement.value.as_text()?.parse().ok())
			.collect();
		types.sort();
		types.dedup();
		Ok(types)
	}

	/// One of the other entities merged with an entity of a space, found by its ID, a
	/// prefix of its ID of at least six characters, or one of its own names, in any form,
	/// without regard to case: what a merge undone would set apart, or merged again move.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the space cannot be read, or no
	/// entity merged with it, or several, match.
	pub fn find_merged(
		&self,
		space: SpaceId,
		entity: EntityId,
		reference: &str,
	) -> Result<EntityId, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let canonical = state.canonical(entity)?;
		let lowered = reference.to_lowercase();
		let looks_like_id = lowered.len() >= SHORTEST_PREFIX
			&& lowered
				.chars()
				.all(|character| character.is_ascii_hexdigit() || character == '-');
		let mut matching = Vec::new();
		for member in state.merged(canonical)? {
			if member == canonical {
				continue;
			}
			let by_id = looks_like_id && member.to_string().starts_with(&lowered);
			if by_id || own_names(state, member)?.contains(&lowered) {
				matching.push(member);
			}
		}
		match matching.as_slice() {
			[] => Err(NodeError::NotMerged(reference.to_owned())),
			&[member] => Ok(member),
			_ => Err(NodeError::Ambiguous {
				reference: reference.to_owned(),
				candidates: matching,
			}),
		}
	}
}

/// The other entities merged with an entity of a state, as [`Library::merged`] gives them.
fn merged<S: Statements>(state: &State<S>, entity: EntityId) -> Result<Vec<Listed>, NodeError>
where
	NodeError: From<S::Error>,
{
	let canonical = state.canonical(entity)?;
	let mut merged = Vec::new();
	for member in state.merged(canonical)? {
		if member == canonical {
			continue;
		}
		merged.push(Listed {
			id: member,
			name: own_name(state, member)?,
			types: Vec::new(),
			via: None,
		});
	}
	merged.sort_by_cached_key(|listed| {
		let name = listed.name.as_deref().map(str::to_lowercase);
		(name.is_none(), name, listed.id)
	});
	Ok(merged)
}

/// Whether the space says anything of an entity, or of one merged with it.
fn exists<S: Statements>(state: &State<S>, entity: EntityId) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(!state.fields(state.canonical(entity)?)?.is_empty())
}

/// The current statements that merge an entity itself into another.
fn own_merges<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Vec<StatementId>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.storage()
		.current_of_field(entity, &fields::MERGED_INTO)?
		.iter()
		.map(|recorded| recorded.statement.id)
		.collect())
}

/// The name that an entity's own statements give it, apart from those merged with it: the
/// first of [`own_names_in`].
fn own_name<S: Statements>(state: &State<S>, entity: EntityId) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(own_names_in(state, entity)?.into_iter().next())
}

/// The names that an entity's own statements give it, in every form, lowercased.
fn own_names<S: Statements>(state: &State<S>, entity: EntityId) -> Result<Vec<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(own_names_in(state, entity)?
		.iter()
		.map(|name| name.to_lowercase())
		.collect())
}

/// The names that an entity's own statements give it, apart from those merged with it, in
/// every form: those of the first field that names it, `org.nibkeep.name`, or else the first
/// by name that ends in `.name`, or else in `.title`, each written in full, or text.
pub(crate) fn own_names_in<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Vec<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	let held: Vec<FieldName> = storage
		.of_entity(entity)?
		.into_iter()
		.map(|(field, _)| field)
		.collect();
	let mut naming: Vec<&FieldName> = held
		.iter()
		.filter(|field| **field == fields::NAME)
		.collect();
	for suffix in fields::NAMING_SUFFIXES {
		naming.extend(
			held.iter()
				.filter(|field| fields::has_suffix(field, suffix)),
		);
	}
	for field in naming {
		let names: Vec<String> = storage
			.current_of_field(entity, field)?
			.iter()
			.filter_map(|recorded| recorded.statement.value.as_name_text().map(str::to_owned))
			.collect();
		if !names.is_empty() {
			return Ok(names);
		}
	}
	Ok(Vec::new())
}
