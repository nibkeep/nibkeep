//! The decoder rejects every input that is not the canonical encoding of a value.

use nibkeep_base::cbor::{DecodeError, DecodeErrorKind, MAX_DEPTH, decode};

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

fn rejects(input: &str) -> DecodeError {
	match decode(&hex(input)) {
		Ok(value) => panic!("{input} was accepted as {value:?}"),
		Err(error) => error,
	}
}

fn rejects_with(input: &str, kind: &DecodeErrorKind) {
	assert_eq!(rejects(input).kind(), kind, "error for {input}");
}

#[test]
fn numbers_must_use_the_shortest_form() {
	rejects_with("18 17", &DecodeErrorKind::NotShortest);
	rejects_with("19 0017", &DecodeErrorKind::NotShortest);
	rejects_with("19 00ff", &DecodeErrorKind::NotShortest);
	rejects_with("1a 0000ffff", &DecodeErrorKind::NotShortest);
	rejects_with("1b 00000000ffffffff", &DecodeErrorKind::NotShortest);
	rejects_with("38 00", &DecodeErrorKind::NotShortest);
	// The length of a byte string, too.
	rejects_with("58 01 61", &DecodeErrorKind::NotShortest);
	// And a tag number.
	rejects_with("d8 04 82 01 02", &DecodeErrorKind::NotShortest);
}

#[test]
fn indefinite_lengths_are_rejected() {
	rejects_with(
		"5f 42 0102 43 030405 ff",
		&DecodeErrorKind::IndefiniteLength,
	);
	rejects_with(
		"7f 65 7374726561 64 6d696e67 ff",
		&DecodeErrorKind::IndefiniteLength,
	);
	rejects_with("9f ff", &DecodeErrorKind::IndefiniteLength);
	rejects_with("bf ff", &DecodeErrorKind::IndefiniteLength);
}

#[test]
fn floats_are_rejected() {
	rejects_with("f9 0000", &DecodeErrorKind::Float);
	rejects_with("f9 7c00", &DecodeErrorKind::Float);
	rejects_with("fa 47c35000", &DecodeErrorKind::Float);
	rejects_with("fb 3ff199999999999a", &DecodeErrorKind::Float);
}

#[test]
fn other_simple_values_are_rejected() {
	rejects_with("f7", &DecodeErrorKind::SimpleValue);
	rejects_with("f0", &DecodeErrorKind::SimpleValue);
	rejects_with("f8 18", &DecodeErrorKind::SimpleValue);
	rejects_with("ff", &DecodeErrorKind::SimpleValue);
}

#[test]
fn reserved_additional_information_is_rejected() {
	rejects_with("1c", &DecodeErrorKind::Reserved);
	rejects_with("5d", &DecodeErrorKind::Reserved);
	rejects_with("fc", &DecodeErrorKind::Reserved);
}

#[test]
fn map_keys_must_be_sorted_and_unique() {
	// {"b": 1, "a": 2}
	rejects_with("a2 6162 01 6161 02", &DecodeErrorKind::UnsortedKeys);
	// {"aa": 1, "b": 2}: "b" has the shorter encoding, so it comes first.
	rejects_with("a2 626161 01 6162 02", &DecodeErrorKind::UnsortedKeys);
	// {"a": 1, "a": 2}
	rejects_with("a2 6161 01 6161 02", &DecodeErrorKind::DuplicateKey);
	// {"a": 1, 1: 2}: integers come before text.
	rejects_with("a2 6161 01 01 02", &DecodeErrorKind::UnsortedKeys);
}

#[test]
fn map_keys_must_be_integers_or_text() {
	rejects_with("a1 40 01", &DecodeErrorKind::InvalidMapKey);
	rejects_with("a1 80 01", &DecodeErrorKind::InvalidMapKey);
	rejects_with("a1 f5 01", &DecodeErrorKind::InvalidMapKey);
	rejects_with(
		&format!("a1 d825 50 {} 01", "00".repeat(16)),
		&DecodeErrorKind::InvalidMapKey,
	);
}

#[test]
fn only_known_tags_are_accepted() {
	rejects_with(
		"c0 74 323031332d30332d32315432303a30343a30305a",
		&DecodeErrorKind::UnsupportedTag,
	);
	rejects_with("c2 49 010000000000000000", &DecodeErrorKind::UnsupportedTag);
	rejects_with("d8 18 45 6449455446", &DecodeErrorKind::UnsupportedTag);
}

#[test]
fn tag_content_must_have_the_right_shape() {
	// A UUID needs exactly 16 bytes.
	rejects_with(
		&format!("d825 4f {}", "00".repeat(15)),
		&DecodeErrorKind::InvalidTagContent,
	);
	rejects_with(
		&format!("d825 51 {}", "00".repeat(17)),
		&DecodeErrorKind::InvalidTagContent,
	);
	rejects_with("d825 61 61", &DecodeErrorKind::InvalidTagContent);
	// A decimal needs an array of exactly two integers.
	rejects_with("c4 83 01 02 03", &DecodeErrorKind::InvalidTagContent);
	rejects_with("c4 82 01 6161", &DecodeErrorKind::InvalidTagContent);
	rejects_with("c4 01", &DecodeErrorKind::InvalidTagContent);
}

#[test]
fn text_must_be_valid_utf8() {
	rejects_with("62 c328", &DecodeErrorKind::InvalidUtf8);
	rejects_with("61 ff", &DecodeErrorKind::InvalidUtf8);
}

#[test]
fn input_must_end_exactly_after_the_value() {
	rejects_with("00 00", &DecodeErrorKind::TrailingBytes);
	rejects_with("", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("19 03", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("62 61", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("82 01", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("a1 01", &DecodeErrorKind::UnexpectedEnd);
}

#[test]
fn huge_lengths_are_rejected_without_allocating() {
	rejects_with("5b ffffffffffffffff", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("7b ffffffffffffffff", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("9b ffffffffffffffff", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("bb ffffffffffffffff", &DecodeErrorKind::UnexpectedEnd);
}

#[test]
fn nesting_is_limited() {
	let nested = |levels: usize| format!("{}00", "81".repeat(levels));
	assert!(decode(&hex(&nested(MAX_DEPTH))).is_ok());
	rejects_with(&nested(MAX_DEPTH + 1), &DecodeErrorKind::TooDeep);
}

#[test]
fn errors_report_where_they_happened() {
	// The second key of {"b": 1, "a": 2} starts at byte 4.
	assert_eq!(rejects("a2 6162 01 6161 02").offset(), 4);
	assert_eq!(rejects("00 00").offset(), 1);
}

#[test]
fn chains_of_tags_are_rejected_without_deep_recursion() {
	// Two million nested tag headers must not exhaust the stack.
	let mut input = vec![0xc4; 2_000_000];
	input.push(0x01);
	let error = decode(&input).expect_err("a chain of tags is not a value");
	assert_eq!(error.kind(), &DecodeErrorKind::InvalidTagContent);
	// The same chain where a map key belongs.
	let mut key = vec![0xa1];
	key.extend(std::iter::repeat_n(0xc4, 2_000_000));
	key.push(0x01);
	let error = decode(&key).expect_err("a tag is not a map key");
	assert_eq!(error.kind(), &DecodeErrorKind::InvalidMapKey);
}

#[test]
fn tags_do_not_count_towards_the_nesting_limit() {
	let decimal_in_arrays = format!("{} c4 82 01 02", "81".repeat(MAX_DEPTH));
	assert!(decode(&hex(&decimal_in_arrays)).is_ok());
}

#[test]
fn an_unsupported_tag_is_reported_where_its_header_starts() {
	assert_eq!(rejects("d8 18 01").offset(), 0);
	assert_eq!(rejects("82 01 db 0000000100000000 01").offset(), 2);
}

#[test]
fn truncated_tag_content_is_reported_as_the_end_of_input() {
	rejects_with("c4 82 01", &DecodeErrorKind::UnexpectedEnd);
	rejects_with("d825 50 0000", &DecodeErrorKind::UnexpectedEnd);
}
