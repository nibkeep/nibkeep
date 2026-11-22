//! Decimals, quantities with their units, and names, through `nib`.

mod common;

use common::{Test, library};
use serde_json::json;

/// A library with a type of dogs: a name, a price and a weight in kilograms.
fn with_dogs(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&[
		"type",
		"add",
		"dog",
		"name:name",
		"price:decimal",
		"weight:quantity:unit=kg",
	])
	.expect("adds the type");
	test
}

/// The line of a field in `nib show`.
fn line<'a>(output: &'a str, field: &str) -> Option<&'a str> {
	output
		.lines()
		.find(|line| line.trim_start().starts_with(field))
		.map(str::trim)
}

#[test]
fn values_keep_their_precision_and_units() {
	let mut test = with_dogs("new kinds");
	let rex = test
		.nib(&["new", "dog", "name=Rex", "price=120.50", "weight=12.40"])
		.expect("adds a dog");
	assert_eq!(rex.lines().next(), Some("Rex"));
	assert_eq!(line(&rex, "local.price"), Some("local.price   120.50"));
	// A number alone takes the field's unit.
	assert_eq!(line(&rex, "local.weight"), Some("local.weight  12.40 kg"));
	let pounds = test
		.nib(&["edit", "Rex", "weight=27.3 [lb_av]"])
		.expect("weighs in pounds");
	assert_eq!(
		line(&pounds, "local.weight"),
		Some("local.weight  27.3 [lb_av]")
	);
	let shown = test.json(&["show", "Rex"]);
	let value = |field: &str| {
		shown["fields"]
			.as_array()
			.and_then(|fields| fields.iter().find(|shown| shown["name"] == field))
			.map(|shown| shown["values"][0]["value"].clone())
	};
	assert_eq!(value("local.price"), Some(json!("120.50")));
	assert_eq!(
		value("local.weight"),
		Some(json!({ "number": "27.3", "unit": "[lb_av]" }))
	);
	assert_eq!(value("local.name"), Some(json!({ "full": "Rex" })));
}

#[test]
fn a_field_of_quantities_shows_its_unit() {
	let mut test = with_dogs("units of fields");
	let shown = test.nib(&["type", "show", "dog"]).expect("shows the type");
	assert_eq!(
		line(&shown, "local.weight"),
		Some("local.weight  quantity, in kg")
	);
	assert_eq!(
		test.json(&["type", "show", "dog"])["fields"][2]["unit"],
		json!("kg")
	);
}

#[test]
fn values_of_the_new_kinds_are_checked() {
	let mut test = with_dogs("checked kinds");
	for (assignment, expected) in [
		("price=12,50", "a decimal number"),
		("weight=heavy", "a number and a unit"),
	] {
		let error = test
			.nib(&["new", "dog", "name=Rex", assignment])
			.expect_err("refuses");
		assert!(error.to_string().contains(expected), "{error}");
	}
	let error = test
		.nib(&["type", "add", "cat", "weight:decimal:unit=kg"])
		.expect_err("refuses a unit for decimals");
	assert!(error.to_string().contains("one unit"), "{error}");
}
