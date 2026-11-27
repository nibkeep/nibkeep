//! `nib secret`: secret values, asked for without being shown, and shown only after the
//! passphrase of the library's secrets.

use crate::common;

use std::fs;

use crate::common::{Test, library};
use nibkeep_cli::CliError;
use nibkeep_node::{
	KEYSTORE_SERVICE, KeysError, Library, NetworkOptions, NewProtection, NodeError, Unlocker,
};
use nibkeep_testkit::TestRng;

/// A library with a login type, whose password is secret, and a login.
fn with_login(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["type", "add", "login", "title:text", "password:text:secret"])
		.expect("adds the type");
	test.nib(&["new", "login", "title=Bank"])
		.expect("adds the login");
	test
}

/// Whether `nib show` names the password as secret.
fn names_secret(shown: &str) -> bool {
	shown
		.lines()
		.any(|line| line.split_whitespace().eq(["secret", "local.password"]))
}

#[test]
fn a_secret_is_set_without_being_shown_and_shown_after_its_passphrase() {
	let mut test = with_login("nib secret set");
	// The first secret sets up the library's secrets, with a passphrase of their own.
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	let shown = test
		.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	assert_eq!(test.asked, 4);
	assert!(!shown.contains("hunter2"), "{shown}");
	assert!(names_secret(&shown), "{shown}");
	let shown = test.nib(&["show", "Bank"]).expect("shows");
	assert!(!shown.contains("hunter2"), "{shown}");
	assert!(names_secret(&shown), "{shown}");
	assert!(
		!nibkeep_testkit::holds_anywhere(&test.here, b"hunter2"),
		"stored sealed"
	);

	test.answer(&["tr0ub4dor"]);
	let secret = test
		.nib(&["secret", "show", "Bank", "password"])
		.expect("shows");
	assert_eq!(secret, "hunter2\n");
	assert_eq!(test.asked, 5);
	test.answer(&["tr0ub4dor"]);
	let json = test.json(&["secret", "show", "Bank", "password"]);
	assert_eq!(json["field"], "local.password");
	assert_eq!(json["values"][0]["value"], "hunter2");

	// The library's own passphrase, or none, opens no secret.
	test.answer(&["correct horse"]);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::Node(NodeError::Keys(KeysError::WrongPassphrase)))
	));
	assert!(test.nib(&["secret", "show", "Bank", "password"]).is_err());
}

#[test]
fn a_new_secret_replaces_the_old_one_which_its_history_keeps() {
	let mut test = with_login("nib secret replaced");
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.now += 60_000;
	test.answer(&["tr0ub4dor", "correct battery", "correct battery"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.answer(&["tr0ub4dor"]);
	assert_eq!(
		test.nib(&["secret", "show", "Bank", "password"])
			.expect("shows"),
		"correct battery\n"
	);
	// `nib history` leaves secret values out, and says where they are.
	let history = test.nib(&["history", "Bank"]).expect("shows");
	assert!(!history.contains("hunter2"), "{history}");
	assert!(matches!(
		test.nib(&["history", "Bank", "password"]),
		Err(CliError::SecretHistory(_))
	));
	test.answer(&["tr0ub4dor"]);
	let history = test
		.nib(&["secret", "history", "Bank", "password"])
		.expect("shows");
	assert!(history.contains("hunter2"), "{history}");
	assert!(history.contains("correct battery"), "{history}");
	assert!(history.contains("replaced"), "{history}");
}

#[test]
fn secret_values_are_never_typed_on_the_command_line() {
	let mut test = with_login("nib secret refused");
	assert!(matches!(
		test.nib(&["edit", "Bank", "password=hunter2"]),
		Err(CliError::SecretField(_))
	));
	assert!(matches!(
		test.nib(&["new", "login", "title=Shop", "password=hunter2"]),
		Err(CliError::SecretField(_))
	));
	assert!(matches!(
		test.nib(&["purge", "Bank", "password=hunter2"]),
		Err(CliError::SecretGiven(_))
	));
	assert!(matches!(
		test.nib(&["secret", "set", "Bank", "title"]),
		Err(CliError::NotSecret(_))
	));
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::NoSecrets)
	));
	// Typed twice, differently, nothing is set; the secrets stay set up.
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter3"]);
	assert!(matches!(
		test.nib(&["secret", "set", "Bank", "password"]),
		Err(CliError::ValuesDiffer)
	));
	test.answer(&["tr0ub4dor"]);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::NoSecretValue { .. })
	));
	test.answer(&["tr0ub4dor"]);
	assert!(matches!(
		test.nib(&["secret", "history", "Bank", "password"]),
		Err(CliError::NoSecretValue { .. })
	));
}

#[test]
fn secret_values_are_purged_after_their_passphrase_and_never_exported() {
	let mut test = with_login("nib secret purged");
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	let export = test.here.with_file_name("nib secret purged, exported");
	let _ = fs::remove_dir_all(&export);
	test.nib(&["export", export.to_str().expect("UTF-8")])
		.expect("exports");
	assert!(!nibkeep_testkit::holds_anywhere(&export, b"hunter2"));

	test.answer(&["tr0ub4dor"]);
	let (said, result) = test.nib_printing(&["purge", "Bank", "password="]);
	assert!(matches!(result, Err(CliError::Unconfirmed)));
	assert!(said.contains("1 value"), "{said}");
	test.answer(&["tr0ub4dor"]);
	test.nib(&["purge", "Bank", "password=", "--yes"])
		.expect("purges");
	test.answer(&["tr0ub4dor"]);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::NoSecretValue { .. })
	));
}

#[test]
fn a_secret_is_cleared_and_one_of_its_values_purged() {
	let mut test = with_login("nib secret cleared");
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.now += 60_000;
	test.answer(&["tr0ub4dor", "correct battery", "correct battery"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	// One old value, typed without being shown, is purged; the current one stays.
	test.answer(&["tr0ub4dor", "hunter2"]);
	let (said, result) = test.nib_printing(&["secret", "purge", "Bank", "password"]);
	assert!(matches!(result, Err(CliError::Unconfirmed)), "{result:?}");
	assert!(said.contains("1 value"), "{said}");
	test.answer(&["tr0ub4dor", "hunter2"]);
	test.nib(&["secret", "purge", "Bank", "password", "--yes"])
		.expect("purges");
	test.answer(&["tr0ub4dor"]);
	let history = test
		.nib(&["secret", "history", "Bank", "password"])
		.expect("shows");
	assert!(!history.contains("hunter2"), "{history}");
	assert!(history.contains("correct battery"), "{history}");
	// Cleared, it shows nothing, and its history keeps what it had.
	test.now += 60_000;
	test.answer(&["tr0ub4dor"]);
	test.nib(&["secret", "clear", "Bank", "password"])
		.expect("clears");
	test.answer(&["tr0ub4dor"]);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::NoSecretValue { .. })
	));
	test.answer(&["tr0ub4dor"]);
	let history = test
		.nib(&["secret", "history", "Bank", "password"])
		.expect("shows");
	assert!(history.contains("correct battery"), "{history}");
}

#[test]
fn a_field_made_secret_by_another_type_is_named_secret() {
	let mut test = library("nib secret by another type");
	test.nib(&["type", "add", "note", "title:text", "local.password:text"])
		.expect("adds the type");
	test.nib(&["type", "add", "login", "local.password:text:secret"])
		.expect("adds the type");
	test.nib(&["new", "note", "title=Bank"])
		.expect("adds the note");
	let shown = test.nib(&["show", "Bank"]).expect("shows");
	assert!(names_secret(&shown), "{shown}");
	let json = test.json(&["show", "Bank"]);
	assert_eq!(json["secret_fields"][0], "local.password");
}

use crate::common::TIMEOUT;

/// A time after the tests' changes, for syncs.
fn later() -> u64 {
	nibkeep_testkit::millis_at("2026-10-12", 11, 0)
}

/// A test's library, opened as nib opens it, with the mock keystore.
fn open(test: &Test) -> Library {
	Library::open(&test.here.join(".nib"), &Unlocker::Keystore).expect("opens")
}

/// Pairs a new library in `joiner`'s folder, on a device named desk, with `sponsor`'s, over
/// connections on localhost, its keys in the mock keystore.
fn pair(sponsor: &Test, joiner: &Test) {
	common::pair_on_localhost(
		&mut open(sponsor),
		&joiner.here.join(".nib"),
		("desk", None),
		&NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
		later(),
		&mut TestRng::new("pairing the desk"),
	);
}

/// Syncs the libraries of two tests over connections on localhost, as nib sync and nib sync
/// --serve do: `first` connects, and `second` answers.
fn sync(first: &Test, second: &Test) {
	let mut first = open(first);
	let second = open(second);
	let first_network = first.network(NetworkOptions::LOCAL).expect("starts");
	let second_network = second.network(NetworkOptions::LOCAL).expect("starts");
	first_network.remember(
		second_network.endpoint(),
		&second_network.local_addresses(),
		None,
	);
	let closed = second.close();
	std::thread::scope(|scope| {
		let answered = scope.spawn(|| {
			let mut answering = TestRng::new("answering");
			nibkeep_node::answer(
				&closed,
				&second_network,
				Some(TIMEOUT),
				&later,
				&mut answering,
			)
		});
		let mut rng = TestRng::new("syncing");
		let peers = first.peers().expect("lists");
		let found = std::collections::BTreeSet::from([second_network.endpoint()]);
		let synced = nibkeep_node::sync_with_found(
			&mut first,
			&first_network,
			peers,
			&found,
			&later,
			&mut rng,
		);
		assert!(synced.iter().all(|with| with.synced.is_ok()), "{synced:?}");
		let answered = answered.join().expect("ran").expect("answers");
		assert!(answered.is_some_and(|answered| answered.synced.is_ok()));
	});
}

#[test]
fn a_device_asks_for_the_secrets_and_another_gives_them() {
	let mut laptop = with_login("secrets given laptop");
	laptop.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	laptop
		.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	let listed = laptop.nib(&["device", "list"]).expect("lists");
	assert!(listed.contains("holds the secrets"), "{listed}");
	let laptop_id = laptop.json(&["device", "list"])[0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();

	let mut desk = Test::with_keystore("secrets given desk");
	pair(&laptop, &desk);
	assert!(matches!(
		desk.nib(&["secret", "ask", "nowhere"]),
		Err(CliError::Node(NodeError::WhichDevice(_)))
	));
	desk.answer(&["desk secrets", "desk secrets"]);
	let asked = desk.nib(&["secret", "ask", &laptop_id[..8]]).expect("asks");
	let desk_id = desk.json(&["device", "list"])[0]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	assert!(
		asked.contains(&format!("nib secret give {}", &desk_id[..8])),
		"{asked}"
	);
	sync(&desk, &laptop);
	let listed = laptop.nib(&["device", "list"]).expect("lists");
	assert!(listed.contains("asks for the secrets"), "{listed}");
	assert!(matches!(
		laptop.nib(&["secret", "give", "nowhere"]),
		Err(CliError::Node(NodeError::WhichDevice(_)))
	));
	laptop.answer(&["tr0ub4dor"]);
	let given = laptop.nib(&["secret", "give", "desk"]).expect("gives");
	assert!(given.contains("Gave the secrets to nib on desk"), "{given}");
	assert!(
		!laptop.json(&["device", "list"])[1]["asks_for_secrets"]
			.as_bool()
			.expect("a boolean")
	);

	sync(&desk, &laptop);
	// The desk's own passphrase opens them: it takes them as it unlocks them.
	desk.answer(&["desk secrets"]);
	let shown = desk
		.nib(&["secret", "show", "Bank", "password"])
		.expect("shows");
	assert_eq!(shown, "hunter2\n");

	// Revoked, the desk can still read what it had: the laptop says so, for this device,
	// before and after.
	sync(&desk, &laptop);
	let (would, revoked) = laptop.nib_printing(&["device", "revoke", "desk"]);
	assert!(revoked.is_err(), "not without --yes");
	assert!(
		would.contains("nib on desk held the secrets. Revoked, it could still read"),
		"{would}"
	);
	let revoked = laptop
		.nib(&["device", "revoke", "desk", "--yes"])
		.expect("revokes");
	assert!(
		revoked.contains(
			"nib on desk held the secrets. It can still read every password and secret \
			 note saved until now"
		),
		"{revoked}"
	);
	assert!(
		revoked.contains("Change the passwords that matter."),
		"{revoked}"
	);
	let listed = laptop.nib(&["device", "list"]).expect("lists");
	assert!(listed.contains("revoked; held the secrets"), "{listed}");
}
