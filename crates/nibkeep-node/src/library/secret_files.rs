//! Secret files: content that only the replicas holding the secrets read, such as the scan
//! of a passport, sealed in the store, with no record of where it was found or of its plain
//! copies ([technical design: secret files]).
//!
//! [technical design: secret files]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#95-secret-files

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::Zeroizing;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::{Name, Value, fields};
use nibkeep_std::{ATTACHED_TO, CONTENT, MEDIA_TYPE, SECRET_CONTENT, SECRET_FILE, SIZE};

use super::Library;
use super::checkouts::{copy_checked, place_for, put_in_place, remove_leftover, temporary_for};
use super::entities::NewValue;
use super::files::HEAD;
use crate::NodeError;

/// What adding a secret file did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedSecretFile {
	/// The secret file.
	pub file: EntityId,
	/// The hash of its content.
	pub content: Hash,
	/// Its media type, as its first bytes tell it, if they do.
	pub media_type: Option<String>,
	/// Its size in bytes.
	pub size: u64,
	/// Whether the space holds the same content where the secrets do not protect it, as an
	/// ordinary file's, which every replica can read.
	pub unprotected_copy: bool,
}

impl Library {
	/// Adds a secret file to a space, with its secrets unlocked: seals `content` for the
	/// secrets, so that only the replicas that hold them read it, and records it as a secret
	/// file named `name`, with its media type as its first bytes tell it and its size,
	/// attached to the entities `attached_to`. Nothing records where it was found: the person
	/// decides what becomes of the original. The type of secret files is added to the space
	/// if it lacks it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsLocked`] if the space's secrets are locked or were never
	/// set up, [`NodeError::NoEntity`] if an entity to attach it to is not in the space,
	/// [`NodeError::Unreadable`], naming the file by `name`, if reading `content` fails, and
	/// another error if the library has no such space, or the content or the change cannot
	/// be saved.
	pub fn add_secret_file(
		&mut self,
		space: SpaceId,
		name: &str,
		content: impl Read,
		attached_to: &[EntityId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedSecretFile, NodeError> {
		if !self.secrets_unlocked(space) {
			return Err(NodeError::SecretsLocked);
		}
		// What would be refused is refused before anything is sealed.
		self.check_not_revoked(space)?;
		let mut records = Vec::with_capacity(attached_to.len());
		{
			let state = self.space(space)?.stored.log().state();
			for &record in attached_to {
				let canonical = state.canonical(record)?;
				if state.is_deleted(canonical)? || state.types(canonical)?.is_empty() {
					return Err(NodeError::NoEntity(record.to_string()));
				}
				records.push(canonical);
			}
		}
		self.install_standard(space, &[SECRET_FILE], wall_millis, rng)?;
		let mut reading = Sniffed {
			inner: content,
			head: Zeroizing::new(Vec::with_capacity(HEAD)),
			size: 0,
			failed: None,
		};
		let imported = self.seal_blob(space, (&mut reading, true), wall_millis, rng);
		// An error of reading the file is the file's; any other is the library's.
		let content = match (imported, reading.failed.take()) {
			(Ok(content), _) => content,
			(Err(_), Some(reason)) => {
				return Err(NodeError::Unreadable {
					path: name.to_owned(),
					reason,
				});
			}
			(Err(error), None) => return Err(error),
		};
		// An ordinary copy here, or a file anywhere that holds the content, as the space says.
		let open = self.space(space)?;
		let unprotected_copy = open.stored.holds_ordinary_blob(&content)
			|| !open
				.stored
				.find(&CONTENT, &Value::Blob(content))?
				.is_empty();
		let media_type = infer::get(&reading.head).map(|kind| kind.mime_type().to_owned());
		let size = reading.size;
		drop(reading);

		let mut values = Vec::new();
		if !name.trim().is_empty() {
			values.push(NewValue::new(fields::NAME, Value::Name(Name::new(name))));
		}
		values.push(NewValue::new(SECRET_CONTENT, Value::Blob(content)));
		if let Some(media_type) = &media_type {
			values.push(NewValue::new(MEDIA_TYPE, Value::Text(media_type.clone())));
		}
		values.push(NewValue::new(
			SIZE,
			Value::Integer(i64::try_from(size).unwrap_or(i64::MAX)),
		));
		values.extend(
			records
				.into_iter()
				.map(|record| NewValue::new(ATTACHED_TO, Value::Reference(record))),
		);
		let (file, operations) = self.new_entity(space, &[SECRET_FILE], values, rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok(AddedSecretFile {
			file,
			content,
			media_type,
			size,
			unprotected_copy,
		})
	}

	/// Opens the content of a secret file of a space for reading, with the secrets unlocked.
	/// The reader checks the content's hash when it reads all of it in order.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsLocked`] if the space's secrets are locked,
	/// [`NodeError::NotSecretFile`] if the entity is no secret file, [`NodeError::Deleted`] if
	/// it is deleted, [`NodeError::NoCopyHere`] if this device holds no copy of its content
	/// yet, and another error if the library has no such space, or the content cannot be read.
	pub fn open_secret_file(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<impl Read + use<>, NodeError> {
		let (file, name, content) = self.secret_content(space, file)?;
		self.open_content(space, (file, name.as_deref()), &content)
	}

	/// Opens content that this device holds a copy of, for reading.
	fn open_content(
		&self,
		space: SpaceId,
		(file, name): (EntityId, Option<&str>),
		content: &Hash,
	) -> Result<impl Read + use<>, NodeError> {
		let stored = &self.space(space)?.stored;
		if !stored.holds_blob(content) {
			let name = name.map_or_else(|| file.to_string(), str::to_owned);
			return Err(NodeError::NoCopyHere(name));
		}
		Ok(stored.open_blob(content)?)
	}

	/// Saves a plain copy of a secret file of a space, with the secrets unlocked, at
	/// `place`: into a folder, under the file's name, or as a new file. Nothing records it:
	/// NibKeep neither protects the copy, nor knows of it, nor deletes it when the file is
	/// purged. It replaces nothing. The copy is written beside its place first, checked
	/// against the content's hash and synced; on Unix, only its owner may read it. Returns
	/// where the copy is.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::open_secret_file`], [`NodeError::PlaceTaken`] if
	/// something is where the copy would go, [`NodeError::NotAFileName`] if a copy into a
	/// folder would have no name, and [`NodeError::File`] if writing fails.
	pub fn save_secret_file(
		&self,
		space: SpaceId,
		file: EntityId,
		place: &Path,
	) -> Result<PathBuf, NodeError> {
		let (file, name, content) = self.secret_content(space, file)?;
		let path = place_for(place, name.as_deref())?;
		if path.symlink_metadata().is_ok() {
			return Err(NodeError::PlaceTaken(path));
		}
		let mut reader = self.open_content(space, (file, name.as_deref()), &content)?;
		let temporary = temporary_for(&path, "copy");
		remove_leftover(&temporary);
		let written = write_private(&temporary, &mut reader, &content);
		match written {
			Ok(true) => {}
			Ok(false) => {
				remove_leftover(&temporary);
				return Err(NodeError::Store(nibkeep_store::StoreError::DamagedBlob));
			}
			Err(error) => {
				remove_leftover(&temporary);
				return Err(NodeError::file(&temporary)(error));
			}
		}
		if let Err(error) = put_in_place(&temporary, &path) {
			remove_leftover(&temporary);
			if error.kind() == io::ErrorKind::AlreadyExists {
				return Err(NodeError::PlaceTaken(path));
			}
			return Err(NodeError::file(&path)(error));
		}
		Ok(path)
	}

	/// A secret file of a space, as the entity it is now, its name, and its current content:
	/// of competing values, the one in the statement with the smallest ID.
	fn secret_content(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<(EntityId, Option<String>, Hash), NodeError> {
		if !self.secrets_unlocked(space) {
			return Err(NodeError::SecretsLocked);
		}
		let state = self.space(space)?.stored.log().state();
		let file = state.canonical(file)?;
		let name = state
			.current(file, &fields::NAME)?
			.iter()
			.min_by_key(|statement| statement.id)
			.and_then(|statement| statement.value.as_name_text().map(str::to_owned));
		let described = || name.clone().unwrap_or_else(|| file.to_string());
		if !state.types(file)?.contains(&SECRET_FILE) {
			return Err(NodeError::NotSecretFile(described()));
		}
		if state.is_deleted(file)? {
			return Err(NodeError::Deleted(described()));
		}
		let content = state
			.current(file, &SECRET_CONTENT)?
			.iter()
			.filter(|statement| matches!(statement.value, Value::Blob(_)))
			.min_by_key(|statement| statement.id)
			.and_then(|statement| match statement.value {
				Value::Blob(content) => Some(content),
				_ => None,
			})
			.ok_or_else(|| NodeError::NoContent(described()))?;
		Ok((file, name, content))
	}
}

/// A reader that keeps the first bytes it reads, which tell the media type, counts them all,
/// and remembers why reading failed. What it keeps is wiped when it goes.
struct Sniffed<R> {
	inner: R,
	head: Zeroizing<Vec<u8>>,
	size: u64,
	failed: Option<String>,
}

impl<R: Read> Read for Sniffed<R> {
	fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		let read = self.inner.read(buffer).inspect_err(|error| {
			self.failed = Some(error.to_string());
		})?;
		let room = HEAD.saturating_sub(self.head.len());
		self.head.extend_from_slice(&buffer[..read.min(room)]);
		self.size += read as u64;
		Ok(read)
	}
}

/// Writes what `reader` reads to `temporary`, a new file that on Unix only its owner may
/// read, and syncs it: whether what was written has the hash `content`.
fn write_private(temporary: &Path, reader: &mut impl Read, content: &Hash) -> io::Result<bool> {
	let mut options = OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	let mut copy: File = options.open(temporary)?;
	if !copy_checked(reader, &mut copy, content)? {
		return Ok(false);
	}
	copy.sync_all()?;
	Ok(true)
}
