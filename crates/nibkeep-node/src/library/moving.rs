//! Moving an entity to another space of the user's: sharing means moving
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)), and giving something
//! to someone is moving it to their space
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves)). A move writes,
//! in the other space, a new entity for the entity and for each folder, list, membership and
//! member that it holds, through every level, each once: their values, and, unless the
//! person chooses otherwise, their history, as the mover's account of it; their secret
//! values, sealed with the other space's secrets key, unless the person leaves them behind;
//! the definitions of their types that the other space lacks, in changes of their own; and
//! their files' content, whose sealed copies the replica's spaces share. Each old entity is
//! marked as moved into its new one, and keeps its history where it was too.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_log::list::MEMBERSHIP;
use nibkeep_model::storage::Statements as _;
use nibkeep_model::{FieldName, Operation, Statement, TypeName, Value, fields};
use nibkeep_std::{
	ATTACHED_TO, CHECKED_OUT_ON, CHECKOUT_OF, CONTENT, FILE, FOUND_ON, IN_FOLDER, IN_TREE,
	ORIGIN_OF, THUMBNAIL, THUMBNAIL_OF,
};
use nibkeep_time::Timestamp;

use super::export::OWN_RECORDS;
use super::links::Links;
use super::{Library, held_copies_in};
use crate::NodeError;

/// How many moves on a lookup follows, from one space to another.
const MOST_MOVES: usize = 16;

/// Whether a move brings the history of what it moves ([technical design: spaces and moves]).
///
/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveHistory {
	/// Its past values too, each replaced as it was, with the mover's note of when the old
	/// space recorded it, and by whom.
	Bring,
	/// Its current values alone, as they are, which leaves its history in the old space.
	Leave,
}

/// Whether a move purges what it leaves behind in the old space ([technical design: spaces and moves]).
///
/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
	/// Purges it, once the new entity counts and another device holds it: what the mover's
	/// role lets them purge, and a mark asks the old space's admins for the rest.
	Purge,
	/// Keeps it, as it was.
	Keep,
}

/// Whether a move takes the secret values of what it moves ([technical design: sharing]): the
/// values of fields secret in the old space.
///
/// [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveSecrets {
	/// Takes them, current and past alike, sealed with the other space's secrets key, which
	/// needs the secrets of both spaces unlocked; a field secret in the old space becomes
	/// secret in the new one.
	Bring,
	/// Leaves them with the old entity, which keeps them, as no purge of what a move leaves
	/// behind takes a secret value that did not move.
	Leave,
}

/// What a move would take, for an app to show before it moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovePreview {
	/// The entities that move: the one given first, then the folders, lists, memberships and
	/// members that it holds, through every level, each once.
	pub entities: Vec<EntityId>,
	/// The folders and lists of the old space that stay, and hold some of them: they find
	/// what moved through the marks that lead to it. Ordered by ID.
	pub staying: Vec<EntityId>,
	/// The members of the old space who are no members of the new one, by name, sorted:
	/// those who no longer see what moves once what stays behind is purged.
	pub losing: Vec<String>,
	/// The members of the new space who are no members of the old one, by name, sorted:
	/// those who see what moves for the first time, its history too where the move brings it.
	pub gaining: Vec<String>,
	/// Whether the mover may purge all that stays behind: in the personal space, or as an
	/// admin of the old space; an editor purges only the changes they signed there, and those
	/// who lose what moves lose it only once an admin purges the rest.
	pub purges_all: bool,
	/// Whether the move needs the secrets unlocked, as far as this replica can tell: what
	/// moves holds secret values, values of fields secret in the old space, which move only
	/// with the secrets of both spaces unlocked, or stay behind; or values of fields secret in
	/// the new space, which are sealed there. While the old space's secrets are locked, an
	/// entity whose types list a secret field may hold secret values.
	pub secrets: bool,
}

/// A statement that a move carries, with what the old space recorded of it.
struct Carried {
	statement: Statement,
	/// Whether it is a secret value in the old space: sealed there, or of a field secret
	/// there now.
	secret: bool,
	/// When the old space recorded it, if the move brings history.
	recorded: Option<Timestamp>,
	/// Who recorded it there, as far as the old space says.
	author: Option<VerifyingKey>,
	/// When the old space recorded that it was replaced: it is carried replaced.
	replaced: Option<Timestamp>,
}

/// What a move takes: each entity with the statements it carries, the one given first; the
/// content of the files that they hold now or held before, each with the fields that hold
/// it, and whether one holds it now; their types; and the fields of their secret values.
struct Taken {
	entities: Vec<(EntityId, Vec<Carried>)>,
	/// What the move writes anew in the other space and leaves where it is too, unmarked:
	/// thumbnails that files staying behind still show.
	copies: Vec<(EntityId, Vec<Carried>)>,
	contents: BTreeMap<Hash, (BTreeSet<FieldName>, bool)>,
	kinds: BTreeSet<TypeName>,
	secret: BTreeSet<FieldName>,
}

impl Taken {
	/// Takes a copy of an entity with the statements it carries, and the content they hold,
	/// leaving the entity where it is.
	fn copy(&mut self, entity: EntityId, carried: Vec<Carried>) {
		self.add(entity, carried);
		if let Some(copied) = self.entities.pop() {
			self.copies.push(copied);
		}
	}

	/// Takes an entity with the statements it carries, and the content they hold. What a
	/// thumbnail was made of, it names without holding it, so it is no content to take.
	fn add(&mut self, entity: EntityId, carried: Vec<Carried>) {
		for statement in &carried {
			if statement.secret {
				self.secret.insert(statement.statement.field.clone());
			}
			if let Value::Blob(content) = statement.statement.value
				&& !nibkeep_std::names_content_only(&statement.statement.field)
			{
				let (holding, now) = self.contents.entry(content).or_default();
				holding.insert(statement.statement.field.clone());
				*now |= statement.replaced.is_none();
			}
		}
		self.entities.push((entity, carried));
	}
}

impl Library {
	/// Moves an entity of `from` to the space `to`, with the folders, lists, memberships and
	/// members that it holds, through every level, each once, as [`Library::move_preview`]
	/// lists them: marks each in `from` as moved into a new one, and writes the new ones in
	/// `to`, with their values, references among them leading to the new ones; with
	/// [`MoveHistory::Bring`], their past values too, replaced, each value with the mover's
	/// note of where it came from, when that space recorded it and by whom, and when it was
	/// replaced there; with [`MoveSecrets::Bring`], their secret values, sealed with the
	/// secrets key of `to`, which this replica sets up there first if it may, and the fields
	/// secret in `from` made secret for good in `to` where its types leave them plain; the
	/// definitions of their types that `to` lacks; and their files' content, whose keys are
	/// wrapped for `to`. A folder that stays no longer holds what moved, which it finds
	/// through its mark. Returns the entity the given one is now. The old entities keep their
	/// history in `from`, and links to them lead to the new ones for those who hold `to`. The
	/// marks come first: a mark whose entity never came is passed over, so a move cut short
	/// leaves the entities where they were.
	///
	/// # Errors
	///
	/// Returns an error if the spaces are one, this library lacks either, this replica is an
	/// AI agent's or revoked there, the entity is not in `from`, was moved already, or what
	/// would move holds one of the library's own records, a definition or a tree, or is in a
	/// tree; with [`MoveSecrets::Bring`], [`NodeError::MoveNeedsSecrets`] if any of it may
	/// hold secret values while the secrets of `from` are locked, or those of `to`, the
	/// personal space, are; [`NodeError::NotSecretThere`] if a field of its secret values is
	/// secret in `from` and would be plain in `to`, whose admins alone make it secret; the
	/// errors of setting up the secrets of `to`, if this replica holds none there, such as
	/// [`NodeError::AdminsOnly`] or [`NodeError::SpaceSecretsElsewhere`];
	/// [`NodeError::NotHere`] if a file's content is not on this device; or an error if a
	/// change cannot be saved. Nothing moves then.
	#[expect(
		clippy::too_many_arguments,
		reason = "one move, with the choices the person makes"
	)]
	pub fn move_entity(
		&mut self,
		from: SpaceId,
		entity: EntityId,
		to: SpaceId,
		history: MoveHistory,
		behind: Behind,
		secrets: MoveSecrets,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<EntityId, NodeError> {
		self.check_move(from, to)?;
		self.check_not_group(to)?;
		// What other programs wrote, so that nothing moved since, or edited, is missed.
		self.catch_up()?;
		let taken = self.to_move((from, to), entity, (history, secrets))?;
		// What would fail later, before anything is written; and where its secret values go.
		self.check_contents_here((from, to), &taken)?;
		self.prepare_secrets((from, to), &taken, wall_millis, rng)?;
		// The types they have, as this space defines them, with what the other lacks of them,
		// as careful as the other's types are, so that a field secret there stays so.
		let source_types = self.types(from)?;
		for kind in &taken.kinds {
			if let Some(definition) = source_types.get(kind) {
				let careful = self.space(to)?.types.careful(definition);
				self.define_type(to, &careful, wall_millis, rng)?;
			}
		}
		// What is secret here stays secret there.
		self.keep_secret(to, &taken.secret, wall_millis, rng)?;
		// Their files' content, whose sealed copies the two spaces share, each of the kind,
		// secret or ordinary, of the fields that hold it there; and what this replica holds of
		// the content they had before, which their history names.
		let secret_there = self.space(to)?.stored.secret_fields().clone();
		for (content, (holding, now)) in &taken.contents {
			if !now && !self.space(from)?.stored.holds_blob(content) {
				continue;
			}
			let kinds: BTreeSet<bool> = holding
				.iter()
				.map(|field| secret_there.contains(field))
				.collect();
			for secret in kinds {
				self.copy_blob(from, content, (to, secret), wall_millis, rng)?;
			}
		}
		let new: BTreeMap<EntityId, EntityId> = taken
			.entities
			.iter()
			.chain(&taken.copies)
			.map(|(old, _)| (*old, EntityId::random(rng)))
			.collect();
		let mut marks = Vec::new();
		for (old, _) in &taken.entities {
			marks.push(Operation::assert(Statement::new(
				StatementId::random(rng),
				*old,
				fields::MOVED_INTO,
				Value::Reference(new[old]),
			)));
			// What stays is purged later, once another device holds the new entity; a move
			// that keeps it takes back what a move cut short asked.
			if behind == Behind::Purge {
				marks.push(Operation::assert(Statement::new(
					StatementId::random(rng),
					*old,
					fields::PURGE_ASKED,
					Value::Boolean(true),
				)));
			} else {
				let asked: Vec<StatementId> = self
					.space(from)?
					.stored
					.log()
					.state()
					.current(*old, &fields::PURGE_ASKED)?
					.iter()
					.map(|statement| statement.id)
					.collect();
				if !asked.is_empty() {
					marks.push(Operation::retract(*old, asked));
				}
			}
		}
		self.commit(from, marks, wall_millis, rng)?;
		let mut operations = Vec::new();
		for (old, carried) in taken.entities.iter().chain(&taken.copies) {
			operations.extend(carried_operations(
				carried,
				(new[old], &new),
				(from, history),
				rng,
			));
		}
		self.commit(to, operations, wall_millis, rng)?;
		let given = taken.entities.first().map_or(entity, |(old, _)| *old);
		Ok(new[&given])
	}

	/// What moving an entity of `from` to `to` would take: the entity, and the folders,
	/// lists, memberships and members that it holds, through every level, each once, but
	/// those deleted or moved already; and the folders and lists of `from` that stay and hold
	/// some of them, which no longer hold them in `to` ([technical design: spaces and moves]). An app shows
	/// this before a move takes more than the entity, or takes it out of a folder or a list.
	///
	/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
	///
	/// # Errors
	///
	/// As [`Library::move_entity`], for what a move refuses.
	pub fn move_preview(
		&self,
		from: SpaceId,
		entity: EntityId,
		to: SpaceId,
	) -> Result<MovePreview, NodeError> {
		self.check_move(from, to)?;
		self.space(from)?.stored.reading(|_| {
			let entities = self.held_with(from, entity)?;
			for &each in &entities {
				self.check_movable(from, each)?;
			}
			let staying = self.staying_holders(from, &entities)?;
			let (losing, purges_all) = self.losing_and_purging((from, to))?;
			let (gaining, _) = self.losing_and_purging((to, from))?;
			// Secret values here, or values that would be sealed there.
			let state = self.space(from)?.stored.log().state();
			let mut kinds = BTreeSet::new();
			for &each in &entities {
				kinds.extend(state.types(each)?);
			}
			let secret_there = self.secret_once_moved((from, to), &kinds)?;
			let mut secrets = false;
			for &each in &entities {
				secrets |= self.may_hold_secrets(from, each)?;
				for member in state.merged(each)? {
					secrets |= state
						.storage()
						.of_entity(member)?
						.iter()
						.any(|(field, _)| secret_there.contains(field));
				}
			}
			Ok(MovePreview {
				entities,
				staying,
				losing,
				gaining,
				purges_all,
				secrets,
			})
		})
	}

	/// Refuses a move from `from` to `to` that no entity may make: within one space, to a
	/// space this library lacks, by an AI agent's replica, or by one revoked in either.
	fn check_move(&self, from: SpaceId, to: SpaceId) -> Result<(), NodeError> {
		if from == to {
			return Err(NodeError::SameSpace);
		}
		self.space(from)?;
		self.space(to)?;
		self.check_not_ai_agent("move entities")?;
		self.check_not_revoked(from)?;
		self.check_not_revoked(to)
	}

	/// The entity that an entity of a space was moved into, and its space, following moves
	/// onward, if this replica holds the space of where it is now: of several, as two moves
	/// at the same time make, the one with the smallest ID. A mark whose entity this replica
	/// holds nowhere is passed over.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or a space cannot be read.
	pub fn moved_into(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Option<(SpaceId, EntityId)>, NodeError> {
		let mut at = (space, entity);
		let mut found = None;
		for _ in 0..MOST_MOVES {
			let Some(next) = self.moved_once(at.0, at.1)? else {
				break;
			};
			if next == (space, entity) || found == Some(next) {
				break;
			}
			found = Some(next);
			at = next;
		}
		Ok(found)
	}

	/// The entities of the replica's other spaces that moved into an entity of `space`, or
	/// into one merged into it there, each with its space, following moves back, as far as
	/// [`Library::moved_into`] leads them to it.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or a space cannot be read.
	pub fn moved_from(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Vec<(SpaceId, EntityId)>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let canonical = state.canonical(entity)?;
		let mut found: Vec<(SpaceId, EntityId)> = Vec::new();
		let mut reached: Vec<(SpaceId, EntityId)> = state
			.merged(canonical)?
			.into_iter()
			.map(|member| (space, member))
			.collect();
		for _ in 0..MOST_MOVES {
			let mut next = Vec::new();
			for &(into, moved) in &reached {
				for held in self.spaces() {
					if held == into {
						continue;
					}
					let marked = self
						.space(held)?
						.stored
						.find(&fields::MOVED_INTO, &Value::Reference(moved))?;
					for old in marked {
						if held != space && !found.contains(&(held, old)) {
							found.push((held, old));
							next.push((held, old));
						}
					}
				}
			}
			if next.is_empty() {
				break;
			}
			reached = next;
		}
		let mut leading = Vec::new();
		for (held, old) in found {
			if let Some((there, now)) = self.moved_into(held, old)?
				&& there == space
				&& state.canonical(now)? == canonical
			{
				leading.push((held, old));
			}
		}
		Ok(leading)
	}

	/// The entities that link to an entity of a space on `day`, as [`Library::links_to`]
	/// gives them, together with those of the replica's other spaces that link to the
	/// entities that moved into it, as a replica that holds both spaces shows them
	/// ([technical design: spaces and moves]): but those whose new entities link to it here, as what moved
	/// along does. Each field's count counts them all, and its entities are the first
	/// `limit` by name.
	///
	/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or a space cannot be read.
	pub fn links_to_anywhere(
		&self,
		space: SpaceId,
		entity: EntityId,
		day: nibkeep_time::Day,
		limit: usize,
	) -> Result<Vec<Links>, NodeError> {
		let moved_from = self.moved_from(space, entity)?;
		if moved_from.is_empty() {
			return self.links_to(space, entity, day, limit);
		}
		let mut links = self.links_to(space, entity, day, usize::MAX)?;
		let here: BTreeSet<EntityId> = links
			.iter()
			.flat_map(|links| links.entities.iter().map(|linking| linking.id))
			.collect();
		for (held, old) in moved_from {
			for mut other in self.links_to(held, old, day, usize::MAX)? {
				let mut kept = Vec::new();
				for linking in other.entities {
					let linked_here = self
						.moved_into(held, linking.id)?
						.is_some_and(|(there, now)| there == space && here.contains(&now));
					if !linked_here {
						kept.push(linking);
					}
				}
				if kept.is_empty() {
					continue;
				}
				other.count = kept.len();
				other.entities = kept;
				match links.iter_mut().find(|links| links.field == other.field) {
					Some(same) => {
						same.count += other.count;
						same.entities.extend(other.entities);
					}
					None => links.push(other),
				}
			}
		}
		for group in &mut links {
			group.entities.sort_by_key(|linking| {
				(
					linking.name.is_none(),
					linking.name.as_deref().map(str::to_lowercase),
					linking.id,
				)
			});
			group.entities.truncate(limit);
		}
		Ok(links)
	}

	/// Where one move of an entity led, as [`Library::moved_into`] says, without following
	/// further moves.
	fn moved_once(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Option<(SpaceId, EntityId)>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let mut targets: Vec<EntityId> = state
			.current(entity, &fields::MOVED_INTO)?
			.iter()
			.filter_map(|statement| statement.value.as_reference())
			.collect();
		targets.sort();
		for target in targets {
			for held in self.spaces() {
				if held == space {
					continue;
				}
				let there = self.space(held)?.stored.log().state();
				if !there.fields(target)?.is_empty() {
					return Ok(Some((held, target)));
				}
			}
		}
		Ok(None)
	}

	/// An entity's name, in the space it was moved into, if it was, or else in whichever
	/// space holds it.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn name_anywhere(&self, entity: EntityId) -> Result<Option<String>, NodeError> {
		let Some(space) = self.space_of(entity)? else {
			return Ok(None);
		};
		match self.moved_into(space, entity)? {
			Some((there, moved)) => self.name(there, moved),
			None => self.name(space, entity),
		}
	}

	/// What a move takes of an entity of `from` to `to`, as [`Library::move_entity`] says,
	/// and what it refuses.
	fn to_move(
		&self,
		(from, to): (SpaceId, SpaceId),
		entity: EntityId,
		(history, secrets): (MoveHistory, MoveSecrets),
	) -> Result<Taken, NodeError> {
		self.space(from)?.stored.reading(|_| {
			let held = self.held_with(from, entity)?;
			let moving: BTreeSet<EntityId> = held.iter().copied().collect();
			let mut taken = Taken {
				entities: Vec::new(),
				copies: Vec::new(),
				contents: BTreeMap::new(),
				kinds: BTreeSet::new(),
				secret: BTreeSet::new(),
			};
			for each in held {
				taken.kinds.extend(self.check_movable(from, each)?);
				// Which values are secret is known only with the secrets unlocked.
				if secrets == MoveSecrets::Bring
					&& !self.secrets_unlocked(from)
					&& self.may_hold_secrets(from, each)?
				{
					return Err(NodeError::MoveNeedsSecrets);
				}
				let carried = self.carried_of((from, to), each, (history, secrets), &moving)?;
				taken.add(each, carried);
			}
			// The thumbnails of the content that moves go with it, as each belongs to a
			// content (technical design: thumbnails); one that a file staying behind still
			// shows is written there anew, and stays.
			for (thumbnail, copied) in self.thumbnails_moving((from, &moving), &taken)? {
				taken.kinds.extend(self.check_movable(from, thumbnail)?);
				let carried =
					self.carried_of((from, to), thumbnail, (history, secrets), &moving)?;
				if copied {
					taken.copy(thumbnail, carried);
				} else {
					taken.add(thumbnail, carried);
				}
			}
			// Where a file's plain copies are, its origins and checkouts, go with it where
			// their device is one of the user's own that holds the other space, so that the
			// other space knows of them; the others are purged with what stays behind. Only
			// those of a content that moves anyway, so that none brings a version that the
			// move leaves behind.
			for record in self.plain_copies_moving((from, to), &moving, &taken)? {
				taken.kinds.extend(self.check_movable(from, record)?);
				let carried = self.carried_of((from, to), record, (history, secrets), &moving)?;
				taken.add(record, carried);
			}
			Ok(taken)
		})
	}

	/// The origins and checkouts of the files of `from` that a move to `to` takes, those
	/// merged into them included, that stand there, whose device is one of the user's own
	/// that holds `to`, by the choices of a replica on it that is not revoked, and whose
	/// content, if they recorded one, the move `taken` takes anyway, ordered by ID: where
	/// those files' plain copies are, which the other space then knows of, and which their
	/// device keeps checking there. Those of other devices, as other members', which this
	/// replica cannot tell, or the user's that do not hold `to`, stay, and are purged with what
	/// the move leaves behind; so are those of a version that the move leaves.
	fn plain_copies_moving(
		&self,
		(from, to): (SpaceId, SpaceId),
		moving: &BTreeSet<EntityId>,
		taken: &Taken,
	) -> Result<Vec<EntityId>, NodeError> {
		let personal = self.space(self.info.personal)?.stored.log().state();
		let devices = self.holding_devices(to)?;
		let open = self.space(from)?;
		let state = open.stored.log().state();
		let mut files = BTreeSet::new();
		for &each in moving {
			files.extend(state.merged(each)?);
		}
		let mut moving_along = Vec::new();
		for (record, moved, deleted) in self.plain_copies_of(from, &files)? {
			if moved || deleted {
				continue;
			}
			let device = state
				.current(record, &FOUND_ON)?
				.into_iter()
				.chain(state.current(record, &CHECKED_OUT_ON)?)
				.find_map(|statement| statement.value.as_reference());
			let Some(device) = device else {
				continue;
			};
			let content = state
				.current(record, &CONTENT)?
				.into_iter()
				.find_map(|statement| match statement.value {
					Value::Blob(content) => Some(content),
					_ => None,
				});
			if devices.contains(&personal.canonical(device)?)
				&& content.is_none_or(|content| taken.contents.contains_key(&content))
			{
				moving_along.push(record);
			}
		}
		Ok(moving_along)
	}

	/// The origins and checkouts of a space that name one of `entities`, as given, each once
	/// as the entity it shows as, with whether it moved and whether it is deleted, ordered by
	/// ID.
	pub(crate) fn plain_copies_of(
		&self,
		space: SpaceId,
		entities: &BTreeSet<EntityId>,
	) -> Result<Vec<(EntityId, bool, bool)>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut found = BTreeMap::new();
		for &entity in entities {
			for field in [&ORIGIN_OF, &CHECKOUT_OF] {
				for statement in state.referrers_in(entity, field)? {
					let record = state.canonical(statement.entity)?;
					if entities.contains(&record) {
						continue;
					}
					let moved = !state.current(record, &fields::MOVED_INTO)?.is_empty();
					found.insert(record, (moved, state.is_deleted(record)?));
				}
			}
		}
		Ok(found
			.into_iter()
			.map(|(record, (moved, deleted))| (record, moved, deleted))
			.collect())
	}

	/// The thumbnails of `from` made of the content that a move takes as a file's ordinary
	/// content, that are neither deleted nor moved already, nor moving otherwise, ordered by
	/// ID; each with whether a file that stays in `from` shows it still, so that it is
	/// written anew in the other space rather than moved. The content of a secret field has
	/// no thumbnail to take, even where an ordinary file has the same content.
	fn thumbnails_moving(
		&self,
		(from, moving): (SpaceId, &BTreeSet<EntityId>),
		taken: &Taken,
	) -> Result<Vec<(EntityId, bool)>, NodeError> {
		let open = self.space(from)?;
		let state = open.stored.log().state();
		let gone =
			|entity: EntityId| -> Result<bool, NodeError> {
				Ok(state.is_deleted(entity)?
					|| !state.current(entity, &fields::MOVED_INTO)?.is_empty())
			};
		let mut thumbnails = BTreeMap::new();
		for (content, (holding, _)) in &taken.contents {
			if !holding.contains(&CONTENT) || open.stored.secret_fields().contains(&CONTENT) {
				continue;
			}
			let mut staying = false;
			for file in open.stored.find(&CONTENT, &Value::Blob(*content))? {
				let file = state.canonical(file)?;
				staying |=
					!moving.contains(&file) && state.types(file)?.contains(&FILE) && !gone(file)?;
			}
			for thumbnail in open.stored.find(&THUMBNAIL_OF, &Value::Blob(*content))? {
				let thumbnail = state.canonical(thumbnail)?;
				if !moving.contains(&thumbnail)
					&& state.types(thumbnail)?.contains(&THUMBNAIL)
					&& !gone(thumbnail)?
				{
					*thumbnails.entry(thumbnail).or_insert(false) |= staying;
				}
			}
		}
		Ok(thumbnails.into_iter().collect())
	}

	/// Whether an entity of a space holds secret values, as far as this replica can tell: a
	/// value it opened, or a value written before its field was secret; or, while the space's
	/// secrets are locked, which hides which entity a sealed value is of, a field secret
	/// there that its types list.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn may_hold_secrets(&self, space: SpaceId, entity: EntityId) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let secret = open.stored.secret_fields();
		for member in state.merged(entity)? {
			for (field, ids) in storage.of_entity(member)? {
				if secret.contains(&field) {
					return Ok(true);
				}
				for id in ids {
					if storage.is_secret(id)? {
						return Ok(true);
					}
				}
			}
		}
		if self.secrets_unlocked(space) {
			return Ok(false);
		}
		let kinds: Vec<TypeName> = state.types(entity)?.into_iter().collect();
		Ok(!self.secret_fields_of(space, &kinds)?.is_empty())
	}

	/// The fields secret in `to` once a move from `from` defined there the types given, as
	/// `from` defines them: a field that one of them makes secret is secret, whatever the
	/// other types say.
	fn secret_once_moved(
		&self,
		(from, to): (SpaceId, SpaceId),
		kinds: &BTreeSet<TypeName>,
	) -> Result<BTreeSet<FieldName>, NodeError> {
		let mut secret_there = self.space(to)?.stored.secret_fields().clone();
		let source_types = &self.space(from)?.types;
		for kind in kinds {
			if let Some(definition) = source_types.get(kind) {
				secret_there.extend(
					definition
						.fields()
						.filter(|(_, field)| field.is_secret())
						.map(|(name, _)| name.clone()),
				);
			}
		}
		Ok(secret_there)
	}

	/// Refuses, before anything is written, a move of content that is not on this device,
	/// as neither space holds a copy, nor was it found here.
	fn check_contents_here(
		&self,
		(from, to): (SpaceId, SpaceId),
		taken: &Taken,
	) -> Result<(), NodeError> {
		let (source, target) = (&self.space(from)?.stored, &self.space(to)?.stored);
		for (content, (_, now)) in &taken.contents {
			if *now
				&& !source.holds_blob(content)
				&& !target.holds_blob(content)
				&& self.originals_here(from, content)?.is_empty()
			{
				return Err(NodeError::NotHere(content.to_string()));
			}
		}
		Ok(())
	}

	/// Refuses, before anything is written, a move of secret values that would not stay
	/// secret in `to`, where a field secret in `from` would be plain and this replica's
	/// member may not make it secret; and makes sure that this replica seals in `to` what will
	/// be secret there: its secrets unlocked, or set up there now, if no other replica holds
	/// them and its member may.
	fn prepare_secrets(
		&mut self,
		(from, to): (SpaceId, SpaceId),
		taken: &Taken,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let secret_there = self.secret_once_moved((from, to), &taken.kinds)?;
		if let Some(field) = taken.secret.difference(&secret_there).next()
			&& !self.may_mark_secret(to)?
		{
			return Err(NodeError::NotSecretThere(field.clone()));
		}
		let sealed_there = taken.entities.iter().any(|(_, carried)| {
			carried.iter().any(|statement| {
				let field = &statement.statement.field;
				secret_there.contains(field) || taken.secret.contains(field)
			})
		});
		if !sealed_there || self.secrets_unlocked(to) {
			return Ok(());
		}
		if to == self.info.personal {
			return Err(NodeError::MoveNeedsSecrets);
		}
		self.set_up_space_secrets(to, wall_millis, rng)
	}

	/// Whether this replica's member may make a field of a space secret for good: in the
	/// personal space, or a space without members, or as an admin.
	fn may_mark_secret(&self, space: SpaceId) -> Result<bool, NodeError> {
		Ok(space == self.info.personal
			|| !self.has_members(space)?
			|| self.check_admin(space, "make a field secret").is_ok())
	}

	/// Makes the fields of secret values that a move brings secret for good in `to`, where
	/// they are not secret already, in a change that governs the space.
	fn keep_secret(
		&mut self,
		to: SpaceId,
		secret: &BTreeSet<FieldName>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let open = self.space(to)?;
		let missing: Vec<FieldName> = secret
			.difference(open.stored.secret_fields())
			.cloned()
			.collect();
		let Some(first) = missing.first().cloned() else {
			return Ok(());
		};
		if !self.may_mark_secret(to)? {
			return Err(NodeError::NotSecretThere(first));
		}
		let own = self.own_record(to)?;
		let operations = open.secret_marks(&self.code_types, (own, missing), rng)?;
		if operations.is_empty() {
			return Err(NodeError::NotSecretThere(first));
		}
		self.commit(to, operations, wall_millis, rng)?;
		// The fields that the change made secret are secret from now on.
		self.read_again(to)?;
		if self.space(to)?.stored.secret_fields().contains(&first) {
			Ok(())
		} else {
			Err(NodeError::NotSecretThere(first))
		}
	}

	/// The entity, as merges make it canonical, and the folders, lists, memberships and
	/// members that it holds, and the files attached to it and to nothing else that stays,
	/// through every level, each once, but those deleted or moved already, the entity first.
	/// What names an entity merged into a folder or a list is in it too. A tree, or what is in one, moves only whole, which waits until trees are
	/// shared, so it is refused; and a membership moves only with its list.
	fn held_with(&self, from: SpaceId, entity: EntityId) -> Result<Vec<EntityId>, NodeError> {
		let open = self.space(from)?;
		let state = open.stored.log().state();
		let given = state.canonical(entity)?;
		if state.fields(given)?.is_empty() || state.is_deleted(given)? {
			return Err(NodeError::NoEntity(given.to_string()));
		}
		if state.types(given)?.contains(&MEMBERSHIP) {
			return Err(NodeError::OwnRecord(given.to_string()));
		}
		if self.moved_into(from, given)?.is_some() {
			return Err(NodeError::MovedAlready);
		}
		let naming = |field: &FieldName, each: EntityId| -> Result<Vec<EntityId>, NodeError> {
			let mut found = Vec::new();
			for member in state.merged(each)? {
				found.extend(open.stored.find(field, &Value::Reference(member))?);
			}
			Ok(found)
		};
		let mut held = Vec::new();
		let mut seen = BTreeSet::new();
		let mut waiting = VecDeque::from([given]);
		while let Some(next) = waiting.pop_front() {
			let each = state.canonical(next)?;
			if !seen.insert(each) {
				continue;
			}
			if each != given
				&& (state.fields(each)?.is_empty()
					|| state.is_deleted(each)?
					|| self.moved_into(from, each)?.is_some())
			{
				continue;
			}
			if state.fields(each)?.contains(&IN_TREE) || !naming(&IN_TREE, each)?.is_empty() {
				return Err(NodeError::TreeStays(each.to_string()));
			}
			held.push(each);
			waiting.extend(naming(&IN_FOLDER, each)?);
			// A file attached to it, as an app shows it in it, unless attached to something
			// else that stays too: one deleted, or moved already, stays nowhere.
			for file in naming(&ATTACHED_TO, each)? {
				let file = state.canonical(file)?;
				let mut alone = true;
				for statement in state.current(file, &ATTACHED_TO)? {
					let Some(other) = statement.value.as_reference() else {
						continue;
					};
					let other = state.canonical(other)?;
					if other != each
						&& !held.contains(&other)
						&& !waiting.contains(&other)
						&& !state.fields(other)?.is_empty()
						&& !state.is_deleted(other)?
						&& self.moved_into(from, other)?.is_none()
					{
						alone = false;
						break;
					}
				}
				if alone {
					waiting.push_back(file);
				}
			}
			for membership in naming(&fields::LIST, each)? {
				let membership = state.canonical(membership)?;
				if !state.types(membership)?.contains(&MEMBERSHIP)
					|| state.is_deleted(membership)?
				{
					continue;
				}
				waiting.push_back(membership);
				waiting.extend(
					state
						.current(membership, &fields::ITEM)?
						.iter()
						.filter_map(|statement| statement.value.as_reference()),
				);
			}
		}
		Ok(held)
	}

	/// The types of an entity of `from` that a move may take, or why it may not: one of the
	/// library's own records.
	fn check_movable(
		&self,
		from: SpaceId,
		entity: EntityId,
	) -> Result<BTreeSet<TypeName>, NodeError> {
		let open = self.space(from)?;
		let state = open.stored.log().state();
		let kinds = state.types(entity)?;
		if OWN_RECORDS.iter().any(|own| kinds.contains(own))
			|| kinds
				.iter()
				.any(|kind| kind.as_str().starts_with("org.nibkeep.") && *kind != MEMBERSHIP)
		{
			return Err(NodeError::OwnRecord(entity.to_string()));
		}
		Ok(kinds)
	}

	/// The statements that a move from `from` to `to` carries of an entity: with
	/// [`MoveHistory::Bring`], every statement that the entity and those merged into it have
	/// held, current or replaced, with when the space recorded each, by whom, and when it was
	/// replaced; otherwise, its current ones. Either way, not whether it is deleted, merged or
	/// moved; of a field whose last writer wins, only the value it shows; none that puts it
	/// in a folder that stays, as that finds it through its mark; and, with
	/// [`MoveSecrets::Leave`], no secret value, sealed or of a field secret in `from`. Secret
	/// values are carried as far as this replica opened them. References are to the entities
	/// as merges make them canonical, so that those to what moves along lead to it, and those
	/// to an entity that moved to `to` already, to that one there.
	fn carried_of(
		&self,
		(from, to): (SpaceId, SpaceId),
		entity: EntityId,
		(history, secrets): (MoveHistory, MoveSecrets),
		moving: &BTreeSet<EntityId>,
	) -> Result<Vec<Carried>, NodeError> {
		let open = self.space(from)?;
		let log = open.stored.log();
		let state = log.state();
		let storage = state.storage();
		let last_writer_wins = open.types.last_writer_wins_fields();
		let secret_here = open.stored.secret_fields();
		let mut carried = Vec::new();
		for member in state.merged(entity)? {
			for (field, ids) in storage.of_entity(member)? {
				if [
					fields::DELETED,
					fields::MERGED_INTO,
					fields::MOVED_INTO,
					fields::PURGE_ASKED,
				]
				.contains(&field)
				{
					continue;
				}
				let shown = if last_writer_wins.contains(&field) {
					state
						.last_written(entity, &field)?
						.map(|statement| statement.id)
				} else {
					None
				};
				for id in ids {
					if last_writer_wins.contains(&field) && shown != Some(id) {
						continue;
					}
					let Some(recorded) = storage.get(id)? else {
						continue;
					};
					let replaced = storage.superseded_at(id)?;
					if history == MoveHistory::Leave && replaced.is_some() {
						continue;
					}
					let secret = secret_here.contains(&field) || storage.is_secret(id)?;
					if secret && secrets == MoveSecrets::Leave {
						continue;
					}
					let mut statement = recorded.statement.into_owned();
					let mut there = false;
					if let Value::Reference(target) = statement.value {
						let target = state.canonical(target)?;
						statement.value = Value::Reference(target);
						if !moving.contains(&target)
							&& let Some((space, now)) = self.moved_into(from, target)?
							&& space == to
						{
							statement.value = Value::Reference(now);
							there = true;
						}
					}
					if replaced.is_none()
						&& statement.field == IN_FOLDER
						&& !there
						&& statement
							.value
							.as_reference()
							.is_some_and(|folder| !moving.contains(&folder))
					{
						continue;
					}
					let (recorded, author) = match history {
						MoveHistory::Bring => (Some(recorded.at), log.author_of(id)?),
						MoveHistory::Leave => (None, None),
					};
					carried.push(Carried {
						statement,
						secret,
						recorded,
						author,
						replaced,
					});
				}
			}
		}
		Ok(carried)
	}

	/// The folders and lists of `from` that stay and hold some of these entities, which move.
	fn staying_holders(
		&self,
		from: SpaceId,
		entities: &[EntityId],
	) -> Result<Vec<EntityId>, NodeError> {
		let open = self.space(from)?;
		let state = open.stored.log().state();
		let moving: BTreeSet<EntityId> = entities.iter().copied().collect();
		let mut staying = BTreeSet::new();
		for &entity in entities {
			for statement in &state.current(entity, &IN_FOLDER)? {
				if let Some(folder) = statement.value.as_reference() {
					staying.insert(state.canonical(folder)?);
				}
			}
			for membership in open.stored.find(&fields::ITEM, &Value::Reference(entity))? {
				let membership = state.canonical(membership)?;
				if !state.types(membership)?.contains(&MEMBERSHIP)
					|| state.is_deleted(membership)?
				{
					continue;
				}
				for statement in &state.current(membership, &fields::LIST)? {
					if let Some(list) = statement.value.as_reference() {
						staying.insert(state.canonical(list)?);
					}
				}
			}
		}
		// A holder that moved already is no longer here.
		let mut here = Vec::new();
		for holder in staying {
			if !moving.contains(&holder) && self.moved_into(from, holder)?.is_none() {
				here.push(holder);
			}
		}
		Ok(here)
	}

	/// Gives a blob of one space to another, with a key of the kind given, secret or
	/// ordinary, unless that one holds it so already: the copy that the first holds with a key
	/// of that kind, whose key is wrapped for the second once the copy was read whole, as a
	/// replica keeps one sealed copy of each content, reusing keys only within their kind
	/// ([technical design: spaces and moves]); or else, as where the second purged that copy, or the first
	/// holds none of that kind, sealed anew for the second from a copy that the first holds,
	/// or from where it was found on this device, unless a third space holds a copy of that
	/// kind that the second takes up.
	///
	/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
	fn copy_blob(
		&mut self,
		from: SpaceId,
		content: &Hash,
		(to, secret): (SpaceId, bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if !self
			.space(to)?
			.stored
			.held_copies(content, secret)
			.is_empty()
		{
			return Ok(());
		}
		let held = self.space(from)?.stored.holds_blob(content);
		let originals = if held {
			Vec::new()
		} else {
			self.originals_here(from, content)?
		};
		let mut target = self.spaces.remove(&to).ok_or(NodeError::NoSpace(to))?;
		let copied = (|| {
			let source = self.space(from)?;
			for (sealed, key) in source.stored.held_copies(content, secret) {
				let shared = target.stored.share_blob(
					&target.keys,
					content,
					(sealed, &key),
					secret,
					wall_millis,
					rng,
				)?;
				if shared {
					return Ok(());
				}
			}
			// A copy of a third space, as those of the first were tried already.
			let others = &self.spaces;
			let elsewhere = |content: &Hash| {
				held_copies_in(
					others
						.iter()
						.filter(|(id, _)| **id != from)
						.map(|(_, open)| open),
					content,
					secret,
				)
			};
			let sealed = if held {
				let reader = source.stored.open_blob(content)?;
				target.stored.import_blob_sharing(
					&target.keys,
					(reader, secret),
					&elsewhere,
					wall_millis,
					rng,
				)?
			} else {
				let mut sealed = None;
				for original in &originals {
					if let Ok(file) = File::open(original) {
						sealed = Some(target.stored.import_blob_sharing(
							&target.keys,
							(file, secret),
							&elsewhere,
							wall_millis,
							rng,
						)?);
						break;
					}
				}
				sealed.ok_or_else(|| NodeError::NotHere(content.to_string()))?
			};
			if sealed == *content {
				Ok(())
			} else {
				Err(NodeError::NotHere(content.to_string()))
			}
		})();
		self.spaces.insert(to, target);
		copied
	}
}

/// The operations that write the statements that a move carries of an entity on its new
/// entity `new`: under new IDs, drawn in the order of the old ones, so that values whose
/// order their IDs make keep it; each form of a value naming its first form by the first
/// form's new ID; references to what moves along leading to its new entity, as `moved` maps
/// them; and, with [`MoveHistory::Bring`], the mover's note of the space it came from, when
/// that space recorded it and by whom, and when it was replaced there, unless a note of an
/// earlier move tells it already, which stays whole; and the replaced ones replaced again.
fn carried_operations(
	carried: &[Carried],
	(new, moved): (EntityId, &BTreeMap<EntityId, EntityId>),
	(from, history): (SpaceId, MoveHistory),
	rng: &mut impl CryptoRng,
) -> Vec<Operation> {
	let mut carried: Vec<&Carried> = carried.iter().collect();
	carried.sort_by_key(|each| each.statement.id);
	let mut ids: Vec<StatementId> = carried.iter().map(|_| StatementId::random(rng)).collect();
	ids.sort();
	let renamed: BTreeMap<StatementId, StatementId> = carried
		.iter()
		.zip(&ids)
		.map(|(each, id)| (each.statement.id, *id))
		.collect();
	let mut operations = Vec::new();
	let mut replaced = Vec::new();
	for (each, id) in carried.into_iter().zip(ids) {
		let mut qualifiers = each.statement.qualifiers.clone();
		if let Some(first) = qualifiers.alternative()
			&& let Some(new) = renamed.get(&first)
		{
			let alternative: FieldName = fields::ALTERNATIVE;
			qualifiers.insert(alternative, Value::Text(new.to_string()));
		}
		if history == MoveHistory::Bring {
			let millis =
				|at: Timestamp| Value::Integer(i64::try_from(at.millis()).unwrap_or(i64::MAX));
			// A note of an earlier move stays whole: it tells of the space that the value came
			// from first, not of this one.
			if qualifiers.get(&fields::CARRIED_FROM).is_none() {
				qualifiers.insert(fields::CARRIED_FROM, Value::Text(from.to_string()));
				if let Some(author) = &each.author {
					qualifiers.insert(fields::CARRIED_BY, Value::Text(author.to_text()));
				}
				if let Some(at) = each.recorded {
					qualifiers.insert(fields::CARRIED_AT, millis(at));
				}
				if let Some(at) = each.replaced {
					qualifiers.insert(fields::CARRIED_REPLACED, millis(at));
				}
			}
		}
		let value = match &each.statement.value {
			Value::Reference(target) => {
				Value::Reference(moved.get(target).copied().unwrap_or(*target))
			}
			value => value.clone(),
		};
		operations.push(Operation::assert(Statement {
			id,
			entity: new,
			field: each.statement.field.clone(),
			value,
			qualifiers,
		}));
		if each.replaced.is_some() {
			replaced.push(id);
		}
	}
	if !replaced.is_empty() {
		operations.push(Operation::retract(new, replaced));
	}
	operations
}
