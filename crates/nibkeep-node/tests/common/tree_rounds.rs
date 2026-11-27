//! Rounds in a tree with secret files: a child process takes the turns of
//! [`tree_turns`](super::tree_turns) until it is killed, and the laptop's library and copy are
//! checked after each. Shared by the power-cut test with LazyFS, on Linux, and the crash test,
//! on every system but iOS.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use nibkeep_crypto::rand_core::TryRng;
use nibkeep_testkit::{TestRng, child};

use super::tree_turns::{
	TreePlaces, check_tree_after, desk_step, key_in, make_tree_libraries, take_turns,
};

/// Set in the child, to the directory of the test.
const TREE_CHILD: &str = "NIBKEEP_POWER_LOSS_TREE";
/// Set in the child too, to the folder that holds the laptop's library and copy of the tree:
/// on LazyFS, or beside the test's directory where no power is cut.
const TREE_LAPTOP: &str = "NIBKEEP_POWER_LOSS_TREE_LAPTOP";

/// Runs `rounds` rounds of the test named `test`, unless `NIBKEEP_POWER_LOSS_ROUNDS` says how
/// many: a child takes turns until it is killed, `cut` cuts the power or does nothing, and the
/// laptop's library and copy are checked. The test calls [`tree_child`] first.
pub(crate) fn tree_rounds(
	test: &str,
	rounds: u64,
	places: &TreePlaces,
	laptop: &Path,
	mut cut: impl FnMut(),
) {
	let rounds = std::env::var("NIBKEEP_POWER_LOSS_ROUNDS")
		.ok()
		.map_or(rounds, |rounds| rounds.parse().expect("a number"));
	let mut rng = TestRng::new(test);
	make_tree_libraries(places, &mut rng);
	cut();
	child::note(&places.directory, &[0, 0, 0]);
	check_tree_after(0, places, (0, 0), true);
	for round in 1..=rounds {
		let mut process = Command::new(std::env::current_exe().expect("this test"))
			.args(["--exact", &nibkeep_testkit::this_test(), "--nocapture"])
			.env(TREE_CHILD, &places.directory)
			.env(TREE_LAPTOP, laptop)
			.stdout(Stdio::null())
			.stderr(child::errors(&places.directory))
			.spawn()
			.expect("starts the child");
		// After the child's first sync: within the update or the commit after it in every
		// other round, as a sync takes far longer; anywhere in the next turns in the others.
		let most = if round % 2 == 0 { 400 } else { 3_000 };
		let delay = rng.try_next_u64().expect("infallible") % most;
		child::kill_after_progress(
			&mut process,
			&places.directory,
			round,
			Duration::from_millis(delay),
		);
		cut();
		// Verifying takes longer as the library grows: every tenth round, and the last.
		let [own, key, syncs] = child::noted(&places.directory);
		let verify = round % 10 == 0 || round == rounds;
		let (own, _) = check_tree_after(round, places, (own, key), verify);
		// The next child starts from what the tree kept and the copy holds now.
		let (_, key) = key_in(&places.on_laptop).expect("the laptop holds the key");
		child::note(&places.directory, &[own, key, syncs]);
	}
}

/// The child of a test of a tree with secret files, if this process is one: it takes turns
/// until it is killed, and never returns. Otherwise it returns at once.
pub(crate) fn tree_child() {
	if let (Some(directory), Some(laptop)) =
		(std::env::var_os(TREE_CHILD), std::env::var_os(TREE_LAPTOP))
	{
		let places = TreePlaces::new(Path::new(&directory), Path::new(&laptop));
		let noted = child::noted(&places.directory);
		let [own, _, _] = noted;
		let step = desk_step(&places.on_desk);
		let mut rng = TestRng::new(&format!("tree child {own} {step}"));
		take_turns(&places, noted, &mut rng, &mut |noted| {
			child::note(&places.directory, &noted);
		});
	}
}
