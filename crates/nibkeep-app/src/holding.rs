//! The spaces on this device: every space of the person's, whether this device holds it or
//! not, with the other devices of theirs that hold each; taking a space given to this
//! device, asking for one it was not given, and giving one to another device of theirs that
//! asks for it; dropping one, with what the drop waits for; whether this device takes the
//! spaces given to it later, or asks first; and the person's other devices, to choose among
//! as they make or join a space. As `nib space list`, `take`, `allow` and `drop`, and `nib
//! device mark --new-spaces`, do
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).

use std::collections::BTreeSet;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_node::{
	DropStage, DropWait, Dropping, HeldElsewhere, Here, KnownSpace, Library, Replica, Taking,
};
use serde_json::{Value as Json, json};

use crate::{Failure, wall_millis};

/// What this device did with a space, as the app tells it apart.
fn here_text(here: Here) -> &'static str {
	match here {
		Here::Held => "held",
		Here::Given => "given",
		Here::Asked => "asked",
		Here::Declined => "declined",
		Here::Elsewhere => "elsewhere",
	}
}

/// What a drop waits for, as `nib space drop --json` says it.
fn wait_json(wait: &DropWait) -> Json {
	match wait {
		DropWait::OwnChanges { changes, more } => json!({
			"waits_for": "own_changes",
			"changes": changes,
			"more": more,
		}),
		DropWait::ChangeCopies {
			disks,
			away,
			archived,
			needs,
		} => json!({
			"waits_for": "change_copies",
			"disks": disks,
			"away": away,
			"archived": archived,
			"needs": needs.copies,
			"needs_away": needs.away,
			"needs_archived": needs.archived,
		}),
		DropWait::Files(files) => json!({"waits_for": "files", "files": files}),
		DropWait::Secrets => json!({"waits_for": "secrets"}),
	}
}

/// How far a drop of a space on this device has come: `waiting`, with what it waits for,
/// `dropped`, until a device of the person's that holds the space learns so, or `deleted`.
pub fn dropping_json(dropping: &Dropping) -> Json {
	json!({
		"stage": match dropping.stage {
			DropStage::Waiting => "waiting",
			DropStage::Dropped => "dropped",
			DropStage::Deleted => "deleted",
		},
		"waits": dropping.waits.iter().map(wait_json).collect::<Vec<_>>(),
	})
}

/// A replica of the person's, as the person tells it: its device's name, with its app's
/// where the device has another, or its app's alone, or its ID.
fn replica_label(device: Option<&str>, app: Option<&str>, id: EntityId) -> String {
	match (device, app) {
		(Some(device), _) => device.to_owned(),
		(None, Some(app)) => app.to_owned(),
		(None, None) => id.to_string(),
	}
}

/// A replica of the person's, as they choose among replicas: its app on its device, as
/// two apps on one device each hold the spaces chosen for them.
fn app_label(device: Option<&str>, app: Option<&str>, id: EntityId) -> String {
	match (device, app) {
		(Some(device), Some(app)) => format!("{app} on {device}"),
		_ => replica_label(device, app, id),
	}
}

/// The names of the person's devices that hold a space, besides this device, once each:
/// two apps on one device hold one copy.
fn other_holders(known: &KnownSpace, replicas: &[Replica]) -> Vec<String> {
	let this_device = replicas
		.iter()
		.find(|replica| replica.this)
		.and_then(|replica| replica.device);
	let mut seen = BTreeSet::new();
	let mut names = Vec::new();
	for holder in &known.holders {
		let device = replicas
			.iter()
			.find(|replica| replica.id == holder.replica)
			.and_then(|replica| replica.device);
		if holder.this || (device.is_some() && device == this_device) {
			continue;
		}
		let name = replica_label(
			holder.device.as_deref(),
			holder.app.as_deref(),
			holder.replica,
		);
		if seen.insert(device.map_or_else(|| holder.replica.to_string(), |id| id.to_string())) {
			names.push(name);
		}
	}
	names
}

/// The person's other replicas, to choose among as they make or join a space, or add a
/// device: each that is not this one, revoked, deleted or an AI agent's, by its ID, with
/// its name, its app's name, and whether it takes the spaces given to it later, so gets a
/// new space anyway.
fn other_devices(library: &Library, replicas: &[Replica]) -> Result<Vec<Json>, Failure> {
	let marks = library.new_space_marks()?;
	Ok(replicas
		.iter()
		.filter(|replica| {
			!replica.this
				&& !replica.revoked
				&& !replica.deleted
				&& !replica.ai_agent
				&& replica.shared.is_none()
		})
		.map(|replica| {
			json!({
				"id": replica.id.to_string(),
				"name": app_label(replica.device_name.as_deref(), replica.name.as_deref(), replica.id),
				"app": replica.name,
				"takes_new_spaces": marks.get(&replica.id).copied().unwrap_or(true),
			})
		})
		.collect())
}

/// Every space of the person's but the personal one, as the personal space knows them, by
/// name: what this device did with each (`held`, `given`, `asked`, `declined` or
/// `elsewhere`), the other devices of theirs that hold it, whether it is a group, and, while
/// this device drops it, how far the drop has come; then the spaces this device holds that
/// the person left or was removed from, read only here, as `left`. The requests of the
/// person's other devices for the spaces this device holds; whether this device takes the
/// spaces given to it later; and the person's other devices, to choose among as they make
/// or join a space, or add a device: each that is not this one, revoked, deleted or an AI
/// agent's, with its name, its app's, and whether it takes the spaces given to it later.
/// How far a drop has come, and the name of a space left, are left out where they cannot be
/// read, so that the other spaces show.
///
/// # Errors
///
/// Where the personal space does not read.
pub fn spaces_here(library: &Library) -> Result<Json, Failure> {
	let replicas = library.replicas()?;
	let known = library.known_spaces()?;
	let progress = |space| {
		library
			.drop_progress(space)
			.ok()
			.flatten()
			.as_ref()
			.map(dropping_json)
	};
	let mut rows = Vec::new();
	for space in &known {
		let dropping = if space.dropping {
			progress(space.space)
		} else {
			None
		};
		let holders = other_holders(space, &replicas);
		rows.push(json!({
			"id": space.space.to_string(),
			"name": space.name,
			"here": here_text(space.here),
			"holders": holders,
			"group": library.is_group(space.space).unwrap_or(false),
			"dropping": dropping,
		}));
	}
	// Left or removed: the personal space knows them no longer, but this device holds them.
	let listed: BTreeSet<SpaceId> = known.iter().map(|space| space.space).collect();
	for space in library.spaces() {
		if space == library.personal_space() || listed.contains(&space) {
			continue;
		}
		if library.has_left(space).unwrap_or(false) {
			rows.push(json!({
				"id": space.to_string(),
				"name": library.space_name(space).ok().flatten(),
				"here": "left",
				"holders": [],
				"group": library.is_group(space).unwrap_or(false),
				"dropping": progress(space),
			}));
		}
	}
	let requests: Vec<Json> = library
		.space_requests()?
		.into_iter()
		.map(|request| {
			json!({
				"space": request.space.to_string(),
				"name": request.name,
				"replica": request.replica.to_string(),
				"device": app_label(request.device.as_deref(), request.app.as_deref(), request.replica),
				"back": request.back,
			})
		})
		.collect();
	Ok(json!({
		"spaces": rows,
		"requests": requests,
		"takes_new_spaces": library.takes_new_spaces()?,
		"devices": other_devices(library, &replicas)?,
	}))
}

/// The space that the request names in `name`, by its ID.
fn space_arg(request: &Json, name: &str) -> Result<SpaceId, Failure> {
	let text = request[name].as_str().unwrap_or_default();
	text.parse()
		.map_err(|_| Failure::Request(format!("no space {text}")))
}

/// The replicas of the person's that the request names in `name`, by their IDs: none if
/// it names none.
///
/// # Errors
///
/// A [`Failure::Request`] for a text that is no replica's ID.
pub fn replicas_arg(request: &Json, name: &str) -> Result<Vec<EntityId>, Failure> {
	request[name]
		.as_array()
		.map_or(&[][..], Vec::as_slice)
		.iter()
		.map(|id| {
			let text = id.as_str().unwrap_or_default();
			text.parse()
				.map_err(|_| Failure::Request(format!("no device {text}")))
		})
		.collect()
}

/// The spaces that the request names in `name`, by their IDs: `None` if it names no list.
///
/// # Errors
///
/// A [`Failure::Request`] for a text that is no space's ID.
pub fn spaces_arg(request: &Json, name: &str) -> Result<Option<Vec<SpaceId>>, Failure> {
	let Some(listed) = request[name].as_array() else {
		return Ok(None);
	};
	listed
		.iter()
		.map(|id| {
			let text = id.as_str().unwrap_or_default();
			text.parse()
				.map_err(|_| Failure::Request(format!("no space {text}")))
		})
		.collect::<Result<Vec<_>, _>>()
		.map(Some)
}

/// Takes the space that the request names on this device: one given to it, or that it
/// drops still, at once, as `took`; one it was not given, or dropped, by asking for it, as
/// `asked`, which the person approves on a device that holds it.
///
/// # Errors
///
/// Where the request names no space, or the engine refuses.
pub fn take(library: &mut Library, request: &Json) -> Result<Json, Failure> {
	let space = space_arg(request, "space")?;
	let taking = library.take_space(space, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(json!({"taking": match taking {
		Taking::Took => "took",
		Taking::Asked => "asked",
	}}))
}

/// Gives the space that the request names to the device of the person's that asked for it,
/// by its replica's ID.
///
/// # Errors
///
/// Where the request names no space or device, or the engine refuses.
pub fn allow(library: &mut Library, request: &Json) -> Result<Json, Failure> {
	let space = space_arg(request, "space")?;
	let text = request["replica"].as_str().unwrap_or_default();
	let replica = text
		.parse()
		.map_err(|_| Failure::Request(format!("no device {text}")))?;
	library.allow_space(space, replica, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(json!({}))
}

/// Drops the space that the request names from this device, or tells how far its drop has
/// come, as [`dropping_json`] says; refused, with the reasons, where it cannot be dropped
/// now.
///
/// # Errors
///
/// Where the request names no space, or the space cannot be dropped now.
pub fn drop_space(library: &mut Library, request: &Json) -> Result<Json, Failure> {
	let space = space_arg(request, "space")?;
	let dropping = library.drop_space(space, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(dropping_json(&dropping))
}

/// Says, on this device, whether it takes the spaces given to it later, or asks first.
///
/// # Errors
///
/// Where the request does not say, or the engine fails.
pub fn take_new_spaces(library: &mut Library, request: &Json) -> Result<Json, Failure> {
	let takes = request["takes"]
		.as_bool()
		.ok_or_else(|| Failure::Request("say whether it takes them".to_owned()))?;
	library.set_takes_new_spaces(takes, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(json!({}))
}

/// Spaces that this device does not hold, each by its name, with the other devices of the
/// person's that hold it, by name: none where only the devices revoked did, or none of
/// the person's devices does.
#[must_use]
pub fn out_of_reach_json(spaces: &[HeldElsewhere]) -> Json {
	json!(
		spaces
			.iter()
			.map(|each| json!({
				"space": each.name.clone().unwrap_or_else(|| each.space.to_string()),
				"holders": each
					.holders
					.iter()
					.map(|holder| replica_label(holder.device.as_deref(), holder.app.as_deref(), holder.replica))
					.collect::<Vec<_>>(),
			}))
			.collect::<Vec<_>>()
	)
}
