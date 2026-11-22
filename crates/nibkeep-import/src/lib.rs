//! Importers of files into a library, on the engine's public API, which a plugin can take
//! over once there are plugins ([technical design: importers]): Google Takeout's archives of
//! Google Photos, read in place, each photo with what its JSON file says of it, and its
//! albums as folders.
//!
//! [technical design: importers]: ../../../docs/Technical-design.md#912-importers-and-where-files-came-from

mod archive;
mod takeout;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::{Decimal, Value};
use nibkeep_node::{AddedFile, FileFrom, FoundElsewhere, Library, NodeError};
use nibkeep_std::{DESCRIPTION, LATITUDE, LONGITUDE};
use nibkeep_time::{Day, Edtf};

use archive::{Entry, MOST_METADATA, Part};
use takeout::{Album, Described, MediaName, Metadata, metadata_for};

/// The source that a Takeout import names unless the person names another.
pub const GOOGLE_PHOTOS: &str = "Google Photos";

/// Why an import failed.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
	/// A file or a folder could not be read or written.
	#[error("{}: {source}", path.display())]
	File {
		/// The file or the folder.
		path: PathBuf,
		/// Why.
		source: io::Error,
	},
	/// A part is neither a zip file, nor a tgz file, nor a folder.
	#[error("{} is not a zip or tgz archive, nor a folder", .0.display())]
	NotAnArchive(PathBuf),
	/// A part could not be read as the archive it is.
	#[error("{} is damaged: {}", .0.display(), .1)]
	Damaged(PathBuf, String),
	/// The parts hold no Google Photos.
	#[error("these archives hold no photos of Google Photos with their JSON files")]
	NoPhotos,
	/// The library failed.
	#[error(transparent)]
	Node(#[from] NodeError),
}

impl ImportError {
	/// A function that makes an error of a file's, for `map_err`.
	pub(crate) fn file(path: &Path) -> impl Fn(io::Error) -> Self + '_ {
		move |source| Self::File {
			path: path.to_owned(),
			source,
		}
	}
}

/// A photo or a video of a Takeout archive, as it was imported, or found before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
	/// Its path within the archive.
	pub path: String,
	/// What adding it did; `None` if an earlier import added it from there.
	pub added: Option<AddedFile>,
	/// Whether its JSON file was found.
	pub described: bool,
}

/// What a Takeout import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakeoutReport {
	/// How many photos and videos it added, new to the library.
	pub added: usize,
	/// How many the library held already, found here too.
	pub found_again: usize,
	/// How many an earlier import added from here.
	pub known: usize,
	/// How many came without a JSON file of their own, with what they say of themselves.
	pub undescribed: usize,
	/// The JSON files of photos found in no part, by their paths.
	pub without_photo: Vec<String>,
	/// How many albums became folders, new to the library.
	pub albums: usize,
	/// How many edited copies it linked to their originals, as edits.
	pub edits: usize,
	/// The photos and videos that could not be read, with why.
	pub failed: Vec<(String, String)>,
	/// How many files of other products than Google Photos were left out.
	pub left_out: usize,
}

/// A JSON file of Google Photos, as the first pass read it.
#[derive(Clone, Debug)]
struct JsonFile {
	/// Its name within its folder.
	name: String,
	/// What it says.
	described: Described,
}

/// The JSON files of a folder of Google Photos, with their index by name.
#[derive(Clone, Debug, Default)]
struct JsonFolder {
	files: Vec<JsonFile>,
	by_name: std::collections::HashMap<String, usize>,
}

/// Whether an extension, such as `.mov`, in lower case, is a video's.
fn is_video(extension: &str) -> bool {
	matches!(
		extension,
		".mp4" | ".mov" | ".m4v" | ".3gp" | ".avi" | ".mkv" | ".webm" | ".mts" | ".mpg"
	)
}

/// Whether a file of a folder is the system's rather than the person's, as macOS and Windows
/// leave in folders: a hidden file, `Thumbs.db` or `desktop.ini`.
fn is_systems(name: &str) -> bool {
	name.starts_with('.')
		|| name.eq_ignore_ascii_case("thumbs.db")
		|| name.eq_ignore_ascii_case("desktop.ini")
}

/// Google Takeout's archive of Google Photos, in parts, of which the first pass read the JSON
/// files: the second, [`Takeout::import`], reads the photos and videos
/// ([technical design: importers]).
///
/// [technical design: importers]: ../../../docs/Technical-design.md#912-importers-and-where-files-came-from
#[derive(Debug)]
pub struct Takeout {
	parts: Vec<Part>,
	/// The JSON files of Google Photos, by the folder that holds them within the archive.
	jsons: BTreeMap<String, JsonFolder>,
	/// The folders, within the archive, of the products that are Google Photos: those that
	/// hold JSON files of photos.
	products: BTreeSet<String>,
	/// How many files the parts hold of other products.
	others: usize,
}

/// The folder within the archive that a path is in, and its name.
fn split(path: &str) -> (&str, &str) {
	match path.rfind('/') {
		Some(slash) => (&path[..slash], &path[slash + 1..]),
		None => ("", path),
	}
}

/// The product folder that holds a path within the archive: `Takeout/<product>`, or the
/// archive's first folder where it has no `Takeout` folder, as when a product's folder was
/// given unpacked.
fn product_of(path: &str) -> &str {
	let mut parts = path.splitn(3, '/');
	match (parts.next(), parts.next(), parts.next()) {
		(Some("Takeout"), Some(product), Some(_)) => &path[.."Takeout/".len() + product.len()],
		(Some(first), Some(_), _) => first,
		_ => "",
	}
}

impl Takeout {
	/// Opens the parts of a Takeout archive, zip or tgz files or folders that one was
	/// unpacked into, and reads every JSON file of them: a photo's may be in another part
	/// than the photo. The products that are Google Photos are those whose folder holds a
	/// photo's JSON file; the others, as Drive or Mail, are left out.
	///
	/// # Errors
	///
	/// Returns an error if a part cannot be read, and [`ImportError::NoPhotos`] if none holds
	/// Google Photos.
	pub fn open(paths: &[PathBuf]) -> Result<Self, ImportError> {
		// Each part where the file system resolves it, so that where a photo was found reads
		// alike however the part was named.
		let mut parts = Vec::new();
		for path in paths {
			let resolved = nibkeep_node::resolved_path(path).map_err(ImportError::file(path))?;
			parts.push(Part::of(&resolved)?);
		}
		let mut jsons: BTreeMap<String, JsonFolder> = BTreeMap::new();
		let mut files_by_product: BTreeMap<String, usize> = BTreeMap::new();
		let is_json = |entry: &Entry| {
			entry.path.to_lowercase().ends_with(".json") && entry.size <= MOST_METADATA
		};
		for part in &parts {
			part.visit(&is_json, &mut |entry, reader| {
				*files_by_product
					.entry(product_of(&entry.path).to_owned())
					.or_default() += 1;
				let Some(reader) = reader else {
					return Ok(());
				};
				let mut bytes = Vec::new();
				reader
					.take(MOST_METADATA)
					.read_to_end(&mut bytes)
					.map_err(ImportError::file(part.path()))?;
				let (folder, name) = split(&entry.path);
				let folder = jsons.entry(folder.to_owned()).or_default();
				folder.by_name.insert(name.to_owned(), folder.files.len());
				folder.files.push(JsonFile {
					name: name.to_owned(),
					described: takeout::describe(&bytes),
				});
				Ok(())
			})?;
		}
		let products: BTreeSet<String> = jsons
			.iter()
			.filter(|(_, folder)| {
				folder
					.files
					.iter()
					.any(|json| matches!(json.described, Described::Media(_)))
			})
			.map(|(folder, _)| product_of(&format!("{folder}/x")).to_owned())
			.collect();
		if products.is_empty() {
			return Err(ImportError::NoPhotos);
		}
		let others = files_by_product
			.iter()
			.filter(|(product, _)| !products.contains(*product))
			.map(|(_, count)| count)
			.sum();
		jsons.retain(|folder, _| products.contains(product_of(&format!("{folder}/x"))));
		Ok(Self {
			parts,
			jsons,
			products,
			others,
		})
	}

	/// The album that a folder within the archive is, as its JSON file says, if it has one.
	fn album_of(&self, folder: &str) -> Option<&Album> {
		self.jsons
			.get(folder)?
			.files
			.iter()
			.find_map(|json| match &json.described {
				Described::Album(album) => Some(album),
				_ => None,
			})
	}

	/// The JSON file of a photo at a path within the archive, if it has one.
	fn metadata_of(&self, path: &str) -> Option<(&str, &Metadata)> {
		let (folder, name) = split(path);
		let folder = self.jsons.get(folder)?;
		let files = &folder.files;
		// By an exact name first, without looking through the folder's others.
		for exact in takeout::exact_names(name) {
			if let Some(&index) = folder.by_name.get(&exact)
				&& let Described::Media(metadata) = &files[index].described
			{
				return Some((files[index].name.as_str(), metadata));
			}
		}
		let described: Vec<(&str, Option<&str>)> = files
			.iter()
			.map(|json| {
				let title = match &json.described {
					Described::Media(metadata) => metadata.title.as_deref(),
					_ => None,
				};
				(json.name.as_str(), title)
			})
			.collect();
		let index = metadata_for(name, &described)?;
		match &files[index].described {
			Described::Media(metadata) => Some((files[index].name.as_str(), metadata)),
			_ => None,
		}
	}

	/// Imports the photos and videos into a space, each read from its part into `scratch`,
	/// a folder on the library's disk, and added from there ([`Library::add_file_from`]),
	/// from the source `source` ([`GOOGLE_PHOTOS`] unless the person names another): with
	/// what its JSON file says in place of what it says of itself, its name as uploaded,
	/// and in the folder of each album it is in, inside a folder named after the source. One
	/// that an earlier import added from the same part, unchanged, is not read again, so an
	/// import cut short goes on where it stopped. `each` hears of each photo and video, with
	/// the library, as a program makes thumbnails meanwhile.
	///
	/// # Errors
	///
	/// Returns an error if a part cannot be read, or the library fails; a photo that cannot
	/// be read is in the report, and the others are imported all the same.
	pub fn import(
		&self,
		library: &mut Library,
		(space, source): (SpaceId, &str),
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
		each: &mut dyn FnMut(&mut Library, &Imported) -> Result<(), NodeError>,
	) -> Result<TakeoutReport, ImportError> {
		let source_id = library.source_named(space, source, now(), rng)?;
		let scratch = library.scratch_folder()?;
		let mut importing = Importing {
			takeout: self,
			library,
			space,
			source: (source, source_id),
			scratch,
			written: 0,
			report: TakeoutReport {
				left_out: self.others,
				..TakeoutReport::default()
			},
			folders: BTreeMap::new(),
			parent: None,
			described: BTreeSet::new(),
			originals: BTreeMap::new(),
			edits_waiting: BTreeMap::new(),
			now,
			rng,
			each,
		};
		let is_media = |entry: &Entry| {
			let name = split(&entry.path).1;
			let extension = |wanted: &str| {
				Path::new(name)
					.extension()
					.is_some_and(|extension| extension.eq_ignore_ascii_case(wanted))
			};
			self.products.contains(product_of(&entry.path))
				&& !extension("json")
				&& !extension("html")
				&& !is_systems(name)
		};
		for part in &self.parts {
			part.visit(&is_media, &mut |entry, reader| match reader {
				Some(reader) => importing.entry(part, entry, reader),
				None => Ok(()),
			})?;
		}
		let Importing {
			scratch,
			mut report,
			described,
			..
		} = importing;
		let _ = fs::remove_dir_all(&scratch);
		for (folder, files) in &self.jsons {
			for json in &files.files {
				if matches!(json.described, Described::Media(_))
					&& !described.contains(&(folder.clone(), json.name.clone()))
				{
					report.without_photo.push(format!("{folder}/{}", json.name));
				}
			}
		}
		Ok(report)
	}
}

/// A Takeout import under way.
struct Importing<'a, R> {
	takeout: &'a Takeout,
	library: &'a mut Library,
	space: SpaceId,
	/// The source's name and record.
	source: (&'a str, EntityId),
	/// The folder that photos are unpacked into, and how many were.
	scratch: PathBuf,
	written: u64,
	report: TakeoutReport,
	/// Each album's folder, by the folder within the archive, and the source's folder.
	folders: BTreeMap<String, EntityId>,
	parent: Option<EntityId>,
	/// The JSON files of photos that photos were found for, by folder and name.
	described: BTreeSet<(String, String)>,
	/// The files of the photos and videos imported or found before but edited copies, each
	/// with its extension, by their folder and name without the extension, in lower case;
	/// and the edited copies whose originals are still to come, by their originals' so.
	originals: BTreeMap<String, Vec<(String, EntityId)>>,
	edits_waiting: BTreeMap<String, Vec<(String, EntityId)>>,
	now: &'a dyn Fn() -> u64,
	rng: &'a mut R,
	each: &'a mut dyn FnMut(&mut Library, &Imported) -> Result<(), NodeError>,
}

impl<R: CryptoRng> Importing<'_, R> {
	/// Imports a photo or a video of a part, read from `reader`.
	fn entry(
		&mut self,
		part: &Part,
		entry: &Entry,
		reader: &mut dyn Read,
	) -> Result<(), ImportError> {
		let (space, (_, source)) = (self.space, self.source);
		let (folder, name) = split(&entry.path);
		let found = FoundElsewhere {
			path: format!("{}/{}", part.path().display(), entry.path),
			name: Some(name.to_owned()),
			modified: entry.modified,
			created: None,
		};
		let metadata = self.takeout.metadata_of(&entry.path);
		if let Some((json, _)) = metadata {
			self.described.insert((folder.to_owned(), json.to_owned()));
		}
		if let Some(file) = self.library.found_before(space, &found)? {
			self.report.known += 1;
			self.link_edits(&entry.path, file)?;
			let imported = Imported {
				path: entry.path.clone(),
				added: None,
				described: metadata.is_some(),
			};
			return Ok((self.each)(self.library, &imported)?);
		}
		let folders = self.album_folder(folder)?.into_iter().collect();
		self.written += 1;
		let copy = self.scratch.join(self.written.to_string());
		let saved = (|| -> io::Result<()> {
			let mut file = File::create(&copy)?;
			io::copy(reader, &mut file)?;
			file.flush()
		})();
		if let Err(error) = saved {
			let _ = fs::remove_file(&copy);
			self.report
				.failed
				.push((entry.path.clone(), error.to_string()));
			return Ok(());
		}
		let mut from = FileFrom {
			source: Some(source),
			source_path: Some(entry.path.clone()),
			found: Some(FoundElsewhere {
				created: metadata
					.and_then(|(_, metadata)| metadata.created)
					.and_then(|seconds| u64::try_from(seconds).ok())
					.and_then(|seconds| seconds.checked_mul(1_000)),
				..found
			}),
			folders,
			..FileFrom::default()
		};
		if let Some((_, metadata)) = metadata {
			apply(&mut from, metadata, name);
		} else {
			self.report.undescribed += 1;
		}
		let added = self
			.library
			.add_file_from(space, &copy, &from, (self.now)(), self.rng);
		let _ = fs::remove_file(&copy);
		match added {
			Ok(added) => {
				if added.known {
					self.report.found_again += 1;
				} else {
					self.report.added += 1;
				}
				self.link_edits(&entry.path, added.file)?;
				let imported = Imported {
					path: entry.path.clone(),
					added: Some(added),
					described: metadata.is_some(),
				};
				(self.each)(self.library, &imported)?;
			}
			Err(NodeError::Unreadable { reason, .. }) => {
				self.report.failed.push((entry.path.clone(), reason));
			}
			Err(error) => return Err(error.into()),
		}
		Ok(())
	}

	/// Links the file at `path` within the archive to its original, if it is an edited copy,
	/// and the edited copies of it, as edits, once both are known: an edited copy is named as
	/// its original with Google's mark of an edit, in the same folder, whatever its extension,
	/// as Google may save the edit of a HEIC photo as a JPEG; of several originals so named,
	/// the one of its own extension, else one that is no video, as a Live Photo's video shares
	/// its photo's name. Before thumbnails are made, so that an edit is never taken for a copy.
	fn link_edits(&mut self, path: &str, file: EntityId) -> Result<(), ImportError> {
		let (folder, name) = split(path);
		let media = MediaName::of(name);
		let copy = media.copy.map_or_default(|number| format!("({number})"));
		let key = format!("{folder}/{}{copy}", media.stem).to_lowercase();
		let extension = media.extension.to_lowercase();
		let mut pairs = Vec::new();
		if media.edited {
			let original = self.originals.get(&key).and_then(|originals| {
				originals
					.iter()
					.find(|(theirs, _)| *theirs == extension)
					.or_else(|| originals.iter().find(|(theirs, _)| !is_video(theirs)))
					.map(|(_, original)| *original)
			});
			match original {
				Some(original) => pairs.push((file, original)),
				None => self
					.edits_waiting
					.entry(key)
					.or_default()
					.push((extension, file)),
			}
		} else {
			if let Some(waiting) = self.edits_waiting.get_mut(&key) {
				let (now, later) = waiting
					.drain(..)
					.partition(|(theirs, _)| *theirs == extension || !is_video(&extension));
				*waiting = later;
				pairs.extend(
					now.into_iter()
						.map(|(_, edit): (String, EntityId)| (edit, file)),
				);
			}
			self.originals
				.entry(key)
				.or_default()
				.push((extension, file));
		}
		for pair in pairs {
			if self
				.library
				.link_edit(self.space, pair, (self.now)(), self.rng)?
			{
				self.report.edits += 1;
			}
		}
		Ok(())
	}

	/// The folder of the album that a folder within the archive is, inside the source's
	/// folder, made the first time; `None` if it is no album.
	fn album_folder(&mut self, folder: &str) -> Result<Option<EntityId>, ImportError> {
		let Some(album) = self.takeout.album_of(folder) else {
			return Ok(None);
		};
		if let Some(id) = self.folders.get(folder) {
			return Ok(Some(*id));
		}
		let (space, (source, _)) = (self.space, self.source);
		let parent = if let Some(parent) = self.parent {
			parent
		} else {
			let (made, _) =
				self.library
					.folder_named(space, (source, None), None, (self.now)(), self.rng)?;
			self.parent = Some(made);
			made
		};
		let (id, made) = self.library.folder_named(
			space,
			(&album.title, Some(parent)),
			album.description.as_deref(),
			(self.now)(),
			self.rng,
		)?;
		self.folders.insert(folder.to_owned(), id);
		if made {
			self.report.albums += 1;
		}
		Ok(Some(id))
	}
}

/// What a photo's JSON file says, in `from`: when it was taken, where, its description, and
/// its name as uploaded, where it differs from the file's.
fn apply(from: &mut FileFrom, metadata: &Metadata, name: &str) {
	from.taken = metadata.taken.and_then(utc_moment);
	if let Some((latitude, longitude)) = metadata.place
		&& let (Some(latitude), Some(longitude)) = (degrees(latitude), degrees(longitude))
	{
		from.values.push((LATITUDE, Value::Decimal(latitude)));
		from.values.push((LONGITUDE, Value::Decimal(longitude)));
	}
	if let Some(description) = &metadata.description {
		from.values
			.push((DESCRIPTION, Value::Text(description.clone())));
	}
	from.source_name = metadata
		.title
		.clone()
		.filter(|title| title != name && !title.is_empty());
}

/// A moment of `seconds` since 1970, in UTC, as `2019-12-24T17:30:00Z`.
fn utc_moment(seconds: i64) -> Option<Edtf> {
	let millis = u64::try_from(seconds).ok()?.checked_mul(1_000)?;
	let day = Day::from_unix_millis(millis, 0)?;
	Edtf::moment(day, Day::second_of_day(millis, 0), 0)
}

/// Degrees to six decimal places, as photos' own places are kept.
fn degrees(degrees: f64) -> Option<Decimal> {
	degrees
		.is_finite()
		.then(|| format!("{degrees:.6}"))
		.and_then(|text| text.parse().ok())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_path_is_in_the_folder_of_its_product() {
		assert_eq!(
			product_of("Takeout/Google Photos/Christmas/IMG_1.jpg"),
			"Takeout/Google Photos"
		);
		assert_eq!(
			product_of("Takeout/Google Fotos/x.json"),
			"Takeout/Google Fotos"
		);
		assert_eq!(product_of("Christmas/IMG_1.jpg"), "Christmas");
		assert_eq!(product_of("IMG_1.jpg"), "");
		assert_eq!(
			utc_moment(1_577_208_600),
			"2019-12-24T17:30:00Z".parse().ok()
		);
	}
}
