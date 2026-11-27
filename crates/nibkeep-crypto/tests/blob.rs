//! Sealed blobs: content encrypted in chunks with a key of its own.

use std::io::{BufReader, Cursor, ErrorKind, Read, Seek, SeekFrom};

use crate::common::{FixedBytes, TestRng, hex, rng};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use nibkeep_base::Hash;
use nibkeep_crypto::{BLOB_CHUNK_LEN, BlobReader, Key, seal_blob};

/// The test vector for ChaCha20-Poly1305 in RFC 8439, section 2.8.2.
mod vector {
	pub(crate) const PLAINTEXT: &[u8] = b"Ladies and Gentlemen of the class of '99: \
		If I could offer you only one tip for the future, sunscreen would be it.";
	pub(crate) const ASSOCIATED_DATA: &str = "50515253c0c1c2c3c4c5c6c7";
	pub(crate) const KEY: &str = "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f";
	pub(crate) const NONCE: &str = "070000004041424344454647";
	pub(crate) const CIPHERTEXT: &str = "
		d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6
		3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36
		92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc
		3ff4def08e4b7a9de576d26586cec64b6116";
	pub(crate) const TAG: &str = "1ae10b594f09e26a7e902ecbd0600691";
}

/// The sealed blob of the content `nibkeep` under the blob key whose bytes are 0, 1, …, 31,
/// with the nonce whose bytes are 32, 33, …, 47, as published in the specification.
const SEALED_NIBKEEP: &str =
	"01202122232425262728292a2b2c2d2e2facbab6d359b5578295540cc903d79f19081e602f479509";

/// Chunk sizes around the edges of the format.
const SIZES: [usize; 7] = [
	0,
	1,
	BLOB_CHUNK_LEN - 1,
	BLOB_CHUNK_LEN,
	BLOB_CHUNK_LEN + 1,
	2 * BLOB_CHUNK_LEN,
	3 * BLOB_CHUNK_LEN + 5,
];

/// The length of the suite and the nonce.
const HEADER_LEN: usize = 17;

/// The length of an encrypted chunk: the chunk and its tag.
const SEALED_CHUNK_LEN: usize = BLOB_CHUNK_LEN + 16;

fn key(seed: &str) -> Key {
	Key::generate(&mut rng(seed))
}

/// Content whose bytes differ from chunk to chunk.
fn content(len: usize) -> Vec<u8> {
	(0..len)
		.map(|index| u8::try_from(index % 251).expect("below 251"))
		.collect()
}

/// Seals content, and checks what sealing tells about it.
fn seal_with(key: &Key, content: &[u8], rng: &mut TestRng) -> Vec<u8> {
	let mut sealed = Vec::new();
	let blob = seal_blob(key, content, &mut sealed, rng).expect("seals");
	assert_eq!(blob.content, Hash::of(content));
	assert_eq!(blob.sealed, Hash::of(&sealed));
	assert_eq!(blob.len, u64::try_from(content.len()).expect("fits"));
	sealed
}

fn seal(key: &Key, content: &[u8]) -> Vec<u8> {
	seal_with(key, content, &mut rng("nonces"))
}

fn reader<'a>(key: &Key, content: Hash, sealed: &'a [u8]) -> BlobReader<Cursor<&'a [u8]>> {
	BlobReader::new(key, content, Cursor::new(sealed)).expect("opens")
}

/// Reads a whole sealed blob.
fn read_all(key: &Key, content: Hash, sealed: &[u8]) -> std::io::Result<Vec<u8>> {
	let mut read = Vec::new();
	BlobReader::new(key, content, Cursor::new(sealed))?.read_to_end(&mut read)?;
	Ok(read)
}

#[test]
fn blobs_of_every_size_survive_sealing() {
	let key = key("sizes");
	for size in SIZES {
		let content = content(size);
		let sealed = seal(&key, &content);
		let chunks = size.div_ceil(BLOB_CHUNK_LEN).max(1);
		assert_eq!(sealed.len(), HEADER_LEN + size + 16 * chunks, "size {size}");
		let hash = Hash::of(&content);
		assert_eq!(read_all(&key, hash, &sealed).expect("opens"), content);
		assert_eq!(
			reader(&key, hash, &sealed).len(),
			u64::try_from(size).expect("fits")
		);
	}
}

#[test]
fn every_sealing_is_different() {
	let content = content(BLOB_CHUNK_LEN + 10);
	let key = key("twice");
	let mut rng = rng("nonces");
	let first = seal_with(&key, &content, &mut rng);
	let second = seal_with(&key, &content, &mut rng);
	assert_ne!(first, second, "a new nonce, and so other chunks");
	assert_ne!(
		first[HEADER_LEN..HEADER_LEN + 32],
		second[HEADER_LEN..HEADER_LEN + 32]
	);
	for sealed in [first, second] {
		assert_eq!(
			read_all(&key, Hash::of(&content), &sealed).expect("opens"),
			content
		);
		let sample = &content[1000..1064];
		assert!(!sealed.windows(sample.len()).any(|window| window == sample));
	}
}

#[test]
fn a_reader_can_start_anywhere() {
	let key = key("seeking");
	let content = content(3 * BLOB_CHUNK_LEN + 5);
	let sealed = seal(&key, &content);
	let mut reader = reader(&key, Hash::of(&content), &sealed);
	for start in [
		0,
		1,
		BLOB_CHUNK_LEN - 3,
		BLOB_CHUNK_LEN,
		2 * BLOB_CHUNK_LEN + 7,
		content.len() - 2,
		content.len(),
	] {
		let position = u64::try_from(start).expect("fits");
		assert_eq!(
			reader.seek(SeekFrom::Start(position)).expect("seeks"),
			position
		);
		let mut read = Vec::new();
		(&mut reader)
			.take(10)
			.read_to_end(&mut read)
			.expect("reads");
		assert_eq!(
			read,
			&content[start..(start + 10).min(content.len())],
			"from {start}"
		);
	}
	let end = u64::try_from(content.len()).expect("fits");
	assert_eq!(reader.seek(SeekFrom::End(-1)).expect("seeks"), end - 1);
	assert_eq!(reader.seek(SeekFrom::Current(-1)).expect("seeks"), end - 2);
	assert!(reader.seek(SeekFrom::Current(-i64::MAX)).is_err());
	let mut rest = Vec::new();
	reader.read_to_end(&mut rest).expect("reads");
	assert_eq!(rest, &content[content.len() - 2..]);
}

#[test]
fn the_content_hash_is_checked_even_when_reads_overlap() {
	let key = key("overlap");
	let content = content(2 * BLOB_CHUNK_LEN + 100);
	let sealed = seal(&key, &content);
	for (hash, good) in [(Hash::of(&content), true), (Hash::of(b"other"), false)] {
		// A buffered reader that is asked for its position seeks the reader back by what
		// it buffered, so its next read starts within what was read already.
		let mut buffered = BufReader::with_capacity(1000, reader(&key, hash, &sealed));
		let mut start = [0; 10];
		buffered.read_exact(&mut start).expect("reads");
		assert_eq!(buffered.stream_position().expect("seeks"), 10);
		let mut rest = Vec::new();
		let result = buffered.read_to_end(&mut rest);
		if good {
			result.expect("reads the rest");
			assert_eq!(rest, &content[10..]);
		} else {
			let error = result.expect_err("the wrong content hash");
			assert_eq!(error.kind(), ErrorKind::InvalidData);
		}
	}
}

#[test]
fn damaged_sealed_blobs_do_not_open() {
	let key = key("damage");
	let content = content(2 * BLOB_CHUNK_LEN + 100);
	let hash = Hash::of(&content);
	let sealed = seal(&key, &content);
	let damaged = |damage: &dyn Fn(&mut Vec<u8>)| {
		let mut copy = sealed.clone();
		damage(&mut copy);
		copy
	};
	let cases: [(&str, Vec<u8>); 12] = [
		("another suite", damaged(&|sealed| sealed[0] = 2)),
		("a changed nonce", damaged(&|sealed| sealed[5] ^= 1)),
		(
			"a changed byte in the first chunk",
			damaged(&|sealed| sealed[HEADER_LEN + 10] ^= 1),
		),
		(
			"a changed byte in the last chunk",
			damaged(&|sealed| {
				let last = sealed.len() - 1;
				sealed[last] ^= 1;
			}),
		),
		(
			"the last chunk cut off",
			damaged(&|sealed| sealed.truncate(HEADER_LEN + 2 * SEALED_CHUNK_LEN)),
		),
		(
			"a chunk cut short",
			damaged(&|sealed| sealed.truncate(sealed.len() - 5)),
		),
		(
			"bytes added at the end",
			damaged(&|sealed| sealed.extend_from_slice(&[0; 20])),
		),
		(
			"a whole chunk added at the end",
			damaged(&|sealed| {
				let first = sealed[HEADER_LEN..HEADER_LEN + SEALED_CHUNK_LEN].to_vec();
				sealed.extend_from_slice(&first);
			}),
		),
		(
			"two chunks swapped",
			damaged(&|sealed| {
				let (first, rest) = sealed[HEADER_LEN..].split_at_mut(SEALED_CHUNK_LEN);
				first.swap_with_slice(&mut rest[..SEALED_CHUNK_LEN]);
			}),
		),
		("nothing at all", Vec::new()),
		("only the suite", vec![1]),
		("only the header", sealed[..HEADER_LEN].to_vec()),
	];
	for (case, sealed) in cases {
		let error = read_all(&key, hash, &sealed).expect_err(case);
		assert_eq!(error.kind(), ErrorKind::InvalidData, "{case}");
	}
	let error = read_all(&self::key("another"), hash, &sealed).expect_err("another key");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
	let error = read_all(&key, Hash::of(b"other"), &sealed).expect_err("another hash");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn an_empty_blob_is_still_authenticated() {
	let key = key("empty");
	let hash = Hash::of(b"");
	let sealed = seal(&key, b"");
	assert_eq!(sealed.len(), HEADER_LEN + 16);
	assert_eq!(read_all(&key, hash, &sealed).expect("opens"), b"");
	let mut forged = sealed.clone();
	let last = forged.len() - 1;
	forged[last] ^= 1;
	let error = read_all(&key, hash, &forged).expect_err("a forged tag");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
	let error = read_all(&key, Hash::of(b"other"), &sealed).expect_err("another hash");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn reading_part_of_a_blob_needs_only_its_chunks() {
	let key = key("parts");
	let content = content(2 * BLOB_CHUNK_LEN + 100);
	let mut sealed = seal(&key, &content);
	// Damage the first chunk: the last one still reads.
	sealed[HEADER_LEN + 10] ^= 1;
	let mut reader = reader(&key, Hash::of(&content), &sealed);
	reader
		.seek(SeekFrom::Start(
			u64::try_from(2 * BLOB_CHUNK_LEN).expect("fits"),
		))
		.expect("seeks");
	let mut rest = Vec::new();
	reader.read_to_end(&mut rest).expect("reads the last chunk");
	assert_eq!(rest, &content[2 * BLOB_CHUNK_LEN..]);
	reader.rewind().expect("seeks");
	let error = reader.read(&mut [0; 10]).expect_err("the damaged chunk");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
}

/// The format of section 6 of the cryptography specification, built from ChaCha20-Poly1305
/// and BLAKE3 directly.
#[test]
fn sealed_blobs_follow_the_specification() {
	let blob_key: [u8; 32] = std::array::from_fn(|index| u8::try_from(index).expect("small"));
	let nonce: [u8; 16] = std::array::from_fn(|index| u8::try_from(32 + index).expect("small"));
	let key = Key::from_bytes(blob_key);
	let seal_fixed = |content: &[u8]| {
		let mut sealed = Vec::new();
		seal_blob(
			&key,
			content,
			&mut sealed,
			&mut FixedBytes::new(nonce.to_vec()),
		)
		.expect("seals");
		sealed
	};
	let chunks_key = blake3::Hasher::new_derive_key("nibkeep 2026-10 blob chunks")
		.update(&blob_key)
		.update(&nonce)
		.finalize();
	let cipher = ChaCha20Poly1305::new(chunks_key.as_bytes().into());
	for size in SIZES {
		let content = content(size);
		let chunks: Vec<&[u8]> = if content.is_empty() {
			vec![&[]]
		} else {
			content.chunks(BLOB_CHUNK_LEN).collect()
		};
		let mut expected = vec![1];
		expected.extend_from_slice(&nonce);
		for (index, chunk) in chunks.iter().enumerate() {
			let mut chunk_nonce = [0; 12];
			chunk_nonce[3..11].copy_from_slice(&u64::try_from(index).expect("fits").to_be_bytes());
			chunk_nonce[11] = u8::from(index == chunks.len() - 1);
			let encrypted = cipher
				.encrypt(&Nonce::from(chunk_nonce), *chunk)
				.expect("encrypts");
			expected.extend_from_slice(&encrypted);
		}
		assert_eq!(seal_fixed(&content), expected, "size {size}");
	}
	assert_eq!(
		seal_fixed(b"nibkeep"),
		hex(SEALED_NIBKEEP),
		"the published vector for the content `nibkeep`"
	);
}

#[test]
fn chacha20_poly1305_matches_its_published_vector() {
	let key: [u8; 32] = hex(vector::KEY).try_into().expect("32 bytes");
	let nonce: [u8; 12] = hex(vector::NONCE).try_into().expect("12 bytes");
	let associated_data = hex(vector::ASSOCIATED_DATA);
	let cipher = ChaCha20Poly1305::new(&key.into());
	let payload = Payload {
		msg: vector::PLAINTEXT,
		aad: &associated_data,
	};
	let encrypted = cipher
		.encrypt(&Nonce::from(nonce), payload)
		.expect("encrypts");
	let mut expected = hex(vector::CIPHERTEXT);
	expected.extend(hex(vector::TAG));
	assert_eq!(encrypted, expected);
}

#[test]
fn keys_can_be_read_from_a_slice() {
	let bytes: [u8; 32] = std::array::from_fn(|index| u8::try_from(index).expect("small"));
	let key = Key::from_slice(&bytes).expect("32 bytes");
	assert_eq!(key.expose_bytes(), &bytes);
	assert!(Key::from_slice(&bytes[1..]).is_none());
	assert!(Key::from_slice(&[0; 33]).is_none());
}
