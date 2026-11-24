//! Secrets: the values of secret fields, sealed with the space's secrets key, which the
//! library has only once the person sets it up, and opens only once they confirm
//! (Design 6.9).

mod common;

use common::{LOGIN, library, login_type, now, passwords, this_day, today};
use nibkeep_crypto::{Key, PassphraseParams};
use nibkeep_keys::{KeyFile, SecretsKeys};
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{
	AskedFor, Edit, ExportKind, Joining, KeysError, Library, NetworkOptions, NewDevice,
	NewProtection, NodeError, Protection, SecretsState, Unlocker,
};
use nibkeep_testkit::TestRng;

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

fn secrets_passphrase() -> NewProtection<'static> {
	NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"))
}

const SECRETS: Unlocker<'static> = Unlocker::Passphrase("tr0ub4dor");

/// A library with the login type, whose password is secret, and a login without one.
fn with_login(
	test: &str,
	rng: &mut TestRng,
) -> (std::path::PathBuf, Library, nibkeep_base::EntityId) {
	let (directory, mut library) = library(test, rng);
	let personal = library.personal_space();
	library
		.define_type(personal, &login_type(), now(), rng)
		.expect("defines");
	let login = library
		.create_entity(
			personal,
			&[LOGIN],
			vec![(field("local.title"), text("Bank"))],
			now(),
			rng,
		)
		.expect("creates");
	(directory, library, login)
}

fn set_password(
	library: &mut Library,
	login: nibkeep_base::EntityId,
	password: &str,
	at: u64,
	rng: &mut TestRng,
) -> Result<(), NodeError> {
	let personal = library.personal_space();
	library.edit_entity(
		personal,
		login,
		vec![Edit::Set(field("local.password"), vec![text(password)])],
		&today(),
		at,
		rng,
	)
}

#[test]
fn a_library_has_no_secrets_until_they_are_set_up() {
	let mut rng = TestRng::new("no secrets yet");
	let (_, mut library, login) = with_login("no secrets yet", &mut rng);
	assert_eq!(library.secrets_protection().expect("reads"), None);
	assert!(!library.secrets_unlocked(library.personal_space()));
	// The library's own passphrase would make the two layers one.
	let libraries = NewProtection::Passphrase(
		"correct horse",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	assert!(matches!(
		library.set_up_secrets(&libraries, now() + 500, &mut rng),
		Err(NodeError::LibraryPassphrase)
	));
	assert_eq!(library.secrets_protection().expect("reads"), None);
	let error = set_password(&mut library, login, "hunter2", now(), &mut rng).expect_err("refused");
	assert!(
		matches!(
			error,
			NodeError::Store(nibkeep_store::StoreError::Log(
				nibkeep_log::LogError::SecretsLocked
			))
		),
		"{error}"
	);
	assert!(matches!(
		library.unlock_secrets(&SECRETS),
		Err(NodeError::Keys(KeysError::NoSecrets))
	));
}

#[test]
fn secret_values_open_only_with_the_secrets_passphrase() {
	let mut rng = TestRng::new("secrets set up");
	let (directory, mut library, login) = with_login("secrets set up", &mut rng);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	assert!(matches!(
		library.secrets_protection().expect("reads"),
		Some(Protection::Passphrase { .. })
	));
	assert!(library.secrets_unlocked(library.personal_space()));
	set_password(&mut library, login, "hunter2", now() + 1_000, &mut rng).expect("sets");
	assert_eq!(passwords(&library, login), [text("hunter2")]);
	library.lock_secrets();
	assert_eq!(passwords(&library, login), []);
	assert!(!library.secrets_unlocked(library.personal_space()));
	drop(library);
	assert!(
		!nibkeep_testkit::holds_anywhere(&directory, b"hunter2"),
		"stored sealed"
	);

	// Opened again, the library's passphrase opens everything but the secrets.
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert_eq!(passwords(&library, login), []);
	assert!(matches!(
		library.unlock_secrets(&Unlocker::Passphrase("correct horse")),
		Err(NodeError::Keys(KeysError::WrongPassphrase))
	));
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(passwords(&library, login), [text("hunter2")]);
	assert!(matches!(
		library.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng),
		Err(NodeError::SecretsSetUp)
	));
}

#[test]
fn a_new_secret_value_replaces_the_old_one_which_history_keeps() {
	let mut rng = TestRng::new("secret replaced");
	let (directory, mut library, login) = with_login("secret replaced", &mut rng);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	set_password(&mut library, login, "hunter2", now() + 1_000, &mut rng).expect("sets");
	set_password(
		&mut library,
		login,
		"correct battery",
		now() + 2_000,
		&mut rng,
	)
	.expect("sets");
	assert_eq!(passwords(&library, login), [text("correct battery")]);
	drop(library);

	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	let personal = library.personal_space();
	let password = field("local.password");
	let locked = library
		.history(personal, login, Some(&password))
		.expect("reads")
		.expect("the login");
	assert!(locked.fields.iter().all(|past| past.field != password));
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(passwords(&library, login), [text("correct battery")]);
	let history = library
		.history(personal, login, Some(&password))
		.expect("reads")
		.expect("the login");
	let values: Vec<(Value, bool)> = history
		.fields
		.iter()
		.filter(|past| past.field == password)
		.flat_map(|past| past.values.iter())
		.map(|past| (past.value.clone(), past.replaced.is_some()))
		.collect();
	assert_eq!(
		values,
		[(text("hunter2"), true), (text("correct battery"), false)]
	);
}

#[test]
fn secrets_set_up_by_another_program_are_kept() {
	let mut rng = TestRng::new("secrets set up elsewhere");
	let (directory, mut library, login) = with_login("secrets set up elsewhere", &mut rng);
	// While the library is open, another program gives it secrets keys, as `nib secret`
	// run from another terminal would.
	let path = common::replica_folder(&directory).join("keys.cbor");
	let mut key_file = KeyFile::read(&path).expect("reads");
	let keys = SecretsKeys {
		spaces: [(
			library.personal_space(),
			[(0, Key::generate(&mut rng))].into(),
		)]
		.into(),
		decryption: nibkeep_crypto::DecryptionKey::generate(&mut rng),
	};
	let elsewhere =
		NewProtection::Passphrase("elsewhere", PassphraseParams::new(64, 1, 1).expect("valid"));
	key_file
		.set_secrets(&keys, &elsewhere, &mut rng)
		.expect("seals");
	key_file.write(&path).expect("writes");
	assert!(matches!(
		library.secrets_protection().expect("reads"),
		Some(Protection::Passphrase { .. })
	));
	// Keys of its own would replace them, and seal values that nothing opens.
	assert!(matches!(
		library.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng),
		Err(NodeError::SecretsSetUp)
	));
	assert_eq!(KeyFile::read(&path).expect("reads"), key_file);
	library
		.unlock_secrets(&Unlocker::Passphrase("elsewhere"))
		.expect("unlocks");
	set_password(&mut library, login, "hunter2", now() + 1_000, &mut rng).expect("sets");
	assert_eq!(passwords(&library, login), [text("hunter2")]);
}

#[test]
fn secret_values_are_purged_once_unlocked_and_never_exported() {
	let mut rng = TestRng::new("secret purged");
	let (directory, mut library, login) = with_login("secret purged", &mut rng);
	let personal = library.personal_space();
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	set_password(&mut library, login, "hunter2", now() + 1_000, &mut rng).expect("sets");
	set_password(
		&mut library,
		login,
		"correct battery",
		now() + 2_000,
		&mut rng,
	)
	.expect("sets");

	// Unlocked or not, an export leaves them out.
	let export =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secret purged, exported")
			.join("export");
	library
		.export(personal, &export, ExportKind::Full)
		.expect("exports");
	assert!(!nibkeep_testkit::holds_anywhere(&export, b"hunter2"));
	assert!(!nibkeep_testkit::holds_anywhere(
		&export,
		b"correct battery"
	));

	let password = field("local.password");
	library.lock_secrets();
	assert!(library.purgeable(personal, login, &password, None).is_err());
	library.unlock_secrets(&SECRETS).expect("unlocks");
	let purge = library
		.purge(
			personal,
			login,
			&password,
			Some(&text("hunter2")),
			now() + 3_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!((purge.statements.len(), purge.secret), (1, 1));
	drop(library);
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(passwords(&library, login), [text("correct battery")]);
	let history = library
		.history(personal, login, Some(&password))
		.expect("reads")
		.expect("the login");
	assert!(
		history
			.fields
			.iter()
			.flat_map(|past| past.values.iter())
			.all(|past| past.value != text("hunter2"))
	);
}

#[test]
fn values_written_before_their_field_was_secret_are_purged_without_secrets() {
	let mut rng = TestRng::new("plain purged");
	let (_, mut library) = library("plain purged", &mut rng);
	let personal = library.personal_space();
	let password = field("local.password");
	library
		.define_type(
			personal,
			&TypeDef::new(TypeName::from_static("local.note"), 1)
				.with_field(password.clone(), FieldDef::new(ValueKind::Text)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let note = library
		.create_entity(
			personal,
			&[TypeName::from_static("local.note")],
			vec![(password.clone(), text("hunter2"))],
			now(),
			&mut rng,
		)
		.expect("creates");
	// Another type makes the field secret, and the most careful definition counts.
	library
		.define_type(
			personal,
			&TypeDef::new(LOGIN, 1)
				.with_field(password.clone(), FieldDef::new(ValueKind::Text).secret()),
			now() + 1_000,
			&mut rng,
		)
		.expect("defines");
	assert!(
		library
			.secret_fields(personal)
			.expect("a space")
			.contains(&password)
	);
	let purge = library
		.purge(personal, note, &password, None, now() + 2_000, &mut rng)
		.expect("purges what is plain");
	assert_eq!((purge.statements.len(), purge.secret), (1, 0));
}

#[test]
fn a_replica_that_joined_makes_no_secrets_of_its_own_where_others_kept_some() {
	let mut rng = TestRng::new("secrets elsewhere");
	let (_, mut laptop, login) = with_login("secrets elsewhere laptop", &mut rng);
	laptop
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	set_password(&mut laptop, login, "hunter2", now(), &mut rng).expect("sets");
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secrets elsewhere desk");
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	// The desk gets no secrets keys; new ones of its own would seal what no other replica
	// reads, and leave the laptop's sealed values closed to it.
	assert_eq!(desk.secrets_protection().expect("reads"), None);
	assert!(desk.secrets_elsewhere().expect("reads"));
	assert!(!laptop.secrets_elsewhere().expect("reads"));
	assert!(matches!(
		desk.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng),
		Err(NodeError::SecretsElsewhere)
	));
	assert_eq!(desk.secrets_protection().expect("reads"), None);
}

/// A laptop with a login whose password is set, and a desk paired with it, which gets no
/// secrets keys; the desk's folder, and the replicas of both.
struct Devices {
	laptop: Library,
	desk: Library,
	login: nibkeep_base::EntityId,
	desk_directory: std::path::PathBuf,
	laptop_replica: nibkeep_base::EntityId,
	desk_replica: nibkeep_base::EntityId,
}

fn laptop_and_desk(test: &str, rng: &mut TestRng) -> Devices {
	let (_, mut laptop, login) = with_login(&format!("{test} laptop"), rng);
	laptop
		.set_up_secrets(&secrets_passphrase(), now() + 500, rng)
		.expect("sets up");
	set_password(&mut laptop, login, "hunter2", now() + 1_000, rng).expect("sets");
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{test} desk"));
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, rng);
	let desk_replica = paired.expect("paired").replica;
	let laptop_replica = laptop.replicas().expect("lists")[0].id;
	Devices {
		laptop,
		desk: joined.expect("joined").library,
		login,
		desk_directory,
		laptop_replica,
		desk_replica,
	}
}

/// Syncs two libraries in memory, as `nib sync` does, and checks that both sides synced.
fn synced(first: &mut Library, second: &mut Library, at: u64, sides: (usize, usize)) {
	let (initiated, responded) = common::sync_in_memory(first, second, at, sides);
	initiated.expect("synced");
	responded.expect("answered");
}

/// The secrets of the desk's own, never the library's nor the laptop's.
fn desk_secrets() -> NewProtection<'static> {
	NewProtection::Passphrase(
		"desk secrets",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	)
}

/// What the library lists of a replica: whether it holds the secrets, and whether it asks.
fn secrets_of(library: &Library, replica: nibkeep_base::EntityId) -> (bool, bool) {
	let replicas = library.replicas().expect("lists");
	let listed = replicas
		.iter()
		.find(|listed| listed.id == replica)
		.expect("listed");
	(listed.holds_secrets, listed.asks_for_secrets)
}

#[test]
fn a_device_that_asks_is_given_the_secrets_and_reads_them() {
	let mut rng = TestRng::new("secrets given");
	let Devices {
		mut laptop,
		mut desk,
		login,
		laptop_replica,
		desk_replica,
		..
	} = laptop_and_desk("secrets given", &mut rng);
	assert_eq!(laptop.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(
		desk.secrets_state().expect("reads"),
		SecretsState::Elsewhere
	);
	assert_eq!(
		secrets_of(&desk, laptop_replica),
		(true, false),
		"the laptop holds them"
	);
	assert_eq!(secrets_of(&desk, desk_replica), (false, false));

	// The desk asks the laptop, with secrets of its own; not itself, nor with the library's
	// passphrase.
	assert!(matches!(
		desk.ask_for_secrets(desk_replica, &desk_secrets(), now() + 3_000, &mut rng),
		Err(NodeError::WhichDevice(_))
	));
	assert!(matches!(
		desk.ask_for_secrets(
			laptop_replica,
			&common::protection(),
			now() + 3_000,
			&mut rng
		),
		Err(NodeError::LibraryPassphrase)
	));
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks");
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Asked);
	assert!(desk.secrets_elsewhere().expect("reads"));
	assert!(matches!(
		desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng),
		Err(NodeError::SecretsAsked)
	));
	assert_eq!(passwords(&desk, login), Vec::<Value>::new());
	assert!(matches!(
		laptop.give_secrets(desk_replica, now() + 4_000, &mut rng),
		Err(NodeError::NotAsked)
	));
	synced(&mut desk, &mut laptop, now() + 5_000, (3, 4));
	assert_eq!(
		secrets_of(&laptop, desk_replica),
		(false, true),
		"the desk asks"
	);

	// Giving needs the secrets unlocked: the person confirms on the laptop.
	laptop.lock_secrets();
	assert!(matches!(
		laptop.give_secrets(desk_replica, now() + 6_000, &mut rng),
		Err(NodeError::SecretsLocked)
	));
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(
		laptop
			.give_secrets(desk_replica, now() + 6_000, &mut rng)
			.expect("gives"),
		1
	);
	assert_eq!(
		laptop
			.give_secrets(desk_replica, now() + 6_500, &mut rng)
			.expect("gives nothing more"),
		0
	);
	assert_eq!(secrets_of(&laptop, desk_replica), (true, false));

	// The desk takes them as it syncs, while its secrets are unlocked, and reads the password.
	synced(&mut desk, &mut laptop, now() + 7_000, (5, 6));
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
	// Its own passphrase opens them from now on; the laptop's does not.
	desk.lock_secrets();
	assert_eq!(passwords(&desk, login), Vec::<Value>::new());
	assert!(desk.unlock_secrets(&SECRETS).is_err());
	desk.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
	// What it writes, the laptop reads.
	set_password(&mut desk, login, "correct horse", now() + 8_000, &mut rng).expect("sets");
	synced(&mut desk, &mut laptop, now() + 9_000, (7, 8));
	assert_eq!(passwords(&laptop, login), [text("correct horse")]);
}

#[test]
fn a_device_that_asks_takes_the_secrets_when_it_next_unlocks_them() {
	let mut rng = TestRng::new("secrets taken later");
	let Devices {
		mut laptop,
		mut desk,
		login,
		laptop_replica,
		desk_replica,
		..
	} = laptop_and_desk("secrets taken later", &mut rng);
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	laptop
		.give_secrets(desk_replica, now() + 5_000, &mut rng)
		.expect("gives");
	// The desk's secrets are locked as the keys arrive: it takes them once they are not.
	desk.lock_secrets();
	synced(&mut desk, &mut laptop, now() + 6_000, (3, 4));
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(passwords(&desk, login), Vec::<Value>::new());
	desk.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	assert_eq!(
		passwords(&desk, login),
		Vec::<Value>::new(),
		"taken as they settle"
	);
	desk.settle_secrets(now() + 7_000, &mut rng)
		.expect("settles");
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
}

/// The passwords of a login in `space`, as the library shows them today.
fn passwords_in(
	library: &Library,
	space: nibkeep_base::SpaceId,
	login: nibkeep_base::EntityId,
) -> Vec<Value> {
	library
		.entity(space, login, this_day())
		.expect("reads")
		.expect("the login")
		.values(&field("local.password"))
		.iter()
		.map(|shown| shown.value.clone())
		.collect()
}

/// The laptop, which holds the secrets, the desk, which holds none, and Work, a space of
/// theirs besides the personal one, where the laptop keeps a password.
struct WithWork {
	laptop: Library,
	desk: Library,
	login: nibkeep_base::EntityId,
	work: nibkeep_base::SpaceId,
	payroll: nibkeep_base::EntityId,
	laptop_replica: nibkeep_base::EntityId,
	desk_replica: nibkeep_base::EntityId,
}

fn with_work(test: &str, rng: &mut TestRng) -> WithWork {
	let (_, mut laptop, login) = with_login(&format!("{test}, laptop"), rng);
	laptop
		.set_up_secrets(&secrets_passphrase(), now() + 500, rng)
		.expect("sets up");
	set_password(&mut laptop, login, "hunter2", now() + 1_000, rng).expect("sets");
	let work = laptop
		.create_space("Work", now() + 1_100, rng)
		.expect("makes it");
	laptop
		.define_type(work, &login_type(), now() + 1_200, rng)
		.expect("defines");
	let payroll = laptop
		.create_entity(
			work,
			&[LOGIN],
			vec![(field("local.title"), text("Payroll"))],
			now() + 1_300,
			rng,
		)
		.expect("creates");
	laptop
		.edit_entity(
			work,
			payroll,
			vec![Edit::Set(
				field("local.password"),
				vec![text("correct horse")],
			)],
			&today(),
			now() + 1_400,
			rng,
		)
		.expect("sets");
	// The desk joins without the secrets, and is listed in Work too: it takes the space with
	// the first sync, and the laptop hears of its listing with the second.
	let mut desk = common::pair_device_as(&mut laptop, (test, "desk"), &NewDevice::computer(), rng);
	synced(&mut desk, &mut laptop, now() + 2_000, (1, 2));
	synced(&mut desk, &mut laptop, now() + 2_100, (3, 4));
	assert!(desk.spaces().contains(&work));
	let (laptop_replica, desk_replica) = (this_replica(&laptop).id, this_replica(&desk).id);
	WithWork {
		laptop,
		desk,
		login,
		work,
		payroll,
		laptop_replica,
		desk_replica,
	}
}

/// What the library lists of this replica.
fn this_replica(library: &Library) -> nibkeep_node::Replica {
	library
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.expect("this one")
}

/// The laptop gives the desk the secrets, as the person chose, with its secrets unlocked as
/// `nib secret give` unlocks them; the desk takes them once both synced. Returns how many keys
/// the laptop gave.
fn give_to_desk(world: &mut WithWork, at: u64, rng: &mut TestRng) -> usize {
	let WithWork {
		laptop,
		desk,
		desk_replica,
		..
	} = world;
	laptop.lock_secrets();
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	laptop.settle_secrets(at, rng).expect("settles");
	let given = laptop
		.give_secrets(*desk_replica, at + 100, rng)
		.expect("gives");
	laptop.lock_secrets();
	desk.lock_secrets();
	synced(desk, laptop, at + 1_000, (11, 12));
	desk.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	desk.settle_secrets(at + 2_000, rng).expect("settles");
	given
}

#[test]
fn a_device_given_the_secrets_takes_those_of_every_space_with_them() {
	let mut rng = TestRng::new("secrets of every space");
	let mut world = with_work("secrets of every space", &mut rng);
	let (laptop_replica, desk_replica) = (world.laptop_replica, world.desk_replica);
	world
		.desk
		.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks");
	synced(&mut world.desk, &mut world.laptop, now() + 4_000, (5, 6));

	// One gift of the person's gives both spaces' keys: the personal space's, which the desk
	// asked for, and Work's, linked with it, as the laptop would pass it on once its secrets
	// next settle.
	assert_eq!(give_to_desk(&mut world, now() + 5_000, &mut rng), 2);
	let WithWork {
		laptop,
		desk,
		login,
		work,
		payroll,
		..
	} = &mut world;
	assert!(desk.secrets_unlocked(*work));
	assert_eq!(passwords(desk, *login), [text("hunter2")]);
	assert_eq!(passwords_in(desk, *work, *payroll), [text("correct horse")]);

	// Given again, it lacks nothing.
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(
		laptop
			.give_secrets(desk_replica, now() + 9_000, &mut rng)
			.expect("gives"),
		0
	);
}

#[test]
fn an_infected_device_cannot_have_another_space_s_secrets_given_to_another_sealed_to_it() {
	for listing in [None, Some(Listing::InTheSameChange)] {
		let test = format!("redirected in a space {listing:?}");
		let mut rng = TestRng::new(&test);
		let mut world = with_work(&test, &mut rng);
		let (laptop_replica, work) = (world.laptop_replica, world.work);
		// A third device of the user's, which holds Work, turns against them.
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{test}, infected"),
		);
		std::fs::remove_dir(&directory).expect("removes");
		let code = nibkeep_node::PairingCode::generate(&mut rng);
		let (paired, joined) = common::pair_named(
			&mut world.laptop,
			&directory,
			"phone",
			(&code, &code),
			&mut rng,
		);
		paired.expect("paired");
		let mut infected = joined.expect("joined").library;
		synced(&mut infected, &mut world.laptop, now() + 2_500, (5, 6));
		synced(&mut infected, &mut world.laptop, now() + 2_600, (7, 8));
		assert!(infected.spaces().contains(&work));
		world
			.desk
			.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
			.expect("asks");
		synced(&mut world.desk, &mut world.laptop, now() + 4_000, (9, 10));

		// In Work, where the desk names nobody it asks, it says that the desk's key to seal
		// the secrets to is a thief's: signed with its own key, or with a key that it lists
		// on the desk's record in the same change.
		let desk_key = this_replica(&world.desk).signing.expect("a key");
		let desk_in_work = nibkeep_governance::replica_record(work, &desk_key);
		let thief = nibkeep_crypto::DecryptionKey::generate(&mut rng);
		let mut operations = Vec::new();
		if listing.is_some() {
			let listed = nibkeep_crypto::SigningKey::generate(&mut rng);
			operations.push(asserting(
				desk_in_work,
				nibkeep_governance::SIGNING_KEY,
				Value::Text(listed.verifying_key().to_text()),
				&mut rng,
			));
		}
		operations.push(asserting(
			desk_in_work,
			nibkeep_governance::SECRETS_ENCRYPTION_KEY,
			sealed_to(&thief),
			&mut rng,
		));
		let NewProtection::Passphrase(passphrase, _) = common::protection() else {
			unreachable!("a passphrase");
		};
		let unlocker = Unlocker::Passphrase(passphrase);
		drop(infected);
		common::forge(
			&directory,
			&unlocker,
			(work, operations),
			(None, now() + 4_500),
			&mut rng,
		);
		let forged = common::current_statements(
			&directory,
			&unlocker,
			work,
			(desk_in_work, &nibkeep_governance::SECRETS_ENCRYPTION_KEY),
		);
		assert!(!forged.is_empty(), "{listing:?}");
		let mut infected = Library::open(&directory, &unlocker).expect("opens");
		synced(&mut infected, &mut world.laptop, now() + 4_800, (13, 14));

		// Only the desk's own key says what to seal to: the gift of Work's key opens on the
		// desk, which reads Work's password.
		assert_eq!(give_to_desk(&mut world, now() + 5_000, &mut rng), 2);
		assert_eq!(
			passwords_in(&world.desk, work, world.payroll),
			[text("correct horse")],
			"{listing:?}"
		);
	}
}

#[test]
fn a_device_takes_the_secrets_only_from_the_one_it_asked() {
	let mut rng = TestRng::new("secrets from the one asked");
	let Devices {
		mut laptop,
		mut desk,
		laptop_replica,
		desk_replica,
		..
	} = laptop_and_desk("secrets from the one asked", &mut rng);
	// A second desk, which holds no secrets, makes up some of its own, as an infected device
	// could, by writing its key file itself.
	let other_directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"secrets from the one asked other",
	);
	std::fs::remove_dir(&other_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &other_directory, &code, &code, &mut rng);
	let other_replica = paired.expect("paired").replica;
	let mut other = joined.expect("joined").library;
	let path = common::replica_folder(&other_directory).join("keys.cbor");
	let mut key_file = KeyFile::read(&path).expect("reads");
	let made_up = SecretsKeys {
		spaces: [(
			other.personal_space(),
			[(0, Key::generate(&mut rng))].into(),
		)]
		.into(),
		decryption: nibkeep_crypto::DecryptionKey::generate(&mut rng),
	};
	key_file
		.set_secrets(&made_up, &desk_secrets(), &mut rng)
		.expect("seals");
	key_file.write(&path).expect("writes");
	other
		.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	other
		.settle_secrets(now() + 2_000, &mut rng)
		.expect("says it holds them");

	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	synced(&mut other, &mut laptop, now() + 5_000, (3, 4));
	assert_eq!(secrets_of(&other, other_replica), (true, false));
	// The desk asked the laptop: the other cannot give it its own.
	assert!(matches!(
		other.give_secrets(desk_replica, now() + 6_000, &mut rng),
		Err(NodeError::WhichDevice(_))
	));
	// Nor can it give the laptop, which takes its secrets from itself, a key to seal with.
	assert!(matches!(
		other.give_secrets(laptop_replica, now() + 6_000, &mut rng),
		Err(NodeError::NotAsked | NodeError::WhichDevice(_))
	));
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Asked);
}

#[test]
fn a_device_that_lost_its_secrets_asks_again_and_one_asks_another() {
	let mut rng = TestRng::new("secrets asked again");
	let Devices {
		mut laptop,
		mut desk,
		login,
		desk_directory,
		laptop_replica,
		desk_replica,
	} = laptop_and_desk("secrets asked again", &mut rng);
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks");
	// Its secrets compartment is lost, as with a key file restored from before.
	let path = common::replica_folder(&desk_directory).join("keys.cbor");
	let mut key_file = KeyFile::read(&path).expect("reads");
	key_file.discard_secrets();
	key_file.write(&path).expect("writes");
	desk.lock_secrets();
	assert_eq!(
		desk.secrets_state().expect("reads"),
		SecretsState::Elsewhere
	);
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 4_000, &mut rng)
		.expect("asks again");
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Asked);
	// Asking from itself, or from a device that holds none, changes nothing.
	assert!(matches!(
		desk.ask_secrets_from(desk_replica, now() + 4_500, &mut rng),
		Err(NodeError::WhichDevice(_))
	));
	desk.ask_secrets_from(laptop_replica, now() + 4_500, &mut rng)
		.expect("asks the same");
	synced(&mut desk, &mut laptop, now() + 5_000, (1, 2));
	laptop
		.give_secrets(desk_replica, now() + 6_000, &mut rng)
		.expect("gives to the new key");
	synced(&mut desk, &mut laptop, now() + 7_000, (3, 4));
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
}

#[test]
fn secrets_that_only_revoked_devices_held_can_be_set_up_anew() {
	let mut rng = TestRng::new("secrets of a lost laptop");
	let Devices {
		mut desk,
		laptop_replica,
		..
	} = laptop_and_desk("secrets of a lost laptop", &mut rng);
	assert_eq!(
		desk.secrets_state().expect("reads"),
		SecretsState::Elsewhere
	);
	desk.revoke(&[laptop_replica], now() + 2_000, &mut rng)
		.expect("revokes the lost laptop");
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::NotSetUp);
	desk.set_up_secrets(&desk_secrets(), now() + 3_000, &mut rng)
		.expect("sets up anew");
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Here);
}

#[test]
fn an_ai_agent_is_never_given_the_secrets() {
	let mut rng = TestRng::new("no secrets for agents");
	let (_, mut laptop, _) = with_login("no secrets for agents laptop", &mut rng);
	laptop
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	let laptop_replica = laptop.replicas().expect("lists")[0].id;
	let agent_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "no secrets for agents");
	std::fs::remove_dir(&agent_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_ai_agent_named(
		&mut laptop,
		&agent_directory,
		("laptop", "Claude"),
		&code,
		&mut rng,
	);
	let agent = paired.expect("paired").replica;
	let mut agent_library = joined.expect("joined").library;
	assert!(matches!(
		agent_library.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 1_000, &mut rng),
		Err(NodeError::AiAgentMayNot(_))
	));
	assert!(matches!(
		laptop.give_secrets(agent, now() + 2_000, &mut rng),
		Err(NodeError::WhichDevice(_))
	));
}

#[test]
fn secrets_set_up_but_not_recorded_say_so_once_they_are_unlocked() {
	let mut rng = TestRng::new("secrets not recorded");
	let (directory, mut library, login) = with_login("secrets not recorded", &mut rng);
	// Secrets keys in the compartment, and records that say nothing of them: as when setting
	// up the secrets was cut short after the key file was written.
	let path = common::replica_folder(&directory).join("keys.cbor");
	let mut key_file = KeyFile::read(&path).expect("reads");
	let keys = SecretsKeys {
		spaces: [(
			library.personal_space(),
			[(0, Key::generate(&mut rng))].into(),
		)]
		.into(),
		decryption: nibkeep_crypto::DecryptionKey::generate(&mut rng),
	};
	key_file
		.set_secrets(&keys, &secrets_passphrase(), &mut rng)
		.expect("seals");
	key_file.write(&path).expect("writes");
	let own = library.replicas().expect("lists")[0].id;
	assert_eq!(library.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(secrets_of(&library, own), (false, false));
	library.unlock_secrets(&SECRETS).expect("unlocks");
	library
		.settle_secrets(now() + 1_000, &mut rng)
		.expect("settles");
	assert_eq!(secrets_of(&library, own), (true, false));
	let changes = library.status().expect("reads").spaces[0].changes;
	library
		.settle_secrets(now() + 2_000, &mut rng)
		.expect("settles again");
	assert_eq!(
		library.status().expect("reads").spaces[0].changes,
		changes,
		"once"
	);
	set_password(&mut library, login, "hunter2", now() + 3_000, &mut rng).expect("sets");
	library.lock_secrets();
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(passwords(&library, login), [text("hunter2")]);
}

/// A laptop with a login whose password is set, whose secrets are set up if `secrets` says;
/// and a desk that pairs with it, asking for the secrets with secrets of its own, which the
/// laptop gives if `give` says so. Returns the laptop, what pairing gave each, and the login.
fn added_asking(
	test: &str,
	(secrets, give): (bool, bool),
	rng: &mut TestRng,
) -> (
	Library,
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
	nibkeep_base::EntityId,
) {
	let (_, mut laptop, login) = with_login(&format!("{test} laptop"), rng);
	if secrets {
		laptop
			.set_up_secrets(&secrets_passphrase(), now() + 500, rng)
			.expect("sets up");
		set_password(&mut laptop, login, "hunter2", now() + 1_000, rng).expect("sets");
	}
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{test} desk"));
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(rng);
	let joining = Joining::new("desk", "Passwords", rng)
		.expect("names")
		.asking_for_secrets(desk_secrets(), rng);
	let (paired, joined) = common::pair_joining(
		&mut laptop,
		&desk_directory,
		(joining, give),
		(&code, &code),
		rng,
	);
	(laptop, paired, joined, login)
}

#[test]
fn a_device_added_with_the_secrets_has_them_at_once() {
	let mut rng = TestRng::new("added with secrets");
	let (laptop, paired, joined, login) =
		added_asking("added with secrets", (true, true), &mut rng);
	let paired = paired.expect("paired");
	assert_eq!(paired.asked_for, AskedFor::SecretsGiven);
	assert_eq!(secrets_of(&laptop, paired.replica), (true, false));
	// The desk took them with its first sync, its secrets unlocked: they are its own.
	let mut desk = joined.expect("joined").library;
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Here);
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
	desk.lock_secrets();
	assert!(desk.unlock_secrets(&SECRETS).is_err());
	desk.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
}

#[test]
fn a_device_added_without_giving_asks_for_the_secrets() {
	let mut rng = TestRng::new("added asking");
	let (mut laptop, paired, joined, login) = added_asking("added asking", (true, false), &mut rng);
	let paired = paired.expect("paired");
	assert_eq!(paired.asked_for, AskedFor::Secrets);
	assert_eq!(secrets_of(&laptop, paired.replica), (false, true));
	let mut desk = joined.expect("joined").library;
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::Asked);
	laptop
		.give_secrets(paired.replica, now() + 71_000, &mut rng)
		.expect("gives");
	synced(&mut desk, &mut laptop, now() + 72_000, (1, 2));
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
}

#[test]
fn a_device_that_asks_as_it_joins_a_library_without_secrets_keeps_none() {
	let mut rng = TestRng::new("added to no secrets");
	let (_, paired, joined, _) = added_asking("added to no secrets", (false, false), &mut rng);
	assert_eq!(paired.expect("paired").asked_for, AskedFor::Nothing);
	let desk = joined.expect("joined").library;
	assert_eq!(desk.secrets_protection().expect("reads"), None);
	assert_eq!(desk.secrets_state().expect("reads"), SecretsState::NotSetUp);
}

#[test]
fn giving_the_secrets_as_a_device_is_added_needs_them_unlocked() {
	let mut rng = TestRng::new("added without unlocking");
	let (_, mut laptop, _) = with_login("added without unlocking laptop", &mut rng);
	laptop
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	laptop.lock_secrets();
	let network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let refused = nibkeep_node::pair_giving_secrets(
		&mut laptop,
		&network,
		&code,
		nibkeep_node::Showing::new(common::TIMEOUT),
		&now,
		&mut rng,
	);
	assert!(matches!(refused, Err(NodeError::SecretsLocked)));
}

#[test]
fn a_device_asks_with_secrets_of_their_own_before_the_code_is_spent() {
	let mut rng = TestRng::new("asks with the library's");
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "asks with the library's");
	std::fs::remove_dir(&directory).expect("removes");
	// The library's passphrase for its secrets too: refused before it looks for the sponsor.
	let library = common::protection();
	let joining = Joining::new("desk", "Passwords", &mut rng)
		.expect("names")
		.asking_for_secrets(library, &mut rng);
	let network = joining.network(NetworkOptions::LOCAL).expect("starts");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let refused = nibkeep_node::join(
		&directory,
		joining,
		&network,
		&code,
		&library,
		common::TIMEOUT,
		&now,
		&mut rng,
	);
	assert!(matches!(refused, Err(NodeError::LibraryPassphrase)));
}

#[test]
fn a_device_seals_its_secrets_before_it_looks_for_the_sponsor() {
	let mut rng = TestRng::new("sealed before pairing");
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "sealed before pairing");
	std::fs::remove_dir(&directory).expect("removes");
	// No keystore here: it fails as a keystore's prompt that the person turns down does,
	// before any sponsor is looked for, whose code it would spend.
	let joining = Joining::new("desk", "Passwords", &mut rng)
		.expect("names")
		.asking_for_secrets(
			NewProtection::Keystore {
				service: "org.nibkeep.test",
			},
			&mut rng,
		);
	let network = joining.network(NetworkOptions::LOCAL).expect("starts");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let refused = nibkeep_node::join(
		&directory,
		joining,
		&network,
		&code,
		&common::protection(),
		common::TIMEOUT,
		&now,
		&mut rng,
	);
	// Not NoSponsor, after looking for 30 seconds.
	assert!(matches!(
		refused,
		Err(NodeError::Keys(KeysError::Keystore(_)))
	));
	assert!(!Library::exists(&directory));
}

/// The laptop and the desk, which asked the laptop for the secrets and was given them if
/// `given` says so, and a third device that the laptop added, which holds none and turns
/// against its user; each synced with the laptop.
struct World {
	devices: Devices,
	/// None only while it writes its files.
	infected: Option<Library>,
	infected_directory: std::path::PathBuf,
	infected_replica: nibkeep_base::EntityId,
}

fn infected(test: &str, given: bool, rng: &mut TestRng) -> World {
	let mut devices = laptop_and_desk(test, rng);
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{test} infected"));
	std::fs::remove_dir(&directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(rng);
	let (paired, joined) = common::pair_named(
		&mut devices.laptop,
		&directory,
		"phone",
		(&code, &code),
		rng,
	);
	let infected_replica = paired.expect("paired").replica;
	let mut infected = joined.expect("joined").library;
	let Devices {
		laptop,
		desk,
		laptop_replica,
		desk_replica,
		..
	} = &mut devices;
	desk.ask_for_secrets(*laptop_replica, &desk_secrets(), now() + 3_000, rng)
		.expect("asks");
	synced(desk, laptop, now() + 4_000, (1, 2));
	if given {
		laptop
			.give_secrets(*desk_replica, now() + 5_000, rng)
			.expect("gives");
		synced(desk, laptop, now() + 6_000, (3, 4));
	}
	synced(&mut infected, laptop, now() + 7_000, (5, 6));
	World {
		devices,
		infected: Some(infected),
		infected_directory: directory,
		infected_replica,
	}
}

impl World {
	/// Has the infected device commit `operations`, signed with `author` or its own key, as
	/// it can by writing its own files, and syncs it with the laptop.
	fn forge(
		&mut self,
		operations: Vec<nibkeep_model::Operation>,
		(author, at): (Option<&nibkeep_crypto::SigningKey>, u64),
		rng: &mut TestRng,
	) {
		let NewProtection::Passphrase(passphrase, _) = common::protection() else {
			unreachable!("a passphrase");
		};
		let unlocker = Unlocker::Passphrase(passphrase);
		// Closed, so that the files are its own.
		let closed = self.infected.take().expect("open");
		let personal = closed.personal_space();
		drop(closed);
		common::forge(
			&self.infected_directory,
			&unlocker,
			(personal, operations),
			(author, at),
			rng,
		);
		let infected = self
			.infected
			.insert(Library::open(&self.infected_directory, &unlocker).expect("opens"));
		synced(infected, &mut self.devices.laptop, at + 500, (7, 8));
	}
}

/// An assertion of a statement of `field` on `entity`.
fn asserting(
	entity: nibkeep_base::EntityId,
	field: FieldName,
	value: Value,
	rng: &mut TestRng,
) -> nibkeep_model::Operation {
	nibkeep_model::Operation::assert(nibkeep_model::Statement::new(
		nibkeep_base::StatementId::random(rng),
		entity,
		field,
		value,
	))
}

/// The text of a key that a secrets key is sealed to.
fn sealed_to(key: &nibkeep_crypto::DecryptionKey) -> Value {
	Value::Text(nibkeep_governance::encryption_key_text(
		&key.encryption_key(),
	))
}

/// A key that the infected device lists on another's record, and whether it signs with it
/// what it says of that record.
#[derive(Clone, Copy, Debug)]
enum Listing {
	InTheSameChange,
	SignedWithTheKeyListed,
}

/// Has the infected device say, with a key that it lists on `record`, the statements of
/// `saying`.
fn say_with_a_key_listed(
	world: &mut World,
	(record, listing): (nibkeep_base::EntityId, Listing),
	saying: Vec<nibkeep_model::Operation>,
	at: u64,
	rng: &mut TestRng,
) {
	let listed = nibkeep_crypto::SigningKey::generate(rng);
	let listing_key = asserting(
		record,
		nibkeep_governance::SIGNING_KEY,
		Value::Text(listed.verifying_key().to_text()),
		rng,
	);
	match listing {
		Listing::InTheSameChange => {
			let mut operations = vec![listing_key];
			operations.extend(saying);
			world.forge(operations, (None, at), rng);
		}
		Listing::SignedWithTheKeyListed => {
			world.forge(vec![listing_key], (None, at), rng);
			world.forge(saying, (Some(&listed), at + 1_000), rng);
		}
	}
}

#[test]
fn an_infected_device_cannot_have_the_secrets_given_to_another_sealed_to_it() {
	for listing in [Listing::InTheSameChange, Listing::SignedWithTheKeyListed] {
		let test = format!("redirected {listing:?}");
		let mut rng = TestRng::new(&test);
		let mut world = infected(&test, false, &mut rng);
		let (laptop_replica, desk_replica) =
			(world.devices.laptop_replica, world.devices.desk_replica);
		assert_eq!(
			secrets_of(&world.devices.laptop, desk_replica),
			(false, true)
		);
		// It says that the desk asks the laptop with a key of the thief's.
		let thief = nibkeep_crypto::DecryptionKey::generate(&mut rng);
		let saying = vec![
			asserting(
				desk_replica,
				nibkeep_governance::SECRETS_ENCRYPTION_KEY,
				sealed_to(&thief),
				&mut rng,
			),
			asserting(
				desk_replica,
				nibkeep_governance::SECRETS_FROM,
				Value::Reference(laptop_replica),
				&mut rng,
			),
		];
		say_with_a_key_listed(
			&mut world,
			(desk_replica, listing),
			saying,
			now() + 8_000,
			&mut rng,
		);
		// Nothing lists the key on the desk's record without that key's own listing, so the
		// desk's own request stands: the person gives it the secrets, sealed to its own key,
		// and nothing to the thief.
		let Devices {
			laptop,
			desk,
			login,
			..
		} = &mut world.devices;
		assert_eq!(
			secrets_of(laptop, desk_replica),
			(false, true),
			"{listing:?}"
		);
		laptop.unlock_secrets(&SECRETS).expect("unlocks");
		laptop
			.give_secrets(desk_replica, now() + 10_000, &mut rng)
			.expect("gives");
		synced(desk, laptop, now() + 11_000, (11, 12));
		assert_eq!(passwords(desk, *login), [text("hunter2")], "{listing:?}");
	}
}

/// The secrets encryption key of a replica, as its record states it.
fn secrets_encryption_key(
	library: &Library,
	replica: nibkeep_base::EntityId,
) -> nibkeep_crypto::EncryptionKey {
	let view = library
		.entity(library.personal_space(), replica, this_day())
		.expect("reads")
		.expect("the record");
	view.fields
		.iter()
		.filter(|shown| shown.field == nibkeep_governance::SECRETS_ENCRYPTION_KEY)
		.flat_map(|shown| &shown.values)
		.find_map(|shown| {
			shown
				.value
				.as_text()
				.and_then(nibkeep_governance::encryption_key_of_text)
		})
		.expect("stated")
}

/// Whom the infected device gives a secrets key of its own making, of the largest number,
/// with a key it lists on another's record: the laptop, which holds the secrets, saying
/// that the laptop takes them from it, or the desk, which asked the laptop, with a key
/// listed on the laptop's record.
#[derive(Clone, Copy, Debug)]
enum Planted {
	OnTheHolder(Listing),
	OnTheAskerThroughTheHolder(Listing),
}

#[test]
fn an_infected_device_cannot_give_a_secrets_key_of_its_making_to_seal_with() {
	for planted in [
		Planted::OnTheHolder(Listing::InTheSameChange),
		Planted::OnTheHolder(Listing::SignedWithTheKeyListed),
		Planted::OnTheAskerThroughTheHolder(Listing::InTheSameChange),
		Planted::OnTheAskerThroughTheHolder(Listing::SignedWithTheKeyListed),
	] {
		let test = format!("planted {planted:?}");
		let mut rng = TestRng::new(&test);
		let mut world = infected(&test, true, &mut rng);
		let (laptop_replica, desk_replica, infected_replica) = (
			world.devices.laptop_replica,
			world.devices.desk_replica,
			world.infected_replica,
		);
		let login = world.devices.login;
		assert_eq!(passwords(&world.devices.desk, login), [text("hunter2")]);
		let (to, listed_on, listing) = match planted {
			Planted::OnTheHolder(listing) => (laptop_replica, laptop_replica, listing),
			Planted::OnTheAskerThroughTheHolder(listing) => (desk_replica, laptop_replica, listing),
		};
		let infected = world.infected.as_ref().expect("open");
		let personal = infected.personal_space();
		let made_up = Key::generate(&mut rng);
		let sealed = nibkeep_governance::SealedSecretsKey::seal(
			&secrets_encryption_key(infected, to),
			personal,
			to,
			(u64::MAX, &made_up, nibkeep_crypto::key_hash(&made_up)),
			(personal, u64::MAX, &made_up),
			&mut rng,
		);
		let mut saying = vec![nibkeep_governance::give_secrets_key(to, &sealed, &mut rng)];
		if to == laptop_replica {
			saying.push(asserting(
				laptop_replica,
				nibkeep_governance::SECRETS_FROM,
				Value::Reference(infected_replica),
				&mut rng,
			));
		}
		say_with_a_key_listed(
			&mut world,
			(listed_on, listing),
			saying,
			now() + 8_000,
			&mut rng,
		);
		let World {
			devices: Devices { laptop, desk, .. },
			..
		} = &mut world;
		synced(desk, laptop, now() + 11_000, (9, 10));
		// Each writes a password, its secrets unlocked; the other reads it, as neither sealed
		// it with the key made up.
		let mut writes = |writer: &mut Library,
		                  (unlocker, password, at): (Unlocker<'_>, &str, u64),
		                  reader: &mut Library| {
			writer.lock_secrets();
			writer.unlock_secrets(&unlocker).expect("unlocks");
			writer.settle_secrets(at, &mut rng).expect("settles");
			set_password(writer, login, password, at + 500, &mut rng).expect("sets");
			synced(writer, reader, at + 1_000, (11, 12));
			assert_eq!(passwords(reader, login), [text(password)], "{planted:?}");
		};
		writes(laptop, (SECRETS, "laptop's", now() + 12_000), desk);
		let desk_secrets = Unlocker::Passphrase("desk secrets");
		writes(desk, (desk_secrets, "desk's", now() + 14_000), laptop);
	}
}

#[test]
fn a_secret_value_that_an_infected_device_writes_without_sealing_it_does_not_count() {
	let mut rng = TestRng::new("written plainly");
	let mut world = infected("written plainly", false, &mut rng);
	let login = world.devices.login;
	let laptop = &world.devices.laptop;
	let sealed = laptop
		.entity(laptop.personal_space(), login, this_day())
		.expect("reads")
		.expect("the login")
		.fields
		.iter()
		.filter(|shown| shown.field == field("local.password"))
		.flat_map(|shown| &shown.values)
		.map(|shown| shown.statement)
		.collect::<Vec<_>>();
	assert_eq!(sealed.len(), 1);
	// A password of its own, beside the one sealed, and one that replaces it.
	let beside = asserting(login, field("local.password"), text("planted"), &mut rng);
	let replacing = nibkeep_model::Operation::replace(
		nibkeep_model::Statement::new(
			nibkeep_base::StatementId::random(&mut rng),
			login,
			field("local.password"),
			text("replaced"),
		),
		sealed,
	);
	world.forge(vec![beside], (None, now() + 8_000), &mut rng);
	world.forge(vec![replacing], (None, now() + 9_000), &mut rng);
	assert_eq!(
		passwords(&world.devices.laptop, login),
		[text("hunter2")],
		"neither counts"
	);
	world.devices.laptop.lock_secrets();
	assert_eq!(passwords(&world.devices.laptop, login), Vec::<Value>::new());
}

#[test]
fn a_key_that_another_device_lists_on_a_record_takes_nothing_from_its_replica() {
	// Without the key's own listing, on a record derived from it, nothing lists it: the
	// laptop keeps its key of its own, and holds and gives the secrets as before.
	let mut rng = TestRng::new("foreign key");
	let mut world = infected("foreign key", false, &mut rng);
	let (laptop_replica, desk_replica) = (world.devices.laptop_replica, world.devices.desk_replica);
	let made_up = nibkeep_crypto::SigningKey::generate(&mut rng).verifying_key();
	let listing = asserting(
		laptop_replica,
		nibkeep_governance::SIGNING_KEY,
		Value::Text(made_up.to_text()),
		&mut rng,
	);
	world.forge(vec![listing], (None, now() + 8_000), &mut rng);
	let Devices {
		laptop,
		desk,
		login,
		..
	} = &mut world.devices;
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(secrets_of(laptop, laptop_replica), (true, false));
	laptop
		.give_secrets(desk_replica, now() + 9_000, &mut rng)
		.expect("gives");
	synced(desk, laptop, now() + 10_000, (9, 10));
	assert_eq!(passwords(desk, *login), [text("hunter2")]);
}

#[test]
fn a_device_that_never_said_it_asks_drops_its_empty_secrets_and_asks_anew() {
	let mut rng = TestRng::new("never said");
	let Devices {
		mut laptop,
		mut desk,
		login,
		desk_directory,
		laptop_replica,
		desk_replica,
	} = laptop_and_desk("never said", &mut rng);
	// A compartment with a key to receive with and nothing else, as one that joined asking
	// has, which stopped before it said that it asks.
	let path = common::replica_folder(&desk_directory).join("keys.cbor");
	let mut key_file = KeyFile::read(&path).expect("reads");
	let asking = SecretsKeys {
		spaces: nibkeep_keys::SpaceKeyMap::new(),
		decryption: nibkeep_crypto::DecryptionKey::generate(&mut rng),
	};
	key_file
		.set_secrets(&asking, &desk_secrets(), &mut rng)
		.expect("seals");
	key_file.write(&path).expect("writes");
	desk.unlock_secrets(&Unlocker::Passphrase("desk secrets"))
		.expect("unlocks");
	desk.settle_secrets(now() + 2_000, &mut rng)
		.expect("settles");
	assert_eq!(desk.secrets_protection().expect("reads"), None);
	assert_eq!(
		desk.secrets_state().expect("reads"),
		SecretsState::Elsewhere
	);
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 3_000, &mut rng)
		.expect("asks anew");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	laptop
		.give_secrets(desk_replica, now() + 5_000, &mut rng)
		.expect("gives");
	synced(&mut desk, &mut laptop, now() + 6_000, (3, 4));
	assert_eq!(passwords(&desk, login), [text("hunter2")]);
}

#[test]
fn a_device_given_the_secrets_is_said_to_hold_them_whatever_it_lists_on_its_record() {
	let mut rng = TestRng::new("hides it held them");
	let mut world = infected("hides it held them", false, &mut rng);
	let (laptop_replica, infected_replica) = (world.devices.laptop_replica, world.infected_replica);
	let infected = world.infected.as_mut().expect("open");
	infected
		.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 8_000, &mut rng)
		.expect("asks");
	let laptop = &mut world.devices.laptop;
	synced(infected, laptop, now() + 9_000, (7, 8));
	laptop
		.give_secrets(infected_replica, now() + 10_000, &mut rng)
		.expect("gives");
	synced(infected, laptop, now() + 11_000, (9, 10));
	// It lists a key on its own record, so that the records would no longer say it holds
	// them: without that key's own listing, it lists nothing.
	let made_up = nibkeep_crypto::SigningKey::generate(&mut rng).verifying_key();
	let listing = asserting(
		infected_replica,
		nibkeep_governance::SIGNING_KEY,
		Value::Text(made_up.to_text()),
		&mut rng,
	);
	world.forge(vec![listing], (None, now() + 12_000), &mut rng);
	let replicas = world.devices.laptop.replicas().expect("lists");
	let listed = replicas
		.iter()
		.find(|replica| replica.id == infected_replica)
		.expect("listed");
	assert!(listed.holds_secrets);
	assert!(listed.given_secrets);
}

#[test]
fn a_key_that_another_device_lists_on_a_record_does_not_stop_its_replica_asking() {
	let mut rng = TestRng::new("cannot ask");
	let mut world = infected("cannot ask", false, &mut rng);
	let (laptop_replica, desk_replica) = (world.devices.laptop_replica, world.devices.desk_replica);
	let made_up = nibkeep_crypto::SigningKey::generate(&mut rng).verifying_key();
	let listing = asserting(
		desk_replica,
		nibkeep_governance::SIGNING_KEY,
		Value::Text(made_up.to_text()),
		&mut rng,
	);
	world.forge(vec![listing], (None, now() + 8_000), &mut rng);
	let Devices {
		laptop,
		desk,
		login,
		..
	} = &mut world.devices;
	// What it asked with still counts: it keeps its compartment, is given the secrets, and
	// reads them.
	synced(desk, laptop, now() + 9_000, (9, 10));
	assert!(desk.secrets_protection().expect("reads").is_some());
	assert_eq!(secrets_of(laptop, desk_replica), (false, true));
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	laptop
		.give_secrets(desk_replica, now() + 10_000, &mut rng)
		.expect("gives");
	synced(desk, laptop, now() + 11_000, (11, 12));
	assert_eq!(passwords(desk, *login), [text("hunter2")]);
	let _ = laptop_replica;
}

#[test]
fn a_field_made_secret_after_a_new_key_is_not_written_plainly_either() {
	let mut rng = TestRng::new("secret after a new key");
	let mut world = infected("secret after a new key", false, &mut rng);
	let desk_replica = world.devices.desk_replica;
	let laptop = &mut world.devices.laptop;
	laptop
		.revoke(&[desk_replica], now() + 8_000, &mut rng)
		.expect("revokes, with a new key");
	let personal = laptop.personal_space();
	let card: TypeName = "local.card".parse().expect("a type name");
	laptop
		.define_type(
			personal,
			&TypeDef::new(card.clone(), 1)
				.with_field(field("local.holder"), FieldDef::new(ValueKind::Text))
				.with_field(field("local.pin"), FieldDef::new(ValueKind::Text).secret()),
			now() + 9_000,
			&mut rng,
		)
		.expect("defines");
	let entity = laptop
		.create_entity(
			personal,
			&[card],
			vec![(field("local.holder"), text("Ada"))],
			now() + 10_000,
			&mut rng,
		)
		.expect("creates");
	let infected = world.infected.as_mut().expect("open");
	synced(infected, &mut world.devices.laptop, now() + 11_000, (7, 8));
	let plainly = asserting(entity, field("local.pin"), text("0000"), &mut rng);
	world.forge(vec![plainly], (None, now() + 12_000), &mut rng);
	let laptop = &world.devices.laptop;
	let pins: Vec<Value> = laptop
		.entity(personal, entity, this_day())
		.expect("reads")
		.expect("the card")
		.fields
		.iter()
		.filter(|shown| shown.field == field("local.pin"))
		.flat_map(|shown| shown.values.iter().map(|value| value.value.clone()))
		.collect();
	assert_eq!(pins, Vec::<Value>::new());
}

#[test]
fn a_device_whose_secrets_cannot_be_sealed_does_not_ask() {
	let mut rng = TestRng::new("asking turned down");
	let Devices {
		mut desk,
		laptop_replica,
		desk_replica,
		..
	} = laptop_and_desk("asking turned down", &mut rng);
	// No keystore here: it fails as a keystore's prompt that the person turns down does.
	let turned_down = desk.ask_for_secrets(
		laptop_replica,
		&NewProtection::Keystore {
			service: "org.nibkeep.test",
		},
		now() + 3_000,
		&mut rng,
	);
	assert!(matches!(
		turned_down,
		Err(NodeError::Keys(KeysError::Keystore(_)))
	));
	// Nothing says it asks, with a key that no compartment holds.
	assert_eq!(secrets_of(&desk, desk_replica), (false, false));
	assert_eq!(
		desk.secrets_state().expect("reads"),
		SecretsState::Elsewhere
	);
	desk.ask_for_secrets(laptop_replica, &desk_secrets(), now() + 4_000, &mut rng)
		.expect("asks");
	assert_eq!(secrets_of(&desk, desk_replica), (false, true));
}
