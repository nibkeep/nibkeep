//! `nibsync` as a program of its own: it tells its version, and of two copies, the one
//! that the list names runs, and the other gives way
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::Test;
use nibkeep_node::at_login::{
	AtLogin, ENGINE_VERSION, Served, ServiceStatus, Serving, service_running, service_version,
};

/// How long a test waits for a copy.
const PATIENCE: Duration = Duration::from_secs(60);

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
	let started = Instant::now();
	while !done() {
		assert!(started.elapsed() < PATIENCE, "{what}: not in time");
		std::thread::sleep(Duration::from_millis(50));
	}
}

/// The program that the tests built.
fn built() -> PathBuf {
	PathBuf::from(env!("CARGO_BIN_EXE_nibsync"))
}

/// A copy of the program in `folder`, as an app would ship one.
fn copy_into(folder: &Path) -> PathBuf {
	std::fs::create_dir_all(folder).expect("makes");
	let copy = folder.join(built().file_name().expect("a name"));
	if std::fs::hard_link(built(), &copy).is_err() {
		std::fs::copy(built(), &copy).expect("copies");
	}
	copy
}

/// A copy running on `state`, kept from the computer's keystore, killed when dropped.
struct Running(Child);

impl Running {
	fn start(program: &Path, state: &Path) -> Self {
		let child = Command::new(program)
			.env("NIBKEEP_STATE", state)
			.env("NIBKEEP_NO_KEYSTORE", "1")
			.env("NIBKEEP_NO_LOGIN_ENTRY", "1")
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
			.expect("starts");
		Self(child)
	}

	fn ended(&mut self) -> bool {
		self.0.try_wait().expect("waits").is_some()
	}
}

impl Drop for Running {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

#[test]
fn it_tells_its_version() {
	assert_eq!(service_version(&built()).as_deref(), Some(ENGINE_VERSION));
	assert_eq!(service_version(Path::new("/nowhere/nibsync")), None);
	let refused = Command::new(built())
		.arg("--sync")
		.stderr(Stdio::null())
		.status()
		.expect("runs");
	assert_eq!(refused.code(), Some(2));
}

#[test]
fn of_two_copies_the_one_the_list_names_runs() {
	// A library that is not here, which a copy keeps listed, and tries again now and then.
	let test = Test::new("two copies");
	let directory = std::path::absolute(test.here.join("gone").join(".nib")).expect("absolute");
	let state = test.here.join("state");
	let (editors, passwords) = (
		copy_into(&test.here.join("editor")),
		copy_into(&test.here.join("passwords")),
	);
	AtLogin::edit(&state, |list| {
		list.program = Some(editors.clone());
		list.add(Served {
			replica: nibkeep_base::EntityId::from_bytes([7; 16]),
			directory: directory.clone(),
		});
	})
	.expect("lists");
	let serving = || {
		ServiceStatus::read(&state)
			.expect("reads")
			.and_then(|status| status.of(&directory).cloned())
	};

	let mut first = Running::start(&editors, &state);
	wait_until("the named copy runs", || {
		serving() == Some(Serving::NotHere) && service_running(&state).expect("looks")
	});
	let mut other = Running::start(&passwords, &state);
	wait_until("a copy that the list does not name ends", || other.ended());
	assert!(!first.ended());

	AtLogin::edit(&state, |list| list.program = Some(passwords.clone())).expect("names");
	let mut second = Running::start(&passwords, &state);
	wait_until("the first ends once the list names the second", || {
		first.ended()
	});
	wait_until("the second runs", || {
		serving() == Some(Serving::NotHere) && service_running(&state).expect("looks")
	});
	assert!(!second.ended());

	// With no library left, it ends, and says nothing more.
	AtLogin::edit(&state, |list| list.libraries.clear()).expect("empties");
	wait_until("the second ends", || second.ended());
	assert!(!service_running(&state).expect("looks"));
	assert_eq!(ServiceStatus::read(&state).expect("reads"), None);
}
