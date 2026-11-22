//! Sealed blobs on disk: stored once, read back, checked, damaged, removed and purged; those
//! of secret fields, which only the secrets open; and one sealed copy of a content that
//! several spaces of a replica hold.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use nibkeep_base::cbor;
use nibkeep_base::{Hash, SpaceId};
use nibkeep_crypto::{BLOB_CHUNK_LEN, SigningKey};
use nibkeep_log::{Log, LogError, SpaceKeys, WrappedBlobKey};
use nibkeep_store::{SealedCheck, StoreError, StoredLog, blob_directory, log_path};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const DRIFT: u64 = 3_600_000;

fn start() -> u64 {
	millis_at("2026-10-12", 10, 0)
}

/// An empty directory for one test.
fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

/// A device with a space's key, and the space's log on its disk.
struct Device {
	rng: TestRng,
	keys: SpaceKeys,
	author: SigningKey,
	space: SpaceId,
	/// The replica's folder.
	root: PathBuf,
	path: PathBuf,
}

impl Device {
	/// A device of a new space.
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let space = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		Self::of(name, space, keys, rng)
	}

	/// Another device of the same space.
	fn another(&self, name: &str) -> Self {
		Self::of(name, self.space, self.keys.clone(), TestRng::new(name))
	}

	/// Another space of the same replica, with a key of its own.
	fn other_space(&self, name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let space = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		Self {
			author: rng.signing_key(),
			rng,
			keys,
			space,
			root: self.root.clone(),
			path: log_path(&self.root, space),
		}
	}

	fn of(name: &str, space: SpaceId, keys: SpaceKeys, mut rng: TestRng) -> Self {
		let root = directory(name);
		Self {
			author: rng.signing_key(),
			rng,
			keys,
			space,
			path: log_path(&root, space),
			root,
		}
	}

	fn open(&self) -> StoredLog {
		StoredLog::open(
			&self.path,
			self.space,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[],
		)
		.expect("opens")
	}

	fn import(&mut self, stored: &mut StoredLog, content: &[u8]) -> Hash {
		stored
			.import_blob(&self.keys, content, start(), &mut self.rng)
			.expect("imports")
	}

	/// The space's keys with a secrets key, as a device that holds the secrets has them while
	/// they are unlocked.
	fn secrets(&self) -> SpaceKeys {
		let mut keys = self.keys.clone();
		keys.insert_secrets(0, TestRng::new("the secrets key").key());
		keys
	}

	fn import_secret(&mut self, stored: &mut StoredLog, content: &[u8]) -> Hash {
		stored
			.import_secret_blob(&self.secrets(), content, start(), &mut self.rng)
			.expect("imports")
	}

	/// Imports content into this space, taking up a copy that `other`, another space of
	/// the replica, holds, if it can.
	fn import_sharing(
		&mut self,
		stored: &mut StoredLog,
		other: &StoredLog,
		content: &[u8],
	) -> Hash {
		stored
			.import_blob_sharing(
				&self.keys,
				(content, false),
				&|content| other.held_copies(content, false),
				start(),
				&mut self.rng,
			)
			.expect("imports")
	}

	/// Gives another device of the space the wrapped keys and the sealed blobs of this one,
	/// as sync would.
	fn send(&self, from: &StoredLog, to: &mut StoredLog, other: &Self) {
		let wrapped: Vec<(Hash, WrappedBlobKey)> = from
			.log()
			.wrapped_blob_keys()
			.map(|(sealed, wrapped)| (*sealed, wrapped.clone()))
			.collect();
		for (sealed, wrapped) in wrapped {
			to.receive_blob_key(sealed, wrapped, &other.keys)
				.expect("keeps the key");
		}
		for file in self.blob_files() {
			let target = other.blobs().join(&file);
			fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
			fs::copy(self.blobs().join(&file), target).expect("copies");
		}
	}

	/// The directory that holds the replica's sealed blobs, which its spaces share.
	fn blobs(&self) -> PathBuf {
		blob_directory(&self.root)
	}

	/// The directory of the files that the space is writing.
	fn temporary(&self) -> PathBuf {
		self.path.with_file_name("tmp")
	}

	/// The files under the blob directory, as paths relative to it.
	fn blob_files(&self) -> Vec<PathBuf> {
		let mut files = Vec::new();
		let mut directories = vec![self.blobs()];
		while let Some(directory) = directories.pop() {
			let Ok(entries) = fs::read_dir(&directory) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("an entry").path();
				if path.is_dir() {
					directories.push(path);
				} else {
					files.push(path.strip_prefix(self.blobs()).expect("inside").to_owned());
				}
			}
		}
		files.sort();
		files
	}

	/// Everything in the space's database files.
	fn database_bytes(&self) -> Vec<u8> {
		let mut bytes = fs::read(&self.path).expect("reads the database");
		bytes.extend(fs::read(self.path.with_extension("sqlite-wal")).unwrap_or_default());
		bytes
	}
}

/// Content whose bytes differ from chunk to chunk.
fn content(len: usize) -> Vec<u8> {
	(0..len)
		.map(|index| u8::try_from(index % 251).expect("below 251"))
		.collect()
}

fn read(stored: &StoredLog, content: &Hash) -> Vec<u8> {
	let mut read = Vec::new();
	stored
		.open_blob(content)
		.expect("opens")
		.read_to_end(&mut read)
		.expect("reads");
	read
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().fold(String::new(), |mut hex, byte| {
		let _ = write!(hex, "{byte:02x}");
		hex
	})
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
	haystack
		.windows(needle.len())
		.any(|window| window == needle)
}

#[test]
fn blobs_survive_a_restart() {
	let mut device = Device::new("blobs survive a restart");
	let photo = content(2 * BLOB_CHUNK_LEN + 100);
	let hash = {
		let mut stored = device.open();
		let hash = device.import(&mut stored, &photo);
		assert_eq!(hash, Hash::of(&photo));
		assert!(stored.holds_blob(&hash));
		assert_eq!(read(&stored, &hash), photo);
		hash
	};
	let stored = device.open();
	assert!(stored.holds_blob(&hash));
	assert_eq!(read(&stored, &hash), photo);

	let mut reader = stored.open_blob(&hash).expect("opens");
	reader
		.seek(SeekFrom::Start(
			u64::try_from(BLOB_CHUNK_LEN + 3).expect("fits"),
		))
		.expect("seeks");
	let mut part = [0; 5];
	reader.read_exact(&mut part).expect("reads");
	assert_eq!(part, photo[BLOB_CHUNK_LEN + 3..BLOB_CHUNK_LEN + 8]);

	let unknown = Hash::of(b"never imported");
	assert!(!stored.holds_blob(&unknown));
	assert!(matches!(
		stored.open_blob(&unknown),
		Err(StoreError::MissingBlob)
	));
}

#[test]
fn identical_content_is_stored_once() {
	let mut device = Device::new("identical content is stored once");
	let mut stored = device.open();
	let photo = device.import(&mut stored, b"a photo");
	assert_eq!(device.import(&mut stored, b"a photo"), photo);
	assert_eq!(device.blob_files().len(), 1);
	let scan = device.import(&mut stored, b"a scan");
	assert_eq!(device.blob_files().len(), 2);

	// A copy that the device no longer needs goes, and the key stays.
	stored.remove_blob(&photo).expect("removes");
	assert!(!stored.holds_blob(&photo));
	assert!(stored.holds_blob(&scan));
	assert_eq!(device.blob_files().len(), 1);
	assert!(matches!(
		stored.open_blob(&photo),
		Err(StoreError::MissingBlob)
	));
	assert_eq!(stored.log().blob_keys(&photo).count(), 1);
	device.import(&mut stored, b"a photo");
	assert!(stored.holds_blob(&photo));
	assert_eq!(read(&stored, &photo), b"a photo");
	assert_eq!(device.blob_files().len(), 2);
}

#[test]
fn files_on_disk_reveal_nothing_of_the_content() {
	let mut device = Device::new("files reveal nothing");
	let photo = content(BLOB_CHUNK_LEN + 10);
	let hash = {
		let mut stored = device.open();
		device.import(&mut stored, &photo)
	};
	let files = device.blob_files();
	assert_eq!(files.len(), 1);
	let file = &files[0];
	let sealed = fs::read(device.blobs().join(file)).expect("reads the blob");
	let digest = hex(Hash::of(&sealed).digest());
	assert_eq!(
		file,
		&Path::new("blake3").join(&digest[..2]).join(&digest),
		"named by the hash of the sealed blob"
	);
	assert!(!file.to_string_lossy().contains(&hex(hash.digest())));
	assert!(!contains(&sealed, &photo[1000..1064]));
	assert!(!contains(&device.database_bytes(), &hash.to_multihash()));
}

#[test]
fn a_damaged_blob_is_detected() {
	let mut device = Device::new("a damaged blob");
	let photo = content(2 * BLOB_CHUNK_LEN + 100);
	let mut stored = device.open();
	let hash = device.import(&mut stored, &photo);
	let file = device.blobs().join(&device.blob_files()[0]);
	let mut sealed = fs::read(&file).expect("reads");
	sealed[BLOB_CHUNK_LEN + 50] ^= 1;
	fs::write(&file, &sealed).expect("damages");

	let mut read = Vec::new();
	let error = stored
		.open_blob(&hash)
		.expect("opens")
		.read_to_end(&mut read)
		.expect_err("damaged");
	assert_eq!(error.kind(), ErrorKind::InvalidData);
	let later = start() + 60_000;
	assert!(matches!(
		stored.verify_blob(&hash, later),
		Err(StoreError::DamagedBlob)
	));
	assert_eq!(stored.blob_verified(&hash).expect("reads"), Some(start()));

	sealed.truncate(10);
	fs::write(&file, &sealed).expect("cuts short");
	assert!(matches!(
		stored.open_blob(&hash),
		Err(StoreError::DamagedBlob)
	));
}

#[test]
fn checks_of_blobs_are_recorded() {
	let mut device = Device::new("checks are recorded");
	let later = start() + 3_600_000;
	let hash = {
		let mut stored = device.open();
		let hash = device.import(&mut stored, b"a photo");
		assert_eq!(stored.blob_verified(&hash).expect("reads"), Some(start()));
		stored.verify_blob(&hash, later).expect("verifies");
		hash
	};
	let mut stored = device.open();
	assert_eq!(stored.blob_verified(&hash).expect("reads"), Some(later));
	stored.remove_blob(&hash).expect("removes");
	assert_eq!(stored.blob_verified(&hash).expect("reads"), None);
	assert!(matches!(
		stored.verify_blob(&hash, later),
		Err(StoreError::MissingBlob)
	));
}

#[test]
fn purged_blobs_leave_no_trace_on_disk() {
	let mut phone = Device::new("purge on the phone");
	let laptop = phone.another("purge on the laptop");
	let photo = content(BLOB_CHUNK_LEN + 10);
	let mut on_phone = phone.open();
	let hash = phone.import(&mut on_phone, &photo);
	let kept = phone.import(&mut on_phone, b"a photo to keep");

	// The laptop gets the wrapped keys and a copy of the sealed blobs, as sync would give
	// them.
	let wrapped: Vec<(Hash, WrappedBlobKey)> = on_phone
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, wrapped)| (*sealed, wrapped.clone()))
		.collect();
	let mut on_laptop = laptop.open();
	for (sealed, wrapped) in &wrapped {
		on_laptop
			.receive_blob_key(*sealed, wrapped.clone(), &laptop.keys)
			.expect("keeps the key");
	}
	for file in phone.blob_files() {
		let target = laptop.blobs().join(&file);
		fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
		fs::copy(phone.blobs().join(&file), target).expect("copies");
	}
	assert_eq!(read(&on_laptop, &hash), photo);

	let purged: Vec<Hash> = on_phone
		.log()
		.blob_keys(&hash)
		.map(|(sealed, _)| *sealed)
		.collect();
	assert_eq!(purged.len(), 1);
	let digest = hex(purged[0].digest());
	let leftover = Path::new("blake3").join(&digest[..2]).join(&digest);
	let leftover_bytes = fs::read(phone.blobs().join(&leftover)).expect("reads");
	let purge = on_phone
		.purge_blob(&phone.author, &phone.keys, &hash, start(), &mut phone.rng)
		.expect("purges");
	on_laptop
		.receive(&purge, &laptop.keys, start())
		.expect("accepts the purge");
	for (device, stored) in [(&phone, &on_phone), (&laptop, &on_laptop)] {
		assert!(stored.is_shredded());
		assert!(!stored.holds_blob(&hash));
		assert!(stored.holds_blob(&kept));
		assert_eq!(device.blob_files().len(), 1);
		assert!(stored.log().is_blob_purged(&purged[0]).expect("reads"));
		let database = device.database_bytes();
		for (sealed, key) in &wrapped {
			let encoded = cbor::to_vec(key);
			assert_eq!(
				contains(&database, &encoded[encoded.len() - 32..]),
				*sealed != purged[0],
				"only the kept blob's key is on disk"
			);
		}
	}
	drop((on_phone, on_laptop));

	// A crash before the purged file was deleted, and one in the middle of writing a blob,
	// leave files that opening the space deletes.
	let stale = phone.blobs().join(&leftover);
	fs::create_dir_all(stale.parent().expect("a directory")).expect("creates");
	fs::write(&stale, leftover_bytes).expect("puts the file back");
	let temporary = phone.temporary().join("1-0.tmp");
	fs::create_dir_all(temporary.parent().expect("a directory")).expect("creates");
	fs::write(&temporary, b"half a blob").expect("writes");
	let stored = phone.open();
	assert!(!stale.exists());
	assert!(!temporary.exists());
	assert!(!stored.holds_blob(&hash));
	assert_eq!(read(&stored, &kept), b"a photo to keep");
	let laptop_again = laptop.open();
	assert!(!laptop_again.holds_blob(&hash));
	assert_eq!(laptop_again.log().blob_keys(&hash).count(), 0);
}

#[test]
fn a_blob_imported_in_one_place_is_read_in_another() {
	let mut device = Device::new("two places at once");
	let mut stored = device.open();
	let mut other = device.open();
	let hash = device.import(&mut stored, b"a photo");
	// The other catches up with the key that the first saved, and reads the blob.
	assert!(!other.holds_blob(&hash));
	assert!(other.catch_up(&device.keys).expect("catches up"));
	assert!(other.holds_blob(&hash));
	let mut content = Vec::new();
	other
		.open_blob(&hash)
		.expect("opens")
		.read_to_end(&mut content)
		.expect("reads");
	assert_eq!(content, b"a photo");
}

#[test]
fn opening_finishes_a_blob_that_a_crash_interrupted() {
	let mut device = Device::new("a crash while writing a blob");
	let hash = {
		let mut stored = device.open();
		device.import(&mut stored, b"a photo")
	};
	// The crash came after the key was saved, before the file was put in place.
	let file = device.blob_files()[0].clone();
	let placed = device.blobs().join(&file);
	let digest = file
		.file_name()
		.and_then(|name| name.to_str())
		.expect("a digest")
		.to_owned();
	let staged = device.temporary().join(format!("blake3-{digest}"));
	fs::rename(&placed, &staged).expect("takes the file out of place");
	// Another crash came before a key was saved.
	let unsaved = device
		.temporary()
		.join(format!("blake3-{}", hex(Hash::of(b"unsaved").digest())));
	fs::write(&unsaved, b"a sealed blob without a key").expect("writes");

	let stored = device.open();
	assert!(placed.exists());
	assert!(!staged.exists());
	assert!(!unsaved.exists());
	assert_eq!(read(&stored, &hash), b"a photo");
}

#[test]
fn of_several_copies_the_intact_one_is_read() {
	let mut phone = Device::new("several copies on the phone");
	let mut laptop = phone.another("several copies on the laptop");
	let photo = content(BLOB_CHUNK_LEN + 10);
	// The phone and the laptop seal the photo before they learn of each other's keys, and
	// then the phone gets the laptop's key and sealed blob.
	let mut on_phone = phone.open();
	let hash = phone.import(&mut on_phone, &photo);
	let own = phone.blob_files()[0].clone();
	let mut on_laptop = laptop.open();
	laptop.import(&mut on_laptop, &photo);
	let wrapped: Vec<(Hash, WrappedBlobKey)> = on_laptop
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, wrapped)| (*sealed, wrapped.clone()))
		.collect();
	for (sealed, wrapped) in wrapped {
		on_phone
			.receive_blob_key(sealed, wrapped, &phone.keys)
			.expect("keeps the key");
	}
	let theirs = laptop.blob_files()[0].clone();
	let target = phone.blobs().join(&theirs);
	fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
	fs::copy(laptop.blobs().join(&theirs), target).expect("copies");
	assert_eq!(phone.blob_files().len(), 2);

	// The phone's own copy gets damaged.
	let damaged = phone.blobs().join(own);
	let mut sealed = fs::read(&damaged).expect("reads");
	sealed[BLOB_CHUNK_LEN / 2] ^= 1;
	fs::write(&damaged, sealed).expect("damages");
	let later = start() + 60_000;
	assert!(matches!(
		on_phone.verify_blob(&hash, later),
		Err(StoreError::DamagedBlob)
	));
	assert_eq!(on_phone.blob_verified(&hash).expect("reads"), Some(later));
	assert_eq!(read(&on_phone, &hash), photo);
}

#[test]
fn blob_keys_open_once_their_space_key_arrives() {
	let mut phone = Device::new("keys arrive later");
	let mut on_phone = phone.open();
	let hash = phone.import(&mut on_phone, b"an old photo");
	let wrapped: Vec<(Hash, WrappedBlobKey)> = on_phone
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, wrapped)| (*sealed, wrapped.clone()))
		.collect();

	// A new member's device has only the space's newest key so far.
	let newest = TestRng::new("the newest space key").key();
	let mut all_keys = phone.keys.clone();
	all_keys.insert(1, newest.clone());
	let newcomer = Device::of(
		"keys arrive later on the newcomer",
		phone.space,
		SpaceKeys::new(1, newest),
		TestRng::new("the newcomer"),
	);
	let mut on_newcomer = newcomer.open();
	for (sealed, wrapped) in wrapped {
		on_newcomer
			.receive_blob_key(sealed, wrapped, &newcomer.keys)
			.expect("keeps the key");
	}
	let file = phone.blob_files()[0].clone();
	let target = newcomer.blobs().join(&file);
	fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
	fs::copy(phone.blobs().join(&file), target).expect("copies");
	assert!(!on_newcomer.holds_blob(&hash), "no key opens it yet");
	let inventory = on_newcomer.blob_inventory(&BTreeSet::new()).expect("lists");
	assert_eq!((inventory.held.len(), inventory.waiting), (0, 1));

	on_newcomer
		.open_blob_keys(&all_keys)
		.expect("opens the keys");
	assert!(on_newcomer.holds_blob(&hash));
	assert_eq!(read(&on_newcomer, &hash), b"an old photo");
}

#[test]
fn the_blob_directory_is_accounted_for() {
	use nibkeep_base::cbor::{ToCbor, Value as Cbor};
	use nibkeep_crypto::Envelope;

	let mut phone = Device::new("inventory");
	let mut on_phone = phone.open();
	let hash = phone.import(&mut on_phone, b"a scan");
	let inventory = on_phone.blob_inventory(&BTreeSet::new()).expect("lists");
	assert_eq!(inventory.held, [hash]);
	assert_eq!(
		(
			inventory.waiting,
			inventory.keyless.len(),
			inventory.unknown.len()
		),
		(0, 0, 0)
	);

	// A copy that no key opens, and a file that is no sealed blob.
	let file = phone.blob_files()[0].clone();
	let lost = Hash::of(b"a key that was lost");
	let digest = hex(lost.digest());
	let target = phone
		.blobs()
		.join("blake3")
		.join(&digest[..2])
		.join(&digest);
	fs::create_dir_all(target.parent().expect("a directory")).expect("creates");
	fs::copy(phone.blobs().join(&file), &target).expect("copies");
	fs::write(phone.blobs().join("notes.txt"), b"not a blob").expect("writes");
	let inventory = on_phone.blob_inventory(&BTreeSet::new()).expect("lists");
	assert_eq!(inventory.held, [hash]);
	assert_eq!(inventory.keyless, [lost]);
	assert_eq!(inventory.unknown, [phone.blobs().join("notes.txt")]);

	// A key of a space key that this device does not have yet waits; once the device has
	// it, a key that turns out to be bad leaves the disk, and opening says so.
	let sealed = *on_phone.log().wrapped_blob_keys().next().expect("a key").0;
	let garbage: WrappedBlobKey = cbor::from_slice(&cbor::encode(&Cbor::Array(vec![
		7_u64.to_cbor(),
		Envelope::seal(&phone.rng.key(), b"garbage", b"", &mut phone.rng).to_cbor(),
	])))
	.expect("well-formed");
	on_phone
		.receive_blob_key(sealed, garbage, &phone.keys)
		.expect("keeps the key for later");
	assert_eq!(on_phone.blob_keys_dropped_at_open(), []);
	drop(on_phone);
	let mut later = phone.keys.clone();
	later.insert(7, TestRng::new("the seventh space key").key());
	let reopened = StoredLog::open(
		&phone.path,
		phone.space,
		&later,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[],
	)
	.expect("opens");
	assert_eq!(reopened.blob_keys_dropped_at_open(), [sealed]);
	assert!(reopened.holds_blob(&hash), "the good key stays");
}

#[test]
fn a_damaged_blob_is_set_aside_until_an_intact_copy_comes_again() {
	let mut phone = Device::new("aside phone");
	let laptop = phone.another("aside laptop");
	let mut sent = phone.open();
	let content = vec![4; 2 * BLOB_CHUNK_LEN + 9];
	let hash = phone.import(&mut sent, &content);
	let sealed = sent.sealed_blobs().expect("lists")[0];
	let (size, mut file) = sent
		.open_sealed_blob(&sealed)
		.expect("opens")
		.expect("holds it");
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes).expect("reads");
	let mut received = laptop.open();
	let wrapped: Vec<(Hash, WrappedBlobKey)> = sent
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, key)| (*sealed, key.clone()))
		.collect();
	for (sealed, key) in wrapped {
		received
			.receive_blob_key(sealed, key, &laptop.keys)
			.expect("keeps the key");
	}
	assert!(
		received
			.receive_sealed_blob(sealed, size, bytes.as_slice())
			.expect("keeps it")
	);
	assert_eq!(
		received
			.check_sealed_blob(&sealed, &|| false)
			.expect("reads"),
		SealedCheck::Intact(size)
	);
	// A flipped bit in the laptop's copy is damage, which a check finds without a key.
	let file = laptop.blobs().join(&laptop.blob_files()[0]);
	let mut stored = fs::read(&file).expect("reads");
	let middle = stored.len() / 2;
	stored[middle] ^= 1;
	fs::write(&file, &stored).expect("writes");
	assert_eq!(
		received
			.check_sealed_blob(&sealed, &|| false)
			.expect("reads"),
		SealedCheck::Damaged
	);
	assert_eq!(
		received
			.check_sealed_blob(&sealed, &|| true)
			.expect("reads"),
		SealedCheck::Stopped
	);
	// Moved aside, the copy is no longer held, so that a sync asks for it again.
	assert!(received.set_aside_blob(&sealed).expect("moves"));
	received
		.record_checks(Vec::new(), vec![(sealed, 6_000)], Vec::new())
		.expect("records");
	assert!(!received.holds_sealed(&sealed));
	assert!(received.is_blob_aside(&sealed));
	assert_eq!(received.aside_blobs(), [sealed]);
	// Read again, the copy aside is still damaged, and stays aside.
	assert_eq!(
		received
			.recheck_aside_blob(&sealed, &|| false)
			.expect("reads"),
		SealedCheck::Damaged
	);
	assert!(received.is_blob_aside(&sealed));
	assert_eq!(received.sealed_blobs().expect("lists"), Vec::<Hash>::new());
	assert!(!received.holds_blob(&hash));
	assert_eq!(
		received.damaged_blobs().expect("reads").get(&sealed),
		Some(&6_000)
	);
	assert!(!received.set_aside_blob(&sealed).expect("moves"), "once");
	assert_eq!(
		received
			.check_sealed_blob(&sealed, &|| false)
			.expect("reads"),
		SealedCheck::Gone
	);
	// Other bytes are refused, and the damaged copy stays aside.
	assert!(
		!received
			.receive_sealed_blob(sealed, size, stored.as_slice())
			.expect("reads")
	);
	assert!(received.is_blob_aside(&sealed));
	// The intact copy takes its place, checked, and the damaged one goes.
	assert!(
		received
			.receive_sealed_blob(sealed, size, bytes.as_slice())
			.expect("keeps it")
	);
	assert!(received.holds_sealed(&sealed));
	assert!(!received.is_blob_aside(&sealed));
	assert_eq!(received.aside_blobs(), Vec::<Hash>::new());
	let mut read = Vec::new();
	received
		.open_blob(&hash)
		.expect("opens")
		.read_to_end(&mut read)
		.expect("reads");
	assert_eq!(read, content);
}

#[test]
fn a_copy_moved_aside_after_a_bad_read_goes_back_once_it_reads_intact() {
	let mut phone = Device::new("aside bad read");
	let mut stored = phone.open();
	let hash = phone.import(&mut stored, &[8; 3_000]);
	let sealed = stored.sealed_blobs().expect("lists")[0];
	// As if a bad read had taken an intact copy for a damaged one.
	assert!(stored.set_aside_blob(&sealed).expect("moves"));
	assert!(!stored.holds_blob(&hash));
	assert!(matches!(
		stored
			.recheck_aside_blob(&sealed, &|| false)
			.expect("reads"),
		SealedCheck::Intact(_)
	));
	assert!(stored.holds_sealed(&sealed));
	assert!(!stored.is_blob_aside(&sealed));
	assert!(stored.holds_blob(&hash));
}

#[test]
fn a_purge_deletes_a_damaged_copy_kept_aside() {
	let mut phone = Device::new("aside purged");
	let mut stored = phone.open();
	let hash = phone.import(&mut stored, &[6; 1_000]);
	let sealed = stored.sealed_blobs().expect("lists")[0];
	let file = phone.blobs().join(&phone.blob_files()[0]);
	let mut bytes = fs::read(&file).expect("reads");
	bytes[10] ^= 1;
	fs::write(&file, &bytes).expect("writes");
	assert!(stored.set_aside_blob(&sealed).expect("moves"));
	stored
		.record_checks(Vec::new(), vec![(sealed, 6_000)], Vec::new())
		.expect("records");
	assert!(stored.is_blob_aside(&sealed));
	stored
		.purge_blob(
			&phone.author,
			&phone.keys,
			&hash,
			start() + 1_000,
			&mut phone.rng,
		)
		.expect("purges");
	assert!(!stored.is_blob_aside(&sealed));
	assert!(stored.damaged_blobs().expect("reads").is_empty());
}

#[test]
fn a_sealed_blob_travels_as_it_is_stored_and_opens_with_its_key() {
	let mut phone = Device::new("travels phone");
	let laptop = phone.another("travels laptop");
	let mut sent = phone.open();
	let content = vec![9; 3 * BLOB_CHUNK_LEN + 5];
	let hash = phone.import(&mut sent, &content);
	let sealed = sent.sealed_blobs().expect("lists");
	assert_eq!(sealed.len(), 1);
	let sealed = sealed[0];
	let (size, mut file) = sent
		.open_sealed_blob(&sealed)
		.expect("opens")
		.expect("holds it");
	let mut bytes = Vec::new();
	file.read_to_end(&mut bytes).expect("reads");
	assert_eq!(bytes.len() as u64, size);

	let mut received = laptop.open();
	// Bytes that are not the sealed blob's, or fewer, are refused.
	let mut changed = bytes.clone();
	changed[100] ^= 1;
	assert!(
		!received
			.receive_sealed_blob(sealed, size, changed.as_slice())
			.expect("reads")
	);
	assert!(
		!received
			.receive_sealed_blob(sealed, size, &bytes[..bytes.len() - 1])
			.expect("reads")
	);
	assert_eq!(received.sealed_blobs().expect("lists"), Vec::<Hash>::new());
	assert!(
		received
			.receive_sealed_blob(sealed, size, bytes.as_slice())
			.expect("keeps it")
	);
	assert!(
		!received
			.receive_sealed_blob(sealed, size, bytes.as_slice())
			.expect("held already"),
		"a blob it holds is kept once"
	);
	// Without its key the content cannot be read, nor is the blob the space's to pass on;
	// with it, it is.
	assert_eq!(received.sealed_blobs().expect("lists"), Vec::<Hash>::new());
	assert!(!received.holds_blob(&hash));
	let wrapped: Vec<(Hash, WrappedBlobKey)> = sent
		.log()
		.wrapped_blob_keys()
		.map(|(sealed, key)| (*sealed, key.clone()))
		.collect();
	for (sealed, key) in wrapped {
		received
			.receive_blob_key(sealed, key, &laptop.keys)
			.expect("keeps the key");
	}
	assert!(received.holds_blob(&hash));
	assert_eq!(received.sealed_blobs().expect("lists"), vec![sealed]);
	let mut read = Vec::new();
	received
		.open_blob(&hash)
		.expect("opens")
		.read_to_end(&mut read)
		.expect("reads");
	assert_eq!(read, content);
	drop(received);
	// It is still there when the space is opened again.
	assert_eq!(laptop.open().sealed_blobs().expect("lists"), vec![sealed]);
}

#[test]
fn a_secret_blob_opens_only_while_the_secrets_are_unlocked() {
	let mut phone = Device::new("secret blob");
	let scan = content(BLOB_CHUNK_LEN + 10);
	let mut on_phone = phone.open();
	let hash = phone.import_secret(&mut on_phone, &scan);
	assert!(on_phone.holds_blob(&hash));
	assert_eq!(read(&on_phone, &hash), scan);
	let bytes = phone.database_bytes();
	assert!(
		!contains(&bytes, &hash.to_multihash()),
		"the content hash is sealed"
	);
	for file in phone.blob_files() {
		let sealed = fs::read(phone.blobs().join(file)).expect("reads");
		assert!(!contains(&sealed, &scan[..64]), "the content is sealed");
	}

	on_phone.lock_secrets();
	assert!(!on_phone.holds_blob(&hash));
	assert!(matches!(
		on_phone.open_blob(&hash),
		Err(StoreError::MissingBlob)
	));
	let inventory = on_phone.blob_inventory(&BTreeSet::new()).expect("lists");
	assert_eq!((inventory.held.len(), inventory.waiting), (0, 1));
	on_phone.unlock_secrets(&phone.secrets()).expect("unlocks");
	assert_eq!(read(&on_phone, &hash), scan);

	// Opened again, the space holds it sealed, until the secrets are unlocked.
	drop(on_phone);
	let mut on_phone = phone.open();
	assert!(!on_phone.holds_blob(&hash));
	assert_eq!(
		on_phone.log().wrapped_blob_keys().count(),
		1,
		"the key is kept"
	);
	on_phone.unlock_secrets(&phone.secrets()).expect("unlocks");
	assert_eq!(read(&on_phone, &hash), scan);
}

#[test]
fn a_secret_blob_needs_the_secrets() {
	let mut phone = Device::new("secret blob locked");
	let mut on_phone = phone.open();
	let imported =
		on_phone.import_secret_blob(&phone.keys, &b"a scan"[..], start(), &mut phone.rng);
	assert!(
		matches!(imported, Err(StoreError::Log(LogError::SecretsLocked))),
		"{imported:?}"
	);
	nibkeep_testkit::assert_empty!(phone.blob_files());
	assert_eq!(on_phone.log().wrapped_blob_keys().count(), 0);
}

#[test]
fn content_is_sealed_once_for_each_kind_of_key() {
	let mut phone = Device::new("secret and ordinary");
	let mut on_phone = phone.open();
	let photo = phone.import(&mut on_phone, b"the same content");
	let scan = phone.import_secret(&mut on_phone, b"the same content");
	assert_eq!(photo, scan);
	assert_eq!(phone.blob_files().len(), 2, "a secret copy of its own");
	phone.import_secret(&mut on_phone, b"the same content");
	phone.import(&mut on_phone, b"the same content");
	assert_eq!(phone.blob_files().len(), 2, "stored once for each");

	// Content held only as a secret is sealed anew for an ordinary file.
	let secret_first = phone.import_secret(&mut on_phone, b"a secret first");
	phone.import(&mut on_phone, b"a secret first");
	assert_eq!(phone.blob_files().len(), 4);
	on_phone.lock_secrets();
	assert_eq!(read(&on_phone, &secret_first), b"a secret first");
}

#[test]
fn a_secret_blob_travels_sealed_and_opens_where_the_secrets_are_unlocked() {
	let mut phone = Device::new("secret blob on the phone");
	let laptop = phone.another("secret blob on the laptop");
	let scan = content(3 * BLOB_CHUNK_LEN);
	let mut on_phone = phone.open();
	let hash = phone.import_secret(&mut on_phone, &scan);
	let mut on_laptop = laptop.open();
	phone.send(&on_phone, &mut on_laptop, &laptop);
	assert!(
		!on_laptop.holds_blob(&hash),
		"the space key alone opens nothing"
	);
	assert_eq!(
		on_laptop
			.blob_inventory(&BTreeSet::new())
			.expect("lists")
			.waiting,
		1
	);

	// A key that an infected replica of the space wrapped for the same sealed blob, with a
	// secrets key it made up.
	let sealed = *on_phone.log().wrapped_blob_keys().next().expect("a key").0;
	let mut rng = TestRng::new("an infected device");
	let mut made_up = phone.keys.clone();
	made_up.insert_secrets(0, rng.key());
	let mut infected = Log::new(phone.space, Clock::new(DRIFT));
	infected
		.add_secret_blob_key(
			Hash::of(b"something else"),
			sealed,
			rng.key(),
			&made_up,
			&mut rng,
		)
		.expect("adds the key");
	let forged = infected.wrapped_blob_keys_of(&sealed)[0].clone();
	on_laptop
		.receive_blob_key(sealed, forged, &laptop.keys)
		.expect("keeps the key");
	assert_eq!(on_laptop.log().wrapped_blob_keys_of(&sealed).len(), 2);

	on_laptop.unlock_secrets(&phone.secrets()).expect("unlocks");
	assert_eq!(read(&on_laptop, &hash), scan);
	assert_eq!(on_laptop.log().wrapped_blob_keys_of(&sealed).len(), 1);
	drop(on_laptop);
	let on_laptop = laptop.open();
	assert_eq!(
		on_laptop.log().wrapped_blob_keys_of(&sealed),
		on_phone.log().wrapped_blob_keys_of(&sealed),
		"the forged key is gone from the disk"
	);
}

#[test]
fn a_secret_blob_keeps_its_key_where_forged_ordinary_keys_arrive_locked() {
	let mut phone = Device::new("secret blob forged ordinary");
	let laptop = phone.another("secret blob forged ordinary on the laptop");
	let scan = content(BLOB_CHUNK_LEN + 10);
	let mut on_phone = phone.open();
	let hash = phone.import_secret(&mut on_phone, &scan);
	let sealed = *on_phone.log().wrapped_blob_keys().next().expect("a key").0;
	let mut on_laptop = laptop.open();
	phone.send(&on_phone, &mut on_laptop, &laptop);
	// An infected replica sends ordinary keys of its making for the same sealed blob, which
	// open with the space key, while the laptop's secrets are locked.
	let mut rng = TestRng::new("forged ordinary keys");
	for content in [b"one", b"two", b"six", b"ten", b"two"] {
		let mut infected = Log::new(phone.space, Clock::new(DRIFT));
		infected
			.add_blob_key(Hash::of(content), sealed, rng.key(), &phone.keys, &mut rng)
			.expect("adds the key");
		let forged = infected.wrapped_blob_keys_of(&sealed)[0].clone();
		on_laptop
			.receive_blob_key(sealed, forged, &laptop.keys)
			.expect("keeps the key");
	}
	// Opened again, locked, then unlocked: the good key is there, and opens.
	drop(on_laptop);
	let mut on_laptop = laptop.open();
	on_laptop.unlock_secrets(&phone.secrets()).expect("unlocks");
	assert_eq!(read(&on_laptop, &hash), scan);
}

#[test]
fn spaces_of_a_replica_keep_one_sealed_copy_of_a_content() {
	let mut personal = Device::new("one copy for two spaces");
	let mut family = personal.other_space("one copy, the family's space");
	let mut in_personal = personal.open();
	let mut in_family = family.open();
	let photo = content(BLOB_CHUNK_LEN + 10);
	let hash = personal.import(&mut in_personal, &photo);
	assert_eq!(
		family.import_sharing(&mut in_family, &in_personal, &photo),
		hash
	);
	assert_eq!(personal.blob_files().len(), 1, "one sealed copy");
	assert_eq!(
		in_family.sealed_blobs().expect("lists"),
		in_personal.sealed_blobs().expect("lists"),
		"its key wrapped for the other space"
	);
	assert_eq!(read(&in_family, &hash), photo);
	assert_eq!(
		in_family.blob_verified(&hash).expect("reads"),
		Some(start())
	);

	// Each space lists for other replicas only the copies it keeps keys for.
	let scan = personal.import(&mut in_personal, b"a scan of the personal space");
	assert_eq!(in_personal.sealed_blobs().expect("lists").len(), 2);
	assert_eq!(in_family.sealed_blobs().expect("lists").len(), 1);
	let personal_only = in_personal
		.held_copies(&scan, false)
		.first()
		.expect("a copy")
		.0;
	assert!(
		in_family
			.open_sealed_blob(&personal_only)
			.expect("reads")
			.is_none()
	);
	// Nor is it the family space's to account for, as the personal space explains it.
	let elsewhere = in_personal.known_blobs().expect("lists");
	let inventory = in_family.blob_inventory(&elsewhere).expect("lists");
	assert_eq!(inventory.held, [hash]);
	nibkeep_testkit::assert_empty!(inventory.keyless);
	assert_eq!(
		in_family
			.blob_inventory(&BTreeSet::new())
			.expect("lists")
			.keyless,
		[personal_only]
	);
}

#[test]
fn a_purge_in_one_space_leaves_the_copy_to_the_others() {
	let mut personal = Device::new("a purge in one space");
	let mut family = personal.other_space("a purge in one space, the family's");
	let mut in_personal = personal.open();
	let mut in_family = family.open();
	let photo = content(BLOB_CHUNK_LEN + 10);
	let hash = personal.import(&mut in_personal, &photo);
	family.import_sharing(&mut in_family, &in_personal, &photo);
	let sealed = in_personal.sealed_blobs().expect("lists")[0];
	let bytes = fs::read(personal.blobs().join(&personal.blob_files()[0])).expect("reads");

	in_personal
		.purge_blob(
			&personal.author,
			&personal.keys,
			&hash,
			start(),
			&mut personal.rng,
		)
		.expect("purges");
	assert!(!in_personal.holds_blob(&hash));
	assert_eq!(read(&in_family, &hash), photo, "the family's key stays");
	assert_eq!(personal.blob_files().len(), 1);
	drop(in_personal);
	let in_personal = personal.open();
	assert_eq!(personal.blob_files().len(), 1, "opening keeps it too");
	assert!(!in_personal.holds_blob(&hash));

	// Once the last space that keeps it purges it, the file goes; and one that a crash left
	// goes when the space is opened again.
	in_family
		.purge_blob(
			&family.author,
			&family.keys,
			&hash,
			start(),
			&mut family.rng,
		)
		.expect("purges");
	nibkeep_testkit::assert_empty!(personal.blob_files());
	drop(in_family);
	let file = personal.blobs().join(
		Path::new("blake3")
			.join(&hex(sealed.digest())[..2])
			.join(hex(sealed.digest())),
	);
	fs::create_dir_all(file.parent().expect("a directory")).expect("creates");
	fs::write(&file, &bytes).expect("puts the file back");
	drop(family.open());
	assert!(!file.exists());
}

#[test]
fn a_space_that_purged_a_copy_seals_the_content_anew() {
	let mut personal = Device::new("a purged copy is not taken up");
	let mut family = personal.other_space("a purged copy, the family's");
	let mut in_personal = personal.open();
	let mut in_family = family.open();
	let hash = personal.import(&mut in_personal, b"a photo");
	family.import_sharing(&mut in_family, &in_personal, b"a photo");
	in_family
		.purge_blob(
			&family.author,
			&family.keys,
			&hash,
			start(),
			&mut family.rng,
		)
		.expect("purges");
	assert_eq!(personal.blob_files().len(), 1);

	// Added to the family's space again, the photo is sealed anew there: the copy whose
	// keys it destroyed stays the personal space's alone.
	family.import_sharing(&mut in_family, &in_personal, b"a photo");
	assert_eq!(personal.blob_files().len(), 2);
	assert_ne!(
		in_family.sealed_blobs().expect("lists"),
		in_personal.sealed_blobs().expect("lists")
	);
	assert_eq!(read(&in_family, &hash), b"a photo");
}

#[test]
fn a_copy_that_does_not_hold_its_content_is_not_taken_up() {
	let mut personal = Device::new("a damaged copy is not taken up");
	let mut family = personal.other_space("a damaged copy, the family's");
	let mut in_personal = personal.open();
	let mut in_family = family.open();
	let photo = content(BLOB_CHUNK_LEN + 10);
	personal.import(&mut in_personal, &photo);
	let file = personal.blobs().join(&personal.blob_files()[0]);
	let mut sealed = fs::read(&file).expect("reads");
	sealed[BLOB_CHUNK_LEN / 2] ^= 1;
	fs::write(&file, sealed).expect("damages");

	let hash = family.import_sharing(&mut in_family, &in_personal, &photo);
	assert_eq!(personal.blob_files().len(), 2, "sealed anew");
	assert_eq!(read(&in_family, &hash), photo);
}

#[test]
fn a_replica_that_waits_keeps_a_purged_blob_apart_until_restored_or_destroyed() {
	let mut phone = Device::new("waiting phone");
	let desktop = phone.another("waiting desktop");
	let photo = content(BLOB_CHUNK_LEN + 10);
	let mut on_phone = phone.open();
	let hash = phone.import(&mut on_phone, &photo);
	let mut on_desktop = desktop.open();
	on_desktop.keep_destroyed(true);
	phone.send(&on_phone, &mut on_desktop, &desktop);
	assert_eq!(read(&on_desktop, &hash), photo);
	let sealed: Vec<Hash> = on_desktop
		.log()
		.blob_keys(&hash)
		.map(|(sealed, _)| *sealed)
		.collect();
	let kept_file = desktop
		.root
		.join("kept")
		.join("blake3")
		.join(hex(sealed[0].digest()));

	// The purge takes effect on the desktop, which keeps the key and the file apart.
	let purge = on_phone
		.purge_blob(&phone.author, &phone.keys, &hash, start(), &mut phone.rng)
		.expect("purges");
	on_desktop
		.receive(&purge, &desktop.keys, start() + 1_000)
		.expect("takes the purge");
	assert!(!on_desktop.holds_blob(&hash));
	nibkeep_testkit::assert_empty!(desktop.blob_files());
	assert!(kept_file.is_file());
	assert_eq!(
		on_desktop.kept_blob_size(&sealed[0]),
		Some(fs::metadata(&kept_file).expect("a file").len())
	);
	let kept = on_desktop.kept().expect("reads");
	assert_eq!(kept.len(), 1);
	assert_eq!(kept[0].purge, purge.id());
	assert_eq!(kept[0].received, start() + 1_000);
	drop(on_desktop);
	let mut on_desktop = desktop.open();
	on_desktop.keep_destroyed(true);
	assert_eq!(
		on_desktop.kept().expect("reads"),
		kept,
		"kept across a restart"
	);

	// Restored, the key and the file come back, and the phone takes the key again.
	let restore = on_desktop
		.restore_kept(
			&desktop.author,
			&desktop.keys,
			(purge.id(), &nibkeep_log::Restores::default()),
			start() + 2_000,
			&mut phone.rng,
		)
		.expect("restores")
		.expect("something was kept");
	assert!(!kept_file.exists());
	nibkeep_testkit::assert_empty!(on_desktop.kept().expect("reads"));
	assert_eq!(read(&on_desktop, &hash), photo);
	on_phone
		.receive(&restore, &phone.keys, start() + 3_000)
		.expect("takes the restore");
	desktop.send(&on_desktop, &mut on_phone, &phone);
	assert_eq!(read(&on_phone, &hash), photo);

	// Purged again, and its wait over, nothing of it is left on the desktop's disk.
	let again = on_phone
		.purge_blob(
			&phone.author,
			&phone.keys,
			&hash,
			start() + 4_000,
			&mut phone.rng,
		)
		.expect("purges again");
	on_desktop
		.receive(&again, &desktop.keys, start() + 5_000)
		.expect("takes the purge");
	assert!(kept_file.is_file());
	let wrapped = on_desktop.kept().expect("reads");
	let Some(nibkeep_log::Kept::BlobKeys(_, keys)) = wrapped.first().map(|row| &row.part) else {
		panic!("the blob's keys are kept");
	};
	let encoded = cbor::to_vec(&keys[0]);
	assert!(contains(
		&desktop.database_bytes(),
		&encoded[encoded.len() - 32..]
	));
	let destroyed = on_desktop
		.destroy_kept(&BTreeSet::from([again.id()]), &desktop.keys)
		.expect("destroys");
	assert_eq!(destroyed, 1);
	assert!(!kept_file.exists());
	nibkeep_testkit::assert_empty!(on_desktop.kept().expect("reads"));
	assert!(on_desktop.is_shredded());
	assert!(!contains(
		&desktop.database_bytes(),
		&encoded[encoded.len() - 32..]
	));
}
