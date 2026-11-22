//! Recovering a library on a new device, after its person lost every device, or the means to
//! unlock them: from a printed recovery code and a source, a backup or a library on this
//! computer that no longer unlocks ([technical design: recovery codes]). The new device opens
//! the personal space's first key from the source's recovery file, takes in what the source
//! holds, opens the keys sealed to the code, and lists itself by the code; then it takes the
//! person's other spaces and groups that the code is given, and lists itself in each, by the
//! code or through a group; it revokes the lost devices, but those that the person keeps, and
//! updates the backup. A library that no longer unlocks is read as a copy is, and never
//! written to; one in the folder where the new replica goes is set aside there, as the
//! recovery's last step.
//!
//! [technical design: recovery codes]: ../../../../docs/Technical-design.md#111-recovery-codes

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, Key, RecoveryCode, VerifyingKey, key_hash};
use nibkeep_governance::{
	Code, Listing, NewReplica, RecoveryListing, Role, SPACE_ENTRY, SpaceEntry, SpaceReplica,
	list_by_recovery_code, list_replica, recovery_record,
};
use nibkeep_keys::{KeyFile, NewProtection, ReplicaKeys};
use nibkeep_log::ChangeId;

use super::holding::HeldElsewhere;
use super::secrets::Compartment;
use super::sync::{Joining, Replica, endpoint_of};
use super::{Found, INFO, KEYS, Library, found, new_replica_folder_marked, replica_folders};
use crate::info::Info;
use crate::place::same_folder;
use crate::recovery_file::{OpenedRecovery, RECOVERY_FILE, RecoveryFile};
use crate::{NodeError, backup};

/// The file in the folder of a replica that a recovery code brings back, until the recovery
/// is done: the library does not open while it is there, and a recovery run again removes
/// that replica, which nobody knows, and starts over. A recovery in place of the replica here
/// writes that replica's folder's name in it, so that the two count as one library while the
/// old one is in its place.
pub(crate) const RECOVERING: &str = "recovering";

/// The file that a recovery in place writes beside its [`RECOVERING`] mark once the new
/// replica is whole: from then on it is the library, and opens. The replica it replaces stays
/// hidden behind the mark until it is set aside, which the recovery tries then, and the
/// library again as it opens; then the mark goes, and this file last.
const RECOVERED: &str = "recovered";

/// What starts the name under which a recovery in place sets aside the folder of the replica
/// it replaces, beside the new one: hidden, as a dot starts it, and never taken for a replica
/// or a copy.
const SET_ASIDE: &str = ".replaced-";

/// What a recovery did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovered {
	/// The replicas of the person's other devices that it revoked, as they were before.
	pub revoked: Vec<Replica>,
	/// The replicas of the person's other devices that it kept, as the person still has them.
	pub kept: Vec<Replica>,
	/// The spaces and groups where it also revoked replicas of the person's that the personal
	/// space it took did not list, as a backup holds that was last updated by a computer that
	/// learned of a device from another member alone.
	pub revoked_elsewhere: Vec<SpaceId>,
	/// The person's other spaces and groups that came back.
	pub spaces: Vec<SpaceId>,
	/// The spaces and groups given to the code that the backup holds nothing of, which did
	/// not come back.
	pub missing: Vec<SpaceId>,
	/// The spaces of the person's that the devices it revoked held and that did not come
	/// back, each with the devices it kept that hold it, which revoke them there after their
	/// next sync with this one: one that none holds stays with the revoked devices
	/// ([technical design](../../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	pub out_of_reach: Vec<HeldElsewhere>,
	/// What became of the secrets.
	pub secrets: RecoveredSecrets,
	/// What it recovered from.
	pub from: RecoveredFrom,
}

/// What a recovery took the library from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredFrom {
	/// A backup.
	Backup {
		/// Its name, if the personal space still names it.
		name: Option<String>,
		/// Whether the recovery updated it, which then carries the new device: an update that
		/// fails, or that another program holds off, leaves it to the next one.
		updated: bool,
	},
	/// A library on this computer that no longer unlocked.
	Library {
		/// The folder of its replica that the recovery read, as it was then.
		folder: PathBuf,
		/// Its replica, as its folder names it, which the recovery revoked.
		replica: Option<EntityId>,
		/// Whether it was the library where the new replica went, which the new one replaces;
		/// otherwise it is as it was.
		replaced: bool,
		/// Where its folder was set aside, beside the new replica's, if it was replaced:
		/// `None` if it could not be set aside yet, as when another program has a file of it
		/// open, which the library tries again as it opens. Its keys, and what it holds, are
		/// of no more use: the person deletes it.
		set_aside: Option<PathBuf>,
	},
}

/// What became of the library's secrets in a recovery, which takes those that the code
/// holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveredSecrets {
	/// The library had no secrets.
	NoneHeld,
	/// The library had secrets, and the code was made without them: they went with the lost
	/// devices.
	NotInCode,
	/// The code held them, and this device holds them now, protected as the person chose:
	/// this many secrets keys.
	Taken {
		/// How many secrets keys it took, of every space.
		keys: usize,
	},
	/// The code held them, and the person left them: this device holds none.
	Left,
}

/// A recovery under way, which [`Library::start_recovery`] began: the new replica, which
/// nobody knows yet, holds what the backup holds of the personal space, and the keys sealed
/// to the code there. [`Recovering::holds_secrets`] says whether the code holds the
/// secrets, so that the person is asked how to protect them only then;
/// [`Recovering::others`] lists the person's other devices, which [`Recovering::keep`]
/// keeps some of; and [`Recovering::finish`] does the rest. One dropped before it finishes
/// removes the new replica.
pub struct Recovering<'a> {
	/// The new replica, until the recovery finishes.
	library: Option<Library>,
	/// The [`RECOVERING`] mark in its folder, locked while the recovery runs.
	mark: Option<fs::File>,
	source: Source<'a>,
	/// The folder of the replica here that the new one replaces, which is set aside last.
	replacing: Option<PathBuf>,
	code: &'a RecoveryCode,
	new: NewRecords,
	holds_secrets: bool,
	/// The replicas of the person's other devices that the recovery keeps.
	keep: BTreeSet<EntityId>,
	/// The replicas that it revokes in any case: the one of the library that no longer
	/// unlocks that it reads, and the one that a recovery cut short left here.
	revoked_anyway: BTreeSet<EntityId>,
}

impl Recovering<'_> {
	/// Whether the code holds the secrets, which [`Recovering::finish`] takes then.
	#[must_use]
	pub const fn holds_secrets(&self) -> bool {
		self.holds_secrets
	}

	/// The replicas of the person's other devices, which [`Recovering::finish`] revokes as
	/// lost, but those that [`Recovering::keep`] keeps: every replica of the person's that
	/// is not revoked yet, but two that it revokes in any case: the one of the library that
	/// no longer unlocks that the recovery reads, if it reads one, and the one that a
	/// recovery cut short left in the same place, which the backup may list.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn others(&self) -> Result<Vec<Replica>, NodeError> {
		let library = self.library.as_ref().ok_or(NodeError::NoLibrary)?;
		Ok(library
			.replicas()?
			.into_iter()
			.filter(|replica| {
				!replica.this && !replica.revoked && !self.revoked_anyway.contains(&replica.id)
			})
			.collect())
	}

	/// The replicas of [`Recovering::others`] that `target` names, as
	/// [`Library::replicas_named`] finds them among them alone: those of a device, by its
	/// name or ID, one replica by its ID, or an AI agent's by its name.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichDevice`] if `target` names none of them, or several, as
	/// the library that the recovery reads, or a device revoked already, and an error if the
	/// personal space cannot be read.
	pub fn others_named(&self, target: &str) -> Result<Vec<Replica>, NodeError> {
		let library = self.library.as_ref().ok_or(NodeError::NoLibrary)?;
		let error = match super::revocation::named_among(self.others()?, target) {
			Ok(named) => return Ok(named),
			Err(error) => error,
		};
		// Said apart: what the target names among the person's other replicas, if anything.
		let Ok(named) = library.replicas_named(target) else {
			return Err(error);
		};
		let why = if named
			.iter()
			.any(|replica| self.source.replica() == Some(replica.id))
		{
			"is the library that no longer unlocks, which the recovery revokes in any case"
		} else if named
			.iter()
			.any(|replica| self.revoked_anyway.contains(&replica.id))
		{
			"was left by a recovery cut short, which this one revokes in any case"
		} else if !named.is_empty() && named.iter().all(|replica| replica.revoked) {
			"is revoked already"
		} else {
			return Err(error);
		};
		Err(NodeError::WhichDevice(format!("{target} {why}")))
	}

	/// Keeps `replicas`, of those that [`Recovering::others`] lists, in place of those kept
	/// before: [`Recovering::finish`] leaves them as they are, rather than revoke them, as
	/// the person still has their devices.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichDevice`] if one is none of those, or if the library that
	/// no longer unlocks, which the recovery reads, names no replica of its own, which then
	/// may be any of them, and then keeps nothing new; and an error if the personal space
	/// cannot be read.
	pub fn keep(&mut self, replicas: &[EntityId]) -> Result<(), NodeError> {
		if !replicas.is_empty() && self.source.unnamed() {
			return Err(NodeError::WhichDevice(
				"the library that no longer unlocks does not say which replica it is, so no device can be kept: it may be any of them".to_owned(),
			));
		}
		let others: BTreeSet<EntityId> = self.others()?.iter().map(|other| other.id).collect();
		if let Some(unknown) = replicas.iter().find(|replica| !others.contains(replica)) {
			return Err(NodeError::WhichDevice(format!(
				"{unknown} is no other device of yours that this recovery would revoke"
			)));
		}
		self.keep = replicas.iter().copied().collect();
		Ok(())
	}

	/// Finishes the recovery: lists the new replica by the code in the personal space; takes
	/// the person's other spaces and groups that the code is given, with what the backup holds
	/// of each, and lists it in each, by the code on the person's record, or through a group in
	/// a space that the person reaches through a group alone; takes the secrets, if the code
	/// holds them, protected as `secrets` says, unless it is `None`; then revokes every other
	/// replica of the person's, whose devices are lost, but those that [`Recovering::keep`]
	/// keeps, and updates the backup; in place of a library here that no longer unlocks, sets
	/// aside that library's replica, as its last step. A recovery that fails removes the new
	/// replica.
	///
	/// # Errors
	///
	/// Returns [`NodeError::LibraryPassphrase`] if `secrets` is the library's passphrase,
	/// [`NodeError::BackupBusy`] if another program updates the backup, and an error if the
	/// source cannot be read, the replica written, or the replica it replaces set aside.
	pub fn finish(
		mut self,
		secrets: Option<&NewProtection<'_>>,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<(Library, Recovered), NodeError> {
		let Some(library) = self.library.as_mut() else {
			return Err(NodeError::NoLibrary);
		};
		// The secrets' passphrase is never the library's, which would make the two one layer.
		if let Some(protection) = secrets.filter(|_| self.holds_secrets)
			&& library.opens_the_other(protection, Compartment::Replica)?
		{
			return Err(NodeError::LibraryPassphrase);
		}
		// Only what the check above covered.
		let secrets = secrets.filter(|_| self.holds_secrets);
		// Whatever was kept, never a replica that it revokes in any case.
		let keep: BTreeSet<EntityId> = self
			.keep
			.difference(&self.revoked_anyway)
			.copied()
			.collect();
		let mut recovered = library.recover_rest(
			&self.source,
			(self.code, &self.new, &keep),
			secrets,
			now,
			rng,
		)?;
		let Some(library) = self.library.take() else {
			return Err(NodeError::NoLibrary);
		};
		let mark = library.directory.join(RECOVERING);
		// In place of a library here, the file that says that the new replica is whole ends
		// the recovery; then the old replica is set aside, if it can be now, as the library
		// does as it opens otherwise.
		if let Some(old) = &self.replacing {
			let done = crate::info::write_file(&library.directory.join(RECOVERED), b"");
			drop(self.mark.take());
			if let Err(error) = done {
				library.remove_unsynced();
				return Err(error);
			}
			tidy(&library.directory);
			let aside = set_aside_path(old);
			if let RecoveredFrom::Library {
				replaced,
				set_aside,
				..
			} = &mut recovered.from
			{
				*replaced = true;
				*set_aside = (!old.exists() && aside.is_dir()).then_some(aside);
			}
			return Ok((library, recovered));
		}
		// The mark goes while it is locked, so that a recovery started meanwhile never takes
		// this one for one cut short; then the lock.
		let removed = fs::remove_file(&mark);
		drop(self.mark.take());
		if let Err(error) = removed {
			library.remove_unsynced();
			return Err(NodeError::file(&mark)(error));
		}
		#[cfg(unix)]
		let _ = fs::File::open(&library.directory).and_then(|folder| folder.sync_all());
		Ok((library, recovered))
	}
}

impl Drop for Recovering<'_> {
	fn drop(&mut self) {
		// Unfinished, the new replica goes, which nobody knows.
		drop(self.mark.take());
		if let Some(library) = self.library.take() {
			library.remove_unsynced();
		}
	}
}

impl Library {
	/// Checks, before anything is asked or made, that `code` opens what the source in
	/// `source` holds for it, a backup or a library that no longer unlocks: the personal
	/// space's first key, from its recovery file.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NoRecoverySource`] if `source` is neither a backup nor a library
	/// with a recovery file, [`NodeError::CodeOpensNothing`] if the code opens nothing there,
	/// [`NodeError::RecoveryFileChanged`] if the file's entry for the code is not what the code
	/// signed, [`NodeError::NewerFormats`] if a later version of NibKeep changed the library,
	/// and an error if the file cannot be read.
	pub fn check_recovery_source(source: &Path, code: &RecoveryCode) -> Result<(), NodeError> {
		open_source(source, code).map(|_| ())
	}

	/// Whether recovering in `directory` from the library in `source` would replace the
	/// library here, which no longer unlocks: its replica is the one here, which a recovery
	/// sets aside once it is done.
	#[must_use]
	pub fn recovery_replaces(directory: &Path, source: &Path) -> bool {
		let Ok(here) = Self::replica_folder(directory) else {
			return false;
		};
		source_replica(source).is_some_and(|folder| same_folder(&folder, &here))
	}

	/// Recovers the library of the person whose recovery code is `code` on this computer,
	/// in `directory`, from the source in `source`, as a new replica that `joining` makes,
	/// protected as `protection` says, and the secrets as `secrets` says, if the code holds
	/// them: [`Library::start_recovery`], then [`Recovering::finish`].
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::start_recovery`] and [`Recovering::finish`].
	pub fn recover(
		directory: &Path,
		(source, code): (&Path, &RecoveryCode),
		joining: Joining<'_>,
		(protection, secrets): (&NewProtection<'_>, Option<&NewProtection<'_>>),
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<(Self, Recovered), NodeError> {
		Self::start_recovery(directory, (source, code), joining, protection, now, rng)?
			.finish(secrets, now, rng)
	}

	/// Starts recovering the library of the person whose recovery code is `code` on this
	/// computer, in `directory`, from the source in `source`, as a new replica that `joining`
	/// makes, protected as `protection` says: it opens the personal space's first key from the
	/// source's recovery file, takes in what the source holds of the personal space, and opens
	/// the keys sealed to the code; [`Recovering::finish`] does the rest. The code's keys are
	/// held in memory alone. Until it is done, nobody knows the new replica, and a recovery
	/// that fails removes it; one cut short, as by a crash, leaves it marked, and a recovery
	/// run again removes it and starts over.
	///
	/// The source is a backup, or a library on this computer that no longer unlocks: its
	/// directory, in which the replica that is here is read, or else its one copy with a
	/// recovery file, or the folder of one of its replicas. It is read as a copy is, and never
	/// written to. If it is the library in `directory`, the new replica goes beside its
	/// replica, which [`Recovering::finish`] sets aside.
	///
	/// # Errors
	///
	/// Returns [`NodeError::Exists`] if another library is here,
	/// [`NodeError::NoRecoverySource`] if `source` is neither a backup nor a library with a
	/// recovery file, [`NodeError::CodeOpensNothing`] if the code opens nothing there,
	/// [`NodeError::RecoveryFileChanged`] if the file's entry for the code is not what the code
	/// signed, all before anything is written, [`NodeError::RecoveryCodeRevoked`] if the source holds the
	/// code's revocation, [`NodeError::BackupBusy`] if another program updates the backup, and
	/// an error if the source cannot be read or the new replica made.
	pub fn start_recovery<'a>(
		directory: &Path,
		(source, code): (&'a Path, &'a RecoveryCode),
		joining: Joining<'_>,
		protection: &NewProtection<'_>,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<Recovering<'a>, NodeError> {
		let cut_short = remove_cut_short(directory)?;
		let replacing = match Self::replica_folder(directory) {
			Err(NodeError::NoLibrary | NodeError::Copied) => None,
			Ok(here) if source_replica(source).is_some_and(|read| same_folder(&read, &here)) => {
				Some(here)
			}
			_ => return Err(NodeError::Exists),
		};
		// What the code opens is read before anything is made: a wrong code writes nothing.
		let (source, opened) = open_source(source, code)?;
		Self::check_place(directory)?;
		let (library, new) = Self::make_recovering(
			directory,
			(&opened, code),
			replacing.as_deref(),
			joining,
			protection,
			rng,
		)?;
		let mark = library.directory.join(RECOVERING);
		let mut recovering = Recovering {
			library: Some(library),
			mark: None,
			source,
			replacing,
			code,
			new,
			holds_secrets: false,
			keep: BTreeSet::new(),
			revoked_anyway: BTreeSet::new(),
		};
		recovering
			.revoked_anyway
			.extend(cut_short.into_iter().chain(recovering.source.replica()));
		// Held while it runs, so that a recovery started meanwhile leaves this one alone.
		let held = fs::File::options()
			.write(true)
			.open(&mark)
			.and_then(|file| file.try_lock().map(|()| file).map_err(io::Error::other))
			.map_err(NodeError::file(&mark))?;
		recovering.mark = Some(held);
		if let Some(library) = recovering.library.as_mut() {
			recovering.holds_secrets =
				library.recover_personal(&recovering.source, (code, &opened), now, rng)?;
		}
		Ok(recovering)
	}

	/// Makes the replica that a recovery brings back, in a folder of `directory` marked as
	/// [`RECOVERING`], beside the replica in `replacing`, if given, which the mark names, with
	/// the keys that `joining` made and the personal space's first key, which `opened` gives,
	/// and its spaces empty; and the records that list it by `code`.
	fn make_recovering(
		directory: &Path,
		(opened, code): (&OpenedRecovery, &RecoveryCode),
		replacing: Option<&Path>,
		joining: Joining<'_>,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<(Self, NewRecords), NodeError> {
		let personal = opened.space;
		let (signing, decryption, device, app) = joining.into_parts();
		// The new replica's records, as pairing would make them, with its own keys, and the
		// listing that its key signs, beside the one that the code signs.
		let key = signing.verifying_key();
		let replica = nibkeep_governance::replica_record(personal, &key);
		let encryption = decryption.encryption_key();
		let endpoint = endpoint_of(&nibkeep_governance::endpoint_secret(&signing));
		let own = Listing::sign(&signing, &encryption, personal, (opened.member, false)).to_text();
		let added = nibkeep_governance::add_replica(
			&NewReplica {
				space: personal,
				user: opened.member,
				signing: &key,
				encryption: &encryption,
				endpoint: &endpoint,
				device: &device,
				app: &app,
				keys: &[],
				ai_agent: false,
				given: &[],
				listing: Some(&own),
			},
			rng,
		);
		let listing =
			RecoveryListing::sign(code, (personal, replica, opened.member), &key, &encryption);
		let info = Info {
			user: opened.member,
			device: added.device,
			replica,
			personal,
		};
		let keys = ReplicaKeys {
			signing,
			decryption,
			storage: Key::generate(rng),
			spaces: BTreeMap::from([(personal, BTreeMap::from([(0, opened.key.clone())]))]),
		};
		let replaced = replacing
			.and_then(Path::file_name)
			.map_or_else(String::new, |name| name.to_string_lossy().into_owned());
		let folder = new_replica_folder_marked(
			directory,
			Some((RECOVERING, replaced.as_bytes())),
			replacing,
			rng,
		)?;
		let (mut key_file, compartment) = match KeyFile::create(&keys, protection, rng) {
			Ok(created) => created,
			Err(error) => {
				let _ = fs::remove_dir_all(&folder);
				return Err(error.into());
			}
		};
		let made = key_file
			.write(&folder.join(KEYS))
			.map_err(NodeError::from)
			.and_then(|()| info.write(&folder.join(INFO)))
			.and_then(|()| Self::open_spaces(&folder, key_file.clone(), compartment, true));
		match made {
			Ok(mut library) => {
				// The space takes the first change that the backup's file names as its first.
				if let Some(open) = library.spaces.get_mut(&personal) {
					open.stored.expect_roots_too([opened.root]);
				}
				Ok((library, NewRecords { added, listing }))
			}
			Err(error) => {
				let _ = fs::remove_dir_all(&folder);
				key_file.discard();
				Err(error)
			}
		}
	}

	/// Takes in what `source` holds of the personal space, checked as a sync is, with the
	/// first key alone, which `opened` gives; then the keys sealed to `code`, which the space
	/// names and has not revoked. Returns whether the code holds the secrets.
	fn recover_personal(
		&mut self,
		source: &Source<'_>,
		(code, opened): (&RecoveryCode, &OpenedRecovery),
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let personal = opened.space;
		self.take_in_source(source, &[personal], now)?;
		self.finish_opening()?;
		if self.space(personal)?.stored.log().root()?.is_none() {
			return Err(NodeError::NoRecoverySource(source.path().to_owned()));
		}
		// The code, which the personal space names and has not revoked, and the keys sealed to
		// it, which open the rest.
		let recovery_key = code.signing_key().verifying_key();
		let record = recovery_record(personal, opened.member, &recovery_key);
		let named = self.roster(personal)?.code(record).map(Code::stands);
		match named {
			None => return Err(NodeError::CodeOpensNothing),
			Some(false) => return Err(NodeError::RecoveryCodeRevoked),
			Some(true) => {}
		}
		let opening = code.decryption_key();
		loop {
			let roster = self.roster(personal)?;
			if self.take_keys_of_code(personal, &roster, (record, &opening), rng)? == 0 {
				break;
			}
		}
		// The secrets that the code holds are the personal space's first.
		let rosters = BTreeMap::from([(personal, self.roster(personal)?)]);
		Ok(!self.secrets_code_opens(&rosters, code)?.is_empty())
	}

	/// Finishes the recovery in this new replica, whose personal space
	/// [`Library::recover_personal`] took in, from `source`, with `code`, the new replica's
	/// records being `new`, keeping the replicas in `keep`, as [`Recovering::finish`] says, but
	/// for setting aside the replica it replaces.
	fn recover_rest(
		&mut self,
		source: &Source<'_>,
		(code, new, keep): (&RecoveryCode, &NewRecords, &BTreeSet<EntityId>),
		secrets: Option<&NewProtection<'_>>,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<Recovered, NodeError> {
		let personal = self.info.personal;
		// It writes with the newest of the keys, never with the first key, which the lost
		// devices hold.
		self.choose_writing_keys()?;
		// It lists itself by the code, in a change that governs the space, which the space
		// itself commits, as nothing lists it yet; then it gives its records their names, as
		// pairing does.
		let mut governing = new.added.governing.clone();
		governing.push(list_by_recovery_code(&new.listing, rng));
		self.commit_unlisted(personal, governing, now(), rng)?;
		self.commit(personal, new.added.operations.clone(), now(), rng)?;
		// The person's other spaces and groups, as far as the source holds them, which it
		// says it holds, as every device that takes a space does.
		let (spaces, missing) = self.recover_spaces(source, code, now, rng)?;
		self.state_holding(&spaces, now(), rng)?;
		// The secrets, while the records still name the lost devices that held them.
		let recovered_secrets = self.take_code_secrets(code, secrets, now(), rng)?;
		// The person's other devices are lost, but those kept: each is revoked, with a new key,
		// in every space where they are listed.
		let (revoked, kept): (Vec<Replica>, Vec<Replica>) = self
			.replicas()?
			.into_iter()
			.filter(|replica| !replica.this && !replica.revoked)
			.partition(|replica| !keep.contains(&replica.id));
		// With them, in each space and group, the devices that they added there and that the
		// personal space taken in does not list, as a computer that learned of a new device
		// from another member first wrote the backup: no device of the person's is left to
		// tell of them.
		let ids: Vec<EntityId> = revoked.iter().map(|replica| replica.id).collect();
		// What it names only: a failure to read it leaves the recovery to go on.
		let out_of_reach = self.out_of_reach(&ids).unwrap_or_default();
		let revoked_elsewhere = if revoked.is_empty() {
			self.revoke_in_spaces_telling(now(), rng).0
		} else {
			self.revoke(&ids, now(), rng)?
		};
		// Then each space that the person reaches through a group alone lists it, and learns
		// of the revocations there, by its own anchor of the groups' changes.
		self.anchor_where_unlisted(&spaces, now(), rng)?;
		self.settle_keys(now(), rng)?;
		let from = match source {
			// The backup, which carries the new device from now on, if the space still names
			// it. The library is whole by now: an update that fails leaves the backup to the
			// next one, as any update does, rather than throw away what came back.
			Source::Backup(folder, backup) => {
				let found = self
					.backups()?
					.into_iter()
					.find(|found| found.id == *backup);
				let updated = found.as_ref().is_some_and(|found| {
					matches!(
						self.update_backup(found, folder, now, &|| false),
						Ok(Some(_))
					)
				});
				RecoveredFrom::Backup {
					name: found.map(|found| found.name),
					updated,
				}
			}
			Source::Library(folder, replica) => RecoveredFrom::Library {
				folder: folder.clone(),
				replica: *replica,
				replaced: false,
				set_aside: None,
			},
		};
		Ok(Recovered {
			revoked_elsewhere,
			revoked,
			kept,
			spaces,
			missing,
			out_of_reach,
			secrets: recovered_secrets,
			from,
		})
	}
}

impl Library {
	/// Commits `operations` in `space` as this replica, which no counting change there may
	/// list yet, as [`Library::commit`] would but for that check: for a replica that lists
	/// itself by a code, or through a group by its own anchor.
	fn commit_unlisted(
		&mut self,
		space: SpaceId,
		operations: Vec<nibkeep_model::Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let reloads = open.stored.reloads();
		open.commit(
			&self.replica.signing,
			&self.code_types,
			operations,
			wall_millis,
			rng,
		)?;
		if open.stored.reloads() != reloads {
			self.read_again(space)?;
		}
		Ok(())
	}

	/// Takes the person's other spaces and groups that `code` is given, with what `source`
	/// holds of each, and the keys sealed to the code there; and lists this replica by the
	/// code in each where the code stands on the record of the person that the personal
	/// space's entry names, which has a role there. Returns the spaces given to the code that
	/// came back, and those that the source holds nothing of, which it takes away again; the
	/// logs of the groups that those spaces add, which it holds to follow them, are neither.
	///
	/// It goes round until a round takes nothing new: a group's entries for the code open
	/// once its keys do, and a space takes the changes of a group's members once it follows
	/// the group, which it does once it holds the change that adds it. So each round takes in
	/// the spaces new to it, and again those that follow groups.
	fn recover_spaces(
		&mut self,
		source: &Source<'_>,
		code: &RecoveryCode,
		now: &dyn Fn() -> u64,
		rng: &mut impl CryptoRng,
	) -> Result<(Vec<SpaceId>, Vec<SpaceId>), NodeError> {
		let personal = self.info.personal;
		let recovery_key = code.signing_key().verifying_key();
		let opening = code.decryption_key();
		// The person's record in each space that the personal space gives the code, and the
		// spaces that any entry gives it.
		let mut members: BTreeMap<SpaceId, EntityId> = BTreeMap::new();
		let mut given: BTreeSet<SpaceId> = BTreeSet::new();
		let mut taken_in: BTreeSet<SpaceId> = BTreeSet::from([personal]);
		loop {
			let held = self.spaces.len();
			for (entry, record, from_personal) in self.code_entries(&recovery_key)? {
				if from_personal {
					members.entry(entry.space).or_insert(entry.member);
				}
				given.insert(entry.space);
				if self.spaces.contains_key(&entry.space) {
					continue;
				}
				let Some(key) = entry
					.open(&opening, record)
					.filter(|key| key_hash(key) == entry.hash)
				else {
					continue;
				};
				self.add_space(entry.space, &key, rng)?;
				if let Some(open) = self.spaces.get_mut(&entry.space) {
					open.stored
						.expect_roots_too([ChangeId::from_hash(entry.root)]);
				}
			}
			// And the logs of the groups that those spaces add, which they follow.
			self.take_group_keys(rng)?;
			let _ = self.follow_all_groups();
			let mut others = Vec::new();
			for space in self.spaces() {
				if !taken_in.contains(&space) || !self.inner_groups(space)?.is_empty() {
					others.push(space);
				}
			}
			others.retain(|&space| space != personal);
			if others.is_empty() {
				break;
			}
			taken_in.extend(others.iter().copied());
			let taken = self.take_in_source(source, &others, now)?;
			let _ = self.follow_all_groups();
			let mut keys = 0;
			for &space in &others {
				keys += self.take_keys_of_codes(space, &recovery_key, &opening, rng)?;
			}
			if self.spaces.len() == held && taken == 0 && keys == 0 {
				break;
			}
		}
		self.choose_writing_keys()?;
		let mut spaces = Vec::new();
		let mut missing = Vec::new();
		for space in given {
			if !self.spaces.contains_key(&space) {
				continue;
			}
			if self.space(space)?.stored.log().root()?.is_some() {
				spaces.push(space);
			} else {
				missing.push(space);
			}
		}
		// A group that a space it holds adds stays, empty, as the groups would take its log
		// again to follow it.
		let mut followed = BTreeSet::new();
		for open in self.spaces.values() {
			followed.extend(
				open.stored
					.log()
					.groups(&open.keys)?
					.into_iter()
					.map(|found| found.group.group),
			);
		}
		for &space in &missing {
			if !followed.contains(&space) {
				self.remove_space(space, rng);
			}
		}
		for &space in &spaces {
			if let Some(&member) = members.get(&space) {
				self.list_by_code_in(space, member, code, now(), rng)?;
			}
		}
		Ok((spaces, missing))
	}

	/// The entries that give spaces to the code whose recovery key is `recovery_key`: on each
	/// of its records in the personal space, and in each group this replica holds, for a
	/// space that the group agreed to join; each with the record it is on, and whether the
	/// personal space gives it, whose entries name the person's record in the space.
	fn code_entries(
		&self,
		recovery_key: &VerifyingKey,
	) -> Result<Vec<(SpaceEntry, EntityId, bool)>, NodeError> {
		let personal = self.info.personal;
		let mut entries = Vec::new();
		let records = self.code_records(personal, recovery_key)?;
		let open = self.space(personal)?;
		for governing in open.stored.log().governing_statements(&open.keys)? {
			let record = governing.statement.entity;
			if !records.contains(&record) {
				continue;
			}
			if let Some(entry) = governing
				.counting_text(&SPACE_ENTRY)
				.and_then(SpaceEntry::from_text)
			{
				entries.push((entry, record, true));
			}
		}
		for space in self.spaces() {
			if space == personal || !self.is_group(space)? {
				continue;
			}
			for record in self.code_records(space, recovery_key)? {
				for (entry, _) in self.agreed_entries(space, record)? {
					entries.push((entry, record, false));
				}
			}
		}
		Ok(entries)
	}

	/// The records in `space` of the code whose recovery key is `recovery_key`, for whichever
	/// member each names, as the space's codes have them.
	fn code_records(
		&self,
		space: SpaceId,
		recovery_key: &VerifyingKey,
	) -> Result<BTreeSet<EntityId>, NodeError> {
		Ok(self
			.roster(space)?
			.codes()
			.filter(|code| code.statement.key == *recovery_key)
			.map(|code| code.record)
			.collect())
	}

	/// Takes the keys of `space` that statements seal to the code whose recovery key is
	/// `recovery_key`, on each of its records there, opening them with `opening`, until none
	/// is left to take: a key may give one that it opens. Returns how many it took.
	fn take_keys_of_codes(
		&mut self,
		space: SpaceId,
		recovery_key: &VerifyingKey,
		opening: &DecryptionKey,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		let mut total = 0;
		loop {
			let roster = self.roster(space)?;
			let records: Vec<EntityId> = roster
				.codes()
				.filter(|code| code.statement.key == *recovery_key)
				.map(|code| code.record)
				.collect();
			let mut taken = 0;
			for record in records {
				taken += self.take_keys_of_code(space, &roster, (record, opening), rng)?;
			}
			if taken == 0 {
				return Ok(total);
			}
			total += taken;
		}
	}

	/// Lists this replica in a space with members by `code`, on the record of `member`, the
	/// person there, as pairing would list it there, with its own listing and the code's: only
	/// where it is not listed yet, the code stands on that member's record for it, and the
	/// member has a role. Returns whether it did.
	fn list_by_code_in(
		&mut self,
		space: SpaceId,
		member: EntityId,
		code: &RecoveryCode,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let key = self.replica.signing.verifying_key();
		{
			let open = self.space(space)?;
			let log = open.stored.log();
			if !log.has_members(&open.keys)? || log.is_listed(&key, &open.keys)? {
				return Ok(false);
			}
			let has_role = log
				.member_roles(&open.keys)?
				.get(&member)
				.is_some_and(|role| *role != Role::None);
			let record = recovery_record(space, member, &code.signing_key().verifying_key());
			if !has_role || !self.roster(space)?.code(record).is_some_and(Code::stands) {
				return Ok(false);
			}
		}
		let encryption = self.replica.decryption.encryption_key();
		let listing = Listing::sign(&self.replica.signing, &encryption, space, (member, false));
		let (device, app) = self.own_names()?;
		let endpoint = endpoint_of(&nibkeep_governance::endpoint_secret(&self.replica.signing));
		let listed = list_replica(
			&SpaceReplica {
				record: listing.record,
				device_record: nibkeep_governance::device_record(space, &key),
				member,
				signing: &key,
				encryption: &encryption,
				listing: &listing.to_text(),
				endpoint: Some(&endpoint),
				device: &device,
				app: &app,
				ai_agent: false,
			},
			rng,
		);
		let by_code =
			RecoveryListing::sign(code, (space, listing.record, member), &key, &encryption);
		let mut governing = listed.governing;
		governing.push(list_by_recovery_code(&by_code, rng));
		self.commit_unlisted(space, governing, wall_millis, rng)?;
		// A listing that does not count, as one that a change it does not know of sets aside,
		// leaves the space to be read, as the backup held it, and the rest of the recovery
		// goes on.
		let open = self.space(space)?;
		if !open.stored.log().is_listed(&key, &open.keys)? {
			return Ok(false);
		}
		self.commit(space, listed.operations, wall_millis, rng)?;
		Ok(true)
	}

	/// Anchors the groups' changes in each of `spaces` that does not list this replica yet,
	/// and that a group which reaches it lists it in: so the space lists it through the group,
	/// by its own anchor, and learns of what the group revoked.
	fn anchor_where_unlisted(
		&mut self,
		spaces: &[SpaceId],
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let _ = self.follow_all_groups();
		let own = self.replica.signing.verifying_key();
		for &space in spaces {
			let root = {
				let open = self.space(space)?;
				let log = open.stored.log();
				if log.is_listed(&own, &open.keys)? {
					continue;
				}
				let Some(root) = log.root()? else { continue };
				root
			};
			let mut listed_by_a_group = false;
			for (_, group) in self.reaching_groups(space, root)? {
				let open = self.space(group.group)?;
				let log = open.stored.log();
				listed_by_a_group |=
					log.is_listed(&own, &open.keys)? && !log.is_revoked(&own, &open.keys)?;
			}
			let operations = if listed_by_a_group {
				self.anchoring(space, rng)?
			} else {
				Vec::new()
			};
			if operations.is_empty() {
				continue;
			}
			self.commit_unlisted(space, operations, wall_millis, rng)?;
		}
		Ok(())
	}
}

/// The records that list the replica that a recovery brings back: those pairing would make,
/// and its listing by the code.
struct NewRecords {
	added: nibkeep_governance::AddedReplica,
	listing: RecoveryListing,
}

/// What a recovery takes the library from.
enum Source<'a> {
	/// A backup, in this folder, by its record's ID.
	Backup(&'a Path, EntityId),
	/// A library on this computer that no longer unlocks: the folder of its replica, which is
	/// read as a copy is, and the replica, as the folder names it, if it does.
	Library(PathBuf, Option<EntityId>),
}

impl Source<'_> {
	/// The folder that holds it.
	fn path(&self) -> &Path {
		match self {
			Self::Backup(folder, _) => folder,
			Self::Library(folder, _) => folder,
		}
	}

	/// The replica of a library that it is, as its folder names it.
	const fn replica(&self) -> Option<EntityId> {
		match self {
			Self::Backup(..) => None,
			Self::Library(_, replica) => *replica,
		}
	}

	/// Whether it is a library whose folder names no replica, as one whose files are
	/// damaged.
	const fn unnamed(&self) -> bool {
		matches!(self, Self::Library(_, None))
	}
}

impl Library {
	/// Takes in what `source` holds of `spaces` and this replica lacks, checked as a sync is:
	/// what a backup holds, or a library's replica, whose changes are taken as from their
	/// authors, as a backup's are. Returns how many changes, or items of a library, were new.
	///
	/// # Errors
	///
	/// Returns [`NodeError::BackupBusy`] if another program updates the backup, and an error
	/// if what is taken in cannot be saved.
	fn take_in_source(
		&mut self,
		source: &Source<'_>,
		spaces: &[SpaceId],
		now: &dyn Fn() -> u64,
	) -> Result<usize, NodeError> {
		match source {
			Source::Backup(folder, backup) => Ok(self
				.take_in_backup_at((folder, *backup), spaces, now)?
				.ok_or(NodeError::BackupBusy)?
				.changes),
			Source::Library(folder, _) => self.take_in_lost_replica(folder, spaces, now),
		}
	}
}

/// The source in `source`, and what its recovery file gives `code`.
fn open_source<'a>(
	source: &'a Path,
	code: &RecoveryCode,
) -> Result<(Source<'a>, OpenedRecovery), NodeError> {
	let no_source = || NodeError::NoRecoverySource(source.to_owned());
	let (opened, file) = if let Some(backup) = backup::named_at(source) {
		(Source::Backup(source, backup), source.join(RECOVERY_FILE))
	} else {
		let folder = source_replica(source).ok_or_else(no_source)?;
		// A replica that a later version changed is read by none of its files.
		crate::engines::check(&folder)?;
		let file = folder.join(RECOVERY_FILE);
		// Read once, as the replica that the recovery revokes in any case.
		let replica = Info::read(&folder.join(INFO)).ok().map(|info| info.replica);
		(Source::Library(folder, replica), file)
	};
	// Where a folder replaces a file by deleting it first, as Android's picker does, a
	// backup's new recovery file waits in `tmp/` until the next update moves it in.
	let waiting = file
		.parent()
		.map(|folder| folder.join("tmp").join(RECOVERY_FILE));
	// One there that is missing or does not read gives way to one waiting that reads.
	let file = match (RecoveryFile::read(&file), &opened, waiting) {
		(Ok(Some(file)), _, _) => file,
		(there, Source::Backup(..), Some(waiting)) => match RecoveryFile::read(&waiting) {
			Ok(Some(file)) => file,
			_ => there?.ok_or_else(no_source)?,
		},
		(there, ..) => there?.ok_or_else(no_source)?,
	};
	let key = file.open(code)?;
	Ok((opened, key))
}

/// The folder of the replica that a recovery from the library in `source` reads: its
/// replica that is here, but one that a recovery left unfinished, or that a recovery in
/// place replaces; or, where none is here, as in a library restored from another computer's
/// backup, its one copy with a recovery file; or `source` itself, if it is the folder of a
/// replica with a recovery file.
fn source_replica(source: &Path) -> Option<PathBuf> {
	let Some(folders) = replica_folders(source) else {
		let replica = source.join(crate::place::PLACE).is_file()
			&& source.join(RECOVERY_FILE).is_file()
			&& !unfinished(source);
		return replica.then(|| source.to_owned());
	};
	let folders: Vec<(PathBuf, bool)> = folders
		.into_iter()
		.filter(|(folder, _)| !unfinished(folder))
		.collect();
	let replaced: Vec<PathBuf> = folders
		.iter()
		.filter(|(_, here)| *here)
		.filter_map(|(folder, _)| replaced_by(folder))
		.collect();
	let mut here = folders
		.iter()
		.filter(|(folder, here)| *here && !replaced.contains(folder));
	match (here.next(), here.next()) {
		(Some((folder, _)), None) => return Some(folder.clone()),
		(Some(_), Some(_)) => return None,
		(None, _) => {}
	}
	let mut copies = folders
		.iter()
		.filter(|(folder, _)| folder.join(RECOVERY_FILE).is_file());
	match (copies.next(), copies.next()) {
		(Some((folder, _)), None) => Some(folder.clone()),
		_ => None,
	}
}

/// Where a recovery in place sets aside the folder of the replica `old`: beside it, under a
/// name that [`SET_ASIDE`] starts.
fn set_aside_path(old: &Path) -> PathBuf {
	let name = old
		.file_name()
		.map_or_else(String::new, |name| name.to_string_lossy().into_owned());
	old.with_file_name(format!("{SET_ASIDE}{name}"))
}

/// Sets aside the folder of the replica `old`, which a recovery in place replaced, at
/// [`set_aside_path`].
fn set_aside(old: &Path) -> Result<(), NodeError> {
	let aside = set_aside_path(old);
	if aside.exists() {
		return Err(NodeError::file(&aside)(io::ErrorKind::AlreadyExists.into()));
	}
	fs::rename(old, &aside).map_err(NodeError::file(old))?;
	#[cfg(unix)]
	if let Some(parent) = aside.parent() {
		let _ = fs::File::open(parent).and_then(|folder| folder.sync_all());
	}
	Ok(())
}

/// The name of the replica's folder that the [`RECOVERING`] mark in `folder` names, which the
/// replica in `folder` replaces: `None` for a mark that names none, as a recovery from a
/// backup leaves, or names no folder beside it.
fn replaced_name(folder: &Path) -> Option<String> {
	let bytes = fs::read(folder.join(RECOVERING)).ok()?;
	let name = String::from_utf8(bytes).ok()?;
	let plain = !name.is_empty()
		&& !name.starts_with('.')
		&& !name.contains(['/', '\\'])
		&& Path::new(&name)
			.file_name()
			.is_some_and(|own| own == name.as_str());
	plain.then_some(name)
}

/// The folder of the replica that the replica in `folder` replaces, as its [`RECOVERING`]
/// mark names it, while it is still in its place: a recovery in place still runs, was cut
/// short, or is done but could not set it aside yet.
pub(crate) fn replaced_by(folder: &Path) -> Option<PathBuf> {
	let replaced = folder.parent()?.join(replaced_name(folder)?);
	replaced.is_dir().then_some(replaced)
}

/// Whether the replica in `folder` is one that a recovery brings back, which still runs or
/// was cut short, so that the library does not open: it has its [`RECOVERING`] mark, and no
/// [`RECOVERED`] file that says that it is whole.
pub(crate) fn unfinished(folder: &Path) -> bool {
	folder.join(RECOVERING).exists() && !folder.join(RECOVERED).exists()
}

/// Finishes what a recovery in place that is done left in `folder`, its new replica's: sets
/// aside the replica it replaces, if that is still in its place, then removes the
/// [`RECOVERING`] mark, and the [`RECOVERED`] file last. Nothing, while a recovery holds the
/// mark, or while the old replica cannot be set aside, which then stays hidden behind the
/// mark.
pub(crate) fn tidy(folder: &Path) {
	let done = folder.join(RECOVERED);
	if !done.exists() {
		return;
	}
	let path = folder.join(RECOVERING);
	if let Ok(mark) = fs::File::options().write(true).open(&path) {
		if mark.try_lock().is_err() {
			return;
		}
		drop(mark);
		if let Some(old) = replaced_by(folder)
			&& set_aside(&old).is_err()
		{
			return;
		}
		if fs::remove_file(&path).is_err() {
			return;
		}
	}
	let _ = fs::remove_file(&done);
	#[cfg(unix)]
	let _ = fs::File::open(folder).and_then(|folder| folder.sync_all());
}

/// Removes the replica of a recovery that was cut short, as by a crash, which nobody knows
/// but through a backup that it updated: its folder and its keystore keys. One whose
/// recovery still runs, which holds its mark, stays, and so does one whose recovery is
/// done. Returns the replica it removed, as its folder named it.
///
/// # Errors
///
/// Returns an error if the folder cannot be removed.
fn remove_cut_short(directory: &Path) -> Result<Option<EntityId>, NodeError> {
	let Found::Here { folder, .. } = found(directory) else {
		return Ok(None);
	};
	if !unfinished(&folder) {
		return Ok(None);
	}
	let Ok(mark) = fs::File::options()
		.write(true)
		.open(folder.join(RECOVERING))
	else {
		return Ok(None);
	};
	if mark.try_lock().is_err() {
		return Ok(None);
	}
	drop(mark);
	let replica = Info::read(&folder.join(INFO)).ok().map(|info| info.replica);
	if let Ok(key_file) = KeyFile::read(&folder.join(KEYS)) {
		key_file.discard();
	}
	fs::remove_dir_all(&folder).map_err(NodeError::file(&folder))?;
	Ok(replica)
}
