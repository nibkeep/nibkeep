//! Hybrid logical clocks never go backwards.

use nibkeep_base::cbor;
use nibkeep_time::{Clock, ClockError, Timestamp};
use proptest::prelude::*;

const DRIFT: u64 = 60_000;

#[test]
fn timestamps_follow_the_wall_clock_when_it_moves_forward() {
	let mut clock = Clock::new(DRIFT);
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(1_000, 0)));
	assert_eq!(clock.now(2_000), Ok(Timestamp::new(2_000, 0)));
}

#[test]
fn timestamps_never_go_backwards_when_the_wall_clock_does() {
	let mut clock = Clock::new(DRIFT);
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(1_000, 0)));
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(1_000, 1)));
	assert_eq!(clock.now(500), Ok(Timestamp::new(1_000, 2)));
	assert_eq!(clock.now(1_001), Ok(Timestamp::new(1_001, 0)));
}

#[test]
fn a_received_timestamp_moves_the_clock_forward() {
	let mut clock = Clock::new(DRIFT);
	clock.now(1_000).expect("not exhausted");
	clock
		.observe(Timestamp::new(5_000, 7), 1_000)
		.expect("within the drift");
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(5_000, 8)));
	// An older received timestamp changes nothing.
	clock
		.observe(Timestamp::new(2_000, 0), 1_000)
		.expect("within the drift");
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(5_000, 9)));
}

#[test]
fn a_timestamp_too_far_ahead_is_rejected() {
	let mut clock = Clock::new(DRIFT);
	clock.now(1_000).expect("not exhausted");
	let far = Timestamp::new(1_000 + DRIFT + 1, 0);
	assert_eq!(
		clock.observe(far, 1_000),
		Err(ClockError::TooFarAhead {
			received: far,
			wall: 1_000
		})
	);
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(1_000, 1)));
	assert!(
		clock
			.observe(Timestamp::new(1_000 + DRIFT, 0), 1_000)
			.is_ok()
	);
}

#[test]
fn checking_a_timestamp_leaves_the_clock_unchanged() {
	let mut clock = Clock::new(DRIFT);
	clock.now(1_000).expect("not exhausted");
	assert_eq!(clock.check(Timestamp::new(5_000, 0), 1_000), Ok(()));
	assert_eq!(clock.last(), Timestamp::new(1_000, 0));
	assert!(
		clock
			.check(Timestamp::new(1_000 + DRIFT + 1, 0), 1_000)
			.is_err()
	);
}

#[test]
fn a_full_counter_carries_into_the_milliseconds() {
	let mut clock = Clock::resume(Timestamp::new(1_000, u32::MAX), DRIFT).expect("in range");
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(1_001, 0)));
}

#[test]
fn a_resumed_clock_continues_after_its_last_timestamp() {
	let mut clock = Clock::resume(Timestamp::new(9_000, 3), DRIFT).expect("in range");
	assert_eq!(clock.now(1_000), Ok(Timestamp::new(9_000, 4)));
}

#[test]
fn timestamps_are_encoded_as_two_integers() {
	let timestamp = Timestamp::new(1_760_000_000_000, 42);
	let encoded = cbor::to_vec(&timestamp);
	assert_eq!(encoded, cbor::to_vec(&vec![1_760_000_000_000u64, 42]));
	assert_eq!(cbor::from_slice::<Timestamp>(&encoded), Ok(timestamp));
}

#[test]
fn timestamps_after_the_year_9999_are_rejected() {
	let beyond = Timestamp::new(Timestamp::MAX_MILLIS + 1, 0);
	// Even a clock that allows any drift rejects them.
	let mut clock = Clock::new(u64::MAX);
	assert_eq!(
		clock.observe(beyond, 0),
		Err(ClockError::OutOfRange { timestamp: beyond })
	);
	assert_eq!(
		Clock::resume(beyond, DRIFT).err(),
		Some(ClockError::OutOfRange { timestamp: beyond })
	);
	assert_eq!(clock.last(), Timestamp::new(0, 0));
	// The last timestamp is accepted.
	let last = Timestamp::new(Timestamp::MAX_MILLIS, u32::MAX);
	clock.observe(last, 0).expect("in range");
	assert_eq!(clock.last(), last);
}

#[test]
fn a_clock_runs_out_of_timestamps_at_the_end_of_the_year_9999() {
	let last = Timestamp::new(Timestamp::MAX_MILLIS, u32::MAX);
	let mut clock = Clock::resume(last, DRIFT).expect("in range");
	assert_eq!(clock.now(u64::MAX), Err(ClockError::Exhausted));
	assert_eq!(clock.last(), last);
}

#[test]
fn a_wall_clock_after_the_year_9999_counts_as_the_last_millisecond() {
	let mut clock = Clock::new(DRIFT);
	assert_eq!(
		clock.now(u64::MAX),
		Ok(Timestamp::new(Timestamp::MAX_MILLIS, 0))
	);
	assert_eq!(
		clock.now(u64::MAX),
		Ok(Timestamp::new(Timestamp::MAX_MILLIS, 1))
	);
}

proptest! {
	#[test]
	fn every_timestamp_is_later_than_everything_seen_before(
		events in prop::collection::vec((any::<bool>(), 0u64..10_000, 0u64..10_000, 0u32..5), 1..60),
	) {
		let mut clock = Clock::new(DRIFT);
		let mut latest_seen = Timestamp::new(0, 0);
		for (is_local, wall, remote_millis, remote_counter) in events {
			if is_local {
				let timestamp = clock.now(wall).expect("not exhausted");
				prop_assert!(timestamp > latest_seen);
				latest_seen = timestamp;
			} else {
				let remote = Timestamp::new(remote_millis, remote_counter);
				if clock.observe(remote, wall).is_ok() && remote > latest_seen {
					latest_seen = remote;
				}
			}
		}
	}
}
