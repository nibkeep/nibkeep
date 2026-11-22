//! The log of a space: the changes a replica has, and the state they build.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, btree_map};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Key, SigningKey, VerifyingKey, key_hash};
use nibkeep_governance::rules::{Fact, Governance};
use nibkeep_governance::{Followed, Role};
use nibkeep_model::storage::{AsOf, Layers, Memory, Origin, Statements};
use nibkeep_model::{FieldName, Operation, State, Statement, fields};
use nibkeep_time::{Clock, Timestamp};

use crate::history::{Accepted, History, MemoryHistory, Status, Summary};
use crate::keys::Unwrapped;
use crate::payload::{open_sealed, seal};
use crate::{
	Change, ChangeId, Draft, Kind, LogError, Parcel, Payload, Purges, Restores, SealedPayload,
	SpaceKeys, WrappedBlobKey, WrappedChangeKey, WrappedKey,
};
pub use governance::Review;

mod governance;

/// How many changes may wait for their parents unless [`Log::limit_waiting`] says
/// otherwise.
const DEFAULT_WAITING_LIMIT: usize = 100_000;

/// How many different wrapped keys a replica keeps for one statement, while it cannot tell
/// which of them are good.
const WRAPPED_KEYS_PER_STATEMENT: usize = 4;

/// How many different wrapped keys a replica keeps for one sealed blob, while it cannot
/// tell which of them are good.
const WRAPPED_KEYS_PER_BLOB: usize = 4;

/// How many different wrapped keys a replica keeps for one change, while it cannot tell
/// which of them are good.
const WRAPPED_KEYS_PER_CHANGE: usize = 4;

/// The state a log builds: its ordinary statements, kept in `S`, with the secret statements
/// this replica has opened, which never reach the storage of the ordinary ones.
pub type SpaceState<S = Memory> = State<Layers<S>>;

/// A replica's copy of a space's log: the changes it has accepted, with their sealed
/// payloads and wrapped keys as far as it has them, the changes waiting for their parents,
/// the changes it rejected for good, the wrapped record keys of secret statements, the
/// wrapped keys of blobs, and the state that the accepted changes build.
///
/// A change is accepted on its header alone, and its operations are applied once its
/// payload and key are there too, in whatever order they arrive.
///
/// The log keeps the space's hybrid logical clock, so the caller passes the wall clock
/// time to every call that makes or receives a change.
///
/// The ordinary statements of the state are kept in a storage `S`, and the accepted
/// changes in a history `H`: in memory, or on disk, where reading and writing may fail. The
/// log then keeps the error, and refuses to make or take changes until it is built again,
/// as it no longer matches its storage.
#[derive(Debug)]
pub struct Log<S: Statements = Memory, H: History = MemoryHistory<<S as Statements>::Error>> {
	space: SpaceId,
	clock: Clock,
	/// The accepted changes.
	history: H,
	/// Changes waiting for some of their parents.
	waiting: BTreeMap<ChangeId, Held>,
	/// The decrypted payloads of the changes whose operations the last call applied, which
	/// the caller may take, so that it need not decrypt them again.
	applied: Vec<(ChangeId, Payload)>,
	/// For each change that waiting changes need and that is not accepted, the changes
	/// that need it.
	waiting_on: BTreeMap<ChangeId, BTreeSet<ChangeId>>,
	waiting_limit: usize,
	/// Changes that can never be accepted, with the reason.
	rejected: BTreeMap<ChangeId, LogError>,
	state: SpaceState<S>,
	/// Changes taken back whose operations the storage holds already.
	covered: BTreeSet<ChangeId>,
	/// Whether reading or writing the storage or the history failed, and the error, until
	/// it is taken.
	failed: Cell<bool>,
	storage_error: RefCell<Option<S::Error>>,
	/// The fields whose values are secret.
	secret_fields: BTreeSet<FieldName>,
	/// The wrapped record keys of secret statements: a few candidates for each, until one
	/// of them is known to be good.
	wrapped: BTreeMap<StatementId, Vec<WrappedKey>>,
	/// The wrapped keys of sealed blobs, by sealed hash: a few candidates for each, until
	/// one of them is known to be good.
	wrapped_blob_keys: BTreeMap<Hash, Vec<WrappedBlobKey>>,
	/// The blob keys this replica has opened: by sealed hash, the content hash and the key.
	blob_keys: BTreeMap<Hash, (Hash, Key)>,
	/// For each content hash, the sealed hashes of the blob keys opened for it.
	sealed_blobs: BTreeMap<Hash, BTreeSet<Hash>>,
	/// The sealed hashes of the opened blob keys that are secret blob keys, which the
	/// replica forgets when it locks its secrets.
	secret_blobs: BTreeSet<Hash>,
	/// The sealed hashes with a wrapped key in the form of a secret blob key, whose outer
	/// layer opened: those that unlocking the secrets may open, or open anew.
	secret_candidates: BTreeSet<Hash>,
	/// Whether a purge took away operations that the state holds, or a change that counted
	/// was set aside, so that the state is to be built again: in a fresh storage at once,
	/// where one costs nothing, or else by the caller, which [`Log::take_stale`] tells.
	stale: bool,
	/// What the governing changes that count say, once read: which keys are listed and
	/// which revoked. It is read from the governing changes and their statuses, and decided
	/// anew when a governing change can be read.
	governance: RefCell<Option<Governance>>,
	/// Whether the space's first change must govern it, as in every space of NibKeep, so that
	/// a change without parents that does not is rejected whenever it comes.
	expects_governed: bool,
	/// The changes that may be the space's first, if the replica was told which: only these
	/// are taken as its first change, and any other is refused, without being rejected;
	/// `None` while it was told nothing, when it takes any.
	expected_roots: Option<BTreeSet<ChangeId>>,
	/// What each governing change that the log has read says, or `None` if it does not have
	/// the form a governing change must: read once, as it never changes.
	facts: RefCell<BTreeMap<ChangeId, Option<governance::ReadGoverning>>>,
	/// In a space with members, the latest governing changes that each change builds on,
	/// found once for each, with the governing changes they were found among: kept while
	/// those are the same.
	frontiers: RefCell<governance::Frontiers>,
	/// What the purges that the last call acted on destroyed, for the caller to delete from
	/// its storage too.
	destroyed: Destroyed,
	/// Whether what purges destroy is kept, as it was, in [`Destroyed::kept`], for a
	/// replica that waits before purging.
	keeping: bool,
	/// The accepted changes that restore anything, once looked for: rare, and looked for
	/// only when a purge must know whether something it names was restored.
	restoring: RefCell<Option<BTreeSet<ChangeId>>>,
	/// The groups in the space that it follows, as the replica's copies of their logs say:
	/// their changes take effect at the space's anchors that name them.
	following: BTreeMap<SpaceId, Followed>,
}

/// What purges destroyed: the record keys of secret statements, the keys of sealed blobs,
/// and whether changes lost their keys and payloads; with what a replica that waits keeps of
/// it, and what changes that restore brought back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Destroyed {
	/// The secret statements whose record keys were destroyed.
	pub statements: Vec<StatementId>,
	/// The sealed hashes of the blobs whose keys were destroyed.
	pub blobs: Vec<Hash>,
	/// Whether changes lost their keys and payloads.
	pub changes: bool,
	/// What the purges destroyed, as it was, by the purge that destroyed each part, if the
	/// log keeps it ([`Log::keep_destroyed`]).
	pub kept: Vec<(ChangeId, Kept)>,
	/// What is no longer purged, as a change that restores it came to count: the changes
	/// that wait for their payloads and keys again, and the statements and blobs that wait
	/// for their wrapped keys.
	pub restored: Restores,
}

impl Destroyed {
	/// Whether nothing was destroyed.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.statements.is_empty() && self.blobs.is_empty() && !self.changes
	}
}

/// Which purge keeps each purged item purged ([`Log::purged_by`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgedBy {
	/// The purged changes, each by its purge.
	pub changes: BTreeMap<ChangeId, ChangeId>,
	/// The secret statements whose record keys were destroyed, each by its purge.
	pub statements: BTreeMap<StatementId, ChangeId>,
	/// The sealed blobs whose keys were destroyed, each by its purge.
	pub blobs: BTreeMap<Hash, ChangeId>,
}

/// A part of what a purge destroyed, as it was, which a replica that waits keeps apart, to
/// give back if a change restores it ([technical design: copies that wait]).
///
/// [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
	/// A change, with the sealed payload and wrapped keys it had.
	Change(Box<Parcel>),
	/// The wrapped record keys of a secret statement.
	RecordKeys(StatementId, Vec<WrappedKey>),
	/// The wrapped keys of a sealed blob.
	BlobKeys(Hash, Vec<WrappedBlobKey>),
}

/// A change waiting for its parents, with the parts that came with it.
#[derive(Clone, Debug)]
struct Held {
	change: Change,
	payload: Option<SealedPayload>,
	/// A few candidates for the change's wrapped key, until one of them is known to be
	/// good, and then that one alone.
	keys: Vec<WrappedChangeKey>,
}

impl Held {
	fn parcel(&self) -> Parcel {
		Parcel {
			change: self.change.clone(),
			payload: self.payload.clone(),
			keys: self.keys.clone(),
		}
	}
}

/// Whether governing operations revoke a replica or set a role: what a replica would undo so,
/// by revoking back or taking a role away, may revoke it or take its role away itself, sent
/// without its payload, so such a change builds on no governing change it has not read.
fn defends(operations: &[Operation]) -> bool {
	operations.iter().any(|operation| {
		matches!(operation, Operation::Assert { statement, .. }
			if statement.field == fields::REVOKED || statement.field == fields::ROLE)
	})
}

/// Whether a change's payload fits its kind: an ordinary change holds no statement that
/// governs the space, and one that restores holds nothing at all; a governing change holds
/// only such statements, and states nothing again.
fn fits_its_kind(change: &Change, payload: &Payload) -> bool {
	match change.kind() {
		// A change that restores holds nothing else.
		Kind::Ordinary if !change.restores().is_empty() => *payload == Payload::default(),
		Kind::Ordinary => {
			!holds_governing(&payload.operations)
				&& payload
					.restated
					.values()
					.all(|restated| !holds_governing(&restated.operations))
		}
		Kind::Governing => holds_only_governing(&payload.operations) && payload.restated.is_empty(),
	}
}

/// The parts of an accepted change, as they travel.
fn parcel_of(accepted: &Accepted) -> Parcel {
	Parcel {
		change: accepted.change.clone(),
		payload: accepted.payload.clone(),
		keys: accepted.keys.clone(),
	}
}

/// Opens an accepted change's payload, if it is there with a good key, and keeps that key
/// alone. Candidate keys that are not the change's are dropped.
fn open_payload(accepted: &mut Accepted, keys: &SpaceKeys) -> Opened {
	let Some(payload) = &accepted.payload else {
		return Opened::NotYet;
	};
	if keys.get(accepted.change.epoch()).is_none() {
		return Opened::NotYet;
	}
	let change = &accepted.change;
	let mut opened = Opened::NotYet;
	accepted.keys.retain(|key| {
		if !matches!(opened, Opened::NotYet) {
			return false;
		}
		match change.open_checked(payload, key, keys) {
			Ok(payload) => {
				opened = Opened::Payload(payload);
				true
			}
			// Not the change's key.
			Err(LogError::OtherKey) => false,
			// The change's key, but a payload that adds nothing.
			Err(_) => {
				opened = Opened::Nothing;
				true
			}
		}
	});
	opened
}

/// Opens an accepted change's payload with the first of its candidate keys that opens it.
fn read_payload(accepted: &Accepted, keys: &SpaceKeys) -> Result<Option<Payload>, LogError> {
	let Some(payload) = &accepted.payload else {
		return Ok(None);
	};
	let mut last = None;
	for key in &accepted.keys {
		match accepted.change.open_checked(payload, key, keys) {
			Ok(opened) => return Ok(Some(opened)),
			Err(error) => last = Some(error),
		}
	}
	last.map_or(Ok(None), Err)
}

/// What opening a change's payload gives.
enum Opened {
	/// The payload, or a good key for it, is not there yet.
	NotYet,
	/// The payload opens with the change's key, but adds nothing: it does not have the
	/// form of a payload.
	Nothing,
	/// The payload.
	Payload(Payload),
}

/// A change whose signature is not valid, or, with why, that the history cannot read.
pub type Forged<E> = (ChangeId, Option<E>);

/// What became of a received change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
	/// The change was accepted, followed by the waiting changes it completed, in the order
	/// they were accepted. Their operations are applied once their payloads and keys are
	/// there.
	Accepted(Vec<ChangeId>),
	/// The change waits for some of its parents.
	Waiting,
	/// The log already has the change, accepted or waiting, and all that came with it.
	Duplicate,
	/// The log already has the change, accepted or waiting, and keeps the payload or a key
	/// that came with it, which it lacked.
	Completed,
}

/// Whether a change's signature is checked as the log takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signature {
	/// Checked: the change comes from elsewhere.
	Check,
	/// Trusted: the change comes from the replica's own storage, and was checked when it
	/// arrived.
	Trusted,
}

impl Log {
	/// Returns an empty log of a space, with the space's clock, and its state and history in
	/// memory.
	#[must_use]
	pub fn new(space: SpaceId, clock: Clock) -> Self {
		Self::with_storage(space, clock, Memory::default())
	}
}

impl<S: Statements> Log<S, MemoryHistory<S::Error>> {
	/// Returns an empty log of a space, with the space's clock, whose state is kept in
	/// `storage`, and its history in memory. The storage may hold the operations of changes
	/// already: those that [`Log::restore`] says it covers.
	#[must_use]
	pub fn with_storage(space: SpaceId, clock: Clock, storage: S) -> Self {
		Self::with_history(space, clock, storage, MemoryHistory::default())
	}
}

impl<S: Statements, H: History<Error = S::Error>> Log<S, H> {
	/// Returns a log of a space, with the space's clock, whose state is kept in `storage`
	/// and whose accepted changes are kept in `history`.
	#[must_use]
	pub fn with_history(space: SpaceId, clock: Clock, storage: S, history: H) -> Self {
		Self {
			space,
			clock,
			history,
			waiting: BTreeMap::new(),
			applied: Vec::new(),
			waiting_on: BTreeMap::new(),
			waiting_limit: DEFAULT_WAITING_LIMIT,
			rejected: BTreeMap::new(),
			state: State::in_storage(Layers::new(storage)),
			covered: BTreeSet::new(),
			failed: Cell::new(false),
			storage_error: RefCell::new(None),
			secret_fields: BTreeSet::new(),
			wrapped: BTreeMap::new(),
			wrapped_blob_keys: BTreeMap::new(),
			blob_keys: BTreeMap::new(),
			sealed_blobs: BTreeMap::new(),
			secret_blobs: BTreeSet::new(),
			secret_candidates: BTreeSet::new(),
			stale: false,
			governance: RefCell::new(None),
			expects_governed: false,
			expected_roots: None,
			facts: RefCell::new(BTreeMap::new()),
			frontiers: RefCell::default(),
			destroyed: Destroyed::default(),
			keeping: false,
			restoring: RefCell::new(None),
			following: BTreeMap::new(),
		}
	}

	/// Empties the log, as [`Log::with_history`] makes it, but keeps its storage, the
	/// fields that are secret and the limit on waiting changes, with the space's clock
	/// `clock`: to take back its changes again, after the storage was made again or a
	/// failure left the log out of step with it. Its history is emptied, and opened secrets
	/// are forgotten.
	pub fn reset(&mut self, clock: Clock) {
		// Every field is named, so that a field added later is decided on here.
		let Self {
			space: _,
			clock: own_clock,
			history,
			waiting,
			applied,
			waiting_on,
			waiting_limit: _,
			rejected,
			state,
			covered,
			failed,
			storage_error,
			secret_fields: _,
			wrapped,
			wrapped_blob_keys,
			blob_keys,
			sealed_blobs,
			secret_blobs,
			secret_candidates,
			stale,
			governance,
			expects_governed: _,
			expected_roots: _,
			facts,
			frontiers,
			destroyed,
			keeping: _,
			restoring,
			following: _,
		} = self;
		*own_clock = clock;
		waiting.clear();
		applied.clear();
		waiting_on.clear();
		rejected.clear();
		state.storage_mut().forget_all();
		covered.clear();
		failed.set(false);
		*storage_error.get_mut() = None;
		wrapped.clear();
		wrapped_blob_keys.clear();
		blob_keys.clear();
		sealed_blobs.clear();
		secret_blobs.clear();
		secret_candidates.clear();
		*stale = false;
		*governance.get_mut() = None;
		facts.get_mut().clear();
		*frontiers.get_mut() = governance::Frontiers::default();
		*destroyed = Destroyed::default();
		*restoring.get_mut() = None;
		if let Err(error) = history.clear() {
			self.fault(error);
		}
	}

	/// Why reading or writing the storage or the history failed, if it did and this was not
	/// asked before. The log keeps refusing changes, as it no longer matches its storage.
	pub fn storage_error(&mut self) -> Option<S::Error> {
		self.storage_error.get_mut().take()
	}

	/// The storage of the state, to begin and end transactions, for example.
	pub fn storage_mut(&mut self) -> &mut S {
		self.state.storage_mut().ordinary_mut()
	}

	/// The history, to begin and end transactions, for example.
	pub fn history_mut(&mut self) -> &mut H {
		&mut self.history
	}

	/// The history.
	#[must_use]
	pub const fn history(&self) -> &H {
		&self.history
	}

	/// Notes that the storage or the history failed: the log refuses changes from now on.
	fn fault(&self, error: S::Error) -> LogError {
		self.failed.set(true);
		*self.storage_error.borrow_mut() = Some(error);
		LogError::Storage
	}

	/// What the storage or the history answered, or [`LogError::Storage`] if it failed.
	fn read<T>(&self, result: Result<T, S::Error>) -> Result<T, LogError> {
		result.map_err(|error| self.fault(error))
	}

	/// Returns [`LogError::Storage`] if reading or writing the storage has failed.
	fn check_storage(&self) -> Result<(), LogError> {
		if self.failed.get() {
			Err(LogError::Storage)
		} else {
			Ok(())
		}
	}

	/// Has the space's first change govern it, as it does in every space of NibKeep: a
	/// change without parents that does not govern the space is rejected from now on,
	/// whatever came before it, so that every replica decides the same of it.
	pub fn expect_governed(&mut self) {
		self.expects_governed = true;
	}

	/// Takes as the space's first change only one of `roots`, as a replica told by another
	/// that it trusts which change founded the space does: any other change without parents
	/// is refused, without being rejected, so that the real one is taken when it comes. With
	/// no roots, it takes none until it is told one.
	pub fn expect_roots(&mut self, roots: impl IntoIterator<Item = ChangeId>) {
		self.expected_roots = Some(roots.into_iter().collect());
	}

	/// Takes as the space's first change one of `roots` too, besides those it was told of
	/// before, as [`Log::expect_roots`] says: when several sources that it trusts each name
	/// the change that founded the space. If it was told of none, it takes only one of
	/// `roots`; with no roots, it then takes none until it is told one, as when a source it
	/// trusts cannot be read.
	pub fn expect_roots_too(&mut self, roots: impl IntoIterator<Item = ChangeId>) {
		self.expected_roots.get_or_insert_default().extend(roots);
	}

	/// Limits how many changes may wait for their parents, which is 100,000 unless set.
	pub fn limit_waiting(&mut self, limit: usize) {
		self.waiting_limit = limit;
	}

	/// Keeps what purges destroy, as it was, in [`Destroyed::kept`], for a replica that waits
	/// before purging, which keeps it apart for a while: or no longer.
	pub fn keep_destroyed(&mut self, keeping: bool) {
		self.keeping = keeping;
	}

	/// Sets the fields whose values are secret, as the space's types mark them. New
	/// statements of these fields are sealed with record keys of their own.
	pub fn set_secret_fields(&mut self, fields: impl IntoIterator<Item = FieldName>) {
		self.secret_fields = fields.into_iter().collect();
	}

	/// The space.
	#[must_use]
	pub const fn space(&self) -> SpaceId {
		self.space
	}

	/// The space's clock, to save across restarts.
	#[must_use]
	pub const fn clock(&self) -> &Clock {
		&self.clock
	}

	/// Makes a change of `operations`, signed by `author`, its key wrapped with the newest
	/// of `keys`, and applies it. Statements of secret fields are sealed, their record keys
	/// wrapped with the newest secrets key, and what they supersede retracted in the
	/// ordinary layer. Statements of the fields that govern the space make a change of
	/// their own ([`Kind::Governing`]), wrapped with the space's first key, which every
	/// replica holds; one that revokes a replica or sets a role builds on no governing
	/// change whose payload this replica has not read. Returns the change with its payload
	/// and key, for other replicas.
	///
	/// # Errors
	///
	/// Returns an error, and leaves the log unchanged, if a secret value is to be written
	/// without a secrets key, if statements that govern the space come with others, if
	/// `keys` lack the space's first key for a governing change, or if the clock has run
	/// out of timestamps.
	pub fn commit(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		operations: Vec<Operation>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let mut governing = 0;
		for operation in &operations {
			if self.read(self.governs(operation))? {
				governing += 1;
			}
		}
		let kind = match governing {
			0 => Kind::Ordinary,
			all if all == operations.len() => Kind::Governing,
			_ => return Err(LogError::MixedGoverning),
		};
		let defends = kind == Kind::Governing && defends(&operations);
		let first_key;
		let keys = if kind == Kind::Governing {
			first_key = keys.get(0).map(|_| keys.writing_with(0));
			first_key.as_ref().ok_or(LogError::UnknownEpoch(0))?
		} else {
			keys
		};
		let mut payload = Payload::default();
		let mut secret = Vec::new();
		let is_secret = |statement: &Statement| {
			kind == Kind::Ordinary && self.secret_fields.contains(&statement.field)
		};
		// What takes away secret statements names no entity, as theirs are sealed with them:
		// one retraction of secret statements, which takes away no ordinary statement, for
		// those this replica knows to be of the entity named: opened, or sealed in this
		// change.
		let mut taking = TakingSecrets {
			sealing: operations
				.iter()
				.filter_map(|operation| match operation {
					Operation::Assert { statement, .. } if is_secret(statement) => {
						Some((statement.id, statement.entity))
					}
					_ => None,
				})
				.collect(),
			taken: BTreeSet::new(),
		};
		for operation in operations {
			match operation {
				Operation::Assert {
					statement,
					supersedes,
					..
				} if is_secret(&statement) => {
					let retraction =
						self.read(self.take_secret(statement.entity, supersedes, &mut taking))?;
					payload.operations.extend(retraction);
					secret.push(statement);
				}
				Operation::Retract {
					entity: Some(entity),
					statements,
				} if kind == Kind::Ordinary => {
					let retraction =
						self.read(self.take_secret(entity, statements, &mut taking))?;
					payload.operations.extend(retraction);
				}
				other => payload.operations.push(other),
			}
		}
		if !taking.taken.is_empty() {
			payload
				.operations
				.push(Operation::retract_secret(taking.taken));
		}
		let mut record_keys = BTreeMap::new();
		let mut wrapped = Vec::new();
		if !secret.is_empty() {
			let secrets_key = keys.newest_secrets().ok_or(LogError::SecretsLocked)?;
			for statement in secret {
				let record_key = Key::generate(rng);
				let id = statement.id;
				payload
					.sealed
					.insert(id, seal(self.space, &statement, &record_key, rng));
				record_keys.insert(id, key_hash(&record_key));
				let key = WrappedKey::wrap(self.space, id, &record_key, secrets_key, rng);
				wrapped.push((id, key));
			}
		}
		let heads = if defends {
			self.defending_parents()?
		} else {
			self.read(self.history.heads())?
		};
		let timestamp = self.clock.now(wall_millis)?;
		let mut draft = Draft::new(heads, timestamp, payload);
		draft.kind = kind;
		draft.record_keys = record_keys;
		let parcel = Change::create(self.space, keys, author, &draft, rng);
		for (id, key) in wrapped {
			self.wrapped.insert(id, vec![key]);
		}
		// The change is read as every other replica reads it.
		self.accept(held(parcel.clone()), keys)?;
		self.check_storage()?;
		Ok(parcel)
	}

	/// Of statements that an operation about `entity` takes away, moves the secret ones of
	/// that entity that this replica knows, opened or sealed in the same change, to those
	/// that the change's retraction of secret statements takes away, and returns the
	/// retraction of the others, which names the entity, if any are left: a statement of
	/// another entity stays there, where it is not superseded, as the operation said.
	fn take_secret(
		&self,
		entity: EntityId,
		statements: BTreeSet<StatementId>,
		taking: &mut TakingSecrets,
	) -> Result<Option<Operation>, S::Error> {
		let storage = self.state.storage();
		let mut others = BTreeSet::new();
		for id in statements {
			let secret = match taking.sealing.get(&id) {
				Some(&of) => of == entity,
				None => {
					storage.is_secret(id)?
						&& storage
							.get(id)?
							.is_some_and(|recorded| recorded.statement.entity == entity)
				}
			};
			if secret {
				taking.taken.insert(id);
			} else {
				others.insert(id);
			}
		}
		Ok((!others.is_empty()).then(|| Operation::retract(entity, others)))
	}

	/// Whether an operation governs the space ([`Kind::Governing`]): sets a field that
	/// governs it, or retracts or replaces a statement of such a field that the state holds.
	/// A change that governs the space holds nothing else: no retraction of secret
	/// statements, as no statement that governs is secret.
	///
	/// # Errors
	///
	/// Returns an error if the storage of the state cannot be read.
	pub fn governs(&self, operation: &Operation) -> Result<bool, S::Error> {
		let asserted = match operation {
			Operation::Assert { statement, .. } => Some(&statement.field),
			Operation::RetractSecret { .. } => return Ok(false),
			_ => None,
		};
		if asserted.is_some_and(fields::is_governing) {
			return Ok(true);
		}
		for &id in operation.superseded().into_iter().flatten() {
			if self
				.state
				.statement(id)?
				.is_some_and(|statement| fields::is_governing(&statement.field))
			{
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// Receives a change from another replica, with its payload and key if they came with
	/// it. Accepts it if its parents are all accepted, together with the waiting changes it
	/// completes; otherwise the change waits for its parents. An accepted change's operations
	/// are applied once its payload and key are there. A payload or key that is not the
	/// change's is dropped, and the change waits for the right one. The clock takes a
	/// change's timestamp into account only once the change is accepted.
	///
	/// # Errors
	///
	/// Returns an error, and leaves the log unchanged, if the change belongs to another
	/// space, its signature is not valid, it would govern the space and do something else
	/// too, its timestamp is too far ahead of the wall clock, or too many changes wait
	/// already. Returns an error, and remembers the change as rejected for good, if the
	/// change is not later than its parents, purges what it does not build on or what
	/// governs the space, or one of its parents was rejected.
	pub fn receive(
		&mut self,
		parcel: Parcel,
		keys: &SpaceKeys,
		wall_millis: u64,
	) -> Result<Received, LogError> {
		self.take(parcel, keys, wall_millis, Signature::Check, false)
	}

	/// Receives a change that this replica accepted or kept waiting before, from its own
	/// storage: like [`Log::receive`], without the checks that were made when the change
	/// first arrived and need not be made again: its signature, which is slow to check,
	/// and its timestamp against the wall clock. [`Log::forged`] checks the signatures.
	/// `covered` says whether the storage holds the change's operations already: if so,
	/// applying them leaves the storage as it is, and they are not among those that
	/// [`Log::take_applied`] gives.
	///
	/// # Errors
	///
	/// Returns the errors of [`Log::receive`], other than the signature's and the
	/// clock's.
	pub fn restore(
		&mut self,
		parcel: Parcel,
		keys: &SpaceKeys,
		covered: bool,
	) -> Result<Received, LogError> {
		self.take(parcel, keys, u64::MAX, Signature::Trusted, covered)
	}

	/// Receives the sealed payload of a change that the log holds, and applies the change's
	/// operations if it is accepted and its key is there too. Returns whether the log kept
	/// the payload, which it does unless it had it already.
	///
	/// # Errors
	///
	/// Returns an error if the log does not hold the change, or the payload is not the
	/// change's.
	pub fn receive_payload(
		&mut self,
		id: ChangeId,
		payload: SealedPayload,
		keys: &SpaceKeys,
	) -> Result<bool, LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let kept = self.add_payload(id, payload, keys)?;
		self.check_storage()?;
		Ok(kept)
	}

	/// Keeps the sealed payload of a change, as [`Log::receive_payload`] says, adding what
	/// it applies to what the last call applied.
	fn add_payload(
		&mut self,
		id: ChangeId,
		payload: SealedPayload,
		keys: &SpaceKeys,
	) -> Result<bool, LogError> {
		if let Some(error) = self.rejected.get(&id) {
			return Err(error.clone());
		}
		if let Some(held) = self.waiting.get_mut(&id) {
			if held.payload.is_some() {
				return Ok(false);
			}
			if payload.hash() != *held.change.payload_hash() {
				return Err(LogError::OtherPayload);
			}
			held.payload = Some(payload);
			return Ok(true);
		}
		if self.read(self.history.is_change_purged(id))? {
			return Ok(false);
		}
		let Some(mut accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Err(LogError::UnknownChange);
		};
		if accepted.payload.is_some() {
			return Ok(false);
		}
		if payload.hash() != *accepted.change.payload_hash() {
			return Err(LogError::OtherPayload);
		}
		accepted.payload = Some(payload);
		self.store(&accepted)?;
		self.complete(id, keys)?;
		Ok(true)
	}

	/// Receives a wrapped key of a change that the log holds, and applies the change's
	/// operations if it is accepted and its payload is there too. A replica that cannot open
	/// the key yet keeps a few. Returns whether the log kept the key.
	///
	/// # Errors
	///
	/// Returns an error if the log does not hold the change, or `keys` hold the space key
	/// the key needs and it is not the change's.
	pub fn receive_change_key(
		&mut self,
		id: ChangeId,
		key: WrappedChangeKey,
		keys: &SpaceKeys,
	) -> Result<bool, LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let kept = self.add_change_key(id, key, keys)?;
		self.check_storage()?;
		Ok(kept)
	}

	/// Keeps a wrapped key of a change, as [`Log::receive_change_key`] says, adding what it
	/// applies to what the last call applied.
	fn add_change_key(
		&mut self,
		id: ChangeId,
		key: WrappedChangeKey,
		keys: &SpaceKeys,
	) -> Result<bool, LogError> {
		if let Some(error) = self.rejected.get(&id) {
			return Err(error.clone());
		}
		if let Some(held) = self.waiting.get_mut(&id) {
			return keep_key(&held.change, &mut held.keys, key, keys);
		}
		if self.read(self.history.is_change_purged(id))? {
			return Ok(false);
		}
		let Some(mut accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Err(LogError::UnknownChange);
		};
		if !keep_key(&accepted.change, &mut accepted.keys, key, keys)? {
			return Ok(false);
		}
		self.store(&accepted)?;
		self.complete(id, keys)?;
		Ok(true)
	}

	/// Keeps what the log has of an accepted change in its history.
	fn store(&mut self, accepted: &Accepted) -> Result<(), LogError> {
		let updated = self.history.update(
			accepted.change.id(),
			accepted.payload.as_ref(),
			&accepted.keys,
			accepted.status,
		);
		self.read(updated)
	}

	/// Applies the operations of every accepted change that `keys` let the log read now,
	/// for example after the replica received an older space key.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if reading or writing the storage failed.
	pub fn open_changes(&mut self, keys: &SpaceKeys) -> Result<(), LogError> {
		let pending = self.read(self.history.pending())?;
		self.open_changes_of(pending, keys)
	}

	/// Applies the operations of these accepted changes, if they are pending and `keys` let
	/// the log read them now.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if reading or writing the storage failed.
	pub fn open_changes_of(
		&mut self,
		changes: impl IntoIterator<Item = ChangeId>,
		keys: &SpaceKeys,
	) -> Result<(), LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		for id in changes {
			self.complete(id, keys)?;
		}
		self.check_storage()
	}

	/// The changes, accepted or waiting, whose signatures are not valid, or that the
	/// history cannot read, with why, ordered by ID: none, unless the storage that the log
	/// read them from was damaged or tampered with. One that cannot be read does not keep
	/// the others from being checked.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot list its changes.
	pub fn forged(&self) -> Result<Vec<Forged<S::Error>>, S::Error> {
		let mut forged: Vec<Forged<S::Error>> = self
			.waiting
			.values()
			.map(|held| &held.change)
			.filter(|change| change.verify().is_err())
			.map(|change| (change.id(), None))
			.collect();
		for id in self.history.ids()? {
			match self.history.change(id) {
				Ok(Some(change)) if change.verify().is_err() => forged.push((id, None)),
				Ok(_) => {}
				Err(error) => forged.push((id, Some(error))),
			}
		}
		forged.sort_unstable_by_key(|(id, _)| *id);
		Ok(forged)
	}

	/// Receives a change, checking its signature or trusting it.
	fn take(
		&mut self,
		parcel: Parcel,
		keys: &SpaceKeys,
		wall_millis: u64,
		signature: Signature,
		covered: bool,
	) -> Result<Received, LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let id = parcel.id();
		if let Some(error) = self.rejected.get(&id) {
			return Err(error.clone());
		}
		if self.waiting.contains_key(&id) || self.read(self.history.contains(id))? {
			return self.take_parts(parcel, keys);
		}
		let change = &parcel.change;
		if change.space() != self.space {
			return Err(LogError::OtherSpace);
		}
		if signature == Signature::Check {
			change.verify()?;
		}
		if change.kind() == Kind::Governing
			&& (!change.sealed().is_empty() || change.purges_or_restores())
		{
			return Err(LogError::MixedGoverning);
		}
		// Every replica reads every governing change, wrapped with the space's first key,
		// which it holds: one wrapped with another key could be read by some replicas and not
		// by others, and decide otherwise on each. The header decides, so it is rejected for
		// good, with what builds on it; one that this replica kept before, from a draft of
		// the formats, stays.
		if signature == Signature::Check && change.kind() == Kind::Governing && change.epoch() != 0
		{
			self.rejected.insert(id, LogError::GoverningEpoch);
			return Err(LogError::GoverningEpoch);
		}
		self.clock.check(change.timestamp(), wall_millis)?;
		if change
			.parents()
			.iter()
			.any(|parent| self.rejected.contains_key(parent))
		{
			self.rejected.insert(id, LogError::RejectedParent);
			return Err(LogError::RejectedParent);
		}
		self.check_governed(id, &parcel, keys, signature)?;
		let mut held = held(parcel);
		// A payload or key that is not the change's is dropped.
		if held
			.payload
			.as_ref()
			.is_some_and(|payload| payload.hash() != *held.change.payload_hash())
		{
			held.payload = None;
		}
		if keys.get(held.change.epoch()).is_some() {
			let change = &held.change;
			held.keys.retain(|key| key.unwrap(change, keys).is_some());
			held.keys.truncate(1);
		} else {
			let mut distinct = Vec::new();
			for key in held.keys.drain(..) {
				if !distinct.contains(&key) && distinct.len() < WRAPPED_KEYS_PER_CHANGE {
					distinct.push(key);
				}
			}
			held.keys = distinct;
		}
		if !self.parents_accepted(&held.change)? {
			if self.waiting.len() >= self.waiting_limit {
				return Err(LogError::TooManyWaiting);
			}
			for parent in held.change.parents() {
				if !self.read(self.history.contains(*parent))? {
					self.waiting_on.entry(*parent).or_default().insert(id);
				}
			}
			if covered {
				self.covered.insert(id);
			}
			self.waiting.insert(id, held);
			return Ok(Received::Waiting);
		}

		match self.check_accepted(&held.change) {
			Ok(()) => {}
			Err(LogError::Storage) => return Err(LogError::Storage),
			Err(error) => {
				self.reject(id, error.clone());
				return Err(error);
			}
		}
		self.clock.observe(held.change.timestamp(), wall_millis)?;
		if covered {
			self.covered.insert(id);
		}
		self.accept(held, keys)?;
		let mut accepted = vec![id];
		accepted.extend(self.accept_waiting(id, keys, wall_millis)?);
		self.check_storage()?;
		Ok(Received::Accepted(accepted))
	}

	/// Checks a change from elsewhere against who may write to the space: a space whose
	/// first change governs it has that one first change, and takes no change of a key that
	/// no governing change lists.
	fn check_governed(
		&mut self,
		id: ChangeId,
		parcel: &Parcel,
		keys: &SpaceKeys,
		signature: Signature,
	) -> Result<(), LogError> {
		let change = &parcel.change;
		// A space whose first change must govern it has that one first change, which does;
		// another space takes any number of changes without parents, whatever order they
		// come in.
		if change.parents().is_empty()
			&& self
				.expected_roots
				.as_ref()
				.is_some_and(|expected| !expected.contains(&id))
			&& self.read(self.history.count())? == 0
		{
			return Err(LogError::UnexpectedRoot);
		}
		if change.parents().is_empty() && self.expects_governed {
			let refused = if change.kind() != Kind::Governing {
				Some(LogError::UngovernedRoot)
			} else if self.read(self.history.count())? > 0 {
				Some(LogError::SecondRoot)
			} else {
				None
			};
			if let Some(error) = refused {
				self.rejected.insert(id, error.clone());
				return Err(error);
			}
		}
		// A change from elsewhere by a key that no governing change lists is not kept, so
		// that nobody can fill a replica with changes that never count. It is taken once a
		// listing of its key arrives, as sync sends it again; a key's listing of itself by a
		// recovery code is taken where the replica holds the code's statement.
		let first = change.parents().is_empty() && change.kind() == Kind::Governing;
		if signature == Signature::Check && !first {
			let known = self.governance(keys)?.is_known(change.author().as_bytes());
			if !known && !self.lists_itself_by_code(parcel, keys)? {
				return Err(LogError::UnknownAuthor);
			}
		}
		Ok(())
	}

	/// Whether a change lists its own author by a recovery code that a governing change the
	/// replica can read names, as its payload, which the keys the replica holds open, says:
	/// a new device listed by a code introduces itself so
	/// ([Log](../../../docs/spec/log.md#8-which-changes-count)).
	fn lists_itself_by_code(&self, parcel: &Parcel, keys: &SpaceKeys) -> Result<bool, LogError> {
		let change = &parcel.change;
		if change.kind() != Kind::Governing {
			return Ok(false);
		}
		// Only the change's own payload says what it does.
		let Some(payload) = parcel
			.payload
			.as_ref()
			.filter(|payload| payload.hash() == *change.payload_hash())
		else {
			return Ok(false);
		};
		let Some(opened) = parcel
			.keys
			.iter()
			.find_map(|key| change.open_checked(payload, key, keys).ok())
		else {
			return Ok(false);
		};
		if !holds_only_governing(&opened.operations) || !opened.restated.is_empty() {
			return Ok(false);
		}
		let mut fact = Fact::read(change, &opened.operations);
		let governance = self.governance(keys)?;
		if governance.governed() {
			fact.keep_signed();
		}
		Ok(governance.takes_listing_by_code(&fact))
	}

	/// Takes what came with a change that the log holds, which may be what the log lacks;
	/// what is not the change's is dropped.
	fn take_parts(&mut self, parcel: Parcel, keys: &SpaceKeys) -> Result<Received, LogError> {
		let id = parcel.id();
		let mut kept = false;
		if let Some(payload) = parcel.payload {
			match self.add_payload(id, payload, keys) {
				Ok(added) => kept |= added,
				Err(LogError::Storage) => return Err(LogError::Storage),
				Err(_) => {}
			}
		}
		for key in parcel.keys {
			match self.add_change_key(id, key, keys) {
				Ok(added) => kept |= added,
				Err(LogError::Storage) => return Err(LogError::Storage),
				Err(_) => {}
			}
		}
		self.check_storage()?;
		Ok(if kept {
			Received::Completed
		} else {
			Received::Duplicate
		})
	}

	/// The decrypted payloads of the changes whose operations the last call that makes or
	/// takes changes, payloads or keys applied, in the order it applied them, without those
	/// that the storage covered; each call forgets those of the one before.
	pub fn take_applied(&mut self) -> Vec<(ChangeId, Payload)> {
		std::mem::take(&mut self.applied)
	}

	/// What the purges that the last call that makes or takes changes, payloads or keys
	/// acted on destroyed: only those of changes that count, also of one that came to count
	/// then. Each call forgets what the one before destroyed.
	pub fn take_destroyed(&mut self) -> Destroyed {
		std::mem::take(&mut self.destroyed)
	}

	/// The payload of an accepted change, decrypted with `keys`, if the log has the payload
	/// and a key for it.
	///
	/// # Errors
	///
	/// Returns an error if `keys` lack the space key that the change's key was wrapped with,
	/// or the payload does not open or does not have the form of a payload, or the history
	/// cannot be read.
	pub fn payload(&self, id: ChangeId, keys: &SpaceKeys) -> Result<Option<Payload>, LogError> {
		match self.read(self.history.get(id))? {
			Some(accepted) => read_payload(&accepted, keys),
			None => Ok(None),
		}
	}

	/// The sealed payload that the log has for a change, accepted or waiting.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn sealed_payload(&self, id: ChangeId) -> Result<Option<SealedPayload>, S::Error> {
		Ok(self.parcel(id)?.and_then(|parcel| parcel.payload))
	}

	/// The wrapped keys that the log has for a change, accepted or waiting: one that it
	/// knows to be good, or a few that it cannot check yet.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn change_keys_of(&self, id: ChangeId) -> Result<Vec<WrappedChangeKey>, S::Error> {
		Ok(self.parcel(id)?.map_or_default(|parcel| parcel.keys))
	}

	/// An accepted or waiting change, with its sealed payload and wrapped keys as far as the
	/// log has them.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn parcel(&self, id: ChangeId) -> Result<Option<Parcel>, S::Error> {
		if let Some(held) = self.waiting.get(&id) {
			return Ok(Some(held.parcel()));
		}
		Ok(self.history.get(id)?.map(|accepted| parcel_of(&accepted)))
	}

	/// The accepted changes with their sealed payloads and wrapped keys, ordered by ID, for
	/// other replicas.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn parcels(&self) -> Result<Vec<Parcel>, S::Error> {
		let mut parcels = Vec::new();
		for id in self.history.ids()? {
			if let Some(accepted) = self.history.get(id)? {
				parcels.push(parcel_of(&accepted));
			}
		}
		Ok(parcels)
	}

	/// Every accepted change in short, for reconciling with another replica, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn summaries(&self) -> Result<Vec<Summary>, S::Error> {
		self.history.summaries()
	}

	/// Whether the operations of an accepted change are applied, or its payload found not to
	/// have the form it must: whether it adds nothing more.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_complete(&self, id: ChangeId) -> Result<bool, S::Error> {
		Ok(self
			.history
			.get(id)?
			.is_some_and(|accepted| accepted.status != Status::Pending))
	}

	/// Whether an accepted change is set aside: kept and passed on, but not applied, as its
	/// author is not listed, or was revoked by a replica that did not have it.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_set_aside(&self, id: ChangeId) -> Result<bool, S::Error> {
		Ok(self
			.history
			.get(id)?
			.is_some_and(|accepted| accepted.status == Status::SetAside))
	}

	/// The accepted changes that are set aside, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn set_aside_changes(&self) -> Result<Vec<ChangeId>, S::Error> {
		self.history.set_aside()
	}

	/// Whether the space's first change governs it, so that only the changes of the keys it
	/// lists count.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_governed(&self, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self.governance(keys)?.governed())
	}

	/// The space's first change, if it governs the space.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn root(&self) -> Result<Option<ChangeId>, S::Error> {
		for id in self.history.governing()? {
			if self
				.history
				.change(id)?
				.is_some_and(|change| change.parents().is_empty())
			{
				return Ok(Some(id));
			}
		}
		Ok(None)
	}

	/// The counting changes that revoke a key, ordered by ID, or every counting revocation
	/// if `key` is `None`.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn revocations_of(
		&self,
		key: Option<&VerifyingKey>,
		keys: &SpaceKeys,
	) -> Result<Vec<ChangeId>, LogError> {
		let governance = self.governance(keys)?;
		Ok(match key {
			Some(key) => governance
				.revocations_of(key.as_bytes())
				.map_or_default(|revocations| revocations.iter().copied().collect()),
			None => governance.revocations().into_iter().collect(),
		})
	}

	/// Every counting change that revokes a replica, ordered by ID.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn revocations(&self, keys: &SpaceKeys) -> Result<Vec<ChangeId>, LogError> {
		self.revocations_of(None, keys)
	}

	/// Whether a key is revoked by a change that counts.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_revoked(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self
			.governance(keys)?
			.revocations_of(key.as_bytes())
			.is_some())
	}

	/// Whether a revoked key would stay if it revoked back the keys that revoked it, as of
	/// two that revoke each other: a replica takes from it what it made without knowing of
	/// its revocation, where that revocation of its own may be, and sends it nothing
	/// ([Sync](../../../docs/spec/sync.md#4-who-syncs)).
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn may_revoke_back(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self.governance(keys)?.may_revoke_back(key.as_bytes()))
	}

	/// Whether a governing change that counts lists a key, or the space's first change does
	/// not govern it.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_listed(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		let governance = self.governance(keys)?;
		Ok(!governance.governed() || governance.is_listed(key.as_bytes()))
	}

	/// Whether the space has members: whether its first change gives a role, as that of
	/// every space other than a user's personal one does. Only the listings that the listed
	/// keys signed count there ([`nibkeep_governance::Listing`]).
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn has_members(&self, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self.governance(keys)?.members())
	}

	/// The members of a space with members, each with the role that the counting governing
	/// changes give them now: the lowest of those that no other of them builds on. Empty for
	/// a space without members.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn member_roles(
		&self,
		keys: &SpaceKeys,
	) -> Result<BTreeMap<nibkeep_base::EntityId, Role>, LogError> {
		Ok(self.governance(keys)?.roles_now())
	}

	/// Whether a space with members allows AI agents now, as its counting governing changes
	/// say: an AI agent's listing counts there only where they allow them. `false` for a
	/// space without members, which has no admins to allow them.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn allows_ai_agents(&self, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self.governance(keys)?.allows_ai_agents())
	}

	/// Whether a key is an AI agent's: whether the governing change that first listed it
	/// marks its replica as one ([Log](../../../docs/spec/log.md#8-which-changes-count)). Its
	/// changes then count only if they write what an AI agent may, and purge, seal and govern
	/// nothing.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_ai_agent(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		Ok(self.governance(keys)?.is_ai_agent(key.as_bytes()))
	}

	/// The choice of the device whose key is `key` that stands in a space with members, of
	/// those that counting changes state: its number and whether it holds the space; none if
	/// none states one, or the space has no members, where choices count for nothing
	/// ([technical design](../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn standing_choice(
		&self,
		key: &VerifyingKey,
		keys: &SpaceKeys,
	) -> Result<Option<(u64, bool)>, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.filter(|_| governance.governed())
			.and_then(|view| view.standing_choice(key.as_bytes())))
	}

	/// Whether the device whose key is `key` holds the space by its choices: whether its
	/// choice that stands holds it, or none does, as [`Log::standing_choice`] finds it.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn holds_space(&self, key: &VerifyingKey, keys: &SpaceKeys) -> Result<bool, LogError> {
		let governance = self.governance(keys)?;
		Ok(governance
			.view()
			.filter(|_| governance.governed())
			.is_none_or(|view| view.holds_now(key.as_bytes())))
	}

	/// Whether the accepted change `ancestor` is `of` or one of its ancestors.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn is_ancestor_of(&self, ancestor: ChangeId, of: ChangeId) -> Result<bool, LogError> {
		self.had(ancestor, of)
	}

	/// The wrapped record keys this replica has for a statement.
	#[must_use]
	pub fn wrapped_keys_of(&self, statement: StatementId) -> &[WrappedKey] {
		self.wrapped.get(&statement).map_or(&[], Vec::as_slice)
	}

	/// Receives a wrapped record key of a secret statement from another replica, and opens
	/// the statement if `keys` hold the secrets key. A key for a purged statement is
	/// ignored.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn receive_wrapped_key(
		&mut self,
		statement: StatementId,
		wrapped: WrappedKey,
		keys: &SpaceKeys,
	) -> Result<(), LogError> {
		if self.read(self.history.is_statement_purged(statement))? {
			return Ok(());
		}
		let candidates = self.wrapped.entry(statement).or_default();
		if !candidates.contains(&wrapped) && candidates.len() < WRAPPED_KEYS_PER_STATEMENT {
			candidates.push(wrapped);
		}
		self.open(statement, keys, &mut BTreeMap::new())
	}

	/// The wrapped record keys this replica has, for other replicas.
	pub fn wrapped_keys(&self) -> impl Iterator<Item = (StatementId, &WrappedKey)> {
		self.wrapped
			.iter()
			.flat_map(|(&statement, keys)| keys.iter().map(move |key| (statement, key)))
	}

	/// Opens every sealed statement that `keys` and the wrapped record keys allow, after
	/// the person has confirmed.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn unlock_secrets(&mut self, keys: &SpaceKeys) -> Result<(), LogError> {
		let sealed = self.read(self.history.sealed())?;
		// Each change that holds sealed statements is decrypted once.
		let mut payloads = BTreeMap::new();
		for statement in sealed {
			self.open(statement, keys, &mut payloads)?;
		}
		let secret: Vec<Hash> = self.secret_candidates.iter().copied().collect();
		for sealed in secret {
			self.open_blob_key(sealed, keys);
		}
		Ok(())
	}

	/// Forgets the sealed statements this replica has opened, and the secret blob keys, so
	/// that the log holds no secret values until they are unlocked again.
	pub fn lock_secrets(&mut self) {
		self.state.storage_mut().forget_all();
		for sealed in std::mem::take(&mut self.secret_blobs) {
			self.forget_blob_key(&sealed);
		}
	}

	/// Restores what purges destroyed, as `restores` names it: makes a change that restores it,
	/// signed by `author`, and takes it in. What it names is no longer purged, unless a purge
	/// that the change does not build on purges it too: a change waits for its payload and
	/// key again, and a statement or a blob for its wrapped key, which a replica that kept
	/// them gives back, as [`Destroyed::restored`] says
	/// ([technical design: copies that wait]). The change holds nothing else, and counts
	/// only where its author may purge what it restores.
	///
	/// [technical design: copies that wait]: ../../../docs/Technical-design.md#911-copies-that-wait
	///
	/// # Errors
	///
	/// Returns [`LogError::InvalidPurge`] if it would restore nothing, or what this replica's
	/// history does not hold; [`LogError::CannotRestore`] if the change would not count, as
	/// the author's role does not allow it, in which case nothing is made; and another error
	/// if the clock has run out of timestamps.
	pub fn restore_purged(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		restores: Restores,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		self.check_storage()?;
		if restores.is_empty() {
			return Err(LogError::InvalidPurge);
		}
		for &change in &restores.changes {
			if !self.read(self.history.contains(change))? {
				return Err(LogError::InvalidPurge);
			}
		}
		for &statement in &restores.statements {
			if self.read(self.history.holder(statement))?.is_none() {
				return Err(LogError::InvalidPurge);
			}
		}
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let heads = self.read(self.history.heads())?;
		let timestamp = self.clock.now(wall_millis)?;
		let mut draft = Draft::new(heads, timestamp, Payload::default());
		draft.restores = restores;
		let parcel = Change::create(self.space, keys, author, &draft, rng);
		// One that would be set aside is never made: it could come to count later, when
		// nobody asks for it.
		let counts = {
			let governance = self.governance(keys)?;
			self.counts_now(&parcel.change, None, &governance, |_| false)?
		};
		if !counts {
			return Err(LogError::CannotRestore);
		}
		self.accept(held(parcel.clone()), keys)?;
		self.check_storage()?;
		Ok(parcel)
	}

	/// Purges statements: makes a change that destroys, everywhere, the record key of each
	/// secret one, and the key and payload of the change that asserted each other one,
	/// stating again everything else that those changes held. What a purged statement
	/// replaced stays replaced, so purging never brings back an older value.
	///
	/// # Errors
	///
	/// Returns an error if a statement is not one that this replica can purge: unknown, or
	/// asserted in a change that governs the space or whose payload it cannot read; or if the
	/// clock has run out of timestamps.
	pub fn purge(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		statements: impl IntoIterator<Item = StatementId>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		self.purge_with_blobs(author, keys, statements, (&[], false), wall_millis, rng)
	}

	/// Purges statements as [`Log::purge`] does, and in the same change the blobs with the
	/// content hashes `contents`: every key of the kind `secret` says that this replica has
	/// opened for them, secret blob keys or ordinary ones, as [`Log::purge_blob`] destroys
	/// them. Content that the replica has no such key for adds nothing.
	///
	/// # Errors
	///
	/// As [`Log::purge`].
	pub fn purge_with_blobs(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		statements: impl IntoIterator<Item = StatementId>,
		(contents, of_secrets): (&[Hash], bool),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		self.check_storage()?;
		let mut secret = BTreeSet::new();
		let mut ordinary = BTreeSet::new();
		for statement in statements {
			if self.read(self.history.holder(statement))?.is_some() {
				secret.insert(statement);
			} else {
				ordinary.insert(statement);
			}
		}
		let changes = self.asserting(&ordinary, keys)?;
		// What the purged changes held besides, by the change each part came from.
		let mut restating = Restating::default();
		for &change in &changes {
			let accepted = self
				.read(self.history.get(change))?
				.ok_or(LogError::CannotPurge)?;
			let payload = read_payload(&accepted, keys)?.ok_or(LogError::CannotPurge)?;
			let mut parts = vec![(change, payload.operations, payload.sealed)];
			parts.extend(
				payload
					.restated
					.into_iter()
					.map(|(source, part)| (source, part.operations, part.sealed)),
			);
			for (source, operations, sealed) in parts {
				let entry = restating.payload.restated.entry(source).or_default();
				entry.operations.extend(
					operations
						.into_iter()
						.filter_map(|operation| without(operation, &ordinary)),
				);
				for (id, envelope) in sealed {
					if secret.contains(&id) {
						continue;
					}
					if let Some(&hash) = accepted.change.sealed().get(&id) {
						restating.record_keys.insert(id, hash);
					}
					entry.sealed.insert(id, envelope);
				}
			}
		}
		restating
			.payload
			.restated
			.retain(|_, part| !part.is_empty());
		let blobs = contents
			.iter()
			.filter_map(|content| self.sealed_blobs.get(content))
			.flatten()
			.filter(|sealed| self.secret_blobs.contains(sealed) == of_secrets)
			.copied()
			.collect();
		let purges = Purges {
			changes,
			statements: secret,
			blobs,
		};
		self.commit_purges(author, keys, purges, restating, wall_millis, rng)
	}

	/// The changes that purging these statements purges: every change that asserts one of
	/// them, which [`Log::purge`] purges and states again without them. Secret statements are
	/// purged through their record keys alone, and add none.
	///
	/// # Errors
	///
	/// Returns [`LogError::CannotPurge`] if a statement is not one that this replica can
	/// purge: unknown, or asserted in a change that governs the space or whose payload it
	/// cannot read; and [`LogError::Storage`] if the state or the history cannot be read.
	pub fn changes_to_purge(
		&self,
		statements: impl IntoIterator<Item = StatementId>,
		keys: &SpaceKeys,
	) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut ordinary = BTreeSet::new();
		for statement in statements {
			if self.read(self.history.holder(statement))?.is_none() {
				ordinary.insert(statement);
			}
		}
		self.asserting(&ordinary, keys)
	}

	/// The changes that assert any of these ordinary statements: the one that first did,
	/// every other that asserted it too, as devices that run one migration at once do, and
	/// the purges that state one again. Each must be one that a purge can purge.
	fn asserting(
		&self,
		statements: &BTreeSet<StatementId>,
		keys: &SpaceKeys,
	) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut changes = BTreeSet::new();
		for &statement in statements {
			let origin = self
				.read(self.state.storage().get(statement))?
				.and_then(|recorded| recorded.change)
				.ok_or(LogError::CannotPurge)?;
			changes.insert(ChangeId::from_hash(origin));
			for again in self.read(self.state.storage().asserted_again(statement))? {
				changes.insert(ChangeId::from_hash(again));
			}
		}
		// Purges that state one again, which only they do besides the first.
		for id in self.read(self.history.purging())? {
			let Some(accepted) = self.read(self.history.get(id))? else {
				continue;
			};
			let Ok(Some(payload)) = read_payload(&accepted, keys) else {
				continue;
			};
			let restates = payload
				.restated
				.values()
				.flat_map(|restated| &restated.operations)
				.any(|operation| {
					matches!(operation, Operation::Assert { statement, .. } if statements.contains(&statement.id))
				});
			if restates {
				changes.insert(id);
			}
		}
		for &change in &changes {
			let purgeable = self
				.read(self.history.get(change))?
				.is_some_and(|accepted| {
					accepted.change.kind() == Kind::Ordinary && accepted.payload.is_some()
				});
			if !purgeable {
				return Err(LogError::CannotPurge);
			}
		}
		Ok(changes)
	}

	/// The changes that were purged, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn purged_changes(&self) -> Result<Vec<ChangeId>, S::Error> {
		self.history.purged_changes()
	}

	/// Whether a change was purged: its key and payload destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_change_purged(&self, id: ChangeId) -> Result<bool, S::Error> {
		self.history.is_change_purged(id)
	}

	/// Whether a secret statement was purged.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_purged(&self, statement: StatementId) -> Result<bool, S::Error> {
		self.history.is_statement_purged(statement)
	}

	/// Whether a statement is secret: held sealed in an accepted change, whether or not
	/// this replica has opened it.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_sealed(&self, statement: StatementId) -> Result<bool, S::Error> {
		Ok(self.history.holder(statement)?.is_some())
	}

	/// Adds the key of a blob that this replica sealed, wrapped with the newest space key, so
	/// that the replica can read the blob and pass the key on. The key of a purged sealed
	/// blob is ignored.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn add_blob_key(
		&mut self,
		content: Hash,
		sealed: Hash,
		key: Key,
		keys: &SpaceKeys,
		rng: &mut impl CryptoRng,
	) -> Result<(), LogError> {
		if self.blob_keys.contains_key(&sealed) || self.is_blob_purged_reading(&sealed)? {
			return Ok(());
		}
		let wrapped = WrappedBlobKey::wrap(self.space, &sealed, &content, &key, keys, rng);
		self.add_wrapped_blob_key(sealed, wrapped, (content, key), false);
		Ok(())
	}

	/// Adds the key of a blob of a secret field that this replica sealed, wrapped with the
	/// newest secrets key inside the newest space key, so that only a replica that holds the
	/// secrets can read the blob, and every replica can pass the key on. The key of a purged
	/// sealed blob is ignored.
	///
	/// # Errors
	///
	/// Returns [`LogError::SecretsLocked`] if `keys` hold no secrets key, and
	/// [`LogError::Storage`] if the history cannot be read.
	pub fn add_secret_blob_key(
		&mut self,
		content: Hash,
		sealed: Hash,
		key: Key,
		keys: &SpaceKeys,
		rng: &mut impl CryptoRng,
	) -> Result<(), LogError> {
		if keys.newest_secrets().is_none() {
			return Err(LogError::SecretsLocked);
		}
		if self.blob_keys.contains_key(&sealed) || self.is_blob_purged_reading(&sealed)? {
			return Ok(());
		}
		let wrapped = WrappedBlobKey::wrap_secret(self.space, &sealed, &content, &key, keys, rng)
			.ok_or(LogError::SecretsLocked)?;
		self.secret_candidates.insert(sealed);
		self.add_wrapped_blob_key(sealed, wrapped, (content, key), true);
		Ok(())
	}

	/// Keeps a blob key that this replica made, first among the wrapped keys of its blob,
	/// opened.
	fn add_wrapped_blob_key(
		&mut self,
		sealed: Hash,
		wrapped: WrappedBlobKey,
		opened: (Hash, Key),
		secret: bool,
	) {
		let candidates = self.wrapped_blob_keys.entry(sealed).or_default();
		candidates.insert(0, wrapped);
		candidates.truncate(WRAPPED_KEYS_PER_BLOB);
		self.keep_blob_key(sealed, opened, secret);
	}

	/// Keeps a blob key opened, with the content hash it claims.
	fn keep_blob_key(&mut self, sealed: Hash, (content, key): (Hash, Key), secret: bool) {
		self.sealed_blobs.entry(content).or_default().insert(sealed);
		self.blob_keys.insert(sealed, (content, key));
		if secret {
			self.secret_blobs.insert(sealed);
		}
	}

	/// Forgets an opened blob key, and keeps its wrapped keys.
	fn forget_blob_key(&mut self, sealed: &Hash) {
		self.secret_blobs.remove(sealed);
		if let Some((content, _)) = self.blob_keys.remove(sealed)
			&& let Some(sealed_blobs) = self.sealed_blobs.get_mut(&content)
		{
			sealed_blobs.remove(sealed);
			if sealed_blobs.is_empty() {
				self.sealed_blobs.remove(&content);
			}
		}
	}

	/// The sealed blobs with a wrapped key in the form of a secret blob key: those whose keys
	/// unlocking the secrets may open or drop.
	pub fn sealed_with_secret_keys(&self) -> impl Iterator<Item = &Hash> {
		self.secret_candidates.iter()
	}

	/// Whether the opened key of a sealed blob is a secret blob key: one of a blob of a
	/// secret field, which only a replica that holds the secrets opens.
	#[must_use]
	pub fn is_secret_blob(&self, sealed: &Hash) -> bool {
		self.secret_blobs.contains(sealed)
	}

	/// Whether a sealed blob's key was destroyed, as the history says.
	fn is_blob_purged_reading(&self, sealed: &Hash) -> Result<bool, LogError> {
		self.read(self.history.is_blob_purged(sealed))
	}

	/// Receives a wrapped blob key from another replica, and opens it if `keys` hold the
	/// space key it needs, and for a secret blob key the secrets key too. A wrapped key that
	/// does not open with the keys it names is dropped, and a key of a purged sealed blob is
	/// ignored. One whose space key the replica holds is kept even when the replica keeps the
	/// most it does for one blob, in place of one whose space key it lacks, so that wrapped
	/// keys it cannot check never crowd out a good one; a secret blob key that waits for the
	/// secrets is never replaced, as it may be the good one. A secret blob key that opens
	/// takes the place of an ordinary key opened for the same sealed blob: only a replica
	/// that holds the secrets can make one, and any replica of the space an ordinary one.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read.
	pub fn receive_blob_key(
		&mut self,
		sealed: Hash,
		wrapped: WrappedBlobKey,
		keys: &SpaceKeys,
	) -> Result<(), LogError> {
		if self.is_blob_purged_reading(&sealed)? {
			return Ok(());
		}
		let checkable = keys.get(wrapped.epoch()).is_some();
		let opened = if checkable {
			match wrapped.unwrap(self.space, &sealed, keys) {
				None => return Ok(()),
				Some(Unwrapped::Secret) => {
					self.secret_candidates.insert(sealed);
					None
				}
				Some(Unwrapped::Key {
					content,
					key,
					secret,
				}) => {
					if secret {
						self.secret_candidates.insert(sealed);
					}
					Some(((content, key), secret))
				}
			}
		} else {
			None
		};
		let space = self.space;
		let candidates = self.wrapped_blob_keys.entry(sealed).or_default();
		// A secret blob key that opens shows the sealed blob to be a secret one, sealed by a
		// replica that holds the secrets: its ordinary keys are forged.
		if matches!(opened, Some((_, true))) {
			candidates.retain(|candidate| {
				!matches!(
					candidate.unwrap(space, &sealed, keys),
					Some(Unwrapped::Key { secret: false, .. })
				)
			});
		}
		if !candidates.contains(&wrapped) {
			if candidates.len() < WRAPPED_KEYS_PER_BLOB {
				candidates.push(wrapped);
			} else if checkable
				&& let Some(unchecked) = candidates
					.iter()
					.position(|candidate| keys.get(candidate.epoch()).is_none())
			{
				candidates[unchecked] = wrapped;
			}
		}
		if let Some((opened, secret)) = opened {
			self.prefer_blob_key(sealed, opened, secret);
		}
		Ok(())
	}

	/// Keeps a blob key opened for a sealed blob that has none opened yet, or, for a secret
	/// blob key, in place of an ordinary one.
	fn prefer_blob_key(&mut self, sealed: Hash, opened: (Hash, Key), secret: bool) {
		if !self.blob_keys.contains_key(&sealed) {
			self.keep_blob_key(sealed, opened, secret);
		} else if secret && !self.secret_blobs.contains(&sealed) {
			self.forget_blob_key(&sealed);
			self.keep_blob_key(sealed, opened, secret);
		}
	}

	/// Opens the wrapped blob keys that `keys` allow, for example after the replica received
	/// an older space key, or unlocked its secrets.
	pub fn open_blob_keys(&mut self, keys: &SpaceKeys) {
		// Those opened with an ordinary key too, if they have other keys, which a new space
		// key may show to be secret ones.
		let unopened: Vec<Hash> = self
			.wrapped_blob_keys
			.iter()
			.filter(|(sealed, candidates)| {
				!self.secret_blobs.contains(*sealed)
					&& (!self.blob_keys.contains_key(*sealed) || candidates.len() > 1)
			})
			.map(|(sealed, _)| *sealed)
			.collect();
		for sealed in unopened {
			self.open_blob_key(sealed, keys);
		}
	}

	/// The wrapped keys this replica has for a sealed blob.
	#[must_use]
	pub fn wrapped_blob_keys_of(&self, sealed: &Hash) -> &[WrappedBlobKey] {
		self.wrapped_blob_keys
			.get(sealed)
			.map_or(&[], Vec::as_slice)
	}

	/// The content that the key this replica opened for a sealed blob says it seals, if it
	/// opened one.
	#[must_use]
	pub fn content_of_sealed(&self, sealed: &Hash) -> Option<Hash> {
		self.blob_keys.get(sealed).map(|(content, _)| *content)
	}

	/// The wrapped blob keys this replica has, by sealed hash, for other replicas.
	pub fn wrapped_blob_keys(&self) -> impl Iterator<Item = (&Hash, &WrappedBlobKey)> {
		self.wrapped_blob_keys
			.iter()
			.flat_map(|(sealed, keys)| keys.iter().map(move |key| (sealed, key)))
	}

	/// The keys this replica has opened for the content with the hash `content`, by the
	/// sealed hash of the blob each key seals. There are several when replicas sealed the
	/// same content before they learned of each other's keys.
	///
	/// The content hash is what the wrapped keys claim, which only reading the whole sealed
	/// blob checks. These keys are therefore for reading, never for sealing other content.
	pub fn blob_keys(&self, content: &Hash) -> impl Iterator<Item = (&Hash, &Key)> {
		self.sealed_blobs
			.get(content)
			.into_iter()
			.flatten()
			.filter_map(|sealed| self.blob_keys.get(sealed).map(|(_, key)| (sealed, key)))
	}

	/// Purges a blob: makes a change that destroys, everywhere, every key that this replica
	/// has opened for the content with the hash `content`.
	///
	/// # Errors
	///
	/// Returns an error if the replica has no key for the content, or if the clock has run
	/// out of timestamps.
	pub fn purge_blob(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		content: &Hash,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		let sealed = self
			.sealed_blobs
			.get(content)
			.filter(|sealed| !sealed.is_empty())
			.ok_or(LogError::UnknownBlob)?;
		let purges = Purges {
			blobs: sealed.clone(),
			..Purges::default()
		};
		self.commit_purges(author, keys, purges, Restating::default(), wall_millis, rng)
	}

	/// Whether the key of a sealed blob was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn is_blob_purged(&self, sealed: &Hash) -> Result<bool, S::Error> {
		self.history.is_blob_purged(sealed)
	}

	/// The sealed hashes of the blobs whose keys were destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn purged_blobs(&self) -> Result<Vec<Hash>, S::Error> {
		self.history.purged_blobs()
	}

	/// The content hashes of the blobs whose keys this replica has opened.
	pub fn blobs(&self) -> impl Iterator<Item = &Hash> {
		self.sealed_blobs.keys()
	}

	/// The current state of the space, with the secret values this replica has opened.
	#[must_use]
	pub const fn state(&self) -> &SpaceState<S> {
		&self.state
	}

	/// The state of the space as of a wall clock time: the result of the changes whose
	/// timestamps are at most `millis`, with the secret values this replica has opened. It is
	/// a view of the state, which knows when each statement was recorded and superseded.
	#[must_use]
	pub fn state_as_of(&self, millis: u64) -> State<AsOf<'_, Layers<S>>> {
		State::in_storage(AsOf::new(self.state.storage(), millis))
	}

	/// The changes that no other change names as a parent.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn heads(&self) -> Result<BTreeSet<ChangeId>, S::Error> {
		self.history.heads()
	}

	/// An accepted change.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn change(&self, id: ChangeId) -> Result<Option<Change>, S::Error> {
		Ok(self.history.change(id)?.map(Cow::into_owned))
	}

	/// The accepted changes, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn changes(&self) -> Result<Vec<Change>, S::Error> {
		let mut changes = Vec::new();
		for id in self.history.ids()? {
			changes.extend(self.change(id)?);
		}
		Ok(changes)
	}

	/// Whether the log has a change, accepted or waiting.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn contains(&self, id: ChangeId) -> Result<bool, S::Error> {
		Ok(self.waiting.contains_key(&id) || self.history.contains(id)?)
	}

	/// Whether waiting changes wait for a change: accepting it may accept them too, whatever
	/// they do.
	#[must_use]
	pub fn is_awaited(&self, id: ChangeId) -> bool {
		self.waiting_on.contains_key(&id)
	}

	/// The changes rejected for good, with the reason. Another replica need not send them
	/// again.
	#[must_use]
	pub const fn rejected(&self) -> &BTreeMap<ChangeId, LogError> {
		&self.rejected
	}

	/// How many changes the log keeps, accepted or waiting.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn kept(&self) -> Result<usize, S::Error> {
		Ok(self.history.count()? + self.waiting.len())
	}

	/// The changes that waiting changes need and that the log does not have.
	///
	/// # Errors
	///
	/// Returns an error if the history cannot be read.
	pub fn missing(&self) -> Result<BTreeSet<ChangeId>, S::Error> {
		let mut missing = BTreeSet::new();
		for &id in self.waiting_on.keys() {
			if !self.contains(id)? {
				missing.insert(id);
			}
		}
		Ok(missing)
	}

	/// The replica that first asserted a statement, if this replica can read the statement.
	///
	/// # Errors
	///
	/// Returns an error if the storage of the state or the history cannot be read.
	pub fn author_of(&self, statement: StatementId) -> Result<Option<VerifyingKey>, S::Error> {
		let Some(change) = self
			.state
			.storage()
			.get(statement)?
			.and_then(|recorded| recorded.change)
		else {
			return Ok(None);
		};
		Ok(self
			.history
			.change(ChangeId::from_hash(change))?
			.map(|change| *change.author()))
	}

	fn parents_accepted(&self, change: &Change) -> Result<bool, LogError> {
		for parent in change.parents() {
			if !self.read(self.history.contains(*parent))? {
				return Ok(false);
			}
		}
		Ok(true)
	}

	/// Checks a change whose parents are all accepted: it must be later than each of them,
	/// and purge, or restore, only changes it builds on that do not govern the space, and
	/// statements sealed in changes it builds on. A change that restores is an ordinary one
	/// that purges nothing.
	fn check_accepted(&self, change: &Change) -> Result<(), LogError> {
		for parent in change.parents() {
			let parent = self.read(self.history.change(*parent))?;
			if parent.is_some_and(|parent| change.timestamp() <= parent.timestamp()) {
				return Err(LogError::NotAfterParents);
			}
		}
		let restores = change.restores();
		if !restores.is_empty()
			&& (change.kind() != Kind::Ordinary
				|| !change.purges().is_empty()
				|| !change.sealed().is_empty())
		{
			return Err(LogError::InvalidPurge);
		}
		for named in [change.purges(), restores] {
			for &purged in &named.changes {
				let ordinary = self
					.read(self.history.change(purged))?
					.is_some_and(|purged| purged.kind() == Kind::Ordinary);
				if !(ordinary && self.is_ancestor(purged, change)?) {
					return Err(LogError::InvalidPurge);
				}
			}
			for &statement in &named.statements {
				let Some(holder) = self.read(self.history.holder(statement))? else {
					return Err(LogError::InvalidPurge);
				};
				if !self.is_ancestor(holder, change)? {
					return Err(LogError::InvalidPurge);
				}
			}
		}
		Ok(())
	}

	/// Whether an ordinary change's payload asserts, without sealing it, a statement of a
	/// field that a counting governing change makes secret for good, where no such change for
	/// that field builds on it: only values written before the field became secret count in
	/// plain. So too for a statement it states again, by the change that it came from.
	/// Every replica seals what it knows to be secret, so a value written in plain since, as
	/// by a replica that turned against its user and leaves the mark out of its change's
	/// parents, would replace a password with one of its own; one that a device which did
	/// not know wrote at the same time is set aside too.
	fn writes_secret_plainly(
		&self,
		change: &Change,
		payload: &Payload,
		governance: &Governance,
	) -> Result<bool, LogError> {
		if !governance.governed() {
			return Ok(false);
		}
		// For each field written in plain that is secret for good, the changes that make it
		// so.
		let marked = |operations: &[Operation]| -> Vec<&BTreeSet<ChangeId>> {
			operations
				.iter()
				.filter_map(|operation| match operation {
					Operation::Assert { statement, .. } => governance.made_secret(&statement.field),
					_ => None,
				})
				.collect()
		};
		// Whether a mark builds on the change, so that the value was there before the field
		// became secret. The space's first change, which makes the standard vocabulary's
		// secret fields secret, builds on none.
		let before = |marks: &BTreeSet<ChangeId>, written: ChangeId| -> Result<bool, LogError> {
			for &mark in marks {
				if let Some(mark) = self.read(self.history.change(mark))?
					&& self.is_ancestor(written, &mark)?
				{
					return Ok(true);
				}
			}
			Ok(false)
		};
		for marks in marked(&payload.operations) {
			if !before(marks, change.id())? {
				return Ok(true);
			}
		}
		for (source, restated) in &payload.restated {
			for marks in marked(&restated.operations) {
				if !before(marks, *source)? {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	/// What a revocation, or a change of roles, of this replica builds on: its heads, but
	/// for what builds on a governing change whose payload it has not read, or is one. Such
	/// a change may revoke this replica, sent without its payload by a replica that it
	/// revoked, and this replica's own revocation of that one would otherwise build on it,
	/// and be set aside, though it would stay by revoking that one back
	/// (`docs/spec/log.md`, section 8). Where nothing is left, as when the space's first
	/// change is unread, it builds on the heads.
	fn defending_parents(&self) -> Result<BTreeSet<ChangeId>, LogError> {
		let heads = self.read(self.history.heads())?;
		let unread = self.unread_governing()?;
		if unread.is_empty() {
			return Ok(heads);
		}
		let tainted = self.building_on(&unread, &heads)?;
		let mut parents = BTreeSet::new();
		let mut to_visit: Vec<ChangeId> = heads.into_iter().collect();
		let mut visited = BTreeSet::new();
		while let Some(id) = to_visit.pop() {
			if !visited.insert(id) {
				continue;
			}
			if !tainted.contains(&id) {
				parents.insert(id);
			} else if let Some(change) = self.read(self.history.change(id))? {
				to_visit.extend(change.parents().iter().copied());
			}
		}
		if parents.is_empty() {
			return self.read(self.history.heads());
		}
		// None that another of them builds on.
		let mut floor = u64::MAX;
		for &id in &parents {
			if let Some(depth) = self.read(self.history.depth(id))? {
				floor = floor.min(depth);
			}
		}
		let mut below = BTreeSet::new();
		for &id in &parents {
			below.extend(self.ancestors_down_to(id, floor)?);
		}
		Ok(parents.difference(&below).copied().collect())
	}

	/// The governing changes whose payloads, or keys that open them, are not there yet,
	/// with their depths.
	fn unread_governing(&self) -> Result<BTreeMap<ChangeId, u64>, LogError> {
		let mut unread = BTreeMap::new();
		for id in self.read(self.history.governing())? {
			if self.read(self.history.status(id))? == Some(Status::Pending)
				&& let Some(depth) = self.read(self.history.depth(id))?
			{
				unread.insert(id, depth);
			}
		}
		Ok(unread)
	}

	/// Of `from` and their ancestors, those that are one of `unread` or build on one. Only
	/// changes as deep as the shallowest of them can, so the walk stops above it.
	fn building_on(
		&self,
		unread: &BTreeMap<ChangeId, u64>,
		from: &BTreeSet<ChangeId>,
	) -> Result<BTreeSet<ChangeId>, LogError> {
		let floor = unread.values().copied().min().unwrap_or(u64::MAX);
		let mut reached: BTreeMap<ChangeId, (u64, Vec<ChangeId>)> = BTreeMap::new();
		let mut to_visit: Vec<ChangeId> = from.iter().copied().collect();
		while let Some(id) = to_visit.pop() {
			if reached.contains_key(&id) {
				continue;
			}
			let Some(accepted) = self.read(self.history.get(id))? else {
				continue;
			};
			if accepted.depth < floor {
				continue;
			}
			let parents: Vec<ChangeId> = accepted.change.parents().iter().copied().collect();
			to_visit.extend(parents.iter().copied());
			reached.insert(id, (accepted.depth, parents));
		}
		// Parents first, as each is shallower than its children.
		let mut order: Vec<(u64, ChangeId)> = reached
			.iter()
			.map(|(&id, &(depth, _))| (depth, id))
			.collect();
		order.sort_unstable();
		let mut building = BTreeSet::new();
		for (_, id) in order {
			let parents = reached
				.get(&id)
				.map_or(&[][..], |(_, parents)| parents.as_slice());
			if unread.contains_key(&id) || parents.iter().any(|parent| building.contains(parent)) {
				building.insert(id);
			}
		}
		Ok(building)
	}

	/// Whether an accepted change is an ancestor of `change`, whose parents are accepted.
	/// Only changes deeper than the ancestor can descend from it, so the walk stops at the
	/// others.
	fn is_ancestor(&self, ancestor: ChangeId, change: &Change) -> Result<bool, LogError> {
		let Some(depth) = self.read(self.history.depth(ancestor))? else {
			return Ok(false);
		};
		let mut to_visit: Vec<ChangeId> = change.parents().iter().copied().collect();
		let mut visited = BTreeSet::new();
		while let Some(id) = to_visit.pop() {
			if id == ancestor {
				return Ok(true);
			}
			if !visited.insert(id) {
				continue;
			}
			if self
				.read(self.history.depth(id))?
				.is_some_and(|visiting| visiting > depth)
				&& let Some(visiting) = self.read(self.history.change(id))?
			{
				to_visit.extend(visiting.parents());
			}
		}
		Ok(false)
	}

	/// Every accepted change that a change builds on, all the way down.
	fn ancestors_of(&self, id: ChangeId) -> Result<BTreeSet<ChangeId>, LogError> {
		let mut found = BTreeSet::new();
		let mut to_visit: Vec<ChangeId> = self
			.read(self.history.change(id))?
			.map_or_else(Vec::new, |change| {
				change.parents().iter().copied().collect()
			});
		while let Some(id) = to_visit.pop() {
			if found.insert(id)
				&& let Some(change) = self.read(self.history.change(id))?
			{
				to_visit.extend(change.parents());
			}
		}
		Ok(found)
	}

	/// The depth of a change whose parents are all accepted: one more than its deepest
	/// parent.
	fn depth_of(&self, change: &Change) -> Result<u64, LogError> {
		let mut deepest = 0;
		for parent in change.parents() {
			deepest = deepest.max(self.read(self.history.depth(*parent))?.unwrap_or(0));
		}
		Ok(deepest + 1)
	}

	/// Accepts the waiting changes that `accepted` completes, then those that they
	/// complete, and so on. Returns them in the order they were accepted. A waiting change
	/// that cannot be accepted is rejected, together with everything that waits for it.
	fn accept_waiting(
		&mut self,
		accepted: ChangeId,
		keys: &SpaceKeys,
		wall_millis: u64,
	) -> Result<Vec<ChangeId>, LogError> {
		let mut accepted_now = Vec::new();
		let mut completed = vec![accepted];
		while let Some(parent) = completed.pop() {
			for child in self.waiting_on.remove(&parent).unwrap_or_default() {
				let ready = match self.waiting.get(&child) {
					Some(held) => self.parents_accepted(&held.change)?,
					None => false,
				};
				if !ready {
					continue;
				}
				let Some(held) = self.waiting.remove(&child) else {
					continue;
				};
				let accepted = self.check_accepted(&held.change).and_then(|()| {
					self.clock
						.observe(held.change.timestamp(), wall_millis)
						.map_err(LogError::from)
				});
				match accepted {
					Ok(()) => {
						self.accept(held, keys)?;
						accepted_now.push(child);
						completed.push(child);
					}
					Err(LogError::Storage) => return Err(LogError::Storage),
					Err(error) => {
						self.waiting.insert(child, held);
						self.reject(child, error);
					}
				}
			}
		}
		Ok(accepted_now)
	}

	/// Rejects a change for good, and with it every waiting change that needs it.
	fn reject(&mut self, id: ChangeId, error: LogError) {
		let mut to_reject = vec![(id, error)];
		while let Some((id, error)) = to_reject.pop() {
			if let Some(held) = self.waiting.remove(&id) {
				for parent in held.change.parents() {
					if let Some(children) = self.waiting_on.get_mut(parent) {
						children.remove(&id);
						if children.is_empty() {
							self.waiting_on.remove(parent);
						}
					}
				}
			}
			self.covered.remove(&id);
			for child in self.waiting_on.remove(&id).unwrap_or_default() {
				to_reject.push((child, LogError::RejectedParent));
			}
			self.rejected.insert(id, error);
		}
	}

	/// Accepts a change whose parents are all accepted: takes in what its header says, the
	/// statements it seals and what it purges, and then applies its operations if its
	/// payload and key are there.
	fn accept(&mut self, held: Held, keys: &SpaceKeys) -> Result<(), LogError> {
		let id = held.change.id();
		let governing = held.change.kind() == Kind::Governing;
		// A governing change is decided on once it can be read. An ordinary one arrives
		// after every revocation known, none of which builds on it.
		let counts = governing || {
			let governance = self.governance(keys)?;
			self.counts_now(&held.change, None, &governance, |_| false)?
		};
		let depth = self.depth_of(&held.change)?;
		let accepted = Accepted {
			change: held.change,
			depth,
			payload: held.payload,
			keys: held.keys,
			status: if counts {
				Status::Pending
			} else {
				Status::SetAside
			},
		};
		let added = self.history.add(&accepted);
		self.read(added)?;
		if !counts {
			self.hold_sealed(&accepted.change)?;
			return Ok(());
		}
		// What it purges goes first: a statement that it states again from a change it
		// purges is held by it.
		let recount = self.take_in_header(&accepted.change, true)?;
		self.hold_sealed(&accepted.change)?;
		if recount {
			self.recount_purges_and_decide(keys)?;
		}
		if self.stale
			&& let Some(fresh) = self.state.storage().ordinary().empty_like()
		{
			self.rebuild(fresh, keys)?;
		}
		self.complete(id, keys)
	}

	/// Acts on what an accepted change's header says, once it counts: purges what it purges.
	/// A change that comes to count later, as when it was set aside, not `fresh`, leaves what a
	/// counting change that builds on it restored as it is. Returns whether what purges
	/// destroyed is to be counted again: after a change that restores.
	pub(super) fn take_in_header(
		&mut self,
		change: &Change,
		fresh: bool,
	) -> Result<bool, LogError> {
		let by = change.id();
		if !change.restores().is_empty() {
			if let Some(restoring) = self.restoring.get_mut() {
				restoring.insert(by);
			}
			return Ok(true);
		}
		let purges = change.purges().clone();
		// A change just accepted has no descendants yet, so nothing restores after it.
		let restorers = if fresh || purges.is_empty() {
			Vec::new()
		} else {
			self.counting_restorers()?
		};
		for purged in purges.changes {
			if !self.restored_after(by, &restorers, &|named| named.changes.contains(&purged))? {
				self.destroy_change(purged, by)?;
			}
		}
		for statement in purges.statements {
			if !self.restored_after(by, &restorers, &|named| {
				named.statements.contains(&statement)
			})? {
				self.destroy_statement(statement, by)?;
			}
		}
		for sealed in purges.blobs {
			if !self.restored_after(by, &restorers, &|named| named.blobs.contains(&sealed))? {
				self.destroy_blob_key(sealed, by)?;
			}
		}
		Ok(false)
	}

	/// Whether one of `restorers`, which count, builds on `purge` and restores what `named`
	/// says.
	pub(super) fn restored_after(
		&self,
		purge: ChangeId,
		restorers: &[(ChangeId, Restores)],
		named: &dyn Fn(&Restores) -> bool,
	) -> Result<bool, LogError> {
		for (restorer, restores) in restorers {
			if named(restores) && self.had(purge, *restorer)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	/// The accepted changes that restore anything and count now, with what each restores:
	/// rare, and found from the changes that ever came to count so, looked for once.
	fn counting_restorers(&self) -> Result<Vec<(ChangeId, Restores)>, LogError> {
		let known = self.restoring.borrow().clone();
		let restoring = if let Some(restoring) = known {
			restoring
		} else {
			let mut restoring = BTreeSet::new();
			for id in self.read(self.history.ids())? {
				if self
					.read(self.history.change(id))?
					.is_some_and(|change| !change.restores().is_empty())
				{
					restoring.insert(id);
				}
			}
			*self.restoring.borrow_mut() = Some(restoring.clone());
			restoring
		};
		let mut counting = Vec::new();
		for id in restoring {
			if self.read(self.history.status(id))? == Some(Status::SetAside) {
				continue;
			}
			if let Some(change) = self.read(self.history.change(id))? {
				counting.push((id, change.restores().clone()));
			}
		}
		Ok(counting)
	}

	/// Destroys the record key of a secret statement, and remembers it, so that it is
	/// ignored if it arrives again; what it was is kept, if the log keeps what purges destroy.
	pub(super) fn destroy_statement(
		&mut self,
		statement: StatementId,
		by: ChangeId,
	) -> Result<(), LogError> {
		let purged = self.history.purge_statement(statement);
		self.read(purged)?;
		let wrapped = self.wrapped.remove(&statement);
		if self.keeping
			&& let Some(wrapped) = wrapped
		{
			self.destroyed
				.kept
				.push((by, Kept::RecordKeys(statement, wrapped)));
		}
		self.state.storage_mut().forget(statement);
		self.destroyed.statements.push(statement);
		Ok(())
	}

	/// Records which accepted change holds each statement that a change seals, counting or
	/// not: of several, one that counts comes first, so that a purge of a statement is
	/// decided alike whether its change counts or not.
	fn hold_sealed(&mut self, change: &Change) -> Result<(), LogError> {
		let mut moved = Vec::new();
		for &statement in change.sealed().keys() {
			if self.hold(statement, change.id())? {
				moved.push(statement);
			}
		}
		// Statements opened from another change than the one that now holds them are
		// opened again, once this change can be read.
		for &statement in &moved {
			self.state.storage_mut().forget(statement);
		}
		Ok(())
	}

	/// Destroys the key and the payload of a change, and remembers it, so that they are
	/// ignored if they arrive again. The statements it sealed are forgotten, and are held by
	/// the purge that states them again, if one does. If the state holds its operations, it
	/// is stale.
	pub(super) fn destroy_change(&mut self, id: ChangeId, by: ChangeId) -> Result<(), LogError> {
		if self.read(self.history.is_change_purged(id))? {
			return Ok(());
		}
		let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Ok(());
		};
		self.destroyed.changes = true;
		if self.keeping && (accepted.payload.is_some() || !accepted.keys.is_empty()) {
			self.destroyed
				.kept
				.push((by, Kept::Change(Box::new(parcel_of(&accepted)))));
		}
		if accepted.status == Status::Applied {
			self.stale = true;
		}
		let purged = self.history.purge_change(id);
		self.read(purged)?;
		for statement in accepted.change.sealed().keys() {
			self.state.storage_mut().forget(*statement);
		}
		Ok(())
	}

	/// Builds the state again in `fresh`, an empty storage, from the operations of every
	/// change that adds them, after a purge took some away, and opens again the secret
	/// statements that `keys` open. The operations that the state took in are not among
	/// those that [`Log::take_applied`] gives.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read, or the storage written,
	/// also if that failed before: a fresh storage does not mend the history, which only
	/// [`Log::reset`] empties.
	pub fn rebuild(&mut self, fresh: S, keys: &SpaceKeys) -> Result<(), LogError> {
		self.check_storage()?;
		*self.state.storage_mut().ordinary_mut() = fresh;
		self.state.storage_mut().forget_all();
		self.stale = false;
		self.covered.clear();
		for id in self.read(self.history.ids())? {
			let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
				continue;
			};
			if accepted.status != Status::Applied {
				continue;
			}
			let Ok(Some(payload)) = read_payload(&accepted, keys) else {
				continue;
			};
			self.apply_payload(id, accepted.change.timestamp(), &payload)?;
		}
		self.unlock_secrets(keys)?;
		self.check_storage()
	}

	/// Resumes the space's clock from the latest time of the changes that the history holds,
	/// as a log that reads its history from disk does not take them back one by one.
	pub fn resume_clock(&mut self, latest: Timestamp) {
		let _ = self.clock.observe(latest, u64::MAX);
	}

	/// Applies again the operations of an accepted change whose history says they are
	/// applied, to a storage of the state that lacks them, such as one that a crash left
	/// behind the history, or one made anew. Nothing is decided again: what was applied is
	/// applied. They are among those that [`Log::take_applied`] gives.
	///
	/// # Errors
	///
	/// Returns [`LogError::Storage`] if the history cannot be read or the storage written,
	/// and an error if the payload no longer opens with `keys`.
	pub fn reapply(&mut self, id: ChangeId, keys: &SpaceKeys) -> Result<(), LogError> {
		self.check_storage()?;
		let Some(accepted) = self.read(self.history.get(id))?.map(Cow::into_owned) else {
			return Ok(());
		};
		if accepted.status != Status::Applied {
			return Ok(());
		}
		// A payload that no longer opens with `keys` adds nothing here, as when building
		// again.
		let Ok(Some(payload)) = read_payload(&accepted, keys) else {
			return Ok(());
		};
		self.apply_payload(id, accepted.change.timestamp(), &payload)?;
		self.applied.push((id, payload));
		self.check_storage()
	}

	/// Whether a purge left the state stale, to be built again by the caller.
	#[must_use]
	pub const fn is_stale(&self) -> bool {
		self.stale
	}

	/// Whether a purge left the state stale, which the caller then builds again, as it is
	/// in storage that the log cannot make anew; the log forgets that it was.
	pub fn take_stale(&mut self) -> bool {
		std::mem::take(&mut self.stale)
	}

	/// Applies the operations of a change's payload, its own and those it states again,
	/// unless writing to the storage failed before.
	fn apply_payload(
		&mut self,
		id: ChangeId,
		timestamp: Timestamp,
		payload: &Payload,
	) -> Result<(), LogError> {
		let own = Origin::new(timestamp, Some(*id.hash()));
		let mut origins: Vec<(Origin, &Operation)> = payload
			.operations
			.iter()
			.map(|operation| (own, operation))
			.collect();
		for (source, restated) in &payload.restated {
			let Some(at) = self
				.read(self.history.change(*source))?
				.map(|source| source.timestamp())
			else {
				continue;
			};
			let origin = Origin::restated(at, timestamp, Some(*id.hash()));
			origins.extend(
				restated
					.operations
					.iter()
					.map(|operation| (origin, operation)),
			);
		}
		for (origin, operation) in origins {
			self.check_storage()?;
			let applied = self.state.apply_from(operation, origin);
			self.read(applied)?;
		}
		Ok(())
	}

	/// Applies the operations of an accepted change, if it has not added everything yet and
	/// its payload and a good key are there, and opens the secret statements it holds.
	/// Wrapped keys that turn out not to be the change's are dropped. A payload that is the
	/// change's but does not open, or does not have the form it must, adds nothing, for
	/// good.
	fn complete(&mut self, id: ChangeId, keys: &SpaceKeys) -> Result<(), LogError> {
		self.complete_with(id, keys, true)
	}

	/// Applies a governing change that counts, as [`Log::complete`] does, without deciding
	/// again which changes count.
	fn complete_now(&mut self, id: ChangeId, keys: &SpaceKeys) -> Result<(), LogError> {
		self.complete_with(id, keys, false)
	}

	/// Applies an accepted change, as [`Log::complete`] says. A governing change, in a space
	/// whose first change governs it, has every change decided again unless `deciding` is
	/// `false`, which then decides whether it counts.
	fn complete_with(
		&mut self,
		id: ChangeId,
		keys: &SpaceKeys,
		deciding: bool,
	) -> Result<(), LogError> {
		let Some(accepted) = self.read(self.history.get(id))? else {
			return Ok(());
		};
		if accepted.status != Status::Pending || accepted.payload.is_none() {
			return Ok(());
		}
		let mut accepted = accepted.into_owned();
		let before = accepted.keys.len();
		let opened = open_payload(&mut accepted, keys);
		let payload = match opened {
			// Only keys that turned out not to be the change's are dropped.
			Opened::NotYet if accepted.keys.len() == before => return Ok(()),
			Opened::NotYet => return self.store(&accepted),
			Opened::Nothing => {
				accepted.status = Status::Ignored;
				return self.store(&accepted);
			}
			Opened::Payload(payload) => payload,
		};
		let fits = fits_its_kind(&accepted.change, &payload);
		// What it states again must come from changes it builds on, whose times it takes.
		let mut restates_its_ancestors = true;
		for &source in payload.restated.keys() {
			if !self.is_ancestor(source, &accepted.change)? {
				restates_its_ancestors = false;
				break;
			}
		}
		if !(fits && restates_its_ancestors) {
			accepted.status = Status::Ignored;
			return self.store(&accepted);
		}
		// An AI agent's change counts only if it writes what an AI agent may; no change counts
		// that writes plainly what its author knew to be secret.
		if accepted.change.kind() == Kind::Ordinary {
			let set_aside = {
				let governance = self.governance(keys)?;
				let mut failed = None;
				// In a space with members, what its author's role allows of what it writes.
				let by_role = governance.view().is_none()
					|| self.counts_now(&accepted.change, Some(&payload), &governance, |other| {
						self.had(id, other).unwrap_or_else(|error| {
							failed = Some(error);
							false
						})
					})?;
				if let Some(error) = failed {
					return Err(error);
				}
				!by_role
					|| (governance.is_ai_agent(accepted.change.author().as_bytes())
						&& !fits_an_ai_agent(&payload))
					|| self.writes_secret_plainly(&accepted.change, &payload, &governance)?
			};
			if set_aside {
				if self.covered.remove(&id) {
					self.stale = true;
				}
				accepted.status = Status::SetAside;
				self.store(&accepted)?;
				// What its header purged was taken in as it was accepted: an AI agent's change
				// never purges, but this one may have.
				let header = &accepted.change;
				if header.purges_or_restores() || !header.sealed().is_empty() {
					self.recount_purges_and_decide(keys)?;
				}
				return Ok(());
			}
		}
		// A governing change without parents makes the space governed, if it was not.
		if deciding
			&& accepted.change.kind() == Kind::Governing
			&& (accepted.change.parents().is_empty() || self.governance(keys)?.governed())
		{
			// It can be read now: which changes count is decided again, this one included.
			self.store(&accepted)?;
			return self.govern(keys);
		}
		// The history says it is applied once the state holds its operations.
		let covered = self.covered.remove(&id);
		if !covered {
			self.apply_payload(id, accepted.change.timestamp(), &payload)?;
		}
		accepted.status = Status::Applied;
		self.store(&accepted)?;
		let mut held_here = Vec::new();
		for (&statement, _) in payload.all_sealed() {
			if self.read(self.history.holder(statement))? == Some(id) {
				held_here.push(statement);
			}
		}
		// The change's own payload is at hand for opening its sealed statements.
		let mut payloads = BTreeMap::from([(id, payload)]);
		for statement in held_here {
			self.open(statement, keys, &mut payloads)?;
		}
		if !covered && let Some(payload) = payloads.remove(&id) {
			self.applied.push((id, payload));
		}
		Ok(())
	}

	/// Makes a change that purges, and states again what `restating` holds, and accepts it.
	fn commit_purges(
		&mut self,
		author: &SigningKey,
		keys: &SpaceKeys,
		purges: Purges,
		restating: Restating,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Parcel, LogError> {
		self.check_storage()?;
		self.applied.clear();
		self.destroyed = Destroyed::default();
		let heads = self.read(self.history.heads())?;
		let timestamp = self.clock.now(wall_millis)?;
		let mut draft = Draft::new(heads, timestamp, restating.payload);
		draft.purges = purges;
		draft.record_keys = restating.record_keys;
		let parcel = Change::create(self.space, keys, author, &draft, rng);
		self.accept(held(parcel.clone()), keys)?;
		self.check_storage()?;
		Ok(parcel)
	}

	/// Opens a sealed blob's key, if `keys` hold the space key that one of its wrapped keys
	/// needs, and for a secret blob key the secrets key too; a secret blob key takes the
	/// place of an ordinary one opened before, as [`Log::receive_blob_key`] says, and its
	/// ordinary keys go. Wrapped keys that do not open with the keys they name are dropped,
	/// also those after the one that opened.
	fn open_blob_key(&mut self, sealed: Hash, keys: &SpaceKeys) {
		if self.secret_blobs.contains(&sealed) {
			return;
		}
		let space = self.space;
		let Some(candidates) = self.wrapped_blob_keys.get(&sealed) else {
			return;
		};
		let opened_ordinarily = self.blob_keys.contains_key(&sealed);
		if opened_ordinarily && candidates.len() < 2 {
			return;
		}
		// What each wrapped key gives: `None` if the replica lacks its space key.
		let results: Vec<Option<Option<Unwrapped>>> = candidates
			.iter()
			.map(|candidate| {
				keys.get(candidate.epoch())
					.map(|_| candidate.unwrap(space, &sealed, keys))
			})
			.collect();
		let secret_opened = results
			.iter()
			.any(|result| matches!(result, Some(Some(Unwrapped::Key { secret: true, .. }))));
		// Those that do not open go; so do ordinary ones once a secret one opens, as only a
		// replica that holds the secrets can make one, and the blob is a secret one.
		let keep: Vec<bool> = results
			.iter()
			.map(|result| match result {
				None | Some(Some(Unwrapped::Secret)) => true,
				Some(None) => false,
				Some(Some(Unwrapped::Key { secret, .. })) => *secret || !secret_opened,
			})
			.collect();
		let secret_form = results.iter().any(|result| {
			matches!(
				result,
				Some(Some(
					Unwrapped::Secret | Unwrapped::Key { secret: true, .. }
				))
			)
		});
		let unchecked = results.iter().any(Option::is_none);
		let mut opened: Option<((Hash, Key), bool)> = None;
		for result in results {
			if let Some(Some(Unwrapped::Key {
				content,
				key,
				secret,
			})) = result
				&& opened.as_ref().is_none_or(|(_, taken)| secret && !taken)
			{
				opened = Some(((content, key), secret));
			}
		}
		let candidates = self
			.wrapped_blob_keys
			.get_mut(&sealed)
			.expect("its keys were just read");
		let mut index = 0;
		candidates.retain(|_| {
			index += 1;
			keep[index - 1]
		});
		if candidates.is_empty() {
			self.wrapped_blob_keys.remove(&sealed);
		}
		// One the replica cannot check yet may still be a secret one.
		if secret_form {
			self.secret_candidates.insert(sealed);
		} else if !unchecked {
			self.secret_candidates.remove(&sealed);
		}
		if let Some((opened, secret)) = opened {
			self.prefer_blob_key(sealed, opened, secret);
		}
	}

	/// Destroys the key of a sealed blob, and remembers it, so that it is ignored if it
	/// arrives again; what it was is kept, if the log keeps what purges destroy.
	pub(super) fn destroy_blob_key(&mut self, sealed: Hash, by: ChangeId) -> Result<(), LogError> {
		let purged = self.history.purge_blob(sealed);
		self.read(purged)?;
		let wrapped = self.wrapped_blob_keys.remove(&sealed);
		if self.keeping
			&& let Some(wrapped) = wrapped
		{
			self.destroyed
				.kept
				.push((by, Kept::BlobKeys(sealed, wrapped)));
		}
		self.secret_candidates.remove(&sealed);
		self.forget_blob_key(&sealed);
		self.destroyed.blobs.push(sealed);
		Ok(())
	}

	/// Records that a change holds a sealed statement. Of several changes that hold one,
	/// which only a faulty replica makes, the earliest counts, and of equally early ones the
	/// smallest. Returns whether the holder changed.
	fn hold(&mut self, statement: StatementId, change: ChangeId) -> Result<bool, LogError> {
		// A change that holds it and was not purged comes first.
		let order = |log: &Self, id: ChangeId| -> Result<_, LogError> {
			Ok((
				log.read(log.history.is_change_purged(id))?
					|| log.read(log.history.status(id))? == Some(Status::SetAside),
				log.read(log.history.change(id))?
					.map(|change| change.timestamp()),
				id,
			))
		};
		if let Some(holder) = self.read(self.history.holder(statement))?
			&& order(self, holder)? <= order(self, change)?
		{
			return Ok(false);
		}
		let held = self.history.set_holder(statement, change);
		self.read(held)?;
		Ok(true)
	}

	/// Opens a sealed statement and applies it, if this replica has the change that holds
	/// it with its payload and key, a good wrapped record key and the secrets key. Wrapped
	/// record keys that do not open with the secrets key they name, or are not the keys
	/// whose hashes the holder's header holds, are dropped.
	///
	/// `payloads` holds the decrypted payloads of changes that hold sealed statements, which
	/// it takes in as it reads them, so that a change is decrypted once for many statements.
	fn open(
		&mut self,
		id: StatementId,
		keys: &SpaceKeys,
		payloads: &mut BTreeMap<ChangeId, Payload>,
	) -> Result<(), LogError> {
		if self.state.storage().is_open(id) || self.read(self.history.is_statement_purged(id))? {
			return Ok(());
		}
		let Some(holder) = self.read(self.history.holder(id))? else {
			return Ok(());
		};
		let Some(accepted) = self.read(self.history.get(holder))?.map(Cow::into_owned) else {
			return Ok(());
		};
		// A payload that adds nothing reveals nothing either.
		if accepted.status != Status::Applied {
			return Ok(());
		}
		let Some(&expected) = accepted.change.sealed().get(&id) else {
			return Ok(());
		};
		let Some(candidates) = self.wrapped.get_mut(&id) else {
			return Ok(());
		};
		// A key that does not open is dropped. One that opens to another hash than the
		// holder's is kept, and not used: another change may seal the statement too, as
		// devices that run one migration at once do, and come to hold it, as an earlier one
		// arrives.
		let mut record_key = None;
		candidates.retain(|candidate| {
			if record_key.is_some() || keys.secrets(candidate.epoch()).is_none() {
				return true;
			}
			let Some(key) = candidate.unwrap(self.space, id, keys) else {
				return false;
			};
			if key_hash(&key) == expected {
				record_key = Some(key);
			}
			true
		});
		let Some(record_key) = record_key else {
			return Ok(());
		};
		let timestamp = accepted.change.timestamp();
		let hash = *holder.hash();
		// The payload is read again, as the log does not keep it opened.
		let payload = match payloads.entry(holder) {
			btree_map::Entry::Occupied(known) => known.into_mut(),
			btree_map::Entry::Vacant(unknown) => {
				let Ok(Some(payload)) = read_payload(&accepted, keys) else {
					return Ok(());
				};
				unknown.insert(payload)
			}
		};
		// A statement that a purge states again counts from the change it came from.
		let (envelope, origin) = if let Some(envelope) = payload.sealed.get(&id) {
			(Some(envelope), Origin::new(timestamp, Some(hash)))
		} else {
			let Some((&source, restated)) = payload
				.restated
				.iter()
				.find(|(_, restated)| restated.sealed.contains_key(&id))
			else {
				return Ok(());
			};
			let Some(at) = self
				.read(self.history.change(source))?
				.map(|source| source.timestamp())
			else {
				return Ok(());
			};
			let origin = Origin::restated(at, timestamp, Some(hash));
			(restated.sealed.get(&id), origin)
		};
		let Some(statement) =
			envelope.and_then(|envelope| open_sealed(self.space, id, envelope, &record_key))
		else {
			return Ok(());
		};
		self.state.storage_mut().open(&statement, origin);
		Ok(())
	}
}

/// Keeps a wrapped key of a change among its candidates, as [`Log::receive_change_key`]
/// says. Returns whether it was kept.
fn keep_key(
	change: &Change,
	candidates: &mut Vec<WrappedChangeKey>,
	key: WrappedChangeKey,
	keys: &SpaceKeys,
) -> Result<bool, LogError> {
	if candidates.contains(&key) {
		return Ok(false);
	}
	if keys.get(change.epoch()).is_some() {
		if key.unwrap(change, keys).is_none() {
			return Err(LogError::OtherKey);
		}
		if candidates
			.iter()
			.any(|known| known.unwrap(change, keys).is_some())
		{
			return Ok(false);
		}
		*candidates = vec![key];
	} else if candidates.len() < WRAPPED_KEYS_PER_CHANGE {
		candidates.push(key);
	} else {
		return Ok(false);
	}
	Ok(true)
}

/// An operation of a purged change, as the purge states it again: without the assertions
/// of the statements it purges, whose replacing of other statements stays as a retraction.
fn without(operation: Operation, purged: &BTreeSet<StatementId>) -> Option<Operation> {
	match operation {
		Operation::Assert {
			statement,
			supersedes,
			any_entity,
		} if purged.contains(&statement.id) => (!supersedes.is_empty()).then(|| Operation::Retract {
			entity: (!any_entity).then_some(statement.entity),
			statements: supersedes,
		}),
		other => Some(other),
	}
}

/// The secret statements that a change being made takes away: the secret statements it seals,
/// with their entities, and those it takes away so far.
struct TakingSecrets {
	sealing: BTreeMap<StatementId, EntityId>,
	taken: BTreeSet<StatementId>,
}

/// What a purge states again: its payload, and the hashes of the record keys of the
/// statements it seals again.
#[derive(Default)]
struct Restating {
	payload: Payload,
	record_keys: BTreeMap<StatementId, Hash>,
}

/// A change as the log first holds it, with the parts that came with it.
fn held(parcel: Parcel) -> Held {
	Held {
		change: parcel.change,
		payload: parcel.payload,
		keys: parcel.keys,
	}
}

/// Whether operations set a field that governs the space.
fn holds_governing(operations: &[Operation]) -> bool {
	operations.iter().any(|operation| {
		matches!(operation, Operation::Assert { statement, .. } if fields::is_governing(&statement.field))
	})
}

/// Whether a payload holds only what an AI agent may write: statements that
/// [`fields::ai_agent_may_write`] allows, and retractions, but of secret statements, with
/// nothing sealed or stated again. An AI agent, which reads no secrets, takes none away.
fn fits_an_ai_agent(payload: &Payload) -> bool {
	payload.sealed.is_empty()
		&& payload.restated.is_empty()
		&& payload.operations.iter().all(|operation| match operation {
			Operation::Assert { statement, .. } => fields::ai_agent_may_write(statement),
			Operation::Retract { .. } => true,
			Operation::RetractSecret { .. } | Operation::Unknown(_) => false,
		})
}

/// Whether operations only set fields that govern the space, or retract statements, but
/// secret ones, as no statement that governs is secret. A field of NibKeep's own that this
/// version does not know may govern in a later version: its statements are kept, with no
/// effect here.
fn holds_only_governing(operations: &[Operation]) -> bool {
	operations.iter().all(|operation| match operation {
		Operation::Assert { statement, .. } => {
			fields::is_governing(&statement.field) || fields::is_later_governing(&statement.field)
		}
		Operation::Retract { .. } => true,
		Operation::RetractSecret { .. } | Operation::Unknown(_) => false,
	})
}

#[cfg(test)]
mod tests {
	use nibkeep_model::{Statement, Value};
	use nibkeep_testkit::TestRng;
	use nibkeep_time::Timestamp;

	use super::*;

	/// A change whose payload does not fit its kind seals a statement: whenever its record
	/// key arrives, before or after the change, and also when secrets are unlocked, the
	/// statement stays sealed, as the payload adds nothing.
	#[test]
	fn a_payload_that_adds_nothing_reveals_no_secret() {
		let mut rng = TestRng::new("ignored secrets");
		let space = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		let author = rng.signing_key();
		let entity = rng.entity_id();
		let secret = Statement::new(
			rng.statement_id(),
			entity,
			"local.password".parse().expect("a field name"),
			Value::Text("hunter2".to_owned()),
		);
		let record_key = Key::generate(&mut rng);
		let key = Statement::new(
			rng.statement_id(),
			entity,
			fields::SIGNING_KEY,
			Value::Text("a key".to_owned()),
		);
		// An ordinary change that sets a replica's key, which it must not.
		let mut payload = Payload::from(vec![Operation::assert(key)]);
		payload
			.sealed
			.insert(secret.id, seal(space, &secret, &record_key, &mut rng));
		let mut draft = Draft::new([], Timestamp::new(1_800_000_000_000, 0), payload);
		draft.record_keys.insert(secret.id, key_hash(&record_key));
		let parcel = Change::create(space, &keys, &author, &draft, &mut rng);
		let wrapped = WrappedKey::wrap(
			space,
			secret.id,
			&record_key,
			keys.newest_secrets().expect("a secrets key"),
			&mut rng,
		);

		let mut key_first = Log::new(space, Clock::new(3_600_000));
		key_first
			.receive_wrapped_key(secret.id, wrapped.clone(), &keys)
			.expect("keeps the key");
		key_first
			.receive(parcel.clone(), &keys, 1_800_000_000_000)
			.expect("accepts");
		let mut key_after = Log::new(space, Clock::new(3_600_000));
		key_after
			.receive(parcel, &keys, 1_800_000_000_000)
			.expect("accepts");
		key_after
			.receive_wrapped_key(secret.id, wrapped, &keys)
			.expect("keeps the key");
		key_after.unlock_secrets(&keys).expect("unlocks");
		for log in [&key_first, &key_after] {
			assert!(log.is_sealed(secret.id).expect("reads"));
			assert!(!log.state().storage().is_open(secret.id));
			assert_eq!(log.state().statements().expect("reads").len(), 0);
		}
	}
}
