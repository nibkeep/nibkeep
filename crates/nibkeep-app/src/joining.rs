//! Joining a space that someone shares, with the code their device shows, as `nib space
//! join` does ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#137-invitations)):
//! with the library unlocked, or making a library first, for someone who has none yet.
//!
//! A join borrows the network from the program that syncs this device, as adding a device
//! does; locking the library meanwhile stops it, before it finds the inviting device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::EntityId;
use nibkeep_node::{Library, NetworkOptions, PairingCode};
use serde_json::{Value as Json, json};

use crate::held::{hold, keep, let_go, waiting};
use crate::setup::{Protect, create_library};
use crate::{Failure, holding, take, wall_millis};

/// How long joining waits to find the device that invites, unless the request says less.
const JOIN_WAIT: Duration = Duration::from_mins(1);

/// How long a join waits to open the library, for a program that has it to itself.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// Set, it stops a join that waits for the network to be lent.
static JOINING: AtomicBool = AtomicBool::new(false);

/// The code that the request's `code` holds: the eight digits shown.
///
/// # Errors
///
/// A [`Failure::Request`] if it holds none.
pub fn code_arg(request: &Json) -> Result<PairingCode, Failure> {
	request["code"]
		.as_str()
		.unwrap_or_default()
		.trim()
		.parse()
		.map_err(|_| Failure::Request("the code is the eight digits shown".to_owned()))
}

/// How long a join waits to find the inviting device: a minute, or the request's `seconds`
/// if fewer.
#[must_use]
pub fn join_wait(request: &Json) -> Duration {
	request["seconds"]
		.as_u64()
		.map_or(JOIN_WAIT, Duration::from_secs)
		.min(JOIN_WAIT)
}

/// Stops a join that waits for the network to be lent, as the library is locked.
pub fn stop() {
	JOINING.store(true, Ordering::SeqCst);
}

/// Joins a shared space with the request's `code`, waiting a minute, or `seconds`, to find
/// the device that invites this one, and gives it to the person's devices that the request
/// names in `also`, besides those that take new spaces, as [`join_with`] says. It works on
/// a library of its own, without the secrets, which joining never needs, so that the app's
/// other calls go on while the person who invites decides.
///
/// # Errors
///
/// Where the code is wrong, the library is locked, or joining fails.
pub fn join_space(request: &Json) -> Result<Json, Failure> {
	let code = code_arg(request)?;
	let copy = {
		let held = hold();
		let Some(closed) = held.as_ref() else {
			return Err(Failure::Locked);
		};
		let mut copy = waiting(|| closed.open())?.close();
		copy.lock_secrets();
		copy
	};
	let also = holding::replicas_arg(request, "also")?;
	let mut library = nibkeep_node::retry_in_use(OPEN_WAIT, || copy.open())?;
	join_with(&mut library, &code, join_wait(request), &also)
}

/// Joins a shared space with `code` on `library`, waiting up to `wait` to find the inviting
/// device, and gives it to the person's devices `also`, besides those that take new spaces:
/// the space, its name, the role the invitation gives, the person it names this user, if
/// any, and why it was not given to those devices, if it was not.
///
/// # Errors
///
/// Where the network cannot be borrowed, or joining fails.
pub fn join_with(
	library: &mut Library,
	code: &PairingCode,
	wait: Duration,
	also: &[EntityId],
) -> Result<Json, Failure> {
	JOINING.store(false, Ordering::SeqCst);
	let borrowed = nibkeep_node::borrow_network(library, &JOINING, UnwrapErr(SysRng))?;
	let network = library.network(NetworkOptions {
		discovery: true,
		port: None,
		accept: false,
	})?;
	let joined = nibkeep_node::join_space(
		library,
		&network,
		code,
		wait,
		&wall_millis,
		&mut UnwrapErr(SysRng),
	);
	drop(network);
	drop(borrowed);
	let joined = joined?;
	// The devices chosen besides those that take new spaces, as nib space join --on does:
	// the space is joined by now, so a failure to give it only says so.
	let not_given = if also.is_empty() {
		None
	} else {
		library
			.give_space(joined.space, also, wall_millis(), &mut UnwrapErr(SysRng))
			.err()
			.map(|error| error.to_string())
	};
	Ok(json!({
		"not_given": not_given,
		"space": joined.space.to_string(),
		"name": joined.name,
		"role": joined.role.as_text(),
		"person": joined.person.map(|(_, name)| name),
	}))
}

/// Makes a library, as [`crate::setup::create`] does, for someone whom another user invited
/// to a shared space, and joins the space with the `code` their device shows, checked
/// first, waiting a minute, or `seconds`, to find it. The library stays, unlocked, if
/// joining fails: the answer says why, in `failed`, or what was joined, in `joined`.
///
/// # Errors
///
/// Where the code is wrong, or the library cannot be made.
pub fn create_and_join(request: &mut Json) -> Result<Json, Failure> {
	let code = code_arg(request)?;
	let wait = join_wait(request);
	let folder = take(request, "folder");
	let passphrase = take(request, "passphrase");
	let protect = Protect::of(request, &passphrase);
	let names = (take(request, "user"), take(request, "device"));
	let mut library = create_library(&folder, (&names.0, &names.1), &protect)?;
	let_go();
	// Whatever happens as it joins, the library is made, and kept.
	let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		join_with(&mut library, &code, wait, &[])
	}));
	keep(library);
	Ok(match joined {
		Ok(Ok(joined)) => json!({"joined": joined}),
		Ok(Err(failure)) => json!({"failed": failure.answer()}),
		Err(_) => json!({"failed": {"error": "joining failed", "kind": "panic"}}),
	})
}
