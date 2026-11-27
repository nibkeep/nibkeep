//! Syncing with the person's other devices
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#105-the-sync-role)):
//! while the library is unlocked, and on other devices, a thread of the app holds its
//! replica's sync role, or waits for the program that holds it, such as `nib sync --serve`
//! or another app, to let go; whichever holds it syncs for all of them, as changes come and
//! devices appear, and records what it did. Syncing now asks the holder, and reads what it
//! recorded.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::EntityId;
use nibkeep_node::{Background, BackgroundOptions, DEFAULT_PORT, NetworkOptions, NodeError};
use serde_json::{Value as Json, json};

use crate::held::{hold, waiting, with_open};
use crate::{Failure, app, clock, locked};

/// How long syncing now waits for the program that syncs this device: a sync takes a while.
const ASK_WAIT: Duration = Duration::from_mins(2);

/// The thread that holds the sync role, while it runs.
static BACKGROUND: Mutex<Option<Background>> = Mutex::new(None);

/// Starts holding the sync role of the library unlocked, or waiting for it, unless it does
/// already, or the library is on no other device: whether it does.
///
/// # Errors
///
/// [`Failure::Locked`] if the library is locked, and where it does not open.
pub fn serve() -> Result<Json, Failure> {
	// The library first, then the thread, as everywhere.
	let held = hold();
	let Some(closed) = held.as_ref() else {
		return Err(Failure::Locked);
	};
	let mut running = locked(&BACKGROUND);
	if running
		.as_ref()
		.is_some_and(|running| running.directory() == closed.directory() && !running.ended())
	{
		return Ok(json!({"serving": true}));
	}
	if waiting(|| closed.open())?.peers()?.is_empty() {
		*running = None;
		return Ok(json!({"serving": false}));
	}
	let options = BackgroundOptions::new(
		app().name,
		NetworkOptions {
			discovery: true,
			port: Some(DEFAULT_PORT),
			accept: true,
		},
	);
	// Without the secrets, which syncing never needs.
	*running = Some(Background::start(
		closed.without_secrets(),
		options,
		clock(),
		|| UnwrapErr(SysRng),
	));
	Ok(json!({"serving": true}))
}

/// Stops holding the sync role, as the library is locked, or the app leaves a phone's
/// screen: the thread lets go once the syncs under way end.
pub fn stop_serving() {
	*locked(&BACKGROUND) = None;
}

/// Lets go of the library whose directory is `directory`, or is in it: it is held unlocked
/// no more, and the thread that syncs it stops, and is waited for, once the syncs under way
/// end, so that nothing of the app's writes into it after.
pub fn let_go_of(directory: &Path) {
	// The library first, then the thread, as everywhere.
	let mut held = hold();
	if held
		.as_ref()
		.is_some_and(|closed| closed.directory().starts_with(directory))
	{
		*held = None;
	}
	let mut running = locked(&BACKGROUND);
	let syncing = if running
		.as_ref()
		.is_some_and(|running| running.directory().starts_with(directory))
	{
		running.take()
	} else {
		None
	};
	drop(running);
	drop(held);
	if let Some(syncing) = syncing {
		syncing.wait();
	}
}

/// Whether the app holds the sync role or waits for it, and why it failed if it did; which
/// program holds it; how many other replicas the library has, as the app syncs only then;
/// and how many changes the library keeps, which grows as changes come, for the app to show
/// it anew.
///
/// # Errors
///
/// [`Failure::Locked`] if the library is locked, and where its records do not read.
pub fn serving() -> Result<Json, Failure> {
	let state = locked(&BACKGROUND)
		.as_ref()
		.map(|running| (!running.ended(), running.state()));
	let (holder, peers, changes) = with_open(|library| {
		let holder = library.sync_records(UnwrapErr(SysRng))?.holder()?;
		Ok((holder, library.peers()?.len(), library.changes_kept()?))
	})?;
	Ok(json!({
		"serving": state.as_ref().is_some_and(|(running, _)| *running),
		"holding": state.as_ref().is_some_and(|(_, state)| state.holding),
		"failed": state.and_then(|(_, state)| state.failed),
		"holder": holder.map(|holder| holder.program),
		"peers": peers,
		"changes": changes,
	}))
}

/// Asks the program that syncs this device to sync now, with the device whose replica the
/// request's `id` names, or with all it finds, and says what each sync did, as it recorded
/// it.
///
/// # Errors
///
/// [`Failure::NotAnswered`] if the program did not sync in time; where the library is on no
/// other device, is locked, or its records do not read.
pub fn sync(request: &Json) -> Result<Json, Failure> {
	let replica = match request["id"].as_str() {
		Some(id) => Some(
			id.parse::<EntityId>()
				.map_err(|_| Failure::Request(format!("{id} is not a device's ID")))?,
		),
		None => None,
	};
	let (records, peers) = with_open(|library| {
		let peers = library.peers()?;
		if peers.is_empty() {
			return Err(NodeError::NoOtherReplicas.into());
		}
		Ok((library.sync_records(UnwrapErr(SysRng))?, peers))
	})?;
	// The app holds the role, or waits for it, from now on, as when a device was added
	// elsewhere since the library was unlocked.
	serve()?;
	let now = clock();
	let Some(asked) = nibkeep_node::ask_to_sync(&records, replica, &now, ASK_WAIT)? else {
		return Err(Failure::NotAnswered);
	};
	let fresh = records.synced_since(asked)?;
	let holder = records.holder()?.map(|holder| holder.program);
	let rows: Vec<Json> = peers
		.iter()
		.filter(|peer| replica.is_none_or(|replica| replica == peer.id))
		.map(|peer| {
			let fresh = fresh.get(&peer.id);
			json!({
				"id": peer.id.to_string(),
				"device": peer.device_name,
				"app": peer.name,
				"found": fresh.is_some(),
				"error": fresh.and_then(|record| record.error.clone()),
				"received": fresh.map_or(0, |record| record.received),
				"sent": fresh.map_or(0, |record| record.sent),
			})
		})
		.collect();
	Ok(json!({"synced": rows, "holder": holder}))
}
