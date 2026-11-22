//! NibKeep under one name: the engine, and the crates whose types its API speaks in.
//!
//! A program that works with a library can depend on this crate alone. The engine's API is
//! at the root, as [`Library`] is: everything that `nibkeep-node` makes public. Beside it
//! are the crates below the engine whose values its API takes and returns:
//!
//! - [`base`]: identifiers and hashes,
//! - [`model`]: names, values, fields and types,
//! - [`time`]: dates and timestamps,
//! - [`vocabulary`]: the standard vocabulary, which is the crate `nibkeep-std`,
//! - [`crypto`]: keys and passphrases.
//!
//! Nothing here is code of its own, so a value that one program got through this crate is
//! the value that another got from the crate it comes from.
//!
//! ```no_run
//! use std::path::Path;
//!
//! use nibkeep::{Library, Unlocker};
//!
//! let library = Library::open(
//!     Path::new("/home/ada/Library"),
//!     &Unlocker::Passphrase("correct horse"),
//! )?;
//! let personal: nibkeep::base::SpaceId = library.personal_space();
//! let types: nibkeep::model::Types = library.types(personal)?;
//! assert!(types.get(&nibkeep::vocabulary::LOGIN).is_some());
//! # Ok::<(), nibkeep::NodeError>(())
//! ```

pub use nibkeep_base as base;
pub use nibkeep_crypto as crypto;
pub use nibkeep_model as model;
pub use nibkeep_node::*;
pub use nibkeep_std as vocabulary;
pub use nibkeep_time as time;
