//! Languages through the engine: the ranked languages of the user, the device and the app,
//! values in a language, forms of one value, and what a reader sees and finds (S17).

mod common;

use common::{now, this_day, today};
use nibkeep_base::EntityId;
use nibkeep_crypto::PassphraseParams;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{FieldDef, Languages, Name, Tag, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Edit, LanguagesFor, Library, NewLibrary, NewProtection, NodeError, Unlocker};
use nibkeep_testkit::TestRng;

fn languages(ranges: &str) -> Languages {
	ranges.parse().expect("ranges")
}

fn tag(text: &str) -> Tag {
	Tag::parse(text).expect("a tag")
}

fn name(text: &str) -> Value {
	Value::Name(Name::new(text))
}

/// A new library whose user reads `ranges`.
fn library(test: &str, ranges: &str, rng: &mut TestRng) -> (std::path::PathBuf, Library) {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), test);
	let protection = NewProtection::Passphrase(
		"correct horse",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	let new = NewLibrary::new("Ada", "laptop", "nib").with_languages(languages(ranges));
	let library = Library::create(&directory, &new, &protection, now(), rng).expect("creates");
	(directory, library)
}

/// The name a reader sees, by its ranges.
fn seen(library: &mut Library, entity: EntityId, ranges: &str, rng: &mut TestRng) -> String {
	library
		.set_languages(
			LanguagesFor::Replica,
			Some(&languages(ranges)),
			now() + 500_000,
			rng,
		)
		.expect("sets the app's languages");
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("a space")
		.and_then(|view| view.name)
		.expect("a name")
}

#[test]
fn the_most_specific_list_of_languages_counts() {
	let mut rng = TestRng::new("whose languages");
	let (directory, mut library) = library("whose languages", "en, ru", &mut rng);
	assert_eq!(library.languages().to_string(), "en, ru");
	let settings = library.language_settings().expect("reads them");
	assert_eq!(settings.user, Some(languages("en, ru")));
	assert_eq!((settings.device, settings.replica), (None, None));

	let mut set = |library: &mut Library, whose, ranges: Option<&str>, millis| {
		library
			.set_languages(
				whose,
				ranges.map(languages).as_ref(),
				now() + millis,
				&mut rng,
			)
			.expect("sets them");
		library.languages().to_string()
	};
	assert_eq!(
		set(&mut library, LanguagesFor::Device, Some("en"), 1_000),
		"en",
		"the device's list comes before the user's"
	);
	assert_eq!(
		set(&mut library, LanguagesFor::Replica, Some("fr, en"), 2_000),
		"fr, en",
		"the app's list comes before the device's"
	);
	drop(library);
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(library.languages().to_string(), "fr, en");
	assert_eq!(set(&mut library, LanguagesFor::Replica, None, 3_000), "en");
	assert_eq!(
		set(&mut library, LanguagesFor::Device, None, 4_000),
		"en, ru"
	);
	assert_eq!(
		set(&mut library, LanguagesFor::User, Some("sv"), 5_000),
		"sv"
	);
	let settings = library.language_settings().expect("reads them");
	assert_eq!(
		(settings.user, settings.device, settings.replica),
		(Some(languages("sv")), None, None)
	);
}

#[test]
fn a_library_started_without_languages_reads_none() {
	let mut rng = TestRng::new("no languages");
	let (_, library) = library("no languages", "", &mut rng);
	assert!(library.languages().is_empty());
	assert_eq!(library.language_settings().expect("reads them").user, None);
}

/// A person named in Ukrainian, with the passport's transliteration, the name in Russian,
/// and its transliteration from Russian as other forms.
fn four_forms(library: &mut Library, rng: &mut TestRng) -> EntityId {
	let personal = library.personal_space();
	let person = library
		.create_entity_with_language(
			personal,
			&[PERSON],
			vec![(NAME, name("Олена Коваль"))],
			&tag("uk"),
			now(),
			rng,
		)
		.expect("creates");
	for (n, (form, language)) in (1..).zip([
		("Olena Koval", "uk-Latn-t-uk"),
		("Елена Коваль", "ru"),
		("Elena Koval", "ru-Latn-t-ru"),
	]) {
		library
			.edit_entity(
				personal,
				person,
				vec![Edit::AddForm {
					field: NAME,
					of: None,
					value: name(form),
				}],
				&today().with_language(tag(language)),
				now() + n * 1_000,
				rng,
			)
			.expect("adds a form");
	}
	person
}

#[test]
fn a_name_in_four_forms_shows_the_readers_and_is_found_by_any() {
	let mut rng = TestRng::new("four forms");
	let (_, mut library) = library("four forms", "en, *-Latn, uk, ru", &mut rng);
	let personal = library.personal_space();
	let person = four_forms(&mut library, &mut rng);

	let view = library
		.entity(personal, person, this_day())
		.expect("a space")
		.expect("the person");
	assert_eq!(view.name.as_deref(), Some("Olena Koval"));
	let names = &view
		.fields
		.iter()
		.find(|field| field.field == NAME)
		.expect("a name");
	assert!(!names.competing, "forms never compete");
	assert_eq!(names.values.len(), 1);
	assert_eq!(names.values[0].language.as_deref(), Some("uk-Latn-t-uk"));
	assert_eq!(names.values[0].other_forms, 3);
	let listed = library.entities(personal, Some(&PERSON)).expect("lists");
	assert!(
		listed
			.iter()
			.any(|each| each.id == person && each.name.as_deref() == Some("Olena Koval"))
	);

	for form in ["Олена Коваль", "olena koval", "Елена Коваль", "ELENA KOVAL"]
	{
		assert_eq!(
			library.find_entity(personal, form).expect("by any form"),
			person,
			"{form}"
		);
	}
	for word in ["Коваль", "Koval", "Елена", "Olena"] {
		let found = library.search(personal, word).expect("searches");
		let found = found
			.iter()
			.find(|each| each.id == person)
			.unwrap_or_else(|| panic!("{word} finds her"));
		assert_eq!(found.name.as_deref(), Some("Olena Koval"));
	}

	assert_eq!(
		seen(&mut library, person, "ru, uk", &mut rng),
		"Елена Коваль"
	);
	assert_eq!(
		seen(&mut library, person, "en, *-Latn, ru", &mut rng),
		"Elena Koval"
	);
	assert_eq!(seen(&mut library, person, "uk", &mut rng), "Олена Коваль");
}

#[test]
fn a_values_other_forms_are_listed_once() {
	let mut rng = TestRng::new("other forms");
	let (_, mut library) = library("other forms", "en, *-Latn, uk, ru", &mut rng);
	let personal = library.personal_space();
	let person = four_forms(&mut library, &mut rng);
	let forms = |library: &Library| {
		let view = library
			.entity(personal, person, this_day())
			.expect("a space")
			.expect("the person");
		let mut forms: Vec<(String, String)> = view.fields[0].values[0]
			.forms
			.iter()
			.map(|form| {
				let Value::Name(name) = &form.value else {
					panic!("a name");
				};
				(
					name.full().to_owned(),
					form.language.clone().unwrap_or_default(),
				)
			})
			.collect();
		forms.sort();
		forms
	};
	let mut sorted = [
		("Elena Koval".to_owned(), "ru-Latn-t-ru".to_owned()),
		("Олена Коваль".to_owned(), "uk".to_owned()),
		("Елена Коваль".to_owned(), "ru".to_owned()),
	];
	sorted.sort();
	assert_eq!(
		forms(&library),
		sorted,
		"the other forms, not the one shown"
	);
	// A form written again is one form, as is one written as the form shown, and tags
	// compare without regard to case.
	for (n, (form, language)) in (10..).zip([
		("Олена Коваль", "uk"),
		("Olena Koval", "uk-Latn-t-uk"),
		("Olena Koval", "uk-latn-t-uk"),
		("Олена Коваль", "UK"),
	]) {
		library
			.edit_entity(
				personal,
				person,
				vec![Edit::AddForm {
					field: NAME,
					of: None,
					value: name(form),
				}],
				&today().with_language(tag(language)),
				now() + n * 1_000,
				&mut rng,
			)
			.expect("adds a form again");
	}
	assert_eq!(forms(&library), sorted);
}

#[test]
fn correcting_a_form_corrects_only_it_and_keeps_its_language_and_group() {
	let mut rng = TestRng::new("correct a form");
	let (_, mut library) = library("correct a form", "en, *-Latn, uk, ru", &mut rng);
	let personal = library.personal_space();
	let person = four_forms(&mut library, &mut rng);
	library
		.edit_entity(
			personal,
			person,
			vec![Edit::Set(NAME, vec![name("Olena Kovalj")])],
			&today(),
			now() + 10_000,
			&mut rng,
		)
		.expect("corrects the form shown");
	let view = library
		.entity(personal, person, this_day())
		.expect("a space")
		.expect("the person");
	let shown = &view
		.fields
		.iter()
		.find(|field| field.field == NAME)
		.expect("a name")
		.values[0];
	assert_eq!(shown.value, name("Olena Kovalj"));
	assert_eq!(shown.language.as_deref(), Some("uk-Latn-t-uk"));
	assert_eq!(shown.other_forms, 3);
	assert_eq!(seen(&mut library, person, "uk", &mut rng), "Олена Коваль");
	assert!(
		library.find_entity(personal, "Olena Koval").is_err(),
		"corrected"
	);

	// Correcting the first form keeps the group as well.
	library
		.edit_entity(
			personal,
			person,
			vec![Edit::Set(NAME, vec![name("Олена Ковальова")])],
			&today(),
			now() + 20_000,
			&mut rng,
		)
		.expect("corrects the Ukrainian form");
	assert_eq!(
		seen(&mut library, person, "uk", &mut rng),
		"Олена Ковальова"
	);
	assert_eq!(
		seen(&mut library, person, "*-Latn, uk", &mut rng),
		"Olena Kovalj"
	);
	assert_eq!(seen(&mut library, person, "ru", &mut rng), "Елена Коваль");
}

#[test]
fn each_field_shows_the_readers_language_where_there_is_one() {
	let mut rng = TestRng::new("a company");
	let (_, mut library) = library("a company", "en, ru, sv", &mut rng);
	let personal = library.personal_space();
	let company = TypeDef::new(TypeName::from_static("local.company"), 1)
		.with_field(
			"local.summary".parse().expect("a field"),
			FieldDef::new(ValueKind::Text),
		)
		.with_field(
			"local.note".parse().expect("a field"),
			FieldDef::new(ValueKind::Text).many(),
		)
		.with_field(
			"local.founded".parse().expect("a field"),
			FieldDef::new(ValueKind::Date),
		);
	library
		.define_type(personal, &company, now(), &mut rng)
		.expect("defines");
	// Notes in Russian, with the founding date.
	let entity = library
		.create_entity_with_language(
			personal,
			&[TypeName::from_static("local.company")],
			vec![
				(NAME, name("Ромашка")),
				(
					"local.summary".parse().expect("a field"),
					Value::Text("Производитель чая".to_owned()),
				),
				(
					"local.note".parse().expect("a field"),
					Value::Text("Основана в Казани".to_owned()),
				),
				(
					"local.founded".parse().expect("a field"),
					Value::Date("1990".parse().expect("a date")),
				),
			],
			&tag("ru"),
			now() + 1_000,
			&mut rng,
		)
		.expect("creates");
	// Later, an article in English, which gives the summary in English.
	library
		.edit_entity(
			personal,
			entity,
			vec![Edit::AddForm {
				field: "local.summary".parse().expect("a field"),
				of: Some(Value::Text("Производитель чая".to_owned())),
				value: Value::Text("A maker of tea".to_owned()),
			}],
			&today().with_language(tag("en")),
			now() + 2_000,
			&mut rng,
		)
		.expect("adds the English form");

	let view = library
		.entity(personal, entity, this_day())
		.expect("a space")
		.expect("the company");
	let field = |name: &str| {
		view.fields
			.iter()
			.find(|field| field.field.as_str() == name)
			.unwrap_or_else(|| panic!("{name}"))
	};
	assert_eq!(
		field("local.summary").values[0].value,
		Value::Text("A maker of tea".to_owned())
	);
	assert_eq!(
		field("local.summary").values[0].language.as_deref(),
		Some("en")
	);
	assert_eq!(
		field("local.note").values[0].language.as_deref(),
		Some("ru"),
		"Russian where there is no English"
	);
	// A date has no language.
	assert_eq!(field("local.founded").values[0].language, None);
	assert_eq!(field("local.founded").values.len(), 1);
}

#[test]
fn a_form_is_added_to_the_value_it_names() {
	let mut rng = TestRng::new("which value");
	let (_, mut library) = library("which value", "en", &mut rng);
	let personal = library.personal_space();
	let tags: nibkeep_model::FieldName = "local.tag".parse().expect("a field");
	let entity = library
		.create_entity(
			personal,
			&[TypeName::from_static("local.thing")],
			vec![
				(tags.clone(), Value::Text("red".to_owned())),
				(tags.clone(), Value::Text("round".to_owned())),
			],
			now(),
			&mut rng,
		)
		.expect("creates");
	let add = |library: &mut Library, of: Option<&str>, rng: &mut TestRng| {
		library.edit_entity(
			personal,
			entity,
			vec![Edit::AddForm {
				field: tags.clone(),
				of: of.map(|of| Value::Text(of.to_owned())),
				value: Value::Text("красный".to_owned()),
			}],
			&today().with_language(tag("ru")),
			now() + 1_000,
			rng,
		)
	};
	assert!(matches!(
		add(&mut library, None, &mut rng),
		Err(NodeError::WhichValue(_))
	));
	assert!(matches!(
		add(&mut library, Some("blue"), &mut rng),
		Err(NodeError::NoValue(_))
	));
	add(&mut library, Some("red"), &mut rng).expect("adds a form of red");
	let view = library
		.entity(personal, entity, this_day())
		.expect("a space")
		.expect("the thing");
	let shown = &view
		.fields
		.iter()
		.find(|field| field.field == tags)
		.expect("tags")
		.values;
	assert_eq!(shown.len(), 2, "still two values");
	assert!(shown.iter().any(|value| value.other_forms == 1));
}

#[test]
fn an_entity_is_created_with_its_values_in_every_language_at_once() {
	use nibkeep_node::NewValue;
	let mut rng = TestRng::new("at once");
	let (_, mut library) = library("at once", "en, *-Latn, uk", &mut rng);
	let personal = library.personal_space();
	let name_forms = NewValue::new(NAME, name("Олена Коваль"))
		.in_language(tag("uk"))
		.with_form(name("Olena Koval"), Some(tag("uk-Latn-t-uk")))
		.with_form(name("Елена Коваль"), Some(tag("ru")));
	let person = library
		.create_entity_with(personal, &[PERSON], vec![name_forms], now(), &mut rng)
		.expect("creates");
	let view = library
		.entity(personal, person, this_day())
		.expect("a space")
		.expect("the person");
	assert_eq!(view.name.as_deref(), Some("Olena Koval"));
	let names = view
		.fields
		.iter()
		.find(|field| field.field == NAME)
		.expect("a name");
	assert!(!names.competing);
	assert_eq!(names.values[0].other_forms, 2);
	assert_eq!(
		library
			.find_entity(personal, "Елена Коваль")
			.expect("by any form"),
		person
	);
	// Two values of a field with one value are refused, but forms are one value.
	assert!(matches!(
		library.create_entity_with(
			personal,
			&[PERSON],
			vec![
				NewValue::new(NAME, name("Ada")),
				NewValue::new(NAME, name("Augusta"))
			],
			now(),
			&mut rng
		),
		Err(NodeError::OneValue(_))
	));
}

/// A thing with the tag `cat`, also in Russian, and the tag `dog`, read in English.
fn tagged(test: &str, rng: &mut TestRng) -> (Library, EntityId, nibkeep_model::FieldName) {
	use nibkeep_node::NewValue;
	let (_, mut library) = library(test, "en", rng);
	let personal = library.personal_space();
	let tags: nibkeep_model::FieldName = "local.tag".parse().expect("a field");
	let plain = |text: &str| Value::Text(text.to_owned());
	let thing = library
		.create_entity_with(
			personal,
			&[TypeName::from_static("local.thing")],
			vec![
				NewValue::new(NAME, name("Ball")),
				NewValue::new(tags.clone(), plain("cat"))
					.in_language(tag("en"))
					.with_form(plain("кот"), Some(tag("ru"))),
				NewValue::new(tags.clone(), plain("dog")).in_language(tag("en")),
			],
			now(),
			rng,
		)
		.expect("creates");
	(library, thing, tags)
}

/// The values a field shows, and how many other forms each has.
fn shown(
	library: &Library,
	entity: EntityId,
	field: &nibkeep_model::FieldName,
) -> Vec<(String, usize)> {
	let view = library
		.entity(library.personal_space(), entity, this_day())
		.expect("a space")
		.expect("the entity");
	view.fields
		.iter()
		.filter(|each| each.field == *field)
		.flat_map(|each| &each.values)
		.map(|value| {
			(
				value.value.as_text().expect("text").to_owned(),
				value.other_forms,
			)
		})
		.collect()
}

#[test]
fn removing_a_value_names_it_by_any_form_and_takes_every_form() {
	let mut rng = TestRng::new("remove a value");
	let (mut library, thing, tags) = tagged("remove a value", &mut rng);
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			thing,
			vec![Edit::Remove(tags.clone(), Value::Text("кот".to_owned()))],
			&today(),
			now() + 1_000,
			&mut rng,
		)
		.expect("removes the cat");
	assert_eq!(shown(&library, thing, &tags), [("dog".to_owned(), 0)]);
}

#[test]
fn removing_a_form_keeps_the_others() {
	let mut rng = TestRng::new("remove a form");
	let (mut library, thing, tags) = tagged("remove a form", &mut rng);
	let personal = library.personal_space();
	let remove = |library: &mut Library, form: &str, rng: &mut TestRng| {
		library.edit_entity(
			personal,
			thing,
			vec![Edit::RemoveForm(tags.clone(), Value::Text(form.to_owned()))],
			&today(),
			now() + 1_000,
			rng,
		)
	};
	assert!(matches!(
		remove(&mut library, "bird", &mut rng),
		Err(NodeError::NoValue(_))
	));
	remove(&mut library, "cat", &mut rng).expect("removes the English form");
	let mut values = shown(&library, thing, &tags);
	values.sort();
	assert_eq!(values, [("dog".to_owned(), 0), ("кот".to_owned(), 0)]);
}

#[test]
fn setting_a_field_that_shows_several_values_replaces_every_form() {
	let mut rng = TestRng::new("settle");
	let (mut library, thing, tags) = tagged("settle", &mut rng);
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			thing,
			vec![Edit::Set(
				tags.clone(),
				vec![Value::Text("fish".to_owned())],
			)],
			&today(),
			now() + 1_000,
			&mut rng,
		)
		.expect("sets the tags");
	assert_eq!(shown(&library, thing, &tags), [("fish".to_owned(), 0)]);
	seen(&mut library, thing, "ru", &mut rng);
	assert_eq!(
		shown(&library, thing, &tags),
		[("fish".to_owned(), 0)],
		"no Russian form is left"
	);
}

#[test]
fn an_empty_or_purged_list_counts_as_none() {
	let mut rng = TestRng::new("empty lists");
	let (_, mut library) = library("empty lists", "en, ru", &mut rng);
	library
		.set_languages(
			LanguagesFor::Replica,
			Some(&languages("fr")),
			now() + 1_000,
			&mut rng,
		)
		.expect("sets them");
	library
		.set_languages(
			LanguagesFor::Replica,
			Some(&Languages::default()),
			now() + 2_000,
			&mut rng,
		)
		.expect("clears them");
	assert_eq!(library.language_settings().expect("reads").replica, None);
	assert_eq!(library.languages().to_string(), "en, ru");

	library
		.set_languages(
			LanguagesFor::Replica,
			Some(&languages("fr")),
			now() + 3_000,
			&mut rng,
		)
		.expect("sets them");
	assert_eq!(library.languages().to_string(), "fr");
	let personal = library.personal_space();
	let replica = library.status().expect("a status").replica.id;
	library
		.purge(
			personal,
			replica,
			&nibkeep_governance::LANGUAGES,
			None,
			now() + 4_000,
			&mut rng,
		)
		.expect("purges the app's list");
	assert_eq!(library.languages().to_string(), "en, ru");
}
