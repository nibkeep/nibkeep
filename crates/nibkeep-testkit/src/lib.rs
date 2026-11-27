//! Helpers shared by the tests of NibKeep: randomness that repeats from run to run, the
//! identifiers and keys made from it, and wall clock times on given dates.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::rand_core::{TryCryptoRng, TryRng};
use nibkeep_crypto::{Key, SigningKey};
use nibkeep_time::Day;

pub mod media;

/// A random number generator with a fixed seed, so that every run of a test sees the same
/// "random" bytes: BLAKE3's extendable output, which is as good as random for tests.
///
/// It is a cryptographically secure generator only in form. Tests use it where the code
/// asks for one; real devices use the operating system's.
pub struct TestRng(blake3::OutputReader);

impl TestRng {
	/// Returns a generator seeded with `seed`.
	#[must_use]
	pub fn new(seed: &str) -> Self {
		Self(blake3::Hasher::new().update(seed.as_bytes()).finalize_xof())
	}

	/// Returns the next `N` bytes.
	pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
		let mut bytes = [0; N];
		self.0.fill(&mut bytes);
		bytes
	}

	/// Returns a new entity ID.
	pub fn entity_id(&mut self) -> EntityId {
		EntityId::from_random_bytes(self.bytes())
	}

	/// Returns a new statement ID.
	pub fn statement_id(&mut self) -> StatementId {
		StatementId::from_random_bytes(self.bytes())
	}

	/// Returns a new space ID.
	pub fn space_id(&mut self) -> SpaceId {
		SpaceId::from_random_bytes(self.bytes())
	}

	/// Returns a new key.
	pub fn key(&mut self) -> Key {
		Key::generate(self)
	}

	/// Returns a new signing key.
	pub fn signing_key(&mut self) -> SigningKey {
		SigningKey::generate(self)
	}
}

impl TryRng for TestRng {
	type Error = Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Infallible> {
		Ok(u32::from_le_bytes(self.bytes()))
	}

	fn try_next_u64(&mut self) -> Result<u64, Infallible> {
		Ok(u64::from_le_bytes(self.bytes()))
	}

	fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
		self.0.fill(destination);
		Ok(())
	}
}

impl TryCryptoRng for TestRng {}

/// The storage key of a device in tests, which tags the changes it stores.
#[must_use]
pub fn storage_key() -> Key {
	Key::from_bytes([0x5a; 32])
}

/// The wall clock time, in milliseconds since 1970-01-01T00:00:00Z, at `hour:minute` UTC on
/// `date`, such as `2026-10-10`.
///
/// # Panics
///
/// Panics if `date` is not a date from 1970 on, or the time is not a time of day.
#[must_use]
pub fn millis_at(date: &str, hour: u8, minute: u8) -> u64 {
	assert!(hour < 24 && minute < 60, "not a time of day");
	let day: Day = date.parse().expect("a date such as 2026-10-10");
	let days = u64::try_from(day.days_since_epoch()).expect("a date from 1970 on");
	((days * 24 + u64::from(hour)) * 60 + u64::from(minute)) * 60_000
}

/// The directory under which tests keep their files: the one that `NIBKEEP_TEST_DIR` names
/// when it is set, such as on a phone, where the directories of the computer that built the
/// tests do not exist; otherwise `default`, usually `env!("CARGO_TARGET_TMPDIR")`. On iOS,
/// where a test runs as an app that may only write in its own container, and whose path is
/// known only on the phone, the default is the app's temporary directory instead.
///
/// On iOS, the first call also raises the process's limit of open files, as
/// [`allow_more_open_files`] does.
///
/// # Panics
///
/// Panics if `NIBKEEP_TEST_DIR` is set but not to an absolute path, so that tests never
/// delete and write files relative to wherever they run.
#[must_use]
pub fn test_directory(default: impl AsRef<Path>) -> PathBuf {
	allow_more_open_files();
	match std::env::var_os("NIBKEEP_TEST_DIR") {
		Some(directory) => {
			let directory = PathBuf::from(directory);
			assert!(
				directory.is_absolute(),
				"NIBKEEP_TEST_DIR must be an absolute path"
			);
			directory
		}
		None if cfg!(target_os = "ios") => std::env::temp_dir(),
		None => default.as_ref().to_owned(),
	}
}

/// The name of the test running on this thread, with its file's module, such as
/// `crash::a_crash_while_writing_leaves_the_last_committed_state`: libtest runs each test on
/// a thread of that name. A test that runs its own program again, as a child to crash, passes
/// it with `--exact`, so that the child runs that test alone.
///
/// # Panics
///
/// Panics on a thread that is not a test's own: one without a name, or the main thread, on
/// which libtest runs a test when it cannot start a thread for it.
#[must_use]
pub fn this_test() -> String {
	let name = std::thread::current()
		.name()
		.expect("the test's thread, which libtest names after the test")
		.to_owned();
	assert_ne!(
		name, "main",
		"the test runs on the main thread, as libtest could start no thread for it, so its name is unknown"
	);
	name
}

/// Raises the process's limit of open files, once, on iOS, which gives an app 256 unless it
/// asks for more: a test runs several devices at once in one app, each with its databases
/// and sockets open, which an iPhone's six threads of tests ran out of. Not on macOS, which
/// starts a program with 256 too, so that the programs that tests start there, such as the
/// end-to-end tests' `nib`, run as a person's would; elsewhere the limit is far higher.
pub fn allow_more_open_files() {
	#[cfg(target_os = "ios")]
	{
		static ONCE: std::sync::Once = std::sync::Once::new();
		ONCE.call_once(|| {
			use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
			let limit = getrlimit(Resource::Nofile);
			// 10,240 is OPEN_MAX on Apple's systems, which refuse an unlimited count.
			let most = limit.maximum.map_or(10_240, |maximum| maximum.min(10_240));
			if limit.current.is_some_and(|current| current < most) {
				let raised = Rlimit {
					current: Some(most),
					maximum: limit.maximum,
				};
				// Without it, the tests of many devices fail as they did, with what they say.
				let _ = setrlimit(Resource::Nofile, raised);
			}
		});
	}
}

/// An empty directory of its own for a test, called `name`, under
/// [`test_directory`]`(default)`: whatever an earlier run left there is deleted.
///
/// # Panics
///
/// Panics if the directory cannot be made, or as [`test_directory`] does.
#[must_use]
pub fn empty_directory(default: impl AsRef<Path>, name: &str) -> PathBuf {
	let directory = test_directory(default).join(name);
	let _ = std::fs::remove_dir_all(&directory);
	std::fs::create_dir_all(&directory).expect("makes the directory");
	directory
}

/// Copies a folder with all it holds, as a backup or a program that syncs folders does: into
/// new files and folders, which the system gives new IDs. Windows lets no other program read
/// a file that a program holds locked, as an open replica holds its locks, so such a file is
/// left out, as those programs leave it out.
///
/// # Panics
///
/// Panics if a folder cannot be made or listed, or a file copied, but for one that Windows
/// keeps locked.
pub fn copy_folder(from: &Path, to: &Path) {
	std::fs::create_dir_all(to).expect("makes the folder");
	for entry in std::fs::read_dir(from).expect("lists the folder") {
		let entry = entry.expect("an entry");
		let target = to.join(entry.file_name());
		if entry.file_type().expect("a type").is_dir() {
			copy_folder(&entry.path(), &target);
		} else if let Err(error) = std::fs::copy(entry.path(), &target) {
			assert!(
				cfg!(windows) && error.raw_os_error() == Some(33),
				"copies {}: {error}",
				entry.path().display()
			);
			let _ = std::fs::remove_file(&target);
		}
	}
}

/// Writes a file that nobody else may read, as a passphrase file must be: on Unix, readable
/// and writable by its owner alone.
///
/// # Panics
///
/// Panics if the file cannot be written.
pub fn write_private(path: &Path, text: &str) {
	use std::io::Write;
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	// Private from the start, and private too if it was there before.
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	let mut file = options.open(path).expect("opens the file");
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		file.set_permissions(std::fs::Permissions::from_mode(0o600))
			.expect("makes it private");
	}
	file.write_all(text.as_bytes()).expect("writes the file");
}

/// Whether any file under a directory, in the directories in it too, holds these bytes: to
/// check that a secret never reaches the disk in plain.
///
/// # Panics
///
/// Panics if a directory cannot be listed or a file read, but for a file that an open library
/// locks, which Windows refuses to read: a space's empty `lock`, and SQLite's shared memory
/// (`-shm`), which holds only an index of the pages in its log.
#[must_use]
pub fn holds_anywhere(directory: &Path, bytes: &[u8]) -> bool {
	let mut to_visit = vec![directory.to_path_buf()];
	while let Some(next) = to_visit.pop() {
		for entry in std::fs::read_dir(&next).expect("lists a directory") {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				to_visit.push(path);
				continue;
			}
			let locked = |path: &Path| {
				path.to_string_lossy().ends_with("-shm")
					|| std::fs::metadata(path).is_ok_and(|metadata| metadata.len() == 0)
			};
			let content = match std::fs::read(&path) {
				Ok(content) => content,
				Err(_) if locked(&path) => continue,
				Err(error) => panic!("reads {}: {error}", path.display()),
			};
			if content.windows(bytes.len()).any(|window| window == bytes) {
				return true;
			}
		}
	}
	false
}

/// The directory of the compatibility fixtures: the one that `NIBKEEP_FIXTURES` names when
/// it is set, such as on a phone, otherwise `fixtures` in the workspace. On iOS, the default
/// is `fixtures` next to the test executable, in the app that `scripts/test-on-device.py`
/// makes of it.
///
/// # Panics
///
/// Panics if `NIBKEEP_FIXTURES` is set but not to an absolute path, or on iOS, if the test
/// executable's path is not known.
#[must_use]
pub fn fixtures_directory() -> PathBuf {
	match std::env::var_os("NIBKEEP_FIXTURES") {
		Some(directory) => {
			let directory = PathBuf::from(directory);
			assert!(
				directory.is_absolute(),
				"NIBKEEP_FIXTURES must be an absolute path"
			);
			directory
		}
		None if cfg!(target_os = "ios") => std::env::current_exe()
			.expect("the test executable's path")
			.with_file_name("fixtures"),
		None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"),
	}
}

/// Asserts that a collection is empty, and shows what it holds if not. A message can follow,
/// as with `assert!`. Collections that may hold secret values are checked with `assert!`
/// instead, so that no secret appears in a panic message.
#[macro_export]
macro_rules! assert_empty {
	($value:expr $(,)?) => {{
		let value = &$value;
		assert!(value.is_empty(), "expected nothing, found {value:?}");
	}};
	($value:expr, $($message:tt)+) => {{
		let value = &$value;
		assert!(
			value.is_empty(),
			"{}: expected nothing, found {value:?}",
			format_args!($($message)+)
		);
	}};
}

/// A monotonic clock that moves only when a test moves it, for code that tells events apart
/// by how soon one follows another, such as the bursts of changes of a stored log: the same
/// on every machine, however fast, and without waiting.
#[derive(Clone, Debug)]
pub struct ManualClock {
	start: Instant,
	/// How far it moved, in nanoseconds.
	elapsed: Arc<AtomicU64>,
}

impl ManualClock {
	/// A clock that stands still until it is moved.
	#[must_use]
	pub fn new() -> Self {
		Self {
			start: Instant::now(),
			elapsed: Arc::new(AtomicU64::new(0)),
		}
	}

	/// The time now, by this clock.
	#[must_use]
	pub fn now(&self) -> Instant {
		self.start + Duration::from_nanos(self.elapsed.load(Ordering::Relaxed))
	}

	/// Moves the clock forward.
	pub fn advance(&self, by: Duration) {
		let nanos = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
		self.elapsed.fetch_add(nanos, Ordering::Relaxed);
	}
}

impl Default for ManualClock {
	fn default() -> Self {
		Self::new()
	}
}

/// Child processes that a test kills while they work, to see what survives: the notes in
/// which they record what is done, and their killing.
pub mod child {
	use std::fs;
	use std::io::Write;
	use std::path::{Path, PathBuf};
	use std::process::{Child, Stdio};
	use std::thread;
	use std::time::{Duration, Instant};

	fn note_path(directory: &Path) -> PathBuf {
		directory.join("done")
	}

	fn errors_path(directory: &Path) -> PathBuf {
		directory.join("child errors")
	}

	/// A child process that is killed when this is dropped, also when the test panics, so
	/// that no child goes on working after its test, into what the next run makes.
	pub struct Guard(pub Child);

	impl std::ops::Deref for Guard {
		type Target = Child;

		fn deref(&self) -> &Child {
			&self.0
		}
	}

	impl std::ops::DerefMut for Guard {
		fn deref_mut(&mut self) -> &mut Child {
			&mut self.0
		}
	}

	impl Drop for Guard {
		fn drop(&mut self) {
			let _ = self.0.kill();
			let _ = self.0.wait();
		}
	}

	/// Notes on the real disk what the child has done, as numbers, synced and replaced in
	/// one step, so that a note is never half written. Only once it is noted is something
	/// done, as far as the test can tell.
	///
	/// # Panics
	///
	/// Panics if the note cannot be written.
	pub fn note(directory: &Path, numbers: &[usize]) {
		let path = directory.join("done.new");
		let text: Vec<String> = numbers.iter().map(ToString::to_string).collect();
		let mut file = fs::File::create(&path).expect("creates the note");
		file.write_all(text.join(" ").as_bytes())
			.and_then(|()| file.sync_all())
			.expect("writes the note");
		fs::rename(&path, note_path(directory)).expect("replaces the note");
	}

	/// The numbers last noted, each 0 if none was.
	#[must_use]
	pub fn noted<const N: usize>(directory: &Path) -> [usize; N] {
		let text = fs::read_to_string(note_path(directory)).unwrap_or_default();
		let mut numbers = text.split(' ').map(|number| number.parse().unwrap_or(0));
		std::array::from_fn(|_| numbers.next().unwrap_or(0))
	}

	/// Where a child writes its errors: a file, so that a child that fails says why.
	///
	/// # Panics
	///
	/// Panics if the file cannot be made.
	#[must_use]
	pub fn errors(directory: &Path) -> Stdio {
		fs::File::create(errors_path(directory))
			.expect("creates the file of the child's errors")
			.into()
	}

	/// What the child wrote to its errors, if anything.
	#[must_use]
	pub fn errors_written(directory: &Path) -> String {
		fs::read_to_string(errors_path(directory)).unwrap_or_default()
	}

	/// Kills the child once it has noted something new since it started, after `delay`, so
	/// that every round tests a crash after real work, however slow the device. Then waits
	/// a moment, so that what the kernel still had of the child's writes arrives.
	///
	/// # Panics
	///
	/// Panics, with the child's errors, if it ends by itself, or notes nothing for two
	/// minutes, when it is killed.
	pub fn kill_after_progress(child: &mut Child, directory: &Path, round: u64, delay: Duration) {
		let before = fs::read_to_string(note_path(directory)).unwrap_or_default();
		wait_for(child, directory, &format!("round {round}: "), || {
			fs::read_to_string(note_path(directory)).unwrap_or_default() != before
		});
		thread::sleep(delay);
		child.kill().expect("kills the child");
		child.wait().expect("waits for the child");
		thread::sleep(Duration::from_millis(50));
	}

	/// Waits until the child has noted something, such as that it has opened what it shares
	/// with the test, so that the test goes on only once the child is that far. The
	/// directory holds no note before the child's first.
	///
	/// # Panics
	///
	/// Panics, with the child's errors, if it ends by itself, or notes nothing for two
	/// minutes, when it is killed.
	pub fn wait_for_note(child: &mut Child, directory: &Path) {
		wait_for(child, directory, "", || note_path(directory).exists());
	}

	/// Waits until `done`, panicking with `context` and the child's errors if the child
	/// ends by itself first, or if two minutes pass, when it is killed.
	fn wait_for(child: &mut Child, directory: &Path, context: &str, done: impl Fn() -> bool) {
		let started = Instant::now();
		while !done() {
			if started.elapsed() > Duration::from_secs(120) {
				let _ = child.kill();
				let _ = child.wait();
				panic!(
					"{context}the child did nothing\n{}",
					errors_written(directory)
				);
			}
			if let Some(status) = child.try_wait().expect("checks the child") {
				// It may have noted what was waited for just before it ended.
				if done() {
					return;
				}
				panic!(
					"{context}the child ended by itself: {status}\n{}",
					errors_written(directory)
				);
			}
			thread::sleep(Duration::from_millis(2));
		}
	}
}

/// LazyFS (<https://github.com/dsrhaslab/lazyfs>), a file system that keeps the data
/// written to files in memory until it is synced, and on command drops what is not, as a
/// power cut does. It carries out creating, renaming and deleting files at once, so the
/// syncing of directories is not tested with it; real power cuts on a Raspberry Pi test
/// that. It runs on Linux, where FUSE is.
#[cfg(target_os = "linux")]
pub mod lazyfs {
	use std::fs;
	use std::io::{BufRead, BufReader, Write};
	use std::path::{Path, PathBuf};
	use std::process::{Child, Command, Stdio};
	use std::sync::mpsc::{self, Receiver};
	use std::thread;
	use std::time::{Duration, Instant};

	/// How long LazyFS may take to start, or to carry out a command.
	const PATIENCE: Duration = Duration::from_secs(30);

	/// LazyFS, mounted for the test, and unmounted when it ends.
	pub struct LazyFs {
		process: Child,
		/// Where it is mounted.
		pub mount: PathBuf,
		commands: fs::File,
		/// The replies of LazyFS to commands, line by line.
		replies: Receiver<String>,
	}

	impl LazyFs {
		/// Mounts LazyFS at `<directory>/mount`, keeping its files in `<directory>/root`.
		///
		/// # Panics
		///
		/// Panics if LazyFS does not mount within half a minute.
		#[must_use]
		pub fn mount(program: &Path, directory: &Path) -> Self {
			let mount = directory.join("mount");
			let root = directory.join("root");
			fs::create_dir_all(&mount).expect("makes the mount point");
			fs::create_dir_all(&root).expect("makes the root");
			// The kernel lists mounts by their real paths.
			let mount = mount.canonicalize().expect("a real path");
			let root = root.canonicalize().expect("a real path");
			assert!(
				!root.to_string_lossy().contains(','),
				"the mount options cannot hold a path with a comma: {}",
				root.display()
			);
			let commands = directory.join("faults.fifo");
			let completed = directory.join("completed.fifo");
			let config = directory.join("lazyfs.toml");
			fs::write(
				&config,
				format!(
					"[faults]\nfifo_path={}\nfifo_path_completed={}\n\
					 [cache]\napply_eviction=false\n[cache.simple]\ncustom_size=\"1gb\"\nblocks_per_page=1\n\
					 [filesystem]\nlog_all_operations=false\nlogfile=\"\"\n",
					toml_string(&commands),
					toml_string(&completed)
				),
			)
			.expect("writes the configuration");
			let log = directory.join("lazyfs.log");
			let mut process = Command::new(program)
				.arg(&mount)
				.arg("--config-path")
				.arg(&config)
				.args(["-o", "modules=subdir", "-o"])
				.arg(format!("subdir={}", root.display()))
				.arg("-f")
				.stdout(Stdio::null())
				.stderr(fs::File::create(&log).expect("creates the log"))
				.spawn()
				.expect("starts LazyFS");
			let started = Instant::now();
			while !(is_mounted(&mount) && commands.exists() && completed.exists()) {
				let ended = process.try_wait().expect("checks LazyFS");
				if ended.is_some() || started.elapsed() > PATIENCE {
					unmount(&mount);
					let _ = process.kill();
					let _ = process.wait();
					panic!("LazyFS did not mount; see {}", log.display());
				}
				thread::sleep(Duration::from_millis(20));
			}
			// Opened for reading and writing, a pipe opens at once on Linux, whether or not
			// LazyFS has opened its end yet.
			let open = |path: &Path| {
				fs::OpenOptions::new()
					.read(true)
					.write(true)
					.open(path)
					.expect("opens the pipe")
			};
			let commands = open(&commands);
			let completed = BufReader::new(open(&completed));
			let (sender, replies) = mpsc::channel();
			thread::spawn(move || {
				for line in completed.lines() {
					let Ok(line) = line else { break };
					if sender.send(line).is_err() {
						break;
					}
				}
			});
			Self {
				process,
				mount,
				commands,
				replies,
			}
		}

		/// Drops all data that was written to files but not synced, as a power cut does, and
		/// waits until it is done.
		///
		/// # Panics
		///
		/// Panics if LazyFS does not carry it out within half a minute.
		pub fn cut_power(&mut self) {
			self.commands
				.write_all(b"lazyfs::clear-cache\n")
				.expect("sends the command");
			match self.replies.recv_timeout(PATIENCE) {
				Ok(reply) => assert_eq!(reply.trim(), "finished::clear-cache"),
				Err(error) => panic!("LazyFS did not drop the cache: {error}"),
			}
		}
	}

	impl Drop for LazyFs {
		fn drop(&mut self) {
			unmount(&self.mount);
			let _ = self.process.kill();
			let _ = self.process.wait();
		}
	}

	/// Unmounts a file system, also one whose program is gone.
	pub fn unmount(path: &Path) {
		let _ = Command::new("fusermount3")
			.arg("-uz")
			.arg(path)
			.stderr(Stdio::null())
			.status();
	}

	/// A path as a TOML string, with backslashes and quotes escaped.
	fn toml_string(path: &Path) -> String {
		let text = path.to_string_lossy();
		format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
	}

	/// Whether a file system is mounted at `path`, a real path. The list of mounts writes
	/// spaces, tabs, new lines and backslashes in paths as octal escapes.
	fn is_mounted(path: &Path) -> bool {
		let escaped: String = path
			.to_string_lossy()
			.chars()
			.map(|character| match character {
				' ' | '\t' | '\n' | '\\' => format!("\\{:03o}", u32::from(character)),
				other => other.to_string(),
			})
			.collect();
		fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mounts| {
			mounts
				.lines()
				.any(|line| line.split(' ').nth(4) == Some(escaped.as_str()))
		})
	}
}
