//! S7, a lost phone: the person revokes it from another of their computers. What it wrote
//! that only some of their devices got is set aside on each, until the person reviews it and
//! accepts it; and the phone syncs no more.

use nibkeep_end_to_end::{EndToEnd, str};

use crate::common::{joined, paired, sorted, synced};

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_lost_phone_is_revoked_and_what_it_wrote_is_reviewed_and_accepted() {
	let test = EndToEnd::new("lost phone");
	let (laptop, desktop) = paired(&test);
	let phone = joined(&test, &laptop, "phone");
	// The desktop learns of the phone from the laptop.
	synced(&laptop, &desktop).ok();

	// The phone adds someone, which only the desktop gets before the phone is lost: the
	// laptop is away, which the sync tells.
	phone.nib(&["new", "person", "name=Grace Hopper"]).ok();
	assert_eq!(
		sorted(&synced(&desktop, &phone).fails()),
		[
			"Looking for your other devices…",
			"Synced with nib on desktop: sent 1 change, received nothing.",
			"nib on laptop: not found on the network.",
		]
	);
	desktop.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada           std.person
[SHORT_ID]  Grace Hopper  std.person

"#]]);

	// The laptop revokes the phone, without having what it added.
	laptop
		.nib(&["device", "revoke", "phone", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Revoked, for good:
  [SHORT_ID]  nib on phone
Your other devices stop syncing with it once they sync with this one, and the library has a new key that it does not get.

"#]]);

	// The desktop hears of it from the laptop, as it looks for the phone one last time: what
	// the phone added is set aside on both, and shows on neither.
	assert_eq!(
		sorted(&synced(&laptop, &desktop).fails()),
		[
			"Looking for your other devices…",
			"Synced with nib on laptop: sent 1 change, received 1 change.",
			"nib on phone: not found on the network.",
		]
	);
	for device in [&laptop, &desktop] {
		device.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada  std.person

"#]]);
		device.nib(&["device", "review"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  by nib on phone, at [..], of 2 operations
Accept a device's changes with nib device accept DEVICE --yes.

"#]]);
	}

	// Accepting says what it would accept first; then the person shows, on both once synced.
	desktop
		.nib(&["device", "accept", "phone"])
		.fails()
		.stdout_eq(str![[r#"
Would accept 1 change.

"#]])
		.stderr_eq(str![[r#"
nib: nothing was accepted; add --yes to accept them

"#]]);
	desktop
		.nib(&["device", "accept", "phone", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Accepted 1 change.

"#]]);
	synced(&laptop, &desktop).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on laptop: sent 1 change, received nothing.

"#]]);
	for device in [&laptop, &desktop] {
		device.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada           std.person
[SHORT_ID]  Grace Hopper  std.person

"#]]);
		device.nib(&["device", "review"]).ok().stdout_eq(str![[r#"
No changes are set aside.

"#]]);
	}

	// The phone syncs with neither, though both are there.
	let serving = [&laptop, &desktop].map(|device| device.start(&["sync", "--serve"]));
	let refused = "the other device ended the connection: it does not sync with this device, which it never paired with or revoked.";
	assert_eq!(
		sorted(&phone.nib(&["sync"]).fails()),
		[
			"Looking for your other devices…",
			&format!("nib on desktop: {refused}"),
			&format!("nib on laptop: {refused}"),
		]
	);
	for serving in serving {
		let _ = serving.kill();
	}
}
