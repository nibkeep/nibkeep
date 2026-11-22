//! Ctrl-C ends `nib sync --serve` cleanly, as the technical design's section on the sync role says: it lets go of the
//! device's sync role, which another `nib sync --serve` that waits for it takes over at once,
//! and syncs on; one stopped while it waits ends too, holding nothing. On Unix, where the
//! tests interrupt commands, as Windows lets no program press Ctrl-C in another's console.

#![cfg(unix)]

use nibkeep_end_to_end::{Device, str};

use crate::common::{paired, synced};

/// The names of the people a device lists.
fn people(device: &Device) -> Vec<String> {
	device
		.nib(&["--json", "list", "person"])
		.ok()
		.json()
		.as_array()
		.expect("a list")
		.iter()
		.map(|person| person["name"].as_str().expect("a name").to_owned())
		.collect()
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn ctrl_c_ends_a_holder_cleanly_and_the_one_that_waits_takes_over() {
	let test = nibkeep_end_to_end::EndToEnd::new("interrupted");
	let (laptop, desktop) = paired(&test);

	// One holds the laptop's sync role, and another waits for it.
	let mut first = laptop.start(&["sync", "--serve"]);
	first.wait_for("Syncing this device");
	let mut second = laptop.start(&["sync", "--serve"]);
	second.wait_for("takes over when it ends");
	desktop.nib(&["new", "person", "name=Grace Hopper"]).ok();
	desktop.nib(&["sync"]).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on laptop: sent 1 change, received nothing.

"#]]);

	// Ctrl-C ends the first, which lets go; the second takes over, and syncs.
	first.interrupt();
	let ended = first.finish().ok().stderr_eq("");
	assert!(
		ended
			.stdout
			.starts_with("Syncing this device with your other devices; stop with Ctrl-C.\n"),
		"{ended}"
	);
	second.wait_for("Syncing this device");
	desktop.nib(&["new", "person", "name=Alan Turing"]).ok();
	desktop.nib(&["sync"]).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on laptop: sent 1 change, received nothing.

"#]]);
	assert_eq!(people(&laptop), ["Ada", "Alan Turing", "Grace Hopper"]);

	// A third that waits, stopped before it takes over, ends, with nothing to let go of.
	let mut third = laptop.start(&["sync", "--serve"]);
	third.wait_for("takes over when it ends");
	third.interrupt();
	third.finish().ok().stderr_eq("").stdout_eq(str![[r#"
nib sync --serve syncs this device; this one takes over when it ends. Stop with Ctrl-C.

"#]]);

	// The second ends the same way; then nothing holds the role, and the laptop syncs by
	// itself, without asking a holder.
	second.interrupt();
	second.finish().ok().stderr_eq("");
	synced(&desktop, &laptop).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on desktop: sent nothing, received nothing.

"#]]);
}
