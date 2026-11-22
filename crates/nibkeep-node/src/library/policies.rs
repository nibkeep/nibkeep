//! Storage policies: how many copies the files of a space need, where, and whether this
//! device keeps them in NibKeep's store or where they are, and the rules that choose a
//! file's policy ([technical design: storage policies]).
//!
//! [technical design: storage policies]: ../../../../docs/Technical-design.md#93-storage-policies

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_log::list::{is_position, position_between};
use nibkeep_model::storage::Statements;
use nibkeep_model::{
	FieldDef, FieldName, Name, Operation, Resolution, State, Statement, TypeDef, TypeName, Value,
	ValueKind, fields,
};
use nibkeep_std::{
	CAMERA_MAKE, CONTENT, FILE, FOUND_ON, MEDIA_TYPE, MODIFIED, ORIGIN, ORIGIN_OF, PATH, SIZE,
};

use super::Library;
use super::accounts::Copies;
use super::entities::{PART, by_parts};
use super::files::{moment, resolved_path};
use crate::NodeError;

/// The type of storage policies.
pub const STORAGE_POLICY: TypeName = TypeName::from_static("org.nibkeep.storage-policy");

/// The type of the rules that choose a file's policy.
pub const STORAGE_RULE: TypeName = TypeName::from_static("org.nibkeep.storage-rule");

/// A file's own choice of policy: a reference to it.
pub const POLICY: FieldName = FieldName::from_static("org.nibkeep.policy");

/// How many copies the data of a policy needs, as an integer.
pub(crate) const POLICY_COPIES: FieldName = FieldName::from_static("org.nibkeep.policy-copies");

/// How many of the copies are outside the home, as an integer.
pub(crate) const POLICY_AWAY: FieldName = FieldName::from_static("org.nibkeep.policy-away");

/// How many of the copies are on archive nodes, as an integer.
pub(crate) const POLICY_ARCHIVED: FieldName = FieldName::from_static("org.nibkeep.policy-archived");

/// Where devices keep the data of a policy: `store` or `in-place`.
pub(crate) const POLICY_KEPT: FieldName = FieldName::from_static("org.nibkeep.policy-kept");

/// The policy that a rule chooses: a reference to it.
pub(crate) const RULE_POLICY: FieldName = FieldName::from_static("org.nibkeep.rule-policy");

/// A condition of a rule: the file has this type, as text.
pub(crate) const RULE_TYPE: FieldName = FieldName::from_static("org.nibkeep.rule-type");

/// A condition of a rule: the file has a value of this field, as text.
pub(crate) const RULE_HAS: FieldName = FieldName::from_static("org.nibkeep.rule-has");

/// A condition of a rule, with [`RULE_DEVICE`]: every place the file was found is in this
/// folder or below it, as text, on that device.
pub(crate) const RULE_FOLDER: FieldName = FieldName::from_static("org.nibkeep.rule-folder");

/// The device whose folder [`RULE_FOLDER`] names: a reference to it.
pub(crate) const RULE_DEVICE: FieldName = FieldName::from_static("org.nibkeep.rule-device");

/// A condition of a rule: the file's media type is this one or starts with it, as text.
pub(crate) const RULE_MEDIA_TYPE: FieldName = FieldName::from_static("org.nibkeep.rule-media-type");

/// A condition of a rule: the file is larger than this many bytes, as an integer.
pub(crate) const RULE_LARGER_THAN: FieldName =
	FieldName::from_static("org.nibkeep.rule-larger-than");

/// Not a condition: adding a file larger than this many bytes, as an integer, that the rule
/// chooses for asks whether it can be got again.
pub(crate) const RULE_ASKS_ABOVE: FieldName = FieldName::from_static("org.nibkeep.rule-asks-above");

/// The fields of policies and rules, and a file's own choice, which only the engine's
/// policy functions write.
const OWN_FIELDS: [FieldName; 13] = [
	POLICY,
	POLICY_COPIES,
	POLICY_AWAY,
	POLICY_ARCHIVED,
	POLICY_KEPT,
	RULE_POLICY,
	RULE_TYPE,
	RULE_HAS,
	RULE_FOLDER,
	RULE_DEVICE,
	RULE_MEDIA_TYPE,
	RULE_LARGER_THAN,
	RULE_ASKS_ABOVE,
];

/// Whether a field is one of those of policies and rules, which only the policy functions
/// write.
pub(crate) fn is_own_field(field: &FieldName) -> bool {
	OWN_FIELDS.contains(field) || super::backups::is_own_field(field)
}

/// Whether a type is one of those of policies and rules, whose entities only the policy
/// functions make.
pub(crate) fn is_own_type(name: &TypeName) -> bool {
	*name == STORAGE_POLICY || *name == STORAGE_RULE || *name == super::backups::BACKUP
}

/// The definitions of the types of policies and rules, which NibKeep defines in code.
pub(crate) fn types() -> [TypeDef; 2] {
	let integer = || FieldDef::new(ValueKind::Integer);
	let text = || FieldDef::new(ValueKind::Text);
	[
		TypeDef::new(STORAGE_POLICY, 1)
			.with_field(fields::NAME, FieldDef::new(ValueKind::Name))
			.with_field(POLICY_COPIES, integer())
			.with_field(POLICY_AWAY, integer())
			.with_field(POLICY_ARCHIVED, integer())
			.with_field(POLICY_KEPT, text()),
		TypeDef::new(STORAGE_RULE, 1)
			.with_field(fields::POSITION, text().last_writer_wins())
			.with_field(RULE_POLICY, FieldDef::new(ValueKind::Reference))
			.with_field(RULE_TYPE, text())
			.with_field(RULE_HAS, text())
			.with_field(RULE_FOLDER, text())
			.with_field(RULE_DEVICE, FieldDef::new(ValueKind::Reference))
			.with_field(RULE_MEDIA_TYPE, text())
			.with_field(RULE_LARGER_THAN, integer())
			.with_field(RULE_ASKS_ABOVE, integer()),
	]
}

/// Where devices keep the files of a policy. The store is the stricter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kept {
	/// Where the file already is: NibKeep records where, and its hash.
	InPlace,
	/// In NibKeep's store, sealed.
	Store,
}

impl Kept {
	/// The text that stands for it.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::InPlace => "in-place",
			Self::Store => "store",
		}
	}
}

/// What a policy asks of the copies of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Needs {
	/// How many copies, at least 1.
	pub copies: u32,
	/// How many of them outside the home.
	pub away: u32,
	/// How many of them on archive nodes.
	pub archived: u32,
	/// Where devices keep them.
	pub kept: Kept,
}

impl Needs {
	/// What the Critical policy asks by default: three copies, one of them away from home,
	/// in the store; the space's changes count against it where it is gone.
	pub const CRITICAL: Self = Self {
		copies: 3,
		away: 1,
		archived: 0,
		kept: Kept::Store,
	};

	/// One copy, in the store: what a content needs that no file holds, as a thumbnail's,
	/// when a device that drops its space removes its copy, as every replica keeps it.
	pub const ONE: Self = Self {
		copies: 1,
		away: 0,
		archived: 0,
		kept: Kept::Store,
	};

	/// What a file needs when no policy is chosen for it: two copies, one of them on an
	/// archive node, in the store, as Standard asks.
	pub const FALLBACK: Self = Self {
		copies: 2,
		away: 0,
		archived: 1,
		kept: Kept::Store,
	};

	/// What a file lacks of this that has copies that count on `disks` disks, `away` of
	/// them away from home and `archived` of them archive nodes'.
	#[must_use]
	pub fn lacks(&self, disks: usize, away: usize, archived: usize) -> Lacks {
		let needs = |wanted: u32| usize::try_from(wanted).unwrap_or(usize::MAX);
		Lacks {
			copies: disks < needs(self.copies.max(1)),
			away: away < needs(self.away),
			archived: archived < needs(self.archived),
		}
	}

	/// What a file lacks of this whose copies are `copies`, as [`Library::copies_of`] gives
	/// them: those that count, by their disks' marks.
	#[must_use]
	pub fn lacks_of(&self, copies: &[super::accounts::FileCopy]) -> Lacks {
		let counting = || copies.iter().filter(|copy| copy.counts);
		self.lacks(
			counting().count(),
			counting().filter(|copy| copy.disk.away).count(),
			counting().filter(|copy| copy.disk.archive).count(),
		)
	}

	/// The strictest of two: the most copies of each kind, and the store over in place.
	#[must_use]
	pub fn strictest(self, other: Self) -> Self {
		Self {
			copies: self.copies.max(other.copies),
			away: self.away.max(other.away),
			archived: self.archived.max(other.archived),
			kept: self.kept.max(other.kept),
		}
	}
}

/// What the copies of a file lack of what its policy needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lacks {
	/// It is on fewer disks than the policy needs.
	pub copies: bool,
	/// Fewer of its copies are away from home than the policy needs.
	pub away: bool,
	/// Fewer of its copies are on archive nodes than the policy needs.
	pub archived: bool,
}

impl Lacks {
	/// Whether it lacks anything.
	#[must_use]
	pub fn any(self) -> bool {
		self.copies || self.away || self.archived
	}
}

/// A storage policy of a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
	/// Its entity.
	pub id: EntityId,
	/// Its name, if it has one.
	pub name: Option<String>,
	/// What it asks, the strictest of its values where they compete.
	pub needs: Needs,
}

/// A folder on a device, which a rule names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
	/// The folder, as the device's file system resolves it.
	pub path: String,
	/// The device.
	pub device: EntityId,
}

/// The conditions of a rule, which must all hold for it to choose. A rule without any
/// chooses for every file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
	/// The file has this type.
	pub of_type: Option<TypeName>,
	/// The file has a value of this field.
	pub has: Option<FieldName>,
	/// Every place the file was found is in this folder, or below it, on its device.
	pub folder: Option<Folder>,
	/// The file's media type is this one, or starts with it.
	pub media_type: Option<String>,
	/// The file is larger than this many bytes.
	pub larger_than: Option<u64>,
}

impl Conditions {
	/// Whether the rule has no conditions, and so chooses for every file.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		*self == Self::default()
	}

	/// Whether every condition holds for a file.
	fn hold(&self, facts: &Facts) -> bool {
		self.of_type
			.as_ref()
			.is_none_or(|name| facts.types.contains(name))
			&& self
				.has
				.as_ref()
				.is_none_or(|field| facts.fields.contains(field))
			&& self.folder.as_ref().is_none_or(|folder| {
				!facts.places.is_empty()
					&& facts.places.iter().all(|(device, path)| {
						*device == folder.device && Path::new(path).starts_with(&folder.path)
					})
			})
			&& self.media_type.as_ref().is_none_or(|prefix| {
				facts
					.media_type
					.as_ref()
					.is_some_and(|media_type| media_type.starts_with(prefix.as_str()))
			})
			&& self
				.larger_than
				.is_none_or(|bytes| facts.size.is_some_and(|size| size > bytes))
	}
}

/// A rule of a space, which chooses a policy for the files that it holds for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
	/// Its entity.
	pub id: EntityId,
	/// The policy it chooses, if it names one that is not deleted.
	pub policy: Option<EntityId>,
	/// Its conditions.
	pub conditions: Conditions,
	/// Adding a file larger than this many bytes that the rule chooses for asks whether the
	/// file can be got again.
	pub asks_above: Option<u64>,
	/// Its position key, if it has a valid one.
	position: Option<String>,
}

/// A rule to add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRule {
	/// The policy it chooses.
	pub policy: EntityId,
	/// The file has this type.
	pub of_type: Option<TypeName>,
	/// The file has a value of this field.
	pub has: Option<FieldName>,
	/// Every place the file was found is in this folder on this device, or below it.
	pub folder: Option<std::path::PathBuf>,
	/// The file's media type is this one, or starts with it.
	pub media_type: Option<String>,
	/// The file is larger than this many bytes.
	pub larger_than: Option<u64>,
	/// Adding a larger file that the rule chooses for asks whether it can be got again.
	pub asks_above: Option<u64>,
}

impl NewRule {
	/// A rule that chooses `policy` for every file.
	#[must_use]
	pub const fn new(policy: EntityId) -> Self {
		Self {
			policy,
			of_type: None,
			has: None,
			folder: None,
			media_type: None,
			larger_than: None,
			asks_above: None,
		}
	}
}

/// Where a rule goes among the rules of a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
	/// Before every other rule, where it wins over them.
	First,
	/// After every other rule.
	Last,
	/// Just before this rule.
	Before(EntityId),
	/// Just after this rule.
	After(EntityId),
	/// Before the rule without conditions at the end, which chooses for every other file,
	/// if there is one, or else last.
	BeforeTheRest,
}

/// What chose a file's policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChosenBy {
	/// The file's own choice.
	Own,
	/// This rule, the first whose conditions held.
	Rule(EntityId),
	/// Nothing: no rule held, and the file needs what [`Needs::FALLBACK`] says.
	Fallback,
}

/// The policy chosen for a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
	/// The policy, unless none was chosen.
	pub policy: Option<EntityId>,
	/// What the file needs.
	pub needs: Needs,
	/// What chose it.
	pub by: ChosenBy,
	/// Adding a file larger than this many bytes asks whether it can be got again, as the
	/// rule that chose says.
	pub asks_above: Option<u64>,
}

/// How a policy is met for the files it is chosen for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyCount {
	/// The policy, or `None` for the files that no rule chooses for.
	pub policy: Option<EntityId>,
	/// Its name, if it has one.
	pub name: Option<String>,
	/// What it asks.
	pub needs: Needs,
	/// The files it is chosen for.
	pub files: usize,
	/// Those that lack copies: every file with fewer disks than the policy needs, or fewer
	/// of them away from home or on archive nodes.
	pub short: usize,
	/// Which they are, for an app to list.
	pub lacking: Vec<EntityId>,
	/// Those on fewer disks than the policy needs.
	pub few_copies: usize,
	/// Those with fewer copies away from home than the policy needs.
	pub few_away: usize,
	/// Those with fewer copies on archive nodes than the policy needs.
	pub few_archived: usize,
	/// Those with a copy that does not count, as it was last checked more than 8 weeks ago
	/// ([technical design: checking and repair]).
	///
	/// [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair
	pub unchecked: usize,
	/// Those of which this device holds no copy at all: not in the store, and not where they
	/// were found, unchanged.
	pub none_here: Vec<EntityId>,
	/// Those that the policy keeps in the store, which this device holds only where they
	/// are.
	pub not_stored: Vec<EntityId>,
}

/// What a policy needs, in policy check order, and the files that do not meet it, with
/// copies counted by disk across devices ([technical design: where the copies are]).
///
/// [technical design: where the copies are]: ../../../../docs/Technical-design.md#96-where-the-copies-are
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyCheck {
	/// Each policy that a file is chosen for, by name, and the files that no rule chooses
	/// for last.
	pub policies: Vec<PolicyCount>,
	/// The disks that hold copies, this replica's first, as their replicas' accounts say.
	pub disks: Vec<super::accounts::Disk>,
	/// How the space's changes are held.
	pub changes: super::accounts::ChangeCopies,
	/// Each file counted, in the order checked: how its copies meet its policy, for an app
	/// to list them all.
	pub files: Vec<CheckedFile>,
}

/// How the copies of a file meet the policy chosen for it, as [`Library::check_policies`]
/// counts them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedFile {
	/// The file.
	pub file: EntityId,
	/// The policy chosen for it, or `None` if no rule chooses one.
	pub policy: Option<EntityId>,
	/// Its size in bytes, as it records it.
	pub size: Option<u64>,
	/// The disks whose copies of it count.
	pub disks: usize,
	/// How many of them are away from home.
	pub away: usize,
	/// How many of them are archive nodes'.
	pub archived: usize,
	/// What its copies lack of its policy.
	pub lacks: Lacks,
	/// Whether a disk holds a copy of it that does not count, as it was last checked more
	/// than 8 weeks ago.
	pub unchecked: bool,
}

/// What rules look at in a file.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
	pub(crate) types: BTreeSet<TypeName>,
	pub(crate) fields: BTreeSet<FieldName>,
	pub(crate) media_type: Option<String>,
	pub(crate) size: Option<u64>,
	/// The places it was found: each device and path.
	pub(crate) places: Vec<(EntityId, String)>,
}

/// The policies and rules of a space, read once for many choices.
pub(crate) struct Policies {
	policies: BTreeMap<EntityId, Policy>,
	rules: Vec<Rule>,
}

impl Policies {
	/// Reads the policies and rules of a space's state.
	pub(crate) fn read<S: Statements>(
		state: &State<S>,
		policies: impl IntoIterator<Item = EntityId>,
		rules: impl IntoIterator<Item = EntityId>,
	) -> Result<Self, NodeError>
	where
		NodeError: From<S::Error>,
	{
		let mut read = BTreeMap::new();
		for entity in policies {
			let entity = state.canonical(entity)?;
			if state.is_deleted(entity)? || !state.types(entity)?.contains(&STORAGE_POLICY) {
				continue;
			}
			read.insert(entity, read_policy(state, entity)?);
		}
		let mut found = Vec::new();
		for entity in rules {
			let entity = state.canonical(entity)?;
			if state.is_deleted(entity)? || !state.types(entity)?.contains(&STORAGE_RULE) {
				continue;
			}
			found.push(read_rule(state, entity, &read)?);
		}
		found.sort_by(|a, b| {
			(a.position.is_none(), &a.position, a.id).cmp(&(
				b.position.is_none(),
				&b.position,
				b.id,
			))
		});
		found.dedup_by_key(|rule| rule.id);
		Ok(Self {
			policies: read,
			rules: found,
		})
	}

	/// The policy chosen for a file, whose own choices are `own`.
	pub(crate) fn choose(&self, own: &[EntityId], facts: &Facts) -> Choice {
		if let Some(policy) = self.strictest(own) {
			return Choice {
				policy: Some(policy.id),
				needs: policy.needs,
				by: ChosenBy::Own,
				asks_above: None,
			};
		}
		for rule in &self.rules {
			if let Some(policy) = rule.policy.and_then(|id| self.policies.get(&id))
				&& rule.conditions.hold(facts)
			{
				return Choice {
					policy: Some(policy.id),
					needs: policy.needs,
					by: ChosenBy::Rule(rule.id),
					asks_above: rule.asks_above,
				};
			}
		}
		Choice {
			policy: None,
			needs: Needs::FALLBACK,
			by: ChosenBy::Fallback,
			asks_above: None,
		}
	}

	/// Whether a rule asks about files of this size: whether any rule asks above a smaller
	/// one.
	pub(crate) fn asks_about(&self, size: u64) -> bool {
		self.rules
			.iter()
			.any(|rule| rule.asks_above.is_some_and(|limit| size > limit))
	}

	/// Whether the space has this policy, not deleted.
	pub(crate) fn has_policy(&self, id: EntityId) -> bool {
		self.policies.contains_key(&id)
	}

	/// Whether the space has this rule, not deleted.
	pub(crate) fn has_rule(&self, id: EntityId) -> bool {
		self.rules.iter().any(|rule| rule.id == id)
	}

	/// The strictest of these policies that are not deleted, if any is.
	fn strictest(&self, ids: &[EntityId]) -> Option<&Policy> {
		ids.iter()
			.filter_map(|id| self.policies.get(id))
			.max_by_key(|policy| (policy.needs, std::cmp::Reverse(policy.id)))
	}

	/// The fields that rules ask a file to have a value of.
	pub(crate) fn fields_asked(&self) -> BTreeSet<FieldName> {
		self.rules
			.iter()
			.filter_map(|rule| rule.conditions.has.clone())
			.collect()
	}
}

/// Reads a policy, the strictest of its values where they compete. A policy that says
/// nothing of a number needs none of it, and one copy at least; one that does not say
/// where its files are kept keeps them in the store.
fn read_policy<S: Statements>(state: &State<S>, entity: EntityId) -> Result<Policy, NodeError>
where
	NodeError: From<S::Error>,
{
	let most = |field: &FieldName| -> Result<u32, NodeError> {
		Ok(state
			.current(entity, field)?
			.iter()
			.filter_map(|statement| match statement.value {
				Value::Integer(n) => u32::try_from(n).ok(),
				_ => None,
			})
			.max()
			.unwrap_or(0))
	};
	let kept: Vec<String> = state
		.current(entity, &POLICY_KEPT)?
		.iter()
		.filter_map(|statement| statement.value.as_text().map(str::to_owned))
		.collect();
	let kept = if kept.is_empty() || kept.iter().any(|kept| kept != Kept::InPlace.as_str()) {
		Kept::Store
	} else {
		Kept::InPlace
	};
	Ok(Policy {
		id: entity,
		name: first_name(state, entity)?,
		needs: Needs {
			copies: most(&POLICY_COPIES)?.max(1),
			away: most(&POLICY_AWAY)?,
			archived: most(&POLICY_ARCHIVED)?,
			kept,
		},
	})
}

/// Reads a rule. Where a field has competing values, the rule takes the first by ID, and
/// the strictest of the policies it names.
fn read_rule<S: Statements>(
	state: &State<S>,
	entity: EntityId,
	policies: &BTreeMap<EntityId, Policy>,
) -> Result<Rule, NodeError>
where
	NodeError: From<S::Error>,
{
	let first = |field: &FieldName| -> Result<Option<Value>, NodeError> {
		Ok(state
			.current(entity, field)?
			.iter()
			.min_by_key(|statement| statement.id)
			.map(|statement| statement.value.clone()))
	};
	let text = |field: &FieldName| -> Result<Option<String>, NodeError> {
		Ok(first(field)?.and_then(|value| value.as_text().map(str::to_owned)))
	};
	let bytes = |field: &FieldName| -> Result<Option<u64>, NodeError> {
		Ok(match first(field)? {
			Some(Value::Integer(n)) => u64::try_from(n).ok(),
			_ => None,
		})
	};
	let mut named = Vec::new();
	for statement in state.current(entity, &RULE_POLICY)? {
		if let Some(policy) = statement.value.as_reference() {
			named.push(state.canonical(policy)?);
		}
	}
	let policy = named
		.iter()
		.filter_map(|id| policies.get(id))
		.max_by_key(|policy| (policy.needs, std::cmp::Reverse(policy.id)))
		.map(|policy| policy.id);
	let device = match first(&RULE_DEVICE)? {
		Some(Value::Reference(device)) => Some(state.canonical(device)?),
		_ => None,
	};
	let folder = match (text(&RULE_FOLDER)?, device) {
		(Some(path), Some(device)) => Some(Folder { path, device }),
		_ => None,
	};
	let conditions = Conditions {
		of_type: text(&RULE_TYPE)?.and_then(|name| name.parse().ok()),
		has: text(&RULE_HAS)?.and_then(|name| name.parse().ok()),
		folder,
		media_type: text(&RULE_MEDIA_TYPE)?,
		larger_than: bytes(&RULE_LARGER_THAN)?,
	};
	// A condition that is there but cannot be read holds for no file, so the rule chooses
	// nothing, rather than for every file, as if it had no such condition.
	let readable = [
		(RULE_TYPE, conditions.of_type.is_some()),
		(RULE_HAS, conditions.has.is_some()),
		(RULE_FOLDER, conditions.folder.is_some()),
		(RULE_DEVICE, conditions.folder.is_some()),
		(RULE_MEDIA_TYPE, conditions.media_type.is_some()),
		(RULE_LARGER_THAN, conditions.larger_than.is_some()),
	]
	.into_iter()
	.try_fold(true, |readable, (field, read)| {
		Ok::<_, NodeError>(readable && (read || first(&field)?.is_none()))
	})?;
	Ok(Rule {
		id: entity,
		policy: policy.filter(|_| readable),
		conditions,
		asks_above: bytes(&RULE_ASKS_ABOVE)?,
		position: last_position(state, entity)?,
	})
}

/// A rule's position key, if it has a valid one: the one written last, as in ordered lists.
fn last_position<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	let definition = FieldDef::new(ValueKind::Text).last_writer_wins();
	Ok(
		match state.resolve(entity, &fields::POSITION, Some(&definition))? {
			Resolution::Values(values) | Resolution::Competing(values) => values
				.first()
				.and_then(|statement| statement.value.as_text())
				.filter(|key| is_position(key))
				.map(str::to_owned),
		},
	)
}

/// The name an entity shows first, by ID among competing ones.
pub(crate) fn first_name<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Option<String>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(entity, &fields::NAME)?
		.iter()
		.min_by_key(|statement| statement.id)
		.and_then(|statement| statement.value.as_name_text().map(str::to_owned)))
}

/// What rules look at in a file of a state: its types, fields, media type and size, and
/// the places its origins say it was found, of those that are not deleted.
pub(crate) fn facts_of<S: Statements>(state: &State<S>, file: EntityId) -> Result<Facts, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut facts = facts_but_places(state, file)?;
	facts.places = origins_of(state, file)?
		.into_iter()
		.map(|(_, device, path)| (device, path))
		.collect();
	Ok(facts)
}

/// What rules look at in a file of a state, but the places it was found.
fn facts_but_places<S: Statements>(state: &State<S>, file: EntityId) -> Result<Facts, NodeError>
where
	NodeError: From<S::Error>,
{
	let media_type = state
		.current(file, &MEDIA_TYPE)?
		.iter()
		.min_by_key(|statement| statement.id)
		.and_then(|statement| statement.value.as_text().map(str::to_owned));
	Ok(Facts {
		types: state.types(file)?,
		fields: state.fields(file)?,
		media_type,
		size: size_of(state, file)?,
		places: Vec::new(),
	})
}

/// The size of a file of a state, as it was recorded.
pub(crate) fn size_of<S: Statements>(
	state: &State<S>,
	file: EntityId,
) -> Result<Option<u64>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(file, &SIZE)?
		.iter()
		.find_map(|statement| match statement.value {
			Value::Integer(n) => u64::try_from(n).ok(),
			_ => None,
		}))
}

/// Whether a record of where a file's plain copy is, an origin or a checkout, stands in a
/// state: neither deleted nor moved to another space with its file, which records it then.
pub(crate) fn stands_here<S: Statements>(
	state: &State<S>,
	record: EntityId,
) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(!state.is_deleted(record)? && state.current(record, &fields::MOVED_INTO)?.is_empty())
}

/// The origins of a file of a state that stand there, neither deleted nor moved with the
/// file, each with the device and the path it says the file was found on.
pub(crate) fn origins_of<S: Statements>(
	state: &State<S>,
	file: EntityId,
) -> Result<Vec<(EntityId, EntityId, String)>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut found = Vec::new();
	let mut seen = BTreeSet::new();
	for statement in state.referrers_in(file, &ORIGIN_OF)? {
		let origin = state.canonical(statement.entity)?;
		if !seen.insert(origin)
			|| !stands_here(state, origin)?
			|| !state.types(origin)?.contains(&ORIGIN)
		{
			continue;
		}
		let devices: Vec<EntityId> = state
			.current(origin, &FOUND_ON)?
			.iter()
			.filter_map(|statement| statement.value.as_reference())
			.collect();
		for path in state.current(origin, &PATH)? {
			if let Some(path) = path.value.as_text() {
				for device in &devices {
					found.push((origin, state.canonical(*device)?, path.to_owned()));
				}
			}
		}
	}
	Ok(found)
}

/// An entity ID that a space's default policies and rules take, derived from the space's ID
/// and `label`, so that devices that install them at once install the same entities.
fn derived(space: SpaceId, label: &str) -> EntityId {
	let material = [space.as_bytes().as_slice(), label.as_bytes()].concat();
	EntityId::derived("nibkeep 2026-10 storage policies", &material)
}

/// The default policies of a space, by label.
const CRITICAL: &str = "critical";
const STANDARD: &str = "standard";
const REPLACEABLE: &str = "replaceable";

/// The default rules of a space, by label; a device's rule for its downloads folder adds
/// the device's ID to its label.
const FROM_A_CAMERA: &str = "from a camera";
const ANY_OTHER: &str = "any other file";
const DOWNLOADS: &str = "downloads";

/// The size above which the rule for any other file asks whether a file can be got again:
/// 1 GiB.
const ASKS_ABOVE: u64 = 1 << 30;

/// The IDs of a space's default policies, Critical, Standard and Replaceable, in that
/// order, whether or not they are installed.
pub(crate) fn default_policies(space: SpaceId) -> [EntityId; 3] {
	[CRITICAL, STANDARD, REPLACEABLE].map(|label| derived(space, label))
}

/// The default policy that the answer about a large file chooses for its folder:
/// Replaceable if the file can be got again, and Standard if not.
pub(crate) fn answer_policy(space: SpaceId, again: bool) -> EntityId {
	derived(space, if again { REPLACEABLE } else { STANDARD })
}

/// An operation that asserts a statement.
pub(crate) fn assert(
	entity: EntityId,
	field: FieldName,
	value: Value,
	rng: &mut impl CryptoRng,
) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		entity,
		field,
		value,
	))
}

/// The operations that make a policy.
fn make_policy(
	entity: EntityId,
	name: &str,
	needs: Needs,
	rng: &mut impl CryptoRng,
) -> Vec<Operation> {
	vec![
		assert(
			entity,
			fields::TYPE,
			Value::Text(STORAGE_POLICY.to_string()),
			rng,
		),
		assert(entity, fields::NAME, Value::Name(Name::new(name)), rng),
		assert(
			entity,
			POLICY_COPIES,
			Value::Integer(i64::from(needs.copies)),
			rng,
		),
		assert(
			entity,
			POLICY_AWAY,
			Value::Integer(i64::from(needs.away)),
			rng,
		),
		assert(
			entity,
			POLICY_ARCHIVED,
			Value::Integer(i64::from(needs.archived)),
			rng,
		),
		assert(
			entity,
			POLICY_KEPT,
			Value::Text(needs.kept.as_str().to_owned()),
			rng,
		),
	]
}

/// The operations that make a rule, at `position`, with its conditions; a folder is on
/// `device`.
fn make_rule(
	entity: EntityId,
	rule: &NewRule,
	folder: Option<(&str, EntityId)>,
	position: String,
	rng: &mut impl CryptoRng,
) -> Vec<Operation> {
	let mut operations = vec![
		assert(
			entity,
			fields::TYPE,
			Value::Text(STORAGE_RULE.to_string()),
			rng,
		),
		assert(entity, fields::POSITION, Value::Text(position), rng),
		assert(entity, RULE_POLICY, Value::Reference(rule.policy), rng),
	];
	if let Some(name) = &rule.of_type {
		operations.push(assert(
			entity,
			RULE_TYPE,
			Value::Text(name.to_string()),
			rng,
		));
	}
	if let Some(field) = &rule.has {
		operations.push(assert(
			entity,
			RULE_HAS,
			Value::Text(field.to_string()),
			rng,
		));
	}
	if let Some((path, device)) = folder {
		operations.push(assert(
			entity,
			RULE_FOLDER,
			Value::Text(path.to_owned()),
			rng,
		));
		operations.push(assert(entity, RULE_DEVICE, Value::Reference(device), rng));
	}
	if let Some(media_type) = &rule.media_type {
		operations.push(assert(
			entity,
			RULE_MEDIA_TYPE,
			Value::Text(media_type.clone()),
			rng,
		));
	}
	for (field, bytes) in [
		(RULE_LARGER_THAN, rule.larger_than),
		(RULE_ASKS_ABOVE, rule.asks_above),
	] {
		if let Some(bytes) = bytes {
			operations.push(assert(
				entity,
				field,
				Value::Integer(i64::try_from(bytes).unwrap_or(i64::MAX)),
				rng,
			));
		}
	}
	operations
}

/// A folder as the file system resolves it, so that it compares with the paths where files
/// were found.
fn resolved_folder(folder: &Path) -> Result<String, NodeError> {
	let resolved = resolved_path(folder).map_err(|error| NodeError::Unreadable {
		path: folder.display().to_string(),
		reason: error.to_string(),
	})?;
	if !resolved.is_dir() {
		return Err(NodeError::Unreadable {
			path: folder.display().to_string(),
			reason: "not a folder".to_owned(),
		});
	}
	Ok(resolved.to_string_lossy().into_owned())
}

impl Library {
	/// Gives a space the default policies and rules, if it never had them, and this device
	/// a rule for its downloads folder, `downloads`, if it never had one ([technical design:
	/// storage policies]): Critical, Standard and Replaceable, and rules that choose Standard for a file
	/// from a camera, Replaceable for one from the downloads folder, and Standard for any
	/// other, asking above 1 GiB.
	///
	/// [technical design: storage policies]: ../../../../docs/Technical-design.md#93-storage-policies
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the change cannot be saved.
	pub fn install_policies(
		&mut self,
		space: SpaceId,
		downloads: Option<&Path>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let critical = derived(space, CRITICAL);
		let standard = derived(space, STANDARD);
		let replaceable = derived(space, REPLACEABLE);
		let any_other = derived(space, ANY_OTHER);
		let device = self.info.device;
		let downloads_rule = derived(space, &format!("{DOWNLOADS} {device}"));
		let (installed, has_downloads) = {
			let open = self.space(space)?;
			let storage = open.stored.log().state().storage();
			// A space with a policy, such as one an import brought, has what it needs.
			let policies = open
				.stored
				.find(&fields::TYPE, &Value::Text(STORAGE_POLICY.to_string()))?;
			(
				!storage.of_entity(critical)?.is_empty() || !policies.is_empty(),
				!storage.of_entity(downloads_rule)?.is_empty(),
			)
		};
		let downloads = match downloads {
			Some(folder) if !has_downloads => resolved_folder(folder).ok(),
			_ => None,
		};
		let mut operations = Vec::new();
		if !installed {
			for (entity, name, needs) in [
				(critical, "Critical", Needs::CRITICAL),
				(standard, "Standard", Needs::FALLBACK),
				(
					replaceable,
					"Replaceable",
					Needs {
						copies: 1,
						away: 0,
						archived: 0,
						kept: Kept::InPlace,
					},
				),
			] {
				operations.extend(make_policy(entity, name, needs, rng));
			}
			let first = position_between(None, None).unwrap_or_else(|| "V".to_owned());
			let last = position_between(Some(&first), None).unwrap_or_else(|| "k".to_owned());
			let camera = NewRule {
				has: Some(CAMERA_MAKE),
				..NewRule::new(standard)
			};
			operations.extend(make_rule(
				derived(space, FROM_A_CAMERA),
				&camera,
				None,
				first,
				rng,
			));
			let rest = NewRule {
				asks_above: Some(ASKS_ABOVE),
				..NewRule::new(standard)
			};
			operations.extend(make_rule(any_other, &rest, None, last, rng));
		}
		if let Some(folder) = downloads {
			let rule = NewRule::new(replaceable);
			let position = if installed {
				let (position, renumbered) = self.placing(space, Place::BeforeTheRest, None)?;
				operations.extend(self.renumbering(space, renumbered, rng)?);
				position
			} else {
				// Between the two rules just made.
				let first = position_between(None, None).unwrap_or_else(|| "V".to_owned());
				let last = position_between(Some(&first), None).unwrap_or_else(|| "k".to_owned());
				position_between(Some(&first), Some(&last)).unwrap_or(last)
			};
			operations.extend(make_rule(
				downloads_rule,
				&rule,
				Some((&folder, device)),
				position,
				rng,
			));
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// The policies and rules of a space, as they are now.
	pub(crate) fn read_policies(&self, space: SpaceId) -> Result<Policies, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let of_type = |name: &TypeName| {
			open.stored
				.find(&fields::TYPE, &Value::Text(name.to_string()))
		};
		Policies::read(state, of_type(&STORAGE_POLICY)?, of_type(&STORAGE_RULE)?)
	}

	/// The policies of a space that are not deleted, ordered by name without regard to case.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn policies(&self, space: SpaceId) -> Result<Vec<Policy>, NodeError> {
		let mut policies: Vec<Policy> = self.read_policies(space)?.policies.into_values().collect();
		policies.sort_by_cached_key(|policy| {
			(
				policy.name.as_ref().map(|name| name.to_lowercase()),
				policy.id,
			)
		});
		Ok(policies)
	}

	/// The rules of a space that are not deleted, in the order in which they choose.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn rules(&self, space: SpaceId) -> Result<Vec<Rule>, NodeError> {
		Ok(self.read_policies(space)?.rules)
	}

	/// The policy of a space that `reference` names: its name, without regard to case, its
	/// ID, or a prefix of its ID of at least six characters.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if no policy is named so, [`NodeError::Ambiguous`] if
	/// several are, and an error if the space cannot be read.
	pub fn find_policy(&self, space: SpaceId, reference: &str) -> Result<EntityId, NodeError> {
		let policies = self.policies(space)?;
		let lowered = reference.to_lowercase();
		let by_name: Vec<EntityId> = policies
			.iter()
			.filter(|policy| {
				policy
					.name
					.as_ref()
					.is_some_and(|name| name.to_lowercase() == lowered)
			})
			.map(|policy| policy.id)
			.collect();
		let found = if by_name.is_empty() {
			policies
				.iter()
				.filter(|policy| {
					let id = policy.id.to_string();
					id == lowered || (lowered.len() >= 6 && id.starts_with(&lowered))
				})
				.map(|policy| policy.id)
				.collect()
		} else {
			by_name
		};
		match found.as_slice() {
			[] => Err(NodeError::NoEntity(reference.to_owned())),
			[one] => Ok(*one),
			_ => Err(NodeError::Ambiguous {
				reference: reference.to_owned(),
				candidates: found,
			}),
		}
	}

	/// Adds a policy to a space.
	///
	/// # Errors
	///
	/// Returns [`NodeError::EmptyName`] if the name is empty, and an error if the library
	/// has no such space, or the change cannot be saved.
	pub fn add_policy(
		&mut self,
		space: SpaceId,
		name: &str,
		needs: Needs,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		if name.trim().is_empty() {
			return Err(NodeError::EmptyName);
		}
		self.space(space)?;
		let entity = EntityId::random(rng);
		let operations = make_policy(
			entity,
			name.trim(),
			Needs {
				copies: needs.copies.max(1),
				..needs
			},
			rng,
		);
		self.commit(space, operations, wall_millis, rng)?;
		Ok(entity)
	}

	/// Changes a policy of a space: each value given replaces the values the policy has of
	/// it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if `policy` is no policy of the space,
	/// [`NodeError::EmptyName`] if the name is empty, and an error if the change cannot be
	/// saved.
	pub fn edit_policy(
		&mut self,
		space: SpaceId,
		policy: EntityId,
		edit: &PolicyEdit,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if edit
			.name
			.as_ref()
			.is_some_and(|name| name.trim().is_empty())
		{
			return Err(NodeError::EmptyName);
		}
		self.check_policy(space, policy)?;
		let mut values = Vec::new();
		if let Some(name) = &edit.name {
			values.push((fields::NAME, Value::Name(Name::new(name.trim()))));
		}
		for (field, number) in [
			(POLICY_COPIES, edit.copies.map(|copies| copies.max(1))),
			(POLICY_AWAY, edit.away),
			(POLICY_ARCHIVED, edit.archived),
		] {
			if let Some(number) = number {
				values.push((field, Value::Integer(i64::from(number))));
			}
		}
		if let Some(kept) = edit.kept {
			values.push((POLICY_KEPT, Value::Text(kept.as_str().to_owned())));
		}
		let operations = self.replacing(space, policy, values, rng)?;
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Adds a rule to a space, at `place` among its rules. A folder is on this device.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if the rule's policy is no policy of the space,
	/// [`NodeError::Unreadable`] if its folder is not one, and an error if the change cannot
	/// be saved.
	pub fn add_rule(
		&mut self,
		space: SpaceId,
		rule: &NewRule,
		place: Place,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.check_policy(space, rule.policy)?;
		let folder = rule.folder.as_deref().map(resolved_folder).transpose()?;
		let (position, renumbered) = self.placing(space, place, None)?;
		let entity = EntityId::random(rng);
		let device = self.info.device;
		let mut operations = self.renumbering(space, renumbered, rng)?;
		operations.extend(make_rule(
			entity,
			rule,
			folder.as_deref().map(|folder| (folder, device)),
			position,
			rng,
		));
		self.commit(space, operations, wall_millis, rng)?;
		Ok(entity)
	}

	/// Removes a rule of a space: deletes it, so that it chooses nothing until it is
	/// restored.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if `rule` is no rule of the space, and an error if
	/// the change cannot be saved.
	pub fn remove_rule(
		&mut self,
		space: SpaceId,
		rule: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_rule(space, rule)?;
		let operations = vec![assert(rule, fields::DELETED, Value::Boolean(true), rng)];
		self.commit(space, operations, wall_millis, rng)
	}

	/// Removes a policy of a space: deletes it, so that rules that choose it choose nothing,
	/// and files that chose it have their policy chosen by the rules, until it is restored.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if `policy` is no policy of the space, and an error if
	/// the change cannot be saved.
	pub fn remove_policy(
		&mut self,
		space: SpaceId,
		policy: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_policy(space, policy)?;
		let operations = vec![assert(policy, fields::DELETED, Value::Boolean(true), rng)];
		self.commit(space, operations, wall_millis, rng)
	}

	/// Moves a rule of a space to `place` among the others.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if `rule` is no rule of the space, and an error if
	/// the change cannot be saved.
	pub fn move_rule(
		&mut self,
		space: SpaceId,
		rule: EntityId,
		place: Place,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_rule(space, rule)?;
		if place == Place::Before(rule) || place == Place::After(rule) {
			return Ok(());
		}
		let (position, mut renumbered) = self.placing(space, place, Some(rule))?;
		renumbered.push((rule, position));
		let operations = self.renumbering(space, renumbered, rng)?;
		self.commit(space, operations, wall_millis, rng)
	}

	/// Sets an entity's own choice of policy, which wins over the rules, or clears it with
	/// `None`, so that the rules choose again.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoEntity`] if `policy` is no policy of the space, and an error
	/// if the change cannot be saved.
	pub fn set_policy(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		policy: Option<EntityId>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if let Some(policy) = policy {
			self.check_policy(space, policy)?;
		}
		let operations = if let Some(policy) = policy {
			self.replacing(space, entity, vec![(POLICY, Value::Reference(policy))], rng)?
		} else {
			let state = self.space(space)?.stored.log().state();
			let current: Vec<StatementId> = state
				.current(entity, &POLICY)?
				.iter()
				.map(|statement| statement.id)
				.collect();
			if current.is_empty() {
				Vec::new()
			} else {
				vec![Operation::retract(entity, current)]
			}
		};
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// The policy chosen for an entity of a space, as its own choice or the rules choose.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn policy_of(&self, space: SpaceId, entity: EntityId) -> Result<Choice, NodeError> {
		let policies = self.read_policies(space)?;
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let entity = state.canonical(entity)?;
		let own = own_choices(state, entity)?;
		Ok(policies.choose(&own, &facts_of(state, entity)?))
	}

	/// How the policies of a space are met for its files, as of `wall_millis`: for each
	/// policy, how many files it is chosen for, and which of them lack copies; the disks that
	/// hold copies; and how the space's changes are held. Copies count disks: this device's
	/// are on its disk, as it holds them now, in the store, where they were found, unchanged,
	/// and as unchanged copies of checkouts of their current content; the other devices' are
	/// on theirs, as their accounts last said ([technical design: where the copies are]). A
	/// copy counts only if it was checked in the 8 weeks before `wall_millis`
	/// ([technical design: checking and repair]).
	///
	/// [technical design: where the copies are]: ../../../../docs/Technical-design.md#96-where-the-copies-are
	/// [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn check_policies(
		&self,
		space: SpaceId,
		wall_millis: u64,
	) -> Result<PolicyCheck, NodeError> {
		self.check_policies_in_parts(space, (PART, false), wall_millis)
	}

	/// How the policies of a space would be met without this replica's disk, for the files
	/// whose copies this disk holds, sealed or where they are: as a drop of the space would
	/// leave them ([technical design: devices that choose their spaces]).
	///
	/// [technical design: devices that choose their spaces]: ../../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces
	pub(crate) fn policies_without_this_disk(
		&self,
		space: SpaceId,
		wall_millis: u64,
	) -> Result<Vec<PolicyCount>, NodeError> {
		Ok(self
			.check_policies_in_parts(space, (PART, true), wall_millis)?
			.policies)
	}

	/// What a space's Critical policy asks, which its changes are counted against.
	pub(crate) fn critical_needs(&self, space: SpaceId) -> Result<Needs, NodeError> {
		Ok(self
			.read_policies(space)?
			.policies
			.get(&derived(space, CRITICAL))
			.map_or(Needs::CRITICAL, |policy| policy.needs))
	}

	/// Checks the policies of a space as [`Library::check_policies`] does, reading the space
	/// part by part, `part` entities at a time: first where its files were found, which
	/// their origins say, in short, then the files, with the fields that rules look at. With
	/// `without_own`, it counts only the files whose copies this replica's disk holds, as
	/// though it did not.
	fn check_policies_in_parts(
		&self,
		space: SpaceId,
		(part, without_own): (usize, bool),
		wall_millis: u64,
	) -> Result<PolicyCheck, NodeError> {
		let policies = self.read_policies(space)?;
		let copies = self.copies_by_disk(space, wall_millis)?;
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let of_type = |name: &TypeName| -> Result<Vec<EntityId>, NodeError> {
			Ok(open
				.stored
				.find(&fields::TYPE, &Value::Text(name.to_string()))?)
		};
		// Places are kept in full on this device, and on those that folder rules name.
		let device = self.info.device;
		let mut devices_needed: BTreeSet<EntityId> = policies
			.rules
			.iter()
			.filter_map(|rule| rule.conditions.folder.as_ref().map(|folder| folder.device))
			.collect();
		devices_needed.insert(device);
		let places = places_by_file(state, &of_type(&ORIGIN)?, (&devices_needed, part))?;
		let mut asked: Vec<FieldName> = vec![
			fields::TYPE,
			fields::DELETED,
			fields::MERGED_INTO,
			POLICY,
			CONTENT,
			SIZE,
			MEDIA_TYPE,
		];
		for field in policies.fields_asked() {
			if !asked.contains(&field) {
				asked.push(field);
			}
		}
		let mut seen = BTreeSet::new();
		let mut counts: BTreeMap<Option<EntityId>, PolicyCount> = BTreeMap::new();
		let mut checked_files = Vec::new();
		by_parts(
			state,
			&of_type(&FILE)?,
			(&asked, part),
			&mut |files, ids| {
				for &file in ids {
					let file = files.canonical(file)?;
					if !seen.insert(file)
						|| files.is_deleted(file)?
						|| !files.types(file)?.contains(&FILE)
					{
						continue;
					}
					// Where the file, or any merged into it, was found.
					let mut found: Vec<&FoundAt> = Vec::new();
					for member in files.merged(file)? {
						found.extend(places.get(&member).into_iter().flatten());
					}
					let mut facts = facts_of_part(files, file)?;
					facts.places = found
						.iter()
						.map(|place| (place.device, place.path.clone()))
						.collect();
					let choice = policies.choose(&own_choices(files, file)?, &facts);
					let content =
						files
							.current(file, &CONTENT)?
							.iter()
							.find_map(|statement| match statement.value {
								Value::Blob(hash) => Some(hash),
								_ => None,
							});
					let stored = content.is_some_and(|hash| open.stored.holds_ordinary_blob(&hash));
					let members: Vec<EntityId> = files.merged(file)?.into_iter().collect();
					let in_place = copies.plain_here(&members, content);
					let sealed = self.ordinary_sealed(space, content)?;
					let (mut disks, old) = copies.holding(&sealed, (&members, content));
					if without_own && !disks.remove(&Copies::OWN) {
						continue;
					}
					let (away, archived) = copies.marked(&disks);
					checked_files.push(tally(
						&mut counts,
						&policies,
						(&choice, file, facts.size),
						(
							(stored, in_place),
							(disks.len(), away, archived, !old.is_empty()),
						),
					));
				}
				Ok(())
			},
		)?;
		let critical = policies
			.policies
			.get(&derived(space, CRITICAL))
			.map_or(Needs::CRITICAL, |policy| policy.needs);
		let mut check = checked(counts, checked_files);
		if without_own {
			check.disks = copies.disks;
			return Ok(check);
		}
		check.changes = self.change_copies(space, &copies, critical)?;
		check.disks = copies.disks;
		Ok(check)
	}

	/// The originals of a space's files on this device, each with its place and the content
	/// and size its origin recorded.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn origin_places_here(
		&self,
		space: SpaceId,
	) -> Result<Vec<super::checking::PlainPlace>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		// Those that moved with their file are the other space's.
		let mut origins = Vec::new();
		for origin in open
			.stored
			.find(&fields::TYPE, &Value::Text(ORIGIN.to_string()))?
		{
			if stands_here(state, state.canonical(origin)?)? {
				origins.push(origin);
			}
		}
		let device = self.info.device;
		let mut found = Vec::new();
		for (file, places) in places_by_file(state, &origins, (&BTreeSet::from([device]), PART))? {
			for place in places.iter().filter(|place| place.device == device) {
				let (Some(content), Some(size)) = (place.content, place.size) else {
					continue;
				};
				let untouched = place.holds(Some(content));
				found.push(super::checking::PlainPlace {
					file,
					path: place.path.clone(),
					content,
					untouched,
					size,
					seen: super::checking::seen_at(&place.path).filter(|_| untouched),
				});
			}
		}
		Ok(found)
	}

	/// Returns [`NodeError::NoEntity`] unless `policy` is a policy of the space that is not
	/// deleted.
	fn check_policy(&self, space: SpaceId, policy: EntityId) -> Result<(), NodeError> {
		if self.read_policies(space)?.policies.contains_key(&policy) {
			Ok(())
		} else {
			Err(NodeError::NoEntity(policy.to_string()))
		}
	}

	/// Returns [`NodeError::NoEntity`] unless `rule` is a rule of the space that is not
	/// deleted.
	fn check_rule(&self, space: SpaceId, rule: EntityId) -> Result<(), NodeError> {
		if self
			.read_policies(space)?
			.rules
			.iter()
			.any(|known| known.id == rule)
		{
			Ok(())
		} else {
			Err(NodeError::NoEntity(rule.to_string()))
		}
	}

	/// The position key for a rule at `place`, among the rules but `moving`, and the rules
	/// that need new keys for it to fit there: none, unless the rules on either side share a
	/// key, or one of them has no valid key, when every rule gets a new one, in its order.
	fn placing(
		&self,
		space: SpaceId,
		place: Place,
		moving: Option<EntityId>,
	) -> Result<(String, Vec<(EntityId, String)>), NodeError> {
		let rules: Vec<Rule> = self
			.read_policies(space)?
			.rules
			.into_iter()
			.filter(|rule| Some(rule.id) != moving)
			.collect();
		let index_of = |id: EntityId| {
			rules
				.iter()
				.position(|rule| rule.id == id)
				.ok_or_else(|| NodeError::NoEntity(id.to_string()))
		};
		// Where the rule goes: before the rule at this index, or last.
		let at = match place {
			Place::First => 0,
			Place::Last => rules.len(),
			Place::Before(id) => index_of(id)?,
			Place::After(id) => index_of(id)? + 1,
			Place::BeforeTheRest => match rules.last() {
				Some(last) if last.conditions.is_empty() => rules.len() - 1,
				_ => rules.len(),
			},
		};
		let neighbour = |index: Option<usize>| match index {
			None => Some(None),
			Some(index) => rules
				.get(index)
				.map_or(Some(None), |rule| rule.position.as_deref().map(Some)),
		};
		if let (Some(before), Some(after)) = (neighbour(at.checked_sub(1)), neighbour(Some(at)))
			&& let Some(key) = position_between(before, after)
		{
			return Ok((key, Vec::new()));
		}
		// No key fits between: every rule gets a new one, in order, the new one at its place.
		let mut keys = Vec::with_capacity(rules.len() + 1);
		for _ in 0..=rules.len() {
			let key = position_between(keys.last().map(String::as_str), None)
				.unwrap_or_else(|| "V".to_owned());
			keys.push(key);
		}
		let key = keys.remove(at);
		let renumbered = rules
			.iter()
			.zip(keys)
			.filter(|(rule, key)| rule.position.as_ref() != Some(key))
			.map(|(rule, key)| (rule.id, key))
			.collect();
		Ok((key, renumbered))
	}

	/// The operations that give rules new position keys.
	fn renumbering(
		&self,
		space: SpaceId,
		renumbered: Vec<(EntityId, String)>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let mut operations = Vec::new();
		for (rule, key) in renumbered {
			operations.extend(self.replacing(
				space,
				rule,
				vec![(fields::POSITION, Value::Text(key))],
				rng,
			)?);
		}
		Ok(operations)
	}

	/// The operations that set fields of an entity: each value replaces the current values
	/// of its field.
	pub(crate) fn replacing(
		&self,
		space: SpaceId,
		entity: EntityId,
		values: Vec<(FieldName, Value)>,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let mut operations = Vec::new();
		for (field, value) in values {
			let current: Vec<StatementId> = state
				.current(entity, &field)?
				.iter()
				.map(|statement| statement.id)
				.collect();
			let statement = Statement::new(StatementId::random(rng), entity, field, value);
			operations.push(Operation::replace(statement, current));
		}
		Ok(operations)
	}
}

/// The policies counted, in the order of a check: by name, and the files that no rule
/// chooses for last.
fn checked(
	counts: BTreeMap<Option<EntityId>, PolicyCount>,
	files: Vec<CheckedFile>,
) -> PolicyCheck {
	let mut checked: Vec<PolicyCount> = counts.into_values().collect();
	checked.sort_by_cached_key(|count| {
		(
			count.policy.is_none(),
			count.name.as_ref().map(|name| name.to_lowercase()),
			count.policy,
		)
	});
	PolicyCheck {
		policies: checked,
		files,
		..PolicyCheck::default()
	}
}

/// Counts a file under the policy chosen for it, `choice`: whether it lacks copies, by the
/// disks whose copies count, how many of them are away and archive nodes', and whether a
/// disk holds a copy that does not count, `unchecked`; whether this device holds none; or
/// holds it only where it was found though its policy keeps it in the store, by whether the
/// device holds it in the store, `stored`, or where it was found, unchanged, `in_place`.
/// Returns the file as counted, with its size.
fn tally(
	counts: &mut BTreeMap<Option<EntityId>, PolicyCount>,
	policies: &Policies,
	(choice, file, size): (&Choice, EntityId, Option<u64>),
	((stored, in_place), (disks, away, archived, unchecked)): (
		(bool, bool),
		(usize, usize, usize, bool),
	),
) -> CheckedFile {
	let count = counts.entry(choice.policy).or_insert_with(|| PolicyCount {
		policy: choice.policy,
		name: choice
			.policy
			.and_then(|id| policies.policies.get(&id))
			.and_then(|policy| policy.name.clone()),
		needs: choice.needs,
		files: 0,
		short: 0,
		lacking: Vec::new(),
		few_copies: 0,
		few_away: 0,
		few_archived: 0,
		unchecked: 0,
		none_here: Vec::new(),
		not_stored: Vec::new(),
	});
	count.files += 1;
	let lacks = choice.needs.lacks(disks, away, archived);
	count.few_copies += usize::from(lacks.copies);
	count.few_away += usize::from(lacks.away);
	count.few_archived += usize::from(lacks.archived);
	count.unchecked += usize::from(unchecked);
	if lacks.any() {
		count.short += 1;
		count.lacking.push(file);
	}
	if !stored && !in_place {
		count.none_here.push(file);
	} else if choice.needs.kept == Kept::Store && !stored {
		count.not_stored.push(file);
	}
	CheckedFile {
		file,
		policy: choice.policy,
		size,
		disks,
		away,
		archived,
		lacks,
		unchecked,
	}
}

/// Where a file was found, as an origin says, in short: on which device, at which path, when
/// it last changed there, and the content and size found there, if the origin says.
#[derive(Clone, Debug)]
struct FoundAt {
	device: EntityId,
	path: String,
	modified: Option<Value>,
	content: Option<Hash>,
	size: Option<u64>,
}

impl FoundAt {
	/// Whether the file is there still, unchanged, as the version `content`: with the
	/// content, size and modification time its origin recorded, as adding a file tells an
	/// unchanged one.
	fn holds(&self, content: Option<Hash>) -> bool {
		if self.content.is_none() || self.content != content {
			return false;
		}
		let Ok(metadata) = fs::metadata(&self.path) else {
			return false;
		};
		let modified = metadata.modified().ok().and_then(moment).map(Value::Date);
		metadata.is_file()
			&& self.size == Some(metadata.len())
			&& modified.is_some()
			&& modified == self.modified
	}
}

/// Where the files of a state were found, by the file each origin names, read from the
/// origins, `origins`, part by part: what an origin says of its file, in short, sits in
/// memory, and the origins themselves a part at a time.
fn places_by_file<S: Statements>(
	state: &State<S>,
	origins: &[EntityId],
	(devices_needed, part): (&BTreeSet<EntityId>, usize),
) -> Result<BTreeMap<EntityId, Vec<FoundAt>>, NodeError>
where
	NodeError: From<S::Error>,
{
	// Devices merged into others are those others, as for a single file; devices are few.
	let mut canonical_devices: BTreeMap<EntityId, EntityId> = BTreeMap::new();
	let asked = [
		fields::TYPE,
		fields::DELETED,
		fields::MERGED_INTO,
		ORIGIN_OF,
		FOUND_ON,
		PATH,
		MODIFIED,
		CONTENT,
		SIZE,
	];
	let mut seen = BTreeSet::new();
	let mut places: BTreeMap<EntityId, Vec<FoundAt>> = BTreeMap::new();
	by_parts(state, origins, (&asked, part), &mut |part, origins| {
		for &origin in origins {
			let origin = part.canonical(origin)?;
			if !seen.insert(origin)
				|| part.is_deleted(origin)?
				|| !part.types(origin)?.contains(&ORIGIN)
			{
				continue;
			}
			let values = |field: &FieldName| -> Result<Vec<Value>, NodeError> {
				Ok(part
					.current(origin, field)?
					.iter()
					.map(|statement| statement.value.clone())
					.collect())
			};
			let modified = values(&MODIFIED)?.into_iter().next();
			let content = values(&CONTENT)?.into_iter().find_map(|value| match value {
				Value::Blob(content) => Some(content),
				_ => None,
			});
			let size = values(&SIZE)?.into_iter().find_map(|value| match value {
				Value::Integer(size) => u64::try_from(size).ok(),
				_ => None,
			});
			let mut devices = Vec::new();
			for device in values(&FOUND_ON)?.iter().filter_map(Value::as_reference) {
				let device = if let Some(canonical) = canonical_devices.get(&device) {
					*canonical
				} else {
					let canonical = canonical_device(state, device)?;
					canonical_devices.insert(device, canonical);
					canonical
				};
				devices.push(device);
			}
			let paths: Vec<String> = values(&PATH)?
				.iter()
				.filter_map(|path| path.as_text().map(str::to_owned))
				.collect();
			for file in values(&ORIGIN_OF)?.iter().filter_map(Value::as_reference) {
				let found = places.entry(file).or_default();
				for device in &devices {
					// A place on a device that neither is this one nor has a folder rule
					// counts only as somewhere else: by its device alone.
					if !devices_needed.contains(device) {
						found.push(FoundAt {
							device: *device,
							path: String::new(),
							modified: None,
							content: None,
							size: None,
						});
						continue;
					}
					for path in &paths {
						found.push(FoundAt {
							device: *device,
							path: path.clone(),
							modified: modified.clone(),
							content,
							size,
						});
					}
				}
			}
		}
		Ok(())
	})?;
	Ok(places)
}

/// The canonical entity of a device of a state.
fn canonical_device<S: Statements>(
	state: &State<S>,
	device: EntityId,
) -> Result<EntityId, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state.canonical(device)?)
}

/// What rules look at in a file of a part of a state, but the places it was found.
fn facts_of_part(part: &State, file: EntityId) -> Result<Facts, NodeError> {
	facts_but_places(part, file)
}

/// Changes to a policy: each value given replaces what the policy has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyEdit {
	/// Its name.
	pub name: Option<String>,
	/// How many copies, at least 1.
	pub copies: Option<u32>,
	/// How many of them outside the home.
	pub away: Option<u32>,
	/// How many of them on archive nodes.
	pub archived: Option<u32>,
	/// Where devices keep them.
	pub kept: Option<Kept>,
}

/// The policies an entity of a state chooses itself.
pub(crate) fn own_choices<S: Statements>(
	state: &State<S>,
	entity: EntityId,
) -> Result<Vec<EntityId>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut own = Vec::new();
	for statement in state.current(entity, &POLICY)? {
		if let Some(policy) = statement.value.as_reference() {
			own.push(state.canonical(policy)?);
		}
	}
	Ok(own)
}

#[cfg(test)]
mod tests {
	use nibkeep_model::storage::Memory;
	use nibkeep_std::{CAMERA_MODEL, PHOTO};
	use nibkeep_testkit::TestRng;
	use nibkeep_time::Timestamp;

	use super::*;

	#[test]
	fn the_strictest_of_competing_values_counts() {
		let mut rng = TestRng::new("strictest");
		let policy = rng.entity_id();
		let text = |text: &str| Value::Text(text.to_owned());
		let operations = [
			assert(
				policy,
				fields::TYPE,
				text(STORAGE_POLICY.as_str()),
				&mut rng,
			),
			// Two devices set each field at once, neither seeing the other.
			assert(policy, POLICY_COPIES, Value::Integer(2), &mut rng),
			assert(policy, POLICY_COPIES, Value::Integer(3), &mut rng),
			assert(policy, POLICY_ARCHIVED, Value::Integer(1), &mut rng),
			assert(policy, POLICY_KEPT, text("in-place"), &mut rng),
			assert(policy, POLICY_KEPT, text("store"), &mut rng),
		];
		let mut state = State::in_storage(Memory::default());
		for (at, operation) in (1_000..).zip(&operations) {
			let Ok(()) = state.apply(operation, Timestamp::new(at, 0));
		}
		let policies = Policies::read(&state, [policy], []).expect("reads");
		assert_eq!(
			policies.policies[&policy].needs,
			Needs {
				copies: 3,
				away: 0,
				archived: 1,
				kept: Kept::Store,
			}
		);
	}

	#[test]
	fn every_condition_must_hold() {
		let mut rng = TestRng::new("conditions");
		let (device, other) = (rng.entity_id(), rng.entity_id());
		let facts = Facts {
			types: [FILE].into_iter().collect(),
			fields: [CAMERA_MAKE].into_iter().collect(),
			media_type: Some("video/mp4".to_owned()),
			size: Some(100),
			places: vec![(device, "/home/ada/Downloads/film.mp4".to_owned())],
		};
		let folder = |path: &str, device: EntityId| Conditions {
			folder: Some(Folder {
				path: path.to_owned(),
				device,
			}),
			..Conditions::default()
		};
		assert!(Conditions::default().hold(&facts), "no conditions");
		assert!(folder("/home/ada/Downloads", device).hold(&facts));
		assert!(folder("/home/ada", device).hold(&facts), "below the folder");
		assert!(
			!folder("/home/ada/Down", device).hold(&facts),
			"a folder is named whole"
		);
		assert!(
			!folder("/home/ada/Downloads", other).hold(&facts),
			"on another device"
		);
		let mut elsewhere = facts.clone();
		elsewhere
			.places
			.push((device, "/home/ada/Films/film.mp4".to_owned()));
		assert!(
			!folder("/home/ada/Downloads", device).hold(&elsewhere),
			"found elsewhere too"
		);
		let media = |prefix: &str| Conditions {
			media_type: Some(prefix.to_owned()),
			..Conditions::default()
		};
		assert!(media("video/").hold(&facts));
		assert!(media("video/mp4").hold(&facts));
		assert!(!media("image/").hold(&facts));
		let larger = |bytes| Conditions {
			larger_than: Some(bytes),
			..Conditions::default()
		};
		assert!(larger(99).hold(&facts));
		assert!(!larger(100).hold(&facts));
		let of_type = |name| Conditions {
			of_type: Some(name),
			..Conditions::default()
		};
		assert!(of_type(FILE).hold(&facts));
		assert!(!of_type(PHOTO).hold(&facts));
		let has = |field| Conditions {
			has: Some(field),
			..Conditions::default()
		};
		assert!(has(CAMERA_MAKE).hold(&facts));
		assert!(!has(CAMERA_MODEL).hold(&facts));
		// All of them at once.
		let all = Conditions {
			of_type: Some(FILE),
			has: Some(CAMERA_MAKE),
			..media("video/")
		};
		assert!(all.hold(&facts));
		assert!(
			!Conditions {
				larger_than: Some(100),
				..all
			}
			.hold(&facts)
		);
	}

	#[test]
	fn a_rule_with_a_condition_it_cannot_read_chooses_nothing() {
		let mut rng = TestRng::new("unreadable");
		let (policy, rule) = (rng.entity_id(), rng.entity_id());
		let text = |text: &str| Value::Text(text.to_owned());
		let operations = [
			assert(
				policy,
				fields::TYPE,
				text(STORAGE_POLICY.as_str()),
				&mut rng,
			),
			assert(rule, fields::TYPE, text(STORAGE_RULE.as_str()), &mut rng),
			assert(rule, RULE_POLICY, Value::Reference(policy), &mut rng),
			assert(rule, RULE_TYPE, text("not a type name"), &mut rng),
		];
		let mut state = State::in_storage(Memory::default());
		for (at, operation) in (1_000..).zip(&operations) {
			let Ok(()) = state.apply(operation, Timestamp::new(at, 0));
		}
		let policies = Policies::read(&state, [policy], [rule]).expect("reads");
		assert_eq!(policies.rules[0].policy, None);
		let choice = policies.choose(&[], &Facts::default());
		assert_eq!(choice.by, ChosenBy::Fallback);
	}

	/// A library in a directory of its own, protected by a quick passphrase, and the
	/// directory.
	fn library(name: &str, rng: &mut TestRng) -> (std::path::PathBuf, Library) {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-policies-{name}-{}", std::process::id()),
		);
		let protection = nibkeep_keys::NewProtection::Passphrase(
			"correct horse",
			nibkeep_crypto::PassphraseParams::new(64, 1, 1).expect("valid"),
		);
		let library = Library::create(
			&directory,
			&crate::NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			NOW,
			rng,
		)
		.expect("creates");
		(directory, library)
	}

	const NOW: u64 = 1_790_000_000_000;

	/// Sets a rule's position key, as another device could.
	fn set_position(library: &mut Library, rule: EntityId, key: &str, rng: &mut TestRng) {
		let space = library.personal_space();
		let operations = library
			.replacing(
				space,
				rule,
				vec![(fields::POSITION, Value::Text(key.to_owned()))],
				rng,
			)
			.expect("reads");
		library
			.commit(space, operations, NOW, rng)
			.expect("commits");
	}

	#[test]
	fn a_rule_goes_where_it_is_put_when_keys_are_shared_or_bad() {
		let mut rng = TestRng::new("placing");
		let (directory, mut library) = library("placing", &mut rng);
		let space = library.personal_space();
		library
			.install_policies(space, None, NOW, &mut rng)
			.expect("installs");
		let rules = library.rules(space).expect("reads");
		let (camera, rest) = (rules[0].id, rules[1].id);
		let standard = derived(space, STANDARD);
		// Two rules with one key, as two devices that add rules at once can leave them.
		let key = rules[0].position.clone().expect("a key");
		set_position(&mut library, rest, &key, &mut rng);
		let order = |library: &Library| -> Vec<EntityId> {
			library
				.rules(space)
				.expect("reads")
				.iter()
				.map(|rule| rule.id)
				.collect()
		};
		let first_two = order(&library);
		let new = library
			.add_rule(
				space,
				&NewRule::new(standard),
				Place::After(first_two[0]),
				NOW,
				&mut rng,
			)
			.expect("adds");
		assert_eq!(order(&library), [first_two[0], new, first_two[1]]);
		// A rule with a key that is not one sorts last, and a rule put last goes after it.
		set_position(&mut library, camera, "not a key!", &mut rng);
		assert_eq!(order(&library).last(), Some(&camera));
		let last = library
			.add_rule(space, &NewRule::new(standard), Place::Last, NOW, &mut rng)
			.expect("adds");
		assert_eq!(order(&library).last(), Some(&last));
		assert_eq!(order(&library).len(), 4);
		drop(library);
		let _ = std::fs::remove_dir_all(directory);
	}

	#[test]
	fn the_position_written_last_counts() {
		// In both orders, so that the statements' IDs cannot decide.
		for (earlier, later) in [("a", "b"), ("b", "a")] {
			let mut rng = TestRng::new(&format!("last position {earlier}"));
			let (rule, policy) = (rng.entity_id(), rng.entity_id());
			let text = |text: &str| Value::Text(text.to_owned());
			let operations = [
				assert(
					policy,
					fields::TYPE,
					text(STORAGE_POLICY.as_str()),
					&mut rng,
				),
				assert(rule, fields::TYPE, text(STORAGE_RULE.as_str()), &mut rng),
				assert(rule, RULE_POLICY, Value::Reference(policy), &mut rng),
				// Two devices move the rule at once; the later move counts.
				assert(rule, fields::POSITION, text(earlier), &mut rng),
				assert(rule, fields::POSITION, text(later), &mut rng),
			];
			let mut state = State::in_storage(Memory::default());
			for (at, operation) in (1_000..).zip(&operations) {
				let Ok(()) = state.apply(operation, Timestamp::new(at, 0));
			}
			let policies = Policies::read(&state, [policy], [rule]).expect("reads");
			assert_eq!(policies.rules[0].position.as_deref(), Some(later));
		}
	}

	#[test]
	fn policies_checked_in_parts_are_checked_as_at_once() {
		let mut rng = TestRng::new("checked in parts");
		let (directory, mut library) = library("checked-in-parts", &mut rng);
		let space = library.personal_space();
		let downloads = directory.join("downloads");
		let elsewhere = directory.join("elsewhere");
		std::fs::create_dir_all(&downloads).expect("makes a folder");
		std::fs::create_dir_all(&elsewhere).expect("makes a folder");
		library
			.install_policies(space, Some(&downloads), NOW, &mut rng)
			.expect("installs");
		let mut added = Vec::new();
		for n in 0..12 {
			let folder = if n % 2 == 0 { &downloads } else { &elsewhere };
			let path = folder.join(format!("file {n}"));
			std::fs::write(&path, format!("the content of file {n}")).expect("writes");
			added.push((
				path.clone(),
				library.add_file(space, &path, NOW, &mut rng).expect("adds"),
			));
		}
		// One kept where it is changes, two are merged, and one has a policy of its own.
		std::fs::write(&added[0].0, b"changed by another app").expect("writes");
		library
			.merge_entities(space, added[2].1.file, added[3].1.file, NOW, &mut rng)
			.expect("merges");
		let critical = derived(space, CRITICAL);
		library
			.set_policy(space, added[4].1.file, Some(critical), NOW, &mut rng)
			.expect("sets");
		let in_parts = library
			.check_policies_in_parts(space, (2, false), NOW)
			.expect("checks");
		let at_once = library
			.check_policies_in_parts(space, (100_000, false), NOW)
			.expect("checks");
		assert_eq!(in_parts, at_once);
		// The same as each file's policy and copies, found one file at a time.
		let open = library.space(space).expect("a space");
		let state = open.stored.log().state();
		let policies = library.read_policies(space).expect("reads");
		let mut counts = BTreeMap::new();
		let files: BTreeSet<EntityId> = added
			.iter()
			.map(|(_, added)| state.canonical(added.file).expect("reads"))
			.collect();
		for file in files {
			let choice = library.policy_of(space, file).expect("chooses");
			let content = state
				.current(file, &CONTENT)
				.expect("reads")
				.iter()
				.find_map(|statement| match statement.value {
					Value::Blob(hash) => Some(hash),
					_ => None,
				});
			let stored = content.is_some_and(|content| open.stored.holds_ordinary_blob(&content));
			let mut in_place = false;
			for (origin, device, path) in origins_of(state, file).expect("reads") {
				let first = |field: &FieldName| {
					state
						.current(origin, field)
						.expect("reads")
						.first()
						.map(|statement| statement.value.clone())
				};
				let place = FoundAt {
					device,
					path,
					modified: first(&MODIFIED),
					content: match first(&CONTENT) {
						Some(Value::Blob(content)) => Some(content),
						_ => None,
					},
					size: match first(&SIZE) {
						Some(Value::Integer(size)) => u64::try_from(size).ok(),
						_ => None,
					},
				};
				in_place |= device == library.info.device && place.holds(content);
			}
			// One device, unmarked: what it holds is on its one disk.
			let disks = (usize::from(stored || in_place), 0, 0, false);
			tally(
				&mut counts,
				&policies,
				(&choice, file, None),
				((stored, in_place), disks),
			);
		}
		assert_eq!(in_parts.policies, checked(counts, Vec::new()).policies);
		let files: usize = at_once.policies.iter().map(|count| count.files).sum();
		assert_eq!(files, 11, "twelve files, two of them merged");
		assert!(
			at_once
				.policies
				.iter()
				.any(|count| count.none_here.contains(&added[0].1.file)),
			"the changed one has no copy here"
		);
		drop(library);
		let _ = std::fs::remove_dir_all(directory);
	}
}
