//! Secret files in `nib`: `nib file add --secret` seals them for the secrets, attached to
//! records with `--to`, and `nib secret save` writes plain copies, which nothing records
//! ([technical design: secret files]).
//!
//! [technical design: secret files]: ../../../docs/Technical-design.md#95-secret-files

mod common;

use std::fs;

use clap::Parser;
use common::{Test, library};
use nibkeep_cli::{Cli, CliError};
use nibkeep_node::{KeysError, NodeError};

/// The secrets' passphrase in these tests.
const SECRETS: &str = "tr0ub4dor";

/// What no file of the library may hold in plain.
const SENTINEL: &str = "s3ntinel-scan";

/// A scan, as a PDF's first bytes tell it.
fn scan() -> Vec<u8> {
	let mut scan = b"%PDF-1.7\n".to_vec();
	scan.extend(
		format!("a scan of a passport, {SENTINEL} ")
			.repeat(500)
			.as_bytes(),
	);
	scan
}

/// A library with a login, and a scan in the current folder.
fn with_scan(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["new", "std.login", "name=Bank"])
		.expect("adds the login");
	fs::write(test.here.join("scan.pdf"), scan()).expect("writes");
	test
}

/// The lines of the output with the short ID after their first word replaced by `<id>`.
fn short_ids_hidden(output: &str) -> String {
	output
		.lines()
		.map(|line| {
			let word = line.find(' ').unwrap_or(line.len());
			let start = word + (line[word..].len() - line[word..].trim_start().len());
			match line.get(start..start + 8) {
				Some(id)
					if id.chars().all(|c| c.is_ascii_hexdigit())
						&& line[start + 8..].starts_with(' ') =>
				{
					format!("{}<id>{}\n", &line[..start], &line[start + 8..])
				}
				_ => format!("{line}\n"),
			}
		})
		.collect()
}

/// Adds a file of the current folder as an ordinary file, by its whole path.
fn add_ordinary(test: &mut Test, name: &str) {
	let path = test.here.join(name);
	test.nib(&["file", "add", path.to_str().expect("UTF-8")])
		.expect("adds an ordinary file");
}

/// Adds the scan as a secret file attached to the login, setting up the secrets.
fn add_scan(test: &mut Test) -> String {
	test.answer(&[SECRETS, SECRETS]);
	test.nib(&["file", "add", "--secret", "--to", "Bank", "scan.pdf"])
		.expect("adds")
}

#[test]
fn a_secret_file_is_added_sealed_and_attached_to_a_record() {
	let mut test = with_scan("nib secret file");
	let printed = add_scan(&mut test);
	let path = test.here.join("scan.pdf");
	assert_eq!(
		short_ids_hidden(&printed),
		format!(
			"secret <id>  {}, attached to Bank\n\
			Added 1 secret file, sealed for the secrets. Its original is still where it was, unprotected.\n",
			path.display()
		)
	);
	assert!(path.exists(), "nib never deletes the original");

	let shown = test.nib(&["show", "scan.pdf"]).expect("shows");
	assert!(shown.contains("std.secret-file"), "{shown}");
	assert!(shown.contains("application/pdf"), "{shown}");
	assert!(shown.contains("Bank"), "{shown}");
	let login = test.nib(&["show", "Bank"]).expect("shows");
	assert!(
		login.contains("scan.pdf"),
		"the login shows what is attached: {login}"
	);

	// Nothing in the library holds the scan in plain, nor its hash.
	let library = test.here.join(".nib");
	assert!(!nibkeep_testkit::holds_anywhere(
		&library,
		SENTINEL.as_bytes()
	));
	let hash = nibkeep_base::Hash::of(&scan());
	assert!(!nibkeep_testkit::holds_anywhere(
		&library,
		&hash.to_multihash()
	));
	assert!(!nibkeep_testkit::holds_anywhere(
		&library,
		hash.to_string().as_bytes()
	));
	assert!(!shown.contains(&hash.to_string()), "{shown}");
}

#[test]
fn adding_secret_files_says_what_it_did_for_programs() {
	let mut test = with_scan("nib secret file json");
	fs::write(test.here.join("again.pdf"), scan()).expect("writes");
	add_ordinary(&mut test, "again.pdf");
	test.answer(&[SECRETS, SECRETS]);
	let json = test.json(&["file", "add", "--secret", "scan.pdf"]);
	let added = &json["secret_files"][0];
	assert_eq!(added["name"], "scan.pdf");
	assert_eq!(added["media_type"], "application/pdf");
	assert_eq!(added["size"], scan().len());
	assert_eq!(added["attached_to"], serde_json::json!([]));
	assert_eq!(
		added["unprotected_copy"], true,
		"an ordinary file holds it too"
	);
	assert!(
		!json
			.to_string()
			.contains(&nibkeep_base::Hash::of(&scan()).to_string())
	);
}

#[test]
fn an_ordinary_copy_of_the_content_is_reported() {
	let mut test = with_scan("nib secret file held plainly");
	add_ordinary(&mut test, "scan.pdf");
	test.answer(&[SECRETS, SECRETS]);
	let printed = test
		.nib(&["file", "add", "--secret", "scan.pdf"])
		.expect("adds");
	assert!(
		printed.contains(", also in the library as an ordinary file, which every device reads"),
		"{printed}"
	);
}

#[test]
fn secret_files_are_added_one_file_at_a_time() {
	let mut test = with_scan("nib secret folder");
	fs::create_dir(test.here.join("scans")).expect("makes a folder");
	test.answer(&[SECRETS, SECRETS]);
	let added = test.nib(&["file", "add", "--secret", "scans"]);
	assert!(matches!(added, Err(CliError::SecretFolder(_))), "{added:?}");
	// --to is for secret files.
	assert!(Cli::try_parse_from(["nib", "file", "add", "--to", "Bank", "scan.pdf"]).is_err());
}

#[test]
fn a_file_that_cannot_be_read_is_reported_and_the_others_are_added() {
	let mut test = with_scan("nib secret file unreadable");
	test.answer(&[SECRETS, SECRETS]);
	let (printed, added) =
		test.nib_printing(&["file", "add", "--secret", "missing.pdf", "scan.pdf"]);
	assert!(matches!(added, Err(CliError::FilesFailed(1))), "{added:?}");
	assert!(
		printed.starts_with(&format!(
			"failed {}: ",
			test.here.join("missing.pdf").display()
		)),
		"{printed}"
	);
	assert!(printed.contains("Added 1 secret file"), "{printed}");
	let shown = test.nib(&["show", "scan.pdf"]).expect("shows");
	assert!(shown.contains("std.secret-file"), "{shown}");
}

#[test]
fn a_saved_copy_is_written_and_nothing_records_it() {
	let mut test = with_scan("nib secret save");
	add_scan(&mut test);
	fs::create_dir(test.here.join("saved")).expect("makes a folder");
	test.answer(&[SECRETS]);
	let printed = test
		.nib(&["secret", "save", "scan.pdf", "saved"])
		.expect("saves");
	let saved = fs::canonicalize(test.here.join("saved"))
		.expect("resolves")
		.join("scan.pdf");
	// As nib names it: without the `\\?\` of a canonical path on Windows.
	let named = saved.display().to_string();
	let named = named.strip_prefix(r"\\?\").unwrap_or(&named);
	assert_eq!(
		printed,
		format!(
			"Saved a plain copy of scan.pdf at {named}. NibKeep neither protects it, nor keeps track of it, nor deletes it if the file is purged.\n"
		)
	);
	assert_eq!(fs::read(&saved).expect("reads"), scan());
	let status = test.nib(&["status"]).expect("shows");
	assert!(!status.contains("checkout"), "{status}");

	// A copy replaces nothing; with --json, nib says where it went.
	test.answer(&[SECRETS]);
	let again = test.nib(&["secret", "save", "scan.pdf", "saved"]);
	assert!(
		matches!(again, Err(CliError::Node(NodeError::PlaceTaken(_)))),
		"{again:?}"
	);
	test.answer(&[SECRETS]);
	let json = test.json(&["secret", "save", "scan.pdf", "copy.pdf"]);
	assert_eq!(
		json["path"],
		test.here.join("copy.pdf").display().to_string()
	);
	assert_eq!(fs::read(test.here.join("copy.pdf")).expect("reads"), scan());

	// A secret file has no checkouts.
	let checkout = test.nib(&["file", "checkout", "scan.pdf", "checked-out.pdf"]);
	assert!(
		matches!(checkout, Err(CliError::SecretFileCheckout(_))),
		"{checkout:?}"
	);
	assert!(!test.here.join("checked-out.pdf").exists());
}

#[test]
fn saving_needs_the_secrets_passphrase() {
	let mut test = with_scan("nib secret save locked");
	add_scan(&mut test);
	test.answer(&["not the passphrase"]);
	let saved = test.nib(&["secret", "save", "scan.pdf", "copy.pdf"]);
	assert!(
		matches!(
			saved,
			Err(CliError::Node(NodeError::Keys(KeysError::WrongPassphrase)))
		),
		"{saved:?}"
	);
	assert!(!test.here.join("copy.pdf").exists());
	// A login is no secret file.
	test.answer(&[SECRETS]);
	let login = test.nib(&["secret", "save", "Bank", "login.pdf"]);
	assert!(
		matches!(login, Err(CliError::Node(NodeError::NotSecretFile(_)))),
		"{login:?}"
	);
}
