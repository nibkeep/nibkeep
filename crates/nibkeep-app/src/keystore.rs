//! The platform's keystore, which can keep a library's key, as `nib`'s does
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#8-keys-at-rest-and-unlocking)):
//! on a computer, the computer's own; on a phone, the one the app gives ([`crate::App`]),
//! which only the app's code reaches.

use std::sync::{Arc, OnceLock};

use keyring_core::CredentialStore;
use nibkeep_node::{KeysError, NodeError};

/// The person turned a keystore's prompt down, or it was closed, as when the app went to
/// the background: nothing was confirmed. A keystore that asks the person fails with it.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("nothing was confirmed")
	}
}

impl std::error::Error for Cancelled {}

/// Whether an error of the engine is a keystore's prompt that the person turned down.
#[must_use]
pub fn cancelled(error: &NodeError) -> bool {
	matches!(
		error,
		NodeError::Keys(KeysError::Keystore(keyring_core::Error::NoStorageAccess(why)))
			if why.is::<Cancelled>()
	)
}

/// Whether an error of the engine is a key that the keystore no longer has, as when a
/// phone's screen lock was removed.
#[must_use]
pub fn lost(error: &NodeError) -> bool {
	matches!(
		error,
		NodeError::Keys(KeysError::Keystore(keyring_core::Error::NoEntry))
	)
}

/// Makes the app's keystore the one that libraries use, the first time: whether there is
/// one. With `NIBKEEP_NO_KEYSTORE` set, as an app's own tests set it on computers, there is
/// none. Before the app names itself ([`crate::start`]), there is none either, and the next
/// call looks again.
#[must_use]
pub fn keystore() -> bool {
	static KEYSTORE: OnceLock<bool> = OnceLock::new();
	if let Some(found) = KEYSTORE.get() {
		return *found;
	}
	let Some(app) = crate::APP.get() else {
		return false;
	};
	*KEYSTORE.get_or_init(|| {
		if nibkeep_node::at_login::app::kept_away() {
			return false;
		}
		match (app.keystore)() {
			Some(store) => {
				keyring_core::set_default_store(store);
				true
			}
			None => false,
		}
	})
}

/// The computer's keystore: macOS's keychain, Windows' credential manager, or the secret
/// service of Linux and the BSDs; `None` where it cannot be reached, and on phones.
#[must_use]
pub fn computer() -> Option<Arc<CredentialStore>> {
	#[cfg(target_os = "macos")]
	let store = apple_native_keyring_store::keychain::Store::new();
	#[cfg(target_os = "windows")]
	let store = windows_native_keyring_store::Store::new();
	#[cfg(all(
		unix,
		not(any(target_os = "macos", target_os = "ios", target_os = "android"))
	))]
	let store = zbus_secret_service_keyring_store::Store::new();
	#[cfg(any(
		target_os = "macos",
		target_os = "windows",
		all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)
	))]
	{
		store.ok().map(|store| -> Arc<CredentialStore> { store })
	}
	#[cfg(not(any(
		target_os = "macos",
		target_os = "windows",
		all(
			unix,
			not(any(target_os = "macos", target_os = "ios", target_os = "android"))
		)
	)))]
	None
}
