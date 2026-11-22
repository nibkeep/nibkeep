//! What an app does with syncing from login ([technical design](../../../../docs/Technical-design.md#106-syncing-from-login)):
//! the switch, which has the copy of `nibkeep-sync` that the app ships beside its program sync
//! a library whenever the person is logged in; and what the app does as it starts. Only on
//! computers.
//!
//! Tests keep apps from the person's list and login entry: with `NIBKEEP_NO_KEYSTORE`, as
//! they run the apps, there is none, and once a test has named a place of its own, or none,
//! the process keeps to it.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use nibkeep_base::EntityId;

use super::{
	AtLoginStatus, EntryOutcome, LoginEntry, Served, TurnedOff, service_version, start_service,
};
use crate::NodeError;

/// Where syncing from login keeps its list and login entry, and the app's own copy of
/// `nibkeep-sync`.
#[derive(Clone)]
struct Place {
	settings: PathBuf,
	entry: Option<LoginEntry>,
	own: Option<PathBuf>,
	/// Whether the app starts copies: a test's place says what it would start instead.
	starts: bool,
}

/// Which place an app uses.
#[derive(Clone)]
enum Chosen {
	/// The person's.
	Persons,
	/// A test's.
	Tests(Place),
	/// None, as a test chose.
	Nowhere,
}

/// The place that an app uses: the person's, until a test chooses.
static CHOSEN: Mutex<Chosen> = Mutex::new(Chosen::Persons);

/// For tests, which keep away from the person's: puts the list in the folder `folder`, the
/// login entry, if any, in the autostart file `entry`, and takes `program` for the app's own
/// copy, which it then never starts. Without a folder, the app has no syncing from login.
pub fn set_test_place(folder: Option<PathBuf>, entry: Option<PathBuf>, program: Option<PathBuf>) {
	let chosen = folder.map_or(Chosen::Nowhere, |settings| {
		Chosen::Tests(Place {
			settings,
			entry: entry.map(LoginEntry::autostart),
			own: program,
			starts: false,
		})
	});
	*CHOSEN.lock().unwrap_or_else(PoisonError::into_inner) = chosen;
}

/// Whether tests keep the program from the computer's keystore, as `NIBKEEP_NO_KEYSTORE`
/// says: an app, `nib` or `nibkeep-sync`. They keep an app from the person's syncing from
/// login too.
#[must_use]
pub fn kept_away() -> bool {
	std::env::var_os("NIBKEEP_NO_KEYSTORE").is_some_and(|off| !off.is_empty())
}

/// Where syncing from login is: a test's, or the person's; none on phones, nor where tests
/// keep the app away.
fn place() -> Option<Place> {
	match CHOSEN
		.lock()
		.unwrap_or_else(PoisonError::into_inner)
		.clone()
	{
		Chosen::Tests(place) => return Some(place),
		Chosen::Nowhere => return None,
		Chosen::Persons => {}
	}
	if kept_away() {
		return None;
	}
	#[cfg(not(any(target_os = "android", target_os = "ios")))]
	return Some(Place {
		settings: super::settings_folder()?,
		entry: LoginEntry::platform(),
		own: own_copy(),
		starts: true,
	});
	#[cfg(any(target_os = "android", target_os = "ios"))]
	None
}

/// The app's own copy of `nibkeep-sync`, beside its program, if it ships one.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn own_copy() -> Option<PathBuf> {
	let name = format!("{}{}", super::SERVICE_PROGRAM, std::env::consts::EXE_SUFFIX);
	let copy = std::env::current_exe().ok()?.with_file_name(name);
	copy.is_file().then_some(copy)
}

/// Starts a copy of `nibkeep-sync`, unless the place is a test's: the copy started, or that
/// would be.
fn start(place: &Place, program: Option<PathBuf>) -> Result<Option<PathBuf>, NodeError> {
	if let Some(program) = &program
		&& place.starts
	{
		start_service(program)?;
	}
	Ok(program)
}

/// Whether the app can turn syncing from login on here: on a computer where it ships
/// `nibkeep-sync`.
#[must_use]
pub fn available() -> bool {
	place().is_some_and(|place| place.own.is_some())
}

/// What the app does as it starts, as [`super::app_started`] says: the copy of
/// `nibkeep-sync` that it started, if any.
///
/// # Errors
///
/// Returns the errors of [`super::app_started`] and [`start_service`].
pub fn started() -> Result<Option<PathBuf>, NodeError> {
	let Some(place) = place() else {
		return Ok(None);
	};
	let Some(own) = &place.own else {
		return Ok(None);
	};
	let program = super::app_started(&place.settings, own, &service_version, place.entry.as_ref())?;
	start(&place, program)
}

/// What syncing from login does for the library in `directory`, whose replica here is
/// `replica`: `None` where the app cannot turn it on here.
#[must_use]
pub fn status(directory: &std::path::Path, replica: Option<EntityId>) -> Option<AtLoginStatus> {
	let place = place().filter(|place| place.own.is_some())?;
	Some(super::status(
		&place.settings,
		(directory, replica),
		place.entry.as_ref(),
	))
}

/// What turning syncing from login on did, for an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppTurnedOn {
	/// What became of the login entry.
	pub entry: EntryOutcome,
	/// What became of Windows' firewall: `opened`, or `refused`; elsewhere, or where its rule
	/// is there, `None`.
	pub firewall: Option<&'static str>,
	/// The copy of `nibkeep-sync` that it started, or that a test's place would.
	pub started: Option<PathBuf>,
}

/// Turns syncing from login on, as [`super::turn_on`] does with the app's own copy; lets the
/// other devices through Windows' firewall, asking for the administrator's consent, before it
/// starts the copy that should run, so that Windows does not ask about that one, whose
/// refusal would block it. `None` where the app cannot turn it on here. It may wait for
/// copies to tell their versions, and for the person's consent: never while the app holds its
/// library.
///
/// # Errors
///
/// Returns the errors of [`super::turn_on`] and [`start_service`].
pub fn turn_on(served: Served) -> Result<Option<AppTurnedOn>, NodeError> {
	let Some(place) = place() else {
		return Ok(None);
	};
	let Some(own) = &place.own else {
		return Ok(None);
	};
	let turned_on = super::turn_on(
		&place.settings,
		served,
		(own, &service_version),
		place.entry.as_ref(),
	)?;
	let firewall = if place.starts { firewall() } else { None };
	let started = start(&place, turned_on.start)?;
	Ok(Some(AppTurnedOn {
		entry: turned_on.entry,
		firewall,
		started,
	}))
}

/// Lets the other devices through Windows' firewall, unless its rule is there: `opened`, or
/// `refused`; elsewhere, nothing to do.
fn firewall() -> Option<&'static str> {
	#[cfg(windows)]
	{
		use super::firewall;
		if firewall::has_rule() {
			None
		} else if firewall::add_rule() {
			Some("opened")
		} else {
			Some("refused")
		}
	}
	#[cfg(not(windows))]
	None
}

/// Turns syncing from login off for the library in `directory`, or its replica here, as
/// [`super::turn_off`] does: nothing where the app has no syncing from login.
///
/// # Errors
///
/// Returns the errors of [`super::turn_off`].
pub fn turn_off(
	directory: &std::path::Path,
	replica: Option<EntityId>,
) -> Result<TurnedOff, NodeError> {
	let Some(place) = place() else {
		return Ok(TurnedOff {
			removed: false,
			entry_failed: None,
		});
	};
	super::turn_off(&place.settings, (directory, replica), place.entry.as_ref())
}
