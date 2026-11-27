//! Joining a space that someone shares, as far as it goes without the inviting device: a
//! library made to join one, kept unless it was locked as it joined, and what stops a join.

use std::time::{Duration, Instant};

use nibkeep_app::{Failure, joining};
use nibkeep_node::NodeError;
use serde_json::json;

use crate::{ask, folder, lock_as_it_lets_go, made, turn};

#[test]
fn joining_a_library_made_to_join_stays_unlocked_if_joining_fails() {
	let _turn = turn();
	let folder = folder("made to join");
	let failed = ask(&json!({
		"op": "create_and_join", "folder": folder, "user": "Bea", "device": "phone",
		"passphrase": "correct horse", "code": "11-111-111", "seconds": 1,
	}));
	assert!(failed["failed"]["error"].is_string(), "{failed}");
	assert!(
		ask(&json!({"op": "spaces_here"}))["spaces"].is_array(),
		"kept, unlocked"
	);
}

#[test]
fn joining_a_library_made_to_join_stays_locked_if_locked_as_it_joins() {
	let _turn = turn();
	let folder = folder("locked as it joins");
	// Locked once the library is made, as it lets go of the one held before to join.
	lock_as_it_lets_go();
	let started = Instant::now();
	let answer = ask(&json!({
		"op": "create_and_join", "folder": folder, "user": "Bea", "device": "phone",
		"passphrase": "correct horse", "code": "11-111-111", "seconds": 30,
	}));
	assert_eq!(answer["kind"], "locked", "{answer}");
	assert!(
		started.elapsed() < Duration::from_secs(20),
		"the lock stopped its join before it looked for the inviting device"
	);
	assert_eq!(ask(&json!({"op": "spaces_here"}))["kind"], "locked");
	let unlocked = ask(&json!({"op": "unlock", "folder": folder, "passphrase": "correct horse"}));
	assert!(
		unlocked["seconds"].is_number(),
		"the library made stays, locked: {unlocked}"
	);
}

#[test]
fn joining_a_space_is_stopped_by_nothing_from_before_it_started() {
	let _turn = turn();
	made("joins a space");
	// As the library held before went, which stops the joins under way then.
	joining::stop();
	let looked = ask(&json!({"op": "join_space", "code": "11-111-111", "seconds": 1}));
	assert!(looked["error"].is_string(), "{looked}");
	assert_ne!(
		looked,
		Failure::Node(NodeError::PairingExpired).answer(),
		"it looked for the inviting device"
	);
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	let refused = ask(&json!({"op": "join_space", "code": "11-111-111", "seconds": 1}));
	assert_eq!(refused["kind"], "locked", "{refused}");
}
