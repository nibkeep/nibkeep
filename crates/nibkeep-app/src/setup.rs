//! Setting a library up: whether a folder holds one and how its keys are protected; making
//! one, joining one on another device, making this device from a copy of one, and unlocking
//! it; protecting its keys anew, with a passphrase or the platform's keystore
//! ([`crate::keystore`]). A library is a folder that holds `.nib`, as `nib` keeps it, so
//! that apps and `nib` use the same libraries.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::EntityId;
use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{
	CONFIRMED_KEYSTORE_SERVICE, Joining, KEYSTORE_SERVICE, KeysError, Library, NetworkOptions,
	NewLibrary, NewProtection, NodeError, PairingCode, Protection, Unlocker,
};
use serde_json::{Value as Json, json};

use crate::held::{hold, keep, let_go, waiting};
use crate::keystore::keystore;
use crate::{Failure, app, clock, syncing, take, wall_millis, wipe};

/// The folder in a library's folder that holds what the library keeps.
const LIBRARY_DATA: &str = ".nib";

/// Where, in a library's folder, the library that joining another replaces waits until the
/// new one is joined: it goes then, and comes back if joining fails.
const REPLACED: &str = ".nib.replaced";

/// How long joining looks for the device that shows the code; tests look less.
const FIND_SPONSOR: Duration = if cfg!(any(test, feature = "testing")) {
	Duration::from_secs(5)
} else {
	Duration::from_mins(1)
};

/// The cost of deriving a key from a new passphrase: little memory, as phones need; tests
/// pay less.
///
/// # Panics
///
/// Never: the tests' parameters are valid.
#[must_use]
pub fn params() -> PassphraseParams {
	if cfg!(any(test, feature = "testing")) {
		PassphraseParams::new(64, 1, 1).expect("valid")
	} else {
		PassphraseParams::LOW_MEMORY
	}
}

/// Where a library's folder keeps the library.
#[must_use]
pub fn data(folder: &str) -> PathBuf {
	Path::new(folder).join(LIBRARY_DATA)
}

/// How keys are protected, or unlocked: by a passphrase, which is never empty, or with
/// `keystore`, by the platform's keystore.
pub struct Protect<'a> {
	/// The passphrase, unless the keystore protects them.
	pub passphrase: &'a str,
	/// Whether the keystore protects them.
	pub keystore: bool,
}

impl<'a> Protect<'a> {
	/// How a request protects or unlocks keys: by `passphrase`, or with its `keystore`, by
	/// the keystore.
	#[must_use]
	pub fn of(request: &Json, passphrase: &'a str) -> Self {
		Self {
			passphrase,
			keystore: request["keystore"].as_bool().unwrap_or(false),
		}
	}

	/// The protection of the library's new keys.
	///
	/// # Errors
	///
	/// A [`Failure::Request`] if there is no keystore here, or the passphrase is empty.
	pub fn new_protection(&self) -> Result<NewProtection<'a>, Failure> {
		if self.keystore {
			if !keystore() {
				return Err(Failure::Request(
					"there is no keystore here; give a passphrase".to_owned(),
				));
			}
			return Ok(NewProtection::Keystore {
				service: KEYSTORE_SERVICE,
			});
		}
		self.new_passphrase()
	}

	/// The protection of the secrets' new keys, in an app that reads them: in the keystore
	/// under the app's service ([`crate::Secrets`]).
	///
	/// # Errors
	///
	/// A [`Failure::Request`] if the app reads no secrets, the secrets' key cannot go in
	/// the keystore here, or the passphrase is empty.
	pub fn new_secrets_protection(&self) -> Result<NewProtection<'a>, Failure> {
		let Some(secrets) = crate::secrets() else {
			return Err(Failure::Request("this app keeps no secrets".to_owned()));
		};
		if self.keystore {
			if !(secrets.keystore)() {
				return Err(Failure::Request(
					"this device cannot keep the secrets' key in its keystore; give a passphrase"
						.to_owned(),
				));
			}
			return Ok(NewProtection::Keystore {
				service: (secrets.service)(),
			});
		}
		self.new_passphrase()
	}

	fn new_passphrase(&self) -> Result<NewProtection<'a>, Failure> {
		if self.passphrase.is_empty() {
			return Err(Failure::Request("the passphrase is empty".to_owned()));
		}
		Ok(NewProtection::Passphrase(self.passphrase, params()))
	}

	/// What unlocks keys protected so.
	///
	/// # Errors
	///
	/// A [`Failure::Request`] if there is no keystore here, and a wrong passphrase if the
	/// passphrase is empty.
	pub fn unlocker(&self) -> Result<Unlocker<'_>, Failure> {
		if self.keystore {
			if !keystore() {
				return Err(Failure::Request(
					"there is no keystore here; give the passphrase".to_owned(),
				));
			}
			return Ok(Unlocker::Keystore);
		}
		if self.passphrase.is_empty() {
			return Err(Failure::Node(NodeError::Keys(KeysError::WrongPassphrase)));
		}
		Ok(Unlocker::Passphrase(self.passphrase))
	}
}

/// How keys are protected, in a word: `passphrase`, `keystore`, or `confirmed` for a
/// phone's keystore that asks for a fingerprint or the screen lock each time.
#[must_use]
pub fn protection_kind(protection: &Protection) -> &'static str {
	match protection {
		Protection::Passphrase { .. } => "passphrase",
		Protection::Keystore { service, .. } if service == CONFIRMED_KEYSTORE_SERVICE => {
			"confirmed"
		}
		Protection::Keystore { .. } => "keystore",
	}
}

/// Whether a library's folder holds a library, or one whose making was cut short, how its
/// keys are protected, and whether the platform has a keystore to keep keys in, and the
/// secrets' key, in an app that reads secrets. A library that must not be opened here as
/// it is says why, in `problem`, and in a word: `unfinished`, `copied` (only copies of
/// replicas made elsewhere, with how the copy's keys and secrets are protected), or
/// `removed` (its drive must be checked first). On a drive without a journal
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#73-where-a-replica-may-live)),
/// whether a new library there needs the person's leave. Whether bringing a library back
/// there with a recovery code was cut short, which a recovery run again starts over.
///
/// # Errors
///
/// Where the app may not read the folder, or the engine fails.
pub fn library_at(folder: &str) -> Result<Json, Failure> {
	finish_replacing(folder);
	let directory = data(folder);
	let exists = Library::exists(&directory);
	// A folder that the system lets the app not read, as macOS does with a removable drive
	// until the person allows it, is not taken for one that holds no library.
	if !exists
		&& let Err(error) = std::fs::read_dir(&directory)
		&& error.kind() == std::io::ErrorKind::PermissionDenied
	{
		return Err(Failure::Request(format!(
			"the app may not read {folder}: {error}"
		)));
	}
	let (mut protection, mut problem) = (None, None);
	let (mut unfinished, mut copied, mut removed, mut cut_short) = (false, false, false, false);
	if exists {
		match Library::protection(&directory) {
			Ok(found) => protection = Some(protection_kind(&found)),
			// A recovery run again starts it over.
			Err(NodeError::RecoveryCutShort) => cut_short = true,
			Err(
				error @ (NodeError::Unfinished | NodeError::Copied | NodeError::UncleanRemoval),
			) => {
				unfinished = matches!(error, NodeError::Unfinished);
				// Only copies of replicas made elsewhere, which this device never uses as the
				// devices they were (technical design: copies of a replica).
				copied = matches!(error, NodeError::Copied);
				removed = matches!(error, NodeError::UncleanRemoval);
				problem = Some(error.to_string());
			}
			Err(error) => return Err(error.into()),
		}
	}
	let no_journal = (!exists || copied)
		&& matches!(Library::check_place(&directory), Err(NodeError::NoJournal));
	// How the copy's keys and secrets are protected, so that the app knows what to ask for.
	let copy = copied
		.then(|| Library::copies_in(&directory).into_iter().next())
		.flatten()
		.and_then(|copy| copy.protections().ok());
	Ok(json!({
		"exists": exists,
		"unfinished": unfinished,
		"copied": copied,
		"copy_protection": copy.as_ref().map(|(keys, _)| protection_kind(keys)),
		"copy_secrets": copy.as_ref().and_then(|(_, secrets)| secrets.as_ref().map(protection_kind)),
		"protection": protection,
		"removed": removed,
		"problem": problem,
		"recovery_cut_short": cut_short,
		"no_journal": no_journal,
		"keystore": keystore(),
		"secrets_keystore": crate::secrets().is_some_and(|secrets| (secrets.keystore)()),
	}))
}

/// Lets a library in a folder on a drive without a journal, as FAT and exFAT, as the person
/// chose, once told what it risks.
///
/// # Errors
///
/// Where the engine fails.
pub fn allow_fat(folder: &str) -> Result<Json, Failure> {
	Library::allow_fat(&data(folder))?;
	Ok(json!({}))
}

/// Says that the drive of a library was checked for errors after it was removed, or lost its
/// power, while the library was open, so that the library opens again; the app verifies it
/// as it unlocks it.
///
/// # Errors
///
/// Where the engine fails.
pub fn drive_checked(folder: &str) -> Result<Json, Failure> {
	Library::drive_checked(&data(folder))?;
	Ok(json!({}))
}

/// Removes a library whose making was cut short, as by a power cut, so that it can be made
/// again; only such a library.
///
/// # Errors
///
/// Where there is no such library, or it cannot be removed.
pub fn remove_unfinished(folder: &str) -> Result<Json, Failure> {
	let directory = data(folder);
	match Library::protection(&directory) {
		// Only this device's replica's folder: copies of others beside it stay, as a program
		// that syncs the folder would delete the originals too.
		Err(NodeError::Unfinished) => {
			let replica = Library::replica_folder(&directory)?;
			std::fs::remove_dir_all(&replica).map_err(|error| {
				Failure::Request(format!("the unfinished library cannot be removed: {error}"))
			})?;
			Ok(json!({}))
		}
		_ => Err(Failure::Request(
			"there is no unfinished library there".to_owned(),
		)),
	}
}

/// Says that the request's `folder` is the app's own on a phone, with the phone's ID, in
/// hex in `id`, which the app keeps where no other phone gets it, so that its library tells
/// itself from a copy of it (technical design: copies of a replica), and with `disk`, if the
/// phone gives one: the name that it gives one developer's apps for itself, as Android
/// does, by which their replicas count as on one disk (technical design: where the copies
/// are).
pub fn declare_phone(request: &mut Json) -> Json {
	let id = take(request, "id");
	let id = (0..id.len())
		.step_by(2)
		.map(|at| {
			id.get(at..at + 2)
				.and_then(|pair| u8::from_str_radix(pair, 16).ok())
		})
		.collect::<Option<Vec<u8>>>()
		.unwrap_or_default();
	let disk = take(request, "disk");
	nibkeep_node::declare_phone_with_disk(
		&data(&take(request, "folder")),
		id,
		disk.as_bytes().to_vec(),
	);
	json!({})
}

/// Unlocks the library in the request's `folder` with its `passphrase`, or with
/// `keystore`, the keystore, and holds it, closed, as [`unlock_library`] does: with
/// `verify`, as after its drive was checked, every change and blob is read first. The
/// library held before goes first, also if this one does not unlock.
///
/// # Errors
///
/// Where it does not unlock.
pub fn unlock(request: &mut Json) -> Result<Json, Failure> {
	let_go();
	*hold() = None;
	let passphrase = take(request, "passphrase");
	let directory = data(&take(request, "folder"));
	let protect = Protect::of(request, &passphrase);
	let verify = request["verify"].as_bool() == Some(true);
	unlock_library(&directory, &protect.unlocker()?, verify)
}

/// Unlocks the library in `directory` with `unlocker`, and holds it, closed, in place of
/// the library held before, whose endpoint goes first. Says how long unlocking took, and
/// with `verify`, which problems reading every change and blob found.
///
/// # Errors
///
/// Where it does not unlock, or does not read.
pub fn unlock_library(
	directory: &Path,
	unlocker: &Unlocker<'_>,
	verify: bool,
) -> Result<Json, Failure> {
	let_go();
	let mut held = hold();
	*held = None;
	let started = Instant::now();
	let mut library = waiting(|| Library::open(directory, unlocker))?;
	let seconds = started.elapsed().as_secs_f64();
	let problems = if verify {
		Some(library.verify(wall_millis())?.problems())
	} else {
		None
	};
	*held = Some(library.close());
	Ok(json!({"seconds": seconds, "problems": problems}))
}

/// The folders that making `folder` makes, which go again if what is made there fails.
fn to_make(folder: &str) -> Vec<PathBuf> {
	Path::new(folder)
		.ancestors()
		.take_while(|folder| !folder.exists())
		.map(Path::to_path_buf)
		.collect()
}

/// Removes what a failure left of a library in `directory`, and the folders `made` for it:
/// only empty folders go.
fn remove_empty(directory: &Path, made: &[PathBuf]) {
	let _ = std::fs::remove_dir(directory);
	for folder in made {
		let _ = std::fs::remove_dir(folder);
	}
}

/// Makes a library in the request's `folder`, as [`create_library`] does, with the names
/// of its `user` and `device`, its keys protected by its `passphrase` or with `keystore`,
/// the keystore, and holds it, unlocked.
///
/// # Errors
///
/// As [`create_library`].
pub fn create(request: &mut Json) -> Result<Json, Failure> {
	let folder = take(request, "folder");
	let passphrase = take(request, "passphrase");
	let protect = Protect::of(request, &passphrase);
	let names = (take(request, "user"), take(request, "device"));
	keep(create_library(&folder, (&names.0, &names.1), &protect)?);
	Ok(json!({}))
}

/// Makes a library in a folder: the user's name, this device's, and its keys protected as
/// `protect` says. It is unlocked afterwards. If making it fails, the folders it made go
/// again, where they are empty.
///
/// # Errors
///
/// Where a name is empty, the protection cannot be had, a library is there already, or
/// the engine fails.
pub fn create_library(
	folder: &str,
	(user, device): (&str, &str),
	protect: &Protect<'_>,
) -> Result<Library, Failure> {
	let user = user.trim();
	let device = device.trim();
	if user.is_empty() || device.is_empty() {
		return Err(Failure::Request(
			"a library needs your name and this device's".to_owned(),
		));
	}
	let protection = protect.new_protection()?;
	let directory = data(folder);
	if Library::replica_folder(&directory).is_ok() {
		return Err(NodeError::Exists.into());
	}
	// Before the folder is made, as a program that syncs it would take it to the cloud.
	Library::check_place(&directory)?;
	let made = to_make(folder);
	std::fs::create_dir_all(folder)
		.map_err(|error| Failure::Request(format!("the folder cannot be made: {error}")))?;
	Library::create(
		&directory,
		&NewLibrary::new(user, device, app().name),
		&protection,
		wall_millis(),
		&mut UnwrapErr(SysRng),
	)
	.map_err(|error| {
		remove_empty(&directory, &made);
		error.into()
	})
}

/// Joins the library of another device of the person's, as [`join_library`] does, into
/// the request's `folder`, with the `code` it shows, as the device named `device`, with
/// its keys protected by `passphrase` or with `keystore`, the keystore; with `replace`,
/// in place of the library there. In an app that reads secrets, with `secrets`, the
/// secrets too, protected as it says. Holds the library, unlocked.
///
/// # Errors
///
/// As [`join_library`].
pub fn join(request: &mut Json) -> Result<Json, Failure> {
	let folder = take(request, "folder");
	let code = take(request, "code");
	let device = take(request, "device");
	let passphrase = take(request, "passphrase");
	let protect = Protect::of(request, &passphrase);
	let replace = request["replace"].as_bool() == Some(true);
	// Taken out of the request, so wiped here.
	let mut secrets = request.get_mut("secrets").map(Json::take);
	let asked = secrets
		.as_mut()
		.filter(|secrets| secrets.is_object() && crate::secrets().is_some())
		.map(|secrets| {
			let keystore = secrets["keystore"].as_bool().unwrap_or(false);
			(take(secrets, "passphrase"), keystore)
		});
	let asks = asked.as_ref().map(|(passphrase, keystore)| Protect {
		passphrase,
		keystore: *keystore,
	});
	let joined = join_library(&folder, (&code, &device), &protect, asks.as_ref(), replace);
	if let Some(secrets) = secrets.as_mut() {
		wipe(secrets);
	}
	keep(joined?);
	Ok(json!({}))
}

/// Joins the library of another device of the person's, which shows `code`, on the same
/// network, as a replica of its own in a folder named `device`, with its keys protected as
/// `protect` says: on a phone, the app's own folder; on a computer, the folder that the
/// person chose, where `nib` and the other apps then use it too, as after `nib init
/// --join`. A folder that holds only copies of replicas made elsewhere takes the new one
/// beside them, which takes in what they hold. With `replace`, as a phone, which keeps one
/// library, joins another once the person agreed, the library in the folder is set aside
/// and goes, with its keys in the keystore, once the new one is joined; it comes back if
/// joining fails. With `secrets`, in an app that reads them, the secrets too, protected so,
/// which the device that shows the code gives if the person chose to there; they are locked
/// until the person unlocks them. It is unlocked afterwards.
///
/// # Errors
///
/// Where the code or a name is wrong, the protection cannot be had, a library that is not
/// to be replaced is there, or joining fails.
pub fn join_library(
	folder: &str,
	(code, device): (&str, &str),
	protect: &Protect<'_>,
	secrets: Option<&Protect<'_>>,
	replace: bool,
) -> Result<Library, Failure> {
	let code: PairingCode = code.trim().parse().map_err(|_| {
		Failure::Request("a code is eight digits, as the other device shows it".to_owned())
	})?;
	let device = device.trim();
	if device.is_empty() {
		return Err(Failure::Request("a device needs a name".to_owned()));
	}
	let protection = protect.new_protection()?;
	let secrets = match secrets {
		Some(secrets) if crate::secrets().is_some() => Some(secrets.new_secrets_protection()?),
		_ => None,
	};
	finish_replacing(folder);
	let directory = data(folder);
	let replacing = replace && Library::exists(&directory);
	// Before the network starts: a library here is opened, not joined, unless it is to be
	// replaced, and one that cannot be opened says why.
	match Library::replica_folder(&directory) {
		_ if replacing => {}
		Err(NodeError::NoLibrary | NodeError::Copied) => {}
		Ok(_) => {
			return Err(match Library::protection(&directory) {
				Err(error @ (NodeError::Unfinished | NodeError::UncleanRemoval)) => error,
				_ => NodeError::Exists,
			}
			.into());
		}
		Err(error) => return Err(error.into()),
	}
	// Before the folder is made, as a program that syncs it would take it to the cloud.
	Library::check_place(&directory)?;
	let made = to_make(folder);
	std::fs::create_dir_all(folder)
		.map_err(|error| Failure::Request(format!("the folder cannot be made: {error}")))?;
	let replaced = Path::new(folder).join(REPLACED);
	if replacing {
		// Nothing of the app's writes into the library as it moves aside.
		syncing::let_go_of(&directory);
		std::fs::rename(&directory, &replaced).map_err(|error| {
			Failure::Request(format!(
				"the library here cannot be set aside to join another: {error}"
			))
		})?;
	}
	let joined = join_into(&directory, (&code, device), &protection, secrets);
	if joined.is_err() {
		// What a failed join leaves of the library goes with it.
		remove_empty(&directory, &made);
		if replacing {
			restore_replaced(&directory, &replaced);
		}
	} else if replacing {
		// Should it not go now, the next look at the folder removes it.
		let _ = Library::discard(&replaced);
	}
	joined
}

/// Joins as [`join_library`] says, into a folder that is there.
fn join_into(
	directory: &Path,
	(code, device): (&PairingCode, &str),
	protection: &NewProtection<'_>,
	secrets: Option<NewProtection<'_>>,
) -> Result<Library, Failure> {
	let mut rng = UnwrapErr(SysRng);
	let mut joining = Joining::new(device, app().name, &mut rng)?;
	if let Some(secrets) = secrets {
		joining = joining.asking_for_secrets(secrets, &mut rng);
	}
	let network = joining.network(NetworkOptions {
		discovery: true,
		port: None,
		accept: false,
	})?;
	let now = clock();
	let joined = nibkeep_node::join(
		directory,
		joining,
		&network,
		code,
		protection,
		FIND_SPONSOR,
		&now,
		&mut rng,
	)?;
	// Taken with the first sync, the secrets are locked until the person unlocks them.
	let mut library = joined.library;
	library.lock_secrets();
	// What copies beside it hold, that only they held, as a backup's: the device is added
	// all the same if they do not read.
	let _ = library.take_in_copies(&now);
	Ok(library)
}

/// Brings back the library that joining another set aside, as joining failed: what that
/// left goes first, with any keys it put in the keystore.
fn restore_replaced(directory: &Path, replaced: &Path) {
	if directory.exists() && !Library::exists(directory) {
		let _ = Library::discard(directory);
	}
	let _ = std::fs::rename(replaced, directory);
}

/// Finishes replacing the library in a folder, which a crash or a failed removal cut short:
/// the library set aside goes once a new one is in its place, and comes back if none is.
fn finish_replacing(folder: &str) {
	let replaced = Path::new(folder).join(REPLACED);
	if !replaced.exists() {
		return;
	}
	let directory = data(folder);
	if Library::exists(&directory) {
		let _ = Library::discard(&replaced);
	} else {
		restore_replaced(&directory, &replaced);
	}
}

/// Makes this device a device of its own from the copy of a library in the request's
/// `folder`, as [`library_from_copy`] does: the copy opened by `copy_passphrase` or with
/// `copy_keystore`, the keystore, and its keys here protected by `passphrase` or with
/// `keystore`. In an app that reads secrets, with `secrets`, the secrets too, opened by its
/// `copy_passphrase` or `copy_keystore` and protected by its `passphrase` or `keystore`.
/// Holds the library, unlocked.
///
/// # Errors
///
/// As [`library_from_copy`].
pub fn from_copy(request: &mut Json) -> Result<Json, Failure> {
	let folder = take(request, "folder");
	let copy_passphrase = take(request, "copy_passphrase");
	let copy = Protect {
		passphrase: &copy_passphrase,
		keystore: request["copy_keystore"].as_bool().unwrap_or(false),
	};
	let passphrase = take(request, "passphrase");
	let protect = Protect::of(request, &passphrase);
	// Taken out of the request, so wiped here.
	let mut secrets = request.get_mut("secrets").map(Json::take);
	let mut asked = secrets
		.as_mut()
		.filter(|secrets| secrets.is_object() && crate::secrets().is_some());
	let secrets_copy = asked
		.as_mut()
		.map_or_default(|secrets| take(secrets, "copy_passphrase"));
	let secrets_new = asked
		.as_mut()
		.map_or_default(|secrets| take(secrets, "passphrase"));
	let given = asked.map(|secrets| {
		(
			Protect {
				passphrase: &secrets_copy,
				keystore: secrets["copy_keystore"].as_bool().unwrap_or(false),
			},
			Protect::of(secrets, &secrets_new),
		)
	});
	let made = library_from_copy(
		&folder,
		&take(request, "device"),
		(&copy, &protect),
		given.as_ref().map(|(copy, protect)| (copy, protect)),
	);
	if let Some(secrets) = secrets.as_mut() {
		wipe(secrets);
	}
	keep(made?);
	Ok(json!({}))
}

/// Makes this device a device of its own from the copy of a library made elsewhere in a
/// folder, with the copy's own keys, which `copy` opens, where no other device can add it
/// (technical design: copies of a replica): the copy's device is revoked. With `secrets`,
/// which open the copy's secrets and protect them here, in an app that reads them, the
/// secrets too, locked until the person unlocks them.
///
/// # Errors
///
/// Where the name is empty, the folder holds no copy of one library, the protections
/// cannot be had, or the engine fails.
pub fn library_from_copy(
	folder: &str,
	device: &str,
	(copy, protect): (&Protect<'_>, &Protect<'_>),
	secrets: Option<(&Protect<'_>, &Protect<'_>)>,
) -> Result<Library, Failure> {
	let device = device.trim();
	if device.is_empty() {
		return Err(Failure::Request("a device needs a name".to_owned()));
	}
	let directory = data(folder);
	let copies = Library::copies_in(&directory);
	let [only] = copies.as_slice() else {
		return Err(Failure::Request(
			"the folder holds no copy of one library to make this device from".to_owned(),
		));
	};
	let unlocker = copy.unlocker()?;
	let protection = protect.new_protection()?;
	let secrets = match secrets {
		Some((copy, protect)) if crate::secrets().is_some() => {
			Some((copy.unlocker()?, protect.new_secrets_protection()?))
		}
		_ => None,
	};
	let mut rng = UnwrapErr(SysRng);
	let joining = Joining::new(device, app().name, &mut rng)?;
	let now = clock();
	let mut library = Library::add_from_copy(
		&directory,
		(&only.folder, &unlocker),
		joining,
		&protection,
		secrets
			.as_ref()
			.map(|(unlocker, protection)| (unlocker, protection)),
		&now,
		&mut rng,
	)?;
	library.lock_secrets();
	Ok(library)
}

/// A replica's ID, as the app has it from a list of devices.
///
/// # Errors
///
/// A [`Failure::Request`] if it is none.
pub fn replica_id(text: &str) -> Result<EntityId, Failure> {
	text.parse()
		.map_err(|_| Failure::Request("that is no device's ID".to_owned()))
}

/// Protects the library's keys anew, as `protect` says.
///
/// # Errors
///
/// Where the protection cannot be had, or the engine fails.
pub fn protect(library: &mut Library, protect: &Protect<'_>) -> Result<Json, Failure> {
	let protection = protect.new_protection()?;
	library.protect(&protection, &mut UnwrapErr(SysRng))?;
	Ok(json!({}))
}
