//! `nib`, the generic command-line tool for NibKeep.
//!
//! The commands run on an [`Environment`], which gives them randomness, the time, prompts
//! for passphrases and the platform's keystore, so that tests can run every command in the
//! test process. The reasons for the tool's shape are in `docs/Technical-design.md`,
//! section 16.

mod clients;
mod environment;
mod mcp;
mod output;
mod trees;
mod types;

pub use trees::TreeCommand;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::RecoveryCode;
use nibkeep_model::{FieldName, Languages, ParseLanguagesError, Tag, TypeName};
use nibkeep_node::{
	Backup, BackupEdit, DeviceRole, Kept, MoveHistory, MoveSecrets, Needs, NewBackup, NewRule,
	Place, PolicyEdit, at_login,
};
use nibkeep_node::{Checkout, Dating, Edit, EntityView, LanguagesFor, NewLibrary};
use nibkeep_node::{
	Holding, KeysError, Library, MemberRole, NewProtection, NodeError, PairingCode, Protection,
	Replica, Role, SecretsState, Unlocker,
};
use nibkeep_std::{FILE, PHOTO, VIDEO};
use nibkeep_time::{Day, Edtf};

pub use environment::{
	Account, Environment, Firewall, System, clear_git_variables, platform_keystore,
};
pub use mcp::McpServer;
use nibkeep_node::pointer::{
	self, Found, LIBRARY_DATA, PointerError, holds_library,
	resolved_as_far_as_it_exists as resolved,
};

/// The service name under which the platform's keystore holds the keys of libraries.
use nibkeep_node::KEYSTORE_SERVICE;

/// The name of the replica that `nib` keeps, after the app.
const APP: &str = "nib";

/// What is wrong with a `.nib` file, as nib says it.
impl From<PointerError> for CliError {
	fn from(error: PointerError) -> Self {
		match error {
			PointerError::Bad { file, why } => Self::BadPointer { file, why },
			PointerError::Stray { file, library } => Self::StrayPointer { file, library },
			PointerError::InTheWay(file) => Self::PointerInTheWay(file),
			PointerError::NotWritten { file, error } => Self::PointerNotWritten { file, error },
			PointerError::NotTakenAway { file, error } => Self::PointerNotTakenAway { file, error },
			PointerError::Library(error) => error.into(),
		}
	}
}

/// nib: structured data and files, on your own devices.
#[derive(Debug, Parser)]
// The name, not the program's file, in usage lines: `nib.exe` on Windows.
#[command(name = "nib", bin_name = "nib", version = env!("NIB_VERSION"), about)]
pub struct Cli {
	/// The library to work in: a folder that holds one in its folder `.nib`. Without it,
	/// the library whose folder the current folder is, or is in; or that the `.nib` file of
	/// the tree whose folder it is, or is in, names; or else, outside every library's folder,
	/// the one that the environment variable `NIBKEEP_LIBRARY` names.
	#[arg(long, global = true, value_name = "FOLDER")]
	pub library: Option<PathBuf>,
	/// The space to add to, and to look for entities in: by its name or the start of its ID,
	/// as nib space list shows them. Without it, new entities go to your personal space,
	/// and an entity is found in whichever space holds it.
	#[arg(long, global = true, value_name = "SPACE")]
	pub space: Option<String>,
	/// Prints JSON instead of text, for scripts.
	#[arg(long, global = true)]
	pub json: bool,
	/// Prints to standard error how long the command's parts took: unlocking the keys,
	/// opening the spaces, the work and printing. For measuring nib, so not listed.
	#[arg(long, global = true, hide = true)]
	pub timings: bool,
	/// Reads the library's passphrase from the first line of this file instead of asking for
	/// it, for scripts. Nobody else may be able to read the file. The passphrase of the
	/// library's secrets is asked for all the same.
	#[arg(
		long,
		global = true,
		value_name = "FILE",
		env = "NIBKEEP_PASSPHRASE_FILE"
	)]
	pub passphrase_file: Option<PathBuf>,
	/// What to do.
	#[command(subcommand)]
	pub command: Command,
}

impl Cli {
	/// How the endpoint of nib starts: on the home network, taking connections at the port
	/// that the command asks for, or only making them, from any free port.
	fn network(&self, accept: bool) -> nibkeep_node::NetworkOptions {
		let port = match &self.command {
			Command::Sync { port, .. } if accept => *port,
			Command::Device {
				command: DeviceCommand::Pair { port, .. },
			}
			| Command::Space {
				command: SpaceCommand::Invite { port, .. },
			} if accept => port.port,
			_ => 0,
		};
		nibkeep_node::NetworkOptions {
			discovery: true,
			port: (port != 0).then_some(port),
			accept,
		}
	}
}

/// The UDP port that a command takes connections on.
#[derive(Debug, Args)]
pub struct Port {
	/// The UDP port to take connections on, for a firewall to let through: the first free of
	/// the ten from it, then any; any with 0.
	#[arg(
		long,
		value_name = "PORT",
		env = "NIBKEEP_PORT",
		default_value_t = nibkeep_node::DEFAULT_PORT
	)]
	pub port: u16,
}

/// The commands that `nib --help` lists, by what they work on, in place of clap's one list.
const OVERVIEW: &[(&str, &[&str])] = &[
	(
		"Library",
		&[
			"init",
			"status",
			"verify",
			"languages",
			"protect",
			"export",
			"import",
		],
	),
	(
		"Entities",
		&[
			"new", "show", "edit", "list", "find", "history", "merge", "move", "purge", "type",
		],
	),
	(
		"Trees",
		&[
			"commit", "mv", "rm", "log", "diff", "blame", "restore", "revert", "update", "pull",
			"clone",
		],
	),
	("Files and secrets", &["file", "policy", "secret"]),
	("Devices and sharing", &["sync", "device", "space", "mcp"]),
	("Safekeeping", &["backup", "recovery", "purges"]),
];

/// `nib`'s commands as clap knows them, with `nib --help` listing them by what they work on,
/// and the other names that some answer to.
#[must_use]
pub fn command() -> clap::Command {
	use clap::CommandFactory;
	use std::fmt::Write as _;
	let command = Cli::command();
	let styles = command.get_styles();
	let (header, literal) = (styles.get_header(), styles.get_literal());
	let width = OVERVIEW
		.iter()
		.flat_map(|(_, names)| names.iter())
		.map(|name| name.len())
		.max()
		.unwrap_or(0);
	let mut overview = String::new();
	let mut aliases = Vec::new();
	for (heading, names) in OVERVIEW {
		let _ = writeln!(overview, "{header}{heading}:{header:#}");
		for name in *names {
			let about = command
				.find_subcommand(name)
				.and_then(clap::Command::get_about)
				.map_or_else(String::new, ToString::to_string);
			let _ = writeln!(overview, "  {literal}{name:width$}{literal:#}  {about}");
			if let Some(sub) = command.find_subcommand(name) {
				aliases.extend(
					sub.get_visible_aliases()
						.map(|alias| format!("{alias} for {name}")),
				);
			}
		}
		overview.push('\n');
	}
	if !aliases.is_empty() {
		// Wrapped to the width of a terminal, as the lines above fit it, never within the
		// words for one command.
		let last = aliases.len() - 1;
		let words =
			"Other names, as other version control systems have them:"
				.split(' ')
				.map(str::to_owned)
				.chain(aliases.iter().enumerate().map(|(index, alias)| {
					format!("{alias}{}", if index == last { "." } else { "," })
				}));
		let mut line = String::new();
		for word in words {
			if !line.is_empty() && line.len() + 1 + word.len() > 80 {
				overview.push_str(&line);
				overview.push('\n');
				line.clear();
			}
			if !line.is_empty() {
				line.push(' ');
			}
			line.push_str(&word);
		}
		overview.push_str(&line);
		overview.push('\n');
	}
	overview.push_str("nib help COMMAND, or nib COMMAND --help, tells more of each.\n\n");
	// The overview holds no braces, which the template would take for its own tags.
	let template = format!(
		"{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{overview}{header}Options:{header:#}\n{{options}}{{after-help}}"
	);
	command.help_template(template)
}

/// The environment's wall clock as it moves on: its time now, moved on as the system's wall
/// clock moves, so that a command that waits makes and checks changes at the time they
/// happen, and one that runs for days, through the computer's sleeps, records the times of
/// syncs as the other programs on the replica read theirs.
fn clock<E: Environment>(environment: &E) -> impl Fn() -> u64 + use<E> {
	let system = || {
		SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map_or(0, |since| {
				i128::try_from(since.as_millis()).unwrap_or(i128::MAX)
			})
	};
	let offset = i128::from(environment.now()) - system();
	move || u64::try_from(system() + offset).unwrap_or(0)
}

/// What `nib` can do.
#[derive(Debug, Subcommand)]
pub enum Command {
	/// Makes a library, once on each computer, or a folder in one a tree.
	///
	/// Where no library is found, in the folders it is in, or named by --library or
	/// `NIBKEEP_LIBRARY`, makes the current folder, or the one --library names, a library: you
	/// as its user, named after your account on this computer, as `whoami` prints it, or as
	/// --name says, linked to a person with your full name, if the system knows it, this
	/// device, and a personal space. The library keeps them in the folder `.nib`; the rest of
	/// the folder is yours, for your files. With --join, makes it a copy of a library you have
	/// on another device instead: there, nib device pair shows the code to give. A library is
	/// brought back with nib recovery restore.
	///
	/// In a library's folder, or with the library that --library or `NIBKEEP_LIBRARY` names,
	/// makes the current folder, or the one given, a tree, as `git init` does: NibKeep keeps
	/// every version of its files, by their paths, and nib commit records them. The folder
	/// given may be anywhere: its own library, if it is in one, or else the current folder's,
	/// keeps it. A tree's folder outside the library's gets a file `.nib` that names the
	/// library, by which nib finds it there; for a tree that lost the file, init writes it
	/// again. Its files
	/// become the tree's copies, committed as its first revision: a purge of a file's content
	/// deletes its copy where it was not edited, so it asks first. What `.nibignore` files
	/// name is left out, in Git's syntax for `.gitignore`, and always `.nib`, every `.git`,
	/// and the inside of folders that hold Git repositories. A tree is never inside another,
	/// nor started in a folder that holds a Git repository. Files such as `.env` and private
	/// keys, and what `.nibsecret` files name, go in as secret files, sealed for the secrets,
	/// whose passphrase it asks for, or sets up.
	#[command(group(
		clap::ArgGroup::new("making")
			.args(["name", "join", "device", "passphrase", "languages", "allow_fat"])
			.multiple(true)
	))]
	Init {
		/// The folder to make a tree; without it, the current one.
		#[arg(conflicts_with = "making")]
		folder: Option<PathBuf>,
		/// The message of the tree's first revision.
		#[arg(short, long, conflicts_with = "making")]
		message: Option<String>,
		/// Makes the folder a tree without asking.
		#[arg(long, conflicts_with = "making")]
		yes: bool,
		/// Your own name, such as `Ada Lovelace`, not the library's, which is its folder's: you
		/// become the library's user, linked to a person of that name. Without it, your
		/// account's name, as `whoami` prints it, and your full name for the person, if the
		/// system knows it. Giving it makes a library.
		#[arg(long)]
		name: Option<String>,
		/// This device's name, instead of the computer's. Giving it makes a library.
		#[arg(long)]
		device: Option<String>,
		/// Protects the library's keys with a passphrase instead of the platform's keystore.
		/// Giving it makes a library.
		#[arg(long)]
		passphrase: bool,
		/// The languages you read, ranked, such as `en, *-Latn, ru`: English first, then
		/// anything in the Latin script, then Russian. Without it, the system's. Giving it
		/// makes a library.
		#[arg(
			long,
			value_name = "RANGES",
			value_parser = parse_languages,
			conflicts_with = "join"
		)]
		languages: Option<Languages>,
		/// Joins the library of another device of yours, which shows this code: nib device
		/// pair shows it there. Both devices must be on the same network.
		#[arg(long, value_name = "CODE", value_parser = parse_code, conflicts_with = "name")]
		join: Option<PairingCode>,
		/// With --join, asks the device that shows the code for the library's secrets too,
		/// with a passphrase of their own here, asked for twice before the code is used. It
		/// gives them if its secrets are unlocked, as nib device pair --give-secrets does;
		/// otherwise nib secret give does there later.
		#[arg(long, requires = "join", conflicts_with = "name")]
		secrets: bool,
		/// Keeps the library on this drive although its file system keeps no journal, as
		/// FAT32 and exFAT, which most USB sticks have. There, a drive removed without
		/// ejecting it, or that loses its power, while the library is open, must be checked
		/// for errors before the library is used again: nib says so then. Giving it makes a
		/// library.
		#[arg(long)]
		allow_fat: bool,
	},
	/// Shows or sets the languages that values are shown in.
	///
	/// The languages are ranked, such as `en, *-Latn, ru`. This app's list comes before this
	/// device's, and this device's before yours.
	Languages {
		/// The languages, ranked. Without them, shows the lists.
		#[arg(value_name = "RANGES")]
		ranges: Option<String>,
		/// Sets this device's list, for every app on it, instead of yours.
		#[arg(long, conflicts_with = "app")]
		device: bool,
		/// Sets this app's list instead of yours.
		#[arg(long)]
		app: bool,
		/// Clears the list, so that the next one counts.
		#[arg(long, conflicts_with = "ranges")]
		clear: bool,
	},
	/// Shows what changed in the tree here, or what the library holds.
	///
	/// In a tree's folder, shows what changed in the tree's copy here since it was written or
	/// committed: new, edited, moved and deleted files, and executable bits, and the Git
	/// repositories found, with the work they hold that their remotes lack; and what changed
	/// in the tree elsewhere. Only the files whose size or modification time changed are read.
	/// Elsewhere, or with --library-wide, shows what the library holds.
	#[command(visible_alias = "st")]
	Status {
		/// Shows what the library holds, in a tree's folder too.
		#[arg(long)]
		library_wide: bool,
		/// Lists the tree's secret files instead, which only devices that hold the secrets
		/// read or write, and its ordinary files that look secret, as files committed before
		/// a rule named them, or taken in as ordinary.
		#[arg(long, conflicts_with = "library_wide")]
		secrets: bool,
	},
	/// Checks every change, log file and readable file of the library.
	///
	/// Every blob this device can read is read in full. Fails if anything is wrong.
	Verify {
		/// Checks an export instead: every file against its sums, and what it holds against
		/// what the library held at its moment.
		#[arg(long, value_name = "FOLDER")]
		export: Option<PathBuf>,
		/// Says that the library's drive was checked for errors, as by Windows' Scan and fix
		/// or by fsck, after it was removed without ejecting it, or lost its power, while the
		/// library was open; then verifies the whole library.
		#[arg(long, conflicts_with = "export")]
		drive_checked: bool,
	},
	/// Adds an entity of a type, with its values.
	///
	/// Its values are given as `field=value`, such as `nib new book name="Leviathan Wakes"
	/// published=2011`.
	New {
		/// Its type, or several separated by commas, such as `edition,audiobook`. A name
		/// without a dot is your own type of that name, or else the only known type whose
		/// name ends in it.
		#[arg(value_name = "TYPES")]
		kind: String,
		/// Its values, as `field=value`. Give a field with many values several times.
		values: Vec<String>,
		/// The language of its text, names and files, such as `en` or `uk-Latn`, except for
		/// fields of codes, such as ISBNs, which are in no language.
		#[arg(long, value_name = "TAG", value_parser = parse_tag)]
		language: Option<Tag>,
	},
	/// Shows an entity, found by its ID, a prefix of its ID, or its name.
	Show {
		/// The entity.
		entity: String,
		/// Shows the entity as it was on this day, such as `2000-01-01`, instead of today.
		#[arg(long, value_name = "DAY", value_parser = parse_day)]
		on: Option<Day>,
		/// Shows the entity as the library had recorded it by then, without the changes
		/// made since: a day, such as `2026-10-10`, which means its end, or a moment, such
		/// as `2026-10-10T20:00`, in local time unless it gives its offset from UTC. Without
		/// --on, as it was on that day.
		#[arg(long, value_name = "WHEN", value_parser = parse_moment)]
		as_of: Option<Moment>,
	},
	/// Moves an entity to another space, such as one you share.
	///
	/// A new entity there gets its values, with their history as you carry it, its types and
	/// its files, and links to it lead there. A folder or a list moves with all it holds,
	/// through every level, which nib lists first. The old space keeps the history it had, for
	/// those who can read it.
	Move {
		/// The entity, by its ID, the start of its ID, or its name.
		entity: String,
		/// The space to move it to, by its name or the start of its ID.
		#[arg(long, value_name = "SPACE")]
		to: String,
		/// Brings its current values alone, and leaves its history in the old space.
		#[arg(long)]
		without_history: bool,
		/// Keeps what it leaves in the old space. Otherwise that is purged once another
		/// device, yours or another member's, holds what moved: what your role there lets you
		/// purge, and the rest once an admin of that space purges it.
		#[arg(long)]
		keep_behind: bool,
		/// Leaves its secret values, such as passwords, with it in the old space, where no
		/// purge of what stays behind takes them. Otherwise they move too, which needs the
		/// secrets of both spaces, unlocked as it moves.
		#[arg(long)]
		leave_secrets: bool,
		/// Moves a folder or a list with all it holds, or an entity out of the folders and
		/// lists that stay, which nib otherwise only lists.
		#[arg(long)]
		yes: bool,
	},
	/// Merges an entity into another that it turns out to be the same as.
	///
	/// The other shows what both say from then on, and references to either lead to it. With
	/// --undo, sets apart again an entity merged into ENTITY: the only one, or the one given.
	Merge {
		/// The entity.
		entity: String,
		/// The entity to merge it into, or with --undo, the entity merged into it to set
		/// apart, by its ID, a prefix of its ID, or its own name.
		#[arg(required_unless_present = "undo")]
		other: Option<String>,
		/// Undoes a merge.
		#[arg(long)]
		undo: bool,
	},
	/// Exports the library into a folder, as JSON, files and their sums.
	///
	/// The folder, empty or new, gets the library's entities as JSON, the files they refer to
	/// as they were added, and the SHA-256 sums of them all, which read without NibKeep. The
	/// folder is not encrypted, and secret values are left out.
	Export {
		/// The folder.
		folder: PathBuf,
		/// Only the values entities have now, without their history, who recorded them or
		/// when, without deleted entities and the records of users and devices: for giving
		/// a collection to someone.
		#[arg(long)]
		current: bool,
	},
	/// Imports an export of a library, or Google Takeout's photos.
	///
	/// A folder that nib export made brings what it holds and the library lacks, with the
	/// same IDs, so that importing it again adds nothing. Its records of users, devices,
	/// replicas and spaces are left out, and an export whose sums do not match is refused.
	///
	/// Anything else is taken for Google Takeout's archive of Google Photos: its parts, zip
	/// or tgz files, all at once, or the folder one was unpacked into, read where they are.
	/// Each photo and video comes with what its JSON file says, when and where it was taken
	/// and its description, in place of what it says of itself, as that is where corrections
	/// made in Google Photos are; its name as uploaded; and in a folder for each album it is
	/// in, inside a folder named after the source. A photo the library holds already, as from
	/// a phone, is stored once, found in one more place. Other products of the archive, as
	/// Drive, are left out. Imported again, what an import added is not read again, so one
	/// cut short goes on where it stopped.
	Import {
		/// The folder of the export; or the parts of Takeout's archive, or the folder it was
		/// unpacked into.
		#[arg(required = true, value_name = "PATH")]
		paths: Vec<PathBuf>,
		/// The source of Takeout's photos, by a name of yours: `Google Photos` unless given,
		/// such as `Google Photos (Mom)`.
		#[arg(long, value_name = "SOURCE")]
		from: Option<String>,
	},
	/// Shows every value an entity has had, or one field's.
	///
	/// Each value comes with when it was recorded, by whom, and when it was replaced.
	History {
		/// The entity.
		entity: String,
		/// The field.
		field: Option<String>,
	},
	/// Changes an entity's values or types.
	///
	/// `field=value` sets a field, correcting the value it shows;
	/// `field+=value` adds a value, which with `--date` records a change, such as a new
	/// employer, that ends the value before it in a field with one value; `field-=value`
	/// removes a value; and `field=` clears the field. `type+=TYPE` gives the entity
	/// another type, and `type-=TYPE` takes one away; its values stay. Where a type of the
	/// entity has a field named `type`, `org.nibkeep.type+=` and `-=` change the types.
	Edit(EditArgs),
	/// Purges values for good, everywhere, with their history.
	///
	/// `field=value` purges that value of an entity, current or past, and `field=` every
	/// value the field has held. What else the changes that held them held stays, and what
	/// they replaced stays replaced. Without --yes, it says what it would purge, and purges
	/// nothing. With --behind and no entity, lists what moves left behind and asked you to
	/// purge, as an admin of its space.
	Purge {
		/// The entity.
		#[arg(required_unless_present = "behind")]
		entity: Option<String>,
		/// What to purge, as `field=value` or `field=`.
		#[arg(required_unless_present = "behind")]
		value: Option<String>,
		/// Purges what a move left behind of the entity, as its mover asked: as an admin of
		/// its space, all but the marks that lead to where it is now; as an editor, the
		/// changes you signed.
		#[arg(long, conflicts_with = "value")]
		behind: bool,
		/// Purges for good, which cannot be undone.
		#[arg(long)]
		yes: bool,
	},
	/// Finds the entities whose values hold every word.
	///
	/// The last word also matches the start of a longer one. Then, through links, what they
	/// lead to: the books of an author they
	/// are, a pen name they use, and the books credited to it. Lists the first 50 of each
	/// space and says how many more there are.
	Find {
		/// The words.
		#[arg(required = true)]
		words: Vec<String>,
		/// Lists every entity found, not only the first 50.
		#[arg(long)]
		all: bool,
	},
	/// Lists the entities, or those of a type.
	///
	/// They are listed by name or in the order of a field, such as `nib list book --where
	/// in-series="The Expanse" --sort number-in-series`. A field is
	/// one of the type's, or else of the other types that its entities have, as an audiobook
	/// can be an edition too; which one a short name finds can change with the entities, so
	/// scripts give full names, such as `std.published`.
	List {
		/// The type.
		#[arg(value_name = "TYPE")]
		kind: Option<String>,
		/// Lists only the entities whose field shows this value today. A reference is given
		/// by the ID, the start of the ID or the name of what it refers to. Give it several
		/// times for entities that meet every condition.
		#[arg(long = "where", value_name = "FIELD=VALUE")]
		conditions: Vec<String>,
		/// Orders the entities by the first value the field shows: numbers by size, dates
		/// by when they start, text and names alphabetically, references by the names of
		/// what they refer to. Entities without a value come last.
		#[arg(long, value_name = "FIELD")]
		sort: Option<String>,
		/// Lists the entities as they were on this day, such as `2000-01-01`, instead of
		/// today: by the names they had then, and the values their fields showed then.
		#[arg(long, value_name = "DAY", value_parser = parse_day)]
		on: Option<Day>,
	},
	/// Works with types: the fields that entities of a type may have.
	Type {
		/// What to do with types.
		#[command(subcommand)]
		command: TypeCommand,
	},
	/// Works with files, each stored once with every place it was found.
	///
	/// A file's content is stored once, however often it is added.
	File {
		/// What to do with files.
		#[command(subcommand)]
		command: FileCommand,
	},
	/// The commands of trees, named as other version control systems name theirs.
	#[command(flatten)]
	Trees(TreeCommand),
	/// Protects the library's keys, or its secrets, anew.
	///
	/// The keys go to the platform's keystore, or with --passphrase, under a new passphrase;
	/// with --secrets, the library's secrets do instead, after their passphrase. The library's
	/// passphrase and its secrets' are never the same.
	Protect {
		/// A new passphrase, asked for twice, instead of the platform's keystore.
		#[arg(long)]
		passphrase: bool,
		/// Protects the library's secrets, not its keys.
		#[arg(long)]
		secrets: bool,
	},
	/// Works with secret values, such as passwords, and secret files.
	///
	/// No other command shows or takes secret values. Each asks for the passphrase of the
	/// library's secrets, which is not the library's, and forgets the secrets when it ends;
	/// nib secret generate needs no library.
	Secret {
		/// What to do with secret values.
		#[command(subcommand)]
		command: SecretCommand,
	},
	/// Works with storage policies: how many copies files need, and where.
	///
	/// A policy says how many copies the files of the personal space need, and whether this
	/// device keeps them in NibKeep's store or where they are. Rules choose a file's policy in
	/// order, unless the file has a policy of its own. The first command gives the space the
	/// default policies, Critical, Standard and Replaceable.
	Policy {
		/// What to do with policies.
		#[command(subcommand)]
		command: PolicyCommand,
	},
	/// Works with spaces, the parts of the library shared as a whole.
	///
	/// Each space holds its own entities, and is shared, or not, as a whole. Your personal
	/// space is never shared.
	Space {
		/// What to do with spaces.
		#[command(subcommand)]
		command: SpaceCommand,
	},
	/// Works with your devices, which sync over the home network.
	///
	/// Each device holds copies of the library, which sync with each other.
	Device {
		/// What to do with devices.
		#[command(subcommand)]
		command: DeviceCommand,
	},
	/// Works with your backups on drives and network shares.
	///
	/// A backup is a folder on a USB drive, a drive kept away from home, or a network share,
	/// which your computers keep up to date whenever they find it, such as when the drive is
	/// plugged in. It holds the library's data encrypted, and no keys: whoever finds it reads
	/// nothing, and you restore from it on a device of yours.
	Backup {
		/// What to do with backups.
		#[command(subcommand)]
		command: BackupCommand,
	},
	/// Works with recovery codes, and brings a library back.
	///
	/// A recovery code is on paper, and brings the library back when every device is lost,
	/// from a backup, or when this computer's library no longer unlocks. It holds keys and no
	/// data. Whoever has the code and a backup reads everything, so keep it as safely as a
	/// passport.
	Recovery {
		/// What to do with recovery codes.
		#[command(subcommand)]
		command: RecoveryCommand,
	},
	/// Lists the purges that wait here, and stops one or ends its wait.
	///
	/// A purge waits on this device, and on your backups plugged in here, before what it
	/// destroyed goes for good. An archive node keeps it apart, unreadable to everyone, for
	/// a while after it learns of a purge, 30 days unless marked otherwise (nib device mark
	/// --waits), so that no mistake or infected device wipes data everywhere at once.
	/// Meanwhile, a purge can be stopped here, which brings back everywhere what it
	/// destroyed, or its wait ended sooner.
	Purges {
		/// What to do with a purge that waits.
		#[command(subcommand)]
		command: Option<PurgesCommand>,
	},
	/// Syncs the library with your other devices on the home network.
	///
	/// Each gets what the other has, files too. Where a program syncs this device for all, as
	/// nib sync --serve or an app that has the library open does, it asks that one to sync
	/// now, and says what happened. As nib push, it sends your commits, as Git's push does.
	#[command(visible_alias = "push")]
	Sync {
		/// Syncs this device for every program here, until stopped: answers your other
		/// devices, and syncs with them as changes come and they appear on the network.
		/// Where another program does that already, it waits, and takes over when that one
		/// ends.
		#[arg(long, conflicts_with = "wait")]
		serve: bool,
		/// How long to look for your other devices on the network, in seconds; where a
		/// program syncs this device, how much longer than two minutes to wait for it.
		#[arg(long, value_name = "SECONDS", default_value_t = 10)]
		wait: u64,
		/// Syncs this library whenever you are logged in to this computer, with no app open,
		/// or stops: nibkeep-sync, installed beside nib, does it for every library turned on.
		/// It needs the library's keys in the keystore, as nobody is there to type a
		/// passphrase.
		#[arg(long, value_name = "SWITCH", conflicts_with_all = ["serve", "wait"])]
		at_login: Option<Switch>,
		/// With --serve, the UDP port to take connections on, for a firewall to let through:
		/// the first free of the ten from it, then any; any with 0. Without --serve, nib sync
		/// only makes connections.
		#[arg(
			long,
			value_name = "PORT",
			env = "NIBKEEP_PORT",
			default_value_t = nibkeep_node::DEFAULT_PORT
		)]
		port: u16,
	},
	/// Syncs with your other devices, then updates the copy of the tree.
	///
	/// It syncs as nib sync does, then brings the copy of the tree here to the tree as they
	/// left it, as nib update does: what Git's pull does. The copy is brought up to date even
	/// if the sync fails, which it says after.
	Pull {
		/// How long to look for your other devices on the network, in seconds.
		#[arg(long, value_name = "SECONDS", default_value_t = 10)]
		wait: u64,
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
	/// Serves nib to AI clients, and lets them work on your library.
	///
	/// An AI client runs nib mcp serve, on your library where nib mcp enable allowed AI
	/// agents, or on an AI agent's copy of the library, which nib device pair --ai-agent adds.
	#[command(subcommand_required = true, arg_required_else_help = true)]
	Mcp {
		/// What to do.
		#[command(subcommand)]
		action: McpAction,
	},
	/// The commands of other version control systems that nib does not have.
	#[command(flatten)]
	Absent(Absent),
}

/// What `nib mcp` does.
#[derive(Debug, Subcommand)]
pub enum McpAction {
	/// Serves an AI client nib's operations as tools, over standard input.
	///
	/// It serves them by the Model Context Protocol, over standard input and output, until
	/// the input ends: on your library, where nib mcp enable allowed AI agents, as the client,
	/// with a key of its own named after it; or on an AI agent's copy of the library, which
	/// nib device pair --ai-agent adds for an AI agent kept apart. It asks for nothing on a
	/// terminal, so a library protected by a passphrase needs --passphrase-file. On an AI
	/// agent's copy, it syncs with your device that waits with nib sync --serve: when it
	/// starts, before a request if it last synced a minute before, and a few seconds after the
	/// AI agent's changes; on your library, the program that syncs this device sends them on.
	Serve {
		/// The AI client's name, which history names its changes by, instead of the name the
		/// client gives itself.
		#[arg(long, value_name = "NAME")]
		agent: Option<String>,
		/// How long to look for your other devices on the network when it syncs, in seconds.
		#[arg(long, value_name = "SECONDS", default_value_t = 2)]
		wait: u64,
	},
	/// Allows AI agents on your library here, and tells the AI clients.
	///
	/// The AI clients on this computer are told of its server through their own commands,
	/// Claude Code's and Codex's: each client then works on the library with a key of its own,
	/// named after it, in your personal space and never with your secrets. A client's session
	/// that runs already sees the server once it starts anew.
	Enable {
		/// Tells this client alone, such as `claude` or `codex`.
		#[arg(long, value_name = "CLIENT", conflicts_with = "print")]
		client: Option<String>,
		/// Tells no client: prints the server's command line, and its entry for the
		/// `mcpServers` of JSON that most clients take, to paste into any.
		#[arg(long)]
		print: bool,
	},
	/// Withdraws AI agents from your library here.
	///
	/// It revokes the key of every AI client that worked on it, and takes its server out of the
	/// clients that nib mcp enable told.
	Disable {
		/// Takes the server out of this client alone, such as `claude` or `codex`.
		#[arg(long, value_name = "CLIENT")]
		client: Option<String>,
	},
}

/// The commands of other version control systems that nib does not have, each of which
/// says what to do instead, with whatever it is given.
#[derive(Debug, Subcommand)]
pub enum Absent {
	/// Git's add.
	#[command(hide = true)]
	Add(Rest),
	/// Git's checkout.
	#[command(hide = true)]
	Checkout(Rest),
	/// Git's branch.
	#[command(hide = true)]
	Branch(Rest),
	/// Git's tag.
	#[command(hide = true)]
	Tag(Rest),
	/// Git's stash.
	#[command(hide = true)]
	Stash(Rest),
	/// Git's reset.
	#[command(hide = true)]
	Reset(Rest),
	/// Git's rebase.
	#[command(hide = true)]
	Rebase(Rest),
	/// Git's fetch.
	#[command(hide = true)]
	Fetch(Rest),
}

impl Absent {
	/// The command's name, and what to do instead.
	const fn instead(&self) -> (&'static str, &'static str) {
		const NO_BRANCHES: &str = "a tree has no branches or tags: what devices change apart competes, and nib update merges it";
		match self {
			Self::Add(_) => (
				"add",
				"nib commit takes in new files, and leaves out what .nibignore names",
			),
			Self::Checkout(_) => (
				"checkout",
				"nib clone writes a tree into a folder, and nib restore writes a file as it was",
			),
			Self::Branch(_) => ("branch", NO_BRANCHES),
			Self::Tag(_) => ("tag", NO_BRANCHES),
			Self::Rebase(_) => ("rebase", NO_BRANCHES),
			Self::Stash(_) => (
				"stash",
				"a tree has no stash: nib restore writes a file as it was, and keeps your edits beside it",
			),
			Self::Reset(_) => (
				"reset",
				"nib restore writes a file as it was, and nib revert undoes a revision",
			),
			Self::Fetch(_) => (
				"fetch",
				"nib sync brings what your other devices recorded without writing it, and nib pull writes it too",
			),
		}
	}
}

/// Whatever a command that nib does not have is given.
#[derive(Debug, Args)]
pub struct Rest {
	/// What it is given.
	#[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
	rest: Vec<String>,
}

/// On or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Switch {
	/// On.
	On,
	/// Off.
	Off,
}

/// Whether a device takes the spaces made later, or asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum NewSpacesArg {
	/// Takes them, as a computer does.
	Take,
	/// Asks first, as a phone does.
	Ask,
}

/// What `nib space` can do.
#[derive(Debug, Subcommand)]
pub enum SpaceCommand {
	/// Makes a space besides your personal one, or a group.
	///
	/// A space, such as `Family`, reaches those of your other devices that take new
	/// spaces, as computers do, when they sync; a phone asks first, with nib space take,
	/// unless you give it the space with --on. With --group, makes a group, such as
	/// `Household`: a space with members and no entities of its own, which you add to spaces
	/// with nib space add-group, so that whoever you invite to the group joins each of them.
	New {
		/// Its name.
		name: String,
		/// Makes a group.
		#[arg(long)]
		group: bool,
		/// Gives it to that device of yours too, by its name or ID, such as a phone, which
		/// still takes it only once you say so there with nib space take; give it once for
		/// each.
		#[arg(long = "on", value_name = "DEVICE")]
		on: Vec<String>,
	},
	/// Adds a group to a space, or to another group.
	///
	/// You add it as an admin of the space who is a member of the group: its members join
	/// with the lower of the group's role there and their own role in the group, and, added
	/// to a group, join every space that group is in. The group joins once an admin of the
	/// group agrees, at once if you are one.
	AddGroup {
		/// The space or the group, by its name or the start of its ID.
		space: String,
		/// The group, by its name or the start of its ID.
		group: String,
		/// The group's role there: editor or viewer, never admin.
		#[arg(long, value_enum, default_value_t = RoleArg::Editor)]
		role: RoleArg,
	},
	/// Takes a group out of a space, as an admin of the space.
	///
	/// Its members leave the space, unless they are members there some other way.
	RemoveGroup {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The group, by its name or the start of its ID.
		group: String,
	},
	/// Agrees, as an admin of a group, that it join a space.
	///
	/// The space is one that a request in the group names, as nib space show lists them.
	Agree {
		/// The group, by its name or the start of its ID.
		group: String,
		/// The space, by its name in the request or the start of its ID.
		space: String,
	},
	/// Withdraws, as an admin of a group, its agreement to join a space.
	///
	/// It is withdrawn for good: its members leave the space, unless they are members there
	/// some other way.
	Withdraw {
		/// The group, by its name or the start of its ID.
		group: String,
		/// The space, by its name in the request or the start of its ID.
		space: String,
	},
	/// Lists your spaces, and which of your devices hold each.
	///
	/// With how many entities each holds here. Your personal space comes first, then those
	/// on this device, then those on your other devices alone: one given to this device
	/// waits for nib space take, and one that another device of yours asks for, for nib
	/// space allow here.
	List,
	/// Takes a space onto this device.
	///
	/// One given to this device, at once: it fills as it syncs. Any other, such as one made
	/// on another device of yours, or one this device dropped, it asks for: approve it with
	/// nib space allow on a device that holds it.
	Take {
		/// The space, by its name or the start of its ID, as nib space list shows it. Not
		/// named `space`, which the global --space would take, for a space this device holds.
		#[arg(value_name = "SPACE")]
		wanted: String,
	},
	/// Drops a space from this device, which your other devices keep.
	///
	/// Refused while, without this device, the space would be below its storage policies
	/// or on no other device of yours, and while a tree's folder or an edited checkout of it
	/// is here: it says what is missing. Otherwise it waits until your changes there, and
	/// the copies of its files, are on other disks as its policies ask, which the holder of
	/// the sync role, such as nib sync --serve, proves, removing this device's copies a
	/// little at a time; and until another device holds its secrets, where this one does.
	/// Then this device says that it no longer holds the space, and deletes it, its
	/// unedited checkouts too, once another device of yours that holds it has learned so,
	/// after their next syncs. Run it again to see what it waits for; nib space take keeps
	/// the space until then. A space that you left goes at once.
	Drop {
		/// The space, by its name or the start of its ID. Not named `space`, which the global
		/// --space would take.
		#[arg(value_name = "SPACE")]
		wanted: String,
	},
	/// Gives a space to another device of yours that asked for it.
	///
	/// The device takes it as it syncs; one that dropped the space gets it back.
	Allow {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The device, by its name or ID, as nib space list shows it.
		device: String,
	},
	/// Shows a space's members, groups, claims and changes set aside.
	///
	/// For a shared space: its members, with their roles and devices; the groups in it, with
	/// their roles, whether they agreed to join, and their members; the claims to its admin
	/// role, with when each takes effect; and the changes set aside there, which wait for an
	/// admin's review: what members changed before they learned that they had left, been
	/// removed or lost a role, and what their roles never allowed, with the entities each
	/// changes. For a group, also the requests in it that it join spaces, with each space's
	/// name where it is sealed to you, as to the group's admins.
	Show {
		/// The space or the group, by its name or the start of its ID.
		space: String,
	},
	/// Renames a space; your personal space keeps its name.
	Rename {
		/// The space, by its name or the start of its ID.
		space: String,
		/// Its new name.
		name: String,
	},
	/// Invites someone to a space, as an admin of it.
	///
	/// It shows a code to give to nib space join on their device, which then gets the space
	/// and syncs it with yours. The code works once, for ten minutes, and both devices must
	/// be on the same network. It shows who asks to join before they are added, and asks
	/// you to confirm, but with --yes.
	Invite {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The role they get.
		#[arg(long, value_enum, default_value_t = RoleArg::Editor)]
		role: RoleArg,
		/// The person in the space whom they are, such as `Olga` in a family tree, by name or
		/// ID: they are linked to that person once they agree, as they join.
		#[arg(long = "as", value_name = "PERSON")]
		person: Option<String>,
		/// Adds whoever joins with the code, without asking.
		#[arg(long)]
		yes: bool,
		/// The port to take the connection on.
		#[command(flatten)]
		port: Port,
	},
	/// Joins a space that someone invited you to, with their code.
	///
	/// Their device shows the code: this device gets the space, and those of your other
	/// devices that take new spaces get it when they sync. If the invitation names the
	/// person in the space whom you are, it asks whether you are.
	Join {
		/// The code, as nib space invite shows it.
		code: PairingCode,
		/// Agrees, without asking, to be the person whom the invitation names you.
		#[arg(long)]
		yes: bool,
		/// Gives the space to that device of yours too, by its name or ID, as nib space new
		/// --on does; give it once for each.
		#[arg(long = "on", value_name = "DEVICE")]
		on: Vec<String>,
	},
	/// Says whether a space allows AI agents, or sets it as an admin.
	///
	/// An AI agent's copy of the library is listed there, by nib device pair --ai-agent
	/// --in, only while it does. Without on or off, says whether it does.
	AiAgents {
		/// The space, by its name or the start of its ID.
		space: String,
		/// Whether it allows them.
		#[arg(value_enum)]
		switch: Option<Switch>,
	},
	/// Links a member of a space to the person in it whom they are.
	///
	/// Without a person, it unlinks them: yourself, or, as an admin, another member. The
	/// link holds once an admin and the member agree, and history names the member by that
	/// person in the space.
	Link {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID.
		member: String,
		/// The person, by name or ID.
		person: Option<String>,
	},
	/// Leaves a space, keeping what your devices have there, read only.
	///
	/// Your devices get nothing added after; the members who stay give the space a new key.
	/// You come back only if an admin invites you again, or lets you back in if you are in
	/// it through a group, whose member you stay. The only admin makes another an admin
	/// first; the last member's leaving closes the space. Without --yes, it says what
	/// leaving would do.
	Leave {
		/// The space, by its name or the start of its ID.
		space: String,
		/// Leaves.
		#[arg(long)]
		yes: bool,
	},
	/// Removes a member of a space, as an admin of it.
	///
	/// Every device of theirs there keeps what it has, read only, and gets nothing added
	/// after, and the space gets a new key. What they change before they learn of it waits
	/// for an admin's review. A member through a group is kept out of the space, and stays
	/// in the group, until you let them back in. Without --yes, it says what removing would
	/// do.
	Remove {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID, or that of their
		/// record in a group.
		member: String,
		/// Removes them.
		#[arg(long)]
		yes: bool,
	},
	/// Lets a member through a group back in, as an admin of the space.
	///
	/// The space kept them out, as nib space remove does, while they stayed in the group.
	LetIn {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID in the group.
		member: String,
	},
	/// Makes a member through a group a direct member of a space.
	///
	/// An admin of the space does it, such as to make them an admin, with no code: once
	/// they confirm with nib space confirm, their devices list themselves on the new
	/// record, with its role.
	MakeDirect {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID in the group.
		member: String,
		/// Their role as a direct member.
		#[arg(long, value_enum, default_value_t = RoleArg::Editor)]
		role: RoleArg,
	},
	/// Confirms the record that an admin of a space made for you.
	///
	/// As a member through a group, you become a direct member there, with its role, and
	/// your other devices follow.
	Confirm {
		/// The space, by its name or the start of its ID.
		space: String,
	},
	/// Claims the admin role of a space when its admins are gone.
	///
	/// You claim it as a direct member who is an editor there: every member sees the claim,
	/// which takes effect 30 days after another member's device first sees it, unless an
	/// admin rejects it, and for 30 days after that you can only undo. Without --yes, it
	/// says what claiming would do.
	Claim {
		/// The space, by its name or the start of its ID.
		space: String,
		/// Claims the role.
		#[arg(long)]
		yes: bool,
	},
	/// Rejects a member's claim to the admin role, as an admin.
	///
	/// The claim,
	/// and what they did as an admin, count for nothing once another member's device sees
	/// the rejection, if that is within 30 days after the claim took effect.
	Reject {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID.
		member: String,
	},
	/// Accepts a member's changes that were set aside in a space.
	///
	/// As an admin of the space, you accept them: they count after all, with their own
	/// times. Only those that the member's role allowed when they made them are accepted;
	/// changes that say who may write to the space never are. Without --yes, it says how
	/// many it would accept.
	Accept {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID.
		member: String,
		/// Accepts them.
		#[arg(long)]
		yes: bool,
	},
	/// Gives a member of a space a role, as an admin of it.
	Role {
		/// The space, by its name or the start of its ID.
		space: String,
		/// The member, by their name or the start of their record's ID.
		member: String,
		/// The role they get.
		#[arg(value_enum)]
		role: RoleArg,
	},
}

/// A role in a shared space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RoleArg {
	/// Sees everything, and changes only their own records.
	Viewer,
	/// Also changes everything but other members' records.
	Editor,
	/// May do anything, as invite others and give roles.
	Admin,
}

impl RoleArg {
	/// The role it names.
	const fn role(self) -> MemberRole {
		match self {
			Self::Viewer => MemberRole::Viewer,
			Self::Editor => MemberRole::Editor,
			Self::Admin => MemberRole::Admin,
		}
	}
}

/// What `nib purges` can do.
#[derive(Debug, Subcommand)]
pub enum PurgesCommand {
	/// Stops a purge that waits here, bringing back what it destroyed.
	///
	/// The purge waits on this device, or on a backup plugged in here. What it destroyed
	/// comes back everywhere, as it was, with its own authors and times, through a change
	/// that every device takes. An admin may stop any purge, and an editor one of what
	/// their own devices wrote. Without --yes, it says what it would bring back.
	Stop {
		/// The purge, by the start of its ID, as nib purges shows it.
		purge: String,
		/// Only where it waits on this backup, by its name or ID, or `here` on this device;
		/// without it, everywhere it waits here.
		#[arg(long, value_name = "BACKUP")]
		on: Option<String>,
		/// Stops it.
		#[arg(long)]
		yes: bool,
	},
	/// Ends the wait of a purge here, destroying now what it kept.
	///
	/// The purge waits on this device, or on a backup plugged in here, which keeps what the
	/// purge destroyed everywhere else; ending the wait destroys it now, as for a photo that
	/// someone asked to have removed. Without --yes, it says what would go.
	End {
		/// The purge, by the start of its ID, as nib purges shows it.
		purge: String,
		/// Only where it waits on this backup, by its name or ID, or `here` on this device;
		/// without it, everywhere it waits here.
		#[arg(long, value_name = "BACKUP")]
		on: Option<String>,
		/// Destroys it for good, which cannot be undone.
		#[arg(long)]
		yes: bool,
	},
}

/// What `nib recovery` can do.
#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
	/// Makes a recovery code and shows it, for you to write down.
	///
	/// Write it down or print it. It counts only once you have typed it back, here or with
	/// nib recovery confirm, so that a code written down wrongly is never relied on. It
	/// replaces the codes made before it, which are revoked once every backup carries it.
	New {
		/// Gives the code the secrets too, so that it brings the passwords back: asks for the
		/// secrets' passphrase first.
		#[arg(long)]
		secrets: bool,
		/// Revokes the codes made before it at once, rather than once every backup carries
		/// it: a backup not updated since then brings nothing back.
		#[arg(long)]
		revoke_older: bool,
	},
	/// Takes a code typed from your paper, and has the library name it.
	///
	/// The library names it, if nothing names it yet.
	Confirm {
		/// Gives the code the secrets too, also one that the library names already: asks
		/// for the secrets' passphrase first.
		#[arg(long)]
		secrets: bool,
		/// Revokes the codes made before the newest at once, rather than once every backup
		/// carries it: a backup not updated since then brings nothing back.
		#[arg(long)]
		revoke_older: bool,
	},
	/// Lists your recovery codes, with when and where each was made.
	///
	/// They go by the names printed beside them, with when and on which device each was made.
	List,
	/// Says whether a code that you type stands and opens the keys.
	Check {
		/// Checks that it opens the secrets too: asks for the secrets' passphrase first.
		#[arg(long)]
		secrets: bool,
	},
	/// Revokes a recovery code, for good.
	///
	/// It opens nothing added later, and brings no device back. Shows what it would revoke,
	/// and revokes with --yes.
	Revoke {
		/// The code, by the name printed beside it, or by its ID.
		code: String,
		/// Revokes it.
		#[arg(long)]
		yes: bool,
	},
	/// Brings your library back on this computer with your recovery code.
	///
	/// nib asks for the code, after you lost your devices, or the means to unlock them. The
	/// library comes from the backup or the library in the folder given, or without one,
	/// from the library here, in the current folder or the one --library names, which no
	/// longer unlocks, as after a forgotten passphrase. This computer becomes a new device of
	/// yours, and your other devices are revoked, but those you keep: unless --keep names
	/// them, nib asks about each where it can ask, at a terminal and without --json, and
	/// otherwise revokes every one. A library here that no longer unlocks is replaced, and
	/// its old folder set aside for you to delete; a library elsewhere is left as it was. A
	/// backup must have been updated since the code was made. If the code holds your secrets,
	/// nib asks twice for a passphrase for them here, not the library's.
	Restore {
		/// The backup, or the folder of a library, to bring the library back from.
		#[arg(value_name = "SOURCE")]
		source: Option<PathBuf>,
		/// Keeps a device of yours that you still have, rather than revoke it as lost, by its
		/// name or ID, or an AI agent by its name, as nib device list showed them; give it
		/// once for each. Then nib asks about no other device. The library that no longer
		/// unlocks, which the recovery reads, is revoked in any case.
		#[arg(long, value_name = "DEVICE")]
		keep: Vec<String>,
		/// How the new device is made.
		#[command(flatten)]
		new: NewDevice,
	},
	/// Makes this computer a device of its own from a copy of the library.
	///
	/// The copy is in the current folder, or the one --library names, which a backup, a
	/// program that syncs folders or a person brought from another computer or drive, with
	/// the copy's own keys: only where no other device of yours can add this one, as after
	/// the library of your only device was restored from its backup. The copy's device is
	/// revoked, and the copy stays as it was. Asks for the copy's passphrase, unless this
	/// computer's keystore holds its key, and for its secrets' passphrase, if it has secrets.
	FromCopy {
		/// How the new device is made.
		#[command(flatten)]
		new: NewDevice,
	},
}

/// How `nib recovery` makes this computer a new device.
#[derive(Debug, Args)]
pub struct NewDevice {
	/// This device's name, instead of the computer's.
	#[arg(long)]
	pub device: Option<String>,
	/// Protects the library's keys with a passphrase instead of the platform's keystore.
	#[arg(long)]
	pub passphrase: bool,
	/// Keeps the library on this drive although its file system keeps no journal, as FAT32
	/// and exFAT, which most USB sticks have. There, a drive removed without ejecting it, or
	/// that loses its power, while the library is open, must be checked for errors before the
	/// library is used again: nib says so then.
	#[arg(long)]
	pub allow_fat: bool,
}

/// What `nib backup` can do.
#[derive(Debug, Subcommand)]
pub enum BackupCommand {
	/// Makes a backup in a folder, such as on a USB drive.
	///
	/// The folder, new or empty, is updated now. While nib sync --serve, syncing from login or
	/// an app runs, your computers then update it whenever they find it.
	New {
		/// The folder.
		folder: PathBuf,
		/// Its name, such as `Blue stick`; the folder's name by default.
		#[arg(long)]
		name: Option<String>,
		/// A space that it holds, by its name or the start of its ID; give it once for each.
		/// Every space of the library by default.
		#[arg(long = "of", value_name = "SPACE")]
		spaces: Vec<String>,
		/// A storage policy whose files it keeps, such as `Critical`, by its name in each of
		/// its spaces; give it once for each. Every policy's files by default. The library's
		/// records, thumbnails and secret files are always kept.
		#[arg(long = "policy", value_name = "POLICY")]
		policies: Vec<String>,
		/// Marks it as kept away from home, so that its copies count as copies away from
		/// home.
		#[arg(long)]
		away: bool,
		/// How many days it waits, from 1 to 365, before what a purge destroys leaves it,
		/// counted from the update that first brings the purge, during which nib purges on a
		/// computer it is plugged into can stop the purge; `none` for not at all, `default`
		/// for 30.
		#[arg(long, value_name = "DAYS", value_parser = parse_waits)]
		waits: Option<Waits>,
	},
	/// Lists your backups, with what they hold and still have to check.
	///
	/// Each comes with where this computer finds it, how many updates it has had and files it
	/// holds, how much of it is still to check, and how many copies were found damaged lately.
	List,
	/// Updates a backup now, or each that this computer finds.
	///
	/// An update takes in what this device lacks, writes what the backup lacks, and checks its
	/// files.
	Update {
		/// The backup, by its name or ID.
		backup: Option<String>,
		/// Where it is, if this computer does not find it on its own.
		#[arg(long, value_name = "FOLDER")]
		at: Option<PathBuf>,
	},
	/// Changes a backup: its name, spaces, policies, and where it is kept.
	///
	/// It is kept at home or away. Taking a space or a policy off stops writing it; what the
	/// backup holds of it stays.
	Edit {
		/// The backup, by its name or ID.
		backup: String,
		/// Its new name.
		#[arg(long)]
		name: Option<String>,
		/// The spaces that it holds from now on; give it once for each.
		#[arg(long = "of", value_name = "SPACE")]
		spaces: Vec<String>,
		/// The policies whose files it keeps from now on; give it once for each.
		#[arg(
			long = "policy",
			value_name = "POLICY",
			conflicts_with = "every_policy"
		)]
		policies: Vec<String>,
		/// Keeps every policy's files from now on.
		#[arg(long)]
		every_policy: bool,
		/// Marks it as kept away from home.
		#[arg(long, conflicts_with = "home")]
		away: bool,
		/// Marks it as kept at home.
		#[arg(long)]
		home: bool,
		/// How many days it waits before purging, from 1 to 365; `none`, or `default` for
		/// 30. A shorter wait, or none, holds only once the wait before it has passed.
		#[arg(long, value_name = "DAYS", value_parser = parse_waits)]
		waits: Option<Waits>,
	},
	/// Forgets a backup, whose folder stays for you to delete.
	///
	/// Your computers stop updating it.
	Remove {
		/// The backup, by its name or ID.
		backup: String,
	},
}

/// What `nib device` can do.
#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
	/// Adds another device of yours, or an AI agent's copy of the library.
	///
	/// It shows a code to give to nib init --join on the device, which then gets a copy of
	/// the library. The code works for ten minutes, three wrong tries end it, and both
	/// devices must be on the same network. A computer gets every space of this device's,
	/// and takes the spaces made later; a phone, with --phone, gets your personal space
	/// alone, and asks before it takes a space made later. The new device gets no secrets,
	/// but with --give-secrets.
	Pair {
		/// Adds a copy of the library for an AI agent instead, named after it, such as
		/// `Claude`, which nib mcp serves it: it gets no secrets, and may not purge, add or
		/// revoke devices, or change the library's own records, such as types and storage
		/// policies. Run nib init --join where nib mcp will run.
		#[arg(long, value_name = "NAME")]
		ai_agent: Option<String>,
		/// Adds a phone: it gets your personal space alone, or the spaces given with --in,
		/// and asks before it takes a space made later, with nib space take there.
		#[arg(long, conflicts_with = "ai_agent")]
		phone: bool,
		/// A space besides your personal one that the new device gets, by its name or the
		/// start of its ID, instead of the default, every space for a computer and none for a
		/// phone; give it once for each. For an AI agent, a space that it may read and write
		/// too: a shared space must allow AI agents, which its admins say with nib space
		/// ai-agents; an admin's device allows them here at once. Pair the AI agent anew to
		/// change its spaces.
		#[arg(long = "in", value_name = "SPACE")]
		spaces: Vec<String>,
		/// Gives the new device the library's secrets too, if it asks for them, as nib init
		/// --join --secrets does: asks for the passphrase of the secrets here before it shows
		/// the code.
		#[arg(long, conflicts_with = "ai_agent")]
		give_secrets: bool,
		/// The port to take the connection on.
		#[command(flatten)]
		port: Port,
	},
	/// Lists your devices: the copies of the library on them.
	///
	/// Each copy is kept by an app; those revoked are listed too, with how many of their
	/// changes wait for review.
	List,
	/// Revokes a device of yours, such as a lost phone.
	///
	/// It revokes every copy of the library on the device, or one copy by its ID: no device
	/// syncs with it any more, what it changed that this device does not have is set aside
	/// for review, and the library gets a new key that it does not get. Revoking is final.
	/// Without --yes, it says what it would revoke, and revokes nothing.
	Revoke {
		/// The device, by its name or ID, one copy of the library by its ID, or an AI
		/// agent by its name, as nib device list shows them.
		device: String,
		/// Revokes for good, which cannot be undone.
		#[arg(long)]
		yes: bool,
		/// Revokes too the recovery codes made on it, which it names: whoever has the device
		/// may have read them as they were made. Without it, they stay.
		#[arg(long)]
		codes: bool,
	},
	/// Marks a device as an archive node or everyday, at home or away.
	///
	/// An archive node, such as a NAS or a desktop, keeps
	/// the files that policies assign it, or as an everyday device, as every device is until
	/// marked; and as kept away from home, such as a relative's NAS, or at home. Copies on an
	/// archive node, and away from home, count where a policy asks for them. Your devices
	/// tell the others at their next syncs. Without a mark, it says how the device is marked.
	Mark {
		/// The device, by its name or ID, or one copy of the library on it by its ID.
		device: String,
		/// Marks it as an archive node.
		#[arg(long, conflicts_with = "everyday")]
		archive: bool,
		/// Marks it as an everyday device.
		#[arg(long)]
		everyday: bool,
		/// Marks it as kept away from home.
		#[arg(long, conflicts_with = "home")]
		away: bool,
		/// Marks it as kept at home.
		#[arg(long)]
		home: bool,
		/// How many bytes of files it keeps as an everyday device, such as 20GB: the files
		/// pinned to it, then the newest and those opened lately, while some device is an
		/// archive node; `default` for a tenth of its disk, or nothing on a phone.
		#[arg(long, value_name = "SIZE", value_parser = parse_keeps)]
		keeps: Option<Keeps>,
		/// How many days it waits as an archive node, from 1 to 365, before it destroys what a
		/// purge destroys, during which nib purges there can stop the purge; `none` for not at
		/// all, `default` for 30. A shorter wait, or none, holds only once the wait before it
		/// has passed, so that no infected device can shorten it and purge at once.
		#[arg(long, value_name = "DAYS", value_parser = parse_waits)]
		waits: Option<Waits>,
		/// Whether nib's copy of the library on this device takes the spaces made later on
		/// your other devices, as a computer does, or asks first, as a phone does, with nib
		/// space take: said only on the device itself, so that no other device can say it for
		/// it, and by each app's copy for itself.
		#[arg(long, value_enum, value_name = "take|ask")]
		new_spaces: Option<NewSpacesArg>,
	},
	/// Lists the changes of revoked devices that were set aside.
	///
	/// They were set aside as no device that revoked them had them; so are those of devices
	/// nobody added.
	Review,
	/// Accepts every change of a device that was set aside.
	///
	/// They count after all, with their own times. Changes that say who may write to the
	/// library, such as one that revokes another device, are never accepted. Without --yes,
	/// it says how many it would accept.
	Accept {
		/// The device, by its name or ID, one copy of the library by its ID, or an AI
		/// agent by its name.
		device: String,
		/// Accepts them.
		#[arg(long)]
		yes: bool,
	},
}

/// What `nib edit` takes.
#[derive(Debug, Args)]
pub struct EditArgs {
	/// The entity, found by its ID, a prefix of its ID, or its name.
	entity: String,
	/// The changes.
	#[arg(required_unless_present_any = ["delete", "restore"])]
	changes: Vec<String>,
	/// When the values written were true: a date for something that happened, such as
	/// `2019-03`, or an interval for a state, such as `1990/2015`.
	#[arg(long, value_name = "DATE", value_parser = parse_edtf)]
	date: Option<Edtf>,
	/// Edits the entity as it was on this day, such as `2000-01-01`, instead of today:
	/// setting a field corrects the value it showed then.
	#[arg(long, value_name = "DAY", value_parser = parse_day)]
	on: Option<Day>,
	/// Deletes the entity: hides it and keeps it, until it is restored.
	#[arg(long, conflicts_with = "restore")]
	delete: bool,
	/// Restores a deleted entity.
	#[arg(long)]
	restore: bool,
	/// The language of the text, names and files written, such as `en` or `uk-Latn`,
	/// except for fields of codes, such as ISBNs, which are in no language.
	#[arg(long, value_name = "TAG", value_parser = parse_tag)]
	language: Option<Tag>,
	/// Works on forms, in other languages or scripts, rather than whole values:
	/// `field+=value` adds another form of the only value the field shows, and
	/// `field-=value` removes one form, where it would remove the value with every form.
	#[arg(long, conflicts_with = "form_of")]
	form: bool,
	/// As `--form`, with forms added to this value of the field, given in any of its forms.
	#[arg(long, value_name = "VALUE")]
	form_of: Option<String>,
}

/// What `field+=value` and `field-=value` work on.
#[derive(Clone, Copy, Debug)]
enum Forms<'a> {
	/// Whole values.
	No,
	/// Forms; those added, of the only value the field shows.
	OfTheOnly,
	/// Forms; those added, of this value, given in any of its forms.
	Of(&'a str),
}

/// What `nib type` can do. Names without a dot are your own, in the namespace `local`.
#[derive(Debug, Subcommand)]
pub enum TypeCommand {
	/// Defines a type, or adds fields to it.
	///
	/// Names that start with std. are the standard vocabulary's: a standard type takes fields
	/// of your own, and a type lists a standard field as the vocabulary defines it, but no
	/// new standard name is made.
	Add {
		/// The type's name, such as `book` or `org.example.book`.
		name: String,
		/// Its fields, as `name:kind`, with `:many`, `:lww`, `:secret` or `:role` after the
		/// kind; for quantities `:unit=` and a unit in UCUM, such as `:unit=kg`; for
		/// references `:stands-for`, `:search-through` and `:reverse=` and a field's name; and
		/// for text `:code`, for values in no language, such as ISBNs, which a language
		/// given with nib new or nib edit is not put on. The kinds are text, integer,
		/// boolean, date, reference, blob, decimal, quantity and name.
		fields: Vec<String>,
	},
	/// Shows a type's fields.
	Show {
		/// The type's name.
		name: String,
	},
	/// Lists the types.
	List,
}

/// What `nib file` can do, in the personal space.
#[derive(Debug, Subcommand)]
pub enum FileCommand {
	/// Adds files, and the files in folders.
	///
	/// They are added in the order of their names: each with its name, its content's type
	/// and size, and where it was found, and a photo or a video with when, where and by
	/// which camera it was taken. Content that the library holds already is that file,
	/// found in one more place; a file found before at the same place, with the same size
	/// and modification time, is not read again. In folders, links, hidden files and
	/// folders, checkouts, and the library's own folder are left out. A file that cannot be
	/// read is reported, and the others are added all the same; an error of the library
	/// ends the command. Each photo, video or other picture gets a thumbnail, if nib reads
	/// its format: JPEG, PNG, GIF, WebP, BMP and TIFF.
	///
	/// With --secret, each file becomes a secret file, such as the scan of a passport: sealed
	/// for the secrets, so that only the devices that hold them read it, with its name, type
	/// and size readable by all, and nothing recorded of where it was found. The original
	/// stays where it is, unprotected. The library's first secret sets up its secrets, with
	/// a passphrase of their own.
	Add {
		/// The files and folders; with --secret, files only.
		#[arg(required = true)]
		paths: Vec<PathBuf>,
		/// Adds secret files.
		#[arg(long)]
		secret: bool,
		/// The entity, such as a login, to attach the secret files to; given again, several.
		#[arg(long, value_name = "ENTITY", requires = "secret")]
		to: Vec<String>,
		/// Where the files came from, such as an old phone, a disc or a drive, by a name of
		/// yours, such as `Mom's old phone`: made the first time, and recorded with where each
		/// file was found, with its path there from the folder given on.
		#[arg(long, value_name = "SOURCE", conflicts_with = "secret")]
		from: Option<String>,
	},
	/// Writes a plain copy of a file to open or edit in other apps.
	///
	/// The copy is of the file's current version, recorded as a checkout on this device:
	/// into a folder, under the file's name, or as a new file; without either, into the
	/// current folder. It replaces nothing, and a copy there already, unchanged, stays as
	/// it is. Checking in records its edits; a purge of the file's content deletes it,
	/// unless it was edited.
	Checkout {
		/// The file, by its ID, a prefix of its ID, or its name.
		file: String,
		/// The folder, or the path of the copy.
		path: Option<PathBuf>,
	},
	/// Says which disks hold a copy of a file, and how.
	///
	/// A copy is sealed in a store, with when it was last checked, or as a plain copy,
	/// where it was found or checked out. Copies count disks, as a copy is lost with its
	/// disk. What other devices hold is as their accounts last said, which each gives at
	/// its syncs.
	Where {
		/// The file, by its ID, a prefix of its ID, or its name.
		file: String,
	},
	/// Writes the thumbnail of a file, a JPEG, for scripts and tests.
	///
	/// Not listed beside nib file thumbnails, which most want.
	///
	/// The thumbnail is of the file's current version, the one every device shows, written
	/// into a file. A photo gets its thumbnail from the device
	/// that adds it, if that device reads its format; nib reads JPEG, PNG, GIF, WebP, BMP
	/// and TIFF.
	#[command(hide = true)]
	Thumbnail {
		/// The file, by its ID, a prefix of its ID, or its name.
		file: String,
		/// Where to write the JPEG, which must not exist yet.
		path: PathBuf,
	},
	/// Makes the thumbnails that files lack, from the copies here.
	///
	/// It makes those of the formats nib reads, whichever device added the files: for files
	/// added before thumbnails were made. The newest go first, by when a photo was taken,
	/// else when it was added.
	Thumbnails,
	/// Pins a file to a device of yours, which keeps it whatever its size.
	///
	/// The device is this one unless --device names another; it keeps the file from its next
	/// sync. Pins are yours alone, kept in your
	/// personal space.
	Pin {
		/// The file, by its ID, a prefix of its ID, or its name.
		file: String,
		/// The device, by its name or ID, or one copy of the library on it by its ID.
		#[arg(long)]
		device: Option<String>,
	},
	/// Unpins a file from a device of yours.
	///
	/// The device is this one unless --device names another; it then keeps the file only
	/// while it is among the newest files that fit its size.
	Unpin {
		/// The file, by its ID, a prefix of its ID, or its name.
		file: String,
		/// The device, by its name or ID, or one copy of the library on it by its ID.
		#[arg(long)]
		device: Option<String>,
	},
	/// Ends a checkout: its copy's edits become a new version.
	///
	/// The edits of the copy on this device become a new version of the file,
	/// sealed into the store, and the copy is deleted. A copy that is gone ends its
	/// checkout. Edits of a version that is no longer the file's own are refused, unless
	/// --force records them on top. A new version of a photo gets its own thumbnail.
	Checkin {
		/// The copy, by its path, or the file, if it is checked out in one place.
		copy: String,
		/// Keeps the copy, as a checkout of the new version.
		#[arg(long)]
		keep: bool,
		/// Records the edits of an older version on top of the file's newer one, which
		/// history keeps.
		#[arg(long)]
		force: bool,
	},
}

/// What `nib secret` can do, in the personal space. The entity is found by its ID, a prefix
/// of its ID, or its name.
#[derive(Debug, Subcommand)]
pub enum SecretCommand {
	/// Sets a secret field of an entity, asking for the value.
	///
	/// It replaces the values the field has: asks for the value twice, without showing it,
	/// or with --generate, makes a new password. The library's first secret sets up its
	/// secrets, with a passphrase of their own. The secret of one-time codes,
	/// std.one-time-code, is taken as a site shows it: as base32 text, or as the otpauth://
	/// link of its QR code.
	Set {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
		/// Sets a new random password, as the options below say, without showing it.
		#[arg(long)]
		generate: bool,
		/// What a generated password is made of.
		#[command(flatten)]
		recipe: RecipeArgs,
	},
	/// Copies a secret value to the clipboard, and clears it after a while.
	///
	/// The value of a secret field of an entity is marked so that clipboard histories leave it
	/// out, and clears it after a while, unless something
	/// else was copied since. nib waits until then; Ctrl+C clears it at once. Of values that
	/// compete, it copies the first.
	Copy {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
		/// How many seconds the clipboard holds the secret.
		#[arg(long, value_name = "SECONDS", default_value_t = nibkeep_passwords::CLEAR_AFTER.as_secs())]
		clear_after: u64,
	},
	/// Prints a new random password, which nothing keeps.
	///
	/// For nib secret set, there is --generate.
	Generate {
		/// What the password is made of.
		#[command(flatten)]
		recipe: RecipeArgs,
	},
	/// Shows the one-time code of an entity now.
	///
	/// It says how many seconds the code lasts, made from the secret in a field:
	/// std.one-time-code, unless another is given.
	Code {
		/// The entity.
		entity: String,
		/// The secret field that holds the secret of one-time codes.
		field: Option<String>,
	},
	/// Shows the values of a secret field of an entity.
	Show {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
	},
	/// Shows every value a secret field of an entity has had.
	///
	/// It shows them as `nib history` does.
	History {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
	},
	/// Clears a secret field of an entity, keeping its history.
	///
	/// It shows no value from now on, and its history keeps those it had.
	Clear {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
	},
	/// Purges one value of a secret field for good, everywhere.
	///
	/// The value of the entity's field, current or past, is asked for without showing it.
	/// Without --yes, it says what it would purge, and purges nothing.
	Purge {
		/// The entity.
		entity: String,
		/// The secret field.
		field: String,
		/// Purges for good, which cannot be undone.
		#[arg(long)]
		yes: bool,
	},
	/// Asks another of your devices for the library's secrets.
	///
	/// The device that holds them is named by its name, or one copy of the library on it,
	/// by its ID, as nib device list shows them: sets up the secrets here, with a
	/// passphrase of their own, asked for twice, and never the library's. That device gives
	/// them, with nib secret give; they arrive with the next sync, and this device takes
	/// them when its secrets are next unlocked. It takes them from that device alone. A
	/// device that asked already asks another this way, without a passphrase. nib protect
	/// --secrets moves them to the keystore. With --space naming a shared space, asks
	/// another member's device there for that space's secrets, by its name, or its ID as
	/// nib space show --json shows it; this device's secrets must be set up, and your other
	/// devices take the space's from it by themselves.
	Ask {
		/// The device that holds the secrets, or the ID of a copy of the library on it.
		from: String,
	},
	/// Gives the library's secrets to a device of yours that asked.
	///
	/// The device is named by its name, or one copy of the library on it, by its ID, as nib
	/// device list shows them. Asks for the passphrase of the secrets here; they reach it
	/// with the next sync. An AI agent never gets them. With --space naming a shared space,
	/// gives that space's secrets to another member's device there that asked this one, by
	/// its name or ID; your own devices get them by themselves.
	Give {
		/// The device, or the ID of a copy of the library on it.
		device: String,
	},
	/// Takes the secrets from a copy of the library beside this one.
	///
	/// The copy is beside this device's, which a backup, a program that syncs folders or a
	/// person brought from another device: only the keys that the copy's device was given.
	/// Asks for the passphrase of the copy's secrets, unless this computer's keystore holds
	/// their key, and for a passphrase of the secrets here, twice. Take them before
	/// revoking the copy's device, if it alone held them.
	FromCopy,
	/// Saves a plain copy of a secret file, which nothing records.
	///
	/// NibKeep neither protects it, nor keeps track of it, nor deletes it if the file is
	/// purged. Into a folder, under the file's name, or as a new file; without either, into
	/// the current folder. It replaces nothing.
	Save {
		/// The secret file.
		file: String,
		/// The folder, or the path of the copy.
		path: Option<PathBuf>,
	},
}

/// What a generated password is made of: twenty characters of every kind, unless these
/// say otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "one flag for each kind of character, as nibkeep_passwords::Recipe has"
)]
pub struct RecipeArgs {
	/// How many characters the password has, from 4 to 1024; 20 unless given.
	#[arg(long)]
	pub length: Option<usize>,
	/// Leaves out lowercase letters.
	#[arg(long)]
	pub no_lowercase: bool,
	/// Leaves out uppercase letters.
	#[arg(long)]
	pub no_uppercase: bool,
	/// Leaves out digits.
	#[arg(long)]
	pub no_digits: bool,
	/// Leaves out symbols.
	#[arg(long)]
	pub no_symbols: bool,
	/// Leaves out characters that look like others, such as 0 and O, or 1 and l.
	#[arg(long)]
	pub unambiguous: bool,
}

impl RecipeArgs {
	/// The recipe that these options ask for.
	fn recipe(&self) -> nibkeep_passwords::Recipe {
		let default = nibkeep_passwords::Recipe::default();
		nibkeep_passwords::Recipe {
			length: self.length.unwrap_or(default.length),
			lowercase: !self.no_lowercase,
			uppercase: !self.no_uppercase,
			digits: !self.no_digits,
			symbols: !self.no_symbols,
			unambiguous: self.unambiguous,
		}
	}

	/// Whether any of these options was given.
	fn given(&self) -> bool {
		self.length.is_some()
			|| self.no_lowercase
			|| self.no_uppercase
			|| self.no_digits
			|| self.no_symbols
			|| self.unambiguous
	}
}

/// What `nib policy` can do, in the personal space. A policy is found by its name, its ID or
/// a prefix of its ID; a rule by its number in `nib policy list`, its ID or a prefix of it.
#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
	/// Lists the policies, and the rules in the order in which they choose.
	List,
	/// Shows a policy, and the rules that choose it.
	Show {
		/// The policy.
		policy: String,
	},
	/// Makes a policy.
	New {
		/// Its name.
		name: String,
		/// How many copies its files need.
		#[arg(long, default_value_t = 1)]
		copies: u32,
		/// How many of the copies are outside the home.
		#[arg(long, default_value_t = 0)]
		away: u32,
		/// How many of the copies are on archive nodes.
		#[arg(long, default_value_t = 0)]
		archived: u32,
		/// Keeps its files where they are, rather than in NibKeep's store.
		#[arg(long)]
		in_place: bool,
	},
	/// Changes a policy.
	Edit {
		/// The policy.
		policy: String,
		/// Its new name.
		#[arg(long)]
		name: Option<String>,
		/// How many copies its files need.
		#[arg(long)]
		copies: Option<u32>,
		/// How many of the copies are outside the home.
		#[arg(long)]
		away: Option<u32>,
		/// How many of the copies are on archive nodes.
		#[arg(long)]
		archived: Option<u32>,
		/// Keeps its files where they are.
		#[arg(long, conflicts_with = "store")]
		in_place: bool,
		/// Keeps its files in NibKeep's store.
		#[arg(long)]
		store: bool,
	},
	/// Removes a policy; its rules choose nothing.
	///
	/// The rules that choose it choose nothing, and the files that have it as their own get
	/// theirs from the rules.
	Remove {
		/// The policy.
		policy: String,
	},
	/// Works with the rules that choose a file's policy.
	Rule {
		/// What to do with rules.
		#[command(subcommand)]
		command: RuleCommand,
	},
	/// Gives a file a policy of its own, which wins over the rules.
	///
	/// With --clear, lets the rules choose again.
	Set {
		/// The file, or any entity.
		entity: String,
		/// The policy.
		#[arg(required_unless_present = "clear")]
		policy: Option<String>,
		/// Clears the file's own policy.
		#[arg(long, conflicts_with = "policy")]
		clear: bool,
	},
	/// Tells which policy a file has, and what chose it.
	Why {
		/// The file.
		entity: String,
	},
	/// Tells, for each policy, how many files it has, and which lack copies.
	///
	/// With one device, everything on it counts as one copy.
	Check,
}

/// What `nib policy rule` can do.
#[derive(Debug, Subcommand)]
pub enum RuleCommand {
	/// Makes a rule: the files that meet every condition get the policy.
	///
	/// Without a place, it goes before the rule for any other file.
	New {
		/// The policy it chooses.
		policy: String,
		/// The file has this type, such as `std.photo`.
		#[arg(long = "type", value_name = "TYPE")]
		of_type: Option<String>,
		/// The file has a value of this field, such as `std.camera-make`.
		#[arg(long, value_name = "FIELD")]
		has: Option<String>,
		/// Every place the file was found is in this folder of this device, or below it.
		#[arg(long, value_name = "FOLDER")]
		folder: Option<PathBuf>,
		/// The file's media type is this one or starts with it, such as `video/`.
		#[arg(long, value_name = "TYPE")]
		media_type: Option<String>,
		/// The file is larger than this, such as `500MB` or `2GiB`.
		#[arg(long, value_name = "SIZE", value_parser = parse_size)]
		larger_than: Option<u64>,
		/// Adding a larger file that the rule chooses for asks whether it can be got again.
		#[arg(long, value_name = "SIZE", value_parser = parse_size)]
		asks_above: Option<u64>,
		/// Where the rule goes.
		#[command(flatten)]
		place: PlaceArgs,
	},
	/// Removes a rule.
	Remove {
		/// The rule.
		rule: String,
	},
	/// Moves a rule.
	Move {
		/// The rule.
		rule: String,
		/// Where it goes.
		#[command(flatten)]
		place: PlaceArgs,
	},
}

/// Where a rule goes among the others.
#[derive(Debug, Args)]
pub struct PlaceArgs {
	/// First, where it wins over every other rule.
	#[arg(long, conflicts_with_all = ["last", "before", "after"])]
	first: bool,
	/// Last.
	#[arg(long, conflicts_with_all = ["before", "after"])]
	last: bool,
	/// Just before this rule.
	#[arg(long, value_name = "RULE", conflicts_with = "after")]
	before: Option<String>,
	/// Just after this rule.
	#[arg(long, value_name = "RULE")]
	after: Option<String>,
}

/// How many of the entities that link to an entity through one field `nib show` lists.
const LINKS_SHOWN: usize = 10;

/// Why a command failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CliError {
	/// An archive to import could not be read, or held nothing to import.
	#[error("{0}")]
	Import(String),
	/// A file has no thumbnail, as its device could not read its format, or it is no
	/// picture.
	#[error(
		"{0} has no thumbnail: the device that added it could not read its format, or it is no picture"
	)]
	NoThumbnail(String),
	/// Something is where a file would be written, which replaces nothing.
	#[error("{} exists already", .0.display())]
	Exists(PathBuf),
	/// No library was given, and the current folder is in none.
	#[error(
		"this folder is not in a library; if it is a tree's folder that lost its .nib file, run nib init FOLDER in your library's folder; otherwise make a library with nib init, or give one with --library or NIBKEEP_LIBRARY"
	)]
	NotInLibrary,
	/// The folder given as the library holds none.
	#[error("there is no library in {}", .0.display())]
	NoLibraryIn(PathBuf),
	/// The system lets nib not read a folder where a library would be, as macOS does with a
	/// removable drive until the person allows the terminal to reach it.
	#[error(
		"nib may not read {folder}: {error}; on a Mac, allow your terminal to reach removable volumes, in System Settings, Privacy & Security, Files and Folders",
		folder = .0.display(),
		error = .1
	)]
	NoAccess(PathBuf, #[source] io::Error),
	/// `NIBKEEP_LIBRARY` names a folder that holds no library.
	#[error("there is no library in {}, which NIBKEEP_LIBRARY names", .0.display())]
	NoDefaultLibrary(PathBuf),
	/// `NIBKEEP_LIBRARY` names a folder relative to the current one.
	#[error("NIBKEEP_LIBRARY names {}, which is not a full path", .0.display())]
	RelativeDefault(PathBuf),
	/// A library's `.nib` holds what is left of starting it, in this replica's folder.
	#[error(
		"a library was started in {}, but not finished; remove that folder to start again",
		.0.display()
	)]
	Unfinished(PathBuf),
	/// A library's `.nib` holds only copies of replicas made elsewhere, which are never used
	/// as the devices they were (technical design: copies of a replica).
	#[error(
		"the library in {} is a copy, made on another computer or drive, or in another folder, and is never used here as the device it was; to make this computer a device of its own, add it from another of your devices with nib device pair there and nib init --join here, which takes in what the copy holds, or, where no other device of yours can, use nib recovery from-copy",
		.0.display()
	)]
	IsCopy(PathBuf),
	/// `nib secret from-copy` found no secrets keys in the copies, or the person left the
	/// passphrase of the copy's secrets empty.
	#[error("no secrets were taken from a copy")]
	NoSecretsInCopies,
	/// `nib recovery from-copy` finds no copy in the library's `.nib`.
	#[error("there is no copy of a library in {}", .0.display())]
	NoCopy(PathBuf),
	/// `nib recovery from-copy` finds several copies, of which it would not know which to use.
	#[error("{} holds copies of several libraries' devices; keep the one to use", .0.display())]
	SeveralCopies(PathBuf),
	/// A library's `.nib` holds only copies of replicas made elsewhere, so a new library is
	/// not started there.
	#[error(
		"{} holds a copy of a library made on another computer or drive, or in another folder; join that library with nib init --join, or start one elsewhere",
		.0.display()
	)]
	CopiedHere(PathBuf),
	/// A library would be on a drive without a journal, which the person did not allow
	/// (technical design: drives without a journal).
	#[error(
		"{} is on a drive whose file system keeps no journal, as FAT32 and exFAT, which most USB sticks have: a library there can be damaged after the drive is removed without ejecting it, or loses its power; keep the library on another drive, or run again with --allow-fat to keep it there, and check the drive for errors after any such removal",
		.0.display()
	)]
	NoJournal(PathBuf),
	/// The drive of a library without a journal was removed, or lost its power, while the
	/// library was open.
	#[error(
		"the library in {} was not closed: its drive was removed without ejecting it, or lost its power, while the library was open, or a program that had it open was stopped, so its files may be damaged; check the drive for errors first, with Scan and fix on Windows, First Aid on a Mac or fsck on Linux, then run nib verify --drive-checked",
		.0.display()
	)]
	UncleanRemoval(PathBuf),
	/// A library would be started in what another library keeps.
	#[error("{} is in a library's own folder .nib; start the library elsewhere", .0.display())]
	InLibraryData(PathBuf),
	/// The current folder cannot be found.
	#[error("the current folder cannot be found: {0}")]
	NoCurrentFolder(#[source] io::Error),
	/// Ranked languages that cannot be read.
	#[error(transparent)]
	Languages(#[from] ParseLanguagesError),
	/// `--form` without forms to add or remove.
	#[error("--form adds and removes forms with field+=value and field-=value; there are none")]
	NoForms,
	/// Entities to merge that are one already.
	#[error("{entity} and {into} are one entity already")]
	OneAlready {
		/// The entity, as it was given.
		entity: String,
		/// The entity to merge it into, as it was given.
		into: String,
	},
	/// A merge to undo, of an entity that nothing was merged into.
	#[error("no entity was merged into {0}")]
	NothingMerged(String),
	/// A merge to undo, of one of several entities merged into another, not said which.
	#[error("say which entity to set apart from {entity}: {candidates}")]
	WhichMerged {
		/// The entity, as it was given.
		entity: String,
		/// The entities merged into it.
		candidates: String,
	},
	/// A merge to undo, of an entity that was not merged into the one given.
	#[error("no entity merged into {entity} is {other}")]
	NotMergedInto {
		/// The entity, as it was given.
		entity: String,
		/// The entity to set apart, as it was given.
		other: String,
	},
	/// A field that an entity never had, in `nib history`.
	#[error("the entity never had a field {0}")]
	NoHistory(String),
	/// A moment that cannot be used.
	#[error("the moment cannot be used: {0}")]
	Moment(String),
	/// An entity shown as recorded by a moment, when the library had no record of it yet.
	#[error("the library had no record of {entity} by {when}")]
	NotYet {
		/// The entity, as it was given.
		entity: String,
		/// The moment, for people.
		when: String,
	},
	/// The passphrase and its confirmation differ.
	#[error("the passphrases differ")]
	PassphrasesDiffer,
	/// The edits of a checkout's copy, made to a version that is no longer the file's own.
	#[error(
		"{} was checked out from a version that is no longer the file's own; --force records its edits on top, and history keeps both",
		.0.display()
	)]
	Stale(PathBuf),
	/// Files that could not be added, while the others were.
	#[error("{}", files_failed(*.0))]
	FilesFailed(usize),
	/// A file to check out that this device lacks, which none of the devices that hold it
	/// gave, as none was found on the network.
	#[error(
		"{name} is not on this device, and no device that holds it was found on the network: {}",
		holders.join(", ")
	)]
	NotFetched {
		/// The file.
		name: String,
		/// The devices that hold it, as their accounts say.
		holders: Vec<String>,
	},
	/// A file to check out that this device lacks, which the devices that hold it, as their
	/// accounts say, did not give, though a sync reached them.
	#[error(
		"{name} is not on this device, and did not come from {}, which hold it as their accounts say; they may have lost it, which nib verify there tells",
		holders.join(", ")
	)]
	NotGiven {
		/// The file.
		name: String,
		/// The devices that a sync reached.
		holders: Vec<String>,
	},
	/// Devices that could not be synced with, while the others were.
	#[error("{}", syncs_failed(*.0))]
	SyncsFailed(usize),
	/// The program that syncs this device did not answer `nib sync` in time.
	#[error("{0} did not sync in time; it may be busy with a long sync")]
	NotAnswered(String),
	/// `nib mcp` on a library that neither allows AI agents nor is an AI agent's.
	#[error(
		"AI agents are not allowed on this library here; allow them with nib mcp enable in its folder"
	)]
	NotAnAiAgent,
	/// `--client` names no AI client that `nib mcp enable` knows.
	#[error(
		"nib knows no AI client {0}; it knows Claude Code (claude) and Codex (codex), and --print gives the line for any other"
	)]
	UnknownAiClient(String),
	/// `nib mcp` on a library whose keys need a passphrase, without a file that holds it.
	#[error(
		"nib mcp serve asks for nothing on a terminal; give the library's passphrase with --passphrase-file"
	)]
	NoTerminal,
	/// A secret value and its confirmation differ.
	#[error("the values differ")]
	ValuesDiffer,
	/// A secret value to set is empty.
	#[error("the value is empty")]
	EmptyValue,
	/// A secret field, in a command that takes no secret values.
	#[error(
		"{0} is secret: nib secret set asks for its value without showing it, and nib secret clear clears it"
	)]
	SecretField(nibkeep_model::FieldName),
	/// A value of a secret field given on the command line, which shells keep.
	#[error(
		"{0} is secret, and its values are not given on the command line: nib secret purge asks for one without showing it, and {0}= purges every value it has had"
	)]
	SecretGiven(nibkeep_model::FieldName),
	/// The history of a secret field, which `nib history` leaves out.
	#[error("{0} is secret: nib secret history shows the values it has had")]
	SecretHistory(nibkeep_model::FieldName),
	/// A folder given to `nib file add --secret`, which takes files one by one.
	#[error("{} is a folder; nib file add --secret adds files one by one", .0.display())]
	SecretFolder(PathBuf),
	/// A secret file to check out, of which NibKeep records no copies.
	#[error(
		"{0} is a secret file, of which NibKeep records no copies; nib secret save writes one that nothing keeps track of"
	)]
	SecretFileCheckout(String),
	/// A field that is not secret, in `nib secret`.
	#[error("{0} is not secret; nib show and nib edit read and set it")]
	NotSecret(nibkeep_model::FieldName),
	/// A password that cannot be generated as asked.
	#[error(transparent)]
	Generator(#[from] nibkeep_passwords::GeneratorError),
	/// A secret of one-time codes that makes none.
	#[error(transparent)]
	OneTime(#[from] nibkeep_passwords::OneTimeError),
	/// The clipboard failed.
	#[error(transparent)]
	Clipboard(#[from] nibkeep_passwords::ClipboardError),
	/// Options for a generated password, without --generate.
	#[error("--length and the --no- options say what --generate makes; give --generate too")]
	RecipeWithoutGenerate,
	/// A generated secret of one-time codes, which only the site can give.
	#[error(
		"{0} holds the secret of one-time codes, which the site gives; --generate makes passwords"
	)]
	GeneratedOneTimeCode(nibkeep_model::FieldName),
	/// A rule that no number or ID names.
	#[error("there is no rule {0}; nib policy list numbers the rules")]
	NoRule(String),
	/// A rule to move, without saying where.
	#[error("say where the rule goes: --first, --last, --before RULE or --after RULE")]
	WhereTo,
	/// Secrets to unlock, in a library that has none.
	#[error("the library holds no secrets yet; nib secret set stores the first")]
	NoSecrets,
	/// A secret field without values, in `nib secret show`.
	#[error("{entity} has no value of {field}")]
	NoSecretValue {
		/// The entity, as it was given.
		entity: String,
		/// The field.
		field: nibkeep_model::FieldName,
	},
	/// The passphrase is empty.
	#[error("the passphrase is empty")]
	EmptyPassphrase,
	/// The file that gives the passphrase cannot be read.
	#[error("the passphrase file {} cannot be read: {error}", path.display())]
	PassphraseFile {
		/// The file.
		path: PathBuf,
		/// Why not.
		error: io::Error,
	},
	/// The file that gives the passphrase can be read by others.
	#[error("others can read the passphrase file {}; make it yours alone, such as with chmod 600", .0.display())]
	PassphraseFileOpen(PathBuf),
	/// Syncing from login, for a library whose keys a passphrase protects.
	#[error(
		"syncing from login needs the library's keys in the keystore, as nobody is there to type a passphrase at login; nib protect puts them there"
	)]
	AtLoginPassphrase,
	/// No home folder to keep the list of libraries synced from login in.
	#[error("there is no home folder here to keep the list of libraries synced from login in")]
	NoStateFolder,
	/// No `nibkeep-sync` beside `nib`.
	#[error(
		"nibkeep-sync, which syncs from login, is not installed beside nib; cargo install --path apps/nib installs both"
	)]
	NoSyncProgram,
	/// The library's keys are in the platform's keystore, which is not there.
	#[error("the library's keys are in the platform's keystore, which is not available")]
	NoKeystore,
	/// The platform's keystore failed while a library was started.
	#[error("the platform's keystore failed ({0}); start the library with --passphrase instead")]
	Keystore(NodeError),
	/// No keystore to protect a library's keys or its secrets with.
	#[error(
		"there is no platform keystore here; nib protect --passphrase protects with a passphrase"
	)]
	NoKeystoreToProtect,
	/// The platform's keystore failed while protecting a library's keys or its secrets.
	#[error(
		"the platform's keystore failed ({0}); nib protect --passphrase protects with a passphrase"
	)]
	KeystoreToProtect(NodeError),
	/// A name is not valid.
	#[error("{0} is not a valid name")]
	Name(String),
	/// A name that a person gives is a standard one, which only the standard vocabulary
	/// gives, so that a later version's standard field never meets another kind of it.
	#[error(
		"{name} is a standard name, which only the standard vocabulary gives; choose another \
		 {what} name, such as one starting with local."
	)]
	StandardName {
		/// The name given.
		name: String,
		/// What it names: a type or a field.
		what: &'static str,
	},
	/// A field is not given as `name:kind`.
	#[error(
		"a field is name:kind, with flags such as :many, :lww, :secret or :role after it, not {0}"
	)]
	Field(String),
	/// There is no such kind of value.
	#[error("there is no kind of value called {kind}; the kinds are {kinds}")]
	Kind {
		/// The kind given.
		kind: String,
		/// The kinds there are.
		kinds: String,
	},
	/// There is no such flag for a field.
	#[error(
		"there is no flag {0} for a field; the flags are many, lww, secret, role, stands-for, search-through, code, unit= and reverse="
	)]
	Flag(String),
	/// A field's flags contradict each other, or repeat.
	#[error("{0}")]
	Flags(String),
	/// There is no such type.
	#[error("there is no type {0}; define it with nib type add")]
	NoType(TypeName),
	/// Several types end in the name.
	#[error("{name} could be any of the types {candidates}")]
	AmbiguousType {
		/// The name as given.
		name: String,
		/// The types it could be.
		candidates: String,
	},
	/// The types have no such field.
	#[error(
		"{types} has no field {field}; give its full name, such as local.{field}, for a field that no type defines"
	)]
	NoField {
		/// The field as given.
		field: String,
		/// The types.
		types: String,
	},
	/// Several fields of the types end in the name.
	#[error("{field} could be any of the fields {candidates}")]
	AmbiguousField {
		/// The field as given.
		field: String,
		/// The fields it could be.
		candidates: String,
	},
	/// Types set, or cleared, rather than given or taken away one by one.
	#[error("an entity's types are given with type+=TYPE and taken away with type-=TYPE")]
	TypeSet,
	/// A value is not given as `field=value`.
	#[error("a value is field=value, not {0}")]
	Assignment(String),
	/// Checking the library found something wrong.
	#[error("{}", unsound(*.0))]
	Unsound(usize),
	/// An export that differs from the library, with how many problems were found.
	#[error("{}", unsound_export(*.0))]
	UnsoundExport(usize),
	/// A purge was not confirmed.
	#[error("nothing was purged; add --yes to purge for good")]
	Unconfirmed,
	/// A move of a folder or a list, with all it holds, was not confirmed.
	#[error("nothing was moved; add --yes to move them all")]
	UnconfirmedMove,
	/// Starting a tree was not confirmed.
	#[error("no tree was started; add --yes to start it")]
	UnconfirmedStart,
	/// Pinning a folder that holds no Git repository with a commit.
	#[error("{} holds no Git repository with a commit to pin", .0.display())]
	NoGitCommit(PathBuf),
	/// A tree's command run outside every tree's folder on this device.
	#[error("{} is in no tree's folder on this device; nib init makes it one", .0.display())]
	NotInTree(PathBuf),
	/// A command that reads a tree's history run outside every tree's folder on this device,
	/// with no tree named.
	#[error(
		"{} is in no tree's folder on this device: name a tree with --tree, or see an entity's values with nib history",
		.0.display()
	)]
	NoTreeHere(PathBuf),
	/// `nib init` given a folder, a message or --yes, to make a tree, where neither that folder
	/// nor the current one is in a library.
	#[error(
		"neither {} nor the current folder is in a library: run nib init FOLDER in your library's folder, nib init alone to make a library here, or nib init --join CODE to join yours from another device",
		.0.display()
	)]
	NoLibraryForTree(PathBuf),
	/// A `.nib` file in a folder that names no library nib may open.
	#[error("{} should name the library of the tree in its folder, but {why}", .file.display())]
	BadPointer {
		/// The file.
		file: PathBuf,
		/// What is wrong with it.
		why: String,
	},
	/// A `.nib` file that names a library which keeps no tree in that folder on this device, as
	/// a copy of a tree's folder would.
	#[error(
		"{} names the library in {}, which keeps no tree in that folder on this device: nib init there makes the folder a tree, and deleting the file leaves the folder out of the library",
		.file.display(),
		.library.display()
	)]
	StrayPointer {
		/// The file.
		file: PathBuf,
		/// The library's folder.
		library: PathBuf,
	},
	/// A command about the library's place, run where only a tree's `.nib` file names the
	/// library.
	#[error(
		"{} names the library for the commands of the tree in its folder; run this in the library's folder, or name the library with --library",
		.0.display()
	)]
	ThroughPointer(PathBuf),
	/// Something other than a file where the `.nib` file of a tree outside the library's
	/// folder goes.
	#[error(
		"{} is no file: nib keeps a file there that names the library of a tree outside the library's folder",
		.0.display()
	)]
	PointerInTheWay(PathBuf),
	/// The `.nib` file of a tree outside the library's folder could not be written.
	#[error(
		"{} could not be written: {error}; the tree is there, and nib init {} in the library's folder writes the file again",
		.file.display(),
		pointer::folder_of(.file)
	)]
	PointerNotWritten {
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
		pointer::folder_of(.file)
	)]
	PointerNotTakenAway {
		/// The file.
		file: PathBuf,
		/// Why not.
		error: io::Error,
	},
	/// `nib init` without --name, where the system names no account to name the user after.
	#[error("the system names no account for you: give your name with nib init --name YOUR-NAME")]
	NoAccountName,
	/// A command of other version control systems that nib does not have.
	#[error("there is no {verb} command: {instead}")]
	NoSuchCommand {
		/// The command.
		verb: &'static str,
		/// What to do instead.
		instead: &'static str,
	},
	/// `nib revert` given a path, as Mercurial's revert takes, rather than a revision.
	#[error(
		"{0} is no revision: nib revert undoes a revision, and nib restore {0} writes a file as it was"
	)]
	RevertTakesRevision(String),
	/// A revocation was not confirmed.
	#[error("nothing was revoked; add --yes to revoke for good")]
	UnconfirmedRevoke,
	/// Whether another device takes new spaces, which only that device says.
	#[error("only {0} says whether it takes new spaces: run nib device mark --new-spaces there")]
	NewSpacesElsewhere(String),
	/// What was typed was no recovery code, each time it was asked for.
	#[error("that was no recovery code; nothing was changed")]
	NotARecoveryCode,
	/// A recovery in place of a library here that still unlocks with the keystore, which it
	/// would only revoke.
	#[error(
		"the library here still unlocks with this computer's keystore: there is nothing to recover, and a recovery would only revoke it"
	)]
	StillUnlocks,
	/// The recovery code typed is none that the library names.
	#[error("the library names no such recovery code")]
	UnknownRecoveryCode,
	/// Accepting changes that were set aside was not confirmed.
	#[error("nothing was accepted; add --yes to accept them")]
	UnconfirmedAccept,
	/// Leaving a space was not confirmed.
	#[error("you did not leave; add --yes to leave")]
	UnconfirmedLeave,
	/// Removing a member was not confirmed.
	#[error("nobody was removed; add --yes to remove them")]
	UnconfirmedRemove,
	/// Claiming the admin role was not confirmed.
	#[error("nothing was claimed; add --yes to claim the admin role")]
	UnconfirmedClaim,
	/// An invitation without --yes, where there is no terminal to ask whether to add whoever
	/// asks.
	#[error(
		"nobody can be asked whether to add whoever joins, as there is no terminal; run it on a terminal, or add --yes to add whoever joins with the code"
	)]
	UnconfirmedInvite,
	/// A field is set and cleared in one edit.
	#[error("{0} is both set and cleared")]
	SetAndCleared(nibkeep_model::FieldName),
	/// A value cannot be read as its field's kind says.
	#[error("{field} takes {expected}")]
	Value {
		/// The field.
		field: nibkeep_model::FieldName,
		/// What it takes.
		expected: &'static str,
	},
	/// A name or a prefix of an ID that several entities have.
	#[error(
		"{reference} could be any of {count} entities; give the ID, or its start:\n{candidates}"
	)]
	Ambiguous {
		/// The name or prefix, as it was given.
		reference: String,
		/// How many entities have it.
		count: usize,
		/// Some of them, one per line, by short ID, name and types.
		candidates: String,
	},
	/// The engine failed.
	#[error(transparent)]
	Node(#[from] NodeError),
	/// Reading or writing failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),
}

/// How long the parts of a command took. A part that the command had no need of is `None`:
/// `nib init` opens no library, and makes and protects its keys as part of its work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timings {
	/// Waiting for someone to type a passphrase, which no other part includes.
	pub waiting: Option<Duration>,
	/// Unlocking the library's keys: reading them, reaching the platform's keystore, and
	/// deriving a key from a passphrase, which is slow on purpose.
	pub unlock: Option<Duration>,
	/// Opening the spaces: reading their logs, and building their indexes.
	pub open: Option<Duration>,
	/// The command's own work.
	pub work: Duration,
	/// Writing out what the command has to say.
	pub print: Duration,
}

impl Timings {
	/// The parts that the command had, by name, in the order they came.
	fn parts(&self) -> Vec<(&'static str, Duration)> {
		let mut parts = Vec::new();
		for (name, part) in [
			("waiting", self.waiting),
			("unlock", self.unlock),
			("open", self.open),
		] {
			if let Some(duration) = part {
				parts.push((name, duration));
			}
		}
		parts.push(("work", self.work));
		parts.push(("print", self.print));
		parts
	}

	/// The timings for people, such as `unlock 0.412 s, open 1.603 s, work 0.010 s, print
	/// 0.001 s`, or as one line of JSON with seconds, such as `{"unlock_seconds":0.412,
	/// …}`. Parts that the command had no need of are left out.
	#[must_use]
	pub fn describe(&self, json: bool) -> String {
		let parts = self.parts();
		if json {
			let object: serde_json::Map<String, serde_json::Value> = parts
				.iter()
				.map(|(part, duration)| (format!("{part}_seconds"), duration.as_secs_f64().into()))
				.collect();
			return serde_json::Value::Object(object).to_string();
		}
		parts
			.iter()
			.map(|(part, duration)| format!("{part} {:.3} s", duration.as_secs_f64()))
			.collect::<Vec<_>>()
			.join(", ")
	}

	/// The sum of the parts before the work.
	fn before_work(&self) -> Duration {
		[self.waiting, self.unlock, self.open]
			.into_iter()
			.flatten()
			.sum()
	}
}

/// Adds to a part of the timings.
fn add(part: &mut Option<Duration>, duration: Duration) {
	*part = Some(part.unwrap_or_default() + duration);
}

/// Runs a command, and writes what it has to say to `output`.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn run<E: Environment>(
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// The parts are timed only when asked for, so that every write is not.
	run_command(cli, environment, output, &mut Timings::default())
}

/// Runs a command as [`run`] does, and tells how long its parts took.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn run_timed<E: Environment>(
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<Timings, CliError> {
	let started = Instant::now();
	let mut timings = Timings::default();
	let mut timed = Timed {
		inner: output,
		spent: Duration::ZERO,
	};
	run_command(cli, environment, &mut timed, &mut timings)?;
	// What is still buffered is written now, so that printing counts all of it.
	timed.flush()?;
	timings.print = timed.spent;
	timings.work = started
		.elapsed()
		.saturating_sub(timings.before_work() + timings.print);
	Ok(timings)
}

/// A line for the terminal, with the characters that it would hide or act on, but line
/// breaks, written as escapes: a carriage return as `\r`, an escape as `\u{1b}`, and the
/// invisible ones that hide or reorder text, as a zero-width space or a right-to-left
/// override. So an argument that a script read on Windows, ending in a carriage return,
/// shows it, and a name or path that another device wrote cannot change what the terminal
/// shows in an error, a note or a question. What commands print on standard output is left
/// as it is, for other programs to read.
#[must_use]
pub fn printable(line: &str) -> std::borrow::Cow<'_, str> {
	fn hidden(c: char) -> bool {
		(c.is_control() && c != '\n')
			|| matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
	}
	if !line.chars().any(hidden) {
		return line.into();
	}
	line.chars()
		.map(|c| {
			if hidden(c) {
				c.escape_debug().to_string()
			} else {
				c.to_string()
			}
		})
		.collect::<String>()
		.into()
}

/// A writer that writes until nothing reads what it writes, as when `head` has read
/// enough, and then takes what is written without writing it: a command finishes its work
/// whether or not its output is read, and says nothing of output that nobody reads.
pub struct UntilUnread<W> {
	inner: W,
	unread: bool,
}

impl<W> UntilUnread<W> {
	/// Writes to `inner` until nothing reads it.
	pub const fn new(inner: W) -> Self {
		Self {
			inner,
			unread: false,
		}
	}

	/// The result of writing, or of flushing, with a broken pipe taken as nothing reading.
	fn read_or_not<T>(&mut self, done: io::Result<T>, nothing: T) -> io::Result<T> {
		match done {
			Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
				self.unread = true;
				Ok(nothing)
			}
			done => done,
		}
	}
}

impl<W: Write> Write for UntilUnread<W> {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		if self.unread {
			return Ok(bytes.len());
		}
		let written = self.inner.write(bytes);
		self.read_or_not(written, bytes.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		if self.unread {
			return Ok(());
		}
		let flushed = self.inner.flush();
		self.read_or_not(flushed, ())
	}
}

/// A writer that counts the time spent writing.
struct Timed<'a> {
	inner: &'a mut dyn Write,
	spent: Duration,
}

impl Write for Timed<'_> {
	fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
		let started = Instant::now();
		let written = self.inner.write(bytes);
		self.spent += started.elapsed();
		written
	}

	fn flush(&mut self) -> io::Result<()> {
		let started = Instant::now();
		let flushed = self.inner.flush();
		self.spent += started.elapsed();
		flushed
	}
}

fn run_command<E: Environment>(
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
	timings: &mut Timings,
) -> Result<(), CliError> {
	// As the replicas it opens record it, beside the apps.
	nibkeep_node::name_program(APP);
	if let Command::Absent(absent) = &cli.command {
		let (verb, instead) = absent.instead();
		return Err(CliError::NoSuchCommand { verb, instead });
	}
	if let Command::Secret {
		command: SecretCommand::Generate { recipe },
	} = &cli.command
	{
		return generate_command(recipe, cli.json, environment, output);
	}
	let (found, making) = command_folder(cli, environment)?;
	place_not_through_pointer(cli, &found)?;
	let folder = found.folder.clone();
	let directory = folder.join(LIBRARY_DATA);
	if let Command::Mcp {
		action: McpAction::Serve { .. },
	} = &cli.command
	{
		return mcp::serve(cli, environment, output);
	}
	if making {
		let library = new_library(&directory, cli, environment, timings, output)?;
		let zone = environment.time_zone();
		let checks = output::Checks {
			status: None,
			name_of: &|_, _| None,
			zone: &zone,
		};
		let status = (
			&library.status()?,
			&[][..],
			&std::collections::BTreeMap::new(),
		);
		return output::status(status, checks, &folder, cli.json, output);
	}
	if let Command::Verify {
		drive_checked: true,
		..
	} = &cli.command
	{
		Library::drive_checked(&directory)?;
	}
	if let Command::Sync {
		at_login: Some(switch),
		..
	} = &cli.command
	{
		return at_login_command(*switch, &directory, cli, environment, timings, output);
	}
	let opened = open(
		&directory,
		cli.passphrase_file.as_deref(),
		environment,
		timings,
	);
	let mut library = opened.map_err(|error| opened_error(error, &directory))?;
	check_found(&library, &found, cli, environment)?;
	if library.told_of_syncing()? {
		environment.note(SYNCED_BY_PROGRAM);
	}
	if let Some(space) = &cli.space {
		let space = library.find_space(space)?;
		library.choose_space(Some(space))?;
	}
	if let Command::Sync { serve: true, .. } = &cli.command {
		return serve_command(library, cli, environment, output);
	}
	if let Command::Secret {
		command: SecretCommand::Copy {
			entity,
			field,
			clear_after,
		},
	} = &cli.command
	{
		// The clipboard first, before the passphrase is asked for; and the library is closed
		// while the copy waits, for other programs to use.
		let board = environment.clipboard()?;
		let read = read_secret(&mut library, entity, field, environment, timings)
			.map_err(|error| candidates_named(&library, error, environment))?;
		drop(library);
		return copy_and_clear(board, &read, *clear_after, cli.json, environment, output);
	}
	let ran = match &cli.command {
		Command::Backup { .. } | Command::Recovery { .. } => {
			safekeeping_command(&mut library, cli, environment, timings, output)
		}
		Command::Purges { command } => purges_waiting_command(
			&mut library,
			command.as_ref(),
			cli.json,
			environment,
			output,
		),
		_ => library_command(&mut library, cli, &folder, environment, timings, output),
	};
	// An entity named ambiguously is told apart by what the library knows of each.
	ran.map_err(|error| candidates_named(&library, error, environment))
}

/// Refuses what concerns the library's place where only a tree's `.nib` file names the
/// library: it runs before the library opens, so before the file could be checked, and
/// belongs to no tree.
fn place_not_through_pointer(cli: &Cli, found: &Found) -> Result<(), CliError> {
	let place = matches!(
		&cli.command,
		Command::Verify {
			drive_checked: true,
			..
		} | Command::Sync {
			at_login: Some(_),
			..
		}
	);
	match &found.named_in {
		Some(file) if place => Err(CliError::ThroughPointer(file.clone())),
		_ => Ok(()),
	}
}

/// Refuses a library that a tree's `.nib` file named, unless it keeps that tree
/// ([`pointer::check`]). `nib init` takes the file in the folder it makes a tree, which it
/// then writes anew, as in a copy of a tree's folder.
fn check_found<E: Environment>(
	library: &Library,
	found: &Found,
	cli: &Cli,
	environment: &E,
) -> Result<(), CliError> {
	let starting = match &cli.command {
		Command::Init { folder, .. } => Some(match folder {
			Some(folder) => absolute(folder, environment)?,
			None => current_dir(environment)?,
		}),
		_ => None,
	};
	Ok(pointer::check(library, found, starting.as_deref())?)
}

/// The folder of the library that a command works in, and whether the command makes it: for
/// `nib init` that makes a library, where it goes; for one that makes a tree, the library it
/// goes into, as for the other commands. `nib init` alone, where it finds no library, makes
/// one, as `git init` makes a repository.
fn command_folder<E: Environment>(cli: &Cli, environment: &E) -> Result<(Found, bool), CliError> {
	if cli.command.makes_library() {
		return Ok((
			Found::library(new_library_folder(cli.library.as_deref(), environment)?),
			true,
		));
	}
	if let Command::Init {
		folder,
		message,
		yes,
		..
	} = &cli.command
	{
		// The library that holds the folder to make a tree, as Git finds a repository, or else
		// that of the current folder, as git worktree add takes it.
		let from = match folder {
			Some(folder) => absolute(folder, environment)?,
			None => current_dir(environment)?,
		};
		let here = environment.current_dir().ok();
		let places: Vec<&Path> = std::iter::once(from.as_path())
			.chain(here.as_deref())
			.collect();
		let given = cli.library.as_deref();
		let alone = folder.is_none() && message.is_none() && !yes;
		let found = match library_folder_from(given, &places, environment) {
			// A file there that names no library, as after the library's folder moved, is
			// written again, in the current folder's library.
			Err(CliError::BadPointer { file, .. }) if file == from.join(LIBRARY_DATA) => {
				library_folder_from(given, &places[1..], environment)
			}
			found => found,
		};
		return match found {
			Ok(found) => Ok((found, false)),
			Err(CliError::NotInLibrary | CliError::NoLibraryIn(_)) if alone => Ok((
				Found::library(new_library_folder(given, environment)?),
				true,
			)),
			Err(CliError::NotInLibrary) => Err(CliError::NoLibraryForTree(from)),
			Err(error) => Err(error),
		};
	}
	if let (
		Command::Sync {
			at_login: Some(Switch::Off),
			..
		},
		Some(given),
	) = (&cli.command, cli.library.as_deref())
	{
		// A library deleted or moved since stays listed, until turned off by its old folder.
		return Ok((
			Found::library(resolved(absolute(given, environment)?)),
			false,
		));
	}
	Ok((library_folder(cli.library.as_deref(), environment)?, false))
}

impl Command {
	/// Whether it is a command of trees, `nib init` among them where it makes no library, or
	/// `nib status`, which shows a tree's status in its folder.
	const fn is_of_trees(&self) -> bool {
		matches!(
			self,
			Self::Init { .. } | Self::Status { .. } | Self::Trees(_) | Self::Pull { .. }
		)
	}

	/// Whether it makes a library on this computer: `nib init` told to make one or join one,
	/// rather than a tree in a library, by any of the arguments that only a new library takes,
	/// or `nib recovery` that brings one back. Without them, `nib init` makes a library only
	/// where none is found.
	const fn makes_library(&self) -> bool {
		matches!(
			self,
			Self::Init {
				name,
				join,
				device,
				passphrase,
				languages,
				allow_fat,
				..
			} if name.is_some()
				|| join.is_some()
				|| device.is_some()
				|| *passphrase
				|| languages.is_some()
				|| *allow_fat
		) || matches!(
			self,
			Self::Recovery {
				command: RecoveryCommand::Restore { .. } | RecoveryCommand::FromCopy { .. }
			}
		)
	}
}

/// The library that a command other than `nib init` works in, as Git finds a repository:
/// the one given; or else the nearest of the current folder and those it is in, below the
/// environment's ceiling, whose `.nib` holds a library, or is a file in a tree's folder that
/// names one; or else the one that the environment names.
fn library_folder<E: Environment>(
	given: Option<&Path>,
	environment: &E,
) -> Result<Found, CliError> {
	if given.is_some() {
		return library_folder_from(given, &[], environment);
	}
	library_folder_from(given, &[&current_dir(environment)?], environment)
}

/// The library that [`library_folder`] finds, looked for from each of `places` in turn
/// rather than from the current folder.
fn library_folder_from<E: Environment>(
	given: Option<&Path>,
	places: &[&Path],
	environment: &E,
) -> Result<Found, CliError> {
	if let Some(given) = given {
		let folder = resolved(absolute(given, environment)?);
		if !holds_library(&folder) {
			return Err(match unreadable(&folder) {
				Some(error) => CliError::NoAccess(folder, error),
				None => CliError::NoLibraryIn(folder),
			});
		}
		return Ok(Found::library(folder));
	}
	let ceiling = environment.ceiling();
	for here in places {
		if let Some(found) = pointer::nearest(here, ceiling.as_deref())? {
			return Ok(found);
		}
		if let Some(error) = unreadable(here) {
			return Err(CliError::NoAccess(here.to_path_buf(), error));
		}
	}
	let named = environment
		.default_library()
		.ok_or(CliError::NotInLibrary)?;
	// Relative to every folder in turn, it would name another library in each.
	if !named.is_absolute() {
		return Err(CliError::RelativeDefault(named));
	}
	let named = resolved(named);
	if !holds_library(&named) {
		return Err(CliError::NoDefaultLibrary(named));
	}
	Ok(Found::library(named))
}

/// The folder that `nib init` makes a library: the one given, or else the current folder,
/// as the file system will resolve it, and never in a library's own `.nib`. Nothing is
/// made yet.
fn new_library_folder<E: Environment>(
	given: Option<&Path>,
	environment: &E,
) -> Result<PathBuf, CliError> {
	let folder = match given {
		Some(given) => absolute(given, environment)?,
		None => current_dir(environment)?,
	};
	let folder = resolved(folder);
	if folder
		.components()
		.any(|part| part.as_os_str() == LIBRARY_DATA)
	{
		return Err(CliError::InLibraryData(folder));
	}
	Ok(folder)
}

/// What `nib` says once, when a program that syncs folders syncs the library's folder.
const SYNCED_BY_PROGRAM: &str = "a program that syncs folders syncs this library's folder. NibKeep syncs it with your other devices on its own, and keeps each device's copy apart, so nothing is lost. But that program puts every device's keys on every computer it syncs and with its provider, where keys protected by a passphrase can be guessed at, and a deletion or an older version on one computer reaches the others. Keep the library out of it if you can.";

/// The error for a library whose start was cut short: it names the replica's own folder,
/// which removing leaves the copies of other replicas beside it as they are.
fn unfinished(directory: &Path) -> CliError {
	CliError::Unfinished(
		Library::replica_folder(directory).unwrap_or_else(|_| directory.to_owned()),
	)
}

/// Why the system lets nib not read a folder, if it does not, so that a library there is not
/// taken for missing.
fn unreadable(folder: &Path) -> Option<io::Error> {
	match std::fs::read_dir(folder) {
		Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Some(error),
		_ => None,
	}
}

fn current_dir<E: Environment>(environment: &E) -> Result<PathBuf, CliError> {
	environment.current_dir().map_err(CliError::NoCurrentFolder)
}

/// A folder given on the command line, relative to the current folder unless it is a full
/// path.
fn absolute<E: Environment>(folder: &Path, environment: &E) -> Result<PathBuf, CliError> {
	if folder.is_absolute() {
		Ok(folder.to_owned())
	} else {
		Ok(current_dir(environment)?.join(folder))
	}
}

/// Runs a command on an open library, in its folder: any but `nib init`.
fn library_command<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	folder: &Path,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	match &cli.command {
		Command::Languages { .. } => {
			languages_command(library, &cli.command, json, environment, output)
		}
		Command::Show { .. }
		| Command::History { .. }
		| Command::Export { .. }
		| Command::Find { .. }
		| Command::List { .. } => reading_command(library, cli, folder, environment, output),
		Command::Verify { export, .. } => {
			verify_command(library, export.as_deref(), json, environment, output)
		}
		Command::New {
			kind,
			values,
			language,
		} => {
			let new = (kind.as_str(), values.as_slice(), language.as_ref());
			new_command(library, new, json, environment, output)
		}
		Command::Merge {
			entity,
			other,
			undo,
		} => {
			let merge = (entity.as_str(), other.as_deref(), *undo);
			merge_command(library, merge, json, environment, output)
		}
		Command::Edit(arguments) => edit_command(library, arguments, json, environment, output),
		Command::Move {
			entity,
			to,
			without_history,
			keep_behind,
			leave_secrets,
			yes,
		} => {
			let args = (
				(entity.as_str(), to.as_str(), *without_history, *yes),
				(*keep_behind, *leave_secrets),
			);
			move_command(library, args, json, environment, timings, output)
		}
		Command::Purge { .. } => purge_or_list(library, cli, environment, timings, output),
		Command::File { command } => {
			let file = (command, folder);
			file_command(library, file, cli, environment, timings, output)
		}
		Command::Policy { command } => policy_command(library, command, json, environment, output),

		Command::Secret { command } => {
			secret_command(library, command, json, environment, timings, output)
		}
		Command::Protect {
			passphrase,
			secrets,
		} => {
			let protect = (*passphrase, *secrets);
			protect_command(library, protect, json, environment, timings, output)?;
			// Nobody is there to type a passphrase at login.
			if *passphrase && !*secrets {
				turn_off_at_login(&folder.join(LIBRARY_DATA), environment);
			}
			Ok(())
		}
		Command::Import { paths, from } => {
			let import = (paths.as_slice(), from.as_deref());
			import_command(library, import, json, environment, output)
		}
		Command::Type { command } => type_command(library, command, json, environment, output),
		Command::Space { command } => space_command(library, command, cli, environment, output),
		Command::Device { command } => {
			let directory = folder.join(LIBRARY_DATA);
			device_command(
				library,
				(command, &directory),
				cli,
				environment,
				timings,
				output,
			)
		}
		Command::Sync {
			serve: false, wait, ..
		} => {
			let wait = Duration::from_secs(*wait);
			sync_command(library, cli, wait, environment, output)
		}
		Command::Sync { serve: true, .. } => unreachable!("nib sync --serve closes the library"),
		Command::Mcp {
			action: McpAction::Serve { .. },
		} => unreachable!("nib mcp serve opens the library itself"),
		Command::Mcp { action } => mcp::manage(library, action, folder, cli, environment, output),
		command if command.is_of_trees() => {
			tree_command(library, cli, folder, environment, timings, output)
		}
		// What makes a library, and safekeeping, purges and commands nib does not have, which
		// run_command runs.
		_ => unreachable!("run_command runs it"),
	}
}

/// Runs the commands of trees, and `nib status`, which shows a tree's status in its folder.
fn tree_command<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	folder: &Path,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	match &cli.command {
		Command::Init {
			folder: target,
			message,
			yes,
			..
		} => {
			let start = (target.as_deref(), message.as_deref(), *yes);
			trees::init_command(
				library,
				(folder, start),
				json,
				(environment, timings),
				output,
			)
		}
		Command::Status { secrets: true, .. } => {
			trees::secrets_command(library, json, environment, output)
		}
		Command::Status { library_wide, .. } => {
			if !*library_wide && trees::in_tree_here(library, environment)? {
				return trees::status_command(library, json, environment, output);
			}
			reading_command(library, cli, folder, environment, output)
		}
		Command::Trees(command) => trees::tree_command(
			library,
			(folder, command),
			json,
			(environment, timings),
			output,
		),
		Command::Pull {
			wait,
			partial,
			clone,
			no_clone,
		} => {
			let wait = Duration::from_secs(*wait);
			let update = (*partial, *clone, *no_clone);
			pull_command(library, cli, (wait, update), environment, timings, output)
		}
		_ => unreachable!("only the commands of trees come here"),
	}
}

/// How long a pairing code works.
const PAIRING_MINUTES: u64 = 10;

/// How long a new device looks for the one that shows its code.
const FIND_SPONSOR: Duration = Duration::from_mins(1);

/// Runs `nib device pair`: shows a code, and waits for a new device, or an AI agent's
/// replica, to pair with it and to sync for the first time.
fn pair_command<E: Environment>(
	library: &mut Library,
	((ai_agent, spaces, give_secrets), device): (
		(Option<&str>, &[nibkeep_base::SpaceId], bool),
		&nibkeep_node::NewDevice,
	),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	// Before showing a code that could not be used.
	library.check_not_ai_agent("add devices")?;
	if ai_agent.is_none() {
		library.check_new_device(device)?;
	}
	// Unlocked first, the secrets are given to a new device that asks for them.
	if give_secrets {
		unlock_secrets(library, false, environment, timings)?;
	}
	// The program that syncs this device stops its endpoint while this one pairs; nib stops
	// waiting for it only as it ends.
	let _borrowed =
		nibkeep_node::borrow_network(library, &AtomicBool::new(false), UnwrapErr(SysRng))?;
	let code = PairingCode::generate(environment.rng());
	let network = library.network(cli.network(true))?;
	output::port_taken(&network, json, output)?;
	output::pairing_code(&code, PAIRING_MINUTES, json, output)?;
	let now = clock(environment);
	// Each try that fails before the code matched shows as the code waits on; one that cannot
	// be shown stops nothing.
	let printing = RefCell::new(output);
	let wrong_try = |error: &NodeError, left: usize| {
		let _ = output::wrong_try(error, left, json, &mut **printing.borrow_mut());
	};
	let showing = nibkeep_node::Showing {
		timeout: Duration::from_mins(PAIRING_MINUTES),
		wrong_try: &wrong_try,
	};
	let paired = match ai_agent {
		Some(name) => nibkeep_node::pair_ai_agent_in(
			library,
			&network,
			&code,
			(name, spaces),
			showing,
			&now,
			environment.rng(),
		),
		None => nibkeep_node::pair_device(
			library,
			&network,
			&code,
			(device, give_secrets),
			showing,
			&now,
			environment.rng(),
		),
	};
	library.lock_secrets();
	output::paired(&paired?, json, printing.into_inner())
}

/// Runs `nib device`.
fn device_command<E: Environment>(
	library: &mut Library,
	(command, directory): (&DeviceCommand, &Path),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		DeviceCommand::List => {
			// What the program that syncs this device recorded, as far as it can be read.
			let records = library.sync_records(UnwrapErr(SysRng)).ok();
			let recorded = records.as_ref().and_then(|records| records.replicas().ok());
			let holder = records.and_then(|records| records.holder().ok().flatten());
			let zone = environment.time_zone();
			let synced = (holder.as_ref(), recorded.as_ref());
			let replicas = library.replicas()?;
			let this = replicas
				.iter()
				.find(|replica| replica.this)
				.map(|replica| replica.id);
			let at_login = at_login_state((directory, this), environment);
			let shown = (synced, at_login.as_ref());
			output::replicas(&replicas, shown, &zone, cli.json, output)
		}
		DeviceCommand::Pair {
			ai_agent,
			phone,
			spaces,
			give_secrets,
			..
		} => {
			let mut chosen = Vec::new();
			for space in spaces {
				chosen.push(library.find_space(space)?);
			}
			let device = nibkeep_node::NewDevice {
				phone: *phone,
				spaces: (!spaces.is_empty()).then(|| chosen.clone()),
			};
			let pairing = (ai_agent.as_deref(), &chosen[..], *give_secrets);
			pair_command(
				library,
				(pairing, &device),
				cli,
				environment,
				timings,
				output,
			)
		}
		DeviceCommand::Revoke { device, yes, codes } => revoke_command(
			library,
			(device, *yes, *codes),
			cli.json,
			environment,
			output,
		),
		DeviceCommand::Mark { .. } => mark_command(library, command, cli.json, environment, output),
		DeviceCommand::Review => output::set_aside(
			&library.set_aside()?,
			&library.replicas()?,
			cli.json,
			output,
		),
		DeviceCommand::Accept { device, yes } => {
			accept_command(library, (device, *yes), cli.json, environment, output)
		}
	}
}

/// Runs `nib device mark`: marks a device as the person says, or says how it is marked.
fn mark_command<E: Environment>(
	library: &mut Library,
	command: &DeviceCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let DeviceCommand::Mark {
		device,
		archive,
		everyday,
		away,
		home,
		keeps,
		waits,
		new_spaces,
	} = command
	else {
		unreachable!("only nib device mark comes here");
	};
	let (chosen, name) = one_device(library, device)?;
	// Whether it takes new spaces is said on the device itself, by its own key.
	let this = library.user_and_device()?.1.id == chosen;
	if let Some(new_spaces) = new_spaces {
		if !this {
			return Err(CliError::NewSpacesElsewhere(
				name.clone().unwrap_or_else(|| device.clone()),
			));
		}
		let now = environment.now();
		library.set_takes_new_spaces(*new_spaces == NewSpacesArg::Take, now, environment.rng())?;
	}
	let takes = if this {
		Some(library.takes_new_spaces()?)
	} else {
		None
	};
	let mut marks = library.device_marks(chosen)?;
	if let Some(Waits(waits)) = waits {
		marks.waits = *waits;
	}
	if *archive {
		marks.role = DeviceRole::Archive;
	}
	if *everyday {
		marks.role = DeviceRole::Everyday;
	}
	if *away {
		marks.away = true;
	}
	if *home {
		marks.away = false;
	}
	if let Some(Keeps(keeps)) = keeps {
		marks.keeps = *keeps;
	}
	// Without a mark given, it only says how the device is marked.
	if *archive || *everyday || *away || *home || keeps.is_some() || waits.is_some() {
		library.set_device_marks(chosen, marks, environment.now(), environment.rng())?;
	}
	output::device_marked(
		(name.as_deref(), chosen),
		(marks, takes, new_spaces.is_some()),
		json,
		output,
	)
}

/// Runs `nib device revoke`: with `yes`, revokes a device's replicas, and with `codes` the
/// recovery codes that they made; without it, says what it would revoke.
fn revoke_command<E: Environment>(
	library: &mut Library,
	(device, yes, codes): (&str, bool, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let mut replicas = library.to_revoke(device)?;
	let ids: Vec<_> = replicas.iter().map(|replica| replica.id).collect();
	// The spaces that this device does not hold, where the devices that do revoke them: only
	// named, so that a failure to read them never stops a revocation.
	let out_of_reach = library.out_of_reach(&ids).unwrap_or_default();
	// The recovery codes made on it, which it names, and revokes as asked.
	let mut made = library.recovery_codes_made_by(&ids)?;
	let mut elsewhere = Vec::new();
	if yes && !replicas.is_empty() {
		let spaces = library.revoke(&ids, environment.now(), environment.rng())?;
		elsewhere = spaces
			.iter()
			.copied()
			.zip(space_names(library, &spaces))
			.collect();
		elsewhere.sort_by(|a, b| a.1.cmp(&b.1));
		if codes {
			for code in &made {
				let now = environment.now();
				library.revoke_recovery_code(&code.record.to_string(), now, environment.rng())?;
			}
			let records: Vec<_> = made.iter().map(|code| code.record).collect();
			made = library
				.recovery_codes()?
				.into_iter()
				.filter(|code| records.contains(&code.record))
				.collect();
		}
		// As the library lists them now, revoked.
		replicas = library
			.replicas()?
			.into_iter()
			.filter(|replica| ids.contains(&replica.id))
			.collect();
	}
	output::revoked(
		&replicas,
		(&made, codes),
		(yes, &elsewhere),
		&out_of_reach,
		json,
		output,
	)?;
	if yes || replicas.is_empty() {
		Ok(())
	} else {
		Err(CliError::UnconfirmedRevoke)
	}
}

/// Runs `nib device accept`: accepts the changes of a device's replicas that were set
/// aside, or with `confirmed` false, says how many it would.
fn accept_command<E: Environment>(
	library: &mut Library,
	(device, confirmed): (&str, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let named: Vec<_> = library
		.replicas_named(device)?
		.into_iter()
		.map(|replica| replica.id)
		.collect();
	let changes: Vec<_> = library
		.set_aside()?
		.into_iter()
		// Only what accepting makes count: never what says who may write to the library.
		.filter(|aside| {
			aside.acceptable
				&& aside
					.replica
					.is_some_and(|replica| named.contains(&replica))
		})
		.map(|aside| aside.change)
		.collect();
	let accepted = if confirmed {
		library.accept_set_aside(&changes, environment.now(), environment.rng())?
	} else {
		changes.len()
	};
	output::accepted(accepted, confirmed, json, output)?;
	if confirmed || accepted == 0 {
		Ok(())
	} else {
		Err(CliError::UnconfirmedAccept)
	}
}

/// Runs `nib sync`: syncs once with each of the user's other devices found on the network.
fn sync_command<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	wait: Duration,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	// A device that sees itself revoked still syncs: a revocation it learns of may set that
	// one aside.
	// Without another device, there is nothing to look for on the network.
	if library.peers()?.is_empty() {
		return Err(NodeError::NoOtherReplicas.into());
	}
	// A program that syncs this device for all is asked to sync now; one that holds the role
	// for a moment, as another nib sync, is waited for.
	let waited = Instant::now();
	let _role = loop {
		if let Some(role) = library.take_role()? {
			break role;
		}
		if library.sync_records(UnwrapErr(SysRng))?.holder()?.is_some() {
			if let Some(asked) = ask_to_sync(library, wait, cli, environment, output)? {
				return asked;
			}
			continue;
		}
		if waited.elapsed() > wait + ASK_WAIT {
			return Err(CliError::NotAnswered("another program".to_owned()));
		}
		std::thread::sleep(Duration::from_millis(100));
	};
	// It only connects, so that a device that connects to it learns at once to wait for
	// another that answers, with nib sync --serve.
	let network = library.network(cli.network(false))?;
	output::progress("Looking for your other devices…", json, output)?;
	let now = clock(environment);
	let synced = nibkeep_node::sync_with_others(library, &network, wait, &now, environment.rng())?;
	output::synced_with(&synced, json, output)?;
	if library.is_revoked()? {
		return Err(NodeError::Revoked.into());
	}
	match synced.iter().filter(|with| with.synced.is_err()).count() {
		0 => Ok(()),
		failed => Err(CliError::SyncsFailed(failed)),
	}
}

/// Runs `nib pull`: syncs, then brings the copy of the tree here up to date, whatever the
/// sync did, and then says how the sync failed, if it did. With --json, one object holds
/// what each printed: `sync`, `null` where it failed before it synced, and `update`.
fn pull_command<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	(wait, update): (Duration, (bool, bool, bool)),
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	let mut synced = Vec::new();
	let sync = if json {
		sync_command(library, cli, wait, environment, &mut synced)
	} else {
		sync_command(library, cli, wait, environment, output)
	};
	let mut updated = Vec::new();
	if let Err(error) =
		trees::update_here(library, update, json, (environment, timings), &mut updated)
	{
		if let Err(failed) = &sync {
			environment.note(&format!("the sync failed too: {failed}"));
		}
		return Err(error);
	}
	if json {
		let read = |printed: &[u8]| -> Result<serde_json::Value, CliError> {
			if printed.iter().all(u8::is_ascii_whitespace) {
				return Ok(serde_json::Value::Null);
			}
			serde_json::from_slice(printed).map_err(|error| CliError::Io(io::Error::other(error)))
		};
		let pulled = serde_json::json!({ "sync": read(&synced)?, "update": read(&updated)? });
		output::print_json(&pulled, output)?;
	} else {
		output.write_all(&updated)?;
	}
	sync
}

/// How long `nib sync` waits for the program that syncs this device to answer, beyond the
/// time it is told to look for the other devices: a sync takes a while.
const ASK_WAIT: Duration = Duration::from_mins(2);

/// Asks the program that holds this device's sync role to sync with all the other devices
/// it finds, waits until it did, and says what happened, as it recorded it: the result of
/// `nib sync`. `None` if the holder let go of the role first, for `nib sync` to sync itself.
fn ask_to_sync<E: Environment>(
	library: &mut Library,
	wait: Duration,
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<Option<Result<(), CliError>>, CliError> {
	let json = cli.json;
	let records = library.sync_records(UnwrapErr(SysRng))?;
	let Some(holder) = records.holder()? else {
		return Ok(None);
	};
	output::progress(&format!("Asking {} to sync…", holder.program), json, output)?;
	let asked = environment.now();
	let ask = records.ask(None, asked)?;
	let waited = Instant::now();
	while records.answered(ask)?.is_none() {
		// The holder ended, even by a crash, which let go of the role.
		if library.take_role()?.is_some() {
			return Ok(None);
		}
		if waited.elapsed() > wait + ASK_WAIT {
			return Err(CliError::NotAnswered(holder.program));
		}
		std::thread::sleep(Duration::from_millis(100));
	}
	// What the holder's sync brought, as a revocation of this device.
	library.catch_up()?;
	let failed = output::asked(
		&library.peers()?,
		&records.synced_since(asked)?,
		json,
		output,
	)?;
	if library.is_revoked()? {
		return Err(NodeError::Revoked.into());
	}
	Ok(Some(match failed {
		0 => Ok(()),
		failed => Err(CliError::SyncsFailed(failed)),
	}))
}

/// How a command makes a library on this computer.
enum Making<'a> {
	/// A new library, `nib init`, with the user's name, if given, and languages.
	New(Option<&'a str>, Option<&'a Languages>),
	/// A copy of a library on another device, `nib init --join`, and whether to ask for its
	/// secrets.
	Join(&'a PairingCode, bool),
	/// The library brought back with a recovery code, `nib recovery restore`, from a source,
	/// keeping these devices.
	Recover(Option<&'a Path>, &'a [String]),
	/// A device of its own from a copy, `nib recovery from-copy`.
	FromCopy,
}

/// Runs `nib init` that makes a library or joins one, or `nib recovery` that brings one back,
/// once it is clear that a new replica can go where the library is.
fn new_library<E: Environment>(
	directory: &Path,
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<Library, CliError> {
	let (making, device, passphrase, allow_fat) = match &cli.command {
		Command::Init {
			name,
			device,
			passphrase,
			languages,
			join,
			secrets,
			allow_fat,
			..
		} => {
			let making = match (join, name) {
				(Some(code), _) => Making::Join(code, *secrets),
				(None, name) => Making::New(name.as_deref(), languages.as_ref()),
			};
			(making, device.as_deref(), *passphrase, *allow_fat)
		}
		Command::Recovery {
			command: RecoveryCommand::Restore { source, keep, new },
		} => (
			Making::Recover(source.as_deref(), keep),
			new.device.as_deref(),
			new.passphrase,
			new.allow_fat,
		),
		Command::Recovery {
			command: RecoveryCommand::FromCopy { new },
		} => (
			Making::FromCopy,
			new.device.as_deref(),
			new.passphrase,
			new.allow_fat,
		),
		_ => unreachable!("only nib init and nib recovery make a library"),
	};
	if allow_fat {
		Library::allow_fat(directory)?;
	}
	// Where a new replica would go, before any passphrase is asked for.
	if matches!(
		Library::replica_folder(directory),
		Err(NodeError::NoLibrary | NodeError::Copied)
	) {
		Library::check_place(directory).map_err(|error| no_journal(error.into(), directory))?;
	}
	let protection = (passphrase, cli.passphrase_file.as_deref());
	match making {
		Making::Join(code, secrets) => join_command(
			directory,
			(code, device, secrets),
			protection,
			cli,
			environment,
			timings,
			output,
		),
		Making::New(name, languages) => init_command(
			directory,
			(name, device, languages),
			protection,
			environment,
			timings,
		),
		Making::Recover(source, keep) => recover_command(
			directory,
			(source, device, keep),
			protection,
			cli,
			environment,
			timings,
			output,
		),
		Making::FromCopy => from_copy_command(
			directory,
			device,
			protection,
			cli,
			environment,
			timings,
			output,
		),
	}
	.map_err(|error| no_journal(error, directory))
}

/// Runs `nib sync --at-login on` or `off`: turns syncing this library from login on or off
/// ([technical design](../../docs/Technical-design.md#106-syncing-from-login)). On, it adds
/// the library to the list, names the copy of `nibkeep-sync` beside `nib` if it is newer
/// than the one listed, and starts the copy that should run. Off, it takes the library off
/// the list by its folder, which may hold no library any more, or by its replica, as after
/// its folder was moved.
fn at_login_command<E: Environment>(
	switch: Switch,
	directory: &Path,
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let state = environment.state_folder().ok_or(CliError::NoStateFolder)?;
	let directory = std::path::absolute(directory)?;
	let keystore = matches!(
		Library::protection(&directory),
		Ok(Protection::Keystore { .. })
	);
	if switch == Switch::Off {
		let replica = if keystore {
			open(&directory, None, environment, timings)
				.ok()
				.and_then(|library| own_replica(&library).ok())
		} else {
			None
		};
		let entry = environment.login_entry();
		let turned_off = at_login::turn_off(&state, (&directory, replica), entry.as_ref())?;
		return output::at_login_off(&turned_off, cli.json, output);
	}
	// Before anything is asked for: nobody is there to type a passphrase at login.
	if !keystore {
		Library::protection(&directory).map_err(|error| opening(error, &directory))?;
		return Err(CliError::AtLoginPassphrase);
	}
	let program = environment.sync_program().ok_or(CliError::NoSyncProgram)?;
	let library = open(&directory, None, environment, timings)
		.map_err(|error| opened_error(error, &directory))?;
	let served = at_login::Served {
		replica: own_replica(&library)?,
		directory,
	};
	drop(library);
	let entry = environment.login_entry();
	let version_of = |listed: &Path| environment.sync_version(listed);
	let turned_on = at_login::turn_on(&state, served, (&program, &version_of), entry.as_ref())
		.map_err(|error| match error {
			NodeError::NotInKeystore => CliError::AtLoginPassphrase,
			other => CliError::Node(other),
		})?;
	// The firewall first, so that Windows does not ask about the copy that starts, whose
	// refusal would block it.
	let firewall = environment.open_firewall();
	if let Some(start) = &turned_on.start {
		environment.start_sync(start)?;
	}
	output::at_login_on(&turned_on.entry, firewall, cli.json, output)
}

/// The ID of the library's own replica.
fn own_replica(library: &Library) -> Result<EntityId, CliError> {
	library
		.replicas()?
		.into_iter()
		.find(|replica| replica.this)
		.map(|replica| replica.id)
		.ok_or_else(|| {
			CliError::Node(NodeError::Damaged(
				"the library lists no replica of its own".to_owned(),
			))
		})
}

/// The error of a library that could not be opened, as the person is told it: where its
/// start was cut short, it holds only copies, or its drive must be checked, with what to do.
fn opening(error: NodeError, directory: &Path) -> CliError {
	match error {
		NodeError::Unfinished => unfinished(directory),
		NodeError::Copied => CliError::IsCopy(directory.to_owned()),
		NodeError::UncleanRemoval => CliError::UncleanRemoval(directory.to_owned()),
		other => CliError::Node(other),
	}
}

/// [`opening`], for an error that opening a library returned.
fn opened_error(error: CliError, directory: &Path) -> CliError {
	match error {
		CliError::Node(error) => opening(error, directory),
		other => other,
	}
}

/// Turns syncing from login off for the library in `directory`, if it was on, and says so:
/// as best it can, since the library's new protection is in place already.
fn turn_off_at_login<E: Environment>(directory: &Path, environment: &mut E) {
	let Some(state) = environment.state_folder() else {
		return;
	};
	let entry = environment.login_entry();
	let turned_off = std::path::absolute(directory)
		.map_err(CliError::from)
		.and_then(|directory| {
			Ok(at_login::turn_off(
				&state,
				(&directory, None),
				entry.as_ref(),
			)?)
		});
	match turned_off {
		Ok(turned_off) => {
			if turned_off.removed {
				environment.note(AT_LOGIN_TURNED_OFF);
			}
			if let Some(error) = &turned_off.entry_failed {
				environment.note(&entry_not_removed(error));
			}
		}
		Err(error) => environment.note(&format!(
			"nib: could not turn syncing from login off for this library ({error}); nibkeep-sync turns it off as it finds the passphrase"
		)),
	}
}

/// What `nib` says when the login entry could not be removed with the last library.
fn entry_not_removed(error: &str) -> String {
	format!(
		"nib: the login entry could not be removed ({error}); nib sync --at-login off tries again"
	)
}

/// What `nib` says when protecting a library with a passphrase turned syncing from login off.
const AT_LOGIN_TURNED_OFF: &str = "nib: syncing from login is off for this library now, as it needs the library's keys in the keystore";

/// Whether the library in `directory`, or with `replica` here, is synced from login, and
/// what `nibkeep-sync` does for it: `None` if it is not, or that cannot be read.
fn at_login_state<E: Environment>(
	(directory, replica): (&Path, Option<EntityId>),
	environment: &E,
) -> Option<at_login::AtLoginStatus> {
	let state = environment.state_folder()?;
	let directory = std::path::absolute(directory).ok()?;
	let entry = environment.login_entry();
	let status = at_login::status(&state, (&directory, replica), entry.as_ref());
	status.on.then_some(status)
}

/// Runs `nib sync --serve`: holds this device's sync role, or waits for the program that
/// holds it to end and takes over, and syncs for every program here until stopped
/// (technical design: the sync role).
fn serve_command<E: Environment>(
	library: Library,
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	let closed = library.close();
	let stop = Arc::new(AtomicBool::new(false));
	environment.stop_on_interrupt(Arc::clone(&stop));
	let role = if let Some(role) = Role::take(&closed)? {
		role
	} else {
		let program = closed
			.open()?
			.sync_records(UnwrapErr(SysRng))?
			.holder()?
			.map_or_else(|| "Another program".to_owned(), |holder| holder.program);
		output::progress(
			&format!(
				"{program} syncs this device; this one takes over when it ends. Stop with Ctrl-C."
			),
			json,
			output,
		)?;
		// Stopped before it took over: nothing to let go of.
		let Some(role) = Role::wait(&closed, &stop)? else {
			return Ok(());
		};
		role
	};
	output::progress(
		"Syncing this device with your other devices; stop with Ctrl-C.",
		json,
		output,
	)?;
	let now = clock(environment);
	let holding = Holding {
		program: "nib sync --serve",
		network: cli.network(true),
		known: Vec::new(),
		quiet: Holding::QUIET,
		appear_wait: Holding::APPEAR_WAIT,
		check: nibkeep_node::Pace::DEFAULT,
		stop: &stop,
		now: &now,
	};
	// Output that fails, as into a pipe closed, stops it too.
	let mut printed = Ok(());
	nibkeep_node::hold(
		&closed,
		role,
		&holding,
		&|| UnwrapErr(SysRng),
		&mut |held| {
			if printed.is_ok() {
				printed = output::held(&held, json, output);
				if printed.is_err() {
					stop.store(true, std::sync::atomic::Ordering::SeqCst);
				}
			}
		},
	)?;
	printed
}

/// Runs `nib init --join`: pairs with the device that shows the code, which makes the
/// library here, and syncs with it.
fn join_command<E: Environment>(
	directory: &Path,
	(code, device, secrets): (&PairingCode, Option<&str>, bool),
	(passphrase, file): (bool, Option<&Path>),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<Library, CliError> {
	// Copies of replicas made elsewhere stay, and this one goes beside them, and takes in
	// what they hold once it has joined.
	if Library::exists(directory) {
		match Library::protection(directory) {
			Err(NodeError::Copied) => {}
			Err(NodeError::Unfinished) => return Err(unfinished(directory)),
			_ => return Err(NodeError::Exists.into()),
		}
	}
	let device = device_name(device, environment);
	let passphrase = new_passphrase((passphrase, file), environment, timings)?;
	let protection = match &passphrase {
		Some(passphrase) => NewProtection::Passphrase(passphrase, environment.passphrase_params()),
		None => NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
	};
	// Before the code is used.
	let secrets_passphrase = if secrets {
		let typed = ask_twice(
			environment,
			timings,
			(
				"A passphrase for the secrets on this device, not the library's: ",
				"The passphrase again: ",
			),
			(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
		)?;
		Some(typed)
	} else {
		None
	};
	let json = cli.json;
	let secrets_params = environment.passphrase_params();
	let mut joining = nibkeep_node::Joining::new(&device, APP, environment.rng())?;
	// With copies here, the secrets come from them, once this device has taken in what
	// they hold, rather than from the device that shows the code.
	let beside_copies = !Library::copies_in(directory).is_empty();
	if let Some(typed) = secrets_passphrase.as_ref().filter(|_| !beside_copies) {
		let protection = NewProtection::Passphrase(typed, secrets_params);
		joining = joining.asking_for_secrets(protection, environment.rng());
	}
	let network = joining.network(cli.network(false))?;
	output::progress("Looking for the device that shows the code…", json, output)?;
	let now = clock(environment);
	let joined = nibkeep_node::join(
		directory,
		joining,
		&network,
		code,
		&protection,
		FIND_SPONSOR,
		&now,
		environment.rng(),
	)
	.map_err(|error| match error {
		error @ NodeError::Keys(KeysError::Keystore(_)) => CliError::Keystore(error),
		other => CliError::Node(other),
	})?;
	// What became of the secrets it asked for: taken with the first sync, asked for, or none.
	let mut library = joined.library;
	library.lock_secrets();
	let secrets = if secrets {
		Some((library.secrets_state()?, library.is_ai_agent()?))
	} else {
		None
	};
	let copies = take_in_copies(&mut library, environment)?;
	// The device joined: if the copy's secrets cannot be taken now, nib secret from-copy
	// takes them later.
	if let (Some(typed), Some(copies)) = (&secrets_passphrase, &copies) {
		let protection = NewProtection::Passphrase(typed, secrets_params);
		if let Err(error) = secrets_from_copies(
			&mut library,
			&copies.folders,
			&protection,
			environment,
			timings,
		) {
			environment.note(&format!(
				"the copy's secrets were not taken: {error}; nib secret from-copy takes them"
			));
		}
	}
	let secrets = match secrets {
		Some(_) => Some((library.secrets_state()?, library.is_ai_agent()?)),
		None => None,
	};
	output::joined(
		(&joined.synced, joined.refused),
		secrets,
		copies.as_ref(),
		json,
		output,
	)?;
	Ok(library)
}

/// Takes the secrets from the first of `copies` that holds them, which the person unlocks:
/// with the keystore that holds their key, or the passphrase of the copy's secrets, which
/// the person may leave empty to take none. Returns how many keys it took.
fn secrets_from_copies<E: Environment>(
	library: &mut Library,
	copies: &[PathBuf],
	protection: &NewProtection<'_>,
	environment: &mut E,
	timings: &mut Timings,
) -> Result<usize, CliError> {
	for folder in copies {
		let copy = nibkeep_node::ReplicaCopy {
			folder: folder.clone(),
			replica: None,
		};
		let Ok((_, Some(kept))) = copy.protections() else {
			continue;
		};
		let typed;
		let unlocker = match kept {
			Protection::Keystore { .. } => Unlocker::Keystore,
			Protection::Passphrase { .. } => {
				typed = ask_once(
					environment,
					timings,
					"The passphrase of the copy's secrets, or nothing to leave them: ",
				)?;
				if typed.is_empty() {
					return Ok(0);
				}
				Unlocker::Passphrase(&typed)
			}
		};
		let now = environment.now();
		return Ok(library.take_secrets_from_copy(
			folder,
			&unlocker,
			protection,
			now,
			environment.rng(),
		)?);
	}
	Ok(0)
}

/// Runs `nib secret from-copy`: takes the secrets from a copy beside this device's replica.
fn from_copy_secrets_command<E: Environment>(
	library: &mut Library,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let copies: Vec<PathBuf> = library
		.copies()
		.into_iter()
		.map(|copy| copy.folder)
		.collect();
	let typed = ask_twice(
		environment,
		timings,
		(
			"A passphrase for the secrets on this device, not the library's: ",
			"The passphrase again: ",
		),
		(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
	)?;
	let protection = NewProtection::Passphrase(&typed, environment.passphrase_params());
	let taken = secrets_from_copies(library, &copies, &protection, environment, timings);
	library.lock_secrets();
	match taken? {
		0 => Err(CliError::NoSecretsInCopies),
		taken => output::progress(
			&format!("Took {taken} secrets keys from the copy: this device holds the secrets."),
			json,
			output,
		),
	}
}

/// Runs `nib recovery from-copy`: makes this computer a device of its own from the copy in the
/// library's folder, with the copy's own keys, where no other device can add it.
fn from_copy_command<E: Environment>(
	directory: &Path,
	device: Option<&str>,
	(passphrase, file): (bool, Option<&Path>),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<Library, CliError> {
	if Library::replica_folder(directory).is_ok() {
		return Err(NodeError::Exists.into());
	}
	let copies = Library::copies_in(directory);
	let copy = match copies.as_slice() {
		[copy] => copy,
		[] => return Err(CliError::NoCopy(directory.to_owned())),
		_ => return Err(CliError::SeveralCopies(directory.to_owned())),
	};
	let (kept, secrets_kept) = copy.protections()?;
	let typed;
	let unlocker = match kept {
		Protection::Keystore { .. } => Unlocker::Keystore,
		Protection::Passphrase { .. } => {
			typed = ask_once(
				environment,
				timings,
				"The passphrase of the copy, the library's: ",
			)?;
			Unlocker::Passphrase(&typed)
		}
	};
	let device = device_name(device, environment);
	let new_passphrase = new_passphrase((passphrase, file), environment, timings)?;
	let protection = match &new_passphrase {
		Some(passphrase) => NewProtection::Passphrase(passphrase, environment.passphrase_params()),
		None => NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
	};
	// The copy's secrets, which only it may hold, are taken before its device is revoked.
	let mut copy_secrets = None;
	let secrets_typed;
	if let Some(kept) = secrets_kept {
		let typed = match kept {
			Protection::Keystore { .. } => None,
			Protection::Passphrase { .. } => Some(ask_once(
				environment,
				timings,
				"The passphrase of the copy's secrets, or nothing to leave them: ",
			)?),
		};
		if typed.as_ref().is_none_or(|typed| !typed.is_empty()) {
			let new = ask_twice(
				environment,
				timings,
				(
					"A passphrase for the secrets on this device, not the library's: ",
					"The passphrase again: ",
				),
				(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
			)?;
			secrets_typed = (typed, new);
			copy_secrets = Some(&secrets_typed);
		}
	}
	let secrets_params = environment.passphrase_params();
	let secrets_unlocker = copy_secrets.map(|(typed, _)| match typed {
		Some(typed) => Unlocker::Passphrase(typed),
		None => Unlocker::Keystore,
	});
	let secrets_protection =
		copy_secrets.map(|(_, new)| NewProtection::Passphrase(new, secrets_params));
	let joining = nibkeep_node::Joining::new(&device, APP, environment.rng())?;
	let now = clock(environment);
	let library = Library::add_from_copy(
		directory,
		(&copy.folder, &unlocker),
		joining,
		&protection,
		secrets_unlocker.as_ref().zip(secrets_protection.as_ref()),
		&now,
		environment.rng(),
	)?;
	output::progress(
		"Made this device a device of its own from the copy, whose device is revoked. Devices of yours that turn up later are added anew.",
		cli.json,
		output,
	)?;
	Ok(library)
}

/// Runs `nib recovery restore`: brings the library back on this computer, as a new device of
/// the person's, from the backup or the library in `source`, or else the library here, and
/// the recovery code that the person types, checked against the source before anything else
/// is asked; the other devices are revoked, but those that `keep` names, or else those that
/// the person says they still have.
fn recover_command<E: Environment>(
	directory: &Path,
	(source, device, keep): (Option<&Path>, Option<&str>, &[String]),
	(passphrase, file): (bool, Option<&Path>),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<Library, CliError> {
	// Without a source, the library here, which no longer unlocks.
	let source = match source {
		Some(source) => absolute(source, environment)?,
		None => directory.to_owned(),
	};
	// Before anything is asked: a library here stays, but for what a recovery cut short left,
	// or the library that the recovery replaces.
	let replaces = Library::exists(directory) && Library::recovery_replaces(directory, &source);
	if Library::exists(directory) && !replaces {
		match Library::protection(directory) {
			Err(NodeError::Copied | NodeError::RecoveryCutShort) => {}
			Err(NodeError::Unfinished) => return Err(unfinished(directory)),
			_ => return Err(NodeError::Exists.into()),
		}
	}
	// One that still unlocks with the keystore has nothing to recover, which a passphrase
	// does not tell.
	if replaces
		&& matches!(
			Library::protection(directory),
			Ok(Protection::Keystore { .. })
		)
		&& environment.keystore()
		&& Library::open(directory, &Unlocker::Keystore).is_ok()
	{
		return Err(CliError::StillUnlocks);
	}
	let code = typed_recovery_code("Your recovery code: ", environment, cli.json, output)?;
	Library::check_recovery_source(&source, &code)?;
	let device = device_name(device, environment);
	let new_passphrase = new_passphrase((passphrase, file), environment, timings)?;
	let protection = match &new_passphrase {
		Some(passphrase) => NewProtection::Passphrase(passphrase, environment.passphrase_params()),
		None => NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
	};
	let joining = nibkeep_node::Joining::new(&device, APP, environment.rng())?;
	let now = clock(environment);
	let mut recovering = Library::start_recovery(
		directory,
		(&source, &code),
		joining,
		&protection,
		&now,
		environment.rng(),
	)?;
	// The devices to keep, first, as a name that --keep does not find ends the recovery;
	// then the secrets' passphrase, asked only of a code that holds them. A recovery left
	// unfinished removes the new device.
	let kept = devices_kept(&recovering, keep, cli.json, environment)?;
	recovering.keep(&kept)?;
	let secrets_passphrase = if recovering.holds_secrets() {
		Some(recovered_secrets_passphrase(
			environment,
			timings,
			new_passphrase.as_deref().map(String::as_str),
		)?)
	} else {
		None
	};
	let secrets_params = environment.passphrase_params();
	let secrets = secrets_passphrase
		.as_ref()
		.map(|typed| NewProtection::Passphrase(typed, secrets_params));
	let (mut library, recovered) = recovering.finish(secrets.as_ref(), &now, environment.rng())?;
	library.lock_secrets();
	let spaces = sorted_space_names(&library, &recovered.spaces);
	let elsewhere = sorted_space_names(&library, &recovered.revoked_elsewhere);
	// The room that the old library's folder takes, which the person deletes.
	let old_size = match &recovered.from {
		nibkeep_node::RecoveredFrom::Library {
			set_aside, folder, ..
		} => folder_size(set_aside.as_ref().unwrap_or(folder)),
		nibkeep_node::RecoveredFrom::Backup { .. } => 0,
	};
	output::recovered(
		&recovered,
		(&spaces, &elsewhere, old_size),
		cli.json,
		output,
	)?;
	Ok(library)
}

/// The replicas of the person's other devices that a recovery keeps: those of the devices
/// that `named` names, with --keep; without it, those of the devices that the person says
/// they still have, asked about each, where nib can ask, which it does not with --json, as
/// `json` says; otherwise none, as every other device is lost.
fn devices_kept<E: Environment>(
	recovering: &nibkeep_node::Recovering<'_>,
	named: &[String],
	json: bool,
	environment: &mut E,
) -> Result<Vec<EntityId>, CliError> {
	if !named.is_empty() {
		let mut kept = Vec::new();
		for target in named {
			kept.extend(
				recovering
					.others_named(target)?
					.into_iter()
					.map(|replica| replica.id),
			);
		}
		return Ok(kept);
	}
	// With --json, nib runs for a program, which is not asked.
	if json || !environment.can_ask() {
		return Ok(Vec::new());
	}
	// One question for each device, with its replicas, and for each AI agent's replica, by
	// name, and by ID too where two would read alike.
	let mut devices: BTreeMap<EntityId, Vec<Replica>> = BTreeMap::new();
	for replica in recovering.others()? {
		let device = match replica.device {
			Some(device) if !replica.ai_agent => device,
			_ => replica.id,
		};
		devices.entry(device).or_default().push(replica);
	}
	let mut asked: Vec<(String, EntityId, Vec<Replica>)> = devices
		.into_iter()
		.map(|(device, replicas)| (output::kept_device_text(&replicas), device, replicas))
		.collect();
	asked.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
	let alike: Vec<bool> = asked
		.iter()
		.map(|(text, ..)| asked.iter().filter(|(other, ..)| other == text).count() > 1)
		.collect();
	let mut kept = Vec::new();
	for ((text, device, replicas), alike) in asked.into_iter().zip(alike) {
		let text = if alike {
			format!("{text} {}", output::short(device))
		} else {
			text
		};
		let answer = environment.ask(&output::keep_question(&text))?;
		if answer.is_some_and(|answer| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
		{
			kept.extend(replicas.iter().map(|replica| replica.id));
		}
	}
	Ok(kept)
}

/// Asks twice for a passphrase for the secrets that a recovery code brings back, and up to
/// three times, as a mistake here would end the recovery and throw away what it took in so
/// far: one that is empty, typed differently the second time, or that is `library`, the
/// library's passphrase, is asked for again.
fn recovered_secrets_passphrase<E: Environment>(
	environment: &mut E,
	timings: &mut Timings,
	library: Option<&str>,
) -> Result<zeroize::Zeroizing<String>, CliError> {
	let mut tries = 0;
	loop {
		tries += 1;
		let typed = ask_twice(
			environment,
			timings,
			(
				"Your recovery code holds your secrets. A passphrase for them on this device, not the library's: ",
				"The passphrase again: ",
			),
			(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
		);
		let error = match typed {
			Ok(typed) if library.is_some_and(|library| *typed == *library) => {
				CliError::Node(NodeError::LibraryPassphrase)
			}
			Ok(typed) => return Ok(typed),
			Err(error @ (CliError::EmptyPassphrase | CliError::PassphrasesDiffer)) => error,
			Err(error) => return Err(error),
		};
		if tries == 3 {
			return Err(error);
		}
		environment.note(&format!("{error}; type it again"));
	}
}

/// Takes in what the copies beside a new replica hold, if there are any: what a backup, a
/// program that syncs folders, or a person brought from the devices they are copies of.
fn take_in_copies<E: Environment>(
	library: &mut Library,
	environment: &mut E,
) -> Result<Option<output::Copies>, CliError> {
	let copies = library.copies();
	if copies.is_empty() {
		return Ok(None);
	}
	let now = clock(environment);
	let taken = library.take_in_copies(&now)?;
	let replicas = library.replicas()?;
	let mut devices: Vec<String> = copies
		.iter()
		.filter_map(|copy| {
			let (replica, _) = copy.replica?;
			let known = replicas.iter().find(|known| known.id == replica)?;
			known.device_name.clone().or_else(|| known.name.clone())
		})
		.collect();
	devices.sort();
	devices.dedup();
	let size = copies.iter().map(|copy| folder_size(&copy.folder)).sum();
	// Secrets that only a copy holds would go with its device, if it were revoked first.
	let secrets_left = library.secrets_state()? != nibkeep_node::SecretsState::Here
		&& copies.iter().any(|copy| {
			copy.protections()
				.is_ok_and(|(_, secrets)| secrets.is_some())
		});
	Ok(Some(output::Copies {
		taken,
		secrets_left,
		devices,
		folders: copies.into_iter().map(|copy| copy.folder).collect(),
		size,
	}))
}

/// How many bytes the files in a folder take, those in its folders too.
fn folder_size(folder: &Path) -> u64 {
	std::fs::read_dir(folder)
		.into_iter()
		.flatten()
		.flatten()
		.map(|entry| match entry.file_type() {
			Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
			_ => entry.metadata().map_or(0, |metadata| metadata.len()),
		})
		.sum()
}

/// The names of a new library's user and of the person the user is: the name given, for
/// both; or else the account's name, as `whoami` prints it, for the user, and the person's
/// full name, if the system knows one that differs, for the person.
fn user_names<E: Environment>(
	given: Option<&str>,
	environment: &E,
) -> Result<(String, Option<String>), CliError> {
	if let Some(given) = given {
		return Ok((given.to_owned(), None));
	}
	let account = environment.account();
	let user = account
		.name
		.map(|name| name.trim().to_owned())
		.filter(|name| !name.is_empty())
		.ok_or(CliError::NoAccountName)?;
	let person = account
		.full_name
		.map(|name| name.trim().to_owned())
		.filter(|name| !name.is_empty() && *name != user);
	Ok((user, person))
}

/// This device's name: the one given, or else the computer's.
fn device_name<E: Environment>(given: Option<&str>, environment: &E) -> String {
	// A computer without a name still gets a device with one.
	given.map_or_else(
		|| {
			Some(environment.computer_name())
				.filter(|name| !name.is_empty())
				.unwrap_or_else(|| "device".to_owned())
		},
		str::to_owned,
	)
}

/// Reads a pairing code from the command line.
fn parse_code(text: &str) -> Result<PairingCode, String> {
	text.parse()
		.map_err(|error: nibkeep_node::NotACode| error.to_string())
}

/// What `nib sync` says when syncs failed.
fn syncs_failed(failed: usize) -> String {
	if failed == 1 {
		"a device could not be synced with".to_owned()
	} else {
		format!("{failed} devices could not be synced with")
	}
}

/// How many of the entities that a name could be an error lists.
const CANDIDATES_LISTED: usize = 10;

/// The error, with the entities it could be named, by short ID, name and types, if it is
/// about a name that several entities have.
fn candidates_named<E: Environment>(
	library: &Library,
	error: CliError,
	environment: &E,
) -> CliError {
	let CliError::Node(NodeError::Ambiguous {
		reference,
		candidates,
	}) = error
	else {
		return error;
	};
	let personal = library.personal_space();
	let day = dating(environment, environment.now(), None).day;
	let mut listed = Vec::new();
	// The members of each merged entity, read once.
	let mut members: std::collections::BTreeMap<EntityId, Vec<nibkeep_node::Listed>> =
		std::collections::BTreeMap::new();
	for &id in candidates.iter().take(CANDIDATES_LISTED) {
		// Each in its own space.
		let space = library.space_of(id).ok().flatten().unwrap_or(personal);
		let (name, types) = match library.entity(space, id, day) {
			Ok(Some(view)) if view.id == id => (view.name, view.types),
			// An entity merged into another goes by its own name and types.
			Ok(Some(view)) => {
				let of_view = members
					.entry(view.id)
					.or_insert_with(|| library.merged(space, view.id).unwrap_or_default());
				let name = of_view
					.iter()
					.find(|member| member.id == id)
					.and_then(|member| member.name.clone());
				let types = library.own_types(space, id).unwrap_or_default();
				(name.or(view.name), types)
			}
			_ => (None, Vec::new()),
		};
		listed.push(nibkeep_node::Listed {
			id,
			name,
			types,
			via: None,
		});
	}
	CliError::Ambiguous {
		reference,
		count: candidates.len(),
		candidates: output::candidates(&listed, candidates.len()),
	}
}

/// What `nib file add` says when files could not be added.
fn files_failed(failed: usize) -> String {
	if failed == 1 {
		"a file could not be added".to_owned()
	} else {
		format!("{failed} files could not be added")
	}
}

/// What `nib verify --export` says when it finds problems.
fn unsound_export(problems: usize) -> String {
	if problems == 1 {
		"the export has a problem".to_owned()
	} else {
		format!("the export has {problems} problems")
	}
}

/// What `nib verify` says when it finds problems.
fn unsound(problems: usize) -> String {
	if problems == 1 {
		"the library has a problem".to_owned()
	} else {
		format!("the library has {problems} problems")
	}
}

/// The edits that changes on the command line ask for: values that one command sets for a
/// field are set together, before the values it adds and removes.
/// Values added and removed are forms if `forms` says so.
fn edits(
	changes: &[String],
	forms: Forms<'_>,
	view: &EntityView,
	library: &Library,
	space: nibkeep_base::SpaceId,
) -> Result<Vec<Edit>, CliError> {
	let types = library.types(space)?;
	let mut sets: Vec<(nibkeep_model::FieldName, Vec<nibkeep_model::Value>)> = Vec::new();
	let mut cleared = std::collections::BTreeSet::new();
	let mut others = Vec::new();
	// Types first, so that the fields of a type given in the same edit can be set.
	let mut kinds = view.types.clone();
	let (mut given, mut taken) = (Vec::new(), Vec::new());
	let mut rest = Vec::new();
	for change in changes {
		let (left, text) = change
			.split_once('=')
			.ok_or_else(|| CliError::Assignment(change.clone()))?;
		let (field, how) = match (left.strip_suffix('+'), left.strip_suffix('-')) {
			(Some(field), _) => (field, '+'),
			(_, Some(field)) => (field, '-'),
			_ => (left, '='),
		};
		// `type` is a field of the entity's types where one of them has a field of that name;
		// `org.nibkeep.type` is always the types.
		let of_types = field == nibkeep_model::fields::TYPE.as_str()
			|| (field == "type"
				&& matches!(
					types::field_named(field, &types, &kinds),
					Err(CliError::NoField { .. })
				));
		if !of_types {
			rest.push((change, field, how, text));
			continue;
		}
		match how {
			'+' => {
				let name = types::type_named(text, &types)?;
				kinds.push(name.clone());
				given.push(Edit::AddType(name));
			}
			'-' => taken.push(Edit::RemoveType(types::type_of(text, &view.types, &types)?)),
			_ => return Err(CliError::TypeSet),
		}
	}
	for (change, field, how, text) in rest {
		let field = types::field_named(field, &types, &kinds)?;
		if is_secret(library, space, &field) {
			return Err(CliError::SecretField(field));
		}
		if text.is_empty() {
			if how != '=' {
				return Err(CliError::Assignment(change.clone()));
			}
			if sets
				.iter()
				.any(|(set, values)| *set == field && !values.is_empty())
			{
				return Err(CliError::SetAndCleared(field));
			}
			if cleared.insert(field.clone()) {
				sets.push((field, Vec::new()));
			}
			continue;
		}
		let value = types::value(text, &field, &types, library, space)?;
		match how {
			'+' if !matches!(forms, Forms::No) => {
				let of = match forms {
					Forms::Of(of) => Some(types::value(of, &field, &types, library, space)?),
					_ => None,
				};
				others.push(Edit::AddForm { field, of, value });
			}
			'-' if !matches!(forms, Forms::No) => others.push(Edit::RemoveForm(field, value)),
			'+' => others.push(Edit::Add(field, value)),
			'-' => others.push(Edit::Remove(field, value)),
			_ => {
				if cleared.contains(&field) {
					return Err(CliError::SetAndCleared(field));
				}
				match sets.iter_mut().find(|(set, _)| *set == field) {
					Some((_, values)) => values.push(value),
					None => sets.push((field, vec![value])),
				}
			}
		}
	}
	if !matches!(forms, Forms::No)
		&& !others
			.iter()
			.any(|edit| matches!(edit, Edit::AddForm { .. } | Edit::RemoveForm(..)))
	{
		return Err(CliError::NoForms);
	}
	// Types are given before any is taken away, so that an entity can trade its only type.
	Ok(given
		.into_iter()
		.chain(taken)
		.chain(
			sets.into_iter()
				.map(|(field, values)| Edit::Set(field, values)),
		)
		.chain(others)
		.collect())
}

/// The ranked languages that the system's languages make: each language, with its script
/// if the system names one, once, in the system's order. Regions are left out, so that a
/// system set to `en_US` shows English from anywhere; `C` and `POSIX` name no language.
/// The modifiers `@latin` and `@cyrillic` name scripts.
fn system_languages(locales: &[String]) -> Languages {
	let mut ranges: Vec<String> = Vec::new();
	for written in locales {
		// `sr_RS.UTF-8@latin` names the language, the region, the encoding and a modifier.
		let script = match written.rsplit_once('@').map(|(_, modifier)| modifier) {
			Some("latin") => Some("Latn"),
			Some("cyrillic") => Some("Cyrl"),
			_ => None,
		};
		let locale = written.split(['.', '@']).next().unwrap_or_default();
		if locale.is_empty() || locale == "C" || locale == "POSIX" {
			continue;
		}
		let mut subtags = locale.split(['_', '-']);
		let mut range = subtags.next().unwrap_or_default().to_owned();
		let named = subtags
			.next()
			.filter(|subtag| subtag.len() == 4 && subtag.chars().all(|c| c.is_ascii_alphabetic()));
		if let Some(script) = named.or(script) {
			range = format!("{range}-{script}");
		}
		// One that is not a range is left out, so that the others still count.
		if let Ok(parsed) = range.parse::<Languages>() {
			let range = parsed.to_string();
			if !range.is_empty() && !ranges.contains(&range) {
				ranges.push(range);
			}
		}
	}
	ranges.join(", ").parse().unwrap_or_default()
}

/// Reads ranked languages from the command line.
fn parse_languages(text: &str) -> Result<Languages, String> {
	text.parse()
		.map_err(|error: ParseLanguagesError| error.to_string())
}

/// Reads a language tag from the command line.
fn parse_tag(text: &str) -> Result<Tag, String> {
	Tag::parse(text).ok_or_else(|| {
		"a language tag is written as in BCP 47, such as en, uk or uk-Latn".to_owned()
	})
}

/// The day a command shows, and the time it writes: today at `now`, at the device's offset
/// from UTC, unless the person chose another day.
fn dating<E: Environment>(environment: &E, now: u64, on: Option<Day>) -> Dating {
	match on {
		Some(day) => Dating::on(day),
		None => Dating::today(now, environment.offset_minutes())
			.or_else(|| Dating::today(now, 0))
			.expect("the time has a day in UTC"),
	}
}

/// Reads a day, such as `2000-01-01`, from the command line.
fn parse_day(text: &str) -> Result<Day, String> {
	text.parse()
		.map_err(|_| "a day is written as YYYY-MM-DD, such as 2000-01-01".to_owned())
}

/// A moment from the command line: a day, which means its end, or a date and time, in local
/// time unless it gives its offset from UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
	/// The end of a day, in local time.
	Day(jiff::civil::Date),
	/// A date and time, in local time.
	Local(jiff::civil::DateTime),
	/// A date and time with its offset from UTC.
	Exact(jiff::Timestamp),
}

impl Moment {
	/// The moment, in milliseconds since 1970, and its day, in a time zone; no milliseconds
	/// for a moment before 1970, before the library recorded anything. The end of a day is
	/// its last millisecond.
	///
	/// # Errors
	///
	/// Returns an error if the moment cannot be placed in the time zone.
	fn resolve(self, zone: &jiff::tz::TimeZone) -> Result<(Option<u64>, Day), CliError> {
		let unplaced = || CliError::Moment("it cannot be placed in local time".to_owned());
		let timestamp = match self {
			Self::Day(date) => date
				.tomorrow()
				.ok()
				.and_then(|next| next.to_zoned(zone.clone()).ok())
				.and_then(|next| {
					next.timestamp()
						.checked_sub(jiff::SignedDuration::from_millis(1))
						.ok()
				})
				.ok_or_else(unplaced)?,
			Self::Local(time) => time
				.to_zoned(zone.clone())
				.map_err(|_| unplaced())?
				.timestamp(),
			Self::Exact(timestamp) => timestamp,
		};
		let millis = u64::try_from(timestamp.as_millisecond()).ok();
		let date = match self {
			Self::Day(date) => date,
			_ => timestamp.to_zoned(zone.clone()).date(),
		};
		let day = Day::from_ymd(
			i64::from(date.year()),
			date.month().unsigned_abs(),
			date.day().unsigned_abs(),
		)
		.expect("every date of a calendar is a day");
		Ok((millis, day))
	}
}

/// Reads a moment from the command line.
fn parse_moment(text: &str) -> Result<Moment, String> {
	let wrong = || {
		"a day such as 2026-10-10, or a moment such as 2026-10-10T20:00, with its offset such as +02:00 if not in local time"
			.to_owned()
	};
	// A day is written as YYYY-MM-DD and nothing more; anything longer has a time.
	if text.len() == "YYYY-MM-DD".len() {
		return text
			.parse::<jiff::civil::Date>()
			.map(Moment::Day)
			.map_err(|_| wrong());
	}
	if let Ok(timestamp) = text.parse::<jiff::Timestamp>() {
		return Ok(Moment::Exact(timestamp));
	}
	text.parse::<jiff::civil::DateTime>()
		.map(Moment::Local)
		.map_err(|_| wrong())
}

/// Reads a date, a date and time, or an interval in EDTF from the command line.
fn parse_edtf(text: &str) -> Result<Edtf, String> {
	text.parse()
		.map_err(|error: nibkeep_time::ParseEdtfError| error.to_string())
}

/// Prints an entity as it is on a day, as recorded now or by a moment, with the names of
/// the entities it refers to.
fn show(
	library: &Library,
	space: nibkeep_base::SpaceId,
	entity: nibkeep_base::EntityId,
	when: When,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	show_added(library, space, entity, &[], when, json, output)
}

/// The day to show an entity on, and the moment, in milliseconds since 1970, by which its
/// record is shown, if not now.
#[derive(Clone, Copy, Debug)]
struct When {
	day: Day,
	as_of: Option<u64>,
	/// The day given with `--on`, if one was: what `nib list --on` lists on.
	given: Option<Day>,
}

impl When {
	/// A day, as recorded now.
	const fn on(day: Day) -> Self {
		Self {
			day,
			as_of: None,
			given: None,
		}
	}

	/// A day given with `--on`, or today, as recorded now.
	fn given(day: Day, given: Option<Day>) -> Self {
		Self {
			given,
			..Self::on(day)
		}
	}
}

/// Prints an entity as [`show`] does, with the entities recorded before it with the same
/// participants, if it was just added.
fn show_added(
	library: &Library,
	space: nibkeep_base::SpaceId,
	entity: nibkeep_base::EntityId,
	recorded_before: &[nibkeep_base::EntityId],
	when: When,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let When { day, as_of, given } = when;
	let view: Option<EntityView> = match as_of {
		Some(millis) => library.entity_as_of(space, entity, day, millis)?,
		None => library.entity(space, entity, day)?,
	};
	let view = view.ok_or_else(|| NodeError::NoEntity(entity.to_string()))?;
	let links = match as_of {
		Some(millis) => library.links_to_as_of(space, view.id, day, millis, LINKS_SHOWN)?,
		None => library.links_to_anywhere(space, view.id, day, LINKS_SHOWN)?,
	};
	// The entities it refers to go by the names they had then.
	let merged = match as_of {
		Some(millis) => library.merged_as_of(space, view.id, millis)?,
		None => library.merged(space, view.id)?,
	};
	// A link may lead to an entity of another space, or one moved there.
	let name_of = |entity| match as_of {
		Some(millis) => library.name_as_of(space, entity, millis).ok().flatten(),
		None => library
			.name_anywhere(entity)
			.ok()
			.flatten()
			.or_else(|| library.name(space, entity).ok().flatten()),
	};
	// Secret values are sealed, so which entity they are of is not known until they are
	// unlocked: the secret fields that its types list are named instead.
	let mut secret = library.secret_fields_of(space, &view.types)?;
	secret.retain(|field| !view.fields.iter().any(|shown| shown.field == *field));
	// What `nib list` takes to list the links left out; none lists them as recorded by a
	// moment.
	let more = match as_of {
		Some(_) => None,
		None => Some(given.map_or_default(|day| format!(" --on {day}"))),
	};
	// A file shows its thumbnail, as recorded now.
	let thumbnail = if as_of.is_none() && view.types.contains(&FILE) {
		library.thumbnail(space, view.id)?
	} else {
		None
	};
	let shown = output::Shown {
		view: &view,
		merged: &merged,
		secret: &secret,
		links: &links,
		more: more.as_deref(),
		recorded_before,
		thumbnail: thumbnail.as_ref(),
	};
	output::entity(&shown, &name_of, json, output)
}

/// Runs `nib init`: starts a library for the user, named as given or else after the account,
/// on this device, with the languages given or else the system's.
fn init_command<E: Environment>(
	directory: &Path,
	(name, device, languages): (Option<&str>, Option<&str>, Option<&Languages>),
	passphrase: (bool, Option<&Path>),
	environment: &mut E,
	timings: &mut Timings,
) -> Result<Library, CliError> {
	let (user, person) = user_names(name, environment)?;
	let device = device_name(device, environment);
	let languages = languages
		.cloned()
		.unwrap_or_else(|| system_languages(&environment.languages()));
	let new = NewLibrary::new(&user, &device, APP)
		.with_person(person.as_deref())
		.with_languages(languages);
	init(directory, &new, passphrase, environment, timings)
}

/// Runs `nib show`, in the personal space: an entity on a day, today unless another is
/// given, as recorded now or by a moment; a tree's revision with what it changed, as
/// `git show` shows a commit.
fn show_command<E: Environment>(
	library: &Library,
	(reference, on, as_of): (&str, Option<Day>, Option<Moment>),
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (space, entity) = library.locate_or_deleted(reference)?;
	let Some(diff) = library.revision_diff(space, entity, false)? else {
		return show_entity(
			library,
			(space, entity),
			(reference, on, as_of),
			json,
			environment,
			output,
		);
	};
	let mut shown = Vec::new();
	let what = (reference, on, as_of);
	show_entity(
		library,
		(space, entity),
		what,
		json,
		environment,
		&mut shown,
	)?;
	if json {
		let mut value: serde_json::Value = serde_json::from_slice(&shown)
			.map_err(|error| CliError::Io(io::Error::other(error)))?;
		value["diff"] = trees::diff_json(&diff);
		return output::print_json(&value, output);
	}
	output.write_all(&shown)?;
	writeln!(output)?;
	trees::diff_text(&diff, output)
}

/// Prints an entity for `nib show`, on a day, as recorded now or by a moment.
fn show_entity<E: Environment>(
	library: &Library,
	(space, entity): (SpaceId, EntityId),
	(reference, on, as_of): (&str, Option<Day>, Option<Moment>),
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Some(as_of) = as_of else {
		let day = dating(environment, environment.now(), on).day;
		return show(library, space, entity, When::given(day, on), json, output);
	};
	let zone = environment.time_zone();
	let (millis, as_of_day) = as_of.resolve(&zone)?;
	let when = When {
		day: on.unwrap_or(as_of_day),
		as_of: millis,
		given: Some(on.unwrap_or(as_of_day)),
	};
	let recorded = match millis {
		Some(millis) => library
			.entity_as_of(space, entity, when.day, millis)?
			.is_some(),
		None => false,
	};
	if !recorded {
		return Err(CliError::NotYet {
			entity: reference.to_owned(),
			when: millis.map_or_else(
				|| "1970".to_owned(),
				|millis| output::moment_text(millis, &zone),
			),
		});
	}
	show(library, space, entity, when, json, output)
}

/// Runs a command that only reads the library: `status`, `show`, `history`, `export`,
/// `find` and `list`.
fn reading_command<E: Environment>(
	library: &Library,
	cli: &Cli,
	library_folder: &Path,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	match &cli.command {
		Command::Status { .. } => {
			let mut checkouts = Vec::new();
			for space in library.spaces() {
				checkouts.extend(library.checkouts(space)?);
			}
			let checks = library
				.sync_records(UnwrapErr(SysRng))
				.ok()
				.and_then(|records| library.check_status(&records, environment.now()).ok());
			let name_of = |space, file: Option<EntityId>| match file {
				Some(file) => library.name(space, file).ok().flatten(),
				None => library.space_name(space).ok().flatten(),
			};
			let zone = environment.time_zone();
			let checks = output::Checks {
				status: checks.as_ref(),
				name_of: &name_of,
				zone: &zone,
			};
			let mut thumbnails = std::collections::BTreeMap::new();
			for space in library.spaces() {
				thumbnails.insert(space, library.thumbnail_counts(space)?);
			}
			output::status(
				(&library.status()?, &checkouts, &thumbnails),
				checks,
				library_folder,
				json,
				output,
			)
		}
		Command::Show { entity, on, as_of } => {
			show_command(library, (entity, *on, *as_of), json, environment, output)
		}
		Command::History { entity, field } => history_command(
			library,
			(entity, field.as_deref()),
			json,
			environment,
			output,
		),
		Command::Export { folder, current } => {
			export_command(library, folder, *current, json, output)
		}
		Command::Find { words, all } => {
			let words = words.join(" ");
			let first = if *all { usize::MAX } else { FIND_FIRST };
			let mut groups = Vec::new();
			for space in spaces_to_read(library) {
				// Not those moved into another space that this library holds, which show
				// there.
				let found = library.search_first(space, &words, first, |entity, _| {
					Ok(library.moved_into(space, entity)?.is_none())
				})?;
				groups.push((library.space_name(space)?, found));
			}
			let missed = not_looked_in(library)?;
			output::found_in_spaces((&groups, &missed), json, output)
		}
		Command::List {
			kind,
			conditions,
			sort,
			on,
		} => {
			let list = (kind.as_deref(), conditions.as_slice(), sort.as_deref());
			list_command(library, list, *on, json, environment, output)
		}
		_ => unreachable!("only commands that read come here"),
	}
}

/// Runs `nib export`: the personal space, in full or as it is now, into a folder.
fn export_command(
	library: &Library,
	folder: &Path,
	current: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let kind = if current {
		nibkeep_node::ExportKind::Current
	} else {
		nibkeep_node::ExportKind::Full
	};
	let exported = library.export(library.default_space(), folder, kind)?;
	output::exported(&exported, folder, kind, json, output)
}

/// Runs `nib verify --export`: checks an export against the library, and fails if it finds
/// problems.
fn verify_export_command(
	library: &Library,
	folder: &Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let check = library.verify_export(folder)?;
	output::export_check(&check, folder, json, output)?;
	match check.problems.len() {
		0 => Ok(()),
		problems => Err(CliError::UnsoundExport(problems)),
	}
}

/// Runs `nib verify`: checks the library, or with a folder, an export against it, and fails
/// if it finds problems.
fn verify_command<E: Environment>(
	library: &mut Library,
	export: Option<&Path>,
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if let Some(folder) = export {
		return verify_export_command(library, folder, json, output);
	}
	let report = library.verify(environment.now())?;
	// A damaged copy, moved aside, comes again with the next sync: the holder of the sync
	// role, if a program holds it, syncs now.
	let set_aside = report.spaces.iter().any(|space| {
		space
			.problems
			.iter()
			.any(|problem| matches!(problem, nibkeep_node::Problem::DamagedBlob(_)))
	});
	if set_aside && let Ok(records) = library.sync_records(UnwrapErr(SysRng)) {
		let _ = records.ask(None, environment.now());
	}
	output::report(&report, json, output)?;
	let problems = report.problems();
	if problems > 0 {
		return Err(CliError::Unsound(problems));
	}
	Ok(())
}

/// Runs `nib list`, in the personal space: the entities, or those of a type.
fn list_command<E: Environment>(
	library: &Library,
	list: (Option<&str>, &[String], Option<&str>),
	on: Option<Day>,
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// The space chosen, or every space: one where the type or a field is unknown is passed
	// over, unless every space is.
	let mut groups = Vec::new();
	let mut failed = None;
	for space in spaces_to_read(library) {
		match listed_in(library, space, list, on, environment) {
			Ok(listed) => groups.push((space, listed)),
			// A type or a field that this space does not know.
			Err(
				error @ (CliError::NoType(_)
				| CliError::AmbiguousType { .. }
				| CliError::NoField { .. }
				| CliError::AmbiguousField { .. }),
			) => {
				failed = failed.or(Some(error));
			}
			Err(error) => return Err(error),
		}
	}
	if groups.is_empty()
		&& let Some(error) = failed
	{
		return Err(error);
	}
	let missed = not_looked_in(library)?;
	output::entities_in_spaces((&named_groups(library, groups)?, &missed), json, output)
}

/// The names of the person's spaces that a command reading every space did not look in, as
/// this device does not hold them: none when a space is chosen.
fn not_looked_in(library: &Library) -> Result<Vec<String>, CliError> {
	if library.chosen_space().is_some() {
		return Ok(Vec::new());
	}
	let looked_in = library.spaces();
	Ok(library
		.known_spaces()?
		.into_iter()
		.filter(|known| !looked_in.contains(&known.space))
		.map(|known| known.name.unwrap_or_else(|| known.space.to_string()))
		.collect())
}

/// The spaces a command that reads looks in: the one chosen, or else every space.
fn spaces_to_read(library: &Library) -> Vec<nibkeep_base::SpaceId> {
	match library.chosen_space() {
		Some(space) => vec![space],
		None => library.spaces(),
	}
}

/// How many entities `nib find` lists of each space, unless asked for all: enough to find
/// what one looks for, while a common word, which finds thousands, takes a moment to list,
/// and fills a screen or an AI agent's context.
const FIND_FIRST: usize = 50;

/// What was found in a space, under the space's name.
type NamedGroup = (Option<String>, Vec<nibkeep_node::Listed>);

/// What was found in each space, with the space's name, but entities moved into another
/// space that this library holds, which show there.
fn named_groups(
	library: &Library,
	groups: Vec<(nibkeep_base::SpaceId, Vec<nibkeep_node::Listed>)>,
) -> Result<Vec<NamedGroup>, CliError> {
	groups
		.into_iter()
		.map(|(space, listed)| {
			let mut kept = Vec::new();
			for entity in listed {
				if library.moved_into(space, entity.id)?.is_none() {
					kept.push(entity);
				}
			}
			Ok((library.space_name(space)?, kept))
		})
		.collect()
}

/// The entities of one space that `nib list` lists.
fn listed_in<E: Environment>(
	library: &Library,
	space: nibkeep_base::SpaceId,
	(kind, conditions, sort): (Option<&str>, &[String], Option<&str>),
	on: Option<Day>,
	environment: &E,
) -> Result<Vec<nibkeep_node::Listed>, CliError> {
	let types = library.types(space)?;
	let kind = match kind {
		Some(kind) => Some(types::type_named(kind, &types)?),
		None => None,
	};
	// Fields are those of the type, or else of the other types its entities have, as an
	// audiobook is an edition too, read only when needed; without a type, any type's.
	let mut held: Option<Vec<TypeName>> = None;
	let mut field_named = |text: &str| -> Result<FieldName, CliError> {
		let Some(kind) = &kind else {
			let every: Vec<TypeName> = types
				.iter()
				.map(|definition| definition.name().clone())
				.collect();
			return types::field_named(text, &types, &every);
		};
		types::field_of_listed(text, &types, kind, || {
			if held.is_none() {
				held = Some(library.types_with(space, kind)?.into_iter().collect());
			}
			Ok(held.as_deref().unwrap_or_default())
		})
	};
	let mut conditions_given = Vec::new();
	for condition in conditions {
		let (field, text) = condition
			.split_once('=')
			.filter(|(_, text)| !text.is_empty())
			.ok_or_else(|| CliError::Assignment(condition.clone()))?;
		let field = field_named(field)?;
		if is_secret(library, space, &field) {
			return Err(CliError::SecretField(field));
		}
		let value = types::value(text, &field, &types, library, space)?;
		conditions_given.push((field, value));
	}
	let order_by = match sort {
		Some(field) => {
			let field = field_named(field)?;
			if is_secret(library, space, &field) {
				return Err(CliError::SecretField(field));
			}
			Some(field)
		}
		None => None,
	};
	let listing = nibkeep_node::Listing {
		of_type: kind,
		conditions: conditions_given,
		order_by,
		on,
	};
	let today = dating(environment, environment.now(), None).day;
	Ok(library.list(space, &listing, today)?)
}

/// Runs `nib merge`, in the personal space: merges an entity into another, or undoes the
/// merge of one merged into it, and shows the entity that stands apart or for both.
fn merge_command<E: Environment>(
	library: &mut Library,
	(reference, other, undo): (&str, Option<&str>, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (space, found) = library.locate(reference)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let now = environment.now();
	let shown = if undo {
		let member = match other {
			Some(other) => match library.find_merged(space, found, other) {
				Err(NodeError::NotMerged(_)) if library.merged(space, found)?.is_empty() => {
					return Err(CliError::NothingMerged(reference.to_owned()));
				}
				Err(NodeError::NotMerged(_)) => {
					return Err(CliError::NotMergedInto {
						entity: reference.to_owned(),
						other: other.to_owned(),
					});
				}
				found => found?,
			},
			None => match library.merged(space, found)?.as_slice() {
				[] => return Err(CliError::NothingMerged(reference.to_owned())),
				[only] => only.id,
				merged => {
					return Err(CliError::WhichMerged {
						entity: reference.to_owned(),
						candidates: output::listed_names(merged),
					});
				}
			},
		};
		library.undo_merge(space, member, now, environment.rng())?;
		member
	} else {
		// An entity merged into another is named by its own names: merged again, it moves.
		let from = match library.find_merged(space, found, reference) {
			Ok(member) => member,
			Err(NodeError::NotMerged(_)) => found,
			Err(error) => return Err(error.into()),
		};
		let other = other.unwrap_or_default();
		let into = library.find_entity(space, other)?;
		library
			.merge_entities(space, from, into, now, environment.rng())
			.map_err(|error| match error {
				NodeError::SameEntity(_) => CliError::OneAlready {
					entity: reference.to_owned(),
					into: other.to_owned(),
				},
				error => error.into(),
			})?;
		into
	};
	let day = dating(environment, now, None).day;
	show(library, space, shown, When::on(day), json, output)
}

/// Runs `nib history`, in the personal space: every value an entity has had, or one
/// field's.
fn history_command<E: Environment>(
	library: &Library,
	(reference, field): (&str, Option<&str>),
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (space, entity) = library.locate_or_deleted(reference)?;
	let mut history = library
		.history(space, entity, None)?
		.ok_or_else(|| NodeError::NoEntity(entity.to_string()))?;
	// A field is any the entity has had, of a type it has now or not.
	if let Some(field) = field {
		let had: Vec<_> = history
			.fields
			.iter()
			.map(|past| past.field.clone())
			.collect();
		let wanted = types::field_among(field, &had).map_err(|error| {
			// Secret values are left out until they are unlocked.
			let day = dating(environment, environment.now(), None).day;
			let view = library.entity(space, entity, day).ok().flatten();
			let types = library.types(space).ok();
			let secret = types
				.zip(view)
				.and_then(|(types, view)| types::field_named(field, &types, &view.types).ok())
				.filter(|field| is_secret(library, space, field));
			secret.map_or(error, CliError::SecretHistory)
		})?;
		history.fields.retain(|past| past.field == wanted);
	}
	let name_of = |entity| library.name(space, entity).ok().flatten();
	output::history(&history, &name_of, &environment.time_zone(), json, output)
}

/// Runs `nib new`, in the personal space: adds an entity of a type, with values as
/// `field=value`, in a language if given.
fn new_command<E: Environment>(
	library: &mut Library,
	(kind, values, language): (&str, &[String], Option<&Tag>),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let types = library.types(space)?;
	let mut kinds: Vec<TypeName> = Vec::new();
	for name in kind.split(',') {
		let name = types::type_named(name.trim(), &types)?;
		// NibKeep's own commands make the records of users, devices, replicas and spaces.
		if nibkeep_node::OWN_RECORDS.contains(&name) {
			return Err(NodeError::OwnType(name).into());
		}
		if !kinds.contains(&name) {
			kinds.push(name);
		}
	}
	let mut parsed = Vec::new();
	for assignment in values {
		let (field, text) = assignment
			.split_once('=')
			.ok_or_else(|| CliError::Assignment(assignment.clone()))?;
		let field = types::field_named(field, &types, &kinds)?;
		if is_secret(library, space, &field) {
			return Err(CliError::SecretField(field));
		}
		let value = types::value(text, &field, &types, library, space)?;
		parsed.push((field, value));
	}
	// A relationship is an entity, so recording one twice makes two: say so.
	let recorded_before = library.same_relationship(space, &kinds, &parsed)?;
	let now = environment.now();
	let rng = environment.rng();
	let entity = match language {
		Some(language) => {
			library.create_entity_with_language(space, &kinds, parsed, language, now, rng)?
		}
		None => library.create_entity(space, &kinds, parsed, now, rng)?,
	};
	let day = dating(environment, now, None).day;
	show_added(
		library,
		space,
		entity,
		&recorded_before,
		When::on(day),
		json,
		output,
	)
}

/// Runs `nib languages`: shows the lists of languages, after setting or clearing one.
fn languages_command<E: Environment>(
	library: &mut Library,
	command: &Command,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Command::Languages {
		ranges,
		device,
		app,
		clear,
	} = command
	else {
		unreachable!("only for nib languages");
	};
	let whose = match (device, app) {
		(true, _) => LanguagesFor::Device,
		(_, true) => LanguagesFor::Replica,
		_ => LanguagesFor::User,
	};
	let set = match ranges {
		Some(ranges) => Some(Some(ranges.parse::<Languages>()?)),
		None if *clear => Some(None),
		None => None,
	};
	if let Some(languages) = set {
		let now = environment.now();
		library.set_languages(whose, languages.as_ref(), now, environment.rng())?;
	}
	output::languages(&library.language_settings()?, json, output)
}

/// Runs `nib edit`, in the personal space.
fn edit_command<E: Environment>(
	library: &mut Library,
	arguments: &EditArgs,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let EditArgs {
		entity,
		changes,
		date,
		on,
		delete,
		restore,
		language,
		form,
		form_of,
	} = arguments;
	let (space, id) = library.locate_or_deleted(entity)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	// One reading of the clock, for the values' dates and the change alike.
	let now = environment.now();
	let mut dating = dating(environment, now, *on);
	if let Some(date) = date {
		dating = dating.with_valid_time(date.clone());
	}
	if let Some(language) = language {
		dating = dating.with_language(language.clone());
	}
	let view = library
		.entity(space, id, dating.day)?
		.ok_or_else(|| NodeError::NoEntity(entity.clone()))?;
	let forms = match (form_of, form) {
		(Some(of), _) => Forms::Of(of),
		(None, true) => Forms::OfTheOnly,
		(None, false) => Forms::No,
	};
	let mut edits = edits(changes, forms, &view, library, space)?;
	if *delete {
		edits.push(Edit::Delete);
	}
	if *restore {
		edits.push(Edit::Restore);
	}
	library.edit_entity(space, id, edits, &dating, now, environment.rng())?;
	show(
		library,
		space,
		id,
		When::given(dating.day, *on),
		json,
		output,
	)
}

/// Runs `nib purge`, in the personal space. The values of a secret field are purged all
/// at once, after the secrets are unlocked, since none is given on the command line; `nib
/// secret purge` purges one.
fn purge_command<E: Environment>(
	library: &mut Library,
	(entity, assignment, confirmed): (&str, &str, bool),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (space, id) = library.locate_or_deleted(entity)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let view = library
		.entity(space, id, dating(environment, environment.now(), None).day)?
		.ok_or_else(|| NodeError::NoEntity(entity.to_owned()))?;
	let (field, text) = assignment
		.split_once('=')
		.ok_or_else(|| CliError::Assignment(assignment.to_owned()))?;
	let types = library.types(space)?;
	let field = types::field_named(field, &types, &view.types)?;
	let secret = is_secret(library, space, &field);
	if secret && !text.is_empty() {
		return Err(CliError::SecretGiven(field));
	}
	let value = if text.is_empty() {
		None
	} else {
		Some(types::value(text, &field, &types, library, space)?)
	};
	// Values written before the field was secret are plain, and show without secrets.
	if secret && library.secrets_protection()?.is_some() {
		unlock_secrets(library, false, environment, timings)?;
	}
	let purged = purge_values(
		library,
		(id, &field, value.as_ref()),
		confirmed,
		environment,
	);
	library.lock_secrets();
	let purged = purged?;
	let name_of = |entity| library.name(space, entity).ok().flatten();
	let elsewhere = space_names(library, &purged.elsewhere);
	output::purge(
		&purged,
		(&field, &name_of),
		&elsewhere,
		confirmed,
		json,
		output,
	)?;
	if confirmed {
		Ok(())
	} else {
		Err(CliError::Unconfirmed)
	}
}

/// Runs `nib purges`: lists the purges that wait on this device, stops one, or ends its wait,
/// once confirmed; unconfirmed, it says what it would do.
fn purges_waiting_command<E: Environment>(
	library: &mut Library,
	command: Option<&PurgesCommand>,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let zone = environment.time_zone();
	// The backups plugged in here, where purges may wait too.
	let records = library.sync_records(UnwrapErr(SysRng))?;
	let remembered: BTreeMap<EntityId, PathBuf> = records
		.backup_folders()?
		.into_iter()
		.map(|(backup, found)| (backup, found.folder))
		.collect();
	let found = library.find_backups(&remembered)?;
	let Some(command) = command else {
		let mut waiting = library.waiting_purges()?;
		waiting.extend(library.waiting_purges_on(&found)?);
		let mut spaces = Vec::new();
		for purge in &waiting {
			spaces.push(space_display_name(library, purge.space)?);
		}
		let days = library.own_wait_days()?;
		return output::waiting_purges((&waiting, &spaces), days, &zone, json, output);
	};
	let (name, on, yes, stopping) = match command {
		PurgesCommand::Stop { purge, on, yes } => (purge, on, *yes, true),
		PurgesCommand::End { purge, on, yes } => (purge, on, *yes, false),
	};
	let waiting = library.find_waiting_purges(name, (&found, on.as_deref()))?;
	let space = space_display_name(library, waiting[0].space)?;
	if yes {
		let now = environment.now();
		let updated = if stopping {
			library.stop_purge(&waiting, now, environment.rng())?
		} else {
			library.end_wait(&waiting, now)?.1
		};
		// The backups updated here, as the holder records its own updates.
		let kept = library.changes_kept()?;
		for backup in updated {
			if let Some(on) = waiting
				.iter()
				.filter_map(|place| place.on.as_ref())
				.find(|on| on.backup == backup.id)
			{
				records.record_backup(on.backup, &on.folder, Some((now, kept)))?;
			}
		}
	}
	output::purge_waited((&waiting, &space), (stopping, yes), &zone, json, output)?;
	if yes {
		Ok(())
	} else {
		Err(CliError::Unconfirmed)
	}
}

/// Runs `nib move`: moves an entity to another space, with its history unless
/// `without_history`, and its secret values unless `leave_secrets`, unlocking the secrets
/// when it may hold some; a folder or a list, with what it holds, or an entity that leaves a
/// folder or a list that stays, only once `confirmed`, and otherwise says what it would take.
fn move_command<E: Environment>(
	library: &mut Library,
	((entity, to, without_history, confirmed), (keep_behind, leave_secrets)): (
		(&str, &str, bool, bool),
		(bool, bool),
	),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (from, found) = library.locate(entity)?;
	let to = library.find_space(to)?;
	let preview = library.move_preview(from, found, to)?;
	let name = |space: nibkeep_base::SpaceId| -> Result<String, CliError> {
		Ok(library
			.space_name(space)?
			.unwrap_or_else(|| space.to_string()))
	};
	let (from_name, to_name) = (name(from)?, name(to)?);
	// What moves along, or out of a folder or a list that stays, is listed first, and who
	// stops seeing it, or sees it for the first time.
	let others = !preview.losing.is_empty() || !preview.gaining.is_empty();
	if (preview.entities.len() > 1 || !preview.staying.is_empty() || others) && !confirmed {
		let name_of = |entity| library.name(from, entity).ok().flatten();
		output::move_preview(&preview, (&from_name, &to_name), &name_of, json, output)?;
		return Err(CliError::UnconfirmedMove);
	}
	let history = if without_history {
		MoveHistory::Leave
	} else {
		MoveHistory::Bring
	};
	let behind = if keep_behind {
		nibkeep_node::Behind::Keep
	} else {
		nibkeep_node::Behind::Purge
	};
	let secrets = if leave_secrets {
		MoveSecrets::Leave
	} else {
		MoveSecrets::Bring
	};
	// Secret values move with the secrets unlocked; held elsewhere, the move says so.
	if preview.secrets && !leave_secrets && library.secrets_protection()?.is_some() {
		unlock_secrets(library, false, environment, timings)?;
	}
	let now = environment.now();
	let moved = library.move_entity(
		from,
		found,
		to,
		history,
		behind,
		secrets,
		now,
		environment.rng(),
	);
	library.lock_secrets();
	let moved = moved?;
	output::moved(moved, preview.entities.len(), &to_name, json, output)?;
	output::seen_first(
		&preview.gaining,
		history == MoveHistory::Bring,
		json,
		output,
	)?;
	if behind == nibkeep_node::Behind::Purge {
		output::purged_later(&preview, &from_name, json, output)?;
	}
	Ok(())
}

/// Runs `nib purge`: of a value, or with no value, as with --behind, of what a move left
/// behind.
fn purge_either<E: Environment>(
	library: &mut Library,
	(entity, value, confirmed): (&str, Option<&str>, bool),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match value {
		Some(value) => purge_command(
			library,
			(entity, value, confirmed),
			json,
			environment,
			timings,
			output,
		),
		None => purge_behind_command(
			library,
			(entity, confirmed),
			json,
			environment,
			timings,
			output,
		),
	}
}

/// Runs `nib purge --behind`: purges what a move left behind of an entity, as its mover
/// asked, once `confirmed`, and otherwise says what it would do. Where secret values may
/// have moved, and the secrets are on this device, it unlocks them first, as only then does
/// it see which secret values moved.
fn purge_behind_command<E: Environment>(
	library: &mut Library,
	(entity, confirmed): (&str, bool),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// The entity that a move left in its space, which keeps it there with its marks, and
	// whose move asked for a purge.
	let mut asked = None;
	for space in library.spaces() {
		if let Ok(found) = library.find_entity_or_deleted(space, entity)
			&& library.behind(space, found)?.is_some()
		{
			asked = Some((space, found));
			break;
		}
	}
	let Some((space, found)) = asked else {
		return Err(NodeError::NoPurgeAsked(entity.to_owned()).into());
	};
	// What moved shows whether secret values may have, after the rest was purged too.
	let mut secret = library.may_hold_secrets(space, found)?;
	if let Some((there, now)) = library.moved_into(space, found)? {
		secret |= library.may_hold_secrets(there, now)?;
	}
	if secret && library.secrets_protection()?.is_some() {
		unlock_secrets(library, false, environment, timings)?;
	}
	let purged = purge_behind_found(
		library,
		(space, found, confirmed),
		json,
		environment,
		output,
	);
	library.lock_secrets();
	purged
}

/// Runs `nib purge --behind` on the entity found, once the secrets are unlocked where they
/// may be needed.
fn purge_behind_found<E: Environment>(
	library: &mut Library,
	(space, found, confirmed): (nibkeep_base::SpaceId, nibkeep_base::EntityId, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let remaining = library.behind(space, found)?.unwrap_or_default();
	if !confirmed && remaining > 0 {
		output::purge_behind(remaining, false, json, output)?;
		return Err(CliError::Unconfirmed);
	}
	let now = environment.now();
	let purged = library.purge_behind(space, found, now, environment.rng())?;
	output::purge_behind(purged, true, json, output)
}

/// The names of spaces, or their IDs where they have none.
fn space_names(library: &Library, spaces: &[nibkeep_base::SpaceId]) -> Vec<String> {
	spaces
		.iter()
		.map(|&space| {
			library
				.space_name(space)
				.ok()
				.flatten()
				.unwrap_or_else(|| space.to_string())
		})
		.collect()
}

/// The names of spaces, as [`space_names`] gives them, sorted.
fn sorted_space_names(library: &Library, spaces: &[nibkeep_base::SpaceId]) -> Vec<String> {
	let mut names = space_names(library, spaces);
	names.sort();
	names
}

/// Purges values of an entity of the personal space, or with `confirmed` false, says what
/// purging them would take away.
fn purge_values<E: Environment>(
	library: &mut Library,
	(entity, field, value): (
		nibkeep_base::EntityId,
		&FieldName,
		Option<&nibkeep_model::Value>,
	),
	confirmed: bool,
	environment: &mut E,
) -> Result<nibkeep_node::Purge, NodeError> {
	let space = library.default_space();
	if confirmed {
		let now = environment.now();
		library.purge(space, entity, field, value, now, environment.rng())
	} else {
		library.purgeable(space, entity, field, value)
	}
}

/// Runs `nib import` of a Takeout archive of Google Photos: imports it into the personal
/// space, from the source `from`, or Google Photos, making thumbnails as it goes, and says
/// what it did.
fn takeout_command<E: Environment>(
	library: &mut Library,
	(parts, from): (&[PathBuf], Option<&str>),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let takeout = nibkeep_import::Takeout::open(parts).map_err(import_error)?;
	let started = environment.now();
	let began = std::time::Instant::now();
	let now = move || started + u64::try_from(began.elapsed().as_millis()).unwrap_or(0);
	library.install_policies(
		space,
		environment.downloads().as_deref(),
		now(),
		environment.rng(),
	)?;
	let mut found = output::FilesAdded::default();
	let mut pictures = Vec::new();
	let mut thumbnails_rng = UnwrapErr(SysRng);
	let source = from.unwrap_or(nibkeep_import::GOOGLE_PHOTOS);
	let report = takeout
		.import(
			library,
			(space, source),
			&now,
			environment.rng(),
			&mut |library, imported| {
				if let Some(added) = &imported.added
					&& !added.known
				{
					pictures.push(added.content);
				}
				if pictures.len() >= THUMBNAIL_ROUND {
					thumbnails_of(
						library,
						(space, &mut pictures),
						&mut found,
						(now(), &mut thumbnails_rng),
					);
				}
				Ok(())
			},
		)
		.map_err(import_error)?;
	thumbnails_of(
		library,
		(space, &mut pictures),
		&mut found,
		(now(), &mut thumbnails_rng),
	);
	output::takeout_imported((&report, source), &found, json, output)?;
	match report.failed.len() {
		0 => Ok(()),
		failed => Err(CliError::FilesFailed(failed)),
	}
}

/// An importer's error, as `nib` reports it: the library's own as it is.
fn import_error(error: nibkeep_import::ImportError) -> CliError {
	match error {
		nibkeep_import::ImportError::Node(error) => error.into(),
		other => CliError::Import(other.to_string()),
	}
}

/// The path of a file within the source it is added from: from the name of the folder or
/// file given that holds it on, its names joined by `/`.
fn path_in_source(given: &[PathBuf], file: &Path) -> Option<String> {
	let root = given.iter().find(|given| file.starts_with(given))?;
	let base = root.parent().unwrap_or(Path::new(""));
	let within = file.strip_prefix(base).ok()?;
	let names: Vec<String> = within
		.components()
		.filter_map(|part| match part {
			std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
			_ => None,
		})
		.collect();
	(!names.is_empty()).then(|| names.join("/"))
}

/// Runs `nib file add`, in the personal space: adds each file found, saying what came of
/// it as it goes, and then what came of all of them. A file that cannot be read fails
/// alone; an error of the library ends the command, after what came of the files before.
fn file_add_command<E: Environment>(
	library: &mut Library,
	(paths, own, from): (&[PathBuf], &Path, Option<&str>),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let source = match from {
		Some(name) => {
			let now = environment.now();
			Some(library.source_named(space, name, now, environment.rng())?)
		}
		None => None,
	};
	let mut found = output::FilesAdded::default();
	let mut files = Vec::new();
	// The library's folder as the file system resolves it, if it can.
	let own = std::fs::canonicalize(own).unwrap_or_else(|_| own.to_owned());
	for path in paths {
		gather(path, (true, &own), &mut files, &mut found);
	}
	if !json {
		for (path, reason) in &found.failed {
			output::file_failed(path, reason, output)?;
		}
	}
	let downloads = environment.downloads();
	let now = environment.now();
	library.install_policies(space, downloads.as_deref(), now, environment.rng())?;
	// The contents added since the last round of thumbnails.
	let mut pictures = Vec::new();
	for path in files {
		if pictures.len() >= THUMBNAIL_ROUND {
			thumbnail_round(library, (space, &mut pictures), &mut found, environment);
		}
		// With --json, nib runs for a program, which is not asked.
		if !json && let Ok(Some(question)) = library.question_for(space, &path) {
			let asked = environment.ask(&output::question(&path, &question))?;
			let answer = asked.and_then(|answer| match answer.to_lowercase().as_str() {
				"y" | "yes" => Some(true),
				"n" | "no" => Some(false),
				_ => None,
			});
			if let Some(again) = answer {
				let now = environment.now();
				library.answer(space, &question, again, now, environment.rng())?;
			}
		}
		let now = environment.now();
		let from = nibkeep_node::FileFrom {
			source,
			source_path: source.and_then(|_| path_in_source(paths, &path)),
			..nibkeep_node::FileFrom::default()
		};
		match library.add_file_from(space, &path, &from, now, environment.rng()) {
			Ok(added) => {
				if !json {
					let name_of = |entity| library.name(space, entity).ok().flatten();
					output::file_added(&path, &added, &name_of, output)?;
				}
				// Content new to the library; what it knew has its thumbnail, or no thumbnailer
				// read it, and nib file thumbnails makes those that files added before lack.
				if !added.known {
					pictures.push(added.content);
				}
				found.added.push((path, added));
			}
			Err(NodeError::Unreadable { reason, .. }) => {
				if !json {
					output::file_failed(&path, &reason, output)?;
				}
				found.failed.push((path, reason));
			}
			// In a folder, a checkout or a tree's file is left out, as hidden files are.
			Err(NodeError::CheckedOut(_) | NodeError::InTree(_)) if !paths.contains(&path) => {
				found.left_out += 1;
			}
			Err(error @ (NodeError::CheckedOut(_) | NodeError::InTree(_))) => {
				let reason = if matches!(error, NodeError::InTree(_)) {
					"in a tree; nib commit records it"
				} else {
					"a checkout; nib file checkin records its edits"
				};
				if !json {
					output::file_failed(&path, reason, output)?;
				}
				found.failed.push((path, reason.to_owned()));
			}
			Err(error) => {
				thumbnail_round(library, (space, &mut pictures), &mut found, environment);
				output::files_added(&found, json, output)?;
				return Err(error.into());
			}
		}
	}
	thumbnail_round(library, (space, &mut pictures), &mut found, environment);
	output::files_added(&found, json, output)?;
	match found.failed.len() {
		0 => Ok(()),
		failed => Err(CliError::FilesFailed(failed)),
	}
}

/// Makes the thumbnails of a round of contents that `nib file add` added, and empties the
/// round. An add never fails because of its thumbnails: an error of the library while they
/// are recorded is kept to be said, and no more rounds are made.
fn thumbnail_round<E: Environment>(
	library: &mut Library,
	(space, pictures): (SpaceId, &mut Vec<nibkeep_base::Hash>),
	found: &mut output::FilesAdded,
	environment: &mut E,
) {
	let now = environment.now();
	thumbnails_of(library, (space, pictures), found, (now, environment.rng()));
}

/// Makes the thumbnails of `pictures`, unless making them failed before, and empties it.
fn thumbnails_of(
	library: &mut Library,
	(space, pictures): (SpaceId, &mut Vec<nibkeep_base::Hash>),
	found: &mut output::FilesAdded,
	(now, rng): (u64, &mut impl getrandom::rand_core::CryptoRng),
) {
	if found.thumbnails_failed.is_none() && !pictures.is_empty() {
		match library.make_thumbnails(space, pictures, now, rng) {
			Ok(made) => found.thumbnails += made,
			Err(error) => found.thumbnails_failed = Some(error.to_string()),
		}
	}
	pictures.clear();
}

/// Runs `nib file`, in the personal space of the library in `folder`.
fn file_command<E: Environment>(
	library: &mut Library,
	(command, folder): (&FileCommand, &Path),
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	match command {
		FileCommand::Add {
			paths,
			secret: true,
			to,
			..
		} => {
			let added =
				secret_file_add_command(library, (paths, to), json, environment, timings, output);
			library.lock_secrets();
			added
		}
		FileCommand::Add { paths, from, .. } => {
			let own = folder.join(LIBRARY_DATA);
			let added = (paths.as_slice(), &*own, from.as_deref());
			file_add_command(library, added, json, environment, output)
		}
		FileCommand::Checkout { file, path } => {
			let checkout = (file.as_str(), path.as_deref());
			file_checkout_command(library, checkout, cli, environment, output)
		}
		FileCommand::Pin { file, device } | FileCommand::Unpin { file, device } => {
			let pinned = matches!(command, FileCommand::Pin { .. });
			let (space, file) = library.locate(file)?;
			let (device, device_name) = if let Some(device) = device {
				one_device(library, device)?
			} else {
				let this = library.replicas()?.into_iter().find(|replica| replica.this);
				(
					this.as_ref()
						.and_then(|replica| replica.device)
						.ok_or_else(|| NodeError::WhichDevice("this device".to_owned()))?,
					this.and_then(|replica| replica.device_name),
				)
			};
			let now = environment.now();
			library.pin_file((space, file), device, pinned, now, environment.rng())?;
			let name = library.name(space, file)?;
			output::pinned(
				(file, name.as_deref()),
				(device, device_name.as_deref()),
				pinned,
				json,
				output,
			)
		}
		FileCommand::Checkin { copy, keep, force } => {
			let checkin = (copy.as_str(), *keep, *force);
			file_checkin_command(library, checkin, json, environment, output)
		}
		FileCommand::Where { file } => {
			let (space, file) = library.locate(file)?;
			let copies = library.copies_of(space, file, environment.now())?;
			let name = library.name(space, file)?;
			let zone = environment.time_zone();
			output::file_copies((&copies, name.as_deref()), &zone, json, output)
		}
		FileCommand::Thumbnail { file, path } => {
			let (space, file) = library.locate(file)?;
			let path = absolute(path, environment)?;
			let thumbnail = library
				.thumbnail(space, file)?
				.ok_or_else(|| CliError::NoThumbnail(file.to_string()))?;
			match library.save_thumbnail(space, &thumbnail, &path) {
				Err(NodeError::PlaceTaken(path)) => return Err(CliError::Exists(path)),
				saved => saved?,
			}
			output::thumbnail_written(&thumbnail, &path, json, output)
		}
		FileCommand::Thumbnails => {
			let space = library.default_space();
			let now = environment.now();
			// A long run says how far it got after each round.
			let mut progress = |made: nibkeep_node::ThumbnailsMade, of: usize| {
				let done = made.made + made.unreadable + made.not_here;
				if of > THUMBNAIL_ROUND && done < of {
					let _ = output::progress(&format!("{done} of {of} pictures…"), json, output);
				}
			};
			let made = library.make_missing_thumbnails(
				space,
				THUMBNAIL_ROUND,
				now,
				environment.rng(),
				&mut progress,
			)?;
			let left = library.missing_thumbnails(space)?.len();
			output::thumbnails_made(made, left, json, output)
		}
	}
}

/// How many pictures go to the thumbnailer at a time as files are added: a round's
/// thumbnails are recorded once its pictures are added, so that an import cut short keeps
/// the thumbnails of what it added.
const THUMBNAIL_ROUND: usize = 64;

/// What makes thumbnails in nib: decoders in memory-safe Rust for JPEG, PNG, GIF, WebP, BMP
/// and TIFF ([technical design](../../../docs/Technical-design.md#98-thumbnails)).
struct RustDecoders;

impl nibkeep_node::Thumbnailer for RustDecoders {
	fn made_with(&self) -> &str {
		nibkeep_thumbnails::MADE_WITH
	}

	fn reads(&self, media_type: &str) -> bool {
		nibkeep_thumbnails::reads(media_type)
	}

	fn make(&self, picture: &[u8]) -> Option<nibkeep_node::Thumbnail> {
		let made = nibkeep_thumbnails::make(picture).ok()?;
		Some(nibkeep_node::Thumbnail {
			jpeg: made.jpeg,
			width: made.width,
			height: made.height,
			picture_hash: Some(made.picture_hash),
		})
	}
}

/// Runs `nib file checkout`, in the personal space: into the folder or at the path given,
/// relative to the current folder, or else into the current folder.
fn file_checkout_command<E: Environment>(
	library: &mut Library,
	(file, place): (&str, Option<&Path>),
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	let (space, file) = library.locate(file)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let place = match place {
		Some(place) => absolute(place, environment)?,
		None => current_dir(environment)?,
	};
	let now = environment.now();
	let checked_out = match library.checkout(space, file, &place, now, environment.rng()) {
		Err(NodeError::SecretFile(name)) => return Err(CliError::SecretFileCheckout(name)),
		// Not on this device: fetched from one that holds it, on the home network.
		Err(NodeError::NoCopyHere(name)) => {
			let (holders, reached) = fetch((space, file), library, cli, environment, output)?;
			let now = environment.now();
			match library.checkout(space, file, &place, now, environment.rng()) {
				Err(NodeError::NoCopyHere(_)) if !reached.is_empty() => {
					return Err(CliError::NotGiven {
						name,
						holders: reached,
					});
				}
				Err(NodeError::NoCopyHere(_)) if !holders.is_empty() => {
					return Err(CliError::NotFetched { name, holders });
				}
				checked_out => checked_out?,
			}
		}
		checked_out => checked_out?,
	};
	let name = library.name(space, checked_out.file)?;
	output::checked_out(&checked_out, name.as_deref(), json, output)
}

/// How long `nib file checkout` looks on the network for a device that holds a file that
/// this device lacks.
const FETCH_WAIT: Duration = Duration::from_secs(10);

/// Fetches the content of a file that this device lacks, which `nib file checkout` recorded
/// as opened, so that this device keeps it: syncs with the devices whose accounts hold it,
/// through the program that syncs this device, or itself. Returns the names of those
/// devices, none if no device holds it, and of those that a sync reached.
fn fetch<E: Environment>(
	(space, file): (nibkeep_base::SpaceId, EntityId),
	library: &mut Library,
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(Vec<String>, Vec<String>), CliError> {
	let holders = library.holders_of(space, file)?;
	let name_of = |replica: &nibkeep_node::Replica| {
		let app = replica.name.clone().unwrap_or_else(|| "nib".to_owned());
		match &replica.device_name {
			Some(device) => format!("{app} on {device}"),
			None => app,
		}
	};
	let names: Vec<String> = holders.iter().map(name_of).collect();
	if holders.is_empty() {
		return Ok((names, Vec::new()));
	}
	output::progress(
		&format!("Fetching it from {}…", names.join(", ")),
		cli.json,
		output,
	)?;
	let waited = Instant::now();
	let _role = loop {
		if let Some(role) = library.take_role()? {
			break role;
		}
		let records = library.sync_records(UnwrapErr(SysRng))?;
		if let Some(holder) = records.holder()? {
			let asked = environment.now();
			let asks = holders
				.iter()
				.map(|replica| records.ask(Some(replica.id), asked))
				.collect::<Result<Vec<_>, _>>()?;
			let mut released = false;
			while !released
				&& asks
					.iter()
					.any(|ask| matches!(records.answered(*ask), Ok(None)))
			{
				if library.take_role()?.is_some() {
					released = true;
				} else if waited.elapsed() > FETCH_WAIT + ASK_WAIT {
					return Err(CliError::NotAnswered(holder.program));
				} else {
					std::thread::sleep(Duration::from_millis(100));
				}
			}
			if !released {
				library.catch_up()?;
				let synced = records.synced_since(asked)?;
				let reached = holders
					.iter()
					.filter(|replica| synced.contains_key(&replica.id))
					.map(name_of)
					.collect();
				return Ok((names, reached));
			}
			continue;
		}
		if waited.elapsed() > FETCH_WAIT + ASK_WAIT {
			return Err(CliError::NotAnswered("another program".to_owned()));
		}
		std::thread::sleep(Duration::from_millis(100));
	};
	let network = library.network(cli.network(false))?;
	let endpoints = holders
		.iter()
		.filter_map(|replica| replica.endpoint)
		.collect();
	let found = network.find(&endpoints, FETCH_WAIT);
	let now = clock(environment);
	// A sync that fails leaves the file not here, which the checkout then says.
	let synced =
		nibkeep_node::sync_with_found(library, &network, holders, &found, &now, environment.rng());
	let reached = synced
		.iter()
		.filter(|with| with.synced.is_ok())
		.map(|with| name_of(&with.replica))
		.collect();
	Ok((names, reached))
}

/// The one device of the person's that `device` names, by its name or ID, or by one copy of
/// the library on it, with its name.
fn one_device(library: &Library, device: &str) -> Result<(EntityId, Option<String>), CliError> {
	let replicas = library.replicas_named(device)?;
	let devices: std::collections::BTreeSet<EntityId> = replicas
		.iter()
		.filter_map(|replica| replica.device)
		.collect();
	let mut found = devices.into_iter();
	let (Some(chosen), None) = (found.next(), found.next()) else {
		return Err(NodeError::WhichDevice(format!(
			"{device} names no one device of yours; nib device list shows them"
		))
		.into());
	};
	let name = replicas
		.iter()
		.find(|replica| replica.device == Some(chosen))
		.and_then(|replica| replica.device_name.clone());
	Ok((chosen, name))
}

/// The records of the replicas of the person's other devices that `devices` name, by name
/// or ID, as `--on` and nib space allow name them: those not revoked, of no AI agent's.
fn devices_named(library: &Library, devices: &[String]) -> Result<Vec<EntityId>, CliError> {
	let mut records = Vec::new();
	for device in devices {
		let named: Vec<EntityId> = library
			.replicas_named(device)?
			.into_iter()
			.filter(|replica| !replica.revoked && !replica.ai_agent && !replica.this)
			.map(|replica| replica.id)
			.collect();
		if named.is_empty() {
			return Err(NodeError::WhichDevice(format!(
				"{device} names no other device of yours; nib device list shows them"
			))
			.into());
		}
		records.extend(named);
	}
	Ok(records)
}

/// How many bytes of files an everyday device keeps, as `--keeps` gives it: `None` for the
/// default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keeps(Option<u64>);

/// How many days an archive node waits before purging, as `--waits` gives it: `None` for the
/// default, `Some(0)` for none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waits(Option<u64>);

/// A wait of `--waits`: days from 1 to 365, such as `30` or `30d`, `none`, or `default`.
fn parse_waits(text: &str) -> Result<Waits, String> {
	let text = text.trim().to_ascii_lowercase();
	match text.as_str() {
		"default" => return Ok(Waits(None)),
		"none" | "0" => return Ok(Waits(Some(0))),
		_ => {}
	}
	let digits = text
		.strip_suffix("days")
		.or_else(|| text.strip_suffix("day"))
		.or_else(|| text.strip_suffix('d'))
		.unwrap_or(&text)
		.trim();
	match digits.parse::<u64>() {
		Ok(days @ 1..=nibkeep_node::MOST_WAIT_DAYS) => Ok(Waits(Some(days))),
		_ => Err("days from 1 to 365, such as 30, or none, or default".to_owned()),
	}
}

/// A size of `--keeps`: a size, such as `20GB`, or `default`.
fn parse_keeps(text: &str) -> Result<Keeps, String> {
	if text.trim().eq_ignore_ascii_case("default") {
		return Ok(Keeps(None));
	}
	parse_size(text).map(|size| Keeps(Some(size)))
}

/// Runs `nib file add --secret`, in the personal space: unlocks the secrets, or sets them up
/// for the library's first secret, and adds each file as a secret file, attached to the
/// entities `to`. The secrets are locked again by the caller.
fn secret_file_add_command<E: Environment>(
	library: &mut Library,
	(paths, to): (&[PathBuf], &[String]),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let mut attached_to = Vec::with_capacity(to.len());
	for entity in to {
		attached_to.push(library.find_entity(space, entity)?);
	}
	// Each a file, before the passphrase is asked for.
	let mut files = Vec::with_capacity(paths.len());
	for path in paths {
		let path = absolute(path, environment)?;
		if path.is_dir() {
			return Err(CliError::SecretFolder(path));
		}
		files.push(path);
	}
	unlock_secrets(library, true, environment, timings)?;
	let mut added = Vec::with_capacity(files.len());
	let mut failed = Vec::new();
	for path in files {
		// A file that cannot be read is reported, and the others are added all the same.
		let file = match std::fs::File::open(&path) {
			Ok(file) => file,
			Err(error) => {
				if !json {
					output::file_failed(&path, &error.to_string(), output)?;
				}
				failed.push((path, error.to_string()));
				continue;
			}
		};
		let name = path
			.file_name()
			.map_or_default(|name| name.to_string_lossy().into_owned());
		let now = environment.now();
		let secret_file =
			match library.add_secret_file(space, &name, file, &attached_to, now, environment.rng())
			{
				Ok(secret_file) => secret_file,
				Err(NodeError::Unreadable { reason, .. }) => {
					if !json {
						output::file_failed(&path, &reason, output)?;
					}
					failed.push((path, reason));
					continue;
				}
				Err(error) => return Err(error.into()),
			};
		if !json {
			let name_of = |entity| library.name(space, entity).ok().flatten();
			output::secret_file_added(&path, &secret_file, (&attached_to, &name_of), output)?;
		}
		added.push((path, name, secret_file));
	}
	output::secret_files_added((&added, &failed), &attached_to, json, output)?;
	match failed.len() {
		0 => Ok(()),
		failed => Err(CliError::FilesFailed(failed)),
	}
}

/// Runs `nib file checkin`, in the personal space: records the edits of the copy, and,
/// unless `keep`, ends the checkout.
fn file_checkin_command<E: Environment>(
	library: &mut Library,
	(copy, keep, force): (&str, bool, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// The checkout's space: the one chosen, or else whichever has it.
	if library.chosen_space().is_none() {
		for space in library.spaces() {
			library.choose_space(Some(space))?;
			if find_checkout(library, copy, environment).is_ok() {
				break;
			}
			library.choose_space(None)?;
		}
	}
	let space = library.default_space();
	let checkout = find_checkout(library, copy, environment)?;
	let now = environment.now();
	let recorded = match library.record_edits(space, checkout.id, force, now, environment.rng()) {
		Err(NodeError::Stale(path)) => return Err(CliError::Stale(path)),
		recorded => recorded?,
	};
	let ended = if keep {
		None
	} else {
		let now = environment.now();
		Some(library.end_checkout(space, checkout.id, now, environment.rng())?)
	};
	// A new version of a picture gets its own thumbnail; a check-in never fails because of
	// it, and says why it has none.
	let mut thumbnail_failed = None;
	if let Some(version) = recorded.version {
		let now = environment.now();
		if let Err(error) = library.make_thumbnails(space, &[version], now, environment.rng()) {
			thumbnail_failed = Some(error.to_string());
		}
	}
	let name = library.name(space, checkout.file)?;
	let checked_in = output::CheckedIn {
		checkout: &checkout,
		recorded,
		ended: ended.as_ref(),
		name: name.as_deref(),
		thumbnail_failed: thumbnail_failed.as_deref(),
	};
	output::checked_in(&checked_in, json, output)
}

/// The checkout on this device of the personal space that `copy` names: the path of its
/// copy, relative to the current folder, or else the file, if it is checked out in one
/// place.
fn find_checkout<E: Environment>(
	library: &Library,
	copy: &str,
	environment: &E,
) -> Result<Checkout, CliError> {
	let space = library.default_space();
	let path = absolute(Path::new(copy), environment)?;
	if let Some(checkout) = library.checkout_at(space, &path)? {
		return Ok(checkout);
	}
	let file = match library.find_entity(space, copy) {
		Ok(file) => file,
		Err(NodeError::NoEntity(_)) => return Err(NodeError::NotCheckedOut(copy.to_owned()).into()),
		Err(error) => return Err(error.into()),
	};
	let mut checkouts = library.checkouts_of(space, file)?;
	match checkouts.len() {
		0 => Err(NodeError::NotCheckedOut(copy.to_owned()).into()),
		1 => Ok(checkouts.remove(0)),
		_ => Err(NodeError::WhichCheckout {
			file: copy.to_owned(),
			paths: checkouts
				.into_iter()
				.map(|checkout| checkout.path)
				.collect(),
		}
		.into()),
	}
}

/// Runs `nib backup edit`: changes a backup, and says how it is now.
fn backup_edit<E: Environment>(
	library: &mut Library,
	command: &BackupCommand,
	environment: &mut E,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let BackupCommand::Edit {
		backup,
		name,
		spaces,
		policies,
		every_policy,
		away,
		home,
		waits,
	} = command
	else {
		unreachable!("backup_command passes only an edit");
	};

	let known = library.find_backup(backup)?;
	let spaces = (!spaces.is_empty())
		.then(|| {
			spaces
				.iter()
				.map(|space| library.find_space(space))
				.collect::<Result<Vec<_>, _>>()
		})
		.transpose()?;
	let of = spaces.clone().unwrap_or_else(|| known.spaces.clone());
	let policies = if *every_policy {
		Some(Vec::new())
	} else if policies.is_empty() {
		None
	} else {
		Some(backup_policies(library, &of, policies, environment)?)
	};
	let edit = BackupEdit {
		name: name.clone(),
		spaces,
		policies,
		away: (*away || *home).then_some(*away),
		waits: waits.map(|Waits(days)| days),
	};
	let now = environment.now();
	library.edit_backup(known.id, &edit, now, environment.rng())?;
	let edited = library.find_backup(&known.id.to_string())?;
	output::backup_edited(&edited, json, output)
}

/// Runs `nib backup` and `nib recovery`, which keep a library safe from the loss of its
/// devices.
fn safekeeping_command<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match &cli.command {
		Command::Backup { command } => backup_command(library, command, cli, environment, output),
		Command::Recovery { command } => {
			recovery_command(library, command, cli.json, environment, timings, output)
		}
		_ => unreachable!("only nib backup and nib recovery come here"),
	}
}

/// How many times a person may type a recovery code before a command gives up.
const CODE_TRIES: usize = 3;

/// Reads a recovery code that the person types, as a passphrase is read, and says which
/// group to look at when it does not check.
fn typed_recovery_code<E: Environment>(
	prompt: &str,
	environment: &mut E,
	json: bool,
	output: &mut dyn Write,
) -> Result<RecoveryCode, CliError> {
	for _ in 0..CODE_TRIES {
		let typed = environment.passphrase(prompt)?;
		match RecoveryCode::from_text(&typed) {
			Ok(code) => return Ok(code),
			Err(error) if !json => writeln!(output, "{error}.")?,
			Err(_) => {}
		}
	}
	Err(CliError::NotARecoveryCode)
}

/// Runs `nib recovery new`: makes a code and shows it, which counts once typed back.
fn recovery_new_command<E: Environment>(
	library: &mut Library,
	(secrets, revoke_older): (bool, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let code = RecoveryCode::generate(environment.rng());
	output::recovery_shown(&code, json, output)?;
	// With --json, a program asked: the person confirms the code another time.
	let mut stands = None;
	for _ in 0..if json { 0 } else { CODE_TRIES } {
		// Nobody to answer, as when the output goes to a file: the code is shown, and
		// counts once it is confirmed.
		let prompt = "Type the code back, or press Enter to do it later: ";
		let Ok(typed) = environment.passphrase(prompt) else {
			break;
		};
		if typed.trim().is_empty() {
			break;
		}
		match RecoveryCode::from_text(&typed) {
			Ok(typed) if typed.same_as(&code) => {
				let now = environment.now();
				stands = Some(library.add_recovery_code(&code, secrets, now, environment.rng())?);
				break;
			}
			Ok(_) => writeln!(output, "That is another code than the one shown.")?,
			Err(error) => writeln!(output, "{error}.")?,
		}
	}
	// The codes that it replaces: revoked once every backup carries it, or at once.
	let replaced = if stands.is_some() {
		let now = environment.now();
		library.revoke_replaced_recovery_codes(revoke_older, now, environment.rng())?
	} else {
		Vec::new()
	};
	let codes = library.recovery_codes()?;
	output::recovery_made(&code, stands.as_ref(), (&replaced, &codes), json, output)
}

/// Runs `nib recovery`.
fn recovery_command<E: Environment>(
	library: &mut Library,
	command: &RecoveryCommand,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	const TYPE_IT: &str = "The recovery code: ";
	// The secrets are unlocked first, so that a wrong passphrase stops before a code is shown.
	if let RecoveryCommand::New { secrets: true, .. }
	| RecoveryCommand::Confirm { secrets: true, .. }
	| RecoveryCommand::Check { secrets: true } = command
	{
		unlock_secrets(library, false, environment, timings)?;
	}
	match command {
		RecoveryCommand::New {
			secrets,
			revoke_older,
		} => {
			let new = (*secrets, *revoke_older);
			recovery_new_command(library, new, json, environment, output)
		}
		RecoveryCommand::Confirm {
			secrets,
			revoke_older,
		} => {
			let code = typed_recovery_code(TYPE_IT, environment, json, output)?;
			let standing = library
				.check_recovery_code(&code)?
				.code
				.filter(|named| !named.revoked);
			let already = standing.is_some();
			let named = match standing {
				Some(named) if !*secrets || named.holds_secrets => named,
				_ => {
					let now = environment.now();
					library.add_recovery_code(&code, *secrets, now, environment.rng())?
				}
			};
			// At once only for the newest code, which replaces the others: an older one typed
			// back with --revoke-older stays, as revoking it is not what was asked.
			let at_once = *revoke_older && named.replaced_by.is_none();
			let now = environment.now();
			let replaced =
				library.revoke_replaced_recovery_codes(at_once, now, environment.rng())?;
			let codes = library.recovery_codes()?;
			output::recovery_confirmed(&named, already, (&replaced, &codes), json, output)
		}
		RecoveryCommand::List => {
			let zone = environment.time_zone();
			output::recovery_list(&library.recovery_codes()?, &zone, json, output)
		}
		RecoveryCommand::Check { .. } => {
			let code = typed_recovery_code(TYPE_IT, environment, json, output)?;
			let check = library.check_recovery_code(&code)?;
			output::recovery_checked(&check, json, output)?;
			if check.code.is_some() {
				Ok(())
			} else {
				Err(CliError::UnknownRecoveryCode)
			}
		}
		RecoveryCommand::Revoke { code, yes } => {
			let mut named = library.find_recovery_code(code)?;
			if *yes {
				let now = environment.now();
				named = library.revoke_recovery_code(code, now, environment.rng())?;
			}
			let zone = environment.time_zone();
			output::recovery_revoked(&named, &zone, json, output)?;
			if *yes {
				Ok(())
			} else {
				Err(CliError::UnconfirmedRevoke)
			}
		}
		RecoveryCommand::Restore { .. } | RecoveryCommand::FromCopy { .. } => {
			unreachable!("run_command makes the library")
		}
	}
}

/// Runs `nib backup`.
fn backup_command<E: Environment>(
	library: &mut Library,
	command: &BackupCommand,
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	let records = library.sync_records(UnwrapErr(SysRng))?;
	let remembered: BTreeMap<EntityId, PathBuf> = records
		.backup_folders()?
		.into_iter()
		.map(|(backup, found)| (backup, found.folder))
		.collect();
	match command {
		BackupCommand::New {
			folder,
			name,
			spaces,
			policies,
			away,
			waits,
		} => {
			let spaces = spaces
				.iter()
				.map(|space| library.find_space(space))
				.collect::<Result<Vec<_>, _>>()?;
			let policies = backup_policies(library, &spaces, policies, environment)?;
			let name = name.clone().unwrap_or_else(|| {
				folder.file_name().map_or_else(
					|| "Backup".to_owned(),
					|name| name.to_string_lossy().into_owned(),
				)
			});
			let new = NewBackup {
				name,
				spaces,
				policies,
				away: *away,
				waits: waits.and_then(|Waits(days)| days),
			};
			backup_add(library, (folder, &new), &records, environment, json, output)
		}
		BackupCommand::List => backup_list(library, &remembered, environment, json, output),
		BackupCommand::Update { backup, at } => {
			let chosen = backups_to_update(library, backup.as_deref(), at.as_deref(), &remembered)?;
			let mut failed = None;
			for (backup, folder) in chosen {
				let folder = std::path::absolute(&folder).unwrap_or(folder);
				let now = || environment.now();
				let updated = library.update_backup(&backup, &folder, &now, &|| false);
				if let Ok(Some(_)) = &updated {
					let kept = library.changes_kept()?;
					records.record_backup(backup.id, &folder, Some((now(), kept)))?;
				}
				let shown = output::backup_updated(&backup, &folder, &updated, json, output);
				if let Err(error) = updated {
					failed.get_or_insert(error);
				}
				shown?;
			}
			failed.map_or(Ok(()), |error| Err(error.into()))
		}
		BackupCommand::Edit { .. } => backup_edit(library, command, environment, json, output),
		BackupCommand::Remove { backup } => {
			let known = library.find_backup(backup)?;
			let now = environment.now();
			library.remove_backup(known.id, now, environment.rng())?;
			let folder = remembered.get(&known.id);
			output::backup_removed(&known, folder.map(PathBuf::as_path), json, output)
		}
	}
}

/// Makes a backup, updates it, and remembers where it is.
fn backup_add<E: Environment>(
	library: &mut Library,
	(folder, new): (&Path, &NewBackup),
	records: &nibkeep_node::Records,
	environment: &mut E,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let now = environment.now();
	let added = library.add_backup(folder, new, now, environment.rng())?;
	let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_owned());
	let now = || environment.now();
	let updated = library
		.update_backup(&added.backup, &folder, &now, &|| false)?
		.unwrap_or_default();
	records.record_backup(
		added.backup.id,
		&folder,
		Some((now(), library.changes_kept()?)),
	)?;
	output::backup_added(&added, &folder, &updated, json, output)
}

/// Lists the person's backups, with what the folder of each that this computer finds says.
fn backup_list<E: Environment>(
	library: &Library,
	remembered: &BTreeMap<EntityId, PathBuf>,
	environment: &E,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let found: BTreeMap<EntityId, PathBuf> = library
		.find_backups(remembered)?
		.into_iter()
		.map(|(backup, folder)| (backup.id, folder))
		.collect();
	let now = environment.now();
	let mut listed = Vec::new();
	for backup in library.backups()? {
		let state = found
			.get(&backup.id)
			.and_then(|folder| library.backup_state(&backup, folder, now).ok());
		let spaces = backup
			.spaces
			.iter()
			.map(|space| {
				library
					.space_name(*space)
					.ok()
					.flatten()
					.unwrap_or_else(|| space.to_string())
			})
			.collect();
		let policies = backup
			.policies
			.iter()
			.map(|policy| {
				backup
					.spaces
					.iter()
					.find_map(|space| library.name(*space, *policy).ok().flatten())
					.unwrap_or_else(|| policy.to_string())
			})
			.collect();
		let folder = found.get(&backup.id).cloned();
		listed.push(output::ListedBackup {
			backup,
			folder,
			state,
			spaces,
			policies,
		});
	}
	output::backups(&listed, json, output)
}

/// The backups that `nib backup update` updates, with their folders: the one named, where
/// `at` says or where this computer finds it; the one whose folder `at` is; or each that
/// this computer finds.
fn backups_to_update(
	library: &Library,
	backup: Option<&str>,
	at: Option<&Path>,
	remembered: &BTreeMap<EntityId, PathBuf>,
) -> Result<Vec<(Backup, PathBuf)>, NodeError> {
	Ok(match (backup, at) {
		(Some(reference), Some(folder)) => {
			vec![(library.find_backup(reference)?, folder.to_owned())]
		}
		(None, Some(folder)) => {
			let named = library
				.backups()?
				.into_iter()
				.find(|known| nibkeep_node::backup_named_at(folder) == Some(known.id))
				.ok_or_else(|| {
					NodeError::WhichBackup(format!("{} is no backup of yours", folder.display()))
				})?;
			vec![(named, folder.to_owned())]
		}
		(Some(reference), None) => {
			let backup = library.find_backup(reference)?;
			let found = library
				.find_backups(remembered)?
				.into_iter()
				.find(|(known, _)| known.id == backup.id)
				.ok_or_else(|| {
					NodeError::WhichBackup(format!(
						"this computer does not find the backup {}; plug it in, or say where it is with --at",
						backup.name
					))
				})?;
			vec![found]
		}
		(None, None) => library.find_backups(remembered)?,
	})
}

/// The policies that `names` name in each of `spaces`, whose default policies are installed
/// first: each must name one in at least one of them.
fn backup_policies<E: Environment>(
	library: &mut Library,
	spaces: &[SpaceId],
	names: &[String],
	environment: &mut E,
) -> Result<Vec<EntityId>, CliError> {
	// No policy named installs none.
	if names.is_empty() {
		return Ok(Vec::new());
	}
	let spaces = if spaces.is_empty() {
		library.spaces()
	} else {
		spaces.to_vec()
	};
	let downloads = environment.downloads();
	for space in &spaces {
		let now = environment.now();
		library.install_policies(*space, downloads.as_deref(), now, environment.rng())?;
	}
	let mut found = Vec::new();
	for name in names {
		let policies: Vec<EntityId> = spaces
			.iter()
			.filter_map(|space| library.find_policy(*space, name).ok())
			.collect();
		if policies.is_empty() {
			return Err(NodeError::NoEntity(format!(
				"no policy of the backup's spaces is named {name}"
			))
			.into());
		}
		found.extend(policies);
	}
	found.sort();
	found.dedup();
	Ok(found)
}

/// Runs `nib policy`, in the personal space, which gets the default policies first if it
/// never had them, and this device a rule for its downloads folder.
fn policy_command<E: Environment>(
	library: &mut Library,
	command: &PolicyCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let downloads = environment.downloads();
	let now = environment.now();
	library.install_policies(space, downloads.as_deref(), now, environment.rng())?;
	match command {
		PolicyCommand::List => {
			let policies = library.policies(space)?;
			let rules = library.rules(space)?;
			let name_of = |entity| library.name(space, entity).ok().flatten();
			output::policies(&policies, &rules, &name_of, json, output)
		}
		PolicyCommand::Show { policy } => {
			let id = library.find_policy(space, policy)?;
			let policies = library.policies(space)?;
			let rules = library.rules(space)?;
			let name_of = |entity| library.name(space, entity).ok().flatten();
			output::policy(id, &policies, &rules, &name_of, json, output)
		}
		PolicyCommand::New {
			name,
			copies,
			away,
			archived,
			in_place,
		} => {
			let needs = Needs {
				copies: *copies,
				away: *away,
				archived: *archived,
				kept: if *in_place {
					Kept::InPlace
				} else {
					Kept::Store
				},
			};
			let id = library.add_policy(space, name, needs, now, environment.rng())?;
			policy_changed("Added", id, library, json, output)
		}
		PolicyCommand::Edit {
			policy,
			name,
			copies,
			away,
			archived,
			in_place,
			store,
		} => {
			let id = library.find_policy(space, policy)?;
			let kept = match (*in_place, *store) {
				(true, _) => Some(Kept::InPlace),
				(_, true) => Some(Kept::Store),
				_ => None,
			};
			let edit = PolicyEdit {
				name: name.clone(),
				copies: *copies,
				away: *away,
				archived: *archived,
				kept,
			};
			library.edit_policy(space, id, &edit, now, environment.rng())?;
			policy_changed("Changed", id, library, json, output)
		}
		PolicyCommand::Remove { policy } => {
			let id = library.find_policy(space, policy)?;
			let name = library
				.policies(space)?
				.into_iter()
				.find(|known| known.id == id)
				.and_then(|known| known.name);
			library.remove_policy(space, id, now, environment.rng())?;
			output::removed_policy(id, name.as_deref(), json, output)
		}
		PolicyCommand::Rule { command } => {
			rule_command(library, command, json, environment, output)
		}
		PolicyCommand::Set {
			entity,
			policy,
			clear: _,
		} => {
			let id = library.find_entity(space, entity)?;
			let policy = policy
				.as_deref()
				.map(|policy| library.find_policy(space, policy))
				.transpose()?;
			library.set_policy(space, id, policy, now, environment.rng())?;
			why(library, id, json, output)
		}
		PolicyCommand::Why { entity } => {
			let id = library.find_entity(space, entity)?;
			why(library, id, json, output)
		}
		PolicyCommand::Check => {
			let check = library.check_policies(space, environment.now())?;
			let name_of = |entity| library.name(space, entity).ok().flatten();
			let zone = environment.time_zone();
			output::policy_check(&check, (&name_of, &zone), json, output)
		}
	}
}

/// Runs `nib policy rule`, in the personal space.
fn rule_command<E: Environment>(
	library: &mut Library,
	command: &RuleCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let now = environment.now();
	match command {
		RuleCommand::New {
			policy,
			of_type,
			has,
			folder,
			media_type,
			larger_than,
			asks_above,
			place,
		} => {
			let types = library.types(space)?;
			let rule = NewRule {
				of_type: of_type
					.as_deref()
					.map(|name| types::type_named(name, &types))
					.transpose()?,
				has: has
					.as_deref()
					.map(|field| types::field_named(field, &types, &[FILE, PHOTO, VIDEO]))
					.transpose()?,
				folder: folder.clone(),
				media_type: media_type.clone(),
				larger_than: *larger_than,
				asks_above: *asks_above,
				..NewRule::new(library.find_policy(space, policy)?)
			};
			let place = place_of(library, place)?.unwrap_or(Place::BeforeTheRest);
			let id = library.add_rule(space, &rule, place, now, environment.rng())?;
			rule_changed("Added", id, library, json, output)
		}
		RuleCommand::Remove { rule } => {
			let id = rule_named(library, rule)?;
			library.remove_rule(space, id, now, environment.rng())?;
			output::removed_rule(id, json, output)
		}
		RuleCommand::Move { rule, place } => {
			let id = rule_named(library, rule)?;
			let place = place_of(library, place)?.ok_or(CliError::WhereTo)?;
			library.move_rule(space, id, place, now, environment.rng())?;
			rule_changed("Moved", id, library, json, output)
		}
	}
}

/// Prints a rule that a command added or moved, as `nib policy list` shows it.
fn rule_changed(
	what: &str,
	id: EntityId,
	library: &Library,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let rules = library.rules(space)?;
	let policies = library.policies(space)?;
	let name_of = |entity| library.name(space, entity).ok().flatten();
	output::rule_changed(what, id, &rules, &policies, &name_of, json, output)
}

/// Prints a policy that a command added or changed.
fn policy_changed(
	what: &str,
	id: EntityId,
	library: &Library,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let policies = library.policies(library.default_space())?;
	match policies.iter().find(|policy| policy.id == id) {
		Some(policy) => output::policy_changed(what, policy, json, output),
		None => Ok(()),
	}
}

/// The rule of the personal space that `text` names: its number in `nib policy list`, its
/// ID, or a prefix of its ID of at least six characters.
fn rule_named(library: &Library, text: &str) -> Result<EntityId, CliError> {
	let rules = library.rules(library.default_space())?;
	// A number in the list, or else a prefix of an ID, which may be all digits too.
	if let Some(rule) = text
		.parse::<usize>()
		.ok()
		.and_then(|number| number.checked_sub(1))
		.and_then(|index| rules.get(index))
	{
		return Ok(rule.id);
	}
	let lowered = text.to_lowercase();
	let found: Vec<EntityId> = rules
		.iter()
		.map(|rule| rule.id)
		.filter(|id| {
			let id = id.to_string();
			id == lowered || (lowered.len() >= 6 && id.starts_with(&lowered))
		})
		.collect();
	match found.as_slice() {
		[one] => Ok(*one),
		_ => Err(CliError::NoRule(text.to_owned())),
	}
}

/// Where a rule goes, as the person said, if they did.
fn place_of(library: &Library, place: &PlaceArgs) -> Result<Option<Place>, CliError> {
	Ok(if place.first {
		Some(Place::First)
	} else if place.last {
		Some(Place::Last)
	} else if let Some(rule) = &place.before {
		Some(Place::Before(rule_named(library, rule)?))
	} else if let Some(rule) = &place.after {
		Some(Place::After(rule_named(library, rule)?))
	} else {
		None
	})
}

/// Prints the policy chosen for an entity of the personal space, and what chose it.
fn why(
	library: &Library,
	entity: EntityId,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let choice = library.policy_of(space, entity)?;
	let policies = library.policies(space)?;
	let rules = library.rules(space)?;
	let name_of = |entity| library.name(space, entity).ok().flatten();
	output::why(&choice, &policies, &rules, &name_of, json, output)
}

/// A size, such as `500MB`, `2 GiB` or `1073741824`: a whole number of bytes, or of a
/// decimal or binary unit.
fn parse_size(text: &str) -> Result<u64, String> {
	let text = text.trim();
	let digits = text
		.find(|c: char| !c.is_ascii_digit())
		.unwrap_or(text.len());
	let (number, unit) = text.split_at(digits);
	let number: u64 = number
		.parse()
		.map_err(|_| format!("{text} is not a size, such as 500MB or 2GiB"))?;
	let unit = unit.trim().to_lowercase();
	let size: u64 = match unit.as_str() {
		"" | "b" => 1,
		"kb" => 1_000,
		"mb" => 1_000_000,
		"gb" => 1_000_000_000,
		"tb" => 1_000_000_000_000,
		"kib" => 1 << 10,
		"mib" => 1 << 20,
		"gib" => 1 << 30,
		"tib" => 1 << 40,
		_ => {
			return Err(format!(
				"{unit} is not a unit of size: B, kB, MB, GB, TB, KiB, MiB, GiB or TiB"
			));
		}
	};
	number
		.checked_mul(size)
		.ok_or_else(|| format!("{text} is too large"))
}

/// Gathers the files to add at `path`: the file, or the files in the folder and the
/// folders in it, in the order of their names. A path given is followed if it is a link,
/// and fails if it is neither a file nor a folder, or in the library's own folder, `own`;
/// in folders, links, hidden files and folders, and `own`, wherever a link puts it, are
/// left out, and counted.
fn gather(
	path: &Path,
	(given, own): (bool, &Path),
	files: &mut Vec<PathBuf>,
	found: &mut output::FilesAdded,
) {
	let mut fail = |reason: String| found.failed.push((path.to_owned(), reason));
	let metadata = if given {
		std::fs::metadata(path)
	} else {
		std::fs::symlink_metadata(path)
	};
	let metadata = match metadata {
		Ok(metadata) => metadata,
		Err(error) => return fail(error.to_string()),
	};
	let is_own = || std::fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with(own));
	if given && (metadata.is_file() || metadata.is_dir()) && is_own() {
		return fail("the library's own folder holds it".to_owned());
	}
	if metadata.is_file() {
		files.push(path.to_owned());
	} else if metadata.is_dir() {
		if !given && is_own() {
			found.left_out += 1;
			return;
		}
		let entries = match std::fs::read_dir(path) {
			Ok(entries) => entries,
			Err(error) => return fail(error.to_string()),
		};
		let mut inside = Vec::new();
		for entry in entries {
			match entry {
				Ok(entry) => inside.push(entry.path()),
				// The rest of the folder may be read all the same.
				Err(error) => fail(error.to_string()),
			}
		}
		inside.sort();
		for entry in inside {
			let hidden = entry
				.file_name()
				.is_some_and(|name| name.to_string_lossy().starts_with('.'));
			if hidden {
				found.left_out += 1;
			} else {
				gather(&entry, (false, own), files, found);
			}
		}
	} else if given {
		fail("neither a file nor a folder".to_owned());
	} else {
		// A link in a folder, or what is neither a file nor a folder.
		found.left_out += 1;
	}
}

/// Runs `nib secret`, in the personal space, and forgets the secrets however it ends.
fn secret_command<E: Environment>(
	library: &mut Library,
	command: &SecretCommand,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (SecretCommand::Set { entity, field, .. }
	| SecretCommand::Show { entity, field }
	| SecretCommand::History { entity, field }
	| SecretCommand::Clear { entity, field }
	| SecretCommand::Purge { entity, field, .. }) = command
	else {
		return entityless_secret_command(library, command, json, environment, timings, output);
	};
	let (space, id) = library.locate_or_deleted(entity)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let day = dating(environment, environment.now(), None).day;
	let view = library
		.entity(space, id, day)?
		.ok_or_else(|| NodeError::NoEntity(entity.clone()))?;
	let types = library.types(space)?;
	let field = types::field_named(field, &types, &view.types)?;
	if !is_secret(library, space, &field) {
		return Err(CliError::NotSecret(field));
	}
	// Before the passphrase is asked for, and the first secrets set up.
	let generated = generated_secret(command, &field, environment)?;
	let set = matches!(command, SecretCommand::Set { .. });
	unlock_secrets(library, set, environment, timings)?;
	let no_value = || CliError::NoSecretValue {
		entity: entity.clone(),
		field: field.clone(),
	};
	let done = match command {
		SecretCommand::Set { .. } => set_secret(
			library,
			(id, &field, &types),
			generated,
			environment,
			timings,
		),
		SecretCommand::Clear { .. } => edit_now(
			library,
			id,
			Edit::Set(field.clone(), Vec::new()),
			environment,
		),
		SecretCommand::Purge { yes, .. } => (|| {
			let asked = Instant::now();
			let text = environment.passphrase(&format!("The value of {field} to purge: "))?;
			add(&mut timings.waiting, asked.elapsed());
			if text.is_empty() {
				return Err(CliError::EmptyValue);
			}
			let value = types::value(&text, &field, &types, library, space)?;
			let purged = purge_values(library, (id, &field, Some(&value)), *yes, environment)?;
			let name_of = |entity| library.name(space, entity).ok().flatten();
			let elsewhere = space_names(library, &purged.elsewhere);
			output::purge(&purged, (&field, &name_of), &elsewhere, *yes, json, output)
		})(),
		SecretCommand::Show { .. } => (|| {
			let view = library
				.entity(space, id, day)?
				.ok_or_else(|| NodeError::NoEntity(entity.clone()))?;
			let values = view.values(&field);
			if values.is_empty() {
				return Err(no_value());
			}
			let name_of = |entity| library.name(space, entity).ok().flatten();
			output::secret_values(view.id, &field, values, &name_of, json, output)
		})(),
		SecretCommand::History { .. } => {
			let history = (entity.as_str(), Some(field.as_str()));
			history_command(library, history, json, environment, output).map_err(
				|error| match error {
					CliError::SecretHistory(_) => no_value(),
					other => other,
				},
			)
		}
		SecretCommand::Copy { .. }
		| SecretCommand::Generate { .. }
		| SecretCommand::Code { .. }
		| SecretCommand::Ask { .. }
		| SecretCommand::Give { .. }
		| SecretCommand::FromCopy
		| SecretCommand::Save { .. } => {
			unreachable!("answered above")
		}
	};
	library.lock_secrets();
	done?;
	match command {
		SecretCommand::Set { .. } | SecretCommand::Clear { .. } => {
			let day = dating(environment, environment.now(), None).day;
			show(library, space, id, When::on(day), json, output)
		}
		SecretCommand::Purge { yes: false, .. } => Err(CliError::Unconfirmed),
		_ => Ok(()),
	}
}

/// Runs the `nib secret` commands that name no entity's field: `code`, which finds its field
/// itself, `ask` and `give`.
fn entityless_secret_command<E: Environment>(
	library: &mut Library,
	command: &SecretCommand,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		SecretCommand::Code { entity, field } => {
			let code = (entity.as_str(), field.as_deref());
			code_command(library, code, json, environment, timings, output)
		}
		SecretCommand::Ask { from } => match shared_space(library) {
			Some(space) => {
				let asking = (space, from.as_str());
				ask_space_command(library, asking, json, environment, timings, output)
			}
			None => ask_command(library, from, json, environment, timings, output),
		},
		SecretCommand::Give { device } => match shared_space(library) {
			Some(space) => {
				let giving = (space, device.as_str());
				give_space_command(library, giving, json, environment, timings, output)
			}
			None => give_command(library, device, json, environment, timings, output),
		},
		SecretCommand::FromCopy => {
			from_copy_secrets_command(library, json, environment, timings, output)
		}
		SecretCommand::Save { file, path } => {
			let save = (file.as_str(), path.as_deref());
			let saved = secret_save_command(library, save, json, environment, timings, output);
			library.lock_secrets();
			saved
		}
		// One needs no library, and the other closes it before it waits; the rest name a
		// field.
		_ => unreachable!("run_command runs nib secret generate and copy"),
	}
}

/// Runs `nib secret save`, in the personal space: unlocks the secrets and saves a plain copy
/// of a secret file into the folder or at the path given, relative to the current folder, or
/// else into the current folder. The secrets are locked again by the caller.
fn secret_save_command<E: Environment>(
	library: &mut Library,
	(file, place): (&str, Option<&Path>),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (space, file) = library.locate(file)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let place = match place {
		Some(place) => absolute(place, environment)?,
		None => current_dir(environment)?,
	};
	unlock_secrets(library, false, environment, timings)?;
	let saved = library.save_secret_file(space, file, &place)?;
	let name = library.name(space, file)?;
	output::secret_saved((file, name.as_deref()), &saved, json, output)
}

/// The space that `--space` chose, if it is shared with another member: in a space of the
/// user's own, the library's own secrets are asked for and given.
fn shared_space(library: &Library) -> Option<SpaceId> {
	library.chosen_space().filter(|&space| {
		space != library.personal_space()
			&& library
				.members(space)
				.is_ok_and(|members| members.iter().any(|member| !member.this_user))
	})
}

/// The device of another member of a shared space that `name` names, by its device's name
/// or the start of its record's ID, of eight characters or more, among those that `wanted`
/// takes, with the member's name.
fn member_device(
	library: &Library,
	space: SpaceId,
	name: &str,
	wanted: impl Fn(&nibkeep_node::MemberReplica) -> bool,
) -> Result<(String, nibkeep_node::MemberReplica), CliError> {
	let mut found = Vec::new();
	for member in library.members(space)? {
		for replica in member.replicas {
			let id = replica.record.to_string();
			let named = replica.device.as_deref() == Some(name)
				|| id == name
				|| (name.len() >= 8 && id.starts_with(name));
			if named && !replica.this && !replica.revoked && wanted(&replica) {
				found.push((member.name.clone().unwrap_or_default(), replica));
			}
		}
	}
	match found.len() {
		1 => Ok(found.remove(0)),
		0 => Err(NodeError::WhichDevice(format!(
			"no other member's device in the space that is {name} can do that; nib space \
			 members shows them"
		))
		.into()),
		_ => Err(NodeError::WhichDevice(format!(
			"{name} names several devices in the space; give the ID of the one you mean, as \
			 nib space show --json shows it"
		))
		.into()),
	}
}

/// Runs `nib secret ask` in a shared space: asks another member's device that holds the
/// space's secrets for them, once this device's secrets are unlocked.
fn ask_space_command<E: Environment>(
	library: &mut Library,
	(space, from): (SpaceId, &str),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (member, device) = member_device(library, space, from, |replica| replica.holds_secrets)?;
	if library.secrets_protection()?.is_none() {
		return Err(NodeError::SetUpSecretsFirst.into());
	}
	unlock_secrets(library, false, environment, timings)?;
	let now = environment.now();
	let asked = library.ask_for_space_secrets(space, device.record, now, environment.rng());
	library.lock_secrets();
	asked?;
	let name = library.space_name(space)?.unwrap_or_default();
	output::asked_for_space_secrets(&name, (&member, &device), json, output)
}

/// Runs `nib secret give` in a shared space: gives the space's secrets to another member's
/// device that asked this one, once the secrets here are unlocked.
fn give_space_command<E: Environment>(
	library: &mut Library,
	(space, to): (SpaceId, &str),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (member, device) = member_device(library, space, to, |replica| replica.asks_for_secrets)?;
	unlock_secrets(library, false, environment, timings)?;
	let now = environment.now();
	let given = library.give_space_secrets(space, device.record, now, environment.rng());
	library.lock_secrets();
	let keys = given?;
	let name = library.space_name(space)?.unwrap_or_default();
	output::gave_space_secrets(&name, (&member, &device), keys, json, output)
}

/// Runs `nib secret ask`: sets up the secrets of this device, which holds none while another
/// does, with a passphrase of their own, and asks for them from the device named; or, on a
/// device that asked already, asks another
/// ([technical design](../../../docs/Technical-design.md#12-secrets)).
fn ask_command<E: Environment>(
	library: &mut Library,
	from: &str,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// The device that holds them: of its replicas, one that does.
	let Some(from) = library
		.replicas_named(from)?
		.into_iter()
		.find(|replica| replica.holds_secrets && !replica.this && !replica.revoked)
	else {
		return Err(NodeError::WhichDevice(
			"that device does not hold the secrets, or has not said so yet: unlock its secrets \
			 once"
				.to_owned(),
		)
		.into());
	};
	let now = environment.now();
	if library.secrets_state()? == SecretsState::Asked {
		library.ask_secrets_from(from.id, now, environment.rng())?;
	} else {
		// Before the passphrase is asked for.
		library.check_may_ask(from.id)?;
		let passphrase = ask_twice(
			environment,
			timings,
			(
				"A passphrase for the secrets on this device, not the library's: ",
				"The passphrase again: ",
			),
			(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
		)?;
		let protection = NewProtection::Passphrase(&passphrase, environment.passphrase_params());
		let now = environment.now();
		let asked = library.ask_for_secrets(from.id, &protection, now, environment.rng());
		library.lock_secrets();
		asked?;
	}
	let own = library.replicas()?.into_iter().find(|replica| replica.this);
	output::asked_for_secrets(own.as_ref(), &from, json, output)
}

/// Runs `nib secret give`: gives the secrets to the replicas of a device, or one replica,
/// that asked for them, after the passphrase of the secrets here.
fn give_command<E: Environment>(
	library: &mut Library,
	device: &str,
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// Before the passphrase is asked for: which replicas, and that one of them asks this one.
	let named: Vec<Replica> = library
		.replicas_named(device)?
		.into_iter()
		.filter(|replica| !replica.this && !replica.revoked)
		.collect();
	if named.is_empty() {
		return Err(NodeError::WhichDevice(
			"that names no other device of yours that is not revoked".to_owned(),
		)
		.into());
	}
	let own = library.replicas()?.into_iter().find(|replica| replica.this);
	let own = own.map(|replica| replica.id);
	let asking: Vec<Replica> = named
		.iter()
		.filter(|replica| replica.asks_for_secrets && replica.secrets_from == own)
		.cloned()
		.collect();
	if asking.is_empty() {
		return Err(if named.iter().any(|replica| replica.asks_for_secrets) {
			NodeError::WhichDevice("that device asks another of yours for the secrets".to_owned())
		} else {
			NodeError::NotAsked
		}
		.into());
	}
	unlock_secrets(library, false, environment, timings)?;
	let mut given = Vec::new();
	let mut failed = None;
	for replica in asking {
		let now = environment.now();
		match library.give_secrets(replica.id, now, environment.rng()) {
			Ok(keys) => given.push((replica, keys)),
			Err(error) => {
				failed = Some(error);
				break;
			}
		}
	}
	library.lock_secrets();
	// What was given is said, also when the rest failed.
	output::gave_secrets(&given, json, output)?;
	match failed {
		Some(error) => Err(error.into()),
		None => Ok(()),
	}
}

/// The password that `nib secret set --generate` sets, generated before the passphrase is
/// asked for, so that a recipe that cannot be met changes nothing; `None` without
/// `--generate`, whose options are refused alone.
fn generated_secret<E: Environment>(
	command: &SecretCommand,
	field: &FieldName,
	environment: &mut E,
) -> Result<Option<zeroize::Zeroizing<String>>, CliError> {
	let SecretCommand::Set {
		generate, recipe, ..
	} = command
	else {
		return Ok(None);
	};
	if !generate {
		return if recipe.given() {
			Err(CliError::RecipeWithoutGenerate)
		} else {
			Ok(None)
		};
	}
	if *field == nibkeep_std::ONE_TIME_CODE {
		return Err(CliError::GeneratedOneTimeCode(field.clone()));
	}
	Ok(Some(nibkeep_passwords::generate(
		&recipe.recipe(),
		environment.rng(),
	)?))
}

/// Sets a secret field of an entity of the personal space to the value `generated`, or to
/// one asked for twice, while the secrets are unlocked.
fn set_secret<E: Environment>(
	library: &mut Library,
	(id, field, types): (EntityId, &FieldName, &nibkeep_model::Types),
	generated: Option<zeroize::Zeroizing<String>>,
	environment: &mut E,
	timings: &mut Timings,
) -> Result<(), CliError> {
	let text = if let Some(generated) = generated {
		generated
	} else {
		ask_twice(
			environment,
			timings,
			(&format!("{field}: "), &format!("{field} again: ")),
			(|| CliError::EmptyValue, || CliError::ValuesDiffer),
		)?
	};
	// The secret of one-time codes is checked before it is kept, as it is typed.
	if *field == nibkeep_std::ONE_TIME_CODE {
		nibkeep_passwords::OneTimeCode::check(&text)?;
	}
	let space = library.default_space();
	let value = types::value(&text, field, types, library, space)?;
	edit_now(
		library,
		id,
		Edit::Set(field.clone(), vec![value]),
		environment,
	)
}

/// Shows the one-time code of an entity now, from the secret in a field: std.one-time-code,
/// unless another is given.
fn code_command<E: Environment>(
	library: &mut Library,
	(entity, field): (&str, Option<&str>),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let default = nibkeep_std::ONE_TIME_CODE;
	let field = field.unwrap_or(default.as_str());
	let read = read_secret(library, entity, field, environment, timings)?;
	let generator = nibkeep_passwords::OneTimeCode::parse(&read.values[0])?;
	output::one_time_code(&generator, environment.now() / 1000, json, output)
}

/// The values of a secret field of an entity, read after the secrets' passphrase.
struct SecretRead {
	/// The entity.
	entity: EntityId,
	/// Its name, or how it was given.
	name: String,
	/// The secret field.
	field: FieldName,
	/// Its values, in the order they are shown: one at least.
	values: Vec<zeroize::Zeroizing<String>>,
}

/// Reads the values of a secret field of an entity of the personal space, asking for the
/// secrets' passphrase, and locks the secrets again.
fn read_secret<E: Environment>(
	library: &mut Library,
	entity: &str,
	field: &str,
	environment: &mut E,
	timings: &mut Timings,
) -> Result<SecretRead, CliError> {
	let (space, id) = library.locate_or_deleted(entity)?;
	// The command works in the space that holds the entity.
	library.choose_space(Some(space))?;
	let day = dating(environment, environment.now(), None).day;
	let view = library
		.entity(space, id, day)?
		.ok_or_else(|| NodeError::NoEntity(entity.to_owned()))?;
	let types = library.types(space)?;
	let field = types::field_named(field, &types, &view.types)?;
	if !is_secret(library, space, &field) {
		return Err(CliError::NotSecret(field));
	}
	unlock_secrets(library, false, environment, timings)?;
	let read = library.entity(space, id, day);
	library.lock_secrets();
	let view = read?.ok_or_else(|| NodeError::NoEntity(entity.to_owned()))?;
	let name_of = |entity| library.name(space, entity).ok().flatten();
	let values: Vec<zeroize::Zeroizing<String>> = view
		.values(&field)
		.iter()
		.map(|shown| zeroize::Zeroizing::new(output::value_text(&shown.value, &name_of)))
		.collect();
	if values.is_empty() {
		return Err(CliError::NoSecretValue {
			entity: entity.to_owned(),
			field,
		});
	}
	Ok(SecretRead {
		entity: view.id,
		name: view.name.unwrap_or_else(|| entity.to_owned()),
		field,
		values,
	})
}

/// Copies the first value of a secret to `board`, marked as sensitive, waits `clear_after`
/// seconds, or until the person interrupts, and then clears the clipboard if it still holds
/// the secret. Whatever fails once the secret is copied, the clipboard is cleared.
fn copy_and_clear<E: Environment>(
	board: Box<dyn nibkeep_passwords::Board>,
	read: &SecretRead,
	clear_after: u64,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	environment.interruptible();
	let mut clipboard = nibkeep_passwords::Clipboard::new(board, environment.rng());
	clipboard.copy_secret(&read.values[0])?;
	let printed = if json {
		Ok(())
	} else {
		output::copied(
			&read.field,
			&read.name,
			read.values.len(),
			clear_after,
			output,
		)
		.and_then(|()| Ok(output.flush()?))
	};
	let interrupted = printed.is_ok() && environment.wait(Duration::from_secs(clear_after));
	// Another program may hold the clipboard for a moment.
	let cleared = clipboard.clear_if_ours().or_else(|_| {
		std::thread::sleep(Duration::from_millis(200));
		clipboard.clear_if_ours()
	});
	printed?;
	output::copy_ended(
		(read.entity, &read.field, read.values.len()),
		(cleared?, interrupted),
		json,
		output,
	)
}

/// Prints a new random password, as `recipe` says.
fn generate_command<E: Environment>(
	recipe: &RecipeArgs,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let recipe = recipe.recipe();
	let password = nibkeep_passwords::generate(&recipe, environment.rng())?;
	output::generated(&password, recipe.bits(), json, output)
}

/// Runs `nib import`: imports an export that nib export made, or else Google Takeout's
/// archive of Google Photos.
fn import_command<E: Environment>(
	library: &mut Library,
	(paths, from): (&[PathBuf], Option<&str>),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// An export says what it is in its manifest and its sums, which Takeout's archives never
	// hold: a folder with either is read as an export, whose errors then say what it lacks.
	if let [folder] = paths
		&& ["manifest.json", "SHA256SUMS"]
			.iter()
			.any(|name| folder.join(name).is_file())
	{
		if from.is_some() {
			return Err(CliError::Import(
				"--from names the source of Google Takeout's photos; an export keeps where its files came from".to_owned(),
			));
		}
		let space = library.default_space();
		let imported = library.import(space, folder, environment.now(), environment.rng())?;
		return output::imported(&imported, folder, json, output);
	}
	takeout_command(library, (paths, from), json, environment, output)
}

/// Protects the library's keys, or with `secrets`, its secrets, with a new passphrase if
/// `passphrase` says so, and otherwise with the platform's keystore.
fn protect_command<E: Environment>(
	library: &mut Library,
	(passphrase, secrets): (bool, bool),
	json: bool,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// Before any passphrase is asked for.
	if !passphrase && !environment.keystore() {
		return Err(CliError::NoKeystoreToProtect);
	}
	if secrets {
		unlock_secrets(library, false, environment, timings)?;
	}
	let protected = (|| {
		let typed = if passphrase {
			let prompt = if secrets {
				"A new passphrase for the secrets, not the library's: "
			} else {
				"A new passphrase for the library: "
			};
			Some(ask_twice(
				environment,
				timings,
				(prompt, "The passphrase again: "),
				(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
			)?)
		} else {
			None
		};
		let params = environment.passphrase_params();
		let protection = match &typed {
			Some(passphrase) => NewProtection::Passphrase(passphrase, params),
			None => NewProtection::Keystore {
				service: KEYSTORE_SERVICE,
			},
		};
		let protected = if secrets {
			library.protect_secrets(&protection, environment.rng())
		} else {
			library.protect(&protection, environment.rng())
		};
		protected.map_err(|error| match error {
			error @ NodeError::Keys(KeysError::Keystore(_)) => CliError::KeystoreToProtect(error),
			other => CliError::Node(other),
		})
	})();
	library.lock_secrets();
	protected?;
	output::protected(secrets, passphrase, json, output)
}

/// Edits an entity of the personal space today, at the time it is edited, which may come
/// long after the command began, as it waited for the person to type.
fn edit_now<E: Environment>(
	library: &mut Library,
	entity: nibkeep_base::EntityId,
	edit: Edit,
	environment: &mut E,
) -> Result<(), CliError> {
	let space = library.default_space();
	let now = environment.now();
	let dating = dating(environment, now, None);
	library.edit_entity(space, entity, vec![edit], &dating, now, environment.rng())?;
	Ok(())
}

/// Whether a field is secret in a space.
fn is_secret(library: &Library, space: nibkeep_base::SpaceId, field: &FieldName) -> bool {
	library
		.secret_fields(space)
		.is_some_and(|secret| secret.contains(field))
}

/// Unlocks the library's secrets, asking for their passphrase; or, for a library that has
/// none yet, if `set_up` says so, sets them up with a passphrase of their own, asked for
/// twice. Then takes the secrets keys that another device gave this one, and states what
/// the records do not say yet ([technical
/// design](../../../docs/Technical-design.md#12-secrets)).
pub(crate) fn unlock_secrets<E: Environment>(
	library: &mut Library,
	set_up: bool,
	environment: &mut E,
	timings: &mut Timings,
) -> Result<(), CliError> {
	match library.secrets_protection()? {
		None if !set_up => return Err(CliError::NoSecrets),
		None => {
			let passphrase = ask_twice(
				environment,
				timings,
				(
					"The library's first secret. A passphrase for its secrets, not the library's: ",
					"The passphrase again: ",
				),
				(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
			)?;
			let protection =
				NewProtection::Passphrase(&passphrase, environment.passphrase_params());
			let now = environment.now();
			return Ok(library.set_up_secrets(&protection, now, environment.rng())?);
		}
		Some(Protection::Passphrase { .. }) => {
			let asked = Instant::now();
			let passphrase = environment.passphrase("Passphrase for secrets: ")?;
			add(&mut timings.waiting, asked.elapsed());
			library.unlock_secrets(&Unlocker::Passphrase(&passphrase))?;
		}
		Some(Protection::Keystore { .. }) => {
			if !environment.keystore() {
				return Err(CliError::NoKeystore);
			}
			library.unlock_secrets(&Unlocker::Keystore)?;
		}
	}
	let now = environment.now();
	let settled = library.settle_secrets(now, environment.rng());
	// Unlocked, and then failed, or without keys yet: the secrets are forgotten, as when it
	// ends.
	let personal = library.personal_space();
	if settled.is_err() || !library.secrets_unlocked(personal) {
		library.lock_secrets();
	}
	settled?;
	if library.secrets_unlocked(personal) {
		Ok(())
	} else {
		Err(NodeError::SecretsAsked.into())
	}
}

/// The errors of something asked for twice: that it is empty, and that the two differ.
type Mistyped = (fn() -> CliError, fn() -> CliError);

/// Asks for a passphrase once, which may be empty: the person waited meanwhile.
fn ask_once<E: Environment>(
	environment: &mut E,
	timings: &mut Timings,
	prompt: &str,
) -> Result<zeroize::Zeroizing<String>, CliError> {
	let asked = Instant::now();
	let typed = environment.passphrase(prompt);
	add(&mut timings.waiting, asked.elapsed());
	Ok(typed?)
}

/// Asks for something new twice, without showing it, as a passphrase or a secret value,
/// and counts the time spent typing as waiting. Fails as `empty` says if it is empty, and
/// as `differ` says if the two differ.
fn ask_twice<E: Environment>(
	environment: &mut E,
	timings: &mut Timings,
	(prompt, again): (&str, &str),
	(empty, differ): Mistyped,
) -> Result<zeroize::Zeroizing<String>, CliError> {
	let asked = Instant::now();
	let first = environment.passphrase(prompt);
	let second = match &first {
		Ok(first) if !first.is_empty() => Some(environment.passphrase(again)),
		_ => None,
	};
	add(&mut timings.waiting, asked.elapsed());
	let first = first?;
	let Some(second) = second else {
		return Err(empty());
	};
	if *first != *second? {
		return Err(differ());
	}
	Ok(first)
}

/// Runs `nib type`, in the personal space.
fn type_command<E: Environment>(
	library: &mut Library,
	command: &TypeCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.default_space();
	let (name, types) = match command {
		TypeCommand::Add { name, fields } => {
			let name: TypeName = types::name(name)?;
			let fields = fields
				.iter()
				.map(|field| types::field(field))
				.collect::<Result<Vec<_>, _>>()?;
			// Standard names come from the vocabulary alone.
			let known = library.types(space)?;
			let type_known = types::standard_type_known(&known, &name);
			types::check_standard(name.as_str(), "type", type_known)?;
			for (field, definition) in &fields {
				let field_known = types::standard_field_known(&known, field, definition);
				types::check_standard(field.as_str(), "field", field_known)?;
				if let Some(reverse) = definition.reverse() {
					let reverse_known = types::standard_reverse_known(&known, reverse);
					types::check_standard(reverse.as_str(), "field", reverse_known)?;
				}
			}
			let definition = library.types(space)?.extend(name.clone(), fields);
			let now = environment.now();
			let types = library.define_type(space, &definition, now, environment.rng())?;
			(name, types)
		}
		TypeCommand::Show { name } => (types::name(name)?, library.types(space)?),
		TypeCommand::List => {
			let types = library.types(space)?;
			return output::type_list(&types, library.languages(), json, output);
		}
	};
	let definition = types.get(&name).ok_or(CliError::NoType(name))?;
	// Whether a field is secret is the space's to say, whatever the type's definition says.
	let secret = library.secret_fields(space).cloned().unwrap_or_default();
	output::type_definition(definition, (library.languages(), &secret), json, output)
}

/// Runs `nib space`: makes, lists or renames spaces, invites to them, joins them, and lists
/// their members and gives them roles.
fn space_command<E: Environment>(
	library: &mut Library,
	command: &SpaceCommand,
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	match command {
		SpaceCommand::New { .. }
		| SpaceCommand::Take { .. }
		| SpaceCommand::Drop { .. }
		| SpaceCommand::Allow { .. } => taking_command(library, command, json, environment, output),
		SpaceCommand::AddGroup { .. }
		| SpaceCommand::RemoveGroup { .. }
		| SpaceCommand::Agree { .. }
		| SpaceCommand::Withdraw { .. } => group_command(library, command, json, environment, output),
		SpaceCommand::List => {
			let status = library.status()?;
			let known = library.known_spaces()?;
			let requests = library.space_requests()?;
			output::spaces(
				(&status.spaces, &known, &requests),
				library.personal_space(),
				json,
				output,
			)
		}
		SpaceCommand::Show { space } => {
			show_space_command(library, space, json, environment, output)
		}
		SpaceCommand::Rename { space, name } => {
			let space = library.find_space(space)?;
			let now = environment.now();
			library.rename_space(space, name, now, environment.rng())?;
			output::space_renamed(space, name, json, output)
		}
		SpaceCommand::Invite {
			space,
			role,
			person,
			yes,
			..
		} => {
			let space = library.find_space(space)?;
			let person = person
				.as_deref()
				.map(|person| library.find_person(space, person))
				.transpose()?;
			let invitation = nibkeep_node::Invitation {
				space,
				role: role.role(),
				person,
			};
			invite_command(library, (&invitation, *yes), cli, environment, output)
		}
		SpaceCommand::Join { code, yes, on } => {
			let also = devices_named(library, on)?;
			join_space_command(library, (code, *yes, &also), cli, environment, output)
		}
		SpaceCommand::AiAgents { space, switch } => {
			let space = library.find_space(space)?;
			if let Some(switch) = switch {
				let now = environment.now();
				library.allow_ai_agents(space, *switch == Switch::On, now, environment.rng())?;
			}
			let allowed = library.ai_agents_allowed(space)?;
			output::ai_agents_allowed(allowed, json, output)
		}
		SpaceCommand::Link {
			space,
			member,
			person,
		} => link_command(
			library,
			(space, member, person.as_deref()),
			json,
			environment,
			output,
		),
		SpaceCommand::Accept { space, member, yes } => {
			accept_in_space_command(library, (space, member, *yes), json, environment, output)
		}
		SpaceCommand::Leave { space, yes } => {
			let space = library.find_space(space)?;
			leave_command(library, (space, *yes), json, environment, output)
		}
		SpaceCommand::Remove { space, member, yes } => {
			remove_command(library, (space, member, *yes), json, environment, output)
		}
		SpaceCommand::LetIn { .. }
		| SpaceCommand::MakeDirect { .. }
		| SpaceCommand::Confirm { .. } => direct_command(library, command, json, environment, output),
		SpaceCommand::Claim { .. } | SpaceCommand::Reject { .. } => {
			claim_command(library, command, json, environment, output)
		}
		SpaceCommand::Role {
			space,
			member,
			role,
		} => role_command(library, (space, member, *role), json, environment, output),
	}
}

/// Runs `nib space new`, `nib space take`, `nib space drop` and `nib space allow`: makes a
/// space for the devices chosen, takes or asks for a space on this device, drops one from it,
/// or gives one to another device that asked for it.
fn taking_command<E: Environment>(
	library: &mut Library,
	command: &SpaceCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		SpaceCommand::New { name, group, on } => {
			let also = devices_named(library, on)?;
			let now = environment.now();
			let space = if *group {
				library.create_group_on(name, &also, now, environment.rng())?
			} else {
				library.create_space_on(name, &also, now, environment.rng())?
			};
			output::space_made(space, name, json, output)
		}
		SpaceCommand::Take { wanted } => {
			let space = library.find_known_space(wanted)?;
			let now = environment.now();
			let taking = library.take_space(space, now, environment.rng())?;
			let name = library
				.known_spaces()?
				.into_iter()
				.find(|known| known.space == space)
				.and_then(|known| known.name)
				.unwrap_or_else(|| space.to_string());
			let this = library
				.replicas()?
				.into_iter()
				.find(|replica| replica.this)
				.ok_or(NodeError::NotListed)?;
			let named = (this.device_name.as_deref(), this.id);
			output::space_taken((space, &name), taking, named, json, output)
		}
		SpaceCommand::Drop { wanted } => {
			let space = library.find_space(wanted)?;
			let name = library
				.space_name(space)?
				.unwrap_or_else(|| space.to_string());
			let now = environment.now();
			let dropping = library.drop_space(space, now, environment.rng())?;
			output::space_dropping(&dropping, &name, json, output)
		}
		SpaceCommand::Allow { space, device } => {
			let space = library.find_space(space)?;
			let replicas = devices_named(library, std::slice::from_ref(device))?;
			let request = library
				.space_requests()?
				.into_iter()
				.find(|request| request.space == space && replicas.contains(&request.replica))
				.ok_or(NodeError::NoRequest)?;
			let now = environment.now();
			library.allow_space(space, request.replica, now, environment.rng())?;
			output::space_allowed(&request, json, output)
		}
		_ => unreachable!("only nib space new, take, drop and allow come here"),
	}
}

/// Runs `nib space role`: gives a member of a space a role.
fn role_command<E: Environment>(
	library: &mut Library,
	(space, member, role): (&str, &str, RoleArg),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.find_space(space)?;
	let member = library.find_member(space, member)?;
	let now = environment.now();
	library.give_role(space, member, role.role(), now, environment.rng())?;
	let name = library
		.members(space)?
		.into_iter()
		.find(|found| found.record == member)
		.and_then(|found| found.name);
	output::role_given(name.as_deref(), role.role(), json, output)
}

/// Runs `nib space accept`: accepts a member's changes that were set aside in a space, those
/// that accepting makes count, or with `confirmed` false, says how many it would.
fn accept_in_space_command<E: Environment>(
	library: &mut Library,
	(space, member, confirmed): (&str, &str, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.find_space(space)?;
	// Every record of the member's user: one who came back has another from before.
	let chosen = library.find_member(space, member)?;
	let members = library.members(space)?;
	let user = members
		.iter()
		.find(|found| found.record == chosen)
		.and_then(|found| found.user);
	let records: Vec<_> = members
		.iter()
		.filter(|found| found.record == chosen || (user.is_some() && found.user == user))
		.map(|found| found.record)
		.collect();
	let changes: Vec<_> = library
		.set_aside_in(space)?
		.into_iter()
		.filter(|aside| {
			aside.acceptable && aside.member.is_some_and(|member| records.contains(&member))
		})
		.map(|aside| aside.change)
		.collect();
	let accepted = if confirmed {
		let now = environment.now();
		library.accept_set_aside_in(space, &changes, now, environment.rng())?
	} else {
		library.check_admin(space, "accept changes that were set aside")?;
		changes.len()
	};
	output::accepted(accepted, confirmed, json, output)?;
	if confirmed || accepted == 0 {
		Ok(())
	} else {
		Err(CliError::UnconfirmedAccept)
	}
}

/// Runs `nib space leave`: leaves a space, or with `confirmed` false, says what leaving
/// would do.
fn leave_command<E: Environment>(
	library: &mut Library,
	(space, confirmed): (nibkeep_base::SpaceId, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let name = library
		.space_name(space)?
		.unwrap_or_else(|| space.to_string());
	if !confirmed {
		let closes = library.check_leave(space)?;
		output::leaving(&name, closes, json, output)?;
		return Err(CliError::UnconfirmedLeave);
	}
	let now = environment.now();
	let closed = library.leave(space, now, environment.rng())?;
	output::left(&name, closed, json, output)
}

/// The name to show for a space: its own, or else its ID.
fn space_display_name(library: &Library, space: nibkeep_base::SpaceId) -> Result<String, CliError> {
	Ok(library
		.space_name(space)?
		.unwrap_or_else(|| space.to_string()))
}

/// Keeps a member through a group out of a space, as `nib space remove` does for one, saying
/// first what it would do without `--yes`.
fn keep_out_command<E: Environment>(
	library: &mut Library,
	(space, member, confirmed): (nibkeep_base::SpaceId, &str, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let found = library.find_group_member(space, member)?;
	let name = space_display_name(library, space)?;
	if !confirmed {
		library.check_admin(space, "keep members out")?;
		output::removing((&name, Some(member)), json, output)?;
		return Err(CliError::UnconfirmedRemove);
	}
	let now = environment.now();
	library.keep_out(space, found, now, environment.rng())?;
	output::removed((&name, Some(member)), json, output)
}

/// Runs `nib space remove`: removes a member of a space, or with `confirmed` false, says what
/// removing them would do.
fn remove_command<E: Environment>(
	library: &mut Library,
	(space, member, confirmed): (&str, &str, bool),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.find_space(space)?;
	let record = match library.find_member(space, member) {
		Ok(record) => record,
		// A member through a group alone is kept out of the space.
		Err(NodeError::NoMember(_)) => {
			return keep_out_command(
				library,
				(space, member, confirmed),
				json,
				environment,
				output,
			);
		}
		Err(error) => return Err(error.into()),
	};
	// Removing oneself is leaving.
	if library.check_remove(space, record)? {
		return leave_command(library, (space, confirmed), json, environment, output);
	}
	let space_name = space_display_name(library, space)?;
	let name = library
		.members(space)?
		.into_iter()
		.find(|found| found.record == record)
		.and_then(|found| found.name);
	if !confirmed {
		output::removing((&space_name, name.as_deref()), json, output)?;
		return Err(CliError::UnconfirmedRemove);
	}
	let now = environment.now();
	library.remove_member(space, record, now, environment.rng())?;
	output::removed((&space_name, name.as_deref()), json, output)
}

/// Runs `nib space show`: a space's members, with the names of the people they are linked
/// to, the groups in it, the claims to its admin role and the changes set aside there; and
/// for a group, the requests in it that it join spaces.
fn show_space_command<E: Environment>(
	library: &Library,
	space: &str,
	json: bool,
	environment: &E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.find_space(space)?;
	let members = library.members(space)?;
	let mut names = std::collections::BTreeMap::new();
	for member in &members {
		let link = member.person;
		for person in [link.holds, link.by_admin, link.by_member]
			.into_iter()
			.flatten()
		{
			if let Some(name) = library.name(space, person)? {
				names.insert(person, name);
			}
		}
	}
	let requests = if library.is_group(space)? {
		Some(library.join_requests(space)?)
	} else {
		None
	};
	let shown = output::SpaceShown {
		members: &members,
		names: &names,
		groups: &library.space_groups(space)?,
		requests: requests.as_deref(),
		claims: &library.claims(space)?,
		set_aside: &library.set_aside_in(space)?,
	};
	output::space_shown(&shown, &environment.time_zone(), json, output)
}

/// Runs `nib purge`: purges values of an entity, or what a move left behind of it; or,
/// without an entity, lists what moves left behind.
fn purge_or_list<E: Environment>(
	library: &mut Library,
	cli: &Cli,
	environment: &mut E,
	timings: &mut Timings,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Command::Purge {
		entity, value, yes, ..
	} = &cli.command
	else {
		unreachable!("nib purge is the command");
	};
	let Some(entity) = entity else {
		return behind_command(library, cli, output);
	};
	let purge = (entity.as_str(), value.as_deref(), *yes);
	purge_either(library, purge, cli.json, environment, timings, output)
}

/// Runs `nib purge --behind` without an entity: lists what moves left behind and asked the
/// admins of its space to purge, in the space that --space names, or in every space.
fn behind_command(library: &Library, cli: &Cli, output: &mut dyn Write) -> Result<(), CliError> {
	let spaces = if cli.space.is_some() {
		vec![library.default_space()]
	} else {
		library.spaces()
	};
	let mut shown = Vec::new();
	for left_in in spaces {
		for request in library.purge_requests(left_in)? {
			let (now_in, moved) = request.moved_into;
			shown.push(output::LeftBehind {
				request,
				left_in: (left_in, library.space_name(left_in)?),
				name: library.name(now_in, moved)?,
				now_in: library.space_name(now_in)?,
			});
		}
	}
	output::purge_requests(&shown, cli.json, output)
}

/// Runs the parts of `nib space` that are about claims to the admin role: claims it, lists
/// the claims, and rejects one.
fn claim_command<E: Environment>(
	library: &mut Library,
	command: &SpaceCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		SpaceCommand::Claim { space, yes } => {
			let space = library.find_space(space)?;
			let name = space_display_name(library, space)?;
			if !*yes {
				library.check_claim(space)?;
				output::claiming(&name, json, output)?;
				return Err(CliError::UnconfirmedClaim);
			}
			let now = environment.now();
			library.claim_admin(space, now, environment.rng())?;
			output::claimed(&name, json, output)
		}
		SpaceCommand::Reject { space, member } => {
			let space = library.find_space(space)?;
			let record = library.find_member(space, member)?;
			let now = environment.now();
			library.reject_claim(space, record, now, environment.rng())?;
			let name = space_display_name(library, space)?;
			output::rejected_claim((&name, member), json, output)
		}
		_ => unreachable!("space_command passes only these"),
	}
}

/// Runs the parts of `nib space` that are about members through groups: makes one a direct
/// member, confirms such a record, and lets one back in.
fn direct_command<E: Environment>(
	library: &mut Library,
	command: &SpaceCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		SpaceCommand::LetIn { space, member } => {
			let space = library.find_space(space)?;
			let found = library.find_group_member(space, member)?;
			let now = environment.now();
			library.let_in(space, found, now, environment.rng())?;
			let name = space_display_name(library, space)?;
			output::let_in((&name, member), json, output)
		}
		SpaceCommand::MakeDirect {
			space,
			member,
			role,
		} => {
			let space = library.find_space(space)?;
			let found = library.find_group_member(space, member)?;
			let now = environment.now();
			library.make_direct(space, found, role.role(), now, environment.rng())?;
			let name = space_display_name(library, space)?;
			output::made_direct((&name, member), role.role(), json, output)
		}
		SpaceCommand::Confirm { space } => {
			let space = library.find_space(space)?;
			let now = environment.now();
			library.confirm_direct(space, now, environment.rng())?;
			let name = space_display_name(library, space)?;
			output::confirmed_direct(&name, json, output)
		}
		_ => unreachable!("space_command passes only these"),
	}
}

/// Runs the parts of `nib space` that are about groups: adds a group to a space, lists a
/// space's groups and a group's requests to join spaces, and agrees or withdraws.
fn group_command<E: Environment>(
	library: &mut Library,
	command: &SpaceCommand,
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match command {
		SpaceCommand::AddGroup { space, group, role } => {
			let (space, group) = (library.find_space(space)?, library.find_space(group)?);
			let now = environment.now();
			library.add_group(space, group, role.role(), now, environment.rng())?;
			let groups = library.space_groups(space)?;
			output::space_groups(&groups, json, output)
		}
		SpaceCommand::RemoveGroup { space, group } => {
			let (space, group) = (library.find_space(space)?, library.find_space(group)?);
			let now = environment.now();
			library.remove_group(space, group, now, environment.rng())?;
			output::space_groups(&library.space_groups(space)?, json, output)
		}
		SpaceCommand::Agree { group, space } | SpaceCommand::Withdraw { group, space } => {
			let group = library.find_space(group)?;
			let space = requested_space(library, group, space)?;
			let now = environment.now();
			if matches!(command, SpaceCommand::Withdraw { .. }) {
				library.withdraw_group(group, space, now, environment.rng())?;
			} else {
				library.agree_to_join(group, space, now, environment.rng())?;
			}
			output::join_requests(&library.join_requests(group)?, json, output)
		}
		_ => Ok(()),
	}
}

/// The space that a request in a group names, by its name there, whatever the case, or by
/// the start of its ID.
fn requested_space(
	library: &Library,
	group: nibkeep_base::SpaceId,
	reference: &str,
) -> Result<nibkeep_base::SpaceId, CliError> {
	let lowered = reference.to_lowercase();
	let requests = library.join_requests(group)?;
	// By name first, as nib finds a space; by the start of its ID, of at least four
	// characters, only where no name matches.
	let by_name: Vec<nibkeep_base::SpaceId> = requests
		.iter()
		.filter(|request| {
			request
				.name
				.as_deref()
				.is_some_and(|name| name.to_lowercase() == lowered)
		})
		.map(|request| request.space)
		.collect();
	let found: Vec<nibkeep_base::SpaceId> = if by_name.is_empty() && lowered.len() >= 4 {
		requests
			.iter()
			.filter(|request| request.space.to_string().starts_with(&lowered))
			.map(|request| request.space)
			.collect()
	} else {
		by_name
	};
	match found.as_slice() {
		[space] => Ok(*space),
		[] => Err(nibkeep_node::NodeError::NoGroupRequest.into()),
		_ => Err(nibkeep_node::NodeError::SpacesNamed(reference.to_owned()).into()),
	}
}

/// Runs `nib space link`: links a member to a person, or unlinks them.
fn link_command<E: Environment>(
	library: &mut Library,
	(space, member, person): (&str, &str, Option<&str>),
	json: bool,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let space = library.find_space(space)?;
	let member = library.find_member(space, member)?;
	let person = person
		.map(|person| library.find_person(space, person))
		.transpose()?;
	let now = environment.now();
	library.link_member(space, member, person, now, environment.rng())?;
	let found = library
		.members(space)?
		.into_iter()
		.find(|found| found.record == member);
	let name = found.as_ref().and_then(|found| found.name.clone());
	let link = found.map_or_default(|found| found.person);
	let person = match person {
		Some(person) => Some((person, library.name(space, person)?)),
		None => None,
	};
	output::linked(name.as_deref(), (person, link), json, output)
}

/// How long an invitation's code works.
const INVITATION_MINUTES: u64 = 10;

/// Runs `nib space invite`: shows a code, shows who asks to join with it, and adds them once
/// the person confirms, or at once with `confirmed`, as a member of `space` with `role`.
fn invite_command<E: Environment>(
	library: &mut Library,
	(invitation, confirmed): (&nibkeep_node::Invitation, bool),
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	let (space, role) = (invitation.space, invitation.role);
	// Before showing a code that could not be used.
	library.check_admin(space, "invite")?;
	if !confirmed && !environment.can_ask() {
		return Err(CliError::UnconfirmedInvite);
	}
	let name = library
		.space_name(space)?
		.unwrap_or_else(|| space.to_string());
	let suggest = role != MemberRole::Admin && library.suggests_second_admin(space)?;
	// The program that syncs this device stops its endpoint while this one invites; nib
	// stops waiting for it only as it ends.
	let _borrowed =
		nibkeep_node::borrow_network(library, &AtomicBool::new(false), UnwrapErr(SysRng))?;
	let code = PairingCode::generate(environment.rng());
	let network = library.network(cli.network(true))?;
	output::port_taken(&network, json, output)?;
	output::invitation_code(
		&code,
		(&name, role),
		(INVITATION_MINUTES, suggest),
		json,
		output,
	)?;
	let now = clock(environment);
	let mut unasked = false;
	let mut confirm = |invitee: &nibkeep_node::Invitee| {
		if confirmed {
			return true;
		}
		let question = output::invitee_question(invitee, (&name, role));
		if let Ok(Some(answer)) = environment.ask(&question) {
			matches!(answer.to_lowercase().as_str(), "y" | "yes")
		} else {
			unasked = true;
			false
		}
	};
	let printing = RefCell::new(output);
	let wrong_try = |error: &NodeError, left: usize| {
		let _ = output::wrong_try(error, left, json, &mut **printing.borrow_mut());
	};
	let invited = nibkeep_node::invite(
		library,
		&network,
		invitation,
		&code,
		&mut confirm,
		nibkeep_node::Showing {
			timeout: Duration::from_mins(INVITATION_MINUTES),
			wrong_try: &wrong_try,
		},
		&now,
		&mut UnwrapErr(SysRng),
	);
	match invited {
		Err(NodeError::Declined) if unasked => Err(CliError::UnconfirmedInvite),
		invited => output::invited(&invited?, (&name, role), json, printing.into_inner()),
	}
}

/// Runs `nib space join`: joins the space that the device showing `code` invites to, and, if
/// the invitation names the person whom the user is, links them once they agree, or at once
/// with `confirmed`.
fn join_space_command<E: Environment>(
	library: &mut Library,
	(code, confirmed, also): (&PairingCode, bool, &[EntityId]),
	cli: &Cli,
	environment: &mut E,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let json = cli.json;
	library.check_not_ai_agent("join spaces")?;
	let _borrowed =
		nibkeep_node::borrow_network(library, &AtomicBool::new(false), UnwrapErr(SysRng))?;
	let network = library.network(cli.network(false))?;
	output::progress("Looking for the device that shows the code…", json, output)?;
	let now = clock(environment);
	let joined = nibkeep_node::join_space(
		library,
		&network,
		code,
		FIND_SPONSOR,
		&now,
		environment.rng(),
	)?;
	// The devices of the person's chosen besides those that take new spaces.
	if !also.is_empty() {
		let now = environment.now();
		library.give_space(joined.space, also, now, environment.rng())?;
	}
	let linked = match &joined.person {
		None => None,
		Some((person, name)) => {
			// The space is joined: what follows only says whether the user is that person.
			let yes = confirmed
				|| matches!(
					environment.ask(&output::person_question(name, &joined.name)),
					Ok(Some(answer)) if matches!(answer.to_lowercase().as_str(), "y" | "yes")
				);
			let now = environment.now();
			let linked = yes
				&& match library.link_member(
					joined.space,
					joined.member,
					Some(*person),
					now,
					environment.rng(),
				) {
					Ok(()) => true,
					Err(error) => {
						environment.note(&format!("could not link you to {name}: {error}"));
						false
					}
				};
			Some(linked)
		}
	};
	output::joined_space(&joined, linked, json, output)
}

/// Tells the person how to keep a library on a drive without a journal, if that is why it was
/// not made.
fn no_journal(error: CliError, directory: &Path) -> CliError {
	match error {
		CliError::Node(NodeError::NoJournal) => CliError::NoJournal(directory.to_owned()),
		other => other,
	}
}

/// Starts a library, protected by the platform's keystore unless a passphrase is asked for,
/// or given in a file, or there is no keystore.
fn init<E: Environment>(
	directory: &Path,
	new: &NewLibrary<'_>,
	(passphrase, file): (bool, Option<&Path>),
	environment: &mut E,
	timings: &mut Timings,
) -> Result<Library, CliError> {
	// Before any passphrase is asked for. A new library is not started among copies of
	// another's replicas.
	if Library::exists(directory) {
		return Err(match Library::protection(directory) {
			Err(NodeError::Unfinished) => unfinished(directory),
			Err(NodeError::Copied) => CliError::CopiedHere(directory.to_owned()),
			_ => NodeError::Exists.into(),
		});
	}
	if let Some(first) = new_passphrase((passphrase, file), environment, timings)? {
		let protection = NewProtection::Passphrase(&first, environment.passphrase_params());
		create(directory, new, &protection, environment)
	} else {
		let protection = NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		};
		create(directory, new, &protection, environment).map_err(|error| match error {
			CliError::Node(error @ NodeError::Keys(KeysError::Keystore(_))) => {
				CliError::Keystore(error)
			}
			other => other,
		})
	}
}

/// The passphrase in the first line of a file, which nobody else may be able to read.
/// The passphrase that protects a new library's keys, where the person asked for one with
/// `passphrase` or in `file`, or this computer has no keystore: read from the file, or
/// asked for twice. `None` for the keystore.
fn new_passphrase<E: Environment>(
	(passphrase, file): (bool, Option<&Path>),
	environment: &mut E,
	timings: &mut Timings,
) -> Result<Option<zeroize::Zeroizing<String>>, CliError> {
	if !(passphrase || file.is_some() || !environment.keystore()) {
		return Ok(None);
	}
	Ok(Some(match file {
		Some(file) => passphrase_in(file)?,
		None => ask_twice(
			environment,
			timings,
			("A passphrase for the library: ", "The passphrase again: "),
			(|| CliError::EmptyPassphrase, || CliError::PassphrasesDiffer),
		)?,
	}))
}

fn passphrase_in(path: &Path) -> Result<zeroize::Zeroizing<String>, CliError> {
	use std::io::Read;
	let unreadable = |error: io::Error| CliError::PassphraseFile {
		path: path.to_owned(),
		error,
	};
	let mut file = std::fs::File::open(path).map_err(unreadable)?;
	let metadata = file.metadata().map_err(unreadable)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		if metadata.permissions().mode() & 0o077 != 0 {
			return Err(CliError::PassphraseFileOpen(path.to_owned()));
		}
	}
	// Room for all of it at once, so that growing leaves no copies behind.
	let room = usize::try_from(metadata.len()).unwrap_or(0);
	let mut text = zeroize::Zeroizing::new(String::with_capacity(room + 1));
	file.read_to_string(&mut text).map_err(unreadable)?;
	let end = text.find(['\n', '\r']).unwrap_or(text.len());
	text.truncate(end);
	if text.is_empty() {
		return Err(CliError::EmptyPassphrase);
	}
	Ok(text)
}

fn create<E: Environment>(
	directory: &Path,
	new: &NewLibrary<'_>,
	protection: &NewProtection<'_>,
	environment: &mut E,
) -> Result<Library, CliError> {
	let now = environment.now();
	let mut library = Library::create(directory, new, protection, now, environment.rng())?;
	library.make_thumbnails_with(Arc::new(RustDecoders));
	Ok(library)
}

/// Opens the library, asking for its passphrase if it has one, unless a file gives it.
fn open<E: Environment>(
	directory: &Path,
	passphrase_file: Option<&Path>,
	environment: &mut E,
	timings: &mut Timings,
) -> Result<Library, CliError> {
	let started = Instant::now();
	let mut waited = Duration::ZERO;
	let (mut library, opening) = match Library::protection(directory)? {
		Protection::Passphrase { .. } => {
			let passphrase = if let Some(file) = passphrase_file {
				passphrase_in(file)?
			} else {
				let asked = Instant::now();
				let passphrase = environment.passphrase("Passphrase: ")?;
				waited = asked.elapsed();
				add(&mut timings.waiting, waited);
				passphrase
			};
			Library::open_timed(directory, &Unlocker::Passphrase(&passphrase))?
		}
		Protection::Keystore { .. } => {
			if !environment.keystore() {
				return Err(CliError::NoKeystore);
			}
			Library::open_timed(directory, &Unlocker::Keystore)?
		}
	};
	// Unlocking is everything but the waiting and the spaces: reading which protection the
	// keys have, reaching the keystore, and unlocking.
	add(
		&mut timings.unlock,
		started.elapsed().saturating_sub(waited + opening.spaces),
	);
	add(&mut timings.open, opening.spaces);
	library.make_thumbnails_with(Arc::new(RustDecoders));
	Ok(library)
}

#[cfg(test)]
mod tests {
	use std::io::{self, Write};

	use super::{CliError, UntilUnread, parse_size};

	/// A writer that takes `room` bytes, then fails as `error` says.
	struct Closing {
		written: Vec<u8>,
		room: usize,
		error: io::ErrorKind,
	}

	impl Write for Closing {
		fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
			if self.written.len() + bytes.len() > self.room {
				return Err(self.error.into());
			}
			self.written.extend_from_slice(bytes);
			Ok(bytes.len())
		}

		fn flush(&mut self) -> io::Result<()> {
			Ok(())
		}
	}

	#[test]
	fn output_that_nothing_reads_is_taken_and_not_written() {
		let closing = Closing {
			written: Vec::new(),
			room: 6,
			error: io::ErrorKind::BrokenPipe,
		};
		let mut output = UntilUnread::new(closing);
		writeln!(output, "first").expect("written");
		writeln!(output, "second").expect("taken");
		writeln!(output, "third").expect("taken");
		output.flush().expect("flushed");
		assert_eq!(output.inner.written, b"first\n");
		assert!(output.unread);
		// Other errors are errors.
		let mut full = UntilUnread::new(Closing {
			written: Vec::new(),
			room: 0,
			error: io::ErrorKind::StorageFull,
		});
		assert_eq!(
			writeln!(full, "first").expect_err("full").kind(),
			io::ErrorKind::StorageFull
		);
		assert!(!full.unread);
	}

	#[test]
	fn sizes_are_read_in_bytes_and_units() {
		assert_eq!(parse_size("1073741824"), Ok(1 << 30));
		assert_eq!(parse_size("1GiB"), Ok(1 << 30));
		assert_eq!(parse_size("500 MB"), Ok(500_000_000));
		assert_eq!(parse_size("10b"), Ok(10));
		assert!(parse_size("1.5GB").is_err());
		assert!(parse_size("GB").is_err());
		assert!(parse_size("2PB").is_err());
		assert!(parse_size("99999999TiB").is_err());
		assert_eq!(super::output::size_text(1 << 30), "1 GiB");
		assert_eq!(super::output::size_text(500_000_000), "500 MB");
		assert_eq!(super::output::size_text(22), "22 bytes");
	}

	#[test]
	fn an_unsound_library_counts_its_problems() {
		assert_eq!(
			CliError::Unsound(1).to_string(),
			"the library has a problem"
		);
		assert_eq!(
			CliError::Unsound(3).to_string(),
			"the library has 3 problems"
		);
	}
}
