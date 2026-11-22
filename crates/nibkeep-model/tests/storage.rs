//! The storage of a state: opened secret statements kept in a layer of their own answer as
//! if they were kept with the ordinary ones, and leave them as they were when forgotten.

mod common;

use std::collections::BTreeSet;
use std::convert::Infallible;

use common::{at, entity, field, statement, statement_id, text, values};
use nibkeep_base::StatementId;
use nibkeep_model::storage::{AsOf, Infallibly, Layers, Memory, Origin, Overlay, Statements};
use nibkeep_model::{Operation, State, Statement, Value};
use nibkeep_time::Timestamp;
use proptest::prelude::*;

const NAME: &str = "org.nibkeep.name";

const FIELDS: [&str; 4] = [
	NAME,
	"org.example.x",
	"org.nibkeep.merged-into",
	"org.nibkeep.deleted",
];

/// A value that suits a field, for merges and deletion to happen.
fn value_for(field_name: &str, n: u16) -> Value {
	match field_name {
		"org.nibkeep.merged-into" => Value::Reference(entity(n)),
		"org.nibkeep.deleted" => Value::Boolean(n > 0),
		_ => Value::Integer(i64::from(n)),
	}
}

/// A random ordinary operation: it asserts statements 0 to 7, and supersedes any of 0 to
/// 11, the secret ones too, some with a retraction of secret statements.
fn ordinary() -> impl Strategy<Value = (Operation, Timestamp)> {
	(
		any::<bool>(),
		0..8_u16,
		0..3_u16,
		prop::sample::select(FIELDS.to_vec()),
		0..3_u16,
		prop::collection::btree_set(0..12_u16, 0..3),
		0..6_u64,
	)
		.prop_map(
			|(assert, id, entity_n, field_name, value, superseded, millis)| {
				let superseded = superseded.into_iter().map(statement_id);
				let operation = if assert {
					Operation::replace(
						statement(id, entity_n, field_name, value_for(field_name, value)),
						superseded,
					)
				} else if value == 0 {
					// The earlier form, which supersedes statements of any entity.
					Operation::Retract {
						entity: None,
						statements: superseded.collect(),
					}
				} else if entity_n == 0 {
					Operation::retract_secret(superseded)
				} else {
					Operation::retract(entity(entity_n), superseded)
				};
				(operation, at(millis))
			},
		)
}

/// Random secret statements, 6 to 11: ordinary operations may assert 6 and 7 too, which
/// only a faulty device does.
fn secrets() -> impl Strategy<Value = Vec<(Statement, Timestamp)>> {
	prop::collection::btree_map(
		6..12_u16,
		(
			0..3_u16,
			prop::sample::select(FIELDS.to_vec()),
			0..3_u16,
			0..6_u64,
		),
		0..4,
	)
	.prop_map(|secrets| {
		secrets
			.into_iter()
			.map(|(id, (entity_n, field_name, value, millis))| {
				(
					statement(id, entity_n, field_name, value_for(field_name, value)),
					at(millis),
				)
			})
			.collect()
	})
}

/// An operation as it acts where the secret statements `secrets` are kept with the
/// ordinary ones: a retraction of secret statements supersedes those alone, of any entity.
fn as_if_ordinary(operation: &Operation, secrets: &BTreeSet<StatementId>) -> Operation {
	match operation {
		Operation::RetractSecret { statements } => Operation::Retract {
			entity: None,
			statements: statements.intersection(secrets).copied().collect(),
		},
		other => other.clone(),
	}
}

/// Checks that two states answer every query alike.
fn same_answers<A, B>(one: &State<A>, other: &State<B>) -> Result<(), TestCaseError>
where
	A: Statements<Error = Infallible>,
	B: Statements<Error = Infallible>,
{
	prop_assert_eq!(
		one.statements().infallible(),
		other.statements().infallible()
	);
	prop_assert_eq!(one.entities().infallible(), other.entities().infallible());
	prop_assert_eq!(
		one.storage().field_names().infallible(),
		other.storage().field_names().infallible()
	);
	for field in one.storage().field_names().infallible() {
		prop_assert_eq!(
			one.storage().current_in_field(&field).infallible(),
			other.storage().current_in_field(&field).infallible()
		);
	}
	prop_assert_eq!(
		one.storage().described().infallible(),
		other.storage().described().infallible()
	);
	for n in 0..3 {
		let entity = entity(n);
		prop_assert_eq!(
			one.canonical(entity).infallible(),
			other.canonical(entity).infallible()
		);
		prop_assert_eq!(
			one.merged(entity).infallible(),
			other.merged(entity).infallible()
		);
		prop_assert_eq!(
			one.is_deleted(entity).infallible(),
			other.is_deleted(entity).infallible()
		);
		prop_assert_eq!(
			one.fields(entity).infallible(),
			other.fields(entity).infallible()
		);
		prop_assert_eq!(
			one.referrers(entity).infallible(),
			other.referrers(entity).infallible()
		);
		for name in FIELDS {
			let field = field(name);
			prop_assert_eq!(
				one.referrers_in(entity, &field).infallible(),
				other.referrers_in(entity, &field).infallible()
			);
			prop_assert_eq!(
				one.current(entity, &field).infallible(),
				other.current(entity, &field).infallible()
			);
			prop_assert_eq!(
				one.last_written(entity, &field).infallible(),
				other.last_written(entity, &field).infallible()
			);
		}
	}
	for n in 0..12 {
		let id = statement_id(n);
		prop_assert_eq!(
			one.statement(id).infallible(),
			other.statement(id).infallible()
		);
		prop_assert_eq!(
			one.recorded(id).infallible(),
			other.recorded(id).infallible()
		);
		prop_assert_eq!(
			one.is_current(id).infallible(),
			other.is_current(id).infallible()
		);
	}
	Ok(())
}

proptest! {
	#[test]
	fn opened_secrets_answer_as_if_kept_with_the_ordinary_statements(
		operations in prop::collection::vec(ordinary(), 0..24),
		secrets in secrets(),
		opened_first in any::<bool>(),
	) {
		// A secret with the ID of an ordinary statement does not count, whatever arrived
		// first.
		let asserted: BTreeSet<StatementId> = operations
			.iter()
			.filter_map(|(operation, _)| match operation {
				Operation::Assert { statement, .. } => Some(statement.id),
				_ => None,
			})
			.collect();
		let counting: Vec<&(Statement, Timestamp)> = secrets
			.iter()
			.filter(|(secret, _)| !asserted.contains(&secret.id))
			.collect();
		let counting_ids: BTreeSet<StatementId> =
			counting.iter().map(|(secret, _)| secret.id).collect();
		let mut layered = State::in_storage(Layers::new(Memory::default()));
		let mut together = State::new();
		let mut alone = State::new();
		let open = |layered: &mut State<Layers<Memory>>| {
			for (secret, recorded) in &secrets {
				layered
					.storage_mut()
					.open(secret, Origin::new(*recorded, None));
			}
		};
		if opened_first {
			open(&mut layered);
		}
		for (operation, recorded) in &operations {
			layered.apply(operation, *recorded).infallible();
			together
				.apply(&as_if_ordinary(operation, &counting_ids), *recorded)
				.infallible();
			alone
				.apply(&as_if_ordinary(operation, &BTreeSet::new()), *recorded)
				.infallible();
		}
		if !opened_first {
			open(&mut layered);
		}
		for (secret, recorded) in &counting {
			together.apply(&Operation::assert(secret.clone()), *recorded).infallible();
		}
		same_answers(&layered, &together)?;

		// Forgetting one, and then the rest, leaves what the ordinary statements alone say.
		if let Some((first, _)) = secrets.first() {
			layered.storage_mut().forget(first.id);
			let mut remaining = counting_ids.clone();
			remaining.remove(&first.id);
			let mut fewer = State::new();
			for (operation, recorded) in &operations {
				fewer
					.apply(&as_if_ordinary(operation, &remaining), *recorded)
					.infallible();
			}
			for (secret, recorded) in &counting {
				if secret.id != first.id {
					fewer.apply(&Operation::assert(secret.clone()), *recorded).infallible();
				}
			}
			same_answers(&layered, &fewer)?;
		}
		layered.storage_mut().forget_all();
		same_answers(&layered, &alone)?;
	}
}

#[test]
fn an_opened_secret_with_the_id_of_an_ordinary_statement_does_not_count() {
	let mut state = State::in_storage(Layers::new(Memory::default()));
	state
		.apply(
			&Operation::assert(statement(1, 1, NAME, text("Ada"))),
			at(1),
		)
		.infallible();
	// A faulty device sealed a statement with the same ID, about another entity.
	state.storage_mut().open(
		&statement(1, 2, NAME, text("hunter2")),
		Origin::new(at(2), None),
	);
	state.storage_mut().open(
		&statement(2, 2, NAME, text("Grace")),
		Origin::new(at(2), None),
	);
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Ada")]
	);
	assert_eq!(
		values(state.current(entity(2), &field(NAME)).infallible()),
		[text("Grace")]
	);
	assert_eq!(state.recorded(statement_id(1)).infallible(), Some(at(1)));
	assert_eq!(
		state
			.entities()
			.infallible()
			.into_iter()
			.collect::<Vec<_>>(),
		[entity(1), entity(2)]
	);
	assert_eq!(state.statements().infallible().len(), 2);
}

/// A retraction of secret statements takes away the secret statements with its IDs, whether
/// they are opened before it or after, and never an ordinary statement with one of them,
/// which a member who sealed a statement under the ID of another's would otherwise take away
/// unseen. An overlay of the state shows it alike.
#[test]
fn a_retraction_of_secret_statements_takes_away_no_ordinary_statement() {
	let mut state = State::in_storage(Layers::new(Memory::default()));
	state
		.apply(
			&Operation::assert(statement(1, 1, NAME, text("Ada"))),
			at(1),
		)
		.infallible();
	state.storage_mut().open(
		&statement(2, 2, NAME, text("hunter2")),
		Origin::new(at(1), None),
	);
	// A faulty device sealed a statement under the ID of Ada's name.
	state.storage_mut().open(
		&statement(1, 2, NAME, text("staple")),
		Origin::new(at(1), None),
	);
	let retraction = Operation::retract_secret([statement_id(1), statement_id(2), statement_id(3)]);
	state.apply(&retraction, at(2)).infallible();
	state.storage_mut().open(
		&statement(3, 2, NAME, text("correct horse")),
		Origin::new(at(1), None),
	);
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Ada")]
	);
	assert_eq!(
		values(state.current(entity(2), &field(NAME)).infallible()),
		Vec::<Value>::new()
	);
	assert_eq!(
		state.storage().superseded_at(statement_id(3)).infallible(),
		Some(at(2))
	);

	// As it stood before a secret statement was recorded, it shows none, secret or not.
	assert!(state.storage().is_secret(statement_id(2)).infallible());
	assert!(
		!AsOf::new(state.storage(), 0)
			.is_secret(statement_id(2))
			.infallible()
	);
	assert!(!state.storage().is_secret(statement_id(1)).infallible());

	// An overlay takes away a secret statement of its base, and no ordinary one.
	state.storage_mut().open(
		&statement(4, 2, NAME, text("tr0ub4dor")),
		Origin::new(at(1), None),
	);
	let mut overlay = State::in_storage(Overlay::new(state.storage()));
	assert_eq!(
		values(overlay.current(entity(2), &field(NAME)).infallible()),
		[text("tr0ub4dor")]
	);
	overlay
		.apply(
			&Operation::retract_secret([statement_id(1), statement_id(4)]),
			at(3),
		)
		.infallible();
	assert_eq!(
		values(overlay.current(entity(2), &field(NAME)).infallible()),
		Vec::<Value>::new()
	);
	assert_eq!(
		values(overlay.current(entity(1), &field(NAME)).infallible()),
		[text("Ada")]
	);
}

proptest! {
	#[test]
	fn an_overlay_answers_as_if_written_to_its_base(
		before in prop::collection::vec(ordinary(), 0..16),
		after in prop::collection::vec(ordinary(), 0..16),
	) {
		let mut base = State::new();
		let mut together = State::new();
		for (operation, recorded) in &before {
			base.apply(operation, *recorded).infallible();
			together.apply(operation, *recorded).infallible();
		}
		let untouched = base.clone();
		let mut overlay = State::in_storage(Overlay::new(base.storage()));
		for (operation, recorded) in &after {
			overlay.apply(operation, *recorded).infallible();
			together.apply(operation, *recorded).infallible();
		}
		same_answers(&overlay, &together)?;
		drop(overlay);
		prop_assert_eq!(base, untouched, "the base is left as it was");
	}
}

/// A random operation whose statements are each one statement, whatever time asserts it:
/// its entity, field and value follow from its ID.
fn of_its_id() -> impl Strategy<Value = (Operation, Timestamp)> {
	(
		any::<bool>(),
		0..8_u16,
		prop::collection::btree_set(0..8_u16, 0..3),
		0..6_u64,
	)
		.prop_map(|(assert, id, superseded, millis)| {
			let superseded = superseded.into_iter().map(statement_id);
			let field_name = FIELDS[usize::from(id) % FIELDS.len()];
			let operation = if assert {
				Operation::replace(
					statement(id, id % 3, field_name, value_for(field_name, id % 3)),
					superseded,
				)
			} else {
				Operation::retract(entity(id % 3), superseded)
			};
			(operation, at(millis))
		})
}

proptest! {
	#[test]
	fn the_state_as_of_a_time_is_what_the_operations_until_then_build(
		operations in prop::collection::vec(of_its_id(), 0..24),
	) {
		let mut now = State::new();
		for (operation, recorded) in &operations {
			now.apply(operation, *recorded).infallible();
		}
		for millis in 0..7 {
			let mut then = State::new();
			for (operation, recorded) in &operations {
				if recorded.millis() <= millis {
					then.apply(operation, *recorded).infallible();
				}
			}
			let as_of = State::in_storage(AsOf::new(now.storage(), millis));
			same_answers(&as_of, &then)?;
		}
	}
}

#[test]
fn an_entity_whose_statements_were_all_replaced_is_still_described() {
	let mut state = State::new();
	state
		.apply(
			&common::set(1, 1, "org.example.note", text("Milk"), &[]),
			at(1),
		)
		.infallible();
	state
		.apply(&Operation::retract(entity(1), [statement_id(1)]), at(2))
		.infallible();
	nibkeep_testkit::assert_empty!(state.storage().entities().infallible());
	assert_eq!(state.storage().described().infallible(), [entity(1)]);
	let then = State::in_storage(AsOf::new(state.storage(), 0));
	nibkeep_testkit::assert_empty!(then.storage().described().infallible());
}
