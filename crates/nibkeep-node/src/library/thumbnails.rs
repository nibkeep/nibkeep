//! Thumbnails: small pictures of photos and videos that every replica keeps, so that a
//! device shows a picture without its original ([technical design: thumbnails]). The device
//! that adds a file makes its thumbnail, if it reads the file's format: the program brings
//! what reads pictures, a [`Thumbnailer`], and the engine records what it makes, as an entity
//! of the type `std.thumbnail` with a sealed blob of its own. A thumbnail belongs to a
//! content, not to a file.
//!
//! [technical design: thumbnails]: ../../../../docs/Technical-design.md#98-thumbnails

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, State, Value, fields};
use nibkeep_std::{
	CONTENT, FILE, HEIGHT, IN_TREE, MADE_WITH, MEDIA_TYPE, PICTURE_HASH, TAKEN, THUMBNAIL,
	THUMBNAIL_OF, WIDTH,
};
use nibkeep_time::{Day, Start};

use super::Library;
use super::checkouts::{put_in_place, remove_leftover, temporary_for};
use super::entities::NewValue;
use crate::NodeError;

/// The most bytes of a picture read to make its thumbnail: larger pictures get none.
pub const MOST_PICTURE_BYTES: u64 = 256 * 1024 * 1024;

/// The most bytes of pictures being made into thumbnails at once, but one picture. Decoding
/// takes about ten times a picture's bytes, so this keeps large pictures on many cores
/// within about a gigabyte: a dozen photos of 12 megapixels at once, or three of 50.
const MOST_BYTES_IN_FLIGHT: u64 = 96 * 1024 * 1024;

/// How long the thumbnailer may take over a picture before it is left to finish alone, and
/// the pictures it has not made count as unreadable: a decoder that a crafted picture sends
/// round for ever does not hold up the program.
const STUCK: std::time::Duration = std::time::Duration::from_secs(60);

/// What reads pictures and makes their thumbnails, which a program brings: `nib` and
/// apps on a computer bring decoders in memory-safe Rust. An app on Android brings none:
/// it makes thumbnails with the phone's own decoders and hands them over, through
/// [`Library::record_thumbnails_made`]. It runs on several threads at once.
pub trait Thumbnailer: Send + Sync {
	/// How it makes thumbnails, as each records it, such as `nibkeep 1`: who makes them, and
	/// the version of their way, which counts up when the way changes.
	fn made_with(&self) -> &str;

	/// Whether it reads pictures of this media type, as their first bytes tell it, such as
	/// `image/jpeg`.
	fn reads(&self, media_type: &str) -> bool;

	/// The thumbnail of a picture, from its bytes, or `None` if it cannot read them: a JPEG
	/// whose longer side is at most 512 pixels, upright, without metadata.
	fn make(&self, picture: &[u8]) -> Option<Thumbnail>;
}

/// A thumbnail that a [`Thumbnailer`] made: a JPEG, its size in pixels, and the picture
/// hash of what it shows, if the thumbnailer makes one.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
	/// The JPEG's bytes.
	pub jpeg: Vec<u8>,
	/// Its width.
	pub width: u32,
	/// Its height.
	pub height: u32,
	/// The picture hash of what it shows, which copies of the picture share, resized or
	/// saved again, but for at most [`nibkeep_std::MOST_PICTURE_BITS_APART`] bits, as a
	/// difference hash of 64 bits does: by which the engine links copies of photos.
	pub picture_hash: Option<u64>,
}

impl std::fmt::Debug for Thumbnail {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("Thumbnail")
			.field("jpeg", &format_args!("{} bytes", self.jpeg.len()))
			.field("width", &self.width)
			.field("height", &self.height)
			.field(
				"picture_hash",
				&self.picture_hash.map(|hash| format!("{hash:016x}")),
			)
			.finish()
	}
}

/// What making thumbnails did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailsMade {
	/// How many thumbnails it made and recorded.
	pub made: usize,
	/// How many pictures the thumbnailer could not read, as of a format it does not read
	/// after all, or damaged.
	pub unreadable: usize,
	/// How many pictures this device does not hold, neither in the store nor where they were
	/// found, unchanged, or that are larger than [`MOST_PICTURE_BYTES`].
	pub not_here: usize,
	/// How many files it linked as copies of others, by their thumbnails' picture hashes.
	pub copies_linked: usize,
	/// Whether an error of the library stopped linking copies, after the thumbnails were
	/// recorded.
	pub linking_failed: bool,
}

impl std::ops::AddAssign for ThumbnailsMade {
	fn add_assign(&mut self, other: Self) {
		self.made += other.made;
		self.unreadable += other.unreadable;
		self.not_here += other.not_here;
		self.copies_linked += other.copies_linked;
		self.linking_failed |= other.linking_failed;
	}
}

/// A thumbnail of a content, as a device shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailOf {
	/// The thumbnail's entity.
	pub entity: EntityId,
	/// Its own content: the JPEG.
	pub content: Hash,
	/// Its width and height in pixels, as it records them.
	pub size: Option<(u32, u32)>,
	/// How it was made, such as `nibkeep 1`.
	pub made_with: Option<String>,
	/// Whether this replica holds its JPEG.
	pub held: bool,
}

/// How many of a space's files are pictures or videos, as their media types say, and how
/// many of those have a thumbnail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailCounts {
	/// The files whose current content is an image or a video, but those of trees.
	pub pictures: usize,
	/// How many of them have a thumbnail.
	pub with: usize,
}

/// The pictures handed to the workers: each content, and its bytes.
type Jobs = mpsc::SyncSender<(Hash, Vec<u8>)>;

/// What the workers made: each content, the size of its picture, and its thumbnail, if
/// the thumbnailer read it.
type Results = mpsc::Receiver<(Hash, u64, Option<Thumbnail>)>;

/// A file whose current content is a picture or a video.
struct Picture {
	content: Hash,
	media_type: String,
	/// When it was taken, else when its content was recorded, to the second, for the newest
	/// to come first.
	newest: (Day, u32),
}

impl Library {
	/// Brings what reads pictures, so that [`Library::make_thumbnails`] makes thumbnails.
	/// Without one, as in a program that adds no files, none are made.
	pub fn make_thumbnails_with(&mut self, thumbnailer: Arc<dyn Thumbnailer>) {
		self.thumbnailer = Some(thumbnailer);
	}

	/// Makes the thumbnails of these contents of files of a space, those that lack one, as
	/// the device that adds a file does: each that is the current content of a file that is
	/// not a tree's, of a media type that the thumbnailer reads, that this device holds, in
	/// the store or where it was found, unchanged, and no larger than [`MOST_PICTURE_BYTES`].
	/// Pictures are read here and made into thumbnails on the computer's other cores, and
	/// each thumbnail is recorded as it comes: sealed as a blob, in a change of its own. A
	/// picture that cannot be read gets none, and is counted, as are those still in hand when
	/// the thumbnailer has made nothing for a minute, which it is left to finish alone. Nothing is made without a
	/// thumbnailer, by an AI agent's replica, which seals nothing, or where files' content is
	/// secret.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read, or a
	/// thumbnail cannot be saved; those recorded before stay.
	pub fn make_thumbnails(
		&mut self,
		space: SpaceId,
		contents: &[Hash],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<ThumbnailsMade, NodeError> {
		let Some(thumbnailer) = self.thumbnailer.clone() else {
			return Ok(ThumbnailsMade::default());
		};
		if self.is_ai_agent()? || self.space(space)?.stored.secret_fields().contains(&CONTENT) {
			return Ok(ThumbnailsMade::default());
		}
		let mut wanted = Vec::new();
		let mut seen = BTreeSet::new();
		for content in contents {
			if seen.insert(*content)
				&& !self.has_thumbnail(space, content)?
				&& self
					.picture_type(space, content)?
					.is_some_and(|media_type| thumbnailer.reads(&media_type))
			{
				wanted.push(*content);
			}
		}
		if wanted.is_empty() {
			return Ok(ThumbnailsMade::default());
		}
		self.install_standard(space, &[THUMBNAIL], wall_millis, rng)?;
		let workers = std::thread::available_parallelism()
			.map_or(1, |cores| cores.get().saturating_sub(1))
			.clamp(1, wanted.len());
		// Workers of their own, never waited for: one that a picture holds for ever is left
		// behind, and the others end once no more pictures come.
		let (to_workers, jobs): (Jobs, _) = mpsc::sync_channel(workers);
		let jobs = Arc::new(Mutex::new(jobs));
		let (to_main, results): (_, Results) = mpsc::channel();
		for _ in 0..workers {
			let jobs = Arc::clone(&jobs);
			let to_main = to_main.clone();
			let thumbnailer = Arc::clone(&thumbnailer);
			std::thread::spawn(move || {
				loop {
					// A worker that another's panic left without its lock stops.
					let Ok(job) = jobs.lock().map(|jobs| jobs.recv()) else {
						return;
					};
					let Ok((content, picture)) = job else {
						return;
					};
					// A decoder that fails on a picture made to break it makes nothing.
					let made = catch_unwind(AssertUnwindSafe(|| thumbnailer.make(&picture)))
						.ok()
						.flatten();
					if to_main.send((content, picture.len() as u64, made)).is_err() {
						return;
					}
				}
			});
		}
		drop(to_main);
		let mut made = self.record_as_made(
			space,
			(&wanted, &*thumbnailer),
			(&to_workers, &results, workers),
			wall_millis,
			rng,
		)?;
		// With their pictures' hashes, files are linked as copies of others; an error of
		// that leaves the thumbnails made.
		match self.link_copies_of(space, &wanted, wall_millis, rng) {
			Ok(linked) => made.copies_linked = linked,
			Err(_) => made.linking_failed = true,
		}
		Ok(made)
	}

	/// Records thumbnails that the program made itself, each of a content of a space, as a
	/// phone's app does, which reads the files it adds with the phone's own decoders: each
	/// of a content that lacks one and is the current content of a picture or a video that
	/// is not a tree's, recorded as made with `made_with`, such as `android 1`. The others
	/// are left out. Nothing is recorded by an AI agent's replica, or where files' content
	/// is secret.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read, or a
	/// thumbnail cannot be saved; those recorded before stay.
	pub fn record_thumbnails_made(
		&mut self,
		space: SpaceId,
		thumbnails: &[(Hash, Thumbnail)],
		made_with: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<ThumbnailsMade, NodeError> {
		let mut made = ThumbnailsMade::default();
		if self.is_ai_agent()? || self.space(space)?.stored.secret_fields().contains(&CONTENT) {
			return Ok(made);
		}
		let mut recorded = Vec::new();
		for (content, thumbnail) in thumbnails {
			if self.has_thumbnail(space, content)? || self.picture_type(space, content)?.is_none() {
				continue;
			}
			if recorded.is_empty() {
				self.install_standard(space, &[THUMBNAIL], wall_millis, rng)?;
			}
			made +=
				self.record_thumbnail(space, *content, (thumbnail, made_with), wall_millis, rng)?;
			recorded.push(*content);
		}
		match self.link_copies_of(space, &recorded, wall_millis, rng) {
			Ok(linked) => made.copies_linked = linked,
			Err(_) => made.linking_failed = true,
		}
		Ok(made)
	}

	/// Reads the pictures of `wanted` and hands them to the workers, keeping at most twice
	/// as many in flight as there are workers, and no more bytes than
	/// [`MOST_BYTES_IN_FLIGHT`] but one picture; and records each thumbnail as it comes.
	fn record_as_made(
		&mut self,
		space: SpaceId,
		(wanted, thumbnailer): (&[Hash], &dyn Thumbnailer),
		(to_workers, results, workers): (&Jobs, &Results, usize),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<ThumbnailsMade, NodeError> {
		let mut made = ThumbnailsMade::default();
		let mut next = wanted.iter();
		let (mut in_flight, mut bytes) = (0_usize, 0_u64);
		loop {
			while in_flight < 2 * workers && (in_flight == 0 || bytes < MOST_BYTES_IN_FLIGHT) {
				let Some(content) = next.next() else {
					break;
				};
				match self.file_content(space, content, MOST_PICTURE_BYTES)? {
					Some(picture) => {
						bytes += picture.len() as u64;
						// Workers stop only once a decoder broke them all: the rest stay
						// unmade.
						if to_workers.send((*content, picture)).is_err() {
							made.unreadable += 1;
							continue;
						}
						in_flight += 1;
					}
					None => made.not_here += 1,
				}
			}
			if in_flight == 0 {
				return Ok(made);
			}
			let Ok((content, size, thumbnail)) = results.recv_timeout(STUCK) else {
				made.unreadable += in_flight;
				return Ok(made);
			};
			in_flight -= 1;
			bytes = bytes.saturating_sub(size);
			match thumbnail {
				Some(thumbnail) => {
					let one = self.record_thumbnail(
						space,
						content,
						(&thumbnail, thumbnailer.made_with()),
						wall_millis,
						rng,
					)?;
					made += one;
				}
				None => made.unreadable += 1,
			}
		}
	}

	/// Records a thumbnail of a content of a space: its JPEG sealed as a blob, and an
	/// entity of the type `std.thumbnail`, in a change of its own. A JPEG that is no
	/// picture of a sensible size is not recorded, and counts as unreadable.
	fn record_thumbnail(
		&mut self,
		space: SpaceId,
		of: Hash,
		(thumbnail, made_with): (&Thumbnail, &str),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<ThumbnailsMade, NodeError> {
		let sensible = |side: u32| (1..=4_096).contains(&side);
		if thumbnail.jpeg.is_empty() || !sensible(thumbnail.width) || !sensible(thumbnail.height) {
			return Ok(ThumbnailsMade {
				unreadable: 1,
				..ThumbnailsMade::default()
			});
		}
		let content = self.import_blob(space, thumbnail.jpeg.as_slice(), wall_millis, rng)?;
		let mut values = vec![
			NewValue::new(THUMBNAIL_OF, Value::Blob(of)),
			NewValue::new(CONTENT, Value::Blob(content)),
			NewValue::new(MEDIA_TYPE, Value::Text("image/jpeg".to_owned())),
			NewValue::new(WIDTH, Value::Integer(i64::from(thumbnail.width))),
			NewValue::new(HEIGHT, Value::Integer(i64::from(thumbnail.height))),
			NewValue::new(MADE_WITH, Value::Text(made_with.to_owned())),
		];
		if let Some(hash) = thumbnail.picture_hash {
			values.push(NewValue::new(
				PICTURE_HASH,
				Value::Integer(i64::from_be_bytes(hash.to_be_bytes())),
			));
		}
		let (_, operations) = self.new_entity(space, &[THUMBNAIL], values, rng)?;
		self.commit(space, operations, wall_millis, rng)?;
		Ok(ThumbnailsMade {
			made: 1,
			..ThumbnailsMade::default()
		})
	}

	/// Makes the thumbnails that the files of a space lack, whichever device added them, as
	/// `nib file thumbnails` does: for files added before thumbnails were made, or of a
	/// format that a newer thumbnailer reads. The newest go first, by when a photo was
	/// taken, else when its content was recorded, in rounds of `round`, after each of which
	/// `progress` hears what was made so far, of how many.
	///
	/// # Errors
	///
	/// As [`Library::make_thumbnails`].
	pub fn make_missing_thumbnails(
		&mut self,
		space: SpaceId,
		round: usize,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
		progress: &mut dyn FnMut(ThumbnailsMade, usize),
	) -> Result<ThumbnailsMade, NodeError> {
		let missing = self.missing_thumbnails(space)?;
		let mut made = ThumbnailsMade::default();
		for contents in missing.chunks(round.max(1)) {
			made += self.make_thumbnails(space, contents, wall_millis, rng)?;
			progress(made, missing.len());
		}
		Ok(made)
	}

	/// The contents of the files of a space that lack a thumbnail, and of a media type that
	/// the thumbnailer reads, newest first, as [`Library::make_missing_thumbnails`] makes
	/// them; none without a thumbnailer.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn missing_thumbnails(&self, space: SpaceId) -> Result<Vec<Hash>, NodeError> {
		let Some(thumbnailer) = &self.thumbnailer else {
			return Ok(Vec::new());
		};
		let with = self.contents_with_thumbnails(space)?;
		let mut missing: Vec<(Reverse<(Day, u32)>, Hash)> = self
			.pictures(space)?
			.into_values()
			.filter(|picture| {
				!with.contains(&picture.content) && thumbnailer.reads(&picture.media_type)
			})
			.map(|picture| (Reverse(picture.newest), picture.content))
			.collect();
		missing.sort();
		Ok(missing.into_iter().map(|(_, content)| content).collect())
	}

	/// The thumbnail that a file of a space shows, of its current content: of those made of
	/// it, the one made the newest way, the first recorded among those, as every device
	/// chooses alike; `None` if none was made, or the file has no content.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn thumbnail(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<Option<ThumbnailOf>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let file = state.canonical(file)?;
		let Some(content) = blob_of(state, file, &CONTENT)? else {
			return Ok(None);
		};
		Ok(self.thumbnails_of(space, &content)?.into_iter().next())
	}

	/// The thumbnails made of a content, in the order that every device chooses the one shown
	/// by: the one made the newest way first, the first recorded among those, then by ID.
	pub(super) fn thumbnails_of(
		&self,
		space: SpaceId,
		content: &Hash,
	) -> Result<Vec<ThumbnailOf>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut made = Vec::new();
		for thumbnail in open.stored.find(&THUMBNAIL_OF, &Value::Blob(*content))? {
			let thumbnail = state.canonical(thumbnail)?;
			if gone(state, thumbnail)? || !state.types(thumbnail)?.contains(&THUMBNAIL) {
				continue;
			}
			let Some(jpeg) = state
				.current(thumbnail, &CONTENT)?
				.into_iter()
				.find_map(|ours| match ours.value {
					Value::Blob(jpeg) => Some((jpeg, ours.id)),
					_ => None,
				})
			else {
				continue;
			};
			let recorded = state
				.storage()
				.get(jpeg.1)?
				.map(|recorded| recorded.at.millis());
			let made_with = state
				.current(thumbnail, &MADE_WITH)?
				.first()
				.and_then(|statement| statement.value.as_text().map(str::to_owned));
			let pixels = |field: &FieldName| -> Result<Option<u32>, NodeError> {
				Ok(state
					.current(thumbnail, field)?
					.first()
					.and_then(|statement| match statement.value {
						Value::Integer(pixels) => u32::try_from(pixels).ok(),
						_ => None,
					}))
			};
			let size = pixels(&WIDTH)?.zip(pixels(&HEIGHT)?);
			let way = made_with.as_deref().map_or(0, way_of);
			made.push((
				(Reverse(way), recorded, thumbnail),
				ThumbnailOf {
					entity: thumbnail,
					content: jpeg.0,
					size,
					made_with,
					held: open.stored.holds_ordinary_blob(&jpeg.0),
				},
			));
		}
		made.sort_by_key(|(key, _)| *key);
		made.dedup_by_key(|(key, _)| key.2);
		Ok(made.into_iter().map(|(_, thumbnail)| thumbnail).collect())
	}

	/// Purges, in each space where this replica may, the thumbnails that it made of contents
	/// that have another thumbnail shown, as two devices that add one photo before they sync
	/// both make one ([technical design: thumbnails]): each whole, but its marks, and its own
	/// content unless something else holds that; once the one shown is held here, so that
	/// this device shows a picture all along. The device that made a thumbnail purges it, as
	/// a member may purge what they wrote; one that never syncs again leaves its own. Returns
	/// how many it purged.
	///
	/// [technical design: thumbnails]: ../../../../docs/Technical-design.md#98-thumbnails
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read, or a purge cannot be saved.
	pub fn purge_thumbnails_made_twice(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		if self.is_ai_agent()? {
			return Ok(0);
		}
		let own = self.replica.signing.verifying_key();
		let mut purged = 0;
		for space in self.spaces() {
			if self.check_not_revoked(space).is_err() {
				continue;
			}
			let mut made_twice = Vec::new();
			let mut counted: BTreeMap<Hash, usize> = BTreeMap::new();
			{
				let open = self.space(space)?;
				let state = open.stored.log().state();
				for thumbnail in open
					.stored
					.find(&fields::TYPE, &Value::Text(THUMBNAIL.to_string()))?
				{
					let thumbnail = state.canonical(thumbnail)?;
					if gone(state, thumbnail)? || blob_of(state, thumbnail, &CONTENT)?.is_none() {
						continue;
					}
					if let Some(of) = blob_of(state, thumbnail, &THUMBNAIL_OF)? {
						*counted.entry(of).or_default() += 1;
					}
				}
			}
			for (content, count) in counted {
				if count < 2 {
					continue;
				}
				let made = self.thumbnails_of(space, &content)?;
				let Some((shown, others)) = made.split_first() else {
					continue;
				};
				if !shown.held {
					continue;
				}
				for other in others {
					if self.wrote_only(space, other.entity, &own)? {
						made_twice.push(other.entity);
					}
				}
			}
			if made_twice.is_empty() {
				continue;
			}
			// One purge of them all in the space, which reads its values once.
			let (statements, contents) = self.thumbnails_whole(space, &made_twice)?;
			if statements.is_empty() {
				continue;
			}
			let contents: Vec<Hash> = contents.into_iter().collect();
			self.purge_statements(space, &statements, (&contents, false), wall_millis, rng)?;
			purged += made_twice.len();
		}
		Ok(purged)
	}

	/// Whether every value that an entity of a space has held, current or past, came from
	/// changes of this replica's own key.
	fn wrote_only(
		&self,
		space: SpaceId,
		entity: EntityId,
		own: &nibkeep_crypto::VerifyingKey,
	) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		for (_, ids) in log.state().storage().of_entity(entity)? {
			for id in ids {
				if log.author_of(id)?.is_none_or(|author| author != *own) {
					return Ok(false);
				}
			}
		}
		Ok(true)
	}

	/// The statements of thumbnails that a purge of them whole takes, every value they have
	/// held but their marks of a move or a merge, and their own contents, but those that
	/// another value of the space holds.
	fn thumbnails_whole(
		&self,
		space: SpaceId,
		thumbnails: &[EntityId],
	) -> Result<(Vec<StatementId>, BTreeSet<Hash>), NodeError> {
		let open = self.space(space)?;
		let secret_fields = open.stored.secret_fields();
		let storage = open.stored.log().state().storage();
		let mut statements = Vec::new();
		let mut pictures = BTreeSet::new();
		for thumbnail in thumbnails {
			for (field, ids) in storage.of_entity(*thumbnail)? {
				if super::behind::is_mark(&field) {
					continue;
				}
				for id in ids {
					statements.push(id);
					if field == CONTENT
						&& let Some(recorded) = storage.get(id)?
						&& let Value::Blob(picture) = recorded.statement.value
					{
						pictures.insert(picture);
					}
				}
			}
		}
		if !pictures.is_empty() {
			let theirs: BTreeSet<StatementId> = statements.iter().copied().collect();
			for entity in storage.described()? {
				for (field, ids) in storage.of_entity(entity)? {
					if secret_fields.contains(&field) {
						continue;
					}
					for id in ids {
						if !theirs.contains(&id)
							&& let Some(recorded) = storage.get(id)?
							&& let Value::Blob(held) = &recorded.statement.value
						{
							pictures.remove(held);
						}
					}
				}
			}
		}
		Ok((statements, pictures))
	}

	/// The JPEG of a thumbnail, from the copy this replica holds, or `None` if it holds none.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn thumbnail_jpeg(
		&self,
		space: SpaceId,
		thumbnail: &ThumbnailOf,
	) -> Result<Option<Vec<u8>>, NodeError> {
		let open = self.space(space)?;
		if !open.stored.holds_ordinary_blob(&thumbnail.content) {
			return Ok(None);
		}
		self.file_content(space, &thumbnail.content, MOST_PICTURE_BYTES)
	}

	/// Writes the JPEG of a thumbnail into a new file at `path`, for scripts and tests. It
	/// replaces nothing: the JPEG is written beside `path` first, synced, and then put in
	/// place, so that a write that fails leaves nothing at `path`.
	///
	/// # Errors
	///
	/// Returns [`NodeError::PlaceTaken`] if something is at `path`, [`NodeError::NotHere`] if
	/// this replica does not hold the JPEG, and [`NodeError::File`] if writing fails.
	pub fn save_thumbnail(
		&self,
		space: SpaceId,
		thumbnail: &ThumbnailOf,
		path: &Path,
	) -> Result<(), NodeError> {
		if path.symlink_metadata().is_ok() {
			return Err(NodeError::PlaceTaken(path.to_owned()));
		}
		let jpeg = self
			.thumbnail_jpeg(space, thumbnail)?
			.ok_or_else(|| NodeError::NotHere(thumbnail.content.to_string()))?;
		let temporary = temporary_for(path, "thumbnail");
		remove_leftover(&temporary);
		let written = File::create_new(&temporary).and_then(|mut file| {
			file.write_all(&jpeg)?;
			file.sync_all()
		});
		if let Err(error) = written {
			remove_leftover(&temporary);
			return Err(NodeError::file(&temporary)(error));
		}
		if let Err(error) = put_in_place(&temporary, path) {
			remove_leftover(&temporary);
			if error.kind() == std::io::ErrorKind::AlreadyExists {
				return Err(NodeError::PlaceTaken(path.to_owned()));
			}
			return Err(NodeError::file(path)(error));
		}
		Ok(())
	}

	/// How many of a space's files are pictures or videos, and how many of those have a
	/// thumbnail.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn thumbnail_counts(&self, space: SpaceId) -> Result<ThumbnailCounts, NodeError> {
		let with = self.contents_with_thumbnails(space)?;
		let pictures = self.pictures(space)?;
		Ok(ThumbnailCounts {
			pictures: pictures.len(),
			with: pictures
				.keys()
				.filter(|content| with.contains(*content))
				.count(),
		})
	}

	/// Whether a content of a space has a thumbnail, recorded and neither deleted nor moved
	/// away.
	fn has_thumbnail(&self, space: SpaceId, content: &Hash) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		for thumbnail in open.stored.find(&THUMBNAIL_OF, &Value::Blob(*content))? {
			let thumbnail = state.canonical(thumbnail)?;
			if state.types(thumbnail)?.contains(&THUMBNAIL)
				&& !gone(state, thumbnail)?
				&& blob_of(state, thumbnail, &CONTENT)?.is_some()
			{
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// The media type of a content of a space, if it is the current content of a file that is
	/// neither gone nor a tree's, and an image or a video, as the file's media type says.
	fn picture_type(&self, space: SpaceId, content: &Hash) -> Result<Option<String>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		for file in open.stored.find(&CONTENT, &Value::Blob(*content))? {
			let file = state.canonical(file)?;
			if !state.types(file)?.contains(&FILE)
				|| gone(state, file)?
				|| !state.current(file, &IN_TREE)?.is_empty()
			{
				continue;
			}
			if let Some(media_type) = media_type_of(state, file)? {
				return Ok(Some(media_type));
			}
		}
		Ok(None)
	}

	/// The contents of a space that have a thumbnail, recorded and neither deleted nor moved
	/// away.
	fn contents_with_thumbnails(&self, space: SpaceId) -> Result<BTreeSet<Hash>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut with = BTreeSet::new();
		for thumbnail in open
			.stored
			.find(&fields::TYPE, &Value::Text(THUMBNAIL.to_string()))?
		{
			let thumbnail = state.canonical(thumbnail)?;
			if gone(state, thumbnail)? || blob_of(state, thumbnail, &CONTENT)?.is_none() {
				continue;
			}
			with.extend(blob_of(state, thumbnail, &THUMBNAIL_OF)?);
		}
		Ok(with)
	}

	/// The files of a space whose current content is an image or a video, as its media type
	/// says, but those of trees and those deleted or moved away: each content once.
	fn pictures(&self, space: SpaceId) -> Result<BTreeMap<Hash, Picture>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut pictures = BTreeMap::new();
		for file in open
			.stored
			.find(&fields::TYPE, &Value::Text(FILE.to_string()))?
		{
			let file = state.canonical(file)?;
			if gone(state, file)? || !state.current(file, &IN_TREE)?.is_empty() {
				continue;
			}
			let Some((content, statement)) =
				state
					.current(file, &CONTENT)?
					.into_iter()
					.find_map(|statement| match statement.value {
						Value::Blob(content) => Some((content, statement.id)),
						_ => None,
					})
			else {
				continue;
			};
			let Some(media_type) = media_type_of(state, file)? else {
				continue;
			};
			let taken = state
				.current(file, &TAKEN)?
				.first()
				.and_then(|statement| match &statement.value {
					Value::Date(taken) => match taken.start() {
						Start::At { day, second } => Some((day, second.unwrap_or(0))),
						Start::BeginningOfTime => None,
					},
					_ => None,
				});
			let recorded = state.storage().get(statement)?.and_then(|recorded| {
				let millis = recorded.at.millis();
				Day::from_unix_millis(millis, 0).map(|day| (day, Day::second_of_day(millis, 0)))
			});
			let Some(newest) = taken.or(recorded) else {
				continue;
			};
			pictures.entry(content).or_insert(Picture {
				content,
				media_type,
				newest,
			});
		}
		Ok(pictures)
	}
}

/// The version of a way of making thumbnails, as `std.made-with` ends with it, such as 1 of
/// `nibkeep 1`; 0 if it names none.
fn way_of(made_with: &str) -> u64 {
	made_with
		.rsplit(' ')
		.next()
		.and_then(|version| version.parse().ok())
		.unwrap_or(0)
}

/// Whether an entity is gone from its space: deleted, or moved to another.
pub(super) fn gone<S: Statements>(state: &State<S>, entity: EntityId) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state.is_deleted(entity)? || !state.current(entity, &fields::MOVED_INTO)?.is_empty())
}

/// A file's media type, if it is an image's or a video's.
fn media_type_of<S: Statements>(
	state: &State<S>,
	file: EntityId,
) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(file, &MEDIA_TYPE)?
		.first()
		.and_then(|statement| statement.value.as_text().map(str::to_owned))
		.filter(|media_type| media_type.starts_with("image/") || media_type.starts_with("video/")))
}

/// The first blob that an entity's field holds now.
pub(super) fn blob_of<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	field: &FieldName,
) -> Result<Option<Hash>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(entity, field)?
		.into_iter()
		.find_map(|statement| match statement.value {
			Value::Blob(content) => Some(content),
			_ => None,
		}))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ways_are_told_by_their_versions() {
		assert_eq!(way_of("nibkeep 1"), 1);
		assert_eq!(way_of("nibkeep 12"), 12);
		assert_eq!(way_of("nibkeep"), 0);
		assert_eq!(way_of(""), 0);
	}
}
