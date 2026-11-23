//! The engines file: the version of a replica's stored files, and the programs that open the
//! replica, each with the latest version its engine reads, so that programs whose engines
//! are of different versions share one replica ([storage](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/storage.md#16-engines-of-different-versions)).
//!
//! A program reads the file before any other of the replica but the place files, and opens
//! nothing if the replica is of a later version than its engine reads. It records itself
//! as it opens the replica, so that a later engine knows which programs still read only
//! the versions before its own.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};

use crate::info::write_file;
use crate::{DAY, NodeError};

/// The engines file, in a replica's folder.
pub(crate) const ENGINES: &str = "engines.cbor";

/// The file that a program locks while it records itself in the engines file.
const ENGINES_LOCK: &str = "engines.lock";

/// The version of a replica's stored files that this engine reads and writes: those of
/// version 1 of the storage specification.
pub const REPLICA_VERSION: u64 = 1;

/// The engine's version, which the engines file records of each program, and a copy of
/// `nibkeep-sync` tells, so that the newest runs.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The name that this program gave itself.
static PROGRAM: OnceLock<String> = OnceLock::new();

/// Names this program, as the engines file of each replica that it opens records it, such
/// as `nib`: once, before it opens a library. A later name, or an empty one, is
/// ignored. A program that names itself nothing is named by its executable's file name.
pub fn name_program(name: &str) {
	// An app may name itself on every request.
	if !name.is_empty() && PROGRAM.get().is_none() {
		let _ = PROGRAM.set(name.to_owned());
	}
}

/// This program's name.
fn program() -> String {
	PROGRAM.get().cloned().unwrap_or_else(|| {
		std::env::current_exe()
			.ok()
			.and_then(|executable| {
				executable
					.file_stem()
					.map(|stem| stem.to_string_lossy().into_owned())
			})
			.unwrap_or_else(|| "unnamed".to_owned())
	})
}

/// Checks that this engine reads the replica in `folder`: that its engines file, if it has
/// one, holds a version no later than [`REPLICA_VERSION`]. Writes nothing.
///
/// # Errors
///
/// Returns [`NodeError::NewerFormats`] if the replica is of a later version, and
/// [`NodeError::Damaged`] if its engines file does not read.
pub(crate) fn check(folder: &Path) -> Result<(), NodeError> {
	read(folder).map(drop)
}

/// Writes the engines file of a new replica into its folder, before the folder goes in
/// place: of this engine's version, recording this program as having opened it at `now`.
pub(crate) fn create(folder: &Path, now: u64) -> Result<(), NodeError> {
	let items = vec![
		REPLICA_VERSION.to_cbor(),
		Value::Array(vec![entry(&program(), now)]),
	];
	write_file(&folder.join(ENGINES), &cbor::encode(&Value::Array(items)))
}

/// Records this program in the engines file of the replica in `folder`, as having opened it
/// at `now`: replaces its entry, or adds one, and keeps the other entries, and what the file
/// holds after the items it knows, as they are. Writes the file only if its entry changes,
/// or was written on another day, once a day at most, so that opening mostly writes nothing.
///
/// # Errors
///
/// As [`check`], and returns an error if the lock or the file cannot be written.
pub(crate) fn record(folder: &Path, now: u64) -> Result<(), NodeError> {
	record_as(folder, &program(), now)
}

/// Records `name` in the engines file, as [`record`] does.
fn record_as(folder: &Path, name: &str, now: u64) -> Result<(), NodeError> {
	let own = entry(name, now);
	// Most openings find their entry as it is to be, and take no lock.
	if let Some(items) = read(folder)?
		&& entry_of(&items, name).is_some_and(|found| same_day(found, &own))
	{
		return Ok(());
	}
	let lock_path = folder.join(ENGINES_LOCK);
	let lock = fs::OpenOptions::new()
		.create(true)
		.truncate(false)
		.write(true)
		.open(&lock_path)
		.map_err(NodeError::file(&lock_path))?;
	// Held until the file is written, so that two programs never record over each other;
	// the file is read again under it, as another may have written it meanwhile.
	lock.lock().map_err(NodeError::file(&lock_path))?;
	let mut items =
		read(folder)?.unwrap_or_else(|| vec![REPLICA_VERSION.to_cbor(), Value::Array(Vec::new())]);
	let Some(Value::Array(programs)) = items.get_mut(1) else {
		return Err(damaged());
	};
	match programs
		.iter_mut()
		.find(|program| name_of(program) == Some(name))
	{
		Some(found) if same_day(found, &own) => return Ok(()),
		Some(found) => *found = own,
		None => programs.push(own),
	}
	write_file(&folder.join(ENGINES), &cbor::encode(&Value::Array(items)))
}

/// Reads the engines file of the replica in `folder`: its items, the version first, then
/// the programs, then any that a later version adds; or `None` if it has none.
fn read(folder: &Path) -> Result<Option<Vec<Value>>, NodeError> {
	let path = folder.join(ENGINES);
	let bytes = match fs::read(&path) {
		Ok(bytes) => bytes,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(NodeError::file(&path)(error)),
	};
	let Ok(Value::Array(items)) = cbor::decode(&bytes) else {
		return Err(damaged());
	};
	// The version first: of a later one, this engine needs to know nothing more.
	let version = items
		.first()
		.and_then(|version| u64::from_cbor(version).ok())
		.filter(|&version| version > 0)
		.ok_or_else(damaged)?;
	if version > REPLICA_VERSION {
		return Err(NodeError::NewerFormats);
	}
	match items.get(1) {
		Some(Value::Array(programs)) if programs.iter().all(is_entry) => Ok(Some(items)),
		_ => Err(damaged()),
	}
}

/// The error of an engines file that does not read.
fn damaged() -> NodeError {
	NodeError::Damaged("the replica's engines file does not read".to_owned())
}

/// A program's entry, `[name, reads, engine, opened]`: of this engine, opened at `now`.
fn entry(name: &str, now: u64) -> Value {
	Value::Array(vec![
		name.to_cbor(),
		REPLICA_VERSION.to_cbor(),
		ENGINE_VERSION.to_cbor(),
		now.to_cbor(),
	])
}

/// Whether a value is a program's entry: `[name, reads, engine, opened]`, with perhaps items
/// after them that a later version adds.
fn is_entry(entry: &Value) -> bool {
	let Value::Array(items) = entry else {
		return false;
	};
	matches!(
		items.as_slice(),
		[Value::Text(_), reads, Value::Text(_), opened, ..]
			if u64::from_cbor(reads).is_ok() && u64::from_cbor(opened).is_ok()
	)
}

/// The entry of the program `name` in the engines file's items, if it has one.
fn entry_of<'a>(items: &'a [Value], name: &str) -> Option<&'a Value> {
	match items.get(1) {
		Some(Value::Array(programs)) => programs
			.iter()
			.find(|program| name_of(program) == Some(name)),
		_ => None,
	}
}

/// The name of a program's entry.
fn name_of(entry: &Value) -> Option<&str> {
	match entry {
		Value::Array(items) => match items.first() {
			Some(Value::Text(name)) => Some(name),
			_ => None,
		},
		_ => None,
	}
}

/// Whether an entry says what `own` says, with a time of the same day (UTC), and nothing
/// more.
fn same_day(entry: &Value, own: &Value) -> bool {
	let (Value::Array(entry), Value::Array(own)) = (entry, own) else {
		return false;
	};
	let day = |items: &[Value]| {
		items
			.get(3)
			.and_then(|opened| u64::from_cbor(opened).ok())
			.map(|opened| opened / DAY)
	};
	entry.len() == own.len() && entry[..3] == own[..3] && day(entry) == day(own)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn folder(name: &str) -> std::path::PathBuf {
		nibkeep_testkit::empty_directory(std::env::temp_dir(), &format!("engines {name}"))
	}

	fn names(folder: &Path) -> Vec<String> {
		let items = read(folder).expect("reads").expect("a file");
		let Value::Array(programs) = &items[1] else {
			panic!("the programs");
		};
		programs
			.iter()
			.map(|program| name_of(program).expect("a name").to_owned())
			.collect()
	}

	#[test]
	fn each_program_records_an_entry_of_its_own() {
		let folder = folder("two programs");
		record_as(&folder, "nib", 1_000).expect("records");
		record_as(&folder, "Passwords", 2_000).expect("records");
		record_as(&folder, "nib", 3_000).expect("records again");
		assert_eq!(names(&folder), ["nib", "Passwords"]);
		check(&folder).expect("reads");
	}

	#[test]
	fn a_record_of_the_same_day_writes_nothing_and_of_another_day_does() {
		let folder = folder("days");
		record_as(&folder, "nib", DAY + 1).expect("records");
		let first = fs::read(folder.join(ENGINES)).expect("reads");
		record_as(&folder, "nib", 2 * DAY - 1).expect("records");
		assert_eq!(fs::read(folder.join(ENGINES)).expect("reads"), first);
		record_as(&folder, "nib", 2 * DAY).expect("records");
		assert_ne!(fs::read(folder.join(ENGINES)).expect("reads"), first);
	}

	#[test]
	fn a_program_named_nothing_takes_its_executables_name() {
		name_program("");
		let executable = std::env::current_exe().expect("an executable");
		assert_eq!(
			Some(program().as_str()),
			executable.file_stem().and_then(|stem| stem.to_str())
		);
	}
}
