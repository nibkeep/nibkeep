//! Recovery codes, as `nib recovery` works with them
//! ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)): the
//! person's codes, which the app shows among the devices; making one, which counts once the
//! person types it back; checking and revoking one; and bringing the library back with one
//! on a computer, from a backup or in place of a library here that no longer unlocks.
//!
//! A recovery runs on a thread of its own, which holds it between its two halves: the first
//! reads the source and says which other devices the person had and whether the code holds
//! the secrets; the second, once the person answered, keeps the devices they still have,
//! takes the secrets, in an app that reads them, and revokes the rest. A recovery that the
//! person stops, or that fails, leaves nothing of the new device.
//!
//! An app that reads no secrets never adds them to a code, counts their keys, or takes
//! them from a code, whatever a request says.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::JoinHandle;

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::EntityId;
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{
	Joining, Library, Protection, RecoveredFrom, RecoveredSecrets, RecoveryCodeInfo, Replica,
	Unlocker,
};
use serde_json::{Value as Json, json};
use zeroize::Zeroizing;

use crate::held::{Mark, keep};
use crate::keystore::keystore;
use crate::setup::{Protect, data, replica_id};
use crate::{Failure, app, clock, locked, take, wall_millis};

/// The code shown last, until the person types it back or gives up.
static SHOWN: Mutex<Option<RecoveryCode>> = Mutex::new(None);

/// The requests that [`answer`] answers.
pub const CLOSED: [&str; 4] = [
	"start_recovery",
	"finish_recovery",
	"stop_recovery",
	"forget_recovery_code",
];

/// Answers the requests that need no library unlocked: starting, finishing and stopping a
/// recovery, and forgetting the code shown last. A library brought back is held, unlocked,
/// unless the library was locked as it came back.
///
/// # Errors
///
/// As [`start`] and [`finish`], and [`Failure::Locked`] if the library was locked as it
/// came back: it stays, locked.
pub fn answer(op: &str, request: &mut Json) -> Result<Json, Failure> {
	match op {
		"start_recovery" => start(request),
		"finish_recovery" => {
			let since = Mark::now();
			let (library, done) = finish(request)?;
			keep(library, since)?;
			Ok(done)
		}
		"stop_recovery" => {
			stop();
			Ok(json!({}))
		}
		_ => {
			forget_shown();
			Ok(json!({}))
		}
	}
}

/// A recovery code that the library names, as the app shows it: its name, when it was made,
/// in milliseconds since 1970, and on which device; whether it was revoked, whether it opens
/// the secrets, the backups that carry it and those still to update, and the newer code that
/// replaces it once every backup carries that one.
#[must_use]
pub fn code_json(code: &RecoveryCodeInfo) -> Json {
	json!({
		"record": code.record.to_string(),
		"name": code.name,
		"made": code.made_at,
		"made_on": code.made_on,
		"revoked": code.revoked,
		"holds_secrets": code.holds_secrets,
		"carried_by": code.carried_by,
		"to_update": code.to_update,
		"replaced_by": code.replaced_by,
	})
}

/// The person's recovery codes, the oldest first, revoked ones too; and their backups, each
/// with when this device last updated it, which making a code names first.
///
/// # Errors
///
/// Where the library's records do not read.
pub fn codes(library: &Library) -> Result<Json, Failure> {
	let codes: Vec<Json> = library.recovery_codes()?.iter().map(code_json).collect();
	let updated = library.sync_records(UnwrapErr(SysRng))?.backup_folders()?;
	let backups: Vec<Json> = library
		.backups()?
		.iter()
		.map(|backup| {
			json!({
				"name": backup.name,
				"updated": updated.get(&backup.id).and_then(|found| found.updated),
			})
		})
		.collect();
	Ok(json!({"codes": codes, "backups": backups}))
}

/// A new recovery code, for the person to write down: its text, in groups, and its name.
/// Nothing names it until the person types it back ([`confirm`]); until then the app holds
/// it here alone, and forgets it as it locks.
#[must_use]
pub fn new_code() -> Json {
	let code = RecoveryCode::generate(&mut UnwrapErr(SysRng));
	let text = code.to_text();
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	*locked(&SHOWN) = Some(code);
	json!({"code": text.as_str(), "name": name})
}

/// Forgets the code shown last, as the person gave up, or the library was locked.
pub fn forget_shown() {
	*locked(&SHOWN) = None;
}

/// The recovery code that the request's `code` holds, as the person typed it.
fn typed(request: &mut Json) -> Result<RecoveryCode, Failure> {
	let text = take(request, "code");
	RecoveryCode::from_text(&text).map_err(|error| Failure::Request(error.to_string()))
}

/// Has the library name the code that the person typed back, if nothing names it yet: with
/// `shown`, the code shown last, which the typed one must be; otherwise any, as one written
/// down earlier. With `secrets`, in an app that reads them, the code receives the secrets
/// too, which are unlocked first, also a code that the library names already, on the device
/// that made it. Then the
/// codes that it replaces are revoked, once every backup carries it. Says what the code is
/// now, whether it stood already, which codes were revoked now, and those whose paper to
/// keep until every backup carries the new one.
///
/// # Errors
///
/// Where the code is none, is not the one shown, the secrets cannot be unlocked, or the engine fails.
pub fn confirm(library: &mut Library, request: &mut Json) -> Result<Json, Failure> {
	let code = typed(request)?;
	let reads = crate::secrets();
	let secrets = reads.is_some() && request["secrets"].as_bool().unwrap_or(false);
	if request["shown"].as_bool().unwrap_or(false)
		&& !locked(&SHOWN)
			.as_ref()
			.is_some_and(|shown| shown.same_as(&code))
	{
		return Err(Failure::Request(
			"that is another code than the one shown; look at it again".to_owned(),
		));
	}
	if let Some(reads) = reads.filter(|_| secrets) {
		(reads.require)(library)?;
	}
	let mut rng = UnwrapErr(SysRng);
	let standing = library
		.check_recovery_code(&code)?
		.code
		.filter(|named| !named.revoked);
	let already = standing.is_some();
	let named = match standing {
		Some(named) if !secrets || named.holds_secrets => named,
		_ => library.add_recovery_code(&code, secrets, wall_millis(), &mut rng)?,
	};
	forget_shown();
	let replaced = library.revoke_replaced_recovery_codes(false, wall_millis(), &mut rng)?;
	let kept: Vec<String> = library
		.recovery_codes()?
		.into_iter()
		.filter(|old| !old.revoked && old.replaced_by.as_deref() == Some(named.name.as_str()))
		.map(|old| old.name)
		.collect();
	let replaced: Vec<String> = replaced.into_iter().map(|old| old.name).collect();
	Ok(json!({
		"code": code_json(&named),
		"already": already,
		"replaced": replaced,
		"kept": kept,
	}))
}

/// Checks a code that the person typed: whether the library names it, whether it stands,
/// how many of the library's keys it opens and lacks, and, in an app that reads secrets,
/// while they are unlocked here, how many of their keys.
///
/// # Errors
///
/// Where the code is none, or the engine fails.
pub fn check(library: &mut Library, request: &mut Json) -> Result<Json, Failure> {
	let code = typed(request)?;
	// The secrets' keys count where the app holds them unlocked.
	let reads = crate::secrets().is_some();
	if reads {
		let _ = library.unlock_kept_secrets();
	}
	let check = library.check_recovery_code(&code)?;
	let opens: usize = check.spaces.iter().map(|space| space.opens.len()).sum();
	let lacks: usize = check.spaces.iter().map(|space| space.lacks.len()).sum();
	let secrets = check
		.spaces
		.iter()
		.filter(|_| reads)
		.filter_map(|space| space.secrets.as_ref())
		.fold(None, |sum: Option<(usize, usize)>, (opens, lacks)| {
			let (opened, lacked) = sum.unwrap_or_default();
			Some((opened + opens.len(), lacked + lacks.len()))
		});
	Ok(json!({
		"code": check.code.as_ref().map(code_json),
		"opens": opens,
		"lacks": lacks,
		"secrets": secrets.map(|(opens, lacks)| json!({"opens": opens, "lacks": lacks})),
	}))
}

/// Revokes the recovery code whose record `id` names, for good, and gives every space it
/// had a new key.
///
/// # Errors
///
/// Where the library names no such code, or the engine fails.
pub fn revoke(library: &mut Library, id: &str) -> Result<Json, Failure> {
	let code = library.revoke_recovery_code(id, wall_millis(), &mut UnwrapErr(SysRng))?;
	Ok(json!({"code": code_json(&code)}))
}

/// The names of the person's codes that stand and that `replica` made.
#[must_use]
pub fn made_by(codes: &[RecoveryCodeInfo], replica: EntityId) -> Vec<String> {
	codes
		.iter()
		.filter(|code| !code.revoked && code.made_by == Some(replica))
		.map(|code| code.name.clone())
		.collect()
}

/// What the person answers a recovery under way: the replicas of the devices they still
/// have, and how the secrets are protected here, if the code holds them and the app reads
/// them, by a passphrase or, with `true`, the keystore.
struct Answer {
	keep: Vec<EntityId>,
	secrets: Option<(Zeroizing<String>, bool)>,
}

/// What the thread of a recovery says.
enum Said {
	/// It read the source: the person's other devices, and whether the code holds the
	/// secrets.
	Asking(Json),
	/// It brought the library back, and what it did.
	Done(Box<Library>, Json),
	/// The devices to keep were not the person's other devices: it asks again.
	Again(Failure),
	/// It failed, and the new device is gone.
	Failed(Failure),
}

/// A recovery under way, on its thread.
struct Running {
	answers: Option<mpsc::Sender<Answer>>,
	said: mpsc::Receiver<Said>,
	thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
	fn drop(&mut self) {
		// Without anyone to answer, the thread ends, and the new device goes with it.
		drop(self.answers.take());
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

/// The recovery under way, if there is one.
static RECOVERY: Mutex<Option<Running>> = Mutex::new(None);

/// Starts bringing the library back on this computer with the recovery code that the request
/// holds, into the folder `folder`, from the folder `source`: a backup, or the folder of a
/// library on this computer that no longer unlocks, which may be `folder` itself, as when its
/// passphrase is forgotten. This device is named `device`, and its keys are protected by the
/// request's passphrase, or with `keystore`, by the keystore. Returns once the source is read:
/// with the person's other devices, which the recovery revokes but those kept, and whether
/// the code holds the secrets; [`finish`] does the rest.
///
/// # Errors
///
/// Where the request lacks a name or a folder, there is nothing to bring back, or the source does not read.
pub fn start(request: &mut Json) -> Result<Json, Failure> {
	stop();
	let code = typed(request)?;
	let folder = take(request, "folder");
	let source = take(request, "source");
	let device = take(request, "device").trim().to_owned();
	if device.is_empty() {
		return Err(Failure::Request("a device needs a name".to_owned()));
	}
	if folder.trim().is_empty() || source.trim().is_empty() {
		return Err(Failure::Request(
			"say where to bring the library back from, and where to".to_owned(),
		));
	}
	let passphrase = take(request, "passphrase");
	let in_keystore = request["keystore"].as_bool().unwrap_or(false);
	let directory = data(&folder);
	// In place, only of a library: never into a backup's folder, say.
	if folder.trim() == source.trim() && !Library::exists(&directory) {
		return Err(Failure::Request(format!(
			"there is no library in {} to bring back in its place",
			source.trim()
		)));
	}
	let source = source_directory(&source);
	still_unlocks(&directory, &source)?;
	let (answers, asked) = mpsc::channel();
	let (says, said) = mpsc::channel();
	let thread = std::thread::spawn(move || {
		let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			recover(
				&directory,
				(&source, &code),
				(&device, (passphrase, in_keystore)),
				&asked,
				&says,
			);
		}));
		if ran.is_err() {
			let _ = says.send(Said::Failed(Failure::Request(
				"bringing the library back failed".to_owned(),
			)));
		}
	});
	let running = Running {
		answers: Some(answers),
		said,
		thread: Some(thread),
	};
	match running.said.recv() {
		Ok(Said::Asking(asking)) => {
			*locked(&RECOVERY) = Some(running);
			Ok(asking)
		}
		Ok(Said::Failed(failure) | Said::Again(failure)) => Err(failure),
		Ok(Said::Done(..)) | Err(_) => Err(Failure::Request(
			"bringing the library back failed".to_owned(),
		)),
	}
}

/// Finishes the recovery under way: keeps the replicas that the request's `keep` names, of
/// the person's other devices, and protects the secrets, if the code holds them and the app
/// reads them, as its `secrets` says, with a passphrase of their own or the keystore. Says
/// what came back, from where, and which devices were revoked, with the library, unlocked.
///
/// # Errors
///
/// Where no recovery is under way, the devices to keep are not the person's, or bringing the library back fails.
pub fn finish(request: &mut Json) -> Result<(Library, Json), Failure> {
	let keep = request["keep"]
		.as_array()
		.map(|ids| {
			ids.iter()
				.map(|id| replica_id(id.as_str().unwrap_or_default()))
				.collect::<Result<Vec<_>, _>>()
		})
		.transpose()?
		.unwrap_or_default();
	let secrets = match request.get_mut("secrets").map(Json::take) {
		Some(mut secrets) if secrets.is_object() && crate::secrets().is_some() => {
			let passphrase = take(&mut secrets, "passphrase");
			let keystore = secrets["keystore"].as_bool().unwrap_or(false);
			crate::wipe(&mut secrets);
			Some((passphrase, keystore))
		}
		Some(mut secrets) => {
			crate::wipe(&mut secrets);
			None
		}
		None => None,
	};
	let mut held = locked(&RECOVERY);
	let Some(running) = held.as_mut() else {
		return Err(Failure::Request(
			"no library is being brought back; start again".to_owned(),
		));
	};
	let sent = running
		.answers
		.as_ref()
		.is_some_and(|answers| answers.send(Answer { keep, secrets }).is_ok());
	if !sent {
		*held = None;
		return Err(Failure::Request(
			"bringing the library back failed; start again".to_owned(),
		));
	}
	match running.said.recv() {
		Ok(Said::Again(failure)) => Err(failure),
		Ok(Said::Done(library, done)) => {
			*held = None;
			Ok((*library, done))
		}
		Ok(Said::Failed(failure)) => {
			*held = None;
			Err(failure)
		}
		Ok(Said::Asking(_)) | Err(_) => {
			*held = None;
			Err(Failure::Request(
				"bringing the library back failed; start again".to_owned(),
			))
		}
	}
}

/// Stops the recovery under way, if there is one: nothing of the new device is left.
pub fn stop() {
	let running = locked(&RECOVERY).take();
	drop(running);
}

/// The directory of a library or backup in the folder that the person named: its `.nib`
/// folder, if it holds one, as a library's folder does, or else the folder itself, as a
/// backup's.
fn source_directory(folder: &str) -> PathBuf {
	let library = data(folder);
	if library.is_dir() {
		library
	} else {
		PathBuf::from(folder)
	}
}

/// Refuses to bring back in its place a library here that still unlocks with the keystore,
/// which a recovery would only revoke.
fn still_unlocks(directory: &Path, source: &Path) -> Result<(), Failure> {
	if Library::exists(directory)
		&& Library::recovery_replaces(directory, source)
		&& matches!(
			Library::protection(directory),
			Ok(Protection::Keystore { .. })
		)
		&& keystore()
		&& Library::open(directory, &Unlocker::Keystore).is_ok()
	{
		return Err(Failure::Request(
			"the library here still unlocks with this computer's keystore: there is nothing to bring back".to_owned(),
		));
	}
	Ok(())
}

/// Brings the library back, as [`start`] and [`finish`] say, on the recovery's thread: says
/// what it read, then waits for the person's answer.
fn recover(
	directory: &Path,
	(source, code): (&Path, &RecoveryCode),
	(device, (passphrase, in_keystore)): (&str, (Zeroizing<String>, bool)),
	asked: &mpsc::Receiver<Answer>,
	says: &mpsc::Sender<Said>,
) {
	let failed = |failure: Failure| {
		let _ = says.send(Said::Failed(failure));
	};
	let mut rng = UnwrapErr(SysRng);
	let joining = match Joining::new(device, app().name, &mut rng) {
		Ok(joining) => joining,
		Err(error) => return failed(error.into()),
	};
	let now = clock();
	// The library's passphrase is needed only to protect the new keys, and goes then, before
	// the recovery waits for the person.
	let started = Protect {
		passphrase: &passphrase,
		keystore: in_keystore,
	}
	.new_protection()
	.and_then(|protection| {
		Library::start_recovery(
			directory,
			(source, code),
			joining,
			&protection,
			&now,
			&mut rng,
		)
		.map_err(Failure::from)
	});
	drop(passphrase);
	let mut recovering = match started {
		Ok(recovering) => recovering,
		Err(failure) => return failed(failure),
	};
	let others = match recovering.others() {
		Ok(others) => others,
		Err(error) => return failed(error.into()),
	};
	let asking = json!({
		"others": devices_json(&others),
		"holds_secrets": recovering.holds_secrets(),
	});
	if says.send(Said::Asking(asking)).is_err() {
		return;
	}
	loop {
		let Ok(answer) = asked.recv() else {
			return;
		};
		if let Err(error) = recovering.keep(&answer.keep) {
			if says.send(Said::Again(error.into())).is_err() {
				return;
			}
			continue;
		}
		let secrets = match &answer.secrets {
			Some((passphrase, keystore)) if recovering.holds_secrets() => {
				let protect = Protect {
					passphrase,
					keystore: *keystore,
				};
				match protect.new_secrets_protection() {
					Ok(protection) => Some(protection),
					Err(failure) => {
						if says.send(Said::Again(failure)).is_err() {
							return;
						}
						continue;
					}
				}
			}
			_ => None,
		};
		let said = match recovering.finish(secrets.as_ref(), &now, &mut rng) {
			Ok((mut library, recovered)) => {
				library.lock_secrets();
				let done = recovered_json(&library, &recovered);
				Said::Done(Box::new(library), done)
			}
			Err(error) => Said::Failed(error.into()),
		};
		let _ = says.send(said);
		return;
	}
}

/// The person's other devices, each with the IDs of its replicas and the names of its apps;
/// an AI agent's replica on its own.
fn devices_json(others: &[Replica]) -> Json {
	let mut devices: Vec<(EntityId, Vec<&Replica>)> = Vec::new();
	for replica in others {
		let device = match replica.device {
			Some(device) if !replica.ai_agent => device,
			_ => replica.id,
		};
		match devices.iter_mut().find(|(known, _)| *known == device) {
			Some((_, replicas)) => replicas.push(replica),
			None => devices.push((device, vec![replica])),
		}
	}
	Json::Array(
		devices
			.into_iter()
			.map(|(_, replicas)| {
				let mut apps: Vec<&str> = replicas
					.iter()
					.filter_map(|replica| replica.name.as_deref())
					.collect();
				apps.sort_unstable();
				apps.dedup();
				json!({
					"replicas": replicas.iter().map(|replica| replica.id.to_string()).collect::<Vec<_>>(),
					"device": replicas.first().and_then(|replica| replica.device_name.as_deref()),
					"apps": apps,
					"ai_agent": replicas.iter().any(|replica| replica.ai_agent),
				})
			})
			.collect(),
	)
}

/// What a recovery did, for the app to say: the spaces that came back, by their names, and
/// how many did not; the devices revoked and kept, by their names; the spaces that the
/// devices revoked held and that did not come back, with the devices kept that hold each,
/// which revoke them there; what became of the secrets; and what it brought the library back
/// from.
fn recovered_json(library: &Library, recovered: &nibkeep_node::Recovered) -> Json {
	let mut spaces: Vec<String> = recovered
		.spaces
		.iter()
		.map(|&space| {
			library
				.space_name(space)
				.ok()
				.flatten()
				.unwrap_or_else(|| space.to_string())
		})
		.collect();
	spaces.sort();
	let source_replica = match &recovered.from {
		RecoveredFrom::Library { replica, .. } => *replica,
		RecoveredFrom::Backup { .. } => None,
	};
	let names = |replicas: &mut dyn Iterator<Item = &Replica>| -> Vec<String> {
		let mut names: Vec<String> = replicas
			.map(|replica| {
				replica
					.device_name
					.clone()
					.or_else(|| replica.name.clone())
					.unwrap_or_else(|| replica.id.to_string())
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
	let kept = names(&mut recovered.kept.iter());
	let secrets = match recovered.secrets {
		RecoveredSecrets::NoneHeld => "none_held",
		RecoveredSecrets::NotInCode => "not_in_code",
		RecoveredSecrets::Taken { .. } => "taken",
		RecoveredSecrets::Left => "left",
	};
	let from = match &recovered.from {
		RecoveredFrom::Backup { name, updated } => json!({
			"backup": name,
			"updated": updated,
		}),
		RecoveredFrom::Library {
			folder,
			replaced,
			set_aside,
			..
		} => json!({
			"library": folder.to_string_lossy(),
			"replaced": replaced,
			"set_aside": set_aside.as_ref().map(|aside| aside.to_string_lossy()),
		}),
	};
	json!({
		"spaces": spaces,
		"missing": recovered.missing.len(),
		"revoked": revoked,
		"kept": kept,
		"out_of_reach": crate::holding::out_of_reach_json(&recovered.out_of_reach),
		"secrets": secrets,
		"from": from,
	})
}
