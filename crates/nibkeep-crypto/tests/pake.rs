//! Keys agreed on from a short code with SPAKE2.

use crate::common::rng;
use nibkeep_crypto::{CryptoError, Key, Pake, PakeSide};

/// Runs SPAKE2 between side A with one password and side B with another, and returns the
/// keys they get.
fn exchange(password_a: &[u8], password_b: &[u8]) -> (Key, Key) {
	let mut rng = rng(&format!("{password_a:?} {password_b:?}"));
	let (a, to_b) = Pake::start(PakeSide::A, password_a, b"a", b"b", &mut rng);
	let (b, to_a) = Pake::start(PakeSide::B, password_b, b"a", b"b", &mut rng);
	(
		a.finish(&to_a).expect("B's message"),
		b.finish(&to_b).expect("A's message"),
	)
}

#[test]
fn the_same_password_gives_both_sides_one_key() {
	let (a, b) = exchange(b"37482177", b"37482177");
	assert!(a.same_as(&b));
}

#[test]
fn another_password_gives_other_keys() {
	let (a, b) = exchange(b"37482177", b"37482178");
	assert!(!a.same_as(&b));
}

#[test]
fn other_identities_give_other_keys() {
	let mut rng = rng("identities");
	let (a, to_b) = Pake::start(PakeSide::A, b"code", b"a", b"b", &mut rng);
	let (b, to_a) = Pake::start(PakeSide::B, b"code", b"a", b"c", &mut rng);
	let (a, b) = (a.finish(&to_a).expect("B's"), b.finish(&to_b).expect("A's"));
	assert!(!a.same_as(&b));
}

#[test]
fn a_message_of_the_same_side_is_refused() {
	let mut rng = rng("same side");
	let (a, _) = Pake::start(PakeSide::A, b"code", b"a", b"b", &mut rng);
	let (_, also_a) = Pake::start(PakeSide::A, b"code", b"a", b"b", &mut rng);
	assert_eq!(a.finish(&also_a).err(), Some(CryptoError::Pake));
}

#[test]
fn a_message_of_another_length_is_refused() {
	let mut rng = rng("length");
	let (a, _) = Pake::start(PakeSide::A, b"code", b"a", b"b", &mut rng);
	assert_eq!(a.finish(b"B").err(), Some(CryptoError::Pake));
}
