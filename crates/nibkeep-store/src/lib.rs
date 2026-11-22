//! Keeps the spaces of a NibKeep replica on disk: the log of each space in an SQLite
//! database, and the sealed blobs in files of their own, which the replica's spaces share.
//! Indexes and search are built from the log in memory. Databases encrypted at rest, for
//! what is derived from the log, go through a VFS of the store's own.
//!
//! The format is specified in `docs/spec/storage.md`.

mod blobs;
mod copy;
mod encrypted;
mod error;
mod file;
mod index;
mod lock;
mod state_file;
mod stored;

pub use blobs::{blob_directory, blobs_starting_with, hash_file, place_in};
pub use copy::{CopiedItem, take_copied_log};
pub use encrypted::EncryptedDatabase;
pub use error::StoreError;
pub use file::{KeptRow, LogFile, PlaceCheck, StoredPage, StoredPageId};
pub use index::{Found, holds_words};
pub use state_file::{Mark, StateFile};
pub use stored::{BlobInventory, SealedCheck, StoredLog, Ticks, log_path};
