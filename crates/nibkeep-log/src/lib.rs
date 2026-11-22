//! The log of a NibKeep space: signed changes linked by their hashes, with payloads sealed
//! under keys of their own; the current state and the earlier states they build; and
//! ordered lists.
//!
//! The formats and rules are specified in `docs/spec/log.md`.

mod account;
mod change;
mod error;
pub mod history;
mod keys;
pub mod list;
mod log;
mod payload;

pub use account::{AccountPage, BackupHead, DeviceRole, Head, MAX_PAGE_LEN, PageBody, PageKind};
pub use change::{
	Change, ChangeId, Draft, Kind, Parcel, Purges, Restores, SealedPayload, WrappedChangeKey,
};
pub use error::LogError;
pub use history::{Accepted, History, MemoryHistory, Status, Summary};
pub use keys::{SpaceKeys, WrappedBlobKey, WrappedKey};
pub use log::{Destroyed, Forged, Kept, Log, PurgedBy, Received, Review, SpaceState};
pub use payload::{Payload, Restated};
