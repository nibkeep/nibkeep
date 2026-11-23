//! Reading a tree's folder: what its commits take, with what `.nibignore` files name left
//! out, and always the library's folder, every `.git`, the inside of a folder that holds a
//! Git repository, and the copies that NibKeep wrote there for other reasons
//! ([technical design: trees]). Links are never followed.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::WalkBuilder;
use nibkeep_base::{Hash, Hasher};

use super::paths::{is_reserved, tree_path_of};

/// The name of the files that say what commits leave out.
pub(crate) const NIBIGNORE: &str = ".nibignore";

/// What is at a path of a tree's folder.
#[derive(Clone, Debug)]
pub(crate) struct Found {
	/// Where it is on the disk.
	pub(crate) on_disk: PathBuf,
	/// A file, or a link and its target.
	pub(crate) kind: FoundKind,
	/// Its size in bytes; of a link, 0.
	pub(crate) size: u64,
	/// When it was last changed, as the file system says, in nanoseconds since 1970.
	pub(crate) modified: Option<i64>,
	/// Whether it is executable, where the system says.
	pub(crate) executable: Option<bool>,
	/// Which file it is, where the system says: its device and inode on Unix, by which what
	/// is opened later is checked to be it, and not what a link put in its place.
	pub(crate) identity: Option<(u64, u64)>,
}

/// A file or a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FoundKind {
	/// A file.
	File,
	/// A symbolic link, with its target.
	Link(String),
}

/// What a tree's folder holds that its commits may take.
#[derive(Debug, Default)]
pub(crate) struct Walked {
	/// What it holds, by path in the tree.
	pub(crate) found: BTreeMap<String, Found>,
	/// What it holds that a commit cannot take, with why.
	pub(crate) skipped: Vec<(String, String)>,
	/// The folders that hold Git repositories, by path in the tree.
	pub(crate) repositories: Vec<String>,
}

/// Reads the folder of a tree at `root`: what its commits may take, left out what the
/// `.nibignore` files name and what is always left out, the paths `left_out`, which NibKeep
/// wrote there for other reasons, and the folders `apart`, full paths, such as the library's
/// own, whatever its name.
pub(crate) fn walk(root: &Path, left_out: &BTreeSet<String>, apart: &[PathBuf]) -> Walked {
	let filter_root = root.to_path_buf();
	let filter_left_out = Arc::new(left_out.clone());
	let filter_apart = apart.to_vec();
	let repositories = Arc::new(std::sync::Mutex::new(Vec::new()));
	let filter_repositories = Arc::clone(&repositories);
	let entries = WalkBuilder::new(root)
		// The `.nibignore` files alone: none of Git's, nor the crate's own `.ignore`, nor
		// the ignore files of folders above the tree.
		.standard_filters(false)
		.hidden(false)
		.parents(false)
		.ignore(false)
		.git_ignore(false)
		.git_global(false)
		.git_exclude(false)
		.require_git(false)
		.follow_links(false)
		.add_custom_ignore_filename(NIBIGNORE)
		.filter_entry(move |entry| {
			if entry.depth() == 0 {
				return true;
			}
			let name = entry.file_name().to_string_lossy();
			if is_reserved(&name) || is_temporary(&name) {
				return false;
			}
			if filter_apart.iter().any(|apart| entry.path() == apart) {
				return false;
			}
			let left_out = entry
				.path()
				.strip_prefix(&filter_root)
				.ok()
				.and_then(|relative| tree_path_of(relative).ok())
				.is_some_and(|path| filter_left_out.contains(&path));
			if left_out {
				return false;
			}
			// A folder that holds a Git repository, in a `.git` folder or file, is recorded
			// as a reference, not file by file.
			let is_folder = entry.file_type().is_some_and(|kind| kind.is_dir());
			if is_folder && fs::symlink_metadata(entry.path().join(".git")).is_ok() {
				if let Ok(mut found) = filter_repositories.lock() {
					found.push(entry.path().to_path_buf());
				}
				return false;
			}
			// A bare repository is Git's own folder: neither its files nor a reference.
			!(is_folder && is_bare(entry.path()))
		})
		.build();
	let mut walked = Walked::default();
	for entry in entries {
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => {
				let path = error_path(&error)
					.and_then(|path| path.strip_prefix(root).ok())
					.map_or_else(String::new, |path| path.to_string_lossy().into_owned());
				walked.skipped.push((path, error.to_string()));
				continue;
			}
		};
		if entry.depth() == 0 {
			continue;
		}
		let Some(kind) = entry.file_type() else {
			continue;
		};
		if kind.is_dir() {
			continue;
		}
		let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
		let path = match tree_path_of(relative) {
			Ok(path) => path,
			Err(why) => {
				let shown = relative.to_string_lossy().into_owned();
				walked.skipped.push((shown, why.to_owned()));
				continue;
			}
		};
		match (look(entry.path()), walked.found.entry(path)) {
			(Ok(found), btree_map::Entry::Vacant(vacant)) => {
				vacant.insert(found);
			}
			(Ok(_), btree_map::Entry::Occupied(occupied)) => {
				let why = "another name of it is the same in composed form".to_owned();
				walked.skipped.push((occupied.key().clone(), why));
			}
			(Err(why), entry) => walked.skipped.push((entry.key().clone(), why)),
		}
	}
	let found = repositories
		.lock()
		.map_or_default(|mut found| std::mem::take(&mut *found));
	walked.repositories = found
		.iter()
		.filter_map(|folder| folder.strip_prefix(root).ok())
		.filter_map(|relative| tree_path_of(relative).ok())
		.collect();
	walked.repositories.sort();
	walked
}

/// Whether a folder is a bare Git repository: it holds `HEAD`, `objects` and `refs`.
pub(crate) fn is_bare(folder: &Path) -> bool {
	fs::symlink_metadata(folder.join("HEAD")).is_ok_and(|metadata| metadata.is_file())
		&& fs::symlink_metadata(folder.join("objects")).is_ok_and(|metadata| metadata.is_dir())
		&& fs::symlink_metadata(folder.join("refs")).is_ok_and(|metadata| metadata.is_dir())
}

/// Whether a name is that of what NibKeep writes beside a copy before it puts it in place,
/// as `.nib-tree-` or `.nib-checkout-` and sixteen hexadecimal digits, which a crash may
/// leave behind.
fn is_temporary(name: &str) -> bool {
	[".nib-tree-", ".nib-checkout-"].iter().any(|start| {
		name.strip_prefix(start).is_some_and(|digest| {
			digest.len() == 16 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
		})
	})
}

/// What is at `on_disk`, without following a link: a file or a link, with its size, time
/// and executable bit, or why it is neither.
pub(crate) fn look(on_disk: &Path) -> Result<Found, String> {
	let metadata = fs::symlink_metadata(on_disk).map_err(|error| error.to_string())?;
	let kind = if metadata.file_type().is_symlink() {
		let target = fs::read_link(on_disk).map_err(|error| error.to_string())?;
		let target = target
			.to_str()
			.ok_or_else(|| "its target is not Unicode".to_owned())?;
		// Windows writes targets with backslashes.
		let target = if cfg!(windows) {
			target.replace('\\', "/")
		} else {
			target.to_owned()
		};
		FoundKind::Link(target)
	} else if metadata.is_file() {
		FoundKind::File
	} else {
		return Err("it is not a file, a folder or a link".to_owned());
	};
	Ok(Found {
		on_disk: on_disk.to_path_buf(),
		size: if kind == FoundKind::File {
			metadata.len()
		} else {
			0
		},
		kind,
		modified: metadata.modified().ok().and_then(nanos),
		executable: executable(&metadata),
		identity: identity(&metadata),
	})
}

/// Opens the file that `found` says is there, as it was found: never through a link, and
/// only if it is still the file that was found, so that a link put in its place, or in the
/// place of a folder on its way, is not read through.
pub(crate) fn open_found(found: &Found) -> io::Result<File> {
	let mut options = File::options();
	options.read(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		// A link is refused rather than followed, and a pipe put in the file's place is
		// opened without waiting for a writer, then refused below.
		options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
	}
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		// FILE_FLAG_OPEN_REPARSE_POINT: a link is opened as itself, not followed.
		options.custom_flags(0x0020_0000);
	}
	let file = options.open(&found.on_disk)?;
	let metadata = file.metadata()?;
	if !metadata.is_file() || metadata.file_type().is_symlink() {
		return Err(io::Error::other("it is no longer a file"));
	}
	if let Some(was) = found.identity
		&& identity(&metadata) != Some(was)
	{
		return Err(io::Error::other("another file took its place"));
	}
	Ok(file)
}

/// The hash of the content of the file that `found` says is there, read as
/// [`open_found`] opens it.
pub(crate) fn hash_found(found: &Found) -> io::Result<Hash> {
	let mut file = open_found(found)?;
	let mut hasher = Hasher::new();
	io::copy(&mut file, &mut hasher)?;
	Ok(hasher.finalize())
}

/// Which file a file is, where the system says.
#[cfg_attr(
	unix,
	expect(clippy::unnecessary_wraps, reason = "Windows says nothing")
)]
fn identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		Some((metadata.dev(), metadata.ino()))
	}
	#[cfg(not(unix))]
	{
		let _ = metadata;
		None
	}
}

/// Whether a file is executable, where the system says: by its owner, as Git reads it.
/// Windows has no executable bit.
#[cfg_attr(
	unix,
	expect(clippy::unnecessary_wraps, reason = "Windows says nothing")
)]
fn executable(metadata: &fs::Metadata) -> Option<bool> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		Some(metadata.is_file() && metadata.permissions().mode() & 0o100 != 0)
	}
	#[cfg(not(unix))]
	{
		let _ = metadata;
		None
	}
}

/// A moment as nanoseconds since 1970, if it fits.
pub(crate) fn nanos(at: SystemTime) -> Option<i64> {
	let since = at.duration_since(UNIX_EPOCH).ok()?;
	i64::try_from(since.as_nanos()).ok()
}

/// The path an error of the walk is about, if it says.
fn error_path(error: &ignore::Error) -> Option<&Path> {
	match error {
		ignore::Error::WithPath { path, .. } => Some(path),
		ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
			error_path(err)
		}
		ignore::Error::Partial(errors) => errors.iter().find_map(error_path),
		_ => None,
	}
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;

	#[test]
	fn a_link_put_in_a_file_s_place_after_it_was_found_is_never_read_through() {
		let name = format!("nibkeep-node-walk-swapped-{}", std::process::id());
		let folder = nibkeep_testkit::empty_directory(std::env::temp_dir(), &name);
		let file = folder.join("notes.txt");
		fs::write(&file, "notes").expect("writes");
		fs::write(folder.join("secret"), "secret").expect("writes");
		let found = look(&file).expect("a file");
		assert!(hash_found(&found).is_ok());
		fs::remove_file(&file).expect("deletes");
		std::os::unix::fs::symlink(folder.join("secret"), &file).expect("links");
		assert!(open_found(&found).is_err());
		assert!(hash_found(&found).is_err());
	}

	#[test]
	#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
	#[cfg_attr(
		target_os = "android",
		ignore = "Android refuses named pipes to the shell that runs the tests"
	)]
	fn a_pipe_put_in_a_file_s_place_is_refused_without_waiting() {
		let name = format!("nibkeep-node-walk-pipe-{}", std::process::id());
		let folder = nibkeep_testkit::empty_directory(std::env::temp_dir(), &name);
		let file = folder.join("notes.txt");
		fs::write(&file, "notes").expect("writes");
		let found = look(&file).expect("a file");
		fs::remove_file(&file).expect("deletes");
		let made = std::process::Command::new("mkfifo")
			.arg(&file)
			.status()
			.expect("runs mkfifo");
		assert!(made.success(), "makes a pipe");
		// Opening it does not wait for a writer, and it is refused.
		assert!(open_found(&found).is_err());
	}
}
