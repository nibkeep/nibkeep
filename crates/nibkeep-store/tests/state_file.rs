//! The state file: a space's state kept on disk, encrypted, which answers every query as
//! the state in memory does ([technical design: the state file]).
//!
//! [technical design: the state file]: ../../../docs/Technical-design.md#75-the-state-file

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_crypto::Key;
use nibkeep_model::storage::{AsOf, Infallibly, Layers, Origin, Scope, Statements};
use nibkeep_model::{FieldName, Operation, Qualifiers, State, Statement, Value, fields};
use nibkeep_store::{EncryptedDatabase, StateFile, StoreError};
use nibkeep_testkit::TestRng;
use nibkeep_time::Timestamp;
use proptest::prelude::*;

const FIELDS: [&str; 5] = [
	"org.nibkeep.name",
	"org.example.x",
	"org.example.y",
	"org.nibkeep.merged-into",
	"org.nibkeep.deleted",
];

fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn key(seed: &str) -> Key {
	TestRng::new(seed).key()
}

fn open(path: &Path, key: &Key) -> Result<StateFile, StoreError> {
	StateFile::open(path, key, TestRng::new(&format!("{}", path.display())))
}

/// Stand-in for random bytes: different for every `n`, and ordered like `n`.
fn bytes(n: u16) -> [u8; 16] {
	let mut bytes = [0; 16];
	bytes[..2].copy_from_slice(&n.to_be_bytes());
	bytes
}

fn statement_id(n: u16) -> StatementId {
	StatementId::from_random_bytes(bytes(n))
}

fn entity(n: u16) -> EntityId {
	EntityId::from_random_bytes(bytes(n))
}

fn at(millis: u64) -> Timestamp {
	Timestamp::new(millis, 0)
}

fn statement(id: u16, entity_n: u16, field: &str, value: Value) -> Statement {
	Statement::new(
		statement_id(id),
		entity(entity_n),
		field.parse().expect("a field name"),
		value,
	)
}

/// A value that suits a field, for merges and deletion to happen.
fn value_for(field: &str, n: u16) -> Value {
	match field {
		"org.nibkeep.merged-into" => Value::Reference(entity(n)),
		"org.nibkeep.deleted" => Value::Boolean(n > 0),
		"org.example.y" => Value::Text(format!("value {n}")),
		_ => Value::Integer(i64::from(n)),
	}
}

/// A random operation on a few statements, entities and fields, with a system time, and
/// sometimes a later one, as a purge states it again. Retractions of secret statements name
/// the secret ones too, 10 to 12.
fn operation() -> impl Strategy<Value = (Operation, Origin)> {
	(
		any::<bool>(),
		0..10_u16,
		0..4_u16,
		prop::sample::select(FIELDS.to_vec()),
		0..4_u16,
		prop::collection::btree_set(0..10_u16, 0..3),
		0..6_u64,
		prop::option::weighted(0.2, 0..3_u64),
		prop::option::of(0..3_u8),
	)
		.prop_map(
			|(assert, id, entity_n, field, value, superseded, millis, later, change)| {
				let ids = |shift: u16| superseded.iter().map(move |n| statement_id(n + shift));
				let operation = if assert {
					Operation::replace(
						statement(id, entity_n, field, value_for(field, value)),
						ids(0),
					)
				} else if value == 0 {
					// The earlier form, which supersedes statements of any entity.
					Operation::Retract {
						entity: None,
						statements: ids(0).collect(),
					}
				} else if entity_n == 0 {
					Operation::retract_secret(ids(3))
				} else {
					Operation::retract(entity(entity_n), ids(0))
				};
				let origin = Origin {
					at: at(millis),
					restated: later.map(|later| at(millis + later)),
					change: change.map(|change| Hash::of(&[change])),
				};
				(operation, origin)
			},
		)
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

/// The answer of the state file, which must read.
fn read<T, E: Debug>(result: Result<T, E>) -> T {
	result.expect("the state file reads")
}

/// Checks that two states answer every query alike.
fn same_answers<A, B>(one: &State<A>, other: &State<B>) -> Result<(), TestCaseError>
where
	A: Statements,
	A::Error: Debug,
	B: Statements<Error = Infallible>,
{
	prop_assert_eq!(read(one.statements()), other.statements().infallible());
	prop_assert_eq!(read(one.entities()), other.entities().infallible());
	prop_assert_eq!(
		read(one.storage().described()),
		other.storage().described().infallible()
	);
	for n in 0..4 {
		let entity = entity(n);
		prop_assert_eq!(
			read(one.canonical(entity)),
			other.canonical(entity).infallible()
		);
		prop_assert_eq!(read(one.merged(entity)), other.merged(entity).infallible());
		prop_assert_eq!(
			read(one.is_deleted(entity)),
			other.is_deleted(entity).infallible()
		);
		prop_assert_eq!(read(one.fields(entity)), other.fields(entity).infallible());
		prop_assert_eq!(read(one.types(entity)), other.types(entity).infallible());
		prop_assert_eq!(
			read(one.referrers(entity)),
			other.referrers(entity).infallible()
		);
		let (excerpt, other_excerpt) = (
			read(one.excerpt(entity)),
			other.excerpt(entity).infallible(),
		);
		prop_assert_eq!(
			excerpt.statements().infallible(),
			other_excerpt.statements().infallible()
		);
		for field in FIELDS {
			let field = field.parse().expect("a field name");
			prop_assert_eq!(
				read(one.current(entity, &field)),
				other.current(entity, &field).infallible()
			);
			prop_assert_eq!(
				read(one.last_written(entity, &field)),
				other.last_written(entity, &field).infallible()
			);
		}
	}
	prop_assert_eq!(
		read(one.storage().field_names()),
		other.storage().field_names().infallible()
	);
	for field in FIELDS {
		let field = field.parse().expect("a field name");
		prop_assert_eq!(
			read(one.storage().current_in_field(&field)),
			other.storage().current_in_field(&field).infallible()
		);
	}
	// Ranges of entities, of some fields and of all, and the parts read from them.
	let all: Vec<FieldName> = FIELDS
		.iter()
		.map(|field| field.parse().expect("a field name"))
		.collect();
	let merges = other
		.storage()
		.current_in_field(&fields::MERGED_INTO)
		.infallible();
	let merges: Vec<_> = merges.iter().collect();
	for (first, last) in [(0, 3), (1, 2), (2, 2), (3, 0)] {
		let (first, last) = (entity(first), entity(last));
		for fields in [&all[..], &all[1..3], &[]] {
			prop_assert_eq!(
				read(one.storage().current_in_range(first, last, fields)),
				other
					.storage()
					.current_in_range(first, last, fields)
					.infallible()
			);
			let (part, other_part) = (
				read(one.excerpt_of_range((first, last), fields, &merges)),
				other
					.excerpt_of_range((first, last), fields, &merges)
					.infallible(),
			);
			prop_assert_eq!(
				part.statements().infallible(),
				other_part.statements().infallible()
			);
		}
	}
	for n in 0..10 {
		let id = statement_id(n);
		prop_assert_eq!(read(one.statement(id)), other.statement(id).infallible());
		prop_assert_eq!(read(one.recorded(id)), other.recorded(id).infallible());
		prop_assert_eq!(read(one.is_current(id)), other.is_current(id).infallible());
	}
	Ok(())
}

/// Each test case gets a file of its own.
static CASES: AtomicUsize = AtomicUsize::new(0);

/// Random secret statements, 10 to 12, which ordinary operations never assert.
fn secrets() -> impl Strategy<Value = Vec<(Statement, Timestamp)>> {
	prop::collection::btree_map(
		10..13_u16,
		(
			0..4_u16,
			prop::sample::select(FIELDS.to_vec()),
			0..4_u16,
			0..6_u64,
		),
		0..3,
	)
	.prop_map(|secrets| {
		secrets
			.into_iter()
			.map(|(id, (entity_n, field, value, millis))| {
				(
					statement(id, entity_n, field, value_for(field, value)),
					at(millis),
				)
			})
			.collect()
	})
}

/// Applies operations, in transactions of `batch` operations, opening secrets after
/// `opened_after` of them.
fn apply_all(
	state: &mut State<Layers<StateFile>>,
	operations: &[(Operation, Origin)],
	secrets: &[(Statement, Timestamp)],
	batch: usize,
	opened_after: usize,
) {
	for (index, chunk) in operations.chunks(batch).enumerate() {
		if index * batch >= opened_after {
			for (secret, recorded) in secrets {
				state
					.storage_mut()
					.open(secret, Origin::new(*recorded, None));
			}
		}
		state.storage_mut().ordinary_mut().begin().expect("begins");
		for (operation, origin) in chunk {
			state.apply_from(operation, *origin).expect("applies");
		}
		state
			.storage_mut()
			.ordinary_mut()
			.commit()
			.expect("commits");
	}
	for (secret, recorded) in secrets {
		state
			.storage_mut()
			.open(secret, Origin::new(*recorded, None));
	}
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(48))]

	#[test]
	fn a_state_file_answers_as_the_state_in_memory_does(
		operations in prop::collection::vec(operation(), 0..32),
		secrets in secrets(),
		batch in 1..8_usize,
		opened_after in 0..32_usize,
	) {
		let case = CASES.fetch_add(1, Ordering::Relaxed);
		let path = directory(&format!("answers-{case}")).join("state");
		let key = key("answers");
		let mut on_disk = State::in_storage(Layers::new(open(&path, &key).expect("opens")));
		apply_all(&mut on_disk, &operations, &secrets, batch, opened_after);
		let mut in_memory = State::new();
		let secret_ids: BTreeSet<StatementId> = secrets.iter().map(|(secret, _)| secret.id).collect();
		for (operation, origin) in &operations {
			in_memory.apply_from(&as_if_ordinary(operation, &secret_ids), *origin).infallible();
		}
		for (secret, recorded) in &secrets {
			in_memory.apply(&Operation::assert(secret.clone()), *recorded).infallible();
		}
		same_answers(&on_disk, &in_memory)?;
		// Each statement keeps when and by which change it was recorded and restated.
		let origins = |all: Vec<nibkeep_model::storage::Recorded<'_>>| -> Vec<(Statement, Origin)> {
			all.into_iter()
				.map(|recorded| {
					let origin = recorded.origin();
					(recorded.statement.into_owned(), origin)
				})
				.collect()
		};
		prop_assert_eq!(
			origins(read(on_disk.storage().all())),
			origins(in_memory.storage().all().infallible())
		);
		// And every other change that asserted it, which a purge of it takes too.
		for n in 0..13 {
			prop_assert_eq!(
				read(on_disk.storage().asserted_again(statement_id(n))),
				in_memory.storage().asserted_again(statement_id(n)).infallible()
			);
		}
		// So do they as they stood at each time, which the state file knows from when each
		// statement was recorded and first superseded.
		for millis in 0..7 {
			same_answers(
				&State::in_storage(AsOf::new(on_disk.storage(), millis)),
				&State::in_storage(AsOf::new(in_memory.storage(), millis)),
			)?;
		}

		// The secrets never reach the file.
		drop(on_disk);
		let mut ordinary = State::new();
		for (operation, origin) in &operations {
			ordinary.apply_from(operation, *origin).infallible();
		}
		let reopened = State::in_storage(open(&path, &key).expect("opens again"));
		same_answers(&reopened, &ordinary)?;
		for n in 0..13 {
			prop_assert_eq!(
				read(reopened.storage().asserted_again(statement_id(n))),
				ordinary.storage().asserted_again(statement_id(n)).infallible()
			);
		}
	}
}

/// Applies one operation in a transaction of its own.
fn apply_one(state: &mut State<StateFile>, operation: &Operation, recorded: Timestamp) {
	state.storage_mut().begin().expect("begins");
	state.apply(operation, recorded).expect("applies");
	state.storage_mut().commit().expect("commits");
}

#[test]
fn statements_come_back_whole() {
	let path = directory("whole").join("state");
	let key = key("whole");
	let valid_time = "1975-07~/1980".parse().expect("a date");
	let dated = statement(1, 1, "org.example.x", Value::Text("Grandma".into())).with_qualifiers(
		Qualifiers::new()
			.with(fields::VALID_TIME, Value::Date(valid_time))
			.with(fields::PREFERRED, Value::Boolean(true)),
	);
	let late = statement(2, 1, "org.example.y", Value::Integer(2));
	let latest = Timestamp::new(u64::MAX, u32::MAX);
	let mut state = State::in_storage(open(&path, &key).expect("opens"));
	apply_one(
		&mut state,
		&Operation::assert(dated.clone()),
		Timestamp::new(7, 3),
	);
	apply_one(&mut state, &Operation::assert(late.clone()), latest);
	drop(state);
	let state = State::in_storage(open(&path, &key).expect("opens again"));
	assert_eq!(
		state
			.statement(dated.id)
			.expect("reads")
			.map(std::borrow::Cow::into_owned),
		Some(dated.clone())
	);
	assert_eq!(
		state.recorded(dated.id).expect("reads"),
		Some(Timestamp::new(7, 3))
	);
	assert_eq!(state.recorded(late.id).expect("reads"), Some(latest));
}

#[test]
fn the_state_file_is_written_only_in_a_transaction() {
	let path = directory("outside").join("state");
	let mut state = State::in_storage(open(&path, &key("outside")).expect("opens"));
	let operation = Operation::assert(statement(1, 1, "org.example.x", Value::Integer(1)));
	assert!(matches!(
		state.apply(&operation, at(1)),
		Err(StoreError::Transaction)
	));
	assert_eq!(state.statement(statement_id(1)).expect("reads"), None);
	state.storage_mut().begin().expect("begins");
	assert!(matches!(
		state.storage_mut().begin(),
		Err(StoreError::Transaction)
	));
}

#[test]
fn an_error_in_a_transaction_ends_it() {
	let path = directory("ends").join("state");
	let key = key("ends");
	drop(open(&path, &key).expect("opens"));
	// A row whose statement does not decode, as in a damaged file.
	{
		let database = EncryptedDatabase::open(&path, &key, TestRng::new("ends")).expect("opens");
		database
			.connection()
			.execute(
				"INSERT INTO entities (n, id) VALUES (1, ?1)",
				[entity(1).as_bytes()],
			)
			.expect("numbers the entity");
		database
			.connection()
			.execute(
				"INSERT INTO statements
					(id, entity, field, reference, recorded_millis, recorded_counter,
						restated_millis, restated_counter, origin,
						superseded_millis, superseded_counter, content)
					VALUES (?1, 1, 'org.example.x', NULL, 1, 0, NULL, NULL, NULL, NULL, NULL, x'ff')",
				[statement_id(1).as_bytes()],
			)
			.expect("inserts");
	}
	let mut state = State::in_storage(open(&path, &key).expect("opens again"));
	state.storage_mut().begin().expect("begins");
	state
		.apply(
			&Operation::assert(statement(2, 1, "org.example.x", Value::Integer(2))),
			at(2),
		)
		.expect("applies");
	let again = Operation::assert(statement(1, 1, "org.example.x", Value::Integer(1)));
	assert!(matches!(
		state.apply(&again, at(3)),
		Err(StoreError::State(_))
	));
	// What the transaction wrote is left out, and the write lock is free; the file is not
	// read until a transaction begins anew, as the next change does.
	assert!(!state.storage().in_transaction());
	assert!(matches!(
		state.statement(statement_id(2)),
		Err(StoreError::State(_))
	));
	state.storage_mut().begin().expect("begins again");
	assert_eq!(state.statement(statement_id(2)).expect("reads"), None);
	state.storage_mut().rollback().expect("rolls back");
}

#[test]
fn a_transaction_rolled_back_leaves_nothing() {
	let path = directory("rolled-back").join("state");
	let key = key("rolled-back");
	let mut state = State::in_storage(open(&path, &key).expect("opens"));
	apply_one(
		&mut state,
		&Operation::assert(statement(1, 1, "org.example.x", Value::Integer(1))),
		at(1),
	);
	state.storage_mut().begin().expect("begins");
	state
		.apply(
			&Operation::replace(
				statement(2, 1, "org.example.x", Value::Integer(2)),
				[statement_id(1)],
			),
			at(2),
		)
		.expect("applies");
	state.storage_mut().rollback().expect("rolls back");
	assert!(state.is_current(statement_id(1)).expect("reads"));
	assert_eq!(state.statement(statement_id(2)).expect("reads"), None);
}

#[test]
fn a_state_file_of_an_earlier_format_is_made_anew_and_of_a_later_one_left_as_it_is() {
	let path = directory("other-format").join("state");
	let key = key("other-format");
	let version = |set: Option<i64>| -> i64 {
		let database =
			EncryptedDatabase::open(&path, &key, TestRng::new("other-format")).expect("opens");
		if let Some(set) = set {
			database
				.connection()
				.pragma_update(None, "user_version", set)
				.expect("sets the version");
		}
		database
			.connection()
			.pragma_query_value(None, "user_version", |row| row.get(0))
			.expect("reads the version")
	};
	let remake = || StateFile::open_or_make(&path, &key, || TestRng::new("remade"));
	drop(open(&path, &key).expect("opens"));
	let current = version(None);
	// An earlier format, while there is one, is of no use: it is made anew.
	if current > 1 {
		version(Some(current - 1));
		assert!(matches!(open(&path, &key), Err(StoreError::State(_))));
		drop(remake().expect("made anew"));
		assert_eq!(version(None), current);
	}
	// A later one only a newer program reads, which another program on the replica may
	// be: it is never made anew.
	version(Some(current + 1));
	assert!(matches!(open(&path, &key), Err(StoreError::Newer)));
	assert!(matches!(remake(), Err(StoreError::Newer)));
	assert_eq!(version(None), current + 1, "left as it is");
}

#[test]
fn a_state_file_whose_header_is_of_a_later_version_is_left_as_it_is() {
	let path = directory("later-header").join("state");
	let key = key("later-header");
	drop(open(&path, &key).expect("opens"));
	let mut bytes = std::fs::read(&path).expect("reads");
	assert_eq!(&bytes[..16], b"nibkeep state\0\0\x01");
	bytes[15] = 2;
	std::fs::write(&path, &bytes).expect("writes");
	assert!(matches!(open(&path, &key), Err(StoreError::Newer)));
	assert!(matches!(
		StateFile::open_or_make(&path, &key, || TestRng::new("remade")),
		Err(StoreError::Newer)
	));
	assert_eq!(std::fs::read(&path).expect("reads"), bytes, "left as it is");
}

#[test]
fn a_statement_whose_entity_has_no_number_is_damage() {
	let path = directory("unnumbered").join("state");
	let key = key("unnumbered");
	let mut state = open(&path, &key).expect("opens");
	state.begin().expect("begins");
	state
		.put(
			&statement(1, 1, "org.example.x", Value::Integer(1)),
			Origin::new(at(1), None),
		)
		.expect("puts");
	state.commit().expect("commits");
	drop(state);
	// As in a damaged file, the entity's number is gone.
	EncryptedDatabase::open(&path, &key, TestRng::new("unnumbered"))
		.expect("opens")
		.connection()
		.execute("DELETE FROM entities", [])
		.expect("deletes");
	let state = open(&path, &key).expect("opens again");
	assert!(state.contains(statement_id(1)).expect("finds"));
	assert!(matches!(
		state.get(statement_id(1)),
		Err(StoreError::State(_))
	));
}

#[test]
fn a_state_file_that_its_key_does_not_open_is_refused() {
	let path = directory("wrong-key").join("state");
	drop(open(&path, &key("right")).expect("opens"));
	assert!(matches!(
		open(&path, &key("wrong")),
		Err(StoreError::State(_))
	));
}

#[test]
fn the_earliest_time_a_statement_was_superseded_counts() {
	let path = directory("earliest").join("state");
	let mut file = open(&path, &key("earliest")).expect("opens");
	file.begin().expect("begins");
	let id = statement_id(1);
	for millis in [u64::MAX, 5, 1 << 63, 7] {
		file.supersede(Scope::Any, [id], Timestamp::new(millis, 0))
			.expect("supersedes");
	}
	file.commit().expect("commits");
	assert_eq!(file.superseded_at(id).expect("reads"), Some(at(5)));
}

/// An operation names the entity it changes, and supersedes only statements of it, whether
/// it comes before or after the statement, and after the file is opened again.
#[test]
fn a_statement_is_superseded_only_by_operations_about_its_entity() {
	let path = directory("of-its-entity").join("state");
	let key = key("of its entity");
	let mut file = open(&path, &key).expect("opens");
	let ada = statement(1, 1, "org.nibkeep.name", Value::Text("Ada".into()));
	file.begin().expect("begins");
	file.supersede(Scope::Entity(entity(2)), [ada.id], at(2))
		.expect("supersedes");
	file.put(&ada, Origin::new(at(1), None)).expect("puts");
	file.commit().expect("commits");
	assert_eq!(file.superseded_at(ada.id).expect("reads"), None);
	assert_eq!(
		file.superseded_at_of(ada.id, entity(2), false)
			.expect("reads"),
		Some(at(2)),
		"as a statement of the other entity, it would be"
	);
	drop(file);
	let mut file = open(&path, &key).expect("opens again");
	assert_eq!(file.superseded_at(ada.id).expect("reads"), None);
	file.begin().expect("begins");
	file.supersede(Scope::Entity(entity(1)), [ada.id], at(3))
		.expect("supersedes");
	file.commit().expect("commits");
	assert_eq!(file.superseded_at(ada.id).expect("reads"), Some(at(3)));
}

/// A retraction of secret statements supersedes none of the file's statements, which are
/// all ordinary, before or after they arrive: only a secret statement opened beside the
/// file, also after it is opened again.
#[test]
fn a_retraction_of_secret_statements_supersedes_only_secret_ones() {
	let path = directory("secret-scope").join("state");
	let key = key("secret scope");
	let mut file = open(&path, &key).expect("opens");
	let ada = statement(1, 1, "org.nibkeep.name", Value::Text("Ada".into()));
	let grace = statement(2, 1, "org.nibkeep.name", Value::Text("Grace".into()));
	file.begin().expect("begins");
	file.put(&ada, Origin::new(at(1), None)).expect("puts");
	file.supersede(Scope::Secret, [ada.id, grace.id], at(4))
		.expect("supersedes");
	file.supersede(Scope::Secret, [ada.id], at(2))
		.expect("supersedes");
	file.put(&grace, Origin::new(at(1), None)).expect("puts");
	file.commit().expect("commits");
	drop(file);
	let file = open(&path, &key).expect("opens again");
	assert_eq!(file.superseded_at(ada.id).expect("reads"), None);
	assert_eq!(file.superseded_at(grace.id).expect("reads"), None);
	assert_eq!(
		file.superseded_at_of(ada.id, entity(1), true)
			.expect("reads"),
		Some(at(2)),
		"a secret statement with its ID would be, from the earliest"
	);
	assert_eq!(
		file.superseded_at_of(ada.id, entity(1), false)
			.expect("reads"),
		None
	);
	// One the file does not hold, as a secret statement that this replica cannot open, any
	// operation supersedes.
	let unopened = statement_id(3);
	let mut file = file;
	file.begin().expect("begins");
	file.supersede(Scope::Secret, [unopened], at(5))
		.expect("supersedes");
	file.commit().expect("commits");
	assert_eq!(file.superseded_at(unopened).expect("reads"), Some(at(5)));
	assert!(file.is_superseded(unopened).expect("reads"));
}
