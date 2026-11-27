//! The library held between calls: made, unlocked and locked; its spaces, and recovery
//! codes in an app that reads no secrets.

use serde_json::json;

use crate::{ask, folder, made, turn};

#[test]
fn library_is_made_unlocked_locked_and_unlocked_again() {
	let _turn = turn();
	let empty = folder("found");
	let found = ask(&json!({"op": "library_at", "folder": empty}));
	assert_eq!(found["exists"], false, "{found}");
	assert_eq!(found["keystore"], true, "the tests' keystore");
	assert_eq!(found["secrets_keystore"], false, "the app reads no secrets");
	let folder = made("unlocked");
	assert!(ask(&json!({"op": "spaces_here"}))["spaces"].is_array());
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	assert_eq!(ask(&json!({"op": "spaces_here"}))["kind"], "locked");
	let wrong = ask(&json!({"op": "unlock", "folder": folder, "passphrase": "wrong"}));
	assert_eq!(wrong["kind"], "passphrase", "{wrong}");
	let unlocked = ask(&json!({"op": "unlock", "folder": folder, "passphrase": "correct horse"}));
	assert!(unlocked["seconds"].is_number(), "{unlocked}");
	assert!(ask(&json!({"op": "spaces_here"}))["spaces"].is_array());
	let found = ask(&json!({"op": "library_at", "folder": folder}));
	assert_eq!(found["exists"], true, "{found}");
	assert_eq!(found["protection"], "passphrase");
	assert_eq!(found["problem"], json!(null));
	// Unlocking another, the one held goes, also if the other does not unlock.
	let empty = ask(&json!({"op": "unlock", "folder": folder, "passphrase": ""}));
	assert_eq!(empty["kind"], "passphrase", "{empty}");
	assert_eq!(ask(&json!({"op": "spaces_here"}))["kind"], "locked");
}

#[test]
fn library_made_in_a_folder_that_holds_one_or_not_at_all_leaves_nothing() {
	let _turn = turn();
	let folder = made("twice");
	let again = ask(&json!({
		"op": "create", "folder": folder, "user": "Alex", "device": "laptop",
		"passphrase": "correct horse",
	}));
	assert_eq!(again["kind"], "exists", "{again}");
	// Refused before any folder is made.
	let new = crate::folder("refused").join("new").join("deeper");
	let refused = ask(&json!({
		"op": "create", "folder": new, "user": "Alex", "device": "laptop", "passphrase": "",
	}));
	assert_eq!(refused["kind"], "request", "{refused}");
	assert!(!new.parent().expect("a parent").exists());
}

#[test]
fn library_locking_never_waits_for_a_call_which_lets_go_when_it_ends() {
	let _turn = turn();
	made("locked while held");
	// A call holds the library, as a sync does for a while; locking returns at once.
	let held = nibkeep_app::held::hold();
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	assert!(held.is_some(), "the call still has it");
	assert!(!held.unchanged());
	drop(held);
	assert_eq!(ask(&json!({"op": "spaces_here"}))["kind"], "locked");
}

#[test]
fn library_spaces_made_show_among_the_spaces_here() {
	let _turn = turn();
	made("spaces");
	let unnamed = ask(&json!({"op": "create_space", "name": " "}));
	assert_eq!(unnamed["kind"], "request");
	let space = ask(&json!({"op": "create_space", "name": "Family"}));
	let id = space["id"].as_str().expect("an ID");
	let here = ask(&json!({"op": "spaces_here"}));
	let family = here["spaces"]
		.as_array()
		.expect("spaces")
		.iter()
		.find(|space| space["id"] == id)
		.expect("listed");
	assert_eq!(family["name"], "Family");
	assert_eq!(family["here"], "held");
	assert_eq!(here["takes_new_spaces"], true);
	assert_eq!(
		ask(&json!({"op": "take_new_spaces", "takes": false})),
		json!({})
	);
	assert_eq!(
		ask(&json!({"op": "spaces_here"}))["takes_new_spaces"],
		false
	);
}

#[test]
fn library_recovery_codes_hold_no_secrets_in_an_app_that_reads_none() {
	let _turn = turn();
	made("recovery");
	let first = ask(&json!({"op": "new_recovery_code"}));
	let code = first["code"].as_str().expect("a code").to_owned();
	assert!(ask(&json!({"op": "new_recovery_code"}))["code"].is_string());
	let not_shown = ask(&json!({"op": "confirm_recovery_code", "code": code, "shown": true}));
	assert_eq!(
		not_shown["kind"], "request",
		"not the one shown last: {not_shown}"
	);
	// One written down earlier.
	let confirmed = ask(&json!({
		"op": "confirm_recovery_code", "code": code, "secrets": true,
	}));
	assert_eq!(confirmed["already"], false, "{confirmed}");
	assert_eq!(
		confirmed["code"]["holds_secrets"], false,
		"never the secrets"
	);
	let check = ask(&json!({"op": "check_recovery_code", "code": code}));
	assert_eq!(check["code"]["name"], confirmed["code"]["name"], "{check}");
	assert_eq!(check["lacks"], 0);
	assert_eq!(check["secrets"], json!(null));
}
