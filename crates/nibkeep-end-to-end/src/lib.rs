//! The end-to-end tests of `nib` ([technical design: end-to-end tests]): the program itself, run as a person or
//! a script runs it, on devices that are folders of this computer, each with an environment of
//! its own, alone or syncing with each other over the home network.
//!
//! This crate reaches `nib` only as a program, never through its code, on which it does not
//! depend. The program is the one that `NIBKEEP_NIB` names, such as a release build, or else
//! the one that the tests build from this workspace as they start, as `cargo install` would,
//! but in the tests' profile.
//!
//! The tests are ignored but when asked for, as some need a network that carries multicast:
//! `cargo end-to-end` runs them all, and `cargo end-to-end NAME` those whose names hold
//! `NAME`. Each test's folders stay in `target/tmp`, under `end to end, NAME`, until it runs
//! again, with `commands.sh`, what it ran. `SNAPSHOTS=overwrite` writes what the commands
//! printed into the tests, for review.
//!
//! [technical design: end-to-end tests]: ../../../docs/Technical-design.md#18-end-to-end-tests

mod process;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub use nibkeep_testkit::write_private;
use process::Process;
use regex::Regex;
pub use snapbox::str;
use snapbox::{Assert, IntoData, Redactions};

/// How long a command may take before the test kills it and fails: long enough for a debug
/// build to pair, sync or add a few hundred files on a slow computer.
const PATIENCE: Duration = Duration::from_secs(180);

/// The variables of the test's own environment that the program gets: the folders of the
/// programs it runs, such as Git, and what Windows needs to run a program at all.
const KEPT: [&str; 8] = [
	"PATH",
	"SystemRoot",
	"SystemDrive",
	"windir",
	"ComSpec",
	"PATHEXT",
	"PROCESSOR_ARCHITECTURE",
	"NUMBER_OF_PROCESSORS",
];

/// Whether the program is on this device: not on another one, such as a phone, to which tests
/// go without it, unless `NIBKEEP_NIB` names it there.
#[must_use]
pub fn program_here() -> bool {
	std::env::var_os("NIBKEEP_NIB").is_some() || std::env::var_os("NIBKEEP_TEST_DIR").is_none()
}

/// The folder of the build that holds the running test, `target/PROFILE`, and the profile's
/// name, as cargo takes it.
fn build_folder() -> (PathBuf, String) {
	let test = std::env::current_exe().expect("the test's path");
	// A test is in target/PROFILE/deps, or, as cargo builds since Rust 1.100, in
	// target/PROFILE/build/PACKAGE/HASH/out: the folder is the one that holds the nearest
	// `deps` or `build` above the test.
	let folder = test
		.ancestors()
		.skip(1)
		.find(|above| {
			above
				.file_name()
				.is_some_and(|name| name == "deps" || name == "build")
		})
		.and_then(Path::parent)
		.expect("the test is below target/PROFILE/deps or target/PROFILE/build")
		.to_owned();
	let profile = match folder.file_name().and_then(OsStr::to_str) {
		Some("debug") => "dev".to_owned(),
		Some(other) => other.to_owned(),
		None => panic!("no profile in {}", folder.display()),
	};
	(folder, profile)
}

/// The program that the tests run: the one that `NIBKEEP_NIB` names, or else `nib` built from
/// this workspace, once for every test executable.
fn program() -> &'static Path {
	static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
	PROGRAM.get_or_init(|| {
		if let Some(program) = std::env::var_os("NIBKEEP_NIB") {
			let program = PathBuf::from(program);
			assert!(program.is_absolute(), "NIBKEEP_NIB must be a full path");
			return program;
		}
		assert!(
			program_here(),
			"the program is not on this device; NIBKEEP_NIB names it"
		);
		let (folder, profile) = build_folder();
		let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
		let mut build = Command::new(cargo);
		build
			.args(["build", "--package", "nibkeep-cli", "--bins", "--profile"])
			.arg(&profile)
			.current_dir(env!("CARGO_MANIFEST_DIR"));
		// The tests are in target/PROFILE, or in target/TRIPLE/PROFILE if they were built for
		// a target named, whose triple the build script tells.
		let above = folder.parent().expect("target");
		let triple = env!("END_TO_END_TARGET");
		if above.file_name().is_some_and(|name| name == triple) {
			build
				.arg("--target")
				.arg(triple)
				.arg("--target-dir")
				.arg(above.parent().expect("target"));
		} else {
			build.arg("--target-dir").arg(above);
		}
		// Without what cargo tells a test about its own crate, which build scripts that
		// watch such variables would take for a change, and build again every time.
		for (variable, _) in std::env::vars_os() {
			if variable.to_str().is_some_and(told_to_a_test) {
				build.env_remove(variable);
			}
		}
		let outcome = build.output().expect("runs cargo");
		assert!(
			outcome.status.success(),
			"cargo could not build nib:\n{}",
			String::from_utf8_lossy(&outcome.stderr)
		);
		folder.join(format!("nib{}", std::env::consts::EXE_SUFFIX))
	})
}

/// Whether cargo sets this variable for a test, about the test's own crate.
fn told_to_a_test(variable: &str) -> bool {
	[
		"CARGO_PKG_",
		"CARGO_MANIFEST_",
		"CARGO_CFG_",
		"CARGO_FEATURE_",
	]
	.iter()
	.any(|start| variable.starts_with(start))
		|| [
			"CARGO_CRATE_NAME",
			"CARGO_BIN_NAME",
			"CARGO_PRIMARY_PACKAGE",
			"CARGO_TARGET_TMPDIR",
			"CARGO_RUSTC_CURRENT_DIR",
			"OUT_DIR",
		]
		.contains(&variable)
}

/// A folder as the system names it, through no link, as the program sees its current folder;
/// on Windows, without the `\\?\` that its full names start with.
fn canonical(folder: &Path) -> PathBuf {
	let canonical = fs::canonicalize(folder).unwrap_or_else(|_| folder.to_owned());
	#[cfg(windows)]
	if let Some(plain) = canonical
		.to_str()
		.and_then(|path| path.strip_prefix(r"\\?\"))
		&& !plain.starts_with("UNC\\")
	{
		return PathBuf::from(plain);
	}
	canonical
}

/// An end-to-end test: devices on this computer, in folders of their own under the test's,
/// and what they ran. As it ends, it checks every device; if it failed, it says where its
/// folders are, and what it ran.
pub struct EndToEnd(Rc<Shared>);

/// What a test and its devices share.
struct Shared {
	name: String,
	folder: PathBuf,
	program: PathBuf,
	devices: RefCell<Vec<Folders>>,
	/// What changes from run to run in what the program prints, such as IDs.
	redactions: Redactions,
	/// How many commands ran in the background, which the transcript numbers.
	jobs: Cell<usize>,
	/// What no device may hold in plain, besides its passphrase, such as a token.
	secrets: RefCell<Vec<String>>,
	/// What each device's settings of Git hold, by the device's name, as the test wrote
	/// them: what else is there, the program wrote.
	git_settings: RefCell<BTreeMap<String, String>>,
	/// The devices whose function for Git the transcript has.
	git_functions: RefCell<BTreeSet<String>>,
}

impl EndToEnd {
	/// A test in a folder of its own, empty, named after `name`.
	///
	/// # Panics
	///
	/// Panics if the program is not on this device, or cannot be built.
	#[must_use]
	pub fn new(name: &str) -> Self {
		let program = program().to_owned();
		let (build, _) = build_folder();
		let tmp = build.parent().expect("target").join("tmp");
		let given = nibkeep_testkit::empty_directory(tmp, &format!("end to end, {name}"));
		let folder = canonical(&given);
		let mut redactions = Redactions::new();
		for root in [&folder, &given] {
			redactions
				.insert("[ROOT]", root.clone())
				.expect("a placeholder");
		}
		for (placeholder, pattern) in [
			(
				"[ID]",
				r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
			),
			// The short IDs that start an ID, but not the start of a whole ID.
			(
				"[SHORT_ID]",
				r"(?m)(?<redacted>\b[0-9a-f]{8}\b)(?:[^-0-9a-f]|$)",
			),
			("[CODE]", r"\b\d\d-\d{3}-\d{3}\b"),
			("[TIME]", r"\b\d{4}-\d\d-\d\d \d\d:\d\d\b"),
			// A recovery code, as nib prints it, five groups to a line, and its name.
			("[RECOVERY_CODE]", r"\b[0-9A-Z]{5}(?:-[0-9A-Z]{5}){2,4}\b"),
			(
				"[CODE_NAME]",
				r"\b(?:[Rr]ecovery code,?|[Cc]ode) (?<redacted>[0-9A-Z]{4})\b",
			),
		] {
			redactions
				.insert(placeholder, Regex::new(pattern).expect("a pattern"))
				.expect("a placeholder");
		}
		let shared = Shared {
			name: name.to_owned(),
			folder,
			program,
			devices: RefCell::default(),
			redactions,
			jobs: Cell::new(0),
			secrets: RefCell::default(),
			git_settings: RefCell::default(),
			git_functions: RefCell::default(),
		};
		shared.write(&format!(
			"#!/bin/sh\n\
			 # What the end-to-end test \"{name}\" ran, in order. Each device runs nib, and Git, in\n\
			 # its library's folder, or in HERE, with the environment that its functions set. To run\n\
			 # it again by hand, empty the test's folder but for each device's passphrase file and\n\
			 # settings of Git; what came from output, such as a pairing code, differs in a new run.\n\
			 # Each job in a process group of its own, which killing it ends whole.\n\
			 set -m\n\
			 NIB={}\n",
			quoted(&shared.program)
		));
		Self(Rc::new(shared))
	}

	/// Text that no device may hold in plain, which every device is checked for as the test
	/// ends, as its passphrase is: such as a token in an address.
	pub fn secret(&self, text: &str) {
		self.0.secrets.borrow_mut().push(text.to_owned());
	}

	/// The test's folder, which holds its devices' folders, and what else the test puts
	/// there, such as a repository that stands for a server.
	#[must_use]
	pub fn folder(&self) -> &Path {
		&self.0.folder
	}

	/// A new device, with folders of its own: its library's, still empty, a home folder, a
	/// folder of NibKeep's state, a temporary folder, a passphrase file, which every command
	/// reads, and settings for Git.
	///
	/// # Panics
	///
	/// Panics if the folders cannot be made.
	#[must_use]
	pub fn device(&self, name: &str) -> Device {
		let root = self.0.folder.join(name);
		let folders = Folders {
			name: name.to_owned(),
			library: root.join("library"),
			home: root.join("home"),
			state: root.join("state"),
			temporary: root.join("temporary"),
			passphrase_file: root.join("passphrase"),
			passphrase: format!("The passphrase of {name} in {}", self.0.name),
			git_config: root.join("gitconfig"),
			ceiling: self.0.folder.clone(),
			root,
		};
		for folder in [
			&folders.library,
			&folders.home,
			&folders.state,
			&folders.temporary,
		] {
			fs::create_dir_all(folder).expect("makes the device's folders");
		}
		write_private(
			&folders.passphrase_file,
			&format!("{}\n", folders.passphrase),
		);
		// Git's settings of the device's own, never the person's: who commits, and the branch
		// that new repositories start on.
		let settings =
			"[user]\n\tname = Ada\n\temail = ada@example.invalid\n[init]\n\tdefaultBranch = main\n";
		fs::write(&folders.git_config, settings).expect("writes Git's settings");
		self.0
			.git_settings
			.borrow_mut()
			.insert(name.to_owned(), settings.to_owned());
		self.0.function(&folders, name, "\"$NIB\"");
		self.0.devices.borrow_mut().push(folders.clone());
		Device {
			shared: Rc::clone(&self.0),
			folders,
		}
	}
}

/// Variables as `env` takes them, each after a space: `PATH` as the shell that runs the
/// transcript has it, and of the others kept from the test's environment, none.
fn words<'a>(variables: impl Iterator<Item = (&'a OsStr, &'a OsStr)>) -> String {
	let mut words = String::new();
	for (name, value) in variables {
		if name == "PATH" {
			words.push_str(" PATH=\"$PATH\"");
		} else if !KEPT.iter().any(|kept| name == *kept) {
			let _ = write!(words, " {}={}", name.to_string_lossy(), quoted(value));
		}
	}
	words
}

impl Shared {
	/// Writes into the transcript a function that runs `program` on the device, with its
	/// environment. An assignment, where the shell takes the quotes of the default away, and
	/// splits nothing; `exec`, so that the program takes the subshell's place.
	fn function(&self, device: &Folders, function: &str, program: &str) {
		let variables = words(
			device
				.environment()
				.iter()
				.map(|(name, value)| (OsStr::new(name), value.as_os_str())),
		);
		self.write(&format!(
			"{function}() {{ (here=${{HERE:-{}}}; cd \"$here\" && exec env -i{variables} {program} \"$@\"); }}\n",
			quoted(&device.library),
		));
	}

	/// Adds to the transcript, `commands.sh` in the test's folder, as it goes, so that it
	/// holds what ran even if the test is killed.
	fn write(&self, text: &str) {
		OpenOptions::new()
			.create(true)
			.append(true)
			.open(self.transcript())
			.and_then(|mut file| file.write_all(text.as_bytes()))
			.expect("writes the transcript");
	}

	fn transcript(&self) -> PathBuf {
		self.folder.join("commands.sh")
	}

	/// The line of the transcript that runs `command` on `device`: through the device's
	/// function for nib or for Git, or else, or with an environment changed, in full.
	fn line(&self, device: &Folders, command: &Command) -> String {
		let here = command.get_current_dir().unwrap_or(&device.library);
		let (program, function) = if command.get_program() == self.program {
			("\"$NIB\"".to_owned(), Some(device.name.clone()))
		} else if command.get_program() == "git" {
			("git".to_owned(), Some(format!("{}_git", device.name)))
		} else {
			(quoted(command.get_program()), None)
		};
		let usual = device.environment();
		let unusual = command.get_envs().any(|(variable, value)| {
			!usual.iter().any(|(name, usual)| {
				OsStr::new(name) == variable && Some(usual.as_os_str()) == value
			})
		});
		// A command not made by the device, with the test's own environment.
		let apart = !usual.iter().all(|(name, _)| {
			command
				.get_envs()
				.any(|(variable, _)| variable == OsStr::new(name))
		});
		if apart {
			let mut arguments = String::new();
			for argument in command.get_args() {
				arguments.push(' ');
				arguments.push_str(&quoted(argument));
			}
			return format!(
				"(cd {} && exec {}{arguments})  # with the test's own environment",
				quoted(here),
				quoted(command.get_program())
			);
		}
		let mut arguments = String::new();
		for argument in command.get_args() {
			arguments.push(' ');
			arguments.push_str(&quoted(argument));
		}
		match function {
			Some(function) if !unusual && here == device.library => {
				format!("{function}{arguments}")
			}
			// In a subshell, as shells differ on whether the assignment outlasts the call.
			Some(function) if !unusual => {
				format!("(HERE={} {function}{arguments})", quoted(here))
			}
			_ => {
				let variables = command
					.get_envs()
					.filter_map(|(name, value)| value.map(|value| (name, value)));
				format!(
					"(cd {} && exec env -i{} {program}{arguments})",
					quoted(here),
					words(variables)
				)
			}
		}
	}

	/// Runs a command until it ends, killing it if it takes longer than a few minutes, with
	/// its line and how long it took in the transcript.
	fn run(&self, device: &Folders, command: Command) -> Result<Ran, Box<Ran>> {
		let line = self.line(device, &command);
		// The line goes first, so that the transcript has it even if the test is killed.
		self.write(&line);
		let started = Instant::now();
		let mut process = Process::start(command);
		let Some(status) = process.wait(PATIENCE) else {
			let status = process.kill();
			self.write("  # killed: it took too long\n");
			return Err(Box::new(self.ran(line, status, &process)));
		};
		let took = started.elapsed().as_secs_f64();
		self.write(&if status.success() {
			format!("  # {took:.1} s\n")
		} else {
			format!("  # {took:.1} s, ended with {status}\n")
		});
		Ok(self.ran(line, status, &process))
	}

	fn ran(&self, line: String, status: ExitStatus, process: &Process) -> Ran {
		Ran {
			line,
			status,
			stdout: process.stdout(),
			stderr: process.stderr(),
			redactions: self.redactions.clone(),
		}
	}

	/// The checks made as a test ends, with every command it started gone: each device's
	/// library is sound, as `nib verify` finds; its passphrase is nowhere in plain; and
	/// nothing was written into its home folder, which would have gone into the person's.
	fn check(&self) -> Vec<String> {
		let mut failures = Vec::new();
		self.write("# The checks as the test ends:\n");
		for device in self.devices.borrow().iter() {
			if device.library.join(".nib").exists() {
				match self.run(device, device.command(&self.program, &["verify"])) {
					Ok(ran) if ran.status.success() => {}
					Ok(ran) => {
						failures.push(format!("{}: nib verify failed:\n{ran}", device.name));
					}
					Err(ran) => {
						failures.push(format!("{}: nib verify took too long:\n{ran}", device.name));
					}
				}
			}
			let secrets = self.secrets.borrow();
			let kept = std::iter::once(("its passphrase", device.passphrase.as_str()))
				.chain(secrets.iter().map(|secret| ("a secret", secret.as_str())));
			for (what, secret) in kept {
				for folder in [device.library.join(".nib"), device.state.clone()] {
					if folder.exists()
						&& nibkeep_testkit::holds_anywhere(&folder, secret.as_bytes())
					{
						failures.push(format!(
							"{}: {what} is in {} in plain",
							device.name,
							folder.display()
						));
					}
				}
			}
			let settings = fs::read_to_string(&device.git_config).unwrap_or_default();
			if self.git_settings.borrow().get(&device.name) != Some(&settings) {
				failures.push(format!(
					"{}: nib changed its settings of Git, which are the person's:\n{settings}",
					device.name
				));
			}
			let written: Vec<OsString> = fs::read_dir(&device.home)
				.expect("lists the home folder")
				.map(|entry| entry.expect("an entry").file_name())
				.collect();
			if !written.is_empty() {
				failures.push(format!(
					"{}: nib wrote {written:?} into its home folder",
					device.name
				));
			}
		}
		failures
	}

	fn tell_where(&self) {
		eprintln!(
			"The end-to-end test \"{}\" failed. Its devices' folders are in {}, and what it ran \
			 is in {}.",
			self.name,
			self.folder.display(),
			self.transcript().display()
		);
	}
}

impl Drop for Shared {
	fn drop(&mut self) {
		if std::thread::panicking() {
			self.tell_where();
			return;
		}
		let failures = self.check();
		if !failures.is_empty() {
			self.tell_where();
			panic!("as the test ended:\n{}", failures.join("\n"));
		}
	}
}

/// A device's folders, and its passphrase.
#[derive(Clone)]
struct Folders {
	name: String,
	/// The folder that holds the others.
	root: PathBuf,
	/// The library's folder, where its commands run.
	library: PathBuf,
	home: PathBuf,
	state: PathBuf,
	temporary: PathBuf,
	passphrase_file: PathBuf,
	passphrase: String,
	/// Git's settings for the device, in place of the person's.
	git_config: PathBuf,
	/// The test's folder, in which and above which the program looks for no library.
	ceiling: PathBuf,
}

impl Folders {
	/// The environment that the device's commands run with, and no other: the folders of the
	/// person's and the computer's that the program would use are the device's own; time,
	/// languages and the port are fixed; and the program keeps from the keystore, the login
	/// entry, and libraries outside the test's folder.
	///
	/// On Windows, the program finds the person's known folders, such as Downloads, through
	/// the system, which reads none of these variables.
	fn environment(&self) -> Vec<(&'static str, OsString)> {
		let mut variables: Vec<(&'static str, OsString)> = KEPT
			.iter()
			.filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
			.collect();
		let home = |path: &str| self.home.join(path).into_os_string();
		variables.extend([
			("HOME", self.home.clone().into_os_string()),
			("USERPROFILE", self.home.clone().into_os_string()),
			("APPDATA", home("AppData/Roaming")),
			("LOCALAPPDATA", home("AppData/Local")),
			("XDG_CONFIG_HOME", home(".config")),
			("XDG_DATA_HOME", home(".local/share")),
			("XDG_STATE_HOME", home(".local/state")),
			("XDG_CACHE_HOME", home(".cache")),
			("TMPDIR", self.temporary.clone().into_os_string()),
			("TEMP", self.temporary.clone().into_os_string()),
			("TMP", self.temporary.clone().into_os_string()),
			("TZ", "UTC".into()),
			("LANG", "C.UTF-8".into()),
			("LANGUAGE", "en-US".into()),
			// No session bus, so no Secret Service: a second fence around the keystore on
			// Linux, besides NIBKEEP_NO_KEYSTORE.
			(
				"DBUS_SESSION_BUS_ADDRESS",
				dbus_address(&self.root.join("no-bus")),
			),
			(
				"GIT_CONFIG_GLOBAL",
				self.git_config.clone().into_os_string(),
			),
			// Git never climbs out of the test, such as into the checkout that holds it.
			(
				"GIT_CEILING_DIRECTORIES",
				self.ceiling.clone().into_os_string(),
			),
			("GIT_CONFIG_NOSYSTEM", "1".into()),
			("GIT_TERMINAL_PROMPT", "0".into()),
			("NIBKEEP_NO_KEYSTORE", "1".into()),
			("NIBKEEP_NO_LOGIN_ENTRY", "1".into()),
			("NIBKEEP_STATE", self.state.clone().into_os_string()),
			(
				"NIBKEEP_PASSPHRASE_FILE",
				self.passphrase_file.clone().into_os_string(),
			),
			("NIBKEEP_CEILING", self.ceiling.clone().into_os_string()),
			// Any free port, so that tests run side by side.
			("NIBKEEP_PORT", "0".into()),
		]);
		variables
	}

	/// The program with these arguments, in the library's folder, with the device's
	/// environment.
	fn command(&self, program: &Path, arguments: &[&str]) -> Command {
		let mut command = Command::new(program);
		command
			.args(arguments)
			.current_dir(&self.library)
			.env_clear()
			.envs(self.environment());
		command
	}
}

/// A D-Bus address of a socket at `path`, with what D-Bus addresses do not take as it is
/// escaped, such as the spaces and comma of a test's folder.
fn dbus_address(path: &Path) -> OsString {
	let mut address = String::from("unix:path=");
	for byte in path.to_string_lossy().bytes() {
		if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) {
			address.push(char::from(byte));
		} else {
			let _ = write!(address, "%{byte:02x}");
		}
	}
	address.into()
}

/// A device of a test: a computer of the person's, with nib, as far as the program can tell.
pub struct Device {
	shared: Rc<Shared>,
	folders: Folders,
}

impl Device {
	/// Runs nib with these arguments in the library's folder, until it ends.
	///
	/// # Panics
	///
	/// Panics if it takes longer than a few minutes, after it is killed.
	#[track_caller]
	#[must_use]
	pub fn nib(&self, arguments: &[&str]) -> Ran {
		self.run(self.command(arguments))
	}

	/// The command that runs nib with these arguments as the device runs it, to change before
	/// [`Device::run`] runs it, such as to run it in another folder.
	#[must_use]
	pub fn command(&self, arguments: &[&str]) -> Command {
		self.folders.command(&self.shared.program, arguments)
	}

	/// Runs a command of [`Device::command`] until it ends.
	///
	/// # Panics
	///
	/// Panics if it takes longer than a few minutes, after it is killed.
	#[track_caller]
	#[must_use]
	pub fn run(&self, command: Command) -> Ran {
		self.shared
			.run(&self.folders, command)
			.unwrap_or_else(|ran| panic!("it took longer than {PATIENCE:?} and was killed:\n{ran}"))
	}

	/// Starts nib with these arguments in the library's folder, in the background, until it
	/// ends or the test ends it. Its standard input is empty.
	#[must_use]
	pub fn start(&self, arguments: &[&str]) -> Running {
		self.start_reading(arguments, false)
	}

	/// Starts nib as [`Device::start`] does, but with standard input that the test writes to,
	/// with [`Running::send`], until it closes it, with [`Running::close_input`]; as a
	/// client of `nib mcp` does.
	#[must_use]
	pub fn start_with_input(&self, arguments: &[&str]) -> Running {
		self.start_reading(arguments, true)
	}

	/// Starts nib with these arguments in the background at a terminal of its own, as a
	/// person runs it, until it ends or the test ends it: nib asks there, as for a
	/// passphrase or a recovery code, which the test types with [`Running::answer`] and
	/// [`Running::answer_unseen`]. The terminal is its standard input and error too, so
	/// that nib asks what it asks only a person, and [`Ran::stderr`] holds what the terminal
	/// showed; its standard output stays apart, as for every command.
	///
	/// Not on Windows, where std starts no program at a pseudo-console.
	#[cfg(unix)]
	#[must_use]
	pub fn start_at_terminal(&self, arguments: &[&str]) -> Running {
		let command = self.command(arguments);
		let line = self.shared.line(&self.folders, &command);
		let job = self.shared.jobs.get() + 1;
		self.shared.jobs.set(job);
		// In the foreground, where a person who runs the transcript again types the answers.
		self.shared
			.write(&format!("{line}  # job{job}, at a terminal of its own\n"));
		Running {
			process: Process::start_at_terminal(command),
			shared: Rc::clone(&self.shared),
			line,
			job,
			at_terminal: true,
			answered: 0,
		}
	}

	fn start_reading(&self, arguments: &[&str], input: bool) -> Running {
		let command = self.command(arguments);
		let line = self.shared.line(&self.folders, &command);
		let job = self.shared.jobs.get() + 1;
		self.shared.jobs.set(job);
		// The transcript feeds the command from a pipe of its own in the test's folder, which
		// it opens for writing, as a descriptor of its own, once the command has started, so
		// that the command holds no writer of its own input.
		if input {
			let pipe = quoted(self.shared.folder.join(format!("input{job}")));
			self.shared.write(&format!(
				"rm -f {pipe}; mkfifo {pipe}; {line} <{pipe} & job{job}=$!; exec {}>{pipe}\n",
				input_descriptor(job)
			));
		} else {
			self.shared.write(&format!("{line} & job{job}=$!\n"));
		}
		Running {
			process: Process::start_reading(command, input),
			shared: Rc::clone(&self.shared),
			line,
			job,
			at_terminal: false,
			#[cfg(unix)]
			answered: 0,
		}
	}

	/// Runs the device's Git with these arguments in `here`, until it ends, with the device's
	/// environment and Git's settings.
	///
	/// # Panics
	///
	/// Panics if it takes longer than a few minutes, after it is killed.
	#[track_caller]
	#[must_use]
	pub fn git(&self, here: &Path, arguments: &[&str]) -> Ran {
		let name = &self.folders.name;
		if self.shared.git_functions.borrow_mut().insert(name.clone()) {
			self.shared
				.function(&self.folders, &format!("{name}_git"), "git");
		}
		let mut command = self.folders.command(Path::new("git"), arguments);
		command.current_dir(here);
		self.run(command)
	}

	/// Adds to the device's settings of Git, such as `url.….insteadOf` to reach a repository
	/// on the disk at an address of the network.
	///
	/// # Panics
	///
	/// Panics if the settings cannot be written.
	pub fn git_config(&self, settings: &str) {
		OpenOptions::new()
			.append(true)
			.open(&self.folders.git_config)
			.and_then(|mut file| file.write_all(settings.as_bytes()))
			.expect("writes Git's settings");
		if let Some(written) = self
			.shared
			.git_settings
			.borrow_mut()
			.get_mut(&self.folders.name)
		{
			written.push_str(settings);
		}
	}

	/// The library's folder, where the device's commands run.
	#[must_use]
	pub fn library(&self) -> &Path {
		&self.folders.library
	}

	/// The folder that holds the device's own.
	#[must_use]
	pub fn folder(&self) -> &Path {
		&self.folders.root
	}
}

/// A command that runs in the background, as `nib device pair` does while it waits, until it
/// ends, or the test ends it. Dropped, it is killed.
pub struct Running {
	// Before the test's own, so that the command is gone before the test's last checks.
	process: Process,
	shared: Rc<Shared>,
	line: String,
	job: usize,
	/// Whether it runs at a terminal of its own, in the transcript's foreground.
	at_terminal: bool,
	/// How many bytes the terminal showed before the last answer typed there.
	#[cfg(unix)]
	answered: usize,
}

impl Running {
	/// Waits until what it printed on its standard output matches `pattern`, and returns the
	/// first match.
	///
	/// # Panics
	///
	/// Panics if the pattern is not a regular expression, or as [`Running::wait_until`] does.
	#[track_caller]
	pub fn wait_for(&mut self, pattern: &str) -> String {
		let regex = Regex::new(pattern).expect("a pattern");
		let found = self.wait_until(pattern, |printed| {
			regex.find(printed).map(|found| found.as_str().to_owned())
		});
		self.shared
			.write(&format!("# job{} printed {found}\n", self.job));
		found
	}

	/// Waits until `found` finds what it looks for, `what`, in what the command printed on
	/// its standard output.
	///
	/// # Panics
	///
	/// Panics if the command ends, or takes longer than a few minutes, before.
	#[track_caller]
	pub fn wait_until<T>(&mut self, what: &str, found: impl Fn(&str) -> Option<T>) -> T {
		self.poll(what, |process| found(&process.stdout()))
	}

	/// Waits until `found` finds what it looks for, `what`, in the command as it runs.
	#[track_caller]
	fn poll<T>(&mut self, what: &str, found: impl Fn(&Process) -> Option<T>) -> T {
		let started = Instant::now();
		loop {
			// Whether it ended is asked first: then what it printed has all been read.
			let ended = self.process.ended();
			if let Some(found) = found(&self.process) {
				return found;
			}
			if ended.is_some() || started.elapsed() >= PATIENCE {
				self.fail_before(what, ended);
			}
			std::thread::sleep(Duration::from_millis(5));
		}
	}

	/// Sends a line to its standard input.
	///
	/// # Panics
	///
	/// Panics if it ended, or its input was closed, or it runs at a terminal, where it is
	/// answered instead.
	#[track_caller]
	pub fn send(&mut self, line: &str) {
		assert!(
			!self.at_terminal,
			"job{} reads from its terminal: answer it there",
			self.job
		);
		self.shared.write(&format!(
			"printf '%s\\n' {} >&{}\n",
			quoted(line),
			input_descriptor(self.job)
		));
		assert!(
			self.process.write(&format!("{line}\n")),
			"job{} takes no more input: it ended, or its input was closed",
			self.job
		);
	}

	/// Waits until its terminal asks `prompt`, after what it showed before the last answer,
	/// and types `text` and Enter there, which the terminal shows, as for a question that
	/// nib asks.
	///
	/// # Panics
	///
	/// Panics if it ends, or takes longer than a few minutes, before it asks, or has no
	/// terminal.
	#[cfg(unix)]
	#[track_caller]
	pub fn answer(&mut self, prompt: &str, text: &str) {
		self.type_after(prompt, text, false);
	}

	/// Waits until its terminal asks `prompt`, as [`Running::answer`] does, and then until
	/// it shows nothing typed, as when nib reads a passphrase or a recovery code; then types
	/// `text` and Enter, which the terminal does not show.
	///
	/// # Panics
	///
	/// Panics as [`Running::answer`] does.
	#[cfg(unix)]
	#[track_caller]
	pub fn answer_unseen(&mut self, prompt: &str, text: &str) {
		self.type_after(prompt, text, true);
	}

	#[cfg(unix)]
	#[track_caller]
	fn type_after(&mut self, prompt: &str, text: &str, unseen: bool) {
		assert!(self.at_terminal, "job{} has no terminal", self.job);
		let answered = self.answered;
		let what = format!("the prompt {prompt:?}");
		let shown = self.poll(&what, |process| {
			let (since, shown) = process.shown_since(answered);
			since.contains(prompt).then_some(shown)
		});
		// A program shows its prompt before it hides what is typed, which it shows again once
		// it has read it.
		if unseen {
			let what = format!("{what}, with what is typed unseen");
			self.poll(&what, |process| {
				(process.terminal_echoes() == Some(false)).then_some(())
			});
		}
		self.shared.write(&format!(
			"# job{} asked {prompt:?}; typed {}\n",
			self.job,
			quoted(text)
		));
		self.answered = shown;
		assert!(
			self.process.type_at_terminal(&format!("{text}\r")),
			"job{}'s terminal takes no more input",
			self.job
		);
	}

	/// Fails the test, as the command ended, or took too long, before it printed or showed
	/// `what`; killed, in the second case.
	#[track_caller]
	fn fail_before(&mut self, what: &str, ended: Option<ExitStatus>) -> ! {
		let status = match ended {
			Some(status) => {
				self.note(&format!("ended before it printed {what}"));
				status
			}
			None => self.kill_noting(&format!("killed: it did not print {what} in time")),
		};
		let ran = self.shared.ran(self.line.clone(), status, &self.process);
		panic!("it ended, or took too long, before it printed {what}:\n{ran}");
	}

	/// Interrupts it, as Ctrl-C on a terminal does, with what it started: on Unix, where the
	/// tests interrupt commands, which Windows gives no other program's console.
	#[cfg(unix)]
	pub fn interrupt(&mut self) {
		self.shared.write(&if self.at_terminal {
			format!("# job{} interrupted, as with Ctrl-C\n", self.job)
		} else {
			format!("kill -INT -- -\"$job{}\"\n", self.job)
		});
		self.process.interrupt();
	}

	/// Closes its standard input, which it then reads to the end.
	///
	/// # Panics
	///
	/// Panics if it runs at a terminal, whose input it keeps.
	pub fn close_input(&mut self) {
		assert!(
			!self.at_terminal,
			"job{} reads from its terminal, which stays open",
			self.job
		);
		self.shared
			.write(&format!("exec {}>&-\n", input_descriptor(self.job)));
		self.process.close_input();
	}

	/// Waits until it ends by itself.
	///
	/// # Panics
	///
	/// Panics if it takes longer than a few minutes, after it is killed.
	#[track_caller]
	#[must_use]
	pub fn finish(mut self) -> Ran {
		let Some(status) = self.process.wait(PATIENCE) else {
			let status = self.kill_noting("killed: it took too long");
			let ran = self.shared.ran(self.line.clone(), status, &self.process);
			panic!("it took longer than {PATIENCE:?} and was killed:\n{ran}");
		};
		self.shared.write(&if self.at_terminal {
			format!("# job{} ended with {status}\n", self.job)
		} else {
			format!("wait \"$job{}\"  # ended with {status}\n", self.job)
		});
		self.shared.ran(self.line.clone(), status, &self.process)
	}

	/// Kills it at once, with what it started, as a crash or the system's task manager ends a
	/// program. It must still be running.
	///
	/// # Panics
	///
	/// Panics if it ended before.
	#[track_caller]
	#[must_use = "what it printed until it was killed"]
	pub fn kill(mut self) -> Ran {
		if let Some(status) = self.process.ended() {
			let ran = self.shared.ran(self.line.clone(), status, &self.process);
			panic!("it ended before it was to be killed:\n{ran}");
		}
		let status = self.kill_noting("killed at once");
		self.shared.ran(self.line.clone(), status, &self.process)
	}

	/// Kills it, saying why in the transcript.
	fn kill_noting(&mut self, why: &str) -> ExitStatus {
		self.shared.write(&if self.at_terminal {
			format!("# job{} {why}\n", self.job)
		} else {
			format!("kill -KILL -- -\"$job{}\"  # {why}\n", self.job)
		});
		let status = self.process.kill();
		// What it printed until then, for a failure to be read from the transcript.
		let mut printed = String::new();
		for (stream, text) in [
			("output", self.process.stdout()),
			("error", self.process.stderr()),
		] {
			for line in text.lines() {
				let _ = writeln!(printed, "#   job{} {stream}: {line}", self.job);
			}
		}
		self.shared.write(&printed);
		status
	}

	/// Says in the transcript what became of it.
	fn note(&self, what: &str) {
		self.shared.write(&format!("# job{} {what}\n", self.job));
	}
}

impl Drop for Running {
	fn drop(&mut self) {
		if self.process.ended().is_none() {
			self.kill_noting("killed as the test let it go");
		}
	}
}

/// The descriptor through which the transcript writes to the input of job `job`: one of the
/// seven above standard error that every shell takes, as a test feeds one command at a time.
fn input_descriptor(job: usize) -> usize {
	3 + job % 7
}

/// What a command did: how it ended, and what it printed.
pub struct Ran {
	line: String,
	/// How it ended.
	pub status: ExitStatus,
	/// What it printed on its standard output.
	pub stdout: String,
	/// What it printed on its standard error, or for a command at a terminal, what the
	/// terminal showed.
	pub stderr: String,
	redactions: Redactions,
}

// Each check returns what it checked, for more checks, or for nothing: the check is its effect.
#[allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]
impl Ran {
	/// Checks that it succeeded.
	///
	/// # Panics
	///
	/// Panics if it did not.
	#[track_caller]
	pub fn ok(self) -> Self {
		assert!(self.status.success(), "it failed:\n{self}");
		self
	}

	/// Checks that it failed, as `nib` does with an error: with exit status 1.
	///
	/// # Panics
	///
	/// Panics if it did not.
	#[track_caller]
	pub fn fails(self) -> Self {
		self.fails_with(1)
	}

	/// Checks that it ended with this exit status.
	///
	/// # Panics
	///
	/// Panics if it did not.
	#[track_caller]
	pub fn fails_with(self, code: i32) -> Self {
		assert_eq!(
			self.status.code(),
			Some(code),
			"another exit status:\n{self}"
		);
		self
	}

	/// Checks what it printed on its standard output, with what changes from run to run
	/// redacted; `SNAPSHOTS=overwrite` writes what it printed into the test instead.
	///
	/// # Panics
	///
	/// Panics if it printed something else.
	#[track_caller]
	pub fn stdout_eq(self, expected: impl IntoData) -> Self {
		self.assert().eq(self.stdout.as_str(), expected);
		self
	}

	/// Checks what it printed on its standard error, as [`Ran::stdout_eq`] does.
	///
	/// # Panics
	///
	/// Panics if it printed something else.
	#[track_caller]
	pub fn stderr_eq(self, expected: impl IntoData) -> Self {
		self.assert().eq(self.stderr.as_str(), expected);
		self
	}

	/// What it printed on its standard output, read as JSON.
	///
	/// # Panics
	///
	/// Panics if that is not JSON.
	#[track_caller]
	#[must_use]
	pub fn json(&self) -> serde_json::Value {
		serde_json::from_str(&self.stdout)
			.unwrap_or_else(|error| panic!("not JSON ({error}):\n{self}"))
	}

	fn assert(&self) -> Assert {
		Assert::new()
			.action_env(snapbox::assert::DEFAULT_ACTION_ENV)
			.redact_with(self.redactions.clone())
	}
}

impl std::fmt::Display for Ran {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			formatter,
			"{}\nended with {}\n--- standard output ---\n{}--- standard error ---\n{}",
			self.line, self.status, self.stdout, self.stderr
		)
	}
}

/// Text as the shell reads it: as it is, if the shell would take it so, or else in single
/// quotes.
fn quoted(text: impl AsRef<OsStr>) -> String {
	let text = text.as_ref().to_string_lossy();
	let plain = |c: char| c.is_ascii_alphanumeric() || "_-./=:,@%+".contains(c);
	if !text.is_empty() && text.chars().all(plain) {
		text.into_owned()
	} else {
		format!("'{}'", text.replace('\'', r"'\''"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn text_is_quoted_for_the_shell_only_where_it_must_be() {
		assert_eq!(quoted("init"), "init");
		assert_eq!(quoted("name=Grace"), "name=Grace");
		assert_eq!(quoted("name=Grace Hopper"), "'name=Grace Hopper'");
		assert_eq!(quoted("Ada's"), r"'Ada'\''s'");
		assert_eq!(quoted(""), "''");
	}

	#[test]
	fn a_bus_address_escapes_what_d_bus_does_not_take() {
		assert_eq!(
			dbus_address(Path::new("/tmp/end to end, keystore/no-bus")),
			"unix:path=/tmp/end%20to%20end%2c%20keystore/no-bus"
		);
	}
}
