//! Adding a device and joining one, in an app that reads no secrets, as far as they go
//! without another device.

use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::{ask, folder, made, turn};

/// How adding a device goes, once the wait ended, or after fifteen seconds.
fn ended() -> Json {
	let mut pairing = ask(&json!({"op": "pairing"}));
	// Starting the network takes a while, which a stop does not cut short.
	for _ in 0..150 {
		if pairing["pairing"] == false {
			break;
		}
		std::thread::sleep(Duration::from_millis(100));
		pairing = ask(&json!({"op": "pairing"}));
	}
	pairing
}

#[test]
fn devices_are_added_one_at_a_time_with_a_code_until_stopped_or_locked() {
	let _turn = turn();
	assert_eq!(ask(&json!({"op": "stop_adding"})), json!({}));
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	assert_eq!(ask(&json!({"op": "add_device"}))["kind"], "locked");
	made("adds devices");
	let nowhere = ask(&json!({"op": "add_device", "spaces": ["nowhere"]}));
	assert_eq!(nowhere["kind"], "request", "{nowhere}");
	// The secrets are never given here, so it needs none unlocked.
	let added = ask(&json!({"op": "add_device", "seconds": 60, "give_secrets": true}));
	assert_eq!(added["seconds"], 60, "{added}");
	let code = added["code"].as_str().expect("a code").to_owned();
	assert_eq!(code.chars().filter(char::is_ascii_digit).count(), 8);
	let pairing = ask(&json!({"op": "pairing"}));
	assert_eq!(pairing["pairing"], true, "{pairing}");
	assert_eq!(pairing["code"], code.as_str());
	let again = ask(&json!({"op": "add_device"}));
	assert_eq!(again["kind"], "request", "one at a time: {again}");
	assert_eq!(ask(&json!({"op": "stop_adding"})), json!({}));
	let stopped = ended();
	assert_eq!(stopped["pairing"], false, "{stopped}");
	assert!(stopped.get("failed").is_some(), "{stopped}");
	// Another wait starts once this one is over, and locking stops it.
	assert!(ask(&json!({"op": "add_device", "seconds": 60}))["code"].is_string());
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	let locked = ended();
	assert_eq!(locked["pairing"], false, "{locked}");
}

#[test]
fn devices_joining_is_refused_before_the_network_starts() {
	let _turn = turn();
	let new = folder("joins");
	let join = |code: &str, device: &str| {
		ask(&json!({
			"op": "join", "folder": new, "code": code, "device": device,
			"passphrase": "correct horse", "secrets": {"passphrase": "battery staple"},
		}))
	};
	assert_eq!(join("12-34", "phone")["kind"], "request");
	assert_eq!(join("12-345-678", " ")["kind"], "request");
	// A library here is opened, not joined.
	let here = made("joins where one is");
	let refused = ask(&json!({
		"op": "join", "folder": here, "code": "12-345-678", "device": "phone",
		"passphrase": "correct horse",
	}));
	assert_eq!(refused["kind"], "exists", "{refused}");
	let no_copy = ask(&json!({
		"op": "from_copy", "folder": folder("no copy"), "device": "phone",
		"copy_passphrase": "old", "passphrase": "new",
	}));
	assert_eq!(no_copy["kind"], "request", "{no_copy}");
}
