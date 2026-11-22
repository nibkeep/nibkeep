//! Identifiers are random version 4 UUIDs with a single text form.

use std::collections::HashSet;

use nibkeep_base::cbor::{self, Value};
use nibkeep_base::{EntityId, PersonId, SpaceId, StatementId};

/// Deterministic stand-in for random bytes.
fn bytes(seed: u8) -> [u8; 16] {
	std::array::from_fn(|i| {
		seed.wrapping_mul(31)
			.wrapping_add(u8::try_from(i).expect("small"))
	})
}

#[test]
fn new_identifiers_are_version_4_uuids() {
	let id = EntityId::from_random_bytes([0xff; 16]);
	let text = id.to_string();
	assert_eq!(&text[14..15], "4", "version digit in {text}");
	assert!(
		matches!(&text[19..20], "8" | "9" | "a" | "b"),
		"variant digit in {text}"
	);
}

#[test]
fn different_random_bytes_give_different_identifiers() {
	let ids: HashSet<EntityId> = (0..=255)
		.map(|seed| EntityId::from_random_bytes(bytes(seed)))
		.collect();
	assert_eq!(ids.len(), 256);
}

#[test]
fn text_form_is_lowercase_and_hyphenated() {
	let id = SpaceId::from_bytes([
		0x0b, 0x6f, 0x3c, 0x39, 0x8b, 0x4e, 0x4b, 0x8e, 0x9c, 0x2f, 0x5d, 0x0f, 0x6a, 0x7e, 0x9a,
		0x10,
	]);
	assert_eq!(id.to_string(), "0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10");
	assert_eq!(
		"0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10".parse::<SpaceId>(),
		Ok(id)
	);
}

#[test]
fn other_text_forms_are_rejected() {
	for text in [
		"0B6F3C39-8B4E-4B8E-9C2F-5D0F6A7E9A10",
		"0b6f3c398b4e4b8e9c2f5d0f6a7e9a10",
		"{0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10}",
		"urn:uuid:0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10",
		"0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a1",
		"",
	] {
		assert!(text.parse::<PersonId>().is_err(), "{text} was accepted");
	}
}

#[test]
fn identifiers_are_encoded_as_uuid_tags() {
	let id = EntityId::from_random_bytes(bytes(7));
	let encoded = cbor::to_vec(&id);
	assert_eq!(&encoded[..3], &[0xd8, 0x25, 0x50]);
	assert_eq!(&encoded[3..], id.as_bytes());
	assert_eq!(cbor::from_slice::<EntityId>(&encoded), Ok(id));
	assert!(
		cbor::from_slice::<EntityId>(&cbor::encode(&Value::Bytes(id.as_bytes().to_vec()))).is_err()
	);
}

#[test]
fn statement_identifiers_work_like_the_others() {
	let id = StatementId::from_random_bytes(bytes(7));
	assert_eq!(id.to_string().parse::<StatementId>(), Ok(id));
	assert_eq!(cbor::from_slice::<StatementId>(&cbor::to_vec(&id)), Ok(id));
}
