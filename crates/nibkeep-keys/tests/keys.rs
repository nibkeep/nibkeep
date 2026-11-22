//! The key file of a replica: its keys at rest, unlocked by a passphrase or the platform's
//! keystore.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Once};

use nibkeep_base::cbor;
use nibkeep_crypto::{DecryptionKey, PassphraseParams};
use nibkeep_keys::{
	KeyFile, KeysError, NewProtection, Protection, ReplicaKeys, SealedSecrets, SecretsKeys,
	SpaceKeyMap, Unlocker,
};
use nibkeep_testkit::TestRng;

const SERVICE: &str = "org.nibkeep.test";

/// Parameters small enough for quick tests.
fn quick() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

/// Uses the mock keystore, once for all tests of this file, which run in parallel.
fn mock_keystore() {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| {
		let store: Arc<keyring_core::CredentialStore> =
			keyring_core::mock::Store::new().expect("a mock store");
		keyring_core::set_default_store(store);
	});
}

fn replica_keys(rng: &mut TestRng) -> ReplicaKeys {
	let space = rng.space_id();
	ReplicaKeys {
		signing: rng.signing_key(),
		decryption: DecryptionKey::generate(rng),
		storage: rng.key(),
		spaces: BTreeMap::from([(space, BTreeMap::from([(0, rng.key())]))]),
	}
}

fn secrets_keys(rng: &mut TestRng, replica: &ReplicaKeys) -> SecretsKeys {
	SecretsKeys {
		spaces: replica
			.spaces
			.keys()
			.map(|&space| (space, BTreeMap::from([(0, rng.key())])))
			.collect(),
		decryption: DecryptionKey::generate(rng),
	}
}

/// The bytes of keys, for comparing them.
fn bytes(spaces: &SpaceKeyMap) -> Vec<[u8; 32]> {
	spaces
		.values()
		.flat_map(BTreeMap::values)
		.map(|key| *key.expose_bytes())
		.collect()
}

fn same_replica_keys(a: &ReplicaKeys, b: &ReplicaKeys) -> bool {
	a.signing.expose_bytes() == b.signing.expose_bytes()
		&& *a.decryption.expose_seed() == *b.decryption.expose_seed()
		&& a.storage.expose_bytes() == b.storage.expose_bytes()
		&& a.spaces.keys().eq(b.spaces.keys())
		&& bytes(&a.spaces) == bytes(&b.spaces)
}

/// The keystore entry of a compartment.
fn keystore_entry(protection: &Protection) -> keyring_core::Entry {
	let Protection::Keystore { service, account } = protection else {
		panic!("protected by the keystore");
	};
	keyring_core::Entry::new(service, account).expect("an entry")
}

/// An empty directory for one test.
fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

#[test]
fn replica_keys_unlock_with_the_right_passphrase_only() {
	let mut rng = TestRng::new("passphrase");
	let replica = replica_keys(&mut rng);
	let (file, created_key) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase("correct horse", quick()),
		&mut rng,
	)
	.expect("creates");
	assert!(same_replica_keys(
		&file.replica(&created_key).expect("opens"),
		&replica
	));
	let key = file
		.unlock_replica(&Unlocker::Passphrase("correct horse"))
		.expect("unlocks");
	assert!(same_replica_keys(
		&file.replica(&key).expect("opens"),
		&replica
	));
	assert!(matches!(
		file.unlock_replica(&Unlocker::Passphrase("correct horse!")),
		Err(KeysError::WrongPassphrase)
	));
	assert!(matches!(
		file.unlock_replica(&Unlocker::Keystore),
		Err(KeysError::WrongUnlocker)
	));
}

#[test]
fn replica_keys_unlock_with_the_platforms_keystore() {
	mock_keystore();
	let mut rng = TestRng::new("keystore");
	let replica = replica_keys(&mut rng);
	let protection = NewProtection::Keystore { service: SERVICE };
	let (file, _) = KeyFile::create(&replica, &protection, &mut rng).expect("creates");
	let key = file.unlock_replica(&Unlocker::Keystore).expect("unlocks");
	assert!(same_replica_keys(
		&file.replica(&key).expect("opens"),
		&replica
	));

	// Another file under the same service has a key of its own, and leaves this one alone.
	let (other, _) = KeyFile::create(&replica, &protection, &mut rng).expect("creates");
	assert!(file.unlock_replica(&Unlocker::Keystore).is_ok());
	assert!(other.unlock_replica(&Unlocker::Keystore).is_ok());

	// Without the keystore's key, nothing opens.
	keystore_entry(other.replica_protection())
		.delete_credential()
		.expect("deletes the key");
	assert!(matches!(
		other.unlock_replica(&Unlocker::Keystore),
		Err(KeysError::Keystore(_))
	));
}

#[test]
fn secrets_keys_unlock_separately() {
	mock_keystore();
	let mut rng = TestRng::new("secrets");
	let replica = replica_keys(&mut rng);
	let secrets = secrets_keys(&mut rng, &replica);
	let (mut file, _) = KeyFile::create(
		&replica,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("creates");
	assert!(matches!(
		file.unlock_secrets(&Unlocker::Keystore),
		Err(KeysError::NoSecrets)
	));
	// Even under the same service, the secrets get a keystore key of their own.
	file.set_secrets(
		&secrets,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("adds the secrets compartment");
	assert_ne!(Some(file.replica_protection()), file.secrets_protection());
	assert!(file.unlock_replica(&Unlocker::Keystore).is_ok());

	file.set_secrets(
		&secrets,
		&NewProtection::Passphrase("a passphrase for secrets", quick()),
		&mut rng,
	)
	.expect("protects the secrets with a passphrase");
	// Unlocking the replica compartment does not unlock the secrets.
	assert!(file.unlock_replica(&Unlocker::Keystore).is_ok());
	assert!(matches!(
		file.unlock_secrets(&Unlocker::Passphrase("a wrong passphrase")),
		Err(KeysError::WrongPassphrase)
	));
	let key = file
		.unlock_secrets(&Unlocker::Passphrase("a passphrase for secrets"))
		.expect("unlocks");
	assert_eq!(
		bytes(&file.secrets(&key).expect("opens").spaces),
		bytes(&secrets.spaces)
	);
	// The replica compartment's key does not open the secrets.
	let replica_key = file.unlock_replica(&Unlocker::Keystore).expect("unlocks");
	assert!(file.secrets(&replica_key).is_err());
}

#[test]
fn a_replaced_keystore_key_is_deleted_once_the_file_no_longer_needs_it() {
	mock_keystore();
	let mut rng = TestRng::new("retired");
	let replica = replica_keys(&mut rng);
	let (mut file, key) = KeyFile::create(
		&replica,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("creates");
	let path = directory("retired keystore key").join("keys.cbor");
	file.write(&path).expect("writes");
	let old_entry = keystore_entry(file.replica_protection());

	file.protect_replica(
		&key,
		&NewProtection::Passphrase("a passphrase", quick()),
		&mut rng,
	)
	.expect("changes the protection");
	// The file on disk still needs the old key, so it stays until the new file is written.
	assert!(old_entry.get_secret().is_ok());
	assert!(
		KeyFile::read(&path)
			.expect("reads")
			.unlock_replica(&Unlocker::Keystore)
			.is_ok()
	);
	file.write(&path).expect("writes");
	assert!(matches!(
		old_entry.get_secret(),
		Err(keyring_core::Error::NoEntry)
	));
	assert!(
		KeyFile::read(&path)
			.expect("reads")
			.unlock_replica(&Unlocker::Passphrase("a passphrase"))
			.is_ok()
	);
}

#[test]
fn the_key_file_round_trips_through_the_disk_without_plain_keys() {
	let mut rng = TestRng::new("disk");
	let replica = replica_keys(&mut rng);
	let (mut file, _) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase("correct horse", quick()),
		&mut rng,
	)
	.expect("creates");
	let path = directory("key file").join("keys.cbor");
	file.write(&path).expect("writes");
	let read = KeyFile::read(&path).expect("reads");
	assert_eq!(read, file);
	let key = read
		.unlock_replica(&Unlocker::Passphrase("correct horse"))
		.expect("unlocks");
	assert!(same_replica_keys(
		&read.replica(&key).expect("opens"),
		&replica
	));

	let content = fs::read(&path).expect("reads the bytes");
	for secret in [
		replica.signing.expose_bytes().as_slice(),
		replica.decryption.expose_seed().as_slice(),
		replica.storage.expose_bytes().as_slice(),
	] {
		assert!(!content.windows(secret.len()).any(|window| window == secret));
	}
	let leftovers: Vec<_> = fs::read_dir(path.parent().expect("a directory"))
		.expect("lists")
		.map(|entry| entry.expect("an entry").file_name())
		.collect();
	assert_eq!(leftovers, ["keys.cbor"]);
}

#[test]
fn keys_and_passphrases_can_change() {
	let mut rng = TestRng::new("changes");
	let mut replica = replica_keys(&mut rng);
	let (mut file, key) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase("old passphrase", quick()),
		&mut rng,
	)
	.expect("creates");

	// The replica joins another space, without asking for the passphrase again.
	replica
		.spaces
		.insert(rng.space_id(), BTreeMap::from([(0, rng.key())]));
	file.set_replica(&replica, &key, &mut rng)
		.expect("saves the new keys");
	assert!(same_replica_keys(
		&file.replica(&key).expect("opens"),
		&replica
	));

	// The passphrase changes.
	let new_key = file
		.protect_replica(
			&key,
			&NewProtection::Passphrase("new passphrase", quick()),
			&mut rng,
		)
		.expect("changes the passphrase");
	assert!(
		file.unlock_replica(&Unlocker::Passphrase("old passphrase"))
			.is_err()
	);
	assert!(
		file.replica(&key).is_err(),
		"the old key no longer opens it"
	);
	assert!(same_replica_keys(
		&file.replica(&new_key).expect("opens"),
		&replica
	));
	assert!(
		file.unlock_replica(&Unlocker::Passphrase("new passphrase"))
			.is_ok()
	);
}

#[test]
fn passphrases_are_never_printed() {
	let printed = format!(
		"{:?} {:?}",
		Unlocker::Passphrase("correct horse battery"),
		NewProtection::Passphrase("correct horse battery", quick())
	);
	assert!(!printed.contains("correct horse"), "{printed}");
}

#[test]
fn a_damaged_or_altered_key_file_is_detected() {
	let mut rng = TestRng::new("damaged");
	let replica = replica_keys(&mut rng);
	let (file, _) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase("correct horse", quick()),
		&mut rng,
	)
	.expect("creates");
	let mut encoded = cbor::to_vec(&file);
	let last = encoded.len() - 1;
	encoded[last] ^= 1;
	let damaged: KeyFile = cbor::from_slice(&encoded).expect("still well-formed");
	assert!(matches!(
		damaged.unlock_replica(&Unlocker::Passphrase("correct horse")),
		Err(KeysError::WrongPassphrase)
	));
	let path = directory("damaged key file").join("keys.cbor");
	fs::write(&path, b"not a key file").expect("writes");
	assert!(matches!(KeyFile::read(&path), Err(KeysError::Damaged(_))));
}

#[test]
fn secrets_sealed_apart_move_into_the_file_on_the_disk() {
	mock_keystore();
	let mut rng = TestRng::new("secrets apart");
	let replica = replica_keys(&mut rng);
	let secrets = secrets_keys(&mut rng, &replica);
	let (mut file, _) = KeyFile::create(
		&replica,
		&NewProtection::Passphrase("a passphrase", quick()),
		&mut rng,
	)
	.expect("creates");
	let path = directory("secrets apart").join("keys.cbor");
	file.write(&path).expect("writes");
	// Sealed apart, then put in the file as it is on the disk.
	let (sealed, key) = SealedSecrets::seal(
		&secrets,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("seals");
	let entry = keystore_entry(sealed.protection().expect("a compartment"));
	let mut on_disk = KeyFile::read(&path).expect("reads");
	on_disk.put_secrets(sealed);
	on_disk.write(&path).expect("writes");
	let unlocked = KeyFile::read(&path)
		.expect("reads")
		.unlock_secrets(&Unlocker::Keystore)
		.expect("unlocks");
	assert_eq!(
		bytes(&on_disk.secrets(&unlocked).expect("opens").spaces),
		bytes(&secrets.spaces)
	);
	assert_eq!(
		bytes(&on_disk.secrets(&key).expect("opens").spaces),
		bytes(&secrets.spaces)
	);
	// One sealed anew, with keys given meanwhile, replaces it; the old key goes once the
	// file is written.
	let (mut again, again_key) = SealedSecrets::seal(
		&secrets,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("seals");
	let mut given = secrets.clone();
	given.spaces.clear();
	again.update(&given, &again_key, &mut rng).expect("updates");
	let again_entry = keystore_entry(again.protection().expect("a compartment"));
	on_disk.put_secrets(again);
	on_disk.write(&path).expect("writes");
	assert!(matches!(
		entry.get_secret(),
		Err(keyring_core::Error::NoEntry)
	));
	assert!(
		on_disk
			.secrets(&again_key)
			.expect("opens")
			.spaces
			.is_empty()
	);
	// A compartment that no file will hold leaves no key in the keystore.
	let (other, _) = SealedSecrets::seal(
		&secrets,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("seals");
	let other_entry = keystore_entry(other.protection().expect("a compartment"));
	drop(other);
	assert!(matches!(
		other_entry.get_secret(),
		Err(keyring_core::Error::NoEntry)
	));
	assert!(again_entry.get_secret().is_ok(), "the written one stays");
}

#[test]
fn keys_to_delete_carry_over_to_a_copy_read_again() {
	mock_keystore();
	let mut rng = TestRng::new("obsolete carried");
	let replica = replica_keys(&mut rng);
	let (mut file, key) = KeyFile::create(
		&replica,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("creates");
	let path = directory("obsolete carried").join("keys.cbor");
	file.write(&path).expect("writes");
	let old_entry = keystore_entry(file.replica_protection());
	file.protect_replica(
		&key,
		&NewProtection::Passphrase("a passphrase", quick()),
		&mut rng,
	)
	.expect("changes the protection");
	// A copy read from the disk, then written, deletes what the older copy had to.
	let mut again = file.clone();
	again.take_obsolete(&mut file);
	again.write(&path).expect("writes");
	assert!(matches!(
		old_entry.get_secret(),
		Err(keyring_core::Error::NoEntry)
	));
}

#[test]
fn a_change_that_was_not_written_is_put_back_and_leaves_no_keystore_key() {
	mock_keystore();
	let mut rng = TestRng::new("restored");
	let replica = replica_keys(&mut rng);
	let (mut file, key) = KeyFile::create(
		&replica,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("creates");
	let secrets = secrets_keys(&mut rng, &replica);
	file.set_secrets(
		&secrets,
		&NewProtection::Passphrase("secrets", quick()),
		&mut rng,
	)
	.expect("sets the secrets");
	let old_entry = keystore_entry(file.replica_protection());
	let before = file.clone();

	// The replica to another keystore key, and the secrets to one: neither written.
	file.protect_replica(
		&key,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("changes the protection");
	file.set_secrets(
		&secrets,
		&NewProtection::Keystore { service: SERVICE },
		&mut rng,
	)
	.expect("changes the protection");
	let new_entries = [
		keystore_entry(file.replica_protection()),
		keystore_entry(file.secrets_protection().expect("secrets")),
	];
	file.restore(before.clone());
	assert_eq!(file, before);
	for entry in new_entries {
		assert!(matches!(
			entry.get_secret(),
			Err(keyring_core::Error::NoEntry)
		));
	}
	// The key that the file still needs stays, and writing it deletes nothing.
	assert!(old_entry.get_secret().is_ok());
	let path = directory("restored key file").join("keys.cbor");
	file.write(&path).expect("writes");
	assert!(old_entry.get_secret().is_ok());
	assert!(
		KeyFile::read(&path)
			.expect("reads")
			.unlock_replica(&Unlocker::Keystore)
			.is_ok()
	);
}

#[test]
fn putting_back_a_change_keeps_the_keys_still_to_delete() {
	mock_keystore();
	let mut rng = TestRng::new("restored, still to delete");
	let replica = replica_keys(&mut rng);
	let keystore = NewProtection::Keystore { service: SERVICE };
	// An earlier copy of the file replaced its keystore key, which is still to delete.
	let (mut earlier, earlier_key) =
		KeyFile::create(&replica, &keystore, &mut rng).expect("creates");
	let retired = keystore_entry(earlier.replica_protection());
	earlier
		.protect_replica(&earlier_key, &keystore, &mut rng)
		.expect("changes the protection");
	// The file as the disk holds it, changed, takes those over, and is not written.
	let (mut file, key) = KeyFile::create(&replica, &keystore, &mut rng).expect("creates");
	let in_use = keystore_entry(file.replica_protection());
	let before = file.clone();
	file.protect_replica(&key, &keystore, &mut rng)
		.expect("changes the protection");
	file.take_obsolete(&mut earlier);
	file.restore(before);
	// Writing it then deletes what was still to delete, and not the key it uses.
	let path = directory("restored, still to delete").join("keys.cbor");
	file.write(&path).expect("writes");
	assert!(matches!(
		retired.get_secret(),
		Err(keyring_core::Error::NoEntry)
	));
	assert!(in_use.get_secret().is_ok());
}
