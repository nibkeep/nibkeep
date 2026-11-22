//! What the compartments of a key file hold.

use std::collections::BTreeMap;

use nibkeep_base::SpaceId;
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_crypto::{DecryptionKey, Key, SigningKey};
use zeroize::{Zeroize, Zeroizing};

use crate::KeysError;

/// The suite of the contents in version 1 of the specification.
const SUITE: u8 = 1;

/// The keys of spaces, by space and epoch. A space without keys is not saved.
pub type SpaceKeyMap = BTreeMap<SpaceId, BTreeMap<u64, Key>>;

/// The keys that a replica unlocks when the app starts.
#[derive(Clone, Debug)]
pub struct ReplicaKeys {
	/// The key the replica signs its changes with.
	pub signing: SigningKey,
	/// The key that opens what others seal to the replica.
	pub decryption: DecryptionKey,
	/// The key the replica tags what it stores with, so that it knows what it reads back
	/// is what it wrote. It never leaves the replica.
	pub storage: Key,
	/// The keys of the spaces the replica holds.
	pub spaces: SpaceKeyMap,
}

/// The secrets keys of the spaces whose secrets the replica holds, unlocked only after the
/// person confirms, and the key that opens the secrets keys that other replicas give it.
#[derive(Clone, Debug)]
pub struct SecretsKeys {
	/// The secrets keys, by space and epoch.
	pub spaces: SpaceKeyMap,
	/// The replica's secrets decryption key, which opens what is sealed to its secrets
	/// encryption key.
	pub decryption: DecryptionKey,
}

impl ReplicaKeys {
	/// The contents of the replica compartment, in canonical CBOR.
	pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
		encode(Value::Array(vec![
			SUITE.to_cbor(),
			Value::Bytes(self.signing.expose_bytes().to_vec()),
			Value::Bytes(self.decryption.expose_seed().to_vec()),
			Value::Bytes(self.storage.expose_bytes().to_vec()),
			spaces_to_cbor(&self.spaces),
		]))
	}

	/// Reads the contents of the replica compartment.
	pub(crate) fn decode(bytes: &[u8]) -> Result<Self, KeysError> {
		decode(bytes, |value| {
			let [suite, signing, decryption, storage, spaces] = suite_items(value)?;
			check_suite(suite)?;
			let signing = with_key_bytes(signing, SigningKey::from_bytes)?;
			let decryption = with_key_bytes(decryption, DecryptionKey::from_seed)?;
			let Value::Bytes(storage) = storage else {
				return Err(damaged(&"a key is not a byte string"));
			};
			let storage =
				Key::from_slice(storage).ok_or_else(|| damaged(&"a key does not have 32 bytes"))?;
			Ok(Self {
				signing,
				decryption,
				storage,
				spaces: spaces_from_cbor(spaces)?,
			})
		})
	}
}

impl SecretsKeys {
	/// The contents of the secrets compartment, in canonical CBOR:
	/// `[1, spaces, decryption key]`.
	pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
		encode(Value::Array(vec![
			SUITE.to_cbor(),
			spaces_to_cbor(&self.spaces),
			Value::Bytes(self.decryption.expose_seed().to_vec()),
		]))
	}

	/// Reads the contents of the secrets compartment.
	pub(crate) fn decode(bytes: &[u8]) -> Result<Self, KeysError> {
		decode(bytes, |value| {
			let Value::Array(items) = value else {
				return Err(damaged(&"the contents are not an array"));
			};
			let [suite, spaces, decryption] = items.as_slice() else {
				return Err(damaged(&"the contents have the wrong number of items"));
			};
			check_suite(suite)?;
			Ok(Self {
				spaces: spaces_from_cbor(spaces)?,
				decryption: with_key_bytes(decryption, DecryptionKey::from_seed)?,
			})
		})
	}
}

/// Encodes contents into a buffer that is never reallocated, so that no copy of the keys
/// is left in memory that was freed, and wipes the copies of keys in the value.
fn encode(mut value: Value) -> Zeroizing<Vec<u8>> {
	let mut encoded = Zeroizing::new(Vec::with_capacity(cbor::encoded_len(&value)));
	cbor::encode_into(&value, &mut encoded);
	wipe(&mut value);
	encoded
}

/// Decodes contents and wipes the copies of keys in the decoded value.
fn decode<T>(
	bytes: &[u8],
	read: impl FnOnce(&Value) -> Result<T, KeysError>,
) -> Result<T, KeysError> {
	let mut value = cbor::decode(bytes).map_err(|error| damaged(&error))?;
	let result = read(&value);
	wipe(&mut value);
	result
}

/// Overwrites the byte strings of a value, where keys are.
fn wipe(value: &mut Value) {
	match value {
		Value::Bytes(bytes) => bytes.zeroize(),
		Value::Array(items) => items.iter_mut().for_each(wipe),
		_ => {}
	}
}

fn damaged(error: &impl ToString) -> KeysError {
	KeysError::Damaged(error.to_string())
}

fn suite_items<const N: usize>(value: &Value) -> Result<&[Value; N], KeysError> {
	let Value::Array(items) = value else {
		return Err(damaged(&"the contents are not an array"));
	};
	items
		.as_slice()
		.try_into()
		.map_err(|_| damaged(&"the contents have the wrong number of items"))
}

fn check_suite(suite: &Value) -> Result<(), KeysError> {
	if u8::from_cbor(suite) == Ok(SUITE) {
		Ok(())
	} else {
		Err(damaged(&"the contents have an unknown suite"))
	}
}

/// Hands 32 bytes of a key to `make`, and wipes the copy afterwards.
fn with_key_bytes<T>(value: &Value, make: impl FnOnce(&[u8; 32]) -> T) -> Result<T, KeysError> {
	let Value::Bytes(bytes) = value else {
		return Err(damaged(&"a key is not a byte string"));
	};
	let mut key: [u8; 32] = bytes
		.as_slice()
		.try_into()
		.map_err(|_| damaged(&"a key does not have 32 bytes"))?;
	let result = make(&key);
	key.zeroize();
	Ok(result)
}

fn spaces_to_cbor(spaces: &SpaceKeyMap) -> Value {
	Value::Array(
		spaces
			.iter()
			.filter(|(_, keys)| !keys.is_empty())
			.map(|(space, keys)| {
				let keys = keys
					.iter()
					.map(|(epoch, key)| {
						Value::Array(vec![
							epoch.to_cbor(),
							Value::Bytes(key.expose_bytes().to_vec()),
						])
					})
					.collect();
				Value::Array(vec![space.to_cbor(), Value::Array(keys)])
			})
			.collect(),
	)
}

fn spaces_from_cbor(value: &Value) -> Result<SpaceKeyMap, KeysError> {
	let Value::Array(spaces) = value else {
		return Err(damaged(&"the spaces are not an array"));
	};
	let mut map = SpaceKeyMap::new();
	for space in spaces {
		let [space, keys] = suite_items(space)?;
		let space = SpaceId::from_cbor(space).map_err(|error| damaged(&error))?;
		if map.last_key_value().is_some_and(|(&last, _)| last >= space) {
			return Err(damaged(&"the spaces are not in order"));
		}
		let Value::Array(keys) = keys else {
			return Err(damaged(&"the keys of a space are not an array"));
		};
		if keys.is_empty() {
			return Err(damaged(&"a space has no keys"));
		}
		let mut by_epoch = BTreeMap::new();
		for key in keys {
			let [epoch, key] = suite_items(key)?;
			let epoch = u64::from_cbor(epoch).map_err(|error| damaged(&error))?;
			if by_epoch
				.last_key_value()
				.is_some_and(|(&last, _)| last >= epoch)
			{
				return Err(damaged(&"the keys of a space are not in order"));
			}
			let Value::Bytes(key) = key else {
				return Err(damaged(&"a key is not a byte string"));
			};
			let key =
				Key::from_slice(key).ok_or_else(|| damaged(&"a key does not have 32 bytes"))?;
			by_epoch.insert(epoch, key);
		}
		map.insert(space, by_epoch);
	}
	Ok(map)
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	fn key(epoch: u64) -> Value {
		Value::Array(vec![epoch.to_cbor(), Value::Bytes(vec![7; 32])])
	}

	fn space(id: SpaceId, keys: Vec<Value>) -> Value {
		Value::Array(vec![id.to_cbor(), Value::Array(keys)])
	}

	fn secrets(spaces: Vec<Value>) -> Vec<u8> {
		cbor::encode(&Value::Array(vec![
			SUITE.to_cbor(),
			Value::Array(spaces),
			Value::Bytes(vec![9; 32]),
		]))
	}

	#[test]
	fn every_space_has_keys_and_everything_is_in_order() {
		let mut rng = TestRng::new("contents");
		let (first, second) = (rng.space_id(), rng.space_id());
		let (a, b) = (first.min(second), first.max(second));

		let good = secrets(vec![space(a, vec![key(0)]), space(b, vec![key(0), key(1)])]);
		let decoded = SecretsKeys::decode(&good).expect("decodes");
		assert_eq!(
			decoded.spaces[&b].keys().copied().collect::<Vec<_>>(),
			[0, 1]
		);

		for bad in [
			vec![space(a, vec![])],
			vec![space(b, vec![key(0)]), space(a, vec![key(0)])],
			vec![space(a, vec![key(0)]), space(a, vec![key(1)])],
			vec![space(a, vec![key(1), key(0)])],
			vec![space(a, vec![key(0), key(0)])],
		] {
			assert!(matches!(
				SecretsKeys::decode(&secrets(bad)),
				Err(KeysError::Damaged(_))
			));
		}

		let empty = SecretsKeys {
			spaces: SpaceKeyMap::from([(a, BTreeMap::new())]),
			decryption: DecryptionKey::generate(&mut rng),
		};
		let decoded = SecretsKeys::decode(&empty.encode()).expect("decodes");
		assert!(
			decoded.spaces.is_empty(),
			"a space without keys is not saved"
		);
	}

	#[test]
	fn a_compartment_keeps_its_decryption_key_and_one_without_is_damaged() {
		let mut rng = TestRng::new("secrets decryption");
		let space_id = rng.space_id();
		let decryption = DecryptionKey::generate(&mut rng);
		let asking = SecretsKeys {
			spaces: SpaceKeyMap::new(),
			decryption: decryption.clone(),
		};
		let decoded = SecretsKeys::decode(&asking.encode()).expect("decodes");
		assert!(decoded.spaces.is_empty(), "it holds no secrets keys yet");
		assert_eq!(
			decoded.decryption.encryption_key(),
			decryption.encryption_key()
		);
		let without = cbor::encode(&Value::Array(vec![
			SUITE.to_cbor(),
			Value::Array(vec![space(space_id, vec![key(0)])]),
		]));
		assert!(matches!(
			SecretsKeys::decode(&without),
			Err(KeysError::Damaged(_))
		));
	}
}
