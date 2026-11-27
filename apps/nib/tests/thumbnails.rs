//! Thumbnails in nib: made as pictures are added and checked in, of the formats nib reads,
//! shown, written, counted, purged with their content, and made where files lack them
//! ([technical design: thumbnails]).
//!
//! [technical design: thumbnails]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#98-thumbnails

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::common::library;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use nibkeep_cli::CliError;

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn text(path: &Path) -> &str {
	path.to_str().expect("UTF-8")
}

/// A picture whose top is one colour and bottom blue, as a PNG.
fn picture(width: u32, height: u32, top: [u8; 3]) -> Vec<u8> {
	let picture = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |_, y| {
		if y < height / 2 {
			Rgb(top)
		} else {
			Rgb([30, 30, 220])
		}
	}));
	let mut bytes = Vec::new();
	picture
		.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
		.expect("encodes");
	bytes
}

/// The colour at the top of a JPEG.
fn top_of(jpeg: &[u8]) -> [u8; 3] {
	image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg)
		.expect("a JPEG")
		.to_rgb8()
		.get_pixel(10, 10)
		.0
}

fn alike(one: [u8; 3], other: [u8; 3]) -> bool {
	one.iter()
		.zip(other)
		.all(|(one, other)| one.abs_diff(other) <= 24)
}

#[test]
fn a_picture_added_gets_a_thumbnail_that_nib_shows_writes_and_counts() {
	let mut test = library("thumbnails shown");
	let files = folder("thumbnails shown");
	fs::write(files.join("sea.png"), picture(1600, 1200, [220, 30, 30])).expect("writes");
	fs::write(files.join("notes.txt"), "Milk.\n").expect("writes");
	// The start of a HEIC photo, as an iPhone writes it, which nib does not read.
	let mut heic = vec![0, 0, 0, 24];
	heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
	heic.resize(4096, 7);
	fs::write(files.join("photo.heic"), heic).expect("writes");
	let said = test.nib(&["file", "add", text(&files)]).expect("adds");
	assert!(
		said.ends_with("Added 3 files, made 1 thumbnail\n"),
		"{said}"
	);
	let shown = test.nib(&["show", "sea.png"]).expect("shows");
	assert!(
		shown
			.lines()
			.any(|line| line.trim_start().starts_with("thumbnail ")
				&& line.contains(" 512×384 pixels, made with nibkeep 1, ")),
		"{shown}"
	);
	let json = test.json(&["show", "sea.png"]);
	assert_eq!(json["thumbnail"]["width"], 512, "{json}");
	assert_eq!(json["thumbnail"]["made_with"], "nibkeep 1", "{json}");
	assert_eq!(json["thumbnail"]["held"], true, "{json}");

	let out = files.join("sea, thumbnail.jpg");
	let wrote = test
		.nib(&["file", "thumbnail", "sea.png", text(&out)])
		.expect("writes");
	assert!(
		wrote.starts_with("Wrote the thumbnail, 512×384 pixels, made with nibkeep 1, "),
		"{wrote}"
	);
	assert!(alike(
		top_of(&fs::read(&out).expect("reads")),
		[220, 30, 30]
	));
	// It replaces nothing.
	assert!(matches!(
		test.nib(&["file", "thumbnail", "sea.png", text(&out)]),
		Err(CliError::Exists(_))
	));
	let other = files.join("photo, thumbnail.jpg");
	assert!(matches!(
		test.nib(&["file", "thumbnail", "photo.heic", text(&other)]),
		Err(CliError::NoThumbnail(_))
	));
	assert!(!other.exists());

	let status = test.nib(&["status"]).expect("says");
	assert!(
		status.contains(", 2 pictures and videos, 1 with a thumbnail\n"),
		"{status}"
	);
	let status = test.json(&["status"]);
	assert_eq!(status["spaces"][0]["pictures"], 2, "{status}");
	assert_eq!(status["spaces"][0]["thumbnails"], 1, "{status}");
	// Nothing lacks one that nib could make.
	assert_eq!(
		test.nib(&["file", "thumbnails"]).expect("makes"),
		"Made 0 thumbnails\n"
	);
}

#[test]
fn a_new_version_checked_in_gets_a_thumbnail_of_its_own() {
	let mut test = library("thumbnails checked in");
	let files = folder("thumbnails checked in");
	let path = files.join("sketch.png");
	fs::write(&path, picture(800, 600, [220, 30, 30])).expect("writes");
	test.nib(&["file", "add", text(&path)]).expect("adds");
	let out = folder("thumbnails checked in, out");
	test.nib(&["file", "checkout", "sketch.png", text(&out)])
		.expect("checks out");
	let copy = out.join("sketch.png");
	fs::write(&copy, picture(800, 600, [30, 220, 30])).expect("edits");
	test.nib(&["file", "checkin", text(&copy)])
		.expect("checks in");
	let written = files.join("sketch, thumbnail.jpg");
	test.nib(&["file", "thumbnail", "sketch.png", text(&written)])
		.expect("writes");
	assert!(alike(
		top_of(&fs::read(&written).expect("reads")),
		[30, 220, 30]
	));
	let status = test.json(&["status"]);
	assert_eq!(status["spaces"][0]["thumbnails"], 1, "{status}");
}

#[test]
fn a_purge_names_the_thumbnails_it_takes_and_one_purged_alone_is_made_again() {
	let mut test = library("thumbnails purged");
	let files = folder("thumbnails purged");
	fs::write(files.join("beach.png"), picture(900, 600, [220, 30, 30])).expect("writes");
	fs::write(files.join("hill.png"), picture(900, 600, [30, 220, 30])).expect("writes");
	test.nib(&["file", "add", text(&files)]).expect("adds");

	let thumbnail = test.json(&["show", "beach.png"])["thumbnail"]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let (said, result) = test.nib_printing(&["purge", "beach.png", "std.content="]);
	assert!(matches!(result, Err(CliError::Unconfirmed)), "{result:?}");
	assert!(
		said.contains(&format!(
			"thumbnail {}, made of that content, would be purged with it\n",
			&thumbnail[..8]
		)),
		"{said}"
	);
	let purged = test.json(&["purge", "beach.png", "std.content=", "--yes"]);
	assert_eq!(purged["thumbnails"][0], thumbnail.as_str(), "{purged}");

	// A thumbnail purged alone, as a person may, is made again only when asked.
	let other = test.json(&["show", "hill.png"])["thumbnail"]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	test.nib(&["purge", &other, "std.content=", "--yes"])
		.expect("purges");
	assert!(test.json(&["show", "hill.png"]).get("thumbnail").is_none());
	assert_eq!(
		test.nib(&["file", "thumbnails"]).expect("makes"),
		"Made 1 thumbnail\n"
	);
	let again = test.json(&["show", "hill.png"]);
	assert_ne!(again["thumbnail"]["id"], other.as_str(), "{again}");
}
