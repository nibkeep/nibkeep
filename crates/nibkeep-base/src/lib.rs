//! Identifiers, hashes and the canonical CBOR encoding of NibKeep.
//!
//! Everything NibKeep stores, hashes, signs or syncs is encoded with [`cbor`], a strict
//! subset of CBOR in which every value has exactly one encoding. Content is identified by
//! its [`Hash`](struct@Hash), and entities, spaces and people by random identifiers such as
//! [`EntityId`]. The formats are specified in `docs/spec/encoding.md`.
//!
//! This crate does no I/O. In particular, it never reads the operating system's random
//! number generator: callers pass in the random bytes for new identifiers.

pub mod cbor;
mod hash;
mod id;
pub mod small;

pub use hash::{ChangeId, Hash, HashAlgorithm, Hasher, ParseHashError};
pub use id::{EntityId, ParseIdError, PersonId, SpaceId, StatementId};
