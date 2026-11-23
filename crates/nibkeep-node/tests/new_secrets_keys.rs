//! A new secrets key after a device that held the secrets is revoked, which every device
//! that holds them takes from any other that does, as each gift of it carries a link made
//! with a secrets key that the receiving device holds already; so a device that never held
//! the secrets gives nobody a key of its own making, and a key that a revoked device made is
//! read but never written with ([technical design: sharing]).
//!
//! [technical design: sharing]: ../../../docs/Technical-design.md#13-sharing

mod common;

use std::path::PathBuf;

use common::{LOGIN, library, login_type, now, pair_joining, pair_named, sync_in_memory, today};
use nibkeep_base::EntityId;
use nibkeep_crypto::{Key, PassphraseParams};
use nibkeep_governance::SealedSecretsKey;
use nibkeep_keys::{KeyFile, SecretsKeys};
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{Edit, Joining, Library, NewProtection, PairingCode, Unlocker};
use nibkeep_testkit::TestRng;

const LAPTOP: usize = 0;

/// The passphrase of the laptop's secrets.
const LAPTOP_SECRETS: &str = "laptop secrets";

/// The passphrase of the secrets of every device paired with them.
const DEVICE_SECRETS: &str = "secrets here";

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn protected(passphrase: &str) -> NewProtection<'_> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// Devices of one user: a laptop that set up the secrets, with a login whose password is
/// secret, and others paired with it or with each other.
struct Devices {
	seed: String,
	rng: TestRng,
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	login: EntityId,
	at: u64,
}

impl Devices {
	/// The laptop, and each device of `paired`, by its name, paired with the device it names,
	/// which gives it the secrets, with `true`, or not.
	fn new(seed: &str, paired: &[(usize, &str, bool)]) -> Self {
		let mut rng = TestRng::new(seed);
		let (directory, mut laptop) = library(&format!("{seed} laptop"), &mut rng);
		let personal = laptop.personal_space();
		laptop
			.define_type(personal, &login_type(), now(), &mut rng)
			.expect("defines");
		let login = laptop
			.create_entity(
				personal,
				&[LOGIN],
				vec![(field("local.title"), Value::Text("Bank".to_owned()))],
				now(),
				&mut rng,
			)
			.expect("creates");
		laptop
			.set_up_secrets(&protected(LAPTOP_SECRETS), now() + 500, &mut rng)
			.expect("sets up");
		let mut devices = Self {
			seed: seed.to_owned(),
			rng,
			libraries: vec![laptop],
			directories: vec![directory],
			login,
			at: now() + 100_000,
		};
		devices.set_password(LAPTOP, "hunter2");
		for &(sponsor, name, secrets) in paired {
			devices.pair(sponsor, name, secrets);
		}
		devices
	}

	/// Pairs a new device with `sponsor`, which gives it the secrets if `secrets` says so.
	fn pair(&mut self, sponsor: usize, name: &str, secrets: bool) -> usize {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{} {name} {}", self.seed, self.directories.len()),
		);
		std::fs::remove_dir(&directory).expect("removes");
		let code = PairingCode::generate(&mut self.rng);
		let (paired, joined) = if secrets {
			let joining = Joining::new(name, "nib", &mut self.rng)
				.expect("names")
				.asking_for_secrets(protected(DEVICE_SECRETS), &mut self.rng);
			pair_joining(
				&mut self.libraries[sponsor],
				&directory,
				(joining, true),
				(&code, &code),
				&mut self.rng,
			)
		} else {
			pair_named(
				&mut self.libraries[sponsor],
				&directory,
				name,
				(&code, &code),
				&mut self.rng,
			)
		};
		paired.expect("paired");
		self.libraries.push(joined.expect("joined").library);
		self.directories.push(directory);
		self.libraries.len() - 1
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Syncs two devices in memory, as `nib sync` does over the network: `a` connects to
	/// `b`; then each settles its keys, and both must manage.
	fn synced(&mut self, a: usize, b: usize) {
		let at = self.tick();
		let (first, second) = if a < b {
			let (left, right) = self.libraries.split_at_mut(b);
			(&mut left[a], &mut right[0])
		} else {
			let (left, right) = self.libraries.split_at_mut(a);
			(&mut right[0], &mut left[b])
		};
		let (initiated, responded) = sync_in_memory(first, second, at, (a, b));
		initiated.expect("the first synced");
		responded.expect("the second synced");
	}

	/// Revokes a device from another, by its name.
	fn revoke(&mut self, from: usize, name: &str) {
		let at = self.tick();
		let library = &mut self.libraries[from];
		let replicas: Vec<EntityId> = library
			.to_revoke(name)
			.expect("names a device")
			.into_iter()
			.map(|replica| replica.id)
			.collect();
		assert_eq!(replicas.len(), 1);
		library
			.revoke(&replicas, at, &mut self.rng)
			.expect("revokes");
	}

	/// Sets the login's password on a device, whose secrets are unlocked.
	fn set_password(&mut self, device: usize, password: &str) {
		let at = self.tick();
		let library = &mut self.libraries[device];
		library
			.edit_entity(
				library.personal_space(),
				self.login,
				vec![Edit::Set(
					field("local.password"),
					vec![Value::Text(password.to_owned())],
				)],
				&today(),
				at,
				&mut self.rng,
			)
			.expect("sets the password");
	}

	/// The passwords that a device shows.
	fn passwords(&self, device: usize) -> Vec<String> {
		common::passwords(&self.libraries[device], self.login)
			.into_iter()
			.filter_map(|value| value.as_text().map(str::to_owned))
			.collect()
	}

	/// The numbers of the personal space's secrets keys that a device holds while its
	/// secrets are unlocked, and the one it seals new secret values with.
	fn secrets_keys(&self, device: usize) -> (Vec<u64>, Option<u64>) {
		let library = &self.libraries[device];
		library
			.secrets_key_numbers(library.personal_space())
			.expect("a space")
	}

	/// Unlocks a device's secrets, and takes and gives the keys, as `nib` and apps do.
	fn unlock(&mut self, device: usize) {
		let at = self.tick();
		let passphrase = if device == LAPTOP {
			LAPTOP_SECRETS
		} else {
			DEVICE_SECRETS
		};
		let library = &mut self.libraries[device];
		library
			.unlock_secrets(&Unlocker::Passphrase(passphrase))
			.expect("unlocks");
		library.settle_secrets(at, &mut self.rng).expect("settles");
	}

	/// What a device's secrets compartment holds, read from its key file.
	fn compartment(&self, device: usize) -> SecretsKeys {
		let path = common::replica_folder(&self.directories[device]).join("keys.cbor");
		let key_file = KeyFile::read(&path).expect("reads");
		let passphrase = if device == LAPTOP {
			LAPTOP_SECRETS
		} else {
			DEVICE_SECRETS
		};
		let compartment = key_file
			.unlock_secrets(&Unlocker::Passphrase(passphrase))
			.expect("unlocks");
		key_file.secrets(&compartment).expect("opens")
	}

	/// Has a device that holds no secrets make up some of its own, as an infected one could,
	/// by writing its key file: a secrets key of number `epoch` and a key to receive with,
	/// which it returns. It then says that it holds them, as it unlocks them.
	fn make_up_secrets(
		&mut self,
		device: usize,
		epoch: u64,
	) -> (Key, nibkeep_crypto::DecryptionKey) {
		let personal = self.libraries[device].personal_space();
		let made_up = Key::generate(&mut self.rng);
		let decryption = nibkeep_crypto::DecryptionKey::generate(&mut self.rng);
		let path = common::replica_folder(&self.directories[device]).join("keys.cbor");
		let mut key_file = KeyFile::read(&path).expect("reads");
		key_file
			.set_secrets(
				&SecretsKeys {
					spaces: [(personal, [(epoch, made_up.clone())].into())].into(),
					decryption: decryption.clone(),
				},
				&protected(DEVICE_SECRETS),
				&mut self.rng,
			)
			.expect("seals");
		key_file.write(&path).expect("writes");
		self.unlock(device);
		(made_up, decryption)
	}

	/// Has a device commit `operations` to its personal space, as an infected one could by
	/// writing its own files, then opens it again, its secrets unlocked.
	fn forge(&mut self, device: usize, operations: Vec<nibkeep_model::Operation>) {
		let personal = self.libraries[device].personal_space();
		let at = self.tick();
		let NewProtection::Passphrase(passphrase, _) = common::protection() else {
			unreachable!("a passphrase");
		};
		let unlocker = Unlocker::Passphrase(passphrase);
		// Closed, so that the files are its own.
		drop(self.libraries.remove(device));
		common::forge(
			&self.directories[device],
			&unlocker,
			(personal, operations),
			(None, at),
			&mut self.rng,
		);
		let opened = Library::open(&self.directories[device], &unlocker).expect("opens");
		self.libraries.insert(device, opened);
		self.unlock(device);
	}

	/// The hashes of the secrets keys that a device's personal space seals to `replica`,
	/// opened with `decryption`, as an infected device could, whatever the rules say: the
	/// device is closed, and is left out of the devices from then on.
	fn opened_by(
		&mut self,
		device: usize,
		replica: EntityId,
		decryption: &nibkeep_crypto::DecryptionKey,
	) -> Vec<nibkeep_base::Hash> {
		let personal = self.libraries[device].personal_space();
		drop(self.libraries.remove(device));
		let directory = self.directories.remove(device);
		let NewProtection::Passphrase(passphrase, _) = common::protection() else {
			unreachable!("a passphrase");
		};
		let folder = common::replica_folder(&directory);
		let key_file = KeyFile::read(&folder.join("keys.cbor")).expect("reads");
		let keys = key_file
			.replica(
				&key_file
					.unlock_replica(&Unlocker::Passphrase(passphrase))
					.expect("unlocks"),
			)
			.expect("opens");
		let mut epochs = keys.spaces[&personal].iter();
		let (&first, key) = epochs.next().expect("a key");
		let mut space_keys = nibkeep_log::SpaceKeys::new(first, key.clone());
		for (&epoch, key) in epochs {
			space_keys.insert(epoch, key.clone());
		}
		let stored = nibkeep_store::StoredLog::open(
			&nibkeep_store::log_path(&folder, personal),
			personal,
			&space_keys,
			&keys.storage,
			600_000,
			[],
		)
		.expect("opens the log");
		stored
			.log()
			.governing_statements(&space_keys)
			.expect("reads")
			.into_iter()
			.filter(|governing| {
				governing.statement.entity == replica
					&& governing.statement.field == nibkeep_governance::SECRETS_KEY
			})
			.filter_map(|governing| {
				let text = governing.statement.value.as_text()?;
				let sealed = SealedSecretsKey::from_text(text)?;
				Some(nibkeep_crypto::key_hash(
					&sealed.open(decryption, personal, replica)?,
				))
			})
			.collect()
	}

	/// The replica of a device, as its library has it.
	fn replica(&self, device: usize) -> EntityId {
		self.libraries[device]
			.replicas()
			.expect("lists")
			.into_iter()
			.find(|replica| replica.this)
			.expect("its own")
			.id
	}
}

#[test]
fn revoking_a_device_that_held_the_secrets_gives_the_others_a_new_secrets_key() {
	const DESK: usize = 1;
	const OLD_PHONE: usize = 2;
	const PHONE: usize = 3;
	let mut devices = Devices::new(
		"secrets key after a revocation",
		&[
			(LAPTOP, "desk", true),
			(LAPTOP, "old phone", true),
			(DESK, "phone", true),
		],
	);
	devices.synced(DESK, LAPTOP);
	devices.synced(PHONE, DESK);
	devices.synced(DESK, LAPTOP);
	let (before, writing) = devices.secrets_keys(LAPTOP);
	assert_eq!(before.len(), 1);
	for device in [DESK, OLD_PHONE, PHONE] {
		assert_eq!(devices.secrets_keys(device), (before.clone(), writing));
	}
	assert_eq!(devices.passwords(PHONE), ["hunter2"]);
	let desk = devices.replica(DESK);
	let phone_asks = devices.libraries[LAPTOP].replicas().expect("lists");
	assert!(
		phone_asks
			.iter()
			.any(|replica| replica.device_name.as_deref() == Some("phone")
				&& replica.secrets_from == Some(desk)),
		"the phone took them from the desk"
	);

	// The laptop revokes the old phone, its secrets unlocked: it makes a new secrets key at
	// once, and seals the new password with it.
	devices.revoke(LAPTOP, "old phone");
	let (after, writing) = devices.secrets_keys(LAPTOP);
	assert_eq!(after.len(), 2, "{after:?}");
	let writing = writing.expect("writes with one");
	assert!(!before.contains(&writing));
	devices.set_password(LAPTOP, "correct horse");

	// The desk passes it on, its secrets locked; the phone, which asked the desk, takes it
	// all the same, as the laptop's gift proves the laptop held a key the phone holds.
	devices.libraries[DESK].lock_secrets();
	devices.synced(DESK, LAPTOP);
	devices.synced(PHONE, DESK);
	assert_eq!(devices.secrets_keys(PHONE), (after.clone(), Some(writing)));
	assert_eq!(devices.passwords(PHONE), ["correct horse"]);
	// The desk takes it once it unlocks.
	devices.unlock(DESK);
	assert_eq!(devices.secrets_keys(DESK), (after.clone(), Some(writing)));
	assert_eq!(devices.passwords(DESK), ["correct horse"]);
	// What the phone writes, the others read.
	devices.set_password(PHONE, "staple");
	devices.synced(PHONE, DESK);
	assert_eq!(devices.passwords(DESK), ["staple"]);
	// The old phone holds the first key alone.
	assert_eq!(devices.secrets_keys(OLD_PHONE).0, before);
}

#[test]
fn a_device_that_unlocks_after_a_revocation_makes_the_new_secrets_key() {
	const DESK: usize = 1;
	let mut devices = Devices::new(
		"secrets key made later",
		&[(LAPTOP, "desk", true), (LAPTOP, "old phone", true)],
	);
	devices.synced(DESK, LAPTOP);
	let (before, _) = devices.secrets_keys(LAPTOP);
	// Revoked with the laptop's secrets locked, and the desk's too.
	devices.libraries[LAPTOP].lock_secrets();
	devices.libraries[DESK].lock_secrets();
	devices.revoke(LAPTOP, "old phone");
	devices.synced(DESK, LAPTOP);
	// The desk is the first to unlock: it makes the key, and the laptop takes it.
	devices.unlock(DESK);
	let (after, writing) = devices.secrets_keys(DESK);
	assert_eq!(after.len(), 2, "{after:?}");
	assert!(!before.contains(&writing.expect("writes with one")));
	devices.set_password(DESK, "correct horse");
	devices.synced(DESK, LAPTOP);
	devices.unlock(LAPTOP);
	assert_eq!(devices.secrets_keys(LAPTOP), (after, writing));
	assert_eq!(devices.passwords(LAPTOP), ["correct horse"]);
}

#[test]
fn two_devices_that_make_a_new_secrets_key_at_once_each_take_the_others() {
	const DESK: usize = 1;
	let mut devices = Devices::new(
		"secrets keys at once",
		&[(LAPTOP, "desk", true), (LAPTOP, "old phone", true)],
	);
	devices.synced(DESK, LAPTOP);
	devices.libraries[LAPTOP].lock_secrets();
	devices.revoke(LAPTOP, "old phone");
	// The desk hears of it and makes a key; the laptop makes one as it unlocks, before it
	// hears of the desk's.
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.secrets_keys(DESK).0.len(), 2);
	devices.unlock(LAPTOP);
	assert_eq!(devices.secrets_keys(LAPTOP).0.len(), 2);
	assert_ne!(devices.secrets_keys(LAPTOP), devices.secrets_keys(DESK));
	devices.set_password(LAPTOP, "correct horse");
	devices.synced(DESK, LAPTOP);
	// Each takes the other's, and both seal with the same.
	let (held, writing) = devices.secrets_keys(LAPTOP);
	assert_eq!(held.len(), 3, "{held:?}");
	assert_eq!(devices.secrets_keys(DESK), (held, writing));
	assert_eq!(devices.passwords(DESK), ["correct horse"]);
}

#[test]
fn a_device_that_never_held_the_secrets_cannot_give_a_key_of_its_own_making() {
	const DESK: usize = 1;
	const PHONE: usize = 2;
	let mut devices = Devices::new(
		"secrets key of an infected phone",
		&[(LAPTOP, "desk", true), (LAPTOP, "phone", false)],
	);
	devices.synced(DESK, LAPTOP);
	devices.synced(PHONE, LAPTOP);
	let personal = devices.libraries[LAPTOP].personal_space();
	let (held, writing) = devices.secrets_keys(DESK);
	let desk = devices.replica(DESK);
	let phone = devices.replica(PHONE);

	// The phone, infected, makes up secrets of its own by writing its key file, and says it
	// holds them. It gives the desk its key, sealed to the desk's secrets key, linked with
	// that key itself, and once more claiming a link with the key the desk holds.
	let made_up_epoch = 7;
	let (made_up, phone_decryption) = devices.make_up_secrets(PHONE, made_up_epoch);
	let desk_encryption = devices.compartment(DESK).decryption.encryption_key();
	let hash = nibkeep_crypto::key_hash(&made_up);
	let gift = |link_epoch: u64, rng: &mut TestRng| {
		let sealed = SealedSecretsKey::seal(
			&desk_encryption,
			personal,
			desk,
			(made_up_epoch, &made_up, hash),
			(personal, link_epoch, &made_up),
			rng,
		);
		nibkeep_governance::give_secrets_key(desk, &sealed, rng)
	};
	let operations = vec![
		gift(made_up_epoch, &mut devices.rng),
		gift(held[0], &mut devices.rng),
	];
	devices.forge(PHONE, operations);
	devices.synced(PHONE, LAPTOP);
	devices.synced(DESK, LAPTOP);

	// The desk takes neither; the laptop, its secrets unlocked, gives the phone nothing.
	assert_eq!(devices.secrets_keys(DESK), (held.clone(), writing));
	devices.synced(PHONE, LAPTOP);
	assert_eq!(devices.secrets_keys(PHONE).0, [made_up_epoch]);
	assert_eq!(devices.passwords(PHONE), Vec::<String>::new());

	// Nor does a new key reach it, after a revocation.
	let tablet = devices.pair(LAPTOP, "tablet", true);
	devices.synced(tablet, LAPTOP);
	devices.revoke(LAPTOP, "tablet");
	devices.set_password(LAPTOP, "correct horse");
	devices.synced(PHONE, LAPTOP);
	devices.synced(DESK, LAPTOP);
	devices.synced(PHONE, LAPTOP);
	let (real, _) = devices.secrets_keys(DESK);
	assert_eq!(real.len(), 2);
	assert_eq!(devices.passwords(DESK), ["correct horse"]);
	assert_eq!(devices.secrets_keys(LAPTOP).0, real);
	// It made a new key of its own too, for its own made-up secrets.
	let (phones, _) = devices.secrets_keys(PHONE);
	assert!(phones.contains(&made_up_epoch), "{phones:?}");
	assert!(
		phones.iter().all(|epoch| !real.contains(epoch)),
		"{phones:?}"
	);
	assert_eq!(devices.passwords(PHONE), Vec::<String>::new());
	// Nothing in its log seals a real key to it, which it could open whatever the rules.
	let real: Vec<nibkeep_base::Hash> = devices.compartment(DESK).spaces[&personal]
		.values()
		.map(nibkeep_crypto::key_hash)
		.collect();
	assert_eq!(real.len(), 2);
	let opened = devices.opened_by(PHONE, phone, &phone_decryption);
	assert!(!opened.is_empty(), "its own keys are sealed to it");
	assert!(opened.iter().all(|hash| !real.contains(hash)), "{opened:?}");
}

#[test]
fn a_secrets_key_that_a_revoked_device_makes_is_read_but_never_written_with() {
	const DESK: usize = 1;
	const OLD_PHONE: usize = 2;
	let mut devices = Devices::new(
		"secrets key of a revoked phone",
		&[
			(LAPTOP, "desk", true),
			(LAPTOP, "old phone", true),
			(LAPTOP, "tablet", true),
		],
	);
	for device in [DESK, OLD_PHONE, 3] {
		devices.synced(device, LAPTOP);
	}
	devices.synced(DESK, LAPTOP);
	// The laptop revokes the old phone, which, not knowing, revokes the tablet itself, and
	// makes a key of its own.
	devices.revoke(LAPTOP, "old phone");
	let (_, laptops) = devices.secrets_keys(LAPTOP);
	devices.revoke(OLD_PHONE, "tablet");
	let (_, old_phones) = devices.secrets_keys(OLD_PHONE);
	assert_ne!(old_phones, laptops);
	// The desk hears of the old phone's first: it takes its key, writes with it, and seals a
	// password with it.
	devices.synced(DESK, OLD_PHONE);
	assert_eq!(devices.secrets_keys(DESK).1, old_phones);
	devices.set_password(DESK, "written with the old phone's key");
	// Then of the laptop's revocation: the old phone's key stays readable, never written
	// with again, as it knew nothing of it.
	devices.synced(DESK, LAPTOP);
	for device in [LAPTOP, DESK] {
		let (held, writing) = devices.secrets_keys(device);
		assert_eq!(held.len(), 3, "on {device}: {held:?}");
		assert_eq!(writing, laptops, "on {device}");
		assert_eq!(
			devices.passwords(device),
			["written with the old phone's key"]
		);
	}
}

/// Makes a recovery code on a device, with the secrets if `secrets` says so: the person
/// wrote it down and typed it back.
fn make_code(devices: &mut Devices, device: usize, secrets: bool) -> nibkeep_crypto::RecoveryCode {
	let at = devices.tick();
	let code = nibkeep_crypto::RecoveryCode::generate(&mut devices.rng);
	devices.libraries[device]
		.add_recovery_code(&code, secrets, at, &mut devices.rng)
		.expect("names the code");
	code
}

/// The numbers of the personal space's secrets keys that a device holds, by whether a code
/// opens them or lacks them, as a check on that device finds.
fn code_opens(
	devices: &Devices,
	device: usize,
	code: &nibkeep_crypto::RecoveryCode,
) -> (Vec<u64>, Vec<u64>) {
	let library = &devices.libraries[device];
	let check = library.check_recovery_code(code).expect("checks");
	check
		.spaces
		.into_iter()
		.find(|keys| keys.space == library.personal_space())
		.expect("the personal space names it")
		.secrets
		.expect("the secrets are unlocked")
}

#[test]
fn a_code_opens_the_secrets_only_if_it_was_made_with_them() {
	const DESK: usize = 1;
	let mut devices = Devices::new("a code and the secrets", &[(LAPTOP, "desk", false)]);
	let (held, _) = devices.secrets_keys(LAPTOP);
	assert_eq!(held.len(), 1);
	let with = make_code(&mut devices, LAPTOP, true);
	let without = make_code(&mut devices, LAPTOP, false);
	assert_eq!(code_opens(&devices, LAPTOP, &with), (held.clone(), vec![]));
	assert_eq!(
		code_opens(&devices, LAPTOP, &without),
		(vec![], held.clone())
	);
	let codes = devices.libraries[LAPTOP].recovery_codes().expect("lists");
	assert_eq!(
		codes
			.iter()
			.map(|code| code.holds_secrets)
			.collect::<Vec<_>>(),
		[true, false]
	);
	// A code named already is given them later, if the person chooses.
	let at = devices.tick();
	devices.libraries[LAPTOP]
		.add_recovery_code(&without, true, at, &mut devices.rng)
		.expect("gives the secrets");
	assert_eq!(code_opens(&devices, LAPTOP, &without), (held, vec![]));
	// A device whose secrets are not unlocked gives none, and names no code then.
	let at = devices.tick();
	let other = nibkeep_crypto::RecoveryCode::generate(&mut devices.rng);
	let error = devices.libraries[DESK]
		.add_recovery_code(&other, true, at, &mut devices.rng)
		.unwrap_err();
	assert!(
		matches!(error, nibkeep_node::NodeError::SecretsLocked),
		"{error}"
	);
	devices.synced(DESK, LAPTOP);
	assert_eq!(
		devices.libraries[DESK]
			.recovery_codes()
			.expect("lists")
			.len(),
		2
	);
}

#[test]
fn only_the_device_that_made_a_code_gives_it_the_secrets_later() {
	const DESK: usize = 1;
	let mut devices = Devices::new(
		"a code given the secrets elsewhere",
		&[(LAPTOP, "desk", true)],
	);
	let code = make_code(&mut devices, LAPTOP, false);
	devices.synced(DESK, LAPTOP);
	devices.unlock(DESK);
	// The desk holds the secrets, but did not make the code: what it gave a code that holds
	// no secrets yet, a device that recovers with the code could not tell from a key of
	// someone's own making.
	let at = devices.tick();
	let error = devices.libraries[DESK]
		.add_recovery_code(&code, true, at, &mut devices.rng)
		.unwrap_err();
	assert!(
		matches!(error, nibkeep_node::NodeError::RecoveryCodeMadeElsewhere),
		"{error}"
	);
	let (held, _) = devices.secrets_keys(DESK);
	assert_eq!(code_opens(&devices, DESK, &code), (vec![], held));
	// The laptop, which made it, still may.
	let at = devices.tick();
	devices.libraries[LAPTOP]
		.add_recovery_code(&code, true, at, &mut devices.rng)
		.expect("gives the secrets");
	let (held, _) = devices.secrets_keys(LAPTOP);
	assert_eq!(code_opens(&devices, LAPTOP, &code), (held, vec![]));
}

#[test]
fn a_new_secrets_key_is_given_to_a_code_that_holds_the_secrets_by_whoever_makes_it() {
	const DESK: usize = 1;
	let mut devices = Devices::new(
		"a code and a new secrets key",
		&[(LAPTOP, "desk", true), (LAPTOP, "phone", true)],
	);
	devices.synced(DESK, LAPTOP);
	let code = make_code(&mut devices, LAPTOP, true);
	devices.synced(DESK, LAPTOP);
	let (before, _) = devices.secrets_keys(DESK);
	// The desk, which never gave the code anything, revokes the phone, which held the
	// secrets: its new secrets key is the code's too, as the code's first gift is linked
	// with a key that the desk holds.
	devices.revoke(DESK, "phone");
	let (held, writing) = devices.secrets_keys(DESK);
	assert_eq!(held.len(), before.len() + 1);
	let (opens, lacks) = code_opens(&devices, DESK, &code);
	assert!(
		opens.contains(&writing.expect("a key")),
		"{opens:?} {lacks:?}"
	);
	assert_eq!(lacks, [] as [u64; 0]);
	devices.synced(DESK, LAPTOP);
	devices.unlock(LAPTOP);
	assert_eq!(code_opens(&devices, LAPTOP, &code), (held, vec![]));
}

#[test]
fn revoking_a_code_that_held_the_secrets_gives_them_a_new_key_that_it_lacks() {
	let mut devices = Devices::new("a code revoked, and the secrets", &[]);
	let code = make_code(&mut devices, LAPTOP, true);
	let (before, _) = devices.secrets_keys(LAPTOP);
	let at = devices.tick();
	let name = nibkeep_crypto::RecoveryCode::name_of(&code.signing_key().verifying_key());
	devices.libraries[LAPTOP]
		.revoke_recovery_code(&name, at, &mut devices.rng)
		.expect("revokes");
	let (held, writing) = devices.secrets_keys(LAPTOP);
	let writing = writing.expect("a key");
	assert_eq!(held.len(), before.len() + 1);
	assert!(!before.contains(&writing));
	let (opens, lacks) = code_opens(&devices, LAPTOP, &code);
	assert_eq!((opens, lacks), (before, vec![writing]));
	// A code that never held them needs no new secrets key when it goes.
	let plain = make_code(&mut devices, LAPTOP, false);
	let at = devices.tick();
	let name = nibkeep_crypto::RecoveryCode::name_of(&plain.signing_key().verifying_key());
	devices.libraries[LAPTOP]
		.revoke_recovery_code(&name, at, &mut devices.rng)
		.expect("revokes");
	assert_eq!(devices.secrets_keys(LAPTOP).1, Some(writing));
}
