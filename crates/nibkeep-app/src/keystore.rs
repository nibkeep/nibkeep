//! The platform's keystore, which can keep a library's key, as `nib`'s does
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#8-keys-at-rest-and-unlocking)):
//! on a computer, the computer's own; on a phone, the one the app gives ([`crate::App`]),
//! which only the app's code reaches.

use std::sync::{Arc, Mutex, OnceLock};

use keyring_core::CredentialStore;
use nibkeep_node::{KeysError, NodeError};

use crate::locked;

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

/// Makes the app's keystore the one that libraries use, once the app gives one: whether
/// there is one. With `NIBKEEP_NO_KEYSTORE` set, as an app's own tests set it on computers,
/// there is none, for good. Before the app names itself ([`crate::start`]), or while it
/// gives none, as a phone's app before its code has what the keystore needs, there is none
/// either, and the next call looks again.
#[must_use]
pub fn keystore() -> bool {
	/// Whether there is a keystore, once that is settled.
	static SETTLED: Mutex<Option<bool>> = Mutex::new(None);
	let Some(app) = crate::APP.get() else {
		return false;
	};
	let mut settled = locked(&SETTLED);
	if let Some(found) = *settled {
		return found;
	}
	if nibkeep_node::at_login::app::kept_away() {
		*settled = Some(false);
		return false;
	}
	let Some(store) = (app.keystore)() else {
		return false;
	};
	keyring_core::set_default_store(store);
	*settled = Some(true);
	true
}

/// The computer's keystore: macOS's keychain, Windows' credential manager, or the secret
/// service of Linux and the BSDs; `None` where it cannot be reached, and on phones. It is
/// looked for once, as each look at the secret service may wait for it on D-Bus.
#[must_use]
pub fn computer() -> Option<Arc<CredentialStore>> {
	static FOUND: OnceLock<Option<Arc<CredentialStore>>> = OnceLock::new();
	FOUND.get_or_init(reach).clone()
}

/// The computer's keystore, reached anew.
fn reach() -> Option<Arc<CredentialStore>> {
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
