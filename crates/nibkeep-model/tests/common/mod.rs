//! Helpers shared by the tests.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use nibkeep_base::{EntityId, StatementId};
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_time::Timestamp;

/// Stand-in for random bytes: different for every `n`, and ordered like `n`.
fn bytes(n: u16) -> [u8; 16] {
	let mut bytes = [0; 16];
	bytes[..2].copy_from_slice(&n.to_be_bytes());
	bytes
}

/// A statement ID for tests. IDs sort like their numbers.
pub(crate) fn statement_id(n: u16) -> StatementId {
	StatementId::from_random_bytes(bytes(n))
}

/// An entity ID for tests. IDs sort like their numbers.
pub(crate) fn entity(n: u16) -> EntityId {
	EntityId::from_random_bytes(bytes(n))
}

/// A system time for tests.
pub(crate) fn at(millis: u64) -> Timestamp {
	Timestamp::new(millis, 0)
}

/// A field name for tests.
pub(crate) fn field(name: &str) -> FieldName {
	name.parse().expect("a valid field name")
}

/// A text value.
pub(crate) fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

/// Statement `id` that `entity`'s `field` has `value`.
pub(crate) fn statement(id: u16, entity_id: u16, field_name: &str, value: Value) -> Statement {
	Statement::new(
		statement_id(id),
		entity(entity_id),
		field(field_name),
		value,
	)
}

/// Asserts statement `id`, superseding the statements in `supersedes`.
pub(crate) fn set(
	id: u16,
	entity_id: u16,
	field_name: &str,
	value: Value,
	supersedes: &[u16],
) -> Operation {
	Operation::replace(
		statement(id, entity_id, field_name, value),
		supersedes.iter().map(|&n| statement_id(n)),
	)
}

/// The values of statements, for comparing them: borrowed, or read from storage.
pub(crate) fn values<S: std::borrow::Borrow<Statement>>(
	statements: impl IntoIterator<Item = S>,
) -> Vec<Value> {
	statements
		.into_iter()
		.map(|statement| statement.borrow().value.clone())
		.collect()
}
