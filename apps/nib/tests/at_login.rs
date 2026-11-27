//! `nib sync --at-login`: syncing a library from login, through `nibsync`
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)).

use std::path::PathBuf;

use crate::common::{Test, library};
use nibkeep_cli::CliError;
use nibkeep_cli::Firewall;
use nibkeep_node::at_login::{AtLogin, ENGINE_VERSION, EntryState, LoginEntry, Served};

/// A library unlocked by the mock keystore, with a folder of state and a copy of
/// `nibsync` beside `nib`.
fn synced_library(name: &str) -> Test {
	let mut test = library(name);
	test.state = Some(test.here.join("state"));
	let program = test.here.join("bin").join("nibsync");
	std::fs::create_dir_all(program.parent().expect("a folder")).expect("makes");
	std::fs::write(&program, "").expect("writes");
	test.sync_program = Some(program);
	test.login_entry = Some(test.here.join("autostart").join("nibsync.desktop"));
	test
}

/// What the test's login entry says.
fn entry(test: &Test) -> EntryState {
	LoginEntry::autostart(test.login_entry.clone().expect("an entry"))
		.state()
		.expect("reads")
}

fn listed(test: &Test) -> AtLogin {
	AtLogin::read(test.state.as_deref().expect("a folder of state")).expect("reads")
}

/// The library's directory, as the list names it.
fn directory(test: &Test) -> PathBuf {
	std::path::absolute(test.here.join(".nib")).expect("absolute")
}

#[test]
fn a_library_is_synced_from_login_and_stops_being() {
	let mut test = synced_library("at login on and off");
	let printed = test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert_eq!(
		printed,
		"Syncing from login is on: nibsync syncs this library with your other devices whenever you are logged in here, with no app open.\n"
	);
	assert_eq!(
		entry(&test),
		EntryState::On(test.sync_program.clone().expect("a copy"))
	);
	let list = listed(&test);
	assert_eq!(list.program, test.sync_program);
	assert_eq!(list.libraries.len(), 1);
	assert_eq!(list.libraries[0].directory, directory(&test));
	let this = test.json(&["device", "list"])[0].clone();
	assert_eq!(this["this"], true);
	assert_eq!(
		list.libraries[0].replica.to_string(),
		this["id"].as_str().expect("an ID"),
		"this computer's replica"
	);
	assert_eq!(
		this["at_login"]["state"], "not running",
		"nothing runs in the test"
	);
	assert_eq!(this["at_login"]["login_entry"], "on");
	assert_eq!(test.started, [test.sync_program.clone().expect("a copy")]);
	assert_eq!(test.asked, 0, "the keystore opens it");

	let printed = test.nib(&["device", "list"]).expect("lists");
	assert!(
		printed.ends_with(
			"Syncing from login is on, but nibsync is not running; nib sync --at-login on starts it.\n"
		),
		"{printed}"
	);

	let json = test.json(&["sync", "--at-login", "off"]);
	assert_eq!(json["at_login"], false);
	assert_eq!(json["was"], true);
	assert_eq!(listed(&test).libraries, Vec::<Served>::new());
	assert_eq!(entry(&test), EntryState::Missing, "with the last library");
	assert_eq!(
		test.nib(&["sync", "--at-login", "off"]).expect("turns off"),
		"Syncing from login was not on for this library.\n"
	);
	let this = test.json(&["device", "list"])[0].clone();
	assert!(this.get("at_login").is_none(), "{this}");
}

#[test]
fn a_newer_copy_named_already_stays_and_an_older_one_gives_way() {
	let mut test = synced_library("at login newer copy");
	let own = test.sync_program.clone().expect("a copy");
	let editors = test.here.join("editor").join("nibsync");
	for copy in [&own, &editors] {
		std::fs::create_dir_all(copy.parent().expect("a folder")).expect("makes");
		std::fs::write(copy, "").expect("writes");
	}
	AtLogin::edit(test.state.as_deref().expect("state"), |list| {
		list.program = Some(editors.clone());
	})
	.expect("names");
	test.sync_versions = vec![
		(own.clone(), ENGINE_VERSION.to_owned()),
		(editors.clone(), "999.0.0".to_owned()),
	];
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert_eq!(listed(&test).program, Some(editors.clone()));
	assert_eq!(test.started, std::slice::from_ref(&editors), "none runs");

	// An older one gives way to the copy beside nib, which starts at once, to take over as
	// the one that runs ends.
	test.sync_versions[1].1 = "0.0.1".to_owned();
	assert_ne!(ENGINE_VERSION, "0.0.1");
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert_eq!(listed(&test).program, Some(own.clone()));
	assert_eq!(test.started, [editors, own]);
}

#[test]
fn a_library_deleted_or_moved_since_is_turned_off_by_its_old_folder_or_its_replica() {
	let mut test = synced_library("at login moved");
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	let old = test.here.clone();
	let moved = old.with_file_name("at login moved elsewhere");
	let _ = std::fs::remove_dir_all(&moved);
	std::fs::rename(&old, &moved).expect("moves");
	std::fs::create_dir_all(&old).expect("makes");
	let state = moved.join("state");
	test.state = Some(state.clone());

	// From its new folder, by its replica.
	test.here = moved.clone();
	assert_eq!(
		test.nib(&["sync", "--at-login", "off"]).expect("turns off"),
		"Syncing from login is off for this library.\n"
	);
	assert_eq!(listed(&test).libraries, Vec::<Served>::new());

	// By the old folder, which holds no library any more.
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	std::fs::remove_dir_all(moved.join(".nib")).expect("deletes");
	let old_folder = moved.to_str().expect("Unicode").to_owned();
	assert_eq!(
		test.nib(&["--library", &old_folder, "sync", "--at-login", "off"])
			.expect("turns off"),
		"Syncing from login is off for this library.\n"
	);
	assert_eq!(listed(&test).libraries, Vec::<Served>::new());
}

#[test]
fn a_library_protected_by_a_passphrase_is_not_synced_from_login() {
	let mut test = Test::new("at login passphrase");
	test.state = Some(test.here.join("state"));
	test.sync_program = Some(test.here.join("nibsync"));
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada", "--passphrase"])
		.expect("starts");
	let asked = test.asked;
	assert!(matches!(
		test.nib(&["sync", "--at-login", "on"]),
		Err(CliError::AtLoginPassphrase)
	));
	assert_eq!(test.asked, asked, "nothing asked for in vain");
	assert_eq!(listed(&test).libraries, Vec::<Served>::new());
	assert_eq!(test.started, Vec::<PathBuf>::new());
}

#[test]
fn protecting_with_a_passphrase_turns_syncing_from_login_off() {
	let mut test = synced_library("at login protect");
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	test.answer(&["new horse", "new horse"]);
	test.nib(&["protect", "--passphrase"]).expect("protects");
	assert_eq!(listed(&test).libraries, Vec::<Served>::new());
	assert_eq!(
		test.notes,
		[
			"nib: syncing from login is off for this library now, as it needs the library's keys in the keystore"
		]
	);
}

#[test]
fn without_nibsync_beside_nib_it_says_how_to_install_it() {
	let mut test = library("at login without nibsync");
	test.state = Some(test.here.join("state"));
	assert!(matches!(
		test.nib(&["sync", "--at-login", "on"]),
		Err(CliError::NoSyncProgram)
	));
	test.state = None;
	assert!(matches!(
		test.nib(&["sync", "--at-login", "on"]),
		Err(CliError::NoStateFolder)
	));
}

#[test]
fn a_login_entry_that_the_person_turned_off_stays_off() {
	let mut test = synced_library("at login entry off");
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	let file = test.login_entry.clone().expect("an entry");
	let written = std::fs::read_to_string(&file).expect("reads");
	std::fs::write(&file, format!("{written}Hidden=true\n")).expect("writes");
	assert_eq!(
		test.nib(&["sync", "--at-login", "on"]).expect("turns on"),
		"Syncing from login is on, and nibsync syncs this library now; but its login entry is turned off in the system's startup settings, so it does not start when you log in.\n"
	);
	assert!(matches!(entry(&test), EntryState::Off(_)));
	let printed = test.nib(&["device", "list"]).expect("lists");
	assert!(
		printed.ends_with(
			"Syncing from login is on, but nibsync is not running; nib sync --at-login on starts it. Its login entry is turned off in the system's startup settings, so it does not start when you log in.\n"
		),
		"{printed}"
	);
}

#[test]
fn turning_on_asks_windows_firewall_to_let_the_other_devices_in() {
	let mut test = synced_library("at login firewall");
	test.firewall = Firewall::Opened;
	let printed = test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert!(
		printed.ends_with(
			"Windows' firewall lets your other devices reach this computer now, on private networks.\n"
		),
		"{printed}"
	);
	test.firewall = Firewall::Refused;
	let json = test.json(&["sync", "--at-login", "on"]);
	assert_eq!(json["firewall"], "refused");
	assert_eq!(json["login_entry"], "on");
	let printed = test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert!(
		printed.ends_with(
			"Windows' firewall was not changed, so your other devices may not reach this computer; nib sync --at-login on asks again.\n"
		),
		"{printed}"
	);
}

#[test]
fn a_login_entry_that_is_gone_is_told_and_made_again() {
	let mut test = synced_library("at login entry gone");
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	std::fs::remove_file(test.login_entry.clone().expect("an entry")).expect("removes");
	let printed = test.nib(&["device", "list"]).expect("lists");
	assert!(
		printed.ends_with(
			"Its login entry is not there, so it does not start when you log in; nib sync --at-login on makes it again.\n"
		),
		"{printed}"
	);
	assert_eq!(
		test.json(&["device", "list"])[0]["at_login"]["login_entry"],
		"missing"
	);
	test.nib(&["sync", "--at-login", "on"]).expect("turns on");
	assert!(matches!(entry(&test), EntryState::On(_)));
}
