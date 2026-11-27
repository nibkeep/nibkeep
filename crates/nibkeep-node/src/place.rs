//! Where a replica is: the drive that holds its folder, the folder itself, and on the drive
//! that the system runs from, the computer, which tell a replica from a copy of it, as
//! the technical design's section on copies of a replica decides.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_crypto::rand_core::CryptoRng;
use unicode_normalization::UnicodeNormalization;

use crate::NodeError;

/// The suite of the place file in version 1 of the specification.
const SUITE: u8 = 1;

/// What the hash of a drive's serial is keyed for, besides the salt.
const SERIAL: &str = "nibkeep 2026-10 drive serial";

/// What the hash of a folder's ID is keyed for, besides the salt.
const FOLDER: &str = "nibkeep 2026-10 folder";

/// What the hash of a computer's ID is keyed for, besides the salt.
const COMPUTER: &str = "nibkeep 2026-10 computer";

/// What the hash of a FAT or exFAT volume's UUID is keyed for, besides the salt.
const VOLUME: &str = "nibkeep 2026-10 volume";

/// The name of a replica's place file.
pub(crate) const PLACE: &str = "place.cbor";

/// A drive, as the system reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "four facts that the system reports apart"
)]
pub struct Drive {
	/// The serial of its file system, if the system reads one.
	pub serial: Option<Vec<u8>>,
	/// On FAT and exFAT, the volume's UUID as macOS makes it from the serial, which macOS
	/// reads in place of the serial, and other systems make alike where they can.
	pub volume: Option<Vec<u8>>,
	/// Whether its file system keeps IDs of folders that a move keeps, as all but FAT and
	/// exFAT do.
	pub folder_ids: bool,
	/// Whether the system runs from it.
	pub system: bool,
	/// Whether its file system is on another computer, as a network drive's is.
	pub remote: bool,
	/// Whether a program may take its files' content away to the cloud, as the folders of
	/// Windows' cloud files and macOS's File Provider are.
	pub cloud: bool,
	/// Whether its file system keeps no journal, as FAT, exFAT and ext2: a removal without
	/// unmounting can leave it inconsistent until it is checked (technical design: drives without a journal).
	pub unjournaled: bool,
	/// The physical drive's own name, as its partition table gives it, if declared: the
	/// system's is read only when asked, as copies are counted.
	pub disk: Option<Vec<u8>>,
}

/// Where a folder is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whereabouts {
	/// The drive that holds it.
	pub drive: Drive,
	/// The folder that the drive is mounted at.
	pub mount: PathBuf,
	/// The folder's ID on its file system, if the file system keeps IDs that a move keeps.
	pub folder: Option<Vec<u8>>,
	/// The folder's path on the drive, from [`Whereabouts::mount`], or `None` in an app's own
	/// folder on a phone.
	pub path: Option<String>,
	/// The computer's ID, if the system gives one: on a phone, the phone's.
	pub computer: Option<Vec<u8>>,
	/// Whether this is an app's own folder on a phone, where the phone's ID has to be there
	/// and match.
	pub phone: bool,
}

/// Folders whose whereabouts are declared rather than read.
static DECLARED: Mutex<Vec<Declared>> = Mutex::new(Vec::new());

#[derive(Clone, Debug)]
enum Declared {
	/// A drive mounted at a folder, on a computer.
	Drive {
		mount: PathBuf,
		drive: Drive,
		computer: Option<Vec<u8>>,
	},
	/// An app's own folder on a phone, the phone's ID, and the name that the apps of one
	/// developer share for the phone, if the app gave one.
	Phone {
		folder: PathBuf,
		phone: Vec<u8>,
		disk: Option<Vec<u8>>,
	},
}

impl Declared {
	fn folder(&self) -> &Path {
		match self {
			Self::Drive { mount, .. } => mount,
			Self::Phone { folder, .. } => folder,
		}
	}
}

/// Declares that the folders in `mount` are on `drive`, mounted there, on the computer whose
/// ID is `computer`, whatever the system reports: for tests, which pretend that folders are
/// on other drives and computers. The IDs of folders are still read from the file system
/// that holds them, if `drive` keeps them. A later declaration of the same folder replaces
/// it.
#[doc(hidden)]
pub fn declare_drive(mount: &Path, drive: Drive, computer: Option<Vec<u8>>) {
	declare(Declared::Drive {
		mount: canonical(mount),
		drive,
		computer,
	});
}

/// Declares that `folder` is an app's own folder on a phone, which no other app reaches,
/// and `phone` the phone's ID, which the app keeps where the system never copies it to
/// another phone: the replicas in it are told from copies by the phone's ID alone, as the
/// folder's path may change when the app is updated.
pub fn declare_phone(folder: &Path, phone: Vec<u8>) {
	declare(Declared::Phone {
		folder: canonical(folder),
		phone,
		disk: None,
	});
}

/// Declares an app's own folder on a phone as [`declare_phone`] does, with `disk`, a name of
/// the phone that the apps of one developer share, such as Android's ID for them: replicas
/// of such apps on one phone are on one disk, and count as one copy.
pub fn declare_phone_with_disk(folder: &Path, phone: Vec<u8>, disk: Vec<u8>) {
	declare(Declared::Phone {
		folder: canonical(folder),
		phone,
		disk: Some(disk).filter(|disk| !disk.is_empty()),
	});
}

fn declare(declared: Declared) {
	let mut all = DECLARED
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	all.retain(|other| other.folder() != declared.folder());
	all.push(declared);
}

/// The folders whose drives tests declared nearly full, each with the bytes it has free.
static FREE: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// Declares that the drive of the folders in `folder` has `free` bytes free, whatever the
/// system reports, or with `None`, what the system reports again: for tests, which pretend
/// that a disk is nearly full.
#[doc(hidden)]
pub fn declare_free_space(folder: &Path, free: Option<u64>) {
	let folder = canonical(folder);
	let mut all = FREE
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner);
	all.retain(|(other, _)| *other != folder);
	all.extend(free.map(|free| (folder, free)));
}

/// How many bytes the drive of `folder` has free for this user, as the system reports it, or
/// as a test declared it.
pub(crate) fn available_space(folder: &Path) -> io::Result<u64> {
	let declared = {
		let all = FREE
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner);
		if all.is_empty() {
			None
		} else {
			let folder = canonical(folder);
			all.iter()
				.filter(|(declared, _)| folder.starts_with(declared))
				.max_by_key(|(declared, _)| declared.components().count())
				.map(|(_, free)| *free)
		}
	};
	declared.map_or_else(|| fs4::available_space(folder), Ok)
}

/// A folder as the file system resolves it: the deepest folder of it that exists, resolved,
/// and the rest of its names after it, so that a folder declared before it exists, through a
/// link such as Android's `/data/user/0`, is the same folder once it does.
fn canonical(folder: &Path) -> PathBuf {
	let mut rest = Vec::new();
	let mut existing = folder;
	loop {
		if let Ok(resolved) = fs::canonicalize(existing) {
			return rest
				.iter()
				.rev()
				.fold(resolved, |path, name| path.join(name));
		}
		match (existing.parent(), existing.file_name()) {
			(Some(parent), Some(name)) => {
				rest.push(name.to_owned());
				existing = parent;
			}
			_ => return folder.to_owned(),
		}
	}
}

/// The computer's ID, as the place file records it on the system's drive: `None` where
/// the system gives none.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn computer_id() -> Option<Vec<u8>> {
	system::computer()
}

/// Where a folder is now: as declared, if a folder that holds it was, or as the system
/// reports.
pub(crate) fn whereabouts(folder: &Path) -> Whereabouts {
	let folder = canonical(folder);
	let declared = DECLARED
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.iter()
		.filter(|declared| folder.starts_with(declared.folder()))
		.max_by_key(|declared| declared.folder().components().count())
		.cloned();
	match declared {
		Some(Declared::Phone { folder, phone, .. }) => Whereabouts {
			drive: Drive {
				system: true,
				..Drive::default()
			},
			mount: folder,
			folder: None,
			path: None,
			computer: Some(phone).filter(|phone| !phone.is_empty()),
			phone: true,
		},
		Some(Declared::Drive {
			mount,
			drive,
			computer,
		}) => Whereabouts {
			folder: drive.folder_ids.then(|| folder_id(&folder)).flatten(),
			path: Some(path_on(&folder, &mount)),
			drive,
			mount,
			computer,
			phone: false,
		},
		None => system::whereabouts(&folder),
	}
}

/// What names the disk that holds a folder, as copies count disks: the physical drive's own
/// name, from its partition table, which every system reads alike, so that partitions of one
/// drive are one disk; else the file system's serial; else, on the drive the system runs
/// from, the computer's ID, as on a phone; or `None` where nothing names it, and the folder
/// counts as on a disk of its own ([technical design: where the copies are]). Each kind of
/// name is told apart by a prefix.
///
/// Public for the tests of apps, which declare phones.
///
/// [technical design: where the copies are]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#96-where-the-copies-are
#[doc(hidden)]
#[must_use]
pub fn disk_name(folder: &Path) -> Option<Vec<u8>> {
	let folder = canonical(folder);
	let declared = DECLARED
		.lock()
		.unwrap_or_else(std::sync::PoisonError::into_inner)
		.iter()
		.filter(|declared| folder.starts_with(declared.folder()))
		.max_by_key(|declared| declared.folder().components().count())
		.cloned();
	let named = |kind: &[u8], name: &[u8]| [kind, b"\0", name].concat();
	let now = whereabouts(&folder);
	let drive = match declared {
		Some(Declared::Phone { disk, phone, .. }) => {
			return disk
				.map(|disk| named(b"phone apps", &disk))
				.or_else(|| (!phone.is_empty()).then(|| named(b"phone", &phone)));
		}
		Some(Declared::Drive { drive, .. }) => drive.disk.clone(),
		None => system_disk_name(&folder, &now),
	};
	drive
		.map(|name| named(b"drive", &name))
		.or_else(|| {
			now.drive
				.serial
				.as_ref()
				.map(|serial| named(b"file system", serial))
		})
		.or_else(|| {
			now.drive
				.volume
				.as_ref()
				.map(|volume| named(b"volume", volume))
		})
		.or_else(|| {
			now.drive
				.system
				.then_some(now.computer.as_ref())
				.flatten()
				.map(|computer| named(b"computer", computer))
		})
}

/// What names the disk of a drive known only by its file system's serial, as Android's
/// picker gives it, such as `9CB1-1616` for FAT and exFAT: named as a computer names a
/// drive whose serial alone it reads.
#[must_use]
pub fn serial_disk_name(serial: &str) -> Option<Vec<u8>> {
	parse_serial(serial).map(|serial| [b"file system".as_slice(), b"\0", &serial].concat())
}

/// A partition table's ID, in one form whatever system read it: a GPT's GUID, as 16 bytes,
/// or an MBR's signature, as 4 bytes, from its text, the GUID with or without braces, or the
/// signature in hexadecimal; `None` for one of zeros, which names no drive. Linux's udev
/// writes them so.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn partition_table(id: &str) -> Option<Vec<u8>> {
	let digits: String = id
		.chars()
		.filter(|character| !matches!(character, '{' | '}' | '-'))
		.collect();
	if !digits.len().is_multiple_of(2) {
		return None;
	}
	let bytes: Vec<u8> = (0..digits.len())
		.step_by(2)
		.map(|at| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok())
		.collect::<Option<_>>()?;
	table_id(&bytes)
}

/// A partition table's ID from its bytes: a GPT's GUID, as 16 bytes in the order its text
/// gives them, or an MBR's signature, as 4 bytes, most significant first; `None` for one of
/// zeros, which names no drive.
#[cfg(any(target_os = "linux", target_os = "android", windows, test))]
fn table_id(bytes: &[u8]) -> Option<Vec<u8>> {
	if bytes.iter().all(|byte| *byte == 0) {
		return None;
	}
	match bytes.len() {
		16 => Some([b"gpt\0".as_slice(), bytes].concat()),
		4 => Some([b"mbr\0".as_slice(), bytes].concat()),
		_ => None,
	}
}

/// The physical drive's own name, as the system reads it: on Linux, from the mount table
/// down to the drives.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn system_disk_name(folder: &Path, _now: &Whereabouts) -> Option<Vec<u8>> {
	system::disk_name(folder)
}

/// The physical drive's own name, as the system reads it: on Windows, from the volume at
/// the mount that the folder's whereabouts found, unless it is a network drive.
#[cfg(windows)]
fn system_disk_name(_folder: &Path, now: &Whereabouts) -> Option<Vec<u8>> {
	if now.drive.remote {
		return None;
	}
	system::disk_name(&now.mount)
}

/// No drive's name: macOS and iOS are not read yet, and the other names count instead.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn system_disk_name(_folder: &Path, _now: &Whereabouts) -> Option<Vec<u8>> {
	None
}

/// The name of a file system's drives, as one disk: each drive's name, sorted, joined by
/// zero bytes; `None` for no drive.
#[cfg(any(target_os = "linux", target_os = "android", windows))]
fn drives_named(names: std::collections::BTreeSet<Vec<u8>>) -> Option<Vec<u8>> {
	(!names.is_empty()).then(|| names.into_iter().collect::<Vec<_>>().join(&0_u8))
}

/// Where a new replica's folder, made as `made`, is once it is renamed to `named`, in the
/// same folder: a rename keeps its ID.
pub(crate) fn whereabouts_as(made: &Path, named: &Path) -> Whereabouts {
	let mut now = whereabouts(made);
	if now.path.is_some()
		&& let (Some(parent), Some(name)) = (named.parent(), named.file_name())
	{
		now.path = Some(path_on(&canonical(parent).join(name), &now.mount));
	}
	now
}

/// A folder's path from the folder that its drive is mounted at: its names, with `/`
/// between them, in Unicode's composed form, so that a drive gives the same path on every
/// system.
fn path_on(folder: &Path, mount: &Path) -> String {
	folder
		.strip_prefix(mount)
		.unwrap_or(folder)
		.components()
		.filter_map(|part| match part {
			Component::Normal(name) => Some(name.to_string_lossy().nfc().collect::<String>()),
			_ => None,
		})
		.collect::<Vec<_>>()
		.join("/")
}

/// A folder's inode, on the systems that have them.
#[cfg(unix)]
fn folder_id(folder: &Path) -> Option<Vec<u8>> {
	use std::os::unix::fs::MetadataExt;
	fs::metadata(folder)
		.ok()
		.map(|metadata| numeric(metadata.ino()))
}

/// A folder's file ID, as Windows gives it.
#[cfg(windows)]
fn folder_id(folder: &Path) -> Option<Vec<u8>> {
	system::folder_id(folder)
}

#[cfg(not(any(unix, windows)))]
fn folder_id(_folder: &Path) -> Option<Vec<u8>> {
	None
}

/// A file system's serial, as the systems write it: a UUID with dashes, as ext4's, Btrfs's
/// and APFS's are; 32 bits as `ABCD-1234`, as FAT's and exFAT's are; or 64 bits as 16
/// hexadecimal digits, as NTFS's are. A UUID is kept as its 16 bytes, and a shorter serial as
/// a number, as Windows reads every one.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
fn parse_serial(text: &str) -> Option<Vec<u8>> {
	let bytes = hex(&text.replace('-', ""))?;
	match bytes.len() {
		16 => Some(bytes),
		1..=8 => {
			let mut number = [0; 8];
			number[8 - bytes.len()..].copy_from_slice(&bytes);
			Some(numeric(u64::from_be_bytes(number)))
		}
		_ => None,
	}
}

/// The UUID that macOS gives a FAT or exFAT volume, which other systems make alike: a
/// version 3 UUID, the MD5 of Apple's namespace of file systems' UUIDs, the serial's four
/// bytes as the volume stores them, least significant first, and on FAT, the count of its
/// sectors the same way, as Apple's `msdosfs` makes it.
#[cfg_attr(target_vendor = "apple", allow(dead_code))]
fn fat_volume_uuid(serial: u32, sectors: Option<u32>) -> Vec<u8> {
	use md5::{Digest as _, Md5};
	const NAMESPACE: [u8; 16] = [
		0xB3, 0xE2, 0x0F, 0x39, 0xF2, 0x92, 0x11, 0xD6, 0x97, 0xA4, 0x00, 0x30, 0x65, 0x43, 0xEC,
		0xAC,
	];
	let mut md5 = Md5::new();
	md5.update(NAMESPACE);
	md5.update(serial.to_le_bytes());
	if let Some(sectors) = sectors {
		md5.update(sectors.to_le_bytes());
	}
	let mut uuid: [u8; 16] = md5.finalize().into();
	uuid[6] = (uuid[6] & 0x0F) | 0x30;
	uuid[8] = (uuid[8] & 0x3F) | 0x80;
	uuid.to_vec()
}

/// A serial or an ID that is a number: its bytes, most significant first, without leading
/// zeros.
fn numeric(number: u64) -> Vec<u8> {
	let bytes = number.to_be_bytes();
	let start = bytes.iter().position(|&byte| byte != 0).unwrap_or(7);
	bytes[start..].to_vec()
}

/// Bytes written as pairs of hexadecimal digits, either case.
fn hex(text: &str) -> Option<Vec<u8>> {
	if text.is_empty() || !text.len().is_multiple_of(2) {
		return None;
	}
	(0..text.len())
		.step_by(2)
		.map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
		.collect()
}

/// Whether a replica is here, or a copy of it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
	/// It is where it was made, or where it moved to on its drive, or with it.
	Here,
	/// It is a copy: on another drive, as another folder, or on the system's drive of
	/// another computer.
	Copy,
}

/// Where a replica was made: the place file, written once. In CBOR, the array
/// `[1, salt, serial, folder, path, computer, volume]`, with the hashes of the drive's serial,
/// of the folder's ID, of the computer's ID and of a FAT or exFAT volume's UUID, and `null`
/// for what was not read. A place file written before the volume's UUID was read has no
/// last item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaceFile {
	/// The key of the hashes, drawn at random when the replica is made.
	salt: [u8; 32],
	serial: Option<[u8; 32]>,
	folder: Option<[u8; 32]>,
	path: Option<String>,
	/// Recorded only on the drive that the system runs from.
	computer: Option<[u8; 32]>,
	volume: Option<[u8; 32]>,
}

impl PlaceFile {
	/// The place file of a new replica, where it is now.
	pub(crate) fn new(now: &Whereabouts, rng: &mut impl CryptoRng) -> Self {
		let mut salt = [0; 32];
		rng.fill_bytes(&mut salt);
		let hashed = |context, value: &Option<Vec<u8>>| {
			value.as_deref().map(|value| hash(&salt, context, value))
		};
		Self {
			salt,
			serial: hashed(SERIAL, &now.drive.serial),
			folder: hashed(FOLDER, &now.folder),
			path: now.path.clone(),
			computer: now
				.drive
				.system
				.then(|| hashed(COMPUTER, &now.computer))
				.flatten(),
			volume: hashed(VOLUME, &now.drive.volume),
		}
	}

	/// Reads a place file, or `None` if there is none.
	pub(crate) fn read(path: &Path) -> Result<Option<Self>, NodeError> {
		match fs::read(path) {
			Ok(bytes) => cbor::from_slice(&bytes)
				.map(Some)
				.map_err(|error| NodeError::Damaged(error.to_string())),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(error) => Err(NodeError::file(path)(error)),
		}
	}

	/// Writes the place file of a new replica, which fails if one is there.
	pub(crate) fn create(&self, path: &Path) -> Result<(), NodeError> {
		use std::io::Write as _;
		let mut file = fs::OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(path)
			.map_err(|error| match error.kind() {
				io::ErrorKind::AlreadyExists => NodeError::Exists,
				_ => NodeError::file(path)(error),
			})?;
		file.write_all(&cbor::to_vec(self))
			.and_then(|()| file.sync_all())
			.map_err(NodeError::file(path))
	}

	/// Whether the replica whose folder is `folder` is here, where it is now `now`. Its drive
	/// matches by its serial, and on the drive that the system runs from, its computer by its
	/// ID. Its folder matches by its ID, or where none is kept, by its path, or if nothing is
	/// at the path it recorded any more. What either side lacks is not compared.
	pub(crate) fn verdict(&self, now: &Whereabouts, folder: &Path) -> Verdict {
		let matches =
			|recorded: &Option<[u8; 32]>, context, value: &Option<Vec<u8>>| match (recorded, value)
			{
				(Some(recorded), Some(value)) => *recorded == hash(&self.salt, context, value),
				_ => true,
			};
		// On a phone, the phone's ID has to be there and match: a copy on another phone has
		// none of the first one's.
		if now.phone {
			let id = self.computer.zip(now.computer.as_deref());
			return match id {
				Some((recorded, id)) if recorded == hash(&self.salt, COMPUTER, id) => Verdict::Here,
				_ => Verdict::Copy,
			};
		}
		if !matches(&self.serial, SERIAL, &now.drive.serial)
			|| !matches(&self.volume, VOLUME, &now.drive.volume)
			|| (now.drive.system && !matches(&self.computer, COMPUTER, &now.computer))
		{
			return Verdict::Copy;
		}
		// A replica made off the drive that the system runs from records no computer's ID,
		// which this computer has: found on that drive, it was copied there, from a stick or
		// another drive whose serial this system may not read.
		if now.drive.system && self.computer.is_none() && now.computer.is_some() {
			return Verdict::Copy;
		}
		if self.folder.is_some() && now.folder.is_some() {
			return if matches(&self.folder, FOLDER, &now.folder) {
				Verdict::Here
			} else {
				Verdict::Copy
			};
		}
		// Without IDs of folders, the path it recorded, looked up on its drive, has to lead
		// to this very folder, as it does in another case on a drive that ignores case, or to
		// nothing any more, as when the folder was moved on its drive.
		let Some(recorded) = &self.path else {
			return Verdict::Here;
		};
		let before = recorded
			.split('/')
			.fold(now.mount.clone(), |path, name| path.join(name));
		match fs::symlink_metadata(&before) {
			Err(error) if error.kind() == io::ErrorKind::NotFound => Verdict::Here,
			Ok(_) if same_folder(&before, folder) => Verdict::Here,
			_ => Verdict::Copy,
		}
	}
}

/// The hash of a serial or an ID, keyed with a replica's salt.
fn hash(salt: &[u8; 32], context: &str, value: &[u8]) -> [u8; 32] {
	let mut hasher = blake3::Hasher::new_keyed(salt);
	hasher.update(context.as_bytes());
	hasher.update(&[0]);
	hasher.update(value);
	*hasher.finalize().as_bytes()
}

/// Whether two paths lead to one folder, as two spellings of it do on a drive that ignores
/// case: the same inode on the same device, which lasts while the drive is mounted, on FAT
/// too.
#[cfg(unix)]
pub(crate) fn same_folder(one: &Path, other: &Path) -> bool {
	use std::os::unix::fs::MetadataExt;
	matches!(
		(fs::metadata(one), fs::metadata(other)),
		(Ok(one), Ok(other)) if one.dev() == other.dev() && one.ino() == other.ino()
	)
}

/// Whether two paths lead to one folder, as Windows resolves them, with the case of the
/// names it keeps.
#[cfg(not(unix))]
pub(crate) fn same_folder(one: &Path, other: &Path) -> bool {
	matches!(
		(fs::canonicalize(one), fs::canonicalize(other)),
		(Ok(one), Ok(other)) if one == other
	)
}

impl ToCbor for PlaceFile {
	fn to_cbor(&self) -> Value {
		let hash =
			|hash: &Option<[u8; 32]>| hash.map_or(Value::Null, |hash| Value::Bytes(hash.to_vec()));
		Value::Array(vec![
			SUITE.to_cbor(),
			Value::Bytes(self.salt.to_vec()),
			hash(&self.serial),
			hash(&self.folder),
			self.path.as_ref().map_or(Value::Null, ToCbor::to_cbor),
			hash(&self.computer),
			hash(&self.volume),
		])
	}
}

impl FromCbor for PlaceFile {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a place file: [1, salt, serial, folder, path, computer, volume]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let (suite, salt, serial, folder, path, computer, volume) = match items.as_slice() {
			[suite, salt, serial, folder, path, computer, volume] => {
				(suite, salt, serial, folder, path, computer, volume)
			}
			// Written before the volume's UUID was read.
			[suite, salt, serial, folder, path, computer] => {
				(suite, salt, serial, folder, path, computer, &Value::Null)
			}
			_ => return Err(cbor::Error::Expected(EXPECTED)),
		};
		if u8::from_cbor(suite) != Ok(SUITE) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		let bytes = |value: &Value| match value {
			Value::Bytes(bytes) => {
				<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| cbor::Error::Expected(EXPECTED))
			}
			_ => Err(cbor::Error::Expected(EXPECTED)),
		};
		let optional = |value: &Value| match value {
			Value::Null => Ok(None),
			value => bytes(value).map(Some),
		};
		Ok(Self {
			salt: bytes(salt)?,
			serial: optional(serial)?,
			folder: optional(folder)?,
			path: match path {
				Value::Null => None,
				path => Some(String::from_cbor(path)?),
			},
			computer: optional(computer)?,
			volume: optional(volume)?,
		})
	}
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod system {
	//! Linux, and `nib` in a shell on Android, from the mount table.

	use std::fs;
	use std::path::{Path, PathBuf};

	use super::{Drive, Whereabouts, fat_volume_uuid, folder_id, hex, parse_serial, path_on};

	/// File systems on another computer, or reached through one: SQLite's write-ahead log
	/// does not work on them. FUSE mounts are named by their subtype.
	const REMOTE: &[&str] = &[
		"nfs",
		"nfs4",
		"cifs",
		"smb3",
		"smbfs",
		"ncpfs",
		"afs",
		"9p",
		"drvfs",
		"ceph",
		"glusterfs",
		"lustre",
		"gpfs",
		"davfs",
		"fuse.sshfs",
		"fuse.rclone",
		"fuse.s3fs",
		"fuse.gcsfuse",
		"fuse.davfs2",
		"fuse.glusterfs",
	];

	/// File systems whose IDs of folders last, which a move keeps: FAT's and exFAT's are made
	/// up when they are mounted, and so are those of many file systems in FUSE.
	const FOLDER_IDS: &[&str] = &[
		"ext2", "ext3", "ext4", "btrfs", "xfs", "zfs", "f2fs", "bcachefs", "jfs", "nilfs2",
		"ntfs3", "hfsplus", "tmpfs",
	];

	/// A mount, as the mount table lists it.
	#[derive(Debug, PartialEq, Eq)]
	pub(super) struct Mount {
		pub(super) point: PathBuf,
		pub(super) kind: String,
		pub(super) source: String,
	}

	pub(super) fn whereabouts(folder: &Path) -> Whereabouts {
		let mounts =
			fs::read_to_string("/proc/self/mountinfo").map_or_default(|table| mounts(&table));
		let Some(mount) = mount_of(&mounts, folder) else {
			return Whereabouts {
				drive: Drive::default(),
				mount: PathBuf::from("/"),
				folder: None,
				path: Some(path_on(folder, Path::new("/"))),
				computer: computer(),
				phone: false,
			};
		};
		let device = device(mount);
		let pool = pool(mount);
		let root = mount_of(&mounts, Path::new("/"));
		let system = mount.point == Path::new("/")
			|| device.is_some() && device == root.and_then(self::device)
			|| pool.is_some() && pool == root.and_then(self::pool);
		let folder_ids = FOLDER_IDS.contains(&mount.kind.as_str());
		// A file system in FUSE is named by udev, which the mount table hides.
		let kind = if mount.kind.starts_with("fuse") {
			device
				.as_deref()
				.and_then(|device| udev_value(device, "ID_FS_TYPE"))
				.unwrap_or_default()
		} else {
			mount.kind.clone()
		};
		let named = device.as_deref().and_then(serial_name);
		Whereabouts {
			drive: Drive {
				serial: named.as_deref().and_then(parse_serial),
				volume: device
					.as_deref()
					.zip(named.as_deref())
					.and_then(|(device, name)| volume(device, &kind, name)),
				folder_ids,
				system,
				remote: REMOTE.contains(&mount.kind.as_str()),
				cloud: false,
				unjournaled: UNJOURNALED.contains(&kind.as_str()),
				disk: None,
			},
			folder: folder_ids.then(|| folder_id(folder)).flatten(),
			path: Some(path_on(folder, &mount.point)),
			mount: mount.point.clone(),
			computer: computer(),
			phone: false,
		}
	}

	/// File systems that keep no journal, as the mount table and udev name them.
	const UNJOURNALED: &[&str] = &["vfat", "msdos", "exfat", "ext2"];

	/// The major and minor numbers of a device.
	fn numbers(device: &Path) -> Option<(u64, u64)> {
		use std::os::unix::fs::MetadataExt;
		let number = fs::metadata(device).ok()?.rdev();
		let major = ((number >> 8) & 0xfff) | ((number >> 32) & !0xfff);
		let minor = (number & 0xff) | ((number >> 12) & !0xff);
		Some((major, minor))
	}

	/// A property of a device that udev recorded, as the type of its file system.
	fn udev_value(device: &Path, name: &str) -> Option<String> {
		let (major, minor) = numbers(device)?;
		let record = fs::read_to_string(format!("/run/udev/data/b{major}:{minor}")).ok()?;
		let prefix = format!("E:{name}=");
		record
			.lines()
			.find_map(|line| line.strip_prefix(prefix.as_str()))
			.map(str::to_owned)
	}

	/// On FAT and exFAT, the volume's UUID as macOS makes it, from its serial, as
	/// `/dev/disk/by-uuid` names it, and on FAT, the count of its sectors: the size of its
	/// file system that udev records, in the device's sectors.
	fn volume(device: &Path, kind: &str, name: &str) -> Option<Vec<u8>> {
		let serial = u32::from_str_radix(&name.replace('-', ""), 16).ok()?;
		match kind {
			"exfat" => Some(fat_volume_uuid(serial, None)),
			"vfat" | "msdos" => {
				let size: u64 = udev_value(device, "ID_FS_SIZE")?.parse().ok()?;
				let (major, minor) = numbers(device)?;
				// A partition's queue is its disk's.
				let sector = ["queue", "../queue"]
					.iter()
					.find_map(|queue| {
						fs::read_to_string(format!(
							"/sys/dev/block/{major}:{minor}/{queue}/logical_block_size"
						))
						.ok()?
						.trim()
						.parse::<u64>()
						.ok()
					})
					.unwrap_or(512);
				let sectors = u32::try_from(size / sector).ok()?;
				Some(fat_volume_uuid(serial, Some(sectors)))
			}
			_ => None,
		}
	}

	/// The device that a mount's file system is on, if it is on one.
	fn device(mount: &Mount) -> Option<PathBuf> {
		mount
			.source
			.starts_with("/dev/")
			.then(|| fs::canonicalize(&mount.source).ok())
			.flatten()
	}

	/// The pool of a ZFS dataset, which the mount table names in place of a device.
	fn pool(mount: &Mount) -> Option<&str> {
		(mount.kind == "zfs")
			.then(|| mount.source.split('/').next())
			.flatten()
	}

	/// The mounts that a mount table lists, in its order.
	pub(super) fn mounts(table: &str) -> Vec<Mount> {
		table
			.lines()
			.filter_map(|line| {
				let fields: Vec<&str> = line.split(' ').collect();
				let point = fields.get(4)?;
				let separator = fields.iter().skip(6).position(|field| *field == "-")? + 6;
				Some(Mount {
					point: PathBuf::from(unescaped(point)),
					kind: (*fields.get(separator + 1)?).to_owned(),
					source: unescaped(fields.get(separator + 2)?),
				})
			})
			.collect()
	}

	/// The mount that holds a folder: the one at the longest path that holds it, and of
	/// several at one path, the last, which hides the others.
	pub(super) fn mount_of<'a>(mounts: &'a [Mount], folder: &Path) -> Option<&'a Mount> {
		mounts
			.iter()
			.enumerate()
			.filter(|(_, mount)| folder.starts_with(&mount.point))
			.max_by_key(|(at, mount)| (mount.point.components().count(), *at))
			.map(|(_, mount)| mount)
	}

	/// A field of the mount table, whose spaces, tabs, line feeds and backslashes are written
	/// as three octal digits after a backslash.
	fn unescaped(field: &str) -> String {
		let bytes = field.as_bytes();
		let mut out = Vec::with_capacity(bytes.len());
		let mut at = 0;
		while at < bytes.len() {
			let octal = bytes
				.get(at + 1..at + 4)
				.filter(|_| bytes[at] == b'\\')
				.and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok());
			if let Some(byte) = octal {
				out.push(byte);
				at += 4;
			} else {
				out.push(bytes[at]);
				at += 1;
			}
		}
		String::from_utf8_lossy(&out).into_owned()
	}

	/// The serial of the file system on a device, as udev lists it under `/dev/disk/by-uuid`.
	fn serial_name(device: &Path) -> Option<String> {
		fs::read_dir("/dev/disk/by-uuid")
			.ok()?
			.flatten()
			.find(|entry| fs::canonicalize(entry.path()).is_ok_and(|target| target == device))
			.map(|entry| entry.file_name().to_string_lossy().into_owned())
	}

	/// The physical drives that hold a folder's file system, named by their partition
	/// tables as udev records them, or by their serials: through device mapper, as for an
	/// encrypted drive, and RAID, down to the partitions, and from each to its drive. A file
	/// system on several drives is named by all of them, sorted, as one disk.
	pub(super) fn disk_name(folder: &Path) -> Option<Vec<u8>> {
		let mounts = fs::read_to_string("/proc/self/mountinfo")
			.map(|table| mounts(&table))
			.ok()?;
		let device = device(mount_of(&mounts, folder)?)?;
		let mut leaves = std::collections::BTreeSet::new();
		under(device.file_name()?.to_str()?, &mut leaves, 0);
		let mut names = std::collections::BTreeSet::new();
		for leaf in leaves {
			names.insert(drive_name(&whole_drive(&leaf))?);
		}
		super::drives_named(names)
	}

	/// The block devices that a block device is made of, as device mapper and RAID make
	/// them, down to those made of none.
	fn under(name: &str, leaves: &mut std::collections::BTreeSet<String>, depth: usize) {
		let made_of: Vec<String> = fs::read_dir(format!("/sys/class/block/{name}/slaves"))
			.map_or_default(|entries| {
				entries
					.flatten()
					.map(|entry| entry.file_name().to_string_lossy().into_owned())
					.collect()
			});
		if made_of.is_empty() || depth > 8 {
			leaves.insert(name.to_owned());
			return;
		}
		for each in made_of {
			under(&each, leaves, depth + 1);
		}
	}

	/// The drive that a partition is on, or the device itself if it is no partition.
	fn whole_drive(name: &str) -> String {
		let block = PathBuf::from(format!("/sys/class/block/{name}"));
		if !block.join("partition").exists() {
			return name.to_owned();
		}
		fs::canonicalize(&block)
			.ok()
			.and_then(|path| Some(path.parent()?.file_name()?.to_string_lossy().into_owned()))
			.unwrap_or_else(|| name.to_owned())
	}

	/// A drive's name: its partition table's ID, as every system reads it, else its WWN or
	/// serial, as udev records them.
	fn drive_name(drive: &str) -> Option<Vec<u8>> {
		let numbers = fs::read_to_string(format!("/sys/class/block/{drive}/dev")).ok()?;
		let record = fs::read_to_string(format!("/run/udev/data/b{}", numbers.trim())).ok()?;
		let value = |name: &str| {
			let prefix = format!("E:{name}=");
			record
				.lines()
				.find_map(|line| line.strip_prefix(prefix.as_str()))
				.filter(|value| !value.is_empty())
				.map(str::to_owned)
		};
		value("ID_PART_TABLE_UUID")
			.and_then(|id| super::partition_table(&id))
			.or_else(|| {
				["ID_WWN", "ID_SERIAL"].iter().find_map(|name| {
					value(name).map(|value| format!("{name}={}", value.to_lowercase()).into_bytes())
				})
			})
	}

	/// The computer's ID: systemd's machine ID, or D-Bus's where systemd's is missing.
	pub(super) fn computer() -> Option<Vec<u8>> {
		["/etc/machine-id", "/var/lib/dbus/machine-id"]
			.iter()
			.find_map(|path| hex(fs::read_to_string(path).ok()?.trim()))
	}
}

#[cfg(windows)]
mod system {
	//! Windows: the volume that holds the folder, among the mounted drives that `sysinfo`
	//! lists; the serial of its file system and the folder's ID, as `file-id` reads them; the
	//! drives under the volume and their partition tables, as `nibkeep-drives` reads them;
	//! and the computer's ID and the cloud files' sync roots, in the registry. Nothing here
	//! needs `unsafe` code of its own.

	use std::path::{Path, PathBuf};

	use file_id::FileId;
	use nibkeep_drives::PartitionTable;
	use sysinfo::{Disk, Disks};
	use winreg::HKLM;
	use winreg::enums::{KEY_READ, KEY_WOW64_64KEY};

	use super::{Drive, Whereabouts, fat_volume_uuid, hex, numeric, path_on};

	/// A path without the `\\?\` that canonical paths have on Windows, as the mounted drives
	/// are named, with a share's in the form `\\server\share`.
	fn plain(path: &Path) -> PathBuf {
		let text = path.to_string_lossy();
		if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
			PathBuf::from(format!(r"\\{share}"))
		} else if let Some(local) = text.strip_prefix(r"\\?\") {
			PathBuf::from(local)
		} else {
			path.to_owned()
		}
	}

	/// The mounted drive that holds a path: the one at the longest folder that holds it.
	/// Drives that are neither fixed nor removable, as network drives, are not listed.
	fn drive_of<'a>(disks: &'a Disks, path: &Path) -> Option<&'a Disk> {
		disks
			.iter()
			.filter(|disk| path.starts_with(disk.mount_point()))
			.max_by_key(|disk| disk.mount_point().as_os_str().len())
	}

	/// Whether a drive's file system keeps IDs of folders that a move keeps.
	fn keeps_ids(disk: Option<&Disk>) -> bool {
		disk.is_some_and(|disk| {
			let kind = disk.file_system().to_string_lossy();
			kind.eq_ignore_ascii_case("NTFS") || kind.eq_ignore_ascii_case("ReFS")
		})
	}

	pub(super) fn whereabouts(folder: &Path) -> Whereabouts {
		let shown = plain(folder);
		let disks = Disks::new_with_refreshed_list().ok();
		let disk = disks.as_ref().and_then(|disks| drive_of(disks, &shown));
		let mount = disk.map_or_else(
			|| shown.ancestors().last().unwrap_or(&shown).to_owned(),
			|disk| disk.mount_point().to_owned(),
		);
		let system = std::env::var_os("SystemRoot")
			.and_then(|windows| drive_of(disks.as_ref()?, &plain(Path::new(&windows))))
			.zip(disk)
			.is_some_and(|(windows, disk)| windows.mount_point() == disk.mount_point());
		let folder_ids = keeps_ids(disk);
		let (serial, id) = identity(folder, disk);
		let exfat = disk.is_some_and(|disk| {
			disk.file_system()
				.to_string_lossy()
				.eq_ignore_ascii_case("exFAT")
		});
		Whereabouts {
			drive: Drive {
				serial: serial.map(numeric),
				// On exFAT, made from the serial alone; on FAT, it takes the count of sectors,
				// which no safe crate reads here.
				volume: serial
					.filter(|_| exfat)
					.and_then(|serial| u32::try_from(serial).ok())
					.map(|serial| fat_volume_uuid(serial, None)),
				folder_ids,
				system,
				// A share, or a drive that is neither fixed nor removable; not one that is not
				// there, as a stick that was taken out.
				remote: shown.to_string_lossy().starts_with(r"\\")
					|| (disk.is_none() && mount.exists()),
				cloud: in_sync_root(&shown),
				unjournaled: disk.is_some_and(|disk| {
					let kind = disk.file_system().to_string_lossy();
					["FAT", "FAT32", "exFAT"]
						.iter()
						.any(|fat| kind.eq_ignore_ascii_case(fat))
				}),
				disk: None,
			},
			folder: id.filter(|_| folder_ids),
			path: Some(path_on(&shown, &mount)),
			mount,
			computer: computer(),
			phone: false,
		}
	}

	/// The physical drives under the volume mounted at `mount`, named by their partition
	/// tables, as their layouts give them: a GPT's GUID, or an MBR's signature. A volume on
	/// several drives is named by all of them, as one disk, as on Linux; one on a drive
	/// without a partition table, or with an ID of zeros, by none, as Windows reads no WWN or
	/// serial of a drive here.
	pub(super) fn disk_name(mount: &Path) -> Option<Vec<u8>> {
		let mut names = std::collections::BTreeSet::new();
		for table in nibkeep_drives::partition_tables(mount).ok()? {
			names.insert(match table? {
				PartitionTable::Gpt(guid) => super::table_id(&guid)?,
				PartitionTable::Mbr(signature) => super::table_id(&signature.to_be_bytes())?,
			});
		}
		super::drives_named(names)
	}

	/// The serial of a folder's volume, and the folder's ID: on NTFS, the number of its record
	/// in the master file table, the lower 48 bits of the ID that Windows gives, whose upper
	/// 16 count the record's reuses, as Linux reads it too.
	fn identity(folder: &Path, disk: Option<&Disk>) -> (Option<u64>, Option<Vec<u8>>) {
		let ntfs = disk.is_some_and(|disk| {
			disk.file_system()
				.to_string_lossy()
				.eq_ignore_ascii_case("NTFS")
		});
		match file_id::get_file_id(folder) {
			Ok(FileId::HighRes {
				volume_serial_number,
				file_id,
			}) => {
				let id = if ntfs {
					numeric((file_id & 0xFFFF_FFFF_FFFF) as u64)
				} else {
					file_id.to_be_bytes().to_vec()
				};
				(Some(volume_serial_number), Some(id))
			}
			Ok(FileId::LowRes {
				volume_serial_number,
				file_index,
			}) => {
				let id = if ntfs {
					file_index & 0xFFFF_FFFF_FFFF
				} else {
					file_index
				};
				(Some(u64::from(volume_serial_number)), Some(numeric(id)))
			}
			_ => (None, None),
		}
	}

	/// A folder's ID, as [`whereabouts`] reads it.
	pub(super) fn folder_id(folder: &Path) -> Option<Vec<u8>> {
		let disks = Disks::new_with_refreshed_list().ok();
		let disk = disks
			.as_ref()
			.and_then(|disks| drive_of(disks, &plain(folder)));
		identity(folder, disk).1
	}

	/// Whether a folder is in a sync root of Windows' cloud files, as OneDrive's, whose files'
	/// content a program may take away to the cloud: the roots that the registry lists.
	fn in_sync_root(folder: &Path) -> bool {
		let Ok(manager) = HKLM.open_subkey_with_flags(
			r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager",
			KEY_READ | KEY_WOW64_64KEY,
		) else {
			return false;
		};
		manager.enum_keys().flatten().any(|root| {
			manager
				.open_subkey_with_flags(format!(r"{root}\UserSyncRoots"), KEY_READ)
				.is_ok_and(|users| {
					users.enum_values().flatten().any(|(_, value)| {
						let path = value.to_string();
						!path.is_empty()
							&& folder.starts_with(plain(Path::new(path.trim_end_matches('\0'))))
					})
				})
		})
	}

	/// The computer's ID: the GUID that Windows draws when it is installed.
	pub(super) fn computer() -> Option<Vec<u8>> {
		let guid: String = HKLM
			.open_subkey_with_flags(
				r"SOFTWARE\Microsoft\Cryptography",
				KEY_READ | KEY_WOW64_64KEY,
			)
			.and_then(|key| key.get_value("MachineGuid"))
			.ok()?;
		hex(&guid.replace('-', ""))
	}
}

#[cfg(target_vendor = "apple")]
mod system {
	//! macOS, and iOS outside an app's own folder: the volume that holds the folder, as the
	//! system's `statfs` of the folder reports it, which finds volumes that Finder does not
	//! show too, as one attached with `-nobrowse`; and on macOS, the hardware UUID that
	//! `ioreg` reports, and the UUID of a FAT or exFAT volume that `diskutil` reports, which
	//! the system makes from its serial. No serial is read: only the system reads a drive's
	//! raw sectors.

	use std::ffi::{OsStr, c_char};
	use std::os::unix::ffi::OsStrExt as _;
	use std::path::{Path, PathBuf};

	use super::{Drive, Whereabouts, folder_id, path_on};

	/// File systems whose IDs of folders do not last.
	const NO_FOLDER_IDS: &[&str] = &["msdos", "exfat"];

	/// File systems on another computer.
	const REMOTE: &[&str] = &["smbfs", "nfs", "afpfs", "webdav", "ftp"];

	/// The flag of a volume on the computer's own drives, `MNT_LOCAL` in `<sys/mount.h>`.
	const LOCAL: u32 = 0x0000_1000;

	/// The volume that holds a folder, or the deepest folder above it that exists, as
	/// `statfs` reports it: the folder it is mounted at, the name of its file system, and
	/// whether it is on the computer's own drives. None if the system lets it not be read,
	/// rather than the volume of a folder above it, which may be another.
	fn volume(folder: &Path) -> Option<(PathBuf, String, bool)> {
		let found = folder
			.ancestors()
			.find_map(|folder| match rustix::fs::statfs(folder) {
				Ok(found) => Some(Some(found)),
				// A folder still to be made: the one above it holds it.
				Err(rustix::io::Errno::NOENT) => None,
				Err(_) => Some(None),
			})??;
		let mount = PathBuf::from(OsStr::from_bytes(&text(&found.f_mntonname)));
		let kind = String::from_utf8_lossy(&text(&found.f_fstypename)).into_owned();
		Some((mount, kind, found.f_flags & LOCAL != 0))
	}

	/// The bytes of a C string in a fixed array, up to its end.
	fn text(chars: &[c_char]) -> Vec<u8> {
		chars
			.iter()
			.take_while(|&&char| char != 0)
			.map(|&char| char.cast_unsigned())
			.collect()
	}

	pub(super) fn whereabouts(folder: &Path) -> Whereabouts {
		let volume = volume(folder);
		let mounted = volume.as_ref().map(|(mount, _, _)| mount.as_path());
		// The system's volume, and the volume of its data, which holds the home folders.
		let system = mounted.is_none_or(|mounted| {
			mounted == Path::new("/") || mounted == Path::new("/System/Volumes/Data")
		});
		// The home folders are on the volume of the system's data, through links from the
		// system's volume, such as `/Users`, so a folder there is placed by its path from `/`.
		let mount = mounted
			.filter(|mounted| folder.starts_with(mounted))
			.map_or_else(|| PathBuf::from("/"), Path::to_owned);
		let kind = volume.as_ref().map(|(_, kind, _)| kind.as_str());
		let folder_ids = !kind.is_some_and(|kind| NO_FOLDER_IDS.contains(&kind));
		let remote = volume
			.as_ref()
			.is_some_and(|(_, kind, local)| !local || REMOTE.contains(&kind.as_str()));
		Whereabouts {
			drive: Drive {
				serial: None,
				folder_ids,
				system,
				remote,
				cloud: in_file_provider(folder),
				unjournaled: !folder_ids,
				volume: if folder_ids {
					None
				} else {
					volume_uuid(&mount)
				},
				disk: None,
			},
			folder: folder_ids.then(|| folder_id(folder)).flatten(),
			path: Some(path_on(folder, &mount)),
			mount,
			computer: computer(),
			phone: false,
		}
	}

	/// Whether a folder is where File Provider keeps the files of iCloud Drive and of other
	/// programs that sync folders, whose content may stay in the cloud until opened: in their
	/// folders, or in the Desktop or Documents when iCloud keeps them, as iCloud Drive's links
	/// to them show.
	fn in_file_provider(folder: &Path) -> bool {
		let Some(home) = home() else {
			return false;
		};
		let library = home.join("Library");
		let icloud = library.join("Mobile Documents");
		folder.starts_with(library.join("CloudStorage"))
			|| folder.starts_with(&icloud)
			|| ["Desktop", "Documents"].into_iter().any(|name| {
				// iCloud Drive is read only for a folder in one of them.
				std::fs::canonicalize(home.join(name)).is_ok_and(|own| {
					folder.starts_with(&own)
						&& std::fs::canonicalize(icloud.join("com~apple~CloudDocs").join(name))
							.is_ok_and(|linked| linked == own)
				})
			})
	}

	/// The person's home folder. In an app's sandbox, `HOME` names the app's container in
	/// it, under `Library/Containers`.
	fn home() -> Option<PathBuf> {
		let home = PathBuf::from(std::env::var_os("HOME")?);
		let outside = home
			.ancestors()
			.find(|folder| folder.ends_with("Library/Containers"))
			.and_then(|containers| containers.parent()?.parent())
			.map(Path::to_owned);
		Some(outside.unwrap_or(home))
	}

	/// The computer's ID: the hardware UUID that `ioreg` reports, read once.
	#[cfg(target_os = "macos")]
	pub(super) fn computer() -> Option<Vec<u8>> {
		static READ: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
		READ.get_or_init(|| {
			let listed = std::process::Command::new("/usr/sbin/ioreg")
				.args(["-rd1", "-c", "IOPlatformExpertDevice"])
				.output()
				.ok()?;
			let text = String::from_utf8_lossy(&listed.stdout);
			let line = text
				.lines()
				.find(|line| line.contains("\"IOPlatformUUID\""))?;
			let uuid = line.rsplit('"').nth(1)?;
			super::hex(&uuid.replace('-', ""))
		})
		.clone()
	}

	/// The UUID of the FAT or exFAT volume mounted at `mount`, as `diskutil` reports it: read
	/// once for each volume mounted there, as it takes a fifth of a second.
	#[cfg(target_os = "macos")]
	fn volume_uuid(mount: &Path) -> Option<Vec<u8>> {
		use std::os::unix::fs::MetadataExt as _;
		use std::sync::{Mutex, PoisonError};
		type Known = Vec<(PathBuf, u64, Option<Vec<u8>>)>;
		static KNOWN: Mutex<Known> = Mutex::new(Vec::new());
		let device = std::fs::metadata(mount).ok()?.dev();
		let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some((_, _, uuid)) = known
			.iter()
			.find(|(at, on, _)| at == mount && *on == device)
		{
			return uuid.clone();
		}
		let uuid = std::process::Command::new("/usr/sbin/diskutil")
			.args(["info", "-plist"])
			.arg(mount)
			.output()
			.ok()
			.and_then(|listed| {
				let text = String::from_utf8_lossy(&listed.stdout);
				let after = text.split("<key>VolumeUUID</key>").nth(1)?;
				let uuid = after.split("<string>").nth(1)?.split("</string>").next()?;
				super::hex(&uuid.trim().replace('-', ""))
			});
		known.push((mount.to_owned(), device, uuid.clone()));
		uuid
	}

	/// iOS lists no volumes of FAT and exFAT that an app could keep a library on.
	#[cfg(not(target_os = "macos"))]
	fn volume_uuid(_mount: &Path) -> Option<Vec<u8>> {
		None
	}

	/// iOS gives apps no ID of the device but through the app, which declares its folder.
	#[cfg(not(target_os = "macos"))]
	pub(super) fn computer() -> Option<Vec<u8>> {
		None
	}
}

#[cfg(not(any(
	target_os = "linux",
	target_os = "android",
	windows,
	target_vendor = "apple"
)))]
mod system {
	//! Other systems: the folder's path alone.

	use std::path::{Path, PathBuf};

	use super::{Drive, Whereabouts, path_on};

	pub(super) fn whereabouts(folder: &Path) -> Whereabouts {
		Whereabouts {
			drive: Drive::default(),
			mount: PathBuf::from("/"),
			folder: None,
			path: Some(path_on(folder, Path::new("/"))),
			computer: None,
			phone: false,
		}
	}

	/// No computer's ID: other systems give none that NibKeep reads.
	pub(super) fn computer() -> Option<Vec<u8>> {
		None
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	/// Where a folder is, on a drive that keeps the IDs of folders unless `id` is `None`.
	fn at(
		path: &str,
		serial: Option<&[u8]>,
		id: Option<&[u8]>,
		system: bool,
		computer: Option<&[u8]>,
	) -> Whereabouts {
		Whereabouts {
			drive: Drive {
				serial: serial.map(<[u8]>::to_vec),
				folder_ids: id.is_some(),
				system,
				..Drive::default()
			},
			mount: PathBuf::from("/nowhere"),
			folder: id.map(<[u8]>::to_vec),
			path: Some(path.to_owned()),
			computer: computer.map(<[u8]>::to_vec),
			phone: false,
		}
	}

	#[test]
	fn a_disk_is_named_by_its_drive_else_its_file_system_else_its_computer() {
		let folder = std::env::temp_dir().join("nibkeep place, disk names");
		let named = |kind: &str, name: &[u8]| [kind.as_bytes(), b"\0", name].concat();
		let declared = |drive: Drive, computer: Option<&[u8]>| {
			declare_drive(&folder, drive, computer.map(<[u8]>::to_vec));
			disk_name(&folder)
		};
		let drive = Drive {
			serial: Some(b"file system".to_vec()),
			system: true,
			disk: Some(b"drive".to_vec()),
			..Drive::default()
		};
		assert_eq!(
			declared(drive.clone(), Some(b"computer")),
			Some(named("drive", b"drive"))
		);
		let no_drive = Drive {
			disk: None,
			..drive.clone()
		};
		assert_eq!(
			declared(no_drive.clone(), Some(b"computer")),
			Some(named("file system", b"file system"))
		);
		let no_serial = Drive {
			serial: None,
			..no_drive
		};
		assert_eq!(
			declared(no_serial.clone(), Some(b"computer")),
			Some(named("computer", b"computer"))
		);
		// Off the system's drive, the computer names no disk: the folder is on one of its own.
		let elsewhere = Drive {
			system: false,
			..no_serial
		};
		assert_eq!(declared(elsewhere, Some(b"computer")), None);
		// On a phone, the name that one developer's apps share, else the phone's ID.
		declare_phone(&folder, b"phone".to_vec());
		assert_eq!(disk_name(&folder), Some(named("phone", b"phone")));
		declare_phone_with_disk(&folder, b"phone".to_vec(), b"apps".to_vec());
		assert_eq!(disk_name(&folder), Some(named("phone apps", b"apps")));
	}

	#[test]
	fn a_partition_table_is_named_alike_by_every_system() {
		// As Linux's udev writes a GPT's GUID and an MBR's signature, and Windows' Get-Disk
		// shows them.
		let gpt = partition_table("2fd12b0b-49b1-45e2-a237-f582ae445a87");
		assert_eq!(
			partition_table("{2FD12B0B-49B1-45E2-A237-F582AE445A87}"),
			gpt
		);
		assert_eq!(gpt.as_ref().map(Vec::len), Some(20));
		let signature: u32 = 3_735_928_559;
		assert_eq!(
			partition_table(&format!("{signature:08x}")),
			partition_table("deadbeef")
		);
		assert_ne!(partition_table("deadbeef"), None);
		// As Windows reads them from the drive's layout: the GUID's bytes in the order of its
		// text, and the signature as a number.
		assert_eq!(
			table_id(&[
				0x2f, 0xd1, 0x2b, 0x0b, 0x49, 0xb1, 0x45, 0xe2, 0xa2, 0x37, 0xf5, 0x82, 0xae, 0x44,
				0x5a, 0x87
			]),
			gpt
		);
		assert_eq!(
			table_id(&signature.to_be_bytes()),
			partition_table("deadbeef")
		);
		assert_eq!(table_id(&[0; 16]), None);
		// Zeros name nothing, nor does what is no ID.
		assert_eq!(partition_table("00000000"), None);
		assert_eq!(
			partition_table("00000000-0000-0000-0000-000000000000"),
			None
		);
		assert_eq!(partition_table("not an ID"), None);
		assert_eq!(partition_table("abc"), None);
	}

	#[test]
	#[cfg(any(target_os = "linux", windows))]
	fn folders_on_one_file_system_have_one_disk_name() {
		let crate_folder = Path::new(env!("CARGO_MANIFEST_DIR"));
		assert_eq!(
			disk_name(crate_folder),
			disk_name(&crate_folder.join("src"))
		);
	}

	#[test]
	fn serials_read_alike_wherever_they_are_written() {
		assert_eq!(
			parse_serial("B86A-4A5C"),
			Some(vec![0xB8, 0x6A, 0x4A, 0x5C])
		);
		assert_eq!(parse_serial("0086-4a5c"), Some(vec![0x86, 0x4A, 0x5C]));
		assert_eq!(parse_serial("0086-4a5c"), Some(numeric(0x0086_4A5C)));
		assert_eq!(
			parse_serial("01D9C2A41B3E5F70"),
			Some(numeric(0x01D9_C2A4_1B3E_5F70))
		);
		assert_eq!(
			parse_serial("7edd2919-49c7-46c0-a749-7af51e392e18").map(|uuid| uuid.len()),
			Some(16)
		);
		assert_eq!(parse_serial("not-hex"), None);
		assert_eq!(parse_serial(""), None);
	}

	#[test]
	fn a_path_is_the_same_on_every_system() {
		let composed = "Чайковский";
		let decomposed: String = composed.nfd().collect();
		assert_ne!(composed, decomposed);
		assert_eq!(
			path_on(
				&Path::new("/run/media/ada/STICK")
					.join(&decomposed)
					.join(".nib"),
				Path::new("/run/media/ada/STICK")
			),
			format!("{composed}/.nib")
		);
		assert_eq!(
			path_on(Path::new("/home/ada/.nib"), Path::new("/")),
			"home/ada/.nib"
		);
	}

	#[test]
	fn a_place_file_reads_as_it_was_written() {
		let mut rng = TestRng::new("place 1");
		for place in [
			PlaceFile::new(
				&at(
					"ada/.nib/replicas/a",
					Some(b"disk"),
					Some(b"id"),
					true,
					Some(b"desk"),
				),
				&mut rng,
			),
			PlaceFile::new(
				&Whereabouts {
					path: None,
					..at("", None, None, false, None)
				},
				&mut rng,
			),
		] {
			assert_eq!(
				cbor::from_slice::<PlaceFile>(&cbor::to_vec(&place)),
				Ok(place)
			);
		}
	}

	#[test]
	fn a_replica_is_here_on_its_drive_as_its_folder() {
		let mut rng = TestRng::new("place 2");
		let folder = Path::new("/nowhere/ada/.nib/replicas/a");
		let made = at(
			"ada/.nib/replicas/a",
			Some(b"disk"),
			Some(b"7"),
			false,
			Some(b"desk"),
		);
		let place = PlaceFile::new(&made, &mut rng);
		assert_eq!(place.verdict(&made, folder), Verdict::Here);
		// Another drive, or another folder on it: a copy.
		let other_drive = at(
			"ada/.nib/replicas/a",
			Some(b"other"),
			Some(b"7"),
			false,
			None,
		);
		assert_eq!(place.verdict(&other_drive, folder), Verdict::Copy);
		let beside = at(
			"ada/lib/.nib/replicas/a",
			Some(b"disk"),
			Some(b"8"),
			false,
			None,
		);
		assert_eq!(place.verdict(&beside, folder), Verdict::Copy);
		// The folder moved on its drive, or the drive into another computer: the same.
		let moved = at(
			"ada/elsewhere/.nib/replicas/a",
			Some(b"disk"),
			Some(b"7"),
			false,
			None,
		);
		assert_eq!(place.verdict(&moved, folder), Verdict::Here);
		let laptop = at(
			"ada/.nib/replicas/a",
			Some(b"disk"),
			Some(b"7"),
			false,
			Some(b"laptop"),
		);
		assert_eq!(place.verdict(&laptop, folder), Verdict::Here);
		// What one side could not read is not compared.
		let unread = at("ada/.nib/replicas/a", None, None, false, None);
		assert_eq!(place.verdict(&unread, folder), Verdict::Here);
	}

	#[test]
	fn a_fat_volumes_uuid_is_made_as_macos_makes_it() {
		// As a MacBook showed them for disk images of 64 MiB, 131,072 sectors.
		assert_eq!(
			fat_volume_uuid(0x6AE4_AA42, None),
			hex("8EF040E0885D3C1BB9D0D48A6456D147").expect("hex"),
			"exFAT"
		);
		assert_eq!(
			fat_volume_uuid(0x7F4E_1709, Some(131_072)),
			hex("8BDA5D182C62313A99BF54AEA17574CD").expect("hex"),
			"FAT"
		);
	}

	#[test]
	fn a_volume_uuid_tells_sticks_apart_where_no_serial_is_read() {
		let mut rng = TestRng::new("place 9");
		let folder = Path::new("/nowhere/stick/.nib/replicas/a");
		let on = |volume: &[u8], serial: Option<&[u8]>| Whereabouts {
			drive: Drive {
				volume: Some(volume.to_vec()),
				..at(".nib/replicas/a", serial, None, false, None).drive
			},
			..at(".nib/replicas/a", serial, None, false, None)
		};
		// Made on Linux, which reads both.
		let place = PlaceFile::new(&on(b"uuid", Some(b"serial")), &mut rng);
		// On a Mac, which reads the UUID alone.
		assert_eq!(place.verdict(&on(b"uuid", None), folder), Verdict::Here);
		assert_eq!(place.verdict(&on(b"another", None), folder), Verdict::Copy);
		// A place file written before the UUID was read, which has no last item.
		let Value::Array(mut items) = place.to_cbor() else {
			unreachable!("an array");
		};
		items.pop();
		let older = PlaceFile::from_cbor(&Value::Array(items)).expect("reads");
		assert_eq!(older.volume, None);
		assert_eq!(older.verdict(&on(b"another", None), folder), Verdict::Here);
	}

	#[test]
	fn on_the_systems_drive_the_computer_has_to_match() {
		let mut rng = TestRng::new("place 3");
		let folder = Path::new("/nowhere/home/ada/.nib/replicas/a");
		let path = "home/ada/.nib/replicas/a";
		let made = at(path, Some(b"root"), Some(b"7"), true, Some(b"desktop"));
		let place = PlaceFile::new(&made, &mut rng);
		assert_eq!(place.verdict(&made, folder), Verdict::Here);
		// A computer cloned from the same image.
		let clone = at(path, Some(b"root"), Some(b"7"), true, Some(b"clone"));
		assert_eq!(place.verdict(&clone, folder), Verdict::Copy);
		// The same drive, as a data drive of another computer.
		let data = at(path, Some(b"root"), Some(b"7"), false, Some(b"other"));
		assert_eq!(place.verdict(&data, folder), Verdict::Here);
	}

	#[test]
	fn a_stick_without_ids_of_folders_moves_between_computers() {
		let mut rng = TestRng::new("place 4");
		let folder = Path::new("/nowhere/stick/.nib/replicas/a");
		let made = at(
			".nib/replicas/a",
			Some(b"stick"),
			None,
			false,
			Some(b"desktop"),
		);
		let place = PlaceFile::new(&made, &mut rng);
		let laptop = at(
			".nib/replicas/a",
			Some(b"stick"),
			None,
			false,
			Some(b"laptop"),
		);
		assert_eq!(place.verdict(&laptop, folder), Verdict::Here);
		let another_stick = at(".nib/replicas/a", Some(b"another"), None, false, None);
		assert_eq!(place.verdict(&another_stick, folder), Verdict::Copy);
	}

	#[test]
	fn a_phone_compares_its_id_alone() {
		let mut rng = TestRng::new("place 5");
		let folder = Path::new("/nowhere/app/library/replicas/a");
		let phone = |id: &[u8], container: &str| Whereabouts {
			drive: Drive {
				system: true,
				..Drive::default()
			},
			mount: PathBuf::from(container),
			folder: None,
			path: None,
			computer: (!id.is_empty()).then(|| id.to_vec()),
			phone: true,
		};
		let place = PlaceFile::new(&phone(b"phone", "/var/one"), &mut rng);
		assert_eq!(
			place.verdict(&phone(b"phone", "/var/two"), folder),
			Verdict::Here
		);
		assert_eq!(
			place.verdict(&phone(b"other", "/var/one"), folder),
			Verdict::Copy
		);
		// A phone that cannot read its ID has a copy.
		assert_eq!(
			place.verdict(&phone(b"", "/var/one"), folder),
			Verdict::Copy
		);
	}

	#[test]
	fn a_place_that_records_nothing_but_a_path_is_here_wherever_the_path_is_but_on_a_computers_own_drive()
	 {
		let made = Whereabouts {
			mount: PathBuf::from("/one"),
			..at("replicas/a", None, None, false, None)
		};
		let place = PlaceFile::new(&made, &mut TestRng::new("place 6"));
		let folder = Path::new("/two/replicas/a");
		for now in [
			at("replicas/a", Some(b"disk"), Some(b"1"), false, None),
			at("replicas/a", None, None, false, None),
			// On the drive that a system runs from, where it reads no computer's ID, as iOS.
			at("replicas/a", None, None, true, None),
		] {
			assert_eq!(place.verdict(&now, folder), Verdict::Here);
		}
		let desktop = at(
			"replicas/a",
			Some(b"disk"),
			Some(b"1"),
			true,
			Some(b"desktop"),
		);
		assert_eq!(place.verdict(&desktop, folder), Verdict::Copy);
	}

	#[test]
	fn a_sticks_replica_copied_to_a_computers_own_drive_is_a_copy_where_no_serial_is_read() {
		let mut rng = TestRng::new("place 8");
		let folder = Path::new("/nowhere/Users/ada/copy/.nib/replicas/a");
		// Made on a stick whose serial this system does not read, as macOS.
		let made = at(".nib/replicas/a", None, None, false, Some(b"mac"));
		let place = PlaceFile::new(&made, &mut rng);
		// The stick again, on the same computer or another.
		assert_eq!(place.verdict(&made, folder), Verdict::Here);
		let other = at(".nib/replicas/a", None, None, false, Some(b"desktop"));
		assert_eq!(place.verdict(&other, folder), Verdict::Here);
		// Copied to the computer's own drive, where the path it recorded leads to nothing.
		let copied = at(
			"Users/ada/copy/.nib/replicas/a",
			None,
			Some(b"9"),
			true,
			Some(b"mac"),
		);
		assert_eq!(place.verdict(&copied, folder), Verdict::Copy);
	}

	#[test]
	fn without_ids_of_folders_one_moved_is_told_from_one_copied_beside_it() {
		let mut rng = TestRng::new("place 7");
		let root = std::env::temp_dir().join(format!("nibkeep-place-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		let first = root.join("first");
		let second = root.join("second");
		fs::create_dir_all(&first).unwrap();
		fs::create_dir_all(&second).unwrap();
		let now = |path: &str| Whereabouts {
			mount: root.clone(),
			..at(path, Some(b"stick"), None, false, None)
		};
		let place = PlaceFile::new(&now("first"), &mut rng);
		// The first folder is still there: the second is a copy of it.
		assert_eq!(place.verdict(&now("second"), &second), Verdict::Copy);
		// Once the first is gone, the second is the replica, moved.
		fs::remove_dir_all(&first).unwrap();
		assert_eq!(place.verdict(&now("second"), &second), Verdict::Here);
		fs::remove_dir_all(&root).unwrap();
	}

	#[cfg(any(target_os = "linux", target_os = "android"))]
	#[test]
	fn the_mount_table_names_the_mount_that_holds_a_folder() {
		let table = "\
33 2 0:30 /@ / rw,relatime shared:1 - btrfs /dev/mapper/root rw,subvol=/@
66 33 0:30 /@home /home rw,relatime shared:193 - btrfs /dev/mapper/root rw,subvol=/@home
70 33 8:17 / /run/media/ada/MY\\040STICK rw,nosuid shared:200 - exfat /dev/sdb1 rw
71 66 0:50 / /home/ada/remote rw master:1 - fuse.sshfs ada@nas: rw
72 66 0:51 / /home/ada/remote rw master:1 - tmpfs tmpfs rw
";
		let mounts = system::mounts(table);
		let of =
			|folder: &str| system::mount_of(&mounts, Path::new(folder)).map(|mount| &mount.point);
		assert_eq!(of("/home/ada/.nib"), Some(&PathBuf::from("/home")));
		assert_eq!(of("/etc"), Some(&PathBuf::from("/")));
		assert_eq!(
			of("/run/media/ada/MY STICK/.nib"),
			Some(&PathBuf::from("/run/media/ada/MY STICK"))
		);
		// Of two mounts at one folder, the later hides the earlier.
		let hiding = system::mount_of(&mounts, Path::new("/home/ada/remote/x")).unwrap();
		assert_eq!(hiding.kind, "tmpfs");
		assert_eq!(hiding.source, "tmpfs");
	}

	#[cfg(windows)]
	#[test]
	fn windows_reads_its_own_drive_and_the_computers_id() {
		let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("Windows' folder"));
		let here = whereabouts(&windows);
		assert!(here.drive.system, "{here:?}");
		assert!(here.drive.serial.is_some(), "{here:?}");
		assert!(here.drive.folder_ids && here.folder.is_some(), "{here:?}");
		assert!(
			!here.drive.remote && !here.drive.cloud && !here.drive.unjournaled,
			"{here:?}"
		);
		// The GUID that Windows draws when it is installed.
		assert_eq!(here.computer.as_ref().map(Vec::len), Some(16), "{here:?}");
		// Its drive, by the partition table that the drive's layout gives.
		let name = disk_name(&windows).expect("names the drive");
		assert!(
			name.starts_with(b"drive\0gpt\0") || name.starts_with(b"drive\0mbr\0"),
			"{name:?}"
		);
	}

	#[cfg(windows)]
	#[test]
	fn onedrives_folder_is_one_that_a_program_takes_to_the_cloud() {
		// OneDrive names its folder in the environment, once it is set up.
		let Some(onedrive) = std::env::var_os("OneDrive")
			.map(PathBuf::from)
			.filter(|folder| folder.is_dir())
		else {
			return;
		};
		assert!(whereabouts(&onedrive.join("Passwords")).drive.cloud);
		let home = onedrive.parent().expect("the home folder");
		assert!(!whereabouts(home).drive.cloud);
	}

	#[cfg(windows)]
	#[test]
	fn a_drive_that_is_not_there_is_not_taken_for_a_network_drive() {
		let Some(letter) = ('D'..='Z')
			.rev()
			.find(|letter| !Path::new(&format!(r"{letter}:\")).exists())
		else {
			return;
		};
		let here = whereabouts(&PathBuf::from(format!(r"{letter}:\Passwords\.nib")));
		assert!(!here.drive.remote, "{here:?}");
	}

	#[cfg(windows)]
	#[test]
	fn a_share_is_a_network_drive() {
		let here = whereabouts(Path::new(r"\\localhost\C$\Windows"));
		assert!(here.drive.remote, "{here:?}");
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn the_home_folder_is_on_the_macs_own_drive() {
		let home = PathBuf::from(std::env::var_os("HOME").expect("a home folder"));
		let here = whereabouts(&home);
		assert!(here.drive.system, "{here:?}");
		assert_eq!(here.mount, Path::new("/"));
		assert!(here.drive.folder_ids && here.folder.is_some(), "{here:?}");
		assert!(!here.drive.remote && !here.drive.unjournaled, "{here:?}");
		assert_eq!(here.computer.as_ref().map(Vec::len), Some(16), "{here:?}");
		// A folder still to be made is on the drive of the folder above it.
		let new = whereabouts(&home.join("not made yet").join(".nib"));
		assert_eq!(new.mount, Path::new("/"));
		assert!(new.drive.system && !new.drive.remote, "{new:?}");
	}
}
