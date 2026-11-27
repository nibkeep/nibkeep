//! Requests and answers as JSON, and failures as answers.

use nibkeep_app::Failure;
use nibkeep_node::{KeysError, NodeError};
use serde_json::{Value as Json, json};

use crate::{ask, turn};

#[test]
fn calls_a_request_s_text_is_taken_out_and_wiped_with_the_rest() {
	let mut request = json!({"passphrase": "correct horse", "nested": {"secret": "battery"}});
	let taken = nibkeep_app::take(&mut request, "passphrase");
	assert_eq!(taken.as_str(), "correct horse");
	assert_eq!(request["passphrase"], Json::Null, "taken out");
	assert_eq!(nibkeep_app::take(&mut request, "absent").as_str(), "");
	nibkeep_app::wipe(&mut request);
	assert_eq!(request["nested"]["secret"], "", "wiped in place");
}

#[test]
fn calls_an_answer_is_a_c_string_of_its_json() {
	let answer = json!({"text": "a\u{0}b", "count": 2});
	let text = nibkeep_app::to_c_string(&answer);
	let read: Json = serde_json::from_slice(text.as_bytes()).expect("JSON");
	assert_eq!(read, answer, "NUL written as \\u0000");
}

#[test]
fn calls_a_panic_and_a_request_that_is_no_json_are_answered() {
	let _turn = turn();
	assert_eq!(
		ask(&json!({"op": "panic"})),
		json!({"error": "the engine failed", "kind": "panic"})
	);
	let text = nibkeep_app::call(b"not JSON", |request| {
		assert_eq!(request, Json::Null);
		json!({})
	});
	assert_eq!(text.as_bytes(), b"{}");
	nibkeep_app::free(text);
	assert_eq!(ask(&json!({"op": "lock"})), json!({}));
	assert_eq!(ask(&json!({"op": "nothing"}))["kind"], "locked");
}

#[test]
fn calls_failures_answer_with_kinds_that_apps_tell_apart() {
	let kind = |failure: Failure| failure.answer()["kind"].clone();
	assert_eq!(
		kind(NodeError::Keys(KeysError::WrongPassphrase).into()),
		"passphrase"
	);
	assert_eq!(kind(NodeError::Exists.into()), "exists");
	assert_eq!(kind(NodeError::NoOtherReplicas.into()), "no_other_devices");
	assert_eq!(kind(Failure::Locked), "locked");
	assert_eq!(kind(Failure::SecretsLocked), "secrets_locked");
	assert_eq!(kind(Failure::Request("no".to_owned())), "request");
	let own = Failure::app("clipboard", "the clipboard is busy");
	assert_eq!(
		own.answer(),
		json!({"error": "the clipboard is busy", "kind": "clipboard"})
	);
	assert_eq!(own.to_string(), "the clipboard is busy");
}
