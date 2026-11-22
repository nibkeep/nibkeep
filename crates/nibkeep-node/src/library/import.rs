//! Imports: an export brought into a space, adding what it holds and the space lacks
//! (`docs/spec/export.md`, section 7).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::Path;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{LINKED_PERSON, PERSON, USER};
use nibkeep_log::History as _;
use nibkeep_model::schema::{FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::storage::Statements as _;
use nibkeep_model::{FieldName, Operation, State, Statement, TypeName, Types, Value, fields};
use nibkeep_time::Timestamp;
use serde_json::Value as Json;

use super::entities::display_name;
use super::export::{
	ExportProblem, Manifest, OWN_RECORDS, blake3_of, check_sums, files_of, read_manifest,
};
use super::json;
use super::{Described, Library};
use crate::NodeError;

/// How many entities' changes an import saves in one batch.
const BATCH: usize = 1_000;

/// What an import brought in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
	/// How many entities it read, those left out aside: NibKeep's own records, and the
	/// definitions of the types left out.
	pub entities: usize,
	/// How many statements it added.
	pub added: usize,
	/// How many statements it replaced, as the export shows them replaced.
	pub replaced: usize,
	/// How many entities it left out: NibKeep's own records of users, devices, replicas and
	/// spaces.
	pub skipped: usize,
	/// How many files it added.
	pub files: usize,
	/// The types whose definitions it left out, as the space defines them otherwise, such as
	/// with a field of another kind.
	pub types_left_out: Vec<TypeName>,
	/// The person of the export's user, whom it took for the library's user, in place of the
	/// person that the new library made, which it deleted.
	pub person: Option<Described>,
}

/// A line of an export's entities, as an import reads it: its entity, whether it is one of
/// NibKeep's own records, and its statements, each with whether the export shows it replaced.
struct Line {
	entity: EntityId,
	own_record: bool,
	held: Vec<(Statement, bool)>,
}

/// What an entity of an export defines, if it defines a type or a field.
enum Definition {
	/// A type, by its name.
	Type(TypeName),
	/// A field of the type that the entity it refers to defines.
	Field(EntityId),
}

impl Line {
	/// The statements of the line that the export shows current.
	fn current(&self) -> impl Iterator<Item = &Statement> {
		self.held
			.iter()
			.filter(|(_, replaced)| !replaced)
			.map(|(statement, _)| statement)
	}

	/// Of a record of a user, the persons that the user is linked to.
	fn linked_persons(&self) -> Option<BTreeSet<EntityId>> {
		let user = self.own_record
			&& self.current().any(|statement| {
				statement.field == fields::TYPE && statement.value.as_text() == Some(USER.as_str())
			});
		user.then(|| {
			self.current()
				.filter(|statement| statement.field == LINKED_PERSON)
				.filter_map(|statement| statement.value.as_reference())
				.collect()
		})
	}

	/// What the entity defines, by its current statements, if it is not one of NibKeep's own
	/// records and defines a type or a field. Of competing names, the one in the statement
	/// with the smallest ID counts, as a space reads its definitions.
	fn definition(&self) -> Option<Definition> {
		if self.own_record {
			return None;
		}
		let first = |field: &FieldName| {
			self.current()
				.filter(|statement| statement.field == *field)
				.min_by_key(|statement| statement.id)
		};
		let is = |kind: &TypeName| {
			self.current().any(|statement| {
				statement.field == fields::TYPE && statement.value.as_text() == Some(kind.as_str())
			})
		};
		if is(&TYPE_DEFINITION) {
			first(&fields::TYPE_NAME)
				.and_then(|name| name.value.as_text()?.parse().ok())
				.map(Definition::Type)
		} else if is(&FIELD_DEFINITION) {
			first(&fields::FIELD_OF)
				.and_then(|of| of.value.as_reference())
				.map(Definition::Field)
		} else {
			None
		}
	}
}

impl Library {
	/// Imports an export into a space, as `docs/spec/export.md` says: its files, and each of
	/// its statements that the space lacks, with their IDs, replaced as the export shows them;
	/// never NibKeep's own records of users, devices, replicas and spaces, nor the fields that
	/// govern a space. Importing an export again adds nothing. Changes are saved in batches.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space; if the folder holds no export
	/// that this version reads, one whose sums or counts do not match, or one of this space
	/// made before one of its purges, of which nothing is imported; or if a file cannot be
	/// read or the changes saved, and then how far the import got.
	pub fn import(
		&mut self,
		space: SpaceId,
		directory: &Path,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Imported, NodeError> {
		let manifest = read_manifest(directory).map_err(NodeError::ExportDamaged)?;
		self.check_not_purged_since(space, &manifest)?;
		let unmatched = check_sums(directory)?
			.into_iter()
			.find_map(|problem| match problem {
				ExportProblem::WrongSum(path) | ExportProblem::Unlisted(path) => Some(path),
				_ => None,
			});
		if let Some(path) = unmatched {
			return Err(NodeError::ExportDamaged(format!(
				"the sums do not match {path}"
			)));
		}
		// A first reading checks every line, so that a damaged one imports nothing, and keeps
		// the definitions of types and fields, which are few and come in first; the second
		// imports the rest, a batch at a time, without holding the export in memory.
		let path = directory.join("entities.jsonl");
		let mut lines = 0;
		let mut definitions = Vec::new();
		let mut users = Vec::new();
		for_lines(&path, &mut |number, line| {
			let line = read_line(line).map_err(|why| damaged_line(number, &why))?;
			if line.definition().is_some() {
				definitions.push(line);
			} else if let Some(persons) = line.linked_persons() {
				users.push(persons);
			}
			lines += 1;
			Ok(())
		})?;
		// The person that the export's user is, of an export of one user, linked to one.
		let person = match users.as_slice() {
			[persons] if persons.len() == 1 => persons.first().copied(),
			_ => None,
		};
		if manifest.entities != Some(lines) {
			return Err(NodeError::ExportDamaged(
				"its manifest counts another number of entities".to_owned(),
			));
		}
		let mut imported = Imported {
			files: self.import_files(space, directory, &manifest, wall_millis, rng)?,
			..Imported::default()
		};
		let mut ended =
			self.import_definitions(space, definitions, &mut imported, wall_millis, rng);
		if ended.is_ok() {
			let mut batch = Vec::with_capacity(BATCH);
			let read = for_lines(&path, &mut |number, line| {
				let line = read_line(line).map_err(|why| damaged_line(number, &why))?;
				if line.definition().is_some() {
					return Ok(());
				}
				batch.push(line);
				if batch.len() == BATCH {
					let lines = std::mem::take(&mut batch);
					self.import_batch(space, &lines, &mut imported, wall_millis, rng)?;
				}
				Ok(())
			});
			ended = read
				.and_then(|()| self.import_batch(space, &batch, &mut imported, wall_millis, rng));
		}
		if ended.is_ok()
			&& let Some(person) = person
		{
			ended = self
				.take_person(space, person, wall_millis, rng)
				.map(|taken| imported.person = taken);
		}
		// What was saved before a failure stays; importing again brings in the rest.
		ended.map_err(|error| NodeError::ImportStopped {
			entities: imported.entities + imported.skipped,
			error: Box::new(error),
		})?;
		Ok(imported)
	}

	/// Refuses an export of this space made before one of its purges, which may hold what
	/// the purge destroyed. Statements purged leave no trace of their IDs, so the time
	/// decides.
	fn check_not_purged_since(&self, space: SpaceId, manifest: &Manifest) -> Result<(), NodeError> {
		if manifest.space != space {
			return Ok(());
		}
		let log = self.space(space)?.stored.log();
		for purge in log.history().purging()? {
			if log
				.change(purge)?
				.is_some_and(|change| change.timestamp().millis() > manifest.as_of)
			{
				return Err(NodeError::ExportDamaged(
					"the space purged values since it was made, which it may hold".to_owned(),
				));
			}
		}
		Ok(())
	}

	/// Imports the files of an export that the space does not hold, each checked against
	/// the hash it is named by, and returns how many it imported.
	fn import_files(
		&mut self,
		space: SpaceId,
		directory: &Path,
		manifest: &Manifest,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let names = files_of(&directory.join("files"))?;
		if manifest.files != Some(names.len()) {
			return Err(NodeError::ExportDamaged(
				"its manifest counts another number of files".to_owned(),
			));
		}
		let mut imported = 0;
		for relative in names {
			let damaged =
				|| NodeError::ExportDamaged(format!("files/{relative} is named by no hash"));
			let (algorithm, digest) = relative.split_once('/').ok_or_else(damaged)?;
			let hash: Hash = format!("{algorithm}:{digest}")
				.parse()
				.map_err(|_| damaged())?;
			if self.space(space)?.stored.holds_ordinary_blob(&hash) {
				continue;
			}
			let path = directory.join("files").join(&relative);
			if blake3_of(&path).map_err(NodeError::file(&path))? != hash {
				return Err(NodeError::ExportDamaged(format!(
					"files/{relative} does not have the hash it is named by"
				)));
			}
			let file = File::open(&path).map_err(NodeError::file(&path))?;
			self.import_blob(space, file, wall_millis, rng)?;
			imported += 1;
		}
		Ok(imported)
	}

	/// Imports the definitions of types and fields of an export (`docs/spec/export.md`,
	/// section 7): as they are, with their IDs, where the space has their type's entity or
	/// defines no such type; else adding to the space's own definition what they hold and it
	/// lacks, as defining the type again does, or leaving the type out if the space cannot
	/// take it as it is.
	fn import_definitions(
		&mut self,
		space: SpaceId,
		lines: Vec<Line>,
		imported: &mut Imported,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		// The export's entities of types that the space defines with entities of its own.
		let mut merged = BTreeMap::new();
		{
			let open = self.space(space)?;
			let storage = open.stored.log().state().storage();
			for line in &lines {
				if let Some(Definition::Type(name)) = line.definition()
					&& open.types.defines_in_space(&name)
					&& storage.of_entity(line.entity)?.is_empty()
				{
					merged.insert(line.entity, name);
				}
			}
		}
		let (merging, as_they_are): (Vec<Line>, Vec<Line>) =
			lines.into_iter().partition(|line| match line.definition() {
				Some(Definition::Type(_)) => merged.contains_key(&line.entity),
				Some(Definition::Field(of)) => merged.contains_key(&of),
				None => false,
			});
		// Those that come in as they are go together in one change, as definitions do.
		let mut operations = Vec::new();
		for line in &as_they_are {
			operations.extend(self.lacking(space, line, imported)?);
		}
		if !operations.is_empty() {
			self.commit(space, operations, wall_millis, rng)?;
		}
		// What the rest define, read as a space reads its own definitions, with how many lines
		// each type takes.
		let mut state = State::new();
		let mut lines_of: BTreeMap<TypeName, usize> = BTreeMap::new();
		for line in &merging {
			let name = match line.definition() {
				Some(Definition::Type(name)) => Some(name),
				Some(Definition::Field(of)) => merged.get(&of).cloned(),
				None => None,
			};
			if let Some(name) = name {
				*lines_of.entry(name).or_default() += 1;
			}
			for (statement, replaced) in &line.held {
				if !replaced {
					let Ok(()) = state.apply(
						&Operation::assert(statement.clone()),
						Timestamp::new(wall_millis, 0),
					);
				}
			}
		}
		let mut defined = Types::new();
		let Ok(()) = defined.read(&state);
		let mut read = merging.len();
		for definition in defined.defined_by_space() {
			let careful = {
				let types = &self.space(space)?.types;
				let careful = types.careful(definition);
				types.check(&careful).is_ok().then_some(careful)
			};
			let Some(careful) = careful else {
				read -= lines_of.get(definition.name()).copied().unwrap_or(0);
				imported.types_left_out.push(definition.name().clone());
				continue;
			};
			imported.added += self.define_lacking(space, &careful, wall_millis, rng)?;
		}
		imported.entities += read;
		Ok(())
	}

	/// Takes the person of the export's user for the library's user (`docs/spec/export.md`,
	/// section 7), in the personal space, where a new library links its user to a person:
	/// links the user to that person, and deletes the one that the user was linked to, if it
	/// holds only its type and a name, as a new library makes it, and no value refers to it
	/// but that link. Returns the person taken, if any.
	fn take_person(
		&mut self,
		space: SpaceId,
		person: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Option<Described>, NodeError> {
		if space != self.info.personal {
			return Ok(None);
		}
		let user = self.info.user;
		let (link, made, name) = {
			let open = self.space(space)?;
			let state = open.stored.log().state();
			let storage = state.storage();
			if state.fields(person)?.is_empty()
				|| state.is_deleted(person)?
				|| state
					.types(person)?
					.iter()
					.any(|kind| OWN_RECORDS.contains(kind))
			{
				return Ok(None);
			}
			let links = state.current(user, &LINKED_PERSON)?;
			let [link] = links.as_slice() else {
				return Ok(None);
			};
			let Some(made) = link.value.as_reference() else {
				return Ok(None);
			};
			if link.entity != user || state.canonical(made)? == state.canonical(person)? {
				return Ok(None);
			}
			// Only its type, a person, and a name, never replaced, nor merged with another.
			let statements = storage.of_entity(made)?;
			let mut only_named = statements.len() == 2 && state.merged(made)?.len() == 1;
			for (field, ids) in &statements {
				let [id] = ids.as_slice() else {
					only_named = false;
					break;
				};
				only_named &= (*field == fields::NAME
					|| (*field == fields::TYPE
						&& storage.get(*id)?.is_some_and(|recorded| {
							recorded.statement.value.as_text() == Some(PERSON.as_str())
						})))
					&& !storage.is_superseded(*id)?;
			}
			let referrers = state.referrers(made)?;
			if !only_named || !matches!(referrers.as_slice(), [only] if only.id == link.id) {
				return Ok(None);
			}
			let name = display_name(state, &open.types, &self.languages, person)?;
			(link.id, made, name)
		};
		let linked = Statement::new(
			StatementId::random(rng),
			user,
			LINKED_PERSON,
			Value::Reference(person),
		);
		let deleted = Statement::new(
			StatementId::random(rng),
			made,
			fields::DELETED,
			Value::Boolean(true),
		);
		// A change for each, saved together: both or neither.
		let operations = vec![
			Operation::replace(linked, [link]),
			Operation::assert(deleted),
		];
		self.commit(space, operations, wall_millis, rng)?;
		Ok(Some(Described { id: person, name }))
	}

	/// Imports lines of an export in one batch.
	fn import_batch(
		&mut self,
		space: SpaceId,
		lines: &[Line],
		imported: &mut Imported,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if lines.is_empty() {
			return Ok(());
		}
		self.batch(space, |library| {
			for line in lines {
				library.import_entity(space, line, imported, wall_millis, rng)?;
			}
			Ok(())
		})
	}

	/// Imports one entity of an export: the statements of its line that the space lacks,
	/// and the replacements that it lacks, in one change.
	fn import_entity(
		&mut self,
		space: SpaceId,
		line: &Line,
		imported: &mut Imported,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let operations = self.lacking(space, line, imported)?;
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// What the space lacks of a line of an export, counted in `imported`: the assertions of
	/// the statements it does not hold, and a retraction of those that the export shows
	/// replaced and it does not; nothing of NibKeep's own records, nor of the fields that
	/// govern a space.
	fn lacking(
		&self,
		space: SpaceId,
		line: &Line,
		imported: &mut Imported,
	) -> Result<Vec<Operation>, NodeError> {
		if line.own_record {
			imported.skipped += 1;
			return Ok(Vec::new());
		}
		imported.entities += 1;
		let mut operations = Vec::new();
		let mut retract: Vec<StatementId> = Vec::new();
		{
			let storage = self.space(space)?.stored.log().state().storage();
			for (statement, replaced) in &line.held {
				if fields::is_governing(&statement.field) {
					continue;
				}
				let known = storage.contains(statement.id)?;
				if !known {
					operations.push(Operation::assert(statement.clone()));
					imported.added += 1;
				}
				if *replaced && (!known || !storage.is_superseded(statement.id)?) {
					retract.push(statement.id);
				}
			}
		}
		if let Some((statement, _)) = line.held.first()
			&& !retract.is_empty()
		{
			imported.replaced += retract.len();
			operations.push(Operation::retract(statement.entity, retract));
		}
		Ok(operations)
	}
}

/// Reads a line of an export's entities.
fn read_line(line: &str) -> Result<Line, String> {
	let line: Json = serde_json::from_str(line).map_err(|_| "no JSON".to_owned())?;
	let entity: EntityId = line["id"]
		.as_str()
		.and_then(|id| id.parse().ok())
		.ok_or("no entity's ID")?;
	let is_own = |kind: &str| OWN_RECORDS.iter().any(|own| own.as_str() == kind);
	let mut own_record = line["types"]
		.as_array()
		.is_some_and(|types| types.iter().filter_map(Json::as_str).any(is_own));
	let mut held = Vec::new();
	for field in line["fields"].as_array().ok_or("no fields")? {
		let name: FieldName = field["name"]
			.as_str()
			.and_then(|name| name.parse().ok())
			.ok_or("a field without a name")?;
		for value in field["values"].as_array().ok_or("a field without values")? {
			let object = value.as_object().ok_or("a value that is no object")?;
			let id: StatementId = value["statement"]
				.as_str()
				.and_then(|id| id.parse().ok())
				.ok_or("a value without its statement's ID")?;
			// Qualifiers are left out where there are none; an export may write them empty.
			let qualifiers = match value.get("qualifiers") {
				Some(qualifiers) => json::parse_qualifiers(qualifiers)?,
				None => nibkeep_model::Qualifiers::new(),
			};
			let statement = Statement::new(id, entity, name.clone(), json::parse_value(object)?)
				.with_qualifiers(qualifiers);
			// A record of who uses a library is one by any type it was given, whatever the
			// line lists.
			if statement.field == fields::TYPE && statement.value.as_text().is_some_and(is_own) {
				own_record = true;
			}
			let replaced = value
				.get("replaced")
				.is_some_and(|replaced| !replaced.is_null());
			held.push((statement, replaced));
		}
	}
	Ok(Line {
		entity,
		own_record,
		held,
	})
}

/// Calls `each` with every line of a file and its number from 1.
fn for_lines(
	path: &Path,
	each: &mut dyn FnMut(usize, &str) -> Result<(), NodeError>,
) -> Result<(), NodeError> {
	let file = File::open(path).map_err(NodeError::file(path))?;
	for (index, line) in io::BufRead::lines(io::BufReader::new(file)).enumerate() {
		let line = line.map_err(NodeError::file(path))?;
		each(index + 1, &line)?;
	}
	Ok(())
}

/// The error of a line of an export's entities that cannot be read.
fn damaged_line(number: usize, why: &str) -> NodeError {
	NodeError::ExportDamaged(format!("line {number} of entities.jsonl: {why}"))
}
