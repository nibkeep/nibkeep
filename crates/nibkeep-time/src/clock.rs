//! Hybrid logical clocks, after Kulkarni, Demirbas and others (2014).

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};

/// A point in system time: milliseconds since 1970-01-01T00:00:00Z on a device's clock,
/// and a counter that orders events within the same millisecond.
///
/// Timestamps are ordered by milliseconds, then by counter. In CBOR, a timestamp is an
/// array of the two integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
	millis: u64,
	counter: u32,
}

impl Timestamp {
	/// The last millisecond a timestamp can have, 9999-12-31T23:59:59.999Z. A wall clock
	/// that reads later is broken.
	pub const MAX_MILLIS: u64 = 253_402_300_799_999;

	/// Returns the timestamp with these parts.
	#[must_use]
	pub const fn new(millis: u64, counter: u32) -> Self {
		Self { millis, counter }
	}

	/// The milliseconds since 1970-01-01T00:00:00Z.
	#[must_use]
	pub const fn millis(self) -> u64 {
		self.millis
	}

	/// The counter.
	#[must_use]
	pub const fn counter(self) -> u32 {
		self.counter
	}

	/// The next timestamp in the same millisecond, or the first one of the next
	/// millisecond when the counter is used up. Returns `None` after the last counter
	/// value of [`Timestamp::MAX_MILLIS`].
	fn next(self) -> Option<Self> {
		match self.counter.checked_add(1) {
			Some(counter) => Some(Self::new(self.millis, counter)),
			None if self.millis < Self::MAX_MILLIS => Some(Self::new(self.millis + 1, 0)),
			None => None,
		}
	}
}

impl ToCbor for Timestamp {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![self.millis.to_cbor(), self.counter.to_cbor()])
	}
}

impl FromCbor for Timestamp {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		match value {
			Value::Array(items) => match items.as_slice() {
				[millis, counter] => {
					Ok(Self::new(u64::from_cbor(millis)?, u32::from_cbor(counter)?))
				}
				_ => Err(cbor::Error::Expected("a timestamp of two integers")),
			},
			_ => Err(cbor::Error::Expected("a timestamp of two integers")),
		}
	}
}

/// A hybrid logical clock. Its timestamps stay close to the wall clock but never go
/// backwards, and every timestamp it hands out is later than every timestamp it has
/// seen, from this device or from others.
///
/// The clock does not read the wall clock itself: callers pass the current wall clock
/// time, in milliseconds since 1970-01-01T00:00:00Z, to every call. No timestamp is later
/// than [`Timestamp::MAX_MILLIS`].
#[derive(Clone, Debug)]
pub struct Clock {
	last: Timestamp,
	max_drift_millis: u64,
}

impl Clock {
	/// Returns a new clock. It rejects received timestamps that are more than
	/// `max_drift_millis` ahead of the wall clock.
	#[must_use]
	pub fn new(max_drift_millis: u64) -> Self {
		Self {
			last: Timestamp::new(0, 0),
			max_drift_millis,
		}
	}

	/// Returns a clock that continues after `last`, the latest timestamp a clock handed
	/// out or saw before a restart, as returned by [`Clock::last`].
	///
	/// # Errors
	///
	/// Returns an error if `last` is later than [`Timestamp::MAX_MILLIS`].
	pub fn resume(last: Timestamp, max_drift_millis: u64) -> Result<Self, ClockError> {
		if last.millis > Timestamp::MAX_MILLIS {
			return Err(ClockError::OutOfRange { timestamp: last });
		}
		Ok(Self {
			last,
			max_drift_millis,
		})
	}

	/// The latest timestamp the clock has handed out or seen.
	#[must_use]
	pub fn last(&self) -> Timestamp {
		self.last
	}

	/// Returns the timestamp of a new local event. A wall clock later than
	/// [`Timestamp::MAX_MILLIS`] counts as that time.
	///
	/// # Errors
	///
	/// Returns an error if the clock has handed out its last timestamp, the one with the
	/// last counter value of [`Timestamp::MAX_MILLIS`].
	pub fn now(&mut self, wall_millis: u64) -> Result<Timestamp, ClockError> {
		let wall_millis = wall_millis.min(Timestamp::MAX_MILLIS);
		self.last = if wall_millis > self.last.millis {
			Timestamp::new(wall_millis, 0)
		} else {
			self.last.next().ok_or(ClockError::Exhausted)?
		};
		Ok(self.last)
	}

	/// Checks a timestamp received from another device, without taking it into account:
	/// for data that is not accepted yet.
	///
	/// # Errors
	///
	/// Returns an error if `received` is further ahead of the wall clock than the clock
	/// allows, or later than [`Timestamp::MAX_MILLIS`].
	pub fn check(&self, received: Timestamp, wall_millis: u64) -> Result<(), ClockError> {
		if received.millis > Timestamp::MAX_MILLIS {
			return Err(ClockError::OutOfRange {
				timestamp: received,
			});
		}
		if received.millis > wall_millis.saturating_add(self.max_drift_millis) {
			return Err(ClockError::TooFarAhead {
				received,
				wall: wall_millis,
			});
		}
		Ok(())
	}

	/// Takes a timestamp received from another device into account, so that later local
	/// events come after it.
	///
	/// # Errors
	///
	/// Returns an error, and leaves the clock unchanged, if [`Clock::check`] fails.
	pub fn observe(&mut self, received: Timestamp, wall_millis: u64) -> Result<(), ClockError> {
		self.check(received, wall_millis)?;
		self.last = self.last.max(received);
		Ok(())
	}
}

/// Why a clock could not hand out or take in a timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClockError {
	/// The timestamp is further ahead of the wall clock than the clock allows.
	#[error("timestamp {received:?} is too far ahead of the wall clock at {wall} ms")]
	TooFarAhead {
		/// The received timestamp.
		received: Timestamp,
		/// The wall clock time, in milliseconds since 1970-01-01T00:00:00Z.
		wall: u64,
	},
	/// The timestamp is later than [`Timestamp::MAX_MILLIS`].
	#[error("timestamp {timestamp:?} is later than the year 9999")]
	OutOfRange {
		/// The timestamp.
		timestamp: Timestamp,
	},
	/// The clock has handed out its last timestamp.
	#[error("the clock has handed out its last timestamp, at the end of the year 9999")]
	Exhausted,
}
