//! Names of types and fields.

use nibkeep_base::cbor::{self, Value};
use nibkeep_model::{FieldName, TypeName};

const VALID: &[&str] = &[
	"std.person",
	"org.nibkeep.valid-time",
	"com.1password.login",
	"io.example.a-b.c2",
	"a.b",
];

const INVALID: &[&str] = &[
	"",
	"person",
	"org..nibkeep",
	".org.nibkeep",
	"org.nibkeep.",
	"Org.nibkeep",
	"org.nibkeep.Person",
	"org.nibkeep.-x",
	"org.nibkeep.x-",
	"org.nibkeep.per son",
	"std.person_name",
	"org.nibkeep.caf\u{e9}",
];

#[test]
fn valid_names_print_back_unchanged() {
	for &text in VALID {
		let type_name: TypeName = text.parse().expect(text);
		let field_name: FieldName = text.parse().expect(text);
		assert_eq!(type_name.to_string(), text);
		assert_eq!(field_name.as_str(), text);
	}
}

#[test]
fn invalid_names_are_rejected() {
	for &text in INVALID {
		assert!(text.parse::<TypeName>().is_err(), "{text:?} was accepted");
		assert!(text.parse::<FieldName>().is_err(), "{text:?} was accepted");
	}
}

#[test]
fn names_have_at_most_128_bytes() {
	let longest = format!("a.{}", "b".repeat(126));
	assert!(longest.parse::<FieldName>().is_ok());
	assert!(format!("{longest}b").parse::<FieldName>().is_err());
}

#[test]
fn names_are_text_in_cbor() {
	let name = FieldName::from_static("org.nibkeep.name");
	assert_eq!(name, "org.nibkeep.name".parse().expect("valid"));
	assert_eq!(
		cbor::to_vec(&name),
		cbor::encode(&Value::from("org.nibkeep.name"))
	);
	assert_eq!(cbor::from_slice(&cbor::to_vec(&name)), Ok(name));
	assert!(cbor::from_slice::<TypeName>(&cbor::encode(&Value::from("Person"))).is_err());
}

#[test]
fn search_takes_words_from_content_but_not_from_structure_or_keys() {
	use nibkeep_model::fields::{self, has_words};

	for searched in [
		"org.nibkeep.name",
		"org.nibkeep.title",
		"org.nibkeep.website",
		"local.notes",
	] {
		assert!(has_words(&searched.parse().expect("a name")), "{searched}");
	}
	for unsearched in [
		fields::TYPE,
		fields::TYPE_NAME,
		fields::SIGNING_KEY,
		fields::POSITION,
	] {
		assert!(!has_words(&unsearched), "{unsearched}");
	}
}

#[test]
fn names_compare_order_and_hash_by_their_text_however_made() {
	use std::collections::BTreeSet;
	use std::hash::{BuildHasher, RandomState};

	let fixed = FieldName::from_static("org.nibkeep.name");
	let parsed: FieldName = "org.nibkeep.name".parse().expect("valid");
	assert_eq!(fixed, parsed);
	let hasher = RandomState::new();
	assert_eq!(hasher.hash_one(&fixed), hasher.hash_one(&parsed));
	let names: BTreeSet<FieldName> = [
		"org.nibkeep.type".parse().expect("valid"),
		FieldName::from_static("org.nibkeep.alternative"),
		parsed,
		fixed,
	]
	.into_iter()
	.collect();
	let texts: Vec<&str> = names.iter().map(FieldName::as_str).collect();
	assert_eq!(
		texts,
		[
			"org.nibkeep.alternative",
			"org.nibkeep.name",
			"org.nibkeep.type"
		]
	);
	assert_eq!(
		format!("{:?}", FieldName::from_static("a.b")),
		r#"FieldName("a.b")"#
	);
}

/// Parses a name twice in a row: the table of shared names starts again only after
/// thousands of new names, which the tests of this file do not parse.
#[test]
fn copies_of_a_name_share_its_text() {
	let parsed: FieldName = "org.example.shared".parse().expect("valid");
	let copy = parsed.clone();
	assert!(std::ptr::eq(parsed.as_str(), copy.as_str()), "a copy");
	let again: FieldName = "org.example.shared".parse().expect("valid");
	assert!(
		std::ptr::eq(parsed.as_str(), again.as_str()),
		"the same name parsed again"
	);
}
