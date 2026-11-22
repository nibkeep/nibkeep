//! Thumbnails of pictures of each format read, turned upright, on white, and refusals of
//! what is not read; and parts of pictures cropped, upright.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use nibkeep_thumbnails::{LONGER_SIDE, Made, Unreadable, crop, make, reads};

const RED: [u8; 3] = [220, 30, 30];
const BLUE: [u8; 3] = [30, 30, 220];

/// A picture written in a format.
fn encoded(picture: &DynamicImage, format: ImageFormat) -> Vec<u8> {
	let mut bytes = Vec::new();
	picture
		.write_to(&mut Cursor::new(&mut bytes), format)
		.expect("encodes");
	bytes
}

/// A picture whose top half is red and bottom half blue.
fn halves(width: u32, height: u32) -> DynamicImage {
	DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |_, y| {
		if y < height / 2 { Rgb(RED) } else { Rgb(BLUE) }
	}))
}

/// The thumbnail decoded back, as a viewer shows it.
fn shown(made: &Made) -> RgbImage {
	let picture = image::load_from_memory_with_format(&made.jpeg, ImageFormat::Jpeg)
		.expect("a JPEG")
		.to_rgb8();
	assert_eq!(
		(picture.width(), picture.height()),
		(made.width, made.height)
	);
	picture
}

/// Whether two colours are alike, as a JPEG keeps them.
fn alike(one: [u8; 3], other: [u8; 3]) -> bool {
	one.iter()
		.zip(other)
		.all(|(one, other)| one.abs_diff(other) <= 24)
}

#[test]
fn a_large_photo_becomes_a_jpeg_512_pixels_long_without_metadata() {
	let photo = encoded(&halves(2000, 1000), ImageFormat::Jpeg);
	let made = make(&photo).expect("makes");
	assert_eq!((made.width, made.height), (LONGER_SIDE, 256));
	let picture = shown(&made);
	assert!(alike(picture.get_pixel(100, 50).0, RED));
	assert!(alike(picture.get_pixel(100, 200).0, BLUE));
	// No EXIF, and so no place.
	assert!(!made.jpeg.windows(4).any(|window| window == b"Exif"));
	assert!(made.jpeg.len() < 40_000, "{} bytes", made.jpeg.len());
}

#[test]
fn a_small_picture_keeps_its_size() {
	let made = make(&encoded(&halves(120, 80), ImageFormat::Png)).expect("makes");
	assert_eq!((made.width, made.height), (120, 80));
}

#[test]
fn a_photo_is_turned_upright_as_its_orientation_says() {
	// Stored wide, red on top; orientation 6 says to turn it a quarter clockwise, so that it
	// shows tall, red on the right.
	let stored = encoded(&halves(600, 300), ImageFormat::Jpeg);
	let made = make(&with_orientation(&stored, 6)).expect("makes");
	assert_eq!((made.width, made.height), (256, LONGER_SIDE));
	let picture = shown(&made);
	assert!(alike(picture.get_pixel(200, 256).0, RED));
	assert!(alike(picture.get_pixel(50, 256).0, BLUE));
	assert!(!made.jpeg.windows(4).any(|window| window == b"Exif"));
}

#[test]
fn every_format_read_gives_a_thumbnail() {
	let picture = halves(800, 600);
	for format in [
		ImageFormat::Jpeg,
		ImageFormat::Png,
		ImageFormat::Gif,
		ImageFormat::WebP,
		ImageFormat::Bmp,
		ImageFormat::Tiff,
	] {
		let made = make(&encoded(&picture, format)).unwrap_or_else(|error| {
			panic!("{format:?}: {error}");
		});
		assert_eq!((made.width, made.height), (LONGER_SIDE, 384), "{format:?}");
		let shown = shown(&made);
		assert!(alike(shown.get_pixel(10, 10).0, RED), "{format:?}");
		assert!(alike(shown.get_pixel(10, 300).0, BLUE), "{format:?}");
		assert!(reads(format.to_mime_type()), "{format:?}");
	}
	let deep = DynamicImage::ImageRgb16(picture.to_rgb16());
	let made = make(&encoded(&deep, ImageFormat::Png)).expect("16 bits a channel");
	assert!(alike(shown(&made).get_pixel(10, 10).0, RED));
}

#[test]
fn transparency_stands_on_white() {
	let picture = DynamicImage::ImageRgba8(RgbaImage::from_fn(100, 100, |x, _| {
		if x < 50 {
			Rgba([0, 0, 0, 0])
		} else {
			Rgba([RED[0], RED[1], RED[2], 255])
		}
	}));
	let made = make(&encoded(&picture, ImageFormat::Png)).expect("makes");
	let shown = shown(&made);
	assert!(alike(shown.get_pixel(10, 50).0, [255, 255, 255]));
	assert!(alike(shown.get_pixel(90, 50).0, RED));
}

#[test]
fn an_animation_gives_its_first_frame() {
	use image::codecs::gif::GifEncoder;
	use image::{Delay, Frame};
	let frame = |colour: [u8; 3]| {
		Frame::from_parts(
			RgbaImage::from_pixel(64, 64, Rgba([colour[0], colour[1], colour[2], 255])),
			0,
			0,
			Delay::from_numer_denom_ms(100, 1),
		)
	};
	let mut gif = Vec::new();
	GifEncoder::new(&mut gif)
		.encode_frames([frame(BLUE), frame(RED)])
		.expect("encodes");
	let made = make(&gif).expect("makes");
	assert!(alike(shown(&made).get_pixel(32, 32).0, BLUE));
}

#[test]
fn what_is_not_read_gives_none() {
	// The start of a HEIC photo, as an iPhone writes it: left to the phone.
	let mut heic = vec![0, 0, 0, 24];
	heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
	heic.resize(4096, 0);
	assert!(make(&heic).is_err());
	assert!(!reads("image/heif"));
	assert!(!reads("video/mp4"));
	assert!(make(b"Milk.\n").is_err());
	assert!(make(&[]).is_err());
	// A photo cut short.
	let photo = encoded(&halves(800, 600), ImageFormat::Png);
	assert!(make(&photo[..photo.len() / 2]).is_err());
}

#[test]
fn a_picture_made_to_exhaust_memory_fails_alone() {
	// A BMP that says it is 60,000 pixels square, 10 GB once decoded, in 54 bytes.
	let mut bmp = Vec::new();
	bmp.extend_from_slice(b"BM");
	bmp.extend_from_slice(&54u32.to_le_bytes());
	bmp.extend_from_slice(&0u32.to_le_bytes());
	bmp.extend_from_slice(&54u32.to_le_bytes());
	bmp.extend_from_slice(&40u32.to_le_bytes());
	bmp.extend_from_slice(&60_000i32.to_le_bytes());
	bmp.extend_from_slice(&60_000i32.to_le_bytes());
	bmp.extend_from_slice(&1u16.to_le_bytes());
	bmp.extend_from_slice(&24u16.to_le_bytes());
	bmp.extend_from_slice(&[0; 24]);
	// Refused before the memory is asked for, which a system that promises memory it does
	// not have would give.
	assert_eq!(
		make(&bmp).err(),
		Some(Unreadable("too large once decoded".to_owned()))
	);
}

/// A JPEG with an EXIF segment that says only its orientation, put right after its start.
fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
	let mut tiff = Vec::new();
	tiff.extend_from_slice(b"MM\0\x2a");
	tiff.extend_from_slice(&8u32.to_be_bytes());
	tiff.extend_from_slice(&1u16.to_be_bytes());
	tiff.extend_from_slice(&0x0112u16.to_be_bytes());
	tiff.extend_from_slice(&3u16.to_be_bytes());
	tiff.extend_from_slice(&1u32.to_be_bytes());
	tiff.extend_from_slice(&orientation.to_be_bytes());
	tiff.extend_from_slice(&[0, 0]);
	tiff.extend_from_slice(&0u32.to_be_bytes());
	let mut segment = b"Exif\0\0".to_vec();
	segment.extend_from_slice(&tiff);
	let length = u16::try_from(segment.len() + 2).expect("short");
	let mut with = jpeg[..2].to_vec();
	with.extend_from_slice(&[0xFF, 0xE1]);
	with.extend_from_slice(&length.to_be_bytes());
	with.extend_from_slice(&segment);
	with.extend_from_slice(&jpeg[2..]);
	with
}

#[test]
fn camera_raw_files_shaped_as_tiff_are_not_read() {
	// The smallest TIFF whose first directory holds one entry, little-endian.
	let tiff = |tag: u16, kind: u16, value: u32| {
		let mut bytes = b"II*\0".to_vec();
		bytes.extend_from_slice(&8u32.to_le_bytes());
		bytes.extend_from_slice(&1u16.to_le_bytes());
		bytes.extend_from_slice(&tag.to_le_bytes());
		bytes.extend_from_slice(&kind.to_le_bytes());
		bytes.extend_from_slice(&1u32.to_le_bytes());
		bytes.extend_from_slice(&value.to_le_bytes());
		bytes.extend_from_slice(&0u32.to_le_bytes());
		bytes
	};
	// A DNG; a RAW file that keeps the photo in a sub-directory, as Nikon's and Sony's do;
	// and one whose first picture is a reduced preview.
	for raw in [
		tiff(0xc612, 1, 0x0000_0401),
		tiff(0x014a, 4, 64),
		tiff(0x00fe, 4, 1),
	] {
		let refused = make(&raw).expect_err("refused");
		assert!(refused.0.contains("RAW"), "{refused}");
	}
	// Canon's CR2 marks itself.
	let mut cr2 = tiff(0x0100, 3, 16);
	cr2.splice(8..8, *b"CR\x02\0");
	assert!(make(&cr2).expect_err("refused").0.contains("RAW"));
	// A scan is a TIFF like any other.
	assert!(make(&encoded(&halves(300, 200), ImageFormat::Tiff)).is_ok());
}

#[test]
fn transparent_edges_take_no_hidden_colour() {
	// Large enough to be made smaller, with hidden black behind the transparent half.
	let picture = DynamicImage::ImageRgba8(RgbaImage::from_fn(2001, 1000, |x, _| {
		if x < 1000 {
			Rgba([0, 0, 0, 0])
		} else {
			Rgba([RED[0], RED[1], RED[2], 255])
		}
	}));
	let made = make(&encoded(&picture, ImageFormat::Png)).expect("makes");
	let shown = shown(&made);
	// Between white and red, red never falls below its own; black would darken the edge.
	for x in 0..shown.width() {
		let [red, ..] = shown.get_pixel(x, shown.height() / 2).0;
		assert!(red >= 212, "{x}: {red}");
	}
}

/// A scene, as a photo of something shows: soft light and shade that `seed` places, so that
/// two seeds give two pictures.
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

#[test]
fn a_resized_copy_saved_again_has_almost_the_same_picture_hash_and_another_picture_not() {
	let original = make(&encoded(&scene(1, 2000, 1500), ImageFormat::Png)).expect("makes");
	let resized = scene(1, 2000, 1500).resize_exact(800, 600, FilterType::Triangle);
	let mut copy = Vec::new();
	image::codecs::jpeg::JpegEncoder::new_with_quality(&mut copy, 60)
		.encode_image(&resized)
		.expect("encodes");
	let copy = make(&copy).expect("makes");
	let apart = (original.picture_hash ^ copy.picture_hash).count_ones();
	assert!(apart <= 4, "{apart} bits apart");
	for seed in 2..6 {
		let other = make(&encoded(&scene(seed, 2000, 1500), ImageFormat::Png)).expect("makes");
		let apart = (original.picture_hash ^ other.picture_hash).count_ones();
		assert!(apart >= 12, "seed {seed}: {apart} bits apart");
	}
}

#[test]
fn a_thumbnail_made_elsewhere_is_measured_and_a_larger_picture_refused() {
	let made = make(&encoded(&scene(5, 1600, 1200), ImageFormat::Png)).expect("makes");
	let (width, height, hash) = nibkeep_thumbnails::measure(&made.jpeg).expect("measures");
	assert_eq!((width, height), (made.width, made.height));
	// Read back from the JPEG, the picture differs a little from the one it was made of.
	assert!((hash ^ made.picture_hash).count_ones() <= 4);
	// No thumbnail: larger than one, or no JPEG.
	let large = encoded(&halves(LONGER_SIDE + 1, 10), ImageFormat::Jpeg);
	assert!(nibkeep_thumbnails::measure(&large).is_err());
	let png = encoded(&halves(100, 80), ImageFormat::Png);
	assert!(nibkeep_thumbnails::measure(&png).is_err());
	assert!(nibkeep_thumbnails::measure(b"no picture").is_err());
}

#[test]
fn a_part_of_a_photo_is_cropped_as_a_jpeg_of_its_own_size_without_metadata() {
	let photo = encoded(&halves(600, 400), ImageFormat::Jpeg);
	// The lower right quarter, all blue.
	let part = crop(&photo, (300, 200), (300, 200)).expect("crops");
	assert_eq!(part.media_type, "image/jpeg");
	assert_eq!((part.width, part.height), (300, 200));
	let picture = image::load_from_memory_with_format(&part.bytes, ImageFormat::Jpeg)
		.expect("a JPEG")
		.to_rgb8();
	assert_eq!((picture.width(), picture.height()), (300, 200));
	assert!(alike(picture.get_pixel(150, 100).0, BLUE));
	assert!(!part.bytes.windows(4).any(|window| window == b"Exif"));
	// Across the middle: red above, blue below.
	let middle = crop(&photo, (0, 100), (600, 200)).expect("crops");
	let picture = image::load_from_memory(&middle.bytes)
		.expect("a picture")
		.to_rgb8();
	assert!(alike(picture.get_pixel(300, 20).0, RED));
	assert!(alike(picture.get_pixel(300, 180).0, BLUE));
}

#[test]
fn a_crop_is_of_the_picture_turned_upright() {
	// Stored wide, red on top; shown tall, red on the right: the right half is red.
	let stored = encoded(&halves(600, 300), ImageFormat::Jpeg);
	let turned = with_orientation(&stored, 6);
	let right = crop(&turned, (150, 0), (150, 600)).expect("crops");
	let picture = image::load_from_memory(&right.bytes)
		.expect("a picture")
		.to_rgb8();
	assert_eq!((picture.width(), picture.height()), (150, 600));
	assert!(alike(picture.get_pixel(75, 300).0, RED));
	// Past the upright picture's width, which is the stored one's height.
	assert!(crop(&turned, (200, 0), (150, 600)).is_err());
}

#[test]
fn a_transparent_picture_is_cropped_as_a_png_and_a_part_outside_refused() {
	let transparent = DynamicImage::ImageRgba8(RgbaImage::from_fn(80, 60, |x, _| {
		if x < 40 {
			Rgba([0, 0, 0, 0])
		} else {
			Rgba([30, 30, 220, 255])
		}
	}));
	let picture = encoded(&transparent, ImageFormat::Png);
	let part = crop(&picture, (0, 0), (60, 60)).expect("crops");
	assert_eq!(part.media_type, "image/png");
	let decoded = image::load_from_memory_with_format(&part.bytes, ImageFormat::Png)
		.expect("a PNG")
		.to_rgba8();
	assert_eq!(decoded.get_pixel(10, 10).0[3], 0, "still transparent");
	assert_eq!(decoded.get_pixel(50, 10).0, [30, 30, 220, 255]);
	for (at, size) in [((0, 0), (0, 10)), ((70, 0), (20, 10)), ((0, 59), (10, 2))] {
		let Err(Unreadable(why)) = crop(&picture, at, size) else {
			panic!("{at:?} {size:?} is cropped");
		};
		assert!(why.contains("not within"), "{why}");
	}
	assert!(crop(b"not a picture", (0, 0), (1, 1)).is_err());
}
