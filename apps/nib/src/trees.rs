//! Trees: folders whose files NibKeep keeps with every version, as Git keeps a working tree
//! ([technical design: trees]). Their commands are named as Git, Mercurial, Subversion, Fossil
//! and Jujutsu name theirs, and each works in the tree whose folder on this device holds the
//! current folder, unless it names one; `nib init` in a library starts one.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use nibkeep_base::EntityId;
use nibkeep_git::Git;
use nibkeep_node::{
	BlameLine, Change, Commit, Committed, FileDiff, Library, NodeError, Reverted, ToClone, Tree,
	TreeChange, TreeLogEntry, TreePoint, TreeSecrets, TreeStatus, Updated, Written,
};
use serde_json::json;

use crate::output::{count, moment_text, print_json, short, shown};
use crate::{CliError, Environment, Timings, absolute, current_dir};
use nibkeep_node::pointer::{self, Settled};

/// The commands of trees, named as Git, Mercurial, Subversion, Fossil and Jujutsu name
/// theirs, so that whoever knows one of them knows these: each works in the tree whose
/// folder holds the current folder, unless it names one.
#[derive(Debug, Subcommand)]
pub enum TreeCommand {
	/// Records what changed in the tree's copy here as one revision.
	///
	/// It records what changed, or what changed at the paths given, with a message or
	/// without: new files, new versions, moves, deletions and executable bits; and the Git
	/// repositories in it, as your Git describes them: their remotes, with no credential,
	/// branch, and commit if pinned. New files need no adding first, and a file gone from
	/// one path and found with the same content at another moved. Every version is kept.
	/// What changed elsewhere since the copy was written is refused, unless --force records
	/// it on top. A new file is secret if its name is one such as `.env` or a private
	/// key's, or a `.nibsecret` file names it, in Git's syntax for `.gitignore`: it is
	/// sealed for the secrets, whose passphrase this asks for, and stays secret for good.
	/// With --pin or --unpin, it records only that.
	#[command(visible_alias = "ci")]
	Commit {
		/// The files and folders to commit; without them, everything that changed.
		#[arg(conflicts_with_all = ["pin", "unpin"])]
		paths: Vec<PathBuf>,
		/// Commits everything that changed, as without paths: Git's -a.
		#[arg(short, long, conflicts_with = "paths")]
		all: bool,
		/// The revision's message.
		#[arg(short, long)]
		message: Option<String>,
		/// Records on top what changed elsewhere since the copy was written, which history
		/// keeps, and copies edited from a version this device does not know.
		#[arg(long)]
		force: bool,
		/// Takes in this new file as a secret file, though no name or .nibsecret makes it
		/// one; or makes this committed file secret: a new secret file, and its old versions
		/// purged everywhere once you agree.
		#[arg(long, value_name = "PATH")]
		secret: Vec<PathBuf>,
		/// Takes in this new file as an ordinary file, though it would be secret.
		#[arg(long, value_name = "PATH")]
		ordinary: Vec<PathBuf>,
		/// Commits the rest where a secret file's change waits for the secrets, as on a
		/// device that does not hold them.
		#[arg(long)]
		leave_secrets: bool,
		/// Pins the Git repository in this folder at the commit it has checked out, as a
		/// revision of its own: writing the tree elsewhere then clones it at that commit, and
		/// commits keep it as Git says it is.
		#[arg(
			long,
			value_name = "FOLDER",
			conflicts_with_all = ["all", "message", "force", "secret", "ordinary", "leave_secrets", "unpin"]
		)]
		pin: Option<PathBuf>,
		/// Takes away the pin of the Git repository in this folder, as a revision of its own:
		/// clones then take the branch as its remote has it.
		#[arg(
			long,
			value_name = "FOLDER",
			conflicts_with_all = ["all", "message", "force", "secret", "ordinary", "leave_secrets"]
		)]
		unpin: Option<PathBuf>,
	},
	/// Moves a file or a folder, on the disk and in the tree at once.
	///
	/// A folder moves with everything in it, as a revision of its own: its history follows
	/// it. Nothing is moved onto what is there, out of the tree, or into `.nib` or a
	/// `.git`.
	#[command(visible_alias = "rename")]
	Mv {
		/// Where it is.
		from: PathBuf,
		/// Where it goes.
		to: PathBuf,
	},
	/// Deletes copies of the tree's files here, for the next commit.
	///
	/// It deletes the copies at the paths given, or in the folders given, and the folders
	/// they leave empty: the next commit records them as deleted. A
	/// copy with edits not committed stays, unless --force, and so do files that the tree does
	/// not hold. It records nothing.
	#[command(visible_alias = "remove")]
	Rm {
		/// The files and folders.
		#[arg(required = true)]
		paths: Vec<PathBuf>,
		/// Takes folders, as Git's -r; they are taken without it too.
		#[arg(short, long)]
		recursive: bool,
		/// Deletes copies with edits not committed too, which are then lost.
		#[arg(short, long)]
		force: bool,
	},
	/// Lists the changes to the tree, or to one file, newest first.
	///
	/// The changes to the file at the path given are followed across its moves: when each
	/// was made, by whom, and its revision's message. A change made otherwise, such as with
	/// nib edit, is listed without one.
	Log {
		/// The file or link, as the tree names it now.
		path: Option<PathBuf>,
		/// The tree, by its name or ID, instead of the one here; paths are then the tree's.
		#[arg(long, value_name = "TREE")]
		tree: Option<String>,
	},
	/// Shows how the copy here differs from the tree, or two revisions.
	///
	/// It shows what a commit would record, with the lines of text that changed. With a
	/// revision, how the tree differs from it to the tree now; with two, from the first to
	/// the second, as their authors had the tree. A secret file shows only that it changed,
	/// unless --secrets asks for its lines. What one revision changed, nib show shows.
	Diff {
		/// The revision to compare from.
		#[arg(conflicts_with_all = ["since", "until"])]
		from: Option<String>,
		/// The revision to compare to.
		to: Option<String>,
		/// The revision to compare from, as Mercurial, Fossil and Jujutsu name it.
		#[arg(long = "from", id = "since", value_name = "REVISION")]
		since: Option<String>,
		/// The revision to compare to, with --from.
		#[arg(long = "to", id = "until", value_name = "REVISION", requires = "since")]
		until: Option<String>,
		/// The tree, by its name or ID, instead of the one here.
		#[arg(long, value_name = "TREE")]
		tree: Option<String>,
		/// Shows the lines of secret files too, asking for the secrets' passphrase.
		#[arg(long)]
		secrets: bool,
	},
	/// Names the revision and author that last changed each line.
	///
	/// For each line of the file at the path given, it follows every version the file had,
	/// wherever it was then.
	#[command(visible_alias = "annotate")]
	Blame {
		/// The file, as the tree names it now.
		path: PathBuf,
		/// The tree, by its name or ID, instead of the one here; the path is then the tree's.
		#[arg(long, value_name = "TREE")]
		tree: Option<String>,
		/// Blames a secret file too, asking for the secrets' passphrase.
		#[arg(long)]
		secrets: bool,
	},
	/// Writes a file into the copy here as it was, or as the tree has it.
	///
	/// It writes the file at the path given, or what is in the folder, as it was at a
	/// revision or as of a moment; without either, as the tree has it, a file deleted
	/// from the tree too. A copy with edits not committed is moved beside it first, as
	/// NAME.orig, so that its edits are kept. It records nothing: commit what it wrote once
	/// you checked it.
	Restore {
		/// The file or folder, as the tree named it then.
		path: PathBuf,
		/// The revision whose author's tree to take it from.
		#[arg(short = 'r', long, value_name = "REVISION", conflicts_with = "as_of")]
		at: Option<String>,
		/// The moment as of which the library recorded it: a day, which means its end, or a
		/// moment, such as 2026-10-10T20:00, in local time unless it gives its offset.
		#[arg(long, value_name = "WHEN", value_parser = crate::parse_moment)]
		as_of: Option<crate::Moment>,
	},
	/// Writes into the copy here what undoes a revision.
	///
	/// It writes that only, as Git's revert and Mercurial's backout do: each file it
	/// changed goes back to its version before it, merged three ways with what later
	/// revisions changed; files it moved go back, files it deleted come back, and files it
	/// added go. Where later edits clash, text gets Git's conflict markers, and other files
	/// the version from before beside them. It records nothing: commit what it wrote once
	/// you checked it.
	#[command(visible_alias = "backout")]
	Revert {
		/// The revision, by its ID or the start of it.
		revision: String,
	},
	/// Brings the copy here to the tree as your other devices left it.
	///
	/// It brings what has reached this device: writes new versions, moves, additions and
	/// deletions, and merges your edits with newer versions. Where versions compete, as
	/// when two devices committed one file apart, text is merged three ways, with Git's
	/// conflict markers where edits clash, and anything else stays as it is, with the other
	/// version beside it as NAME.conflict-DEVICE. A file edited here and deleted elsewhere
	/// stays. Nothing is recorded: committing what it wrote settles what competed. A commit
	/// in several parts waits until all its parts have arrived. The Git repositories that
	/// this copy lacks are listed, with where they come from, and cloned with your Git once
	/// you agree. Secret files are written with the secrets unlocked, readable only by you;
	/// a secret file moved elsewhere, and a .nibsecret that lost lines, are written once
	/// you agree. nib pull syncs first.
	#[command(visible_alias = "up")]
	Update {
		/// Writes what parts have arrived of a commit whose other parts have not.
		#[arg(long)]
		partial: bool,
		/// Clones the Git repositories added elsewhere without asking.
		#[arg(long, conflicts_with = "no_clone")]
		clone: bool,
		/// Clones no Git repository, and lists those it would.
		#[arg(long)]
		no_clone: bool,
	},
	/// Writes a tree into a folder, as it is or as it was.
	///
	/// By default into a folder named after the tree in the current folder: as it is, which
	/// becomes this device's copy of it, as on a device that joined the library, with its Git
	/// repositories cloned once you agree; or with --at or --as-of, into an empty or new
	/// folder, a picture of the past, which is no copy to commit from.
	///
	/// A copy goes into a folder that holds files too, and replaces none of them: a file that
	/// holds the tree's version counts as written, one that holds an older version is brought
	/// up to date by nib update, and one that holds anything else stays, as an edit, which
	/// nib commit records and nib restore replaces with the tree's version. The Git
	/// repositories there stay as they are, and what the tree does not have, the next commit
	/// takes in, unless a `.nibignore` file leaves it out.
	Clone {
		/// The tree, by its name or ID.
		tree: String,
		/// The folder.
		folder: Option<PathBuf>,
		/// The revision whose author's tree to write.
		#[arg(short = 'r', long, value_name = "REVISION", conflicts_with = "as_of")]
		at: Option<String>,
		/// The moment as of which the library recorded the tree to write.
		#[arg(long, value_name = "WHEN", value_parser = crate::parse_moment)]
		as_of: Option<crate::Moment>,
		/// Clones the tree's Git repositories without asking.
		#[arg(long, conflicts_with = "no_clone")]
		clone: bool,
		/// Clones no Git repository, and lists those it would.
		#[arg(long)]
		no_clone: bool,
	},
}

/// Whether to clone: yes or no as the person said with --clone or --no-clone, or ask.
fn clone_choice(clone: bool, no_clone: bool) -> Option<bool> {
	match (clone, no_clone) {
		(true, _) => Some(true),
		(_, true) => Some(false),
		_ => None,
	}
}

/// What became of a Git repository that a tree records and its copy here does not hold.
#[derive(Debug)]
enum Cloning {
	/// Cloned.
	Cloned,
	/// Not cloned, as the person did not agree.
	NotAgreed,
	/// Refused, with why.
	Refused(String),
	/// Failed, with what was said.
	Failed(String),
}

/// Lists the Git repositories that the copy of `tree` here does not hold, and clones those
/// the engine allows once the person agrees, or as --clone or --no-clone said; never for a
/// program that reads --json, unless --clone.
fn clone_step<E: Environment>(
	library: &Library,
	tree: EntityId,
	choice: Option<bool>,
	json: bool,
	environment: &mut E,
) -> Result<Vec<(ToClone, Cloning)>, CliError> {
	let space = library.default_space();
	let to_clone = library.repositories_to_clone(space, tree)?;
	let allowed: Vec<&ToClone> = to_clone
		.iter()
		.filter(|repository| repository.refused.is_none())
		.collect();
	let agreed = match choice {
		Some(choice) => choice,
		None if json || allowed.is_empty() => false,
		None => {
			use std::fmt::Write as _;
			let mut question =
				String::from("Clone these Git repositories, with your Git and its credentials?\n");
			for repository in &allowed {
				let _ = writeln!(
					question,
					"  {} from {} ({}), recorded by {}",
					shown(&repository.path),
					shown(&repository.address),
					shown(&repository.remote),
					shown(&repository.recorded_by)
				);
				for (name, address) in &repository.others {
					let _ = writeln!(
						question,
						"    and the remote {} at {}",
						shown(name),
						shown(address)
					);
				}
			}
			question.push_str("[y/N] ");
			environment
				.ask(&question)?
				.is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
		}
	};
	let mut results = Vec::with_capacity(to_clone.len());
	for repository in to_clone {
		let outcome = if let Some(why) = &repository.refused {
			Cloning::Refused(why.clone())
		} else if !agreed {
			Cloning::NotAgreed
		} else {
			clone_one(
				library,
				tree,
				&repository,
				&Git::from_template(&environment.git()),
			)
		};
		results.push((repository, outcome));
	}
	Ok(results)
}

/// Clones one repository into the folder the engine makes for it. A clone that fails is
/// removed, with the folders made for it, as nothing in them is the person's; one whose
/// branch no remote has takes the remote's own branch instead.
fn clone_one(library: &Library, tree: EntityId, repository: &ToClone, git: &Git) -> Cloning {
	let space = library.default_space();
	let mut place = match library.prepare_clone(space, tree, repository) {
		Ok(place) => place,
		Err(error) => return Cloning::Failed(error.to_string()),
	};
	let mut cloned = git.clone(repository, &place.folder);
	if let Err(why) = &cloned
		&& repository.branch.is_some()
		&& why.contains("not found in upstream")
	{
		remove_clone(&place);
		let without_branch = ToClone {
			branch: None,
			..repository.clone()
		};
		match library.prepare_clone(space, tree, repository) {
			Ok(again) => {
				place = again;
				cloned = git.clone(&without_branch, &place.folder);
			}
			// What is in the place now is not this clone's: it stays.
			Err(error) => return Cloning::Failed(error.to_string()),
		}
	}
	match cloned {
		Ok(()) => {
			let Some(described) = git.describe_one(&place.folder, &repository.path) else {
				remove_clone(&place);
				return Cloning::Failed("Git cannot read the clone".to_owned());
			};
			// Moved or deleted elsewhere meanwhile: the clone goes, and an update tells.
			if let Err(error) = library.repository_cloned(space, tree, (repository, &described)) {
				remove_clone(&place);
				return Cloning::Failed(error.to_string());
			}
			Cloning::Cloned
		}
		Err(why) => {
			remove_clone(&place);
			Cloning::Failed(why)
		}
	}
}

/// Removes a clone that failed, and the folders made for it, from the innermost, each only
/// if nothing else came into it meanwhile: a folder in which Git made no repository is
/// removed only if it is empty, as what is in it is not the clone's.
fn remove_clone(place: &nibkeep_node::CloneFolder) {
	if fs::symlink_metadata(place.folder.join(".git")).is_ok_and(|metadata| metadata.is_dir()) {
		let _ = fs::remove_dir_all(&place.folder);
	} else {
		let _ = fs::remove_dir(&place.folder);
	}
	for folder in place.made.iter().rev() {
		if fs::remove_dir(folder).is_err() {
			break;
		}
	}
}

/// The results of cloning, for --json.
fn clones_json(clones: &[(ToClone, Cloning)]) -> Vec<serde_json::Value> {
	clones
		.iter()
		.map(|(repository, outcome)| {
			let (state, why) = match outcome {
				Cloning::Cloned => ("cloned", None),
				Cloning::NotAgreed => ("to clone", None),
				Cloning::Refused(why) => ("refused", Some(why.as_str())),
				Cloning::Failed(why) => ("failed", Some(why.as_str())),
			};
			json!({
				"path": repository.path,
				"remote": repository.remote,
				"address": repository.address,
				"recorded_by": repository.recorded_by,
				"state": state,
				"why": why,
			})
		})
		.collect()
}

/// Prints the results of cloning, one a line.
fn clones_text(clones: &[(ToClone, Cloning)], output: &mut dyn Write) -> Result<(), CliError> {
	for (repository, outcome) in clones {
		let path = shown(&repository.path);
		let from = format!(
			"from {} ({}), recorded by {}",
			shown(&repository.address),
			shown(&repository.remote),
			shown(&repository.recorded_by)
		);
		match outcome {
			Cloning::Cloned => writeln!(output, "cloned    {path}, {from}")?,
			Cloning::NotAgreed => writeln!(
				output,
				"to clone  {path}, {from}: nib update --clone clones it"
			)?,
			Cloning::Refused(why) => writeln!(output, "not cloned {path}: {}", shown(why))?,
			Cloning::Failed(why) => writeln!(output, "could not clone {path}: {}", shown(why))?,
		}
	}
	Ok(())
}

/// What a status says of the Git repositories in a tree's copy: what a commit would record
/// of them, and the work that only this computer holds, by path.
#[derive(Debug, Default)]
struct Repositories {
	changes: Vec<TreeChange>,
	work: Vec<(String, Vec<String>)>,
}

/// Reads with Git what the repositories that a status found hold, against the tree.
fn repositories_here<E: Environment>(
	library: &Library,
	(tree, root): (EntityId, &Path),
	status: &TreeStatus,
	environment: &E,
) -> Result<Repositories, CliError> {
	if status.repositories.is_empty() {
		return Ok(Repositories::default());
	}
	let space = library.default_space();
	let git = Git::from_template(&environment.git());
	let described = git.describe(root, &status.repositories);
	let changes = library.repository_changes(space, tree, (&status.repositories, &described))?;
	let pinned: std::collections::BTreeMap<String, String> = library
		.tree_repositories(space, tree)?
		.into_iter()
		.filter_map(|recorded| recorded.pinned.map(|pinned| (recorded.path, pinned)))
		.collect();
	let mut work = Vec::new();
	for path in &status.repositories {
		let found = git.work_only_here(&root.join(path), pinned.get(path).map(String::as_str));
		if !found.is_empty() {
			work.push((path.clone(), found));
		}
	}
	Ok(Repositories { changes, work })
}

/// Prints what a commit recorded, or would record, of Git repositories.
fn repositories_text(changes: &[TreeChange], output: &mut dyn Write) -> Result<(), CliError> {
	for change in changes {
		let path = &shown(&change.path);
		match &change.change {
			Change::New => writeln!(output, "new       {path}, a Git repository")?,
			Change::Deleted => writeln!(output, "deleted   {path}, a Git repository")?,
			Change::Moved { from } => {
				writeln!(
					output,
					"moved     {path}, from {}, a Git repository",
					shown(from)
				)?;
			}
			_ => writeln!(
				output,
				"edited    {path}, a Git repository's remotes, branch or pinned commit"
			)?,
		}
	}
	Ok(())
}

/// Runs a command of trees, of the library in `library_folder`, in the space of the tree it
/// works on.
pub(crate) fn tree_command<E: Environment>(
	library: &mut Library,
	(library_folder, command): (&Path, &TreeCommand),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	choose_tree_space(library, named_tree(command), environment)?;
	match command {
		TreeCommand::Commit {
			pin: Some(folder), ..
		} => pin_command(library, (folder, false), json, environment, output),
		TreeCommand::Commit {
			unpin: Some(folder),
			..
		} => pin_command(library, (folder, true), json, environment, output),
		commit @ TreeCommand::Commit { .. } => {
			commit_command(library, commit, json, (environment, timings), output)
		}
		TreeCommand::Mv { from, to } => {
			let (tree, root) = tree_here(library, environment)?;
			let from = nibkeep_node::tree_path_at(&root, &absolute(from, environment)?)?;
			let to = nibkeep_node::tree_path_at(&root, &absolute(to, environment)?)?;
			let now = environment.now();
			let space = library.default_space();
			let moved =
				library.move_in_tree(space, tree.id, (&from, &to), now, environment.rng())?;
			if json {
				return committed_output((&moved, &0), json, output);
			}
			let revision = moved.revision.map_or_else(String::new, |revision| {
				format!(", as revision {}", short(revision))
			});
			writeln!(output, "Moved {from} to {to}{revision}")?;
			Ok(())
		}
		TreeCommand::Rm { paths, force, .. } => {
			rm_command(library, (paths, *force), json, environment, output)
		}
		read @ (TreeCommand::Log { .. } | TreeCommand::Diff { .. } | TreeCommand::Blame { .. }) => {
			read_command(library, read, json, (environment, timings), output)
		}
		back => back_command(
			library,
			(library_folder, back),
			json,
			(environment, timings),
			output,
		),
	}
}

/// Runs `nib status --secrets`: lists the tree's secret files, and its ordinary files that
/// look secret.
pub(crate) fn secrets_command<E: Environment>(
	library: &mut Library,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	choose_tree_space(library, None, environment)?;
	let (tree, _) = tree_here(library, environment)?;
	let listed = library.tree_secrets(library.default_space(), tree.id)?;
	secrets_output(&listed, json, output)
}

/// Runs `nib init` in the library in `library_folder`: makes the folder given, or the
/// current one, a tree, once the person agrees, unless `yes`.
pub(crate) fn init_command<E: Environment>(
	library: &mut Library,
	(library_folder, (folder, message, yes)): (&Path, (Option<&Path>, Option<&str>, bool)),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let folder = match folder {
		Some(folder) => absolute(folder, environment)?,
		None => current_dir(environment)?,
	};
	if rewrite_pointer(
		library,
		(library_folder, &folder),
		json,
		environment,
		output,
	)? {
		return Ok(());
	}
	let start = (folder.as_path(), message, yes);
	start_command(
		library,
		(library_folder, start),
		json,
		(environment, timings),
		output,
	)
}

/// Settles the `.nib` file of the tree whose folder is `folder` already, and says so: writes it
/// again if commands there need it to find the library in `library_folder` and it does not
/// name it, as after a crash, or after the library's folder moved; or takes away one that
/// they do not need, which would take them over, as one copied there with the folder. Whether
/// it did either.
fn rewrite_pointer<E: Environment>(
	library: &Library,
	(library_folder, folder): (&Path, &Path),
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<bool, CliError> {
	let Ok(root) = nibkeep_node::resolved_path(folder) else {
		return Ok(false);
	};
	let Some((space, tree)) = pointer::tree_rooted_at(library, &root)? else {
		return Ok(false);
	};
	let ceiling = environment.ceiling();
	if pointer::needed(&root, library_folder, ceiling.as_deref())
		&& pointer::names(&root, library_folder)
	{
		return Ok(false);
	}
	let settled = pointer::settle(&root, library_folder, ceiling.as_deref())?;
	if settled == Settled::Nothing {
		return Ok(false);
	}
	if json {
		// As a tree that starts prints, with nothing committed.
		let (written, taken_away) = pointer_json(&settled);
		print_json(
			&json!({
				"tree": tree.id.to_string(),
				"root": root.display().to_string(),
				"revision": null,
				"parts": 0,
				"changes": [],
				"repositories": [],
				"secret": [],
				"pointer": written,
				"pointer_taken_away": taken_away,
			}),
			output,
		)?;
		return Ok(true);
	}
	let name = library
		.name(space, tree.id)?
		.unwrap_or_else(|| short(tree.id));
	if matches!(settled, Settled::Written(_)) {
		writeln!(
			output,
			"Wrote .nib again into {}, the tree {name}'s folder, {TELLS}",
			root.display()
		)?;
	} else {
		writeln!(
			output,
			"Took away .nib from {}, the tree {name}'s folder, {NEEDLESS}",
			root.display()
		)?;
	}
	Ok(true)
}

/// What the `.nib` file in the folder of a tree outside the library's folder is for.
const TELLS: &str = "which tells nib there which library to use";

/// Why a tree's folder inside the library's folder keeps no `.nib` file.
const NEEDLESS: &str = "where nib finds the library without it";

/// Settles the `.nib` file of the tree whose folder on this device is `root`, as commands
/// there need it to find the library in `library_folder` ([`pointer::settle`]).
fn settle_pointer<E: Environment>(
	root: &Path,
	library_folder: &Path,
	environment: &E,
) -> Result<Settled, CliError> {
	let ceiling = environment.ceiling();
	Ok(pointer::settle(root, library_folder, ceiling.as_deref())?)
}

/// The `.nib` file written, and the one taken away, for programs.
fn pointer_json(settled: &Settled) -> (Option<String>, Option<String>) {
	match settled {
		Settled::Written(file) => (Some(file.display().to_string()), None),
		Settled::TakenAway(file) => (None, Some(file.display().to_string())),
		Settled::Nothing => (None, None),
	}
}

/// Prints what it did with the `.nib` file of a tree's folder.
fn pointer_text(settled: &Settled, output: &mut dyn Write) -> Result<(), CliError> {
	match settled {
		Settled::Written(_) => writeln!(output, "Wrote .nib into the tree's folder, {TELLS}")?,
		Settled::TakenAway(_) => {
			writeln!(output, "Took away .nib from the tree's folder, {NEEDLESS}")?;
		}
		Settled::Nothing => {}
	}
	Ok(())
}

/// Whether the current folder is in a tree's folder on this device, of the space chosen, or
/// else of any space, which it chooses.
pub(crate) fn in_tree_here<E: Environment>(
	library: &mut Library,
	environment: &E,
) -> Result<bool, CliError> {
	// A current folder that is gone, as when --library names the library, is in no tree.
	let Ok(here) = current_dir(environment) else {
		return Ok(false);
	};
	choose_tree_space(library, None, environment)?;
	Ok(library
		.tree_at(library.default_space(), &here)?
		.is_some_and(|tree| tree.root.is_some()))
}

/// Runs `nib status` in a tree's folder: what changed in its copy here, and in the tree
/// elsewhere.
pub(crate) fn status_command<E: Environment>(
	library: &mut Library,
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (tree, root) = tree_here(library, environment)?;
	let status = library.tree_status(library.default_space(), tree.id)?;
	let repositories = repositories_here(library, (tree.id, &root), &status, environment)?;
	status_output((&status, &repositories), json, output)
}

/// Runs what `nib update` does, as `nib pull` does once it synced.
pub(crate) fn update_here<E: Environment>(
	library: &mut Library,
	(partial, clone, no_clone): (bool, bool, bool),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	choose_tree_space(library, None, environment)?;
	let update = (partial, clone_choice(clone, no_clone));
	update_command(library, update, json, (environment, timings), output)
}

/// The tree that a command names, if it names one.
fn named_tree(command: &TreeCommand) -> Option<&str> {
	match command {
		TreeCommand::Clone { tree, .. } => Some(tree.as_str()),
		TreeCommand::Log { tree, .. }
		| TreeCommand::Diff { tree, .. }
		| TreeCommand::Blame { tree, .. } => tree.as_deref(),
		_ => None,
	}
}

/// Runs `nib rm`: deletes the copies of the tree's files at the paths given, for the next
/// commit to record.
fn rm_command<E: Environment>(
	library: &Library,
	(paths, force): (&[PathBuf], bool),
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (tree, root) = tree_here(library, environment)?;
	let mut in_tree = Vec::with_capacity(paths.len());
	for path in paths {
		in_tree.push(nibkeep_node::tree_path_at(
			&root,
			&absolute(path, environment)?,
		)?);
	}
	let removed = library.remove_in_tree(library.default_space(), tree.id, &in_tree, force)?;
	let skipped = removed
		.skipped
		.iter()
		.map(|(path, why)| json!({ "path": path, "why": why }));
	if json {
		return print_json(
			&json!({
				"removed": removed.removed,
				"skipped": skipped.collect::<Vec<_>>(),
			}),
			output,
		);
	}
	for path in &removed.removed {
		writeln!(output, "removed   {}", shown(path))?;
	}
	for (path, why) in &removed.skipped {
		writeln!(output, "skipped   {}: {}", shown(path), shown(why))?;
	}
	writeln!(output, "Nothing is recorded: nib commit records it.")?;
	Ok(())
}

/// Runs `nib commit`: records what changed in the tree's copy here, or at the paths
/// given; where a secret file's change needs the secrets, asks for their passphrase, or sets
/// them up, and commits again; then asks whether to purge the old versions of the files it
/// made secret.
fn commit_command<E: Environment>(
	library: &mut Library,
	command: &TreeCommand,
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let TreeCommand::Commit {
		paths,
		message,
		force,
		secret,
		ordinary,
		leave_secrets,
		..
	} = command
	else {
		return Ok(());
	};
	let (tree, root) = tree_here(library, environment)?;
	let in_tree = |paths: &[PathBuf], environment: &E| -> Result<Vec<String>, CliError> {
		let mut in_tree = Vec::with_capacity(paths.len());
		for path in paths {
			let path = absolute(path, environment)?;
			in_tree.push(nibkeep_node::tree_path_at(&root, &path)?);
		}
		Ok(in_tree)
	};
	let paths_in_tree = in_tree(paths, environment)?;
	let secret = in_tree(secret, environment)?;
	let ordinary = in_tree(ordinary, environment)?;
	let space = library.default_space();
	// What Git says of the repositories that the commit finds, for it to record.
	let git = Git::from_template(&environment.git());
	let read = |found: &[String]| git.describe(&root, found);
	let mut commit = Commit::new(message.as_deref())
		.with_paths(paths_in_tree)
		.with_repositories_read_by(&read)
		.with_secret(secret)
		.with_ordinary(ordinary);
	if *force {
		commit = commit.with_force();
	}
	if *leave_secrets {
		commit = commit.leaving_secrets();
	}
	let now = environment.now();
	let committed = match library.commit_tree(space, tree.id, &commit, now, environment.rng()) {
		Err(NodeError::SecretsNeeded(paths)) => {
			let refused = NodeError::SecretsNeeded(paths);
			secrets_for(library, (environment, timings), refused)?;
			let now = environment.now();
			library.commit_tree(space, tree.id, &commit, now, environment.rng())?
		}
		committed => committed?,
	};
	let purged = purge_made_secret(library, &committed, json, environment)?;
	committed_output((&committed, &purged), json, output)
}

/// Runs `nib update`: brings the copy here to the tree, the secrets unlocked if a
/// person is there to type their passphrase, and writes what it held back once the person
/// agrees; then clones the Git repositories the copy lacks.
fn update_command<E: Environment>(
	library: &mut Library,
	(partial, cloning): (bool, Option<bool>),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (tree, _) = tree_here(library, environment)?;
	secrets_if_asked(library, tree.id, (environment, timings))?;
	let space = library.default_space();
	let mut updated = library.update_tree(space, tree.id, partial)?;
	if !updated.held.is_empty() && !json {
		let question = format!(
			"Changes from other devices that could expose secrets: {}. Write them? [y/N] ",
			updated.held.join(", ")
		);
		let agreed = environment
			.ask(&question)?
			.is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"));
		if agreed {
			let then = library.update_tree_agreeing(space, tree.id, partial)?;
			merge_updates(&mut updated, then);
		}
	}
	let clones = clone_step(library, tree.id, cloning, json, environment)?;
	updated_output((&updated, &clones), json, output)
}

/// Unlocks the secrets for a command that needs them, asking for their passphrase, or sets
/// them up with a passphrase of their own where the library has none; where nobody can type
/// one, as for a script or an AI agent, fails as `refused` says.
fn secrets_for<E: Environment>(
	library: &mut Library,
	(environment, timings): (&mut E, &mut Timings),
	refused: NodeError,
) -> Result<(), CliError> {
	match crate::unlock_secrets(library, true, environment, timings) {
		Ok(()) => Ok(()),
		Err(CliError::Io(_)) => Err(refused.into()),
		Err(error) => Err(error),
	}
}

/// Unlocks the secrets, if a person is there to type their passphrase, before a command
/// writes a tree that holds secret files: without them, it writes the rest.
fn secrets_if_asked<E: Environment>(
	library: &mut Library,
	tree: EntityId,
	(environment, timings): (&mut E, &mut Timings),
) -> Result<(), CliError> {
	let space = library.default_space();
	if !environment.can_ask()
		|| library.secrets_unlocked(space)
		|| library.secrets_protection()?.is_none()
		|| library.tree_secrets(space, tree)?.secret.is_empty()
	{
		return Ok(());
	}
	crate::unlock_secrets(library, false, environment, timings)
}

/// Asks, for each ordinary file that a commit made secret, whether to purge its old
/// versions everywhere, for good: their contents that no other file holds. Without a
/// person to ask, nothing is purged. Returns how many contents were purged.
fn purge_made_secret<E: Environment>(
	library: &mut Library,
	committed: &Committed,
	json: bool,
	environment: &mut E,
) -> Result<usize, CliError> {
	let space = library.default_space();
	let mut purged = 0;
	for &file in &committed.made_secret {
		let made = library.would_purge_made_secret(space, file)?;
		if made.purged.is_empty() || json {
			continue;
		}
		let name = library.name(space, file)?.unwrap_or_else(|| short(file));
		let kept = if made.kept.is_empty() {
			String::new()
		} else {
			format!(
				"; {} that other files hold stay",
				count(made.kept.len(), "content", "contents")
			)
		};
		let question = format!(
			"The old versions of {name} hold {} that the secrets did not protect{kept}. Purge them on every device, for good? [y/N] ",
			count(made.purged.len(), "content", "contents"),
		);
		let agreed = environment
			.ask(&question)?
			.is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"));
		if agreed {
			let now = environment.now();
			purged += library
				.purge_made_secret(space, file, now, environment.rng())?
				.purged
				.len();
		}
	}
	Ok(purged)
}

/// Chooses the space of the tree that a command works on, where --space chose none: the
/// space of the tree it names, or else of the tree whose folder the current folder is in. A
/// tree is started in the space chosen, or else in the personal space.
fn choose_tree_space<E: Environment>(
	library: &mut Library,
	named: Option<&str>,
	environment: &E,
) -> Result<(), CliError> {
	if library.chosen_space().is_some() {
		return Ok(());
	}
	let space = if let Some(named) = named {
		Some(library.locate(named)?.0)
	} else {
		let here = current_dir(environment)?;
		let mut found = None;
		for space in library.spaces() {
			if library.tree_at(space, &here)?.is_some() {
				found = Some(space);
				break;
			}
		}
		found
	};
	if space.is_some() {
		library.choose_space(space)?;
	}
	Ok(())
}

/// Runs `nib commit --pin` or `--unpin`: pins the repository in a folder at the commit it has
/// checked out, or takes its pin away.
fn pin_command<E: Environment>(
	library: &mut Library,
	(path, unpin): (&Path, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (tree, root) = tree_here(library, environment)?;
	let folder = absolute(path, environment)?;
	let path = nibkeep_node::tree_path_at(&root, &folder)?;
	let commit = if unpin {
		None
	} else {
		Some(
			Git::from_template(&environment.git())
				.head(&folder)
				.ok_or(CliError::NoGitCommit(folder))?,
		)
	};
	let now = environment.now();
	let space = library.default_space();
	let pinned = library.pin_repository(
		space,
		tree.id,
		&path,
		commit.as_deref(),
		now,
		environment.rng(),
	)?;
	if json {
		return committed_output((&pinned, &0), json, output);
	}
	let revision = pinned.revision.map_or_else(
		|| ", as it was".to_owned(),
		|revision| format!(", as revision {}", short(revision)),
	);
	match commit {
		Some(commit) => writeln!(output, "Pinned {path} at {}{revision}", &commit[..12])?,
		None => writeln!(output, "Unpinned {path}{revision}")?,
	}
	Ok(())
}

/// Runs the commands that read a tree's history: log, diff and blame; outside every tree's
/// folder, with the tree named.
fn read_command<E: Environment>(
	library: &mut Library,
	command: &TreeCommand,
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		TreeCommand::Log { path, tree } => {
			let (tree, root) = named_or_here(library, tree.as_deref(), environment)?;
			let path = match path {
				Some(path) => Some(path_given(path, root.as_deref(), environment)?),
				None => None,
			};
			let log = library.tree_log(library.default_space(), tree, path.as_deref())?;
			log_output(&log, json, &environment.time_zone(), output)
		}
		TreeCommand::Diff {
			from,
			to,
			since,
			until,
			tree,
			secrets,
		} => {
			let (tree, _) = named_or_here(library, tree.as_deref(), environment)?;
			let space = library.default_space();
			if *secrets && !library.secrets_unlocked(space) {
				secrets_for(library, (environment, timings), NodeError::SecretsLocked)?;
			}
			let (from, to) = (
				from.as_ref().or(since.as_ref()),
				to.as_ref().or(until.as_ref()),
			);
			let between = match (from, to) {
				(None, _) => None,
				(Some(from), to) => {
					let from = TreePoint::Revision(library.find_entity(space, from)?);
					let to = match to {
						Some(to) => TreePoint::Revision(library.find_entity(space, to)?),
						None => TreePoint::Now,
					};
					Some((from, to))
				}
			};
			let diffs = library.tree_diff_with(space, tree, between, *secrets)?;
			if json {
				return print_json(&diff_json(&diffs), output);
			}
			diff_text(&diffs, output)
		}
		TreeCommand::Blame {
			path,
			tree,
			secrets,
		} => {
			let (tree, root) = named_or_here(library, tree.as_deref(), environment)?;
			let path = path_given(path, root.as_deref(), environment)?;
			let space = library.default_space();
			if *secrets && !library.secrets_unlocked(space) {
				secrets_for(library, (environment, timings), NodeError::SecretsLocked)?;
			}
			let blame = library.tree_blame_with(space, tree, &path, *secrets)?;
			blame_output(&blame, json, &environment.time_zone(), output)
		}
		_ => Ok(()),
	}
}

/// Runs the commands that write a tree, or into its copy: update, restore, revert and
/// clone, of the library in `library_folder`.
fn back_command<E: Environment>(
	library: &mut Library,
	(library_folder, command): (&Path, &TreeCommand),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		TreeCommand::Update {
			partial,
			clone,
			no_clone,
		} => {
			let update = (*partial, clone_choice(*clone, *no_clone));
			update_command(library, update, json, (environment, timings), output)
		}
		TreeCommand::Restore { path, at, as_of } => {
			let (tree, root) = tree_here(library, environment)?;
			secrets_if_asked(library, tree.id, (environment, timings))?;
			let path = nibkeep_node::tree_path_at(&root, &absolute(path, environment)?)?;
			let point = point_given(library, (at.as_deref(), *as_of), environment)?;
			let written =
				library.restore_in_tree(library.default_space(), tree.id, &path, point)?;
			written_output(&written, "restored ", json, output)?;
			if !json {
				writeln!(
					output,
					"Nothing is recorded: nib commit records what you keep."
				)?;
			}
			Ok(())
		}
		TreeCommand::Revert { revision } => {
			let (tree, _) = tree_here(library, environment)?;
			secrets_if_asked(library, tree.id, (environment, timings))?;
			let reverted = library
				.locate(revision)
				.and_then(|(space, found)| library.revert_in_tree(space, tree.id, found));
			match reverted {
				Ok(reverted) => reverted_output(&reverted, json, output),
				// A path, as the revert of Mercurial, Subversion and Fossil takes, is for
				// restore.
				Err(_)
					if fs::symlink_metadata(absolute(Path::new(revision), environment)?)
						.is_ok() =>
				{
					Err(CliError::RevertTakesRevision(revision.clone()))
				}
				Err(error) => Err(error.into()),
			}
		}
		clone @ TreeCommand::Clone { .. } => clone_command(
			library,
			(library_folder, clone),
			json,
			(environment, timings),
			output,
		),
		_ => Ok(()),
	}
}

/// Runs `nib clone`: writes a tree into a folder, as this device's copy of it, or as a
/// picture of the past. A copy names the library in `library_folder`, where commands in its
/// folder would not find it otherwise.
fn clone_command<E: Environment>(
	library: &mut Library,
	(library_folder, command): (&Path, &TreeCommand),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let TreeCommand::Clone {
		tree,
		folder,
		at,
		as_of,
		clone,
		no_clone,
	} = command
	else {
		return Ok(());
	};
	let (space, tree) = library.locate(tree)?;
	secrets_if_asked(library, tree, (environment, timings))?;
	let folder = match folder {
		Some(folder) => absolute(folder, environment)?,
		None => current_dir(environment)?.join(folder_named_after(library, space, tree)?),
	};
	let point = point_given(library, (at.as_deref(), *as_of), environment)?;
	let now = environment.now();
	let written = library.write_tree(space, tree, &folder, point, now, environment.rng())?;
	// A picture of the past clones nothing, and is no tree's folder; this device's copy
	// clones what the person agrees.
	let (clones, settled) = if point == TreePoint::Now {
		let root = nibkeep_node::resolved_path(&folder)?;
		let settled = settle_pointer(&root, library_folder, environment)?;
		let clones = clone_step(
			library,
			tree,
			clone_choice(*clone, *no_clone),
			json,
			environment,
		)?;
		(clones, settled)
	} else {
		(Vec::new(), Settled::Nothing)
	};
	if json {
		let (pointer, taken_away) = pointer_json(&settled);
		return print_json(
			&json!({
				"written": written.written,
				"skipped": written
					.skipped
					.iter()
					.map(|(path, why)| json!({ "path": path, "why": why }))
					.collect::<Vec<_>>(),
				"sealed": written.sealed,
				"found": written.found,
				"older": written.older,
				"differing": written.differing,
				"others": written.others,
				"repositories": clones_json(&clones),
				"pointer": pointer,
				"pointer_taken_away": taken_away,
			}),
			output,
		);
	}
	let copy = if point == TreePoint::Now {
		", which is this device's copy of it"
	} else {
		", a picture of the past, which is no copy"
	};
	writeln!(
		output,
		"Wrote {} into {}{copy}",
		count(written.written.len(), "file", "files"),
		folder.display()
	)?;
	for (path, why) in &written.skipped {
		writeln!(output, "skipped   {path}: {why}")?;
	}
	held_text(&written, output)?;
	sealed_text(&written.sealed, output)?;
	clones_text(&clones, output)?;
	pointer_text(&settled, output)
}

/// What a folder held before a tree was written into it, and what came of it.
fn held_text(written: &Written, output: &mut dyn Write) -> Result<(), CliError> {
	match written.found.len() {
		0 => {}
		1 => writeln!(output, "1 file there held the tree's version already")?,
		found => writeln!(
			output,
			"{found} files there held the tree's version already"
		)?,
	}
	for path in &written.older {
		writeln!(
			output,
			"older     {}: an older version, which nib update brings up to date",
			shown(path)
		)?;
	}
	for path in &written.differing {
		writeln!(
			output,
			"differs   {}: kept as an edit; nib diff shows it, nib restore writes the tree's version",
			shown(path)
		)?;
	}
	match written.others.len() {
		0 => {}
		1 => writeln!(
			output,
			"1 file there is not the tree's: the next commit adds it, unless .nibignore leaves it out"
		)?,
		others => writeln!(
			output,
			"{others} files there are not the tree's: the next commit adds them, unless .nibignore leaves them out"
		)?,
	}
	Ok(())
}

/// The name of the folder that `nib clone` writes a tree into when none is given: the
/// tree's name, if it is a name a folder can have, or else the start of its ID.
fn folder_named_after(
	library: &Library,
	space: nibkeep_base::SpaceId,
	tree: EntityId,
) -> Result<String, CliError> {
	let named = library.name(space, tree)?.filter(|name| {
		!name.is_empty()
			&& name != "."
			&& name != ".."
			&& !name.contains(['/', '\\'])
			&& !name.contains(char::is_control)
	});
	Ok(named.unwrap_or_else(|| short(tree)))
}

/// Takes into what a first update did what a second one did once the person agreed to
/// what the first held back.
fn merge_updates(first: &mut Updated, then: Updated) {
	first.written.extend(then.written);
	first.moved.extend(then.moved);
	first.deleted.extend(then.deleted);
	first.merged.extend(then.merged);
	first.conflicts.extend(then.conflicts);
	first.kept = then.kept;
	first.contested = then.contested;
	first.skipped = then.skipped;
	first.waiting = then.waiting;
	first.held = then.held;
	first.sealed = then.sealed;
	for list in [
		&mut first.written,
		&mut first.deleted,
		&mut first.merged,
		&mut first.conflicts,
	] {
		list.sort();
		list.dedup();
	}
	first.moved.sort();
	first.moved.dedup();
}

/// Prints the secret files not written, as this device cannot read them.
fn sealed_text(sealed: &[String], output: &mut dyn Write) -> Result<(), CliError> {
	for path in sealed {
		writeln!(
			output,
			"sealed    {}: a secret file, not written here, as the secrets are locked or not on this device",
			shown(path)
		)?;
	}
	Ok(())
}

/// Prints a tree's secret files, and its ordinary files that look secret.
fn secrets_output(
	listed: &TreeSecrets,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"secret": listed.secret,
				"sealed": listed.sealed,
				"looking_secret": listed.looking_secret,
			}),
			output,
		);
	}
	if listed.secret.is_empty() && listed.looking_secret.is_empty() {
		writeln!(output, "The tree has no secret files")?;
		return Ok(());
	}
	for path in &listed.secret {
		if listed.sealed.contains(path) {
			writeln!(
				output,
				"secret    {}, not written here, as the secrets are locked or not on this device",
				shown(path)
			)?;
		} else {
			writeln!(output, "secret    {}", shown(path))?;
		}
	}
	for path in &listed.looking_secret {
		writeln!(
			output,
			"ordinary  {}, which looks secret: nib commit --secret {} makes it a secret file",
			shown(path),
			shown(path)
		)?;
	}
	Ok(())
}

/// The tree named, by its name or ID, with its folder here if it has one; or else the tree
/// whose folder here holds the current folder.
fn named_or_here<E: Environment>(
	library: &Library,
	named: Option<&str>,
	environment: &E,
) -> Result<(EntityId, Option<PathBuf>), CliError> {
	if let Some(named) = named {
		let tree = library.find_entity(library.default_space(), named)?;
		Ok((tree, None))
	} else {
		let (tree, root) = tree_here(library, environment).map_err(|error| match error {
			CliError::NotInTree(here) => CliError::NoTreeHere(here),
			error => error,
		})?;
		Ok((tree.id, Some(root)))
	}
}

/// A path given on the command line, as the tree names it: relative to the current folder
/// in the tree's folder here, or, for a tree named, as the tree names it.
fn path_given<E: Environment>(
	path: &Path,
	root: Option<&Path>,
	environment: &E,
) -> Result<String, CliError> {
	match root {
		Some(root) => Ok(nibkeep_node::tree_path_at(
			root,
			&absolute(path, environment)?,
		)?),
		None => Ok(path.to_string_lossy().replace('\\', "/")),
	}
}

/// The point of a tree's history given on the command line: a revision, a moment, or now.
fn point_given<E: Environment>(
	library: &Library,
	(at, as_of): (Option<&str>, Option<crate::Moment>),
	environment: &E,
) -> Result<TreePoint, CliError> {
	if let Some(revision) = at {
		return Ok(TreePoint::Revision(
			library.find_entity(library.default_space(), revision)?,
		));
	}
	if let Some(moment) = as_of {
		let (millis, _) = moment.resolve(&environment.time_zone())?;
		return Ok(TreePoint::AsOf(millis.unwrap_or(0)));
	}
	Ok(TreePoint::Now)
}

/// Starts a tree, as `nib init` in a library does: asks the person first, unless `yes`,
/// then starts the tree in `folder`, and names the library in `library_folder` there, where
/// commands in the tree's folder would not find it otherwise.
fn start_command<E: Environment>(
	library: &mut Library,
	(library_folder, (folder, message, yes)): (&Path, (&Path, Option<&str>, bool)),
	json: bool,
	(environment, timings): (&mut E, &mut Timings),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let preview = library.would_start_tree(space, folder)?;
	// Nothing starts where the file that names the library could not go.
	let ceiling = environment.ceiling();
	if let Ok(root) = nibkeep_node::resolved_path(folder)
		&& pointer::needed(&root, library_folder, ceiling.as_deref())
	{
		pointer::check_place(&root)?;
	}
	// With --json, nib runs for a program, which is not asked.
	if !yes {
		let repositories = if preview.repositories.is_empty() {
			String::new()
		} else {
			format!(
				", and {} recorded by their remotes",
				count(
					preview.repositories.len(),
					"Git repository",
					"Git repositories"
				)
			)
		};
		let question = format!(
			"{} in {} ({}) become the tree's copies{repositories}: a purge of a file's content deletes its copy where it was not edited. Start the tree? [y/N] ",
			count(preview.files, "file", "files"),
			folder.display(),
			bytes(preview.bytes),
		);
		let answer = if json {
			None
		} else {
			environment.ask(&question)?
		};
		let agreed = answer
			.is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"));
		if !agreed {
			return Err(CliError::UnconfirmedStart);
		}
	}
	let described = Git::from_template(&environment.git()).describe(folder, &preview.repositories);
	let commit = Commit::new(message).with_repositories(&described);
	let now = environment.now();
	let started = match library.start_tree_with(space, folder, &commit, now, environment.rng()) {
		Err(NodeError::SecretsNeeded(paths)) => {
			secrets_for(
				library,
				(environment, timings),
				NodeError::SecretsNeeded(paths),
			)?;
			let now = environment.now();
			library.start_tree_with(space, folder, &commit, now, environment.rng())?
		}
		started => started?,
	};
	let settled = settle_pointer(&started.root, library_folder, environment)?;
	if json {
		let (pointer, taken_away) = pointer_json(&settled);
		return print_json(
			&json!({
				"tree": started.tree.to_string(),
				"root": started.root.display().to_string(),
				"revision": revision_json(&started.committed),
				"parts": started.committed.parts,
				"changes": changes_json(&started.committed.changes),
				"repositories": changes_json(&started.committed.repositories),
				"secret": started.committed.secret,
				"pointer": pointer,
				"pointer_taken_away": taken_away,
			}),
			output,
		);
	}
	let name = library
		.name(space, started.tree)?
		.unwrap_or_else(|| short(started.tree));
	let as_revision = started
		.committed
		.revision
		.map_or_else(String::new, |revision| {
			format!("as revision {}", short(revision))
		});
	writeln!(
		output,
		"Started the tree {name} in {}: {}, committed {as_revision}",
		started.root.display(),
		count(started.committed.changes.len(), "file", "files"),
	)?;
	secret_text(&started.committed, output)?;
	repositories_text(&started.committed.repositories, output)?;
	pointer_text(&settled, output)
}

/// Prints what a commit did of secret files: those it sealed for the secrets, those it
/// left, and the ordinary files that hold a secret file's content.
fn secret_text(committed: &Committed, output: &mut dyn Write) -> Result<(), CliError> {
	for path in &committed.secret {
		writeln!(output, "secret    {}, sealed for the secrets", shown(path))?;
	}
	for path in &committed.left {
		writeln!(
			output,
			"left      {}: a secret file's change, which waits for the secrets",
			shown(path)
		)?;
	}
	for path in &committed.unprotected {
		writeln!(
			output,
			"plain     {}: holds a secret file's content, which the secrets do not protect here",
			shown(path)
		)?;
	}
	Ok(())
}

/// The tree whose folder on this device holds the current folder, and that folder.
fn tree_here<E: Environment>(
	library: &Library,
	environment: &E,
) -> Result<(Tree, PathBuf), CliError> {
	let here = current_dir(environment)?;
	let tree = library
		.tree_at(library.default_space(), &here)?
		.ok_or_else(|| CliError::NotInTree(here.clone()))?;
	let root = tree.root.clone().ok_or(CliError::NotInTree(here))?;
	Ok((tree, root))
}

/// Prints what changed in a tree's copy, and in the tree elsewhere.
fn status_output(
	(status, repositories): (&TreeStatus, &Repositories),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let skipped: Vec<serde_json::Value> = status
			.skipped
			.iter()
			.map(|(path, why)| json!({ "path": path, "why": why }))
			.collect();
		let cut_short: Vec<serde_json::Value> = status
			.cut_short
			.iter()
			.map(|cut| {
				json!({
					"revision": cut.revision.to_string(),
					"parts": cut.parts,
					"made": cut.made,
				})
			})
			.collect();
		return print_json(
			&json!({
				"tree": status.tree.to_string(),
				"root": status.root.display().to_string(),
				"changes": changes_json(&status.changes),
				"behind": status.behind,
				"competing": status.competing,
				"skipped": skipped,
				"cut_short": cut_short,
				"secret": status.secret,
				"sealed": status.sealed,
				"read": status.read,
				"repositories": changes_json(&repositories.changes),
				"only_here": repositories
					.work
					.iter()
					.map(|(path, work)| json!({ "path": path, "work": work }))
					.collect::<Vec<_>>(),
			}),
			output,
		);
	}
	let nothing = status.changes.is_empty()
		&& status.behind.is_empty()
		&& status.competing.is_empty()
		&& status.skipped.is_empty()
		&& status.cut_short.is_empty()
		&& status.sealed.is_empty()
		&& repositories.changes.is_empty()
		&& repositories.work.is_empty();
	if nothing {
		writeln!(output, "Nothing changed in the tree's copy here")?;
		return Ok(());
	}
	changes_text(&status.changes, output)?;
	for path in &status.secret {
		writeln!(
			output,
			"secret    {}, which a commit seals for the secrets",
			shown(path)
		)?;
	}
	sealed_text(&status.sealed, output)?;
	repositories_text(&repositories.changes, output)?;
	for (path, work) in &repositories.work {
		writeln!(output, "only here {}: {}", shown(path), work.join(", "))?;
	}
	for path in &status.behind {
		writeln!(
			output,
			"elsewhere {}, changed in the tree since this copy",
			shown(path)
		)?;
	}
	for path in &status.competing {
		writeln!(
			output,
			"competing {}, which devices changed apart",
			shown(path)
		)?;
	}
	for (path, why) in &status.skipped {
		writeln!(output, "skipped   {}: {}", shown(path), shown(why))?;
	}
	for cut in &status.cut_short {
		writeln!(
			output,
			"cut short revision {}: {} of {} parts; the next commit takes in the rest",
			short(cut.revision),
			cut.made,
			cut.parts
		)?;
	}
	Ok(())
}

/// Prints what a commit recorded, and how many contents it purged of files it made secret.
fn committed_output(
	(committed, purged): (&Committed, &usize),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"revision": revision_json(committed),
				"parts": committed.parts,
				"changes": changes_json(&committed.changes),
				"repositories": changes_json(&committed.repositories),
				"secret": committed.secret,
				"left": committed.left,
				"unprotected": committed.unprotected,
				"made_secret": committed
					.made_secret
					.iter()
					.map(ToString::to_string)
					.collect::<Vec<_>>(),
			}),
			output,
		);
	}
	let Some(revision) = committed.revision.map(short) else {
		writeln!(output, "Nothing to commit")?;
		return secret_text(committed, output);
	};
	// The repositories' changes count with the files', as both are listed below.
	let recorded = committed.changes.len() + committed.repositories.len();
	let changes = count(recorded, "change", "changes");
	writeln!(output, "Committed {changes} as revision {revision}")?;
	changes_text(&committed.changes, output)?;
	secret_text(committed, output)?;
	if *purged > 0 {
		writeln!(
			output,
			"Purged {} of the old versions of files made secret",
			count(*purged, "content", "contents")
		)?;
	} else if !committed.made_secret.is_empty() {
		writeln!(
			output,
			"The old versions of {} made secret stay readable on every device, as nothing purged them",
			count(committed.made_secret.len(), "file", "files")
		)?;
	}
	repositories_text(&committed.repositories, output)
}

/// Prints changes, one a line, as a word and a path, ordered by what they are and then by
/// path: deletions first, then moves, new files, versions and executable bits.
fn changes_text(changes: &[TreeChange], output: &mut dyn Write) -> Result<(), CliError> {
	let mut ordered: Vec<&TreeChange> = changes.iter().collect();
	ordered.sort_by_key(|change| (rank(&change.change), change.path.as_str()));
	for change in ordered {
		let path = &shown(&change.path);
		match &change.change {
			Change::New => writeln!(output, "new       {path}")?,
			Change::Edited => writeln!(output, "edited    {path}")?,
			Change::Executable(true) => writeln!(output, "executable {path}")?,
			Change::Executable(false) => writeln!(output, "no longer executable {path}")?,
			Change::Deleted => writeln!(output, "deleted   {path}")?,
			Change::Moved { from } => writeln!(output, "moved     {path}, from {}", shown(from))?,
			Change::Unknown => writeln!(
				output,
				"edited    {path}, from a version this device does not know"
			)?,
		}
	}
	Ok(())
}

/// The order changes are shown in.
fn rank(change: &Change) -> u8 {
	match change {
		Change::Deleted => 0,
		Change::Moved { .. } => 1,
		Change::New => 2,
		Change::Edited | Change::Unknown => 3,
		Change::Executable(_) => 4,
	}
}

/// Changes in JSON.
fn changes_json(changes: &[TreeChange]) -> Vec<serde_json::Value> {
	changes
		.iter()
		.map(|change| {
			let (what, from, executable) = match &change.change {
				Change::New => ("new", None, None),
				Change::Edited => ("edited", None, None),
				Change::Executable(executable) => ("executable", None, Some(*executable)),
				Change::Deleted => ("deleted", None, None),
				Change::Moved { from } => ("moved", Some(from.as_str()), None),
				Change::Unknown => ("unknown", None, None),
			};
			let mut shown = json!({ "path": change.path, "change": what });
			if let Some(from) = from {
				shown["from"] = json!(from);
			}
			if let Some(executable) = executable {
				shown["executable"] = json!(executable);
			}
			shown
		})
		.collect()
}

/// The revision of a commit in JSON, if it made one.
fn revision_json(committed: &Committed) -> Option<String> {
	committed.revision.map(|revision| revision.to_string())
}

/// A number of bytes, as a person reads it.
fn bytes(bytes: u64) -> String {
	const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
	if bytes < 1_000 {
		return count(
			usize::try_from(bytes).unwrap_or(usize::MAX),
			"byte",
			"bytes",
		);
	}
	let mut value = u128::from(bytes);
	let mut unit = 0;
	while value >= 1_000_000 && unit < UNITS.len() - 1 {
		value /= 1_000;
		unit += 1;
	}
	// One decimal, from what is left in thousands.
	let tenths = value / 100;
	format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
}

/// Who made a change, for people: the user's name, and the AI agent through which they
/// made it, if one did.
fn who(author: Option<&nibkeep_node::Author>) -> String {
	let Some(author) = author else {
		return "unknown".to_owned();
	};
	let user = author
		.user
		.name
		.clone()
		.unwrap_or_else(|| "unknown".to_owned());
	match &author.ai_agent {
		Some(ai_agent) => format!("{user}, through {ai_agent}"),
		None => user,
	}
}

/// Who made a change, in JSON.
fn author_json(author: Option<&nibkeep_node::Author>) -> serde_json::Value {
	author.map_or(serde_json::Value::Null, |author| {
		json!({
			"user": author.user.id.to_string(),
			"name": author.user.name,
			"replica": author.replica.to_string(),
			"ai_agent": author.ai_agent,
		})
	})
}

/// A moment in JSON: milliseconds since 1970, in RFC 3339, in UTC.
fn moment_json(millis: u64) -> serde_json::Value {
	i64::try_from(millis)
		.ok()
		.and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
		.map_or_else(|| json!(millis), |timestamp| json!(timestamp.to_string()))
}

/// Prints a tree's log: each change, newest first, with its revision, time, author and
/// message, and what it did.
fn log_output(
	log: &[TreeLogEntry],
	json: bool,
	zone: &jiff::tz::TimeZone,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let entries: Vec<serde_json::Value> = log
			.iter()
			.map(|entry| {
				json!({
					"change": entry.change,
					"at": moment_json(entry.at),
					"author": author_json(entry.author.as_ref()),
					"revision": entry.revision.map(|revision| revision.to_string()),
					"message": entry.message,
					"arrived": entry.part.map(|(arrived, parts)| json!({ "parts": arrived, "of": parts })),
					"changes": changes_json(&entry.changes),
				})
			})
			.collect();
		return print_json(&json!(entries), output);
	}
	for (number, entry) in log.iter().enumerate() {
		if number > 0 {
			writeln!(output)?;
		}
		let what = match entry.revision {
			Some(revision) => format!("revision {}", short(revision)),
			None => format!("change {}", entry.change.get(..8).unwrap_or(&entry.change)),
		};
		let part = entry.part.map_or_else(String::new, |(arrived, parts)| {
			format!(", {arrived} of {parts} parts so far")
		});
		writeln!(
			output,
			"{what}  {}  {}{part}",
			moment_text(entry.at, zone),
			who(entry.author.as_ref())
		)?;
		if let Some(message) = &entry.message {
			for line in message.lines() {
				writeln!(output, "    {line}")?;
			}
		}
		let mut changes = Vec::new();
		changes_text(&entry.changes, &mut changes)?;
		for line in String::from_utf8_lossy(&changes).lines() {
			writeln!(output, "  {line}")?;
		}
	}
	Ok(())
}

/// A diff, for --json.
pub(crate) fn diff_json(diffs: &[FileDiff]) -> serde_json::Value {
	let shown: Vec<serde_json::Value> = diffs
		.iter()
		.map(|diff| {
			let mut shown = changes_json(&[TreeChange {
				path: diff.path.clone(),
				change: diff.change.clone(),
			}])
			.remove(0);
			shown["patch"] = json!(diff.patch);
			shown["binary"] = json!(diff.binary);
			shown["secret"] = json!(diff.secret);
			shown
		})
		.collect();
	json!(shown)
}

/// Prints a diff: each file's change, and the lines of text that changed.
pub(crate) fn diff_text(diffs: &[FileDiff], output: &mut dyn Write) -> Result<(), CliError> {
	if diffs.is_empty() {
		writeln!(output, "No differences")?;
		return Ok(());
	}
	for diff in diffs {
		changes_text(
			&[TreeChange {
				path: diff.path.clone(),
				change: diff.change.clone(),
			}],
			output,
		)?;
		if let Some(patch) = &diff.patch {
			write!(output, "{patch}")?;
		} else if diff.secret {
			writeln!(
				output,
				"  a secret file; nib diff --secrets shows its lines"
			)?;
		} else if diff.binary {
			writeln!(output, "  versions differ; not text")?;
		}
	}
	Ok(())
}

/// Prints blame: each line with the revision, author and time that last changed it.
fn blame_output(
	blame: &[BlameLine],
	json: bool,
	zone: &jiff::tz::TimeZone,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let lines: Vec<serde_json::Value> = blame
			.iter()
			.map(|line| {
				json!({
					"text": line.text,
					"revision": line.revision.map(|revision| revision.to_string()),
					"author": author_json(line.author.as_ref()),
					"at": moment_json(line.at),
				})
			})
			.collect();
		return print_json(&json!(lines), output);
	}
	let names: Vec<String> = blame.iter().map(|line| who(line.author.as_ref())).collect();
	let width = names
		.iter()
		.map(|name| name.chars().count())
		.max()
		.unwrap_or(0);
	for (line, name) in blame.iter().zip(&names) {
		let revision = line.revision.map_or_else(|| "--------".to_owned(), short);
		writeln!(
			output,
			"{revision}  {name:<width$}  {}  {}",
			moment_text(line.at, zone),
			line.text
		)?;
	}
	Ok(())
}

/// Prints what writing wrote, each path after `word`, and what it skipped, with why.
fn written_output(
	written: &Written,
	word: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let skipped: Vec<serde_json::Value> = written
			.skipped
			.iter()
			.map(|(path, why)| json!({ "path": path, "why": why }))
			.collect();
		let aside: Vec<serde_json::Value> = written
			.aside
			.iter()
			.map(|(path, to)| json!({ "path": path, "to": to }))
			.collect();
		return print_json(
			&json!({
				"written": written.written,
				"skipped": skipped,
				"sealed": written.sealed,
				"aside": aside,
			}),
			output,
		);
	}
	for path in &written.written {
		match written.aside.iter().find(|(at, _)| at == path) {
			Some((_, beside)) => writeln!(output, "{word} {path}, your edits kept as {beside}")?,
			None => writeln!(output, "{word} {path}")?,
		}
	}
	for (path, why) in &written.skipped {
		writeln!(output, "skipped   {path}: {why}")?;
	}
	sealed_text(&written.sealed, output)
}

/// Prints what undoing a revision wrote.
fn reverted_output(
	reverted: &Reverted,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let skipped: Vec<serde_json::Value> = reverted
			.skipped
			.iter()
			.map(|(path, why)| json!({ "path": path, "why": why }))
			.collect();
		let moved: Vec<serde_json::Value> = reverted
			.moved
			.iter()
			.map(|(from, to)| json!({ "from": from, "to": to }))
			.collect();
		return print_json(
			&json!({
				"written": reverted.written,
				"deleted": reverted.deleted,
				"moved": moved,
				"conflicts": reverted.conflicts,
				"skipped": skipped,
			}),
			output,
		);
	}
	for (from, to) in &reverted.moved {
		writeln!(output, "moved     {to}, back from {from}")?;
	}
	for path in &reverted.deleted {
		writeln!(output, "deleted   {path}")?;
	}
	for path in &reverted.written {
		writeln!(output, "written   {path}")?;
	}
	for path in &reverted.conflicts {
		writeln!(output, "conflict  {path}")?;
	}
	for (path, why) in &reverted.skipped {
		writeln!(output, "skipped   {path}: {why}")?;
	}
	writeln!(
		output,
		"Nothing is recorded: nib commit records what you keep."
	)?;
	Ok(())
}

/// Prints what updating the copy did.
fn updated_output(
	(updated, clones): (&Updated, &[(ToClone, Cloning)]),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let with_why = |list: &[(String, String)]| -> Vec<serde_json::Value> {
		list.iter()
			.map(|(path, why)| json!({ "path": path, "why": why }))
			.collect()
	};
	if json {
		let moved: Vec<serde_json::Value> = updated
			.moved
			.iter()
			.map(|(from, to)| json!({ "from": from, "to": to }))
			.collect();
		let waiting: Vec<serde_json::Value> = updated
			.waiting
			.iter()
			.map(
				|cut| json!({ "revision": cut.revision.to_string(), "parts": cut.parts, "made": cut.made }),
			)
			.collect();
		return print_json(
			&json!({
				"written": updated.written,
				"moved": moved,
				"deleted": updated.deleted,
				"merged": updated.merged,
				"conflicts": updated.conflicts,
				"contested": updated.contested,
				"kept": with_why(&updated.kept),
				"skipped": with_why(&updated.skipped),
				"waiting": waiting,
				"held": updated.held,
				"sealed": updated.sealed,
				"repositories": clones_json(clones),
			}),
			output,
		);
	}
	let nothing = updated.written.is_empty()
		&& updated.contested.is_empty()
		&& updated.moved.is_empty()
		&& updated.deleted.is_empty()
		&& updated.merged.is_empty()
		&& updated.conflicts.is_empty()
		&& updated.kept.is_empty()
		&& updated.skipped.is_empty()
		&& updated.waiting.is_empty()
		&& updated.held.is_empty()
		&& updated.sealed.is_empty()
		&& clones.is_empty();
	if nothing {
		writeln!(output, "The copy here has the tree as it is")?;
		return Ok(());
	}
	for (from, to) in &updated.moved {
		writeln!(output, "moved     {}, from {}", shown(to), shown(from))?;
	}
	for path in &updated.deleted {
		writeln!(output, "deleted   {}", shown(path))?;
	}
	for path in &updated.written {
		writeln!(output, "written   {}", shown(path))?;
	}

	for path in &updated.merged {
		writeln!(output, "merged    {}, to commit", shown(path))?;
	}
	for path in &updated.conflicts {
		writeln!(output, "conflict  {}", shown(path))?;
	}
	for path in &updated.contested {
		writeln!(
			output,
			"competing {}: deleted on one device while changed on another; a commit keeps it, unless it is deleted first",
			shown(path)
		)?;
	}
	for (path, why) in &updated.kept {
		writeln!(output, "kept      {}: {}", shown(path), shown(why))?;
	}
	for (path, why) in &updated.skipped {
		writeln!(output, "skipped   {}: {}", shown(path), shown(why))?;
	}
	for cut in &updated.waiting {
		writeln!(
			output,
			"waiting   revision {}: {} of {} parts have arrived",
			short(cut.revision),
			cut.made,
			cut.parts
		)?;
	}
	for held in &updated.held {
		writeln!(
			output,
			"held      {}: changed elsewhere, which could expose a secret; written once you agree",
			shown(held)
		)?;
	}
	sealed_text(&updated.sealed, output)?;
	clones_text(clones, output)
}
