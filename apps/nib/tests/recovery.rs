//! `nib recovery`: a printed code that brings a library back, made, confirmed, listed,
//! checked and revoked ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: ../../../docs/Technical-design.md#111-recovery-codes

mod common;

use common::{TIMEOUT, Test, library};
use nibkeep_cli::CliError;
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{
	KEYSTORE_SERVICE, Library, NetworkOptions, NewProtection, Protection, Unlocker,
};
use nibkeep_testkit::TestRng;

/// The code that `nib recovery new` printed, on the lines that hold its groups.
fn printed_code(output: &str) -> &'static str {
	let groups: Vec<&str> = output
		.lines()
		.map(str::trim)
		.filter(|line| {
			!line.is_empty()
				&& line.split('-').all(|group| {
					group.len() == 5
						&& group
							.bytes()
							.all(|symbol| symbol.is_ascii_digit() || symbol.is_ascii_uppercase())
				})
		})
		.collect();
	let code = groups.join("-");
	assert!(RecoveryCode::from_text(&code).is_ok(), "{output}");
	Box::leak(code.into_boxed_str())
}

/// Makes a code and confirms it, as a person who wrote it down does.
fn make_code(test: &mut Test) -> &'static str {
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	test.answer(&[code]);
	test.nib(&["recovery", "confirm"]).expect("confirms");
	code
}

#[test]
fn a_code_is_made_and_counts_once_typed_back() {
	let mut test = library("recovery, made");
	// Nobody answers the prompt: the code is shown, and nothing names it yet.
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	assert!(made.contains("holds keys and no data"), "{made}");
	assert!(made.contains("nib recovery confirm"), "{made}");
	assert_eq!(test.json(&["recovery", "list"]), serde_json::json!([]));
	// A mistyped group is named, and the person types it again.
	let mistyped: &'static str = Box::leak(
		format!(
			"{}{}",
			if code.starts_with('2') { '3' } else { '2' },
			&code[1..]
		)
		.into_boxed_str(),
	);
	test.answer(&[mistyped, code]);
	let confirmed = test.nib(&["recovery", "confirm"]).expect("confirms");
	assert!(confirmed.contains("group 1"), "{confirmed}");
	let name = RecoveryCode::name_of(
		&RecoveryCode::from_text(code)
			.expect("a code")
			.signing_key()
			.verifying_key(),
	);
	assert!(
		confirmed.contains(&format!("The recovery code {name} now stands")),
		"{confirmed}"
	);
	let listed = test.json(&["recovery", "list"]);
	assert_eq!(listed[0]["name"], name.as_str());
	assert_eq!(listed[0]["revoked"], false);
	assert_eq!(listed[0]["made_on"], "laptop");
	let listed = test.nib(&["recovery", "list"]).expect("lists");
	assert!(
		listed.contains(&name) && listed.contains("laptop"),
		"{listed}"
	);
	// Typing it again changes nothing.
	test.answer(&[code]);
	let again = test.nib(&["recovery", "confirm"]).expect("confirms");
	assert!(again.contains("stands already"), "{again}");
	assert_eq!(
		test.json(&["recovery", "list"]).as_array().map(Vec::len),
		Some(1)
	);
}

#[test]
fn a_code_typed_back_at_once_counts_at_once() {
	let mut test = library("recovery, at once");
	// The code that the command will make, from random numbers that the test knows.
	test.rng = TestRng::new("the code to come");
	let expected = RecoveryCode::generate(&mut TestRng::new("the code to come"));
	let expected: &'static str = Box::leak(expected.to_text().as_str().to_owned().into_boxed_str());
	test.answer(&[expected]);
	let made = test.nib(&["recovery", "new"]).expect("makes");
	assert_eq!(printed_code(&made), expected);
	assert!(made.contains("now stands"), "{made}");
	assert_eq!(
		test.json(&["recovery", "list"]).as_array().map(Vec::len),
		Some(1)
	);
}

#[test]
fn enter_at_the_prompt_leaves_the_code_for_later() {
	let mut test = library("recovery, later");
	test.answer(&["", "never asked for"]);
	let made = test.nib(&["recovery", "new"]).expect("makes");
	assert!(made.contains("nib recovery confirm"), "{made}");
	assert!(!made.contains("65 letters"), "{made}");
	assert_eq!(test.asked, 1);
	assert_eq!(test.json(&["recovery", "list"]), serde_json::json!([]));
}

#[test]
fn a_code_that_is_typed_wrongly_three_times_names_nothing() {
	let mut test = library("recovery, mistyped");
	test.answer(&["not a code", "00000-00000", "still not"]);
	let (printed, result) = test.nib_printing(&["recovery", "confirm"]);
	assert!(
		matches!(result, Err(CliError::NotARecoveryCode)),
		"{result:?}"
	);
	assert!(printed.contains("65 letters and digits"), "{printed}");
	assert_eq!(test.json(&["recovery", "list"]), serde_json::json!([]));
}

#[test]
fn a_code_is_checked_on_a_device_that_works() {
	let mut test = library("recovery, checked");
	let code = make_code(&mut test);
	test.answer(&[code]);
	let checked = test.json(&["recovery", "check"]);
	assert_eq!(checked["known"], true);
	assert_eq!(checked["revoked"], false);
	assert_eq!(checked["lacks"], 0);
	test.answer(&[code]);
	let checked = test.nib(&["recovery", "check"]).expect("checks");
	assert!(checked.contains("stands"), "{checked}");
	// A code that nothing names.
	let other = RecoveryCode::generate(&mut TestRng::new("another code"));
	let other: &'static str = Box::leak(other.to_text().as_str().to_owned().into_boxed_str());
	test.answer(&[other]);
	let (printed, result) = test.nib_printing(&["recovery", "check"]);
	assert!(
		matches!(result, Err(CliError::UnknownRecoveryCode)),
		"{result:?}"
	);
	assert!(printed.contains("names no recovery code"), "{printed}");
}

#[test]
fn a_code_is_revoked_only_once_confirmed() {
	let mut test = library("recovery, revoked");
	let code = make_code(&mut test);
	let name = test.json(&["recovery", "list"])[0]["name"]
		.as_str()
		.expect("a name")
		.to_owned();
	let (printed, result) = test.nib_printing(&["recovery", "revoke", &name]);
	assert!(
		matches!(result, Err(CliError::UnconfirmedRevoke)),
		"{result:?}"
	);
	assert!(printed.contains("Would revoke"), "{printed}");
	assert_eq!(test.json(&["recovery", "list"])[0]["revoked"], false);
	let revoked = test
		.nib(&["recovery", "revoke", &name, "--yes"])
		.expect("revokes");
	assert!(revoked.contains("Revoked, for good"), "{revoked}");
	assert_eq!(test.json(&["recovery", "list"])[0]["revoked"], true);
	// The paper no longer checks, and is not named again.
	test.answer(&[code]);
	let checked = test.json(&["recovery", "check"]);
	assert_eq!(checked["revoked"], true);
	assert_eq!(checked["lacks"], 1);
	test.answer(&[code]);
	let again = test.nib(&["recovery", "confirm"]).unwrap_err();
	assert!(again.to_string().contains("was revoked"), "{again}");
	let unknown = test
		.nib(&["recovery", "revoke", &name, "--yes"])
		.unwrap_err();
	assert!(unknown.to_string().contains("revoked already"), "{unknown}");
}

#[test]
fn a_code_is_given_the_secrets_when_asked() {
	let mut test = library("recovery, secrets");
	// The library's first secret sets its secrets up.
	test.nib(&["type", "add", "login", "title:text", "password:text:secret"])
		.expect("adds a type");
	test.nib(&["new", "login", "title=Bank"])
		.expect("adds a login");
	test.answer(&[
		"secrets passphrase",
		"secrets passphrase",
		"hunter2",
		"hunter2",
	]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets the password");
	// A code made without the secrets opens none.
	let code = make_code(&mut test);
	assert_eq!(test.json(&["recovery", "list"])[0]["holds_secrets"], false);
	test.answer(&["secrets passphrase", code]);
	let checked = test.json(&["recovery", "check", "--secrets"]);
	assert_eq!(checked["secrets"]["opens"], 0);
	assert_eq!(checked["secrets"]["lacks"], 1);
	// Confirmed again with the secrets, it opens them.
	test.answer(&["secrets passphrase", code]);
	let confirmed = test
		.nib(&["recovery", "confirm", "--secrets"])
		.expect("confirms");
	assert!(
		confirmed.contains("stands already, and opens the secrets too"),
		"{confirmed}"
	);
	assert_eq!(test.json(&["recovery", "list"])[0]["holds_secrets"], true);
	test.answer(&["secrets passphrase", code]);
	let checked = test
		.nib(&["recovery", "check", "--secrets"])
		.expect("checks");
	assert!(checked.contains("It opens the secrets too."), "{checked}");
	// A wrong passphrase for the secrets stops before any code is shown.
	test.answer(&["not the passphrase"]);
	let (printed, result) = test.nib_printing(&["recovery", "new", "--secrets"]);
	assert!(result.is_err());
	assert!(!printed.contains("A new recovery code"), "{printed}");
}

#[test]
fn a_code_names_the_backups_that_carry_it_and_those_still_to_update() {
	let mut test = library("recovery, backups");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "recovery, backups, drive")
			.join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	// The backup is still to carry a new code, as the person is told once it stands.
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	test.answer(&[code]);
	let confirmed = test.nib(&["recovery", "confirm"]).expect("confirms");
	assert!(
		confirmed.contains("nib backup update") && confirmed.contains("Blue stick"),
		"{confirmed}"
	);
	let listed = test.json(&["recovery", "list"]);
	assert_eq!(
		listed[0]["backups"],
		serde_json::json!({"carried_by": [], "to_update": ["Blue stick"]})
	);
	let shown = test.nib(&["recovery", "list"]).expect("lists");
	assert!(shown.contains("still to update: Blue stick"), "{shown}");
	// Updated, the backup carries it: the recovery file is there.
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	assert!(folder.join("recovery.cbor").exists());
	let listed = test.json(&["recovery", "list"]);
	assert_eq!(
		listed[0]["backups"],
		serde_json::json!({"carried_by": ["Blue stick"], "to_update": []})
	);
	let shown = test.nib(&["recovery", "list"]).expect("lists");
	assert!(shown.contains("carried by: Blue stick"), "{shown}");
}

#[test]
fn a_code_with_no_backup_to_carry_it_says_so() {
	let mut test = library("recovery, no backup");
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	test.answer(&[code]);
	let confirmed = test.nib(&["recovery", "confirm"]).expect("confirms");
	assert!(confirmed.contains("nib backup new"), "{confirmed}");
	let shown = test.nib(&["recovery", "list"]).expect("lists");
	assert!(shown.contains("nib backup new"), "{shown}");
}

/// The name printed beside a code.
fn name_of(code: &str) -> String {
	RecoveryCode::name_of(
		&RecoveryCode::from_text(code)
			.expect("a code")
			.signing_key()
			.verifying_key(),
	)
}

/// Makes a code and confirms it with `flags`, as a person who wrote it down does; returns
/// the code and what `nib recovery confirm` said.
fn make_code_saying(test: &mut Test, flags: &[&str]) -> (&'static str, String) {
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	test.answer(&[code]);
	let mut confirm = vec!["recovery", "confirm"];
	confirm.extend(flags);
	let confirmed = test.nib(&confirm).expect("confirms");
	(code, confirmed)
}

/// The person's code named `name`, as `nib recovery list --json` lists it.
fn listed(test: &mut Test, name: &str) -> serde_json::Value {
	test.json(&["recovery", "list"])
		.as_array()
		.expect("a list")
		.iter()
		.find(|code| code["name"] == name)
		.expect("the code is listed")
		.clone()
}

#[test]
fn a_new_code_replaces_the_one_before_once_every_backup_carries_it() {
	let mut test = library("recovery, replaced");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "recovery, replaced, drive")
			.join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	let old = name_of(make_code(&mut test));
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	// A new code: the person is told to keep the old paper, until the backup carries it.
	let (new, confirmed) = make_code_saying(&mut test, &[]);
	let new = name_of(new);
	assert!(
		confirmed.contains(&format!("Keep the paper of {old}")) && confirmed.contains("Blue stick"),
		"{confirmed}"
	);
	assert_eq!(listed(&mut test, &old)["replaced_by"], new.as_str());
	assert_eq!(listed(&mut test, &old)["revoked"], false);
	let shown = test.nib(&["recovery", "list"]).expect("lists");
	assert!(shown.contains(&format!("replaced by {new}")), "{shown}");
	// Updated, the backup carries the new code, and the old one is revoked.
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	assert_eq!(listed(&mut test, &old)["revoked"], true);
	assert_eq!(listed(&mut test, &new)["revoked"], false);
}

#[test]
fn a_new_code_replaces_the_one_before_at_once_with_revoke_older_or_with_no_backup() {
	let mut test = library("recovery, replaced at once");
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"recovery, replaced at once, drive",
	)
	.join("NibKeep");
	let first = name_of(make_code(&mut test));
	// With no backup, a new code replaces the one before at once.
	let (second, confirmed) = make_code_saying(&mut test, &[]);
	let second = name_of(second);
	assert!(
		confirmed.contains(&format!("It replaces {first}, revoked now")),
		"{confirmed}"
	);
	assert_eq!(listed(&mut test, &first)["revoked"], true);
	// With a backup, --revoke-older does as the person says.
	let path = folder.to_str().expect("UTF-8");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	let (_, confirmed) = make_code_saying(&mut test, &["--revoke-older"]);
	assert!(
		confirmed.contains(&format!("It replaces {second}, revoked now")),
		"{confirmed}"
	);
	assert_eq!(listed(&mut test, &second)["revoked"], true);
}

/// A time after the tests' changes, for syncs.
fn later() -> u64 {
	nibkeep_testkit::millis_at("2026-11-13", 11, 0)
}

/// A test's library, opened as nib opens it, with the mock keystore.
fn open(test: &Test) -> Library {
	Library::open(&test.here.join(".nib"), &Unlocker::Keystore).expect("opens")
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
fn revoking_a_device_names_the_codes_that_it_made_and_revokes_them_with_codes() {
	let mut laptop = library("recovery, a device's codes, laptop");
	let desk = Test::with_keystore("recovery, a device's codes, desk");
	common::pair_on_localhost(
		&mut open(&laptop),
		&desk.here.join(".nib"),
		("desk", None),
		&NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
		later(),
		&mut TestRng::new("pairing the desk"),
	);
	// The desk makes a code, which the laptop learns of.
	let mut rng = TestRng::new("the desk's code");
	let code = RecoveryCode::generate(&mut rng);
	open(&desk)
		.add_recovery_code(&code, false, later(), &mut rng)
		.expect("names the code");
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	sync(&desk, &laptop);
	// Without --yes, the laptop says what it would revoke, the code too, and how.
	let (would, revoked) = laptop.nib_printing(&["device", "revoke", "desk"]);
	assert!(revoked.is_err(), "not without --yes");
	assert!(
		would.contains(&format!("Recovery codes made on it: {name}")) && would.contains("--codes"),
		"{would}"
	);
	let revoked = laptop
		.nib(&["device", "revoke", "desk", "--yes", "--codes"])
		.expect("revokes");
	assert!(
		revoked.contains(&format!(
			"Revoked the recovery codes made on it too: {name}"
		)),
		"{revoked}"
	);
	assert_eq!(listed(&mut laptop, &name)["revoked"], true);
}

#[test]
fn revoke_older_revokes_only_the_codes_before_the_newest() {
	let mut test = library("recovery, revoke older");
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"recovery, revoke older, drive",
	)
	.join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	let old = make_code(&mut test);
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	let new = name_of(make_code(&mut test));
	// The old code typed back with --revoke-older stays: the newest replaces it.
	test.answer(&[old]);
	test.nib(&["recovery", "confirm", "--revoke-older"])
		.expect("confirms");
	let old = name_of(old);
	assert_eq!(listed(&mut test, &old)["revoked"], false);
	assert_eq!(listed(&mut test, &old)["replaced_by"], new.as_str());
}

#[test]
fn a_library_comes_back_on_a_new_computer_from_its_code_and_a_backup() {
	let mut test = library("recovery, recovering");
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"recovery, recovering, drive",
	)
	.join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	let code = make_code(&mut test);
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	// The laptop is lost. On a new computer, a code of another library opens nothing, and
	// nothing is made, before any passphrase is asked for.
	let mut new = Test::with_keystore("recovery, recovering, new computer");
	let other = RecoveryCode::generate(&mut TestRng::new("another library's code"));
	let other: &'static str = Box::leak(other.to_text().as_str().to_owned().into_boxed_str());
	new.answer(&[other]);
	let refused = new.nib(&["recovery", "restore", path, "--device", "desktop"]);
	assert!(
		refused
			.err()
			.is_some_and(|error| error.to_string().contains("opens nothing")),
	);
	assert_eq!(new.asked, 1);
	assert!(!new.here.join(".nib").exists());
	// The right code brings it back, and revokes the laptop; as the library has no secrets,
	// nothing more is asked.
	new.answer(&[code]);
	let recovered = new
		.nib(&["recovery", "restore", path, "--device", "desktop"])
		.expect("recovers");
	assert_eq!(new.asked, 2);
	assert!(!recovered.contains("secrets"), "{recovered}");
	assert!(
		recovered.contains("Brought your library back from the backup Blue stick"),
		"{recovered}"
	);
	assert!(recovered.contains("which you lost: laptop"), "{recovered}");
	assert!(
		recovered.contains("Your spaces came back too: Household."),
		"{recovered}"
	);
	let devices = new.json(&["device", "list"]);
	let devices = devices.as_array().expect("a list");
	assert_eq!(devices.len(), 2);
	for device in devices {
		let this = device["this"] == true;
		assert_eq!(device["revoked"], !this, "{device}");
		if this {
			assert_eq!(device["device_name"], "desktop");
		}
	}
	// The code still stands, and is the library's.
	assert_eq!(new.json(&["recovery", "list"])[0]["revoked"], false);
}

/// Pairs a device named `name` with the library of `laptop`, in a test of its own whose name
/// `test` starts.
fn pair(laptop: &Test, (test, name): (&str, &str)) -> Test {
	let device = Test::with_keystore(&format!("{test}, {name}"));
	common::pair_on_localhost(
		&mut open(laptop),
		&device.here.join(".nib"),
		(name, None),
		&NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
		later(),
		&mut TestRng::new(&format!("pairing the {name}")),
	);
	device
}

#[test]
fn a_recovery_keeps_the_devices_named_or_said_to_be_at_hand() {
	let mut laptop = library("recovery, keeping");
	// Every device's clock after the pairings, so that none takes the others' changes for
	// ones from the future.
	laptop.now = later();
	let _desk = pair(&laptop, ("recovery, keeping", "desk"));
	let _phone = pair(&laptop, ("recovery, keeping", "phone"));
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "recovery, keeping, drive")
			.join("NibKeep");
	let path = folder.to_str().expect("UTF-8");
	laptop
		.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	let code = make_code(&mut laptop);
	laptop
		.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	// The laptop and the desk are lost. A device that --keep does not find ends the recovery,
	// which leaves nothing.
	let mut new = Test::with_keystore("recovery, keeping, new computer");
	new.now = later();
	new.answer(&[code]);
	let refused = new.nib(&[
		"recovery", "restore", path, "--device", "desktop", "--keep", "tablet",
	]);
	let error = refused.expect_err("no tablet");
	assert!(error.to_string().contains("tablet"), "{error}");
	assert!(Library::replica_folder(&new.here.join(".nib")).is_err());
	// With --keep, the phone stays, and nothing is asked about the others.
	new.answer(&[code]);
	new.answers = Some(std::collections::VecDeque::new());
	let recovered = new
		.nib(&[
			"recovery", "restore", path, "--device", "desktop", "--keep", "phone",
		])
		.expect("recovers");
	assert_eq!(new.questions, [] as [String; 0]);
	assert!(
		recovered.contains("which you lost: desk, laptop."),
		"{recovered}"
	);
	assert!(
		recovered.contains(
			"Kept phone, which you still have: it learns of this device, and of the revocations, the first time this device syncs with it: run nib sync here while it serves, as nib sync --serve, syncing from login or an app does; or run nib backup update \"Blue stick\" there."
		),
		"{recovered}"
	);
	let devices = new.json(&["device", "list"]);
	for device in devices.as_array().expect("a list") {
		let at_hand = device["this"] == true || device["device_name"] == "phone";
		assert_eq!(device["revoked"], !at_hand, "{device}");
	}
	// Then the desktop is lost too. Recovering again from the backup, which carries it, asks
	// about each device not revoked yet, by name, and keeps those the person still has.
	let mut again = Test::with_keystore("recovery, keeping, again");
	again.now = later();
	again.answer(&[code]);
	again.answers = Some(["n", "yes"].into());
	let recovered = again
		.nib(&["recovery", "restore", path, "--device", "new desktop"])
		.expect("recovers");
	assert_eq!(again.questions.len(), 2, "{:?}", again.questions);
	assert!(
		again.questions[0].starts_with("Keep desktop (nib)? Only if you still have it"),
		"{:?}",
		again.questions
	);
	assert!(
		again.questions[1].starts_with("Keep phone (nib)?"),
		"{:?}",
		again.questions
	);
	assert!(
		recovered.contains("which you lost: desktop."),
		"{recovered}"
	);
	// With --json, nib runs for a program, which is not asked: every other device goes.
	let mut scripted = Test::with_keystore("recovery, keeping, scripted");
	scripted.now = later();
	scripted.answer(&[code]);
	scripted.answers = Some(["yes", "yes"].into());
	scripted
		.nib(&["--json", "recovery", "restore", path])
		.expect("recovers");
	assert_eq!(scripted.questions, [] as [String; 0]);
	assert!(
		recovered.contains("Kept phone, which you still have:"),
		"{recovered}"
	);
}

/// A library with a login whose password is secret, its secrets under the passphrase
/// `secrets passphrase`, and a backup named Blue stick in a folder of its own; with the
/// backup's path.
fn library_with_a_password(name: &str) -> (Test, &'static str) {
	let mut test = library(name);
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, drive"))
			.join("NibKeep");
	let path: &'static str = Box::leak(folder.to_str().expect("UTF-8").to_owned().into_boxed_str());
	test.nib(&["type", "add", "login", "title:text", "password:text:secret"])
		.expect("adds a type");
	test.nib(&["new", "login", "title=Bank"])
		.expect("adds a login");
	test.answer(&[
		"secrets passphrase",
		"secrets passphrase",
		"hunter2",
		"hunter2",
	]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets the password");
	test.nib(&["backup", "new", path, "--name", "Blue stick"])
		.expect("adds");
	(test, path)
}

#[test]
fn a_code_made_with_the_secrets_brings_them_back_under_a_passphrase_of_their_own() {
	let (mut test, path) = library_with_a_password("recovery, recovering the secrets");
	let made = test.nib(&["recovery", "new"]).expect("makes");
	let code = printed_code(&made);
	test.answer(&["secrets passphrase", code]);
	test.nib(&["recovery", "confirm", "--secrets"])
		.expect("confirms");
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	// The laptop is lost. The code, then a passphrase for the secrets here, twice: one
	// that is the library's, or typed differently, is asked for again, three times at most,
	// and then nothing is left.
	let mut new = Test::new("recovery, recovering the secrets, new computer");
	new.answer(&[
		code, "library", "library", "library", "library", "one", "other", "library", "library",
	]);
	let refused = new.nib(&["recovery", "restore", path, "--passphrase"]);
	assert!(
		refused
			.err()
			.is_some_and(|error| error.to_string().contains("passphrases of their own")),
	);
	assert_eq!(new.notes.len(), 2, "{:?}", new.notes);
	assert!(!Library::exists(&new.here));
	new.answer(&[
		code,
		"library",
		"library",
		"library",
		"library",
		"new secrets",
		"new secrets",
	]);
	let recovered = new
		.nib(&["recovery", "restore", path, "--passphrase"])
		.expect("recovers");
	assert!(
		recovered.contains("Your secrets came back too"),
		"{recovered}"
	);
	new.answer(&["library", "new secrets"]);
	let shown = new
		.nib(&["secret", "show", "Bank", "password"])
		.expect("shows");
	assert!(shown.contains("hunter2"), "{shown}");
	new.answer(&["library"]);
	assert_eq!(new.json(&["recovery", "list"])[0]["holds_secrets"], true);
}

#[test]
fn a_code_made_without_the_secrets_says_that_they_did_not_come_back() {
	let (mut test, path) = library_with_a_password("recovery, the secrets lost");
	let code = make_code(&mut test);
	test.nib(&["backup", "update", "Blue stick"])
		.expect("updates");
	let mut new = Test::with_keystore("recovery, the secrets lost, new computer");
	new.answer(&[code]);
	let recovered = new.nib(&["recovery", "restore", path]).expect("recovers");
	assert_eq!(new.asked, 1);
	assert!(
		recovered.contains("Your secrets did not come back"),
		"{recovered}"
	);
}

#[test]
fn a_library_that_no_longer_unlocks_comes_back_in_its_own_folder() {
	let mut test = library("recovery, in place");
	test.nib(&["space", "new", "Household"]).expect("makes");
	let code = make_code(&mut test);
	let old = Library::replica_folder(&test.here.join(".nib")).expect("here");
	// While it unlocks, there is nothing to recover, which is said before the code is
	// asked for.
	let asked = test.asked;
	let refused = test.nib(&["recovery", "restore"]);
	assert!(
		refused
			.err()
			.is_some_and(|error| error.to_string().contains("still unlocks")),
	);
	assert_eq!(test.asked, asked);
	// Its keystore entry is lost. Without a source, the library here is the one that comes
	// back, with the code alone asked for.
	let Ok(Protection::Keystore { service, account }) =
		Library::protection(&test.here.join(".nib"))
	else {
		panic!("under the keystore");
	};
	keyring_core::Entry::new(&service, &account)
		.and_then(|entry| entry.delete_credential())
		.expect("loses the entry");
	test.answer(&[code]);
	let recovered = test
		.nib(&["recovery", "restore", "--device", "desktop again"])
		.expect("recovers");
	assert_eq!(test.asked, asked + 1);
	assert!(
		recovered.contains("Brought your library back in place of the one here"),
		"{recovered}"
	);
	assert!(
		recovered.contains("Its old folder is set aside in"),
		"{recovered}"
	);
	assert!(!recovered.contains("which you lost"), "{recovered}");
	assert!(
		recovered.contains("Your spaces came back too: Household."),
		"{recovered}"
	);
	assert!(!old.exists());
	let devices = test.json(&["device", "list"]);
	let devices = devices.as_array().expect("a list");
	assert_eq!(devices.len(), 2);
	for device in devices {
		let this = device["this"] == true;
		assert_eq!(device["revoked"], !this, "{device}");
		if this {
			assert_eq!(device["device_name"], "desktop again");
		}
	}
	// Named by its folder, a library elsewhere is a source too; the one here now unlocks, so
	// it stays.
	let path = test.here.join(".nib");
	let path = path.to_str().expect("UTF-8");
	let mut other = Test::with_keystore("recovery, in place, elsewhere");
	other.answer(&[code]);
	let recovered = other.nib(&["recovery", "restore", path]).expect("recovers");
	assert!(
		recovered.contains("That folder is left as it was"),
		"{recovered}"
	);
	assert!(Library::replica_folder(&test.here.join(".nib")).is_ok());
}

#[test]
fn recovering_where_a_library_is_asks_nothing() {
	let mut test = library("recovery, a library here");
	let refused = test.nib(&["recovery", "restore", "anywhere"]);
	assert!(
		refused
			.err()
			.is_some_and(|error| error.to_string().contains("a library here already")),
	);
	assert_eq!(test.asked, 0);
}
