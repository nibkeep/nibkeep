//! The paths of a tree's files: relative to the tree's folder, with `/` between folders, in
//! Unicode's composed form (NFC); never leaving the folder, and never naming the library's
//! own folder or Git's, by any spelling that Windows or macOS read as one of them
//! ([technical design: trees]). Git's own checks came to the same after folder names were used
//! against it.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::path::{Component, Path};

use unicode_normalization::{UnicodeNormalization, is_nfc};

/// How long a path in a tree may be, in bytes: longer than any system's limit.
const MAX_PATH: usize = 4_096;

/// Whether a path is one that a tree may hold, and so one that writing a tree may write:
/// relative, with `/` between its folders, in composed form, at most 4,096 bytes, with no
/// empty name, no `.` or `..`, no backslash, which Windows reads as a separator, no zero
/// byte, and no name that any system reads as `.git` or `.nib`, the folders of Git and of
/// the library.
#[must_use]
pub fn is_safe_tree_path(path: &str) -> bool {
	!path.is_empty() && path.len() <= MAX_PATH && is_nfc(path) && path.split('/').all(is_safe_name)
}

/// Whether a name of a file or a folder may be in a tree's path, as [`is_safe_tree_path`]
/// says.
pub(crate) fn is_safe_name(name: &str) -> bool {
	!name.is_empty()
		&& name != "."
		&& name != ".."
		&& !name.contains(['/', '\\', '\0'])
		&& !is_reserved(name)
}

/// Whether a name is one that some system reads as `.git` or `.nib`: compared without regard
/// to case, as Windows' and macOS's file systems compare names, with what Windows drops from
/// a name's end, dots and spaces, dropped, with what macOS ignores in names dropped, with
/// what follows a colon dropped, which on Windows names a stream of the file, and with
/// Windows' short names of them, as `GIT~1`.
pub(crate) fn is_reserved(name: &str) -> bool {
	let before_stream = name.split(':').next().unwrap_or_default();
	let seen: String = before_stream
		.chars()
		.filter(|character| !is_ignored_by_macos(*character))
		.collect();
	let seen = seen.trim_end_matches(['.', ' ']).to_uppercase();
	if seen == ".GIT" || seen == ".NIB" {
		return true;
	}
	// A short name, as Windows makes for names that do not fit 8.3: GIT~1, NIB~2.
	["GIT~", "NIB~"].iter().any(|start| {
		seen.strip_prefix(start)
			.is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
	})
}

/// Whether macOS's file systems ignore a character in names, as HFS+ did: the joiners and
/// the marks of direction, which Git ignores too when it looks for `.git`.
fn is_ignored_by_macos(character: char) -> bool {
	matches!(
		character,
		'\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
	)
}

/// Whether a name can be written on this system: on Windows, without the characters it
/// forbids, such as a colon, without a dot or a space at its end, and not a device's name,
/// such as `CON` or `NUL.txt`; elsewhere, any name a tree may hold.
pub(crate) fn fits_this_system(name: &str) -> bool {
	if cfg!(windows) {
		let device = name
			.split('.')
			.next()
			.unwrap_or_default()
			.trim_end()
			.to_ascii_uppercase();
		let numbered = |start: &str| {
			device.strip_prefix(start).is_some_and(|rest| {
				matches!(
					rest,
					"1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
				)
			})
		};
		!name.contains(['<', '>', ':', '"', '|', '?', '*'])
			&& !name.chars().any(char::is_control)
			&& !name.ends_with(['.', ' '])
			&& !matches!(
				device.as_str(),
				"CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
			)
			&& !numbered("COM")
			&& !numbered("LPT")
	} else {
		true
	}
}

/// The path in a tree of what is at `relative` in its folder, if its names are Unicode: in
/// composed form, with `/` between folders. `Err` says why it cannot be.
pub(crate) fn tree_path_of(relative: &Path) -> Result<String, &'static str> {
	let mut names = Vec::new();
	for component in relative.components() {
		let Component::Normal(name) = component else {
			return Err("it is not in the tree's folder");
		};
		let name = name.to_str().ok_or("its name is not Unicode")?;
		names.push(name.nfc().collect::<String>());
	}
	let path = names.join("/");
	if path.is_empty() {
		return Err("it is the tree's folder");
	}
	if path.split('/').any(|name| name.contains('\\')) {
		return Err("its name holds a backslash, which Windows reads as between folders");
	}
	if !is_safe_tree_path(&path) {
		return Err("its path cannot be in a tree");
	}
	Ok(path)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn names_of_git_and_the_library_are_reserved_however_spelled() {
		for reserved in [
			".git",
			".GIT",
			".gIt",
			".git.",
			".git...",
			".git ",
			".git. .",
			".git:stream",
			".git::$INDEX_ALLOCATION",
			"\u{200c}.git",
			".g\u{200d}it",
			".git\u{feff}",
			"git~1",
			"GIT~12",
			".g\u{131}t",
			".nib",
			".NIB",
			"nib~1",
		] {
			assert!(is_reserved(reserved), "{reserved:?}");
		}
		for allowed in [
			".gitignore",
			".github",
			".gitmodules",
			"git",
			".git-crypt",
			"git~",
			"git~a",
			".nibignore",
			"nib",
			"a.git",
		] {
			assert!(!is_reserved(allowed), "{allowed:?}");
		}
	}

	#[test]
	fn a_path_on_disk_becomes_a_tree_path_in_composed_form() {
		assert_eq!(
			tree_path_of(Path::new("docs/cafe\u{301}.md")),
			Ok("docs/caf\u{e9}.md".to_owned())
		);
		assert!(tree_path_of(Path::new("../a")).is_err());
		assert!(tree_path_of(Path::new("")).is_err());
		assert!(tree_path_of(Path::new("x/.git/config")).is_err());
		#[cfg(unix)]
		{
			use std::os::unix::ffi::OsStrExt;
			let not_unicode = std::ffi::OsStr::from_bytes(b"a\xff.txt");
			assert_eq!(
				tree_path_of(Path::new(not_unicode)),
				Err("its name is not Unicode")
			);
			assert!(tree_path_of(Path::new("a\\b.txt")).is_err());
		}
	}

	#[test]
	fn windows_takes_only_names_it_can_write() {
		let names = [
			"CON", "nul.txt", "com1", "LPT9.log", "a:b", "a?", "end.", "end ",
		];
		for name in names {
			assert_eq!(fits_this_system(name), !cfg!(windows), "{name}");
		}
		for name in ["CONSOLE", "com0", "notes.md", "lpt"] {
			assert!(fits_this_system(name), "{name}");
		}
	}
}
