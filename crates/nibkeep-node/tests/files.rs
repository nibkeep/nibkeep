//! Files: content stored once however often it is added, with where it was found, and
//! what photos and videos say of themselves (Design 6.4).

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::{library, now, this_day, today};
use nibkeep_model::{FieldName, Name, TypeName, Value, fields};
use nibkeep_node::{AddedFile, Edit, Kept, Library, NodeError, resolved_path};
use nibkeep_std::{
	CAMERA_MAKE, CAMERA_MODEL, CONTENT, DURATION, FILE, FOUND_ON, HEIGHT, LATITUDE, LONGITUDE,
	MEDIA_TYPE, ORIGIN, ORIGIN_OF, PATH, PHOTO, SIZE, TAKEN, VIDEO, WIDTH,
};
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::{Photo, Video};

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::write(&path, bytes).expect("writes");
	path
}

/// The values of an entity's field, as the library shows them today.
fn values(library: &Library, entity: nibkeep_base::EntityId, field: &FieldName) -> Vec<Value> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity")
		.fields
		.into_iter()
		.filter(|shown| shown.field == *field)
		.flat_map(|shown| shown.values.into_iter().map(|value| value.value))
		.collect()
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

fn christmas() -> Photo<'static> {
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken: "2019:12:24 18:30:00",
		offset: Some("+01:00"),
		size: (4032, 3024),
		orientation: None,
		place: Some(([59, 19, 48], [18, 4, 12])),
	}
}

#[test]
fn a_file_is_added_with_its_name_content_and_origin() {
	let mut rng = TestRng::new("file added");
	let (_, mut library) = library("file added", &mut rng);
	let files = folder("file added");
	let path = write(&files, "notes.txt", b"Milk, eggs, bread.\n");
	let added = library
		.add_file(library.personal_space(), &path, now(), &mut rng)
		.expect("adds");
	assert!(!added.known);
	assert_eq!(added.types, [FILE]);
	assert_eq!(
		values(&library, added.file, &fields::NAME),
		[Value::Name(Name::new("notes.txt"))]
	);
	assert_eq!(
		values(&library, added.file, &CONTENT),
		[Value::Blob(added.content)]
	);
	assert_eq!(values(&library, added.file, &SIZE), [Value::Integer(19)]);
	assert_eq!(values(&library, added.file, &MEDIA_TYPE), []);
	let origin = added.origin.expect("an origin");
	assert_eq!(
		values(&library, origin, &ORIGIN_OF),
		[Value::Reference(added.file)]
	);
	let device = library.status().expect("a status").device.id;
	assert_eq!(
		values(&library, origin, &FOUND_ON),
		[Value::Reference(device)]
	);
	let resolved = resolved_path(&path).expect("resolves");
	assert_eq!(
		values(&library, origin, &PATH),
		[text(&resolved.to_string_lossy())]
	);
	// As people write it, which on Windows is not the form the file system resolves to.
	assert!(
		!resolved.to_string_lossy().starts_with(r"\\?\"),
		"{}",
		resolved.display()
	);
	let personal = library.personal_space();
	assert!(
		library
			.types(personal)
			.expect("types")
			.get(&ORIGIN)
			.is_some()
	);
}

#[test]
fn content_added_again_is_one_file_found_in_several_places() {
	let mut rng = TestRng::new("file again");
	let (_, mut library) = library("file again", &mut rng);
	let personal = library.personal_space();
	let files = folder("file again");
	let first = write(&files, "IMG_2041.JPG", &christmas().jpeg());
	let copy = write(&files, "IMG_2041 (1).JPG", &christmas().jpeg());
	let added = library
		.add_file(personal, &first, now(), &mut rng)
		.expect("adds");
	// The same file, from the same place, unchanged, adds nothing.
	let again = library
		.add_file(personal, &first, now() + 1_000, &mut rng)
		.expect("adds");
	assert_eq!(
		again,
		AddedFile {
			known: true,
			origin: None,
			..added.clone()
		}
	);
	// A copy elsewhere is the same file, found there too.
	let copied = library
		.add_file(personal, &copy, now() + 2_000, &mut rng)
		.expect("adds");
	assert!(copied.known);
	assert_eq!(copied.file, added.file);
	assert_ne!(copied.origin, None);
	let origins = library
		.links_to(personal, added.file, this_day(), 10)
		.expect("links");
	let found: usize = origins.iter().map(|links| links.count).sum();
	assert_eq!(found, 2);
	assert_eq!(
		library
			.entities(personal, Some(&FILE))
			.expect("lists")
			.len(),
		1
	);
}

#[test]
fn a_search_that_matches_where_a_file_was_found_finds_the_file() {
	let mut rng = TestRng::new("found where it was");
	let (_, mut library) = library("found where it was", &mut rng);
	let personal = library.personal_space();
	let files = folder("found where it was");
	fs::create_dir_all(files.join("Covers")).expect("makes");
	let mut added = Vec::new();
	for name in ["Dune.jpg", "Emma.jpg"] {
		let path = write(&files.join("Covers"), name, name.as_bytes());
		let file = library
			.add_file(personal, &path, now(), &mut rng)
			.expect("adds");
		added.push(file.file);
	}
	// Found by its name and by where it was found, it is found once.
	let dune = write(&files, "dune notes.txt", b"Spice.\n");
	let notes = library
		.add_file(personal, &dune, now(), &mut rng)
		.expect("adds");
	let found = |library: &Library, words: &str| -> Vec<_> {
		let found = library.search(personal, words).expect("searches");
		assert!(
			found.iter().all(|entity| !entity.types.contains(&ORIGIN)),
			"{found:?}"
		);
		let mut ids: Vec<_> = found
			.into_iter()
			.map(|entity| (entity.id, entity.via))
			.collect();
		ids.sort();
		ids
	};
	let mut expected: Vec<_> = added.iter().map(|&file| (file, None)).collect();
	expected.sort();
	assert_eq!(found(&library, "covers"), expected);
	let mut both = vec![(added[0], None), (notes.file, None)];
	both.sort();
	assert_eq!(found(&library, "dune"), both);
	// A file deleted is not found by where it was found either.
	library
		.edit_entity(
			personal,
			added[1],
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	assert_eq!(found(&library, "covers"), [(added[0], None)]);
	// An origin may say that several entities were found there, as two devices may at once:
	// it finds those of them that are files, and not deleted.
	fs::create_dir_all(files.join("Kitchen")).expect("makes");
	let list = write(&files.join("Kitchen"), "shopping.txt", b"Milk.\n");
	let list = library
		.add_file(personal, &list, now(), &mut rng)
		.expect("adds");
	assert_eq!(found(&library, "kitchen"), [(list.file, None)]);
	let person = library
		.create_entity(
			personal,
			&[TypeName::from_static("std.person")],
			vec![(fields::NAME, Value::Name(Name::new("Emma Woodhouse")))],
			now(),
			&mut rng,
		)
		.expect("creates");
	let origin = list.origin.expect("an origin");
	let edit = |library: &mut Library, rng: &mut TestRng, edits: Vec<Edit>| {
		library
			.edit_entity(personal, origin, edits, &today(), now(), rng)
			.expect("edits the origin");
	};
	let elsewhere = vec![
		Edit::Set(ORIGIN_OF, vec![Value::Reference(added[1])]),
		Edit::Add(ORIGIN_OF, Value::Reference(person)),
	];
	edit(&mut library, &mut rng, elsewhere);
	assert_eq!(found(&library, "kitchen"), []);
	let again = vec![Edit::Add(ORIGIN_OF, Value::Reference(list.file))];
	edit(&mut library, &mut rng, again);
	assert_eq!(found(&library, "kitchen"), [(list.file, None)]);
}

#[test]
fn a_photo_says_when_where_and_by_which_camera_it_was_taken() {
	let mut rng = TestRng::new("photo added");
	let (_, mut library) = library("photo added", &mut rng);
	let files = folder("photo added");
	let path = write(&files, "IMG_2041.JPG", &christmas().jpeg());
	let added = library
		.add_file(library.personal_space(), &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.types, [FILE, PHOTO]);
	let photo = added.file;
	assert_eq!(values(&library, photo, &MEDIA_TYPE), [text("image/jpeg")]);
	assert_eq!(
		values(&library, photo, &TAKEN),
		[Value::Date(
			"2019-12-24T18:30:00+01".parse().expect("a date")
		)]
	);
	assert_eq!(values(&library, photo, &CAMERA_MAKE), [text("Apple")]);
	assert_eq!(
		values(&library, photo, &CAMERA_MODEL),
		[text("iPhone 12 Pro")]
	);
	assert_eq!(values(&library, photo, &WIDTH), [Value::Integer(4032)]);
	assert_eq!(values(&library, photo, &HEIGHT), [Value::Integer(3024)]);
	assert_eq!(
		values(&library, photo, &LATITUDE),
		[Value::Decimal("59.330000".parse().expect("a decimal"))]
	);
	assert_eq!(
		values(&library, photo, &LONGITUDE),
		[Value::Decimal("18.070000".parse().expect("a decimal"))]
	);
}

#[test]
fn a_photo_without_an_offset_or_a_place_is_taken_in_local_time() {
	let mut rng = TestRng::new("photo local");
	let (_, mut library) = library("photo local", &mut rng);
	let files = folder("photo local");
	let photo = Photo {
		offset: None,
		place: None,
		..christmas()
	};
	let path = write(&files, "DSC_0001.JPG", &photo.jpeg());
	let added = library
		.add_file(library.personal_space(), &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(
		values(&library, added.file, &TAKEN),
		[Value::Date("2019-12-24T18:30:00".parse().expect("a date"))]
	);
	assert_eq!(values(&library, added.file, &LATITUDE), []);
}

#[test]
fn a_video_says_when_it_was_made_how_long_and_how_large_it_is() {
	let mut rng = TestRng::new("video added");
	let (_, mut library) = library("video added", &mut rng);
	let files = folder("video added");
	let video = Video {
		created: Some(1_577_208_600),
		duration_millis: 12_345,
		size: (1920, 1080),
	};
	let path = write(&files, "first steps.mp4", &video.mp4());
	let added = library
		.add_file(library.personal_space(), &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.types, [FILE, VIDEO]);
	assert_eq!(
		values(&library, added.file, &MEDIA_TYPE),
		[text("video/mp4")]
	);
	assert_eq!(
		values(&library, added.file, &TAKEN),
		[Value::Date("2019-12-24T17:30:00Z".parse().expect("a date"))]
	);
	assert_eq!(values(&library, added.file, &WIDTH), [Value::Integer(1920)]);
	assert_eq!(
		values(&library, added.file, &HEIGHT),
		[Value::Integer(1080)]
	);
	assert_eq!(
		values(&library, added.file, &DURATION),
		[Value::Quantity(
			nibkeep_model::Quantity::new("12.345".parse().expect("a decimal"), "s")
				.expect("a quantity")
		)]
	);
}

#[test]
fn what_is_not_a_file_is_refused() {
	let mut rng = TestRng::new("not a file");
	let (_, mut library) = library("not a file", &mut rng);
	let files = folder("not a file");
	let personal = library.personal_space();
	for path in [files.clone(), files.join("missing")] {
		let error = library
			.add_file(personal, &path, now(), &mut rng)
			.expect_err("refused");
		assert!(matches!(error, NodeError::Unreadable { .. }), "{error}");
	}
}

#[test]
fn a_photo_turned_a_quarter_is_as_wide_as_it_is_stored_high() {
	let mut rng = TestRng::new("photo turned");
	let (_, mut library) = library("photo turned", &mut rng);
	let files = folder("photo turned");
	let portrait = Photo {
		orientation: Some(6),
		..christmas()
	};
	let path = write(&files, "IMG_2042.JPG", &portrait.jpeg());
	let added = library
		.add_file(library.personal_space(), &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(values(&library, added.file, &WIDTH), [Value::Integer(3024)]);
	assert_eq!(
		values(&library, added.file, &HEIGHT),
		[Value::Integer(4032)]
	);
}

#[test]
fn a_video_header_that_says_nothing_gives_no_date_and_audio_is_no_video() {
	let mut rng = TestRng::new("video says nothing");
	let (_, mut library) = library("video says nothing", &mut rng);
	let personal = library.personal_space();
	let files = folder("video says nothing");
	let undated = Video {
		created: None,
		duration_millis: 1_000,
		size: (640, 480),
	};
	let path = write(&files, "screen.mp4", &undated.mp4());
	let added = library
		.add_file(personal, &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.types, [FILE, VIDEO]);
	assert_eq!(values(&library, added.file, &TAKEN), []);
	// A track without a picture.
	let sound = Video {
		size: (0, 0),
		..undated
	};
	let path = write(&files, "voice.m4a", &sound.mp4());
	let added = library
		.add_file(personal, &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.types, [FILE]);
}

#[test]
fn a_file_unchanged_at_its_path_is_not_read_again() {
	let mut rng = TestRng::new("file unchanged");
	let (_, mut library) = library("file unchanged", &mut rng);
	let personal = library.personal_space();
	let files = folder("file unchanged");
	let path = write(&files, "notes.txt", b"Milk.\n");
	let first = library
		.add_file(personal, &path, now(), &mut rng)
		.expect("adds");
	// Changed, but with its size and modification time as they were: it is taken to be
	// what it was, as `rsync` takes it.
	let modified = fs::metadata(&path)
		.and_then(|metadata| metadata.modified())
		.expect("a time");
	fs::write(&path, b"Eggs.\n").expect("writes");
	let file = fs::File::options().write(true).open(&path).expect("opens");
	file.set_modified(modified).expect("sets the time");
	drop(file);
	let again = library
		.add_file(personal, &path, now() + 1_000, &mut rng)
		.expect("adds");
	assert_eq!(
		(again.known, again.origin, again.content),
		(true, None, first.content)
	);
	// Its time changed too, it is read, and is another file.
	let file = fs::File::options().write(true).open(&path).expect("opens");
	file.set_modified(modified + std::time::Duration::from_secs(60))
		.expect("sets the time");
	drop(file);
	let changed = library
		.add_file(personal, &path, now() + 2_000, &mut rng)
		.expect("adds");
	assert!(!changed.known);
	assert_ne!(changed.content, first.content);
}

#[test]
fn a_file_in_a_temporary_folder_goes_into_the_store_and_records_no_origin() {
	let mut rng = TestRng::new("file in temp");
	let (directory, mut library) = library("file in temp", &mut rng);
	let temporary = resolved_path(&std::env::temp_dir()).expect("resolves");
	// Where tests keep libraries in the temporary folder, as on iOS, there is nothing to
	// tell apart.
	if resolved_path(&directory).is_ok_and(|directory| directory.starts_with(&temporary)) {
		return;
	}
	let personal = library.personal_space();
	// As an AI agent's scratch folder, which is gone once its session ends.
	let files = nibkeep_testkit::empty_directory(
		temporary.join("nibkeep-node-tests"),
		&format!("file in temp, scratch {}", std::process::id()),
	);
	let path = write(&files, "cover.jpg", &christmas().jpeg());
	let added = library
		.add_file(personal, &path, now(), &mut rng)
		.expect("adds");
	assert_eq!(
		(added.known, added.origin, added.kept),
		(false, None, Kept::Store)
	);
	assert_eq!(
		values(&library, added.file, &fields::NAME),
		[Value::Name(Name::new("cover.jpg"))]
	);
	assert_eq!(
		library
			.entities(personal, Some(&ORIGIN))
			.expect("lists")
			.len(),
		0
	);
	// Added again, it is the file it was, and still found nowhere.
	let again = library
		.add_file(personal, &path, now() + 1_000, &mut rng)
		.expect("adds");
	assert_eq!((again.known, again.origin), (true, None));
	assert_eq!((again.file, again.kept), (added.file, Kept::Store));
	// The same content found in a lasting folder records an origin there.
	let lasting = write(&folder("file in temp"), "cover.jpg", &christmas().jpeg());
	let found = library
		.add_file(personal, &lasting, now() + 2_000, &mut rng)
		.expect("adds");
	assert_eq!(found.file, added.file);
	let origin = found.origin.expect("an origin");
	assert_eq!(
		values(&library, origin, &PATH),
		[text(
			&resolved_path(&lasting).expect("resolves").to_string_lossy()
		)]
	);
	fs::remove_dir_all(&files).expect("removes the scratch folder");
}

#[test]
fn one_place_has_one_path() {
	let mut rng = TestRng::new("one path");
	let (_, mut library) = library("one path", &mut rng);
	let personal = library.personal_space();
	let files = folder("one path");
	fs::create_dir_all(files.join("2019")).expect("makes");
	let path = write(&files.join("2019"), "notes.txt", b"Milk.\n");
	let first = library
		.add_file(personal, &path, now(), &mut rng)
		.expect("adds");
	let roundabout = files.join("2019").join("..").join("2019").join("notes.txt");
	let again = library
		.add_file(personal, &roundabout, now() + 1_000, &mut rng)
		.expect("adds");
	assert_eq!((again.known, again.origin), (true, None));
	let origin = first.origin.expect("an origin");
	let resolved = resolved_path(&path).expect("resolves");
	assert_eq!(
		values(&library, origin, &PATH),
		[text(&resolved.to_string_lossy())]
	);
}
