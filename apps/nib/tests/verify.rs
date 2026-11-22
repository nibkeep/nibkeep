//! `nib verify`: checking the library.

mod common;

use common::library;
use serde_json::json;

#[test]
fn a_sound_library_is_found_sound() {
	let mut test = library("verify");
	test.nib(&["new", "person", "name=Ada Lovelace"])
		.expect("adds");
	assert_eq!(
		test.nib(&["verify"]).expect("verifies"),
		"Personal: 8 changes and 0 blobs checked, sound\n"
	);
	let report = test.json(&["verify"]);
	assert_eq!(report["sound"], json!(true));
	assert_eq!(report["spaces"][0]["name"], "Personal");
	assert_eq!(report["spaces"][0]["changes"], 8);
	assert_eq!(report["spaces"][0]["problems"], json!([]));
}

#[test]
fn a_damaged_blob_is_moved_aside_and_shown_until_it_comes_again() {
	let mut test = library("verify damaged");
	let scan = test.here.join("scan.png");
	std::fs::write(&scan, vec![9; 20_000]).expect("writes");
	test.nib(&["file", "add", &scan.to_string_lossy()])
		.expect("adds");
	// A bit flips in the one sealed copy.
	let replica =
		nibkeep_node::Library::replica_folder(&test.here.join(".nib")).expect("a replica");
	let blobs = replica.join("blobs");
	let file = walk(&blobs).into_iter().next().expect("a sealed blob");
	let mut bytes = std::fs::read(&file).expect("reads");
	bytes[1_000] ^= 1;
	std::fs::write(&file, &bytes).expect("writes");
	let (printed, verified) = test.nib_printing(&["verify"]);
	assert!(verified.is_err(), "{printed}");
	assert!(
		printed.contains(
			": it is moved aside, and the next sync brings it again from a device that holds it intact\n"
		),
		"{printed}"
	);
	assert!(!file.exists(), "moved aside");
	let status = test.nib(&["status"]).expect("describes");
	assert!(
		status.contains("  scan.png (Personal): its sealed copy is damaged, found 2026-10-12 10:00, and waits for an intact copy from another device\n"),
		"{status}"
	);
	let json = test.json(&["status"]);
	assert_eq!(json["checks"]["damaged"][0]["replaced"], json!(false));
	// Checking again still finds it damaged: no other device has brought it yet.
	let (printed, verified) = test.nib_printing(&["verify"]);
	assert!(verified.is_err(), "{printed}");
	assert!(
		printed.contains("is damaged: it is moved aside"),
		"{printed}"
	);
}

/// The files under a folder.
fn walk(folder: &std::path::Path) -> Vec<std::path::PathBuf> {
	let mut files = Vec::new();
	let mut folders = vec![folder.to_path_buf()];
	while let Some(folder) = folders.pop() {
		for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
			let path = entry.path();
			if path.is_dir() {
				folders.push(path);
			} else {
				files.push(path);
			}
		}
	}
	files
}
