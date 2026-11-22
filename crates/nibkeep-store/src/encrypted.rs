//! Encrypted databases: SQLite through a VFS of the store's own, which encrypts every page
//! of a database and every record of its write-ahead log with XAES-256-GCM, each under a
//! random nonce and authenticated with its place, as the technical design's section on the state file decided. The
//! format is in the storage specification.
//!
//! The file is a header that holds the database's own key, sealed under the key it is
//! opened with, then each page as a sealed record: a nonce,
//! the page encrypted, and a tag. The write-ahead log is its header, then for each frame
//! the frame's header and its page, each a sealed record of its own. SQLite writes exactly
//! these units, so writing never needs a record read first.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Key, PageCipher, RECORD_OVERHEAD};
use nibkeep_sqlite_vfs::{Error, FileKind, RawFile, SyncMode, Vfs, full_path, register};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use zeroize::Zeroize;

use crate::StoreError;

/// The name under which the VFS is registered with SQLite.
const VFS_NAME: &str = "nibkeep-encrypted";

/// How long to wait for a lock that another connection holds.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The size of a page, the only one the VFS stores.
const PAGE_SIZE: u16 = 4096;
const PAGE: usize = PAGE_SIZE as usize;

/// What the header starts with: the file's kind and the version of its format.
const MAGIC: &[u8; 16] = b"nibkeep state\0\0\x01";

/// The part of the header that its sealed record authenticates: the magic, the size of a
/// page, and four bytes of zeros, kept for later.
const HEADER_FIELDS: usize = MAGIC.len() + 8;

/// The length of a key.
const KEY: usize = 32;

/// The header: its fields, and a sealed record of the database's own key.
const HEADER: usize = HEADER_FIELDS + RECORD_OVERHEAD + KEY;

/// The header of the write-ahead log, and of each of its frames, as SQLite writes them.
const LOG_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;

/// The VFS's keys, by the full path of the database each opens, with how many connections
/// of this program have it open.
type Keys = Mutex<BTreeMap<PathBuf, (Arc<Keyed>, usize)>>;

/// What encrypts one database: the key it was opened with and its cipher, which seals the
/// database's own key in its header; the database's key and cipher, once its header is
/// read or written; and randomness for nonces and for the key of a new database.
struct Keyed {
	opened_with: Key,
	wrapping: PageCipher,
	own: OnceLock<(Key, PageCipher)>,
	rng: Mutex<Box<dyn CryptoRng + Send>>,
}

impl Keyed {
	/// The cipher of the database's pages and log, once its header is read or written.
	fn cipher(&self) -> Option<&PageCipher> {
		self.own.get().map(|(_, cipher)| cipher)
	}

	/// Keeps the database's own key, read from its header or made for it. A key other than
	/// one kept already means the file was made again under another key while open.
	fn keep(&self, key: &Key) -> Result<(), Error> {
		let kept = self.own.get_or_init(|| (key.clone(), PageCipher::new(key)));
		if kept.0.same_as(key) {
			Ok(())
		} else {
			Err(Error::NotADatabase)
		}
	}
}

/// An SQLite database encrypted at rest, with a write-ahead log and temporary data in
/// memory. Its file and its log reach the disk encrypted; beside them, while it is open,
/// SQLite's shared memory, which holds the index of the log and SQLite's checksums of its
/// frames, unencrypted, as the technical design's section on several programs on one replica explains. Several
/// processes may have it open at once, reading together and writing in turn.
pub struct EncryptedDatabase {
	/// The connection, which is dropped first, so that the database is closed before its
	/// key is forgotten.
	connection: Connection,
	key: HeldKey,
}

/// A database's key, held by the VFS while the database is open, and forgotten when this
/// is dropped.
struct HeldKey {
	path: PathBuf,
	keys: Arc<Keys>,
}

impl Drop for HeldKey {
	fn drop(&mut self) {
		if let Ok(mut keys) = self.keys.lock()
			&& let Some((_, connections)) = keys.get_mut(&self.path)
		{
			*connections -= 1;
			if *connections == 0 {
				keys.remove(&self.path);
			}
		}
	}
}

impl EncryptedDatabase {
	/// Opens the database at `path` with `key`, creating it if it does not exist. A new
	/// database gets a random key of its own, which its header holds sealed under `key`.
	/// `rng` makes that key and the nonces of what is written, so it must be a
	/// cryptographically secure generator, such as the operating system's.
	///
	/// Other processes may have the database open at the same time, and other connections
	/// of this one, through SQLite's own locks. Within this program, whatever path names
	/// it, its connections share the key it was first opened with, and what SQLite's default
	/// VFS keeps of the file.
	///
	/// # Errors
	///
	/// Returns [`StoreError::Newer`] if its header names a later version of the format,
	/// which only a newer program reads, [`StoreError::InUse`] if the database is open
	/// already in this program under another key, and [`StoreError::Database`] if it is not
	/// one that `key` opens, or cannot be opened.
	pub fn open(
		path: &Path,
		key: &Key,
		rng: impl CryptoRng + Send + 'static,
	) -> Result<Self, StoreError> {
		if of_later_version(path) {
			return Err(StoreError::Newer);
		}
		let keys = registered_keys()?;
		let path = full_path(path)?;
		{
			let mut held = keys.lock().map_err(|_| StoreError::Broken)?;
			match held.get_mut(&path) {
				Some((keyed, connections)) if keyed.opened_with.same_as(key) => {
					*connections += 1;
				}
				Some(_) => return Err(StoreError::InUse),
				None => {
					let keyed = Keyed {
						opened_with: key.clone(),
						wrapping: PageCipher::new(key),
						own: OnceLock::new(),
						rng: Mutex::new(Box::new(rng)),
					};
					held.insert(path.clone(), (Arc::new(keyed), 1));
				}
			}
		}
		// From here on, a failure forgets the key again.
		let key = HeldKey { path, keys };
		let mut connection =
			Connection::open_with_flags_and_vfs(&key.path, OpenFlags::default(), VFS_NAME)
				.map_err(StoreError::Database)?;
		settle(&mut connection)?;
		Ok(Self { connection, key })
	}

	/// The connection to the database.
	#[must_use]
	pub fn connection(&self) -> &Connection {
		&self.connection
	}

	/// The connection to the database, for transactions. Its `transaction` takes the write
	/// lock when it begins, so that one that reads and then writes never meets a change
	/// made in between, and holds it until it ends; a long transaction that only reads
	/// should begin as a deferred one, with `transaction_with_behavior`, so that it does not
	/// keep writers of other processes waiting.
	pub fn connection_mut(&mut self) -> &mut Connection {
		&mut self.connection
	}
}

impl std::fmt::Debug for EncryptedDatabase {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// The path, and nothing of the key.
		formatter
			.debug_struct("EncryptedDatabase")
			.field("path", &self.key.path)
			.finish_non_exhaustive()
	}
}

/// Sets a connection up as the VFS needs it: waiting for the locks of other connections,
/// temporary data in memory, pages of [`PAGE`] bytes, a write-ahead log, and full
/// synchronization. Other connections, of this program or others, may use the database at
/// the same time ([technical design: several programs on one replica]).
///
/// [technical design: several programs on one replica]: ../../../docs/Technical-design.md#78-several-programs-on-one-replica
fn settle(connection: &mut Connection) -> Result<(), StoreError> {
	connection
		.busy_timeout(BUSY_TIMEOUT)
		.map_err(StoreError::Database)?;
	// Every transaction takes the write lock at once, waiting for it: one that read first
	// and wrote later would fail when another connection wrote in between.
	connection.set_transaction_behavior(TransactionBehavior::Immediate);
	crate::file::steady_plans(connection).map_err(StoreError::Database)?;
	connection
		.pragma_update(None, "temp_store", "MEMORY")
		.map_err(StoreError::Database)?;
	// A new file switches to the write-ahead log in its first transaction, whose rollback
	// journal the VFS keeps in memory: a crash loses nothing of a file that holds nothing.
	// A file that is in the write-ahead log already stays as it is, whoever made it.
	connection
		.pragma_update(None, "page_size", i64::from(PAGE_SIZE))
		.map_err(StoreError::Database)?;
	// SQLite reports at once, rather than waiting, that another connection is making the
	// same switch: then this one asks again until the switch is made.
	let started = std::time::Instant::now();
	let mode: String = loop {
		match connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0)) {
			Err(error)
				if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
					&& started.elapsed() < BUSY_TIMEOUT =>
			{
				std::thread::sleep(Duration::from_millis(10));
			}
			result => break result.map_err(StoreError::Database)?,
		}
	};
	if !mode.eq_ignore_ascii_case("wal") {
		return Err(StoreError::Unsupported("write-ahead logging"));
	}
	crate::file::synchronize_fully(connection).map_err(StoreError::Database)?;
	let page: i64 = connection
		.query_row("PRAGMA page_size", [], |row| row.get(0))
		.map_err(StoreError::Database)?;
	if usize::try_from(page) != Ok(PAGE) {
		return Err(StoreError::Damaged(
			"an encrypted database has pages of another size".to_owned(),
		));
	}
	Ok(())
}

/// The VFS's keys, registering the VFS with SQLite the first time.
fn registered_keys() -> Result<Arc<Keys>, StoreError> {
	static KEYS: OnceLock<Option<Arc<Keys>>> = OnceLock::new();
	KEYS.get_or_init(|| {
		let keys = Arc::new(Keys::default());
		register(VFS_NAME, EncryptingVfs(Arc::clone(&keys)))
			.ok()
			.map(|()| keys)
	})
	.clone()
	.ok_or(StoreError::Unsupported("an encrypting VFS for SQLite"))
}

/// The VFS: it opens databases whose keys it holds, and their write-ahead logs.
struct EncryptingVfs(Arc<Keys>);

impl Vfs for EncryptingVfs {
	type File = OpenFile;

	fn accepts(&self, path: &Path, kind: FileKind) -> Result<(), Error> {
		// A database or its log needs the database's key, before any file is made.
		let database = match kind {
			FileKind::Database => path.to_owned(),
			FileKind::WriteAheadLog => without_suffix(path, "-wal").ok_or(Error::CannotOpen)?,
			FileKind::Journal => without_suffix(path, "-journal").ok_or(Error::CannotOpen)?,
			// Temporary files would reach the disk unencrypted.
			FileKind::Other => return Err(Error::CannotOpen),
		};
		self.keyed(&database).map(|_| ())
	}

	fn open(
		&self,
		path: &Path,
		kind: FileKind,
		raw: Option<&mut RawFile>,
	) -> Result<OpenFile, Error> {
		let (kind, database) = match kind {
			FileKind::Database => (Kind::Database, path.to_owned()),
			FileKind::WriteAheadLog => (
				Kind::Log,
				without_suffix(path, "-wal").ok_or(Error::CannotOpen)?,
			),
			FileKind::Journal => {
				// Only the first transaction of a new database has one, which switches it to
				// the write-ahead log; it stays in memory, so that nothing reaches the disk
				// unencrypted, as a database with no pages loses nothing in a crash. A
				// database with pages never leaves the write-ahead log, which needs a
				// journal that a crash cannot lose.
				let database = without_suffix(path, "-journal").ok_or(Error::CannotOpen)?;
				self.keyed(&database)?;
				// A database whose size cannot be found counts as one with pages.
				let size = std::fs::metadata(&database).map_or(u64::MAX, |metadata| metadata.len());
				if size > HEADER as u64 {
					return Err(Error::CannotOpen);
				}
				return Ok(OpenFile::Journal(Vec::new()));
			}
			// Temporary files would reach the disk unencrypted.
			FileKind::Other => return Err(Error::CannotOpen),
		};
		let keyed = self.keyed(&database)?;
		if kind == Kind::Log {
			// Until the key is known, a log reads as holding nothing.
			let _ = keep_key_from_disk(&keyed, &database);
		}
		let raw = raw.ok_or(Error::CannotOpen)?;
		// The default VFS has made the file already, if it was new: a new database has
		// nothing in it.
		let existed = raw.size()? > 0;
		let mut opened = EncryptedFile {
			path: path.to_owned(),
			kind,
			keyed,
			has_header: false,
			// SQLite's VFS syncs the directory of a new log itself, not of a new database.
			sync_directory: kind == Kind::Database && !existed,
			buffer: Vec::new(),
		};
		// A file with pages has its header, which is synced before any page: one whose
		// header does not open is under another key. A file with no more than a header may
		// be being made by another process at this moment, before SQLite holds a lock: its
		// header is checked when it is first read or written, under SQLite's locks.
		if kind == Kind::Database && physical_size(raw)? > HEADER {
			opened.check_header(raw)?;
			opened.has_header = true;
		}
		Ok(OpenFile::Encrypted(opened))
	}
}

impl EncryptingVfs {
	/// What encrypts the database at `database`, if it was opened with a key.
	fn keyed(&self, database: &Path) -> Result<Arc<Keyed>, Error> {
		self.0
			.lock()
			.map_err(|_| Error::CannotOpen)?
			.get(database)
			.map(|(keyed, _)| Arc::clone(keyed))
			.ok_or(Error::CannotOpen)
	}
}

/// The database whose write-ahead log or journal is at `path`: the same path without
/// `suffix`.
fn without_suffix(path: &Path, suffix: &str) -> Option<PathBuf> {
	let name = path.file_name()?;
	#[cfg(unix)]
	let database = {
		use std::os::unix::ffi::OsStrExt;
		std::ffi::OsStr::from_bytes(name.as_bytes().strip_suffix(suffix.as_bytes())?)
	};
	#[cfg(not(unix))]
	let database = name.to_str()?.strip_suffix(suffix)?;
	Some(path.with_file_name(database))
}

/// A file that the VFS opened: a database or its write-ahead log, encrypted on disk, or the
/// rollback journal of a new database's first transaction, in memory.
enum OpenFile {
	Encrypted(EncryptedFile),
	Journal(Vec<u8>),
}

/// The raw file of a database or its log, which always has one.
fn lent(raw: Option<&mut RawFile>) -> Result<&mut RawFile, Error> {
	raw.ok_or(Error::Read)
}

impl nibkeep_sqlite_vfs::File for OpenFile {
	fn read(
		&mut self,
		raw: Option<&mut RawFile>,
		offset: u64,
		buffer: &mut [u8],
	) -> Result<usize, Error> {
		match self {
			Self::Encrypted(file) => file.read(lent(raw)?, offset, buffer),
			Self::Journal(journal) => {
				let start =
					usize::try_from(offset).map_or(journal.len(), |start| start.min(journal.len()));
				let count = (journal.len() - start).min(buffer.len());
				buffer[..count].copy_from_slice(&journal[start..start + count]);
				Ok(count)
			}
		}
	}

	fn write(&mut self, raw: Option<&mut RawFile>, offset: u64, data: &[u8]) -> Result<(), Error> {
		match self {
			Self::Encrypted(file) => file.write(lent(raw)?, offset, data),
			Self::Journal(journal) => {
				let start = usize::try_from(offset).map_err(|_| Error::Write)?;
				if journal.len() < start + data.len() {
					journal.resize(start + data.len(), 0);
				}
				journal[start..start + data.len()].copy_from_slice(data);
				Ok(())
			}
		}
	}

	fn truncate(&mut self, raw: Option<&mut RawFile>, size: u64) -> Result<(), Error> {
		match self {
			Self::Encrypted(file) => file.truncate(lent(raw)?, size),
			Self::Journal(journal) => {
				journal.truncate(usize::try_from(size).map_err(|_| Error::Truncate)?);
				Ok(())
			}
		}
	}

	fn sync(&mut self, raw: Option<&mut RawFile>, sync: SyncMode) -> Result<(), Error> {
		match self {
			Self::Encrypted(file) => file.sync(lent(raw)?, sync),
			Self::Journal(_) => Ok(()),
		}
	}

	fn size(&mut self, raw: Option<&mut RawFile>) -> Result<u64, Error> {
		match self {
			Self::Encrypted(file) => file.size(lent(raw)?),
			Self::Journal(journal) => Ok(journal.len() as u64),
		}
	}
}

/// Which of the two files a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Database,
	Log,
}

/// One sealed record: where it is in the file that SQLite sees, and on disk.
#[derive(Clone, Copy, Debug)]
struct Record {
	/// Its place among the file's records, which its associated data names.
	index: u64,
	/// Where what it holds starts in the file that SQLite sees, and how long it is.
	start: usize,
	length: usize,
	/// Where it starts on disk.
	stored: usize,
	/// Whether it is a header of the write-ahead log or of one of its frames.
	header: bool,
}

impl Kind {
	/// The record that holds byte `at` of the file that SQLite sees.
	fn record(self, at: usize) -> Record {
		match self {
			Self::Database => {
				let page = at / PAGE;
				Record {
					index: page as u64,
					start: page * PAGE,
					length: PAGE,
					stored: stored_page(page),
					header: false,
				}
			}
			Self::Log if at < LOG_HEADER => Record {
				index: 0,
				start: 0,
				length: LOG_HEADER,
				stored: 0,
				header: true,
			},
			Self::Log => {
				let frame = (at - LOG_HEADER) / FRAME;
				let start = LOG_HEADER + frame * FRAME;
				let stored = LOG_HEADER + RECORD_OVERHEAD + frame * STORED_FRAME;
				if at - start < FRAME_HEADER {
					Record {
						index: 1 + 2 * frame as u64,
						start,
						length: FRAME_HEADER,
						stored,
						header: true,
					}
				} else {
					Record {
						index: 2 + 2 * frame as u64,
						start: start + FRAME_HEADER,
						length: PAGE,
						stored: stored + FRAME_HEADER + RECORD_OVERHEAD,
						header: false,
					}
				}
			}
		}
	}

	/// The size that SQLite sees of a file of `stored` bytes: its whole records.
	fn size(self, stored: usize) -> usize {
		match self {
			Self::Database => {
				let pages = if stored <= stored_page(FIRST_MOVED - 1) + STORED_PAGE {
					stored.saturating_sub(HEADER) / STORED_PAGE
				} else {
					FIRST_MOVED + stored.saturating_sub(LOCKED.end) / STORED_PAGE
				};
				pages * PAGE
			}
			Self::Log if stored < LOG_HEADER + RECORD_OVERHEAD => 0,
			Self::Log => {
				let frames = stored - LOG_HEADER - RECORD_OVERHEAD;
				let header = if frames % STORED_FRAME >= FRAME_HEADER + RECORD_OVERHEAD {
					FRAME_HEADER
				} else {
					0
				};
				LOG_HEADER + frames / STORED_FRAME * FRAME + header
			}
		}
	}

	/// The size on disk of a file that SQLite cuts to `size` bytes, the start of a record:
	/// for a database, the end of its last page.
	fn stored_size(self, size: usize) -> Result<usize, Error> {
		if size == 0 {
			return Ok(0);
		}
		if self == Self::Database {
			return if size.is_multiple_of(PAGE) {
				Ok(stored_page(size / PAGE - 1) + STORED_PAGE)
			} else {
				Err(Error::Truncate)
			};
		}
		let record = self.record(size);
		if record.start == size {
			Ok(record.stored)
		} else {
			Err(Error::Truncate)
		}
	}

	/// The associated data of the record `index`: which file it is in, and its place.
	fn associated_data(self, index: u64) -> [u8; 9] {
		let mut data = [0; 9];
		data[0] = match self {
			Self::Database => b'D',
			Self::Log => b'W',
		};
		data[1..].copy_from_slice(&index.to_le_bytes());
		data
	}
}

/// A page as stored: its nonce, the page encrypted, and its tag.
const STORED_PAGE: usize = PAGE + RECORD_OVERHEAD;

/// The bytes of a database file that SQLite locks: from its "pending byte", at 1 GiB, to the
/// end of the range of shared locks, 512 bytes on. SQLite keeps them out of its own layout,
/// since locks on Windows keep other connections from reading and writing them; no record
/// is stored there either.
const LOCKED: std::ops::Range<usize> = 0x4000_0000..0x4000_0200;

/// The first page whose record would reach the locked bytes, and how far it and every page
/// after it are moved, to start after them.
const FIRST_MOVED: usize = (LOCKED.start - HEADER) / STORED_PAGE;
const MOVED_BY: usize = LOCKED.end - (HEADER + FIRST_MOVED * STORED_PAGE);

/// Where page `page` of a database starts on disk.
const fn stored_page(page: usize) -> usize {
	let start = HEADER + page * STORED_PAGE;
	if page < FIRST_MOVED {
		start
	} else {
		start + MOVED_BY
	}
}

/// A frame of the write-ahead log, as SQLite sees it and as stored.
const FRAME: usize = FRAME_HEADER + PAGE;
const STORED_FRAME: usize = FRAME_HEADER + PAGE + 2 * RECORD_OVERHEAD;

/// An open database or write-ahead log.
/// Every byte is read and written through the raw file that each call is lent, the file
/// as SQLite's default VFS opened it, whose locks and shared memory SQLite uses.
struct EncryptedFile {
	path: PathBuf,
	kind: Kind,
	keyed: Arc<Keyed>,
	/// Whether a database has its header on disk.
	has_header: bool,
	/// Whether the directory must be synchronized at the next sync, as the file is new.
	sync_directory: bool,
	/// Room for one sealed record, reused.
	buffer: Vec<u8>,
}

/// The size of a raw file, as stored.
fn physical_size(raw: &mut RawFile) -> Result<usize, Error> {
	usize::try_from(raw.size()?).map_err(|_| Error::Size)
}

impl EncryptedFile {
	/// The header of a new database, and the database's own key: the one kept already, or
	/// a new random one. The header holds the magic, the size of a page, and the key,
	/// sealed in a record that authenticates them.
	fn header(&self) -> Result<([u8; HEADER], Key), Error> {
		let mut header = [0; HEADER];
		header[..MAGIC.len()].copy_from_slice(MAGIC);
		header[MAGIC.len()..MAGIC.len() + 4].copy_from_slice(&u32::from(PAGE_SIZE).to_le_bytes());
		let mut rng = self.keyed.rng.lock().map_err(|_| Error::Write)?;
		let key = match self.keyed.own.get() {
			Some((key, _)) => key.clone(),
			None => Key::generate(&mut **rng),
		};
		// Sealed in place right away, so that nothing can fail while the key's bytes are
		// in the clear.
		let (fields, record) = header.split_at_mut(HEADER_FIELDS);
		record[nibkeep_crypto::PAGE_NONCE_LEN..][..KEY].copy_from_slice(key.expose_bytes());
		self.keyed
			.wrapping
			.seal(&mut **rng, &header_data(fields), record);
		Ok((header, key))
	}

	/// Checks the header of a database, and keeps its key: its magic, its size of a page,
	/// and that its key opens with the key the database was opened with.
	fn check_header(&self, raw: &mut RawFile) -> Result<(), Error> {
		let mut header = [0; HEADER];
		if !raw.read(0, &mut header)? {
			return Err(Error::NotADatabase);
		}
		check_header(&self.keyed, &mut header)
	}
}

/// Whether the file at `path` is a database of this kind whose header names a later version
/// of the format than this program reads. A header being written reads as one of no
/// version, never a later one.
fn of_later_version(path: &Path) -> bool {
	use std::io::Read as _;
	let mut start = [0; MAGIC.len()];
	let read = File::open(path).and_then(|mut file| file.read_exact(&mut start));
	let (kind, version) = MAGIC.split_at(MAGIC.len() - 1);
	read.is_ok() && start.starts_with(kind) && start[kind.len()] > version[0]
}

/// Checks a database's `header`, as [`EncryptedFile::check_header`] does, and wipes it.
fn check_header(keyed: &Keyed, header: &mut [u8; HEADER]) -> Result<(), Error> {
	let (fields, record) = header.split_at_mut(HEADER_FIELDS);
	let sound = fields.starts_with(MAGIC)
		&& fields[MAGIC.len()..MAGIC.len() + 4] == u32::from(PAGE_SIZE).to_le_bytes()
		&& fields[MAGIC.len() + 4..] == [0; 4];
	let key = if sound {
		keyed
			.wrapping
			.open(&header_data(fields), record)
			.ok()
			.and_then(Key::from_slice)
	} else {
		None
	};
	header.zeroize();
	keyed.keep(&key.ok_or(Error::NotADatabase)?)
}

/// Keeps the key of the database at `database` from its header on disk, when its
/// write-ahead log is opened and this program has not read or written the header yet.
/// SQLite writes a database's first pages to the database itself, so a log with frames
/// has a database with its header beside it.
fn keep_key_from_disk(keyed: &Keyed, database: &Path) -> Result<(), Error> {
	if keyed.own.get().is_some() {
		return Ok(());
	}
	let mut header = [0; HEADER];
	let mut file = File::open(database).map_err(|_| Error::Read)?;
	std::io::Read::read_exact(&mut file, &mut header).map_err(|_| Error::Read)?;
	check_header(keyed, &mut header)
}

/// The associated data of the header's sealed record: its fields, marked apart from any
/// record's.
fn header_data(fields: &[u8]) -> Vec<u8> {
	[b"H".as_slice(), fields].concat()
}

impl EncryptedFile {
	fn read(&mut self, raw: &mut RawFile, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
		let offset = usize::try_from(offset).map_err(|_| Error::Read)?;
		let end = offset + buffer.len();
		// A read that starts at a header of the write-ahead log is SQLite recovering the
		// log, which checks what it reads: there a frame that does not open reads as zeros,
		// from its header on, which SQLite's checks take as the end of the log, as they
		// would a frame that a crash cut short.
		let recovering = self.kind == Kind::Log && self.kind.record(offset).header;
		if self.kind == Kind::Database && !self.has_header && physical_size(raw)? > HEADER {
			// A page was written since this connection opened the file, after the header was
			// synced: the header is whole. A file of no more than its header reads as empty:
			// SQLite reads its first bytes when it opens a database, without a lock, while
			// another process may be writing the header.
			self.check_header(raw)?;
			self.has_header = true;
		}
		let mut at = offset;
		while at < end {
			let record = self.kind.record(at);
			self.buffer.resize(record.length + RECORD_OVERHEAD, 0);
			if !raw.read(record.stored as u64, &mut self.buffer)? {
				// The file ends before this record does.
				return Ok(at - offset);
			}
			if self.kind == Kind::Database && self.keyed.cipher().is_none() {
				// The file was no more than its header when this read began, and has pages
				// now: its header is whole.
				self.check_header(raw)?;
				self.has_header = true;
			}
			let associated = self.kind.associated_data(record.index);
			let opened = self
				.keyed
				.cipher()
				.map(|cipher| cipher.open(&associated, &mut self.buffer));
			let content = match opened {
				Some(Ok(content)) => content,
				// A log whose database's key is unknown holds nothing that can be read.
				Some(Err(_)) | None if recovering => {
					// The frame's header, the log's header for the first record, and
					// everything after it in this read.
					let frame = if record.index == 0 || record.header {
						record.start
					} else {
						record.start - FRAME_HEADER
					};
					buffer[frame.max(offset) - offset..].fill(0);
					return Ok(buffer.len());
				}
				Some(Err(_)) => return Err(Error::Damaged),
				None => return Err(Error::Read),
			};
			let from = at - record.start;
			let count = (record.length - from).min(end - at);
			buffer[at - offset..at - offset + count].copy_from_slice(&content[from..from + count]);
			at += count;
		}
		Ok(buffer.len())
	}

	fn write(&mut self, raw: &mut RawFile, offset: u64, data: &[u8]) -> Result<(), Error> {
		let offset = usize::try_from(offset).map_err(|_| Error::Write)?;
		if self.kind == Kind::Database && !self.has_header {
			// SQLite writes under its locks, so no other connection is writing the header.
			if physical_size(raw)? == 0 {
				// Synced before any page, so that no page is ever on disk without it; its key
				// is kept once it is there.
				let (header, key) = self.header()?;
				raw.write(0, &header)?;
				raw.sync(SyncMode::FULL)?;
				self.keyed.keep(&key)?;
			} else {
				// Another connection made the file since this one opened it; or a crash cut
				// the file short while it was made, and its header does not open.
				self.check_header(raw)?;
			}
			self.has_header = true;
		}
		let cipher = self.keyed.cipher().ok_or(Error::Write)?;
		let mut at = offset;
		while at < offset + data.len() {
			let record = self.kind.record(at);
			// SQLite writes whole records: anything else would need a record read first.
			if record.start != at || at + record.length > offset + data.len() {
				return Err(Error::Write);
			}
			self.buffer.resize(record.length + RECORD_OVERHEAD, 0);
			self.buffer[nibkeep_crypto::PAGE_NONCE_LEN..][..record.length]
				.copy_from_slice(&data[at - offset..at - offset + record.length]);
			let associated = self.kind.associated_data(record.index);
			{
				let mut rng = self.keyed.rng.lock().map_err(|_| Error::Write)?;
				cipher.seal(&mut **rng, &associated, &mut self.buffer);
			}
			raw.write(record.stored as u64, &self.buffer)?;
			at += record.length;
		}
		Ok(())
	}

	fn truncate(&mut self, raw: &mut RawFile, size: u64) -> Result<(), Error> {
		let size = usize::try_from(size).map_err(|_| Error::Truncate)?;
		let stored = self.kind.stored_size(size)?;
		raw.truncate(stored as u64)?;
		if stored == 0 {
			self.has_header = false;
		}
		Ok(())
	}

	fn sync(&mut self, raw: &mut RawFile, sync: SyncMode) -> Result<(), Error> {
		raw.sync(sync)?;
		if self.sync_directory {
			sync_directory_of(&self.path)?;
			self.sync_directory = false;
		}
		Ok(())
	}

	fn size(&mut self, raw: &mut RawFile) -> Result<u64, Error> {
		Ok(self.kind.size(physical_size(raw)?) as u64)
	}
}

/// Synchronizes the directory of `path`, so that a file made or deleted in it stays so.
/// Windows has no such thing, nor needs it.
#[cfg_attr(
	not(unix),
	expect(clippy::unnecessary_wraps, reason = "only Unix syncs a directory")
)]
fn sync_directory_of(path: &Path) -> Result<(), Error> {
	#[cfg(unix)]
	{
		let directory = path.parent().ok_or(Error::SyncDirectory)?;
		File::open(directory)
			.and_then(|directory| directory.sync_all())
			.map_err(|_| Error::SyncDirectory)
	}
	#[cfg(not(unix))]
	{
		let _ = path;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The pages around the locked bytes, and a few at the start.
	fn pages() -> impl Iterator<Item = usize> {
		(0..5).chain(FIRST_MOVED - 5..FIRST_MOVED + 5)
	}

	#[test]
	fn no_page_is_stored_in_the_locked_bytes() {
		for page in pages() {
			let stored = stored_page(page)..stored_page(page) + STORED_PAGE;
			assert!(
				stored.end <= LOCKED.start || stored.start >= LOCKED.end,
				"page {page} at {stored:?}"
			);
			assert_eq!(Kind::Database.record(page * PAGE).stored, stored.start);
		}
		assert_eq!(stored_page(FIRST_MOVED), LOCKED.end);
		assert!(stored_page(FIRST_MOVED - 1) + STORED_PAGE <= LOCKED.start);
	}

	#[test]
	fn pages_follow_one_another_without_overlapping() {
		for page in pages().filter(|page| *page > 0) {
			assert!(
				stored_page(page - 1) + STORED_PAGE <= stored_page(page),
				"page {page}"
			);
		}
	}

	#[test]
	fn sizes_convert_both_ways() {
		for pages in pages() {
			let size = pages * PAGE;
			let stored = Kind::Database.stored_size(size).expect("whole pages");
			assert_eq!(Kind::Database.size(stored), size, "{pages} pages");
			// A page cut short counts as not there.
			if stored > HEADER + 1 {
				assert_eq!(Kind::Database.size(stored + STORED_PAGE - 1), size);
			}
		}
		// The gap holds no page.
		let end = stored_page(FIRST_MOVED - 1) + STORED_PAGE;
		for stored in [end, end + 1, LOCKED.start, LOCKED.end - 1, LOCKED.end] {
			assert_eq!(Kind::Database.size(stored), FIRST_MOVED * PAGE, "{stored}");
		}
	}
}
