//! Hashes name their algorithm, in binary and in text.

use std::io::Write;

use nibkeep_base::cbor::{self, Bytes, Value};
use nibkeep_base::{Hash, HashAlgorithm, Hasher, ParseHashError};

/// The BLAKE3 hash of empty input, from the BLAKE3 test vectors.
const EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

#[test]
fn blake3_of_empty_input_matches_the_test_vector() {
	let hash = Hash::of(b"");
	assert_eq!(hash.algorithm(), HashAlgorithm::Blake3);
	assert_eq!(hash.to_string(), format!("blake3:{EMPTY}"));
}

#[test]
fn text_form_survives_a_round_trip() {
	let hash = Hash::of(b"hello");
	assert_eq!(hash.to_string().parse::<Hash>(), Ok(hash));
}

#[test]
fn text_form_is_strict() {
	let digest_63 = &EMPTY[..63];
	for text in [
		format!("sha256:{EMPTY}"),
		format!("blake3:{digest_63}"),
		format!("blake3:{EMPTY}00"),
		format!("blake3:{}", EMPTY.to_uppercase()),
		format!("BLAKE3:{EMPTY}"),
		EMPTY.to_owned(),
		format!("blake3: {EMPTY}"),
		String::new(),
	] {
		assert!(text.parse::<Hash>().is_err(), "{text} was accepted");
	}
	assert_eq!(
		format!("sha256:{EMPTY}").parse::<Hash>(),
		Err(ParseHashError::UnknownAlgorithm)
	);
}

#[test]
fn binary_form_is_a_multihash() {
	let hash = Hash::of(b"");
	let multihash = hash.to_multihash();
	assert_eq!(multihash.len(), 34);
	assert_eq!(&multihash[..2], &[0x1e, 0x20]);
	assert_eq!(Hash::from_multihash(&multihash), Ok(hash));
}

#[test]
fn binary_form_is_strict() {
	let digest = Hash::of(b"").digest().to_vec();
	let with = |prefix: &[u8], digest: &[u8]| [prefix, digest].concat();
	// SHA-256 has the multihash code 0x12.
	assert_eq!(
		Hash::from_multihash(&with(&[0x12, 0x20], &digest)),
		Err(ParseHashError::UnknownAlgorithm)
	);
	assert_eq!(
		Hash::from_multihash(&with(&[0x1e, 0x1f], &digest[..31])),
		Err(ParseHashError::WrongLength)
	);
	assert_eq!(
		Hash::from_multihash(&with(&[0x1e, 0x20], &digest[..31])),
		Err(ParseHashError::Malformed)
	);
	assert_eq!(
		Hash::from_multihash(&with(&[0x1e, 0x20], &[digest.as_slice(), &[0]].concat())),
		Err(ParseHashError::Malformed)
	);
	// The code 0x1e written in two bytes instead of one is not minimal.
	assert_eq!(
		Hash::from_multihash(&with(&[0x9e, 0x00, 0x20], &digest)),
		Err(ParseHashError::Malformed)
	);
	assert_eq!(Hash::from_multihash(&[]), Err(ParseHashError::Malformed));
}

#[test]
fn incremental_hashing_matches_hashing_at_once() {
	let mut hasher = Hasher::new();
	hasher.update(b"hello, ");
	hasher
		.write_all(b"world")
		.expect("writing to a hasher cannot fail");
	assert_eq!(hasher.finalize(), Hash::of(b"hello, world"));
}

#[test]
fn hashes_are_encoded_as_multihash_byte_strings() {
	let hash = Hash::of(b"hello");
	assert_eq!(
		cbor::to_vec(&hash),
		cbor::to_vec(&Bytes(hash.to_multihash()))
	);
	assert_eq!(cbor::from_slice::<Hash>(&cbor::to_vec(&hash)), Ok(hash));
	assert!(cbor::from_slice::<Hash>(&cbor::encode(&Value::Text(hash.to_string()))).is_err());
}
