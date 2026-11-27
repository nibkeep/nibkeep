//! The keystore that the app's hook gives: looked for again while the hook gives none, as
//! on a phone before the app's code gave it what it needs, kept once found, and kept away
//! for good where tests set `NIBKEEP_NO_KEYSTORE`. A program of its own, as it sets the
//! process's default keystore, whose tests each run in a child of their own.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use keyring_core::CredentialStore;
use nibkeep_app::{App, keystore};

/// Set in the child that runs a test, each in a process of its own.
const CHILD: &str = "NIBKEEP_APP_KEYSTORE_CHILD";

/// Whether the app's hook gives a keystore yet.
static READY: AtomicBool = AtomicBool::new(false);

/// How many times the app's hook was asked.
static ASKED: AtomicUsize = AtomicUsize::new(0);

/// The app's hook: a keystore in memory, once it is ready.
fn hook() -> Option<Arc<CredentialStore>> {
	ASKED.fetch_add(1, Ordering::SeqCst);
	READY
		.load(Ordering::SeqCst)
		.then(|| -> Arc<CredentialStore> {
			keyring_core::mock::Store::new().expect("a mock store")
		})
}

/// The app of the tests: it reads no secrets.
static APP: App = App {
	name: "Example",
	keystore: hook,
	secrets: None,
	let_go: || {},
};

/// Runs this test again in a child, with `NIBKEEP_NO_KEYSTORE` set if `kept_away`, or
/// without it whatever the tests run with, unless this is that child: whether it is.
fn in_child(kept_away: bool) -> bool {
	if std::env::var_os(CHILD).is_some() {
		return true;
	}
	let mut child = Command::new(std::env::current_exe().expect("this test"));
	child
		.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
		.env(CHILD, "1");
	if kept_away {
		child.env("NIBKEEP_NO_KEYSTORE", "1");
	} else {
		child.env_remove("NIBKEEP_NO_KEYSTORE");
	}
	let ran = child.output().expect("runs the child");
	let said = format!(
		"{}{}",
		String::from_utf8_lossy(&ran.stdout),
		String::from_utf8_lossy(&ran.stderr)
	);
	assert!(ran.status.success(), "{said}");
	assert!(said.contains("1 passed"), "the child ran the test: {said}");
	false
}

#[test]
fn keystore_is_looked_for_until_the_app_gives_one_and_kept_then() {
	if !in_child(false) {
		return;
	}
	assert!(!keystore::keystore(), "none before the app names itself");
	assert_eq!(ASKED.load(Ordering::SeqCst), 0);
	nibkeep_app::start(&APP);
	assert!(!keystore::keystore(), "none yet");
	assert!(!keystore::keystore(), "still none");
	assert_eq!(ASKED.load(Ordering::SeqCst), 2, "asked each time");
	READY.store(true, Ordering::SeqCst);
	assert!(keystore::keystore(), "the one given");
	READY.store(false, Ordering::SeqCst);
	assert!(keystore::keystore(), "kept");
	assert_eq!(ASKED.load(Ordering::SeqCst), 3, "not asked again");
}

#[test]
fn keystore_is_kept_away_where_tests_say_so() {
	if !in_child(true) {
		return;
	}
	READY.store(true, Ordering::SeqCst);
	nibkeep_app::start(&APP);
	assert!(!keystore::keystore(), "none, though the app gives one");
	assert!(!keystore::keystore(), "for good");
	assert_eq!(ASKED.load(Ordering::SeqCst), 0, "the app is never asked");
}
