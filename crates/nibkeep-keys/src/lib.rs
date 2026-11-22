//! The keys of a NibKeep replica at rest: one key file with two compartments, the
//! replica's keys and the secrets keys, each unlocked by a passphrase or by the platform's
//! keystore.
//!
//! The app chooses the platform's store with [`keyring_core::set_default_store`] when it
//! starts. The format is specified in `docs/spec/keys.md`.

mod contents;
mod error;
mod file;

pub use contents::{ReplicaKeys, SecretsKeys, SpaceKeyMap};
pub use error::KeysError;
pub use file::{CompartmentKey, KeyFile, NewProtection, Protection, SealedSecrets, Unlocker};
