//! Joining a space that someone shares, with the code their device shows, as `nib space
//! join` does ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#137-invitations)):
//! with the library unlocked, or making a library first, for someone who has none yet.
//!
//! A join borrows the network from the program that syncs this device, as adding a device
//! does. Locking the library, or another library taking its place, stops the joins under
//! way that have not started looking for the inviting device yet: those that wait for the
//! network to be lent, or open the library first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::EntityId;
use nibkeep_node::{Library, NetworkOptions, NodeError, PairingCode};
use serde_json::{Value as Json, json};

use crate::held::{Mark, hold, keep, let_go, waiting};
use crate::setup::{Protect, create_library};
use crate::{Failure, holding, locked, take, wall_millis};

/// How long joining waits to find the device that invites, unless the request says less.
const JOIN_WAIT: Duration = Duration::from_mins(1);

/// How long a join waits to open the library, for a program that has it to itself.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// The stop flags of the joins under way, which [`stop`] sets.
static JOINS: Mutex<Vec<Arc<AtomicBool>>> = Mutex::new(Vec::new());

/// A join under way, from its start until it ends: its stop flag, of its own, which only a
/// [`stop`] made meanwhile sets.
struct Join {
	stop: Arc<AtomicBool>,
}

impl Join {
	/// A join that starts now.
	fn start() -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		locked(&JOINS).push(Arc::clone(&stop));
		Self { stop }
	}

	/// A join that starts now, stopped already if the library was locked since `since`.
	fn since(since: &Mark) -> Self {
		let join = Self::start();
		if since.library_locked() {
			join.stop.store(true, Ordering::SeqCst);
		}
		join
	}

	/// Whether the join was stopped.
	fn stopped(&self) -> bool {
		self.stop.load(Ordering::SeqCst)
	}
}

impl Drop for Join {
	fn drop(&mut self) {
		locked(&JOINS).retain(|stop| !Arc::ptr_eq(stop, &self.stop));
	}
}

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

/// Stops the joins under way that have not started looking for the inviting device yet, as
/// the library is locked, or another takes its place: a join that starts after goes on.
pub fn stop() {
	for stop in locked(&JOINS).iter() {
		stop.store(true, Ordering::SeqCst);
	}
}

/// Joins a shared space with the request's `code`, waiting a minute, or `seconds`, to find
/// the device that invites this one, and gives it to the person's devices that the request
/// names in `also`, besides those that take new spaces: the space, its name, the role the
/// invitation gives, the person it names this user, if any, and why it was not given to
/// those devices, if it was not. It works on a library of its own, without the secrets,
/// which joining never needs, so that the app's other calls go on while the person who
/// invites decides.
///
/// # Errors
///
/// Where the code is wrong, the library is locked, the join is stopped ([`stop`]), or
/// joining fails.
pub fn join_space(request: &Json) -> Result<Json, Failure> {
	let code = code_arg(request)?;
	// Under way from here, so that a lock as it opens the library stops it.
	let join = Join::start();
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
	join_with(&mut library, &join, &code, join_wait(request), &also)
}

/// Joins a shared space with `code` on `library`, as [`join_space`] says, waiting up to
/// `wait` to find the inviting device, unless `join` is stopped first.
fn join_with(
	library: &mut Library,
	join: &Join,
	code: &PairingCode,
	wait: Duration,
	also: &[EntityId],
) -> Result<Json, Failure> {
	let borrowed = nibkeep_node::borrow_network(library, &join.stop, UnwrapErr(SysRng))?;
	// Stopped before the network was lent, as borrowing stops while it waits.
	if join.stopped() {
		return Err(NodeError::PairingExpired.into());
	}
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
/// joining fails: the answer says why, in `failed`, or what was joined, in `joined`. If the
/// library was locked meanwhile, it stays, locked, and its join stops if it has not started
/// looking for the inviting device yet.
///
/// # Errors
///
/// Where the code is wrong, or the library cannot be made; [`Failure::Locked`] if it was
/// locked meanwhile.
pub fn create_and_join(request: &mut Json) -> Result<Json, Failure> {
	let since = Mark::now();
	let code = code_arg(request)?;
	let wait = join_wait(request);
	let folder = take(request, "folder");
	let passphrase = take(request, "passphrase");
	let protect = Protect::of(request, &passphrase);
	let names = (take(request, "user"), take(request, "device"));
	let mut library = create_library(&folder, (&names.0, &names.1), &protect)?;
	// The joins of the library held before stop with it; this one starts after.
	let_go();
	let join = Join::since(&since);
	// Whatever happens as it joins, the library is made, and kept unless it was locked.
	let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		join_with(&mut library, &join, &code, wait, &[])
	}));
	drop(join);
	keep(library, since)?;
	Ok(match joined {
		Ok(Ok(joined)) => json!({"joined": joined}),
		Ok(Err(failure)) => json!({"failed": failure.answer()}),
		Err(_) => json!({"failed": {"error": "joining failed", "kind": "panic"}}),
	})
}

#[cfg(test)]
mod tests {
	//! The joins' stop flags, which only a program that keeps the network it lends would
	//! show from outside.

	use super::{JOINS, Join, stop};
	use crate::held::{Mark, lock};
	use crate::locked;

	#[test]
	fn joining_a_stop_reaches_the_joins_under_way_and_none_that_start_after() {
		let one = Join::start();
		let two = Join::start();
		stop();
		assert!(one.stopped() && two.stopped(), "each join under way");
		let three = Join::start();
		assert!(!three.stopped(), "a join that starts after goes on");
		assert!(one.stopped(), "and leaves the others stopped");
		drop(one);
		drop(two);
		assert_eq!(locked(&JOINS).len(), 1, "joins that end leave the list");
		// A lock since the work began stops a join that starts after it.
		let since = Mark::now();
		lock();
		assert!(Join::since(&since).stopped());
		assert!(!Join::since(&Mark::now()).stopped());
		drop(three);
		assert!(locked(&JOINS).is_empty());
	}
}
