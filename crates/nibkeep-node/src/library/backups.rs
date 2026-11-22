//! Backups: folders on drives and shares that a person's computers fill with the sealed
//! changes, keys and blobs of the spaces they choose, and take in from, without keys and
//! without a database ([technical design: backups]).
//!
//! [technical design: backups]: ../../../../docs/Technical-design.md#910-backups

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{AWAY, WAITS};
use nibkeep_log::BackupHead;
use nibkeep_model::storage::Statements;
use nibkeep_model::{
	FieldDef, FieldName, Name, Operation, Statement, TypeDef, TypeName, Value, ValueKind, fields,
};
use nibkeep_std::{CONTENT, FILE};
use nibkeep_sync::{
	Holder, Holding, MOST_PACK_BLOB, PACK_SIZE, PackItem, PackReader, item_id, pack_header,
	pack_item,
};

use super::Library;
use super::accounts::disk_id;
use super::holding::HeldElsewhere;
use super::planner::{Keeps, Moment, gone, moment, taken_of};
use super::policies::{Kept, answer_policy, default_policies, facts_of, first_name, own_choices};
use super::recovery::RecoveryFileNeed;
use super::sync::{MIN_FREE_BYTES, Syncing};
use crate::backup::{self, BackupFolder, InFileSystem, Record, Updating};
use crate::{NodeError, place};

/// The type of backups, in the personal space.
pub(crate) const BACKUP: TypeName = TypeName::from_static("org.nibkeep.backup");

/// A backup's folder, by its path from the folder that its drive or share is mounted at.
pub(crate) const BACKUP_FOLDER: FieldName = FieldName::from_static("org.nibkeep.backup-folder");

/// A space that a backup holds, by its ID as text: one value for each.
pub(crate) const BACKUP_SPACE: FieldName = FieldName::from_static("org.nibkeep.backup-space");

/// A policy whose files a backup keeps: one value for each, none for every policy's.
pub(crate) const BACKUP_POLICY: FieldName = FieldName::from_static("org.nibkeep.backup-policy");

/// The fields of backups, which only the engine's backup functions write.
const OWN_FIELDS: [FieldName; 3] = [BACKUP_FOLDER, BACKUP_SPACE, BACKUP_POLICY];

/// How old a blob file's last check is before an update reads it again: 4 weeks.
const CHECK_AGAIN: u64 = 28 * 24 * 60 * 60 * 1000;

/// How long a damaged copy found counts: 4 weeks, as on devices.
const DAMAGE_COUNTS_FOR: u64 = 28 * 24 * 60 * 60 * 1000;

/// How often an update that checks writes its record, so that a drive pulled out loses
/// little progress.
const SAVE_EVERY: Duration = Duration::from_secs(60);

/// A space's packs smaller than this are merged once there are more than
/// [`MERGE_ABOVE`] of them: 16 MiB.
const MERGE_BELOW: u64 = 16 * 1024 * 1024;

/// How many small packs a space keeps before they are merged.
const MERGE_ABOVE: usize = 16;

/// How long a look for backups waits for the drives and shares to answer: 10 seconds.
const FIND_WITHIN: Duration = Duration::from_secs(10);

/// Whether a field is one of backups', which only the backup functions write.
pub(crate) fn is_own_field(field: &FieldName) -> bool {
	OWN_FIELDS.contains(field)
}

/// The definition of the type of backups, which NibKeep defines in code.
pub(crate) fn types() -> [TypeDef; 1] {
	[TypeDef::new(BACKUP, 1)
		.with_field(fields::NAME, FieldDef::new(ValueKind::Name))
		.with_field(
			BACKUP_FOLDER,
			FieldDef::new(ValueKind::Text).last_writer_wins(),
		)
		.with_field(BACKUP_SPACE, FieldDef::new(ValueKind::Text).many())
		.with_field(BACKUP_POLICY, FieldDef::new(ValueKind::Reference).many())
		.with_field(AWAY, FieldDef::new(ValueKind::Boolean).last_writer_wins())
		.with_field(WAITS, FieldDef::new(ValueKind::Integer).last_writer_wins())]
}

/// One of the person's backups, as their personal space records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
	/// Its record's ID, which its folder names.
	pub id: EntityId,
	/// Its name.
	pub name: String,
	/// Its folder's path from the folder that its drive or share is mounted at.
	pub folder: String,
	/// The spaces it holds.
	pub spaces: Vec<SpaceId>,
	/// The policies whose files it keeps; none for every policy's.
	pub policies: Vec<EntityId>,
	/// Whether it is kept away from home.
	pub away: bool,
	/// How many days it waits before it destroys what a purge destroys, if its owner set it,
	/// 0 for none; without it, 30 ([`super::waiting::backup_wait_days`]).
	pub waits: Option<u64>,
}

/// What a new backup holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewBackup {
	/// Its name.
	pub name: String,
	/// The spaces it holds; none for every space of the library.
	pub spaces: Vec<SpaceId>,
	/// The policies whose files it keeps; none for every policy's.
	pub policies: Vec<EntityId>,
	/// Whether it is kept away from home.
	pub away: bool,
	/// How many days it waits before purging, 0 for none; without it, 30.
	pub waits: Option<u64>,
}

/// A backup just made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedBackup {
	/// The backup.
	pub backup: Backup,
	/// Whether its folder is on a network share, whose drive no system names, so that it
	/// counts as a disk of its own.
	pub on_share: bool,
}

/// What to change of a backup: each value given replaces what it had.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupEdit {
	/// A new name.
	pub name: Option<String>,
	/// The spaces it holds from now on.
	pub spaces: Option<Vec<SpaceId>>,
	/// The policies whose files it keeps from now on; none for every policy's.
	pub policies: Option<Vec<EntityId>>,
	/// Whether it is kept away from home.
	pub away: Option<bool>,
	/// How many days it waits before purging, 0 for none, or `Some(None)` for the default.
	pub waits: Option<Option<u64>>,
}

/// How many things went one way in an update of a backup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupMoved {
	/// Changes.
	pub changes: usize,
	/// Wrapped record keys and wrapped blob keys.
	pub keys: usize,
	/// Sealed blobs.
	pub blobs: usize,
	/// The sealed blobs' bytes.
	pub bytes: u64,
}

/// What an update of a backup did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupUpdate {
	/// The update's number.
	pub update: u64,
	/// Whether the backup was pulled out or lost its power during an update before.
	pub unclean: bool,
	/// What the replica took in from the backup.
	pub taken: BackupMoved,
	/// What it wrote to the backup.
	pub written: BackupMoved,
	/// How many items and files of purged content left the backup.
	pub purged: usize,
	/// How many purges wait on the backup before what they destroyed leaves it.
	pub waiting: usize,
	/// How many sealed blobs that the backup keeps did not fit, and their bytes.
	pub unfit: usize,
	/// The bytes of those blobs.
	pub unfit_bytes: u64,
	/// How many blob files it read in full and found intact, and their bytes.
	pub checked: usize,
	/// The bytes of those files.
	pub checked_bytes: u64,
	/// How many copies it found damaged.
	pub damaged: usize,
	/// How many of those it wrote again from the replica's copy.
	pub repaired: usize,
	/// How many packs and files it set aside as damaged.
	pub set_aside: usize,
	/// How many bytes of blob files are left to check, as the update stopped first.
	pub to_check: u64,
	/// How many sealed blobs the backup holds, and their bytes.
	pub held: usize,
	/// The bytes of those blobs.
	pub held_bytes: u64,
	/// The spaces of the backup that this device does not hold, which the update neither
	/// took in nor wrote, each with the devices of the person's that hold it: one that none
	/// holds gets no update any more
	/// ([technical design](../../../../docs/Technical-design.md#910-backups)).
	pub elsewhere: Vec<HeldElsewhere>,
}

/// What a backup's folder says of it, without an update: read from its record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupState {
	/// The number of its last update; 0 for none.
	pub update: u64,
	/// How many sealed blobs it holds.
	pub held: usize,
	/// How many bytes of its blob files are due to be read again: not read in full in the
	/// last 4 weeks.
	pub to_check: u64,
	/// How many copies checks found damaged in the 4 weeks before its last update.
	pub damaged: usize,
}

/// How a backup waits before purging, as an update applies purges: its record keeps when
/// the update that first brought each purge ran, and the backup's waits; a purge whose wait
/// is over, or that its owner ended, applies ([technical design: copies that wait]).
///
/// [technical design: copies that wait]: ../../../../docs/Technical-design.md#911-copies-that-wait
pub(crate) struct Waits<'a> {
	record: &'a mut Record,
	now: u64,
	/// The purges whose wait the owner ended, which apply now.
	ending: &'a BTreeSet<ChangeId>,
	/// The purges of which something stays for its wait, with the items and files it keeps,
	/// each once, by its ID.
	waiting: BTreeMap<ChangeId, BTreeSet<[u8; 32]>>,
}

impl<'a> Waits<'a> {
	pub(crate) fn new(record: &'a mut Record, now: u64, ending: &'a BTreeSet<ChangeId>) -> Self {
		Self {
			record,
			now,
			ending,
			waiting: BTreeMap::new(),
		}
	}

	/// Whether what `purge` destroyed of the item `item` leaves the backup now: its wait is
	/// over, or its owner ended it. A purge first seen now begins its wait; an item purged by
	/// a purge that cannot be told stays, as a copy that waits keeps rather than destroys.
	fn due(&mut self, purge: Option<&ChangeId>, item: [u8; 32]) -> bool {
		let Some(&purge) = purge else {
			return false;
		};
		if self.ending.contains(&purge) {
			return true;
		}
		let (seen, _) = *self.record.purges.entry(purge).or_insert((self.now, 0));
		let due =
			seen.saturating_add(super::waiting::wait_at(&self.record.waits, seen)) <= self.now;
		if !due {
			self.waiting.entry(purge).or_default().insert(item);
		}
		due
	}

	/// Keeps in the record the purges of which something stays, with how much, and those that
	/// this update could not tell of, as of a space that this computer does not hold.
	fn settle(self, told: impl Fn(&ChangeId) -> bool) {
		let waiting = self.waiting;
		self.record
			.purges
			.retain(|purge, _| waiting.contains_key(purge) || !told(purge));
		for (purge, (_, items)) in &mut self.record.purges {
			if let Some(kept) = waiting.get(purge) {
				*items = u64::try_from(kept.len()).unwrap_or(u64::MAX);
			}
		}
	}
}

/// What an entry of a pack is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum What {
	Change { id: ChangeId, complete: bool },
	RecordKey(StatementId),
	BlobKey(Hash),
	Blob(Hash),
}

/// An entry of a pack: what it is, its ID as reconciliation knows it (and a blob's by its
/// sealed hash), and where its encoding is.
#[derive(Clone, Debug)]
struct Entry {
	what: What,
	id: [u8; 32],
	start: usize,
	length: usize,
	/// A blob's size; 0 for other items.
	size: u64,
}

/// A pack of a backup, as read.
#[derive(Clone, Debug)]
struct PackFile {
	space: SpaceId,
	path: String,
	size: u64,
	update: u64,
	entries: Vec<Entry>,
}

/// Where a backup keeps a sealed blob.
#[derive(Clone, Debug)]
enum BlobAt {
	File(u64),
	Pack {
		path: String,
		start: usize,
		length: usize,
		space: SpaceId,
		size: u64,
	},
}

/// The packs and blob files of a backup.
struct Packs {
	files: Vec<PackFile>,
	blob_files: BTreeMap<Hash, u64>,
	/// Where the packs hold each sealed blob, as [`Packs::reindex`] found it.
	blobs_at: BTreeMap<Hash, BlobAt>,
}

/// What a pack's bytes are.
enum Indexed {
	/// A sound pack.
	Sound(PackFile),
	/// Not the bytes that its name says: damaged.
	Damaged,
	/// The bytes its name says, which this engine does not read, as a pack of a later
	/// version's: left as they are.
	Unread,
}

/// The ID of a sealed blob's entry in a pack.
fn blob_id(sealed: &Hash) -> [u8; 32] {
	let digest = Hash::of(&cbor::encode(&Cbor::Array(vec![
		3_u8.to_cbor(),
		sealed.to_cbor(),
	])));
	digest
		.digest()
		.try_into()
		.unwrap_or_else(|_| unreachable!("BLAKE3 digests are 32 bytes"))
}

/// What a pack's bytes at `path`, in the folder of `space`, are.
fn index(space: SpaceId, path: &str, bytes: &[u8]) -> Indexed {
	if Updating::pack_path(space, &Hash::of(bytes)) != path {
		return Indexed::Damaged;
	}
	read_entries(space, path, bytes).map_or(Indexed::Unread, Indexed::Sound)
}

/// The entries of a pack of `space`, if it reads as one.
fn read_entries(space: SpaceId, path: &str, bytes: &[u8]) -> Option<PackFile> {
	let mut reader = PackReader::new(bytes).ok()?;
	if reader.space() != space {
		return None;
	}
	let mut entries = Vec::new();
	while let Some((item, start, length)) = reader.next_item().ok()? {
		let size = match &item {
			PackItem::Blob(_, bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
			_ => 0,
		};
		let (what, id) = match item {
			PackItem::Change(parcel) => {
				let complete = parcel.payload.is_some() && !parcel.keys.is_empty();
				let id = parcel.id();
				let holding = Holding::Change {
					id,
					millis: 0,
					depth: 0,
					complete,
				};
				(What::Change { id, complete }, item_id(&holding))
			}
			PackItem::RecordKey(statement, key) => (
				What::RecordKey(statement),
				item_id(&Holding::RecordKey { statement, key }),
			),
			PackItem::BlobKey(sealed, key) => (
				What::BlobKey(sealed),
				item_id(&Holding::BlobKey { sealed, key }),
			),
			PackItem::Blob(sealed, _) => (What::Blob(sealed), blob_id(&sealed)),
		};
		entries.push(Entry {
			what,
			id,
			start,
			length,
			size,
		});
	}
	Some(PackFile {
		space,
		path: path.to_owned(),
		size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
		update: reader.update(),
		entries,
	})
}

impl Packs {
	/// Reads every pack of the backup, and sets aside those that are damaged.
	fn read(updating: &Updating<'_>, done: &mut BackupUpdate) -> Result<Self, NodeError> {
		let mut files = Vec::new();
		for (space, path, _) in updating.packs()? {
			// A pack that cannot be read now is tried again at the next update.
			let Ok(bytes) = updating.read(&path) else {
				continue;
			};
			match index(space, &path, &bytes) {
				Indexed::Sound(file) => files.push(file),
				Indexed::Damaged => {
					updating.set_aside(&path);
					done.set_aside += 1;
				}
				Indexed::Unread => {}
			}
		}
		let mut packs = Self {
			files,
			blob_files: updating.blob_files()?,
			blobs_at: BTreeMap::new(),
		};
		packs.reindex();
		Ok(packs)
	}

	/// Orders the packs by the update that wrote them, and notes where they hold each sealed
	/// blob, after packs came or went.
	fn reindex(&mut self) {
		self.files
			.sort_by(|a, b| (a.update, &a.path).cmp(&(b.update, &b.path)));
		self.blobs_at.clear();
		for file in &self.files {
			for entry in &file.entries {
				if let What::Blob(sealed) = entry.what {
					self.blobs_at.entry(sealed).or_insert_with(|| BlobAt::Pack {
						path: file.path.clone(),
						start: entry.start,
						length: entry.length,
						space: file.space,
						size: entry.size,
					});
				}
			}
		}
	}

	fn of_space(&self, space: SpaceId) -> impl Iterator<Item = &PackFile> {
		self.files.iter().filter(move |file| file.space == space)
	}

	/// The IDs of the items that the packs of a space hold.
	fn ids(&self, space: SpaceId) -> BTreeSet<[u8; 32]> {
		self.of_space(space)
			.flat_map(|file| file.entries.iter().map(|entry| entry.id))
			.collect()
	}

	/// The changes that the packs of a space hold, each with whether one holds it with its
	/// payload and a key.
	fn changes(&self, space: SpaceId) -> BTreeMap<ChangeId, bool> {
		let mut changes = BTreeMap::new();
		for file in self.of_space(space) {
			for entry in &file.entries {
				if let What::Change { id, complete } = entry.what {
					*changes.entry(id).or_insert(false) |= complete;
				}
			}
		}
		changes
	}

	/// Where the backup keeps a sealed blob, if it holds it.
	fn blob(&self, sealed: &Hash) -> Option<BlobAt> {
		if let Some(size) = self.blob_files.get(sealed) {
			return Some(BlobAt::File(*size));
		}
		self.blobs_at.get(sealed).cloned()
	}

	/// The sealed blobs that the backup holds, with their sizes: in files of their own, and
	/// in packs.
	fn blobs(&self) -> BTreeMap<Hash, u64> {
		let mut blobs = self.blob_files.clone();
		for (sealed, at) in &self.blobs_at {
			if let BlobAt::Pack { size, .. } = at {
				blobs.entry(*sealed).or_insert(*size);
			}
		}
		blobs
	}

	/// The sealed blobs that the backup holds in packs.
	fn blobs_in_packs(&self) -> BTreeSet<Hash> {
		self.blobs_at.keys().copied().collect()
	}

	/// For each sealed blob, the spaces whose packs hold a key for it.
	fn keyed(&self) -> BTreeMap<Hash, BTreeSet<SpaceId>> {
		let mut keyed: BTreeMap<Hash, BTreeSet<SpaceId>> = BTreeMap::new();
		for file in &self.files {
			for entry in &file.entries {
				if let What::BlobKey(sealed) = entry.what {
					keyed.entry(sealed).or_default().insert(file.space);
				}
			}
		}
		keyed
	}
}

/// A pack being written: its bytes, written whole as it fills, and the packs it wrote.
struct PackWriter {
	space: SpaceId,
	update: u64,
	buffer: Vec<u8>,
	items: usize,
	written: Vec<PackFile>,
	/// The bytes of the packs it wrote.
	flushed: u64,
}

impl PackWriter {
	fn new(space: SpaceId, update: u64) -> Self {
		Self {
			space,
			update,
			buffer: pack_header(space, update),
			items: 0,
			written: Vec::new(),
			flushed: 0,
		}
	}

	/// Adds an item, from its encoding.
	fn push_encoded(&mut self, updating: &Updating<'_>, encoded: &[u8]) -> Result<(), NodeError> {
		if self.items > 0 && self.buffer.len() + encoded.len() > PACK_SIZE {
			self.flush(updating)?;
		}
		self.buffer.extend_from_slice(encoded);
		self.items += 1;
		Ok(())
	}

	fn push(&mut self, updating: &Updating<'_>, item: &PackItem) -> Result<(), NodeError> {
		self.push_encoded(updating, &pack_item(self.space, item))
	}

	/// The bytes that it holds, not written yet, and that it wrote.
	fn used(&self) -> u64 {
		u64::try_from(self.buffer.len())
			.unwrap_or(u64::MAX)
			.saturating_add(self.flushed)
	}

	/// Writes what it holds as a pack, if it holds any item.
	fn flush(&mut self, updating: &Updating<'_>) -> Result<(), NodeError> {
		if self.items == 0 {
			return Ok(());
		}
		let path = updating.write_pack(self.space, &self.buffer)?;
		let file = read_entries(self.space, &path, &self.buffer).ok_or_else(|| {
			NodeError::Damaged("a pack written to a backup does not read".to_owned())
		})?;
		self.flushed = self.flushed.saturating_add(file.size);
		self.written.push(file);
		self.buffer = pack_header(self.space, self.update);
		self.items = 0;
		Ok(())
	}

	/// Writes what is left, and returns the packs it wrote.
	fn finish(mut self, updating: &Updating<'_>) -> Result<Vec<PackFile>, NodeError> {
		self.flush(updating)?;
		Ok(self.written)
	}
}

/// Where a sealed blob goes in the order in which an update writes blobs.
type Rank = (
	u8,
	std::cmp::Reverse<u32>,
	std::cmp::Reverse<Option<Moment>>,
);

/// What a backup keeps of a space's sealed blobs, and in which order an update writes them.
struct BackupPlan {
	/// Whether it keeps no sealed blob of the space, as of the personal space where only a
	/// recovery code brings it.
	nothing: bool,
	keeps: Keeps,
	/// For each content that a file holds or held: the most copies that the policies of
	/// its files ask for, and the newest moment of them.
	order: BTreeMap<Hash, (u32, Option<Moment>)>,
}

impl BackupPlan {
	/// Where a sealed blob goes in the order of writing, if the backup keeps it: those that
	/// every replica keeps first, then the files' of the policies that ask for the most
	/// copies, then the newest.
	fn rank(&self, secret: bool, content: Option<Hash>) -> Option<Rank> {
		if self.nothing {
			return None;
		}
		if secret {
			return Some((0, std::cmp::Reverse(0), std::cmp::Reverse(None)));
		}
		let Some(content) = content else {
			return Some((0, std::cmp::Reverse(0), std::cmp::Reverse(None)));
		};
		if !self.keeps.keeps(&content) {
			return None;
		}
		match self.order.get(&content) {
			None => Some((0, std::cmp::Reverse(0), std::cmp::Reverse(None))),
			Some((copies, newest)) => {
				Some((1, std::cmp::Reverse(*copies), std::cmp::Reverse(*newest)))
			}
		}
	}
}

impl Library {
	/// The person's backups, as their personal space records them, by name.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn backups(&self) -> Result<Vec<Backup>, NodeError> {
		let open = self.space(self.info.personal)?;
		let state = open.stored.log().state();
		let mut found = Vec::new();
		let mut seen = BTreeSet::new();
		for entity in open
			.stored
			.find(&fields::TYPE, &Value::Text(BACKUP.to_string()))?
		{
			let entity = state.canonical(entity)?;
			if !seen.insert(entity)
				|| state.is_deleted(entity)?
				|| !state.types(entity)?.contains(&BACKUP)
			{
				continue;
			}
			let mut spaces: Vec<SpaceId> = state
				.current(entity, &BACKUP_SPACE)?
				.iter()
				.filter_map(|statement| statement.value.as_text()?.parse().ok())
				.collect();
			spaces.sort();
			spaces.dedup();
			let mut policies = Vec::new();
			for statement in state.current(entity, &BACKUP_POLICY)? {
				if let Some(policy) = statement.value.as_reference() {
					policies.push(policy);
				}
			}
			policies.sort();
			policies.dedup();
			found.push(Backup {
				id: entity,
				name: first_name(state, entity)?.unwrap_or_default(),
				folder: state
					.last_written(entity, &BACKUP_FOLDER)?
					.and_then(|statement| statement.value.as_text().map(str::to_owned))
					.unwrap_or_default(),
				spaces,
				policies,
				away: state
					.last_written(entity, &AWAY)?
					.is_some_and(|statement| statement.value == Value::Boolean(true)),
				waits: state.last_written(entity, &WAITS)?.and_then(|statement| {
					match statement.value {
						Value::Integer(days) => u64::try_from(days).ok(),
						_ => None,
					}
				}),
			});
		}
		found.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
		Ok(found)
	}

	/// Whether the person removed a backup: its record, in the personal space, is deleted.
	/// Another member's backup, of which the personal space knows nothing, is not.
	pub(crate) fn backup_forgotten(&self, backup: EntityId) -> Result<bool, NodeError> {
		let state = self.space(self.info.personal)?.stored.log().state();
		let backup = state.canonical(backup)?;
		Ok(state.is_deleted(backup)?)
	}

	/// The backup that `reference` names, by its ID or its name.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichBackup`] if it names none, or several.
	pub fn find_backup(&self, reference: &str) -> Result<Backup, NodeError> {
		let backups = self.backups()?;
		if let Ok(id) = reference.parse::<EntityId>()
			&& let Some(backup) = backups.iter().find(|backup| backup.id == id)
		{
			return Ok(backup.clone());
		}
		let named: Vec<&Backup> = backups
			.iter()
			.filter(|backup| backup.name == reference)
			.collect();
		let named = if named.is_empty() {
			backups
				.iter()
				.filter(|backup| backup.name.to_lowercase() == reference.to_lowercase())
				.collect()
		} else {
			named
		};
		match named.as_slice() {
			[backup] => Ok((*backup).clone()),
			[] => Err(NodeError::WhichBackup(format!(
				"no backup of yours is named {reference}"
			))),
			_ => Err(NodeError::WhichBackup(format!(
				"several backups of yours are named {reference}; name one by its ID"
			))),
		}
	}

	/// Makes a backup in `folder`, which must be new or empty, and records it in the personal
	/// space ([technical design: backups]).
	///
	/// [technical design: backups]: ../../../../docs/Technical-design.md#910-backups
	///
	/// # Errors
	///
	/// Returns [`NodeError::EmptyName`] for an empty name, [`NodeError::NoSpace`] for a
	/// space that this replica does not hold, [`NodeError::NoEntity`] for a policy of none
	/// of them, [`NodeError::BackupFolder`] for a folder that is not empty, lies in a
	/// library or in a cloud drive's folder, and an error if the folder or the change cannot
	/// be written.
	pub fn add_backup(
		&mut self,
		folder: &Path,
		new: &NewBackup,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedBackup, NodeError> {
		let name = new.name.trim();
		if name.is_empty() {
			return Err(NodeError::EmptyName);
		}
		let spaces = self.chosen_spaces(&new.spaces)?;
		self.check_backup_policies(&spaces, &new.policies)?;
		let folder = std::path::absolute(folder).map_err(NodeError::file(folder))?;
		let existing = folder
			.ancestors()
			.find(|ancestor| ancestor.exists())
			.unwrap_or(&folder);
		// Compared as the file system resolves them, links and all.
		let resolved = fs::canonicalize(existing)
			.map_err(NodeError::file(existing))?
			.join(folder.strip_prefix(existing).unwrap_or(Path::new("")));
		// The library's directory holds `replicas/`, which holds this replica's folder.
		let root = self.directory.ancestors().nth(2).unwrap_or(&self.directory);
		let library = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
		if resolved.components().any(|part| part.as_os_str() == ".nib")
			|| resolved.starts_with(&library)
		{
			return Err(NodeError::BackupFolder(format!(
				"{} is in a library; a backup is a folder of its own",
				folder.display()
			)));
		}
		if place::whereabouts(existing).drive.cloud {
			return Err(NodeError::BackupFolder(format!(
				"{} is in a cloud drive's folder, which may keep its files in the cloud; backups do not support such folders yet",
				folder.display()
			)));
		}
		let made_new = !folder.exists();
		let added = self.record_new_backup(
			&InFileSystem::new(&folder),
			(name, spaces),
			new,
			wall_millis,
			rng,
		);
		if added.is_err() && made_new {
			let _ = fs::remove_dir(&folder);
		}
		added
	}

	/// Makes a backup as [`Library::add_backup`] does, in a folder reached through the
	/// interface of files, as the folder that Android's picker gives: it checks only that the
	/// folder is empty, and records it by the path that its drive gives.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::add_backup`] but those of where the folder is.
	pub fn add_backup_in(
		&mut self,
		folder: &dyn BackupFolder,
		new: &NewBackup,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedBackup, NodeError> {
		let name = new.name.trim();
		if name.is_empty() {
			return Err(NodeError::EmptyName);
		}
		let spaces = self.chosen_spaces(&new.spaces)?;
		self.check_backup_policies(&spaces, &new.policies)?;
		self.record_new_backup(folder, (name, spaces), new, wall_millis, rng)
	}

	/// Makes a backup's folder and records the backup, once its name, spaces and policies
	/// are checked.
	fn record_new_backup(
		&mut self,
		folder: &dyn BackupFolder,
		(name, spaces): (&str, Vec<SpaceId>),
		new: &NewBackup,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<AddedBackup, NodeError> {
		let id = EntityId::random(rng);
		backup::make(folder, id)?;
		let drive = folder.drive();
		let path = drive.path.clone().unwrap_or_default();
		let backup = Backup {
			id,
			name: name.to_owned(),
			folder: path,
			spaces,
			policies: new.policies.clone(),
			away: new.away,
			waits: new.waits,
		};
		let mut operations = vec![
			assert(id, fields::TYPE, Value::Text(BACKUP.to_string()), rng),
			assert(id, fields::NAME, Value::Name(Name::new(name)), rng),
			assert(id, BACKUP_FOLDER, Value::Text(backup.folder.clone()), rng),
		];
		for space in &backup.spaces {
			operations.push(assert(
				id,
				BACKUP_SPACE,
				Value::Text(space.to_string()),
				rng,
			));
		}
		for policy in &backup.policies {
			operations.push(assert(id, BACKUP_POLICY, Value::Reference(*policy), rng));
		}
		if backup.away {
			operations.push(assert(id, AWAY, Value::Boolean(true), rng));
		}
		if let Some(days) = backup.waits {
			let days = Value::Integer(i64::try_from(days).unwrap_or(i64::MAX));
			operations.push(assert(id, WAITS, days, rng));
		}
		if let Err(error) = self.commit(self.info.personal, operations, wall_millis, rng) {
			backup::unmake(folder);
			return Err(error);
		}
		Ok(AddedBackup {
			backup,
			on_share: drive.remote,
		})
	}

	/// Changes one of the person's backups.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichBackup`] if `backup` is none of the person's backups, the
	/// errors of [`Library::add_backup`] for what is given, and an error if the change cannot
	/// be saved.
	pub fn edit_backup(
		&mut self,
		backup: EntityId,
		edit: &BackupEdit,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(before) = self.backups()?.into_iter().find(|known| known.id == backup) else {
			return Err(NodeError::WhichBackup(format!(
				"{backup} is no backup of yours"
			)));
		};
		let personal = self.info.personal;
		let mut values = Vec::new();
		if let Some(name) = &edit.name {
			if name.trim().is_empty() {
				return Err(NodeError::EmptyName);
			}
			if name.trim() != before.name {
				values.push((fields::NAME, Value::Name(Name::new(name.trim()))));
			}
		}
		if let Some(away) = edit.away
			&& away != before.away
		{
			values.push((AWAY, Value::Boolean(away)));
		}
		let mut operations = self.replacing(personal, backup, values, rng)?;
		// The wait, or the default without one: its values go.
		if let Some(waits) = edit.waits
			&& waits != before.waits
		{
			let days = waits.map(|days| Value::Integer(i64::try_from(days).unwrap_or(i64::MAX)));
			operations.extend(self.replacing_or_clearing(personal, backup, WAITS, days, rng)?);
		}
		let spaces = match &edit.spaces {
			Some(spaces) => self.chosen_spaces(spaces)?,
			None => before.spaces.clone(),
		};
		if let Some(policies) = &edit.policies {
			self.check_backup_policies(&spaces, policies)?;
		}
		// Spaces taken on keep the files of the policies that the backup keeps, by their
		// names there, as `policies` gives none.
		let policies = match &edit.policies {
			None if edit.spaces.is_some() && !before.policies.is_empty() => {
				Some(self.policies_in(&before.policies, &before.spaces, &spaces)?)
			}
			given => given.clone(),
		};
		let state = self.space(personal)?.stored.log().state();
		// Values of many, each added or retracted as the new list says.
		let mut many = |field: FieldName, values: Vec<Value>| -> Result<(), NodeError> {
			let current = state.current(backup, &field)?;
			let retracted: Vec<StatementId> = current
				.iter()
				.filter(|statement| !values.contains(&statement.value))
				.map(|statement| statement.id)
				.collect();
			if !retracted.is_empty() {
				operations.push(Operation::retract(backup, retracted));
			}
			for value in values {
				if !current.iter().any(|statement| statement.value == value) {
					operations.push(assert(backup, field.clone(), value, rng));
				}
			}
			Ok(())
		};
		if edit.spaces.is_some() {
			many(
				BACKUP_SPACE,
				spaces
					.iter()
					.map(|space| Value::Text(space.to_string()))
					.collect(),
			)?;
		}
		if let Some(policies) = &policies {
			many(
				BACKUP_POLICY,
				policies
					.iter()
					.map(|policy| Value::Reference(*policy))
					.collect(),
			)?;
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(personal, operations, wall_millis, rng)
	}

	/// Forgets one of the person's backups: its record is deleted, and its folder stays for
	/// the person to delete.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichBackup`] if `backup` is none of the person's backups, and an
	/// error if the change cannot be saved.
	pub fn remove_backup(
		&mut self,
		backup: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if !self.backups()?.iter().any(|known| known.id == backup) {
			return Err(NodeError::WhichBackup(format!(
				"{backup} is no backup of yours"
			)));
		}
		let operations = vec![assert(backup, fields::DELETED, Value::Boolean(true), rng)];
		self.commit(self.info.personal, operations, wall_millis, rng)
	}

	/// The person's backups that hold a space of this replica's and are reachable from this
	/// computer, each with its folder: at its path under each folder that a drive or share is
	/// mounted at, and where `remembered` says this computer found it last.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn find_backups(
		&self,
		remembered: &BTreeMap<EntityId, PathBuf>,
	) -> Result<Vec<(Backup, PathBuf)>, NodeError> {
		let mounts = backup::mounts();
		let mut backups = Vec::new();
		let mut candidates = Vec::new();
		for backup in self.backups()? {
			if self.backup_spaces(&backup)?.is_empty() {
				continue;
			}
			if let Some(folder) = remembered.get(&backup.id) {
				candidates.push((folder.clone(), folder.clone(), backup.id));
			}
			if !backup.folder.is_empty() {
				let path = backup.folder.replace('/', std::path::MAIN_SEPARATOR_STR);
				candidates.extend(
					mounts
						.iter()
						.map(|mount| (mount.clone(), mount.join(&path), backup.id)),
				);
			}
			backups.push(backup);
		}
		let named = backup::find_named(candidates, FIND_WITHIN);
		Ok(backups
			.into_iter()
			.filter_map(|backup| {
				let folder = named
					.iter()
					.find(|(id, _)| *id == backup.id)
					.map(|(_, folder)| folder.clone())?;
				Some((backup, folder))
			})
			.collect())
	}

	/// What a backup's folder says of it, as of `wall_millis`, without updating it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder is not the backup, and an error if it
	/// cannot be read.
	pub fn backup_state(
		&self,
		backup: &Backup,
		folder: &Path,
		wall_millis: u64,
	) -> Result<BackupState, NodeError> {
		self.backup_state_in(backup, &InFileSystem::new(folder), wall_millis)
	}

	/// What a backup's folder, reached through the interface of files, says of it, as
	/// [`Library::backup_state`] does.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder is not the backup, and an error if it
	/// cannot be read.
	pub fn backup_state_in(
		&self,
		backup: &Backup,
		folder: &dyn BackupFolder,
		wall_millis: u64,
	) -> Result<BackupState, NodeError> {
		if backup::named_in(folder) != Some(backup.id) {
			return Err(NodeError::NotTheBackup {
				path: folder.shown(""),
				name: backup.name.clone(),
			});
		}
		let record = backup::read_record(folder);
		let to_check = backup::blob_files_in(folder)?
			.into_iter()
			.filter(|(sealed, _)| {
				record
					.checks
					.get(sealed)
					.is_none_or(|checked| checked.saturating_add(CHECK_AGAIN) <= wall_millis)
			})
			.map(|(_, size)| size)
			.sum();
		Ok(BackupState {
			update: record.update,
			held: record.checks.len(),
			to_check,
			damaged: record.damaged.len(),
		})
	}

	/// Updates a backup in `folder` ([technical design: backups]): takes in what the replica
	/// lacks, applies its purges, writes what the backup lacks, and checks the backup's
	/// copies, until `stop` says to stop. `None` while another program updates it.
	///
	/// [technical design: backups]: ../../../../docs/Technical-design.md#910-backups
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder is not the backup, and an error if
	/// the library or the backup cannot be read or written.
	pub fn update_backup(
		&mut self,
		backup: &Backup,
		folder: &Path,
		now: &dyn Fn() -> u64,
		stop: &dyn Fn() -> bool,
	) -> Result<Option<BackupUpdate>, NodeError> {
		self.update_backup_in(backup, &InFileSystem::new(folder), now, stop)
	}

	/// Updates a backup as [`Library::update_backup`] does, in a folder reached through the
	/// interface of files, as the folder that Android's picker gives.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::update_backup`].
	pub fn update_backup_in(
		&mut self,
		backup: &Backup,
		folder: &dyn BackupFolder,
		now: &dyn Fn() -> u64,
		stop: &dyn Fn() -> bool,
	) -> Result<Option<BackupUpdate>, NodeError> {
		self.update_backup_through(backup, folder, (now, stop), &BTreeSet::new())
	}

	/// Updates a backup as [`Library::update_backup`] does, ending the wait of the purges
	/// `ending`, whose destruction then leaves the backup at once.
	pub(crate) fn update_backup_ending(
		&mut self,
		backup: &Backup,
		folder: &Path,
		(now, stop): (&dyn Fn() -> u64, &dyn Fn() -> bool),
		ending: &BTreeSet<ChangeId>,
	) -> Result<Option<BackupUpdate>, NodeError> {
		self.update_backup_through(backup, &InFileSystem::new(folder), (now, stop), ending)
	}

	/// Updates a backup in a folder reached through the interface of files, ending the wait
	/// of the purges `ending`.
	fn update_backup_through(
		&mut self,
		backup: &Backup,
		folder: &dyn BackupFolder,
		(now, stop): (&dyn Fn() -> u64, &dyn Fn() -> bool),
		ending: &BTreeSet<ChangeId>,
	) -> Result<Option<BackupUpdate>, NodeError> {
		let Some(updating) = Updating::hold(folder, backup.id, &backup.name)? else {
			return Ok(None);
		};
		let mut record = updating.record();
		let mut done = BackupUpdate {
			unclean: updating.unclean,
			..BackupUpdate::default()
		};
		// A drive pulled out while written, on a file system that then may damage other
		// files: every copy counts again only once read back.
		if updating.unclean && updating.unjournaled {
			for checked in record.checks.values_mut() {
				*checked = 0;
			}
		}
		let mut spaces = self.backup_spaces(backup)?;
		let mut packs = Packs::read(&updating, &mut done)?;
		// Past every update that wrote a pack, and that the backup's accounts known here tell
		// of, too, so that a record lost to a pulled drive does not start the numbers again,
		// by which devices tell the backup's latest account.
		let accounted = spaces
			.iter()
			.filter_map(|space| {
				let accounts = self.backup_accounts(*space).ok()?;
				accounts.get(&backup.id).map(|account| account.head.update)
			})
			.max()
			.unwrap_or(0);
		let update = packs
			.files
			.iter()
			.map(|file| file.update)
			.fold(record.update.max(accounted), u64::max)
			.saturating_add(1);
		// A backup that holds the personal space keeps it, also where this computer has not
		// heard yet of the code that put it there: so that it takes the code in, and leaves
		// it in the backup's recovery file.
		let personal = self.info.personal;
		if !spaces.contains(&personal) && packs.of_space(personal).next().is_some() {
			spaces.push(personal);
			spaces.sort();
		}
		let read_at = now();
		for sealed in packs.blobs_in_packs() {
			record.checks.insert(sealed, read_at);
		}
		self.take_in_backup(&updating, &packs, &spaces, now, &mut done)?;
		// The backup's wait, as its owner marks it: a shorter one holds only once the wait
		// before it has passed.
		let marked = super::waiting::backup_wait_days(backup).saturating_mul(super::waiting::DAY);
		if let Some(waits) = super::waiting::waits_marked(&record.waits, marked, read_at) {
			record.waits = waits;
		}
		let mut waits = Waits::new(&mut record, read_at, ending);
		self.purge_backup(
			&updating, &mut packs, &spaces, update, &mut waits, &mut done,
		)?;
		done.waiting = waits.waiting.len();
		// A purge of a space that this computer does not hold stays as the record has it.
		let told = |purge: &ChangeId| {
			spaces.iter().any(|space| {
				self.spaces
					.get(space)
					.is_some_and(|open| open.stored.log().contains(*purge).unwrap_or(true))
			})
		};
		waits.settle(told);
		let wall = now();
		// The personal space, which a backup holds for the person's recovery codes whatever
		// spaces it names, gives its files only where it is named.
		let keys_only = (!backup.spaces.contains(&personal)).then_some(personal);
		self.write_backup(
			&updating,
			&mut packs,
			(&spaces, &backup.policies, keys_only),
			(update, wall),
			&mut record,
			&mut done,
		)?;
		self.write_backup_recovery(&updating)?;
		merge(&updating, &mut packs, &spaces, update)?;
		self.check_backup(&updating, &mut record, update, (now, stop), &mut done)?;
		// What the record knows, of what the backup holds now.
		let held = packs.blobs();
		record.checks.retain(|sealed, _| held.contains_key(sealed));
		for sealed in held.keys() {
			record.checks.entry(*sealed).or_insert(0);
		}
		let wall = now();
		record
			.damaged
			.retain(|_, found| wall.saturating_sub(*found) < DAMAGE_COUNTS_FOR);
		record.update = update;
		updating.save_record(&record)?;
		self.account_backup(backup, (folder, &packs, &record), &spaces, wall);
		// The codes that a newer one replaces, if the backup was the last to carry it: the
		// update is done, and the next sync tries again if this fails.
		let _ = self.revoke_replaced_recovery_codes(false, wall, &mut UnwrapErr(SysRng));
		done.update = update;
		done.held = held.len();
		done.held_bytes = held.values().sum();
		// What it names only, after the update is done.
		done.elsewhere = self
			.spaces_held_elsewhere(&backup.spaces)
			.unwrap_or_default();
		Ok(Some(done))
	}

	/// Takes in what the backup in `folder`, whose record is `backup`, holds of `spaces`, as
	/// the first two steps of an update do, holding the backup meanwhile: for a library that a
	/// recovery code brings back, which does not read the backup's record yet. `None` while
	/// another program updates the backup.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder is not the backup, and an error if
	/// the backup cannot be read, or what it holds saved.
	pub(crate) fn take_in_backup_at(
		&mut self,
		(folder, backup): (&Path, EntityId),
		spaces: &[SpaceId],
		now: &dyn Fn() -> u64,
	) -> Result<Option<BackupMoved>, NodeError> {
		let name = folder
			.file_name()
			.map_or_else(String::new, |name| name.to_string_lossy().into_owned());
		let files = InFileSystem::new(folder);
		let Some(updating) = Updating::hold(&files, backup, &name)? else {
			return Ok(None);
		};
		let mut done = BackupUpdate::default();
		let packs = Packs::read(&updating, &mut done)?;
		self.take_in_backup(&updating, &packs, spaces, now, &mut done)?;
		Ok(Some(done.taken))
	}

	/// What a backup keeps of what a purge of `space` destroyed, as its packs hold it while
	/// it waits: the changes with their payloads, the record keys and the blob keys that the
	/// purge keeps purged. `None` while another program updates the backup.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder is not the backup, and an error if
	/// the backup or the space cannot be read.
	pub(crate) fn backup_kept_of(
		&self,
		backup: &Backup,
		folder: &Path,
		(space, purge): (SpaceId, ChangeId),
	) -> Result<Option<nibkeep_log::Restores>, NodeError> {
		let files = InFileSystem::new(folder);
		let Some(updating) = Updating::hold(&files, backup.id, &backup.name)? else {
			return Ok(None);
		};
		let packs = Packs::read(&updating, &mut BackupUpdate::default())?;
		let by = self.space(space)?.stored.log().purged_by()?;
		let mut restores = nibkeep_log::Restores::default();
		for file in packs.of_space(space) {
			for entry in &file.entries {
				match &entry.what {
					What::Change { id, complete: true } if by.changes.get(id) == Some(&purge) => {
						restores.changes.insert(*id);
					}
					What::RecordKey(statement) if by.statements.get(statement) == Some(&purge) => {
						restores.statements.insert(*statement);
					}
					What::BlobKey(sealed) if by.blobs.get(sealed) == Some(&purge) => {
						restores.blobs.insert(*sealed);
					}
					_ => {}
				}
			}
		}
		Ok(Some(restores))
	}

	/// Issues the account of a backup just updated in each of its spaces where this replica
	/// counts: its disk, its mark, the space's changes it holds, which are this replica's,
	/// what checks found damaged, the update's number, and the sealed blobs that its packs
	/// of the space hold keys for, with when each was last checked. Accounts only count and
	/// show copies, so one that cannot be issued now waits for the next update.
	fn account_backup(
		&mut self,
		backup: &Backup,
		(folder, packs, record): (&dyn BackupFolder, &Packs, &Record),
		spaces: &[SpaceId],
		wall_millis: u64,
	) {
		let own = self.replica.signing.verifying_key();
		let name = backup::disk_name(folder, backup.id);
		let held = packs.blobs();
		let keyed = packs.keyed();
		let mut rng = UnwrapErr(SysRng);
		for &space in spaces {
			if !self.counts_in(space, &own).unwrap_or(false) {
				continue;
			}
			let Ok(heads) = self
				.space(space)
				.and_then(|open| Ok(open.stored.log().heads()?))
			else {
				continue;
			};
			// What checks found damaged of this space's blobs alone, as a replica counts its
			// own, so that no space learns of another's.
			let damaged: Vec<&Hash> = record
				.damaged
				.keys()
				.filter(|sealed| {
					keyed
						.get(*sealed)
						.is_some_and(|keyed| keyed.contains(&space))
				})
				.collect();
			let blobs: BTreeMap<Hash, u64> = held
				.keys()
				.filter(|sealed| {
					keyed
						.get(*sealed)
						.is_some_and(|keyed| keyed.contains(&space))
				})
				.map(|sealed| (*sealed, record.checks.get(sealed).copied().unwrap_or(0)))
				.collect();
			let account = BackupHead {
				disk: disk_id(space, Some(&name), &own),
				away: backup.away,
				heads: heads.into_iter().collect(),
				damaged: u64::try_from(damaged.len()).unwrap_or(u64::MAX),
				unrepaired: u64::try_from(
					damaged
						.iter()
						.filter(|sealed| !held.contains_key(**sealed))
						.count(),
				)
				.unwrap_or(u64::MAX),
				update: record.update,
				waits: super::waiting::wait_at(&record.waits, wall_millis)
					.div_ceil(super::waiting::DAY),
			};
			let _ = self.issue_backup_account(
				space,
				backup.id,
				(account, &blobs),
				wall_millis,
				&mut rng,
			);
		}
	}

	/// Keeps the backup's recovery file as the person's codes call for, as
	/// [`Library::recovery_file_anew`] says: written anew only where the codes that stand
	/// changed, or where it does not read, as on a damaged drive, so that the computers that
	/// update a backup in turn leave it as it is.
	fn write_backup_recovery(&self, updating: &Updating<'_>) -> Result<(), NodeError> {
		// One that does not read is written anew.
		let existing = updating.recovery();
		match self.recovery_file_anew(existing.as_ref())? {
			RecoveryFileNeed::Kept => Ok(()),
			RecoveryFileNeed::Written(file) => updating.save_recovery(&file.to_bytes()),
			RecoveryFileNeed::Removed => updating.delete_recovery(),
		}
	}

	/// The spaces whose items a backup takes and gives on this replica: those that it holds
	/// and the replica holds, and the groups that they add, at any level; and the personal
	/// space, whatever it names, while a recovery code of the person's stands, as a code
	/// opens the person's other spaces through it.
	pub(crate) fn backup_spaces(&self, backup: &Backup) -> Result<Vec<SpaceId>, NodeError> {
		let mut spaces = BTreeSet::new();
		let personal = self.info.personal;
		if self
			.roster(personal)?
			.codes()
			.any(nibkeep_governance::Code::stands)
		{
			spaces.insert(personal);
		}
		for space in &backup.spaces {
			if !self.spaces.contains_key(space) {
				continue;
			}
			spaces.insert(*space);
			for group in self.inner_groups(*space)? {
				if self.spaces.contains_key(&group) {
					spaces.insert(group);
				}
			}
		}
		Ok(spaces.into_iter().collect())
	}

	/// The spaces chosen for a backup, checked: every space of the library that is no
	/// group's, if none is chosen.
	fn chosen_spaces(&self, chosen: &[SpaceId]) -> Result<Vec<SpaceId>, NodeError> {
		let mut spaces = BTreeSet::new();
		if chosen.is_empty() {
			for (&id, open) in &self.spaces {
				if !open.is_group()? {
					spaces.insert(id);
				}
			}
		}
		for space in chosen {
			self.space(*space)?;
			spaces.insert(*space);
		}
		Ok(spaces.into_iter().collect())
	}

	/// The policies of `to` that stand for `policies`, of `from`: each default policy of
	/// `to` that is one of them in `from`, whether or not it is installed there, and each
	/// other of `to` named as one of them is.
	fn policies_in(
		&self,
		policies: &[EntityId],
		from: &[SpaceId],
		to: &[SpaceId],
	) -> Result<Vec<EntityId>, NodeError> {
		let mut labels = BTreeSet::new();
		let mut names = BTreeSet::new();
		for space in from {
			for (label, id) in default_policies(*space).into_iter().enumerate() {
				if policies.contains(&id) {
					labels.insert(label);
				}
			}
			if self.spaces.contains_key(space) {
				for policy in self.policies(*space)? {
					if policies.contains(&policy.id)
						&& let Some(name) = policy.name
					{
						names.insert(name);
					}
				}
			}
		}
		let mut found = BTreeSet::new();
		for space in to {
			let defaults = default_policies(*space);
			found.extend(
				labels
					.iter()
					.filter_map(|label| defaults.get(*label).copied()),
			);
			for policy in self.policies(*space)? {
				if policy
					.name
					.as_ref()
					.is_some_and(|name| names.contains(name))
				{
					found.insert(policy.id);
				}
			}
		}
		Ok(found.into_iter().collect())
	}

	/// Checks that each policy is one of those spaces'.
	fn check_backup_policies(
		&self,
		spaces: &[SpaceId],
		policies: &[EntityId],
	) -> Result<(), NodeError> {
		for policy in policies {
			let mut found = false;
			for space in spaces {
				found |= default_policies(*space).contains(policy)
					|| self
						.policies(*space)?
						.iter()
						.any(|known| known.id == *policy);
			}
			if !found {
				return Err(NodeError::NoEntity(format!(
					"{policy} is no policy of the backup's spaces"
				)));
			}
		}
		Ok(())
	}

	/// Takes in what the backup's packs and files hold and the replica lacks, as a sync takes
	/// in what another replica sends: changes and keys, then the sealed blobs that the
	/// replica keeps and lacks.
	fn take_in_backup(
		&mut self,
		updating: &Updating<'_>,
		packs: &Packs,
		spaces: &[SpaceId],
		now: &dyn Fn() -> u64,
		done: &mut BackupUpdate,
	) -> Result<(), NodeError> {
		let mut syncing = Syncing::new(self, now);
		for &space in spaces {
			let held: BTreeSet<[u8; 32]> = syncing.holdings(space)?.iter().map(item_id).collect();
			for file in packs.of_space(space) {
				let wanted: Vec<&Entry> = file
					.entries
					.iter()
					.filter(|entry| {
						!matches!(entry.what, What::Blob(_)) && !held.contains(&entry.id)
					})
					.collect();
				if wanted.is_empty() {
					continue;
				}
				let Ok(bytes) = updating.read(&file.path) else {
					continue;
				};
				for entry in wanted {
					let Some(Ok(item)) = bytes
						.get(entry.start..entry.start + entry.length)
						.map(|encoded| PackReader::item_at(space, encoded))
					else {
						continue;
					};
					match item {
						PackItem::Change(parcel) => {
							// Taken as from its author, so that what a replica that the space
							// ended made after that is left out, as a sync with it leaves it.
							let author = *parcel.change.author();
							if syncing.receive_change(space, &author, *parcel)? {
								done.taken.changes += 1;
							}
						}
						PackItem::RecordKey(statement, key) => {
							if syncing.receive_record_key(space, statement, key)? {
								done.taken.keys += 1;
							}
						}
						PackItem::BlobKey(sealed, key) => {
							if syncing.receive_blob_key(space, sealed, key)? {
								done.taken.keys += 1;
							}
						}
						PackItem::Blob(..) => {}
					}
				}
			}
			syncing.pause()?;
			let on_backup = |sealed: &Hash| packs.blob(sealed).is_some();
			for sealed in syncing.wanted_among(space, &on_backup, &BTreeSet::new(), usize::MAX)? {
				let kept = match packs.blob(&sealed) {
					Some(BlobAt::File(size)) => {
						let Ok(mut file) = updating.open(&backup::blob_path(&sealed)) else {
							continue;
						};
						syncing
							.receive_blob(space, sealed, size, &mut file)?
							.then_some(size)
					}
					Some(BlobAt::Pack {
						path,
						start,
						length,
						space: of,
						..
					}) => {
						let Some(PackItem::Blob(_, bytes)) =
							read_item(updating, &path, of, start, length)
						else {
							continue;
						};
						let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
						syncing
							.receive_blob(space, sealed, size, &mut bytes.as_slice())?
							.then_some(size)
					}
					None => None,
				};
				if let Some(size) = kept {
					done.taken.blobs += 1;
					done.taken.bytes += size;
				}
			}
			syncing.finish_space(space)?;
		}
		Ok(())
	}

	/// Applies the replica's purges to the backup: what they took away leaves the packs of
	/// the spaces that the replica holds, which are written anew without it, and a purged
	/// blob goes unless a pack of another space holds a key for it.
	fn purge_backup(
		&self,
		updating: &Updating<'_>,
		packs: &mut Packs,
		spaces: &[SpaceId],
		update: u64,
		waits: &mut Waits<'_>,
		done: &mut BackupUpdate,
	) -> Result<(), NodeError> {
		// Which purge keeps each item purged, by space, for the wait it counts from.
		let mut purged_by = BTreeMap::new();
		for &space in spaces {
			if let Some(open) = self.spaces.get(&space) {
				purged_by.insert(space, open.stored.log().purged_by()?);
			}
		}
		let gone_blobs = self.purged_blobs_of(packs, spaces, &purged_by, waits)?;
		let no_purges = nibkeep_log::PurgedBy::default();
		let mut rewritten = Vec::new();
		for (at, file) in packs.files.iter().enumerate() {
			if !spaces.contains(&file.space) {
				continue;
			}
			let log = self.space(file.space)?.stored.log();
			let by = purged_by.get(&file.space).unwrap_or(&no_purges);
			// For each entry: kept as it is, kept without its payload, or left out, once the
			// wait for its purge is over.
			let mut verdicts = Vec::with_capacity(file.entries.len());
			for entry in &file.entries {
				verdicts.push(match &entry.what {
					What::Change { id, complete: true }
						if log.is_change_purged(*id)?
							&& waits.due(by.changes.get(id), entry.id) =>
					{
						Verdict::Strip
					}
					What::RecordKey(statement)
						if log.is_purged(*statement)?
							&& waits.due(by.statements.get(statement), entry.id) =>
					{
						Verdict::Drop
					}
					What::BlobKey(sealed)
						if log.is_blob_purged(sealed)?
							&& waits.due(by.blobs.get(sealed), entry.id) =>
					{
						Verdict::Drop
					}
					What::Blob(sealed) if gone_blobs.contains(sealed) => Verdict::Drop,
					_ => Verdict::Keep,
				});
			}
			if verdicts.iter().all(|verdict| *verdict == Verdict::Keep) {
				continue;
			}
			let bytes = updating
				.read(&file.path)
				.map_err(NodeError::file(&updating.shown(&file.path)))?;
			let mut writer = PackWriter::new(file.space, update);
			for (entry, verdict) in file.entries.iter().zip(&verdicts) {
				let encoded = bytes
					.get(entry.start..entry.start + entry.length)
					.unwrap_or_default();
				match verdict {
					Verdict::Keep => writer.push_encoded(updating, encoded)?,
					Verdict::Strip => {
						if let Ok(PackItem::Change(mut parcel)) =
							PackReader::item_at(file.space, encoded)
						{
							parcel.payload = None;
							parcel.keys.clear();
							writer.push(updating, &PackItem::Change(parcel))?;
						}
						done.purged += 1;
					}
					Verdict::Drop => done.purged += 1,
				}
			}
			rewritten.push((at, writer.finish(updating)?));
		}
		// The new packs in place of the old, each deleted once its new one is written.
		for (at, new) in rewritten.into_iter().rev() {
			let old = packs.files.remove(at);
			if !new.iter().any(|file| file.path == old.path) {
				updating.delete(&old.path)?;
			}
			packs.files.extend(new);
		}
		for sealed in &gone_blobs {
			if packs.blob_files.remove(sealed).is_some() {
				updating.delete(&backup::blob_path(sealed))?;
				done.purged += 1;
			}
		}
		packs.reindex();
		Ok(())
	}

	/// The sealed blobs that leave a backup in an update: purged, their wait over, in a space
	/// of the replica's, and no pack of a space that kept it holds a key for them.
	fn purged_blobs_of(
		&self,
		packs: &Packs,
		spaces: &[SpaceId],
		purged_by: &BTreeMap<SpaceId, nibkeep_log::PurgedBy>,
		waits: &mut Waits<'_>,
	) -> Result<BTreeSet<Hash>, NodeError> {
		let keyed = packs.keyed();
		// Whether a purged blob leaves the backup: purged, its wait over, in a space of the
		// replica's, and no pack of a space that kept it holds a key for it.
		let mut purged_in = |sealed: &Hash| -> Result<bool, NodeError> {
			let mut purged_here = |space: &SpaceId| -> Result<Option<bool>, NodeError> {
				match (self.spaces.get(space), purged_by.get(space)) {
					(Some(open), Some(by)) => Ok(Some(
						open.stored.log().is_blob_purged(sealed)?
							&& waits.due(by.blobs.get(sealed), blob_id(sealed)),
					)),
					_ => Ok(None),
				}
			};
			let mut purged = false;
			match keyed.get(sealed) {
				Some(keyed) => {
					for space in keyed {
						match purged_here(space)? {
							Some(true) => purged = true,
							_ => return Ok(false),
						}
					}
				}
				None => {
					for space in spaces {
						purged |= purged_here(space)? == Some(true);
					}
				}
			}
			Ok(purged)
		};
		let mut gone_blobs = BTreeSet::new();
		for sealed in packs.blobs().keys() {
			if purged_in(sealed)? {
				gone_blobs.insert(*sealed);
			}
		}
		Ok(gone_blobs)
	}

	/// Writes what the backup lacks, for each space: a pack of the changes and keys, and the
	/// sealed blobs that the backup keeps, while its drive keeps 1 GiB free after each.
	fn write_backup(
		&self,
		updating: &Updating<'_>,
		packs: &mut Packs,
		(spaces, policies, keys_only): (&[SpaceId], &[EntityId], Option<SpaceId>),
		(update, wall): (u64, u64),
		record: &mut Record,
		done: &mut BackupUpdate,
	) -> Result<(), NodeError> {
		let mut free = updating.free()?;
		let mut written_blobs = BTreeSet::new();
		for &space in spaces {
			let open = self.space(space)?;
			let log = open.stored.log();
			let on_backup = packs.ids(space);
			let changes_on = packs.changes(space);
			let mut writer = PackWriter::new(space, update);
			let mut changes: Vec<(u64, ChangeId)> = log
				.summaries()?
				.into_iter()
				.filter(|summary| match changes_on.get(&summary.id) {
					None => true,
					Some(complete) => !complete && summary.complete,
				})
				.map(|summary| (summary.depth, summary.id))
				.collect();
			changes.sort();
			for (_, id) in changes {
				if let Some(parcel) = log.parcel(id)? {
					writer.push(updating, &PackItem::Change(Box::new(parcel)))?;
					done.written.changes += 1;
				}
			}
			for (statement, key) in log.wrapped_keys() {
				let holding = Holding::RecordKey {
					statement,
					key: key.clone(),
				};
				if !on_backup.contains(&item_id(&holding)) {
					writer.push(updating, &PackItem::RecordKey(statement, key.clone()))?;
					done.written.keys += 1;
				}
			}
			for (sealed, key) in log.wrapped_blob_keys() {
				let holding = Holding::BlobKey {
					sealed: *sealed,
					key: key.clone(),
				};
				if !on_backup.contains(&item_id(&holding)) {
					writer.push(updating, &PackItem::BlobKey(*sealed, key.clone()))?;
					done.written.keys += 1;
				}
			}
			let mut plan = self.backup_plan(space, policies)?;
			plan.nothing = keys_only == Some(space);
			let mut blobs = Vec::new();
			for sealed in open.stored.sealed_blobs()? {
				if written_blobs.contains(&sealed) || packs.blob(&sealed).is_some() {
					continue;
				}
				if let Some(rank) =
					plan.rank(log.is_secret_blob(&sealed), log.content_of_sealed(&sealed))
				{
					blobs.push((rank, sealed));
				}
			}
			blobs.sort();
			for (_, sealed) in blobs {
				let Some((size, mut file)) = open.stored.open_sealed_blob(&sealed)? else {
					continue;
				};
				let room = free.saturating_sub(writer.used());
				if room < size.saturating_add(MIN_FREE_BYTES) {
					done.unfit += 1;
					done.unfit_bytes += size;
					continue;
				}
				let kept = if size <= MOST_PACK_BLOB {
					let mut bytes = Vec::new();
					std::io::Read::read_to_end(&mut file, &mut bytes)
						.map_err(NodeError::file(&updating.shown("")))?;
					// A copy here that is damaged is not passed on.
					if Hash::of(&bytes) == sealed {
						writer.push(updating, &PackItem::Blob(sealed, bytes))?;
						true
					} else {
						false
					}
				} else if updating.write_blob(&sealed, &mut file)? {
					free = free.saturating_sub(size);
					packs.blob_files.insert(sealed, size);
					true
				} else {
					false
				};
				if kept {
					record.checks.insert(sealed, wall);
					written_blobs.insert(sealed);
					done.written.blobs += 1;
					done.written.bytes += size;
				}
			}
			free = free.saturating_sub(writer.used());
			packs.files.extend(writer.finish(updating)?);
			packs.reindex();
		}
		Ok(())
	}

	/// What a backup keeps of a space's sealed blobs: what an archive node keeps, of the
	/// files of `policies` only if any are named; and the order in which it writes them.
	fn backup_plan(&self, space: SpaceId, policies: &[EntityId]) -> Result<BackupPlan, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let rules = self.read_policies(space)?;
		let filter: Option<BTreeSet<EntityId>> =
			(!policies.is_empty()).then(|| policies.iter().copied().collect());
		let standard = answer_policy(space, false);
		let (mut in_store, mut in_place) = (BTreeSet::new(), BTreeSet::new());
		let (mut chosen, mut unchosen) = (BTreeSet::new(), BTreeSet::new());
		let mut order: BTreeMap<Hash, (u32, Option<Moment>)> = BTreeMap::new();
		let mut seen = BTreeSet::new();
		for file in open
			.stored
			.find(&fields::TYPE, &Value::Text(FILE.to_string()))?
		{
			let file = state.canonical(file)?;
			if !seen.insert(file) || !state.types(file)?.contains(&FILE) {
				continue;
			}
			let choice = rules.choose(&own_choices(state, file)?, &facts_of(state, file)?);
			let taken = taken_of(state, file)?;
			let mut contents = BTreeSet::new();
			let mut newest = taken;
			for member in state.merged(file)? {
				for id in storage.of_field(member, &CONTENT)? {
					if let Some(recorded) = storage.get(id)?
						&& let Value::Blob(content) = recorded.statement.value
					{
						contents.insert(content);
						if taken.is_none() {
							newest = newest.max(moment(recorded.at.millis()));
						}
					}
				}
			}
			if !gone(state, file)?
				&& let Some(current) = state.current(file, &CONTENT)?.iter().find_map(|statement| {
					match statement.value {
						Value::Blob(content) => Some(content),
						_ => None,
					}
				}) {
				if choice.needs.kept == Kept::Store {
					in_store.insert(current);
				} else {
					in_place.insert(current);
				}
			}
			if let Some(filter) = &filter {
				if filter.contains(&choice.policy.unwrap_or(standard)) {
					chosen.extend(contents.iter().copied());
				} else {
					unchosen.extend(contents.iter().copied());
				}
			}
			for content in contents {
				let entry = order.entry(content).or_insert((0, None));
				entry.0 = entry.0.max(choice.needs.copies);
				entry.1 = entry.1.max(newest);
			}
		}
		let mut not: BTreeSet<Hash> = in_place.difference(&in_store).copied().collect();
		not.extend(unchosen.difference(&chosen).copied());
		Ok(BackupPlan {
			nothing: false,
			keeps: Keeps::AllBut(not),
			order,
		})
	}

	/// Checks the backup's blob files that were last read in full 4 weeks before or more,
	/// the oldest first, at the drive's full speed, until `stop` says to stop: a damaged one
	/// is written again from the replica's copy, or set aside.
	fn check_backup(
		&self,
		updating: &Updating<'_>,
		record: &mut Record,
		update: u64,
		(now, stop): (&dyn Fn() -> u64, &dyn Fn() -> bool),
		done: &mut BackupUpdate,
	) -> Result<(), NodeError> {
		let wall = now();
		let mut due: Vec<(u64, Hash, u64)> = updating
			.blob_files()?
			.into_iter()
			.filter_map(|(sealed, size)| {
				let last = record.checks.get(&sealed).copied().unwrap_or(0);
				(last.saturating_add(CHECK_AGAIN) <= wall).then_some((last, sealed, size))
			})
			.collect();
		due.sort();
		let mut saved = Instant::now();
		for (at, (_, sealed, size)) in due.iter().enumerate() {
			if stop() {
				done.to_check = due.iter().skip(at).map(|(_, _, size)| size).sum();
				break;
			}
			let path = backup::blob_path(sealed);
			let intact = || updating.hash(&path).ok().map(|hash| hash == *sealed);
			match intact() {
				Some(true) => {
					record.checks.insert(*sealed, now());
					done.checked += 1;
					done.checked_bytes += size;
				}
				// Read again before it counts as damaged.
				Some(false) if intact() == Some(false) => {
					done.damaged += 1;
					record.damaged.insert(*sealed, now());
					let mut repaired = false;
					for open in self.spaces.values() {
						if !open.stored.holds_sealed(sealed) {
							continue;
						}
						if let Some((_, mut file)) = open.stored.open_sealed_blob(sealed)? {
							repaired = updating.write_blob(sealed, &mut file)?;
						}
						break;
					}
					if repaired {
						record.checks.insert(*sealed, now());
						done.repaired += 1;
					} else {
						updating.set_aside(&path);
						record.checks.remove(sealed);
						done.set_aside += 1;
					}
				}
				// One that cannot be read now is read at the next update.
				_ => {}
			}
			if saved.elapsed() >= SAVE_EVERY {
				let mut saving = record.clone();
				saving.update = update;
				updating.save_record(&saving)?;
				saved = Instant::now();
			}
		}
		Ok(())
	}
}

/// What to do with an entry of a pack that a purge may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
	Keep,
	Strip,
	Drop,
}

/// The item of a pack at `start`, `length` bytes long.
fn read_item(
	updating: &Updating<'_>,
	path: &str,
	space: SpaceId,
	start: usize,
	length: usize,
) -> Option<PackItem> {
	use std::io::{Read, Seek, SeekFrom};
	let mut file = updating.open(path).ok()?;
	file.seek(SeekFrom::Start(start as u64)).ok()?;
	let mut encoded = vec![0; length];
	file.read_exact(&mut encoded).ok()?;
	PackReader::item_at(space, &encoded).ok()
}

/// Merges, in each space of `spaces` whose folder has more than [`MERGE_ABOVE`] packs
/// smaller than [`MERGE_BELOW`], those packs into new ones, leaving out what other packs
/// hold.
fn merge(
	updating: &Updating<'_>,
	packs: &mut Packs,
	spaces: &[SpaceId],
	update: u64,
) -> Result<(), NodeError> {
	for &space in spaces {
		let small: Vec<usize> = packs
			.files
			.iter()
			.enumerate()
			.filter(|(_, file)| file.space == space && file.size < MERGE_BELOW)
			.map(|(at, _)| at)
			.collect();
		if small.len() <= MERGE_ABOVE {
			continue;
		}
		let mut seen: BTreeSet<[u8; 32]> = packs
			.files
			.iter()
			.enumerate()
			.filter(|(at, file)| file.space == space && !small.contains(at))
			.flat_map(|(_, file)| file.entries.iter().map(|entry| entry.id))
			.collect();
		seen.extend(packs.blob_files.keys().map(blob_id));
		let complete: BTreeSet<ChangeId> = packs
			.changes(space)
			.into_iter()
			.filter(|(_, complete)| *complete)
			.map(|(id, _)| id)
			.collect();
		let mut writer = PackWriter::new(space, update);
		let mut merged = Vec::new();
		for &at in &small {
			let file = &packs.files[at];
			// A pack that cannot be read now stays as it is.
			let Ok(bytes) = updating.read(&file.path) else {
				continue;
			};
			for entry in &file.entries {
				if let What::Change {
					id,
					complete: false,
				} = entry.what
					&& complete.contains(&id)
				{
					continue;
				}
				if !seen.insert(entry.id) {
					continue;
				}
				writer.push_encoded(
					updating,
					bytes
						.get(entry.start..entry.start + entry.length)
						.unwrap_or_default(),
				)?;
			}
			merged.push(at);
		}
		let new = writer.finish(updating)?;
		for at in merged.into_iter().rev() {
			let old = packs.files.remove(at);
			if !new.iter().any(|file| file.path == old.path) {
				updating.delete(&old.path)?;
			}
		}
		packs.files.extend(new);
		packs.reindex();
	}
	Ok(())
}

/// The operation that asserts a value.
fn assert(entity: EntityId, field: FieldName, value: Value, rng: &mut impl CryptoRng) -> Operation {
	Operation::assert(Statement::new(
		StatementId::random(rng),
		entity,
		field,
		value,
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_backup_counts_each_item_once_and_keeps_purges_it_could_not_tell_of() {
		let day = super::super::waiting::DAY;
		let purge = ChangeId::from_hash(Hash::of(b"a purge"));
		let elsewhere = ChangeId::from_hash(Hash::of(b"a purge of a space not held here"));
		let mut record = Record {
			waits: vec![(0, 30 * day)],
			purges: [(elsewhere, (5, 4))].into(),
			..Record::default()
		};
		let ending = BTreeSet::new();
		let mut waits = Waits::new(&mut record, 10, &ending);
		// One item met twice, as a blob and its key in two packs, and another.
		assert!(!waits.due(Some(&purge), [1; 32]));
		assert!(!waits.due(Some(&purge), [1; 32]));
		assert!(!waits.due(Some(&purge), [2; 32]));
		// An item whose purge cannot be told stays, untold.
		assert!(!waits.due(None, [3; 32]));
		waits.settle(|seen| *seen == purge);
		assert_eq!(record.purges.get(&purge), Some(&(10, 2)));
		assert_eq!(
			record.purges.get(&elsewhere),
			Some(&(5, 4)),
			"kept as it was"
		);
		// Its wait over, it leaves.
		let mut waits = Waits::new(&mut record, 10 + 30 * day, &ending);
		assert!(waits.due(Some(&purge), [1; 32]));
		waits.settle(|seen| *seen == purge);
		assert!(!record.purges.contains_key(&purge));
	}
}
