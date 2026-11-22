//! The person's own Git, run for the repositories in NibKeep's trees, with their settings and
//! credentials: reading what a repository's remotes, branch and commit are, finding the work
//! that only this computer holds, and cloning what other devices recorded, once the engine
//! has said a clone may take it. The engine knows nothing of Git: `nib` and apps run
//! it through this crate, alike ([technical design: trees]).
//!
//! [technical design: trees]: ../../../docs/Technical-design.md#15-trees

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use nibkeep_node::{GitRepository, ToClone};

/// Clears from a command of Git the variables that would make it work on another repository
/// than the folder it is given, as a hook or an alias of Git that runs `nib` sets them.
pub fn clear_variables(git: &mut Command) {
	for variable in [
		"GIT_DIR",
		"GIT_WORK_TREE",
		"GIT_INDEX_FILE",
		"GIT_OBJECT_DIRECTORY",
		"GIT_ALTERNATE_OBJECT_DIRECTORIES",
		"GIT_COMMON_DIR",
		"GIT_NAMESPACE",
		"GIT_PREFIX",
		"GIT_CEILING_DIRECTORIES",
		"GIT_DISCOVERY_ACROSS_FILESYSTEM",
		"GIT_CONFIG",
		"GIT_CONFIG_PARAMETERS",
		"GIT_CONFIG_COUNT",
		"GIT_SHALLOW_FILE",
		"GIT_GRAFT_FILE",
		"GIT_REPLACE_REF_BASE",
		"GIT_NO_REPLACE_OBJECTS",
		"GIT_IMPLICIT_WORK_TREE",
		"GIT_QUARANTINE_PATH",
		"GIT_INTERNAL_SUPER_PREFIX",
	] {
		git.env_remove(variable);
	}
}

/// The person's Git: its program and the variables it runs with, apart from the
/// environment, so that what reads with it may be handed on.
#[derive(Clone, Debug)]
pub struct Git {
	program: OsString,
	variables: Vec<(OsString, Option<OsString>)>,
}

impl Git {
	/// The `git` that the system finds, with none of the variables that would point it
	/// elsewhere.
	#[must_use]
	pub fn person() -> Self {
		let mut git = Command::new("git");
		clear_variables(&mut git);
		Self::from_template(&git)
	}

	/// The Git of a template: its program, and the variables it sets or takes away, as a
	/// test sets them.
	#[must_use]
	pub fn from_template(template: &Command) -> Self {
		Self {
			program: template.get_program().to_owned(),
			variables: template
				.get_envs()
				.map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
				.collect(),
		}
	}

	/// A command of it, to give arguments to.
	fn command(&self) -> Command {
		let mut command = Command::new(&self.program);
		for (key, value) in &self.variables {
			match value {
				Some(value) => command.env(key, value),
				None => command.env_remove(key),
			};
		}
		command
	}

	/// Runs it in `folder` with `arguments`, reading nothing from the terminal, never
	/// looking for a repository above `folder`, and taking no lock it need not, so that the
	/// person's own Git is never in its way, and returns what it wrote, trimmed, if it
	/// succeeded.
	fn run(&self, folder: &Path, arguments: &[&str]) -> Option<String> {
		let mut git = self.command();
		git.arg("-C").arg(folder).args(arguments);
		if let Some(parent) = folder.parent() {
			git.env("GIT_CEILING_DIRECTORIES", parent);
		}
		let output = git
			.env("GIT_OPTIONAL_LOCKS", "0")
			.env("GIT_TERMINAL_PROMPT", "0")
			.stdin(Stdio::null())
			.stderr(Stdio::null())
			.output()
			.ok()?;
		output
			.status
			.success()
			.then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
	}

	/// What Git says of the repositories at `paths` in the tree at `root`: their remotes,
	/// branch, the remote the branch follows, and commit. One that Git cannot read is left
	/// out, which leaves it as the tree records it.
	#[must_use]
	pub fn describe(&self, root: &Path, paths: &[String]) -> Vec<GitRepository> {
		paths
			.iter()
			.filter_map(|path| self.describe_one(&root.join(path), path))
			.collect()
	}

	/// What Git says of the repository in `folder`, at `path` in the tree: its own, never
	/// one above it, with the remotes of its own settings, each at the address it fetches
	/// from, its first.
	#[must_use]
	pub fn describe_one(&self, folder: &Path, path: &str) -> Option<GitRepository> {
		// A folder that holds no repository of its own is left as it is.
		if !self.holds_own(folder) {
			return None;
		}
		let mut remotes: Vec<(String, String)> = Vec::new();
		let listed = self
			.run(
				folder,
				&["config", "--local", "--get-regexp", r"^remote\..*\.url$"],
			)
			.unwrap_or_default();
		for line in listed.lines() {
			if let Some((key, address)) = line.split_once(' ')
				&& let Some(name) = key
					.strip_prefix("remote.")
					.and_then(|rest| rest.strip_suffix(".url"))
				&& !remotes.iter().any(|(known, _)| known == name)
			{
				remotes.push((name.to_owned(), address.to_owned()));
			}
		}
		let branch = self
			.run(folder, &["symbolic-ref", "--quiet", "--short", "HEAD"])
			.filter(|branch| !branch.is_empty());
		let upstream = branch
			.as_ref()
			.and_then(|branch| {
				self.run(
					folder,
					&["config", "--get", &format!("branch.{branch}.remote")],
				)
			})
			// Git takes an address there too: only a remote's name is the upstream.
			.filter(|remote| remotes.iter().any(|(name, _)| name == remote));
		Some(GitRepository {
			path: path.to_owned(),
			remotes,
			branch,
			upstream,
			commit: self.head(folder),
		})
	}

	/// The work that the repository in `folder` holds and its remotes lack: changes not
	/// committed, commits not pushed, stashes, a pinned commit that it has and no remote
	/// has, or no remote at all. A reference brings back only what a remote holds.
	#[must_use]
	pub fn work_only_here(&self, folder: &Path, pinned: Option<&str>) -> Vec<String> {
		let mut work = Vec::new();
		if !self.holds_own(folder) {
			return work;
		}
		let has_remote = self
			.run(folder, &["remote"])
			.is_some_and(|remotes| !remotes.is_empty());
		if self
			.run(folder, &["status", "--porcelain"])
			.is_some_and(|changes| !changes.is_empty())
		{
			work.push("changes not committed".to_owned());
		}
		if !has_remote {
			work.push("no remote at all".to_owned());
		} else if let Some(count) = self
			.run(
				folder,
				&["rev-list", "--count", "--branches", "--not", "--remotes"],
			)
			.and_then(|count| count.parse::<u64>().ok())
			.filter(|count| *count > 0)
		{
			let commits = if count == 1 { "commit" } else { "commits" };
			work.push(format!("{count} {commits} not pushed"));
		}
		if self
			.run(folder, &["stash", "list"])
			.is_some_and(|stashes| !stashes.is_empty())
		{
			work.push("stashes".to_owned());
		}
		// A pinned commit this clone does not have is no work of this computer's.
		if let Some(pinned) = pinned
			&& has_remote
			&& self
				.run(folder, &["cat-file", "-e", &format!("{pinned}^{{commit}}")])
				.is_some()
			&& self
				.run(folder, &["branch", "--remotes", "--contains", pinned])
				.is_none_or(|branches| branches.is_empty())
		{
			work.push("the pinned commit, which no remote has".to_owned());
		}
		work
	}

	/// The commit that the repository in `folder` has checked out, in full.
	#[must_use]
	pub fn head(&self, folder: &Path) -> Option<String> {
		if !self.holds_own(folder) {
			return None;
		}
		self.run(folder, &["rev-parse", "--verify", "--quiet", "HEAD"])
			.filter(|commit| !commit.is_empty())
	}

	/// Whether `folder` holds a repository of its own: a `.git` folder in it, never a file
	/// that points elsewhere, as a worktree's or a submodule's does, and Git takes `folder`
	/// for the top of its working tree, never a folder above it.
	fn holds_own(&self, folder: &Path) -> bool {
		if !std::fs::symlink_metadata(folder.join(".git")).is_ok_and(|metadata| metadata.is_dir()) {
			return false;
		}
		let Some(top) = self.run(folder, &["rev-parse", "--show-toplevel"]) else {
			return false;
		};
		let same = |path: &Path| std::fs::canonicalize(path).ok();
		same(Path::new(&top)).is_some_and(|top| Some(top) == same(folder))
	}

	/// Clones a repository into `folder`, empty and checked by the engine, as `to` says:
	/// from its remote, with the address after `--` so that Git never reads it as an option,
	/// no submodule, its other remotes added, and its branch checked out, at its pinned
	/// commit if it has one. Only a repository the engine did not refuse comes here. Git
	/// speaks English here, so that what it says can be told apart.
	///
	/// # Errors
	///
	/// Returns what Git said, if a step failed.
	pub fn clone(&self, to: &ToClone, folder: &Path) -> Result<(), String> {
		let mut cloning = self.command();
		cloning
			.arg("clone")
			.arg("--no-recurse-submodules")
			.arg(format!("--origin={}", to.remote));
		if let Some(branch) = &to.branch {
			cloning.arg(format!("--branch={branch}"));
		}
		cloning.arg("--").arg(&to.address).arg(folder);
		checked(cloning)?;
		for (name, address) in &to.others {
			let mut adding = self.command();
			adding
				.arg("-C")
				.arg(folder)
				.args(["remote", "add", "--"])
				.arg(name)
				.arg(address);
			checked(adding)?;
		}
		if let Some(pinned) = &to.pinned {
			let mut resetting = self.command();
			resetting
				.arg("-C")
				.arg(folder)
				.args(["reset", "--hard", "--quiet"])
				.arg(pinned);
			checked(resetting)?;
		}
		Ok(())
	}
}

/// Runs a command of Git, its prompts on the terminal, as for credentials, in English, and
/// returns what it said on failure.
fn checked(mut command: Command) -> Result<(), String> {
	let output = command
		.env("LC_ALL", "C")
		.env_remove("LANGUAGE")
		.stdout(Stdio::null())
		.output()
		.map_err(|error| format!("Git could not run: {error}"))?;
	if output.status.success() {
		Ok(())
	} else {
		let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
		Err(if said.is_empty() {
			format!("Git failed: {}", output.status)
		} else {
			said
		})
	}
}
