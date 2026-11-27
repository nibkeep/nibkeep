//! Bringing a library back with a recovery code, in an app that reads no secrets: kept,
//! unlocked, unless the library was locked as it came back.

use serde_json::{Value as Json, json};

use crate::{ask, folder, lock_as_it_lets_go, made, turn};

/// A library in a folder of its own, with a recovery code that it names, locked: the
/// folder and the code.
fn written_down(name: &str) -> (String, String) {
	let folder = made(name);
	let code = ask(&json!({"op": "new_recovery_code"}))["code"]
		.as_str()
		.expect("a code")
		.to_owned();
	let confirmed = ask(&json!({"op": "confirm_recovery_code", "code": code, "shown": true}));
	assert_eq!(confirmed["already"], false, "{confirmed}");
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	(folder, code)
}

/// Starts bringing back the library in `source` with `code`, into a folder of its own, as
/// the device "new laptop": the folder.
fn started(source: &str, code: &str, name: &str) -> String {
	let folder = folder(name).to_str().expect("UTF-8").to_owned();
	let asking = ask(&json!({
		"op": "start_recovery", "code": code, "folder": folder, "source": source,
		"device": "new laptop", "passphrase": "battery staple",
	}));
	assert!(asking["others"].is_array(), "{asking}");
	folder
}

/// Unlocks the library in `folder` with the passphrase given as it came back.
fn unlocked(folder: &str) -> Json {
	ask(&json!({"op": "unlock", "folder": folder, "passphrase": "battery staple"}))
}

#[test]
fn recovery_brings_a_library_back_unlocked() {
	let _turn = turn();
	let (source, code) = written_down("recovery");
	let folder = started(&source, &code, "recovery, here");
	let done = ask(&json!({"op": "finish_recovery", "keep": []}));
	assert!(done["spaces"].is_array(), "{done}");
	assert!(ask(&json!({"op": "spaces_here"}))["spaces"].is_array());
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	assert!(unlocked(&folder)["seconds"].is_number());
}

#[test]
fn recovery_leaves_the_library_locked_if_locked_as_it_comes_back() {
	let _turn = turn();
	let (source, code) = written_down("recovery locked");
	let folder = started(&source, &code, "recovery locked, here");
	// Locked once the library is back, as it lets go of the one held before to keep it.
	lock_as_it_lets_go();
	let answer = ask(&json!({"op": "finish_recovery", "keep": []}));
	assert_eq!(answer["kind"], "locked", "{answer}");
	assert_eq!(ask(&json!({"op": "spaces_here"}))["kind"], "locked");
	let unlocked = unlocked(&folder);
	assert!(
		unlocked["seconds"].is_number(),
		"the library brought back stays, locked: {unlocked}"
	);
}
