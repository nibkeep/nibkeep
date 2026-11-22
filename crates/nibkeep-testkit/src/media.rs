//! Photos and videos made for tests: the smallest files that carry what a camera writes,
//! with values that tests choose.

/// What a test photo's EXIF data says.
#[derive(Clone, Debug)]
pub struct Photo<'a> {
	/// The camera's maker.
	pub make: &'a str,
	/// The camera's model.
	pub model: &'a str,
	/// When it was taken, as EXIF writes it, such as `2019:12:24 18:30:00`.
	pub taken: &'a str,
	/// The offset from UTC, as EXIF writes it, such as `+01:00`, if any.
	pub offset: Option<&'a str>,
	/// Its width and height in pixels, as stored.
	pub size: (u32, u32),
	/// How it is to be turned to be shown, as EXIF's orientation says, if it says: 6, for
	/// example, for a quarter turn clockwise.
	pub orientation: Option<u16>,
	/// Where it was taken: degrees, minutes and seconds of latitude north and longitude
	/// east, if anywhere.
	pub place: Option<([u32; 3], [u32; 3])>,
}

/// A value of an EXIF entry.
enum Entry<'a> {
	Ascii(&'a str),
	Short(u16),
	Long(u32),
	Rationals(Vec<(u32, u32)>),
}

impl Photo<'_> {
	/// A JPEG of no picture, whose EXIF data says what the photo does.
	///
	/// # Panics
	///
	/// Panics if the EXIF data takes more than 64 KiB, which a JPEG cannot hold.
	#[must_use]
	pub fn jpeg(&self) -> Vec<u8> {
		let exif_entries = {
			let mut entries = vec![(0x9003, Entry::Ascii(self.taken))];
			if let Some(offset) = self.offset {
				entries.push((0x9011, Entry::Ascii(offset)));
			}
			entries.push((0xa002, Entry::Long(self.size.0)));
			entries.push((0xa003, Entry::Long(self.size.1)));
			entries
		};
		let gps_entries = {
			self.place.map_or_default(|(latitude, longitude)| {
				let rationals =
					|dms: [u32; 3]| Entry::Rationals(dms.iter().map(|&part| (part, 1)).collect());
				vec![
					(1, Entry::Ascii("N")),
					(2, rationals(latitude)),
					(3, Entry::Ascii("E")),
					(4, rationals(longitude)),
				]
			})
		};
		let main_entries = |exif: u32, gps: u32| {
			let mut entries = vec![
				(0x010f, Entry::Ascii(self.make)),
				(0x0110, Entry::Ascii(self.model)),
			];
			if let Some(orientation) = self.orientation {
				entries.push((0x0112, Entry::Short(orientation)));
			}
			entries.push((0x8769, Entry::Long(exif)));
			if self.place.is_some() {
				entries.push((0x8825, Entry::Long(gps)));
			}
			entries
		};
		// Offsets count from the start of the TIFF header, which takes 8 bytes; the sizes of
		// the directories do not depend on the offsets they hold.
		let exif_at = 8 + directory(8, &main_entries(0, 0)).len();
		let gps_at = exif_at + directory(exif_at, &exif_entries).len();
		let mut tiff = b"II*\0\x08\0\0\0".to_vec();
		tiff.extend(directory(8, &main_entries(offset(exif_at), offset(gps_at))));
		tiff.extend(directory(exif_at, &exif_entries));
		if self.place.is_some() {
			tiff.extend(directory(gps_at, &gps_entries));
		}
		let mut app1 = b"Exif\0\0".to_vec();
		app1.extend(tiff);
		let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
		jpeg.extend(u16::try_from(app1.len() + 2).expect("small").to_be_bytes());
		jpeg.extend(app1);
		jpeg.extend([0xff, 0xd9]);
		jpeg
	}

	/// A JPEG of a picture, whose EXIF data says what the photo does: `picture`, a JPEG
	/// that says nothing of itself, with the EXIF segment of [`Photo::jpeg`] right after its
	/// start.
	///
	/// # Panics
	///
	/// Panics if `picture` is no JPEG, or as [`Photo::jpeg`] does.
	#[must_use]
	pub fn jpeg_of(&self, picture: &[u8]) -> Vec<u8> {
		assert_eq!(picture.get(..2), Some([0xff, 0xd8].as_slice()), "a JPEG");
		let said = self.jpeg();
		// The EXIF segment: its marker, its length, and what the length counts.
		let length = usize::from(u16::from_be_bytes([said[4], said[5]]));
		let mut jpeg = picture[..2].to_vec();
		jpeg.extend_from_slice(&said[2..4 + length]);
		jpeg.extend_from_slice(&picture[2..]);
		jpeg
	}
}

/// An offset in a test file, which is always small.
fn offset(at: usize) -> u32 {
	u32::try_from(at).expect("small")
}

/// A TIFF directory that starts at `start`, little-endian, with its entries' values that
/// do not fit in four bytes after it, and no next directory.
fn directory(start: usize, entries: &[(u16, Entry<'_>)]) -> Vec<u8> {
	let mut data_at = start + 2 + entries.len() * 12 + 4;
	let mut head = u16::try_from(entries.len())
		.expect("few")
		.to_le_bytes()
		.to_vec();
	let mut data = Vec::new();
	for (tag, entry) in entries {
		let (kind, count, bytes): (u16, usize, Vec<u8>) = match entry {
			Entry::Ascii(text) => {
				let mut bytes = text.as_bytes().to_vec();
				bytes.push(0);
				(2, bytes.len(), bytes)
			}
			Entry::Short(value) => (3, 1, value.to_le_bytes().to_vec()),
			Entry::Long(value) => (4, 1, value.to_le_bytes().to_vec()),
			Entry::Rationals(values) => (
				5,
				values.len(),
				values
					.iter()
					.flat_map(|(top, bottom)| [top.to_le_bytes(), bottom.to_le_bytes()])
					.flatten()
					.collect(),
			),
		};
		head.extend(tag.to_le_bytes());
		head.extend(kind.to_le_bytes());
		head.extend(offset(count).to_le_bytes());
		if bytes.len() <= 4 {
			let mut inline = bytes;
			inline.resize(4, 0);
			head.extend(inline);
		} else {
			head.extend(offset(data_at).to_le_bytes());
			data_at += bytes.len();
			data.extend(bytes);
		}
	}
	head.extend([0; 4]);
	head.extend(data);
	head
}

/// What a test video's tracks say.
#[derive(Clone, Debug)]
pub struct Video {
	/// When it was made, in seconds since 1970, in UTC, or `None` for a header that does not
	/// say, as some programs write it: as zero.
	pub created: Option<u64>,
	/// How long it lasts, in milliseconds.
	pub duration_millis: u32,
	/// Its width and height in pixels.
	pub size: (u16, u16),
}

impl Video {
	/// An MP4 of no picture, whose boxes say what the video does.
	///
	/// # Panics
	///
	/// Panics if the video was made after 2040, which these headers cannot hold.
	#[must_use]
	pub fn mp4(&self) -> Vec<u8> {
		// QuickTime counts seconds from 1904.
		let created = self.created.map_or(0, |created| {
			u32::try_from(created + 2_082_844_800).expect("before 2040")
		});
		let mut mvhd = vec![0; 4];
		mvhd.extend(created.to_be_bytes());
		mvhd.extend(created.to_be_bytes());
		mvhd.extend(1_000_u32.to_be_bytes());
		mvhd.extend(self.duration_millis.to_be_bytes());
		mvhd.extend(0x0001_0000_u32.to_be_bytes());
		mvhd.extend(0x0100_u16.to_be_bytes());
		mvhd.extend([0; 10]);
		mvhd.extend(matrix());
		mvhd.extend([0; 24]);
		mvhd.extend(2_u32.to_be_bytes());
		let mut tkhd = vec![0, 0, 0, 3];
		tkhd.extend(created.to_be_bytes());
		tkhd.extend(created.to_be_bytes());
		tkhd.extend(1_u32.to_be_bytes());
		tkhd.extend([0; 4]);
		tkhd.extend(self.duration_millis.to_be_bytes());
		tkhd.extend([0; 8]);
		tkhd.extend([0; 4]);
		tkhd.extend([0; 4]);
		tkhd.extend(matrix());
		tkhd.extend((u32::from(self.size.0) << 16).to_be_bytes());
		tkhd.extend((u32::from(self.size.1) << 16).to_be_bytes());
		let mut hdlr = vec![0; 8];
		hdlr.extend(b"vide");
		hdlr.extend([0; 12]);
		hdlr.extend(b"Video\0");
		let mdia = boxed(*b"mdia", &boxed(*b"hdlr", &hdlr));
		let trak = boxed(*b"trak", &[boxed(*b"tkhd", &tkhd), mdia].concat());
		let moov = boxed(*b"moov", &[boxed(*b"mvhd", &mvhd), trak].concat());
		let mut ftyp = b"isom".to_vec();
		ftyp.extend(0x200_u32.to_be_bytes());
		ftyp.extend(b"isomiso2mp41");
		[boxed(*b"ftyp", &ftyp), moov, boxed(*b"mdat", &[])].concat()
	}
}

/// The identity matrix of the headers of `QuickTime` and MP4.
fn matrix() -> Vec<u8> {
	[0x0001_0000_u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
		.iter()
		.flat_map(|value| value.to_be_bytes())
		.collect()
}

/// A box of an MP4 file: its size, its kind and its contents.
fn boxed(kind: [u8; 4], contents: &[u8]) -> Vec<u8> {
	let mut bytes = offset(contents.len() + 8).to_be_bytes().to_vec();
	bytes.extend(kind);
	bytes.extend(contents);
	bytes
}
