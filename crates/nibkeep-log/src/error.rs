//! Why changes are rejected.

use nibkeep_base::cbor;
use nibkeep_time::ClockError;

/// Why a change could not be made or was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LogError {
	/// The change belongs to another space.
	#[error("the change belongs to another space")]
	OtherSpace,
	/// The change's signature is not valid for its author.
	#[error("the change's signature is not valid")]
	InvalidSignature,
	/// The change's key is wrapped with a space key that this replica does not have.
	#[error(
		"the change's key is wrapped with key {0} of the space, which this device does not have"
	)]
	UnknownEpoch(u64),
	/// The change's payload does not open with the change's key.
	#[error("the change's operations cannot be decrypted")]
	Decryption,
	/// The change's payload does not have the form of a payload, or does not seal the
	/// statements that the change's header names.
	#[error("the change's operations cannot be read: {0}")]
	Operations(cbor::Error),
	/// The sealed payload is not the one whose hash the change's header holds.
	#[error("the payload is not the change's")]
	OtherPayload,
	/// The wrapped key does not open to the key whose hash the change's header holds.
	#[error("the wrapped key is not the change's")]
	OtherKey,
	/// The replica does not have the change that a payload or key belongs to.
	#[error("this device does not have the change")]
	UnknownChange,
	/// A change that sets the keys of replicas would hold something else too.
	#[error("a change that sets the keys of devices holds nothing else")]
	MixedGoverning,
	/// A change that governs the space is wrapped with another key than the space's first,
	/// which every replica holds, so that some replicas could not read it.
	#[error("a change that sets the keys of devices is wrapped with another key than the first")]
	GoverningEpoch,
	/// The change is not later than one of its parents.
	#[error("the change is not later than its parents")]
	NotAfterParents,
	/// One of the change's parents, or one of their ancestors, was rejected.
	#[error("one of the change's parents was rejected")]
	RejectedParent,
	/// A change without parents that is not the space's first change, which the replica
	/// was told.
	#[error("the change is not the space's first change")]
	UnexpectedRoot,
	/// A change without parents, in a space whose first change governs it and that has one
	/// already.
	#[error("the space has a first change already")]
	SecondRoot,
	/// A change without parents that does not govern the space, in a space whose first change
	/// must govern it.
	#[error("the space's first change must say who may write to it")]
	UngovernedRoot,
	/// No governing change of the space lists the key that signed the change, in a space
	/// whose first change governs it. The change is not kept, until such a listing arrives.
	#[error("no device of the space has the key that signed the change")]
	UnknownAuthor,
	/// Too many changes are already waiting for their parents.
	#[error("too many changes are waiting for their parents")]
	TooManyWaiting,
	/// Writing or purging a secret value needs the space's secrets key, or the opened
	/// value.
	#[error("secret values need the space's secrets key")]
	SecretsLocked,
	/// A statement is not one that this replica can purge: it does not have it, or it was
	/// asserted in a change that governs the space, or whose payload it cannot read.
	#[error("the value cannot be purged here")]
	CannotPurge,
	/// This replica has no key for the blob to purge.
	#[error("this device has no key for the blob")]
	UnknownBlob,
	/// The change purges or restores a change that it does not build on, or one that governs
	/// the space, or a statement that is not sealed in a change it builds on; or it restores
	/// and purges at once.
	#[error("the change purges or restores what it does not build on, or what governs the space")]
	InvalidPurge,
	/// A change that restores what purges destroyed would not count: its author's role does
	/// not allow purging what it restores.
	#[error("this replica's role does not allow restoring what the purge destroyed")]
	CannotRestore,
	/// The storage of the state could not be read or written, so the log no longer matches
	/// it and must be built again. [`Log::storage_error`](crate::Log::storage_error) says why.
	#[error("the state could not be read or written; the log must be built again")]
	Storage,
	/// A page of a replica's account of what it holds does not read
	/// ([`AccountPage`](crate::AccountPage)).
	#[error("an account of copies does not read: {0}")]
	Account(&'static str),
	/// The change's timestamp is too far ahead of the wall clock, or the clock has run out
	/// of timestamps.
	#[error(transparent)]
	Clock(#[from] ClockError),
}
