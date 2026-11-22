//! The IDs that a migration derives for what it writes, and its note on each value it
//! writes ([technical design: migrations]).
//!
//! A migration moves statements to the shape that a newer version of a type gives them: to
//! new fields, or to new entities. Devices that run one at once must write the same
//! statements, which then count once, and the same new entities. So a migration derives the
//! IDs of what it writes from what it replaces, rather than drawing random ones: a
//! statement's from the statements it replaces, and an entity's from the entity it comes
//! from. Never from what the statements say: a secret statement's ID is in its change's
//! header, which anyone who passes the change on reads.
//!
//! The rules are in `docs/spec/model.md`, section 12.
//!
//! [technical design: migrations]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#57-migrations

use std::collections::BTreeSet;

use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{EntityId, SpaceId, StatementId};

use crate::{MigrationName, Value};

/// The context of the key that a migrated statement's ID is derived from.
const STATEMENTS: &str = "nibkeep 2026-11 migrated statements";

/// The context of the key that a migrated entity's ID is derived from.
const ENTITIES: &str = "nibkeep 2026-11 migrated entities";

/// The ID of the statement that a migration writes in `space` from the statements it
/// replaces, `replaced`, as the `n`th it writes from them, counting from 0: the first 16
/// bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 migrated statements`
/// from the canonical encoding of `[space, migration, replaced, n]`, `replaced` sorted by
/// their bytes without duplicates, as a version 4 UUID.
///
/// What a migration writes from a set of statements depends on them alone, in an order of
/// its own, so that devices that run it at once write the same statement under each ID.
#[must_use]
pub fn statement_id(
	space: SpaceId,
	migration: &MigrationName,
	replaced: impl IntoIterator<Item = StatementId>,
	n: u64,
) -> StatementId {
	let replaced: BTreeSet<StatementId> = replaced.into_iter().collect();
	let material = Cbor::Array(vec![
		space.to_cbor(),
		migration.to_cbor(),
		Cbor::Array(replaced.iter().map(ToCbor::to_cbor).collect()),
		n.to_cbor(),
	]);
	StatementId::derived(STATEMENTS, &cbor::to_vec(&material))
}

/// The ID of an entity that a migration makes in `space` for the entity `from`, such as an
/// edition for a book: the first 16 bytes of the BLAKE3 key derived with the context
/// `nibkeep 2026-11 migrated entities` from the canonical encoding of `[space, migration,
/// from, label]`, as a version 4 UUID. The label tells apart the entities that it makes for
/// one entity.
#[must_use]
pub fn entity_id(
	space: SpaceId,
	migration: &MigrationName,
	from: EntityId,
	label: &str,
) -> EntityId {
	let material = Cbor::Array(vec![
		space.to_cbor(),
		migration.to_cbor(),
		from.to_cbor(),
		label.to_cbor(),
	]);
	EntityId::derived(ENTITIES, &cbor::to_vec(&material))
}

/// The note that a migration puts on each value it writes, as the qualifier
/// [`crate::fields::MIGRATED_FROM`]: the migration's name, then the IDs of the statements it
/// replaced, sorted by their bytes without duplicates, each after a space; `None` if it
/// replaced none, as a value that a migration writes always comes from some. A note is the
/// migrating device's account of where the value came from, never proof, and values that
/// are compared leave it out.
#[must_use]
pub fn note(
	migration: &MigrationName,
	replaced: impl IntoIterator<Item = StatementId>,
) -> Option<Value> {
	let replaced: BTreeSet<StatementId> = replaced.into_iter().collect();
	if replaced.is_empty() {
		return None;
	}
	let mut text = migration.as_str().to_owned();
	for id in replaced {
		text.push(' ');
		text.push_str(&id.to_string());
	}
	Some(Value::Text(text))
}

/// The migration and the statements it replaced that a note names, if the value is a note
/// in the form that [`note`] writes, with at least one statement; `None` otherwise.
#[must_use]
pub fn read_note(value: &Value) -> Option<(MigrationName, Vec<StatementId>)> {
	let mut parts = value.as_text()?.split(' ');
	let migration: MigrationName = parts.next()?.parse().ok()?;
	let replaced = parts
		.map(|part| part.parse().ok())
		.collect::<Option<Vec<StatementId>>>()?;
	let in_order = replaced.windows(2).all(|pair| pair[0] < pair[1]);
	(!replaced.is_empty() && in_order).then_some((migration, replaced))
}
