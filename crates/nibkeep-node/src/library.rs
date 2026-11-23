//! A library, as one replica keeps it: its keys, who it belongs to, and its spaces.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, Key, SigningKey, VerifyingKey};
use nibkeep_governance::{OWNER, SECRET_FIELD, SIGNING_KEY, SPACE};
use nibkeep_keys::{CompartmentKey, KeyFile, NewProtection, Protection, ReplicaKeys, Unlocker};
use nibkeep_log::SpaceKeys;
use nibkeep_model::schema::{FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::storage::Overlay;
use nibkeep_model::{
	FieldName, Languages, Operation, State, Statement, TypeDef, Types, Value, fields,
};
use nibkeep_store::{StoreError, StoredLog, Ticks, log_path};
use nibkeep_time::Timestamp;

use crate::engines;
use crate::info::Info;
use crate::library::entities::{referred, user_name};
use crate::place::{self, PLACE, PlaceFile, Verdict};
use crate::{MemberRole, NodeError};

pub(crate) mod accounts;
pub(crate) mod agents;
pub(crate) mod backups;
pub(crate) mod behind;
pub(crate) mod checking;
pub(crate) mod checkouts;
pub(crate) mod claims;
pub(crate) mod copies;
mod deleting_types;
pub(crate) mod direct;
pub(crate) mod dropping;
pub(crate) mod entities;
pub(crate) mod export;
pub(crate) mod files;
pub(crate) mod groups;
pub(crate) mod history;
pub(crate) mod holding;
pub(crate) mod import;
pub(crate) mod in_use;
pub(crate) mod json;
pub(crate) mod languages;
pub(crate) mod linked_copies;
pub(crate) mod links;
pub(crate) mod listing;
pub(crate) mod media;
pub(crate) mod members;
pub(crate) mod merge;
pub(crate) mod moving;
pub(crate) mod planner;
pub(crate) mod policies;
pub(crate) mod protection;
pub(crate) mod purge;
pub(crate) mod recovering;
pub(crate) mod recovery;
pub(crate) mod removal;
pub(crate) mod revocation;
pub(crate) mod secret_files;
pub(crate) mod secrets;
mod secrets_keys;
pub(crate) mod sources;
pub(crate) mod spaces;
pub(crate) mod sync;
pub(crate) mod taking;
pub(crate) mod thumbnails;
pub(crate) mod trees;
pub(crate) mod verify;
pub(crate) mod waiting;

/// The file of the library's keys.
const KEYS: &str = "keys.cbor";

/// The file that says who the library belongs to.
const INFO: &str = "library.cbor";

/// The file in a library's directory that says that the person asked for its replicas on a
/// drive without a journal (technical design: drives without a journal).
const FAT_ALLOWED: &str = "fat-allowed";

/// The folder of a library's replicas, each in a folder of its own, so that two computers
/// whose libraries a program syncs never write the same file (technical design: copies of a replica).
const REPLICAS: &str = "replicas";

/// How far ahead of this device's clock a change from another replica may be: ten minutes.
const MAX_DRIFT_MILLIS: u64 = 600_000;

/// A library, opened: its replica's keys unlocked, and its spaces open.
pub struct Library {
	/// The folder of the library's replica that is here.
	directory: PathBuf,
	/// The key file, as this library last read or wrote it. A change to it reads it again
	/// from the disk, under a lock, and so does reading its secrets, as another program may
	/// have changed it since.
	key_file: KeyFile,
	/// The key of the key file's replica compartment, which opens the library again after
	/// it was closed.
	compartment: CompartmentKey,
	/// The key of the key file's secrets compartment, while the secrets are unlocked: it
	/// unlocks them again when a closed library opens, and protects them anew.
	secrets_compartment: Option<CompartmentKey>,
	replica: ReplicaKeys,
	info: Info,
	spaces: BTreeMap<SpaceId, Space>,
	/// The space that the program chose for this session, if it did: new entities go there,
	/// and entities are looked for there alone ([`Library::choose_space`]).
	chosen: Option<SpaceId>,
	/// The types that NibKeep defines in code.
	code_types: Types,
	/// The languages that what the library shows is chosen by, read again whenever a
	/// change may change them.
	languages: Languages,
	/// The statements of the personal space that hold the lists of languages.
	language_statements: BTreeSet<StatementId>,
	/// The guard of a replica on a drive without a journal, which goes last, once the spaces
	/// are closed (technical design: drives without a journal).
	#[expect(dead_code, reason = "held for what it does as it goes")]
	pub(crate) in_use: Option<in_use::InUse>,
	/// The latest wall clock time, in milliseconds, that the library was given: the
	/// computer's when it opened, and each one that a change or a sync was given since. What a
	/// claim to the admin role gives now depends on it.
	pub(crate) wall: std::cell::Cell<u64>,
	/// For each space, the state mark and the wall clock at which this replica last brought
	/// its account of the space up to date ([`accounts`]).
	pub(crate) accounts_refreshed: std::cell::RefCell<BTreeMap<SpaceId, (StateMark, u64)>>,
	/// What reads pictures to make thumbnails, if the program brought it
	/// ([`Library::make_thumbnails_with`]).
	pub(crate) thumbnailer: Option<std::sync::Arc<dyn thumbnails::Thumbnailer>>,
	/// The AI client that the library acts as, if it does ([`Library::act_as_ai_agent`]):
	/// its key signs the library's changes.
	pub(crate) acting: Option<agents::Acting>,
}

/// The computer's wall clock, in milliseconds since the Unix epoch.
pub(crate) fn wall_clock() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |since| {
			u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
		})
}

/// Who and where a new library starts: the user's name, and the person's, the user's own
/// entity among the people, if it differs; this device's name, and the name of the app that
/// keeps the replica; and the languages the user reads, ranked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLibrary<'a> {
	/// The user's name.
	pub user: &'a str,
	/// The name of the person the user is, if not the user's, such as a full name beside an
	/// account's name.
	pub person: Option<&'a str>,
	/// This device's name.
	pub device: &'a str,
	/// The app's name, which the replica gets.
	pub app: &'a str,
	/// The languages the user reads, ranked; none if empty.
	pub languages: Languages,
}

impl<'a> NewLibrary<'a> {
	/// A new library for a user on a device, kept by an app, without languages.
	#[must_use]
	pub fn new(user: &'a str, device: &'a str, app: &'a str) -> Self {
		Self {
			user,
			person: None,
			device,
			app,
			languages: Languages::default(),
		}
	}

	/// The same, with the person the user is named apart from the user, if a name is given.
	#[must_use]
	pub fn with_person(mut self, person: Option<&'a str>) -> Self {
		self.person = person;
		self
	}

	/// The same, with the languages the user reads.
	#[must_use]
	pub fn with_languages(mut self, languages: Languages) -> Self {
		self.languages = languages;
		self
	}
}

/// A space of the library, open.
pub(crate) struct Space {
	stored: StoredLog,
	keys: SpaceKeys,
	/// Whether it is a group, as its first change says, once read.
	group: std::cell::Cell<Option<bool>>,
	/// The types of the space: those in code, combined with those that the space defines.
	/// Read when the space is opened, and again whenever a change touches what defines
	/// them, as reading them goes through every definition.
	types: Types,
}

/// What a library holds, as a person would want to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
	/// The user the library belongs to, named as history names them: by the person linked
	/// to the user in the personal space, or else by the user's own name.
	pub user: Described,
	/// This device.
	pub device: Described,
	/// The replica that the library is.
	pub replica: Described,
	/// The spaces, the personal space first, then in the order of their IDs.
	pub spaces: Vec<SpaceStatus>,
}

/// Where a space's state stands: [`Library::state_mark`]. Two marks of one library are the
/// same only while neither space took in anything between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateMark {
	space: SpaceId,
	/// The space's mark and the personal space's: the sequence and tag of the last row of
	/// the log file that each state file covers.
	marks: [Option<(i64, Vec<u8>)>; 2],
}

/// An entity and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
	/// The entity.
	pub id: EntityId,
	/// Its name, if it has one.
	pub name: Option<String>,
}

/// What a space holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceStatus {
	/// The space.
	pub id: SpaceId,
	/// Its name, if it has one.
	pub name: Option<String>,
	/// How many entities it holds, deleted ones and those merged into others aside.
	pub entities: usize,
	/// How many changes its log holds.
	pub changes: usize,
	/// Whether this replica's member left it or was removed: it keeps what it holds there,
	/// read only.
	pub left: bool,
}

/// Who made a change, as far as the space knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Author {
	/// The user, named as history names them: by the person linked to the user in the
	/// space, or else by the user's own name.
	pub user: Described,
	/// The person linked to the user in the space, if any.
	pub person: Option<EntityId>,
	/// The replica that signed the change.
	pub replica: EntityId,
	/// If the replica is an AI agent's, the AI agent's name, which is the replica's: history
	/// names its changes by the user and the AI agent.
	pub ai_agent: Option<String>,
}

/// How long opening a library took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Opening {
	/// Unlocking the replica's keys, which a passphrase makes slow on purpose.
	pub unlock: Duration,
	/// Opening the spaces: reading their logs, and building their indexes.
	pub spaces: Duration,
}

/// What a library's directory holds.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Found {
	/// No replica.
	Nothing,
	/// The replica that is here, in its folder: whole, or what is left of making it, keys
	/// without the library file.
	Here { folder: PathBuf, whole: bool },
	/// Only copies: replicas that are not here, or that a program may still be bringing in.
	Copies,
	/// Two or more replicas that are here.
	Several(Vec<PathBuf>),
}

impl Library {
	/// Starts a library in a directory that holds none: a new replica, named after the
	/// app that keeps it, with new keys protected as `protection` says, and the user's
	/// personal space, founded with the user, a person linked to the user, this device, the
	/// replica and the space itself, as `new` says. If starting fails, what was written is
	/// removed, keystore keys included.
	///
	/// # Errors
	///
	/// Returns an error if a name is empty, if the directory holds a library already, or if
	/// the keys or the space cannot be saved.
	pub fn create(
		directory: &Path,
		new: &NewLibrary<'_>,
		protection: &NewProtection<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Self, NodeError> {
		// Every name is written in full, so an empty one is refused before anything is made.
		if new.user.is_empty()
			|| new.person.is_some_and(str::is_empty)
			|| new.device.is_empty()
			|| new.app.is_empty()
		{
			return Err(NodeError::EmptyName);
		}
		// Copies of another library's replicas are left alone, and no library is started
		// among them.
		if found(directory) == Found::Copies {
			return Err(NodeError::Copied);
		}
		let folder = new_replica_folder(directory, wall_millis, rng)?;
		let in_use = in_use::InUse::take(&folder)?;
		let directory = folder.as_path();
		let keys_path = directory.join(KEYS);
		let personal = SpaceId::random(rng);
		let replica = ReplicaKeys {
			signing: SigningKey::generate(rng),
			decryption: DecryptionKey::generate(rng),
			storage: Key::generate(rng),
			spaces: BTreeMap::from([(personal, BTreeMap::from([(0, Key::generate(rng))]))]),
		};
		let (mut key_file, compartment) = match KeyFile::create(&replica, protection, rng) {
			Ok(created) => created,
			Err(error) => {
				drop(in_use);
				let _ = fs::remove_dir_all(directory);
				return Err(error.into());
			}
		};
		let code_types = code_types();
		let started = New {
			directory,
			replica: &replica,
			personal,
			library: new,
			code_types: &code_types,
		};
		let started = start(&started, &mut key_file, wall_millis, rng).and_then(|(info, space)| {
			let (settings, statements) = languages::read_lists(space.stored.log().state(), &info)?;
			Ok((info, space, settings.in_use(), statements))
		});
		match started {
			Ok((info, space, languages, language_statements)) => Ok(Self {
				directory: directory.to_owned(),
				key_file,
				compartment,
				secrets_compartment: None,
				replica,
				info,
				spaces: BTreeMap::from([(personal, space)]),
				chosen: None,
				code_types,
				languages,
				language_statements,
				in_use,
				wall: std::cell::Cell::new(wall_clock()),
				accounts_refreshed: std::cell::RefCell::new(BTreeMap::new()),
				acting: None,
				thumbnailer: None,
			})
			.map(|mut library| {
				// A phone asks before it takes a space given to it later, as the person may
				// change there; if saying so fails, it takes them, as a computer does
				// ([technical design](../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
				if cfg!(any(target_os = "android", target_os = "ios")) {
					let _ = library.set_takes_new_spaces(false, wall_millis, rng);
					library.end_bursts();
				}
				library
			}),
			Err(error) => {
				drop(in_use);
				let _ = fs::remove_file(&keys_path);
				let _ = fs::remove_dir_all(directory);
				key_file.discard();
				Err(error)
			}
		}
	}

	/// Whether the directory holds a library: a replica that is here, what is left of
	/// starting one, or copies of replicas made elsewhere.
	#[must_use]
	pub fn exists(directory: &Path) -> bool {
		found(directory) != Found::Nothing
	}

	/// Deletes a library's directory from this device, with the keys of its replicas that
	/// the platform's keystore keeps: for one that the person replaces, as a phone's app,
	/// which keeps one library, does as it joins another. No program may have it open.
	/// What other devices hold of it stays there.
	///
	/// # Errors
	///
	/// Returns an error if the directory cannot be removed. A key that the keystore does not
	/// delete stays there.
	pub fn discard(directory: &Path) -> Result<(), NodeError> {
		for entry in fs::read_dir(directory.join(REPLICAS))
			.into_iter()
			.flatten()
			.flatten()
		{
			if let Ok(key_file) = KeyFile::read(&entry.path().join(KEYS)) {
				key_file.discard();
			}
		}
		match fs::remove_dir_all(directory) {
			Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
				Err(NodeError::file(directory)(error))
			}
			_ => Ok(()),
		}
	}

	/// The folder, in a library's directory, of its replica that is here, whole or not.
	///
	/// # Errors
	///
	/// Returns an error if the directory holds no library, only copies, or several
	/// replicas that are here.
	pub fn replica_folder(directory: &Path) -> Result<PathBuf, NodeError> {
		match found(directory) {
			Found::Nothing => Err(NodeError::NoLibrary),
			Found::Here { folder, .. } => Ok(folder),
			Found::Copies => Err(NodeError::Copied),
			Found::Several(folders) => Err(NodeError::SeveralHere(folders)),
		}
	}

	/// Whether a new replica of a library can be made in `directory`, as making, joining or
	/// adding it from a copy would find, so that a program can tell before it asks the person
	/// for anything: not on a network drive, nor in a folder that a program takes to the
	/// cloud, nor on a drive without a journal unless the person allowed it.
	///
	/// # Errors
	///
	/// Returns [`NodeError::RemoteDrive`], [`NodeError::CloudFolder`] or
	/// [`NodeError::NoJournal`].
	pub fn check_place(directory: &Path) -> Result<(), NodeError> {
		let drive = place::whereabouts(directory).drive;
		local(&drive)?;
		journal(&drive, directory)
	}

	/// Allows the replicas of the library in `directory` on a drive without a journal, as FAT
	/// and exFAT, where a removal without unmounting can damage them later, as the person
	/// asked: they are then guarded, and a drive removed while one was open is checked
	/// before it is used again (technical design: drives without a journal).
	///
	/// # Errors
	///
	/// Returns [`NodeError::RemoteDrive`] or [`NodeError::CloudFolder`] for a folder that
	/// holds no library, before anything is written there, or an error if the file that says
	/// so cannot be written.
	pub fn allow_fat(directory: &Path) -> Result<(), NodeError> {
		check_local(directory)?;
		fs::create_dir_all(directory).map_err(NodeError::file(directory))?;
		let path = directory.join(FAT_ALLOWED);
		fs::write(&path, b"").map_err(NodeError::file(&path))
	}

	/// Says that the drive of the library in `directory` was checked, as by `fsck` or
	/// Windows' "Scan and fix", after it was removed, or lost its power, while the library
	/// was open: the library opens again. The caller then verifies it, as
	/// [`Library::verify`] does, before it is used (technical design: drives without a journal).
	///
	/// # Errors
	///
	/// Returns an error if the directory holds no replica here, or the guard's files cannot
	/// be written.
	pub fn drive_checked(directory: &Path) -> Result<(), NodeError> {
		let folder = Self::replica_folder(directory)?;
		// A replica that a later version changed is left as it is.
		engines::check(&folder)?;
		in_use::checked(&folder)
	}

	/// How the library's keys are protected, so that an app knows whether to ask for a
	/// passphrase before it opens the library.
	///
	/// # Errors
	///
	/// Returns an error if the directory holds no library, one whose start was cut short,
	/// or only copies, or its key file is damaged, and [`NodeError::UncleanRemoval`] if its
	/// drive must be checked before it opens, so that nothing is asked for in vain.
	pub fn protection(directory: &Path) -> Result<Protection, NodeError> {
		let (folder, key_file) = read_key_file(directory)?;
		if in_use::removed(&folder)? {
			return Err(NodeError::UncleanRemoval);
		}
		Ok(key_file.replica_protection().clone())
	}

	/// Opens a library: unlocks the replica's keys, and opens every space.
	///
	/// # Errors
	///
	/// Returns an error if the directory holds no library, or one whose start was cut
	/// short, `unlocker` does not unlock it, or a space cannot be opened.
	pub fn open(directory: &Path, unlocker: &Unlocker<'_>) -> Result<Self, NodeError> {
		Self::open_timed(directory, unlocker).map(|(library, _)| library)
	}

	/// Opens a library as [`Library::open`] does, and tells how long its two parts took.
	///
	/// # Errors
	///
	/// As [`Library::open`].
	pub fn open_timed(
		directory: &Path,
		unlocker: &Unlocker<'_>,
	) -> Result<(Self, Opening), NodeError> {
		let started = Instant::now();
		check_local(directory)?;
		let (folder, key_file) = read_key_file(directory)?;
		let compartment = key_file.unlock_replica(unlocker)?;
		// What a recovery in place that is done left: the old replica to set aside.
		recovering::tidy(&folder);
		let keys_unlocked = Instant::now();
		let library = Self::open_unlocked(&folder, key_file, compartment)?;
		Ok((
			library,
			Opening {
				unlock: keys_unlocked - started,
				spaces: keys_unlocked.elapsed(),
			},
		))
	}

	/// The library's directory, as [`Library::open`] takes it: the folder that holds its
	/// replicas.
	#[must_use]
	pub fn library_directory(&self) -> Option<&Path> {
		library_directory(&self.directory)
	}

	/// Closes the library, and keeps its replica's keys unlocked, and its secrets if they
	/// are, so that it opens again without a passphrase: for a program that waits for other
	/// replicas, or an app that opens the library for each operation, which should hold no
	/// lock on the spaces in between.
	#[must_use]
	pub fn close(self) -> Closed {
		Closed {
			directory: self.directory,
			compartment: self.compartment,
			secrets: self.secrets_compartment,
		}
	}

	/// Opens a library whose replica compartment `compartment` unlocks.
	fn open_unlocked(
		directory: &Path,
		key_file: KeyFile,
		compartment: CompartmentKey,
	) -> Result<Self, NodeError> {
		let mut library = Self::open_spaces(directory, key_file, compartment, false)?;
		// Once the replica is marked open, on a drive without a journal. A program that cannot
		// write the file, as while another program holds it for a moment, opens all the same,
		// and records itself the next time; a later version or a damaged file do not open.
		match engines::record(directory, wall_clock()) {
			Ok(()) | Err(NodeError::File { .. }) => {}
			Err(error) => return Err(error),
		}
		library.finish_opening()?;
		Ok(library)
	}

	/// Opens a library's spaces, and with `empty`, makes those whose logs are missing, as
	/// the private copy of a copy of a replica does before it takes in what the copy holds.
	/// [`Library::finish_opening`] comes next, once the spaces hold their changes.
	pub(crate) fn open_spaces(
		directory: &Path,
		key_file: KeyFile,
		compartment: CompartmentKey,
		empty: bool,
	) -> Result<Self, NodeError> {
		let in_use = in_use::InUse::take(directory)?;
		let info = Info::read(&directory.join(INFO))?;
		let replica = key_file.replica(&compartment)?;
		let code_types = code_types();
		let mut spaces = BTreeMap::new();
		for (&id, epochs) in &replica.spaces {
			let log = log_path(directory, id);
			if !empty && !log.exists() {
				return Err(NodeError::Damaged(format!(
					"the log of the space {id} is missing"
				)));
			}
			spaces.insert(
				id,
				Space::open(&log, id, epochs, &replica.storage, &code_types)?,
			);
		}
		if !spaces.contains_key(&info.personal) {
			return Err(NodeError::Damaged(
				"the keys of the personal space are missing".to_owned(),
			));
		}
		Ok(Self {
			directory: directory.to_owned(),
			key_file,
			compartment,
			secrets_compartment: None,
			replica,
			info,
			spaces,
			chosen: None,
			code_types,
			languages: Languages::default(),
			language_statements: BTreeSet::new(),
			in_use,
			wall: std::cell::Cell::new(wall_clock()),
			accounts_refreshed: std::cell::RefCell::new(BTreeMap::new()),
			acting: None,
			thumbnailer: None,
		})
	}

	/// Reads what opening reads once the spaces hold their changes: the lists of languages,
	/// and the keys that the replica writes with.
	pub(crate) fn finish_opening(&mut self) -> Result<(), NodeError> {
		self.refresh_languages()?;
		// A device that waits keeps what purges destroy before anything can purge here.
		let now = wall_clock();
		self.refresh_wait(now)?;
		self.expect_space_roots()?;
		// What a power cut left of the deletion of a dropped space, once its key went.
		self.finish_cut_short_drops();
		// Each space follows its groups before it takes or makes a change; one that cannot keeps
		// the library from opening no more than any unreadable space does, and the next sync
		// tries again. Then the groups say which first change each space that lacks one takes.
		let _ = self.follow_all_groups();
		let _ = self.expect_roots_from_groups();
		self.choose_writing_keys()?;
		// What a restore brought back whose giving back a crash cut short.
		let _ = self.take_back_restored(now, true);
		Ok(())
	}

	/// Reads the times that tell bursts of changes apart from `ticks`, in every space it has
	/// open, instead of the system's monotonic clock, as [`StoredLog::set_ticks`] does: tests
	/// give a clock of their own, so that the same changes form the same bursts on every
	/// machine.
	pub fn set_ticks(&mut self, ticks: &Ticks) {
		for space in self.spaces.values_mut() {
			space.stored.set_ticks(ticks.clone());
		}
	}

	/// The user's personal space.
	#[must_use]
	pub const fn personal_space(&self) -> SpaceId {
		self.info.personal
	}

	/// The fields whose values are secret in a space: those that the types of NibKeep, of
	/// apps or of the space mark as secret, those that statements governing the space make
	/// secret for good, and those that the standard vocabulary makes secret, in every space
	/// ([Data model](../../../docs/spec/model.md#9-secret-fields)). Whether a field is
	/// secret is decided here, not by a type's own definition of it.
	#[must_use]
	pub fn secret_fields(&self, space: SpaceId) -> Option<&BTreeSet<FieldName>> {
		self.spaces
			.get(&space)
			.map(|space| space.stored.secret_fields())
	}

	/// The fields that these types of a space list and that are secret there, as
	/// [`Library::secret_fields`] says, ordered by name: those whose values an entity of the
	/// types may hold sealed. A type of the standard vocabulary that the space does not define
	/// lists its fields as the vocabulary does.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space.
	pub fn secret_fields_of(
		&self,
		space: SpaceId,
		types: &[nibkeep_model::TypeName],
	) -> Result<Vec<FieldName>, NodeError> {
		let open = self.space(space)?;
		let secret = open.stored.secret_fields();
		let vocabulary = nibkeep_std::types();
		let listed: BTreeSet<&FieldName> = types
			.iter()
			.filter_map(|name| {
				open.types.get(name).or_else(|| {
					vocabulary
						.iter()
						.find(|definition| definition.name() == name)
				})
			})
			.flat_map(TypeDef::fields)
			.map(|(field, _)| field)
			.filter(|field| secret.contains(*field))
			.collect();
		Ok(listed.into_iter().cloned().collect())
	}

	/// The types of a space: those that NibKeep defines, combined with those of the space.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space.
	pub fn types(&self, space: SpaceId) -> Result<Types, NodeError> {
		Ok(self.space(space)?.types.clone())
	}

	/// Defines a type in a space, or adds to it what the space's types lack, as
	/// [`Types::define_in`] says, after [`Types::check`] found nothing wrong with it. Returns
	/// the space's types afterwards.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, the definition cannot be added as
	/// it is, or the change cannot be saved.
	pub fn define_type(
		&mut self,
		space: SpaceId,
		definition: &TypeDef,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Types, NodeError> {
		self.define_lacking(space, definition, wall_millis, rng)?;
		self.types(space)
	}

	/// Adds to a space what its types lack of a definition, as [`Library::define_type`] does,
	/// and returns how many statements of definitions it wrote: none if they lack nothing.
	pub(crate) fn define_lacking(
		&mut self,
		space: SpaceId,
		definition: &TypeDef,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let (operations, defined) = {
			let open = self.space(space)?;
			let types = &open.types;
			types.check(definition)?;
			let mut operations = types.define_in(definition, open.stored.log().state(), rng)?;
			if operations.is_empty() {
				return Ok(0);
			}
			let defined = operations.len();
			// The fields it makes secret are secret for good, which governs the space.
			let secret = definition
				.fields()
				.filter(|(_, field)| field.is_secret())
				.map(|(name, _)| name.clone());
			let own = self.own_record(space)?;
			operations.extend(open.secret_marks(&self.code_types, (own, secret), rng)?);
			(operations, defined)
		};
		self.commit(space, operations, wall_millis, rng)?;
		Ok(defined)
	}

	/// States, in a change that governs the space, that the fields its types make secret are
	/// secret for good, where no counting statement says so yet, as for types that an older
	/// copy of NibKeep defined.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the change cannot be saved.
	pub(crate) fn mark_secret_fields(
		&mut self,
		space: SpaceId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let own = self.own_record(space)?;
		let open = self.space(space)?;
		let secret = open.types.secret_fields();
		let operations = open.secret_marks(&self.code_types, (own, secret), rng)?;
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// What the library holds.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn status(&self) -> Result<Status, NodeError> {
		let (user, device) = self.user_and_device()?;
		let mut spaces = Vec::new();
		for (&id, space) in &self.spaces {
			spaces.push(space.status(
				id,
				&self.languages,
				&self.replica.signing.verifying_key(),
			)?);
		}
		spaces.sort_by_key(|space| (space.id != self.info.personal, space.id));
		Ok(Status {
			user,
			device,
			replica: self.described(self.info.replica)?,
			spaces,
		})
	}

	/// The user the library belongs to and this device, named as [`Library::status`] names
	/// them, without counting what the spaces hold, which costs much more in a large
	/// library.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn user_and_device(&self) -> Result<(Described, Described), NodeError> {
		let personal = &self.spaces[&self.info.personal];
		let state = personal.stored.log().state();
		let user = Described {
			id: self.info.user,
			name: user_name(state, &personal.types, &self.languages, self.info.user)?.0,
		};
		Ok((user, self.described(self.info.device)?))
	}

	/// An entity of the personal space with the name to show for it.
	fn described(&self, entity: EntityId) -> Result<Described, NodeError> {
		let personal = &self.spaces[&self.info.personal];
		Ok(Described {
			id: entity,
			name: entities::display_name(
				personal.stored.log().state(),
				&personal.types,
				&self.languages,
				entity,
			)?,
		})
	}

	/// Where a space's state stands, and the personal space's, whose lists of languages
	/// name entities: how far into its log file each state file goes. It moves whenever
	/// either space takes in anything, here or in another program, so a program may keep
	/// what it worked out from the space while the mark stays the same, as an app
	/// keeps a long list while a person pages through it.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or a state file cannot be read.
	pub fn state_mark(&self, space: SpaceId) -> Result<StateMark, NodeError> {
		let mark = |space: SpaceId| -> Result<_, NodeError> {
			let open = self.space(space)?;
			Ok(open
				.stored
				.log()
				.state()
				.storage()
				.ordinary()
				.mark()?
				.map(|mark| (mark.sequence, mark.tag)))
		};
		Ok(StateMark {
			space,
			marks: [mark(space)?, mark(self.info.personal)?],
		})
	}

	/// Who made the change that asserted a statement of a space: the replica whose key
	/// signed it, and the user it belongs to, as the space says. `None` if the space holds
	/// no such statement, or no replica of the space that is not deleted has the key that
	/// signed it now, or that replica has no owner.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the space cannot be read.
	pub fn author_of(
		&self,
		space: SpaceId,
		statement: StatementId,
	) -> Result<Option<Author>, NodeError> {
		let open = self.space(space)?;
		open.stored.reading(|stored| {
			let Some(key) = stored.log().author_of(statement)? else {
				return Ok(None);
			};
			self.author_with_key(open, stored, &key)
		})
	}

	/// Who holds a signing key in a space, as [`Library::author_of`] names them.
	pub(crate) fn author_with_key(
		&self,
		open: &Space,
		stored: &StoredLog,
		key: &nibkeep_crypto::VerifyingKey,
	) -> Result<Option<Author>, NodeError> {
		let author = Self::author_in(open, stored, stored.log().state(), key, &self.languages)?;
		if author.is_some() || !stored.log().has_members(&open.keys)? {
			return Ok(author);
		}
		// A member through a group, at any level, is named as the group on their path names
		// them, where this replica holds its log.
		let Some((group, user)) = stored.log().group_member_of(key, &open.keys)? else {
			return Ok(None);
		};
		let Some(in_group) = self.spaces.get(&group) else {
			return Ok(None);
		};
		let state = in_group.stored.log().state();
		let name = entities::display_name(state, &in_group.types, &self.languages, user)?;
		Ok(Some(Author {
			user: Described { id: user, name },
			person: None,
			replica: nibkeep_governance::replica_record(stored.log().space(), key),
			ai_agent: None,
		}))
	}

	/// Who holds a signing key in a space, named as a state gives them, for a reader of
	/// `languages`.
	pub(crate) fn author_in<S: nibkeep_model::storage::Statements>(
		open: &Space,
		stored: &StoredLog,
		state: &State<S>,
		key: &nibkeep_crypto::VerifyingKey,
		languages: &Languages,
	) -> Result<Option<Author>, NodeError>
	where
		NodeError: From<S::Error>,
	{
		let key_of = key;
		let key = Value::Text(key.to_text());
		// Of replicas that claim one key, which only a faulty replica makes, the first by ID
		// counts, so that every replica names the same author. What the state says of a
		// replica's key and owner can be taken away, as an AI agent's change may: the
		// governing changes that count say it all the same.
		// In a space with members, another member's governing change may state anyone's key
		// on a record of its own, so only what the counting listings say names the author,
		// and a link to a person names them only once an admin and they agree on it.
		if stored.log().has_members(&open.keys)? {
			let (_, owners) = members::users_and_owners(stored, &open.keys)?;
			let Some(&user) = owners.get(key_of.as_bytes()) else {
				return Ok(None);
			};
			let replica = nibkeep_governance::replica_record(stored.log().space(), key_of);
			let roles = stored.log().member_roles(&open.keys)?;
			let link = members::person_link((stored, state), (&owners, &roles), user)?;
			let linked = match link.holds {
				Some(person) => entities::display_name(state, &open.types, languages, person)?,
				None => None,
			};
			let (name, person) = match linked {
				Some(name) => (Some(name), link.holds),
				None => (
					entities::display_name(state, &open.types, languages, user)?,
					None,
				),
			};
			return Ok(Some(Author {
				user: Described { id: user, name },
				person,
				replica,
				ai_agent: Self::ai_agent_name(open, stored, state, (key_of, replica), languages)?,
			}));
		}
		let state_replica = stored.find(&SIGNING_KEY, &key)?.first().copied();
		let state_user = match state_replica {
			Some(replica) => referred(state, &open.types, languages, replica, &OWNER)?,
			None => None,
		};
		let (replica, user) = match (state_replica, state_user) {
			(Some(replica), Some(user)) => (replica, user),
			_ => match governed_record(stored, &open.keys, &key)? {
				Some(record) => record,
				None => return Ok(None),
			},
		};
		let (name, person) = user_name(state, &open.types, languages, user)?;
		let ai_agent = Self::ai_agent_name(open, stored, state, (key_of, replica), languages)?;
		Ok(Some(Author {
			user: Described { id: user, name },
			person,
			replica,
			ai_agent,
		}))
	}
}

impl Library {
	/// The AI agent's name, which its replica's is, if a key is an AI agent's.
	fn ai_agent_name<S: nibkeep_model::storage::Statements>(
		open: &Space,
		stored: &StoredLog,
		state: &State<S>,
		(key, replica): (&nibkeep_crypto::VerifyingKey, EntityId),
		languages: &Languages,
	) -> Result<Option<String>, NodeError>
	where
		NodeError: From<S::Error>,
	{
		if !stored.log().is_ai_agent(key, &open.keys)? {
			return Ok(None);
		}
		let name = entities::display_name(state, &open.types, languages, replica)?;
		Ok(Some(name.unwrap_or_else(|| "an AI agent".to_owned())))
	}
}

impl Library {
	/// A space of the library.
	pub(crate) fn space(&self, space: SpaceId) -> Result<&Space, NodeError> {
		self.spaces.get(&space).ok_or(NodeError::NoSpace(space))
	}

	/// Seals content as a blob of a space, unless the device holds it already, and returns
	/// its content hash. Content that another space of the replica holds is not sealed
	/// again: the space takes up that copy, once it has read it whole, unless it purged it
	/// ([technical design](../../../docs/Technical-design.md#79-blobs)).
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or reading the content or saving
	/// the blob fails.
	pub fn import_blob(
		&mut self,
		space: SpaceId,
		content: impl Read,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, NodeError> {
		self.seal_blob(space, (content, false), wall_millis, rng)
	}

	/// Seals content as a blob of a space, with a secret key if `secret` says so, unless the
	/// space holds it already with a key of that kind; or, if another space of the replica
	/// holds a sealed copy with a key of that kind, takes up that copy, the one with the
	/// smallest sealed hash first, and wraps its key for the space, once it has read it whole
	/// ([technical design: blobs]). A space that purged a copy seals the content anew. An AI
	/// agent's replica wraps no keys of other spaces, as it seals nothing.
	///
	/// [technical design: blobs]: ../../../docs/Technical-design.md#79-blobs
	pub(crate) fn seal_blob(
		&mut self,
		space: SpaceId,
		(content, secret): (impl Read, bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Hash, NodeError> {
		// Only with another space is there a copy to take up.
		let shares = self.spaces.len() > 1 && !self.is_ai_agent()?;
		let mut target = self
			.spaces
			.remove(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let others = &self.spaces;
		let elsewhere = |content: &Hash| -> Vec<(Hash, Key)> {
			if shares {
				held_copies_in(others.values(), content, secret)
			} else {
				Vec::new()
			}
		};
		let sealed = target.stored.import_blob_sharing(
			&target.keys,
			(content, secret),
			&elsewhere,
			wall_millis,
			rng,
		);
		self.spaces.insert(space, target);
		Ok(sealed?)
	}

	/// The spaces of the replica besides `space` that keep any of these contents, ordered by
	/// ID: those that keep a key, open or not, for a sealed copy that `space` keeps of it
	/// with a key of the kind given, secret or ordinary, and those that can read another
	/// copy of it with such a key now. A purge in `space` leaves the content to them, as
	/// each space destroys only its own keys.
	pub(crate) fn spaces_holding(
		&self,
		space: SpaceId,
		contents: &BTreeSet<Hash>,
		secret: bool,
	) -> Result<Vec<SpaceId>, NodeError> {
		let log = self.space(space)?.stored.log();
		let sealed: Vec<Hash> = contents
			.iter()
			.flat_map(|content| log.blob_keys(content))
			.filter(|(sealed, _)| log.is_secret_blob(sealed) == secret)
			.map(|(sealed, _)| *sealed)
			.collect();
		Ok(self
			.spaces
			.iter()
			.filter(|(id, open)| {
				let other = open.stored.log();
				**id != space
					&& (sealed
						.iter()
						.any(|sealed| !other.wrapped_blob_keys_of(sealed).is_empty())
						|| contents
							.iter()
							.any(|content| !open.stored.held_copies(content, secret).is_empty()))
			})
			.map(|(id, _)| *id)
			.collect())
	}

	/// Runs `run` as a batch of changes to a space, saved together when it returns, which
	/// makes many changes quicker to save ([technical design: saving in batches]). Until then, the changes it
	/// makes are read as any others, but a crash loses all of them. Whether `run` succeeds or
	/// fails, the changes it made to the space that were not refused are saved, and what it
	/// returned is returned; if saving fails, none of them is, and that error is returned. If
	/// `run` panics, none of them is saved. Changes to other spaces are saved one at a time,
	/// unless they are in a batch of their own; a space has one batch open at a time. What
	/// writes outside a change, such as importing a blob or a purge, is refused, and so is
	/// verifying the library.
	/// Each batch holds up the space's other writers until it ends, so it is kept short,
	/// such as a thousand changes.
	///
	/// [technical design: saving in batches]: ../../../docs/Technical-design.md#77-saving-in-batches
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, a batch of the space is open
	/// already, or the files fail, and otherwise the error of `run`.
	pub fn batch<T>(
		&mut self,
		space: SpaceId,
		run: impl FnOnce(&mut Self) -> Result<T, NodeError>,
	) -> Result<T, NodeError> {
		self.batch_as(space, false, run)
	}

	/// Runs `run` as a batch of changes to a space, as [`Library::batch`] does, but saves
	/// none of them if `run` fails: for what must be all or nothing, such as a move of files
	/// in a tree, whose copies are moved already.
	pub(crate) fn batch_whole<T>(
		&mut self,
		space: SpaceId,
		run: impl FnOnce(&mut Self) -> Result<T, NodeError>,
	) -> Result<T, NodeError> {
		self.batch_as(space, true, run)
	}

	fn batch_as<T>(
		&mut self,
		space: SpaceId,
		whole: bool,
		run: impl FnOnce(&mut Self) -> Result<T, NodeError>,
	) -> Result<T, NodeError> {
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let reloads = open.stored.reloads();
		open.stored.begin_batch(&open.keys)?;
		// The batch began on what others wrote, as it caught up: the changes in it are made
		// with the space's types as they are now.
		if open.stored.reloads() != reloads
			&& let Err(error) = self.read_again(space)
		{
			if let Some(open) = self.spaces.get_mut(&space) {
				open.stored.abandon_batch();
			}
			return Err(error);
		}
		let ran = catch_unwind(AssertUnwindSafe(|| run(self)));
		let ended = match (&ran, self.spaces.get_mut(&space)) {
			(Ok(Ok(_)), Some(open)) => open.stored.end_batch(),
			(Ok(Err(_)), Some(open)) if !whole => open.stored.end_batch(),
			(_, Some(open)) => {
				open.stored.abandon_batch();
				Err(StoreError::BatchLost)
			}
			(_, None) => Err(StoreError::BatchLost),
		};
		if ended.is_err() {
			// What was read of the batch's changes went with them. Reading it again fails only
			// when the storage does, which the batch's error tells already.
			let _ = self.read_again(space);
		}
		match (ran, ended) {
			(Err(panic), _) => resume_unwind(panic),
			(Ok(Err(error)), Err(StoreError::BatchLost)) => Err(error),
			(Ok(_), Err(error)) => Err(error.into()),
			(Ok(result), Ok(())) => result,
		}
	}

	/// Whether this replica is an AI agent's: whether the change that listed its key in the
	/// personal space marks it so ([technical design](../../../docs/Technical-design.md#14-ai-agents)).
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn is_ai_agent(&self) -> Result<bool, NodeError> {
		let open = self.space(self.info.personal)?;
		let own = self.author_key().verifying_key();
		Ok(open.stored.log().is_ai_agent(&own, &open.keys)?)
	}

	/// Refuses, with [`NodeError::AiAgentMayNot`] and `what`, if this replica is an AI agent's,
	/// before an app asks for more, such as pairing a device.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] if this replica is an AI agent's, and an error if
	/// the personal space cannot be read.
	pub fn check_not_ai_agent(&self, what: &'static str) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Err(NodeError::AiAgentMayNot(what));
		}
		Ok(())
	}

	/// Returns [`NodeError::Revoked`] if this replica was revoked in a space,
	/// [`NodeError::NotListed`] if no counting change lists it, and [`NodeError::Removed`] if
	/// its member left the space or was removed, or it is an AI agent's where the space
	/// allows none: the space would set aside any change of its.
	pub(crate) fn check_not_revoked(&self, space: SpaceId) -> Result<(), NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let own = self.author_key().verifying_key();
		if log.is_revoked(&own, &open.keys)? {
			return Err(NodeError::Revoked);
		}
		if !log.is_listed(&own, &open.keys)? {
			return Err(NodeError::NotListed);
		}
		if log.is_removed(&own, &open.keys)? {
			return Err(NodeError::Removed);
		}
		Ok(())
	}

	/// Whether this replica's member left a shared space or was removed from it, or, for an
	/// AI agent's replica, the space allows AI agents no more: it then keeps what it holds
	/// there, read only, and gets nothing added after. Never for the personal space.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub fn has_left(&self, space: SpaceId) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.is_removed(&self.author_key().verifying_key(), &open.keys)?)
	}

	/// The role that this replica's user has in a space now, which says what they may do
	/// there, as an app shows it: admin in the personal space; in a shared space, the role
	/// that its counting governing changes give this replica, through its member directly or
	/// through groups, or admin by a claim of theirs that took effect by the latest wall clock
	/// time the library was given; none while this replica is not listed there or holds
	/// nothing of the space yet, and once it is revoked, or its member left or was removed.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoSpace`] if the library holds no such space, and an error if it
	/// cannot be read.
	pub fn own_role(&self, space: SpaceId) -> Result<MemberRole, NodeError> {
		if space == self.info.personal {
			return Ok(MemberRole::Admin);
		}
		let own = self.author_key().verifying_key();
		let role = {
			let open = self.space(space)?;
			let log = open.stored.log();
			// A space other than the personal one has members, unless this replica holds
			// nothing of it yet.
			let Some(role) = log.key_role(&own, &open.keys)? else {
				return Ok(MemberRole::None);
			};
			if log.is_revoked(&own, &open.keys)? || log.is_removed(&own, &open.keys)? {
				return Ok(MemberRole::None);
			}
			role
		};
		if role == MemberRole::Editor
			&& let Some(member) = self.member_in(space)?
			&& self.claim_now(space, member)?.is_some()
		{
			return Ok(MemberRole::Admin);
		}
		Ok(role)
	}

	/// Reads again what the library keeps of a space in memory, from what was stored: its
	/// types, and for the personal space, the languages.
	fn read_again(&mut self, space: SpaceId) -> Result<(), NodeError> {
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		open.read_types(&self.code_types)?;
		if space == self.info.personal {
			self.refresh_languages()?;
		}
		Ok(())
	}

	/// The folder of the library's replica.
	#[must_use]
	pub fn directory(&self) -> &Path {
		&self.directory
	}

	/// A key derived from the replica's storage key with `context`, for a file of the
	/// replica's own beside its spaces.
	pub(crate) fn derived_key(&self, context: &'static str) -> nibkeep_crypto::Key {
		self.replica.storage.derive(context)
	}

	/// How many changes the replica keeps, in all its spaces: what grows when changes come
	/// into it, made here or received.
	///
	/// # Errors
	///
	/// Returns an error if a log cannot be read.
	pub fn changes_kept(&self) -> Result<usize, NodeError> {
		self.spaces
			.values()
			.try_fold(0, |sum, space| Ok(sum + space.stored.log().kept()?))
	}

	/// Catches up with what other programs, or other libraries of the replica open in this
	/// one, wrote to it since this one last read it ([technical design: several programs on one replica]): reads again what
	/// it keeps of each space in memory, such as its types, from each space that another
	/// wrote to. A change catches up by itself before it is made; a program that keeps the
	/// library open catches up before it reads what it keeps. Returns whether another wrote.
	///
	/// [technical design: several programs on one replica]: ../../../docs/Technical-design.md#78-several-programs-on-one-replica
	///
	/// # Errors
	///
	/// Returns [`NodeError::in_use`] errors if a space's state file must be made anew while
	/// another program has it open: the library is to be closed, and opened again later.
	pub fn catch_up(&mut self) -> Result<bool, NodeError> {
		let added = self.catch_up_spaces()?;
		let mut caught_up = added;
		let spaces: Vec<SpaceId> = self.spaces.keys().copied().collect();
		for space in spaces {
			caught_up |= self.catch_up_space(space)?;
		}
		// Once the personal space and the groups hold the entries that gave the spaces another
		// program added, as when the library opens.
		if added {
			let expected = self.expect_space_roots();
			let _ = self.expect_roots_from_groups();
			expected?;
		}
		Ok(caught_up)
	}

	/// Catches up with what others wrote to a space, as [`Library::catch_up`] does.
	fn catch_up_space(&mut self, space: SpaceId) -> Result<bool, NodeError> {
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let caught_up = open.stored.catch_up(&open.keys)?;
		if caught_up {
			self.read_again(space)?;
		}
		Ok(caught_up)
	}

	/// Commits a change to a space, signed by this replica, as [`Space::commit`] does, once
	/// the library has caught up with what others wrote to the space, which may have made
	/// more fields secret. If others write while the change is made, which it catches up
	/// with too, what the library keeps of the space is read again after it.
	///
	/// Its ordinary operations are saved as a change for each entity they are about, as every
	/// change is ([technical design](../../../docs/Technical-design.md#62-one-entity-per-change)), so
	/// that a purge of one entity states again no value of another.
	fn commit(
		&mut self,
		space: SpaceId,
		operations: Vec<Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.observe_wall(wall_millis);
		self.catch_up_space(space)?;
		let touched = space == self.info.personal && self.touches_languages(&operations);
		self.check_not_revoked(space)?;
		// A shared space whose secrets this replica does not hold sets them up, if it may.
		if space != self.info.personal
			&& !self.secrets_unlocked(space)
			&& let Some(secret) = self.secret_fields(space)
			&& operations.iter().any(|operation| {
				matches!(operation, Operation::Assert { statement, .. }
					if secret.contains(&statement.field))
			}) {
			self.set_up_space_secrets(space, wall_millis, rng)?;
		}
		// What every replica would set aside, an AI agent's does not make; nor does it take
		// away or delete the library's own records, which every replica would take.
		if self.is_ai_agent()? {
			self.check_ai_agents_operations(space, &operations)?;
		}
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let reloads = open.stored.reloads();
		open.commit(
			self.acting
				.as_ref()
				.map_or(&self.replica.signing, |acting| &acting.signing),
			&self.code_types,
			operations,
			wall_millis,
			rng,
		)?;
		// The change is saved by now. Reading the lists again fails only when the storage
		// does, which is reported all the same.
		if open.stored.reloads() != reloads {
			self.read_again(space)?;
		} else if touched {
			self.refresh_languages()?;
		}
		// A change to a group takes effect in the spaces it is in at their anchors; the next
		// sync anchors it where this fails, as the change is saved by now.
		if space != self.info.personal && self.space(space)?.is_group()? {
			let _ = self.after_group_change(space, wall_millis, rng);
		}
		Ok(())
	}

	/// Refuses, on an AI agent's replica, operations that every replica would set aside, as
	/// they govern, take secret values away, or write the library's own records; and those that
	/// every replica would take, as it decides from the payload alone, but that change the
	/// library's own records or a Git repository's record otherwise: that take their
	/// statements away, or delete, restore or merge them, as a clone trusts what remains of a
	/// repository's record that a person made, and statements of fields of NibKeep's own on any
	/// entity ([technical design](../../../docs/Technical-design.md#14-ai-agents)).
	fn check_ai_agents_operations(
		&self,
		space: SpaceId,
		operations: &[Operation],
	) -> Result<(), NodeError> {
		const RECORDS: &str = "change the library's own records, such as its types, devices, \
		                       languages and storage policies, or its Git repositories' records";
		let log = self.space(space)?.stored.log();
		let state = log.state();
		let kept = |entity: EntityId| -> Result<bool, NodeError> {
			let types = state.types(entity)?;
			Ok(fields::is_library_record(&types) || types.contains(&nibkeep_std::GIT_REPOSITORY))
		};
		for operation in operations {
			if log.governs(operation)? {
				return Err(NodeError::AiAgentMayNot(
					"change who may write to the library",
				));
			}
			let taken = match operation {
				Operation::Assert {
					statement,
					supersedes,
					..
				} => {
					if !fields::ai_agent_may_write(statement) {
						return Err(NodeError::AiAgentMayNot(RECORDS));
					}
					// What a deletion or a merge ends: the entity, and what it is merged into.
					let ended = match &statement.field {
						field if *field == fields::DELETED => [
							(statement.value == Value::Boolean(true)).then_some(statement.entity),
							None,
						],
						field if *field == fields::MERGED_INTO => {
							[Some(statement.entity), statement.value.as_reference()]
						}
						_ => [None, None],
					};
					for entity in ended.into_iter().flatten() {
						if kept(entity)? {
							return Err(NodeError::AiAgentMayNot(RECORDS));
						}
					}
					supersedes
				}
				Operation::Retract { statements, .. } => statements,
				Operation::RetractSecret { .. } => {
					return Err(NodeError::AiAgentMayNot("take secret values away"));
				}
				Operation::Unknown(_) => {
					return Err(NodeError::AiAgentMayNot(
						"write operations that this version does not know",
					));
				}
			};
			for id in taken {
				if let Some(statement) = state.statement(*id)?
					&& (!fields::ai_agent_may_take_away(&statement) || kept(statement.entity)?)
				{
					return Err(NodeError::AiAgentMayNot(RECORDS));
				}
			}
		}
		Ok(())
	}
}

impl Library {
	/// Notes a wall clock time that the library was given, if it is the latest.
	pub(crate) fn observe_wall(&self, wall_millis: u64) {
		self.wall.set(self.wall.get().max(wall_millis));
	}
}

/// A library that was closed, with its replica's keys still unlocked, and its secrets if
/// they were.
pub struct Closed {
	directory: PathBuf,
	compartment: CompartmentKey,
	secrets: Option<CompartmentKey>,
}

impl Closed {
	/// Opens the library again, as it is on the disk now. If its secrets were unlocked when
	/// it closed, it keeps their key, and [`Library::unlock_kept_secrets`] opens them when an
	/// operation needs them.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::open`]; an error of the keys if the key file's
	/// protection was changed since.
	pub fn open(&self) -> Result<Library, NodeError> {
		// A program of a later version may have opened the library since.
		engines::check(&self.directory)?;
		let key_file = KeyFile::read(&self.directory.join(KEYS))?;
		let mut library =
			Library::open_unlocked(&self.directory, key_file, self.compartment.clone())?;
		library.secrets_compartment.clone_from(&self.secrets);
		Ok(library)
	}

	/// Whether it keeps the key of the secrets, which were unlocked when it closed: another
	/// program may have protected them anew since, which only unlocking them tells.
	#[must_use]
	pub fn secrets_kept(&self) -> bool {
		self.secrets.is_some()
	}

	/// Locks the secrets: forgets their key.
	pub fn lock_secrets(&mut self) {
		self.secrets = None;
	}

	/// The library, closed, with its replica's keys but never its secrets', for a part of
	/// the program that never needs them, such as the holder of the sync role.
	#[must_use]
	pub fn without_secrets(&self) -> Self {
		Self {
			directory: self.directory.clone(),
			compartment: self.compartment.clone(),
			secrets: None,
		}
	}

	/// The folder of the library's replica.
	#[must_use]
	pub fn directory(&self) -> &Path {
		&self.directory
	}

	/// The library's directory, as [`Library::open`] takes it.
	#[must_use]
	pub fn library_directory(&self) -> Option<&Path> {
		library_directory(&self.directory)
	}
}

/// The library's directory of the replica in `folder`: the folder that holds its replicas,
/// each in `replicas/<name>/`.
fn library_directory(folder: &Path) -> Option<&Path> {
	let replicas = folder.parent()?;
	(replicas.file_name()? == REPLICAS).then_some(replicas.parent()?)
}

/// What a new library starts with.
struct New<'a> {
	directory: &'a Path,
	replica: &'a ReplicaKeys,
	personal: SpaceId,
	library: &'a NewLibrary<'a>,
	code_types: &'a Types,
}

/// Saves the keys, founds the personal space and writes the library file, which comes last:
/// a library without it was never finished.
fn start(
	new: &New<'_>,
	key_file: &mut KeyFile,
	wall_millis: u64,
	rng: &mut impl CryptoRng,
) -> Result<(Info, Space), NodeError> {
	let New {
		directory,
		replica,
		personal,
		library,
		code_types,
	} = *new;
	key_file.write(&directory.join(KEYS))?;
	#[cfg(test)]
	if tests::STARTS_FAIL.get() {
		return Err(NodeError::Damaged(
			"the start fails, as the test asks".to_owned(),
		));
	}
	let encryption = replica.decryption.encryption_key();
	let founding = nibkeep_governance::found(
		personal,
		(library.user, library.person.unwrap_or(library.user)),
		(library.device, library.app),
		&library.languages,
		(&replica.signing, &encryption),
		rng,
	);
	let mut space = Space::open(
		&log_path(directory, personal),
		personal,
		&replica.spaces[&personal],
		&replica.storage,
		code_types,
	)?;
	// The space installs the standard vocabulary as definitions of its own, so that every
	// device and app sees it, in the change that founds it.
	let mut operations = founding.operations;
	for definition in nibkeep_std::types() {
		operations.extend(code_types.define(&definition, rng));
	}
	// The fields it makes secret, such as passwords, are secret for good, which governs the
	// space: the first change says so, beside the replica's key.
	let secret = nibkeep_std::secret_fields().iter().cloned();
	#[cfg(test)]
	let secret = secret.filter(|_| !tests::FOUNDED_AS_BEFORE.get());
	let mut governing = founding.governing;
	governing.extend(space.secret_marks(code_types, (founding.replica, secret), rng)?);
	for operations in [governing, operations] {
		space.commit(&replica.signing, code_types, operations, wall_millis, rng)?;
	}
	let info = Info {
		user: founding.user,
		device: founding.device,
		replica: founding.replica,
		personal,
	};
	info.write(&directory.join(INFO))?;
	Ok((info, space))
}

impl Space {
	/// Opens a space of the library with its keys, and the types that NibKeep defines in
	/// code; the space's own types then decide which fields are secret.
	fn open(
		log: &Path,
		id: SpaceId,
		epochs: &BTreeMap<u64, Key>,
		storage: &Key,
		code_types: &Types,
	) -> Result<Self, NodeError> {
		let mut epochs = epochs.iter();
		let (&first, key) = epochs
			.next()
			.ok_or_else(|| NodeError::Damaged(format!("the space {id} has no keys")))?;
		let mut keys = SpaceKeys::new(first, key.clone());
		for (&epoch, key) in epochs {
			keys.insert(epoch, key.clone());
		}
		let mut stored = StoredLog::open(
			log,
			id,
			&keys,
			storage,
			MAX_DRIFT_MILLIS,
			code_types.secret_fields(),
		)?;
		// Every space of a library is founded by a change that says who may write to it.
		stored.expect_governed();
		let mut space = Self {
			stored,
			keys,
			group: std::cell::Cell::new(None),
			types: Types::new(),
		};
		space.read_types(code_types)?;
		Ok(space)
	}

	/// Commits what a writer asked for to the space, signed by `signing`: what governs the
	/// space, such as a replica's key, first, in a change of its own, as the log requires,
	/// which stays if the rest then fails; then a change for each entity, as
	/// [`Space::by_entity`] groups them, in one batch, all or none, or, in a batch the writer
	/// opened, saved or not as that batch saves its other changes. If the operations define
	/// types, the fields they make secret are secret before anything is committed, so that
	/// values written with them are sealed too, or refused before any of them is saved, and
	/// search treats the fields whose last writer wins as such. After a change that may touch
	/// the definitions of types, the types are read again from what was stored.
	fn commit(
		&mut self,
		signing: &SigningKey,
		code_types: &Types,
		operations: Vec<Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		// Each operation changes only the entity it names.
		let operations = self.scoped(operations)?;
		// Before anything is committed, as far as the fields secret now tell.
		self.check_sealed_for_secrets(&operations)?;
		// What governs the space makes a change of its own, before the rest.
		let mut governing = Vec::new();
		let mut operations_left = Vec::new();
		for operation in operations {
			if self.stored.log().governs(&operation)? {
				governing.push(operation);
			} else {
				operations_left.push(operation);
			}
		}
		if !governing.is_empty() {
			// The log wraps a governing change with the space's first key, which every
			// replica holds, so that every replica reads it.
			self.stored
				.commit(signing, &self.keys, governing, wall_millis, rng)?;
			if operations_left.is_empty() {
				return Ok(());
			}
		}
		// Content that the write's own definitions make secret is refused before any of it is
		// saved, as each change checks only what the space knows to be secret when it is made.
		if defines_types(&operations_left) {
			let types = self.types_with(code_types, &operations_left, wall_millis)?;
			self.refresh_fields(&types)?;
			let checked = self.check_sealed_for_secrets(&operations_left);
			self.read_types(code_types)?;
			checked?;
		}
		let changes = self.one_entity_each(operations_left)?;
		if changes.len() == 1 || self.stored.in_batch() {
			// One change; or several in the writer's own batch, which saves them, or not, as
			// it does its other changes.
			for operations in changes {
				self.commit_ordinary(signing, code_types, operations, wall_millis, rng)?;
			}
			return Ok(());
		}
		self.stored.begin_batch(&self.keys)?;
		let ran = catch_unwind(AssertUnwindSafe(|| -> Result<(), NodeError> {
			for operations in changes {
				self.commit_ordinary(signing, code_types, operations, wall_millis, rng)?;
			}
			Ok(())
		}));
		let ended = if matches!(ran, Ok(Ok(()))) {
			self.stored.end_batch().map_err(NodeError::from)
		} else {
			self.stored.abandon_batch();
			Ok(())
		};
		if !matches!((&ran, &ended), (Ok(Ok(())), Ok(()))) {
			// What was read of the definitions of the changes left out went with them. Reading
			// it again fails only when the storage does, which the error tells already.
			let _ = self.read_types(code_types);
		}
		match ran {
			Err(panic) => resume_unwind(panic),
			Ok(result) => result.and(ended),
		}
	}

	/// The ordinary operations of what a writer asked for, as changes about one entity each,
	/// as every change is ([the technical design's section on
	/// one entity per change](../../../docs/Technical-design.md#62-one-entity-per-change)), as [`Space::by_entity`] groups
	/// them.
	fn one_entity_each(
		&self,
		operations: Vec<Operation>,
	) -> Result<Vec<Vec<Operation>>, NodeError> {
		Ok(self
			.by_entity(operations)?
			.into_iter()
			.map(|(_, operations)| operations)
			.collect())
	}

	/// What a writer asked for, as operations that each change only the entity they name,
	/// as the log supersedes no statement of another entity ([Data
	/// model](../../../docs/spec/model.md#5-operations)): what an assertion replaces of
	/// another entity, such as one merged into its own, is retracted from that entity apart,
	/// right after it, and a retraction is split by the entities of the statements it names,
	/// as the space and the operations before it hold them. A statement that neither holds
	/// stays with the entity named, or is dropped from a retraction of the earlier form, which
	/// names none. A retraction of secret statements, which names none either, is split by
	/// the entities of the secret statements this replica opened, as retractions from each,
	/// which the log writes again as a retraction of secret statements; the rest is dropped,
	/// as it takes away no ordinary statement.
	pub(crate) fn scoped(&self, operations: Vec<Operation>) -> Result<Vec<Operation>, NodeError> {
		use nibkeep_model::storage::Statements;
		let storage = self.stored.log().state().storage();
		let mut placed = Vec::with_capacity(operations.len());
		for operation in operations {
			let Operation::RetractSecret { statements } = operation else {
				placed.push(operation);
				continue;
			};
			let mut by_entity: BTreeMap<EntityId, BTreeSet<StatementId>> = BTreeMap::new();
			for id in statements {
				if storage.is_secret(id)?
					&& let Some(recorded) = storage.get(id)?
				{
					by_entity
						.entry(recorded.statement.entity)
						.or_default()
						.insert(id);
				}
			}
			placed.extend(
				by_entity
					.into_iter()
					.map(|(entity, ids)| Operation::retract(entity, ids)),
			);
		}
		scope(placed, |id| {
			Ok(storage.get(id)?.map(|recorded| recorded.statement.entity))
		})
	}

	/// Ordinary operations, which each name their entity, as [`Space::scoped`] makes them,
	/// grouped by the entity each is about, in the order each entity first comes: first
	/// every definition of a type or a field, under `None`, which go together and which the
	/// rest may need, then the operations of each other entity. An operation that names none,
	/// of an unknown kind, goes with the operations before it.
	pub(crate) fn by_entity(&self, operations: Vec<Operation>) -> Result<ByEntity, NodeError> {
		let state = self.stored.log().state();
		// Whether each entity is a definition, found once for each.
		let mut definitions: BTreeMap<EntityId, bool> = operations
			.iter()
			.filter(|operation| defines_types(std::slice::from_ref(*operation)))
			.filter_map(|operation| match operation {
				Operation::Assert { statement, .. } => Some((statement.entity, true)),
				_ => None,
			})
			.collect();
		let mut key_of = |entity: EntityId| -> Result<Option<EntityId>, NodeError> {
			if let Some(&definition) = definitions.get(&entity) {
				return Ok((!definition).then_some(entity));
			}
			let types = state.types(entity)?;
			let definition = types.contains(&TYPE_DEFINITION) || types.contains(&FIELD_DEFINITION);
			definitions.insert(entity, definition);
			Ok((!definition).then_some(entity))
		};
		// `None` stands for the definitions.
		let mut groups: ByEntity = Vec::new();
		for operation in operations {
			let key = match operation.entity() {
				Some(entity) => Some(key_of(entity)?),
				None => None,
			};
			let at = match key {
				Some(key) => groups.iter().position(|(of, _)| *of == key),
				None => groups.len().checked_sub(1),
			};
			match (at, key) {
				(Some(at), _) => groups[at].1.push(operation),
				(None, key) => groups.push((key.flatten(), vec![operation])),
			}
		}
		// The definitions first.
		groups.sort_by_key(|(key, _)| key.is_some());
		Ok(groups)
	}

	/// Commits an ordinary change, about one entity.
	fn commit_ordinary(
		&mut self,
		signing: &SigningKey,
		code_types: &Types,
		operations: Vec<Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let defines = defines_types(&operations);
		let touches = defines || self.touches_definitions(&operations)?;
		if defines {
			let types = self.types_with(code_types, &operations, wall_millis)?;
			self.refresh_fields(&types)?;
		}
		// Fields that the change itself makes secret count too.
		if let Err(refused) = self.check_sealed_for_secrets(&operations) {
			if defines {
				self.read_types(code_types)?;
			}
			return Err(refused);
		}
		let committed = self
			.stored
			.commit(signing, &self.keys, operations, wall_millis, rng);
		// After a failure, the fields made secret for the change are the types' again.
		if (committed.is_ok() && touches) || (committed.is_err() && defines) {
			self.read_types(code_types)?;
		}
		committed?;
		Ok(())
	}

	/// The types of the space as `operations` would leave them, with the fields they make
	/// secret, tried on the state without changing it.
	fn types_with(
		&self,
		code_types: &Types,
		operations: &[Operation],
		wall_millis: u64,
	) -> Result<Types, NodeError> {
		let mut definitions: BTreeSet<EntityId> = self
			.stored
			.find(&fields::TYPE, &Value::Text(TYPE_DEFINITION.to_string()))?
			.into_iter()
			.collect();
		let mut state = State::in_storage(Overlay::new(self.stored.log().state().storage()));
		for operation in operations {
			state.apply(operation, Timestamp::new(wall_millis, 0))?;
			if let Operation::Assert { statement, .. } = operation
				&& statement.field == fields::TYPE
				&& statement.value.as_text() == Some(TYPE_DEFINITION.as_str())
			{
				definitions.insert(state.canonical(statement.entity)?);
			}
		}
		let mut types = code_types.clone();
		types.read_entities(&state, definitions)?;
		types.mark_secret(self.secret_for_good()?);
		Ok(types)
	}

	/// Refuses content in a secret field that is not sealed for the secrets, which every
	/// replica of the space could read: content of whose secret blob keys this replica has
	/// opened none, whether or not it holds the sealed copy yet.
	fn check_sealed_for_secrets(&self, operations: &[Operation]) -> Result<(), NodeError> {
		for operation in operations {
			if let Operation::Assert { statement, .. } = operation
				&& let Value::Blob(content) = &statement.value
				&& self.stored.secret_fields().contains(&statement.field)
				&& !self.stored.has_secret_blob_key(content)
			{
				return Err(NodeError::NotSealedForSecrets(statement.field.clone()));
			}
		}
		Ok(())
	}

	/// Reads the types of the space again: those in code, combined with those that the space
	/// defines. The fields that they make secret become the secret ones. The definitions are
	/// read in one transaction: with a transaction for each query, the 2,400 queries of the
	/// 11 types of generated data took the state file's lock 2,400 times, which made opening
	/// a library on a Windows laptop take twice as long, 22 ms rather than 10.
	fn read_types(&mut self, code_types: &Types) -> Result<(), NodeError> {
		let marked = self.secret_for_good()?;
		let types = self.stored.reading(|stored| -> Result<Types, NodeError> {
			let mut types = code_types.clone();
			let definitions =
				stored.find(&fields::TYPE, &Value::Text(TYPE_DEFINITION.to_string()))?;
			types.read_entities(stored.log().state(), definitions.iter().copied())?;
			types.mark_secret(marked);
			Ok(types)
		})?;
		self.refresh_fields(&types)?;
		self.types = types;
		Ok(())
	}

	/// The fields that are secret for good: those that counting statements governing the
	/// space make so, and the standard vocabulary's secret fields, which are secret in every
	/// space, whether it defines their types or not, and are marked in a space that lacks
	/// the marks at its next sync ([Data model](../../../docs/spec/model.md#9-secret-fields)).
	fn secret_for_good(&self) -> Result<BTreeSet<FieldName>, NodeError> {
		let mut secret = self.marked_secret()?;
		secret.extend(nibkeep_std::secret_fields().iter().cloned());
		Ok(secret)
	}

	/// The fields that counting statements governing the space make secret for good
	/// ([Data model](../../../docs/spec/model.md#9-secret-fields)).
	fn marked_secret(&self) -> Result<BTreeSet<FieldName>, NodeError> {
		Ok(self
			.stored
			.log()
			.governing_statements(&self.keys)?
			.into_iter()
			.filter_map(|governing| governing.counting_text(&SECRET_FIELD)?.parse().ok())
			.collect())
	}

	/// The operations that state, for good, that `secret` fields are secret: those of them
	/// that NibKeep and apps do not make secret in code, and that no counting statement
	/// names yet. They govern the space, and are stated on the record of `replica`, the one
	/// that states them, which every replica has, whatever else the state holds.
	fn secret_marks(
		&self,
		code_types: &Types,
		(replica, secret): (EntityId, impl IntoIterator<Item = FieldName>),
		rng: &mut impl CryptoRng,
	) -> Result<Vec<Operation>, NodeError> {
		let in_code = code_types.secret_fields();
		let marked = self.marked_secret()?;
		let missing: Vec<FieldName> = secret
			.into_iter()
			.filter(|field| {
				!in_code.contains(field) && !marked.contains(field) && !fields::is_reserved(field)
			})
			.collect();
		Ok(missing
			.into_iter()
			.map(|field| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					replica,
					SECRET_FIELD,
					Value::Text(field.to_string()),
				))
			})
			.collect())
	}

	/// Whether operations may change what the space's types are: whether they say anything
	/// of an entity that defines a type or a field, or of one merged into it, supersede or
	/// retract what one says, or merge an entity into one. A deleted entity still defines
	/// its types, so that restoring it brings them back; what is said of other entities,
	/// such as deleting or merging them, leaves the types as they are.
	fn touches_definitions(&self, operations: &[Operation]) -> Result<bool, NodeError> {
		let state = self.stored.log().state();
		// Each entity is looked at once: a change mostly says things of one entity.
		let mut known: BTreeMap<EntityId, bool> = BTreeMap::new();
		let mut defines = |entity: EntityId| -> Result<bool, NodeError> {
			if let Some(&defines) = known.get(&entity) {
				return Ok(defines);
			}
			let defines = entities::defines_types(state, entity)?;
			known.insert(entity, defines);
			Ok(defines)
		};
		for operation in operations {
			let Some(removed) = operation.superseded() else {
				continue;
			};
			if let Operation::Assert { statement, .. } = operation {
				if defines(statement.entity)? {
					return Ok(true);
				}
				if statement.field == fields::MERGED_INTO
					&& let Some(into) = statement.value.as_reference()
					&& defines(into)?
				{
					return Ok(true);
				}
			}
			for &id in removed {
				if let Some(statement) = state.statement(id)?
					&& defines(statement.entity)?
				{
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	/// Makes the fields that the types mark secret the secret ones, and tells search which
	/// fields show only the value written last.
	fn refresh_fields(&mut self, types: &Types) -> Result<(), NodeError> {
		let secret = types.secret_fields();
		let last_writer_wins = types.last_writer_wins_fields();
		if secret != *self.stored.secret_fields()
			|| last_writer_wins != *self.stored.last_writer_wins_fields()
		{
			self.stored.set_fields(secret, last_writer_wins)?;
		}
		Ok(())
	}

	/// What the space holds, its name as a reader of `languages` sees it, and whether the
	/// replica of key `own` left it.
	fn status(
		&self,
		id: SpaceId,
		languages: &Languages,
		own: &VerifyingKey,
	) -> Result<SpaceStatus, NodeError> {
		let state = self.stored.log().state();
		Ok(SpaceStatus {
			id,
			name: self.name(languages)?,
			entities: entities::things(state)?.len(),
			changes: self.stored.log().kept()?,
			left: self.stored.log().is_removed(own, &self.keys)?,
		})
	}

	/// The space's name, if it has one, as a reader of `languages` sees it.
	pub(crate) fn name(&self, languages: &Languages) -> Result<Option<String>, NodeError> {
		let state = self.stored.log().state();
		match self.own_entity()? {
			Some(entity) => entities::display_name(state, &self.types, languages, entity),
			None => Ok(None),
		}
	}

	/// The space's own entity, of the type `org.nibkeep.space`, if it has one: of several,
	/// the first by ID.
	pub(crate) fn own_entity(&self) -> Result<Option<EntityId>, NodeError> {
		for kind in [SPACE, nibkeep_governance::GROUP] {
			if let Some(entity) = self
				.stored
				.find(&fields::TYPE, &Value::Text(kind.to_string()))?
				.first()
			{
				return Ok(Some(*entity));
			}
		}
		Ok(None)
	}

	/// The hash of the space's first key, if the replica keeps it: the key that the entry
	/// which gave it the space held.
	pub(crate) fn first_key_hash(&self) -> Option<Hash> {
		self.keys
			.space_keys()
			.find(|(epoch, _)| *epoch == 0)
			.map(|(_, key)| nibkeep_crypto::key_hash(key))
	}

	/// Whether it is a group: a space with members and no data of its own, as its first
	/// change says, which only its founder writes; `false` until the replica holds it.
	pub(crate) fn is_group(&self) -> Result<bool, NodeError> {
		if let Some(group) = self.group.get() {
			return Ok(group);
		}
		let log = self.stored.log();
		let Some(root) = log.root()? else {
			return Ok(false);
		};
		let group = log
			.governing_statements(&self.keys)?
			.iter()
			.any(|governing| {
				governing.change == root
					&& governing.statement.field == fields::SPACE_KIND
					&& governing.statement.value.as_text() == Some(nibkeep_governance::GROUP_KIND)
			});
		self.group.set(Some(group));
		Ok(group)
	}
}

/// The replica that the counting governing changes of a space list `key`, in its text form,
/// for, and the user they say it belongs to, if they say both: who a key is, whatever the
/// state still holds of it.
fn governed_record(
	stored: &StoredLog,
	keys: &SpaceKeys,
	key: &Value,
) -> Result<Option<(EntityId, EntityId)>, NodeError> {
	let statements = stored.log().governing_statements(keys)?;
	let mut counting = statements.iter().filter(|governing| governing.counts);
	let Some(listing) = counting.clone().find(|governing| {
		governing.statement.field == SIGNING_KEY
			&& governing.lists
			&& governing.statement.value == *key
	}) else {
		return Ok(None);
	};
	let replica = listing.statement.entity;
	// In a space with members, the owner is the one that the key's own listing names, in
	// the change that lists it: another member could state an owner on the record.
	if stored.log().has_members(keys)? {
		return Ok(counting
			.filter(|governing| {
				governing.change == listing.change
					&& governing.statement.entity == replica
					&& governing.statement.field == fields::LISTING
			})
			.find_map(|governing| {
				let signed =
					nibkeep_governance::Listing::from_text(governing.statement.value.as_text()?)?;
				(signed.verifies() && signed.record == replica).then_some(signed.owner)
			})
			.map(|owner| (replica, owner)));
	}
	Ok(counting
		.find_map(|governing| {
			(governing.statement.entity == replica && governing.statement.field == OWNER)
				.then(|| governing.statement.value.as_reference())
				.flatten()
		})
		.map(|user| (replica, user)))
}

/// Operations grouped by the entity each is about, the definitions of types and fields under
/// `None`.
pub(crate) type ByEntity = Vec<(Option<EntityId>, Vec<Operation>)>;

/// Whether operations define types or fields.
fn defines_types(operations: &[Operation]) -> bool {
	operations.iter().any(|operation| {
		matches!(
			operation,
			Operation::Assert { statement, .. }
				if statement.field == fields::TYPE
					&& matches!(
						statement.value.as_text(),
						Some(name) if name == TYPE_DEFINITION.as_str() || name == FIELD_DEFINITION.as_str()
					)
		)
	})
}

/// The sealed copies of a content that these spaces hold and can read, with keys of the kind
/// given, secret or ordinary, smallest sealed hash first, each once.
pub(crate) fn held_copies_in<'a>(
	spaces: impl IntoIterator<Item = &'a Space>,
	content: &Hash,
	secret: bool,
) -> Vec<(Hash, Key)> {
	let mut copies: Vec<(Hash, Key)> = spaces
		.into_iter()
		.flat_map(|open| open.stored.held_copies(content, secret))
		.collect();
	copies.sort_by_key(|(sealed, _)| *sealed);
	copies.dedup_by_key(|(sealed, _)| *sealed);
	copies
}

/// The types that NibKeep defines in code.
fn code_types() -> Types {
	let mut types = Types::new();
	for definition in nibkeep_governance::types()
		.into_iter()
		.chain(policies::types())
		.chain(backups::types())
	{
		types.add(definition);
	}
	types
}

/// What a library's directory holds: of its replicas, which is here, as its place file says,
/// and which are copies. A library has both its key file and its library file, which is
/// written last. A replica's folder without a place file, or with one that does not read, is
/// a copy, which a program may still be bringing in, file by file. Folders whose names start
/// with a dot are new replicas' that are not finished, or replicas set aside. A replica that
/// a recovery brings back in place of the one here, until it is done, hides that one, so
/// that the two count as one library.
fn found(directory: &Path) -> Found {
	let Some(folders) = replica_folders(directory) else {
		return Found::Nothing;
	};
	let copies = folders.iter().any(|(_, here)| !here);
	let mut here: Vec<PathBuf> = folders
		.into_iter()
		.filter_map(|(folder, here)| here.then_some(folder))
		.collect();
	let replaced: Vec<PathBuf> = here
		.iter()
		.filter_map(|folder| recovering::replaced_by(folder))
		.collect();
	here.retain(|folder| !replaced.contains(folder));
	match here.len() {
		0 if copies => Found::Copies,
		0 => Found::Nothing,
		1 => {
			let folder = here.remove(0);
			let whole = folder.join(KEYS).exists() && folder.join(INFO).exists();
			Found::Here { folder, whole }
		}
		_ => Found::Several(here),
	}
}

/// The folders of the replicas in a library's directory, each with whether it is here, as
/// its place file says, but those whose names start with a dot: `None` if the directory has
/// no folder of replicas.
pub(crate) fn replica_folders(directory: &Path) -> Option<Vec<(PathBuf, bool)>> {
	let entries = fs::read_dir(directory.join(REPLICAS)).ok()?;
	let mut folders = Vec::new();
	for entry in entries.flatten() {
		let folder = entry.path();
		if entry.file_name().to_string_lossy().starts_with('.') || !folder.is_dir() {
			continue;
		}
		let verdict = PlaceFile::read(&folder.join(PLACE))
			.ok()
			.flatten()
			.map(|place| place.verdict(&place::whereabouts(&folder), &folder));
		folders.push((folder, verdict == Some(Verdict::Here)));
	}
	folders.sort();
	Some(folders)
}

/// Reads the key file of a library's replica that is here, and tells its folder.
fn read_key_file(directory: &Path) -> Result<(PathBuf, KeyFile), NodeError> {
	match found(directory) {
		Found::Nothing => Err(NodeError::NoLibrary),
		Found::Here { folder, whole } => {
			// Before any other file of the replica, which a later version may have changed:
			// one without the files of this version is not taken for one whose making was
			// cut short, which an app may offer to remove.
			engines::check(&folder)?;
			if !whole {
				return Err(NodeError::Unfinished);
			}
			// A recovery cut short left a replica that nobody knows, which a recovery run
			// again starts over.
			if recovering::unfinished(&folder) {
				return Err(NodeError::RecoveryCutShort);
			}
			let key_file = KeyFile::read(&folder.join(KEYS))?;
			Ok((folder, key_file))
		}
		Found::Copies => Err(NodeError::Copied),
		Found::Several(folders) => Err(NodeError::SeveralHere(folders)),
	}
}

/// Removes the folders of new replicas whose making this computer started and a crash cut
/// short, before they were renamed into place: those whose place file says they are here.
/// Another computer's, which a program that syncs the folder may bring, are left alone.
fn remove_unfinished_starts(replicas: &Path) {
	for entry in fs::read_dir(replicas).into_iter().flatten().flatten() {
		let name = entry.file_name().to_string_lossy().into_owned();
		let new = Path::new(&name)
			.extension()
			.is_some_and(|extension| extension == "new");
		if !(name.starts_with('.') && new) {
			continue;
		}
		let folder = entry.path();
		let ours = PlaceFile::read(&folder.join(PLACE))
			.ok()
			.flatten()
			.is_some_and(|place| {
				place.verdict(&place::whereabouts(&folder), &folder) == Verdict::Here
			});
		if ours {
			let _ = fs::remove_dir_all(&folder);
		}
	}
}

/// Bytes in lowercase hexadecimal, two digits each.
pub(crate) fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write as _;
	bytes.iter().fold(String::new(), |mut text, byte| {
		let _ = write!(text, "{byte:02x}");
		text
	})
}

/// Refuses a library's directory on a network drive, or in a folder whose files a program
/// may take away to the cloud.
pub(crate) fn check_local(directory: &Path) -> Result<(), NodeError> {
	local(&place::whereabouts(directory).drive)
}

/// Refuses a new replica in a library's directory on a drive without a journal, unless the
/// person allowed it there (technical design: drives without a journal).
fn journal(drive: &place::Drive, directory: &Path) -> Result<(), NodeError> {
	if drive.unjournaled && !directory.join(FAT_ALLOWED).exists() {
		return Err(NodeError::NoJournal);
	}
	Ok(())
}

/// As [`check_local`], on the drive that holds the directory.
fn local(drive: &place::Drive) -> Result<(), NodeError> {
	if drive.remote {
		return Err(NodeError::RemoteDrive);
	}
	if drive.cloud {
		return Err(NodeError::CloudFolder);
	}
	Ok(())
}

/// Makes the folder of a new replica in a library's directory that has none here, with its
/// place file and its engines file, which records this program as having made it at
/// `wall_millis`: under a temporary name first, so that the folder appears with them or not
/// at all. Copies of replicas made elsewhere may be there.
pub(crate) fn new_replica_folder(
	directory: &Path,
	wall_millis: u64,
	rng: &mut impl CryptoRng,
) -> Result<PathBuf, NodeError> {
	new_replica_folder_at(directory, (None, None), wall_millis, rng)
}

/// Makes the folder of a new replica as [`new_replica_folder`] does, at the computer's time,
/// with the file `mark` in it too, if given, holding its bytes, so that the folder appears
/// with it; beside the replica here in `replacing`, if given, which the mark names, as a
/// recovery in place makes it.
pub(crate) fn new_replica_folder_marked(
	directory: &Path,
	mark: Option<(&str, &[u8])>,
	replacing: Option<&Path>,
	rng: &mut impl CryptoRng,
) -> Result<PathBuf, NodeError> {
	new_replica_folder_at(directory, (mark, replacing), wall_clock(), rng)
}

/// Makes the folder of a new replica as [`new_replica_folder_marked`] does, at `wall_millis`.
fn new_replica_folder_at(
	directory: &Path,
	(mark, replacing): (Option<(&str, &[u8])>, Option<&Path>),
	wall_millis: u64,
	rng: &mut impl CryptoRng,
) -> Result<PathBuf, NodeError> {
	match found(directory) {
		Found::Here { folder, .. } if Some(folder.as_path()) == replacing => {}
		Found::Here { .. } | Found::Several(_) => return Err(NodeError::Exists),
		Found::Nothing | Found::Copies => {}
	}
	// Before anything is made, as a program that syncs the folder would take it to the cloud.
	let replicas = directory.join(REPLICAS);
	let here = place::whereabouts(&replicas);
	local(&here.drive)?;
	// A phone that gives no ID would make a replica that never opens again.
	if here.phone && here.computer.is_none() {
		return Err(NodeError::NoPhoneId);
	}
	journal(&here.drive, directory)?;
	fs::create_dir_all(&replicas).map_err(NodeError::file(&replicas))?;
	remove_unfinished_starts(&replicas);
	let mut name = [0u8; 8];
	rng.fill_bytes(&mut name);
	let name = hex(&name);
	let temporary = replicas.join(format!(".{name}.new"));
	fs::create_dir(&temporary).map_err(NodeError::file(&temporary))?;
	let folder = replicas.join(&name);
	let made = PlaceFile::new(&place::whereabouts_as(&temporary, &folder), rng)
		.create(&temporary.join(PLACE))
		.and_then(|()| engines::create(&temporary, wall_millis))
		.and_then(|()| match mark {
			Some((mark, bytes)) => crate::info::write_file(&temporary.join(mark), bytes),
			None => Ok(()),
		})
		.and_then(|()| fs::rename(&temporary, &folder).map_err(NodeError::file(&folder)));
	if let Err(error) = made {
		let _ = fs::remove_dir_all(&temporary);
		return Err(error);
	}
	#[cfg(unix)]
	let _ = fs::File::open(&replicas).and_then(|replicas| replicas.sync_all());
	// Another replica made at the same time makes two here: this one gives way. One that
	// it replaces counts as none, as its mark names it.
	if let Found::Several(_) = found(directory) {
		let _ = fs::remove_dir_all(&folder);
		return Err(NodeError::Exists);
	}
	Ok(folder)
}

/// Operations that each change only the entity they name, as [`Space::scoped`] says, where
/// `entity_of` finds the entity of a statement that the operations do not assert themselves.
pub(crate) fn scope(
	operations: Vec<Operation>,
	mut entity_of: impl FnMut(StatementId) -> Result<Option<EntityId>, NodeError>,
) -> Result<Vec<Operation>, NodeError> {
	// The entity of each statement these operations assert, which a later one may
	// supersede.
	let mut asserted: BTreeMap<StatementId, EntityId> = BTreeMap::new();
	let mut entity_of =
		|asserted: &BTreeMap<StatementId, EntityId>, id: StatementId| match asserted.get(&id) {
			Some(&entity) => Ok(Some(entity)),
			None => entity_of(id),
		};
	// Statements grouped by entity, in the order each first comes.
	let group =
		|groups: &mut Vec<(EntityId, BTreeSet<StatementId>)>, entity: EntityId, id: StatementId| {
			match groups.iter_mut().find(|(of, _)| *of == entity) {
				Some((_, ids)) => {
					ids.insert(id);
				}
				None => groups.push((entity, BTreeSet::from([id]))),
			}
		};
	let mut scoped = Vec::with_capacity(operations.len());
	for operation in operations {
		match operation {
			Operation::Assert {
				statement,
				supersedes,
				any_entity: false,
			} => {
				let mut own = BTreeSet::new();
				let mut others = Vec::new();
				for id in supersedes {
					match entity_of(&asserted, id)? {
						Some(entity) if entity != statement.entity => {
							group(&mut others, entity, id);
						}
						_ => {
							own.insert(id);
						}
					}
				}
				asserted.insert(statement.id, statement.entity);
				scoped.push(Operation::Assert {
					statement,
					supersedes: own,
					any_entity: false,
				});
				scoped.extend(
					others
						.into_iter()
						.map(|(entity, ids)| Operation::retract(entity, ids)),
				);
			}
			Operation::Retract { entity, statements } => {
				let mut groups = Vec::new();
				for id in statements {
					if let Some(of) = entity_of(&asserted, id)?.or(entity) {
						group(&mut groups, of, id);
					}
				}
				scoped.extend(
					groups
						.into_iter()
						.map(|(entity, ids)| Operation::retract(entity, ids)),
				);
			}
			// What a program writes is never of the earlier form; a retraction of secret
			// statements, which names none, is placed before.
			other @ (Operation::Assert { .. }
			| Operation::RetractSecret { .. }
			| Operation::Unknown(_)) => scoped.push(other),
		}
	}
	Ok(scoped)
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::PassphraseParams;
	use nibkeep_model::{FieldDef, Statement, TypeName, ValueKind};
	use nibkeep_testkit::{TestRng, millis_at};

	use super::*;

	thread_local! {
		/// Whether libraries that this thread starts are founded as versions before the
		/// vocabulary had secret fields founded them: without their marks.
		pub(super) static FOUNDED_AS_BEFORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

		/// Whether libraries that this thread starts fail once their keys are saved, as when
		/// the disk is full.
		pub(super) static STARTS_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
	}

	#[test]
	fn a_failed_start_leaves_nothing_behind() {
		static ONCE: std::sync::Once = std::sync::Once::new();
		ONCE.call_once(|| {
			let store: std::sync::Arc<keyring_core::CredentialStore> =
				keyring_core::mock::Store::new().expect("a mock store");
			keyring_core::set_default_store(store);
		});
		let directory = Removed(nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-node-failed-start-{}", std::process::id()),
		));
		let mut rng = TestRng::new("failed start");
		STARTS_FAIL.set(true);
		for protection in [
			NewProtection::Passphrase(
				"correct horse",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			NewProtection::Keystore {
				service: "org.nibkeep.test",
			},
		] {
			assert!(
				Library::create(
					&directory.0,
					&NewLibrary::new("Ada", "laptop", "nib"),
					&protection,
					millis_at("2026-10-12", 10, 0),
					&mut rng
				)
				.is_err()
			);
			let left: Vec<_> = fs::read_dir(directory.0.join(REPLICAS))
				.expect("lists")
				.collect();
			assert!(left.is_empty(), "no replica's folder is left");
			assert!(!Library::exists(&directory.0));
		}
		STARTS_FAIL.set(false);
		// Neither left a key in the keystore. The mock store lists the entries whose keys were
		// deleted, without the keys.
		let entries = keyring_core::get_default_store()
			.expect("a store")
			.search(&std::collections::HashMap::new())
			.expect("searches");
		assert!(
			entries.iter().all(|entry| entry.get_secret().is_err()),
			"no key is left in the keystore"
		);
	}

	/// A directory that goes when the test ends, also when it fails.
	struct Removed(PathBuf);

	impl Drop for Removed {
		fn drop(&mut self) {
			let _ = fs::remove_dir_all(&self.0);
		}
	}

	/// A library in a directory of its own, protected by a quick passphrase.
	fn library(name: &str, now: u64, rng: &mut TestRng) -> (Removed, Library) {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-node-{name}-{}", std::process::id()),
		);
		let protection = NewProtection::Passphrase(
			"correct horse",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		);
		let library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			now,
			rng,
		)
		.expect("creates");
		(Removed(directory), library)
	}

	/// The entity of a space that defines the type named `name`.
	fn definition_of(space: &Space, name: &str) -> EntityId {
		let state = space.stored.log().state();
		*space
			.stored
			.find(&fields::TYPE, &Value::Text(TYPE_DEFINITION.to_string()))
			.expect("finds")
			.iter()
			.find(|entity| {
				state
					.current(**entity, &fields::TYPE_NAME)
					.expect("reads")
					.iter()
					.any(|statement| statement.value.as_text() == Some(name))
			})
			.expect("the definition")
	}

	/// The operation that deletes an entity.
	fn delete(entity: EntityId, rng: &mut TestRng) -> Operation {
		Operation::assert(Statement::new(
			StatementId::random(rng),
			entity,
			fields::DELETED,
			Value::Boolean(true),
		))
	}

	/// The operation that merges an entity into another.
	fn merge(entity: EntityId, into: EntityId, rng: &mut TestRng) -> Operation {
		Operation::assert(Statement::new(
			StatementId::random(rng),
			entity,
			fields::MERGED_INTO,
			Value::Reference(into),
		))
	}

	/// A type of books with a title, defined in the personal space, and a book of it.
	fn a_book(library: &mut Library, now: u64, rng: &mut TestRng) -> (TypeDef, EntityId) {
		let personal = library.personal_space();
		let title: FieldName = "local.title".parse().expect("a field name");
		let book = TypeDef::new(TypeName::from_static("local.book"), 1)
			.with_field(title.clone(), FieldDef::new(ValueKind::Text));
		library
			.define_type(personal, &book, now, rng)
			.expect("defines");
		let novel = library
			.create_entity(
				personal,
				&[book.name().clone()],
				vec![(title, Value::Text("Lavinia".to_owned()))],
				now + 1_000,
				rng,
			)
			.expect("adds");
		(book, novel)
	}

	/// The ID of the statement that an operation asserts.
	fn asserted(operation: &Operation) -> StatementId {
		match operation {
			Operation::Assert { statement, .. } => statement.id,
			_ => panic!("an assertion"),
		}
	}

	/// The change that first recorded a statement of the personal space.
	fn change_of(library: &Library, id: StatementId) -> Option<Hash> {
		let space = library.space(library.personal_space()).expect("a space");
		nibkeep_model::storage::Statements::get(space.stored.log().state().storage(), id)
			.expect("reads")
			.expect("the statement")
			.change
	}

	#[test]
	fn what_a_writer_says_of_two_entities_is_saved_as_a_change_for_each() {
		let mut rng = TestRng::new("one entity per change");
		let now = millis_at("2026-11-03", 10, 0);
		let (_directory, mut library) = library("one entity per change", now, &mut rng);
		let personal = library.personal_space();
		let changes =
			|library: &Library| library.status().expect("reads the status").spaces[0].changes;
		let (a, b) = (EntityId::random(&mut rng), EntityId::random(&mut rng));
		let operations = vec![
			delete(a, &mut rng),
			delete(b, &mut rng),
			merge(a, b, &mut rng),
		];
		let ids: Vec<StatementId> = operations.iter().map(asserted).collect();
		let before = changes(&library);
		library
			.commit(personal, operations, now + 1_000, &mut rng)
			.expect("commits");
		assert_eq!(changes(&library), before + 2, "a change for each entity");
		assert_eq!(
			change_of(&library, ids[0]),
			change_of(&library, ids[2]),
			"both of A's"
		);
		assert_ne!(
			change_of(&library, ids[0]),
			change_of(&library, ids[1]),
			"B's apart"
		);
		// A retraction of statements of both is split too.
		library
			.commit(
				personal,
				vec![Operation::retract(a, [ids[0], ids[1]])],
				now + 2_000,
				&mut rng,
			)
			.expect("commits");
		assert_eq!(
			changes(&library),
			before + 4,
			"a retraction for each entity"
		);
		let space = library.space(personal).expect("a space");
		let storage = space.stored.log().state().storage();
		for id in [ids[0], ids[1]] {
			assert!(nibkeep_model::storage::Statements::is_superseded(storage, id).expect("reads"));
		}
	}

	#[test]
	fn content_refused_with_the_definition_of_its_secret_field_leaves_the_fields_as_they_were() {
		let mut rng = TestRng::new("content refused with its definition");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("refused with definition", now, &mut rng);
		let personal = library.personal_space();
		library
			.set_up_secrets(
				&NewProtection::Passphrase(
					"tr0ub4dor",
					PassphraseParams::new(64, 1, 1).expect("valid"),
				),
				now + 500,
				&mut rng,
			)
			.expect("sets up");
		let scan: FieldName = "local.scan".parse().expect("a field name");
		let document = TypeDef::new(TypeName::from_static("local.document"), 1)
			.with_field(scan.clone(), FieldDef::new(ValueKind::Blob).secret());
		// Content sealed as every blob is, which every replica of the space reads.
		let plain = library
			.import_blob(personal, &b"a scan"[..], now + 600, &mut rng)
			.expect("imports");
		let mut operations = library
			.types(personal)
			.expect("a space")
			.define(&document, &mut rng);
		operations.push(Operation::assert(Statement::new(
			nibkeep_base::StatementId::random(&mut rng),
			EntityId::random(&mut rng),
			scan.clone(),
			Value::Blob(plain),
		)));
		let committed = library.commit(personal, operations, now + 1_000, &mut rng);
		assert!(
			matches!(committed, Err(NodeError::NotSealedForSecrets(ref field)) if *field == scan),
			"{committed:?}"
		);
		// Nothing of the change counts: the type is not defined, nor its field secret.
		assert!(
			!library
				.secret_fields(personal)
				.expect("a space")
				.contains(&scan)
		);
		assert!(
			library
				.types(personal)
				.expect("a space")
				.get(document.name())
				.is_none()
		);
		drop(library);
	}

	#[test]
	fn content_refused_with_its_secret_field_in_a_batch_saves_none_of_the_write() {
		let mut rng = TestRng::new("refused in a batch");
		let now = millis_at("2026-11-04", 10, 0);
		let (_directory, mut library) = library("refused in a batch", now, &mut rng);
		let personal = library.personal_space();
		library
			.set_up_secrets(
				&NewProtection::Passphrase(
					"tr0ub4dor",
					PassphraseParams::new(64, 1, 1).expect("valid"),
				),
				now + 500,
				&mut rng,
			)
			.expect("sets up");
		let scan: FieldName = "local.scan".parse().expect("a field name");
		let document = TypeDef::new(TypeName::from_static("local.document"), 1)
			.with_field(scan.clone(), FieldDef::new(ValueKind::Blob).secret());
		let plain = library
			.import_blob(personal, &b"a scan"[..], now + 600, &mut rng)
			.expect("imports");
		let mut operations = library
			.types(personal)
			.expect("a space")
			.define(&document, &mut rng);
		operations.push(Operation::assert(Statement::new(
			StatementId::random(&mut rng),
			EntityId::random(&mut rng),
			scan.clone(),
			Value::Blob(plain),
		)));
		let changes =
			|library: &Library| library.status().expect("reads the status").spaces[0].changes;
		let before = changes(&library);
		// In a batch of the writer's, which saves what was made before a failure, the write
		// is refused before any of its changes is made.
		let committed = library.batch(personal, |library| {
			library.commit(personal, operations, now + 1_000, &mut rng)
		});
		assert!(
			matches!(committed, Err(NodeError::NotSealedForSecrets(ref field)) if *field == scan),
			"{committed:?}"
		);
		assert_eq!(changes(&library), before);
		assert!(
			library
				.types(personal)
				.expect("a space")
				.get(document.name())
				.is_none()
		);
	}

	#[test]
	fn a_retraction_of_what_the_same_write_asserts_goes_with_its_entity() {
		let mut rng = TestRng::new("retracted in the same write");
		let now = millis_at("2026-11-04", 10, 0);
		let (_directory, library) = library("retracted in the same write", now, &mut rng);
		let personal = library.personal_space();
		let a = EntityId::random(&mut rng);
		let deleting = delete(a, &mut rng);
		let id = asserted(&deleting);
		let title: FieldName = "local.title".parse().expect("a field name");
		let book = TypeDef::new(TypeName::from_static("local.book"), 1)
			.with_field(title, FieldDef::new(ValueKind::Text));
		let mut operations = vec![deleting];
		operations.extend(
			library
				.types(personal)
				.expect("a space")
				.define(&book, &mut rng),
		);
		operations.push(Operation::retract(a, [id]));
		let groups = library
			.space(personal)
			.expect("a space")
			.by_entity(operations)
			.expect("groups");
		// The definitions first, with no retraction; then the entity, with both.
		assert_eq!(groups.len(), 2);
		assert_eq!(groups[0].0, None);
		assert!(
			groups[0]
				.1
				.iter()
				.all(|operation| matches!(operation, Operation::Assert { .. }))
		);
		assert_eq!(groups[1].0, Some(a));
		assert_eq!(groups[1].1.len(), 2);
	}

	#[test]
	fn values_committed_with_their_secret_field_are_sealed() {
		let mut rng = TestRng::new("sealed with the definition");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("sealed", now, &mut rng);
		let personal = library.personal_space();
		let password: FieldName = "local.password".parse().expect("a field name");
		let login = TypeDef::new(TypeName::from_static("local.login"), 1)
			.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret());
		let mut operations = library
			.types(personal)
			.expect("a space")
			.define(&login, &mut rng);
		operations.push(Operation::assert(Statement::new(
			nibkeep_base::StatementId::random(&mut rng),
			EntityId::random(&mut rng),
			password,
			Value::Text("hunter2".to_owned()),
		)));
		// The value must be sealed with the space's secrets key, which there is none of yet:
		// committed as an ordinary value, it would have been stored in plain.
		let committed = library.commit(personal, operations, now + 1_000, &mut rng);
		assert!(
			matches!(
				committed,
				Err(NodeError::Store(nibkeep_store::StoreError::Log(
					nibkeep_log::LogError::SecretsLocked
				)))
			),
			"{committed:?}"
		);
		drop(library);
	}

	/// A retraction of secret statements, which names no entity, goes with the entity of each
	/// secret statement it takes away, and the log writes it again as such a retraction; it
	/// takes away no ordinary statement.
	#[test]
	fn a_retraction_of_secret_statements_goes_with_the_entity_of_each() {
		let mut rng = TestRng::new("secret retraction placed");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("placed", now, &mut rng);
		let personal = library.personal_space();
		library
			.set_up_secrets(
				&NewProtection::Passphrase(
					"tr0ub4dor",
					PassphraseParams::new(64, 1, 1).expect("valid"),
				),
				now + 500,
				&mut rng,
			)
			.expect("sets up");
		let (login, pin) = a_login();
		let mut operations = library
			.types(personal)
			.expect("a space")
			.define(&login, &mut rng);
		let (first, second) = (EntityId::random(&mut rng), EntityId::random(&mut rng));
		let mut pins = Vec::new();
		for (entity, value) in [(first, "1234"), (second, "5678")] {
			let statement = Statement::new(
				StatementId::random(&mut rng),
				entity,
				pin.clone(),
				Value::Text(value.to_owned()),
			);
			pins.push(statement.id);
			operations.push(Operation::assert(statement));
		}
		let note = Operation::assert(Statement::new(
			StatementId::random(&mut rng),
			first,
			"local.note".parse().expect("a field name"),
			Value::Text("a note".to_owned()),
		));
		let note_id = asserted(&note);
		operations.push(note);
		library
			.commit(personal, operations, now + 1_000, &mut rng)
			.expect("commits");
		let mut taking = pins.clone();
		taking.push(note_id);
		let space = library.space(personal).expect("a space");
		let scoped = space
			.scoped(vec![Operation::retract_secret(taking.clone())])
			.expect("scopes");
		let mut expected = vec![
			Operation::retract(first, [pins[0]]),
			Operation::retract(second, [pins[1]]),
		];
		expected.sort_by_key(Operation::entity);
		assert_eq!(scoped, expected);
		library
			.commit(
				personal,
				vec![Operation::retract_secret(taking)],
				now + 2_000,
				&mut rng,
			)
			.expect("commits");
		let state = library
			.space(personal)
			.expect("a space")
			.stored
			.log()
			.state();
		for id in pins {
			assert!(!state.is_current(id).expect("reads"));
		}
		assert!(state.is_current(note_id).expect("reads"));
	}

	#[test]
	fn only_what_touches_definitions_has_the_types_read_again() {
		let mut rng = TestRng::new("touches definitions");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("definitions", now, &mut rng);
		let personal = library.personal_space();
		let (book, novel) = a_book(&mut library, now, &mut rng);
		let space = library.space(personal).expect("a space");
		let definition = definition_of(space, "local.book");
		assert!(
			!space
				.touches_definitions(&[delete(novel, &mut rng)])
				.expect("reads"),
			"deleting an entity of a type leaves the types"
		);
		let deleting = delete(definition, &mut rng);
		assert!(
			space
				.touches_definitions(std::slice::from_ref(&deleting))
				.expect("reads"),
			"deleting a definition"
		);
		let Operation::Assert {
			statement: deleted, ..
		} = &deleting
		else {
			unreachable!("an assertion");
		};
		let restoring = Operation::retract(deleted.entity, [deleted.id]);
		library
			.commit(personal, vec![deleting.clone()], now + 2_000, &mut rng)
			.expect("deletes the definition");
		assert!(
			library
				.types(personal)
				.expect("a space")
				.get(book.name())
				.is_none(),
			"a deleted definition defines nothing"
		);
		// The deleted definition is found among none of the definitions, and still defines the
		// type once it is restored.
		let space = library.space(personal).expect("a space");
		assert!(
			space
				.touches_definitions(std::slice::from_ref(&restoring))
				.expect("reads"),
			"restoring a definition"
		);
		library
			.commit(personal, vec![restoring], now + 3_000, &mut rng)
			.expect("restores the definition");
		assert_eq!(
			library.types(personal).expect("a space").get(book.name()),
			Some(&book)
		);
		drop(library);
	}

	#[test]
	fn saying_something_of_an_entity_merged_into_a_definition_touches_it() {
		let mut rng = TestRng::new("merged into a definition");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("merged-definition", now, &mut rng);
		let personal = library.personal_space();
		let (_, novel) = a_book(&mut library, now, &mut rng);
		let title: FieldName = "local.title".parse().expect("a field name");
		let space = library.space(personal).expect("a space");
		let state = space.stored.log().state();
		let field = *space
			.stored
			.find(&fields::TYPE, &Value::Text(FIELD_DEFINITION.to_string()))
			.expect("finds")
			.iter()
			.find(|entity| {
				state
					.current(**entity, &fields::FIELD_NAME)
					.expect("reads")
					.iter()
					.any(|statement| statement.value.as_text() == Some(title.as_str()))
			})
			.expect("the field's definition");
		library
			.commit(
				personal,
				vec![merge(novel, field, &mut rng)],
				now + 2_000,
				&mut rng,
			)
			.expect("merges");
		let secret = Operation::assert(Statement::new(
			StatementId::random(&mut rng),
			novel,
			fields::FIELD_SECRET,
			Value::Boolean(true),
		));
		library
			.commit(personal, vec![secret], now + 3_000, &mut rng)
			.expect("makes the field secret");
		assert!(
			library
				.types(personal)
				.expect("a space")
				.secret_fields()
				.contains(&title),
			"the field is secret as soon as its definition says so"
		);
		drop(library);
	}

	#[test]
	fn merging_into_a_definition_touches_it() {
		let mut rng = TestRng::new("merging into a definition");
		let now = millis_at("2026-10-12", 10, 0);
		let (_directory, mut library) = library("merging", now, &mut rng);
		let personal = library.personal_space();
		let (book, novel) = a_book(&mut library, now, &mut rng);
		let other = library
			.create_entity(
				personal,
				&[book.name().clone()],
				Vec::new(),
				now + 2_000,
				&mut rng,
			)
			.expect("adds");
		let space = library.space(personal).expect("a space");
		let definition = definition_of(space, "local.book");
		assert!(
			!space
				.touches_definitions(&[merge(novel, other, &mut rng)])
				.expect("reads"),
			"merging two things leaves the types"
		);
		let merging = merge(novel, definition, &mut rng);
		assert!(
			space
				.touches_definitions(std::slice::from_ref(&merging))
				.expect("reads"),
			"merging into a definition"
		);
		let Operation::Assert {
			statement: merged, ..
		} = &merging
		else {
			unreachable!("an assertion");
		};
		let undoing = Operation::retract(merged.entity, [merged.id]);
		library
			.commit(personal, vec![merging], now + 3_000, &mut rng)
			.expect("merges");
		let space = library.space(personal).expect("a space");
		assert!(
			space
				.touches_definitions(std::slice::from_ref(&undoing))
				.expect("reads"),
			"undoing a merge into a definition"
		);
		drop(library);
	}

	/// A type of logins with a secret PIN.
	fn a_login() -> (TypeDef, FieldName) {
		let pin: FieldName = "local.pin".parse().expect("a field name");
		let login = TypeDef::new(TypeName::from_static("local.login"), 1)
			.with_field(pin.clone(), FieldDef::new(ValueKind::Text).secret());
		(login, pin)
	}

	#[test]
	fn a_field_once_secret_stays_secret_whatever_its_definition_says_later() {
		let mut rng = TestRng::new("secret for good");
		let now = millis_at("2026-10-26", 10, 0);
		let (_directory, mut library) = library("secret for good", now, &mut rng);
		let personal = library.personal_space();
		let (login, pin) = a_login();
		library
			.define_type(personal, &login, now + 1_000, &mut rng)
			.expect("defines");
		// The type's definition is deleted, and its field's mark taken away, as a change
		// that another replica forged could do.
		let space = library.space(personal).expect("a space");
		let definition = definition_of(space, "local.login");
		let state = space.stored.log().state();
		let fields_of_it: Vec<EntityId> = state
			.referrers(definition)
			.expect("reads")
			.into_iter()
			.map(|statement| statement.entity)
			.collect();
		let mut marks = Vec::new();
		for field in &fields_of_it {
			marks.extend(
				state
					.current(*field, &fields::FIELD_SECRET)
					.expect("reads")
					.iter()
					.map(|statement| statement.id),
			);
		}
		assert!(!marks.is_empty(), "the definition marks the field secret");
		library
			.commit(
				personal,
				vec![
					delete(definition, &mut rng),
					Operation::retract(fields_of_it[0], marks),
				],
				now + 2_000,
				&mut rng,
			)
			.expect("commits");
		let types = library.types(personal).expect("a space");
		assert!(types.get(login.name()).is_none(), "the type is gone");
		assert!(
			library
				.secret_fields(personal)
				.expect("a space")
				.contains(&pin),
			"its field stays secret"
		);
	}

	#[test]
	fn settling_marks_a_secret_field_that_an_older_copy_defined() {
		let mut rng = TestRng::new("secret marked later");
		let now = millis_at("2026-10-26", 10, 0);
		let (_directory, mut library) = library("secret marked later", now, &mut rng);
		let personal = library.personal_space();
		let (login, pin) = a_login();
		// An older copy of NibKeep defines the type without marking its field.
		let operations = library
			.space(personal)
			.expect("a space")
			.types
			.define(&login, &mut rng);
		library
			.commit(personal, operations, now + 1_000, &mut rng)
			.expect("defines");
		let marked = |library: &Library| {
			library
				.space(personal)
				.expect("a space")
				.marked_secret()
				.expect("reads")
		};
		// A new library marks the vocabulary's secret fields, and not the older copy's.
		let mut before = marked(&library);
		assert!(before.contains(&nibkeep_std::PASSWORD));
		assert!(!before.contains(&pin));
		library.settle_keys(now + 2_000, &mut rng).expect("settles");
		before.insert(pin.clone());
		assert_eq!(marked(&library), before);
		// Settling again marks nothing more.
		let changes = library.status().expect("a status").spaces[0].changes;
		library.settle_keys(now + 3_000, &mut rng).expect("settles");
		assert_eq!(
			library.status().expect("a status").spaces[0].changes,
			changes
		);
	}

	#[test]
	fn a_space_made_before_the_vocabulary_had_secrets_keeps_them_secret_and_marks_them() {
		let mut rng = TestRng::new("vocabulary secret");
		let now = millis_at("2026-10-26", 10, 0);
		FOUNDED_AS_BEFORE.set(true);
		let made = library("vocabulary secret", now, &mut rng);
		FOUNDED_AS_BEFORE.set(false);
		let (_directory, mut library) = made;
		let personal = library.personal_space();
		let vocabulary = nibkeep_std::secret_fields();
		let marked = |library: &Library| {
			library
				.space(personal)
				.expect("a space")
				.marked_secret()
				.expect("reads")
		};
		assert!(
			vocabulary
				.iter()
				.all(|field| !marked(&library).contains(field))
		);
		// Without the definition of logins either, as in a space made before them.
		let login = library
			.entities(personal, Some(&TYPE_DEFINITION))
			.expect("lists")
			.into_iter()
			.find(|listed| {
				let state = library
					.space(personal)
					.expect("a space")
					.stored
					.log()
					.state();
				state
					.current(listed.id, &fields::TYPE_NAME)
					.expect("reads")
					.iter()
					.any(|statement| statement.value == Value::Text(nibkeep_std::LOGIN.to_string()))
			})
			.expect("the definition of logins");
		// Deleted as an older copy could, which no app does.
		let deleted = Operation::assert(Statement::new(
			StatementId::random(&mut rng),
			login.id,
			fields::DELETED,
			Value::Boolean(true),
		));
		library
			.commit(personal, vec![deleted], now + 1_000, &mut rng)
			.expect("deletes");
		assert!(
			library
				.types(personal)
				.expect("types")
				.get(&nibkeep_std::LOGIN)
				.is_none()
		);

		// The vocabulary's secret fields are secret all the same: a password is sealed, an
		// entity of logins names it as secret, and purging it needs the secrets.
		let secret = library.secret_fields(personal).expect("a space");
		assert!(vocabulary.iter().all(|field| secret.contains(field)));
		let password = |text: &str| (nibkeep_std::PASSWORD, Value::Text(text.to_owned()));
		let refused = library.create_entity(
			personal,
			&[nibkeep_std::LOGIN],
			vec![password("hunter2")],
			now + 2_000,
			&mut rng,
		);
		assert!(refused.is_err(), "no secrets to seal it with");
		let quick = PassphraseParams::new(64, 1, 1).expect("valid");
		let secrets = NewProtection::Passphrase("tr0ub4dor", quick);
		library
			.set_up_secrets(&secrets, now + 1_500, &mut rng)
			.expect("sets up");
		let entity = library
			.create_entity(
				personal,
				&[nibkeep_std::LOGIN],
				vec![password("hunter2")],
				now + 3_000,
				&mut rng,
			)
			.expect("creates");
		// Of logins, every one but the content of secret files.
		let mut of_logins = vocabulary.to_vec();
		of_logins.retain(|field| *field != nibkeep_std::SECRET_CONTENT);
		assert_eq!(
			library
				.secret_fields_of(personal, &[nibkeep_std::LOGIN])
				.expect("lists"),
			of_logins
		);
		library.lock_secrets();
		assert!(matches!(
			library.purgeable(personal, entity, &nibkeep_std::PASSWORD, None),
			Err(NodeError::SecretsLocked)
		));

		// Settling marks them, for replicas of other versions.
		library.settle_keys(now + 4_000, &mut rng).expect("settles");
		assert!(
			vocabulary
				.iter()
				.all(|field| marked(&library).contains(field))
		);
	}

	#[test]
	fn entities_that_define_types_are_never_found() {
		let mut rng = TestRng::new("definitions unfound");
		let now = millis_at("2026-10-26", 10, 0);
		let (_directory, mut library) = library("definitions unfound", now, &mut rng);
		let personal = library.personal_space();
		let definition = library
			.entities(personal, Some(&TYPE_DEFINITION))
			.expect("lists")[0]
			.id;
		// Words on a definition, as a version that did not reserve labels indexed them.
		let words = Operation::assert(Statement::new(
			StatementId::random(&mut rng),
			definition,
			"local.note".parse().expect("a field"),
			Value::Text("zebra crossing".to_owned()),
		));
		library
			.commit(personal, vec![words], now + 1_000, &mut rng)
			.expect("says");
		nibkeep_testkit::assert_empty!(library.search(personal, "zebra").expect("searches"));
	}

	#[test]
	fn an_author_is_named_after_its_records_key_and_owner_are_taken_away() {
		let mut rng = TestRng::new("author taken away");
		let now = millis_at("2026-10-26", 10, 0);
		let (_directory, mut library) = library("author taken away", now, &mut rng);
		let personal = library.personal_space();
		let (_, book) = a_book(&mut library, now + 1_000, &mut rng);
		let title: FieldName = "local.title".parse().expect("a field name");
		let statement = {
			let state = library
				.space(personal)
				.expect("a space")
				.stored
				.log()
				.state();
			state.current(book, &title).expect("reads")[0].id
		};
		let named = |library: &Library| {
			library
				.author_of(personal, statement)
				.expect("reads")
				.and_then(|author| author.user.name)
		};
		assert_eq!(named(&library).as_deref(), Some("Ada"));
		// The record of this replica loses its key and owner, as an AI agent's change that
		// takes them away would make it.
		let replica = library.info.replica;
		let taken: Vec<StatementId> = {
			let state = library
				.space(personal)
				.expect("a space")
				.stored
				.log()
				.state();
			[SIGNING_KEY, OWNER]
				.iter()
				.flat_map(|field| state.current(replica, field).expect("reads"))
				.map(|statement| statement.id)
				.collect()
		};
		assert_eq!(taken.len(), 2);
		library
			.commit(
				personal,
				vec![Operation::retract(replica, taken)],
				now + 2_000,
				&mut rng,
			)
			.expect("commits");
		assert_eq!(named(&library).as_deref(), Some("Ada"));
	}
}
