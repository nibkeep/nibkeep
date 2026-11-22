//! Who the replicas of a space are, as the governing changes that count say: their keys,
//! the encryption keys that keys are sealed to, the space keys sealed to them, and the
//! secrets keys sealed to those that hold the secrets
//! ([Identity](../../../docs/spec/identity.md#4-revoking-replicas)). The state, which
//! any replica's ordinary changes write to as well, does not decide any of it.

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId};
use nibkeep_crypto::{EncryptionKey, VerifyingKey};
use nibkeep_time::Timestamp;

use crate::{
	ENCRYPTION_KEY, GoverningStatement, RECOVERY_ENTRY, RECOVERY_KEY, RECOVERY_REVOKED,
	RecoveryEntry, RecoveryStatement, SECRETS_ENCRYPTION_KEY, SECRETS_FROM, SECRETS_KEY,
	SIGNING_KEY, SPACE_KEY, SealedSecretsKey, SealedSpaceKey,
};

/// Where a key was first given out: the earliest statement of its number and hash that the
/// replica can read, whether or not its change counts. Only a key whose rotation a member's
/// replica made, by the changes it builds on, is given out, and only one whose rotation
/// builds on every departure is written with
/// ([technical design](../../../docs/Technical-design.md#13-sharing)).
#[derive(Clone, Copy, Debug)]
pub struct Rotation {
	/// When the statement's change was made.
	pub at: Timestamp,
	/// The statement's change.
	pub change: ChangeId,
	/// Whether that change counts.
	pub counts: bool,
}

/// A space key that a statement seals to a replica, with the change that states it, which
/// may count or not: a gift counts if a member's replica made it, by what it builds on.
#[derive(Clone, Debug)]
pub struct SpaceGift {
	/// The space key, sealed to the replica.
	pub sealed: SealedSpaceKey,
	/// The change that states it.
	pub change: ChangeId,
	/// Whether that change counts.
	pub counts: bool,
}

/// A secrets key that a statement seals to a replica, with the key that signed it and the
/// change that states it, which may count or not: a replica takes one from any replica, if
/// its link was made with a secrets key that the replica holds, and a member's replica made
/// it, by what it builds on.
#[derive(Clone, Debug)]
pub struct SecretsGift {
	/// The key that signed the change.
	pub author: VerifyingKey,
	/// The secrets key, sealed to the replica.
	pub sealed: SealedSecretsKey,
	/// The change that states it.
	pub change: ChangeId,
	/// Whether that change counts.
	pub counts: bool,
}

/// A recovery code of a member's, as a counting change states it on the code's record in the
/// space ([technical design](../../../docs/Technical-design.md#111-recovery-codes)). It is
/// no replica: no key of it is listed, and it signs no change.
#[derive(Clone, Debug)]
pub struct Code {
	/// The code's record, derived from the space, its member and its recovery key.
	pub record: EntityId,
	/// The member, the recovery key, and the encryption keys that the recovery key signed.
	pub statement: RecoveryStatement,
	/// The key that signed the change that states it: the replica that made the code, in the
	/// space where it was made, or carried it there.
	pub author: VerifyingKey,
	/// The change that states it.
	pub change: ChangeId,
	/// When that change was made.
	pub at: Timestamp,
	/// The counting changes that revoke it: those that name its recovery key on its record.
	pub revoked_by: Vec<ChangeId>,
	/// The space's entries for it that it signed, as counting statements state them on its
	/// record: one, from the replica that named it, in a personal space; none elsewhere.
	pub entries: Vec<RecoveryEntry>,
}

impl Code {
	/// Whether no counting change revokes the code.
	#[must_use]
	pub fn stands(&self) -> bool {
		self.revoked_by.is_empty()
	}
}

/// Who the replicas of a space are, as its counting governing changes say.
#[derive(Clone, Debug, Default)]
pub struct Roster {
	/// Each replica's keys: those a counting change listed for it first, before any other
	/// replica.
	keys: BTreeMap<EntityId, Vec<VerifyingKey>>,
	/// The replica each listed key is of.
	replica_of: BTreeMap<[u8; 32], EntityId>,
	/// Each replica's key of its own: the one key that counting changes list on its record,
	/// if they list it on no other record. Every replica of the user may list keys on any
	/// record, of its own making too, and date the change early; a record with two keys, or
	/// a key that two records claim, has none of its own.
	own_key: BTreeMap<EntityId, VerifyingKey>,
	/// Each replica's encryption key: the earliest that a counting change states for it,
	/// either signed by one of its keys or listing one of them.
	encryption: BTreeMap<EntityId, EncryptionKey>,
	/// The space keys that statements seal to each replica, whether their changes count or
	/// not.
	given: BTreeMap<EntityId, Vec<SpaceGift>>,
	/// Each replica's secrets encryption key, and the replica it takes the secrets from: the
	/// latest that counting changes signed by its key of its own state, so that a replica
	/// that lost its secrets can ask for them again, and no other can say it for it.
	secrets_encryption: BTreeMap<EntityId, EncryptionKey>,
	secrets_from: BTreeMap<EntityId, EntityId>,
	/// The secrets keys that statements seal to each replica, whether their changes count or
	/// not.
	secrets_given: BTreeMap<EntityId, Vec<SecretsGift>>,
	/// Where each key but key 0 was first given out, by its number and hash: a statement of
	/// a number with another hash is of another key, which has a rotation of its own.
	rotations: BTreeMap<(u64, Hash), Rotation>,
	/// Where each secrets key was first given out, by its number and hash, as for space keys.
	secrets_rotations: BTreeMap<(u64, Hash), Rotation>,
	/// The members' recovery codes, by their records: the earliest counting statement of
	/// each, on the record derived from the space, the member and the recovery key, with
	/// encryption keys that the recovery key signed.
	codes: BTreeMap<EntityId, Code>,
}

impl Roster {
	/// The roster that the governing statements of `space`, in the order of their changes,
	/// make.
	#[must_use]
	pub fn new(space: SpaceId, statements: &[GoverningStatement]) -> Self {
		let mut roster = Self::default();
		let listings = roster.list_keys(statements);
		roster.name_codes(space, statements);
		for governing in statements {
			let statement = &governing.statement;
			// Stated by the replica itself, or by the change that listed it.
			let by_itself_or_listing = || {
				roster
					.keys
					.get(&statement.entity)
					.is_some_and(|keys| keys.contains(&governing.author))
					|| listings
						.get(&statement.entity)
						.is_some_and(|changes| changes.contains(&governing.change))
			};
			// Whom a replica takes the secrets from, and with which key, only its key of its
			// own says: not the change that listed it, nor a key that another listed on its
			// record, which any replica of the user can.
			let by_own_key = || roster.own_key.get(&statement.entity) == Some(&governing.author);
			if statement.field == SECRETS_FROM && governing.counts {
				if by_own_key()
					&& let Some(from) = statement.value.as_reference()
				{
					roster.secrets_from.insert(statement.entity, from);
				}
				continue;
			}
			let Some(text) = statement.value.as_text() else {
				continue;
			};
			if statement.field == ENCRYPTION_KEY && governing.counts {
				if by_itself_or_listing()
					&& !roster.encryption.contains_key(&statement.entity)
					&& let Some(key) = crate::encryption_key_of_text(text)
				{
					roster.encryption.insert(statement.entity, key);
				}
			} else if statement.field == SECRETS_ENCRYPTION_KEY && governing.counts {
				if by_own_key()
					&& let Some(key) = crate::encryption_key_of_text(text)
				{
					roster.secrets_encryption.insert(statement.entity, key);
				}
			} else if statement.field == SECRETS_KEY {
				if let Some(sealed) = SealedSecretsKey::from_text(text) {
					roster
						.secrets_rotations
						.entry((sealed.epoch, sealed.hash))
						.or_insert(Rotation {
							at: governing.timestamp,
							change: governing.change,
							counts: governing.counts,
						});
					roster
						.secrets_given
						.entry(statement.entity)
						.or_default()
						.push(SecretsGift {
							author: governing.author,
							sealed,
							change: governing.change,
							counts: governing.counts,
						});
				}
			} else if statement.field == SPACE_KEY
				&& let Some(sealed) = SealedSpaceKey::from_text(text)
			{
				if sealed.epoch != 0 && !roster.rotations.contains_key(&(sealed.epoch, sealed.hash))
				{
					roster.rotations.insert(
						(sealed.epoch, sealed.hash),
						Rotation {
							at: governing.timestamp,
							change: governing.change,
							counts: governing.counts,
						},
					);
				}
				roster
					.given
					.entry(statement.entity)
					.or_default()
					.push(SpaceGift {
						sealed,
						change: governing.change,
						counts: governing.counts,
					});
			}
		}
		roster
	}

	/// Takes in the keys that the counting statements list: whose each key is, and each
	/// replica's key of its own. Returns the changes that list a key for each replica.
	fn list_keys(
		&mut self,
		statements: &[GoverningStatement],
	) -> BTreeMap<EntityId, BTreeSet<ChangeId>> {
		let mut listings: BTreeMap<EntityId, BTreeSet<ChangeId>> = BTreeMap::new();
		// The keys listed on each record, and the records each key is listed on.
		let mut listed_on: BTreeMap<EntityId, BTreeMap<[u8; 32], VerifyingKey>> = BTreeMap::new();
		let mut records_of: BTreeMap<[u8; 32], BTreeSet<EntityId>> = BTreeMap::new();
		for governing in statements.iter().filter(|governing| governing.counts) {
			let statement = &governing.statement;
			if statement.field == SIGNING_KEY
				&& governing.lists
				&& let Some(key) = statement.value.as_text().and_then(VerifyingKey::from_text)
			{
				listings
					.entry(statement.entity)
					.or_default()
					.insert(governing.change);
				listed_on
					.entry(statement.entity)
					.or_default()
					.insert(*key.as_bytes(), key);
				records_of
					.entry(*key.as_bytes())
					.or_default()
					.insert(statement.entity);
				if !self.replica_of.contains_key(key.as_bytes()) {
					self.replica_of.insert(*key.as_bytes(), statement.entity);
					self.keys.entry(statement.entity).or_default().push(key);
				}
			}
		}
		for (record, keys) in listed_on {
			if let [(bytes, key)] = Vec::from_iter(keys).as_slice()
				&& records_of
					.get(bytes)
					.is_some_and(|records| records.len() == 1)
			{
				self.own_key.insert(record, *key);
			}
		}
		listings
	}

	/// Takes in the recovery codes that the counting statements name, and their revocations.
	/// Whether the code is its member's own to state, the rules of a space with members
	/// decide before a change counts; in a personal space every replica is the user's.
	fn name_codes(&mut self, space: SpaceId, statements: &[GoverningStatement]) {
		let counting = || statements.iter().filter(|governing| governing.counts);
		for governing in counting() {
			let statement = &governing.statement;
			if statement.field != RECOVERY_KEY || self.codes.contains_key(&statement.entity) {
				continue;
			}
			let Some(stated) = statement
				.value
				.as_text()
				.and_then(RecoveryStatement::from_text)
			else {
				continue;
			};
			if stated.record(space) != statement.entity || !stated.verifies() {
				continue;
			}
			self.codes.insert(
				statement.entity,
				Code {
					record: statement.entity,
					statement: stated,
					author: governing.author,
					change: governing.change,
					at: governing.timestamp,
					revoked_by: Vec::new(),
					entries: Vec::new(),
				},
			);
		}
		// A revocation names the key it revokes, on the code's own record: one member's
		// revocation says nothing of another member's record for the same code.
		for governing in counting() {
			let statement = &governing.statement;
			if statement.field == RECOVERY_REVOKED
				&& let Some(code) = self.codes.get_mut(&statement.entity)
				&& statement.value.as_text().and_then(VerifyingKey::from_text)
					== Some(code.statement.key)
				&& !code.revoked_by.contains(&governing.change)
			{
				code.revoked_by.push(governing.change);
			}
		}
		// The entries that the code signed, on its own record, for this space: whoever states
		// one, it says only what the code signed.
		for governing in counting() {
			let statement = &governing.statement;
			if statement.field == RECOVERY_ENTRY
				&& let Some(code) = self.codes.get_mut(&statement.entity)
				&& let Some(entry) = statement.value.as_text().and_then(RecoveryEntry::from_text)
				&& entry.space == space
				&& entry.key == code.statement.key
				&& entry.record() == statement.entity
				&& entry.verifies()
				&& !code.entries.contains(&entry)
			{
				code.entries.push(entry);
			}
		}
	}

	/// The members' recovery codes that counting changes state, revoked ones too.
	pub fn codes(&self) -> impl Iterator<Item = &Code> + '_ {
		self.codes.values()
	}

	/// The recovery code whose record this is.
	#[must_use]
	pub fn code(&self, record: EntityId) -> Option<&Code> {
		self.codes.get(&record)
	}

	/// Adds a replica that a group in the space lists, on its record in the space, derived
	/// from the space and its key, with the encryption key that its record in the group
	/// states, unless a counting change of the space lists the key already.
	pub fn add_listed(&mut self, record: EntityId, key: VerifyingKey, encryption: EncryptionKey) {
		if self.replica_of.contains_key(key.as_bytes()) {
			return;
		}
		self.replica_of.insert(*key.as_bytes(), record);
		self.keys.entry(record).or_default().push(key);
		self.own_key.entry(record).or_insert(key);
		self.encryption.entry(record).or_insert(encryption);
	}

	/// The replicas that counting changes list a key for.
	pub fn replicas(&self) -> impl Iterator<Item = EntityId> + '_ {
		self.keys.keys().copied()
	}

	/// A replica's keys.
	pub fn keys_of(&self, replica: EntityId) -> &[VerifyingKey] {
		self.keys.get(&replica).map_or(&[], Vec::as_slice)
	}

	/// The replica a key is of.
	#[must_use]
	pub fn replica_of(&self, key: &VerifyingKey) -> Option<EntityId> {
		self.replica_of.get(key.as_bytes()).copied()
	}

	/// The replica whose key of its own a key is: the only key on its record, listed on no
	/// other.
	#[must_use]
	pub fn owner_of(&self, key: &VerifyingKey) -> Option<EntityId> {
		self.replica_of(key)
			.filter(|replica| self.own_key.get(replica) == Some(key))
	}

	/// A replica's encryption key.
	#[must_use]
	pub fn encryption_of(&self, replica: EntityId) -> Option<&EncryptionKey> {
		self.encryption.get(&replica)
	}

	/// The space keys that statements seal to a replica, whether their changes count or not.
	pub fn given_to(&self, replica: EntityId) -> &[SpaceGift] {
		self.given.get(&replica).map_or(&[], Vec::as_slice)
	}

	/// A replica's secrets encryption key: it asks for the secrets, or holds them.
	#[must_use]
	pub fn secrets_encryption_of(&self, replica: EntityId) -> Option<&EncryptionKey> {
		self.secrets_encryption.get(&replica)
	}

	/// The replica that a replica takes the secrets from: the one the person chose on it, or
	/// itself, if it set them up.
	#[must_use]
	pub fn secrets_from(&self, replica: EntityId) -> Option<EntityId> {
		self.secrets_from.get(&replica).copied()
	}

	/// The secrets keys given to a replica: those that the replica it takes the secrets from
	/// sealed to its secrets encryption key now, signed by its key of its own. No other
	/// replica can give it a key, nor one that lists a key on the giver's record, so that one
	/// which never held the secrets cannot make up a key that others seal with.
	pub fn secrets_given_to(
		&self,
		replica: EntityId,
	) -> impl Iterator<Item = &SealedSecretsKey> + '_ {
		let giver = self
			.secrets_from(replica)
			.and_then(|from| self.own_key.get(&from));
		let recipient = self
			.secrets_encryption_of(replica)
			.map(crate::secrets_recipient);
		self.secrets_given
			.get(&replica)
			.into_iter()
			.flatten()
			.filter(move |gift| {
				gift.counts
					&& giver == Some(&gift.author)
					&& Some(gift.sealed.recipient) == recipient
			})
			.map(|gift| &gift.sealed)
	}

	/// Whether any counting change gives a replica a secrets key, by whichever replica, and
	/// sealed to whichever key.
	#[must_use]
	pub fn given_any_secrets(&self, replica: EntityId) -> bool {
		self.secrets_given
			.get(&replica)
			.is_some_and(|given| given.iter().any(|gift| gift.counts))
	}

	/// Whether any statement gives a replica a secrets key, whether its change counts or not,
	/// by whichever replica, and sealed to whichever key: a revoked replica that was given
	/// one may hold the secret values sealed with it.
	#[must_use]
	pub fn may_hold_secrets(&self, replica: EntityId) -> bool {
		self.secrets_given
			.get(&replica)
			.is_some_and(|given| !given.is_empty())
	}

	/// The secrets keys that statements seal to a replica, whether their changes count or
	/// not, by whichever replica, and sealed to whichever key.
	pub fn secrets_gifts(&self, replica: EntityId) -> &[SecretsGift] {
		self.secrets_given.get(&replica).map_or(&[], Vec::as_slice)
	}

	/// A replica's key of its own: the only key on its record, listed on no other.
	#[must_use]
	pub fn own_key_of(&self, replica: EntityId) -> Option<&VerifyingKey> {
		self.own_key.get(&replica)
	}

	/// The rotation of a secrets key's number that names this hash, if the replica can read
	/// one.
	#[must_use]
	pub fn secrets_rotation(&self, epoch: u64, hash: &Hash) -> Option<&Rotation> {
		self.secrets_rotations.get(&(epoch, *hash))
	}

	/// Every secrets key number that statements name.
	#[must_use]
	pub fn secrets_numbers(&self) -> BTreeSet<u64> {
		self.secrets_rotations
			.keys()
			.map(|(epoch, _)| *epoch)
			.collect()
	}

	/// The replicas that hold the secrets of the space: those that set them up, taking them
	/// from themselves, and those that the replica they take them from gave a key to, and
	/// which that replica reaches thus from one that set them up, so that two replicas that
	/// say they take them from each other hold nothing
	/// ([technical design](../../../docs/Technical-design.md#13-sharing)).
	#[must_use]
	pub fn secrets_holders(&self) -> BTreeSet<EntityId> {
		let given: BTreeSet<EntityId> = self
			.secrets_given
			.keys()
			.copied()
			.filter(|&replica| self.secrets_given_to(replica).next().is_some())
			.collect();
		let mut holders: BTreeSet<EntityId> = given
			.iter()
			.copied()
			.filter(|&replica| self.secrets_from(replica) == Some(replica))
			.collect();
		loop {
			let reached: Vec<EntityId> = given
				.iter()
				.copied()
				.filter(|replica| {
					!holders.contains(replica)
						&& self
							.secrets_from(*replica)
							.is_some_and(|from| holders.contains(&from))
				})
				.collect();
			if reached.is_empty() {
				return holders;
			}
			holders.extend(reached);
		}
	}

	/// The rotation of a key's number that names this hash, if the replica can read one.
	#[must_use]
	pub fn rotation(&self, epoch: u64, hash: &Hash) -> Option<&Rotation> {
		self.rotations.get(&(epoch, *hash))
	}

	/// Every key number the space knows of.
	#[must_use]
	pub fn numbers(&self) -> BTreeSet<u64> {
		let mut numbers: BTreeSet<u64> = self.rotations.keys().map(|(epoch, _)| *epoch).collect();
		numbers.extend(self.given.values().flatten().map(|gift| gift.sealed.epoch));
		numbers
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_base::StatementId;
	use nibkeep_crypto::{DecryptionKey, Key, SigningKey};
	use nibkeep_model::{FieldName, Statement, Value};
	use nibkeep_testkit::TestRng;
	use nibkeep_time::Timestamp;

	use super::*;

	/// The statements of governing changes, each in a change of its own, in their order.
	struct Statements {
		rng: TestRng,
		space: SpaceId,
		made: Vec<GoverningStatement>,
	}

	impl Statements {
		fn new(seed: &str) -> Self {
			let mut rng = TestRng::new(seed);
			Self {
				space: SpaceId::random(&mut rng),
				rng,
				made: Vec::new(),
			}
		}

		/// A statement in a new change by `author`, which counts or not.
		fn state(
			&mut self,
			author: &VerifyingKey,
			counts: bool,
			(entity, field, value): (EntityId, FieldName, Value),
		) -> ChangeId {
			let change = ChangeId::from_hash(Hash::of(&self.rng.bytes::<16>()));
			self.add(change, author, counts, (entity, field, value));
			change
		}

		/// A statement in the change `change`.
		fn add(
			&mut self,
			change: ChangeId,
			author: &VerifyingKey,
			counts: bool,
			(entity, field, value): (EntityId, FieldName, Value),
		) {
			let at = Timestamp::new(1_800_000_000_000 + self.made.len() as u64, 0);
			// The personal space's rules: every statement of a key lists it.
			let lists = field == SIGNING_KEY;
			self.made.push(GoverningStatement {
				change,
				author: *author,
				timestamp: at,
				counts,
				statement: Statement::new(StatementId::random(&mut self.rng), entity, field, value),
				lists,
			});
		}

		fn roster(&self) -> Roster {
			Roster::new(self.space, &self.made)
		}
	}

	fn key(rng: &mut TestRng) -> VerifyingKey {
		SigningKey::generate(rng).verifying_key()
	}

	#[test]
	fn a_key_is_of_the_replica_first_listed_with_it() {
		let mut statements = Statements::new("first listed");
		let (laptop, phone) = (key(&mut statements.rng), key(&mut statements.rng));
		let (laptop_record, phone_record) = (
			EntityId::random(&mut statements.rng),
			EntityId::random(&mut statements.rng),
		);
		for (record, listed) in [(laptop_record, laptop), (phone_record, phone)] {
			statements.state(
				&laptop,
				true,
				(record, SIGNING_KEY, Value::Text(listed.to_text())),
			);
		}
		// The phone claims the laptop's key for its own record.
		statements.state(
			&phone,
			true,
			(phone_record, SIGNING_KEY, Value::Text(laptop.to_text())),
		);
		let roster = statements.roster();
		assert_eq!(roster.keys_of(phone_record), [phone]);
		assert_eq!(roster.keys_of(laptop_record), [laptop]);
		assert_eq!(roster.replica_of(&laptop), Some(laptop_record));
	}

	#[test]
	fn only_a_replica_or_its_listing_states_its_encryption_key() {
		let mut statements = Statements::new("encryption keys");
		let (laptop, phone) = (key(&mut statements.rng), key(&mut statements.rng));
		let phone_record = EntityId::random(&mut statements.rng);
		let own = DecryptionKey::generate(&mut statements.rng).encryption_key();
		let thief = DecryptionKey::generate(&mut statements.rng).encryption_key();
		let laptop_record = EntityId::random(&mut statements.rng);
		statements.state(
			&laptop,
			true,
			(laptop_record, SIGNING_KEY, Value::Text(laptop.to_text())),
		);
		// The listing of the phone states its encryption key; another change by the laptop,
		// which listed the phone, may not state another, even before it.
		statements.state(
			&laptop,
			true,
			(
				phone_record,
				ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&thief)),
			),
		);
		let listing = statements.state(
			&laptop,
			true,
			(phone_record, SIGNING_KEY, Value::Text(phone.to_text())),
		);
		statements.add(
			listing,
			&laptop,
			true,
			(
				phone_record,
				ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&own)),
			),
		);
		let roster = statements.roster();
		assert_eq!(roster.encryption_of(phone_record), Some(&own));
	}

	/// A secrets key that `author` gives a replica of a space, sealed to `to`, linked with the
	/// key itself.
	fn gift(
		statements: &mut Statements,
		author: &VerifyingKey,
		((space, replica), to): ((nibkeep_base::SpaceId, EntityId), &DecryptionKey),
		(key, hash): (&Key, Hash),
	) {
		let sealed = SealedSecretsKey::seal(
			&to.encryption_key(),
			space,
			replica,
			(0, key, hash),
			(space, 0, key),
			&mut statements.rng,
		);
		statements.state(
			author,
			true,
			(replica, SECRETS_KEY, Value::Text(sealed.to_text())),
		);
	}

	/// Three replicas, a laptop, a phone and another, listed by the laptop; the phone asks the
	/// laptop for the secrets of a space, with a key of its own.
	struct Asking {
		statements: Statements,
		laptop: (VerifyingKey, EntityId),
		phone: EntityId,
		other: (VerifyingKey, EntityId),
		space: nibkeep_base::SpaceId,
		asked: DecryptionKey,
	}

	fn phone_asks_the_laptop(seed: &str) -> Asking {
		let mut statements = Statements::new(seed);
		let rng = &mut statements.rng;
		let (laptop, phone, other) = (key(rng), key(rng), key(rng));
		let (laptop_record, phone_record, other_record) = (
			EntityId::random(rng),
			EntityId::random(rng),
			EntityId::random(rng),
		);
		let space = nibkeep_base::SpaceId::random(rng);
		let asked = DecryptionKey::generate(rng);
		for (record, listed) in [
			(laptop_record, laptop),
			(phone_record, phone),
			(other_record, other),
		] {
			statements.state(
				&laptop,
				true,
				(record, SIGNING_KEY, Value::Text(listed.to_text())),
			);
		}
		for (field, value) in [
			(
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&asked.encryption_key())),
			),
			(SECRETS_FROM, Value::Reference(laptop_record)),
		] {
			statements.state(&phone, true, (phone_record, field, value));
		}
		Asking {
			statements,
			laptop: (laptop, laptop_record),
			phone: phone_record,
			other: (other, other_record),
			space,
			asked,
		}
	}

	#[test]
	fn secrets_keys_count_only_from_the_replica_chosen_and_to_the_key_asked_with() {
		let Asking {
			mut statements,
			laptop: (laptop, laptop_record),
			phone,
			other: (other, _),
			space,
			asked,
		} = phone_asks_the_laptop("secrets from");
		let before = DecryptionKey::generate(&mut statements.rng);
		let secrets_key = Key::generate(&mut statements.rng);
		let made_up = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&secrets_key);
		// Another replica gives it a key of its own making, with its own hash, then with the
		// real key's; the laptop gives one sealed to a key that the phone had before; then
		// the real one.
		let to_phone = (space, phone);
		let made_up_hash = nibkeep_crypto::key_hash(&made_up);
		for (author, to, (key, hash)) in [
			(&other, &asked, (&made_up, made_up_hash)),
			(&other, &asked, (&made_up, hash)),
			(&laptop, &before, (&secrets_key, hash)),
		] {
			gift(&mut statements, author, (to_phone, to), (key, hash));
		}
		let roster = statements.roster();
		assert_eq!(roster.secrets_given_to(phone).count(), 0);
		assert!(roster.secrets_holders().is_empty());
		gift(
			&mut statements,
			&laptop,
			(to_phone, &asked),
			(&secrets_key, hash),
		);
		let roster = statements.roster();
		let given: Vec<_> = roster.secrets_given_to(phone).collect();
		assert_eq!(given.len(), 1);
		assert!(
			given[0]
				.open(&asked, space, phone)
				.is_some_and(|key| key.expose_bytes() == secrets_key.expose_bytes())
		);
		// It holds them once the laptop says it holds them itself, as it set them up.
		assert!(roster.secrets_holders().is_empty());
		set_up(
			&mut statements,
			(&laptop, laptop_record),
			space,
			(&secrets_key, hash),
		);
		let roster = statements.roster();
		assert_eq!(roster.secrets_holders().into_iter().collect::<Vec<_>>(), {
			let mut holders = vec![laptop_record, phone];
			holders.sort();
			holders
		});
	}

	/// The statements of a replica that sets up the secrets of `space`: its secrets encryption
	/// key, that it takes them from itself, and `key` given to itself.
	fn set_up(
		statements: &mut Statements,
		(author, record): (&VerifyingKey, EntityId),
		space: nibkeep_base::SpaceId,
		(key, hash): (&Key, Hash),
	) {
		let own = DecryptionKey::generate(&mut statements.rng);
		statements.state(
			author,
			true,
			(
				record,
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&own.encryption_key())),
			),
		);
		statements.state(
			author,
			true,
			(record, SECRETS_FROM, Value::Reference(record)),
		);
		gift(statements, author, ((space, record), &own), (key, hash));
	}

	#[test]
	fn two_replicas_that_say_they_take_the_secrets_from_each_other_hold_nothing() {
		let Asking {
			mut statements,
			laptop: (laptop, laptop_record),
			phone,
			other: (other, other_record),
			space,
			asked,
		} = phone_asks_the_laptop("secrets from each other");
		let made_up = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&made_up);
		// The other says it takes them from the phone, and each gives the other a key that
		// neither got from a replica that set them up.
		let others = DecryptionKey::generate(&mut statements.rng);
		statements.state(
			&other,
			true,
			(
				other_record,
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&others.encryption_key())),
			),
		);
		statements.state(
			&other,
			true,
			(other_record, SECRETS_FROM, Value::Reference(phone)),
		);
		let phone_key = statements
			.roster()
			.own_key_of(phone)
			.copied()
			.expect("its own");
		gift(
			&mut statements,
			&phone_key,
			((space, other_record), &others),
			(&made_up, hash),
		);
		gift(
			&mut statements,
			&laptop,
			((space, phone), &asked),
			(&made_up, hash),
		);
		// The laptop gave the phone a key, but never said it holds them: nobody holds them.
		let roster = statements.roster();
		assert_eq!(roster.secrets_given_to(other_record).count(), 1);
		assert!(roster.secrets_holders().is_empty());
		// Once it does, both are reached from it.
		set_up(
			&mut statements,
			(&laptop, laptop_record),
			space,
			(&made_up, hash),
		);
		assert_eq!(statements.roster().secrets_holders().len(), 3);
	}

	#[test]
	fn another_replica_cannot_say_whom_a_replica_asks_nor_with_which_key() {
		let Asking {
			mut statements,
			laptop: (laptop, laptop_record),
			phone,
			other: (other, other_record),
			space,
			asked,
		} = phone_asks_the_laptop("asked for another");
		let thief = DecryptionKey::generate(&mut statements.rng);
		for (author, field, value) in [
			(&other, SECRETS_FROM, Value::Reference(other_record)),
			(
				&other,
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&thief.encryption_key())),
			),
			// Nor the replica it asks.
			(
				&laptop,
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&thief.encryption_key())),
			),
		] {
			statements.state(author, true, (phone, field, value));
		}
		let made_up = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&made_up);
		gift(
			&mut statements,
			&other,
			((space, phone), &thief),
			(&made_up, hash),
		);
		let roster = statements.roster();
		assert_eq!(roster.secrets_from(phone), Some(laptop_record));
		assert_eq!(
			roster.secrets_encryption_of(phone),
			Some(&asked.encryption_key())
		);
		assert_eq!(roster.secrets_given_to(phone).count(), 0);
	}

	/// What another replica says of the phone's asking, with a key it lists on the phone's
	/// record: one of its own making, or its own, listed either in the change that says it or
	/// before, and signing with the key it made.
	#[derive(Clone, Copy, Debug)]
	enum Listed {
		MadeUpInTheSameChange,
		MadeUpAndSigned,
		OwnInTheSameChange,
	}

	#[test]
	fn a_key_that_another_lists_on_a_replicas_record_says_nothing_of_its_secrets() {
		for listed in [
			Listed::MadeUpInTheSameChange,
			Listed::MadeUpAndSigned,
			Listed::OwnInTheSameChange,
		] {
			let Asking {
				mut statements,
				phone,
				other: (other, other_record),
				space,
				..
			} = phone_asks_the_laptop(&format!("listed {listed:?}"));
			let thief = DecryptionKey::generate(&mut statements.rng);
			let made_up = key(&mut statements.rng);
			let on_record = match listed {
				Listed::MadeUpInTheSameChange | Listed::MadeUpAndSigned => made_up,
				Listed::OwnInTheSameChange => other,
			};
			let listing = statements.state(
				&other,
				true,
				(phone, SIGNING_KEY, Value::Text(on_record.to_text())),
			);
			let asking = [
				(SECRETS_FROM, Value::Reference(other_record)),
				(
					SECRETS_ENCRYPTION_KEY,
					Value::Text(crate::encryption_key_text(&thief.encryption_key())),
				),
			];
			for (field, value) in asking {
				match listed {
					Listed::MadeUpAndSigned => {
						statements.state(&made_up, true, (phone, field, value));
					}
					_ => statements.add(listing, &other, true, (phone, field, value)),
				}
			}
			let secrets_key = Key::generate(&mut statements.rng);
			let hash = nibkeep_crypto::key_hash(&secrets_key);
			gift(
				&mut statements,
				&other,
				((space, phone), &thief),
				(&secrets_key, hash),
			);
			// A record with two keys is no replica's own: it says nothing of the secrets, so
			// that none are given to it, rather than to the thief.
			let roster = statements.roster();
			assert_eq!(roster.secrets_from(phone), None, "{listed:?}");
			assert_eq!(roster.secrets_encryption_of(phone), None, "{listed:?}");
			assert_eq!(roster.secrets_given_to(phone).count(), 0, "{listed:?}");
		}
	}

	#[test]
	fn the_change_that_lists_a_replica_does_not_say_whom_it_asks() {
		let mut statements = Statements::new("listing asks");
		let (laptop, phone) = (key(&mut statements.rng), key(&mut statements.rng));
		let (laptop_record, phone_record) = (
			EntityId::random(&mut statements.rng),
			EntityId::random(&mut statements.rng),
		);
		let asked = DecryptionKey::generate(&mut statements.rng).encryption_key();
		statements.state(
			&laptop,
			true,
			(laptop_record, SIGNING_KEY, Value::Text(laptop.to_text())),
		);
		let listing = statements.state(
			&laptop,
			true,
			(phone_record, SIGNING_KEY, Value::Text(phone.to_text())),
		);
		for (field, value) in [
			(
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&asked)),
			),
			(SECRETS_FROM, Value::Reference(laptop_record)),
		] {
			statements.add(listing, &laptop, true, (phone_record, field, value));
		}
		let roster = statements.roster();
		assert_eq!(roster.secrets_from(phone_record), None);
		assert_eq!(roster.secrets_encryption_of(phone_record), None);
		// The phone says it, with its own key.
		for (field, value) in [
			(
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&asked)),
			),
			(SECRETS_FROM, Value::Reference(laptop_record)),
		] {
			statements.state(&phone, true, (phone_record, field, value));
		}
		let roster = statements.roster();
		assert_eq!(roster.secrets_from(phone_record), Some(laptop_record));
		assert_eq!(roster.secrets_encryption_of(phone_record), Some(&asked));
	}

	#[test]
	fn a_replica_whose_key_another_claims_first_says_nothing_of_its_secrets() {
		let mut statements = Statements::new("claimed early");
		let rng = &mut statements.rng;
		let (laptop, phone, other) = (key(rng), key(rng), key(rng));
		let (laptop_record, phone_record, other_record, claimed) = (
			EntityId::random(rng),
			EntityId::random(rng),
			EntityId::random(rng),
			EntityId::random(rng),
		);
		let space = nibkeep_base::SpaceId::random(rng);
		let (asked, thief) = (DecryptionKey::generate(rng), DecryptionKey::generate(rng));
		for (record, listed) in [(laptop_record, laptop), (other_record, other)] {
			statements.state(
				&laptop,
				true,
				(record, SIGNING_KEY, Value::Text(listed.to_text())),
			);
		}
		// The other lists the phone's key on a record of its own making, in a change it dates
		// before the phone's listing, and says there that this record asks the laptop with
		// its key: the phone's own statements would then count for that record.
		let early = statements.state(
			&other,
			true,
			(claimed, SIGNING_KEY, Value::Text(phone.to_text())),
		);
		for (field, value) in [
			(
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&thief.encryption_key())),
			),
			(SECRETS_FROM, Value::Reference(laptop_record)),
		] {
			statements.add(early, &other, true, (claimed, field, value));
		}
		statements.state(
			&laptop,
			true,
			(phone_record, SIGNING_KEY, Value::Text(phone.to_text())),
		);
		for (field, value) in [
			(
				SECRETS_ENCRYPTION_KEY,
				Value::Text(crate::encryption_key_text(&asked.encryption_key())),
			),
			(SECRETS_FROM, Value::Reference(laptop_record)),
		] {
			for record in [phone_record, claimed] {
				statements.state(&phone, true, (record, field.clone(), value.clone()));
			}
		}
		let secrets_key = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&secrets_key);
		for (record, to) in [(phone_record, &asked), (claimed, &thief)] {
			gift(
				&mut statements,
				&laptop,
				((space, record), to),
				(&secrets_key, hash),
			);
		}
		let roster = statements.roster();
		for record in [phone_record, claimed] {
			assert_eq!(roster.secrets_encryption_of(record), None);
			assert_eq!(roster.secrets_given_to(record).count(), 0);
		}
		assert_eq!(roster.owner_of(&phone), None);
	}

	#[test]
	fn a_key_listed_on_the_record_of_the_replica_asked_gives_nothing() {
		let Asking {
			mut statements,
			laptop: (laptop, laptop_record),
			phone,
			other: (other, _),
			space,
			asked,
		} = phone_asks_the_laptop("listed on the giver");
		let secrets_key = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&secrets_key);
		gift(
			&mut statements,
			&laptop,
			((space, phone), &asked),
			(&secrets_key, hash),
		);
		assert_eq!(statements.roster().secrets_given_to(phone).count(), 1);
		assert_eq!(statements.roster().owner_of(&laptop), Some(laptop_record));
		// The other lists a key of its own making on the laptop's record, and gives the phone
		// a key of its making with it, of a larger number, that the phone would seal with.
		let made_up_signing = key(&mut statements.rng);
		statements.state(
			&other,
			true,
			(
				laptop_record,
				SIGNING_KEY,
				Value::Text(made_up_signing.to_text()),
			),
		);
		let made_up = Key::generate(&mut statements.rng);
		let sealed = SealedSecretsKey::seal(
			&asked.encryption_key(),
			space,
			phone,
			(u64::MAX, &made_up, nibkeep_crypto::key_hash(&made_up)),
			(space, u64::MAX, &made_up),
			&mut statements.rng,
		);
		statements.state(
			&made_up_signing,
			true,
			(phone, SECRETS_KEY, Value::Text(sealed.to_text())),
		);
		// Nothing the laptop's record has signed counts now, not even what the laptop gave.
		let roster = statements.roster();
		assert_eq!(roster.secrets_from(phone), Some(laptop_record));
		assert_eq!(roster.secrets_given_to(phone).count(), 0);
		assert_eq!(roster.owner_of(&laptop), None);
	}

	#[test]
	fn a_keys_rotation_is_its_first_statement_whether_it_counts_or_not() {
		let mut statements = Statements::new("set aside rotation");
		let (thief, laptop) = (key(&mut statements.rng), key(&mut statements.rng));
		let record = EntityId::random(&mut statements.rng);
		let decryption = DecryptionKey::generate(&mut statements.rng);
		let space = nibkeep_base::SpaceId::random(&mut statements.rng);
		let space_key = Key::generate(&mut statements.rng);
		let hash = nibkeep_crypto::key_hash(&space_key);
		let sealed = SealedSpaceKey::seal(
			&decryption.encryption_key(),
			space,
			record,
			(7, &space_key, hash),
			&mut statements.rng,
		);
		// The thief's rotation is set aside; a later statement of its key, as a pairing that
		// grants it would make, counts.
		for (author, counts) in [(&thief, false), (&laptop, true)] {
			statements.state(
				author,
				counts,
				(record, SPACE_KEY, Value::Text(sealed.to_text())),
			);
		}
		// The rotation is the earliest statement, whose change does not count: whether it is
		// given out is for the replica to decide, by who made it.
		let roster = statements.roster();
		let rotation = roster.rotation(7, &hash).expect("a rotation");
		assert_eq!(rotation.change, roster.given_to(record)[0].change);
		assert_eq!(roster.given_to(record).len(), 2);
	}

	/// A statement of a code of `member`, made by `author`, on the code's record.
	fn code_statement(
		statements: &Statements,
		code: &nibkeep_crypto::RecoveryCode,
		member: EntityId,
	) -> (EntityId, FieldName, Value) {
		let stated = RecoveryStatement::sign(code, member);
		(
			stated.record(statements.space),
			RECOVERY_KEY,
			Value::Text(stated.to_text()),
		)
	}

	#[test]
	fn a_code_is_named_by_a_counting_statement_on_its_record() {
		let mut statements = Statements::new("a code");
		let laptop = key(&mut statements.rng);
		let user = EntityId::random(&mut statements.rng);
		let code = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let named = code_statement(&statements, &code, user);
		let record = named.0;
		// A statement whose change does not count names nothing.
		statements.state(&laptop, false, named.clone());
		assert!(statements.roster().code(record).is_none());
		let change = statements.state(&laptop, true, named);
		let roster = statements.roster();
		let found = roster.code(record).expect("the code");
		assert_eq!(found.statement, RecoveryStatement::sign(&code, user));
		assert_eq!((found.author, found.change), (laptop, change));
		assert!(found.stands());
		assert_eq!(roster.codes().count(), 1);
		// A code is no replica: its key is listed nowhere.
		assert_eq!(roster.replica_of(&code.signing_key().verifying_key()), None);
		assert_eq!(roster.replicas().count(), 0);
	}

	#[test]
	fn a_code_on_another_record_or_with_other_keys_is_not_named() {
		let mut statements = Statements::new("not a code");
		let laptop = key(&mut statements.rng);
		let user = EntityId::random(&mut statements.rng);
		let code = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let other = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let (record, field, value) = code_statement(&statements, &code, user);
		// On a record that is not the one derived from the space, the member and the key.
		let elsewhere = EntityId::random(&mut statements.rng);
		statements.state(&laptop, true, (elsewhere, field.clone(), value));
		// With an encryption key that the recovery key did not sign.
		let mut planted = RecoveryStatement::sign(&code, user);
		planted.encryption = RecoveryStatement::sign(&other, user).encryption;
		statements.state(
			&laptop,
			true,
			(record, field.clone(), Value::Text(planted.to_text())),
		);
		// Not a statement at all.
		statements.state(&laptop, true, (record, field, Value::Text("code".into())));
		assert_eq!(statements.roster().codes().count(), 0);
	}

	#[test]
	fn a_codes_entry_is_taken_only_as_the_code_signed_it_for_the_space_and_its_record() {
		let mut statements = Statements::new("a code's entry");
		let laptop = key(&mut statements.rng);
		let user = EntityId::random(&mut statements.rng);
		let code = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let other = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let named = code_statement(&statements, &code, user);
		let record = named.0;
		statements.state(&laptop, true, named);
		let (root, hash) = (Hash::of(b"the first change"), Hash::of(b"the first key"));
		let space = statements.space;
		let entry = |code: &nibkeep_crypto::RecoveryCode, space, member| {
			RecoveryEntry::sign(code, (space, member, root), hash)
		};
		let state = |statements: &mut Statements, counts, entry: &RecoveryEntry| {
			statements.state(
				&laptop,
				counts,
				(record, RECOVERY_ENTRY, Value::Text(entry.to_text())),
			);
		};
		let genuine = entry(&code, space, user);
		// In a change that does not count; for another space; for another member, whose record
		// it is not; by another code; with what the code did not sign.
		state(&mut statements, false, &genuine);
		let another_space = nibkeep_base::SpaceId::random(&mut statements.rng);
		state(&mut statements, true, &entry(&code, another_space, user));
		let another_member = EntityId::random(&mut statements.rng);
		state(&mut statements, true, &entry(&code, space, another_member));
		state(&mut statements, true, &entry(&other, space, user));
		let mut forged = genuine.clone();
		forged.root = Hash::of(b"another first change");
		state(&mut statements, true, &forged);
		assert_eq!(statements.roster().code(record).unwrap().entries, []);
		// The genuine one, stated twice, is taken once, whoever states it.
		state(&mut statements, true, &genuine);
		let thief = key(&mut statements.rng);
		statements.state(
			&thief,
			true,
			(record, RECOVERY_ENTRY, Value::Text(genuine.to_text())),
		);
		assert_eq!(statements.roster().code(record).unwrap().entries, [genuine]);
	}

	#[test]
	fn a_revocation_on_a_codes_record_revokes_it_and_no_other() {
		let mut statements = Statements::new("revoking a code");
		let laptop = key(&mut statements.rng);
		let (olga, leo) = (
			EntityId::random(&mut statements.rng),
			EntityId::random(&mut statements.rng),
		);
		let code = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let recovery_key = Value::Text(code.signing_key().verifying_key().to_text());
		// Leo copied Olga's code onto a record of his own.
		let olgas = code_statement(&statements, &code, olga);
		let leos = code_statement(&statements, &code, leo);
		let (olgas_record, leos_record) = (olgas.0, leos.0);
		statements.state(&laptop, true, olgas);
		statements.state(&laptop, true, leos);
		// A revocation of another key, or one that does not count, revokes nothing.
		let another = key(&mut statements.rng);
		statements.state(
			&laptop,
			true,
			(
				olgas_record,
				RECOVERY_REVOKED,
				Value::Text(another.to_text()),
			),
		);
		statements.state(
			&laptop,
			false,
			(olgas_record, RECOVERY_REVOKED, recovery_key.clone()),
		);
		assert!(statements.roster().code(olgas_record).unwrap().stands());
		// Revoking it on his record leaves hers standing.
		let revocation =
			statements.state(&laptop, true, (leos_record, RECOVERY_REVOKED, recovery_key));
		let roster = statements.roster();
		assert!(roster.code(olgas_record).unwrap().stands());
		let revoked = roster.code(leos_record).unwrap();
		assert!(!revoked.stands());
		assert_eq!(revoked.revoked_by, [revocation]);
	}

	#[test]
	fn the_first_counting_statement_of_a_code_is_the_one_that_names_it() {
		let mut statements = Statements::new("stated twice");
		let (laptop, phone) = (key(&mut statements.rng), key(&mut statements.rng));
		let user = EntityId::random(&mut statements.rng);
		let code = nibkeep_crypto::RecoveryCode::generate(&mut statements.rng);
		let named = code_statement(&statements, &code, user);
		let record = named.0;
		let first = statements.state(&laptop, true, named.clone());
		statements.state(&phone, true, named);
		let roster = statements.roster();
		let found = roster.code(record).unwrap();
		assert_eq!((found.author, found.change), (laptop, first));
	}
}
