//! Secrets: the secrets keys of the library's spaces, which seal the values of secret
//! fields, in the key file's secrets compartment (`docs/Design.md`, section 6.9); and
//! giving them to the replicas that the person chooses, which ask for them with a key of
//! their secrets compartment ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#12-secrets)).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, Key, RecoveryCode, VerifyingKey, key_hash};
use nibkeep_governance::{Roster, SealedSecretsKey};
use nibkeep_keys::{
	CompartmentKey, KeyFile, NewProtection, Protection, SealedSecrets, SecretsKeys, SpaceKeyMap,
	Unlocker,
};

use super::recovering::RecoveredSecrets;
use super::secrets_keys::{fresh_number, keep_given, take_given};
use super::{INFO, KEYS, Library};
use crate::NodeError;
use crate::info::Info;

/// Where a library's secrets are, as this replica sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretsState {
	/// No replica has set up the secrets yet: the first secret saved sets them up here.
	NotSetUp,
	/// This replica holds the secrets keys, or takes those given to it when its secrets are
	/// next unlocked.
	Here,
	/// This replica asked for the secrets, and waits for one that holds them to give them.
	Asked,
	/// Another replica holds the secrets; this one can ask for them.
	Elsewhere,
}

/// A compartment of the key file.
#[derive(Clone, Copy)]
pub(super) enum Compartment {
	/// The replica's keys.
	Replica,
	/// The secrets keys.
	Secrets,
}

/// The file that writers of the key file lock, so that two of them never replace each
/// other's keys.
const KEYS_LOCK: &str = "keys.lock";

impl Library {
	/// How the library's secrets keys are protected, or `None` if it has none yet, as the
	/// key file on the disk says: another program may have given them since the library was
	/// opened.
	///
	/// # Errors
	///
	/// Returns an error if the key file cannot be read.
	pub fn secrets_protection(&self) -> Result<Option<Protection>, NodeError> {
		Ok(self.key_file_on_disk()?.secrets_protection().cloned())
	}

	/// Whether another replica keeps the library's secrets, which this one cannot read or
	/// write until one that holds them gives them: it has not asked for them, or waits.
	///
	/// # Errors
	///
	/// Returns an error if the key file or a space cannot be read.
	pub fn secrets_elsewhere(&self) -> Result<bool, NodeError> {
		Ok(matches!(
			self.secrets_state()?,
			SecretsState::Asked | SecretsState::Elsewhere
		))
	}

	/// Where the library's secrets are: here, asked for, held by another replica, or not set
	/// up anywhere yet, as the key file on the disk and the personal space's records say. A
	/// replica with a secrets compartment holds them, unless it asked another replica that
	/// gave it nothing yet; one whose secrets compartment is gone, whatever its records say,
	/// can ask for them again.
	///
	/// # Errors
	///
	/// Returns an error if the key file or a space cannot be read.
	pub fn secrets_state(&self) -> Result<SecretsState, NodeError> {
		let own = self.info.replica;
		let roster = self.roster(self.info.personal)?;
		if self.secrets_protection()?.is_some() {
			if roster.secrets_given_to(own).next().is_some() {
				return Ok(SecretsState::Here);
			}
			return Ok(match roster.secrets_from(own) {
				Some(from) if from != own => SecretsState::Asked,
				_ => SecretsState::Here,
			});
		}
		Ok(if self.secrets_held_elsewhere(&roster)? {
			SecretsState::Elsewhere
		} else {
			SecretsState::NotSetUp
		})
	}

	/// Whether another replica that is not revoked holds the secrets, as the records give
	/// them. Where only revoked replicas held them, their secrets are lost, and new ones may
	/// be set up.
	fn secrets_held_elsewhere(&self, roster: &Roster) -> Result<bool, NodeError> {
		let own = self.info.replica;
		for holder in roster.secrets_holders() {
			if holder != own && self.is_remaining(self.info.personal, roster, holder)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// The key of the secrets compartment, which must be open, or why it is not: locked, or
	/// not set up on this replica.
	pub(super) fn open_compartment(&self) -> Result<CompartmentKey, NodeError> {
		match &self.secrets_compartment {
			Some(compartment) => Ok(compartment.clone()),
			None if self.secrets_protection()?.is_some() => Err(NodeError::SecretsLocked),
			None => Err(NodeError::SetUpSecretsFirst),
		}
	}

	/// Whether a replica of a space remains: its keys are listed, and none is revoked.
	pub(super) fn is_remaining(
		&self,
		space: SpaceId,
		roster: &Roster,
		replica: EntityId,
	) -> Result<bool, NodeError> {
		Ok(self
			.remaining(space, roster, &[])?
			.iter()
			.any(|(remaining, _)| *remaining == replica))
	}

	/// The roster of every space.
	pub(super) fn rosters(&self) -> Result<BTreeMap<SpaceId, Roster>, NodeError> {
		self.secret_spaces()?
			.into_iter()
			.map(|space| Ok((space, self.roster(space)?)))
			.collect()
	}

	/// Whether a space's secrets are unlocked: whether its secret values can be read and
	/// written.
	#[must_use]
	pub fn secrets_unlocked(&self, space: nibkeep_base::SpaceId) -> bool {
		self.spaces
			.get(&space)
			.is_some_and(|open| open.keys.newest_secrets().is_some())
	}

	/// Gives the library its secrets keys, the first time a secret is stored: a new secrets
	/// key for the personal space, in the key file's secrets compartment, protected as
	/// `protection` says, apart from the library's other keys, and never by the library's
	/// own passphrase. The records then say that this replica holds them, at the time
	/// `wall_millis`. The secrets are unlocked afterwards.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase is the library's; [`NodeError::SecretsElsewhere`]
	/// if another replica holds the secrets, which this one asks for instead; if the library
	/// has secrets keys already, also when another program gave them since the library was
	/// opened; if the passphrase cannot be used or the keystore fails; or if the key file
	/// cannot be read or written, in which case nothing changed. If only the records cannot
	/// be written, the secrets are set up all the same, and locked: the next time they are
	/// unlocked, the records say so.
	pub fn set_up_secrets(
		&mut self,
		protection: &NewProtection<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		// The library's passphrase would make the two layers one.
		if self.opens_the_other(protection, Compartment::Replica)? {
			return Err(NodeError::LibraryPassphrase);
		}
		match self.secrets_state()? {
			SecretsState::NotSetUp => {}
			SecretsState::Here | SecretsState::Asked => return Err(NodeError::SecretsSetUp),
			// Keys made here would seal what no other replica reads.
			SecretsState::Elsewhere => return Err(NodeError::SecretsElsewhere),
		}
		// Records that do not say it holds them would have another set up secrets of its own.
		self.check_own_key()?;
		let decryption = DecryptionKey::generate(rng);
		let rosters = self.rosters()?;
		// The personal space's, of a random number, as new keys have, so that two set up at the
		// same time differ; another space's are set up there, by an admin.
		let personal = self.info.personal;
		let epoch = fresh_number(&rosters[&personal].secrets_numbers(), rng);
		let keys = SecretsKeys {
			spaces: BTreeMap::from([(personal, BTreeMap::from([(epoch, Key::generate(rng))]))]),
			decryption: decryption.clone(),
		};
		// Sealed before the key file is locked, as deriving a key from a passphrase, or a
		// keystore's prompt, takes a while.
		let (sealed, key) = SealedSecrets::seal(&keys, protection, rng)?;
		self.write_sealed_secrets(sealed, key)?;
		self.open_secrets(&keys)?;
		let own = self.info.replica;
		let encryption = decryption.encryption_key();
		let stated =
			self.state_held_secrets(&rosters, (&keys, &encryption), Some(own), wall_millis, rng);
		if stated.is_err() {
			self.lock_secrets();
			return stated;
		}
		// Sealed with the key that the records give now.
		let _ = self.settle_secrets_keys(wall_millis, rng);
		Ok(())
	}

	/// Takes the secrets from a copy of a replica made elsewhere, whose secrets `unlocker`
	/// opens, by their passphrase or their keystore entry (technical design: copies of a replica): only the
	/// keys that the records gave the copy's replica, by number and hash, so that no key of
	/// another's making is taken, as a replica takes keys given to it. This replica then
	/// holds them, in its secrets compartment, protected as `protection` says, and the
	/// records say so, as when a replica sets up the secrets. The copy is only read. Returns
	/// how many keys it took. The secrets are unlocked afterwards.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase is the library's; [`NodeError::SecretsSetUp`] if
	/// this replica holds the secrets or asked for them already; an error of the keys if
	/// `unlocker` does not open the copy's secrets; [`NodeError::NoSecretsInCopy`] if they
	/// hold none of the keys that the records gave the copy's replica;
	/// [`NodeError::NewerFormats`] if a later version of NibKeep changed the copy; and the
	/// errors of [`Library::set_up_secrets`].
	pub fn take_secrets_from_copy(
		&mut self,
		copy: &Path,
		unlocker: &Unlocker<'_>,
		protection: &NewProtection<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		if self.opens_the_other(protection, Compartment::Replica)? {
			return Err(NodeError::LibraryPassphrase);
		}
		if matches!(
			self.secrets_state()?,
			SecretsState::Here | SecretsState::Asked
		) {
			return Err(NodeError::SecretsSetUp);
		}
		self.check_own_key()?;
		// A copy that a later version changed is read by none of its files.
		crate::engines::check(copy)?;
		let key_file = KeyFile::read(&copy.join(KEYS))?;
		let copied = key_file.secrets(&key_file.unlock_secrets(unlocker)?)?;
		let replica = Info::read(&copy.join(INFO))?.replica;
		let rosters = self.rosters()?;
		// The copy's keys that the records gave its replica, by number and hash: from the
		// replica it asked, or with a link that a key kept checks.
		let Some(copy_key) = rosters
			.get(&self.info.personal)
			.and_then(|roster| roster.own_key_of(replica))
			.copied()
		else {
			return Err(NodeError::NoSecretsInCopy);
		};
		let given = self.secrets_given(&rosters, Some(&copy_key), None)?;
		let spaces = keep_given(&given, &copied.spaces);
		let taken = spaces.values().map(BTreeMap::len).sum();
		if taken == 0 {
			return Err(NodeError::NoSecretsInCopy);
		}
		self.hold_taken_secrets(&rosters, spaces, protection, wall_millis, rng)?;
		// A copy of a device that was revoked holds no safe key: a new one, now that the
		// records say this replica holds them.
		let _ = self.settle_secrets_keys(wall_millis, rng);
		Ok(taken)
	}

	/// Takes the secrets that the recovery code `code` holds, on a replica that recovers with
	/// it, which holds no secrets yet: the keys that the replica which made the code gave it,
	/// and those linked with them, as [`Library::code_secrets_given`] says, so that no key of
	/// another's making is taken. This replica then holds them, in its secrets compartment,
	/// protected as `protection` says, and the records say so, as when it takes a copy's
	/// secrets; they are unlocked afterwards. Without `protection`, as the person chose, it
	/// takes none ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	/// [`Recovering::finish`] checks first that `protection` is not the library's passphrase.
	///
	/// # Errors
	///
	/// Returns an error if the key file or a change cannot be saved.
	///
	/// [`Recovering::finish`]: super::recovering::Recovering::finish
	pub(super) fn take_code_secrets(
		&mut self,
		code: &RecoveryCode,
		protection: Option<&NewProtection<'_>>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<RecoveredSecrets, NodeError> {
		let rosters = self.rosters()?;
		let spaces = self.secrets_code_opens(&rosters, code)?;
		let taken = spaces.values().map(BTreeMap::len).sum();
		if taken == 0 {
			return Ok(if self.lost_devices_held_secrets(&rosters)? {
				RecoveredSecrets::NotInCode
			} else {
				RecoveredSecrets::NoneHeld
			});
		}
		let Some(protection) = protection else {
			return Ok(RecoveredSecrets::Left);
		};
		self.hold_taken_secrets(&rosters, spaces, protection, wall_millis, rng)?;
		Ok(RecoveredSecrets::Taken { keys: taken })
	}

	/// Whether, before a recovery revokes them, the person's other replicas held secrets: a
	/// replica that the records say holds them, and that is listed and not revoked. Secrets
	/// that only revoked replicas held were lost before.
	fn lost_devices_held_secrets(
		&self,
		rosters: &BTreeMap<SpaceId, Roster>,
	) -> Result<bool, NodeError> {
		let personal = self.info.personal;
		let Some(roster) = rosters.get(&personal) else {
			return Ok(false);
		};
		let holders = roster.secrets_holders();
		let open = self.space(personal)?;
		let log = open.stored.log();
		for holder in holders {
			if holder == self.info.replica {
				continue;
			}
			let keys = roster.keys_of(holder);
			let mut stands = !keys.is_empty();
			for key in keys {
				stands &= log.is_listed(key, &open.keys)? && !log.is_revoked(key, &open.keys)?;
			}
			if stands {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// Makes this replica's secrets compartment, which it did not have, with `spaces`, the
	/// secrets keys that it took from elsewhere, and a new key to receive with, protected as
	/// `protection` says; then states on its records, as `rosters` have them, that it holds
	/// them and takes them from itself, as when it sets up the secrets. They are unlocked
	/// afterwards, but if the records cannot be written.
	fn hold_taken_secrets(
		&mut self,
		rosters: &BTreeMap<SpaceId, Roster>,
		spaces: SpaceKeyMap,
		protection: &NewProtection<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let decryption = DecryptionKey::generate(rng);
		let keys = SecretsKeys {
			spaces,
			decryption: decryption.clone(),
		};
		// Sealed before the key file is locked, as deriving a key from a passphrase, or a
		// keystore's prompt, takes a while.
		let (sealed, key) = SealedSecrets::seal(&keys, protection, rng)?;
		self.write_sealed_secrets(sealed, key)?;
		self.open_secrets(&keys)?;
		let own = self.info.replica;
		let encryption = decryption.encryption_key();
		let stated =
			self.state_held_secrets(rosters, (&keys, &encryption), Some(own), wall_millis, rng);
		if stated.is_err() {
			self.lock_secrets();
		}
		stated
	}

	/// Whether this replica may ask for the secrets from `from`: it holds none, while `from`
	/// does, is not this one, and is not revoked. A replica that asked already may name
	/// another, with [`Library::ask_secrets_from`].
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsNowhere`] if no replica holds the secrets yet,
	/// [`NodeError::SecretsSetUp`] if this one does, [`NodeError::SecretsAsked`] if it asked
	/// already, [`NodeError::WhichDevice`] if `from` holds none, as the records say, or is
	/// revoked; [`NodeError::ForeignKey`] if this replica's record lists a key that it did
	/// not make; and an error if this replica is an AI agent's, or the library cannot be
	/// read.
	pub fn check_may_ask(&self, from: EntityId) -> Result<(), NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		match self.secrets_state()? {
			SecretsState::NotSetUp => return Err(NodeError::SecretsNowhere),
			SecretsState::Here => return Err(NodeError::SecretsSetUp),
			SecretsState::Asked => return Err(NodeError::SecretsAsked),
			SecretsState::Elsewhere => {}
		}
		self.check_holder(from)?;
		self.check_own_key()
	}

	/// Whether this replica's key is its own in the personal space, as only then does what it
	/// says of the secrets count: the only key on its record, listed on no other.
	fn check_own_key(&self) -> Result<(), NodeError> {
		let own = self.replica.signing.verifying_key();
		if self.roster(self.info.personal)?.owner_of(&own) == Some(self.info.replica) {
			Ok(())
		} else {
			Err(NodeError::ForeignKey)
		}
	}

	/// Whether `from` holds the secrets, as the records give them, and is not revoked.
	fn check_holder(&self, from: EntityId) -> Result<(), NodeError> {
		let roster = self.roster(self.info.personal)?;
		if from == self.info.replica
			|| !roster.secrets_holders().contains(&from)
			|| !self.is_remaining(self.info.personal, &roster, from)?
		{
			return Err(NodeError::WhichDevice(
				"that device does not hold the secrets, or has not said so yet: unlock its \
				 secrets once"
					.to_owned(),
			));
		}
		Ok(())
	}

	/// Asks for the secrets that another replica holds, from `from`, which the person
	/// chose: states, at the time `wall_millis`, a new key that opens what is sealed to it,
	/// and that it takes the secrets from `from` alone; then makes this replica's secrets
	/// compartment, protected as `protection` says, and never by the library's passphrase,
	/// with that key and no secrets keys yet. When the person chooses to, `from` gives them,
	/// and this replica takes them while its secrets are unlocked. They are unlocked
	/// afterwards.
	///
	/// # Errors
	///
	/// Returns the errors of [`Library::check_may_ask`]; an error if the passphrase is the
	/// library's, cannot be used, or the keystore fails; or if the key file cannot be read or
	/// written. If the compartment cannot be written after the records were, the replica can
	/// ask again.
	pub fn ask_for_secrets(
		&mut self,
		from: EntityId,
		protection: &NewProtection<'_>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_may_ask(from)?;
		if self.opens_the_other(protection, Compartment::Replica)? {
			return Err(NodeError::LibraryPassphrase);
		}
		let decryption = DecryptionKey::generate(rng);
		let keys = SecretsKeys {
			spaces: SpaceKeyMap::new(),
			decryption: decryption.clone(),
		};
		// Sealed first, as the person may turn a keystore's prompt down, which then leaves no
		// request for a key that nothing holds.
		let (sealed, key) = SealedSecrets::seal(&keys, protection, rng)?;
		// Stated before it is written: a compartment that the records name no key of would
		// hold the secrets of a replica that never asked.
		let rosters = self.rosters()?;
		let encryption = decryption.encryption_key();
		self.state_held_secrets(&rosters, (&keys, &encryption), Some(from), wall_millis, rng)?;
		self.write_sealed_secrets(sealed, key)
	}

	/// Asks for the secrets from another replica than the one this one asked first, which
	/// may have been lost: it takes them from `from` alone from now on.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotAsked`] if this replica did not ask for the secrets,
	/// [`NodeError::WhichDevice`] if `from` holds none or is revoked, and an error if the
	/// change cannot be saved.
	pub fn ask_secrets_from(
		&mut self,
		from: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.secrets_state()? != SecretsState::Asked {
			return Err(NodeError::NotAsked);
		}
		self.check_holder(from)?;
		self.check_own_key()?;
		let own = self.info.replica;
		let personal = self.info.personal;
		let roster = self.roster(personal)?;
		if roster.keys_of(own).is_empty() || roster.secrets_from(own) == Some(from) {
			return Ok(());
		}
		let operation = nibkeep_governance::state_secrets_from(own, from, rng);
		self.commit(personal, vec![operation], wall_millis, rng)
	}

	/// After the first sync of a replica that asked for the secrets as it joined: states, at
	/// the time `wall_millis`, that it takes them from the replica that added it, whose key
	/// is `sponsor`, with the key of the secrets compartment it made, and takes those given;
	/// only its own key may say whom it asks. If the records do not say that the replica that
	/// added it holds the secrets, or this one is an AI agent's, it removes the compartment,
	/// which holds none. Returns whether it asked.
	pub(crate) fn ask_as_joined(
		&mut self,
		sponsor: &VerifyingKey,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<bool, NodeError> {
		let roster = self.roster(self.info.personal)?;
		let from = roster
			.owner_of(sponsor)
			.filter(|&from| roster.secrets_holders().contains(&from));
		let keys = match (&self.secrets_compartment, from) {
			(Some(compartment), Some(_)) if !self.is_ai_agent()? => {
				Some(self.key_file_on_disk()?.secrets(compartment)?)
			}
			_ => None,
		};
		let Some(keys) = keys else {
			self.remove_secrets_compartment()?;
			return Ok(false);
		};
		let encryption = keys.decryption.encryption_key();
		let rosters = self.rosters()?;
		self.state_held_secrets(&rosters, (&keys, &encryption), from, wall_millis, rng)?;
		self.settle_secrets(wall_millis, rng)?;
		Ok(true)
	}

	/// Locks the secrets, and removes the secrets compartment from the key file.
	fn remove_secrets_compartment(&mut self) -> Result<(), NodeError> {
		self.lock_secrets();
		self.update_key_file(|key_file| {
			key_file.remove_secrets();
			Ok(())
		})
	}

	/// Writes a secrets compartment sealed apart in the key file as it is on the disk, unless
	/// it has one, and keeps it open. One that is not written leaves nothing in the keystore.
	fn write_sealed_secrets(
		&mut self,
		sealed: SealedSecrets,
		key: CompartmentKey,
	) -> Result<(), NodeError> {
		let mut sealed = Some(sealed);
		self.update_key_file(|key_file| {
			if key_file.secrets_protection().is_some() {
				return Err(NodeError::SecretsSetUp);
			}
			if let Some(sealed) = sealed.take() {
				key_file.put_secrets(sealed);
			}
			Ok(())
		})?;
		self.secrets_compartment = Some(key);
		Ok(())
	}

	/// Gives the secrets keys this replica holds to `replica`, which asked for them from this
	/// one, sealed to the key it asked with, at the time `wall_millis`: in each space that
	/// both are replicas of, those keys that it was not given yet; in a space where it asks
	/// nobody, as in every space but the personal one, linked with the personal space's key.
	/// The secrets must be unlocked: the person confirmed. It checks all of it before it gives
	/// any, but for those other spaces, which follow, each on its own: one where giving fails
	/// is left to the next time the secrets settle. Returns how many keys it gave.
	///
	/// # Errors
	///
	/// Returns [`NodeError::SecretsLocked`] if the secrets are locked, and
	/// [`NodeError::SecretsElsewhere`] or [`NodeError::SecretsNowhere`] if this replica holds
	/// none; [`NodeError::NotAsked`] if `replica` did not ask for them;
	/// [`NodeError::WhichDevice`] if it asked another, or is this one, an AI agent's,
	/// revoked, or no replica of the user's; [`NodeError::ForeignKey`] if this replica's
	/// record lists a key that it did not make, so that what it gives would not count; and
	/// an error if a change cannot be saved, in which case the spaces before it were given
	/// theirs.
	pub fn give_secrets(
		&mut self,
		replica: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.check_not_ai_agent("give secrets")?;
		let personal = self.info.personal;
		let own = self.info.replica;
		if !self.secrets_unlocked(personal) {
			return Err(match self.secrets_state()? {
				SecretsState::Here => NodeError::SecretsLocked,
				SecretsState::NotSetUp => NodeError::SecretsNowhere,
				SecretsState::Asked | SecretsState::Elsewhere => NodeError::SecretsElsewhere,
			});
		}
		if replica == own {
			return Err(NodeError::WhichDevice(
				"this device holds the secrets already".to_owned(),
			));
		}
		let rosters = self.rosters()?;
		let roster = &rosters[&personal];
		if roster.keys_of(replica).is_empty() {
			return Err(NodeError::WhichDevice(
				"that is no device of yours".to_owned(),
			));
		}
		let open = self.space(personal)?;
		for key in roster.keys_of(replica) {
			if open.stored.log().is_ai_agent(key, &open.keys)? {
				return Err(NodeError::WhichDevice(
					"an AI agent never gets the secrets".to_owned(),
				));
			}
		}
		if !self.is_remaining(personal, roster, replica)? {
			return Err(NodeError::WhichDevice("that device is revoked".to_owned()));
		}
		match (
			roster.secrets_encryption_of(replica),
			roster.secrets_from(replica),
		) {
			(Some(_), Some(from)) if from == own => {}
			(Some(_), Some(_)) => {
				return Err(NodeError::WhichDevice(
					"that device asks another of yours for the secrets".to_owned(),
				));
			}
			_ => return Err(NodeError::NotAsked),
		}
		self.check_own_key()?;
		let mut changes = Vec::new();
		for (&space, roster) in &rosters {
			// A space where it has not asked yet: it asks there as it next settles.
			let Some(encryption) = roster
				.secrets_encryption_of(replica)
				.filter(|_| roster.secrets_from(replica) == Some(own))
			else {
				continue;
			};
			let held = self.space(space)?.keys.secrets_keys();
			let has = given_by_the_one_asked(roster, replica);
			let operations = gifts(&has, (space, replica), encryption, held, rng);
			if !operations.is_empty() {
				changes.push((space, operations));
			}
		}
		let given: usize = changes.iter().map(|(_, operations)| operations.len()).sum();
		for (space, operations) in changes {
			self.commit(space, operations, wall_millis, rng)?;
		}
		// In the other spaces it names no replica to ask: their keys go linked with the personal
		// space's, as the secrets would next settle. What fails here, that settling gives.
		let passed = self.give_other_spaces_secrets(replica, wall_millis, rng);
		Ok(given + passed)
	}

	/// Asks for the secrets of a shared space from `from`, the record there of a replica of
	/// another member that holds them, which the person chose: states, at the time
	/// `wall_millis`, this replica's secrets encryption key in the space and that it takes the
	/// space's secrets from `from`. When the person of `from` chooses to, it gives them, with
	/// [`Library::give_space_secrets`], and this replica takes them while its secrets are
	/// unlocked. This user's own replicas pass a space's secrets on to each other by
	/// themselves, and need not ask.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for a space with no members,
	/// [`NodeError::SetUpSecretsFirst`] if this replica has no secrets compartment,
	/// [`NodeError::SecretsLocked`] if it is locked, [`NodeError::SecretsSetUp`] if this
	/// replica holds the space's secrets, [`NodeError::WhichDevice`] if `from` holds none, as
	/// the records say, or is this replica or ended there, [`NodeError::Removed`] if this
	/// replica's member left or was removed, [`NodeError::ForeignKey`] if its record lists a
	/// key it did not make, and an error if this replica is an AI agent's or the change
	/// cannot be saved.
	pub fn ask_for_space_secrets(
		&mut self,
		space: SpaceId,
		from: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.check_not_ai_agent("hold secrets")?;
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		let own_key = self.replica.signing.verifying_key();
		{
			let open = self.space(space)?;
			if open.stored.log().is_removed(&own_key, &open.keys)? {
				return Err(NodeError::Removed);
			}
		}
		let compartment = self.open_compartment()?;
		if self.secrets_unlocked(space) {
			return Err(NodeError::SecretsSetUp);
		}
		let roster = self.roster(space)?;
		let own = self.own_record(space)?;
		let holds =
			roster.secrets_holders().contains(&from) && self.is_remaining(space, &roster, from)?;
		if from == own || !holds {
			return Err(NodeError::WhichDevice(
				"that device does not hold this space's secrets".to_owned(),
			));
		}
		if roster.owner_of(&own_key) != Some(own) {
			return Err(NodeError::ForeignKey);
		}
		let encryption = self
			.key_file_on_disk()?
			.secrets(&compartment)?
			.decryption
			.encryption_key();
		let mut operations = Vec::new();
		if roster.secrets_encryption_of(own) != Some(&encryption) {
			operations.push(nibkeep_governance::state_secrets_key(own, &encryption, rng));
		}
		if roster.secrets_from(own) != Some(from) {
			operations.push(nibkeep_governance::state_secrets_from(own, from, rng));
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(space, operations, wall_millis, rng)
	}

	/// Gives the secrets of a shared space that this replica holds to `replica`, the record
	/// there of another member's replica that asked this one for them, sealed to the key it
	/// asked with, at the time `wall_millis`: those keys it was not given yet. The secrets
	/// must be unlocked: the person confirmed. From then on this replica gives it the space's
	/// new keys by itself, as it does to every replica known to hold them. Returns how many
	/// keys it gave.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NeverShared`] for a space with no members,
	/// [`NodeError::SecretsLocked`] if the secrets are locked,
	/// [`NodeError::SpaceSecretsElsewhere`] if this replica holds none of the space's;
	/// [`NodeError::NotAsked`] if `replica` did not ask for them; [`NodeError::WhichDevice`]
	/// if it asked another, or is this one, an AI agent's, or ended or no replica of the
	/// space; [`NodeError::ForeignKey`] if this replica's record lists a key it did not make;
	/// and an error if the change cannot be saved.
	pub fn give_space_secrets(
		&mut self,
		space: SpaceId,
		replica: EntityId,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<usize, NodeError> {
		self.check_not_ai_agent("give secrets")?;
		self.check_shared(space)?;
		self.check_not_revoked(space)?;
		{
			let open = self.space(space)?;
			let own_key = self.replica.signing.verifying_key();
			if open.stored.log().is_removed(&own_key, &open.keys)? {
				return Err(NodeError::Removed);
			}
		}
		if !self.secrets_unlocked(space) {
			return Err(if self.secrets_compartment.is_some() {
				NodeError::SpaceSecretsElsewhere
			} else {
				NodeError::SecretsLocked
			});
		}
		let roster = self.roster(space)?;
		let own = self.own_record(space)?;
		if replica == own {
			return Err(NodeError::WhichDevice(
				"this device holds the secrets already".to_owned(),
			));
		}
		if roster.keys_of(replica).is_empty() {
			return Err(NodeError::WhichDevice(
				"that is no device of the space".to_owned(),
			));
		}
		{
			let open = self.space(space)?;
			for key in roster.keys_of(replica) {
				if open.stored.log().is_ai_agent(key, &open.keys)? {
					return Err(NodeError::WhichDevice(
						"an AI agent never gets the secrets".to_owned(),
					));
				}
			}
		}
		if !self.is_remaining(space, &roster, replica)? {
			return Err(NodeError::WhichDevice(
				"that device left the space, or was removed or revoked".to_owned(),
			));
		}
		let encryption = match (
			roster.secrets_encryption_of(replica),
			roster.secrets_from(replica),
		) {
			(Some(encryption), Some(from)) if from == own => encryption.clone(),
			(Some(_), Some(_)) => {
				return Err(NodeError::WhichDevice(
					"that device asks another for the secrets".to_owned(),
				));
			}
			_ => return Err(NodeError::NotAsked),
		};
		if roster.owner_of(&self.replica.signing.verifying_key()) != Some(own) {
			return Err(NodeError::ForeignKey);
		}
		let has = given_by_the_one_asked(&roster, replica);
		let held = self.space(space)?.keys.secrets_keys();
		let operations = gifts(&has, (space, replica), &encryption, held, rng);
		let given = operations.len();
		if given > 0 {
			self.commit(space, operations, wall_millis, rng)?;
		}
		Ok(given)
	}

	/// While the secrets compartment is open: takes the secrets keys given to this replica,
	/// by the replica it takes the secrets from, sealed to the key it asked with, if their
	/// hashes are those stated; saves them, and opens the secret values. Then states,
	/// at the time `wall_millis`, what the records do not say yet: that key, the replica it
	/// takes the secrets from, and for one that set them up, the keys it holds; if that
	/// cannot be saved, the next time tries again. A compartment that holds no secrets keys,
	/// for a key that the records do not say it asks with, is removed, so that the replica can
	/// ask anew: one that joined and stopped before it said so, or whose record lists a key
	/// that it did not make. Does nothing while the compartment is closed, and on a replica
	/// that is an AI agent's, revoked or not listed; if another program protected the
	/// secrets anew, they are locked.
	///
	/// # Errors
	///
	/// Returns an error if the key file or a space cannot be read, or the keys taken saved.
	pub fn settle_secrets(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let Some(compartment) = self.secrets_compartment.clone() else {
			return Ok(());
		};
		if self.is_ai_agent()? || !matches!(self.is_revoked(), Ok(false)) {
			return Ok(());
		}
		let Ok(mut keys) = self.key_file_on_disk()?.secrets(&compartment) else {
			// Another program protected them anew: they are locked until unlocked again.
			self.lock_secrets();
			return Ok(());
		};
		let own = self.info.replica;
		let rosters = self.rosters()?;
		if keys.spaces.is_empty()
			&& rosters.get(&self.info.personal).is_some_and(|roster| {
				roster.secrets_encryption_of(own) != Some(&keys.decryption.encryption_key())
					|| roster.secrets_from(own).is_none()
			}) {
			return self.remove_secrets_compartment();
		}
		// The keys given to it: by the replica it asked, or with a link that a key it holds
		// checks, from any replica.
		let recipient = nibkeep_governance::secrets_recipient(&keys.decryption.encryption_key());
		let given = self.secrets_given(&rosters, None, Some(recipient))?;
		// Written only for a key that opens and is not held: a key that does not open is left
		// alone, each time.
		if take_given(&given, &mut keys.clone()) > 0 {
			// Read again and written while no other program writes the key file.
			let mut taken = None;
			self.update_key_file(|key_file| {
				let mut keys = key_file.secrets(&compartment)?;
				take_given(&given, &mut keys);
				keys.spaces.retain(|_, held| !held.is_empty());
				key_file.update_secrets(&keys, &compartment, rng)?;
				taken = Some(keys);
				Ok(())
			})?;
			if let Some(taken) = taken {
				keys = taken;
				self.open_secrets(&keys)?;
			}
		}
		// The replica it asked, in every space; a replica whose records do not say so yet, as
		// when setting up its secrets was cut short, takes them from itself.
		let from = rosters
			.get(&self.info.personal)
			.and_then(|roster| roster.secrets_from(own))
			.or_else(|| keys.spaces.contains_key(&self.info.personal).then_some(own));
		let encryption = keys.decryption.encryption_key();
		let _ = self.state_held_secrets(&rosters, (&keys, &encryption), from, wall_millis, rng);
		// A new secrets key where none is safe, and the keys it holds to the replicas known to
		// hold the secrets that lack them; what fails, the next time tries again.
		let _ = self.settle_secrets_keys(wall_millis, rng);
		Ok(())
	}

	/// States on this replica's record, in each space where its key is its own, what the
	/// records do not say yet: its secrets encryption key, in the personal space the replica
	/// it takes the secrets from, if `from` names one, and, where it takes them from itself,
	/// the secrets keys of `keys` sealed to it; in one change of each space. Where another key is listed on its
	/// record, nothing it says of the secrets counts, so it says nothing.
	fn state_held_secrets(
		&mut self,
		rosters: &BTreeMap<SpaceId, Roster>,
		(keys, encryption): (&SecretsKeys, &EncryptionKey),
		from: Option<EntityId>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let personal = self.info.personal;
		let own_key = self.replica.signing.verifying_key();
		// The personal space first, whose errors are told; another space where this fails
		// says it the next time its secrets settle.
		let mut spaces: Vec<SpaceId> = rosters.keys().copied().collect();
		spaces.sort_by_key(|&space| space != personal);
		for space in spaces {
			let roster = &rosters[&space];
			// Another space this replica left, or that cannot be read now, says nothing.
			let ended =
				|| -> Result<bool, NodeError> {
					let open = self.space(space)?;
					let log = open.stored.log();
					Ok(log.is_revoked(&own_key, &open.keys)?
						|| log.is_removed(&own_key, &open.keys)?)
				};
			if space != personal && !matches!(ended(), Ok(false)) {
				continue;
			}
			let own = match self.own_record(space) {
				Ok(own) => own,
				Err(error) if space == personal => return Err(error),
				Err(_) => continue,
			};
			if roster.owner_of(&own_key) != Some(own) {
				continue;
			}
			let mut operations = Vec::new();
			let anew = roster.secrets_encryption_of(own) != Some(encryption);
			if anew {
				operations.push(nibkeep_governance::state_secrets_key(own, encryption, rng));
			}
			// Whom it asked, the personal space says; in another, the replica that gave it a key
			// it holds, as from any replica with a link, so that the records say it holds them.
			let from = if space == personal {
				from
			} else if roster.secrets_from(own).is_none() && !anew {
				let held = keys.spaces.get(&space);
				let recipient = nibkeep_governance::secrets_recipient(encryption);
				roster
					.secrets_gifts(own)
					.iter()
					.filter(|gift| {
						gift.counts && gift.sealed.recipient == recipient && gift.author != own_key
					})
					.filter(|gift| {
						held.and_then(|held| held.get(&gift.sealed.epoch))
							.is_some_and(|key| key_hash(key) == gift.sealed.hash)
					})
					.find_map(|gift| roster.replica_of(&gift.author))
			} else {
				None
			};
			if let Some(from) = from
				&& roster.secrets_from(own) != Some(from)
			{
				operations.push(nibkeep_governance::state_secrets_from(own, from, rng));
			}
			if from.or(roster.secrets_from(own)) == Some(own) {
				let held = keys.spaces.get(&space).into_iter().flatten();
				let held = held.map(|(&epoch, key)| (epoch, key));
				// Those that no statement gives it, as others may have, with a link; keys given
				// to the key before count no more.
				let recipient = nibkeep_governance::secrets_recipient(encryption);
				let stated: BTreeSet<(u64, Hash)> = roster
					.secrets_gifts(own)
					.iter()
					.filter(|gift| !anew && gift.counts && gift.sealed.recipient == recipient)
					.map(|gift| (gift.sealed.epoch, gift.sealed.hash))
					.collect();
				operations.extend(gifts(&stated, (space, own), encryption, held, rng));
			}
			if !operations.is_empty() {
				let committed = self.commit(space, operations, wall_millis, rng);
				if space == personal {
					committed?;
				}
			}
		}
		Ok(())
	}

	/// Unlocks the secrets: the secrets keys, after the person has confirmed, which open
	/// the secret values of the spaces and seal new ones. Lock them again soon, with
	/// [`Library::lock_secrets`]. Secrets keys that another replica gave this one since are
	/// taken by [`Library::settle_secrets`], which apps call next.
	///
	/// # Errors
	///
	/// Returns an error if the library has no secrets compartment, `unlocker` does not
	/// unlock it, or the key file cannot be read. A replica that asked for the secrets, and
	/// was not given them yet, unlocks a compartment without keys.
	pub fn unlock_secrets(&mut self, unlocker: &Unlocker<'_>) -> Result<(), NodeError> {
		let key = Self::secrets_key(&self.directory, unlocker)?;
		self.unlock_secrets_with(&key)
	}

	/// The key of the secrets of the library in `directory`, after the person has confirmed,
	/// without opening the library: a keystore's prompt may take a while, during which other
	/// programs may use the library. [`Library::unlock_secrets_with`] unlocks them with it.
	///
	/// # Errors
	///
	/// Returns an error if the library has no secrets compartment, `unlocker` does not
	/// unlock it, or the key file cannot be read.
	pub fn secrets_key(
		directory: &Path,
		unlocker: &Unlocker<'_>,
	) -> Result<CompartmentKey, NodeError> {
		Ok(KeyFile::read(&directory.join(KEYS))?.unlock_secrets(unlocker)?)
	}

	/// Unlocks the secrets with their key, as [`Library::unlock_secrets`] does.
	///
	/// # Errors
	///
	/// Returns an error if the library has no secrets compartment, `key` does not open it, as
	/// when another program protected it anew since, or the key file cannot be read.
	pub fn unlock_secrets_with(&mut self, key: &CompartmentKey) -> Result<(), NodeError> {
		// Another program may have given the library its secrets keys since it was opened.
		let keys = self.key_file_on_disk()?.secrets(key)?;
		self.open_secrets(&keys)?;
		self.secrets_compartment = Some(key.clone());
		Ok(())
	}

	/// Unlocks the secrets with the key that a library opened again from [`Closed`] kept,
	/// if it kept one: whether they are unlocked now. If another program protected them anew
	/// since, the key opens them no longer, and is forgotten.
	///
	/// # Errors
	///
	/// Returns an error if the key file cannot be read, or the secret values cannot be
	/// opened.
	///
	/// [`Closed`]: crate::Closed
	pub fn unlock_kept_secrets(&mut self) -> Result<bool, NodeError> {
		let personal = self.personal_space();
		if self.secrets_unlocked(personal) {
			return Ok(true);
		}
		let Some(compartment) = self.secrets_compartment.clone() else {
			return Ok(false);
		};
		let Ok(keys) = self.key_file_on_disk()?.secrets(&compartment) else {
			self.secrets_compartment = None;
			return Ok(false);
		};
		self.open_secrets(&keys)?;
		self.secrets_compartment = Some(compartment);
		// A replica that asked holds no keys until they are given.
		Ok(self.secrets_unlocked(personal))
	}

	/// Locks the secrets: forgets the secrets keys and the secret values they opened.
	pub fn lock_secrets(&mut self) {
		self.secrets_compartment = None;
		for open in self.spaces.values_mut() {
			open.keys.forget_secrets();
			open.stored.lock_secrets();
		}
	}

	/// Gives the spaces their secrets keys, and opens their secret values.
	pub(super) fn open_secrets(&mut self, keys: &SecretsKeys) -> Result<(), NodeError> {
		for (space, epochs) in &keys.spaces {
			let Some(open) = self.spaces.get_mut(space) else {
				continue;
			};
			for (&epoch, key) in epochs {
				open.keys.insert_secrets(epoch, key.clone());
			}
			if let Err(error) = open.stored.unlock_secrets(&open.keys) {
				self.lock_secrets();
				return Err(error.into());
			}
		}
		// Each seals new secret values with its newest safe key.
		for &space in keys.spaces.keys() {
			if self.spaces.contains_key(&space) {
				let chosen = self
					.roster(space)
					.and_then(|roster| self.choose_secrets_key_with(space, &roster));
				if let Err(error) = chosen {
					self.lock_secrets();
					return Err(error);
				}
			}
		}
		Ok(())
	}

	/// The key file as it is on the disk.
	pub(super) fn key_file_on_disk(&self) -> Result<KeyFile, NodeError> {
		Ok(KeyFile::read(&self.directory.join(KEYS))?)
	}

	/// Whether a new passphrase for one compartment opens the other, the replica's or the
	/// secrets', as the key file on the disk has them: the two would then be one layer.
	pub(super) fn opens_the_other(
		&self,
		protection: &NewProtection<'_>,
		other: Compartment,
	) -> Result<bool, NodeError> {
		let NewProtection::Passphrase(passphrase, _) = protection else {
			return Ok(false);
		};
		let key_file = self.key_file_on_disk()?;
		let unlocker = Unlocker::Passphrase(passphrase);
		Ok(match other {
			Compartment::Replica => {
				matches!(key_file.replica_protection(), Protection::Passphrase { .. })
					&& key_file.unlock_replica(&unlocker).is_ok()
			}
			Compartment::Secrets => {
				matches!(
					key_file.secrets_protection(),
					Some(Protection::Passphrase { .. })
				) && key_file.unlock_secrets(&unlocker).is_ok()
			}
		})
	}

	/// Changes the key file as it is on the disk, read again while no other program writes
	/// it, with `change`, and writes it, so that two programs never replace each other's
	/// keys. If the change fails, or writing does, the file is put back as it was, and a
	/// keystore key that only the change made is deleted. Keystore keys that the library's
	/// copy had still to delete are deleted.
	pub(super) fn update_key_file(
		&mut self,
		change: impl FnOnce(&mut KeyFile) -> Result<(), NodeError>,
	) -> Result<(), NodeError> {
		let lock_path = self.directory.join(KEYS_LOCK);
		let lock = File::options()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&lock_path)
			.map_err(NodeError::file(&lock_path))?;
		lock.lock().map_err(NodeError::file(&lock_path))?;
		let path = self.directory.join(KEYS);
		let mut key_file = KeyFile::read(&path)?;
		let before = key_file.clone();
		if let Err(error) = change(&mut key_file) {
			key_file.restore(before);
			return Err(error);
		}
		key_file.take_obsolete(&mut self.key_file);
		if let Err(error) = key_file.write(&path) {
			key_file.restore(before);
			self.key_file.take_obsolete(&mut key_file);
			return Err(error.into());
		}
		self.key_file = key_file;
		// Closing the file releases the lock.
		drop(lock);
		Ok(())
	}
}

/// The secrets keys that the replica that `replica` takes the secrets from gives it, by
/// number and hash.
fn given_by_the_one_asked(roster: &Roster, replica: EntityId) -> BTreeSet<(u64, Hash)> {
	roster
		.secrets_given_to(replica)
		.map(|sealed| (sealed.epoch, sealed.hash))
		.collect()
}

/// The operations that give `replica` of `space` the secrets keys of `held` that `has` does
/// not name, by number and hash, sealed to `encryption`.
fn gifts<'a>(
	has: &BTreeSet<(u64, Hash)>,
	(space, replica): (SpaceId, EntityId),
	encryption: &EncryptionKey,
	held: impl Iterator<Item = (u64, &'a Key)>,
	rng: &mut impl CryptoRng,
) -> Vec<nibkeep_model::Operation> {
	let missing = held.filter(|(epoch, key)| !has.contains(&(*epoch, key_hash(key))));
	sealed_secrets_keys(missing, (space, replica), encryption, rng)
		.iter()
		.map(|sealed| nibkeep_governance::give_secrets_key(replica, sealed, rng))
		.collect()
}

/// The secrets keys of `held`, of `space`, sealed to `encryption`, the secrets encryption
/// key of `replica`, each linked with itself, so that the replicas that hold it know that
/// this one does.
pub(super) fn sealed_secrets_keys<'a>(
	held: impl Iterator<Item = (u64, &'a Key)>,
	(space, replica): (SpaceId, EntityId),
	encryption: &EncryptionKey,
	rng: &mut impl CryptoRng,
) -> Vec<SealedSecretsKey> {
	held.map(|(epoch, key)| {
		let hash = key_hash(key);
		SealedSecretsKey::seal(
			encryption,
			space,
			replica,
			(epoch, key, hash),
			(space, epoch, key),
			rng,
		)
	})
	.collect()
}
