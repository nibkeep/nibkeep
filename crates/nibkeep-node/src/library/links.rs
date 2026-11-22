//! Links read from their other end, and relationships recorded twice. A link is a reference
//! statement on one entity, stored in one direction; the state file finds the statements
//! that refer to an entity, so links are read in both (`docs/spec/model.md`, section 10).

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::schema::{FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldDef, FieldName, Languages, Resolution, State, TypeName, Types, Value};
use nibkeep_time::Day;

use super::Library;
use super::entities::{Listed, More, Searched, defines_types, display_name, resolve};
use crate::NodeError;

/// The entities that link to an entity through one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Links {
	/// The field that links them.
	pub field: FieldName,
	/// The name that the field goes by from its other end, if its types give one, such as
	/// `std.author-of` for `std.author`.
	pub reverse: Option<FieldName>,
	/// Whether the field is a role, so that the entities that link are relationships.
	pub role: bool,
	/// How many entities link through the field.
	pub count: usize,
	/// The first of them by name, without regard to case, unnamed ones last, as many as
	/// were asked for.
	pub entities: Vec<Linking>,
}

/// An entity that links to another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linking {
	/// The entity, or of entities merged together, the one they were merged into.
	pub id: EntityId,
	/// The name to show for it, if it has one.
	pub name: Option<String>,
	/// Its types.
	pub types: Vec<TypeName>,
	/// Of a relationship, which links through a role: the participants of its other roles
	/// on the day, by role, ordered by the role's name.
	pub participants: Vec<(FieldName, Vec<EntityId>)>,
}

impl Library {
	/// The entities of a space that link to an entity on `day`, grouped by the field that
	/// links them and ordered by its name: those whose field shows a reference to it, or to
	/// an entity merged into it, on that day. Deleted entities, and those that define types,
	/// are left out. Of each field, `limit` entities are given, and how many there are.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn links_to(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: Day,
		limit: usize,
	) -> Result<Vec<Links>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|_| {
			links_to(
				open.stored.log().state(),
				&open.types,
				&self.languages,
				(entity, Some(day)),
				(day, limit),
			)
		})
	}

	/// The entities of a space that link to an entity on any day, as [`Library::links_to`]
	/// gives those that link on one: those whose field shows a reference to it on some day,
	/// as a publisher that had someone as its head for some years, and has another now. A
	/// relationship's participants are those on `day`.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn links_to_on_any_day(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: Day,
		limit: usize,
	) -> Result<Vec<Links>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|_| {
			links_to(
				open.stored.log().state(),
				&open.types,
				&self.languages,
				(entity, None),
				(day, limit),
			)
		})
	}

	/// The entities of a space, not deleted, that are relationships of all these types
	/// with the participants that these values give in roles: those that hold, of each such
	/// role, every entity given, on whichever days. None if the values give no participant
	/// in a role. A relationship is an entity, so two devices that record one make two,
	/// which this finds, as recorded rather than as shown on some day.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn same_relationship(
		&self,
		space: SpaceId,
		kinds: &[TypeName],
		values: &[(FieldName, Value)],
	) -> Result<Vec<EntityId>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let types = &open.types;
		let mut roles: BTreeMap<&FieldName, BTreeSet<EntityId>> = BTreeMap::new();
		for (field, value) in values {
			if let Some(participant) = value.as_reference()
				&& types.field(field).is_some_and(|field| field.is_role())
			{
				roles
					.entry(field)
					.or_default()
					.insert(state.canonical(participant)?);
			}
		}
		let Some((first_role, first_participants)) = roles.first_key_value() else {
			return Ok(Vec::new());
		};
		let Some(&first) = first_participants.first() else {
			return Ok(Vec::new());
		};
		let mut same = BTreeSet::new();
		for statement in state.referrers(first)? {
			if statement.field != **first_role {
				continue;
			}
			let candidate = state.canonical(statement.entity)?;
			if same.contains(&candidate) || !may_link(state, candidate, first)? {
				continue;
			}
			let held = state.types(candidate)?;
			if !kinds.iter().all(|kind| held.contains(kind)) {
				continue;
			}
			let mut holds_all = true;
			for (field, participants) in &roles {
				let mut current = BTreeSet::new();
				for statement in state.current(candidate, field)? {
					if let Some(reference) = statement.value.as_reference() {
						current.insert(state.canonical(reference)?);
					}
				}
				if !participants.is_subset(&current) {
					holds_all = false;
					break;
				}
			}
			if holds_all {
				same.insert(candidate);
			}
		}
		Ok(same.into_iter().collect())
	}
}

/// Whether an entity counts as linking to another: it is not deleted, defines no types, and
/// is not the other entity, which an entity merged into the one it referred to would be.
pub(crate) fn may_link<S: Statements>(
	state: &State<S>,
	holder: EntityId,
	entity: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(holder != entity && !state.is_deleted(holder)? && !defines_types(state, holder)?)
}

/// Whether one of the statements shown refers to the entity, or to one merged into it.
fn refers_to<S: Statements>(
	state: &State<S>,
	shown: &[std::borrow::Cow<'_, nibkeep_model::Statement>],
	entity: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	for statement in shown {
		if let Some(reference) = statement.value.as_reference()
			&& state.canonical(reference)? == entity
		{
			return Ok(true);
		}
	}
	Ok(false)
}

/// The participants of a relationship on `day` in its roles other than `through`, by role,
/// ordered by the role's name.
fn participants<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	relationship: EntityId,
	through: &FieldName,
	day: Day,
) -> Result<Vec<(FieldName, Vec<EntityId>)>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut participants = Vec::new();
	for field in state.fields(relationship)? {
		if field == *through || !types.field(&field).is_some_and(|field| field.is_role()) {
			continue;
		}
		let (shown, _) = resolve(state, types, languages, relationship, &field, day)?;
		let mut entities = Vec::new();
		for statement in &shown {
			if let Some(reference) = statement.value.as_reference() {
				let reference = state.canonical(reference)?;
				if !entities.contains(&reference) {
					entities.push(reference);
				}
			}
		}
		if !entities.is_empty() {
			participants.push((field, entities));
		}
	}
	Ok(participants)
}

/// What a search lists of what it finds, given the entities found by their words, in order:
/// each of them, or the file it is an origin of, then the entities found through links, each
/// with the entity it was found through, as [`Library::search`] says; of those, the first
/// `first` that `keep` keeps, given each one's ID and types, and how many more it keeps.
///
/// Links come after every entity found by its words, so they are followed only when no more
/// than `first` of those are kept, and the count is then exact. Otherwise it is of those
/// found by their words alone, at least as many as there are. Links are those of current
/// statements, which hold on some day, as the words that a search finds are.
///
/// The entities listed are named, and so are, to order them, the others of a group found
/// through one entity that the page ends in, and those that stand for an entity found, while
/// the page is not full.
pub(crate) fn searched_through<S: Statements>(
	state: &State<S>,
	(types, languages): (&Types, &Languages),
	(hits, first): (&[EntityId], usize),
	keep: &mut Keep<'_>,
) -> Result<Searched, NodeError>
where
	NodeError: From<S::Error>,
{
	let listed = |id: EntityId,
	              kinds: BTreeSet<TypeName>,
	              via: Option<EntityId>|
	 -> Result<Listed, NodeError> {
		Ok(Listed {
			id,
			name: display_name(state, types, languages, id)?,
			types: kinds.into_iter().collect(),
			via,
		})
	};
	// Every hit is seen, so that an origin or a checkout is never found, through links
	// either.
	let mut seen: BTreeSet<EntityId> = hits.iter().copied().collect();
	let by_words = found_by_words(state, hits, &mut seen)?;
	// The first entities kept, and how many are kept in all.
	let mut found = Vec::new();
	let mut kept = 0_usize;
	for (entity, kinds) in &by_words {
		if keep(*entity, kinds)? {
			kept += 1;
			if found.len() < first {
				found.push(listed(*entity, kinds.clone(), None)?);
			}
		}
	}
	if kept > first {
		return Ok(Searched {
			listed: found,
			more: More::AtLeast(kept - first),
		});
	}
	// Adds the entities not found yet, through `via`: those kept, ordered by name, while
	// fewer than `first` are listed, or else only counted. Returns every new one, kept or
	// not, as the links of each are followed either way.
	let searched = types.searched_fields();
	let mut add = |entities: Vec<EntityId>,
	               via: EntityId,
	               (found, kept): (&mut Vec<Listed>, &mut usize)|
	 -> Result<Vec<EntityId>, NodeError> {
		let mut new = Vec::new();
		let mut listing = Vec::new();
		for entity in entities {
			if !seen.insert(entity) {
				continue;
			}
			new.push(entity);
			let kinds = state.types(entity)?;
			if keep(entity, &kinds)? {
				*kept += 1;
				if found.len() < first {
					listing.push(listed(entity, kinds, Some(via))?);
				}
			}
		}
		listing.sort_by_cached_key(|entity| by_name(entity.name.as_deref(), entity.id));
		found.extend(listing);
		Ok(new)
	};
	for (hit, _) in by_words {
		let (stand_ins, linked) = linking_for_search(state, (types, &searched), hit)?;
		let both: Vec<EntityId> = stand_ins.iter().chain(&linked).copied().collect();
		let listed_before = found.len();
		let added = add(both, hit, (&mut found, &mut kept))?;
		// Through each new entity that stands for the hit, in the order they are listed in,
		// kept or not, named as they were listed. Once the page is full, the order changes
		// nothing.
		let mut through: Vec<EntityId> = added
			.into_iter()
			.filter(|entity| stand_ins.contains(entity))
			.collect();
		if found.len() < first && through.len() > 1 {
			let mut ordered = Vec::with_capacity(through.len());
			for entity in through {
				let name = match found[listed_before..]
					.iter()
					.find(|listed| listed.id == entity)
				{
					Some(listed) => listed.name.clone(),
					None => display_name(state, types, languages, entity)?,
				};
				ordered.push((by_name(name.as_deref(), entity), entity));
			}
			ordered.sort();
			through = ordered.into_iter().map(|(_, entity)| entity).collect();
		}
		for stand_in in through {
			let (_, linked) = linking_for_search(state, (types, &searched), stand_in)?;
			add(
				linked.into_iter().collect(),
				stand_in,
				(&mut found, &mut kept),
			)?;
		}
	}
	found.truncate(first);
	Ok(Searched {
		more: More::Exactly(kept - found.len()),
		listed: found,
	})
}

/// The order of entities that link to another, found by a search or listed: by name without
/// regard to case, unnamed ones last, then by ID.
fn by_name(name: Option<&str>, id: EntityId) -> (bool, Option<String>, EntityId) {
	let name = name.map(str::to_lowercase);
	(name.is_none(), name, id)
}

/// Which entities a search lists and counts, given each one's ID and types.
pub(crate) type Keep<'a> = dyn FnMut(EntityId, &BTreeSet<TypeName>) -> Result<bool, NodeError> + 'a;

/// The entities that a search finds by their words, given what holds them, in order, each
/// with its types: each of them, or the file it is an origin or a checkout of, if that is
/// not seen yet nor deleted, which it then sees; never an entity that defines types, nor a
/// thumbnail, whose media type and way of making are no words of a picture.
fn found_by_words<S: Statements>(
	state: &State<S>,
	hits: &[EntityId],
	seen: &mut BTreeSet<EntityId>,
) -> Result<Vec<(EntityId, BTreeSet<TypeName>)>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut found = Vec::with_capacity(hits.len());
	for &hit in hits {
		// Each hit's types are read once, to tell an origin or a checkout from what it stands
		// for.
		let kinds = state.types(hit)?;
		// The entities that define types are never found, even by words that a version
		// which did not reserve their fields indexed, such as labels; nor the records of the
		// names of spaces, which name spaces, not entities.
		if kinds.contains(&TYPE_DEFINITION)
			|| kinds.contains(&FIELD_DEFINITION)
			|| kinds.contains(&nibkeep_std::THUMBNAIL)
			|| kinds.contains(&nibkeep_governance::SPACE_NAMES)
		{
			continue;
		}
		let of = if kinds.contains(&nibkeep_std::ORIGIN) {
			nibkeep_std::ORIGIN_OF
		} else if kinds.contains(&nibkeep_std::CHECKOUT) {
			nibkeep_std::CHECKOUT_OF
		} else {
			found.push((hit, kinds));
			continue;
		};
		for file in super::files::found_at(state, (hit, &of))? {
			if !seen.insert(file) || state.is_deleted(file)? {
				continue;
			}
			let kinds = state.types(file)?;
			if kinds.contains(&nibkeep_std::FILE) {
				found.push((file, kinds));
			}
		}
	}
	Ok(found)
}

/// The entities, not deleted and not defining types, that link to an entity through a
/// current statement of a field that searches go through: those that stand for it, and
/// those that link to it otherwise. Only those fields are read, through the index of
/// references by field.
fn linking_for_search<S: Statements>(
	state: &State<S>,
	(types, searched): (&Types, &BTreeSet<FieldName>),
	entity: EntityId,
) -> Result<(BTreeSet<EntityId>, BTreeSet<EntityId>), NodeError>
where
	NodeError: From<S::Error>,
{
	let mut stand_ins = BTreeSet::new();
	let mut linked = BTreeSet::new();
	for field in searched {
		let stands_for = types
			.field(field)
			.is_some_and(|definition| definition.is_stands_for());
		for statement in state.referrers_in(entity, field)? {
			let holder = state.canonical(statement.entity)?;
			if !may_link(state, holder, entity)? {
				continue;
			}
			if stands_for {
				stand_ins.insert(holder);
			} else {
				linked.insert(holder);
			}
		}
	}
	linked.retain(|holder| !stand_ins.contains(holder));
	Ok((stand_ins, linked))
}

/// The entities that link to an entity on a day, `shown`, as [`Library::links_to`] says, or
/// without one, on any day, as [`Library::links_to_on_any_day`] says; `limit` of each field,
/// with the participants of relationships on `day`.
pub(crate) fn links_to<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	(entity, shown): (EntityId, Option<Day>),
	(day, limit): (Day, usize),
) -> Result<Vec<Links>, NodeError>
where
	NodeError: From<S::Error>,
{
	let entity = state.canonical(entity)?;
	let mut linking: BTreeMap<FieldName, BTreeSet<EntityId>> = BTreeMap::new();
	let mut checked: BTreeSet<(EntityId, FieldName)> = BTreeSet::new();
	for statement in state.referrers(entity)? {
		let holder = state.canonical(statement.entity)?;
		if !checked.insert((holder, statement.field.clone())) || !may_link(state, holder, entity)? {
			continue;
		}
		// A link counts on the days its field shows it, as any value does: on any day, as
		// one of the values the field shows on the days they hold, which leaves out those
		// written before the last of a field whose last writer wins.
		let counts = if let Some(day) = shown {
			let (shown, _) = resolve(state, types, languages, holder, &statement.field, day)?;
			refers_to(state, &shown, entity)?
		} else {
			let definition = types.field(&statement.field);
			let (Resolution::Values(shown) | Resolution::Competing(shown)) =
				state.resolve(holder, &statement.field, definition.as_ref())?;
			refers_to(state, &shown, entity)?
		};
		if counts {
			linking
				.entry(statement.field.clone())
				.or_default()
				.insert(holder);
		}
	}
	let mut links = Vec::new();
	for (field, holders) in linking {
		let definition = types.field(&field);
		let role = definition.as_ref().is_some_and(FieldDef::is_role);
		let mut entities = Vec::new();
		// Every name is needed to order them; the rest, only for those given.
		for id in &holders {
			entities.push(Linking {
				id: *id,
				name: display_name(state, types, languages, *id)?,
				types: Vec::new(),
				participants: Vec::new(),
			});
		}
		entities.sort_by_cached_key(|linking| by_name(linking.name.as_deref(), linking.id));
		entities.truncate(limit);
		for linking in &mut entities {
			linking.types = state.types(linking.id)?.into_iter().collect();
			if role {
				linking.participants =
					participants(state, types, languages, linking.id, &field, day)?;
			}
		}
		links.push(Links {
			reverse: definition.and_then(|field| field.reverse().cloned()),
			field,
			role,
			count: holders.len(),
			entities,
		});
	}
	Ok(links)
}
