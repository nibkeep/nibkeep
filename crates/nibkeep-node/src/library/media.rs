//! What photos and videos say of themselves: when and where they were taken, by which
//! camera, and how large they are, read from their EXIF data or their tracks. Plugins take
//! this over once there are plugins ([7.1](https://github.com/nibkeep/nibkeep/blob/main/docs/Design.md#71-plugins)).

use std::fs::File;
use std::path::Path;

use nibkeep_model::{Decimal, FieldName, Quantity, TypeName, Value};
use nibkeep_std::{
	CAMERA_MAKE, CAMERA_MODEL, DURATION, HEIGHT, LATITUDE, LONGITUDE, PHOTO, TAKEN, VIDEO, WIDTH,
};
use nibkeep_time::Edtf;
use nom_exif::{
	EntryValue, Exif, ExifTag, GPSInfo, MediaKind, MediaParser, MediaSource, TrackInfo,
	TrackInfoTag,
};

/// The start of the time of MP4 and `QuickTime` headers, 1904-01-01T00:00:00Z, in seconds
/// since 1970.
const QUICKTIME_EPOCH: i64 = -2_082_844_800;

/// The longest duration a video can have that a camera recorded, rather than a header
/// that says nothing: ten years, in milliseconds.
const LONGEST_MILLIS: u64 = 10 * 366 * 24 * 60 * 60 * 1_000;

/// What a photo or a video says of itself: its type, and the values of its fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Recorded {
	pub(crate) kind: Option<TypeName>,
	pub(crate) values: Vec<(FieldName, Value)>,
}

/// Reads what the file at `path` says of itself, if it is a photo, with EXIF data that
/// names when it was taken or by which camera, or a video, with a picture of a size.
/// Anything else, and anything that cannot be read, says nothing: a file is added all the
/// same.
pub(crate) fn read(path: &Path) -> Recorded {
	let Ok(file) = File::open(path) else {
		return Recorded::default();
	};
	let Ok(source) = MediaSource::seekable(file) else {
		return Recorded::default();
	};
	let mut parser = MediaParser::new();
	match source.kind() {
		MediaKind::Image => parser
			.parse_exif(source)
			.map_or_default(|iter| photo(&iter.into())),
		MediaKind::Track => parser
			.parse_track(source)
			.map_or_default(|track| video(&track)),
	}
}

/// What EXIF data says of a photo, if it says when it was taken or by which camera.
fn photo(exif: &Exif) -> Recorded {
	let mut values = Vec::new();
	let offset = exif
		.get(ExifTag::OffsetTimeOriginal)
		.and_then(EntryValue::as_str);
	if let Some(taken) = exif
		.get(ExifTag::DateTimeOriginal)
		.and_then(|taken| moment(taken, offset))
	{
		values.push((TAKEN, Value::Date(taken)));
	}
	for (field, tag) in [(CAMERA_MAKE, ExifTag::Make), (CAMERA_MODEL, ExifTag::Model)] {
		if let Some(written) = exif.get(tag).and_then(text) {
			values.push((field, written));
		}
	}
	if values.is_empty() {
		return Recorded::default();
	}
	// A photo turned a quarter, as orientations 5 to 8 say, is as wide as it is stored high.
	let turned = exif
		.get(ExifTag::Orientation)
		.and_then(EntryValue::as_u16)
		.is_some_and(|orientation| (5..=8).contains(&orientation));
	let (across, down) = if turned {
		(HEIGHT, WIDTH)
	} else {
		(WIDTH, HEIGHT)
	};
	for (field, tags) in [
		(across, [ExifTag::ExifImageWidth, ExifTag::ImageWidth]),
		(down, [ExifTag::ExifImageHeight, ExifTag::ImageHeight]),
	] {
		if let Some(pixels) = tags.iter().find_map(|&tag| exif.get(tag).and_then(pixels)) {
			values.push((field, pixels));
		}
	}
	values.extend(place(exif.gps_info()));
	values.sort_by(|(one, _), (other, _)| one.cmp(other));
	Recorded {
		kind: Some(PHOTO),
		values,
	}
}

/// What a video's tracks say of it, if they hold a picture: audio comes as tracks too.
/// Which way the picture is turned is not read yet.
fn video(track: &TrackInfo) -> Recorded {
	let mut values = Vec::new();
	for (field, tag) in [(WIDTH, TrackInfoTag::Width), (HEIGHT, TrackInfoTag::Height)] {
		if let Some(pixels) = track.get(tag).and_then(pixels) {
			values.push((field, pixels));
		}
	}
	if values.is_empty() {
		return Recorded::default();
	}
	// A header without a creation time counts from 1904, where the time of its kind starts.
	if let Some(taken) = track
		.get(TrackInfoTag::CreateDate)
		.filter(
			|taken| !matches!(taken, EntryValue::DateTime(at) if at.timestamp() == QUICKTIME_EPOCH),
		)
		.and_then(|taken| moment(taken, None))
	{
		values.push((TAKEN, Value::Date(taken)));
	}
	for (field, tag) in [
		(CAMERA_MAKE, TrackInfoTag::Make),
		(CAMERA_MODEL, TrackInfoTag::Model),
	] {
		if let Some(written) = track.get(tag).and_then(text) {
			values.push((field, written));
		}
	}
	if let Some(duration) = track
		.get(TrackInfoTag::DurationMs)
		.and_then(EntryValue::as_u64)
		// A header without a time scale says the longest duration there is.
		.filter(|&millis| millis < LONGEST_MILLIS)
		.and_then(seconds)
	{
		values.push((DURATION, duration));
	}
	values.extend(place(track.gps_info()));
	values.sort_by(|(one, _), (other, _)| one.cmp(other));
	Recorded {
		kind: Some(VIDEO),
		values,
	}
}

/// A moment as EDTF: a date and time, with its offset from UTC if it has one, such as
/// `2019-12-24T18:30:00+01`. EXIF writes the offset apart, as `+01:00`.
fn moment(value: &EntryValue, offset: Option<&str>) -> Option<Edtf> {
	let (local, offset) = match value {
		EntryValue::DateTime(aware) => (
			aware.format("%Y-%m-%dT%H:%M:%S").to_string(),
			Some(aware.offset().local_minus_utc() / 60),
		),
		EntryValue::NaiveDateTime(naive) => (
			naive.format("%Y-%m-%dT%H:%M:%S").to_string(),
			offset.and_then(offset_minutes),
		),
		_ => return None,
	};
	let zone = match offset {
		None => String::new(),
		Some(0) => "Z".to_owned(),
		Some(minutes) => {
			let sign = if minutes < 0 { '-' } else { '+' };
			let (hours, minutes) = (minutes.abs() / 60, minutes.abs() % 60);
			if minutes == 0 {
				format!("{sign}{hours:02}")
			} else {
				format!("{sign}{hours:02}:{minutes:02}")
			}
		}
	};
	format!("{local}{zone}").parse().ok()
}

/// The minutes of an offset from UTC as EXIF writes it, such as `+01:00` or `-04:30`.
fn offset_minutes(text: &str) -> Option<i32> {
	let text = text.trim_end_matches('\0').trim();
	let (sign, rest) = match text.as_bytes().first()? {
		b'+' => (1, &text[1..]),
		b'-' => (-1, &text[1..]),
		_ => return None,
	};
	let (hours, minutes) = rest.split_once(':')?;
	let (hours, minutes): (i32, i32) = (hours.parse().ok()?, minutes.parse().ok()?);
	(hours <= 14 && minutes < 60).then_some(sign * (hours * 60 + minutes))
}

/// Text that a camera wrote, without the padding that EXIF often has.
fn text(value: &EntryValue) -> Option<Value> {
	let text = value
		.as_str()?
		.trim_matches(|c: char| c == '\0' || c.is_whitespace());
	(!text.is_empty()).then(|| Value::Text(text.to_owned()))
}

/// A number of pixels, as EXIF writes it, in one of several sizes of integer.
fn pixels(value: &EntryValue) -> Option<Value> {
	let pixels = value
		.as_u32()
		.or_else(|| value.as_u16().map(u32::from))
		.filter(|&pixels| pixels > 0)?;
	Some(Value::Integer(i64::from(pixels)))
}

/// A duration in milliseconds, as a quantity of seconds.
fn seconds(millis: u64) -> Option<Value> {
	let number: Decimal = format!("{}.{:03}", millis / 1_000, millis % 1_000)
		.parse()
		.ok()?;
	Quantity::new(number, "s").map(Value::Quantity)
}

/// Where a photo or a video was taken, in degrees, to six decimal places: about ten
/// centimetres, finer than any phone's position.
fn place(gps: Option<&GPSInfo>) -> Vec<(FieldName, Value)> {
	let Some(gps) = gps else {
		return Vec::new();
	};
	let degrees = |degrees: f64| -> Option<Value> {
		if !degrees.is_finite() {
			return None;
		}
		format!("{degrees:.6}").parse().ok().map(Value::Decimal)
	};
	match (
		gps.latitude_decimal().and_then(degrees),
		gps.longitude_decimal().and_then(degrees),
	) {
		// A position of zero is a camera that had none.
		(Some(latitude), Some(longitude))
			if gps.latitude_decimal() != Some(0.0) || gps.longitude_decimal() != Some(0.0) =>
		{
			vec![(LATITUDE, latitude), (LONGITUDE, longitude)]
		}
		_ => Vec::new(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn offsets_are_written_as_edtf_writes_them() {
		assert_eq!(offset_minutes("+01:00"), Some(60));
		assert_eq!(offset_minutes("-04:30"), Some(-270));
		assert_eq!(offset_minutes("+00:00\0"), Some(0));
		assert_eq!(offset_minutes("01:00"), None);
		assert_eq!(offset_minutes("+25:00"), None);
	}

	#[test]
	fn durations_are_seconds() {
		assert_eq!(
			seconds(12_345),
			Some(Value::Quantity(
				Quantity::new("12.345".parse().expect("a decimal"), "s").expect("a quantity")
			))
		);
	}
}
