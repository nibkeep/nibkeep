//! What a log's history is kept in: the changes it accepted, with their payloads and keys
//! as far as it has them, and what they say for good, such as which change holds each sealed
//! statement and what was purged. The log works on these lookups alone, so that the same
//! logic reads its history from memory and from the log file on disk ([technical design: the state file]).
//!
//! [technical design: the state file]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#75-the-state-file

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt::Debug;
use std::marker::PhantomData;

use nibkeep_base::{Hash, StatementId};

use crate::{Change, ChangeId, SealedPayload, WrappedChangeKey};

/// What became of an accepted change's payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
	/// Its payload, or a key that opens it, is not there yet: its operations wait.
	Pending,
	/// Its operations are applied.
	Applied,
	/// It adds nothing, for good: its payload does not open with its key, or does not have
	/// the form it must, or the change was purged.
	Ignored,
	/// It does not count, for now: its author is not listed, or was revoked, and not every
	/// replica that revoked it had the change. Its operations are not applied, and what its
	/// header says is not acted on, until it counts.
	SetAside,
}

/// An accepted change, with what the log has of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
	/// The change.
	pub change: Change,
	/// How deep it is in the history: one more than its deepest parent, and 1 for a change
	/// without parents. A change is deeper than all its ancestors, so an ancestry check
	/// looks only at changes deeper than the ancestor it looks for.
	pub depth: u64,
	/// Its sealed payload, if the log has it.
	pub payload: Option<SealedPayload>,
	/// Its wrapped key: one that the log knows to be good, or a few that it cannot check yet.
	pub keys: Vec<WrappedChangeKey>,
	/// What became of its payload.
	pub status: Status,
}

/// What another replica needs to know of an accepted change to tell whether it holds the
/// same: its ID, its timestamp's milliseconds, its depth, and whether the log holds its
/// payload and a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
	/// The change.
	pub id: ChangeId,
	/// The milliseconds of its timestamp.
	pub millis: u64,
	/// Its depth, as [`Accepted::depth`] says.
	pub depth: u64,
	/// Whether the log holds its sealed payload and a wrapped key.
	pub complete: bool,
}

/// The storage of a log's history. Reading and writing may fail where the storage is on
/// disk: [`History::Error`] says why.
pub trait History {
	/// Why reading or writing failed.
	type Error: Debug;

	/// An accepted change, with what the log has of it.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn get(&self, id: ChangeId) -> Result<Option<Cow<'_, Accepted>>, Self::Error>;

	/// An accepted change's header alone.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn change(&self, id: ChangeId) -> Result<Option<Cow<'_, Change>>, Self::Error> {
		Ok(self.get(id)?.map(|accepted| match accepted {
			Cow::Borrowed(accepted) => Cow::Borrowed(&accepted.change),
			Cow::Owned(accepted) => Cow::Owned(accepted.change),
		}))
	}

	/// What became of an accepted change's payload.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn status(&self, id: ChangeId) -> Result<Option<Status>, Self::Error> {
		Ok(self.get(id)?.map(|accepted| accepted.status))
	}

	/// How deep an accepted change is in the history.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn depth(&self, id: ChangeId) -> Result<Option<u64>, Self::Error> {
		Ok(self.get(id)?.map(|accepted| accepted.depth))
	}

	/// Whether a change is accepted.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn contains(&self, id: ChangeId) -> Result<bool, Self::Error> {
		Ok(self.depth(id)?.is_some())
	}

	/// The accepted changes, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn ids(&self) -> Result<Vec<ChangeId>, Self::Error>;

	/// Every accepted change in short, as [`Summary`] says, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn summaries(&self) -> Result<Vec<Summary>, Self::Error> {
		let mut summaries = Vec::new();
		for id in self.ids()? {
			if let Some(accepted) = self.get(id)? {
				summaries.push(Summary {
					id,
					millis: accepted.change.timestamp().millis(),
					depth: accepted.depth,
					complete: accepted.payload.is_some() && !accepted.keys.is_empty(),
				});
			}
		}
		Ok(summaries)
	}

	/// How many changes are accepted.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn count(&self) -> Result<usize, Self::Error>;

	/// The accepted changes that no other names as a parent.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn heads(&self) -> Result<BTreeSet<ChangeId>, Self::Error>;

	/// The accepted changes whose operations wait for their payloads or keys, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn pending(&self) -> Result<Vec<ChangeId>, Self::Error>;

	/// The accepted changes that govern the space, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn governing(&self) -> Result<Vec<ChangeId>, Self::Error> {
		let mut governing = Vec::new();
		for id in self.ids()? {
			if self
				.change(id)?
				.is_some_and(|change| change.kind() == crate::Kind::Governing)
			{
				governing.push(id);
			}
		}
		Ok(governing)
	}

	/// The accepted changes that are set aside, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn set_aside(&self) -> Result<Vec<ChangeId>, Self::Error> {
		let mut set_aside = Vec::new();
		for id in self.ids()? {
			if self
				.get(id)?
				.is_some_and(|accepted| accepted.status == Status::SetAside)
			{
				set_aside.push(id);
			}
		}
		Ok(set_aside)
	}

	/// The accepted changes that purge changes and add their operations, which may state
	/// statements again, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn purging(&self) -> Result<Vec<ChangeId>, Self::Error>;

	/// The accepted change that holds a sealed statement.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn holder(&self, statement: StatementId) -> Result<Option<ChangeId>, Self::Error>;

	/// The sealed statements, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn sealed(&self) -> Result<Vec<StatementId>, Self::Error>;

	/// The changes whose keys and payloads were destroyed, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn purged_changes(&self) -> Result<Vec<ChangeId>, Self::Error>;

	/// Whether a change's key and payload were destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn is_change_purged(&self, id: ChangeId) -> Result<bool, Self::Error>;

	/// Whether a secret statement's record key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn is_statement_purged(&self, statement: StatementId) -> Result<bool, Self::Error>;

	/// The secret statements whose record keys were destroyed, ordered by ID.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn purged_statements(&self) -> Result<Vec<StatementId>, Self::Error>;

	/// The sealed hashes of the blobs whose keys were destroyed, ordered by hash.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn purged_blobs(&self) -> Result<Vec<Hash>, Self::Error>;

	/// Whether a sealed blob's key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be read.
	fn is_blob_purged(&self, sealed: &Hash) -> Result<bool, Self::Error> {
		Ok(self.purged_blobs()?.binary_search(sealed).is_ok())
	}

	/// Keeps an accepted change, which becomes a head in place of its parents.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn add(&mut self, accepted: &Accepted) -> Result<(), Self::Error>;

	/// Keeps what the log now has of an accepted change: its payload and keys, and what
	/// became of its payload.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn update(
		&mut self,
		id: ChangeId,
		payload: Option<&SealedPayload>,
		keys: &[WrappedChangeKey],
		status: Status,
	) -> Result<(), Self::Error>;

	/// Records the change that holds a sealed statement.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn set_holder(&mut self, statement: StatementId, change: ChangeId) -> Result<(), Self::Error>;

	/// Records that a change's key and payload were destroyed, and drops them. The change
	/// then adds nothing, unless it is set aside, which it stays.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn purge_change(&mut self, id: ChangeId) -> Result<(), Self::Error>;

	/// Records that a secret statement's record key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn purge_statement(&mut self, statement: StatementId) -> Result<(), Self::Error>;

	/// Records that a sealed blob's key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn purge_blob(&mut self, sealed: Hash) -> Result<(), Self::Error>;

	/// Forgets that a change's key and payload were destroyed, after the change that
	/// destroyed them was set aside: it is pending again, without them, until another
	/// replica sends them.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn unpurge_change(&mut self, id: ChangeId) -> Result<(), Self::Error>;

	/// Forgets that a secret statement's record key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn unpurge_statement(&mut self, statement: StatementId) -> Result<(), Self::Error>;

	/// Forgets that a sealed blob's key was destroyed.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn unpurge_blob(&mut self, sealed: Hash) -> Result<(), Self::Error>;

	/// Forgets everything, to take the changes back again.
	///
	/// # Errors
	///
	/// Returns an error if the storage cannot be written.
	fn clear(&mut self) -> Result<(), Self::Error>;
}

/// A log's history in memory. It never fails, so it takes the error type of the storage of
/// the log's state, `E`.
#[derive(Debug)]
pub struct MemoryHistory<E = Infallible> {
	accepted: BTreeMap<ChangeId, Accepted>,
	governing: BTreeSet<ChangeId>,
	heads: BTreeSet<ChangeId>,
	holders: BTreeMap<StatementId, ChangeId>,
	purged_changes: BTreeSet<ChangeId>,
	purged_statements: BTreeSet<StatementId>,
	purged_blobs: BTreeSet<Hash>,
	error: PhantomData<fn() -> E>,
}

impl<E> Default for MemoryHistory<E> {
	fn default() -> Self {
		Self {
			accepted: BTreeMap::new(),
			governing: BTreeSet::new(),
			heads: BTreeSet::new(),
			holders: BTreeMap::new(),
			purged_changes: BTreeSet::new(),
			purged_statements: BTreeSet::new(),
			purged_blobs: BTreeSet::new(),
			error: PhantomData,
		}
	}
}

impl<E: Debug> History for MemoryHistory<E> {
	type Error = E;

	fn get(&self, id: ChangeId) -> Result<Option<Cow<'_, Accepted>>, E> {
		Ok(self.accepted.get(&id).map(Cow::Borrowed))
	}

	fn contains(&self, id: ChangeId) -> Result<bool, E> {
		Ok(self.accepted.contains_key(&id))
	}

	fn ids(&self) -> Result<Vec<ChangeId>, E> {
		Ok(self.accepted.keys().copied().collect())
	}

	fn count(&self) -> Result<usize, E> {
		Ok(self.accepted.len())
	}

	fn heads(&self) -> Result<BTreeSet<ChangeId>, E> {
		Ok(self.heads.clone())
	}

	fn pending(&self) -> Result<Vec<ChangeId>, E> {
		Ok(self
			.accepted
			.iter()
			.filter(|(_, accepted)| accepted.status == Status::Pending)
			.map(|(&id, _)| id)
			.collect())
	}

	fn governing(&self) -> Result<Vec<ChangeId>, E> {
		Ok(self.governing.iter().copied().collect())
	}

	fn purging(&self) -> Result<Vec<ChangeId>, E> {
		Ok(self
			.accepted
			.iter()
			.filter(|(_, accepted)| {
				accepted.status == Status::Applied && !accepted.change.purges().changes.is_empty()
			})
			.map(|(&id, _)| id)
			.collect())
	}

	fn holder(&self, statement: StatementId) -> Result<Option<ChangeId>, E> {
		Ok(self.holders.get(&statement).copied())
	}

	fn sealed(&self) -> Result<Vec<StatementId>, E> {
		Ok(self.holders.keys().copied().collect())
	}

	fn purged_changes(&self) -> Result<Vec<ChangeId>, E> {
		Ok(self.purged_changes.iter().copied().collect())
	}

	fn is_change_purged(&self, id: ChangeId) -> Result<bool, E> {
		Ok(self.purged_changes.contains(&id))
	}

	fn is_statement_purged(&self, statement: StatementId) -> Result<bool, E> {
		Ok(self.purged_statements.contains(&statement))
	}

	fn purged_statements(&self) -> Result<Vec<StatementId>, E> {
		Ok(self.purged_statements.iter().copied().collect())
	}

	fn purged_blobs(&self) -> Result<Vec<Hash>, E> {
		Ok(self.purged_blobs.iter().copied().collect())
	}

	fn is_blob_purged(&self, sealed: &Hash) -> Result<bool, E> {
		Ok(self.purged_blobs.contains(sealed))
	}

	fn add(&mut self, accepted: &Accepted) -> Result<(), E> {
		let id = accepted.change.id();
		for parent in accepted.change.parents() {
			self.heads.remove(parent);
		}
		self.heads.insert(id);
		if accepted.change.kind() == crate::Kind::Governing {
			self.governing.insert(id);
		}
		self.accepted.insert(id, accepted.clone());
		Ok(())
	}

	fn update(
		&mut self,
		id: ChangeId,
		payload: Option<&SealedPayload>,
		keys: &[WrappedChangeKey],
		status: Status,
	) -> Result<(), E> {
		if let Some(accepted) = self.accepted.get_mut(&id) {
			accepted.payload = payload.cloned();
			accepted.keys = keys.to_vec();
			accepted.status = status;
		}
		Ok(())
	}

	fn set_holder(&mut self, statement: StatementId, change: ChangeId) -> Result<(), E> {
		self.holders.insert(statement, change);
		Ok(())
	}

	fn purge_change(&mut self, id: ChangeId) -> Result<(), E> {
		self.purged_changes.insert(id);
		if let Some(accepted) = self.accepted.get_mut(&id) {
			accepted.payload = None;
			accepted.keys.clear();
			// A change set aside stays so, to be decided on again.
			if accepted.status != Status::SetAside {
				accepted.status = Status::Ignored;
			}
		}
		Ok(())
	}

	fn purge_statement(&mut self, statement: StatementId) -> Result<(), E> {
		self.purged_statements.insert(statement);
		Ok(())
	}

	fn purge_blob(&mut self, sealed: Hash) -> Result<(), E> {
		self.purged_blobs.insert(sealed);
		Ok(())
	}

	fn unpurge_change(&mut self, id: ChangeId) -> Result<(), E> {
		self.purged_changes.remove(&id);
		if let Some(accepted) = self.accepted.get_mut(&id)
			&& accepted.status == Status::Ignored
		{
			accepted.status = Status::Pending;
		}
		Ok(())
	}

	fn unpurge_statement(&mut self, statement: StatementId) -> Result<(), E> {
		self.purged_statements.remove(&statement);
		Ok(())
	}

	fn unpurge_blob(&mut self, sealed: Hash) -> Result<(), E> {
		self.purged_blobs.remove(&sealed);
		Ok(())
	}

	fn clear(&mut self) -> Result<(), E> {
		*self = Self::default();
		Ok(())
	}
}
