//! Syncing from login ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)):
//! `nibsync`, a program without a window that starts when the person logs in, holds the
//! sync role of the libraries they chose, as an app holds it while unlocked. Every NibKeep
//! program reads and writes the list of those libraries alike, in the person's folder of
//! settings; the service follows it, and writes beside it what it does for each.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nibkeep_base::EntityId;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_keys::{KeysError, Protection, Unlocker};
use serde_json::{Value as Json, json};

pub mod app;

use crate::info::write_file;
use crate::{Background, BackgroundOptions, Closed, Library, NodeError};

/// The list of libraries that the service syncs from login, and the copy that serves them.
const LIST: &str = "sync-at-login";

/// The file that a program locks while it changes the list.
const LIST_LOCK: &str = "sync-at-login.lock";

/// The file that the running service locks.
const RUNNING: &str = "sync-at-login.running";

/// What the service does for each library.
const STATUS: &str = "sync-at-login.status";

pub use crate::engines::ENGINE_VERSION;

/// What the service names itself in a replica's records, and the name of its program.
pub const SERVICE_PROGRAM: &str = "nibsync";

/// How long a look at whether the service runs may hold its lock, which the service then
/// waits out.
const LOOKING: Duration = Duration::from_millis(50);

/// How long a copy of `nibsync` may take to tell its version.
const VERSION_WAIT: Duration = Duration::from_secs(5);

/// How often a library's thread looks at what its holder does.
const STEP: Duration = Duration::from_millis(100);

/// How long a library's thread waits at most before it tries again to serve a library that
/// it could not.
const LONGEST_WAIT: Duration = Duration::from_mins(1);

/// This computer's folder within NibKeep's folder of state for the person, where the list
/// and the files beside it are: in `~/.local/state/nibkeep` on Linux (or
/// `$XDG_STATE_HOME/nibkeep`), `%LOCALAPPDATA%\NibKeep` on Windows, which no roaming profile
/// carries, and `~/Library/Application Support/NibKeep` on macOS. It is named by a hash of
/// the computer's ID, so that a program that syncs the home folder between computers makes
/// no two of them write the same file. Or the folder that `NIBKEEP_STATE` names, as tests
/// name one. `None` where there is no home folder.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[must_use]
pub fn settings_folder() -> Option<PathBuf> {
	if let Some(named) = std::env::var_os("NIBKEEP_STATE").filter(|named| !named.is_empty()) {
		return Some(PathBuf::from(named));
	}
	let base = directories::BaseDirs::new()?;
	let nibkeep = if cfg!(any(windows, target_vendor = "apple")) {
		base.data_local_dir().join("NibKeep")
	} else {
		base.state_dir()?.join("nibkeep")
	};
	Some(nibkeep.join(computer_name()))
}

/// The name of this computer's folder of state, and of its login entry: the first 8 bytes,
/// in hexadecimal, of a hash of the computer's ID, or `computer` where there is none.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn computer_name() -> String {
	use std::fmt::Write as _;
	/// The context of the hash that names this computer's folder.
	const COMPUTER_CONTEXT: &str = "nibkeep 2026-11 computer's folder of state";
	crate::place::computer_id().map_or_else(
		|| "computer".to_owned(),
		|id| {
			let hash = blake3::derive_key(COMPUTER_CONTEXT, &id);
			hash[..8].iter().fold(String::new(), |mut hex, byte| {
				let _ = write!(hex, "{byte:02x}");
				hex
			})
		},
	)
}

/// A library that the service syncs from login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Served {
	/// The ID of this computer's replica of it, which the replica that is here in its
	/// directory must have: a list that a program syncing dotfolders brought from another
	/// computer names replicas that are not here.
	pub replica: EntityId,
	/// The library's directory, as [`Library::open`] takes it.
	pub directory: PathBuf,
}

/// The list of libraries to sync from login, and the copy of `nibsync` that serves
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AtLogin {
	/// The copy of `nibsync` that serves them, which the login entry starts.
	pub program: Option<PathBuf>,
	/// The libraries.
	pub libraries: Vec<Served>,
}

impl AtLogin {
	/// Reads the list in the folder of settings: empty if there is none.
	///
	/// # Errors
	///
	/// Returns an error if the list cannot be read, or does not read as a list.
	pub fn read(settings: &Path) -> Result<Self, NodeError> {
		let path = settings.join(LIST);
		match fs::read(&path) {
			Ok(bytes) => Self::parse(&bytes).ok_or_else(|| {
				NodeError::Damaged(format!("{} is not a list of libraries", path.display()))
			}),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
			Err(error) => Err(NodeError::file(&path)(error)),
		}
	}

	/// Changes the list as `change` does, and writes it if it changed: while holding the
	/// lock on `sync-at-login.lock`, so that two programs never undo each other's changes.
	/// Returns what `change` returns.
	///
	/// # Errors
	///
	/// Returns an error if the list cannot be read, locked or written, or names a path that
	/// is not Unicode.
	pub fn edit<T>(settings: &Path, change: impl FnOnce(&mut Self) -> T) -> Result<T, NodeError> {
		Self::edit_and(settings, change, |_, _| ()).map(|(changed, ())| changed)
	}

	/// Changes the list as [`AtLogin::edit`] does, and once it is written, runs `then` on
	/// it and on what `change` returned, still holding the lock: as for the login entry,
	/// which changes with the list, and never before the list is written. Returns what both
	/// return.
	fn edit_and<T, U>(
		settings: &Path,
		change: impl FnOnce(&mut Self) -> T,
		then: impl FnOnce(&Self, &T) -> U,
	) -> Result<(T, U), NodeError> {
		fs::create_dir_all(settings).map_err(NodeError::file(settings))?;
		let path = settings.join(LIST_LOCK);
		let lock = open_lock(&path)?;
		lock.lock().map_err(NodeError::file(&path))?;
		let mut list = Self::read(settings)?;
		let before = list.clone();
		let changed = change(&mut list);
		if list != before {
			write_file(&settings.join(LIST), &list.to_bytes()?)?;
		}
		let then = then(&list, &changed);
		Ok((changed, then))
	}

	/// Adds a library, in place of any with its directory or replica.
	pub fn add(&mut self, served: Served) {
		self.libraries.retain(|listed| {
			listed.replica != served.replica && listed.directory != served.directory
		});
		self.libraries.push(served);
	}

	/// Removes the library in `directory`: whether the list had it.
	pub fn remove(&mut self, directory: &Path) -> bool {
		let before = self.libraries.len();
		self.libraries
			.retain(|listed| listed.directory != directory);
		self.libraries.len() != before
	}

	/// Removes the library whose replica on this computer is `replica`: whether the list had
	/// it.
	pub fn remove_replica(&mut self, replica: EntityId) -> bool {
		let before = self.libraries.len();
		self.libraries.retain(|listed| listed.replica != replica);
		self.libraries.len() != before
	}

	/// The library in `directory`, if the list has it.
	#[must_use]
	pub fn find(&self, directory: &Path) -> Option<&Served> {
		self.libraries
			.iter()
			.find(|listed| listed.directory == directory)
	}

	fn parse(bytes: &[u8]) -> Option<Self> {
		let json: Json = serde_json::from_slice(bytes).ok()?;
		let program = match &json["program"] {
			Json::Null => None,
			program => Some(PathBuf::from(program.as_str()?)),
		};
		let libraries = json["libraries"]
			.as_array()?
			.iter()
			.map(parse_served)
			.collect::<Option<_>>()?;
		Some(Self { program, libraries })
	}

	fn to_bytes(&self) -> Result<Vec<u8>, NodeError> {
		let program = self.program.as_deref().map(unicode).transpose()?;
		let libraries = self
			.libraries
			.iter()
			.map(served_json)
			.collect::<Result<Vec<_>, _>>()?;
		let json = json!({"program": program, "libraries": libraries});
		Ok(serde_json::to_vec_pretty(&json).unwrap_or_default())
	}
}

fn parse_served(json: &Json) -> Option<Served> {
	Some(Served {
		replica: json["replica"].as_str()?.parse().ok()?,
		directory: PathBuf::from(json["directory"].as_str()?),
	})
}

fn served_json(served: &Served) -> Result<Json, NodeError> {
	Ok(json!({
		"replica": served.replica.to_string(),
		"directory": unicode(&served.directory)?,
	}))
}

fn unicode(path: &Path) -> Result<&str, NodeError> {
	path.to_str()
		.ok_or_else(|| NodeError::NotUnicode(path.to_owned()))
}

/// What the service does for a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Serving {
	/// It is opening the library, and has not said more yet.
	Starting,
	/// It holds the library's sync role, and syncs it.
	Syncing,
	/// Another program holds the role, which it takes as soon as that one lets go.
	Waiting,
	/// The keystore's entry of the library's key is locked, as a Secret Service is after an
	/// automatic login, until the person unlocks it.
	WaitingForKeyring,
	/// No replica with the listed ID is here in the directory: the folder is gone, holds
	/// another library, or a copy of another computer's replica.
	NotHere,
	/// The library's keys are not in the keystore: a passphrase protects them, or the
	/// keystore has no key for them.
	NoKey,
	/// It failed, as this says; it tries again a while later.
	Failed(String),
}

impl Serving {
	/// How the status file names it.
	#[must_use]
	pub fn word(&self) -> &'static str {
		match self {
			Self::Starting => "starting",
			Self::Syncing => "syncing",
			Self::Waiting => "waiting",
			Self::WaitingForKeyring => "waiting for the keyring",
			Self::NotHere => "not here",
			Self::NoKey => "no key",
			Self::Failed(_) => "failed",
		}
	}

	/// Why it failed, if it did.
	#[must_use]
	pub fn error(&self) -> Option<&str> {
		match self {
			Self::Failed(error) => Some(error),
			_ => None,
		}
	}

	fn from_word(word: &str, error: Option<&str>) -> Option<Self> {
		Some(match word {
			"starting" => Self::Starting,
			"syncing" => Self::Syncing,
			"waiting" => Self::Waiting,
			"waiting for the keyring" => Self::WaitingForKeyring,
			"not here" => Self::NotHere,
			"no key" => Self::NoKey,
			"failed" => Self::Failed(error?.to_owned()),
			_ => return None,
		})
	}
}

/// What the service does for each library it serves, which it writes beside the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
	/// The engine's version of the copy that runs.
	pub version: String,
	/// Each library, and what the service does for it.
	pub libraries: Vec<(Served, Serving)>,
}

impl ServiceStatus {
	/// Reads what the service last wrote: `None` if it wrote nothing, as when none runs.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be read, or does not read as a status.
	pub fn read(settings: &Path) -> Result<Option<Self>, NodeError> {
		let path = settings.join(STATUS);
		match fs::read(&path) {
			Ok(bytes) => Self::parse(&bytes)
				.map(Some)
				.ok_or_else(|| NodeError::Damaged(format!("{} is not a status", path.display()))),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(error) => Err(NodeError::file(&path)(error)),
		}
	}

	/// What it does for the library in `directory`, if it serves it.
	#[must_use]
	pub fn of(&self, directory: &Path) -> Option<&Serving> {
		self.libraries
			.iter()
			.find(|(served, _)| served.directory == directory)
			.map(|(_, serving)| serving)
	}

	fn parse(bytes: &[u8]) -> Option<Self> {
		let json: Json = serde_json::from_slice(bytes).ok()?;
		let libraries = json["libraries"]
			.as_array()?
			.iter()
			.map(|library| {
				let serving =
					Serving::from_word(library["state"].as_str()?, library["error"].as_str())?;
				Some((parse_served(library)?, serving))
			})
			.collect::<Option<_>>()?;
		Some(Self {
			version: json["version"].as_str()?.to_owned(),
			libraries,
		})
	}

	fn write(&self, settings: &Path) -> Result<(), NodeError> {
		let mut libraries = Vec::new();
		for (served, serving) in &self.libraries {
			let mut library = served_json(served)?;
			library["state"] = json!(serving.word());
			if let Some(error) = serving.error() {
				library["error"] = json!(error);
			}
			libraries.push(library);
		}
		let json = json!({"version": self.version, "libraries": libraries});
		write_file(
			&settings.join(STATUS),
			&serde_json::to_vec_pretty(&json).unwrap_or_default(),
		)
	}
}

/// Whether a copy of `nibsync` runs for the person: whether one holds the lock on
/// `sync-at-login.running`, which this takes for a moment to tell.
///
/// # Errors
///
/// Returns an error if the lock's file cannot be opened or locked.
pub fn service_running(settings: &Path) -> Result<bool, NodeError> {
	let path = settings.join(RUNNING);
	if !path.exists() {
		return Ok(false);
	}
	let file = open_lock(&path)?;
	Ok(!try_alone(&file, &path)?)
}

/// The engine's version that a copy of `nibsync` tells, or `None` if it tells none
/// within a few seconds, as one that is gone.
#[must_use]
pub fn service_version(program: &Path) -> Option<String> {
	let mut child = Command::new(program)
		.arg("--version")
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::null())
		.spawn()
		.ok()?;
	let started = Instant::now();
	loop {
		match child.try_wait() {
			Ok(Some(status)) if status.success() => break,
			Ok(None) if started.elapsed() < VERSION_WAIT => std::thread::sleep(STEP),
			_ => {
				let _ = child.kill();
				let _ = child.wait();
				return None;
			}
		}
	}
	let mut told = String::new();
	child.stdout?.read_to_string(&mut told).ok()?;
	let version = told.trim().strip_prefix(SERVICE_PROGRAM)?.trim();
	(!version.is_empty()).then(|| version.to_owned())
}

/// Whether version `a` is newer than `b`, comparing their numbers in turn.
#[must_use]
pub fn newer(a: &str, b: &str) -> bool {
	let numbers = |version: &str| -> Vec<u64> {
		version
			.split(['.', '-', '+'])
			.map_while(|part| part.parse().ok())
			.collect()
	};
	numbers(a) > numbers(b)
}

/// Names `own`, this program's copy of `nibsync`, in the list, where the list names
/// none, or one that is gone, as after its app was removed, or one older than `own`, as
/// `version_of` tells of each; and points the login entry at it, if there is one, leaving it
/// off if the person turned it off: whether it named it. A copy of the same version stays,
/// so that two programs do not take it from each other, and so does one that does not tell
/// its version in time, as one that a scanner of viruses holds up.
///
/// # Errors
///
/// Returns the errors of [`AtLogin::edit`], and of [`LoginEntry::point`] once the list is
/// written.
pub fn name_own_copy(
	settings: &Path,
	own: &Path,
	version_of: &VersionOf<'_>,
	entry: Option<&LoginEntry>,
) -> Result<bool, NodeError> {
	let ((named, _), pointed) = AtLogin::edit_and(
		settings,
		|list| name_in(list, own, version_of),
		|_, (named, program)| match entry {
			Some(entry) if *named => entry.point(program).map(|_| ()),
			_ => Ok(()),
		},
	)?;
	pointed?;
	Ok(named)
}

/// Names `own` in `list`, as [`name_own_copy`] says: whether it did, and the copy that the
/// list names then.
fn name_in(list: &mut AtLogin, own: &Path, version_of: &VersionOf<'_>) -> (bool, PathBuf) {
	let replace = match list.program.as_deref() {
		None => true,
		Some(listed) if same_file(listed, own) => false,
		Some(listed) if !listed.is_file() => true,
		Some(listed) => match (version_of(own), version_of(listed)) {
			(Some(own), Some(listed)) => newer(&own, &listed),
			_ => false,
		},
	};
	let program = match &list.program {
		Some(listed) if !replace => listed.clone(),
		_ => own.to_owned(),
	};
	list.program = Some(program.clone());
	(replace, program)
}

/// Starts a copy of `nibsync`, apart from this program, so that it runs on when this
/// one ends.
///
/// # Errors
///
/// Returns an error if it cannot be started.
pub fn start_service(program: &Path) -> Result<(), NodeError> {
	let mut command = Command::new(program);
	command
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::null());
	#[cfg(unix)]
	{
		use std::os::unix::process::CommandExt;
		// Apart from the terminal's group, so that its Ctrl-C does not end it.
		command.process_group(0);
	}
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const DETACHED_PROCESS: u32 = 0x0000_0008;
		const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
		command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
	}
	let mut child = command.spawn().map_err(NodeError::file(program))?;
	// Reaped when it ends, if this program, as an app, runs that long.
	std::thread::spawn(move || {
		let _ = child.wait();
	});
	Ok(())
}

/// What tells the engine's version of a copy of `nibsync`, as [`service_version`] does.
pub type VersionOf<'a> = dyn Fn(&Path) -> Option<String> + 'a;

/// What became of the login entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryOutcome {
	/// The platform has none, as macOS has none yet, or tests keep away from it.
	NoEntry,
	/// It starts this program at login.
	On(PathBuf),
	/// It would start this program, but the person turned it off.
	Off(PathBuf),
	/// It could not be made, as this says.
	Failed(String),
}

impl EntryOutcome {
	/// In a word, as programs show it: `on`, `off` or `failed`; `None` where there is no
	/// entry.
	#[must_use]
	pub fn word(&self) -> Option<&'static str> {
		match self {
			Self::NoEntry => None,
			Self::On(_) => Some("on"),
			Self::Off(_) => Some("off"),
			Self::Failed(_) => Some("failed"),
		}
	}

	/// Why it could not be made, if it could not.
	#[must_use]
	pub fn error(&self) -> Option<&str> {
		match self {
			Self::Failed(error) => Some(error),
			_ => None,
		}
	}
}

/// What turning syncing from login on did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnedOn {
	/// The copy of `nibsync` to start, if any: the one listed, unless one runs; or the
	/// program's own, just named, which waits for the one that runs to read the list and end.
	pub start: Option<PathBuf>,
	/// What became of the login entry.
	pub entry: EntryOutcome,
}

/// Turns syncing from login on for a library: adds it to the list; names `own`, this
/// program's copy of `nibsync`, if it is newer than the one listed, as `version_of`
/// tells; and, once the list is written, makes the login entry start the listed copy,
/// leaving it off if the person turned it off. The entry changes while the list is locked,
/// so that turning another library off meanwhile never removes it. A login entry that
/// cannot be made leaves the library on the list, and is told.
///
/// # Errors
///
/// Returns [`NodeError::NotInKeystore`] if a passphrase protects the library's keys, as
/// nobody is there to type it at login, and the errors of [`AtLogin::edit`].
pub fn turn_on(
	settings: &Path,
	served: Served,
	(own, version_of): (&Path, &VersionOf<'_>),
	entry: Option<&LoginEntry>,
) -> Result<TurnedOn, NodeError> {
	if !matches!(
		Library::protection(&served.directory)?,
		Protection::Keystore { .. }
	) {
		return Err(NodeError::NotInKeystore);
	}
	let ((named, program), entry) = AtLogin::edit_and(
		settings,
		|list| {
			list.add(served);
			name_in(list, own, version_of)
		},
		|_, (_, program)| match entry {
			None => EntryOutcome::NoEntry,
			Some(entry) => match entry.make(program) {
				Ok(EntryState::On(program)) => EntryOutcome::On(program),
				Ok(EntryState::Off(program)) => EntryOutcome::Off(program),
				Ok(EntryState::Missing) => {
					EntryOutcome::Failed("it does not read back as written".to_owned())
				}
				Err(error) => EntryOutcome::Failed(error.to_string()),
			},
		},
	)?;
	let start = named || !service_running(settings)?;
	Ok(TurnedOn {
		start: start.then_some(program),
		entry,
	})
}

/// What turning syncing from login off did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnedOff {
	/// Whether the list had the library.
	pub removed: bool,
	/// Why the login entry could not be removed, with the last library, if it could not.
	pub entry_failed: Option<String>,
}

/// Turns syncing from login off for the library in `directory`, or, if the list has none
/// there, for `replica`, as after the library's folder was moved. The service lets go of
/// its role once it reads the list, and ends if it was the last. Once the list, written,
/// holds no library, the login entry goes too, while the list is locked; so a later turning
/// off removes an entry left by one that failed to. Where there is no list, nothing is
/// written, and the entry is left alone.
///
/// # Errors
///
/// Returns the errors of [`AtLogin::edit`].
pub fn turn_off(
	settings: &Path,
	(directory, replica): (&Path, Option<EntityId>),
	entry: Option<&LoginEntry>,
) -> Result<TurnedOff, NodeError> {
	if !settings.join(LIST).exists() {
		return Ok(TurnedOff {
			removed: false,
			entry_failed: None,
		});
	}
	let (removed, entry_failed) = AtLogin::edit_and(
		settings,
		|list| {
			list.remove(directory) || replica.is_some_and(|replica| list.remove_replica(replica))
		},
		|list, _| match entry {
			Some(entry) if list.libraries.is_empty() => {
				entry.remove().err().map(|error| error.to_string())
			}
			_ => None,
		},
	)?;
	Ok(TurnedOff {
		removed,
		entry_failed,
	})
}

/// What syncing from login does for a library, as a program shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtLoginStatus {
	/// Whether the list has the library.
	pub on: bool,
	/// What the copy that runs does for it, or `None` where none runs.
	pub serving: Option<Serving>,
	/// What the login entry says, where the platform has one, or why it cannot be read.
	pub entry: Option<Result<EntryState, String>>,
	/// The copy of `nibsync` that the list names, which the entry should start.
	pub listed: Option<PathBuf>,
}

impl AtLoginStatus {
	/// Whether the entry starts another program than the copy that the list names, or one
	/// that is gone.
	#[must_use]
	pub fn entry_stale(&self) -> bool {
		match &self.entry {
			Some(Ok(EntryState::On(program) | EntryState::Off(program))) => {
				self.listed.as_ref() != Some(program) || !program.is_file()
			}
			_ => false,
		}
	}

	/// What the login entry says, in a word, as programs show it: `off` where the person
	/// turned it off, whatever it starts; `missing`; `stale` where it starts another program
	/// than the copy that the list names; `on`; `unreadable`; `None` where there is none.
	#[must_use]
	pub fn entry_word(&self) -> Option<&'static str> {
		Some(match &self.entry {
			None => return None,
			Some(Err(_)) => "unreadable",
			Some(Ok(EntryState::Off(_))) => "off",
			Some(Ok(EntryState::Missing)) => "missing",
			Some(Ok(EntryState::On(_))) if self.entry_stale() => "stale",
			Some(Ok(EntryState::On(_))) => "on",
		})
	}
}

/// What syncing from login does for the library in `directory`, or the one whose replica
/// here is `replica`, as after its folder was named another way: as far as it can be read,
/// for a program to show; a list that cannot be read counts as one without it. Of a library
/// that is off, nothing more is read.
#[must_use]
pub fn status(
	settings: &Path,
	(directory, replica): (&Path, Option<EntityId>),
	entry: Option<&LoginEntry>,
) -> AtLoginStatus {
	let list = AtLogin::read(settings).unwrap_or_default();
	let listed = list.find(directory).cloned().or_else(|| {
		let replica = replica?;
		list.libraries
			.iter()
			.find(|listed| listed.replica == replica)
			.cloned()
	});
	let Some(listed) = listed else {
		return AtLoginStatus {
			on: false,
			serving: None,
			entry: None,
			listed: list.program,
		};
	};
	let serving = service_running(settings).unwrap_or(false).then(|| {
		ServiceStatus::read(settings)
			.ok()
			.flatten()
			.and_then(|status| status.of(&listed.directory).cloned())
			.unwrap_or(Serving::Starting)
	});
	AtLoginStatus {
		on: true,
		serving,
		entry: entry.map(|entry| entry.state().map_err(|error| error.to_string())),
		listed: list.program,
	}
}

/// What an app does as it starts, where the list holds libraries and the login entry is on,
/// or the platform has none: names `own`, its copy of `nibsync`, if the listed one is
/// gone or older, pointing the login entry along; and returns the copy to start, if any: its
/// own, just named, which waits for the one that runs to end, or the listed one, where none
/// runs, as after a crash. Where the person turned the entry off, or removed it, it does
/// nothing, so that the copy that runs, as one that turning on started, goes on.
///
/// # Errors
///
/// Returns the errors of [`name_own_copy`].
pub fn app_started(
	settings: &Path,
	own: &Path,
	version_of: &VersionOf<'_>,
	entry: Option<&LoginEntry>,
) -> Result<Option<PathBuf>, NodeError> {
	if !settings.join(LIST).exists() || AtLogin::read(settings)?.libraries.is_empty() {
		return Ok(None);
	}
	if let Some(entry) = entry
		&& !matches!(entry.state()?, EntryState::On(_))
	{
		return Ok(None);
	}
	let named = name_own_copy(settings, own, version_of, entry)?;
	let start = named || !service_running(settings)?;
	Ok(AtLogin::read(settings)?.program.filter(|_| start))
}

/// Whether the keystore holds its entry of a service and an account locked.
pub type KeyringLocked = dyn Fn(&str, &str) -> bool + Send + Sync;

/// How the service runs.
#[derive(Clone)]
pub struct ServiceOptions {
	/// Its own program, which the list must name for it to run.
	pub program: PathBuf,
	/// How it holds each library's role, as an app holds it.
	pub background: BackgroundOptions,
	/// Whether the keystore holds its entry of a service and an account locked, as a
	/// Secret Service does until the person unlocks it: asked without a prompt.
	pub locked: Arc<KeyringLocked>,
	/// How often it looks at the list, and again at a library that it cannot serve yet.
	pub look: Duration,
	/// The login entry, which it removes with the last library, as it does when it takes
	/// off the list a library that a passphrase protects.
	pub entry: Option<LoginEntry>,
}

impl ServiceOptions {
	/// For `program`, holding each role as `background` says, with a keystore that is never
	/// locked, looking every two seconds.
	#[must_use]
	pub fn new(program: PathBuf, background: BackgroundOptions) -> Self {
		Self {
			program,
			background,
			locked: Arc::new(|_, _| false),
			look: Duration::from_secs(2),
			entry: None,
		}
	}
}

/// Why the service ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceEnded {
	/// It was stopped, as when the session ends.
	Stopped,
	/// The list holds no library.
	Empty,
	/// The list names another copy of `nibsync`.
	NamesAnother,
}

/// Runs the service: takes the lock on `sync-at-login.running`, or waits for the copy that
/// holds it while the list names this one; then holds the sync role of each library on the
/// list, each on a thread of its own, following the list as it changes, and writes what it
/// does for each, until stopped, or the list is empty or names another copy. `now` is the
/// wall clock, and `rng` makes randomness for each thread.
///
/// # Errors
///
/// Returns an error if the folder of settings, or the lock in it, cannot be made.
pub fn serve_at_login<R: CryptoRng + Send + 'static>(
	settings: &Path,
	options: &ServiceOptions,
	now: impl Fn() -> u64 + Send + Sync + 'static,
	rng: impl Fn() -> R + Send + Sync + 'static,
	stop: &AtomicBool,
) -> Result<ServiceEnded, NodeError> {
	fs::create_dir_all(settings).map_err(NodeError::file(settings))?;
	let path = settings.join(RUNNING);
	let shared = Shared {
		settings: settings.to_owned(),
		options: options.clone(),
		now: Arc::new(now),
		rng: Arc::new(rng),
	};
	loop {
		let running = open_lock(&path)?;
		loop {
			if take_alone(&running, &path)? {
				break;
			}
			if !names(&AtLogin::read(settings)?, &options.program) {
				return Ok(ServiceEnded::NamesAnother);
			}
			if pause(options.look, stop) {
				return Ok(ServiceEnded::Stopped);
			}
		}
		let (ended, workers) = serve(&shared, stop);
		// Each lets go of its role once the syncs under way end, all at once; and the lock
		// goes first, so that a copy started meanwhile takes each role as it is let go.
		for worker in workers.values() {
			worker.stop.store(true, Ordering::SeqCst);
		}
		let _ = fs::remove_file(settings.join(STATUS));
		drop(running);
		drop(workers);
		// A program that added a library while this one ended found it running, and started
		// none: so it serves again, unless another copy runs by now.
		let again = ended == ServiceEnded::Empty
			&& AtLogin::read(settings)
				.is_ok_and(|list| !list.libraries.is_empty() && names(&list, &options.program))
			&& !service_running(settings)?;
		if !again {
			return Ok(ended);
		}
	}
}

/// Serves the listed libraries, as [`serve_at_login`] says, holding the lock on
/// `sync-at-login.running`: until stopped, or the list is empty or names another copy.
/// Returns why, and the libraries' threads, still running.
fn serve<R: CryptoRng + Send + 'static>(
	shared: &Shared<R>,
	stop: &AtomicBool,
) -> (ServiceEnded, BTreeMap<PathBuf, Worker>) {
	let (settings, options) = (shared.settings.as_path(), &shared.options);
	let mut workers: BTreeMap<PathBuf, Worker> = BTreeMap::new();
	let mut written = None;
	let mut list = AtLogin::default();
	let ended = loop {
		if stop.load(Ordering::SeqCst) {
			break ServiceEnded::Stopped;
		}
		// A list that does not read for a moment, as one written by hand, leaves things as
		// they were.
		if let Ok(read) = AtLogin::read(settings) {
			list = read;
		}
		if !names(&list, &options.program) {
			break ServiceEnded::NamesAnother;
		}
		if list.libraries.is_empty() {
			break ServiceEnded::Empty;
		}
		workers.retain(|directory, worker| {
			list.find(directory)
				.is_some_and(|listed| listed.replica == worker.served.replica)
		});
		for served in &list.libraries {
			if !workers.contains_key(&served.directory) {
				let worker = Worker::start(served.clone(), shared);
				workers.insert(served.directory.clone(), worker);
			}
		}
		let status = ServiceStatus {
			version: ENGINE_VERSION.to_owned(),
			libraries: list
				.libraries
				.iter()
				.filter_map(|served| {
					let worker = workers.get(&served.directory)?;
					Some((served.clone(), locked(&worker.serving).clone()))
				})
				.collect(),
		};
		if written.as_ref() != Some(&status) && status.write(settings).is_ok() {
			written = Some(status);
		}
		if pause(options.look.min(Duration::from_secs(1)), stop) {
			break ServiceEnded::Stopped;
		}
	};
	(ended, workers)
}

/// The wall clock, shared by the threads.
type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What makes randomness for each thread, shared by the threads.
type Randomness<R> = Arc<dyn Fn() -> R + Send + Sync>;

/// What every library's thread shares.
struct Shared<R> {
	/// The folder of the list.
	settings: PathBuf,
	options: ServiceOptions,
	now: Clock,
	rng: Randomness<R>,
}

/// A thread that serves one library: opens it once it can, and holds its role in the
/// background, until stopped or dropped.
struct Worker {
	served: Served,
	stop: Arc<AtomicBool>,
	serving: Arc<Mutex<Serving>>,
	thread: Option<JoinHandle<()>>,
}

impl Worker {
	fn start<R: CryptoRng + Send + 'static>(served: Served, shared: &Shared<R>) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		let serving = Arc::new(Mutex::new(Serving::Starting));
		let thread = {
			let (served, stop, serving) = (served.clone(), Arc::clone(&stop), Arc::clone(&serving));
			let (settings, options) = (shared.settings.clone(), shared.options.clone());
			let (now, rng) = (Arc::clone(&shared.now), Arc::clone(&shared.rng));
			std::thread::spawn(move || {
				let context = (settings.as_path(), &options);
				serve_library(&served, context, (&now, &rng), &stop, &serving);
			})
		};
		Self {
			served,
			stop,
			serving,
			thread: Some(thread),
		}
	}
}

impl Drop for Worker {
	/// Stops it, and waits until it let go of the role.
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

/// The thread of a [`Worker`]. Where it cannot serve the library, it tries again, at
/// first after `look`, then less and less often, but for a locked keyring, which the person
/// may unlock any time; and it turns syncing from login off for a library that a passphrase
/// protects now, as nobody is there to type it.
fn serve_library<R: CryptoRng + Send + 'static>(
	served: &Served,
	(settings, options): (&Path, &ServiceOptions),
	(now, rng): (&Clock, &Randomness<R>),
	stop: &AtomicBool,
	serving: &Mutex<Serving>,
) {
	let mut wait = options.look;
	let again = |wait: &mut Duration| {
		let stopped = pause(*wait, stop);
		*wait = (*wait * 2).min(LONGEST_WAIT);
		stopped
	};
	while !stop.load(Ordering::SeqCst) {
		let closed = match prepare(served, options.locked.as_ref()) {
			Ok(closed) => closed,
			Err(Cannot::Serve(Serving::WaitingForKeyring)) => {
				*locked(serving) = Serving::WaitingForKeyring;
				if pause(options.look, stop) {
					return;
				}
				continue;
			}
			Err(Cannot::Serve(cannot)) => {
				*locked(serving) = cannot;
				if again(&mut wait) {
					return;
				}
				continue;
			}
			Err(Cannot::Passphrase) => {
				*locked(serving) = Serving::NoKey;
				let _ = turn_off(settings, (&served.directory, None), options.entry.as_ref());
				if again(&mut wait) {
					return;
				}
				continue;
			}
		};
		let background = {
			let (now, rng) = (Arc::clone(now), Arc::clone(rng));
			Background::start(
				closed,
				options.background.clone(),
				move || now(),
				move || rng(),
			)
		};
		// Until stopped, or the holder ends, as after another program protected the library
		// anew, when the library is opened again with its new key.
		let mut held = false;
		loop {
			let state = background.state();
			held |= state.holding;
			*locked(serving) = match (state.holding, state.failed) {
				(true, _) => Serving::Syncing,
				(false, Some(failed)) => Serving::Failed(failed),
				(false, None) => Serving::Waiting,
			};
			if stop.load(Ordering::SeqCst) || background.ended() {
				break;
			}
			std::thread::sleep(STEP);
		}
		background.wait();
		if held {
			wait = options.look;
		}
		if again(&mut wait) {
			return;
		}
	}
}

/// Why the service cannot serve a library now.
enum Cannot {
	/// As this says.
	Serve(Serving),
	/// A passphrase protects the library's keys.
	Passphrase,
}

/// Opens the listed library with its key from the keystore, and closes it, without its
/// secrets; or says why it cannot.
fn prepare(served: &Served, keyring_locked: &KeyringLocked) -> Result<Closed, Cannot> {
	let failed = |error: NodeError| {
		Cannot::Serve(match error {
			NodeError::NoLibrary | NodeError::Copied | NodeError::SeveralHere(_) => {
				Serving::NotHere
			}
			NodeError::Keys(KeysError::Keystore(keyring_core::Error::NoEntry)) => Serving::NoKey,
			error => Serving::Failed(error.to_string()),
		})
	};
	let Protection::Keystore { service, account } =
		Library::protection(&served.directory).map_err(failed)?
	else {
		return Err(Cannot::Passphrase);
	};
	if keyring_locked(&service, &account) {
		return Err(Cannot::Serve(Serving::WaitingForKeyring));
	}
	let library = Library::open(&served.directory, &Unlocker::Keystore).map_err(failed)?;
	let here = library
		.replicas()
		.map_err(failed)?
		.into_iter()
		.find(|replica| replica.this)
		.map(|replica| replica.id);
	if here != Some(served.replica) {
		return Err(Cannot::Serve(Serving::NotHere));
	}
	// Without the secrets, which syncing never needs.
	Ok(library.close().without_secrets())
}

/// Whether the list names `program`.
fn names(list: &AtLogin, program: &Path) -> bool {
	list.program
		.as_deref()
		.is_some_and(|listed| same_file(listed, program))
}

/// Whether two paths name one file: the same path, or the same once resolved.
fn same_file(a: &Path, b: &Path) -> bool {
	a == b
		|| matches!(
			(fs::canonicalize(a), fs::canonicalize(b)),
			(Ok(a), Ok(b)) if a == b
		)
}

/// Waits for `wait`, or less if stopped: whether it was stopped.
fn pause(wait: Duration, stop: &AtomicBool) -> bool {
	let paused = Instant::now();
	while paused.elapsed() < wait {
		if stop.load(Ordering::SeqCst) {
			return true;
		}
		std::thread::sleep(STEP.min(wait));
	}
	stop.load(Ordering::SeqCst)
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_lock(path: &Path) -> Result<File, NodeError> {
	OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(path)
		.map_err(NodeError::file(path))
}

/// Takes the lock on a file alone, unless another holds it: whether it did.
fn try_alone(file: &File, path: &Path) -> Result<bool, NodeError> {
	match file.try_lock() {
		Ok(()) => Ok(true),
		Err(TryLockError::WouldBlock) => Ok(false),
		Err(TryLockError::Error(error)) => Err(NodeError::file(path)(error)),
	}
}

/// Takes the lock on a file alone, trying for a while, as another program may hold it for
/// a moment only to see whether the service runs: whether it did.
fn take_alone(file: &File, path: &Path) -> Result<bool, NodeError> {
	let tried = Instant::now();
	loop {
		if try_alone(file, path)? {
			return Ok(true);
		}
		if tried.elapsed() >= LOOKING {
			return Ok(false);
		}
		std::thread::sleep(Duration::from_millis(5));
	}
}

/// The name of the login entry, as the system's startup settings show it.
const ENTRY_NAME: &str = "NibKeep Sync";

/// The login entry that starts `nibsync` when the person logs in: the platform's own,
/// where the person finds the programs that start with their session. Each computer has one
/// of its own, named as its folder of state is, so that a program that syncs dotfolders, or
/// a roaming profile, never makes one computer's turning off remove another's.
#[derive(Clone, Debug)]
pub struct LoginEntry {
	place: EntryPlace,
}

#[derive(Clone, Debug)]
enum EntryPlace {
	/// An XDG autostart entry: its file.
	Autostart(PathBuf),
	/// A value of a Run key in the person's registry, named as it says; and the key in which
	/// Windows keeps whether the person turned it off.
	#[cfg(windows)]
	Run {
		key: String,
		approved: String,
		name: String,
	},
}

/// What the login entry says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryState {
	/// There is none, or none that reads as one.
	Missing,
	/// It starts this program at login.
	On(PathBuf),
	/// It would start this program, but the person turned it off in the system's startup
	/// settings.
	Off(PathBuf),
}

impl LoginEntry {
	/// This computer's login entry: on Windows, the value `NibKeep Sync` with the
	/// computer's name after it, of the Run key in the person's registry. None where tests
	/// keep away from it.
	#[cfg(windows)]
	#[must_use]
	pub fn platform() -> Option<Self> {
		if kept_away() {
			return None;
		}
		Some(Self::run_key(
			r"Software\Microsoft\Windows\CurrentVersion\Run",
			r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
			&format!("{ENTRY_NAME} {}", computer_name()),
		))
	}

	/// This computer's login entry: on Linux and the like, the autostart entry
	/// `~/.config/autostart/nibsync-COMPUTER.desktop`, or in `$XDG_CONFIG_HOME/autostart`.
	/// None where tests keep away from it.
	#[cfg(all(
		unix,
		not(any(target_os = "macos", target_os = "ios", target_os = "android"))
	))]
	#[must_use]
	pub fn platform() -> Option<Self> {
		if kept_away() {
			return None;
		}
		directories::BaseDirs::new().map(|base| {
			let file = format!("{SERVICE_PROGRAM}-{}.desktop", computer_name());
			Self::autostart(base.config_dir().join("autostart").join(file))
		})
	}

	/// This computer's login entry: none on macOS, where a login item is not built yet,
	/// nor on phones.
	#[cfg(not(any(
		windows,
		all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)
	)))]
	#[must_use]
	pub fn platform() -> Option<Self> {
		None
	}

	/// An XDG autostart entry in `file`, as tests make one.
	#[must_use]
	pub fn autostart(file: PathBuf) -> Self {
		Self {
			place: EntryPlace::Autostart(file),
		}
	}

	/// A value named `name` of the Run key `key` in the person's registry, whose being
	/// turned off Windows keeps in the key `approved`, as tests make one.
	#[cfg(windows)]
	#[must_use]
	pub fn run_key(key: &str, approved: &str, name: &str) -> Self {
		Self {
			place: EntryPlace::Run {
				key: key.to_owned(),
				approved: approved.to_owned(),
				name: name.to_owned(),
			},
		}
	}

	/// What it says now.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be read.
	pub fn state(&self) -> Result<EntryState, NodeError> {
		match &self.place {
			EntryPlace::Autostart(file) => Ok(autostart::state(autostart::read(file)?.as_deref())),
			#[cfg(windows)]
			EntryPlace::Run {
				key,
				approved,
				name,
			} => run::state(key, approved, name),
		}
	}

	/// Makes it start `program`: a new one, on, where there is none; otherwise the one
	/// there, starting `program` instead, and still off if the person turned it off. Writes
	/// nothing where it starts `program` already. Returns what it says then.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be read or written.
	pub fn make(&self, program: &Path) -> Result<EntryState, NodeError> {
		self.write(program, true)
	}

	/// Makes it start `program` instead, as [`LoginEntry::make`] does, if there is one: what
	/// it says then. A program that names a newer copy does this, and never makes an entry
	/// that the person removed.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be read or written.
	pub fn point(&self, program: &Path) -> Result<EntryState, NodeError> {
		self.write(program, false)
	}

	/// Removes it, with the record that the person turned it off, so that one made later
	/// starts on.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be removed.
	pub fn remove(&self) -> Result<(), NodeError> {
		match &self.place {
			EntryPlace::Autostart(file) => match fs::remove_file(file) {
				Err(error) if error.kind() != io::ErrorKind::NotFound => {
					Err(NodeError::file(file)(error))
				}
				_ => Ok(()),
			},
			#[cfg(windows)]
			EntryPlace::Run {
				key,
				approved,
				name,
			} => run::remove(key, approved, name),
		}
	}

	/// Makes it start `program`, a new one only where `new` allows: what it says then.
	fn write(&self, program: &Path, new: bool) -> Result<EntryState, NodeError> {
		match &self.place {
			EntryPlace::Autostart(file) => {
				let text = autostart::read(file)?;
				let written = match (autostart::state(text.as_deref()), &text) {
					(EntryState::On(listed) | EntryState::Off(listed), _) if listed == program => {
						return Ok(autostart::state(text.as_deref()));
					}
					(EntryState::Missing, _) if !new => return Ok(EntryState::Missing),
					(EntryState::Missing, _) | (_, None) => autostart::new(program),
					(_, Some(text)) => autostart::pointed(text, program),
				};
				// Through a link, as a program that manages dotfiles makes, to its file.
				let target = fs::canonicalize(file).unwrap_or_else(|_| file.clone());
				if let Some(folder) = target.parent() {
					fs::create_dir_all(folder).map_err(NodeError::file(folder))?;
				}
				write_file(&target, written.as_bytes())?;
				Ok(autostart::state(Some(&written)))
			}
			#[cfg(windows)]
			EntryPlace::Run {
				key,
				approved,
				name,
			} => {
				let state = run::state(key, approved, name)?;
				match &state {
					EntryState::On(listed) | EntryState::Off(listed) if listed == program => {
						return Ok(state);
					}
					EntryState::Missing if !new => return Ok(state),
					_ => {}
				}
				let anew = state == EntryState::Missing;
				run::write((key, approved, name), program, anew)?;
				run::state(key, approved, name)
			}
		}
	}
}

/// Whether tests keep away from the person's login entry: where `NIBKEEP_NO_LOGIN_ENTRY`
/// says so, or `NIBKEEP_STATE` names another folder of state, whose list the entry would not
/// serve.
#[cfg(any(
	windows,
	all(
		unix,
		not(any(target_os = "macos", target_os = "ios", target_os = "android"))
	)
))]
fn kept_away() -> bool {
	["NIBKEEP_NO_LOGIN_ENTRY", "NIBKEEP_STATE"]
		.iter()
		.any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

mod autostart {
	//! XDG autostart entries, as the Desktop Entry Specification has them.

	use std::fs;
	use std::io;
	use std::path::{Path, PathBuf};

	use super::{ENTRY_NAME, EntryState};
	use crate::NodeError;

	/// The entry's text, if there is a file.
	pub(super) fn read(file: &Path) -> Result<Option<String>, NodeError> {
		match fs::read(file) {
			Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(error) => Err(NodeError::file(file)(error)),
		}
	}

	/// A new entry that starts `program`, on.
	pub(super) fn new(program: &Path) -> String {
		format!(
			"[Desktop Entry]\nType=Application\nName={ENTRY_NAME}\nComment=Syncs your NibKeep libraries with your other devices while you are logged in\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
			exec(program)
		)
	}

	/// The lines of an entry, each with whether it is in the group `[Desktop Entry]`, after
	/// any byte-order mark.
	fn lines(text: &str) -> impl Iterator<Item = (bool, &str)> {
		let text = text.strip_prefix('\u{feff}').unwrap_or(text);
		let mut group = None;
		text.lines().map(move |line| {
			if line.starts_with('[') {
				group = Some(line.trim());
			}
			(
				group == Some("[Desktop Entry]") && !line.starts_with('['),
				line,
			)
		})
	}

	/// The entry `text`, starting `program` instead, and nothing else changed: its `Exec`
	/// replaced, or added after the group's header if it has none.
	pub(super) fn pointed(text: &str, program: &Path) -> String {
		let exec = format!("Exec={}\n", exec(program));
		let has_exec = lines(text).any(|(entry, line)| entry && key(line) == Some("Exec"));
		let mut pointed = String::new();
		for (entry, line) in lines(text) {
			if entry && key(line) == Some("Exec") {
				pointed.push_str(&exec);
				continue;
			}
			pointed.push_str(line);
			pointed.push('\n');
			if !has_exec && line.trim() == "[Desktop Entry]" {
				pointed.push_str(&exec);
			}
		}
		pointed
	}

	/// What the entry says: missing if there is no file, or no group `[Desktop Entry]`; off
	/// if it is hidden, as the specification has an entry deleted, or GNOME's flag turns it
	/// off. One whose `Exec` does not read starts nothing that it names.
	pub(super) fn state(text: Option<&str>) -> EntryState {
		let Some(text) = text else {
			return EntryState::Missing;
		};
		let header = lines(text).any(|(_, line)| line.trim() == "[Desktop Entry]");
		let (mut program, mut off) = (PathBuf::new(), false);
		for (_, line) in lines(text).filter(|(entry, _)| *entry) {
			let value = line.split_once('=').map_or("", |(_, value)| value.trim());
			match key(line) {
				Some("Exec") => program = parse_exec(value).unwrap_or_default(),
				Some("Hidden") => off |= value == "true",
				Some("X-GNOME-Autostart-enabled") => off |= value == "false",
				_ => {}
			}
		}
		match (header, off) {
			(false, _) => EntryState::Missing,
			(true, false) => EntryState::On(program),
			(true, true) => EntryState::Off(program),
		}
	}

	/// The key of a line `key=value`.
	fn key(line: &str) -> Option<&str> {
		line.split_once('=').map(|(key, _)| key.trim())
	}

	/// `program` as the value of `Exec`: quoted, with the characters that quotes reserve
	/// escaped, then with the backslashes of a string value escaped, and `%` doubled, as
	/// field codes take it.
	fn exec(program: &Path) -> String {
		let mut quoted = String::from("\"");
		for character in program.to_string_lossy().chars() {
			if matches!(character, '"' | '`' | '$' | '\\') {
				quoted.push('\\');
			}
			quoted.push(character);
		}
		quoted.push('"');
		quoted.replace('\\', "\\\\").replace('%', "%%")
	}

	/// The program of an `Exec` value: its first argument.
	fn parse_exec(value: &str) -> Option<PathBuf> {
		let mut unescaped = String::new();
		let mut characters = value.chars();
		while let Some(character) = characters.next() {
			match (character, characters.clone().next()) {
				('\\', Some('\\')) => {
					unescaped.push('\\');
					characters.next();
				}
				('\\', Some('s')) => {
					unescaped.push(' ');
					characters.next();
				}
				('%', Some('%')) => {
					unescaped.push('%');
					characters.next();
				}
				_ => unescaped.push(character),
			}
		}
		let mut program = String::new();
		let mut characters = unescaped.chars();
		if unescaped.starts_with('"') {
			characters.next();
			while let Some(character) = characters.next() {
				match character {
					'\\' => program.push(characters.next()?),
					'"' => break,
					_ => program.push(character),
				}
			}
		} else {
			unescaped.split(' ').next()?.clone_into(&mut program);
		}
		(!program.is_empty()).then(|| PathBuf::from(program))
	}
}

#[cfg(windows)]
mod run {
	//! A value of a Run key in the person's registry.

	use std::io;
	use std::path::{Path, PathBuf};

	use winreg::RegKey;
	use winreg::enums::HKEY_CURRENT_USER;

	use super::EntryState;
	use crate::NodeError;

	fn failed(key: &str) -> impl FnOnce(io::Error) -> NodeError {
		let key = key.to_owned();
		move |error| NodeError::file(Path::new(&format!(r"HKCU\{key}")))(error)
	}

	pub(super) fn state(key: &str, approved: &str, name: &str) -> Result<EntryState, NodeError> {
		let user = RegKey::predef(HKEY_CURRENT_USER);
		let value: String = match user.open_subkey(key).and_then(|run| run.get_value(name)) {
			Ok(value) => value,
			Err(error) if error.kind() == io::ErrorKind::NotFound => {
				return Ok(EntryState::Missing);
			}
			Err(error) => return Err(failed(key)(error)),
		};
		let program = PathBuf::from(value.trim().trim_matches('"'));
		// Windows marks an entry turned off with an odd first byte.
		let off = user
			.open_subkey(approved)
			.and_then(|approved| approved.get_raw_value(name))
			.is_ok_and(|value| value.bytes.first().is_some_and(|first| first & 1 == 1));
		Ok(if off {
			EntryState::Off(program)
		} else {
			EntryState::On(program)
		})
	}

	/// Writes the value; made `anew`, without Windows' record of an earlier one turned off,
	/// so that it starts on.
	pub(super) fn write(
		(key, approved, name): (&str, &str, &str),
		program: &Path,
		anew: bool,
	) -> Result<(), NodeError> {
		if anew {
			let removed = RegKey::predef(HKEY_CURRENT_USER)
				.open_subkey_with_flags(approved, winreg::enums::KEY_SET_VALUE)
				.and_then(|opened| opened.delete_value(name));
			if let Err(error) = removed
				&& error.kind() != io::ErrorKind::NotFound
			{
				return Err(failed(approved)(error));
			}
		}
		let (run, _) = RegKey::predef(HKEY_CURRENT_USER)
			.create_subkey(key)
			.map_err(failed(key))?;
		run.set_value(name, &format!("\"{}\"", program.display()))
			.map_err(failed(key))
	}

	pub(super) fn remove(key: &str, approved: &str, name: &str) -> Result<(), NodeError> {
		let user = RegKey::predef(HKEY_CURRENT_USER);
		for key in [key, approved] {
			let removed = user
				.open_subkey_with_flags(key, winreg::enums::KEY_SET_VALUE)
				.and_then(|opened| opened.delete_value(name));
			match removed {
				Err(error) if error.kind() != io::ErrorKind::NotFound => {
					return Err(failed(key)(error));
				}
				_ => {}
			}
		}
		Ok(())
	}
}

/// Windows' firewall: the rule that lets other replicas reach any of NibKeep's programs on
/// private networks, by port.
#[cfg(windows)]
pub mod firewall {
	use std::process::{Command, Stdio};

	/// The rule's name.
	pub const RULE: &str = "NibKeep";

	/// Whether the rule is there as NibKeep makes it: on, letting in, over UDP, with its
	/// ports, on private networks among others.
	#[must_use]
	pub fn has_rule() -> bool {
		powershell(
			"$rules = @(Get-NetFirewallRule -DisplayName NibKeep -ErrorAction SilentlyContinue | Where-Object { $_.Enabled -eq 'True' -and $_.Action -eq 'Allow' -and $_.Direction -eq 'Inbound' -and \"$($_.Profile)\" -match 'Private|Any' } | Where-Object { $filter = $_ | Get-NetFirewallPortFilter; $ports = @($filter.LocalPort); ($filter.Protocol -eq 'UDP') -and ($ports -contains '5353') -and ($ports -contains '53300-53309') }); if ($rules.Count -gt 0) { exit 0 } else { exit 1 }",
		)
	}

	/// Adds the rule, in place of any of its name, asking Windows for the administrator's
	/// consent: whether it is there then, as it is not when the person refused. The rule:
	/// UDP ports 53300 to 53309 for syncing, and 5353 for multicast DNS, by which replicas
	/// find each other, into any program, on private networks.
	#[must_use]
	pub fn add_rule() -> bool {
		let commands = "/c netsh advfirewall firewall delete rule name=NibKeep & netsh advfirewall firewall add rule name=NibKeep dir=in action=allow protocol=UDP localport=53300-53309,5353 profile=private";
		let _ = powershell(&format!(
			"Start-Process -FilePath cmd -ArgumentList '{commands}' -Verb RunAs -Wait -WindowStyle Hidden"
		));
		has_rule()
	}

	/// Runs a script of PowerShell, with no console window, as from an app: whether it
	/// succeeded.
	fn powershell(script: &str) -> bool {
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		Command::new("powershell")
			.creation_flags(CREATE_NO_WINDOW)
			.args(["-NoProfile", "-NonInteractive", "-Command", script])
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.status()
			.is_ok_and(|status| status.success())
	}
}
