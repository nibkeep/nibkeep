//! Indexes and search of a space's current statements, kept in the state file.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use nibkeep_base::SpaceId;
use nibkeep_base::cbor;
#[cfg(test)]
use nibkeep_base::small::SmallSet;
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_log::{History, Log, SpaceState};
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// An entity that a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
	/// The entity, or of entities merged together, the one they were merged into.
	pub entity: EntityId,
	/// How many current statements of the entity, and of the entities merged with it, hold
	/// words of the search in their values.
	pub matching: usize,
}

/// How many times as many statements as the rarest whole word the beginning of the last
/// word may start, at most, for its statements to be read with the whole words', rather
/// than looked for in each candidate's own: reading a statement of a word costs a fraction
/// of checking a candidate, so for a common last word, such as `of` in `wizard of`, reading
/// costs less even when it holds a few times as many statements.
const PREFIX_READ_FACTOR: usize = 4;

/// The indexes of a space's ordinary current statements: by the words of their text, and
/// by their values. Secret statements are left out: those the log holds sealed, and those
/// of fields that are secret on this replica. So are the older values of fields whose last
/// writer wins, which no one sees: of those, only the value written last is indexed. The
/// fields that NibKeep reserves for its own structure, such as the types of entities, and
/// those that hold keys and positions, are found by value but not by their words.
///
/// The ordinary statements change only through the operations of accepted changes, so
/// taking in those operations keeps the indexes exact. Opening and forgetting secret
/// statements does not touch them.
///
/// The indexes are kept in postings: in the state file, or in memory. Writing them takes
/// `&self`, as the postings of the state file are kept by the storage that the state reads.
pub(crate) trait Postings {
	/// Why reading or writing failed.
	type Error;

	/// What tells statements apart in a search, which never needs their IDs: a number the
	/// postings give each statement, or its ID.
	type Key: Copy + Ord;

	/// What is indexed of a statement.
	fn indexed(&self, id: StatementId) -> Result<Option<Indexed>, Self::Error>;

	/// Indexes a statement.
	fn insert(&self, id: StatementId, indexed: &Indexed) -> Result<(), Self::Error>;

	/// Takes a statement out of the indexes, if it is in them.
	fn remove(&self, id: StatementId) -> Result<(), Self::Error>;

	/// Empties the indexes.
	fn clear(&self) -> Result<(), Self::Error>;

	/// The statements whose text holds a word, with the entities they show under, of those
	/// that are not deleted.
	fn with_word(&self, word: &str) -> Result<Vec<(Self::Key, EntityId)>, Self::Error>;

	/// How many statements hold a word.
	fn word_count(&self, word: &str) -> Result<usize, Self::Error>;

	/// The statements whose text holds a word that starts with `prefix`, each once, with the
	/// entities they show under, of those that are not deleted.
	fn with_prefix(&self, prefix: &str) -> Result<Vec<(Self::Key, EntityId)>, Self::Error>;

	/// How many statements hold each word that starts with `prefix`, added together, or
	/// `at_most` if they are more: counting stops there, as a short beginning can start a
	/// great many words.
	fn prefix_count(&self, prefix: &str, at_most: usize) -> Result<usize, Self::Error>;

	/// The indexed statements that show under an entity whose text holds a word that starts
	/// with `prefix`.
	fn shown_with_prefix(
		&self,
		entity: EntityId,
		prefix: &str,
	) -> Result<Vec<Self::Key>, Self::Error>;

	/// The entities that the statements of a field with a value show under, of those that
	/// are not deleted, by the hash of the value's canonical CBOR.
	fn with_value(&self, field: &FieldName, value: &Hash) -> Result<Vec<EntityId>, Self::Error>;

	/// The entities that the statements of fields that name entities, with a name that is
	/// `lowered` once lowercased, show under, deleted ones included.
	fn with_name(&self, lowered: &str) -> Result<Vec<EntityId>, Self::Error>;

	/// The indexed statements that show under an entity.
	fn of_shown(&self, entity: EntityId) -> Result<Vec<(StatementId, Indexed)>, Self::Error>;

	/// Marks the statements that show under an entity as deleted, or not.
	fn set_deleted(&self, entity: EntityId, deleted: bool) -> Result<(), Self::Error>;

	/// Takes the statements of a field, of every entity, out of the indexes.
	fn remove_field(&self, field: &FieldName) -> Result<(), Self::Error>;
}

/// The entities that statements show under, and whether they are deleted, as the state
/// says, kept for a call that indexes many statements of one entity.
#[derive(Default)]
struct Shown(BTreeMap<EntityId, (EntityId, bool)>);

impl Shown {
	/// The entity that an entity's statements show under: its own, or the one it was merged
	/// into; and whether that one is deleted.
	fn of<S: Statements>(
		&mut self,
		state: &SpaceState<S>,
		entity: EntityId,
	) -> Result<(EntityId, bool), S::Error> {
		if let Some(&known) = self.0.get(&entity) {
			return Ok(known);
		}
		let canonical = state.canonical(entity)?;
		let known = (canonical, state.is_deleted(canonical)?);
		self.0.insert(entity, known);
		Ok(known)
	}
}

/// Which fields a space treats specially: those that are secret on this replica, and those
/// whose last writer wins.
#[derive(Clone, Copy)]
pub(crate) struct Fields<'a> {
	pub(crate) secret: &'a BTreeSet<FieldName>,
	pub(crate) last_writer_wins: &'a BTreeSet<FieldName>,
}

/// What is indexed of a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Indexed {
	pub(crate) entity: EntityId,
	/// The entity it shows under: its own, or the one that it was merged into.
	pub(crate) shown: EntityId,
	/// Whether that entity is deleted.
	pub(crate) deleted: bool,
	/// The words of its text, sorted, without duplicates.
	pub(crate) words: Vec<String>,
	/// Of a field that names entities, the name it holds, lowercased.
	pub(crate) name: Option<String>,
	pub(crate) value: (FieldName, Hash),
}

/// Of each field whose last writer wins, the value written last, worked out once for every
/// entity, or of entities merged together, once for all of them.
#[derive(Default)]
struct LastWritten(BTreeMap<(EntityId, FieldName), Option<StatementId>>);

impl LastWritten {
	/// The value written last of the field of a statement, for the statement's entity and
	/// those merged with it: the statement is shown if it is that one.
	fn of<S: Statements>(
		&mut self,
		state: &SpaceState<S>,
		statement: &Statement,
	) -> Result<Option<StatementId>, S::Error> {
		let key = (state.canonical(statement.entity)?, statement.field.clone());
		if let Some(written) = self.0.get(&key) {
			return Ok(*written);
		}
		let written = state
			.last_written(statement.entity, &statement.field)?
			.map(|last| last.id);
		self.0.insert(key, written);
		Ok(written)
	}
}

/// Builds the indexes of a log's current state anew in `postings`.
pub(crate) fn build<S, H, P>(
	log: &Log<S, H>,
	postings: &P,
	fields: Fields<'_>,
) -> Result<(), S::Error>
where
	S: Statements,
	H: History<Error = S::Error>,
	P: Postings<Error = S::Error>,
{
	let state = log.state();
	postings.clear()?;
	let mut shown = Shown::default();
	let mut last = LastWritten::default();
	for statement in state.statements()? {
		if fields.last_writer_wins.contains(&statement.field)
			&& last.of(state, &statement)? != Some(statement.id)
		{
			continue;
		}
		add(log, postings, fields, &mut shown, &statement)?;
	}
	Ok(())
}

/// Brings the indexes of a log's current state in `postings` from the fields they were built
/// with, `was`, to the fields that are special now, `fields`: the statements of a field that
/// became secret leave; those of a field that is no longer secret come in; of a field whose
/// last writer wins now, only the value written last of each entity, or of entities merged
/// together, stays; and every current statement of a field whose last writer no longer wins
/// comes in. The other fields stay as they are.
pub(crate) fn rebuild_fields<S, H, P>(
	log: &Log<S, H>,
	postings: &P,
	was: Fields<'_>,
	fields: Fields<'_>,
) -> Result<(), S::Error>
where
	S: Statements,
	H: History<Error = S::Error>,
	P: Postings<Error = S::Error>,
{
	let state = log.state();
	let mut shown = Shown::default();
	let mut last = LastWritten::default();
	let changed: BTreeSet<&FieldName> = was
		.secret
		.symmetric_difference(fields.secret)
		.chain(
			was.last_writer_wins
				.symmetric_difference(fields.last_writer_wins),
		)
		.collect();
	for field in changed {
		postings.remove_field(field)?;
		if fields.secret.contains(field) {
			continue;
		}
		let last_wins = fields.last_writer_wins.contains(field);
		for recorded in state.storage().current_in_field(field)? {
			let statement = &recorded.statement;
			if last_wins && last.of(state, statement)? != Some(statement.id) {
				continue;
			}
			add(log, postings, fields, &mut shown, statement)?;
		}
	}
	Ok(())
}

/// What the operations of a change touch, noted as they are taken in: what is looked at
/// again once they all are.
#[derive(Default)]
struct Touched {
	/// The fields whose last writer wins that changed, with their entities.
	last_writer_wins: BTreeSet<(EntityId, FieldName)>,
	/// The entities whose deletion changed.
	deletions: BTreeSet<EntityId>,
	/// The entities whose merges changed.
	merged: BTreeSet<EntityId>,
	/// The entities they were, or are, merged into.
	merged_into: BTreeSet<EntityId>,
	/// The entities that merges taken out of the indexes showed under: where the statements
	/// of the entities merged showed before.
	merged_under: BTreeSet<EntityId>,
}

impl Touched {
	/// Notes a field of an entity that an operation touched, and for a merge, the entity it
	/// leads to: the one it refers to, or the one its statement was indexed under.
	fn field(
		&mut self,
		fields: Fields<'_>,
		entity: EntityId,
		field: &FieldName,
		other: Option<EntityId>,
	) {
		if *field == fields::DELETED {
			self.deletions.insert(entity);
		}
		if *field == fields::MERGED_INTO {
			self.merged.insert(entity);
			self.merged_into.extend(other);
		}
		if fields.last_writer_wins.contains(field) {
			self.last_writer_wins.insert((entity, field.clone()));
		}
	}

	/// Notes a statement that an operation asserts, supersedes or retracts, or that the state
	/// holds under its ID.
	fn statement(&mut self, fields: Fields<'_>, statement: &Statement) {
		self.field(
			fields,
			statement.entity,
			&statement.field,
			statement.value.as_reference(),
		);
	}

	/// Notes a statement taken out of the indexes, as it was indexed.
	fn indexed(&mut self, fields: Fields<'_>, indexed: &Indexed) {
		self.field(fields, indexed.entity, &indexed.value.0, None);
		if indexed.value.0 == fields::MERGED_INTO {
			self.merged_under.insert(indexed.shown);
		}
	}
}

/// Takes in the operations of a change after the log accepted it. A statement is indexed
/// once it is asserted, unless it is superseded already, and taken out once it is
/// superseded, which is for good. Of a field whose last writer wins, the value written last
/// is indexed again whenever the field changes. A merge of entities moves the statements of
/// the entities merged to the entity they show under now, looks again at every field whose
/// last writer wins of the entities merged together, and leaves the other statements alone.
pub(crate) fn apply<'a, S, H, P>(
	log: &Log<S, H>,
	postings: &P,
	fields: Fields<'_>,
	operations: impl IntoIterator<Item = &'a Operation>,
) -> Result<(), S::Error>
where
	S: Statements,
	H: History<Error = S::Error>,
	P: Postings<Error = S::Error>,
{
	let state = log.state();
	let mut shown = Shown::default();
	let mut touched = Touched::default();
	// The statements indexed by this call, which show under the right entity already.
	let mut fresh = BTreeSet::new();
	for operation in operations {
		match operation {
			Operation::Assert {
				statement,
				supersedes,
				..
			} => {
				touched.statement(fields, statement);
				// The state keeps one statement of an ID that was asserted twice: the one it
				// had, perhaps recorded anew, or this one, which may replace the one indexed,
				// of another entity or field. The fields of all three are looked at again.
				if let Some(kept) = state.statement(statement.id)? {
					touched.statement(fields, &kept);
				}
				if let Some(indexed) = take_out(postings, statement.id)? {
					touched.indexed(fields, &indexed);
				}
				if state.is_current(statement.id)?
					&& let Some(current) = state.statement(statement.id)?
					&& !fields.last_writer_wins.contains(&current.field)
				{
					add(log, postings, fields, &mut shown, &current)?;
					fresh.insert(current.id);
				}
				for id in supersedes {
					// One of another entity is not superseded.
					if state.is_current(*id)? {
						continue;
					}
					if let Some(superseded) = state.statement(*id)? {
						touched.statement(fields, &superseded);
					}
					if let Some(indexed) = take_out(postings, *id)? {
						touched.indexed(fields, &indexed);
					}
				}
			}
			// A retraction of secret statements leaves an ordinary statement with one of
			// their IDs current, which is then left as it is.
			Operation::Retract { statements, .. } | Operation::RetractSecret { statements } => {
				for id in statements {
					if state.is_current(*id)? {
						continue;
					}
					if let Some(retracted) = state.statement(*id)? {
						touched.statement(fields, &retracted);
					}
					if let Some(indexed) = take_out(postings, *id)? {
						touched.indexed(fields, &indexed);
					}
				}
			}
			Operation::Unknown(_) => {}
		}
	}
	if !touched.merged.is_empty()
		&& !move_merged(log, postings, fields, &mut shown, &mut touched, &fresh)?
	{
		return build(log, postings, fields);
	}
	// What shows under a deleted or restored entity is marked so.
	for entity in touched.deletions {
		let entity = state.canonical(entity)?;
		postings.set_deleted(entity, state.is_deleted(entity)?)?;
	}
	// The state holds every change applied at once, so a statement taken out here may be
	// indexed as another entity's or field's, whose value written last is then looked at
	// again too.
	let mut pending: Vec<(EntityId, FieldName)> = touched.last_writer_wins.into_iter().collect();
	let mut done = BTreeSet::new();
	while let Some((entity, field)) = pending.pop() {
		if !done.insert((state.canonical(entity)?, field.clone())) {
			continue;
		}
		for statement in state.current(entity, &field)? {
			if let Some(indexed) = postings.indexed(statement.id)? {
				if indexed.entity != statement.entity || indexed.value.0 != statement.field {
					return build(log, postings, fields);
				}
				if fields.last_writer_wins.contains(&indexed.value.0) {
					pending.push((indexed.entity, indexed.value.0.clone()));
				}
			}
			postings.remove(statement.id)?;
		}
		if let Some(last) = state.last_written(entity, &field)? {
			add(log, postings, fields, &mut shown, &last)?;
		}
	}
	Ok(())
}

/// Moves the statements that merges moved: those of the entities merged, `touched.merged`,
/// and of the entities merged into them, which show under another entity now, or under one
/// that is deleted where theirs was not. The entities they were merged into keep what they
/// show, and so do the statements in `fresh`, indexed by this change. Of every field whose
/// last writer wins of an entity moved, the value written last among the entities merged
/// together may have changed, for the entities merged into too: `touched` notes those
/// fields, for the caller to look at again.
///
/// Returns `false` if a statement turns out indexed as another entity's or field's than the
/// state's statement of its ID: two different statements with one ID, which only a faulty
/// replica makes, and which an earlier change applied with this one may have swapped in the
/// indexes unnoticed. The caller then builds the indexes anew.
fn move_merged<S, H, P>(
	log: &Log<S, H>,
	postings: &P,
	fields: Fields<'_>,
	shown: &mut Shown,
	touched: &mut Touched,
	fresh: &BTreeSet<StatementId>,
) -> Result<bool, S::Error>
where
	S: Statements,
	H: History<Error = S::Error>,
	P: Postings<Error = S::Error>,
{
	let state = log.state();
	let Touched {
		merged,
		merged_into,
		merged_under,
		last_writer_wins,
		..
	} = touched;
	// Whose statements may have moved: the entities merged, and the entities that showed
	// under them, under what their merges showed under, or under the same entities as their
	// statements did before this change, and show under another now: the rest of their old
	// groups.
	let mut candidates = BTreeSet::new();
	let mut under = merged_under.clone();
	for &entity in merged.iter() {
		candidates.insert(entity);
		under.insert(entity);
		for (field, ids) in state.storage().of_entity(entity)? {
			for id in ids {
				if fresh.contains(&id) {
					continue;
				}
				if let Some(indexed) = postings.indexed(id)? {
					if indexed.entity != entity || indexed.value.0 != field {
						return Ok(false);
					}
					under.insert(indexed.shown);
				}
			}
		}
	}
	for &shown_under in &under {
		for (_, indexed) in postings.of_shown(shown_under)? {
			if shown.of(state, indexed.entity)?.0 != shown_under {
				candidates.insert(indexed.entity);
			}
		}
	}
	// Each statement is looked at on its own: an earlier change applied with this one may
	// have indexed some of an entity's statements where they show now, and not others.
	let mut lww_fields = BTreeSet::new();
	for candidate in candidates {
		let (canonical, deleted) = shown.of(state, candidate)?;
		for (field, ids) in state.storage().of_entity(candidate)? {
			let last_wins = fields.last_writer_wins.contains(&field);
			if last_wins {
				last_writer_wins.insert((candidate, field.clone()));
				lww_fields.insert(field.clone());
			}
			for id in ids {
				if fresh.contains(&id) {
					continue;
				}
				let Some(indexed) = postings.indexed(id)? else {
					continue;
				};
				if indexed.entity != candidate || indexed.value.0 != field {
					return Ok(false);
				}
				if indexed.shown == canonical && indexed.deleted == deleted {
					continue;
				}
				postings.remove(id)?;
				// Of a field whose last writer wins, the caller indexes the value written last.
				if last_wins {
					continue;
				}
				if state.is_current(id)?
					&& let Some(statement) = state.statement(id)?
				{
					add(log, postings, fields, shown, &statement)?;
				}
			}
		}
	}
	// What was written last of those fields may have changed for the entities merged into,
	// and for those that the statements moved showed under before: a value that shows only
	// while another entity is merged with them, or one of theirs that such a value hid.
	for &entity in merged_into.iter().chain(under.iter()) {
		for field in &lww_fields {
			last_writer_wins.insert((entity, field.clone()));
		}
	}
	Ok(true)
}

/// Finds the entities whose current values hold every word of `query`. The last word also
/// matches the start of a longer word, so that results appear while typing. Entities with
/// more matching values come first, deleted ones not at all.
///
/// The candidates are the entities that hold every whole word, and the beginning of the
/// last one too unless it starts more than [`PREFIX_READ_FACTOR`] times as many statements
/// as the rarest whole word; their own statements are then checked for the beginning.
pub(crate) fn search<P: Postings>(postings: &P, query: &str) -> Result<Vec<Found>, P::Error> {
	let words = words(query);
	let Some(last) = words.len().checked_sub(1) else {
		return Ok(Vec::new());
	};
	// Every word must be held by the entity or one merged with it, so the candidates are
	// the entities that the statements of every whole word belong to, starting with the
	// word held least. Reading a word's statements costs as much as there are, so the
	// last word, the beginning of words, joins them only when it does not start many more
	// statements than the rarest whole word holds.
	let mut counts = Vec::new();
	for word in &words[..last] {
		counts.push((postings.word_count(word)?, Some(word.as_str())));
	}
	let prefix_read = match counts.iter().map(|(count, _)| *count).min() {
		None => {
			counts.push((0, None));
			true
		}
		Some(rarest) => {
			let most = rarest.saturating_mul(PREFIX_READ_FACTOR);
			let beginning = postings.prefix_count(&words[last], most.saturating_add(1))?;
			let read = beginning <= most;
			if read {
				counts.push((beginning, None));
			}
			read
		}
	};
	counts.sort_by_key(|(count, _)| *count);
	// For each candidate, the statements that hold the words read so far.
	let mut candidates: Option<BTreeMap<EntityId, BTreeSet<P::Key>>> = None;
	for (_, word) in counts {
		let held = match word {
			Some(word) => postings.with_word(word)?,
			None => postings.with_prefix(&words[last])?,
		};
		let mut holding: BTreeMap<EntityId, BTreeSet<P::Key>> = BTreeMap::new();
		for (statement, entity) in held {
			if candidates
				.as_ref()
				.is_none_or(|known| known.contains_key(&entity))
			{
				holding.entry(entity).or_default().insert(statement);
			}
		}
		if let Some(known) = candidates {
			for (entity, statements) in &mut holding {
				statements.extend(&known[entity]);
			}
		}
		if holding.is_empty() {
			return Ok(Vec::new());
		}
		candidates = Some(holding);
	}
	let mut found = Vec::new();
	for (entity, mut statements) in candidates.unwrap_or_default() {
		// The beginning of the last word, if it was not read with the others, is looked for
		// in the candidates' own statements.
		if !prefix_read {
			let holding = postings.shown_with_prefix(entity, &words[last])?;
			if holding.is_empty() {
				continue;
			}
			statements.extend(holding);
		}
		found.push(Found {
			entity,
			matching: statements.len(),
		});
	}
	found.sort_by(|a, b| b.matching.cmp(&a.matching).then(a.entity.cmp(&b.entity)));
	Ok(found)
}

/// The entities whose field `field` has the current value `value`, exactly, ordered by ID.
/// Of entities merged together, the one they were merged into is given, and deleted ones
/// not at all.
pub(crate) fn find<P: Postings>(
	postings: &P,
	field: &FieldName,
	value: &Value,
) -> Result<Vec<EntityId>, P::Error> {
	let entities: BTreeSet<EntityId> = postings
		.with_value(field, &Hash::of(&cbor::to_vec(value)))?
		.into_iter()
		.collect();
	Ok(entities.into_iter().collect())
}

/// Takes a statement out of the indexes, if it is in them, and says what was indexed of it,
/// for the caller to look at its entity and field again: the state, which holds every change
/// applied at once, may already hold another statement of the same ID.
fn take_out<P: Postings>(postings: &P, id: StatementId) -> Result<Option<Indexed>, P::Error> {
	let indexed = postings.indexed(id)?;
	if indexed.is_some() {
		postings.remove(id)?;
	}
	Ok(indexed)
}

/// Indexes a statement, unless it is secret. Of a field whose last writer wins, the caller
/// adds only the value written last.
fn add<S, H, P>(
	log: &Log<S, H>,
	postings: &P,
	fields: Fields<'_>,
	shown: &mut Shown,
	statement: &Statement,
) -> Result<(), P::Error>
where
	S: Statements,
	H: History<Error = S::Error>,
	P: Postings<Error = S::Error>,
{
	if fields.secret.contains(&statement.field) || log.is_sealed(statement.id)? {
		return Ok(());
	}
	let mut words: Vec<String> = if fields::has_words(&statement.field) {
		statement
			.value
			.searchable_texts()
			.into_iter()
			.flat_map(self::words)
			.collect()
	} else {
		Vec::new()
	};
	words.sort_unstable();
	words.dedup();
	let name = fields::name_key(&statement.field, &statement.value);
	let (shown_under, deleted) = shown.of(log.state(), statement.entity)?;
	postings.insert(
		statement.id,
		&Indexed {
			entity: statement.entity,
			shown: shown_under,
			deleted,
			words,
			name,
			value: (
				statement.field.clone(),
				Hash::of(&cbor::to_vec(&statement.value)),
			),
		},
	)
}

/// The indexes in memory, for tests.
#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryPostings(std::cell::RefCell<Maps>);

/// The maps of indexes in memory, and what was done to them, which tests look at.
#[cfg(test)]
#[derive(Debug, Default)]
struct Maps {
	/// For each word, the statements whose text holds it.
	words: BTreeMap<String, SmallSet<StatementId>>,
	/// For each field and hash of a value in canonical CBOR, the statements that hold it.
	values: BTreeMap<(FieldName, Hash), SmallSet<StatementId>>,
	/// The indexed statements that show under each entity.
	shown: BTreeMap<EntityId, SmallSet<StatementId>>,
	/// For each name, lowercased, the statements that hold it.
	names: BTreeMap<String, SmallSet<StatementId>>,
	/// What is indexed of each statement, so that it can be taken out again.
	indexed: BTreeMap<StatementId, Indexed>,
	/// The statements indexed or taken out so far.
	touched: BTreeSet<StatementId>,
	/// How often the indexes were emptied.
	cleared: usize,
}

/// Two sets of indexes are equal when they hold the same, whatever was done to them.
#[cfg(test)]
impl PartialEq for Maps {
	fn eq(&self, other: &Self) -> bool {
		self.words == other.words
			&& self.values == other.values
			&& self.shown == other.shown
			&& self.names == other.names
			&& self.indexed == other.indexed
	}
}

#[cfg(test)]
impl Eq for Maps {}

#[cfg(test)]
impl Postings for MemoryPostings {
	type Error = std::convert::Infallible;
	type Key = StatementId;

	fn indexed(&self, id: StatementId) -> Result<Option<Indexed>, Self::Error> {
		Ok(self.0.borrow().indexed.get(&id).cloned())
	}

	fn insert(&self, id: StatementId, indexed: &Indexed) -> Result<(), Self::Error> {
		Postings::remove(self, id)?;
		let mut maps = self.0.borrow_mut();
		maps.touched.insert(id);
		for word in &indexed.words {
			maps.words.entry(word.clone()).or_default().insert(id);
		}
		maps.values
			.entry(indexed.value.clone())
			.or_default()
			.insert(id);
		maps.shown.entry(indexed.shown).or_default().insert(id);
		if let Some(name) = &indexed.name {
			maps.names.entry(name.clone()).or_default().insert(id);
		}
		maps.indexed.insert(id, indexed.clone());
		Ok(())
	}

	fn remove(&self, id: StatementId) -> Result<(), Self::Error> {
		let mut maps = self.0.borrow_mut();
		let Some(indexed) = maps.indexed.remove(&id) else {
			return Ok(());
		};
		maps.touched.insert(id);
		for word in &indexed.words {
			remove_from(&mut maps.words, word, id);
		}
		remove_from(&mut maps.values, &indexed.value, id);
		remove_from(&mut maps.shown, &indexed.shown, id);
		if let Some(name) = &indexed.name {
			remove_from(&mut maps.names, name, id);
		}
		Ok(())
	}

	fn clear(&self) -> Result<(), Self::Error> {
		let mut maps = self.0.borrow_mut();
		*maps = Maps {
			touched: std::mem::take(&mut maps.touched),
			cleared: maps.cleared + 1,
			..Maps::default()
		};
		Ok(())
	}

	fn with_word(&self, word: &str) -> Result<Vec<(Self::Key, EntityId)>, Self::Error> {
		let maps = self.0.borrow();
		Ok(maps
			.words
			.get(word)
			.into_iter()
			.flatten()
			.filter(|id| !maps.indexed[id].deleted)
			.map(|id| (*id, maps.indexed[id].shown))
			.collect())
	}

	fn word_count(&self, word: &str) -> Result<usize, Self::Error> {
		Ok(self.0.borrow().words.get(word).map_or(0, SmallSet::len))
	}

	fn with_prefix(&self, prefix: &str) -> Result<Vec<(Self::Key, EntityId)>, Self::Error> {
		let maps = self.0.borrow();
		let ids: BTreeSet<StatementId> = maps
			.words
			.range(prefix.to_owned()..)
			.take_while(|(word, _)| word.starts_with(prefix))
			.flat_map(|(_, ids)| ids.iter().copied())
			.collect();
		Ok(ids
			.into_iter()
			.filter(|id| !maps.indexed[id].deleted)
			.map(|id| (id, maps.indexed[&id].shown))
			.collect())
	}

	fn prefix_count(&self, prefix: &str, at_most: usize) -> Result<usize, Self::Error> {
		let count: usize = self
			.0
			.borrow()
			.words
			.range(prefix.to_owned()..)
			.take_while(|(word, _)| word.starts_with(prefix))
			.map(|(_, ids)| ids.len())
			.sum();
		Ok(count.min(at_most))
	}

	fn shown_with_prefix(
		&self,
		entity: EntityId,
		prefix: &str,
	) -> Result<Vec<Self::Key>, Self::Error> {
		let maps = self.0.borrow();
		Ok(maps
			.shown
			.get(&entity)
			.into_iter()
			.flatten()
			.filter(|id| {
				maps.indexed[id]
					.words
					.iter()
					.any(|word| word.starts_with(prefix))
			})
			.copied()
			.collect())
	}

	fn with_value(&self, field: &FieldName, value: &Hash) -> Result<Vec<EntityId>, Self::Error> {
		let maps = self.0.borrow();
		Ok(maps
			.values
			.get(&(field.clone(), *value))
			.into_iter()
			.flatten()
			.filter(|id| !maps.indexed[id].deleted)
			.map(|id| maps.indexed[id].shown)
			.collect())
	}

	fn with_name(&self, lowered: &str) -> Result<Vec<EntityId>, Self::Error> {
		let maps = self.0.borrow();
		let entities: BTreeSet<EntityId> = maps
			.names
			.get(lowered)
			.into_iter()
			.flatten()
			.map(|id| maps.indexed[id].shown)
			.collect();
		Ok(entities.into_iter().collect())
	}

	fn of_shown(&self, entity: EntityId) -> Result<Vec<(StatementId, Indexed)>, Self::Error> {
		let maps = self.0.borrow();
		Ok(maps
			.shown
			.get(&entity)
			.into_iter()
			.flatten()
			.map(|id| (*id, maps.indexed[id].clone()))
			.collect())
	}

	fn remove_field(&self, field: &FieldName) -> Result<(), Self::Error> {
		let ids: Vec<StatementId> = self
			.0
			.borrow()
			.indexed
			.iter()
			.filter(|(_, indexed)| indexed.value.0 == *field)
			.map(|(id, _)| *id)
			.collect();
		for id in ids {
			self.remove(id)?;
		}
		Ok(())
	}

	fn set_deleted(&self, entity: EntityId, deleted: bool) -> Result<(), Self::Error> {
		let mut maps = self.0.borrow_mut();
		let ids: Vec<StatementId> = maps
			.shown
			.get(&entity)
			.into_iter()
			.flatten()
			.copied()
			.collect();
		for id in ids {
			if let Some(indexed) = maps.indexed.get_mut(&id) {
				indexed.deleted = deleted;
			}
		}
		Ok(())
	}
}

/// Takes a statement out of the set of a key, and the key out with its last statement.
#[cfg(test)]
fn remove_from<K: Ord>(map: &mut BTreeMap<K, SmallSet<StatementId>>, key: &K, id: StatementId) {
	if let Some(ids) = map.get_mut(key) {
		ids.remove(&id);
		if ids.is_empty() {
			map.remove(key);
		}
	}
}

/// Whether a text holds every word of `query` as a search compares them: without regard to
/// case or accents, each a whole word of the text, but the last, which may also start a
/// longer one. A query without words is held by no text.
#[must_use]
pub fn holds_words(text: &str, query: &str) -> bool {
	let wanted = words(query);
	let Some((last, whole)) = wanted.split_last() else {
		return false;
	};
	let held = words(text);
	whole.iter().all(|word| held.contains(word))
		&& held.iter().any(|word| word.starts_with(last.as_str()))
}

/// The words of a text, compared without regard to case or accents: decomposed, so that
/// characters such as `㎒` become letters, lowercased, without combining marks, and split
/// at every character that is not a letter or a digit.
fn words(text: &str) -> Vec<String> {
	let folded: String = text
		.nfkd()
		.flat_map(char::to_lowercase)
		.collect::<String>()
		.nfkd()
		.filter(|&character| !is_combining_mark(character))
		.collect();
	folded
		.split(|character: char| !character.is_alphanumeric())
		.filter(|word| !word.is_empty())
		.map(str::to_owned)
		.collect()
}

#[cfg(test)]
mod tests {
	use std::fs;

	use nibkeep_crypto::SigningKey;
	use nibkeep_log::{Log, Parcel, Received, SpaceKeys};
	use nibkeep_model::storage::Infallibly;
	use nibkeep_testkit::{TestRng, millis_at};
	use nibkeep_time::Clock;
	use proptest::prelude::*;

	use super::*;
	use crate::StateFile;

	const DRIFT: u64 = 3_600_000;

	const WORDS: [&str; 6] = ["ada", "adam", "lovelace", "Café", "cafe", "zoë"];

	/// What one of the writers does: asserts a text of some words on one of a few entities,
	/// replacing some statements, or retracts some. Statements are named by the order in
	/// which they were asserted, by any writer, so writers can supersede each other's
	/// statements, also before those arrive.
	#[derive(Clone, Debug)]
	enum Step {
		/// Asserts a text on a field of an entity, under a new statement ID or, rarely,
		/// under the ID of an earlier statement, as two replicas could by chance.
		Assert {
			writer: usize,
			entity: usize,
			field: usize,
			words: Vec<usize>,
			replaces: Vec<usize>,
			reuses: Option<usize>,
		},
		Retract {
			writer: usize,
			statements: Vec<usize>,
		},
		/// Merges one entity into another.
		Merge {
			writer: usize,
			entity: usize,
			into: usize,
		},
		/// Deletes an entity, which retracting the statement restores.
		Delete { writer: usize, entity: usize },
	}

	fn step() -> impl Strategy<Value = Step> {
		prop_oneof![
			6 => (
				0..3_usize,
				0..3_usize,
				0..2_usize,
				prop::collection::vec(0..WORDS.len(), 1..3),
				prop::collection::vec(0..16_usize, 0..2),
				prop::option::weighted(0.1, 0..16_usize),
			)
				.prop_map(|(writer, entity, field, words, replaces, reuses)| Step::Assert {
					writer,
					entity,
					field,
					words,
					replaces,
					reuses,
				}),
			2 => (0..3_usize, prop::collection::vec(0..16_usize, 1..3))
				.prop_map(|(writer, statements)| Step::Retract { writer, statements }),
			1 => (0..3_usize, 0..3_usize, 0..3_usize)
				.prop_map(|(writer, entity, into)| Step::Merge { writer, entity, into }),
			1 => (0..3_usize, 0..3_usize)
				.prop_map(|(writer, entity)| Step::Delete { writer, entity }),
		]
	}

	/// Shuffles changes with a seed, so that a reader receives them in another order.
	fn shuffle(changes: &mut [Parcel], mut seed: u64) {
		for last in (1..changes.len()).rev() {
			seed ^= seed << 13;
			seed ^= seed >> 7;
			seed ^= seed << 17;
			let other = usize::try_from(seed % (last as u64 + 1)).expect("small");
			changes.swap(last, other);
		}
	}

	/// A search that looks at every current statement, to compare the index with: of a
	/// field whose last writer wins, at the one written last.
	fn every_statement(state: &SpaceState, fields: Fields<'_>, query: &str) -> Vec<Found> {
		let words = words(query);
		let Some(last) = words.len().checked_sub(1) else {
			return Vec::new();
		};
		let mut entities: BTreeMap<EntityId, (Vec<bool>, BTreeSet<StatementId>)> = BTreeMap::new();
		for statement in state.statements().infallible() {
			let Value::Text(text) = &statement.value else {
				continue;
			};
			if fields::is_reserved(&statement.field) {
				continue;
			}
			if fields.last_writer_wins.contains(&statement.field)
				&& state
					.last_written(statement.entity, &statement.field)
					.infallible()
					.is_none_or(|last| last.id != statement.id)
			{
				continue;
			}
			let held = self::words(text);
			let (matched, statements) = entities
				.entry(state.canonical(statement.entity).infallible())
				.or_insert_with(|| (vec![false; words.len()], BTreeSet::new()));
			for (index, word) in words.iter().enumerate() {
				if held
					.iter()
					.any(|held| held == word || (index == last && held.starts_with(word.as_str())))
				{
					matched[index] = true;
					statements.insert(statement.id);
				}
			}
		}
		let mut found: Vec<Found> = entities
			.into_iter()
			.filter(|(entity, (matched, _))| {
				matched.iter().all(|&matched| matched) && !state.is_deleted(*entity).infallible()
			})
			.map(|(entity, (_, statements))| Found {
				entity,
				matching: statements.len(),
			})
			.collect();
		found.sort_by(|a, b| b.matching.cmp(&a.matching).then(a.entity.cmp(&b.entity)));
		found
	}

	/// The entities that a name finds, looked for in every current statement: of a field
	/// whose last writer wins, in the one written last.
	fn every_name(state: &SpaceState, fields: Fields<'_>, lowered: &str) -> Vec<EntityId> {
		let mut named = BTreeSet::new();
		for statement in state.statements().infallible() {
			if fields::name_key(&statement.field, &statement.value).as_deref() == Some(lowered)
				&& (!fields.last_writer_wins.contains(&statement.field)
					|| state
						.last_written(statement.entity, &statement.field)
						.infallible()
						.is_some_and(|last| last.id == statement.id))
			{
				named.insert(state.canonical(statement.entity).infallible());
			}
		}
		named.into_iter().collect()
	}

	/// A writer of a space, whose changes postings take in as it commits them.
	struct Writer {
		log: Log,
		author: SigningKey,
		keys: SpaceKeys,
		wall: u64,
		rng: TestRng,
	}

	impl Writer {
		fn new(name: &str) -> Self {
			let mut rng = TestRng::new(name);
			Self {
				log: Log::new(rng.space_id(), Clock::new(DRIFT)),
				author: rng.signing_key(),
				keys: SpaceKeys::new(0, rng.key()),
				wall: millis_at("2026-10-12", 10, 0),
				rng,
			}
		}

		/// A statement of an entity's field, with a new ID.
		fn statement(&mut self, entity: EntityId, field: &FieldName, value: Value) -> Statement {
			Statement::new(self.rng.statement_id(), entity, field.clone(), value)
		}

		/// Commits an operation, a second after the one before, and takes it into `postings`.
		fn commit<P: Postings<Error = std::convert::Infallible>>(
			&mut self,
			postings: &P,
			fields: Fields<'_>,
			operation: Operation,
		) {
			self.wall += 1_000;
			self.log
				.commit(
					&self.author,
					&self.keys,
					vec![operation],
					self.wall,
					&mut self.rng,
				)
				.expect("commits");
			for (_, payload) in self.log.take_applied() {
				apply(&self.log, postings, fields, payload.all_operations()).infallible();
			}
		}
	}

	#[test]
	fn a_merge_moves_only_the_statements_of_the_entity_merged() {
		let mut writer = Writer::new("a merge moves statements");
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let (secret, last_writer_wins) = (BTreeSet::new(), BTreeSet::new());
		let fields = Fields {
			secret: &secret,
			last_writer_wins: &last_writer_wins,
		};
		let index = MemoryPostings::default();
		let [apart, from, into] = [
			writer.rng.entity_id(),
			writer.rng.entity_id(),
			writer.rng.entity_id(),
		];
		let mut titles = BTreeMap::new();
		for (entity, text) in [(apart, "Zoë"), (from, "Ada"), (into, "Lovelace")] {
			let statement = writer.statement(entity, &title, Value::Text(text.to_owned()));
			titles.insert(entity, statement.id);
			writer.commit(&index, fields, Operation::assert(statement));
		}
		index.0.borrow_mut().touched.clear();
		let merge = writer.statement(from, &fields::MERGED_INTO, Value::Reference(into));
		let merged = merge.id;
		writer.commit(&index, fields, Operation::assert(merge));
		{
			let maps = index.0.borrow();
			assert_eq!(maps.cleared, 0, "nothing is built anew");
			assert!(maps.touched.contains(&titles[&from]) && maps.touched.contains(&merged));
			assert!(
				!maps.touched.contains(&titles[&into]) && !maps.touched.contains(&titles[&apart]),
				"the entity merged into, and an entity apart, are left alone"
			);
		}
		let built = MemoryPostings::default();
		build(&writer.log, &built, fields).infallible();
		assert_eq!(index, built);
		assert_eq!(
			search(&index, "ada").infallible(),
			vec![Found {
				entity: into,
				matching: 1,
			}]
		);
	}

	#[test]
	fn a_merge_shows_the_value_written_last_among_the_entities_merged() {
		let mut writer = Writer::new("a merge and the last writer");
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let secret = BTreeSet::new();
		let last_writer_wins = BTreeSet::from([title.clone()]);
		let fields = Fields {
			secret: &secret,
			last_writer_wins: &last_writer_wins,
		};
		let index = MemoryPostings::default();
		let [from, into] = [writer.rng.entity_id(), writer.rng.entity_id()];
		let mut titles = BTreeMap::new();
		// The title of the entity merged into is written last.
		for (entity, text) in [(from, "Ada"), (into, "Lovelace")] {
			let statement = writer.statement(entity, &title, Value::Text(text.to_owned()));
			titles.insert(entity, statement.id);
			writer.commit(&index, fields, Operation::assert(statement));
		}
		let merge = writer.statement(from, &fields::MERGED_INTO, Value::Reference(into));
		let merged = merge.id;
		writer.commit(&index, fields, Operation::assert(merge));
		let built = MemoryPostings::default();
		build(&writer.log, &built, fields).infallible();
		assert_eq!(index, built);
		nibkeep_testkit::assert_empty!(search(&index, "ada").infallible());
		assert_eq!(
			search(&index, "lovelace").infallible(),
			vec![Found {
				entity: into,
				matching: 1,
			}]
		);
		// Undone, each shows its own title again.
		writer.commit(&index, fields, Operation::retract(from, [merged]));
		let built = MemoryPostings::default();
		build(&writer.log, &built, fields).infallible();
		assert_eq!(index, built);
		assert_eq!(
			search(&index, "ada").infallible(),
			vec![Found {
				entity: from,
				matching: 1,
			}]
		);
	}

	/// Postings that count how often each word's and each beginning's statements are read,
	/// and each candidate's own statements.
	#[derive(Default)]
	struct Counting {
		postings: MemoryPostings,
		prefixes_read: std::cell::Cell<usize>,
		candidates_checked: std::cell::Cell<usize>,
	}

	impl Postings for Counting {
		type Error = std::convert::Infallible;
		type Key = StatementId;

		fn indexed(&self, id: StatementId) -> Result<Option<Indexed>, Self::Error> {
			self.postings.indexed(id)
		}
		fn insert(&self, id: StatementId, indexed: &Indexed) -> Result<(), Self::Error> {
			self.postings.insert(id, indexed)
		}
		fn remove(&self, id: StatementId) -> Result<(), Self::Error> {
			self.postings.remove(id)
		}
		fn clear(&self) -> Result<(), Self::Error> {
			self.postings.clear()
		}
		fn with_word(&self, word: &str) -> Result<Vec<(StatementId, EntityId)>, Self::Error> {
			self.postings.with_word(word)
		}
		fn word_count(&self, word: &str) -> Result<usize, Self::Error> {
			self.postings.word_count(word)
		}
		fn with_prefix(&self, prefix: &str) -> Result<Vec<(StatementId, EntityId)>, Self::Error> {
			self.prefixes_read.set(self.prefixes_read.get() + 1);
			self.postings.with_prefix(prefix)
		}
		fn prefix_count(&self, prefix: &str, at_most: usize) -> Result<usize, Self::Error> {
			self.postings.prefix_count(prefix, at_most)
		}
		fn shown_with_prefix(
			&self,
			entity: EntityId,
			prefix: &str,
		) -> Result<Vec<StatementId>, Self::Error> {
			self.candidates_checked
				.set(self.candidates_checked.get() + 1);
			self.postings.shown_with_prefix(entity, prefix)
		}
		fn with_value(
			&self,
			field: &FieldName,
			value: &Hash,
		) -> Result<Vec<EntityId>, Self::Error> {
			self.postings.with_value(field, value)
		}
		fn with_name(&self, lowered: &str) -> Result<Vec<EntityId>, Self::Error> {
			self.postings.with_name(lowered)
		}
		fn of_shown(&self, entity: EntityId) -> Result<Vec<(StatementId, Indexed)>, Self::Error> {
			self.postings.of_shown(entity)
		}
		fn set_deleted(&self, entity: EntityId, deleted: bool) -> Result<(), Self::Error> {
			self.postings.set_deleted(entity, deleted)
		}
		fn remove_field(&self, field: &FieldName) -> Result<(), Self::Error> {
			self.postings.remove_field(field)
		}
	}

	#[test]
	fn the_last_word_is_read_unless_it_starts_many_more_statements_than_the_rarest_word() {
		let mut writer = Writer::new("the last word is read");
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let (secret, last_writer_wins) = (BTreeSet::new(), BTreeSet::new());
		let fields = Fields {
			secret: &secret,
			last_writer_wins: &last_writer_wins,
		};
		let index = Counting::default();
		// Two entities hold "lovelace", one of them "ada" too; three more hold "adam", and ten
		// "adler".
		let texts = ["Ada Lovelace", "Lovelace"]
			.into_iter()
			.chain(std::iter::repeat_n("Adam", 3))
			.chain(std::iter::repeat_n("Adler", 10));
		for text in texts {
			let entity = writer.rng.entity_id();
			let statement = writer.statement(entity, &title, Value::Text(text.to_owned()));
			writer.commit(&index, fields, Operation::assert(statement));
		}
		// Two statements hold the rarest whole word, so a beginning that starts at most eight
		// is read: "ada" starts four, "ad" fourteen.
		for (query, read) in [("lovelace ada", true), ("lovelace ad", false)] {
			index.prefixes_read.set(0);
			index.candidates_checked.set(0);
			let found = search(&index, query).infallible();
			assert_eq!(found, every_statement(writer.log.state(), fields, query));
			assert_eq!(found.len(), 1, "{query}");
			assert_eq!(index.prefixes_read.get(), usize::from(read), "{query}");
			assert_eq!(
				index.candidates_checked.get(),
				if read { 0 } else { 2 },
				"{query}: each entity that holds the whole word is checked"
			);
		}
	}

	#[test]
	fn a_text_holds_the_words_of_a_query_the_last_as_a_beginning() {
		let path = "/home/anna/Pictures/Été 2019/IMG_0042.JPG on Anna's laptop";
		assert!(holds_words(path, "ete img_0042"));
		assert!(holds_words(path, "pictures 2019 lap"));
		assert!(holds_words(path, "anna"));
		// Whole words but the last.
		assert!(!holds_words(path, "pict 2019"));
		assert!(!holds_words(path, "pictures phone"));
		assert!(!holds_words(path, " ,. "));
	}

	#[test]
	fn words_are_compared_without_case_or_accents() {
		assert_eq!(words("Café Bleu, ZOË"), ["cafe", "bleu", "zoe"]);
		assert_eq!(words("Radio 5㎒ İstanbul"), ["radio", "5mhz", "istanbul"]);
		assert_eq!(words("netflix.com/ada"), ["netflix", "com", "ada"]);
		nibkeep_testkit::assert_empty!(words(" -- "));
	}

	/// Where two indexes differ: the statements indexed in one and not the other, or
	/// differently, for a report.
	fn differences(index: &MemoryPostings, built: &MemoryPostings) -> String {
		use std::fmt::Write as _;

		let (index, built) = (index.0.borrow(), built.0.borrow());
		let mut report = String::new();
		for id in index
			.indexed
			.keys()
			.chain(built.indexed.keys())
			.collect::<BTreeSet<_>>()
		{
			let (kept, expected) = (index.indexed.get(id), built.indexed.get(id));
			if kept != expected {
				let _ = write!(report, "\n{id}: indexed as {kept:?}, built as {expected:?}");
			}
		}
		report
	}

	/// Receives changes in order, keeping an index of the state up to date and checking it
	/// against one built anew.
	fn read(
		log: &mut Log,
		index: &MemoryPostings,
		fields: Fields<'_>,
		keys: &SpaceKeys,
		changes: Vec<Parcel>,
		wall: u64,
	) {
		for change in changes {
			let received = log.receive(change, keys, wall).expect("accepts");
			if let Received::Accepted(_) = received {
				for (_, payload) in log.take_applied() {
					apply(log, index, fields, payload.all_operations()).infallible();
				}
				// Several changes can apply at once, and the index takes them in one by one
				// against the state that holds them all; after each batch it is exact.
				let built = MemoryPostings::default();
				build(log, &built, fields).infallible();
				assert!(
					*index == built,
					"the index follows the log{}",
					differences(index, &built)
				);
			}
		}
	}

	/// The changes that three writers make through `steps` in a space, and the time of the
	/// last: statements are named by the order in which they were asserted, by any writer,
	/// merges among them since 2026-10-24, so the regression cases saved before then replay
	/// other steps than they were saved for.
	fn written(
		steps: Vec<Step>,
		space: SpaceId,
		keys: &SpaceKeys,
		entities: &[EntityId],
		rng: &mut TestRng,
	) -> (Vec<Parcel>, u64) {
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let tag: FieldName = "org.nibkeep.tag".parse().expect("a field name");
		let mut writers: Vec<(SigningKey, Log)> = (0..3)
			.map(|_| (rng.signing_key(), Log::new(space, Clock::new(DRIFT))))
			.collect();
		let mut wall = millis_at("2026-10-12", 10, 0);
		let mut asserted: Vec<StatementId> = Vec::new();
		let mut of_entity: BTreeMap<StatementId, EntityId> = BTreeMap::new();
		let mut changes = Vec::new();
		for step in steps {
			let (writer, operation) = match step {
				Step::Assert {
					writer,
					entity,
					field,
					words,
					replaces,
					reuses,
				} => {
					let text: Vec<&str> = words.iter().map(|&word| WORDS[word]).collect();
					let replaced: Vec<StatementId> = replaces
						.iter()
						.filter_map(|&index| asserted.get(index).copied())
						.collect();
					let id = match reuses.and_then(|index| asserted.get(index)) {
						Some(&id) => id,
						None => rng.statement_id(),
					};
					asserted.push(id);
					of_entity.insert(id, entities[entity]);
					let statement = Statement::new(
						id,
						entities[entity],
						if field == 0 {
							title.clone()
						} else {
							tag.clone()
						},
						Value::Text(text.join(" ")),
					);
					(writer, Operation::replace(statement, replaced))
				}
				Step::Merge {
					writer,
					entity,
					into,
				} => {
					// A merge can be retracted and replaced, as the others can.
					let id = rng.statement_id();
					asserted.push(id);
					of_entity.insert(id, entities[entity]);
					let statement = Statement::new(
						id,
						entities[entity],
						fields::MERGED_INTO,
						Value::Reference(entities[into]),
					);
					(writer, Operation::assert(statement))
				}
				Step::Delete { writer, entity } => {
					let id = rng.statement_id();
					asserted.push(id);
					of_entity.insert(id, entities[entity]);
					let statement =
						Statement::new(id, entities[entity], fields::DELETED, Value::Boolean(true));
					(writer, Operation::assert(statement))
				}
				Step::Retract { writer, statements } => {
					let ids: Vec<StatementId> = statements
						.iter()
						.filter_map(|&index| asserted.get(index).copied())
						.collect();
					// A retraction names the entity of its first statement: those of others it
					// names are not superseded.
					let entity = ids
						.first()
						.and_then(|id| of_entity.get(id))
						.copied()
						.unwrap_or(entities[0]);
					(writer, Operation::retract(entity, ids))
				}
			};
			wall += 1_000;
			let (author, log) = &mut writers[writer];
			// A writer's own log may refuse a step, such as an ID it holds already.
			if let Ok(change) = log.commit(author, keys, vec![operation], wall, rng) {
				changes.push(change);
			}
		}
		(changes, wall)
	}

	/// Checks that an index kept up to date through `steps`, received in order and in the
	/// order that `seed` shuffles them into, equals one built anew, and that searches and
	/// names answer as looking at every statement does.
	fn follows(steps: Vec<Step>, seed: u64, last_writer_wins: bool) -> Result<(), TestCaseError> {
		let mut rng = TestRng::new("the index follows changes");
		let space = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let entities: Vec<EntityId> = (0..3).map(|_| rng.entity_id()).collect();
		let secret = BTreeSet::new();
		let last_writer_wins: BTreeSet<FieldName> = last_writer_wins
			.then(|| "org.nibkeep.title".parse().expect("a field name"))
			.into_iter()
			.collect();
		let fields = Fields {
			secret: &secret,
			last_writer_wins: &last_writer_wins,
		};
		let (changes, wall) = written(steps, space, &keys, &entities, &mut rng);
		let mut in_order = (
			Log::new(space, Clock::new(DRIFT)),
			MemoryPostings::default(),
		);
		read(
			&mut in_order.0,
			&in_order.1,
			fields,
			&keys,
			changes.clone(),
			wall,
		);
		let for_state_file = changes.clone();
		let mut shuffled = changes;
		shuffle(&mut shuffled, seed);
		let mut out_of_order = (
			Log::new(space, Clock::new(DRIFT)),
			MemoryPostings::default(),
		);
		read(
			&mut out_of_order.0,
			&out_of_order.1,
			fields,
			&keys,
			shuffled,
			wall,
		);

		let built = MemoryPostings::default();
		build(&in_order.0, &built, fields).infallible();
		prop_assert_eq!(&in_order.1, &built);
		prop_assert_eq!(&out_of_order.1, &built);
		for query in [
			"ada",
			"ad",
			"cafe",
			"zoe lovelace",
			"CAF",
			"ada a",
			"lovelace ad",
		] {
			let expected = every_statement(in_order.0.state(), fields, query);
			prop_assert_eq!(search(&in_order.1, query).infallible(), expected.clone());
			prop_assert_eq!(search(&out_of_order.1, query).infallible(), expected);
		}
		for name in ["ada", "ada lovelace", "café", "cafe zoë", "lovelace ada"] {
			let expected = every_name(in_order.0.state(), fields, name);
			prop_assert_eq!(in_order.1.with_name(name).infallible(), expected.clone());
			prop_assert_eq!(out_of_order.1.with_name(name).infallible(), expected);
		}
		let received = (space, &keys, for_state_file, wall);
		same_postings(received, &built, fields, &entities)
	}

	/// What postings in memory give, `found`, with each statement's number in a state file
	/// in place of its ID, sorted: the state file tells statements apart by those numbers.
	fn numbered<T: Ord>(state: &StateFile, found: Vec<(StatementId, T)>) -> Vec<(i64, T)> {
		sorted(
			found
				.into_iter()
				.map(|(id, value)| {
					let number = state.statement_number(id).expect("reads");
					(number.expect("held"), value)
				})
				.collect(),
		)
	}

	fn sorted<T: Ord>(mut found: Vec<T>) -> Vec<T> {
		found.sort();
		found
	}

	/// Each case of a state file gets a directory of its own.
	static STATE_FILES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

	/// Checks that the indexes that a state file builds of a log that has received
	/// `changes`, in one transaction, answer as those built in memory do, `built`. The file
	/// is in a directory of its own, removed once it is closed.
	fn same_postings(
		(space, keys, changes, wall): (SpaceId, &SpaceKeys, Vec<Parcel>, u64),
		built: &MemoryPostings,
		fields: Fields<'_>,
		entities: &[EntityId],
	) -> Result<(), TestCaseError> {
		let case = STATE_FILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-index-state-file-{}-{case}", std::process::id()),
		);
		let mut rng = TestRng::new("an index in a state file");
		let state = StateFile::open(&directory.join("state"), &rng.key(), rng).expect("opens");
		let mut log = Log::with_storage(space, Clock::new(DRIFT), state);
		log.storage_mut().begin().expect("begins");
		for change in changes {
			log.receive(change, keys, wall).expect("accepts");
			let _ = log.take_applied();
		}
		let postings = log.state().storage().ordinary();
		build(&log, postings, fields).expect("builds");
		let checked = (|| {
			for query in ["ada", "ad", "cafe", "zoe lovelace", "CAF", "ada a"] {
				prop_assert_eq!(
					search(postings, query).expect("searches"),
					search(built, query).infallible()
				);
			}
			for name in ["ada", "ada lovelace", "café", "lovelace ada"] {
				prop_assert_eq!(
					postings.with_name(name).expect("finds"),
					built.with_name(name).infallible()
				);
			}
			for word in WORDS.iter().flat_map(|word| words(word)) {
				prop_assert_eq!(
					sorted(postings.with_word(&word).expect("finds")),
					numbered(postings, built.with_word(&word).infallible())
				);
			}
			for prefix in ["a", "ad", "caf", "zoe", "lovelaces", ""] {
				prop_assert_eq!(
					sorted(postings.with_prefix(prefix).expect("finds")),
					numbered(postings, built.with_prefix(prefix).infallible())
				);
				for at_most in [0, 1, 3, usize::MAX] {
					prop_assert_eq!(
						postings.prefix_count(prefix, at_most).expect("counts"),
						built.prefix_count(prefix, at_most).infallible()
					);
				}
			}
			for &entity in entities {
				for prefix in ["a", "ad", "caf", "zoe", "lovelaces", ""] {
					let shown = |found: Vec<StatementId>| found.into_iter().map(|id| (id, ()));
					prop_assert_eq!(
						sorted(postings.shown_with_prefix(entity, prefix).expect("finds")),
						numbered(
							postings,
							shown(built.shown_with_prefix(entity, prefix).infallible()).collect()
						)
						.into_iter()
						.map(|(number, ())| number)
						.collect::<Vec<_>>()
					);
				}
				let sorted = |mut found: Vec<(StatementId, Indexed)>| {
					found.sort_by_key(|(id, _)| *id);
					found
				};
				prop_assert_eq!(
					sorted(postings.of_shown(entity).expect("finds")),
					sorted(built.of_shown(entity).infallible())
				);
			}
			Ok(())
		})();
		// Closed first: Windows does not delete a file that is open.
		drop(log);
		let _ = fs::remove_dir_all(directory);
		checked
	}

	/// The special fields of a case: the title's last writer wins, or not, and the tag is
	/// secret, or not.
	fn special(title_wins: bool, tag_secret: bool) -> (BTreeSet<FieldName>, BTreeSet<FieldName>) {
		let title: FieldName = "org.nibkeep.title".parse().expect("a field name");
		let tag: FieldName = "org.nibkeep.tag".parse().expect("a field name");
		(
			tag_secret.then_some(tag).into_iter().collect(),
			title_wins.then_some(title).into_iter().collect(),
		)
	}

	/// Checks that indexes built with one set of special fields and brought to another equal
	/// indexes built anew with the other, and back.
	fn follows_fields(
		steps: Vec<Step>,
		(was_wins, was_secret): (bool, bool),
		(now_wins, now_secret): (bool, bool),
	) -> Result<(), TestCaseError> {
		let mut rng = TestRng::new("the indexes follow the fields");
		let space = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let entities: Vec<EntityId> = (0..3).map(|_| rng.entity_id()).collect();
		let (was, now) = (special(was_wins, was_secret), special(now_wins, now_secret));
		let was = Fields {
			secret: &was.0,
			last_writer_wins: &was.1,
		};
		let now = Fields {
			secret: &now.0,
			last_writer_wins: &now.1,
		};
		let (changes, wall) = written(steps, space, &keys, &entities, &mut rng);
		let mut log = Log::new(space, Clock::new(DRIFT));
		let index = MemoryPostings::default();
		read(&mut log, &index, was, &keys, changes, wall);
		for (from, to) in [(was, now), (now, was)] {
			rebuild_fields(&log, &index, from, to).infallible();
			let built = MemoryPostings::default();
			build(&log, &built, to).infallible();
			prop_assert_eq!(
				&index,
				&built,
				"from {:?} to {:?}",
				from.last_writer_wins,
				to.last_writer_wins
			);
		}
		Ok(())
	}

	proptest! {
		// Every case makes and receives signed, encrypted changes, so fewer cases are run.
		#![proptest_config(ProptestConfig::with_cases(64))]

		#[test]
		fn the_index_follows_changes_in_any_order(
			steps in prop::collection::vec(step(), 1..16),
			seed in 1..u64::MAX,
			last_writer_wins in any::<bool>(),
		) {
			follows(steps, seed, last_writer_wins)?;
		}

		#[test]
		fn the_indexes_follow_the_fields(
			steps in prop::collection::vec(step(), 1..16),
			was in (any::<bool>(), any::<bool>()),
			now in (any::<bool>(), any::<bool>()),
		) {
			follows_fields(steps, was, now)?;
		}
	}
}
