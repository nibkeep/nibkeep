//! A `nib` that cargo starts in this workspace, as a build under development is, looks for
//! its library only below the workspace's folder, so that it never opens a library above it,
//! such as the project's own (the plan: guards for builds under development). The
//! workspace's `.cargo/config.toml` sets `NIBKEEP_CEILING` to the workspace's folder, and
//! empties `NIBKEEP_LIBRARY`, for whatever cargo starts in the workspace: these tests too.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Whether cargo started this test, which gives it what cargo tells a test about its crate
/// besides the workspace's `[env]`: not when the test runs on another device, nor when
/// something else, such as a debugger, starts it.
fn started_by_cargo() -> bool {
	if std::env::var_os("CARGO_MANIFEST_DIR").is_none() {
		eprintln!("skipped: cargo did not start this test");
		return false;
	}
	true
}

/// The workspace's folder, as `nib` resolves the folders it compares.
fn workspace() -> PathBuf {
	let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
	nibkeep_node::resolved_path(&folder).expect("the workspace's folder resolves")
}

#[test]
fn cargo_gives_a_ceiling_at_the_workspace_and_no_default_library() {
	if !started_by_cargo() {
		return;
	}
	let ceiling = std::env::var_os("NIBKEEP_CEILING").expect("cargo sets NIBKEEP_CEILING");
	let ceiling = PathBuf::from(ceiling);
	assert!(
		ceiling.is_absolute(),
		"{} is not a full path",
		ceiling.display()
	);
	assert_eq!(
		nibkeep_node::resolved_path(&ceiling).expect("the ceiling resolves"),
		workspace()
	);
	// Empty, whatever the shell says, which `nib` takes as naming no library.
	assert_eq!(
		std::env::var_os("NIBKEEP_LIBRARY").as_deref(),
		Some(std::ffi::OsStr::new(""))
	);
}

/// `nib` run in `here` with `arguments` and the environment that cargo gave this test, but
/// for `ceiling` if given, kept from the computer's keystore and its folder of state.
fn nib(test: &Path, here: &Path, ceiling: Option<&Path>, arguments: &[&str]) -> Output {
	let mut command = Command::new(env!("CARGO_BIN_EXE_nib"));
	command
		.current_dir(here)
		.env("NIBKEEP_NO_KEYSTORE", "1")
		.env("NIBKEEP_STATE", test.with_extension("state"))
		.arg("--passphrase-file")
		.arg(test.with_extension("passphrase"))
		.args(arguments)
		.stdin(Stdio::null());
	if let Some(ceiling) = ceiling {
		command.env("NIBKEEP_CEILING", ceiling);
	}
	command.output().expect("runs")
}

#[test]
fn a_build_under_development_finds_a_scratch_library_below_its_ceiling_and_none_above() {
	if !started_by_cargo() {
		return;
	}
	let test = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "a scratch library");
	nibkeep_testkit::write_private(&test.with_extension("passphrase"), "correct horse\n");
	let started = nib(&test, &test, None, &["init", "--name", "Ada"]);
	assert!(started.status.success(), "{started:?}");
	let deeper = test.join("Books").join("Read");
	std::fs::create_dir_all(&deeper).expect("makes the folders");
	// With the ceiling that cargo gives, at the workspace's folder: a library in the
	// workspace, such as this one under target, is found from the folders in it.
	let resolved = nibkeep_node::resolved_path(&test).expect("the test's folder resolves");
	if resolved.starts_with(workspace()) {
		let status = nib(&test, &deeper, None, &["status"]);
		assert!(status.status.success(), "{status:?}");
	} else {
		eprintln!(
			"the target folder is outside the workspace, where cargo's ceiling counts for nothing"
		);
	}
	// With a ceiling below the library, as the workspace is below the project's library, the
	// library above it is not found, and as cargo empties NIBKEEP_LIBRARY, none is opened in
	// its place.
	let ceiling = test.join("Books");
	let status = nib(&test, &deeper, Some(&ceiling), &["status"]);
	assert!(!status.status.success(), "{status:?}");
	assert_eq!(
		String::from_utf8(status.stderr).expect("UTF-8").trim(),
		"nib: this folder is not in a library; if it is a tree's folder that lost its .nib file, run nib init FOLDER in your library's folder; otherwise make a library with nib init, or give one with --library or NIBKEEP_LIBRARY"
	);
}
