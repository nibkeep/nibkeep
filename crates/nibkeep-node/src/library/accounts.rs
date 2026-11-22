//! Where the copies are: each replica's account of what it holds in a space, which it brings
//! up to date before each sync and which syncs pass on; the marks that people give their
//! devices; and the disks that hold each file's copies and the space's changes
//! ([technical design: where the copies are]).
//!
//! [technical design: where the copies are]: ../../../../docs/Technical-design.md#96-where-the-copies-are

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::VerifyingKey;
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{AWAY, DEVICE, DEVICE_ROLE, KEEPS_UP_TO, WAITS};
use nibkeep_log::{AccountPage, BackupHead, DeviceRole, Head, History as _, PageBody, PageKind};
use nibkeep_model::{Operation, Statement, Value};
use nibkeep_store::StoredPage;

use super::Library;
use super::checking::COUNTS_FOR_MILLIS;
use crate::{NodeError, place};

/// The context of the key, derived from a space's ID, that a disk's name is hashed with.
const DISK_CONTEXT: &str = "nibkeep 2026-11 disk";

/// How many of the newest changes a check looks at to count those held here alone.
const NEWEST_LOOKED_AT: usize = 10_000;

/// How long a replica's account of a space stands without being brought up to date while
/// the space's state stays the same, in milliseconds: a minute, so that what changed beside
/// the state, such as a blob that came on its own or an original changed where it was
/// found, shows at a sync a minute later.
const REFRESHED_FOR: u64 = 60_000;

/// How many backups one replica's pages may tell of in a space: 8.
const BACKUPS_PER_REPLICA: usize = 8;

/// How a person marked a device, in the personal space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceMarks {
	/// What it does with the files of its spaces: every device is an everyday device unless
	/// marked as an archive node.
	pub role: DeviceRole,
	/// Whether it is kept away from home.
	pub away: bool,
	/// How many bytes of files it keeps as an everyday device, if the person set it; without
	/// it, a tenth of its disk on a computer, and none on a phone
	/// ([`Library::keeps_up_to`]).
	pub keeps: Option<u64>,
	/// How many days it waits before it destroys what a purge destroys, as an archive node,
	/// if the person set it, 0 for none; without it, 30 ([`super::waiting::wait_days`]).
	pub waits: Option<u64>,
}

/// A disk that holds copies of a space's files or changes, as this replica holds them now
/// and the other replicas' accounts say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
	/// Its ID in the space.
	pub id: Hash,
	/// Whether this replica is on it.
	pub here: bool,
	/// The replicas on it whose accounts came, this one first if it is on it.
	pub replicas: Vec<DiskReplica>,
	/// Whether it is an archive node's: every replica on it says that its device is one.
	pub archive: bool,
	/// Whether it is kept away from home: every replica on it says that its device is.
	pub away: bool,
	/// When the latest head of an account of a replica on it was issued, in milliseconds
	/// since 1970, if one is kept: this replica's own, on its disk.
	pub issued: Option<u64>,
	/// How many copies the checks of the replicas on it found damaged in the last 4 weeks,
	/// as their accounts say: this replica's as it knows now.
	pub damaged: u64,
	/// How many of the copies that their checks found damaged are damaged still.
	pub unrepaired: u64,
	/// The backups on it whose accounts came.
	pub backups: Vec<DiskBackup>,
}

/// A backup on a disk ([technical design: backups]).
///
/// [technical design: backups]: ../../../../docs/Technical-design.md#910-backups
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskBackup {
	/// Its record's ID.
	pub backup: EntityId,
	/// Its name, if it is one of this user's, whose personal space names it.
	pub name: Option<String>,
	/// The number of its update that its account tells of.
	pub update: u64,
}

/// A replica on a disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskReplica {
	/// Its record, if this replica knows it.
	pub replica: Option<EntityId>,
	/// Its name: the name of the app that keeps it.
	pub name: Option<String>,
	/// Its device's name.
	pub device: Option<String>,
	/// For another user's replica, the name of the member it is of.
	pub member: Option<String>,
	/// Whether it is this replica.
	pub this: bool,
}

/// How a disk holds a copy of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyHeld {
	/// Sealed in a store, last read in full and found intact at this time, in milliseconds
	/// since 1970, if ever.
	Sealed {
		/// When it was last checked.
		checked: Option<u64>,
	},
	/// As a plain copy, unchanged: an original kept where it was found, or a checkout, last
	/// read in full and found holding the content at this time, in milliseconds since 1970,
	/// if ever.
	InPlace {
		/// When it was last checked.
		checked: Option<u64>,
	},
}

/// A disk that holds a copy of a file, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCopy {
	/// The disk.
	pub disk: Disk,
	/// How it holds the copy.
	pub held: CopyHeld,
	/// Whether the copy counts: it was checked in the last 8 weeks
	/// ([technical design: checking and repair]).
	///
	/// [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair
	pub counts: bool,
}

/// How the space's changes are held, as a check counts them against the space's Critical
/// policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCopies {
	/// What the Critical policy asks.
	pub needs: crate::Needs,
	/// How many disks hold every change that this replica holds, its own included.
	pub disks: usize,
	/// How many of them are away from home.
	pub away: usize,
	/// How many of them are archive nodes'.
	pub archived: usize,
	/// How many of the changes this replica holds are held on its disk alone.
	pub here_alone: usize,
	/// Whether there may be more of those than counted, which is counted among the newest
	/// changes only.
	pub here_alone_more: bool,
}

impl Default for ChangeCopies {
	fn default() -> Self {
		Self {
			needs: crate::Needs::CRITICAL,
			disks: 0,
			away: 0,
			archived: 0,
			here_alone: 0,
			here_alone_more: false,
		}
	}
}

/// Sealed blobs, or contents, each with when a replica last checked it, in milliseconds
/// since 1970, or 0 if it never did.
pub(crate) type Checked = BTreeMap<Hash, u64>;

/// What another replica's latest account of a space says it holds.
pub(crate) struct Listed {
	/// Its disk.
	pub(crate) disk: Hash,
	/// Its sealed blobs.
	pub(crate) sealed: Checked,
	/// The contents of its unchanged plain copies.
	pub(crate) plain: Checked,
}

/// One author's pages about a backup, opened: its head, with when it was issued, and its
/// sealed blobs.
type PagesAbout = (Option<(BackupHead, u64)>, Vec<(Hash, u64)>);

/// The latest account of a backup in a space, opened.
pub(crate) struct BackupAccount {
	/// Its head.
	pub(crate) head: BackupHead,
	/// When its head was issued.
	pub(crate) issued: u64,
	/// Its sealed blobs, with when each was last checked.
	pub(crate) blobs: Vec<(Hash, u64)>,
}

/// One replica's account, opened.
#[derive(Default)]
struct Account {
	head: Option<(Head, u64)>,
	blobs: Vec<(Hash, u64)>,
	copies: Vec<(EntityId, Hash, u64)>,
}

/// The copies of a space by disk: this replica's as it holds them now, and the others' as
/// their accounts say.
pub(crate) struct Copies {
	/// The disks, this replica's first.
	pub(crate) disks: Vec<Disk>,
	/// For each sealed hash, the disks that hold it, this replica's as it holds it now, with
	/// when each last checked it, or 0 if it never did.
	by_sealed: BTreeMap<Hash, Vec<(usize, u64)>>,
	/// For each file and content, the disks that hold an unchanged plain copy, this replica's
	/// as it holds it now, with when each last checked it, or 0 if it never did.
	by_copy: BTreeMap<(EntityId, Hash), Vec<(usize, u64)>>,
	/// The heads of each other replica, with its disk.
	heads: Vec<(usize, Vec<ChangeId>)>,
	/// Copies last checked before this time count no longer, in milliseconds since 1970.
	since: u64,
}

impl Copies {
	/// This replica's disk.
	pub(crate) const OWN: usize = 0;

	/// The disks that hold a copy of the content `content` of a file, whose entities, those
	/// merged into it included, are `members`: those that hold one of its sealed hashes,
	/// `sealed`, or an unchanged plain copy of it, each with when it last checked one.
	fn checked_on(
		&self,
		sealed: &[Hash],
		(members, content): (&[EntityId], Option<Hash>),
	) -> BTreeMap<usize, u64> {
		let mut disks: BTreeMap<usize, u64> = BTreeMap::new();
		let mut note = |disk: usize, checked: u64| {
			let latest = disks.entry(disk).or_insert(0);
			*latest = (*latest).max(checked);
		};
		for each in sealed {
			for (disk, checked) in self.by_sealed.get(each).into_iter().flatten() {
				note(*disk, *checked);
			}
		}
		if let Some(content) = content {
			for member in members {
				for (disk, checked) in self.by_copy.get(&(*member, content)).into_iter().flatten() {
					note(*disk, *checked);
				}
			}
		}
		disks
	}

	/// The disks whose copies of a file count, as [`Copies::checked_on`] finds them: those
	/// checked in the last 8 weeks; and the disks that hold copies checked before only, which
	/// do not count.
	pub(crate) fn holding(
		&self,
		sealed: &[Hash],
		of: (&[EntityId], Option<Hash>),
	) -> (BTreeSet<usize>, BTreeSet<usize>) {
		let (counting, old): (Vec<_>, Vec<_>) = self
			.checked_on(sealed, of)
			.into_iter()
			.partition(|(_, checked)| *checked >= self.since);
		(
			counting.into_iter().map(|(disk, _)| disk).collect(),
			old.into_iter().map(|(disk, _)| disk).collect(),
		)
	}

	/// Whether this replica holds an unchanged plain copy of the content of a file whose
	/// entities are `members`, which the last check did not find damaged.
	pub(crate) fn plain_here(&self, members: &[EntityId], content: Option<Hash>) -> bool {
		content.is_some_and(|content| {
			members.iter().any(|member| {
				self.by_copy
					.get(&(*member, content))
					.is_some_and(|disks| disks.iter().any(|(disk, _)| *disk == Self::OWN))
			})
		})
	}

	/// How many of the disks are away from home, and how many are archive nodes'.
	pub(crate) fn marked(&self, disks: &BTreeSet<usize>) -> (usize, usize) {
		let away = disks.iter().filter(|disk| self.disks[**disk].away).count();
		let archived = disks
			.iter()
			.filter(|disk| self.disks[**disk].archive)
			.count();
		(away, archived)
	}
}

/// A disk's ID in a space: its name hashed with a key derived from the space's ID, so that
/// replicas on one disk show one ID within a space, and a disk's IDs in two spaces cannot be
/// linked; a replica whose disk nothing names is a disk of its own.
pub(crate) fn disk_id(space: SpaceId, name: Option<&[u8]>, replica: &VerifyingKey) -> Hash {
	let key = blake3::derive_key(DISK_CONTEXT, space.as_bytes());
	let mut hasher = blake3::Hasher::new_keyed(&key);
	match name {
		Some(name) => hasher.update(b"disk\0").update(name),
		None => hasher.update(b"replica\0").update(replica.as_bytes()),
	};
	Hash::of(hasher.finalize().as_bytes())
}

/// A page with nothing on it, as a replica issues for a page it no longer has anything for.
fn empty(kind: PageKind) -> Option<PageBody> {
	match kind {
		PageKind::Head => None,
		PageKind::Blobs(byte) => Some(PageBody::Blobs(byte, Vec::new())),
		PageKind::Copies(byte) => Some(PageBody::Copies(byte, Vec::new())),
	}
}

impl Library {
	/// How the person marked one of their devices, in the personal space: as an archive node
	/// or an everyday device, and whether it is kept away from home.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn device_marks(&self, device: EntityId) -> Result<DeviceMarks, NodeError> {
		let state = self.space(self.info.personal)?.stored.log().state();
		let role = state
			.last_written(device, &DEVICE_ROLE)?
			.and_then(|statement| statement.value.as_text().and_then(DeviceRole::from_name))
			.unwrap_or_default();
		let away = state
			.last_written(device, &AWAY)?
			.is_some_and(|statement| statement.value == Value::Boolean(true));
		let number = |field| -> Result<Option<u64>, NodeError> {
			Ok(state
				.last_written(device, field)?
				.and_then(|statement| match statement.value {
					Value::Integer(number) => u64::try_from(number).ok(),
					_ => None,
				}))
		};
		Ok(DeviceMarks {
			role,
			away,
			keeps: number(&KEEPS_UP_TO)?,
			waits: number(&WAITS)?,
		})
	}

	/// Marks one of the person's devices, in the personal space: as an archive node or an
	/// everyday device, as kept away from home or not, and with how many bytes of files it
	/// keeps as an everyday device, or the default. Its replicas tell the others so at
	/// their next syncs ([technical design: where the copies are]).
	///
	/// [technical design: where the copies are]: ../../../../docs/Technical-design.md#96-where-the-copies-are
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichDevice`] if `device` is no device of the personal space, and
	/// another error if the change cannot be saved.
	pub fn set_device_marks(
		&mut self,
		device: EntityId,
		marks: DeviceMarks,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let state = self.space(personal)?.stored.log().state();
		let device = state.canonical(device)?;
		if state.is_deleted(device)? || !state.types(device)?.contains(&DEVICE) {
			return Err(NodeError::WhichDevice(format!(
				"{device} is no device of yours"
			)));
		}
		let mut operations = Vec::new();
		for (field, value) in [
			(DEVICE_ROLE, Value::Text(marks.role.as_str().to_owned())),
			(AWAY, Value::Boolean(marks.away)),
		] {
			// The field's last writer wins: a new value replaces every value before it.
			let shown = state
				.last_written(device, &field)?
				.map(|statement| statement.value.clone());
			let unmarked = match &value {
				Value::Text(role) => role == DeviceRole::Everyday.as_str(),
				_ => value == Value::Boolean(false),
			};
			if shown.as_ref() == Some(&value) || (shown.is_none() && unmarked) {
				continue;
			}
			let replaced: Vec<StatementId> = state
				.current(device, &field)?
				.iter()
				.map(|statement| statement.id)
				.collect();
			operations.push(Operation::replace(
				Statement::new(StatementId::random(rng), device, field, value),
				replaced,
			));
		}
		// The size and the wait, or the defaults without them: their values go.
		for (field, number) in [(KEEPS_UP_TO, marks.keeps), (WAITS, marks.waits)] {
			let values: Vec<StatementId> = state
				.current(device, &field)?
				.iter()
				.map(|statement| statement.id)
				.collect();
			let shown = state
				.last_written(device, &field)?
				.map(|statement| statement.value.clone());
			match number.map(|number| Value::Integer(i64::try_from(number).unwrap_or(i64::MAX))) {
				Some(value) if shown.as_ref() != Some(&value) => {
					operations.push(Operation::replace(
						Statement::new(StatementId::random(rng), device, field, value),
						values,
					));
				}
				None if !values.is_empty() => {
					operations.push(Operation::retract(device, values));
				}
				_ => {}
			}
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(personal, operations, wall_millis, rng)?;
		// This device's wait holds at once if it is longer, and so does keeping what purges
		// destroy.
		if device == self.info.device {
			self.refresh_wait(wall_millis)?;
		}
		Ok(())
	}

	/// Whether the replica whose key is `key` counts in a space: listed there, and neither
	/// revoked nor removed.
	pub(crate) fn counts_in(&self, space: SpaceId, key: &VerifyingKey) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		Ok(log.is_listed(key, &open.keys)?
			&& !log.is_revoked(key, &open.keys)?
			&& !log.is_removed(key, &open.keys)?)
	}

	/// This replica's pages of its account of a space, as the log file keeps them, opened,
	/// by kind, with when each was issued.
	fn own_pages(&self, space: SpaceId) -> Result<BTreeMap<PageKind, (PageBody, u64)>, NodeError> {
		let own = self.replica.signing.verifying_key();
		let open = self.space(space)?;
		let mut pages = BTreeMap::new();
		for stored in open.stored.account_pages()? {
			if stored.author != *own.as_bytes() {
				continue;
			}
			if let Some(body) = AccountPage::from_bytes(&stored.bytes)
				.ok()
				.and_then(|page| page.open(&open.keys).ok())
			{
				pages.insert(body.kind(), (body, stored.issued));
			}
		}
		Ok(pages)
	}

	/// The disk that this replica is on, by its ID in a space, as its folder's drive tells,
	/// read anew each time. A name read later than a guess replaces the guess.
	pub(crate) fn own_disk(&self, space: SpaceId) -> Hash {
		disk_id(
			space,
			place::disk_name(&self.directory).as_deref(),
			&self.replica.signing.verifying_key(),
		)
	}

	/// What this replica holds of a space now, as the pages of its account tell it at
	/// `wall_millis`: the head, and each page that lists something.
	fn own_bodies(
		&self,
		space: SpaceId,
		disk: Hash,
		wall_millis: u64,
	) -> Result<Vec<PageBody>, NodeError> {
		let open = self.space(space)?;
		let marks = self.device_marks(self.info.device)?;
		let plain = self.plain_state(space)?;
		let (damaged, unrepaired) = self.own_damage(space, &plain, wall_millis)?;
		let mut bodies = vec![PageBody::Head(Head {
			disk,
			role: marks.role,
			away: marks.away,
			heads: open.stored.log().heads()?.into_iter().collect(),
			damaged,
			unrepaired,
			waits: self
				.wait_in_force(wall_millis)?
				.div_ceil(super::waiting::DAY),
		})];
		let checks = self.blob_checks(space)?;
		let mut blobs: BTreeMap<u8, Vec<(Hash, u64)>> = BTreeMap::new();
		for sealed in open.stored.sealed_blobs()? {
			let checked = checks.get(&sealed).copied().unwrap_or(0);
			blobs
				.entry(sealed.digest()[0])
				.or_default()
				.push((sealed, checked));
		}
		bodies.extend(
			blobs
				.into_iter()
				.map(|(byte, blobs)| PageBody::Blobs(byte, blobs)),
		);
		let mut copies: BTreeMap<u8, Vec<(EntityId, Hash, u64)>> = BTreeMap::new();
		for ((file, content), checked) in plain.copies() {
			copies
				.entry(file.as_bytes()[0])
				.or_default()
				.push((file, content, checked));
		}
		bodies.extend(
			copies
				.into_iter()
				.map(|(byte, copies)| PageBody::Copies(byte, copies)),
		);
		Ok(bodies)
	}

	/// Brings this replica's account of a space up to date: issues each page whose content
	/// changed since it last issued it, a page it no longer has anything for as an empty
	/// one, at the wall clock `wall_millis` or just after its last page of that kind, and
	/// keeps them in the log file. Returns how many pages it issued.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read, or the pages cannot be saved.
	pub(crate) fn refresh_account(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		// A sync with each of several replicas, one after the other, finds the same.
		let mark = self.state_mark(space)?;
		if self
			.accounts_refreshed
			.borrow()
			.get(&space)
			.is_some_and(|(at_mark, at)| {
				*at_mark == mark && wall_millis.saturating_sub(*at) < REFRESHED_FOR
			}) {
			return Ok(0);
		}
		let count = self.issue_account(space, wall_millis, rng)?;
		self.accounts_refreshed
			.borrow_mut()
			.insert(space, (mark, wall_millis));
		Ok(count)
	}

	/// Issues each page of this replica's account of a space whose content changed, as
	/// [`Library::refresh_account`] says.
	fn issue_account(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let issued_before: BTreeMap<u16, u64> = {
			let own = self.replica.signing.verifying_key();
			self.space(space)?
				.stored
				.account_page_ids()?
				.into_iter()
				.filter(|stored| stored.author == *own.as_bytes())
				.map(|stored| (stored.page, stored.issued))
				.collect()
		};
		let pages = self.own_pages(space)?;
		let disk = self.own_disk(space);
		let mut bodies: BTreeMap<PageKind, PageBody> = self
			.own_bodies(space, disk, wall_millis)?
			.into_iter()
			.map(|body| (body.kind(), body))
			.collect();
		for (kind, (body, _)) in &pages {
			if !bodies.contains_key(kind)
				&& let Some(nothing) = empty(*kind)
				&& *body != nothing
			{
				bodies.insert(*kind, nothing);
			}
		}
		let author = *self.replica.signing.verifying_key().as_bytes();
		let mut issued = Vec::new();
		let open = self.space(space)?;
		for (kind, body) in bodies {
			if pages.get(&kind).is_some_and(|(kept, _)| *kept == body) {
				continue;
			}
			let at = issued_before
				.get(&kind.number())
				.map_or(wall_millis, |last| wall_millis.max(last.saturating_add(1)));
			let page =
				AccountPage::issue(space, &open.keys, &self.replica.signing, (at, &body), rng);
			let bytes = page.to_bytes();
			issued.push(StoredPage {
				author,
				backup: None,
				page: kind.number(),
				issued: at,
				id: Hash::of(&bytes),
				bytes,
			});
		}
		let count = issued.len();
		self.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?
			.stored
			.save_account_pages(&issued)?;
		Ok(count)
	}

	/// Checks a page of an account that another replica sent: whether its author signed it,
	/// it is of this space, its author is a replica of the space that counts there and not
	/// this one, and it is later than the one of that page that the log file keeps. Returns
	/// it as the log file keeps it if so, for [`Library::save_accounts`].
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn check_account(
		&self,
		space: SpaceId,
		bytes: &[u8],
	) -> Result<Option<StoredPage>, NodeError> {
		let Ok(page) = AccountPage::from_bytes(bytes) else {
			return Ok(None);
		};
		if page.space() != space
			|| *page.author() == self.replica.signing.verifying_key()
			|| !self.counts_in(space, page.author())?
		{
			return Ok(None);
		}
		let stored = StoredPage {
			author: *page.author().as_bytes(),
			backup: page.about().map(|backup| *backup.as_bytes()),
			page: page.kind().number(),
			issued: page.issued(),
			id: Hash::of(bytes),
			bytes: bytes.to_vec(),
		};
		let open = self.space(space)?;
		let kept = if stored.backup.is_some() {
			open.stored.backup_account_page_ids()?
		} else {
			open.stored.account_page_ids()?
		};
		// One replica speaks for a few backups at most, so that none fills the others' disks
		// with pages about backups it made up.
		if let Some(backup) = stored.backup {
			let spoken_for: BTreeSet<[u8; 16]> = kept
				.iter()
				.filter(|kept| kept.author == stored.author)
				.filter_map(|kept| kept.backup)
				.collect();
			if !spoken_for.contains(&backup) && spoken_for.len() >= BACKUPS_PER_REPLICA {
				return Ok(None);
			}
		}
		let later = kept
			.into_iter()
			.find(|kept| {
				kept.author == stored.author
					&& kept.backup == stored.backup
					&& kept.page == stored.page
			})
			.is_none_or(|kept| (stored.issued, stored.id) > (kept.issued, kept.id));
		Ok(later.then_some(stored))
	}

	/// Keeps pages of accounts of a space that [`Library::check_account`] passed, together:
	/// each that is still later than the one of its page that the log file keeps.
	///
	/// # Errors
	///
	/// Returns an error if the pages cannot be saved.
	pub(crate) fn save_accounts(
		&mut self,
		space: SpaceId,
		pages: &[StoredPage],
	) -> Result<usize, NodeError> {
		Ok(self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?
			.stored
			.save_account_pages(pages)?)
	}

	/// The pages of accounts of a space that a sync passes on, by the hash of each page's
	/// encoding, with when it was issued: this replica's, and those of the replicas that
	/// count in the space.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn account_holdings(&self, space: SpaceId) -> Result<Vec<(Hash, u64)>, NodeError> {
		let own = *self.replica.signing.verifying_key().as_bytes();
		let mut counting: BTreeMap<[u8; 32], bool> = BTreeMap::new();
		let mut holdings = Vec::new();
		let open = self.space(space)?;
		let pages = open
			.stored
			.account_page_ids()?
			.into_iter()
			.chain(open.stored.backup_account_page_ids()?);
		for stored in pages {
			let counts = if stored.author == own {
				true
			} else if let Some(counts) = counting.get(&stored.author) {
				*counts
			} else {
				let counts = VerifyingKey::from_bytes(&stored.author)
					.map_or(Ok(false), |key| self.counts_in(space, &key))?;
				counting.insert(stored.author, counts);
				counts
			};
			if counts {
				holdings.push((stored.id, stored.issued));
			}
		}
		Ok(holdings)
	}

	/// The encoding of a page of an account of a space that the log file keeps, by the hash
	/// of its encoding.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn account_page(
		&self,
		space: SpaceId,
		id: &Hash,
	) -> Result<Option<Vec<u8>>, NodeError> {
		Ok(self.space(space)?.stored.account_page(id)?)
	}

	/// The copies of a space by disk: what this replica holds now, and what the latest pages
	/// of the accounts of the replicas that count in the space say they hold, as of
	/// `wall_millis`, before which copies checked more than 8 weeks earlier count no longer.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn copies_by_disk(
		&self,
		space: SpaceId,
		wall_millis: u64,
	) -> Result<Copies, NodeError> {
		let own = self.replica.signing.verifying_key();
		let pages = self.own_pages(space)?;
		let own_disk = self.own_disk(space);
		let marks = self.device_marks(self.info.device)?;
		let plain = self.plain_state(space)?;
		let (damaged, unrepaired) = self.own_damage(space, &plain, wall_millis)?;
		let mut replicas: BTreeMap<[u8; 32], crate::Replica> = BTreeMap::new();
		for replica in self.replicas()?.into_iter().chain(self.peers()?) {
			if let Some(key) = replica.signing {
				replicas.entry(*key.as_bytes()).or_insert(replica);
			}
		}
		let named = |key: &[u8; 32], this: bool| {
			let replica = replicas.get(key);
			DiskReplica {
				replica: replica.map(|replica| replica.id),
				name: replica.and_then(|replica| replica.name.clone()),
				device: replica.and_then(|replica| replica.device_name.clone()),
				member: replica.and_then(|replica| replica.member.clone()),
				this,
			}
		};
		let mut copies = Copies {
			disks: vec![Disk {
				id: own_disk,
				here: true,
				replicas: vec![named(own.as_bytes(), true)],
				archive: marks.role == DeviceRole::Archive,
				away: marks.away,
				issued: pages.get(&PageKind::Head).map(|(_, issued)| *issued),
				damaged,
				unrepaired,
				backups: Vec::new(),
			}],
			by_sealed: BTreeMap::new(),
			by_copy: BTreeMap::new(),
			heads: Vec::new(),
			since: wall_millis.saturating_sub(COUNTS_FOR_MILLIS),
		};
		self.add_own_copies(space, &plain, &mut copies)?;
		let accounts = self.other_accounts(space)?;
		let mut by_id: BTreeMap<Hash, usize> = BTreeMap::from([(own_disk, Copies::OWN)]);
		for (author, account) in accounts {
			// Without its head, an account says nothing of where its copies are.
			let Some((head, issued)) = account.head else {
				continue;
			};
			let index = *by_id.entry(head.disk).or_insert_with(|| {
				copies.disks.push(Disk {
					id: head.disk,
					here: false,
					replicas: Vec::new(),
					archive: true,
					away: true,
					issued: None,
					damaged: 0,
					unrepaired: 0,
					backups: Vec::new(),
				});
				copies.disks.len() - 1
			});
			let disk = &mut copies.disks[index];
			disk.replicas.push(named(&author, false));
			disk.archive &= head.role == DeviceRole::Archive;
			disk.away &= head.away;
			disk.issued = disk.issued.max(Some(issued));
			disk.damaged = disk.damaged.saturating_add(head.damaged);
			disk.unrepaired = disk.unrepaired.saturating_add(head.unrepaired);
			for (sealed, checked) in account.blobs {
				copies
					.by_sealed
					.entry(sealed)
					.or_default()
					.push((index, checked));
			}
			for (file, content, checked) in account.copies {
				copies
					.by_copy
					.entry((file, content))
					.or_default()
					.push((index, checked));
			}
			copies.heads.push((index, head.heads));
		}
		self.add_backup_copies(space, &mut copies, &mut by_id)?;
		Ok(copies)
	}

	/// Adds the backups of a space to `copies`, each as the replica that updated it last
	/// says: a disk too, which keeps what an archive node keeps; `by_id` holds the index of
	/// each disk by its ID.
	fn add_backup_copies(
		&self,
		space: SpaceId,
		copies: &mut Copies,
		by_id: &mut BTreeMap<Hash, usize>,
	) -> Result<(), NodeError> {
		// The backups, each as the replica that updated it last says: a disk too, which keeps
		// what an archive node keeps.
		let backup_names: BTreeMap<EntityId, String> = self
			.backups()?
			.into_iter()
			.map(|backup| (backup.id, backup.name))
			.collect();
		for (backup, account) in self.backup_accounts(space)? {
			// One that the person removed no longer counts, whatever pages about it remain.
			if self.backup_forgotten(backup)? {
				continue;
			}
			let head = account.head;
			let index = *by_id.entry(head.disk).or_insert_with(|| {
				copies.disks.push(Disk {
					id: head.disk,
					here: false,
					replicas: Vec::new(),
					archive: true,
					away: true,
					issued: None,
					damaged: 0,
					unrepaired: 0,
					backups: Vec::new(),
				});
				copies.disks.len() - 1
			});
			let disk = &mut copies.disks[index];
			disk.backups.push(DiskBackup {
				backup,
				name: backup_names.get(&backup).cloned(),
				update: head.update,
			});
			disk.away &= head.away;
			disk.issued = disk.issued.max(Some(account.issued));
			disk.damaged = disk.damaged.saturating_add(head.damaged);
			disk.unrepaired = disk.unrepaired.saturating_add(head.unrepaired);
			for (sealed, checked) in account.blobs {
				copies
					.by_sealed
					.entry(sealed)
					.or_default()
					.push((index, checked));
			}
			copies.heads.push((index, head.heads));
		}
		Ok(())
	}

	/// The latest account of each backup of a space, as the replica that counts there and
	/// tells of its latest update says it, this one included: its head, when that was
	/// issued, and its sealed blobs with when each was last checked.
	pub(crate) fn backup_accounts(
		&self,
		space: SpaceId,
	) -> Result<BTreeMap<EntityId, BackupAccount>, NodeError> {
		let own = *self.replica.signing.verifying_key().as_bytes();
		let open = self.space(space)?;
		// Each author's pages about each backup, opened.
		let mut by_author: BTreeMap<(EntityId, [u8; 32]), PagesAbout> = BTreeMap::new();
		let mut counting: BTreeMap<[u8; 32], bool> = BTreeMap::new();
		for stored in open.stored.backup_account_pages()? {
			let Some(backup) = stored.backup.map(EntityId::from_bytes) else {
				continue;
			};
			let counts = if stored.author == own {
				true
			} else if let Some(counts) = counting.get(&stored.author) {
				*counts
			} else {
				let counts = VerifyingKey::from_bytes(&stored.author)
					.map_or(Ok(false), |key| self.counts_in(space, &key))?;
				counting.insert(stored.author, counts);
				counts
			};
			let Some(body) = counts
				.then(|| AccountPage::from_bytes(&stored.bytes).ok())
				.flatten()
				.filter(|page| {
					page.author().as_bytes() == &stored.author && page.about() == Some(backup)
				})
				.and_then(|page| page.open(&open.keys).ok())
			else {
				continue;
			};
			let entry = by_author.entry((backup, stored.author)).or_default();
			match body {
				PageBody::BackupHead(head) => entry.0 = Some((head, stored.issued)),
				PageBody::Blobs(_, blobs) => entry.1.extend(blobs),
				PageBody::Head(_) | PageBody::Copies(..) => {}
			}
		}
		let mut accounts: BTreeMap<EntityId, BackupAccount> = BTreeMap::new();
		for ((backup, _), (head, blobs)) in by_author {
			let Some((head, issued)) = head else {
				continue;
			};
			let later = accounts
				.get(&backup)
				.is_none_or(|kept| (head.update, issued) > (kept.head.update, kept.issued));
			if later {
				accounts.insert(
					backup,
					BackupAccount {
						head,
						issued,
						blobs,
					},
				);
			}
		}
		Ok(accounts)
	}

	/// Issues the pages of the account of a backup in a space whose content changed since
	/// this replica last issued them, a page it no longer has anything for as an empty one,
	/// at `wall_millis` or just after its last page of that kind, and keeps them in the log
	/// file. Returns how many it issued.
	pub(crate) fn issue_backup_account(
		&mut self,
		space: SpaceId,
		backup: EntityId,
		(head, blobs): (BackupHead, &BTreeMap<Hash, u64>),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let author = *self.replica.signing.verifying_key().as_bytes();
		let id = *backup.as_bytes();
		let open = self.space(space)?;
		let mut before: BTreeMap<PageKind, (PageBody, u64)> = BTreeMap::new();
		for stored in open.stored.backup_account_pages()? {
			if stored.author != author || stored.backup != Some(id) {
				continue;
			}
			if let Some(body) = AccountPage::from_bytes(&stored.bytes)
				.ok()
				.and_then(|page| page.open(&open.keys).ok())
			{
				before.insert(body.kind(), (body, stored.issued));
			}
		}
		// Each page later than the last of its kind that this replica issued, whether that
		// one opens or not.
		let last_issued: BTreeMap<u16, u64> = open
			.stored
			.backup_account_page_ids()?
			.into_iter()
			.filter(|stored| stored.author == author && stored.backup == Some(id))
			.map(|stored| (stored.page, stored.issued))
			.collect();
		let mut bodies: BTreeMap<PageKind, PageBody> =
			BTreeMap::from([(PageKind::Head, PageBody::BackupHead(head))]);
		let mut pages: BTreeMap<u8, Vec<(Hash, u64)>> = BTreeMap::new();
		for (sealed, checked) in blobs {
			pages
				.entry(sealed.digest()[0])
				.or_default()
				.push((*sealed, *checked));
		}
		for (byte, listed) in pages {
			bodies.insert(PageKind::Blobs(byte), PageBody::Blobs(byte, listed));
		}
		for (kind, (body, _)) in &before {
			if !bodies.contains_key(kind)
				&& let Some(nothing) = empty(*kind)
				&& *body != nothing
			{
				bodies.insert(*kind, nothing);
			}
		}
		let mut issued = Vec::new();
		for (kind, body) in bodies {
			if before.get(&kind).is_some_and(|(kept, _)| *kept == body) {
				continue;
			}
			let at = last_issued
				.get(&kind.number())
				.map_or(wall_millis, |last| wall_millis.max(last.saturating_add(1)));
			let page = AccountPage::issue_about(
				space,
				&open.keys,
				&self.replica.signing,
				backup,
				(at, &body),
				rng,
			);
			let bytes = page.to_bytes();
			issued.push(StoredPage {
				author,
				backup: Some(id),
				page: kind.number(),
				issued: at,
				id: Hash::of(&bytes),
				bytes,
			});
		}
		let count = issued.len();
		self.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?
			.stored
			.save_account_pages(&issued)?;
		Ok(count)
	}

	/// The latest accounts of the replicas that count in a space, but this one's, opened.
	fn other_accounts(&self, space: SpaceId) -> Result<BTreeMap<[u8; 32], Account>, NodeError> {
		self.other_accounts_where(space, |_, _| true)
	}

	/// The heads of the latest accounts of the other replicas that count in a space, by
	/// their keys: their pages of blobs and copies are not read.
	pub(crate) fn other_heads(&self, space: SpaceId) -> Result<Vec<([u8; 32], Head)>, NodeError> {
		Ok(self
			.other_accounts_where(space, |_, page| page == 0)?
			.into_iter()
			.filter_map(|(author, account)| account.head.map(|(head, _)| (author, head)))
			.collect())
	}

	/// Whether a device of a space is an archive node: this replica's, as the person marked
	/// it, or another's, as the latest account of a replica of the space on it says.
	pub(crate) fn has_archive_node(&self, space: SpaceId) -> Result<bool, NodeError> {
		if self.device_marks(self.info.device)?.role == DeviceRole::Archive {
			return Ok(true);
		}
		let head = PageKind::Head.number();
		Ok(self
			.other_accounts_where(space, |_, page| page == head)?
			.values()
			.any(|account| {
				account
					.head
					.as_ref()
					.is_some_and(|(head, _)| head.role == DeviceRole::Archive)
			}))
	}

	/// The sealed blobs of a space that another replica's latest account lists, by its key;
	/// `None` if this replica keeps no account of it in the space.
	pub(crate) fn listed_blobs(
		&self,
		space: SpaceId,
		author: &VerifyingKey,
	) -> Result<Option<BTreeSet<Hash>>, NodeError> {
		let accounts = self.other_accounts_where(space, |of, _| of == author.as_bytes())?;
		Ok(accounts
			.get(author.as_bytes())
			.map(|account| account.blobs.iter().map(|(sealed, _)| *sealed).collect()))
	}

	/// Whether each disk of the space's other replicas is an archive node's, by its ID, as the
	/// heads of their latest accounts say: only if every replica on it says that its device is
	/// one.
	pub(crate) fn archive_disks(&self, space: SpaceId) -> Result<BTreeMap<Hash, bool>, NodeError> {
		let head = PageKind::Head.number();
		let mut disks = BTreeMap::new();
		for account in self
			.other_accounts_where(space, |_, page| page == head)?
			.into_values()
		{
			if let Some((head, _)) = account.head {
				let archive = disks.entry(head.disk).or_insert(true);
				*archive &= head.role == DeviceRole::Archive;
			}
		}
		Ok(disks)
	}

	/// What the latest accounts of these other replicas of a space, by their keys, say they
	/// hold: for each that sent its head, its disk, its sealed blobs with when it last checked
	/// each, and, if `plain` asks, the contents of its unchanged plain copies, as originals
	/// kept where they were found, with when it last checked one of each; 0 if it never did.
	pub(crate) fn listed_by(
		&self,
		space: SpaceId,
		authors: &BTreeSet<[u8; 32]>,
		plain: bool,
	) -> Result<BTreeMap<[u8; 32], Listed>, NodeError> {
		let copies = PageKind::Copies(0).number();
		Ok(self
			.other_accounts_where(space, |author, page| {
				authors.contains(author) && (plain || page < copies)
			})?
			.into_iter()
			.filter_map(|(author, account)| {
				let (head, _) = account.head?;
				let mut plain: Checked = BTreeMap::new();
				for (_, content, checked) in account.copies {
					let latest = plain.entry(content).or_insert(0);
					*latest = (*latest).max(checked);
				}
				Some((
					author,
					Listed {
						disk: head.disk,
						sealed: account.blobs.into_iter().collect(),
						plain,
					},
				))
			})
			.collect())
	}

	/// The latest accounts of a space's other replicas that count there, of the pages that
	/// `keep` keeps, by their authors' keys and the pages' numbers.
	fn other_accounts_where(
		&self,
		space: SpaceId,
		keep: impl Fn(&[u8; 32], u16) -> bool,
	) -> Result<BTreeMap<[u8; 32], Account>, NodeError> {
		let own = self.replica.signing.verifying_key();
		let open = self.space(space)?;
		let mut accounts: BTreeMap<[u8; 32], Account> = BTreeMap::new();
		let mut counting: BTreeMap<[u8; 32], bool> = BTreeMap::new();
		for stored in open.stored.account_pages()? {
			if stored.author == *own.as_bytes() || !keep(&stored.author, stored.page) {
				continue;
			}
			let counts = if let Some(counts) = counting.get(&stored.author) {
				*counts
			} else {
				let counts = VerifyingKey::from_bytes(&stored.author)
					.map_or(Ok(false), |key| self.counts_in(space, &key))?;
				counting.insert(stored.author, counts);
				counts
			};
			let Some(body) = counts
				.then(|| AccountPage::from_bytes(&stored.bytes).ok())
				.flatten()
				.filter(|page| page.author().as_bytes() == &stored.author)
				.and_then(|page| page.open(&open.keys).ok())
			else {
				continue;
			};
			let account = accounts.entry(stored.author).or_default();
			match body {
				PageBody::Head(head) => account.head = Some((head, stored.issued)),
				PageBody::Blobs(_, blobs) => account.blobs.extend(blobs),
				PageBody::Copies(_, entries) => account.copies.extend(entries),
				PageBody::BackupHead(_) => {}
			}
		}
		Ok(accounts)
	}

	/// When this replica last read each sealed blob of a space in full and found it intact: as
	/// the space records it, or as another of the replica's spaces does, whichever is later,
	/// as its spaces share one file of each blob. A blob whose key came to a space after its
	/// file, as the new space of a move, so counts as checked when its file last was.
	pub(crate) fn blob_checks(&self, space: SpaceId) -> Result<BTreeMap<Hash, u64>, NodeError> {
		let mut checks = self.space(space)?.stored.checks()?;
		for (&other, open) in &self.spaces {
			if other == space {
				continue;
			}
			for (sealed, at) in open.stored.checks()? {
				if let Some(checked) = checks.get_mut(&sealed) {
					*checked = (*checked).max(at);
				} else if open.stored.holds_sealed(&sealed) {
					checks.insert(sealed, at);
				}
			}
		}
		Ok(checks)
	}

	/// Adds what this replica holds of a space now to `copies`, on its disk, each copy with
	/// when it was last checked; `plain` is its plain copies there.
	fn add_own_copies(
		&self,
		space: SpaceId,
		plain: &super::checking::PlainState,
		copies: &mut Copies,
	) -> Result<(), NodeError> {
		let open = self.space(space)?;
		let checks = self.blob_checks(space)?;
		for sealed in open.stored.sealed_blobs()? {
			let checked = checks.get(&sealed).copied().unwrap_or(0);
			copies
				.by_sealed
				.entry(sealed)
				.or_default()
				.push((Copies::OWN, checked));
		}
		for (copy, checked) in plain.copies() {
			copies
				.by_copy
				.entry(copy)
				.or_default()
				.push((Copies::OWN, checked));
		}
		Ok(())
	}

	/// The disks that hold a copy of a file of a space, and how each holds it, as of
	/// `wall_millis`: this replica's, as it holds the file now, and the others' as their
	/// accounts say; a sealed copy before a plain one. A copy last checked more than 8 weeks
	/// before `wall_millis` does not count.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn copies_of(
		&self,
		space: SpaceId,
		file: EntityId,
		wall_millis: u64,
	) -> Result<Vec<FileCopy>, NodeError> {
		let copies = self.copies_by_disk(space, wall_millis)?;
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let file = state.canonical(file)?;
		let content = state
			.current(file, &nibkeep_std::CONTENT)?
			.iter()
			.find_map(|statement| match statement.value {
				Value::Blob(hash) => Some(hash),
				_ => None,
			});
		let members: Vec<EntityId> = state.merged(file)?.into_iter().collect();
		let sealed = self.ordinary_sealed(space, content)?;
		let known = |checked: u64| Some(checked).filter(|checked| *checked > 0);
		let mut found: BTreeMap<usize, CopyHeld> = BTreeMap::new();
		for each in &sealed {
			for (disk, checked) in copies.by_sealed.get(each).into_iter().flatten() {
				let held = found
					.entry(*disk)
					.or_insert(CopyHeld::Sealed { checked: None });
				if let CopyHeld::Sealed { checked: before } = held {
					*before = (*before).max(known(*checked));
				}
			}
		}
		if let Some(content) = content {
			for member in &members {
				for (disk, checked) in copies
					.by_copy
					.get(&(*member, content))
					.into_iter()
					.flatten()
				{
					let held = found
						.entry(*disk)
						.or_insert(CopyHeld::InPlace { checked: None });
					if let CopyHeld::InPlace { checked: before } = held {
						*before = (*before).max(known(*checked));
					}
				}
			}
		}
		let latest = copies.checked_on(&sealed, (&members, content));
		Ok(found
			.into_iter()
			.map(|(disk, held)| FileCopy {
				disk: copies.disks[disk].clone(),
				held,
				counts: latest
					.get(&disk)
					.is_some_and(|checked| *checked >= copies.since),
			})
			.collect())
	}

	/// The sealed hashes of a content's copies that every replica of the space can read: the
	/// ordinary ones whose keys this replica holds.
	pub(crate) fn ordinary_sealed(
		&self,
		space: SpaceId,
		content: Option<Hash>,
	) -> Result<Vec<Hash>, NodeError> {
		let Some(content) = content else {
			return Ok(Vec::new());
		};
		let log = self.space(space)?.stored.log();
		Ok(log
			.blob_keys(&content)
			.filter(|(sealed, _)| !log.is_secret_blob(sealed))
			.map(|(sealed, _)| *sealed)
			.collect())
	}

	/// How the space's changes are held: how many disks hold every change this replica
	/// holds, how many of those are away or archive nodes', and how many of the newest
	/// changes are held on this replica's disk alone. A head of another replica that this
	/// one has not received hides what it builds on, so such a disk counts as holding less.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn change_copies(
		&self,
		space: SpaceId,
		copies: &Copies,
		needs: crate::Needs,
	) -> Result<ChangeCopies, NodeError> {
		self.change_copies_among(space, (copies, needs), NEWEST_LOOKED_AT)
	}

	/// Each other disk's heads of a space, as `copies` give them, that this replica holds.
	fn other_heads_held(
		&self,
		space: SpaceId,
		copies: &Copies,
	) -> Result<BTreeMap<usize, Vec<ChangeId>>, NodeError> {
		let history = self.space(space)?.stored.log().history();
		let mut heads: BTreeMap<usize, Vec<ChangeId>> = BTreeMap::new();
		for (disk, theirs) in &copies.heads {
			if *disk == Copies::OWN {
				continue;
			}
			for head in theirs {
				if history.contains(*head)? {
					heads.entry(*disk).or_default().push(*head);
				}
			}
		}
		Ok(heads)
	}

	/// The disks that hold every change of a space that this replica holds, this replica's
	/// own first: those whose heads, as `copies` give them, are each of this replica's heads
	/// or build on it.
	pub(crate) fn disks_holding_all(
		&self,
		space: SpaceId,
		copies: &Copies,
	) -> Result<BTreeSet<usize>, NodeError> {
		let log = self.space(space)?.stored.log();
		let mine: Vec<ChangeId> = log.heads()?.into_iter().collect();
		let mut holding = BTreeSet::from([Copies::OWN]);
		for (disk, theirs) in &self.other_heads_held(space, copies)? {
			let mut all = true;
			for head in &mine {
				let mut held = false;
				for their in theirs {
					if head == their || log.is_ancestor_of(*head, *their)? {
						held = true;
						break;
					}
				}
				if !held {
					all = false;
					break;
				}
			}
			if all {
				holding.insert(*disk);
			}
		}
		Ok(holding)
	}

	/// How the space's changes are held, as [`Library::change_copies`] says, looking at the
	/// `newest` newest changes to count those held here alone.
	fn change_copies_among(
		&self,
		space: SpaceId,
		(copies, needs): (&Copies, crate::Needs),
		newest_looked_at: usize,
	) -> Result<ChangeCopies, NodeError> {
		let log = self.space(space)?.stored.log();
		let history = log.history();
		let mine: Vec<ChangeId> = log.heads()?.into_iter().collect();
		let heads = self.other_heads_held(space, copies)?;
		let holding = self.disks_holding_all(space, copies)?;
		let (away, archived) = copies.marked(&holding);
		// The newest changes, deepest first, and those of them that other disks hold.
		let mut newest: BTreeSet<ChangeId> = BTreeSet::new();
		let mut queue: BinaryHeap<(u64, ChangeId)> = BinaryHeap::new();
		for head in &mine {
			if let Some(depth) = history.depth(*head)? {
				queue.push((depth, *head));
			}
		}
		let mut floor = u64::MAX;
		let mut more = false;
		while let Some((depth, id)) = queue.pop() {
			if newest.contains(&id) {
				continue;
			}
			// A change beyond those looked at: there may be more held here alone.
			if newest.len() >= newest_looked_at {
				more = true;
				break;
			}
			newest.insert(id);
			floor = floor.min(depth);
			if let Some(change) = history.change(id)? {
				for parent in change.parents() {
					if let Some(depth) = history.depth(*parent)? {
						queue.push((depth, *parent));
					}
				}
			}
		}
		// What other disks hold, down to the depth of the oldest of those changes.
		let mut elsewhere: BTreeSet<ChangeId> = BTreeSet::new();
		let mut walk: Vec<ChangeId> = heads.values().flatten().copied().collect();
		while let Some(id) = walk.pop() {
			let Some(depth) = history.depth(id)? else {
				continue;
			};
			if depth < floor || !elsewhere.insert(id) {
				continue;
			}
			if let Some(change) = history.change(id)? {
				walk.extend(change.parents().iter().copied());
			}
		}
		Ok(ChangeCopies {
			needs,
			disks: holding.len(),
			away,
			archived,
			here_alone: newest.difference(&elsewhere).count(),
			here_alone_more: more,
		})
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn changes_held_here_alone_beyond_those_looked_at_say_there_may_be_more() {
		let mut rng = TestRng::new("changes beyond");
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-accounts-beyond-{}", std::process::id()),
		);
		let mut library = Library::create(
			&directory,
			&crate::NewLibrary::new("Ada", "laptop", "nib"),
			&nibkeep_keys::NewProtection::Passphrase(
				"correct horse",
				nibkeep_crypto::PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			1_760_000_000_000,
			&mut rng,
		)
		.expect("creates");
		let space = library.personal_space();
		for minute in 1..=12 {
			let name = (
				nibkeep_model::fields::NAME,
				Value::Name(nibkeep_model::Name::new(format!("book {minute}"))),
			);
			library
				.create_entity(
					space,
					&[],
					vec![name],
					1_760_000_000_000 + minute * 60_000,
					&mut rng,
				)
				.expect("adds");
		}
		let copies = library
			.copies_by_disk(space, 1_760_000_000_000 + 13 * 60_000)
			.expect("counts");
		let all = library
			.change_copies_among(space, (&copies, crate::Needs::CRITICAL), 1_000)
			.expect("counts");
		assert!(all.here_alone > 12 && !all.here_alone_more, "{all:?}");
		let some = library
			.change_copies_among(space, (&copies, crate::Needs::CRITICAL), 5)
			.expect("counts");
		assert_eq!((some.here_alone, some.here_alone_more), (5, true));
		let just = library
			.change_copies_among(space, (&copies, crate::Needs::CRITICAL), all.here_alone)
			.expect("counts");
		assert_eq!(
			(just.here_alone, just.here_alone_more),
			(all.here_alone, false)
		);
		drop(library);
		let _ = std::fs::remove_dir_all(directory);
	}
}
