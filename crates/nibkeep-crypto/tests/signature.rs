//! Signatures with Ed25519, over a context and a message.

mod common;

use common::{hex, rng};
use nibkeep_base::cbor::{self, ToCbor, Value};
use nibkeep_crypto::{CryptoError, Signature, SigningKey, VerifyingKey};

/// Secret and public keys from RFC 8032, section 7.1, tests 1 to 3.
const RFC_8032: [(&str, &str); 3] = [
	(
		"9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
		"d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
	),
	(
		"4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
		"3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
	),
	(
		"c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
		"fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
	),
];

fn bytes32(text: &str) -> [u8; 32] {
	hex(text).try_into().expect("32 bytes")
}

/// Builds the CBOR array `[suite, bytes]`.
fn versioned(suite: u8, bytes: Vec<u8>) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![suite.to_cbor(), Value::Bytes(bytes)]))
}

#[test]
fn verifying_keys_match_rfc_8032() {
	for (secret, public) in RFC_8032 {
		let key = SigningKey::from_bytes(&bytes32(secret));
		assert_eq!(key.verifying_key().as_bytes(), &bytes32(public));
		assert_eq!(
			cbor::to_vec(&key.verifying_key()),
			versioned(1, hex(public))
		);
	}
}

#[test]
fn a_signature_is_plain_ed25519_over_the_context_and_the_message() {
	let (secret, public) = RFC_8032[0];
	let signature = SigningKey::from_bytes(&bytes32(secret)).sign("nibkeep test", b"hello");
	let signed = cbor::encode(&Value::Array(vec![
		Value::Text("nibkeep test".into()),
		Value::Bytes(b"hello".to_vec()),
	]));
	let independent = ed25519_dalek::VerifyingKey::from_bytes(&bytes32(public)).expect("valid");
	independent
		.verify_strict(
			&signed,
			&ed25519_dalek::Signature::from_bytes(signature.as_bytes()),
		)
		.expect("a valid Ed25519 signature");
	assert_eq!(
		cbor::to_vec(&signature),
		versioned(1, signature.as_bytes().to_vec())
	);
}

#[test]
fn signatures_verify_only_for_the_same_key_context_and_message() {
	let key = SigningKey::generate(&mut rng("signing"));
	let verifying_key = key.verifying_key();
	let signature = key.sign("nibkeep test", b"hello");
	assert_eq!(
		verifying_key.verify("nibkeep test", b"hello", &signature),
		Ok(())
	);
	assert_eq!(
		verifying_key.verify("nibkeep test", b"hello!", &signature),
		Err(CryptoError::InvalidSignature)
	);
	assert_eq!(
		verifying_key.verify("nibkeep other test", b"hello", &signature),
		Err(CryptoError::InvalidSignature)
	);
	let other = SigningKey::generate(&mut rng("other")).verifying_key();
	assert_eq!(
		other.verify("nibkeep test", b"hello", &signature),
		Err(CryptoError::InvalidSignature)
	);
}

#[test]
fn signatures_with_a_non_canonical_scalar_are_rejected() {
	let key = SigningKey::generate(&mut rng("malleability"));
	let signature = key.sign("nibkeep test", b"hello");
	// Adding the group order L to S gives another encoding of the same signature, which
	// lax verification accepts. L = 2^252 + 27742317777372353535851937790883648493.
	let order = hex("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
	let mut bytes = signature.as_bytes().to_vec();
	let mut carry = 0;
	for (byte, order_byte) in bytes[32..].iter_mut().zip(order) {
		let sum = u16::from(*byte) + u16::from(order_byte) + carry;
		*byte = sum.to_le_bytes()[0];
		carry = sum >> 8;
	}
	assert_eq!(carry, 0, "S + L fits in 32 bytes");
	let changed: Signature = cbor::from_slice(&versioned(1, bytes)).expect("decodes");
	assert_eq!(
		key.verifying_key()
			.verify("nibkeep test", b"hello", &changed),
		Err(CryptoError::InvalidSignature)
	);
}

#[test]
fn small_order_verifying_keys_are_rejected() {
	// The neutral element, and a point of order 4.
	let mut neutral = vec![0; 32];
	neutral[0] = 1;
	for key in [neutral, vec![0; 32]] {
		assert!(cbor::from_slice::<VerifyingKey>(&versioned(1, key)).is_err());
	}
}

#[test]
fn non_canonical_verifying_keys_are_rejected() {
	// A y-coordinate y + p, where p = 2^255 - 19, names the same point as y, and lax
	// decoders accept it. Only y below 19 has such a second encoding below 2^255.
	let mut found = false;
	for y in 2..19_u8 {
		let mut canonical = [0; 32];
		canonical[0] = y;
		match ed25519_dalek::VerifyingKey::from_bytes(&canonical) {
			Ok(point) if !point.is_weak() => {}
			_ => continue,
		}
		found = true;
		let mut non_canonical = [0xff; 32];
		non_canonical[0] = 0xed + y;
		non_canonical[31] = 0x7f;
		assert!(
			ed25519_dalek::VerifyingKey::from_bytes(&non_canonical).is_ok(),
			"the lax decoder accepts y + p"
		);
		// The canonical encoding is remembered once decoded; the other one still fails,
		// also when decoded again.
		for _ in 0..2 {
			assert!(cbor::from_slice::<VerifyingKey>(&versioned(1, canonical.to_vec())).is_ok());
			assert!(
				cbor::from_slice::<VerifyingKey>(&versioned(1, non_canonical.to_vec())).is_err()
			);
		}
	}
	assert!(found, "some y below 19 is on the curve");
}

#[test]
fn keys_decode_the_same_however_often_and_however_many() {
	// More keys than a thread remembers, decoded three times over, and checked each time.
	let keys: Vec<VerifyingKey> = (0..40)
		.map(|n| SigningKey::generate(&mut rng(&format!("key {n}"))).verifying_key())
		.collect();
	for _ in 0..3 {
		for key in &keys {
			assert_eq!(
				cbor::from_slice::<VerifyingKey>(&cbor::to_vec(key)).as_ref(),
				Ok(key)
			);
		}
	}
	let mut neutral = vec![0; 32];
	neutral[0] = 1;
	for _ in 0..2 {
		assert!(cbor::from_slice::<VerifyingKey>(&versioned(1, neutral.clone())).is_err());
	}
}

#[test]
fn keys_and_signatures_round_trip_through_cbor() {
	let key = SigningKey::generate(&mut rng("cbor"));
	let verifying_key = key.verifying_key();
	let signature = key.sign("nibkeep test", b"hello");
	assert_eq!(
		cbor::from_slice::<VerifyingKey>(&cbor::to_vec(&verifying_key)),
		Ok(verifying_key)
	);
	assert_eq!(
		cbor::from_slice::<Signature>(&cbor::to_vec(&signature)),
		Ok(signature)
	);
	let copy = SigningKey::from_bytes(key.expose_bytes());
	assert_eq!(copy.verifying_key(), verifying_key);
}

#[test]
fn malformed_keys_and_signatures_are_rejected() {
	let public = hex(RFC_8032[0].1);
	assert!(cbor::from_slice::<VerifyingKey>(&versioned(2, public.clone())).is_err());
	assert!(cbor::from_slice::<VerifyingKey>(&versioned(1, public[..31].to_vec())).is_err());
	assert!(cbor::from_slice::<Signature>(&versioned(2, vec![0; 64])).is_err());
	assert!(cbor::from_slice::<Signature>(&versioned(1, vec![0; 63])).is_err());
}

#[test]
fn signing_keys_are_never_printed() {
	let key = SigningKey::from_bytes(&bytes32(RFC_8032[0].0));
	let printed = format!("{key:?}");
	assert!(!printed.contains("9d61"), "{printed}");
	assert!(printed.contains("d75a9801"), "{printed}");
}
