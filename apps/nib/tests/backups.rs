//! `nib backup`: backups on drives and shares, which the person's computers update
//! ([technical design: backups]).
//!
//! [technical design: backups]: ../../../docs/Technical-design.md#910-backups

mod common;

use std::fs;
use std::path::PathBuf;

use common::library;

/// An empty folder that a drive's folder stands in for.
fn drive(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, drive"))
}

#[test]
fn a_backup_is_made_listed_updated_changed_and_forgotten() {
	let mut test = library("backup commands");
	let files = drive("backup commands, files");
	fs::write(files.join("recipe.txt"), b"three apples").expect("writes");
	test.nib(&[
		"file",
		"add",
		files.join("recipe.txt").to_str().expect("UTF-8"),
	])
	.expect("adds");
	let folder = drive("backup commands").join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	let made = test.json(&["backup", "new", path, "--name", "Blue stick", "--away"]);
	assert_eq!(made["name"], "Blue stick");
	assert_eq!(made["away"], true);
	assert_eq!(made["updated"]["written"]["files"], 1);
	assert!(folder.join("README.txt").exists());
	// Listed where this computer finds it.
	let listed = test.json(&["backup", "list"]);
	assert_eq!(listed[0]["name"], "Blue stick");
	assert_eq!(listed[0]["files"], 1);
	assert!(listed[0]["found_at"].is_string());
	// An update with nothing new writes nothing.
	let updated = test.json(&["backup", "update", "Blue stick"]);
	assert_eq!(updated["updated"]["written"]["changes"], 0);
	assert_eq!(updated["updated"]["written"]["files"], 0);
	// Changed, then forgotten: the folder stays.
	let changed = test.json(&[
		"backup",
		"edit",
		"Blue stick",
		"--home",
		"--name",
		"Red stick",
	]);
	assert_eq!(changed["name"], "Red stick");
	assert_eq!(changed["away"], false);
	let forgot = test
		.nib(&["backup", "remove", "Red stick"])
		.expect("removes");
	assert!(
		forgot.starts_with("Forgot the backup Red stick"),
		"{forgot}"
	);
	assert!(folder.join("backup.cbor").exists());
	assert_eq!(test.json(&["backup", "list"]), serde_json::json!([]));
}

#[test]
fn a_backup_is_refused_in_a_folder_that_holds_files() {
	let mut test = library("backup refused");
	let folder = drive("backup refused");
	fs::write(folder.join("photo.jpg"), b"mine").expect("writes");
	let refused = test
		.nib(&["backup", "new", folder.to_str().expect("UTF-8")])
		.expect_err("refused");
	assert!(refused.to_string().contains("is not empty"), "{refused}");
}

#[test]
fn a_backup_waits_before_purging_and_a_purge_that_waits_there_is_stopped() {
	let mut test = library("backup waits");
	let files = drive("backup waits, files");
	fs::write(files.join("recipe.txt"), b"three apples").expect("writes");
	test.nib(&[
		"file",
		"add",
		files.join("recipe.txt").to_str().expect("UTF-8"),
	])
	.expect("adds");
	let folder = drive("backup waits").join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	let made = test.json(&[
		"backup",
		"new",
		path,
		"--name",
		"Blue stick",
		"--waits",
		"40",
	]);
	assert_eq!(
		(&made["waits"], &made["waits_days"]),
		(&40.into(), &40.into())
	);
	let changed = test
		.nib(&["backup", "edit", "Blue stick", "--waits", "default"])
		.expect("edits");
	assert!(
		changed.contains("waits 30 days before purging"),
		"{changed}"
	);
	// A purge says first that the backup waits; its next update keeps what it destroyed.
	let (said, _) = test.nib_printing(&["purge", "recipe.txt", "std.content="]);
	assert!(
		said.contains("waits     on Blue stick, a backup, for 30 days after its next update"),
		"{said}"
	);
	test.nib(&["purge", "recipe.txt", "std.content=", "--yes"])
		.expect("purges");
	let updated = test.json(&["backup", "update", "Blue stick"]);
	assert_eq!(updated["updated"]["waiting_purges"], 1);
	let listed = test.json(&["purges"]);
	let purges = listed["purges"].as_array().expect("a list");
	assert_eq!(purges.len(), 1, "{listed}");
	assert_eq!(purges[0]["backup_name"], "Blue stick");
	let purge = purges[0]["purge"].as_str().expect("an ID").to_owned();
	// Stopped there, the file comes back from the backup.
	let said = test
		.nib(&["purges", "stop", &purge, "--on", "Blue stick", "--yes"])
		.expect("stops");
	assert!(said.starts_with("Stopped the purge "), "{said}");
	let back = drive("backup waits, back");
	test.nib(&[
		"file",
		"checkout",
		"recipe.txt",
		back.to_str().expect("UTF-8"),
	])
	.expect("checks out");
	assert_eq!(
		fs::read(back.join("recipe.txt")).expect("reads"),
		b"three apples"
	);
}
