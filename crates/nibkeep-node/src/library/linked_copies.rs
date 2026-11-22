//! Copies and edits of photos, linked, never merged ([technical design: importers]): a photo
//! resized or saved again, as Google Photos keeps one, is linked to the larger by
//! `std.copy-of`, and an edit to its original by `std.edit-of`. Each stays a file of its own,
//! with its own origins and content, and a person who wants one file merges them.
//!
//! A copy is told by its thumbnail: taken in the same second, of the same proportions, and
//! with picture hashes at most [`MOST_PICTURE_BITS_APART`] bits apart. Files found in one
//! folder are never copies of each other, as photos of a burst taken within a second are
//! alike; the smaller, in pixels and else in bytes, is the copy of the larger.
//!
//! [technical design: importers]: ../../../../docs/Technical-design.md#912-importers-and-where-files-came-from

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements as _;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_std::{
	CONTENT, COPY_OF, EDIT_OF, FILE, FOUND_ON, FROM_SOURCE, HEIGHT, IN_TREE,
	MOST_PICTURE_BITS_APART, ORIGIN, ORIGIN_OF, PATH, PICTURE_HASH, SIZE, SOURCE_PATH, TAKEN,
	WIDTH,
};
use nibkeep_time::Edtf;

use super::Library;
use super::thumbnails::{blob_of, gone};
use crate::NodeError;

/// The offsets from UTC, in minutes, that places use or used since cameras were digital,
/// from −12 to +14 hours: a copy's time may be written in any of them, or in UTC, where its
/// original's has another or none. Two times are the same second in these alone.
const OFFSETS: [i32; 43] = [
	-720, -660, -600, -570, -540, -480, -420, -360, -300, -270, -240, -210, -180, -150, -120, -60,
	0, 60, 120, 180, 210, 240, 270, 300, 330, 345, 360, 390, 420, 480, 510, 525, 540, 570, 600,
	630, 660, 690, 720, 765, 780, 825, 840,
];

/// How far apart the proportions of a picture's and its copy's thumbnails may be, as a
/// share: thumbnails of small pictures round their sides.
const PROPORTIONS_APART: u64 = 50;

/// A folder that a file was found in: in a source, by the folder of its path there, or on a
/// device.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Folder {
	InSource(EntityId, String),
	OnDevice(EntityId, String),
}

/// What tells whether a file is a copy of another: when it was taken, its thumbnail's
/// proportions and picture hash, its size, and the folders it was found in.
#[derive(Clone, Debug)]
struct Likeness {
	file: EntityId,
	content: Hash,
	/// When it was taken, as [`Edtf::local_seconds`] gives it.
	taken: (i64, Option<i32>),
	/// Its thumbnail's width and height.
	proportions: (u32, u32),
	picture_hash: u64,
	/// Its pixels, as its width and height say, if it has both.
	pixels: Option<u64>,
	bytes: Option<u64>,
	folders: BTreeSet<Folder>,
}

impl Likeness {
	/// Whether `other` looks like a copy of this file, or this of it.
	fn alike(&self, other: &Self) -> bool {
		self.file != other.file
			&& self.content != other.content
			&& same_second(self.taken, other.taken)
			&& same_proportions(self.proportions, other.proportions)
			&& (self.picture_hash ^ other.picture_hash).count_ones() <= MOST_PICTURE_BITS_APART
			&& self.folders.is_disjoint(&other.folders)
	}

	/// Whether this file is larger than `other`: by pixels, a file that says none being
	/// smaller than one that says, then by bytes, then by ID, as some order must decide. One
	/// order, so that copies never link round to themselves.
	fn larger_than(&self, other: &Self) -> bool {
		self.size() > other.size()
	}

	/// What [`Likeness::larger_than`] compares.
	fn size(&self) -> (u64, u64, std::cmp::Reverse<EntityId>) {
		(
			self.pixels.unwrap_or(0),
			self.bytes.unwrap_or(0),
			std::cmp::Reverse(self.file),
		)
	}
}

impl Library {
	/// Links the files of `contents` that look like copies of others, or others like copies
	/// of them, each to the larger it looks most like, as [`Likeness`] tells, unless it is or
	/// was a copy of one already, or an edit of it. Returns how many it linked.
	pub(super) fn link_copies_of(
		&mut self,
		space: SpaceId,
		contents: &[Hash],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let mut seen = BTreeSet::new();
		let mut linked = 0;
		for content in contents {
			for file in self.files_holding(space, content)? {
				let Some(this) = self.likeness(space, file)? else {
					continue;
				};
				let alike = self.alike(space, &this)?;
				if seen.insert(this.file)
					&& self.link_to_original(space, (&this, &alike), wall_millis, rng)?
				{
					linked += 1;
				}
				// Each that looks like it, unless linked already, may be a copy of it, or of
				// another that looks more like it.
				for one in alike {
					if !seen.insert(one.file) || self.ever_had(space, one.file, &COPY_OF)? {
						continue;
					}
					let its = self.alike(space, &one)?;
					if self.link_to_original(space, (&one, &its), wall_millis, rng)? {
						linked += 1;
					}
				}
			}
		}
		Ok(linked)
	}

	/// Links an edit of a file, such as a photo that Google Photos keeps cropped beside its
	/// original, to the original, by `std.edit-of`, unless it is or was an edit already, or
	/// has the same content, or either is no file of the space, as one gone or moved away.
	/// Returns whether it linked it.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read or
	/// saved.
	pub fn link_edit(
		&mut self,
		space: SpaceId,
		(edit, original): (EntityId, EntityId),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let (edit, original) = (state.canonical(edit)?, state.canonical(original)?);
		for file in [edit, original] {
			if gone(state, file)? || !state.types(file)?.contains(&FILE) {
				return Ok(false);
			}
		}
		if edit == original
			|| blob_of(state, edit, &CONTENT)? == blob_of(state, original, &CONTENT)?
			|| self.ever_had(space, edit, &EDIT_OF)?
		{
			return Ok(false);
		}
		self.link(space, (edit, &EDIT_OF, original), wall_millis, rng)?;
		Ok(true)
	}

	/// Links `copy` to the larger file it looks most like, by the fewest bits apart, then the
	/// largest, unless it is or was a copy already: a link that a person removed stays
	/// removed.
	fn link_to_original(
		&mut self,
		space: SpaceId,
		(copy, alike): (&Likeness, &[Likeness]),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		if self.ever_had(space, copy.file, &COPY_OF)? {
			return Ok(false);
		}
		let mut best: Option<&Likeness> = None;
		for original in alike {
			if !original.larger_than(copy)
				|| self.edits_of_each_other(space, copy.file, original.file)?
			{
				continue;
			}
			let apart = |one: &Likeness| (one.picture_hash ^ copy.picture_hash).count_ones();
			let better = best.is_none_or(|best| {
				apart(original) < apart(best)
					|| (apart(original) == apart(best) && original.larger_than(best))
			});
			if better {
				best = Some(original);
			}
		}
		let Some(original) = best else {
			return Ok(false);
		};
		self.link(
			space,
			(copy.file, &COPY_OF, original.file),
			wall_millis,
			rng,
		)?;
		Ok(true)
	}

	/// The files that look like copies of `this`, or it like a copy of them, found by when
	/// they were taken, as written in any time zone.
	fn alike(&self, space: SpaceId, this: &Likeness) -> Result<Vec<Likeness>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut files = BTreeSet::new();
		for moment in written_as(this.taken) {
			for file in open.stored.find(&TAKEN, &Value::Date(moment))? {
				files.insert(state.canonical(file)?);
			}
		}
		let mut alike = Vec::new();
		for file in files {
			if let Some(other) = self.likeness(space, file)?
				&& this.alike(&other)
			{
				alike.push(other);
			}
		}
		Ok(alike)
	}

	/// The likeness of a file: none if it is gone, a tree's, or lacks a time taken or a
	/// thumbnail with a picture hash.
	fn likeness(&self, space: SpaceId, file: EntityId) -> Result<Option<Likeness>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let file = state.canonical(file)?;
		if gone(state, file)?
			|| !state.types(file)?.contains(&FILE)
			|| !state.current(file, &IN_TREE)?.is_empty()
		{
			return Ok(None);
		}
		let Some(content) = blob_of(state, file, &CONTENT)? else {
			return Ok(None);
		};
		let first = |entity: EntityId, field: &FieldName| -> Result<Option<Value>, NodeError> {
			Ok(state
				.current(entity, field)?
				.first()
				.map(|statement| statement.value.clone()))
		};
		let integer = |entity: EntityId, field: &FieldName| -> Result<Option<i64>, NodeError> {
			Ok(match first(entity, field)? {
				Some(Value::Integer(integer)) => Some(integer),
				_ => None,
			})
		};
		let Some(taken) = (match first(file, &TAKEN)? {
			Some(Value::Date(taken)) => taken.local_seconds(),
			_ => None,
		}) else {
			return Ok(None);
		};
		let mut shown = None;
		for thumbnail in self.thumbnails_of(space, &content)? {
			if let (Some(size), Some(hash)) =
				(thumbnail.size, integer(thumbnail.entity, &PICTURE_HASH)?)
			{
				shown = Some((size, u64::from_be_bytes(hash.to_be_bytes())));
				break;
			}
		}
		let Some((proportions, picture_hash)) = shown else {
			return Ok(None);
		};
		let positive = |value: Option<i64>| {
			value
				.and_then(|value| u64::try_from(value).ok())
				.filter(|value| *value > 0)
		};
		let pixels = positive(integer(file, &WIDTH)?)
			.zip(positive(integer(file, &HEIGHT)?))
			.map(|(width, height)| width.saturating_mul(height));
		let bytes = positive(integer(file, &SIZE)?);
		let mut folders = BTreeSet::new();
		for origin in open.stored.find(&ORIGIN_OF, &Value::Reference(file))? {
			let origin = state.canonical(origin)?;
			if gone(state, origin)? || !state.types(origin)?.contains(&ORIGIN) {
				continue;
			}
			let reference = |field: &FieldName| -> Result<Option<EntityId>, NodeError> {
				Ok(match first(origin, field)? {
					Some(Value::Reference(entity)) => Some(entity),
					_ => None,
				})
			};
			let text = |field: &FieldName| -> Result<Option<String>, NodeError> {
				Ok(first(origin, field)?.and_then(|value| value.as_text().map(str::to_owned)))
			};
			if let (Some(source), Some(path)) = (reference(&FROM_SOURCE)?, text(&SOURCE_PATH)?) {
				folders.insert(Folder::InSource(source, parent_of(&path).to_owned()));
			}
			if let (Some(device), Some(path)) = (reference(&FOUND_ON)?, text(&PATH)?) {
				folders.insert(Folder::OnDevice(device, parent_of(&path).to_owned()));
			}
		}
		Ok(Some(Likeness {
			file,
			content,
			taken,
			proportions,
			picture_hash,
			pixels,
			bytes,
			folders,
		}))
	}

	/// The files of a space whose current content is `content`, but those gone.
	fn files_holding(&self, space: SpaceId, content: &Hash) -> Result<Vec<EntityId>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut files = BTreeSet::new();
		for file in open.stored.find(&CONTENT, &Value::Blob(*content))? {
			let file = state.canonical(file)?;
			if !gone(state, file)? && state.types(file)?.contains(&FILE) {
				files.insert(file);
			}
		}
		Ok(files.into_iter().collect())
	}

	/// Whether `entity` has, or had, a value of `field`, current or replaced.
	fn ever_had(
		&self,
		space: SpaceId,
		entity: EntityId,
		field: &FieldName,
	) -> Result<bool, NodeError> {
		let state = self.space(space)?.stored.log().state();
		for member in state.merged(entity)? {
			let had = state
				.storage()
				.of_entity(member)?
				.into_iter()
				.any(|(name, ids)| name == *field && !ids.is_empty());
			if had {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// Whether one of two files is an edit of the other.
	fn edits_of_each_other(
		&self,
		space: SpaceId,
		one: EntityId,
		other: EntityId,
	) -> Result<bool, NodeError> {
		let state = self.space(space)?.stored.log().state();
		for (edit, original) in [(one, other), (other, one)] {
			for statement in state.current(edit, &EDIT_OF)? {
				if let Value::Reference(linked) = statement.value
					&& state.canonical(linked)? == original
				{
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	/// Records that `from` refers to `to` in `field`, in a change of its own, the space's file
	/// type installed first in the version that has the field.
	fn link(
		&mut self,
		space: SpaceId,
		(from, field, to): (EntityId, &FieldName, EntityId),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.install_standard(space, &[FILE], wall_millis, rng)?;
		let operations = vec![Operation::assert(Statement::new(
			StatementId::random(rng),
			from,
			field.clone(),
			Value::Reference(to),
		))];
		self.commit(space, operations, wall_millis, rng)
	}
}

/// The folder of a path, with `/` or `\\` between folders: all of it but its last name.
fn parent_of(path: &str) -> &str {
	path.rfind(['/', '\\']).map_or("", |slash| &path[..slash])
}

/// Whether two times taken, as [`Edtf::local_seconds`] gives them, are the same second: the
/// same moment if both have an offset from UTC, the same local time if neither has, and else
/// the same moment in one of the [`OFFSETS`].
fn same_second(one: (i64, Option<i32>), other: (i64, Option<i32>)) -> bool {
	let utc = |(local, offset): (i64, i32)| local - i64::from(offset) * 60;
	match (one, other) {
		((one, Some(this)), (other, Some(that))) => utc((one, this)) == utc((other, that)),
		((one, None), (other, None)) => one == other,
		((local, None), (with, Some(offset))) | ((with, Some(offset)), (local, None)) => {
			let ahead = local - utc((with, offset));
			ahead % 60 == 0
				&& i32::try_from(ahead / 60).is_ok_and(|minutes| OFFSETS.contains(&minutes))
		}
	}
}

/// Whether two thumbnails' widths and heights have the same proportions, within
/// 1/[`PROPORTIONS_APART`].
fn same_proportions((width, height): (u32, u32), (other_width, other_height): (u32, u32)) -> bool {
	let one = u64::from(width) * u64::from(other_height);
	let other = u64::from(other_width) * u64::from(height);
	one.abs_diff(other) * PROPORTIONS_APART <= one.max(other)
}

/// The ways a copy may write the time `taken`, as [`Edtf::local_seconds`] gives it: the same
/// moment in each offset, with it or without; or, for a time without an offset, the same local
/// time with each offset, and the moment it is in each, in UTC.
fn written_as((local, offset): (i64, Option<i32>)) -> Vec<Edtf> {
	let mut written = Vec::new();
	if let Some(offset) = offset {
		let utc = local - i64::from(offset) * 60;
		for other in OFFSETS {
			let there = utc + i64::from(other) * 60;
			written.extend(Edtf::from_local_seconds(there, Some(other)));
			written.extend(Edtf::from_local_seconds(there, None));
		}
	} else {
		written.extend(Edtf::from_local_seconds(local, None));
		for other in OFFSETS {
			written.extend(Edtf::from_local_seconds(local, Some(other)));
			written.extend(Edtf::from_local_seconds(
				local - i64::from(other) * 60,
				Some(0),
			));
		}
	}
	written.sort_by_key(ToString::to_string);
	written.dedup();
	written
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_same_second_is_found_in_any_time_zone_and_another_is_not() {
		let at = |text: &str| -> (i64, Option<i32>) {
			text.parse::<Edtf>()
				.expect("a date and time")
				.local_seconds()
				.expect("a date and time")
		};
		let local = at("2019-12-24T18:30:00");
		for same in [
			"2019-12-24T18:30:00+01",
			"2019-12-24T17:30:00Z",
			"2019-12-24T18:30:00",
		] {
			assert!(same_second(local, at(same)), "{same}");
			assert!(same_second(at(same), local), "{same}");
			assert!(
				written_as(local).contains(&same.parse().expect("a date")),
				"{same}"
			);
		}
		assert!(same_second(
			at("2019-12-24T18:30:00+01"),
			at("2019-12-24T12:30:00-05")
		));
		assert!(
			written_as(at("2019-12-24T18:30:00+01"))
				.contains(&"2019-12-24T12:30:00-05".parse().expect("a date"))
		);
		for other in [
			"2019-12-24T18:30:01",
			"2019-12-24T18:30:01Z",
			"2019-12-24T18:31:00+01",
		] {
			assert!(!same_second(local, at(other)), "{other}");
		}
		assert!(!same_second(
			at("2019-12-24T18:30:00+01"),
			at("2019-12-24T18:30:00Z")
		));
	}

	#[test]
	fn proportions_are_the_same_but_for_rounding() {
		assert!(same_proportions((512, 384), (512, 383)));
		assert!(same_proportions((512, 384), (133, 100)));
		assert!(!same_proportions((512, 384), (384, 512)));
		assert!(!same_proportions((512, 384), (512, 288)));
	}
}
