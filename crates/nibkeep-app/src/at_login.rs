//! Syncing from login ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)),
//! as the engine's `at_login::app` does it for an app: the switch, which has `nibsync`,
//! shipped beside the app, sync the library whenever the person is logged in, with no app
//! open; and what the app does as it starts. The library is held only to read which it is,
//! never while copies tell their versions, nor while Windows asks for consent.

use std::path::{Path, PathBuf};

use nibkeep_node::at_login::{Served, app};
use nibkeep_node::{Library, Protection};
use serde_json::{Value as Json, json};

use crate::held::with_open;
use crate::{Failure, take};

/// Answers a request of syncing from login.
///
/// # Errors
///
/// A [`Failure::Request`] for an operation it does not know, and as the operation fails.
pub fn answer(op: &str, request: &mut Json) -> Result<Json, Failure> {
	match op {
		"at_login_place" => Ok(set_place(request)),
		// It needs no library, to start the copy that should run as the app starts.
		"at_login_started" => started(),
		"at_login" => state(),
		"at_login_on" => turn_on(),
		"at_login_off" => turn_off(),
		_ => Err(Failure::Request(format!("there is no operation {op}"))),
	}
}

/// Puts syncing from login in a place of a test's own, as `folder`, `entry` and `program`
/// say, or, without a folder, in none: for tests, which keep away from the person's.
pub fn set_place(request: &mut Json) -> Json {
	let path = |text: &str| (!text.is_empty()).then(|| PathBuf::from(text));
	app::set_test_place(
		path(&take(request, "folder")),
		path(&take(request, "entry")),
		path(&take(request, "program")),
	);
	json!({})
}

/// What the app does as it starts: the copy of `nibsync` that it started, if any.
///
/// # Errors
///
/// Where the copy that should run cannot be started.
pub fn started() -> Result<Json, Failure> {
	let started = app::started()?;
	Ok(json!({"started": started.map(|program| program.to_string_lossy().into_owned())}))
}

/// The library's directory, and its replica here, read while it is held for a moment.
fn which() -> Result<(Option<PathBuf>, Option<nibkeep_base::EntityId>), Failure> {
	with_open(|library| {
		Ok((
			library.library_directory().map(Path::to_owned),
			own_replica(library).ok(),
		))
	})
}

/// The ID of the library's own replica.
fn own_replica(library: &Library) -> Result<nibkeep_base::EntityId, Failure> {
	library
		.replicas()?
		.into_iter()
		.find(|replica| replica.this)
		.map(|replica| replica.id)
		.ok_or_else(|| Failure::Request("the library lists no replica of its own".to_owned()))
}

/// What syncing from login does for the library: whether the app can turn it on here, as on
/// a computer where it ships `nibsync`; whether it is on; whether the library's keys are
/// in the keystore, which it needs; what the copy that runs does, and `why` it fails, if it
/// does; and what the login entry says.
///
/// # Errors
///
/// Where the library is locked, or the engine fails.
pub fn state() -> Result<Json, Failure> {
	if !app::available() {
		return Ok(json!({"available": false}));
	}
	let (Some(directory), replica) = which()? else {
		return Ok(json!({"available": false}));
	};
	let Some(status) = app::status(&directory, replica) else {
		return Ok(json!({"available": false}));
	};
	let keystore = matches!(
		Library::protection(&directory)?,
		Protection::Keystore { .. }
	);
	let (serving, error) = match &status.serving {
		None => ("not running", None),
		Some(serving) => (serving.word(), serving.error()),
	};
	let mut shown = json!({
		"available": true,
		"on": status.on,
		"keystore": keystore,
		"state": serving,
		"entry": status.entry_word(),
	});
	// The app takes an answer with an error for a failure.
	if let Some(error) = error {
		shown["why"] = json!(error);
	}
	Ok(shown)
}

/// Turns syncing from login on: what became of the login entry, and of Windows' firewall,
/// and the copy started.
///
/// # Errors
///
/// Where the library is locked or lists no replica of its own, syncing from login is not available here, or the engine fails.
pub fn turn_on() -> Result<Json, Failure> {
	let (Some(directory), Some(replica)) = which()? else {
		return Err(Failure::Request(
			"the library lists no replica of its own".to_owned(),
		));
	};
	let Some(turned) = app::turn_on(Served { replica, directory })? else {
		return Err(Failure::Request(
			"syncing from login is not available here".to_owned(),
		));
	};
	Ok(json!({
		"entry": turned.entry.word(),
		"entry_error": turned.entry.error(),
		"firewall": turned.firewall,
		"started": turned.started.map(|program| program.to_string_lossy().into_owned()),
	}))
}

/// Turns syncing from login off: whether it was on, and why the login entry could not be
/// removed with the last library, if it could not.
///
/// # Errors
///
/// Where the library is locked, or the engine fails.
pub fn turn_off() -> Result<Json, Failure> {
	let (Some(directory), replica) = which()? else {
		return Ok(json!({"was": false}));
	};
	let turned_off = app::turn_off(&directory, replica)?;
	Ok(json!({"was": turned_off.removed, "entry_error": turned_off.entry_failed}))
}

/// Turns syncing from login off for a library that a passphrase protects now, as nobody is
/// there to type it at login: whether it was on.
pub fn protected_with_passphrase(library: &Library) -> bool {
	let Some(directory) = library.library_directory() else {
		return false;
	};
	app::turn_off(directory, own_replica(library).ok()).is_ok_and(|turned_off| turned_off.removed)
}
