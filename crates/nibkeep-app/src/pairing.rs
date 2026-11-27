//! Adding a device: the app shows a code, which the new device types to join, as `nib device
//! pair` does ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#104-pairing)).
//! An app that reads secrets may give them to a new device that asks; another never gives
//! them, and a new device that asks for them records the ask, for an app that reads them to
//! answer.
//!
//! The wait runs on a thread of its own, on the network borrowed from the program that
//! syncs this device, the app or another, which stops its endpoint meanwhile.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_node::{AskedFor, Closed, DEFAULT_PORT, NetworkOptions, NodeError, PairingCode};
use serde_json::{Value as Json, json};

use crate::held::{hold, waiting};
use crate::{Failure, clock, holding, locked};

/// How long adding a device waits to open the library, for a program that has it to itself.
const BORROW_OPEN_WAIT: Duration = Duration::from_secs(10);

/// How long adding a device waits for it: shorter than `nib device pair`, which nothing
/// else waits for, and never more than its ten minutes.
const PAIRING_WINDOW: Duration = Duration::from_mins(2);
const LONGEST_PAIRING: Duration = Duration::from_mins(10);

/// Adding a device: the code it types, what stops the wait, whether it gives the secrets,
/// which locking them stops, the thread that waits, the last try that failed before the
/// code matched, and once done, the device added or why not.
struct Pairing {
	code: String,
	stop: Arc<AtomicBool>,
	gives_secrets: bool,
	thread: Option<JoinHandle<()>>,
	failed_try: Option<Json>,
	done: Option<Result<Json, Json>>,
}

/// The device being added, or added last.
static PAIRING: Mutex<Option<Pairing>> = Mutex::new(None);

/// Changes the record of the wait for the device that types `code`, if it is still the one
/// kept: a wait never writes into another's.
fn update(code: &str, change: impl FnOnce(&mut Pairing)) {
	if let Some(pairing) = locked(&PAIRING)
		.as_mut()
		.filter(|pairing| pairing.code == code)
	{
		change(pairing);
	}
}

/// A try at a code that failed before the code matched, as the app shows it: whether a
/// device typed a wrong code, and how many tries the code has left.
#[must_use]
pub fn failed_try(error: &NodeError, left: usize) -> Json {
	json!({"wrong_code": error.is_wrong_code(), "tries_left": left})
}

/// Starts adding a device: makes a code, which the new device types to join, and waits for
/// it on a thread of its own, for two minutes, or `seconds` if the request says, up to ten.
/// The new device is a computer, which takes the spaces given to it later, or a `phone`,
/// which asks first; it gets the personal space and the `spaces` of this device's that the
/// request names, or by default, every space for a computer and none for a phone. With
/// `give_secrets`, in an app that reads them, the secrets too, to a device that asks for
/// them: the person chose to, and unlocked them. The library is opened only once the
/// device connects. Returns the code; [`pairing`] says how it goes.
///
/// # Errors
///
/// Where a device is being added already, the library is locked, the secrets to give are
/// locked, or this device may not add that one.
pub fn add_device(request: &Json) -> Result<Json, Failure> {
	let window = request["seconds"]
		.as_u64()
		.map_or(PAIRING_WINDOW, Duration::from_secs)
		.min(LONGEST_PAIRING);
	let device = nibkeep_node::NewDevice {
		phone: request["phone"].as_bool().unwrap_or(false),
		spaces: holding::spaces_arg(request, "spaces")?,
	};
	let give_secrets =
		crate::secrets().is_some() && request["give_secrets"].as_bool().unwrap_or(false);
	// A wait that was stopped ends first, so that the new one has the connections.
	let ending = {
		let mut pairing = locked(&PAIRING);
		match pairing.as_mut() {
			Some(now) if now.done.is_none() && !now.stop.load(Ordering::SeqCst) => {
				return Err(Failure::Request(
					"a device is being added already".to_owned(),
				));
			}
			Some(now) => now.thread.take(),
			None => None,
		}
	};
	if let Some(ending) = ending {
		let _ = ending.join();
	}
	// The library is opened once: to check that it may add devices, and for a closed
	// library of the pairing's own, without the secrets, unless it gives them, as the app's
	// may be locked or replaced meanwhile.
	let copy = {
		let held = hold();
		let Some(closed) = held.as_ref() else {
			return Err(Failure::Locked);
		};
		if give_secrets && !closed.secrets_kept() {
			return Err(Failure::SecretsLocked);
		}
		let library = waiting(|| closed.open())?;
		library.check_not_ai_agent("add devices")?;
		library.check_new_device(&device)?;
		let mut copy = library.close();
		if !give_secrets {
			copy.lock_secrets();
		}
		copy
	};
	let code = PairingCode::generate(&mut UnwrapErr(SysRng));
	let stop = Arc::new(AtomicBool::new(false));
	let shown = code.to_string();
	let mut pairing = locked(&PAIRING);
	// Another call that started a wait meanwhile keeps it.
	if pairing
		.as_ref()
		.is_some_and(|now| now.thread.is_some() && now.done.is_none())
	{
		return Err(Failure::Request(
			"a device is being added already".to_owned(),
		));
	}
	let waiting = stop.clone();
	let own = shown.clone();
	let thread = std::thread::spawn(move || {
		let waiting = nibkeep_node::Waiting {
			showing: nibkeep_node::Showing {
				timeout: window,
				wrong_try: &|error, left| {
					update(&own, |pairing| {
						pairing.failed_try = Some(failed_try(error, left));
					});
				},
			},
			stop: &waiting,
			give_secrets,
			device: &device,
		};
		let done = wait_for_device(copy, &code, &waiting);
		update(&own, |pairing| pairing.done = Some(done));
	});
	*pairing = Some(Pairing {
		code: shown.clone(),
		stop,
		gives_secrets: give_secrets,
		thread: Some(thread),
		failed_try: None,
		done: None,
	});
	Ok(json!({"code": shown, "seconds": window.as_secs()}))
}

/// Waits for a new device to pair with `code`, on the network borrowed from the program
/// that syncs this device; then gives the network back, before the app hears that the wait
/// is over. What it added, or why not: the device and its app, and whether it asked for the
/// secrets, in `asked_for_secrets`, and in `secrets`, `asked`, or `given` if it was given
/// them.
fn wait_for_device(
	copy: Closed,
	code: &PairingCode,
	waiting: &nibkeep_node::Waiting<'_>,
) -> Result<Json, Json> {
	let paired = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let now = clock();
		let mut rng = UnwrapErr(SysRng);
		let (borrowed, network) = {
			let library = nibkeep_node::retry_in_use(BORROW_OPEN_WAIT, || copy.open())?;
			// Until the person gives up, or locks the library or the secrets that the copy
			// may hold.
			let borrowed = nibkeep_node::borrow_network(&library, waiting.stop, UnwrapErr(SysRng))?;
			let network = library.network(NetworkOptions {
				discovery: true,
				port: Some(DEFAULT_PORT),
				accept: true,
			})?;
			(borrowed, network)
		};
		let paired = nibkeep_node::pair_closed(&copy, &network, code, waiting, &now, &mut rng);
		drop(network);
		drop(borrowed);
		paired
	}));
	// The copy, with the secrets' key if it gave them, goes first.
	drop(copy);
	match paired {
		Ok(Ok(paired)) => Ok(json!({
			"device": paired.device,
			"app": paired.app,
			"asked_for_secrets": paired.asked_for != AskedFor::Nothing,
			"secrets": match paired.asked_for {
				AskedFor::Nothing => None,
				AskedFor::Secrets => Some("asked"),
				AskedFor::SecretsGiven => Some("given"),
			},
		})),
		Ok(Err(error)) => Err(Failure::from(error).answer()),
		Err(_) => Err(json!({"error": "adding the device failed", "kind": "panic"})),
	}
}

/// Stops waiting for a device to add, as when the person gives up, or the library is
/// locked; the wait ends within a quarter of a second.
pub fn stop_adding() {
	if let Some(pairing) = locked(&PAIRING).as_ref() {
		pairing.stop.store(true, Ordering::SeqCst);
	}
}

/// Stops waiting for a device to add that would be given the secrets, which are locked:
/// its copy of their key goes with it.
pub fn stop_giving() {
	if let Some(pairing) = locked(&PAIRING)
		.as_ref()
		.filter(|pairing| pairing.gives_secrets)
	{
		pairing.stop.store(true, Ordering::SeqCst);
	}
}

/// How adding a device goes: the code; while it waits, the last try that failed before the
/// code matched, if one did; and once done, the device added, or why not.
#[must_use]
pub fn pairing() -> Json {
	match locked(&PAIRING).as_ref() {
		None => json!({"pairing": false}),
		Some(Pairing {
			code,
			failed_try,
			done: None,
			..
		}) => json!({"pairing": true, "code": code, "failed_try": failed_try}),
		Some(Pairing {
			code,
			done: Some(Ok(added)),
			..
		}) => json!({"pairing": false, "code": code, "added": added}),
		Some(Pairing {
			code,
			done: Some(Err(failed)),
			..
		}) => json!({"pairing": false, "code": code, "failed": failed}),
	}
}
