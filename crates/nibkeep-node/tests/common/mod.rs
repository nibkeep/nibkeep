//! Helpers shared by the tests.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

pub(crate) mod devices;
pub(crate) mod in_process;
pub(crate) mod tree_rounds;
pub(crate) mod tree_turns;

use std::path::{Path, PathBuf};

pub(crate) use in_process::{
	TIMEOUT, now, pair_joining, pair_joining_as, protection, sync_in_memory, sync_in_memory_proving,
};
use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	Joining, Library, NetworkOptions, NewLibrary, NewProtection, NodeError, PairingCode, join,
	pair_ai_agent,
};
use nibkeep_testkit::TestRng;

/// Pairs a new replica in `directory`, on a device named `device`, with `sponsor`, which
/// shows `shown`, while the new one types `typed`, over connections on localhost.
pub(crate) fn pair_named(
	sponsor: &mut Library,
	directory: &Path,
	device: &str,
	(shown, typed): (&PairingCode, &PairingCode),
	rng: &mut TestRng,
) -> (
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
) {
	let joining = Joining::new(device, "nib", rng).expect("names");
	pair_joining(sponsor, directory, (joining, false), (shown, typed), rng)
}

/// Pairs a new replica of an AI agent named `name` in `directory`, on a device named
/// `device`, with `sponsor`, which shows the code that the new replica types, over
/// connections on localhost.
pub(crate) fn pair_ai_agent_named(
	sponsor: &mut Library,
	directory: &Path,
	(device, name): (&str, &str),
	code: &PairingCode,
	rng: &mut TestRng,
) -> (
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
) {
	let sponsor_network = sponsor.network(NetworkOptions::LOCAL).expect("starts");
	let joining = Joining::new(device, "nib", rng).expect("names");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		sponsor_network.endpoint(),
		&sponsor_network.local_addresses(),
		Some(&code.announcement()),
	);
	let mut sponsor_rng = TestRng::new(&format!("sponsor of {name} in {}", directory.display()));
	std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			pair_ai_agent(
				sponsor,
				&sponsor_network,
				code,
				name,
				nibkeep_node::Showing::new(TIMEOUT),
				&|| now() + 60_000,
				&mut sponsor_rng,
			)
		});
		let joined = join(
			directory,
			joining,
			&joiner_network,
			code,
			&protection(),
			TIMEOUT,
			&|| now() + 60_000,
			rng,
		);
		(paired.join().expect("the sponsor ran"), joined)
	})
}

/// Pairs a new replica in `directory`, on a device named `desk`, as [`pair_named`] does.
pub(crate) fn pair_with(
	sponsor: &mut Library,
	directory: &Path,
	shown: &PairingCode,
	typed: &PairingCode,
	rng: &mut TestRng,
) -> (
	Result<nibkeep_node::Paired, NodeError>,
	Result<nibkeep_node::Joined, NodeError>,
) {
	pair_named(sponsor, directory, "desk", (shown, typed), rng)
}

/// The folder of the replica that is here in a library's directory.
pub(crate) fn replica_folder(directory: &Path) -> std::path::PathBuf {
	nibkeep_node::Library::replica_folder(directory).expect("a replica is here")
}

/// Commits `operations` to the personal space `personal` of the closed library in
/// `directory`, whose keys `unlocker` opens, as an infected device could by writing its own
/// files: signed with `author`, or with its replica's key; no field sealed, and nothing that
/// the engine checks before it commits checked.
pub(crate) fn forge(
	directory: &Path,
	unlocker: &nibkeep_node::Unlocker<'_>,
	(personal, operations): (nibkeep_base::SpaceId, Vec<nibkeep_model::Operation>),
	(author, at): (Option<&nibkeep_crypto::SigningKey>, u64),
	rng: &mut TestRng,
) {
	let (replica, keys) = replica_keys(directory, unlocker, personal);
	let mut stored = nibkeep_store::StoredLog::open(
		&nibkeep_store::log_path(&replica_folder(directory), personal),
		personal,
		&keys,
		&replica.storage,
		600_000,
		[],
	)
	.expect("opens the log");
	stored.set_secret_fields([]).expect("indexes again");
	stored
		.commit(
			author.unwrap_or(&replica.signing),
			&keys,
			operations,
			at,
			rng,
		)
		.expect("commits");
}

/// The current statements of `field` on `entity` in `space`, as the closed library in
/// `directory`, which `unlocker` opens, holds them: read from its files, as an infected
/// device reads them, also those that the engine shows nowhere, such as types.
pub(crate) fn current_statements(
	directory: &Path,
	unlocker: &nibkeep_node::Unlocker<'_>,
	space: nibkeep_base::SpaceId,
	(entity, field): (nibkeep_base::EntityId, &nibkeep_model::FieldName),
) -> Vec<nibkeep_base::StatementId> {
	let (replica, keys) = replica_keys(directory, unlocker, space);
	let stored = nibkeep_store::StoredLog::open(
		&nibkeep_store::log_path(&replica_folder(directory), space),
		space,
		&keys,
		&replica.storage,
		600_000,
		[],
	)
	.expect("opens the log");
	stored
		.log()
		.state()
		.current(entity, field)
		.expect("reads")
		.iter()
		.map(|statement| statement.id)
		.collect()
}

/// The keys of the replica that is here in a library's directory, which `unlocker` opens,
/// and its keys of `space`, read from its key file, as an infected device reads them.
pub(crate) fn replica_keys(
	directory: &Path,
	unlocker: &nibkeep_node::Unlocker<'_>,
	space: nibkeep_base::SpaceId,
) -> (nibkeep_keys::ReplicaKeys, nibkeep_log::SpaceKeys) {
	let key_file =
		nibkeep_keys::KeyFile::read(&replica_folder(directory).join("keys.cbor")).expect("reads");
	let replica = key_file
		.replica(&key_file.unlock_replica(unlocker).expect("unlocks"))
		.expect("opens");
	let mut epochs = replica.spaces[&space].iter();
	let (&first, key) = epochs.next().expect("a key");
	let mut keys = nibkeep_log::SpaceKeys::new(first, key.clone());
	for (&epoch, key) in epochs {
		keys.insert(epoch, key.clone());
	}
	(replica, keys)
}

/// A new library, in a directory of its own, protected by a quick passphrase.
pub(crate) fn library(name: &str, rng: &mut TestRng) -> (PathBuf, Library) {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	let protection = NewProtection::Passphrase(
		"correct horse",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		rng,
	)
	.expect("creates");
	(directory, library)
}

/// Editing and viewing today, at [`now`], two hours east of UTC.
pub(crate) fn today() -> nibkeep_node::Dating {
	nibkeep_node::Dating::today(now(), 120).expect("a valid moment")
}

/// The day of [`now`], two hours east of UTC.
pub(crate) fn this_day() -> nibkeep_time::Day {
	today().day
}

/// The values of a field of an entity of the personal space, as the library shows them on
/// [`this_day`].
pub(crate) fn values(
	library: &Library,
	entity: nibkeep_base::EntityId,
	field: &nibkeep_model::FieldName,
) -> Vec<nibkeep_model::Value> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity")
		.values(field)
		.iter()
		.map(|shown| shown.value.clone())
		.collect()
}

/// Runs a pass of removals on `remover` in memory, as its holder of the sync role does, with
/// `other` found on the network, at `at`: plans it, syncs with `other` if it is to prove
/// copies, putting the challenges, and removes what it proved.
pub(crate) fn remove_in_memory(
	remover: &mut Library,
	other: &mut Library,
	at: u64,
	sides: (usize, usize),
) -> nibkeep_node::Removed {
	let found = std::collections::BTreeSet::from([other.endpoint()]);
	let records = remover
		.sync_records(TestRng::new(&format!("records of {} at {at}", sides.0)))
		.expect("opens");
	let mut rng = TestRng::new(&format!("removals of {} at {at}", sides.0));
	let mut removals = remover
		.plan_removals(&records, &found, at, &mut rng)
		.expect("plans");
	if !removals.provers().is_empty() {
		let (first, second) =
			sync_in_memory_proving(remover, other, at, sides, Some(&mut removals));
		first.expect("syncs");
		second.expect("syncs");
	}
	remover
		.remove_proven(&removals, &records, at, &mut rng)
		.expect("removes")
}

/// The type of logins, whose passwords are secret.
pub(crate) const LOGIN: nibkeep_model::TypeName =
	nibkeep_model::TypeName::from_static("local.login");

/// The type of logins: a title, and a password, which is secret.
pub(crate) fn login_type() -> nibkeep_model::TypeDef {
	use nibkeep_model::{FieldDef, ValueKind};
	nibkeep_model::TypeDef::new(LOGIN, 1)
		.with_field(
			"local.title".parse().expect("a field name"),
			FieldDef::new(ValueKind::Text),
		)
		.with_field(
			"local.password".parse().expect("a field name"),
			FieldDef::new(ValueKind::Text).secret(),
		)
}

/// The values that a login's password shows today.
pub(crate) fn passwords(
	library: &Library,
	login: nibkeep_base::EntityId,
) -> Vec<nibkeep_model::Value> {
	let password: nibkeep_model::FieldName = "local.password".parse().expect("a field name");
	library
		.entity(library.personal_space(), login, this_day())
		.expect("reads")
		.expect("the login")
		.values(&password)
		.iter()
		.map(|shown| shown.value.clone())
		.collect()
}

/// A name for a folder of the tests, which run at once, that no other test of the file takes.
pub(crate) fn unique_folder(name: &str) -> String {
	static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	format!(
		"{name} {}",
		NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
	)
}

/// A new library of a user named `user`, on a device named `device`, in a folder of its own.
pub(crate) fn user_library(user: &str, device: &str, rng: &mut TestRng) -> Library {
	let directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&unique_folder(&format!("{user} {device}")),
	);
	Library::create(
		&directory,
		&NewLibrary::new(user, device, "nib"),
		&protection(),
		now(),
		rng,
	)
	.expect("creates")
}

/// Invites the user of `newcomer` to `space` of `admin`, as `role`, over connections on
/// localhost, and returns their member record.
pub(crate) fn invite_member(
	admin: &mut Library,
	(space, role): (nibkeep_base::SpaceId, nibkeep_node::MemberRole),
	newcomer: &mut Library,
	at: u64,
) -> nibkeep_base::EntityId {
	let mut rng = TestRng::new(&format!("inviting at {at}"));
	let code = PairingCode::generate(&mut rng);
	let admin_network = admin.network(NetworkOptions::LOCAL).expect("starts");
	let joiner_network = newcomer.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		admin_network.endpoint(),
		&admin_network.local_addresses(),
		Some(&code.invitation_announcement()),
	);
	let mut admin_rng = TestRng::new(&format!("admin inviting at {at}"));
	let (invited, joined) = std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			nibkeep_node::invite(
				admin,
				&admin_network,
				&nibkeep_node::Invitation {
					space,
					role,
					person: None,
				},
				&code,
				&mut |_| true,
				nibkeep_node::Showing::new(TIMEOUT),
				&|| at,
				&mut admin_rng,
			)
		});
		let joined =
			nibkeep_node::join_space(newcomer, &joiner_network, &code, TIMEOUT, &|| at, &mut rng);
		(invited.join().expect("the admin ran"), joined)
	});
	joined.expect("joins");
	invited.expect("invites").member
}

/// Every file under a folder, with its bytes, to tell that nothing was written there.
pub(crate) fn files(folder: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
	let mut all = std::collections::BTreeMap::new();
	for entry in std::fs::read_dir(folder).expect("lists") {
		let entry = entry.expect("an entry");
		if entry.file_type().expect("a type").is_dir() {
			all.extend(files(&entry.path()));
		} else {
			all.insert(entry.path(), std::fs::read(entry.path()).expect("reads"));
		}
	}
	all
}

/// Pairs a new device of the person's, named `device`, with `sponsor`, in a folder named
/// after the test's `name` and the device.
pub(crate) fn pair_device(
	sponsor: &mut Library,
	name: &str,
	device: &str,
	rng: &mut TestRng,
) -> Library {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, {device}"));
	let code = PairingCode::generate(rng);
	let (paired, joined) = pair_named(sponsor, &directory, device, (&code, &code), rng);
	paired.expect("paired");
	joined.expect("joined").library
}

/// Pairs a new device of the person's, named `device`, with `sponsor`, as the person chose
/// there, `new_device`, in a folder named after the test's `name` and the device.
pub(crate) fn pair_device_as(
	sponsor: &mut Library,
	(name, device): (&str, &str),
	new_device: &nibkeep_node::NewDevice,
	rng: &mut TestRng,
) -> Library {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, {device}"));
	let code = PairingCode::generate(rng);
	let joining = Joining::new(device, "nib", rng).expect("names");
	let (paired, joined) = pair_joining_as(
		sponsor,
		&directory,
		(joining, false, new_device),
		(&code, &code),
		rng,
	);
	paired.expect("paired");
	joined.expect("joined").library
}

/// The record of `library`'s own replica in its personal space.
pub(crate) fn own_record(library: &Library) -> nibkeep_base::EntityId {
	library
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.map(|replica| replica.id)
		.expect("its record")
}

/// The verifying key of `library`'s own replica.
pub(crate) fn own_key(library: &Library) -> nibkeep_crypto::VerifyingKey {
	library
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.and_then(|replica| replica.signing)
		.expect("its key")
}

/// Whether the replica whose key is `key` is listed in `space`, as `library` knows it, for
/// any member, and whether it is revoked there.
pub(crate) fn listed_there(
	library: &Library,
	space: nibkeep_base::SpaceId,
	key: &nibkeep_crypto::VerifyingKey,
) -> Option<bool> {
	library
		.members(space)
		.expect("lists")
		.iter()
		.flat_map(|member| &member.replicas)
		.find(|replica| replica.signing == *key)
		.map(|replica| replica.revoked)
}

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
pub(crate) fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// Writes the SHA-256 sum of a file of an export again, as someone who changed it might.
pub(crate) fn sum_again(directory: &Path, path: &str) {
	use sha2::Digest as _;
	let sum = sha2::Sha256::digest(std::fs::read(directory.join(path)).expect("the file"))
		.iter()
		.fold(String::new(), |mut sum, byte| {
			use std::fmt::Write as _;
			let _ = write!(sum, "{byte:02x}");
			sum
		});
	let mut sums = String::new();
	for line in std::fs::read_to_string(directory.join("SHA256SUMS"))
		.expect("sums")
		.lines()
	{
		if line.ends_with(&format!("  {path}")) {
			sums.push_str(&sum);
			sums.push_str("  ");
			sums.push_str(path);
		} else {
			sums.push_str(line);
		}
		sums.push('\n');
	}
	std::fs::write(directory.join("SHA256SUMS"), sums).expect("writes");
}
