//! Statements, their qualifiers, and the operations that change them.

use crate::common::{entity, field, statement, statement_id, text};
use nibkeep_base::cbor::{self, Map, ToCbor, Value as Cbor};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{Operation, Qualifiers, State, Statement, Value, fields};
use nibkeep_time::Timestamp;

/// The CBOR form of a text value.
fn text_cbor(text: &str) -> Cbor {
	Cbor::Array(vec![0_u64.to_cbor(), Cbor::from(text)])
}

#[test]
fn statements_have_their_cbor_form() {
	let statement = statement(1, 2, "org.nibkeep.name", text("Ada"))
		.with_qualifiers(Qualifiers::new().with(fields::CREDITED_AS, text("A. Lovelace")));
	let mut qualifiers = Map::new();
	qualifiers.insert("org.nibkeep.credited-as", text_cbor("A. Lovelace"));
	let expected = Cbor::Array(vec![
		statement_id(1).to_cbor(),
		entity(2).to_cbor(),
		Cbor::from("org.nibkeep.name"),
		text_cbor("Ada"),
		Cbor::from(qualifiers),
	]);
	assert_eq!(cbor::to_vec(&statement), cbor::encode(&expected));
	assert_eq!(
		cbor::from_slice::<Statement>(&cbor::encode(&expected)),
		Ok(statement)
	);
}

#[test]
fn operations_have_their_cbor_form() {
	let new_name = statement(3, 2, "org.nibkeep.name", text("Ada"));
	let assert = Operation::replace(new_name.clone(), [statement_id(2), statement_id(1)]);
	let expected = Cbor::Array(vec![
		3_u64.to_cbor(),
		new_name.to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor(), statement_id(2).to_cbor()]),
	]);
	assert_eq!(cbor::to_vec(&assert), cbor::encode(&expected));
	assert_eq!(
		cbor::from_slice::<Operation>(&cbor::encode(&expected)),
		Ok(assert)
	);

	// An assertion written before operations named their entity is read and written as it
	// was, superseding statements of any entity.
	let earlier = Cbor::Array(vec![
		0_u64.to_cbor(),
		new_name.to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor()]),
	]);
	let read = cbor::from_slice::<Operation>(&cbor::encode(&earlier));
	assert_eq!(
		read,
		Ok(Operation::Assert {
			statement: new_name.clone(),
			supersedes: [statement_id(1)].into(),
			any_entity: true,
		})
	);
	assert_eq!(
		read.map(|operation| cbor::to_vec(&operation)),
		Ok(cbor::encode(&earlier))
	);

	let retract = Operation::retract(entity(2), [statement_id(3)]);
	let expected = Cbor::Array(vec![
		2_u64.to_cbor(),
		entity(2).to_cbor(),
		Cbor::Array(vec![statement_id(3).to_cbor()]),
	]);
	assert_eq!(cbor::to_vec(&retract), cbor::encode(&expected));
	assert_eq!(
		cbor::from_slice::<Operation>(&cbor::encode(&expected)),
		Ok(retract)
	);

	// A retraction written before operations named their entity is read and written as it
	// was, naming none.
	let earlier = Cbor::Array(vec![
		1_u64.to_cbor(),
		Cbor::Array(vec![statement_id(3).to_cbor()]),
	]);
	let read = cbor::from_slice::<Operation>(&cbor::encode(&earlier));
	assert_eq!(
		read,
		Ok(Operation::Retract {
			entity: None,
			statements: [statement_id(3)].into(),
		})
	);
	assert_eq!(
		read.map(|operation| cbor::to_vec(&operation)),
		Ok(cbor::encode(&earlier))
	);

	// A retraction of secret statements names no entity, as theirs are sealed with them.
	let retract_secret = Operation::retract_secret([statement_id(4), statement_id(3)]);
	let expected = Cbor::Array(vec![
		4_u64.to_cbor(),
		Cbor::Array(vec![statement_id(3).to_cbor(), statement_id(4).to_cbor()]),
	]);
	assert_eq!(cbor::to_vec(&retract_secret), cbor::encode(&expected));
	assert_eq!(
		cbor::from_slice::<Operation>(&cbor::encode(&expected)),
		Ok(retract_secret.clone())
	);
	assert_eq!(retract_secret.entity(), None);
	assert!(!retract_secret.supersedes_any_entity());
}

#[test]
fn superseded_ids_must_be_sorted_and_unique() {
	let unsorted = Cbor::Array(vec![
		1_u64.to_cbor(),
		Cbor::Array(vec![statement_id(2).to_cbor(), statement_id(1).to_cbor()]),
	]);
	let repeated = Cbor::Array(vec![
		1_u64.to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor(), statement_id(1).to_cbor()]),
	]);
	// A known kind of operation in another form.
	let reshaped = Cbor::Array(vec![
		1_u64.to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor()]),
		Cbor::Null,
	]);
	let reshaped_new = Cbor::Array(vec![
		2_u64.to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor()]),
	]);
	let reshaped_secret = Cbor::Array(vec![
		4_u64.to_cbor(),
		entity(1).to_cbor(),
		Cbor::Array(vec![statement_id(1).to_cbor()]),
	]);
	for value in [unsorted, repeated, reshaped, reshaped_new, reshaped_secret] {
		assert!(
			cbor::from_slice::<Operation>(&cbor::encode(&value)).is_err(),
			"{value:?} was accepted"
		);
	}
}

#[test]
fn qualifier_names_must_be_valid() {
	let mut qualifiers = Map::new();
	qualifiers.insert("Not A Name", text_cbor("x"));
	let encoded = cbor::encode(&Cbor::Array(vec![
		statement_id(1).to_cbor(),
		entity(2).to_cbor(),
		Cbor::from("org.nibkeep.name"),
		text_cbor("Ada"),
		Cbor::from(qualifiers),
	]));
	assert!(cbor::from_slice::<Statement>(&encoded).is_err());
}

#[test]
fn any_value_can_carry_qualifiers() {
	let grandma = entity(7);
	let qualifiers = Qualifiers::new()
		.with(
			fields::VALID_TIME,
			Value::Date("1952/1953".parse().expect("valid")),
		)
		.with(fields::SOURCE, Value::Reference(grandma))
		.with(fields::CREDITED_AS, text("Granny"))
		.with(fields::CONFIDENCE, Value::Integer(60))
		.with(fields::PREFERRED, Value::Boolean(true))
		.with(field("org.example.genealogy.note"), text("told at Easter"));
	let values = [
		text("Stockholm"),
		Value::Integer(3),
		Value::Reference(entity(8)),
		Value::Date("1952".parse().expect("valid")),
	];
	for value in values {
		let statement = statement(1, 2, "org.example.genealogy.place", value)
			.with_qualifiers(qualifiers.clone());
		let decoded: Statement = cbor::from_slice(&cbor::to_vec(&statement)).expect("round trip");
		assert_eq!(decoded, statement);
		let qualifiers = &decoded.qualifiers;
		assert_eq!(
			qualifiers.valid_time().map(ToString::to_string),
			Some("1952/1953".to_owned())
		);
		assert_eq!(qualifiers.source(), Some(&Value::Reference(grandma)));
		assert_eq!(qualifiers.credited_as(), Some("Granny"));
		assert_eq!(qualifiers.confidence(), Some(60));
		assert!(qualifiers.is_preferred());
		assert_eq!(
			qualifiers.get(&field("org.example.genealogy.note")),
			Some(&text("told at Easter"))
		);
	}
}

#[test]
fn qualifiers_of_the_wrong_kind_are_kept_but_not_interpreted() {
	let qualifiers = Qualifiers::new()
		.with(fields::VALID_TIME, text("last summer"))
		.with(fields::CONFIDENCE, Value::Integer(150))
		.with(fields::PREFERRED, Value::Boolean(false));
	assert_eq!(qualifiers.valid_time(), None);
	assert_eq!(qualifiers.confidence(), None);
	assert!(!qualifiers.is_preferred());
	assert_eq!(
		qualifiers.get(&fields::VALID_TIME),
		Some(&text("last summer"))
	);
	assert_eq!(qualifiers.len(), 3);
}

#[test]
fn operations_of_unknown_kinds_survive_unchanged_and_have_no_effect() {
	let encoded = cbor::encode(&Cbor::Array(vec![
		7_u64.to_cbor(),
		Cbor::from("a newer kind of operation"),
		Cbor::Array(vec![statement_id(1).to_cbor()]),
	]));
	let operation: Operation = cbor::from_slice(&encoded).expect("kept");
	assert!(matches!(operation, Operation::Unknown(_)));
	assert_eq!(cbor::to_vec(&operation), encoded);
	let mut state = State::new();
	state.apply(&operation, Timestamp::new(1, 0)).infallible();
	assert_eq!(state, State::new());
}
