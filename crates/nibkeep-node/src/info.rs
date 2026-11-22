//! The library file: who a library belongs to.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};

use crate::NodeError;

/// The suite of the library file in version 1 of the specification.
const SUITE: u8 = 1;

/// Counts files written, so that writes use temporary files of their own.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// Who a library belongs to, and where it is. In CBOR, the array
/// `[1, user, device, replica, personal space]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Info {
	/// The user's entity.
	pub(crate) user: EntityId,
	/// The device's entity.
	pub(crate) device: EntityId,
	/// The entity of the replica that the library is.
	pub(crate) replica: EntityId,
	/// The user's personal space.
	pub(crate) personal: SpaceId,
}

impl Info {
	/// Reads the library file.
	pub(crate) fn read(path: &Path) -> Result<Self, NodeError> {
		let bytes = fs::read(path).map_err(NodeError::file(path))?;
		cbor::from_slice(&bytes).map_err(|error| NodeError::Damaged(error.to_string()))
	}

	/// Writes the library file.
	pub(crate) fn write(&self, path: &Path) -> Result<(), NodeError> {
		write_file(path, &cbor::to_vec(self))
	}
}

/// Writes a file as a whole: into a new file next to it, which is synced and renamed over
/// it, so that a crash leaves either the old file or the new one. Then syncs the
/// directory, where the file system allows it, so that the rename lasts.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), NodeError> {
	let name = path
		.file_name()
		.map_or_else(|| "file".into(), |name| name.to_string_lossy());
	let temporary = path.with_file_name(format!(
		".{name}.{}.{}.tmp",
		std::process::id(),
		WRITES.fetch_add(1, Ordering::Relaxed)
	));
	let written = write_new(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
	if let Err(error) = written {
		let _ = fs::remove_file(&temporary);
		return Err(NodeError::file(path)(error));
	}
	#[cfg(unix)]
	if let Some(directory) = path
		.parent()
		.filter(|directory| !directory.as_os_str().is_empty())
	{
		let _ = fs::File::open(directory).and_then(|directory| directory.sync_all());
	}
	Ok(())
}

/// Creates a new file with these bytes, and syncs it.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let mut file = fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(path)?;
	file.write_all(bytes)?;
	file.sync_all()
}

impl ToCbor for Info {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![
			SUITE.to_cbor(),
			self.user.to_cbor(),
			self.device.to_cbor(),
			self.replica.to_cbor(),
			self.personal.to_cbor(),
		])
	}
}

impl FromCbor for Info {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a library file: [1, user, device, replica, personal space]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [suite, user, device, replica, personal] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		if u8::from_cbor(suite) != Ok(SUITE) {
			return Err(cbor::Error::Expected(EXPECTED));
		}
		Ok(Self {
			user: EntityId::from_cbor(user)?,
			device: EntityId::from_cbor(device)?,
			replica: EntityId::from_cbor(replica)?,
			personal: SpaceId::from_cbor(personal)?,
		})
	}
}
