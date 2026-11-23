//! Recovering the secrets with a recovery code: a code made with the secrets brings them back,
//! in the personal space and the person's other spaces, held by the new device as one that
//! took a copy's secrets holds them, under a passphrase of their own, and sealed from then on
//! with a key that the lost devices never held; a code made without them brings none, which
//! the recovery names, and a key that a device of the person's gave the code of its own
//! making is never taken
//! ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

mod common;

use std::path::{Path, PathBuf};

use common::devices::on_disk;
use common::{LOGIN, library, login_type, now, pair_named, protection, today};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{DecryptionKey, Key, PassphraseParams, RecoveryCode, key_hash};
use nibkeep_governance::{RecoveryStatement, SealedSecretsKey};
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	Edit, Joining, Library, NewBackup, NewProtection, NodeError, PairingCode, Recovered,
	RecoveredSecrets, Recovering, SecretsState, Unlocker,
};
use nibkeep_testkit::TestRng;

/// The passphrase of the lost laptop's secrets.
const LAPTOP_SECRETS: &str = "laptop secrets";

/// The passphrase of the secrets on the new computer.
const NEW_SECRETS: &str = "new secrets";

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn protected(passphrase: &str) -> NewProtection<'_> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// A person's laptop, which set up the secrets, with a login in the personal space whose
/// password is secret.
struct Laptop {
	name: String,
	rng: TestRng,
	library: Library,
	login: EntityId,
	at: u64,
}

impl Laptop {
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let (_, mut library) = library(&format!("{name}, laptop"), &mut rng);
		let personal = library.personal_space();
		library
			.define_type(personal, &login_type(), now(), &mut rng)
			.expect("defines");
		let login = library
			.create_entity(
				personal,
				&[LOGIN],
				vec![(field("local.title"), Value::Text("Bank".to_owned()))],
				now(),
				&mut rng,
			)
			.expect("creates");
		library
			.set_up_secrets(&protected(LAPTOP_SECRETS), now() + 500, &mut rng)
			.expect("sets up");
		let mut laptop = Self {
			name: name.to_owned(),
			rng,
			library,
			login,
			at: now() + 100_000,
		};
		laptop.set_password(personal, login, "hunter2");
		laptop
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Sets a login's password, with the secrets unlocked.
	fn set_password(&mut self, space: SpaceId, login: EntityId, password: &str) {
		let at = self.tick();
		self.library
			.edit_entity(
				space,
				login,
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

	/// Makes a recovery code, with the secrets if `secrets` says so: the person wrote it
	/// down and typed it back.
	fn make_code(&mut self, secrets: bool) -> RecoveryCode {
		let at = self.tick();
		let code = RecoveryCode::generate(&mut self.rng);
		self.library
			.add_recovery_code(&code, secrets, at, &mut self.rng)
			.expect("names the code");
		code
	}

	/// A backup of every space, made and updated here.
	fn backup(&mut self) -> PathBuf {
		let folder = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, backup", self.name),
		)
		.join("Backup");
		let at = self.tick();
		let backup = self
			.library
			.add_backup(
				&folder,
				&NewBackup {
					name: "Blue stick".to_owned(),
					..NewBackup::default()
				},
				at,
				&mut self.rng,
			)
			.expect("adds")
			.backup;
		let at = self.tick();
		self.library
			.update_backup(&backup, &folder, &|| at, &|| false)
			.expect("updates")
			.expect("not held by another");
		folder
	}

	/// Starts recovering on a new computer from the backup in `folder` with `code`.
	fn start_recovery<'a>(
		&mut self,
		folder: &'a Path,
		code: &'a RecoveryCode,
	) -> Result<(PathBuf, Recovering<'a>), NodeError> {
		let directory = self.new_computer();
		let joining = Joining::new("new laptop", "nib", &mut self.rng).expect("names");
		let at = self.tick();
		Library::start_recovery(
			&directory,
			(folder, code),
			joining,
			&protection(),
			&|| at,
			&mut self.rng,
		)
		.map(|recovering| (directory, recovering))
	}

	/// Finishes a recovery, the secrets protected as `secrets` says.
	fn finish(
		&mut self,
		recovering: Recovering<'_>,
		secrets: Option<&NewProtection<'_>>,
	) -> Result<(Library, Recovered), NodeError> {
		let at = self.tick();
		recovering.finish(secrets, &|| at, &mut self.rng)
	}

	/// An empty folder on a new computer's disk.
	fn new_computer(&self) -> PathBuf {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{}, new", self.name),
		);
		on_disk(&directory, &format!("{} new", self.name));
		directory
	}
}

/// The passwords that a login in `space` shows on `library`.
fn passwords(library: &Library, space: SpaceId, login: EntityId) -> Vec<String> {
	library
		.entity(space, login, common::this_day())
		.expect("reads")
		.expect("the login")
		.values(&field("local.password"))
		.iter()
		.filter_map(|shown| shown.value.as_text().map(str::to_owned))
		.collect()
}

/// The numbers of a space's secrets keys that `library` holds while its secrets are
/// unlocked, and the one it seals new secret values with.
fn secrets_keys(library: &Library, space: SpaceId) -> (Vec<u64>, Option<u64>) {
	library.secrets_key_numbers(space).expect("a space")
}

#[test]
fn a_code_made_with_the_secrets_brings_them_back() {
	let mut laptop = Laptop::new("recovering the secrets");
	let personal = laptop.library.personal_space();
	let code = laptop.make_code(true);
	let folder = laptop.backup();
	let (lost, _) = secrets_keys(&laptop.library, personal);
	// The laptop is lost.
	let (directory, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	assert!(recovering.holds_secrets());
	let (mut library, recovered) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert_eq!(
		recovered.secrets,
		RecoveredSecrets::Taken { keys: lost.len() }
	);
	assert_eq!(library.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(
		passwords(&library, personal, laptop.login),
		["hunter2".to_owned()]
	);
	// It seals with a key of its own, which the lost laptop never held, and which the code
	// opens too.
	let (held, writing) = secrets_keys(&library, personal);
	let writing = writing.expect("a key to seal with");
	assert!(!lost.contains(&writing), "{lost:?} {writing}");
	assert_eq!(held.len(), lost.len() + 1);
	let check = library.check_recovery_code(&code).expect("checks");
	let in_personal = check
		.spaces
		.iter()
		.find(|keys| keys.space == personal)
		.expect("the personal space names it");
	assert_eq!(in_personal.secrets, Some((held, vec![])));
	assert!(library.recovery_codes().expect("lists")[0].holds_secrets);
	// The secrets open with their own passphrase from now on, never with the library's.
	library.lock_secrets();
	assert!(
		library
			.unlock_secrets(&Unlocker::Passphrase("battery staple"))
			.is_err()
	);
	library
		.unlock_secrets(&Unlocker::Passphrase(NEW_SECRETS))
		.expect("unlocks");
	assert_eq!(
		passwords(&library, personal, laptop.login),
		["hunter2".to_owned()]
	);
	drop(library);
	assert!(Library::exists(&directory));
}

#[test]
fn a_code_made_without_the_secrets_brings_none_back() {
	let mut laptop = Laptop::new("recovering, a code without the secrets");
	let code = laptop.make_code(false);
	let folder = laptop.backup();
	let (_, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	assert!(!recovering.holds_secrets());
	let (library, recovered) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert_eq!(recovered.secrets, RecoveredSecrets::NotInCode);
	assert_eq!(library.secrets_protection().expect("reads"), None);
	assert_eq!(
		passwords(&library, library.personal_space(), laptop.login),
		[] as [String; 0]
	);
}

#[test]
fn secrets_left_on_purpose_are_not_taken() {
	let mut laptop = Laptop::new("recovering, the secrets left");
	let code = laptop.make_code(true);
	let folder = laptop.backup();
	let (_, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	assert!(recovering.holds_secrets());
	let (library, recovered) = laptop.finish(recovering, None).expect("recovers");
	assert_eq!(recovered.secrets, RecoveredSecrets::Left);
	assert_eq!(library.secrets_protection().expect("reads"), None);
}

#[test]
fn the_secrets_of_the_persons_other_spaces_come_back_too() {
	let mut laptop = Laptop::new("recovering the secrets of another space");
	let at = laptop.tick();
	let notes = laptop
		.library
		.create_space("Notes", at, &mut laptop.rng)
		.expect("makes it");
	let at = laptop.tick();
	laptop
		.library
		.define_type(notes, &login_type(), at, &mut laptop.rng)
		.expect("defines");
	let at = laptop.tick();
	let login = laptop
		.library
		.create_entity(
			notes,
			&[LOGIN],
			vec![(field("local.title"), Value::Text("Shop".to_owned()))],
			at,
			&mut laptop.rng,
		)
		.expect("creates");
	laptop.set_password(notes, login, "correct horse");
	let code = laptop.make_code(true);
	let folder = laptop.backup();
	let (lost, _) = secrets_keys(&laptop.library, notes);
	assert_ne!(lost, [] as [u64; 0]);
	let (_, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	let (library, recovered) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert!(recovered.spaces.contains(&notes), "{recovered:?}");
	assert_eq!(
		passwords(&library, notes, login),
		["correct horse".to_owned()]
	);
	// There too, it seals with a key that the lost laptop never held.
	let (_, writing) = secrets_keys(&library, notes);
	let writing = writing.expect("a key to seal with");
	assert!(!lost.contains(&writing), "{lost:?} {writing}");
}

#[test]
fn a_kept_device_that_holds_the_secrets_reads_those_set_after_the_recovery() {
	let mut laptop = Laptop::new("keeping a device with the secrets");
	let personal = laptop.library.personal_space();
	// A desk that holds the secrets too.
	let directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"keeping a device with the secrets, desk",
	);
	std::fs::remove_dir(&directory).expect("removes");
	let joining = Joining::new("desk", "nib", &mut laptop.rng)
		.expect("names")
		.asking_for_secrets(protected("desk secrets"), &mut laptop.rng);
	let pairing = PairingCode::generate(&mut laptop.rng);
	let (paired, joined) = common::pair_joining(
		&mut laptop.library,
		&directory,
		(joining, true),
		(&pairing, &pairing),
		&mut laptop.rng,
	);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	let code = laptop.make_code(true);
	let folder = laptop.backup();
	let at = laptop.tick();
	let (first, second) = common::sync_in_memory(&mut desk, &mut laptop.library, at, (1, 0));
	first.expect("the desk syncs");
	second.expect("the laptop syncs");
	assert_eq!(
		passwords(&desk, personal, laptop.login),
		["hunter2".to_owned()]
	);
	// The laptop is lost, and the desk is at hand.
	let (_, mut recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	let kept: Vec<EntityId> = recovering
		.others_named("desk")
		.expect("names it")
		.iter()
		.map(|replica| replica.id)
		.collect();
	recovering.keep(&kept).expect("keeps");
	let (mut library, recovered) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert_eq!(recovered.kept.len(), 1);
	assert_eq!(recovered.revoked.len(), 1);
	let (_, writing) = secrets_keys(&library, personal);
	let writing = writing.expect("a key to seal with");
	// The desk learns of the new device from the backup; then a password set on either after
	// the recovery reaches the other, sealed with a key that the lost laptop never held.
	let stick = desk.backups().expect("lists").remove(0);
	let at = laptop.tick();
	desk.update_backup(&stick, &folder, &|| at, &|| false)
		.expect("updates")
		.expect("not held by another");
	let at = laptop.tick();
	library
		.edit_entity(
			personal,
			laptop.login,
			vec![Edit::Set(
				field("local.password"),
				vec![Value::Text("set on the new laptop".to_owned())],
			)],
			&today(),
			at,
			&mut laptop.rng,
		)
		.expect("sets the password");
	let at = laptop.tick();
	let (first, second) = common::sync_in_memory(&mut desk, &mut library, at, (1, 0));
	first.expect("the desk syncs");
	second.expect("the new laptop syncs");
	assert_eq!(
		passwords(&desk, personal, laptop.login),
		["set on the new laptop".to_owned()]
	);
	let (held, _) = secrets_keys(&desk, personal);
	assert!(held.contains(&writing), "{held:?} {writing}");
	let at = laptop.tick();
	desk.edit_entity(
		personal,
		laptop.login,
		vec![Edit::Set(
			field("local.password"),
			vec![Value::Text("set on the desk".to_owned())],
		)],
		&today(),
		at,
		&mut laptop.rng,
	)
	.expect("sets the password");
	let at = laptop.tick();
	let (first, second) = common::sync_in_memory(&mut desk, &mut library, at, (1, 0));
	first.expect("the desk syncs");
	second.expect("the new laptop syncs");
	assert_eq!(
		passwords(&library, personal, laptop.login),
		["set on the desk".to_owned()]
	);
}

/// A desk paired with the laptop, which never held the secrets: its folder and its record.
fn pair_desk(laptop: &mut Laptop) -> (PathBuf, EntityId) {
	let directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&format!("{}, desk", laptop.name),
	);
	std::fs::remove_dir(&directory).expect("removes");
	let pairing = PairingCode::generate(&mut laptop.rng);
	let (paired, joined) = pair_named(
		&mut laptop.library,
		&directory,
		"desk",
		(&pairing, &pairing),
		&mut laptop.rng,
	);
	paired.expect("paired");
	let record = joined
		.expect("joined")
		.library
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.expect("its own")
		.id;
	(directory, record)
}

/// Has the desk in `directory`, infected, make up a secrets key and say that it holds it, as
/// it can by writing its own files; with `more` of its making in the same change, which may
/// give the key further. Then it syncs with the laptop. Returns the key, its number and its
/// hash.
fn make_up_secrets(
	laptop: &mut Laptop,
	(directory, desk): (&Path, EntityId),
	more: impl FnOnce(&mut TestRng, (u64, &Key, nibkeep_base::Hash)) -> Vec<nibkeep_model::Operation>,
) {
	let personal = laptop.library.personal_space();
	let made_up = Key::generate(&mut laptop.rng);
	let epoch = 7;
	let hash = key_hash(&made_up);
	let decryption = DecryptionKey::generate(&mut laptop.rng);
	let to_desk = SealedSecretsKey::seal(
		&decryption.encryption_key(),
		personal,
		desk,
		(epoch, &made_up, hash),
		(personal, epoch, &made_up),
		&mut laptop.rng,
	);
	let mut operations = vec![
		nibkeep_governance::state_secrets_key(desk, &decryption.encryption_key(), &mut laptop.rng),
		nibkeep_governance::state_secrets_from(desk, desk, &mut laptop.rng),
		nibkeep_governance::give_secrets_key(desk, &to_desk, &mut laptop.rng),
	];
	operations.extend(more(&mut laptop.rng, (epoch, &made_up, hash)));
	let at = laptop.tick();
	common::forge(
		directory,
		&Unlocker::Passphrase("battery staple"),
		(personal, operations),
		(None, at),
		&mut laptop.rng,
	);
	let mut desk =
		Library::open(directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	let at = laptop.tick();
	let (first, second) = common::sync_in_memory(&mut desk, &mut laptop.library, at, (1, 0));
	first.expect("the desk syncs");
	second.expect("the laptop syncs");
}

/// Recovers with `code` after the laptop and the desk are lost, and checks that nothing of
/// the desk's making came back.
fn recover_nothing_made_up(laptop: &mut Laptop, code: &RecoveryCode) {
	let folder = laptop.backup();
	let (_, recovering) = laptop
		.start_recovery(&folder, code)
		.expect("starts recovering");
	assert!(!recovering.holds_secrets());
	let (library, recovered) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert_eq!(recovered.secrets, RecoveredSecrets::NotInCode);
	assert_eq!(library.secrets_protection().expect("reads"), None);
}

#[test]
fn a_key_that_another_device_gave_the_code_of_its_own_making_is_not_taken() {
	let mut laptop = Laptop::new("recovering, a key made up for the code");
	let personal = laptop.library.personal_space();
	let (directory, desk) = pair_desk(&mut laptop);
	// A code made without the secrets; the desk gives it its made-up key, linked with that
	// key itself.
	let code = laptop.make_code(false);
	let record = laptop.library.recovery_codes().expect("lists")[0].record;
	let encryption = code.secrets_decryption_key().encryption_key();
	make_up_secrets(&mut laptop, (&directory, desk), |rng, key| {
		let link = (personal, key.0, key.1);
		let sealed = SealedSecretsKey::seal(&encryption, personal, record, key, link, rng);
		vec![nibkeep_governance::give_secrets_key(record, &sealed, rng)]
	});
	// The laptop does not count the code as holding the secrets, nor does a recovery.
	assert!(!laptop.library.recovery_codes().expect("lists")[0].holds_secrets);
	recover_nothing_made_up(&mut laptop, &code);
}

#[test]
fn a_codes_statement_copied_onto_another_record_makes_nobody_its_maker() {
	let mut laptop = Laptop::new("recovering, a code's statement copied");
	let personal = laptop.library.personal_space();
	let (directory, desk) = pair_desk(&mut laptop);
	let code = laptop.make_code(false);
	// The desk copies the code's statement onto a record for another member, which its
	// signature allows, as that names only the code's keys; it states it there first, and
	// gives that record its made-up key.
	let named = laptop.library.recovery_codes().expect("lists")[0].record;
	// What the copy holds: the code's signature names only its keys, so it is the same as
	// the code's own statement for that member.
	let copied = RecoveryStatement::sign(&code, EntityId::random(&mut laptop.rng));
	let record = copied.record(personal);
	assert_ne!(record, named);
	let encryption = copied.secrets_encryption.clone();
	make_up_secrets(&mut laptop, (&directory, desk), |rng, key| {
		let link = (personal, key.0, key.1);
		let sealed = SealedSecretsKey::seal(&encryption, personal, record, key, link, rng);
		vec![
			nibkeep_governance::state_recovery_code(personal, &copied, rng),
			nibkeep_governance::give_secrets_key(record, &sealed, rng),
		]
	});
	recover_nothing_made_up(&mut laptop, &code);
	// Nor does the copy, newer, replace the person's own code, which stands.
	let codes = laptop.library.recovery_codes().expect("lists");
	let own = codes
		.iter()
		.find(|listed| listed.record == named)
		.expect("named");
	assert!(!own.revoked && own.replaced_by.is_none(), "{own:?}");
}

#[test]
fn the_secrets_passphrase_is_never_the_librarys_and_a_failed_recovery_leaves_nothing() {
	let mut laptop = Laptop::new("recovering, the library's passphrase");
	let code = laptop.make_code(true);
	let folder = laptop.backup();
	let (directory, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	let refused = laptop.finish(recovering, Some(&protection()));
	assert!(
		matches!(refused, Err(NodeError::LibraryPassphrase)),
		"{:?}",
		refused.err()
	);
	assert!(!Library::exists(&directory));
	// A recovery started and dropped leaves nothing either.
	let (directory, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	drop(recovering);
	assert!(!Library::exists(&directory));
	// And one run again goes through.
	let (_, recovering) = laptop
		.start_recovery(&folder, &code)
		.expect("starts recovering");
	let (library, _) = laptop
		.finish(recovering, Some(&protected(NEW_SECRETS)))
		.expect("recovers");
	assert_eq!(library.secrets_state().expect("reads"), SecretsState::Here);
}
