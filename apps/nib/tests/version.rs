//! `nib --version` prints the name, the version, and the commit it was built from.

use std::path::Path;
use std::process::Command;

use clap::Parser;
use nibkeep_cli::Cli;

fn expected() -> String {
	format!("nib {}", env!("NIB_VERSION"))
}

#[test]
fn prints_name_and_version() {
	let error = Cli::try_parse_from(["nib", "--version"]).expect_err("prints the version");
	assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
	assert_eq!(error.render().to_string().trim(), expected());
}

/// The version names the commit, by the first 12 digits of its hash, and `+changes` if the
/// code differed from it; or says it is unknown.
#[test]
fn the_version_names_the_commit() {
	let version = env!("NIB_VERSION");
	let commit = version
		.strip_prefix(&format!("{} (", env!("CARGO_PKG_VERSION")))
		.and_then(|rest| rest.strip_suffix(')'))
		.expect("the version, then the commit in parentheses");
	if commit == "unknown commit" {
		return;
	}
	let hash = commit.strip_suffix("+changes").unwrap_or(commit);
	assert_eq!(hash.len(), 12, "{commit}");
	assert!(
		hash.chars()
			.all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase())
	);
}

/// What Git prints for these arguments in the workspace, if it runs and succeeds.
fn git(arguments: &[&str]) -> Option<String> {
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	let output = Command::new("git")
		.args(arguments)
		.current_dir(workspace)
		.output()
		.ok()?;
	output
		.status
		.success()
		.then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Where the workspace is a repository of its own, the commit is its own.
#[test]
fn the_commit_is_the_repositorys() {
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	let own = git(&["rev-parse", "--show-toplevel"]).is_some_and(|top| {
		std::fs::canonicalize(top).ok() == std::fs::canonicalize(&workspace).ok()
	});
	let Some(head) = git(&["rev-parse", "HEAD"]).filter(|_| own) else {
		eprintln!("skipped: the workspace is no repository of its own here");
		return;
	};
	let short = &head[..12];
	assert!(
		env!("NIB_VERSION").contains(&format!("({short}")),
		"{} is not of {short}",
		env!("NIB_VERSION")
	);
}

/// The program itself, where it was built: not on another device, to which the tests go
/// without it, and where `NIBKEEP_TEST_DIR` is set.
#[test]
fn the_program_prints_name_and_version() {
	if std::env::var_os("NIBKEEP_TEST_DIR").is_some() {
		eprintln!("skipped: the program is not on this device");
		return;
	}
	let program = Path::new(env!("CARGO_BIN_EXE_nib"));
	let output = Command::new(program)
		.arg("--version")
		.output()
		.expect("the nib binary runs");
	assert!(output.status.success());
	let stdout = String::from_utf8(output.stdout).expect("output is UTF-8");
	assert_eq!(stdout.trim(), expected());
}
