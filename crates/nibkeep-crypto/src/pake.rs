//! Keys agreed on from a short code: SPAKE2 in the group Ed25519, as Magic Wormhole uses it.

use std::fmt;

use rand_core::CryptoRng;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::{CryptoError, Key};

/// Which side of SPAKE2 a party takes: the two sides blind their messages differently, so
/// the parties must take one each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PakeSide {
	/// Side A.
	A,
	/// Side B.
	B,
}

/// One party's half of SPAKE2, between its own message and the other party's. A wrong
/// password on either side gives the two parties different keys, which only a check of the
/// key afterwards reveals: an attacker gets one guess at the password for each exchange.
pub struct Pake(Spake2<Ed25519Group>);

impl Pake {
	/// Starts SPAKE2 on `side` with the shared `password` and the two sides' identities, and
	/// returns the message to send to the other party.
	#[must_use]
	pub fn start(
		side: PakeSide,
		password: &[u8],
		identity_a: &[u8],
		identity_b: &[u8],
		rng: &mut impl CryptoRng,
	) -> (Self, Vec<u8>) {
		let password = Password::new(password);
		let (a, b) = (Identity::new(identity_a), Identity::new(identity_b));
		let (pake, message) = match side {
			PakeSide::A => Spake2::start_a_with_rng(&password, &a, &b, rng),
			PakeSide::B => Spake2::start_b_with_rng(&password, &a, &b, rng),
		};
		(Self(pake), message)
	}

	/// Finishes SPAKE2 with the other party's message, and returns the key, which is the
	/// other party's only if both used the same password and identities.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Pake`] if the message is not one of the other side's.
	pub fn finish(self, message: &[u8]) -> Result<Key, CryptoError> {
		let key = self.0.finish(message).map_err(|_| CryptoError::Pake)?;
		Key::from_slice(&key).ok_or(CryptoError::Pake)
	}
}

impl fmt::Debug for Pake {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("Pake(..)")
	}
}
