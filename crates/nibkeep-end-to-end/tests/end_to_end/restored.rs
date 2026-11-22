//! A computer restored from a backup of its library's folder, onto a new disk: the backup is
//! a copy, which no command takes for this computer's own replica. The computer joins the
//! library anew beside it, takes in what only the backup held, a file too, which reaches the
//! person's other devices, and revokes the device that it was.

use std::fs;

use nibkeep_end_to_end::{EndToEnd, str};

use crate::common::{devices, paired, pairing_alone, sorted, synced};

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_computer_restored_from_a_backup_joins_anew_and_takes_in_what_only_the_backup_held() {
	let test = EndToEnd::new("restored");
	let (laptop, desktop) = paired(&test);
	desktop.nib(&["new", "person", "name=Grace Hopper"]).ok();
	synced(&laptop, &desktop).ok();

	// The desktop adds someone and a file, and is backed up before it syncs again; then its
	// disk fails.
	desktop.nib(&["new", "person", "name=Alan Turing"]).ok();
	let notes = desktop.library().join("notes");
	fs::create_dir(&notes).expect("makes a folder");
	fs::write(notes.join("milk.txt"), "Milk.\n").expect("writes");
	desktop.nib(&["file", "add", "notes"]).ok();
	let backup = test.folder().join("backup");
	nibkeep_testkit::copy_folder(desktop.library(), &backup);
	fs::remove_dir_all(desktop.library()).expect("the disk fails");

	// On the new disk, the backup is a copy, which no command opens as this computer's.
	let restored = test.device("restored");
	nibkeep_testkit::copy_folder(&backup, restored.library());
	restored
		.nib(&["list", "person"])
		.fails()
		.stdout_eq("")
		.stderr_eq(str![[r#"
nib: the library in [ROOT]/restored/library/.nib is a copy, made on another computer or drive, or in another folder, and is never used here as the device it was; to make this computer a device of its own, add it from another of your devices with nib device pair there and nib init --join here, which takes in what the copy holds, or, where no other device of yours can, use nib recovery from-copy

"#]]);

	// The computer joins anew, beside the copy, and takes in what only the copy held.
	let alone = pairing_alone();
	let mut pairing = laptop.start(&["device", "pair"]);
	let code = pairing.wait_for(r"\d\d-\d{3}-\d{3}");
	restored
		.nib(&["init", "--join", &code, "--device", "restored"])
		.ok()
		.stdout_eq(str![[r#"
Looking for the device that shows the code…
Joined the library: sent nothing, received 15 changes.
Took in 10 changes, keys and files that only a copy from desktop held.
If desktop is gone, revoke it once this device has synced with your others: nib device revoke "desktop"
The copy stays as it was, in [ROOT]/restored/library/.nib/replicas/[..]. NibKeep never deletes it, as a program that syncs the folder would delete the original too; delete it yourself if none does.
It takes [..] [..]B.
Ada, on restored, in [ROOT]/restored/library
Personal: 18 entities, 23 changes

"#]]);
	pairing.finish().ok();
	drop(alone);
	restored.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada           std.person
[SHORT_ID]  Alan Turing   std.person
[SHORT_ID]  Grace Hopper  std.person

"#]]);

	// The laptop gets it from the restored computer, the file's content too, while the
	// desktop that was is away.
	assert_eq!(
		sorted(&synced(&laptop, &restored).fails()),
		[
			"Looking for your other devices…",
			"Synced with nib on laptop: sent 8 changes, 1 key, 1 file (39 bytes), received nothing.",
			"nib on desktop: not found on the network.",
		]
	);
	laptop.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada           std.person
[SHORT_ID]  Alan Turing   std.person
[SHORT_ID]  Grace Hopper  std.person

"#]]);
	let out = laptop.folder().join("out");
	fs::create_dir(&out).expect("makes a folder");
	laptop
		.nib(&["file", "checkout", "milk.txt", out.to_str().expect("text")])
		.ok();
	assert_eq!(
		fs::read_to_string(out.join("milk.txt")).expect("reads"),
		"Milk.\n"
	);

	// The device that the computer was is revoked, as the computer said to do once synced;
	// then the computer syncs with the laptop alone.
	restored
		.nib(&["device", "revoke", "desktop", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Revoked, for good:
  [SHORT_ID]  nib on desktop
Your other devices stop syncing with it once they sync with this one, and the library has a new key that it does not get.

"#]]);
	synced(&laptop, &restored).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on laptop: sent 1 change, received 1 change.

"#]]);
	assert_eq!(
		devices(&restored),
		[
			"nib on restored (this one)",
			"nib on desktop (revoked)",
			"nib on laptop"
		]
	);
}
