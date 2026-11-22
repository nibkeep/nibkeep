//! The current values of a space: concurrent writes, last writer wins, valid time, types,
//! deletion and merges.

mod common;

use nibkeep_model::storage::{Infallibly, Statements};
use std::collections::BTreeSet;

use common::{at, entity, field, set, statement, statement_id, text, values};
use nibkeep_base::cbor;
use nibkeep_model::{
	FieldDef, Operation, Qualifiers, Resolution, State, Statement, TypeName, Value, ValueKind,
	fields,
};
use nibkeep_time::{Day, Presence, Timestamp};
use proptest::prelude::*;

const NAME: &str = "org.nibkeep.name";

/// Applies operations, each with the system time it was recorded at.
fn apply(operations: &[(&Operation, u64)]) -> State {
	let mut state = State::new();
	for &(operation, millis) in operations {
		state.apply(operation, at(millis)).infallible();
	}
	state
}

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

#[test]
fn setting_a_field_replaces_the_values_the_writer_saw() {
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada Byron"), &[]), 1),
		(&set(2, 1, NAME, text("Ada King"), &[1]), 2),
	]);
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Ada King")]
	);
	assert!(!state.is_current(statement_id(1)).infallible());
	// Superseded statements stay, as history.
	assert_eq!(
		state
			.statement(statement_id(1))
			.infallible()
			.map(|statement| statement.into_owned().value),
		Some(text("Ada Byron"))
	);
}

#[test]
fn concurrent_writes_keep_both_values() {
	let first = set(1, 1, NAME, text("Grandma"), &[]);
	let phone = set(2, 1, NAME, text("Granny"), &[1]);
	let laptop = set(3, 1, NAME, text("Gran"), &[1]);
	let one = FieldDef::new(ValueKind::Text);
	for order in [
		[(&first, 1), (&phone, 2), (&laptop, 3)],
		[(&first, 1), (&laptop, 3), (&phone, 2)],
		[(&laptop, 3), (&phone, 2), (&first, 1)],
	] {
		let state = apply(&order);
		assert_eq!(
			values(state.current(entity(1), &field(NAME)).infallible()),
			[text("Granny"), text("Gran")]
		);
		let Resolution::Competing(competing) = state
			.resolve(entity(1), &field(NAME), Some(&one))
			.infallible()
		else {
			panic!("the values compete");
		};
		assert_eq!(values(competing), [text("Granny"), text("Gran")]);
	}
}

#[test]
fn a_write_that_saw_both_values_resolves_the_conflict() {
	let state = apply(&[
		(&set(1, 1, NAME, text("Grandma"), &[]), 1),
		(&set(2, 1, NAME, text("Granny"), &[1]), 2),
		(&set(3, 1, NAME, text("Gran"), &[1]), 3),
		(&set(4, 1, NAME, text("Granny"), &[2, 3]), 4),
	]);
	let one = FieldDef::new(ValueKind::Text);
	assert_eq!(
		state
			.resolve(entity(1), &field(NAME), Some(&one))
			.infallible(),
		Resolution::Values(state.current(entity(1), &field(NAME)).infallible())
	);
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Granny")]
	);
}

#[test]
fn adding_values_keeps_the_others() {
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
		(&set(2, 1, NAME, text("The Enchantress of Numbers"), &[]), 2),
	]);
	let many = FieldDef::new(ValueKind::Text).many();
	let Resolution::Values(current) = state
		.resolve(entity(1), &field(NAME), Some(&many))
		.infallible()
	else {
		panic!("many values do not compete");
	};
	assert_eq!(current.len(), 2);
}

#[test]
fn retracting_removes_a_value() {
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
		(&Operation::retract(entity(1), [statement_id(1)]), 2),
	]);
	nibkeep_testkit::assert_empty!(state.current(entity(1), &field(NAME)).infallible());
}

#[test]
fn a_statement_superseded_before_it_arrives_never_becomes_current() {
	let state = apply(&[
		(&Operation::retract(entity(1), [statement_id(1)]), 2),
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
	]);
	nibkeep_testkit::assert_empty!(state.current(entity(1), &field(NAME)).infallible());
}

/// An operation names the one entity it changes, so that every replica can tell what it
/// changes from the operation alone: a retraction, or an assertion, supersedes only the
/// statements of its own entity, whichever arrives first.
#[test]
fn an_operation_supersedes_statements_of_its_own_entity_alone() {
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let charles = set(2, 2, NAME, text("Charles"), &[]);
	let retraction = Operation::retract(entity(1), [statement_id(1), statement_id(2)]);
	let replacement = set(3, 1, NAME, text("Ada King"), &[1, 2]);
	for removing in [&retraction, &replacement] {
		for state in [
			apply(&[(&ada, 1), (&charles, 1), (removing, 2)]),
			apply(&[(removing, 2), (&ada, 1), (&charles, 1)]),
		] {
			assert!(!state.is_current(statement_id(1)).infallible());
			assert_eq!(
				values(state.current(entity(2), &field(NAME)).infallible()),
				[text("Charles")],
				"another entity's statement stays"
			);
		}
	}
}

/// A retraction written before operations named their entity, in the form `[1, superseded]`,
/// supersedes statements of any entity, as it did.
#[test]
fn a_retraction_of_the_earlier_form_supersedes_statements_of_any_entity() {
	let earlier = Operation::Retract {
		entity: None,
		statements: [statement_id(1), statement_id(2)].into(),
	};
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
		(&set(2, 2, NAME, text("Charles"), &[]), 1),
		(&earlier, 2),
	]);
	nibkeep_testkit::assert_empty!(state.current(entity(1), &field(NAME)).infallible());
	nibkeep_testkit::assert_empty!(state.current(entity(2), &field(NAME)).infallible());
}

/// An assertion written before operations named their entity, in the form
/// `[0, statement, superseded]`, supersedes statements of any entity, as it did.
#[test]
fn an_assertion_of_the_earlier_form_supersedes_statements_of_any_entity() {
	let earlier = Operation::Assert {
		statement: statement(3, 1, NAME, text("Ada King")),
		supersedes: [statement_id(1), statement_id(2)].into(),
		any_entity: true,
	};
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
		(&set(2, 2, NAME, text("Ada (merged)"), &[]), 1),
		(&earlier, 2),
	]);
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Ada King")]
	);
	nibkeep_testkit::assert_empty!(state.current(entity(2), &field(NAME)).infallible());
}

/// Of two statements with one ID, which only a faulty replica writes, the one that counts
/// is superseded by the operations about its own entity, whatever order they arrive in.
#[test]
fn a_statement_with_two_entities_is_superseded_alike_in_any_order() {
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let impostor = set(1, 2, NAME, text("Impostor"), &[]);
	let retraction = Operation::retract(entity(2), [statement_id(1)]);
	let orders = [
		[(&ada, 1), (&impostor, 2), (&retraction, 3)],
		[(&retraction, 3), (&ada, 1), (&impostor, 2)],
		[(&impostor, 2), (&retraction, 3), (&ada, 1)],
		[(&ada, 1), (&retraction, 3), (&impostor, 2)],
	];
	let states: Vec<State> = orders.iter().map(|order| apply(order)).collect();
	for state in &states[1..] {
		assert_eq!(state, &states[0]);
		assert_eq!(
			state.is_current(statement_id(1)).infallible(),
			states[0].is_current(statement_id(1)).infallible()
		);
	}
}

#[test]
fn last_writer_wins_fields_show_the_value_recorded_last() {
	let last_opened = "org.example.notes.last-opened";
	let latest = FieldDef::new(ValueKind::Integer).last_writer_wins();
	let state = apply(&[
		(&set(1, 1, last_opened, Value::Integer(100), &[]), 9),
		(&set(2, 1, last_opened, Value::Integer(50), &[]), 5),
	]);
	assert_eq!(
		state
			.resolve(entity(1), &field(last_opened), Some(&latest))
			.infallible(),
		Resolution::Values(vec![
			state
				.statement(statement_id(1))
				.infallible()
				.expect("recorded")
		])
	);
	// At the same system time, the greater statement ID wins.
	let state = apply(&[
		(&set(4, 1, last_opened, Value::Integer(100), &[]), 5),
		(&set(3, 1, last_opened, Value::Integer(50), &[]), 5),
	]);
	assert_eq!(
		values(
			match state
				.resolve(entity(1), &field(last_opened), Some(&latest))
				.infallible()
			{
				Resolution::Values(values) | Resolution::Competing(values) => values,
			}
		),
		[Value::Integer(100)]
	);
	assert_eq!(state.recorded(statement_id(4)).infallible(), Some(at(5)));
}

#[test]
fn fields_that_no_type_defines_show_all_their_values() {
	let state = apply(&[
		(&set(1, 1, "org.example.x", text("a"), &[]), 1),
		(&set(2, 1, "org.example.x", text("b"), &[]), 2),
	]);
	let Resolution::Values(current) = state
		.resolve(entity(1), &field("org.example.x"), None)
		.infallible()
	else {
		panic!("unknown fields show all values");
	};
	assert_eq!(current.len(), 2);
}

#[test]
fn two_statements_with_one_id_resolve_the_same_way_in_any_order() {
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let impostor = set(1, 2, NAME, text("Impostor"), &[]);
	let first = apply(&[(&ada, 1), (&impostor, 2)]);
	let second = apply(&[(&impostor, 2), (&ada, 1)]);
	assert_eq!(first, second);
	// The statement with the smaller encoding counts.
	let encoded = |operation: &Operation| match operation {
		Operation::Assert { statement, .. } => cbor::to_vec(statement),
		_ => unreachable!("both are assertions"),
	};
	let expected = if encoded(&ada) < encoded(&impostor) {
		(entity(1), entity(2))
	} else {
		(entity(2), entity(1))
	};
	assert_eq!(
		values(first.current(expected.0, &field(NAME)).infallible()).len(),
		1
	);
	nibkeep_testkit::assert_empty!(first.current(expected.1, &field(NAME)).infallible());
}

#[test]
fn a_statement_recorded_twice_counts_from_the_earliest_time() {
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let state = apply(&[(&ada, 7), (&ada, 3), (&ada, 5)]);
	assert_eq!(state.recorded(statement_id(1)).infallible(), Some(at(3)));
}

#[test]
fn every_change_that_asserted_a_statement_is_kept() {
	// As two devices that run one migration at once both do: a purge takes every one.
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let changes = [b"a", b"b", b"c"].map(|seed| nibkeep_base::Hash::of(seed));
	let made = [(0, 7), (1, 3), (2, 5)];
	for order in [[0, 1, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
		let mut state = State::new();
		for index in order {
			let (change, millis) = made[index];
			state
				.apply_in(&ada, at(millis), Some(changes[change]))
				.infallible();
			// Applied again, it adds nothing.
			state
				.apply_in(&ada, at(millis), Some(changes[change]))
				.infallible();
		}
		let recorded = state.storage().get(statement_id(1)).infallible();
		assert_eq!(
			recorded.and_then(|recorded| recorded.change),
			Some(changes[1])
		);
		let mut again = vec![changes[0], changes[2]];
		again.sort();
		assert_eq!(
			state.storage().asserted_again(statement_id(1)).infallible(),
			again,
			"{order:?}"
		);
	}
}

#[test]
fn a_change_that_asserted_another_statement_under_the_id_is_kept_too() {
	// Only a faulty replica writes two statements under one ID: a purge of the ID takes both
	// changes, so that neither statement comes back.
	let ada = set(1, 1, NAME, text("Ada"), &[]);
	let other = set(1, 1, NAME, text("Ada Lovelace"), &[]);
	let (first, second) = (nibkeep_base::Hash::of(b"a"), nibkeep_base::Hash::of(b"b"));
	let mut both = vec![first, second];
	both.sort();
	for order in [
		[(&ada, first), (&other, second)],
		[(&other, second), (&ada, first)],
	] {
		let mut state = State::new();
		for (operation, change) in order {
			state.apply_in(operation, at(1), Some(change)).infallible();
		}
		let storage = state.storage();
		let mut all = storage.asserted_again(statement_id(1)).infallible();
		all.extend(
			storage
				.get(statement_id(1))
				.infallible()
				.and_then(|recorded| recorded.change),
		);
		all.sort();
		assert_eq!(all, both);
	}
}

#[test]
fn values_on_a_day_follow_their_valid_time() {
	let valid = |edtf: &str| {
		Qualifiers::new().with(
			fields::VALID_TIME,
			Value::Date(edtf.parse().expect("valid")),
		)
	};
	let byron =
		statement(1, 1, NAME, text("Ada Byron")).with_qualifiers(valid("1815-12-10/1835-07-08"));
	let king = statement(2, 1, NAME, text("Ada King")).with_qualifiers(valid("1835-07-08/.."));
	let nickname = statement(3, 1, NAME, text("The Enchantress of Numbers"));
	let state = apply(&[
		(&Operation::assert(byron), 1),
		(&Operation::assert(king), 2),
		(&Operation::assert(nickname), 3),
	]);
	let on = |date: &str| -> Vec<(Value, Presence)> {
		state
			.current_on(entity(1), &field(NAME), day(date))
			.infallible()
			.into_iter()
			.map(|(statement, presence)| (statement.value.clone(), presence))
			.collect()
	};
	assert_eq!(
		on("1820-01-01"),
		[
			(text("Ada Byron"), Presence::Yes),
			(text("The Enchantress of Numbers"), Presence::DateUnknown),
		]
	);
	assert_eq!(
		on("1840-01-01"),
		[
			(text("Ada King"), Presence::Yes),
			(text("The Enchantress of Numbers"), Presence::DateUnknown),
		]
	);
	assert_eq!(on("1835-07-08").len(), 3);
}

#[test]
fn types_and_deletion_are_statements() {
	let mut state = apply(&[
		(&set(1, 1, "org.nibkeep.type", text("std.person"), &[]), 1),
		(
			&set(2, 1, "org.nibkeep.type", text("org.example.food.chef"), &[]),
			2,
		),
		// Text that is not a type name is kept but is not a type.
		(&set(3, 1, "org.nibkeep.type", text("Chef!"), &[]), 3),
	]);
	assert_eq!(
		state.types(entity(1)).infallible(),
		BTreeSet::from([
			TypeName::from_static("org.example.food.chef"),
			TypeName::from_static("std.person"),
		])
	);
	assert!(!state.is_deleted(entity(1)).infallible());
	state
		.apply(
			&set(4, 1, "org.nibkeep.deleted", Value::Boolean(true), &[]),
			at(4),
		)
		.infallible();
	assert!(state.is_deleted(entity(1)).infallible());
	// Undoing the deletion retracts its statement.
	state
		.apply(&Operation::retract(entity(1), [statement_id(4)]), at(5))
		.infallible();
	assert!(!state.is_deleted(entity(1)).infallible());
}

#[test]
fn merged_entities_show_together() {
	let author = "org.nibkeep.author";
	let mut state = apply(&[
		(&set(1, 1, NAME, text("Daniel Abraham"), &[]), 1),
		(&set(2, 2, NAME, text("D. Abraham"), &[]), 2),
		// A book refers to the duplicate.
		(&set(3, 3, author, Value::Reference(entity(2)), &[]), 3),
	]);
	// Entity 2 turns out to be the same person, and is merged into entity 1.
	state
		.apply(
			&set(
				4,
				2,
				"org.nibkeep.merged-into",
				Value::Reference(entity(1)),
				&[],
			),
			at(4),
		)
		.infallible();
	assert_eq!(state.canonical(entity(2)).infallible(), entity(1));
	assert_eq!(state.canonical(entity(1)).infallible(), entity(1));
	assert_eq!(
		state.merged(entity(2)).infallible(),
		BTreeSet::from([entity(1), entity(2)])
	);
	for id in [entity(1), entity(2)] {
		assert_eq!(
			values(state.current(id, &field(NAME)).infallible()),
			[text("Daniel Abraham"), text("D. Abraham")]
		);
		assert_eq!(
			state
				.referrers(id)
				.infallible()
				.iter()
				.map(|statement| statement.entity)
				.collect::<Vec<_>>(),
			[entity(3)]
		);
		assert_eq!(
			state
				.referrers_in(id, &field(author))
				.infallible()
				.iter()
				.map(|statement| statement.entity)
				.collect::<Vec<_>>(),
			[entity(3)]
		);
		nibkeep_testkit::assert_empty!(state.referrers_in(id, &field(NAME)).infallible());
	}

	// Undoing the merge separates them again.
	state
		.apply(&Operation::retract(entity(2), [statement_id(4)]), at(5))
		.infallible();
	assert_eq!(state.canonical(entity(2)).infallible(), entity(2));
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Daniel Abraham")]
	);
	nibkeep_testkit::assert_empty!(state.referrers(entity(1)).infallible());
}

#[test]
fn a_deleted_duplicate_does_not_delete_the_entity_it_is_merged_into() {
	let state = apply(&[
		(&set(1, 1, NAME, text("Ada"), &[]), 1),
		(&set(2, 2, NAME, text("Ada (duplicate)"), &[]), 1),
		(
			&set(3, 2, "org.nibkeep.deleted", Value::Boolean(true), &[]),
			2,
		),
		(
			&set(
				4,
				2,
				"org.nibkeep.merged-into",
				Value::Reference(entity(1)),
				&[],
			),
			3,
		),
	]);
	assert!(!state.is_deleted(entity(1)).infallible());
	assert!(
		!state.is_deleted(entity(2)).infallible(),
		"it stands for entity 1 now"
	);
	// Each entity's own record keeps its own deletion and merge.
	nibkeep_testkit::assert_empty!(state.current(entity(1), &fields::DELETED).infallible());
	nibkeep_testkit::assert_empty!(state.current(entity(1), &fields::MERGED_INTO).infallible());
	assert_eq!(
		state
			.current(entity(2), &fields::MERGED_INTO)
			.infallible()
			.len(),
		1
	);
	// Deleting the canonical entity deletes what it stands for.
	let mut state = state;
	state
		.apply(
			&set(5, 1, "org.nibkeep.deleted", Value::Boolean(true), &[]),
			at(4),
		)
		.infallible();
	assert!(state.is_deleted(entity(2)).infallible());
}

#[test]
fn merge_loops_resolve_to_the_smallest_id() {
	let merged_into = "org.nibkeep.merged-into";
	let state = apply(&[
		(&set(1, 1, merged_into, Value::Reference(entity(2)), &[]), 1),
		(&set(2, 2, merged_into, Value::Reference(entity(1)), &[]), 1),
		(&set(3, 3, merged_into, Value::Reference(entity(2)), &[]), 1),
	]);
	for id in [1, 2, 3] {
		assert_eq!(state.canonical(entity(id)).infallible(), entity(1));
	}
	assert_eq!(
		state.merged(entity(3)).infallible(),
		BTreeSet::from([entity(1), entity(2), entity(3)])
	);
}

#[test]
fn of_several_merges_the_smallest_target_counts() {
	let merged_into = "org.nibkeep.merged-into";
	let state = apply(&[
		(&set(1, 5, merged_into, Value::Reference(entity(4)), &[]), 1),
		(&set(2, 5, merged_into, Value::Reference(entity(3)), &[]), 1),
	]);
	assert_eq!(state.canonical(entity(5)).infallible(), entity(3));
}

#[test]
fn moving_statements_splits_an_entity() {
	let mut state = apply(&[
		(&set(1, 1, NAME, text("Ann (the aunt)"), &[]), 1),
		(&set(2, 1, NAME, text("Ann (the neighbour)"), &[]), 2),
	]);
	// The neighbour is someone else: her name moves to a new entity, and leaves the first,
	// as an operation supersedes only statements of its own entity.
	state
		.apply(&set(3, 2, NAME, text("Ann (the neighbour)"), &[2]), at(3))
		.infallible();
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()).len(),
		2,
		"an assertion on another entity leaves the statement"
	);
	state
		.apply(&Operation::retract(entity(1), [statement_id(2)]), at(3))
		.infallible();
	assert_eq!(
		values(state.current(entity(1), &field(NAME)).infallible()),
		[text("Ann (the aunt)")]
	);
	assert_eq!(
		values(state.current(entity(2), &field(NAME)).infallible()),
		[text("Ann (the neighbour)")]
	);
}

/// A random operation on a few statements, entities and fields, with a system time.
fn operation() -> impl Strategy<Value = (Operation, Timestamp)> {
	(
		any::<bool>(),
		0..8_u16,
		0..3_u16,
		prop::sample::select(vec![
			NAME,
			"org.example.x",
			"org.nibkeep.merged-into",
			"org.nibkeep.deleted",
		]),
		0..3_u16,
		prop::collection::btree_set(0..8_u16, 0..3),
		0..6_u64,
	)
		.prop_map(
			|(assert, id, entity_n, field_name, value, superseded, millis)| {
				let superseded = superseded.into_iter().map(statement_id);
				let operation = if assert {
					let value = match field_name {
						"org.nibkeep.merged-into" => Value::Reference(entity(value)),
						"org.nibkeep.deleted" => Value::Boolean(value > 0),
						_ => Value::Integer(i64::from(value)),
					};
					Operation::Assert {
						statement: Statement::new(
							statement_id(id),
							entity(entity_n),
							field(field_name),
							value,
						),
						supersedes: superseded.collect(),
						// Now and then of the earlier form.
						any_entity: id % 3 == 0,
					}
				} else if value == 0 {
					// The earlier form, which supersedes statements of any entity.
					Operation::Retract {
						entity: None,
						statements: superseded.collect(),
					}
				} else {
					Operation::retract(entity(entity_n), superseded)
				};
				(operation, at(millis))
			},
		)
}

proptest! {
	#[test]
	fn replicas_converge_whatever_the_order(
		(operations, shuffled) in prop::collection::vec(operation(), 1..24)
			.prop_flat_map(|operations| (Just(operations.clone()), Just(operations).prop_shuffle())),
	) {
		let mut first = State::new();
		for (operation, recorded) in &operations {
			first.apply(operation, *recorded).infallible();
		}
		let mut second = State::new();
		for (operation, recorded) in &shuffled {
			second.apply(operation, *recorded).infallible();
		}
		prop_assert_eq!(&first, &second);
		for n in 0..3 {
			prop_assert_eq!(first.canonical(entity(n)).infallible(), second.canonical(entity(n)).infallible());
			prop_assert_eq!(first.merged(entity(n)).infallible(), second.merged(entity(n)).infallible());
			prop_assert_eq!(first.is_deleted(entity(n)).infallible(), second.is_deleted(entity(n)).infallible());
			prop_assert_eq!(
				first.current(entity(n), &field(NAME)).infallible(),
				second.current(entity(n), &field(NAME)).infallible()
			);
		}
	}
}

proptest! {
	#[test]
	fn an_excerpt_shows_its_entity_as_the_whole_state_does(
		operations in prop::collection::vec(operation(), 1..24),
		n in 0..3_u16,
		later in operation(),
	) {
		let mut whole = State::new();
		for (operation, recorded) in &operations {
			whole.apply(operation, *recorded).infallible();
		}
		let canonical = whole.canonical(entity(n)).infallible();
		let mut excerpt = whole.excerpt(canonical).infallible();
		// A new statement about the entity, as an edit writes, shows in both. Edits give
		// statements IDs never seen before, never merge, and supersede only statements of
		// the entity's that are not merges.
		let group = whole.merged(canonical).infallible();
		if let Operation::Assert {
			statement,
			supersedes,
			..
		} = &later.0
			&& group.contains(&statement.entity)
			&& statement.field != fields::MERGED_INTO
		{
			let its_own = supersedes.iter().copied().filter(|&id| {
				whole.statement(id).infallible().is_some_and(|superseded| {
					group.contains(&superseded.entity) && superseded.field != fields::MERGED_INTO
				})
			});
			let fresh = Operation::replace(
				Statement::new(
					statement_id(100),
					statement.entity,
					statement.field.clone(),
					statement.value.clone(),
				),
				its_own,
			);
			whole.apply(&fresh, later.1).infallible();
			excerpt.apply(&fresh, later.1).infallible();
		}
		let canonical = whole.canonical(entity(n)).infallible();
		prop_assert_eq!(excerpt.canonical(canonical).infallible(), canonical);
		prop_assert_eq!(excerpt.merged(canonical).infallible(), whole.merged(canonical).infallible());
		prop_assert_eq!(excerpt.fields(canonical), whole.fields(canonical));
		prop_assert_eq!(excerpt.types(canonical).infallible(), whole.types(canonical).infallible());
		prop_assert_eq!(excerpt.is_deleted(canonical).infallible(), whole.is_deleted(canonical).infallible());
		for name in [NAME, "org.example.x"] {
			prop_assert_eq!(
				excerpt.current(canonical, &field(name)).infallible(),
				whole.current(canonical, &field(name)).infallible()
			);
		}
	}
}

#[test]
fn entities_and_their_fields_are_listed() {
	use std::collections::BTreeSet;

	let mut state = State::new();
	for operation in [
		set(1, 1, "org.example.title", text("Leviathan Wakes"), &[]),
		set(2, 1, "org.example.year", Value::Integer(2011), &[]),
		set(3, 2, "org.example.title", text("Leviathan wakes"), &[]),
		set(4, 2, "org.example.notes", text("a duplicate"), &[]),
		set(5, 1, "org.example.year", Value::Integer(2012), &[2]),
		set(
			6,
			2,
			"org.nibkeep.merged-into",
			Value::Reference(entity(1)),
			&[],
		),
		set(7, 3, "org.example.title", text("Caliban's War"), &[]),
		set(8, 3, "org.example.subtitle", text("gone"), &[]),
		Operation::retract(entity(3), [statement_id(8)]),
	] {
		state.apply(&operation, at(1)).infallible();
	}
	assert_eq!(
		state.entities().infallible(),
		BTreeSet::from([entity(1), entity(3)])
	);
	let names = |entity_id: u16| -> Vec<String> {
		state
			.fields(entity(entity_id))
			.infallible()
			.into_iter()
			.map(|field| field.to_string())
			.collect()
	};
	assert_eq!(
		names(1),
		["org.example.notes", "org.example.title", "org.example.year"],
		"the merged entity's fields, but not its merge"
	);
	assert_eq!(
		names(2),
		[
			"org.example.notes",
			"org.example.title",
			"org.example.year",
			"org.nibkeep.merged-into"
		],
		"the group's fields, as for current values, and its own merge"
	);
	assert_eq!(names(3), ["org.example.title"], "retracted fields are gone");
}
