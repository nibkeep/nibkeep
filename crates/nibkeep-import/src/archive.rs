//! The parts of an archive, as Google Takeout writes them, read in place: zip files, tgz
//! files, or a folder that one was unpacked into. Each entry is named by its path within the
//! archive, its names joined by `/`, which is the same in every part, as one folder's files
//! may be split among parts.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use flate2::read::GzDecoder;

use crate::ImportError;

/// The most bytes of a metadata file that are read: 4 MiB.
pub(crate) const MOST_METADATA: u64 = 4 * 1024 * 1024;

/// A part of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
	/// A zip file.
	Zip(PathBuf),
	/// A gzipped tar file.
	Tgz(PathBuf),
	/// A folder that an archive was unpacked into.
	Folder(PathBuf),
}

/// What visits the files of a part: each with a reader of its bytes, if it is wanted.
pub(crate) type Visitor<'a> =
	dyn FnMut(&Entry, Option<&mut dyn Read>) -> Result<(), ImportError> + 'a;

/// An entry of a part: a file, by its path within the archive, with its size and when it was
/// last changed, in milliseconds since 1970, as the archive says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
	/// Its path within the archive, its names joined by `/`.
	pub(crate) path: String,
	/// Its size in bytes.
	pub(crate) size: u64,
	/// When it was last changed, if the archive says.
	pub(crate) modified: Option<u64>,
}

impl Part {
	/// The part at `path`: a folder, or a file whose name ends in `.zip`, `.tgz` or
	/// `.tar.gz`, or else whose first bytes tell.
	pub(crate) fn of(path: &Path) -> Result<Self, ImportError> {
		if path.is_dir() {
			return Ok(Self::Folder(path.to_owned()));
		}
		let extension = |wanted: &str| {
			path.extension()
				.is_some_and(|extension| extension.eq_ignore_ascii_case(wanted))
		};
		let tar_gz = path
			.file_stem()
			.map(Path::new)
			.and_then(Path::extension)
			.is_some_and(|inner| inner.eq_ignore_ascii_case("tar"));
		if extension("zip") {
			return Ok(Self::Zip(path.to_owned()));
		}
		if extension("tgz") || (extension("gz") && tar_gz) {
			return Ok(Self::Tgz(path.to_owned()));
		}
		let mut head = [0; 4];
		let read = File::open(path)
			.and_then(|mut file| file.read(&mut head))
			.map_err(ImportError::file(path))?;
		match &head[..read] {
			[b'P', b'K', 3, 4] => Ok(Self::Zip(path.to_owned())),
			[0x1f, 0x8b, ..] => Ok(Self::Tgz(path.to_owned())),
			_ => Err(ImportError::NotAnArchive(path.to_owned())),
		}
	}

	/// Where the part is.
	pub(crate) fn path(&self) -> &Path {
		match self {
			Self::Zip(path) | Self::Tgz(path) | Self::Folder(path) => path,
		}
	}

	/// Visits each file of the part, in the order the part holds them, with a reader of its
	/// bytes when `wanted` says so, which the visitor may read or leave.
	pub(crate) fn visit(
		&self,
		wanted: &dyn Fn(&Entry) -> bool,
		visit: &mut Visitor<'_>,
	) -> Result<(), ImportError> {
		match self {
			Self::Zip(path) => visit_zip(path, wanted, visit),
			Self::Tgz(path) => visit_tgz(path, wanted, visit),
			Self::Folder(path) => visit_folder(path, wanted, visit),
		}
	}
}

/// Visits each file of a zip file.
fn visit_zip(
	path: &Path,
	wanted: &dyn Fn(&Entry) -> bool,
	visit: &mut Visitor<'_>,
) -> Result<(), ImportError> {
	let file = File::open(path).map_err(ImportError::file(path))?;
	let mut archive = zip::ZipArchive::new(BufReader::new(file))
		.map_err(|error| ImportError::Damaged(path.to_owned(), error.to_string()))?;
	for index in 0..archive.len() {
		let mut file = archive
			.by_index(index)
			.map_err(|error| ImportError::Damaged(path.to_owned(), error.to_string()))?;
		if !file.is_file() {
			continue;
		}
		let Some(name) = file.enclosed_name() else {
			continue;
		};
		let entry = Entry {
			path: joined(&name),
			size: file.size(),
			modified: file.last_modified().and_then(|at| {
				millis_of(
					(i64::from(at.year()), at.month(), at.day()),
					(at.hour(), at.minute(), at.second()),
				)
			}),
		};
		if wanted(&entry) {
			visit(&entry, Some(&mut file))?;
		} else {
			visit(&entry, None)?;
		}
	}
	Ok(())
}

/// Visits each file of a gzipped tar file, as it streams.
fn visit_tgz(
	path: &Path,
	wanted: &dyn Fn(&Entry) -> bool,
	visit: &mut Visitor<'_>,
) -> Result<(), ImportError> {
	let file = File::open(path).map_err(ImportError::file(path))?;
	let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
	let damaged = |error: io::Error| ImportError::Damaged(path.to_owned(), error.to_string());
	for item in archive.entries().map_err(damaged)? {
		let mut item = item.map_err(damaged)?;
		if !item.header().entry_type().is_file() {
			continue;
		}
		let name = item.path().map_err(damaged)?.into_owned();
		// No path that leaves the archive's folder, as `..` would.
		if name
			.components()
			.any(|part| !matches!(part, std::path::Component::Normal(_)))
		{
			continue;
		}
		let entry = Entry {
			path: joined(&name),
			size: item.size(),
			modified: item
				.header()
				.mtime()
				.ok()
				.and_then(|seconds| seconds.checked_mul(1_000)),
		};
		if wanted(&entry) {
			visit(&entry, Some(&mut item))?;
		} else {
			visit(&entry, None)?;
		}
	}
	Ok(())
}

/// Visits each file in a folder and its folders, in the order of their paths, but links.
fn visit_folder(
	root: &Path,
	wanted: &dyn Fn(&Entry) -> bool,
	visit: &mut Visitor<'_>,
) -> Result<(), ImportError> {
	let mut files = Vec::new();
	let mut folders = vec![root.to_owned()];
	while let Some(folder) = folders.pop() {
		let entries = fs::read_dir(&folder).map_err(ImportError::file(&folder))?;
		for entry in entries {
			let entry = entry.map_err(ImportError::file(&folder))?;
			let kind = entry
				.file_type()
				.map_err(ImportError::file(&entry.path()))?;
			if kind.is_dir() {
				folders.push(entry.path());
			} else if kind.is_file() {
				files.push(entry.path());
			}
		}
	}
	files.sort();
	for path in files {
		let Ok(within) = path.strip_prefix(root) else {
			continue;
		};
		let metadata = fs::metadata(&path).map_err(ImportError::file(&path))?;
		let entry = Entry {
			path: joined(within),
			size: metadata.len(),
			modified: metadata
				.modified()
				.ok()
				.and_then(|at| at.duration_since(UNIX_EPOCH).ok())
				.and_then(|since| u64::try_from(since.as_millis()).ok()),
		};
		if wanted(&entry) {
			let mut file = File::open(&path).map_err(ImportError::file(&path))?;
			visit(&entry, Some(&mut file))?;
		} else {
			visit(&entry, None)?;
		}
	}
	Ok(())
}

/// A path's names joined by `/`, as an archive names its entries.
fn joined(path: &Path) -> String {
	path.components()
		.filter_map(|part| match part {
			std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
			_ => None,
		})
		.collect::<Vec<_>>()
		.join("/")
}

/// The milliseconds since 1970 of a date and time that an archive wrote, taken as UTC, as a
/// zip file's carry no time zone.
fn millis_of(
	(year, month, day): (i64, u8, u8),
	(hour, minute, second): (u8, u8, u8),
) -> Option<u64> {
	let day = nibkeep_time::Day::from_ymd(year, month, day)?;
	let seconds = day.days_since_epoch() * 86_400
		+ i64::from(hour) * 3_600
		+ i64::from(minute) * 60
		+ i64::from(second);
	u64::try_from(seconds).ok()?.checked_mul(1_000)
}
