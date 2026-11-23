//! The sealed blobs of a replica, each in a file of its own, in one directory that its spaces
//! share, so that a replica keeps one sealed copy of each content
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#79-blobs)). Each space writes
//! new blobs through a temporary directory of its own, so that opening a space finishes or
//! deletes only what it was writing. A file goes only once no space of the replica keeps a
//! key for it: deleting one, and a space's taking up a copy that another space holds, take
//! the replica's blob lock, so that neither misses the other. A copy that a check found
//! damaged is moved aside, out of the blob directory, until an intact one takes its place
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#97-checking-and-repair)).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nibkeep_base::{Hash, SpaceId};

use crate::file::{blob_keys_in, kept_blobs_in};

/// The name of the directory of a replica's sealed blobs.
const BLOBS: &str = "blobs";

/// The directory of the files that a space is writing, inside its own directory.
const TEMPORARY: &str = "tmp";

/// The replica's blob lock, beside its blob directory.
const LOCK: &str = "blobs.lock";

/// The directory, beside the blob directory, of the copies that checks found damaged.
const DAMAGED: &str = "damaged";

/// The directory, beside the blob directory, of the sealed blobs whose keys a purge destroyed,
/// which a replica that waits keeps apart until its wait ends
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait)).
const KEPT: &str = "kept";

/// How many bytes of a file a check reads at a time.
const CHECK_CHUNK: usize = 1024 * 1024;

/// Counts temporary files, so that every one is new.
static TEMPORARY_FILES: AtomicU64 = AtomicU64::new(0);

/// The directory that holds a replica's sealed blobs, as one of its spaces sees it.
pub(crate) struct BlobFiles {
	/// The replica's blob directory: `blobs` in its folder.
	directory: PathBuf,
	/// The space's directory of files being written: `tmp` in its folder.
	temporary: PathBuf,
	/// The directory that holds the replica's spaces, each in a directory named by its ID.
	spaces: PathBuf,
	/// The space.
	space: SpaceId,
}

/// The replica's blob lock, held until it goes.
pub(crate) struct BlobLock(File);

impl Drop for BlobLock {
	fn drop(&mut self) {
		let _ = self.0.unlock();
	}
}

impl BlobFiles {
	/// The blob files of the space whose log file is `log`, at `spaces/<space>/log.sqlite`
	/// in the replica's folder.
	pub(crate) fn of_log(log: &Path, space: SpaceId) -> Self {
		let own = log.parent().unwrap_or(log);
		let spaces = own.parent().unwrap_or(own);
		Self {
			directory: blob_directory(replica_of_log(log)),
			temporary: own.join(TEMPORARY),
			spaces: spaces.to_owned(),
			space,
		}
	}

	/// Where the sealed blob with this sealed hash is kept: `<algorithm>/<xx>/<digest>`.
	pub(crate) fn path(&self, sealed: &Hash) -> PathBuf {
		place_in(&self.directory, sealed)
	}

	/// Whether the replica holds the sealed blob.
	pub(crate) fn holds(&self, sealed: &Hash) -> bool {
		self.path(sealed).is_file()
	}

	/// Opens the file of a sealed blob.
	pub(crate) fn open(&self, sealed: &Hash) -> io::Result<File> {
		File::open(self.path(sealed))
	}

	/// Where a damaged copy of the sealed blob is kept aside: `damaged/<algorithm>/<digest>`
	/// in the replica's folder.
	fn aside(&self, sealed: &Hash) -> PathBuf {
		let replica = self.directory.parent().unwrap_or(&self.directory);
		replica
			.join(DAMAGED)
			.join(sealed.algorithm().name())
			.join(hex(sealed))
	}

	/// Whether a damaged copy of the sealed blob is kept aside.
	pub(crate) fn is_aside(&self, sealed: &Hash) -> bool {
		self.aside(sealed).is_file()
	}

	/// Where a sealed blob whose key a purge destroyed is kept apart while the replica waits:
	/// `kept/<algorithm>/<digest>` in the replica's folder.
	fn kept_path(&self, sealed: &Hash) -> PathBuf {
		let replica = self.directory.parent().unwrap_or(&self.directory);
		replica
			.join(KEPT)
			.join(sealed.algorithm().name())
			.join(hex(sealed))
	}

	/// Whether the replica keeps a sealed blob apart, as a purge destroyed its key.
	pub(crate) fn is_kept(&self, sealed: &Hash) -> bool {
		self.kept_path(sealed).is_file()
	}

	/// Moves the files of sealed blobs whose keys a purge destroyed out of the blob
	/// directory, to be kept apart while the replica waits, under the blob lock: those that
	/// no other space of the replica keeps a key for, which stay where they are. A damaged
	/// copy kept aside goes. A file that cannot be moved now stays, for the purge to delete as
	/// it would have.
	pub(crate) fn keep_apart(&self, sealed: &[Hash]) -> io::Result<()> {
		let held: Vec<Hash> = sealed
			.iter()
			.copied()
			.filter(|sealed| self.holds(sealed) || self.is_aside(sealed))
			.collect();
		if held.is_empty() {
			return Ok(());
		}
		let _lock = self.lock()?;
		let elsewhere = self.kept_elsewhere(&held);
		let mut first = None;
		for sealed in held.iter().filter(|sealed| !elsewhere.contains(sealed)) {
			let path = self.path(sealed);
			let kept = self.kept_path(sealed);
			let moved = (|| {
				if path.is_file() {
					let directory = kept.parent().unwrap_or(&self.directory);
					make_directory(directory)?;
					fs::rename(&path, &kept)?;
					sync_directory(directory);
					if let Some(from) = path.parent() {
						sync_directory(from);
					}
				}
				ignore_missing(fs::remove_file(self.aside(sealed)))
			})();
			if let Err(error) = moved {
				first.get_or_insert(error);
			}
		}
		first.map_or(Ok(()), Err)
	}

	/// Puts a sealed blob kept apart back in place, under the blob lock, as a change restored
	/// its key; a copy the replica holds again takes its place. Returns whether one is in
	/// place now.
	pub(crate) fn take_back(&self, sealed: &Hash) -> io::Result<bool> {
		let _lock = self.lock()?;
		let kept = self.kept_path(sealed);
		if !kept.is_file() {
			return Ok(self.holds(sealed));
		}
		if self.holds(sealed) {
			fs::remove_file(&kept)?;
			return Ok(true);
		}
		let path = self.path(sealed);
		let directory = path.parent().unwrap_or(&self.directory);
		make_directory(directory)?;
		fs::rename(&kept, &path)?;
		sync_directory(directory);
		Ok(true)
	}

	/// Deletes the files of these sealed blobs kept apart, as the wait for the purges that
	/// destroyed their keys ended, under the blob lock: but those that another space of the
	/// replica keeps apart too, as its log file says, and all of them if one cannot be read.
	/// Returns those whose files went.
	pub(crate) fn destroy_kept(&self, sealed: &[Hash]) -> io::Result<Vec<Hash>> {
		let held: Vec<Hash> = sealed
			.iter()
			.copied()
			.filter(|sealed| self.is_kept(sealed))
			.collect();
		if held.is_empty() {
			return Ok(Vec::new());
		}
		let _lock = self.lock()?;
		let elsewhere = self.kept_apart_elsewhere(&held);
		let mut destroyed = Vec::new();
		let mut first = None;
		for sealed in held.iter().filter(|sealed| !elsewhere.contains(sealed)) {
			match ignore_missing(fs::remove_file(self.kept_path(sealed))) {
				Ok(()) => destroyed.push(*sealed),
				Err(error) => {
					first.get_or_insert(error);
				}
			}
		}
		match first {
			Some(error) if destroyed.is_empty() => Err(error),
			_ => Ok(destroyed),
		}
	}

	/// The sealed blobs kept apart in the replica's folder, as purges destroyed their keys.
	pub(crate) fn kept_files(&self) -> Vec<Hash> {
		let replica = self.directory.parent().unwrap_or(&self.directory);
		let mut found = Vec::new();
		for algorithm in entries(&replica.join(KEPT)).unwrap_or_default() {
			let Some(name) = algorithm.file_name().and_then(|name| name.to_str()) else {
				continue;
			};
			let name = name.to_owned();
			for path in entries(&algorithm).unwrap_or_default() {
				if let Some(sealed) = path
					.file_name()
					.and_then(|digest| digest.to_str())
					.and_then(|digest| format!("{name}:{digest}").parse::<Hash>().ok())
					.filter(|sealed| self.kept_path(sealed) == path && path.is_file())
				{
					found.push(sealed);
				}
			}
		}
		found.sort_unstable();
		found
	}

	/// The size of a sealed blob's file kept apart, if it is.
	pub(crate) fn kept_size(&self, sealed: &Hash) -> Option<u64> {
		fs::metadata(self.kept_path(sealed))
			.ok()
			.map(|metadata| metadata.len())
	}

	/// Of these sealed blobs, those that another space of the replica keeps apart, as its log
	/// file says now; all of them if one cannot be read.
	fn kept_apart_elsewhere(&self, sealed: &[Hash]) -> BTreeSet<Hash> {
		let mut kept = BTreeSet::new();
		let own = self.space.to_string();
		let Ok(entries) = fs::read_dir(&self.spaces) else {
			return sealed.iter().copied().collect();
		};
		for entry in entries {
			let Ok(entry) = entry else {
				return sealed.iter().copied().collect();
			};
			if entry.file_name().to_str() == Some(own.as_str()) {
				continue;
			}
			let log = entry.path().join("log.sqlite");
			if !log.is_file() {
				continue;
			}
			match kept_blobs_in(&log, sealed) {
				Ok(found) => kept.extend(found),
				Err(_) => return sealed.iter().copied().collect(),
			}
		}
		kept
	}

	/// Moves the replica's copy of a sealed blob aside, as a check found it damaged, under the
	/// blob lock, replacing a damaged copy kept aside before. Returns whether it did: not if
	/// the replica no longer holds it, as when a purge deleted it meanwhile.
	pub(crate) fn set_aside(&self, sealed: &Hash) -> io::Result<bool> {
		let _lock = self.lock()?;
		let path = self.path(sealed);
		if !path.is_file() {
			return Ok(false);
		}
		let aside = self.aside(sealed);
		let directory = aside.parent().unwrap_or(&self.directory);
		make_directory(directory)?;
		fs::rename(&path, &aside)?;
		sync_directory(directory);
		if let Some(from) = path.parent() {
			sync_directory(from);
		}
		Ok(true)
	}

	/// Reads the file of a sealed blob in full, `stop` asked between chunks: its BLAKE3 hash
	/// and size, or `None` if `stop` said to stop first.
	pub(crate) fn hash_file(
		&self,
		sealed: &Hash,
		stop: &dyn Fn() -> bool,
	) -> io::Result<Option<(Hash, u64)>> {
		hash_file(&self.path(sealed), stop)
	}

	/// The sealed blobs of which a damaged copy is kept aside.
	pub(crate) fn aside_files(&self) -> Vec<Hash> {
		let replica = self.directory.parent().unwrap_or(&self.directory);
		let mut found = Vec::new();
		for algorithm in entries(&replica.join(DAMAGED)).unwrap_or_default() {
			let Some(name) = algorithm.file_name().and_then(|name| name.to_str()) else {
				continue;
			};
			let name = name.to_owned();
			for path in entries(&algorithm).unwrap_or_default() {
				if let Some(sealed) = path
					.file_name()
					.and_then(|digest| digest.to_str())
					.and_then(|digest| format!("{name}:{digest}").parse::<Hash>().ok())
					.filter(|sealed| self.aside(sealed) == path && path.is_file())
				{
					found.push(sealed);
				}
			}
		}
		found.sort_unstable();
		found
	}

	/// Reads the damaged copy of a sealed blob kept aside in full, `stop` asked between
	/// chunks: its BLAKE3 hash and size, or `None` if `stop` said to stop first.
	pub(crate) fn hash_aside(
		&self,
		sealed: &Hash,
		stop: &dyn Fn() -> bool,
	) -> io::Result<Option<(Hash, u64)>> {
		hash_file(&self.aside(sealed), stop)
	}

	/// Puts a copy kept aside back in place, under the blob lock, as a check found it intact
	/// after all, unless the replica holds the blob again. Returns whether it did.
	pub(crate) fn restore(&self, sealed: &Hash) -> io::Result<bool> {
		let _lock = self.lock()?;
		let aside = self.aside(sealed);
		if self.holds(sealed) || !aside.is_file() {
			return Ok(false);
		}
		let path = self.path(sealed);
		let directory = path.parent().unwrap_or(&self.directory);
		make_directory(directory)?;
		fs::rename(&aside, &path)?;
		sync_directory(directory);
		Ok(true)
	}

	/// Creates a new file for a sealed blob that the space is writing.
	pub(crate) fn temporary(&self) -> io::Result<(PathBuf, File)> {
		make_directory(&self.temporary)?;
		let path = self.temporary.join(format!(
			"{}-{}.tmp",
			std::process::id(),
			TEMPORARY_FILES.fetch_add(1, Ordering::Relaxed)
		));
		let file = OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(&path)?;
		Ok((path, file))
	}

	/// Renames a written and synced file, among the files the space is writing, after the
	/// sealed hash of the blob it holds, so that opening the space can put it in place if a
	/// crash comes first. Prepares the directory it goes to. Returns its new path.
	pub(crate) fn stage(&self, written: &Path, sealed: &Hash) -> io::Result<PathBuf> {
		let staged = self.temporary.join(staged_name(sealed));
		fs::rename(written, &staged)?;
		// The staged name must outlast a power cut, since the key is saved next.
		sync_directory(&self.temporary);
		if let Some(directory) = self.path(sealed).parent() {
			make_directory(directory)?;
		}
		Ok(staged)
	}

	/// Puts a staged file in place as the sealed blob with this hash, and deletes a damaged
	/// copy of it kept aside, which it replaces.
	pub(crate) fn place(&self, staged: &Path, sealed: &Hash) -> io::Result<()> {
		let path = self.path(sealed);
		let directory = path.parent().unwrap_or(&self.directory);
		make_directory(directory)?;
		fs::rename(staged, &path)?;
		sync_directory(directory);
		let _ = fs::remove_file(self.aside(sealed));
		Ok(())
	}

	/// The files the space was writing when it was last open, each with the sealed hash it
	/// was staged under, if it was.
	pub(crate) fn temporary_files(&self) -> io::Result<Vec<(PathBuf, Option<Hash>)>> {
		staged_files(&self.temporary)
	}

	/// Every file in the replica's blob directory: as the sealed hash its path names, or as
	/// its path if it names none.
	pub(crate) fn files(&self) -> io::Result<Vec<Result<Hash, PathBuf>>> {
		files_in(&self.directory)
	}

	/// Takes the replica's blob lock, waiting for another stored log, in this program or
	/// another, that holds it.
	pub(crate) fn lock(&self) -> io::Result<BlobLock> {
		let parent = self.directory.parent().unwrap_or(&self.directory);
		make_directory(parent)?;
		let file = OpenOptions::new()
			.read(true)
			.write(true)
			.create(true)
			.truncate(false)
			.open(parent.join(LOCK))?;
		file.lock()?;
		Ok(BlobLock(file))
	}

	/// Deletes the files of sealed blobs that this space no longer keeps, once no other
	/// space of the replica keeps a key for them, under the blob lock. A file that another
	/// space may still need, as when its log file cannot be read, stays.
	pub(crate) fn release(&self, sealed: &[Hash]) -> io::Result<()> {
		let held: Vec<Hash> = sealed
			.iter()
			.copied()
			.filter(|sealed| self.holds(sealed) || self.is_aside(sealed))
			.collect();
		match self.delete(&held, true)?.1 {
			Some(error) => Err(error),
			None => Ok(()),
		}
	}

	/// Removes the replica's copies of these sealed blobs, which the space no longer keeps
	/// though it keeps their keys, as the planner removes a copy that other disks hold, under
	/// the blob lock. One that another space of the replica keeps a key for stays, as that
	/// space may keep it; so does a damaged copy kept aside, and one whose file cannot be
	/// removed now, as while another program has it open on Windows. Returns those removed.
	pub(crate) fn remove(&self, sealed: &[Hash]) -> io::Result<Vec<Hash>> {
		let held: Vec<Hash> = sealed
			.iter()
			.copied()
			.filter(|sealed| self.holds(sealed))
			.collect();
		Ok(self.delete(&held, false)?.0)
	}

	/// Deletes the files of these sealed blobs, and the damaged copies kept aside of them too
	/// if `aside` says so, under the blob lock, but those that another space of the replica
	/// keeps a key for. Returns those whose files went, and the first error, after which it
	/// goes on with the others.
	fn delete(&self, held: &[Hash], aside: bool) -> io::Result<(Vec<Hash>, Option<io::Error>)> {
		if held.is_empty() {
			return Ok((Vec::new(), None));
		}
		let _lock = self.lock()?;
		let kept = self.kept_elsewhere(held);
		let (mut deleted, mut failed) = (Vec::new(), None);
		let mut directories = BTreeSet::new();
		for sealed in held.iter().filter(|sealed| !kept.contains(sealed)) {
			let path = self.path(sealed);
			let mut result = ignore_missing(fs::remove_file(&path));
			if aside && result.is_ok() {
				result = ignore_missing(fs::remove_file(self.aside(sealed)));
			}
			match result {
				Ok(()) => {
					if let Some(parent) = path.parent() {
						directories.insert(parent.to_owned());
					}
					deleted.push(*sealed);
				}
				Err(error) => {
					failed.get_or_insert(error);
				}
			}
		}
		for directory in directories {
			sync_directory(&directory);
		}
		Ok((deleted, failed))
	}

	/// Of these sealed blobs, those that another space of the replica keeps a key for, as
	/// its log file says now; all of those whose space's log file cannot be read.
	pub(crate) fn kept_elsewhere(&self, sealed: &[Hash]) -> BTreeSet<Hash> {
		let mut kept = BTreeSet::new();
		let own = self.space.to_string();
		let Ok(entries) = fs::read_dir(&self.spaces) else {
			return sealed.iter().copied().collect();
		};
		for entry in entries {
			let Ok(entry) = entry else {
				return sealed.iter().copied().collect();
			};
			if entry.file_name().to_str() == Some(own.as_str()) {
				continue;
			}
			let log = entry.path().join("log.sqlite");
			if !log.is_file() {
				continue;
			}
			match blob_keys_in(&log, sealed) {
				Ok(found) => kept.extend(found),
				Err(_) => return sealed.iter().copied().collect(),
			}
		}
		kept
	}
}

/// Where a sealed blob is kept in a blob directory: `<algorithm>/<xx>/<digest>`.
#[must_use]
pub fn place_in(directory: &Path, sealed: &Hash) -> PathBuf {
	let digest = hex(sealed);
	directory
		.join(sealed.algorithm().name())
		.join(&digest[..2])
		.join(digest)
}

/// The directory of a replica's sealed blobs, in the replica's folder `replica`.
#[must_use]
pub fn blob_directory(replica: &Path) -> PathBuf {
	replica.join(BLOBS)
}

/// Reads a file in full, `stop` asked between chunks of a mebibyte: its BLAKE3 hash and size,
/// or `None` if `stop` said to stop first.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn hash_file(path: &Path, stop: &dyn Fn() -> bool) -> io::Result<Option<(Hash, u64)>> {
	let mut file = File::open(path)?;
	let mut hasher = nibkeep_base::Hasher::new();
	let mut buffer = vec![0; CHECK_CHUNK];
	let mut size = 0_u64;
	loop {
		if stop() {
			return Ok(None);
		}
		let count = io::Read::read(&mut file, &mut buffer)?;
		if count == 0 {
			return Ok(Some((hasher.finalize(), size)));
		}
		hasher.update(&buffer[..count]);
		size += count as u64;
	}
}

/// The sealed blobs in the blob directory of the replica whose folder is `replica`, of the
/// algorithm of `like`, whose digests start with `byte`: sorted, each with its size in bytes,
/// as a regular check goes through them in order.
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub fn blobs_starting_with(replica: &Path, like: &Hash, byte: u8) -> io::Result<Vec<(Hash, u64)>> {
	let algorithm = like.algorithm().name();
	let directory = blob_directory(replica);
	let mut found = Vec::new();
	for path in entries(&directory.join(algorithm).join(format!("{byte:02x}")))? {
		let Some(sealed) = path
			.file_name()
			.and_then(|digest| digest.to_str())
			.and_then(|digest| format!("{algorithm}:{digest}").parse::<Hash>().ok())
			.filter(|sealed| place_in(&directory, sealed) == path)
		else {
			continue;
		};
		match fs::metadata(&path) {
			Ok(metadata) if metadata.is_file() => found.push((sealed, metadata.len())),
			Ok(_) => {}
			Err(error) if error.kind() == io::ErrorKind::NotFound => {}
			Err(error) => return Err(error),
		}
	}
	found.sort_unstable();
	Ok(found)
}

/// The replica's folder that holds a space's log file, at `spaces/<space>/log.sqlite` in it.
pub(crate) fn replica_of_log(log: &Path) -> &Path {
	let own = log.parent().unwrap_or(log);
	let spaces = own.parent().unwrap_or(own);
	spaces.parent().unwrap_or(spaces)
}

/// Makes a directory and those above it that are missing, and syncs each that it made into
/// the one above, so that what is renamed into it outlasts a power cut, as a file moved there
/// from where it was would not otherwise, where the file system allows it.
fn make_directory(directory: &Path) -> io::Result<()> {
	if directory.is_dir() {
		return Ok(());
	}
	let mut missing = vec![directory];
	while let Some(parent) = missing.last().and_then(|last| last.parent()) {
		if parent.as_os_str().is_empty() || parent.is_dir() {
			break;
		}
		missing.push(parent);
	}
	fs::create_dir_all(directory)?;
	for made in missing.into_iter().rev() {
		if let Some(parent) = made.parent() {
			sync_directory(parent);
		}
	}
	Ok(())
}

/// The name of a staged file: `<algorithm>-<digest>`.
fn staged_name(sealed: &Hash) -> String {
	format!("{}-{}", sealed.algorithm().name(), hex(sealed))
}

/// The files in a directory of files being written, each with the sealed hash it was staged
/// under, if it was.
fn staged_files(directory: &Path) -> io::Result<Vec<(PathBuf, Option<Hash>)>> {
	let entries = match fs::read_dir(directory) {
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		entries => entries?,
	};
	let mut files = Vec::new();
	for entry in entries {
		let path = entry?.path();
		let staged = path
			.file_name()
			.and_then(|name| name.to_str())
			.and_then(|name| name.replacen('-', ":", 1).parse().ok());
		files.push((path, staged));
	}
	Ok(files)
}

/// Every file in a blob directory: as the sealed hash its path names, or as its path if it
/// names none.
fn files_in(directory: &Path) -> io::Result<Vec<Result<Hash, PathBuf>>> {
	let mut files = Vec::new();
	for algorithm in entries(directory)? {
		let Some(name) = algorithm.file_name().and_then(|name| name.to_str()) else {
			files.push(Err(algorithm));
			continue;
		};
		if !algorithm.is_dir() {
			files.push(Err(algorithm));
			continue;
		}
		let name = name.to_owned();
		for prefix in entries(&algorithm)? {
			if !prefix.is_dir() {
				files.push(Err(prefix));
				continue;
			}
			for path in entries(&prefix)? {
				let sealed = path
					.file_name()
					.and_then(|digest| digest.to_str())
					.and_then(|digest| format!("{name}:{digest}").parse::<Hash>().ok())
					.filter(|sealed| path.is_file() && place_in(directory, sealed) == path);
				files.push(sealed.ok_or(path));
			}
		}
	}
	Ok(files)
}

/// The digest of a hash in lowercase hexadecimal.
fn hex(hash: &Hash) -> String {
	hash.digest().iter().fold(String::new(), |mut hex, byte| {
		let _ = write!(hex, "{byte:02x}");
		hex
	})
}

/// The paths in a directory, or none if it does not exist.
fn entries(directory: &Path) -> io::Result<Vec<PathBuf>> {
	let entries = match fs::read_dir(directory) {
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
		entries => entries?,
	};
	entries.map(|entry| Ok(entry?.path())).collect()
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
	match result {
		Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
		other => other,
	}
}

/// Syncs a directory, so that a file renamed into it survives a power cut, where the file
/// system allows it. The rename is in place either way.
fn sync_directory(directory: &Path) {
	#[cfg(unix)]
	{
		let _ = File::open(directory).and_then(|directory| directory.sync_all());
	}
	#[cfg(not(unix))]
	let _ = directory;
}
