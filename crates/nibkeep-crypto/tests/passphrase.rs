//! Keys derived from passphrases with Argon2id.

mod common;

use common::hex;
use nibkeep_base::cbor::{self, ToCbor, Value};
use nibkeep_crypto::{CryptoError, Key, PassphraseParams};

/// Parameters small enough for quick tests.
fn quick() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

#[test]
fn the_reference_test_vectors_pass() {
	// From the tests of the reference implementation of Argon2 (src/test.c), Argon2id
	// version 1.3, with the password "password" and the salt "somesalt".
	let vectors = [
		(
			256,
			2,
			1,
			"9dfeb910e80bad0311fee20f9c0e2b12c17987b4cac90c2ef54d5b3021c68bfe",
		),
		(
			256,
			2,
			2,
			"6d093c501fd5999645e0ea3bf620d7b8be7fd2db59c20d9fff9539da2bf57037",
		),
	];
	for (memory, iterations, lanes, expected) in vectors {
		let params = PassphraseParams::new(memory, iterations, lanes).expect("valid");
		let key = Key::from_passphrase("password", b"somesalt", &params).expect("derives");
		assert_eq!(key.expose_bytes().as_slice(), hex(expected));
	}
}

#[test]
fn a_passphrase_typed_differently_gives_the_same_key() {
	// "é" as one code point, and as "e" followed by a combining accent.
	let composed = Key::from_passphrase("caf\u{e9}", &[7; 16], &quick()).expect("derives");
	let decomposed = Key::from_passphrase("cafe\u{301}", &[7; 16], &quick()).expect("derives");
	assert_eq!(composed.expose_bytes(), decomposed.expose_bytes());
}

#[test]
fn other_passphrases_and_salts_give_other_keys() {
	let key = Key::from_passphrase("correct horse", &[7; 16], &quick()).expect("derives");
	let other_passphrase =
		Key::from_passphrase("correct horse!", &[7; 16], &quick()).expect("derives");
	let other_salt = Key::from_passphrase("correct horse", &[8; 16], &quick()).expect("derives");
	assert_ne!(key.expose_bytes(), other_passphrase.expose_bytes());
	assert_ne!(key.expose_bytes(), other_salt.expose_bytes());
}

#[test]
fn salts_shorter_than_8_bytes_are_rejected() {
	assert!(matches!(
		Key::from_passphrase("password", b"7 bytes", &quick()),
		Err(CryptoError::Passphrase(_))
	));
}

#[test]
fn invalid_parameters_are_rejected() {
	for (memory, iterations, lanes) in [
		(64, 0, 1),
		(64, 1, 0),
		(15, 1, 2),
		(u32::MAX, 1, 1 << 24),
		// The upper limits: 4 GiB of memory, 16 iterations and 16 lanes.
		(4 * 1024 * 1024 + 1, 1, 1),
		(64, 17, 1),
		(256, 1, 17),
	] {
		assert!(
			matches!(
				PassphraseParams::new(memory, iterations, lanes),
				Err(CryptoError::Passphrase(_))
			),
			"memory {memory}, iterations {iterations}, lanes {lanes} were accepted"
		);
	}
	assert!(PassphraseParams::new(16, 1, 2).is_ok());
	assert!(PassphraseParams::new(4 * 1024 * 1024, 16, 16).is_ok());
}

#[test]
fn the_recommended_parameters_are_those_of_rfc_9106() {
	let encoded = |params: &PassphraseParams| cbor::to_vec(params);
	let array =
		|items: [u32; 4]| cbor::encode(&Value::Array(items.iter().map(ToCbor::to_cbor).collect()));
	assert_eq!(
		encoded(&PassphraseParams::RECOMMENDED),
		array([1, 2_097_152, 1, 4])
	);
	assert_eq!(
		encoded(&PassphraseParams::LOW_MEMORY),
		array([1, 65_536, 3, 4])
	);
}

#[test]
fn parameters_round_trip_through_cbor_and_are_checked_when_read() {
	let params = PassphraseParams::new(65_536, 3, 4).expect("valid");
	assert_eq!(
		cbor::from_slice::<PassphraseParams>(&cbor::to_vec(&params)),
		Ok(params)
	);
	let read = |items: Vec<Value>| {
		cbor::from_slice::<PassphraseParams>(&cbor::encode(&Value::Array(items)))
	};
	// Unknown suite, invalid values, and the wrong number of items.
	assert!(
		read(vec![
			2_u8.to_cbor(),
			64_u32.to_cbor(),
			1_u32.to_cbor(),
			1_u32.to_cbor()
		])
		.is_err()
	);
	assert!(
		read(vec![
			1_u8.to_cbor(),
			64_u32.to_cbor(),
			0_u32.to_cbor(),
			1_u32.to_cbor()
		])
		.is_err()
	);
	assert!(read(vec![1_u8.to_cbor(), 64_u64.to_cbor(), 1_u32.to_cbor()]).is_err());
	// A damaged or hostile record cannot ask for unlimited memory and work.
	assert!(
		read(vec![
			1_u8.to_cbor(),
			u32::MAX.to_cbor(),
			u32::MAX.to_cbor(),
			4_u32.to_cbor()
		])
		.is_err()
	);
	assert!(
		read(vec![
			1_u8.to_cbor(),
			(1_u64 << 32).to_cbor(),
			1_u32.to_cbor(),
			1_u32.to_cbor()
		])
		.is_err()
	);
}
