//! More names than the table of shared names holds, in a process of their own, so that
//! they fill it for no other test.

use nibkeep_model::FieldName;

#[test]
fn any_number_of_names_can_be_parsed() {
	for number in 0..10_000 {
		let text = format!("org.example.field-{number}");
		let name: FieldName = text.parse().expect("valid");
		assert_eq!(name.as_str(), text);
	}
}

#[test]
fn names_new_after_thousands_of_others_are_shared() {
	for round in 0..3 {
		for number in 0..5_000 {
			let junk: FieldName = format!("org.example.junk-{round}-{number}")
				.parse()
				.expect("valid");
			drop(junk);
		}
		let wanted = format!("org.example.wanted-{round}");
		let first: FieldName = wanted.parse().expect("valid");
		let again: FieldName = wanted.parse().expect("valid");
		assert!(
			std::ptr::eq(first.as_str(), again.as_str()),
			"a name new after 5,000 others, in round {round}"
		);
	}
}
