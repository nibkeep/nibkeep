//! The entities of a library's spaces: created, shown, listed and found.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{LINKED_PERSON, NAME, USER};
use nibkeep_model::schema::{FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::storage::{Layers, Memory, Recorded, Statements};
use nibkeep_model::{
	FieldDef, FieldName, Languages, Multiplicity, Operation, Qualifiers, Resolution, State,
	Statement, Tag, TypeName, Types, Value, content, fields, presence_on,
};

use nibkeep_store::{StateFile, StoredLog};
use nibkeep_time::{Day, Edtf, Presence, Timestamp};

use super::{Library, policies};
use crate::NodeError;

/// The shortest prefix of an ID that finds an entity: six characters leave few entities
/// of a large library to choose from.
pub(crate) const SHORTEST_PREFIX: usize = 6;

/// An entity as a person sees it: its name, types, and the values of its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityView {
	/// The entity, or of entities merged together, the one they were merged into.
	pub id: EntityId,
	/// The name to show for it, if it has one.
	pub name: Option<String>,
	/// Its types.
	pub types: Vec<TypeName>,
	/// Whether it is deleted.
	pub deleted: bool,
	/// The fields with values, ordered by name, without the fields of its types and of its
	/// own record.
	pub fields: Vec<FieldView>,
}

impl EntityView {
	/// The values that a field shows, in the order to show them; none if it shows none.
	#[must_use]
	pub fn values(&self, field: &FieldName) -> &[ShownValue] {
		self.fields
			.iter()
			.find(|shown| &shown.field == field)
			.map_or(&[], |shown| shown.values.as_slice())
	}
}

/// A field of an entity, and its values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldView {
	/// The field.
	pub field: FieldName,
	/// The values to show, in the order to show them (`docs/spec/model.md`, section 6).
	pub values: Vec<ShownValue>,
	/// Whether the values compete: several values of a field with one value, until a person
	/// chooses.
	pub competing: bool,
}

/// A value as it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownValue {
	/// The statement that holds it.
	pub statement: StatementId,
	/// The value.
	pub value: Value,
	/// When it was true, if its valid time can be read: a date for something that happened,
	/// an interval for a state.
	pub valid_time: Option<Edtf>,
	/// Whether it certainly holds on the day shown, only possibly holds there, or has no
	/// date that can be read, and so shows on every day.
	pub presence: Presence,
	/// Whether it has a valid time that this version cannot read.
	pub unreadable_date: bool,
	/// Its language tag, as written, if it has one.
	pub language: Option<String>,
	/// How many other forms the value has, in other languages or scripts.
	pub other_forms: usize,
	/// The other forms, ordered by the IDs of their statements.
	pub forms: Vec<OtherForm>,
}

/// Another form of a value shown, in another language or script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherForm {
	/// The form.
	pub value: Value,
	/// Its language tag, as written, if it has one.
	pub language: Option<String>,
}

/// The time of an edit or a view: the day a person sees, and when they write.
///
/// A field shows its values as of [`Dating::day`]. Setting a field replaces what it shows
/// on that day, and keeps what belongs to other days. Values written get
/// [`Dating::valid_time`] if a person gave one, and otherwise, where a field with one value
/// has dated values, a date of their own (`docs/spec/model.md`, section 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dating {
	/// The day a person sees: today, unless they chose another.
	pub day: Day,
	/// The date and time of writing, with the device's offset from UTC, when the day is
	/// today.
	pub now: Option<Edtf>,
	/// When the values written are true, if the person said: a date for something that
	/// happened, an interval for a state.
	pub valid_time: Option<Edtf>,
	/// The language that the text, names and files written are in, if the person said.
	pub language: Option<Tag>,
}

impl Dating {
	/// Today, at a moment in milliseconds since 1970 (UTC), at the device's offset from UTC
	/// in minutes. Returns `None` if the offset is outside the range from −12 to +14 hours.
	#[must_use]
	pub fn today(wall_millis: u64, offset_minutes: i32) -> Option<Self> {
		let day = Day::from_unix_millis(wall_millis, offset_minutes)?;
		let second = Day::second_of_day(wall_millis, offset_minutes);
		Some(Self {
			day,
			now: Some(Edtf::moment(day, second, offset_minutes)?),
			valid_time: None,
			language: None,
		})
	}

	/// Another day than today.
	#[must_use]
	pub fn on(day: Day) -> Self {
		Self {
			day,
			now: None,
			valid_time: None,
			language: None,
		}
	}

	/// The same, with the valid time that a person gave for the values written.
	#[must_use]
	pub fn with_valid_time(mut self, valid_time: Edtf) -> Self {
		self.valid_time = Some(valid_time);
		self
	}

	/// The same, with the language that a person gave for the values written.
	#[must_use]
	pub fn with_language(mut self, language: Tag) -> Self {
		self.language = Some(language);
		self
	}

	/// The valid time of a value that supersedes nothing in a dated field with one value:
	/// the moment of writing, if the day is today, and the day otherwise.
	fn start(&self) -> Edtf {
		self.now.clone().unwrap_or_else(|| Edtf::day(self.day))
	}
}

/// A change to an entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
	/// Makes these the field's values, replacing those it has; without values, clears it.
	Set(FieldName, Vec<Value>),
	/// Adds a value to the field. A second value of a field with one value competes with
	/// the first until a person chooses, as a claim against another.
	Add(FieldName, Value),
	/// Removes the values of the field that this one names, in any of their forms, with
	/// every form they have.
	Remove(FieldName, Value),
	/// Removes the forms of the field's values that equal this one, and keeps the values'
	/// other forms.
	RemoveForm(FieldName, Value),
	/// Removes the forms of the field's values that equal this one and are in this language,
	/// or in none, and keeps the values' other forms, as those that say the same in another
	/// language.
	RemoveFormIn {
		/// The field.
		field: FieldName,
		/// The form, as it says.
		value: Value,
		/// Its language, or `None` for a form in none.
		language: Option<Tag>,
	},
	/// Adds another form of a value that the field shows, in another language or script,
	/// with the value's valid time: of the value `of`, in any of its forms, or, without
	/// it, of the only value the field shows.
	AddForm {
		/// The field.
		field: FieldName,
		/// The value, in any of its forms, if the field shows several.
		of: Option<Value>,
		/// The new form.
		value: Value,
	},
	/// Deletes the entity: hides it and keeps it, until it is restored.
	Delete,
	/// Restores a deleted entity.
	Restore,
	/// Gives the entity another type, which it may have already: types combine, as a
	/// photo is a file too.
	AddType(TypeName),
	/// Takes a type away from the entity, which keeps at least one; its values stay, as
	/// values of fields that no type of it defines.
	RemoveType(TypeName),
}

/// A value of a new entity: in a language, if given, and with other forms, each in its
/// language, if given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewValue {
	/// The field.
	pub field: FieldName,
	/// The value, its first form.
	pub value: Value,
	/// The language of the value, if it can have one and it is known.
	pub language: Option<Tag>,
	/// Other forms of the value, in other languages or scripts.
	pub forms: Vec<(Value, Option<Tag>)>,
}

impl NewValue {
	/// A value without a language or other forms.
	#[must_use]
	pub fn new(field: FieldName, value: Value) -> Self {
		Self {
			field,
			value,
			language: None,
			forms: Vec::new(),
		}
	}

	/// The same, in a language.
	#[must_use]
	pub fn in_language(mut self, language: Tag) -> Self {
		self.language = Some(language);
		self
	}

	/// The same, with another form, in a language if given.
	#[must_use]
	pub fn with_form(mut self, form: Value, language: Option<Tag>) -> Self {
		self.forms.push((form, language));
		self
	}
}

/// What a space holds, counted: [`Library::census`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
	/// How many things have each type, of the things that [`Census::things`] counts: a
	/// thing of several types counts for each.
	pub types: BTreeMap<TypeName, usize>,
	/// How many things the space holds, as [`Library::entities`] lists them: entities that
	/// are not deleted and do not define types, those merged together as one.
	pub things: usize,
	/// How many deleted entities the space holds, as [`Library::deleted`] lists them: those
	/// that can be restored.
	pub deleted: usize,
}

/// A value with when it held: [`Library::dated_values`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatedValue {
	/// The statement that holds it.
	pub statement: StatementId,
	/// The value.
	pub value: Value,
	/// When it was true: a date for something that happened, an interval for a state.
	pub valid_time: Edtf,
}

/// An entity in a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
	/// The entity.
	pub id: EntityId,
	/// The name to show for it, if it has one.
	pub name: Option<String>,
	/// Its types.
	pub types: Vec<TypeName>,
	/// Of an entity that a search found through a link, rather than by its own words, the
	/// entity that it was found through.
	pub via: Option<EntityId>,
}

/// The first entities that a search finds, and how many more there are, as
/// [`Library::search_first`] lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Searched {
	/// The first entities found, in the order of [`Library::search`].
	pub listed: Vec<Listed>,
	/// How many more it finds.
	pub more: More,
}

/// How many more entities a search finds than it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum More {
	/// This many, exactly.
	Exactly(usize),
	/// At least this many: those found by their words, without those found through links,
	/// which were not looked for.
	AtLeast(usize),
}

impl More {
	/// The number, exact or not.
	#[must_use]
	pub fn count(self) -> usize {
		match self {
			Self::Exactly(count) | Self::AtLeast(count) => count,
		}
	}
}

impl Library {
	/// Adds an entity of these types, with these values, to a space. Returns the entity.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space; if a type is one of those that
	/// define types, which [`Library::define_type`] defines; if a field is one of NibKeep's
	/// own, which its own operations set; if a field with one value gets several; or if
	/// the change cannot be saved, such as a secret value without the space's secrets key.
	pub fn create_entity(
		&mut self,
		space: SpaceId,
		types: &[TypeName],
		values: Vec<(FieldName, Value)>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		let values = values
			.into_iter()
			.map(|(field, value)| NewValue::new(field, value))
			.collect();
		self.create_entity_with(space, types, values, wall_millis, rng)
	}

	/// Adds an entity as [`Library::create_entity`] does, with its text, names and files in
	/// `language`.
	///
	/// # Errors
	///
	/// As [`Library::create_entity`].
	pub fn create_entity_with_language(
		&mut self,
		space: SpaceId,
		types: &[TypeName],
		values: Vec<(FieldName, Value)>,
		language: &Tag,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		let values = values
			.into_iter()
			.map(|(field, value)| NewValue::new(field, value).in_language(language.clone()))
			.collect();
		self.create_entity_with(space, types, values, wall_millis, rng)
	}

	/// Adds an entity as [`Library::create_entity`] does, with values that can be in a
	/// language and have other forms, as an import of data in several languages writes
	/// them. Forms of one value count as one.
	///
	/// # Errors
	///
	/// As [`Library::create_entity`].
	pub fn create_entity_with(
		&mut self,
		space: SpaceId,
		types: &[TypeName],
		values: Vec<NewValue>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.check_not_group(space)?;
		let (entity, operations) = self.new_entity(space, types, values, rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok(entity)
	}

	/// A new entity and the operations that make it, as [`Library::create_entity_with`]
	/// makes it, for a change that holds more.
	pub(crate) fn new_entity(
		&self,
		space: SpaceId,
		types: &[TypeName],
		values: Vec<NewValue>,
		rng: &mut impl CryptoRng,
	) -> Result<(EntityId, Vec<Operation>), NodeError> {
		let known = &self.space(space)?.types;
		// The records of users, devices, replicas and spaces are made here too, by NibKeep's
		// own operations, which apps reach through their own commands.
		if let Some(own) = types.iter().find(|name| {
			**name == TYPE_DEFINITION || **name == FIELD_DEFINITION || policies::is_own_type(name)
		}) {
			return Err(NodeError::OwnType(own.clone()));
		}
		let mut counts: BTreeMap<&FieldName, usize> = BTreeMap::new();
		for value in &values {
			if fields::is_reserved(&value.field) || policies::is_own_field(&value.field) {
				return Err(NodeError::OwnField(value.field.clone()));
			}
			*counts.entry(&value.field).or_default() += 1;
		}
		if let Some((field, _)) = counts
			.iter()
			.find(|&(field, &count)| count > 1 && has_one_value(known, field))
		{
			return Err(NodeError::OneValue((*field).clone()));
		}
		let entity = EntityId::random(rng);
		let mut operations: Vec<Operation> = types
			.iter()
			.map(|name| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					entity,
					fields::TYPE,
					Value::Text(name.to_string()),
				))
			})
			.collect();
		for NewValue {
			field,
			value,
			language,
			forms,
		} in values
		{
			let first = StatementId::random(rng);
			let qualifiers = in_language(Qualifiers::new(), &value, language.as_ref());
			let qualifiers = for_field(known, &field, qualifiers);
			operations.push(Operation::assert(
				Statement::new(first, entity, field.clone(), value).with_qualifiers(qualifiers),
			));
			for (form, language) in forms {
				let qualifiers =
					Qualifiers::new().with(fields::ALTERNATIVE, Value::Text(first.to_string()));
				let qualifiers = in_language(qualifiers, &form, language.as_ref());
				let qualifiers = for_field(known, &field, qualifiers);
				operations.push(Operation::assert(
					Statement::new(StatementId::random(rng), entity, field.clone(), form)
						.with_qualifiers(qualifiers),
				));
			}
		}
		Ok((entity, operations))
	}

	/// Changes an entity of a space, with every edit in one change, as of `dating`, except
	/// that edits of what governs the space, such as a replica's key, make a change of their
	/// own, committed first. Each edit sees what the edits before it did. Edits that change
	/// nothing, such as deleting a deleted entity or setting a field to the values it shows,
	/// commit nothing.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space or the space no such entity; if the
	/// entity defines a type or field, which only [`Library::define_type`] changes; if a field
	/// is one of NibKeep's own; if a field with one value is set to several; if a value to
	/// remove is not shown; if a type to give or take away is one of NibKeep's own, or one to
	/// take away is not the entity's, or its only one; or if the change cannot be saved.
	pub fn edit_entity(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		edits: Vec<Edit>,
		dating: &Dating,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let types = &self.space(space)?.types;
		let operations = {
			let state = self.space(space)?.stored.log().state();
			let entity = state.canonical(entity)?;
			if state.fields(entity)?.is_empty() {
				return Err(NodeError::NoEntity(entity.to_string()));
			}
			if let Some(own) = state.types(entity)?.into_iter().find(|name| {
				*name == TYPE_DEFINITION || *name == FIELD_DEFINITION || policies::is_own_type(name)
			}) {
				return Err(NodeError::OwnType(own));
			}
			// In a space with members, a member's link to a person takes both sides, each
			// replacing only its own statements: only `Library::link_member` writes it.
			if self.has_members(space)?
				&& edits.iter().any(|edit| {
					matches!(edit, Edit::Set(field, _)
						| Edit::Add(field, _)
						| Edit::Remove(field, _)
						| Edit::RemoveForm(field, _)
						| Edit::RemoveFormIn { field, .. }
						| Edit::AddForm { field, .. } if *field == LINKED_PERSON)
				}) {
				return Err(NodeError::OwnField(LINKED_PERSON));
			}
			// Each edit sees the ones before it, in a copy of the entity's part of the state.
			let mut working = state.excerpt(entity)?;
			let at = Timestamp::new(wall_millis, 0);
			let mut operations = Vec::new();
			for edit in edits {
				let made =
					edit_operations(&working, types, &self.languages, entity, edit, dating, rng)?;
				// Each changes only the entity it names, so that what an edit replaced of an
				// entity merged into this one is gone for the next edit too.
				let made = super::scope(made, |id| {
					Ok(working.statement(id)?.map(|statement| statement.entity))
				})?;
				for operation in &made {
					working.apply(operation, at)?;
				}
				operations.extend(made);
			}
			operations
		};
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Finds the entities of a space that are not deleted and whose values shown hold every
	/// word of `query`, in the order of the search index: those with more matching values
	/// first. Secret values are never searched, nor the text of NibKeep's fields for
	/// structure, keys and positions. A search that matches where a file was found, the path
	/// of one of its origins, or where it is checked out, finds the file, unless it is
	/// deleted, rather than the origin or the checkout. Entities that define types are never
	/// found.
	///
	/// Through links, it finds more, each with the entity it was found through, after those
	/// found by their words (`docs/spec/model.md`, section 10): for each of them, the
	/// entities that stand for it, such as a pen name for a person who uses it, and those
	/// that link to it through a field that searches go through, such as a book through its
	/// author, ordered by name; and then, through each entity that stands for it, those that
	/// link to that entity so. No entity is found twice.
	///
	/// Everything found is listed, which costs most for common words: where a page is
	/// enough, [`Library::search_first`] costs less.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn search(&self, space: SpaceId, query: &str) -> Result<Vec<Listed>, NodeError> {
		Ok(self
			.search_first(space, query, usize::MAX, |_, _| Ok(true))?
			.listed)
	}

	/// Lists what [`Library::search`] finds, in its order, but only the entities that `keep`
	/// keeps, given each one's ID and types, and only the first `first` of those; and counts
	/// the others it keeps. Naming entities is most of what a search costs, and those past
	/// the page are not named, but for those found through links in the group that the page
	/// ends in, which are ordered by name. Entities found through links come after every
	/// entity found by its words, so links are followed only when no more than `first` of
	/// those are kept; otherwise the count leaves out what links would find, and is
	/// [`More::AtLeast`].
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the space cannot be read, or
	/// `keep` fails.
	pub fn search_first(
		&self,
		space: SpaceId,
		query: &str,
		first: usize,
		mut keep: impl FnMut(EntityId, &BTreeSet<TypeName>) -> Result<bool, NodeError>,
	) -> Result<Searched, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		open.stored.reading(|stored| {
			let hits: Vec<EntityId> = stored
				.search(query)?
				.into_iter()
				.map(|found| found.entity)
				.collect();
			super::links::searched_through(
				state,
				(&open.types, &self.languages),
				(&hits, first),
				&mut keep,
			)
		})
	}

	/// An entity of a space, as a person sees it on `day`, or `None` if the space says
	/// nothing about it. Of entities merged together, the one they were merged into is shown.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn entity(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: Day,
	) -> Result<Option<EntityView>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			entity_view(
				stored.log().state(),
				&open.types,
				&self.languages,
				entity,
				day,
			)
		})
	}

	/// The name to show for an entity of a space, if it has one.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn name(&self, space: SpaceId, entity: EntityId) -> Result<Option<String>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			display_name(
				state,
				&open.types,
				&self.languages,
				state.canonical(entity)?,
			)
		})
	}

	/// The entities of a space that are not deleted, without those that define types, or
	/// the entities of a type, ordered by name without regard to case, unnamed ones last.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn entities(
		&self,
		space: SpaceId,
		of_type: Option<&TypeName>,
	) -> Result<Vec<Listed>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let Some(name) = of_type else {
			// Every thing of the space, told apart and named part by part.
			return listing(state, &open.types, &self.languages, PART);
		};
		let ids = with_type(&open.stored, name)?;
		listed_from(state, &open.types, &self.languages, (ids, PART))
	}

	/// The deleted entities of a space, without those that define types, ordered by name
	/// without regard to case, unnamed ones last: those that can be restored.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn deleted(&self, space: SpaceId) -> Result<Vec<Listed>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let ids = entities_where(state, true, PART)?;
		listed_from(state, &open.types, &self.languages, (ids, PART))
	}

	/// Counts what a space holds, as [`Library::entities`] and [`Library::deleted`] list
	/// it, in one pass over the space that names nothing, which costs much less than
	/// listing: for an overview of a large space.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn census(&self, space: SpaceId) -> Result<Census, NodeError> {
		let open = self.space(space)?;
		// In one reading, so that the counts are of one state, whatever another program
		// writes meanwhile.
		open.stored
			.reading(|stored| census(stored.log().state(), PART))
	}

	/// The current values of a field of an entity that have a valid time, with it, ordered
	/// by statement ID: what the field held over time, such as each of a dog's weights, for
	/// a chart of how it changed. Of entities merged together, those of all of them; a value
	/// in several forms counts once, by its first, and statements that repeat one with a
	/// smaller ID count as that one.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn dated_values(
		&self,
		space: SpaceId,
		entity: EntityId,
		field: &FieldName,
	) -> Result<Vec<DatedValue>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			let entity = state.canonical(entity)?;
			dated_in(state, entity, field)
		})
	}

	/// Each field of an entity that `wanted` accepts and that has current values with a valid
	/// time, with those values, as [`Library::dated_values`] gives them, ordered by field:
	/// what an entity held over time in all its fields, read at once, for a time line of many
	/// entities. Fields that `wanted` refuses, such as secret ones, are not read.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn all_dated_values(
		&self,
		space: SpaceId,
		entity: EntityId,
		wanted: impl Fn(&FieldName) -> bool,
	) -> Result<Vec<(FieldName, Vec<DatedValue>)>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state();
			let entity = state.canonical(entity)?;
			let mut all = Vec::new();
			for field in state.fields(entity)? {
				if !wanted(&field) {
					continue;
				}
				let dated = dated_in(state, entity, &field)?;
				if !dated.is_empty() {
					all.push((field, dated));
				}
			}
			Ok(all)
		})
	}

	/// Finds an entity of a space by its ID, or else by a prefix of its ID of at least six
	/// characters or by its name, without regard to case. A name and a prefix that match
	/// different entities are ambiguous. Names find only entities that are not deleted, so
	/// that references never point to a deleted entity by accident.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the space cannot be read, or no
	/// entity or several match.
	pub fn find_entity(&self, space: SpaceId, reference: &str) -> Result<EntityId, NodeError> {
		self.lookup(space, reference, false)
	}

	/// Finds an entity as [`Library::find_entity`] does, except that a name that finds no
	/// entity that is not deleted finds deleted ones, for showing or restoring them.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the space cannot be read, or no
	/// entity or several match.
	pub fn find_entity_or_deleted(
		&self,
		space: SpaceId,
		reference: &str,
	) -> Result<EntityId, NodeError> {
		self.lookup(space, reference, true)
	}

	fn lookup(
		&self,
		space: SpaceId,
		reference: &str,
		deleted: bool,
	) -> Result<EntityId, NodeError> {
		let open = self.space(space)?;
		open.stored
			.reading(|stored| lookup(stored, &open.types, &self.languages, reference, deleted))
	}
}

/// Finds an entity as [`Library::find_entity`] does, or with `deleted`, as
/// [`Library::find_entity_or_deleted`] does.
fn lookup(
	stored: &StoredLog,
	types: &Types,
	languages: &Languages,
	reference: &str,
	deleted: bool,
) -> Result<EntityId, NodeError> {
	let state = stored.log().state();
	if let Ok(id) = reference.parse::<EntityId>() {
		// The entity, or one merged with it, has a current statement: a merge, or another.
		let canonical = state.canonical(id)?;
		if canonical != id
			|| !state.fields(canonical)?.is_empty()
			|| state.merged(canonical)?.len() > 1
		{
			return Ok(canonical);
		}
	}
	let lowered = reference.to_lowercase();
	let looks_like_id = lowered.len() >= SHORTEST_PREFIX
		&& lowered
			.chars()
			.all(|character| character.is_ascii_hexdigit() || character == '-');
	// Every entity, only when a reference may be the beginning of an ID, or when the
	// indexes cannot tell which entities have the name.
	let named = stored.named(reference)?;
	let entities = if looks_like_id || named.is_none() {
		state.entities()?
	} else {
		BTreeSet::new()
	};
	let by_prefix = entities
		.iter()
		.copied()
		.filter(|entity| looks_like_id && entity.to_string().starts_with(&lowered));
	let mut with_name = Vec::new();
	let mut live = Vec::new();
	for entity in named.unwrap_or_else(|| entities.iter().copied().collect()) {
		if names_now(state, types, entity)?.contains(&lowered) {
			with_name.push(entity);
			if !state.is_deleted(entity)? {
				live.push(entity);
			}
		}
	}
	let mut named = if live.is_empty() && deleted {
		with_name
	} else {
		live
	};
	// A user and the person linked to the user often share a name, which finds the person.
	let mut linked = BTreeSet::new();
	for &entity in &named {
		if state.types(entity)?.contains(&USER)
			&& let Some(person) = referred(state, types, languages, entity, &LINKED_PERSON)?
			&& named.contains(&person)
		{
			linked.insert(entity);
		}
	}
	named.retain(|entity| !linked.contains(entity));
	let candidates: Vec<EntityId> = by_prefix
		.chain(named)
		.collect::<BTreeSet<EntityId>>()
		.into_iter()
		.collect();
	match candidates.as_slice() {
		[] => Err(NodeError::NoEntity(reference.to_owned())),
		&[entity] => Ok(entity),
		_ => Err(NodeError::Ambiguous {
			reference: reference.to_owned(),
			candidates,
		}),
	}
}

/// An entity of a state, as a person sees it on `day`, as [`Library::entity`] says.
pub(crate) fn entity_view<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	day: Day,
) -> Result<Option<EntityView>, NodeError>
where
	NodeError: From<S::Error>,
{
	let entity = state.canonical(entity)?;
	let names = state.fields(entity)?;
	if names.is_empty() {
		return Ok(None);
	}
	let mut fields = Vec::new();
	for field in names {
		if [fields::TYPE, fields::DELETED, fields::MERGED_INTO].contains(&field) {
			continue;
		}
		let definition = types.field(&field);
		let (statements, competing) =
			state.resolve_counted(entity, &field, definition.as_ref(), day, languages)?;
		let mut values = Vec::new();
		// The field's forms, read once for all its values, when one of them has several.
		let groups = if statements.iter().any(|(_, forms)| *forms > 1) {
			state.form_groups(entity, &field)?
		} else {
			Vec::new()
		};
		for (statement, forms) in statements {
			let others = if forms > 1 {
				other_forms(&groups, &statement)
			} else {
				Vec::new()
			};
			values.push(ShownValue {
				statement: statement.id,
				value: statement.value.clone(),
				valid_time: statement.qualifiers.valid_time().cloned(),
				presence: presence_on(&statement, day),
				unreadable_date: statement.qualifiers.has_valid_time()
					&& statement.qualifiers.valid_time().is_none(),
				language: statement.qualifiers.language().map(str::to_owned),
				other_forms: forms.saturating_sub(1),
				forms: others,
			});
		}
		if !values.is_empty() {
			fields.push(FieldView {
				field,
				values,
				competing,
			});
		}
	}
	Ok(Some(EntityView {
		id: entity,
		name: name_on(state, types, languages, entity, day)?,
		types: state.types(entity)?.into_iter().collect(),
		deleted: state.is_deleted(entity)?,
		fields,
	}))
}

/// The current values of an entity's field that have a valid time, with it, ordered by
/// statement ID: a value in several forms by its first.
fn dated_in<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
) -> Result<Vec<DatedValue>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut dated = Vec::new();
	for forms in state.form_groups(entity, field)? {
		let Some(first) = forms.first() else {
			continue;
		};
		if let Some(valid_time) = first.qualifiers.valid_time() {
			dated.push(DatedValue {
				statement: first.id,
				value: first.value.clone(),
				valid_time: valid_time.clone(),
			});
		}
	}
	Ok(dated)
}

/// The other forms of the value that a statement shows, of a field's groups of forms: those
/// that hold another value, or the same in another language. Tags compare without regard to
/// case.
fn other_forms(groups: &[Vec<Cow<'_, Statement>>], shown: &Statement) -> Vec<OtherForm> {
	let same =
		|value: &Value, language: Option<&str>, other: &Value, other_language: Option<&str>| {
			value == other
				&& language.map(str::to_lowercase) == other_language.map(str::to_lowercase)
		};
	let language = shown.qualifiers.language();
	// The group that holds the statement, or else the one that holds what it repeats.
	let Some(group) = groups
		.iter()
		.find(|group| group.iter().any(|form| form.id == shown.id))
		.or_else(|| {
			groups
				.iter()
				.find(|group| group.iter().any(|form| content(form) == content(shown)))
		})
	else {
		return Vec::new();
	};
	let mut others: Vec<OtherForm> = Vec::new();
	for form in group {
		let form_language = form.qualifiers.language();
		// A form written twice, or as the value shown, is one form.
		let repeated = same(&form.value, form_language, &shown.value, language)
			|| others.iter().any(|other| {
				same(
					&other.value,
					other.language.as_deref(),
					&form.value,
					form_language,
				)
			});
		if form.id != shown.id && !repeated {
			others.push(OtherForm {
				value: form.value.clone(),
				language: form_language.map(str::to_owned),
			});
		}
	}
	others
}

/// The entities of a stored space that have a type, by the value index, deleted ones aside.
pub(crate) fn with_type(
	stored: &StoredLog,
	name: &TypeName,
) -> Result<BTreeSet<EntityId>, NodeError> {
	Ok(stored
		.find(&fields::TYPE, &Value::Text(name.to_string()))?
		.into_iter()
		.collect())
}

/// The entities of a state that a person counts as things: not deleted, and not defining
/// types.
pub(crate) fn things<S: Statements>(state: &State<S>) -> Result<BTreeSet<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	entities_where(state, false, PART)
}

/// How many entities a part holds when a whole space is read part by part: few enough for
/// what they say to sit in memory at once, and many enough for each read to take in much.
pub(crate) const PART: usize = 2_048;

/// The fields that say what an entity is, whether it is deleted and what it is merged into.
const KINDS: [FieldName; 3] = [fields::TYPE, fields::DELETED, fields::MERGED_INTO];

/// What [`by_parts`] calls with the state of each part and the chosen entities in it.
pub(crate) type EachPart<'a> = dyn FnMut(&State, &[EntityId]) -> Result<(), NodeError> + 'a;

/// Reads what `fields` say of `chosen`, entities ordered by ID, part by part, and calls
/// `each` with the state of each part and the chosen entities in it. A part is a range of
/// `part` entities of the space, as [`State::excerpt_of_range`] takes it from the state, and
/// only the parts that hold a chosen entity are read: each read takes in a part, not an
/// entity, and no more than a part sits in memory, however few or spread out the chosen
/// entities are.
pub(crate) fn by_parts<S: Statements>(
	state: &State<S>,
	chosen: &[EntityId],
	(fields, part): (&[FieldName], usize),
	each: &mut EachPart<'_>,
) -> Result<(), NodeError>
where
	NodeError: From<S::Error>,
{
	// Listing the space's entities takes in the whole space, which no part is wanted from.
	if chosen.is_empty() {
		return Ok(());
	}
	let space = state.storage().entities()?;
	let merges = state.storage().current_in_field(&fields::MERGED_INTO)?;
	let groups = MergeGroups::of(&merges);
	let mut bounds: Vec<(EntityId, Option<EntityId>)> = space
		.chunks(part.max(1))
		.filter_map(|entities| Some((*entities.first()?, Some(*entities.last()?))))
		.collect();
	// Chosen entities after the last of the space, which no current statement describes.
	if let (Some(&chosen_last), Some(&(_, Some(space_last)))) = (chosen.last(), bounds.last())
		&& chosen_last > space_last
	{
		bounds.push((chosen_last, None));
	} else if bounds.is_empty() && !chosen.is_empty() {
		bounds.push((chosen[0], None));
	}
	let mut chosen = chosen.iter().copied().peekable();
	for (first, last) in bounds {
		let mut in_part = Vec::new();
		while let Some(entity) = chosen.next_if(|entity| last.is_none_or(|last| *entity <= last)) {
			in_part.push(entity);
		}
		let (Some(&lowest), Some(&highest)) = (in_part.first(), in_part.last()) else {
			continue;
		};
		// Chosen entities between two parts of the space count with the later one; nothing
		// else lies between them.
		let range = (first.min(lowest), last.unwrap_or(highest).max(highest));
		let excerpt = state.excerpt_of_range(range, fields, &groups.touching(range))?;
		each(&excerpt, &in_part)?;
	}
	Ok(())
}

/// The entities of a space merged together, in groups by the entity each group shows as,
/// with the merges of each group, which a part that the group touches needs: merges are
/// few, and a part takes only those of the groups it touches.
struct MergeGroups<'a> {
	merges: &'a [Recorded<'a>],
	/// The group of each entity that a merge names.
	group_of: BTreeMap<EntityId, EntityId>,
	/// The merges of each group, by their place in `merges`.
	of_group: BTreeMap<EntityId, Vec<usize>>,
}

impl<'a> MergeGroups<'a> {
	/// The groups that `merges`, the current merges of a space, make.
	fn of(merges: &'a [Recorded<'a>]) -> Self {
		let mut all = Memory::default();
		for recorded in merges {
			all.insert(recorded.statement.clone().into_owned(), recorded.origin());
		}
		let all = State::in_storage(all);
		let mut group_of = BTreeMap::new();
		let mut of_group: BTreeMap<EntityId, Vec<usize>> = BTreeMap::new();
		for (index, recorded) in merges.iter().enumerate() {
			let statement = &recorded.statement;
			// A merge belongs to the group of the entity it merges, whose canonical entity it
			// decides, with that entity's other merges.
			let Ok(group) = all.canonical(statement.entity);
			of_group.entry(group).or_default().push(index);
			group_of.insert(statement.entity, group);
			if let Some(into) = statement.value.as_reference() {
				let Ok(group) = all.canonical(into);
				group_of.insert(into, group);
			}
		}
		Self {
			merges,
			group_of,
			of_group,
		}
	}

	/// The merges of the groups that the entities from `first` to `last` belong to.
	fn touching(&self, (first, last): (EntityId, EntityId)) -> Vec<&'a Recorded<'a>> {
		let groups: BTreeSet<EntityId> = self
			.group_of
			.range(first..=last)
			.map(|(_, group)| *group)
			.collect();
		groups
			.iter()
			.filter_map(|group| self.of_group.get(group))
			.flatten()
			.map(|&index| &self.merges[index])
			.collect()
	}
}

/// The canonical entities that current statements describe, as [`State::entities`] finds
/// them, that do not define types and are deleted or not, as `deleted` says: the things of
/// the space, or those that can be restored. They are told apart part by part.
fn entities_where<S: Statements>(
	state: &State<S>,
	deleted: bool,
	part: usize,
) -> Result<BTreeSet<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	let entities = state.storage().entities()?;
	let mut kept = BTreeSet::new();
	by_parts(state, &entities, (&KINDS, part), &mut |kinds, entities| {
		for &entity in entities {
			let entity = kinds.canonical(entity)?;
			if is_listed(kinds, entity, deleted)? {
				kept.insert(entity);
			}
		}
		Ok(())
	})?;
	Ok(kept)
}

/// What a space holds, counted as [`listing`] and [`entities_where`] tell its entities
/// apart, in one pass over the space, `part` entities at a time.
fn census<S: Statements>(state: &State<S>, part: usize) -> Result<Census, NodeError>
where
	NodeError: From<S::Error>,
{
	let entities = state.storage().entities()?;
	// Entities merged together count once, as the one they show as, which may be in
	// another part.
	let mut seen = BTreeSet::new();
	let mut census = Census::default();
	by_parts(state, &entities, (&KINDS, part), &mut |kinds, entities| {
		for &entity in entities {
			let entity = kinds.canonical(entity)?;
			if !seen.insert(entity) || kept_apart::<Memory>(kinds, entity)? {
				continue;
			}
			if kinds.is_deleted(entity)? {
				census.deleted += 1;
			} else {
				census.things += 1;
				for name in kinds.types(entity)? {
					*census.types.entry(name).or_default() += 1;
				}
			}
		}
		Ok(())
	})?;
	Ok(census)
}

/// Whether an entity is listed among the deleted or the others, as `deleted` says, by what
/// a part of the state says: never one that defines types.
fn is_listed(kinds: &State, entity: EntityId, deleted: bool) -> Result<bool, NodeError> {
	Ok(kinds.is_deleted(entity)? == deleted && !kept_apart(kinds, entity)?)
}

/// Whether an entity is kept apart from those listed and counted: one that defines types,
/// or the record of a space's names in the personal space, which names a space.
pub(crate) fn kept_apart<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(defines_types(state, entity)?
		|| state
			.types(entity)?
			.contains(&nibkeep_governance::SPACE_NAMES))
}

/// Up to this many entities are listed by reading each from the state, which costs less
/// than reading the space part by part, as listing more does.
const FEW_LISTED: usize = 64;

/// Entities of a stored space in a list, as [`listed`] lists them, read one by one when
/// they are few, or else part by part.
pub(crate) fn listed_from(
	state: &State<Layers<StateFile>>,
	types: &Types,
	languages: &Languages,
	(ids, part): (BTreeSet<EntityId>, usize),
) -> Result<Vec<Listed>, NodeError> {
	if ids.len() <= FEW_LISTED {
		return listed(state, types, languages, ids);
	}
	let ids: Vec<EntityId> = ids.into_iter().collect();
	let fields = naming_fields(state)?;
	let mut all = Vec::new();
	by_parts(state, &ids, (&fields, part), &mut |part, ids| {
		all.extend(listed_in(part, types, languages, ids.iter().copied())?);
		Ok(())
	})?;
	sort_listed(&mut all);
	Ok(all)
}

/// Every thing of a space in a list, as [`listed`] lists them: told apart and named part by
/// part, `part` entities at a time, in one pass over the space.
fn listing<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	part: usize,
) -> Result<Vec<Listed>, NodeError>
where
	NodeError: From<S::Error>,
{
	let entities = state.storage().entities()?;
	let fields = naming_fields(state)?;
	let mut seen = BTreeSet::new();
	let mut all = Vec::new();
	by_parts(state, &entities, (&fields, part), &mut |part, entities| {
		let mut new = Vec::new();
		for &entity in entities {
			let entity = part.canonical(entity)?;
			if !seen.contains(&entity) && is_listed(part, entity, false)? {
				seen.insert(entity);
				new.push(entity);
			}
		}
		all.extend(listed_in(part, types, languages, new)?);
		Ok(())
	})?;
	sort_listed(&mut all);
	Ok(all)
}

/// Entities of a part of a space in a list, as [`listed`] lists them.
fn listed_in(
	part: &State,
	types: &Types,
	languages: &Languages,
	ids: impl IntoIterator<Item = EntityId>,
) -> Result<Vec<Listed>, NodeError> {
	listed(part, types, languages, ids)
}

/// The fields that listing entities reads: those that say what an entity is, whether it is
/// deleted or merged, and what names it, which [`naming`] looks for in every field whose
/// name ends in a naming suffix.
fn naming_fields<S: Statements>(state: &State<S>) -> Result<Vec<FieldName>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut fields = KINDS.to_vec();
	for field in state.storage().field_names()? {
		if fields::can_name(&field) {
			fields.push(field);
		}
	}
	Ok(fields)
}

/// Whether an entity defines a type or a field.
pub(crate) fn defines_types<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	let types = state.types(entity)?;
	Ok(types.contains(&TYPE_DEFINITION) || types.contains(&FIELD_DEFINITION))
}

/// Entities in a list, ordered by name without regard to case, unnamed ones last.
fn listed<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	ids: impl IntoIterator<Item = EntityId>,
) -> Result<Vec<Listed>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut listed = Vec::new();
	for id in ids {
		listed.push(Listed {
			id,
			name: display_name(state, types, languages, id)?,
			types: state.types(id)?.into_iter().collect(),
			via: None,
		});
	}
	sort_listed(&mut listed);
	Ok(listed)
}

/// Orders a list by name without regard to case, unnamed entities last.
fn sort_listed(listed: &mut [Listed]) {
	listed.sort_by_cached_key(|entity| {
		let name = entity.name.as_deref().map(str::to_lowercase);
		(name.is_none(), name, entity.id)
	});
}

/// The operations of one edit of an entity, as the state stands after the edits before it.
fn edit_operations<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	edit: Edit,
	dating: &Dating,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	if let Edit::Set(field, _)
	| Edit::Add(field, _)
	| Edit::Remove(field, _)
	| Edit::RemoveForm(field, _)
	| Edit::RemoveFormIn { field, .. }
	| Edit::AddForm { field, .. } = &edit
		&& (fields::is_reserved(field) || policies::is_own_field(field))
	{
		return Err(NodeError::OwnField(field.clone()));
	}
	let mut assert =
		|field: FieldName, value: Value, qualifiers: Qualifiers, supersedes: Vec<StatementId>| {
			let qualifiers = for_field(types, &field, qualifiers);
			let statement = Statement::new(StatementId::random(rng), entity, field, value)
				.with_qualifiers(qualifiers);
			Operation::replace(statement, supersedes)
		};
	let language = dating.language.as_ref();
	let at = (entity, dating.day);
	Ok(match edit {
		Edit::Set(field, values) => {
			let Some(Setting {
				mut supersedes,
				valid_time,
				form,
			}) = setting(state, types, languages, entity, &field, &values, dating)?
			else {
				return Ok(Vec::new());
			};
			if values.is_empty() {
				return Ok(vec![Operation::retract(entity, supersedes)]);
			}
			values
				.into_iter()
				.map(|value| {
					let qualifiers = corrected(valid_time.clone(), form.as_ref(), &value, language);
					assert(
						field.clone(),
						value,
						qualifiers,
						std::mem::take(&mut supersedes),
					)
				})
				.collect()
		}
		Edit::AddForm { field, of, value } => {
			let shown = resolve(state, types, languages, entity, &field, dating.day)?.0;
			let target = form_of(state, entity, &field, &shown, of.as_ref())?;
			// Forms of one value share its valid time.
			let group = target.qualifiers.alternative().unwrap_or(target.id);
			let qualifiers = dated(target.qualifiers.valid_time().cloned())
				.with(fields::ALTERNATIVE, Value::Text(group.to_string()));
			let qualifiers = in_language(qualifiers, &value, language);
			vec![assert(field, value, qualifiers, Vec::new())]
		}
		Edit::Add(field, value) => {
			// In a field with one value, a value added on another day than today starts on
			// that day, and one added today starts now if the field's values are dated.
			let definition = types.field(&field);
			let one = is_multiplicity(definition.as_ref(), Multiplicity::One);
			let starts = match &dating.valid_time {
				Some(valid_time) => Some(valid_time.clone()),
				None if one
					&& (dating.now.is_none()
						|| dated_one(state, definition.as_ref(), entity, &field)?) =>
				{
					Some(dating.start())
				}
				None => None,
			};
			let qualifiers = in_language(dated(starts), &value, language);
			vec![assert(field, value, qualifiers, Vec::new())]
		}
		Edit::Remove(..) | Edit::RemoveForm(..) | Edit::RemoveFormIn { .. } => {
			removal_of(state, types, languages, at, edit)?
		}
		Edit::Delete if state.is_deleted(entity)? => Vec::new(),
		Edit::Delete => vec![assert(
			fields::DELETED,
			Value::Boolean(true),
			Qualifiers::new(),
			Vec::new(),
		)],
		Edit::Restore => {
			let deletions = ids(&state.current(entity, &fields::DELETED)?);
			if deletions.is_empty() {
				Vec::new()
			} else {
				vec![Operation::retract(entity, deletions)]
			}
		}
		Edit::AddType(_) | Edit::RemoveType(_) => type_operations(state, entity, edit, rng)?,
	})
}

/// The operations that give an entity a type or take one away.
fn type_operations<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	edit: Edit,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let types = state.types(entity)?;
	// The records of users, devices, replicas and spaces keep the types NibKeep gave them.
	let record = super::export::OWN_RECORDS
		.into_iter()
		.find(|own| types.contains(own));
	Ok(match edit {
		Edit::AddType(name) => {
			refuse_own_type(&name)?;
			if types.contains(&name) {
				Vec::new()
			} else if let Some(record) = record {
				return Err(NodeError::OwnType(record.clone()));
			} else {
				let statement = Statement::new(
					StatementId::random(rng),
					entity,
					fields::TYPE,
					Value::Text(name.to_string()),
				);
				vec![Operation::assert(statement)]
			}
		}
		Edit::RemoveType(name) => {
			refuse_own_type(&name)?;
			if let Some(record) = record {
				return Err(NodeError::OwnType(record.clone()));
			}
			if !types.contains(&name) {
				return Err(NodeError::NotOfType(name));
			}
			if types.len() == 1 {
				return Err(NodeError::OnlyType(name));
			}
			let statements: Vec<StatementId> = state
				.current(entity, &fields::TYPE)?
				.iter()
				.filter(|statement| {
					statement
						.value
						.as_text()
						.and_then(|text| text.parse::<TypeName>().ok())
						== Some(name.clone())
				})
				.map(|statement| statement.id)
				.collect();
			vec![Operation::retract(entity, statements)]
		}
		_ => unreachable!("only edits of types come here"),
	})
}

/// Refuses a type that only NibKeep's own operations give to entities or take away: those
/// that define types and fields, storage policies and rules, and the records of users,
/// devices, replicas and spaces.
fn refuse_own_type(name: &TypeName) -> Result<(), NodeError> {
	if *name == TYPE_DEFINITION
		|| *name == FIELD_DEFINITION
		|| policies::is_own_type(name)
		|| super::export::OWN_RECORDS.contains(name)
	{
		return Err(NodeError::OwnType(name.clone()));
	}
	Ok(())
}

/// The operations of an edit that removes values or forms.
fn removal_of<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	at: (EntityId, Day),
	edit: Edit,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	match edit {
		Edit::Remove(field, value) => {
			removal(state, types, languages, at, field, &value, Removing::Whole)
		}
		Edit::RemoveForm(field, value) => {
			removal(state, types, languages, at, field, &value, Removing::Forms)
		}
		Edit::RemoveFormIn {
			field,
			value,
			language,
		} => {
			let removing = Removing::FormsIn(language.as_ref());
			removal(state, types, languages, at, field, &value, removing)
		}
		_ => Ok(Vec::new()),
	}
}

/// What a removal takes: whole values, with every form, or forms alone, in any language or
/// in one.
#[derive(Clone, Copy)]
enum Removing<'a> {
	Whole,
	Forms,
	FormsIn(Option<&'a Tag>),
}

/// The operations that remove `value` from an entity's field as shown on a day: with
/// `whole`, every form of the values that it names, in any of their forms; otherwise the
/// forms of the values shown that hold it.
fn removal<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	(entity, day): (EntityId, Day),
	field: FieldName,
	value: &Value,
	removing: Removing<'_>,
) -> Result<Vec<Operation>, NodeError>
where
	NodeError: From<S::Error>,
{
	let shown = resolve(state, types, languages, entity, &field, day)?.0;
	let values = values_shown(state, entity, &field, &shown)?;
	let whole = matches!(removing, Removing::Whole);
	let forms: Vec<Cow<'_, Statement>> = match removing {
		Removing::Whole => values
			.into_iter()
			.filter(|forms| forms.iter().any(|form| removes(&form.value, value)))
			.flatten()
			.collect(),
		Removing::Forms => values
			.into_iter()
			.flatten()
			.filter(|form| removes(&form.value, value))
			.collect(),
		Removing::FormsIn(language) => values
			.into_iter()
			.flatten()
			.filter(|form| {
				removes(&form.value, value)
					&& match (form.qualifiers.language(), language) {
						(None, None) => true,
						(Some(written), Some(language)) => {
							Tag::parse(written).as_ref() == Some(language)
						}
						_ => false,
					}
			})
			.collect(),
	};
	if forms.is_empty() {
		return Err(NodeError::NoValue(field));
	}
	// A field that shows the value recorded last also has older values, which removing the
	// one it shows must not bring back; any other field keeps the value on other days.
	let removed =
		if whole && is_multiplicity(types.field(&field).as_ref(), Multiplicity::LastWriterWins) {
			state.current(entity, &field)?
		} else {
			with_repeats(state, entity, &field, &forms)?
		};
	Ok(vec![Operation::retract(entity, ids(&removed))])
}

/// The value of `of`, in any of its forms, among the values a field shows, or, without it,
/// the only value the field shows: the value that a new form is of.
fn form_of<'a, S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
	shown: &'a [Cow<'a, Statement>],
	of: Option<&Value>,
) -> Result<&'a Statement, NodeError>
where
	NodeError: From<S::Error>,
{
	let Some(of) = of else {
		return match shown {
			[] => Err(NodeError::NoValue(field.clone())),
			[one] => Ok(one),
			_ => Err(NodeError::WhichValue(field.clone())),
		};
	};
	let values = values_shown(state, entity, field, shown)?;
	for statement in shown {
		let said = content(statement);
		let named = values.iter().any(|forms| {
			forms.iter().any(|form| content(form) == said)
				&& forms.iter().any(|form| removes(&form.value, of))
		});
		if named {
			return Ok(statement);
		}
	}
	Err(NodeError::NoValue(field.clone()))
}

/// The values that shown statements stand for, each with every form it has.
fn values_shown<'a, S: Statements>(
	state: &'a State<S>,
	entity: EntityId,
	field: &FieldName,
	shown: &[Cow<'_, Statement>],
) -> Result<Vec<Vec<Cow<'a, Statement>>>, NodeError>
where
	NodeError: From<S::Error>,
{
	let said: BTreeSet<(Vec<u8>, Vec<u8>)> = shown.iter().map(|each| content(each)).collect();
	Ok(state
		.form_groups(entity, field)?
		.into_iter()
		.filter(|forms| forms.iter().any(|form| said.contains(&content(form))))
		.collect())
}

/// The qualifiers of a value that a person sets: its valid time, and, when it corrects one
/// form of a value, the group of forms, which it stays in, and the form's language, unless
/// the person gave another.
fn corrected(
	valid_time: Option<Edtf>,
	form: Option<&Form>,
	value: &Value,
	language: Option<&Tag>,
) -> Qualifiers {
	let mut qualifiers = dated(valid_time);
	if let Some(Form {
		group,
		language: was,
	}) = form
	{
		qualifiers.insert(fields::ALTERNATIVE, Value::Text(group.to_string()));
		if language.is_none()
			&& let Some(was) = was
		{
			qualifiers.insert(fields::LANGUAGE, Value::Text(was.clone()));
		}
	}
	in_language(qualifiers, value, language)
}

/// Qualifiers with a valid time, if given.
fn dated(valid_time: Option<Edtf>) -> Qualifiers {
	match valid_time {
		Some(valid_time) => Qualifiers::new().with(fields::VALID_TIME, Value::Date(valid_time)),
		None => Qualifiers::new(),
	}
}

/// Qualifiers with the language of a value, if it has one: text, names and files can be in
/// a language; numbers, dates and references cannot.
fn in_language(mut qualifiers: Qualifiers, value: &Value, language: Option<&Tag>) -> Qualifiers {
	if let Some(language) = language
		&& matches!(value, Value::Text(_) | Value::Name(_) | Value::Blob(_))
	{
		qualifiers.insert(fields::LANGUAGE, Value::Text(language.to_string()));
	}
	qualifiers
}

/// The qualifiers of a value of a field, without a language if the field holds codes, which
/// are in no language: a language given for every value written is not theirs.
fn for_field(types: &Types, field: &FieldName, mut qualifiers: Qualifiers) -> Qualifiers {
	if types
		.field(field)
		.is_some_and(|definition| definition.is_code())
	{
		qualifiers.remove(&fields::LANGUAGE);
	}
	qualifiers
}

/// The IDs of statements.
fn ids(statements: &[Cow<'_, Statement>]) -> Vec<StatementId> {
	statements.iter().map(|statement| statement.id).collect()
}

/// Whether statements hold exactly these values, each as often.
fn same_values(statements: &[Cow<'_, Statement>], values: &[Value]) -> bool {
	let mut left: Vec<&Value> = statements
		.iter()
		.map(|statement| &statement.value)
		.collect();
	for value in values {
		match left.iter().position(|held| *held == value) {
			Some(index) => {
				left.swap_remove(index);
			}
			None => return false,
		}
	}
	left.is_empty()
}

/// Whether a field takes one value: one that competes, or the one recorded last.
fn has_one_value(types: &Types, field: &FieldName) -> bool {
	types.field(field).is_some_and(|definition| {
		matches!(
			definition.multiplicity(),
			Multiplicity::One | Multiplicity::LastWriterWins
		)
	})
}

/// What setting a field does.
struct Setting {
	/// The statements it supersedes.
	supersedes: Vec<StatementId>,
	/// The valid time of the values it writes.
	valid_time: Option<Edtf>,
	/// The group of forms that the value it writes joins, when it corrects one form of a
	/// value that has others, or one that names its group.
	form: Option<Form>,
}

/// A form of a value that a correction replaces.
struct Form {
	/// The ID that names the group of forms.
	group: StatementId,
	/// The language tag of the form, as written.
	language: Option<String>,
}

/// What setting a field to `values` does, or `None` if the field already shows them.
fn setting<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	field: &FieldName,
	values: &[Value],
	dating: &Dating,
) -> Result<Option<Setting>, NodeError>
where
	NodeError: From<S::Error>,
{
	if values.len() > 1 && has_one_value(types, field) {
		return Err(NodeError::OneValue(field.clone()));
	}
	let definition = types.field(field);
	// A field whose last writer wins hides older values, which setting it replaces too; any
	// other field keeps what belongs to other days. One value set in place of the one value
	// shown corrects the form shown; other settings replace the values shown, every form.
	let shown = resolve(state, types, languages, entity, field, dating.day)?.0;
	let last_writer_wins = is_multiplicity(definition.as_ref(), Multiplicity::LastWriterWins);
	let correcting = !last_writer_wins && shown.len() == 1 && values.len() == 1;
	let replaced = if last_writer_wins {
		state.current(entity, field)?
	} else if correcting {
		with_repeats(state, entity, field, &shown)?
	} else {
		let forms: Vec<Cow<'_, Statement>> = values_shown(state, entity, field, &shown)?
			.into_iter()
			.flatten()
			.collect();
		with_repeats(state, entity, field, &forms)?
	};
	let unchanged = same_values(&shown, values)
		&& dating.valid_time.as_ref().is_none_or(|valid_time| {
			shown
				.iter()
				.all(|statement| statement.qualifiers.valid_time() == Some(valid_time))
		});
	if unchanged {
		return Ok(None);
	}
	let valid_time = match &dating.valid_time {
		Some(valid_time) => Some(valid_time.clone()),
		// A correction keeps the date of what it replaces, the last of them in the field's
		// order; a value on a day that showed nothing starts on that day.
		None if dated_one(state, definition.as_ref(), entity, field)? => match replaced
			.iter()
			.max_by_key(|statement| statement.qualifiers.valid_time().map(Edtf::start))
		{
			Some(last) => last.qualifiers.valid_time().cloned(),
			None => Some(dating.start()),
		},
		None => None,
	};
	// One value that corrects one form keeps it among the others.
	let form = match shown.as_slice() {
		[corrected] if correcting => {
			let group = corrected.qualifiers.alternative();
			let others = state.forms(entity, field, corrected.id)?.len() > 1;
			(group.is_some() || others).then(|| Form {
				group: group.unwrap_or(corrected.id),
				language: corrected.qualifiers.language().map(str::to_owned),
			})
		}
		_ => None,
	};
	Ok(Some(Setting {
		supersedes: ids(&replaced),
		valid_time,
		form,
	}))
}

/// Shown statements, with the current statements of the field that repeat their value and
/// qualifiers, which show as one with them and go with them.
fn with_repeats<'a, S: Statements>(
	state: &'a State<S>,
	entity: EntityId,
	field: &FieldName,
	shown: &[Cow<'a, Statement>],
) -> Result<Vec<Cow<'a, Statement>>, NodeError>
where
	NodeError: From<S::Error>,
{
	let shown: BTreeSet<(Vec<u8>, Vec<u8>)> =
		shown.iter().map(|statement| content(statement)).collect();
	Ok(state
		.current(entity, field)?
		.into_iter()
		.filter(|statement| shown.contains(&content(statement)))
		.collect())
}

/// Whether removing `value` removes a value shown: the same value, or, for a name given only
/// in full, any name or text written that way, whatever other parts it has.
fn removes(shown: &Value, value: &Value) -> bool {
	if shown == value {
		return true;
	}
	match value {
		Value::Name(name) if name.parts().count() == 1 => shown.as_name_text() == Some(name.full()),
		_ => false,
	}
}

/// Whether a field definition has a multiplicity.
fn is_multiplicity(definition: Option<&FieldDef>, multiplicity: Multiplicity) -> bool {
	definition.is_some_and(|definition| definition.multiplicity() == multiplicity)
}

/// Whether a field has one value, and some of its values are dated: then a value written
/// without a valid time would count as earlier than all of them, and never show, so it
/// gets one of its own.
fn dated_one<S: Statements>(
	state: &State<S>,
	definition: Option<&FieldDef>,
	entity: EntityId,
	field: &FieldName,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(is_multiplicity(definition, Multiplicity::One)
		&& state
			.current(entity, field)?
			.iter()
			.any(|statement| statement.qualifiers.valid_time().is_some()))
}

/// The values of a field to show on `day`, as the field's definition says, each in the form
/// that suits a reader of `languages`, and whether they compete.
pub(crate) fn resolve<'a, S: Statements>(
	state: &'a State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	field: &FieldName,
	day: Day,
) -> Result<(Vec<Cow<'a, Statement>>, bool), NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(
		match state.resolve_for(entity, field, types.field(field).as_ref(), day, languages)? {
			Resolution::Values(statements) => (statements, false),
			Resolution::Competing(statements) => (statements, true),
		},
	)
}

/// The last day there is: names are shown as of it, so that an entity is named by the name
/// it has now, the last that started, unless its own period has ended. A name dated in the
/// future would show before its day.
fn last_day() -> Day {
	Day::from_ymd(999_999_999_999_999, 12, 31).expect("the last day of the largest year")
}

/// The name to show for an entity in lists and searches: its name as of the latest values,
/// as [`name_on`] finds it.
pub(crate) fn display_name<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	name_on(state, types, languages, entity, last_day())
}

/// The entity that a field refers to, as of the latest values: of several, the first that
/// the field shows. Of entities merged together, the one they were merged into.
pub(crate) fn referred<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	field: &FieldName,
) -> Result<Option<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	let referred = resolve(state, types, languages, entity, field, last_day())?
		.0
		.iter()
		.find_map(|statement| statement.value.as_reference());
	Ok(match referred {
		Some(referred) => Some(state.canonical(referred)?),
		None => None,
	})
}

/// A user as history names them, and the person linked to the user in the space, if any
/// and not deleted: the name of that person, or else the user's own name.
pub(crate) fn user_name<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	user: EntityId,
) -> Result<(Option<String>, Option<EntityId>), NodeError>
where
	NodeError: From<S::Error>,
{
	let mut person = referred(state, types, languages, user, &LINKED_PERSON)?;
	if let Some(linked) = person
		&& state.is_deleted(linked)?
	{
		person = None;
	}
	if let Some(person) = person
		&& let Some(name) = display_name(state, types, languages, person)?
	{
		return Ok((Some(name), Some(person)));
	}
	Ok((display_name(state, types, languages, user)?, person))
}

/// The field that names an entity, and its statements that show a name.
type Naming<'a> = (FieldName, Vec<Cow<'a, Statement>>);

/// The field that names an entity on `day`, and the value it shows, in the form that suits
/// a reader of `languages`: `org.nibkeep.name`, or else the first field by name that ends
/// in `.name`, or else in `.title`, that shows a name written in full, or text, each
/// resolved as the field's definition says. Of several values, the first that the field
/// shows counts, in the order of `docs/spec/model.md`, section 6.
fn naming<'a, S: Statements>(
	state: &'a State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	day: Day,
) -> Result<Option<Naming<'a>>, NodeError>
where
	NodeError: From<S::Error>,
{
	// The statements of the field that show a name, in the order the field shows them.
	let shown = |field: &FieldName| -> Result<Vec<Cow<'a, Statement>>, NodeError> {
		Ok(resolve(state, types, languages, entity, field, day)?
			.0
			.into_iter()
			.filter(|statement| statement.value.as_name_text().is_some())
			.collect())
	};
	let named = shown(&NAME)?;
	if !named.is_empty() {
		return Ok(Some((NAME, named)));
	}
	let fields = state.fields(entity)?;
	for suffix in fields::NAMING_SUFFIXES {
		for field in fields
			.iter()
			.filter(|field| fields::has_suffix(field, suffix))
		{
			let named = shown(field)?;
			if !named.is_empty() {
				return Ok(Some((field.clone(), named)));
			}
		}
	}
	Ok(None)
}

/// The name of an entity on `day`, as a reader of `languages` sees it: the name that
/// [`naming`] finds, written in full, or text.
pub(crate) fn name_on<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	entity: EntityId,
	day: Day,
) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(naming(state, types, languages, entity, day)?
		.and_then(|(_, statements)| statements.into_iter().next())
		.and_then(|statement| statement.value.as_name_text().map(str::to_owned)))
}

/// The names an entity has now, in every form, lowercased, by which it is found: the forms
/// of every name that the field [`naming`] finds shows as of the last day, competing names
/// included; and of entities merged into it, their own names too, whichever fields name them.
fn names_now<S: Statements>(
	state: &State<S>,
	types: &Types,
	entity: EntityId,
) -> Result<Vec<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut names = Vec::new();
	if let Some((field, shown)) = naming(state, types, &Languages::default(), entity, last_day())? {
		for statement in shown {
			for form in &state.forms(entity, &field, statement.id)? {
				if let Some(name) = form.value.as_name_text() {
					names.push(name.to_lowercase());
				}
			}
		}
	}
	for member in state.merged(entity)? {
		if member != entity {
			names.extend(
				super::merge::own_names_in(state, member)?
					.iter()
					.map(|name| name.to_lowercase()),
			);
		}
	}
	Ok(names)
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_keys::NewProtection;
	use nibkeep_testkit::{TestRng, millis_at};

	use super::*;
	use crate::NewLibrary;

	#[test]
	fn a_space_read_in_parts_lists_what_it_lists_read_at_once() {
		let mut rng = TestRng::new("in parts");
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-entities-in-parts-{}", std::process::id()),
		);
		let now = millis_at("2026-10-24", 12, 0);
		let protection = NewProtection::Passphrase(
			"correct horse",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		);
		let mut library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			now,
			&mut rng,
		)
		.expect("creates");
		let space = library.personal_space();
		let mut made = Vec::new();
		for n in 0..90 {
			// Every fifth has no name, only a note.
			let values = if n % 5 == 4 {
				let note: FieldName = "local.note".parse().expect("a field name");
				vec![(note, Value::Text(format!("note {n}")))]
			} else {
				vec![(
					fields::NAME,
					Value::Name(nibkeep_model::Name::new(format!("thing {n}"))),
				)]
			};
			made.push(
				library
					.create_entity(space, &[], values, now, &mut rng)
					.expect("creates"),
			);
		}
		// Merges between entities far apart by ID, so across parts, a chain of them, and a
		// deletion, of a merged entity too.
		let mut sorted = made.clone();
		sorted.sort();
		for (from, into) in [(0, 89), (89, 11), (5, 6), (17, 2), (40, 41)] {
			library
				.merge_entities(space, sorted[from], sorted[into], now, &mut rng)
				.expect("merges");
		}
		let dating = Dating::today(now, 0).expect("a day");
		for deleted in [sorted[11], sorted[8]] {
			library
				.edit_entity(space, deleted, vec![Edit::Delete], &dating, now, &mut rng)
				.expect("deletes");
		}
		let open = library.space(space).expect("a space");
		let state = open.stored.log().state();
		let at_once = |part| listing(state, &open.types, &library.languages, part).expect("lists");
		assert_eq!(at_once(2), at_once(100_000));
		for deleted in [false, true] {
			assert_eq!(
				entities_where(state, deleted, 3).expect("tells apart"),
				entities_where(state, deleted, 100_000).expect("tells apart"),
			);
		}
		assert!(
			entities_where(state, true, 3)
				.expect("tells apart")
				.contains(&state.canonical(sorted[0]).expect("reads")),
			"deleted through the entity it is merged into"
		);
		// More than a few chosen ones, spread over the space, listed part by part as each alone.
		let chosen: BTreeSet<EntityId> = entities_where(state, false, 100_000)
			.expect("tells apart")
			.into_iter()
			.enumerate()
			.filter(|(n, _)| n % 10 != 3)
			.map(|(_, entity)| entity)
			.collect();
		assert!(chosen.len() > FEW_LISTED);
		assert_eq!(
			listed_from(state, &open.types, &library.languages, (chosen.clone(), 3))
				.expect("lists"),
			listed(state, &open.types, &library.languages, chosen).expect("lists"),
		);
		drop(library);
		let _ = std::fs::remove_dir_all(directory);
	}
}
