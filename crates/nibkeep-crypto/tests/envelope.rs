//! Envelopes: data encrypted with a key.

use crate::common::{FixedBytes, hex, rng};
use nibkeep_base::cbor::{self, ToCbor, Value};
use nibkeep_crypto::{CryptoError, Envelope, Key};

/// The test vector for XChaCha20-Poly1305 in draft-irtf-cfrg-xchacha-03, appendix A.3.1.
mod vector {
	pub(crate) const PLAINTEXT: &str = "
		4c616469657320616e642047656e746c656d656e206f662074686520636c6173
		73206f66202739393a204966204920636f756c64206f6666657220796f75206f
		6e6c79206f6e652074697020666f7220746865206675747572652c2073756e73
		637265656e20776f756c642062652069742e";
	pub(crate) const ASSOCIATED_DATA: &str = "50515253c0c1c2c3c4c5c6c7";
	pub(crate) const KEY: &str = "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f";
	pub(crate) const NONCE: &str = "404142434445464748494a4b4c4d4e4f5051525354555657";
	pub(crate) const CIPHERTEXT: &str = "
		bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb
		731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452
		2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9
		21f9664c97637da9768812f615c68b13b52e";
	pub(crate) const TAG: &str = "c0875924c1c7987947deafd8780acf49";
}

fn key(seed: &str) -> Key {
	Key::generate(&mut rng(seed))
}

/// Builds the CBOR array `[1, nonce, ciphertext]`.
fn envelope_value(nonce: Vec<u8>, ciphertext: Vec<u8>) -> Value {
	Value::Array(vec![
		1_u8.to_cbor(),
		Value::Bytes(nonce),
		Value::Bytes(ciphertext),
	])
}

#[test]
fn the_published_test_vector_passes() {
	let key = Key::from_bytes(hex(vector::KEY).try_into().expect("32 bytes"));
	let associated_data = hex(vector::ASSOCIATED_DATA);
	let mut nonce = FixedBytes::new(hex(vector::NONCE));
	let envelope = Envelope::seal(&key, &hex(vector::PLAINTEXT), &associated_data, &mut nonce);

	let mut ciphertext = hex(vector::CIPHERTEXT);
	ciphertext.extend(hex(vector::TAG));
	assert_eq!(
		cbor::to_vec(&envelope),
		cbor::encode(&envelope_value(hex(vector::NONCE), ciphertext))
	);
	let opened = envelope.open(&key, &associated_data).expect("opens");
	assert_eq!(*opened, hex(vector::PLAINTEXT));
}

#[test]
fn every_envelope_gets_a_new_nonce() {
	let key = key("nonces");
	let mut rng = rng("nonces");
	let first = Envelope::seal(&key, b"same", b"", &mut rng);
	let second = Envelope::seal(&key, b"same", b"", &mut rng);
	assert_ne!(first, second);
	assert_eq!(*first.open(&key, b"").expect("opens"), b"same");
	assert_eq!(*second.open(&key, b"").expect("opens"), b"same");
}

#[test]
fn the_wrong_key_does_not_open_an_envelope() {
	let envelope = Envelope::seal(&key("right"), b"secret", b"record 1", &mut rng("seal"));
	assert_eq!(
		envelope.open(&key("wrong"), b"record 1"),
		Err(CryptoError::Decryption)
	);
}

#[test]
fn other_associated_data_does_not_open_an_envelope() {
	let key = key("associated data");
	let envelope = Envelope::seal(&key, b"secret", b"record 1", &mut rng("seal"));
	assert_eq!(
		envelope.open(&key, b"record 2"),
		Err(CryptoError::Decryption)
	);
}

#[test]
fn no_changed_bit_goes_unnoticed() {
	let key = key("tampering");
	let envelope = Envelope::seal(&key, b"secret", b"record 1", &mut rng("seal"));
	let encoded = cbor::to_vec(&envelope);
	for position in 0..encoded.len() {
		for bit in 0..8 {
			let mut changed = encoded.clone();
			changed[position] ^= 1 << bit;
			if let Ok(changed) = cbor::from_slice::<Envelope>(&changed) {
				assert!(
					changed.open(&key, b"record 1").is_err(),
					"changing bit {bit} of byte {position} went unnoticed"
				);
			}
		}
	}
}

#[test]
fn envelopes_round_trip_through_cbor() {
	let envelope = Envelope::seal(&key("cbor"), b"secret", b"", &mut rng("seal"));
	let encoded = cbor::to_vec(&envelope);
	assert_eq!(cbor::from_slice::<Envelope>(&encoded), Ok(envelope));
}

#[test]
fn malformed_envelopes_are_rejected() {
	let malformed = [
		// An unknown suite.
		Value::Array(vec![
			2_u8.to_cbor(),
			Value::Bytes(vec![0; 24]),
			Value::Bytes(vec![0; 16]),
		]),
		// A nonce of the wrong length.
		envelope_value(vec![0; 12], vec![0; 16]),
		// A ciphertext too short to hold the tag.
		envelope_value(vec![0; 24], vec![0; 15]),
		// Missing and extra items.
		Value::Array(vec![1_u8.to_cbor(), Value::Bytes(vec![0; 24])]),
		Value::Array(vec![
			1_u8.to_cbor(),
			Value::Bytes(vec![0; 24]),
			Value::Bytes(vec![0; 16]),
			Value::Bytes(vec![]),
		]),
		// Not an array of byte strings.
		Value::Array(vec![
			1_u8.to_cbor(),
			Value::Text("nonce".into()),
			Value::Bytes(vec![0; 16]),
		]),
		Value::Bytes(vec![0; 41]),
	];
	for value in malformed {
		assert!(
			cbor::from_slice::<Envelope>(&cbor::encode(&value)).is_err(),
			"{value:?} was accepted"
		);
	}
}

#[test]
fn wrapped_keys_open_with_the_wrapping_key() {
	let space_key = key("space");
	let record_key = key("record");
	let wrapped = Envelope::seal_key(&space_key, &record_key, b"record 1", &mut rng("wrap"));
	let unwrapped = wrapped.open_key(&space_key, b"record 1").expect("opens");
	assert_eq!(unwrapped.expose_bytes(), record_key.expose_bytes());
	assert!(wrapped.open_key(&key("other"), b"record 1").is_err());
}

#[test]
fn an_envelope_that_does_not_hold_a_key_does_not_open_as_one() {
	let key = key("not a key");
	let envelope = Envelope::seal(&key, b"too short to be a key", b"", &mut rng("seal"));
	assert!(envelope.open_key(&key, b"").is_err());
}

#[test]
fn purging_a_key_makes_its_value_unreadable() {
	// A password gets a key of its own, wrapped with a key derived from the space key.
	let space_key = key("space");
	let wrapping_key = space_key.derive("nibkeep 2026-10 test record keys");
	let record_key = key("record");
	let mut rng = rng("purge");
	let password = Envelope::seal(&record_key, b"hunter2", b"record 1", &mut rng);
	let wrapped = Envelope::seal_key(&wrapping_key, &record_key, b"record 1", &mut rng);
	assert_eq!(
		*wrapped
			.open_key(&wrapping_key, b"record 1")
			.and_then(|key| password.open(&key, b"record 1"))
			.expect("readable before the purge"),
		b"hunter2"
	);

	// Purging destroys the wrapped key and the record key everywhere.
	drop(wrapped);
	drop(record_key);

	// What is left, the space key and the keys derived from it, cannot open the password.
	assert!(password.open(&space_key, b"record 1").is_err());
	assert!(password.open(&wrapping_key, b"record 1").is_err());
}
