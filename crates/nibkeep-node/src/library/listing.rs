//! Listing the things of a space that meet conditions, in the order of a field.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::storage::Statements;
use nibkeep_model::{Decimal, FieldName, Languages, State, TypeName, Types, Value};
use nibkeep_time::{Day, Start};

use super::Library;
use super::entities::{
	Listed, PART, display_name, kept_apart, listed_from, name_on, things, with_type,
};
use super::links::may_link;
use crate::NodeError;

/// What [`Library::list`] lists: the things of a space, or of a type, that meet every
/// condition, ordered by name or by a field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
	/// Only the entities of this type.
	pub of_type: Option<TypeName>,
	/// Only the entities whose field shows the value, for each of these: references by
	/// the entity they refer to, or one merged with it, and names by their full text.
	pub conditions: Vec<(FieldName, Value)>,
	/// The field to order the entities by, by the first value it shows of each: numbers by
	/// size, dates by when they start, text and names alphabetically without regard to
	/// case, references by the names of what they refer to, no before yes. Entities without
	/// such a value come last, and entities with the same value by name.
	pub order_by: Option<FieldName>,
	/// The day whose values the conditions and the order look at, and whose names the
	/// entities go by, if not today.
	pub on: Option<Day>,
}

impl Library {
	/// The items of an ordered list of a space, in their order: the memberships that put
	/// them in it, not deleted, by position key ([technical design: ordered lists]).
	///
	/// [technical design: ordered lists]: ../../../../docs/Technical-design.md#66-ordered-lists-and-long-text
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn list_members(
		&self,
		space: SpaceId,
		list: EntityId,
	) -> Result<Vec<nibkeep_log::list::Member>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		Ok(nibkeep_log::list::members(state, state.canonical(list)?)?)
	}

	/// The things of a space, deleted ones and those that define types aside, that the
	/// listing asks for, as they are on its day, or else `today`, ordered by name without
	/// regard to case, unnamed ones last, unless the listing orders them by a field.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn list(
		&self,
		space: SpaceId,
		listing: &Listing,
		today: Day,
	) -> Result<Vec<Listed>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let known = (&open.types, &self.languages);
		let day = listing.on.unwrap_or(today);
		let mut listed = if listing.conditions.is_empty() {
			self.entities(space, listing.of_type.as_ref())?
		} else {
			open.stored.reading(|stored| {
				// Each condition's value, in the value index, narrows the candidates, except a
				// name, compared by its full text, and a decimal or a quantity, compared by
				// size, which the index does not hold apart.
				let mut candidates: Option<BTreeSet<EntityId>> = None;
				for (field, value) in &listing.conditions {
					if matches!(
						value,
						Value::Name(_) | Value::Decimal(_) | Value::Quantity(_)
					) {
						continue;
					}
					let mut found = BTreeSet::new();
					for target in targets(state, value)? {
						found.extend(stored.find(field, &target)?);
					}
					candidates = Some(match candidates {
						None => found,
						Some(before) => before.intersection(&found).copied().collect(),
					});
				}
				let candidates = match (candidates, &listing.of_type) {
					(Some(candidates), _) => candidates,
					(None, Some(of_type)) => with_type(stored, of_type)?,
					(None, None) => things(state)?,
				};
				let mut kept = BTreeSet::new();
				for entity in candidates {
					let entity = state.canonical(entity)?;
					if kept.contains(&entity)
						|| state.is_deleted(entity)?
						|| kept_apart(state, entity)?
					{
						continue;
					}
					if let Some(of_type) = &listing.of_type
						&& !state.types(entity)?.contains(of_type)
					{
						continue;
					}
					let mut holds = true;
					for (field, value) in &listing.conditions {
						if !shows(state, known, (entity, day), field, value)? {
							holds = false;
							break;
						}
					}
					if holds {
						kept.insert(entity);
					}
				}
				listed_from(state, known.0, known.1, (kept, PART))
			})?
		};
		// On another day, entities go by the names they had then.
		if listing.on.is_some() {
			open.stored.reading(|_| {
				for entity in &mut listed {
					entity.name = name_on(state, known.0, known.1, entity.id, day)?;
				}
				Ok::<_, NodeError>(())
			})?;
			listed.sort_by_cached_key(|entity| {
				let name = entity.name.as_deref().map(str::to_lowercase);
				(name.is_none(), name, entity.id)
			});
		}
		let Some(field) = &listing.order_by else {
			return Ok(listed);
		};
		open.stored.reading(|_| {
			let mut keyed = Vec::with_capacity(listed.len());
			for entity in listed {
				let key = sort_key(state, known, (entity.id, day), field)?;
				keyed.push((key, entity));
			}
			// A stable sort keeps entities with the same value in the order of their names.
			keyed.sort_by(|(left, _), (right, _)| match (left, right) {
				(Some(left), Some(right)) => left.cmp(right),
				(Some(_), None) => Ordering::Less,
				(None, Some(_)) => Ordering::Greater,
				(None, None) => Ordering::Equal,
			});
			Ok(keyed.into_iter().map(|(_, entity)| entity).collect())
		})
	}
}

impl Library {
	/// The types that the entities of a type have, that type too, deleted entities aside:
	/// what else they are, such as editions that are audiobooks.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn types_with(
		&self,
		space: SpaceId,
		of_type: &TypeName,
	) -> Result<BTreeSet<TypeName>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		open.stored.reading(|stored| {
			let mut held = BTreeSet::from([of_type.clone()]);
			for entity in with_type(stored, of_type)? {
				held.extend(state.types(entity)?);
			}
			Ok(held)
		})
	}
}

/// The values whose statements can meet a condition of `value`: the value, and for a
/// reference, a reference to each entity merged with the one it refers to.
fn targets<S: Statements>(state: &State<S>, value: &Value) -> Result<Vec<Value>, NodeError>
where
	NodeError: From<S::Error>,
{
	let Some(entity) = value.as_reference() else {
		return Ok(vec![value.clone()]);
	};
	let canonical = state.canonical(entity)?;
	let mut targets = vec![Value::Reference(canonical)];
	for member in state.merged(canonical)? {
		if member != canonical {
			targets.push(Value::Reference(member));
		}
	}
	if !targets.contains(value) {
		targets.push(value.clone());
	}
	Ok(targets)
}

/// The values a field of an entity shows on a day, as [`State::resolve_counted`] chooses
/// them.
fn shown<S: Statements>(
	state: &State<S>,
	(types, languages): (&Types, &Languages),
	(entity, day): (EntityId, Day),
	field: &FieldName,
) -> Result<Vec<Value>, NodeError>
where
	NodeError: From<S::Error>,
{
	let definition = types.field(field);
	let (shown, _) = state.resolve_counted(entity, field, definition.as_ref(), day, languages)?;
	Ok(shown
		.into_iter()
		.map(|(statement, _)| statement.value.clone())
		.collect())
}

/// Whether a field of an entity shows a value on a day: a reference to the entity, or to
/// one merged with it, unless the entity is merged with it itself; a name with the same
/// full text; and a number of the same size, a quantity in the same unit too.
fn shows<S: Statements>(
	state: &State<S>,
	known: (&Types, &Languages),
	at: (EntityId, Day),
	field: &FieldName,
	value: &Value,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	let (entity, _) = at;
	for shown in shown(state, known, at, field)? {
		let same = match (&shown, value) {
			(Value::Reference(shown), Value::Reference(wanted)) => {
				// As links are counted: an entity merged into the one it refers to does not
				// link to itself.
				let wanted = state.canonical(*wanted)?;
				state.canonical(*shown)? == wanted && may_link(state, entity, wanted)?
			}
			(Value::Name(shown), Value::Name(wanted)) => shown.full() == wanted.full(),
			(Value::Quantity(shown), Value::Quantity(wanted)) => {
				shown.unit() == wanted.unit() && size(shown.number()) == size(wanted.number())
			}
			(shown, wanted) => match (number(shown), number(wanted)) {
				(Some(shown), Some(wanted)) => shown == wanted,
				_ => shown == wanted,
			},
		};
		if same {
			return Ok(true);
		}
	}
	Ok(false)
}

/// An integer or a decimal as its size: digits without trailing zeros, and a power of ten,
/// so that 12.5 and 12.50 are one size.
fn number(value: &Value) -> Option<(i128, i64)> {
	match value {
		Value::Integer(integer) => Some(size(
			Decimal::new(i128::from(*integer), 0).expect("an integer is a decimal"),
		)),
		Value::Decimal(decimal) => Some(size(*decimal)),
		_ => None,
	}
}

/// A decimal's size, as [`number`] gives it.
fn size(decimal: Decimal) -> (i128, i64) {
	let (mut mantissa, mut exponent) = (decimal.mantissa(), decimal.exponent());
	if mantissa == 0 {
		return (0, 0);
	}
	while mantissa % 10 == 0 {
		mantissa /= 10;
		exponent += 1;
	}
	(mantissa, exponent)
}

/// What an entity is ordered by: the first value a field shows, if one can be ordered.
#[derive(Clone, Debug, PartialEq)]
enum SortKey {
	/// No before yes.
	Boolean(bool),
	/// Integers and decimals, by size.
	Number(f64),
	/// Quantities: those in the field's unit by size, then those in other units, by unit
	/// and size, since no unit is converted.
	Measure(bool, String, f64),
	/// Dates, by when they start, as a day and a second of it: an interval open at its
	/// start, with none, before every other date.
	Date(Option<(Day, u32)>),
	/// Text, names and the names of what references refer to, without regard to case.
	Text(String),
}

impl SortKey {
	/// The place of the key's kind, so that keys of different kinds are ordered too.
	fn rank(&self) -> u8 {
		match self {
			Self::Boolean(_) => 0,
			Self::Number(_) => 1,
			Self::Measure(..) => 2,
			Self::Date(..) => 3,
			Self::Text(_) => 4,
		}
	}

	fn cmp(&self, other: &Self) -> Ordering {
		match (self, other) {
			(Self::Boolean(left), Self::Boolean(right)) => left.cmp(right),
			(Self::Number(left), Self::Number(right)) => left.total_cmp(right),
			(Self::Measure(other, unit, left), Self::Measure(other_right, unit_right, right)) => {
				(other, unit)
					.cmp(&(other_right, unit_right))
					.then_with(|| left.total_cmp(right))
			}
			(Self::Date(left), Self::Date(right)) => left.cmp(right),
			(Self::Text(left), Self::Text(right)) => left.cmp(right),
			_ => self.rank().cmp(&other.rank()),
		}
	}
}

/// The key that orders an entity by a field: the first value it shows, if it can be
/// ordered.
fn sort_key<S: Statements>(
	state: &State<S>,
	known: (&Types, &Languages),
	at: (EntityId, Day),
	field: &FieldName,
) -> Result<Option<SortKey>, NodeError>
where
	NodeError: From<S::Error>,
{
	let Some(value) = shown(state, known, at, field)?.into_iter().next() else {
		return Ok(None);
	};
	let decimal = |mantissa: i128, exponent: i64| {
		#[expect(clippy::cast_precision_loss, reason = "close enough to order by")]
		let mantissa = mantissa as f64;
		mantissa * 10_f64.powi(i32::try_from(exponent).unwrap_or(0))
	};
	Ok(match value {
		Value::Boolean(boolean) => Some(SortKey::Boolean(boolean)),
		#[expect(clippy::cast_precision_loss, reason = "close enough to order by")]
		Value::Integer(integer) => Some(SortKey::Number(integer as f64)),
		Value::Decimal(number) => Some(SortKey::Number(decimal(
			number.mantissa(),
			number.exponent(),
		))),
		Value::Quantity(quantity) => {
			let number = quantity.number();
			let (types, _) = known;
			let unit = types
				.field(field)
				.and_then(|definition| definition.unit().map(str::to_owned));
			Some(SortKey::Measure(
				unit.as_deref() != Some(quantity.unit()),
				quantity.unit().to_owned(),
				decimal(number.mantissa(), number.exponent()),
			))
		}
		Value::Date(date) => Some(match date.start() {
			Start::BeginningOfTime => SortKey::Date(None),
			Start::At { day, second } => SortKey::Date(Some((day, second.unwrap_or(0)))),
		}),
		Value::Text(text) => Some(SortKey::Text(text.to_lowercase())),
		Value::Name(name) => Some(SortKey::Text(name.full().to_lowercase())),
		Value::Reference(entity) => {
			let (types, languages) = known;
			display_name(state, types, languages, state.canonical(entity)?)?
				.map(|name| SortKey::Text(name.to_lowercase()))
		}
		Value::Blob(_) | Value::Unknown(_) => None,
	})
}
