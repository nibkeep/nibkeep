//! Messages sealed to a public key with HPKE and X-Wing.

use crate::common::{rng, vectors};
use nibkeep_base::cbor::{self, ToCbor, Value};
use nibkeep_crypto::{CryptoError, DecryptionKey, EncryptionKey, Key, SealedMessage};

/// The vector's `info`: ASCII text that happens to spell hexadecimal digits.
const VECTOR_INFO: &str = "4f6465206f6e2061204772656369616e2055726e";

/// Builds the CBOR array `[1, items...]` with byte strings.
fn versioned(items: Vec<Vec<u8>>) -> Vec<u8> {
	let mut array = vec![1_u8.to_cbor()];
	array.extend(items.into_iter().map(Value::Bytes));
	cbor::encode(&Value::Array(array))
}

fn decryption_key(seed: &str) -> DecryptionKey {
	DecryptionKey::generate(&mut rng(seed))
}

#[test]
fn the_published_test_vector_passes() {
	let vector = vectors(include_str!("vectors/hpke-x-wing.txt"));
	assert_eq!(vector["info"], VECTOR_INFO.as_bytes());

	let key = DecryptionKey::from_seed(&vector["skRm"].clone().try_into().expect("32 bytes"));
	assert_eq!(
		cbor::to_vec(&key.encryption_key()),
		versioned(vec![vector["pkRm"].clone()])
	);
	let sealed: SealedMessage = cbor::from_slice(&versioned(vec![
		vector["enc"].clone(),
		vector["ct"].clone(),
	]))
	.expect("decodes");
	let opened = sealed
		.open(&key, VECTOR_INFO, &vector["aad"])
		.expect("opens");
	assert_eq!(*opened, vector["pt"]);
}

#[test]
fn sealed_messages_open_with_the_decryption_key() {
	let recipient = decryption_key("recipient");
	let sealed = SealedMessage::seal(
		&recipient.encryption_key(),
		b"welcome",
		"nibkeep test",
		b"space 1",
		&mut rng("seal"),
	);
	assert_eq!(
		*sealed
			.open(&recipient, "nibkeep test", b"space 1")
			.expect("opens"),
		b"welcome"
	);
	// Another decryption key, context or associated data does not open it.
	assert_eq!(
		sealed.open(&decryption_key("other"), "nibkeep test", b"space 1"),
		Err(CryptoError::Decryption)
	);
	assert_eq!(
		sealed.open(&recipient, "nibkeep other test", b"space 1"),
		Err(CryptoError::Decryption)
	);
	assert_eq!(
		sealed.open(&recipient, "nibkeep test", b"space 2"),
		Err(CryptoError::Decryption)
	);
}

#[test]
fn every_message_gets_a_new_encapsulation() {
	let recipient = decryption_key("recipient").encryption_key();
	let mut rng = rng("seal");
	let first = SealedMessage::seal(&recipient, b"same", "nibkeep test", b"", &mut rng);
	let second = SealedMessage::seal(&recipient, b"same", "nibkeep test", b"", &mut rng);
	assert_ne!(cbor::to_vec(&first), cbor::to_vec(&second));
}

#[test]
fn changed_messages_do_not_open() {
	let recipient = decryption_key("tampering");
	let sealed = SealedMessage::seal(
		&recipient.encryption_key(),
		b"welcome",
		"nibkeep test",
		b"",
		&mut rng("seal"),
	);
	let Value::Array(items) = cbor::decode(&cbor::to_vec(&sealed)).expect("decodes") else {
		panic!("a sealed message is an array");
	};
	let [_, Value::Bytes(enc), Value::Bytes(ciphertext)] = items.as_slice() else {
		panic!("a sealed message is [1, enc, ciphertext]");
	};
	// Decapsulating is slow in debug builds, so the test changes a sample of the bytes:
	// the first and last bytes of both parts of the encapsulated key, the ML-KEM
	// ciphertext (1088 bytes) and the X25519 public key (32 bytes), and of the ciphertext.
	let mut changed_messages = Vec::new();
	for position in [0, 1087, 1088, 1119] {
		let mut changed = enc.clone();
		changed[position] ^= 1;
		changed_messages.push(versioned(vec![changed, ciphertext.clone()]));
	}
	for position in [0, ciphertext.len() - 1] {
		let mut changed = ciphertext.clone();
		changed[position] ^= 1;
		changed_messages.push(versioned(vec![enc.clone(), changed]));
	}
	for (index, changed) in changed_messages.iter().enumerate() {
		let changed: SealedMessage = cbor::from_slice(changed).expect("still well-formed");
		assert_eq!(
			changed.open(&recipient, "nibkeep test", b""),
			Err(CryptoError::Decryption),
			"change {index} went unnoticed"
		);
	}
}

#[test]
fn sealed_keys_open_as_keys() {
	let recipient = decryption_key("recipient");
	let space_key = Key::generate(&mut rng("space"));
	let sealed = SealedMessage::seal_key(
		&recipient.encryption_key(),
		&space_key,
		"nibkeep test",
		b"space 1",
		&mut rng("seal"),
	);
	let opened = sealed
		.open_key(&recipient, "nibkeep test", b"space 1")
		.expect("opens");
	assert_eq!(opened.expose_bytes(), space_key.expose_bytes());

	let not_a_key = SealedMessage::seal(
		&recipient.encryption_key(),
		b"not a key",
		"nibkeep test",
		b"",
		&mut rng("seal"),
	);
	assert!(not_a_key.open_key(&recipient, "nibkeep test", b"").is_err());
}

#[test]
fn decryption_keys_are_their_seeds() {
	let key = decryption_key("seed");
	let copy = DecryptionKey::from_seed(&key.expose_seed());
	assert_eq!(copy.encryption_key(), key.encryption_key());
	assert_ne!(
		decryption_key("other seed").encryption_key(),
		key.encryption_key()
	);
	assert_eq!(format!("{key:?}"), "DecryptionKey(..)");
}

#[test]
fn keys_and_messages_round_trip_through_cbor() {
	let recipient = decryption_key("cbor");
	let encryption_key = recipient.encryption_key();
	assert_eq!(
		cbor::from_slice::<EncryptionKey>(&cbor::to_vec(&encryption_key)),
		Ok(encryption_key.clone())
	);
	let sealed = SealedMessage::seal(
		&encryption_key,
		b"hi",
		"nibkeep test",
		b"",
		&mut rng("seal"),
	);
	assert_eq!(
		cbor::from_slice::<SealedMessage>(&cbor::to_vec(&sealed)),
		Ok(sealed)
	);
}

#[test]
fn malformed_keys_and_messages_are_rejected() {
	let encryption_key = cbor::to_vec(&decryption_key("malformed").encryption_key());
	let key_bytes = encryption_key[encryption_key.len() - 1216..].to_vec();
	let decode_key = |bytes: Vec<u8>| cbor::from_slice::<EncryptionKey>(&versioned(vec![bytes]));
	assert!(decode_key(key_bytes.clone()).is_ok());
	assert!(decode_key(key_bytes[..1215].to_vec()).is_err());
	// An ML-KEM public key holds numbers below 3329; all ones is out of range.
	assert!(decode_key(vec![0xff; 1216]).is_err());

	let decode_message = |items: Vec<Vec<u8>>| cbor::from_slice::<SealedMessage>(&versioned(items));
	assert!(decode_message(vec![vec![0; 1120], vec![0; 16]]).is_ok());
	assert!(decode_message(vec![vec![0; 1119], vec![0; 16]]).is_err());
	assert!(decode_message(vec![vec![0; 1120], vec![0; 15]]).is_err());
	assert!(decode_message(vec![vec![0; 1120]]).is_err());
}
