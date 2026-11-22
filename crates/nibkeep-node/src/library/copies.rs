//! Copies of replicas made elsewhere, which a library's directory holds beside the replica
//! that is here: brought by a backup, a program that syncs folders, or by hand. A copy is
//! never opened or written to; what it holds is taken in, as from another replica
//! (technical design: copies of a replica).

use std::convert::Infallible;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_crypto::rand_core::{CryptoRng, TryCryptoRng, TryRng};
use nibkeep_governance::PairingCode;
use nibkeep_keys::{KeyFile, NewProtection, Protection, Unlocker};
use nibkeep_store::{CopiedItem, log_path, take_copied_log};
use nibkeep_sync::{Holder, Transport, TransportError};

use super::sync::{Joining, Sponsor, Syncing, endpoint_of};
use super::taking::NewDevice;
use super::{INFO, KEYS, Library, REPLICAS, new_replica_folder, replica_folders};
use crate::NodeError;
use crate::info::{Info, write_file};
use crate::place::{Drive, declare_drive};

/// The file, in a replica's folder, that records how the copies beside it looked when it
/// last looked, and whether the person was told that a program syncs the library's folder.
const SEEN: &str = "copies.cbor";

/// What programs that sync folders leave in the folders they sync, or in their roots, and
/// whether it is a file: Syncthing's folder, Dropbox's file and folder of its cache, which
/// its folder of settings in a home folder is not, and ownCloud's file; Nextcloud's journal
/// is `.sync_*.db`.
const MARKS: &[(&str, bool)] = &[
	(".stfolder", false),
	(".dropbox", true),
	(".dropbox.cache", false),
	(".owncloudsync.log", true),
];

/// A copy of a replica made elsewhere, in a library's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaCopy {
	/// Its folder, which is read and never written.
	pub folder: PathBuf,
	/// The replica it is a copy of, and its device, as its library file says, if it reads.
	pub replica: Option<(EntityId, EntityId)>,
}

impl ReplicaCopy {
	/// How the copy's keys are protected, and its secrets', if it has any: whether a
	/// passphrase opens them, or a keystore entry, which only the computer that made it
	/// holds, unless its keyring came with the copy.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NewerFormats`] if a later version of NibKeep changed the copy,
	/// and an error if its key file does not read.
	pub fn protections(&self) -> Result<(Protection, Option<Protection>), NodeError> {
		crate::engines::check(&self.folder)?;
		let key_file = KeyFile::read(&self.folder.join(KEYS))?;
		Ok((
			key_file.replica_protection().clone(),
			key_file.secrets_protection().cloned(),
		))
	}
}

/// What taking in copies brought.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TakenIn {
	/// The copies read.
	pub copies: usize,
	/// The changes, keys and blobs that were new to this replica.
	pub items: usize,
}

impl Library {
	/// The copies that a library's directory holds: the folders of replicas that are not
	/// here, also those that a program may still be bringing in, but the one that a recovery
	/// in place replaces, until it is set aside.
	#[must_use]
	pub fn copies_in(directory: &Path) -> Vec<ReplicaCopy> {
		let here = Self::replica_folder(directory).ok();
		let replaced = here.as_deref().and_then(super::recovering::replaced_by);
		// Sorted by folder, as the folders are.
		replica_folders(directory)
			.unwrap_or_default()
			.into_iter()
			.map(|(folder, _)| folder)
			.filter(|folder| Some(folder) != here.as_ref() && Some(folder) != replaced.as_ref())
			.map(|folder| ReplicaCopy {
				replica: Info::read(&folder.join(INFO))
					.ok()
					.map(|info| (info.replica, info.device)),
				folder,
			})
			.collect()
	}

	/// The copies beside this replica in its library's directory.
	#[must_use]
	pub fn copies(&self) -> Vec<ReplicaCopy> {
		self.directory
			.parent()
			.and_then(Path::parent)
			.map_or_default(Self::copies_in)
	}

	/// Adds this computer as a device of its own, in a new replica beside the copy in `copy`
	/// in the library's directory `directory`, with the copy's own keys, which `unlocker`
	/// opens: for
	/// when no other device of the person's can add it, as after a library restored from the
	/// backup of the only one. The copy is opened in a private copy of its own, with its
	/// keys in a key file of that copy's, and nothing of the copy or its keystore entries is
	/// written to. It adds the new replica as pairing does, in this process; the new replica
	/// takes in what the copy holds, and with `secrets`, which open the copy's secrets and
	/// protect them here, the secrets too, as [`Library::take_secrets_from_copy`] does; then
	/// it revokes the replicas of the copy's device, whose keys a thief of that device may
	/// hold. A device of the person's that turns up later does not know the new one, and is
	/// added anew.
	///
	/// # Errors
	///
	/// Returns [`NodeError::Exists`] if a replica is here already, an error of the keys if
	/// `unlocker` does not open the copy's, [`NodeError::AiAgentMayNot`] if the copy is an AI
	/// agent's, [`NodeError::NewerFormats`] if a later version of NibKeep changed the copy,
	/// and an error if the copy does not read, or the new replica cannot be made.
	pub fn add_from_copy(
		directory: &Path,
		(copy, unlocker): (&Path, &Unlocker<'_>),
		joining: Joining,
		protection: &NewProtection<'_>,
		secrets: Option<(&Unlocker<'_>, &NewProtection<'_>)>,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<Self, NodeError> {
		match Self::replica_folder(directory) {
			Err(NodeError::NoLibrary | NodeError::Copied) => {}
			_ => return Err(NodeError::Exists),
		}
		// A copy that a later version changed is read by none of its files.
		crate::engines::check(copy)?;
		// The secrets' unlocker is tried first, so that a mistyped passphrase leaves nothing
		// made.
		if let Some((unlocker, _)) = secrets {
			KeyFile::read(&copy.join(KEYS))?.unlock_secrets(unlocker)?;
		}
		// Before the private copy is made there, as a program that syncs the folder would take
		// it to the cloud.
		Self::check_place(directory)?;
		// The private copy is of this computer's, beside the replicas, where the folders of
		// replicas that are being made are too.
		let replicas = directory.join(REPLICAS);
		fs::create_dir_all(&replicas).map_err(NodeError::file(&replicas))?;
		let private = private_folder(&replicas, "private", rng)?;
		let added = (|| {
			let mut sponsor = open_privately(copy, unlocker, &private, now, rng)?;
			sponsor.check_not_ai_agent("add devices")?;
			let old = sponsor.info.device;
			// The new device takes the spaces that the copy holds, and asks before it takes a
			// space given to it later if the copy did.
			let device = sponsor.as_new_device()?;
			let mut library = pair_in_process(
				&mut sponsor,
				directory,
				(joining, &device),
				protection,
				now,
				rng,
			)?;
			// Then the spaces settle between the two, as they do after every pairing, before
			// the copy goes.
			if let Err(error) = spaces_in_process(&mut sponsor, &mut library, now, rng) {
				library.remove_unsynced();
				return Err(error);
			}
			drop(sponsor);
			// The new device is known only to the private copy, which goes: if what follows
			// fails, it goes too, and the person can begin again.
			let finished = (|| {
				library.take_in_from(copy, now)?;
				// Before the copy's device is revoked, whose secrets would be lost with it.
				if let Some((unlocker, protection)) = secrets {
					library.take_secrets_from_copy(copy, unlocker, protection, now(), rng)?;
				}
				let revoking: Vec<EntityId> = library
					.replicas()?
					.into_iter()
					.filter(|replica| {
						!replica.this && !replica.revoked && replica.device == Some(old)
					})
					.map(|replica| replica.id)
					.collect();
				if !revoking.is_empty() {
					library.revoke(&revoking, now(), rng)?;
				}
				Ok::<(), NodeError>(())
			})();
			match finished {
				Ok(()) => Ok(library),
				Err(error) => {
					library.remove_unsynced();
					Err(error)
				}
			}
		})();
		let _ = fs::remove_dir_all(&private);
		added
	}

	/// Takes in what the copy in `copy` holds and this replica lacks, as
	/// [`Library::take_in_copies`] does.
	///
	/// # Errors
	///
	/// As [`Library::take_in_copies`].
	pub fn take_in_from(
		&mut self,
		copy: &Path,
		now: &dyn Fn() -> u64,
	) -> Result<TakenIn, NodeError> {
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		self.take_in(&[copy.to_owned()], &spaces, As::ThisReplica, now)
	}

	/// Takes in what the replica in `folder`, of a lost device, holds of `spaces` and this
	/// replica lacks, as [`Library::take_in_copies`] does, but taking each change as from its
	/// author, as a backup's are, so that what a replica that the space ended made after that
	/// is left out. Returns how many changes, keys and blobs were new.
	///
	/// # Errors
	///
	/// As [`Library::take_in_copies`].
	pub(crate) fn take_in_lost_replica(
		&mut self,
		folder: &Path,
		spaces: &[SpaceId],
		now: &dyn Fn() -> u64,
	) -> Result<usize, NodeError> {
		Ok(self
			.take_in(&[folder.to_owned()], spaces, As::Authors, now)?
			.items)
	}

	/// Adds a new replica of this library in `directory`, which `joining` asks for, as
	/// pairing does, but in this process and without a network: pairing, then the new
	/// replica's first sync, which brings it the personal space. `now` is the new replica's
	/// wall clock; this one makes its changes at the time pairing starts. Benchmarks time a
	/// first sync with it.
	///
	/// # Errors
	///
	/// Returns an error if this replica cannot sponsor one, `directory` holds a library
	/// already, or the first sync fails, in which case the new replica is removed.
	pub fn pair_in_process(
		&mut self,
		directory: &Path,
		joining: Joining,
		protection: &NewProtection<'_>,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<Self, NodeError> {
		pair_in_process(
			self,
			directory,
			(joining, &NewDevice::computer()),
			protection,
			now,
			rng,
		)
	}

	/// What a device made from this replica, a copy's, is to take: the spaces that it holds,
	/// but those whose drop it stated, and, as it says, the spaces given to it later or not. A
	/// space that it was dropping, and still holds, it takes, as its blobs whose copies
	/// elsewhere were not proven yet may be only in the copy.
	fn as_new_device(&self) -> Result<NewDevice, NodeError> {
		let mut spaces = Vec::new();
		for space in self.spaces() {
			if space != self.info.personal && self.holds_here(space)? {
				spaces.push(space);
			}
		}
		let device = if self.takes_new_spaces()? {
			NewDevice::computer()
		} else {
			NewDevice::phone()
		};
		Ok(device.with_spaces(spaces))
	}

	/// Takes in what the copies beside this replica hold and it lacks, in the spaces that it
	/// holds: changes, the keys of statements and blobs, and sealed blobs, as a sync takes
	/// in what another replica sends, checking each. The copies' keys are not needed, and
	/// nothing of them is written to. `now` is the wall clock, read as each change comes in.
	///
	/// # Errors
	///
	/// Returns an error if what is taken in cannot be saved. A copy whose log does not read,
	/// or that a later version of NibKeep changed, is passed over.
	pub fn take_in_copies(&mut self, now: &dyn Fn() -> u64) -> Result<TakenIn, NodeError> {
		let copies: Vec<PathBuf> = self.copies().into_iter().map(|copy| copy.folder).collect();
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		self.take_in(&copies, &spaces, As::ThisReplica, now)
	}

	/// Takes in what `copies` hold of `spaces`, each change taken as `taken_as` says.
	fn take_in(
		&mut self,
		copies: &[PathBuf],
		spaces: &[SpaceId],
		taken_as: As,
		now: &dyn Fn() -> u64,
	) -> Result<TakenIn, NodeError> {
		let mut taken = TakenIn::default();
		// Private copies of the copies' logs go in this replica's own folder, never where
		// another user of the computer could have made a folder of the name first.
		let own = self.directory.clone();
		remove_private_folders(&own, "taking-in");
		let mut rng = Derived::seeded(&own, now());
		let mut syncing = Syncing::new(self, now);
		for copy in copies {
			// A copy of a later version, or whose engines file does not read, gives nothing.
			if crate::engines::check(copy).is_err() {
				continue;
			}
			let mut read = false;
			for &space in spaces {
				let log = log_path(copy, space);
				if !log.exists() {
					continue;
				}
				let private = private_folder(&own, "taking-in", &mut rng)?;
				let took = take_copied_log(&log, &private, &mut |item| {
					let new = take(&mut syncing, space, item, taken_as).map_err(Taking::Here)?;
					taken.items += usize::from(new);
					Ok::<(), Taking>(())
				});
				let _ = fs::remove_dir_all(&private);
				match took {
					Ok(()) => read = true,
					// A copy whose log does not read, as one damaged or not a log at all,
					// gives nothing, and the others are still read.
					Err(Taking::Copy) => {}
					// What this replica cannot save is an error of its own.
					Err(Taking::Here(error)) => return Err(error),
				}
				syncing.finish_space(space)?;
			}
			taken.copies += usize::from(read);
		}
		Ok(taken)
	}
}

impl Library {
	/// Whether a program that syncs folders syncs this library's folder, as the marks it
	/// leaves in the folder or above it show, or as the copies beside this replica do, when
	/// their files changed since this replica last looked: only the first time that it is
	/// found, so that the person is told once. NibKeep syncs on its own, and such a program
	/// puts every replica's key file on every computer and with its provider.
	///
	/// # Errors
	///
	/// Returns an error if what this replica saw cannot be saved.
	pub fn told_of_syncing(&self) -> Result<bool, NodeError> {
		let path = self.directory.join(SEEN);
		let before = Seen::read(&path);
		if before.told {
			return Ok(false);
		}
		let now = Seen {
			told: before.told,
			copies: self
				.copies()
				.iter()
				.filter_map(|copy| {
					let name = copy.folder.file_name()?.to_string_lossy().into_owned();
					Some((name, look(&copy.folder)))
				})
				.collect(),
		};
		let changed = now.copies.iter().any(|(name, looks)| {
			before
				.copies
				.iter()
				.any(|(was, looked)| was == name && looked != looks)
		});
		let marked = self.directory.ancestors().any(|folder| {
			MARKS.iter().any(|(mark, file)| {
				fs::metadata(folder.join(mark)).is_ok_and(|found| found.is_file() == *file)
			}) || fs::read_dir(folder)
				.into_iter()
				.flatten()
				.flatten()
				.any(|entry| {
					let name = entry.file_name().to_string_lossy().into_owned();
					name.starts_with(".sync_")
						&& Path::new(&name)
							.extension()
							.is_some_and(|extension| extension.eq_ignore_ascii_case("db"))
				})
		});
		let tell = !now.told && (changed || marked);
		let seen = Seen {
			told: now.told || tell,
			..now
		};
		if seen != before {
			write_file(&path, &cbor::to_vec(&seen))?;
		}
		Ok(tell)
	}
}

/// How the copies beside a replica looked, and whether the person was told that a program
/// syncs the folder. In CBOR, `[1, told, [[name, looks], …]]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seen {
	told: bool,
	copies: Vec<(String, [u8; 32])>,
}

impl Seen {
	/// What a replica saw before, or nothing if it saw nothing, or what it saw does not read.
	fn read(path: &Path) -> Self {
		fs::read(path)
			.ok()
			.and_then(|bytes| cbor::from_slice(&bytes).ok())
			.unwrap_or_default()
	}
}

impl ToCbor for Seen {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![
			1u8.to_cbor(),
			Value::Bool(self.told),
			Value::Array(
				self.copies
					.iter()
					.map(|(name, looks)| {
						Value::Array(vec![name.to_cbor(), Value::Bytes(looks.to_vec())])
					})
					.collect(),
			),
		])
	}
}

impl FromCbor for Seen {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "what copies looked like: [1, told, [[name, looks], …]]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [suite, told, copies] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		if u8::from_cbor(suite) != Ok(1) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		let Value::Array(copies) = copies else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let copies = copies
			.iter()
			.map(|copy| match copy {
				Value::Array(pair) => match pair.as_slice() {
					[name, Value::Bytes(looks)] => Ok((
						String::from_cbor(name)?,
						<[u8; 32]>::try_from(looks.as_slice())
							.map_err(|_| cbor::Error::Expected(EXPECTED))?,
					)),
					_ => Err(cbor::Error::Expected(EXPECTED)),
				},
				_ => Err(cbor::Error::Expected(EXPECTED)),
			})
			.collect::<Result<_, _>>()?;
		Ok(Self {
			told: bool::from_cbor(told)?,
			copies,
		})
	}
}

/// How a copy looks: a hash of the sizes and times of its files but its blobs and those it
/// was writing, which a program that syncs it changes as the replica it copies writes.
fn look(folder: &Path) -> [u8; 32] {
	let mut hasher = blake3::Hasher::new();
	let mut files = Vec::new();
	gather(folder, folder, &mut files);
	files.sort();
	for (path, size, written) in files {
		hasher.update(path.as_bytes());
		hasher.update(&size.to_le_bytes());
		hasher.update(&written.to_le_bytes());
	}
	*hasher.finalize().as_bytes()
}

/// The files in a copy's folder but its blobs and those it was writing, with their sizes and
/// when they were written.
fn gather(root: &Path, folder: &Path, files: &mut Vec<(String, u64, u128)>) {
	for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
		let path = entry.path();
		match entry.file_type() {
			Ok(kind) if kind.is_dir() => {
				if entry.file_name() != "blobs" && entry.file_name() != "tmp" {
					gather(root, &path, files);
				}
			}
			Ok(_) => {
				let Ok(metadata) = entry.metadata() else {
					continue;
				};
				let written = metadata
					.modified()
					.ok()
					.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
					.map_or(0, |since| since.as_nanos());
				let name = path
					.strip_prefix(root)
					.unwrap_or(&path)
					.to_string_lossy()
					.into_owned();
				files.push((name, metadata.len(), written));
			}
			Err(_) => {}
		}
	}
}

/// Opens a copy of a replica in `private`, a folder of the caller's own: its keys, which
/// `unlocker` opens, in a key file of that folder's, under a passphrase drawn anew, its
/// library file, and private copies of its logs, checked first as what another computer
/// wrote. The copy itself is only read.
fn open_privately(
	copy: &Path,
	unlocker: &Unlocker<'_>,
	private: &Path,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Library, NodeError> {
	let key_file = KeyFile::read(&copy.join(KEYS))?;
	let replica = key_file.replica(&key_file.unlock_replica(unlocker)?)?;
	let info = Info::read(&copy.join(INFO))?;
	fs::create_dir_all(private).map_err(NodeError::file(private))?;
	// The private folder is placed by its path, as the copy's folder is not here.
	declare_drive(private, Drive::default(), None);
	let directory = private.join("library");
	let folder = new_replica_folder(&directory, now(), rng)?;
	let mut passphrase = [0u8; 32];
	rng.fill_bytes(&mut passphrase);
	let passphrase = super::hex(&passphrase);
	let params =
		PassphraseParams::new(64, 1, 1).map_err(|error| NodeError::Damaged(error.to_string()))?;
	let (mut private_keys, compartment) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase(&passphrase, params),
		rng,
	)?;
	private_keys.write(&folder.join(KEYS))?;
	info.write(&folder.join(INFO))?;
	// Its spaces start empty, and take in what the copy's hold, checked as what another
	// replica sends: the copy's logs, which another computer may have written, are never
	// opened as logs of this computer's.
	let mut library = Library::open_spaces(&folder, private_keys, compartment, true)?;
	library.take_in_from(copy, now)?;
	library.finish_opening()?;
	Ok(library)
}

/// Adds a new replica, which `joining` asks for, in `directory`, as `sponsor` pairs with it,
/// in this process, through two pipes in memory: pairing, then the new replica's first sync.
fn pair_in_process(
	sponsor: &mut Library,
	directory: &Path,
	(joining, device): (Joining, &NewDevice),
	protection: &NewProtection<'_>,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<Library, NodeError> {
	let code = PairingCode::generate(rng);
	let (sponsor_end, joiner_end) = (sponsor.endpoint(), endpoint_of(&joining.endpoint_secret()));
	let (mut pairing, mut joining_end) = pipes(rng, sponsor_end, joiner_end);
	let (mut answering, mut syncing_end) = pipes(rng, sponsor_end, joiner_end);
	let mut sponsor_rng = Derived::from(rng);
	// The sponsor's side makes its changes at the time the pairing starts.
	let started = now();
	let sponsor_now = move || started;
	let sponsor_code = code.clone();
	std::thread::scope(|scope| {
		let sponsored = scope.spawn(move || -> Result<(), NodeError> {
			nibkeep_sync::sponsor(
				&mut pairing,
				&mut Sponsor::new(sponsor, &sponsor_now).for_device(device.clone()),
				&sponsor_code,
				&mut sponsor_rng,
			)?;
			nibkeep_sync::respond(&mut answering, &mut Syncing::new(sponsor, &sponsor_now))?;
			Ok(())
		});
		let joined = (|| {
			let grant = nibkeep_sync::join(&mut joining_end, &joining.request(), &code, rng)?;
			let mut library = joining.make(directory, &grant, protection, rng)?;
			let listings = library.pairing_listings(&grant);
			let synced = nibkeep_sync::joined(&mut joining_end, grant.clone(), listings)
				.map_err(NodeError::from)
				.and_then(|_| {
					let mut syncing = Syncing::new(&mut library, now).trusting(grant.sponsor);
					nibkeep_sync::initiate(&mut syncing_end, &mut syncing).map_err(NodeError::from)
				});
			match synced {
				Ok(_) => {
					// It takes the spaces given to it as it paired, then says, as the person
					// chose, whether it takes those given to it later, as a pairing over the
					// network does.
					let _ = library.settle_keys(now(), rng);
					let _ = library.take_paired_spaces(now(), rng);
					if grant.asks_first {
						let _ = library.set_takes_new_spaces(false, now(), rng);
					}
					Ok(library)
				}
				Err(error) => {
					library.remove_unsynced();
					Err(error)
				}
			}
		})();
		// So that the sponsor's side ends, if this side stopped early.
		drop((joining_end, syncing_end));
		let sponsored = sponsored
			.join()
			.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
		let library = joined?;
		sponsored?;
		Ok(library)
	})
}

/// Settles the user's spaces between the private copy and the new replica that it paired,
/// syncing them in this process: the copy gives the new replica its spaces, the new one asks
/// to be listed in them, and the copy lists it, so that it can take in the copy's logs of
/// every space before the copy's device is revoked.
fn spaces_in_process(
	sponsor: &mut Library,
	library: &mut Library,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<(), NodeError> {
	// What fails to settle leaves the spaces it concerns to the next sync, as pairing does;
	// only a failed sync stops it.
	for _ in 0..3 {
		let _ = sponsor.settle_spaces(now(), rng);
		sync_in_process(library, sponsor, now, rng)?;
		let _ = library.settle_keys(now(), rng);
		sync_in_process(library, sponsor, now, rng)?;
	}
	Ok(())
}

/// Syncs two libraries open in this process: `first` connects to `second`.
fn sync_in_process(
	first: &mut Library,
	second: &mut Library,
	now: &dyn Fn() -> u64,
	rng: &mut impl CryptoRng,
) -> Result<(), NodeError> {
	let (mut connecting, mut answering) = pipes(rng, first.endpoint(), second.endpoint());
	// Both sides at one time, as the closure is not shared between threads.
	let at = now();
	let answering_now = move || at;
	std::thread::scope(|scope| {
		let answered = scope.spawn(move || {
			let synced =
				nibkeep_sync::respond(&mut answering, &mut Syncing::new(second, &answering_now));
			drop(answering);
			synced
		});
		let synced = nibkeep_sync::initiate(&mut connecting, &mut Syncing::new(first, now));
		drop(connecting);
		let answered = answered
			.join()
			.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
		synced?;
		answered?;
		Ok(())
	})
}

/// One end of a pipe in memory, between two sides in one process.
struct Pipe {
	to: mpsc::Sender<Vec<u8>>,
	from: mpsc::Receiver<Vec<u8>>,
	/// A message taken from `from` to tell whether one had arrived, and not yet received.
	early: Option<Result<Vec<u8>, TransportError>>,
	binding: [u8; 32],
	own: [u8; 32],
	peer: [u8; 32],
}

/// The two ends of a pipe in memory, with a binding drawn anew: the first end is `first`'s,
/// and the second `second`'s.
fn pipes(rng: &mut impl CryptoRng, first: [u8; 32], second: [u8; 32]) -> (Pipe, Pipe) {
	let mut binding = [0u8; 32];
	rng.fill_bytes(&mut binding);
	let (to_second, from_first) = mpsc::channel();
	let (to_first, from_second) = mpsc::channel();
	(
		Pipe {
			to: to_second,
			from: from_second,
			early: None,
			binding,
			own: first,
			peer: second,
		},
		Pipe {
			to: to_first,
			from: from_first,
			early: None,
			binding,
			own: second,
			peer: first,
		},
	)
}

impl Transport for Pipe {
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
		self.to
			.send(message.to_vec())
			.map_err(|_| TransportError("the other side ended".to_owned()))
	}

	fn receive(&mut self) -> Result<Vec<u8>, TransportError> {
		self.early.take().unwrap_or_else(|| {
			self.from
				.recv()
				.map_err(|_| TransportError("the other side ended".to_owned()))
		})
	}

	fn has_message(&mut self) -> bool {
		if self.early.is_none() {
			self.early = match self.from.try_recv() {
				Ok(message) => Some(Ok(message)),
				Err(mpsc::TryRecvError::Empty) => None,
				Err(mpsc::TryRecvError::Disconnected) => {
					Some(Err(TransportError("the other side ended".to_owned())))
				}
			};
		}
		self.early.is_some()
	}

	fn binding(&self) -> [u8; 32] {
		self.binding
	}

	fn own_endpoint(&self) -> [u8; 32] {
		self.own
	}

	fn peer_endpoint(&self) -> [u8; 32] {
		self.peer
	}
}

/// Random numbers for a second side in one process: BLAKE3's extendable output, keyed
/// with 32 bytes of the caller's generator.
struct Derived(blake3::OutputReader);

impl Derived {
	/// Random enough for the names of private folders, where no caller's generator is at
	/// hand: a hash of the folder, the time, the process and the clock's nanoseconds. The
	/// names need only be new, as the folder they go in is this replica's own.
	fn seeded(folder: &Path, now: u64) -> Self {
		let mut hasher = blake3::Hasher::new();
		hasher.update(b"nibkeep 2026-10 private folders");
		hasher.update(folder.as_os_str().as_encoded_bytes());
		hasher.update(&now.to_le_bytes());
		hasher.update(&std::process::id().to_le_bytes());
		let unique = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map_or(0, |since| since.as_nanos());
		hasher.update(&unique.to_le_bytes());
		Self(hasher.finalize_xof())
	}

	fn from(rng: &mut impl CryptoRng) -> Self {
		let mut seed = [0u8; 32];
		rng.fill_bytes(&mut seed);
		Self(
			blake3::Hasher::new_keyed(&seed)
				.update(b"nibkeep 2026-10 second side")
				.finalize_xof(),
		)
	}
}

impl TryRng for Derived {
	type Error = Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Infallible> {
		let mut bytes = [0; 4];
		self.0.fill(&mut bytes);
		Ok(u32::from_le_bytes(bytes))
	}

	fn try_next_u64(&mut self) -> Result<u64, Infallible> {
		let mut bytes = [0; 8];
		self.0.fill(&mut bytes);
		Ok(u64::from_le_bytes(bytes))
	}

	fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
		self.0.fill(destination);
		Ok(())
	}
}

impl TryCryptoRng for Derived {}

/// Whom a change taken in from a copy is taken as from.
#[derive(Clone, Copy)]
enum As {
	/// This replica, of whose device the copy is: the space ends it only if it ends this one
	/// too.
	ThisReplica,
	/// Its author, as a backup's changes are: what a replica that the space ended made after
	/// that is left out.
	Authors,
}

/// Why taking in a copy's log stopped: the copy's log did not read, which passes the copy
/// over, or what it held could not be saved here, which is an error.
enum Taking {
	Copy,
	Here(NodeError),
}

impl From<nibkeep_store::StoreError> for Taking {
	fn from(_: nibkeep_store::StoreError) -> Self {
		Self::Copy
	}
}

/// Makes a new folder of this process's own in `parent`, under a name that starts with a dot
/// and is drawn at random, which only its owner reaches: for private copies of copies.
fn private_folder(
	parent: &Path,
	what: &str,
	rng: &mut impl CryptoRng,
) -> Result<PathBuf, NodeError> {
	let mut name = [0u8; 8];
	rng.fill_bytes(&mut name);
	let folder = parent.join(format!(".{what}-{}", super::hex(&name)));
	#[cfg(unix)]
	let builder = {
		let mut builder = fs::DirBuilder::new();
		std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
		builder
	};
	#[cfg(not(unix))]
	let builder = fs::DirBuilder::new();
	builder.create(&folder).map_err(NodeError::file(&folder))?;
	Ok(folder)
}

/// Removes the private folders that a crash left in a replica's own folder.
fn remove_private_folders(own: &Path, what: &str) {
	let prefix = format!(".{what}-");
	for entry in fs::read_dir(own).into_iter().flatten().flatten() {
		if entry.file_name().to_string_lossy().starts_with(&prefix) {
			let _ = fs::remove_dir_all(entry.path());
		}
	}
}

/// Takes in one item of a copy, as a sync takes in what another replica sends, a change as
/// from whom `taken_as` says; whether it was new.
fn take(
	syncing: &mut Syncing<'_>,
	space: SpaceId,
	item: CopiedItem,
	taken_as: As,
) -> Result<bool, NodeError> {
	match item {
		// A copy is of this device: what it holds is taken as from this replica, which the
		// space ends only if it ends this one too; a lost device's, as from their authors.
		CopiedItem::Change(parcel) => {
			let from = match taken_as {
				As::ThisReplica => syncing.verifying_key(),
				As::Authors => *parcel.change.author(),
			};
			syncing.receive_change(space, &from, *parcel)
		}
		CopiedItem::RecordKey(statement, key) => syncing.receive_record_key(space, statement, key),
		CopiedItem::BlobKey(sealed, key) => syncing.receive_blob_key(space, sealed, key),
		CopiedItem::Blob(sealed, path) => {
			// Read from the copy's file, and checked against its sealed hash as it is
			// written here: never linked to the copy's.
			let Ok(file) = fs::File::open(&path) else {
				return Ok(false);
			};
			let size = file.metadata().map_or(0, |metadata| metadata.len());
			let mut bytes: Box<dyn Read> = Box::new(file);
			syncing.receive_blob(space, sealed, size, &mut bytes)
		}
	}
}
