//! Files: content that a person adds, stored once however often it is added, with each
//! place it was found, and what a photo or a video says of itself (`docs/Design.md`,
//! section 6.4).

use std::fs::{self, File};
use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::{FieldName, Name, Operation, TypeName, Value, fields};
use nibkeep_std::{
	CHECKOUT, CHECKOUT_OF, CONTENT, CREATED, FILE, FOLDER, FOUND_ON, FROM_SOURCE, IN_FOLDER,
	IN_TREE, MEDIA_TYPE, MODIFIED, ORIGIN, ORIGIN_OF, PATH, PHOTO, SIZE, SOURCE_NAME, SOURCE_PATH,
	TAKEN, VIDEO,
};
use nibkeep_time::{Day, Edtf};

use super::entities::NewValue;
use super::policies::{self, ChosenBy, Facts, Kept, NewRule, Place};
use super::sources::{FileFrom, FoundElsewhere};
use super::{Library, media};
use crate::NodeError;

/// How many of a file's first bytes tell its media type.
pub(crate) const HEAD: usize = 8_192;

/// What adding a file did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedFile {
	/// The file: the entity of its content.
	pub file: EntityId,
	/// Whether the library held its content already, as this file.
	pub known: bool,
	/// The origin recorded for where it was found, or `None` if the library knew it found
	/// there already, unchanged since, or it was in a temporary folder, whose paths are
	/// not recorded.
	pub origin: Option<EntityId>,
	/// The hash of its content.
	pub content: Hash,
	/// Its types: a file, and a photo or a video if it says it is one.
	pub types: Vec<TypeName>,
	/// How this device holds it: in the store, or only where it was found.
	pub kept: Kept,
	/// Whether the rule that chose its policy asks whether a file as large can be got again,
	/// and nobody answered for its folder, so that it went into the store.
	pub unasked: bool,
}

/// A file large enough for the rule that would choose its policy to ask whether it can be
/// got again, if it is lost: the answer counts for its folder, as [`Library::answer`] takes
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
	/// The folder the file is in, as the file system resolves it.
	pub folder: PathBuf,
	/// The file's size in bytes.
	pub size: u64,
	/// The rule that asks, before which the answer goes.
	pub rule: EntityId,
}

/// Where a file is, as the file system says: what tells whether it changed since it was
/// added there.
struct At {
	metadata: fs::Metadata,
	resolved: PathBuf,
	found: Found,
}

/// What a file says of itself before it is read in full: its media type from its first
/// bytes, what a photo or a video says in its headers, and so what rules look at.
struct Looked {
	media_type: Option<String>,
	recorded: media::Recorded,
	types: Vec<TypeName>,
	facts: Facts,
}

impl Library {
	/// Adds the file at `path` to a space, found on this device. Its content is stored once:
	/// a file whose content the space holds already, as a file, is that file, found again,
	/// and a new origin records where, unless the space knows the file was found there,
	/// unchanged since. A file that this device found at the same path, with the same size
	/// and modification time, is taken to be unchanged and is not read again, as `rsync`
	/// takes it. A new file is named as it was found, with its content's media type and
	/// size, and is a photo or a video too if it says it is one, with when and where it was
	/// taken, by which camera, and how large it is. The types it needs are added to the
	/// space if it lacks them. Paths are kept as the file system resolves them, links and
	/// `..` followed, so that one place has one path. A file in the system's temporary
	/// folder, while the library is not there, goes into the store, and no origin records
	/// where it was, as the folder is soon emptied.
	///
	/// Its storage policy, chosen before the file is read in full, says whether the device
	/// keeps it in the store, or where it is, of which the space records its hash alone
	/// ([technical design: storage policies]). The space gets the default policies if it never had them. A
	/// file that the rule choosing for it would ask about, as [`Library::question_for`]
	/// tells, goes into the store until [`Library::answer`] answers for its folder.
	///
	/// [technical design: storage policies]: ../../../../docs/Technical-design.md#93-storage-policies
	///
	/// # Errors
	///
	/// Returns [`NodeError::Unreadable`] if `path` is not a file that can be read,
	/// [`NodeError::CheckedOut`] if it is the copy of a checkout on this device, and another
	/// error if the library has no such space, the types cannot be added, or the content or
	/// the changes cannot be saved.
	pub fn add_file(
		&mut self,
		space: SpaceId,
		path: &Path,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedFile, NodeError> {
		self.add_file_from(space, path, &FileFrom::default(), wall_millis, rng)
	}

	/// Adds the file at `path` to a space, as [`Library::add_file`] does, from where `from`
	/// says ([technical design: importers]): its origin names the source it was imported
	/// from, with its path and name there; a file read from a copy that is no original, as
	/// one unpacked from an archive, is found where `from` says, and goes into the store; the
	/// values `from` gives replace what the file says of itself, but on a file the library
	/// holds already, a value that someone changed since; and the file goes into the folders
	/// it names. The same file found again from another source gets an origin of its own.
	///
	/// [technical design: importers]: ../../../../docs/Technical-design.md#912-importers-and-where-files-came-from
	///
	/// # Errors
	///
	/// As [`Library::add_file`].
	pub fn add_file_from(
		&mut self,
		space: SpaceId,
		path: &Path,
		from: &FileFrom,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedFile, NodeError> {
		self.install_policies(space, None, wall_millis, rng)?;
		let (metadata, resolved, found) = self.found_where(space, path, from)?;
		// A file in a temporary folder is soon gone: it goes into the store, and where it
		// was is not recorded.
		let passing = from.found.is_none() && self.in_temporary_folder(&resolved);
		// A file found here before, unchanged, is not read again, unless its policy keeps it
		// in the store now, and the device holds it only here.
		let mut known = None;
		if !passing && let Some(added) = self.unchanged(space, &found, metadata.len())? {
			if added.kept == Kept::Store
				|| self.policy_of(space, added.file)?.needs.kept == Kept::InPlace
			{
				self.name_source(space, &found, from, wall_millis, rng)?;
				return Ok(added);
			}
			known = Some(added.file);
		}
		let Looked {
			media_type,
			recorded,
			mut types,
			facts,
		} = self.look_inside(path, &resolved, &found, &metadata)?;
		let (given, said) = given_and_said(&recorded, from);
		let kind = kind_of(&recorded, &given, media_type.as_deref());
		types.extend(kind.clone());
		if !from.folders.is_empty() {
			types.push(FOLDER);
		}
		// A space founded before these types, or one that dropped them, gets them now.
		let mut needed: Vec<TypeName> = types.clone();
		needed.push(ORIGIN);
		self.install_standard(space, &needed, wall_millis, rng)?;

		// A copy that is no original goes into the store: nothing keeps it where it was found.
		let (kept, unasked) = if from.found.is_some() || passing {
			(Kept::Store, false)
		} else {
			self.kept_for(space, known, &facts, metadata.len())?
		};
		let (content, size) =
			self.read_content(space, (path, &resolved), kept, wall_millis, rng)?;
		let existing = self.file_with(space, &content)?;
		let mut stored = self.space(space)?.stored.holds_ordinary_blob(&content);
		// The file it turns out to be may have a policy of its own, or be found elsewhere,
		// so that its policy keeps it in the store after all.
		if let Some(file) = existing
			&& !stored
			&& self.kept_by_policy(space, file, &found.path)? == Kept::Store
		{
			self.read_content(space, (path, &resolved), Kept::Store, wall_millis, rng)?;
			stored = true;
		}

		let mut operations = Vec::new();
		let file = if let Some(file) = existing {
			file
		} else {
			let values = file_values(
				&found,
				(content, size, media_type.as_deref()),
				(&said, &from.folders),
			);
			let file_types: Vec<TypeName> = types
				.iter()
				.filter(|kind| **kind != FOLDER)
				.cloned()
				.collect();
			let (file, made) = self.new_entity(space, &file_types, new_values(values), rng)?;
			operations.extend(made);
			file
		};
		if existing.is_some() {
			operations.extend(self.typed(space, file, kind, rng)?);
			operations.extend(self.found_again(space, file, (&recorded.values, &given), rng)?);
			operations.extend(self.putting_in_folders(space, file, &from.folders, rng)?);
		}
		let known = existing.is_some()
			&& self
				.known_origin(space, &found)?
				.is_some_and(|(_, of)| of == file);
		// A file found there before, or in a temporary folder, gets no new origin.
		let origin = if known || passing {
			self.name_source(space, &found, from, wall_millis, rng)?;
			None
		} else {
			// What was found there stays what the origin says, whatever versions the file
			// gets later.
			let values = self.origin_values(file, (&found, from), (content, size));
			let (origin, made) = self.new_entity(space, &[ORIGIN], new_values(values), rng)?;
			operations.extend(made);
			Some(origin)
		};
		if !operations.is_empty() {
			self.commit(space, operations, wall_millis, rng)?;
		}
		// The file was read whole where it was found: the first check of the original there,
		// which a copy that is no original is not.
		if from.found.is_none() && !passing {
			self.record_first_check(space, (found.path.clone(), content), wall_millis);
		}
		let types = match existing {
			Some(file) => self.types_of(space, file)?,
			None => types.into_iter().filter(|kind| *kind != FOLDER).collect(),
		};
		Ok(AddedFile {
			file,
			known: existing.is_some(),
			origin,
			content,
			types,
			kept: if stored { Kept::Store } else { Kept::InPlace },
			unasked,
		})
	}

	/// The question to ask before the file at `path` is added to a space, if the rule that
	/// would choose its policy, as for a new file found there, asks about files as large:
	/// whether it can be got again, if it is lost.
	///
	/// # Errors
	///
	/// Returns [`NodeError::Unreadable`] if `path` is not a file that can be read, and
	/// another error if the library has no such space, or the space cannot be read.
	pub fn question_for(&self, space: SpaceId, path: &Path) -> Result<Option<Question>, NodeError> {
		let policies = self.read_policies(space)?;
		let size = fs::metadata(path).map_or(0, |metadata| metadata.len());
		// Most files are smaller than any rule asks about, and are not looked at.
		if !policies.asks_about(size) {
			return Ok(None);
		}
		let At {
			metadata,
			resolved,
			found,
		} = at(path)?;
		// The answer changes nothing for a file found here before, unchanged, or a checkout.
		if self.unchanged(space, &found, size)?.is_some() || self.is_checkout(space, &found.path)? {
			return Ok(None);
		}
		let looked = self.look_inside(path, &resolved, &found, &metadata)?;
		let choice = policies.choose(&[], &looked.facts);
		Ok(match (choice.by, resolved.parent()) {
			(ChosenBy::Rule(rule), Some(folder))
				if choice.asks_above.is_some_and(|limit| size > limit) =>
			{
				Some(Question {
					folder: plain(folder.to_owned()),
					size,
					rule,
				})
			}
			_ => None,
		})
	}

	/// Answers the question about a large file for its folder on this device: a rule that
	/// chooses Replaceable for the files there if they can be got again, `again`, and
	/// Standard if not, just before the rule that asked, so that it asks no more about them.
	/// If the space no longer has that default policy, nothing is added, and such files go
	/// into the store.
	///
	/// # Errors
	///
	/// Returns [`NodeError::Unreadable`] if the folder is not one, and another error if the
	/// library has no such space, or the change cannot be saved.
	pub fn answer(
		&mut self,
		space: SpaceId,
		question: &Question,
		again: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let policies = self.read_policies(space)?;
		let policy = policies::answer_policy(space, again);
		if !policies.has_policy(policy) {
			return Ok(());
		}
		let place = if policies.has_rule(question.rule) {
			Place::Before(question.rule)
		} else {
			Place::BeforeTheRest
		};
		let rule = NewRule {
			folder: Some(question.folder.clone()),
			..NewRule::new(policy)
		};
		self.add_rule(space, &rule, place, wall_millis, rng)?;
		Ok(())
	}

	/// Where a file to add is kept, before it is read: as the policy of the file it is says,
	/// if it was found here before, `known`, and otherwise as the rules choose for a new file
	/// found here; and whether the rule that chose asks about a file as large, `size`, and
	/// nobody answered, which sends it into the store.
	fn kept_for(
		&self,
		space: SpaceId,
		known: Option<EntityId>,
		facts: &Facts,
		size: u64,
	) -> Result<(Kept, bool), NodeError> {
		let choice = match known {
			Some(file) => self.policy_of(space, file)?,
			None => self.read_policies(space)?.choose(&[], facts),
		};
		let unasked =
			choice.by != ChosenBy::Own && choice.asks_above.is_some_and(|limit| size > limit);
		Ok((
			if unasked {
				Kept::Store
			} else {
				choice.needs.kept
			},
			unasked,
		))
	}

	/// Where a file of a space is kept, as its policy says, once it is found at `place` on
	/// this device too.
	fn kept_by_policy(
		&self,
		space: SpaceId,
		file: EntityId,
		place: &str,
	) -> Result<Kept, NodeError> {
		let policies = self.read_policies(space)?;
		let state = self.space(space)?.stored.log().state();
		let mut facts = policies::facts_of(state, file)?;
		facts.places.push((self.info.device, place.to_owned()));
		let own = policies::own_choices(state, file)?;
		Ok(policies.choose(&own, &facts).needs.kept)
	}

	/// Reads the file at `path`, resolved as `resolved`, in full, into the store of a space,
	/// or only to hash it, if it is kept where it is, and returns the hash of its content and
	/// its size.
	fn read_content(
		&mut self,
		space: SpaceId,
		(path, resolved): (&Path, &Path),
		kept: Kept,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(Hash, u64), NodeError> {
		let unreadable = |reason: String| NodeError::Unreadable {
			path: path.display().to_string(),
			reason,
		};
		let mut reading = Sniffing {
			inner: File::open(resolved).map_err(|error| unreadable(error.to_string()))?,
			size: 0,
			failed: None,
		};
		let content = match kept {
			Kept::Store => self.import_blob(space, &mut reading, wall_millis, rng),
			Kept::InPlace => {
				let mut hasher = nibkeep_base::Hasher::new();
				io::copy(&mut reading, &mut hasher)
					.map(|_| hasher.finalize())
					.map_err(|error| unreadable(error.to_string()))
			}
		};
		// An error of reading the file is the file's; any other is the library's.
		match (content, reading.failed.take()) {
			(Ok(content), _) => Ok((content, reading.size)),
			(Err(_), Some(reason)) => Err(unreadable(reason)),
			(Err(error), None) => Err(error),
		}
	}

	/// Looks inside the file at `path`, resolved as `resolved` and found as `found`, before
	/// reading it in full: its first bytes and its headers.
	fn look_inside(
		&self,
		path: &Path,
		resolved: &Path,
		found: &Found,
		metadata: &fs::Metadata,
	) -> Result<Looked, NodeError> {
		let mut head = Vec::with_capacity(HEAD);
		File::open(resolved)
			.and_then(|file| file.take(HEAD as u64).read_to_end(&mut head))
			.map_err(|error| NodeError::Unreadable {
				path: path.display().to_string(),
				reason: error.to_string(),
			})?;
		let media_type = infer::get(&head).map(|kind| kind.mime_type().to_owned());
		// What a photo or a video says is read from a file that may be anything; a reader
		// that fails on it says nothing.
		let recorded = catch_unwind(AssertUnwindSafe(|| media::read(resolved))).unwrap_or_default();
		let mut types = vec![FILE];
		types.extend(recorded.kind.clone());
		let mut facts = Facts {
			types: types.iter().cloned().collect(),
			fields: [CONTENT, SIZE].into_iter().collect(),
			media_type: media_type.clone(),
			size: Some(metadata.len()),
			places: vec![(self.info.device, found.path.clone())],
		};
		facts
			.fields
			.extend(recorded.values.iter().map(|(field, _)| field.clone()));
		if found.name.is_some() {
			facts.fields.insert(fields::NAME);
		}
		if media_type.is_some() {
			facts.fields.insert(MEDIA_TYPE);
		}
		Ok(Looked {
			media_type,
			recorded,
			types,
			facts,
		})
	}

	/// The file found before at the same place, if this device found it there with the
	/// same size and modification time, which is taken to be unchanged, as it was then.
	fn unchanged(
		&self,
		space: SpaceId,
		found: &Found,
		size: u64,
	) -> Result<Option<AddedFile>, NodeError> {
		// Without a modification time, only reading the file tells whether it changed.
		if found.modified.is_none() {
			return Ok(None);
		}
		let Some((origin, file)) = self.known_origin(space, found)? else {
			return Ok(None);
		};
		let state = self.space(space)?.stored.log().state();
		// What was found there, as the origin says.
		let first = |field: &FieldName| -> Result<Option<Value>, NodeError> {
			Ok(state
				.current(origin, field)?
				.first()
				.map(|statement| statement.value.clone()))
		};
		let (Some(Value::Blob(content)), Some(Value::Integer(known_size))) =
			(first(&CONTENT)?, first(&SIZE)?)
		else {
			return Ok(None);
		};
		if u64::try_from(known_size).ok() != Some(size) {
			return Ok(None);
		}
		let stored = self.space(space)?.stored.holds_ordinary_blob(&content);
		Ok(Some(AddedFile {
			file,
			known: true,
			origin: None,
			content,
			types: self.types_of(space, file)?,
			kept: if stored { Kept::Store } else { Kept::InPlace },
			unasked: false,
		}))
	}

	/// The file of a space whose content is this, if there is one: the first by ID, of
	/// those that are not deleted; or else the file whose origin or checkout says it held
	/// this content, as a version before its current one.
	fn file_with(&self, space: SpaceId, content: &Hash) -> Result<Option<EntityId>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let holders = open.stored.find(&CONTENT, &Value::Blob(*content))?;
		// A file of a tree is the file at its path, whatever its content (technical design:
		// trees), never the file of content found elsewhere.
		let is_file = |entity: EntityId| -> Result<bool, NodeError> {
			Ok(!state.is_deleted(entity)?
				&& state.types(entity)?.contains(&FILE)
				&& state.current(entity, &IN_TREE)?.is_empty())
		};
		for &entity in &holders {
			let entity = state.canonical(entity)?;
			if is_file(entity)? {
				return Ok(Some(entity));
			}
		}
		for &holder in &holders {
			let holder = state.canonical(holder)?;
			let kinds = state.types(holder)?;
			let of = if kinds.contains(&ORIGIN) {
				ORIGIN_OF
			} else if kinds.contains(&CHECKOUT) {
				CHECKOUT_OF
			} else {
				continue;
			};
			if state.is_deleted(holder)? {
				continue;
			}
			for file in found_at(state, (holder, &of))? {
				if is_file(file)? {
					return Ok(Some(file));
				}
			}
		}
		Ok(None)
	}

	/// The file that this device found where `found` says, as last changed at the same
	/// moment, as an earlier import did: then adding it again records nothing, and an
	/// importer need not unpack it to tell.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn found_before(
		&self,
		space: SpaceId,
		found: &FoundElsewhere,
	) -> Result<Option<EntityId>, NodeError> {
		Ok(self
			.known_origin(space, &Found::elsewhere(found))?
			.map(|(_, file)| file))
	}

	/// An origin that says this device found a file at this path, as last changed at the
	/// same moment, and the file, if neither is deleted.
	fn known_origin(
		&self,
		space: SpaceId,
		found: &Found,
	) -> Result<Option<(EntityId, EntityId)>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let device = Value::Reference(self.info.device);
		let modified = found.modified.clone().map(Value::Date);
		for origin in open.stored.find(&PATH, &Value::Text(found.path.clone()))? {
			let origin = state.canonical(origin)?;
			let values = |field: &FieldName| -> Result<Vec<Value>, NodeError> {
				Ok(state
					.current(origin, field)?
					.into_iter()
					.map(|statement| statement.value.clone())
					.collect())
			};
			if state.is_deleted(origin)?
				|| !values(&FOUND_ON)?.contains(&device)
				|| values(&MODIFIED)?.first() != modified.as_ref()
			{
				continue;
			}
			let Some(Value::Reference(file)) = values(&ORIGIN_OF)?.first().cloned() else {
				continue;
			};
			let file = state.canonical(file)?;
			if !state.is_deleted(file)? && state.types(file)?.contains(&FILE) {
				return Ok(Some((origin, file)));
			}
		}
		Ok(None)
	}

	/// Where the file at `path` is, as the file system resolves it, and where it was found,
	/// as `from` says, or where it is, if it is no other's file, as a checkout or a tree's.
	fn found_where(
		&self,
		space: SpaceId,
		path: &Path,
		from: &FileFrom,
	) -> Result<(fs::Metadata, PathBuf, Found), NodeError> {
		let At {
			metadata,
			resolved,
			mut found,
		} = at(path)?;
		if let Some(elsewhere) = &from.found {
			found = Found::elsewhere(elsewhere);
		} else {
			self.check_not_others(space, path, (&resolved, &found.path))?;
		}
		found.source = from.source;
		Ok((metadata, resolved, found))
	}

	/// Whether a file at `resolved`, as the file system resolves it, is in the system's
	/// temporary folder, while the library is not: such a folder is emptied, as an AI
	/// agent's scratch folder is once its session ends, so a path there tells nothing
	/// lasting. A library kept there is as passing as the files beside it.
	fn in_temporary_folder(&self, resolved: &Path) -> bool {
		// Resolved once for the process, as files are added by the thousand.
		static TEMPORARY: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
		let Some(temporary) = TEMPORARY.get_or_init(|| fs::canonicalize(std::env::temp_dir()).ok())
		else {
			return false;
		};
		resolved.starts_with(temporary)
			&& !fs::canonicalize(&self.directory)
				.is_ok_and(|library| library.starts_with(temporary))
	}

	/// What an origin records of where a file was found: the file, this device, the path,
	/// the content and size found there, when it was changed and made there, and the source
	/// it came from, with its path and name there.
	fn origin_values(
		&self,
		file: EntityId,
		(found, from): (&Found, &FileFrom),
		(content, size): (Hash, u64),
	) -> Vec<(FieldName, Value)> {
		let mut values = vec![
			(ORIGIN_OF, Value::Reference(file)),
			(FOUND_ON, Value::Reference(self.info.device)),
			(PATH, Value::Text(found.path.clone())),
			(CONTENT, Value::Blob(content)),
			(
				SIZE,
				Value::Integer(i64::try_from(size).unwrap_or(i64::MAX)),
			),
		];
		values.extend(found.modified.clone().map(|at| (MODIFIED, Value::Date(at))));
		values.extend(found.created.clone().map(|at| (CREATED, Value::Date(at))));
		values.extend(
			from.source
				.map(|source| (FROM_SOURCE, Value::Reference(source))),
		);
		values.extend(
			from.source_path
				.clone()
				.map(|path| (SOURCE_PATH, Value::Text(path))),
		);
		values.extend(
			from.source_name
				.clone()
				.map(|name| (SOURCE_NAME, Value::Text(name))),
		);
		values
	}

	/// Names the source on the origin of a file found at the same place before, if it names
	/// none: a place has one origin, and a source given later is added to it.
	fn name_source(
		&mut self,
		space: SpaceId,
		found: &Found,
		from: &FileFrom,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(source) = from.source else {
			return Ok(());
		};
		let Some((origin, _)) = self.known_origin(space, found)? else {
			return Ok(());
		};
		let state = self.space(space)?.stored.log().state();
		if !state.current(origin, &FROM_SOURCE)?.is_empty() {
			return Ok(());
		}
		let mut operations = vec![Operation::assert(nibkeep_model::Statement::new(
			nibkeep_base::StatementId::random(rng),
			origin,
			FROM_SOURCE,
			Value::Reference(source),
		))];
		for (field, value) in [
			(SOURCE_PATH, from.source_path.clone()),
			(SOURCE_NAME, from.source_name.clone()),
		] {
			if let Some(value) = value {
				operations.push(Operation::assert(nibkeep_model::Statement::new(
					nibkeep_base::StatementId::random(rng),
					origin,
					field,
					Value::Text(value),
				)));
			}
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// What gives a file the library holds already the type that where it was found again
	/// says it has, as a photo, if it lacks it.
	fn typed(
		&self,
		space: SpaceId,
		file: EntityId,
		kind: Option<TypeName>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let Some(kind) = kind else {
			return Ok(Vec::new());
		};
		let state = self.space(space)?.stored.log().state();
		if state.types(file)?.contains(&kind) {
			return Ok(Vec::new());
		}
		Ok(vec![Operation::assert(nibkeep_model::Statement::new(
			nibkeep_base::StatementId::random(rng),
			file,
			fields::TYPE,
			Value::Text(kind.to_string()),
		))])
	}

	/// What a file the library holds already gets from where it was found again: the values
	/// that `from` gives, where the file has none of that field, or the value the file said
	/// of itself, `said`, which nobody changed since, unlike a value someone set.
	fn found_again(
		&self,
		space: SpaceId,
		file: EntityId,
		(said, from): (Values<'_>, Values<'_>),
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let mut operations = Vec::new();
		for (field, value) in from {
			let current = state.current(file, field)?;
			let as_said = said
				.iter()
				.find(|(known, _)| known == field)
				.map(|(_, value)| value);
			let replaceable = current.is_empty()
				|| current
					.iter()
					.all(|statement| Some(&statement.value) == as_said);
			if !replaceable || current.iter().any(|statement| statement.value == *value) {
				continue;
			}
			operations.push(Operation::replace(
				nibkeep_model::Statement::new(
					nibkeep_base::StatementId::random(rng),
					file,
					field.clone(),
					value.clone(),
				),
				current.iter().map(|statement| statement.id),
			));
		}
		Ok(operations)
	}

	/// What puts a file into folders that it is not in yet.
	fn putting_in_folders(
		&self,
		space: SpaceId,
		file: EntityId,
		folders: &[EntityId],
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let current = state.current(file, &IN_FOLDER)?;
		let mut operations = Vec::new();
		for folder in folders {
			if current
				.iter()
				.any(|statement| statement.value == Value::Reference(*folder))
			{
				continue;
			}
			operations.push(Operation::assert(nibkeep_model::Statement::new(
				nibkeep_base::StatementId::random(rng),
				file,
				IN_FOLDER,
				Value::Reference(*folder),
			)));
		}
		Ok(operations)
	}

	/// The types of an entity of a space, ordered by name.
	fn types_of(&self, space: SpaceId, entity: EntityId) -> Result<Vec<TypeName>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		Ok(state.types(entity)?.into_iter().collect())
	}

	/// Refuses a file to add that is another's: a checkout's copy, which is the file it is a
	/// copy of, whose edits checking in records; or a file in a tree's folder, whose commits
	/// record it (technical design: trees). `resolved` is its path as the file system resolves it,
	/// and `text` that path as text.
	fn check_not_others(
		&self,
		space: SpaceId,
		path: &Path,
		(resolved, text): (&Path, &str),
	) -> Result<(), NodeError> {
		if self.is_checkout(space, text)? {
			return Err(NodeError::CheckedOut(path.to_owned()));
		}
		if self.in_any_tree(resolved)? {
			return Err(NodeError::InTree(path.to_owned()));
		}
		Ok(())
	}

	/// Adds to a space what the standard types named lack, such as logins in a space made
	/// before them, in a change of its own: each as carefully as the space defines its fields
	/// already, after checking that it can be added so. Nothing is added if the space lacks
	/// nothing.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, if a type cannot be added, such as
	/// one whose field the space defines with another kind, or if the change cannot be saved.
	pub fn install_standard(
		&mut self,
		space: SpaceId,
		names: &[TypeName],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let mut operations: Vec<Operation> = Vec::new();
		{
			let open = self.space(space)?;
			let types = &open.types;
			for definition in nibkeep_std::types()
				.iter()
				.filter(|definition| names.contains(definition.name()))
			{
				// As careful as the space's own definitions, which a person may have made
				// more careful, so that what is added is only what the space lacks.
				let definition = types.careful(definition);
				let lacking = types.define_in(&definition, open.stored.log().state(), rng)?;
				if !lacking.is_empty() {
					types.check(&definition)?;
					operations.extend(lacking);
				}
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}
}

/// Values of fields, as a file says them of itself or where it was found says them.
type Values<'a> = &'a [(FieldName, Value)];

/// Values of fields, owned.
type OwnedValues = Vec<(FieldName, Value)>;

/// The values of a new file: its name where it was found, its content, media type and size,
/// what it says of itself with what where it was found gives in place of it, `said`, and the
/// folders it is in.
fn file_values(
	found: &Found,
	(content, size, media_type): (Hash, u64, Option<&str>),
	(said, folders): (Values<'_>, &[EntityId]),
) -> OwnedValues {
	let mut values: OwnedValues = Vec::new();
	if let Some(name) = &found.name {
		values.push((fields::NAME, Value::Name(Name::new(name))));
	}
	values.push((CONTENT, Value::Blob(content)));
	if let Some(media_type) = media_type {
		values.push((MEDIA_TYPE, Value::Text(media_type.to_owned())));
	}
	values.push((
		SIZE,
		Value::Integer(i64::try_from(size).unwrap_or(i64::MAX)),
	));
	values.extend(said.iter().cloned());
	values.extend(
		folders
			.iter()
			.map(|folder| (IN_FOLDER, Value::Reference(*folder))),
	);
	values
}

/// What `from` gives of a file in place of what it says of itself, `recorded`, the time it
/// was taken as [`super::sources::taken_of`] decides it; and what it says of itself with
/// that in place.
fn given_and_said(recorded: &media::Recorded, from: &FileFrom) -> (OwnedValues, OwnedValues) {
	let mut given = from.values.clone();
	if let Some(taken) = &from.taken {
		let own = recorded
			.values
			.iter()
			.find_map(|(field, value)| match value {
				Value::Date(at) if *field == TAKEN => Some(at),
				_ => None,
			});
		given.push((TAKEN, Value::Date(super::sources::taken_of(own, taken))));
	}
	let mut said = recorded.values.clone();
	for (field, value) in &given {
		said.retain(|(known, _)| known != field);
		said.push((field.clone(), value.clone()));
	}
	(given, said)
}

/// The type of a picture that says nothing of itself, by its media type: a photo or a video
/// all the same, if where it was found says when or where it was taken.
fn kind_of(
	recorded: &media::Recorded,
	given: Values<'_>,
	media_type: Option<&str>,
) -> Option<TypeName> {
	if recorded.kind.is_some() || given.is_empty() {
		return None;
	}
	let media = media_type?;
	if media.starts_with("image/") {
		Some(PHOTO)
	} else if media.starts_with("video/") {
		Some(VIDEO)
	} else {
		None
	}
}

/// Where the file at `path` is, as the file system resolves it, and when it last changed.
fn at(path: &Path) -> Result<At, NodeError> {
	let unreadable = |reason: String| NodeError::Unreadable {
		path: path.display().to_string(),
		reason,
	};
	let metadata = fs::metadata(path).map_err(|error| unreadable(error.to_string()))?;
	if !metadata.is_file() {
		return Err(unreadable("not a file".to_owned()));
	}
	let resolved = fs::canonicalize(path).map_err(|error| unreadable(error.to_string()))?;
	let found = Found {
		name: path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.filter(|name| !name.is_empty()),
		path: plain(resolved.clone()).to_string_lossy().into_owned(),
		modified: metadata.modified().ok().and_then(moment),
		created: metadata.created().ok().and_then(moment),
		source: None,
	};
	Ok(At {
		metadata,
		resolved,
		found,
	})
}

/// Where a file was found, on this device.
struct Found {
	/// Its name there, if it has one that is not empty.
	name: Option<String>,
	/// Its path there, as the file system resolves it.
	path: String,
	/// When it was last changed there.
	modified: Option<Edtf>,
	/// When it was made there, if the file system says.
	created: Option<Edtf>,
	/// The source it came from, if it was imported from one.
	source: Option<EntityId>,
}

impl Found {
	/// Where a file was found that is read from a copy of it, as `from` says.
	fn elsewhere(elsewhere: &FoundElsewhere) -> Self {
		let at = |millis: u64| UNIX_EPOCH.checked_add(std::time::Duration::from_millis(millis));
		Self {
			name: elsewhere.name.clone().filter(|name| !name.is_empty()),
			path: elsewhere.path.clone(),
			modified: elsewhere.modified.and_then(at).and_then(moment),
			created: elsewhere.created.and_then(at).and_then(moment),
			source: None,
		}
	}
}

/// Values without languages or other forms.
fn new_values(values: Vec<(FieldName, Value)>) -> Vec<NewValue> {
	values
		.into_iter()
		.map(|(field, value)| NewValue::new(field, value))
		.collect()
}

/// The files that an origin says were found where it says, or that a checkout is a copy of,
/// each once, as the entities they were merged into: those of its current `std.origin-of`
/// or `std.checkout-of` statements, `of`, which a search that matches where they are finds
/// in its place.
pub(crate) fn found_at<S: nibkeep_model::storage::Statements>(
	state: &nibkeep_model::State<S>,
	(record, of): (EntityId, &FieldName),
) -> Result<Vec<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut files = Vec::new();
	for statement in state.current(record, of)? {
		if let Some(file) = statement.value.as_reference() {
			let file = state.canonical(file)?;
			if !files.contains(&file) {
				files.push(file);
			}
		}
	}
	Ok(files)
}

/// A moment of the file system, in UTC, to the second, such as `2026-10-12T10:00:00Z`.
pub(crate) fn moment(at: SystemTime) -> Option<Edtf> {
	let millis = u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
	let day = Day::from_unix_millis(millis, 0)?;
	Edtf::moment(day, Day::second_of_day(millis, 0), 0)
}

/// Where `path` is, as the file system resolves it, written as people write it: the form in
/// which a space keeps the path where a file was found, and the folder of a rule.
///
/// # Errors
///
/// Returns the file system's error if `path` cannot be resolved, such as when it does not
/// exist.
pub fn resolved_path(path: &Path) -> io::Result<PathBuf> {
	fs::canonicalize(path).map(plain)
}

/// A path as people write it. Windows resolves paths into their extended form, such as
/// `\\?\C:\Photos`, which its other programs do not take; elsewhere, and where a path is
/// not Unicode, so that it would not name the same file again, paths are as they are.
pub(crate) fn plain(path: PathBuf) -> PathBuf {
	#[cfg(windows)]
	{
		let Some(text) = path.to_str() else {
			return path;
		};
		if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
			return PathBuf::from(format!(r"\\{rest}"));
		}
		if let Some(rest) = text.strip_prefix(r"\\?\")
			&& rest.as_bytes().get(1) == Some(&b':')
		{
			return PathBuf::from(rest);
		}
	}
	path
}

/// A reader that counts the bytes it reads, and remembers why reading failed, so that an
/// error of the file is told from one of the library.
pub(crate) struct Sniffing<R> {
	pub(crate) inner: R,
	pub(crate) size: u64,
	pub(crate) failed: Option<String>,
}

impl<R: Read> Read for Sniffing<R> {
	fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		let read = self.inner.read(buffer).inspect_err(|error| {
			self.failed = Some(error.to_string());
		})?;
		self.size += read as u64;
		Ok(read)
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_keys::NewProtection;
	use nibkeep_model::schema::TYPE_DEFINITION;
	use nibkeep_testkit::{TestRng, millis_at};

	use super::*;
	use crate::NewLibrary;

	#[test]
	fn paths_are_resolved_as_people_write_them() {
		let resolved = resolved_path(Path::new(".")).expect("resolves");
		assert!(resolved.is_absolute(), "{}", resolved.display());
		assert!(
			!resolved.to_string_lossy().starts_with(r"\\?\"),
			"{}",
			resolved.display()
		);
		#[cfg(windows)]
		{
			assert_eq!(
				plain(PathBuf::from(r"\\?\C:\Photos")),
				PathBuf::from(r"C:\Photos")
			);
			assert_eq!(
				plain(PathBuf::from(r"\\?\UNC\server\share\Photos")),
				PathBuf::from(r"\\server\share\Photos")
			);
		}
	}

	#[test]
	fn a_space_without_the_types_of_files_gets_them() {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-node-file-types-{}", std::process::id()),
		);
		let mut rng = TestRng::new("file types installed");
		let now = millis_at("2026-10-12", 10, 0);
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
		let personal = library.personal_space();
		let definitions = |library: &Library| {
			let open = library.space(personal).expect("a space");
			open.stored
				.find(&fields::TYPE, &Value::Text(TYPE_DEFINITION.to_string()))
				.expect("finds")
				.len()
		};
		let before = definitions(&library);
		// As a space founded before them sees its types.
		let space = library.spaces.get_mut(&personal).expect("a space");
		space.types = super::super::code_types();
		let note = directory.join("note.txt");
		fs::write(&note, "Milk.\n").expect("writes");
		let added = library
			.add_file(personal, &note, now, &mut rng)
			.expect("adds");
		assert_eq!(added.types, [FILE]);
		// std.file and std.origin are defined again.
		assert_eq!(definitions(&library), before + 2);
		assert!(
			library
				.space(personal)
				.expect("a space")
				.types
				.get(&FILE)
				.is_some()
		);
		drop(library);
		let _ = fs::remove_dir_all(&directory);
	}
}
