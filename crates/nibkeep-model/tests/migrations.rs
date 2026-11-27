//! The IDs that a migration derives for what it writes, so that devices that run it at once
//! write the same statements and entities, and its note on each value it writes
//! ([technical design: migrations]).
//!
//! [technical design: migrations]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#57-migrations

use crate::common;

use crate::common::{entity, field, statement, statement_id, text};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{
	FieldDef, MigrationName, Operation, Qualifiers, Resolution, State, Value, ValueKind, content,
	fields, migration,
};

const SPLIT: &str = "org.example.split-names";

fn name(text: &str) -> MigrationName {
	text.parse().expect("a migration's name")
}

/// Sixteen bytes counting up from `from`.
fn counting(from: u8) -> [u8; 16] {
	std::array::from_fn(|index| from + u8::try_from(index).expect("small"))
}

fn space() -> SpaceId {
	SpaceId::from_bytes(counting(0))
}

/// A UUID as canonical CBOR writes it: tag 37 and a byte string of 16 bytes.
fn uuid(bytes: [u8; 16]) -> Vec<u8> {
	let mut encoded = vec![0xd8, 0x25, 0x50];
	encoded.extend(bytes);
	encoded
}

/// The first 16 bytes of the BLAKE3 key derived with `context` from `material`, as a
/// version 4 UUID.
fn derived(context: &str, material: &[u8]) -> [u8; 16] {
	let key = blake3::derive_key(context, material);
	let mut bytes = [0; 16];
	bytes.copy_from_slice(&key[..16]);
	*StatementId::from_random_bytes(bytes).as_bytes()
}

#[test]
fn a_migrated_statement_has_the_id_that_its_inputs_derive() {
	let (first, second) = (
		StatementId::from_bytes(counting(16)),
		StatementId::from_bytes(counting(32)),
	);
	// `[space, name, [first, second], 0]` in canonical CBOR, written out by hand.
	let mut material = vec![0x84];
	material.extend(uuid(counting(0)));
	material.push(0x60 + u8::try_from(SPLIT.len()).expect("short"));
	material.extend(SPLIT.as_bytes());
	material.push(0x82);
	material.extend(uuid(counting(16)));
	material.extend(uuid(counting(32)));
	material.push(0x00);
	let expected =
		StatementId::from_bytes(derived("nibkeep 2026-11 migrated statements", &material));
	assert_eq!(
		migration::statement_id(space(), &name(SPLIT), [first, second], 0),
		expected
	);
	// The test vector in the data model's specification.
	assert_eq!(expected.to_string(), "5e020f4c-1646-41d9-bd08-ce3aafe77c4e");
}

#[test]
fn the_order_of_the_replaced_statements_does_not_count() {
	let (first, second) = (statement_id(1), statement_id(2));
	let migration = name(SPLIT);
	let id = migration::statement_id(space(), &migration, [first, second], 0);
	assert_eq!(
		migration::statement_id(space(), &migration, [second, first], 0),
		id
	);
	assert_eq!(
		migration::statement_id(space(), &migration, [second, first, second], 0),
		id
	);
}

#[test]
fn every_input_of_a_migrated_statement_changes_its_id() {
	let migration = name(SPLIT);
	let id = migration::statement_id(space(), &migration, [statement_id(1)], 0);
	for other in [
		migration::statement_id(
			SpaceId::from_bytes(counting(1)),
			&migration,
			[statement_id(1)],
			0,
		),
		migration::statement_id(
			space(),
			&name("org.example.split-names-2"),
			[statement_id(1)],
			0,
		),
		migration::statement_id(space(), &migration, [statement_id(2)], 0),
		migration::statement_id(space(), &migration, [statement_id(1), statement_id(2)], 0),
		migration::statement_id(space(), &migration, [statement_id(1)], 1),
	] {
		assert_ne!(other, id);
	}
}

#[test]
fn a_migrated_entity_has_the_id_that_its_inputs_derive() {
	let book = EntityId::from_bytes(counting(16));
	// `[space, name, book, "edition"]` in canonical CBOR, written out by hand.
	let mut material = vec![0x84];
	material.extend(uuid(counting(0)));
	material.push(0x60 + u8::try_from(SPLIT.len()).expect("short"));
	material.extend(SPLIT.as_bytes());
	material.extend(uuid(counting(16)));
	material.push(0x67);
	material.extend(b"edition");
	let expected = EntityId::from_bytes(derived("nibkeep 2026-11 migrated entities", &material));
	assert_eq!(
		migration::entity_id(space(), &name(SPLIT), book, "edition"),
		expected
	);
	// The test vector in the data model's specification.
	assert_eq!(expected.to_string(), "c6b9b406-2466-4a58-b367-1b3b71b844dd");
	for other in [
		migration::entity_id(
			SpaceId::from_bytes(counting(1)),
			&name(SPLIT),
			book,
			"edition",
		),
		migration::entity_id(space(), &name("org.example.editions"), book, "edition"),
		migration::entity_id(space(), &name(SPLIT), entity(2), "edition"),
		migration::entity_id(space(), &name(SPLIT), book, "publisher"),
	] {
		assert_ne!(other, expected);
	}
}

#[test]
fn derived_ids_are_version_4_uuids() {
	let migration = name(SPLIT);
	for bytes in [
		*migration::statement_id(space(), &migration, [statement_id(1)], 0).as_bytes(),
		*migration::entity_id(space(), &migration, entity(1), "edition").as_bytes(),
	] {
		assert_eq!(bytes[6] >> 4, 4, "the version");
		assert_eq!(bytes[8] >> 6, 0b10, "the variant");
	}
}

#[test]
fn the_note_names_the_migration_and_what_it_replaced() {
	let migration = name(SPLIT);
	let (first, second) = (statement_id(1), statement_id(2));
	let note = migration::note(&migration, [second, first]).expect("names two");
	assert_eq!(note, text(&format!("{SPLIT} {first} {second}")));
	assert_eq!(
		migration::read_note(&note),
		Some((migration, vec![first, second]))
	);
}

#[test]
fn a_note_names_at_least_one_statement() {
	assert_eq!(migration::note(&name(SPLIT), []), None);
}

#[test]
fn a_note_in_another_form_reads_as_none() {
	let (first, second) = (statement_id(1), statement_id(2));
	let lettered = StatementId::from_bytes([0xab; 16]);
	for note in [
		text(""),
		text(SPLIT),
		text(&format!("{SPLIT} ")),
		text(&format!("{SPLIT}  {first}")),
		text(&format!("{SPLIT} {first} ")),
		text(&format!("{SPLIT} {second} {first}")),
		text(&format!("{SPLIT} {first} {first}")),
		text(&format!("{SPLIT} {}", lettered.to_string().to_uppercase())),
		text(&format!("Org.Example {first}")),
		text(&format!("{first}")),
		Value::Integer(1),
	] {
		assert_eq!(migration::read_note(&note), None, "{note:?}");
	}
}

#[test]
fn a_migrated_copy_and_the_same_value_typed_again_show_as_one() {
	let nickname = "org.example.nickname";
	let typed = statement(1, 1, nickname, text("Ada"));
	let migrated = statement(2, 1, nickname, text("Ada")).with_qualifiers(Qualifiers::new().with(
		fields::MIGRATED_FROM,
		migration::note(&name(SPLIT), [statement_id(3)]).expect("names one"),
	));
	assert!(fields::is_note(&fields::MIGRATED_FROM));
	assert_eq!(content(&typed), content(&migrated));
	let mut state = State::new();
	for statement in [typed, migrated] {
		state
			.apply(&Operation::assert(statement), common::at(1))
			.infallible();
	}
	let Resolution::Values(shown) = state
		.resolve_on(
			entity(1),
			&field(nickname),
			Some(&FieldDef::new(ValueKind::Text)),
			"2026-11-15".parse().expect("a day"),
		)
		.infallible()
	else {
		panic!("one value, not competing ones");
	};
	assert_eq!(shown.len(), 1);
}

#[test]
fn an_ai_agent_writes_no_note_of_a_migration() {
	let noted = statement(1, 1, "org.example.nickname", text("Ada")).with_qualifiers(
		Qualifiers::new().with(
			fields::MIGRATED_FROM,
			migration::note(&name(SPLIT), [statement_id(3)]).expect("names one"),
		),
	);
	assert!(!fields::ai_agent_may_write(&noted));
	assert!(fields::is_reserved(&fields::MIGRATED_FROM));
}

#[test]
fn a_migration_is_named_as_types_and_fields_are() {
	for valid in [SPLIT, "std.editions", "local.my-migration"] {
		assert!(valid.parse::<MigrationName>().is_ok(), "{valid}");
	}
	for invalid in [
		"",
		"editions",
		"Std.Editions",
		"std..editions",
		"std.-editions",
	] {
		assert!(invalid.parse::<MigrationName>().is_err(), "{invalid}");
	}
}
