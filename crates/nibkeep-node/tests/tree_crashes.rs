//! Crashes in a tree with secret files, on the real disk, on every system but iOS: a child
//! process takes a laptop's and a desk's turns, the desk editing, moving, deleting and adding
//! secret files, the laptop taking them in a sync, updating its copy and committing its own,
//! until the test kills it. The laptop's library and copy must then open, hold every commit
//! noted, and be found sound. The same with power cuts, on LazyFS, is in `power_loss.rs`.

mod common;

use common::tree_rounds::{tree_child, tree_rounds};
use common::tree_turns::TreePlaces;

/// The name of this test, which its child runs.
const TEST: &str = "crashes_in_a_tree_with_secrets_lose_nothing_committed";

/// How many crashes a run makes, unless `NIBKEEP_POWER_LOSS_ROUNDS` says how many: few, as
/// each round's syncs take seconds.
const ROUNDS: u64 = 4;

#[test]
#[cfg_attr(target_os = "ios", ignore = "an iOS app may not start a process")]
fn crashes_in_a_tree_with_secrets_lose_nothing_committed() {
	tree_child();
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "tree-crashes");
	let laptop = directory.join("laptop");
	let places = TreePlaces::new(&directory, &laptop);
	tree_rounds(TEST, ROUNDS, &places, &laptop, || ());
}
