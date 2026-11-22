//! A backup's folder: what it holds, and how each file in it is written whole, without keys
//! and without a database ([Storage](../../../docs/spec/storage.md#6-backups)). The engine
//! reaches it through an interface of files, [`BackupFolder`]: on computers, the file system
//! stands behind it; on Android, the folder that the system's picker gives.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId};

use crate::recovery_file::{RECOVERY_FILE, RecoveryFile};
use crate::{NodeError, place};

/// The file that names a backup.
pub(crate) const BACKUP_FILE: &str = "backup.cbor";

/// The backup's record of its updates and checks.
const RECORD: &str = "record.cbor";

/// What the folder is, in plain words.
const README: &str = "README.txt";

/// The folders of packs, of blobs, of what checks found damaged, and of files being
/// written.
const SPACES: &str = "spaces";
const BLOBS: &str = "blobs";
const DAMAGED: &str = "damaged";
const TMP: &str = "tmp";

/// The file that an update locks, and the mark of an update under way.
const LOCK: &str = "lock";
const IN_USE: &str = "in-use";

/// The files written anew in place of the old: where a folder cannot replace a file in one
/// step, the old one is deleted before the new one moves in from `tmp/`, so an update that
/// finds one of them in `tmp/` and none in its place moves it there first.
const REPLACED: [&str; 2] = [RECORD, RECOVERY_FILE];

/// The extension of packs.
const PACK: &str = "pack";

/// The version of the formats of `backup.cbor` and `record.cbor`.
const VERSION: u64 = 1;

/// What `README.txt` says.
const README_TEXT: &str = "\
This folder is a backup that NibKeep keeps up to date whenever one of its owner's
devices finds it, such as when this drive is plugged in.

Everything in it is encrypted, and it holds no keys: whoever finds it reads nothing.
To restore from it, plug it into a computer of the owner's whose library holds the keys.

Do not change, rename or add files here. To stop using it, remove it in NibKeep first
(nib backup remove), then delete this folder.
";

/// A backup's folder, as the engine reads and writes it: files and folders under it, named by
/// their paths from it, with `/` between names, such as `spaces/<space>/<digest>.pack`. On
/// computers, the file system stands behind it; on Android, the folder that the system's
/// picker gives, which gives files but no paths, never replaces a file in one step, and
/// moves a file only under its own name ([technical design: backups]).
///
/// [technical design: backups]: ../../../docs/Technical-design.md#910-backups
pub trait BackupFolder: Send + Sync {
	/// A path in the folder, as messages show it.
	fn shown(&self, path: &str) -> PathBuf;

	/// The entries of a folder in it, `""` for the folder itself: each one's name, with a
	/// file's size, or `None` for a folder.
	///
	/// # Errors
	///
	/// Returns an error of the kind [`io::ErrorKind::NotFound`] if there is no such folder.
	fn list(&self, folder: &str) -> io::Result<Vec<(String, Option<u64>)>>;

	/// Opens a file to read.
	///
	/// # Errors
	///
	/// Returns an error of the kind [`io::ErrorKind::NotFound`] if there is no such file.
	fn open(&self, file: &str) -> io::Result<File>;

	/// Makes a new file to write, and the folders it is in.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be made, or one is there already.
	fn create(&self, file: &str) -> io::Result<File>;

	/// Opens a file to read and write, made if there is none: the one that an update locks.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be opened or made.
	fn open_lock(&self, file: &str) -> io::Result<File>;

	/// Moves a file into another folder, made if there is none, under the same name, in place
	/// of a file of that name there: in one step where the system can, and otherwise by
	/// deleting that one first.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be moved.
	fn put(&self, file: &str, folder: &str) -> io::Result<()>;

	/// Deletes a file, if it is there.
	///
	/// # Errors
	///
	/// Returns an error if it is there and cannot be deleted.
	fn remove(&self, file: &str) -> io::Result<()>;

	/// Whether a file is there.
	fn exists(&self, file: &str) -> bool {
		self.open(file).is_ok()
	}

	/// Saves a folder's entries to the disk, where the system allows it.
	fn sync_folder(&self, _folder: &str) {}

	/// How many bytes its drive has free.
	///
	/// # Errors
	///
	/// Returns an error if the system does not say.
	fn free(&self) -> io::Result<u64>;

	/// What its drive is, as copies count it.
	fn drive(&self) -> BackupDrive;

	/// Whether its file system keeps no journal, as [`BackupDrive::unjournaled`] says, which
	/// an update asks before it needs the rest.
	fn unjournaled(&self) -> bool {
		self.drive().unjournaled
	}
}

/// What the drive or share that holds a backup's folder is, as copies count it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupDrive {
	/// The folder's path from the root of its drive or share, with `/` between names, if the
	/// system says: where computers look for it under each mount.
	pub path: Option<String>,
	/// What names its disk, as a replica's would be, so that a backup and a replica on one
	/// drive count once; `None` where nothing names it, as on a network share, where the
	/// backup itself names its disk.
	pub disk: Option<Vec<u8>>,
	/// Whether it is on another computer, as a network share is.
	pub remote: bool,
	/// Whether its file system keeps no journal, as FAT, exFAT and ext2, so that a drive
	/// pulled out while written may damage other files.
	pub unjournaled: bool,
}

/// A backup's folder in the file system, on a computer.
pub(crate) struct InFileSystem {
	folder: PathBuf,
}

impl InFileSystem {
	pub(crate) fn new(folder: &Path) -> Self {
		Self {
			folder: folder.to_owned(),
		}
	}

	/// The path in the file system of a path in the folder.
	fn at(&self, path: &str) -> PathBuf {
		path.split('/')
			.filter(|name| !name.is_empty())
			.fold(self.folder.clone(), |at, name| at.join(name))
	}
}

impl BackupFolder for InFileSystem {
	fn shown(&self, path: &str) -> PathBuf {
		self.at(path)
	}

	fn list(&self, folder: &str) -> io::Result<Vec<(String, Option<u64>)>> {
		let mut entries = Vec::new();
		for entry in fs::read_dir(self.at(folder))?.flatten() {
			let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
				continue;
			};
			let Ok(metadata) = entry.metadata() else {
				continue;
			};
			entries.push((name, metadata.is_file().then_some(metadata.len())));
		}
		Ok(entries)
	}

	fn open(&self, file: &str) -> io::Result<File> {
		File::open(self.at(file))
	}

	fn create(&self, file: &str) -> io::Result<File> {
		let at = self.at(file);
		if let Some(parent) = at.parent() {
			fs::create_dir_all(parent)?;
		}
		File::options()
			.read(true)
			.write(true)
			.create_new(true)
			.open(at)
	}

	fn open_lock(&self, file: &str) -> io::Result<File> {
		File::options()
			.read(true)
			.write(true)
			.create(true)
			.truncate(false)
			.open(self.at(file))
	}

	fn put(&self, file: &str, folder: &str) -> io::Result<()> {
		let from = self.at(file);
		let name = from.file_name().ok_or(io::ErrorKind::InvalidInput)?;
		let into = self.at(folder);
		fs::create_dir_all(&into)?;
		// A rename replaces in one step.
		fs::rename(&from, into.join(name))
	}

	fn remove(&self, file: &str) -> io::Result<()> {
		match fs::remove_file(self.at(file)) {
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
			done => done,
		}
	}

	fn exists(&self, file: &str) -> bool {
		self.at(file).exists()
	}

	fn sync_folder(&self, folder: &str) {
		sync_folder(&self.at(folder));
	}

	fn free(&self) -> io::Result<u64> {
		fs4::available_space(&self.folder)
	}

	fn unjournaled(&self) -> bool {
		place::whereabouts(&self.folder).drive.unjournaled
	}

	fn drive(&self) -> BackupDrive {
		let whereabouts = place::whereabouts(&self.folder);
		let remote = whereabouts.drive.remote;
		BackupDrive {
			disk: (!remote).then(|| place::disk_name(&self.folder)).flatten(),
			path: whereabouts.path,
			remote,
			unjournaled: whereabouts.drive.unjournaled,
		}
	}
}

/// A folder that keeps the rules of Android's picker over a folder of the file system, for
/// tests: making a file that exists fails, a move keeps the file's name, nothing replaces a
/// file in one step, and no folder is synchronized. Its drive is as `drive` says, and it
/// shows paths as the picker does, after the drive's title and the folder's path on it.
#[doc(hidden)]
pub struct PickerRules {
	root: PathBuf,
	drive: BackupDrive,
	/// The moves asked for, each a file and the folder it went to.
	moves: Mutex<Vec<(String, String)>>,
}

impl PickerRules {
	/// The folder `root`, made if there is none, on `drive`.
	///
	/// # Panics
	///
	/// Panics if the folder cannot be made.
	#[must_use]
	pub fn new(root: &Path, drive: BackupDrive) -> Self {
		fs::create_dir_all(root).expect("makes the picked folder");
		Self {
			root: root.to_owned(),
			drive,
			moves: Mutex::new(Vec::new()),
		}
	}

	/// The moves asked for until now, each a file and the folder it went to.
	#[must_use]
	pub fn moves(&self) -> Vec<(String, String)> {
		self.moves
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone()
	}

	fn at(&self, path: &str) -> PathBuf {
		InFileSystem::new(&self.root).at(path)
	}
}

impl BackupFolder for PickerRules {
	/// With `/` between names, as on Android, whatever system the test runs on.
	fn shown(&self, path: &str) -> PathBuf {
		let names = self
			.drive
			.path
			.iter()
			.flat_map(|on| on.split('/'))
			.chain(path.split('/'))
			.filter(|name| !name.is_empty());
		PathBuf::from(
			std::iter::once("USB drive")
				.chain(names)
				.collect::<Vec<_>>()
				.join("/"),
		)
	}

	fn list(&self, folder: &str) -> io::Result<Vec<(String, Option<u64>)>> {
		InFileSystem::new(&self.root).list(folder)
	}

	fn open(&self, file: &str) -> io::Result<File> {
		File::open(self.at(file))
	}

	fn create(&self, file: &str) -> io::Result<File> {
		InFileSystem::new(&self.root).create(file)
	}

	fn open_lock(&self, file: &str) -> io::Result<File> {
		InFileSystem::new(&self.root).open_lock(file)
	}

	fn put(&self, file: &str, folder: &str) -> io::Result<()> {
		let from = self.at(file);
		let name = from.file_name().ok_or(io::ErrorKind::InvalidInput)?;
		let to = self.at(folder).join(name);
		// What is there goes first, in a step of its own.
		match fs::remove_file(&to) {
			Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
			_ => {}
		}
		fs::create_dir_all(self.at(folder))?;
		self.moves
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.push((file.to_owned(), folder.to_owned()));
		fs::rename(from, to)
	}

	fn remove(&self, file: &str) -> io::Result<()> {
		InFileSystem::new(&self.root).remove(file)
	}

	fn free(&self) -> io::Result<u64> {
		Ok(64 << 30)
	}

	fn drive(&self) -> BackupDrive {
		self.drive.clone()
	}
}

/// A path in a backup's folder, from a folder's path in it and a name.
fn joined(folder: &str, name: &str) -> String {
	if folder.is_empty() {
		name.to_owned()
	} else {
		format!("{folder}/{name}")
	}
}

/// The folder that holds a path in a backup's folder, `""` for the folder itself.
fn parent_of(path: &str) -> &str {
	path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// A backup's record of its updates and checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
	/// The number of its last update.
	pub(crate) update: u64,
	/// When each sealed blob that it holds was last read in full and found intact, in
	/// milliseconds since 1970, or 0.
	pub(crate) checks: BTreeMap<Hash, u64>,
	/// When checks found each damaged copy, in the 4 weeks before the update.
	pub(crate) damaged: BTreeMap<Hash, u64>,
	/// How long the backup waits before purging, from each time on, in milliseconds, as its
	/// updates saw its owner's mark ([technical design: copies that wait]).
	///
	/// [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait
	pub(crate) waits: Vec<(u64, u64)>,
	/// The purges whose destruction the backup keeps apart, each with when the update that
	/// first brought it ran, and how many items and files it keeps of it.
	pub(crate) purges: BTreeMap<ChangeId, (u64, u64)>,
}

impl Record {
	fn to_bytes(&self) -> Vec<u8> {
		let pairs = |map: &BTreeMap<Hash, u64>| {
			Value::Array(
				map.iter()
					.map(|(sealed, at)| Value::Array(vec![sealed.to_cbor(), at.to_cbor()]))
					.collect(),
			)
		};
		let waits = Value::Array(
			self.waits
				.iter()
				.map(|(since, wait)| Value::Array(vec![since.to_cbor(), wait.to_cbor()]))
				.collect(),
		);
		let purges = Value::Array(
			self.purges
				.iter()
				.map(|(purge, (seen, items))| {
					Value::Array(vec![purge.to_cbor(), seen.to_cbor(), items.to_cbor()])
				})
				.collect(),
		);
		cbor::encode(&Value::Array(vec![
			VERSION.to_cbor(),
			self.update.to_cbor(),
			pairs(&self.checks),
			pairs(&self.damaged),
			waits,
			purges,
		]))
	}

	fn from_bytes(bytes: &[u8]) -> Option<Self> {
		let Value::Array(items) = cbor::decode(bytes).ok()? else {
			return None;
		};
		let [version, update, checks, damaged, waits, purges] = items.as_slice() else {
			return None;
		};
		if u64::from_cbor(version).ok()? != VERSION {
			return None;
		}
		let pairs = |value: &Value| -> Option<BTreeMap<Hash, u64>> {
			let Value::Array(pairs) = value else {
				return None;
			};
			let mut map = BTreeMap::new();
			for pair in pairs {
				let Value::Array(pair) = pair else {
					return None;
				};
				let [sealed, at] = pair.as_slice() else {
					return None;
				};
				map.insert(Hash::from_cbor(sealed).ok()?, u64::from_cbor(at).ok()?);
			}
			Some(map)
		};
		let Value::Array(waits) = waits else {
			return None;
		};
		let mut read = Vec::new();
		for pair in waits {
			let Value::Array(pair) = pair else {
				return None;
			};
			let [since, wait] = pair.as_slice() else {
				return None;
			};
			read.push((u64::from_cbor(since).ok()?, u64::from_cbor(wait).ok()?));
		}
		let Value::Array(purges) = purges else {
			return None;
		};
		let mut seen = BTreeMap::new();
		for pair in purges {
			let Value::Array(pair) = pair else {
				return None;
			};
			let [purge, at, items] = pair.as_slice() else {
				return None;
			};
			seen.insert(
				ChangeId::from_cbor(purge).ok()?,
				(u64::from_cbor(at).ok()?, u64::from_cbor(items).ok()?),
			);
		}
		Some(Self {
			update: u64::from_cbor(update).ok()?,
			checks: pairs(checks)?,
			damaged: pairs(damaged)?,
			waits: read,
			purges: seen,
		})
	}
}

/// A file's bytes in a backup's folder.
fn read_all(files: &dyn BackupFolder, file: &str) -> io::Result<Vec<u8>> {
	let mut bytes = Vec::new();
	files.open(file)?.read_to_end(&mut bytes)?;
	Ok(bytes)
}

/// The backup that a folder names, if it names one, by its record's ID.
#[must_use]
pub fn named_at(folder: &Path) -> Option<EntityId> {
	named_in(&InFileSystem::new(folder))
}

/// The backup that a folder reached through the interface of files names, if it names one,
/// by its record's ID.
#[must_use]
pub fn named_in(files: &dyn BackupFolder) -> Option<EntityId> {
	let bytes = read_all(files, BACKUP_FILE).ok()?;
	let Value::Array(items) = cbor::decode(&bytes).ok()? else {
		return None;
	};
	let [version, id] = items.as_slice() else {
		return None;
	};
	if u64::from_cbor(version).ok()? != VERSION {
		return None;
	}
	let Value::Bytes(id) = id else {
		return None;
	};
	Some(EntityId::from_bytes(
		<[u8; 16]>::try_from(id.as_slice()).ok()?,
	))
}

/// Makes a backup's folder, which must be new or empty: the file that names the backup, and
/// what the folder is.
pub(crate) fn make(files: &dyn BackupFolder, backup: EntityId) -> Result<(), NodeError> {
	match files.list("") {
		Ok(entries) => {
			if !entries.is_empty() {
				return Err(NodeError::BackupFolder(format!(
					"{} is not empty, and a backup is made only in an empty folder or a new one",
					files.shown("").display()
				)));
			}
		}
		Err(error) if error.kind() == io::ErrorKind::NotFound => {}
		Err(error) => return Err(NodeError::file(&files.shown(""))(error)),
	}
	let named = cbor::encode(&Value::Array(vec![
		VERSION.to_cbor(),
		Value::Bytes(backup.as_bytes().to_vec()),
	]));
	for (name, bytes) in [
		(README, README_TEXT.as_bytes()),
		(BACKUP_FILE, named.as_slice()),
	] {
		(|| {
			let mut file = files.create(name)?;
			file.write_all(bytes)?;
			file.sync_all()
		})()
		.map_err(NodeError::file(&files.shown(name)))?;
	}
	files.sync_folder("");
	Ok(())
}

/// Takes away what [`make`] wrote, after the backup could not be recorded.
pub(crate) fn unmake(files: &dyn BackupFolder) {
	for name in [README, BACKUP_FILE] {
		let _ = files.remove(name);
	}
}

/// A backup's record, read from its folder, or an empty one where it has none that reads.
pub(crate) fn read_record(files: &dyn BackupFolder) -> Record {
	read_all(files, RECORD)
		.ok()
		.and_then(|bytes| Record::from_bytes(&bytes))
		.unwrap_or_default()
}

/// The sealed blobs that the backup in `files` holds in files of their own, with their
/// sizes.
///
/// # Errors
///
/// Returns an error if the folder of blobs cannot be read.
pub(crate) fn blob_files_in(files: &dyn BackupFolder) -> Result<BTreeMap<Hash, u64>, NodeError> {
	let mut blobs = BTreeMap::new();
	let algorithms = match files.list(BLOBS) {
		Ok(entries) => entries,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(blobs),
		Err(error) => return Err(NodeError::file(&files.shown(BLOBS))(error)),
	};
	for (algorithm, size) in algorithms {
		if size.is_some() {
			continue;
		}
		let of_algorithm = joined(BLOBS, &algorithm);
		let Ok(prefixes) = files.list(&of_algorithm) else {
			continue;
		};
		for (prefix, size) in prefixes {
			if size.is_some() {
				continue;
			}
			let of_prefix = joined(&of_algorithm, &prefix);
			let Ok(entries) = files.list(&of_prefix) else {
				continue;
			};
			for (digest, size) in entries {
				if let Some(size) = size
					&& let Ok(sealed) = format!("{algorithm}:{digest}").parse::<Hash>()
					&& blob_path(&sealed) == joined(&of_prefix, &digest)
				{
					blobs.insert(sealed, size);
				}
			}
		}
	}
	Ok(blobs)
}

/// Whether the backup in `files` holds the mark of an update: one under way, or one that a
/// drive pulled out or a lost power cut short, after which its copies count again only once
/// read back.
pub(crate) fn marked_in_use(files: &dyn BackupFolder) -> bool {
	files.exists(IN_USE)
}

/// Where a backup keeps a sealed blob in a file of its own, as a replica does.
pub(crate) fn blob_path(sealed: &Hash) -> String {
	nibkeep_store::place_in(Path::new(BLOBS), sealed)
		.components()
		.map(|name| name.as_os_str().to_string_lossy())
		.collect::<Vec<_>>()
		.join("/")
}

/// What names the disk of a backup in `files`, as copies count disks: its drive's name, as
/// a replica's would be, so that a backup and a replica on one drive count once; on a
/// network share, whose drive no system names, or where nothing names it, the backup
/// itself ([technical design](../../../docs/Technical-design.md#910-backups)).
pub(crate) fn disk_name(files: &dyn BackupFolder, backup: EntityId) -> Vec<u8> {
	files
		.drive()
		.disk
		.unwrap_or_else(|| [b"backup\0".as_slice(), backup.as_bytes()].concat())
}

/// The folders that drives and shares are mounted at, as the system lists them: on Linux,
/// each mount point of the mount table; on macOS, the system's drive and each volume in
/// `/Volumes`; on Windows, each drive's letter.
pub(crate) fn mounts() -> Vec<PathBuf> {
	let mut mounts = system_mounts();
	mounts.sort();
	mounts.dedup();
	mounts
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn system_mounts() -> Vec<PathBuf> {
	let Ok(table) = fs::read_to_string("/proc/self/mountinfo") else {
		return Vec::new();
	};
	table.lines().filter_map(mount_of_line).collect()
}

/// The mount point of a line of the mount table, unless its file system holds no files of
/// a person's, as the kernel's own do, or is mounted on access, as autofs is, which a look
/// would set off.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn mount_of_line(line: &str) -> Option<PathBuf> {
	const NOT_FOR_FILES: [&str; 22] = [
		"autofs",
		"binfmt_misc",
		"bpf",
		"cgroup",
		"cgroup2",
		"configfs",
		"debugfs",
		"devpts",
		"devtmpfs",
		"efivarfs",
		"fusectl",
		"hugetlbfs",
		"mqueue",
		"nsfs",
		"proc",
		"pstore",
		"ramfs",
		"rpc_pipefs",
		"securityfs",
		"squashfs",
		"sysfs",
		"tracefs",
	];
	let mount = line.split(' ').nth(4)?;
	let kind = line.split(" - ").nth(1)?.split(' ').next()?;
	(!NOT_FOR_FILES.contains(&kind)).then(|| PathBuf::from(unescape_mount(mount)))
}

/// A mount point as the mount table writes it, with a space, a tab, a line feed and a
/// backslash as octal escapes.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn unescape_mount(written: &str) -> String {
	let mut text = String::with_capacity(written.len());
	let mut rest = written;
	while let Some(at) = rest.find('\\') {
		text.push_str(rest.get(..at).unwrap_or_default());
		let code = rest
			.get(at + 1..at + 4)
			.and_then(|digits| u8::from_str_radix(digits, 8).ok());
		if let Some(byte) = code {
			text.push(char::from(byte));
			rest = rest.get(at + 4..).unwrap_or_default();
		} else {
			text.push('\\');
			rest = rest.get(at + 1..).unwrap_or_default();
		}
	}
	text.push_str(rest);
	text
}

#[cfg(target_os = "macos")]
fn system_mounts() -> Vec<PathBuf> {
	let mut mounts = vec![PathBuf::from("/")];
	if let Ok(volumes) = fs::read_dir("/Volumes") {
		mounts.extend(volumes.flatten().map(|volume| volume.path()));
	}
	mounts
}

#[cfg(windows)]
fn system_mounts() -> Vec<PathBuf> {
	(b'A'..=b'Z')
		.map(|letter| PathBuf::from(format!("{}:\\", char::from(letter))))
		.filter(|root| fs::metadata(root).is_ok())
		.collect()
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	target_os = "macos",
	windows
)))]
fn system_mounts() -> Vec<PathBuf> {
	Vec::new()
}

/// The folders of mounts where a look did not end in time, as a network share whose server
/// went away: left out of looks for ten minutes, so that looks do not pile up behind them.
static SLOW: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());

/// How long a mount where a look did not end in time is left out of looks.
const SLOW_FOR: Duration = Duration::from_mins(10);

/// Of `candidates`, each a mount's folder (or the folder itself), a folder there and the
/// backup it may hold, those whose folder names that backup, in their order; looked at on a
/// thread of their own, for at most `within`, so that a network share that does not answer
/// never holds up the caller: one found slow is left out of looks for a while.
pub(crate) fn find_named(
	candidates: Vec<(PathBuf, PathBuf, EntityId)>,
	within: Duration,
) -> Vec<(EntityId, PathBuf)> {
	let now = Instant::now();
	let candidates: Vec<(PathBuf, PathBuf, EntityId)> = {
		let mut slow = SLOW
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		slow.retain(|_, since| now.duration_since(*since) < SLOW_FOR);
		candidates
			.into_iter()
			.filter(|(root, _, _)| !slow.contains_key(root))
			.collect()
	};
	let (sender, results) = std::sync::mpsc::channel();
	let looked = candidates.clone();
	std::thread::spawn(move || {
		for (at, (_, folder, backup)) in looked.into_iter().enumerate() {
			let named = named_at(&folder) == Some(backup);
			if sender.send((at, named)).is_err() {
				return;
			}
		}
	});
	let deadline = now + within;
	let mut found = Vec::new();
	let mut answered = 0;
	while answered < candidates.len() {
		let Some(left) = deadline.checked_duration_since(Instant::now()) else {
			break;
		};
		match results.recv_timeout(left) {
			Ok((at, named)) => {
				answered = at + 1;
				if named && let Some((_, folder, backup)) = candidates.get(at) {
					found.push((*backup, folder.clone()));
				}
			}
			Err(_) => break,
		}
	}
	if let Some((root, _, _)) = candidates.get(answered) {
		SLOW.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.insert(root.clone(), Instant::now());
	}
	found
}

/// A backup's folder, held by an update: locked, and marked as being updated, until it
/// goes.
pub(crate) struct Updating<'a> {
	files: &'a dyn BackupFolder,
	_lock: File,
	/// Whether the folder held the mark of an update while nobody held the lock: a drive
	/// pulled out or that lost its power during an update.
	pub(crate) unclean: bool,
	/// Whether its file system keeps no journal, as FAT, exFAT and ext2.
	pub(crate) unjournaled: bool,
}

impl<'a> Updating<'a> {
	/// Holds the backup in `files` for an update: `None` while another program holds it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotTheBackup`] if the folder names another backup or none, and an
	/// error if its files cannot be written.
	pub(crate) fn hold(
		files: &'a dyn BackupFolder,
		backup: EntityId,
		name: &str,
	) -> Result<Option<Self>, NodeError> {
		if named_in(files) != Some(backup) {
			return Err(NodeError::NotTheBackup {
				path: files.shown(""),
				name: name.to_owned(),
			});
		}
		let lock = files
			.open_lock(LOCK)
			.map_err(NodeError::file(&files.shown(LOCK)))?;
		match lock.try_lock() {
			Ok(()) => {}
			Err(fs::TryLockError::WouldBlock) => return Ok(None),
			Err(fs::TryLockError::Error(error)) => {
				return Err(NodeError::file(&files.shown(LOCK))(error));
			}
		}
		let unclean = files.exists(IN_USE);
		if !unclean {
			(|| {
				let mark = files.create(IN_USE)?;
				mark.sync_all()
			})()
			.map_err(NodeError::file(&files.shown(IN_USE)))?;
			files.sync_folder("");
		}
		// A file written anew whose old one was deleted, before it moved into place: told from
		// the folders' listings, never from a look that failed.
		if let (Ok(here), Ok(staged)) = (files.list(""), files.list(TMP)) {
			let holds = |entries: &[(String, Option<u64>)], name: &str| {
				entries
					.iter()
					.any(|(entry, size)| entry == name && size.is_some())
			};
			for name in REPLACED {
				if !holds(&here, name) && holds(&staged, name) {
					files
						.put(&joined(TMP, name), "")
						.map_err(NodeError::file(&files.shown(name)))?;
				}
			}
		}
		// What a pulled drive left half written.
		if let Ok(entries) = files.list(TMP) {
			for (name, _) in entries {
				let _ = files.remove(&joined(TMP, &name));
			}
		}
		Ok(Some(Self {
			files,
			_lock: lock,
			unclean,
			unjournaled: files.unjournaled(),
		}))
	}

	/// A path in the folder, as messages show it.
	pub(crate) fn shown(&self, path: &str) -> PathBuf {
		self.files.shown(path)
	}

	/// The backup's record, or an empty one where it has none that reads.
	pub(crate) fn record(&self) -> Record {
		read_record(self.files)
	}

	/// The backup's recovery file: `None` where there is none, or one that does not read, as
	/// on a damaged drive.
	pub(crate) fn recovery(&self) -> Option<RecoveryFile> {
		RecoveryFile::from_bytes(&read_all(self.files, RECOVERY_FILE).ok()?)
	}

	/// Writes the backup's recovery file anew, whole, from its bytes.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be written whole, or does not read back.
	pub(crate) fn save_recovery(&self, bytes: &[u8]) -> Result<(), NodeError> {
		self.write_whole(RECOVERY_FILE, &mut &*bytes, None)
			.map(|_| ())
	}

	/// Deletes the backup's recovery file.
	///
	/// # Errors
	///
	/// Returns an error if it is there and cannot be deleted.
	pub(crate) fn delete_recovery(&self) -> Result<(), NodeError> {
		self.delete(RECOVERY_FILE)
	}

	/// Writes the backup's record anew.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be written whole.
	pub(crate) fn save_record(&self, record: &Record) -> Result<(), NodeError> {
		let bytes = record.to_bytes();
		self.write_whole(RECORD, &mut bytes.as_slice(), None)
			.map(|_| ())
	}

	/// The packs of each space, by their paths, with their sizes.
	///
	/// # Errors
	///
	/// Returns an error if the folders cannot be read.
	pub(crate) fn packs(&self) -> Result<Vec<(SpaceId, String, u64)>, NodeError> {
		let mut packs = Vec::new();
		let spaces = match self.files.list(SPACES) {
			Ok(entries) => entries,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(packs),
			Err(error) => return Err(NodeError::file(&self.shown(SPACES))(error)),
		};
		for (name, size) in spaces {
			let Some(space) = size
				.is_none()
				.then(|| name.parse::<SpaceId>().ok())
				.flatten()
			else {
				continue;
			};
			let folder = joined(SPACES, &name);
			let Ok(files) = self.files.list(&folder) else {
				continue;
			};
			for (file, size) in files {
				if let Some(size) = size
					&& Path::new(&file)
						.extension()
						.and_then(|extension| extension.to_str())
						== Some(PACK)
				{
					packs.push((space, joined(&folder, &file), size));
				}
			}
		}
		packs.sort();
		Ok(packs)
	}

	/// The sealed blobs that the backup holds in files of their own, with their sizes.
	///
	/// # Errors
	///
	/// Returns an error if the folder of blobs cannot be read.
	pub(crate) fn blob_files(&self) -> Result<BTreeMap<Hash, u64>, NodeError> {
		blob_files_in(self.files)
	}

	/// Where the backup keeps a pack of `space` whose BLAKE3 hash is `hash`.
	pub(crate) fn pack_path(space: SpaceId, hash: &Hash) -> String {
		format!("{SPACES}/{space}/{}.{PACK}", hex(hash.digest()))
	}

	/// A file's bytes.
	pub(crate) fn read(&self, file: &str) -> io::Result<Vec<u8>> {
		read_all(self.files, file)
	}

	/// Opens a file to read.
	pub(crate) fn open(&self, file: &str) -> io::Result<File> {
		self.files.open(file)
	}

	/// The BLAKE3 hash of a file's bytes.
	pub(crate) fn hash(&self, file: &str) -> io::Result<Hash> {
		hash_of(&mut self.files.open(file)?)
	}

	/// Writes a pack of `space` from its bytes, whole, and returns its path.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be written whole, or does not read back.
	pub(crate) fn write_pack(&self, space: SpaceId, bytes: &[u8]) -> Result<String, NodeError> {
		let hash = Hash::of(bytes);
		let path = Self::pack_path(space, &hash);
		if !self.files.exists(&path) {
			self.write_whole(&path, &mut &*bytes, Some(&hash))?;
		}
		Ok(path)
	}

	/// Writes a sealed blob in a file of its own, whole, from `bytes`, and returns whether it
	/// did: not if `bytes` do not have its sealed hash, as a damaged copy's.
	///
	/// # Errors
	///
	/// Returns an error if it cannot be written whole, or does not read back as written.
	pub(crate) fn write_blob(
		&self,
		sealed: &Hash,
		bytes: &mut dyn Read,
	) -> Result<bool, NodeError> {
		self.write_whole(&blob_path(sealed), bytes, Some(sealed))
	}

	/// Moves a pack or a blob's file aside, to `damaged/`, as a check found it damaged.
	pub(crate) fn set_aside(&self, path: &str) {
		if self.files.put(path, DAMAGED).is_err() {
			let _ = self.files.remove(path);
		}
		self.files.sync_folder(DAMAGED);
	}

	/// Deletes a pack or a blob's file.
	pub(crate) fn delete(&self, path: &str) -> Result<(), NodeError> {
		self.files
			.remove(path)
			.map_err(NodeError::file(&self.shown(path)))?;
		self.files.sync_folder(parent_of(path));
		Ok(())
	}

	/// How many bytes its drive has free.
	pub(crate) fn free(&self) -> Result<u64, NodeError> {
		self.files.free().map_err(NodeError::file(&self.shown("")))
	}

	/// Writes a file whole: into `tmp/` under its own name, synchronized, moved into place, and
	/// read back as written. Returns whether it wrote it: not if `hash` is given and the bytes
	/// do not have it, which leaves `path` as it was. One that does not read back is deleted.
	fn write_whole(
		&self,
		path: &str,
		bytes: &mut dyn Read,
		hash: Option<&Hash>,
	) -> Result<bool, NodeError> {
		let name = path.rsplit('/').next().unwrap_or(path);
		let staged = joined(TMP, name);
		let failed = |error| NodeError::file(&self.shown(&staged))(error);
		// One left by a write that failed.
		self.files.remove(&staged).map_err(failed)?;
		let written = (|| -> io::Result<Hash> {
			let mut file = self.files.create(&staged)?;
			let mut hasher = nibkeep_base::Hasher::new();
			let mut buffer = vec![0; 1 << 20];
			loop {
				let read = bytes.read(&mut buffer)?;
				if read == 0 {
					break;
				}
				let chunk = buffer.get(..read).unwrap_or_default();
				hasher.update(chunk);
				file.write_all(chunk)?;
			}
			file.sync_all()?;
			Ok(hasher.finalize())
		})();
		let written = match written {
			Ok(written) => written,
			Err(error) => {
				let _ = self.files.remove(&staged);
				return Err(failed(error));
			}
		};
		if hash.is_some_and(|hash| *hash != written) {
			let _ = self.files.remove(&staged);
			return Ok(false);
		}
		let parent = parent_of(path);
		if let Err(error) = self.files.put(&staged, parent) {
			// One that replaces a file stays, so that the next update moves it in.
			if !REPLACED.contains(&path) {
				let _ = self.files.remove(&staged);
			}
			return Err(NodeError::file(&self.shown(path))(error));
		}
		self.files.sync_folder(parent);
		// Read back, as the drive keeps it, and synchronized where it is now: where a folder
		// cannot be synchronized, as through Android's picker, FAT and exFAT write a file's
		// entry in its folder as they synchronize the file.
		// A system that synchronizes no file opened to read, as Windows, has done so already.
		let read_back = self.files.open(path).and_then(|mut file| {
			let hash = hash_of(&mut file)?;
			let _ = file.sync_all();
			Ok(hash)
		});
		if read_back.ok() != Some(written) {
			let _ = self.files.remove(path);
			return Err(NodeError::BackupWrite(self.shown(path)));
		}
		Ok(true)
	}
}

impl Drop for Updating<'_> {
	/// Takes the mark away, as the update ends, before the lock goes.
	fn drop(&mut self) {
		if self.files.remove(IN_USE).is_ok() {
			self.files.sync_folder("");
		}
	}
}

/// The BLAKE3 hash of what a file holds, read from where it is to its end.
fn hash_of(file: &mut File) -> io::Result<Hash> {
	let mut hasher = nibkeep_base::Hasher::new();
	let mut buffer = vec![0; 1 << 20];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			return Ok(hasher.finalize());
		}
		hasher.update(buffer.get(..read).unwrap_or_default());
	}
}

/// Saves a folder's entries to the disk, where the system allows it.
fn sync_folder(folder: &Path) {
	#[cfg(unix)]
	let _ = File::open(folder).and_then(|folder| folder.sync_all());
	#[cfg(not(unix))]
	let _ = folder;
}

/// Bytes in lowercase hexadecimal.
fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write as _;
	bytes.iter().fold(String::new(), |mut text, byte| {
		let _ = write!(text, "{byte:02x}");
		text
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_mount_table_gives_the_mounts_that_hold_files() {
		assert_eq!(
			mount_of_line("36 25 8:2 / /run/media/ada/STICK rw,nosuid - exfat /dev/sdb1 rw"),
			Some(PathBuf::from("/run/media/ada/STICK"))
		);
		assert_eq!(
			mount_of_line("22 1 0:21 / /proc rw,nosuid shared:5 - proc proc rw"),
			None
		);
		assert_eq!(
			mount_of_line("40 1 0:40 / /mnt/nas rw - autofs systemd-1 rw"),
			None
		);
	}

	#[test]
	fn a_mount_point_reads_with_its_escapes() {
		assert_eq!(
			unescape_mount("/run/media/ada/My\\040Stick"),
			"/run/media/ada/My Stick"
		);
		assert_eq!(unescape_mount("/plain"), "/plain");
	}

	#[test]
	fn a_record_reads_back_and_a_damaged_one_reads_as_none() {
		let mut record = Record {
			update: 4,
			checks: [(Hash::of(b"a"), 7), (Hash::of(b"b"), 0)].into(),
			damaged: [(Hash::of(b"c"), 9)].into(),
			waits: Vec::new(),
			purges: BTreeMap::new(),
		};
		// One without waits, as backups' records were before they waited, reads as none.
		let older = cbor::encode(&Value::Array(vec![
			VERSION.to_cbor(),
			4_u64.to_cbor(),
			Value::Array(vec![
				Value::Array(vec![Hash::of(b"a").to_cbor(), 7_u64.to_cbor()]),
				Value::Array(vec![Hash::of(b"b").to_cbor(), 0_u64.to_cbor()]),
			]),
			Value::Array(vec![Value::Array(vec![
				Hash::of(b"c").to_cbor(),
				9_u64.to_cbor(),
			])]),
		]));
		assert_eq!(Record::from_bytes(&older), None);
		assert_eq!(Record::from_bytes(&record.to_bytes()), Some(record.clone()));
		record.waits = vec![(5, 30 * 86_400_000)];
		record.purges = [(ChangeId::from_hash(Hash::of(b"a purge")), (6, 3))].into();
		assert_eq!(Record::from_bytes(&record.to_bytes()), Some(record));
		assert_eq!(Record::from_bytes(b"\x80"), None);
	}
}
