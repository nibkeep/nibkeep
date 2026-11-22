//! The current statements of a space, built from operations in any order.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::cbor;
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_time::{Day, Presence, Start, Timestamp, happened_by, holds_on};

use crate::storage::{Memory, Origin, Recorded, Scope, Statements};
use crate::{
	FieldDef, FieldName, Languages, Multiplicity, Operation, Statement, Tag, TypeName, Value,
	fields,
};

/// The statements of a space, and which of them are current, kept in a storage `S`: in
/// memory, or on disk.
///
/// The state depends only on the set of operations applied, not on their order, so every
/// replica that has received the same operations has the same state. Superseded statements
/// are kept, as history. Every query of a state on disk may fail, which a state in memory
/// never does ([`Infallibly`](crate::storage::Infallibly) unwraps its results).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State<S = Memory> {
	storage: S,
}

/// Whether a field describes an entity's own record rather than what it stands for, so
/// that merging does not combine its statements.
fn is_own_record(field: &FieldName) -> bool {
	*field == fields::DELETED || *field == fields::MERGED_INTO || *field == fields::MOVED_INTO
}

/// The values of a field to show, each in the form chosen, with the number of forms it has.
pub type Counted<'a> = Vec<(Cow<'a, Statement>, usize)>;

/// How the current statements of a field are to be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution<'a> {
	/// The values to show: none, one, or several for a field with many values.
	Values(Vec<Cow<'a, Statement>>),
	/// Several values of a field that has one, which compete until a person chooses.
	Competing(Vec<Cow<'a, Statement>>),
}

impl State {
	/// Returns an empty state in memory.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}
}

impl<S: Statements> State<S> {
	/// A state kept in `storage`, which holds what was applied before.
	pub const fn in_storage(storage: S) -> Self {
		Self { storage }
	}

	/// The storage the state is kept in.
	pub const fn storage(&self) -> &S {
		&self.storage
	}

	/// The storage the state is kept in, to change.
	pub const fn storage_mut(&mut self) -> &mut S {
		&mut self.storage
	}

	/// Applies an operation that was recorded at the system time `recorded`.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read or written.
	pub fn apply(&mut self, operation: &Operation, recorded: Timestamp) -> Result<(), S::Error> {
		self.apply_in(operation, recorded, None)
	}

	/// Applies an operation of the change whose hash is `change`, recorded at the system time
	/// `recorded`: the statements it asserts first record that change as theirs.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read or written.
	pub fn apply_in(
		&mut self,
		operation: &Operation,
		recorded: Timestamp,
		change: Option<Hash>,
	) -> Result<(), S::Error> {
		self.apply_from(operation, Origin::new(recorded, change))
	}

	/// Applies an operation as `origin` says: the statements it asserts are recorded so, and
	/// what it supersedes is superseded from when they show. An operation that a purge stated
	/// again so counts from the time of the change it came from, and shows from the purge on.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read or written.
	pub fn apply_from(&mut self, operation: &Operation, origin: Origin) -> Result<(), S::Error> {
		match operation {
			Operation::Assert {
				statement,
				supersedes,
				any_entity,
			} => {
				self.record(statement, origin)?;
				self.storage.supersede(
					Scope::named((!any_entity).then_some(statement.entity)),
					supersedes.iter().copied(),
					origin.shown(),
				)
			}
			Operation::Retract { entity, statements } => self.storage.supersede(
				Scope::named(*entity),
				statements.iter().copied(),
				origin.shown(),
			),
			Operation::RetractSecret { statements } => {
				self.storage
					.supersede(Scope::Secret, statements.iter().copied(), origin.shown())
			}
			// An operation that this version does not know has no effect here.
			Operation::Unknown(_) => Ok(()),
		}
	}

	/// The part of the state about an entity and the entities merged with it: their
	/// statements, current or superseded, in memory. It shows them as the whole state does,
	/// and keeps doing so as new statements about them are applied to it, as edits write
	/// them: not merges, and superseding only statements of theirs that are not merges. So a
	/// change to one entity can be tried without copying every other. What it lacks: the
	/// entities a merge would join them with, and statements superseded before they arrived;
	/// and it holds opened secret statements as ordinary ones, which a retraction of secret
	/// statements does not supersede there, as edits write none: the log writes it in place
	/// of what they take away of secret statements.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn excerpt(&self, entity: EntityId) -> Result<State, S::Error> {
		let mut excerpt = Memory::default();
		for member in self.merged(entity)? {
			for (_, ids) in self.storage.of_entity(member)? {
				for id in ids {
					if let Some(recorded) = self.storage.get(id)? {
						let origin = recorded.origin();
						excerpt.insert(recorded.statement.into_owned(), origin);
					}
					if let Some(at) = self.storage.superseded_at(id)? {
						let Ok(()) = excerpt.supersede(Scope::Entity(member), [id], at);
					}
				}
			}
		}
		Ok(State::in_storage(excerpt))
	}

	/// The current statements of some fields, of every entity, as a state of their own in
	/// memory, for reading many entities at once: what those fields say now is the same from
	/// it as from the state, and found without a round trip to the storage for each entity.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn excerpt_of_fields(&self, fields: &[FieldName]) -> Result<State, S::Error> {
		let mut excerpt = Memory::default();
		for field in fields {
			for recorded in self.storage.current_in_field(field)? {
				let origin = recorded.origin();
				excerpt.insert(recorded.statement.into_owned(), origin);
			}
		}
		Ok(State::in_storage(excerpt))
	}

	/// The current statements of some fields of the entities from `first` to `last`, both
	/// included, and of the entities merged together with any of them, as a state of their
	/// own in memory with `merges`, the current statements that merge entities together with
	/// those of the range, or more: what those fields say now of the entities of the range,
	/// and of those they are merged into, is the same from it as from the state. A space is
	/// read part by part this way, each part small enough to sit in memory, and each read
	/// once.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn excerpt_of_range(
		&self,
		(first, last): (EntityId, EntityId),
		fields: &[FieldName],
		merges: &[&Recorded<'_>],
	) -> Result<State, S::Error> {
		let mut excerpt = Memory::default();
		for recorded in merges {
			excerpt.insert(recorded.statement.clone().into_owned(), recorded.origin());
		}
		let grouped = State::in_storage(excerpt);
		// The groups of entities merged together that the range touches, each walked once,
		// and their entities that the range leaves out.
		let in_range = |entity: &EntityId| (first..=last).contains(entity);
		let mut groups = BTreeSet::new();
		let mut outside = BTreeSet::new();
		for recorded in merges {
			let statement = &recorded.statement;
			for entity in [Some(statement.entity), statement.value.as_reference()]
				.into_iter()
				.flatten()
				.filter(in_range)
			{
				let Ok(canonical) = grouped.canonical(entity);
				if groups.insert(canonical) {
					let Ok(group) = grouped.merged(canonical);
					outside.extend(group.into_iter().filter(|entity| !in_range(entity)));
				}
			}
		}
		let mut excerpt = grouped.storage;
		for recorded in self.storage.current_in_range(first, last, fields)? {
			let origin = recorded.origin();
			excerpt.insert(recorded.statement.into_owned(), origin);
		}
		for entity in outside {
			for recorded in self.storage.current_in_range(entity, entity, fields)? {
				let origin = recorded.origin();
				excerpt.insert(recorded.statement.into_owned(), origin);
			}
		}
		Ok(State::in_storage(excerpt))
	}

	/// The statement with this ID, current or superseded.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn statement(&self, id: StatementId) -> Result<Option<Cow<'_, Statement>>, S::Error> {
		Ok(self.storage.get(id)?.map(|recorded| recorded.statement))
	}

	/// The system time a statement was first recorded at.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn recorded(&self, id: StatementId) -> Result<Option<Timestamp>, S::Error> {
		Ok(self.storage.get(id)?.map(|recorded| recorded.at))
	}

	/// The current statements, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn statements(&self) -> Result<Vec<Cow<'_, Statement>>, S::Error> {
		Ok(self
			.storage
			.current_statements()?
			.into_iter()
			.map(|recorded| recorded.statement)
			.collect())
	}

	/// The canonical entities that current statements describe, deleted ones included,
	/// ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn entities(&self) -> Result<BTreeSet<EntityId>, S::Error> {
		let mut entities = BTreeSet::new();
		for entity in self.storage.entities()? {
			entities.insert(self.canonical(entity)?);
		}
		Ok(entities)
	}

	/// The fields that an entity, or an entity merged with it, has current statements of,
	/// ordered by name. The fields `deleted` and `merged-into` count only for the entity
	/// itself, as in [`State::current`].
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn fields(&self, entity: EntityId) -> Result<BTreeSet<FieldName>, S::Error> {
		let mut fields = BTreeSet::new();
		for member in self.merged(entity)? {
			for field in self.storage.current_fields(member)? {
				if member == entity || !is_own_record(&field) {
					fields.insert(field);
				}
			}
		}
		Ok(fields)
	}

	/// Whether a statement has been asserted and not superseded.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn is_current(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(self.storage.contains(id)? && !self.storage.is_superseded(id)?)
	}

	/// The current statements of an entity's field, including those of the entities
	/// merged with it, ordered by statement ID.
	///
	/// The fields `deleted` and `merged-into` describe an entity's own record rather than
	/// what it stands for, so only the entity's own statements of them count.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn current(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Cow<'_, Statement>>, S::Error> {
		Ok(self
			.current_recorded(entity, field)?
			.into_iter()
			.map(|recorded| recorded.statement)
			.collect())
	}

	/// The current statements of an entity's field, as [`State::current`] finds them, with
	/// when they were recorded.
	fn current_recorded(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, S::Error> {
		let mut current = if is_own_record(field) {
			self.own(entity, field)?
		} else {
			let mut all = Vec::new();
			for member in self.merged(entity)? {
				all.extend(self.own(member, field)?);
			}
			all
		};
		current.sort_by_key(|recorded| recorded.statement.id);
		Ok(current)
	}

	/// The current statements of an entity's field that may be shown on `day`, by their
	/// valid time, with how certain that is, as [`presence_on`] says. Values without a
	/// valid time, or with one that this version of NibKeep cannot read, are included with
	/// [`Presence::DateUnknown`].
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn current_on(
		&self,
		entity: EntityId,
		field: &FieldName,
		day: Day,
	) -> Result<Vec<(Cow<'_, Statement>, Presence)>, S::Error> {
		Ok(self
			.current(entity, field)?
			.into_iter()
			.map(|statement| {
				let presence = presence_on(&statement, day);
				(statement, presence)
			})
			.filter(|(_, presence)| *presence != Presence::No)
			.collect())
	}

	/// Of the current values of a field of an entity, the one recorded last, and of those
	/// recorded at once, the one with the greatest ID: the value that a field whose last
	/// writer wins shows.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn last_written(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Option<Cow<'_, Statement>>, S::Error> {
		Ok(self
			.current_recorded(entity, field)?
			.into_iter()
			.max_by_key(|recorded| (recorded.at, recorded.statement.id))
			.map(|recorded| recorded.statement))
	}

	/// How the current statements of an entity's field are to be shown, given the field's
	/// definition, if a type defines it. [`FieldDef::combine`] combines the definitions of
	/// several types.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn resolve(
		&self,
		entity: EntityId,
		field: &FieldName,
		definition: Option<&FieldDef>,
	) -> Result<Resolution<'_>, S::Error> {
		Ok(match definition.map(FieldDef::multiplicity) {
			Some(Multiplicity::LastWriterWins) => {
				Resolution::Values(self.last_written(entity, field)?.into_iter().collect())
			}
			Some(Multiplicity::One) => {
				let current = self.current(entity, field)?;
				if current.len() > 1 {
					Resolution::Competing(current)
				} else {
					Resolution::Values(current)
				}
			}
			_ => Resolution::Values(self.current(entity, field)?),
		})
	}

	/// How the current statements of an entity's field are to be shown on `day`, by their
	/// valid time and the field's definition (`docs/spec/model.md`, section 6). A value
	/// dated with an interval is a state, shown while it holds; one dated with a date, or a
	/// date and time, is an event, shown from the day it happened; one without a valid time
	/// is shown on every day. In a field with one value, each value shows until the next one
	/// starts, in the order of [`Start`]; an undated value comes before every dated one,
	/// and one whose valid time cannot be read stays out of the order. In a field with many
	/// values, the preferred one comes first, then the one that started latest. Statements
	/// with the same value and qualifiers count as one. A field whose last writer wins
	/// ignores valid time.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn resolve_on(
		&self,
		entity: EntityId,
		field: &FieldName,
		definition: Option<&FieldDef>,
		day: Day,
	) -> Result<Resolution<'_>, S::Error> {
		self.resolve_for(entity, field, definition, day, &Languages::default())
	}

	/// How the current statements of an entity's field are to be shown on `day` to a reader
	/// of `languages`, as [`State::resolve_on`] says: forms of one value in several
	/// languages or scripts count as one value, which shows in the form that suits the
	/// reader, and values come in the reader's languages first (`docs/spec/model.md`,
	/// section 11).
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn resolve_for(
		&self,
		entity: EntityId,
		field: &FieldName,
		definition: Option<&FieldDef>,
		day: Day,
		languages: &Languages,
	) -> Result<Resolution<'_>, S::Error> {
		let (shown, competing) = self.resolve_counted(entity, field, definition, day, languages)?;
		let shown = shown.into_iter().map(|(statement, _)| statement).collect();
		Ok(if competing {
			Resolution::Competing(shown)
		} else {
			Resolution::Values(shown)
		})
	}

	/// The values of an entity's field to show, as [`State::resolve_for`] chooses them, each
	/// with the number of forms it has, and whether they compete.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn resolve_counted(
		&self,
		entity: EntityId,
		field: &FieldName,
		definition: Option<&FieldDef>,
		day: Day,
		languages: &Languages,
	) -> Result<(Counted<'_>, bool), S::Error> {
		Ok(match definition.map(FieldDef::multiplicity) {
			Some(Multiplicity::LastWriterWins) => {
				// The value written last, in the form that suits the reader.
				let Some(last) = self.last_written(entity, field)? else {
					return Ok((Vec::new(), false));
				};
				let last = content(&last);
				let value = groups(self.current(entity, field)?)
					.into_iter()
					.find(|group| group.forms.iter().any(|form| content(form) == last));
				let value = value.map(|group| group.counted(languages));
				(value.into_iter().collect(), false)
			}
			Some(Multiplicity::One) => {
				let shown: Vec<_> = one_on(groups(self.current(entity, field)?), day)
					.into_iter()
					.map(|group| group.counted(languages))
					.collect();
				let competing = shown.len() > 1;
				(shown, competing)
			}
			_ => (
				many_on(groups(self.current(entity, field)?), day, languages),
				false,
			),
		})
	}

	/// The current forms of the value that a current statement of an entity's field holds,
	/// in several languages or scripts, that statement or the one it repeats included,
	/// ordered by ID; none if the statement is not a current one of the field.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn forms(
		&self,
		entity: EntityId,
		field: &FieldName,
		statement: StatementId,
	) -> Result<Vec<Cow<'_, Statement>>, S::Error> {
		let current = self.current(entity, field)?;
		let Some(said) = current
			.iter()
			.find(|each| each.id == statement)
			.map(|each| content(each))
		else {
			return Ok(Vec::new());
		};
		Ok(groups(current)
			.into_iter()
			.find(|group| group.forms.iter().any(|form| content(form) == said))
			.map_or_default(|group| group.forms))
	}

	/// The current values of an entity's field, each with its forms, in several languages or
	/// scripts, ordered by ID; statements that repeat the value and qualifiers of one with a
	/// smaller ID count as that one.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn form_groups(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Vec<Cow<'_, Statement>>>, S::Error> {
		Ok(groups(self.current(entity, field)?)
			.into_iter()
			.map(|group| group.forms)
			.collect())
	}

	/// The types of an entity. Values of its type field that are not type names are
	/// ignored.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn types(&self, entity: EntityId) -> Result<BTreeSet<TypeName>, S::Error> {
		Ok(self
			.current(entity, &fields::TYPE)?
			.into_iter()
			.filter_map(|statement| statement.value.as_text()?.parse().ok())
			.collect())
	}

	/// Whether an entity is deleted: whether its canonical entity is.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn is_deleted(&self, entity: EntityId) -> Result<bool, S::Error> {
		Ok(self
			.own(self.canonical(entity)?, &fields::DELETED)?
			.iter()
			.any(|recorded| recorded.statement.value == Value::Boolean(true)))
	}

	/// The canonical entity of an entity: the one at the end of its chain of merges.
	///
	/// Of several merges of one entity, the one into the smallest ID counts; if merges form
	/// a loop, the smallest ID in the loop is canonical.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn canonical(&self, entity: EntityId) -> Result<EntityId, S::Error> {
		let mut path = vec![entity];
		let mut current = entity;
		while let Some(next) = self.merge_target(current)? {
			if let Some(start) = path.iter().position(|&seen| seen == next) {
				return Ok(path[start..].iter().copied().min().unwrap_or(next));
			}
			path.push(next);
			current = next;
		}
		Ok(current)
	}

	/// The canonical entity of an entity, and every entity merged into it.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn merged(&self, entity: EntityId) -> Result<BTreeSet<EntityId>, S::Error> {
		let canonical = self.canonical(entity)?;
		// Walk the merges backwards from the canonical entity, through the statements that
		// refer to each member. An entity merged into a member belongs to the group unless
		// another of its merges takes precedence.
		let mut merged = BTreeSet::from([canonical]);
		let mut to_visit = vec![canonical];
		while let Some(member) = to_visit.pop() {
			for recorded in self
				.storage
				.current_referring_to(member, &fields::MERGED_INTO)?
			{
				let other = recorded.statement.entity;
				if !merged.contains(&other) && self.canonical(other)? == canonical {
					merged.insert(other);
					to_visit.push(other);
				}
			}
		}
		Ok(merged)
	}

	/// The current statements whose values refer to an entity, or to an entity merged with
	/// it, ordered by statement ID. They lead to the relationships the entity takes part
	/// in. Merges themselves are not included.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn referrers(&self, entity: EntityId) -> Result<Vec<Cow<'_, Statement>>, S::Error> {
		let mut referrers = Vec::new();
		for member in self.merged(entity)? {
			for id in self.storage.referring_to(member)? {
				if self.storage.is_superseded(id)? {
					continue;
				}
				if let Some(recorded) = self.storage.get(id)?
					&& recorded.statement.field != fields::MERGED_INTO
				{
					referrers.push(recorded.statement);
				}
			}
		}
		referrers.sort_by_key(|statement| statement.id);
		Ok(referrers)
	}

	/// The current statements of one field whose values refer to an entity, or to an entity
	/// merged with it, ordered by statement ID: those of [`State::referrers`] of that field,
	/// which a storage can read without the others.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	pub fn referrers_in(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Cow<'_, Statement>>, S::Error> {
		if *field == fields::MERGED_INTO {
			return Ok(Vec::new());
		}
		let mut referrers = Vec::new();
		for member in self.merged(entity)? {
			for recorded in self.storage.current_referring_to(member, field)? {
				referrers.push(recorded.statement);
			}
		}
		referrers.sort_by_key(|statement| statement.id);
		Ok(referrers)
	}

	/// The current statements of an entity's own field, without merged entities, ordered
	/// by ID.
	fn own(&self, entity: EntityId, field: &FieldName) -> Result<Vec<Recorded<'_>>, S::Error> {
		self.storage.current_of_field(entity, field)
	}

	/// The entity that an entity was merged into, if any.
	fn merge_target(&self, entity: EntityId) -> Result<Option<EntityId>, S::Error> {
		Ok(self
			.own(entity, &fields::MERGED_INTO)?
			.iter()
			.filter_map(|recorded| recorded.statement.value.as_reference())
			.min())
	}

	/// Records an asserted statement.
	fn record(&mut self, statement: &Statement, origin: Origin) -> Result<(), S::Error> {
		let existing = self
			.storage
			.written(statement.id)?
			.map(|recorded| (recorded.origin(), recorded.statement.into_owned()));
		let Some((earlier, existing)) = existing else {
			return self.storage.put(statement, origin);
		};
		// The same statement, asserted again: it counts from its earliest time, rather than
		// as a purge stated it again, and of changes that asserted it at once, from the one
		// with the smallest hash. Two different statements with the same ID, which only a
		// faulty replica makes: the one with the smaller encoding counts, whatever the order
		// they arrived in.
		let replaces = if existing == *statement {
			origin < earlier
		} else {
			cbor::to_vec(statement) < cbor::to_vec(&existing)
		};
		let (kept, other) = if replaces {
			self.storage.put(statement, origin)?;
			(origin, earlier)
		} else {
			(earlier, origin)
		};
		// The other change is noted, as a purge of the ID takes it too, so that nothing
		// asserted under the ID comes back.
		match other.change {
			Some(change) if other.change != kept.change => {
				self.storage.assert_again(statement.id, change)
			}
			_ => Ok(()),
		}
	}
}

/// Whether a value is shown on `day`, by its valid time: a state while its interval holds,
/// an event from the day it happened, and a value without a readable valid time on every
/// day.
#[must_use]
pub fn presence_on(statement: &Statement, day: Day) -> Presence {
	match statement.qualifiers.valid_time() {
		None => Presence::DateUnknown,
		Some(valid_time) if valid_time.is_interval() => holds_on(Some(valid_time), day),
		Some(valid_time) => happened_by(Some(valid_time), day),
	}
}

/// Where a statement stands in the order of a field with one value: an undated one before
/// every dated one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
	Undated,
	Dated(Start),
}

impl Place {
	/// Whether a value in this place has started by the end of `day`.
	fn started_by(self, day: Day) -> bool {
		match self {
			Self::Undated | Self::Dated(Start::BeginningOfTime) => true,
			Self::Dated(Start::At { day: first, .. }) => first <= day,
		}
	}
}

/// Where a statement stands in the order of a field with one value, or `None` if its valid
/// time cannot be read, which keeps it out of the order.
fn place(statement: &Statement) -> Option<Place> {
	match statement.qualifiers.valid_time() {
		Some(valid_time) => Some(Place::Dated(valid_time.start())),
		None if statement.qualifiers.has_valid_time() => None,
		None => Some(Place::Undated),
	}
}

/// The canonical encodings of a statement's value and qualifiers, without the notes about
/// the value ([`fields::is_note`]): equal for statements that say the same.
#[must_use]
pub fn content(statement: &Statement) -> (Vec<u8>, Vec<u8>) {
	let noted = statement
		.qualifiers
		.iter()
		.any(|(field, _)| fields::is_note(field));
	let qualifiers = if noted {
		cbor::to_vec(&statement.qualifiers.without_notes())
	} else {
		cbor::to_vec(&statement.qualifiers)
	};
	(cbor::to_vec(&statement.value), qualifiers)
}

/// Forms of one value, in several languages or scripts.
struct Group<'a> {
	/// The ID of the value's first form, which names the group.
	id: StatementId,
	/// The forms, ordered by ID.
	forms: Vec<Cow<'a, Statement>>,
}

/// Statements, ordered by ID, grouped into the values whose forms they are, ordered by the
/// IDs that name the groups: the statement that a form's `alternative` qualifier names, or
/// else the statement itself. A statement that repeats the value and qualifiers of one with
/// a smaller ID counts as that one, also where a form names it.
fn groups(statements: Vec<Cow<'_, Statement>>) -> Vec<Group<'_>> {
	let mut said: BTreeMap<(Vec<u8>, Vec<u8>), StatementId> = BTreeMap::new();
	let mut repeats: BTreeMap<StatementId, StatementId> = BTreeMap::new();
	let mut distinct = Vec::new();
	for statement in statements {
		match said.entry(content(&statement)) {
			std::collections::btree_map::Entry::Occupied(first) => {
				repeats.insert(statement.id, *first.get());
			}
			std::collections::btree_map::Entry::Vacant(new) => {
				new.insert(statement.id);
				distinct.push(statement);
			}
		}
	}
	let mut groups: BTreeMap<StatementId, Vec<Cow<'_, Statement>>> = BTreeMap::new();
	for statement in distinct {
		let group = statement
			.qualifiers
			.alternative()
			.map_or(statement.id, |first| {
				repeats.get(&first).copied().unwrap_or(first)
			});
		groups.entry(group).or_default().push(statement);
	}
	groups
		.into_iter()
		.map(|(id, forms)| Group { id, forms })
		.collect()
}

impl<'a> Group<'a> {
	/// The form whose time stands for the value's: the first form, or, once it was
	/// corrected, the form with the smallest ID.
	fn first(&self) -> &Statement {
		self.forms
			.iter()
			.find(|form| form.id == self.id)
			.unwrap_or(&self.forms[0])
	}

	/// The form that a reader of `languages` sees, as [`Group::choose`] finds it, with the
	/// number of forms.
	fn counted(self, languages: &Languages) -> (Cow<'a, Statement>, usize) {
		let forms = self.forms.len();
		(self.choose(languages).0, forms)
	}

	/// Whether a form of the value is marked preferred.
	fn is_preferred(&self) -> bool {
		self.forms.iter().any(|form| form.qualifiers.is_preferred())
	}

	/// The form that a reader of `languages` sees, with its match: the one with the earliest
	/// match; of those that match equally, the one derived from the language ranked earliest,
	/// then one that names no source, then one derived from a language not ranked; then the
	/// one marked preferred; then the first form; then the one with the smallest ID. A
	/// machine translation comes after every form that a person wrote, unless every such form
	/// is in a known language that matches no range.
	fn choose(self, languages: &Languages) -> (Cow<'a, Statement>, usize) {
		if let [_] = self.forms.as_slice() {
			let mut forms = self.forms;
			let form = forms.swap_remove(0);
			// Without ranges, every value matches after them all; no tag needs reading.
			let rank = if languages.is_empty() {
				0
			} else {
				languages.rank(form.qualifiers.language().and_then(Tag::parse).as_ref())
			};
			return (form, rank);
		}
		let tags: Vec<Option<Tag>> = self
			.forms
			.iter()
			.map(|form| form.qualifiers.language().and_then(Tag::parse))
			.collect();
		let machine = |tag: &Option<Tag>| tag.as_ref().is_some_and(Tag::is_machine);
		let written: Vec<&Option<Tag>> = tags.iter().filter(|tag| !machine(tag)).collect();
		let readable_written = !written.is_empty()
			&& !written.iter().all(|tag| {
				tag.as_ref().is_some_and(Tag::is_known)
					&& languages.rank(tag.as_ref()) == languages.len()
			});
		let (index, rank) = tags
			.iter()
			.enumerate()
			.map(|(index, tag)| {
				let rank = languages.rank(tag.as_ref());
				let key = (
					readable_written && machine(tag),
					rank,
					languages.derivation(tag.as_ref()),
					!self.forms[index].qualifiers.is_preferred(),
					self.forms[index].id != self.id,
					self.forms[index].id,
				);
				(key, index, rank)
			})
			.min()
			.map(|(_, index, rank)| (index, rank))
			.expect("a group has forms");
		let mut forms = self.forms;
		(forms.swap_remove(index), rank)
	}
}

/// The values a field with one value shows on `day`: of those that started by then, the
/// ones that start last, unless their own period has ended; and those whose valid time
/// cannot be read. Forms of one value stand where their first form does. Ordered by the
/// IDs that name them.
fn one_on(groups: Vec<Group<'_>>, day: Day) -> Vec<Group<'_>> {
	let last = groups
		.iter()
		.filter_map(|group| place(group.first()))
		.filter(|place| place.started_by(day))
		.max();
	groups
		.into_iter()
		.filter(|group| match place(group.first()) {
			// Those whose valid time cannot be read are always shown.
			None => true,
			Some(place) => {
				place.started_by(day)
					&& Some(place) == last
					&& presence_on(group.first(), day) != Presence::No
			}
		})
		.collect()
}

/// The values a field with many values shows on `day`, each by its own valid time and in
/// the form that suits a reader of `languages`: the preferred ones first, then those in the
/// reader's languages, by their match, then the ones that started latest, then by statement
/// ID; each with the number of forms it has.
fn many_on<'a>(
	groups: Vec<Group<'a>>,
	day: Day,
	languages: &Languages,
) -> Vec<(Cow<'a, Statement>, usize)> {
	let mut shown: Vec<_> = groups
		.into_iter()
		.filter(|group| presence_on(group.first(), day) != Presence::No)
		.map(|group| {
			let preferred = group.is_preferred();
			let started = place(group.first());
			let forms = group.forms.len();
			let (form, rank) = group.choose(languages);
			let key = (!preferred, rank, std::cmp::Reverse(started), form.id);
			(key, (form, forms))
		})
		.collect();
	shown.sort_by_key(|(key, _)| *key);
	shown.into_iter().map(|(_, value)| value).collect()
}
