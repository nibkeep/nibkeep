//! Types: optional fields, unknown fields, secret fields, roles and labels.

mod common;

use common::{at, entity, set, statement_id, text};
use nibkeep_base::EntityId;
use nibkeep_base::cbor::{self, Map, ToCbor, Value as Cbor};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{
	FieldDef, FieldName, Fit, Labels, Languages, Multiplicity, Operation, Resolution, State, Tag,
	TypeDef, TypeName, Value, ValueKind,
};

fn name(text: &'static str) -> FieldName {
	FieldName::from_static(text)
}

fn person_v1() -> TypeDef {
	TypeDef::new(TypeName::from_static("std.person"), 1)
		.with_field(
			name("org.nibkeep.name"),
			FieldDef::new(ValueKind::Text).many(),
		)
		.with_field(
			name("org.nibkeep.birth-date"),
			FieldDef::new(ValueKind::Date),
		)
}

fn person_v2() -> TypeDef {
	TypeDef::new(TypeName::from_static("std.person"), 2)
		.with_field(
			name("org.nibkeep.name"),
			FieldDef::new(ValueKind::Text).many(),
		)
		.with_field(
			name("org.nibkeep.birth-date"),
			FieldDef::new(ValueKind::Date),
		)
		.with_field(name("org.nibkeep.nickname"), FieldDef::new(ValueKind::Text))
}

#[test]
fn all_fields_are_optional() {
	// An entity needs no values at all to be of a type.
	let mut state = State::new();
	state
		.apply(
			&set(1, 1, "org.nibkeep.type", text("std.person"), &[]),
			at(1),
		)
		.infallible();
	let person = person_v2();
	assert!(state.types(entity(1)).infallible().contains(person.name()));
	for (field, definition) in person.fields() {
		assert_eq!(
			state
				.resolve(entity(1), field, Some(definition))
				.infallible(),
			Resolution::Values(vec![])
		);
	}
}

#[test]
fn a_field_that_an_older_type_version_does_not_know_survives_a_round_trip() {
	// A newer app records a nickname, a field that version 1 of the type does not list,
	// with a value of a kind that this version of NibKeep does not know either.
	let encoded = cbor::encode(&Cbor::Array(vec![
		0_u64.to_cbor(),
		Cbor::Array(vec![
			statement_id(1).to_cbor(),
			entity(1).to_cbor(),
			Cbor::from("org.nibkeep.nickname"),
			Cbor::Array(vec![42_u64.to_cbor(), Cbor::from("Ada")]),
			Cbor::from(Map::new()),
		]),
		Cbor::Array(vec![]),
	]));

	// An app that knows only version 1 reads the operation and writes it back unchanged.
	let operation: Operation = cbor::from_slice(&encoded).expect("decodes");
	assert_eq!(cbor::to_vec(&operation), encoded);

	// It keeps the statement, even when it changes other fields.
	let mut state = State::new();
	state.apply(&operation, at(1)).infallible();
	state
		.apply(&set(2, 1, "org.nibkeep.name", text("Ada"), &[]), at(2))
		.infallible();
	let nickname = state
		.current(entity(1), &name("org.nibkeep.nickname"))
		.infallible();
	assert_eq!(nickname.len(), 1);
	assert_eq!(person_v1().fit(&nickname[0]), Fit::UnknownField);
	assert_eq!(person_v2().fit(&nickname[0]), Fit::OtherKind);
}

#[test]
fn values_of_another_kind_are_kept_and_flagged() {
	let mut state = State::new();
	state
		.apply(
			&set(1, 1, "org.nibkeep.birth-date", text("around 1815"), &[]),
			at(1),
		)
		.infallible();
	let person = person_v1();
	let birth_date = name("org.nibkeep.birth-date");
	let current = state.current(entity(1), &birth_date).infallible();
	assert_eq!(person.fit(&current[0]), Fit::OtherKind);
	assert_eq!(
		state
			.resolve(entity(1), &birth_date, person.field(&birth_date))
			.infallible(),
		Resolution::Values(current)
	);
	state
		.apply(
			&set(
				2,
				1,
				"org.nibkeep.birth-date",
				Value::Date("1815-12-10".parse().expect("valid")),
				&[1],
			),
			at(2),
		)
		.infallible();
	assert_eq!(
		person.fit(&state.current(entity(1), &birth_date).infallible()[0]),
		Fit::Matches
	);
}

#[test]
fn secret_fields_are_marked_as_such() {
	let password = name("org.nibkeep.password");
	let username = name("org.nibkeep.username");
	let login = TypeDef::new(TypeName::from_static("org.nibkeep.login"), 1)
		.with_field(username.clone(), FieldDef::new(ValueKind::Text))
		.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret());
	assert!(login.field(&password).is_some_and(FieldDef::is_secret));
	assert!(!login.field(&username).is_some_and(FieldDef::is_secret));
	// The mark survives a round trip.
	let decoded: TypeDef = cbor::from_slice(&cbor::to_vec(&login)).expect("decodes");
	assert!(decoded.field(&password).is_some_and(FieldDef::is_secret));
}

#[test]
fn a_relationship_entity_connects_participants_through_roles() {
	// Ilya Sutskever's PhD connects four entities and carries data of its own.
	let reference = || FieldDef::new(ValueKind::Reference).role();
	let education = TypeDef::new(TypeName::from_static("org.nibkeep.education"), 1)
		.with_field(name("org.nibkeep.student"), reference())
		.with_field(name("org.nibkeep.institution"), reference())
		.with_field(name("org.nibkeep.field-of-study"), reference())
		.with_field(name("org.nibkeep.advisor"), reference().many())
		.with_field(name("org.nibkeep.degree"), FieldDef::new(ValueKind::Text));
	let (phd, ilya, toronto, computer_science, hinton) =
		(10, entity(1), entity(2), entity(3), entity(4));
	let mut state = State::new();
	for (n, (field, value)) in [
		("org.nibkeep.type", text("org.nibkeep.education")),
		("org.nibkeep.student", Value::Reference(ilya)),
		("org.nibkeep.institution", Value::Reference(toronto)),
		(
			"org.nibkeep.field-of-study",
			Value::Reference(computer_science),
		),
		("org.nibkeep.advisor", Value::Reference(hinton)),
		("org.nibkeep.degree", text("PhD")),
	]
	.into_iter()
	.enumerate()
	{
		let n = u16::try_from(n).expect("small");
		state
			.apply(&set(n, phd, field, value, &[]), at(u64::from(n)))
			.infallible();
	}

	// The relationships Ilya takes part in are found through the statements that refer to
	// him, and their fields are roles.
	let referrers = state.referrers(ilya).infallible();
	assert_eq!(referrers.len(), 1);
	assert_eq!(referrers[0].entity, entity(phd));
	assert!(
		education
			.field(&referrers[0].field)
			.is_some_and(FieldDef::is_role)
	);
	assert_eq!(
		state.referrers(hinton).infallible()[0].field,
		name("org.nibkeep.advisor")
	);

	// From the relationship, every participant can be reached through its roles.
	let participants: Vec<EntityId> = education
		.fields()
		.filter(|(_, definition)| definition.is_role())
		.flat_map(|(field, _)| state.current(entity(phd), field).infallible())
		.filter_map(|statement| statement.value.as_reference())
		.collect();
	assert_eq!(participants.len(), 4);
	assert!(
		!education
			.field(&name("org.nibkeep.degree"))
			.is_some_and(FieldDef::is_role)
	);
}

#[test]
fn field_definitions_report_their_multiplicity() {
	assert_eq!(
		FieldDef::new(ValueKind::Text).multiplicity(),
		Multiplicity::One
	);
	assert_eq!(
		FieldDef::new(ValueKind::Text).many().multiplicity(),
		Multiplicity::Many
	);
	assert_eq!(
		FieldDef::new(ValueKind::Text)
			.last_writer_wins()
			.multiplicity(),
		Multiplicity::LastWriterWins
	);
	assert_eq!(FieldDef::new(ValueKind::Date).kind(), ValueKind::Date);
}

/// The CBOR form of a field definition with the given kind and extra keys.
fn field_map(kind: u64, extra: &[(&str, Cbor)]) -> Cbor {
	let mut map = Map::new();
	map.insert("kind", kind.to_cbor());
	for (key, value) in extra {
		map.insert(*key, value.clone());
	}
	Cbor::from(map)
}

/// The CBOR form of a type with the given fields.
fn type_map(fields: Map) -> Map {
	let mut map = Map::new();
	map.insert("name", Cbor::from("org.nibkeep.login"));
	map.insert("version", 3_u64.to_cbor());
	map.insert("fields", Cbor::from(fields));
	map
}

#[test]
fn types_have_one_cbor_form() {
	let login = TypeDef::new(TypeName::from_static("org.nibkeep.login"), 3)
		.with_field(
			name("org.nibkeep.password"),
			FieldDef::new(ValueKind::Text).secret(),
		)
		.with_field(
			name("org.nibkeep.last-used"),
			FieldDef::new(ValueKind::Date).last_writer_wins(),
		)
		.with_field(
			name("org.nibkeep.tag"),
			FieldDef::new(ValueKind::Text).many(),
		);
	let mut fields = Map::new();
	fields.insert(
		"org.nibkeep.last-used",
		field_map(3, &[("multiplicity", 2_u64.to_cbor())]),
	);
	fields.insert(
		"org.nibkeep.password",
		field_map(0, &[("secret", Cbor::from(true))]),
	);
	fields.insert(
		"org.nibkeep.tag",
		field_map(0, &[("multiplicity", 1_u64.to_cbor())]),
	);
	let expected = cbor::encode(&Cbor::from(type_map(fields)));
	assert_eq!(cbor::to_vec(&login), expected);
	assert_eq!(cbor::from_slice::<TypeDef>(&expected), Ok(login));
}

#[test]
fn types_with_explicit_defaults_are_rejected() {
	let malformed_fields = [
		field_map(0, &[("secret", Cbor::from(false))]),
		field_map(0, &[("role", Cbor::from(false))]),
		field_map(0, &[("multiplicity", 0_u64.to_cbor())]),
		field_map(0, &[("multiplicity", Cbor::from("many"))]),
		Cbor::from(Map::new()),
	];
	for field in malformed_fields {
		let mut fields = Map::new();
		fields.insert("org.nibkeep.password", field.clone());
		let encoded = cbor::encode(&Cbor::from(type_map(fields)));
		assert!(
			cbor::from_slice::<TypeDef>(&encoded).is_err(),
			"{field:?} was accepted"
		);
	}
	let mut bad_field_name = Map::new();
	bad_field_name.insert("Password", field_map(0, &[]));
	let mut missing_version = type_map(Map::new());
	missing_version.remove(&"version".into());
	for map in [type_map(bad_field_name), missing_version] {
		assert!(cbor::from_slice::<TypeDef>(&cbor::encode(&Cbor::from(map))).is_err());
	}
}

#[test]
fn keys_from_newer_versions_of_a_type_survive_a_round_trip() {
	let mut fields = Map::new();
	fields.insert(
		"org.nibkeep.tag",
		field_map(
			0,
			&[
				("multiplicity", 7_u64.to_cbor()),
				("label", Cbor::from("Tags")),
			],
		),
	);
	let mut map = type_map(fields);
	map.insert("colour", Cbor::from("red"));
	let encoded = cbor::encode(&Cbor::from(map));
	let type_def: TypeDef = cbor::from_slice(&encoded).expect("kept");
	assert_eq!(cbor::to_vec(&type_def), encoded);

	// A multiplicity this version does not know shows all values, like many.
	let tag = type_def.field(&name("org.nibkeep.tag")).expect("listed");
	assert!(matches!(tag.multiplicity(), Multiplicity::Other(other) if other.code() == 7));
	let mut state = State::new();
	state
		.apply(&set(1, 1, "org.nibkeep.tag", text("a"), &[]), at(1))
		.infallible();
	state
		.apply(&set(2, 1, "org.nibkeep.tag", text("b"), &[]), at(1))
		.infallible();
	let Resolution::Values(values) = state
		.resolve(entity(1), &name("org.nibkeep.tag"), Some(tag))
		.infallible()
	else {
		panic!("all values are shown");
	};
	assert_eq!(values.len(), 2);
}

#[test]
fn versions_can_be_large() {
	let type_def = TypeDef::new(
		TypeName::from_static("org.example.plugin.thing"),
		20_261_012_120_000,
	);
	let decoded: TypeDef = cbor::from_slice(&cbor::to_vec(&type_def)).expect("decodes");
	assert_eq!(decoded.version(), 20_261_012_120_000);
}

#[test]
fn types_that_define_a_field_differently_combine_carefully() {
	let one = FieldDef::new(ValueKind::Text);
	let latest = FieldDef::new(ValueKind::Text).last_writer_wins();
	let many = FieldDef::new(ValueKind::Text).many();
	let secret = FieldDef::new(ValueKind::Text).secret();
	let combine = |definitions: &[&FieldDef]| {
		FieldDef::combine(definitions.iter().copied()).expect("some definitions")
	};
	assert_eq!(combine(&[&latest, &one]).multiplicity(), Multiplicity::One);
	assert_eq!(
		combine(&[&one, &many, &latest]).multiplicity(),
		Multiplicity::Many
	);
	assert!(combine(&[&one, &secret]).is_secret());
	assert_eq!(combine(&[&latest]), latest);
	assert_eq!(FieldDef::combine([]), None);
}

#[test]
fn a_link_has_a_name_for_its_other_end() {
	let author_of = name("org.example.author-of");
	let author = FieldDef::new(ValueKind::Reference)
		.many()
		.with_reverse(author_of.clone());
	assert_eq!(author.reverse(), Some(&author_of));
	assert_eq!(FieldDef::new(ValueKind::Reference).reverse(), None);
	let encoded = cbor::to_vec(&author);
	assert_eq!(
		encoded,
		cbor::encode(&field_map(
			4,
			&[
				("multiplicity", 1_u64.to_cbor()),
				("reverse", Cbor::from("org.example.author-of")),
			]
		))
	);
	assert_eq!(cbor::from_slice::<FieldDef>(&encoded), Ok(author));
	// A reverse name is a field's name in form, though no field of its own.
	for wrong in [Cbor::from("Author of"), 1_u64.to_cbor()] {
		let encoded = cbor::encode(&field_map(4, &[("reverse", wrong.clone())]));
		assert!(
			cbor::from_slice::<FieldDef>(&encoded).is_err(),
			"{wrong:?} was accepted"
		);
	}
}

#[test]
fn a_link_can_stand_for_what_it_refers_to() {
	let used_by = FieldDef::new(ValueKind::Reference).many().stands_for();
	assert!(used_by.is_stands_for());
	assert!(!FieldDef::new(ValueKind::Reference).is_stands_for());
	let encoded = cbor::to_vec(&used_by);
	assert_eq!(
		encoded,
		cbor::encode(&field_map(
			4,
			&[
				("multiplicity", 1_u64.to_cbor()),
				("stands-for", Cbor::from(true)),
			]
		))
	);
	assert_eq!(cbor::from_slice::<FieldDef>(&encoded), Ok(used_by));
	// Like the other flags, it is written only when it is set.
	let explicit = cbor::encode(&field_map(4, &[("stands-for", Cbor::from(false))]));
	assert!(cbor::from_slice::<FieldDef>(&explicit).is_err());
}

#[test]
fn definitions_of_a_link_combine() {
	let plain = FieldDef::new(ValueKind::Reference).many();
	let first = name("org.example.author-of");
	let second = name("org.example.wrote");
	let named = plain.clone().with_reverse(first.clone());
	let standing = plain.clone().with_reverse(second).stands_for();
	// The field stands for what it refers to if any definition says so, and the reverse
	// name is that of the first definition that has one, as with units.
	let combined = FieldDef::combine([&plain, &named, &standing]).expect("combined");
	assert!(combined.is_stands_for());
	assert_eq!(combined.reverse(), Some(&first));
	// Standing for is a matter of care, as being a role is; a reverse name is not.
	assert!(!plain.same_care(&standing));
	assert!(plain.same_care(&named));
}

#[test]
fn a_search_can_go_through_a_link() {
	let author = FieldDef::new(ValueKind::Reference).many().search_through();
	assert!(author.is_search_through());
	assert!(!FieldDef::new(ValueKind::Reference).is_search_through());
	let encoded = cbor::to_vec(&author);
	assert_eq!(
		encoded,
		cbor::encode(&field_map(
			4,
			&[
				("multiplicity", 1_u64.to_cbor()),
				("search-through", Cbor::from(true)),
			]
		))
	);
	assert_eq!(cbor::from_slice::<FieldDef>(&encoded), Ok(author.clone()));
	let explicit = cbor::encode(&field_map(4, &[("search-through", Cbor::from(false))]));
	assert!(cbor::from_slice::<FieldDef>(&explicit).is_err());
	// A search goes through the link if any definition says so, which is a matter of care.
	let plain = FieldDef::new(ValueKind::Reference).many();
	let combined = FieldDef::combine([&plain, &author]).expect("combined");
	assert!(combined.is_search_through());
	assert!(!plain.same_care(&author));
}

#[test]
fn text_can_hold_codes() {
	let isbn = FieldDef::new(ValueKind::Text).many().code();
	assert!(isbn.is_code());
	assert!(!FieldDef::new(ValueKind::Text).is_code());
	let encoded = cbor::to_vec(&isbn);
	assert_eq!(
		encoded,
		cbor::encode(&field_map(
			0,
			&[
				("multiplicity", 1_u64.to_cbor()),
				("code", Cbor::from(true))
			]
		))
	);
	assert_eq!(cbor::from_slice::<FieldDef>(&encoded), Ok(isbn.clone()));
	let explicit = cbor::encode(&field_map(0, &[("code", Cbor::from(false))]));
	assert!(cbor::from_slice::<FieldDef>(&explicit).is_err());
	// The field holds codes if any definition says so; a definition that does not say so
	// takes no less care, since codes only have no language.
	let plain = FieldDef::new(ValueKind::Text).many();
	let combined = FieldDef::combine([&plain, &isbn]).expect("combined");
	assert!(combined.is_code());
	assert!(plain.same_care(&isbn));
}

fn tag(text: &str) -> Tag {
	Tag::parse(text).expect("a language tag")
}

fn languages(text: &str) -> Languages {
	text.parse().expect("language ranges")
}

#[test]
fn types_and_fields_have_labels_in_languages() {
	let account = TypeDef::new(TypeName::from_static("org.example.account"), 1)
		.with_label(Some(&tag("en")), "Bank account")
		.with_label(Some(&tag("ru")), "Банковский счёт")
		.with_field(
			name("org.example.number"),
			FieldDef::new(ValueKind::Text)
				.code()
				.with_label(Some(&tag("en")), "Account #")
				.with_label(None, "№ счёта"),
		)
		.with_field(name("org.example.bank"), FieldDef::new(ValueKind::Text));
	let mut type_labels = Map::new();
	type_labels.insert("en", Cbor::from("Bank account"));
	type_labels.insert("ru", Cbor::from("Банковский счёт"));
	let mut field_labels = Map::new();
	field_labels.insert("en", Cbor::from("Account #"));
	field_labels.insert("und", Cbor::from("№ счёта"));
	let mut fields = Map::new();
	fields.insert("org.example.bank", field_map(0, &[]));
	fields.insert(
		"org.example.number",
		field_map(
			0,
			&[
				("code", Cbor::from(true)),
				("labels", Cbor::from(field_labels)),
			],
		),
	);
	let mut map = Map::new();
	map.insert("name", Cbor::from("org.example.account"));
	map.insert("version", 1_u64.to_cbor());
	map.insert("fields", Cbor::from(fields));
	map.insert("labels", Cbor::from(type_labels));
	let expected = cbor::encode(&Cbor::from(map));
	assert_eq!(cbor::to_vec(&account), expected);
	assert_eq!(cbor::from_slice::<TypeDef>(&expected), Ok(account.clone()));

	// Labels say how a field is shown, not how carefully it is kept.
	let number = account.field(&name("org.example.number")).expect("listed");
	assert!(number.same_care(&FieldDef::new(ValueKind::Text)));
	assert_eq!(number.labels().get(Some(&tag("EN"))), Some("Account #"));
	assert_eq!(number.labels().get(None), Some("№ счёта"));
	let bank = account.field(&name("org.example.bank")).expect("listed");
	assert!(bank.labels().is_empty());
}

#[test]
fn labels_are_a_map_of_text_when_given() {
	let mut fields = Map::new();
	for labels in [
		Cbor::from(Map::new()),
		Cbor::from("Account"),
		{
			let mut map = Map::new();
			map.insert("en", 1_u64.to_cbor());
			Cbor::from(map)
		},
		{
			let mut map = Map::new();
			map.insert("en", Cbor::from(""));
			Cbor::from(map)
		},
	] {
		fields.insert(
			"org.example.number",
			field_map(0, &[("labels", labels.clone())]),
		);
		let encoded = cbor::encode(&Cbor::from(type_map(fields.clone())));
		assert!(
			cbor::from_slice::<TypeDef>(&encoded).is_err(),
			"{labels:?} was accepted"
		);
		let mut map = type_map(Map::new());
		map.insert("labels", labels.clone());
		let encoded = cbor::encode(&Cbor::from(map));
		assert!(
			cbor::from_slice::<TypeDef>(&encoded).is_err(),
			"{labels:?} was accepted for the type"
		);
	}
}

#[test]
fn labels_combine_language_by_language_and_the_first_counts() {
	let first = FieldDef::new(ValueKind::Text)
		.with_label(Some(&tag("en")), "PIN")
		.with_label(Some(&tag("sv")), "PIN-kod");
	let second = FieldDef::new(ValueKind::Text)
		.secret()
		.with_label(Some(&tag("en")), "Personal code")
		.with_label(Some(&tag("ru")), "ПИН");
	let combined = FieldDef::combine([&first, &second]).expect("combined");
	assert!(combined.is_secret());
	let labels: Vec<(String, &str)> = combined
		.labels()
		.iter()
		.map(|(tag, text)| (tag.to_owned(), text))
		.collect();
	assert_eq!(
		labels,
		[
			("en".to_owned(), "PIN"),
			("ru".to_owned(), "ПИН"),
			("sv".to_owned(), "PIN-kod")
		]
	);

	let type_name = TypeName::from_static("org.example.lock");
	let one = TypeDef::new(type_name.clone(), 1).with_label(Some(&tag("en")), "Lock");
	let two = TypeDef::new(type_name, 2)
		.with_label(Some(&tag("en")), "Padlock")
		.with_label(Some(&tag("uk")), "Замок");
	let combined = one.combine(&two);
	assert_eq!(combined.labels().get(Some(&tag("en"))), Some("Lock"));
	assert_eq!(combined.labels().get(Some(&tag("uk"))), Some("Замок"));
}

#[test]
fn the_label_shown_is_in_the_readers_first_language_and_any_label_beats_none() {
	let labels = Labels::new()
		.with(Some(&tag("ru")), "Счёт")
		.with(Some(&tag("en")), "Account")
		.with(None, "Konto?");
	assert_eq!(labels.best(&languages("en, ru")), Some("Account"));
	assert_eq!(labels.best(&languages("ru, en")), Some("Счёт"));
	assert_eq!(labels.best(&languages("sv, *-Cyrl")), Some("Счёт"));
	// In no language the reader ranked: one in a known language, before one in none.
	assert_eq!(labels.best(&languages("sv")), Some("Account"));
	assert_eq!(labels.best(&Languages::default()), Some("Account"));
	assert_eq!(
		Labels::new().with(None, "Konto?").best(&languages("sv")),
		Some("Konto?")
	);
	assert_eq!(Labels::new().best(&languages("en")), None);
	// A tag is kept in its canonical case, and a label replaces another in its language.
	let replaced = Labels::new()
		.with(Some(&tag("EN-gb")), "Colour")
		.with(Some(&tag("en-GB")), "Color");
	assert_eq!(replaced.iter().collect::<Vec<_>>(), [("en-GB", "Color")]);
}

#[test]
fn labels_in_tags_this_version_does_not_write_are_kept_unread() {
	for tag in ["EN", "not a tag", "x-newer-syntax!"] {
		let mut labels = Map::new();
		labels.insert(tag, Cbor::from("Account"));
		let mut fields = Map::new();
		fields.insert(
			"org.example.number",
			field_map(0, &[("labels", Cbor::from(labels.clone()))]),
		);
		let mut map = type_map(fields);
		map.insert("labels", Cbor::from(labels));
		let encoded = cbor::encode(&Cbor::from(map));
		let read: TypeDef = cbor::from_slice(&encoded).expect("kept");
		// A language never has two labels: this version reads none of them.
		assert!(read.labels().is_empty(), "{tag}");
		let field = read.field(&name("org.example.number")).expect("listed");
		assert!(field.labels().is_empty(), "{tag}");
		assert_eq!(cbor::to_vec(&read), encoded, "{tag} kept as it was");
	}
}
