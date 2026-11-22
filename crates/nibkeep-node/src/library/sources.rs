//! Sources: where files came from, as the person names them when they import them, such as
//! an old phone, a disc, a drive or Google Photos; each origin of a file found there names
//! its source ([technical design: importers]).
//!
//! [technical design: importers]: ../../../../docs/Technical-design.md#912-importers-and-where-files-came-from

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::{FieldName, Name, Operation, Statement, Value, fields};
use nibkeep_std::{DESCRIPTION, FOLDER, FROM_SOURCE, IN_FOLDER, SOURCE};
use nibkeep_time::Edtf;

use super::Library;
use super::entities::NewValue;
use super::policies::first_name;
use crate::NodeError;

/// A source of files, as a space records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
	/// Its record.
	pub id: EntityId,
	/// Its name.
	pub name: String,
	/// What it is, if the person said.
	pub description: Option<String>,
	/// How many origins name it: files found there.
	pub found: usize,
}

/// Where a file to add came from, beyond the folder it is read from: the source it was
/// imported from, and its path and name there; where it was found, if it is read from a copy
/// that is no original, as one unpacked from an archive; values that replace what it says
/// of itself, as an archive's metadata; and the folders to put it in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileFrom {
	/// The source it came from, by its record.
	pub source: Option<EntityId>,
	/// Its path within the source, its names joined by `/`.
	pub source_path: Option<String>,
	/// Its name in the source, where it differs from the name it is added under.
	pub source_name: Option<String>,
	/// Where it was found, if the file read is a copy of it that is no original: then the
	/// file goes into the store, as nothing keeps it where it was found.
	pub found: Option<FoundElsewhere>,
	/// Values that replace what the file says of itself, where it says otherwise: on a file
	/// the library holds already, only those that nobody changed since.
	pub values: Vec<(FieldName, Value)>,
	/// When it was taken, as where it was found says, such as a service that held it: it
	/// replaces when the photo says it was taken, unless the photo's own time is the same
	/// moment, written in its local time, which then stays, with its offset from UTC if it
	/// had none ([`taken_of`]).
	pub taken: Option<Edtf>,
	/// Folders to put it in.
	pub folders: Vec<EntityId>,
}

/// Where a file was found, when the file that is read is a copy of it, as one unpacked from
/// an archive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoundElsewhere {
	/// Where it was found: a path as people write it, such as the archive's path followed by
	/// the path within it.
	pub path: String,
	/// Its name there.
	pub name: Option<String>,
	/// When it was last changed there, in milliseconds since 1970.
	pub modified: Option<u64>,
	/// When it was made there, in milliseconds since 1970, if known.
	pub created: Option<u64>,
}

impl Library {
	/// The sources of a space's files, ordered by name, each with how many files were found
	/// there.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn sources(&self, space: SpaceId) -> Result<Vec<Source>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut sources = Vec::new();
		let mut seen = BTreeSet::new();
		for entity in open
			.stored
			.find(&fields::TYPE, &Value::Text(SOURCE.to_string()))?
		{
			let entity = state.canonical(entity)?;
			if !seen.insert(entity) || state.is_deleted(entity)? {
				continue;
			}
			let found = open
				.stored
				.find(&FROM_SOURCE, &Value::Reference(entity))?
				.into_iter()
				.filter(|origin| !state.is_deleted(*origin).unwrap_or(true))
				.count();
			sources.push(Source {
				id: entity,
				name: first_name(state, entity)?.unwrap_or_default(),
				description: state
					.current(entity, &DESCRIPTION)?
					.first()
					.and_then(|statement| statement.value.as_text().map(str::to_owned)),
				found,
			});
		}
		sources.sort_by_key(|source| (source.name.to_lowercase(), source.id));
		Ok(sources)
	}

	/// The folder of a space named `name` in the folder `parent`, or in none, made if the
	/// space has none, with `description` if it is made or has none, and whether it was made.
	/// An importer puts albums in folders so.
	///
	/// # Errors
	///
	/// Returns [`NodeError::EmptyName`] for an empty name, and an error if the space cannot be
	/// read or written.
	pub fn folder_named(
		&mut self,
		space: SpaceId,
		(name, parent): (&str, Option<EntityId>),
		description: Option<&str>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(EntityId, bool), NodeError> {
		let name = name.trim();
		if name.is_empty() {
			return Err(NodeError::EmptyName);
		}
		let description = description.map(str::trim).filter(|text| !text.is_empty());
		self.install_standard(space, &[FOLDER], wall_millis, rng)?;
		let open = self.space(space)?;
		let state = open.stored.log().state();
		for entity in open
			.stored
			.find(&fields::TYPE, &Value::Text(FOLDER.to_string()))?
		{
			let entity = state.canonical(entity)?;
			if state.is_deleted(entity)?
				|| first_name(state, entity)?.is_none_or(|known| known != name)
			{
				continue;
			}
			let parents: Vec<EntityId> = state
				.current(entity, &IN_FOLDER)?
				.iter()
				.filter_map(|statement| statement.value.as_reference())
				.collect();
			let same = match parent {
				Some(parent) => parents.contains(&parent),
				None => parents.is_empty(),
			};
			if !same {
				continue;
			}
			// One that has no description gets this one.
			if let Some(description) = description
				&& state.current(entity, &DESCRIPTION)?.is_empty()
			{
				let operations = vec![Operation::assert(Statement::new(
					StatementId::random(rng),
					entity,
					DESCRIPTION,
					Value::Text(description.to_owned()),
				))];
				self.commit(space, operations, wall_millis, rng)?;
			}
			return Ok((entity, false));
		}
		let mut values = vec![NewValue::new(fields::NAME, Value::Name(Name::new(name)))];
		if let Some(parent) = parent {
			values.push(NewValue::new(IN_FOLDER, Value::Reference(parent)));
		}
		if let Some(description) = description {
			values.push(NewValue::new(
				DESCRIPTION,
				Value::Text(description.to_owned()),
			));
		}
		let (folder, operations) = self.new_entity(space, &[FOLDER], values, rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok((folder, true))
	}

	/// A folder on the library's disk for files that an importer unpacks before it adds
	/// them, as from an archive, emptied of what an import cut short left: on the same disk,
	/// so that a large file does not fill a smaller one, such as a temporary file system in
	/// memory.
	///
	/// # Errors
	///
	/// Returns an error if the folder cannot be made.
	pub fn scratch_folder(&self) -> Result<std::path::PathBuf, NodeError> {
		let folder = self.directory.join("import");
		if folder.exists() {
			let _ = std::fs::remove_dir_all(&folder);
		}
		std::fs::create_dir_all(&folder).map_err(NodeError::file(&folder))?;
		Ok(folder)
	}

	/// The source of a space named `name`, made if the space has none of that name, whatever
	/// its case.
	///
	/// # Errors
	///
	/// Returns [`NodeError::EmptyName`] for an empty name, and an error if the space cannot be
	/// read or written.
	pub fn source_named(
		&mut self,
		space: SpaceId,
		name: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		let name = name.trim();
		if name.is_empty() {
			return Err(NodeError::EmptyName);
		}
		if let Some(known) = self
			.sources(space)?
			.into_iter()
			.find(|source| source.name.to_lowercase() == name.to_lowercase())
		{
			return Ok(known.id);
		}
		self.install_standard(space, &[SOURCE], wall_millis, rng)?;
		let (source, operations) = self.new_entity(
			space,
			&[SOURCE],
			vec![NewValue::new(fields::NAME, Value::Name(Name::new(name)))],
			rng,
		)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok(source)
	}
}

/// When a photo was taken, of what it says itself, `own`, and what where it was found says,
/// `given`: its own, if it is the same moment, to the second, as given, or the same moment in
/// a time zone of whole quarter hours when it says no offset, which it then gets; else given,
/// as when someone corrected the date where it was found.
#[must_use]
pub fn taken_of(own: Option<&Edtf>, given: &Edtf) -> Edtf {
	let (Some(own), Some((given_local, given_offset))) = (own, given.local_seconds()) else {
		return given.clone();
	};
	let Some((own_local, own_offset)) = own.local_seconds() else {
		return given.clone();
	};
	let given_utc = given_local - i64::from(given_offset.unwrap_or(0)) * 60;
	if let Some(offset) = own_offset {
		let own_utc = own_local - i64::from(offset) * 60;
		return if (own_utc - given_utc).abs() <= 1 {
			own.clone()
		} else {
			given.clone()
		};
	}
	// Local time ahead of UTC by the offset, within a second of a quarter hour.
	let ahead = own_local - given_utc;
	let quarters = (ahead + 450).div_euclid(900);
	let off_by = (ahead - quarters * 900).abs();
	if off_by <= 1 && (-12 * 4..=14 * 4).contains(&quarters) {
		let minutes = i32::try_from(quarters * 15).unwrap_or(0);
		own.with_offset(minutes).unwrap_or_else(|| own.clone())
	} else {
		given.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn at(text: &str) -> Edtf {
		text.parse().expect("a date")
	}

	#[test]
	fn the_photos_own_time_stays_when_it_is_the_same_moment_and_a_corrected_one_wins() {
		let given = at("2019-12-24T17:30:00Z");
		// The same moment, written in local time with its offset.
		assert_eq!(
			taken_of(Some(&at("2019-12-24T18:30:00+01")), &given),
			at("2019-12-24T18:30:00+01")
		);
		// The same moment, written without an offset: it gets the one the difference shows.
		assert_eq!(
			taken_of(Some(&at("2019-12-24T18:30:00")), &given),
			at("2019-12-24T18:30:00+01")
		);
		assert_eq!(
			taken_of(Some(&at("2019-12-24T23:00:00")), &given),
			at("2019-12-24T23:00:00+05:30")
		);
		// A day apart: someone corrected the date where it was found.
		assert_eq!(taken_of(Some(&at("2019-12-23T18:30:00+01")), &given), given);
		assert_eq!(taken_of(Some(&at("2018-01-01T00:00:00")), &given), given);
		// None of its own.
		assert_eq!(taken_of(None, &given), given);
	}
}
