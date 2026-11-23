//! What commands print: text for people, or JSON for scripts.

use std::io::Write;

use nibkeep_base::EntityId;
use nibkeep_model::{FieldName, Labels, Languages, Multiplicity, TypeDef, Types, Value};
use nibkeep_node::{
	AddedFile, AddedSecretFile, CheckedOut, Checkout, Choice, ChosenBy, CopyState, EditsRecorded,
	Ended, EntityView, ExportCheck, ExportKind, ExportProblem, Exported, History, Imported, Kept,
	LanguageSettings, Linking, Links, Listed, More, Needs, PastValue, Policy, PolicyCheck, Problem,
	Purge, Question, Replica, Report, Rule, Searched, ShownValue, SpaceSynced, Status, Synced,
};
use nibkeep_node::{
	AskedFor, CheckStatus, CopyHeld, Damaged, DeviceMarks, DeviceRole, Disk, DiskReplica, FileCopy,
	Part, ReplicaRecord, at_login,
};
use nibkeep_time::Presence;
use serde_json::json;

use crate::types::{kind_json, kind_text, multiplicity_json, multiplicity_text};
use crate::{CliError, Firewall};

/// Prints what a library holds, and its folder; and how many checkouts this device has,
/// with those whose copies were edited or are gone.
pub(crate) fn status(
	(status, checkouts, thumbnails): (
		&Status,
		&[Checkout],
		&std::collections::BTreeMap<nibkeep_base::SpaceId, nibkeep_node::ThumbnailCounts>,
	),
	checks: Checks<'_>,
	folder: &std::path::Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let spaces: Vec<serde_json::Value> = status
			.spaces
			.iter()
			.map(|space| {
				let counts = thumbnails.get(&space.id).copied().unwrap_or_default();
				json!({
					"id": space.id.to_string(),
					"name": space.name,
					"entities": space.entities,
					"changes": space.changes,
					"pictures": counts.pictures,
					"thumbnails": counts.with,
				})
			})
			.collect();
		let checkouts: Vec<serde_json::Value> = checkouts
			.iter()
			.map(|checkout| {
				json!({
					"id": checkout.id.to_string(),
					"file": checkout.file.to_string(),
					"path": checkout.path.display().to_string(),
					"content": checkout.content.map(|content| content.to_string()),
					"copy": checkout.copy.as_str(),
				})
			})
			.collect();
		let value = json!({
			"directory": folder.display().to_string(),
			"user": { "id": status.user.id.to_string(), "name": status.user.name },
			"device": { "id": status.device.id.to_string(), "name": status.device.name },
			"replica": { "id": status.replica.id.to_string(), "name": status.replica.name },
			"spaces": spaces,
			"checkouts": checkouts,
			"checks": checks_json(&checks),
		});
		return print_json(&value, output);
	}
	writeln!(
		output,
		"{}, on {}, in {}",
		status.user.name.as_deref().unwrap_or("Someone"),
		status.device.name.as_deref().unwrap_or("a device"),
		folder.display()
	)?;
	for space in &status.spaces {
		// Pictures and videos, as their media types say, and how many have a thumbnail.
		let pictures = match thumbnails.get(&space.id) {
			Some(counts) if counts.pictures > 0 => format!(
				", {}, {} with a thumbnail",
				count(counts.pictures, "picture or video", "pictures and videos"),
				counts.with
			),
			_ => String::new(),
		};
		writeln!(
			output,
			"{}: {}, {}{pictures}",
			space.name.as_deref().unwrap_or("A space"),
			count(space.entities, "entity", "entities"),
			count(space.changes, "change", "changes"),
		)?;
	}
	checks_text(&checks, output)?;
	if checkouts.is_empty() {
		return Ok(());
	}
	let of = |state: CopyState| {
		checkouts
			.iter()
			.filter(move |checkout| checkout.copy == state)
	};
	let mut parts = vec![checkouts.len().to_string()];
	for state in [CopyState::Edited, CopyState::Gone] {
		let counted = of(state).count();
		if counted > 0 {
			parts.push(format!("{counted} {}", state.as_str()));
		}
	}
	writeln!(output, "Checkouts on this device: {}", parts.join(", "))?;
	for state in [CopyState::Edited, CopyState::Gone] {
		for checkout in of(state) {
			writeln!(
				output,
				"{:6}  {}  {}",
				state.as_str(),
				short(checkout.file),
				checkout.path.display()
			)?;
		}
	}
	Ok(())
}

/// Where this replica's checks stand, for `nib status`, with the names of files and spaces,
/// and the time zone to show times in.
#[derive(Clone, Copy)]
pub(crate) struct Checks<'a> {
	/// Where they stand, if the records could be read.
	pub(crate) status: Option<&'a CheckStatus>,
	/// The name of a file of a space, and of a space.
	pub(crate) name_of: &'a dyn Fn(nibkeep_base::SpaceId, Option<EntityId>) -> Option<String>,
	pub(crate) zone: &'a jiff::tz::TimeZone,
}

/// How much of one kind of copies a round read, for people: "43% of 120 MB".
fn part_text(part: &Part) -> String {
	let percent = if part.done || part.total == 0 {
		100
	} else {
		(u128::from(part.read) * 100 / u128::from(part.total)).min(100)
	};
	format!("{percent}% of {}", rough_size(part.total))
}

/// A size for people, to a tenth of its unit: `1.2 GB`.
fn rough_size(bytes: u64) -> String {
	for (unit, size) in [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("kB", 1e3)] {
		#[expect(clippy::cast_precision_loss, reason = "a size for people")]
		let scaled = bytes as f64 / size;
		if scaled >= 1.0 {
			return format!("{scaled:.1} {unit}");
		}
	}
	count(
		usize::try_from(bytes).unwrap_or(usize::MAX),
		"byte",
		"bytes",
	)
}

/// Prints where this replica's checks stand, and what they found damaged.
fn checks_text(checks: &Checks<'_>, output: &mut dyn Write) -> Result<(), CliError> {
	let Some(status) = checks.status else {
		return Ok(());
	};
	match &status.round {
		// Nothing to check: no round to tell of, of 0 bytes.
		_ if !status.holds => {}
		None => writeln!(
			output,
			"Checks: none yet. The program that syncs this device checks its copies: nib sync --serve, or syncing from login."
		)?,
		Some(round) => writeln!(
			output,
			"Checks: this round began {}, and read {} of sealed copies and {} of originals",
			moment_text(round.began, checks.zone),
			part_text(&round.blobs),
			part_text(&round.plain)
		)?,
	}
	for damaged in &status.damaged {
		let named: Vec<String> = damaged
			.files
			.iter()
			.map(|file| {
				(checks.name_of)(damaged.space, Some(*file)).unwrap_or_else(|| short(*file))
			})
			.collect();
		let what = if named.is_empty() {
			"a file".to_owned()
		} else {
			named.join(", ")
		};
		let space =
			(checks.name_of)(damaged.space, None).map_or_default(|space| format!(" ({space})"));
		let found = moment_text(damaged.found, checks.zone);
		let line = match &damaged.copy {
			Damaged::Sealed { replaced: true, .. } => {
				format!(
					"{what}{space}: its sealed copy was found damaged {found}, and an intact copy is in place again"
				)
			}
			Damaged::Sealed {
				replaced: false, ..
			} => format!(
				"{what}{space}: its sealed copy is damaged, found {found}, and waits for an intact copy from another device"
			),
			Damaged::Plain { path } => format!(
				"{what}{space}: {path} no longer holds its content, though its size and time are unchanged, found {found}: it may be damaged, and NibKeep leaves it as it is"
			),
		};
		writeln!(output, "  {line}")?;
	}
	Ok(())
}

/// Where this replica's checks stand, in JSON.
fn checks_json(checks: &Checks<'_>) -> serde_json::Value {
	let Some(status) = checks.status else {
		return serde_json::Value::Null;
	};
	let part = |part: &Part| {
		json!({
			"total": part.total,
			"read": part.read,
			"done": part.done,
		})
	};
	json!({
		"holds": status.holds,
		"round": status.round.as_ref().map(|round| json!({
			"began": moment_json(round.began),
			"sealed": part(&round.blobs),
			"originals": part(&round.plain),
		})),
		"last": status.last.map(moment_json),
		"damaged": status.damaged.iter().map(|damaged| {
			let mut value = json!({
				"space": damaged.space.to_string(),
				"files": damaged.files.iter().map(ToString::to_string).collect::<Vec<_>>(),
				"found": moment_json(damaged.found),
			});
			match &damaged.copy {
				Damaged::Sealed { sealed, replaced } => {
					value["sealed"] = json!(sealed.to_string());
					value["replaced"] = json!(replaced);
				}
				Damaged::Plain { path } => value["path"] = json!(path),
			}
			value
		}).collect::<Vec<_>>(),
	})
}

/// The names of the devices that hold a space, as the person's spaces are known: "this
/// device" for this one.
fn holder_names(known: &[nibkeep_node::KnownSpace], space: nibkeep_base::SpaceId) -> Vec<String> {
	known
		.iter()
		.find(|known| known.space == space)
		.map_or_else(Vec::new, |known| {
			known.holders.iter().map(holder_name).collect()
		})
}

/// The name of a device that holds a space: "this device" for this one, else its device's
/// name, or its replica's short ID.
fn holder_name(holder: &nibkeep_node::SpaceHolder) -> String {
	if holder.this {
		"this device".to_owned()
	} else {
		holder
			.device
			.clone()
			.unwrap_or_else(|| short(holder.replica))
	}
}

/// The names of the devices that hold a space that this device does not hold.
fn elsewhere_holders(each: &nibkeep_node::HeldElsewhere) -> Vec<String> {
	each.holders.iter().map(holder_name).collect()
}

/// Spaces that this device does not hold, in JSON: each with its name and the devices that
/// hold it.
fn elsewhere_json(elsewhere: &[nibkeep_node::HeldElsewhere]) -> serde_json::Value {
	json!(
		elsewhere
			.iter()
			.map(|each| json!({
				"space": each.space.to_string(),
				"name": each.name,
				"holders": elsewhere_holders(each),
			}))
			.collect::<Vec<_>>()
	)
}

/// The name of a space that this device does not hold, shown safely, or its ID.
fn elsewhere_name(each: &nibkeep_node::HeldElsewhere) -> String {
	each.name
		.as_deref()
		.map_or_else(|| each.space.to_string(), shown)
}

/// Spaces that this device does not hold and other devices do, each with those devices, as
/// "Project (on the desktop and the laptop)" reads without the articles.
fn elsewhere_with_holders(elsewhere: &[&nibkeep_node::HeldElsewhere]) -> String {
	let named: Vec<String> = elsewhere
		.iter()
		.map(|each| {
			let holders: Vec<String> = elsewhere_holders(each)
				.iter()
				.map(|name| shown(name))
				.collect();
			format!("{} (on {})", elsewhere_name(each), named_list(&holders))
		})
		.collect();
	named_list(&named)
}

/// Spaces that this device does not hold, by name.
fn elsewhere_names(elsewhere: &[&nibkeep_node::HeldElsewhere]) -> String {
	let named: Vec<String> = elsewhere.iter().map(|each| elsewhere_name(each)).collect();
	named_list(&named)
}

/// The devices that ask for a space here: each by its name, and as a command names it.
fn asking_names(
	requests: &[nibkeep_node::SpaceRequest],
	space: nibkeep_base::SpaceId,
) -> Vec<(String, String)> {
	requests
		.iter()
		.filter(|request| request.space == space)
		.map(|request| {
			let name = request
				.device
				.clone()
				.unwrap_or_else(|| short(request.replica));
			let word = command_word(request.device.as_deref(), request.replica);
			(name, word)
		})
		.collect()
}

/// A device as a command that the person types names it: by its name where the name is one
/// plain word, which a shell passes as it is, or else by its replica's ID.
fn command_word(name: Option<&str>, replica: EntityId) -> String {
	match name {
		Some(name)
			if !name.is_empty()
				&& name
					.chars()
					.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) =>
		{
			name.to_owned()
		}
		_ => replica.to_string(),
	}
}

/// What a device did with a space of the person's, in JSON.
const fn here_name(here: nibkeep_node::Here) -> &'static str {
	match here {
		nibkeep_node::Here::Held => "held",
		nibkeep_node::Here::Given => "given",
		nibkeep_node::Here::Asked => "asked",
		nibkeep_node::Here::Declined => "declined",
		nibkeep_node::Here::Elsewhere => "elsewhere",
	}
}

/// The spaces: those held here, the personal space first, with how many entities each
/// holds and which of the person's devices hold it, and the requests for it; then those on
/// the person's other devices alone, with what this device did with each.
pub(crate) fn spaces(
	(spaces, known, requests): (
		&[nibkeep_node::SpaceStatus],
		&[nibkeep_node::KnownSpace],
		&[nibkeep_node::SpaceRequest],
	),
	personal: nibkeep_base::SpaceId,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	// Those not on this device; one dropped here, whose files stay, is listed with those
	// held, as dropped.
	let elsewhere: Vec<&nibkeep_node::KnownSpace> = known
		.iter()
		.filter(|known| !known.held() && spaces.iter().all(|space| space.id != known.space))
		.collect();
	let dropped = |space: nibkeep_base::SpaceId| {
		known
			.iter()
			.any(|known| known.space == space && known.here == nibkeep_node::Here::Declined)
	};
	if json {
		let mut listed: Vec<serde_json::Value> = spaces
			.iter()
			.map(|space| {
				let mut shown = json!({
					"id": space.id.to_string(),
					"name": space.name,
					"personal": space.id == personal,
					"held": !dropped(space.id),
					"dropping": known
						.iter()
						.any(|known| known.space == space.id && known.dropping),
					"entities": space.entities,
					"changes": space.changes,
					"left": space.left,
				});
				if space.id != personal {
					shown["holders"] = json!(holder_names(known, space.id));
					let asking: Vec<String> = asking_names(requests, space.id)
						.into_iter()
						.map(|(name, _)| name)
						.collect();
					shown["asked_by"] = json!(asking);
				}
				shown
			})
			.collect();
		listed.extend(elsewhere.iter().map(|known_space| {
			json!({
				"id": known_space.space.to_string(),
				"name": known_space.name,
				"personal": false,
				"held": false,
				"here": here_name(known_space.here),
				"holders": holder_names(known, known_space.space),
			})
		}));
		return print_json(&json!({ "spaces": listed }), output);
	}
	for space in spaces {
		let name = space
			.name
			.as_deref()
			.map_or_else(|| "A space".to_owned(), shown);
		// A space that another device made, before it was synced here.
		let held = if space.changes == 0 {
			"waiting for your other devices".to_owned()
		} else {
			count(space.entities, "entity", "entities")
		};
		let dropping = known
			.iter()
			.any(|known| known.space == space.id && known.dropping);
		let left = if dropped(space.id) {
			"  dropped here"
		} else if dropping {
			"  dropping: nib space drop says what it waits for"
		} else if space.left {
			"  left: read only"
		} else {
			""
		};
		let on = holder_names(known, space.id);
		let on = if space.id == personal || on.is_empty() {
			String::new()
		} else {
			format!("  on: {}", shown(&on.join(", ")))
		};
		writeln!(output, "{name}  {}  {held}{on}{left}", space.id)?;
		for (device, word) in asking_names(requests, space.id) {
			writeln!(
				output,
				"  {} asks for it: nib space allow {} {}",
				shown(&device),
				space.id,
				shown(&word)
			)?;
		}
	}
	for known_space in elsewhere {
		spaces_elsewhere(known_space, known, output)?;
	}
	Ok(())
}

/// A space of the person's that this device does not hold, in a line: what this device
/// did with it, and which devices hold it.
fn spaces_elsewhere(
	known_space: &nibkeep_node::KnownSpace,
	known: &[nibkeep_node::KnownSpace],
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let name = known_space
		.name
		.as_deref()
		.map_or_else(|| "A space".to_owned(), shown);
	let space = known_space.space;
	let here = match known_space.here {
		nibkeep_node::Here::Given => format!("given to this device: nib space take {space}"),
		nibkeep_node::Here::Asked => "asked for here".to_owned(),
		nibkeep_node::Here::Declined => "declined here".to_owned(),
		nibkeep_node::Here::Held | nibkeep_node::Here::Elsewhere => {
			format!("not on this device: nib space take {space}")
		}
	};
	let on = holder_names(known, space);
	let on = if on.is_empty() {
		"on none of your devices yet".to_owned()
	} else {
		format!("on: {}", shown(&on.join(", ")))
	};
	writeln!(output, "{name}  {space}  {here}  {on}")?;
	Ok(())
}

/// What taking a space did: took it, or asked for it, to be approved on a device that holds
/// it, this one named `this`.
pub(crate) fn space_taken(
	(space, name): (nibkeep_base::SpaceId, &str),
	taking: nibkeep_node::Taking,
	(this, replica): (Option<&str>, EntityId),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let took = taking == nibkeep_node::Taking::Took;
	if json {
		return print_json(
			&json!({
				"space": space.to_string(),
				"name": name,
				"taken": took,
				"asked": !took,
			}),
			output,
		);
	}
	let name = shown(name);
	if took {
		writeln!(
			output,
			"Took the space {name}: it fills as this device syncs."
		)?;
	} else {
		let this = shown(&command_word(this, replica));
		writeln!(
			output,
			"Asked for the space {name}: on a device of yours that holds it, run nib space allow {space} {this}."
		)?;
	}
	Ok(())
}

/// How far a drop of a space named `name` on this device has come, and what it waits for.
pub(crate) fn space_dropping(
	dropping: &nibkeep_node::Dropping,
	name: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	use nibkeep_node::{DropStage, DropWait};
	let stage = match dropping.stage {
		DropStage::Waiting => "waiting",
		DropStage::Dropped => "dropped",
		DropStage::Deleted => "deleted",
	};
	if json {
		let waits: Vec<serde_json::Value> = dropping
			.waits
			.iter()
			.map(|wait| match wait {
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
					"needs": {
						"copies": needs.copies,
						"away": needs.away,
						"archived": needs.archived,
					},
				}),
				DropWait::Files(files) => json!({ "waits_for": "files", "files": files }),
				DropWait::Secrets => json!({ "waits_for": "secrets" }),
			})
			.collect();
		return print_json(
			&json!({
				"space": dropping.space.to_string(),
				"name": name,
				"stage": stage,
				"waits": waits,
			}),
			output,
		);
	}
	let name = shown(name);
	match dropping.stage {
		DropStage::Deleted => {
			writeln!(
				output,
				"Dropped the space {name}, and deleted it from this device."
			)?;
		}
		DropStage::Dropped => writeln!(
			output,
			"Dropped the space {name}: it is deleted from this device once another device of \
			 yours that holds it has learned so, after their next syncs."
		)?,
		DropStage::Waiting => {
			writeln!(output, "Dropping the space {name}, which waits for:")?;
			for wait in &dropping.waits {
				match wait {
					DropWait::OwnChanges { changes, more } => writeln!(
						output,
						"  {}{} of yours held on this disk alone, to sync",
						count(*changes, "change", "changes"),
						if *more { " or more" } else { "" }
					)?,
					DropWait::ChangeCopies { disks, needs, .. } => writeln!(
						output,
						"  its changes on {} besides this one, as its Critical policy asks: {} \
						 hold all of them",
						count(needs.copies as usize, "disk", "disks"),
						disks
					)?,
					DropWait::Files(files) => writeln!(
						output,
						"  the copies elsewhere of {} to be proven, which the holder of the \
						 sync role does, as nib sync --serve",
						count(*files, "file", "files")
					)?,
					DropWait::Secrets => writeln!(
						output,
						"  another device of yours to hold its secrets: give them to one"
					)?,
				}
			}
			writeln!(
				output,
				"It goes on by itself; nib space take {} keeps the space here.",
				dropping.space
			)?;
		}
	}
	Ok(())
}

/// That a device's request for a space was approved.
pub(crate) fn space_allowed(
	request: &nibkeep_node::SpaceRequest,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"space": request.space.to_string(),
				"name": request.name,
				"device": request.device,
				"replica": request.replica.to_string(),
				"back": request.back,
			}),
			output,
		);
	}
	let name = request
		.name
		.as_deref()
		.map_or_else(|| request.space.to_string(), shown);
	let device = request
		.device
		.as_deref()
		.map_or_else(|| short(request.replica), shown);
	writeln!(
		output,
		"Gave the space {name} to {device}: it takes it as it syncs."
	)?;
	Ok(())
}

/// Whether a character is one that changes how the text around it shows, such as the marks
/// of direction, which could make a host read backwards.
pub(crate) fn is_format(character: char) -> bool {
	matches!(
		character,
		'\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
	)
}

/// Text that another device recorded, as it may be shown on a terminal: with no control
/// character, which could rewrite what the person reads.
pub(crate) fn shown(text: &str) -> String {
	text.chars()
		.map(|character| {
			if character.is_control() || is_format(character) {
				'\u{fffd}'
			} else {
				character
			}
		})
		.collect()
}

/// The code of an invitation to a space, and what to do with it.
pub(crate) fn invitation_code(
	code: &nibkeep_node::PairingCode,
	(space, role): (&str, nibkeep_node::MemberRole),
	(minutes, suggest_admin): (u64, bool),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		print_json(
			&json!({
				"code": code.to_string(),
				"minutes": minutes,
				"space": space,
				"role": role.as_text(),
				"suggest_second_admin": suggest_admin,
			}),
			output,
		)?;
	} else {
		writeln!(
			output,
			"On their device, in their library, run:\n\n    nib space join {code}\n\nThey join {space} as {}. The code works for {}; {} wrong tries end it. Both devices must be on the same network. Waiting…",
			with_article(role),
			count(
				usize::try_from(minutes).unwrap_or(usize::MAX),
				"minute",
				"minutes"
			),
			nibkeep_node::CODE_TRIES,
		)?;
		if suggest_admin {
			writeln!(
				output,
				"{space} has one admin. So that it is never stuck without one, consider making them an admin too, with --role admin."
			)?;
		}
	}
	output.flush()?;
	Ok(())
}

/// A role with its article: an editor, a viewer, an admin.
fn with_article(role: nibkeep_node::MemberRole) -> String {
	match role {
		nibkeep_node::MemberRole::Admin | nibkeep_node::MemberRole::Editor => {
			format!("an {role}")
		}
		_ => format!("a {role}"),
	}
}

/// The question that asks whether to add the one who asks to join a space.
pub(crate) fn invitee_question(
	invitee: &nibkeep_node::Invitee,
	(space, role): (&str, nibkeep_node::MemberRole),
) -> String {
	let user_name = shown(&invitee.user_name);
	let mut lines = vec![format!(
		"{user_name} asks to join {space}, from {} on {}.",
		shown(&invitee.app),
		shown(&invitee.device)
	)];
	for member in &invitee.same_user {
		let devices: Vec<String> = member
			.replicas
			.iter()
			.filter_map(|replica| replica.device.as_deref().map(shown))
			.collect();
		let on = if devices.is_empty() {
			String::new()
		} else {
			format!(", on {}", devices.join(", "))
		};
		lines.push(format!(
			"The same user is a member already, as {}, {}{on}; adding them makes another member, with a role of its own.",
			shown(member.name.as_deref().unwrap_or("someone")),
			with_article(member.role),
		));
	}
	if invitee.was_member {
		lines.push("They were a member before, and come back as a new member.".to_owned());
	}
	lines.push(format!("Add {user_name} as {}? [y/N] ", with_article(role)));
	lines.join("\n")
}

/// Says whom an invitation added, and how their first sync went.
pub(crate) fn invited(
	invited: &nibkeep_node::Invited,
	(space, role): (&str, nibkeep_node::MemberRole),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"invited": {
					"member": invited.member.to_string(),
					"name": invited.user_name,
					"device_name": invited.device,
					"space": space,
					"role": role.as_text(),
				},
				"synced": { "spaces": spaces_json(&invited.synced.spaces) },
			}),
			output,
		);
	}
	writeln!(
		output,
		"Added {} to {space} as {}, on {}.",
		shown(&invited.user_name),
		with_article(role),
		shown(&invited.device)
	)?;
	writeln!(output, "Synced with it: {}.", synced_text(&invited.synced))?;
	Ok(())
}

/// Says which space this library joined, and how its first sync went.
pub(crate) fn joined_space(
	joined: &nibkeep_node::JoinedSpace,
	agreed: Option<bool>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"joined": {
					"space": joined.space.to_string(),
					"name": joined.name,
					"member": joined.member.to_string(),
					"role": joined.role.as_text(),
					"person": joined.person.as_ref().map(|(person, name)| json!({
						"id": person.to_string(),
						"name": name,
						"agreed": agreed,
					})),
				},
				"synced": { "spaces": spaces_json(&joined.synced.spaces) },
				"refused": joined.refused,
			}),
			output,
		);
	}
	writeln!(
		output,
		"Joined {} as {}; your other devices that take new spaces get it when they sync.",
		shown(&joined.name),
		with_article(joined.role)
	)?;
	writeln!(output, "Synced with it: {}.", synced_text(&joined.synced))?;
	if let (Some((_, person)), Some(agreed)) = (&joined.person, agreed) {
		if agreed {
			writeln!(output, "You are {} there now.", shown(person))?;
		} else {
			writeln!(
				output,
				"The invitation names you {}; you are linked to nobody there until you agree, with nib space link.",
				shown(person)
			)?;
		}
	}
	Ok(())
}

/// The members of a space, with their roles and devices, as JSON.
fn members_json(
	members: &[nibkeep_node::SpaceMember],
	names: &std::collections::BTreeMap<nibkeep_base::EntityId, String>,
) -> serde_json::Value {
	let person_json = |person: Option<nibkeep_base::EntityId>| {
		person.map(|person| json!({ "id": person.to_string(), "name": names.get(&person) }))
	};
	serde_json::Value::Array(
		members
			.iter()
			.map(|member| {
				json!({
					"id": member.record.to_string(),
					"user": member.user.map(|user| user.to_string()),
					"name": member.name,
					"role": member.role.as_text(),
					"you": member.this_user,
					"person": person_json(member.person.holds),
					"person_by_admin": person_json(member.person.by_admin),
					"person_by_member": person_json(member.person.by_member),
					"person_deleted": member.person.deleted,
					"devices": member.replicas.iter().map(|replica| json!({
						"id": replica.record.to_string(),
						"name": replica.app,
						"device_name": replica.device,
						"revoked": replica.revoked,
						"ai_agent": replica.ai_agent,
						"this": replica.this,
						"holds_secrets": replica.holds_secrets,
						"asks_for_secrets": replica.asks_for_secrets,
					})).collect::<Vec<_>>(),
				})
			})
			.collect(),
	)
}

/// Prints the members of a space, with their roles and devices.
fn print_members(
	members: &[nibkeep_node::SpaceMember],
	names: &std::collections::BTreeMap<nibkeep_base::EntityId, String>,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	for member in members {
		let name = shown(member.name.as_deref().unwrap_or("Someone"));
		let you = if member.this_user { " (you)" } else { "" };
		let devices: Vec<String> = member
			.replicas
			.iter()
			.map(|replica| {
				let device = shown(replica.device.as_deref().unwrap_or("a device"));
				let app = shown(replica.app.as_deref().unwrap_or("an app"));
				let revoked = if replica.revoked { ", revoked" } else { "" };
				let ai_agent = if replica.ai_agent {
					", an AI agent"
				} else {
					""
				};
				let secrets = if replica.holds_secrets {
					", holds the secrets"
				} else if replica.asks_for_secrets {
					", asks for the secrets"
				} else {
					""
				};
				format!("{app} on {device}{ai_agent}{secrets}{revoked}")
			})
			.collect();
		let role = match member.role {
			nibkeep_node::MemberRole::None => "no longer a member".to_owned(),
			role => role.to_string(),
		};
		let person_name = |person: nibkeep_base::EntityId| {
			shown(names.get(&person).map_or("someone", String::as_str))
		};
		let link = &member.person;
		let person = match (link.holds, link.by_admin, link.by_member) {
			(Some(person), ..) => format!("  as {}", person_name(person)),
			(None, _, Some(person)) if link.deleted => {
				format!("  linked to {}, who is deleted", person_name(person))
			}
			(None, Some(person), None) => {
				format!(
					"  named {} by an admin, which they have not agreed to",
					person_name(person)
				)
			}
			(None, None, Some(person)) => {
				format!(
					"  says they are {}, which no admin has agreed to",
					person_name(person)
				)
			}
			(None, Some(admin), Some(own)) => format!(
				"  named {} by an admin, while they say they are {}",
				person_name(admin),
				person_name(own)
			),
			(None, None, None) => String::new(),
		};
		writeln!(output, "{name}{you}  {role}  {}{person}", member.record)?;
		for device in devices {
			writeln!(output, "    {device}")?;
		}
	}
	Ok(())
}

/// Says what linking a member to a person did: whether the link holds now, or waits for the
/// other side.
pub(crate) fn linked(
	member: Option<&str>,
	(person, link): (
		Option<(nibkeep_base::EntityId, Option<String>)>,
		nibkeep_node::PersonLink,
	),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"member": member,
				"person": person.as_ref().map(|(id, name)| json!({
					"id": id.to_string(),
					"name": name,
				})),
				"holds": link.holds.is_some(),
			}),
			output,
		);
	}
	let member = shown(member.unwrap_or("The member"));
	let person = person.map(|(id, name)| name.unwrap_or_else(|| id.to_string()));
	match (person.as_deref(), link.holds) {
		(None, _) => writeln!(output, "{member} is linked to nobody now, on this side.")?,
		(Some(person), Some(_)) => writeln!(output, "{member} is {} now.", shown(person))?,
		(Some(person), None) => writeln!(
			output,
			"{member} is {} once the other side agrees: the member, or an admin.",
			shown(person)
		)?,
	}
	Ok(())
}

/// The question that asks a joiner whether they are the person whom the invitation names.
pub(crate) fn person_question(person: &str, space: &str) -> String {
	format!(
		"The invitation names you {} in {}. Are you? [y/N] ",
		shown(person),
		shown(space)
	)
}

/// Says whether a space allows AI agents.
pub(crate) fn ai_agents_allowed(
	allowed: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "ai_agents": allowed }), output);
	}
	if allowed {
		writeln!(output, "AI agents are allowed there.")?;
	} else {
		writeln!(output, "AI agents are not allowed there.")?;
	}
	Ok(())
}

/// The changes set aside in a space with members, as JSON.
fn set_aside_json(
	set_aside: &[nibkeep_node::SetAside],
	members: &[nibkeep_node::SpaceMember],
) -> serde_json::Value {
	let member_of = |id: Option<EntityId>| members.iter().find(|member| Some(member.record) == id);
	serde_json::Value::Array(
		set_aside
			.iter()
			.map(|aside| {
				json!({
					"change": aside.change.to_string(),
					"member": aside.member.map(|member| member.to_string()),
					"member_name": member_of(aside.member).and_then(|member| member.name.clone()),
					"replica": aside.replica.map(|replica| replica.to_string()),
					"millis": aside.millis,
					"operations": aside.operations,
					"governs": aside.governs,
					"acceptable": aside.acceptable,
					"entities": aside.entities.iter().map(|(entity, name)| json!({
						"id": entity.to_string(),
						"name": name,
					})).collect::<Vec<_>>(),
				})
			})
			.collect(),
	)
}

/// Prints the changes set aside in a space with members, by member and device, with what
/// each changes, and whether accepting would make it count.
fn print_set_aside(
	set_aside: &[nibkeep_node::SetAside],
	members: &[nibkeep_node::SpaceMember],
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let member_of = |id: Option<EntityId>| members.iter().find(|member| Some(member.record) == id);
	for aside in set_aside {
		let member = member_of(aside.member);
		let mut by = member
			.and_then(|member| member.name.as_deref())
			.map_or_else(|| "nobody listed".to_owned(), shown);
		if let Some(device) = member
			.and_then(|member| {
				member
					.replicas
					.iter()
					.find(|replica| Some(replica.record) == aside.replica)
			})
			.and_then(|replica| replica.device.as_deref())
		{
			by = format!("{by}, on {}", shown(device));
		}
		let at = jiff::Timestamp::from_millisecond(i64::try_from(aside.millis).unwrap_or(i64::MAX))
			.map_or_else(|_| "an unknown time".to_owned(), |at| at.to_string());
		let what = if aside.governs {
			"which says who may write to the space, and is never accepted".to_owned()
		} else if let Some(count) = aside.operations {
			let operations = if count == 1 {
				"of 1 operation".to_owned()
			} else {
				format!("of {count} operations")
			};
			let names: Vec<String> = aside
				.entities
				.iter()
				.map(|(entity, name)| name.as_deref().map_or_else(|| short_id(*entity), shown))
				.collect();
			let on = if names.is_empty() {
				String::new()
			} else {
				format!(", on {}", names.join(", "))
			};
			let allowed = if aside.acceptable {
				""
			} else {
				", which accepting would not make count"
			};
			format!("{operations}{on}{allowed}")
		} else {
			"that this device cannot read".to_owned()
		};
		let change = aside.change.to_string();
		// The hash's first eight digits, after the name of its algorithm.
		let short = change
			.split_once(':')
			.map_or(change.as_str(), |(_, digits)| {
				&digits[..digits.len().min(8)]
			});
		writeln!(output, "{short}  by {by}, at {at}, {what}")?;
	}
	writeln!(
		output,
		"Accept a member's changes with nib space accept SPACE MEMBER --yes."
	)?;
	Ok(())
}

/// The first eight characters of an entity's ID, as lists show them.
fn short_id(entity: EntityId) -> String {
	entity.to_string().chars().take(8).collect()
}

/// Says what leaving a space would do, before the person confirms it: whether it closes it,
/// as its last member.
pub(crate) fn leaving(
	space: &str,
	closes: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "left": false, "closes": closes }),
			output,
		);
	}
	let space = shown(space);
	if closes {
		writeln!(
			output,
			"You are the last member of {space}: leaving closes it for good. Your devices keep what they have, read only."
		)?;
	} else {
		writeln!(
			output,
			"Leaving {space} ends your devices there: they keep what they have, read only, and get nothing added after. You come back only if an admin invites you again."
		)?;
	}
	Ok(())
}

/// Says that the person left a space, and whether that closed it.
pub(crate) fn left(
	space: &str,
	closed: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "left": true, "closed": closed }),
			output,
		);
	}
	let space = shown(space);
	if closed {
		writeln!(
			output,
			"You left {space}, its last member, which closes it: every device keeps what it has, read only."
		)?;
	} else {
		writeln!(
			output,
			"You left {space}. Your devices there keep what they have, read only; the others learn of it when they sync, and give the space a new key."
		)?;
	}
	Ok(())
}

/// Says what removing a member would do, before the person confirms it.
pub(crate) fn removing(
	(space, member): (&str, Option<&str>),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "member": member, "removed": false }),
			output,
		);
	}
	writeln!(
		output,
		"Removing {} ends every device of theirs in {}: they keep what they have, read only, and get nothing added after, and the space gets a new key. What they change before they learn of it waits for an admin's review.",
		shown(member.unwrap_or("the member")),
		shown(space)
	)?;
	Ok(())
}

/// Says that a member was removed.
pub(crate) fn removed(
	(space, member): (&str, Option<&str>),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "member": member, "removed": true }),
			output,
		);
	}
	writeln!(
		output,
		"{} is no longer a member of {}, which has a new key; their devices learn of it when they sync.",
		shown(member.unwrap_or("The member")),
		shown(space)
	)?;
	Ok(())
}

/// Says what claiming the admin role of a space would do.
pub(crate) fn claiming(space: &str, json: bool, output: &mut dyn Write) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "space": space, "claimed": false }), output);
	}
	writeln!(
		output,
		"Claiming the admin role of {} shows every member your claim. It takes effect 30 days after another member's device first sees it, unless an admin rejects it, and for 30 days after that you can only undo: no purge, no new admin, and no admin lowered.",
		shown(space)
	)?;
	Ok(())
}

/// Says that this user claimed the admin role of a space.
pub(crate) fn claimed(space: &str, json: bool, output: &mut dyn Write) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "space": space, "claimed": true }), output);
	}
	writeln!(
		output,
		"You claim the admin role of {}; the 30 days start when another member's device sees the claim, after it syncs.",
		shown(space)
	)?;
	Ok(())
}

/// The claims to the admin role in a space, as JSON.
fn claims_json(claims: &[nibkeep_node::ClaimInSpace]) -> serde_json::Value {
	serde_json::Value::Array(
		claims
			.iter()
			.map(|claim| {
				json!({
					"claimant": claim.claimant.to_string(),
					"name": claim.name,
					"made": moment_json(claim.made),
					"seen": claim.seen.map(moment_json),
					"effective": claim.effective.map(moment_json),
					"settled": claim.settled.map(moment_json),
					"rejected": claim.rejected,
				})
			})
			.collect(),
	)
}

/// Prints the claims to the admin role in a space.
fn print_claims(
	claims: &[nibkeep_node::ClaimInSpace],
	zone: &jiff::tz::TimeZone,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	for claim in claims {
		let name = claim.name.as_deref().unwrap_or("(no name)");
		let state = match (claim.rejected, claim.effective, claim.settled) {
			(true, ..) => "rejected by an admin".to_owned(),
			(false, Some(effective), Some(settled)) => format!(
				"takes effect {}, settled {}",
				moment_text(effective, zone),
				moment_text(settled, zone)
			),
			_ => "waiting for another member's device to see it".to_owned(),
		};
		writeln!(
			output,
			"{} claims the admin role since {}: {state}",
			shown(name),
			moment_text(claim.made, zone)
		)?;
	}
	Ok(())
}

/// Says that a member's claim to the admin role was rejected.
pub(crate) fn rejected_claim(
	(space, member): (&str, &str),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "member": member, "rejected": true }),
			output,
		);
	}
	writeln!(
		output,
		"Rejected {}'s claim to the admin role of {}: it counts for nothing once another member's device sees the rejection, within 30 days after the claim took effect.",
		shown(member),
		shown(space)
	)?;
	Ok(())
}

/// Says that a member through a group was let back into a space.
pub(crate) fn let_in(
	(space, member): (&str, &str),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "member": member, "kept_out": false }),
			output,
		);
	}
	writeln!(
		output,
		"{} is in {} again through their group; their devices get its keys when they sync.",
		shown(member),
		shown(space)
	)?;
	Ok(())
}

/// Says that a record was made for a member through a group, which they confirm.
pub(crate) fn made_direct(
	(space, member): (&str, &str),
	role: nibkeep_node::MemberRole,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "space": space, "member": member, "role": role.as_text(), "confirmed": false }),
			output,
		);
	}
	writeln!(
		output,
		"{} becomes a direct member of {}, as {}, once they confirm with nib space confirm after a sync.",
		shown(member),
		shown(space),
		role
	)?;
	Ok(())
}

/// Says that this user is a direct member of a space now.
pub(crate) fn confirmed_direct(
	space: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "space": space, "confirmed": true }), output);
	}
	writeln!(
		output,
		"You are a direct member of {} now; your other devices follow when they sync.",
		shown(space)
	)?;
	Ok(())
}

/// Says that a member was given a role.
pub(crate) fn role_given(
	name: Option<&str>,
	role: nibkeep_node::MemberRole,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "name": name, "role": role.as_text() }), output);
	}
	writeln!(
		output,
		"{} is {} now; their devices learn of it when they sync.",
		shown(name.unwrap_or("The member")),
		with_article(role)
	)?;
	Ok(())
}

/// Says that a space was made.
pub(crate) fn space_made(
	space: nibkeep_base::SpaceId,
	name: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "id": space.to_string(), "name": name }), output);
	}
	writeln!(
		output,
		"Made the space {name} ({space}); your other devices that take new spaces get it when they sync"
	)?;
	Ok(())
}

/// Prints the groups in a space, with their roles, whether they agreed, and their members,
/// and the groups inside each, at every level.
pub(crate) fn space_groups(
	groups: &[nibkeep_node::GroupInSpace],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&groups_json(groups), output);
	}
	if groups.is_empty() {
		writeln!(output, "No groups are in this space.")?;
	}
	print_groups(groups, 0, output)
}

/// What `nib space show` shows of a space or a group.
pub(crate) struct SpaceShown<'a> {
	/// Its members.
	pub(crate) members: &'a [nibkeep_node::SpaceMember],
	/// The names of the people that members are linked to.
	pub(crate) names: &'a std::collections::BTreeMap<EntityId, String>,
	/// The groups in it.
	pub(crate) groups: &'a [nibkeep_node::GroupInSpace],
	/// For a group, the requests in it that it join spaces.
	pub(crate) requests: Option<&'a [nibkeep_node::GroupJoinRequest]>,
	/// The claims to its admin role.
	pub(crate) claims: &'a [nibkeep_node::ClaimInSpace],
	/// The changes set aside in it, for an admin's review.
	pub(crate) set_aside: &'a [nibkeep_node::SetAside],
}

/// Prints what `nib space show` shows: the members first, then each part that holds
/// something, under its heading.
pub(crate) fn space_shown(
	space: &SpaceShown<'_>,
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut shown = json!({
			"members": members_json(space.members, space.names),
			"groups": groups_json(space.groups),
			"claims": claims_json(space.claims),
			"set_aside": set_aside_json(space.set_aside, space.members),
		});
		if let Some(requests) = space.requests {
			shown["requests"] = join_requests_json(requests);
		}
		return print_json(&shown, output);
	}
	writeln!(output, "Members:")?;
	print_members(space.members, space.names, &mut Indented::new(output))?;
	if !space.groups.is_empty() {
		writeln!(output, "\nGroups:")?;
		print_groups(space.groups, 0, &mut Indented::new(output))?;
	}
	if let Some(requests) = space.requests.filter(|requests| !requests.is_empty()) {
		writeln!(output, "\nSpaces that ask for this group:")?;
		print_join_requests(requests, &mut Indented::new(output))?;
	}
	if !space.claims.is_empty() {
		writeln!(output, "\nClaims to the admin role:")?;
		print_claims(space.claims, zone, &mut Indented::new(output))?;
	}
	if !space.set_aside.is_empty() {
		writeln!(output, "\nChanges set aside for review:")?;
		print_set_aside(space.set_aside, space.members, &mut Indented::new(output))?;
	}
	Ok(())
}

/// A writer that indents each line but empty ones by two spaces, as a part under its
/// heading.
struct Indented<'a> {
	/// Where the lines go.
	inner: &'a mut dyn Write,
	/// Whether what comes next starts a line.
	starts: bool,
}

impl<'a> Indented<'a> {
	/// Indents what goes to `inner`.
	fn new(inner: &'a mut dyn Write) -> Self {
		Self {
			inner,
			starts: true,
		}
	}
}

impl Write for Indented<'_> {
	fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
		for line in bytes.split_inclusive(|byte| *byte == b'\n') {
			if self.starts && line != b"\n" {
				self.inner.write_all(b"  ")?;
			}
			self.inner.write_all(line)?;
			self.starts = line.ends_with(b"\n");
		}
		Ok(bytes.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		self.inner.flush()
	}
}

/// The groups in a space or a group, as JSON.
fn groups_json(groups: &[nibkeep_node::GroupInSpace]) -> serde_json::Value {
	serde_json::Value::Array(
		groups
			.iter()
			.map(|group| {
				json!({
					"group": group.group.to_string(),
					"name": group.name,
					"role": group.role.as_text(),
					"agreed": group.agreed,
					"members": group.members.iter().map(|member| json!({
						"record": member.record.to_string(),
						"name": member.name,
						"role": member.role.as_text(),
					})).collect::<Vec<_>>(),
					"groups": groups_json(&group.groups),
				})
			})
			.collect(),
	)
}

/// Prints the groups in a space or a group, each indented by `depth` levels, with its members
/// and the groups inside it one level deeper.
fn print_groups(
	groups: &[nibkeep_node::GroupInSpace],
	depth: usize,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let indent = "  ".repeat(depth);
	for group in groups {
		let name = group
			.name
			.clone()
			.unwrap_or_else(|| group.group.to_string());
		let state = if group.agreed {
			""
		} else {
			", waiting for the group's admin to agree"
		};
		writeln!(output, "{indent}{name}: {}{state}", group.role)?;
		for member in &group.members {
			let name = member.name.as_deref().unwrap_or("(no name)");
			writeln!(output, "{indent}  {name}: {}", member.role)?;
		}
		print_groups(&group.groups, depth + 1, output)?;
	}
	Ok(())
}

/// Prints the requests in a group that it join spaces.
pub(crate) fn join_requests(
	requests: &[nibkeep_node::GroupJoinRequest],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&join_requests_json(requests), output);
	}
	if requests.is_empty() {
		writeln!(output, "No space asks for this group.")?;
	}
	print_join_requests(requests, output)
}

/// The requests in a group that it join spaces, as JSON.
fn join_requests_json(requests: &[nibkeep_node::GroupJoinRequest]) -> serde_json::Value {
	serde_json::Value::Array(
		requests
			.iter()
			.map(|request| {
				json!({
					"space": request.space.to_string(),
					"name": request.name,
					"agreed": request.agreed,
					"withdrawn": request.withdrawn,
				})
			})
			.collect(),
	)
}

/// Prints the requests in a group that it join spaces, a line for each.
fn print_join_requests(
	requests: &[nibkeep_node::GroupJoinRequest],
	output: &mut dyn Write,
) -> Result<(), CliError> {
	for request in requests {
		let name = request
			.name
			.clone()
			.unwrap_or_else(|| request.space.to_string());
		let state = if request.withdrawn {
			"withdrawn"
		} else if request.agreed {
			"agreed"
		} else {
			"waiting for an admin of the group to agree"
		};
		writeln!(output, "{name} ({}): {state}", request.space)?;
	}
	Ok(())
}

/// Says that an entity was moved, and what it is now.
pub(crate) fn moved(
	entity: EntityId,
	entities: usize,
	space: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "id": entity.to_string(), "space": space, "entities": entities }),
			output,
		);
	}
	if entities > 1 {
		writeln!(
			output,
			"Moved to {space} as {}, with {} it held",
			short(entity),
			count(entities - 1, "entity", "entities")
		)?;
	} else {
		writeln!(output, "Moved to {space} as {}", short(entity))?;
	}
	Ok(())
}

/// Says when what a move leaves behind is purged, who purges the rest, and who no longer sees
/// what moved once it is all purged.
pub(crate) fn purged_later(
	preview: &nibkeep_node::MovePreview,
	from: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return Ok(());
	}
	writeln!(
		output,
		"What stays in {from} is purged once another device holds what moved."
	)?;
	if !preview.purges_all {
		writeln!(
			output,
			"You purge the changes you signed there; an admin of {from} purges the rest."
		)?;
	}
	if !preview.losing.is_empty() {
		let losing: Vec<String> = preview.losing.iter().map(|name| shown(name)).collect();
		let when = if preview.purges_all {
			"once it is purged"
		} else {
			"once an admin purges the rest"
		};
		writeln!(
			output,
			"{} will no longer see it {when}.",
			losing.join(", ")
		)?;
	}
	Ok(())
}

/// Names the members who see what moved for the first time, with its history if the move
/// brought it.
pub(crate) fn seen_first(
	gaining: &[String],
	with_history: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json || gaining.is_empty() {
		return Ok(());
	}
	let names: Vec<String> = gaining.iter().map(|name| shown(name)).collect();
	let what = if with_history {
		"it, with its history,"
	} else {
		"it"
	};
	writeln!(
		output,
		"{} see {what} for the first time.",
		names.join(", ")
	)?;
	Ok(())
}

/// Says what `nib purge --behind` purged, or, before it is confirmed, what it would do.
pub(crate) fn purge_behind(
	values: usize,
	purged: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "values": values, "purged": purged }), output);
	}
	match (values, purged) {
		(0, _) => writeln!(output, "Nothing was left for you to purge.")?,
		(values, false) => writeln!(
			output,
			"Would purge {} that the move left behind, but the marks that lead to where it is now.",
			count(values, "value", "values")
		)?,
		(values, true) => writeln!(
			output,
			"Purged {} that the move left behind.",
			count(values, "value", "values")
		)?,
	}
	Ok(())
}

/// What a move left behind and asked the admins of its space to purge.
pub(crate) struct LeftBehind {
	/// The request.
	pub(crate) request: nibkeep_node::PurgeRequest,
	/// The space it was left in, with its name.
	pub(crate) left_in: (nibkeep_base::SpaceId, Option<String>),
	/// The name of what moved, where it is now.
	pub(crate) name: Option<String>,
	/// The name of the space it moved to.
	pub(crate) now_in: Option<String>,
}

/// Lists what moves left behind and asked the admins of their spaces to purge.
pub(crate) fn purge_requests(
	requests: &[LeftBehind],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let requests: Vec<serde_json::Value> = requests
			.iter()
			.map(|left| {
				json!({
					"id": left.request.entity.to_string(),
					"left_in": left.left_in.0.to_string(),
					"left_in_name": left.left_in.1,
					"moved_into": left.request.moved_into.1.to_string(),
					"space": left.request.moved_into.0.to_string(),
					"name": left.name,
					"space_name": left.now_in,
				})
			})
			.collect();
		return print_json(&json!({ "requests": requests }), output);
	}
	if requests.is_empty() {
		writeln!(
			output,
			"Nothing that a move left behind waits for you to purge."
		)?;
	}
	for left in requests {
		writeln!(
			output,
			"{}  {}  in {}, moved to {}",
			shown(left.name.as_deref().unwrap_or("Something")),
			left.request.entity,
			shown(left.left_in.1.as_deref().unwrap_or("a space")),
			shown(left.now_in.as_deref().unwrap_or("another space"))
		)?;
	}
	Ok(())
}

/// Says what a move of a folder or a list would take: each entity that would move, and the
/// folders and lists that stay and hold some of them.
pub(crate) fn move_preview(
	preview: &nibkeep_node::MovePreview,
	(from, to): (&str, &str),
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let named = |entity: &EntityId| {
		(
			short(*entity),
			name_of(*entity).unwrap_or_else(|| "(no name)".to_owned()),
		)
	};
	if json {
		let ids = |entities: &[EntityId]| -> Vec<String> {
			entities.iter().map(ToString::to_string).collect()
		};
		return print_json(
			&json!({
								"entities": ids(&preview.entities),
				"staying": ids(&preview.staying),
				"losing": preview.losing,
				"gaining": preview.gaining,
				"purges_all": preview.purges_all,
				"moved": false,
			}),
			output,
		);
	}
	writeln!(
		output,
		"Moving it takes {} to {to}:",
		count(preview.entities.len(), "entity", "entities")
	)?;
	for entity in &preview.entities {
		let (id, name) = named(entity);
		writeln!(output, "  {id}  {name}")?;
	}
	if !preview.staying.is_empty() {
		writeln!(
			output,
			"Some are also in these, which stay in {from} and find them where they go:"
		)?;
		for entity in &preview.staying {
			let (id, name) = named(entity);
			writeln!(output, "  {id}  {name}")?;
		}
	}
	let listed = |people: &[String]| -> String {
		people
			.iter()
			.map(|name| shown(name))
			.collect::<Vec<_>>()
			.join(", ")
	};
	if !preview.gaining.is_empty() {
		writeln!(
			output,
			"{} see it in {to} for the first time, with what history it brings.",
			listed(&preview.gaining)
		)?;
	}
	if !preview.losing.is_empty() {
		let when = if preview.purges_all {
			"once what stays is purged"
		} else {
			"once an admin of the old space purges the rest"
		};
		writeln!(
			output,
			"{} will no longer see it in {from} {when}.",
			listed(&preview.losing)
		)?;
	}
	Ok(())
}

/// Says that a space was renamed.
pub(crate) fn space_renamed(
	space: nibkeep_base::SpaceId,
	name: &str,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "id": space.to_string(), "name": name }), output);
	}
	writeln!(output, "Renamed the space to {name}")?;
	Ok(())
}

/// Prints what checking the library found.
pub(crate) fn report(report: &Report, json: bool, output: &mut dyn Write) -> Result<(), CliError> {
	if json {
		let spaces: Vec<serde_json::Value> = report
			.spaces
			.iter()
			.map(|space| {
				let problems: Vec<String> = space.problems.iter().map(problem).collect();
				json!({
					"id": space.id.to_string(),
					"name": space.name,
					"changes": space.changes,
					"blobs": space.blobs,
					"waiting": space.waiting,
					"in_place": space.in_place,
					"changed": space.changed.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
					"problems": problems,
				})
			})
			.collect();
		return print_json(
			&json!({ "sound": report.is_sound(), "spaces": spaces }),
			output,
		);
	}
	for space in &report.spaces {
		let waiting = if space.waiting == 0 {
			String::new()
		} else {
			format!(
				", {} waiting for {}",
				count(space.waiting, "blob", "blobs"),
				if space.waiting == 1 {
					"its key"
				} else {
					"their keys"
				}
			)
		};
		writeln!(
			output,
			"{}: {} and {} checked{waiting}, {}",
			space.name.as_deref().unwrap_or("A space"),
			count(space.changes, "change", "changes"),
			count(space.blobs, "blob", "blobs"),
			if space.problems.is_empty() {
				"sound"
			} else {
				"damaged"
			},
		)?;
		for found in &space.problems {
			writeln!(output, "  {}", problem(found))?;
		}
		if space.in_place > 0 {
			writeln!(
				output,
				"  {} kept where {} found, unchanged",
				count(space.in_place, "file", "files"),
				if space.in_place == 1 {
					"it was"
				} else {
					"they were"
				}
			)?;
		}
		for path in &space.changed {
			writeln!(
				output,
				"  {} changed, went or cannot be read since it was added, and does not count as a copy",
				path.display()
			)?;
		}
	}
	Ok(())
}

/// A problem that checking found, in words.
fn problem(problem: &Problem) -> String {
	match problem {
		Problem::Database(found) => format!("the log file is damaged: {found}"),
		Problem::ForgedChange(change) => format!(
			"change {change} has a bad signature: the log file was damaged or tampered with"
		),
		Problem::DamagedChange(change, error) => format!(
			"change {change} cannot be read: {error}; the log file was damaged or tampered with"
		),
		Problem::DamagedBlob(content) => format!(
			"the copy of blob {content} is damaged: it is moved aside, and the next sync brings it again from a device that holds it intact"
		),
		Problem::WrongBlobKey(content) => {
			format!("the copy of blob {content} is intact, but its key does not open it")
		}
		Problem::UnreadableBlob(content, error) => {
			format!("the copy of blob {content} cannot be read: {error}")
		}
		Problem::BadBlobKeys(sealed) => format!(
			"bad keys of the sealed file {sealed} were found and removed when the library opened"
		),
		Problem::KeylessBlob(sealed) => format!(
			"no key opens the sealed file {sealed}; unless another device has one, its content is lost"
		),
		Problem::UnknownFile(path) => format!("{} is not a sealed blob", path.display()),
		Problem::Unchecked(error) => format!("the checks could not finish: {error}"),
		_ => "something else is wrong".to_owned(),
	}
}

/// What a purge takes, or took, in JSON.
fn purge_json(purge: &Purge, field: &FieldName, done: bool) -> serde_json::Value {
	let copies = |copies: &[nibkeep_node::PurgedCheckout]| -> Vec<serde_json::Value> {
		copies
			.iter()
			.map(|copy| {
				json!({
					"path": copy.path.display().to_string(),
					"edited": copy.edited,
					"deleted": copy.deleted,
				})
			})
			.collect()
	};
	let originals: Vec<serde_json::Value> = purge
		.originals
		.iter()
		.map(|(device, path)| json!({ "device": device.to_string(), "path": path }))
		.collect();
	let also: Vec<String> = purge.also.iter().map(ToString::to_string).collect();
	let thumbnails: Vec<String> = purge.thumbnails.iter().map(ToString::to_string).collect();
	let elsewhere: Vec<String> = purge.elsewhere.iter().map(ToString::to_string).collect();
	json!({
		"field": field.to_string(),
		"values": purge.statements.len(),
		"secret": purge.secret,
		"changes": purge.changes,
		"blobs": purge.blobs,
		"elsewhere": elsewhere,
		"also": also,
		"thumbnails": thumbnails,
		"checkouts": copies(&purge.checkouts),
		"tree_copies": copies(&purge.tree_copies),
		"originals": originals,
		"waiting": waiting_copies_json(&purge.waiting),
		"purged": done,
	})
}

/// What a purge that waits on this device keeps apart, for people: how many changes, secret
/// values and files.
fn waiting_kept(purge: &nibkeep_node::WaitingPurge) -> String {
	if let Some(on) = &purge.on {
		return format!(
			"{} on the backup {}",
			count(
				usize::try_from(on.items).unwrap_or(usize::MAX),
				"item",
				"items"
			),
			on.name
		);
	}
	let mut parts = Vec::new();
	if purge.changes > 0 {
		parts.push(count(purge.changes, "change", "changes"));
	}
	if purge.secrets > 0 {
		parts.push(count(purge.secrets, "secret value", "secret values"));
	}
	if purge.blobs > 0 {
		let size = if purge.bytes > 0 {
			format!(" ({})", amount_text(purge.bytes))
		} else {
			String::new()
		};
		parts.push(format!(
			"{}{size}",
			count(purge.blobs, "stored file", "stored files")
		));
	}
	if parts.is_empty() {
		"nothing".to_owned()
	} else {
		parts.join(", ")
	}
}

/// A purge that waits on this device, in JSON.
fn waiting_purge_json(purge: &nibkeep_node::WaitingPurge, space: &str) -> serde_json::Value {
	json!({
		"purge": purge.purge.to_string(),
		"space": purge.space.to_string(),
		"space_name": space,
		"received": moment_json(purge.received),
		"ends": moment_json(purge.ends),
		"changes": purge.changes,
		"secrets": purge.secrets,
		"blobs": purge.blobs,
		"bytes": purge.bytes,
		"backup": purge.on.as_ref().map(|on| on.backup.to_string()),
		"backup_name": purge.on.as_ref().map(|on| on.name.clone()),
		"backup_items": purge.on.as_ref().map(|on| on.items),
	})
}

/// Prints the purges that wait on this device: each with its space, when it came, when its
/// wait ends, and what it keeps apart until then; and how long this device waits.
pub(crate) fn waiting_purges(
	(waiting, spaces): (&[nibkeep_node::WaitingPurge], &[String]),
	days: u64,
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let purges: Vec<serde_json::Value> = waiting
			.iter()
			.zip(spaces)
			.map(|(purge, space)| waiting_purge_json(purge, space))
			.collect();
		return print_json(&json!({ "waits_days": days, "purges": purges }), output);
	}
	if days == 0 {
		writeln!(
			output,
			"This device destroys what a purge destroys at once: only an archive node waits (nib device mark --archive)."
		)?;
	} else {
		writeln!(
			output,
			"This device waits {} before it destroys what a purge destroys.",
			count(usize::try_from(days).unwrap_or(usize::MAX), "day", "days")
		)?;
	}
	if waiting.is_empty() {
		writeln!(output, "No purge waits here.")?;
		return Ok(());
	}
	for (purge, space) in waiting.iter().zip(spaces) {
		writeln!(
			output,
			"{}  in {space}, came {}, waits until {}: {}",
			short_change(purge.purge),
			moment_text(purge.received, zone),
			moment_text(purge.ends, zone),
			waiting_kept(purge)
		)?;
	}
	writeln!(
		output,
		"nib purges stop PURGE brings one back everywhere; nib purges end PURGE destroys it now; --on BACKUP picks a backup."
	)?;
	Ok(())
}

/// Prints that a purge that waited here, in each place `places` lists, was stopped, or its
/// wait ended, or with `done` false, what that would do.
pub(crate) fn purge_waited(
	(places, space): (&[nibkeep_node::WaitingPurge], &str),
	(stopping, done): (bool, bool),
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let values: Vec<serde_json::Value> = places
			.iter()
			.map(|place| {
				let mut value = waiting_purge_json(place, space);
				value["stopped"] = json!(stopping && done);
				value["ended"] = json!(!stopping && done);
				value
			})
			.collect();
		return print_json(&json!(values), output);
	}
	let Some(first) = places.first() else {
		return Ok(());
	};
	let kept = places
		.iter()
		.map(|place| match &place.on {
			Some(_) => waiting_kept(place),
			None => format!("{} on this device", waiting_kept(place)),
		})
		.collect::<Vec<_>>()
		.join(", and ");
	let id = short_change(first.purge);
	let ends = places
		.iter()
		.map(|place| place.ends)
		.max()
		.unwrap_or(first.ends);
	let text = match (stopping, done) {
		(true, true) => format!(
			"Stopped the purge {id} in {space}: {kept} came back, here now and on your other devices at their next syncs"
		),
		(true, false) => format!(
			"Would stop the purge {id} in {space}, bringing back {kept} everywhere; nib purges stop {id} --yes does"
		),
		(false, true) => {
			format!("Ended the wait of the purge {id} in {space}: {kept} went for good")
		}
		(false, false) => format!(
			"Would destroy now {kept} that the purge {id} in {space} destroyed, which waits until {}; nib purges end {id} --yes does, for good",
			moment_text(ends, zone)
		),
	};
	writeln!(output, "{text}")?;
	Ok(())
}

/// The start of a change's ID, as `nib purges` shows a purge.
fn short_change(change: nibkeep_base::ChangeId) -> String {
	change.to_string().chars().take(16).collect()
}

/// The copies that wait before they destroy what a purge destroys, in JSON.
fn waiting_copies_json(copies: &[nibkeep_node::WaitingCopy]) -> Vec<serde_json::Value> {
	copies
		.iter()
		.map(|copy| {
			json!({
				"name": copy.name,
				"this": copy.this,
				"backup": copy.backup,
				"others": copy.others,
				"member": copy.member,
				"days": copy.days,
			})
		})
		.collect()
}

/// Says which copies wait before they destroy what a purge destroys, and for how long; and
/// that their owners can stop it meanwhile, on each.
fn waiting_copies(
	copies: &[nibkeep_node::WaitingCopy],
	output: &mut dyn Write,
) -> Result<(), CliError> {
	for copy in copies {
		let days = count(
			usize::try_from(copy.days).unwrap_or(usize::MAX),
			"day",
			"days",
		);
		let whose = match (&copy.member, copy.others) {
			(Some(member), _) => format!("{member}'s "),
			(None, true) => "another member's ".to_owned(),
			(None, false) => String::new(),
		};
		let line = match (&copy.name, copy.this, copy.backup) {
			(_, true, _) => {
				format!("on this device, an archive node, for {days} after it learns of the purge")
			}
			(Some(name), false, true) => {
				format!("on {whose}{name}, a backup, for {days} after its next update")
			}
			(None, false, true) => {
				format!("on {whose}backup, for {days} after its next update")
			}
			(Some(name), false, false) => format!(
				"on {whose}{name}, an archive node, for {days} after it learns of the purge"
			),
			(None, false, false) => {
				format!("on {whose}archive node, for {days} after it learns of the purge")
			}
		};
		writeln!(output, "waits     {line}")?;
	}
	if !copies.is_empty() {
		writeln!(
			output,
			"Until then, its owner can stop the purge there with nib purges, which brings it back everywhere, or end the wait."
		)?;
	}
	Ok(())
}

/// Prints what a purge takes away, or would: how many values of the field, of them how many
/// secret, from how many changes, and how many stored copies of the content they hold; then
/// the thumbnails made of that content, this device's checkouts of it and copies of trees'
/// files that hold it, deleted unless edited, and the originals that stay where they are.
pub(crate) fn purge(
	purge: &Purge,
	(field, name_of): (&FieldName, &dyn Fn(EntityId) -> Option<String>),
	elsewhere: &[String],
	done: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&purge_json(purge, field, done), output);
	}
	let secret = if purge.secret == 0 {
		String::new()
	} else {
		format!(", {} of them secret", purge.secret)
	};
	let blobs = if purge.blobs == 0 {
		String::new()
	} else {
		format!(
			", and {}",
			count(purge.blobs, "stored copy", "stored copies")
		)
	};
	writeln!(
		output,
		"{field}: {}{secret}, from {}{blobs}, {}",
		count(purge.statements.len(), "value", "values"),
		count(purge.changes, "change", "changes"),
		if done {
			"purged for good"
		} else {
			"would be purged for good"
		},
	)?;
	let kept = if done { "kept" } else { "would be kept" };
	for name in elsewhere {
		writeln!(
			output,
			"also in   {name}, a space that holds the same content, which it keeps"
		)?;
	}
	for &entity in &purge.also {
		let name = name_of(entity).unwrap_or_else(|| "(no name)".to_owned());
		writeln!(
			output,
			"also      {}  {name}, which holds the same content",
			short(entity)
		)?;
	}
	for &thumbnail in &purge.thumbnails {
		writeln!(
			output,
			"thumbnail {}, made of that content, {}",
			short(thumbnail),
			if done {
				"purged with it"
			} else {
				"would be purged with it"
			}
		)?;
	}
	let checkouts = purge.checkouts.iter().map(|copy| ("checkout", copy));
	let tree_copies = purge.tree_copies.iter().map(|copy| ("in tree ", copy));
	for (kind, copy) in checkouts.chain(tree_copies) {
		let what = match (copy.edited, done, copy.deleted) {
			(true, _, _) => format!("edited, {kept}"),
			(false, false, _) => "would be deleted".to_owned(),
			(false, true, true) => "deleted".to_owned(),
			(false, true, false) => "could not be deleted".to_owned(),
		};
		writeln!(output, "{kind}  {}, {what}", copy.path.display())?;
	}
	for (device, path) in &purge.originals {
		let device = name_of(*device).unwrap_or_else(|| short(*device));
		writeln!(output, "original  {path}, on {device}, {kept}")?;
	}
	waiting_copies(&purge.waiting, output)
}

/// Labels in JSON: an object from language tags to text.
fn labels_json(labels: &Labels) -> serde_json::Value {
	labels
		.iter()
		.map(|(tag, text)| (tag.to_owned(), serde_json::Value::from(text)))
		.collect::<serde_json::Map<_, _>>()
		.into()
}

/// Prints a type: its name, its label in the reader's languages if it has one, and its
/// version, then its fields, each with its kind, its marks, and its label.
pub(crate) fn type_definition(
	definition: &TypeDef,
	(languages, secret): (&Languages, &std::collections::BTreeSet<FieldName>),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let is_secret = |name: &FieldName, field: &nibkeep_model::FieldDef| {
		field.is_secret() || secret.contains(name)
	};
	if json {
		let fields: Vec<serde_json::Value> = definition
			.fields()
			.map(|(name, field)| {
				json!({
					"name": name.to_string(),
					"label": field.labels().best(languages),
					"labels": labels_json(field.labels()),
					"kind": kind_json(field.kind()),
					"multiplicity": multiplicity_json(field.multiplicity()),
					"secret": is_secret(name, field),
					"role": field.is_role(),
					"stands_for": field.is_stands_for(),
					"search_through": field.is_search_through(),
					"code": field.is_code(),
					"unit": field.unit(),
					"reverse": field.reverse().map(ToString::to_string),
				})
			})
			.collect();
		let value = json!({
			"name": definition.name().to_string(),
			"label": definition.labels().best(languages),
			"labels": labels_json(definition.labels()),
			"version": definition.version(),
			"fields": fields,
		});
		return print_json(&value, output);
	}
	match definition.labels().best(languages) {
		Some(label) => writeln!(
			output,
			"{} ({label}), version {}",
			definition.name(),
			definition.version()
		)?,
		None => writeln!(
			output,
			"{}, version {}",
			definition.name(),
			definition.version()
		)?,
	}
	let rows: Vec<(&str, String, Option<&str>)> = definition
		.fields()
		.map(|(name, field)| {
			let mut traits = vec![kind_text(field.kind())];
			if field.multiplicity() != Multiplicity::One {
				traits.push(multiplicity_text(field.multiplicity()));
			}
			if is_secret(name, field) {
				traits.push("secret".to_owned());
			}
			if field.is_role() {
				traits.push("role".to_owned());
			}
			if field.is_stands_for() {
				traits.push("stands for".to_owned());
			}
			if field.is_search_through() {
				traits.push("searched through".to_owned());
			}
			if field.is_code() {
				traits.push("code".to_owned());
			}
			if let Some(unit) = field.unit() {
				traits.push(format!("in {unit}"));
			}
			if let Some(reverse) = field.reverse() {
				traits.push(format!("reverse {reverse}"));
			}
			(
				name.as_str(),
				traits.join(", "),
				field.labels().best(languages),
			)
		})
		.collect();
	let width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
	let traits_width = rows
		.iter()
		.map(|(_, traits, _)| traits.chars().count())
		.max()
		.unwrap_or(0);
	for (name, traits, label) in rows {
		let line = match label {
			Some(label) => format!("  {name:<width$}  {traits:<traits_width$}  {label}"),
			None => format!("  {name:<width$}  {traits}"),
		};
		writeln!(output, "{}", line.trim_end())?;
	}
	Ok(())
}

/// Prints the types, with how many fields each has, and its label in the reader's
/// languages if it has one.
pub(crate) fn type_list(
	types: &Types,
	languages: &Languages,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let listed: Vec<serde_json::Value> = types
			.iter()
			.map(|definition| {
				json!({
					"name": definition.name().to_string(),
					"label": definition.labels().best(languages),
					"labels": labels_json(definition.labels()),
					"version": definition.version(),
					"fields": definition.fields().count(),
				})
			})
			.collect();
		return print_json(&serde_json::Value::Array(listed), output);
	}
	let rows: Vec<(&str, String, Option<&str>)> = types
		.iter()
		.map(|definition| {
			(
				definition.name().as_str(),
				count(definition.fields().count(), "field", "fields"),
				definition.labels().best(languages),
			)
		})
		.collect();
	let width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
	let count_width = rows
		.iter()
		.map(|(_, count, _)| count.len())
		.max()
		.unwrap_or(0);
	for (name, count, label) in rows {
		let line = format!(
			"{name:<width$}  {count:<count_width$}  {}",
			label.unwrap_or_default()
		);
		writeln!(output, "{}", line.trim_end())?;
	}
	Ok(())
}

/// An entity to print: as it is shown, what links to it, and the entities that were
/// recorded before it with the same participants, if it was just added.
pub(crate) struct Shown<'a> {
	pub(crate) view: &'a EntityView,
	/// The entities merged into it, by their own names.
	pub(crate) merged: &'a [Listed],
	/// The fields that its types make secret, whose values it does not show.
	pub(crate) secret: &'a [FieldName],
	pub(crate) links: &'a [Links],
	/// What follows `nib list --where FIELD=ID` to list the links left out, such as
	/// ` --on 2000-01-01`, or none if no command lists them.
	pub(crate) more: Option<&'a str>,
	pub(crate) recorded_before: &'a [EntityId],
	/// The thumbnail of a file's current content, if it has one.
	pub(crate) thumbnail: Option<&'a nibkeep_node::ThumbnailOf>,
}

/// Prints an entity: its name, ID and types, then its fields, and what links to it.
/// `name_of` names the entities it refers to.
pub(crate) fn entity(
	shown: &Shown<'_>,
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Shown {
		view,
		merged,
		secret,
		links,
		more,
		recorded_before,
		thumbnail,
	} = *shown;
	if json {
		let mut value = entity_json(view, links, recorded_before);
		if let Some(thumbnail) = thumbnail {
			value["thumbnail"] = thumbnail_json(thumbnail);
		}
		value["secret_fields"] = json!(secret.iter().map(ToString::to_string).collect::<Vec<_>>());
		value["merged"] = json!(
			merged
				.iter()
				.map(|listed| json!({ "id": listed.id.to_string(), "name": listed.name }))
				.collect::<Vec<_>>()
		);
		return print_json(&value, output);
	}
	writeln!(output, "{}", view.name.as_deref().unwrap_or("(no name)"))?;
	let types: Vec<String> = view.types.iter().map(ToString::to_string).collect();
	let mut lines = vec![
		("id".to_owned(), view.id.to_string()),
		("type".to_owned(), types.join(", ")),
	];
	if view.deleted {
		lines.push(("deleted".to_owned(), "yes".to_owned()));
	}
	if !merged.is_empty() {
		lines.push(("merged".to_owned(), listed_names(merged)));
	}
	if !secret.is_empty() {
		let names: Vec<String> = secret.iter().map(ToString::to_string).collect();
		lines.push(("secret".to_owned(), names.join(", ")));
	}
	for field in &view.fields {
		let values: Vec<String> = field
			.values
			.iter()
			.map(|shown| {
				let text = value_text(&shown.value, name_of);
				// "Date unknown" matters where the field's other values have dates.
				let dated = field.values.iter().any(|other| other.valid_time.is_some());
				let mut notes = Vec::new();
				if let Some(language) = &shown.language {
					notes.push(language.clone());
				}
				if let Some(valid_time) = &shown.valid_time {
					notes.push(valid_time.to_string());
				}
				if shown.unreadable_date {
					notes.push("unreadable date".to_owned());
				} else if shown.presence == Presence::Uncertain {
					notes.push("uncertain".to_owned());
				} else if shown.presence == Presence::DateUnknown && dated {
					notes.push("date unknown".to_owned());
				}
				let mut text = if notes.is_empty() {
					text
				} else {
					format!("{text} ({})", notes.join(", "))
				};
				// The value's other forms follow it, each in its language.
				for form in &shown.forms {
					text.push_str(" / ");
					text.push_str(&value_text(&form.value, name_of));
					if let Some(language) = &form.language {
						text.push_str(" (");
						text.push_str(language);
						text.push(')');
					}
				}
				text
			})
			.collect();
		let mut text = values.join("; ");
		if field.competing {
			text.push_str("  (competing)");
		}
		lines.push((field.field.to_string(), text));
	}
	if let Some(thumbnail) = thumbnail {
		lines.push(("thumbnail".to_owned(), thumbnail_text(thumbnail)));
	}
	print_lines(&lines, output)?;
	linked_from((view.id, links, more), recorded_before, name_of, output)
}

/// Prints the values of a secret field, one per line, or in JSON with the entity and the
/// field.
pub(crate) fn secret_values(
	entity: EntityId,
	field: &FieldName,
	values: &[ShownValue],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let values: Vec<serde_json::Value> = values
			.iter()
			.map(|shown| {
				json!({
					"statement": shown.statement.to_string(),
					"kind": kind_json(shown.value.kind()),
					"value": value_json(&shown.value),
				})
			})
			.collect();
		return print_json(
			&json!({ "id": entity.to_string(), "field": field.to_string(), "values": values }),
			output,
		);
	}
	for shown in values {
		writeln!(output, "{}", value_text(&shown.value, name_of))?;
	}
	Ok(())
}

/// What `nib protect` protected, and how.
pub(crate) fn protected(
	secrets: bool,
	passphrase: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"protected": if secrets { "secrets" } else { "library" },
				"by": if passphrase { "passphrase" } else { "keystore" },
			}),
			output,
		);
	}
	let what = if secrets {
		"The library's secrets are"
	} else {
		"The library's keys are"
	};
	let by = if passphrase {
		"a passphrase"
	} else {
		"the keystore"
	};
	writeln!(output, "{what} protected by {by} now.")?;
	Ok(())
}

/// That `nib sync --at-login on` turned syncing from login on: what became of the login
/// entry, and what `nib` did about the firewall.
pub(crate) fn at_login_on(
	entry: &at_login::EntryOutcome,
	firewall: Firewall,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	use at_login::EntryOutcome;
	if json {
		let (state, error) = match entry {
			EntryOutcome::NoEntry => (None, None),
			EntryOutcome::On(_) => (Some("on"), None),
			EntryOutcome::Off(_) => (Some("off"), None),
			EntryOutcome::Failed(error) => (Some("failed"), Some(error)),
		};
		let firewall = match firewall {
			Firewall::NotHere => None,
			Firewall::Open => Some("open"),
			Firewall::Opened => Some("opened"),
			Firewall::Refused => Some("refused"),
		};
		let mut value = json!({"at_login": true, "login_entry": state, "firewall": firewall});
		if let Some(error) = error {
			value["login_entry_error"] = json!(error);
		}
		return print_json(&value, output);
	}
	let now = "Syncing from login is on, and nibsync syncs this library now";
	match entry {
		EntryOutcome::On(_) => writeln!(
			output,
			"Syncing from login is on: nibsync syncs this library with your other devices whenever you are logged in here, with no app open."
		)?,
		EntryOutcome::Off(_) => writeln!(
			output,
			"{now}; but its login entry is turned off in the system's startup settings, so it does not start when you log in."
		)?,
		EntryOutcome::Failed(error) => writeln!(
			output,
			"{now}; but its login entry could not be made ({error}), so it does not start when you log in."
		)?,
		EntryOutcome::NoEntry => writeln!(
			output,
			"{now}, with no app open; no login entry starts it here when you log in."
		)?,
	}
	match firewall {
		Firewall::Opened => writeln!(
			output,
			"Windows' firewall lets your other devices reach this computer now, on private networks."
		)?,
		Firewall::Refused => writeln!(
			output,
			"Windows' firewall was not changed, so your other devices may not reach this computer; nib sync --at-login on asks again."
		)?,
		Firewall::NotHere | Firewall::Open => {}
	}
	Ok(())
}

/// That `nib sync --at-login off` turned syncing from login off, or that it was not on.
pub(crate) fn at_login_off(
	turned_off: &at_login::TurnedOff,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = json!({"at_login": false, "was": turned_off.removed});
		if let Some(error) = &turned_off.entry_failed {
			value["login_entry_error"] = json!(error);
		}
		return print_json(&value, output);
	}
	if turned_off.removed {
		writeln!(output, "Syncing from login is off for this library.")?;
	} else {
		writeln!(output, "Syncing from login was not on for this library.")?;
	}
	if let Some(error) = &turned_off.entry_failed {
		writeln!(
			output,
			"Its login entry could not be removed ({error}); nib sync --at-login off tries again."
		)?;
	}
	Ok(())
}

fn at_login_json(shown: &at_login::AtLoginStatus) -> serde_json::Value {
	let mut value = match &shown.serving {
		None => json!({"state": "not running"}),
		Some(serving) => {
			let mut value = json!({"state": serving.word()});
			if let Some(error) = serving.error() {
				value["error"] = json!(error);
			}
			value
		}
	};
	value["login_entry"] = json!(shown.entry_word());
	value
}

fn at_login_text(shown: &at_login::AtLoginStatus) -> String {
	let state = match &shown.serving {
		None => "but nibsync is not running; nib sync --at-login on starts it".to_owned(),
		Some(serving) => match serving {
			at_login::Serving::Starting => "and nibsync is opening it".to_owned(),
			at_login::Serving::Syncing => "and nibsync syncs this device".to_owned(),
			at_login::Serving::Waiting => {
				"and nibsync takes over when that program lets go".to_owned()
			}
			at_login::Serving::WaitingForKeyring => {
				"and nibsync waits for the keyring to be unlocked".to_owned()
			}
			at_login::Serving::NotHere => "but nibsync finds no replica of it here".to_owned(),
			at_login::Serving::NoKey => {
				"but nibsync finds no key for it in the keystore".to_owned()
			}
			at_login::Serving::Failed(error) => format!("but nibsync failed: {error}"),
		},
	};
	let entry = match &shown.entry {
		Some(Ok(at_login::EntryState::Off(_))) => {
			" Its login entry is turned off in the system's startup settings, so it does not start when you log in.".to_owned()
		}
		Some(Ok(at_login::EntryState::Missing)) => {
			" Its login entry is not there, so it does not start when you log in; nib sync --at-login on makes it again.".to_owned()
		}
		Some(Err(error)) => format!(" Its login entry cannot be read ({error})."),
		Some(Ok(at_login::EntryState::On(_))) if shown.entry_stale() => {
			" Its login entry starts another program than the copy that serves the list; nib sync --at-login on points it there.".to_owned()
		}
		None | Some(Ok(at_login::EntryState::On(_))) => String::new(),
	};
	format!("Syncing from login is on, {state}.{entry}")
}

/// A secret copied, as text, before `nib secret copy` waits to clear it.
pub(crate) fn copied(
	field: &FieldName,
	name: &str,
	values: usize,
	clear_after: u64,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	writeln!(
		output,
		"Copied {field} of {name}. The clipboard clears in {clear_after} s; Ctrl+C clears it now."
	)?;
	if values > 1 {
		writeln!(
			output,
			"It has {values} values that compete; this is the first. nib secret show shows them all."
		)?;
	}
	Ok(())
}

/// How a copy ended: whether the clipboard was cleared, or held something else, and
/// whether the person interrupted the wait.
pub(crate) fn copy_ended(
	(entity, field, values): (EntityId, &FieldName, usize),
	(cleared, interrupted): (bool, bool),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"id": entity.to_string(),
				"field": field.to_string(),
				"values": values,
				"cleared": cleared,
				"interrupted": interrupted,
			}),
			output,
		);
	}
	if cleared {
		writeln!(output, "Cleared the clipboard.")?;
	} else {
		writeln!(
			output,
			"The clipboard holds something else now, which stays."
		)?;
	}
	Ok(())
}

/// A generated password, and how strong it is.
pub(crate) fn generated(
	password: &str,
	bits: u32,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "password": password, "bits": bits }), output);
	}
	writeln!(output, "{password}")?;
	Ok(())
}

/// A one-time code at a moment, in seconds since 1970, and how long it lasts.
pub(crate) fn one_time_code(
	generator: &nibkeep_passwords::OneTimeCode,
	unix_seconds: u64,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let code = generator.code_at(unix_seconds);
	let left = generator.seconds_left(unix_seconds);
	if json {
		return print_json(
			&json!({
				"code": code.as_str(),
				"seconds_left": left,
				"period": generator.period(),
				"digits": generator.digits(),
				"issuer": generator.issuer(),
				"account": generator.account(),
			}),
			output,
		);
	}
	writeln!(output, "{} ({left} s left)", code.as_str())?;
	Ok(())
}

/// An entity in JSON, with what links to it and the entities recorded before it with the
/// same participants, none unless it was just added.
fn entity_json(
	view: &EntityView,
	links: &[Links],
	recorded_before: &[EntityId],
) -> serde_json::Value {
	let fields: Vec<serde_json::Value> = view
		.fields
		.iter()
		.map(|field| {
			let values: Vec<serde_json::Value> = field
				.values
				.iter()
				.map(|shown| {
					json!({
						"statement": shown.statement.to_string(),
						"kind": kind_json(shown.value.kind()),
						"value": value_json(&shown.value),
						"valid_time": shown.valid_time.as_ref().map(ToString::to_string),
						"presence": presence_json(shown),
						"language": shown.language,
						"other_forms": shown.other_forms,
						"forms": shown.forms.iter().map(|form| json!({
							"value": value_json(&form.value),
							"language": form.language,
						})).collect::<Vec<_>>(),
					})
				})
				.collect();
			json!({
				"name": field.field.to_string(),
				"competing": field.competing,
				"values": values,
			})
		})
		.collect();
	let types: Vec<String> = view.types.iter().map(ToString::to_string).collect();
	json!({
		"id": view.id.to_string(),
		"name": view.name,
		"types": types,
		"deleted": view.deleted,
		"fields": fields,
		"linked_from": links.iter().map(links_json).collect::<Vec<_>>(),
		"recorded_before": recorded_before.iter().map(ToString::to_string).collect::<Vec<_>>(),
	})
}

/// Prints what links to an entity, by the name that each field goes by from its other end,
/// and the entities recorded before it with the same participants.
fn linked_from(
	(view_id, links, command): (EntityId, &[Links], Option<&str>),
	recorded_before: &[EntityId],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if !links.is_empty() {
		writeln!(output, "Linked from")?;
		let rows: Vec<(String, String)> = links
			.iter()
			.map(|links| {
				let mut texts: Vec<String> = links
					.entities
					.iter()
					.map(|linking| linking_text(linking, name_of))
					.collect();
				let more = links.count.saturating_sub(links.entities.len());
				if more > 0 {
					// The command that lists them all, if one can.
					texts.push(match command {
						Some(flags) => format!(
							"and {more} more: nib list --where {}={}{flags}",
							links.field,
							short(view_id)
						),
						None => format!("and {more} more"),
					});
				}
				let field = links.reverse.as_ref().unwrap_or(&links.field);
				(field.to_string(), texts.join("; "))
			})
			.collect();
		print_lines(&rows, output)?;
	}
	if !recorded_before.is_empty() {
		let ids: Vec<String> = recorded_before.iter().map(|id| short(*id)).collect();
		writeln!(
			output,
			"Recorded before with the same participants: {}",
			ids.join(", ")
		)?;
	}
	Ok(())
}

/// Prints names and texts as two columns, indented.
fn print_lines(lines: &[(String, String)], output: &mut dyn Write) -> Result<(), CliError> {
	let width = lines.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
	for (name, text) in lines {
		writeln!(output, "  {name:<width$}  {text}")?;
	}
	Ok(())
}

/// An entity that links to another, for people: by its name, or a relationship by its name
/// or types, with its other participants and their roles.
fn linking_text(linking: &Linking, name_of: &dyn Fn(EntityId) -> Option<String>) -> String {
	let types: Vec<String> = linking.types.iter().map(ToString::to_string).collect();
	let label = linking.name.clone().unwrap_or_else(|| {
		if types.is_empty() {
			short(linking.id)
		} else {
			types.join(", ")
		}
	});
	if linking.participants.is_empty() {
		// An entity without a name or participants, such as where a file was found, goes
		// by its type and its ID, which finds it.
		return match (&linking.name, types.is_empty()) {
			(None, false) => format!("{label} {}", short(linking.id)),
			_ => label,
		};
	}
	let participants: Vec<String> = linking
		.participants
		.iter()
		.flat_map(|(role, entities)| {
			entities.iter().map(move |entity| {
				let name = name_of(*entity).unwrap_or_else(|| short(*entity));
				format!("{name} ({role})")
			})
		})
		.collect();
	format!("{label} with {}", participants.join(", "))
}

/// The entities that link to another through one field, in JSON.
fn links_json(links: &Links) -> serde_json::Value {
	let entities: Vec<serde_json::Value> = links
		.entities
		.iter()
		.map(|linking| {
			let participants: Vec<serde_json::Value> = linking
				.participants
				.iter()
				.map(|(role, entities)| {
					let entities: Vec<String> = entities.iter().map(ToString::to_string).collect();
					json!({ "role": role.to_string(), "entities": entities })
				})
				.collect();
			let types: Vec<String> = linking.types.iter().map(ToString::to_string).collect();
			json!({
				"id": linking.id.to_string(),
				"name": linking.name,
				"types": types,
				"participants": participants,
			})
		})
		.collect();
	json!({
		"field": links.field.to_string(),
		"reverse": links.reverse.as_ref().map(ToString::to_string),
		"role": links.role,
		"count": links.count,
		"entities": entities,
	})
}

/// Prints what an export wrote, and the files it could not, which this device does not
/// hold.
pub(crate) fn exported(
	exported: &Exported,
	folder: &std::path::Path,
	kind: ExportKind,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let missing: Vec<String> = exported
		.missing_files
		.iter()
		.map(ToString::to_string)
		.collect();
	if json {
		let value = json!({
			"directory": folder.display().to_string(),
			"history": kind == ExportKind::Full,
			"entities": exported.entities,
			"files": exported.files,
			"missing_files": missing,
		});
		return print_json(&value, output);
	}
	let holds = match kind {
		ExportKind::Full => ", with their history,",
		ExportKind::Current => " as they are now,",
	};
	writeln!(
		output,
		"Exported {} and {}{holds} to {}",
		count(exported.entities, "entity", "entities"),
		count(exported.files, "file", "files"),
		folder.display()
	)?;
	if !missing.is_empty() {
		writeln!(
			output,
			"Not on this device, so not exported: {}",
			missing.join(", ")
		)?;
	}
	Ok(())
}

/// Prints what an import brought in, and what it left out.
pub(crate) fn imported(
	imported: &Imported,
	folder: &std::path::Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let value = json!({
			"directory": folder.display().to_string(),
			"entities": imported.entities,
			"added": imported.added,
			"replaced": imported.replaced,
			"skipped": imported.skipped,
			"files": imported.files,
			"types_left_out": imported.types_left_out.iter().map(ToString::to_string).collect::<Vec<_>>(),
			"person": imported.person.as_ref().map(|person| json!({
				"id": person.id.to_string(),
				"name": person.name,
			})),
		});
		return print_json(&value, output);
	}
	write!(
		output,
		"Imported from {}: {} added and {} replaced, of {}, and {}; left out {} of users, devices, replicas and spaces",
		folder.display(),
		count(imported.added, "value", "values"),
		count(imported.replaced, "value", "values"),
		count(imported.entities, "entity", "entities"),
		count(imported.files, "file", "files"),
		count(imported.skipped, "record", "records"),
	)?;
	if !imported.types_left_out.is_empty() {
		let names: Vec<String> = imported
			.types_left_out
			.iter()
			.map(ToString::to_string)
			.collect();
		write!(
			output,
			", and the export's definitions of {}, which the library defines otherwise",
			names.join(", ")
		)?;
	}
	writeln!(output)?;
	if let Some(person) = &imported.person {
		writeln!(
			output,
			"You are {} now, the person of the export's user; the person you were, with only a name, is deleted.",
			shown(person.name.as_deref().unwrap_or("someone unnamed"))
		)?;
	}
	Ok(())
}

/// What `nib file add` did: the files it added or found again, those it could not add, and
/// how many it left out of the folders.
#[derive(Debug, Default)]
pub(crate) struct FilesAdded {
	pub(crate) added: Vec<(std::path::PathBuf, AddedFile)>,
	pub(crate) failed: Vec<(std::path::PathBuf, String)>,
	pub(crate) left_out: usize,
	/// The thumbnails made of what was added.
	pub(crate) thumbnails: nibkeep_node::ThumbnailsMade,
	/// Why recording thumbnails stopped, if an error of the library stopped it.
	pub(crate) thumbnails_failed: Option<String>,
}

/// What a Takeout import did, in JSON.
fn takeout_json(
	(report, source): (&nibkeep_import::TakeoutReport, &str),
	found: &FilesAdded,
) -> serde_json::Value {
	let failed: Vec<serde_json::Value> = report
		.failed
		.iter()
		.map(|(path, reason)| json!({ "path": path, "reason": reason }))
		.collect();
	json!({
		"source": source,
		"added": report.added,
		"found_again": report.found_again,
		"known": report.known,
		"undescribed": report.undescribed,
		"without_photo": report.without_photo,
		"albums": report.albums,
		"edits": report.edits,
		"copies": found.thumbnails.copies_linked,
		"linking_failed": found.thumbnails.linking_failed,
		"failed": failed,
		"left_out": report.left_out,
		"thumbnails": found.thumbnails.made,
		"thumbnails_failed": found.thumbnails_failed,
	})
}

/// Prints what a Takeout import did: how many photos and videos it added, found again or
/// knew from an earlier import, the albums it made folders of, what came without a JSON
/// file, the JSON files whose photos were missing, what failed, and the thumbnails made.
pub(crate) fn takeout_imported(
	(report, source): (&nibkeep_import::TakeoutReport, &str),
	found: &FilesAdded,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&takeout_json((report, source), found), output);
	}
	writeln!(
		output,
		"From {source}: {} added, {} found again that the library held, {} from an earlier import",
		count(report.added, "photo or video", "photos and videos"),
		report.found_again,
		report.known,
	)?;
	if report.albums > 0 {
		writeln!(
			output,
			"{}, inside the folder {source}",
			count(
				report.albums,
				"album became a folder",
				"albums became folders"
			)
		)?;
	}
	if report.edits > 0 {
		writeln!(
			output,
			"{} linked to {}",
			count(report.edits, "edited copy", "edited copies"),
			if report.edits == 1 {
				"its original"
			} else {
				"their originals"
			}
		)?;
	}
	if found.thumbnails.copies_linked > 0 {
		writeln!(
			output,
			"{} as copies of larger ones, resized or saved again, never merged",
			count(
				found.thumbnails.copies_linked,
				"photo linked",
				"photos linked"
			)
		)?;
	}
	if report.undescribed > 0 {
		writeln!(
			output,
			"{} came without a JSON file, with what they say of themselves",
			report.undescribed
		)?;
	}
	for path in &report.without_photo {
		writeln!(
			output,
			"no photo  {path}, a JSON file whose photo is in no part given"
		)?;
	}
	for (path, reason) in &report.failed {
		writeln!(output, "failed    {path}: {reason}")?;
	}
	if report.left_out > 0 {
		writeln!(
			output,
			"{} of other products than Google Photos left out",
			count(report.left_out, "file", "files")
		)?;
	}
	if found.thumbnails.made > 0 {
		writeln!(
			output,
			"{} made",
			count(found.thumbnails.made, "thumbnail", "thumbnails")
		)?;
	}
	if found.thumbnails.linking_failed {
		writeln!(output, "An error of the library stopped linking copies")?;
	}
	if let Some(why) = &found.thumbnails_failed {
		writeln!(output, "Thumbnails stopped: {why}")?;
	}
	Ok(())
}

/// What a file is, for people: a photo, a video, or a file.
fn file_kind(added: &AddedFile) -> &'static str {
	if added.types.contains(&nibkeep_std::PHOTO) {
		"photo"
	} else if added.types.contains(&nibkeep_std::VIDEO) {
		"video"
	} else {
		"file"
	}
}

/// Prints what came of adding a file: added; found again, in another place, as the file
/// it is; or known already, found there before.
pub(crate) fn file_added(
	path: &std::path::Path,
	added: &AddedFile,
	name_of: &dyn Fn(EntityId) -> Option<String>,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let what = match (added.known, added.origin) {
		(false, _) => "added",
		(true, Some(_)) => "again",
		(true, None) => "known",
	};
	let as_file = if added.known && added.origin.is_some() {
		let name = name_of(added.file).unwrap_or_else(|| "(no name)".to_owned());
		format!(", as {name}")
	} else {
		String::new()
	};
	let kept = if added.kept == Kept::InPlace {
		", kept where it is"
	} else {
		""
	};
	writeln!(
		output,
		"{what:6} {}  {:5}  {}{as_file}{kept}",
		short(added.file),
		file_kind(added),
		path.display()
	)?;
	Ok(())
}

/// The question whether a large file can be got again, if it is lost.
pub(crate) fn question(path: &std::path::Path, question: &Question) -> String {
	format!(
		"{} is {}. Can you get it again if it is lost? Yes keeps the files of {} where they are; no keeps a copy of each in NibKeep's store. [y/n] ",
		path.display(),
		amount_text(question.size),
		question.folder.display()
	)
}

/// A secret file added: where it was found, its name, and what adding it did.
pub(crate) type SecretFileAdded = (std::path::PathBuf, String, AddedSecretFile);

/// Prints that a secret file was added, and what it is attached to.
pub(crate) fn secret_file_added(
	path: &std::path::Path,
	added: &AddedSecretFile,
	(attached_to, name_of): (&[EntityId], &dyn Fn(EntityId) -> Option<String>),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let mut line = format!("secret {}  {}", short(added.file), path.display());
	if !attached_to.is_empty() {
		let names: Vec<String> = attached_to
			.iter()
			.map(|&entity| name_of(entity).unwrap_or_else(|| short(entity)))
			.collect();
		line.push_str(", attached to ");
		line.push_str(&names.join(", "));
	}
	if added.unprotected_copy {
		line.push_str(", also in the library as an ordinary file, which every device reads");
	}
	writeln!(output, "{line}")?;
	Ok(())
}

/// Prints what came of adding secret files: with `json`, each, without the hash of its
/// content, which is as secret as the content; otherwise how many, and that the originals
/// stay unprotected.
pub(crate) fn secret_files_added(
	(added, failed): (&[SecretFileAdded], &[(std::path::PathBuf, String)]),
	attached_to: &[EntityId],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let files: Vec<serde_json::Value> = added
			.iter()
			.map(|(path, name, added)| {
				json!({
					"path": path.display().to_string(),
					"id": added.file.to_string(),
					"name": name,
					"media_type": added.media_type,
					"size": added.size,
					"attached_to": attached_to.iter().map(ToString::to_string).collect::<Vec<_>>(),
					"unprotected_copy": added.unprotected_copy,
				})
			})
			.collect();
		let failed: Vec<serde_json::Value> = failed
			.iter()
			.map(|(path, error)| json!({ "path": path.display().to_string(), "error": error }))
			.collect();
		return print_json(&json!({ "secret_files": files, "failed": failed }), output);
	}
	let originals = if added.len() == 1 {
		"Its original is still where it was, unprotected."
	} else {
		"Their originals are still where they were, unprotected."
	};
	writeln!(
		output,
		"Added {}, sealed for the secrets. {originals}",
		count(added.len(), "secret file", "secret files")
	)?;
	Ok(())
}

/// Prints where a plain copy of a secret file was saved, and that nothing records it.
pub(crate) fn secret_saved(
	(file, name): (EntityId, Option<&str>),
	path: &std::path::Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "file": file.to_string(), "path": path.display().to_string() }),
			output,
		);
	}
	let name = name.map_or_else(|| short(file), str::to_owned);
	writeln!(
		output,
		"Saved a plain copy of {name} at {}. NibKeep neither protects it, nor keeps track of it, nor deletes it if the file is purged.",
		path.display()
	)?;
	Ok(())
}

/// Prints that a file could not be added, and why.
pub(crate) fn file_failed(
	path: &std::path::Path,
	error: &str,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	writeln!(output, "failed {}: {error}", path.display())?;
	Ok(())
}

/// Prints what came of checking out a file: its copy written, or there already.
pub(crate) fn checked_out(
	checked_out: &CheckedOut,
	name: Option<&str>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"file": checked_out.file.to_string(),
				"checkout": checked_out.checkout.to_string(),
				"path": checked_out.path.display().to_string(),
				"content": checked_out.content.to_string(),
				"written": checked_out.written,
			}),
			output,
		);
	}
	let name = name.map_or_else(|| short(checked_out.file), str::to_owned);
	if checked_out.written {
		writeln!(
			output,
			"Checked out {name} to {}",
			checked_out.path.display()
		)?;
	} else {
		writeln!(
			output,
			"{name} is checked out to {} already, unchanged",
			checked_out.path.display()
		)?;
	}
	Ok(())
}

/// What came of checking in a copy: the checkout, whether its copy was edited and the new
/// version that its edits made, and how it ended, unless it was kept; and the file's name.
pub(crate) struct CheckedIn<'a> {
	pub(crate) checkout: &'a Checkout,
	pub(crate) recorded: EditsRecorded,
	pub(crate) ended: Option<&'a Ended>,
	pub(crate) name: Option<&'a str>,
	/// Why the new version has no thumbnail, if an error of the library kept it from one.
	pub(crate) thumbnail_failed: Option<&'a str>,
}

/// Prints what came of checking in a copy.
pub(crate) fn checked_in(
	checked_in: &CheckedIn<'_>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let CheckedIn {
		checkout,
		recorded: EditsRecorded { copy, version },
		ended,
		name,
		thumbnail_failed,
	} = checked_in;
	if json {
		return print_json(
			&json!({
				"file": checkout.file.to_string(),
				"path": checkout.path.display().to_string(),
				"copy": copy.as_str(),
				"version": version.map(|version| version.to_string()),
				"kept": ended.is_none(),
				"deleted": ended.is_some_and(|ended| ended.deleted),
				"sealed": ended.is_some_and(|ended| ended.sealed),
				"thumbnail_failed": thumbnail_failed,
			}),
			output,
		);
	}
	let name = name.map_or_else(|| short(checkout.file), str::to_owned);
	let path = checkout.path.display();
	match (version, ended) {
		(Some(_), Some(_)) => writeln!(
			output,
			"Recorded the edits of {path} as a new version of {name}, and deleted the copy"
		)?,
		(Some(_), None) => writeln!(
			output,
			"Recorded the edits of {path} as a new version of {name}, and kept the copy"
		)?,
		(None, Some(ended)) if ended.deleted => {
			let sealed = if ended.sealed {
				", after sealing it into the store, as no other copy on this device held it"
			} else {
				""
			};
			writeln!(
				output,
				"Deleted the copy of {name} at {path}, which was unchanged{sealed}"
			)?;
		}
		(None, Some(_)) => writeln!(
			output,
			"The copy of {name} at {path} was gone; its checkout ended"
		)?,
		(None, None) => writeln!(
			output,
			"The copy of {name} at {path} is {}, with no edits to record",
			copy.as_str()
		)?,
	}
	if let Some(error) = thumbnail_failed {
		writeln!(
			output,
			"The new version has no thumbnail: {error}; nib file thumbnails makes it"
		)?;
	}
	Ok(())
}

/// What making thumbnails did, in JSON.
fn thumbnails_json(made: nibkeep_node::ThumbnailsMade) -> serde_json::Value {
	json!({
		"made": made.made,
		"unreadable": made.unreadable,
		"not_here": made.not_here,
		"copies_linked": made.copies_linked,
		"linking_failed": made.linking_failed,
	})
}

/// Prints what `nib file thumbnails` made, and how many pictures still lack one.
pub(crate) fn thumbnails_made(
	made: nibkeep_node::ThumbnailsMade,
	left: usize,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = thumbnails_json(made);
		value["left"] = json!(left);
		return print_json(&value, output);
	}
	let mut parts = vec![format!(
		"Made {}",
		count(made.made, "thumbnail", "thumbnails")
	)];
	if made.unreadable > 0 {
		parts.push(format!(
			"could not read {}",
			count(made.unreadable, "picture", "pictures")
		));
	}
	if made.not_here > 0 {
		parts.push(format!(
			"{} not on this device",
			count(made.not_here, "picture is", "pictures are")
		));
	}
	if made.copies_linked > 0 {
		parts.push(format!(
			"linked {} as copies of larger ones",
			count(made.copies_linked, "photo", "photos")
		));
	}
	if made.linking_failed {
		parts.push("an error of the library stopped linking copies".to_owned());
	}
	writeln!(output, "{}", parts.join(", "))?;
	if left > 0 {
		writeln!(
			output,
			"{} of the formats nib reads still {} none",
			count(left, "picture", "pictures"),
			if left == 1 { "has" } else { "have" }
		)?;
	}
	Ok(())
}

/// A thumbnail, for people: its size and how it was made, such as `512×384 pixels, made
/// with nibkeep 1`.
fn thumbnail_text(thumbnail: &nibkeep_node::ThumbnailOf) -> String {
	let mut parts = Vec::new();
	if let Some((width, height)) = thumbnail.size {
		parts.push(format!("{width}×{height} pixels"));
	}
	if let Some(made_with) = &thumbnail.made_with {
		parts.push(format!("made with {made_with}"));
	}
	if !thumbnail.held {
		parts.push("not on this device yet".to_owned());
	}
	parts.push(short(thumbnail.entity));
	parts.join(", ")
}

/// A thumbnail in JSON.
fn thumbnail_json(thumbnail: &nibkeep_node::ThumbnailOf) -> serde_json::Value {
	json!({
		"id": thumbnail.entity.to_string(),
		"content": thumbnail.content.to_string(),
		"width": thumbnail.size.map(|(width, _)| width),
		"height": thumbnail.size.map(|(_, height)| height),
		"made_with": thumbnail.made_with,
		"held": thumbnail.held,
	})
}

/// Prints where `nib file thumbnail` wrote a thumbnail.
pub(crate) fn thumbnail_written(
	thumbnail: &nibkeep_node::ThumbnailOf,
	path: &std::path::Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = thumbnail_json(thumbnail);
		value["path"] = json!(path.display().to_string());
		return print_json(&value, output);
	}
	writeln!(
		output,
		"Wrote the thumbnail, {}, to {}",
		thumbnail_text(thumbnail),
		path.display()
	)?;
	Ok(())
}

/// What came of `nib file add`, in JSON: every file, what failed, and the thumbnails.
fn files_added_json(found: &FilesAdded) -> serde_json::Value {
	let files: Vec<serde_json::Value> = found
		.added
		.iter()
		.map(|(path, added)| {
			json!({
				"path": path.display().to_string(),
				"id": added.file.to_string(),
				"known": added.known,
				"origin": added.origin.map(|origin| origin.to_string()),
				"content": added.content.to_string(),
				"types": added.types.iter().map(ToString::to_string).collect::<Vec<_>>(),
				"kept": added.kept.as_str(),
				"unasked": added.unasked,
			})
		})
		.collect();
	let failed: Vec<serde_json::Value> = found
		.failed
		.iter()
		.map(|(path, error)| json!({ "path": path.display().to_string(), "error": error }))
		.collect();
	json!({
		"files": files,
		"failed": failed,
		"left_out": found.left_out,
		"thumbnails": thumbnails_json(found.thumbnails),
		"thumbnails_failed": found.thumbnails_failed,
	})
}

/// Prints what came of `nib file add`: in JSON, every file, and otherwise, how many of
/// each, the lines for each file having been printed as they came.
pub(crate) fn files_added(
	found: &FilesAdded,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&files_added_json(found), output);
	}
	let added = found.added.iter().filter(|(_, added)| !added.known).count();
	let again = found
		.added
		.iter()
		.filter(|(_, added)| added.known && added.origin.is_some())
		.count();
	let known = found.added.len() - added - again;
	let mut parts = vec![format!("added {}", count(added, "file", "files"))];
	if again > 0 {
		parts.push(format!("found {again} again in other places"));
	}
	if known > 0 {
		parts.push(format!("knew {known} already"));
	}
	let thumbnails = found.thumbnails;
	if thumbnails.made > 0 {
		parts.push(format!(
			"made {}",
			count(thumbnails.made, "thumbnail", "thumbnails")
		));
	}
	if thumbnails.unreadable > 0 {
		parts.push(format!(
			"could not read {} for thumbnails",
			count(thumbnails.unreadable, "picture", "pictures")
		));
	}
	if thumbnails.copies_linked > 0 {
		parts.push(format!(
			"linked {} as copies of larger ones",
			count(thumbnails.copies_linked, "photo", "photos")
		));
	}
	if thumbnails.linking_failed {
		parts.push("an error of the library stopped linking copies".to_owned());
	}
	if found.left_out > 0 {
		parts.push(format!(
			"left out {} of folders: links, hidden files and folders, checkouts, and the library's own",
			count(found.left_out, "item", "items")
		));
	}
	let in_place = found
		.added
		.iter()
		.filter(|(_, added)| added.kept == Kept::InPlace)
		.count();
	if in_place > 0 {
		parts.push(format!(
			"kept {in_place} where {} found, as {} policy says",
			if in_place == 1 { "it was" } else { "they were" },
			if in_place == 1 { "its" } else { "their" }
		));
	}
	let mut summary = parts.join(", ");
	if let Some(first) = summary.get_mut(..1) {
		first.make_ascii_uppercase();
	}
	writeln!(output, "{summary}")?;
	if let Some(error) = &found.thumbnails_failed {
		writeln!(
			output,
			"Thumbnails stopped: {error}; nib file thumbnails makes those that files lack"
		)?;
	}
	let unasked = found
		.added
		.iter()
		.filter(|(_, added)| added.unasked)
		.count();
	if unasked > 0 {
		writeln!(
			output,
			"{} went into the store without asking whether {} can be got again, as nobody answered; nib policy rule new Replaceable --folder FOLDER keeps the files of a folder where they are",
			count(unasked, "large file", "large files"),
			if unasked == 1 { "it" } else { "they" }
		)?;
	}
	Ok(())
}

/// What a policy asks, for people, such as `2 copies, 1 on an archive node, in the store`.
fn needs_text(needs: &Needs) -> String {
	let mut parts = vec![count(needs.copies as usize, "copy", "copies")];
	if needs.away > 0 {
		parts.push(format!("{} outside the home", needs.away));
	}
	if needs.archived > 0 {
		parts.push(format!("{} on an archive node", needs.archived));
	}
	parts.push(
		match needs.kept {
			Kept::Store => "in the store",
			Kept::InPlace => "kept where it is",
		}
		.to_owned(),
	);
	parts.join(", ")
}

/// What a policy asks, in JSON.
fn needs_json(needs: &Needs) -> serde_json::Value {
	json!({
		"copies": needs.copies,
		"away": needs.away,
		"archived": needs.archived,
		"kept": needs.kept.as_str(),
	})
}

/// A size for people: in the largest binary or decimal unit that it is a whole number of,
/// such as `1 GiB` or `500 MB`, or else in bytes.
pub(crate) fn size_text(bytes: u64) -> String {
	for (unit, size) in [
		("TiB", 1_u64 << 40),
		("GiB", 1 << 30),
		("MiB", 1 << 20),
		("TB", 1_000_000_000_000),
		("GB", 1_000_000_000),
		("MB", 1_000_000),
		("KiB", 1 << 10),
		("kB", 1_000),
	] {
		if bytes >= size && bytes.is_multiple_of(size) {
			return format!("{} {unit}", bytes / size);
		}
	}
	count(
		usize::try_from(bytes).unwrap_or(usize::MAX),
		"byte",
		"bytes",
	)
}

/// A rule's conditions for people, such as `a file with std.camera-make`, or `any other
/// file` for a rule without conditions.
fn conditions_text(rule: &Rule, name_of: &dyn Fn(EntityId) -> Option<String>) -> String {
	let conditions = &rule.conditions;
	if conditions.is_empty() {
		return "any other file".to_owned();
	}
	let mut parts = Vec::new();
	if let Some(name) = &conditions.of_type {
		parts.push(format!("of type {name}"));
	}
	if let Some(field) = &conditions.has {
		parts.push(format!("with {field}"));
	}
	if let Some(folder) = &conditions.folder {
		let device = name_of(folder.device).unwrap_or_else(|| short(folder.device));
		parts.push(format!("found in {} on {device}", folder.path));
	}
	if let Some(media_type) = &conditions.media_type {
		parts.push(format!("of media type {media_type}"));
	}
	if let Some(bytes) = conditions.larger_than {
		parts.push(format!("larger than {}", size_text(bytes)));
	}
	format!("a file {}", parts.join(", "))
}

/// A rule in JSON.
fn rule_json(number: usize, rule: &Rule) -> serde_json::Value {
	let conditions = &rule.conditions;
	json!({
		"number": number,
		"id": rule.id.to_string(),
		"policy": rule.policy.map(|policy| policy.to_string()),
		"type": conditions.of_type.as_ref().map(ToString::to_string),
		"has": conditions.has.as_ref().map(ToString::to_string),
		"folder": conditions.folder.as_ref().map(|folder| folder.path.clone()),
		"device": conditions.folder.as_ref().map(|folder| folder.device.to_string()),
		"media_type": conditions.media_type,
		"larger_than": conditions.larger_than,
		"asks_above": rule.asks_above,
	})
}

/// A rule for people, on one line: its number, its conditions and the policy it chooses.
fn rule_line(
	number: usize,
	rule: &Rule,
	policies: &[Policy],
	name_of: &dyn Fn(EntityId) -> Option<String>,
) -> String {
	let policy = rule
		.policy
		.and_then(|id| policies.iter().find(|policy| policy.id == id))
		.map_or_else(|| "no policy: it chooses nothing".to_owned(), policy_name);
	let asks = rule.asks_above.map_or_else(String::new, |bytes| {
		format!(", asking above {}", size_text(bytes))
	});
	format!(
		"{number}. {}: {policy}{asks}",
		conditions_text(rule, name_of)
	)
}

/// A policy's name, or its short ID if it has none.
fn policy_name(policy: &Policy) -> String {
	policy.name.clone().unwrap_or_else(|| short(policy.id))
}

/// A policy in JSON.
fn policy_json(policy: &Policy) -> serde_json::Value {
	json!({
		"id": policy.id.to_string(),
		"name": policy.name,
		"needs": needs_json(&policy.needs),
	})
}

/// Prints the policies of a space, and its rules in the order in which they choose.
pub(crate) fn policies(
	policies: &[Policy],
	rules: &[Rule],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let value = json!({
			"policies": policies.iter().map(policy_json).collect::<Vec<_>>(),
			"rules": rules.iter().enumerate().map(|(index, rule)| rule_json(index + 1, rule)).collect::<Vec<_>>(),
		});
		return print_json(&value, output);
	}
	let width = policies
		.iter()
		.map(|policy| policy_name(policy).chars().count())
		.max()
		.unwrap_or(0);
	writeln!(output, "Policies:")?;
	for policy in policies {
		writeln!(
			output,
			"  {:width$}  {}",
			policy_name(policy),
			needs_text(&policy.needs)
		)?;
	}
	writeln!(output, "Rules, in the order in which they choose:")?;
	for (index, rule) in rules.iter().enumerate() {
		writeln!(
			output,
			"  {}",
			rule_line(index + 1, rule, policies, name_of)
		)?;
	}
	if rules.is_empty() {
		writeln!(
			output,
			"  none: every file gets 2 copies, 1 on an archive node, in the store"
		)?;
	}
	Ok(())
}

/// Prints a policy, and the rules that choose it.
pub(crate) fn policy(
	id: EntityId,
	policies: &[Policy],
	rules: &[Rule],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Some(policy) = policies.iter().find(|policy| policy.id == id) else {
		return Ok(());
	};
	let choosing: Vec<(usize, &Rule)> = rules
		.iter()
		.enumerate()
		.filter(|(_, rule)| rule.policy == Some(id))
		.map(|(index, rule)| (index + 1, rule))
		.collect();
	if json {
		let mut value = policy_json(policy);
		value["rules"] = choosing
			.iter()
			.map(|(number, rule)| rule_json(*number, rule))
			.collect();
		return print_json(&value, output);
	}
	writeln!(output, "{}  {}", short(policy.id), policy_name(policy))?;
	writeln!(output, "  {}", needs_text(&policy.needs))?;
	if choosing.is_empty() {
		writeln!(output, "  No rule chooses it.")?;
	}
	for (number, rule) in choosing {
		writeln!(output, "  {}", rule_line(number, rule, policies, name_of))?;
	}
	Ok(())
}

/// Prints the policy chosen for a file, and what chose it.
pub(crate) fn why(
	choice: &Choice,
	policies: &[Policy],
	rules: &[Rule],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let policy = choice
		.policy
		.and_then(|id| policies.iter().find(|policy| policy.id == id));
	let rule = match choice.by {
		ChosenBy::Rule(id) => rules
			.iter()
			.enumerate()
			.find(|(_, rule)| rule.id == id)
			.map(|(index, rule)| (index + 1, rule)),
		_ => None,
	};
	if json {
		let by = match choice.by {
			ChosenBy::Own => json!("own"),
			ChosenBy::Rule(id) => {
				json!({ "rule": id.to_string(), "number": rule.map(|(number, _)| number) })
			}
			ChosenBy::Fallback => json!("none"),
		};
		let value = json!({
			"policy": policy.map(policy_json),
			"needs": needs_json(&choice.needs),
			"by": by,
		});
		return print_json(&value, output);
	}
	let name = policy.map_or_else(|| "No policy".to_owned(), policy_name);
	let by = match (choice.by, rule) {
		(ChosenBy::Own, _) => "its own".to_owned(),
		(ChosenBy::Rule(_), Some((number, rule))) => {
			format!("by rule {}", rule_line(number, rule, policies, name_of))
		}
		(ChosenBy::Rule(id), None) => format!("by rule {}", short(id)),
		(ChosenBy::Fallback, _) => "no rule chose one".to_owned(),
	};
	writeln!(output, "{name}, {by}")?;
	writeln!(output, "  {}", needs_text(&choice.needs))?;
	Ok(())
}

/// Prints, for each policy, how many files it has and which lack copies.
pub(crate) fn policy_check(
	check: &PolicyCheck,
	(name_of, zone): (&dyn Fn(EntityId) -> Option<String>, &jiff::tz::TimeZone),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&policy_check_json(check), output);
	}
	if check.policies.is_empty() {
		writeln!(output, "No files yet.")?;
	}
	let names = |files: &[EntityId]| {
		files
			.iter()
			.map(|&file| name_of(file).unwrap_or_else(|| short(file)))
			.collect::<Vec<_>>()
			.join(", ")
	};
	for count in &check.policies {
		let name = count.name.clone().unwrap_or_else(|| "No policy".to_owned());
		let short_of = if count.short == 0 {
			"every one has its copies".to_owned()
		} else if count.short == count.files {
			"each lacks copies".to_owned()
		} else {
			format!("{} lack copies", count.short)
		};
		writeln!(
			output,
			"{name}: {}, {short_of}; it needs {}",
			count_files(count.files),
			needs_text(&count.needs)
		)?;
		let mut lacking = Vec::new();
		if count.few_copies > 0 {
			lacking.push(format!("{} on too few disks", count.few_copies));
		}
		if count.few_away > 0 {
			lacking.push(format!("{} with too few outside the home", count.few_away));
		}
		if count.few_archived > 0 {
			lacking.push(format!(
				"{} with too few on an archive node",
				count.few_archived
			));
		}
		if !lacking.is_empty() {
			writeln!(output, "  {}", lacking.join("; "))?;
		}
		if count.unchecked > 0 {
			writeln!(
				output,
				"  {} a copy not checked in 8 weeks, which does not count",
				if count.unchecked == 1 {
					"1 has".to_owned()
				} else {
					format!("{} have", count.unchecked)
				}
			)?;
		}
		if !count.none_here.is_empty() {
			writeln!(
				output,
				"  No copy on this device: {}",
				names(&count.none_here)
			)?;
		}
		if !count.not_stored.is_empty() {
			writeln!(
				output,
				"  Kept only where they were found, though the policy keeps them in the store; nib file add stores them: {}",
				names(&count.not_stored)
			)?;
		}
	}
	disks_and_changes(check, zone, output)
}

/// What `nib policy check` prints in JSON.
fn policy_check_json(check: &PolicyCheck) -> serde_json::Value {
	let ids = |files: &[EntityId]| files.iter().map(ToString::to_string).collect::<Vec<_>>();
	let policies: Vec<serde_json::Value> = check
		.policies
		.iter()
		.map(|count| {
			json!({
				"policy": count.policy.map(|id| id.to_string()),
				"name": count.name,
				"needs": needs_json(&count.needs),
				"files": count.files,
				"short": count.short,
				"few_copies": count.few_copies,
				"few_away": count.few_away,
				"few_archived": count.few_archived,
				"unchecked": count.unchecked,
				"none_here": ids(&count.none_here),
				"not_stored": ids(&count.not_stored),
			})
		})
		.collect();
	let changes = &check.changes;
	json!({
		"policies": policies,
		"disks": check.disks.iter().map(disk_json).collect::<Vec<_>>(),
		"changes": {
			"needs": needs_json(&changes.needs),
			"disks": changes.disks,
			"away": changes.away,
			"archived": changes.archived,
			"here_alone": changes.here_alone,
			"here_alone_more": changes.here_alone_more,
		},
	})
}

/// Prints how the space's changes are held, and the disks that hold copies.
fn disks_and_changes(
	check: &PolicyCheck,
	zone: &jiff::tz::TimeZone,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let changes = &check.changes;
	let away = match changes.away {
		0 => "none".to_owned(),
		away => away.to_string(),
	};
	writeln!(
		output,
		"The space's changes: on {}, {away} outside the home; Critical asks for {}",
		count(changes.disks, "disk", "disks"),
		needs_text(&changes.needs)
	)?;
	// With no other disk, the line above says it all.
	if changes.here_alone > 0 && check.disks.len() > 1 {
		writeln!(
			output,
			"  {}{} only on this disk, until another device syncs",
			changes_text(changes.here_alone),
			if changes.here_alone_more {
				" or more"
			} else {
				""
			}
		)?;
	}
	writeln!(output, "Disks:")?;
	for disk in &check.disks {
		let told = disk
			.issued
			.filter(|_| !disk.here)
			.map_or_default(|at| format!(", as told {}", moment_text(at, zone)));
		let which = if disk.here { "this disk: " } else { "" };
		writeln!(output, "  {which}{}{told}", disk_text(disk))?;
		if disk.damaged > 0 || disk.unrepaired > 0 {
			writeln!(output, "    {}", damage_text(disk))?;
		}
	}
	let wanted = check
		.policies
		.iter()
		.any(|count| count.needs.archived > 0 && count.files > 0);
	if wanted && !check.disks.iter().any(|disk| disk.archive) {
		writeln!(
			output,
			"No device is an archive node: nib device mark DEVICE --archive marks one."
		)?;
	}
	Ok(())
}

/// What the checks of the replicas on a disk found damaged, for people: a disk that loses
/// data may be failing.
fn damage_text(disk: &Disk) -> String {
	let found = match disk.damaged {
		1 => "1 copy".to_owned(),
		damaged => format!("{damaged} copies"),
	};
	let still = match disk.unrepaired {
		0 => String::new(),
		unrepaired if unrepaired == disk.damaged => ", all damaged still".to_owned(),
		unrepaired => format!(", {unrepaired} damaged still"),
	};
	format!("checks found {found} damaged in the last 4 weeks{still}: the disk may be failing")
}

/// A replica on a disk, for people: its app on its device, and whose it is if another
/// user's.
fn disk_replica_text(replica: &DiskReplica) -> String {
	let app = replica
		.name
		.clone()
		.unwrap_or_else(|| "a replica".to_owned());
	let mut text = match &replica.device {
		Some(device) => format!("{app} on {device}"),
		None => app,
	};
	if let Some(member) = &replica.member {
		text = format!("{member}'s {text}");
	}
	if replica.this {
		text.push_str(" (this one)");
	}
	text
}

/// A disk, for people: the replicas and backups on it whose accounts came, and how it is
/// marked.
fn disk_text(disk: &Disk) -> String {
	let backups = disk.backups.iter().map(|backup| {
		backup.name.as_ref().map_or_else(
			|| "a backup".to_owned(),
			|name| format!("the backup {name}"),
		)
	});
	let named: Vec<String> = disk
		.replicas
		.iter()
		.map(disk_replica_text)
		.chain(backups)
		.collect();
	let who = if named.is_empty() {
		"a device's disk".to_owned()
	} else {
		named.join(", ")
	};
	let mut marks = Vec::new();
	if disk.archive {
		marks.push("an archive node");
	}
	if disk.away {
		marks.push("away from home");
	}
	if marks.is_empty() {
		who
	} else {
		format!("{who}; {}", marks.join(", "))
	}
}

/// A disk, in JSON.
fn disk_json(disk: &Disk) -> serde_json::Value {
	json!({
		"id": disk.id.to_string(),
		"here": disk.here,
		"archive": disk.archive,
		"away": disk.away,
		"issued": disk.issued.map(moment_json),
		"damaged": disk.damaged,
		"unrepaired": disk.unrepaired,
		"replicas": disk.replicas.iter().map(|replica| json!({
			"replica": replica.replica.map(|id| id.to_string()),
			"name": replica.name,
			"device_name": replica.device,
			"member": replica.member,
			"this": replica.this,
		})).collect::<Vec<_>>(),
		"backups": disk.backups.iter().map(|backup| json!({
			"backup": backup.backup.to_string(),
			"name": backup.name,
			"update": backup.update,
		})).collect::<Vec<_>>(),
	})
}

/// Prints the disks that hold a copy of a file, and how each holds it.
pub(crate) fn file_copies(
	(copies, name): (&[FileCopy], Option<&str>),
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let listed: Vec<serde_json::Value> = copies
			.iter()
			.map(|copy| {
				let (held, checked) = match copy.held {
					CopyHeld::Sealed { checked } => ("sealed", checked),
					CopyHeld::InPlace { checked } => ("in place", checked),
				};
				json!({
					"disk": disk_json(&copy.disk),
					"held": held,
					"checked": checked.map(moment_json),
					"counts": copy.counts,
				})
			})
			.collect();
		return print_json(&json!({ "copies": listed }), output);
	}
	let name = name.unwrap_or("The file");
	if copies.is_empty() {
		writeln!(
			output,
			"{name}: no copy on any disk that this device knows of"
		)?;
		return Ok(());
	}
	writeln!(
		output,
		"{name}: on {}",
		count(copies.len(), "disk", "disks")
	)?;
	for copy in copies {
		let (kind, checked) = match copy.held {
			CopyHeld::Sealed { checked } => ("sealed", checked),
			CopyHeld::InPlace { checked } => ("a plain copy, unchanged", checked),
		};
		let checked = match checked {
			Some(checked) => format!("checked {}", moment_text(checked, zone)),
			None => "never checked".to_owned(),
		};
		let counts = if copy.counts {
			""
		} else {
			", so it does not count"
		};
		let which = if copy.disk.here { "this disk: " } else { "" };
		writeln!(
			output,
			"  {which}{}: {kind}, {checked}{counts}",
			disk_text(&copy.disk)
		)?;
	}
	Ok(())
}

/// Prints how a device is marked now.
pub(crate) fn device_marked(
	(name, device): (Option<&str>, EntityId),
	(marks, takes, said): (DeviceMarks, Option<bool>, bool),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"device": device.to_string(),
				"device_name": name,
				"role": marks.role.as_str(),
				"away": marks.away,
				"keeps": marks.keeps,
				"waits": marks.waits,
				"waits_days": nibkeep_node::wait_days(&marks),
				"takes_new_spaces": takes,
			}),
			output,
		);
	}
	let role = match marks.role {
		DeviceRole::Archive => "an archive node",
		DeviceRole::Everyday => "an everyday device",
	};
	let place = if marks.away {
		"away from home"
	} else {
		"at home"
	};
	// A size set shows; an archive node keeps every file whatever it is.
	let keeps = match (marks.role, marks.keeps) {
		(DeviceRole::Everyday, Some(bytes)) => {
			format!(", keeps up to {} of files", amount_text(bytes))
		}
		_ => String::new(),
	};
	// An archive node waits before it destroys what a purge destroys, unless marked not to.
	let waits = match (marks.role, nibkeep_node::wait_days(&marks)) {
		(DeviceRole::Everyday, _) => String::new(),
		(DeviceRole::Archive, 0) => ", purges at once".to_owned(),
		(DeviceRole::Archive, days) => format!(
			", waits {} before purging",
			count(usize::try_from(days).unwrap_or(usize::MAX), "day", "days")
		),
	};
	// Shown on the device itself, which alone says it: where it asks first, as a phone
	// does, or the person just said so.
	let new_spaces = match takes {
		Some(true) if said => ", takes new spaces",
		Some(false) => ", asks before taking new spaces",
		_ => "",
	};
	writeln!(
		output,
		"{}: {role}, {place}{keeps}{waits}{new_spaces}",
		name.map_or_else(|| short(device), str::to_owned)
	)?;
	Ok(())
}

/// Prints that a file was pinned to a device, or unpinned from it.
pub(crate) fn pinned(
	(file, name): (EntityId, Option<&str>),
	(device, device_name): (EntityId, Option<&str>),
	pinned: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"file": file.to_string(),
				"name": name,
				"device": device.to_string(),
				"device_name": device_name,
				"pinned": pinned,
			}),
			output,
		);
	}
	let file = name.map_or_else(|| short(file), str::to_owned);
	let device = device_name.map_or_else(|| short(device), str::to_owned);
	if pinned {
		writeln!(
			output,
			"Pinned {file} to {device}, which keeps it whatever its size"
		)?;
	} else {
		writeln!(output, "Unpinned {file} from {device}")?;
	}
	Ok(())
}

/// A number of files, for people.
fn count_files(files: usize) -> String {
	count(files, "file", "files")
}

/// Prints a policy that a command added or changed.
pub(crate) fn policy_changed(
	what: &str,
	policy: &Policy,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&policy_json(policy), output);
	}
	writeln!(
		output,
		"{what} {}: {}",
		policy_name(policy),
		needs_text(&policy.needs)
	)?;
	Ok(())
}

/// Prints a rule that a command added or moved, with its number now.
pub(crate) fn rule_changed(
	what: &str,
	id: EntityId,
	rules: &[Rule],
	policies: &[Policy],
	name_of: &dyn Fn(EntityId) -> Option<String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let Some((index, rule)) = rules.iter().enumerate().find(|(_, rule)| rule.id == id) else {
		return Ok(());
	};
	if json {
		return print_json(&rule_json(index + 1, rule), output);
	}
	writeln!(
		output,
		"{what} rule {}",
		rule_line(index + 1, rule, policies, name_of)
	)?;
	Ok(())
}

/// Prints that a policy was removed.
pub(crate) fn removed_policy(
	id: EntityId,
	name: Option<&str>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "id": id.to_string(), "name": name }), output);
	}
	writeln!(
		output,
		"Removed {}",
		name.map_or_else(|| short(id), str::to_owned)
	)?;
	Ok(())
}

/// Prints that a rule was removed.
pub(crate) fn removed_rule(
	id: EntityId,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({ "id": id.to_string() }), output);
	}
	writeln!(output, "Removed rule {}", short(id))?;
	Ok(())
}

/// Prints how an export compares with the library: what it holds, or what is wrong.
pub(crate) fn export_check(
	check: &ExportCheck,
	folder: &std::path::Path,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let problems: Vec<serde_json::Value> =
			check.problems.iter().map(export_problem_json).collect();
		let value = json!({
			"directory": folder.display().to_string(),
			"space": check.space.map(|space| space.to_string()),
			"as_of": check.as_of,
			"entities": check.entities,
			"files": check.files,
			"sound": check.problems.is_empty(),
			"problems": problems,
		});
		return print_json(&value, output);
	}
	if check.problems.is_empty() {
		writeln!(
			output,
			"Export in {}: {} and {}, as the library held them at {}",
			folder.display(),
			count(check.entities, "entity", "entities"),
			count(check.files, "file", "files"),
			check.as_of.as_deref().unwrap_or("its start")
		)?;
		return Ok(());
	}
	writeln!(
		output,
		"Export in {}: {}",
		folder.display(),
		count(check.problems.len(), "problem", "problems")
	)?;
	for problem in &check.problems {
		writeln!(output, "  {}", export_problem_text(problem))?;
	}
	Ok(())
}

/// A problem of an export, for people.
fn export_problem_text(problem: &ExportProblem) -> String {
	match problem {
		ExportProblem::NotAnExport(why) => format!("it is no export: {why}"),
		ExportProblem::OtherSpace(space) => {
			format!("it is of the space {space}, which this library does not have")
		}
		ExportProblem::WrongSum(path) => format!("{path}: its sum differs"),
		ExportProblem::Unlisted(path) => format!("{path}: no sum lists it"),
		ExportProblem::MissingEntity(entity) => format!("entity {entity}: missing from the export"),
		ExportProblem::ExtraEntity(entity) => format!("entity {entity}: not in the library"),
		ExportProblem::DifferentEntity(entity) => {
			format!("entity {entity}: its line differs from the library's")
		}
		ExportProblem::DifferentChanges => {
			"changes.jsonl: it differs from the library's changes".to_owned()
		}
		ExportProblem::MissingFile(hash) => format!("file {hash}: missing from the export"),
		ExportProblem::DifferentFile(hash) => format!("file {hash}: its content differs"),
		ExportProblem::DamagedLine(number) => format!("entities.jsonl, line {number}: no entity"),
		ExportProblem::ExtraFile(path) => format!("{path}: no value refers to it"),
		ExportProblem::WrongManifest(count) => {
			format!("manifest.json: its count of {count} is wrong")
		}
		_ => "a problem this version does not name".to_owned(),
	}
}

/// A problem of an export, in JSON.
fn export_problem_json(problem: &ExportProblem) -> serde_json::Value {
	match problem {
		ExportProblem::NotAnExport(why) => json!({ "problem": "not an export", "why": why }),
		ExportProblem::OtherSpace(space) => {
			json!({ "problem": "other space", "space": space.to_string() })
		}
		ExportProblem::WrongSum(path) => json!({ "problem": "wrong sum", "path": path }),
		ExportProblem::Unlisted(path) => json!({ "problem": "unlisted", "path": path }),
		ExportProblem::MissingEntity(entity) => {
			json!({ "problem": "missing entity", "entity": entity.to_string() })
		}
		ExportProblem::ExtraEntity(entity) => {
			json!({ "problem": "extra entity", "entity": entity.to_string() })
		}
		ExportProblem::DifferentEntity(entity) => {
			json!({ "problem": "different entity", "entity": entity.to_string() })
		}
		ExportProblem::DifferentChanges => json!({ "problem": "different changes" }),
		ExportProblem::MissingFile(hash) => {
			json!({ "problem": "missing file", "file": hash.to_string() })
		}
		ExportProblem::DifferentFile(hash) => {
			json!({ "problem": "different file", "file": hash.to_string() })
		}
		ExportProblem::DamagedLine(number) => json!({ "problem": "damaged line", "line": number }),
		ExportProblem::ExtraFile(path) => json!({ "problem": "extra file", "path": path }),
		ExportProblem::WrongManifest(count) => {
			json!({ "problem": "wrong manifest", "count": count })
		}
		_ => json!({ "problem": "unknown" }),
	}
}

/// A moment in milliseconds since 1970, for people: its date and time to the minute in a
/// time zone.
pub(crate) fn moment_text(millis: u64, zone: &jiff::tz::TimeZone) -> String {
	i64::try_from(millis)
		.ok()
		.and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
		.map_or_else(
			|| format!("{millis} ms"),
			|timestamp| {
				timestamp
					.to_zoned(zone.clone())
					.strftime("%Y-%m-%d %H:%M")
					.to_string()
			},
		)
}

/// A moment in milliseconds since 1970, in JSON: in RFC 3339, in UTC.
fn moment_json(millis: u64) -> serde_json::Value {
	i64::try_from(millis)
		.ok()
		.and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
		.map_or_else(|| json!(millis), |timestamp| json!(timestamp.to_string()))
}

/// Prints every value an entity has had, by field: when each was recorded and by whom, the
/// value, and when it was replaced, in the order they were recorded.
pub(crate) fn history(
	history: &History,
	name_of: &dyn Fn(EntityId) -> Option<String>,
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let who = |value: &PastValue| {
		let Some(author) = &value.author else {
			return "unknown".to_owned();
		};
		let user = author
			.user
			.name
			.clone()
			.unwrap_or_else(|| "unknown".to_owned());
		match &author.ai_agent {
			Some(ai_agent) => format!("{user}, through {ai_agent}"),
			None => user,
		}
	};
	if json {
		let fields: Vec<serde_json::Value> = history
			.fields
			.iter()
			.map(|field| {
				let values: Vec<serde_json::Value> = field
					.values
					.iter()
					.map(|value| {
						json!({
							"statement": value.statement.to_string(),
							"kind": kind_json(value.value.kind()),
							"value": value_json(&value.value),
							"valid_time": value.valid_time.as_ref().map(ToString::to_string),
							"language": value.language,
							"recorded": moment_json(value.recorded.millis()),
							"restated": value.restated.map(|at| moment_json(at.millis())),
							"replaced": value.replaced.map(|at| moment_json(at.millis())),
							"author": value.author.as_ref().map(author_json),
							"carried": value.carried.as_ref().map(|carried| json!({
								"from": carried.from.map(|space| space.to_string()),
								"from_name": carried.from_name,
								"author": carried.author.as_ref().map(author_json),
								"recorded": carried.recorded.map(moment_json),
								"replaced": carried.replaced.map(moment_json),
							})),
						})
					})
					.collect();
				json!({ "name": field.field.to_string(), "values": values })
			})
			.collect();
		let value = json!({
			"id": history.id.to_string(),
			"name": history.name,
			"fields": fields,
		});
		return print_json(&value, output);
	}
	writeln!(output, "{}", history.name.as_deref().unwrap_or("(no name)"))?;
	let width = history
		.fields
		.iter()
		.flat_map(|field| &field.values)
		.map(|value| who(value).chars().count())
		.max()
		.unwrap_or(0);
	for field in &history.fields {
		writeln!(output, "  {}", field.field)?;
		for value in &field.values {
			let notes = past_value_notes(value, zone);
			let notes = if notes.is_empty() {
				String::new()
			} else {
				format!("  ({})", notes.join(", "))
			};
			writeln!(
				output,
				"    {}  {:<width$}  {}{notes}",
				moment_text(value.recorded.millis(), zone),
				who(value),
				value_text(&value.value, name_of),
			)?;
		}
	}
	Ok(())
}

/// What `nib history` notes of a past value: its language, its valid time, a purge that
/// stated it again, a move that carried it, and when it was replaced.
fn past_value_notes(value: &PastValue, zone: &jiff::tz::TimeZone) -> Vec<String> {
	let mut notes = Vec::new();
	if let Some(language) = &value.language {
		notes.push(language.clone());
	}
	if let Some(valid_time) = &value.valid_time {
		notes.push(valid_time.to_string());
	}
	if let Some(restated) = value.restated {
		notes.push(format!(
			"stated again by a purge {}",
			moment_text(restated.millis(), zone)
		));
	}
	// A value carried replaced was replaced where it came from, as the mover noted;
	// one carried current may be replaced here since.
	if let Some(carried) = &value.carried {
		notes.push(carried_text(carried, zone));
	}
	if let Some(replaced) = value.replaced
		&& value
			.carried
			.as_ref()
			.is_none_or(|carried| carried.replaced.is_none())
	{
		notes.push(format!("replaced {}", moment_text(replaced.millis(), zone)));
	}
	notes
}

/// An author in JSON: the user, by ID and name, the replica, and the AI agent, if one wrote.
fn author_json(author: &nibkeep_node::Author) -> serde_json::Value {
	json!({
		"user": author.user.id.to_string(),
		"name": author.user.name,
		"replica": author.replica.to_string(),
		"ai_agent": author.ai_agent,
	})
}

/// What a move noted of a value it carried: the space it came from, when and by whom that
/// space recorded it, and when it was replaced there.
fn carried_text(carried: &nibkeep_node::Carried, zone: &jiff::tz::TimeZone) -> String {
	use std::fmt::Write as _;
	let from = carried
		.from_name
		.clone()
		.or_else(|| carried.from.map(|space| space.to_string()))
		.unwrap_or_else(|| "another space".to_owned());
	let mut text = format!("carried from {from}");
	if let Some(recorded) = carried.recorded {
		let _ = write!(text, ", recorded there {}", moment_text(recorded, zone));
	}
	if let Some(name) = carried
		.author
		.as_ref()
		.and_then(|author| author.user.name.as_deref())
	{
		let _ = write!(text, " by {name}");
	}
	if let Some(replaced) = carried.replaced {
		let _ = write!(text, ", replaced there {}", moment_text(replaced, zone));
	}
	text
}

/// Prints the ranked languages in use, and those of the user, this device and this app.
pub(crate) fn languages(
	settings: &LanguageSettings,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let text = |languages: Option<&nibkeep_model::Languages>| -> Option<String> {
		languages
			.filter(|languages| !languages.is_empty())
			.map(ToString::to_string)
	};
	let in_use = settings.in_use();
	if json {
		let value = json!({
			"in_use": text(Some(&in_use)),
			"user": text(settings.user.as_ref()),
			"device": text(settings.device.as_ref()),
			"replica": text(settings.replica.as_ref()),
		});
		return print_json(&value, output);
	}
	for (whose, languages) in [
		("In use", Some(&in_use)),
		("Yours", settings.user.as_ref()),
		("This device's", settings.device.as_ref()),
		("This app's", settings.replica.as_ref()),
	] {
		let languages = text(languages).unwrap_or_else(|| "none".to_owned());
		writeln!(output, "{whose}: {languages}")?;
	}
	Ok(())
}

/// Prints a list of entities: a short ID, the name and the types of each, and of those
/// that a search found through a link, the entity it went through, which the list holds.
pub(crate) fn entities(
	listed: &[Listed],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let entities: Vec<serde_json::Value> = listed.iter().map(listed_json).collect();
		return print_json(&serde_json::Value::Array(entities), output);
	}
	let name_of = |id: EntityId| {
		listed
			.iter()
			.find(|entity| entity.id == id)
			.and_then(|entity| entity.name.clone())
			.unwrap_or_else(|| short(id))
	};
	let width = listed
		.iter()
		.map(|entity| entity.name.as_deref().unwrap_or("(no name)").len())
		.max()
		.unwrap_or(0);
	for entity in listed {
		let types: Vec<String> = entity.types.iter().map(ToString::to_string).collect();
		let via = entity
			.via
			.map_or_default(|via| format!("  (via {})", name_of(via)));
		writeln!(
			output,
			"{}  {:<width$}  {}{via}",
			short(entity.id),
			entity.name.as_deref().unwrap_or("(no name)"),
			types.join(", ")
		)?;
	}
	Ok(())
}

/// The entities listed in a space, under the space's name.
type SpaceEntities = (Option<String>, Vec<Listed>);

/// Prints the entities found or listed in each space. With one space, as [`entities`] does;
/// with several, under each space's name, and in JSON, each with its space's name.
pub(crate) fn entities_in_spaces(
	(groups, missed): (&[SpaceEntities], &[String]),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if let [(_, listed)] = groups {
		entities(listed, json, output)?;
		return if json {
			Ok(())
		} else {
			not_looked_in(missed, output)
		};
	}
	if json {
		let entities: Vec<serde_json::Value> = groups
			.iter()
			.flat_map(|(space, listed)| {
				listed.iter().map(move |entity| {
					let mut entity = listed_json(entity);
					entity["space"] = json!(space);
					entity
				})
			})
			.collect();
		return print_json(&serde_json::Value::Array(entities), output);
	}
	let mut first = true;
	for (space, listed) in groups.iter().filter(|(_, listed)| !listed.is_empty()) {
		if !first {
			writeln!(output)?;
		}
		first = false;
		writeln!(output, "{}:", space.as_deref().unwrap_or("A space"))?;
		entities(listed, false, output)?;
	}
	not_looked_in(missed, output)
}

/// Says which of the person's spaces a list or a search did not look in, as this device
/// does not hold them, if any.
fn not_looked_in(missed: &[String], output: &mut dyn Write) -> Result<(), CliError> {
	if missed.is_empty() {
		return Ok(());
	}
	let names: Vec<String> = missed.iter().map(|name| shown(name)).collect();
	writeln!(
		output,
		"Not looked in, as not on this device: {}.",
		names.join(", ")
	)?;
	Ok(())
}

/// An entity listed, in JSON: its ID, name, types, and the entity it was found through.
fn listed_json(entity: &Listed) -> serde_json::Value {
	let types: Vec<String> = entity.types.iter().map(ToString::to_string).collect();
	json!({
		"id": entity.id.to_string(),
		"name": entity.name,
		"types": types,
		"via": entity.via.map(|via| via.to_string()),
	})
}

/// Prints what `nib find` found in each space: the first entities, as [`entities_in_spaces`]
/// prints them, and how many more there are, then the person's spaces it did not look in,
/// `missed`. In JSON, an object: the entities, how many more there are, `more`, whether
/// there are at least that many, `at_least`, as those found through links were not
/// counted, and the spaces not looked in, `not_looked_in`.
pub(crate) fn found_in_spaces(
	(groups, missed): (&[(Option<String>, Searched)], &[String]),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut entities = Vec::new();
		let (mut more, mut at_least) = (0, false);
		for (space, found) in groups {
			for entity in &found.listed {
				let mut entity = listed_json(entity);
				if groups.len() > 1 {
					entity["space"] = json!(space);
				}
				entities.push(entity);
			}
			more += found.more.count();
			at_least |= matches!(found.more, More::AtLeast(_));
		}
		return print_json(
			&json!({
				"entities": entities,
				"more": more,
				"at_least": at_least,
				"not_looked_in": missed,
			}),
			output,
		);
	}
	let mut first = true;
	for (space, found) in groups {
		if found.listed.is_empty() && found.more.count() == 0 {
			continue;
		}
		if groups.len() > 1 {
			if !first {
				writeln!(output)?;
			}
			writeln!(output, "{}:", space.as_deref().unwrap_or("A space"))?;
		}
		first = false;
		entities(&found.listed, false, output)?;
		let more = match found.more {
			More::Exactly(0) => continue,
			More::Exactly(count) => format!("{count} more"),
			More::AtLeast(count) => format!("At least {count} more"),
		};
		writeln!(
			output,
			"{more}: add words to narrow it, or list them all with --all."
		)?;
	}
	not_looked_in(missed, output)
}

/// The entities that a name could be, for an error: one per line, indented, by short ID,
/// name and types, and how many more there are, of `count` in all.
pub(crate) fn candidates(listed: &[Listed], count: usize) -> String {
	let width = listed
		.iter()
		.map(|entity| {
			entity
				.name
				.as_deref()
				.unwrap_or("(no name)")
				.chars()
				.count()
		})
		.max()
		.unwrap_or(0);
	let mut lines: Vec<String> = listed
		.iter()
		.map(|entity| {
			let types: Vec<String> = entity.types.iter().map(ToString::to_string).collect();
			let name = entity.name.as_deref().unwrap_or("(no name)");
			let pad = width - name.chars().count();
			format!(
				"  {}  {name}{}  {}",
				short(entity.id),
				" ".repeat(pad),
				types.join(", ")
			)
		})
		.collect();
	if count > listed.len() {
		lines.push(format!("  and {} more", count - listed.len()));
	}
	lines.join("\n")
}

/// Entities for people, each by its name and short ID, such as `D. Abraham (1a2b3c4d)`.
pub(crate) fn listed_names(listed: &[Listed]) -> String {
	listed
		.iter()
		.map(|listed| {
			format!(
				"{} ({})",
				listed.name.as_deref().unwrap_or("no name"),
				short(listed.id)
			)
		})
		.collect::<Vec<_>>()
		.join("; ")
}

/// The first eight characters of an ID.
pub(crate) fn short(id: EntityId) -> String {
	id.to_string()[..8].to_owned()
}

/// A value, for people. References are shown by the name of what they refer to.
pub(crate) fn value_text(value: &Value, name_of: &dyn Fn(EntityId) -> Option<String>) -> String {
	match value {
		Value::Text(text) => text.clone(),
		Value::Integer(integer) => integer.to_string(),
		Value::Boolean(true) => "yes".to_owned(),
		Value::Boolean(false) => "no".to_owned(),
		Value::Date(date) => date.to_string(),
		Value::Reference(entity) => name_of(*entity).unwrap_or_else(|| short(*entity)),
		Value::Blob(hash) => hash.to_string(),
		Value::Decimal(decimal) => decimal.to_string(),
		Value::Quantity(quantity) => quantity.to_string(),
		Value::Name(name) => name.full().to_owned(),
		Value::Unknown(_) => "(unreadable)".to_owned(),
	}
}

/// A value in JSON. Integers that many JSON readers cannot hold exactly are strings.
fn value_json(value: &Value) -> serde_json::Value {
	const EXACT: i64 = (1 << 53) - 1;
	match value {
		Value::Text(text) => json!(text),
		Value::Integer(integer) if (-EXACT..=EXACT).contains(integer) => json!(integer),
		Value::Integer(integer) => json!(integer.to_string()),
		Value::Boolean(boolean) => json!(boolean),
		Value::Date(date) => json!(date.to_string()),
		Value::Reference(entity) => json!(entity.to_string()),
		Value::Blob(hash) => json!(hash.to_string()),
		// Decimals are strings that keep their precision, as the encoding's JSON form says.
		Value::Decimal(decimal) => json!(decimal.to_string()),
		Value::Quantity(quantity) => json!({
			"number": quantity.number().to_string(),
			"unit": quantity.unit(),
		}),
		Value::Name(name) => {
			let parts: serde_json::Map<String, serde_json::Value> = name
				.parts()
				.map(|(part, text)| (part.to_owned(), json!(text)))
				.collect();
			serde_json::Value::Object(parts)
		}
		Value::Unknown(_) => serde_json::Value::Null,
	}
}

/// Prints JSON, indented, with a new line at the end, in one write: standard output is
/// line-buffered, so writing it as it is made would write each line on its own, which
/// takes longer than making it.
pub(crate) fn print_json(
	value: &serde_json::Value,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let mut text = serde_json::to_vec_pretty(value).map_err(std::io::Error::from)?;
	text.push(b'\n');
	output.write_all(&text)?;
	Ok(())
}

/// A count with its noun, such as "1 change" or "3 changes".
pub(crate) fn count(count: usize, one: &str, many: &str) -> String {
	format!("{count} {}", if count == 1 { one } else { many })
}

/// How certainly a value shows on the day shown, in JSON: `yes`, `uncertain`, `no`, or
/// `date unknown` for one without a date that can be read.
fn presence_json(shown: &ShownValue) -> &'static str {
	match shown.presence {
		Presence::Yes => "yes",
		Presence::Uncertain => "uncertain",
		Presence::No => "no",
		Presence::DateUnknown => "date unknown",
	}
}

/// Prints the code that pairs a new device, before waiting for it.
pub(crate) fn pairing_code(
	code: &nibkeep_node::PairingCode,
	minutes: u64,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		print_json(
			&json!({ "code": code.to_string(), "minutes": minutes }),
			output,
		)?;
	} else {
		writeln!(
			output,
			"On the new device, in the folder for the library, run:\n\n    nib init --join {code}\n\nThe code works for {}; {} wrong tries end it. Both devices must be on the same network. Waiting…",
			count(
				usize::try_from(minutes).unwrap_or(usize::MAX),
				"minute",
				"minutes"
			),
			nibkeep_node::CODE_TRIES,
		)?;
	}
	output.flush()?;
	Ok(())
}

/// Tells, while a code waits on, of a try that failed before the code matched, such as a
/// wrong code that a device typed, and how many tries the code has left.
pub(crate) fn wrong_try(
	error: &nibkeep_node::NodeError,
	left: usize,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let wrong_code = error.is_wrong_code();
	let tries = count(left, "try", "tries");
	if json {
		print_json(
			&json!({
				"failed_try": error.to_string(),
				"wrong_code": wrong_code,
				"tries_left": left,
			}),
			output,
		)?;
	} else if wrong_code {
		writeln!(output, "A device typed a wrong code: {tries} left.")?;
	} else {
		writeln!(output, "A device's try failed, as {error}: {tries} left.")?;
	}
	output.flush()?;
	Ok(())
}

/// Warns, for people only, when the port that other devices connect to was taken, as by
/// another nib, so that this one takes their connections on another, which a firewall may
/// block.
pub(crate) fn port_taken(
	network: &nibkeep_node::Network,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if let Some((asked, why)) = network.port_refused()
		// One of the ports from the one asked for, which one firewall rule lets through.
		&& !network.port().is_some_and(|port| {
			(asked..asked.saturating_add(nibkeep_node::PORTS)).contains(&port)
		}) {
		let port = network
			.port()
			.map_or_else(|| "another port".to_owned(), |port| format!("port {port}"));
		progress(
			&format!(
				"UDP port {asked} could not be used ({why}), maybe as another nib has it; this one takes connections on {port}, which a firewall may block."
			),
			json,
			output,
		)?;
	}
	Ok(())
}

/// Prints a line of progress, for people only.
pub(crate) fn progress(line: &str, json: bool, output: &mut dyn Write) -> Result<(), CliError> {
	if !json {
		writeln!(output, "{line}")?;
		output.flush()?;
	}
	Ok(())
}

/// What `nib recovery restore` did: the spaces that came back, by the names in `spaces`, and
/// those that did not, the devices it revoked, those too that only the spaces named in
/// `elsewhere` listed, and the backup it updated, with what to do next. The library's status
/// follows.
pub(crate) fn recovered(
	recovered: &nibkeep_node::Recovered,
	(spaces, elsewhere, old_size): (&[String], &[String], u64),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let (devices, kept) = recovered_devices(recovered);
	let brought = recovered_from(&recovered.from, old_size);
	let from_backup = matches!(recovered.from, nibkeep_node::RecoveredFrom::Backup { .. });
	let source_name = if from_backup { "backup" } else { "library" };
	let came = if spaces.is_empty() {
		String::new()
	} else {
		format!(" Your spaces came back too: {}.", shown_list(spaces))
	};
	let missing = match recovered.missing.len() {
		0 => String::new(),
		1 => format!(" One space of yours was not in that {source_name}, so it did not come back."),
		count => format!(
			" {count} spaces of yours were not in that {source_name}, so they did not come back."
		),
	};
	let revoked = if devices.is_empty() {
		String::new()
	} else {
		format!(
			" Revoked your other devices, which you lost: {}.",
			devices.join(", ")
		)
	};
	let elsewhere = if elsewhere.is_empty() {
		String::new()
	} else {
		format!(
			" Also revoked, in {}, devices of yours that a revoked device had added there, which the {source_name} did not list among your devices.",
			shown_list(elsewhere)
		)
	};
	let out_of_reach = recovered_out_of_reach(&recovered.out_of_reach);
	let kept = kept_text(&kept, !devices.is_empty(), &recovered.from);
	let secrets = match recovered.secrets {
		nibkeep_node::RecoveredSecrets::NoneHeld => "",
		nibkeep_node::RecoveredSecrets::NotInCode if from_backup => {
			" Your secrets did not come back, as your code was made without them: they went with your lost devices."
		}
		nibkeep_node::RecoveredSecrets::NotInCode => {
			" Your secrets did not come back, as your code was made without them: they stay sealed in the old library's folder, and go with it."
		}
		nibkeep_node::RecoveredSecrets::Taken { .. } => {
			" Your secrets came back too, under the passphrase you gave them here."
		}
		nibkeep_node::RecoveredSecrets::Left => {
			" Your secrets were left, as you chose: this device holds none."
		}
	};
	let line = format!(
		"{brought}{came}{missing}{secrets}{revoked}{elsewhere}{out_of_reach}{kept} Your recovery code still works, so keep it safe, or replace it: nib recovery new."
	);
	progress(&line, json, output)
}

/// The spaces where a revocation also revoked devices that a revoked device added, and those
/// that it cannot reach from this device, which does not hold them: those that another device
/// of the person's holds, which revokes there once it syncs with this one, and those that none
/// holds, where the revoked device stays.
fn revoked_elsewhere(
	(confirmed, elsewhere): (bool, &[(nibkeep_base::SpaceId, String)]),
	out_of_reach: &[nibkeep_node::HeldElsewhere],
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if !elsewhere.is_empty() {
		let names: Vec<String> = elsewhere.iter().map(|(_, name)| name.clone()).collect();
		writeln!(
			output,
			"Also revoked, in {}, devices of yours that a revoked device had added there and \
			 that none of your devices had heard of: nothing tells them from devices that \
			 someone else paired on it.",
			shown_list(&names)
		)?;
	}
	let (held, lost): (Vec<_>, Vec<_>) = out_of_reach
		.iter()
		.partition(|each| !each.holders.is_empty());
	let (is, can, keeps) = if confirmed {
		("is", "can", "keeps")
	} else {
		("would be", "could", "would keep")
	};
	if !held.is_empty() {
		writeln!(
			output,
			"Not on this device, so it {is} revoked there once a device of yours that holds it \
			 syncs with this one: {}.",
			elsewhere_with_holders(&held)
		)?;
	}
	if !lost.is_empty() {
		writeln!(
			output,
			"On no other device of yours, so none {can} revoke it there: {}. It {keeps} what it \
			 holds there, and where others share the space, your place, until an admin \
			 removes you and invites you again.",
			elsewhere_names(&lost)
		)?;
	}
	Ok(())
}

/// The spaces that the lost devices held and that did not come back: those that a kept
/// device holds, which revokes the lost ones there once it syncs with this one, and those
/// that only the lost devices held, which stay with them.
fn recovered_out_of_reach(out_of_reach: &[nibkeep_node::HeldElsewhere]) -> String {
	let (held, lost): (Vec<_>, Vec<_>) = out_of_reach
		.iter()
		.partition(|each| !each.holders.is_empty());
	let held = if held.is_empty() {
		String::new()
	} else {
		format!(
			" Not on this device, so your lost devices are revoked there once a device of yours that holds it syncs with this one: {}.",
			elsewhere_with_holders(&held)
		)
	};
	let lost = if lost.is_empty() {
		String::new()
	} else {
		format!(
			" Only your lost devices held {}, so they keep it.",
			elsewhere_names(&lost)
		)
	};
	held + &lost
}

/// Names that others may have chosen, each shown safely, as [`named_list`] joins them.
fn shown_list(names: &[String]) -> String {
	let shown: Vec<String> = names.iter().map(|name| shown(name)).collect();
	named_list(&shown)
}

/// A device of the person's that a recovery would revoke, by its replicas there, all of one
/// device, or one AI agent's replica: its name, and its apps, such as `phone (nib,
/// passwords)`.
pub(crate) fn kept_device_text(replicas: &[Replica]) -> String {
	match replicas {
		[replica] if replica.ai_agent => replica_text(replica),
		[first, ..] => {
			let mut apps: Vec<&str> = replicas
				.iter()
				.map(|replica| replica.name.as_deref().unwrap_or("an app"))
				.collect();
			apps.sort_unstable();
			apps.dedup();
			let device = first.device_name.as_deref().unwrap_or("a device");
			format!("{} ({})", shown(device), shown(&apps.join(", ")))
		}
		[] => String::new(),
	}
}

/// The question whether a recovery keeps a device of the person's, which `device` names, as
/// [`kept_device_text`] does.
pub(crate) fn keep_question(device: &str) -> String {
	format!(
		"Keep {device}? Only if you still have it, and its library still unlocks there; otherwise it is revoked as lost. [y/N] "
	)
}

/// The names of the devices that a recovery revoked, but the replica of a library that no
/// longer unlocked, which is named apart, and of those it kept: each by its device's name,
/// but by its app and device where one device has replicas both revoked and kept.
fn recovered_devices(recovered: &nibkeep_node::Recovered) -> (Vec<String>, Vec<String>) {
	let source_replica = match &recovered.from {
		nibkeep_node::RecoveredFrom::Library { replica, .. } => *replica,
		nibkeep_node::RecoveredFrom::Backup { .. } => None,
	};
	let devices_of = |replicas: &[Replica]| -> std::collections::BTreeSet<EntityId> {
		replicas
			.iter()
			.filter_map(|replica| replica.device)
			.collect()
	};
	let split: Vec<EntityId> = devices_of(&recovered.revoked)
		.intersection(&devices_of(&recovered.kept))
		.copied()
		.collect();
	let names = |replicas: &mut dyn Iterator<Item = &Replica>| -> Vec<String> {
		let mut names: Vec<String> = replicas
			.map(|replica| {
				if replica.device.is_some_and(|device| split.contains(&device)) {
					replica_text(replica)
				} else {
					shown(
						&replica
							.device_name
							.clone()
							.or_else(|| replica.name.clone())
							.unwrap_or_else(|| replica.id.to_string()),
					)
				}
			})
			.collect();
		names.sort();
		names.dedup();
		names
	};
	let revoked = names(
		&mut recovered
			.revoked
			.iter()
			.filter(|replica| Some(replica.id) != source_replica),
	);
	(revoked, names(&mut recovered.kept.iter()))
}

/// What `nib recovery restore` says of the devices that it kept, by the names in `kept`, after
/// recovering as `from` says, and revoking others if `revoked`: they learn of this one, and
/// of the revocations, as this one introduces itself in the syncs that it starts, which reach
/// them while they serve, or from a backup that carries them.
fn kept_text(kept: &[String], revoked: bool, from: &nibkeep_node::RecoveredFrom) -> String {
	if kept.is_empty() {
		return String::new();
	}
	let (it, learns, them, serve) = if kept.len() == 1 {
		("it", "learns", "it", "serves")
	} else {
		("they", "learn", "them", "serve")
	};
	let revocations = if revoked {
		", and of the revocations,"
	} else {
		""
	};
	let or = match from {
		nibkeep_node::RecoveredFrom::Backup {
			name: Some(backup),
			updated: true,
		} => format!("run nib backup update \"{backup}\" there"),
		_ => "update there a backup that carries this device".to_owned(),
	};
	format!(
		" Kept {}, which you still have: {it} {learns} of this device{revocations} the first time this device syncs with {them}: run nib sync here while {it} {serve}, as nib sync --serve, syncing from login or an app does; or {or}.",
		named_list(kept)
	)
}

/// What `nib recovery restore` says it brought the library back from, with the room that the
/// old library's folder takes, if it was a library.
fn recovered_from(from: &nibkeep_node::RecoveredFrom, old_size: u64) -> String {
	match from {
		nibkeep_node::RecoveredFrom::Backup {
			name: Some(backup),
			updated: true,
		} => format!("Brought your library back from the backup {backup}, and updated it."),
		nibkeep_node::RecoveredFrom::Backup {
			name: Some(backup),
			updated: false,
		} => format!(
			"Brought your library back from the backup {backup}, but could not update it, so it does not carry this device yet: nib backup update \"{backup}\"."
		),
		nibkeep_node::RecoveredFrom::Backup { name: None, .. } => {
			"Brought your library back from the backup.".to_owned()
		}
		nibkeep_node::RecoveredFrom::Library {
			replaced: true,
			set_aside: Some(aside),
			..
		} => format!(
			"Brought your library back in place of the one here, which no longer unlocked, and revoked that one. Its old folder is set aside in {} ({}): delete it once you no longer need it.",
			aside.display(),
			amount_text(old_size)
		),
		nibkeep_node::RecoveredFrom::Library {
			replaced: true,
			set_aside: None,
			folder,
			..
		} => format!(
			"Brought your library back in place of the one here, which no longer unlocked, and revoked that one. Its old folder, {} ({}), could not be set aside yet, as another program may have a file of it open: it goes aside as the library next opens, beside where it is, under a name that starts with .replaced-; delete it then.",
			folder.display(),
			amount_text(old_size)
		),
		nibkeep_node::RecoveredFrom::Library {
			replaced: false,
			folder,
			..
		} => format!(
			"Brought your library back from the one whose replica is in {} ({}), which no longer unlocked, and revoked that one. That folder is left as it was: delete it once you no longer need it.",
			folder.display(),
			amount_text(old_size)
		),
	}
}

/// An amount of data for people, rounded, such as `48.2 MB`.
fn amount_text(bytes: u64) -> String {
	#[expect(clippy::cast_precision_loss, reason = "rounded for people")]
	let value = bytes as f64;
	match bytes {
		0..1_000 => count(usize::try_from(bytes).unwrap_or(0), "byte", "bytes"),
		1_000..1_000_000 => format!("{:.1} kB", value / 1e3),
		1_000_000..1_000_000_000 => format!("{:.1} MB", value / 1e6),
		_ => format!("{:.1} GB", value / 1e9),
	}
}

/// A backup as `nib backup list` shows it: where this computer finds it, and what its
/// folder says of it, with the names of its spaces and policies.
pub(crate) struct ListedBackup {
	pub(crate) backup: nibkeep_node::Backup,
	pub(crate) folder: Option<std::path::PathBuf>,
	pub(crate) state: Option<nibkeep_node::BackupState>,
	pub(crate) spaces: Vec<String>,
	pub(crate) policies: Vec<String>,
}

/// What a backup's update moved one way, for people, such as `3 changes, 2 files (48.2
/// MB)`, or `nothing`.
fn moved_text(moved: &nibkeep_node::BackupMoved) -> String {
	counts_text(&nibkeep_node::Counts {
		changes: moved.changes,
		keys: moved.keys,
		blobs: moved.blobs,
		blob_bytes: moved.bytes,
	})
}

fn moved_json(moved: &nibkeep_node::BackupMoved) -> serde_json::Value {
	json!({
		"changes": moved.changes,
		"keys": moved.keys,
		"files": moved.blobs,
		"bytes": moved.bytes,
	})
}

fn backup_json(backup: &nibkeep_node::Backup) -> serde_json::Value {
	json!({
		"backup": backup.id.to_string(),
		"name": backup.name,
		"folder_on_drive": backup.folder,
		"spaces": backup.spaces.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"policies": backup.policies.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"away": backup.away,
		"waits": backup.waits,
		"waits_days": nibkeep_node::backup_wait_days(backup),
	})
}

fn update_json(updated: &nibkeep_node::BackupUpdate) -> serde_json::Value {
	json!({
		"update": updated.update,
		"unclean": updated.unclean,
		"taken_in": moved_json(&updated.taken),
		"written": moved_json(&updated.written),
		"purged": updated.purged,
		"waiting_purges": updated.waiting,
		"did_not_fit": updated.unfit,
		"did_not_fit_bytes": updated.unfit_bytes,
		"checked": updated.checked,
		"checked_bytes": updated.checked_bytes,
		"damaged": updated.damaged,
		"repaired": updated.repaired,
		"set_aside": updated.set_aside,
		"to_check_bytes": updated.to_check,
		"files": updated.held,
		"bytes": updated.held_bytes,
		"elsewhere": elsewhere_json(&updated.elsewhere),
	})
}

/// Prints a backup just made, and its first update.
pub(crate) fn backup_added(
	added: &nibkeep_node::AddedBackup,
	folder: &std::path::Path,
	updated: &nibkeep_node::BackupUpdate,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = backup_json(&added.backup);
		value["folder"] = json!(folder.display().to_string());
		value["on_share"] = json!(added.on_share);
		value["updated"] = update_json(updated);
		return print_json(&value, output);
	}
	writeln!(
		output,
		"Made the backup {} in {}{}.",
		added.backup.name,
		folder.display(),
		if added.backup.away {
			", kept away from home"
		} else {
			""
		}
	)?;
	backup_update_lines(updated, output)?;
	if added.on_share {
		writeln!(
			output,
			"It is on a network share, whose drive NibKeep cannot see, so it counts as a disk of its own: put no other backup or library on that drive."
		)?;
	}
	writeln!(
		output,
		"Your computers update it whenever they find it, while nib sync --serve, syncing from login or an app runs; nib backup update does it by hand."
	)?;
	Ok(())
}

/// The lines that say what an update of a backup did.
fn backup_update_lines(
	updated: &nibkeep_node::BackupUpdate,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if updated.unclean {
		writeln!(
			output,
			"It was pulled out or lost its power while it was written before; its files are read back before they count."
		)?;
	}
	writeln!(
		output,
		"Wrote {}; took in {}.",
		moved_text(&updated.written),
		moved_text(&updated.taken)
	)?;
	if updated.purged > 0 {
		writeln!(
			output,
			"Took away {} of purged content.",
			count(updated.purged, "item", "items")
		)?;
	}
	if updated.unfit > 0 {
		writeln!(
			output,
			"{} ({}) did not fit, as its drive keeps 1 GB free.",
			count(updated.unfit, "file", "files"),
			amount_text(updated.unfit_bytes)
		)?;
	}
	if updated.checked > 0 {
		writeln!(
			output,
			"Checked {} ({}).",
			count(updated.checked, "file", "files"),
			amount_text(updated.checked_bytes)
		)?;
	}
	if updated.damaged > 0 {
		writeln!(
			output,
			"Found {} damaged: {} written again, {} set aside. Its drive may be failing.",
			count(updated.damaged, "copy", "copies"),
			updated.repaired,
			updated.damaged - updated.repaired
		)?;
	} else if updated.set_aside > 0 {
		writeln!(
			output,
			"Set aside {} that did not read; what they held was written again.",
			count(updated.set_aside, "damaged file", "damaged files")
		)?;
	}
	if updated.to_check > 0 {
		writeln!(output, "{} still to check.", amount_text(updated.to_check))?;
	}
	writeln!(
		output,
		"It holds {} ({}).",
		count(updated.held, "file", "files"),
		amount_text(updated.held_bytes)
	)?;
	let (held, lost): (Vec<_>, Vec<_>) = updated
		.elsewhere
		.iter()
		.partition(|each| !each.holders.is_empty());
	if !held.is_empty() {
		writeln!(
			output,
			"Not on this device, so not updated now: {}.",
			elsewhere_with_holders(&held)
		)?;
	}
	if !lost.is_empty() {
		writeln!(
			output,
			"On none of your devices, so no update adds to it any more: {}.",
			elsewhere_names(&lost)
		)?;
	}
	Ok(())
}

/// Prints an update of a backup, or why it failed, or that another program updates it.
pub(crate) fn backup_updated(
	backup: &nibkeep_node::Backup,
	folder: &std::path::Path,
	updated: &Result<Option<nibkeep_node::BackupUpdate>, nibkeep_node::NodeError>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let updated = match updated {
		Ok(updated) => Ok(updated.clone()),
		Err(error) => Err(error.to_string()),
	};
	backup_update_text(backup, folder, updated, json, output)
}

fn backup_update_text(
	backup: &nibkeep_node::Backup,
	folder: &std::path::Path,
	updated: Result<Option<nibkeep_node::BackupUpdate>, String>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = backup_json(backup);
		value["folder"] = json!(folder.display().to_string());
		match &updated {
			Ok(Some(updated)) => value["updated"] = update_json(updated),
			Ok(None) => value["busy"] = json!(true),
			Err(error) => value["error"] = json!(error),
		}
		return print_json(&value, output);
	}
	match updated {
		Ok(Some(updated)) => {
			writeln!(
				output,
				"Updated the backup {} in {}.",
				backup.name,
				folder.display()
			)?;
			backup_update_lines(&updated, output)
		}
		Ok(None) => {
			writeln!(
				output,
				"Another program updates the backup {} now.",
				backup.name
			)?;
			Ok(())
		}
		Err(error) => {
			writeln!(
				output,
				"Could not update the backup {} in {}: {error}",
				backup.name,
				folder.display()
			)?;
			Ok(())
		}
	}
}

/// Prints the person's backups.
pub(crate) fn backups(
	listed: &[ListedBackup],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let values: Vec<serde_json::Value> = listed
			.iter()
			.map(|each| {
				let mut value = backup_json(&each.backup);
				value["found_at"] = json!(
					each.folder
						.as_ref()
						.map(|folder| folder.display().to_string())
				);
				value["space_names"] = json!(each.spaces);
				value["policy_names"] = json!(each.policies);
				if let Some(state) = &each.state {
					value["updates"] = json!(state.update);
					value["files"] = json!(state.held);
					value["to_check_bytes"] = json!(state.to_check);
					value["damaged"] = json!(state.damaged);
				}
				value
			})
			.collect();
		return print_json(&json!(values), output);
	}
	if listed.is_empty() {
		writeln!(
			output,
			"No backups yet; nib backup new FOLDER makes one, such as on a USB drive."
		)?;
		return Ok(());
	}
	for each in listed {
		let keeps = if each.policies.is_empty() {
			"every file".to_owned()
		} else {
			format!("the files of {}", each.policies.join(", "))
		};
		writeln!(
			output,
			"{}: {}, {keeps}{}{}",
			each.backup.name,
			each.spaces.join(", "),
			if each.backup.away {
				", kept away from home"
			} else {
				""
			},
			backup_waits(&each.backup)
		)?;
		match (&each.folder, &each.state) {
			(Some(folder), Some(state)) => {
				writeln!(
					output,
					"  in {}: {} updates, {}{}{}",
					folder.display(),
					state.update,
					count(state.held, "file", "files"),
					if state.to_check > 0 {
						format!(", {} still to check", amount_text(state.to_check))
					} else {
						String::new()
					},
					if state.damaged > 0 {
						format!(
							", {} found damaged lately",
							count(state.damaged, "copy", "copies")
						)
					} else {
						String::new()
					}
				)?;
			}
			_ => writeln!(output, "  not found on this computer")?,
		}
	}
	Ok(())
}

/// Prints a backup changed.
pub(crate) fn backup_edited(
	backup: &nibkeep_node::Backup,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&backup_json(backup), output);
	}
	writeln!(
		output,
		"Changed the backup {}{}{}.",
		backup.name,
		if backup.away {
			", kept away from home"
		} else {
			", kept at home"
		},
		backup_waits(backup)
	)?;
	Ok(())
}

/// How long a backup waits before purging, for people: after a comma.
fn backup_waits(backup: &nibkeep_node::Backup) -> String {
	match nibkeep_node::backup_wait_days(backup) {
		0 => ", purges at once".to_owned(),
		days => format!(
			", waits {} before purging",
			count(usize::try_from(days).unwrap_or(usize::MAX), "day", "days")
		),
	}
}

/// Prints a backup forgotten, with where its folder stays.
pub(crate) fn backup_removed(
	backup: &nibkeep_node::Backup,
	folder: Option<&std::path::Path>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = backup_json(backup);
		value["removed"] = json!(true);
		value["folder"] = json!(folder.map(|folder| folder.display().to_string()));
		return print_json(&value, output);
	}
	match folder {
		Some(folder) => writeln!(
			output,
			"Forgot the backup {}; its folder {} stays, for you to delete.",
			backup.name,
			folder.display()
		)?,
		None => writeln!(
			output,
			"Forgot the backup {}; its folder stays, for you to delete.",
			backup.name
		)?,
	}
	Ok(())
}

/// A recovery code's name: the short name of its recovery key.
fn recovery_name(code: &nibkeep_crypto::RecoveryCode) -> String {
	nibkeep_crypto::RecoveryCode::name_of(&code.signing_key().verifying_key())
}

/// A recovery code that the library names, in JSON.
fn recovery_json(code: &nibkeep_node::RecoveryCodeInfo) -> serde_json::Value {
	json!({
		"record": code.record.to_string(),
		"name": code.name,
		"made": moment_json(code.made_at),
		"made_on": code.made_on,
		"revoked": code.revoked,
		"holds_secrets": code.holds_secrets,
		"backups": {
			"carried_by": code.carried_by,
			"to_update": code.to_update,
		},
		"replaced_by": code.replaced_by,
	})
}

/// Names a list: "A", "A and B", "A, B and C".
fn named_list(names: &[String]) -> String {
	match names {
		[] => String::new(),
		[one] => one.clone(),
		[rest @ .., last] => format!("{} and {last}", rest.join(", ")),
	}
}

/// Says what became of the codes that a code that now stands replaces: those revoked now,
/// and those whose paper to keep until every backup carries the new code.
fn recovery_replacing_told(
	code: &nibkeep_node::RecoveryCodeInfo,
	(replaced, codes): (
		&[nibkeep_node::RecoveryCodeInfo],
		&[nibkeep_node::RecoveryCodeInfo],
	),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if !replaced.is_empty() {
		let names: Vec<String> = replaced.iter().map(|old| old.name.clone()).collect();
		writeln!(output, "It replaces {}, revoked now.", named_list(&names))?;
	}
	let kept: Vec<String> = codes
		.iter()
		.filter(|old| !old.revoked && old.replaced_by.as_deref() == Some(code.name.as_str()))
		.map(|old| old.name.clone())
		.collect();
	if !kept.is_empty() {
		let backups = match code.to_update.as_slice() {
			[] => "every backup carries".to_owned(),
			[one] => format!("{one} carries"),
			several => format!("{} carry", named_list(several)),
		};
		writeln!(
			output,
			"Keep the paper of {} until {backups} {}: until then a backup opens with the old \
			 code alone. The old code is revoked then; nib recovery confirm --revoke-older \
			 revokes it now.",
			named_list(&kept),
			code.name
		)?;
	}
	Ok(())
}

/// What the person's backups have of a recovery code that stands, each line a sentence or
/// an indented item: those that carry it, those still to update, or that there is none.
fn recovery_backups_lines(code: &nibkeep_node::RecoveryCodeInfo) -> Vec<String> {
	if code.revoked {
		return Vec::new();
	}
	let mut lines = Vec::new();
	if let Some(newer) = &code.replaced_by {
		lines.push(format!(
			"replaced by {newer} once every backup carries it: keep its paper until then"
		));
	}
	if !code.carried_by.is_empty() {
		lines.push(format!("carried by: {}", code.carried_by.join(", ")));
	}
	if !code.to_update.is_empty() {
		lines.push(format!("still to update: {}", code.to_update.join(", ")));
	}
	if lines.is_empty() {
		lines.push(
			"no backup carries it, and a code brings the library back only with one; nib backup new makes one"
				.to_owned(),
		);
	}
	lines
}

/// Says what the person's backups lack of a code that now stands: those to update before
/// they carry it, or that there is none.
fn recovery_backups_told(
	code: &nibkeep_node::RecoveryCodeInfo,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if !code.to_update.is_empty() {
		writeln!(
			output,
			"It brings the library back from a backup once nib backup update has updated it: {}.",
			code.to_update.join(", ")
		)?;
	} else if code.carried_by.is_empty() {
		writeln!(
			output,
			"A code brings the library back only with a backup, and there is none: nib backup new makes one."
		)?;
	}
	Ok(())
}

/// A recovery code that the library names, on a line: its name, and when and where it was
/// made.
fn recovery_line(code: &nibkeep_node::RecoveryCodeInfo, zone: &jiff::tz::TimeZone) -> String {
	let made = moment_text(code.made_at, zone);
	match &code.made_on {
		Some(device) => format!("{}, made {made} on {device}", code.name),
		None => format!("{}, made {made}", code.name),
	}
}

/// Shows a new recovery code, for the person to write down: its name, and its groups, five
/// to a line. With `--json`, it is shown once the command ends.
pub(crate) fn recovery_shown(
	code: &nibkeep_crypto::RecoveryCode,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return Ok(());
	}
	writeln!(output, "A new recovery code, {}:", recovery_name(code))?;
	writeln!(output)?;
	let text = code.to_text();
	let groups: Vec<&str> = text.split('-').collect();
	for line in groups.chunks(5) {
		writeln!(output, "  {}", line.join("-"))?;
	}
	writeln!(output)?;
	writeln!(
		output,
		"Write it down or print it, with its name, and keep it as safely as a passport:"
	)?;
	writeln!(
		output,
		"whoever has it and a backup of this library reads everything in it."
	)?;
	writeln!(
		output,
		"The code holds keys and no data: it brings the library back from a backup, or"
	)?;
	writeln!(
		output,
		"from a library on this computer that no longer unlocks."
	)?;
	Ok(())
}

/// Says whether a new recovery code stands, as the person typed it back, or waits for
/// `nib recovery confirm`.
pub(crate) fn recovery_made(
	code: &nibkeep_crypto::RecoveryCode,
	stands: Option<&nibkeep_node::RecoveryCodeInfo>,
	(replaced, codes): (
		&[nibkeep_node::RecoveryCodeInfo],
		&[nibkeep_node::RecoveryCodeInfo],
	),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let name = recovery_name(code);
	if json {
		return print_json(
			&json!({
				"name": name,
				"code": code.to_text().as_str(),
				"stands": stands.is_some(),
				"replaced": replaced.iter().map(|old| old.name.as_str()).collect::<Vec<_>>(),
			}),
			output,
		);
	}
	if let Some(named) = stands {
		writeln!(
			output,
			"The recovery code {name} now stands{}.",
			secrets_text(named)
		)?;
		recovery_backups_told(named, output)?;
		recovery_replacing_told(named, (replaced, codes), output)?;
	} else {
		writeln!(
			output,
			"Nothing names the code yet. Once it is written down, run nib recovery confirm and type it."
		)?;
	}
	Ok(())
}

/// Says that a recovery code that the person typed stands.
pub(crate) fn recovery_confirmed(
	code: &nibkeep_node::RecoveryCodeInfo,
	already: bool,
	(replaced, codes): (
		&[nibkeep_node::RecoveryCodeInfo],
		&[nibkeep_node::RecoveryCodeInfo],
	),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut value = recovery_json(code);
		value["already"] = json!(already);
		value["replaced"] = json!(
			replaced
				.iter()
				.map(|old| old.name.as_str())
				.collect::<Vec<_>>()
		);
		return print_json(&value, output);
	}
	let secrets = secrets_text(code);
	if already {
		writeln!(
			output,
			"The recovery code {} stands already{secrets}.",
			code.name
		)?;
	} else {
		writeln!(
			output,
			"The recovery code {} now stands{secrets}.",
			code.name
		)?;
		recovery_backups_told(code, output)?;
	}
	recovery_replacing_told(code, (replaced, codes), output)?;
	Ok(())
}

/// Whether a recovery code opens the secrets, as the end of a sentence about it.
fn secrets_text(code: &nibkeep_node::RecoveryCodeInfo) -> &'static str {
	if code.holds_secrets {
		", and opens the secrets too"
	} else {
		"; it does not open the secrets, which nib recovery confirm --secrets gives it"
	}
}

/// Lists the person's recovery codes.
pub(crate) fn recovery_list(
	codes: &[nibkeep_node::RecoveryCodeInfo],
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&serde_json::Value::Array(codes.iter().map(recovery_json).collect()),
			output,
		);
	}
	if codes.is_empty() {
		writeln!(output, "No recovery code; nib recovery new makes one.")?;
	}
	for code in codes {
		let state = match (code.revoked, code.holds_secrets) {
			(true, _) => " (revoked)",
			(false, true) => " (opens the secrets)",
			(false, false) => "",
		};
		writeln!(output, "{}{state}", recovery_line(code, zone))?;
		for line in recovery_backups_lines(code) {
			writeln!(output, "  {line}")?;
		}
	}
	Ok(())
}

/// Says what a check of a recovery code found.
pub(crate) fn recovery_checked(
	check: &nibkeep_node::RecoveryCheck,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let opens: usize = check.spaces.iter().map(|space| space.opens.len()).sum();
	let lacks: usize = check.spaces.iter().map(|space| space.lacks.len()).sum();
	// The secrets keys, where the secrets are unlocked: those it opens, and those it lacks.
	let secrets = check
		.spaces
		.iter()
		.filter_map(|space| space.secrets.as_ref())
		.fold(None, |sum: Option<(usize, usize)>, (opens, lacks)| {
			let (opened, lacked) = sum.unwrap_or_default();
			Some((opened + opens.len(), lacked + lacks.len()))
		});
	if json {
		return print_json(
			&json!({
				"known": check.code.is_some(),
				"name": check.code.as_ref().map(|code| &code.name),
				"revoked": check.code.as_ref().map(|code| code.revoked),
				"opens": opens,
				"lacks": lacks,
				"secrets": secrets.map(|(opens, lacks)| json!({ "opens": opens, "lacks": lacks })),
			}),
			output,
		);
	}
	match &check.code {
		None => writeln!(
			output,
			"This library names no recovery code like the one typed."
		)?,
		Some(code) if code.revoked => writeln!(
			output,
			"The recovery code {} is revoked: it opens nothing added since, and brings no device back.",
			code.name
		)?,
		Some(code) if lacks == 0 => writeln!(
			output,
			"The recovery code {} stands, and opens every key of the library.",
			code.name
		)?,
		Some(code) => writeln!(
			output,
			"The recovery code {} stands, but lacks {lacks} of the library's keys; a sync gives it them.",
			code.name
		)?,
	}
	if check.code.as_ref().is_some_and(|code| !code.revoked) {
		match secrets {
			Some((_, 0)) => writeln!(output, "It opens the secrets too.")?,
			Some((0, _)) => writeln!(
				output,
				"It does not open the secrets; nib recovery confirm --secrets gives it them."
			)?,
			Some((_, lacked)) => writeln!(
				output,
				"It lacks {lacked} of the secrets' keys; unlocking the secrets on a device that holds them gives it them."
			)?,
			None => {}
		}
	}
	Ok(())
}

/// Says which recovery code was revoked, or would be.
pub(crate) fn recovery_revoked(
	code: &nibkeep_node::RecoveryCodeInfo,
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&recovery_json(code), output);
	}
	writeln!(
		output,
		"{}",
		if code.revoked {
			"Revoked, for good:"
		} else {
			"Would revoke, for good:"
		}
	)?;
	writeln!(output, "  the recovery code {}", recovery_line(code, zone))?;
	if code.revoked {
		writeln!(
			output,
			"Its paper brings nothing back any more; nib recovery new makes another."
		)?;
	}
	Ok(())
}

/// What one direction of a sync carried, for people, such as `3 changes, 2 keys, 1 file
/// (48.2 MB)`, or `nothing`.
fn counts_text(counts: &nibkeep_node::Counts) -> String {
	let mut parts = Vec::new();
	if counts.changes > 0 {
		parts.push(count(counts.changes, "change", "changes"));
	}
	if counts.keys > 0 {
		parts.push(count(counts.keys, "key", "keys"));
	}
	if counts.blobs > 0 {
		parts.push(format!(
			"{} ({})",
			count(counts.blobs, "file", "files"),
			amount_text(counts.blob_bytes)
		));
	}
	if parts.is_empty() {
		"nothing".to_owned()
	} else {
		parts.join(", ")
	}
}

fn counts_json(counts: &nibkeep_node::Counts) -> serde_json::Value {
	json!({
		"changes": counts.changes,
		"keys": counts.keys,
		"files": counts.blobs,
		"file_bytes": counts.blob_bytes,
	})
}

fn spaces_json(spaces: &[SpaceSynced]) -> serde_json::Value {
	spaces
		.iter()
		.map(|space| {
			json!({
				"space": space.space.to_string(),
				"sent": counts_json(&space.sent),
				"received": counts_json(&space.received),
			})
		})
		.collect()
}

/// What a sync sent and received, for people: `sent …, received …`.
fn synced_text(synced: &Synced) -> String {
	let mut sent = nibkeep_node::Counts::default();
	let mut received = nibkeep_node::Counts::default();
	for space in &synced.spaces {
		add(&mut sent, &space.sent);
		add(&mut received, &space.received);
	}
	format!(
		"sent {}, received {}",
		counts_text(&sent),
		counts_text(&received)
	)
}

fn add(total: &mut nibkeep_node::Counts, counts: &nibkeep_node::Counts) {
	total.changes += counts.changes;
	total.keys += counts.keys;
	total.blobs += counts.blobs;
	total.blob_bytes += counts.blob_bytes;
}

/// A replica for people, as `nib on desk`.
fn replica_text(replica: &Replica) -> String {
	let whose = replica
		.member
		.as_deref()
		.map_or_else(String::new, |member| format!("{}'s ", shown(member)));
	format!(
		"{whose}{} on {}",
		shown(replica.name.as_deref().unwrap_or("an app")),
		shown(replica.device_name.as_deref().unwrap_or("a device"))
	)
}

fn replica_json(replica: &Replica) -> serde_json::Value {
	json!({
		"id": replica.id.to_string(),
		"name": replica.name,
		"device": replica.device.map(|device| device.to_string()),
		"device_name": replica.device_name,
		"this": replica.this,
		"revoked": replica.revoked,
		"deleted": replica.deleted,
		"ai_agent": replica.ai_agent,
		"set_aside": replica.set_aside,
		"holds_secrets": replica.holds_secrets,
		"given_secrets": replica.given_secrets,
		"asks_for_secrets": replica.asks_for_secrets,
		"shared": replica.shared.map(|space| space.to_string()),
		"member": replica.member,
		"role": replica.marks.role.as_str(),
		"away": replica.marks.away,
	})
}

/// Prints the replica that pairing added, and its first sync.
pub(crate) fn paired(
	paired: &nibkeep_node::Paired,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"paired": {
					"replica": paired.replica.to_string(),
					"device_name": paired.device,
					"name": paired.app,
					"ai_agent": paired.ai_agent,
					"secrets": match paired.asked_for {
						AskedFor::Nothing => None,
						AskedFor::Secrets => Some("asked"),
						AskedFor::SecretsGiven => Some("given"),
					},
				},
				"synced": { "spaces": spaces_json(&paired.synced.spaces) },
			}),
			output,
		);
	}
	if paired.ai_agent {
		writeln!(
			output,
			"Paired the AI agent {} on {}: serve it the library with nib mcp serve there.",
			paired.app, paired.device
		)?;
	} else {
		writeln!(output, "Paired {} on {}.", paired.app, paired.device)?;
	}
	writeln!(output, "Synced with it: {}.", synced_text(&paired.synced))?;
	match paired.asked_for {
		AskedFor::Nothing => {}
		AskedFor::Secrets => writeln!(
			output,
			"It asked for the secrets: give them with nib secret give {}.",
			short(paired.replica)
		)?,
		AskedFor::SecretsGiven => writeln!(output, "It asked for the secrets, and got them.")?,
	}
	Ok(())
}

/// Prints a sync that another device started.
pub(crate) fn answered(
	answered: &nibkeep_node::Answered,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let name = answered.replica.as_ref();
	let refused = answered.refused;
	if json {
		let value = match &answered.synced {
			Ok(synced) => json!({
				"replica": name.map(replica_json),
				"spaces": spaces_json(&synced.spaces),
				"refused": refused,
				"deleted": paths_json(&answered.deleted),
			}),
			Err(error) => json!({
				"replica": name.map(replica_json),
				"error": error.to_string(),
				"refused": refused,
			}),
		};
		let mut line = serde_json::to_vec(&value).map_err(std::io::Error::from)?;
		line.push(b'\n');
		output.write_all(&line)?;
	} else {
		let who = name.map_or_else(|| "another device".to_owned(), replica_text);
		match &answered.synced {
			Ok(synced) => writeln!(output, "Synced with {who}: {}.", synced_text(synced))?,
			Err(error) => writeln!(output, "A sync with {who} failed: {error}.")?,
		}
		refused_text(refused, output)?;
		deleted_text(&answered.deleted, output)?;
	}
	output.flush()?;
	Ok(())
}

/// The paths, as JSON.
fn paths_json(paths: &[std::path::PathBuf]) -> serde_json::Value {
	paths
		.iter()
		.map(|path| serde_json::Value::String(path.display().to_string()))
		.collect()
}

/// Says which copies of checkouts and of trees' files a sync deleted, as it brought purges
/// of their content.
fn deleted_text(deleted: &[std::path::PathBuf], output: &mut dyn Write) -> Result<(), CliError> {
	for path in deleted {
		writeln!(
			output,
			"Deleted the copy at {}: its content was purged.",
			path.display()
		)?;
	}
	Ok(())
}

/// Says that files and changes were not received, as the disk would have had too little
/// room left.
fn refused_text(refused: usize, output: &mut dyn Write) -> Result<(), CliError> {
	match refused {
		0 => {}
		1 => writeln!(
			output,
			"1 file or change was not received: the disk would have had less than 1 GiB free."
		)?,
		_ => writeln!(
			output,
			"{refused} files and changes were not received: the disk would have had less than 1 GiB free."
		)?,
	}
	Ok(())
}

/// Prints the syncs with the user's other devices.
pub(crate) fn synced_with(
	synced: &[nibkeep_node::SyncedWith],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let value: Vec<serde_json::Value> = synced
			.iter()
			.map(|with| match &with.synced {
				Ok(done) => json!({
					"replica": replica_json(&with.replica),
					"spaces": spaces_json(&done.spaces),
					"refused": with.refused,
					"deleted": paths_json(&with.deleted),
				}),
				Err(error) => json!({
					"replica": replica_json(&with.replica),
					"error": error.to_string(),
					"refused": with.refused,
				}),
			})
			.collect();
		return print_json(&serde_json::Value::Array(value), output);
	}
	for with in synced {
		match &with.synced {
			Ok(done) => writeln!(
				output,
				"Synced with {}: {}.",
				replica_text(&with.replica),
				synced_text(done)
			)?,
			Err(error) => writeln!(output, "{}: {error}.", replica_text(&with.replica))?,
		}
		refused_text(with.refused, output)?;
		deleted_text(&with.deleted, output)?;
	}
	Ok(())
}

/// Prints the user's devices: the replicas of the library on them.
pub(crate) fn replicas(
	replicas: &[Replica],
	((holder, records), at_login): (SyncRecords<'_>, Option<&at_login::AtLoginStatus>),
	zone: &jiff::tz::TimeZone,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let recorded = |replica: &Replica| records.and_then(|records| records.get(&replica.id));
	if json {
		let listed = replicas
			.iter()
			.map(|replica| {
				let mut value = replica_json(replica);
				if let Some(record) = recorded(replica) {
					value["sync"] = record_json(record);
				}
				if let Some(state) = at_login.filter(|_| replica.this) {
					value["at_login"] = at_login_json(state);
				}
				value
			})
			.collect();
		return print_json(&serde_json::Value::Array(listed), output);
	}
	for replica in replicas {
		let mut notes = Vec::new();
		if replica.this {
			notes.push("this one".to_owned());
		}
		if let Some(record) = recorded(replica) {
			notes.extend(record_notes(record, zone));
		}
		if replica.ai_agent {
			notes.push("an AI agent".to_owned());
		}
		if replica.marks.role == DeviceRole::Archive {
			notes.push("an archive node".to_owned());
		}
		if replica.marks.away {
			notes.push("away from home".to_owned());
		}
		if replica.revoked {
			notes.push("revoked".to_owned());
		} else if replica.deleted {
			notes.push("deleted, still syncs until revoked".to_owned());
		}
		if replica.revoked && replica.given_secrets {
			notes.push("held the secrets".to_owned());
		} else if replica.holds_secrets && !replica.revoked {
			notes.push("holds the secrets".to_owned());
		} else if replica.asks_for_secrets && !replica.revoked {
			notes.push("asks for the secrets, see nib secret give".to_owned());
		}
		if replica.set_aside > 0 {
			notes.push(format!(
				"{} set aside, see nib device review",
				changes_text(replica.set_aside)
			));
		}
		let notes = if notes.is_empty() {
			String::new()
		} else {
			format!(" ({})", notes.join("; "))
		};
		writeln!(
			output,
			"{}  {}{notes}",
			short(replica.id),
			replica_text(replica),
		)?;
	}
	if let Some(holder) = holder {
		writeln!(output, "{} syncs this device.", holder.program)?;
	}
	if let Some(state) = at_login {
		writeln!(output, "{}", at_login_text(state))?;
	}
	Ok(())
}

/// What the program that holds the sync role recorded, as far as it could be read: itself,
/// if one holds it, and of each other replica, its presence and last sync.
pub(crate) type SyncRecords<'a> = (
	Option<&'a nibkeep_node::HolderRecord>,
	Option<&'a std::collections::BTreeMap<EntityId, ReplicaRecord>>,
);

/// What the holder of the sync role recorded of a replica, for people.
fn record_notes(record: &ReplicaRecord, zone: &jiff::tz::TimeZone) -> Vec<String> {
	let mut notes = Vec::new();
	if record.present {
		notes.push("on the network".to_owned());
	}
	match (&record.synced, &record.error) {
		(Some(at), Some(error)) => notes.push(format!(
			"last sync failed {}: {error}",
			moment_text(*at, zone)
		)),
		(Some(at), None) => notes.push(format!("synced {}", moment_text(*at, zone))),
		_ => {}
	}
	notes
}

/// What the holder of the sync role recorded of a replica, in JSON.
fn record_json(record: &ReplicaRecord) -> serde_json::Value {
	json!({
		"present": record.present,
		"seen": record.seen.map(moment_json),
		"synced": record.synced.map(moment_json),
		"received": record.received,
		"sent": record.sent,
		"error": record.error,
	})
}

/// Prints what the program that holds the sync role did, as asked to sync: of each other
/// replica, how its sync went, as `synced` recorded it since the ask, or that it was not
/// found. Returns how many failed.
pub(crate) fn asked(
	peers: &[Replica],
	synced: &std::collections::BTreeMap<EntityId, ReplicaRecord>,
	json: bool,
	output: &mut dyn Write,
) -> Result<usize, CliError> {
	let fresh = |replica: &Replica| synced.get(&replica.id);
	// As when nib syncs itself, one not found failed too.
	let failed = peers
		.iter()
		.filter(|replica| fresh(replica).is_none_or(|record| record.error.is_some()))
		.count();
	if json {
		let value: Vec<serde_json::Value> = peers
			.iter()
			.map(|replica| {
				json!({
					"replica": replica_json(replica),
					"error": match fresh(replica) {
						None => Some("not found on the network".to_owned()),
						Some(record) => record.error.clone(),
					},
					"sync": fresh(replica).map(record_json),
				})
			})
			.collect();
		print_json(&serde_json::Value::Array(value), output)?;
		return Ok(failed);
	}
	for replica in peers {
		let name = replica_text(replica);
		match fresh(replica) {
			None => writeln!(output, "{name}: not found on the network")?,
			Some(ReplicaRecord {
				error: Some(error), ..
			}) => writeln!(output, "{name}: failed: {error}")?,
			Some(record) => writeln!(
				output,
				"{name}: synced, {} received, {} sent",
				changes_text(record.received),
				changes_text(record.sent)
			)?,
		}
	}
	Ok(failed)
}

/// Prints what the program that holds the sync role does, as `nib sync --serve` does it.
pub(crate) fn held(
	held: &nibkeep_node::Held,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	match held {
		nibkeep_node::Held::Started { port, refused } => {
			let outside = refused.as_ref().filter(|(asked, _)| {
				!port.is_some_and(|port| {
					(*asked..asked.saturating_add(nibkeep_node::PORTS)).contains(&port)
				})
			});
			if let Some((asked, why)) = outside {
				let port =
					port.map_or_else(|| "another port".to_owned(), |port| format!("port {port}"));
				progress(
					&format!(
						"UDP ports from {asked} could not be used ({why}); this one takes connections on {port}, which a firewall may block."
					),
					json,
					output,
				)?;
			}
			Ok(())
		}
		nibkeep_node::Held::Lent => progress(
			"Another program adds a device; syncing goes on once it is done.",
			json,
			output,
		),
		nibkeep_node::Held::Answered(answered) => self::answered(answered, json, output),
		nibkeep_node::Held::Synced(with) => synced_with(std::slice::from_ref(with), json, output),
		nibkeep_node::Held::Checked(run) if run.damaged > 0 => progress(
			&format!(
				"A check found {} damaged; a sealed copy comes again from another device, and an original stays as it is.",
				count(run.damaged, "copy", "copies")
			),
			json,
			output,
		),
		nibkeep_node::Held::Checked(_) => Ok(()),
		nibkeep_node::Held::Removed(pass) => removal_pass(pass, json, output),
		nibkeep_node::Held::BackedUp(update) => {
			let updated = match &update.updated {
				Ok(updated) => Ok(Some(updated.clone())),
				Err(error) => Err(error.to_string()),
			};
			backup_update_text(&update.backup, &update.folder, updated, json, output)
		}
	}
}

/// Prints a pass of removals, when it removed something or failed: the copies of files that
/// this device no longer keeps, removed once other devices proved theirs.
fn removal_pass(
	pass: &Result<nibkeep_node::RemovalPass, nibkeep_node::NodeError>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let pass = match pass {
		Ok(pass) => pass,
		Err(error) => {
			return progress(
				&format!("Removing what this device no longer keeps failed: {error}."),
				json,
				output,
			);
		}
	};
	for with in pass.synced.iter().filter(|with| nibkeep_node::tried(with)) {
		if let Err(error) = &with.synced {
			progress(
				&format!("{}: {error}.", replica_text(&with.replica)),
				json,
				output,
			)?;
		}
	}
	let removed = &pass.removed;
	for failed in &removed.failed {
		progress(
			&format!("A purge waits for the next pass: {failed}."),
			json,
			output,
		)?;
	}
	if removed.copies == 0 && pass.thumbnails == 0 && removed.moves == 0 {
		return Ok(());
	}
	if json {
		return print_json(
			&json!({
				"removed": removed.copies,
				"removed_bytes": removed.bytes,
				"waiting": removed.waiting,
				"thumbnails_purged": pass.thumbnails,
				"moves_purged": removed.moves,
				"deleted": paths_json(&removed.deleted),
			}),
			output,
		);
	}
	if removed.copies > 0 {
		writeln!(
			output,
			"Removed {} ({}) of files that this device no longer keeps: other devices proved they hold them.",
			count(removed.copies, "copy", "copies"),
			amount_text(removed.bytes)
		)?;
	}
	if pass.thumbnails > 0 {
		writeln!(
			output,
			"Purged {} that this device made of pictures that show another.",
			count(pass.thumbnails, "thumbnail", "thumbnails")
		)?;
	}
	if removed.moves > 0 {
		writeln!(
			output,
			"Purged what {} left behind: their files' copies where they went are proven.",
			count(removed.moves, "move", "moves")
		)?;
		deleted_text(&removed.deleted, output)?;
	}
	Ok(())
}

/// "1 change" or "3 changes".
fn changes_text(count: usize) -> String {
	if count == 1 {
		"1 change".to_owned()
	} else {
		format!("{count} changes")
	}
}

/// Prints the replicas that a revocation revokes, or would revoke without `confirmed`, and
/// the spaces named in `elsewhere` where it also revoked devices that they had added.
pub(crate) fn revoked(
	replicas: &[Replica],
	(codes, codes_too): (&[nibkeep_node::RecoveryCodeInfo], bool),
	(confirmed, elsewhere): (bool, &[(nibkeep_base::SpaceId, String)]),
	out_of_reach: &[nibkeep_node::HeldElsewhere],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({
				"revoked": confirmed,
				"replicas": replicas.iter().map(replica_json).collect::<Vec<_>>(),
				"also_revoked_in": elsewhere
					.iter()
					.map(|(space, _)| space.to_string())
					.collect::<Vec<_>>(),
				"out_of_reach": elsewhere_json(out_of_reach),
				"codes": codes.iter().map(|code| json!({
					"record": code.record.to_string(),
					"name": code.name,
					"revoked": code.revoked,
				})).collect::<Vec<_>>(),
			}),
			output,
		);
	}
	if replicas.is_empty() {
		writeln!(output, "That is revoked already.")?;
		return Ok(());
	}
	writeln!(
		output,
		"{}",
		if confirmed {
			"Revoked, for good:"
		} else {
			"Would revoke, for good:"
		}
	)?;
	for replica in replicas {
		writeln!(output, "  {}  {}", short(replica.id), replica_text(replica))?;
	}
	if confirmed {
		writeln!(
			output,
			"Your other devices stop syncing with it once they sync with this one, and the \
			 library has a new key that it does not get."
		)?;
	}
	revoked_elsewhere((confirmed, elsewhere), out_of_reach, output)?;
	// The recovery codes made on it: whoever has it may have read them as they were made.
	if !codes.is_empty() {
		let names: Vec<String> = codes.iter().map(|code| code.name.clone()).collect();
		let names = named_list(&names);
		match (confirmed, codes_too) {
			(false, _) => writeln!(
				output,
				"Recovery codes made on it: {names}. With --codes, they are revoked too; \
				 without it, they stay."
			)?,
			(true, true) => writeln!(
				output,
				"Revoked the recovery codes made on it too: {names}."
			)?,
			(true, false) => writeln!(
				output,
				"The recovery codes made on it stay: {names}. nib recovery revoke revokes one."
			)?,
		}
	}
	// Revoking takes nothing back from a device that was given the secrets: said apart, by
	// name.
	let held: Vec<String> = replicas
		.iter()
		.filter(|replica| replica.given_secrets)
		.map(replica_text)
		.collect();
	if !held.is_empty() {
		let (names, they) = match held.as_slice() {
			[one] => (one.clone(), "It"),
			[rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "They"),
			[] => unreachable!("not empty"),
		};
		writeln!(output)?;
		if confirmed {
			writeln!(
				output,
				"{names} held the secrets. {they} can still read every password and secret \
				 note saved until now, though none saved from now on. Change the passwords that \
				 matter."
			)?;
		} else {
			writeln!(
				output,
				"{names} held the secrets. Revoked, {} could still read every password and \
				 secret note saved until then, though none saved after: change the passwords \
				 that matter then.",
				they.to_lowercase()
			)?;
		}
	}
	Ok(())
}

/// Prints that this device asked for the secrets from `from`, and what to do there: name
/// this one by the ID of its replica, which needs no quoting.
pub(crate) fn asked_for_secrets(
	own: Option<&Replica>,
	from: &Replica,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(&json!({"asked": true, "from": replica_json(from)}), output);
	}
	let give = own.map_or_else(
		|| "nib secret give with this device's name".to_owned(),
		|own| format!("nib secret give {}", short(own.id)),
	);
	writeln!(
		output,
		"Asked {} for the secrets. There, run {give}; they arrive with the next sync, and \
		 this device takes them when its secrets are next unlocked.",
		replica_text(from)
	)?;
	Ok(())
}

/// A state of the secrets, in a word, for JSON.
fn secrets_state_text(state: nibkeep_node::SecretsState) -> &'static str {
	use nibkeep_node::SecretsState;
	match state {
		SecretsState::Here => "here",
		SecretsState::Asked => "asked",
		SecretsState::Elsewhere => "elsewhere",
		SecretsState::NotSetUp => "not set up",
	}
}

/// Prints what became of the secrets that a new device asked for as it joined, or, for an
/// AI agent's replica, that it gets none.
fn paired_secrets(
	(state, ai_agent): (nibkeep_node::SecretsState, bool),
	output: &mut dyn Write,
) -> Result<(), CliError> {
	use nibkeep_node::SecretsState;
	let said = match state {
		_ if ai_agent => "An AI agent gets no secrets.",
		SecretsState::Here => "The secrets are here too, behind their own passphrase.",
		SecretsState::Asked => {
			"Asked for the secrets: give them on the other device with nib secret give; they \
			 arrive with the next sync."
		}
		SecretsState::Elsewhere => {
			"The other device holds no secrets: ask one that does, with nib secret ask."
		}
		SecretsState::NotSetUp => "The library holds no secrets yet.",
	};
	writeln!(output, "{said}")?;
	Ok(())
}

/// Prints the replicas that were given the secrets, and how many keys each got.
pub(crate) fn gave_secrets(
	given: &[(Replica, usize)],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let given: Vec<_> = given
			.iter()
			.map(|(replica, keys)| json!({"replica": replica_json(replica), "keys": keys}))
			.collect();
		return print_json(&json!({"given": given}), output);
	}
	for (replica, keys) in given {
		if *keys == 0 {
			writeln!(output, "{} has the secrets already.", replica_text(replica))?;
		} else {
			writeln!(
				output,
				"Gave the secrets to {}; they reach it with the next sync.",
				replica_text(replica)
			)?;
		}
	}
	Ok(())
}

/// Prints that this device asked `member`'s `device` for the secrets of `space`, and what to
/// do there.
pub(crate) fn asked_for_space_secrets(
	space: &str,
	(member, device): (&str, &nibkeep_node::MemberReplica),
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({"asked": {"id": device.record.to_string(), "device_name": device.device, "member": member}, "space": space}),
			output,
		);
	}
	let device = shown(device.device.as_deref().unwrap_or("a device"));
	writeln!(
		output,
		"Asked {}'s {device} for the secrets of {}. Once {} gives them there, they reach this \
		 device with a sync, and your other devices from it.",
		shown(member),
		shown(space),
		shown(member)
	)?;
	Ok(())
}

/// Prints that this device gave `member`'s `device` `keys` keys of the secrets of `space`.
pub(crate) fn gave_space_secrets(
	space: &str,
	(member, device): (&str, &nibkeep_node::MemberReplica),
	keys: usize,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({"given": {"id": device.record.to_string(), "device_name": device.device, "member": member}, "keys": keys, "space": space}),
			output,
		);
	}
	let device = shown(device.device.as_deref().unwrap_or("a device"));
	writeln!(
		output,
		"Gave the secrets of {} to {}'s {device}; they reach it with the next sync.",
		shown(space),
		shown(member)
	)?;
	Ok(())
}

/// Prints the changes that were set aside, by the replica that made them.
pub(crate) fn set_aside(
	set_aside: &[nibkeep_node::SetAside],
	replicas: &[Replica],
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	let replica_of = |id: Option<EntityId>| replicas.iter().find(|replica| Some(replica.id) == id);
	if json {
		return print_json(
			&serde_json::Value::Array(
				set_aside
					.iter()
					.map(|aside| {
						json!({
							"change": aside.change.to_string(),
							"replica": aside.replica.map(|replica| replica.to_string()),
							"millis": aside.millis,
							"operations": aside.operations,
							"governs": aside.governs,
						})
					})
					.collect(),
			),
			output,
		);
	}
	if set_aside.is_empty() {
		writeln!(output, "No changes are set aside.")?;
		return Ok(());
	}
	for aside in set_aside {
		let by = replica_of(aside.replica)
			.map_or_else(|| "a device nobody added".to_owned(), replica_text);
		let at = jiff::Timestamp::from_millisecond(i64::try_from(aside.millis).unwrap_or(i64::MAX))
			.map_or_else(|_| "an unknown time".to_owned(), |at| at.to_string());
		let what = if aside.governs {
			"which says who may write to the library, and is never accepted".to_owned()
		} else {
			aside.operations.map_or_else(
				|| "that this device cannot read".to_owned(),
				|count| {
					if count == 1 {
						"of 1 operation".to_owned()
					} else {
						format!("of {count} operations")
					}
				},
			)
		};
		let change = aside.change.to_string();
		// The hash's first eight digits, after the name of its algorithm.
		let short = change
			.split_once(':')
			.map_or(change.as_str(), |(_, digits)| {
				&digits[..digits.len().min(8)]
			});
		writeln!(output, "{short}  by {by}, at {at}, {what}")?;
	}
	writeln!(
		output,
		"Accept a device's changes with nib device accept DEVICE --yes."
	)?;
	Ok(())
}

/// Prints how many changes that were set aside were accepted, or would be without
/// `confirmed`.
pub(crate) fn accepted(
	accepted: usize,
	confirmed: bool,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		return print_json(
			&json!({ "accepted": confirmed, "changes": accepted }),
			output,
		);
	}
	if accepted == 0 {
		writeln!(output, "None of its changes are set aside.")?;
	} else if confirmed {
		writeln!(output, "Accepted {}.", changes_text(accepted))?;
	} else {
		writeln!(output, "Would accept {}.", changes_text(accepted))?;
	}
	Ok(())
}

/// Prints what a new device's first sync brought.
pub(crate) fn joined(
	(synced, refused): (&nibkeep_node::Synced, usize),
	secrets: Option<(nibkeep_node::SecretsState, bool)>,
	copies: Option<&Copies>,
	json: bool,
	output: &mut dyn Write,
) -> Result<(), CliError> {
	if json {
		let mut joined = json!({
			"synced": spaces_json(&synced.spaces),
			"refused": refused,
		});
		if let Some((state, _)) = secrets {
			joined["secrets"] = json!(secrets_state_text(state));
		}
		if let Some(copies) = copies {
			joined["copies"] = json!({
				"taken_in": copies.taken.items,
				"secrets_left": copies.secrets_left,
				"devices": copies.devices,
				"folders": copies.folders,
				"bytes": copies.size,
			});
		}
		return print_json(&joined, output);
	}
	writeln!(output, "Joined the library: {}.", synced_text(synced))?;
	refused_text(refused, output)?;
	if let Some(secrets) = secrets {
		paired_secrets(secrets, output)?;
	}
	if let Some(copies) = copies {
		copies_text(copies, output)?;
	}
	Ok(())
}

/// The copies beside a new replica, made elsewhere, and what it took in of them.
pub(crate) struct Copies {
	/// What it took in.
	pub(crate) taken: nibkeep_node::TakenIn,
	/// Whether a copy holds secrets that this device does not.
	pub(crate) secrets_left: bool,
	/// The names of the devices they are copies of, as far as the library knows them.
	pub(crate) devices: Vec<String>,
	/// Their folders.
	pub(crate) folders: Vec<std::path::PathBuf>,
	/// How many bytes they take.
	pub(crate) size: u64,
}

/// What a new replica took in of the copies beside it, which devices they are copies of,
/// and what the person may do about them.
fn copies_text(copies: &Copies, output: &mut dyn Write) -> Result<(), CliError> {
	let of = match copies.devices.as_slice() {
		[] => "another device".to_owned(),
		devices => devices.join(", "),
	};
	match copies.taken.items {
		0 => writeln!(
			output,
			"A copy of the library from {of} is here; it held nothing new."
		)?,
		1 => writeln!(
			output,
			"Took in 1 change, key or file that only a copy from {of} held."
		)?,
		items => writeln!(
			output,
			"Took in {items} changes, keys and files that only a copy from {of} held."
		)?,
	}
	if copies.secrets_left {
		writeln!(
			output,
			"The copy holds secrets that this device does not: take them with nib secret from-copy first, as revoking the copy's device loses them if it alone held them."
		)?;
	}
	for device in &copies.devices {
		writeln!(
			output,
			"If {device} is gone, revoke it once this device has synced with your others: nib device revoke \"{device}\""
		)?;
	}
	for folder in &copies.folders {
		writeln!(
			output,
			"The copy stays as it was, in {}. NibKeep never deletes it, as a program that syncs the folder would delete the original too; delete it yourself if none does.",
			folder.display()
		)?;
	}
	writeln!(output, "It takes {}.", amount_text(copies.size))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use nibkeep_base::cbor;
	use nibkeep_base::{Hash, SpaceId};
	use nibkeep_node::{CheckStatus, Part, Problem, Report, Round, SpaceReport};

	use super::{
		Checks, backup_update_lines, checks_text, imported, kept_text, question, recovered,
		recovered_out_of_reach, report, revoked, revoked_elsewhere, type_definition, update_json,
	};

	#[test]
	fn an_import_names_the_types_it_left_out() {
		let mut report = nibkeep_node::Imported {
			entities: 3,
			added: 5,
			..nibkeep_node::Imported::default()
		};
		let printed = |report: &nibkeep_node::Imported, json: bool| {
			let mut output = Vec::new();
			imported(report, std::path::Path::new("export"), json, &mut output).expect("prints");
			String::from_utf8(output).expect("text")
		};
		assert_eq!(
			printed(&report, false),
			"Imported from export: 5 values added and 0 values replaced, of 3 entities, and 0 files; left out 0 records of users, devices, replicas and spaces\n"
		);
		report.types_left_out = vec![
			"local.note".parse().expect("a name"),
			"local.recipe".parse().expect("a name"),
		];
		assert!(
			printed(&report, false).ends_with(
				"left out 0 records of users, devices, replicas and spaces, and the export's definitions of local.note, local.recipe, which the library defines otherwise\n"
			)
		);
		let json: serde_json::Value = serde_json::from_str(&printed(&report, true)).expect("JSON");
		assert_eq!(
			json["types_left_out"],
			serde_json::json!(["local.note", "local.recipe"])
		);
		assert_eq!(json["person"], serde_json::Value::Null);
	}

	#[test]
	fn an_import_names_the_person_it_took_for_the_user() {
		let id: nibkeep_base::EntityId = "d8342faa-8153-4ebd-b914-83600894c2af"
			.parse()
			.expect("an ID");
		let report = nibkeep_node::Imported {
			person: Some(nibkeep_node::Described {
				id,
				name: Some("Ada Lovelace".to_owned()),
			}),
			..nibkeep_node::Imported::default()
		};
		let mut output = Vec::new();
		imported(&report, std::path::Path::new("export"), false, &mut output).expect("prints");
		assert!(
			String::from_utf8(output).expect("text").ends_with(
				"spaces\nYou are Ada Lovelace now, the person of the export's user; the person you were, with only a name, is deleted.\n"
			)
		);
		let mut output = Vec::new();
		imported(&report, std::path::Path::new("export"), true, &mut output).expect("prints");
		let json: serde_json::Value = serde_json::from_slice(&output).expect("JSON");
		assert_eq!(
			json["person"],
			serde_json::json!({ "id": id.to_string(), "name": "Ada Lovelace" })
		);
	}

	#[test]
	fn a_replica_with_no_copies_to_check_says_nothing_of_checks() {
		let round = Round {
			began: 0,
			blobs: Part::default(),
			plain: Part::default(),
		};
		let zone = jiff::tz::TimeZone::UTC;
		let printed = |holds: bool| {
			let status = CheckStatus {
				holds,
				round: Some(round.clone()),
				last: None,
				damaged: Vec::new(),
			};
			let checks = Checks {
				status: Some(&status),
				name_of: &|_, _| None,
				zone: &zone,
			};
			let mut output = Vec::new();
			checks_text(&checks, &mut output).expect("prints");
			String::from_utf8(output).expect("text")
		};
		assert_eq!(printed(false), "", "no round of 0 bytes");
		assert!(printed(true).starts_with("Checks: this round began"));
	}

	#[test]
	fn the_devices_kept_by_a_recovery_are_told_how_they_learn_of_the_new_one() {
		let backup = nibkeep_node::RecoveredFrom::Backup {
			name: Some("Blue stick".to_owned()),
			updated: false,
		};
		let kept = ["desk".to_owned(), "phone".to_owned()];
		assert_eq!(
			kept_text(&kept, true, &backup),
			" Kept desk and phone, which you still have: they learn of this device, and of the revocations, the first time this device syncs with them: run nib sync here while they serve, as nib sync --serve, syncing from login or an app does; or update there a backup that carries this device."
		);
		assert_eq!(
			kept_text(&kept[..1], false, &backup),
			" Kept desk, which you still have: it learns of this device the first time this device syncs with it: run nib sync here while it serves, as nib sync --serve, syncing from login or an app does; or update there a backup that carries this device."
		);
		assert_eq!(kept_text(&[], true, &backup), "");
	}

	#[test]
	fn the_spaces_where_devices_that_a_revoked_one_added_went_too_are_named() {
		let family = SpaceId::from_bytes([7; 16]);
		let named = [(family, "Family".to_owned())];
		let mut json = Vec::new();
		revoked(&[], (&[], false), (true, &named), &[], true, &mut json).expect("prints");
		let json: serde_json::Value = serde_json::from_slice(&json).expect("JSON");
		assert_eq!(
			json["also_revoked_in"],
			serde_json::json!([family.to_string()])
		);
		let back = nibkeep_node::Recovered {
			revoked: Vec::new(),
			kept: Vec::new(),
			spaces: vec![family],
			missing: Vec::new(),
			out_of_reach: Vec::new(),
			secrets: nibkeep_node::RecoveredSecrets::NoneHeld,
			from: nibkeep_node::RecoveredFrom::Backup {
				name: Some("Blue stick".to_owned()),
				updated: true,
			},
			revoked_elsewhere: vec![family],
		};
		let mut text = Vec::new();
		recovered(
			&back,
			(&["Family".to_owned()], &["Family".to_owned()], 0),
			false,
			&mut text,
		)
		.expect("prints");
		let text = String::from_utf8(text).expect("text");
		assert!(
			text.contains("Your spaces came back too: Family."),
			"{text}"
		);
		assert!(
			text.contains(
				"Also revoked, in Family, devices of yours that a revoked device had added there"
			),
			"{text}"
		);
	}

	#[test]
	fn the_spaces_not_on_this_device_are_named_with_the_devices_that_hold_them() {
		let project = SpaceId::from_bytes([8; 16]);
		let taxes = SpaceId::from_bytes([9; 16]);
		let desktop = nibkeep_node::SpaceHolder {
			replica: nibkeep_base::EntityId::from_bytes([1; 16]),
			device: Some("desktop".to_owned()),
			app: Some("nib".to_owned()),
			this: false,
		};
		let elsewhere = vec![
			nibkeep_node::HeldElsewhere {
				space: project,
				name: Some("Project".to_owned()),
				holders: vec![desktop],
			},
			nibkeep_node::HeldElsewhere {
				space: taxes,
				name: Some("Taxes".to_owned()),
				holders: Vec::new(),
			},
		];
		let mut text = Vec::new();
		revoked_elsewhere((true, &[]), &elsewhere, &mut text).expect("prints");
		assert_eq!(
			String::from_utf8(text).expect("text"),
			"Not on this device, so it is revoked there once a device of yours that holds it \
			 syncs with this one: Project (on desktop).\nOn no other device of yours, so none \
			 can revoke it there: Taxes. It keeps what it holds there, and where others share the \
			 space, your place, until an admin removes you and invites you again.\n"
		);
		assert_eq!(
			recovered_out_of_reach(&elsewhere),
			" Not on this device, so your lost devices are revoked there once a device of yours \
			 that holds it syncs with this one: Project (on desktop). Only your lost devices held \
			 Taxes, so they keep it."
		);
		let updated = nibkeep_node::BackupUpdate {
			elsewhere,
			..nibkeep_node::BackupUpdate::default()
		};
		let mut text = Vec::new();
		backup_update_lines(&updated, &mut text).expect("prints");
		let text = String::from_utf8(text).expect("text");
		assert!(
			text.ends_with(
				"Not on this device, so not updated now: Project (on desktop).\nOn none of your \
				 devices, so no update adds to it any more: Taxes.\n"
			),
			"{text}"
		);
		assert_eq!(
			update_json(&updated)["elsewhere"][0]["holders"],
			serde_json::json!(["desktop"])
		);
	}

	#[test]
	fn a_large_files_size_is_rounded_for_people_and_a_policys_limit_is_not() {
		let asked = question(
			std::path::Path::new("film.mkv"),
			&nibkeep_node::Question {
				folder: "films".into(),
				size: 4_718_592_317,
				rule: nibkeep_base::EntityId::from_bytes([1; 16]),
			},
		);
		assert!(asked.starts_with("film.mkv is 4.7 GB. "), "{asked}");
		assert_eq!(super::size_text(4 << 30), "4 GiB");
	}

	#[test]
	fn a_field_secret_in_the_space_is_shown_secret_whatever_its_type_says() {
		let password: nibkeep_model::FieldName = "std.password".parse().expect("a name");
		let definition = nibkeep_model::TypeDef::new("local.account".parse().expect("a name"), 1)
			.with_field(
				password.clone(),
				nibkeep_model::FieldDef::new(nibkeep_model::ValueKind::Text),
			);
		let secret = std::collections::BTreeSet::from([password]);
		let languages = nibkeep_model::Languages::default();
		let mut text = Vec::new();
		type_definition(&definition, (&languages, &secret), false, &mut text).expect("prints");
		assert_eq!(
			String::from_utf8(text).expect("UTF-8"),
			"local.account, version 1\n  std.password  text, secret\n"
		);
		let mut json = Vec::new();
		type_definition(&definition, (&languages, &secret), true, &mut json).expect("prints");
		let json: serde_json::Value = serde_json::from_slice(&json).expect("JSON");
		assert_eq!(json["fields"][0]["secret"], true);
	}

	#[test]
	fn problems_are_listed_under_their_space() {
		let content = Hash::of(b"a scan");
		let change = Hash::of(b"a change");
		let found = Report {
			spaces: vec![SpaceReport {
				id: SpaceId::from_bytes([7; 16]),
				name: Some("Personal".to_owned()),
				changes: 1,
				blobs: 2,
				waiting: 1,
				in_place: 0,
				changed: Vec::new(),
				problems: vec![
					Problem::DamagedBlob(content),
					Problem::Database("row 3 missing from index".to_owned()),
					Problem::ForgedChange(
						cbor::from_slice(&cbor::to_vec(&change)).expect("a change's ID"),
					),
				],
			}],
		};
		let mut output = Vec::new();
		report(&found, false, &mut output).expect("prints");
		assert_eq!(
			String::from_utf8(output).expect("UTF-8"),
			format!(
				"\
Personal: 1 change and 2 blobs checked, 1 blob waiting for its key, damaged
  the copy of blob {content} is damaged: it is moved aside, and the next sync brings it again from a device that holds it intact
  the log file is damaged: row 3 missing from index
  change {change} has a bad signature: the log file was damaged or tampered with
"
			)
		);
	}
}
