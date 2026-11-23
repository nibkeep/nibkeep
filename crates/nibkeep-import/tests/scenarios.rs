//! Scenario S3 of the design guidelines, consolidation: photos from two old phones, a Google
//! Takeout archive, a DVD and an old hard drive come into one library; identical files are
//! stored once, resized copies are linked to their originals, never merged, and the
//! original names, dates and sources are kept ([design: S3], [technical design: importers]).
//!
//! [design: S3]: https://github.com/nibkeep/nibkeep/blob/main/docs/Design.md#5-scenarios
//! [technical design: importers]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#912-importers-and-where-files-came-from

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::imageops::FilterType;
use image::{DynamicImage, Rgb, RgbImage};
use nibkeep_base::{EntityId, Hash};
use nibkeep_import::{GOOGLE_PHOTOS, Takeout};
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	AddedFile, Dating, FileFrom, Library, NewLibrary, NewProtection, PassphraseParams, Thumbnail,
	Thumbnailer,
};
use nibkeep_std::{COPY_OF, DESCRIPTION, FROM_SOURCE, ORIGIN_OF, PATH, SOURCE_PATH, TAKEN};
use nibkeep_testkit::media::Photo;
use nibkeep_testkit::{TestRng, millis_at};

fn now() -> u64 {
	millis_at("2026-11-11", 10, 0)
}

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

fn jpeg(picture: &DynamicImage, quality: u8) -> Vec<u8> {
	let mut bytes = Vec::new();
	image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
		.encode_image(picture)
		.expect("encodes");
	bytes
}

/// A phone's photo of `seed`'s scene, taken at `taken` local time, as EXIF writes it.
fn phone_photo(seed: u32, taken: &'static str) -> Vec<u8> {
	Photo {
		make: "Samsung",
		model: "Galaxy S7",
		taken,
		offset: None,
		size: (2000, 1500),
		orientation: None,
		place: None,
	}
	.jpeg_of(&jpeg(&scene(seed, 2000, 1500), 92))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes the folder");
	fs::write(&path, bytes).expect("writes");
	path
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

/// The household's library, and what each import added.
struct Household {
	library: Library,
	rng: TestRng,
	/// Each file added, by its source and its path there.
	added: Vec<(String, String, AddedFile)>,
}

impl Household {
	/// Adds every file of a folder from a source, as `nib file add FOLDER --from NAME` does,
	/// and makes their thumbnails.
	fn add_folder(&mut self, source: &str, folder: &Path, files: &[(&str, Vec<u8>)]) {
		let space = self.library.personal_space();
		let id = self
			.library
			.source_named(space, source, now(), &mut self.rng)
			.expect("names the source");
		let mut contents = Vec::new();
		for (name, bytes) in files {
			let path = write(folder, name, bytes);
			let from = FileFrom {
				source: Some(id),
				source_path: Some((*name).to_owned()),
				..FileFrom::default()
			};
			let added = self
				.library
				.add_file_from(space, &path, &from, now(), &mut self.rng)
				.expect("adds");
			contents.push(added.content);
			self.added
				.push((source.to_owned(), (*name).to_owned(), added));
		}
		self.thumbnails(&contents);
	}

	/// Imports a Takeout archive in one zip part, as `nib import` does, making
	/// thumbnails at the end.
	fn add_takeout(&mut self, part: &Path) {
		let space = self.library.personal_space();
		let takeout = Takeout::open(&[part.to_owned()]).expect("opens");
		let mut added = Vec::new();
		let report = takeout
			.import(
				&mut self.library,
				(space, GOOGLE_PHOTOS),
				&now,
				&mut self.rng,
				&mut |_, imported| {
					if let Some(file) = &imported.added {
						added.push((imported.path.clone(), file.clone()));
					}
					Ok(())
				},
			)
			.expect("imports");
		nibkeep_testkit::assert_empty!(report.failed);
		let contents: Vec<Hash> = added.iter().map(|(_, file)| file.content).collect();
		self.thumbnails(&contents);
		for (path, file) in added {
			self.added.push((GOOGLE_PHOTOS.to_owned(), path, file));
		}
	}

	fn thumbnails(&mut self, contents: &[Hash]) {
		let space = self.library.personal_space();
		self.library
			.make_thumbnails(space, contents, now(), &mut self.rng)
			.expect("makes thumbnails");
	}

	/// The file added from `source` at `path`.
	fn file(&self, source: &str, path: &str) -> &AddedFile {
		let Some((_, _, added)) = self
			.added
			.iter()
			.find(|(from, at, _)| from == source && at == path)
		else {
			panic!("{path} from {source}");
		};
		added
	}
}

/// A Takeout archive of Google Photos in one zip part.
fn takeout(at: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
	let path = at.join("takeout-20261111T100000Z-001.zip");
	let mut zip = zip::ZipWriter::new(File::create(&path).expect("creates"));
	let options = zip::write::SimpleFileOptions::default()
		.compression_method(zip::CompressionMethod::Deflated);
	for (name, bytes) in files {
		zip.start_file(name, options).expect("starts");
		zip.write_all(bytes).expect("writes");
	}
	zip.finish().expect("finishes");
	path
}

/// The household's library, with a thumbnailer.
fn household() -> Household {
	let mut rng = TestRng::new("s03");
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "s03");
	let mut library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "desktop", "nib"),
		&NewProtection::Passphrase(
			"correct horse",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		),
		now(),
		&mut rng,
	)
	.expect("creates");
	library.make_thumbnails_with(Arc::new(Decoders));
	Household {
		library,
		rng,
		added: Vec::new(),
	}
}

/// Where Takeout keeps the household's photos of 2019.
const PHOTOS: &str = "Takeout/Google Photos/Photos from 2019";

/// Imports two old phones, a Takeout archive, a DVD and an old drive.
fn import_everything(household: &mut Household) {
	let files = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "s03, files");

	// Christmas on Mom's old phone; the dog on both phones, sent from one to the other; the
	// beach on Dad's.
	let christmas = phone_photo(1, "2019:12:24 18:30:00");
	let dog = phone_photo(2, "2019:06:02 10:15:00");
	let beach = phone_photo(3, "2019:08:10 16:45:00");
	household.add_folder(
		"Mom's old phone",
		&files.join("mom"),
		&[
			("DCIM/Camera/IMG_1.jpg", christmas.clone()),
			("DCIM/Camera/IMG_2.jpg", dog.clone()),
		],
	);
	household.add_folder(
		"Dad's old phone",
		&files.join("dad"),
		&[
			("DCIM/Camera/received.jpg", dog),
			("DCIM/Camera/IMG_7.jpg", beach.clone()),
		],
	);
	// Google Photos keeps Christmas resized, as storage saver does, and the beach as it was,
	// with a description someone wrote there.
	let photos = PHOTOS;
	let part = takeout(
		&files,
		&[
			(
				format!("{photos}/IMG_1.jpg"),
				jpeg(
					&scene(1, 2000, 1500).resize_exact(1000, 750, FilterType::Triangle),
					75,
				),
			),
			(
				format!("{photos}/IMG_1.jpg.json"),
				br#"{"title":"IMG_1.jpg","photoTakenTime":{"timestamp":"1577208600"}}"#.to_vec(),
			),
			(format!("{photos}/IMG_7.jpg"), beach),
			(
				format!("{photos}/IMG_7.jpg.json"),
				br#"{"title":"IMG_7.jpg","description":"Low tide",
					"photoTakenTime":{"timestamp":"1565448300"}}"#
					.to_vec(),
			),
		],
	);
	household.add_takeout(&part);
	// A DVD of scans, and an old drive with a backup of Mom's phone.
	household.add_folder(
		"The holiday DVD",
		&files.join("dvd"),
		&[("Scans/1998 lake.jpg", jpeg(&scene(4, 1200, 800), 85))],
	);
	household.add_folder(
		"The grey drive",
		&files.join("drive"),
		&[("Backup/Phone/IMG_1.jpg", christmas)],
	);
}

#[test]
fn s03_consolidation() {
	let mut household = household();
	import_everything(&mut household);
	let photos = PHOTOS;

	// Identical files are stored once, with an origin from each source.
	let original = household
		.file("Mom's old phone", "DCIM/Camera/IMG_1.jpg")
		.clone();
	let on_drive = household.file("The grey drive", "Backup/Phone/IMG_1.jpg");
	assert_eq!(on_drive.file, original.file);
	assert!(on_drive.known);
	let dog = household
		.file("Mom's old phone", "DCIM/Camera/IMG_2.jpg")
		.file;
	assert_eq!(
		household
			.file("Dad's old phone", "DCIM/Camera/received.jpg")
			.file,
		dog
	);
	let beach = household
		.file("Dad's old phone", "DCIM/Camera/IMG_7.jpg")
		.file;
	let from_google = household.file(GOOGLE_PHOTOS, &format!("{photos}/IMG_7.jpg"));
	assert_eq!(from_google.file, beach);
	let library = &household.library;
	let space = library.personal_space();
	let mut sources: Vec<(String, usize)> = library
		.sources(space)
		.expect("lists")
		.into_iter()
		.map(|source| (source.name, source.found))
		.collect();
	sources.sort();
	assert_eq!(
		sources,
		[
			("Dad's old phone".to_owned(), 2),
			(GOOGLE_PHOTOS.to_owned(), 2),
			("Mom's old phone".to_owned(), 2),
			("The grey drive".to_owned(), 1),
			("The holiday DVD".to_owned(), 1),
		]
	);

	// The resized copy is linked to its original, and stays a file of its own.
	let resized = household.file(GOOGLE_PHOTOS, &format!("{photos}/IMG_1.jpg"));
	assert_ne!(resized.file, original.file);
	assert_eq!(
		values(library, resized.file, &COPY_OF),
		[Value::Reference(original.file)]
	);
	nibkeep_testkit::assert_empty!(values(library, original.file, &COPY_OF));
	checks_what_is_kept(&household, (&original, on_drive), beach);
}

/// Checks that names, dates and sources are kept: each origin's path in its source, and on
/// the device; the photo's own date where Google agrees; Google's description.
fn checks_what_is_kept(
	household: &Household,
	(original, on_drive): (&AddedFile, &AddedFile),
	beach: EntityId,
) {
	let library = &household.library;
	let space = library.personal_space();
	let origin = on_drive.origin.expect("an origin of its own");
	assert_eq!(
		values(library, origin, &ORIGIN_OF),
		[Value::Reference(original.file)]
	);
	assert_eq!(
		values(library, origin, &SOURCE_PATH),
		[Value::Text("Backup/Phone/IMG_1.jpg".to_owned())]
	);
	assert!(values(library, origin, &PATH).iter().any(|path| {
		path.as_text()
			.is_some_and(|path| path.ends_with("IMG_1.jpg"))
	}));
	let drive = library
		.sources(space)
		.expect("lists")
		.into_iter()
		.find(|source| source.name == "The grey drive")
		.expect("the drive");
	assert_eq!(
		values(library, origin, &FROM_SOURCE),
		[Value::Reference(drive.id)]
	);
	assert_eq!(
		values(library, original.file, &TAKEN),
		[Value::Date("2019-12-24T18:30:00".parse().expect("a time"))]
	);
	assert_eq!(
		values(library, beach, &DESCRIPTION),
		[Value::Text("Low tide".to_owned())]
	);
	assert_eq!(
		values(library, beach, &TAKEN),
		[Value::Date(
			"2019-08-10T16:45:00+02".parse().expect("a time")
		)],
		"the photo's own local time, with the offset Google's moment shows"
	);
}
