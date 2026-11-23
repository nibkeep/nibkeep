//! Google Takeout's archives of Google Photos, read in place: each photo with what its JSON
//! file says, its albums as folders, its edited copies linked to their originals, from the
//! source Google Photos, and an import cut short going on where it stopped ([technical
//! design: importers]).
//!
//! [technical design: importers]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#912-importers-and-where-files-came-from

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use nibkeep_base::EntityId;
use nibkeep_import::{GOOGLE_PHOTOS, Takeout};
use nibkeep_model::{FieldName, Value, fields};
use nibkeep_node::{Dating, FileFrom, Library, NewLibrary, NewProtection, PassphraseParams};
use nibkeep_std::{
	DESCRIPTION, EDIT_OF, FOLDER, FROM_SOURCE, IN_FOLDER, LATITUDE, ORIGIN_OF, SOURCE_NAME, TAKEN,
};
use nibkeep_testkit::media::Photo;
use nibkeep_testkit::{TestRng, millis_at};

fn now() -> u64 {
	millis_at("2026-11-11", 10, 0)
}

fn library(name: &str, rng: &mut TestRng) -> Library {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&NewProtection::Passphrase(
			"correct horse",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		),
		now(),
		rng,
	)
	.expect("creates")
}

fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, archive"))
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

/// The files of a small Takeout of Google Photos, by their paths: an album with a photo whose
/// date someone corrected, a copy of it, and an edited copy; a year's folder with a photo
/// whose name Takeout cut short; a photo without a JSON file; a JSON file whose photo is
/// missing; and a file of Drive.
fn takeout() -> Vec<(String, Vec<u8>)> {
	let photos = "Takeout/Google Photos";
	let long = "a weekend at the lake with the whole family and the dog.jpg";
	let cut = "a weekend at the lake with the whole family and";
	vec![
		(
			format!("{photos}/Christmas 2019/metadata.json"),
			br#"{"title":"Christmas 2019","description":"At Grandma's"}"#.to_vec(),
		),
		(
			format!("{photos}/Christmas 2019/IMG_2041.jpg"),
			photo("2019:12:24 18:30:00"),
		),
		(
			format!("{photos}/Christmas 2019/IMG_2041.jpg.json"),
			br#"{"title":"IMG_2041.jpg","description":"Grandma's kitchen",
				"photoTakenTime":{"timestamp":"1545672600"},
				"creationTime":{"timestamp":"1577300000"},
				"geoData":{"latitude":59.33,"longitude":18.07}}"#
				.to_vec(),
		),
		(
			format!("{photos}/Christmas 2019/IMG_2041-edited.jpg"),
			photo("2019:12:24 18:31:00"),
		),
		(
			format!("{photos}/Photos from 2020/{cut}.jpg"),
			photo("2020:07:01 12:00:00"),
		),
		(
			format!("{photos}/Photos from 2020/{cut}.json"),
			format!(
				r#"{{"title":"{long}","photoTakenTime":{{"timestamp":"1593597600"}},
				"geoData":{{"latitude":0.0,"longitude":0.0}}}}"#
			)
			.into_bytes(),
		),
		(
			format!("{photos}/Photos from 2020/scan.jpg"),
			photo("2020:08:01 09:00:00"),
		),
		(
			format!("{photos}/Photos from 2020/lost.jpg.json"),
			br#"{"title":"lost.jpg","photoTakenTime":{"timestamp":"1596272400"}}"#.to_vec(),
		),
		(
			"Takeout/Drive/notes.txt".to_owned(),
			b"not a photo".to_vec(),
		),
	]
}

/// Writes the files into zip parts: the JSON files in the second, as Takeout may split them.
fn zip_parts(at: &Path, files: &[(String, Vec<u8>)]) -> Vec<PathBuf> {
	let parts = [at.join("takeout-001.zip"), at.join("takeout-002.zip")];
	let mut writers = parts
		.iter()
		.map(|path| zip::ZipWriter::new(File::create(path).expect("creates")))
		.collect::<Vec<_>>();
	let options = zip::write::SimpleFileOptions::default()
		.compression_method(zip::CompressionMethod::Deflated);
	for (path, bytes) in files {
		let part = usize::from(
			Path::new(path)
				.extension()
				.is_some_and(|extension| extension.eq_ignore_ascii_case("json")),
		);
		writers[part].start_file(path, options).expect("starts");
		writers[part].write_all(bytes).expect("writes");
	}
	for writer in writers {
		writer.finish().expect("finishes");
	}
	parts.to_vec()
}

/// Writes the files into one tgz part.
fn tgz_part(at: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
	let path = at.join("takeout-001.tgz");
	let gz = flate2::write::GzEncoder::new(
		File::create(&path).expect("creates"),
		flate2::Compression::fast(),
	);
	let mut builder = tar::Builder::new(gz);
	for (name, bytes) in files {
		let mut header = tar::Header::new_gnu();
		header.set_size(bytes.len() as u64);
		header.set_mode(0o644);
		header.set_mtime(1_700_000_000);
		header.set_cksum();
		builder
			.append_data(&mut header, name, bytes.as_slice())
			.expect("appends");
	}
	builder.into_inner().expect("ends").finish().expect("ends");
	path
}

/// Writes the files into a folder, as an archive unpacked, with the files a system leaves in
/// folders.
fn unpacked(at: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
	let mut files = files.to_vec();
	files.push((
		"Takeout/Google Photos/Christmas 2019/.DS_Store".to_owned(),
		b"Finder's".to_vec(),
	));
	files.push((
		"Takeout/Google Photos/Christmas 2019/desktop.ini".to_owned(),
		b"[.ShellClassInfo]".to_vec(),
	));
	for (name, bytes) in &files {
		let path = at.join(name);
		fs::create_dir_all(path.parent().expect("a folder")).expect("makes");
		fs::write(path, bytes).expect("writes");
	}
	at.to_owned()
}

fn values(library: &Library, entity: EntityId, field: &FieldName) -> Vec<Value> {
	let day = Dating::today(now(), 0).expect("a day").day;
	library
		.entity(library.personal_space(), entity, day)
		.expect("reads")
		.expect("the entity")
		.fields
		.into_iter()
		.filter(|shown| shown.field == *field)
		.flat_map(|shown| shown.values.into_iter().map(|value| value.value))
		.collect()
}

/// The file of the library named `name`.
fn named(library: &Library, name: &str) -> EntityId {
	library
		.find_entity(library.personal_space(), name)
		.expect("finds the file")
}

/// Imports the parts, and returns the report with each photo's origin, by its path.
fn import(
	library: &mut Library,
	parts: &[PathBuf],
	rng: &mut TestRng,
) -> (nibkeep_import::TakeoutReport, BTreeMap<String, EntityId>) {
	let takeout = Takeout::open(parts).expect("opens");
	let space = library.personal_space();
	let mut origins = BTreeMap::new();
	let report = takeout
		.import(
			library,
			(space, GOOGLE_PHOTOS),
			&now,
			rng,
			&mut |_, imported| {
				if let Some(origin) = imported.added.as_ref().and_then(|added| added.origin) {
					origins.insert(imported.path.clone(), origin);
				}
				Ok(())
			},
		)
		.expect("imports");
	(report, origins)
}

/// Checks that the photo whose name Takeout cut short has its name as uploaded on its
/// origin, from the source Google Photos, and no place, as 0, 0 is nowhere.
fn checks_the_name_cut_short(library: &Library, origins: &BTreeMap<String, EntityId>) {
	let lake = named(
		library,
		"a weekend at the lake with the whole family and.jpg",
	);
	nibkeep_testkit::assert_empty!(values(library, lake, &LATITUDE));
	let origin = origins[&format!(
		"Takeout/Google Photos/Photos from 2020/{}.jpg",
		"a weekend at the lake with the whole family and"
	)];
	assert_eq!(
		values(library, origin, &ORIGIN_OF),
		[Value::Reference(lake)]
	);
	assert_eq!(
		values(library, origin, &SOURCE_NAME),
		[Value::Text(
			"a weekend at the lake with the whole family and the dog.jpg".to_owned()
		)]
	);
	let source = library
		.sources(library.personal_space())
		.expect("lists")
		.into_iter()
		.find(|source| source.name == GOOGLE_PHOTOS)
		.expect("the source");
	assert_eq!(
		values(library, origin, &FROM_SOURCE),
		[Value::Reference(source.id)]
	);
	assert_eq!(source.found, 4);
}

fn checks_the_import(name: &str, parts: &dyn Fn(&Path) -> Vec<PathBuf>) {
	let mut rng = TestRng::new(name);
	let mut library = library(name, &mut rng);
	let parts = parts(&folder(name));
	let (report, origins) = import(&mut library, &parts, &mut rng);
	assert_eq!(report.added, 4, "{report:?}");
	assert_eq!(report.undescribed, 1, "the scan: {report:?}");
	assert_eq!(report.albums, 1, "{report:?}");
	assert_eq!(report.edits, 1, "{report:?}");
	assert_eq!(report.left_out, 1, "Drive's file: {report:?}");
	assert_eq!(
		report.without_photo,
		["Takeout/Google Photos/Photos from 2020/lost.jpg.json"],
		"{report:?}"
	);
	nibkeep_testkit::assert_empty!(report.failed);

	// The corrected date wins over the photo's own, and the place and description come.
	let christmas = named(&library, "IMG_2041.jpg");
	assert_eq!(
		values(&library, christmas, &TAKEN),
		[Value::Date(
			"2018-12-24T17:30:00Z".parse().expect("a moment")
		)]
	);
	assert_eq!(
		values(&library, christmas, &DESCRIPTION),
		[Value::Text("Grandma's kitchen".to_owned())]
	);
	assert_eq!(values(&library, christmas, &LATITUDE).len(), 1);
	// Its edited copy, which shares its JSON file, is linked to it as an edit, whichever of
	// the two came first in the archive.
	let edited = named(&library, "IMG_2041-edited.jpg");
	assert_eq!(
		values(&library, edited, &EDIT_OF),
		[Value::Reference(christmas)]
	);
	// In the album's folder, inside the source's folder.
	let [Value::Reference(album)] = values(&library, christmas, &IN_FOLDER)[..] else {
		panic!("in one folder");
	};
	assert_eq!(
		values(&library, album, &fields::NAME),
		[Value::Name(nibkeep_model::Name::new("Christmas 2019"))]
	);
	assert_eq!(
		values(&library, album, &DESCRIPTION),
		[Value::Text("At Grandma's".to_owned())]
	);
	let [Value::Reference(google)] = values(&library, album, &IN_FOLDER)[..] else {
		panic!("inside the source's folder");
	};
	assert_eq!(
		values(&library, google, &fields::NAME),
		[Value::Name(nibkeep_model::Name::new(GOOGLE_PHOTOS))]
	);
	assert!(
		library
			.types(library.personal_space())
			.expect("types")
			.get(&FOLDER)
			.is_some()
	);
	checks_the_name_cut_short(&library, &origins);

	// Imported again, every photo is known, and nothing is read again, however the parts are
	// named.
	let named_otherwise: Vec<PathBuf> = parts
		.iter()
		.map(|part| {
			part.parent()
				.expect("a folder")
				.join(".")
				.join(part.file_name().expect("a name"))
		})
		.collect();
	let (again, _) = import(&mut library, &named_otherwise, &mut rng);
	assert_eq!(
		(again.added, again.known, again.edits),
		(0, 4, 0),
		"{again:?}"
	);
}

#[test]
fn a_takeout_in_zip_parts_is_imported_with_its_json_files_and_albums() {
	checks_the_import("takeout in zip parts", &|at| zip_parts(at, &takeout()));
}

#[test]
fn a_takeout_in_a_tgz_part_is_imported_alike() {
	checks_the_import("takeout in a tgz part", &|at| {
		vec![tgz_part(at, &takeout())]
	});
}

#[test]
fn a_takeout_unpacked_into_a_folder_is_imported_alike() {
	checks_the_import("takeout unpacked", &|at| vec![unpacked(at, &takeout())]);
}

#[test]
fn a_photo_from_takeout_and_from_a_phone_is_stored_once_with_both_sources() {
	let mut rng = TestRng::new("takeout and phone");
	let mut library = library("takeout and phone", &mut rng);
	let space = library.personal_space();
	let phone = folder("takeout and phone, phone");
	fs::write(phone.join("IMG_2041.jpg"), photo("2019:12:24 18:30:00")).expect("writes");
	let old_phone = library
		.source_named(space, "Mom's old phone", now(), &mut rng)
		.expect("names it");
	let from = FileFrom {
		source: Some(old_phone),
		..FileFrom::default()
	};
	let added = library
		.add_file_from(space, &phone.join("IMG_2041.jpg"), &from, now(), &mut rng)
		.expect("adds");
	let parts = zip_parts(&folder("takeout and phone"), &takeout());
	let (report, origins) = import(&mut library, &parts, &mut rng);
	assert_eq!(report.found_again, 1, "{report:?}");
	let from_takeout = origins["Takeout/Google Photos/Christmas 2019/IMG_2041.jpg"];
	assert_ne!(Some(from_takeout), added.origin);
	assert_eq!(
		values(&library, from_takeout, &ORIGIN_OF),
		[Value::Reference(added.file)]
	);
}

#[test]
fn an_edit_saved_as_another_format_is_linked_to_the_photo_not_to_its_live_video() {
	let folder_of = "Takeout/Google Photos/Photos from 2021";
	let files = |video_first: bool| {
		let mut files = vec![
			(
				format!("{folder_of}/IMG_3000.HEIC.json"),
				br#"{"title":"IMG_3000.HEIC","photoTakenTime":{"timestamp":"1625140800"}}"#
					.to_vec(),
			),
			(
				format!("{folder_of}/IMG_3000-edited.jpg"),
				photo("2021:07:01 14:00:00"),
			),
			(
				format!("{folder_of}/IMG_3000.HEIC"),
				b"a HEIC photo, as NibKeep reads none".to_vec(),
			),
		];
		let video = (
			format!("{folder_of}/IMG_3000.MP4"),
			b"a Live Photo's video".to_vec(),
		);
		if video_first {
			files.insert(0, video);
		} else {
			files.push(video);
		}
		files
	};
	for (name, video_first) in [("live edit, folder", false), ("live edit, zip", true)] {
		let mut rng = TestRng::new(name);
		let mut library = library(name, &mut rng);
		let at = folder(name);
		let parts = if video_first {
			zip_parts(&at, &files(true))
		} else {
			vec![unpacked(&at, &files(false))]
		};
		let (report, _) = import(&mut library, &parts, &mut rng);
		assert_eq!((report.added, report.edits), (3, 1), "{name}: {report:?}");
		let edited = named(&library, "IMG_3000-edited.jpg");
		let heic = named(&library, "IMG_3000.HEIC");
		assert_eq!(
			values(&library, edited, &EDIT_OF),
			[Value::Reference(heic)],
			"{name}"
		);
	}
}
