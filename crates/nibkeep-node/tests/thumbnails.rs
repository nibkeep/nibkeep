//! Thumbnails: the device that adds a picture makes its thumbnail, if it reads its format,
//! and every replica keeps it; a purge of the content purges it, a move takes it along, and
//! exports leave it out ([technical design: thumbnails]).
//!
//! [technical design: thumbnails]: ../../../docs/Technical-design.md#98-thumbnails

mod common;

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{library, now, pair_named, sync_in_memory, this_day};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use nibkeep_base::{EntityId, Hash};
use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	Behind, ExportKind, ExportProblem, Library, MoveHistory, MoveSecrets, NewProtection,
	PairingCode, Thumbnail, ThumbnailCounts, Thumbnailer, ThumbnailsMade,
};
use nibkeep_std::CONTENT;
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::{Photo, Video};

/// What nib makes thumbnails with, under a way of its own, such as `nibkeep 1`.
struct Decoders(&'static str);

impl Thumbnailer for Decoders {
	fn made_with(&self) -> &str {
		self.0
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

/// A thumbnailer that breaks on every picture.
struct Breaks;

impl Thumbnailer for Breaks {
	fn made_with(&self) -> &'static str {
		"breaks 1"
	}

	fn reads(&self, _: &str) -> bool {
		true
	}

	fn make(&self, _: &[u8]) -> Option<Thumbnail> {
		panic!("a decoder that a picture broke");
	}
}

fn decoders() -> Arc<dyn Thumbnailer> {
	Arc::new(Decoders(nibkeep_thumbnails::MADE_WITH))
}

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::write(&path, bytes).expect("writes");
	path
}

/// A picture whose top is one colour and bottom another, in a format.
fn picture(width: u32, height: u32, top: [u8; 3], format: ImageFormat) -> Vec<u8> {
	let picture = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |_, y| {
		if y < height / 2 {
			Rgb(top)
		} else {
			Rgb([30, 30, 220])
		}
	}));
	let mut bytes = Vec::new();
	picture
		.write_to(&mut Cursor::new(&mut bytes), format)
		.expect("encodes");
	bytes
}

/// A photo taken at `taken`, as EXIF writes it.
fn taken(taken: &'static str) -> Photo<'static> {
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken,
		offset: Some("+01:00"),
		size: (800, 600),
		orientation: None,
		place: None,
	}
}

/// A second device of the library's user, paired and synced with it.
fn desk(laptop: &mut Library, test: &str, rng: &mut TestRng) -> Library {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{test}, desk"));
	let code = PairingCode::generate(rng);
	let (paired, joined) = pair_named(laptop, &directory, "desk", (&code, &code), rng);
	paired.expect("paired");
	joined.expect("joined").library
}

fn sync(one: &mut Library, other: &mut Library, at: u64) {
	let (first, second) = sync_in_memory(one, other, at, (1, 2));
	first.expect("syncs");
	second.expect("syncs");
}

/// Adds a file of `bytes` to the personal space, and returns it and its content.
fn add(
	library: &mut Library,
	files: &Path,
	(name, bytes): (&str, &[u8]),
	at: u64,
	rng: &mut TestRng,
) -> (EntityId, Hash) {
	let path = write(files, name, bytes);
	let added = library
		.add_file(library.personal_space(), &path, at, rng)
		.expect("adds");
	(added.file, added.content)
}

fn made(made: usize) -> ThumbnailsMade {
	ThumbnailsMade {
		made,
		..ThumbnailsMade::default()
	}
}

#[test]
fn a_picture_added_gets_a_thumbnail_that_every_replica_keeps() {
	let test = "thumbnail kept";
	let mut rng = TestRng::new(test);
	let (_, mut laptop) = library(test, &mut rng);
	laptop.make_thumbnails_with(decoders());
	let space = laptop.personal_space();
	let files = folder(test);
	let sea = picture(2000, 1500, [220, 30, 30], ImageFormat::Png);
	let (file, content) = add(&mut laptop, &files, ("sea.png", &sea), now(), &mut rng);
	let making = laptop
		.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	assert_eq!(making, made(1));
	let thumbnail = laptop
		.thumbnail(space, file)
		.expect("reads")
		.expect("a thumbnail");
	assert_eq!(thumbnail.size, Some((512, 384)));
	assert_eq!(thumbnail.made_with.as_deref(), Some("nibkeep 1"));
	assert!(thumbnail.held);
	let jpeg = laptop
		.thumbnail_jpeg(space, &thumbnail)
		.expect("reads")
		.expect("held");
	let shown = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg).expect("a JPEG");
	assert_eq!((shown.width(), shown.height()), (512, 384));
	// Made once.
	assert_eq!(
		laptop
			.make_thumbnails(space, &[content], now() + 2_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade::default()
	);
	assert_eq!(
		laptop.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 1,
			with: 1
		}
	);

	// The desk, which makes none, shows the laptop's.
	let mut desk = desk(&mut laptop, test, &mut rng);
	sync(&mut desk, &mut laptop, now() + 100_000);
	let theirs = desk
		.thumbnail(space, file)
		.expect("reads")
		.expect("a thumbnail");
	assert_eq!(theirs, thumbnail);
	assert_eq!(
		desk.thumbnail_jpeg(space, &theirs)
			.expect("reads")
			.expect("held"),
		jpeg
	);
	assert_eq!(
		desk.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 1,
			with: 1
		}
	);
}

#[test]
fn formats_the_thumbnailer_does_not_read_get_none() {
	let test = "thumbnail formats";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let files = folder(test);
	// The start of a HEIC photo, as an iPhone writes it, which the iPhone reads.
	let mut heic = vec![0, 0, 0, 24];
	heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
	heic.resize(4096, 7);
	let video = Video {
		created: Some(1_577_210_400),
		duration_millis: 12_345,
		size: (1920, 1080),
	}
	.mp4();
	let mut contents = Vec::new();
	for (name, bytes) in [
		("photo.heic", heic.as_slice()),
		("film.mp4", video.as_slice()),
		("notes.txt", b"Milk.\n".as_slice()),
	] {
		contents.push(add(&mut library, &files, (name, bytes), now(), &mut rng).1);
	}
	assert_eq!(
		library
			.make_thumbnails(space, &contents, now() + 1_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade::default()
	);
	// A photo and a video, which their devices may make thumbnails of.
	assert_eq!(
		library.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 2,
			with: 0
		}
	);
	assert_eq!(library.missing_thumbnails(space).expect("lists"), []);
}

/// A phone's app makes thumbnails with the phone's own decoders, of the files it adds, and
/// hands them over: the engine records those of pictures and videos, once.
#[test]
fn thumbnails_that_the_program_made_itself_are_recorded() {
	let test = "thumbnail handed over";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	let space = library.personal_space();
	let files = folder(test);
	let mut heic = vec![0, 0, 0, 24];
	heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
	heic.resize(4096, 7);
	let video = Video {
		created: Some(1_577_210_400),
		duration_millis: 12_345,
		size: (1920, 1080),
	}
	.mp4();
	let (photo, of_photo) = add(&mut library, &files, ("photo.heic", &heic), now(), &mut rng);
	let (film, of_film) = add(&mut library, &files, ("film.mp4", &video), now(), &mut rng);
	let (notes, of_notes) = add(
		&mut library,
		&files,
		("notes.txt", b"Milk.\n"),
		now(),
		&mut rng,
	);
	let small = nibkeep_thumbnails::make(&picture(800, 600, [220, 30, 30], ImageFormat::Png))
		.expect("makes");
	let thumbnail = Thumbnail {
		jpeg: small.jpeg,
		width: small.width,
		height: small.height,
		picture_hash: Some(small.picture_hash),
	};
	let handed = [
		(of_photo, thumbnail.clone()),
		(of_film, thumbnail.clone()),
		// No picture, and a content that the space has no file of.
		(of_notes, thumbnail.clone()),
		(Hash::of(b"elsewhere"), thumbnail.clone()),
	];
	let recorded = library
		.record_thumbnails_made(space, &handed, "android 1", now() + 1_000, &mut rng)
		.expect("records");
	assert_eq!(recorded.made, 2);
	for file in [photo, film] {
		let shown = library
			.thumbnail(space, file)
			.expect("reads")
			.expect("a thumbnail");
		assert_eq!(shown.size, Some((512, 384)));
		assert_eq!(shown.made_with.as_deref(), Some("android 1"));
		let jpeg = library.thumbnail_jpeg(space, &shown).expect("reads");
		assert_eq!(jpeg.as_deref(), Some(thumbnail.jpeg.as_slice()));
	}
	assert_eq!(library.thumbnail(space, notes).expect("reads"), None);
	// Recorded once.
	let again = library
		.record_thumbnails_made(space, &handed, "android 1", now() + 2_000, &mut rng)
		.expect("records");
	assert_eq!(again.made, 0);
	assert_eq!(
		library.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 2,
			with: 2
		}
	);
}

#[test]
fn a_picture_that_cannot_be_read_gets_none_and_the_others_do() {
	let test = "thumbnail unreadable";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let files = folder(test);
	let whole = picture(800, 600, [220, 30, 30], ImageFormat::Png);
	let (_, cut) = add(
		&mut library,
		&files,
		("cut.png", &whole[..whole.len() / 2]),
		now(),
		&mut rng,
	);
	let (good, content) = add(
		&mut library,
		&files,
		(
			"good.jpg",
			&picture(800, 600, [30, 220, 30], ImageFormat::Jpeg),
		),
		now(),
		&mut rng,
	);
	assert_eq!(
		library
			.make_thumbnails(space, &[cut, content], now() + 1_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade {
			made: 1,
			unreadable: 1,
			not_here: 0,
			copies_linked: 0,
			linking_failed: false,
		}
	);
	assert!(library.thumbnail(space, good).expect("reads").is_some());

	// A decoder that a picture breaks makes nothing, and the library goes on.
	let (_, other) = add(
		&mut library,
		&files,
		("other.png", &picture(300, 200, [9, 9, 9], ImageFormat::Png)),
		now(),
		&mut rng,
	);
	library.make_thumbnails_with(Arc::new(Breaks));
	assert_eq!(
		library
			.make_thumbnails(space, &[other], now() + 2_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade {
			made: 0,
			unreadable: 1,
			not_here: 0,
			copies_linked: 0,
			linking_failed: false,
		}
	);
}

#[test]
fn neither_secret_files_nor_trees_files_get_one_nor_without_a_thumbnailer() {
	let test = "thumbnail never";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	let space = library.personal_space();
	let files = folder(test);
	let scan = picture(800, 600, [220, 30, 30], ImageFormat::Png);
	// Without a thumbnailer.
	let (_, content) = add(&mut library, &files, ("scan.png", &scan), now(), &mut rng);
	assert_eq!(
		library
			.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade::default()
	);
	assert_eq!(library.missing_thumbnails(space).expect("lists"), []);
	library.make_thumbnails_with(decoders());
	assert_eq!(library.missing_thumbnails(space).expect("lists"), [content]);

	// A secret file of a picture.
	library
		.set_up_secrets(
			&NewProtection::Passphrase(
				"battery staple",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 2_000,
			&mut rng,
		)
		.expect("sets up the secrets");
	let passport = picture(600, 400, [30, 220, 30], ImageFormat::Png);
	let secret = library
		.add_secret_file(
			space,
			"passport.png",
			passport.as_slice(),
			&[],
			now() + 3_000,
			&mut rng,
		)
		.expect("adds");
	assert_eq!(
		library
			.make_thumbnails(space, &[secret.content], now() + 4_000, &mut rng)
			.expect("makes"),
		ThumbnailsMade::default()
	);

	// A tree's picture.
	let root =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{test}, tree"));
	write(
		&root,
		"diagram.png",
		&picture(640, 480, [9, 9, 220], ImageFormat::Png),
	);
	library
		.start_tree(space, &root, Some("Start"), now() + 5_000, &mut rng)
		.expect("starts");
	assert_eq!(library.missing_thumbnails(space).expect("lists"), [content]);
	assert_eq!(
		library.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 1,
			with: 0
		}
	);
}

#[test]
fn a_purge_of_the_content_purges_its_thumbnail_on_every_replica() {
	let test = "thumbnail purged";
	let mut rng = TestRng::new(test);
	let (_, mut laptop) = library(test, &mut rng);
	laptop.make_thumbnails_with(decoders());
	let space = laptop.personal_space();
	let files = folder(test);
	let beach = picture(1200, 900, [220, 30, 30], ImageFormat::Jpeg);
	let (file, content) = add(&mut laptop, &files, ("beach.jpg", &beach), now(), &mut rng);
	laptop
		.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	let thumbnail = laptop
		.thumbnail(space, file)
		.expect("reads")
		.expect("a thumbnail");
	let mut desk = desk(&mut laptop, test, &mut rng);
	sync(&mut desk, &mut laptop, now() + 100_000);
	assert!(
		desk.file_content(space, &thumbnail.content, u64::MAX)
			.expect("reads")
			.is_some()
	);

	let purge = laptop
		.purgeable(space, file, &CONTENT, None)
		.expect("finds");
	assert_eq!(purge.thumbnails, [thumbnail.entity]);
	assert_eq!(purge.also, []);
	assert!(purge.contents.contains(&thumbnail.content), "{purge:?}");
	let purged = laptop
		.purge(space, file, &CONTENT, None, now() + 200_000, &mut rng)
		.expect("purges");
	assert_eq!(purged.thumbnails, [thumbnail.entity]);
	let gone = |library: &Library| {
		library
			.file_content(space, &thumbnail.content, u64::MAX)
			.expect("reads")
			.is_none()
			&& library
				.entity(space, thumbnail.entity, this_day())
				.expect("reads")
				.is_none_or(|view| view.fields.is_empty())
	};
	assert!(gone(&laptop));
	sync(&mut desk, &mut laptop, now() + 300_000);
	assert!(gone(&desk));
	assert_eq!(laptop.thumbnail(space, file).expect("reads"), None);
}

#[test]
fn a_move_takes_the_thumbnail_along() {
	let test = "thumbnail moved";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let personal = library.personal_space();
	let household = library
		.create_space("Household", now(), &mut rng)
		.expect("makes the space");
	let files = folder(test);
	let garden = picture(1000, 800, [30, 220, 30], ImageFormat::Jpeg);
	let (file, content) = add(
		&mut library,
		&files,
		("garden.jpg", &garden),
		now(),
		&mut rng,
	);
	library
		.make_thumbnails(personal, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	let thumbnail = library
		.thumbnail(personal, file)
		.expect("reads")
		.expect("a thumbnail");
	let jpeg = library
		.thumbnail_jpeg(personal, &thumbnail)
		.expect("reads")
		.expect("held");
	// What the person sees moving is the file alone.
	let preview = library
		.move_preview(personal, file, household)
		.expect("previews");
	assert_eq!(preview.entities, [file]);
	let moved = library
		.move_entity(
			personal,
			file,
			household,
			MoveHistory::Bring,
			Behind::Keep,
			MoveSecrets::Leave,
			now() + 2_000,
			&mut rng,
		)
		.expect("moves");
	let there = library
		.thumbnail(household, moved)
		.expect("reads")
		.expect("a thumbnail there");
	assert_eq!(there.content, thumbnail.content);
	assert_eq!(
		library
			.thumbnail_jpeg(household, &there)
			.expect("reads")
			.expect("held"),
		jpeg
	);
	assert_eq!(
		library
			.moved_into(personal, thumbnail.entity)
			.expect("reads")
			.map(|(space, _)| space),
		Some(household)
	);
}

#[test]
fn missing_thumbnails_are_made_newest_first() {
	let test = "thumbnails missing";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	let space = library.personal_space();
	let files = folder(test);
	let jpeg = |top| picture(800, 600, top, ImageFormat::Jpeg);
	// Added before thumbnails were made: two photos, and a picture that says nothing of
	// when it was taken, which counts from when it was added.
	let (_, christmas) = add(
		&mut library,
		&files,
		(
			"christmas.jpg",
			&taken("2019:12:24 18:30:00").jpeg_of(&jpeg([200, 0, 0])),
		),
		now(),
		&mut rng,
	);
	let (_, screenshot) = add(
		&mut library,
		&files,
		(
			"screenshot.png",
			&picture(800, 600, [0, 200, 0], ImageFormat::Png),
		),
		now() + 1_000,
		&mut rng,
	);
	let (_, summer) = add(
		&mut library,
		&files,
		(
			"summer.jpg",
			&taken("2021:07:01 12:00:00").jpeg_of(&jpeg([0, 0, 200])),
		),
		now() + 2_000,
		&mut rng,
	);
	library.make_thumbnails_with(decoders());
	assert_eq!(
		library.missing_thumbnails(space).expect("lists"),
		[screenshot, summer, christmas]
	);
	let mut rounds = Vec::new();
	let made_now = library
		.make_missing_thumbnails(space, 2, now() + 3_000, &mut rng, &mut |made, of| {
			rounds.push((made.made, of));
		})
		.expect("makes");
	assert_eq!(made_now, made(3));
	assert_eq!(rounds, [(2, 3), (3, 3)]);
	assert_eq!(library.missing_thumbnails(space).expect("lists"), []);
	assert_eq!(
		library.thumbnail_counts(space).expect("counts"),
		ThumbnailCounts {
			pictures: 3,
			with: 3
		}
	);
}

#[test]
fn of_two_thumbnails_of_one_content_every_device_shows_the_same() {
	let test = "thumbnails twice";
	let mut rng = TestRng::new(test);
	let (_, mut laptop) = library(test, &mut rng);
	let space = laptop.personal_space();
	let mut desk = desk(&mut laptop, test, &mut rng);
	sync(&mut desk, &mut laptop, now() + 100_000);
	let files = folder(test);
	let desk_files = folder(&format!("{test}, desk"));
	// Both add the same pictures before they sync, and each makes thumbnails: the desk a
	// newer way for the first, the same way for the second, after the laptop.
	let first = picture(900, 600, [200, 0, 0], ImageFormat::Png);
	let second = picture(900, 600, [0, 200, 0], ImageFormat::Png);
	let mut on_laptop = Vec::new();
	let mut on_desk = Vec::new();
	for (at, (name, bytes)) in [("first.png", &first), ("second.png", &second)]
		.into_iter()
		.enumerate()
	{
		let at = now() + 200_000 + 10_000 * at as u64;
		on_laptop.push(add(&mut laptop, &files, (name, bytes), at, &mut rng));
		on_desk.push(add(&mut desk, &desk_files, (name, bytes), at, &mut rng));
	}
	laptop.make_thumbnails_with(decoders());
	let contents: Vec<Hash> = on_laptop.iter().map(|(_, content)| *content).collect();
	laptop
		.make_thumbnails(space, &contents, now() + 300_000, &mut rng)
		.expect("makes");
	desk.make_thumbnails_with(Arc::new(Decoders("nibkeep 2")));
	desk.make_thumbnails(space, &contents[..1], now() + 310_000, &mut rng)
		.expect("makes");
	desk.make_thumbnails_with(decoders());
	desk.make_thumbnails(space, &contents[1..], now() + 320_000, &mut rng)
		.expect("makes");
	let shown = |library: &Library, file: EntityId| {
		library
			.thumbnail(space, file)
			.expect("reads")
			.expect("a thumbnail")
	};
	// The desk's, made the newer way, and the laptop's, recorded first.
	let chosen = [shown(&desk, on_desk[0].0), shown(&laptop, on_laptop[1].0)];
	assert_ne!(chosen[1], shown(&desk, on_desk[1].0));
	sync(&mut desk, &mut laptop, now() + 400_000);
	for (at, chosen) in chosen.iter().enumerate() {
		assert_eq!(
			chosen.made_with.as_deref(),
			Some(["nibkeep 2", "nibkeep 1"][at])
		);
		for library in [&laptop, &desk] {
			assert_eq!(shown(library, on_laptop[at].0), *chosen, "{at}");
			assert_eq!(shown(library, on_desk[at].0), *chosen, "{at}");
		}
	}
}

#[test]
fn exports_leave_thumbnails_out() {
	let test = "thumbnails exported";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let files = folder(test);
	let (_, content) = add(
		&mut library,
		&files,
		(
			"hill.png",
			&picture(900, 600, [200, 0, 0], ImageFormat::Png),
		),
		now(),
		&mut rng,
	);
	library
		.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	for kind in [ExportKind::Full, ExportKind::Current] {
		let export = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{test}, {kind:?}"),
		);
		let exported = library.export(space, &export, kind).expect("exports");
		assert_eq!(exported.files, 1, "{kind:?}");
		let entities = fs::read_to_string(export.join("entities.jsonl")).expect("reads");
		// The type's definition stays, as every definition does; no thumbnail is there.
		assert!(
			!entities.contains(r#""types":["std.thumbnail"]"#),
			"{kind:?}"
		);
		assert!(entities.contains(r#""types":["std.file"]"#), "{kind:?}");
		assert_eq!(
			library.verify_export(&export).expect("checks").problems,
			Vec::<ExportProblem>::new()
		);
	}
}

#[test]
fn purging_a_thumbnails_link_alone_leaves_the_photo() {
	let test = "thumbnail link purged";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let files = folder(test);
	let (file, content) = add(
		&mut library,
		&files,
		(
			"lake.png",
			&picture(900, 600, [30, 30, 220], ImageFormat::Png),
		),
		now(),
		&mut rng,
	);
	library
		.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	let thumbnail = library
		.thumbnail(space, file)
		.expect("reads")
		.expect("a thumbnail");
	// What a thumbnail was made of, it names: purging the name takes no content.
	let purged = library
		.purge(
			space,
			thumbnail.entity,
			&nibkeep_std::THUMBNAIL_OF,
			None,
			now() + 2_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(purged.contents, []);
	assert_eq!(purged.also, []);
	assert!(
		library
			.file_content(space, &content, u64::MAX)
			.expect("reads")
			.is_some()
	);
	// It is made again where asked.
	assert_eq!(library.missing_thumbnails(space).expect("lists"), [content]);
}

#[test]
fn a_purge_keeps_the_picture_that_another_thumbnail_holds_too() {
	let test = "thumbnail shared";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let files = folder(test);
	// One picture in two files that differ, whose thumbnails are one JPEG.
	let pixels = |format| picture(900, 600, [200, 100, 0], format);
	let (first, first_content) = add(
		&mut library,
		&files,
		("dune.png", &pixels(ImageFormat::Png)),
		now(),
		&mut rng,
	);
	let (second, second_content) = add(
		&mut library,
		&files,
		("dune.bmp", &pixels(ImageFormat::Bmp)),
		now(),
		&mut rng,
	);
	library
		.make_thumbnails(
			space,
			&[first_content, second_content],
			now() + 1_000,
			&mut rng,
		)
		.expect("makes");
	let shown = |library: &Library, file| library.thumbnail(space, file).expect("reads");
	let theirs = shown(&library, second).expect("a thumbnail");
	assert_eq!(
		shown(&library, first).expect("a thumbnail").content,
		theirs.content
	);
	let purged = library
		.purge(space, first, &CONTENT, None, now() + 2_000, &mut rng)
		.expect("purges");
	assert_eq!(purged.also, []);
	assert!(!purged.contents.contains(&theirs.content));
	assert_eq!(shown(&library, second), Some(theirs.clone()));
	assert!(
		library
			.thumbnail_jpeg(space, &theirs)
			.expect("reads")
			.is_some()
	);
}

#[test]
fn a_thumbnail_that_a_file_staying_behind_shows_is_copied_by_a_move() {
	let test = "thumbnail copied";
	let mut rng = TestRng::new(test);
	let (_, mut laptop) = library(test, &mut rng);
	laptop.make_thumbnails_with(decoders());
	let personal = laptop.personal_space();
	let mut desk = desk(&mut laptop, test, &mut rng);
	sync(&mut desk, &mut laptop, now() + 100_000);
	// Both add the same photo before they sync: two files of one content.
	let bytes = picture(900, 600, [0, 150, 0], ImageFormat::Png);
	let (moving, content) = add(
		&mut laptop,
		&folder(test),
		("field.png", &bytes),
		now() + 200_000,
		&mut rng,
	);
	let (staying, _) = add(
		&mut desk,
		&folder(&format!("{test}, desk")),
		("field.png", &bytes),
		now() + 200_000,
		&mut rng,
	);
	laptop
		.make_thumbnails(personal, &[content], now() + 210_000, &mut rng)
		.expect("makes");
	sync(&mut desk, &mut laptop, now() + 300_000);
	let thumbnail = laptop
		.thumbnail(personal, staying)
		.expect("reads")
		.expect("a thumbnail");
	let household = laptop
		.create_space("Household", now() + 310_000, &mut rng)
		.expect("makes the space");
	let moved = laptop
		.move_entity(
			personal,
			moving,
			household,
			MoveHistory::Bring,
			Behind::Keep,
			MoveSecrets::Leave,
			now() + 320_000,
			&mut rng,
		)
		.expect("moves");
	// The file that stays keeps it; the one that moved has a copy of its own.
	assert_eq!(
		laptop.thumbnail(personal, staying).expect("reads"),
		Some(thumbnail.clone())
	);
	assert_eq!(
		laptop
			.moved_into(personal, thumbnail.entity)
			.expect("reads"),
		None
	);
	let there = laptop
		.thumbnail(household, moved)
		.expect("reads")
		.expect("a thumbnail there");
	assert_ne!(there.entity, thumbnail.entity);
	assert_eq!(there.content, thumbnail.content);
}

#[test]
fn a_secret_files_move_takes_no_thumbnail_of_its_content() {
	let test = "thumbnail of a secret";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let personal = library.personal_space();
	library
		.set_up_secrets(
			&NewProtection::Passphrase(
				"battery staple",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			now() + 1_000,
			&mut rng,
		)
		.expect("sets up the secrets");
	// A scan added both as a file, which has a thumbnail, and as a secret file.
	let scan = picture(800, 600, [120, 0, 120], ImageFormat::Png);
	let (_, content) = add(
		&mut library,
		&folder(test),
		("scan.png", &scan),
		now() + 2_000,
		&mut rng,
	);
	library
		.make_thumbnails(personal, &[content], now() + 3_000, &mut rng)
		.expect("makes");
	let secret = library
		.add_secret_file(
			personal,
			"passport.png",
			scan.as_slice(),
			&[],
			now() + 4_000,
			&mut rng,
		)
		.expect("adds");
	assert_eq!(secret.content, content);
	let household = library
		.create_space("Household", now() + 5_000, &mut rng)
		.expect("makes the space");
	library
		.move_entity(
			personal,
			secret.file,
			household,
			MoveHistory::Bring,
			Behind::Keep,
			MoveSecrets::Bring,
			now() + 6_000,
			&mut rng,
		)
		.expect("moves");
	assert_eq!(
		library.thumbnail_counts(personal).expect("counts"),
		ThumbnailCounts {
			pictures: 1,
			with: 1
		}
	);
	let jpeg = library
		.thumbnail(
			personal,
			library.find_entity(personal, "scan.png").expect("finds"),
		)
		.expect("reads")
		.expect("a thumbnail")
		.content;
	assert_eq!(
		library
			.file_content(household, &jpeg, u64::MAX)
			.expect("reads"),
		None
	);
}

#[test]
fn searches_never_find_thumbnails() {
	let test = "thumbnails unsearched";
	let mut rng = TestRng::new(test);
	let (_, mut library) = library(test, &mut rng);
	library.make_thumbnails_with(decoders());
	let space = library.personal_space();
	let (_, content) = add(
		&mut library,
		&folder(test),
		(
			"sunset.png",
			&picture(900, 600, [250, 120, 0], ImageFormat::Png),
		),
		now(),
		&mut rng,
	);
	library
		.make_thumbnails(space, &[content], now() + 1_000, &mut rng)
		.expect("makes");
	for words in ["jpeg", "image", "nibkeep"] {
		let found = library.search(space, words).expect("searches");
		assert!(
			found
				.iter()
				.all(|listed| !listed.types.contains(&nibkeep_std::THUMBNAIL)),
			"{words}: {found:?}"
		);
	}
}

#[test]
fn a_move_without_history_purges_the_thumbnails_of_versions_it_leaves() {
	let test = "thumbnails left behind";
	let mut rng = TestRng::new(test);
	let (laptop_folder, mut laptop) = library(test, &mut rng);
	laptop.make_thumbnails_with(decoders());
	let personal = laptop.personal_space();
	let mut desk = desk(&mut laptop, test, &mut rng);
	// On disks of their own, so that the desk's copy counts as another.
	common::devices::on_disk(&laptop_folder, "thumbnails left behind, laptop");
	common::devices::on_disk(
		&nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR")).join(format!("{test}, desk")),
		"thumbnails left behind, desk",
	);
	let mut at = now() + 100_000;
	sync(&mut desk, &mut laptop, at);
	let household = laptop
		.create_space("Household", at + 100, &mut rng)
		.expect("makes the space");
	let (drawing, versions, [old, new]) = two_versions(&mut laptop, at, &mut rng);
	at += 1_000;
	sync(&mut desk, &mut laptop, at);

	let moved = laptop
		.move_entity(
			personal,
			drawing,
			household,
			MoveHistory::Leave,
			Behind::Purge,
			MoveSecrets::Leave,
			at + 100,
			&mut rng,
		)
		.expect("moves");
	// Once the desk holds the new entity and proves its copy of the content that moved, what
	// stayed is purged, the old version's thumbnail with the old version.
	for round in 0..4 {
		sync(&mut desk, &mut laptop, at + 1_000 * (round + 2));
	}
	for library in [&mut laptop, &mut desk] {
		library
			.record_own_endpoint(at + 10_000, &mut rng)
			.expect("records");
	}
	at += 100_000;
	sync(&mut desk, &mut laptop, at);
	sync(&mut desk, &mut laptop, at + 1_000);
	// The drawing's, and its thumbnail's, which moved along.
	assert_eq!(
		common::remove_in_memory(&mut laptop, &mut desk, at + 2_000, (0, 1)).moves,
		2
	);
	assert_eq!(
		laptop
			.file_content(personal, &versions[0], u64::MAX)
			.expect("reads"),
		None
	);
	assert_eq!(
		laptop
			.file_content(personal, &old.content, u64::MAX)
			.expect("reads"),
		None
	);
	assert!(
		laptop
			.entity(personal, old.entity, this_day())
			.expect("reads")
			.is_none_or(|view| view.fields.is_empty())
	);
	let there = laptop
		.thumbnail(household, moved)
		.expect("reads")
		.expect("a thumbnail there");
	assert_eq!(there.content, new.content);
}

/// A drawing of the personal space with two versions, each with its thumbnail, held by
/// nothing else, as no origin records where they were found: the drawing, its versions, and
/// their thumbnails.
fn two_versions(
	laptop: &mut Library,
	at: u64,
	rng: &mut TestRng,
) -> (EntityId, Vec<Hash>, [nibkeep_node::ThumbnailOf; 2]) {
	let personal = laptop.personal_space();
	let versions: Vec<Hash> = [[200, 0, 0], [0, 0, 200]]
		.into_iter()
		.map(|top| {
			laptop
				.import_blob(
					personal,
					picture(900, 600, top, ImageFormat::Png).as_slice(),
					at + 200,
					rng,
				)
				.expect("seals")
		})
		.collect();
	let drawing = laptop
		.create_entity(
			personal,
			&[nibkeep_std::FILE],
			vec![
				(CONTENT, nibkeep_model::Value::Blob(versions[0])),
				(
					nibkeep_std::MEDIA_TYPE,
					nibkeep_model::Value::Text("image/png".to_owned()),
				),
			],
			at + 300,
			rng,
		)
		.expect("adds");
	laptop
		.make_thumbnails(personal, &versions[..1], at + 400, rng)
		.expect("makes");
	let old = laptop
		.thumbnail(personal, drawing)
		.expect("reads")
		.expect("a thumbnail");
	laptop
		.edit_entity(
			personal,
			drawing,
			vec![nibkeep_node::Edit::Set(
				CONTENT,
				vec![nibkeep_model::Value::Blob(versions[1])],
			)],
			&common::today(),
			at + 500,
			rng,
		)
		.expect("edits");
	laptop
		.make_thumbnails(personal, &versions[1..], at + 600, rng)
		.expect("makes");
	let new = laptop
		.thumbnail(personal, drawing)
		.expect("reads")
		.expect("a thumbnail");
	(drawing, versions, [old, new])
}
