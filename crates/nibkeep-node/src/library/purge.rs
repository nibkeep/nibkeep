//! Purging values for good: every change that asserted them loses its key and payload, and
//! the purge states again what else they held ([technical design: purging]). A value that refers to
//! a blob, such as a file's content, takes the blob with it, and the checkouts of its
//! content ([technical design: checkouts]).
//!
//! [technical design: purging]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#64-purging
//! [technical design: checkouts]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#92-checkouts

use std::collections::BTreeSet;
use std::path::PathBuf;

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_model::storage::Statements;
use nibkeep_model::{FieldName, Value};
use nibkeep_std::{CHECKOUT, CONTENT, FOUND_ON, ORIGIN, PATH};

use super::Library;
use crate::NodeError;

/// What a purge takes away: the statements of the values, current or past, and how many
/// changes lose their keys and payloads; the content they refer to, with its sealed copies
/// and this device's checkouts of it; and the originals of that content that NibKeep knows
/// of, which it leaves where they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Purge {
	/// The statements of the values, and of every other value that holds content that they
	/// hold.
	pub statements: Vec<StatementId>,
	/// How many changes that asserted them lose their keys and payloads, and are stated
	/// again without them.
	pub changes: usize,
	/// How many of the values are secret, and lose only their own keys.
	pub secret: usize,
	/// The content hashes of the blobs that the values refer to, which are purged with them.
	pub contents: Vec<Hash>,
	/// Whether that content is a secret field's, such as a secret file's, whose secret
	/// copies are purged; or else an ordinary field's, whose ordinary copies are. The same
	/// content in fields of the other kind is sealed apart, and stays ([technical design: secret files]).
	///
	/// [technical design: secret files]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#95-secret-files
	pub of_secrets: bool,
	/// How many sealed copies of that content lose their keys: those whose keys this replica
	/// has opened.
	pub blobs: usize,
	/// The replica's other spaces that keep that content too, ordered by ID: those that keep
	/// a key, open or not, for a sealed copy that this space purges, and those that read
	/// another copy of it with a key of the same kind. A purge destroys only its own space's
	/// keys, so the content stays there, and an app says so before the purge. Spaces that
	/// this replica does not hold are not known ([technical design: spaces and moves]).
	///
	/// [technical design: spaces and moves]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#131-spaces-and-moves
	pub elsewhere: Vec<SpaceId>,
	/// The entities other than the one purged whose values hold that content, current or
	/// past, which lose it too, but the origins and checkouts of files, and thumbnails: files
	/// with the same content, and any other value that refers to it.
	pub also: Vec<EntityId>,
	/// The thumbnails made of that content, ordered by ID, which are purged whole, their own
	/// content too ([technical design: thumbnails]).
	///
	/// [technical design: thumbnails]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#98-thumbnails
	pub thumbnails: Vec<EntityId>,
	/// The checkouts on this device of that content, ordered by path.
	pub checkouts: Vec<PurgedCheckout>,
	/// The copies on this device of trees' files whose version holds that content, ordered by
	/// path ([technical design: trees]).
	///
	/// [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees
	pub tree_copies: Vec<PurgedCheckout>,
	/// Where files with that content were found, on which device, ordered by device and
	/// path: their originals, which a purge leaves where they are.
	pub originals: Vec<(EntityId, String)>,
	/// The person's own copies that wait before they destroy what the purge destroys, as a
	/// purge says before the person confirms: this device first
	/// ([technical design: copies that wait]).
	///
	/// [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait
	pub waiting: Vec<super::waiting::WaitingCopy>,
}

/// What a purge of content takes away besides the values named: the content, the entities
/// whose values hold it, as they show, and the thumbnails made of it.
struct ContentsPurged {
	contents: BTreeSet<Hash>,
	holders: BTreeSet<EntityId>,
	thumbnails: BTreeSet<EntityId>,
}

/// A copy on this device of content that a purge takes away: a checkout's, or a tree's file's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurgedCheckout {
	/// Where its copy is.
	pub path: PathBuf,
	/// Whether its copy was edited, and stays; otherwise it is deleted, or would be.
	pub edited: bool,
	/// Whether its copy was deleted.
	pub deleted: bool,
}

impl Library {
	/// What purging an entity's values of a field would take away: every value it has held,
	/// current or past, or only those equal to `value`, also those of the entities merged
	/// into it. Content that a value refers to, such as a file's, goes with every value
	/// that holds it in a field of the same kind, secret or ordinary, of any entity, current
	/// or past, and with its sealed copies of that kind; finding those values reads the whole
	/// space, as content is rarely purged, and a value left behind would refer to content
	/// that nothing can read. Ordinary content takes its thumbnails with it, whole, with
	/// their own content. The same content in a field of the other kind is sealed apart,
	/// and stays: purging a file leaves a secret file of the same content, whether the
	/// secrets are unlocked or not, and the other way round.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space; if the entity has no such value;
	/// if the field is secret and the space's secrets are locked while it holds secret
	/// values, as only unlocked secrets show which values they are; if a value cannot be purged, such as a replica's key; or
	/// if the space's state cannot be read.
	pub fn purgeable(
		&self,
		space: SpaceId,
		entity: EntityId,
		field: &FieldName,
		value: Option<&Value>,
	) -> Result<Purge, NodeError> {
		let open = self.space(space)?;
		// Values written before their field was secret are plain, and show; sealed ones
		// show only once they are unlocked.
		if open.stored.secret_fields().contains(field)
			&& open.keys.newest_secrets().is_none()
			&& !nibkeep_log::History::sealed(open.stored.log().history())?.is_empty()
		{
			return Err(NodeError::SecretsLocked);
		}
		let log = open.stored.log();
		let state = log.state();
		let mut statements = Vec::new();
		for entity in state.merged(state.canonical(entity)?)? {
			for id in state.storage().of_field(entity, field)? {
				let Some(statement) = state.statement(id)? else {
					continue;
				};
				if value.is_none_or(|value| statement.value == *value) {
					statements.push(id);
				}
			}
		}
		if statements.is_empty() {
			return Err(NodeError::NoValue(field.clone()));
		}
		let of_secrets = open.stored.secret_fields().contains(field);
		let ContentsPurged {
			contents,
			holders,
			thumbnails,
		} = self.purge_contents(space, &mut statements, of_secrets)?;
		let mut secret = 0;
		for &statement in &statements {
			if log.is_sealed(statement)? {
				secret += 1;
			}
		}
		let changes = log
			.changes_to_purge(statements.iter().copied(), &open.keys)?
			.len();
		let blobs = contents
			.iter()
			.map(|content| {
				log.blob_keys(content)
					.filter(|(sealed, _)| log.is_secret_blob(sealed) == of_secrets)
					.count()
			})
			.sum();
		// Secret files have no checkouts; one of the same content is a file's, which stays.
		let holding = if of_secrets {
			Vec::new()
		} else {
			self.checkouts_holding(space, &contents)?
		};
		let purged_copy = |(path, edited)| PurgedCheckout {
			path,
			edited,
			deleted: false,
		};
		let checkouts = holding.into_iter().map(purged_copy).collect();
		let tree_copies = self.tree_copies_holding(space, &contents, of_secrets)?;
		let tree_copies = tree_copies.into_iter().map(purged_copy).collect();
		let purged = state.canonical(entity)?;
		let (also, originals) = also_and_originals(state, (purged, &holders), &thumbnails)?;
		let elsewhere = self.spaces_holding(space, &contents, of_secrets)?;
		Ok(Purge {
			statements,
			changes,
			secret,
			contents: contents.into_iter().collect(),
			of_secrets,
			blobs,
			elsewhere,
			also,
			thumbnails: thumbnails.into_iter().collect(),
			checkouts,
			tree_copies,
			originals: originals.into_iter().collect(),
			waiting: self.waiting_copies(space)?,
		})
	}

	/// The content that the statements to purge refer to, the entities whose values hold
	/// it, as the entities they show as, and the thumbnails made of it; every value of the
	/// space that holds it, of any field secret if `of_secrets` says so and ordinary
	/// otherwise, current or past, is added to `statements`. A value that only names a
	/// content, as a thumbnail's `std.thumbnail-of` does, takes no content with it. Of
	/// ordinary content, every value that a thumbnail made of it has held is added too, but
	/// its marks; and the thumbnail's own content goes with it, unless something else holds
	/// that content, as the thumbnail of another picture alike may.
	fn purge_contents(
		&self,
		space: SpaceId,
		statements: &mut Vec<StatementId>,
		of_secrets: bool,
	) -> Result<ContentsPurged, NodeError> {
		let open = self.space(space)?;
		let secret_fields = open.stored.secret_fields();
		let state = open.stored.log().state();
		let mut contents = BTreeSet::new();
		for &id in statements.iter() {
			if let Some(statement) = state.statement(id)?
				&& let Value::Blob(content) = statement.value
				&& !nibkeep_std::names_content_only(&statement.field)
			{
				contents.insert(content);
			}
		}
		let mut holders = BTreeSet::new();
		let mut thumbnails = BTreeSet::new();
		if contents.is_empty() {
			return Ok(ContentsPurged {
				contents,
				holders,
				thumbnails,
			});
		}
		let storage = state.storage();
		let mut seen: BTreeSet<StatementId> = statements.iter().copied().collect();
		let mut made_of = BTreeSet::new();
		for entity in storage.described()? {
			for (field, ids) in storage.of_entity(entity)? {
				// Content in a field of the other kind is sealed apart, and stays.
				if secret_fields.contains(&field) != of_secrets {
					continue;
				}
				for id in ids {
					let Some(recorded) = storage.get(id)? else {
						continue;
					};
					if let Value::Blob(content) = &recorded.statement.value
						&& contents.contains(content)
					{
						if nibkeep_std::names_content_only(&field) {
							made_of.insert(entity);
						} else {
							holders.insert(state.canonical(entity)?);
						}
						if seen.insert(id) {
							statements.push(id);
						}
					}
				}
			}
		}
		// A thumbnail goes whole, but its marks of a move or a merge, which lead to where it
		// went; and its own content, unless another value holds that.
		let mut theirs = BTreeSet::new();
		let mut pictures = BTreeSet::new();
		for thumbnail in made_of {
			thumbnails.insert(state.canonical(thumbnail)?);
			for (field, ids) in storage.of_entity(thumbnail)? {
				if super::behind::is_mark(&field) {
					continue;
				}
				for id in ids {
					theirs.insert(id);
					if seen.insert(id) {
						statements.push(id);
					}
					if field == CONTENT
						&& let Some(recorded) = storage.get(id)?
						&& let Value::Blob(picture) = recorded.statement.value
					{
						pictures.insert(picture);
					}
				}
			}
		}
		if !pictures.is_empty() {
			for entity in storage.described()? {
				for (field, ids) in storage.of_entity(entity)? {
					if secret_fields.contains(&field) {
						continue;
					}
					for id in ids {
						if !theirs.contains(&id)
							&& let Some(recorded) = storage.get(id)?
							&& let Value::Blob(held) = &recorded.statement.value
						{
							pictures.remove(held);
						}
					}
				}
			}
			contents.extend(pictures);
		}
		Ok(ContentsPurged {
			contents,
			holders,
			thumbnails,
		})
	}

	/// Purges an entity's values of a field for good, as [`Library::purgeable`] finds them:
	/// destroys, on every replica, the keys and payloads of the changes that asserted them,
	/// or the record keys of secret ones, and states again what else those changes held.
	/// What a purged value replaced stays replaced. The same change destroys the keys of the
	/// sealed copies of the content they refer to, and the copies of this device's checkouts
	/// of it are deleted where they are unchanged, as [`Library::settle_checkouts`] deletes
	/// them after a sync on other devices. Whoever may purge is decided with roles; the
	/// person confirms before the call.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::purgeable`], or an error if a value cannot be purged,
	/// such as a replica's key, or the purge cannot be saved.
	pub fn purge(
		&mut self,
		space: SpaceId,
		entity: EntityId,
		field: &FieldName,
		value: Option<&Value>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Purge, NodeError> {
		let mut purge = self.purgeable(space, entity, field, value)?;
		self.check_not_revoked(space)?;
		self.check_not_ai_agent("purge")?;
		let deleted = self.purge_statements(
			space,
			&purge.statements,
			(&purge.contents, purge.of_secrets),
			wall_millis,
			rng,
		)?;
		for copy in purge.checkouts.iter_mut().chain(&mut purge.tree_copies) {
			copy.deleted = deleted.contains(&copy.path);
		}
		Ok(purge)
	}

	/// Purges statements of a space, and the sealed copies of `contents` of the kind that
	/// `of_secrets` says, as [`Library::purge`] does once it found them, and returns the
	/// copies of this device's checkouts that it deleted.
	pub(crate) fn purge_statements(
		&mut self,
		space: SpaceId,
		statements: &[StatementId],
		(contents, of_secrets): (&[Hash], bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<PathBuf>, NodeError> {
		// What the checkouts held before, by which their hashes tell what the purge leaves
		// unchanged.
		let known = self.checkout_contents_in(space)?;
		let signing = &self.replica.signing;
		let code_types = &self.code_types;
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		open.stored.purge_with_blobs(
			signing,
			&open.keys,
			statements,
			(contents, of_secrets),
			wall_millis,
			rng,
		)?;
		// What was purged may have defined types, or held a list of languages.
		open.read_types(code_types)?;
		if space == self.info.personal {
			self.refresh_languages()?;
		}
		// The purge is done; a later settling ends what this one could not. Trees learn the
		// content first, so that no copy of it anywhere in them is committed again.
		let contents: BTreeSet<Hash> = contents.iter().copied().collect();
		let _ = self.note_purged(space, (&contents, of_secrets));
		Ok(self
			.settle_checkouts_in(space, &known, wall_millis, rng)
			.unwrap_or_default())
	}
}

/// A place where a file was found: its device, and its path there.
type Place = (EntityId, String);

/// Of the entities that hold the content a purge takes, `holders`, those that lose it too
/// besides the one purged, `purged`, and the thumbnails made of it, which a purge names
/// apart: files with the same content, and any other value that refers to it, but origins
/// and checkouts; and where files of that content were found, which a purge leaves.
fn also_and_originals<S: Statements>(
	state: &nibkeep_model::State<S>,
	(purged, holders): (EntityId, &BTreeSet<EntityId>),
	thumbnails: &BTreeSet<EntityId>,
) -> Result<(Vec<EntityId>, BTreeSet<Place>), NodeError>
where
	NodeError: From<S::Error>,
{
	let mut also = Vec::new();
	let mut originals = BTreeSet::new();
	for &holder in holders {
		let kinds = state.types(holder)?;
		if kinds.contains(&ORIGIN) {
			originals.extend(place_of(state, holder)?);
			continue;
		}
		if kinds.contains(&CHECKOUT) {
			continue;
		}
		if holder != purged && !thumbnails.contains(&holder) {
			also.push(holder);
		}
	}
	Ok((also, originals))
}

/// Where an origin of a state says a file was found: each device and path it names.
fn place_of<S: Statements>(
	state: &nibkeep_model::State<S>,
	origin: EntityId,
) -> Result<Vec<(EntityId, String)>, NodeError>
where
	NodeError: From<S::Error>,
{
	let mut places = Vec::new();
	for device in state.current(origin, &FOUND_ON)? {
		let Some(device) = device.value.as_reference() else {
			continue;
		};
		for path in state.current(origin, &PATH)? {
			if let Some(path) = path.value.as_text() {
				places.push((state.canonical(device)?, path.to_owned()));
			}
		}
	}
	Ok(places)
}
