//! Lets a VFS written in safe Rust serve SQLite: the layer through which SQLite reads and
//! writes its files. This is the one crate of the engine with `unsafe` code, which
//! turns SQLite's calls into the methods of [`Vfs`] and [`File`], and nothing more, as
//! the technical design decided (`docs/Technical-design.md`, sections 7.5 and 7.8). Every
//! `unsafe` block says why it is sound.
//!
//! A VFS registered here is a layer over SQLite's default VFS. Every file it opens is also
//! opened by the default VFS, as a [`RawFile`], through which the VFS reads and writes its
//! own bytes. Everything else goes to the default VFS unchanged:
//!
//! - **Locks and shared memory,** so that several processes use a database at once, reading
//!   together and writing in turn, as with SQLite's own VFS. No process keeps another
//!   handle to the file, whose closing would drop the locks of the process on Unix.
//! - **Deleting files and finding them,** the time, randomness and the rest.
//!
//! What it adds:
//!
//! - **Temporary data in memory.** Temporary files, which have no name, are refused.
//! - **What the disk promises.** A power cut damages only what was being written, which is
//!   what SQLite's own VFS assumes by default on Unix; nothing more is promised to SQLite.
//! - **No memory mapping,** so that every read goes through the VFS.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr;

use libsqlite3_sys as ffi;

/// The size of a sector that SQLite is told, as its own VFS tells it on Unix.
const SECTOR_SIZE: c_int = 4096;

/// The longest path the VFS hands SQLite.
const MAX_PATH: c_int = 4096;

/// Which of SQLite's files a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
	/// A database.
	Database,
	/// The write-ahead log of a database.
	WriteAheadLog,
	/// The rollback journal of a database, which a database in WAL mode does not use.
	Journal,
	/// Anything else, such as a file of temporary data.
	Other,
}

/// Why a call into the VFS failed, as SQLite is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	/// The file cannot be opened: it does not exist, or the VFS refuses it.
	CannotOpen,
	/// The file is not a database that the VFS can open, such as one under another key.
	NotADatabase,
	/// The file holds data that does not check out.
	Damaged,
	/// Reading failed.
	Read,
	/// Writing failed.
	Write,
	/// Truncating failed.
	Truncate,
	/// Synchronizing the file failed.
	Sync,
	/// Synchronizing the file's directory failed.
	SyncDirectory,
	/// Finding the file's size failed.
	Size,
}

impl Error {
	fn code(self) -> c_int {
		match self {
			Self::CannotOpen => ffi::SQLITE_CANTOPEN,
			Self::NotADatabase => ffi::SQLITE_NOTADB,
			Self::Damaged => ffi::SQLITE_IOERR_DATA,
			Self::Read => ffi::SQLITE_IOERR_READ,
			Self::Write => ffi::SQLITE_IOERR_WRITE,
			Self::Truncate => ffi::SQLITE_IOERR_TRUNCATE,
			Self::Sync => ffi::SQLITE_IOERR_FSYNC,
			Self::SyncDirectory => ffi::SQLITE_IOERR_DIR_FSYNC,
			Self::Size => ffi::SQLITE_IOERR_FSTAT,
		}
	}
}

/// A VFS: how SQLite opens its files.
pub trait Vfs: Send + Sync + 'static {
	/// The files it opens.
	type File: File;

	/// Whether the VFS takes the file at `path`, an absolute path: asked before the default
	/// VFS opens it, and so before it is made, if it is new, so that a file the VFS refuses
	/// is never made.
	///
	/// # Errors
	///
	/// Returns an error if the VFS refuses the file.
	fn accepts(&self, path: &Path, kind: FileKind) -> Result<(), Error>;

	/// Opens the file at `path`, an absolute path, if the VFS takes it. A database and its
	/// write-ahead log come with `raw`, the file as SQLite's default VFS opened it; other
	/// files, such as a journal that the VFS keeps in memory, come without.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be opened, or the VFS refuses it.
	fn open(
		&self,
		path: &Path,
		kind: FileKind,
		raw: Option<&mut RawFile>,
	) -> Result<Self::File, Error>;
}

/// An open file, as SQLite sees it. Each call comes with the file's raw file, if it has
/// one, lent for the call: the crate keeps it, so that it is closed with the file, and
/// nothing holds it longer.
pub trait File: Send + 'static {
	/// Reads `buffer.len()` bytes at `offset`, and returns how many it read: fewer only if
	/// the file ends before.
	///
	/// # Errors
	///
	/// Returns an error if reading fails, or what was read does not check out.
	fn read(
		&mut self,
		raw: Option<&mut RawFile>,
		offset: u64,
		buffer: &mut [u8],
	) -> Result<usize, Error>;

	/// Writes `data` at `offset`.
	///
	/// # Errors
	///
	/// Returns an error if writing fails.
	fn write(&mut self, raw: Option<&mut RawFile>, offset: u64, data: &[u8]) -> Result<(), Error>;

	/// Cuts the file to `size` bytes.
	///
	/// # Errors
	///
	/// Returns an error if truncating fails.
	fn truncate(&mut self, raw: Option<&mut RawFile>, size: u64) -> Result<(), Error>;

	/// Synchronizes the file with the disk, as `sync` says.
	///
	/// # Errors
	///
	/// Returns an error if synchronizing fails.
	fn sync(&mut self, raw: Option<&mut RawFile>, sync: SyncMode) -> Result<(), Error>;

	/// The file's size.
	///
	/// # Errors
	///
	/// Returns an error if the size cannot be found.
	fn size(&mut self, raw: Option<&mut RawFile>) -> Result<u64, Error>;
}

/// How SQLite asks a file to be synchronized, which a VFS passes on to its raw file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncMode(c_int);

impl SyncMode {
	/// A full synchronization, as for a file just made.
	pub const FULL: Self = Self(ffi::SQLITE_SYNC_FULL);
}

/// Opens a file through SQLite's default VFS.
///
/// # Safety
///
/// `default` must be SQLite's default VFS, and `name` the name that SQLite handed `xOpen`,
/// which stays valid until the file is closed; the raw file must be closed before then,
/// which it is, as it lives beside the file it belongs to.
unsafe fn open_raw(
	default: *mut ffi::sqlite3_vfs,
	name: ffi::sqlite3_filename,
	flags: c_int,
) -> Result<RawFile, Error> {
	// SAFETY: `default` is SQLite's default VFS, which lives as long as the program.
	let size = usize::try_from(unsafe { (*default).szOsFile }).map_err(|_| Error::CannotOpen)?;
	// Memory for the default VFS's file, aligned to 8 bytes as SQLite's allocator is.
	let mut memory = vec![0_u64; size.div_ceil(8).max(1)].into_boxed_slice();
	let file = memory.as_mut_ptr().cast::<ffi::sqlite3_file>();
	let mut out_flags = 0;
	// SAFETY: `default` has `xOpen` as every VFS does; `name` is valid as the caller
	// promises; `file` has the `szOsFile` bytes the default VFS asks.
	let code = unsafe {
		match (*default).xOpen {
			Some(open) => open(default, name, file, flags, &raw mut out_flags),
			None => ffi::SQLITE_CANTOPEN,
		}
	};
	let raw = RawFile {
		memory,
		flags: out_flags,
	};
	if code == ffi::SQLITE_OK {
		Ok(raw)
	} else {
		// Dropping it closes it if the default VFS set its methods despite failing, as
		// SQLite's contract for `xOpen` allows.
		drop(raw);
		Err(Error::CannotOpen)
	}
}

/// A file as SQLite's default VFS opened it: its bytes as they are on disk, its locks and
/// its shared memory. It is closed when dropped.
pub struct RawFile {
	memory: Box<[u64]>,
	/// The flags that the default VFS opened it with, such as read-only where it could
	/// not open it for writing.
	flags: c_int,
}

// SAFETY: the default VFS's file is used by one thread at a time, as SQLite uses the file
// of a connection, and holds nothing tied to the thread that opened it.
unsafe impl Send for RawFile {}

impl RawFile {
	fn file(&self) -> *mut ffi::sqlite3_file {
		self.memory.as_ptr().cast::<ffi::sqlite3_file>().cast_mut()
	}

	/// The methods of the default VFS's file, if it was opened.
	fn methods(&self) -> Option<&ffi::sqlite3_io_methods> {
		// SAFETY: the memory holds a `sqlite3_file`, whose `pMethods` is null or points to
		// methods that live as long as the default VFS.
		unsafe { (*self.file()).pMethods.as_ref() }
	}

	/// Reads `buffer.len()` bytes at `offset`. Returns whether all of them were there: if the
	/// file ends before, the rest of `buffer` is zeros.
	///
	/// # Errors
	///
	/// Returns [`Error::Read`] if reading fails.
	pub fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<bool, Error> {
		let (Some(read), Ok(length), Ok(offset)) = (
			self.methods().and_then(|methods| methods.xRead),
			c_int::try_from(buffer.len()),
			i64::try_from(offset),
		) else {
			return Err(Error::Read);
		};
		// SAFETY: the file is open; `buffer` has `length` writable bytes.
		let code = unsafe { read(self.file(), buffer.as_mut_ptr().cast(), length, offset) };
		match code {
			ffi::SQLITE_OK => Ok(true),
			ffi::SQLITE_IOERR_SHORT_READ => Ok(false),
			_ => Err(Error::Read),
		}
	}

	/// Writes `data` at `offset`.
	///
	/// # Errors
	///
	/// Returns [`Error::Write`] if writing fails.
	pub fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
		let (Some(write), Ok(length), Ok(offset)) = (
			self.methods().and_then(|methods| methods.xWrite),
			c_int::try_from(data.len()),
			i64::try_from(offset),
		) else {
			return Err(Error::Write);
		};
		// SAFETY: the file is open; `data` has `length` readable bytes.
		let code = unsafe { write(self.file(), data.as_ptr().cast(), length, offset) };
		if code == ffi::SQLITE_OK {
			Ok(())
		} else {
			Err(Error::Write)
		}
	}

	/// Cuts the file to `size` bytes.
	///
	/// # Errors
	///
	/// Returns [`Error::Truncate`] if truncating fails.
	pub fn truncate(&mut self, size: u64) -> Result<(), Error> {
		let (Some(truncate), Ok(size)) = (
			self.methods().and_then(|methods| methods.xTruncate),
			i64::try_from(size),
		) else {
			return Err(Error::Truncate);
		};
		// SAFETY: the file is open.
		let code = unsafe { truncate(self.file(), size) };
		if code == ffi::SQLITE_OK {
			Ok(())
		} else {
			Err(Error::Truncate)
		}
	}

	/// Synchronizes the file with the disk, as `sync` says.
	///
	/// # Errors
	///
	/// Returns [`Error::Sync`] if synchronizing fails.
	pub fn sync(&mut self, sync: SyncMode) -> Result<(), Error> {
		let Some(function) = self.methods().and_then(|methods| methods.xSync) else {
			return Err(Error::Sync);
		};
		// SAFETY: the file is open.
		let code = unsafe { function(self.file(), sync.0) };
		if code == ffi::SQLITE_OK {
			Ok(())
		} else {
			Err(Error::Sync)
		}
	}

	/// The file's size.
	///
	/// # Errors
	///
	/// Returns [`Error::Size`] if the size cannot be found.
	pub fn size(&mut self) -> Result<u64, Error> {
		let Some(size) = self.methods().and_then(|methods| methods.xFileSize) else {
			return Err(Error::Size);
		};
		let mut bytes: ffi::sqlite3_int64 = 0;
		// SAFETY: the file is open; `bytes` is an integer to fill.
		let code = unsafe { size(self.file(), &raw mut bytes) };
		match (code, u64::try_from(bytes)) {
			(ffi::SQLITE_OK, Ok(bytes)) => Ok(bytes),
			_ => Err(Error::Size),
		}
	}
}

impl Drop for RawFile {
	fn drop(&mut self) {
		if let Some(close) = self.methods().and_then(|methods| methods.xClose) {
			// SAFETY: the file is open, and is closed once, here.
			unsafe { close(self.file()) };
		}
	}
}

/// Why a VFS could not be registered.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegisterError {
	/// The name is taken by another VFS, or holds a zero byte.
	#[error("the name of the VFS is taken or not valid")]
	Name,
	/// SQLite refused it.
	#[error("SQLite refused the VFS: error {0}")]
	Sqlite(c_int),
}

/// Registers `vfs` with SQLite under `name`, for connections that name it when they open a
/// database. It stays registered as long as the program runs.
///
/// # Errors
///
/// Returns an error if the name is taken or not valid, or SQLite refuses the VFS.
///
/// # Panics
///
/// Panics if an open file of `V` is larger than a C `int` can count, which no file is.
pub fn register<V: Vfs>(name: &str, vfs: V) -> Result<(), RegisterError> {
	// SQLite allocates the memory of an open file aligned to 8 bytes, as its own
	// allocator does: a file of `V` must not need more.
	const {
		assert!(
			align_of::<OpenFile<V::File>>() <= 8,
			"a file of the VFS needs more alignment than SQLite gives"
		);
	};
	let name = CString::new(name).map_err(|_| RegisterError::Name)?;
	// SAFETY: `sqlite3_initialize` may be called at any time, and more than once.
	let code = unsafe { ffi::sqlite3_initialize() };
	if code != ffi::SQLITE_OK {
		return Err(RegisterError::Sqlite(code));
	}
	// SAFETY: `name` is a valid C string; finding a VFS only reads SQLite's list of them.
	if !unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null() {
		return Err(RegisterError::Name);
	}
	// SAFETY: a null name asks for the default VFS, which SQLite always has once it is
	// initialized.
	let default = unsafe { ffi::sqlite3_vfs_find(ptr::null()) };
	if default.is_null() {
		return Err(RegisterError::Sqlite(ffi::SQLITE_ERROR));
	}
	// What SQLite holds a pointer to is never freed: SQLite may use a registered VFS until
	// the program ends.
	let state: &'static State<V> = Box::leak(Box::new(State {
		vfs,
		default,
		methods: io_methods::<V>(),
	}));
	let name: &'static CStr = Box::leak(name.into_boxed_c_str());
	let registered: &'static mut ffi::sqlite3_vfs = Box::leak(Box::new(ffi::sqlite3_vfs {
		iVersion: 2,
		szOsFile: c_int::try_from(size_of::<OpenFile<V::File>>()).expect("a small struct"),
		mxPathname: MAX_PATH,
		pNext: ptr::null_mut(),
		zName: name.as_ptr(),
		pAppData: ptr::from_ref(state).cast_mut().cast(),
		xOpen: Some(x_open::<V>),
		xDelete: Some(x_delete::<V>),
		xAccess: Some(x_access::<V>),
		xFullPathname: Some(x_full_pathname),
		xDlOpen: Some(x_dl_open::<V>),
		xDlError: Some(x_dl_error::<V>),
		xDlSym: Some(x_dl_sym::<V>),
		xDlClose: Some(x_dl_close::<V>),
		xRandomness: Some(x_randomness::<V>),
		xSleep: Some(x_sleep::<V>),
		xCurrentTime: Some(x_current_time::<V>),
		xGetLastError: Some(x_get_last_error::<V>),
		xCurrentTimeInt64: Some(x_current_time_int64::<V>),
		xSetSystemCall: None,
		xGetSystemCall: None,
		xNextSystemCall: None,
	}));
	// SAFETY: `registered` and everything it points to live until the program ends, and
	// its functions follow the contract of a VFS of version 2.
	let code = unsafe { ffi::sqlite3_vfs_register(registered, 0) };
	if code == ffi::SQLITE_OK {
		Ok(())
	} else {
		Err(RegisterError::Sqlite(code))
	}
}

/// What a registered VFS keeps: the VFS, SQLite's default VFS, and the methods of its files.
struct State<V: Vfs> {
	vfs: V,
	default: *mut ffi::sqlite3_vfs,
	methods: ffi::sqlite3_io_methods,
}

// SAFETY: `default` points to SQLite's default VFS, which SQLite never frees, and whose
// functions may be called from any thread; the rest is `Send` and `Sync`.
unsafe impl<V: Vfs> Send for State<V> {}
// SAFETY: as for `Send`.
unsafe impl<V: Vfs> Sync for State<V> {}

/// An open file as SQLite holds it: the `sqlite3_file` that SQLite knows, followed by the
/// file, in the memory of `szOsFile` bytes that SQLite gives `xOpen`.
#[repr(C)]
struct OpenFile<F> {
	base: ffi::sqlite3_file,
	file: F,
	/// The file as SQLite's default VFS opened it, for a database or its log: closed with
	/// this, and only lent to the calls of `file`.
	raw: Option<RawFile>,
}

/// The methods of the files of `V`: version 2, with shared memory but without memory
/// mapping.
fn io_methods<V: Vfs>() -> ffi::sqlite3_io_methods {
	ffi::sqlite3_io_methods {
		iVersion: 2,
		xClose: Some(x_close::<V>),
		xRead: Some(x_read::<V>),
		xWrite: Some(x_write::<V>),
		xTruncate: Some(x_truncate::<V>),
		xSync: Some(x_sync::<V>),
		xFileSize: Some(x_file_size::<V>),
		xLock: Some(x_lock::<V>),
		xUnlock: Some(x_unlock::<V>),
		xCheckReservedLock: Some(x_check_reserved_lock::<V>),
		xFileControl: Some(x_file_control),
		xSectorSize: Some(x_sector_size),
		xDeviceCharacteristics: Some(x_device_characteristics),
		xShmMap: Some(x_shm_map::<V>),
		xShmLock: Some(x_shm_lock::<V>),
		xShmBarrier: Some(x_shm_barrier::<V>),
		xShmUnmap: Some(x_shm_unmap::<V>),
		xFetch: None,
		xUnfetch: None,
	}
}

/// Runs `body`, and turns a panic into `SQLITE_IOERR`: a panic must not unwind into SQLite.
fn guarded(body: impl FnOnce() -> c_int) -> c_int {
	catch_unwind(AssertUnwindSafe(body)).unwrap_or(ffi::SQLITE_IOERR)
}

/// The result of a call into the VFS, as SQLite is told.
fn code(result: Result<(), Error>) -> c_int {
	match result {
		Ok(()) => ffi::SQLITE_OK,
		Err(error) => error.code(),
	}
}

/// The state of the VFS that SQLite calls.
///
/// # Safety
///
/// `vfs` must be a VFS that [`register`] registered for `V`.
unsafe fn state<'a, V: Vfs>(vfs: *mut ffi::sqlite3_vfs) -> &'a State<V> {
	// SAFETY: `register` set `pAppData` to a `State<V>` that is never freed.
	unsafe { &*(*vfs).pAppData.cast::<State<V>>() }
}

/// The file that SQLite calls.
///
/// # Safety
///
/// `file` must be a file that `x_open::<V>` opened, and not yet closed.
unsafe fn open_file<'a, V: Vfs>(file: *mut ffi::sqlite3_file) -> &'a mut OpenFile<V::File> {
	// SAFETY: `x_open` wrote an `OpenFile<V::File>` at `file`, which SQLite uses from one
	// thread at a time.
	unsafe { &mut *file.cast::<OpenFile<V::File>>() }
}

/// A path that SQLite hands the VFS.
///
/// # Safety
///
/// `path` must be null or a valid C string.
unsafe fn path<'a>(path: *const c_char) -> Option<&'a Path> {
	if path.is_null() {
		return None;
	}
	// SAFETY: as the caller promises.
	let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
	#[cfg(unix)]
	{
		use std::os::unix::ffi::OsStrExt;
		Some(Path::new(std::ffi::OsStr::from_bytes(bytes)))
	}
	#[cfg(not(unix))]
	{
		std::str::from_utf8(bytes).ok().map(Path::new)
	}
}

unsafe extern "C" fn x_open<V: Vfs>(
	vfs: *mut ffi::sqlite3_vfs,
	name: ffi::sqlite3_filename,
	file: *mut ffi::sqlite3_file,
	flags: c_int,
	out_flags: *mut c_int,
) -> c_int {
	// SQLite calls `xClose` only if `pMethods` is set, which it is only once the file is.
	// SAFETY: SQLite gives `xOpen` `szOsFile` writable bytes at `file`.
	unsafe { (*file).pMethods = ptr::null() };
	guarded(|| {
		// SAFETY: SQLite calls this VFS's `xOpen` with the VFS it registered.
		let state = unsafe { state::<V>(vfs) };
		// SAFETY: SQLite hands `xOpen` a null name or a valid C string.
		let Some(path) = (unsafe { path(name) }) else {
			// A file without a name holds temporary data, which is kept in memory.
			return ffi::SQLITE_CANTOPEN;
		};
		let kind = if flags & ffi::SQLITE_OPEN_MAIN_DB != 0 {
			FileKind::Database
		} else if flags & ffi::SQLITE_OPEN_WAL != 0 {
			FileKind::WriteAheadLog
		} else if flags & ffi::SQLITE_OPEN_MAIN_JOURNAL != 0 {
			FileKind::Journal
		} else {
			FileKind::Other
		};
		if let Err(error) = state.vfs.accepts(path, kind) {
			return error.code();
		}
		let mut raw = match kind {
			FileKind::Database | FileKind::WriteAheadLog => {
				// SAFETY: `state.default` is SQLite's default VFS; `name` is the name SQLite
				// handed `xOpen`, valid until the file is closed, and the raw file is closed
				// with the file, as it lives in its `OpenFile`.
				match unsafe { open_raw(state.default, name, flags) } {
					Ok(raw) => Some(raw),
					Err(error) => return error.code(),
				}
			}
			FileKind::Journal | FileKind::Other => None,
		};
		match state.vfs.open(path, kind, raw.as_mut()) {
			Ok(opened) => {
				// What the default VFS opened it with, which SQLite must know; a file kept
				// in memory is what SQLite asked for.
				let opened_flags = raw.as_ref().map_or(flags, |raw| raw.flags);
				// SAFETY: `file` has room for an `OpenFile<V::File>`, as `szOsFile` says,
				// and SQLite aligns it to 8 bytes, which `register` checked is enough.
				unsafe {
					ptr::write(
						file.cast::<OpenFile<V::File>>(),
						OpenFile {
							base: ffi::sqlite3_file {
								pMethods: &raw const state.methods,
							},
							file: opened,
							raw,
						},
					);
				}
				if !out_flags.is_null() {
					// SAFETY: SQLite passes null or a pointer to an integer to fill.
					unsafe { *out_flags = opened_flags };
				}
				ffi::SQLITE_OK
			}
			Err(error) => error.code(),
		}
	})
}

/// Calls a function of SQLite's default VFS, all of whose functions here are those of a VFS
/// of version 1.
macro_rules! delegate {
	($name:ident, $field:ident, ($($argument:ident: $type:ty),*) -> $result:ty, $fallback:expr) => {
		unsafe extern "C" fn $name<V: Vfs>(
			vfs: *mut ffi::sqlite3_vfs,
			$($argument: $type),*
		) -> $result {
			// SAFETY: SQLite calls this VFS with the VFS it registered, whose default VFS
			// lives as long as the program and has every function of version 1, and passes
			// on what SQLite gave.
			unsafe {
				let default = state::<V>(vfs).default;
				match (*default).$field {
					Some(function) => function(default, $($argument),*),
					None => $fallback,
				}
			}
		}
	};
}

delegate!(x_delete, xDelete, (name: *const c_char, sync_directory: c_int) -> c_int, ffi::SQLITE_IOERR_DELETE);
delegate!(x_access, xAccess, (name: *const c_char, flags: c_int, out: *mut c_int) -> c_int, ffi::SQLITE_IOERR_ACCESS);
delegate!(x_dl_open, xDlOpen, (name: *const c_char) -> *mut c_void, ptr::null_mut());
delegate!(x_dl_error, xDlError, (length: c_int, out: *mut c_char) -> (), ());
delegate!(
	x_dl_sym,
	xDlSym,
	(handle: *mut c_void, symbol: *const c_char) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)>,
	None
);
delegate!(x_dl_close, xDlClose, (handle: *mut c_void) -> (), ());
delegate!(x_randomness, xRandomness, (length: c_int, out: *mut c_char) -> c_int, 0);
delegate!(x_sleep, xSleep, (microseconds: c_int) -> c_int, 0);
delegate!(x_current_time, xCurrentTime, (out: *mut f64) -> c_int, ffi::SQLITE_ERROR);
delegate!(x_get_last_error, xGetLastError, (length: c_int, out: *mut c_char) -> c_int, 0);

/// The time, in milliseconds since noon in Greenwich on November 24, 4714 BC, as SQLite's
/// default VFS tells it: with its own function of version 2 if it has one, as SQLite does,
/// and otherwise from its time in days.
unsafe extern "C" fn x_current_time_int64<V: Vfs>(
	vfs: *mut ffi::sqlite3_vfs,
	out: *mut ffi::sqlite3_int64,
) -> c_int {
	// SAFETY: SQLite calls this VFS with the VFS it registered, whose default VFS lives as
	// long as the program; a field of version 2 is read only from a VFS of version 2 or
	// later; SQLite passes a pointer to an integer to fill.
	unsafe {
		let default = state::<V>(vfs).default;
		if (*default).iVersion >= 2
			&& let Some(function) = (*default).xCurrentTimeInt64
		{
			return function(default, out);
		}
		let Some(days) = (*default).xCurrentTime else {
			return ffi::SQLITE_ERROR;
		};
		let mut time = 0.0;
		let code = days(default, &raw mut time);
		#[expect(
			clippy::cast_possible_truncation,
			reason = "milliseconds since 4714 BC fit"
		)]
		{
			*out = (time * 86_400_000.0) as ffi::sqlite3_int64;
		}
		code
	}
}

unsafe extern "C" fn x_full_pathname(
	_vfs: *mut ffi::sqlite3_vfs,
	name: *const c_char,
	length: c_int,
	out: *mut c_char,
) -> c_int {
	guarded(|| {
		// SAFETY: SQLite passes a valid C string.
		let Some(path) = (unsafe { path(name) }) else {
			return ffi::SQLITE_CANTOPEN;
		};
		let Ok(absolute) = full_path(path) else {
			return ffi::SQLITE_CANTOPEN;
		};
		let bytes = path_bytes(&absolute);
		let Some(bytes) = bytes else {
			return ffi::SQLITE_CANTOPEN;
		};
		let Ok(length) = usize::try_from(length) else {
			return ffi::SQLITE_CANTOPEN;
		};
		if bytes.len() >= length || bytes.contains(&0) {
			return ffi::SQLITE_CANTOPEN;
		}
		// SAFETY: SQLite gives `length` writable bytes at `out`, and the path and its zero
		// byte fit in them.
		unsafe {
			ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), bytes.len());
			*out.add(bytes.len()) = 0;
		}
		ffi::SQLITE_OK
	})
}

/// A path's bytes, as SQLite takes them: as they are on Unix, and in UTF-8 elsewhere, where
/// a path that is not UTF-8 has none.
#[cfg_attr(
	unix,
	expect(clippy::unnecessary_wraps, reason = "only Unix takes any path")
)]
fn path_bytes(path: &Path) -> Option<Vec<u8>> {
	#[cfg(unix)]
	{
		use std::os::unix::ffi::OsStrExt;
		Some(path.as_os_str().as_bytes().to_vec())
	}
	#[cfg(not(unix))]
	{
		path.to_str().map(|path| path.as_bytes().to_vec())
	}
}

unsafe extern "C" fn x_close<V: Vfs>(file: *mut ffi::sqlite3_file) -> c_int {
	guarded(|| {
		// SAFETY: SQLite closes a file that `x_open` opened once, and uses it no more.
		// Dropping it closes its raw file too, the last thing that holds SQLite's name of
		// the file.
		unsafe { ptr::drop_in_place(file.cast::<OpenFile<V::File>>()) };
		ffi::SQLITE_OK
	})
}

unsafe extern "C" fn x_read<V: Vfs>(
	file: *mut ffi::sqlite3_file,
	buffer: *mut c_void,
	length: c_int,
	offset: ffi::sqlite3_int64,
) -> c_int {
	guarded(|| {
		let (Ok(length), Ok(offset)) = (usize::try_from(length), u64::try_from(offset)) else {
			return ffi::SQLITE_IOERR_READ;
		};
		// SAFETY: SQLite reads a file that `x_open` opened, into `length` writable bytes.
		let (file, buffer) = unsafe {
			(
				open_file::<V>(file),
				std::slice::from_raw_parts_mut(buffer.cast::<u8>(), length),
			)
		};
		match file.file.read(file.raw.as_mut(), offset, buffer) {
			Ok(read) if read >= length => ffi::SQLITE_OK,
			Ok(read) => {
				// SQLite asks that the rest be zeros when a read comes up short.
				buffer[read..].fill(0);
				ffi::SQLITE_IOERR_SHORT_READ
			}
			Err(error) => error.code(),
		}
	})
}

unsafe extern "C" fn x_write<V: Vfs>(
	file: *mut ffi::sqlite3_file,
	data: *const c_void,
	length: c_int,
	offset: ffi::sqlite3_int64,
) -> c_int {
	guarded(|| {
		let (Ok(length), Ok(offset)) = (usize::try_from(length), u64::try_from(offset)) else {
			return ffi::SQLITE_IOERR_WRITE;
		};
		// SAFETY: SQLite writes to a file that `x_open` opened, from `length` readable bytes.
		let (file, data) = unsafe {
			(
				open_file::<V>(file),
				std::slice::from_raw_parts(data.cast::<u8>(), length),
			)
		};
		code(file.file.write(file.raw.as_mut(), offset, data))
	})
}

unsafe extern "C" fn x_truncate<V: Vfs>(
	file: *mut ffi::sqlite3_file,
	size: ffi::sqlite3_int64,
) -> c_int {
	guarded(|| {
		let Ok(size) = u64::try_from(size) else {
			return ffi::SQLITE_IOERR_TRUNCATE;
		};
		// SAFETY: SQLite truncates a file that `x_open` opened.
		let file = unsafe { open_file::<V>(file) };
		code(file.file.truncate(file.raw.as_mut(), size))
	})
}

unsafe extern "C" fn x_sync<V: Vfs>(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
	guarded(|| {
		// SAFETY: SQLite synchronizes a file that `x_open` opened.
		let file = unsafe { open_file::<V>(file) };
		code(file.file.sync(file.raw.as_mut(), SyncMode(flags)))
	})
}

unsafe extern "C" fn x_file_size<V: Vfs>(
	file: *mut ffi::sqlite3_file,
	out: *mut ffi::sqlite3_int64,
) -> c_int {
	guarded(|| {
		// SAFETY: SQLite asks the size of a file that `x_open` opened.
		let file = unsafe { open_file::<V>(file) };
		match file.file.size(file.raw.as_mut()) {
			Ok(size) => {
				let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
					return ffi::SQLITE_IOERR_FSTAT;
				};
				// SAFETY: SQLite passes a pointer to an integer to fill.
				unsafe { *out = size };
				ffi::SQLITE_OK
			}
			Err(error) => error.code(),
		}
	})
}

/// Calls a method of the raw file under a file that `x_open` opened, with what SQLite gave,
/// if the raw file's methods are of `$version` or later.
macro_rules! raw_method {
	($name:ident, $field:ident, $version:literal, ($($argument:ident: $type:ty),*) -> $result:ty, $fallback:expr) => {
		unsafe extern "C" fn $name<V: Vfs>(
			file: *mut ffi::sqlite3_file,
			$($argument: $type),*
		) -> $result {
			catch_unwind(AssertUnwindSafe(|| {
				// SAFETY: SQLite calls a file that `x_open` opened; its raw file is open,
				// and gets what SQLite gave, as SQLite would give it to a file of the
				// default VFS.
				unsafe {
					let Some(raw) = open_file::<V>(file).raw.as_mut() else {
						return $fallback;
					};
					let file = raw.file();
					match raw
						.methods()
						.filter(|methods| methods.iVersion >= $version)
						.and_then(|methods| methods.$field)
					{
						Some(function) => function(file, $($argument),*),
						None => $fallback,
					}
				}
			}))
			// A panic must not unwind into SQLite.
			.unwrap_or($fallback)
		}
	};
}

raw_method!(x_lock, xLock, 1, (level: c_int) -> c_int, ffi::SQLITE_IOERR_LOCK);
raw_method!(x_unlock, xUnlock, 1, (level: c_int) -> c_int, ffi::SQLITE_IOERR_UNLOCK);
raw_method!(x_check_reserved_lock, xCheckReservedLock, 1, (out: *mut c_int) -> c_int, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK);
raw_method!(
	x_shm_map,
	xShmMap,
	2,
	(region: c_int, size: c_int, extend: c_int, out: *mut *mut c_void) -> c_int,
	ffi::SQLITE_IOERR_SHMMAP
);
raw_method!(x_shm_lock, xShmLock, 2, (offset: c_int, count: c_int, flags: c_int) -> c_int, ffi::SQLITE_IOERR_SHMLOCK);

/// The barrier of the raw file's shared memory, which returns nothing.
unsafe extern "C" fn x_shm_barrier<V: Vfs>(file: *mut ffi::sqlite3_file) {
	// A panic must not unwind into SQLite; a barrier has nothing to report it with.
	let _ = catch_unwind(AssertUnwindSafe(|| {
		// SAFETY: SQLite calls a file that `x_open` opened; its raw file is open, and gets
		// the call as a file of the default VFS would.
		unsafe {
			if let Some(raw) = open_file::<V>(file).raw.as_mut() {
				let file = raw.file();
				if let Some(barrier) = raw
					.methods()
					.filter(|methods| methods.iVersion >= 2)
					.and_then(|methods| methods.xShmBarrier)
				{
					barrier(file);
				}
			}
		}
	}));
}
raw_method!(x_shm_unmap, xShmUnmap, 2, (delete: c_int) -> c_int, ffi::SQLITE_IOERR_SHMMAP);

unsafe extern "C" fn x_file_control(
	_file: *mut ffi::sqlite3_file,
	_operation: c_int,
	_argument: *mut c_void,
) -> c_int {
	ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn x_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
	SECTOR_SIZE
}

unsafe extern "C" fn x_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
	ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE
}

/// Where a file is, as a path the VFS is handed for it: its real path, with `..` and links
/// resolved, or that of its directory if it does not exist yet, so that one file has one
/// path. A VFS that keeps something per file, such as a key, finds it by this path.
///
/// # Errors
///
/// Returns an error if the working directory cannot be found, or the path names no file in
/// a directory.
pub fn full_path(path: &Path) -> std::io::Result<PathBuf> {
	if let Ok(real) = path.canonicalize() {
		return Ok(real);
	}
	let absolute = std::path::absolute(path)?;
	let (Some(directory), Some(name)) = (absolute.parent(), absolute.file_name()) else {
		return Err(std::io::ErrorKind::InvalidInput.into());
	};
	match directory.canonicalize() {
		Ok(directory) => Ok(directory.join(name)),
		// A file in a directory that does not exist cannot be opened anyway.
		Err(_) => Ok(absolute),
	}
}
