//! Encodings from the examples in RFC 8949, appendix A and section 3.4.4, for the
//! data items that canonical CBOR allows.

use nibkeep_base::cbor::{Integer, Map, MapKey, Value, decode, encode};

/// Bytes from hexadecimal text; spaces are ignored.
fn hex(text: &str) -> Vec<u8> {
	let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
	digits
		.chunks(2)
		.map(|pair| {
			u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII"), 16)
				.expect("valid hex in test")
		})
		.collect()
}

fn int(value: i128) -> Value {
	Value::Integer(Integer::new(value).expect("integer in range"))
}

fn text(value: &str) -> Value {
	Value::Text(value.to_owned())
}

fn map(entries: Vec<(MapKey, Value)>) -> Value {
	Value::Map(entries.into_iter().collect::<Map>())
}

/// Checks that `value` encodes to `expected` and that `expected` decodes to `value`.
fn check(value: &Value, expected: &str) {
	let bytes = hex(expected);
	assert_eq!(encode(value), bytes, "encoding of {value:?}");
	assert_eq!(
		&decode(&bytes).expect("example decodes"),
		value,
		"decoding of {expected}"
	);
}

#[test]
fn integers() {
	check(&int(0), "00");
	check(&int(1), "01");
	check(&int(10), "0a");
	check(&int(23), "17");
	check(&int(24), "1818");
	check(&int(25), "1819");
	check(&int(100), "1864");
	check(&int(1000), "1903e8");
	check(&int(1_000_000), "1a000f4240");
	check(&int(1_000_000_000_000), "1b000000e8d4a51000");
	check(&int(18_446_744_073_709_551_615), "1bffffffffffffffff");
	check(&int(-18_446_744_073_709_551_616), "3bffffffffffffffff");
	check(&int(-1), "20");
	check(&int(-10), "29");
	check(&int(-100), "3863");
	check(&int(-1000), "3903e7");
}

#[test]
fn integers_outside_the_cbor_range_are_not_values() {
	assert!(Integer::new(18_446_744_073_709_551_616).is_none());
	assert!(Integer::new(-18_446_744_073_709_551_617).is_none());
}

#[test]
fn simple_values() {
	check(&Value::Bool(false), "f4");
	check(&Value::Bool(true), "f5");
	check(&Value::Null, "f6");
}

#[test]
fn byte_strings() {
	check(&Value::Bytes(vec![]), "40");
	check(&Value::Bytes(vec![1, 2, 3, 4]), "4401020304");
}

#[test]
fn text_strings() {
	check(&text(""), "60");
	check(&text("a"), "6161");
	check(&text("IETF"), "6449455446");
	check(&text("\"\\"), "62225c");
	check(&text("\u{00fc}"), "62c3bc");
	check(&text("\u{6c34}"), "63e6b0b4");
	check(&text("\u{10151}"), "64f0908591");
}

#[test]
fn arrays() {
	check(&Value::Array(vec![]), "80");
	check(&Value::Array(vec![int(1), int(2), int(3)]), "83010203");
	check(
		&Value::Array(vec![
			int(1),
			Value::Array(vec![int(2), int(3)]),
			Value::Array(vec![int(4), int(5)]),
		]),
		"8301820203820405",
	);
	check(
		&Value::Array((1..=25).map(int).collect()),
		"98190102030405060708090a0b0c0d0e0f101112131415161718181819",
	);
}

#[test]
fn maps() {
	check(&map(vec![]), "a0");
	check(
		&map(vec![(MapKey::from(1), int(2)), (MapKey::from(3), int(4))]),
		"a201020304",
	);
	check(
		&map(vec![
			(MapKey::from("a"), int(1)),
			(MapKey::from("b"), Value::Array(vec![int(2), int(3)])),
		]),
		"a26161016162820203",
	);
	check(
		&Value::Array(vec![text("a"), map(vec![(MapKey::from("b"), text("c"))])]),
		"826161a161626163",
	);
	check(
		&map(["a", "b", "c", "d", "e"]
			.into_iter()
			.map(|key| (MapKey::from(key), text(&key.to_uppercase())))
			.collect()),
		"a56161614161626142616361436164614461656145",
	);
}

#[test]
fn decimal_fraction() {
	// 273.15, the example in RFC 8949, section 3.4.4.
	check(
		&Value::Decimal {
			exponent: Integer::from(-2),
			mantissa: Integer::from(27315),
		},
		"c48221196ab3",
	);
}

#[test]
fn uuid() {
	check(
		&Value::Uuid([
			0x0b, 0x6f, 0x3c, 0x39, 0x8b, 0x4e, 0x4b, 0x8e, 0x9c, 0x2f, 0x5d, 0x0f, 0x6a, 0x7e,
			0x9a, 0x10,
		]),
		"d825500b6f3c398b4e4b8e9c2f5d0f6a7e9a10",
	);
}

#[test]
fn map_keys_sort_by_their_encoding() {
	// "b" (61 62) sorts before "aa" (62 61 61), because its encoding starts with a
	// smaller length. Integers sort before negative integers, which sort before text.
	let unordered = map(vec![
		(MapKey::from("aa"), int(1)),
		(MapKey::from("b"), int(2)),
		(MapKey::from(-1), int(3)),
		(MapKey::from(1), int(4)),
	]);
	check(&unordered, "a4 01 04 20 03 6162 02 626161 01");
}
