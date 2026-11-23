//! What each replica keeps of its spaces' files, and asks for in each sync: archive nodes
//! every file kept in the store, everyday devices pinned files and the newest that fit their
//! size, and every replica every change, key and thumbnail ([technical design: the planner]).
//!
//! [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner

mod common;

use std::fs;
use std::io::Cursor;
use std::sync::Arc;

use common::devices::{Devices, blob_files, bytes, damage_blobs};
use common::replica_folder;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use nibkeep_node::{DeviceRole, NodeError, Thumbnail, Thumbnailer};
use nibkeep_testkit::media::Photo;

/// What nib makes thumbnails with.
struct Decoders;

impl Thumbnailer for Decoders {
	fn made_with(&self) -> &'static str {
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

/// A photo taken at `taken`, as EXIF writes it, with pixels of its own colour.
fn photo(taken: &'static str, colour: u8) -> Vec<u8> {
	let picture = DynamicImage::ImageRgb8(RgbImage::from_pixel(64, 48, Rgb([colour, 90, 40])));
	let mut jpeg = Vec::new();
	picture
		.write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
		.expect("encodes");
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken,
		offset: Some("+01:00"),
		size: (64, 48),
		orientation: None,
		place: None,
	}
	.jpeg_of(&jpeg)
}

#[test]
fn while_no_device_is_an_archive_node_every_replica_keeps_every_file() {
	let mut devices = Devices::new("planner, no archive node", false);
	let laptop = devices.pair("laptop", Some(0));
	let (_, content) = devices.add(0, "notes.txt", &bytes(1, 10_000));
	devices.sync(0, laptop);
	assert!(devices.holds(laptop, &content));
}

#[test]
fn an_everyday_device_keeps_the_newest_files_that_fit_its_size() {
	let mut devices = Devices::new("planner, newest", true);
	let old = photo("2015:06:01 10:00:00", 10);
	let new = photo("2025:06:01 10:00:00", 200);
	let laptop = devices.pair(
		"laptop",
		Some(u64::try_from(new.len()).expect("small") + 10),
	);
	let (_, old) = devices.add(0, "old.jpg", &old);
	let (_, new) = devices.add(0, "new.jpg", &new);
	devices.sync(0, laptop);
	assert!(devices.holds(laptop, &new));
	assert!(!devices.holds(laptop, &old));
	// A sync later asks for nothing more, nor sends what the laptop does not keep.
	devices.sync(laptop, 0);
	assert!(!devices.holds(laptop, &old));
	// With room for both, it keeps both.
	devices.mark(laptop, DeviceRole::Everyday, Some(1 << 20));
	devices.sync(laptop, 0);
	assert!(devices.holds(laptop, &old));
}

#[test]
fn an_archive_node_keeps_every_file_but_those_kept_in_place() {
	let mut devices = Devices::new("planner, archive", true);
	let laptop = devices.pair("laptop", None);
	let (_, stored) = devices.add(laptop, "report.pdf", &bytes(2, 5_000));
	let (film, in_place) = devices.add(laptop, "film.mkv", &bytes(3, 7_000));
	let space = devices.personal();
	let at = devices.tick();
	let replaceable = devices.libraries[laptop]
		.find_policy(space, "Replaceable")
		.expect("installed");
	devices.libraries[laptop]
		.set_policy(space, film, Some(replaceable), at, &mut devices.rng)
		.expect("sets");
	devices.sync(laptop, 0);
	assert!(devices.holds(0, &stored));
	assert!(!devices.holds(0, &in_place));
}

#[test]
fn every_replica_keeps_every_thumbnail_whatever_its_size() {
	let mut devices = Devices::new("planner, thumbnails", true);
	let phone = devices.pair("phone", Some(0));
	devices.libraries[0].make_thumbnails_with(Arc::new(Decoders));
	let (file, content) = devices.add(0, "beach.jpg", &photo("2024:07:01 12:00:00", 120));
	let space = devices.personal();
	let at = devices.tick();
	devices.libraries[0]
		.make_thumbnails(space, &[content], at, &mut devices.rng)
		.expect("makes");
	devices.sync(phone, 0);
	assert!(!devices.holds(phone, &content));
	let thumbnail = devices.libraries[phone]
		.thumbnail(space, file)
		.expect("reads")
		.expect("a thumbnail");
	assert!(
		devices.libraries[phone]
			.thumbnail_jpeg(space, &thumbnail)
			.expect("reads")
			.is_some()
	);
}

#[test]
fn a_pinned_file_is_kept_whatever_the_size() {
	let mut devices = Devices::new("planner, pinned", true);
	let phone = devices.pair("phone", Some(0));
	let (file, content) = devices.add(0, "passport scan.png", &bytes(4, 9_000));
	devices.sync(phone, 0);
	assert!(!devices.holds(phone, &content));
	// Pinned on the NAS, for the phone: the phone keeps it from its next sync.
	let space = devices.personal();
	let device = devices.device_of(phone);
	let at = devices.tick();
	devices.libraries[0]
		.pin_file((space, file), device, true, at, &mut devices.rng)
		.expect("pins");
	devices.sync(phone, 0);
	assert!(devices.holds(phone, &content));
	assert_eq!(
		devices.libraries[phone]
			.pinned_files(device)
			.expect("reads"),
		[file].into()
	);
	// Unpinned, it is no longer pinned; removing the copy comes with safe removal.
	let at = devices.tick();
	devices.libraries[phone]
		.pin_file((space, file), device, false, at, &mut devices.rng)
		.expect("unpins");
	assert!(
		devices.libraries[phone]
			.pinned_files(device)
			.expect("reads")
			.is_empty()
	);
	// Only files are pinned.
	let at = devices.tick();
	assert!(matches!(
		devices.libraries[0].pin_file((space, device), device, true, at, &mut devices.rng),
		Err(NodeError::NotAFile(_))
	));
}

#[test]
fn every_replica_receives_every_change_and_key_whatever_files_it_keeps() {
	let mut devices = Devices::new("planner, changes and keys", true);
	let phone = devices.pair("phone", Some(0));
	let (file, content) = devices.add(0, "song.flac", &bytes(5, 12_000));
	devices.sync(phone, 0);
	assert!(!devices.holds(phone, &content));
	let space = devices.personal();
	// The file is there, with where its copies are.
	let copies = devices.libraries[phone]
		.copies_of(space, file, devices.at)
		.expect("counts");
	assert_eq!(copies.len(), 1);
	// And its key: once opened, it comes, and opens.
	let at = devices.tick();
	devices.libraries[phone]
		.record_opened(space, file, at)
		.expect("records");
	devices.sync(phone, 0);
	assert_eq!(
		devices.libraries[phone]
			.file_content(space, &content, u64::MAX)
			.expect("reads"),
		Some(bytes(5, 12_000))
	);
}

#[test]
fn a_file_opened_on_a_device_that_lacks_it_arrives_checked_against_its_hash() {
	let mut devices = Devices::new("planner, opened", true);
	let phone = devices.pair("phone", Some(0));
	let (file, content) = devices.add(0, "contract.pdf", &bytes(6, 20_000));
	devices.sync(phone, 0);
	let space = devices.personal();
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "planner, opened, checkouts");
	// Checking it out where the device lacks it records it as opened, and fails.
	let at = devices.tick();
	assert!(matches!(
		devices.libraries[phone].checkout(space, file, &folder, at, &mut devices.rng),
		Err(NodeError::NoCopyHere(_))
	));
	// The NAS's copy is damaged: what comes is refused, as its hash says.
	let nas = replica_folder(&devices.nas);
	damage_blobs(&nas.join("blobs"));
	devices.sync(phone, 0);
	assert!(!devices.holds(phone, &content));
	// Once intact again, the next sync brings it, checked, and the checkout writes it.
	damage_blobs(&nas.join("blobs"));
	devices.sync(phone, 0);
	assert!(devices.holds(phone, &content));
	let at = devices.tick();
	let checked_out = devices.libraries[phone]
		.checkout(space, file, &folder, at, &mut devices.rng)
		.expect("checks out");
	assert_eq!(fs::read(checked_out.path).expect("reads"), bytes(6, 20_000));
}

#[test]
fn a_replica_keeps_one_sealed_copy_of_a_content() {
	let mut devices = Devices::new("planner, one sealed copy", true);
	let laptop = devices.pair("laptop", None);
	let desk = devices.pair("desk", None);
	// Both add the same content before they sync: two sealed copies of it.
	let (_, content) = devices.add(laptop, "same.txt", &bytes(7, 3_000));
	let (_, again) = devices.add(desk, "same again.txt", &bytes(7, 3_000));
	assert_eq!(content, again);
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	devices.sync(laptop, 0);
	// It may hold both for a while, as it fetches the smaller even while it holds the
	// larger; its next pass of removals lets the larger go.
	devices.remove(0);
	let blobs = blob_files(&replica_folder(&devices.nas).join("blobs"));
	assert_eq!(blobs.len(), 1, "{blobs:?}");
}

#[test]
fn a_content_comes_from_whichever_of_its_sealed_copies_the_other_device_holds() {
	let mut devices = Devices::new("planner, either sealed copy", true);
	let laptop = devices.pair("laptop", None);
	let desk = devices.pair("desk", None);
	let phone = devices.pair("phone", Some(0));
	// Two sealed copies of one content; the NAS holds the desk's alone, whichever is smaller.
	let (file, content) = devices.add(laptop, "same.txt", &bytes(8, 4_000));
	devices.add(desk, "same again.txt", &bytes(8, 4_000));
	devices.sync(desk, 0);
	devices.sync(laptop, 0);
	devices.sync(phone, 0);
	let at = devices.tick();
	let space = devices.personal();
	devices.libraries[phone]
		.record_opened(space, file, at)
		.expect("records");
	devices.sync(phone, 0);
	assert!(devices.holds(phone, &content));
}

#[test]
fn a_file_too_large_to_fit_leaves_room_for_older_ones() {
	let mut devices = Devices::new("planner, too large", true);
	let laptop = devices.pair("laptop", Some(20_000));
	let (_, older) = devices.add(0, "older.jpg", &photo("2020:01:01 10:00:00", 30));
	let (_, newest) = devices.add(0, "film.mkv", &bytes(9, 50_000));
	devices.sync(laptop, 0);
	assert!(!devices.holds(laptop, &newest));
	assert!(devices.holds(laptop, &older));
}

#[test]
fn sizes_and_pins_are_marks_of_the_personal_space() {
	let mut devices = Devices::new("planner, marks", false);
	let laptop = devices.pair("laptop", Some(5_000_000));
	let device = devices.device_of(laptop);
	devices.sync(laptop, 0);
	assert_eq!(
		devices.libraries[0]
			.device_marks(device)
			.expect("reads")
			.keeps,
		Some(5_000_000)
	);
	assert_eq!(
		devices.libraries[laptop].keeps_up_to().expect("reads"),
		5_000_000
	);
	// Back to the default: a tenth of the disk on a computer.
	devices.mark(laptop, DeviceRole::Everyday, None);
	devices.sync(laptop, 0);
	assert_eq!(
		devices.libraries[0]
			.device_marks(device)
			.expect("reads")
			.keeps,
		None
	);
	assert!(devices.libraries[laptop].keeps_up_to().expect("reads") > 5_000_000);
}

#[test]
fn of_two_thumbnails_of_a_photo_the_one_not_shown_is_purged_by_its_maker() {
	let mut devices = Devices::new("planner, thumbnails twice", false);
	let laptop = devices.pair("laptop", None);
	let space = devices.personal();
	// Both add the same photo before they sync, and each makes a thumbnail of it.
	let mut files = Vec::new();
	for device in [0, laptop] {
		devices.libraries[device].make_thumbnails_with(Arc::new(Decoders));
		let (file, content) = devices.add(device, "beach.jpg", &photo("2024:07:01 12:00:00", 120));
		let at = devices.tick();
		devices.libraries[device]
			.make_thumbnails(space, &[content], at, &mut devices.rng)
			.expect("makes");
		files.push(file);
	}
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let shown = |devices: &Devices, device: usize| {
		devices.libraries[device]
			.thumbnail(space, files[0])
			.expect("reads")
			.expect("a thumbnail")
			.entity
	};
	assert_eq!(shown(&devices, 0), shown(&devices, laptop));
	let mut purged = 0;
	for device in [0, laptop] {
		let at = devices.tick();
		purged += devices.libraries[device]
			.purge_thumbnails_made_twice(at, &mut devices.rng)
			.expect("purges");
	}
	assert_eq!(purged, 1, "the maker of the one not shown purges it");
	devices.sync(laptop, 0);
	for device in [0, laptop] {
		let at = devices.tick();
		assert_eq!(
			devices.libraries[device]
				.purge_thumbnails_made_twice(at, &mut devices.rng)
				.expect("purges"),
			0
		);
	}
	// The one shown stays, on both.
	assert_eq!(shown(&devices, 0), shown(&devices, laptop));
	assert!(
		devices.libraries[laptop]
			.thumbnail_jpeg(
				space,
				&devices.libraries[laptop]
					.thumbnail(space, files[1])
					.expect("reads")
					.expect("one")
			)
			.expect("reads")
			.is_some()
	);
}

#[test]
fn a_copy_to_view_is_read_only_fetched_when_lacking_and_removed_together() {
	let mut devices = Devices::new("planner, viewed", true);
	let phone = devices.pair("phone", Some(0));
	let (file, content) = devices.add(0, "contract.pdf", &bytes(6, 20_000));
	devices.sync(phone, 0);
	let space = devices.personal();
	let views =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "planner, viewed, copies");
	// Where the device lacks the file, viewing records it as opened, and fails, so that a
	// sync brings it.
	let at = devices.tick();
	assert!(matches!(
		devices.libraries[phone].view_copy(space, file, &views, at),
		Err(NodeError::NoCopyHere(_))
	));
	devices.sync(phone, 0);
	assert!(devices.holds(phone, &content));
	let at = devices.tick();
	let copy = devices.libraries[phone]
		.view_copy(space, file, &views, at)
		.expect("writes");
	assert_eq!(fs::read(&copy).expect("reads"), bytes(6, 20_000));
	assert_eq!(copy.file_name().expect("a name"), "contract.pdf");
	let within = copy
		.parent()
		.and_then(std::path::Path::parent)
		.expect("in folders");
	assert_eq!(
		fs::canonicalize(within).expect("resolves"),
		fs::canonicalize(&views).expect("resolves")
	);
	// Its folder is named as Windows takes too.
	let of_content = copy.parent().and_then(|folder| folder.file_name());
	let of_content = of_content.and_then(|name| name.to_str()).expect("a name");
	assert!(
		of_content
			.chars()
			.all(|char| char.is_ascii_alphanumeric() || char == '-'),
		"{of_content}"
	);
	assert!(copy.metadata().expect("is there").permissions().readonly());
	// No checkout: nothing is recorded, and NibKeep takes no edits from it.
	let checkouts = devices.libraries[phone].checkouts(space).expect("reads");
	assert_eq!(checkouts.len(), 0);
	// Viewed again, it is the same copy.
	let at = devices.tick();
	assert_eq!(
		devices.libraries[phone]
			.view_copy(space, file, &views, at)
			.expect("writes"),
		copy
	);
	// The copies go together, and nothing else in the folder does.
	fs::write(views.join("settings.json"), "{}").expect("writes");
	fs::create_dir(views.join("library")).expect("makes");
	fs::write(views.join("library").join("log"), "kept").expect("writes");
	assert_eq!(nibkeep_node::remove_view_copies(&views), 1);
	assert!(!copy.exists());
	assert!(views.join("settings.json").exists());
	assert!(views.join("library").join("log").exists());
	assert_eq!(nibkeep_node::remove_view_copies(&views), 0);
}
