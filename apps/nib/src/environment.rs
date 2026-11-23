//! What commands need from the world: randomness, the time, passphrases, keystores and the
//! clipboard.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use getrandom::SysRng;
use nibkeep_crypto::rand_core::{CryptoRng, UnwrapErr};
use nibkeep_node::at_login::{self, LoginEntry};
use nibkeep_node::{NodeError, PassphraseParams};
use nibkeep_passwords::{Board, ClipboardError};
use zeroize::Zeroizing;

/// The person's account on this computer, which names the user of a new library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
	/// The account's name, as `whoami` prints it, if the system names one.
	pub name: Option<String>,
	/// The person's full name, if the system knows it.
	pub full_name: Option<String>,
}

/// What commands need from the world.
pub trait Environment {
	/// The kind of random number generator.
	type Rng: CryptoRng;

	/// A cryptographically secure random number generator.
	fn rng(&mut self) -> &mut Self::Rng;

	/// The wall clock time, in milliseconds since 1970 (UTC).
	fn now(&self) -> u64;

	/// The device's offset from UTC, in minutes, at the current time.
	fn offset_minutes(&self) -> i32;

	/// The device's time zone, by which times are read and shown: a fixed offset from UTC,
	/// [`Environment::offset_minutes`], unless the environment knows the zone's rules.
	fn time_zone(&self) -> jiff::tz::TimeZone {
		jiff::tz::Offset::from_seconds(self.offset_minutes() * 60)
			.map_or(jiff::tz::TimeZone::UTC, jiff::tz::TimeZone::fixed)
	}

	/// Asks the person for a passphrase.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase cannot be read.
	fn passphrase(&mut self, prompt: &str) -> io::Result<Zeroizing<String>>;

	/// How much work deriving a key from a new passphrase takes.
	fn passphrase_params(&self) -> PassphraseParams;

	/// The computer's name, the default name of a device.
	fn computer_name(&self) -> String;

	/// The person's account on this computer, after which a new library's user is named.
	fn account(&self) -> Account;

	/// The current folder: `nib init` makes it a library, and the other commands look for
	/// their library in it and in the folders it is in.
	///
	/// # Errors
	///
	/// Returns an error if the current folder cannot be found, such as one deleted since.
	fn current_dir(&self) -> io::Result<PathBuf>;

	/// The folder of the library that commands work in outside every library's folder, if
	/// the person names one: `NIBKEEP_LIBRARY`.
	fn default_library(&self) -> Option<PathBuf>;

	/// A folder in which, and above which, commands do not look for their library, as
	/// Git's `GIT_CEILING_DIRECTORIES` says: tests keep to their own folders with it, and the
	/// end-to-end tests name it in `NIBKEEP_CEILING`.
	fn ceiling(&self) -> Option<PathBuf> {
		None
	}

	/// Whether the platform has a keystore to keep a library's keys in, connecting to it
	/// the first time.
	fn keystore(&mut self) -> bool;

	/// The languages the system is set to, first the one it uses, as the platform names
	/// them, such as `en_US.UTF-8` or `en-US`.
	fn languages(&self) -> Vec<String>;

	/// This device's downloads folder, as the platform names it, if it has one.
	fn downloads(&self) -> Option<PathBuf> {
		None
	}

	/// Tells the person something beside what the command writes: on standard error, so
	/// that output that other programs read stays as it is.
	fn note(&mut self, line: &str) {
		eprintln!("nib: {}", crate::printable(line));
	}

	/// Asks the person a question on the terminal, and returns the answer, or `None` if
	/// there is no terminal to ask on, as for a script or an AI agent.
	///
	/// # Errors
	///
	/// Returns an error if the answer cannot be read.
	fn ask(&mut self, question: &str) -> io::Result<Option<String>> {
		let _ = question;
		Ok(None)
	}

	/// Whether [`Environment::ask`] has someone to ask, as before showing a code that would
	/// be spent without an answer.
	fn can_ask(&self) -> bool {
		false
	}

	/// The clipboard, to copy secrets to: none, unless the environment has one.
	///
	/// # Errors
	///
	/// Returns an error if there is no clipboard, such as on a computer without a display.
	fn clipboard(&mut self) -> Result<Box<dyn Board>, ClipboardError> {
		Err(ClipboardError::new("there is none here"))
	}

	/// Makes an interruption, as with Ctrl+C, end the next wait rather than `nib`, from now
	/// on: before `nib` copies a secret, so that an interruption never leaves it on the
	/// clipboard.
	fn interruptible(&mut self) {}

	/// Waits for `duration`, or until the person interrupts `nib`, as with Ctrl+C, whichever
	/// comes first. Returns whether the person interrupted it.
	fn wait(&mut self, duration: Duration) -> bool {
		std::thread::sleep(duration);
		false
	}

	/// Makes an interruption, as with Ctrl+C, set `stop` rather than end `nib`: for a
	/// command that runs until stopped, and lets go of what it holds as it ends.
	fn stop_on_interrupt(&mut self, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
		let _ = stop;
	}

	/// This computer's folder of NibKeep's state, where the list of libraries synced from
	/// login is ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)): `None`
	/// where there is none.
	fn state_folder(&self) -> Option<PathBuf> {
		None
	}

	/// The copy of `nibkeep-sync` installed beside `nib`, if there is one.
	fn sync_program(&self) -> Option<PathBuf> {
		None
	}

	/// The program of `nib` itself, which AI clients run as a library's server.
	fn nib_program(&self) -> Option<PathBuf> {
		None
	}

	/// A command of the AI client's program `program`, such as `claude`, if it is installed
	/// here, to tell the client of a server or take it away: none unless the environment
	/// finds it.
	fn ai_client(&self, program: &str) -> Option<std::process::Command> {
		let _ = program;
		None
	}

	/// The person's own Git, with their settings and credentials, as a command to give
	/// arguments to ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees)).
	fn git(&self) -> std::process::Command {
		let mut git = std::process::Command::new("git");
		clear_git_variables(&mut git);
		git
	}

	/// The login entry that starts `nibkeep-sync`, where the platform has one.
	fn login_entry(&self) -> Option<LoginEntry> {
		None
	}

	/// Lets the person's other devices reach this computer through its firewall, where
	/// `nib` does that, as on Windows, asking for the administrator's consent.
	fn open_firewall(&mut self) -> Firewall {
		Firewall::NotHere
	}

	/// The engine's version that a copy of `nibkeep-sync` tells, or `None` if it is gone.
	fn sync_version(&self, program: &Path) -> Option<String> {
		at_login::service_version(program)
	}

	/// Starts a copy of `nibkeep-sync`, apart from `nib`, so that it runs on when `nib`
	/// ends.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be started.
	fn start_sync(&mut self, program: &Path) -> Result<(), NodeError> {
		at_login::start_service(program)
	}
}

/// What `nib` did about the firewall, so that the person's other devices reach `nibkeep-sync`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firewall {
	/// Nothing: `nib` changes no firewall here, as on Linux and macOS.
	NotHere,
	/// Nothing: the rule is there already.
	Open,
	/// It added the rule, with the administrator's consent.
	Opened,
	/// The rule is not there: the person refused, or could not consent.
	Refused,
}

/// The real world: the operating system's randomness, clock and keystore, and the
/// terminal.
pub struct System {
	rng: UnwrapErr<SysRng>,
	/// Whether there is a keystore, once a command asked.
	keystore: Option<bool>,
	/// The interruptions, once `nib` takes them instead of ending.
	interruptions: Option<mpsc::Receiver<()>>,
}

impl System {
	/// The real world. The platform's keystore is connected to only when a command needs
	/// it.
	#[must_use]
	pub fn new() -> Self {
		Self {
			rng: UnwrapErr(SysRng),
			keystore: None,
			interruptions: None,
		}
	}
}

impl Default for System {
	fn default() -> Self {
		Self::new()
	}
}

impl Environment for System {
	type Rng = UnwrapErr<SysRng>;

	fn rng(&mut self) -> &mut Self::Rng {
		&mut self.rng
	}

	fn now(&self) -> u64 {
		SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map_or(0, |since| {
				u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
			})
	}

	fn offset_minutes(&self) -> i32 {
		jiff::tz::TimeZone::system()
			.to_offset(jiff::Timestamp::now())
			.seconds()
			/ 60
	}

	fn time_zone(&self) -> jiff::tz::TimeZone {
		jiff::tz::TimeZone::system()
	}

	fn passphrase(&mut self, prompt: &str) -> io::Result<Zeroizing<String>> {
		rpassword::prompt_password(prompt).map(Zeroizing::new)
	}

	fn passphrase_params(&self) -> PassphraseParams {
		// Every command derives the key again, so the parameters for little memory.
		PassphraseParams::LOW_MEMORY
	}

	fn computer_name(&self) -> String {
		gethostname::gethostname().to_string_lossy().into_owned()
	}

	fn account(&self) -> Account {
		// Read as the system gives them, so that a name in another encoding still names the
		// user.
		let lossy = |name: std::ffi::OsString| name.to_string_lossy().into_owned();
		Account {
			name: whoami::username_os().ok().map(lossy),
			full_name: whoami::realname_os().ok().map(lossy),
		}
	}

	fn current_dir(&self) -> io::Result<PathBuf> {
		std::env::current_dir()
	}

	fn default_library(&self) -> Option<PathBuf> {
		std::env::var_os("NIBKEEP_LIBRARY")
			.filter(|folder| !folder.is_empty())
			.map(PathBuf::from)
	}

	fn ceiling(&self) -> Option<PathBuf> {
		// A full path, through no link, as the current folder's are, which it is compared
		// with.
		std::env::var_os("NIBKEEP_CEILING")
			.map(PathBuf::from)
			.filter(|folder| folder.is_absolute())
			.map(|folder| nibkeep_node::resolved_path(&folder).unwrap_or(folder))
	}

	fn keystore(&mut self) -> bool {
		// Tests keep nib from the computer's keystore, as they keep apps: with
		// `NIBKEEP_NO_KEYSTORE`, there is none.
		*self
			.keystore
			.get_or_insert_with(|| !at_login::app::kept_away() && platform_keystore())
	}

	fn downloads(&self) -> Option<PathBuf> {
		directories::UserDirs::new()
			.and_then(|directories| directories.download_dir().map(PathBuf::from))
	}

	fn state_folder(&self) -> Option<PathBuf> {
		#[cfg(not(any(target_os = "android", target_os = "ios")))]
		return at_login::settings_folder();
		#[cfg(any(target_os = "android", target_os = "ios"))]
		None
	}

	fn login_entry(&self) -> Option<LoginEntry> {
		LoginEntry::platform()
	}

	fn open_firewall(&mut self) -> Firewall {
		#[cfg(windows)]
		{
			use nibkeep_node::at_login::firewall;
			if firewall::has_rule() {
				Firewall::Open
			} else if firewall::add_rule() {
				Firewall::Opened
			} else {
				Firewall::Refused
			}
		}
		#[cfg(not(windows))]
		Firewall::NotHere
	}

	fn sync_program(&self) -> Option<PathBuf> {
		let name = format!(
			"{}{}",
			at_login::SERVICE_PROGRAM,
			std::env::consts::EXE_SUFFIX
		);
		let program = std::env::current_exe().ok()?.with_file_name(name);
		program.is_file().then_some(program)
	}

	fn nib_program(&self) -> Option<PathBuf> {
		std::env::current_exe().ok()
	}

	fn ai_client(&self, program: &str) -> Option<std::process::Command> {
		let name = format!("{program}{}", std::env::consts::EXE_SUFFIX);
		let found = std::env::split_paths(&std::env::var_os("PATH")?)
			.map(|folder| folder.join(&name))
			.find(|path| path.is_file())?;
		Some(std::process::Command::new(found))
	}

	fn can_ask(&self) -> bool {
		io::stdin().is_terminal() && io::stderr().is_terminal()
	}

	fn ask(&mut self, question: &str) -> io::Result<Option<String>> {
		// Only a person at a terminal answers; a program that runs nib has no say.
		if !self.can_ask() {
			return Ok(None);
		}
		let mut stderr = io::stderr();
		write!(stderr, "{}", crate::printable(question))?;
		stderr.flush()?;
		let mut answer = String::new();
		if io::stdin().read_line(&mut answer)? == 0 {
			return Ok(None);
		}
		Ok(Some(answer.trim().to_owned()))
	}

	fn clipboard(&mut self) -> Result<Box<dyn Board>, ClipboardError> {
		#[cfg(not(any(target_os = "android", target_os = "ios")))]
		return Ok(Box::new(nibkeep_passwords::SystemBoard::new()?));
		#[cfg(any(target_os = "android", target_os = "ios"))]
		Err(ClipboardError::new("nib has no clipboard on phones"))
	}

	fn interruptible(&mut self) {
		// Ctrl+C, and a terminal closed on Linux and macOS, end the wait instead of nib: on
		// Windows and macOS what nib copied would otherwise stay on the clipboard. Windows
		// ends a program whose console window is closed without waiting for it, so there the
		// copy stays until something else is copied.
		if self.interruptions.is_none() {
			let (interrupted, interruptions) = mpsc::channel();
			if ctrlc::set_handler(move || {
				let _ = interrupted.send(());
			})
			.is_ok()
			{
				self.interruptions = Some(interruptions);
			}
		}
	}

	fn stop_on_interrupt(&mut self, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
		if self.interruptions.is_none() {
			let _ = ctrlc::set_handler(move || {
				stop.store(true, std::sync::atomic::Ordering::SeqCst);
			});
		}
	}

	fn wait(&mut self, duration: Duration) -> bool {
		self.interruptible();
		if let Some(interruptions) = &self.interruptions {
			interruptions.recv_timeout(duration).is_ok()
		} else {
			std::thread::sleep(duration);
			false
		}
	}

	fn languages(&self) -> Vec<String> {
		// GNU systems rank the languages of messages in LANGUAGE, as `en_US:ru`.
		#[cfg(unix)]
		if let Ok(ranked) = std::env::var("LANGUAGE")
			&& !ranked.is_empty()
		{
			return ranked.split(':').map(str::to_owned).collect();
		}
		sys_locale::get_locales().collect()
	}
}

/// Makes the platform's keystore the one that libraries use. Returns whether there is one.
#[must_use]
pub fn platform_keystore() -> bool {
	#[cfg(target_os = "macos")]
	let store = apple_native_keyring_store::keychain::Store::new();
	#[cfg(target_os = "windows")]
	let store = windows_native_keyring_store::Store::new();
	#[cfg(all(
		unix,
		not(any(target_os = "macos", target_os = "ios", target_os = "android"))
	))]
	let store = zbus_secret_service_keyring_store::Store::new();
	#[cfg(any(
		target_os = "macos",
		target_os = "windows",
		all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)
	))]
	{
		match store {
			Ok(store) => {
				keyring_core::set_default_store(store);
				true
			}
			Err(_) => false,
		}
	}
	#[cfg(not(any(
		target_os = "macos",
		target_os = "windows",
		all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)
	)))]
	false
}

/// Clears from a command of Git the variables that would make it work on another repository
/// than the folder it is given, as a hook or an alias of Git that runs `nib` sets them.
pub use nibkeep_git::clear_variables as clear_git_variables;
