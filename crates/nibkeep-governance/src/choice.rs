//! A device's choice of a space: whether it holds the space, signed by its own key and
//! numbered, so that no other device takes a space from it or gives it one it declined
//! ([technical design](../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
//!
//! A device states its choice on its own record in the personal space, where its user's
//! other devices read it, and on its record in the space, derived from the space and its
//! key, where any replica of its member may carry it. Of a key's choices of a space, the
//! one with the highest number stands ([Log](../../../docs/spec/log.md#8-which-changes-count)).

use base64ct::{Base64UrlUnpadded, Encoding};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{Signature, SigningKey, VerifyingKey};

use crate::replica_record;

/// The context of a choice's signature.
const CHOICES: &str = "nibkeep space choice";

/// A device's choice of a space, signed by its key: the space, the device's record there,
/// derived from the space and the key, the key, the number of the choice, counting the
/// device's choices of that space from 1, and whether it holds the space. In text, the
/// canonical encoding of `[space, record, key, number, holds, signature]` in base64url
/// without padding, where the signature, with the context `nibkeep space choice`, is of the
/// canonical encoding of `[space, record, key, number, holds]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
	/// The space chosen.
	pub space: SpaceId,
	/// The device's record in the space, derived from the space and the key.
	pub record: EntityId,
	/// The device's verifying key.
	pub key: VerifyingKey,
	/// The number of the choice: of a key's choices of a space, the highest stands.
	pub number: u64,
	/// Whether the device holds the space.
	pub holds: bool,
	/// The key's signature of the rest.
	pub signature: Signature,
}

impl Choice {
	/// The choice numbered `number` of the device whose signing key is `signing`, of
	/// `space`, on the record derived from them.
	#[must_use]
	pub fn sign(signing: &SigningKey, space: SpaceId, number: u64, holds: bool) -> Self {
		let key = signing.verifying_key();
		let record = replica_record(space, &key);
		let signature = signing.sign(CHOICES, &signed(space, record, &key, number, holds));
		Self {
			space,
			record,
			key,
			number,
			holds,
			signature,
		}
	}

	/// Whether the key signed it, and its record is the one derived from its space and key.
	#[must_use]
	pub fn verifies(&self) -> bool {
		self.record == replica_record(self.space, &self.key)
			&& self
				.key
				.verify(
					CHOICES,
					&signed(self.space, self.record, &self.key, self.number, self.holds),
					&self.signature,
				)
				.is_ok()
	}

	/// Its text form.
	#[must_use]
	pub fn to_text(&self) -> String {
		Base64UrlUnpadded::encode_string(&cbor::to_vec(self))
	}

	/// The choice that a text holds, if it holds one, whether it verifies or not.
	#[must_use]
	pub fn from_text(text: &str) -> Option<Self> {
		let bytes = Base64UrlUnpadded::decode_vec(text).ok()?;
		cbor::from_slice(&bytes).ok()
	}
}

/// What a choice's key signs.
fn signed(
	space: SpaceId,
	record: EntityId,
	key: &VerifyingKey,
	number: u64,
	holds: bool,
) -> Vec<u8> {
	cbor::to_vec(&Cbor::Array(vec![
		space.to_cbor(),
		record.to_cbor(),
		key.to_cbor(),
		number.to_cbor(),
		Cbor::Bool(holds),
	]))
}

impl ToCbor for Choice {
	fn to_cbor(&self) -> Cbor {
		Cbor::Array(vec![
			self.space.to_cbor(),
			self.record.to_cbor(),
			self.key.to_cbor(),
			self.number.to_cbor(),
			Cbor::Bool(self.holds),
			self.signature.to_cbor(),
		])
	}
}

impl FromCbor for Choice {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a choice: [space, record, key, number, holds, signature]";
		let Cbor::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [space, record, key, number, holds, signature] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let Cbor::Bool(holds) = holds else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		Ok(Self {
			space: SpaceId::from_cbor(space)?,
			record: EntityId::from_cbor(record)?,
			key: VerifyingKey::from_cbor(key)?,
			number: u64::from_cbor(number)?,
			holds: *holds,
			signature: Signature::from_cbor(signature)?,
		})
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::SigningKey;
	use nibkeep_testkit::TestRng;

	use super::*;
	use crate::replica_record;

	#[test]
	fn a_choice_verifies_and_survives_its_text_form() {
		let mut rng = TestRng::new("a choice");
		let signing = SigningKey::generate(&mut rng);
		let choice = Choice::sign(&signing, rng.space_id(), 3, false);
		assert!(choice.verifies());
		let again = Choice::from_text(&choice.to_text()).expect("reads");
		assert_eq!(again, choice);
		assert!(again.verifies());
		assert_eq!((again.number, again.holds), (3, false));
	}

	#[test]
	fn a_choice_is_on_the_record_derived_from_its_space_and_key() {
		let mut rng = TestRng::new("a choice's record");
		let signing = SigningKey::generate(&mut rng);
		let space = rng.space_id();
		let choice = Choice::sign(&signing, space, 1, true);
		assert_eq!(
			choice.record,
			replica_record(space, &signing.verifying_key())
		);
		assert_eq!(choice.key, signing.verifying_key());
	}

	#[test]
	fn a_choice_changed_after_signing_does_not_verify() {
		let mut rng = TestRng::new("a changed choice");
		let signing = SigningKey::generate(&mut rng);
		let choice = Choice::sign(&signing, rng.space_id(), 2, false);
		let mut number = choice.clone();
		number.number = 3;
		assert!(!number.verifies());
		let mut holds = choice.clone();
		holds.holds = true;
		assert!(!holds.verifies());
		let mut space = choice.clone();
		space.space = rng.space_id();
		assert!(!space.verifies());
		let mut record = choice.clone();
		record.record = rng.entity_id();
		assert!(!record.verifies());
		// Another key's choice, put on the first key's record, does not verify.
		let other = SigningKey::generate(&mut rng);
		let mut borrowed = Choice::sign(&other, choice.space, 2, false);
		borrowed.record = choice.record;
		assert!(!borrowed.verifies());
		// Nor does the first key's signature with another key.
		let mut moved = choice;
		moved.key = other.verifying_key();
		assert!(!moved.verifies());
	}

	#[test]
	fn a_text_that_is_no_choice_reads_as_none() {
		assert!(Choice::from_text("not base64!").is_none());
		assert!(Choice::from_text("").is_none());
	}
}
