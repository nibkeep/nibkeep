//! `nib file add --from` and `nib import`: files imported from a source the person names,
//! and Google Takeout's archives of Google Photos ([technical design: importers]).
//!
//! [technical design: importers]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#912-importers-and-where-files-came-from

mod common;

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use common::library;
use nibkeep_testkit::media::Photo;

fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn photo(taken: &'static str) -> Vec<u8> {
	Photo {
		make: "Google",
		model: "Pixel 7",
		taken,
		offset: Some("+01:00"),
		size: (4000, 3000),
		orientation: None,
		place: None,
	}
	.jpeg()
}

#[test]
fn files_come_from_a_named_source_and_a_takeout_with_its_albums() {
	let mut test = library("imports");
	// An old phone's folder, copied to the computer.
	let phone = folder("imports, phone");
	fs::create_dir_all(phone.join("DCIM/Camera")).expect("makes");
	fs::write(
		phone.join("DCIM/Camera/IMG_2041.jpg"),
		photo("2019:12:24 18:30:00"),
	)
	.expect("writes");
	let added = test.json(&[
		"file",
		"add",
		phone.join("DCIM").to_str().expect("UTF-8"),
		"--from",
		"Mom's old phone",
	]);
	assert_eq!(added["files"].as_array().map(Vec::len), Some(1), "{added}");
	let sources = test.json(&["list", "std.source"]);
	assert_eq!(sources.as_array().map(Vec::len), Some(1), "{sources}");

	// A Takeout of one album, with the same photo, and one of its own.
	let archive = folder("imports, takeout").join("takeout-001.zip");
	let mut zip = zip::ZipWriter::new(File::create(&archive).expect("creates"));
	let options = zip::write::SimpleFileOptions::default();
	let album = "Takeout/Google Photos/Christmas 2019";
	for (path, bytes) in [
		(format!("{album}/metadata.json"), br#"{"title":"Christmas 2019"}"#.to_vec()),
		(format!("{album}/IMG_2041.jpg"), photo("2019:12:24 18:30:00")),
		(
			format!("{album}/IMG_2041.jpg.json"),
			br#"{"title":"IMG_2041.jpg","photoTakenTime":{"timestamp":"1577208600"}}"#.to_vec(),
		),
		(format!("{album}/IMG_2042.jpg"), photo("2019:12:24 19:00:00")),
		(
			format!("{album}/IMG_2042.jpg.json"),
			br#"{"title":"IMG_2042.jpg","description":"The tree","photoTakenTime":{"timestamp":"1577210400"}}"#.to_vec(),
		),
	] {
		zip.start_file(path, options).expect("starts");
		zip.write_all(&bytes).expect("writes");
	}
	zip.finish().expect("finishes");
	let said = test
		.nib(&["import", archive.to_str().expect("UTF-8")])
		.expect("imports");
	assert!(
		said.starts_with("From Google Photos: 1 photo or video added, 1 found again that the library held, 0 from an earlier import\n1 album became a folder, inside the folder Google Photos\n"),
		"{said}"
	);
	let again = test.json(&["import", archive.to_str().expect("UTF-8")]);
	assert_eq!(
		(&again["added"], &again["known"]),
		(&0.into(), &2.into()),
		"{again}"
	);
	let sources = test.json(&["list", "std.source"]);
	assert_eq!(sources.as_array().map(Vec::len), Some(2), "{sources}");
	let folders = test.json(&["list", "std.folder"]);
	assert_eq!(
		folders.as_array().map(Vec::len),
		Some(2),
		"Google Photos and the album: {folders}"
	);
	// A file of another kind is no Takeout.
	let other = folder("imports, other").join("notes.txt");
	fs::write(&other, b"notes").expect("writes");
	assert!(
		test.nib(&["import", other.to_str().expect("UTF-8")])
			.is_err()
	);
}
