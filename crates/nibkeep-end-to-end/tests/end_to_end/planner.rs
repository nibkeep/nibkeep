//! A desktop that keeps few files, once the laptop is an archive node: a sync brings it every
//! change and key but no file it does not keep, and checking a file out fetches it from the
//! laptop, which serves, over the home network ([technical design: the planner]).
//!
//! [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner

use std::fs;

use nibkeep_end_to_end::{EndToEnd, str};

use crate::common::{paired, synced};

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_device_that_keeps_nothing_fetches_a_file_when_it_checks_it_out() {
	let test = EndToEnd::new("planner");
	let (laptop, desktop) = paired(&test);
	laptop
		.nib(&["device", "mark", "laptop", "--archive"])
		.ok()
		.stdout_eq(str![[r#"
laptop: an archive node, at home, waits 30 days before purging

"#]]);
	desktop
		.nib(&["device", "mark", "desktop", "--keeps", "0"])
		.ok()
		.stdout_eq(str![[r#"
desktop: an everyday device, at home, keeps up to 0 bytes of files

"#]]);
	fs::write(laptop.library().join("report.txt"), "Quarterly report.\n").expect("writes");
	laptop.nib(&["file", "add", "report.txt"]).ok();

	// The desktop gets the file's change and key, and no copy of it, which the laptop holds;
	// the devices of the tests share this computer's disk.
	let ran = synced(&laptop, &desktop).ok();
	assert!(
		ran.stdout.contains("Synced with nib on laptop:") && !ran.stdout.contains("file ("),
		"{ran}"
	);
	desktop
		.nib(&["file", "where", "report.txt"])
		.ok()
		.stdout_eq(str![[r#"
report.txt: on 1 disk
  this disk: nib on desktop (this one), nib on laptop: sealed, checked [TIME]

"#]]);

	// Checking it out fetches it from the laptop, which serves.
	let out = desktop.folder().join("out");
	fs::create_dir(&out).expect("makes a folder");
	let serving = laptop.start(&["sync", "--serve"]);
	let ran = desktop
		.nib(&[
			"file",
			"checkout",
			"report.txt",
			out.to_str().expect("text"),
		])
		.ok();
	let _ = serving.kill();
	assert!(
		ran.stdout.starts_with("Fetching it from nib on laptop…\n"),
		"{ran}"
	);
	assert_eq!(
		fs::read_to_string(out.join("report.txt")).expect("reads"),
		"Quarterly report.\n"
	);
}
