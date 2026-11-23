//! Finding the paths of a tree on the disk: name by name, as each folder lists its names,
//! with neither links nor files on the way followed. A path in a tree is in composed form,
//! while a name on the disk may be decomposed, as a Mac writes it, and a file system may
//! compare names without regard to case, so each name is matched as its folder lists it,
//! exactly in composed form ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use unicode_normalization::UnicodeNormalization;

use super::paths::is_safe_tree_path;

/// Where a path of a tree is on the disk, as far as the disk tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Located {
	/// There, at this path on the disk.
	Here(PathBuf),
	/// Not there: a name on the way is not in its folder, or is a file or a link where a
	/// folder would be, which is never gone through.
	Absent,
	/// The disk does not tell, such as when a folder on the way cannot be read: the path
	/// is not taken as gone.
	Unknown(String),
}

/// The names that folders hold, each folder listed once, by its names in composed form:
/// for looking up many paths at once.
#[derive(Debug, Default)]
pub(crate) struct Listings {
	folders: RefCell<BTreeMap<PathBuf, Result<Rc<Names>, String>>>,
	/// Folders that no path of a tree reaches, such as the library's own, whatever its
	/// name, as full paths.
	apart: Vec<PathBuf>,
}

/// The names in a folder, by their composed form; a name that two hold is `None`.
type Names = BTreeMap<String, Option<OsString>>;

impl Listings {
	/// Listings that keep paths out of the folders `apart`.
	pub(crate) fn apart(apart: &[PathBuf]) -> Self {
		Self {
			folders: RefCell::default(),
			apart: apart.to_vec(),
		}
	}

	/// The names in `folder`, listed now or before.
	fn of(&self, folder: &Path) -> Result<Rc<Names>, String> {
		if let Some(listed) = self.folders.borrow().get(folder) {
			return listed.clone();
		}
		let listed = list(folder).map(Rc::new);
		self.folders
			.borrow_mut()
			.insert(folder.to_path_buf(), listed.clone());
		listed
	}
}

/// The names in a folder, by their composed form.
fn list(folder: &Path) -> Result<Names, String> {
	let mut names = Names::new();
	for entry in fs::read_dir(folder).map_err(|error| error.to_string())? {
		let on_disk = entry.map_err(|error| error.to_string())?.file_name();
		let Some(text) = on_disk.to_str() else {
			continue;
		};
		names
			.entry(text.nfc().collect())
			.and_modify(|twice| *twice = None)
			.or_insert(Some(on_disk));
	}
	Ok(names)
}

/// The name in `folder` that is `name` in composed form, exactly; `Err` if the folder cannot
/// be read, or holds two such names.
fn listed(listings: &Listings, folder: &Path, name: &str) -> Result<Option<OsString>, String> {
	match listings.of(folder)?.get(name) {
		None => Ok(None),
		Some(Some(on_disk)) => Ok(Some(on_disk.clone())),
		Some(None) => Err("two names of it are the same in composed form".to_owned()),
	}
}

/// Where the path `path` of the tree at `root` is on the disk, kept out of the folders
/// `apart`.
pub(crate) fn located(root: &Path, path: &str, apart: &[PathBuf]) -> Located {
	located_in(&Listings::apart(apart), root, path)
}

/// Where the path `path` of the tree at `root` is on the disk, with the folders listed in
/// `listings`, as the disk was when they were first listed.
pub(crate) fn located_in(listings: &Listings, root: &Path, path: &str) -> Located {
	if !is_safe_tree_path(path) {
		return Located::Absent;
	}
	// The inside of a Git repository is Git's: a tree neither writes nor deletes there.
	if holds_git(listings, root) {
		return Located::Unknown("the tree's folder holds a Git repository".to_owned());
	}
	let names: Vec<&str> = path.split('/').collect();
	let mut at = root.to_path_buf();
	for (position, name) in names.iter().enumerate() {
		let on_disk = match listed(listings, &at, name) {
			Ok(Some(on_disk)) => on_disk,
			Ok(None) => return Located::Absent,
			Err(why) => return Located::Unknown(why),
		};
		at.push(on_disk);
		if listings.apart.iter().any(|apart| at.starts_with(apart)) {
			return Located::Unknown("it is in the library's own folder".to_owned());
		}
		if position + 1 < names.len() {
			match fs::symlink_metadata(&at) {
				Ok(metadata) if metadata.is_dir() => {}
				Ok(_) => return Located::Absent,
				Err(error) if error.kind() == io::ErrorKind::NotFound => return Located::Absent,
				Err(error) => return Located::Unknown(error.to_string()),
			}
			if holds_git(listings, &at) {
				return Located::Unknown("it is inside a Git repository".to_owned());
			}
		}
	}
	Located::Here(at)
}

/// Whether a folder holds a Git repository, as its listing tells: a `.git`, or what a bare
/// repository holds, `HEAD`, `objects` and `refs`.
pub(crate) fn holds_git(listings: &Listings, folder: &Path) -> bool {
	let has = |name: &str| listed(listings, folder, name).is_ok_and(|found| found.is_some());
	has(".git") || (has("HEAD") && has("objects") && has("refs"))
}

/// Why a new file or folder cannot go at a path of a tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocked {
	/// Something is there, or a file is where a folder on the way would be.
	Taken,
	/// The way goes through a link, a folder on it cannot be read, or the path is no path a
	/// tree may hold.
	Unsafe,
}

/// Where a new file or folder at the path `path` of the tree at `root` goes on the disk:
/// the folders on the way that are there, by their names on the disk, each a folder and no
/// link, and the rest as the path names them; never in the folders `apart`.
pub(crate) fn place_for_new(
	root: &Path,
	path: &str,
	apart: &[PathBuf],
) -> Result<PathBuf, Blocked> {
	if !is_safe_tree_path(path) {
		return Err(Blocked::Unsafe);
	}
	let listings = Listings::apart(apart);
	if holds_git(&listings, root) {
		return Err(Blocked::Unsafe);
	}
	let names: Vec<&str> = path.split('/').collect();
	let (last, folders) = names.split_last().ok_or(Blocked::Unsafe)?;
	let mut at = root.to_path_buf();
	let mut made = false;
	for name in folders {
		if made {
			at.push(name);
			continue;
		}
		if let Some(on_disk) = listed(&listings, &at, name).map_err(|_| Blocked::Unsafe)? {
			at.push(on_disk);
			let metadata = fs::symlink_metadata(&at).map_err(|_| Blocked::Unsafe)?;
			if metadata.file_type().is_symlink() {
				return Err(Blocked::Unsafe);
			}
			if !metadata.is_dir() {
				return Err(Blocked::Taken);
			}
			// The inside of a Git repository is Git's: a tree records it as a reference,
			// and writes nothing there.
			if holds_git(&listings, &at) {
				return Err(Blocked::Unsafe);
			}
		} else {
			at.push(name);
			// What a file system that ignores case finds by another spelling is in the way.
			if fs::symlink_metadata(&at).is_ok() {
				return Err(Blocked::Taken);
			}
			made = true;
		}
	}
	if !made
		&& listed(&listings, &at, last)
			.map_err(|_| Blocked::Unsafe)?
			.is_some()
	{
		return Err(Blocked::Taken);
	}
	at.push(last);
	if apart.iter().any(|apart| at.starts_with(apart)) {
		return Err(Blocked::Unsafe);
	}
	if fs::symlink_metadata(&at).is_ok() {
		return Err(Blocked::Taken);
	}
	Ok(at)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn folder(name: &str) -> PathBuf {
		let name = format!("nibkeep-node-{name}-{}", std::process::id());
		nibkeep_testkit::empty_directory(std::env::temp_dir(), &name)
	}

	#[test]
	fn a_decomposed_name_on_the_disk_is_found_by_its_composed_path() {
		let root = folder("locate decomposed");
		fs::create_dir(root.join("cafe\u{301}")).expect("makes a folder");
		fs::write(root.join("cafe\u{301}/menu.txt"), "").expect("writes");
		assert_eq!(
			located(&root, "caf\u{e9}/menu.txt", &[]),
			Located::Here(root.join("cafe\u{301}/menu.txt"))
		);
		assert_eq!(located(&root, "caf\u{e9}/other.txt", &[]), Located::Absent);
		assert_eq!(
			place_for_new(&root, "caf\u{e9}/new/x.txt", &[]),
			Ok(root.join("cafe\u{301}/new/x.txt"))
		);
	}

	#[test]
	fn names_match_exactly_and_nothing_is_gone_through() {
		let root = folder("locate exactly");
		fs::create_dir(root.join("Docs")).expect("makes a folder");
		fs::write(root.join("Docs/a.txt"), "").expect("writes");
		fs::write(root.join("file"), "").expect("writes");
		assert_eq!(located(&root, "docs/a.txt", &[]), Located::Absent);
		assert_eq!(located(&root, "file/a.txt", &[]), Located::Absent);
		assert_eq!(place_for_new(&root, "file/a.txt", &[]), Err(Blocked::Taken));
		assert_eq!(place_for_new(&root, "Docs/a.txt", &[]), Err(Blocked::Taken));
		#[cfg(unix)]
		{
			let outside = folder("locate exactly, outside");
			std::os::unix::fs::symlink(&outside, root.join("link")).expect("links");
			assert_eq!(located(&root, "link/a.txt", &[]), Located::Absent);
			assert_eq!(
				place_for_new(&root, "link/a.txt", &[]),
				Err(Blocked::Unsafe)
			);
			assert_eq!(
				located(&root, "link", &[]),
				Located::Here(root.join("link"))
			);
		}
	}
}
