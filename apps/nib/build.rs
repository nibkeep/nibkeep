//! Names the commit that `nib` is built from, for `nib --version`: the one that Git has
//! checked out here, with `+changes` if what goes into the build differs from it; or else
//! the one that `git archive` wrote into the file `commit`; or else none. Either way it is
//! the first 12 digits of the commit's hash, so that a build of one commit names it the same
//! way each time, and builds stay reproducible.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

/// What goes into the build besides the crates' sources and manifests: Cargo's settings and
/// the toolchain's.
const SETTINGS: [&str; 4] = [
	"Cargo.lock",
	".cargo",
	"rust-toolchain",
	"rust-toolchain.toml",
];

fn main() {
	let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
	let workspace = package.join("..").join("..");
	let commit = from_git(&workspace)
		.or_else(|| from_archive(&package.join("commit")))
		.unwrap_or_else(|| "unknown commit".to_owned());
	let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
	println!("cargo:rustc-env=NIB_VERSION={version} ({commit})");
	// Built again when the commit changes, or what `+changes` looks at. A file that does not
	// exist would have Cargo build again every time, so only those that do are named.
	let mut watched = vec![package.join("commit")];
	watched.extend(sources(&workspace));
	watched.extend(SETTINGS.iter().map(|path| workspace.join(path)));
	for path in ["HEAD", "packed-refs"] {
		watched.extend(git(&workspace, &["rev-parse", "--git-path", path]).map(PathBuf::from));
	}
	if let Some(branch) = git(&workspace, &["symbolic-ref", "-q", "HEAD"]) {
		watched.extend(git(&workspace, &["rev-parse", "--git-path", &branch]).map(PathBuf::from));
	}
	for path in watched {
		let path = if path.is_relative() {
			workspace.join(path)
		} else {
			path
		};
		if path.exists() {
			println!("cargo:rerun-if-changed={}", path.display());
		}
	}
}

/// The sources and manifests of the workspace's crates: what `+changes` looks at, beside
/// [`SETTINGS`]. Their tests, and files that Git does not track, are left out.
fn sources(workspace: &Path) -> Vec<PathBuf> {
	let mut sources = vec![workspace.join("Cargo.toml")];
	for folder in ["crates", "apps"] {
		let Ok(crates) = fs::read_dir(workspace.join(folder)) else {
			continue;
		};
		for crate_folder in crates.flatten() {
			let path = crate_folder.path();
			sources.extend(
				["src", "Cargo.toml", "build.rs"]
					.into_iter()
					.map(|part| path.join(part)),
			);
		}
	}
	sources
}

/// The commit that Git has checked out in the workspace, with `+changes` if what goes into
/// the build differs from it, if the workspace is a repository of its own and not a folder
/// in another's.
fn from_git(workspace: &Path) -> Option<String> {
	let top = git(workspace, &["rev-parse", "--show-toplevel"])?;
	if fs::canonicalize(top).ok()? != fs::canonicalize(workspace).ok()? {
		return None;
	}
	let hash = short(&git(workspace, &["rev-parse", "HEAD"])?)?;
	let mut status = vec![
		"status".to_owned(),
		"--porcelain".to_owned(),
		"--untracked-files=no".to_owned(),
		"--".to_owned(),
	];
	for path in sources(workspace).iter().chain(
		SETTINGS
			.iter()
			.map(|path| workspace.join(path))
			.collect::<Vec<_>>()
			.iter(),
	) {
		if let Ok(relative) = path.strip_prefix(workspace) {
			status.push(relative.to_string_lossy().replace('\\', "/"));
		}
	}
	let status: Vec<&str> = status.iter().map(String::as_str).collect();
	let changes = git(workspace, &status)?;
	Some(if changes.is_empty() {
		hash
	} else {
		format!("{hash}+changes")
	})
}

/// The commit whose hash `git archive` wrote into the file, which in a repository holds the
/// placeholder that Git replaces.
fn from_archive(file: &Path) -> Option<String> {
	short(fs::read_to_string(file).ok()?.trim())
}

/// The first 12 digits of a commit's full hash, of SHA-1 or SHA-256, if it is one.
fn short(hash: &str) -> Option<String> {
	let full = (hash.len() == 40 || hash.len() == 64)
		&& hash
			.chars()
			.all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit));
	full.then(|| hash[..12].to_owned())
}

/// What Git prints for these arguments in the workspace, trimmed, if it runs and succeeds.
/// It takes no lock that it can do without, so that a build never keeps a commit waiting.
fn git(workspace: &Path, arguments: &[&str]) -> Option<String> {
	let output = Command::new("git")
		.args(arguments)
		.current_dir(workspace)
		.env("GIT_OPTIONAL_LOCKS", "0")
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}
