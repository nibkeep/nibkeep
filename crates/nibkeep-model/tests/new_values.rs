//! Decimals, quantities and names (`docs/spec/model.md`, section 2): their CBOR and text
//! forms, and what is kept when this version cannot read one.

use nibkeep_base::cbor::{self, Integer, Map, ToCbor, Value as Cbor};
use nibkeep_model::{Decimal, Name, Quantity, Value, ValueKind};

/// Builds the CBOR array `[kind, payload]`.
fn tagged(kind: u64, payload: Cbor) -> Vec<u8> {
	cbor::encode(&Cbor::Array(vec![kind.to_cbor(), payload]))
}

fn cbor_decimal(mantissa: i128, exponent: i128) -> Cbor {
	Cbor::Decimal {
		exponent: Integer::new(exponent).expect("in range"),
		mantissa: Integer::new(mantissa).expect("in range"),
	}
}

fn map(pairs: &[(&str, Cbor)]) -> Map {
	let mut map = Map::new();
	for (key, value) in pairs {
		map.insert(*key, value.clone());
	}
	map
}

fn decimal(text: &str) -> Decimal {
	text.parse().expect("a decimal")
}

#[test]
fn a_decimal_keeps_its_precision() {
	for (text, mantissa, exponent) in [
		("1.5", 15, -1),
		("1.50", 150, -2),
		("1500", 1500, 0),
		("15e2", 15, 2),
		("-0.005", -5, -3),
		("0", 0, 0),
		("0.0", 0, -1),
	] {
		let parsed = decimal(text);
		assert_eq!(
			(parsed.mantissa(), parsed.exponent()),
			(mantissa, exponent),
			"{text}"
		);
		assert_eq!(parsed.to_string(), text);
		let value = Value::Decimal(parsed);
		let encoded = tagged(6, cbor_decimal(mantissa, i128::from(exponent)));
		assert_eq!(cbor::to_vec(&value), encoded, "{text}");
		assert_eq!(cbor::from_slice::<Value>(&encoded), Ok(value));
	}
	assert_ne!(decimal("1.5"), decimal("1.50"));
}

#[test]
fn a_decimal_has_one_text_form() {
	for text in [
		"", "1.", ".5", "+1", "1,5", "1e-2", "1.5e2", "1e", "e2", "01", "-", "1 000",
	] {
		assert!(text.parse::<Decimal>().is_err(), "{text:?}");
	}
}

#[test]
fn a_decimal_with_a_huge_exponent_is_kept_unread() {
	for exponent in [-1001, 1001] {
		let encoded = tagged(6, cbor_decimal(15, exponent));
		let value: Value = cbor::from_slice(&encoded).expect("decodes");
		assert!(matches!(value, Value::Unknown(_)), "{exponent}");
		assert_eq!(value.kind(), ValueKind::Decimal);
		assert_eq!(cbor::to_vec(&value), encoded);
	}
	assert!(Decimal::new(15, -1000).is_some());
	assert!(Decimal::new(15, 1001).is_none());
}

#[test]
fn a_quantity_is_a_decimal_with_its_unit() {
	let quantity: Quantity = "72.3 kg".parse().expect("a quantity");
	assert_eq!(quantity.number(), decimal("72.3"));
	assert_eq!(quantity.unit(), "kg");
	assert_eq!(quantity.to_string(), "72.3 kg");
	let value = Value::Quantity(quantity);
	let encoded = tagged(
		7,
		Cbor::Array(vec![cbor_decimal(723, -1), Cbor::from("kg")]),
	);
	assert_eq!(cbor::to_vec(&value), encoded);
	assert_eq!(cbor::from_slice::<Value>(&encoded), Ok(value));
	let pounds: Quantity = "159.4 [lb_av]".parse().expect("a quantity");
	assert_eq!(pounds.unit(), "[lb_av]");
	for text in ["72.3", "kg", "72.3  kg", "72.3 k g", "x kg"] {
		assert!(text.parse::<Quantity>().is_err(), "{text:?}");
	}
	// A unit with a space cannot be read, and is kept as it is.
	let encoded = tagged(
		7,
		Cbor::Array(vec![cbor_decimal(723, -1), Cbor::from("k g")]),
	);
	let value: Value = cbor::from_slice(&encoded).expect("decodes");
	assert!(matches!(value, Value::Unknown(_)));
	assert_eq!(cbor::to_vec(&value), encoded);
}

#[test]
fn a_name_has_parts_and_always_its_full_form() {
	let name = Name::new("Anna Jones")
		.with_part("given", "Anna")
		.with_part("family", "Jones");
	assert_eq!(name.full(), "Anna Jones");
	assert_eq!(name.part("family"), Some("Jones"));
	assert_eq!(name.part("prefix"), None);
	let mut parts = Map::new();
	parts.insert("full", Cbor::from("Anna Jones"));
	parts.insert("given", Cbor::from("Anna"));
	parts.insert("family", Cbor::from("Jones"));
	let encoded = tagged(8, Cbor::from(parts));
	let value = Value::Name(name);
	assert_eq!(cbor::to_vec(&value), encoded);
	assert_eq!(cbor::from_slice::<Value>(&encoded), Ok(value));
	assert_eq!(ValueKind::Name.code(), 8);
}

#[test]
fn a_name_keeps_parts_from_newer_versions_and_needs_its_full_form() {
	let mut parts = Map::new();
	parts.insert("full", Cbor::from("Ada"));
	parts.insert("nickname", Cbor::from("Enchantress"));
	let encoded = tagged(8, Cbor::from(parts));
	let value: Value = cbor::from_slice(&encoded).expect("decodes");
	let Value::Name(name) = &value else {
		panic!("a name: {value:?}");
	};
	assert_eq!(name.part("nickname"), Some("Enchantress"));
	assert_eq!(cbor::to_vec(&value), encoded);
	for unreadable in [
		map(&[("given", Cbor::from("Ada"))]),
		map(&[("full", Cbor::from(""))]),
		map(&[("full", 5_i64.to_cbor())]),
	] {
		let encoded = tagged(8, Cbor::from(unreadable));
		let value: Value = cbor::from_slice(&encoded).expect("decodes");
		assert!(matches!(value, Value::Unknown(_)), "{value:?}");
		assert_eq!(value.kind(), ValueKind::Name);
		assert_eq!(cbor::to_vec(&value), encoded);
	}
}

#[test]
fn the_new_kinds_have_their_numbers() {
	assert_eq!(ValueKind::Decimal.code(), 6);
	assert_eq!(ValueKind::Quantity.code(), 7);
	assert_eq!(ValueKind::from_code(8), ValueKind::Name);
	assert!(matches!(ValueKind::from_code(9), ValueKind::Other(_)));
}

#[test]
#[should_panic(expected = "a name is not empty")]
fn a_name_written_in_full_is_never_emptied() {
	let _ = Name::new("Ada").with_part("full", "");
}
