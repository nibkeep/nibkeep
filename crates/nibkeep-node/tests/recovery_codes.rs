//! Recovery codes, with libraries on this computer standing in for the devices: a code is
//! made on a device, named in the personal space, and given the space's keys as a device is,
//! also those made later, until it is revoked ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

mod common;

use std::path::PathBuf;

use common::{library, now, pair_ai_agent_named, pair_named, sync_in_memory};
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{Library, NodeError, PairingCode, RecoveryCheck};
use nibkeep_testkit::TestRng;

/// Devices of one user: a laptop that started the library, and others paired with it.
struct Devices {
	seed: String,
	rng: TestRng,
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	at: u64,
}

const LAPTOP: usize = 0;
const DESK: usize = 1;

impl Devices {
	/// A laptop, and the devices `names` paired with it, each synced with it once.
	fn new(seed: &str, names: &[&str]) -> Self {
		let mut rng = TestRng::new(seed);
		let (directory, laptop) = library(&format!("{seed} laptop"), &mut rng);
		let mut devices = Self {
			seed: seed.to_owned(),
			rng,
			libraries: vec![laptop],
			directories: vec![directory],
			at: now() + 100_000,
		};
		for name in names {
			let device = devices.pair(name);
			devices.synced(device, LAPTOP);
		}
		devices
	}

	/// A folder for a new device's library, which pairing makes.
	fn new_directory(&self, name: &str) -> PathBuf {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{} {name} {}", self.seed, self.directories.len()),
		);
		std::fs::remove_dir(&directory).expect("removes");
		directory
	}

	/// Pairs a new device with the laptop.
	fn pair(&mut self, name: &str) -> usize {
		let directory = self.new_directory(name);
		let code = PairingCode::generate(&mut self.rng);
		let (paired, joined) = pair_named(
			&mut self.libraries[LAPTOP],
			&directory,
			name,
			(&code, &code),
			&mut self.rng,
		);
		paired.expect("paired");
		self.libraries.push(joined.expect("joined").library);
		self.directories.push(directory);
		self.libraries.len() - 1
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Syncs two devices in memory, each settling its keys after.
	fn synced(&mut self, a: usize, b: usize) {
		let at = self.tick();
		let (first, second) = if a < b {
			let (left, right) = self.libraries.split_at_mut(b);
			(&mut left[a], &mut right[0])
		} else {
			let (left, right) = self.libraries.split_at_mut(a);
			(&mut right[0], &mut left[b])
		};
		let (first, second) = sync_in_memory(first, second, at, (a, b));
		first.expect("the first synced");
		second.expect("the second synced");
	}

	/// Revokes a device from another, by its name.
	fn revoke(&mut self, from: usize, name: &str) {
		let at = self.tick();
		let library = &mut self.libraries[from];
		let replicas: Vec<_> = library
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

	/// Makes a code on a device: the person wrote it down and typed it back.
	fn make_code(&mut self, device: usize) -> RecoveryCode {
		let at = self.tick();
		let code = RecoveryCode::generate(&mut self.rng);
		self.libraries[device]
			.add_recovery_code(&code, false, at, &mut self.rng)
			.expect("names the code");
		code
	}

	/// Revokes a code from a device, by its name.
	fn revoke_code(&mut self, from: usize, code: &RecoveryCode) {
		let at = self.tick();
		let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
		self.libraries[from]
			.revoke_recovery_code(&name, at, &mut self.rng)
			.expect("revokes the code");
	}

	fn check(&self, device: usize, code: &RecoveryCode) -> RecoveryCheck {
		self.libraries[device]
			.check_recovery_code(code)
			.expect("checks")
	}

	/// The numbers of the personal space's keys that a device holds, but the first, which is
	/// never given out, and the one it writes with.
	fn keys(&self, device: usize) -> (Vec<u64>, u64) {
		let library = &self.libraries[device];
		let (mut held, writing) = library
			.space_key_numbers(library.personal_space())
			.expect("a space");
		held.retain(|&number| number != 0);
		(held, writing)
	}
}

/// The numbers of the personal space's keys that a check found the code to open, and those
/// it found it to lack.
fn opened(check: &RecoveryCheck) -> (Vec<u64>, Vec<u64>) {
	let [space] = check.spaces.as_slice() else {
		panic!("one space: {:?}", check.spaces);
	};
	(space.opens.clone(), space.lacks.clone())
}

#[test]
fn a_code_is_named_once_made_and_opens_every_key_given_out() {
	let mut devices = Devices::new("a code", &["desk", "phone"]);
	// A key besides the first: the phone is lost.
	devices.revoke(LAPTOP, "phone");
	let (held, _) = devices.keys(LAPTOP);
	assert_eq!(held.len(), 1);
	assert_eq!(devices.libraries[LAPTOP].recovery_codes().unwrap(), []);
	let code = devices.make_code(LAPTOP);
	let recovery_key = code.signing_key().verifying_key();
	let codes = devices.libraries[LAPTOP].recovery_codes().unwrap();
	let [named] = codes.as_slice() else {
		panic!("one code: {codes:?}");
	};
	assert_eq!(named.key, recovery_key);
	assert_eq!(named.name, RecoveryCode::name_of(&recovery_key));
	assert!(!named.revoked);
	assert_eq!(named.made_on.as_deref(), Some("laptop"));
	// The code opens the key that the revocation made.
	let check = devices.check(LAPTOP, &code);
	assert!(check.code.as_ref().is_some_and(|code| !code.revoked));
	assert_eq!(opened(&check), (held, vec![]));
	// The desk learns of the code by syncing, and names it alike.
	devices.synced(DESK, LAPTOP);
	let on_desk = devices.libraries[DESK].recovery_codes().unwrap();
	assert_eq!(on_desk.len(), 1);
	assert_eq!(on_desk[0].name, named.name);
	assert_eq!(on_desk[0].made_on.as_deref(), Some("laptop"));
	assert!(devices.check(DESK, &code).code.is_some());
}

#[test]
fn a_code_that_nothing_names_checks_as_unknown() {
	let mut devices = Devices::new("an unknown code", &[]);
	devices.make_code(LAPTOP);
	let other = RecoveryCode::generate(&mut devices.rng);
	let check = devices.check(LAPTOP, &other);
	assert!(check.code.is_none());
	assert_eq!(check.spaces, []);
}

#[test]
fn making_the_same_code_again_names_it_once() {
	let mut devices = Devices::new("the same code", &[]);
	let code = devices.make_code(LAPTOP);
	let at = devices.tick();
	devices.libraries[LAPTOP]
		.add_recovery_code(&code, false, at, &mut devices.rng)
		.expect("names it still");
	assert_eq!(devices.libraries[LAPTOP].recovery_codes().unwrap().len(), 1);
}

#[test]
fn a_key_made_after_the_code_is_sealed_to_it_by_whichever_device_makes_it() {
	let mut devices = Devices::new("keys made later", &["desk", "phone"]);
	let code = devices.make_code(LAPTOP);
	assert_eq!(opened(&devices.check(LAPTOP, &code)), (vec![], vec![]));
	// The desk, which knows of the code, revokes the phone: its new key is the code's too.
	devices.synced(DESK, LAPTOP);
	devices.revoke(DESK, "phone");
	let (held, writing) = devices.keys(DESK);
	assert_eq!(held, [writing]);
	assert_eq!(opened(&devices.check(DESK, &code)), (held.clone(), vec![]));
	devices.synced(DESK, LAPTOP);
	assert_eq!(opened(&devices.check(LAPTOP, &code)), (held, vec![]));
}

#[test]
fn a_key_made_by_a_device_that_had_not_heard_of_the_code_reaches_it_after_a_sync() {
	let mut devices = Devices::new("keys made meanwhile", &["desk", "phone"]);
	// The desk knows of the phone, which was paired after it.
	devices.synced(DESK, LAPTOP);
	let code = devices.make_code(LAPTOP);
	// The desk has not synced since: its new key is sealed to no code.
	devices.revoke(DESK, "phone");
	devices.synced(DESK, LAPTOP);
	// Each gives the keys it holds to whoever lacks them, after a sync: the code too.
	let (held, _) = devices.keys(LAPTOP);
	assert_eq!(held.len(), 1);
	assert_eq!(opened(&devices.check(LAPTOP, &code)), (held, vec![]));
}

#[test]
fn a_revoked_code_is_given_no_key_made_after() {
	let mut devices = Devices::new("a revoked code", &["desk"]);
	let code = devices.make_code(LAPTOP);
	devices.synced(DESK, LAPTOP);
	let (before, _) = devices.keys(LAPTOP);
	assert_eq!(before, [] as [u64; 0]);
	devices.revoke_code(LAPTOP, &code);
	// Revoking it gave the space a new key, which the code was not given.
	let (held, writing) = devices.keys(LAPTOP);
	assert_eq!(held, [writing]);
	let check = devices.check(LAPTOP, &code);
	assert!(check.code.as_ref().is_some_and(|code| code.revoked));
	assert_eq!(opened(&check), (vec![], held.clone()));
	let codes = devices.libraries[LAPTOP].recovery_codes().unwrap();
	assert!(codes[0].revoked);
	// Nor does the desk give it that key, or one of its own making, once it has heard.
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	assert_eq!(opened(&devices.check(DESK, &code)), (vec![], held));
	assert_eq!(devices.keys(DESK).1, writing);
}

#[test]
fn a_key_made_without_knowing_of_a_codes_revocation_is_not_written_with() {
	let mut devices = Devices::new("revoked meanwhile", &["desk", "phone"]);
	let code = devices.make_code(LAPTOP);
	devices.synced(DESK, LAPTOP);
	// The laptop revokes the code while the desk, which has not heard, revokes the phone:
	// the desk's new key is sealed to the code.
	devices.revoke_code(LAPTOP, &code);
	devices.revoke(DESK, "phone");
	let desks = devices.keys(DESK).1;
	assert!(opened(&devices.check(DESK, &code)).0.contains(&desks));
	// Neither key knows of both departures, so a new one is made, which both write with
	// and the code lacks.
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	let writing = devices.keys(LAPTOP).1;
	assert_eq!(devices.keys(DESK).1, writing);
	assert_ne!(writing, desks);
	let (opens, lacks) = opened(&devices.check(LAPTOP, &code));
	assert!(!opens.contains(&writing), "{opens:?}");
	assert!(lacks.contains(&writing), "{lacks:?}");
}

#[test]
fn revoking_a_code_on_a_librarys_only_device_gives_that_device_the_new_key() {
	// A device that never paired nor synced has stated no encryption key yet.
	let mut devices = Devices::new("the only device", &[]);
	let code = devices.make_code(LAPTOP);
	devices.revoke_code(LAPTOP, &code);
	let (held, writing) = devices.keys(LAPTOP);
	assert_eq!(held, [writing]);
	assert_eq!(opened(&devices.check(LAPTOP, &code)), (vec![], held));
}

#[test]
fn a_codes_record_is_the_librarys_own() {
	let mut devices = Devices::new("an own record", &[]);
	devices.make_code(LAPTOP);
	let at = devices.tick();
	let library = &mut devices.libraries[LAPTOP];
	let personal = library.personal_space();
	let record = library.recovery_codes().unwrap()[0].record;
	let user = library.status().expect("a status").user.id;
	// Nothing merges it, into anything or anything into it.
	for (from, into) in [(record, user), (user, record)] {
		let error = library
			.merge_entities(personal, from, into, at, &mut devices.rng)
			.unwrap_err();
		assert!(matches!(error, NodeError::OwnRecord(_)), "{error}");
	}
	// Nor is its type given to another entity.
	let error = library
		.edit_entity(
			personal,
			user,
			vec![nibkeep_node::Edit::AddType(
				nibkeep_model::TypeName::from_static("org.nibkeep.recovery-code"),
			)],
			&common::today(),
			at,
			&mut devices.rng,
		)
		.unwrap_err();
	assert!(matches!(error, NodeError::OwnType(_)), "{error}");
}

#[test]
fn a_code_named_for_another_member_is_listed_and_revoked_all_the_same() {
	// An infected device names a code of its own in the personal space, for a member it
	// made up: the code is given keys as any is, so it shows, and is revoked by its name.
	let mut devices = Devices::new("a made-up member", &[]);
	let at = devices.tick();
	let library = &devices.libraries[LAPTOP];
	let personal = library.personal_space();
	let code = RecoveryCode::generate(&mut devices.rng);
	let stated = nibkeep_governance::RecoveryStatement::sign(
		&code,
		nibkeep_base::EntityId::random(&mut devices.rng),
	);
	let operation = nibkeep_governance::state_recovery_code(personal, &stated, &mut devices.rng);
	let directory = devices.directories[LAPTOP].clone();
	let closed = devices.libraries.remove(LAPTOP);
	drop(closed);
	let unlocker = nibkeep_node::Unlocker::Passphrase("correct horse");
	common::forge(
		&directory,
		&unlocker,
		(personal, vec![operation]),
		(None, at),
		&mut devices.rng,
	);
	let reopened = Library::open(&directory, &unlocker).expect("opens");
	devices.libraries.insert(LAPTOP, reopened);
	let codes = devices.libraries[LAPTOP].recovery_codes().unwrap();
	assert_eq!(codes.len(), 1);
	assert_eq!(codes[0].key, code.signing_key().verifying_key());
	assert!(devices.check(LAPTOP, &code).code.is_some());
	devices.revoke_code(LAPTOP, &code);
	assert!(devices.libraries[LAPTOP].recovery_codes().unwrap()[0].revoked);
	let (held, _) = devices.keys(LAPTOP);
	assert_eq!(opened(&devices.check(LAPTOP, &code)), (vec![], held));
}

#[test]
fn a_code_to_revoke_is_named_by_its_name_or_its_record() {
	let mut devices = Devices::new("naming a code", &[]);
	let first = devices.make_code(LAPTOP);
	let second = devices.make_code(LAPTOP);
	let codes = devices.libraries[LAPTOP].recovery_codes().unwrap();
	assert_eq!(codes.len(), 2);
	// The older first.
	assert_eq!(codes[0].key, first.signing_key().verifying_key());
	let at = devices.tick();
	let library = &mut devices.libraries[LAPTOP];
	let error = library
		.revoke_recovery_code("ZZZZ", at, &mut devices.rng)
		.unwrap_err();
	assert!(matches!(error, NodeError::WhichRecoveryCode(_)), "{error}");
	// By its record, and in lowercase by its name.
	let record = codes[0].record.to_string();
	library
		.revoke_recovery_code(&record, at + 1, &mut devices.rng)
		.expect("revokes the first");
	let name = RecoveryCode::name_of(&second.signing_key().verifying_key()).to_lowercase();
	library
		.revoke_recovery_code(&name, at + 2, &mut devices.rng)
		.expect("revokes the second");
	let codes = library.recovery_codes().unwrap();
	assert!(codes.iter().all(|code| code.revoked));
	// A code revoked already says so.
	let error = library
		.revoke_recovery_code(&name, at + 3, &mut devices.rng)
		.unwrap_err();
	assert!(matches!(error, NodeError::WhichRecoveryCode(_)), "{error}");
}

#[test]
fn an_ai_agents_replica_makes_and_revokes_no_code() {
	let mut devices = Devices::new("an AI agent", &[]);
	let code = devices.make_code(LAPTOP);
	let directory = devices.new_directory("agent");
	let pairing = PairingCode::generate(&mut devices.rng);
	let (paired, joined) = pair_ai_agent_named(
		&mut devices.libraries[LAPTOP],
		&directory,
		("laptop", "Claude"),
		&pairing,
		&mut devices.rng,
	);
	paired.expect("paired");
	let mut agent = joined.expect("joined").library;
	let at = devices.tick();
	let own = RecoveryCode::generate(&mut devices.rng);
	let error = agent
		.add_recovery_code(&own, false, at, &mut devices.rng)
		.unwrap_err();
	assert!(matches!(error, NodeError::AiAgentMayNot(_)), "{error}");
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	let error = agent
		.revoke_recovery_code(&name, at, &mut devices.rng)
		.unwrap_err();
	assert!(matches!(error, NodeError::AiAgentMayNot(_)), "{error}");
	assert!(
		agent
			.recovery_codes()
			.unwrap()
			.iter()
			.all(|code| !code.revoked)
	);
}
