//! `nib edit --date` and `--on`, and `nib show --on`: values that change over time.

use crate::common::{Test, ids_hidden, library};

/// A library with a person whose employer has one value at a time.
fn with_a_person(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&[
		"type",
		"add",
		"person",
		"name:text",
		"employer:text",
		"phone:text:many",
	])
	.expect("adds the type");
	test.nib(&["new", "person", "name=Anna"])
		.expect("adds the person");
	test
}

/// The line of a field in `nib show`, or `None` if the field shows nothing.
fn line<'a>(output: &'a str, field: &str) -> Option<&'a str> {
	output
		.lines()
		.find(|line| line.trim_start().starts_with(field))
		.map(str::trim)
}

#[test]
fn a_change_with_a_date_ends_the_value_before_it() {
	let mut test = with_a_person("dated changes");
	let ended = test
		.nib(&["edit", "Anna", "--date", "1990/2015", "employer+=Acme"])
		.expect("adds a past employer");
	// Acme's period ended in 2015, so today shows no employer.
	assert_eq!(line(&ended, "local.employer"), None, "{ended}");
	let now = test
		.nib(&["edit", "Anna", "--date", "2019-03", "employer+=Globex"])
		.expect("adds an employer since March 2019");
	assert_eq!(
		ids_hidden(&now),
		"\
Anna
  id              <id>
  type            local.person
  local.employer  Globex (2019-03)
  local.name      Anna
"
	);
	let then = test
		.nib(&["show", "Anna", "--on", "2000-01-01"])
		.expect("shows the past");
	assert_eq!(
		line(&then, "local.employer"),
		Some("local.employer  Acme (1990/2015)")
	);
}

#[test]
fn editing_another_day_corrects_the_value_it_showed() {
	let mut test = with_a_person("correcting the past");
	test.nib(&["edit", "Anna", "--date", "1990/2015", "employer+=Acme"])
		.expect("adds a past employer");
	test.nib(&["edit", "Anna", "--date", "2019-03", "employer+=Globex"])
		.expect("adds an employer");
	let corrected = test
		.nib(&["edit", "Anna", "--on", "2000-01-01", "employer=ACME"])
		.expect("corrects the past");
	assert_eq!(
		line(&corrected, "local.employer"),
		Some("local.employer  ACME (1990/2015)")
	);
	let today = test.nib(&["show", "Anna"]).expect("shows today");
	assert_eq!(
		line(&today, "local.employer"),
		Some("local.employer  Globex (2019-03)")
	);
}

#[test]
fn a_value_added_without_a_date_to_a_dated_field_starts_now() {
	let mut test = with_a_person("added now");
	test.nib(&["edit", "Anna", "--date", "2019-03", "employer+=Globex"])
		.expect("adds an employer");
	let added = test
		.nib(&["edit", "Anna", "employer+=Initech"])
		.expect("adds an employer now");
	assert_eq!(
		line(&added, "local.employer"),
		Some("local.employer  Initech (2026-10-12T10:00:00Z)")
	);
}

#[test]
fn json_gives_each_value_its_valid_time() {
	let mut test = with_a_person("valid time in JSON");
	test.nib(&["edit", "Anna", "--date", "2019-03", "employer+=Globex"])
		.expect("adds an employer");
	let shown = test.json(&["show", "Anna"]);
	let employer = shown["fields"]
		.as_array()
		.and_then(|fields| {
			fields
				.iter()
				.find(|field| field["name"] == "local.employer")
		})
		.expect("the employer");
	assert_eq!(employer["values"][0]["valid_time"], "2019-03");
	let name = shown["fields"]
		.as_array()
		.and_then(|fields| fields.iter().find(|field| field["name"] == "local.name"))
		.expect("the name");
	assert!(name["values"][0]["valid_time"].is_null());
}

#[test]
fn values_that_may_not_have_happened_and_undated_ones_are_marked() {
	let mut test = with_a_person("marks");
	test.nib(&["edit", "Anna", "--date", "2010-06~", "employer+=Acme"])
		.expect("adds an employer about June 2010");
	let uncertain = test
		.nib(&["show", "Anna", "--on", "2010-06-10"])
		.expect("shows June 2010");
	assert_eq!(
		line(&uncertain, "local.employer"),
		Some("local.employer  Acme (2010-06~, uncertain)")
	);
	test.nib(&["edit", "Anna", "phone+=home", "--date", "1985/2003"])
		.expect("adds a dated phone");
	test.nib(&["edit", "Anna", "phone+=work"])
		.expect("adds an undated phone");
	let phones = test
		.nib(&["show", "Anna", "--on", "1990-01-01"])
		.expect("shows 1990");
	assert_eq!(
		line(&phones, "local.phone"),
		Some("local.phone  home (1985/2003); work (date unknown)")
	);
	let shown = test.json(&["show", "Anna", "--on", "2010-06-10"]);
	let employer = shown["fields"]
		.as_array()
		.and_then(|fields| {
			fields
				.iter()
				.find(|field| field["name"] == "local.employer")
		})
		.expect("the employer");
	assert_eq!(employer["values"][0]["presence"], "uncertain");
}
