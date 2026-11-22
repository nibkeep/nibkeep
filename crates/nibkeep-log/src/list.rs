//! Ordered lists: items belong to a list through memberships, ordered by position keys.

use std::collections::BTreeSet;
use std::iter;

use nibkeep_base::{EntityId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{
	FieldDef, Operation, Resolution, State, Statement, TypeDef, TypeName, Value, ValueKind, fields,
};

/// The type of memberships.
pub const MEMBERSHIP: TypeName = TypeName::from_static("org.nibkeep.membership");

/// The list a membership belongs to, the item it puts in its list, and its position key.
pub use nibkeep_model::fields::{ITEM, LIST, POSITION};

/// The digits of position keys, in the order of their bytes.
const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// One item of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
	/// The membership that puts the item in the list.
	pub membership: EntityId,
	/// The item, if the membership names one.
	pub item: Option<EntityId>,
	/// The membership's position key, if it has a valid one.
	pub position: Option<String>,
}

/// The definition of the membership type.
#[must_use]
pub fn membership_type() -> TypeDef {
	TypeDef::new(MEMBERSHIP, 1)
		.with_field(LIST, FieldDef::new(ValueKind::Reference).role())
		.with_field(ITEM, FieldDef::new(ValueKind::Reference).role())
		.with_field(POSITION, FieldDef::new(ValueKind::Text).last_writer_wins())
}

/// The items of a list, in order: the memberships that refer to it and are not deleted,
/// sorted by position key, and those with equal keys by ID.
///
/// # Errors
///
/// Returns an error if the state's storage cannot be read.
pub fn members<S: Statements>(state: &State<S>, list: EntityId) -> Result<Vec<Member>, S::Error> {
	let membership_type = membership_type();
	let position = membership_type.field(&POSITION);
	let mut memberships = BTreeSet::new();
	for statement in state.referrers(list)? {
		if statement.field == LIST {
			let membership = state.canonical(statement.entity)?;
			if state.types(membership)?.contains(&MEMBERSHIP) {
				memberships.insert(membership);
			}
		}
	}
	let mut members = Vec::new();
	for membership in memberships {
		if state.is_deleted(membership)? {
			continue;
		}
		let item = state
			.current(membership, &ITEM)?
			.iter()
			.find_map(|statement| statement.value.as_reference());
		let position = match state.resolve(membership, &POSITION, position)? {
			Resolution::Values(values) | Resolution::Competing(values) => values
				.first()
				.and_then(|statement| statement.value.as_text())
				.filter(|key| is_position(key))
				.map(str::to_owned),
		};
		members.push(Member {
			membership,
			item,
			position,
		});
	}
	members.sort_by(|a, b| (&a.position, a.membership).cmp(&(&b.position, b.membership)));
	Ok(members)
}

/// The operations that add `item` to `list` at `index`: a new membership with its type,
/// list, item and position. Returns the membership, too.
///
/// # Errors
///
/// Returns an error if the state's storage cannot be read.
pub fn insert<S: Statements>(
	state: &State<S>,
	list: EntityId,
	item: EntityId,
	index: usize,
	rng: &mut impl CryptoRng,
) -> Result<(EntityId, Vec<Operation>), S::Error> {
	let membership = EntityId::random(rng);
	let mut operations = vec![
		Operation::assert(Statement::new(
			StatementId::random(rng),
			membership,
			fields::TYPE,
			Value::Text(MEMBERSHIP.to_string()),
		)),
		Operation::assert(Statement::new(
			StatementId::random(rng),
			membership,
			LIST,
			Value::Reference(list),
		)),
		Operation::assert(Statement::new(
			StatementId::random(rng),
			membership,
			ITEM,
			Value::Reference(item),
		)),
	];
	operations.extend(move_to(state, list, membership, index, rng)?);
	Ok((membership, operations))
}

/// The operations that move a membership to `index` in its list, counted among the other
/// members. Usually that is a new position key for the membership alone; members after
/// the new place that share the key before it move up with it, in their order.
///
/// # Errors
///
/// Returns an error if the state's storage cannot be read.
pub fn move_to<S: Statements>(
	state: &State<S>,
	list: EntityId,
	membership: EntityId,
	index: usize,
	rng: &mut impl CryptoRng,
) -> Result<Vec<Operation>, S::Error> {
	let membership = state.canonical(membership)?;
	let order: Vec<Member> = members(state, list)?
		.into_iter()
		.filter(|member| member.membership != membership)
		.collect();
	let index = index.min(order.len());
	let lower = index
		.checked_sub(1)
		.and_then(|before| order[before].position.clone());
	let run_end = order[index..]
		.iter()
		.position(|member| member.position != lower)
		.map_or(order.len(), |offset| index + offset);
	let upper = order
		.get(run_end)
		.and_then(|member| member.position.clone());

	let moved =
		iter::once(membership).chain(order[index..run_end].iter().map(|member| member.membership));
	let mut previous = lower;
	let mut operations = Vec::new();
	for membership in moved {
		// The members are sorted, and the run ends where the keys grow, so there is
		// always room.
		let Some(key) = position_between(previous.as_deref(), upper.as_deref()) else {
			unreachable!("the key before is smaller than the key after");
		};
		operations.push(set_position(state, membership, key.clone(), rng)?);
		previous = Some(key);
	}
	Ok(operations)
}

/// Whether text is a valid position key: one or more of the characters `0`–`9`, `A`–`Z`
/// and `a`–`z`, not ending in `0`.
#[must_use]
pub fn is_position(key: &str) -> bool {
	!key.is_empty() && key.bytes().all(|byte| byte.is_ascii_alphanumeric()) && !key.ends_with('0')
}

/// A position key between `before` and `after`, where `None` stands for the start and the
/// end of the list. Returns `None` if a key is not valid, or `before` is not smaller than
/// `after`.
#[must_use]
pub fn position_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
	if before.is_some_and(|key| !is_position(key)) || after.is_some_and(|key| !is_position(key)) {
		return None;
	}
	if let (Some(before), Some(after)) = (before, after)
		&& before >= after
	{
		return None;
	}
	let key = midpoint(before.unwrap_or("").as_bytes(), after.map(str::as_bytes));
	String::from_utf8(key).ok()
}

/// A key between `lower` and `upper`, after David Greenspan's fractional indexing: keys
/// are fractions in base 62 without trailing zeros, so there is always room between two
/// of them. `lower` is empty for the start of the list, and `upper` is `None` for its end.
fn midpoint(lower: &[u8], upper: Option<&[u8]>) -> Vec<u8> {
	if let Some(upper) = upper {
		// Keep the common prefix, reading missing digits of `lower` as zeros.
		let common = upper
			.iter()
			.enumerate()
			.take_while(|&(index, &digit)| lower.get(index).copied().unwrap_or(b'0') == digit)
			.count();
		if common > 0 {
			let mut key = upper[..common].to_vec();
			let rest = lower.get(common..).unwrap_or(&[]);
			key.extend(midpoint(rest, Some(&upper[common..])));
			return key;
		}
	}
	let low = lower.first().map_or(0, |&byte| digit(byte));
	let high = upper.map_or(DIGITS.len(), |upper| digit(upper[0]));
	if high - low > 1 {
		vec![DIGITS[usize::midpoint(low, high)]]
	} else if let Some(upper) = upper.filter(|upper| upper.len() > 1) {
		// The first digits are neighbours, and `upper` goes on: its first digit alone is
		// between the two.
		vec![upper[0]]
	} else {
		let mut key = vec![DIGITS[low]];
		key.extend(midpoint(lower.get(1..).unwrap_or(&[]), None));
		key
	}
}

/// The value of a digit of a position key.
fn digit(byte: u8) -> usize {
	usize::from(match byte {
		b'0'..=b'9' => byte - b'0',
		b'A'..=b'Z' => byte - b'A' + 10,
		_ => byte - b'a' + 36,
	})
}

/// Sets a membership's position key, replacing its current ones.
fn set_position<S: Statements>(
	state: &State<S>,
	membership: EntityId,
	key: String,
	rng: &mut impl CryptoRng,
) -> Result<Operation, S::Error> {
	let replaced: Vec<StatementId> = state
		.current(membership, &POSITION)?
		.iter()
		.map(|statement| statement.id)
		.collect();
	Ok(Operation::replace(
		Statement::new(
			StatementId::random(rng),
			membership,
			POSITION,
			Value::Text(key),
		),
		replaced,
	))
}
