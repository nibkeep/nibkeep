//! Helpers shared by the tests: an environment for `nib` with fixed randomness and time.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::Duration;

use clap::Parser;
use nibkeep_cli::{Cli, CliError, Environment};
use nibkeep_node::PassphraseParams;
use nibkeep_passwords::{Board, ClipboardError};
use nibkeep_testkit::{TestRng, millis_at};
use zeroize::Zeroizing;

/// An environment for `nib` in tests.
pub(crate) struct Test {
	pub(crate) rng: TestRng,
	pub(crate) now: u64,
	/// The answers to passphrase prompts, in order.
	pub(crate) passphrases: VecDeque<&'static str>,
	/// How many passphrases were asked for.
	pub(crate) asked: usize,
	/// How long typing each passphrase takes.
	pub(crate) typing: Duration,
	/// The current folder, where `nib init` starts a library.
	pub(crate) here: PathBuf,
	/// The library that `NIBKEEP_LIBRARY` names.
	pub(crate) default_library: Option<PathBuf>,
	/// The person's account, which names a new library's user.
	pub(crate) account: nibkeep_cli::Account,
	/// The folder that holds the test's own, which commands look for no library in.
	pub(crate) ceiling: Option<PathBuf>,
	pub(crate) keystore: bool,
	/// The system's languages, as the platform names them.
	pub(crate) languages: Vec<String>,
	/// The downloads folder, if the platform names one.
	pub(crate) downloads: Option<PathBuf>,
	/// The answers to questions on the terminal, in order, or `None` for no terminal.
	pub(crate) answers: Option<VecDeque<&'static str>>,
	/// Whether what `nib` prints goes to a terminal that shows colors.
	pub(crate) colors: bool,
	/// The questions asked on the terminal.
	pub(crate) questions: Vec<String>,
	/// The clipboard, if the environment has one.
	pub(crate) clipboard: Option<Rc<RefCell<Clip>>>,
	/// How long each wait was to last.
	pub(crate) waited: Vec<Duration>,
	/// What the clipboard held as each wait began, and whether it was marked as sensitive.
	pub(crate) held: Vec<(Option<String>, bool)>,
	/// What the person copies while `nib` waits, if anything.
	pub(crate) copied_meanwhile: Option<&'static str>,
	/// Whether the person interrupts `nib` while it waits.
	pub(crate) interrupts: bool,
	/// What `nib` told the person beside its output.
	pub(crate) notes: Vec<String>,
	/// This computer's folder of NibKeep's state, if it has one.
	pub(crate) state: Option<PathBuf>,
	/// The copy of `nibsync` beside `nib`, if there is one.
	pub(crate) sync_program: Option<PathBuf>,
	/// The engine's versions that copies of `nibsync` tell, by their paths.
	pub(crate) sync_versions: Vec<(PathBuf, String)>,
	/// The copies of `nibsync` that `nib` started.
	pub(crate) started: Vec<PathBuf>,
	/// The file of the login entry, an autostart entry, if the platform has one.
	pub(crate) login_entry: Option<PathBuf>,
	/// What opening the firewall comes to.
	pub(crate) firewall: nibkeep_cli::Firewall,
	/// The home of the test's own Git settings, never the computer's.
	pub(crate) git_home: PathBuf,
	/// The programs of AI clients installed, by name, such as a script in place of `claude`:
	/// never the computer's own.
	pub(crate) ai_clients: Vec<(&'static str, PathBuf)>,
}

/// What a clipboard holds, as the platform's would.
#[derive(Debug, Default)]
pub(crate) struct Clip {
	/// The text on it.
	pub(crate) text: Option<String>,
	/// Whether it is marked as sensitive.
	pub(crate) sensitive: bool,
	/// Whether `nib` takes interruptions, rather than ending.
	pub(crate) interruptible: bool,
	/// For each copy, whether `nib` took interruptions already.
	pub(crate) copied_interruptible: Vec<bool>,
}

/// The test's clipboard, shared with the test.
struct Shared(Rc<RefCell<Clip>>);

impl Board for Shared {
	fn copy(&mut self, text: &str, sensitive: bool) -> Result<(), ClipboardError> {
		let mut clip = self.0.borrow_mut();
		clip.text = Some(text.to_owned());
		clip.sensitive = sensitive;
		let interruptible = clip.interruptible;
		clip.copied_interruptible.push(interruptible);
		Ok(())
	}

	fn text(&mut self) -> Result<Option<Zeroizing<String>>, ClipboardError> {
		Ok(self.0.borrow().text.clone().map(Zeroizing::new))
	}

	fn clear(&mut self) -> Result<(), ClipboardError> {
		let mut clip = self.0.borrow_mut();
		clip.text = None;
		clip.sensitive = false;
		Ok(())
	}
}

impl Test {
	/// An environment in an empty folder of its own, and no keystore.
	pub(crate) fn new(name: &str) -> Self {
		let here = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		let ceiling = here.parent().map(PathBuf::from);
		Self {
			rng: TestRng::new(name),
			now: millis_at("2026-10-12", 10, 0),
			passphrases: VecDeque::new(),
			asked: 0,
			typing: Duration::ZERO,
			here,
			default_library: None,
			account: nibkeep_cli::Account {
				name: Some("ada".to_owned()),
				full_name: Some("Ada Lovelace".to_owned()),
			},
			ceiling,
			keystore: false,
			languages: Vec::new(),
			downloads: None,
			answers: None,
			colors: false,
			questions: Vec::new(),
			clipboard: Some(Rc::default()),
			waited: Vec::new(),
			held: Vec::new(),
			copied_meanwhile: None,
			interrupts: false,
			notes: Vec::new(),
			state: None,
			sync_program: None,
			sync_versions: Vec::new(),
			started: Vec::new(),
			login_entry: None,
			firewall: nibkeep_cli::Firewall::NotHere,
			git_home: git_home(name),
			ai_clients: Vec::new(),
		}
	}

	/// Git, with the test's own settings.
	pub(crate) fn git_command(&self) -> std::process::Command {
		let mut git = std::process::Command::new("git");
		nibkeep_cli::clear_git_variables(&mut git);
		git.env("GIT_CONFIG_GLOBAL", self.git_home.join("config"))
			.env("GIT_CONFIG_NOSYSTEM", "1")
			.env("HOME", &self.git_home)
			.env("XDG_CONFIG_HOME", &self.git_home)
			.env("GIT_TERMINAL_PROMPT", "0");
		git
	}

	/// What the clipboard holds.
	pub(crate) fn clip(&self) -> std::cell::Ref<'_, Clip> {
		self.clipboard.as_ref().expect("a clipboard").borrow()
	}

	/// The same, with the mock keystore.
	pub(crate) fn with_keystore(name: &str) -> Self {
		mock_keystore();
		Self {
			keystore: true,
			..Self::new(name)
		}
	}

	/// Queues answers to passphrase prompts.
	pub(crate) fn answer(&mut self, passphrases: &[&'static str]) {
		self.passphrases.extend(passphrases);
	}

	/// Runs `nib` with these arguments, and returns what it printed.
	pub(crate) fn nib(&mut self, arguments: &[&str]) -> Result<String, CliError> {
		let cli = Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
			.expect("valid arguments");
		let mut output = Vec::new();
		nibkeep_cli::run(&cli, self, &mut output)?;
		Ok(String::from_utf8(output).expect("UTF-8"))
	}

	/// Runs `nib` with these arguments, and returns what it printed, and whether it failed.
	pub(crate) fn nib_printing(&mut self, arguments: &[&str]) -> (String, Result<(), CliError>) {
		let cli = Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
			.expect("valid arguments");
		let mut output = Vec::new();
		let result = nibkeep_cli::run(&cli, self, &mut output);
		(String::from_utf8(output).expect("UTF-8"), result)
	}

	/// Runs `nib` with `--json` and these arguments, and returns the JSON it printed.
	pub(crate) fn json(&mut self, arguments: &[&str]) -> serde_json::Value {
		let arguments: Vec<&str> = std::iter::once("--json")
			.chain(arguments.iter().copied())
			.collect();
		let output = self.nib(&arguments).expect("runs");
		serde_json::from_str(&output).expect("JSON")
	}
}

impl Environment for Test {
	type Rng = TestRng;

	fn git(&self) -> std::process::Command {
		self.git_command()
	}

	fn rng(&mut self) -> &mut TestRng {
		&mut self.rng
	}

	fn now(&self) -> u64 {
		self.now
	}

	fn offset_minutes(&self) -> i32 {
		0
	}

	fn passphrase(&mut self, _prompt: &str) -> io::Result<Zeroizing<String>> {
		self.asked += 1;
		std::thread::sleep(self.typing);
		self.passphrases
			.pop_front()
			.map(|passphrase| Zeroizing::new(passphrase.to_owned()))
			.ok_or_else(|| io::Error::other("no passphrase was given"))
	}

	fn passphrase_params(&self) -> PassphraseParams {
		PassphraseParams::new(64, 1, 1).expect("valid")
	}

	fn computer_name(&self) -> String {
		"laptop".to_owned()
	}

	fn account(&self) -> nibkeep_cli::Account {
		self.account.clone()
	}

	fn current_dir(&self) -> io::Result<PathBuf> {
		Ok(self.here.clone())
	}

	fn default_library(&self) -> Option<PathBuf> {
		self.default_library.clone()
	}

	fn ceiling(&self) -> Option<PathBuf> {
		self.ceiling.clone()
	}

	fn keystore(&mut self) -> bool {
		self.keystore
	}

	fn languages(&self) -> Vec<String> {
		self.languages.clone()
	}

	fn downloads(&self) -> Option<PathBuf> {
		self.downloads.clone()
	}

	fn note(&mut self, line: &str) {
		self.notes.push(line.to_owned());
	}

	fn can_ask(&self) -> bool {
		self.answers.is_some()
	}

	fn colors(&self) -> bool {
		self.colors
	}

	fn ask(&mut self, question: &str) -> io::Result<Option<String>> {
		let Some(answers) = &mut self.answers else {
			return Ok(None);
		};
		self.questions.push(question.to_owned());
		Ok(answers.pop_front().map(str::to_owned))
	}

	fn clipboard(&mut self) -> Result<Box<dyn Board>, ClipboardError> {
		match &self.clipboard {
			Some(clip) => Ok(Box::new(Shared(Rc::clone(clip)))),
			None => Err(ClipboardError::new("no display")),
		}
	}

	fn state_folder(&self) -> Option<PathBuf> {
		self.state.clone()
	}

	fn sync_program(&self) -> Option<PathBuf> {
		self.sync_program.clone()
	}

	fn nib_program(&self) -> Option<PathBuf> {
		Some(PathBuf::from("/opt/nib/bin/nib"))
	}

	fn ai_client(&self, program: &str) -> Option<std::process::Command> {
		self.ai_clients
			.iter()
			.find(|(name, _)| *name == program)
			.map(|(_, path)| std::process::Command::new(path))
	}

	fn login_entry(&self) -> Option<nibkeep_node::at_login::LoginEntry> {
		self.login_entry
			.clone()
			.map(nibkeep_node::at_login::LoginEntry::autostart)
	}

	fn open_firewall(&mut self) -> nibkeep_cli::Firewall {
		self.firewall
	}

	fn sync_version(&self, program: &std::path::Path) -> Option<String> {
		self.sync_versions
			.iter()
			.find(|(path, _)| path == program)
			.map(|(_, version)| version.clone())
	}

	fn start_sync(&mut self, program: &std::path::Path) -> Result<(), nibkeep_node::NodeError> {
		self.started.push(program.to_owned());
		Ok(())
	}

	fn interruptible(&mut self) {
		if let Some(clip) = &self.clipboard {
			clip.borrow_mut().interruptible = true;
		}
	}

	fn wait(&mut self, duration: Duration) -> bool {
		self.waited.push(duration);
		if let Some(clip) = &self.clipboard {
			let clip = clip.borrow();
			self.held.push((clip.text.clone(), clip.sensitive));
		}
		if let (Some(text), Some(clip)) = (self.copied_meanwhile, &self.clipboard) {
			let mut clip = clip.borrow_mut();
			clip.text = Some(text.to_owned());
			clip.sensitive = false;
		}
		self.interrupts
	}
}

/// Uses the mock keystore, once for all tests of a file.
fn mock_keystore() {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| {
		let store: Arc<keyring_core::CredentialStore> =
			keyring_core::mock::Store::new().expect("a mock store");
		keyring_core::set_default_store(store);
	});
}

/// The output with every ID, and every short ID that starts a line, replaced by `<id>`, so
/// that tests can compare it as text.
pub(crate) fn ids_hidden(output: &str) -> String {
	let is_id = |token: &str| token.parse::<nibkeep_base::EntityId>().is_ok();
	let is_short = |token: &str| token.len() == 8 && token.chars().all(|c| c.is_ascii_hexdigit());
	output
		.lines()
		.map(|line| {
			let mut hidden = String::new();
			let mut rest = line;
			while !rest.is_empty() {
				let spaces = rest.len() - rest.trim_start().len();
				hidden.push_str(&rest[..spaces]);
				rest = &rest[spaces..];
				let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
				let token = &rest[..token_end];
				let at_start = hidden.trim().is_empty();
				if is_id(token) || (at_start && is_short(token)) {
					hidden.push_str("<id>");
				} else {
					hidden.push_str(token);
				}
				rest = &rest[token_end..];
			}
			hidden + "\n"
		})
		.collect()
}

/// A library to work in, unlocked by the mock keystore.
pub(crate) fn library(name: &str) -> Test {
	let mut test = Test::with_keystore(name);
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	test
}

/// How long a test waits for the other side over localhost.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// Pairs a new library in `directory`, on a device named `device`, with `sponsor`, over
/// connections on localhost, its keys protected as `protection` says: a replica of the
/// user's, or with `ai_agent`, of the AI agent of that name. Both make their changes at the
/// time `at`.
pub(crate) fn pair_on_localhost(
	sponsor: &mut nibkeep_node::Library,
	directory: &std::path::Path,
	(device, ai_agent): (&str, Option<&str>),
	protection: &nibkeep_node::NewProtection<'_>,
	at: u64,
	rng: &mut TestRng,
) {
	pair_device_on_localhost(
		sponsor,
		directory,
		(device, ai_agent, &nibkeep_node::NewDevice::computer()),
		protection,
		at,
		rng,
	);
}

/// Pairs a new library as [`pair_on_localhost`] does; a replica of the user's as the person
/// chose on the sponsor, `new_device`: a computer or a phone, with the spaces chosen.
pub(crate) fn pair_device_on_localhost(
	sponsor: &mut nibkeep_node::Library,
	directory: &std::path::Path,
	(device, ai_agent, new_device): (&str, Option<&str>, &nibkeep_node::NewDevice),
	protection: &nibkeep_node::NewProtection<'_>,
	at: u64,
	rng: &mut TestRng,
) {
	use nibkeep_node::{Joining, NetworkOptions, PairingCode};
	let code = PairingCode::generate(rng);
	let sponsor_network = sponsor.network(NetworkOptions::LOCAL).expect("starts");
	let joining = Joining::new(device, "nib", rng).expect("names");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		sponsor_network.endpoint(),
		&sponsor_network.local_addresses(),
		Some(&code.announcement()),
	);
	let now = move || at;
	std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			// Each pairing draws apart.
			let mut sponsor_rng = TestRng::new(&format!("the sponsor of {}", directory.display()));
			match ai_agent {
				Some(name) => nibkeep_node::pair_ai_agent(
					sponsor,
					&sponsor_network,
					&code,
					name,
					nibkeep_node::Showing::new(TIMEOUT),
					&now,
					&mut sponsor_rng,
				),
				None => nibkeep_node::pair_device(
					sponsor,
					&sponsor_network,
					&code,
					(new_device, false),
					nibkeep_node::Showing::new(TIMEOUT),
					&now,
					&mut sponsor_rng,
				),
			}
		});
		nibkeep_node::join(
			directory,
			joining,
			&joiner_network,
			&code,
			protection,
			TIMEOUT,
			&now,
			rng,
		)
		.expect("joins");
		paired.join().expect("the sponsor ran").expect("pairs");
	});
}

/// A home for a test's own Git settings: a name and an email to commit with, and `main` for
/// new repositories' branch.
fn git_home(name: &str) -> PathBuf {
	let home =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, git home"));
	std::fs::write(
		home.join("config"),
		"[user]\n\tname = Ada\n\temail = ada@example.invalid\n[init]\n\tdefaultBranch = main\n",
	)
	.expect("writes Git's settings");
	home
}

/// Syncs the libraries of two tests over connections on localhost, as nib sync and nib sync
/// --serve do: `first` connects, and `second` answers; both at the time `at` gives. The other
/// devices that `first` knows of are not found.
pub(crate) fn sync_on_localhost(first: &Test, second: &Test, at: fn() -> u64) {
	use nibkeep_node::{Library, NetworkOptions, Unlocker};
	let open =
		|test: &Test| Library::open(&test.here.join(".nib"), &Unlocker::Keystore).expect("opens");
	let mut first = open(first);
	let second = open(second);
	let first_network = first.network(NetworkOptions::LOCAL).expect("starts");
	let second_network = second.network(NetworkOptions::LOCAL).expect("starts");
	first_network.remember(
		second_network.endpoint(),
		&second_network.local_addresses(),
		None,
	);
	let closed = second.close();
	std::thread::scope(|scope| {
		let answered = scope.spawn(|| {
			let mut answering = TestRng::new("answering");
			nibkeep_node::answer(&closed, &second_network, Some(TIMEOUT), &at, &mut answering)
		});
		let mut rng = TestRng::new("syncing");
		let peers = first.peers().expect("lists");
		let found = std::collections::BTreeSet::from([second_network.endpoint()]);
		let synced =
			nibkeep_node::sync_with_found(&mut first, &first_network, peers, &found, &at, &mut rng);
		assert!(
			synced.iter().any(|with| with.synced.is_ok())
				&& synced.iter().all(|with| {
					with.synced.is_ok()
						|| matches!(with.synced, Err(nibkeep_node::NodeError::NotFound))
				}),
			"{synced:?}"
		);
		let answered = answered.join().expect("ran").expect("answers");
		assert!(answered.is_some_and(|answered| answered.synced.is_ok()));
	});
}
