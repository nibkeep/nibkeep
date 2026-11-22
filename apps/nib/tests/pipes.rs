//! `nib` does its work whether or not what it prints is read: once nothing reads it, as
//! when `head` has read enough, it stops printing, says nothing of it, and finishes. And
//! what it prints of an error shows the characters that a terminal would hide.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The program itself, where it was built, with a library of its own: not on another device,
/// to which the tests go without it, and where `NIBKEEP_TEST_DIR` is set.
fn program(name: &str) -> Option<(PathBuf, PathBuf)> {
	if std::env::var_os("NIBKEEP_TEST_DIR").is_some() {
		eprintln!("skipped: the program is not on this device");
		return None;
	}
	let folder = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	// A passphrase in a file, so that no keystore is used, apart from the library's folder.
	let passphrase = folder.with_extension("passphrase");
	nibkeep_testkit::write_private(&passphrase, "correct horse\n");
	Some((folder, passphrase))
}

fn nib(folder: &Path, passphrase: &Path, arguments: &[&str]) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_nib"));
	command
		.arg("--library")
		.arg(folder)
		.arg("--passphrase-file")
		.arg(passphrase)
		.args(arguments)
		.stdin(Stdio::null());
	command
}

#[test]
fn a_command_whose_output_nothing_reads_finishes_without_an_error() {
	let Some((folder, passphrase)) = program("output nothing reads") else {
		return;
	};
	let started = nib(&folder, &passphrase, &["init", "--name", "Ada"])
		.output()
		.expect("runs");
	assert!(started.status.success(), "{started:?}");
	std::fs::write(folder.join("notes.txt"), "Milk.\n").expect("writes");
	std::fs::write(folder.join("todo.txt"), "Eggs.\n").expect("writes");
	// Nothing reads what it prints: the pipe's other end is closed before it starts.
	let (reader, writer) = std::io::pipe().expect("a pipe");
	drop(reader);
	let added = nib(&folder, &passphrase, &["file", "add", "."])
		.current_dir(&folder)
		.stdout(writer)
		.stderr(Stdio::piped())
		.output()
		.expect("runs");
	assert!(added.status.success(), "{added:?}");
	assert_eq!(String::from_utf8_lossy(&added.stderr), "");
	// Both files were added all the same.
	let listed = nib(&folder, &passphrase, &["list", "file"])
		.output()
		.expect("runs");
	let listed = String::from_utf8(listed.stdout).expect("UTF-8");
	assert!(
		listed.contains("notes.txt") && listed.contains("todo.txt"),
		"{listed}"
	);
}

/// A line that a script read on Windows ends in a carriage return, which a terminal hides:
/// the error names it, so that `local.person\r` is not taken for `local.person`.
#[test]
fn an_error_shows_the_characters_that_a_terminal_hides() {
	let Some((folder, passphrase)) = program("hidden characters") else {
		return;
	};
	let started = nib(&folder, &passphrase, &["init", "--name", "Ada"])
		.output()
		.expect("runs");
	assert!(started.status.success(), "{started:?}");
	let shown = nib(&folder, &passphrase, &["type", "show", "local.person\r"])
		.output()
		.expect("runs");
	assert!(!shown.status.success(), "{shown:?}");
	assert_eq!(
		String::from_utf8_lossy(&shown.stderr),
		"nib: local.person\\r is not a valid name\n"
	);
	// So is an escape, which could change what the terminal shows after it.
	let shown = nib(
		&folder,
		&passphrase,
		&["type", "show", "local.\u{1b}[2Jperson"],
	)
	.output()
	.expect("runs");
	assert!(!shown.status.success(), "{shown:?}");
	assert_eq!(
		String::from_utf8_lossy(&shown.stderr),
		"nib: local.\\u{1b}[2Jperson is not a valid name\n"
	);
	// And a zero-width space, which a name copied from a web page may hold.
	let shown = nib(
		&folder,
		&passphrase,
		&["type", "show", "local.person\u{200b}"],
	)
	.output()
	.expect("runs");
	assert!(!shown.status.success(), "{shown:?}");
	assert_eq!(
		String::from_utf8_lossy(&shown.stderr),
		"nib: local.person\\u{200b} is not a valid name\n"
	);
}
