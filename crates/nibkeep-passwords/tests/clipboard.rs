//! Copying secrets: marked as sensitive, and cleared only while the clipboard still holds
//! them.

use nibkeep_passwords::{Board, Clipboard, ClipboardError};
use nibkeep_testkit::TestRng;
use zeroize::Zeroizing;

/// A clipboard in memory, as the platform's would be.
#[derive(Default)]
struct Memory {
	text: Option<String>,
	sensitive: bool,
	clears: usize,
	broken: bool,
	/// Whether reading fails, as when another program holds the clipboard.
	busy: bool,
}

impl Board for Memory {
	fn copy(&mut self, text: &str, sensitive: bool) -> Result<(), ClipboardError> {
		if self.broken {
			return Err(ClipboardError::new("no display"));
		}
		self.text = Some(text.to_owned());
		self.sensitive = sensitive;
		Ok(())
	}

	fn text(&mut self) -> Result<Option<Zeroizing<String>>, ClipboardError> {
		if self.busy {
			return Err(ClipboardError::new("busy"));
		}
		Ok(self.text.clone().map(Zeroizing::new))
	}

	fn clear(&mut self) -> Result<(), ClipboardError> {
		self.text = None;
		self.sensitive = false;
		self.clears += 1;
		Ok(())
	}
}

fn clipboard() -> Clipboard<Memory> {
	Clipboard::new(Memory::default(), &mut TestRng::new("clipboard"))
}

#[test]
fn a_secret_is_copied_marked_as_sensitive_and_cleared() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("hunter2").expect("copied");
	assert_eq!(clipboard.board().text.as_deref(), Some("hunter2"));
	assert!(clipboard.board().sensitive);
	assert!(clipboard.holds_secret());
	assert!(clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().text, None);
	assert!(!clipboard.holds_secret());
	// Once cleared, there is nothing of ours left to clear.
	assert!(!clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().clears, 1);
}

#[test]
fn what_was_copied_since_stays() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("hunter2").expect("copied");
	// The person copies something else, in another app.
	clipboard.board().text = Some("a link".to_owned());
	assert!(!clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().text.as_deref(), Some("a link"));
	assert_eq!(clipboard.board().clears, 0);
}

#[test]
fn a_plain_copy_is_not_marked_and_never_cleared() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("hunter2").expect("copied");
	clipboard.copy("alice@example.com").expect("copied");
	assert!(!clipboard.board().sensitive);
	assert!(!clipboard.holds_secret());
	assert!(!clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().text.as_deref(), Some("alice@example.com"));
}

#[test]
fn the_secret_is_not_kept_to_tell_it_again() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("a sentinel secret").expect("copied");
	let kept = format!("{clipboard:?}");
	assert!(!kept.contains("a sentinel secret"), "{kept}");
}

#[test]
fn a_clipboard_that_fails_says_so() {
	let mut clipboard = Clipboard::new(
		Memory {
			broken: true,
			..Memory::default()
		},
		&mut TestRng::new("broken"),
	);
	let error = clipboard.copy_secret("hunter2").expect_err("no display");
	assert_eq!(error.to_string(), "the clipboard failed: no display");
	assert!(!clipboard.holds_secret());
}

#[test]
fn a_copy_that_fails_leaves_the_secret_before_it_to_clear() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("hunter2").expect("copied");
	clipboard.board().broken = true;
	assert!(clipboard.copy_secret("another").is_err());
	assert!(clipboard.copy("alice").is_err());
	clipboard.board().broken = false;
	assert!(clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().text, None);
}

#[test]
fn a_clipboard_busy_for_a_moment_is_cleared_when_asked_again() {
	let mut clipboard = clipboard();
	clipboard.copy_secret("hunter2").expect("copied");
	clipboard.board().busy = true;
	assert!(clipboard.clear_if_ours().is_err());
	assert!(clipboard.holds_secret());
	clipboard.board().busy = false;
	assert!(clipboard.clear_if_ours().expect("read"));
	assert_eq!(clipboard.board().text, None);
}
