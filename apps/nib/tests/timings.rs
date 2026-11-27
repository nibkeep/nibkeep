//! `nib --timings`: how long each part of a command took.

use std::time::{Duration, Instant};

use crate::common::Test;
use clap::Parser;
use nibkeep_cli::{Cli, Timings};

/// Runs `nib` with these arguments and timings, and returns them.
fn timed(test: &mut Test, arguments: &[&str]) -> Timings {
	let cli = Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
		.expect("valid arguments");
	let mut output = Vec::new();
	nibkeep_cli::run_timed(&cli, test, &mut output).expect("runs")
}

#[test]
fn each_part_is_timed_and_waiting_for_a_passphrase_is_apart() {
	let typing = Duration::from_millis(100);
	let mut test = Test::new("timings");
	test.typing = typing;
	test.answer(&["correct horse"; 3]);
	let started = timed(&mut test, &["init", "--name", "Ada", "--passphrase"]);
	// Starting a library opens none: making its keys is its work.
	assert!(started.waiting >= Some(typing * 2), "{started:?}");
	assert_eq!((started.unlock, started.open), (None, None));

	test.nib(&["type", "add", "book", "title:text"])
		.expect("adds the type");
	test.answer(&["correct horse"]);
	let before = Instant::now();
	let shown = timed(&mut test, &["--timings", "list"]);
	let elapsed = before.elapsed();
	let waiting = shown.waiting.expect("waited for the passphrase");
	let unlock = shown.unlock.expect("unlocked");
	let open = shown.open.expect("opened");
	assert!(waiting >= typing, "{shown:?}");
	// Deriving the key takes time even with the tests' small parameters, and none of the
	// typing is counted as unlocking or work.
	assert!(unlock > Duration::ZERO && unlock < typing, "{shown:?}");
	assert!(open > Duration::ZERO, "{shown:?}");
	assert!(shown.work < typing, "{shown:?}");
	assert!(waiting + unlock + open + shown.work + shown.print <= elapsed);
}

#[test]
fn timings_read_for_people_and_for_scripts() {
	let timings = Timings {
		waiting: None,
		unlock: Some(Duration::from_millis(412)),
		open: Some(Duration::from_millis(1603)),
		work: Duration::from_millis(10),
		print: Duration::from_millis(1),
	};
	assert_eq!(
		timings.describe(false),
		"unlock 0.412 s, open 1.603 s, work 0.010 s, print 0.001 s"
	);
	let json: serde_json::Value = serde_json::from_str(&timings.describe(true)).expect("JSON");
	assert_eq!(
		json,
		serde_json::json!({
			"unlock_seconds": 0.412,
			"open_seconds": 1.603,
			"work_seconds": 0.01,
			"print_seconds": 0.001,
		})
	);
}
