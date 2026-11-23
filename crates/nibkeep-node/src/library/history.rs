//! History in system time: every value an entity has had, when it was recorded, by whom and
//! when it was replaced; and an entity, and what links to it, as the library had recorded it
//! at a moment (`docs/Design.md`, Q18; `docs/spec/time.md`, section 3).

use nibkeep_base::{EntityId, SpaceId, StatementId};
use std::collections::BTreeMap;

use nibkeep_model::storage::Statements as _;
use nibkeep_model::{FieldName, Qualifiers, Value, fields};
use nibkeep_time::{Day, Edtf, Timestamp};

use super::entities::{EntityView, display_name, entity_view};
use super::links::{Links, links_to};
use super::{Author, Library};
use crate::NodeError;

/// Every value an entity has had, by field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
	/// The entity, or of entities merged together, the one they were merged into.
	pub id: EntityId,
	/// The name to show for it, if it has one.
	pub name: Option<String>,
	/// Its fields, those of its types and of its own record included, ordered by name.
	pub fields: Vec<FieldHistory>,
}

/// Every value of one field of an entity, in the order they were recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldHistory {
	/// The field.
	pub field: FieldName,
	/// Its values, current and replaced.
	pub values: Vec<PastValue>,
}

/// A value an entity had, or has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PastValue {
	/// The statement that holds it.
	pub statement: StatementId,
	/// The value.
	pub value: Value,
	/// When it was true, if it says.
	pub valid_time: Option<Edtf>,
	/// Its language tag, if it has one.
	pub language: Option<String>,
	/// When it was first recorded.
	pub recorded: Timestamp,
	/// When a purge stated it again, if one did.
	pub restated: Option<Timestamp>,
	/// When it was replaced, corrected or removed, if it was.
	pub replaced: Option<Timestamp>,
	/// Who recorded it, as far as the space knows.
	pub author: Option<Author>,
	/// What the move that carried it from another space noted of it, if one did: the
	/// mover's account, which only that space proves.
	pub carried: Option<Carried>,
}

/// What a move noted of a value that it carried from another space, as its mover's account
/// of it, not as proof ([technical design: spaces and moves]).
///
/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
	/// The space it came from, if the note names it.
	pub from: Option<SpaceId>,
	/// That space's name, if this replica holds it.
	pub from_name: Option<String>,
	/// Who recorded it there, as far as this space, or else the personal space, knows the key
	/// that the note names.
	pub author: Option<Author>,
	/// When it was recorded there: milliseconds since 1970.
	pub recorded: Option<u64>,
	/// When it was replaced there, if it was: milliseconds since 1970.
	pub replaced: Option<u64>,
}

/// The authors and the names of spaces that moves noted, as a history found them, so that
/// each is found once.
#[derive(Default)]
struct Noted {
	authors: BTreeMap<String, Option<Author>>,
	spaces: BTreeMap<SpaceId, Option<String>>,
}

impl Library {
	/// Every value an entity of a space has had, or one field's, with when each was recorded,
	/// by whom, and when it was replaced: the history of the record, in system time. Of
	/// entities merged together, the values of all of them. `None` if the space says nothing
	/// about the entity. Values that were purged are gone; secret values that this replica
	/// has not opened are left out.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn history(
		&self,
		space: SpaceId,
		entity: EntityId,
		field: Option<&FieldName>,
	) -> Result<Option<History>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let log = stored.log();
			let state = log.state();
			let storage = state.storage();
			let entity = state.canonical(entity)?;
			if state.fields(entity)?.is_empty() && state.merged(entity)?.len() <= 1 {
				let none = storage
					.of_entity(entity)?
					.iter()
					.all(|(_, ids)| ids.is_empty());
				if none {
					return Ok(None);
				}
			}
			// Most values have one of a few authors, each found once.
			let mut authors: BTreeMap<String, Option<Author>> = BTreeMap::new();
			// And the authors and spaces that moves noted, each found once.
			let mut noted = Noted::default();
			let mut by_field: BTreeMap<FieldName, Vec<PastValue>> = BTreeMap::new();
			for member in state.merged(entity)? {
				for (name, ids) in storage.of_entity(member)? {
					if field.is_some_and(|wanted| *wanted != name) {
						continue;
					}
					for id in ids {
						let Some(recorded) = storage.get(id)? else {
							continue;
						};
						let author = match log.author_of(id)? {
							Some(key) => {
								let text = key.to_text();
								if let Some(author) = authors.get(&text) {
									author.clone()
								} else {
									let author = self.author_with_key(open, stored, &key)?;
									authors.insert(text, author.clone());
									author
								}
							}
							None => None,
						};
						let statement = recorded.statement;
						let carried =
							self.carried(open, stored, &statement.qualifiers, &mut noted)?;
						by_field.entry(name.clone()).or_default().push(PastValue {
							statement: statement.id,
							value: statement.value.clone(),
							valid_time: statement.qualifiers.valid_time().cloned(),
							language: statement.qualifiers.language().map(str::to_owned),
							recorded: recorded.at,
							restated: recorded.restated,
							replaced: storage.superseded_at(id)?,
							author,
							carried,
						});
					}
				}
			}
			let mut fields = Vec::new();
			for (field, mut values) in by_field {
				// A carried value goes by when the space it came from recorded it.
				values.sort_by_key(|value| {
					let noted = value.carried.as_ref().and_then(|carried| carried.recorded);
					(
						noted.unwrap_or_else(|| value.recorded.millis()),
						value.recorded,
						value.statement,
					)
				});
				fields.push(FieldHistory { field, values });
			}
			Ok(Some(History {
				id: entity,
				name: display_name(state, &open.types, &self.languages, entity)?,
				fields,
			}))
		})
	}

	/// What a move noted of a value it carried, as its qualifiers hold it, if they hold any
	/// of it.
	fn carried(
		&self,
		open: &super::Space,
		stored: &nibkeep_store::StoredLog,
		qualifiers: &Qualifiers,
		noted: &mut Noted,
	) -> Result<Option<Carried>, NodeError> {
		let millis = |field: &FieldName| match qualifiers.get(field) {
			Some(Value::Integer(millis)) => u64::try_from(*millis).ok(),
			_ => None,
		};
		let from = qualifiers
			.get(&fields::CARRIED_FROM)
			.and_then(Value::as_text)
			.and_then(|text| text.parse::<SpaceId>().ok());
		let key = qualifiers
			.get(&fields::CARRIED_BY)
			.and_then(Value::as_text)
			.and_then(nibkeep_crypto::VerifyingKey::from_text);
		let (recorded, replaced) = (
			millis(&fields::CARRIED_AT),
			millis(&fields::CARRIED_REPLACED),
		);
		if from.is_none() && key.is_none() && recorded.is_none() && replaced.is_none() {
			return Ok(None);
		}
		let mut author = None;
		if let Some(key) = &key {
			let text = key.to_text();
			if let Some(known) = noted.authors.get(&text) {
				author.clone_from(known);
			} else {
				author = self.author_with_key(open, stored, key)?;
				if author.is_none() {
					let personal = self.space(self.info.personal)?;
					author = self.author_with_key(personal, &personal.stored, key)?;
				}
				noted.authors.insert(text, author.clone());
			}
		}
		let from_name = match from {
			Some(space) if self.spaces.contains_key(&space) => {
				if let Some(name) = noted.spaces.get(&space) {
					name.clone()
				} else {
					let name = self.space_name(space)?;
					noted.spaces.insert(space, name.clone());
					name
				}
			}
			_ => None,
		};
		Ok(Some(Carried {
			from,
			from_name,
			author,
			recorded,
			replaced,
		}))
	}

	/// The name an entity of a space had as the library had recorded it at the end of the
	/// millisecond `millis`, if it had one: what [`Library::name`] gave then.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn name_as_of(
		&self,
		space: SpaceId,
		entity: EntityId,
		millis: u64,
	) -> Result<Option<String>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state_as_of(millis);
			let entity = state.canonical(entity)?;
			display_name(&state, &open.types, &self.languages, entity)
		})
	}

	/// An entity of a space as the library had recorded it at the end of the millisecond
	/// `millis`, shown as on `day`: what [`Library::entity`] showed then. Changes recorded
	/// later are left out, and values replaced later are shown. The space's types are those
	/// it has now. `None` if the library knew nothing of the entity then.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn entity_as_of(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: Day,
		millis: u64,
	) -> Result<Option<EntityView>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state_as_of(millis);
			entity_view(&state, &open.types, &self.languages, entity, day)
		})
	}

	/// The entities that linked to an entity on `day`, as the library had recorded them at
	/// the end of the millisecond `millis`, as [`Library::links_to`] gives them.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn links_to_as_of(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: Day,
		millis: u64,
		limit: usize,
	) -> Result<Vec<Links>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let state = stored.log().state_as_of(millis);
			links_to(
				&state,
				&open.types,
				&self.languages,
				(entity, Some(day)),
				(day, limit),
			)
		})
	}
}
