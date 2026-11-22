//! The types of a space: those that NibKeep and apps define in code, combined with those
//! that the space defines as entities. `docs/spec/model.md` describes the format.

use std::collections::{BTreeMap, BTreeSet};

use std::convert::Infallible;

use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, StatementId};
use rand_core::CryptoRng;

use crate::storage::Statements;
use crate::{
	FieldDef, FieldName, Labels, Multiplicity, Operation, Qualifiers, Quantity, State, Statement,
	Tag, TypeDef, TypeName, Value, ValueKind, fields,
};

/// The type of the entities that define types.
pub const TYPE_DEFINITION: TypeName = TypeName::from_static("org.nibkeep.type-definition");

/// The type of the entities that define the fields of types.
pub const FIELD_DEFINITION: TypeName = TypeName::from_static("org.nibkeep.field-definition");

/// Why a definition cannot be added to a space's types as it is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DefinitionError {
	/// The type is one of the two that NibKeep defines for defining types.
	#[error("{0} is defined by NibKeep, and no space can define it")]
	OwnType(TypeName),
	/// The field is one that NibKeep reserves for its own structure.
	#[error("{0} is one of NibKeep's own fields, which no type can define")]
	ReservedField(FieldName),
	/// The field is one that NibKeep defines, and a type lists it otherwise.
	#[error("{0} is defined by NibKeep, and a type can list it only as NibKeep defines it")]
	NibKeepField(FieldName),
	/// The field has values of another kind already, and a field's kind never changes.
	#[error("{field} has values of another kind already, and a field's kind never changes")]
	OtherKind {
		/// The field.
		field: FieldName,
		/// The kind it has.
		kind: ValueKind,
	},
	/// A field that is no reference stands for what it refers to, is searched through, or
	/// has a reverse name, which only links can.
	#[error(
		"{0} is no reference, so it cannot stand for what it refers to, be searched through, or have a reverse name"
	)]
	NotALink(FieldName),
	/// A field that is not text holds codes, which only text can.
	#[error("{0} is not text, so its values cannot be codes")]
	NotText(FieldName),
	/// The field goes by another reverse name already, and the first that names one counts.
	#[error("{field} goes by {known} from its other end already")]
	OtherReverse {
		/// The field.
		field: FieldName,
		/// The reverse name it has.
		known: FieldName,
	},
	/// The field is defined more carefully already: it shows more values, or is secret, or
	/// is a role, and a definition can only add care.
	#[error("{field} is defined more carefully already, and a definition can only add care")]
	LessCareful {
		/// The field.
		field: FieldName,
		/// How it is defined.
		known: Box<FieldDef>,
	},
}

/// The types of a space: those that NibKeep and apps define in code, combined with those
/// that the space defines.
///
/// All definitions of a type combine into the union of their fields, and definitions of a
/// field combine as [`FieldDef::combine`] does. Definitions in code come first, so their
/// kinds count; the space's definitions combine in the order of their entities' IDs. The
/// space cannot change the fields that NibKeep reserves for its own structure
/// ([`fields::is_reserved`]), nor define the two types that define types.
///
/// A `Types` reflects the state it read. After the state changes, build a new one.
#[derive(Clone, Debug)]
pub struct Types {
	/// The types that NibKeep and apps define in code.
	code: BTreeMap<TypeName, TypeDef>,
	/// The types that the space defines.
	space: BTreeMap<TypeName, TypeDef>,
	/// Both combined, those in code first.
	combined: BTreeMap<TypeName, TypeDef>,
	/// For each type that the space defines, the entities that define it.
	entities: BTreeMap<TypeName, BTreeSet<EntityId>>,
	/// For each type that the space defines, the first entity that defines each of its
	/// fields, by ID, to which labels in more languages are added.
	field_entities: BTreeMap<TypeName, FieldEntities>,
	/// The fields that statements governing the space make secret for good.
	marked: BTreeSet<FieldName>,
}

impl Default for Types {
	fn default() -> Self {
		Self::new()
	}
}

impl Types {
	/// The types that NibKeep defines for defining types, and no others yet.
	#[must_use]
	pub fn new() -> Self {
		let mut types = Self {
			code: BTreeMap::new(),
			space: BTreeMap::new(),
			combined: BTreeMap::new(),
			entities: BTreeMap::new(),
			field_entities: BTreeMap::new(),
			marked: BTreeSet::new(),
		};
		types.add(type_definition());
		types.add(field_definition());
		types
	}

	/// Adds a type that NibKeep or an app defines in code.
	pub fn add(&mut self, definition: TypeDef) {
		let name = definition.name().clone();
		let combined = match self.code.remove(&name) {
			Some(known) => known.combine(&definition),
			None => definition,
		};
		self.code.insert(name.clone(), combined);
		self.combine(&name);
	}

	/// Adds the types that a space defines, looking through all of its current statements.
	///
	/// # Errors
	///
	/// Returns an error if the state's storage cannot be read.
	pub fn read<S: Statements>(&mut self, state: &State<S>) -> Result<(), S::Error> {
		let mut entities = BTreeSet::new();
		for statement in state.statements()? {
			if statement.field == fields::TYPE
				&& statement.value.as_text() == Some(TYPE_DEFINITION.as_str())
			{
				entities.insert(state.canonical(statement.entity)?);
			}
		}
		self.read_entities(state, entities)
	}

	/// Adds the types that some entities of a space define, such as the entities of the
	/// type definition type that an index found. Other entities are passed over.
	///
	/// # Errors
	///
	/// Returns an error if the state's storage cannot be read.
	pub fn read_entities<S: Statements>(
		&mut self,
		state: &State<S>,
		entities: impl IntoIterator<Item = EntityId>,
	) -> Result<(), S::Error> {
		let entities: BTreeSet<EntityId> = entities.into_iter().collect();
		for entity in entities {
			let Some((definition, field_entities)) = read_type(state, entity)? else {
				continue;
			};
			let name = definition.name().clone();
			let known = self.field_entities.entry(name.clone()).or_default();
			for (field, field_entity) in field_entities {
				let first = known.entry(field).or_insert(field_entity);
				*first = (*first).min(field_entity);
			}
			let combined = match self.space.remove(&name) {
				Some(known) => known.combine(&definition),
				None => definition,
			};
			self.space.insert(name.clone(), combined);
			self.entities
				.entry(name.clone())
				.or_default()
				.insert(entity);
			self.combine(&name);
		}
		Ok(())
	}

	/// The type with this name.
	#[must_use]
	pub fn get(&self, name: &TypeName) -> Option<&TypeDef> {
		self.combined.get(name)
	}

	/// The types, ordered by name.
	pub fn iter(&self) -> impl Iterator<Item = &TypeDef> {
		self.combined.values()
	}

	/// The types that the space defines, ordered by name, as it defines them: without the
	/// fields that the code gives types of the same name.
	pub fn defined_by_space(&self) -> impl Iterator<Item = &TypeDef> {
		self.space.values()
	}

	/// Whether the space defines the type with entities of its own.
	#[must_use]
	pub fn defines_in_space(&self, name: &TypeName) -> bool {
		self.space.contains_key(name)
	}

	/// The definition of a field, combined from every type that lists it.
	#[must_use]
	pub fn field(&self, name: &FieldName) -> Option<FieldDef> {
		// A field that NibKeep defines is defined by NibKeep alone, so that no app or space
		// changes how, for example, every entity's name is shown.
		self.nibkeep_field(name).or_else(|| {
			FieldDef::combine(
				self.combined
					.values()
					.filter_map(|definition| definition.field(name)),
			)
		})
	}

	/// How NibKeep itself defines a field, in its own types in code, if it does.
	fn nibkeep_field(&self, name: &FieldName) -> Option<FieldDef> {
		if !name.is_nibkeep() {
			return None;
		}
		FieldDef::combine(
			self.code
				.iter()
				.filter(|(type_name, _)| type_name.is_nibkeep())
				.filter_map(|(_, definition)| definition.field(name)),
		)
	}

	/// The fields of every type for which a definition says yes, ordered by name.
	fn fields_where(&self, test: impl Fn(&FieldDef) -> bool) -> BTreeSet<FieldName> {
		let names: BTreeSet<&FieldName> = self
			.combined
			.values()
			.flat_map(TypeDef::fields)
			.map(|(name, _)| name)
			.collect();
		names
			.into_iter()
			.filter(|name| self.field(name).as_ref().is_some_and(&test))
			.cloned()
			.collect()
	}

	/// The fields that show only the value written last, as the types combined define
	/// them.
	#[must_use]
	pub fn last_writer_wins_fields(&self) -> BTreeSet<FieldName> {
		self.fields_where(|definition| definition.multiplicity() == Multiplicity::LastWriterWins)
	}

	/// The fields that a search goes through: those that stand for what they refer to, or
	/// are searched through, as [`Types::field`] defines them.
	#[must_use]
	pub fn searched_fields(&self) -> BTreeSet<FieldName> {
		self.fields_where(|field| field.is_stands_for() || field.is_search_through())
	}

	/// Makes fields secret for good, as statements that govern the space say, whatever the
	/// space's definitions say now: all but those that NibKeep reserves or defines itself.
	pub fn mark_secret(&mut self, marked: impl IntoIterator<Item = FieldName>) {
		self.marked.extend(marked);
	}

	/// The fields whose values are secret: as [`Types::field`] defines them, and those that
	/// [`Types::mark_secret`] made secret for good.
	#[must_use]
	pub fn secret_fields(&self) -> BTreeSet<FieldName> {
		let mut secret = self.fields_where(FieldDef::is_secret);
		secret.extend(
			self.marked
				.iter()
				.filter(|name| self.nibkeep_field(name).is_none())
				.cloned(),
		);
		secret.retain(|name| !fields::is_reserved(name));
		secret
	}

	/// Checks that a definition can be added to the types as it is: that it does not define
	/// a type or field that NibKeep reserves, nor change a field's kind or reverse name, nor
	/// define a field less carefully than the types do, which would have no effect, nor
	/// give a field that is no reference what only links have, nor make codes of what is
	/// not text.
	///
	/// # Errors
	///
	/// Returns the first problem found.
	pub fn check(&self, definition: &TypeDef) -> Result<(), DefinitionError> {
		let name = definition.name();
		if *name == TYPE_DEFINITION || *name == FIELD_DEFINITION {
			return Err(DefinitionError::OwnType(name.clone()));
		}
		for (field, requested) in definition.fields() {
			if fields::is_reserved(field) {
				return Err(DefinitionError::ReservedField(field.clone()));
			}
			if requested.kind() != ValueKind::Reference
				&& (requested.is_stands_for()
					|| requested.is_search_through()
					|| requested.reverse().is_some())
			{
				return Err(DefinitionError::NotALink(field.clone()));
			}
			if requested.is_code() && requested.kind() != ValueKind::Text {
				return Err(DefinitionError::NotText(field.clone()));
			}
			if let Some(nibkeep) = self.nibkeep_field(field)
				&& (nibkeep.kind() != requested.kind() || !nibkeep.same_care(requested))
			{
				return Err(DefinitionError::NibKeepField(field.clone()));
			}
			let Some(known) = self.field(field) else {
				continue;
			};
			if known.kind() != requested.kind() {
				return Err(DefinitionError::OtherKind {
					field: field.clone(),
					kind: known.kind(),
				});
			}
			if let (Some(known), Some(reverse)) = (known.reverse(), requested.reverse())
				&& known != reverse
			{
				return Err(DefinitionError::OtherReverse {
					field: field.clone(),
					known: known.clone(),
				});
			}
			// A unit only says how values are shown, so it is no matter of care.
			if !FieldDef::combine([&known, requested])
				.is_some_and(|combined| combined.same_care(requested))
			{
				return Err(DefinitionError::LessCareful {
					field: field.clone(),
					known: Box::new(known),
				});
			}
		}
		Ok(())
	}

	/// The definition, with each field that the space's types define already of the same
	/// kind combined with their definition, theirs first: as careful as they are, with their
	/// unit and reverse name, and the labels that the type has already before its own. Defining it then adds only
	/// what the space lacks, such as labels in more languages, and is never refused for
	/// taking less care. A field of another kind is left as it is, for [`Types::check`] to
	/// refuse.
	#[must_use]
	pub fn careful(&self, definition: &TypeDef) -> TypeDef {
		let own = self.get(definition.name());
		let mut careful = definition.clone();
		for (name, field) in definition.fields() {
			if let Some(known) = self.field(name)
				&& known.kind() == field.kind()
				&& let Some(mut combined) = FieldDef::combine([&known, field])
			{
				// Labels only from the type itself: another type's labels of a shared field
				// are not this type's.
				let mut labels = own
					.and_then(|own| own.field(name))
					.map_or_default(|own| own.labels().clone());
				labels.add_missing(field.labels());
				combined.set_labels(labels);
				careful = careful.with_field(name.clone(), combined);
			}
		}
		careful
	}

	/// A definition of a type with these fields, to grow it: at the next version if any of
	/// the fields is new to the type, at its version if none is, and at version 1 for a new
	/// type.
	pub fn extend(
		&self,
		name: TypeName,
		fields: impl IntoIterator<Item = (FieldName, FieldDef)>,
	) -> TypeDef {
		let fields: Vec<(FieldName, FieldDef)> = fields.into_iter().collect();
		let version = match self.combined.get(&name) {
			None => 1,
			Some(known) => {
				let grows = fields.iter().any(|(field, _)| known.field(field).is_none());
				known.version() + u64::from(grows)
			}
		};
		fields.into_iter().fold(
			TypeDef::new(name, version),
			|definition, (field, field_definition)| definition.with_field(field, field_definition),
		)
	}

	/// The operations that define a type in a space that holds nothing yet, as a new one:
	/// [`Types::define_in`], with every ID derived from what it states.
	pub fn define(&self, definition: &TypeDef, rng: &mut impl CryptoRng) -> Vec<Operation> {
		let Ok(operations) =
			self.define_with(definition, &mut |_| Ok::<bool, Infallible>(false), rng);
		operations
	}

	/// The operations that define a type in the space whose state this is, or add to it what
	/// the space's types lack: an entity for the type, unless the space has one, an entity
	/// for each field that the types lack or define less carefully, and a newer version. None
	/// if they lack nothing.
	///
	/// Their IDs are derived from what they state, the same in every space, unless the state
	/// knows one already, as after a person deleted part of a definition, and then random
	/// (`docs/spec/model.md`, section 8). Fields that NibKeep reserves, and kinds,
	/// multiplicities and versions too large for an integer value, are left out.
	///
	/// # Errors
	///
	/// Returns an error if the state's storage cannot be read.
	pub fn define_in<S: Statements>(
		&self,
		definition: &TypeDef,
		state: &State<S>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, S::Error> {
		let storage = state.storage();
		let mut known = |id: Derived| match id {
			Derived::Entity(entity) => Ok(!storage.of_entity(entity)?.is_empty()),
			Derived::Statement(id) => Ok(storage.contains(id)? || storage.is_superseded(id)?),
		};
		self.define_with(definition, &mut known, rng)
	}

	/// The operations of [`Types::define_in`], with the IDs that `known` says the space knows
	/// drawn at random.
	fn define_with<E>(
		&self,
		definition: &TypeDef,
		known: &mut dyn FnMut(Derived) -> Result<bool, E>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, E> {
		let mut ids = Ids { known, rng };
		let name = definition.name();
		if *name == TYPE_DEFINITION || *name == FIELD_DEFINITION {
			return Ok(Vec::new());
		}
		let known = self.combined.get(name);
		let missing: Vec<(&FieldName, &FieldDef)> = definition
			.fields()
			.filter(|(name, field)| !fields::is_reserved(name) && storable(field))
			.filter(|(name, field)| {
				known
					.and_then(|known| known.field(name))
					.is_none_or(|known| FieldDef::combine([known, *field]).as_ref() != Some(known))
			})
			.collect();
		let version = i64::try_from(definition.version()).ok();
		let newer = known.is_some_and(|known| definition.version() > known.version());
		let labels = missing_labels(known.map(TypeDef::labels), definition.labels());
		let titles = missing_labels(known.map(TypeDef::titles), definition.titles());
		let existing = self
			.entities
			.get(name)
			.and_then(|entities| entities.first().copied());
		let mut operations = Vec::new();
		let type_entity = match existing {
			Some(entity) => {
				if newer && let Some(version) = version {
					operations.push(ids.assert(
						entity,
						fields::TYPE_VERSION,
						Value::Integer(version),
					)?);
				}
				entity
			}
			None if known.is_some()
				&& missing.is_empty()
				&& labels.is_empty()
				&& titles.is_empty() =>
			{
				return Ok(operations);
			}
			None => {
				let entity = ids.entity(type_entity(name))?;
				operations.push(ids.assert(
					entity,
					fields::TYPE,
					Value::Text(TYPE_DEFINITION.to_string()),
				)?);
				operations.push(ids.assert(
					entity,
					fields::TYPE_NAME,
					Value::Text(name.to_string()),
				)?);
				if let Some(version) = version.filter(|&version| version != 1) {
					operations.push(ids.assert(
						entity,
						fields::TYPE_VERSION,
						Value::Integer(version),
					)?);
				}
				entity
			}
		};
		for (tag, text) in labels {
			operations.push(ids.label(type_entity, fields::TYPE_LABEL, tag, text)?);
		}
		for (tag, text) in titles {
			operations.push(ids.label(type_entity, fields::TYPE_TITLE, tag, text)?);
		}
		let field_entities = self.field_entities.get(name);
		for (field_name, field) in missing {
			let known = known.and_then(|known| known.field(field_name));
			// A field that lacks only labels gets them on the entity that defines it, rather
			// than a whole definition of its own.
			let only_labels = known.is_some_and(|known| {
				let mut bare = field.clone();
				bare.set_labels(Labels::new());
				FieldDef::combine([known, &bare]).as_ref() == Some(known)
			});
			match field_entities.and_then(|entities| entities.get(field_name)) {
				Some(&entity) if only_labels => {
					for (tag, text) in missing_labels(known.map(FieldDef::labels), field.labels()) {
						operations.push(ids.label(entity, fields::FIELD_LABEL, tag, text)?);
					}
				}
				_ => operations.extend(define_field(
					&mut ids,
					type_entity,
					(field_name, field),
					known,
				)?),
			}
		}
		Ok(operations)
	}

	/// Combines the definitions of a type in code and in the space, those in code first.
	fn combine(&mut self, name: &TypeName) {
		let combined = match (self.code.get(name), self.space.get(name)) {
			(Some(code), Some(space)) => code.combine(space),
			(Some(code), None) => code.clone(),
			(None, Some(space)) => space.clone(),
			(None, None) => return,
		};
		self.combined.insert(name.clone(), combined);
	}
}

/// The type of the entities that define types.
fn type_definition() -> TypeDef {
	TypeDef::new(TYPE_DEFINITION, 1)
		.with_field(fields::TYPE_NAME, FieldDef::new(ValueKind::Text))
		.with_field(fields::TYPE_VERSION, FieldDef::new(ValueKind::Integer))
		.with_field(fields::TYPE_LABEL, FieldDef::new(ValueKind::Text).many())
		.with_field(fields::TYPE_TITLE, FieldDef::new(ValueKind::Text).many())
}

/// The type of the entities that define fields.
fn field_definition() -> TypeDef {
	TypeDef::new(FIELD_DEFINITION, 1)
		.with_field(fields::FIELD_OF, FieldDef::new(ValueKind::Reference).role())
		.with_field(fields::FIELD_NAME, FieldDef::new(ValueKind::Text))
		.with_field(fields::FIELD_KIND, FieldDef::new(ValueKind::Integer))
		.with_field(
			fields::FIELD_MULTIPLICITY,
			FieldDef::new(ValueKind::Integer),
		)
		.with_field(fields::FIELD_SECRET, FieldDef::new(ValueKind::Boolean))
		.with_field(fields::FIELD_ROLE, FieldDef::new(ValueKind::Boolean))
		.with_field(fields::FIELD_UNIT, FieldDef::new(ValueKind::Text))
		.with_field(fields::FIELD_STANDS_FOR, FieldDef::new(ValueKind::Boolean))
		.with_field(
			fields::FIELD_SEARCH_THROUGH,
			FieldDef::new(ValueKind::Boolean),
		)
		.with_field(fields::FIELD_CODE, FieldDef::new(ValueKind::Boolean))
		.with_field(fields::FIELD_REVERSE, FieldDef::new(ValueKind::Text))
		.with_field(fields::FIELD_LABEL, FieldDef::new(ValueKind::Text).many())
}

/// The context of the IDs of the entities that define types.
const TYPE_ENTITIES: &str = "nibkeep 2026-11 type definitions";

/// The context of the IDs of the entities that define fields.
const FIELD_ENTITIES: &str = "nibkeep 2026-11 field definitions";

/// The context of the IDs of the statements that define types and fields.
const STATEMENTS: &str = "nibkeep 2026-11 definition statements";

/// The ID of the entity that defines a type, in every space: the first 16 bytes of the
/// BLAKE3 key derived with the context `nibkeep 2026-11 type definitions` from the
/// canonical encoding of the type's name, as a version 4 UUID.
#[must_use]
pub fn type_entity(name: &TypeName) -> EntityId {
	EntityId::derived(TYPE_ENTITIES, &cbor::to_vec(&name.to_cbor()))
}

/// The ID of an entity that defines a field of a type, in every space: derived as
/// [`type_entity`] is, with the context `nibkeep 2026-11 field definitions`, from the
/// canonical encoding of `[type, name, definition]`: the ID of the type's entity, the
/// field's name, and the field definition that the entity states, with only the labels
/// that it states.
#[must_use]
pub fn field_entity(type_entity: EntityId, name: &FieldName, definition: &FieldDef) -> EntityId {
	let material = Cbor::Array(vec![
		type_entity.to_cbor(),
		name.to_cbor(),
		definition.to_cbor(),
	]);
	EntityId::derived(FIELD_ENTITIES, &cbor::to_vec(&material))
}

/// The ID of a statement that defines a type or a field, in every space: derived as
/// [`type_entity`] is, with the context `nibkeep 2026-11 definition statements`, from the
/// canonical encoding of `[entity, field, value, qualifiers]`.
#[must_use]
pub fn statement_id(
	entity: EntityId,
	field: &FieldName,
	value: &Value,
	qualifiers: &Qualifiers,
) -> StatementId {
	let material = Cbor::Array(vec![
		entity.to_cbor(),
		field.to_cbor(),
		value.to_cbor(),
		qualifiers.to_cbor(),
	]);
	StatementId::derived(STATEMENTS, &cbor::to_vec(&material))
}

/// An ID that a definition derives, which the space may know already.
#[derive(Clone, Copy, Debug)]
enum Derived {
	/// An entity's.
	Entity(EntityId),
	/// A statement's.
	Statement(StatementId),
}

/// Where the IDs of what a definition writes come from: derived from what it states, unless
/// the space knows one already, and then random.
struct Ids<'a, E, R> {
	/// Whether the space knows an ID.
	known: &'a mut dyn FnMut(Derived) -> Result<bool, E>,
	/// Where random IDs come from.
	rng: &'a mut R,
}

impl<E, R: CryptoRng> Ids<'_, E, R> {
	/// The entity with the derived ID, or a random one if the space knows it.
	fn entity(&mut self, derived: EntityId) -> Result<EntityId, E> {
		Ok(if (self.known)(Derived::Entity(derived))? {
			EntityId::random(self.rng)
		} else {
			derived
		})
	}

	/// The assertion of a statement, under its derived ID, or a random one if the space
	/// knows that.
	fn statement(
		&mut self,
		entity: EntityId,
		field: FieldName,
		value: Value,
		qualifiers: Qualifiers,
	) -> Result<Operation, E> {
		let derived = statement_id(entity, &field, &value, &qualifiers);
		let id = if (self.known)(Derived::Statement(derived))? {
			StatementId::random(self.rng)
		} else {
			derived
		};
		Ok(Operation::assert(
			Statement::new(id, entity, field, value).with_qualifiers(qualifiers),
		))
	}

	/// The assertion of a statement without qualifiers.
	fn assert(&mut self, entity: EntityId, field: FieldName, value: Value) -> Result<Operation, E> {
		self.statement(entity, field, value, Qualifiers::new())
	}

	/// The assertion of a label, in its language unless its tag is `und`.
	fn label(
		&mut self,
		entity: EntityId,
		field: FieldName,
		tag: &str,
		text: &str,
	) -> Result<Operation, E> {
		let qualifiers = if tag == "und" {
			Qualifiers::new()
		} else {
			Qualifiers::new().with(fields::LANGUAGE, Value::Text(tag.to_owned()))
		};
		self.statement(entity, field, Value::Text(text.to_owned()), qualifiers)
	}
}

/// The operations that define a field of a type, for a space whose types lack it, or define
/// it less carefully, or without some of its labels: an entity of its own, with only the
/// labels in languages that the field lacks, so that one it has stays.
fn define_field<E, R: CryptoRng>(
	ids: &mut Ids<'_, E, R>,
	type_entity: EntityId,
	(name, field): (&FieldName, &FieldDef),
	known: Option<&FieldDef>,
) -> Result<Vec<Operation>, E> {
	// What the entity states, from which its ID is derived.
	let mut stated = FieldDef::new(field.kind()).with_multiplicity(field.multiplicity());
	if field.is_secret() {
		stated = stated.secret();
	}
	if field.is_role() {
		stated = stated.role();
	}
	if field.is_stands_for() {
		stated = stated.stands_for();
	}
	if field.is_search_through() {
		stated = stated.search_through();
	}
	if field.is_code() {
		stated = stated.code();
	}
	if let Some(unit) = field.unit() {
		stated = stated.with_unit(unit);
	}
	if let Some(reverse) = field.reverse() {
		stated = stated.with_reverse(reverse.clone());
	}
	let mut labels = Labels::new();
	for (tag, text) in missing_labels(known.map(FieldDef::labels), field.labels()) {
		labels.insert(Tag::parse(tag).as_ref(), text);
	}
	stated.set_labels(labels);
	let entity = ids.entity(field_entity(type_entity, name, &stated))?;
	let mut operations = Vec::new();
	let mut define = |field: FieldName, value: Value| -> Result<(), E> {
		operations.push(ids.assert(entity, field, value)?);
		Ok(())
	};
	define(fields::TYPE, Value::Text(FIELD_DEFINITION.to_string()))?;
	define(fields::FIELD_OF, Value::Reference(type_entity))?;
	define(fields::FIELD_NAME, Value::Text(name.to_string()))?;
	define(
		fields::FIELD_KIND,
		Value::Integer(integer(stated.kind().code())),
	)?;
	if let Some(code) = stated.multiplicity().code() {
		define(fields::FIELD_MULTIPLICITY, Value::Integer(integer(code)))?;
	}
	if stated.is_secret() {
		define(fields::FIELD_SECRET, Value::Boolean(true))?;
	}
	if stated.is_role() {
		define(fields::FIELD_ROLE, Value::Boolean(true))?;
	}
	if stated.is_stands_for() {
		define(fields::FIELD_STANDS_FOR, Value::Boolean(true))?;
	}
	if stated.is_search_through() {
		define(fields::FIELD_SEARCH_THROUGH, Value::Boolean(true))?;
	}
	if stated.is_code() {
		define(fields::FIELD_CODE, Value::Boolean(true))?;
	}
	if let Some(unit) = stated.unit() {
		define(fields::FIELD_UNIT, Value::Text(unit.to_owned()))?;
	}
	if let Some(reverse) = stated.reverse() {
		define(fields::FIELD_REVERSE, Value::Text(reverse.to_string()))?;
	}
	for (tag, text) in stated.labels().iter() {
		operations.push(ids.label(entity, fields::FIELD_LABEL, tag, text)?);
	}
	Ok(operations)
}

/// The labels of a definition in the languages that the known labels lack, as tags and
/// text.
fn missing_labels<'a>(known: Option<&Labels>, labels: &'a Labels) -> Vec<(&'a str, &'a str)> {
	labels
		.iter()
		.filter(|(tag, _)| {
			known.is_none_or(|known| !known.iter().any(|(known_tag, _)| known_tag == *tag))
		})
		.collect()
}

/// The labels in a definition's current statements of a field: of competing labels in one
/// language, the one in the statement with the smallest ID counts. A label without a
/// language is in no known language. A value that is not text, or is empty, is passed over,
/// and so is one whose language this version cannot read, as from a newer version.
fn read_labels<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
) -> Result<Labels, S::Error> {
	let mut labels = Labels::new();
	for statement in state.current(entity, field)? {
		let Some(text) = statement.value.as_text().filter(|text| !text.is_empty()) else {
			continue;
		};
		let language = match statement.qualifiers.get(&fields::LANGUAGE) {
			None => None,
			Some(language) => match language.as_text().and_then(Tag::parse) {
				Some(tag) => Some(tag),
				None => continue,
			},
		};
		if labels.get(language.as_ref()).is_none() {
			labels.insert(language.as_ref(), text);
		}
	}
	Ok(labels)
}

/// Whether the codes of a field's kind and multiplicity fit in an integer value.
fn storable(field: &FieldDef) -> bool {
	i64::try_from(field.kind().code()).is_ok()
		&& field
			.multiplicity()
			.code()
			.is_none_or(|code| i64::try_from(code).is_ok())
}

/// A code as an integer value; [`storable`] checks that it fits.
fn integer(code: u64) -> i64 {
	i64::try_from(code).unwrap_or(i64::MAX)
}

/// The first entity, by ID, that defines each field of a type.
type FieldEntities = BTreeMap<FieldName, EntityId>;

/// The type that an entity of the space defines, if it is a type definition that is not
/// deleted, with a name this version of NibKeep can read that is not one of the two types
/// that define types, and the first entity that defines each of its fields. Fields that
/// NibKeep reserves are passed over.
fn read_type<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<(TypeDef, FieldEntities)>, S::Error> {
	if state.is_deleted(entity)? || !state.types(entity)?.contains(&TYPE_DEFINITION) {
		return Ok(None);
	}
	let Some(name) = first(state, entity, &fields::TYPE_NAME)?
		.and_then(|name| name.as_text()?.parse::<TypeName>().ok())
	else {
		return Ok(None);
	};
	if name == TYPE_DEFINITION || name == FIELD_DEFINITION {
		return Ok(None);
	}
	let version = state
		.current(entity, &fields::TYPE_VERSION)?
		.into_iter()
		.filter_map(|statement| unsigned(&statement.value))
		.max()
		.unwrap_or(1);
	let mut fields: BTreeMap<FieldName, BTreeMap<EntityId, FieldDef>> = BTreeMap::new();
	for referrer in state.referrers(entity)? {
		let field_entity = state.canonical(referrer.entity)?;
		if referrer.field == fields::FIELD_OF
			&& let Some((name, field)) = read_field(state, field_entity)?
			&& !fields::is_reserved(&name)
		{
			fields.entry(name).or_default().insert(field_entity, field);
		}
	}
	let mut definition = TypeDef::new(name, version);
	definition.set_labels(read_labels(state, entity, &fields::TYPE_LABEL)?);
	definition.set_titles(read_labels(state, entity, &fields::TYPE_TITLE)?);
	let mut entities = BTreeMap::new();
	for (name, definitions) in fields {
		if let Some(field) = FieldDef::combine(definitions.values()) {
			if let Some(&first) = definitions.keys().next() {
				entities.insert(name.clone(), first);
			}
			definition = definition.with_field(name, field);
		}
	}
	Ok(Some((definition, entities)))
}

/// The field that an entity of the space defines, if it is a field definition that is not
/// deleted. Of competing values, the one in the statement with the smallest ID counts, and
/// the definition is passed over if this version of NibKeep cannot read it. The field is
/// secret, a role, stands for what it refers to, or is searched through, if any value says
/// so. A unit or a reverse name that cannot be read is passed over.
fn read_field<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<(FieldName, FieldDef)>, S::Error> {
	if state.is_deleted(entity)? || !state.types(entity)?.contains(&FIELD_DEFINITION) {
		return Ok(None);
	}
	let name = first(state, entity, &fields::FIELD_NAME)?
		.and_then(|name| name.as_text()?.parse::<FieldName>().ok());
	let kind = first(state, entity, &fields::FIELD_KIND)?.and_then(|kind| unsigned(&kind));
	let (Some(name), Some(kind)) = (name, kind) else {
		return Ok(None);
	};
	let mut definition = FieldDef::new(ValueKind::from_code(kind));
	if let Some(multiplicity) = first(state, entity, &fields::FIELD_MULTIPLICITY)? {
		let Some(multiplicity) = unsigned(&multiplicity).and_then(Multiplicity::from_code) else {
			return Ok(None);
		};
		definition = definition.with_multiplicity(multiplicity);
	}
	let flag = |field: &FieldName| -> Result<bool, S::Error> {
		Ok(state
			.current(entity, field)?
			.iter()
			.any(|statement| statement.value == Value::Boolean(true)))
	};
	if flag(&fields::FIELD_SECRET)? {
		definition = definition.secret();
	}
	if flag(&fields::FIELD_ROLE)? {
		definition = definition.role();
	}
	if flag(&fields::FIELD_STANDS_FOR)? {
		definition = definition.stands_for();
	}
	if flag(&fields::FIELD_SEARCH_THROUGH)? {
		definition = definition.search_through();
	}
	if flag(&fields::FIELD_CODE)? {
		definition = definition.code();
	}
	if let Some(unit) = first(state, entity, &fields::FIELD_UNIT)?
		.as_ref()
		.and_then(Value::as_text)
		.filter(|unit| Quantity::is_unit(unit))
	{
		definition = definition.with_unit(unit);
	}
	if let Some(reverse) = first(state, entity, &fields::FIELD_REVERSE)?
		.as_ref()
		.and_then(Value::as_text)
		.and_then(|reverse| reverse.parse::<FieldName>().ok())
	{
		definition = definition.with_reverse(reverse);
	}
	definition.set_labels(read_labels(state, entity, &fields::FIELD_LABEL)?);
	Ok(Some((name, definition)))
}

/// The value of the current statement of a field with the smallest ID.
fn first<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
) -> Result<Option<Value>, S::Error> {
	Ok(state
		.current(entity, field)?
		.into_iter()
		.next()
		.map(|statement| statement.into_owned().value))
}

/// The value, if it is an integer that is not negative.
fn unsigned(value: &Value) -> Option<u64> {
	match value {
		Value::Integer(integer) => u64::try_from(*integer).ok(),
		_ => None,
	}
}
