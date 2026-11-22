//! What handling passwords takes, which `nib` and apps share: the password generator,
//! one-time codes, and copying secrets to the clipboard. Logins and folders are in the
//! standard vocabulary.
//!
//! Nothing here keeps a secret longer than it is asked to: generated passwords and codes are
//! wiped when dropped, and the clipboard remembers a copied secret only by a keyed hash.

mod base32;
mod clipboard;
mod generator;
mod one_time;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use clipboard::SystemBoard;
pub use clipboard::{Board, CLEAR_AFTER, Clipboard, ClipboardError};
pub use generator::{GeneratorError, MAX_LENGTH, MIN_LENGTH, Recipe, generate};
pub use one_time::{Algorithm, OneTimeCode, OneTimeError, hotp};
