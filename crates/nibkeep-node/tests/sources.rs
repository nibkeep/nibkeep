//! Where files came from: a source the person names, recorded with each origin of a file
//! found there; a file read from a copy that is no original, as one unpacked from an
//! archive; and what where it was found says of it, in place of what it says of itself
//! ([technical design: importers]).
//!
//! [technical design: importers]: ../../../docs/Technical-design.md#912-importers-and-where-files-came-from

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{library, now, today, values};
use nibkeep_model::Value;
use nibkeep_node::{FileFrom, FoundElsewhere, Kept, Library};
use nibkeep_std::{DESCRIPTION, FROM_SOURCE, ORIGIN_OF, PATH, SOURCE_NAME, SOURCE_PATH, TAKEN};
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::Photo;

fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes the folder");
	fs::write(&path, bytes).expect("writes");
	path
}

fn christmas() -> Photo<'static> {
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken: "2019:12:24 18:30:00",
		offset: Some("+01:00"),
		size: (4032, 3024),
		orientation: None,
		place: None,
	}
}

#[test]
fn a_file_from_two_sources_is_stored_once_with_an_origin_from_each() {
	let mut rng = TestRng::new("two sources");
	let (_, mut library) = library("two sources", &mut rng);
	let space = library.personal_space();
	let phone = folder("two sources, phone");
	let photo = write(&phone, "DCIM/Camera/IMG_2041.JPG", &christmas().jpeg());
	let old_phone = library
		.source_named(space, "Mom's old phone", now(), &mut rng)
		.expect("names it");
	assert_eq!(
		library
			.source_named(space, "mom's OLD phone", now(), &mut rng)
			.expect("finds it"),
		old_phone,
		"one source, whatever the case"
	);
	let from_phone = FileFrom {
		source: Some(old_phone),
		source_path: Some("DCIM/Camera/IMG_2041.JPG".to_owned()),
		..FileFrom::default()
	};
	let added = library
		.add_file_from(space, &photo, &from_phone, now(), &mut rng)
		.expect("adds");
	let origin = added.origin.expect("an origin");
	assert_eq!(
		values(&library, origin, &FROM_SOURCE),
		[Value::Reference(old_phone)]
	);
	assert_eq!(
		values(&library, origin, &SOURCE_PATH),
		[Value::Text("DCIM/Camera/IMG_2041.JPG".to_owned())]
	);
	// Added again, unchanged, it is known: a place has one origin, whatever source is named
	// later.
	let drive = library
		.source_named(space, "The grey drive", now(), &mut rng)
		.expect("names it");
	let from_drive = FileFrom {
		source: Some(drive),
		..FileFrom::default()
	};
	for from in [&from_phone, &from_drive] {
		let again = library
			.add_file_from(space, &photo, from, now(), &mut rng)
			.expect("adds");
		assert_eq!(again.origin, None);
	}
	assert_eq!(
		values(&library, origin, &FROM_SOURCE),
		[Value::Reference(old_phone)]
	);
	// Found elsewhere, from another source, it is found there too, and stored once.
	let grey = folder("two sources, drive");
	let copy = write(&grey, "Photos/IMG_2041.JPG", &christmas().jpeg());
	let found_again = library
		.add_file_from(space, &copy, &from_drive, now(), &mut rng)
		.expect("adds");
	assert_eq!(found_again.file, added.file);
	assert!(found_again.origin.is_some_and(|other| other != origin));
	// One added first without a source gets the one named later.
	let loose = write(&grey, "loose.jpg", b"a loose file");
	let first = library
		.add_file(space, &loose, now(), &mut rng)
		.expect("adds");
	let first_origin = first.origin.expect("an origin");
	library
		.add_file_from(space, &loose, &from_drive, now(), &mut rng)
		.expect("adds");
	assert_eq!(
		values(&library, first_origin, &FROM_SOURCE),
		[Value::Reference(drive)]
	);
	let sources = library.sources(space).expect("lists");
	let names: Vec<(&str, usize)> = sources
		.iter()
		.map(|source| (source.name.as_str(), source.found))
		.collect();
	assert_eq!(names, [("Mom's old phone", 1), ("The grey drive", 2)]);
}

#[test]
fn a_file_unpacked_from_an_archive_is_found_in_the_archive_and_kept_in_the_store() {
	let mut rng = TestRng::new("from an archive");
	let (_, mut library) = library("from an archive", &mut rng);
	let space = library.personal_space();
	let unpacked = folder("from an archive, unpacked");
	let copy = write(&unpacked, "tmp-1", &christmas().jpeg());
	let takeout = library
		.source_named(space, "Google Photos", now(), &mut rng)
		.expect("names it");
	let from = FileFrom {
		source: Some(takeout),
		source_path: Some("Takeout/Google Photos/Christmas/IMG_2041.JPG".to_owned()),
		source_name: Some("IMG_2041 the long name it had.JPG".to_owned()),
		found: Some(FoundElsewhere {
			path: "/archives/takeout-001.zip/Takeout/Google Photos/Christmas/IMG_2041.JPG"
				.to_owned(),
			name: Some("IMG_2041.JPG".to_owned()),
			modified: Some(1_577_000_000_000),
			created: None,
		}),
		values: vec![(DESCRIPTION, Value::Text("Grandma's kitchen".to_owned()))],
		..FileFrom::default()
	};
	let added = library
		.add_file_from(space, &copy, &from, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.kept, Kept::Store);
	fs::remove_file(&copy).expect("the copy goes");
	let origin = added.origin.expect("an origin");
	assert_eq!(
		values(&library, origin, &PATH),
		[Value::Text(
			"/archives/takeout-001.zip/Takeout/Google Photos/Christmas/IMG_2041.JPG".to_owned()
		)]
	);
	assert_eq!(
		values(&library, origin, &SOURCE_NAME),
		[Value::Text("IMG_2041 the long name it had.JPG".to_owned())]
	);
	assert_eq!(
		values(&library, origin, &ORIGIN_OF),
		[Value::Reference(added.file)]
	);
	assert_eq!(
		values(&library, added.file, &DESCRIPTION),
		[Value::Text("Grandma's kitchen".to_owned())]
	);
	assert!(
		library
			.file_content(space, &added.content, u64::MAX)
			.expect("reads")
			.is_some()
	);
	// Found again in the same archive, unchanged, it is known without reading it.
	let copy = write(&unpacked, "tmp-2", &christmas().jpeg());
	let again = library
		.add_file_from(space, &copy, &from, now(), &mut rng)
		.expect("adds");
	assert_eq!(again.origin, None);
}

#[test]
fn where_it_was_found_says_when_it_was_taken_unless_someone_changed_it() {
	let mut rng = TestRng::new("taken where found");
	let (_, mut library) = library("taken where found", &mut rng);
	let space = library.personal_space();
	let files = folder("taken where found");
	let photo = write(&files, "IMG_2041.JPG", &christmas().jpeg());
	// The same moment, as a service writes it: the photo's own local time stays.
	let same = FileFrom {
		taken: Some("2019-12-24T17:30:00Z".parse().expect("a moment")),
		..FileFrom::default()
	};
	let added = library
		.add_file_from(space, &photo, &same, now(), &mut rng)
		.expect("adds");
	let taken = |library: &Library| values(library, added.file, &TAKEN);
	assert_eq!(
		taken(&library),
		[Value::Date(
			"2019-12-24T18:30:00+01".parse().expect("a moment")
		)]
	);
	// A corrected date, found again elsewhere, wins over what the photo says.
	let other = folder("taken where found, again");
	let copy = write(&other, "IMG_2041.JPG", &christmas().jpeg());
	let corrected = FileFrom {
		taken: Some("2018-12-24T17:30:00Z".parse().expect("a moment")),
		..FileFrom::default()
	};
	library
		.add_file_from(space, &copy, &corrected, now(), &mut rng)
		.expect("adds");
	assert_eq!(
		taken(&library),
		[Value::Date(
			"2018-12-24T17:30:00Z".parse().expect("a moment")
		)]
	);
	// A date someone set in the library stays.
	library
		.edit_entity(
			space,
			added.file,
			vec![nibkeep_node::Edit::Set(
				TAKEN,
				vec![Value::Date("2017-12-24".parse().expect("a day"))],
			)],
			&today(),
			now(),
			&mut rng,
		)
		.expect("edits");
	let third = folder("taken where found, third");
	let copy = write(&third, "IMG_2041.JPG", &christmas().jpeg());
	library
		.add_file_from(space, &copy, &corrected, now(), &mut rng)
		.expect("adds");
	assert_eq!(
		taken(&library),
		[Value::Date("2017-12-24".parse().expect("a day"))]
	);
}

#[test]
fn a_plain_picture_found_again_with_when_it_was_taken_becomes_a_photo() {
	let mut rng = TestRng::new("plain picture");
	let (_, mut library) = library("plain picture", &mut rng);
	let space = library.personal_space();
	let files = folder("plain picture");
	// A PNG that says nothing of itself.
	let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
	png.extend_from_slice(&[0; 64]);
	let path = write(&files, "scan.png", &png);
	let added = library
		.add_file(space, &path, now(), &mut rng)
		.expect("adds");
	assert!(
		!added.types.contains(&nibkeep_std::PHOTO),
		"{:?}",
		added.types
	);
	let again = folder("plain picture, again");
	let copy = write(&again, "scan.png", &png);
	let dated = FileFrom {
		taken: Some("2020-08-01T09:00:00Z".parse().expect("a moment")),
		..FileFrom::default()
	};
	let found = library
		.add_file_from(space, &copy, &dated, now(), &mut rng)
		.expect("adds");
	assert!(
		found.types.contains(&nibkeep_std::PHOTO),
		"{:?}",
		found.types
	);
	assert_eq!(
		values(&library, added.file, &TAKEN),
		[Value::Date(
			"2020-08-01T09:00:00Z".parse().expect("a moment")
		)]
	);
}
