//! Properties that must hold for every value and every input.

use nibkeep_base::cbor::{Integer, Map, MapKey, Value, decode, encode, encode_into, encoded_len};
use proptest::prelude::*;

fn integer() -> impl Strategy<Value = Integer> {
	prop_oneof![
		(-30i128..30).prop_map(|value| Integer::new(value).expect("in range")),
		any::<u64>().prop_map(Integer::from),
		any::<u64>()
			.prop_map(|magnitude| Integer::new(-1 - i128::from(magnitude)).expect("in range")),
	]
}

fn map_key() -> impl Strategy<Value = MapKey> {
	prop_oneof![
		integer().prop_map(MapKey::Integer),
		"[a-z]{0,6}".prop_map(MapKey::Text),
	]
}

fn value() -> impl Strategy<Value = Value> {
	let leaf = prop_oneof![
		integer().prop_map(Value::Integer),
		prop::collection::vec(any::<u8>(), 0..40).prop_map(Value::Bytes),
		any::<String>().prop_map(Value::Text),
		any::<[u8; 16]>().prop_map(Value::Uuid),
		(integer(), integer())
			.prop_map(|(exponent, mantissa)| Value::Decimal { exponent, mantissa }),
		any::<bool>().prop_map(Value::Bool),
		Just(Value::Null),
	];
	leaf.prop_recursive(4, 64, 8, |inner| {
		prop_oneof![
			prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
			prop::collection::vec((map_key(), inner), 0..8)
				.prop_map(|entries| Value::Map(entries.into_iter().collect::<Map>())),
		]
	})
}

/// Converts what the independent ciborium decoder read back into a `Value`, checking on
/// the way that map keys arrive in canonical order.
fn from_ciborium(value: ciborium::Value) -> Value {
	match value {
		ciborium::Value::Integer(integer) => {
			Value::Integer(Integer::new(i128::from(integer)).expect("in range"))
		}
		ciborium::Value::Bytes(bytes) => Value::Bytes(bytes),
		ciborium::Value::Text(text) => Value::Text(text),
		ciborium::Value::Bool(flag) => Value::Bool(flag),
		ciborium::Value::Null => Value::Null,
		ciborium::Value::Array(items) => {
			Value::Array(items.into_iter().map(from_ciborium).collect())
		}
		ciborium::Value::Map(entries) => {
			let keys: Vec<MapKey> = entries
				.iter()
				.map(|(key, _)| match from_ciborium(key.clone()) {
					Value::Integer(integer) => MapKey::Integer(integer),
					Value::Text(text) => MapKey::Text(text),
					other => panic!("unexpected map key {other:?}"),
				})
				.collect();
			assert!(
				keys.windows(2).all(|pair| pair[0] < pair[1]),
				"keys in canonical order"
			);
			let values = entries.into_iter().map(|(_, value)| from_ciborium(value));
			Value::Map(keys.into_iter().zip(values).collect())
		}
		ciborium::Value::Tag(37, content) => match *content {
			ciborium::Value::Bytes(bytes) => Value::Uuid(bytes.try_into().expect("16 bytes")),
			other => panic!("unexpected UUID content {other:?}"),
		},
		ciborium::Value::Tag(4, content) => match *content {
			ciborium::Value::Array(items) => match items.as_slice() {
				[
					ciborium::Value::Integer(exponent),
					ciborium::Value::Integer(mantissa),
				] => Value::Decimal {
					exponent: Integer::new(i128::from(*exponent)).expect("in range"),
					mantissa: Integer::new(i128::from(*mantissa)).expect("in range"),
				},
				other => panic!("unexpected decimal content {other:?}"),
			},
			other => panic!("unexpected decimal content {other:?}"),
		},
		other => panic!("unexpected value {other:?}"),
	}
}

proptest! {
	#[test]
	fn the_encoded_length_is_exact(value in value()) {
		let encoded = encode(&value);
		prop_assert_eq!(encoded_len(&value), encoded.len());
		let mut appended = vec![0xff];
		encode_into(&value, &mut appended);
		prop_assert_eq!(&appended[1..], encoded.as_slice());
	}

	#[test]
	fn every_value_survives_encoding_and_decoding(value in value()) {
		prop_assert_eq!(decode(&encode(&value)).expect("canonical bytes decode"), value);
	}

	#[test]
	fn an_independent_decoder_reads_the_same_value(value in value()) {
		let bytes = encode(&value);
		let read: ciborium::Value = ciborium::from_reader(bytes.as_slice()).expect("valid CBOR");
		prop_assert_eq!(from_ciborium(read), value);
	}

	#[test]
	fn accepted_input_is_exactly_the_canonical_encoding(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
		if let Ok(value) = decode(&bytes) {
			prop_assert_eq!(encode(&value), bytes);
		}
	}

	#[test]
	fn damaged_encodings_are_rejected_or_canonical(
		value in value(),
		position in any::<prop::sample::Index>(),
		replacement in any::<u8>(),
	) {
		let mut bytes = encode(&value);
		let index = position.index(bytes.len());
		bytes[index] = replacement;
		if let Ok(decoded) = decode(&bytes) {
			prop_assert_eq!(encode(&decoded), bytes);
		}
	}
}
