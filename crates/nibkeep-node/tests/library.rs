//! A replica's library: started, opened again, and described.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Once};

use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	KeysError, Library, NewLibrary, NewProtection, NodeError, Protection, Unlocker,
};
use nibkeep_testkit::{TestRng, millis_at};

/// Parameters small enough for quick tests.
fn quick() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

fn now() -> u64 {
	millis_at("2026-10-12", 10, 0)
}

/// An empty directory for one test.
fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

/// Uses the mock keystore, once for all tests of this file.
fn mock_keystore() {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| {
		let store: Arc<keyring_core::CredentialStore> =
			keyring_core::mock::Store::new().expect("a mock store");
		keyring_core::set_default_store(store);
	});
}

#[test]
fn a_library_starts_with_a_user_a_device_a_replica_and_a_personal_space() {
	let directory = directory("a new library");
	let mut rng = TestRng::new("a new library");
	let protection = NewProtection::Passphrase("correct horse", quick());
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates");
	let status = library.status().expect("reads the status");
	assert_eq!(status.user.name.as_deref(), Some("Ada"));
	assert_eq!(status.device.name.as_deref(), Some("laptop"));
	assert_eq!(status.replica.name.as_deref(), Some("nib"));
	assert_eq!(status.spaces.len(), 1);
	let personal = &status.spaces[0];
	assert_eq!(personal.id, library.personal_space());
	assert_eq!(personal.name.as_deref(), Some("Personal"));
	assert_eq!(
		personal.entities, 5,
		"the user, the person, the device, the replica and the space"
	);
	assert_eq!(
		personal.changes, 7,
		"what governs the space, the standard vocabulary, and a change for each of the five, as every change is about one entity"
	);
	drop(library);

	assert!(matches!(
		Library::protection(&directory),
		Ok(Protection::Passphrase { .. })
	));
	let reopened =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(reopened.status().expect("reads the status"), status);
	drop(reopened);
	assert!(matches!(
		Library::open(&directory, &Unlocker::Passphrase("correct horse!")),
		Err(NodeError::Keys(KeysError::WrongPassphrase))
	));
	assert!(matches!(
		Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			now(),
			&mut rng
		),
		Err(NodeError::Exists)
	));
}

#[test]
fn a_library_can_be_unlocked_by_the_platforms_keystore() {
	mock_keystore();
	let directory = directory("a library in the keystore");
	let mut rng = TestRng::new("keystore");
	let protection = NewProtection::Keystore {
		service: "org.nibkeep.test",
	};
	let status = Library::create(
		&directory,
		&NewLibrary::new("Ada", "phone", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates")
	.status()
	.expect("reads the status");
	assert!(matches!(
		Library::protection(&directory),
		Ok(Protection::Keystore { .. })
	));
	let reopened = Library::open(&directory, &Unlocker::Keystore).expect("opens");
	assert_eq!(reopened.status().expect("reads the status"), status);
}

#[test]
fn a_directory_without_a_library_is_refused() {
	let directory = directory("no library");
	assert!(matches!(
		Library::protection(&directory),
		Err(NodeError::NoLibrary)
	));
	assert!(matches!(
		Library::open(&directory, &Unlocker::Passphrase("anything")),
		Err(NodeError::NoLibrary)
	));
	assert!(matches!(
		Library::protection(&directory.join("missing")),
		Err(NodeError::NoLibrary)
	));
}

#[test]
fn a_start_that_was_cut_short_says_so() {
	let directory = directory("a start cut short");
	let mut rng = TestRng::new("cut short");
	let protection = NewProtection::Passphrase("correct horse", quick());
	Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates");
	fs::remove_file(
		Library::replica_folder(&directory)
			.expect("a replica is here")
			.join("library.cbor"),
	)
	.expect("removes the library file");
	assert!(Library::exists(&directory));
	assert!(matches!(
		Library::protection(&directory),
		Err(NodeError::Unfinished)
	));
	assert!(matches!(
		Library::open(&directory, &Unlocker::Passphrase("correct horse")),
		Err(NodeError::Unfinished)
	));
	assert!(matches!(
		Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			now(),
			&mut rng
		),
		Err(NodeError::Exists)
	));
}

#[test]
fn a_missing_space_log_is_damage() {
	let directory = directory("a missing log");
	let mut rng = TestRng::new("missing log");
	let protection = NewProtection::Passphrase("correct horse", quick());
	let personal = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates")
	.personal_space();
	fs::remove_file(nibkeep_store::log_path(
		&Library::replica_folder(&directory).expect("a replica is here"),
		personal,
	))
	.expect("removes the log");
	assert!(matches!(
		Library::open(&directory, &Unlocker::Passphrase("correct horse")),
		Err(NodeError::Damaged(_))
	));
	assert!(
		!nibkeep_store::log_path(
			&Library::replica_folder(&directory).expect("a replica is here"),
			personal
		)
		.exists(),
		"no empty log is made in its place"
	);
}

#[test]
fn fields_that_a_space_marks_secret_are_secret() {
	use nibkeep_keys::KeyFile;
	use nibkeep_log::SpaceKeys;
	use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Types, ValueKind};
	use nibkeep_store::{StoredLog, log_path};

	let directory = directory("secret fields of a space");
	let mut rng = TestRng::new("secret fields");
	let protection = NewProtection::Passphrase("correct horse", quick());
	let personal = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates")
	.personal_space();

	// The space gains a type with a secret field, as another replica could have added it.
	let key_file = KeyFile::read(
		&Library::replica_folder(&directory)
			.expect("a replica is here")
			.join("keys.cbor"),
	)
	.expect("reads the keys");
	let compartment = key_file
		.unlock_replica(&Unlocker::Passphrase("correct horse"))
		.expect("unlocks");
	let replica = key_file.replica(&compartment).expect("opens");
	let keys = SpaceKeys::new(0, replica.spaces[&personal][&0].clone());
	let password: FieldName = "org.example.password".parse().expect("a field name");
	let login = TypeDef::new(TypeName::from_static("org.example.login"), 1)
		.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret());
	{
		let mut stored = StoredLog::open(
			&log_path(
				&Library::replica_folder(&directory).expect("a replica is here"),
				personal,
			),
			personal,
			&keys,
			&replica.storage,
			600_000,
			[],
		)
		.expect("opens the space");
		let operations = Types::new().define(&login, &mut rng);
		stored
			.commit(&replica.signing, &keys, operations, now() + 1_000, &mut rng)
			.expect("commits");
	}

	let library = Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert!(
		library
			.secret_fields(personal)
			.is_some_and(|secret| secret.contains(&password))
	);
}

#[test]
fn a_library_dropped_opens_again_with_everything() {
	let directory = directory("closed for exit");
	let mut rng = TestRng::new("closed for exit");
	let protection = NewProtection::Passphrase("correct horse", quick());
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates");
	let status = library.status().expect("reads the status");
	drop(library);
	let reopened = Library::open(&directory, &Unlocker::Passphrase("correct horse"))
		.expect("opens again: its spaces were released");
	assert_eq!(reopened.status().expect("reads the status"), status);
}
