//! Copies and edits of photos, linked, never merged: a photo resized or saved again is
//! linked to the larger by `std.copy-of` once both have thumbnails, whichever came first;
//! photos of one folder are never copies of each other; an edit is linked by `std.edit-of`
//! and never taken for a copy; and a link that a person removed stays removed
//! ([technical design: importers]).
//!
//! [technical design: importers]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#912-importers-and-where-files-came-from

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{library, now, today, values};
use image::imageops::FilterType;
use image::{DynamicImage, Rgb, RgbImage};
use nibkeep_base::Hash;
use nibkeep_model::Value;
use nibkeep_node::{AddedFile, Edit, FileFrom, Library, Thumbnail, Thumbnailer};
use nibkeep_std::{COPY_OF, EDIT_OF};
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::Photo;

/// What nib makes thumbnails with.
struct Decoders;

impl Thumbnailer for Decoders {
	fn made_with(&self) -> &str {
		nibkeep_thumbnails::MADE_WITH
	}

	fn reads(&self, media_type: &str) -> bool {
		nibkeep_thumbnails::reads(media_type)
	}

	fn make(&self, picture: &[u8]) -> Option<Thumbnail> {
		let made = nibkeep_thumbnails::make(picture).ok()?;
		Some(Thumbnail {
			jpeg: made.jpeg,
			width: made.width,
			height: made.height,
			picture_hash: Some(made.picture_hash),
		})
	}
}

fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes the folder");
	fs::write(&path, bytes).expect("writes");
	path
}

/// A scene, as a photo of something shows: soft light and shade that `seed` places.
fn scene(seed: u32, width: u32, height: u32) -> DynamicImage {
	let seed = f64::from(seed);
	DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
		let (u, v) = (
			f64::from(x) / f64::from(width),
			f64::from(y) / f64::from(height),
		);
		let light = (u * (3.0 + seed) + seed).sin() * (v * (2.0 + seed / 2.0) - seed).cos()
			+ (u * 7.0 * v + seed * 1.3).sin() / 2.0;
		let grey = 128.0 + 80.0 * light.clamp(-1.5, 1.5) / 1.5;
		let channel = |shift: f64| {
			// Within 0 to 255, and so a byte.
			#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
			let byte = (grey + shift).clamp(0.0, 255.0) as u8;
			byte
		};
		Rgb([channel(10.0), channel(0.0), channel(-10.0)])
	}))
}

/// A picture as a JPEG of a quality.
fn jpeg(picture: &DynamicImage, quality: u8) -> Vec<u8> {
	let mut bytes = Vec::new();
	image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
		.encode_image(picture)
		.expect("encodes");
	bytes
}

/// A photo of `seed`'s scene taken at Christmas 2019 at 18:30, local time, as a phone keeps
/// it: large, with EXIF data without an offset.
fn original(seed: u32, taken: &'static str) -> Vec<u8> {
	let photo = Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken,
		offset: None,
		size: (2000, 1500),
		orientation: None,
		place: None,
	};
	photo.jpeg_of(&jpeg(&scene(seed, 2000, 1500), 92))
}

/// The same photo as Google keeps it, resized and saved again, without EXIF data.
fn resized(seed: u32, width: u32, height: u32, quality: u8) -> Vec<u8> {
	jpeg(
		&scene(seed, 2000, 1500).resize_exact(width, height, FilterType::Triangle),
		quality,
	)
}

/// Where Google said a resized copy was taken: the same moment in UTC.
fn from_google() -> FileFrom {
	FileFrom {
		taken: Some("2019-12-24T17:30:00Z".parse().expect("a moment")),
		..FileFrom::default()
	}
}

fn thumbnails(library: &mut Library, contents: &[Hash], rng: &mut TestRng) -> usize {
	let space = library.personal_space();
	library
		.make_thumbnails(space, contents, now(), rng)
		.expect("makes")
		.copies_linked
}

fn add(library: &mut Library, path: &Path, from: &FileFrom, rng: &mut TestRng) -> AddedFile {
	let space = library.personal_space();
	library
		.add_file_from(space, path, from, now(), rng)
		.expect("adds")
}

fn setup(test: &str, rng: &mut TestRng) -> Library {
	let (_, mut library) = library(test, rng);
	library.make_thumbnails_with(Arc::new(Decoders));
	library
}

#[test]
fn a_resized_copy_is_linked_to_its_original_whichever_comes_first_and_never_merged() {
	let mut rng = TestRng::new("resized copy");
	let mut library = setup("resized copy", &mut rng);
	let phone = folder("resized copy, phone");
	let takeout = folder("resized copy, takeout");

	// The original first, then Google's copy, taken the same second, written in UTC.
	let first = add(
		&mut library,
		&write(&phone, "IMG_1.JPG", &original(1, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	assert_eq!(thumbnails(&mut library, &[first.content], &mut rng), 0);
	let copy = add(
		&mut library,
		&write(&takeout, "IMG_1.jpg", &resized(1, 800, 600, 70)),
		&from_google(),
		&mut rng,
	);
	assert_eq!(thumbnails(&mut library, &[copy.content], &mut rng), 1);
	assert_ne!(copy.file, first.file);
	assert_eq!(
		values(&library, copy.file, &COPY_OF),
		[Value::Reference(first.file)]
	);
	nibkeep_testkit::assert_empty!(values(&library, first.file, &COPY_OF));

	// The copy first, then the original: the copy is linked once the original's thumbnail
	// is made. A copy only saved again, of the same size, is the smaller in bytes.
	let small = add(
		&mut library,
		&write(&takeout, "IMG_2.jpg", &resized(2, 2000, 1500, 40)),
		&from_google(),
		&mut rng,
	);
	assert_eq!(thumbnails(&mut library, &[small.content], &mut rng), 0);
	let large = add(
		&mut library,
		&write(&phone, "IMG_2.JPG", &original(2, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	assert_eq!(thumbnails(&mut library, &[large.content], &mut rng), 1);
	assert_eq!(
		values(&library, small.file, &COPY_OF),
		[Value::Reference(large.file)]
	);
	nibkeep_testkit::assert_empty!(values(&library, large.file, &COPY_OF));
}

#[test]
fn photos_of_one_folder_another_second_or_another_scene_are_not_copies() {
	let mut rng = TestRng::new("not copies");
	let mut library = setup("not copies", &mut rng);
	let phone = folder("not copies, phone");
	let elsewhere = folder("not copies, elsewhere");
	let one = add(
		&mut library,
		&write(&phone, "IMG_1.JPG", &original(1, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	// A burst: the same scene, the same second, in the same folder.
	let burst = add(
		&mut library,
		&write(&phone, "IMG_2.JPG", &resized(1, 1600, 1200, 85)),
		&from_google(),
		&mut rng,
	);
	// The same scene a second later, and another scene the same second, elsewhere.
	let later = add(
		&mut library,
		&write(&elsewhere, "later.jpg", &resized(1, 800, 600, 70)),
		&FileFrom {
			taken: Some("2019-12-24T17:30:01Z".parse().expect("a moment")),
			..FileFrom::default()
		},
		&mut rng,
	);
	let other = add(
		&mut library,
		&write(&elsewhere, "other.jpg", &resized(5, 800, 600, 70)),
		&from_google(),
		&mut rng,
	);
	let contents = [one.content, burst.content, later.content, other.content];
	assert_eq!(thumbnails(&mut library, &contents, &mut rng), 0);
	for file in [one.file, burst.file, later.file, other.file] {
		nibkeep_testkit::assert_empty!(values(&library, file, &COPY_OF));
	}
}

#[test]
fn an_edit_is_linked_as_an_edit_and_never_as_a_copy() {
	let mut rng = TestRng::new("edit");
	let mut library = setup("edit", &mut rng);
	let space = library.personal_space();
	let phone = folder("edit, phone");
	let edited = folder("edit, edited");
	let original = add(
		&mut library,
		&write(&phone, "IMG_1.JPG", &original(1, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	let edit = add(
		&mut library,
		&write(&edited, "IMG_1-edited.jpg", &resized(1, 1000, 750, 80)),
		&from_google(),
		&mut rng,
	);
	assert!(
		library
			.link_edit(space, (edit.file, original.file), now(), &mut rng)
			.expect("links")
	);
	assert!(
		!library
			.link_edit(space, (edit.file, original.file), now(), &mut rng)
			.expect("links"),
		"linked once"
	);
	assert_eq!(
		thumbnails(&mut library, &[original.content, edit.content], &mut rng),
		0
	);
	assert_eq!(
		values(&library, edit.file, &EDIT_OF),
		[Value::Reference(original.file)]
	);
	nibkeep_testkit::assert_empty!(values(&library, edit.file, &COPY_OF));
}

#[test]
fn a_link_that_a_person_removed_stays_removed() {
	let mut rng = TestRng::new("removed link");
	let mut library = setup("removed link", &mut rng);
	let space = library.personal_space();
	let phone = folder("removed link, phone");
	let takeout = folder("removed link, takeout");
	let drive = folder("removed link, drive");
	let large = add(
		&mut library,
		&write(&phone, "IMG_1.JPG", &original(1, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	let copy = add(
		&mut library,
		&write(&takeout, "IMG_1.jpg", &resized(1, 800, 600, 70)),
		&from_google(),
		&mut rng,
	);
	assert_eq!(
		thumbnails(&mut library, &[large.content, copy.content], &mut rng),
		1
	);
	library
		.edit_entity(
			space,
			copy.file,
			vec![Edit::Remove(COPY_OF, Value::Reference(large.file))],
			&today(),
			now(),
			&mut rng,
		)
		.expect("removes the link");
	// A smaller copy still: it is linked, and the first copy stays unlinked.
	let smaller = add(
		&mut library,
		&write(&drive, "IMG_1 small.jpg", &resized(1, 400, 300, 60)),
		&from_google(),
		&mut rng,
	);
	assert_eq!(thumbnails(&mut library, &[smaller.content], &mut rng), 1);
	nibkeep_testkit::assert_empty!(values(&library, copy.file, &COPY_OF));
	assert_eq!(
		values(&library, smaller.file, &COPY_OF).len(),
		1,
		"linked to a larger one"
	);
}

#[test]
fn a_picture_without_a_picture_hash_is_linked_to_nothing() {
	let mut rng = TestRng::new("no hash");
	let (_, mut library) = library("no hash", &mut rng);
	library.make_thumbnails_with(Arc::new(NoHash));
	let phone = folder("no hash, phone");
	let takeout = folder("no hash, takeout");
	let large = add(
		&mut library,
		&write(&phone, "IMG_1.JPG", &original(1, "2019:12:24 18:30:00")),
		&FileFrom::default(),
		&mut rng,
	);
	let copy = add(
		&mut library,
		&write(&takeout, "IMG_1.jpg", &resized(1, 800, 600, 70)),
		&from_google(),
		&mut rng,
	);
	assert_eq!(
		thumbnails(&mut library, &[large.content, copy.content], &mut rng),
		0
	);
}

/// A thumbnailer that makes no picture hashes, as a phone's might not.
struct NoHash;

impl Thumbnailer for NoHash {
	fn made_with(&self) -> &'static str {
		"no hash 1"
	}

	fn reads(&self, media_type: &str) -> bool {
		nibkeep_thumbnails::reads(media_type)
	}

	fn make(&self, picture: &[u8]) -> Option<Thumbnail> {
		let made = Decoders.make(picture)?;
		Some(Thumbnail {
			picture_hash: None,
			..made
		})
	}
}
