//! A copy that waits before purging: an archive node keeps apart what a purge destroys for
//! its wait and no longer, its owner can stop the purge meanwhile, which brings back what it
//! destroyed everywhere, or end the wait, and a shorter wait holds only once the wait before
//! it has passed ([technical design: copies that wait]).
//!
//! [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait

use crate::common;

use crate::common::devices::{Devices, bytes};
use nibkeep_node::{DAY, DeviceRole, NodeError};
use nibkeep_std::CONTENT;

/// A NAS that is an archive node, and a laptop, which hold a photo, purged on the laptop and
/// synced: returns the devices, the laptop, the photo and its content.
fn purged(name: &str) -> (Devices, usize, nibkeep_base::EntityId, nibkeep_base::Hash) {
	let mut devices = Devices::new(name, true);
	let laptop = devices.pair("laptop", None);
	let (photo, content) = devices.add(laptop, "photo.jpg", &bytes(7, 200_000));
	devices.sync(laptop, 0);
	assert!(devices.holds(0, &content) && devices.holds(laptop, &content));
	let space = devices.personal();
	let at = devices.tick();
	let purge = devices.libraries[laptop]
		.purgeable(space, photo, &CONTENT, None)
		.expect("says what it would purge");
	// The purge says first which copies wait, and for how long.
	assert_eq!(purge.waiting.len(), 1, "{:?}", purge.waiting);
	assert_eq!(purge.waiting[0].name.as_deref(), Some("nas"));
	assert!(!purge.waiting[0].this);
	assert_eq!(purge.waiting[0].days, 30);
	devices.libraries[laptop]
		.purge(space, photo, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	devices.sync(laptop, 0);
	(devices, laptop, photo, content)
}

#[test]
fn an_archive_node_keeps_what_a_purge_destroys_for_its_wait_and_no_longer() {
	let (mut devices, laptop, _, content) = purged("waits thirty days");
	// The purge took effect everywhere: no device shows or serves the photo.
	assert!(!devices.holds(0, &content) && !devices.holds(laptop, &content));
	let waiting = devices.libraries[0].waiting_purges().expect("lists");
	assert_eq!(waiting.len(), 1, "{waiting:?}");
	assert_eq!(waiting[0].blobs, 1);
	assert!(waiting[0].changes >= 1);
	assert!(waiting[0].bytes >= 200_000);
	assert_eq!(waiting[0].ends, waiting[0].received + 30 * DAY);
	nibkeep_testkit::assert_empty!(devices.libraries[laptop].waiting_purges().expect("lists"));
	let kept = common::replica_folder(&devices.nas).join("kept");
	assert_eq!(common::devices::blob_files(&kept).len(), 1);

	// A pass before the wait ends destroys nothing; one after it does.
	let before = waiting[0].ends - 1;
	assert_eq!(
		devices.libraries[0].destroy_due(before).expect("destroys"),
		0
	);
	assert_eq!(
		devices.libraries[0].waiting_purges().expect("lists").len(),
		1
	);
	let destroyed = devices.libraries[0]
		.destroy_due(waiting[0].ends)
		.expect("destroys");
	assert!(destroyed >= 2, "{destroyed}");
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));
	nibkeep_testkit::assert_empty!(common::devices::blob_files(
		&common::replica_folder(&devices.nas).join("kept")
	));
}

#[test]
fn stopping_a_purge_brings_back_what_it_destroyed_on_every_device() {
	let (mut devices, laptop, _, content) = purged("purge stopped");
	let waiting = devices.libraries[0]
		.find_waiting_purges(
			&devices.libraries[0].waiting_purges().expect("lists")[0]
				.purge
				.to_string()[..10],
			(&[], None),
		)
		.expect("finds it");
	let at = devices.tick();
	devices.libraries[0]
		.stop_purge(&waiting, at, &mut devices.rng)
		.expect("stops it");
	assert!(devices.holds(0, &content), "back on the NAS at once");
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));
	devices.sync(laptop, 0);
	assert!(
		devices.holds(laptop, &content),
		"and on the laptop after a sync"
	);
}

#[test]
fn ending_the_wait_destroys_at_once_what_the_purge_destroyed() {
	let (mut devices, laptop, _, content) = purged("wait ended");
	let waiting = devices.libraries[0].waiting_purges().expect("lists");
	let at = devices.tick();
	let ended = devices.libraries[0].end_wait(&waiting, at).expect("ends").0;
	assert!(ended >= 2, "{ended}");
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));
	// Nothing is left to stop.
	let at = devices.tick();
	assert!(matches!(
		devices.libraries[0].stop_purge(&waiting, at, &mut devices.rng),
		Err(NodeError::WhichPurge(_))
	));
	devices.sync(laptop, 0);
	assert!(!devices.holds(0, &content) && !devices.holds(laptop, &content));
}

#[test]
fn an_everyday_device_never_waits_and_a_shorter_wait_holds_only_after_the_wait_before_it() {
	let mut devices = Devices::new("shorter wait", true);
	let laptop = devices.pair("laptop", None);
	let space = devices.personal();
	let (first, _) = devices.add(laptop, "first.txt", &bytes(1, 5_000));
	devices.sync(laptop, 0);
	// The laptop, an everyday device, keeps nothing of its own purge.
	let at = devices.tick();
	devices.libraries[laptop]
		.purge(space, first, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	nibkeep_testkit::assert_empty!(devices.libraries[laptop].waiting_purges().expect("lists"));

	// The NAS is marked to wait for none: until the 30 days before pass, it still waits.
	let nas = devices.device_of(0);
	let mut marks = devices.libraries[laptop].device_marks(nas).expect("reads");
	marks.waits = Some(0);
	let marked = devices.tick();
	devices.libraries[laptop]
		.set_device_marks(nas, marks, marked, &mut devices.rng)
		.expect("marks");
	devices.sync(laptop, 0);
	assert_eq!(devices.libraries[0].own_wait_days().expect("reads"), 0);
	let waiting = devices.libraries[0].waiting_purges().expect("lists");
	assert_eq!(
		waiting.len(),
		1,
		"the first purge waits as it did: {waiting:?}"
	);
	let (second, _) = devices.add(laptop, "second.txt", &bytes(2, 5_000));
	devices.sync(laptop, 0);
	let at = devices.tick();
	devices.libraries[laptop]
		.purge(space, second, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	devices.sync(laptop, 0);
	let waiting = devices.libraries[0].waiting_purges().expect("lists");
	assert_eq!(
		waiting.len(),
		2,
		"a purge after the mark waits too: {waiting:?}"
	);
	assert_eq!(waiting[1].ends, waiting[1].received + 30 * DAY);

	// Once 30 days have passed since the mark came, the NAS no longer waits.
	let later = devices.at + 31 * DAY;
	devices.libraries[0].destroy_due(later).expect("destroys");
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));
	devices.at = later;
	let (third, _) = devices.add(laptop, "third.txt", &bytes(3, 5_000));
	devices.sync(laptop, 0);
	let at = devices.tick();
	devices.libraries[laptop]
		.purge(space, third, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	devices.sync(laptop, 0);
	nibkeep_testkit::assert_empty!(devices.libraries[0].waiting_purges().expect("lists"));

	// Marked as an everyday device, it would not wait either; marked to wait longer, it
	// waits at once.
	devices.mark(0, DeviceRole::Everyday, None);
	assert_eq!(devices.libraries[0].own_wait_days().expect("reads"), 0);
	let mut marks = devices.libraries[0].device_marks(nas).expect("reads");
	marks.role = DeviceRole::Archive;
	marks.waits = Some(60);
	let marked = devices.tick();
	devices.libraries[0]
		.set_device_marks(nas, marks, marked, &mut devices.rng)
		.expect("marks");
	let (fourth, _) = devices.add(laptop, "fourth.txt", &bytes(4, 5_000));
	devices.sync(laptop, 0);
	let at = devices.tick();
	devices.libraries[laptop]
		.purge(space, fourth, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	devices.sync(laptop, 0);
	let waiting = devices.libraries[0].waiting_purges().expect("lists");
	assert_eq!(waiting.len(), 1, "{waiting:?}");
	assert_eq!(waiting[0].ends, waiting[0].received + 60 * DAY);
}
