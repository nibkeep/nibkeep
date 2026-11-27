//! Types in a space: defined by entities, combined with the types in code, and grown by
//! adding fields.

use nibkeep_model::storage::Infallibly;
use std::collections::BTreeSet;

use crate::common::{at, entity, statement};
use nibkeep_base::cbor::{self, FromCbor, Map, MapKey, ToCbor, Value as Cbor};
use nibkeep_model::schema::{self, FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::{
	DefinitionError, FieldDef, FieldName, Multiplicity, Operation, State, TypeDef, TypeName, Types,
	Value, ValueKind, fields,
};
use nibkeep_testkit::TestRng;

const BOOK: TypeName = TypeName::from_static("org.example.book");

fn name(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn book() -> TypeDef {
	TypeDef::new(BOOK, 1)
		.with_field(name("org.example.title"), FieldDef::new(ValueKind::Text))
		.with_field(
			name("org.example.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		)
}

/// Applies operations to a state, as if one change at `millis` held them.
fn apply(state: &mut State, operations: &[Operation], millis: u64) {
	for operation in operations {
		state.apply(operation, at(millis)).infallible();
	}
}

/// The types of a space with the given state.
fn read(state: &State) -> Types {
	let mut types = Types::new();
	types.read(state).infallible();
	types
}

/// The names of a type's fields.
fn field_names(definition: &TypeDef) -> Vec<String> {
	definition
		.fields()
		.map(|(name, _)| name.to_string())
		.collect()
}

#[test]
fn a_type_defined_in_a_space_is_read_back() {
	let mut rng = TestRng::new("read back");
	let mut state = State::new();
	let operations = Types::new().define(&book(), &mut rng);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	assert_eq!(types.get(&BOOK), Some(&book()));
	assert!(types.iter().any(|definition| *definition.name() == BOOK));

	// Defining it again adds nothing.
	nibkeep_testkit::assert_empty!(types.define(&book(), &mut rng));
	// NibKeep's own types are known without being stored.
	for own in [TYPE_DEFINITION, FIELD_DEFINITION] {
		let definition = Types::new().get(&own).cloned().expect("defined by NibKeep");
		nibkeep_testkit::assert_empty!(Types::new().define(&definition, &mut rng));
	}
}

#[test]
fn fields_added_on_two_devices_both_stand() {
	let mut rng = TestRng::new("two devices");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let known = read(&state);
	let with_year = book().with_field(name("org.example.year"), FieldDef::new(ValueKind::Date));
	let with_isbn = book().with_field(name("org.example.isbn"), FieldDef::new(ValueKind::Text));
	let on_phone = known.define(&with_year, &mut rng);
	let on_laptop = known.define(&with_isbn, &mut rng);
	assert_eq!(
		on_phone.len(),
		4,
		"one new field: its type, type, name and kind"
	);
	apply(&mut state, &on_phone, 2);
	apply(&mut state, &on_laptop, 2);
	let types = read(&state);
	assert_eq!(
		field_names(types.get(&BOOK).expect("the book")),
		[
			"org.example.author",
			"org.example.isbn",
			"org.example.title",
			"org.example.year"
		]
	);
}

#[test]
fn a_type_defined_twice_combines() {
	// Two devices define the same type before they learn of each other.
	let mut rng = TestRng::new("twice");
	let mut state = State::new();
	let notes = name("org.example.notes");
	let title = name("org.example.title");
	let second = TypeDef::new(BOOK, 2)
		.with_field(title.clone(), FieldDef::new(ValueKind::Text).many())
		.with_field(notes.clone(), FieldDef::new(ValueKind::Text).secret());
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	apply(&mut state, &Types::new().define(&second, &mut rng), 1);
	let types = read(&state);
	let combined = types.get(&BOOK).expect("the book");
	assert_eq!(combined.version(), 2, "the newest version");
	assert_eq!(
		field_names(combined),
		[
			"org.example.author",
			"org.example.notes",
			"org.example.title"
		]
	);
	let title_definition = combined.field(&title).expect("a title");
	assert_eq!(
		title_definition.multiplicity(),
		Multiplicity::Many,
		"the most careful"
	);
	assert_eq!(types.secret_fields(), BTreeSet::from([notes]));
}

#[test]
fn a_field_is_secret_if_any_type_says_so() {
	let mut rng = TestRng::new("secret");
	let password = name("org.example.password");
	let mut types = Types::new();
	types.add(
		TypeDef::new(TypeName::from_static("org.example.login"), 1)
			.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret()),
	);
	let mut state = State::new();
	let account = TypeDef::new(TypeName::from_static("org.example.account"), 1)
		.with_field(password.clone(), FieldDef::new(ValueKind::Text));
	apply(&mut state, &Types::new().define(&account, &mut rng), 1);
	types.read(&state).infallible();
	assert!(types.field(&password).expect("defined").is_secret());
	assert!(types.secret_fields().contains(&password));
	assert!(types.field(&name("org.example.unknown")).is_none());
}

#[test]
fn a_deleted_field_definition_leaves_the_type() {
	let mut rng = TestRng::new("deleted");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let author = state
		.statements()
		.infallible()
		.into_iter()
		.find(|statement| {
			statement.field == fields::FIELD_NAME
				&& statement.value == Value::Text("org.example.author".into())
		})
		.map(|statement| statement.entity)
		.expect("the author's definition");
	apply(
		&mut state,
		&[Operation::assert(nibkeep_model::Statement::new(
			rng.statement_id(),
			author,
			fields::DELETED,
			Value::Boolean(true),
		))],
		2,
	);
	assert_eq!(
		field_names(read(&state).get(&BOOK).expect("the book")),
		["org.example.title"]
	);
}

#[test]
fn competing_values_and_unknown_multiplicities_are_read() {
	// A type, written by hand: its field has two competing kinds and a multiplicity from a
	// newer version of NibKeep.
	let mut state = State::new();
	let operations = [
		statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		),
		statement(2, 1, "org.nibkeep.type-name", Value::Text(BOOK.to_string())),
		statement(
			3,
			2,
			"org.nibkeep.type",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(4, 2, "org.nibkeep.field-of", Value::Reference(entity(1))),
		statement(
			5,
			2,
			"org.nibkeep.field-name",
			Value::Text("org.example.pages".into()),
		),
		statement(9, 2, "org.nibkeep.field-kind", Value::Integer(1)),
		statement(6, 2, "org.nibkeep.field-kind", Value::Integer(0)),
		statement(7, 2, "org.nibkeep.field-multiplicity", Value::Integer(7)),
	]
	.map(Operation::assert);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	let pages = types
		.get(&BOOK)
		.and_then(|book| book.field(&name("org.example.pages")))
		.expect("the pages");
	assert_eq!(
		pages.kind(),
		ValueKind::Text,
		"the statement with the smallest ID"
	);
	assert!(matches!(pages.multiplicity(), Multiplicity::Other(other) if other.code() == 7));
	assert_eq!(types.get(&BOOK).map(TypeDef::version), Some(1));
	// A field without a kind is no field.
	let mut broken = State::new();
	apply(&mut broken, &operations[..5], 1);
	assert!(
		read(&broken)
			.get(&BOOK)
			.is_some_and(|book| book.fields().count() == 0)
	);
}

#[test]
fn a_newer_version_of_a_type_is_recorded() {
	let mut rng = TestRng::new("versions");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let second =
		TypeDef::new(BOOK, 2).with_field(name("org.example.year"), FieldDef::new(ValueKind::Date));
	let operations = read(&state).define(&second, &mut rng);
	apply(&mut state, &operations, 2);
	assert_eq!(read(&state).get(&BOOK).map(TypeDef::version), Some(2));
	// A newer version without new fields is recorded too.
	let third = TypeDef::new(BOOK, 3);
	let operations = read(&state).define(&third, &mut rng);
	assert_eq!(operations.len(), 1);
	apply(&mut state, &operations, 3);
	let types = read(&state);
	assert_eq!(types.get(&BOOK).map(TypeDef::version), Some(3));
	assert_eq!(types.get(&BOOK).map(|book| book.fields().count()), Some(3));
	nibkeep_testkit::assert_empty!(types.define(&third, &mut rng));
}

#[test]
fn the_fields_nibkeep_reserves_keep_their_definitions() {
	let mut rng = TestRng::new("reserved");
	// Defining them is left out.
	let sneaky = TypeDef::new(TypeName::from_static("org.example.sneaky"), 1)
		.with_field(fields::TYPE, FieldDef::new(ValueKind::Text).secret())
		.with_field(name("org.example.note"), FieldDef::new(ValueKind::Text));
	let operations = Types::new().define(&sneaky, &mut rng);
	assert!(operations.iter().all(|operation| match operation {
		Operation::Assert { statement, .. } =>
			statement.value != Value::Text(fields::TYPE.to_string()),
		_ => true,
	}));
	// So is reading them, and a space's definitions of the two types that define types.
	let mut state = State::new();
	let by_hand = [
		statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		),
		statement(
			2,
			1,
			"org.nibkeep.type-name",
			Value::Text("org.example.sneaky".into()),
		),
		statement(
			3,
			2,
			"org.nibkeep.type",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(4, 2, "org.nibkeep.field-of", Value::Reference(entity(1))),
		statement(
			5,
			2,
			"org.nibkeep.field-name",
			Value::Text("org.nibkeep.type".into()),
		),
		statement(6, 2, "org.nibkeep.field-kind", Value::Integer(0)),
		statement(7, 2, "org.nibkeep.field-secret", Value::Boolean(true)),
		statement(
			8,
			3,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		),
		statement(
			9,
			3,
			"org.nibkeep.type-name",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(
			10,
			4,
			"org.nibkeep.type",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(11, 4, "org.nibkeep.field-of", Value::Reference(entity(3))),
		statement(
			12,
			4,
			"org.nibkeep.field-name",
			Value::Text("org.example.extra".into()),
		),
		statement(13, 4, "org.nibkeep.field-kind", Value::Integer(0)),
	]
	.map(Operation::assert);
	apply(&mut state, &by_hand, 1);
	let types = read(&state);
	assert!(types.secret_fields().is_empty());
	assert!(
		types
			.get(&TypeName::from_static("org.example.sneaky"))
			.is_some_and(|sneaky| sneaky.fields().count() == 0)
	);
	assert_eq!(
		types.get(&FIELD_DEFINITION),
		Types::new().get(&FIELD_DEFINITION)
	);
}

#[test]
fn definitions_in_code_count_first_in_any_order() {
	let mut rng = TestRng::new("code first");
	let pages = name("org.example.pages");
	let in_code =
		TypeDef::new(BOOK, 1).with_field(pages.clone(), FieldDef::new(ValueKind::Integer));
	let in_space =
		TypeDef::new(BOOK, 1).with_field(pages.clone(), FieldDef::new(ValueKind::Text).many());
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&in_space, &mut rng), 1);

	let mut code_then_space = Types::new();
	code_then_space.add(in_code.clone());
	code_then_space.read(&state).infallible();
	let mut space_then_code = Types::new();
	space_then_code.read(&state).infallible();
	space_then_code.add(in_code);
	for types in [code_then_space, space_then_code] {
		let field = types.field(&pages).expect("the pages");
		assert_eq!(field.kind(), ValueKind::Integer, "the kind in code");
		assert_eq!(field.multiplicity(), Multiplicity::Many, "the most careful");
	}
}

#[test]
fn keys_from_newer_versions_of_nibkeep_are_kept() {
	let mut rng = TestRng::new("newer keys");
	let mut map = Map::new();
	map.insert("name", BOOK.to_cbor());
	map.insert("version", 1_u64.to_cbor());
	map.insert("fields", Map::new());
	map.insert("from-the-future", true.to_cbor());
	let with_future_key = TypeDef::from_cbor(&Cbor::Map(map)).expect("a type");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let mut types = Types::new();
	types.add(with_future_key);
	types.read(&state).infallible();
	let combined = cbor::to_vec(types.get(&BOOK).expect("the book"));
	let decoded: Cbor = cbor::from_slice(&combined).expect("CBOR");
	let Cbor::Map(decoded) = decoded else {
		panic!("a map");
	};
	assert_eq!(
		decoded.get(&MapKey::from("from-the-future")),
		Some(&Cbor::Bool(true))
	);
}

#[test]
fn a_definition_whose_first_value_cannot_be_read_is_passed_over() {
	let mut state = State::new();
	let operations = [
		statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		),
		statement(2, 1, "org.nibkeep.type-name", Value::Text(BOOK.to_string())),
		statement(
			3,
			2,
			"org.nibkeep.type",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(4, 2, "org.nibkeep.field-of", Value::Reference(entity(1))),
		statement(
			5,
			2,
			"org.nibkeep.field-name",
			Value::Text("org.example.pages".into()),
		),
		statement(
			6,
			2,
			"org.nibkeep.field-kind",
			Value::Text("a kind from the future".into()),
		),
		statement(9, 2, "org.nibkeep.field-kind", Value::Integer(1)),
	]
	.map(Operation::assert);
	apply(&mut state, &operations, 1);
	assert!(
		read(&state)
			.get(&BOOK)
			.is_some_and(|book| book.fields().count() == 0)
	);
}

#[test]
fn definitions_that_would_have_no_effect_are_refused() {
	use nibkeep_model::DefinitionError;

	let mut rng = TestRng::new("refused");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let types = read(&state);
	let title = name("org.example.title");
	let author = name("org.example.author");
	assert_eq!(types.check(&book()), Ok(()), "the same again");
	assert_eq!(
		types.check(&TypeDef::new(TYPE_DEFINITION, 1)),
		Err(DefinitionError::OwnType(TYPE_DEFINITION))
	);
	assert_eq!(
		types
			.check(&TypeDef::new(BOOK, 1).with_field(fields::TYPE, FieldDef::new(ValueKind::Text))),
		Err(DefinitionError::ReservedField(fields::TYPE))
	);
	assert_eq!(
		types.check(
			&TypeDef::new(BOOK, 1).with_field(title.clone(), FieldDef::new(ValueKind::Integer))
		),
		Err(DefinitionError::OtherKind {
			field: title.clone(),
			kind: ValueKind::Text
		})
	);
	assert!(matches!(
		types.check(&TypeDef::new(BOOK, 1).with_field(author, FieldDef::new(ValueKind::Reference))),
		Err(DefinitionError::LessCareful { .. })
	));
	assert_eq!(
		types
			.check(&TypeDef::new(BOOK, 1).with_field(title, FieldDef::new(ValueKind::Text).many())),
		Ok(()),
		"more care is fine"
	);
}

#[test]
fn growing_a_type_raises_its_version() {
	let mut rng = TestRng::new("growing");
	let mut state = State::new();
	let year = name("org.example.year");
	let first = Types::new().extend(
		BOOK,
		[(name("org.example.title"), FieldDef::new(ValueKind::Text))],
	);
	assert_eq!(first.version(), 1, "a new type");
	apply(&mut state, &Types::new().define(&first, &mut rng), 1);
	let types = read(&state);
	let same = types.extend(
		BOOK,
		[(name("org.example.title"), FieldDef::new(ValueKind::Text))],
	);
	assert_eq!(same.version(), 1, "nothing new");
	let grown = types.extend(BOOK, [(year.clone(), FieldDef::new(ValueKind::Date))]);
	assert_eq!(grown.version(), 2);
	assert!(grown.field(&year).is_some());
	let operations = types.define(&grown, &mut rng);
	apply(&mut state, &operations, 2);
	let types = read(&state);
	assert_eq!(types.get(&BOOK).map(TypeDef::version), Some(2));
	assert_eq!(types.get(&BOOK).map(|book| book.fields().count()), Some(2));
}

#[test]
fn only_nibkeep_defines_its_own_fields() {
	let nibkeep_name: FieldName = "org.nibkeep.name".parse().expect("a field name");
	let person = TypeDef::new(TypeName::from_static("org.nibkeep.user"), 1)
		.with_field(nibkeep_name.clone(), FieldDef::new(ValueKind::Name));
	// An app's type and a space's type both say names have many values, and are secret.
	let city = TypeDef::new(TypeName::from_static("org.example.city"), 1).with_field(
		nibkeep_name.clone(),
		FieldDef::new(ValueKind::Text).many().secret(),
	);
	let town = TypeDef::new(TypeName::from_static("local.town"), 1)
		.with_field(nibkeep_name.clone(), FieldDef::new(ValueKind::Text).many());
	let mut types = Types::new();
	types.add(person);
	types.add(city);
	let mut rng = TestRng::new("only NibKeep defines its fields");
	let mut state = State::new();
	let operations = types.define(&town, &mut rng);
	apply(&mut state, &operations, 1);
	types.read(&state).infallible();
	let definition = types.field(&nibkeep_name).expect("defined");
	assert_eq!(definition.kind(), ValueKind::Name);
	assert_eq!(definition.multiplicity(), Multiplicity::One);
	assert!(!definition.is_secret());
	assert!(!types.secret_fields().contains(&nibkeep_name));
}

#[test]
fn a_field_of_quantities_keeps_its_unit() {
	let weight: FieldName = "org.example.weight".parse().expect("a field name");
	let dog = TypeDef::new(TypeName::from_static("org.example.dog"), 1).with_field(
		weight.clone(),
		FieldDef::new(ValueKind::Quantity).with_unit("kg"),
	);
	let encoded = cbor::to_vec(&dog);
	assert_eq!(
		cbor::from_slice::<TypeDef>(&encoded).ok(),
		Some(dog.clone())
	);
	let mut rng = TestRng::new("a unit in a space");
	let mut state = State::new();
	let operations = Types::new().define(&dog, &mut rng);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	assert_eq!(
		types.field(&weight).as_ref().and_then(FieldDef::unit),
		Some("kg")
	);
}

#[test]
fn a_space_lists_a_field_of_nibkeep_only_as_nibkeep_defines_it() {
	let nibkeep_name: FieldName = "org.nibkeep.name".parse().expect("a field name");
	let mut types = Types::new();
	types.add(
		TypeDef::new(TypeName::from_static("org.nibkeep.user"), 1)
			.with_field(nibkeep_name.clone(), FieldDef::new(ValueKind::Name)),
	);
	let as_nibkeep = TypeDef::new(TypeName::from_static("local.city"), 1)
		.with_field(nibkeep_name.clone(), FieldDef::new(ValueKind::Name));
	assert_eq!(types.check(&as_nibkeep), Ok(()));
	let otherwise = TypeDef::new(TypeName::from_static("local.town"), 1)
		.with_field(nibkeep_name.clone(), FieldDef::new(ValueKind::Name).many());
	assert!(matches!(
		types.check(&otherwise),
		Err(nibkeep_model::DefinitionError::NibKeepField(field)) if field == nibkeep_name
	));
}

#[test]
fn a_field_of_nibkeep_that_nibkeep_does_not_define_keeps_its_secret() {
	let pin: FieldName = "org.nibkeep.pin".parse().expect("a field name");
	let login = TypeDef::new(TypeName::from_static("local.login"), 1)
		.with_field(pin.clone(), FieldDef::new(ValueKind::Text).secret());
	let mut rng = TestRng::new("a secret field of nibkeep");
	let mut state = State::new();
	let operations = Types::new().define(&login, &mut rng);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	assert!(
		types
			.field(&pin)
			.is_some_and(|definition| definition.is_secret())
	);
	assert!(types.secret_fields().contains(&pin));
}

#[test]
fn another_unit_is_no_matter_of_care() {
	let weight: FieldName = "org.example.weight".parse().expect("a field name");
	let mut types = Types::new();
	types.add(
		TypeDef::new(TypeName::from_static("org.example.dog"), 1).with_field(
			weight.clone(),
			FieldDef::new(ValueKind::Quantity).with_unit("kg"),
		),
	);
	for definition in [
		FieldDef::new(ValueKind::Quantity),
		FieldDef::new(ValueKind::Quantity).with_unit("[lb_av]"),
	] {
		let cat = TypeDef::new(TypeName::from_static("local.cat"), 1)
			.with_field(weight.clone(), definition);
		assert_eq!(types.check(&cat), Ok(()));
	}
}

#[test]
fn a_link_keeps_its_reverse_name_and_its_marks() {
	let used_by = name("org.example.used-by");
	let pen_name = TypeDef::new(TypeName::from_static("org.example.pen-name"), 1).with_field(
		used_by.clone(),
		FieldDef::new(ValueKind::Reference)
			.many()
			.stands_for()
			.search_through()
			.with_reverse(name("org.example.pen-name")),
	);
	let mut rng = TestRng::new("a link in a space");
	let mut state = State::new();
	let operations = Types::new().define(&pen_name, &mut rng);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	let field = types.field(&used_by).expect("defined");
	assert!(field.is_stands_for());
	assert!(field.is_search_through());
	assert_eq!(field.reverse(), Some(&name("org.example.pen-name")));
	assert_eq!(types.get(pen_name.name()), Some(&pen_name));
	nibkeep_testkit::assert_empty!(types.define(&pen_name, &mut rng));

	// A reverse name that is no field's name in form is passed over, as a unit that is no
	// unit is.
	let mut state = State::new();
	let by_hand = [
		statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		),
		statement(
			2,
			1,
			"org.nibkeep.type-name",
			Value::Text("org.example.pen-name".into()),
		),
		statement(
			3,
			2,
			"org.nibkeep.type",
			Value::Text(FIELD_DEFINITION.to_string()),
		),
		statement(4, 2, "org.nibkeep.field-of", Value::Reference(entity(1))),
		statement(
			5,
			2,
			"org.nibkeep.field-name",
			Value::Text("org.example.used-by".into()),
		),
		statement(6, 2, "org.nibkeep.field-kind", Value::Integer(4)),
		statement(
			7,
			2,
			"org.nibkeep.field-reverse",
			Value::Text("Pen name".into()),
		),
	];
	apply(&mut state, &by_hand.map(Operation::assert), 1);
	let field = read(&state).field(&used_by).expect("defined");
	assert_eq!(field.reverse(), None);
	assert!(!field.is_stands_for());
}

#[test]
fn only_links_have_what_links_have_and_keep_their_reverse_name() {
	let author = name("org.example.author");
	let title = name("org.example.title");
	let book = |field: FieldDef| TypeDef::new(BOOK, 1).with_field(author.clone(), field);
	let types = Types::new();
	for definition in [
		FieldDef::new(ValueKind::Text).stands_for(),
		FieldDef::new(ValueKind::Text).search_through(),
		FieldDef::new(ValueKind::Text).with_reverse(name("org.example.titled")),
	] {
		let titled = TypeDef::new(BOOK, 1).with_field(title.clone(), definition);
		assert_eq!(
			types.check(&titled),
			Err(DefinitionError::NotALink(title.clone()))
		);
	}
	let mut rng = TestRng::new("reverse names");
	let mut state = State::new();
	let named =
		book(FieldDef::new(ValueKind::Reference).with_reverse(name("org.example.author-of")));
	apply(&mut state, &Types::new().define(&named, &mut rng), 1);
	let types = read(&state);
	assert_eq!(types.check(&named), Ok(()));
	assert_eq!(
		types.check(&book(FieldDef::new(ValueKind::Reference))),
		Ok(()),
		"a definition without a reverse name leaves it"
	);
	assert_eq!(
		types.check(&book(
			FieldDef::new(ValueKind::Reference).with_reverse(name("org.example.wrote"))
		)),
		Err(DefinitionError::OtherReverse {
			field: author.clone(),
			known: name("org.example.author-of"),
		})
	);
}

#[test]
fn a_field_of_codes_is_text_and_keeps_its_mark() {
	let isbn = name("org.example.isbn");
	let edition = TypeDef::new(TypeName::from_static("org.example.edition"), 1)
		.with_field(isbn.clone(), FieldDef::new(ValueKind::Text).many().code());
	let mut rng = TestRng::new("codes in a space");
	let mut state = State::new();
	let operations = Types::new().define(&edition, &mut rng);
	apply(&mut state, &operations, 1);
	let types = read(&state);
	assert!(types.field(&isbn).expect("defined").is_code());
	assert_eq!(types.get(edition.name()), Some(&edition));
	nibkeep_testkit::assert_empty!(types.define(&edition, &mut rng));
	// Listing the field without the mark is no less careful.
	let listed = TypeDef::new(TypeName::from_static("org.example.book"), 1)
		.with_field(isbn.clone(), FieldDef::new(ValueKind::Text).many());
	assert_eq!(types.check(&listed), Ok(()));

	let pages = name("org.example.pages");
	let numbered = TypeDef::new(TypeName::from_static("org.example.book"), 1)
		.with_field(pages.clone(), FieldDef::new(ValueKind::Integer).code());
	assert_eq!(
		Types::new().check(&numbered),
		Err(DefinitionError::NotText(pages))
	);
}

fn tag(text: &str) -> nibkeep_model::Tag {
	nibkeep_model::Tag::parse(text).expect("a language tag")
}

/// A book whose type and title have labels.
fn labelled_book() -> TypeDef {
	TypeDef::new(BOOK, 1)
		.with_label(Some(&tag("en")), "Book")
		.with_label(Some(&tag("sv")), "Bok")
		.with_field(
			name("org.example.title"),
			FieldDef::new(ValueKind::Text)
				.with_label(Some(&tag("en")), "Title")
				.with_label(None, "Titel?"),
		)
		.with_field(
			name("org.example.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		)
}

#[test]
fn labels_are_defined_in_a_space_and_read_back() {
	let mut rng = TestRng::new("labels");
	let mut state = State::new();
	let operations = Types::new().define(&labelled_book(), &mut rng);
	// Each label is a value of its own, in its language.
	let labels: Vec<(String, Option<String>)> = operations
		.iter()
		.filter_map(|operation| match operation {
			Operation::Assert { statement, .. }
				if [fields::TYPE_LABEL, fields::FIELD_LABEL].contains(&statement.field) =>
			{
				Some((
					statement.value.as_text()?.to_owned(),
					statement.qualifiers.language().map(str::to_owned),
				))
			}
			_ => None,
		})
		.collect();
	assert_eq!(labels.len(), 4, "{labels:?}");
	assert!(labels.contains(&("Bok".to_owned(), Some("sv".to_owned()))));
	assert!(labels.contains(&("Titel?".to_owned(), None)));
	apply(&mut state, &operations, 1);
	let types = read(&state);
	assert_eq!(types.get(&BOOK), Some(&labelled_book()));
	nibkeep_testkit::assert_empty!(types.define(&labelled_book(), &mut rng));
}

#[test]
fn a_label_in_another_language_is_added_and_one_in_a_known_language_is_not() {
	let mut rng = TestRng::new("more labels");
	let mut state = State::new();
	apply(
		&mut state,
		&Types::new().define(&labelled_book(), &mut rng),
		1,
	);
	let known = read(&state);
	let title = name("org.example.title");
	let more = TypeDef::new(BOOK, 1)
		.with_label(Some(&tag("en")), "Volume")
		.with_label(Some(&tag("ru")), "Книга")
		.with_field(
			title.clone(),
			FieldDef::new(ValueKind::Text)
				.with_label(Some(&tag("en")), "Heading")
				.with_label(Some(&tag("ru")), "Название"),
		);
	assert_eq!(known.check(&more), Ok(()));
	let operations = known.define(&more, &mut rng);
	apply(&mut state, &operations, 2);
	let types = read(&state);
	let book = types.get(&BOOK).expect("the book");
	assert_eq!(book.labels().get(Some(&tag("en"))), Some("Book"));
	assert_eq!(book.labels().get(Some(&tag("ru"))), Some("Книга"));
	let title = book.field(&title).expect("the title");
	assert_eq!(title.labels().get(Some(&tag("en"))), Some("Title"));
	assert_eq!(title.labels().get(Some(&tag("ru"))), Some("Название"));
	assert_eq!(title.labels().get(None), Some("Titel?"));
	// No new version: labels change only how the type is shown.
	assert_eq!(book.version(), 1);
	nibkeep_testkit::assert_empty!(types.define(&more, &mut rng));
}

#[test]
fn labels_in_code_count_first_and_a_space_adds_languages() {
	let mut rng = TestRng::new("labels in code");
	let in_code = TypeDef::new(BOOK, 1).with_label(Some(&tag("en")), "Book");
	let in_space = TypeDef::new(BOOK, 1)
		.with_label(Some(&tag("en")), "Novel")
		.with_label(Some(&tag("sv")), "Bok");
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&in_space, &mut rng), 1);
	let mut types = Types::new();
	types.read(&state).infallible();
	types.add(in_code);
	let book = types.get(&BOOK).expect("the book");
	assert_eq!(book.labels().get(Some(&tag("en"))), Some("Book"));
	assert_eq!(book.labels().get(Some(&tag("sv"))), Some("Bok"));
}

#[test]
fn of_competing_labels_in_one_language_the_smallest_statement_counts() {
	let mut state = State::new();
	let label = |id: u16, text: &str| {
		Operation::assert(
			statement(
				id,
				1,
				"org.nibkeep.type-label",
				Value::Text(text.to_owned()),
			)
			.with_qualifiers(
				nibkeep_model::Qualifiers::new()
					.with(fields::LANGUAGE, Value::Text("en".to_owned())),
			),
		)
	};
	let operations = [
		Operation::assert(statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		)),
		Operation::assert(statement(
			2,
			1,
			"org.nibkeep.type-name",
			Value::Text(BOOK.to_string()),
		)),
		label(4, "Second"),
		label(3, "First"),
		// A label that is not text, or that is empty, is passed over.
		Operation::assert(statement(5, 1, "org.nibkeep.type-label", Value::Integer(1))),
		Operation::assert(statement(
			6,
			1,
			"org.nibkeep.type-label",
			Value::Text(String::new()),
		)),
	];
	apply(&mut state, &operations, 1);
	let types = read(&state);
	let book = types.get(&BOOK).expect("the book");
	assert_eq!(book.labels().iter().collect::<Vec<_>>(), [("en", "First")]);
}

#[test]
fn a_space_cannot_redefine_the_fields_of_labels() {
	for field in [fields::TYPE_LABEL, fields::FIELD_LABEL] {
		assert!(fields::is_reserved(&field));
		assert_eq!(
			Types::new().check(
				&TypeDef::new(BOOK, 1).with_field(field.clone(), FieldDef::new(ValueKind::Text))
			),
			Err(DefinitionError::ReservedField(field))
		);
	}
}

#[test]
fn a_field_that_lacks_only_labels_gets_them_where_it_is_defined() {
	let mut rng = TestRng::new("labels added");
	let mut state = State::new();
	// As a space made before labels has it.
	apply(&mut state, &Types::new().define(&book(), &mut rng), 1);
	let known = read(&state);
	let operations = known.define(&labelled_book(), &mut rng);
	// One label of the type, and two of the title, on the entities that define them: no
	// new definition of a field.
	let asserted: Vec<&nibkeep_model::Statement> = operations
		.iter()
		.filter_map(|operation| match operation {
			Operation::Assert { statement, .. } => Some(statement),
			_ => None,
		})
		.collect();
	assert_eq!(asserted.len(), 4, "{asserted:?}");
	assert!(
		asserted.iter().all(|statement| {
			[fields::TYPE_LABEL, fields::FIELD_LABEL].contains(&statement.field)
		})
	);
	let title_entities: BTreeSet<_> = asserted
		.iter()
		.filter(|statement| statement.field == fields::FIELD_LABEL)
		.map(|statement| statement.entity)
		.collect();
	assert_eq!(title_entities.len(), 1);
	apply(&mut state, &operations, 2);
	assert_eq!(read(&state).get(&BOOK), Some(&labelled_book()));
}

#[test]
fn a_careful_definition_takes_what_the_space_defines_and_adds_only_what_it_lacks() {
	let mut rng = TestRng::new("careful");
	let title = name("org.example.title");
	let mut state = State::new();
	// The space made its title secret and many, and the book has no labels.
	let made_careful = TypeDef::new(BOOK, 1).with_field(
		title.clone(),
		FieldDef::new(ValueKind::Text).many().secret(),
	);
	apply(&mut state, &Types::new().define(&made_careful, &mut rng), 1);
	let known = read(&state);
	assert!(matches!(
		known.check(&labelled_book()),
		Err(DefinitionError::LessCareful { .. })
	));
	let careful = known.careful(&labelled_book());
	assert_eq!(known.check(&careful), Ok(()));
	let field = careful.field(&title).expect("the title");
	assert!(field.is_secret());
	assert_eq!(field.multiplicity(), Multiplicity::Many);
	assert_eq!(field.labels().get(Some(&tag("en"))), Some("Title"));
	apply(&mut state, &known.define(&careful, &mut rng), 2);
	let types = read(&state);
	let book = types.get(&BOOK).expect("the book");
	assert!(book.field(&title).is_some_and(FieldDef::is_secret));
	assert_eq!(book.labels().get(Some(&tag("sv"))), Some("Bok"));
	// A field of another kind is left for the check to refuse.
	let other_kind =
		TypeDef::new(BOOK, 1).with_field(title.clone(), FieldDef::new(ValueKind::Integer));
	assert!(matches!(
		types.check(&types.careful(&other_kind)),
		Err(DefinitionError::OtherKind { .. })
	));
}

#[test]
fn a_label_in_a_language_this_version_cannot_read_is_passed_over() {
	let mut state = State::new();
	let label = |id: u16, text: &str, language: Option<Value>| {
		let mut qualifiers = nibkeep_model::Qualifiers::new();
		if let Some(language) = language {
			qualifiers = qualifiers.with(fields::LANGUAGE, language);
		}
		Operation::assert(
			statement(
				id,
				1,
				"org.nibkeep.type-label",
				Value::Text(text.to_owned()),
			)
			.with_qualifiers(qualifiers),
		)
	};
	let operations = [
		Operation::assert(statement(
			1,
			1,
			"org.nibkeep.type",
			Value::Text(TYPE_DEFINITION.to_string()),
		)),
		Operation::assert(statement(
			2,
			1,
			"org.nibkeep.type-name",
			Value::Text(BOOK.to_string()),
		)),
		label(3, "Newer", Some(Value::Text("not a tag!".to_owned()))),
		label(4, "Odd", Some(Value::Integer(7))),
		label(5, "Plain", None),
	];
	apply(&mut state, &operations, 1);
	let types = read(&state);
	let book = types.get(&BOOK).expect("the book");
	assert_eq!(book.labels().iter().collect::<Vec<_>>(), [("und", "Plain")]);
}

#[test]
fn a_careful_definition_takes_no_labels_from_other_types() {
	let mut rng = TestRng::new("labels of others");
	let shared = name("org.example.code");
	let other = TypeDef::new(TypeName::from_static("org.example.card"), 1).with_field(
		shared.clone(),
		FieldDef::new(ValueKind::Text).with_label(Some(&tag("en")), "Login"),
	);
	let mut state = State::new();
	apply(&mut state, &Types::new().define(&other, &mut rng), 1);
	let known = read(&state);
	let mine = TypeDef::new(BOOK, 1).with_field(
		shared.clone(),
		FieldDef::new(ValueKind::Text).with_label(Some(&tag("en")), "User name"),
	);
	let careful = known.careful(&mine);
	assert_eq!(
		careful
			.field(&shared)
			.expect("listed")
			.labels()
			.get(Some(&tag("en"))),
		Some("User name")
	);
	apply(&mut state, &known.define(&careful, &mut rng), 2);
	let types = read(&state);
	let book = types.get(&BOOK).expect("the book");
	assert_eq!(
		book.field(&shared)
			.expect("listed")
			.labels()
			.get(Some(&tag("en"))),
		Some("User name")
	);
}

/// The statements that operations assert.
fn asserted(operations: &[Operation]) -> Vec<&nibkeep_model::Statement> {
	operations
		.iter()
		.filter_map(|operation| match operation {
			Operation::Assert { statement, .. } => Some(statement),
			_ => None,
		})
		.collect()
}

#[test]
fn devices_that_define_a_type_at_once_write_one_definition() {
	let on_phone = Types::new().define(&labelled_book(), &mut TestRng::new("phone"));
	let on_laptop = Types::new().define(&labelled_book(), &mut TestRng::new("laptop"));
	assert_eq!(on_phone, on_laptop);
	let mut state = State::new();
	apply(&mut state, &on_phone, 1);
	apply(&mut state, &on_laptop, 2);
	assert_eq!(state.statements().infallible().len(), on_phone.len());
	// So do labels added later, and fields.
	let known = read(&state);
	let grown = labelled_book()
		.with_label(Some(&tag("de")), "Buch")
		.with_field(name("org.example.year"), FieldDef::new(ValueKind::Date));
	let on_phone = known.define(&grown, &mut TestRng::new("phone"));
	assert_eq!(
		on_phone.len(),
		5,
		"a label, and a field's type, of, name and kind"
	);
	assert_eq!(on_phone, known.define(&grown, &mut TestRng::new("laptop")));
}

#[test]
fn definitions_take_ids_derived_from_what_they_state() {
	let operations = Types::new().define(&book(), &mut TestRng::new("derived"));
	let book_entity = schema::type_entity(&BOOK);
	let title = name("org.example.title");
	let title_entity = schema::field_entity(book_entity, &title, &FieldDef::new(ValueKind::Text));
	let statements = asserted(&operations);
	let type_name = statements
		.iter()
		.find(|statement| statement.field == fields::TYPE_NAME)
		.expect("the type's name");
	assert_eq!(type_name.entity, book_entity);
	assert_eq!(
		type_name.id,
		schema::statement_id(
			book_entity,
			&fields::TYPE_NAME,
			&Value::Text("org.example.book".to_owned()),
			&nibkeep_model::Qualifiers::new()
		)
	);
	assert!(
		statements
			.iter()
			.any(|statement| statement.entity == title_entity
				&& statement.field == fields::FIELD_NAME)
	);
	// The vectors of `docs/spec/model.md`, section 8.
	assert_eq!(
		book_entity.to_string(),
		"d8342faa-8153-4ebd-b914-83600894c2af"
	);
	assert_eq!(
		title_entity.to_string(),
		"a19680f7-9406-4082-8828-97f7265347ba"
	);
	assert_eq!(
		type_name.id.to_string(),
		"56dba4a9-d0b7-425d-936a-4fbb4c86ecdd"
	);
}

#[test]
fn an_id_that_the_space_knows_already_is_drawn_at_random() {
	let mut rng = TestRng::new("known");
	let mut state = State::new();
	let first = Types::new().define(&labelled_book(), &mut rng);
	assert_eq!(
		Types::new()
			.define_in(&labelled_book(), &State::new(), &mut rng)
			.infallible(),
		first,
		"in a space that knows none of them, the IDs that defining it anywhere derives"
	);
	apply(&mut state, &first, 1);
	// A person deletes the book's English label, and the author's definition.
	let statements = asserted(&first);
	let label = statements
		.iter()
		.find(|statement| statement.value == Value::Text("Book".to_owned()))
		.expect("the label");
	let author = statements
		.iter()
		.find(|statement| statement.value == Value::Text("org.example.author".to_owned()))
		.expect("the author's definition")
		.entity;
	apply(
		&mut state,
		&[
			Operation::retract(label.entity, [label.id]),
			Operation::assert(nibkeep_model::Statement::new(
				rng.statement_id(),
				author,
				fields::DELETED,
				Value::Boolean(true),
			)),
		],
		2,
	);
	// Defining the book again brings both back, under other IDs, as the derived ones are
	// taken.
	let again = read(&state)
		.define_in(&labelled_book(), &state, &mut rng)
		.infallible();
	let statements = asserted(&again);
	assert!(statements.iter().all(|statement| statement.id != label.id));
	assert!(
		statements
			.iter()
			.all(|statement| statement.entity != author)
	);
	apply(&mut state, &again, 3);
	let types = read(&state);
	assert_eq!(types.get(&BOOK), Some(&labelled_book()));
	nibkeep_testkit::assert_empty!(
		types
			.define_in(&labelled_book(), &state, &mut rng)
			.infallible()
	);
}
