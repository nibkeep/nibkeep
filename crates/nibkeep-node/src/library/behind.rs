//! Purging what a move leaves behind ([technical design: spaces and moves]): the mover's replica purges, in
//! the old space, what its member's role lets it purge there, once the new entity counts and
//! another device holds it; a mark asks the old space's admins for the rest, and an admin's
//! replica lists those requests where the new entity counts with the same values.
//!
//! [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::Role as MemberRole;
use nibkeep_log::ChangeId;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Value, fields};
use nibkeep_sync::Synced;

use super::Library;
use super::members::users_and_owners;
use crate::NodeError;

/// An entity that a move left behind, whose mover asked that the rest of it be purged, as an
/// admin's replica lists it: where the new entity counts with the same values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurgeRequest {
	/// The entity left behind.
	pub entity: EntityId,
	/// The space it is in now, and the entity it is there.
	pub moved_into: (SpaceId, EntityId),
}

/// What a member may purge of an entity left behind, of one kind, secret or ordinary: the
/// statements, and the content that no statement that stays holds.
#[derive(Default)]
struct Part {
	statements: Vec<StatementId>,
	contents: Vec<Hash>,
}

/// A move of this replica's whose leftovers hold content, which waits for that content's
/// copies in the new space to be proven before it is purged.
pub(crate) struct WaitingMove {
	/// The old space, and the entity left there.
	pub(crate) space: SpaceId,
	pub(crate) entity: EntityId,
	/// The space it moved into.
	pub(crate) now_in: SpaceId,
	/// The contents that the leftovers hold which moved, each with whether it is secret.
	pub(crate) contents: Vec<(Hash, bool)>,
}

/// What a member may purge of an entity left behind: its ordinary values, and its secret
/// values, sealed or of a field secret in the space, each with their content.
#[derive(Default)]
struct Remains {
	ordinary: Part,
	secret: Part,
}

impl Remains {
	/// How many values it purges.
	fn len(&self) -> usize {
		self.ordinary.statements.len() + self.secret.statements.len()
	}

	/// Whether it purges no value.
	fn is_empty(&self) -> bool {
		self.len() == 0
	}
}

/// Leaves out of `contents`, ordinary and secret, what a statement that stays holds in a
/// field of the same kind, of the entity left behind or another: one that none of
/// `statements` is. A purge of content destroys only the keys of its kind, so a secret
/// file's content stays readable through the secrets, and the other way round, as a purge
/// of a value says.
fn keep_held<S: Statements>(
	storage: &S,
	secret_here: &BTreeSet<FieldName>,
	statements: &[BTreeSet<StatementId>; 2],
	contents: &mut [BTreeSet<Hash>; 2],
) -> Result<(), S::Error> {
	if contents.iter().all(BTreeSet::is_empty) {
		return Ok(());
	}
	for other in storage.described()? {
		for (field, ids) in storage.of_entity(other)? {
			for id in ids {
				if statements.iter().any(|kind| kind.contains(&id)) {
					continue;
				}
				// What a thumbnail was made of, it names without holding it.
				if let Some(recorded) = storage.get(id)?
					&& let Value::Blob(content) = &recorded.statement.value
					&& !nibkeep_std::names_content_only(&field)
				{
					let kind = secret_here.contains(&field) || storage.is_secret(id)?;
					contents[usize::from(kind)].remove(content);
				}
			}
		}
	}
	Ok(())
}

/// The fields whose statements a purge of what stays behind leaves: the marks.
pub(crate) fn is_mark(field: &FieldName) -> bool {
	*field == fields::MOVED_INTO || *field == fields::PURGE_ASKED || *field == fields::MERGED_INTO
}

/// Who purges: a member who may purge anything, as in the personal space or as an admin; an
/// editor, who purges only the changes their member's replicas signed; or nobody, as a
/// replica that its space does not list, or revoked.
enum Purger {
	All,
	Own(EntityId, BTreeMap<[u8; 32], EntityId>),
	Nobody,
}

impl Library {
	/// The members of `from` who are no members of `to`, by name, sorted, and whether this
	/// replica's member may purge all that a move from `from` leaves behind, as
	/// [`crate::MovePreview`] says.
	pub(crate) fn losing_and_purging(
		&self,
		(from, to): (SpaceId, SpaceId),
	) -> Result<(Vec<String>, bool), NodeError> {
		if from == self.info.personal || !self.has_members(from)? {
			return Ok((Vec::new(), true));
		}
		let users_of = |space: SpaceId| -> Result<BTreeSet<EntityId>, NodeError> {
			if space == self.info.personal || !self.has_members(space)? {
				return Ok(BTreeSet::from([self.info.user]));
			}
			Ok(self
				.members(space)?
				.into_iter()
				.filter(|member| member.role != MemberRole::None)
				.filter_map(|member| member.user)
				.collect())
		};
		let staying = users_of(to)?;
		let members = self.members(from)?;
		let mut losing: Vec<String> = members
			.iter()
			.filter(|member| member.role != MemberRole::None)
			.filter(|member| member.user.is_none_or(|user| !staying.contains(&user)))
			.map(|member| member.name.clone().unwrap_or_default())
			.collect();
		losing.sort();
		losing.dedup();
		let purges_all = members
			.iter()
			.any(|member| member.this_user && member.role == MemberRole::Admin);
		Ok((losing, purges_all))
	}

	/// Purges what moves that this replica made left behind, where a sync that started at
	/// `started`, after the move, just synced the new entity's space with another device,
	/// which then holds it: what this replica's member may purge in the old space, its own
	/// changes as an editor, everything but the marks as an admin or in the personal space.
	/// Leftovers that hold content, which the old space's replicas would lose, wait for the
	/// planner, which purges them once that content's copies in the new space are proven
	/// ([`Library::remove_proven`]). Called after each sync; returns the copies of this
	/// device's checkouts that it deleted.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read, or a purge cannot be saved.
	pub fn settle_moves(
		&mut self,
		synced: &Synced,
		started: u64,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<PathBuf>, NodeError> {
		// Not where the other device was revoked or removed, which is sent nothing new, nor
		// in a space that this device dropped and deleted after the sync.
		let mut held = BTreeSet::new();
		for space in synced.spaces.iter().map(|space| space.space) {
			if !self.spaces.contains_key(&space) {
				continue;
			}
			let open = self.space(space)?;
			let log = open.stored.log();
			if !log.is_revoked(&synced.peer, &open.keys)?
				&& !log.is_removed(&synced.peer, &open.keys)?
			{
				held.insert(space);
			}
		}
		let synced = held;
		if synced.is_empty() || self.is_ai_agent()? {
			return Ok(Vec::new());
		}
		let own = self.replica.signing.verifying_key();
		let mut due = Vec::new();
		for space in self.spaces() {
			if self.check_not_revoked(space).is_err() {
				continue;
			}
			for entity in self.asked(space)? {
				// Only this replica's own moves, made before the sync started.
				if !self.asked_by(space, entity, (&own, started))? {
					continue;
				}
				if let Some((now_in, _)) = self.moved_into(space, entity)?
					&& synced.contains(&now_in)
				{
					due.push((space, entity, now_in));
				}
			}
		}
		let mut deleted = Vec::new();
		for (space, entity, now_in) in due {
			// Leftovers that hold content that moved wait for the planner, which purges them
			// once the copies of that content in the new space are proven.
			if let Some(remains) = self.remains(space, entity)?
				&& !remains.is_empty()
				&& self.moved_contents(now_in, &remains).is_empty()
			{
				deleted.extend(self.purge_remains(space, &remains, wall_millis, rng)?);
			}
		}
		Ok(deleted)
	}

	/// This replica's own moves whose leftovers hold content that moved, which wait for the
	/// planner: each with its old space and entity, the new space, and those contents, with
	/// whether each is secret ([technical design: the planner]).
	///
	/// [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner
	pub(crate) fn moves_waiting_for_copies(&self) -> Result<Vec<WaitingMove>, NodeError> {
		if self.is_ai_agent()? {
			return Ok(Vec::new());
		}
		let own = self.replica.signing.verifying_key();
		let mut waiting = Vec::new();
		for space in self.spaces() {
			if self.check_not_revoked(space).is_err() {
				continue;
			}
			for entity in self.asked(space)? {
				if !self.asked_by(space, entity, (&own, u64::MAX))? {
					continue;
				}
				let Some((now_in, _)) = self.moved_into(space, entity)? else {
					continue;
				};
				if !self.spaces.contains_key(&now_in) {
					continue;
				}
				let Some(remains) = self.remains(space, entity)? else {
					continue;
				};
				if remains.is_empty() {
					continue;
				}
				let contents = self.moved_contents(now_in, &remains);
				if contents.is_empty() {
					continue;
				}
				waiting.push(WaitingMove {
					space,
					entity,
					now_in,
					contents,
				});
			}
		}
		Ok(waiting)
	}

	/// Adds to what a move left behind of a file, `taken`, where its plain copies were: its
	/// origins and checkouts that stayed, as those of other members' devices, deleted ones
	/// too, so that nothing that stays holds its content; those that moved are left behind
	/// themselves.
	fn with_plain_copies(
		&self,
		space: SpaceId,
		taken: &mut Vec<EntityId>,
	) -> Result<(), NodeError> {
		let files: BTreeSet<EntityId> = taken.iter().copied().collect();
		for (record, moved, _) in self.plain_copies_of(space, &files)? {
			if !moved {
				taken.push(record);
			}
		}
		Ok(())
	}

	/// The contents that leftovers hold which moved into `now_in`, as a key of a sealed copy
	/// there says, each with whether it is secret: those whose copies there are to be proven
	/// before the leftovers go. What did not move, as the old versions that a move without
	/// history leaves, goes as the mover chose.
	fn moved_contents(&self, now_in: SpaceId, remains: &Remains) -> Vec<(Hash, bool)> {
		let Some(there) = self.spaces.get(&now_in) else {
			return Vec::new();
		};
		let log = there.stored.log();
		remains
			.ordinary
			.contents
			.iter()
			.map(|content| (*content, false))
			.chain(
				remains
					.secret
					.contents
					.iter()
					.map(|content| (*content, true)),
			)
			.filter(|(content, secret)| {
				log.blob_keys(content)
					.any(|(sealed, _)| log.is_secret_blob(sealed) == *secret)
			})
			.collect()
	}

	/// Purges what this replica's move of an entity left behind, as [`Library::settle_moves`]
	/// does, once the planner has proven the copies in the new space of the content it holds;
	/// returns the copies of this device's checkouts that it deleted, or `None` if nothing
	/// was left to purge.
	pub(crate) fn purge_left_behind(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Option<Vec<PathBuf>>, NodeError> {
		match self.remains(space, entity)? {
			Some(remains) if !remains.is_empty() => self
				.purge_remains(space, &remains, wall_millis, rng)
				.map(Some),
			_ => Ok(None),
		}
	}

	/// Purges what a member may purge of an entity left behind, in a purge for each kind of
	/// content, secret or ordinary, as a purge destroys keys of one kind; returns the copies
	/// of this device's checkouts that it deleted. If the second purge fails, the first
	/// stays, and the next purge of what stays behind takes the rest.
	fn purge_remains(
		&mut self,
		space: SpaceId,
		remains: &Remains,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<PathBuf>, NodeError> {
		let mut deleted = Vec::new();
		for (part, of_secrets) in [(&remains.ordinary, false), (&remains.secret, true)] {
			if part.statements.is_empty() {
				continue;
			}
			deleted.extend(self.purge_statements(
				space,
				&part.statements,
				(&part.contents, of_secrets),
				wall_millis,
				rng,
			)?);
		}
		Ok(deleted)
	}

	/// The entities of a space that moves left behind and whose movers asked that the rest
	/// be purged, which this replica's member, an admin there, may purge: those whose new
	/// entity counts in a space this replica holds, with the same values, and that still hold
	/// more than their marks. Empty for a member who is no admin, and for the personal space,
	/// where the mover purges all.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn purge_requests(&self, space: SpaceId) -> Result<Vec<PurgeRequest>, NodeError> {
		if space == self.info.personal || !self.has_members(space)? {
			return Ok(Vec::new());
		}
		if self.check_settled_admin(space, "purge").is_err() {
			return Ok(Vec::new());
		}
		let mut requests = Vec::new();
		for entity in self.asked(space)? {
			if let Some(moved_into) = self.purgeable_behind(space, entity)?
				&& self
					.remains(space, entity)?
					.is_some_and(|remains| !remains.is_empty())
			{
				requests.push(PurgeRequest { entity, moved_into });
			}
		}
		Ok(requests)
	}

	/// How many values [`Library::purge_behind`] would purge of an entity that a move left
	/// behind: what this replica's member may purge there. `None` if no move asked for a
	/// purge of it.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn behind(&self, space: SpaceId, entity: EntityId) -> Result<Option<usize>, NodeError> {
		Ok(self.remains(space, entity)?.map(|remains| remains.len()))
	}

	/// Purges what a move left behind of an entity, as asked: what this replica's member may
	/// purge there, as [`Library::settle_moves`] says, once the new entity counts in a space
	/// this replica holds with the values that the old one shows; returns how many values it
	/// purged.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoPurgeAsked`] if no move asked for a purge of the entity,
	/// [`NodeError::MovedDiffers`] if what moved is not here or differs, and an error if the
	/// space cannot be read or the purge saved.
	pub fn purge_behind(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("purge")?;
		let Some(remains) = self.remains(space, entity)? else {
			return Err(NodeError::NoPurgeAsked(entity.to_string()));
		};
		if remains.is_empty() {
			return Ok(0);
		}
		if self.purgeable_behind(space, entity)?.is_none() {
			return Err(NodeError::MovedDiffers(entity.to_string()));
		}
		self.purge_remains(space, &remains, wall_millis, rng)?;
		Ok(remains.len())
	}

	/// Where the entity that a move made of one left behind counts, in a space this replica
	/// holds, if it counts there with the values that the one left behind shows.
	fn purgeable_behind(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Option<(SpaceId, EntityId)>, NodeError> {
		let Some(moved_into) = self.moved_into(space, entity)? else {
			return Ok(None);
		};
		Ok(self
			.same_values((space, entity), moved_into)?
			.then_some(moved_into))
	}

	/// The entities of a space whose moves asked that what stays of them be purged.
	fn asked(&self, space: SpaceId) -> Result<Vec<EntityId>, NodeError> {
		Ok(self
			.space(space)?
			.stored
			.find(&fields::PURGE_ASKED, &Value::Boolean(true))?)
	}

	/// The changes of the marks that ask for a purge of an entity, with when each was made.
	fn asking_changes(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Vec<(ChangeId, u64)>, NodeError> {
		let state = self.space(space)?.stored.log().state();
		let mut changes = Vec::new();
		for statement in state.current(entity, &fields::PURGE_ASKED)? {
			if let Some(recorded) = state.storage().get(statement.id)?
				&& let Some(change) = recorded.change
			{
				changes.push((ChangeId::from_hash(change), recorded.at.millis()));
			}
		}
		Ok(changes)
	}

	/// Whether a key signed a mark that asks for a purge of an entity, before `before`:
	/// whether its replica made the move, before a sync that started then.
	fn asked_by(
		&self,
		space: SpaceId,
		entity: EntityId,
		(key, before): (&VerifyingKey, u64),
	) -> Result<bool, NodeError> {
		let log = self.space(space)?.stored.log();
		for (change, at) in self.asking_changes(space, entity)? {
			if at < before
				&& log
					.change(change)?
					.is_some_and(|change| change.author() == key)
			{
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// Who this replica's member is, as one who purges what moves leave in a space.
	fn purger(&self, space: SpaceId) -> Result<Purger, NodeError> {
		if space == self.info.personal || !self.has_members(space)? {
			return Ok(Purger::All);
		}
		if self.check_not_revoked(space).is_err() {
			return Ok(Purger::Nobody);
		}
		let Some(own) = self.member_in(space)? else {
			return Ok(Purger::Nobody);
		};
		let open = self.space(space)?;
		let roles = open.stored.log().member_roles(&open.keys)?;
		Ok(match roles.get(&own) {
			Some(MemberRole::Admin) => Purger::All,
			Some(MemberRole::Editor) => {
				Purger::Own(own, users_and_owners(&open.stored, &open.keys)?.1)
			}
			_ => Purger::Nobody,
		})
	}

	/// What this replica's member may purge of an entity that a move left behind, as asked:
	/// every statement, current or past, but the marks, of it and of the entities merged into
	/// it before the move, that the move's mark builds on; of a secret value, sealed or of a
	/// field secret in the space, only one that moved, as this replica sees the new entity
	/// hold the same value in the same field, which needs the secrets of both spaces unlocked,
	/// so that a value left behind, or one sealed out of sight, stays; of those, as an editor
	/// of a shared space, only the ones its member's replicas signed; and, but for an editor,
	/// the content of its files that no statement that stays holds. `None` if no move asked
	/// for a purge of it.
	fn remains(&self, space: SpaceId, entity: EntityId) -> Result<Option<Remains>, NodeError> {
		let marks: Vec<ChangeId> = self
			.asking_changes(space, entity)?
			.into_iter()
			.map(|(change, _)| change)
			.collect();
		if marks.is_empty() {
			return Ok(None);
		}
		let purger = self.purger(space)?;
		if matches!(purger, Purger::Nobody) {
			return Ok(Some(Remains::default()));
		}
		// What the new entity holds, read once a secret value is met.
		let mut moved: Option<Vec<(FieldName, Value)>> = None;
		let open = self.space(space)?;
		let secret_here = open.stored.secret_fields();
		let log = open.stored.log();
		let state = log.state();
		let storage = state.storage();
		let is_before = |change: ChangeId| -> Result<bool, NodeError> {
			for mark in &marks {
				if change != *mark && log.is_ancestor_of(change, *mark)? {
					return Ok(true);
				}
			}
			Ok(false)
		};
		// The entity, and those merged into it before the move, which the move took; never
		// one merged with it since, which the move did not take.
		let mut taken = vec![entity];
		for other in state.merged(state.canonical(entity)?)? {
			if other == entity {
				continue;
			}
			for merge in state.current(other, &fields::MERGED_INTO)? {
				if merge.value.as_reference() == Some(entity)
					&& let Some(recorded) = storage.get(merge.id)?
					&& let Some(change) = recorded.change
					&& is_before(ChangeId::from_hash(change))?
				{
					taken.push(other);
				}
			}
		}
		self.with_plain_copies(space, &mut taken)?;
		// The statements, and the contents, of each kind: ordinary, then secret.
		let mut statements = [BTreeSet::new(), BTreeSet::new()];
		let mut contents = [BTreeSet::new(), BTreeSet::new()];
		for &each in &taken {
			for (field, ids) in storage.of_entity(each)? {
				if is_mark(&field) {
					continue;
				}
				for id in ids {
					let Some(recorded) = storage.get(id)? else {
						continue;
					};
					let secret = secret_here.contains(&field) || storage.is_secret(id)?;
					// A secret value only where it moved, as far as this replica sees.
					let value = &recorded.statement.value;
					if secret && !self.secret_moved((space, entity), (&field, value), &mut moved)? {
						continue;
					}
					let kind = usize::from(secret);
					let Some(change) = recorded.change.map(ChangeId::from_hash) else {
						continue;
					};
					// An editor purges only the changes their own replicas signed, which is
					// cheaper to tell than whether the move took it.
					if let Purger::Own(own, owners) = &purger {
						let signer = log
							.change(change)?
							.and_then(|change| owners.get(change.author().as_bytes()).copied());
						if signer != Some(*own) {
							continue;
						}
					}
					// Only what the move took: what its mark builds on.
					if !is_before(change)? {
						continue;
					}
					// What a thumbnail was made of, it names without holding it.
					if matches!(purger, Purger::All)
						&& let Value::Blob(content) = &recorded.statement.value
						&& !nibkeep_std::names_content_only(&field)
					{
						contents[kind].insert(*content);
					}
					statements[kind].insert(id);
				}
			}
		}
		keep_held(storage, secret_here, &statements, &mut contents)?;
		self.take_thumbnails_staying(space, &mut statements, &mut contents)?;
		let [ordinary, secret] = statements;
		let [ordinary_contents, secret_contents] = contents;
		Ok(Some(Remains {
			ordinary: Part {
				statements: ordinary.into_iter().collect(),
				contents: ordinary_contents.into_iter().collect(),
			},
			secret: Part {
				statements: secret.into_iter().collect(),
				contents: secret_contents.into_iter().collect(),
			},
		}))
	}

	/// Adds to what a purge of what a move left behind takes, ordinary and secret, the
	/// thumbnails made of the ordinary content it takes that stayed, as of a version the move
	/// did not take: each one's statements, but its marks, and its own content, unless a
	/// statement that stays holds that. Those that moved are left behind themselves, and
	/// purged so.
	fn take_thumbnails_staying(
		&self,
		space: SpaceId,
		statements: &mut [BTreeSet<StatementId>; 2],
		contents: &mut [BTreeSet<Hash>; 2],
	) -> Result<(), NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let mut pictures = BTreeSet::new();
		for content in &contents[0] {
			for thumbnail in open
				.stored
				.find(&nibkeep_std::THUMBNAIL_OF, &Value::Blob(*content))?
			{
				if !state.current(thumbnail, &fields::MOVED_INTO)?.is_empty() {
					continue;
				}
				for (field, ids) in storage.of_entity(thumbnail)? {
					if is_mark(&field) {
						continue;
					}
					for id in ids {
						if field == nibkeep_std::CONTENT
							&& let Some(recorded) = storage.get(id)?
							&& let Value::Blob(picture) = recorded.statement.value
						{
							pictures.insert(picture);
						}
						statements[0].insert(id);
					}
				}
			}
		}
		if !pictures.is_empty() {
			let mut theirs = [pictures, BTreeSet::new()];
			keep_held(
				storage,
				open.stored.secret_fields(),
				statements,
				&mut theirs,
			)?;
			let [pictures, _] = theirs;
			contents[0].extend(pictures);
		}
		Ok(())
	}

	/// Whether a secret value of an entity that a move left behind moved, as this replica
	/// sees the new entity hold the same value in the same field, a reference leading where
	/// what it refers to moved, as a move carries it. Reads what the new entity holds into
	/// `moved` the first time.
	fn secret_moved(
		&self,
		(space, entity): (SpaceId, EntityId),
		(field, value): (&FieldName, &Value),
		moved: &mut Option<Vec<(FieldName, Value)>>,
	) -> Result<bool, NodeError> {
		if moved.is_none() {
			*moved = Some(self.moved_values(space, entity)?);
		}
		let carried = match value {
			Value::Reference(target) => {
				let target = self.space(space)?.stored.log().state().canonical(*target)?;
				self.moved_into(space, target)?
					.map_or_else(|| value.clone(), |(_, now)| Value::Reference(now))
			}
			_ => value.clone(),
		};
		Ok(moved
			.as_deref()
			.unwrap_or_default()
			.iter()
			.any(|(moved_field, moved_value)| {
				moved_field == field && (*moved_value == carried || moved_value == value)
			}))
	}

	/// The values, current or past, that the entity a move made of one left behind holds,
	/// each with its field, as far as this replica sees them: none where it does not hold the
	/// space of the new entity, and no secret value of it while that space's secrets are
	/// locked.
	fn moved_values(
		&self,
		space: SpaceId,
		entity: EntityId,
	) -> Result<Vec<(FieldName, Value)>, NodeError> {
		let Some((now_in, new)) = self.moved_into(space, entity)? else {
			return Ok(Vec::new());
		};
		let state = self.space(now_in)?.stored.log().state();
		let storage = state.storage();
		let mut values = Vec::new();
		for member in state.merged(state.canonical(new)?)? {
			for (field, ids) in storage.of_entity(member)? {
				for id in ids {
					if let Some(recorded) = storage.get(id)? {
						values.push((field.clone(), recorded.statement.into_owned().value));
					}
				}
			}
		}
		Ok(values)
	}

	/// Whether the entity a move made counts with the values that the one it left behind
	/// shows now: each of the old entity's fields that a move carries has, on the new one, a
	/// value equal to one of the old one's, a reference leading where the old one's moved, if
	/// it moved; one, as a field whose last writer wins carries only the value it shows.
	fn same_values(
		&self,
		(space, entity): (SpaceId, EntityId),
		(now_in, new): (SpaceId, EntityId),
	) -> Result<bool, NodeError> {
		let old_state = self.space(space)?.stored.log().state();
		let new_state = self.space(now_in)?.stored.log().state();
		let entity = old_state.canonical(entity)?;
		for field in old_state.storage().current_fields(entity)? {
			if is_mark(&field) || field == fields::DELETED {
				continue;
			}
			let theirs = new_state.current(new, &field)?;
			let (mut carried, mut matched) = (false, false);
			for statement in old_state.current(entity, &field)? {
				let value = match &statement.value {
					Value::Reference(target) => match self.moved_into(space, *target)? {
						Some((_, moved)) => Value::Reference(moved),
						// A folder that stays holds what moved through its mark alone.
						None if field == nibkeep_std::IN_FOLDER => continue,
						None => statement.value.clone(),
					},
					value => value.clone(),
				};
				carried = true;
				matched |= theirs.iter().any(|their| their.value == value);
			}
			if carried && !matched {
				return Ok(false);
			}
		}
		Ok(true)
	}
}
