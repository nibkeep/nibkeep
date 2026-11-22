//! Which new files of a tree are secret: those that a built-in name matches, which no tree
//! turns off, and those that a line of a `.nibsecret` file matches, in Git's syntax for
//! `.gitignore`, read from this device's copies ([technical design: secrets in trees]). What
//! a file is stays as its first commit decided.
//!
//! [technical design: secrets in trees]: ../../../../../docs/Technical-design.md#151-secrets-in-trees

use std::collections::BTreeMap;
use std::io::Read;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::walk::{Found, FoundKind, open_found};

/// The name of the files that say which other files are secret.
pub(crate) const NIBSECRET: &str = ".nibsecret";

/// How much of a `.nibsecret` file is read: more is no list of names.
const READ_LIMIT: u64 = 64 * 1024;

/// The names that are secret in every tree, in Git's syntax for `.gitignore`.
const BUILT_IN: [&str; 17] = [
	".env",
	".env.*",
	"!.env.example",
	"!.env.sample",
	"!.env.template",
	".envrc",
	"*.pem",
	"*.key",
	"*.p12",
	"*.pfx",
	"id_rsa",
	"id_dsa",
	"id_ecdsa",
	"id_ed25519",
	"id_ecdsa_sk",
	"id_ed25519_sk",
	".netrc",
];

/// What makes a path of a tree secret: the built-in names, and the `.nibsecret` files of a
/// copy, by the folder each is in, the deepest first.
#[derive(Clone, Debug)]
pub(crate) struct Rules {
	built_in: Gitignore,
	files: Vec<(String, Gitignore)>,
}

impl Default for Rules {
	fn default() -> Self {
		Self::built_in()
	}
}

impl Rules {
	/// The built-in names alone, as where the tree has no copy.
	pub(crate) fn built_in() -> Self {
		Self {
			built_in: matcher(BUILT_IN.iter().copied()),
			files: Vec::new(),
		}
	}

	/// The built-in names and the `.nibsecret` files among what a copy holds, by path in the
	/// tree. A file that cannot be read says nothing.
	pub(crate) fn of_copy(found: &BTreeMap<String, Found>) -> Self {
		let mut files = Vec::new();
		for (path, at) in found {
			let folder = match path.rsplit_once('/') {
				Some((folder, NIBSECRET)) => folder.to_owned(),
				None if path == NIBSECRET => String::new(),
				_ => continue,
			};
			if at.kind != FoundKind::File {
				continue;
			}
			let Some(text) = read_text(at) else {
				continue;
			};
			files.push((folder, matcher(text.lines())));
		}
		// The deepest first, as a folder's file speaks before those above it.
		files.sort_by_key(|(folder, _)| std::cmp::Reverse(depth(folder)));
		Self {
			built_in: matcher(BUILT_IN.iter().copied()),
			files,
		}
	}

	/// Whether a built-in name matches a path of the tree.
	pub(crate) fn is_built_in(&self, path: &str) -> bool {
		self.built_in
			.matched_path_or_any_parents(path, false)
			.is_ignore()
	}

	/// Whether a path of the tree is secret: a built-in name matches it, or else the
	/// `.nibsecret` file nearest it that speaks of it, as Git reads `.gitignore` files.
	pub(crate) fn is_secret(&self, path: &str) -> bool {
		if self.is_built_in(path) {
			return true;
		}
		for (folder, rules) in &self.files {
			let relative = if folder.is_empty() {
				path
			} else if let Some(rest) = path.strip_prefix(folder.as_str())
				&& let Some(rest) = rest.strip_prefix('/')
			{
				rest
			} else {
				continue;
			};
			match rules.matched_path_or_any_parents(relative, false) {
				Match::Ignore(_) => return true,
				Match::Whitelist(_) => return false,
				Match::None => {}
			}
		}
		false
	}
}

/// The meaningful lines of a `.nibsecret` file: neither empty nor comments.
pub(crate) fn lines(text: &str) -> std::collections::BTreeSet<&str> {
	text.lines()
		.map(str::trim_end)
		.filter(|line| !line.is_empty() && !line.starts_with('#'))
		.collect()
}

/// A matcher of the lines given, relative to the folder they are for.
fn matcher<'a>(lines: impl Iterator<Item = &'a str>) -> Gitignore {
	let mut builder = GitignoreBuilder::new("");
	for line in lines {
		// A line that is no pattern says nothing, as Git skips it.
		let _ = builder.add_line(None, line);
	}
	builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// How many folders deep a folder of the tree is.
fn depth(folder: &str) -> usize {
	if folder.is_empty() {
		0
	} else {
		folder.split('/').count()
	}
}

/// A small file's text, read as it was found, never through a link.
fn read_text(at: &Found) -> Option<String> {
	let mut bytes = Vec::new();
	let file = open_found(at).ok()?;
	file.take(READ_LIMIT + 1).read_to_end(&mut bytes).ok()?;
	if bytes.len() as u64 > READ_LIMIT {
		return None;
	}
	String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
	use super::Rules;

	#[test]
	fn the_built_in_names_are_secret_wherever_they_are_but_examples_of_them() {
		let rules = Rules::built_in();
		for secret in [
			".env",
			"app/.env.local",
			".envrc",
			"deploy/id_ed25519",
			"certs/server.pem",
			"tls.key",
			"store.p12",
			".netrc",
		] {
			assert!(rules.is_secret(secret), "{secret}");
		}
		for ordinary in [
			".env.example",
			"app/.env.sample",
			".env.template",
			"id_rsa.pub",
			"notes.md",
			"environment.md",
			"key.txt",
		] {
			assert!(!rules.is_secret(ordinary), "{ordinary}");
		}
	}
}
