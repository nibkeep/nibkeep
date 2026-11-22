//! Thumbnails of pictures, made with decoders in memory-safe Rust: the formats that every
//! system reads alike, JPEG, PNG, GIF, WebP, BMP and TIFF, and nothing else
//! ([technical design](../../../docs/Technical-design.md#98-thumbnails)). HEIC, AVIF, JPEG XL,
//! camera RAW and video are left to the phones that take them.
//!
//! A thumbnail is a JPEG whose longer side is 512 pixels, or the picture's own size if that
//! is smaller, at quality 80, turned upright as the picture's orientation says, with no
//! metadata. An animation's is its first frame, and a transparent picture's stands on white.
//! Camera RAW files that take the form of TIFF, as Nikon's, Sony's and DNG do, are not read:
//! their first picture is a small preview, not the photo.
//!
//! Each thumbnail comes with a picture hash, which copies of a picture share, resized or
//! saved again: a difference hash of 64 bits, each bit telling whether a part of the picture,
//! of a grid of 9 by 8 grey parts, is darker than the part on its right.
//!
//! The same decoders crop a picture: the part of it turned upright that a person chose, as
//! a JPEG at quality 92, or a PNG where the picture has transparent parts, with no
//! metadata.

use std::fmt;
use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits};

/// How these thumbnails are made, as a thumbnail records it: a later version that makes
/// them otherwise counts up, so that what an older one made can be told and made again.
pub const MADE_WITH: &str = "nibkeep 1";

/// The longer side of a thumbnail, in pixels.
pub const LONGER_SIDE: u32 = 512;

/// The quality of a thumbnail's JPEG, from 1 to 100.
pub const QUALITY: u8 = 80;

/// The quality of a cropped picture's JPEG, from 1 to 100: high, as it stands for the
/// picture, where a thumbnail only shows it small.
pub const CROP_QUALITY: u8 = 92;

/// The most bytes a picture may have: larger ones are not read.
pub const MOST_BYTES: u64 = 256 * 1024 * 1024;

/// The most pixels a picture may have on a side, as JPEG allows.
const MOST_SIDE: u32 = 65_535;

/// The most memory that decoding one picture may take: a picture made to exhaust memory
/// fails alone. A photo of 100 megapixels takes 300 MiB.
const MOST_MEMORY: u64 = 512 * 1024 * 1024;

/// The media types read, as their first bytes tell them.
const READ: [&str; 6] = [
	"image/jpeg",
	"image/png",
	"image/gif",
	"image/webp",
	"image/bmp",
	"image/tiff",
];

/// A thumbnail: a JPEG, its size in pixels, and the picture hash of what it shows.
#[derive(Clone, PartialEq, Eq)]
pub struct Made {
	/// The JPEG's bytes.
	pub jpeg: Vec<u8>,
	/// Its width.
	pub width: u32,
	/// Its height.
	pub height: u32,
	/// The picture's hash, which its copies share, resized or saved again, but for a few
	/// bits: see [`picture_hash`].
	pub picture_hash: u64,
}

impl fmt::Debug for Made {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Made")
			.field("jpeg", &format_args!("{} bytes", self.jpeg.len()))
			.field("width", &self.width)
			.field("height", &self.height)
			.field("picture_hash", &format_args!("{:016x}", self.picture_hash))
			.finish()
	}
}

/// Why a picture gave no thumbnail: a format not read, or a picture that could not be read,
/// as the decoder says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable(pub String);

impl fmt::Display for Unreadable {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

impl std::error::Error for Unreadable {}

/// Whether pictures of this media type, as their first bytes tell it, such as `image/jpeg`,
/// are read.
#[must_use]
pub fn reads(media_type: &str) -> bool {
	READ.contains(&media_type)
}

/// Makes the thumbnail of a picture, from its bytes, whatever their media type says.
///
/// # Errors
///
/// Returns [`Unreadable`] if the bytes are not a picture of a format read, are larger than
/// [`MOST_BYTES`], or cannot be decoded within the limits of pixels and memory.
pub fn make(picture: &[u8]) -> Result<Made, Unreadable> {
	let unreadable = |error: image::ImageError| Unreadable(error.to_string());
	let (mut picture, orientation) = decode(picture)?;
	let (width, height) = (picture.width(), picture.height());
	if width == 0 || height == 0 {
		return Err(Unreadable("a picture without pixels".to_owned()));
	}
	// On white before it is made smaller, so that the colour that transparent pixels hide
	// does not bleed into the edges of what shows.
	if picture.color().has_alpha() {
		picture = DynamicImage::ImageRgb8(on_white(&picture));
	}
	if width.max(height) > LONGER_SIDE {
		picture = picture.thumbnail(LONGER_SIDE, LONGER_SIDE);
	}
	// Turned once small: a quarter turn keeps it within the square either way.
	picture.apply_orientation(orientation);
	let picture_hash = picture_hash(&picture);
	let rgb = picture.to_rgb8();
	let mut jpeg = Vec::new();
	JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
		.write_image(
			rgb.as_raw(),
			rgb.width(),
			rgb.height(),
			image::ExtendedColorType::Rgb8,
		)
		.map_err(unreadable)?;
	Ok(Made {
		jpeg,
		width: rgb.width(),
		height: rgb.height(),
		picture_hash,
	})
}

/// A part of a picture, as [`crop`] makes it: its bytes, their media type, and its size.
#[derive(Clone, PartialEq, Eq)]
pub struct Cropped {
	/// The picture's bytes: a JPEG, or a PNG.
	pub bytes: Vec<u8>,
	/// `image/jpeg` or `image/png`.
	pub media_type: &'static str,
	/// Its width, in pixels.
	pub width: u32,
	/// Its height, in pixels.
	pub height: u32,
}

impl fmt::Debug for Cropped {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Cropped")
			.field("bytes", &format_args!("{} bytes", self.bytes.len()))
			.field("media_type", &self.media_type)
			.field("width", &self.width)
			.field("height", &self.height)
			.finish()
	}
}

/// A part of a picture: the rectangle from `left` and `top`, `width` by `height` pixels, of
/// the picture turned upright as its orientation says, which the rectangle must lie within.
/// It is a JPEG at [`CROP_QUALITY`], or a PNG if the picture has transparent parts, with
/// no metadata, upright.
///
/// # Errors
///
/// Returns [`Unreadable`] as [`make`] does, or if the rectangle is empty or reaches past the
/// picture.
pub fn crop(
	picture: &[u8],
	(left, top): (u32, u32),
	(width, height): (u32, u32),
) -> Result<Cropped, Unreadable> {
	let unreadable = |error: image::ImageError| Unreadable(error.to_string());
	let (mut picture, orientation) = decode(picture)?;
	picture.apply_orientation(orientation);
	let fits = |start: u32, length: u32, side: u32| {
		length > 0 && start.checked_add(length).is_some_and(|end| end <= side)
	};
	if !fits(left, width, picture.width()) || !fits(top, height, picture.height()) {
		return Err(Unreadable(format!(
			"the part {left},{top},{width},{height} is not within the picture, {} by {} pixels",
			picture.width(),
			picture.height()
		)));
	}
	let part = picture.crop_imm(left, top, width, height);
	let mut bytes = Vec::new();
	let media_type = if part.color().has_alpha() {
		let rgba = part.to_rgba8();
		PngEncoder::new(&mut bytes)
			.write_image(
				rgba.as_raw(),
				width,
				height,
				image::ExtendedColorType::Rgba8,
			)
			.map_err(unreadable)?;
		"image/png"
	} else {
		let rgb = part.to_rgb8();
		JpegEncoder::new_with_quality(&mut bytes, CROP_QUALITY)
			.write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
			.map_err(unreadable)?;
		"image/jpeg"
	};
	Ok(Cropped {
		bytes,
		media_type,
		width,
		height,
	})
}

/// Decodes a picture of a format read, within the limits of bytes, pixels and memory, and
/// says how to turn it upright.
fn decode(picture: &[u8]) -> Result<(DynamicImage, Orientation), Unreadable> {
	let unreadable = |error: image::ImageError| Unreadable(error.to_string());
	if picture.len() as u64 > MOST_BYTES {
		return Err(Unreadable("larger than pictures are read".to_owned()));
	}
	let mut reader = ImageReader::new(Cursor::new(picture))
		.with_guessed_format()
		.map_err(|error| Unreadable(error.to_string()))?;
	match reader.format() {
		Some(
			ImageFormat::Jpeg
			| ImageFormat::Png
			| ImageFormat::Gif
			| ImageFormat::WebP
			| ImageFormat::Bmp
			| ImageFormat::Tiff,
		) => {}
		_ => return Err(Unreadable("not a format read".to_owned())),
	}
	if reader.format() == Some(ImageFormat::Tiff) && camera_raw(picture) {
		return Err(Unreadable("camera RAW, not read".to_owned()));
	}
	let mut limits = Limits::default();
	limits.max_image_width = Some(MOST_SIDE);
	limits.max_image_height = Some(MOST_SIDE);
	limits.max_alloc = Some(MOST_MEMORY);
	reader.limits(limits.clone());
	let mut decoder = reader.into_decoder().map_err(unreadable)?;
	// The decoded picture must fit the memory allowed too, which the decoder does not check:
	// `DynamicImage::from_decoder` allocates it whole, and an allocation that fails ends the
	// program, so a picture that only says it is huge would end it.
	// What is left is the decoder's own, as `ImageReader::decode` shares it.
	if limits.reserve(decoder.total_bytes()).is_err() {
		return Err(Unreadable("too large once decoded".to_owned()));
	}
	decoder.set_limits(limits).map_err(unreadable)?;
	let orientation = decoder.orientation().map_err(unreadable)?;
	let picture = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
	Ok((picture, orientation))
}

/// Measures a thumbnail that another decoder made, as a phone's own: the width and height
/// of the JPEG, and the picture hash of what it shows.
///
/// # Errors
///
/// Returns [`Unreadable`] if the bytes are no JPEG that decodes, or its longer side is
/// more than [`LONGER_SIDE`].
pub fn measure(jpeg: &[u8]) -> Result<(u32, u32, u64), Unreadable> {
	let unreadable = |error: image::ImageError| Unreadable(error.to_string());
	let mut reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
	let mut limits = Limits::default();
	limits.max_image_width = Some(LONGER_SIDE);
	limits.max_image_height = Some(LONGER_SIDE);
	limits.max_alloc = Some(MOST_MEMORY);
	reader.limits(limits);
	let picture = reader.decode().map_err(unreadable)?;
	if picture.width() == 0 || picture.height() == 0 {
		return Err(Unreadable("a picture without pixels".to_owned()));
	}
	Ok((picture.width(), picture.height(), picture_hash(&picture)))
}

/// The difference hash of a picture, upright: the picture made grey and as small as 9 by 8
/// parts, each the mean of what it covers, and a bit for each part but the last of its row,
/// set if the part is darker than the next, from the top left, the first bit the highest.
/// Resizing a picture or saving it again changes the means a little, and so only the bits of
/// parts almost as bright as their neighbours; another picture differs in about half.
#[must_use]
pub fn picture_hash(picture: &DynamicImage) -> u64 {
	let small = picture.resize_exact(9, 8, FilterType::Triangle).to_luma8();
	let mut hash = 0_u64;
	for y in 0..8 {
		for x in 0..8 {
			let darker = small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0];
			hash = hash << 1 | u64::from(darker);
		}
	}
	hash
}

/// Whether a TIFF is a camera's RAW file: one that says it is a DNG, that keeps further
/// pictures in sub-directories, as RAW files keep the photo, or whose first picture is a
/// reduced one, a preview; or Canon's CR2, which marks itself so. Ordinary TIFFs, such as
/// scans, are none of these.
fn camera_raw(tiff: &[u8]) -> bool {
	const NEW_SUBFILE_TYPE: u16 = 0x00fe;
	const SUB_IFDS: u16 = 0x014a;
	const DNG_VERSION: u16 = 0xc612;
	let little = match tiff.get(..2) {
		Some(b"II") => true,
		Some(b"MM") => false,
		_ => return false,
	};
	if tiff.get(8..10) == Some(b"CR".as_slice()) {
		return true;
	}
	let u16_at = |at: usize| -> Option<u16> {
		let bytes: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
		Some(if little {
			u16::from_le_bytes(bytes)
		} else {
			u16::from_be_bytes(bytes)
		})
	};
	let u32_at = |at: usize| -> Option<u32> {
		let bytes: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
		Some(if little {
			u32::from_le_bytes(bytes)
		} else {
			u32::from_be_bytes(bytes)
		})
	};
	let Some(first) = u32_at(4).and_then(|at| usize::try_from(at).ok()) else {
		return false;
	};
	let entries = u16_at(first).unwrap_or(0);
	(0..usize::from(entries)).any(|entry| {
		let at = first + 2 + entry * 12;
		match u16_at(at) {
			Some(SUB_IFDS | DNG_VERSION) => true,
			// A short or a long, whose value starts the entry's last four bytes.
			Some(NEW_SUBFILE_TYPE) => {
				let value = match u16_at(at + 2) {
					Some(3) => u16_at(at + 8).map(u32::from),
					_ => u32_at(at + 8),
				};
				value.is_some_and(|value| value & 1 == 1)
			}
			_ => false,
		}
	})
}

/// A picture in 8-bit RGB, where any transparency stands on white, as a page shows it.
fn on_white(picture: &DynamicImage) -> image::RgbImage {
	let rgba = picture.to_rgba8();
	let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
	for (into, from) in rgb.pixels_mut().zip(rgba.pixels()) {
		let alpha = u16::from(from.0[3]);
		for channel in 0..3 {
			let blended = (u16::from(from.0[channel]) * alpha + 255 * (255 - alpha) + 127) / 255;
			into.0[channel] = u8::try_from(blended).unwrap_or(u8::MAX);
		}
	}
	rgb
}
