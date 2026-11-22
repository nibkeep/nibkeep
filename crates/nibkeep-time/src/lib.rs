//! Valid time and system time for NibKeep.
//!
//! - **Valid time,** when something was true, is written in the Extended Date/Time Format:
//!   see [`Edtf`]. [`holds_on`] and [`happened_by`] answer the questions of time travel for
//!   a [`Day`].
//! - **System time,** when something was recorded, comes from a hybrid logical [`Clock`]
//!   as a [`Timestamp`].
//!
//! The formats and rules are specified in `docs/spec/time.md`. This crate does no I/O: the
//! clock is given the wall clock time instead of reading it.

mod clock;
mod day;
mod edtf;
mod presence;

pub use clock::{Clock, ClockError, Timestamp};
pub use day::{Day, ParseDayError};
pub use edtf::{Edge, Edtf, ParseEdtfError, Start};
pub use presence::{Presence, happened_by, holds_on};
