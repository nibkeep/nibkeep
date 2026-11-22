//! Recovery codes: a person's printed code, which works as one more device that lives on
//! paper. It is named in the personal space, and given the keys of the space as a replica
//! is, until it is revoked ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: ../../../../docs/Technical-design.md#111-recovery-codes

use std::collections::BTreeSet;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, RecoveryCode, VerifyingKey, key_hash};
use nibkeep_governance::{
	RECOVERY_CODE, RecoveryEntry, RecoveryStatement, Roster, SPACE_ENTRY, SpaceEntry,
	recovery_record, state_recovery_code, state_recovery_entry,
};
use nibkeep_keys::SpaceKeyMap;
use nibkeep_model::{Name, Value, fields};

use super::Library;
use super::policies::assert;
use super::revocation::Makers;
use crate::NodeError;
use crate::recovery_file::{RECOVERY_FILE, RecoveryFile};

/// A recovery code of the person's, as their personal space names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCodeInfo {
	/// The code's record in the personal space.
	pub record: EntityId,
	/// Its short name, printed beside the code, which tells one sheet of paper from another.
	pub name: String,
	/// Its recovery key.
	pub key: VerifyingKey,
	/// When it was made, in milliseconds since 1970, as the device that made it says.
	pub made_at: u64,
	/// The replica that made it: the one whose key signed its statement.
	pub made_by: Option<EntityId>,
	/// The name of the device that made it.
	pub made_on: Option<String>,
	/// Whether it was revoked: it is given no key made since, and brings no device back.
	pub revoked: bool,
	/// Whether the replica that made it gave it a secrets key of the personal space that a
	/// replica which holds the secrets was given too, sealed to the secrets encryption key
	/// that it signed: whether it opens the secrets, as a recovery with it takes them.
	pub holds_secrets: bool,
	/// For a code that stands, the names of the person's backups that carry it: whose last
	/// update, as its account in the personal space tells, wrote the personal space with the
	/// code named, and so the recovery file with the code's entry.
	pub carried_by: Vec<String>,
	/// For a code that stands, the names of the person's backups still to update before they
	/// carry it, with which the code brings nothing back yet.
	pub to_update: Vec<String>,
	/// For a code that stands and that a newer one replaces, the newer one's name: it is
	/// revoked once every backup carries the newer one, as a backup not updated since opens
	/// with this one alone; until then its paper is to be kept.
	pub replaced_by: Option<String>,
}

/// What the person's codes call for of a recovery file, in a replica's folder or a backup's.
pub(crate) enum RecoveryFileNeed {
	/// To keep it as it is, as it holds the personal space's first key for the codes that
	/// stand and no other, or as there is none and no code stands.
	Kept,
	/// To write this one, sealed anew.
	Written(RecoveryFile),
	/// To remove it, as no code stands.
	Removed,
}

/// What a check of a recovery code on a device that works found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCheck {
	/// The code, if the personal space names it: one of the person's own.
	pub code: Option<RecoveryCodeInfo>,
	/// For each space that names it, the personal one or another, for whichever member, the
	/// keys that it opens and those it lacks.
	pub spaces: Vec<RecoveryKeys>,
}

/// The keys of a space that a recovery code opens, of those that this replica holds and that
/// were given out. The first key is never given out: it reaches a code another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryKeys {
	/// The space.
	pub space: SpaceId,
	/// The numbers of the keys that the records seal to the code, and that open with it to
	/// the keys that this replica holds: on any of this replica's member's records for the
	/// code there, as a member made direct, or one through a group, may have more than one,
	/// or else on one member's.
	pub opens: Vec<u64>,
	/// The numbers of the keys that the code was not given, as those made after it was
	/// revoked, or that do not open with it.
	pub lacks: Vec<u64>,
	/// For a space other than the personal one, whether the code is given that space: an
	/// entry on the code's record, in the personal space or in a group that the space is
	/// reached through, whose first key opens with the code to the one that this replica
	/// holds. `None` for the personal space, whose first key reaches a code another way, and
	/// where the personal space does not name the code.
	pub entry: Option<bool>,
	/// While this replica's secrets of the space are unlocked, the numbers of the secrets
	/// keys that it holds and that the code opens, and of those that the code lacks; `None`
	/// while they are locked, or the replica holds none.
	pub secrets: Option<(Vec<u64>, Vec<u64>)>,
}

impl Library {
	/// The person's recovery codes, as their personal space names them, the oldest first,
	/// revoked ones too: every code that the space names, whichever member its statement
	/// names, as every replica there is the user's, and each is given the space's keys.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn recovery_codes(&self) -> Result<Vec<RecoveryCodeInfo>, NodeError> {
		let roster = self.roster(self.info.personal)?;
		let mut codes: Vec<_> = roster.codes().collect();
		codes.sort_by_key(|code| (code.at, code.change));
		if codes.is_empty() {
			return Ok(Vec::new());
		}
		// The newest code of the person's own that stands replaces those before it: one named
		// for another member, as by a device that copied the code's statement, replaces none.
		let newest = codes
			.iter()
			.rfind(|code| code.stands() && code.statement.member == self.info.user)
			.map(|code| (code.record, RecoveryCode::name_of(&code.statement.key)));
		let replicas = self.replicas()?;
		// Each backup of the person's, with the heads of the personal space's changes that its
		// latest account says it holds: it carries a code whose naming one of them is or builds
		// on, as walked down to that change alone.
		let accounts = self.backup_accounts(self.info.personal)?;
		let mut backups: Vec<(String, Vec<nibkeep_log::ChangeId>)> = Vec::new();
		for backup in self.backups()? {
			let heads = accounts
				.get(&backup.id)
				.map_or_default(|account| account.head.heads.clone());
			backups.push((backup.name, heads));
		}
		let personal_log = self.space(self.info.personal)?.stored.log();
		let holds = |heads: &[nibkeep_log::ChangeId], change: nibkeep_log::ChangeId| {
			let mut found = false;
			for &head in heads {
				found |= personal_log.builds_on_all(head, &[change])?;
			}
			Ok::<_, NodeError>(found)
		};
		let mut carried: Vec<Vec<bool>> = Vec::new();
		for code in &codes {
			let mut of_code = Vec::new();
			for (_, heads) in &backups {
				of_code.push(code.stands() && holds(heads, code.change)?);
			}
			carried.push(of_code);
		}
		// The secrets keys that the replicas which hold the secrets were given: a key of
		// someone's own making, given to a code, opens nothing of theirs.
		let held: std::collections::BTreeSet<(u64, nibkeep_base::Hash)> = roster
			.secrets_holders()
			.into_iter()
			.flat_map(|holder| roster.secrets_given_to(holder))
			.map(|sealed| (sealed.epoch, sealed.hash))
			.collect();
		Ok(codes
			.into_iter()
			.zip(carried)
			.map(|(code, carried)| {
				let made_by = roster.replica_of(&code.author);
				let made_on = replicas
					.iter()
					.find(|replica| Some(replica.id) == made_by)
					.and_then(|replica| replica.device_name.clone());
				RecoveryCodeInfo {
					record: code.record,
					name: RecoveryCode::name_of(&code.statement.key),
					key: code.statement.key,
					made_at: code.at.millis(),
					made_by,
					made_on,
					revoked: !code.stands(),
					holds_secrets: {
						let recipient = nibkeep_governance::secrets_recipient(
							&code.statement.secrets_encryption,
						);
						// Given by the replica that made it, as a recovery takes them.
						let by_maker = made_by
							.is_some_and(|maker| roster.own_key_of(maker) == Some(&code.author));
						by_maker
							&& roster.secrets_gifts(code.record).iter().any(|gift| {
								gift.counts
									&& gift.author == code.author
									&& gift.sealed.recipient == recipient
									&& held.contains(&(gift.sealed.epoch, gift.sealed.hash))
							})
					},
					carried_by: backups
						.iter()
						.zip(&carried)
						.filter(|(_, carries)| **carries)
						.map(|((name, _), _)| name.clone())
						.collect(),
					to_update: backups
						.iter()
						.zip(&carried)
						.filter(|(_, carries)| code.stands() && !**carries)
						.map(|((name, _), _)| name.clone())
						.collect(),
					replaced_by: newest
						.as_ref()
						.filter(|(record, _)| code.stands() && *record != code.record)
						.map(|(_, name)| name.clone()),
				}
			})
			.collect())
	}

	/// What the person's codes call for of a recovery file, as `existing` stands, as
	/// [`RecoveryFileNeed`] says: an entry for each code of the person's own that stands,
	/// with the personal space's ID, first change and first key's hash, and the person, and
	/// the code's signature of these, which the personal space states on the code's record.
	/// A code without one, which a version before it named, gets no entry.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub(crate) fn recovery_file_anew(
		&self,
		existing: Option<&RecoveryFile>,
	) -> Result<RecoveryFileNeed, NodeError> {
		let personal = self.info.personal;
		let roster = self.roster(personal)?;
		let Some((first, hash, root)) = self.what_codes_sign()? else {
			return Ok(RecoveryFileNeed::Kept);
		};
		// The person's own codes alone: a recovery takes the person from the entry that its
		// code opens, and a code's statement copied onto a record for another member, as its
		// signature allows, opens with the same code. Each with the entry that it signed for
		// the space, which the roster takes only for this space and the code's record, and so
		// its member.
		let standing: Vec<(&nibkeep_governance::Code, &RecoveryEntry)> = roster
			.codes()
			.filter(|code| code.stands() && code.statement.member == self.info.user)
			.filter_map(|code| {
				code.entries
					.iter()
					.find(|entry| entry.root == root && entry.hash == hash)
					.map(|entry| (code, entry))
			})
			.collect();
		if standing.is_empty() {
			return Ok(if existing.is_some() {
				RecoveryFileNeed::Removed
			} else {
				RecoveryFileNeed::Kept
			});
		}
		// Kept where it holds an entry for each code that stands, as it should be, and no
		// other.
		let kept = existing.is_some_and(|file| {
			let entries = file.entries();
			entries.len() == standing.len()
				&& standing.iter().all(|(code, signed)| {
					entries.iter().any(|(record, entry, signature)| {
						*record == code.record
							&& entry.space == personal
							&& entry.member == code.statement.member
							&& entry.root == root
							&& entry.hash == hash
							&& *signature == signed.signature
					})
				})
		});
		if kept {
			return Ok(RecoveryFileNeed::Kept);
		}
		let mut rng = UnwrapErr(SysRng);
		let entries: Vec<(EntityId, SpaceEntry, nibkeep_crypto::Signature)> = standing
			.iter()
			.map(|(code, signed)| {
				let entry = SpaceEntry::seal(
					&code.statement.encryption,
					(personal, code.statement.member, root),
					code.record,
					(&first, hash),
					&mut rng,
				);
				(code.record, entry, signed.signature)
			})
			.collect();
		Ok(RecoveryFileNeed::Written(RecoveryFile::new(entries)))
	}

	/// What a code signs of the personal space, as its entry names it, and the first key that
	/// the entry seals: the first key, its hash, and the space's first change; `None` before
	/// the replica holds them.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	fn what_codes_sign(&self) -> Result<Option<(nibkeep_crypto::Key, Hash, Hash)>, NodeError> {
		let open = self.space(self.info.personal)?;
		let first = open.keys.space_keys().find(|(epoch, _)| *epoch == 0);
		Ok(match (first, open.stored.log().root()?) {
			(Some((_, first)), Some(root)) => Some((first.clone(), key_hash(first), *root.hash())),
			_ => None,
		})
	}

	/// Keeps the recovery file in the replica's folder as the person's codes call for, as
	/// [`Library::recovery_file_anew`] says: done as a code is named or revoked, and after
	/// each sync. An AI agent's replica keeps none, as it names no code.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read, or the file written.
	pub(crate) fn save_recovery_file(&self) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		let path = self.directory.join(RECOVERY_FILE);
		// One that does not read is written anew.
		let existing = RecoveryFile::read(&path).ok().flatten();
		match self.recovery_file_anew(existing.as_ref())? {
			RecoveryFileNeed::Kept => Ok(()),
			RecoveryFileNeed::Written(file) => crate::info::write_file(&path, &file.to_bytes()),
			RecoveryFileNeed::Removed => match std::fs::remove_file(&path) {
				Ok(()) => Ok(()),
				Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
				Err(error) => Err(NodeError::file(&path)(error)),
			},
		}
	}

	/// Revokes the person's codes that a newer one replaces, as their records say: once every
	/// backup carries the newest code that stands, or at once with `at_once`, as the person
	/// says. Done after each sync and each update of a backup, and as a new code stands.
	/// Returns those it revoked; none on an AI agent's replica, which revokes no code.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read, or a revocation saved.
	pub fn revoke_replaced_recovery_codes(
		&mut self,
		at_once: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<RecoveryCodeInfo>, NodeError> {
		if self.is_ai_agent()? {
			return Ok(Vec::new());
		}
		// Nothing to replace with one code or none, as is usual: the backups are not read.
		let standing = self
			.roster(self.info.personal)?
			.codes()
			.filter(|code| code.stands())
			.count();
		if standing < 2 {
			return Ok(Vec::new());
		}
		let codes = self.recovery_codes()?;
		// The code that replaces the others, as each names it.
		let Some(newest) = codes.iter().find_map(|code| code.replaced_by.as_ref()) else {
			return Ok(Vec::new());
		};
		let carried = codes
			.iter()
			.find(|code| !code.revoked && code.replaced_by.is_none() && code.name == *newest)
			.is_some_and(|newest| newest.to_update.is_empty());
		if !at_once && !carried {
			return Ok(Vec::new());
		}
		let mut revoked = Vec::new();
		for code in codes.iter().filter(|code| code.replaced_by.is_some()) {
			revoked.push(self.revoke_recovery_code(&code.record.to_string(), wall_millis, rng)?);
		}
		Ok(revoked)
	}

	/// The person's codes that stand and that one of `replicas` made, as the key that signed
	/// each one's statement says: those that revoking a device names.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn recovery_codes_made_by(
		&self,
		replicas: &[EntityId],
	) -> Result<Vec<RecoveryCodeInfo>, NodeError> {
		Ok(self
			.recovery_codes()?
			.into_iter()
			.filter(|code| {
				!code.revoked
					&& code
						.made_by
						.is_some_and(|replica| replicas.contains(&replica))
			})
			.collect())
	}

	/// The recovery code that `reference` names, by its short name, in either case, or by
	/// its record's ID, of those that stand.
	///
	/// # Errors
	///
	/// Returns [`NodeError::WhichRecoveryCode`] if it names none that stands, or several.
	pub fn find_recovery_code(&self, reference: &str) -> Result<RecoveryCodeInfo, NodeError> {
		let reference = reference.trim();
		let named: Vec<RecoveryCodeInfo> = self
			.recovery_codes()?
			.into_iter()
			.filter(|code| {
				code.name.eq_ignore_ascii_case(reference) || code.record.to_string() == reference
			})
			.collect();
		let mut standing = named.iter().filter(|code| !code.revoked);
		match (standing.next(), standing.next()) {
			(Some(code), None) => Ok(code.clone()),
			(Some(_), Some(_)) => Err(NodeError::WhichRecoveryCode(format!(
				"several recovery codes are named {reference}; name one by its ID, which nib recovery list shows"
			))),
			(None, _) if named.is_empty() => Err(NodeError::WhichRecoveryCode(format!(
				"no recovery code is named {reference}; nib recovery list shows them"
			))),
			(None, _) => Err(NodeError::WhichRecoveryCode(format!(
				"the recovery code {reference} is revoked already"
			))),
		}
	}

	/// Names a recovery code in the personal space, once the person wrote it down and typed
	/// it back, and gives it the keys of the space that were given out, as a replica that
	/// lacks them is given them. With `secrets`, as the person chose, it also gives the code
	/// the secrets, which must be unlocked: the code then opens them too. Naming a code that
	/// is named already changes nothing, but gives it the secrets if asked, on the device that
	/// made it alone.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] on an AI agent's replica,
	/// [`NodeError::WhichRecoveryCode`] if the code was revoked, as a revocation is final,
	/// [`NodeError::SecretsLocked`] if `secrets` is asked while they are locked, before
	/// anything is named, [`NodeError::RecoveryCodeMadeElsewhere`] if they are asked for a
	/// code that another device made, and an error if the changes cannot be saved.
	pub fn add_recovery_code(
		&mut self,
		code: &RecoveryCode,
		secrets: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<RecoveryCodeInfo, NodeError> {
		if self.is_ai_agent()? {
			return Err(NodeError::AiAgentMayNot("make a recovery code"));
		}
		if secrets && !self.secrets_unlocked(self.info.personal) {
			return Err(NodeError::SecretsLocked);
		}
		let personal = self.info.personal;
		let stated = RecoveryStatement::sign(code, self.info.user);
		let record = stated.record(personal);
		match self.roster(personal)?.code(record) {
			Some(named) if !named.stands() => {
				return Err(NodeError::WhichRecoveryCode(
					"this recovery code was revoked, which is final; make a new one".to_owned(),
				));
			}
			// What another device gave it, a device that recovers with it could not tell from
			// a key of someone's own making.
			Some(named) if secrets && named.author != self.replica.signing.verifying_key() => {
				return Err(NodeError::RecoveryCodeMadeElsewhere);
			}
			Some(_) => {}
			None => {
				let name = RecoveryCode::name_of(&stated.key);
				// The code signs, once, what the personal space's entry for it names, which
				// each recovery file then carries: no device can write another for it.
				let Some((_, hash, root)) = self.what_codes_sign()? else {
					return Err(NodeError::Damaged(
						"the personal space has no first key or first change".to_owned(),
					));
				};
				let entry = RecoveryEntry::sign(code, (personal, self.info.user, root), hash);
				let operations = vec![
					state_recovery_code(personal, &stated, rng),
					state_recovery_entry(&entry, rng),
					assert(
						record,
						fields::TYPE,
						Value::Text(RECOVERY_CODE.to_string()),
						rng,
					),
					assert(record, fields::NAME, Value::Name(Name::new(&name)), rng),
				];
				self.commit(personal, operations, wall_millis, rng)?;
			}
		}
		// The keys given out so far: a code is given them as a replica that lacks them is.
		self.give_keys(personal, wall_millis, rng)?;
		// And the person's other spaces: each is given to the code, and names it. The code
		// stands in the personal space already; a space where this fails gets it after the
		// next sync.
		let _ = self.give_space_entries(wall_millis, rng);
		let _ = self.carry_codes(wall_millis, rng);
		// And the recovery file, which the next sync writes if this fails.
		let _ = self.save_recovery_file();
		if secrets {
			self.give_code_secrets(record, wall_millis, rng)?;
			// And the secrets of the person's other spaces, which their holders give to those
			// known to hold the personal space's.
			let _ = self.settle_secrets(wall_millis, rng);
		}
		self.recovery_codes()?
			.into_iter()
			.find(|named| named.record == record)
			.ok_or_else(|| NodeError::NoEntity(record.to_string()))
	}

	/// Checks a recovery code on this device, which works: whether the personal space names
	/// it, whether it stands, and, in each space that names it, which of the keys that this
	/// replica holds it opens.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn check_recovery_code(&self, code: &RecoveryCode) -> Result<RecoveryCheck, NodeError> {
		let key = code.signing_key().verifying_key();
		let personal = self.info.personal;
		// The record derived for the user first, as this version names a code.
		let own = recovery_record(personal, self.info.user, &key);
		let mut named: Vec<RecoveryCodeInfo> = self
			.recovery_codes()?
			.into_iter()
			.filter(|named| named.key == key)
			.collect();
		named.sort_by_key(|named| (named.record != own, named.revoked));
		let named = named.into_iter().next();
		let decryption = code.decryption_key();
		// The secrets keys that a recovery with the code takes, where the secrets are unlocked.
		let secrets_opened = if self
			.spaces()
			.into_iter()
			.any(|space| self.secrets_unlocked(space))
		{
			self.secrets_code_opens(&self.rosters()?, code)?
		} else {
			SpaceKeyMap::new()
		};
		let mut spaces = Vec::new();
		for space in self.spaces() {
			let roster = self.roster(space)?;
			let mut records: Vec<_> = roster
				.codes()
				.filter(|named| named.statement.key == key)
				.map(|named| named.record)
				.collect();
			if records.is_empty() {
				continue;
			}
			// Every record of this replica's member for the code in each space; or else one
			// member's, the user's own first.
			let own_members = if space == personal {
				Vec::new()
			} else {
				self.own_code_members(space)?
			};
			let own_records: Vec<EntityId> = records
				.iter()
				.copied()
				.filter(|record| {
					own_members
						.iter()
						.any(|member| recovery_record(space, *member, &key) == *record)
				})
				.collect();
			records.sort_by_key(|record| *record != own);
			records.truncate(1);
			let records = if own_records.is_empty() {
				records
			} else {
				own_records
			};
			let mut keys = RecoveryKeys {
				space,
				opens: Vec::new(),
				lacks: Vec::new(),
				entry: None,
				secrets: None,
			};
			for &record in &records {
				let found = self.code_keys(space, &roster, record, &decryption)?;
				keys.opens.extend(found.opens);
				keys.lacks.extend(found.lacks);
			}
			keys.opens = merged(keys.opens, &[]);
			keys.lacks = merged(keys.lacks, &keys.opens);
			if self.secrets_unlocked(space) {
				keys.secrets = Some(self.code_secrets(space, &roster, &secrets_opened)?);
			}
			if space != personal {
				keys.entry = self.code_entry(space, named.as_ref(), &decryption)?;
			}
			spaces.push(keys);
		}
		Ok(RecoveryCheck {
			code: named,
			spaces,
		})
	}

	/// The keys of `space` that this replica holds and that were given out, by whether the
	/// statements on a code's record there seal them to the code, so that they open with its
	/// decryption key to the keys held.
	fn code_keys(
		&self,
		space: SpaceId,
		roster: &Roster,
		record: EntityId,
		decryption: &DecryptionKey,
	) -> Result<RecoveryKeys, NodeError> {
		let open = self.space(space)?;
		let mut makers = Makers::of(open);
		let mut keys = RecoveryKeys {
			space,
			opens: Vec::new(),
			lacks: Vec::new(),
			entry: None,
			secrets: None,
		};
		for (epoch, held) in open.keys.space_keys() {
			let hash = key_hash(held);
			// Only a key whose rotation a member's replica made is given out; the first key
			// never is.
			let Some(rotation) = roster.rotation(epoch, &hash).filter(|_| epoch != 0) else {
				continue;
			};
			if !makers.made(rotation.change)? {
				continue;
			}
			let opens = roster.given_to(record).iter().any(|gift| {
				gift.sealed.epoch == epoch
					&& gift.sealed.hash == hash
					&& gift
						.sealed
						.open(decryption, space, record)
						.is_some_and(|opened| opened.same_as(held))
			});
			if opens {
				keys.opens.push(epoch);
			} else {
				keys.lacks.push(epoch);
			}
		}
		Ok(keys)
	}

	/// The secrets keys of `space` that this replica holds, unlocked, and that were given out,
	/// by whether a recovery with the code takes them, as `opened` has them: those it opens,
	/// and those it lacks.
	fn code_secrets(
		&self,
		space: SpaceId,
		roster: &Roster,
		opened: &SpaceKeyMap,
	) -> Result<(Vec<u64>, Vec<u64>), NodeError> {
		let open = self.space(space)?;
		let mut makers = Makers::of(open);
		let (mut opens, mut lacks) = (Vec::new(), Vec::new());
		for (epoch, held) in open.keys.secrets_keys() {
			let hash = key_hash(held);
			let Some(rotation) = roster.secrets_rotation(epoch, &hash) else {
				continue;
			};
			if !makers.made(rotation.change)? {
				continue;
			}
			let taken = opened
				.get(&space)
				.and_then(|keys| keys.get(&epoch))
				.is_some_and(|key| key.same_as(held));
			if taken {
				opens.push(epoch);
			} else {
				lacks.push(epoch);
			}
		}
		Ok((opens, lacks))
	}

	/// Whether a code of the person's is given the space `space`: whether an entry on the
	/// code's record, in the personal space or in a group that this replica holds, names the
	/// space, and opens with the code to the first key that this replica holds. `None` where
	/// the personal space does not name the code.
	fn code_entry(
		&self,
		space: SpaceId,
		named: Option<&RecoveryCodeInfo>,
		decryption: &DecryptionKey,
	) -> Result<Option<bool>, NodeError> {
		let Some(named) = named else {
			return Ok(None);
		};
		let Some(first) = self
			.space(space)?
			.keys
			.space_keys()
			.find(|(epoch, _)| *epoch == 0)
			.map(|(_, key)| key.clone())
		else {
			return Ok(Some(false));
		};
		// Whether a counting statement of a log, on one of `records`, gives the space.
		let gives = |open: &super::Space, records: &BTreeSet<EntityId>| {
			let statements = open.stored.log().governing_statements(&open.keys)?;
			Ok::<_, NodeError>(statements.iter().any(|governing| {
				let record = governing.statement.entity;
				records.contains(&record)
					&& governing
						.counting_text(&SPACE_ENTRY)
						.and_then(SpaceEntry::from_text)
						.is_some_and(|entry| {
							entry.space == space
								&& entry
									.open(decryption, record)
									.is_some_and(|opened| opened.same_as(&first))
						})
			}))
		};
		// The personal space's entry, on the code's record there.
		if gives(
			self.space(self.info.personal)?,
			&BTreeSet::from([named.record]),
		)? {
			return Ok(Some(true));
		}
		// Or a group's, on the person's own record for the code there, for a space that the
		// group is in.
		for (&group, open) in &self.spaces {
			if group == space || group == self.info.personal || !open.is_group()? {
				continue;
			}
			let records: BTreeSet<EntityId> = self
				.own_code_members(group)?
				.into_iter()
				.map(|member| recovery_record(group, member, &named.key))
				.collect();
			if !records.is_empty() && gives(open, &records)? {
				return Ok(Some(true));
			}
		}
		Ok(Some(false))
	}

	/// Revokes the recovery code that `reference` names, as [`Library::find_recovery_code`]
	/// finds it, and gives the personal space a new key in the same change, which the code is
	/// not given: so it opens nothing added later, and brings no device back.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] on an AI agent's replica,
	/// [`NodeError::WhichRecoveryCode`] if `reference` names no code that stands, or several,
	/// and an error if the change cannot be saved.
	pub fn revoke_recovery_code(
		&mut self,
		reference: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<RecoveryCodeInfo, NodeError> {
		if self.is_ai_agent()? {
			return Err(NodeError::AiAgentMayNot("revoke a recovery code"));
		}
		let code = self.find_recovery_code(reference)?;
		let personal = self.info.personal;
		self.check_not_revoked(personal)?;
		// The new key is sealed to this replica too, which needs its encryption key on its
		// record: a library's first replica states it only now, or as it first syncs.
		self.record_own_endpoint(wall_millis, rng)?;
		let roster = self.roster(personal)?;
		self.rotate_revoking(
			personal,
			&[],
			&[(code.record, code.key)],
			&roster,
			wall_millis,
			rng,
		)?;
		// And in every other space that names it, each with a new key. The personal space's
		// revocation is made: a space where this fails revokes after the next sync.
		let _ = self.carry_codes(wall_millis, rng);
		// The recovery file without the code, which the next sync writes if this fails.
		let _ = self.save_recovery_file();
		// A new secrets key, if the secrets are unlocked here and the code held them; or else
		// the first replica that unlocks them makes one.
		let _ = self.settle_secrets(wall_millis, rng);
		Ok(RecoveryCodeInfo {
			revoked: true,
			..code
		})
	}
}

/// The key numbers of `numbers`, each once and in order, but those of `but`.
fn merged(numbers: Vec<u64>, but: &[u64]) -> Vec<u64> {
	let numbers: BTreeSet<u64> = numbers.into_iter().collect();
	numbers
		.into_iter()
		.filter(|number| !but.contains(number))
		.collect()
}
