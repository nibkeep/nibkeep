//! The library's replicas, and what pairing and syncing read and change: the list of
//! replicas in the personal space, a new replica's records, and each space's changes, keys
//! and blobs ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md), [Identity](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/identity.md#3-a-new-replica)).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, Key, Signature, SigningKey, VerifyingKey};
use nibkeep_governance::{
	ENCRYPTION_KEY, ENDPOINT, Grant, JoinRequest, Listing, MAX_NAME_LEN, NewReplica,
	RecoveryListing, SealedSpaceKey, encryption_key_text, endpoint_of_text, endpoint_secret,
	endpoint_text,
};
use nibkeep_keys::{
	CompartmentKey, KeyFile, NewProtection, ReplicaKeys, SealedSecrets, SecretsKeys, SpaceKeyMap,
};
use nibkeep_log::{ChangeId, LogError, Parcel, Received, WrappedBlobKey, WrappedKey};
use nibkeep_model::{Operation, Statement, Value, fields};
use nibkeep_store::{StoreError, log_path};
use nibkeep_sync::{Challenge, Holder, Holding, Sponsoring};

use super::planner::Keeps;
use super::removal::{Removals, answer_from_file};
use super::secrets::SecretsState;
use super::taking::NewDevice;
use super::{INFO, KEYS, Library, Space, code_types, new_replica_folder};
use crate::NodeError;
use crate::info::Info;
use crate::role::Records;

/// A replica of the library, as its personal space lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "each says something apart of the replica, which lists show apart"
)]
pub struct Replica {
	/// The replica.
	pub id: EntityId,
	/// Its name: the name of the app that keeps it.
	pub name: Option<String>,
	/// Its device.
	pub device: Option<EntityId>,
	/// Its device's name.
	pub device_name: Option<String>,
	/// Its endpoint ID, which the others connect to, if it has one.
	pub endpoint: Option<[u8; 32]>,
	/// Its verifying key.
	pub signing: Option<VerifyingKey>,
	/// Whether it is this replica.
	pub this: bool,
	/// Whether it was revoked, every key of it: no replica sends it anything, and of its
	/// changes only those that every replica which revoked it had count.
	pub revoked: bool,
	/// Whether its record was deleted, which keeps it from nothing: only revoking does.
	pub deleted: bool,
	/// Whether it is an AI agent's, as the change that listed its key says.
	pub ai_agent: bool,
	/// How many of its changes are set aside in the personal space, waiting for review.
	pub set_aside: usize,
	/// Whether it holds the secrets: the records give it the secrets keys.
	pub holds_secrets: bool,
	/// Whether the records give it any secrets key at all, by whichever replica, so that it
	/// may read the secrets: what revoking it cannot take back. A replica that turned against
	/// its user can stop holding them, as the records say, by listing a key on its record.
	pub given_secrets: bool,
	/// Whether it asked for the secrets, and was not given them yet.
	pub asks_for_secrets: bool,
	/// The replica it takes the secrets from: the one it asked, or itself.
	pub secrets_from: Option<EntityId>,
	/// For another user's replica, a shared space that lists it, with which this replica
	/// syncs it; `None` for the user's own.
	pub shared: Option<SpaceId>,
	/// For another user's replica, the name of the member it is of there.
	pub member: Option<String>,
	/// How the person marked its device: as an archive node, and as kept away from home;
	/// for another user's replica, unmarked, as only its own account says.
	pub marks: super::accounts::DeviceMarks,
}

impl Replica {
	/// Another member's replica, as a shared space lists it.
	pub(crate) fn of_member(
		(space, member): (SpaceId, Option<&str>),
		replica: &super::members::MemberReplica,
	) -> Self {
		Self {
			id: replica.record,
			name: replica.app.clone(),
			device: None,
			device_name: replica.device.clone(),
			endpoint: replica.endpoint,
			signing: Some(replica.signing),
			this: false,
			revoked: replica.revoked,
			deleted: false,
			ai_agent: replica.ai_agent,
			set_aside: 0,
			holds_secrets: false,
			given_secrets: false,
			asks_for_secrets: false,
			secrets_from: None,
			shared: Some(space),
			member: member.map(str::to_owned),
			marks: super::accounts::DeviceMarks::default(),
		}
	}
}

impl Library {
	/// The 32-byte secret of this replica's endpoint key, derived from its signing key.
	pub(crate) fn endpoint_secret(&self) -> Key {
		endpoint_secret(&self.replica.signing)
	}

	/// This replica's endpoint ID.
	#[must_use]
	pub fn endpoint(&self) -> [u8; 32] {
		endpoint_of(&self.endpoint_secret())
	}

	/// The replicas that the personal space's counting governing changes list, with what
	/// their records say of them, revoked and deleted ones too, as all of them but the
	/// revoked sync: this one first, then the others by ID.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn replicas(&self) -> Result<Vec<Replica>, NodeError> {
		let personal = &self.spaces[&self.info.personal];
		let roster = personal.stored.log().roster(&personal.keys)?;
		let log = personal.stored.log();
		let state = log.state();
		let name = |entity: EntityId| {
			super::entities::display_name(state, &personal.types, &self.languages, entity)
		};
		// The changes set aside, by the key that signed them.
		let mut set_aside: std::collections::BTreeMap<[u8; 32], usize> =
			std::collections::BTreeMap::new();
		for id in log.set_aside_changes()? {
			if let Some(change) = log.change(id)? {
				*set_aside.entry(*change.author().as_bytes()).or_default() += 1;
			}
		}
		let holders = roster.secrets_holders();
		let mut replicas = Vec::new();
		for id in roster.replicas() {
			// What the record says only names it: a record without a type, or deleted, or
			// another owner's, is of a replica all the same.
			let record = nibkeep_governance::replica(state, id)?;
			let keys = roster.keys_of(id);
			let mut revoked = true;
			let mut ai_agent = false;
			for key in keys {
				revoked &= log.is_revoked(key, &personal.keys)?;
				ai_agent |= log.is_ai_agent(key, &personal.keys)?;
			}
			let device = record.as_ref().and_then(|record| record.device);
			replicas.push(Replica {
				id,
				name: name(id)?,
				device,
				device_name: device.map(name).transpose()?.flatten(),
				endpoint: record.as_ref().and_then(|record| record.endpoint),
				signing: keys.first().copied(),
				this: id == self.info.replica,
				revoked,
				deleted: record.is_none_or(|record| record.deleted),
				ai_agent,
				set_aside: keys
					.iter()
					.filter_map(|key| set_aside.get(key.as_bytes()))
					.sum(),
				holds_secrets: holders.contains(&id),
				given_secrets: roster.given_any_secrets(id),
				asks_for_secrets: roster.secrets_encryption_of(id).is_some()
					&& roster.secrets_from(id).is_some_and(|from| from != id)
					&& !holders.contains(&id),
				secrets_from: roster.secrets_from(id),
				shared: None,
				member: None,
				marks: device
					.map(|device| self.device_marks(device))
					.transpose()?
					.unwrap_or_default(),
			});
		}
		replicas.sort_by_key(|replica| (!replica.this, replica.id));
		Ok(replicas)
	}

	/// The replicas to sync with: the user's other replicas, then those of other members of
	/// the shared spaces that this replica is listed in, each with an endpoint and not
	/// revoked.
	///
	/// # Errors
	///
	/// Returns an error if a space cannot be read.
	pub fn peers(&self) -> Result<Vec<Replica>, NodeError> {
		let own = self.replica.signing.verifying_key();
		let mut peers: Vec<Replica> = self
			.replicas()?
			.into_iter()
			.filter(|replica| {
				!replica.this
					&& !replica.revoked
					&& replica.endpoint.is_some()
					&& replica.signing != Some(own)
			})
			.collect();
		for other in self.other_members_replicas() {
			if !peers.iter().any(|peer| peer.signing == other.signing) {
				peers.push(other);
			}
		}
		Ok(peers)
	}

	/// Makes sure that this replica's record has its current encryption key and endpoint ID,
	/// which an AI agent may have taken away.
	/// An AI agent's replica, which may not write them, leaves them to the replica that paired
	/// it.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read or the change saved.
	pub fn record_own_endpoint(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if self.is_ai_agent()? {
			return Ok(());
		}
		let personal = &self.spaces[&self.info.personal];
		let roster = personal.stored.log().roster(&personal.keys)?;
		let state = personal.stored.log().state();
		let endpoint = self.endpoint();
		let mut operations = Vec::new();
		if roster.encryption_of(self.info.replica).is_none() {
			operations.push(Operation::assert(Statement::new(
				StatementId::random(rng),
				self.info.replica,
				ENCRYPTION_KEY,
				Value::Text(encryption_key_text(
					&self.replica.decryption.encryption_key(),
				)),
			)));
		}
		// Read as the record reads it, the first that is an endpoint ID, but also where the
		// record is no longer a replica's, which an infected device of the user's can make
		// it: else this replica would state its endpoint again at every sync.
		let stated = state.current(self.info.replica, &ENDPOINT)?;
		let first = stated
			.iter()
			.filter_map(|statement| statement.value.as_text())
			.find_map(endpoint_of_text);
		if first != Some(endpoint) {
			let current: Vec<StatementId> = stated.iter().map(|statement| statement.id).collect();
			operations.push(Operation::replace(
				Statement::new(
					StatementId::random(rng),
					self.info.replica,
					ENDPOINT,
					Value::Text(endpoint_text(&endpoint)),
				),
				current,
			));
		}
		if operations.is_empty() {
			return Ok(());
		}
		self.commit(self.info.personal, operations, wall_millis, rng)
	}

	/// What to grant a replica that asks to join, and the operations that record it, as
	/// the identity specification says: who it is, and every space key sealed to it, and for
	/// an AI agent's replica, that it is one, named after the AI agent. This replica's own
	/// record gets its encryption key and endpoint ID first, if it lacks them, and the keys it
	/// holds.
	fn prepare_replica(
		&mut self,
		request: &JoinRequest,
		(endpoint, ai_agent, give_secrets): (&[u8; 32], Option<&str>, bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(Grant, Vec<Operation>, AskedFor), NodeError> {
		self.record_own_endpoint(wall_millis, rng)?;
		// So that the new replica knows which keys this one holds, and gives it none of them.
		self.give_keys(self.info.personal, wall_millis, rng)?;
		// The new replica's record is derived from the space and its key, as the keys are
		// sealed to it.
		let replica = nibkeep_governance::replica_record(self.info.personal, &request.signing);
		// Only the personal space's keys: the others reach a new replica as they reach every
		// replica of the user, through the personal space, once it asks to be listed there,
		// and never an AI agent's.
		let personal_keys: std::collections::BTreeMap<_, _> = self
			.replica
			.spaces
			.iter()
			.filter(|(space, _)| **space == self.info.personal)
			.map(|(space, keys)| (*space, keys.clone()))
			.collect();
		let sealed =
			nibkeep_governance::seal_space_keys(&request.encryption, replica, &personal_keys, rng);
		// Its records state the keys of the personal space it is granted, as sealed, but key
		// 0, which every replica holds, and keys whose rotation does not count, which no
		// replica writes with: so that a statement of a key never makes it count.
		let personal = self.info.personal;
		let roster = self.roster(personal)?;
		let mut makers = super::revocation::Makers::of(self.space(personal)?);
		let mut granted: Vec<SealedSpaceKey> = Vec::new();
		for (epoch, sealed) in sealed
			.iter()
			.filter(|space| space.space == personal)
			.flat_map(|space| &space.keys)
		{
			let Some(key) = self
				.replica
				.spaces
				.get(&personal)
				.and_then(|keys| keys.get(epoch))
			else {
				continue;
			};
			let hash = nibkeep_crypto::key_hash(key);
			if let Some(rotation) = roster.rotation(*epoch, &hash)
				&& makers.made(rotation.change)?
			{
				granted.push(SealedSpaceKey {
					epoch: *epoch,
					hash,
					sealed: sealed.clone(),
				});
			}
		}
		drop(makers);
		// It asks this replica for the secrets, if the records say that this one holds them,
		// and is given them, if the person chose to, and they are unlocked; it states itself
		// that it asks, after its first sync, as only its own key may. A key that the
		// library's passphrase opens would make the two layers one.
		let asks = match &request.secrets {
			Some(key) if *key == request.encryption => {
				return Err(NodeError::Damaged(
					"the new replica asks for the secrets with the key of its replica".to_owned(),
				));
			}
			Some(key) if ai_agent.is_none() && self.secrets_state()? == SecretsState::Here => {
				let unlocked = give_secrets && self.unlock_kept_secrets()?;
				if unlocked {
					// Records that do not say yet that this replica holds them say so now.
					self.settle_secrets(wall_millis, rng)?;
				}
				let own = self.info.replica;
				let holds = self.roster(personal)?.secrets_holders().contains(&own);
				let given = if unlocked && holds {
					let held = self.space(personal)?.keys.secrets_keys();
					super::secrets::sealed_secrets_keys(held, (personal, replica), key, rng)
				} else {
					Vec::new()
				};
				holds.then_some(given)
			}
			_ => None,
		};
		let asked_for = match &asks {
			None => AskedFor::Nothing,
			Some(given) if given.is_empty() => AskedFor::Secrets,
			Some(_) => AskedFor::SecretsGiven,
		};
		let added = nibkeep_governance::add_replica(
			&NewReplica {
				space: personal,
				user: self.info.user,
				signing: &request.signing,
				encryption: &request.encryption,
				endpoint,
				device: &request.device,
				app: ai_agent.unwrap_or(&request.app),
				keys: &granted,
				ai_agent: ai_agent.is_some(),
				given: asks.as_deref().unwrap_or_default(),
				// Its key signs its listing once it knows who it is, from the grant.
				listing: None,
			},
			rng,
		);
		let mut operations = added.governing;
		operations.extend(added.operations);
		let grant = Grant {
			chosen: Vec::new(),
			asks_first: false,
			user: self.info.user,
			device: added.device,
			replica: added.replica,
			space: personal,
			sponsor: self.replica.signing.verifying_key(),
			keys: sealed,
			ai_agent: ai_agent.is_some(),
		};
		Ok((grant, operations, asked_for))
	}

	/// Revokes a replica that pairing added, after its first sync failed: the pairing
	/// failed, and as it got the space keys, the personal space gets a new key. Its records
	/// and its device's are deleted too, so that they are not listed.
	pub(crate) fn forget_replica(
		&mut self,
		grant: &Grant,
		key: &VerifyingKey,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		self.revoke_key(grant.replica, key, wall_millis, rng)?;
		let operations = [grant.replica, grant.device]
			.into_iter()
			.map(|entity| {
				Operation::assert(Statement::new(
					StatementId::random(rng),
					entity,
					fields::DELETED,
					Value::Boolean(true),
				))
			})
			.collect();
		self.commit(self.info.personal, operations, wall_millis, rng)
	}
}

/// The public half of an endpoint key: an Ed25519 key, as the signing keys are.
pub(crate) fn endpoint_of(secret: &Key) -> [u8; 32] {
	*SigningKey::from_bytes(secret.expose_bytes())
		.verifying_key()
		.as_bytes()
}

/// The library as a sync reads and changes it.
pub struct Syncing<'a> {
	library: &'a mut Library,
	/// The key that a new replica trusts for its first sync, for spaces that list no replica
	/// yet: its sponsor's.
	trusted: Option<VerifyingKey>,
	/// The peer that introduced itself with its listings by a recovery code, and the spaces
	/// and groups where a code that stands here signed its listing, which sync with it as
	/// with their replicas.
	introduced: Option<(VerifyingKey, BTreeSet<SpaceId>)>,
	/// The wall clock, read as each change arrives.
	now: &'a dyn Fn() -> u64,
	/// How many blobs and ordinary payloads were refused, as the disk would have had too
	/// little room left, of those that this replica lacked.
	refused: usize,
	/// How many bytes the disk had free, as last read, less what was received since.
	free: Option<u64>,
	/// For each space, the peers that it ended, as found since a governing change last came:
	/// what ended them, and what they may still get.
	ended: RefCell<BTreeMap<SpaceId, EndedPeers>>,
	/// The batch that saves together what is received of a space, if one is open.
	batch: Option<Receiving>,
	/// Random numbers for the envelopes of this replica's pages of accounts.
	rng: UnwrapErr<SysRng>,
	/// Pages of accounts received and checked, kept together when the batch ends.
	pages: Vec<(SpaceId, nibkeep_store::StoredPage)>,
	/// The sealed blobs received, each read whole as it came, with when: checks, recorded
	/// together as the space's sync ends.
	received_checks: Vec<(SpaceId, Hash, u64)>,
	/// What the replica keeps of each space's blobs, as worked out for this sync: again only
	/// once changes or keys came since, which may change what its files are.
	plan: Option<BTreeMap<SpaceId, Keeps>>,
	/// The records of the sync role, opened once the other replica asks for proofs, which
	/// hold what was proved.
	records: Option<Records>,
	/// The pass of removals whose challenges this sync puts to the other replica, if one is.
	removals: Option<&'a mut Removals>,
}

/// A batch of what a sync receives of a space, saved together ([technical design: saving in
/// batches]): changes and record keys, up to [`BATCH_ITEMS`] or for [`BATCH_TIME`].
///
/// [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches
struct Receiving {
	space: SpaceId,
	/// How many items it holds.
	items: usize,
	/// When it began.
	began: Instant,
}

/// How many items a batch of what a sync receives holds at most, as importers' batches do.
const BATCH_ITEMS: usize = 1_000;

/// How long a batch of what a sync receives stays open at most, as importers' batches do:
/// the space's other writers wait meanwhile.
const BATCH_TIME: Duration = Duration::from_secs(1);

/// For each peer's key, what ended its replica in a space, if anything did.
type EndedPeers = BTreeMap<[u8; 32], Option<Rc<Ended>>>;

/// What ended a peer's replica in a space, as its member left or was removed, or another
/// replica of its member's revoked it, and the changes that it may still get: those that
/// ended it and what they build on, but none at all once it is revoked.
pub(crate) struct Ended {
	/// The removals and revocations of it.
	ending: Vec<ChangeId>,
	reach: BTreeSet<ChangeId>,
	revoked: bool,
}

impl<'a> Syncing<'a> {
	/// Whether a space syncs with a peer through groups: a group's log with the replicas of
	/// each space that adds the group, which remain there, as a space's sync carries its
	/// groups' logs; and a space that this replica has nothing of yet, which a group it is in
	/// gave it, with the replicas of that group.
	fn through_groups(
		&self,
		id: SpaceId,
		governed: bool,
		peer: &VerifyingKey,
	) -> Result<bool, NodeError> {
		let own = self.library.replica.signing.verifying_key();
		for (&other, open) in &self.library.spaces {
			if other == id || other == self.library.info.personal {
				continue;
			}
			let log = open.stored.log();
			let remains = |key: &VerifyingKey| -> Result<bool, NodeError> {
				Ok(log.is_listed(key, &open.keys)?
					&& !log.is_revoked(key, &open.keys)?
					&& !log.is_removed(key, &open.keys)?)
			};
			let adds = self.library.inner_groups(other)?.contains(&id);
			if adds && remains(peer)? {
				return Ok(true);
			}
			if !governed
				&& open.is_group()?
				&& remains(&own)?
				&& remains(peer)?
				&& gave(log, &open.keys, &own, other, id)?
			{
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// A sync of `library`, with the wall clock `now`.
	pub fn new(library: &'a mut Library, now: &'a dyn Fn() -> u64) -> Self {
		library.observe_wall(now());
		Self {
			library,
			trusted: None,
			introduced: None,
			now,
			refused: 0,
			free: None,
			ended: RefCell::new(BTreeMap::new()),
			batch: None,
			rng: UnwrapErr(SysRng),
			pages: Vec::new(),
			received_checks: Vec::new(),
			plan: None,
			records: None,
			removals: None,
		}
	}

	/// The same, putting to the other replica the challenges that a pass of removals has for
	/// it, and noting its answers there.
	#[must_use]
	pub fn proving(mut self, removals: &'a mut Removals) -> Self {
		self.removals = Some(removals);
		self
	}

	/// What ended `peer`'s replica in a space, if the space ended it, found once for each
	/// sync of the space, and again after a governing change came.
	fn ended(&self, space: SpaceId, peer: &VerifyingKey) -> Result<Option<Rc<Ended>>, NodeError> {
		if let Some(found) = self
			.ended
			.borrow()
			.get(&space)
			.and_then(|peers| peers.get(peer.as_bytes()))
		{
			return Ok(found.clone());
		}
		let open = self.library.space(space)?;
		let log = open.stored.log();
		let removals = log.removals_of(peer, &open.keys)?;
		let revocations = log.revocations_of(Some(peer), &open.keys)?;
		let found = if removals.is_empty() && revocations.is_empty() {
			None
		} else {
			// A revoked replica gets nothing, which would tell it of its revocation.
			let revoked = !revocations.is_empty();
			let reach = if revoked {
				BTreeSet::new()
			} else {
				log.with_ancestors(&removals)?
			};
			let mut ending = removals;
			ending.extend(revocations);
			Some(Rc::new(Ended {
				ending,
				reach,
				revoked,
			}))
		};
		self.ended
			.borrow_mut()
			.entry(space)
			.or_default()
			.insert(*peer.as_bytes(), found.clone());
		Ok(found)
	}

	/// Whether the disk has room for `size` bytes more and 1 GiB besides. The free space is
	/// read once, and again whenever what was received since may have used up the room.
	fn has_room(&mut self, size: u64) -> Result<bool, NodeError> {
		let directory = &self.library.directory;
		let free = match self.free {
			Some(free) if leaves_room(free, size.saturating_mul(2)) => free,
			_ => crate::place::available_space(directory).map_err(NodeError::file(directory))?,
		};
		if !leaves_room(free, size) {
			self.free = Some(free);
			return Ok(false);
		}
		self.free = Some(free - size);
		Ok(true)
	}

	/// How many blobs and ordinary changes were refused, as the disk would have had less
	/// than 1 GiB free: those that this replica lacked, and lacks still.
	#[must_use]
	pub const fn refused(&self) -> usize {
		self.refused
	}

	/// The same, for a new replica's first sync with its sponsor.
	pub(crate) fn trusting(mut self, sponsor: VerifyingKey) -> Self {
		self.trusted = Some(sponsor);
		self
	}

	fn space(&mut self, space: SpaceId) -> Result<&mut Space, NodeError> {
		self.library
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))
	}

	/// Saves what comes of `space` in a batch from now on, beginning one unless one is open:
	/// one of another space ends first. While another program has the space open, which
	/// would wait for the batch, what comes is saved item by item, as the changes of a burst
	/// are then.
	fn batch(&mut self, space: SpaceId) -> Result<(), NodeError> {
		if self
			.batch
			.as_ref()
			.is_some_and(|batch| batch.space == space)
		{
			return Ok(());
		}
		self.end_batch()?;
		let open = self.space(space)?;
		if open.stored.shared_with_others() {
			return Ok(());
		}
		open.stored.begin_batch(&open.keys)?;
		self.batch = Some(Receiving {
			space,
			items: 0,
			began: Instant::now(),
		});
		Ok(())
	}

	/// Counts an item that the open batch received, if one is open, and ends the batch once it
	/// holds [`BATCH_ITEMS`], has been open for [`BATCH_TIME`], or holds a purge, whose
	/// files and keys are to leave the disk soon.
	fn received_in_batch(&mut self) -> Result<(), NodeError> {
		let Some(batch) = &mut self.batch else {
			return Ok(());
		};
		batch.items += 1;
		let purged = self
			.library
			.spaces
			.get(&batch.space)
			.is_some_and(|open| open.stored.batch_purged());
		if purged || batch.items >= BATCH_ITEMS || batch.began.elapsed() >= BATCH_TIME {
			self.end_batch()?;
		}
		Ok(())
	}

	/// Ends the open batch, if one is, which saves what it received. If saving fails, none of
	/// it is saved, and what the library keeps of the space is read again.
	fn end_batch(&mut self) -> Result<(), NodeError> {
		let ended = match self.batch.take() {
			None => Ok(()),
			Some(batch) => {
				let ended = self.space(batch.space)?.stored.end_batch();
				if ended.is_err() {
					let _ = self.library.read_again(batch.space);
				}
				ended.map_err(NodeError::from)
			}
		};
		self.save_pages();
		ended
	}

	/// Keeps the pages of accounts received since the last were kept, together for each
	/// space. Accounts only count and show copies, so pages that cannot be kept now, as
	/// while another program holds the log file, are left for the next sync, which brings
	/// them again, and the sync goes on.
	fn save_pages(&mut self) {
		let pages = std::mem::take(&mut self.pages);
		let mut by_space: BTreeMap<SpaceId, Vec<nibkeep_store::StoredPage>> = BTreeMap::new();
		for (space, page) in pages {
			by_space.entry(space).or_default().push(page);
		}
		for (space, pages) in by_space {
			let _ = self.library.save_accounts(space, &pages);
		}
	}

	/// Records the checks of the sealed blobs received: a check that cannot be recorded
	/// waits for the regular check, and the sync goes on.
	fn save_received_checks(&mut self) {
		let mut by_space: BTreeMap<SpaceId, Vec<(Hash, u64)>> = BTreeMap::new();
		for (space, sealed, at) in std::mem::take(&mut self.received_checks) {
			by_space.entry(space).or_default().push((sealed, at));
		}
		for (space, intact) in by_space {
			if let Some(open) = self.library.spaces.get_mut(&space) {
				let _ = open.stored.record_checks(intact, Vec::new(), Vec::new());
			}
		}
	}
}

impl Syncing<'_> {
	/// The sealed blobs of a space that the replica keeps and lacks, of those that `lists`
	/// says another holds, at most `most` of them, none of `asked`: of two of one content,
	/// the one with the smallest sealed hash of those, unless one held here is smaller.
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read.
	pub(crate) fn wanted_among(
		&mut self,
		space: SpaceId,
		lists: &dyn Fn(&Hash) -> bool,
		asked: &BTreeSet<Hash>,
		most: usize,
	) -> Result<Vec<Hash>, NodeError> {
		if self.plan.is_none() {
			self.plan = Some(self.library.plan((self.now)())?);
		}
		let keeps = self
			.plan
			.as_ref()
			.and_then(|plan| plan.get(&space))
			.cloned()
			.unwrap_or(Keeps::All);
		let open = self.library.space(space)?;
		let log = open.stored.log();
		let held: BTreeSet<Hash> = open.stored.sealed_blobs()?.into_iter().collect();
		let aside: BTreeSet<Hash> = open.stored.aside_blobs().into_iter().collect();
		let sealed: BTreeSet<Hash> = log.wrapped_blob_keys().map(|(sealed, _)| *sealed).collect();
		let mut wanted = Vec::new();
		for sealed in sealed {
			if held.contains(&sealed) || asked.contains(&sealed) || !lists(&sealed) {
				continue;
			}
			// A blob whose content this replica cannot tell, as a secret file's while the
			// secrets are locked, is kept, as every secret file is.
			let ordinary = !log.is_secret_blob(&sealed);
			if let Some(content) = log.content_of_sealed(&sealed).filter(|_| ordinary) {
				// One whose damaged copy is kept aside comes again all the same, so that the
				// damaged copy goes, and the copy that came then goes the planner's way.
				if !keeps.keeps(&content) && !aside.contains(&sealed) {
					continue;
				}
				// Of sealed copies of one content, the one with the smallest sealed hash of those
				// that the other replica holds, unless one held here is smaller: a replica
				// settles on the smallest, and lets a larger one that it holds go once the
				// smaller has come.
				let copies: Vec<Hash> = log
					.blob_keys(&content)
					.map(|(copy, _)| *copy)
					.filter(|copy| !log.is_secret_blob(copy))
					.collect();
				let first_listed = copies.iter().filter(|copy| lists(copy)).min();
				let first_held = copies.iter().filter(|copy| held.contains(copy)).min();
				if first_listed != Some(&sealed) || first_held.is_some_and(|first| *first < sealed)
				{
					continue;
				}
			}
			wanted.push(sealed);
			if wanted.len() >= most {
				break;
			}
		}
		Ok(wanted)
	}
}

impl Drop for Syncing<'_> {
	/// Saves what the open batch received, as a sync that fails keeps what it received; after
	/// a panic, leaves it out, as the log may hold a change half taken.
	fn drop(&mut self) {
		if std::thread::panicking() {
			if let Some(batch) = self.batch.take()
				&& let Some(open) = self.library.spaces.get_mut(&batch.space)
			{
				open.stored.abandon_batch();
			}
		} else {
			let _ = self.end_batch();
			self.save_received_checks();
		}
	}
}

/// How much of its disk a replica keeps free when it receives blobs: 1 GiB.
pub(crate) const MIN_FREE_BYTES: u64 = 1 << 30;

/// Whether a group's log gives the replica whose key is `own` the space `space`: whether a
/// counting statement there gives it an entry for that space, on its record in the group.
fn gave(
	log: &nibkeep_log::Log<nibkeep_store::StateFile, nibkeep_store::LogFile>,
	keys: &nibkeep_log::SpaceKeys,
	own: &VerifyingKey,
	group: SpaceId,
	space: SpaceId,
) -> Result<bool, NodeError> {
	let record = nibkeep_governance::replica_record(group, own);
	Ok(log.governing_statements(keys)?.iter().any(|governing| {
		governing.statement.entity == record
			&& governing
				.counting_text(&nibkeep_model::fields::SPACE_ENTRY)
				.and_then(nibkeep_governance::SpaceEntry::from_text)
				.is_some_and(|entry| entry.space == space)
	}))
}

/// Whether a disk with `free` bytes free has room for a blob of `size` bytes, and
/// [`MIN_FREE_BYTES`] besides.
fn leaves_room(free: u64, size: u64) -> bool {
	free.checked_sub(size)
		.is_some_and(|left| left >= MIN_FREE_BYTES)
}

/// Whether an error of receiving is the log's refusal of what came, which drops it, and not
/// a failure of the storage.
fn refused(error: &StoreError) -> bool {
	matches!(error, StoreError::Log(error) if *error != LogError::Storage)
}

impl Holder for Syncing<'_> {
	type Error = NodeError;

	fn verifying_key(&self) -> VerifyingKey {
		self.library.replica.signing.verifying_key()
	}

	fn sign(&self, context: &'static str, message: &[u8]) -> Signature {
		self.library.replica.signing.sign(context, message)
	}

	fn spaces_with(&self, peer: &VerifyingKey) -> Result<Vec<SpaceId>, NodeError> {
		if *peer == self.library.replica.signing.verifying_key() {
			return Ok(Vec::new());
		}
		// The user's own replicas, as the personal space lists them, but AI agents'.
		let own_replica = {
			let personal = self.library.space(self.library.info.personal)?;
			let log = personal.stored.log();
			log.is_governed(&personal.keys)?
				&& log.is_listed(peer, &personal.keys)?
				&& !log.is_revoked(peer, &personal.keys)?
				&& !log.is_ai_agent(peer, &personal.keys)?
		};
		let mut spaces = Vec::new();
		for (&id, space) in &self.library.spaces {
			// Who may sync is decided by key, as the space's counting governing changes say.
			let log = space.stored.log();
			let governed = log.is_governed(&space.keys)?;
			// A replica that another of its member's revoked, but which would stay if it
			// revoked that one back, is heard: it is sent nothing, and what it made without
			// knowing of its revocation is taken, set aside, so that two replicas that revoke
			// each other meet wherever either revocation is, and the one listed first stays.
			let listed = governed
				&& log.is_listed(peer, &space.keys)?
				&& (!log.is_revoked(peer, &space.keys)?
					|| log.may_revoke_back(peer, &space.keys)?);
			// A new replica trusts its sponsor for a space it has nothing of yet, and a replica
			// its user's other replicas for a space that the personal space gave it.
			let trusted = !governed
				&& (self.trusted == Some(*peer)
					|| (own_replica && id != self.library.info.personal));
			// A new device introduced itself here by a recovery code that stands.
			let introduced = self
				.introduced
				.as_ref()
				.is_some_and(|(key, spaces)| key == peer && spaces.contains(&id));
			if listed || trusted || introduced || self.through_groups(id, governed, peer)? {
				spaces.push(id);
			}
		}
		Ok(spaces)
	}

	fn introductions(&self, peer: &[u8; 32]) -> Result<Vec<RecoveryListing>, NodeError> {
		// Only a replica that listed itself by a code has any.
		let own = self.library.replica.signing.verifying_key();
		let mut listings: BTreeMap<SpaceId, RecoveryListing> = BTreeMap::new();
		for (&id, open) in &self.library.spaces {
			if let Some(listing) = open.stored.log().listings_by_code(&own, &open.keys)?.pop() {
				listings.insert(id, listing);
			}
		}
		if listings.is_empty() {
			return Ok(Vec::new());
		}
		// It shows them to the replica it connected to alone, by the endpoint ID that its
		// record states, for the spaces that list that replica, which holds those IDs
		// already; the personal space first, should there be more than a hello carries.
		let Some(key) = self
			.library
			.peers()?
			.into_iter()
			.find(|replica| replica.endpoint == Some(*peer))
			.and_then(|replica| replica.signing)
		else {
			return Ok(Vec::new());
		};
		let shared = self.spaces_with(&key)?;
		let personal = self.library.info.personal;
		let mut shown: Vec<RecoveryListing> = listings
			.into_iter()
			.filter(|(space, _)| shared.contains(space))
			.map(|(_, listing)| listing)
			.collect();
		shown.sort_by_key(|listing| listing.space != personal);
		Ok(shown)
	}

	fn introduced(
		&mut self,
		peer: &VerifyingKey,
		listings: &[RecoveryListing],
	) -> Result<(), NodeError> {
		// Listings of the peer's own key, by space, each space once.
		let mut by_space: BTreeMap<SpaceId, Vec<&RecoveryListing>> = BTreeMap::new();
		for listing in listings.iter().filter(|listing| listing.key == *peer) {
			by_space.entry(listing.space).or_default().push(listing);
		}
		// In a governed space or group held here that neither lists the peer, which syncs
		// with it anyway, nor revoked it: a listing signed by a code that counts there, one
		// that stands for a member with a role, so that strangers, and members who left,
		// still fill nobody's log. The cheap checks first, and the roster once.
		let mut spaces = BTreeSet::new();
		for (space, listings) in by_space {
			let Some(open) = self.library.spaces.get(&space) else {
				continue;
			};
			let log = open.stored.log();
			if !log.is_governed(&open.keys)?
				|| log.is_listed(peer, &open.keys)?
				|| log.is_revoked(peer, &open.keys)?
			{
				continue;
			}
			let roster = log.roster(&open.keys)?;
			let codes: BTreeSet<EntityId> = self
				.library
				.code_recipients(space, &roster, &[])?
				.iter()
				.map(|code| code.record)
				.collect();
			if listings
				.iter()
				.any(|listing| codes.contains(&listing.code_record()) && listing.verifies())
			{
				spaces.insert(space);
			}
		}
		self.introduced = Some((*peer, spaces));
		Ok(())
	}

	fn prepare(&mut self, space: SpaceId) -> Result<(), NodeError> {
		self.end_batch()?;
		// A replica gives an account of a space only once it counts there. Accounts only
		// count and show copies, so one that cannot be brought up to date now, as while
		// another program holds the log file, waits for the next sync, and this one goes on.
		let own = self.library.replica.signing.verifying_key();
		if self.library.counts_in(space, &own)? {
			let wall = (self.now)();
			let _ = self.library.refresh_account(space, wall, &mut self.rng);
		}
		Ok(())
	}

	fn holdings(&self, space: SpaceId) -> Result<Vec<Holding>, NodeError> {
		let stored = &self.library.space(space)?.stored;
		let log = stored.log();
		let mut holdings: Vec<Holding> = log
			.summaries()?
			.into_iter()
			.map(|summary| Holding::Change {
				id: summary.id,
				millis: summary.millis,
				depth: summary.depth,
				complete: summary.complete,
			})
			.collect();
		holdings.extend(
			log.wrapped_keys()
				.map(|(statement, key)| Holding::RecordKey {
					statement,
					key: key.clone(),
				}),
		);
		holdings.extend(
			log.wrapped_blob_keys()
				.map(|(sealed, key)| Holding::BlobKey {
					sealed: *sealed,
					key: key.clone(),
				}),
		);
		holdings.extend(
			self.library
				.account_holdings(space)?
				.into_iter()
				.map(|(id, issued)| Holding::Account { id, issued }),
		);
		Ok(holdings)
	}

	fn account(&self, space: SpaceId, id: &Hash) -> Result<Option<Vec<u8>>, NodeError> {
		self.library.account_page(space, id)
	}

	fn receive_account(&mut self, space: SpaceId, page: Vec<u8>) -> Result<bool, NodeError> {
		// Not where the disk would have too little room left after it, as for blobs; kept
		// with the others that came, once the batch of changes ends.
		if !self.has_room(u64::try_from(page.len()).unwrap_or(u64::MAX))? {
			return Ok(false);
		}
		let Some(checked) = self.library.check_account(space, &page)? else {
			return Ok(false);
		};
		self.pages.push((space, checked));
		Ok(true)
	}

	fn sends(
		&self,
		space: SpaceId,
		peer: &VerifyingKey,
		holding: &Holding,
	) -> Result<bool, NodeError> {
		// A replica whose member left or was removed, or an AI agent's where the space allows
		// none, gets the changes that ended it and what they build on, and nothing after: no
		// keys or files either, which it holds already or does without. A revoked one gets
		// nothing.
		match (self.ended(space, peer)?, holding) {
			(None, _) => Ok(true),
			(Some(ended), Holding::Change { id, .. }) => Ok(ended.reach.contains(id)),
			(Some(_), _) => Ok(false),
		}
	}

	fn wanted_blobs(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		asked: &BTreeSet<Hash>,
		most: usize,
	) -> Result<Vec<Hash>, NodeError> {
		// Nor does a revoked replica give any.
		if self.ended(space, peer)?.is_some_and(|ended| ended.revoked) {
			return Ok(Vec::new());
		}
		let listed = self.library.listed_blobs(space, peer)?;
		let lists = |sealed: &Hash| listed.as_ref().is_none_or(|listed| listed.contains(sealed));
		self.wanted_among(space, &lists, asked, most)
	}

	fn sends_blobs(&self, space: SpaceId, peer: &VerifyingKey) -> Result<bool, NodeError> {
		// A replica that the space ended gets no files, which it holds already or does
		// without.
		Ok(self.ended(space, peer)?.is_none())
	}

	fn challenges(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
	) -> Result<Vec<Challenge>, NodeError> {
		// A revoked replica's copies count for nothing.
		if self.ended(space, peer)?.is_some_and(|ended| ended.revoked) {
			return Ok(Vec::new());
		}
		Ok(self
			.removals
			.as_deref_mut()
			.map_or_default(|removals| removals.challenges(space, peer)))
	}

	fn proved(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
		answers: &[Option<[u8; 32]>],
	) -> Result<(), NodeError> {
		if let Some(removals) = self.removals.as_deref_mut() {
			removals.proved(space, peer, challenges, answers);
		}
		Ok(())
	}

	fn prove(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
	) -> Result<Vec<Option<[u8; 32]>>, NodeError> {
		let mut answers = vec![None; challenges.len()];
		// A replica that the space ended removes nothing of it on this one's word.
		if !self.sends_blobs(space, peer)? {
			return Ok(answers);
		}
		// Plain challenges, of a content's own bytes, are answered from an unchanged original
		// of it on this device, which NibKeep never removes, so nothing is held for them;
		// never from a checkout, which NibKeep deletes and the person edits.
		if challenges.iter().any(|challenge| challenge.plain) {
			let originals: BTreeMap<Hash, String> = self
				.library
				.origin_places_here(space)?
				.into_iter()
				.filter(|place| place.untouched)
				.map(|place| (place.content, place.path))
				.collect();
			for (challenge, answer) in challenges.iter().zip(answers.iter_mut()) {
				if challenge.plain
					&& let Some(path) = originals.get(&challenge.sealed)
					&& let Ok(mut file) = fs::File::open(path)
				{
					*answer = answer_from_file(challenge, &mut file).ok();
				}
			}
		}
		let held: Vec<Hash> = {
			let stored = &self.library.space(space)?.stored;
			challenges
				.iter()
				.filter(|challenge| !challenge.plain)
				.map(|challenge| challenge.sealed)
				.filter(|sealed| {
					!stored.log().wrapped_blob_keys_of(sealed).is_empty()
						&& stored.holds_sealed(sealed)
				})
				.collect()
		};
		if held.is_empty() {
			return Ok(answers);
		}
		if self.records.is_none() {
			self.records = Some(self.library.sync_records(UnwrapErr(SysRng))?);
		}
		let records = self
			.records
			.as_ref()
			.unwrap_or_else(|| unreachable!("set above"));
		// Held before it answers, so that it removes none of them while the other replica
		// counts on them; none that it is removing itself.
		let holding = records.hold(&held, (self.now)())?;
		let stored = &self.library.space(space)?.stored;
		for (challenge, answer) in challenges.iter().zip(answers.iter_mut()) {
			if challenge.plain || !holding.contains(&challenge.sealed) {
				continue;
			}
			if let Some((_, mut file)) = stored.open_sealed_blob(&challenge.sealed)? {
				*answer = answer_from_file(challenge, &mut file).ok();
			}
		}
		Ok(answers)
	}

	fn parcel(&self, space: SpaceId, id: ChangeId) -> Result<Option<Parcel>, NodeError> {
		Ok(self.library.space(space)?.stored.log().parcel(id)?)
	}

	fn blob(
		&self,
		space: SpaceId,
		sealed: &Hash,
	) -> Result<Option<(u64, nibkeep_sync::BlobBytes<'_>)>, NodeError> {
		Ok(self
			.library
			.space(space)?
			.stored
			.open_sealed_blob(sealed)?
			.map(|(size, file)| {
				let file: Box<dyn Read + '_> = Box::new(file);
				(size, file)
			}))
	}

	fn receive_change(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		mut parcel: Parcel,
	) -> Result<bool, NodeError> {
		let wall_millis = (self.now)();
		// Of what a replica that the space ended made itself, only what it made without
		// knowing of that, built on changes held here, which a revocation sets aside unless
		// one of its own of the replica that revoked it comes; what it passes on of others'
		// is taken as from anyone.
		if parcel.change.author() == peer
			&& let Some(ended) = self.ended(space, peer)?
		{
			let log = self.library.space(space)?.stored.log();
			for parent in parcel.change.parents() {
				// Accepted, not only waiting, so that what it builds on is known.
				if log.change(*parent)?.is_none() {
					return Ok(false);
				}
				for &ending in &ended.ending {
					if log.is_ancestor_of(ending, *parent)? {
						return Ok(false);
					}
				}
			}
		}
		let governs = parcel.change.kind() == nibkeep_log::Kind::Governing;
		// An ordinary change's payload, too, is refused if the disk would have too little
		// room left after it. Its header is taken, so that the changes that build on it,
		// such as a revocation, are not held up; the payload comes at a later sync.
		if parcel.change.kind() == nibkeep_log::Kind::Ordinary
			&& let Some(payload) = &parcel.payload
		{
			let size = u64::try_from(payload.byte_len()).unwrap_or(u64::MAX);
			if !self.has_room(size)? {
				parcel.payload = None;
				// A payload held already is no loss, as when a copy offers it again.
				let log = self.library.space(space)?.stored.log();
				if log.sealed_payload(parcel.id())?.is_none() {
					self.refused += 1;
				}
			}
		}
		self.batch(space)?;
		let open = self.space(space)?;
		let mut alone = false;
		let mut received = open.stored.receive(&parcel, &open.keys, wall_millis);
		// A change that governs, or that waiting changes wait for, which may govern, is
		// received alone, as one made here that governs is made outside a batch.
		if matches!(received, Err(StoreError::InBatch)) {
			self.end_batch()?;
			let open = self.space(space)?;
			received = open.stored.receive(&parcel, &open.keys, wall_millis);
			alone = true;
		}
		let received = match received {
			Ok(Received::Duplicate) => false,
			Ok(_) => true,
			Err(error) if refused(&error) => false,
			Err(error) => return Err(error.into()),
		};
		// Who the space ended may differ once a governing change came.
		if (governs || alone) && received {
			self.ended.borrow_mut().remove(&space);
		}
		if received {
			self.plan = None;
		}
		self.received_in_batch()?;
		Ok(received)
	}

	fn receive_record_key(
		&mut self,
		space: SpaceId,
		statement: StatementId,
		key: WrappedKey,
	) -> Result<bool, NodeError> {
		self.batch(space)?;
		let open = self.space(space)?;
		let before = open.stored.log().wrapped_keys_of(statement).len();
		let kept = match open.stored.receive_wrapped_key(statement, key, &open.keys) {
			Err(error) if !refused(&error) => return Err(error.into()),
			_ => open.stored.log().wrapped_keys_of(statement).len() > before,
		};
		self.received_in_batch()?;
		Ok(kept)
	}

	fn receive_blob_key(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		key: WrappedBlobKey,
	) -> Result<bool, NodeError> {
		// Saved at once, under the replica's blob lock, which a batch would hold too long.
		self.end_batch()?;
		let open = self.space(space)?;
		let before = open.stored.log().wrapped_blob_keys_of(&sealed).to_vec();
		let kept = match open.stored.receive_blob_key(sealed, key, &open.keys) {
			Err(error) if !refused(&error) => return Err(error.into()),
			_ => open.stored.log().wrapped_blob_keys_of(&sealed) != before.as_slice(),
		};
		if kept {
			self.plan = None;
		}
		Ok(kept)
	}

	fn receive_blob(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		size: u64,
		bytes: &mut dyn Read,
	) -> Result<bool, NodeError> {
		// A blob takes long to come, and is written apart from the log.
		self.end_batch()?;
		// Keys come before blobs: a blob without one is no blob of the space, and whatever
		// its size, none of it is written.
		if self
			.space(space)?
			.stored
			.log()
			.wrapped_blob_keys_of(&sealed)
			.is_empty()
		{
			return Ok(false);
		}
		// Nor is one that would leave the disk too full, so that another replica cannot
		// fill it; the sync goes on without it.
		if !self.has_room(size)? {
			// A blob held already is no loss, as when a copy offers it again.
			let stored = &self.space(space)?.stored;
			if stored.open_sealed_blob(&sealed)?.is_none() {
				self.refused += 1;
			}
			return Ok(false);
		}
		let now = (self.now)();
		let stored = &mut self.space(space)?.stored;
		let kept = stored.receive_sealed_blob(sealed, size, bytes)?;
		if kept {
			self.received_checks.push((space, sealed, now));
		}
		Ok(kept)
	}

	fn finish_space(&mut self, space: SpaceId) -> Result<(), NodeError> {
		self.end_batch()?;
		self.save_received_checks();
		// What came may define types, or change the lists of languages.
		self.library.read_again(space)
	}

	fn pause(&mut self) -> Result<(), NodeError> {
		self.end_batch()
	}
}

/// The library as pairing changes it: what grants a new replica the space keys, and records
/// it once it has joined.
pub struct Sponsor<'a> {
	library: &'a mut Library,
	now: &'a dyn Fn() -> u64,
	/// The name of the AI agent whose replica it adds, if it adds one.
	ai_agent: Option<String>,
	/// The spaces besides the personal one chosen for that AI agent.
	spaces: Vec<SpaceId>,
	/// What the replica granted last asked with, where it connected from, and what it was
	/// granted, for listing an AI agent's replica in the spaces chosen for it.
	granted: Option<(JoinRequest, [u8; 32], Grant)>,
	/// Whether it gives the secrets to a new replica that asks for them, as the person chose.
	give_secrets: bool,
	/// What records the replica granted last.
	pending: Option<Vec<Operation>>,
	/// What the replica granted last asked for, and got.
	asked_for: AskedFor,
	/// Whether a code matched: a joiner's request came with the tag of the code's key.
	matched: bool,
	/// What the person chose for a new device of theirs: a computer or a phone, and its
	/// spaces.
	device: NewDevice,
}

/// What a new replica asked for, besides the library, as it paired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AskedFor {
	/// Nothing, or the secrets from a replica that holds none.
	#[default]
	Nothing,
	/// The secrets, which it was not given: the replica that added it gives them later.
	Secrets,
	/// The secrets, which it was given.
	SecretsGiven,
}

impl<'a> Sponsor<'a> {
	/// Pairing of `library`, with the wall clock `now`.
	pub fn new(library: &'a mut Library, now: &'a dyn Fn() -> u64) -> Self {
		Self {
			library,
			now,
			ai_agent: None,
			spaces: Vec::new(),
			granted: None,
			give_secrets: false,
			pending: None,
			asked_for: AskedFor::Nothing,
			matched: false,
			device: NewDevice::computer(),
		}
	}

	/// The same, pairing a device of the person's as they chose: a computer, which takes
	/// every space of this device's, or the spaces chosen, and those given to it later; or a
	/// phone, which takes the personal space alone, or the spaces chosen, and asks before it
	/// takes a space given to it later
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[must_use]
	pub fn for_device(mut self, device: NewDevice) -> Self {
		self.device = device;
		self
	}

	/// The same, giving the secrets, which are unlocked, to a new replica that asks for them:
	/// the person chose to.
	#[must_use]
	pub fn giving_secrets(mut self) -> Self {
		self.give_secrets = true;
		self
	}

	/// What the replica granted last asked for, and got.
	#[must_use]
	pub fn asked_for(&self) -> AskedFor {
		self.asked_for
	}

	/// Whether the code matched: a joiner was granted, or would have been, had granting not
	/// failed. A pairing that failed before then leaves the code for another try.
	#[must_use]
	pub const fn matched(&self) -> bool {
		self.matched
	}

	/// Pairing of `library` with an AI agent's replica, named after the AI agent, as
	/// [`Sponsor::new`] pairs a replica of the user's.
	pub fn ai_agent(library: &'a mut Library, name: &str, now: &'a dyn Fn() -> u64) -> Self {
		Self::ai_agent_in(library, (name, &[]), now)
	}

	/// The same, listing the AI agent's replica in the spaces besides the personal one that
	/// the person chose for it too, which the AI agent's replica signs its listings in.
	pub fn ai_agent_in(
		library: &'a mut Library,
		(name, spaces): (&str, &[SpaceId]),
		now: &'a dyn Fn() -> u64,
	) -> Self {
		Self {
			ai_agent: Some(name.to_owned()),
			spaces: spaces.to_vec(),
			..Self::new(library, now)
		}
	}
}

impl Sponsoring for Sponsor<'_> {
	type Error = NodeError;

	fn grant<R: CryptoRng>(
		&mut self,
		request: &JoinRequest,
		endpoint: &[u8; 32],
		rng: &mut R,
	) -> Result<Grant, NodeError> {
		// Pairing asks for a grant only once the code matched.
		self.matched = true;
		let (mut grant, operations, asked_for) = self.library.prepare_replica(
			request,
			(endpoint, self.ai_agent.as_deref(), self.give_secrets),
			(self.now)(),
			rng,
		)?;
		// The AI agent's replica signs its listing in each space chosen, as the user's
		// member there.
		self.library.check_ai_spaces(&self.spaces)?;
		let mut chosen = Vec::new();
		for &space in &self.spaces {
			let member = self.library.member_in(space)?.ok_or(NodeError::NotListed)?;
			chosen.push((space, member));
		}
		chosen.sort();
		chosen.dedup();
		grant.chosen = chosen;
		grant.asks_first = self.ai_agent.is_none() && self.device.phone;
		self.pending = Some(operations);
		self.granted = Some((request.clone(), *endpoint, grant.clone()));
		self.asked_for = asked_for;
		Ok(grant)
	}

	fn record<R: CryptoRng>(&mut self, listings: &[String], rng: &mut R) -> Result<(), NodeError> {
		let Some(operations) = self.pending.take() else {
			return Ok(());
		};
		let Some((request, endpoint, grant)) = &self.granted else {
			return Err(NodeError::Damaged(
				"pairing records a replica that it granted nothing".to_owned(),
			));
		};
		let personal = self.library.info.personal;
		let wall_millis = (self.now)();
		// Its listing in the personal space, which its key signed for the record and the
		// owner that the grant named, as an AI agent's if it is one, with the encryption key it
		// asked with, goes with its records there: no other lists it.
		let Some((own, chosen)) = listings.split_first() else {
			return Err(NodeError::Damaged(
				"the new replica sent no listing of its own".to_owned(),
			));
		};
		let encryption = nibkeep_governance::encryption_hash(&request.encryption.to_bytes());
		Listing::from_text(own)
			.filter(|listing| {
				listing.space == personal
					&& listing.record == grant.replica
					&& listing.owner == grant.user
					&& listing.ai_agent == grant.ai_agent
					&& listing.key == request.signing
					&& listing.encryption == encryption
					&& listing.verifies()
			})
			.ok_or_else(|| {
				NodeError::Damaged("the new replica's listing is not the one asked for".to_owned())
			})?;
		let mut operations = operations;
		operations.push(Operation::assert(Statement::new(
			StatementId::random(rng),
			grant.replica,
			fields::LISTING,
			Value::Text(own.clone()),
		)));
		// An AI agent's listings in the spaces chosen for it are checked before anything is
		// committed; the personal space's entries go with its records there.
		let mut listed = Vec::new();
		if let Some(name) = &self.ai_agent
			&& !grant.chosen.is_empty()
		{
			let prepared =
				self.library
					.ai_agent_listings((request, endpoint, grant, name), chosen, rng)?;
			operations.extend(prepared.entries);
			listed = prepared.spaces;
		}
		// A device of the person's gets the spaces chosen for it, with its records: it takes
		// them after its first sync, whatever it says of the spaces given to it later.
		if self.ai_agent.is_none() {
			operations.extend(self.library.entries_for_new(
				(grant.replica, &request.encryption),
				&self.device,
				rng,
			)?);
		}
		self.library
			.commit(personal, operations, wall_millis, rng)?;
		self.library.list_ai_agent(listed, wall_millis, rng)
	}
}

/// A new replica that is joining a library: its keys, and the names of its device and app,
/// before the sponsor grants it the space keys.
pub struct Joining<'a> {
	signing: SigningKey,
	decryption: DecryptionKey,
	device: String,
	app: String,
	/// The key it asks for the secrets with, if it does, and how its secrets compartment is
	/// protected.
	secrets: Option<(DecryptionKey, NewProtection<'a>)>,
	/// That compartment, once sealed, and its key.
	sealed: Option<(SealedSecrets, CompartmentKey)>,
}

impl<'a> Joining<'a> {
	/// A new replica on a device, kept by an app, with new keys.
	///
	/// # Errors
	///
	/// Returns [`NodeError::EmptyName`] if a name is empty.
	pub fn new(device: &str, app: &str, rng: &mut impl CryptoRng) -> Result<Self, NodeError> {
		if device.is_empty() || app.is_empty() {
			return Err(NodeError::EmptyName);
		}
		// The sponsor refuses longer names, which would spend the code.
		if device.len().max(app.len()) > MAX_NAME_LEN {
			return Err(NodeError::LongName(MAX_NAME_LEN));
		}
		Ok(Self {
			signing: SigningKey::generate(rng),
			decryption: DecryptionKey::generate(rng),
			device: device.to_owned(),
			app: app.to_owned(),
			secrets: None,
			sealed: None,
		})
	}

	/// Asks the sponsor for the secrets too, with a new secrets key, which the new library
	/// keeps in a secrets compartment protected as `protection` says, never by the library's
	/// passphrase: the sponsor gives them, if it holds them and the person chose to there.
	/// After its first sync, the new replica states that it asks the sponsor, with that key,
	/// and takes what was given; if the sponsor held none, the compartment is removed again.
	#[must_use]
	pub fn asking_for_secrets(
		mut self,
		protection: NewProtection<'a>,
		rng: &mut impl CryptoRng,
	) -> Self {
		self.secrets = Some((DecryptionKey::generate(rng), protection));
		self
	}

	/// Whether it asks for the secrets.
	pub(crate) fn asks_for_secrets(&self) -> bool {
		self.secrets.is_some()
	}

	/// Seals the compartment that receives the secrets, if it asks for them, before the
	/// sponsor is found: the person may turn a keystore's prompt down, which then spends no
	/// code, or take longer than the sponsor waits.
	///
	/// # Errors
	///
	/// Returns an error if the passphrase cannot be used or the keystore fails.
	pub(crate) fn seal_secrets(&mut self, rng: &mut impl CryptoRng) -> Result<(), NodeError> {
		if self.sealed.is_some() {
			return Ok(());
		}
		let Some((decryption, protection)) = &self.secrets else {
			return Ok(());
		};
		let keys = SecretsKeys {
			spaces: SpaceKeyMap::new(),
			decryption: decryption.clone(),
		};
		self.sealed = Some(SealedSecrets::seal(&keys, protection, rng)?);
		Ok(())
	}

	/// Refuses the library's own passphrase for the secrets, before the code is spent.
	pub(crate) fn check_secrets_protection(
		&self,
		protection: &NewProtection<'_>,
	) -> Result<(), NodeError> {
		if let (
			Some((_, NewProtection::Passphrase(secrets, _))),
			NewProtection::Passphrase(library, _),
		) = (&self.secrets, protection)
			&& secrets == library
		{
			return Err(NodeError::LibraryPassphrase);
		}
		Ok(())
	}

	/// What it asks the sponsor for.
	#[must_use]
	pub fn request(&self) -> JoinRequest {
		JoinRequest {
			signing: self.signing.verifying_key(),
			encryption: self.decryption.encryption_key(),
			device: self.device.clone(),
			app: self.app.clone(),
			secrets: self
				.secrets
				.as_ref()
				.map(|(decryption, _)| decryption.encryption_key()),
		}
	}

	/// The 32-byte secret of its endpoint key.
	pub(crate) fn endpoint_secret(&self) -> Key {
		endpoint_secret(&self.signing)
	}

	/// Its keys and the names of its device and app, for a replica that lists itself, as a
	/// device that a recovery code brings back does; it asks nobody for the secrets, so a
	/// request for them is dropped.
	pub(crate) fn into_parts(self) -> (SigningKey, DecryptionKey, String, String) {
		(self.signing, self.decryption, self.device, self.app)
	}

	/// Starts the new replica's endpoint, as [`Library::network`] does.
	///
	/// # Errors
	///
	/// Returns an error if networking cannot start.
	pub fn network(
		&self,
		options: nibkeep_net::Options,
	) -> Result<nibkeep_net::Network, NodeError> {
		crate::network::start_network(&self.endpoint_secret(), options)
	}

	/// Makes the library in a directory that holds none, from what the sponsor granted:
	/// the new replica's keys, protected as `protection` says, with the space keys, and the
	/// library file. Its spaces are empty until it syncs. If making it fails, what was
	/// written is removed, keystore keys included.
	///
	/// # Errors
	///
	/// Returns an error if the directory holds a library already, the grant's keys do not
	/// open or lack the personal space, or the files cannot be written.
	pub fn make(
		mut self,
		directory: &Path,
		grant: &Grant,
		protection: &NewProtection<'_>,
		rng: &mut impl CryptoRng,
	) -> Result<Library, NodeError> {
		// Its records are derived from the personal space and its key, as its listing says.
		let own = self.signing.verifying_key();
		if grant.replica != nibkeep_governance::replica_record(grant.space, &own)
			|| grant.device != nibkeep_governance::device_record(grant.space, &own)
		{
			return Err(NodeError::Damaged(
				"the grant names records that are not this replica's".to_owned(),
			));
		}
		// Sealed already, unless it was made without [`join`](crate::join).
		self.seal_secrets(rng)?;
		let sealed = self.sealed.take();
		let spaces =
			nibkeep_governance::open_space_keys(&self.decryption, grant.replica, &grant.keys)
				.map_err(|_| {
					NodeError::Damaged("the space keys of the grant do not open".to_owned())
				})?;
		if !spaces.contains_key(&grant.space) {
			return Err(NodeError::Damaged(
				"the grant lacks the keys of the personal space".to_owned(),
			));
		}
		let folder = new_replica_folder(directory, super::wall_clock(), rng)?;
		let in_use = super::in_use::InUse::take(&folder)?;
		let directory = folder.as_path();
		let keys_path = directory.join(KEYS);
		let replica = ReplicaKeys {
			signing: self.signing,
			decryption: self.decryption,
			storage: Key::generate(rng),
			spaces,
		};
		let info = Info {
			user: grant.user,
			device: grant.device,
			replica: grant.replica,
			personal: grant.space,
		};
		let (mut key_file, compartment) = match KeyFile::create(&replica, protection, rng) {
			Ok(created) => created,
			Err(error) => {
				drop(in_use);
				let _ = fs::remove_dir_all(directory);
				return Err(error.into());
			}
		};
		// The compartment that receives the secrets, if it asks for them, is written with the
		// rest of the library, so that the key it asked with is never only in memory.
		let secrets_compartment = sealed.map(|(sealed, key)| {
			key_file.put_secrets(sealed);
			key
		});
		let made = (|| {
			key_file.write(&keys_path)?;
			let code_types = code_types();
			let mut spaces = std::collections::BTreeMap::new();
			for (&id, epochs) in &replica.spaces {
				let log = log_path(directory, id);
				spaces.insert(
					id,
					Space::open(&log, id, epochs, &replica.storage, &code_types)?,
				);
			}
			info.write(&directory.join(INFO))?;
			Ok::<_, NodeError>((code_types, spaces))
		})();
		match made {
			Ok((code_types, spaces)) => {
				let mut library = Library {
					directory: directory.to_owned(),
					key_file,
					compartment,
					secrets_compartment,
					replica,
					info,
					spaces,
					chosen: None,
					code_types,
					languages: nibkeep_model::Languages::default(),
					language_statements: std::collections::BTreeSet::new(),
					in_use,
					wall: std::cell::Cell::new(super::wall_clock()),
					accounts_refreshed: std::cell::RefCell::new(BTreeMap::new()),
					acting: None,
					thumbnailer: None,
				};
				library.refresh_languages()?;
				library.choose_writing_keys()?;
				Ok(library)
			}
			Err(error) => {
				drop(in_use);
				let _ = fs::remove_dir_all(directory);
				key_file.discard();
				Err(error)
			}
		}
	}
}

impl Library {
	/// Removes a library that a new replica made, keystore keys included, after its first
	/// sync failed: the pairing failed, and the sponsor deletes the replica's records.
	pub(crate) fn remove_unsynced(self) {
		let directory = self.directory.clone();
		let key_file = self.key_file.clone();
		drop(self);
		// The folder is this replica's own, made as it joined.
		let _ = fs::remove_dir_all(directory);
		key_file.discard();
	}
}

#[cfg(test)]
mod tests {
	use super::{MIN_FREE_BYTES, leaves_room};

	#[test]
	fn a_blob_is_taken_only_if_a_gibibyte_stays_free() {
		assert!(leaves_room(MIN_FREE_BYTES + 10, 10));
		assert!(!leaves_room(MIN_FREE_BYTES + 10, 11));
		assert!(!leaves_room(5, 10), "a blob larger than the free space");
		assert!(leaves_room(u64::MAX, u64::MAX - MIN_FREE_BYTES));
	}
}
