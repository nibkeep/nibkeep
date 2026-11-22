//! A command run apart from the test and from the person at the computer: on Unix in a session
//! of its own, with no terminal to ask on, or with a terminal of its own, which the test types
//! into; on Windows in a console that nobody sees, and in a job of its own. It ends with what
//! it started there, whether the test kills it, it ends by itself, or the test is interrupted.

#[cfg(unix)]
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A running command, with what it printed so far.
pub(crate) struct Process {
	child: Child,
	/// Its standard input, where the test writes to it, until the test closes it.
	stdin: Option<ChildStdin>,
	stdout: Arc<Mutex<Vec<u8>>>,
	/// What it printed on its standard error, or for a command at a terminal, what the
	/// terminal showed.
	stderr: Arc<Mutex<Vec<u8>>>,
	/// The terminal's side that the test reads and types into, for a command at a terminal.
	#[cfg(unix)]
	terminal: Option<File>,
	readers: Vec<JoinHandle<()>>,
	/// How it ended, once it has.
	ended: Option<ExitStatus>,
	#[cfg(windows)]
	job: Option<windows::Job>,
}

impl Process {
	/// Starts `command`, which writes to pipes that the process reads as it goes, and reads
	/// nothing: its standard input is empty.
	pub(crate) fn start(command: Command) -> Self {
		Self::start_reading(command, false)
	}

	/// Starts `command`, as [`Process::start`] does, with standard input that the test writes
	/// to, with [`Process::write`], if `input`.
	pub(crate) fn start_reading(mut command: Command, input: bool) -> Self {
		command
			.stdin(if input { Stdio::piped() } else { Stdio::null() })
			.stderr(Stdio::piped());
		Self::spawn(
			command,
			#[cfg(unix)]
			None,
		)
	}

	/// Starts `command` at a terminal of its own, as a person runs it: the terminal is its
	/// controlling terminal, where it asks for passphrases, its standard input and its
	/// standard error, and the test types into it, with [`Process::type_at_terminal`]. Its
	/// standard output goes to a pipe, as for every command, for the test to compare.
	#[cfg(unix)]
	pub(crate) fn start_at_terminal(mut command: Command) -> Self {
		let (ours, its) = unix::open_terminal();
		command
			.stdin(its.try_clone().expect("shares the terminal"))
			.stderr(its);
		Self::spawn(command, Some(ours))
	}

	/// Starts `command`, whose standard input and error are set, with its standard output
	/// to a pipe, apart, and at `terminal` if it is given, the test's side of a terminal.
	fn spawn(mut command: Command, #[cfg(unix)] terminal: Option<File>) -> Self {
		command.stdout(Stdio::piped());
		apart(&mut command);
		#[cfg(unix)]
		if terminal.is_some() {
			unix::controlled_by_its_terminal(&mut command);
		}
		let mut child = command
			.spawn()
			.unwrap_or_else(|error| panic!("cannot start {command:?}: {error}"));
		// The command holds the test's copies of the terminal's other side: closed, the
		// terminal ends when the program and what it started have closed theirs.
		drop(command);
		#[cfg(unix)]
		unix::started(&child);
		#[cfg(windows)]
		let job = windows::Job::around(&child);
		let stdout = Arc::default();
		let stderr = Arc::default();
		let mut readers = vec![read_into(
			child.stdout.take().expect("piped"),
			Arc::clone(&stdout),
		)];
		if let Some(piped) = child.stderr.take() {
			readers.push(read_into(piped, Arc::clone(&stderr)));
		}
		#[cfg(unix)]
		if let Some(terminal) = &terminal {
			let shown = terminal.try_clone().expect("reads the terminal");
			readers.push(read_into(shown, Arc::clone(&stderr)));
		}
		Self {
			stdin: child.stdin.take(),
			child,
			stdout,
			stderr,
			#[cfg(unix)]
			terminal,
			readers,
			ended: None,
			#[cfg(windows)]
			job,
		}
	}

	/// Types `text` at its terminal, as keys that a person presses: `\r` for Enter. Returns
	/// whether it could: not without a terminal, or once the terminal ended.
	#[cfg(unix)]
	pub(crate) fn type_at_terminal(&mut self, text: &str) -> bool {
		self.terminal.as_mut().is_some_and(|terminal| {
			terminal
				.write_all(text.as_bytes())
				.and_then(|()| terminal.flush())
				.is_ok()
		})
	}

	/// What its terminal showed after the first `from` bytes, with the line ends of files,
	/// and how many bytes it showed in all.
	#[cfg(unix)]
	pub(crate) fn shown_since(&self, from: usize) -> (String, usize) {
		let shown = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
		let since = String::from_utf8_lossy(shown.get(from..).unwrap_or_default());
		(since.replace("\r\n", "\n"), shown.len())
	}

	/// Whether its terminal shows what is typed, as it does unless the program reads a
	/// passphrase; `None` without a terminal, or if its settings cannot be read.
	#[cfg(unix)]
	pub(crate) fn terminal_echoes(&self) -> Option<bool> {
		self.terminal.as_ref().and_then(unix::echoes)
	}

	/// Writes `text` to its standard input. Returns whether it could: not once it ended, or
	/// its input was closed.
	pub(crate) fn write(&mut self, text: &str) -> bool {
		self.stdin.as_mut().is_some_and(|stdin| {
			stdin
				.write_all(text.as_bytes())
				.and_then(|()| stdin.flush())
				.is_ok()
		})
	}

	/// Closes its standard input, which it then reads to the end.
	pub(crate) fn close_input(&mut self) {
		self.stdin = None;
	}

	/// What it printed so far on its standard output.
	pub(crate) fn stdout(&self) -> String {
		text(&self.stdout)
	}

	/// What it printed so far on its standard error, or for a command at a terminal, what
	/// the terminal showed, with the line ends of files.
	pub(crate) fn stderr(&self) -> String {
		let text = text(&self.stderr);
		#[cfg(unix)]
		if self.terminal.is_some() {
			return text.replace("\r\n", "\n");
		}
		text
	}

	/// How it ended, if it has: then, what it started has ended too, and what it printed has
	/// all been read.
	pub(crate) fn ended(&mut self) -> Option<ExitStatus> {
		if self.ended.is_some() {
			return self.ended;
		}
		// On Unix, what it started is ended before it is waited for: until then, its
		// process group cannot be another's.
		#[cfg(unix)]
		if unix::exited(&self.child) {
			unix::kill_group(&self.child);
		} else {
			return None;
		}
		if let Some(status) = self.child.try_wait().expect("looks at the program") {
			self.end_the_rest();
			self.ended = Some(status);
		}
		self.ended
	}

	/// Waits until it ends, for as long as `patience`.
	pub(crate) fn wait(&mut self, patience: Duration) -> Option<ExitStatus> {
		let started = Instant::now();
		loop {
			if let Some(status) = self.ended() {
				return Some(status);
			}
			if started.elapsed() >= patience {
				return None;
			}
			std::thread::sleep(Duration::from_millis(10));
		}
	}

	/// Interrupts it, with what it started, as Ctrl-C on a terminal does, unless it ended.
	#[cfg(unix)]
	pub(crate) fn interrupt(&mut self) {
		if self.ended().is_none() {
			unix::interrupt_group(&self.child);
		}
	}

	/// Kills it at once, with what it started, and waits until it is gone.
	pub(crate) fn kill(&mut self) -> ExitStatus {
		if let Some(status) = self.ended {
			return status;
		}
		#[cfg(unix)]
		unix::kill_group(&self.child);
		#[cfg(windows)]
		if let Some(job) = &self.job {
			job.kill();
		}
		let _ = self.child.kill();
		let status = self.child.wait().expect("waits for the program");
		self.end_the_rest();
		self.ended = Some(status);
		status
	}

	/// Ends what it started and left running, which on Unix is done before it is waited for,
	/// and waits a little for the rest of what it printed: not for ever, in case something it
	/// started holds its output open.
	fn end_the_rest(&mut self) {
		#[cfg(unix)]
		unix::ended(&self.child);
		#[cfg(windows)]
		if let Some(job) = &self.job {
			job.kill();
		}
		let started = Instant::now();
		while self.readers.iter().any(|reader| !reader.is_finished())
			&& started.elapsed() < Duration::from_secs(5)
		{
			std::thread::sleep(Duration::from_millis(5));
		}
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		self.kill();
	}
}

fn text(bytes: &Mutex<Vec<u8>>) -> String {
	String::from_utf8_lossy(&bytes.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
}

/// Reads all that `from` gives into `into`, on a thread of its own.
fn read_into(mut from: impl Read + Send + 'static, into: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
	std::thread::spawn(move || {
		let mut buffer = [0; 8192];
		loop {
			match from.read(&mut buffer) {
				Ok(0) => break,
				Ok(read) => into
					.lock()
					.unwrap_or_else(PoisonError::into_inner)
					.extend_from_slice(&buffer[..read]),
				Err(error) if error.kind() == ErrorKind::Interrupted => {}
				Err(_) => break,
			}
		}
	})
}

/// Sets the command up to run apart: on Unix, a session of its own, so that it has no
/// controlling terminal, and a process group of its own, which ends with it; on Linux, also
/// killed when the thread that started it ends, as when the test is killed. On other Unixes,
/// a test killed at once, rather than interrupted, leaves its commands running.
#[cfg(unix)]
#[allow(unsafe_code)]
fn apart(command: &mut Command) {
	use std::os::unix::process::CommandExt;
	// SAFETY: between fork and exec, the closure calls only setsid and prctl, which are
	// async-signal-safe, and allocates nothing.
	unsafe {
		command.pre_exec(|| {
			if libc::setsid() == -1 {
				return Err(std::io::Error::last_os_error());
			}
			#[cfg(target_os = "linux")]
			if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
				return Err(std::io::Error::last_os_error());
			}
			Ok(())
		});
	}
}

/// Sets the command up to run apart: on Windows, a console of its own that nobody sees, so
/// that it never reads the person's keys, and that what it starts, such as Git, opens no
/// window either. A question that it asks there waits until the test kills it.
#[cfg(windows)]
fn apart(command: &mut Command) {
	use std::os::windows::process::CommandExt;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn apart(_command: &mut Command) {}

#[cfg(unix)]
mod unix {
	use std::ffi::CStr;
	use std::fs::{File, OpenOptions};
	use std::os::fd::AsRawFd;
	use std::os::unix::fs::OpenOptionsExt;
	use std::os::unix::process::CommandExt;
	use std::process::{Child, Command};
	use std::sync::{Mutex, Once, PoisonError};

	/// A new terminal, a pseudo-terminal: the side that the test reads and types into, and
	/// the side that the program has. Both are closed in the programs that other tests start
	/// meanwhile, as std opens every file, and neither becomes the test's own terminal.
	#[allow(unsafe_code)]
	pub(super) fn open_terminal() -> (File, File) {
		/// `ptsname` names the other side in a buffer of its own, which another call
		/// overwrites.
		static NAMING: Mutex<()> = Mutex::new(());
		let ours = OpenOptions::new()
			.read(true)
			.write(true)
			.custom_flags(libc::O_NOCTTY)
			.open("/dev/ptmx")
			.expect("opens a pseudo-terminal");
		let fd = ours.as_raw_fd();
		let its_name = {
			let _naming = NAMING.lock().unwrap_or_else(PoisonError::into_inner);
			// SAFETY: the calls take the descriptor of a pseudo-terminal that `ours` holds
			// open. ptsname's buffer is read before the lock goes, and only this function
			// calls it in the test.
			unsafe {
				assert!(
					libc::grantpt(fd) == 0 && libc::unlockpt(fd) == 0,
					"cannot unlock the pseudo-terminal: {}",
					std::io::Error::last_os_error()
				);
				let name = libc::ptsname(fd);
				assert!(!name.is_null(), "the pseudo-terminal has no name");
				CStr::from_ptr(name).to_owned()
			}
		};
		let its = OpenOptions::new()
			.read(true)
			.write(true)
			.custom_flags(libc::O_NOCTTY)
			.open(its_name.to_str().expect("a path in UTF-8"))
			.expect("opens the program's side of the pseudo-terminal");
		(ours, its)
	}

	/// Makes the terminal of the command's standard input its controlling terminal, as a
	/// terminal is of the programs that a person starts there: where `/dev/tty` leads, which
	/// passphrases are read from. It must come after [`super::apart`], as only the leader of
	/// a session without one takes a controlling terminal.
	#[allow(unsafe_code)]
	pub(super) fn controlled_by_its_terminal(command: &mut Command) {
		// SAFETY: between fork and exec, the closure calls only ioctl, which is
		// async-signal-safe, and allocates nothing. Standard input is the terminal by then.
		unsafe {
			command.pre_exec(|| {
				if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
					return Err(std::io::Error::last_os_error());
				}
				Ok(())
			});
		}
	}

	/// Whether the terminal shows what is typed: the settings of a pseudo-terminal, which its
	/// two sides share. `None` if they cannot be read.
	#[allow(unsafe_code)]
	pub(super) fn echoes(terminal: &File) -> Option<bool> {
		// SAFETY: `settings` is a zeroed structure on the stack that tcgetattr fills, for a
		// descriptor that `terminal` holds open.
		unsafe {
			let mut settings: libc::termios = std::mem::zeroed();
			(libc::tcgetattr(terminal.as_raw_fd(), &raw mut settings) == 0)
				.then_some(settings.c_lflag & libc::ECHO != 0)
		}
	}

	/// The process groups of the commands running, which an interruption of the test ends:
	/// in sessions of their own, they do not get the terminal's Ctrl-C.
	static RUNNING: Mutex<Vec<libc::pid_t>> = Mutex::new(Vec::new());

	fn group(child: &Child) -> Option<libc::pid_t> {
		libc::pid_t::try_from(child.id()).ok()
	}

	pub(super) fn started(child: &Child) {
		static HANDLER: Once = Once::new();
		HANDLER.call_once(|| {
			// Another handler of the test's own would win; then that one cleans up.
			let _ = ctrlc::set_handler(|| {
				for &group in RUNNING
					.lock()
					.unwrap_or_else(PoisonError::into_inner)
					.iter()
				{
					signal_group(group);
				}
				std::process::exit(130);
			});
		});
		if let Some(group) = group(child) {
			RUNNING
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.push(group);
		}
	}

	/// Whether the command has exited, without waiting for it: until it is waited for, its
	/// process ID, and so its group's, stays its own.
	#[allow(unsafe_code)]
	pub(super) fn exited(child: &Child) -> bool {
		let Some(pid) = group(child).and_then(|pid| libc::id_t::try_from(pid).ok()) else {
			return false;
		};
		// SAFETY: `info` is a zeroed structure on the stack that waitid fills; WNOWAIT leaves
		// the command to be waited for, and WNOHANG returns at once.
		unsafe {
			let mut info: libc::siginfo_t = std::mem::zeroed();
			let found = libc::waitid(
				libc::P_PID,
				pid,
				&raw mut info,
				libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
			);
			found == 0 && pid_of(&info) != 0
		}
	}

	/// The process that a `siginfo_t` that waitid filled tells of, or 0 for none.
	#[cfg(target_os = "linux")]
	#[allow(unsafe_code)]
	fn pid_of(info: &libc::siginfo_t) -> libc::pid_t {
		// SAFETY: waitid fills the field of the process for the codes it gives.
		unsafe { info.si_pid() }
	}

	#[cfg(not(target_os = "linux"))]
	fn pid_of(info: &libc::siginfo_t) -> libc::pid_t {
		info.si_pid
	}

	pub(super) fn ended(child: &Child) {
		if let Some(group) = group(child) {
			RUNNING
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.retain(|&running| running != group);
		}
	}

	/// Kills the command's process group: the command, which leads it, and what it started
	/// that stayed in it.
	pub(super) fn kill_group(child: &Child) {
		if let Some(group) = group(child) {
			signal_group(group);
		}
	}

	/// Interrupts the command's process group, as Ctrl-C on a terminal does.
	pub(super) fn interrupt_group(child: &Child) {
		if let Some(group) = group(child) {
			send(group, libc::SIGINT);
		}
	}

	fn signal_group(group: libc::pid_t) {
		send(group, libc::SIGKILL);
	}

	#[allow(unsafe_code)]
	fn send(group: libc::pid_t, signal: libc::c_int) {
		// SAFETY: killpg takes plain integers and touches no memory of this process. The
		// group is the one that the command leads, which lives on while it or a program
		// it started is in it.
		unsafe {
			libc::killpg(group, signal);
		}
	}
}

#[cfg(windows)]
mod windows {
	use std::os::windows::io::AsRawHandle;
	use std::process::Child;

	use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
	use windows_sys::Win32::System::JobObjects::{
		AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
		JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
		SetInformationJobObject, TerminateJobObject,
	};

	/// A job that holds the command and what it starts, killed with them when it closes: when
	/// the test ends it, or when the test's process ends, however it ends.
	pub(super) struct Job(HANDLE);

	impl Job {
		/// A job around `child`, if Windows makes one. A program that the command starts
		/// before it is in the job is not in it.
		#[allow(unsafe_code)]
		pub(super) fn around(child: &Child) -> Option<Self> {
			// SAFETY: the calls take a job handle that this function made and checked, the
			// child's own handle, which lives as long as `child`, and a pointer to a
			// structure on the stack, with its size.
			unsafe {
				let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
				if job.is_null() {
					return None;
				}
				let job = Self(job);
				let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
				limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
				let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
					.expect("a small structure");
				let limited = SetInformationJobObject(
					job.0,
					JobObjectExtendedLimitInformation,
					(&raw const limits).cast(),
					size,
				);
				let assigned = AssignProcessToJobObject(job.0, child.as_raw_handle());
				(limited != 0 && assigned != 0).then_some(job)
			}
		}

		/// Kills every program in the job, with an exit code that no program of NibKeep ends
		/// with by itself, as a signal's 128 + 9 is on Unix.
		#[allow(unsafe_code)]
		pub(super) fn kill(&self) {
			// SAFETY: the handle is this job's, open until it is dropped.
			unsafe {
				TerminateJobObject(self.0, 137);
			}
		}
	}

	impl Drop for Job {
		#[allow(unsafe_code)]
		fn drop(&mut self) {
			// SAFETY: the handle is this job's, and is closed only here.
			unsafe {
				CloseHandle(self.0);
			}
		}
	}
}
