//! Why storing failed.

use nibkeep_log::LogError;
use rusqlite::ErrorCode;

/// Why a space's data could not be stored or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
	/// The file is not a log file of a NibKeep space.
	#[error("the file is not the log of a NibKeep space")]
	NotALog,
	/// The log file or the state file is of a later version than this program reads, which
	/// a newer program that shares the replica wrote: it was left as it was.
	#[error("a newer version of NibKeep wrote this file; update this program to read it")]
	Newer,
	/// The file holds data that does not check out: it was damaged or tampered with.
	#[error("the log file is damaged or was tampered with: {0}")]
	Damaged(String),
	/// The log rejected an operation.
	#[error(transparent)]
	Log(#[from] LogError),
	/// The database failed.
	#[error("the database failed: {0}")]
	Database(rusqlite::Error),
	/// The file system failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),
	/// The state file cannot be used: it is damaged, or of an earlier format. It can always
	/// be built again from the log.
	#[error("the state file cannot be used: {0}")]
	State(String),
	/// The state file was to be written outside a transaction, or a transaction was to
	/// begin within another.
	#[error("the state file is written only in a transaction, one at a time")]
	Transaction,
	/// The file system or SQLite lacks something the store needs.
	#[error("the file system does not support {0}")]
	Unsupported(&'static str),
	/// Another program, or another part of this one, has the space to itself for a moment,
	/// as while it makes the state file anew; or waits to, after a purge, until the others
	/// close it, and this one is to close it too, and open it again later.
	#[error("another program has the space to itself for a moment")]
	InUse,
	/// The replica holds no copy of the blob.
	#[error("the device holds no copy of the blob")]
	MissingBlob,
	/// The replica's copy of the blob is damaged, or its key is not the blob's.
	#[error("the copy of the blob is damaged, or its key is wrong")]
	DamagedBlob,
	/// Saving failed and the log file could not be read again; the stored log is unusable.
	#[error("the log could not be saved or read again, and is unusable until it is opened again")]
	Broken,
	/// What was asked waits until the batch of changes that is open ends, such as a purge,
	/// receiving a change, or another batch.
	#[error("not while a batch of changes is open")]
	InBatch,
	/// No batch of changes is open to end.
	#[error("no batch of changes is open")]
	NoBatch,
	/// The files failed while a batch of changes was open, and none of its changes was
	/// saved; the changes after the failure are refused until the batch ends.
	#[error("the batch of changes could not be saved, and none of its changes was")]
	BatchLost,
}

impl StoreError {
	/// Whether it is SQLite's error that another connection held a lock longer than it
	/// waits.
	#[must_use]
	pub fn busy(&self) -> bool {
		matches!(self, Self::Database(error)
			if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy))
	}
}
