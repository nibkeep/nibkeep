//! Copying secrets to the clipboard: marked as sensitive, so that clipboard histories and
//! cloud clipboards leave them out, and cleared after a while if the clipboard still holds
//! them, so that whatever a person copied since stays. Waiting is the caller's: an app has
//! its own timers, and `nib` its own process.

use std::fmt;
use std::time::Duration;

use rand_core::CryptoRng;
use zeroize::Zeroizing;

/// How long a copied secret stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(30);

/// A clipboard, as a platform offers it.
pub trait Board {
	/// Puts text on the clipboard; if `sensitive`, marked so that histories, cloud clipboards
	/// and the clipboard's previews leave it out, where the platform knows such a mark.
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be written.
	fn copy(&mut self, text: &str, sensitive: bool) -> Result<(), ClipboardError>;

	/// The text on the clipboard, if it holds text.
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be read.
	fn text(&mut self) -> Result<Option<Zeroizing<String>>, ClipboardError>;

	/// Empties the clipboard.
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be written.
	fn clear(&mut self) -> Result<(), ClipboardError>;
}

impl<B: Board + ?Sized> Board for Box<B> {
	fn copy(&mut self, text: &str, sensitive: bool) -> Result<(), ClipboardError> {
		(**self).copy(text, sensitive)
	}

	fn text(&mut self) -> Result<Option<Zeroizing<String>>, ClipboardError> {
		(**self).text()
	}

	fn clear(&mut self) -> Result<(), ClipboardError> {
		(**self).clear()
	}
}

/// A clipboard that failed, with the platform's reason.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the clipboard failed: {0}")]
pub struct ClipboardError(String);

impl ClipboardError {
	/// A failure, for the platform's reason.
	#[must_use]
	pub fn new(reason: impl Into<String>) -> Self {
		Self(reason.into())
	}
}

/// A clipboard that knows whether it still holds the secret it copied last. It keeps no copy
/// of the secret, only a hash of it under a key of its own.
pub struct Clipboard<B> {
	board: B,
	key: Zeroizing<[u8; 32]>,
	copied: Option<blake3::Hash>,
}

impl<B> fmt::Debug for Clipboard<B> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Clipboard")
			.field("holds_secret", &self.copied.is_some())
			.finish_non_exhaustive()
	}
}

impl<B: Board> Clipboard<B> {
	/// The clipboard of `board`, with a new key for the hashes of what it copies.
	pub fn new(board: B, rng: &mut impl CryptoRng) -> Self {
		let mut key = Zeroizing::new([0; 32]);
		rng.fill_bytes(key.as_mut());
		Self {
			board,
			key,
			copied: None,
		}
	}

	/// Copies a secret, marked as sensitive. Clear it with [`Clipboard::clear_if_ours`] after
	/// [`CLEAR_AFTER`].
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be written.
	pub fn copy_secret(&mut self, secret: &str) -> Result<(), ClipboardError> {
		// A copy that fails leaves what was copied before, which is still to clear.
		self.board.copy(secret, true)?;
		self.copied = Some(self.hash(secret));
		Ok(())
	}

	/// Copies text that is not secret, such as a user name, which stays. It replaces a
	/// secret copied before, which is then no longer there to clear.
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be written.
	pub fn copy(&mut self, text: &str) -> Result<(), ClipboardError> {
		self.board.copy(text, false)?;
		self.copied = None;
		Ok(())
	}

	/// Whether a secret was copied and not cleared since, as far as this clipboard knows.
	#[must_use]
	pub fn holds_secret(&self) -> bool {
		self.copied.is_some()
	}

	/// Clears the clipboard if it still holds the secret copied last; whatever was copied
	/// since stays. Returns whether it cleared. The clipboard holds the secret no longer
	/// either way, as far as this clipboard knows; but nothing changes if it fails.
	///
	/// # Errors
	///
	/// Returns an error if the clipboard cannot be read or written, as when another program
	/// holds it for a moment; asking again may clear it.
	pub fn clear_if_ours(&mut self) -> Result<bool, ClipboardError> {
		let Some(copied) = self.copied else {
			return Ok(false);
		};
		let holds = self
			.board
			.text()?
			.is_some_and(|text| self.hash(&text) == copied);
		if holds {
			self.board.clear()?;
		}
		self.copied = None;
		Ok(holds)
	}

	/// The platform's clipboard underneath.
	pub fn board(&mut self) -> &mut B {
		&mut self.board
	}

	fn hash(&self, text: &str) -> blake3::Hash {
		blake3::keyed_hash(&self.key, text.as_bytes())
	}
}

/// The clipboard of a computer: Linux (Wayland, or X11), Windows and macOS.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct SystemBoard(arboard::Clipboard);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl SystemBoard {
	/// The computer's clipboard.
	///
	/// # Errors
	///
	/// Returns an error if there is none, such as on Linux without a display.
	pub fn new() -> Result<Self, ClipboardError> {
		arboard::Clipboard::new()
			.map(Self)
			.map_err(|error| ClipboardError::new(error.to_string()))
	}
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Board for SystemBoard {
	fn copy(&mut self, text: &str, sensitive: bool) -> Result<(), ClipboardError> {
		#[cfg_attr(
			not(any(target_os = "linux", target_os = "windows", target_os = "macos")),
			expect(unused_mut, reason = "no mark for a sensitive copy here")
		)]
		let mut set = self.0.set();
		if sensitive {
			// KDE's mark, which other Linux clipboard managers read too.
			#[cfg(target_os = "linux")]
			{
				use arboard::SetExtLinux;
				set = set.exclude_from_history();
			}
			#[cfg(target_os = "windows")]
			{
				use arboard::SetExtWindows;
				set = set
					.exclude_from_history()
					.exclude_from_cloud()
					.exclude_from_monitoring();
			}
			// org.nspasteboard.ConcealedType.
			#[cfg(target_os = "macos")]
			{
				use arboard::SetExtApple;
				set = set.exclude_from_history();
			}
		}
		set.text(text)
			.map_err(|error| ClipboardError::new(error.to_string()))
	}

	fn text(&mut self) -> Result<Option<Zeroizing<String>>, ClipboardError> {
		match self.0.get_text() {
			Ok(text) => Ok(Some(Zeroizing::new(text))),
			Err(arboard::Error::ContentNotAvailable) => Ok(None),
			Err(error) => Err(ClipboardError::new(error.to_string())),
		}
	}

	fn clear(&mut self) -> Result<(), ClipboardError> {
		self.0
			.clear()
			.map_err(|error| ClipboardError::new(error.to_string()))
	}
}
