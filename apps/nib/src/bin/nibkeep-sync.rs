//! `nibkeep-sync`: syncs the libraries that the person chose with their other devices, from
//! login, with no window ([technical design](../../../../docs/Technical-design.md#106-syncing-from-login)).
//! With `--version`, it tells the engine's version, so that the newest copy runs.

// No console window on Windows, where the login starts it.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_node::at_login::{self, ENGINE_VERSION, LoginEntry, SERVICE_PROGRAM, ServiceOptions};
use nibkeep_node::{BackgroundOptions, DEFAULT_PORT, NetworkOptions};

fn main() -> ExitCode {
	let mut arguments = std::env::args_os().skip(1);
	match arguments.next() {
		None => {}
		Some(argument) if argument == "--version" && arguments.next().is_none() => {
			println!("{SERVICE_PROGRAM} {ENGINE_VERSION}");
			return ExitCode::SUCCESS;
		}
		Some(_) => {
			eprintln!("usage: {SERVICE_PROGRAM} [--version]");
			return ExitCode::from(2);
		}
	}
	// As the replicas it opens record it, beside the other programs.
	nibkeep_node::name_program(SERVICE_PROGRAM);
	let (Some(settings), Ok(program)) = (at_login::settings_folder(), std::env::current_exe())
	else {
		eprintln!("{SERVICE_PROGRAM}: there is no home folder here");
		return ExitCode::FAILURE;
	};
	let stop = Arc::new(AtomicBool::new(false));
	{
		let stop = Arc::clone(&stop);
		let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
	}
	let network = NetworkOptions {
		discovery: true,
		port: Some(DEFAULT_PORT),
		accept: true,
	};
	let mut options =
		ServiceOptions::new(program, BackgroundOptions::new(SERVICE_PROGRAM, network));
	options.entry = LoginEntry::platform();
	// Tests keep it from the computer's keystore, as they keep apps.
	if !at_login::app::kept_away() {
		options.locked = keystore_locked();
	}
	#[cfg(all(
		unix,
		not(any(target_os = "macos", target_os = "ios", target_os = "android"))
	))]
	linux::end_with_session(Arc::clone(&stop));
	match at_login::serve_at_login(&settings, &options, now, || UnwrapErr(SysRng), &stop) {
		Ok(_) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("{SERVICE_PROGRAM}: {error}");
			ExitCode::FAILURE
		}
	}
}

/// How often it tries again to reach the platform's keystore, which may not be there yet at
/// login, as before the session's bus starts.
const KEYSTORE_LOOK: Duration = Duration::from_secs(5);

/// Whether the keystore holds an entry locked: as long as the platform's keystore cannot be
/// reached, every entry counts as locked, and a thread tries again to reach it; on Linux, the
/// Secret Service then tells.
fn keystore_locked() -> Arc<at_login::KeyringLocked> {
	let ready = Arc::new(AtomicBool::new(nibkeep_cli::platform_keystore()));
	if !ready.load(Ordering::SeqCst) {
		let ready = Arc::clone(&ready);
		std::thread::spawn(move || {
			while !nibkeep_cli::platform_keystore() {
				std::thread::sleep(KEYSTORE_LOOK);
			}
			ready.store(true, Ordering::SeqCst);
		});
	}
	Arc::new(move |service, account| {
		if !ready.load(Ordering::SeqCst) {
			return true;
		}
		#[cfg(all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		))]
		return linux::keyring_locked(service, account);
		#[cfg(not(all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)))]
		{
			let _ = (service, account);
			false
		}
	})
}

/// The wall clock, in milliseconds since 1970.
fn now() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |since| {
			u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
		})
}

#[cfg(all(
	unix,
	not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod linux {
	//! Linux and the other systems with a Secret Service and logind.

	use std::collections::HashMap;
	use std::ffi::OsString;
	use std::process::{Command, Stdio};
	use std::sync::Arc;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::Duration;

	use secret_service::EncryptionType;
	use secret_service::blocking::SecretService;

	/// How often it asks logind whether its session is closing.
	const SESSION_LOOK: Duration = Duration::from_secs(10);

	/// Whether the Secret Service holds the keystore's entry of `service` and `account`
	/// locked, as GNOME Keyring does after an automatic login: it lists entries locked and
	/// unlocked without a prompt, which reading the key would show. An entry it does not
	/// list, as another provider may while it is closed, counts as not locked, and opening
	/// the library then finds no key.
	pub(super) fn keyring_locked(service: &str, account: &str) -> bool {
		let Ok(secrets) = SecretService::connect(EncryptionType::Plain) else {
			return false;
		};
		let attributes = HashMap::from([("service", service), ("username", account)]);
		secrets
			.search_items(attributes)
			.is_ok_and(|found| found.unlocked.is_empty() && !found.locked.is_empty())
	}

	/// How many times in a row logind may know no such session before the service takes it
	/// that the session is gone, as logind may not answer for a moment, as while it restarts.
	const GONE_AFTER: u32 = 3;

	/// Sets `stop` once logind says that this program's session is closing, or knows it no
	/// more, three times in a row once it knew it: a system may leave a session's programs
	/// running after the person logs out, as logind does unless told to kill them. Where it
	/// knows no session, or there is no `loginctl`, it never does.
	pub(super) fn end_with_session(stop: Arc<AtomicBool>) {
		let Some(session) = std::env::var_os("XDG_SESSION_ID").filter(|id| !id.is_empty()) else {
			return;
		};
		std::thread::spawn(move || {
			let (mut known, mut unknown) = (false, 0);
			while !stop.load(Ordering::SeqCst) {
				match state(&session) {
					Err(()) => return,
					Ok(Some(state)) if state == "closing" => break,
					Ok(Some(_)) => (known, unknown) = (true, 0),
					Ok(None) => unknown += 1,
				}
				if known && unknown >= GONE_AFTER {
					break;
				}
				std::thread::sleep(SESSION_LOOK);
			}
			stop.store(true, Ordering::SeqCst);
		});
	}

	/// The state of a session, as logind tells it: `None` if it knows none such; an error if
	/// there is no `loginctl` to ask.
	fn state(session: &OsString) -> Result<Option<String>, ()> {
		let shown = Command::new("loginctl")
			.arg("show-session")
			.arg(session)
			.args(["--property=State", "--value"])
			.stdin(Stdio::null())
			.stderr(Stdio::null())
			.output()
			.map_err(|_| ())?;
		Ok(shown
			.status
			.success()
			.then(|| String::from_utf8_lossy(&shown.stdout).trim().to_owned()))
	}
}
