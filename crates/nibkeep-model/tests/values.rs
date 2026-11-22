//! Values carry their kind, and values of kinds from newer versions survive.

mod common;

use common::entity;
use nibkeep_base::Hash;
use nibkeep_base::cbor::{self, Map, ToCbor, Value as Cbor};
use nibkeep_model::{Value, ValueKind};

/// Builds the CBOR array `[kind, payload]`.
fn tagged(kind: u64, payload: Cbor) -> Vec<u8> {
	cbor::encode(&Cbor::Array(vec![kind.to_cbor(), payload]))
}

#[test]
fn each_kind_of_value_has_its_cbor_form() {
	let hash = Hash::of(b"a photo");
	let examples = [
		(Value::Text("hello".into()), tagged(0, Cbor::from("hello"))),
		(Value::Integer(-5), tagged(1, (-5_i64).to_cbor())),
		(Value::Boolean(true), tagged(2, Cbor::from(true))),
		(
			Value::Date("1985-04~".parse().expect("valid")),
			tagged(3, Cbor::from("1985-04~")),
		),
		(Value::Reference(entity(1)), tagged(4, entity(1).to_cbor())),
		(
			Value::Blob(hash),
			tagged(5, Cbor::Bytes(hash.to_multihash())),
		),
	];
	for (value, encoded) in examples {
		assert_eq!(cbor::to_vec(&value), encoded, "{value:?}");
		assert_eq!(cbor::from_slice::<Value>(&encoded), Ok(value));
	}
}

#[test]
fn values_report_their_kind() {
	assert_eq!(Value::Text("x".into()).kind(), ValueKind::Text);
	assert_eq!(Value::Reference(entity(1)).kind(), ValueKind::Reference);
	assert_eq!(Value::Reference(entity(1)).as_reference(), Some(entity(1)));
	assert_eq!(Value::Text("x".into()).as_reference(), None);
}

#[test]
fn values_of_unknown_kinds_survive_unchanged() {
	let mut payload = Map::new();
	payload.insert("lat", 59_i64.to_cbor());
	payload.insert("lon", 18_i64.to_cbor());
	let encoded = tagged(9, Cbor::from(payload.clone()));
	let value: Value = cbor::from_slice(&encoded).expect("decodes");
	let Value::Unknown(unknown) = &value else {
		panic!("an unknown value");
	};
	assert_eq!(unknown.kind(), ValueKind::from_code(9));
	assert_eq!(unknown.payload(), &Cbor::from(payload));
	assert_eq!(value.kind().code(), 9);
	assert_eq!(cbor::to_vec(&value), encoded);
}

#[test]
fn values_this_version_cannot_read_are_kept() {
	// Payloads that do not fit their kind, as a newer version might write them: a date in
	// a newer format, a larger integer, a hash with a new algorithm.
	let unreadable = [
		(0, tagged(0, 5_i64.to_cbor())),
		(1, tagged(1, Cbor::from("five"))),
		(1, tagged(1, (1_u64 << 63).to_cbor())),
		(2, tagged(2, Cbor::from("true"))),
		(3, tagged(3, Cbor::from("2004-?06-11"))),
		(4, tagged(4, Cbor::Bytes(vec![0; 16]))),
		(5, tagged(5, Cbor::Bytes(vec![0x99, 0x01, 0x00]))),
		(0, tagged(0, Cbor::Null)),
	];
	for (kind, encoded) in unreadable {
		let value: Value = cbor::from_slice(&encoded).expect("kept");
		assert!(matches!(value, Value::Unknown(_)), "{value:?}");
		assert_eq!(value.kind(), ValueKind::from_code(kind));
		assert_eq!(cbor::to_vec(&value), encoded);
		// It never equals a value that can be read.
		assert_ne!(value, Value::Text("5".into()));
	}
}

#[test]
fn malformed_values_are_rejected() {
	let malformed = [
		cbor::encode(&Cbor::Array(vec![0_u64.to_cbor()])),
		cbor::encode(&Cbor::Array(vec![(-1_i64).to_cbor(), Cbor::from("x")])),
		cbor::encode(&Cbor::Array(vec![Cbor::from("text"), Cbor::from("x")])),
		cbor::encode(&Cbor::Array(vec![
			0_u64.to_cbor(),
			Cbor::from("a"),
			Cbor::from("b"),
		])),
		cbor::encode(&Cbor::from("hello")),
	];
	for encoded in malformed {
		assert!(
			cbor::from_slice::<Value>(&encoded).is_err(),
			"{encoded:02x?} was accepted"
		);
	}
}
