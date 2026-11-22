//! What the fuzz targets check, for every input: that reading bytes from a disk or another
//! device never panics, and that what is read writes out again as it reads.
//!
//! Each function is a fuzz target. A target that reads several kinds of value takes the
//! first byte of the input to choose one. The tests replay the inputs in `seeds/`, among
//! them every input that once failed.

#![expect(
	clippy::missing_panics_doc,
	reason = "a fuzz target panics on purpose, to report what it found"
)]

use std::fmt::{Debug, Display};
use std::io::{Cursor, Read};
use std::str::FromStr;
use std::sync::OnceLock;

use nibkeep_base::cbor::{self, FromCbor, ToCbor};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_crypto::{BlobReader, DecryptionKey, Envelope, Key, SealedMessage};
use nibkeep_keys::KeyFile;
use nibkeep_log::{
	AccountPage, Change, Log, Parcel, Payload, SealedPayload, SpaceKeys, WrappedBlobKey,
	WrappedChangeKey, WrappedKey,
};
use nibkeep_model::{
	FieldDef, FieldName, Languages, Operation, Qualifiers, Statement, Tag, TypeDef, TypeName, Value,
};
use nibkeep_time::{Clock, Day, Edtf};

/// The key that sealed blobs in the seeds are sealed with, which the blob target reads
/// with, so that mutations of whole sealed blobs reach every chunk.
fn key() -> Key {
	Key::from_slice(&[7; 32]).expect("32 bytes")
}

/// The keys of the space that the log target reads changes of: the seeds' changes are
/// made with them, so that mutations of their payloads and keys reach every check.
#[must_use]
pub fn space_keys() -> SpaceKeys {
	let mut keys = SpaceKeys::new(0, key());
	keys.insert_secrets(0, key());
	keys
}

/// The content of the sealed blobs in the seeds: two chunks and a little more.
#[must_use]
pub fn blob_content() -> &'static [u8] {
	static CONTENT: OnceLock<Vec<u8>> = OnceLock::new();
	CONTENT.get_or_init(|| {
		(0..140_000_u32)
			.map(|index| u8::try_from(index % 251).expect("below 251"))
			.collect()
	})
}

/// The context of sealed messages in the seeds.
pub const CONTEXT: &str = "nibkeep fuzz";

/// Reads a value, and if it is one, checks that it writes out to the very bytes it was
/// read from: what is read is canonical, and nothing in it is lost or changed.
fn round_trip<T: FromCbor + ToCbor>(data: &[u8]) -> Option<T> {
	let value = cbor::from_slice::<T>(data).ok()?;
	assert!(
		cbor::to_vec(&value) == data,
		"what is read writes out as it was read"
	);
	Some(value)
}

/// Canonical CBOR: whatever decodes encodes to the same bytes, as only canonical encodings
/// are read.
pub fn cbor(data: &[u8]) {
	if let Ok(value) = cbor::decode(data) {
		assert_eq!(
			cbor::encode(&value),
			data,
			"canonical CBOR encodes as it was read"
		);
		assert_eq!(cbor::encoded_len(&value), data.len());
	}
}

// The first byte of the input chooses what a target reads. The choices never change their
// meaning, so that saved inputs keep testing what they were saved for; new readers take
// new numbers, and other bytes choose nothing.

/// The parts of a space's log: changes, which a log of their space then receives,
/// payloads, and keys; and changes with their sealed payloads and wrapped keys, which a log
/// receives and opens with [`space_keys`].
pub fn log(data: &[u8]) {
	match data.split_first() {
		Some((0, data)) => {
			if let Some(change) = round_trip::<Change>(data) {
				let parcel = Parcel {
					change,
					payload: None,
					keys: Vec::new(),
				};
				receive(parcel);
			}
		}
		Some((1, data)) => {
			round_trip::<Payload>(data);
		}
		Some((2, data)) => {
			round_trip::<WrappedKey>(data);
		}
		Some((3, data)) => {
			round_trip::<WrappedBlobKey>(data);
		}
		Some((4, data)) => {
			if let Some(parcel) = read_parcel(data) {
				receive(parcel);
			}
		}
		Some((5, data)) => {
			round_trip::<WrappedChangeKey>(data);
		}
		Some((6, data)) => {
			// A page of an account of copies, as another replica sends it: one that reads
			// writes out as it was read, and opens, or not, without panicking.
			if let Ok(page) = AccountPage::from_bytes(data) {
				assert!(
					page.to_bytes() == data,
					"what is read writes out as it was read"
				);
				let _ = page.open(&space_keys());
			}
		}
		_ => {}
	}
}

/// Reads a change with its parts: the CBOR array `[change, payload or null, [keys]]`, in
/// which only the change must be canonical.
fn read_parcel(data: &[u8]) -> Option<Parcel> {
	let cbor::Value::Array(items) = cbor::decode(data).ok()? else {
		return None;
	};
	let [change, payload, keys] = items.as_slice() else {
		return None;
	};
	let payload = match payload {
		cbor::Value::Null => None,
		payload => Some(SealedPayload::from_cbor(payload).ok()?),
	};
	Some(Parcel {
		change: Change::from_cbor(change).ok()?,
		payload,
		keys: Vec::from_cbor(keys).ok()?,
	})
}

/// Writes a change with its parts as [`read_parcel`] reads them.
#[must_use]
pub fn write_parcel(parcel: &Parcel) -> Vec<u8> {
	cbor::encode(&cbor::Value::Array(vec![
		parcel.change.to_cbor(),
		parcel
			.payload
			.as_ref()
			.map_or(cbor::Value::Null, ToCbor::to_cbor),
		cbor::Value::Array(parcel.keys.iter().map(ToCbor::to_cbor).collect()),
	]))
}

/// Receives a change into a log of its space, and reads its payload.
fn receive(parcel: Parcel) {
	let id = parcel.id();
	let mut log = Log::new(parcel.change.space(), Clock::new(3_600_000));
	let keys = space_keys();
	if log.receive(parcel, &keys, 1_800_000_000_000).is_ok() {
		let _ = log.payload(id, &keys);
	}
}

/// The parts of the data model: values, statements, operations, qualifiers and types.
pub fn model(data: &[u8]) {
	match data.split_first() {
		Some((0, data)) => {
			round_trip::<Value>(data);
		}
		Some((1, data)) => {
			round_trip::<Statement>(data);
		}
		Some((2, data)) => {
			round_trip::<Operation>(data);
		}
		Some((3, data)) => {
			round_trip::<Qualifiers>(data);
		}
		Some((4, data)) => {
			round_trip::<TypeDef>(data);
		}
		Some((5, data)) => {
			round_trip::<FieldDef>(data);
		}
		_ => {}
	}
}

/// A device's key file. Unlocking it is not tried: its passphrase parameters can ask for
/// any amount of work, which the key file's reader limits.
pub fn keys(data: &[u8]) {
	round_trip::<KeyFile>(data);
}

/// Encrypted data. Envelopes and sealed messages in the seeds are sealed with other keys
/// than the target opens them with, and must fail to open. Sealed blobs in the seeds are
/// sealed with the key the target reads them with: read to the end, they must be the
/// content they were sealed from.
pub fn crypto(data: &[u8]) {
	match data.split_first() {
		Some((0, data)) => {
			if let Some(envelope) = round_trip::<Envelope>(data) {
				assert!(
					envelope.open(&key(), b"").is_err(),
					"opens with the wrong key"
				);
			}
		}
		Some((1, data)) => {
			if let Some(message) = round_trip::<SealedMessage>(data) {
				let recipient = DecryptionKey::from_seed(&[7; 32]);
				assert!(
					message.open(&recipient, CONTEXT, b"").is_err(),
					"opens with the wrong key"
				);
			}
		}
		Some((2, data)) => {
			let content = Hash::of(blob_content());
			if let Ok(mut reader) = BlobReader::new(&key(), content, Cursor::new(data)) {
				let mut read = Vec::new();
				if reader.read_to_end(&mut read).is_ok() {
					assert!(read == blob_content(), "a blob reads as its content");
				}
			}
		}
		_ => {}
	}
}

/// Reads text as a value, and if it is one, checks that it writes out to text that reads
/// back as the same value.
fn text_round_trip<T: FromStr + Display + PartialEq + Debug>(text: &str) {
	if let Ok(value) = text.parse::<T>() {
		let written = value.to_string();
		let again = written.parse::<T>().ok();
		assert_eq!(
			again.as_ref(),
			Some(&value),
			"{written:?} reads back as it was written"
		);
	}
}

/// The text forms of dates, names, identifiers, hashes, ranked languages, language tags and
/// the space keys that statements seal to replicas.
pub fn text(data: &[u8]) {
	let Some((&choice, data)) = data.split_first() else {
		return;
	};
	let Ok(text) = std::str::from_utf8(data) else {
		return;
	};
	match choice {
		0 => text_round_trip::<Edtf>(text),
		1 => text_round_trip::<Day>(text),
		2 => text_round_trip::<FieldName>(text),
		3 => text_round_trip::<TypeName>(text),
		4 => text_round_trip::<Hash>(text),
		5 => text_round_trip::<EntityId>(text),
		6 => text_round_trip::<StatementId>(text),
		7 => text_round_trip::<Languages>(text),
		8 => {
			if let Some(tag) = Tag::parse(text) {
				let written = tag.to_string();
				assert_eq!(
					Tag::parse(&written).as_ref(),
					Some(&tag),
					"{written:?} reads back as it was written"
				);
				// Matching a tag against ranges never fails, whatever the tag.
				let ranges: Languages = "en, *-Latn, ru".parse().expect("ranges");
				assert!(ranges.rank(Some(&tag)) <= ranges.len());
			}
		}
		9 => {
			if let Some(key) = nibkeep_governance::SealedSpaceKey::from_text(text) {
				assert_eq!(
					key.to_text(),
					text,
					"a sealed key writes out as it was read"
				);
			}
		}
		_ => {}
	}
}

/// What an encrypted database is to the store's VFS, written here apart from the store, as
/// the storage specification describes it: the header, which holds the database's own key
/// sealed under the key it is opened with, then each page sealed with its place under the
/// database's own key.
pub mod encrypted_format {
	use nibkeep_crypto::rand_core::CryptoRng;
	use nibkeep_crypto::{Key, PAGE_NONCE_LEN, PageCipher, RECORD_OVERHEAD};

	/// The size of a page.
	pub const PAGE: usize = 4096;

	/// The header of a database opened with `key`, whose own key is `own`.
	#[must_use]
	pub fn header(key: &Key, own: &Key, rng: &mut impl CryptoRng) -> Vec<u8> {
		let mut header = vec![0; 24 + 32 + RECORD_OVERHEAD];
		header[..16].copy_from_slice(b"nibkeep state\0\0\x01");
		header[16..20].copy_from_slice(&4096_u32.to_le_bytes());
		header[24 + PAGE_NONCE_LEN..][..32].copy_from_slice(own.expose_bytes());
		let associated = [b"H".as_slice(), &header[..24]].concat();
		PageCipher::new(key).seal(rng, &associated, &mut header[24..]);
		header
	}

	/// The page `index` of a database under `key`, sealed in its place.
	#[must_use]
	pub fn page(key: &Key, rng: &mut impl CryptoRng, index: u64, page: &[u8]) -> Vec<u8> {
		let mut record = vec![0; PAGE + RECORD_OVERHEAD];
		record[PAGE_NONCE_LEN..PAGE_NONCE_LEN + page.len().min(PAGE)]
			.copy_from_slice(&page[..page.len().min(PAGE)]);
		let mut associated = vec![b'D'];
		associated.extend_from_slice(&index.to_le_bytes());
		PageCipher::new(key).seal(rng, &associated, &mut record);
		record
	}
}

/// The key of encrypted databases in the seeds, which the target opens them with.
fn database_key() -> Key {
	Key::from_slice(&[9; 32]).expect("32 bytes")
}

/// The own key of the databases that the target seals pages for.
fn own_key() -> Key {
	Key::from_slice(&[10; 32]).expect("32 bytes")
}

/// Randomness for the target: the same on every run, which is all a fuzz target needs.
struct Counter(u64);

impl nibkeep_crypto::rand_core::TryRng for Counter {
	type Error = std::convert::Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
		self.0 += 1;
		Ok(u32::try_from(self.0 & 0xffff_ffff).expect("fits"))
	}

	fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
		self.0 += 1;
		Ok(self.0)
	}

	fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
		for chunk in destination.chunks_mut(8) {
			self.0 += 1;
			chunk.copy_from_slice(&self.0.to_le_bytes()[..chunk.len()]);
		}
		Ok(())
	}
}

impl nibkeep_crypto::rand_core::TryCryptoRng for Counter {}

/// Encrypted databases, as the store's VFS reads them: whatever is on disk, SQLite reading
/// through the VFS never panics. With the first byte 0, the rest is a database and its
/// write-ahead log as they lie on disk, apart where the next two bytes say. With 1, the
/// rest is pages, sealed in place under the key, so that SQLite reads through the VFS
/// whatever the pages hold, its own header among them.
pub fn encrypted(data: &[u8]) {
	use std::path::PathBuf;
	let directory =
		std::env::temp_dir().join(format!("nibkeep-fuzz-encrypted-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&directory);
	std::fs::create_dir_all(&directory).expect("makes a directory");
	let database: PathBuf = directory.join("state.sqlite");
	let log = directory.join("state.sqlite-wal");
	match data.split_first() {
		Some((0, rest)) if rest.len() >= 2 => {
			let split = usize::from(u16::from_le_bytes([rest[0], rest[1]])).min(rest.len() - 2);
			let (file, wal) = rest[2..].split_at(split);
			std::fs::write(&database, file).expect("writes the database");
			std::fs::write(&log, wal).expect("writes the log");
		}
		Some((1, pages)) => {
			let own = own_key();
			let mut rng = Counter(0);
			let mut file = encrypted_format::header(&database_key(), &own, &mut rng);
			for (index, page) in pages.chunks(encrypted_format::PAGE).enumerate() {
				let index = u64::try_from(index).expect("small");
				file.extend(encrypted_format::page(&own, &mut rng, index, page));
			}
			std::fs::write(&database, file).expect("writes the database");
		}
		_ => return,
	}
	if let Ok(opened) =
		nibkeep_store::EncryptedDatabase::open(&database, &database_key(), Counter(1 << 40))
	{
		let connection = opened.connection();
		for query in [
			"PRAGMA integrity_check",
			"SELECT name, sql FROM sqlite_schema",
			"PRAGMA wal_checkpoint(TRUNCATE)",
		] {
			if let Ok(mut statement) = connection.prepare(query) {
				let _ = statement.query_map([], |_| Ok(())).map(|rows| rows.count());
			}
		}
	}
	let _ = std::fs::remove_dir_all(&directory);
}

/// Messages from other devices: of pairing, of syncing and of invitations, which must write
/// out as they were read, and of negentropy, which must not panic; and packs, which
/// backups hold, whose items must write out as they were read.
pub fn sync(data: &[u8]) {
	match data.split_first() {
		Some((0, data)) => {
			if let Some(written) = nibkeep_sync::fuzzing::pair_message(data) {
				assert!(written == data, "what is read writes out as it was read");
			}
		}
		Some((1, data)) => {
			if let Some(written) = nibkeep_sync::fuzzing::sync_message(data) {
				assert!(written == data, "what is read writes out as it was read");
			}
		}
		Some((3, data)) => {
			if let Some(written) = nibkeep_sync::fuzzing::invite_message(data) {
				assert!(written == data, "what is read writes out as it was read");
			}
		}
		Some((4, data)) => {
			let _ = nibkeep_sync::fuzzing::pack(data);
		}
		Some((_, data)) => nibkeep_sync::fuzzing::negentropy(data),
		None => {}
	}
}
