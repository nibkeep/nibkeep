//! The tests' switch for the keystore, `NIBKEEP_NO_KEYSTORE`: with it, `nib` finds no keystore,
//! where it would otherwise move a library's keys into the computer's. The end-to-end tests
//! ([technical design](../../../docs/Technical-design.md#18-end-to-end-tests)) and the apps' tests rely
//! on it to keep away from the person's keystore. This test runs the program built with it,
//! never an older one, which would not know the switch. On Linux, it also takes the session
//! bus away, so that a failing switch cannot reach the Secret Service. On Windows and macOS
//! nothing else fences the keystore, so there it runs only in CI, whose machines are thrown
//! away after each run: that is where it proves the switch.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn nib(folder: &Path, arguments: &[&str]) -> Output {
	Command::new(env!("CARGO_BIN_EXE_nib"))
		.arg("--library")
		.arg(folder.join("library"))
		.args(arguments)
		.env("NIBKEEP_NO_KEYSTORE", "1")
		.env("NIBKEEP_PASSPHRASE_FILE", folder.join("passphrase"))
		.env(
			"DBUS_SESSION_BUS_ADDRESS",
			"unix:path=/nonexistent/nibkeep-no-bus",
		)
		.stdin(Stdio::null())
		.output()
		.expect("runs")
}

#[test]
fn nib_finds_no_keystore_where_tests_say_so() {
	if std::env::var_os("NIBKEEP_TEST_DIR").is_some() {
		eprintln!("skipped: the program is not on this device");
		return;
	}
	if !cfg!(target_os = "linux") && std::env::var_os("CI").is_none() {
		eprintln!("skipped: outside CI, a failing switch would reach this computer's keystore");
		return;
	}
	let folder = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "no keystore");
	std::fs::create_dir(folder.join("library")).expect("makes the library's folder");
	nibkeep_testkit::write_private(&folder.join("passphrase"), "correct horse\n");
	let started = nib(&folder, &["init", "--name", "Ada", "--languages", "en"]);
	assert!(started.status.success(), "{started:?}");
	let protected = nib(&folder, &["protect"]);
	assert_eq!(protected.status.code(), Some(1), "{protected:?}");
	assert_eq!(
		String::from_utf8_lossy(&protected.stderr),
		"nib: there is no platform keystore here; nib protect --passphrase protects with a passphrase\n"
	);
}
