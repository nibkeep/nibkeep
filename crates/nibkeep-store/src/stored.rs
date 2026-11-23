//! A space's log and sealed blobs, kept on disk.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use getrandom::SysRng;
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::{CryptoRng, UnwrapErr};
use nibkeep_crypto::{BlobReader, Key, SigningKey, seal_blob};
use nibkeep_log::{
	ChangeId, History, Kept, Kind, Log, LogError, Parcel, Payload, Received, SealedPayload,
	SpaceKeys, WrappedBlobKey, WrappedChangeKey, WrappedKey,
};
use nibkeep_model::{FieldName, Operation, Value, fields};
use nibkeep_time::Clock;

use crate::blobs::BlobFiles;
use crate::file::{KeptRow, LogFile, PlaceCheck, StoredPage, StoredPageId, Writes};
use crate::index::{self, Fields, Postings};
use crate::lock::SpaceLock;
use crate::state_file::{Savepoint, SpecialFields};
use crate::{Found, StateFile, StoreError};

/// The context of the key that seals the state file's own key, derived from the replica's
/// storage key.
const STATE_CONTEXT: &str = "nibkeep 2026-10 state file";

/// A change that begins within this time of the last change saved belongs to a burst, such
/// as an import that saves them one at a time: the state file takes in the changes of a
/// burst in one transaction, which commits when a change comes later than that, when it
/// holds [`PENDING_CHANGES`], when the space is used again more than [`PENDING_FOR`] after
/// the first of them, before a batch begins, and when the space closes ([technical design:
/// the state file]). A change made on its own commits the state file at once, as before, and so does
/// each change while another program has the space open, which would wait meanwhile.
///
/// [technical design: the state file]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#75-the-state-file
const BURST_GAP: Duration = Duration::from_millis(100);
const PENDING_CHANGES: usize = 1_000;
const PENDING_FOR: Duration = Duration::from_secs(1);

/// The file in a space's folder that says that a program waits to make the state file anew
/// alone, after a purge, until the other programs that have the space open close it
/// ([technical design: several programs on one replica]): they see it at their next change, and close the space, which
/// opens again once the state file is made anew.
///
/// [technical design: several programs on one replica]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#78-several-programs-on-one-replica
const REMAKE: &str = "remake";

/// How long a purge waits for the other programs that have the space open to close it.
const PURGE_WAIT: Duration = Duration::from_secs(10);

/// How often a purge looks whether the others closed the space.
const PURGE_LOOK: Duration = Duration::from_millis(50);

/// How many times a change catches up with what others wrote before it gives up, as when
/// another program writes all the while.
const CATCH_UP_TRIES: usize = 10;

/// Where a stored log reads the time that tells the changes of a burst from changes made one
/// by one: the system's monotonic clock, unless a test sets another, so that the same
/// changes form the same bursts on every machine, however fast.
#[derive(Clone)]
pub struct Ticks(Arc<dyn Fn() -> Instant + Send + Sync>);

impl Ticks {
	/// The system's monotonic clock.
	#[must_use]
	pub fn system() -> Self {
		Self(Arc::new(Instant::now))
	}

	/// A clock that `now` reads.
	#[must_use]
	pub fn from_fn(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
		Self(Arc::new(now))
	}

	/// The time now.
	fn now(&self) -> Instant {
		(self.0)()
	}
}

impl std::fmt::Debug for Ticks {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("Ticks")
	}
}

/// Randomness for the state file: its key, and the nonces of what is written to it.
fn state_rng() -> UnwrapErr<SysRng> {
	UnwrapErr(SysRng)
}

/// The path of a space's log file in a replica's data directory.
#[must_use]
pub fn log_path(root: &Path, space: SpaceId) -> PathBuf {
	root.join("spaces")
		.join(space.to_string())
		.join("log.sqlite")
}

/// A space's log, kept on disk: what changes the log is saved before the call returns,
/// and opening the file builds the log again. The space's sealed blobs are kept in the
/// replica's directory `blobs`, which its spaces share, and written through the directory
/// `tmp` beside the log file.
pub struct StoredLog {
	blobs: BlobFiles,
	/// The log, whose history is the log file, and whose state is kept in the state file
	/// beside it.
	log: Log<StateFile, LogFile>,
	/// The key that seals the state file's own key, to make the file again.
	state_key: Key,
	/// The fields the indexes in the state file were built with, as they were last read or
	/// written, if they were built.
	index_built_with: Option<SpecialFields>,
	max_drift_millis: u64,
	secret_fields: BTreeSet<FieldName>,
	/// The fields whose last writer wins, of which search finds only the value written
	/// last.
	last_writer_wins: BTreeSet<FieldName>,
	/// Whether the person locked the secrets, so that building the log again must not open
	/// them.
	locked: bool,
	/// Whether a purge still waits for the write-ahead log to be truncated.
	unshredded: bool,
	/// Whether the log can be used, or is behind or broken.
	standing: Standing,
	/// How many times the log was built again from the files since the space was opened.
	reloads: u64,
	/// The sealed hashes of the blobs some of whose stored wrapped keys turned out to be
	/// bad when the space was opened, and left the disk.
	dropped_at_open: Vec<Hash>,
	/// The batch of changes that is open, if one is.
	batch: Option<Batch>,
	/// The changes of a burst that the state file's transaction holds, saved to the log file
	/// and not committed to the state file yet, if any. Reading the log commits them once
	/// they have waited long enough, so they are kept where reading can change them.
	pending: Cell<Option<Pending>>,
	/// Whether a read is in progress, in which no burst commits: the read sees one state.
	reading: Cell<bool>,
	/// When a change was last saved, which tells a burst from changes made one by one.
	last_saved: Option<Instant>,
	/// When the change being made began, if one is: within [`BURST_GAP`] of the last change
	/// saved, it belongs to a burst.
	change_began: Option<Instant>,
	/// Where the times of bursts are read.
	ticks: Ticks,
	/// The file that says that a program waits to make the state file anew.
	remake: PathBuf,
	/// The two files' counts of what other connections committed, as this stored log last
	/// caught up with them: when either moves, another program, or another stored log of
	/// the space, wrote to it.
	seen: (i64, i64),
	/// What the replica keeps apart of what purges destroy, if it waits before purging.
	waiting: Waiting,
	/// The lock on the space, held shared while the space is open here, and alone while
	/// this stored log must run alone. It is the last field, so that it is released after
	/// everything else is dropped, such as a damaged state file that is deleted then.
	lock: SpaceLock,
}

/// How a stored log keeps apart what purges destroy, on a replica that waits before purging
/// ([technical design: copies that wait]).
///
/// [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait
#[derive(Clone, Copy, Debug, Default)]
struct Waiting {
	/// Whether the replica waits, so keeps it apart.
	keeping: bool,
	/// The wall clock time that the call under way gave, if it took one, in milliseconds
	/// since 1970: when a purge that it takes in is received. Saving what it destroyed
	/// takes it.
	last_wall: Option<u64>,
	/// Whether a change that restores came to count since what this replica keeps apart was
	/// last given back, so that [`StoredLog::take_back_kept`] has something to do.
	restored: bool,
}

/// Whether a stored log can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
	/// It can.
	Usable,
	/// The log could not be built again, as the state file must be made anew, which only a
	/// program that has the space alone may do: the log is empty, and the space is to be
	/// closed, and opened again once the others close it too. The next change tries again.
	Behind,
	/// Saving failed and building the log again from the file failed too: the log is empty
	/// until the space is opened again.
	Broken,
}

/// Changes of a burst that the state file holds and has not committed yet.
#[derive(Clone, Copy)]
struct Pending {
	/// How many changes.
	changes: usize,
	/// When the first was saved.
	since: Instant,
	/// Whether the change being made has a savepoint in the transaction.
	savepoint: bool,
	/// Whether the transaction that held the changes ended already, in an error of the
	/// state file that was not damage: the log file holds the changes, and the state file
	/// takes them in when the log is built again from the files, before the next change.
	lost: bool,
}

impl Drop for StoredLog {
	fn drop(&mut self) {
		self.commit_pending();
		// A damaged state file is deleted as it goes, but only by a program that has the
		// space alone.
		if self.postings().is_damaged() && !self.lock.alone() {
			self.postings().keep();
		}
	}
}

/// A batch of changes that is open ([technical design: saving in batches]): one transaction of each file
/// spans it, and each change in it has a savepoint of its own.
///
/// [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches
struct Batch {
	/// The keys to build the log again with, if the files fail.
	keys: SpaceKeys,
	/// Whether the files failed, which left out every change of the batch.
	lost: bool,
	/// Whether a change received in the batch purged: once the batch is saved, the
	/// write-ahead log is truncated, and the state file written anew if the purge took away
	/// what it holds.
	purged: bool,
	/// The sealed hashes of the blobs whose files are to go once the batch is saved, as its
	/// purges destroyed their keys.
	released: Vec<Hash>,
}

/// What reading a sealed blob's file in full against its sealed hash found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedCheck {
	/// It holds its sealed hash; its size in bytes.
	Intact(u64),
	/// It holds other bytes, read twice.
	Damaged,
	/// The replica does not hold it, as when a purge deleted it meanwhile.
	Gone,
	/// The check stopped before the end, as asked.
	Stopped,
}

/// The files in a replica's blob directory, by what a space's log knows of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlobInventory {
	/// The content hashes of the space's blobs that the replica holds a copy of and can
	/// read.
	pub held: Vec<Hash>,
	/// How many of the space's sealed files have keys that the replica cannot open yet,
	/// such as keys of a space key it has not received.
	pub waiting: usize,
	/// The sealed hashes of the files that no key of the space or of the replica's other
	/// spaces opens, and no purge explains: unless a key arrives, their content is lost.
	pub keyless: Vec<Hash>,
	/// Files that are not sealed blobs at all.
	pub unknown: Vec<PathBuf>,
}

impl StoredLog {
	/// Opens a space's log file, creating it and its directory if needed, as the log's
	/// history, which the log reads as it needs it, checking each change it reads against
	/// the tag that `storage`, the replica's storage key, makes. The state is in the state
	/// file beside the log file, which takes in the changes it does not cover yet, and is
	/// made again if it cannot be used, as the storage specification says in section 5. The
	/// fields that the state file's indexes were built with, secret and whose last writer
	/// wins, are the space's special fields until [`StoredLog::set_fields`] says otherwise,
	/// unless `secret_fields` holds a field that they do not: the space's types made them
	/// special when it was last open. Holds a shared lock on the space until the stored log
	/// is dropped, so that several programs, and several stored logs in one, have it open at
	/// once, each catching up with what the others write before it writes
	/// ([technical design: several programs on one replica]). Unless another has the space open, finishes what a crash
	/// interrupted: puts blobs whose keys were saved in place, and deletes the other files
	/// that were being written; and deletes the files of purged blobs that no other space of
	/// the replica keeps a key for ([technical design: blobs]).
	///
	/// [technical design: several programs on one replica]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#78-several-programs-on-one-replica
	/// [technical design: blobs]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#79-blobs
	///
	/// # Errors
	///
	/// Returns [`StoreError::InUse`] if another program has the space to itself for a moment,
	/// or waits to, or the state file must be made anew while another has the space open;
	/// and an error if the file cannot be opened, is not a log file of this version, or is
	/// damaged.
	pub fn open(
		path: &Path,
		space: SpaceId,
		keys: &SpaceKeys,
		storage: &Key,
		max_drift_millis: u64,
		secret_fields: impl IntoIterator<Item = FieldName>,
	) -> Result<Self, StoreError> {
		if let Some(directory) = path.parent() {
			fs::create_dir_all(directory)?;
		}
		let lock = SpaceLock::shared(&path.with_file_name("lock"))?;
		// Alone, a stored log puts the space in order after a crash, and may make the state
		// file anew; while others have the space open, it takes the space as they keep it.
		let alone = lock.alone();
		let remake = path.with_file_name(REMAKE);
		if !alone && remake.exists() {
			return Err(StoreError::InUse);
		}
		let file = LogFile::open(path, storage)?;
		let blobs = BlobFiles::of_log(path, space);
		let secret_fields: BTreeSet<FieldName> = secret_fields.into_iter().collect();
		let state_key = storage.derive(STATE_CONTEXT);
		let state = open_state(&path.with_file_name("state.sqlite"), &state_key, alone)?;
		// Before the log is built, so that what others write meanwhile is caught up with
		// later.
		let seen = (file.data_version()?, state.data_version()?);
		// The fields the indexes were built with, which the space's types made special last
		// time, count until the space says otherwise, unless more fields are secret now.
		let (secret_fields, last_writer_wins) = match state.indexed_fields() {
			Ok(Some((secret, last))) if secret_fields.is_subset(&secret) => (secret, last),
			_ => (secret_fields, BTreeSet::new()),
		};
		let mut log = Log::with_history(space, Clock::new(max_drift_millis), state, file);
		log.set_secret_fields(secret_fields.iter().cloned());
		let fields = Fields {
			secret: &secret_fields,
			last_writer_wins: &last_writer_wins,
		};
		let dropped = build(
			&mut log,
			keys,
			max_drift_millis,
			&state_key,
			fields,
			&|| alone,
		)?;
		let mut stored = Self {
			remake,
			seen,
			lock,
			blobs,
			log,
			state_key,
			index_built_with: None,
			max_drift_millis,
			secret_fields,
			last_writer_wins,
			locked: false,
			unshredded: false,
			standing: Standing::Usable,
			reloads: 0,
			dropped_at_open: dropped.clone(),
			batch: None,
			pending: Cell::new(None),
			reading: Cell::new(false),
			last_saved: None,
			change_began: None,
			ticks: Ticks::system(),
			waiting: Waiting::default(),
		};
		stored.note_index();
		stored.recover_blobs(dropped)?;
		if stored.lock.is_alone() {
			// Alone, nothing was missed, and the state file may have been made anew, with a
			// count of its own.
			stored.seen = stored.versions()?;
			let _ = fs::remove_file(&stored.remake);
		}
		stored.lock.share();
		Ok(stored)
	}

	/// Catches up with what other programs, or other stored logs of the space in this one,
	/// wrote to the space since this one last read it, if they did: builds the log again
	/// from the files, which takes in what they wrote. Returns whether it did. A change
	/// catches up by itself before it writes; a caller that keeps what it read of the space,
	/// such as its types, catches up first to read them again.
	///
	/// # Errors
	///
	/// Returns [`StoreError::InUse`] if the state file must be made anew while another
	/// program has the space open, and an error if the files cannot be read.
	pub fn catch_up(&mut self, keys: &SpaceKeys) -> Result<bool, StoreError> {
		// A batch holds both files' write locks: nobody else wrote.
		if self.batch.is_some() {
			return Ok(false);
		}
		if self.standing != Standing::Behind {
			self.check_usable()?;
			if self.versions()? == self.seen {
				return Ok(false);
			}
		}
		self.commit_pending();
		self.reload(keys);
		self.check_usable()?;
		Ok(true)
	}

	/// How many times the log was built again from the files since the space was opened,
	/// as after another program wrote to it, or saving failed.
	#[must_use]
	pub const fn reloads(&self) -> u64 {
		self.reloads
	}

	/// The two files' counts of what other connections committed.
	fn versions(&self) -> Result<(i64, i64), StoreError> {
		Ok((self.file().data_version()?, self.postings().data_version()?))
	}

	/// Has the space's first change govern it, as [`Log::expect_governed`] says: a change
	/// without parents that does not is rejected.
	pub fn expect_governed(&mut self) {
		self.log.expect_governed();
	}

	/// Takes as the space's first change only one of `roots`, as [`Log::expect_roots`] says.
	pub fn expect_roots(&mut self, roots: impl IntoIterator<Item = ChangeId>) {
		self.log.expect_roots(roots);
	}

	/// Takes as the space's first change one of `roots` too, as [`Log::expect_roots_too`]
	/// says.
	pub fn expect_roots_too(&mut self, roots: impl IntoIterator<Item = ChangeId>) {
		self.log.expect_roots_too(roots);
	}

	/// Reads the times of bursts of changes from `ticks` from now on, instead of the
	/// system's monotonic clock: tests give a clock of their own, so that the same changes
	/// form the same bursts on every machine. Times read from the clock before are not
	/// compared with those of the new one: the next change begins anew, and commits a burst
	/// pending from before, whose second is counted again from now.
	pub fn set_ticks(&mut self, ticks: Ticks) {
		self.last_saved = None;
		if let Some(mut pending) = self.pending.get() {
			pending.since = ticks.now();
			self.pending.set(Some(pending));
		}
		self.ticks = ticks;
	}

	/// How many changes saved to the log file the state file holds in a transaction that
	/// has not committed yet: those of a burst, which commit together.
	#[must_use]
	pub fn pending_changes(&self) -> usize {
		self.pending.get().map_or(0, |pending| pending.changes)
	}

	/// Commits the changes of a burst that the state file holds, if any, as closing the
	/// space does: for a program that made a few changes at once and stays open, so that
	/// another program can open the space at once. Nothing is committed during a batch, a
	/// change or a read, which sees one state. A failure leaves the state file behind the log
	/// file, which the next change, or opening the space again, takes in.
	pub fn end_burst(&mut self) {
		if self.change_began.is_none() && !self.reading.get() {
			self.commit_pending();
		}
	}

	/// Commits the changes of a burst that the state file holds, if any, when the space
	/// closes. A change half made, which the log file never got, such as one a panic cut
	/// short, is left out first. A failure leaves the state file behind the log file, which
	/// opening the space again takes in.
	fn commit_pending(&mut self) {
		let Some(pending) = self.pending.take() else {
			return;
		};
		if self.batch.is_some() || pending.lost || !self.postings().in_transaction() {
			return;
		}
		if pending.savepoint {
			let _ = self
				.log
				.storage_mut()
				.rollback_to_savepoint(Savepoint::Change);
		}
		let _ = self.log.storage_mut().commit();
	}

	/// The log. After a failure that left the log file unreadable, it is empty. On the way,
	/// the changes of a burst that have waited a second commit to the state file, so that a
	/// burst that stopped does not hold them long once the space is used again.
	#[must_use]
	pub fn log(&self) -> &Log<StateFile, LogFile> {
		self.settle();
		&self.log
	}

	/// Commits the changes of a burst that have waited [`PENDING_FOR`], unless a change is
	/// being made. If that fails, the log file holds them, and the next change has the log
	/// built again from the files, which takes them in.
	fn settle(&self) {
		if let Some(mut pending) = self.pending.get()
			&& !pending.savepoint
			&& !pending.lost
			&& !self.reading.get()
			&& self.change_began.is_none()
			&& self.batch.is_none()
			&& self.ticks.now().duration_since(pending.since) >= PENDING_FOR
			&& self.postings().in_transaction()
		{
			if self.postings().commit().is_ok() {
				self.pending.set(None);
			} else {
				pending.lost = true;
				self.pending.set(Some(pending));
			}
		}
	}

	/// Commits a change, as [`Log::commit`] does, and saves it.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::commit`], or an error if saving fails, in which case
	/// the change is not committed.
	pub fn commit(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		operations: Vec<Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, StoreError> {
		// A change that governs may set aside or bring back purges, which delete files at
		// once, so it is made outside a batch.
		if self.batch.is_some() {
			for operation in &operations {
				if self.log.governs(operation)? {
					return Err(StoreError::InBatch);
				}
			}
		}
		// A copy for a second try, if the first finds the state file damaged.
		let again = operations.clone();
		let mut first = Some(operations);
		self.changing(keys, |stored| {
			let operations = first.take().unwrap_or_else(|| again.clone());
			stored.waiting.last_wall = Some(wall_millis);
			let parcel = stored
				.log
				.commit(author, keys, operations, wall_millis, rng)?;
			let applied = stored.log.take_applied();
			let sealed: Vec<StatementId> = parcel.change.sealed().keys().copied().collect();
			let mut writes = Writes {
				wrapped_keys: stored.key_sets(sealed),
				changes: vec![parcel.clone()],
				..Writes::default()
			};
			// A change that governs may have set aside purges, or brought one back.
			let purged_blobs = stored.save_destroyed(&mut writes, &applied, keys)?;
			stored.remove_blob_files(&purged_blobs);
			Ok(parcel)
		})
	}

	/// Begins a batch of changes, which [`StoredLog::end_batch`] saves together
	/// ([technical design: saving in batches]): until then, the changes that [`StoredLog::commit`] makes are
	/// read here, but not saved, and so are the changes and record keys that
	/// [`StoredLog::receive`] and [`StoredLog::receive_wrapped_key`] receive. A change that
	/// is refused leaves the others of the batch. If the files fail, the log is built again
	/// from them with `keys`, and every change of the batch is left out. Nothing else that
	/// writes, such as a purge, a change that governs, or receiving a blob or its key, is
	/// done while a batch is open.
	///
	/// [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches
	///
	/// # Errors
	///
	/// Returns [`StoreError::InBatch`] if a batch is open already, or an error if the files
	/// fail.
	pub fn begin_batch(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		self.check_not_in_batch()?;
		if self.standing == Standing::Behind || self.postings().is_damaged() || self.pending_lost()
		{
			self.reload(keys);
		}
		self.check_usable()?;
		self.build_index()?;
		// The changes of a burst commit before the batch, which commits as a whole.
		if self.pending.get().is_some() {
			self.commit_state(keys)?;
		}
		self.begin_writing(keys)?;
		if let Err(error) = self.log.storage_mut().begin() {
			let _ = self.file_mut().rollback();
			return Err(error);
		}
		self.batch = Some(Batch {
			keys: keys.clone(),
			lost: false,
			purged: false,
			released: Vec::new(),
		});
		Ok(())
	}

	/// Whether a batch of changes is open.
	#[must_use]
	pub const fn in_batch(&self) -> bool {
		self.batch.is_some()
	}

	/// Whether a change received in the open batch purged. What it destroyed leaves the
	/// disk once the batch ends, so a caller ends the batch soon.
	#[must_use]
	pub fn batch_purged(&self) -> bool {
		self.batch.as_ref().is_some_and(|batch| batch.purged)
	}

	/// Whether another stored log, here or in another program, has the space open now: it
	/// would wait for the write locks while a batch holds them.
	#[must_use]
	pub fn shared_with_others(&self) -> bool {
		self.lock.shared_with_others()
	}

	/// Ends the batch of changes, and saves them: commits the log file, which is then
	/// synchronized, then the state file. If only the state file fails, the changes are
	/// saved, and the state file takes them in from the log file. Once saved, what the
	/// batch's purges destroyed leaves the disk: the files of their blobs go, the
	/// write-ahead log is truncated, and the state file is written anew if they took away
	/// what it holds, as after a single change that purges.
	///
	/// # Errors
	///
	/// Returns [`StoreError::NoBatch`] if no batch is open, [`StoreError::BatchLost`] if the
	/// files failed while it was, or an error if saving fails, in which case none of its
	/// changes is saved.
	pub fn end_batch(&mut self) -> Result<(), StoreError> {
		let batch = self.batch.take().ok_or(StoreError::NoBatch)?;
		if batch.lost {
			return Err(StoreError::BatchLost);
		}
		if !self.both_in_transaction() {
			// A read found a file failing, which ended its transaction.
			self.reload(&batch.keys);
			return Err(StoreError::BatchLost);
		}
		if let Err(error) = self.file_mut().commit() {
			self.reload(&batch.keys);
			return Err(error);
		}
		// The log file holds the batch from here on: a state file that fails is built again
		// from it, as after a single change.
		self.pending.set(None);
		self.release_blob_files(&batch.released);
		let saved = if self.log.take_stale() {
			self.remake_state(&batch.keys);
			if self.standing == Standing::Broken {
				Err(StoreError::Broken)
			} else {
				Ok(())
			}
		} else if self.mark_state().is_err() || self.log.storage_mut().commit().is_err() {
			self.reload(&batch.keys);
			self.check_usable()
		} else {
			Ok(())
		};
		if batch.purged || self.unshredded {
			self.unshredded = !self.file().truncate_wal().unwrap_or(false);
		}
		saved
	}

	/// Leaves out every change of the batch that is open, if one is, and builds the log
	/// again from the files: for a batch that cannot end, such as when the program that
	/// made it panicked.
	pub fn abandon_batch(&mut self) {
		if let Some(batch) = self.batch.take()
			&& !batch.lost
		{
			self.reload(&batch.keys);
		}
	}

	/// Receives a change with what came of its payload and key, as [`Log::receive`] does,
	/// and saves what the log keeps. Waiting changes that are rejected for good on the way
	/// leave the file. In a batch, it is saved with the batch, and what a purge destroys
	/// leaves the disk once the batch is saved, as [`StoredLog::end_batch`] says; a change
	/// that governs is received outside a batch, as [`StoredLog::commit`] makes one, and so
	/// is a change that waiting changes wait for, which may govern.
	///
	/// # Errors
	///
	/// Returns [`StoreError::InBatch`] for a change that governs, or that waiting changes
	/// wait for, while a batch is open; the errors of [`Log::receive`]; or an error if saving
	/// fails, in which case the change is not kept.
	pub fn receive(
		&mut self,
		parcel: &Parcel,
		keys: &SpaceKeys,
		wall_millis: u64,
	) -> Result<Received, StoreError> {
		if self.batch.is_some()
			&& (parcel.change.kind() == Kind::Governing || self.log.is_awaited(parcel.id()))
		{
			return Err(StoreError::InBatch);
		}
		let received = self.changing(keys, |stored| {
			stored.receive_saving(parcel, keys, wall_millis)
		})?;
		Ok(received?)
	}

	/// Receives the sealed payload of a change, as [`Log::receive_payload`] does, and saves
	/// it if the log keeps it.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::receive_payload`], or an error if saving fails, in
	/// which case the payload is not kept.
	pub fn receive_payload(
		&mut self,
		id: ChangeId,
		payload: &SealedPayload,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			let kept = stored.log.receive_payload(id, payload.clone(), keys);
			stored.save_part(id, kept, keys)
		})
	}

	/// Receives a wrapped key of a change, as [`Log::receive_change_key`] does, and saves it
	/// if the log keeps it.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::receive_change_key`], or an error if saving fails, in
	/// which case the key is not kept.
	pub fn receive_change_key(
		&mut self,
		id: ChangeId,
		key: &WrappedChangeKey,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			let kept = stored.log.receive_change_key(id, key.clone(), keys);
			stored.save_part(id, kept, keys)
		})
	}

	/// Applies the changes that `keys` let the log read now, as [`Log::open_changes`] does,
	/// for example after the replica received an older space key, and saves what they
	/// changed.
	///
	/// # Errors
	///
	/// Returns an error if the state file cannot be written.
	pub fn open_changes(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			let opened = stored.log.open_changes(keys);
			let applied = stored.log.take_applied();
			opened?;
			// The log file records them applied, and the state file takes them in, with
			// what the governing changes read now set aside or brought back.
			let purged_blobs = stored.save_destroyed(&mut Writes::default(), &applied, keys)?;
			stored.remove_blob_files(&purged_blobs);
			Ok(())
		})
	}

	/// Follows the groups in this space, as the replica's copies of their logs say, as
	/// [`Log::follow`] does, and saves what deciding again changed. A program follows them
	/// each time it opens the space, before it takes or makes changes there.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::follow`], or an error if saving fails.
	pub fn follow(
		&mut self,
		groups: &[nibkeep_governance::Followed],
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			let followed = stored.log.follow(groups.to_vec(), keys);
			let applied = stored.log.take_applied();
			followed?;
			let purged_blobs = stored.save_destroyed(&mut Writes::default(), &applied, keys)?;
			stored.remove_blob_files(&purged_blobs);
			Ok(())
		})
	}

	/// Saves a change whose payload or key the log kept, and what applying it changed, in a
	/// transaction of the state file.
	fn save_part(
		&mut self,
		id: ChangeId,
		kept: Result<bool, LogError>,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		let applied = self.log.take_applied();
		if !kept? {
			return Ok(());
		}
		let mut writes = Writes {
			completed: self.waiting_parcel(id)?.into_iter().collect(),
			..Writes::default()
		};
		let purged_blobs = self.save_destroyed(&mut writes, &applied, keys)?;
		self.remove_blob_files(&purged_blobs);
		Ok(())
	}

	/// Receives a change and saves what changed, in a transaction of the state file. The
	/// log's refusal is returned once what it changed is saved, such as waiting changes that
	/// it rejected for good on the way.
	fn receive_saving(
		&mut self,
		parcel: &Parcel,
		keys: &SpaceKeys,
		wall_millis: u64,
	) -> Result<Result<Received, LogError>, StoreError> {
		let rejected_before: BTreeSet<ChangeId> = self.log.rejected().keys().copied().collect();
		let id = parcel.id();
		self.waiting.last_wall = Some(wall_millis);
		let received = self.log.receive(parcel.clone(), keys, wall_millis);
		let applied = self.log.take_applied();
		if received == Err(LogError::Storage) {
			// Nothing is saved: the log no longer matches the state file.
			return Err(LogError::Storage.into());
		}
		let rejected: Vec<ChangeId> = self
			.log
			.rejected()
			.keys()
			.filter(|id| !rejected_before.contains(id))
			.copied()
			.collect();
		let mut writes = Writes {
			rejected,
			..Writes::default()
		};
		match &received {
			Err(_) | Ok(Received::Duplicate) => {}
			Ok(Received::Completed) => writes.completed.extend(self.waiting_parcel(id)?),
			Ok(Received::Waiting | Received::Accepted(_)) => {
				writes.changes.extend(self.log.parcel(id)?);
			}
		}
		let purged_blobs = self.save_destroyed(&mut writes, &applied, keys)?;
		self.remove_blob_files(&purged_blobs);
		Ok(received)
	}

	/// Saves `writes`, what applying changes did, and the deletion of what purges destroyed
	/// in the last call to the log: only purges of changes that count, which the log says,
	/// and not those it set aside. Returns the sealed hashes of the blobs whose files are to
	/// go, once saved.
	fn save_destroyed(
		&mut self,
		writes: &mut Writes,
		applied: &[(ChangeId, Payload)],
		keys: &SpaceKeys,
	) -> Result<Vec<Hash>, StoreError> {
		let destroyed = self.log.take_destroyed();
		let purges = !destroyed.is_empty();
		// What a replica that waits keeps apart is saved with the purge that destroyed it, and
		// when it came: the time that the call which brought it gave, or else now.
		let received = self.waiting.last_wall.take().unwrap_or_else(wall_now);
		for (purge, part) in destroyed.kept {
			writes.kept.push(KeptRow {
				purge,
				part,
				received,
			});
		}
		self.waiting.restored |= !destroyed.restored.is_empty();
		for statement in &destroyed.statements {
			writes.wrapped_keys.push((*statement, Vec::new()));
		}
		for sealed in &destroyed.blobs {
			writes.blob_keys.push((*sealed, Vec::new()));
			writes.unheld.push(*sealed);
		}
		self.take_in(applied, keys)?;
		self.save(writes, purges, keys)?;
		Ok(destroyed.blobs)
	}

	/// What SQLite's own check of the log file finds wrong: nothing if it is sound.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn integrity_problems(&self) -> Result<Vec<String>, StoreError> {
		self.file().integrity_problems()
	}

	/// The sealed hashes of the blobs some of whose stored wrapped keys turned out to be
	/// bad when the space was opened. Opening removed them from the disk, since no key can
	/// be read from them; another replica may still hold good ones.
	#[must_use]
	pub fn blob_keys_dropped_at_open(&self) -> &[Hash] {
		&self.dropped_at_open
	}

	/// The files in the replica's blob directory, by what the space's log knows of them, and
	/// `elsewhere`, the sealed hashes that the replica's other spaces keep keys for or
	/// purged, as [`StoredLog::known_blobs`] gives them: a file that neither explains is
	/// keyless.
	///
	/// # Errors
	///
	/// Returns an error if the directory cannot be read.
	pub fn blob_inventory(&self, elsewhere: &BTreeSet<Hash>) -> Result<BlobInventory, StoreError> {
		let mut readable: BTreeMap<Hash, Hash> = BTreeMap::new();
		for content in self.log.blobs() {
			for (sealed, _) in self.log.blob_keys(content) {
				readable.insert(*sealed, *content);
			}
		}
		let wrapped: BTreeSet<Hash> = self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect();
		let purged: BTreeSet<Hash> = self.log.purged_blobs()?.into_iter().collect();
		let mut inventory = BlobInventory::default();
		let mut held = BTreeSet::new();
		for file in self.blobs.files()? {
			match file {
				Ok(sealed) => match readable.get(&sealed) {
					Some(content) => {
						held.insert(*content);
					}
					None if wrapped.contains(&sealed) => inventory.waiting += 1,
					None if purged.contains(&sealed) || elsewhere.contains(&sealed) => {}
					None => inventory.keyless.push(sealed),
				},
				Err(path) => inventory.unknown.push(path),
			}
		}
		inventory.held = held.into_iter().collect();
		Ok(inventory)
	}

	/// The sealed hashes that the space keeps wrapped keys for, or purged: those of the files
	/// in the replica's blob directory that the space explains.
	///
	/// # Errors
	///
	/// Returns an error if the log file cannot be read.
	pub fn known_blobs(&self) -> Result<BTreeSet<Hash>, StoreError> {
		let mut known: BTreeSet<Hash> = self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect();
		known.extend(self.log.purged_blobs()?);
		Ok(known)
	}

	/// Finds the entities whose current values hold every word of `query`, compared without
	/// regard to case or accents. The last word also matches the start of a longer word, so
	/// that results appear while typing. Entities with more matching values come first.
	/// Secret fields are never searched.
	///
	/// # Errors
	///
	/// Returns an error if the state file cannot be read.
	pub fn search(&self, query: &str) -> Result<Vec<Found>, StoreError> {
		if self.standing == Standing::Broken {
			return Ok(Vec::new());
		}
		self.check_index()?;
		index::search(self.postings(), query)
	}

	/// The entities, deleted ones included, that a current statement of a field that can
	/// name them names `name`, compared lowercased; of entities merged together, the one
	/// they were merged into. Which of its names an entity shows is for the caller to check.
	/// `None` if the indexes cannot tell: if a field that can name entities is secret on
	/// this replica, or the state file or its indexes cannot be used.
	///
	/// # Errors
	///
	/// Returns an error if the state file or the log file cannot be read.
	pub fn named(&self, name: &str) -> Result<Option<Vec<EntityId>>, StoreError> {
		if self.standing == Standing::Broken
			|| self.secret_fields.iter().any(fields::can_name)
			|| self.check_index().is_err()
		{
			return Ok(None);
		}
		let lowered = name.to_lowercase();
		let mut named: BTreeSet<EntityId> =
			self.postings().with_name(&lowered)?.into_iter().collect();
		// Sealed statements are not indexed: of them, those that this replica opened, kept in
		// memory, can name an entity.
		let state = self.log.state();
		for statement in state.storage().opened() {
			if fields::name_key(&statement.field, &statement.value).as_deref() == Some(&lowered)
				&& state.is_current(statement.id)?
			{
				named.insert(state.canonical(statement.entity)?);
			}
		}
		Ok(Some(named.into_iter().collect()))
	}

	/// Runs `read`, which reads the space, with the state file read in one transaction:
	/// many small reads, such as those of listing entities, then cost less, and all see the
	/// state as it was when the first began.
	///
	/// # Errors
	///
	/// Returns the error of `read`, or an error if the state file fails.
	pub fn reading<T, E: From<StoreError>>(
		&self,
		read: impl FnOnce(&Self) -> Result<T, E>,
	) -> Result<T, E> {
		if self.standing == Standing::Broken {
			return read(self);
		}
		// A burst that stopped commits before the read, not in it; one whose transaction an
		// error ended is noted as lost, so that nothing takes the read's transaction for it.
		self.settle();
		if let Some(mut pending) = self.pending.get()
			&& !self.postings().in_transaction()
		{
			pending.lost = true;
			self.pending.set(Some(pending));
		}
		// A read within a read leaves the outer one reading.
		let was_reading = self.reading.replace(true);
		let result = self.postings().reading(|| read(self));
		self.reading.set(was_reading);
		result
	}

	/// The entities whose field `field` has the current value `value`, exactly, ordered by
	/// ID. Secret fields are never searched.
	///
	/// # Errors
	///
	/// Returns an error if the state file cannot be read.
	pub fn find(&self, field: &FieldName, value: &Value) -> Result<Vec<EntityId>, StoreError> {
		if self.standing == Standing::Broken {
			return Ok(Vec::new());
		}
		self.check_index()?;
		index::find(self.postings(), field, value)
	}

	/// Receives a wrapped record key, as [`Log::receive_wrapped_key`] does, and saves the
	/// statement's keys if they changed; in a batch, with the batch.
	///
	/// # Errors
	///
	/// Returns an error if saving fails, in which case the key is not kept.
	pub fn receive_wrapped_key(
		&mut self,
		statement: StatementId,
		wrapped: WrappedKey,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		let receive = |stored: &mut Self| {
			let before = stored.log.wrapped_keys_of(statement).to_vec();
			let received = stored.log.receive_wrapped_key(statement, wrapped, keys);
			if stored.batch.is_some() {
				// A failure of the storage loses the batch.
				received?;
			} else {
				logged(&mut stored.log, received)?;
			}
			if stored.log.wrapped_keys_of(statement) != before.as_slice() {
				let writes = Writes {
					wrapped_keys: stored.key_sets([statement]),
					..Writes::default()
				};
				stored.save(&writes, false, keys)?;
			}
			Ok(())
		};
		if self.batch.is_some() {
			return self.change_in_batch(receive);
		}
		self.apart(keys, receive)
	}

	/// Purges values, as [`Log::purge`] does: deletes from the disk the wrapped keys of the
	/// secret ones, and the sealed payloads and wrapped keys of the changes that asserted
	/// the others, and writes the state file anew under a new key, without what was purged.
	/// [`StoredLog::is_shredded`] tells whether no copy of the keys and payloads is left in
	/// the log file.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::purge`], or an error if saving fails, in which case
	/// nothing is purged.
	pub fn purge(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		statements: &[StatementId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, StoreError> {
		self.purge_with_blobs(author, keys, statements, (&[], false), wall_millis, rng)
	}

	/// Purges values as [`StoredLog::purge`] does, and in the same change the blobs with
	/// the content hashes `contents`, as [`Log::purge_with_blobs`] does: deletes their
	/// wrapped keys from the disk, then the files of their sealed copies.
	///
	/// # Errors
	///
	/// As [`StoredLog::purge`].
	pub fn purge_with_blobs(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		statements: &[StatementId],
		contents: (&[Hash], bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			stored.waiting.last_wall = Some(wall_millis);
			let parcel = stored.log.purge_with_blobs(
				author,
				keys,
				statements.iter().copied(),
				contents,
				wall_millis,
				rng,
			)?;
			let applied = stored.log.take_applied();
			let mut writes = Writes {
				changes: vec![parcel.clone()],
				..Writes::default()
			};
			let purged_blobs = stored.save_destroyed(&mut writes, &applied, keys)?;
			stored.remove_blob_files(&purged_blobs);
			Ok(parcel)
		})
	}

	/// Whether every purged key is gone from the database files. If another program held
	/// the database while purging, copies may remain until the next save or
	/// [`StoredLog::finish_shredding`].
	#[must_use]
	pub const fn is_shredded(&self) -> bool {
		!self.unshredded
	}

	/// Tries again to remove the copies of purged keys from the database files. Returns
	/// whether none is left.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn finish_shredding(&mut self) -> Result<bool, StoreError> {
		self.check_not_in_batch()?;
		if self.unshredded {
			self.unshredded = !self.file().truncate_wal()?;
		}
		Ok(!self.unshredded)
	}

	/// Keeps apart what purges destroy, as a replica that waits before purging does, or no
	/// longer: the sealed payloads and wrapped keys of the changes they purge, the wrapped
	/// keys of their statements and blobs, in the log file, with when the purge was taken in,
	/// and the files of sealed blobs that no other space keeps a key for, in `kept/` in the
	/// replica's folder ([technical design: copies that wait]). What is kept apart is purged
	/// as far as everything else goes: never sent, proved, nor counted as a copy.
	///
	/// [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait
	pub fn keep_destroyed(&mut self, keeping: bool) {
		self.waiting.keeping = keeping;
		self.log.keep_destroyed(keeping);
	}

	/// Whether this stored log keeps apart what purges destroy, as
	/// [`StoredLog::keep_destroyed`] set it.
	#[must_use]
	pub const fn keeps_destroyed(&self) -> bool {
		self.waiting.keeping
	}

	/// What this replica keeps apart of what purges destroyed, by the purge that destroyed
	/// each part, with when it took the purge in.
	///
	/// # Errors
	///
	/// Returns an error if the log file cannot be read.
	pub fn kept(&self) -> Result<Vec<KeptRow>, StoreError> {
		self.file().kept()
	}

	/// Whether this replica keeps anything apart of what purges destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the log file cannot be read.
	pub fn keeps_any(&self) -> Result<bool, StoreError> {
		self.file().keeps_any()
	}

	/// The size of the file of a sealed blob kept apart, if it is.
	#[must_use]
	pub fn kept_blob_size(&self, sealed: &Hash) -> Option<u64> {
		self.blobs.kept_size(sealed)
	}

	/// Stops a purge whose destruction this replica keeps apart, or another copy that waits,
	/// such as a backup, holds: makes a change, signed by `author`, that restores what it
	/// keeps of what the purge destroyed, and `also`, as [`Log::restore_purged`] does, and
	/// gives back what it kept, as [`StoredLog::take_back_kept`] does; what `also` names comes
	/// from that copy. Returns the change, or `None` if nothing of the purge is kept.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::restore_purged`], or an error if saving fails.
	pub fn restore_kept(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		(purge, also): (ChangeId, &nibkeep_log::Restores),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Option<Parcel>, StoreError> {
		self.check_not_in_batch()?;
		let mut restores = also.clone();
		for row in self.kept()? {
			if row.purge != purge {
				continue;
			}
			match row.part {
				Kept::Change(parcel) => {
					restores.changes.insert(parcel.id());
				}
				Kept::RecordKeys(statement, _) => {
					restores.statements.insert(statement);
				}
				Kept::BlobKeys(sealed, _) => {
					restores.blobs.insert(sealed);
				}
			}
		}
		if restores.is_empty() {
			return Ok(None);
		}
		let parcel = self.changing(keys, |stored| {
			stored.waiting.last_wall = Some(wall_millis);
			let parcel =
				stored
					.log
					.restore_purged(author, keys, restores.clone(), wall_millis, rng)?;
			let applied = stored.log.take_applied();
			let mut writes = Writes {
				changes: vec![parcel.clone()],
				..Writes::default()
			};
			let purged = stored.save_destroyed(&mut writes, &applied, keys)?;
			stored.remove_blob_files(&purged);
			Ok(parcel)
		})?;
		self.take_back_kept(keys, wall_millis)?;
		Ok(Some(parcel))
	}

	/// Gives back what this replica keeps apart of what purges destroyed, where a change that
	/// restores it came to count, so that it is no longer purged: a change's sealed payload
	/// and wrapped keys, which apply its operations again, a statement's or a blob's wrapped
	/// keys, and a sealed blob's file, back in place. What is given back is no longer kept
	/// apart. Returns how many parts were given back.
	///
	/// # Errors
	///
	/// Returns an error if the files cannot be read or written.
	pub fn take_back_kept(
		&mut self,
		keys: &SpaceKeys,
		wall_millis: u64,
	) -> Result<usize, StoreError> {
		if !self.keeps_any()? {
			self.waiting.restored = false;
			return Ok(0);
		}
		let mut given = Vec::new();
		for row in self.kept()? {
			let log = &self.log;
			let purged = match &row.part {
				Kept::Change(parcel) => log.is_change_purged(parcel.id())?,
				Kept::RecordKeys(statement, _) => log.history().is_statement_purged(*statement)?,
				Kept::BlobKeys(sealed, _) => log.is_blob_purged(sealed)?,
			};
			if purged {
				continue;
			}
			match &row.part {
				Kept::Change(parcel) => {
					match self.receive(parcel, keys, wall_millis) {
						Ok(_) | Err(StoreError::Log(_)) => {}
						Err(error) => return Err(error),
					}
					// Kept still, unless the log holds its payload again.
					if self.log.sealed_payload(parcel.id())?.is_none() {
						continue;
					}
				}
				Kept::RecordKeys(statement, wrapped) => {
					for wrapped in wrapped {
						self.receive_wrapped_key(*statement, wrapped.clone(), keys)?;
					}
				}
				Kept::BlobKeys(sealed, wrapped) => {
					for wrapped in wrapped {
						self.receive_blob_key(*sealed, wrapped.clone(), keys)?;
					}
					self.blobs.take_back(sealed)?;
				}
			}
			given.push(row.part);
		}
		let count = given.len();
		if count > 0 {
			self.apart(keys, |stored| {
				let writes = Writes {
					unkept: given,
					..Writes::default()
				};
				stored.save(&writes, false, keys)
			})?;
		}
		// Only once all went well: else the next sync tries again.
		self.waiting.restored = false;
		Ok(count)
	}

	/// How long this device waits before purging, from each time on, in milliseconds, in the
	/// order of those times, as the personal space's log file keeps it: the device's own, which
	/// never syncs.
	///
	/// # Errors
	///
	/// Returns an error if the log file cannot be read.
	pub fn waits(&self) -> Result<Vec<(u64, u64)>, StoreError> {
		self.file().waits()
	}

	/// Replaces how long this device waits before purging, from each time on.
	///
	/// # Errors
	///
	/// Returns an error if the log file cannot be written.
	pub fn save_waits(
		&mut self,
		waits: Vec<(u64, u64)>,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		self.apart(keys, |stored| {
			let writes = Writes {
				waits: Some(waits),
				..Writes::default()
			};
			stored.save(&writes, false, keys)
		})
	}

	/// Whether a change that restores came to count since what this replica keeps apart was
	/// last given back.
	#[must_use]
	pub const fn restored_since(&self) -> bool {
		self.waiting.restored
	}

	/// Destroys what this replica keeps apart of what these purges destroyed, as their wait
	/// ended or its owner ended it: deletes it from the log file, which then holds no copy
	/// of it, and the files of their sealed blobs kept apart, but those that another space
	/// keeps apart too; and the files kept apart that no purge of this space or another
	/// keeps. Returns how many parts went.
	///
	/// # Errors
	///
	/// Returns an error if the files cannot be read or written.
	pub fn destroy_kept(
		&mut self,
		purges: &BTreeSet<ChangeId>,
		keys: &SpaceKeys,
	) -> Result<usize, StoreError> {
		self.check_not_in_batch()?;
		let rows = self.kept()?;
		let mut ending = Vec::new();
		let mut staying = BTreeSet::new();
		for row in rows {
			let blob = match &row.part {
				Kept::BlobKeys(sealed, _) => Some(*sealed),
				_ => None,
			};
			if purges.contains(&row.purge) {
				ending.push(row.part);
			} else if let Some(sealed) = blob {
				staying.insert(sealed);
			}
		}
		let count = ending.len();
		if !ending.is_empty() {
			self.apart(keys, |stored| {
				let writes = Writes {
					unkept: ending,
					..Writes::default()
				};
				stored.save(&writes, true, keys)
			})?;
		}
		let gone: Vec<Hash> = self
			.blobs
			.kept_files()
			.into_iter()
			.filter(|sealed| !staying.contains(sealed))
			.collect();
		self.blobs.destroy_kept(&gone)?;
		Ok(count)
	}

	/// Opens the secret values that `keys` allow, as [`Log::unlock_secrets`] does, and
	/// removes from the disk the wrapped keys that turned out to be bad.
	///
	/// # Errors
	///
	/// Returns an error if saving fails.
	pub fn unlock_secrets(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		self.apart(keys, |stored| {
			let before = stored.all_key_sets();
			// Only the keys of secret blobs change as the secrets unlock.
			let blob_keys_before =
				stored.blob_key_counts(stored.log.sealed_with_secret_keys().copied());
			let unlocked = stored.log.unlock_secrets(keys);
			logged(&mut stored.log, unlocked)?;
			stored.locked = false;
			let changed: Vec<StatementId> = before
				.into_iter()
				.filter(|(statement, keys)| {
					stored.log.wrapped_keys_of(*statement) != keys.as_slice()
				})
				.map(|(statement, _)| statement)
				.collect();
			let writes = Writes {
				wrapped_keys: stored.key_sets(changed),
				blob_keys: stored.blob_key_sets(stored.dropped_blob_keys(blob_keys_before)),
				..Writes::default()
			};
			stored.save(&writes, false, keys)
		})
	}

	/// The fields whose values are secret.
	#[must_use]
	pub const fn secret_fields(&self) -> &BTreeSet<FieldName> {
		&self.secret_fields
	}

	/// Changes which fields are secret, for example after a type of the space marked
	/// another field secret: values of them committed from now on are sealed, and search
	/// leaves them out.
	///
	/// # Errors
	///
	/// Returns an error if the state file cannot be read to build the indexes again.
	pub fn set_secret_fields(
		&mut self,
		secret_fields: impl IntoIterator<Item = FieldName>,
	) -> Result<(), StoreError> {
		self.secret_fields = secret_fields.into_iter().collect();
		self.log
			.set_secret_fields(self.secret_fields.iter().cloned());
		self.build_index()
	}

	/// The fields whose last writer wins, of which search finds only the value written last.
	#[must_use]
	pub const fn last_writer_wins_fields(&self) -> &BTreeSet<FieldName> {
		&self.last_writer_wins
	}

	/// Changes which fields are secret, as [`StoredLog::set_secret_fields`] does, and which
	/// show only the value written last, of which search finds only that value; for example
	/// after a type of the space defined such fields. Builds the indexes again once.
	///
	/// # Errors
	///
	/// Returns an error if the state file cannot be read to build the indexes again.
	pub fn set_fields(
		&mut self,
		secret: impl IntoIterator<Item = FieldName>,
		last_writer_wins: impl IntoIterator<Item = FieldName>,
	) -> Result<(), StoreError> {
		self.secret_fields = secret.into_iter().collect();
		self.log
			.set_secret_fields(self.secret_fields.iter().cloned());
		self.last_writer_wins = last_writer_wins.into_iter().collect();
		self.build_index()
	}

	/// Brings the indexes up to date, if the fields that are secret or show the value written
	/// last are not those they were built with: the fields that changed are indexed again,
	/// or everything, if the indexes were never built. If that fails, they are not searched
	/// until they are brought up to date, before the next change, as they may hold values of
	/// fields that became secret.
	fn build_index(&mut self) -> Result<(), StoreError> {
		if self.index_matches() {
			return Ok(());
		}
		let special = (self.secret_fields.clone(), self.last_writer_wins.clone());
		let in_transaction = self.postings().in_transaction();
		let was = self.index_built_with.clone();
		let built = ensure_index(&mut self.log, was.as_ref(), special, in_transaction);
		self.note_index();
		built
	}

	/// Notes the fields the indexes in the state file were built with.
	fn note_index(&mut self) {
		self.index_built_with = self.postings().indexed_fields().ok().flatten();
	}

	/// Whether the indexes were built with the fields that are special now.
	fn index_matches(&self) -> bool {
		matches!(
			&self.index_built_with,
			Some((secret, last)) if *secret == self.secret_fields && *last == self.last_writer_wins
		)
	}

	/// Returns an error if the indexes must not be searched, as they were built with other
	/// fields than those that are special now.
	fn check_index(&self) -> Result<(), StoreError> {
		if self.index_matches() {
			Ok(())
		} else {
			Err(StoreError::State(
				"the search index could not be built again".to_owned(),
			))
		}
	}

	/// The postings of the indexes, in the state file.
	fn postings(&self) -> &StateFile {
		self.log.state().storage().ordinary()
	}

	/// Which fields the space treats specially.
	fn fields(&self) -> Fields<'_> {
		Fields {
			secret: &self.secret_fields,
			last_writer_wins: &self.last_writer_wins,
		}
	}

	/// Forgets the opened secret values, as [`Log::lock_secrets`] does.
	pub fn lock_secrets(&mut self) {
		self.log.lock_secrets();
		self.locked = true;
	}

	/// Seals content as a blob of the space, unless the replica holds it already with an
	/// ordinary key, and returns its content hash. Reads `content` to its end and seals it
	/// with a new blob key into a new file. Saves the key, wrapped with the newest space key,
	/// before the file takes its place, so that every blob in place has its key. Records
	/// `wall_millis` as the time the blob was last checked.
	///
	/// # Errors
	///
	/// Returns an error if reading the content or writing fails, in which case no blob is
	/// added.
	pub fn import_blob(
		&mut self,
		keys: &SpaceKeys,
		content: impl Read,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, StoreError> {
		self.import(keys, (content, false), &|_| Vec::new(), wall_millis, rng)
	}

	/// Seals content as a blob of the space, as [`StoredLog::import_blob`] does, with a
	/// secret key if `secret` says so, as [`StoredLog::import_secret_blob`] does, unless the
	/// replica holds it already for another of its spaces: `elsewhere` gives the sealed
	/// copies of a content that the replica's other spaces hold, with their keys, smallest
	/// sealed hash first, as [`StoredLog::held_copies`] gives them. The first that
	/// [`StoredLog::share_blob`] takes up is the space's copy too, and the content is not
	/// sealed anew ([technical design: blobs]).
	///
	/// [technical design: blobs]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#79-blobs
	///
	/// # Errors
	///
	/// As [`StoredLog::import_blob`] and [`StoredLog::import_secret_blob`].
	pub fn import_blob_sharing(
		&mut self,
		keys: &SpaceKeys,
		(content, secret): (impl Read, bool),
		elsewhere: &dyn Fn(&Hash) -> Vec<(Hash, Key)>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, StoreError> {
		if secret && keys.newest_secrets().is_none() {
			return Err(StoreError::Log(LogError::SecretsLocked));
		}
		self.import(keys, (content, secret), elsewhere, wall_millis, rng)
	}

	/// The sealed copies of a content that the replica holds and can read in the space,
	/// with keys of the kind given, secret or ordinary, smallest sealed hash first, for its
	/// other spaces to take up.
	#[must_use]
	pub fn held_copies(&self, content: &Hash, secret: bool) -> Vec<(Hash, Key)> {
		self.held(content)
			.filter(|(sealed, _)| self.log.is_secret_blob(sealed) == secret)
			.map(|(sealed, key)| (*sealed, key.clone()))
			.collect()
	}

	/// Takes up, for the space, a sealed copy of `content` that the replica holds for
	/// another of its spaces, `copy`, with its key: reads it whole first, as the content
	/// hash a key claims is only a claim, then wraps its key for the space, secret if
	/// `secret` says so, and records `wall_millis` as the time the copy was last checked.
	/// Returns whether the space holds the content now with a key of that kind, which it
	/// may have already. A copy whose sealed hash the space purged is never taken up, nor
	/// one that does not hold the content: the content is then to be sealed anew
	/// ([technical design: blobs]).
	///
	/// [technical design: blobs]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#79-blobs
	///
	/// # Errors
	///
	/// Returns [`StoreError::Log`] with [`LogError::SecretsLocked`] if `secret` and `keys`
	/// hold no secrets key, and another error if the blob lock cannot be taken, or saving
	/// fails.
	pub fn share_blob(
		&mut self,
		keys: &SpaceKeys,
		content: &Hash,
		(sealed, key): (Hash, &Key),
		secret: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		if secret && keys.newest_secrets().is_none() {
			return Err(StoreError::Log(LogError::SecretsLocked));
		}
		if self.held_as(content, secret) {
			return Ok(true);
		}
		if self.log.is_blob_purged(&sealed)? {
			return Ok(false);
		}
		// A copy that cannot be read whole, or does not hold the content, is not taken up.
		let checked = self.blobs.open(&sealed).and_then(|file| {
			BlobReader::new(key, *content, file)
				.and_then(|mut reader| io::copy(&mut reader, &mut io::sink()))
		});
		if checked.is_err() {
			return Ok(false);
		}
		// Under the blob lock, so that no other space deletes the file in between, as it
		// would if it saw no key of this space for it; and once the log has caught up, so
		// that the keys saved replace none that another program saved meanwhile.
		let _lock = self.blobs.lock()?;
		if !self.blobs.holds(&sealed) {
			return Ok(false);
		}
		self.apart(keys, |stored| {
			let added = if secret {
				stored
					.log
					.add_secret_blob_key(*content, sealed, key.clone(), keys, rng)
			} else {
				stored
					.log
					.add_blob_key(*content, sealed, key.clone(), keys, rng)
			};
			logged(&mut stored.log, added)?;
			if !stored.held_as(content, secret) {
				return Ok(false);
			}
			let writes = Writes {
				blob_keys: stored.blob_key_sets([sealed]),
				verified: vec![(sealed, wall_millis)],
				..Writes::default()
			};
			stored.save(&writes, false, keys)?;
			Ok(true)
		})
	}

	/// Seals the content of a secret field as a blob of the space, as
	/// [`StoredLog::import_blob`] seals other content, unless the replica holds it already
	/// with a secret key: its key is wrapped with the newest secrets key inside the newest
	/// space key, so that only a replica that holds the secrets reads it. Content that the
	/// replica holds with an ordinary key only is sealed anew.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Log`] with [`LogError::SecretsLocked`] if `keys` hold no
	/// secrets key, and another error if reading the content or writing fails, in which case
	/// no blob is added.
	pub fn import_secret_blob(
		&mut self,
		keys: &SpaceKeys,
		content: impl Read,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, StoreError> {
		if keys.newest_secrets().is_none() {
			return Err(StoreError::Log(LogError::SecretsLocked));
		}
		self.import(keys, (content, true), &|_| Vec::new(), wall_millis, rng)
	}

	/// Seals content as a blob of the space, with a secret key or an ordinary one, unless it
	/// takes up a copy that `elsewhere` gives.
	fn import(
		&mut self,
		keys: &SpaceKeys,
		content: (impl Read, bool),
		elsewhere: &dyn Fn(&Hash) -> Vec<(Hash, Key)>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		let (temporary, file) = self.blobs.temporary()?;
		let imported = self.import_from(
			&temporary,
			file,
			keys,
			content,
			elsewhere,
			(wall_millis, rng),
		);
		// After a success the file is in place, or was not needed.
		let _ = fs::remove_file(&temporary);
		imported
	}

	/// Whether the replica holds a sealed copy of the content with the hash `content` that it
	/// can read now: with an ordinary key, or with a secret one while the secrets are
	/// unlocked. Whether the content is stored for every replica,
	/// [`StoredLog::holds_ordinary_blob`] tells.
	#[must_use]
	pub fn holds_blob(&self, content: &Hash) -> bool {
		self.held(content).next().is_some()
	}

	/// Whether the replica holds a sealed copy of the content with the hash `content` with
	/// an ordinary key, which every replica of the space can read.
	#[must_use]
	pub fn holds_ordinary_blob(&self, content: &Hash) -> bool {
		self.held_as(content, false)
	}

	/// Whether the replica has opened a secret blob key for the content with the hash
	/// `content`, whether or not it holds the sealed copy yet: whether the content is sealed
	/// for the secrets, as far as this replica knows while its secrets are unlocked.
	#[must_use]
	pub fn has_secret_blob_key(&self, content: &Hash) -> bool {
		self.log
			.blob_keys(content)
			.any(|(sealed, _)| self.log.is_secret_blob(sealed))
	}

	/// Whether the replica holds a sealed copy of the content with a key of the kind given.
	fn held_as(&self, content: &Hash, secret: bool) -> bool {
		self.held(content)
			.any(|(sealed, _)| self.log.is_secret_blob(sealed) == secret)
	}

	/// Opens the content with the hash `content` for reading, from a sealed copy that the
	/// replica holds. The reader can start anywhere, and checks the content hash when it
	/// reads all of the content in order.
	///
	/// # Errors
	///
	/// Returns an error if the replica holds no copy, or the copy is damaged.
	pub fn open_blob(&self, content: &Hash) -> Result<BlobReader<File>, StoreError> {
		let copy = self.checked_last(content)?.ok_or(StoreError::MissingBlob)?;
		BlobReader::new(copy.key, *content, self.blobs.open(copy.sealed)?).map_err(blob_error)
	}

	/// Reads every sealed copy of the content that the replica holds in full, checking the
	/// content hash, and records `wall_millis` as the time each intact copy was last
	/// checked. A replica normally holds one copy of a content, and more only when replicas
	/// sealed it before they learned of each other's keys.
	///
	/// # Errors
	///
	/// Returns an error if the replica holds no copy, a copy is damaged, which is reported
	/// after the intact ones are recorded, or saving fails.
	pub fn verify_blob(&mut self, content: &Hash, wall_millis: u64) -> Result<(), StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		let mut held = false;
		let mut damaged = false;
		let mut verified = Vec::new();
		for (sealed, key) in self.held(content) {
			held = true;
			let checked = BlobReader::new(key, *content, self.blobs.open(sealed)?)
				.and_then(|mut reader| io::copy(&mut reader, &mut io::sink()));
			match checked {
				Ok(_) => verified.push((*sealed, wall_millis)),
				Err(error) if error.kind() == io::ErrorKind::InvalidData => damaged = true,
				Err(error) => return Err(error.into()),
			}
		}
		if !held {
			return Err(StoreError::MissingBlob);
		}
		self.save_records(&Writes {
			verified,
			..Writes::default()
		})?;
		if damaged {
			return Err(StoreError::DamagedBlob);
		}
		Ok(())
	}

	/// When the replica last found a copy of the content intact, in milliseconds since 1970,
	/// or `None` if it holds no copy.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn blob_verified(&self, content: &Hash) -> Result<Option<u64>, StoreError> {
		Ok(self.checked_last(content)?.and_then(|copy| copy.verified))
	}

	/// When the replica last read each sealed blob of the space in full and found it intact,
	/// by sealed hash, in milliseconds since 1970.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn checks(&self) -> Result<BTreeMap<Hash, u64>, StoreError> {
		self.file().checks()
	}

	/// The pages of the replicas' accounts of what they hold in the space that the log file
	/// keeps: the latest of each page of each replica ([technical design: where the copies
	/// are]).
	///
	/// [technical design: where the copies are]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#96-where-the-copies-are
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or a page's row is damaged.
	pub fn account_pages(&self) -> Result<Vec<StoredPage>, StoreError> {
		self.file().account_pages()
	}

	/// The pages of the replicas' accounts that the log file keeps, as
	/// [`StoredLog::account_pages`] lists them, without their encodings.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or a page's row is damaged.
	pub fn account_page_ids(&self) -> Result<Vec<StoredPageId>, StoreError> {
		self.file().account_page_ids()
	}

	/// The pages of the accounts of backups that the log file keeps: the latest of each page
	/// of each backup that each replica issued
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#910-backups)).
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or a page's row is damaged.
	pub fn backup_account_pages(&self) -> Result<Vec<StoredPage>, StoreError> {
		self.file().backup_account_pages()
	}

	/// The same, without their encodings.
	///
	/// # Errors
	///
	/// Returns an error if the database fails, or a page's row is damaged.
	pub fn backup_account_page_ids(&self) -> Result<Vec<StoredPageId>, StoreError> {
		self.file().backup_account_page_ids()
	}

	/// The encoding of the page of an account whose hash is `id`, if the log file keeps it.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn account_page(&self, id: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
		self.file().account_page(id)
	}

	/// Keeps each of these pages of accounts that is later than the one of its replica and
	/// number that the log file keeps, if it keeps one: issued later, or at the same time
	/// with a larger hash. Checking the pages is the caller's. Returns how many it kept.
	///
	/// # Errors
	///
	/// Returns an error if a batch is open, or the database fails, in which case none is
	/// kept.
	pub fn save_account_pages(&mut self, pages: &[StoredPage]) -> Result<usize, StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		if pages.is_empty() {
			return Ok(0);
		}
		// A burst that holds the state file's write lock commits first, so that this program
		// never holds it while it waits for the log file's, which another program may hold
		// while it waits for the state file's.
		self.commit_pending();
		self.file_mut().save_account_pages(pages)
	}

	/// Deletes the replica's ordinary sealed copies of the content, and keeps the keys: for
	/// when the replica no longer needs a copy. A secret file's copy, which no storage policy
	/// chooses for, stays, and so does a copy that another space of the replica keeps a key
	/// for.
	///
	/// # Errors
	///
	/// Returns an error if a file or the record of its checks cannot be deleted.
	pub fn remove_blob(&mut self, content: &Hash) -> Result<(), StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		let held: Vec<Hash> = self
			.held(content)
			.map(|(sealed, _)| *sealed)
			.filter(|sealed| !self.log.is_secret_blob(sealed))
			.collect();
		self.blobs.release(&held)?;
		self.save_records(&Writes {
			unheld: held,
			..Writes::default()
		})
	}

	/// The sealed hashes of the sealed blobs of the space that the replica holds, whether or
	/// not it can read them yet, for other replicas: those it keeps a wrapped key for in the
	/// space, as the replica's other spaces keep theirs in the same directory.
	///
	/// # Errors
	///
	/// Returns an error if the blob directory cannot be read: none now, as each file is
	/// looked up.
	pub fn sealed_blobs(&self) -> Result<Vec<Hash>, StoreError> {
		Ok(self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect::<BTreeSet<Hash>>()
			.into_iter()
			.filter(|sealed| self.blobs.holds(sealed))
			.collect())
	}

	/// Opens a sealed blob of the space that the replica holds as it is stored, for another
	/// replica, with its size in bytes; `None` if it holds none with that sealed hash, or
	/// keeps no key for it in the space.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be opened.
	pub fn open_sealed_blob(&self, sealed: &Hash) -> Result<Option<(u64, File)>, StoreError> {
		if self.log.wrapped_blob_keys_of(sealed).is_empty() {
			return Ok(None);
		}
		let file = match self.blobs.open(sealed) {
			Ok(file) => file,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
			Err(error) => return Err(error.into()),
		};
		let size = file.metadata()?.len();
		Ok(Some((size, file)))
	}

	/// Receives a sealed blob of `size` bytes from another replica, and keeps it if its
	/// bytes have the sealed hash, and the replica neither holds it nor purged it. Its key
	/// comes on its own, before or after: the sealed hash is checked without it, and until
	/// it comes, the space does not pass the blob on. A blob kept replaces a damaged copy of
	/// it kept aside. Returns whether the blob was kept: it was read whole then, which counts
	/// as a check that the caller records ([`StoredLog::record_checks`]).
	///
	/// # Errors
	///
	/// Returns an error if reading `bytes` or writing the file fails, in which case no blob
	/// is added.
	pub fn receive_sealed_blob(
		&mut self,
		sealed: Hash,
		size: u64,
		bytes: impl Read,
	) -> Result<bool, StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		if self.blobs.holds(&sealed) || self.log.is_blob_purged(&sealed)? {
			return Ok(false);
		}
		let (temporary, file) = self.blobs.temporary()?;
		let received = self.receive_into(&temporary, file, &sealed, size, bytes);
		// After a success the file is in place.
		let _ = fs::remove_file(&temporary);
		received
	}

	/// Reads the replica's copy of a sealed blob in full against its sealed hash, as a
	/// regular check does, without a key: again if its bytes differ, so that one bad read is
	/// not taken for damage. `stop` is asked between chunks.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be read, which is no sign of damage.
	pub fn check_sealed_blob(
		&self,
		sealed: &Hash,
		stop: &dyn Fn() -> bool,
	) -> Result<SealedCheck, StoreError> {
		for _ in 0..2 {
			match self.blobs.hash_file(sealed, stop) {
				Ok(None) => return Ok(SealedCheck::Stopped),
				Ok(Some((found, size))) if found == *sealed => {
					return Ok(SealedCheck::Intact(size));
				}
				Ok(Some(_)) => {}
				Err(error) if error.kind() == io::ErrorKind::NotFound => {
					return Ok(SealedCheck::Gone);
				}
				Err(error) => return Err(error.into()),
			}
		}
		Ok(SealedCheck::Damaged)
	}

	/// Moves the replica's damaged copy of a sealed blob aside, out of the blob directory, so
	/// that the replica no longer holds it and a sync brings it again, which then replaces
	/// it ([technical design: checking and repair]). Returns whether it did: not if the
	/// replica no longer holds it.
	///
	/// [technical design: checking and repair]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#97-checking-and-repair
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be moved.
	pub fn set_aside_blob(&self, sealed: &Hash) -> Result<bool, StoreError> {
		Ok(self.blobs.set_aside(sealed)?)
	}

	/// Removes the replica's copies of sealed blobs of the space that it no longer keeps,
	/// though the space keeps their keys, as the planner does once other disks proved their
	/// copies ([technical design: the planner]). A blob that another space of the replica
	/// keeps a key for stays. Returns those removed.
	///
	/// [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner
	///
	/// # Errors
	///
	/// Returns an error if a file cannot be removed; those removed before it are gone.
	pub fn remove_sealed_blobs(&self, sealed: &[Hash]) -> Result<Vec<Hash>, StoreError> {
		let keyed: Vec<Hash> = sealed
			.iter()
			.copied()
			.filter(|sealed| !self.log.wrapped_blob_keys_of(sealed).is_empty())
			.collect();
		Ok(self.blobs.remove(&keyed)?)
	}

	/// Deletes the files of every sealed blob that the space keeps a key for, and their
	/// damaged copies kept aside, but those that another space of the replica keeps a key
	/// for, under the blob lock: as the space leaves the replica, when its device drops it
	/// ([technical design: devices that choose their spaces]).
	///
	/// [technical design: devices that choose their spaces]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces
	///
	/// # Errors
	///
	/// Returns an error if a file cannot be deleted; the others are deleted all the same.
	pub fn release_all_blobs(&self) -> Result<(), StoreError> {
		let sealed: Vec<Hash> = self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect::<BTreeSet<Hash>>()
			.into_iter()
			.collect();
		Ok(self.blobs.release(&sealed)?)
	}

	/// Holds the space's lock alone, if no other stored log, here or in another program,
	/// has the space open: whether it does. It is held alone until this stored log goes, as
	/// when the space leaves the replica.
	#[must_use]
	pub fn hold_alone(&self) -> bool {
		self.lock.alone()
	}

	/// Shares the space's lock again, held alone before, so that other programs open the
	/// space again.
	pub fn share_again(&self) {
		self.lock.share();
	}

	/// Of these sealed blobs, those that another space of the replica keeps a key for, as
	/// its log file says now: all of them if one cannot be read.
	#[must_use]
	pub fn keyed_elsewhere(&self, sealed: &[Hash]) -> BTreeSet<Hash> {
		self.blobs.kept_elsewhere(sealed)
	}

	/// Whether the replica holds a sealed blob, in its blob directory.
	#[must_use]
	pub fn holds_sealed(&self, sealed: &Hash) -> bool {
		self.blobs.holds(sealed)
	}

	/// Whether a damaged copy of a sealed blob is kept aside, waiting for an intact one.
	#[must_use]
	pub fn is_blob_aside(&self, sealed: &Hash) -> bool {
		self.blobs.is_aside(sealed)
	}

	/// The sealed blobs of the space, whose keys it keeps, that the replica no longer holds
	/// as a damaged copy of each is kept aside.
	#[must_use]
	pub fn aside_blobs(&self) -> Vec<Hash> {
		self.blobs
			.aside_files()
			.into_iter()
			.filter(|sealed| {
				!self.log.wrapped_blob_keys_of(sealed).is_empty() && !self.blobs.holds(sealed)
			})
			.collect()
	}

	/// Reads the copy of a sealed blob kept aside in full against its sealed hash, as
	/// [`StoredLog::check_sealed_blob`] reads one in place, and puts it back in place if it
	/// is intact after all, as when the damage was a bad read, unless the replica holds the
	/// blob again. Returns what the read found: [`SealedCheck::Intact`] once it is back.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be read or moved.
	pub fn recheck_aside_blob(
		&self,
		sealed: &Hash,
		stop: &dyn Fn() -> bool,
	) -> Result<SealedCheck, StoreError> {
		match self.blobs.hash_aside(sealed, stop) {
			Ok(None) => Ok(SealedCheck::Stopped),
			Ok(Some((found, size))) if found == *sealed => {
				if self.blobs.restore(sealed)? {
					Ok(SealedCheck::Intact(size))
				} else {
					Ok(SealedCheck::Gone)
				}
			}
			Ok(Some(_)) => Ok(SealedCheck::Damaged),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SealedCheck::Gone),
			Err(error) => Err(error.into()),
		}
	}

	/// Records what checks found: sealed blobs found intact and found damaged, each with
	/// when, and checks of plain copies on this device.
	///
	/// # Errors
	///
	/// Returns an error if a batch is open, or saving fails.
	pub fn record_checks(
		&mut self,
		intact: Vec<(Hash, u64)>,
		damaged: Vec<(Hash, u64)>,
		places: Vec<PlaceCheck>,
	) -> Result<(), StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		if intact.is_empty() && damaged.is_empty() && places.is_empty() {
			return Ok(());
		}
		// A burst that holds the state file's write lock commits first, as for pages of
		// accounts.
		self.commit_pending();
		self.save_records(&Writes {
			verified: intact,
			damaged,
			places,
			..Writes::default()
		})
	}

	/// The sealed blobs of the space that checks found damaged, with when each was last found
	/// so, in milliseconds since 1970; those that came again since are held once more.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn damaged_blobs(&self) -> Result<BTreeMap<Hash, u64>, StoreError> {
		self.file().damaged_blobs()
	}

	/// The latest check of each plain copy on this device.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn place_checks(&self) -> Result<Vec<PlaceCheck>, StoreError> {
		self.file().place_checks()
	}

	/// Records that a file was opened on this device at `at`, in milliseconds since 1970,
	/// outside the history: it decides which files an everyday device keeps, and is never
	/// synced.
	///
	/// # Errors
	///
	/// Returns an error if a batch is open, or saving fails.
	pub fn record_opened(&mut self, file: EntityId, at: u64) -> Result<(), StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		self.commit_pending();
		self.save_records(&Writes {
			opened: vec![(file, at)],
			..Writes::default()
		})
	}

	/// When each file was last opened on this device, by its ID, in milliseconds since 1970.
	///
	/// # Errors
	///
	/// Returns an error if the database fails.
	pub fn opened(&self) -> Result<BTreeMap<EntityId, u64>, StoreError> {
		self.file().opened()
	}

	/// Writes a received sealed blob into a temporary file, and puts it in place if it has
	/// its sealed hash.
	fn receive_into(
		&self,
		temporary: &Path,
		file: File,
		sealed: &Hash,
		size: u64,
		bytes: impl Read,
	) -> Result<bool, StoreError> {
		let mut hasher = nibkeep_base::Hasher::new();
		let mut output = BufWriter::new(file);
		let mut buffer = vec![0; 64 * 1024];
		let mut bytes = bytes.take(size);
		let mut written = 0_u64;
		loop {
			let count = bytes.read(&mut buffer)?;
			if count == 0 {
				break;
			}
			hasher.update(&buffer[..count]);
			io::Write::write_all(&mut output, &buffer[..count])?;
			written += count as u64;
		}
		if written != size || hasher.finalize() != *sealed {
			return Ok(false);
		}
		output
			.into_inner()
			.map_err(io::IntoInnerError::into_error)?
			.sync_all()?;
		let staged = self.blobs.stage(temporary, sealed)?;
		self.blobs.place(&staged, sealed)?;
		Ok(true)
	}

	/// Opens the wrapped blob keys that `keys` allow, as [`Log::open_blob_keys`] does, for
	/// example after the replica received an older space key, and removes from the disk
	/// those that turned out to be bad.
	///
	/// # Errors
	///
	/// Returns an error if saving fails.
	pub fn open_blob_keys(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		self.check_usable()?;
		self.check_not_in_batch()?;
		let all: Vec<Hash> = self
			.log
			.wrapped_blob_keys()
			.map(|(sealed, _)| *sealed)
			.collect();
		let before = self.blob_key_counts(all);
		self.log.open_blob_keys(keys);
		self.save_records(&Writes {
			blob_keys: self.blob_key_sets(self.dropped_blob_keys(before)),
			..Writes::default()
		})
	}

	/// How many wrapped keys the log has for each of these sealed blobs.
	fn blob_key_counts(&self, sealed: impl IntoIterator<Item = Hash>) -> BTreeMap<Hash, usize> {
		sealed
			.into_iter()
			.map(|sealed| (sealed, self.log.wrapped_blob_keys_of(&sealed).len()))
			.collect()
	}

	/// The sealed blobs that the log has fewer wrapped keys for than `before` counted, as it
	/// dropped those that turned out to be bad.
	fn dropped_blob_keys(&self, before: BTreeMap<Hash, usize>) -> Vec<Hash> {
		before
			.into_iter()
			.filter(|(sealed, count)| self.log.wrapped_blob_keys_of(sealed).len() < *count)
			.map(|(sealed, _)| sealed)
			.collect()
	}

	/// Receives a wrapped blob key, as [`Log::receive_blob_key`] does, and saves the blob's
	/// keys if they changed.
	///
	/// # Errors
	///
	/// Returns an error if the replica's blob lock cannot be taken, or saving fails, in which
	/// case the key is not kept.
	pub fn receive_blob_key(
		&mut self,
		sealed: Hash,
		wrapped: WrappedBlobKey,
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		// Under the blob lock, so that another space that purges the blob either sees this
		// key, or deletes the file before it, which the sealed blob that comes after the key
		// then brings back.
		let _lock = self.blobs.lock()?;
		self.apart(keys, |stored| {
			let before = stored.log.wrapped_blob_keys_of(&sealed).to_vec();
			let received = stored.log.receive_blob_key(sealed, wrapped, keys);
			logged(&mut stored.log, received)?;
			if stored.log.wrapped_blob_keys_of(&sealed) != before.as_slice() {
				let writes = Writes {
					blob_keys: stored.blob_key_sets([sealed]),
					..Writes::default()
				};
				stored.save(&writes, false, keys)?;
			}
			Ok(())
		})
	}

	/// Purges a blob, as [`Log::purge_blob`] does: deletes its wrapped keys from the disk,
	/// then the files of its sealed copies. [`StoredLog::is_shredded`] tells whether no copy
	/// of the keys is left. Without its keys a sealed copy is unreadable, so a file that
	/// cannot be deleted now is deleted when the space is opened again.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::purge_blob`], or an error if saving fails, in which
	/// case nothing is purged.
	pub fn purge_blob(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		content: &Hash,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, StoreError> {
		self.check_not_in_batch()?;
		self.changing(keys, |stored| {
			stored.waiting.last_wall = Some(wall_millis);
			let parcel = stored
				.log
				.purge_blob(author, keys, content, wall_millis, rng)?;
			let applied = stored.log.take_applied();
			let mut writes = Writes {
				changes: vec![parcel.clone()],
				..Writes::default()
			};
			let purged = stored.save_destroyed(&mut writes, &applied, keys)?;
			stored.remove_blob_files(&purged);
			Ok(parcel)
		})
	}

	/// Seals content into a temporary file, and puts it in place unless the space holds the
	/// content already, or takes up a copy that another of the replica's spaces holds.
	fn import_from(
		&mut self,
		temporary: &Path,
		file: File,
		keys: &SpaceKeys,
		(content, secret): (impl Read, bool),
		elsewhere: &dyn Fn(&Hash) -> Vec<(Hash, Key)>,
		(wall_millis, rng): (u64, &mut impl CryptoRng),
	) -> Result<Hash, StoreError> {
		// A new key for every sealing: a key that another replica sent may not belong to the
		// content it claims.
		let key = Key::generate(rng);
		let mut output = BufWriter::new(file);
		let sealed = seal_blob(&key, content, &mut output, rng)?;
		if self.held_as(&sealed.content, secret) {
			return Ok(sealed.content);
		}
		for (copy, copy_key) in elsewhere(&sealed.content) {
			let shared = self.share_blob(
				keys,
				&sealed.content,
				(copy, &copy_key),
				secret,
				wall_millis,
				rng,
			)?;
			if shared {
				return Ok(sealed.content);
			}
		}
		output
			.into_inner()
			.map_err(io::IntoInnerError::into_error)?
			.sync_all()?;
		// Named after its sealed hash, the file is put in place when the space is opened
		// again if a crash comes after its key is saved.
		let staged = self.blobs.stage(temporary, &sealed.sealed)?;
		let added = if secret {
			self.log
				.add_secret_blob_key(sealed.content, sealed.sealed, key, keys, rng)
		} else {
			self.log
				.add_blob_key(sealed.content, sealed.sealed, key, keys, rng)
		};
		if let Err(error) = logged(&mut self.log, added) {
			let _ = fs::remove_file(&staged);
			return Err(error);
		}
		let writes = Writes {
			blob_keys: self.blob_key_sets([sealed.sealed]),
			verified: vec![(sealed.sealed, wall_millis)],
			..Writes::default()
		};
		if let Err(error) = self.save(&writes, false, keys) {
			let _ = fs::remove_file(&staged);
			return Err(error);
		}
		self.blobs.place(&staged, &sealed.sealed)?;
		Ok(sealed.content)
	}

	/// The sealed copy of some content that the replica found intact most recently, or else
	/// the first one it holds.
	fn checked_last(&self, content: &Hash) -> Result<Option<HeldCopy<'_>>, StoreError> {
		let mut last: Option<HeldCopy<'_>> = None;
		for (sealed, key) in self.held(content) {
			let verified = self.file().verified(sealed)?;
			if last.as_ref().is_none_or(|last| verified > last.verified) {
				last = Some(HeldCopy {
					sealed,
					key,
					verified,
				});
			}
		}
		Ok(last)
	}

	/// The sealed copies of some content that the replica holds, with their keys.
	fn held<'a>(&'a self, content: &Hash) -> impl Iterator<Item = (&'a Hash, &'a Key)> {
		self.log
			.blob_keys(content)
			.filter(|(sealed, _)| self.blobs.holds(sealed))
	}

	/// Deletes the files of purged blobs that no other space of the replica keeps a key for,
	/// or in a batch, once the batch is saved. Without their keys they are unreadable, so one
	/// that cannot be deleted now is deleted when the space is opened again.
	fn remove_blob_files(&mut self, purged: &[Hash]) {
		match &mut self.batch {
			Some(batch) => batch.released.extend_from_slice(purged),
			None => self.release_blob_files(purged),
		}
	}

	/// Deletes the files of sealed blobs whose keys purges destroyed, as far as no other space
	/// keeps a key for them; or, if the replica waits, keeps them apart.
	fn release_blob_files(&self, purged: &[Hash]) {
		if self.waiting.keeping {
			// A file that cannot be moved goes, as it would have.
			if self.blobs.keep_apart(purged).is_err() {
				let _ = self.blobs.release(purged);
			}
		} else {
			let _ = self.blobs.release(purged);
		}
	}

	/// Finishes what a crash interrupted, with the space alone. A blob being written whose
	/// key was saved goes in place, and the other files being written go, but while another
	/// program has the space open, as it may be writing them. The files of purged blobs, which are unreadable without their keys, go as far as no other space
	/// keeps a key for them and they can be deleted now. The wrapped blob keys that turned
	/// out to be bad when the log was built leave the disk.
	fn recover_blobs(&mut self, dropped: Vec<Hash>) -> Result<(), StoreError> {
		let temporary = if self.lock.is_alone() {
			self.blobs.temporary_files()?
		} else {
			Vec::new()
		};
		for (path, staged) in temporary {
			match staged {
				Some(sealed) if !self.log.wrapped_blob_keys_of(&sealed).is_empty() => {
					let _ = self.blobs.place(&path, &sealed);
				}
				_ => {
					let _ = fs::remove_file(&path);
				}
			}
		}
		// Files of purged blobs whose keys a device that waits keeps apart are kept apart too,
		// as a crash between the two left them.
		let kept = self.file().kept_blobs()?;
		let (apart, purged): (Vec<Hash>, Vec<Hash>) = self
			.log
			.purged_blobs()?
			.into_iter()
			.partition(|sealed| kept.contains(sealed));
		let _ = self.blobs.keep_apart(&apart);
		let _ = self.blobs.release(&purged);
		self.save_records(&Writes {
			blob_keys: self.blob_key_sets(dropped),
			..Writes::default()
		})
	}

	/// Saves records that do not change the log, such as checks of blobs and the removal
	/// of bad keys, and retries an unfinished truncation of the write-ahead log.
	fn save_records(&mut self, writes: &Writes) -> Result<(), StoreError> {
		self.file_mut().save(writes)?;
		if self.unshredded {
			self.unshredded = !self.file().truncate_wal().unwrap_or(false);
		}
		Ok(())
	}

	/// Takes the operations of applied changes into the indexes, and notes that the state
	/// file holds them, in the transaction that takes them into the state. If that fails,
	/// the log, which accepted them already, is built again from the files, which do not
	/// hold them.
	fn take_in(
		&mut self,
		applied: &[(ChangeId, Payload)],
		keys: &SpaceKeys,
	) -> Result<(), StoreError> {
		// A state that a purge left stale is built again from the files, indexes and all.
		if self.log.is_stale() {
			return Ok(());
		}
		let taken = applied.iter().try_for_each(|(id, payload)| {
			index::apply(
				&self.log,
				self.postings(),
				self.fields(),
				payload.all_operations(),
			)?;
			self.postings().note_applied(id.hash())
		});
		if let Err(error) = taken {
			self.reload(keys);
			return Err(error);
		}
		Ok(())
	}

	/// Runs `change`, which changes the log and saves it, in a transaction of the state
	/// file, which saving commits together with how far into the log file it now goes, or
	/// leaves open for the next change of a burst, as [`StoredLog::end_state_change`] says.
	/// If `change` fails before saving, what it wrote to the state file is left out; if the
	/// state file failed, the log is built again from the files. A change that finds the
	/// state file damaged is made once more, after the state file is made again. In a batch,
	/// `change` runs in the batch's transactions instead, as [`StoredLog::change_in_batch`]
	/// says.
	fn changing<T>(
		&mut self,
		keys: &SpaceKeys,
		mut change: impl FnMut(&mut Self) -> Result<T, StoreError>,
	) -> Result<T, StoreError> {
		if self.batch.is_some() {
			return self.change_in_batch(change);
		}
		let mut tries = 0;
		loop {
			tries += 1;
			match self.change_once(keys, &mut change) {
				Err(StoreError::State(_)) if tries == 1 => {
					if self.log.state().storage().ordinary().is_damaged() {
						self.reload(keys);
					}
				}
				result => return result,
			}
		}
	}

	/// Runs `change` once, as [`StoredLog::changing`] says.
	fn change_once<T>(
		&mut self,
		keys: &SpaceKeys,
		change: &mut impl FnMut(&mut Self) -> Result<T, StoreError>,
	) -> Result<T, StoreError> {
		if self.standing == Standing::Behind
			|| self.log.state().storage().ordinary().is_damaged()
			|| self.pending_lost()
		{
			self.reload(keys);
		}
		self.check_usable()?;
		self.build_index()?;
		self.begin_writing(keys)?;
		if let Err(error) = self.begin_state_change() {
			let _ = self.file_mut().rollback();
			return Err(error);
		}
		let result = change(self);
		// The log's own account of a storage failure, taken before anything builds the log
		// again, which forgets it.
		let storage_failed = matches!(result, Err(StoreError::Log(LogError::Storage)));
		let result = result.map_err(|error| match error {
			StoreError::Log(LogError::Storage) => {
				self.log.storage_error().unwrap_or(StoreError::Broken)
			}
			error => error,
		});
		if storage_failed {
			self.reload(keys);
		} else if result.is_err() || self.file().in_transaction() {
			// A change that saved left no transaction of the log file open; what else is open
			// is left out, and the state file's transaction of the burst, if any, stays.
			self.roll_back_change(keys);
		}
		self.change_began = None;
		result
	}

	/// Begins the log file's transaction, for a change or a batch, once the log has caught up
	/// with what others wrote, under that transaction's write lock, so that it never writes
	/// on a state it has not seen ([technical design: several programs on one replica]). The log file's write lock is taken
	/// before the state file's: the changes of a burst, which hold the state file's, commit
	/// first if another program holds the log file's, so that the two never wait for each
	/// other. A program that waits to make the state file anew, after a purge, is told so:
	/// the space is to be closed.
	///
	/// [technical design: several programs on one replica]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#78-several-programs-on-one-replica
	fn begin_writing(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		for _ in 0..CATCH_UP_TRIES {
			let began = self.postings().in_transaction() && self.file_mut().begin_now()?;
			if !began {
				self.commit_pending();
				self.file_mut().begin()?;
			}
			if self.remake.exists() {
				// Built again at the next change, which makes the state file anew, or lets go
				// of the mark, once no other program has the space open.
				let _ = self.file_mut().rollback();
				self.standing = Standing::Behind;
				return Err(StoreError::InUse);
			}
			let versions = self.versions();
			if versions
				.as_ref()
				.is_ok_and(|versions| *versions == self.seen)
			{
				return Ok(());
			}
			let _ = self.file_mut().rollback();
			versions?;
			self.commit_pending();
			self.reload(keys);
			self.check_usable()?;
			self.build_index()?;
		}
		Err(StoreError::InUse)
	}

	/// Changes the log in memory, and saves what changes apart from a change, such as a key
	/// received, once the log has caught up with what others wrote, under the log file's
	/// write lock, as [`StoredLog::begin_writing`] says: what it saves of a statement or a
	/// blob replaces what the files hold of it.
	fn apart<T>(
		&mut self,
		keys: &SpaceKeys,
		change: impl FnOnce(&mut Self) -> Result<T, StoreError>,
	) -> Result<T, StoreError> {
		self.check_not_in_batch()?;
		if self.standing == Standing::Behind {
			self.reload(keys);
		}
		self.check_usable()?;
		self.begin_writing(keys)?;
		let result = change(self);
		// Nothing was saved, or saving failed, which builds the log again.
		if self.file().in_transaction() {
			let _ = self.file_mut().rollback();
		}
		result
	}

	/// Begins the state file's part of a change: its transaction, or, while the transaction
	/// of a burst holds changes saved before, a savepoint in it, so that this change alone
	/// can be left out.
	fn begin_state_change(&mut self) -> Result<(), StoreError> {
		self.change_began = Some(self.ticks.now());
		if self.postings().in_transaction()
			&& let Some(mut pending) = self.pending.get()
		{
			self.log.storage_mut().savepoint(Savepoint::Change)?;
			pending.savepoint = true;
			self.pending.set(Some(pending));
			return Ok(());
		}
		self.pending.set(None);
		self.log.storage_mut().begin()
	}

	/// Whether an error ended the state file's transaction with the changes of a burst in
	/// it: the log file holds them, and the state file lacks them until the log is built
	/// again from the files, which takes them in.
	fn pending_lost(&self) -> bool {
		self.pending
			.get()
			.is_some_and(|pending| pending.lost || !self.postings().in_transaction())
	}

	/// Ends the state file's part of a change saved to the log file: keeps what it wrote,
	/// notes how far into the log file the state file goes now, and commits the transaction,
	/// unless the change began within [`BURST_GAP`] of the last change saved: the changes of
	/// a burst commit together, when a change comes later than that, when they are
	/// [`PENDING_CHANGES`], when a change comes more than [`PENDING_FOR`] after the first of
	/// them, and when the space closes. What is saved apart from a change while a burst is
	/// pending, such as a blob or a key, waits with the burst, and does not count as a
	/// change. If the state file fails, the log file holds what it lacks, which building
	/// the log again takes in.
	fn end_state_change(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		if self.change_began.is_none()
			&& let Some(pending) = self.pending.get()
		{
			if self.mark_state().is_err() {
				self.reload(keys);
				return self.check_usable();
			}
			// Saving is using the space, which commits a burst that has waited long enough.
			if self.ticks.now().duration_since(pending.since) >= PENDING_FOR {
				self.commit_state(keys)?;
			}
			return Ok(());
		}
		let burst = match (self.last_saved, self.change_began.take()) {
			(Some(last), Some(began)) => began.duration_since(last) < BURST_GAP,
			_ => false,
		};
		let kept = match self.pending.get() {
			Some(pending) if pending.savepoint => {
				self.pending.set(Some(Pending {
					savepoint: false,
					..pending
				}));
				self.log.storage_mut().release(Savepoint::Change)
			}
			_ => Ok(()),
		};
		if kept.and_then(|()| self.mark_state()).is_err() {
			self.reload(keys);
			return self.check_usable();
		}
		let now = self.ticks.now();
		let mut pending = self.pending.get().unwrap_or(Pending {
			changes: 0,
			since: now,
			savepoint: false,
			lost: false,
		});
		pending.changes += 1;
		self.pending.set(Some(pending));
		// Another program that has the space open would wait for the state file's write lock
		// while a burst holds it: changes commit one by one then.
		let due = !burst
			|| pending.changes >= PENDING_CHANGES
			|| now.duration_since(pending.since) >= PENDING_FOR
			|| (pending.changes == 1 && self.lock.shared_with_others());
		if due {
			self.commit_state(keys)?;
		}
		self.last_saved = Some(self.ticks.now());
		Ok(())
	}

	/// Leaves out what the change being made wrote to both files: the log file's
	/// transaction, and in the state file the change's savepoint, or its transaction where
	/// no change of a burst is pending in it. If an error ended the state file's transaction
	/// already, with the changes of a burst in it, the log is built again from the files,
	/// which takes them in.
	fn roll_back_change(&mut self, keys: &SpaceKeys) {
		self.change_began = None;
		if self.postings().in_transaction() {
			match self.pending.get() {
				Some(pending) if pending.savepoint => {
					self.pending.set(Some(Pending {
						savepoint: false,
						..pending
					}));
					let _ = self
						.log
						.storage_mut()
						.rollback_to_savepoint(Savepoint::Change);
				}
				Some(_) => {}
				None => {
					let _ = self.log.storage_mut().rollback();
				}
			}
		} else if self.pending.take().is_some() {
			self.reload(keys);
			return;
		}
		if self.file().in_transaction() {
			let _ = self.file_mut().rollback();
		}
	}

	/// Runs `change` in the open batch, after a savepoint of each file. A change that is
	/// refused goes back to its savepoints, and the batch goes on. If the files fail, the
	/// whole batch is left out, and the log is built again from the files; the batch's
	/// later changes are then refused. A change that finds the state file damaged is not
	/// made again, which would leave out the changes before it.
	fn change_in_batch<T>(
		&mut self,
		change: impl FnOnce(&mut Self) -> Result<T, StoreError>,
	) -> Result<T, StoreError> {
		if self.batch.as_ref().is_some_and(|batch| batch.lost) {
			return Err(StoreError::BatchLost);
		}
		if !self.both_in_transaction() {
			// A read found a file failing, which ended its transaction.
			self.lose_batch(false);
			return Err(StoreError::BatchLost);
		}
		let reloads = self.reloads;
		// What the batch's purges destroyed so far, kept if this change goes back to its
		// savepoints.
		let destroyed = self
			.batch
			.as_ref()
			.map(|batch| (batch.purged, batch.released.len()));
		let began = self.build_index().and_then(|()| {
			self.file_mut().savepoint()?;
			self.log.storage_mut().savepoint(Savepoint::Change)
		});
		if let Err(error) = began {
			self.lose_batch(self.reloads > reloads);
			return Err(error);
		}
		let result = change(self);
		let failed = match &result {
			Ok(_) => !self.both_in_transaction(),
			Err(error) => {
				matches!(error, StoreError::Log(LogError::Storage)) || !self.both_in_transaction()
			}
		};
		if failed {
			let error = match result {
				Err(StoreError::Log(LogError::Storage)) => {
					self.log.storage_error().unwrap_or(StoreError::Broken)
				}
				Err(error) => error,
				Ok(_) => StoreError::BatchLost,
			};
			self.lose_batch(self.reloads > reloads);
			return Err(error);
		}
		let ended = if result.is_ok() {
			self.file_mut()
				.release()
				.and_then(|()| self.log.storage_mut().release(Savepoint::Change))
		} else {
			if let (Some(batch), Some((purged, released))) = (&mut self.batch, destroyed) {
				batch.purged = purged;
				batch.released.truncate(released);
			}
			self.file_mut().rollback_to_savepoint().and_then(|()| {
				self.log
					.storage_mut()
					.rollback_to_savepoint(Savepoint::Change)
			})
		};
		if let Err(error) = ended {
			self.lose_batch(false);
			return Err(error);
		}
		result
	}

	/// Leaves out every change of the open batch, after the files failed, and builds the
	/// log again from them, unless the failure did already. The batch stays open, lost,
	/// until it ends.
	fn lose_batch(&mut self, rebuilt: bool) {
		if let Some(mut batch) = self.batch.take() {
			if !batch.lost {
				if !rebuilt {
					self.reload(&batch.keys);
				}
				batch.lost = true;
			}
			self.batch = Some(batch);
		}
	}

	/// Whether both files have a transaction open, as a batch has.
	fn both_in_transaction(&self) -> bool {
		self.file().in_transaction() && self.postings().in_transaction()
	}

	/// Returns [`StoreError::InBatch`] if a batch of changes is open.
	const fn check_not_in_batch(&self) -> Result<(), StoreError> {
		if self.batch.is_some() {
			return Err(StoreError::InBatch);
		}
		Ok(())
	}

	/// Leaves out what was written to both files since their transactions began, if they
	/// have not ended, the changes of a burst that the state file holds included: the log
	/// file holds them, and building the log again takes them in.
	fn roll_back(&mut self) {
		self.pending.set(None);
		self.change_began = None;
		if self.log.storage_mut().in_transaction() {
			let _ = self.log.storage_mut().rollback();
		}
		if self.file().in_transaction() {
			let _ = self.file_mut().rollback();
		}
	}

	/// A waiting change with its parts, which the log file holds apart from the history:
	/// what the log has of an accepted change, it saves itself.
	fn waiting_parcel(&self, id: ChangeId) -> Result<Option<Parcel>, StoreError> {
		if self.file().contains(id)? {
			return Ok(None);
		}
		self.log.parcel(id)
	}

	/// The log file.
	const fn file(&self) -> &LogFile {
		self.log.history()
	}

	/// The log file, to write.
	fn file_mut(&mut self) -> &mut LogFile {
		self.log.history_mut()
	}

	fn check_usable(&self) -> Result<(), StoreError> {
		if self.standing == Standing::Broken {
			return Err(StoreError::Broken);
		}
		if self.standing == Standing::Behind {
			return Err(StoreError::InUse);
		}
		Ok(())
	}

	/// The wrapped keys that the log has for some statements.
	fn key_sets(
		&self,
		statements: impl IntoIterator<Item = StatementId>,
	) -> Vec<(StatementId, Vec<WrappedKey>)> {
		statements
			.into_iter()
			.map(|statement| (statement, self.log.wrapped_keys_of(statement).to_vec()))
			.collect()
	}

	/// The wrapped keys that the log has for some sealed blobs.
	fn blob_key_sets(
		&self,
		sealed: impl IntoIterator<Item = Hash>,
	) -> Vec<(Hash, Vec<WrappedBlobKey>)> {
		sealed
			.into_iter()
			.map(|sealed| {
				let keys = self.log.wrapped_blob_keys_of(&sealed).to_vec();
				(sealed, keys)
			})
			.collect()
	}

	fn all_key_sets(&self) -> BTreeMap<StatementId, Vec<WrappedKey>> {
		let mut sets: BTreeMap<StatementId, Vec<WrappedKey>> = BTreeMap::new();
		for (statement, wrapped) in self.log.wrapped_keys() {
			sets.entry(statement).or_default().push(wrapped.clone());
		}
		sets
	}

	/// Saves to the log file, with what the log wrote to it as its history, and then ends
	/// the state file's part of the change, if its transaction has begun, as
	/// [`StoredLog::end_state_change`] says; truncates the write-ahead log after purges. If
	/// saving fails, builds the log again from the files, so that the log never holds what
	/// the file does not; if that fails too, the stored log is unusable and its log empty.
	/// If only the state file fails, the change is saved, and the state file takes it in
	/// from the log file. In a batch, both transactions stay open, for the batch's end to
	/// commit, and to act on purges, which only changes received in it make.
	fn save(&mut self, writes: &Writes, purges: bool, keys: &SpaceKeys) -> Result<(), StoreError> {
		let batch = self.batch.is_some();
		if purges && let Some(batch) = &mut self.batch {
			batch.purged = true;
		}
		if writes.is_empty() && !self.file().wrote() && !self.log.is_stale() {
			// Nothing changed, such as for a change received twice: neither file is written.
			if !batch {
				self.roll_back_change(keys);
			}
			return Ok(());
		}
		let saved = self.file_mut().save(writes).and_then(|()| {
			if !batch && self.file().in_transaction() {
				self.file_mut().commit()
			} else {
				Ok(())
			}
		});
		if let Err(error) = saved {
			self.reload(keys);
			return Err(error);
		}
		if batch {
			return Ok(());
		}
		if self.log.take_stale() {
			// A purge took away what the state file holds: it is written anew, under a new
			// key, from the log file, which no longer holds what was purged. The change is
			// saved, even if that waits for others to close the space.
			self.remake_state(keys);
			if self.standing == Standing::Broken {
				return Err(StoreError::Broken);
			}
		} else if self.log.storage_mut().in_transaction() {
			self.end_state_change(keys)?;
		}
		if purges || self.unshredded {
			// The purge is saved; if another program holds the database, the old copies are
			// removed on a later save.
			self.unshredded = !self.file().truncate_wal().unwrap_or(false);
		}
		Ok(())
	}

	/// Notes in the state file's transaction how far into the log file it goes now: to its
	/// last row, after the log file committed.
	fn mark_state(&mut self) -> Result<(), StoreError> {
		if let Some(mark) = self.file().last()? {
			self.log.storage_mut().set_mark(&mark)?;
		}
		Ok(())
	}

	/// Commits the transaction of the state file, with the changes it holds and how far
	/// into the log file it goes. If that fails, the log file holds what the state file
	/// lacks, which building the log again takes in.
	fn commit_state(&mut self, keys: &SpaceKeys) -> Result<(), StoreError> {
		self.pending.set(None);
		if self.log.storage_mut().commit().is_err() {
			self.reload(keys);
			self.check_usable()?;
		}
		Ok(())
	}

	/// Makes the state file anew, under a new key, and builds the log and the indexes again
	/// from the files, alone: other programs that have the space open are waited for, for a
	/// while, as [`StoredLog::wait_alone`] says. If they keep it, the stored log is behind,
	/// and the state file is made anew by the first that opens the space alone. If making it
	/// fails, the stored log is unusable and its log empty.
	fn remake_state(&mut self, keys: &SpaceKeys) {
		self.roll_back();
		if !self.wait_alone() {
			self.log.reset(Clock::new(self.max_drift_millis));
			self.standing = Standing::Behind;
			return;
		}
		if self
			.log
			.storage_mut()
			.remake(&self.state_key, state_rng())
			.is_ok()
		{
			self.reload(keys);
		} else {
			self.log.reset(Clock::new(self.max_drift_millis));
			self.standing = Standing::Broken;
		}
		let _ = fs::remove_file(&self.remake);
		self.lock.share();
	}

	/// Takes the space's lock alone, waiting up to [`PURGE_WAIT`] for the other programs that
	/// have the space open to close it: the mark that says so tells them to at their next
	/// change. Returns whether it has the lock alone. The mark stays if it does not, until a
	/// program opens the space alone, which makes the state file anew.
	fn wait_alone(&self) -> bool {
		if self.lock.alone() {
			return true;
		}
		let _ = File::create(&self.remake);
		let waited = Instant::now();
		while waited.elapsed() < PURGE_WAIT {
			std::thread::sleep(PURGE_LOOK);
			if self.lock.alone() {
				return true;
			}
		}
		false
	}

	/// Builds the log and the indexes again from the files: after another program wrote to
	/// the space, or after a failure left the log out of step with them. A state file that
	/// must be made anew is made so only alone: while others have the space open, the stored
	/// log is behind. If building fails otherwise, the stored log is unusable and its log
	/// empty.
	fn reload(&mut self, keys: &SpaceKeys) {
		self.reloads += 1;
		let mut keys = keys.clone();
		if self.locked {
			keys.forget_secrets();
		}
		self.roll_back();
		self.log.reset(Clock::new(self.max_drift_millis));
		self.adopt_indexed_fields();
		let was_alone = self.lock.is_alone();
		let lock = &self.lock;
		// Damage found since opening may be where building again does not read.
		let remade = if !self.log.state().storage().ordinary().is_damaged() {
			Ok(())
		} else if lock.alone() {
			self.log.storage_mut().remake(&self.state_key, state_rng())
		} else {
			Err(StoreError::InUse)
		};
		// Before the log is built, so that what others write meanwhile is caught up with
		// later.
		let seen = remade.and_then(|()| {
			Ok((
				self.log.history().data_version()?,
				self.log.state().storage().ordinary().data_version()?,
			))
		});
		let fields = Fields {
			secret: &self.secret_fields,
			last_writer_wins: &self.last_writer_wins,
		};
		let built = seen.and_then(|seen| {
			build(
				&mut self.log,
				&keys,
				self.max_drift_millis,
				&self.state_key,
				fields,
				&|| lock.alone(),
			)?;
			Ok(seen)
		});
		match built {
			Ok(seen) => {
				if self.standing == Standing::Behind {
					self.standing = Standing::Usable;
				}
				// The state file is in step with the log file now: a mark that waits for it to
				// be made anew goes, once no other program has the space open.
				if self.remake.exists() && self.lock.alone() {
					let _ = fs::remove_file(&self.remake);
				}
				self.seen = seen;
				// Alone, nothing was missed, and the state file may have been made anew.
				if self.lock.is_alone()
					&& let Ok(versions) = self.versions()
				{
					self.seen = versions;
				}
			}
			// Another program has the space alone, or holds a write lock longer than SQLite
			// waits: building again is tried at the next change.
			Err(error) if matches!(error, StoreError::InUse) || error.busy() => {
				self.log.reset(Clock::new(self.max_drift_millis));
				self.standing = Standing::Behind;
			}
			Err(_) => {
				// The state goes too, so that it shows nothing, as the log does.
				self.log.reset(Clock::new(self.max_drift_millis));
				if self.lock.alone() {
					let _ = self.log.storage_mut().remake(&self.state_key, state_rng());
				}
				self.standing = Standing::Broken;
			}
		}
		if !was_alone {
			self.lock.share();
		}
		self.note_index();
	}

	/// Takes the fields that the state file's indexes were built with as the special ones,
	/// unless fewer of them are secret than here, as opening does: another program that has
	/// the space open may have made more fields secret, whose values are sealed from then on
	/// here too.
	fn adopt_indexed_fields(&mut self) {
		if let Ok(Some((secret, last_writer_wins))) = self.postings().indexed_fields()
			&& self.secret_fields.is_subset(&secret)
		{
			self.log.set_secret_fields(secret.iter().cloned());
			self.secret_fields = secret;
			self.last_writer_wins = last_writer_wins;
		}
	}
}

/// A sealed copy of some content that the replica holds.
struct HeldCopy<'a> {
	sealed: &'a Hash,
	key: &'a Key,
	/// When the replica last found it intact.
	verified: Option<u64>,
}

/// Builds a space's log from its file into `log`, which keeps its history in the log file
/// and its state in the state file, as [`load`] does, and the indexes, unless they were
/// built with the fields that are special now. A state file that does not match the log
/// file, or turns out damaged on the way, is made again and takes in every change, if
/// `may_remake` says that the program has the space alone; otherwise the build fails with
/// [`StoreError::InUse`]. Returns the sealed hashes of the blobs some of whose stored
/// wrapped keys turned out to be bad.
fn build(
	log: &mut Log<StateFile, LogFile>,
	keys: &SpaceKeys,
	max_drift_millis: u64,
	state_key: &Key,
	fields: Fields<'_>,
	may_remake: &dyn Fn() -> bool,
) -> Result<Vec<Hash>, StoreError> {
	let attempt = |log: &mut Log<StateFile, LogFile>| -> Result<Vec<Hash>, StoreError> {
		let dropped = load(log, keys, fields)?;
		if !indexed_with(log, fields)? {
			let was = log.state().storage().ordinary().indexed_fields()?;
			ensure_index(
				log,
				was.as_ref(),
				(fields.secret.clone(), fields.last_writer_wins.clone()),
				false,
			)?;
		}
		Ok(dropped)
	};
	match attempt(log) {
		Err(StoreError::State(_)) if may_remake() => {
			log.reset(Clock::new(max_drift_millis));
			log.storage_mut().remake(state_key, state_rng())?;
			attempt(log)
		}
		// Only a program that has the space alone may make the state file anew.
		Err(StoreError::State(_)) => Err(StoreError::InUse),
		built => built,
	}
}

/// Opens a space's state file, or makes it anew if it cannot be used, as
/// [`StateFile::open_or_make`] does, but only `alone`: while others have the space open, it
/// is made anew by the first that opens the space alone.
fn open_state(path: &Path, key: &Key, alone: bool) -> Result<StateFile, StoreError> {
	if alone {
		return StateFile::open_or_make(path, key, state_rng);
	}
	match StateFile::open(path, key, state_rng()) {
		Err(StoreError::State(_)) => Err(StoreError::InUse),
		opened => opened,
	}
}

/// Whether the indexes of a log's state were built with the fields that are special now.
fn indexed_with(log: &Log<StateFile, LogFile>, fields: Fields<'_>) -> Result<bool, StoreError> {
	Ok(matches!(
		log.state().storage().ordinary().indexed_fields()?,
		Some((secret, last)) if secret == *fields.secret && last == *fields.last_writer_wins
	))
}

/// Brings the indexes of a log's state to the fields that are special, secret and whose
/// last writer wins: from the fields they were built with, `was`, indexing again the fields
/// that changed, or anew, if they were not built. It runs in a transaction of its own, which
/// only the indexes are written in, so that a failure leaves the state file readable, or
/// else, in a transaction that has begun, of a batch or of a burst of changes, in a
/// savepoint of it, which commits with its changes.
fn ensure_index(
	log: &mut Log<StateFile, LogFile>,
	was: Option<&SpecialFields>,
	(secret, last_writer_wins): SpecialFields,
	in_transaction: bool,
) -> Result<(), StoreError> {
	let own = !in_transaction;
	if own {
		log.storage_mut().begin_derived()?;
	} else {
		log.storage_mut().savepoint(Savepoint::Indexes)?;
	}
	let fields = Fields {
		secret: &secret,
		last_writer_wins: &last_writer_wins,
	};
	let postings = log.state().storage().ordinary();
	let brought = match was {
		Some((was_secret, was_last)) => index::rebuild_fields(
			log,
			postings,
			Fields {
				secret: was_secret,
				last_writer_wins: was_last,
			},
			fields,
		),
		None => index::build(log, postings, fields),
	};
	let built = brought.and_then(|()| postings.set_indexed_fields(&secret, &last_writer_wins));
	match built {
		Ok(()) if own => log.storage_mut().commit(),
		Ok(()) => log.storage_mut().release(Savepoint::Indexes),
		Err(error) => {
			if log.storage_mut().in_transaction() {
				let _ = if own {
					log.storage_mut().rollback()
				} else {
					log.storage_mut().rollback_to_savepoint(Savepoint::Indexes)
				};
			}
			Err(error)
		}
	}
}

/// Opens a space's log, whose history is the log file: takes the waiting changes and the
/// wrapped keys into memory, resumes the clock, and has the state file catch up with the
/// log file, in one transaction of each. Returns the sealed hashes of the blobs some of
/// whose stored wrapped keys turned out to be bad.
fn load(
	log: &mut Log<StateFile, LogFile>,
	keys: &SpaceKeys,
	fields: Fields<'_>,
) -> Result<Vec<Hash>, StoreError> {
	if let Some(latest) = log.history().latest()? {
		log.resume_clock(latest);
	}
	log.history_mut().begin()?;
	let caught_up = log
		.storage_mut()
		.begin()
		.and_then(|()| catch_up(log, keys, fields));
	let committed = match caught_up {
		Ok(true) => log
			.history_mut()
			.commit()
			.and_then(|()| log.storage_mut().commit()),
		Ok(false) => Ok(()),
		Err(error) => Err(error),
	};
	// Whatever happened, no transaction is left open.
	if log.storage_mut().in_transaction() {
		let _ = log.storage_mut().rollback();
	}
	if log.history().in_transaction() {
		let _ = log.history_mut().rollback();
	}
	committed?;
	for (statement, wrapped) in log.history().wrapped_keys()? {
		let received = log.receive_wrapped_key(statement, wrapped, keys);
		logged(log, received)?;
	}
	let mut stored: BTreeMap<Hash, usize> = BTreeMap::new();
	for (sealed, wrapped) in log.history().blob_keys()? {
		*stored.entry(sealed).or_default() += 1;
		let received = log.receive_blob_key(sealed, wrapped, keys);
		logged(log, received)?;
	}
	Ok(stored
		.into_iter()
		.filter(|(sealed, count)| log.wrapped_blob_keys_of(sealed).len() < *count)
		.map(|(sealed, _)| sealed)
		.collect())
}

/// Has the state file catch up with the log file, in transactions of both that have
/// begun: takes the waiting changes back, applies the changes after the state file's mark
/// that the log file says are applied and the state file lacks, and those that `keys` let
/// the log read now, takes them into the indexes, and moves the mark. Returns whether it
/// wrote anything. Returns [`StoreError::State`] if the mark's tag is no change's in the log
/// file at the mark's sequence or later, or the state file holds a change that was purged:
/// the state file is then ahead of the log file, does not belong to it, or holds what a
/// purge took away, such as when a crash kept it from being written anew.
fn catch_up(
	log: &mut Log<StateFile, LogFile>,
	keys: &SpaceKeys,
	fields: Fields<'_>,
) -> Result<bool, StoreError> {
	let mark = log.state().storage().ordinary().mark()?;
	if let Some(mark) = &mark {
		// A row moves only to the end, so one before the mark's sequence is from an older
		// log file.
		let at = log.history().sequence_of(&mark.tag)?;
		if at.is_none_or(|at| at < mark.sequence) {
			return Err(StoreError::State(
				"it is ahead of the log file, or does not match it".to_owned(),
			));
		}
	}
	for purged in log.purged_changes()? {
		if log
			.state()
			.storage()
			.ordinary()
			.has_applied(purged.hash())?
		{
			return Err(StoreError::State(
				"it holds what a purge took away".to_owned(),
			));
		}
	}
	for set_aside in log.set_aside_changes()? {
		if log
			.state()
			.storage()
			.ordinary()
			.has_applied(set_aside.hash())?
		{
			return Err(StoreError::State(
				"it holds a change that was set aside".to_owned(),
			));
		}
	}
	// The changes the state file takes in go into the indexes too, in the same transaction,
	// unless the indexes are to be built anew, with other fields.
	let indexed = indexed_with(log, fields)?;
	// Each change goes into the indexes right after it goes into the state, as the indexes
	// follow the state change by change.
	let mut wrote = false;
	for parcel in log.history().waiting()? {
		let restored = log.restore(parcel, keys, false);
		logged(log, restored)?;
		wrote |= take_in_indexes(log, fields, indexed)?;
	}
	let since = mark.as_ref().map_or(0, |mark| mark.sequence);
	for id in log.history().applied_since(since)? {
		if !log.state().storage().ordinary().has_applied(id.hash())? {
			let reapplied = log.reapply(id, keys);
			logged(log, reapplied)?;
			wrote |= take_in_indexes(log, fields, indexed)?;
		}
	}
	// Pending changes that changed since may be readable now; the others were not when
	// the state file last caught up, and [`StoredLog::open_changes`] tries them again when
	// keys arrive.
	let pending = log.history().pending_since(since)?;
	let opened = log.open_changes_of(pending, keys);
	logged(log, opened)?;
	wrote |= take_in_indexes(log, fields, indexed)?;
	let last = log.history().last()?;
	if !wrote && last == mark {
		return Ok(false);
	}
	// The indexes, which do not take these changes in, are to be built again.
	if !indexed {
		log.state().storage().ordinary().forget_indexed_fields()?;
	}
	if let Some(last) = last {
		log.storage_mut().set_mark(&last)?;
	}
	Ok(true)
}

/// Takes the changes whose operations the log just applied into the indexes, unless they
/// are to be built anew, and notes that the state file holds them. Returns whether there
/// were any.
fn take_in_indexes(
	log: &mut Log<StateFile, LogFile>,
	fields: Fields<'_>,
	indexed: bool,
) -> Result<bool, StoreError> {
	let applied = log.take_applied();
	for (id, payload) in &applied {
		if indexed {
			index::apply(
				log,
				log.state().storage().ordinary(),
				fields,
				payload.all_operations(),
			)?;
		}
		log.state().storage().ordinary().note_applied(id.hash())?;
	}
	Ok(!applied.is_empty())
}

/// What the log did, with its failure to read or write the storage told as the storage's
/// error.
fn logged<T>(
	log: &mut Log<StateFile, LogFile>,
	result: Result<T, LogError>,
) -> Result<T, StoreError> {
	result.map_err(|error| match error {
		LogError::Storage => log.storage_error().unwrap_or(StoreError::Broken),
		error => StoreError::Log(error),
	})
}

/// The error for a sealed copy that does not read back.
fn blob_error(error: io::Error) -> StoreError {
	if error.kind() == io::ErrorKind::InvalidData {
		StoreError::DamagedBlob
	} else {
		StoreError::Io(error)
	}
}

/// The wall clock time now, in milliseconds since 1970.
fn wall_now() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |since| {
			u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
		})
}
