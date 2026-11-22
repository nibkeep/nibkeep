//! Power cuts: an encrypted database on LazyFS, which keeps the data written to files in
//! memory until it is synced, and on command drops what is not, as a power cut does.
//!
//! Two child processes write transactions to the database at once, and after each that
//! commits, note it on the real disk. The test kills both at a random moment and drops what
//! LazyFS holds unsynced. The database must then open, hold every transaction noted of
//! both, and be found sound.
//!
//! The test needs LazyFS, built, and its program in `NIBKEEP_LAZYFS`; without it, it says
//! so and passes. It runs on Linux, where FUSE is.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

mod common;

use nibkeep_crypto::rand_core::TryRng;
use nibkeep_testkit::lazyfs::{LazyFs, unmount};
use nibkeep_testkit::{TestRng, child};

/// Set in the child, to the directory of the test.
const CHILD: &str = "NIBKEEP_POWER_LOSS_ENCRYPTED_CHILD";
const TEST: &str = "power_cuts_lose_nothing_committed_to_an_encrypted_database";

/// How many power cuts a run makes, unless `NIBKEEP_POWER_LOSS_ROUNDS` says how many.
const ROUNDS: u64 = 8;

/// The writers, each a process of its own, writing to one database at once.
const WRITERS: [&str; 2] = ["first", "second"];

#[test]
fn power_cuts_lose_nothing_committed_to_an_encrypted_database() {
	if let Some(notebook) = std::env::var_os(CHILD) {
		let notebook = PathBuf::from(notebook);
		let name = notebook
			.file_name()
			.and_then(|name| name.to_str())
			.expect("a name")
			.to_owned();
		let directory = notebook.parent().expect("a parent");
		common::write_until_killed(
			&notebook,
			&directory.join("lazyfs/mount/state.sqlite"),
			&name,
		);
	}
	let Some(program) = std::env::var_os("NIBKEEP_LAZYFS") else {
		eprintln!("skipped: set NIBKEEP_LAZYFS to the LazyFS program to cut the power");
		return;
	};
	// A run that was interrupted may have left LazyFS mounted.
	let base = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"));
	unmount(&base.join("power-loss-encrypted/lazyfs/mount"));
	let directory = nibkeep_testkit::empty_directory(&base, "power-loss-encrypted");
	let mut lazyfs = LazyFs::mount(Path::new(&program), &directory.join("lazyfs"));
	let database = lazyfs.mount.join("state.sqlite");
	let mut rng = TestRng::new("power loss");
	for name in WRITERS {
		fs::create_dir_all(directory.join(name)).expect("makes the directory");
	}

	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(ROUNDS, |rounds| rounds.parse().expect("a number"));
	for round in 0..rounds {
		let mut processes: Vec<_> = WRITERS
			.iter()
			.map(|name| {
				let notebook = directory.join(name);
				child::Guard(
					Command::new(std::env::current_exe().expect("this test"))
						.args(["--exact", TEST, "--nocapture"])
						.env(CHILD, &notebook)
						.stdout(Stdio::null())
						.stderr(child::errors(&notebook))
						.spawn()
						.expect("starts a writer"),
				)
			})
			.collect();
		// Up to a second after the first writer's first progress, so that some cuts come
		// while SQLite moves its write-ahead log into the database; the second writer dies
		// as soon as it has made progress too.
		let delay = rng.try_next_u64().expect("infallible") % 1000;
		for (index, (process, name)) in processes.iter_mut().zip(WRITERS).enumerate() {
			let delay = if index == 0 { delay } else { 0 };
			child::kill_after_progress(
				process,
				&directory.join(name),
				round,
				Duration::from_millis(delay),
			);
		}
		lazyfs.cut_power();

		let opened = common::open(&database, &format!("power loss reading {round}"));
		common::check_sound(&opened, round);
		for name in WRITERS {
			let notebook = directory.join(name);
			let [noted] = child::noted(&notebook);
			let kept = common::check(&opened, name, noted, round);
			// The next writer starts from what was kept.
			child::note(&notebook, &[kept]);
		}
	}
}
