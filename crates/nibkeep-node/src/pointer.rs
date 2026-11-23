//! The `.nib` file in the folder of a tree outside its library's folder, which names the
//! library, as a worktree's `.git` file names its repository, so that commands run there find
//! it; and none in the folder of a tree inside it, where the commands find the library
//! without one, and a file left there would take them over. `nib` and apps that start
//! trees settle it alike ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Display, Path, PathBuf};

use nibkeep_base::SpaceId;

use crate::{Library, NodeError, Tree, resolved_path};

/// The folder in a library's folder that holds the library, and the file in a tree's folder
/// that names it.
pub const LIBRARY_DATA: &str = ".nib";

/// What the file's one line says before the library's folder.
const NAMES: &str = "library: ";

/// The most of a `.nib` file that is read, far more than any path.
const MOST: u64 = 64 * 1024;

/// What is wrong with a `.nib` file, or with writing one.
#[derive(Debug, thiserror::Error)]
pub enum PointerError {
	/// A `.nib` file in a folder that names no library that may be opened.
	#[error("{} should name the library of the tree in its folder, but {why}", .file.display())]
	Bad {
		/// The file.
		file: PathBuf,
		/// What is wrong with it.
		why: String,
	},
	/// A `.nib` file that names a library which keeps no tree in that folder on this device,
	/// as a copy of a tree's folder would.
	#[error(
		"{} names the library in {}, which keeps no tree in that folder on this device",
		.file.display(),
		.library.display()
	)]
	Stray {
		/// The file.
		file: PathBuf,
		/// The library's folder.
		library: PathBuf,
	},
	/// Something other than a file where the `.nib` file of a tree outside the library's
	/// folder goes.
	#[error(
		"{} is no file: the file there names the library of a tree outside the library's folder",
		.0.display()
	)]
	InTheWay(PathBuf),
	/// The `.nib` file of a tree outside the library's folder could not be written.
	#[error(
		"{} could not be written: {error}; the tree is there, and nib init {} in the library's folder writes the file again",
		.file.display(),
		folder_of(.file)
	)]
	NotWritten {
		/// The file.
		file: PathBuf,
		/// Why not.
		error: io::Error,
	},
	/// The `.nib` file of a tree inside the library's folder, which would take over the
	/// commands there, could not be deleted.
	#[error(
		"{} could not be deleted: {error}; the tree is there, and once the file is gone, commands in {} find the library",
		.file.display(),
		folder_of(.file)
	)]
	NotTakenAway {
		/// The file.
		file: PathBuf,
		/// Why not.
		error: io::Error,
	},
	/// The library could not be read.
	#[error(transparent)]
	Library(#[from] NodeError),
}

/// A library that a command found, and the `.nib` file that named it, if one did.
#[derive(Debug)]
pub struct Found {
	/// The library's folder, as the file system resolves it.
	pub folder: PathBuf,
	/// The `.nib` file in a tree's folder that named the library, if one did.
	pub named_in: Option<PathBuf>,
}

impl Found {
	/// A library found in its own folder, or given.
	#[must_use]
	pub const fn library(folder: PathBuf) -> Self {
		Self {
			folder,
			named_in: None,
		}
	}
}

/// A folder as the file system resolves it, so that one folder is named one way: the
/// nearest part of it that exists, resolved, then the rest, which nothing can link
/// elsewhere yet.
#[must_use]
pub fn resolved_as_far_as_it_exists(folder: PathBuf) -> PathBuf {
	let mut rest = Vec::new();
	let mut existing = folder.as_path();
	loop {
		if let Ok(mut resolved) = resolved_path(existing) {
			for part in rest.into_iter().rev() {
				match part {
					Component::ParentDir => {
						resolved.pop();
					}
					Component::CurDir => {}
					part => resolved.push(part),
				}
			}
			return resolved;
		}
		let mut parts = existing.components();
		match parts.next_back() {
			Some(last) if !parts.as_path().as_os_str().is_empty() => {
				rest.push(last);
				existing = parts.as_path();
			}
			_ => return folder,
		}
	}
}

/// Whether a folder's `.nib` holds a library, what is left of starting one, or copies of
/// replicas made elsewhere.
#[must_use]
pub fn holds_library(folder: &Path) -> bool {
	let data = folder.join(LIBRARY_DATA);
	data.is_dir() && Library::exists(&data)
}

/// The folder that a `.nib` file is in, for messages.
#[must_use]
pub fn folder_of(file: &Path) -> Display<'_> {
	file.parent().unwrap_or(file).display()
}

/// The nearest library of `here` and the folders it is in, below `ceiling`: the folder whose
/// `.nib` holds it, or the one that a `.nib` file names.
///
/// # Errors
///
/// Returns [`PointerError::Bad`] if the nearest `.nib` is a file that names no library that
/// may be opened; nothing looks further, as the person meant that library.
pub fn nearest(here: &Path, ceiling: Option<&Path>) -> Result<Option<Found>, PointerError> {
	for folder in here
		.ancestors()
		.take_while(|folder| ceiling != Some(*folder))
	{
		let data = folder.join(LIBRARY_DATA);
		if data.is_dir() {
			if Library::exists(&data) {
				return Ok(Some(Found::library(resolved_as_far_as_it_exists(
					folder.to_owned(),
				))));
			}
		} else if data.is_file() {
			let named = named(&data, ceiling)?;
			return Ok(Some(Found {
				folder: named,
				named_in: Some(data),
			}));
		}
	}
	Ok(None)
}

/// The folder of the library that the `.nib` file `file` names, as the file system resolves
/// it.
fn named(file: &Path, ceiling: Option<&Path>) -> Result<PathBuf, PointerError> {
	let bad = |why: String| PointerError::Bad {
		file: file.to_owned(),
		why,
	};
	let mut text = String::new();
	fs::File::open(file)
		.and_then(|opened| opened.take(MOST).read_to_string(&mut text))
		.map_err(|error| bad(format!("it cannot be read: {error}")))?;
	// A mark of UTF-8 that some editors write first.
	let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
	let line = text
		.strip_suffix('\n')
		.map_or(text, |line| line.strip_suffix('\r').unwrap_or(line));
	let Some(named) = line
		.strip_prefix(NAMES)
		.filter(|named| !named.contains(['\n', '\r']))
	else {
		return Err(bad(format!("it is not one line, {NAMES}FOLDER")));
	};
	let named = PathBuf::from(named);
	if !named.is_absolute() {
		return Err(bad(format!("{} is not a full path", named.display())));
	}
	// A build under development never leaves its ceiling, as it never looks above it.
	if let Some(ceiling) = ceiling {
		let ceiling = resolved_as_far_as_it_exists(ceiling.to_owned());
		let below = resolved_as_far_as_it_exists(named.clone());
		if below == ceiling || !below.starts_with(&ceiling) {
			return Err(bad(format!(
				"{} is not below {}, which NIBKEEP_CEILING names",
				named.display(),
				ceiling.display()
			)));
		}
	}
	if !holds_library(&named) {
		return Err(bad(format!(
			"{} holds no library; run nib init {} in the library's folder, its new one if it moved",
			named.display(),
			folder_of(file)
		)));
	}
	Ok(resolved_as_far_as_it_exists(named))
}

/// The tree whose folder on this device is `folder` itself, in any space of the library,
/// with its space.
///
/// # Errors
///
/// Returns an error if a space cannot be read.
pub fn tree_rooted_at(
	library: &Library,
	folder: &Path,
) -> Result<Option<(SpaceId, Tree)>, NodeError> {
	for space in library.spaces() {
		if let Some(tree) = library.tree_at(space, folder)?
			&& tree.root.as_deref() == Some(folder)
		{
			return Ok(Some((space, tree)));
		}
	}
	Ok(None)
}

/// Refuses a library that a `.nib` file named, unless the library keeps a tree in that very
/// folder on this device: a `.nib` file from elsewhere, such as in an archive, must not point
/// commands at the person's library. A file in `starting`, the folder that `nib init` makes
/// a tree, counts all the same, as init writes it anew.
///
/// # Errors
///
/// Returns [`PointerError::Stray`] if it keeps none there, and an error if a space cannot be
/// read.
pub fn check(
	library: &Library,
	found: &Found,
	starting: Option<&Path>,
) -> Result<(), PointerError> {
	let Some(file) = &found.named_in else {
		return Ok(());
	};
	let folder = resolved_as_far_as_it_exists(file.parent().unwrap_or(file).to_owned());
	let started_here = starting
		.is_some_and(|starting| resolved_as_far_as_it_exists(starting.to_owned()) == folder);
	if started_here || tree_rooted_at(library, &folder)?.is_some() {
		return Ok(());
	}
	Err(PointerError::Stray {
		file: file.clone(),
		library: found.folder.clone(),
	})
}

/// Whether commands run in the tree's folder `root` need a `.nib` file there to find the
/// library in `library`: unless the nearest library of the folder and those it is in, below
/// `ceiling`, is that one, in its own folder. Another tree's `.nib` file above it would name
/// another library, or none.
#[must_use]
pub fn needed(root: &Path, library: &Path, ceiling: Option<&Path>) -> bool {
	let ceiling = ceiling.map(|ceiling| resolved_as_far_as_it_exists(ceiling.to_owned()));
	for folder in root
		.ancestors()
		.take_while(|folder| ceiling.as_deref() != Some(*folder))
	{
		let data = folder.join(LIBRARY_DATA);
		if data.is_dir() {
			if Library::exists(&data) {
				return resolved_as_far_as_it_exists(folder.to_owned()) != library;
			}
		} else if folder != root && data.is_file() {
			return true;
		}
	}
	true
}

/// Whether the tree's folder `root` holds a `.nib` file that names the library in `library`.
#[must_use]
pub fn names(root: &Path, library: &Path) -> bool {
	named(&root.join(LIBRARY_DATA), None).is_ok_and(|named| named == library)
}

/// Refuses where something other than a file is in the place of the tree's `.nib` file,
/// which writing must neither follow nor replace.
///
/// # Errors
///
/// Returns [`PointerError::InTheWay`] if something else is there.
pub fn check_place(root: &Path) -> Result<(), PointerError> {
	let file = root.join(LIBRARY_DATA);
	match fs::symlink_metadata(&file) {
		Ok(metadata) if !metadata.is_file() => Err(PointerError::InTheWay(file)),
		_ => Ok(()),
	}
}

/// Writes into the tree's folder `root` the `.nib` file that names the library in `library`,
/// over one that is there, and saves it, and the folder where the system allows.
///
/// # Errors
///
/// Returns [`PointerError::InTheWay`] if something other than a file is in its place, and
/// [`PointerError::NotWritten`] if writing fails.
pub fn write(root: &Path, library: &Path) -> Result<PathBuf, PointerError> {
	check_place(root)?;
	let file = root.join(LIBRARY_DATA);
	let text = format!("{NAMES}{}\n", library.display());
	fs::File::create(&file)
		.and_then(|mut opened| {
			opened.write_all(text.as_bytes())?;
			opened.sync_all()
		})
		.map_err(|error| PointerError::NotWritten {
			file: file.clone(),
			error,
		})?;
	// Windows opens no folder as a file, and saves its entries with the file.
	let _ = fs::File::open(root).and_then(|folder| folder.sync_all());
	Ok(file)
}

/// What [`settle`] did in a tree's folder.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled {
	/// It wrote the file that names the library, over one that was there.
	Written(PathBuf),
	/// It took away a file that the commands there do not need, and that would take them over.
	TakenAway(PathBuf),
	/// Nothing: no file is needed, and none was there.
	Nothing,
}

/// Settles the `.nib` file in the tree's folder `root`, as commands there need it to find the
/// library in `library`: writes the file that names it where they would not find it
/// otherwise, as [`needed`] says, and else takes away what they would read as one, such as a
/// file copied with the folder, which would take them over; of a link to a file, the link
/// goes, not what it links to. A folder in its place stays, such as the library's own where
/// the tree's folder is the library's.
///
/// # Errors
///
/// Returns [`PointerError::InTheWay`] if a file is needed and something else is in its place,
/// and [`PointerError::NotWritten`] or [`PointerError::NotTakenAway`] if the file system
/// fails.
pub fn settle(
	root: &Path,
	library: &Path,
	ceiling: Option<&Path>,
) -> Result<Settled, PointerError> {
	if needed(root, library, ceiling) {
		return write(root, library).map(Settled::Written);
	}
	// What `nearest` reads, links followed; deleting a link deletes the link alone.
	let file = root.join(LIBRARY_DATA);
	if !file.is_file() {
		return Ok(Settled::Nothing);
	}
	match fs::remove_file(&file) {
		Ok(()) => {}
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settled::Nothing),
		Err(error) => return Err(PointerError::NotTakenAway { file, error }),
	}
	let _ = fs::File::open(root).and_then(|folder| folder.sync_all());
	Ok(Settled::TakenAway(file))
}
