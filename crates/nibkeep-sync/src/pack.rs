//! Packs: files of a space's items, as the messages of a sync carry them, which backups hold
//! without keys ([Storage](../../../docs/spec/storage.md#6-backups)).
//!
//! A pack is a CBOR sequence: `[1, space, update]`, then items of that space, each the
//! message `[4, space, …]` that a sync sends of it, and a blob's bytes after its item as
//! one message `[7, bytes]`.

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{Hash, SpaceId, StatementId};
use nibkeep_log::{Parcel, WrappedBlobKey, WrappedKey};

use crate::SyncError;
use crate::message::{Item, SyncMessage, bytes_message};

/// The version of the format of packs.
const VERSION: u64 = 1;

/// The largest sealed blob that a pack holds, 1 MiB: a larger one has a file of its own.
pub const MOST_PACK_BLOB: u64 = 1024 * 1024;

/// How many bytes a pack takes at most, 64 MiB, but for a single item that is larger: a
/// writer with more to write writes another.
pub const PACK_SIZE: usize = 64 * 1024 * 1024;

/// An item of a pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackItem {
	/// A change, with its sealed payload and keys as far as the writer held them.
	Change(Box<Parcel>),
	/// A wrapped record key of a secret statement.
	RecordKey(StatementId, WrappedKey),
	/// A wrapped blob key.
	BlobKey(Hash, WrappedBlobKey),
	/// A sealed blob of at most [`MOST_PACK_BLOB`] bytes, by its sealed hash.
	Blob(Hash, Vec<u8>),
}

/// The encoding of a pack's first value: its space, and the number of the update that wrote
/// it.
#[must_use]
pub fn pack_header(space: SpaceId, update: u64) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![
		VERSION.to_cbor(),
		space.to_cbor(),
		update.to_cbor(),
	]))
}

/// The encoding of an item in a pack of `space`: the message that a sync sends of it, and of
/// a blob, its bytes after it.
#[must_use]
pub fn pack_item(space: SpaceId, item: &PackItem) -> Vec<u8> {
	match item {
		PackItem::Change(parcel) => SyncMessage::Item(space, Item::Change(parcel.clone())).encode(),
		PackItem::RecordKey(statement, key) => {
			SyncMessage::Item(space, Item::RecordKey(*statement, key.clone())).encode()
		}
		PackItem::BlobKey(sealed, key) => {
			SyncMessage::Item(space, Item::BlobKey(*sealed, key.clone())).encode()
		}
		PackItem::Blob(sealed, bytes) => {
			let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
			let mut encoded = SyncMessage::Item(space, Item::Blob(*sealed, size)).encode();
			encoded.extend(bytes_message(bytes));
			encoded
		}
	}
}

/// A pack as it reads: its space, the update that wrote it, and its items, one after the
/// other, each checked as it is read.
pub struct PackReader<'a> {
	bytes: &'a [u8],
	position: usize,
	space: SpaceId,
	update: u64,
}

impl<'a> PackReader<'a> {
	/// Starts reading a pack from its bytes.
	///
	/// # Errors
	///
	/// Returns [`SyncError::Protocol`] if the bytes do not start with a pack's first value.
	pub fn new(bytes: &'a [u8]) -> Result<Self, SyncError> {
		let (header, length) = first(bytes)?;
		let Value::Array(fields) = header else {
			return Err(SyncError::protocol("a pack starts with [1, space, update]"));
		};
		let [version, space, update] = fields.as_slice() else {
			return Err(SyncError::protocol("a pack starts with [1, space, update]"));
		};
		if u64::from_cbor(version).ok() != Some(VERSION) {
			return Err(SyncError::protocol("a pack of another version"));
		}
		let wrong = |error: cbor::Error| SyncError::protocol(error.to_string());
		Ok(Self {
			bytes,
			position: length,
			space: SpaceId::from_cbor(space).map_err(wrong)?,
			update: u64::from_cbor(update).map_err(wrong)?,
		})
	}

	/// The item whose encoding `bytes` holds whole, as [`PackReader::next_item`] found it
	/// in a pack of `space`.
	///
	/// # Errors
	///
	/// Returns [`SyncError::Protocol`] if `bytes` is not exactly one such item.
	pub fn item_at(space: SpaceId, bytes: &[u8]) -> Result<PackItem, SyncError> {
		let mut reader = PackReader {
			bytes,
			position: 0,
			space,
			update: 0,
		};
		match reader.next_item()? {
			Some((item, _, length)) if length == bytes.len() => Ok(item),
			_ => Err(SyncError::protocol("not one item of a pack")),
		}
	}

	/// The space whose items the pack holds.
	#[must_use]
	pub const fn space(&self) -> SpaceId {
		self.space
	}

	/// The number of the update that wrote the pack.
	#[must_use]
	pub const fn update(&self) -> u64 {
		self.update
	}

	/// The next item, with where its encoding starts in the pack and how long it is, or
	/// `None` at the pack's end.
	///
	/// # Errors
	///
	/// Returns [`SyncError::Protocol`] if what follows is no item of the pack's space, or a
	/// blob whose bytes do not have its sealed hash, or is larger than [`MOST_PACK_BLOB`].
	pub fn next_item(&mut self) -> Result<Option<(PackItem, usize, usize)>, SyncError> {
		if self.position == self.bytes.len() {
			return Ok(None);
		}
		let start = self.position;
		let message = self.message()?;
		let SyncMessage::Item(space, item) = message else {
			return Err(SyncError::protocol("a pack holds items only"));
		};
		if space != self.space {
			return Err(SyncError::protocol("an item of another space in a pack"));
		}
		let item = match item {
			Item::Change(parcel) => PackItem::Change(parcel),
			Item::RecordKey(statement, key) => PackItem::RecordKey(statement, key),
			Item::BlobKey(sealed, key) => PackItem::BlobKey(sealed, key),
			Item::Blob(sealed, size) => {
				if size > MOST_PACK_BLOB {
					return Err(SyncError::protocol("a blob too large for a pack"));
				}
				let SyncMessage::Bytes(bytes) = self.message()? else {
					return Err(SyncError::protocol("a blob's item without its bytes"));
				};
				if u64::try_from(bytes.len()).ok() != Some(size) || Hash::of(&bytes) != sealed {
					return Err(SyncError::protocol("a blob whose bytes are not its own"));
				}
				PackItem::Blob(sealed, bytes)
			}
			Item::Account(_) => return Err(SyncError::protocol("a page of an account in a pack")),
		};
		Ok(Some((item, start, self.position - start)))
	}

	/// The next message, read whole.
	fn message(&mut self) -> Result<SyncMessage, SyncError> {
		let rest = self.bytes.get(self.position..).unwrap_or_default();
		let (value, length) = first(rest)?;
		let message = SyncMessage::from_value(value)?;
		self.position += length;
		Ok(message)
	}
}

/// The first value of `bytes`, and how long its encoding is.
fn first(bytes: &[u8]) -> Result<(Value, usize), SyncError> {
	cbor::decode_first(bytes).map_err(|error| SyncError::protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_pack_reads_back_its_blobs_and_refuses_one_that_is_not_its_own() {
		let space = SpaceId::from_bytes([7; 16]);
		let blob = b"sealed bytes".to_vec();
		let sealed = Hash::of(&blob);
		let mut pack = pack_header(space, 3);
		pack.extend(pack_item(space, &PackItem::Blob(sealed, blob.clone())));
		let mut reader = PackReader::new(&pack).expect("a pack");
		assert_eq!((reader.space(), reader.update()), (space, 3));
		let (item, start, length) = reader.next_item().expect("reads").expect("an item");
		assert_eq!(item, PackItem::Blob(sealed, blob));
		assert_eq!(start + length, pack.len());
		assert!(reader.next_item().expect("reads").is_none());
		// Another blob's bytes under that sealed hash.
		let mut forged = pack_header(space, 3);
		forged.extend(pack_item(space, &PackItem::Blob(sealed, b"other".to_vec())));
		let mut reader = PackReader::new(&forged).expect("a pack");
		assert!(reader.next_item().is_err());
		// An item of another space.
		let mut mixed = pack_header(space, 3);
		mixed.extend(pack_item(
			SpaceId::from_bytes([8; 16]),
			&PackItem::Blob(sealed, b"sealed bytes".to_vec()),
		));
		let mut reader = PackReader::new(&mixed).expect("a pack");
		assert!(reader.next_item().is_err());
	}
}
