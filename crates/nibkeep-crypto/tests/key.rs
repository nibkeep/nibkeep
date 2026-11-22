//! Keys: generating them, deriving keys for separate purposes, and keeping them secret.

mod common;

use nibkeep_crypto::Key;

#[test]
fn generated_keys_are_random() {
	let mut rng = common::rng("generated keys");
	let first = Key::generate(&mut rng);
	let second = Key::generate(&mut rng);
	assert_ne!(first.expose_bytes(), second.expose_bytes());
}

#[test]
fn keys_round_trip_through_their_bytes() {
	let key = Key::generate(&mut common::rng("round trip"));
	let copy = Key::from_bytes(*key.expose_bytes());
	assert_eq!(copy.expose_bytes(), key.expose_bytes());
}

#[test]
fn keys_are_the_same_only_when_every_byte_is() {
	let key = Key::from_bytes([1; 32]);
	assert!(key.same_as(&Key::from_bytes([1; 32])));
	let mut last = [1; 32];
	last[31] = 2;
	assert!(!key.same_as(&Key::from_bytes(last)));
	assert!(!key.same_as(&Key::from_bytes([0; 32])));
}

#[test]
fn derived_keys_use_blake3_in_key_derivation_mode() {
	let key = Key::from_bytes([1; 32]);
	let derived = key.derive("nibkeep 2026-10 test");
	assert_eq!(
		derived.expose_bytes(),
		&blake3::derive_key("nibkeep 2026-10 test", &[1; 32])
	);
	// Each purpose gets its own key, unrelated to the original.
	assert_ne!(
		key.derive("nibkeep 2026-10 other test").expose_bytes(),
		derived.expose_bytes()
	);
	assert_ne!(derived.expose_bytes(), key.expose_bytes());
}

#[test]
fn keys_are_never_printed() {
	let key = Key::from_bytes([0xab; 32]);
	let printed = format!("{key:?}");
	assert_eq!(printed, "Key(..)");
}

#[test]
fn a_tag_is_made_only_with_its_key_and_for_its_message() {
	let key = Key::generate(&mut common::rng("tag")).derive("nibkeep test tags");
	let other = Key::generate(&mut common::rng("another tag")).derive("nibkeep test tags");
	let tag = key.tag(b"a change");
	assert!(key.verify_tag(b"a change", &tag));
	assert!(!key.verify_tag(b"a change!", &tag));
	assert!(!other.verify_tag(b"a change", &tag));
	assert!(!key.verify_tag(b"a change", &tag[..31]));
	let mut changed = tag;
	changed[31] ^= 1;
	assert!(!key.verify_tag(b"a change", &changed));
	// The tag is BLAKE3 in keyed mode.
	assert_eq!(
		tag,
		*blake3::keyed_hash(key.expose_bytes(), b"a change").as_bytes()
	);
}

#[test]
fn the_tag_of_a_stored_change_matches_the_specification() {
	// docs/spec/crypto.md, section 7: the storage key whose bytes are 0, 1, …, 31, and the
	// data `nibkeep`.
	let storage = Key::from_bytes(std::array::from_fn(|index| {
		u8::try_from(index).expect("small")
	}));
	let tag = storage
		.derive("nibkeep 2026-10 stored changes")
		.tag(b"nibkeep");
	assert_eq!(
		tag.to_vec(),
		common::hex("8f044fed0112e471ec8c97afe530b2244760017541aeec81e9976ccc89b2dae0")
	);
}
