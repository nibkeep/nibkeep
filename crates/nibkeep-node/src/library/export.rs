//! Exports: a space in ordinary files that stay useful without NibKeep, JSON and SHA-256
//! sums (`docs/spec/export.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Languages, State, TypeName, Types, Value};
use nibkeep_std::CONTENT;
use nibkeep_store::StoredLog;
use nibkeep_time::Timestamp;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};

use super::entities::display_name;
use super::{Author, Library, Space, json, policies};
use crate::NodeError;

/// The version of the format of exports.
const VERSION: u64 = 1;

/// The types of NibKeep's own records of who uses a library, recovery codes among them: an
/// export of current values leaves them out, a merge refuses them, and edits neither give
/// them nor take them away.
pub const OWN_RECORDS: [TypeName; 5] = [
	nibkeep_governance::USER,
	nibkeep_governance::DEVICE,
	nibkeep_governance::REPLICA,
	nibkeep_governance::SPACE,
	nibkeep_governance::RECOVERY_CODE,
];

/// What an export holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
	/// Every entity, with every statement, current and replaced, and when each was
	/// recorded, by whom, and when replaced.
	Full,
	/// The current statements, without when or by whom they were recorded, of the entities
	/// that are not deleted, without NibKeep's own records of users, devices, replicas and
	/// spaces: for giving a collection to someone.
	Current,
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
	/// How many entities it holds.
	pub entities: usize,
	/// How many files it holds.
	pub files: usize,
	/// The files that values refer to but the replica does not hold, ordered by their text.
	pub missing_files: Vec<Hash>,
}

impl Library {
	/// Exports a space into a directory, which must be empty or not exist yet: its
	/// entities as JSON, the files its values refer to as they were added, a manifest, a
	/// note for people, and the SHA-256 sums of them all, as `docs/spec/export.md` says.
	/// Secret values are left out. A space that has not changed since gives the same files.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the directory holds anything, or
	/// the space cannot be read, or the files written.
	pub fn export(
		&self,
		space: SpaceId,
		directory: &Path,
		kind: ExportKind,
	) -> Result<Exported, NodeError> {
		let open = self.space(space)?;
		if fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_some()) {
			return Err(NodeError::ExportExists(directory.display().to_string()));
		}
		// The export is written beside the directory and moved into place when it is whole, so
		// that one cut short leaves nothing that could pass for an export.
		let Some(name) = directory.file_name() else {
			return Err(NodeError::Export(format!(
				"{} is no folder of its own",
				directory.display()
			)));
		};
		let partial = directory.with_file_name(format!(
			".{}.partial-{}",
			name.to_string_lossy(),
			std::process::id()
		));
		let _ = fs::remove_dir_all(&partial);
		fs::create_dir_all(&partial).map_err(NodeError::file(&partial))?;
		let device = self.info.device;
		let written = open
			.stored
			.reading(|stored| Self::write_export(open, stored, (space, device), &partial, kind));
		let moved = written.and_then(|exported| {
			if directory.exists() {
				fs::remove_dir(directory).map_err(NodeError::file(directory))?;
			}
			fs::rename(&partial, directory).map_err(NodeError::file(directory))?;
			if let Some(parent) = directory.parent() {
				sync_directory(parent);
			}
			Ok(exported)
		});
		if moved.is_err() {
			let _ = fs::remove_dir_all(&partial);
		}
		moved
	}

	/// Writes an export of a space into a new, empty directory. The files that this device,
	/// `device`, holds only where they were found are read from there, and checked against
	/// their hashes.
	fn write_export(
		open: &Space,
		stored: &StoredLog,
		(space, device): (SpaceId, EntityId),
		directory: &Path,
		kind: ExportKind,
	) -> Result<Exported, NodeError> {
		let mut sums = Vec::new();
		let path = directory.join("entities.jsonl");
		let mut lines = Summed::create(&path).map_err(NodeError::file(&path))?;
		let written = entities_of(open, stored, stored.log().state(), kind, &mut |line| {
			lines.write_all(line.as_bytes())?;
			lines.write_all(b"\n")
		})?;
		sums.push((
			"entities.jsonl".to_owned(),
			lines.finish().map_err(NodeError::file(&path))?,
		));
		if kind == ExportKind::Full {
			let path = directory.join("changes.jsonl");
			let mut lines = Summed::create(&path).map_err(NodeError::file(&path))?;
			for change in &written.changes {
				lines
					.write_all(change.as_bytes())
					.and_then(|()| lines.write_all(b"\n"))
					.map_err(NodeError::file(&path))?;
			}
			sums.push((
				"changes.jsonl".to_owned(),
				lines.finish().map_err(NodeError::file(&path))?,
			));
		}
		let mut files = 0;
		let mut missing = Vec::new();
		for hash in &written.blobs {
			let relative = blob_path(hash);
			let path = directory.join(&relative);
			let copied = copy_blob(stored, hash, &path).and_then(|copied| match copied {
				Some(sum) => Ok(Some(sum)),
				None => copy_in_place(stored, device, hash, &path),
			});
			match copied {
				Ok(Some(sum)) => {
					sums.push((relative, sum));
					files += 1;
				}
				// A file that the replica does not hold, or cannot read, is listed as missing.
				Ok(None) => missing.push(*hash),
				Err(error) => return Err(NodeError::file(&path)(error)),
			}
		}
		for written in [
			directory.join("files").join("blake3"),
			directory.join("files"),
		] {
			sync_directory(&written);
		}
		let manifest = json!({
			"format": "nibkeep-export",
			"version": VERSION,
			"space": {
				"id": space.to_string(),
				"name": open.name(&Languages::default())?,
			},
			"as_of": written.as_of.map(json::moment),
			"history": kind == ExportKind::Full,
			"entities": written.entities,
			"files": files,
			"missing_files": missing.iter().map(ToString::to_string).collect::<Vec<_>>(),
			"secrets": "left out",
		});
		let mut text = serde_json::to_string_pretty(&manifest)
			.map_err(io::Error::from)
			.map_err(NodeError::file(&directory.join("manifest.json")))?;
		text.push('\n');
		for (relative, bytes) in [
			("manifest.json", text.into_bytes()),
			("README.txt", readme(kind).into_bytes()),
		] {
			let path = directory.join(relative);
			write_synced(&path, &bytes).map_err(NodeError::file(&path))?;
			sums.push((relative.to_owned(), hex(&Sha256::digest(&bytes))));
		}
		sums.sort();
		let mut listed = String::new();
		for (path, sum) in &sums {
			listed.push_str(sum);
			listed.push_str("  ");
			listed.push_str(path);
			listed.push('\n');
		}
		let path = directory.join("SHA256SUMS");
		write_synced(&path, listed.as_bytes()).map_err(NodeError::file(&path))?;
		sync_directory(directory);
		Ok(Exported {
			entities: written.entities,
			files,
			missing_files: missing,
		})
	}
}

/// Writes the lines of an export's entities from a state of a space, one call of `line`
/// each, in order, with the changes they name and who made them, as the state says. Names
/// are in no reader's languages, so that every device writes the same.
fn entities_of<S: Statements>(
	open: &Space,
	stored: &StoredLog,
	state: &State<S>,
	kind: ExportKind,
	line: &mut dyn FnMut(&str) -> io::Result<()>,
) -> Result<Written, NodeError>
where
	NodeError: From<S::Error>,
{
	let languages = Languages::default();
	let mut authors = Authors::default();
	let mut author = |change: Hash| -> Result<Option<Author>, NodeError> {
		authors.of(open, stored, state, change, &languages)
	};
	write_entities(state, &open.types, &languages, kind, &mut author, line)
}

/// What the lines of an export's entities hold, besides themselves.
pub(crate) struct Written {
	/// How many lines were written.
	pub(crate) entities: usize,
	/// The blobs that their values refer to.
	pub(crate) blobs: BTreeSet<Hash>,
	/// When the last change of the space was recorded, secret values aside, if it has one.
	pub(crate) as_of: Option<Timestamp>,
	/// The lines of the changes that they name, in the order they first name them; none in
	/// an export of current values.
	pub(crate) changes: Vec<String>,
}

/// A change as the values of an export name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ChangeKey {
	/// A change of the log, by its hash.
	Change(Hash),
	/// What recorded statements that came in no change, by when they were recorded.
	Unsigned(Timestamp),
}

/// The changes that the lines of an export name, numbered in the order they first name
/// them, each with the line that tells when it was recorded and who made it.
#[derive(Default)]
struct Changes {
	numbers: BTreeMap<ChangeKey, usize>,
	lines: Vec<String>,
}

impl Changes {
	/// The number of a change recorded at `at`, whose author `author` names when the change
	/// is first seen.
	fn number(
		&mut self,
		key: ChangeKey,
		at: Timestamp,
		author: &mut dyn FnMut(Hash) -> Result<Option<Author>, NodeError>,
	) -> Result<usize, NodeError> {
		if let Some(&number) = self.numbers.get(&key) {
			return Ok(number);
		}
		let mut line = Map::new();
		line.insert("recorded".to_owned(), json::moment(at));
		let author = match key {
			ChangeKey::Change(hash) => author(hash)?,
			ChangeKey::Unsigned(_) => None,
		};
		if let Some(author) = author {
			line.insert("replica".to_owned(), json!(author.replica.to_string()));
			line.insert("user".to_owned(), json!(author.user.id.to_string()));
			if let Some(person) = author.person {
				line.insert("person".to_owned(), json!(person.to_string()));
			}
			if author.ai_agent.is_some() {
				line.insert("ai_agent".to_owned(), json!(true));
			}
		}
		let number = self.lines.len();
		self.lines.push(Json::Object(line).to_string());
		self.numbers.insert(key, number);
		Ok(number)
	}
}

/// A value of an entity's line, before the line is written and its change numbered.
struct Pending {
	/// When it was first recorded, and its statement's ID, which order the values.
	at: Timestamp,
	id: StatementId,
	/// The change that recorded it, or the purge that stated it again, and when.
	change: ChangeKey,
	change_at: Timestamp,
	/// Its JSON, without the change's number.
	value: Map<String, Json>,
}

/// Writes the lines of an export's entities from a state, one call of `line` each, ordered
/// by entity ID; with, in a full export, the changes they name, whose authors `author`
/// names by their hashes.
pub(crate) fn write_entities<S: Statements>(
	state: &State<S>,
	types: &Types,
	languages: &Languages,
	kind: ExportKind,
	author: &mut dyn FnMut(Hash) -> Result<Option<Author>, NodeError>,
	line: &mut dyn FnMut(&str) -> io::Result<()>,
) -> Result<Written, NodeError>
where
	NodeError: From<S::Error>,
{
	let storage = state.storage();
	let secret = types.secret_fields();
	let mut written = Written {
		entities: 0,
		blobs: BTreeSet::new(),
		as_of: None,
		changes: Vec::new(),
	};
	let mut changes = Changes::default();
	for entity in storage.described()? {
		// Thumbnails are made again from the originals (technical design: thumbnails).
		if state.types(entity)?.contains(&nibkeep_std::THUMBNAIL) {
			continue;
		}
		let mut fields: BTreeMap<FieldName, Vec<Pending>> = BTreeMap::new();
		// The files the entity's line refers to, which count only if the line is written.
		let mut blobs = BTreeSet::new();
		for (field, ids) in storage.of_entity(entity)? {
			let mut values = Vec::new();
			for id in ids {
				let Some(recorded) = storage.get(id)? else {
					continue;
				};
				let replaced = storage.superseded_at(id)?;
				if secret.contains(&field) {
					continue;
				}
				// The moment of the export is that of the space, what it leaves out included,
				// secret values aside.
				for at in [Some(recorded.at), recorded.restated, replaced]
					.into_iter()
					.flatten()
				{
					written.as_of = written.as_of.max(Some(at));
				}
				if kind == ExportKind::Current && replaced.is_some() {
					continue;
				}
				let statement = recorded.statement;
				for value in std::iter::once(&statement.value)
					.chain(statement.qualifiers.iter().map(|(_, qualifier)| qualifier))
				{
					if let Value::Blob(hash) = value {
						blobs.insert(*hash);
					}
				}

				let mut value = Map::new();
				value.insert("statement".to_owned(), json!(statement.id.to_string()));
				json::value(&statement.value, &mut value);
				if !statement.qualifiers.is_empty() {
					value.insert(
						"qualifiers".to_owned(),
						json::qualifiers(&statement.qualifiers),
					);
				}
				if kind == ExportKind::Full {
					// A value that a purge stated again came in the purge, later than it was
					// first recorded.
					if recorded.restated.is_some() {
						value.insert("recorded".to_owned(), json::moment(recorded.at));
					}
					if let Some(replaced) = replaced {
						value.insert("replaced".to_owned(), json::moment(replaced));
					}
				}
				let change_at = recorded.restated.unwrap_or(recorded.at);
				values.push(Pending {
					at: recorded.at,
					id: statement.id,
					change: recorded
						.change
						.map_or(ChangeKey::Unsigned(change_at), ChangeKey::Change),
					change_at,
					value,
				});
			}
			values.sort_by_key(|pending| (pending.at, pending.id));
			if !values.is_empty() {
				fields.insert(field, values);
			}
		}
		if fields.is_empty() || (kind == ExportKind::Current && !is_given(state, entity)?) {
			continue;
		}
		written.blobs.append(&mut blobs);
		let entity_types: Vec<String> = state
			.types(entity)?
			.iter()
			.map(ToString::to_string)
			.collect();
		// Changes are numbered as the line names them, once it is written.
		let numbered = kind == ExportKind::Full;
		let object = json!({
			"id": entity.to_string(),
			"types": entity_types,
			"name": display_name(state, types, languages, state.canonical(entity)?)?,
			"fields": fields_of(fields, numbered.then_some(&mut changes), author)?,
		});
		line(&object.to_string()).map_err(|error| NodeError::Export(error.to_string()))?;
		written.entities += 1;
	}
	written.changes = changes.lines;
	Ok(written)
}

/// The fields of an entity's line, as JSON; with, if `changes` are given, the number of
/// each value's change, which they number if it is new, its author named by `author`.
fn fields_of(
	fields: BTreeMap<FieldName, Vec<Pending>>,
	mut changes: Option<&mut Changes>,
	author: &mut dyn FnMut(Hash) -> Result<Option<Author>, NodeError>,
) -> Result<Vec<Json>, NodeError> {
	let mut written = Vec::with_capacity(fields.len());
	for (field, values) in fields {
		let mut objects = Vec::with_capacity(values.len());
		for mut pending in values {
			if let Some(changes) = &mut changes {
				let number = changes.number(pending.change, pending.change_at, author)?;
				pending.value.insert("change".to_owned(), json!(number));
			}
			objects.push(Json::Object(pending.value));
		}
		written.push(json!({ "name": field.to_string(), "values": objects }));
	}
	Ok(written)
}

/// Whether an export of current values holds an entity: one that is not deleted, and not
/// one of NibKeep's own records.
fn is_given<S: Statements>(state: &State<S>, entity: EntityId) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	if state.is_deleted(entity)? {
		return Ok(false);
	}
	let held = state.types(entity)?;
	Ok(!OWN_RECORDS.iter().any(|own| held.contains(own)))
}

/// The authors of changes, each found once per signing key.
#[derive(Default)]
struct Authors {
	by_key: BTreeMap<String, Option<Author>>,
}

impl Authors {
	fn of<S: Statements>(
		&mut self,
		open: &Space,
		stored: &StoredLog,
		state: &State<S>,
		change: Hash,
		languages: &Languages,
	) -> Result<Option<Author>, NodeError>
	where
		NodeError: From<S::Error>,
	{
		let Some(key) = stored
			.log()
			.change(nibkeep_log::ChangeId::from_hash(change))?
			.map(|change| *change.author())
		else {
			return Ok(None);
		};
		let text = key.to_text();
		if let Some(author) = self.by_key.get(&text) {
			return Ok(author.clone());
		}
		let author = Library::author_in(open, stored, state, &key, languages)?;
		self.by_key.insert(text, author.clone());
		Ok(author)
	}
}

/// Copies a file that the replica holds into an export, and returns its SHA-256 sum, or
/// `None`, leaving nothing behind, if the replica does not hold it or cannot read it.
fn copy_blob(stored: &StoredLog, hash: &Hash, path: &Path) -> io::Result<Option<String>> {
	if !stored.holds_ordinary_blob(hash) {
		return Ok(None);
	}
	let Ok(mut reader) = stored.open_blob(hash) else {
		return Ok(None);
	};
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let mut file = Summed::create(path)?;
	if io::copy(&mut reader, &mut file).is_err() {
		drop(file);
		fs::remove_file(path)?;
		return Ok(None);
	}
	file.finish().map(Some)
}

/// Copies into an export a file that this device holds only where it was found: from the
/// first place on `device` that holds its content, as its hash tells while it is copied.
/// Returns its SHA-256 sum, or `None`, leaving nothing behind, if no place does.
fn copy_in_place(
	stored: &StoredLog,
	device: EntityId,
	hash: &Hash,
	path: &Path,
) -> io::Result<Option<String>> {
	let state = stored.log().state();
	let Ok(files) = stored.find(&CONTENT, &Value::Blob(*hash)) else {
		return Ok(None);
	};
	let mut places = Vec::new();
	for file in files {
		let Ok(origins) = policies::origins_of(state, file) else {
			continue;
		};
		places.extend(
			origins
				.into_iter()
				.filter(|(_, on, _)| *on == device)
				.map(|(_, _, place)| place),
		);
	}
	for place in places {
		let Ok(mut original) = File::open(&place) else {
			continue;
		};
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut file = Summed::create(path)?;
		let mut hasher = nibkeep_base::Hasher::new();
		let copied = io::copy(&mut original, &mut Both(&mut file, &mut hasher));
		if copied.is_ok() && hasher.finalize() == *hash {
			return file.finish().map(Some);
		}
		drop(file);
		fs::remove_file(path)?;
	}
	Ok(None)
}

/// Writes what it is given to two writers.
struct Both<'a, A, B>(&'a mut A, &'a mut B);

impl<A: Write, B: Write> Write for Both<'_, A, B> {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		let written = self.0.write(bytes)?;
		self.1.write_all(&bytes[..written])?;
		Ok(written)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.0.flush()?;
		self.1.flush()
	}
}

/// Writes a file and saves it to the disk.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let mut file = File::create_new(path)?;
	file.write_all(bytes)?;
	file.sync_all()
}

/// Saves a directory's entries to the disk, where the system allows it; a failure leaves
/// the export as the system keeps it.
fn sync_directory(directory: &Path) {
	#[cfg(unix)]
	if let Ok(opened) = File::open(directory) {
		let _ = opened.sync_all();
	}
	#[cfg(not(unix))]
	let _ = directory;
}

/// Where an export keeps a file: `files/blake3/` and the hexadecimal digest.
fn blob_path(hash: &Hash) -> String {
	format!("files/{}/{}", hash.algorithm().name(), hex(hash.digest()))
}

/// Bytes in lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
	const DIGITS: &[u8; 16] = b"0123456789abcdef";
	let mut text = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		text.push(char::from(DIGITS[usize::from(byte >> 4)]));
		text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
	}
	text
}

/// A file that is summed with SHA-256 as it is written.
struct Summed {
	file: BufWriter<File>,
	sum: Sha256,
}

impl Summed {
	fn create(path: &Path) -> io::Result<Self> {
		Ok(Self {
			file: BufWriter::new(File::create_new(path)?),
			sum: Sha256::new(),
		})
	}

	/// Saves the file, and returns its sum in hexadecimal.
	fn finish(self) -> io::Result<String> {
		let file = self
			.file
			.into_inner()
			.map_err(io::IntoInnerError::into_error)?;
		file.sync_all()?;
		Ok(hex(&self.sum.finalize()))
	}
}

impl Write for Summed {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		let written = self.file.write(bytes)?;
		self.sum.update(&bytes[..written]);
		Ok(written)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.file.flush()
	}
}

/// The note for people at the top of an export.
fn readme(kind: ExportKind) -> String {
	let holds = match kind {
		ExportKind::Full => {
			"every value its entities have had, with when each was recorded, by whom, and when it was replaced"
		}
		ExportKind::Current => {
			"the values its entities have now, without who recorded them or when"
		}
	};
	let changes = match kind {
		ExportKind::Full => {
			"\n  changes.jsonl    who made each change that its values name, and when"
		}
		ExportKind::Current => "",
	};
	format!(
		"This folder is an export of a space of NibKeep, in the format nibkeep-export, version {VERSION}.
It holds {holds}, as JSON that any program reads, and the files that its values refer to, as they were added.

  manifest.json    what the export holds
  entities.jsonl   one entity per line, as JSON{changes}
  files/           the files, named by their BLAKE3 hashes
  SHA256SUMS       the SHA-256 sum of every other file

To check every file, run:

  sha256sum -c SHA256SUMS

The format is described in docs/spec/export.md of NibKeep's source. This folder is not encrypted: whoever can read it can read everything in it. Secret values are not in it.
"
	)
}

/// What is wrong with an export, compared with the library it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportProblem {
	/// The folder holds no export that this version reads, for the reason given.
	NotAnExport(String),
	/// The export is of a space that the library does not have.
	OtherSpace(SpaceId),
	/// A file that `SHA256SUMS` lists is missing, or its sum differs.
	WrongSum(String),
	/// A file of the export that `SHA256SUMS` does not list.
	Unlisted(String),
	/// An entity that the space held at the export's moment, which the export lacks.
	MissingEntity(EntityId),
	/// An entity of the export that the space did not hold at its moment.
	ExtraEntity(EntityId),
	/// An entity whose line differs from what the space held at the export's moment.
	DifferentEntity(EntityId),
	/// The list of changes, `changes.jsonl`, which differs from the changes that the lines
	/// should name, or which is missing from a full export or found in one of current values.
	DifferentChanges,
	/// A file that the space holds and the export should, but does not.
	MissingFile(Hash),
	/// A file of the export whose content does not have the hash it is named by.
	DifferentFile(Hash),
	/// A line of `entities.jsonl`, by its number from 1, that is no entity.
	DamagedLine(usize),
	/// A file in `files/` that no value of the space referred to at the export's moment.
	ExtraFile(String),
	/// A count of the manifest, `entities` or `files`, that is not what the export holds.
	WrongManifest(String),
}

/// An export, compared with the library it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportCheck {
	/// The space it is of, if its manifest says.
	pub space: Option<SpaceId>,
	/// The moment it holds the space as of, as its manifest gives it.
	pub as_of: Option<String>,
	/// How many entities it holds.
	pub entities: usize,
	/// How many files it holds.
	pub files: usize,
	/// What is wrong with it: its sums first, by path, then its lines that are no entities,
	/// its entities, by ID, its list of changes, its files, and its manifest's counts. None
	/// for an export that holds what the space held at its moment.
	pub problems: Vec<ExportProblem>,
}

impl Library {
	/// Checks an export against the library it came from: every file against `SHA256SUMS`,
	/// and its entities and files against those that an export of the space as of the
	/// export's moment holds, so that changes made since do not count. The types the space
	/// has now decide what is secret and how entities are named, and values purged since are
	/// gone from the library, so either makes an export differ.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read, or a file of the export.
	pub fn verify_export(&self, directory: &Path) -> Result<ExportCheck, NodeError> {
		let mut check = ExportCheck {
			space: None,
			as_of: None,
			entities: 0,
			files: 0,
			problems: Vec::new(),
		};
		let manifest = match read_manifest(directory) {
			Ok(manifest) => manifest,
			Err(why) => {
				check.problems.push(ExportProblem::NotAnExport(why));
				return Ok(check);
			}
		};
		check.space = Some(manifest.space);
		check.as_of.clone_from(&manifest.as_of_text);
		let Some(open) = self.spaces.get(&manifest.space) else {
			check
				.problems
				.push(ExportProblem::OtherSpace(manifest.space));
			return Ok(check);
		};
		check.problems.extend(check_sums(directory)?);
		open.stored.reading(|stored| {
			let then = stored.log().state_as_of(manifest.as_of);
			let mut expected = Vec::new();
			let written = entities_of(open, stored, &then, manifest.kind, &mut |line| {
				expected.push(line.to_owned());
				Ok(())
			})?;
			// A file gone or damaged is told of by the sums; its lines are compared as they are.
			let held = fs::read(directory.join("entities.jsonl")).unwrap_or_default();
			let held = String::from_utf8_lossy(&held);
			let held: Vec<&str> = held.lines().collect();
			let listed = fs::read(directory.join("changes.jsonl")).ok();
			let listed = listed.as_deref().map(String::from_utf8_lossy);
			let listed: Option<Vec<&str>> = listed.as_deref().map(|text| text.lines().collect());
			let expected_changes: Vec<&str> = written.changes.iter().map(String::as_str).collect();
			let (problems, entities) = compare_lines(
				(&expected, &expected_changes),
				(&held, listed.as_deref().unwrap_or_default()),
			);
			check.entities = entities;
			check.problems.extend(problems);
			let changes_differ = match (manifest.kind, &listed) {
				(ExportKind::Full, Some(listed)) => *listed != expected_changes,
				(ExportKind::Full, None) => true,
				(ExportKind::Current, listed) => listed.is_some(),
			};
			if changes_differ {
				check.problems.push(ExportProblem::DifferentChanges);
			}
			let mut expected_files = BTreeSet::new();
			for hash in &written.blobs {
				let relative = blob_path(hash);
				let path = directory.join(&relative);
				expected_files.insert(relative);
				match blake3_of(&path) {
					Ok(found) => {
						check.files += 1;
						if found != *hash {
							check.problems.push(ExportProblem::DifferentFile(*hash));
						}
					}
					// A file the export says it could not hold, the library may hold since.
					Err(_) if manifest.missing_files.contains(hash) => {}
					Err(_) if stored.holds_ordinary_blob(hash) => {
						check.problems.push(ExportProblem::MissingFile(*hash));
					}
					Err(_) => {}
				}
			}
			for relative in files_of(&directory.join("files"))? {
				let relative = format!("files/{relative}");
				if !expected_files.contains(&relative) {
					check.problems.push(ExportProblem::ExtraFile(relative));
				}
			}
			if manifest.entities != Some(check.entities) {
				check
					.problems
					.push(ExportProblem::WrongManifest("entities".to_owned()));
			}
			if manifest.files != Some(check.files) {
				check
					.problems
					.push(ExportProblem::WrongManifest("files".to_owned()));
			}
			Ok(check)
		})
	}
}

/// What an export's manifest says, as far as a check needs it.
pub(crate) struct Manifest {
	pub(crate) space: SpaceId,
	/// The moment, in milliseconds since 1970; 0 for an export of a space without changes.
	pub(crate) as_of: u64,
	pub(crate) as_of_text: Option<String>,
	pub(crate) kind: ExportKind,
	/// How many entities and files it says it holds.
	pub(crate) entities: Option<usize>,
	pub(crate) files: Option<usize>,
	/// The files it says the replica did not hold.
	pub(crate) missing_files: BTreeSet<Hash>,
}

/// Reads an export's manifest, or says why it is none that this version reads.
pub(crate) fn read_manifest(directory: &Path) -> Result<Manifest, String> {
	let text = fs::read_to_string(directory.join("manifest.json"))
		.map_err(|_| "it has no manifest.json".to_owned())?;
	let manifest: Json =
		serde_json::from_str(&text).map_err(|_| "its manifest is no JSON".to_owned())?;
	if manifest["format"] != "nibkeep-export" {
		return Err("its manifest names no nibkeep-export".to_owned());
	}
	if manifest["version"] != VERSION {
		return Err(format!(
			"it is of version {}, which this version does not read",
			manifest["version"]
		));
	}
	let space = manifest["space"]["id"]
		.as_str()
		.and_then(|id| id.parse().ok())
		.ok_or_else(|| "its manifest names no space".to_owned())?;
	let kind = match manifest["history"].as_bool() {
		Some(true) => ExportKind::Full,
		Some(false) => ExportKind::Current,
		None => return Err("its manifest does not say whether it holds history".to_owned()),
	};
	let as_of_text = manifest["as_of"].as_str().map(str::to_owned);
	let as_of = match &as_of_text {
		None => 0,
		Some(text) => text
			.parse::<jiff::Timestamp>()
			.ok()
			.and_then(|moment| u64::try_from(moment.as_millisecond()).ok())
			.ok_or_else(|| format!("its moment {text} cannot be read"))?,
	};
	let count = |key: &str| {
		manifest[key]
			.as_u64()
			.and_then(|count| usize::try_from(count).ok())
	};
	let missing_files = manifest["missing_files"]
		.as_array()
		.map_or_default(|hashes| {
			hashes
				.iter()
				.filter_map(|hash| hash.as_str()?.parse().ok())
				.collect()
		});
	Ok(Manifest {
		space,
		as_of,
		as_of_text,
		kind,
		entities: count("entities"),
		files: count("files"),
		missing_files,
	})
}

/// Checks every file of an export against `SHA256SUMS`, and finds the files it does not
/// list, ordered by path.
pub(crate) fn check_sums(directory: &Path) -> Result<Vec<ExportProblem>, NodeError> {
	let path = directory.join("SHA256SUMS");
	let Ok(sums) = fs::read_to_string(&path) else {
		return Ok(vec![ExportProblem::WrongSum("SHA256SUMS".to_owned())]);
	};
	let mut problems = Vec::new();
	let mut listed = BTreeSet::new();
	for line in sums.lines() {
		let Some((sum, relative)) = line.split_once("  ") else {
			problems.push(ExportProblem::WrongSum("SHA256SUMS".to_owned()));
			continue;
		};
		listed.insert(relative.to_owned());
		// A path that leads out of the export, or through a link, names no file of it.
		let inside = !relative.contains('\\')
			&& Path::new(relative)
				.components()
				.all(|part| matches!(part, std::path::Component::Normal(_)));
		let path = directory.join(relative);
		let plain = inside && fs::symlink_metadata(&path).is_ok_and(|found| found.is_file());
		let matches = plain && sum_of(&path).is_ok_and(|found| found == sum);
		if !matches {
			problems.push(ExportProblem::WrongSum(relative.to_owned()));
		}
	}
	for relative in files_of(directory)? {
		if relative != "SHA256SUMS" && !listed.contains(&relative) {
			problems.push(ExportProblem::Unlisted(relative));
		}
	}
	problems.sort_by(|one, other| problem_path(one).cmp(problem_path(other)));
	Ok(problems)
}

/// The SHA-256 sum of a file, in hexadecimal.
fn sum_of(path: &Path) -> io::Result<String> {
	let mut file = File::open(path)?;
	let mut summing = Sha256::new();
	let mut buffer = vec![0; 64 * 1024];
	loop {
		let read = io::Read::read(&mut file, &mut buffer)?;
		if read == 0 {
			return Ok(hex(&summing.finalize()));
		}
		summing.update(&buffer[..read]);
	}
}

/// The path that a problem with the sums names.
fn problem_path(problem: &ExportProblem) -> &str {
	match problem {
		ExportProblem::WrongSum(path) | ExportProblem::Unlisted(path) => path,
		_ => "",
	}
}

/// Every file under a directory, by its path relative to it, with `/`, ordered.
pub(crate) fn files_of(directory: &Path) -> Result<Vec<String>, NodeError> {
	let mut found = Vec::new();
	let mut to_visit = vec![directory.to_path_buf()];
	while let Some(next) = to_visit.pop() {
		let Ok(entries) = fs::read_dir(&next) else {
			continue;
		};
		for entry in entries {
			let entry = entry.map_err(NodeError::file(&next))?;
			let path = entry.path();
			// Links are not followed, so that one cannot lead the walk out or round.
			if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
				to_visit.push(path);
			} else if let Ok(relative) = path.strip_prefix(directory) {
				let parts: Vec<String> = relative
					.components()
					.map(|part| part.as_os_str().to_string_lossy().into_owned())
					.collect();
				found.push(parts.join("/"));
			}
		}
	}
	found.sort();
	Ok(found)
}

/// Compares the lines of entities that an export should hold with those it holds, each
/// with the list of changes they name, and says which entities are missing, extra or
/// different, and which lines are no entity's; with how many lines are entities. Lines
/// that differ only in the numbers of the changes they name, which name the same changes,
/// are alike: a change that one list lacks shifts the numbers after it.
fn compare_lines(
	(expected, expected_changes): (&[String], &[&str]),
	(held, held_changes): (&[&str], &[&str]),
) -> (Vec<ExportProblem>, usize) {
	let id_of = |line: &str| -> Option<EntityId> {
		serde_json::from_str::<Json>(line)
			.ok()?
			.get("id")?
			.as_str()?
			.parse()
			.ok()
	};
	let mut expected: BTreeMap<EntityId, &str> = expected
		.iter()
		.filter_map(|line| Some((id_of(line)?, line.as_str())))
		.collect();
	let mut problems = BTreeMap::new();
	let mut damaged = Vec::new();
	let mut entities = 0;
	for (number, line) in held.iter().enumerate() {
		let Some(id) = id_of(line) else {
			damaged.push(ExportProblem::DamagedLine(number + 1));
			continue;
		};
		entities += 1;
		match expected.remove(&id) {
			Some(want)
				if want == *line
					|| with_changes(want, expected_changes) == with_changes(line, held_changes) => {}
			Some(_) => {
				problems.insert(id, ExportProblem::DifferentEntity(id));
			}
			None => {
				problems.insert(id, ExportProblem::ExtraEntity(id));
			}
		}
	}
	for id in expected.into_keys() {
		problems.insert(id, ExportProblem::MissingEntity(id));
	}
	damaged.extend(problems.into_values());
	(damaged, entities)
}

/// An entity's line with each change it names written out, as its list of changes gives
/// it, in place of its number; `None` for a line that is no JSON.
fn with_changes(line: &str, changes: &[&str]) -> Option<Json> {
	let mut line: Json = serde_json::from_str(line).ok()?;
	let values = line
		.get_mut("fields")
		.and_then(Json::as_array_mut)
		.into_iter()
		.flatten()
		.filter_map(|field| field.get_mut("values").and_then(Json::as_array_mut))
		.flatten();
	for value in values {
		if let Some(number) = value.get("change").and_then(Json::as_u64) {
			let change = usize::try_from(number)
				.ok()
				.and_then(|number| changes.get(number))
				.map_or(Json::Null, |change| Json::from(*change));
			value["change"] = change;
		}
	}
	Some(line)
}

/// The BLAKE3 hash of a file, read as a stream.
pub(crate) fn blake3_of(path: &Path) -> io::Result<Hash> {
	let read = nibkeep_store::hash_file(path, &|| false)?;
	read.map(|(hash, _)| hash)
		.ok_or_else(|| io::Error::other("stopped"))
}
