//! How a library's keys and its secrets are protected, changed after they were set up: a new
//! passphrase, or the platform's keystore, which may ask the person; a keystore that lost the
//! secrets' key; and a closed library that keeps its secrets unlocked, for an app that opens
//! the library for each operation.

mod common;

use std::any::Any;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once, PoisonError};

use common::{library, now, this_day, today};
use keyring_core::api::{CredentialApi, CredentialStoreApi};
use keyring_core::{Credential, Entry};
use nibkeep_crypto::{Key, PassphraseParams};
use nibkeep_keys::KeyFile;
use nibkeep_model::Value;
use nibkeep_node::{
	Edit, KeysError, Library, NewProtection, NodeError, Protection, SecretsState, Unlocker,
};
use nibkeep_std::{LOGIN, PASSWORD};
use nibkeep_testkit::TestRng;

/// The passphrase of the libraries that `library` makes.
const LIBRARY: &str = "correct horse";

/// The secrets' passphrase in these tests.
const SECRETS: &str = "tr0ub4dor";

fn quick() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

/// Uses the mock keystore, once for all tests of this file, through [`AskingStore`].
fn mock_keystore() {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| {
		let store: Arc<keyring_core::CredentialStore> = Arc::new(AskingStore(
			keyring_core::mock::Store::new().expect("a mock store"),
		));
		keyring_core::set_default_store(store);
	});
}

/// The keystore service whose keys ask the person each time they are kept or released, as
/// the app's on a phone asks for a fingerprint.
const ASKING: &str = "org.nibkeep.test.asking";

/// What the person does when the keystore asks, as the test sets it: run once, with the
/// library's directory.
type Answer = Box<dyn FnOnce(&Path) + Send>;

/// The library whose keys ask, and what happens while the keystore asks.
static ASKING_IN: Mutex<Option<(PathBuf, Option<Answer>)>> = Mutex::new(None);

/// Whether the key file's lock was free each time the keystore asked.
static LOCK_FREE: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// The mock keystore, whose keys under [`ASKING`] ask the person: the test notes whether
/// any program holds the key file's lock meanwhile, and runs its answer.
struct AskingStore(Arc<keyring_core::CredentialStore>);

struct AskingEntry {
	inner: Entry,
	asks: bool,
}

impl CredentialStoreApi for AskingStore {
	fn vendor(&self) -> String {
		"nibkeep tests".to_owned()
	}

	fn id(&self) -> String {
		"asking".to_owned()
	}

	fn build(
		&self,
		service: &str,
		user: &str,
		modifiers: Option<&HashMap<&str, &str>>,
	) -> keyring_core::Result<Entry> {
		let inner = self.0.build(service, user, modifiers)?;
		Ok(Entry::new_with_credential(Arc::new(AskingEntry {
			inner,
			asks: service == ASKING,
		})))
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl AskingEntry {
	fn ask(&self) {
		if !self.asks {
			return;
		}
		let mut asking = ASKING_IN.lock().unwrap_or_else(PoisonError::into_inner);
		let Some((directory, answer)) = asking.as_mut() else {
			return;
		};
		let lock = File::options()
			.create(true)
			.truncate(false)
			.write(true)
			.open(common::replica_folder(directory).join("keys.lock"))
			.expect("opens the lock");
		let free = lock.try_lock().is_ok();
		drop(lock);
		LOCK_FREE
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.push(free);
		if let Some(answer) = answer.take() {
			answer(directory);
		}
	}
}

impl CredentialApi for AskingEntry {
	fn set_secret(&self, secret: &[u8]) -> keyring_core::Result<()> {
		self.ask();
		self.inner.set_secret(secret)
	}

	fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
		self.ask();
		self.inner.get_secret()
	}

	// Whether the key is there, without asking.
	fn get_attributes(&self) -> keyring_core::Result<HashMap<String, String>> {
		self.inner.get_attributes()
	}

	fn delete_credential(&self) -> keyring_core::Result<()> {
		self.inner.delete_credential()
	}

	fn get_credential(&self) -> keyring_core::Result<Option<Arc<Credential>>> {
		self.inner.get_credential().map(|_| None)
	}

	fn get_specifiers(&self) -> Option<(String, String)> {
		self.inner.get_specifiers()
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

/// Whether the keystore holds the key that a protection names.
fn in_keystore(protection: &Protection) -> bool {
	let Protection::Keystore { service, account } = protection else {
		panic!("not in the keystore: {protection:?}");
	};
	keyring_core::Entry::new(service, account)
		.expect("an entry")
		.get_secret()
		.is_ok()
}

/// A library with secrets, unlocked, and a login whose password is `hunter2`.
fn with_password(name: &str, rng: &mut TestRng) -> (std::path::PathBuf, Library) {
	let (directory, mut library) = library(name, rng);
	let personal = library.personal_space();
	library
		.set_up_secrets(
			&NewProtection::Passphrase(SECRETS, quick()),
			now() + 500,
			rng,
		)
		.expect("sets up");
	let login = library
		.create_entity(personal, &[LOGIN], Vec::new(), now(), rng)
		.expect("creates");
	library
		.edit_entity(
			personal,
			login,
			vec![Edit::Set(PASSWORD, vec![Value::Text("hunter2".to_owned())])],
			&today(),
			now(),
			rng,
		)
		.expect("sets the password");
	(directory, library)
}

/// The passwords that the library's logins show.
fn passwords(library: &Library) -> Vec<Value> {
	let personal = library.personal_space();
	library
		.entities(personal, Some(&LOGIN))
		.expect("lists")
		.into_iter()
		.flat_map(|listed| {
			let view = library
				.entity(personal, listed.id, this_day())
				.expect("reads")
				.expect("the login");
			view.values(&PASSWORD)
				.iter()
				.map(|shown| shown.value.clone())
				.collect::<Vec<_>>()
		})
		.collect()
}

#[test]
fn a_library_gets_a_new_passphrase_and_the_old_one_opens_it_no_longer() {
	let mut rng = TestRng::new("a new passphrase");
	let (directory, mut library) = library("a new passphrase", &mut rng);
	library
		.protect(&NewProtection::Passphrase("new horse", quick()), &mut rng)
		.expect("protects");
	// The library that changed it closes and opens again with the new key.
	let reopened = library.close().open().expect("opens again");
	drop(reopened);
	assert!(matches!(
		Library::open(&directory, &Unlocker::Passphrase(LIBRARY)),
		Err(NodeError::Keys(KeysError::WrongPassphrase))
	));
	Library::open(&directory, &Unlocker::Passphrase("new horse")).expect("opens");
	assert!(matches!(
		Library::protection(&directory).expect("reads"),
		Protection::Passphrase { .. }
	));
}

#[test]
fn a_library_moves_to_the_keystore_and_back() {
	mock_keystore();
	let mut rng = TestRng::new("keystore and back");
	let (directory, mut library) = library("keystore and back", &mut rng);
	library
		.protect(
			&NewProtection::Keystore {
				service: "org.nibkeep.test",
			},
			&mut rng,
		)
		.expect("protects");
	let in_store = Library::protection(&directory).expect("reads");
	assert!(in_keystore(&in_store));
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Keystore).expect("opens");
	library
		.protect(&NewProtection::Passphrase("new horse", quick()), &mut rng)
		.expect("protects");
	// The keystore's key is deleted once the file that needed it is replaced.
	assert!(!in_keystore(&in_store));
	drop(library);
	Library::open(&directory, &Unlocker::Passphrase("new horse")).expect("opens");
}

#[test]
fn secrets_get_a_new_passphrase_and_keep_their_values() {
	let mut rng = TestRng::new("secrets passphrase");
	let (directory, mut library) = with_password("secrets passphrase", &mut rng);
	library
		.protect_secrets(&NewProtection::Passphrase("new secrets", quick()), &mut rng)
		.expect("protects");
	// Still unlocked.
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
	library.lock_secrets();
	assert!(matches!(
		library.unlock_secrets(&Unlocker::Passphrase(SECRETS)),
		Err(NodeError::Keys(KeysError::WrongPassphrase))
	));
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	library
		.unlock_secrets(&Unlocker::Passphrase("new secrets"))
		.expect("unlocks");
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
}

#[test]
fn secrets_move_to_the_keystore() {
	mock_keystore();
	let mut rng = TestRng::new("secrets in the keystore");
	let (directory, mut library) = with_password("secrets in the keystore", &mut rng);
	library
		.protect_secrets(
			&NewProtection::Keystore {
				service: "org.nibkeep.test",
			},
			&mut rng,
		)
		.expect("protects");
	let protection = library
		.secrets_protection()
		.expect("reads")
		.expect("secrets");
	assert!(in_keystore(&protection));
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	library
		.unlock_secrets(&Unlocker::Keystore)
		.expect("unlocks");
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
}

#[test]
fn secrets_are_protected_anew_only_while_unlocked() {
	let mut rng = TestRng::new("secrets locked");
	let (_directory, mut library) = with_password("secrets locked", &mut rng);
	library.lock_secrets();
	assert!(matches!(
		library.protect_secrets(&NewProtection::Passphrase("new secrets", quick()), &mut rng),
		Err(NodeError::SecretsLocked)
	));
	// A library without secrets has none to protect.
	let (_other, mut other) = common::library("no secrets to protect", &mut rng);
	assert!(matches!(
		other.protect_secrets(&NewProtection::Passphrase("new secrets", quick()), &mut rng),
		Err(NodeError::Keys(KeysError::NoSecrets))
	));
}

#[test]
fn the_library_and_its_secrets_keep_passphrases_of_their_own() {
	let mut rng = TestRng::new("passphrases apart");
	let (directory, mut library) = with_password("passphrases apart", &mut rng);
	assert!(matches!(
		library.protect(&NewProtection::Passphrase(SECRETS, quick()), &mut rng),
		Err(NodeError::LibraryPassphrase)
	));
	assert!(matches!(
		library.protect_secrets(&NewProtection::Passphrase(LIBRARY, quick()), &mut rng),
		Err(NodeError::LibraryPassphrase)
	));
	// Nothing changed.
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	library
		.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		.expect("unlocks");
}

#[test]
fn a_closed_library_opens_again_with_its_secrets_unlocked() {
	let mut rng = TestRng::new("closed with secrets");
	let (directory, library) = with_password("closed with secrets", &mut rng);
	let personal = library.personal_space();
	let mut closed = library.close();
	assert!(closed.secrets_kept());
	// Opened, it keeps the secrets' key, and opens them only when asked.
	let mut library = closed.open().expect("opens");
	assert!(!library.secrets_unlocked(personal));
	assert!(library.unlock_kept_secrets().expect("unlocks"));
	assert!(library.secrets_unlocked(personal));
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
	// Closed again, it still keeps them.
	let mut library = library.close().open().expect("opens");
	assert!(library.unlock_kept_secrets().expect("unlocks"));
	// Another program can use the library while it is closed.
	drop(library);
	drop(Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens"));

	closed.lock_secrets();
	assert!(!closed.secrets_kept());
	let mut library = closed.open().expect("opens");
	assert!(!library.unlock_kept_secrets().expect("reads"));
	assert_eq!(passwords(&library), Vec::<Value>::new());
	// Closed with the secrets locked, it opens with them locked.
	let mut library = library.close().open().expect("opens");
	assert!(!library.unlock_kept_secrets().expect("reads"));
	assert!(!library.secrets_unlocked(personal));
}

#[test]
fn secrets_protected_anew_by_another_program_open_locked() {
	let mut rng = TestRng::new("secrets changed meanwhile");
	let (directory, library) = with_password("secrets changed meanwhile", &mut rng);
	let personal = library.personal_space();
	let closed = library.close();
	let mut other = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	other
		.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		.expect("unlocks");
	other
		.protect_secrets(&NewProtection::Passphrase("new secrets", quick()), &mut rng)
		.expect("protects");
	drop(other);
	// The key it kept opens the secrets no longer: they stay locked, and it is forgotten.
	let mut library = closed.open().expect("opens");
	assert!(!library.unlock_kept_secrets().expect("reads"));
	assert!(!library.secrets_unlocked(personal));
	assert!(!library.close().secrets_kept());
}

#[test]
fn secrets_are_sealed_anew_while_other_programs_may_write_the_key_file() {
	mock_keystore();
	let mut rng = TestRng::new("sealed while asking");
	let (directory, mut library) = with_password("sealed while asking", &mut rng);
	let personal = library.personal_space();
	// While the keystore asks the person, another program gives the library a secrets key,
	// as `nib` may.
	let given = Key::generate(&mut rng);
	let answer = {
		let given = given.clone();
		move |directory: &Path| {
			let path = common::replica_folder(directory).join("keys.cbor");
			let mut key_file = KeyFile::read(&path).expect("reads");
			let key = key_file
				.unlock_secrets(&Unlocker::Passphrase(SECRETS))
				.expect("unlocks");
			let mut keys = key_file.secrets(&key).expect("opens");
			keys.spaces
				.get_mut(&personal)
				.expect("the personal space")
				.insert(7, given);
			key_file
				.update_secrets(&keys, &key, &mut TestRng::new("given meanwhile"))
				.expect("seals");
			key_file.write(&path).expect("writes");
		}
	};
	*ASKING_IN.lock().unwrap_or_else(PoisonError::into_inner) =
		Some((directory.clone(), Some(Box::new(answer))));
	library
		.protect_secrets(&NewProtection::Keystore { service: ASKING }, &mut rng)
		.expect("protects");
	// It asked once, with the key file free.
	assert_eq!(
		std::mem::take(&mut *LOCK_FREE.lock().unwrap_or_else(PoisonError::into_inner)),
		[true]
	);
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
	// The key given meanwhile is kept, under the new protection.
	let key_file =
		KeyFile::read(&common::replica_folder(&directory).join("keys.cbor")).expect("reads");
	let key = key_file
		.unlock_secrets(&Unlocker::Keystore)
		.expect("unlocks");
	let keys = key_file.secrets(&key).expect("opens");
	assert_eq!(
		keys.spaces[&personal].get(&7).map(Key::expose_bytes),
		Some(given.expose_bytes())
	);
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	library
		.unlock_secrets(&Unlocker::Keystore)
		.expect("unlocks");
	assert_eq!(passwords(&library), [Value::Text("hunter2".to_owned())]);
	*ASKING_IN.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

#[test]
fn secrets_whose_key_the_keystore_lost_are_forgotten_and_set_up_anew() {
	mock_keystore();
	let mut rng = TestRng::new("keystore lost the key");
	let (directory, mut library) = with_password("keystore lost the key", &mut rng);
	let keystore = NewProtection::Keystore {
		service: "org.nibkeep.test",
	};
	// Not while the keystore has the key, nor for a passphrase.
	assert!(matches!(
		library.forget_lost_secrets(),
		Err(NodeError::SecretsKeyKept)
	));
	library
		.protect_secrets(&keystore, &mut rng)
		.expect("protects");
	assert!(matches!(
		library.forget_lost_secrets(),
		Err(NodeError::SecretsKeyKept)
	));
	library
		.unlock_secrets(&Unlocker::Keystore)
		.expect("still unlocks");
	// The keystore loses the key, as Android destroys keys that need a confirmation once
	// the phone's screen lock is removed.
	let Some(Protection::Keystore { service, account }) =
		library.secrets_protection().expect("reads")
	else {
		panic!("in the keystore");
	};
	keyring_core::Entry::new(&service, &account)
		.expect("an entry")
		.delete_credential()
		.expect("deletes");
	library.lock_secrets();
	assert!(matches!(
		library.unlock_secrets(&Unlocker::Keystore),
		Err(NodeError::Keys(KeysError::Keystore(
			keyring_core::Error::NoEntry
		)))
	));
	library.forget_lost_secrets().expect("forgets");
	assert!(library.secrets_protection().expect("reads").is_none());
	// No other replica held them: they are lost, and new ones are set up.
	assert_eq!(
		library.secrets_state().expect("reads"),
		SecretsState::NotSetUp
	);
	library
		.set_up_secrets(&keystore, now() + 5_000, &mut rng)
		.expect("sets up anew");
	assert_eq!(passwords(&library), Vec::<Value>::new());
	drop(library);
	let mut library = Library::open(&directory, &Unlocker::Passphrase(LIBRARY)).expect("opens");
	library
		.unlock_secrets(&Unlocker::Keystore)
		.expect("unlocks the new ones");
}
