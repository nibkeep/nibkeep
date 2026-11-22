//! What a [`State`](crate::State) is kept in: its statements, which are superseded, and the
//! indexes that find them. The state's logic works on these lookups alone, so that the same
//! logic answers from memory and from a file on disk ([technical design: the state file]).
//!
//! [technical design: the state file]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#75-the-state-file

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use nibkeep_base::small::{SmallMap, SmallSet};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_time::Timestamp;

use crate::{FieldName, Statement};

/// When a statement was first recorded, and by which change.
///
/// Origins order by when they count from, then by when a purge stated them again, if one
/// did, and then by the change's hash: of several origins of one statement, the first
/// counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Origin {
	/// When it counts from, in deciding which value was written last: the time of the
	/// change that asserted it, also when a purge stated it again.
	pub at: Timestamp,
	/// When a purge stated it again, if it came that way: it shows from then on, and its
	/// change is the purge's.
	pub restated: Option<Timestamp>,
	/// The hash of the change that asserted it, if it came in a change.
	pub change: Option<Hash>,
}

impl Origin {
	/// A statement recorded at `at` by the change whose hash is `change`.
	#[must_use]
	pub const fn new(at: Timestamp, change: Option<Hash>) -> Self {
		Self {
			at,
			restated: None,
			change,
		}
	}

	/// A statement that a purge, the change whose hash is `change`, made at `purge`, stated
	/// again from a change made at `at`: it counts from `at`, and shows from `purge` on.
	#[must_use]
	pub const fn restated(at: Timestamp, purge: Timestamp, change: Option<Hash>) -> Self {
		Self {
			at,
			restated: Some(purge),
			change,
		}
	}

	/// When the statement shows from: when it was restated, if it was, or else when it was
	/// recorded.
	#[must_use]
	pub fn shown(&self) -> Timestamp {
		self.restated.unwrap_or(self.at)
	}
}

/// A statement, with the system time it was first recorded at, and the change that first
/// asserted it: borrowed from storage in memory, or read from storage on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded<'a> {
	/// The statement.
	pub statement: Cow<'a, Statement>,
	/// When it was first recorded.
	pub at: Timestamp,
	/// When a purge stated it again, if it came that way: see [`Origin::restated`].
	pub restated: Option<Timestamp>,
	/// The hash of the change that first asserted it, if it came in a change.
	pub change: Option<Hash>,
}

impl Recorded<'_> {
	/// When and by which change it was recorded.
	#[must_use]
	pub const fn origin(&self) -> Origin {
		Origin {
			at: self.at,
			restated: self.restated,
			change: self.change,
		}
	}

	fn from_origin(statement: Cow<'_, Statement>, origin: Origin) -> Recorded<'_> {
		Recorded {
			statement,
			at: origin.at,
			restated: origin.restated,
			change: origin.change,
		}
	}
}

/// The storage of a state's statements. Reading and writing may fail where the storage is
/// on disk: [`Statements::Error`] says why, and is [`Infallible`] in memory.
pub trait Statements {
	/// Why reading or writing failed.
	type Error: std::fmt::Debug;

	/// The statement with this ID, current or superseded.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, Self::Error>;

	/// Whether there is a statement with this ID, current or superseded.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn contains(&self, id: StatementId) -> Result<bool, Self::Error> {
		Ok(self.get(id)?.is_some())
	}

	/// The statement with this ID that [`Statements::put`] wrote, which a statement asserted
	/// again is compared with. Storage in layers answers from the layer it writes to.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn written(&self, id: StatementId) -> Result<Option<Recorded<'_>>, Self::Error> {
		self.get(id)
	}

	/// Every statement, current or superseded, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn all(&self) -> Result<Vec<Recorded<'_>>, Self::Error>;

	/// The hashes of the changes that asserted a statement with this ID besides the one that
	/// the statement held is recorded by, ordered by hash: those that
	/// [`Statements::assert_again`] noted.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn asserted_again(&self, id: StatementId) -> Result<Vec<Hash>, Self::Error>;

	/// Whether an operation superseded the statement with this ID, which may not have
	/// arrived.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn is_superseded(&self, id: StatementId) -> Result<bool, Self::Error> {
		Ok(self.superseded_at(id)?.is_some())
	}

	/// When an operation first superseded the statement with this ID, if one did: of a
	/// statement that the storage holds, an operation about its entity, a retraction of the
	/// earlier form, which names none, and, of a secret one, a retraction of secret
	/// statements; of one it does not hold, any operation.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, Self::Error>;

	/// When an operation first superseded the statement with this ID as a statement of
	/// `entity`, secret or not, if one did: an operation about that entity, a retraction of
	/// the earlier form, which names none, and, if `secret`, a retraction of secret
	/// statements. For a statement held apart from the storage, such as a secret statement
	/// opened beside it, or one written over it.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, Self::Error>;

	/// Whether the statement with this ID that the storage shows is a secret one, opened
	/// apart from the ordinary statements, which a retraction of secret statements
	/// supersedes.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn is_secret(&self, id: StatementId) -> Result<bool, Self::Error> {
		let _ = id;
		Ok(false)
	}

	/// The statements of an entity's field, current or superseded, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<StatementId>, Self::Error>;

	/// The fields of an entity that have statements, current or superseded, each with them,
	/// ordered by field and by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn of_entity(
		&self,
		entity: EntityId,
	) -> Result<Vec<(FieldName, Vec<StatementId>)>, Self::Error>;

	/// The fields of an entity that have current statements, ordered by name. A storage that
	/// can read them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_fields(&self, entity: EntityId) -> Result<Vec<FieldName>, Self::Error> {
		let mut fields = Vec::new();
		for (field, ids) in self.of_entity(entity)? {
			for id in ids {
				if !self.is_superseded(id)? {
					fields.push(field);
					break;
				}
			}
		}
		Ok(fields)
	}

	/// The statements whose values refer to an entity, current or superseded, ordered by
	/// ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, Self::Error>;

	/// The current statements of an entity's field, ordered by ID: those of
	/// [`Statements::of_field`] that are not superseded. A storage that can read them at once
	/// says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let ids = self.of_field(entity, field)?;
		self.current_of(ids, |_| true)
	}

	/// The current statements of a field whose values refer to an entity, ordered by ID:
	/// those of [`Statements::referring_to`] of the field that are not superseded. A storage
	/// that can read them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_referring_to(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let ids = self.referring_to(entity)?;
		self.current_of(ids, |statement| statement.field == *field)
	}

	/// Of these statements, those that are not superseded and that `wanted` keeps, read.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_of(
		&self,
		ids: Vec<StatementId>,
		wanted: impl Fn(&Statement) -> bool,
	) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let mut current = Vec::new();
		for id in ids {
			if self.is_superseded(id)? {
				continue;
			}
			if let Some(recorded) = self.get(id)?
				&& wanted(&recorded.statement)
			{
				current.push(recorded);
			}
		}
		Ok(current)
	}

	/// The current statements, ordered by ID. A storage that can read them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_statements(&self) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let mut current = Vec::new();
		for recorded in self.all()? {
			if !self.is_superseded(recorded.statement.id)? {
				current.push(recorded);
			}
		}
		Ok(current)
	}

	/// The current statements of a field, of every entity, ordered by ID. A storage that can
	/// read them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_in_field(&self, field: &FieldName) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let mut current = Vec::new();
		for recorded in self.all()? {
			if recorded.statement.field == *field && !self.is_superseded(recorded.statement.id)? {
				current.push(recorded);
			}
		}
		Ok(current)
	}
	/// The current statements of these fields of the entities from `first` to `last`, both
	/// included, ordered by ID: what a part of a space says of them, read at once, so that a
	/// space is read part by part, each small enough to sit in memory. A storage that can read
	/// them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn current_in_range(
		&self,
		first: EntityId,
		last: EntityId,
		fields: &[FieldName],
	) -> Result<Vec<Recorded<'_>>, Self::Error> {
		let mut current = Vec::new();
		for recorded in self.all()? {
			let statement = &recorded.statement;
			if (first..=last).contains(&statement.entity)
				&& fields.contains(&statement.field)
				&& !self.is_superseded(statement.id)?
			{
				current.push(recorded);
			}
		}
		Ok(current)
	}

	/// The entities that current statements describe, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn entities(&self) -> Result<Vec<EntityId>, Self::Error>;

	/// The entities that any statement describes, current or superseded, ordered by ID:
	/// every entity whose history the storage holds. A storage that can read them at once
	/// says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn described(&self) -> Result<Vec<EntityId>, Self::Error> {
		let entities: BTreeSet<EntityId> = self
			.all()?
			.iter()
			.map(|recorded| recorded.statement.entity)
			.collect();
		Ok(entities.into_iter().collect())
	}

	/// The names of the fields that any statement has, current or superseded, ordered. A
	/// storage that can read them at once says so.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn field_names(&self) -> Result<Vec<FieldName>, Self::Error> {
		let fields: BTreeSet<FieldName> = self
			.all()?
			.iter()
			.map(|recorded| recorded.statement.field.clone())
			.collect();
		Ok(fields.into_iter().collect())
	}

	/// Keeps a statement, recorded as `origin` says, in place of any with its ID, and
	/// indexes it.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), Self::Error>;

	/// Notes that the change whose hash is `change` asserted a statement with this ID too,
	/// besides the one that the statement held is recorded by: the same statement, as two
	/// devices that run one migration at once both do ([technical design: migrations]), or
	/// another, which only a faulty replica writes. A purge of the ID takes every such
	/// change.
	///
	/// [technical design: migrations]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#57-migrations
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn assert_again(&mut self, id: StatementId, change: Hash) -> Result<(), Self::Error>;

	/// An empty storage of the same kind, if making one costs next to nothing, as in memory;
	/// `None` otherwise, such as for a file, which its owner makes anew. A log builds its
	/// state again in it after a purge.
	#[must_use]
	fn empty_like(&self) -> Option<Self>
	where
		Self: Sized,
	{
		None
	}

	/// Notes that an operation, at `at`, superseded statements, whether or not they have
	/// arrived: those that `scope` covers are superseded from then, unless an operation
	/// superseded them earlier.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn supersede(
		&mut self,
		scope: Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), Self::Error>;
}

/// Which of the statements whose IDs an operation names it supersedes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
	/// Those of one entity, as an operation that names its entity supersedes.
	Entity(EntityId),
	/// Those of any entity, as an operation of the earlier form supersedes.
	Any,
	/// Secret statements alone, of any entity, as a retraction of secret statements
	/// supersedes: an ordinary statement with one of their IDs stays.
	Secret,
}

impl Scope {
	/// What an operation that names `entity` supersedes, or, with `None`, one of the earlier
	/// form, which names none.
	#[must_use]
	pub fn named(entity: Option<EntityId>) -> Self {
		entity.map_or(Self::Any, Self::Entity)
	}

	/// Whether it supersedes a statement of `entity`, secret or not, or, with `None`, a
	/// statement whose entity is not known, which an operation of any scope may supersede.
	#[must_use]
	pub fn covers(self, entity: Option<EntityId>, secret: bool) -> bool {
		match (self, entity) {
			(_, None) | (Self::Any, _) => true,
			(Self::Entity(named), Some(entity)) => named == entity,
			(Self::Secret, Some(_)) => secret,
		}
	}
}

/// The operations that superseded a statement: the earliest time of each scope.
pub type Supersessions = SmallMap<Scope, Timestamp>;

/// The earliest of `supersessions` that supersede a statement of `entity`, secret or not,
/// or, with `None`, a statement whose entity is not known, which any may.
#[must_use]
pub fn earliest(
	supersessions: &Supersessions,
	entity: Option<EntityId>,
	secret: bool,
) -> Option<Timestamp> {
	supersessions
		.iter()
		.filter(|(scope, _)| scope.covers(entity, secret))
		.map(|(_, at)| *at)
		.min()
}

/// A state's storage in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
	/// Every statement asserted so far, with when and by which change it was first
	/// recorded.
	statements: BTreeMap<StatementId, (Statement, Origin)>,
	/// The operations that supersede statements, including some that may not have arrived
	/// yet: for each, the earliest time of each scope. Whether a statement is superseded
	/// follows from them and from its entity, whatever arrived first. Those it holds are
	/// ordinary statements, which a retraction of secret statements does not supersede.
	superseded: BTreeMap<StatementId, Supersessions>,
	/// The statements of each field of each entity. Entities have few fields, and fields
	/// few statements, so these are small sets.
	by_entity: BTreeMap<EntityId, SmallMap<FieldName, SmallSet<StatementId>>>,
	/// The statements whose values refer to each entity.
	by_reference: BTreeMap<EntityId, SmallSet<StatementId>>,
	/// The changes that asserted each statement besides the one it is recorded by.
	again: BTreeMap<StatementId, BTreeSet<Hash>>,
}

impl Memory {
	/// When an operation first superseded a statement, as [`Statements::superseded_at`]
	/// says.
	fn superseded_time(&self, id: StatementId) -> Option<Timestamp> {
		let supersessions = self.superseded.get(&id)?;
		earliest(
			supersessions,
			self.statements
				.get(&id)
				.map(|(statement, _)| statement.entity),
			false,
		)
	}

	fn index(&mut self, statement: &Statement) {
		self.by_entity
			.entry(statement.entity)
			.or_default()
			.get_or_insert_with(statement.field.clone(), SmallSet::new)
			.insert(statement.id);
		if let Some(target) = statement.value.as_reference() {
			self.by_reference
				.entry(target)
				.or_default()
				.insert(statement.id);
		}
	}

	/// Removes a statement from the indexes, together with entries left empty, so that the
	/// storage does not depend on what arrived first.
	fn unindex(&mut self, statement: &Statement) {
		if let Some(entity_fields) = self.by_entity.get_mut(&statement.entity) {
			if let Some(ids) = entity_fields.get_mut(&statement.field) {
				ids.remove(&statement.id);
				if ids.is_empty() {
					entity_fields.remove(&statement.field);
				}
			}
			if entity_fields.is_empty() {
				self.by_entity.remove(&statement.entity);
			}
		}
		if let Some(target) = statement.value.as_reference()
			&& let Some(ids) = self.by_reference.get_mut(&target)
		{
			ids.remove(&statement.id);
			if ids.is_empty() {
				self.by_reference.remove(&target);
			}
		}
	}

	/// Forgets every statement, keeping nothing.
	pub fn clear(&mut self) {
		*self = Self::default();
	}

	/// Keeps a statement, as [`Statements::put`] does, without copying it.
	pub fn insert(&mut self, statement: Statement, origin: Origin) {
		if let Some((old, _)) = self.statements.remove(&statement.id) {
			self.unindex(&old);
		}
		self.index(&statement);
		self.statements.insert(statement.id, (statement, origin));
	}
}

impl Statements for Memory {
	type Error = Infallible;

	fn empty_like(&self) -> Option<Self> {
		Some(Self::default())
	}

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, Infallible> {
		Ok(self
			.statements
			.get(&id)
			.map(|(statement, origin)| Recorded::from_origin(Cow::Borrowed(statement), *origin)))
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, Infallible> {
		Ok(self
			.statements
			.values()
			.map(|(statement, origin)| Recorded::from_origin(Cow::Borrowed(statement), *origin))
			.collect())
	}

	fn contains(&self, id: StatementId) -> Result<bool, Infallible> {
		Ok(self.statements.contains_key(&id))
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<Hash>, Infallible> {
		Ok(self
			.again
			.get(&id)
			.map_or_else(Vec::new, |changes| changes.iter().copied().collect()))
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, Infallible> {
		Ok(self.superseded_time(id))
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, Infallible> {
		Ok(self
			.superseded
			.get(&id)
			.and_then(|supersessions| earliest(supersessions, Some(entity), secret)))
	}

	fn of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<StatementId>, Infallible> {
		Ok(self
			.by_entity
			.get(&entity)
			.and_then(|fields| fields.get(field))
			.into_iter()
			.flatten()
			.copied()
			.collect())
	}

	fn of_entity(
		&self,
		entity: EntityId,
	) -> Result<Vec<(FieldName, Vec<StatementId>)>, Infallible> {
		Ok(self
			.by_entity
			.get(&entity)
			.into_iter()
			.flatten()
			.map(|(field, ids)| (field.clone(), ids.iter().copied().collect()))
			.collect())
	}

	fn current_in_range(
		&self,
		first: EntityId,
		last: EntityId,
		fields: &[FieldName],
	) -> Result<Vec<Recorded<'_>>, Infallible> {
		let mut current = Vec::new();
		// A range that ends before it begins holds nothing.
		if first > last {
			return Ok(current);
		}
		for (_, of_entity) in self.by_entity.range(first..=last) {
			for (field, ids) in of_entity {
				if !fields.contains(field) {
					continue;
				}
				for id in ids {
					if self.superseded_time(*id).is_none()
						&& let Some((statement, origin)) = self.statements.get(id)
					{
						current.push(Recorded::from_origin(Cow::Borrowed(statement), *origin));
					}
				}
			}
		}
		current.sort_by_key(|recorded| recorded.statement.id);
		Ok(current)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, Infallible> {
		Ok(self
			.by_reference
			.get(&entity)
			.into_iter()
			.flatten()
			.copied()
			.collect())
	}

	fn entities(&self) -> Result<Vec<EntityId>, Infallible> {
		Ok(self
			.by_entity
			.iter()
			.filter(|(_, fields)| {
				fields
					.iter()
					.flat_map(|(_, ids)| ids.iter())
					.any(|id| self.superseded_time(*id).is_none())
			})
			.map(|(&entity, _)| entity)
			.collect())
	}

	fn described(&self) -> Result<Vec<EntityId>, Infallible> {
		Ok(self.by_entity.keys().copied().collect())
	}

	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), Infallible> {
		self.insert(statement.clone(), origin);
		Ok(())
	}

	fn assert_again(&mut self, id: StatementId, change: Hash) -> Result<(), Infallible> {
		self.again.entry(id).or_default().insert(change);
		Ok(())
	}

	fn supersede(
		&mut self,
		scope: Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), Infallible> {
		for id in ids {
			let supersessions = self.superseded.entry(id).or_default();
			match supersessions.get_mut(&scope) {
				Some(first) => *first = (*first).min(at),
				None => {
					supersessions.insert(scope, at);
				}
			}
		}
		Ok(())
	}
}

/// A state's ordinary statements, in any storage, with the secret statements this replica has
/// opened, in memory. Opened secrets never reach the storage of the ordinary ones, and
/// forgetting them is dropping them from their layer.
///
/// Secret statements are only asserted and never supersede: what supersedes one is an
/// ordinary operation, such as a retraction of secret statements, which supersedes no
/// ordinary statement. Opening secrets only adds statements: an opened secret with the ID
/// of an ordinary statement, which only a faulty replica makes, does not count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layers<S> {
	ordinary: S,
	opened: Memory,
}

impl<S> Layers<S> {
	/// Ordinary statements in `ordinary`, with no secret statements opened.
	pub fn new(ordinary: S) -> Self {
		Self {
			ordinary,
			opened: Memory::default(),
		}
	}

	/// The storage of the ordinary statements.
	pub const fn ordinary(&self) -> &S {
		&self.ordinary
	}

	/// The storage of the ordinary statements, to write.
	pub const fn ordinary_mut(&mut self) -> &mut S {
		&mut self.ordinary
	}

	/// Adds a secret statement that this replica opened, recorded as `origin` says.
	pub fn open(&mut self, statement: &Statement, origin: Origin) {
		let Ok(()) = self.opened.put(statement, origin);
	}

	/// Forgets one opened secret statement, if it is opened.
	pub fn forget(&mut self, id: StatementId) {
		if let Some((statement, _)) = self.opened.statements.remove(&id) {
			self.opened.unindex(&statement);
		}
	}

	/// Forgets every opened secret statement.
	pub fn forget_all(&mut self) {
		self.opened.clear();
	}

	/// Whether a secret statement is opened.
	#[must_use]
	pub fn is_open(&self, id: StatementId) -> bool {
		self.opened.statements.contains_key(&id)
	}

	/// The opened secret statements, ordered by ID.
	pub fn opened(&self) -> impl Iterator<Item = &Statement> {
		self.opened
			.statements
			.values()
			.map(|(statement, _)| statement)
	}
}

impl<S: Statements> Layers<S> {
	/// Current statements of the ordinary ones, with those opened that are current and that
	/// `wanted` keeps, ordered by ID.
	fn with_opened<'a>(
		&'a self,
		mut current: Vec<Recorded<'a>>,
		opened: Vec<StatementId>,
		wanted: impl Fn(&Statement) -> bool,
	) -> Result<Vec<Recorded<'a>>, S::Error> {
		if opened.is_empty() {
			return Ok(current);
		}
		for id in opened {
			if self.is_superseded(id)? {
				continue;
			}
			let Ok(recorded) = self.opened.get(id);
			if let Some(recorded) = recorded
				&& wanted(&recorded.statement)
			{
				current.push(recorded);
			}
		}
		current.sort_by_key(|recorded| recorded.statement.id);
		Ok(current)
	}

	/// Opened secret statements, without those that an ordinary statement with their ID
	/// shadows.
	fn unshadowed(&self, ids: Vec<StatementId>) -> Result<Vec<StatementId>, S::Error> {
		let mut kept = Vec::with_capacity(ids.len());
		for id in ids {
			if !self.ordinary.contains(id)? {
				kept.push(id);
			}
		}
		Ok(kept)
	}
}

/// Merges two lists that are each ordered, keeping the order, without repeats.
fn merged<T: Ord>(one: Vec<T>, other: Vec<T>) -> Vec<T> {
	if other.is_empty() {
		return one;
	}
	let mut all: Vec<T> = one.into_iter().chain(other).collect();
	all.sort();
	all.dedup();
	all
}

impl<S: Statements> Statements for Layers<S> {
	type Error = S::Error;

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, S::Error> {
		if let Some(recorded) = self.ordinary.get(id)? {
			return Ok(Some(recorded));
		}
		let Ok(opened) = self.opened.get(id);
		Ok(opened)
	}

	fn contains(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(self.ordinary.contains(id)? || self.opened.statements.contains_key(&id))
	}

	fn written(&self, id: StatementId) -> Result<Option<Recorded<'_>>, S::Error> {
		self.ordinary.written(id)
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<Hash>, S::Error> {
		self.ordinary.asserted_again(id)
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, S::Error> {
		let ordinary = self.ordinary.all()?;
		let mut opened = Vec::new();
		for (statement, origin) in self.opened.statements.values() {
			if !self.ordinary.contains(statement.id)? {
				opened.push(Recorded::from_origin(Cow::Borrowed(statement), *origin));
			}
		}
		if opened.is_empty() {
			return Ok(ordinary);
		}
		// Both are ordered by ID, and no ID is in both.
		let mut all = Vec::with_capacity(ordinary.len() + opened.len());
		let mut opened = opened.into_iter().peekable();
		for recorded in ordinary {
			while let Some(secret) =
				opened.next_if(|secret| secret.statement.id < recorded.statement.id)
			{
				all.push(secret);
			}
			all.push(recorded);
		}
		all.extend(opened);
		Ok(all)
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, S::Error> {
		if !self.ordinary.contains(id)?
			&& let Some((statement, _)) = self.opened.statements.get(&id)
		{
			return self.ordinary.superseded_at_of(id, statement.entity, true);
		}
		self.ordinary.superseded_at(id)
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, S::Error> {
		self.ordinary.superseded_at_of(id, entity, secret)
	}

	fn is_secret(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(self.opened.statements.contains_key(&id) && !self.ordinary.contains(id)?)
	}

	fn of_field(&self, entity: EntityId, field: &FieldName) -> Result<Vec<StatementId>, S::Error> {
		let Ok(opened) = self.opened.of_field(entity, field);
		let opened = self.unshadowed(opened)?;
		Ok(merged(self.ordinary.of_field(entity, field)?, opened))
	}

	fn of_entity(&self, entity: EntityId) -> Result<Vec<(FieldName, Vec<StatementId>)>, S::Error> {
		let mut fields = self.ordinary.of_entity(entity)?;
		let Ok(opened) = self.opened.of_entity(entity);
		for (field, ids) in opened {
			let ids = self.unshadowed(ids)?;
			if ids.is_empty() {
				continue;
			}
			match fields.binary_search_by(|(other, _)| other.cmp(&field)) {
				Ok(at) => {
					let existing = std::mem::take(&mut fields[at].1);
					fields[at].1 = merged(existing, ids);
				}
				Err(at) => fields.insert(at, (field, ids)),
			}
		}
		Ok(fields)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, S::Error> {
		let Ok(opened) = self.opened.referring_to(entity);
		let opened = self.unshadowed(opened)?;
		Ok(merged(self.ordinary.referring_to(entity)?, opened))
	}

	fn current_fields(&self, entity: EntityId) -> Result<Vec<FieldName>, S::Error> {
		let mut fields = self.ordinary.current_fields(entity)?;
		let Ok(opened) = self.opened.of_entity(entity);
		for (field, ids) in opened {
			if fields.contains(&field) {
				continue;
			}
			for id in self.unshadowed(ids)? {
				if !self.is_superseded(id)? {
					fields.push(field);
					break;
				}
			}
		}
		fields.sort();
		Ok(fields)
	}

	fn current_of_field(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, S::Error> {
		let Ok(opened) = self.opened.of_field(entity, field);
		let opened = self.unshadowed(opened)?;
		let ordinary = self.ordinary.current_of_field(entity, field)?;
		self.with_opened(ordinary, opened, |_| true)
	}

	fn current_referring_to(
		&self,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<Recorded<'_>>, S::Error> {
		let Ok(opened) = self.opened.referring_to(entity);
		let opened = self.unshadowed(opened)?;
		let ordinary = self.ordinary.current_referring_to(entity, field)?;
		self.with_opened(ordinary, opened, |statement| statement.field == *field)
	}

	fn current_statements(&self) -> Result<Vec<Recorded<'_>>, S::Error> {
		let opened: Vec<StatementId> = self.opened.statements.keys().copied().collect();
		let opened = self.unshadowed(opened)?;
		let ordinary = self.ordinary.current_statements()?;
		self.with_opened(ordinary, opened, |_| true)
	}

	fn current_in_field(&self, field: &FieldName) -> Result<Vec<Recorded<'_>>, S::Error> {
		let opened: Vec<StatementId> = self
			.opened
			.statements
			.values()
			.filter(|(statement, _)| statement.field == *field)
			.map(|(statement, _)| statement.id)
			.collect();
		let opened = self.unshadowed(opened)?;
		let ordinary = self.ordinary.current_in_field(field)?;
		self.with_opened(ordinary, opened, |_| true)
	}
	fn current_in_range(
		&self,
		first: EntityId,
		last: EntityId,
		fields: &[FieldName],
	) -> Result<Vec<Recorded<'_>>, S::Error> {
		let wanted = |statement: &Statement| {
			(first..=last).contains(&statement.entity) && fields.contains(&statement.field)
		};
		let opened: Vec<StatementId> = self
			.opened
			.statements
			.values()
			.filter(|(statement, _)| wanted(statement))
			.map(|(statement, _)| statement.id)
			.collect();
		let opened = self.unshadowed(opened)?;
		let ordinary = self.ordinary.current_in_range(first, last, fields)?;
		self.with_opened(ordinary, opened, |_| true)
	}

	fn entities(&self) -> Result<Vec<EntityId>, S::Error> {
		let mut opened = Vec::new();
		for (statement, _) in self.opened.statements.values() {
			if !self.ordinary.contains(statement.id)? && !self.is_superseded(statement.id)? {
				opened.push(statement.entity);
			}
		}
		opened.sort();
		opened.dedup();
		Ok(merged(self.ordinary.entities()?, opened))
	}

	fn field_names(&self) -> Result<Vec<FieldName>, S::Error> {
		let mut fields: BTreeSet<FieldName> = self.ordinary.field_names()?.into_iter().collect();
		for (statement, _) in self.opened.statements.values() {
			if !self.ordinary.contains(statement.id)? {
				fields.insert(statement.field.clone());
			}
		}
		Ok(fields.into_iter().collect())
	}

	fn described(&self) -> Result<Vec<EntityId>, S::Error> {
		let mut opened = Vec::new();
		for (statement, _) in self.opened.statements.values() {
			if !self.ordinary.contains(statement.id)? {
				opened.push(statement.entity);
			}
		}
		opened.sort();
		opened.dedup();
		Ok(merged(self.ordinary.described()?, opened))
	}

	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), S::Error> {
		self.ordinary.put(statement, origin)
	}

	fn assert_again(&mut self, id: StatementId, change: Hash) -> Result<(), S::Error> {
		self.ordinary.assert_again(id, change)
	}

	fn supersede(
		&mut self,
		scope: Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), S::Error> {
		self.ordinary.supersede(scope, ids, at)
	}
}

/// A storage that reads through to another, `base`, and keeps what is written to it in
/// memory, leaving `base` as it is: to try operations on a state, such as the edits of a
/// change before it is made, without copying the state.
#[derive(Debug)]
pub struct Overlay<'a, S> {
	base: &'a S,
	top: Memory,
}

impl<'a, S> Overlay<'a, S> {
	/// An overlay of `base`, with nothing written to it yet.
	pub fn new(base: &'a S) -> Self {
		Self {
			base,
			top: Memory::default(),
		}
	}
}

impl<S: Statements> Overlay<'_, S> {
	/// IDs from the base, without those of statements written to the overlay, which may be
	/// of another entity or field now.
	fn unwritten(&self, ids: Vec<StatementId>) -> Vec<StatementId> {
		ids.into_iter()
			.filter(|id| !self.top.statements.contains_key(id))
			.collect()
	}

	/// Whether an entity has a current statement, as the overlay shows it.
	fn has_current(&self, entity: EntityId) -> Result<bool, S::Error> {
		for (_, ids) in self.of_entity(entity)? {
			for id in ids {
				if !self.is_superseded(id)? {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}
}

impl<S: Statements> Statements for Overlay<'_, S> {
	type Error = S::Error;

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, S::Error> {
		let Ok(top) = self.top.get(id);
		match top {
			Some(recorded) => Ok(Some(recorded)),
			None => self.base.get(id),
		}
	}

	fn contains(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(self.top.statements.contains_key(&id) || self.base.contains(id)?)
	}

	fn asserted_again(&self, id: StatementId) -> Result<Vec<Hash>, S::Error> {
		let Ok(top) = self.top.asserted_again(id);
		Ok(merged(self.base.asserted_again(id)?, top))
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, S::Error> {
		let mut all: Vec<Recorded<'_>> = self
			.base
			.all()?
			.into_iter()
			.filter(|recorded| !self.top.statements.contains_key(&recorded.statement.id))
			.collect();
		let Ok(top) = self.top.all();
		if !top.is_empty() {
			all.extend(top);
			all.sort_by_key(|recorded| recorded.statement.id);
		}
		Ok(all)
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, S::Error> {
		// The statement as the overlay shows it: written to it, which makes it an ordinary
		// one, or in the base, which says how its operations supersede it.
		let (entity, secret, base) = match self.top.statements.get(&id) {
			Some((statement, _)) => (
				Some(statement.entity),
				false,
				self.base.superseded_at_of(id, statement.entity, false)?,
			),
			None => (
				self.base.get(id)?.map(|recorded| recorded.statement.entity),
				self.base.is_secret(id)?,
				self.base.superseded_at(id)?,
			),
		};
		let top = self
			.top
			.superseded
			.get(&id)
			.and_then(|supersessions| earliest(supersessions, entity, secret));
		Ok(top.into_iter().chain(base).min())
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, S::Error> {
		let Ok(top) = self.top.superseded_at_of(id, entity, secret);
		Ok(top
			.into_iter()
			.chain(self.base.superseded_at_of(id, entity, secret)?)
			.min())
	}

	fn is_secret(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(!self.top.statements.contains_key(&id) && self.base.is_secret(id)?)
	}

	fn of_field(&self, entity: EntityId, field: &FieldName) -> Result<Vec<StatementId>, S::Error> {
		let base = self.unwritten(self.base.of_field(entity, field)?);
		let Ok(top) = self.top.of_field(entity, field);
		Ok(merged(base, top))
	}

	fn of_entity(&self, entity: EntityId) -> Result<Vec<(FieldName, Vec<StatementId>)>, S::Error> {
		let mut fields: Vec<(FieldName, Vec<StatementId>)> = Vec::new();
		for (field, ids) in self.base.of_entity(entity)? {
			let ids = self.unwritten(ids);
			if !ids.is_empty() {
				fields.push((field, ids));
			}
		}
		let Ok(top) = self.top.of_entity(entity);
		for (field, ids) in top {
			match fields.binary_search_by(|(other, _)| other.cmp(&field)) {
				Ok(at) => {
					let existing = std::mem::take(&mut fields[at].1);
					fields[at].1 = merged(existing, ids);
				}
				Err(at) => fields.insert(at, (field, ids)),
			}
		}
		Ok(fields)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, S::Error> {
		let base = self.unwritten(self.base.referring_to(entity)?);
		let Ok(top) = self.top.referring_to(entity);
		Ok(merged(base, top))
	}

	fn entities(&self) -> Result<Vec<EntityId>, S::Error> {
		// What the overlay wrote or superseded may change whether an entity has a current
		// statement: those entities are looked at again, and the rest are as in the base.
		let mut touched = BTreeSet::new();
		for (statement, _) in self.top.statements.values() {
			touched.insert(statement.entity);
		}
		for &id in self.top.statements.keys().chain(self.top.superseded.keys()) {
			if let Some(recorded) = self.base.get(id)? {
				touched.insert(recorded.statement.entity);
			}
		}
		let mut entities: BTreeSet<EntityId> = self
			.base
			.entities()?
			.into_iter()
			.filter(|entity| !touched.contains(entity))
			.collect();
		for entity in touched {
			if self.has_current(entity)? {
				entities.insert(entity);
			}
		}
		Ok(entities.into_iter().collect())
	}

	fn put(&mut self, statement: &Statement, origin: Origin) -> Result<(), S::Error> {
		let Ok(()) = self.top.put(statement, origin);
		Ok(())
	}

	fn assert_again(&mut self, id: StatementId, change: Hash) -> Result<(), S::Error> {
		let Ok(()) = self.top.assert_again(id, change);
		Ok(())
	}

	fn supersede(
		&mut self,
		scope: Scope,
		ids: impl IntoIterator<Item = StatementId>,
		at: Timestamp,
	) -> Result<(), S::Error> {
		let Ok(()) = self.top.supersede(scope, ids, at);
		Ok(())
	}
}

/// A storage as it stood at the end of a millisecond in the past: the statements recorded
/// by then, or restated by then if a purge stated them again, superseded only by operations
/// recorded by then. It is the state that the
/// operations recorded by then build, and takes no writes, which it leaves out.
///
/// Of two different statements with one ID, which only a faulty replica makes, it knows only
/// the one that counts now, from when that one was recorded; and an ordinary statement that
/// hides an opened secret with its ID hides it at every time.
#[derive(Debug)]
pub struct AsOf<'a, S> {
	base: &'a S,
	millis: u64,
}

impl<'a, S> AsOf<'a, S> {
	/// `base` as it stood at the end of the millisecond `millis`.
	pub const fn new(base: &'a S, millis: u64) -> Self {
		Self { base, millis }
	}
}

impl<S: Statements> AsOf<'_, S> {
	/// Whether something recorded at `at` was recorded by then.
	const fn by_then(&self, at: Timestamp) -> bool {
		at.millis() <= self.millis
	}

	/// The IDs of statements recorded by then.
	fn recorded(&self, ids: Vec<StatementId>) -> Result<Vec<StatementId>, S::Error> {
		let mut kept = Vec::with_capacity(ids.len());
		for id in ids {
			if self.contains(id)? {
				kept.push(id);
			}
		}
		Ok(kept)
	}
}

impl<S: Statements> Statements for AsOf<'_, S> {
	type Error = S::Error;

	fn get(&self, id: StatementId) -> Result<Option<Recorded<'_>>, S::Error> {
		Ok(self
			.base
			.get(id)?
			.filter(|recorded| self.by_then(recorded.origin().shown())))
	}

	fn all(&self) -> Result<Vec<Recorded<'_>>, S::Error> {
		Ok(self
			.base
			.all()?
			.into_iter()
			.filter(|recorded| self.by_then(recorded.origin().shown()))
			.collect())
	}

	fn asserted_again(&self, _: StatementId) -> Result<Vec<Hash>, S::Error> {
		// When each change was made is not kept with it, and the past is not purged.
		Ok(Vec::new())
	}

	fn superseded_at(&self, id: StatementId) -> Result<Option<Timestamp>, S::Error> {
		Ok(self.base.superseded_at(id)?.filter(|&at| self.by_then(at)))
	}

	fn superseded_at_of(
		&self,
		id: StatementId,
		entity: EntityId,
		secret: bool,
	) -> Result<Option<Timestamp>, S::Error> {
		Ok(self
			.base
			.superseded_at_of(id, entity, secret)?
			.filter(|&at| self.by_then(at)))
	}

	fn is_secret(&self, id: StatementId) -> Result<bool, S::Error> {
		Ok(self.get(id)?.is_some() && self.base.is_secret(id)?)
	}

	fn of_field(&self, entity: EntityId, field: &FieldName) -> Result<Vec<StatementId>, S::Error> {
		self.recorded(self.base.of_field(entity, field)?)
	}

	fn of_entity(&self, entity: EntityId) -> Result<Vec<(FieldName, Vec<StatementId>)>, S::Error> {
		let mut fields = Vec::new();
		for (field, ids) in self.base.of_entity(entity)? {
			let ids = self.recorded(ids)?;
			if !ids.is_empty() {
				fields.push((field, ids));
			}
		}
		Ok(fields)
	}

	fn referring_to(&self, entity: EntityId) -> Result<Vec<StatementId>, S::Error> {
		self.recorded(self.base.referring_to(entity)?)
	}

	fn entities(&self) -> Result<Vec<EntityId>, S::Error> {
		let mut entities = BTreeSet::new();
		for recorded in self.all()? {
			if !self.is_superseded(recorded.statement.id)? {
				entities.insert(recorded.statement.entity);
			}
		}
		Ok(entities.into_iter().collect())
	}

	fn put(&mut self, _: &Statement, _: Origin) -> Result<(), S::Error> {
		// The past is not written.
		Ok(())
	}

	fn assert_again(&mut self, _: StatementId, _: Hash) -> Result<(), S::Error> {
		Ok(())
	}

	fn supersede(
		&mut self,
		_: Scope,
		_: impl IntoIterator<Item = StatementId>,
		_: Timestamp,
	) -> Result<(), S::Error> {
		Ok(())
	}
}

/// The value of a result that cannot fail, such as a query of a state in memory.
pub trait Infallibly<T> {
	/// The value.
	fn infallible(self) -> T;
}

impl<T> Infallibly<T> for Result<T, Infallible> {
	fn infallible(self) -> T {
		match self {
			Ok(value) => value,
			Err(never) => match never {},
		}
	}
}
