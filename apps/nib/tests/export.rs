//! `nib export`: the personal space in ordinary files, JSON and SHA-256 sums.

use std::fs;

use crate::common::library;
use nibkeep_cli::CliError;
use nibkeep_node::NodeError;

#[test]
fn export_writes_a_folder_that_reads_without_nibkeep() {
	let mut test = library("export");
	test.nib(&["new", "book", "name=Leviathan Wakes"])
		.expect("adds a book");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "export, its folder")
			.join("export");
	let folder_text = folder.to_str().expect("a path as text");
	let exported = test.nib(&["export", folder_text]).expect("exports");
	let entities = fs::read_to_string(folder.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.count();
	assert_eq!(
		exported,
		format!("Exported {entities} entities and 0 files, with their history, to {folder_text}\n")
	);
	for file in ["README.txt", "manifest.json", "SHA256SUMS"] {
		assert!(folder.join(file).is_file(), "{file}");
	}
	assert!(
		fs::read_to_string(folder.join("entities.jsonl"))
			.expect("entities")
			.contains("\"Leviathan Wakes\"")
	);

	// Into a folder that holds an export already, it writes nothing.
	assert_eq!(
		test.nib(&["export", folder_text])
			.expect_err("holds files")
			.to_string(),
		format!(
			"{folder_text} holds files already; an export goes into an empty directory or a new one"
		)
	);

	let current = folder.with_file_name("current");
	let current_text = current.to_str().expect("a path as text");
	let report = test.json(&["export", current_text, "--current"]);
	assert_eq!(report["history"], false);
	assert_eq!(report["directory"], current_text);
	assert_eq!(report["files"], 0);
	assert_eq!(report["missing_files"], serde_json::json!([]));
	assert!(
		report["entities"]
			.as_u64()
			.is_some_and(|entities| entities > 0)
	);
	let manifest: serde_json::Value =
		serde_json::from_str(&fs::read_to_string(current.join("manifest.json")).expect("manifest"))
			.expect("JSON");
	assert_eq!(manifest["history"], false);
}

#[test]
fn verify_compares_an_export_with_the_library() {
	let mut test = library("verify export");
	test.nib(&["new", "book", "name=Leviathan Wakes"])
		.expect("adds a book");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "verify export, its folder")
			.join("export");
	let folder_text = folder.to_str().expect("a path as text");
	test.nib(&["export", folder_text]).expect("exports");
	let entities = fs::read_to_string(folder.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.count();
	let verified = test
		.nib(&["verify", "--export", folder_text])
		.expect("verifies");
	assert_eq!(
		verified,
		format!(
			"Export in {folder_text}: {entities} entities and 0 files, as the library held them at 2026-10-12T10:00:00.000Z\n"
		)
	);
	let report = test.json(&["verify", "--export", folder_text]);
	assert_eq!(report["sound"], true);
	assert_eq!(report["problems"], serde_json::json!([]));

	// An export changed by hand is found out, and the command fails.
	let lines = fs::read_to_string(folder.join("entities.jsonl")).expect("entities");
	fs::write(
		folder.join("entities.jsonl"),
		lines.replacen("Leviathan Wakes", "Leviathan Sleeps", 1),
	)
	.expect("writes");
	let (printed, result) = test.nib_printing(&["verify", "--export", folder_text]);
	assert_eq!(
		result.expect_err("unsound").to_string(),
		"the export has 2 problems"
	);
	assert!(
		printed.contains("  entities.jsonl: its sum differs\n"),
		"{printed}"
	);
	assert!(
		printed.contains(": its line differs from the library's\n"),
		"{printed}"
	);
	fs::remove_file(folder.join("changes.jsonl")).expect("removes");
	let (printed, _) = test.nib_printing(&["verify", "--export", folder_text]);
	assert!(
		printed.contains("  changes.jsonl: it differs from the library's changes\n"),
		"{printed}"
	);
	let (printed, _) = test.nib_printing(&["--json", "verify", "--export", folder_text]);
	let report: serde_json::Value = serde_json::from_str(&printed).expect("JSON");
	assert!(
		report["problems"]
			.as_array()
			.expect("problems")
			.contains(&serde_json::json!({ "problem": "different changes" })),
		"{report}"
	);
}

#[test]
fn import_brings_an_export_into_another_library() {
	let mut from = library("import, from");
	from.nib(&["new", "book", "name=Leviathan Wakes"])
		.expect("adds a book");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "import, its folder")
			.join("export");
	let folder_text = folder.to_str().expect("a path as text");
	from.nib(&["export", folder_text]).expect("exports");

	let mut into = library("import, into");
	let imported = into.nib(&["import", folder_text]).expect("imports");
	assert!(
		imported.starts_with(&format!("Imported from {folder_text}: ")),
		"{imported}"
	);
	assert!(imported.contains("left out"), "{imported}");
	assert_eq!(
		into.json(&["show", "Leviathan Wakes"])["id"],
		from.json(&["show", "Leviathan Wakes"])["id"]
	);
	// Again, it adds nothing.
	let again = into.json(&["import", folder_text]);
	assert_eq!(again["added"], 0);
	assert_eq!(again["replaced"], 0);
	assert_eq!(again["files"], 0);
	// An export keeps where its files came from, which only Takeout's photos are given.
	assert!(matches!(
		into.nib(&["import", folder_text, "--from", "Mom's old phone"]),
		Err(CliError::Import(_))
	));
	// An export that lost its manifest is still read as one, and says what it lacks, rather
	// than taken for Takeout's archive.
	fs::remove_file(folder.join("manifest.json")).expect("removes the manifest");
	let damaged = into.nib(&["import", folder_text]);
	assert!(
		matches!(damaged, Err(CliError::Node(NodeError::ExportDamaged(_)))),
		"{damaged:?}"
	);
}
