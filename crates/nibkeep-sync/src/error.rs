//! Why pairing or syncing failed.

use std::fmt;

use crate::TransportError;

/// Why a side ended a connection, as `[0, reason]` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
	/// The pairing code did not match.
	WrongCode,
	/// The peer is no replica that this replica syncs with.
	Refused,
	/// The replica is syncing with another.
	Busy,
	/// Something went wrong on the side that ended.
	Failed,
	/// The person who invites declined the one who asked.
	Declined,
	/// The joiner's library holds the space that it was invited to already.
	HoldsSpace,
}

impl Reason {
	pub(crate) const fn number(self) -> u8 {
		match self {
			Self::WrongCode => 1,
			Self::Refused => 2,
			Self::Busy => 3,
			Self::Failed => 4,
			Self::Declined => 5,
			Self::HoldsSpace => 6,
		}
	}

	pub(crate) const fn of_number(number: u8) -> Option<Self> {
		match number {
			1 => Some(Self::WrongCode),
			2 => Some(Self::Refused),
			3 => Some(Self::Busy),
			4 => Some(Self::Failed),
			5 => Some(Self::Declined),
			6 => Some(Self::HoldsSpace),
			_ => None,
		}
	}
}

impl fmt::Display for Reason {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			Self::WrongCode => "the pairing code did not match",
			Self::Refused => {
				"it does not sync with this device, which it never paired with or revoked"
			}
			Self::Busy => "it is syncing with another replica",
			Self::Failed => "something went wrong on its side",
			Self::Declined => "the invitation was declined",
			Self::HoldsSpace => "the library that was invited holds the space already",
		})
	}
}

/// Why pairing or syncing failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
	/// The connection failed.
	#[error("the connection failed: {0}")]
	Connection(#[from] TransportError),
	/// The other side sent what the protocol does not allow then.
	#[error("the other device broke the protocol: {0}")]
	Protocol(String),
	/// The other side ended the connection.
	#[error("the other device ended the connection: {0}")]
	Ended(Reason),
	/// The pairing code did not match: this side found so, or, in pairing and invitations,
	/// the other side said so.
	#[error("the pairing code did not match")]
	WrongCode,
	/// The other side is no replica that this replica syncs with.
	#[error("the other device is no replica of this library")]
	NotAReplica,
	/// The person who invites declined the one who asked, on this side.
	#[error("the invitation was declined")]
	Declined,
	/// This side's library holds the space that it was invited to already.
	#[error("this library holds the space already")]
	HoldsSpace,
	/// Reading or writing this replica's data failed.
	#[error(transparent)]
	Holder(Box<dyn std::error::Error + Send + Sync>),
}

impl SyncError {
	pub(crate) fn protocol(what: impl Into<String>) -> Self {
		Self::Protocol(what.into())
	}

	pub(crate) fn holder(error: impl std::error::Error + Send + Sync + 'static) -> Self {
		Self::Holder(Box::new(error))
	}

	/// A code that did not match on the other side, as one that did not match.
	pub(crate) fn either_side(self) -> Self {
		match self {
			Self::Ended(Reason::WrongCode) => Self::WrongCode,
			error => error,
		}
	}

	/// Why this side ends the connection, to tell the other side: nothing if the other side
	/// ended it or the connection failed.
	pub(crate) const fn reason(&self) -> Option<Reason> {
		match self {
			Self::Ended(_) | Self::Connection(_) => None,
			Self::NotAReplica => Some(Reason::Refused),
			Self::WrongCode => Some(Reason::WrongCode),
			Self::Declined => Some(Reason::Declined),
			Self::HoldsSpace => Some(Reason::HoldsSpace),
			Self::Protocol(_) | Self::Holder(_) => Some(Reason::Failed),
		}
	}
}
