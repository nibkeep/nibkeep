//! The end-to-end tests of `nib` ([technical design: end-to-end tests]), one module for each story, in one
//! executable, so that `nib` is built once and the stories run side by side.
//!
//! [technical design: end-to-end tests]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#18-end-to-end-tests

// SNAPSHOTS=overwrite writes every snapshot as a raw string with hashes, which one that holds
// a quote needs.
#![allow(clippy::needless_raw_string_hashes)]

mod ai_agent;
mod common;
mod first_steps;
mod interrupted;
mod killed;
mod lost_phone;
mod planner;
// It types at a terminal, which the tests give a program only on Unix.
#[cfg(unix)]
mod recovered;
mod restored;
mod shared_space;
mod trees;
mod two_devices;
