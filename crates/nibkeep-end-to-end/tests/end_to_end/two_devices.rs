//! Two computers of the person's on the home network: one joins the other's library with the
//! code it shows, typed again if mistyped, both change it and sync, a value changed on both
//! while apart competes until one is chosen, and a revoked device syncs no more.

use nibkeep_end_to_end::{Device, EndToEnd, str};

use crate::common::{paired, pairing_alone, synced};

/// The names that compete in an entity's field of names, sorted.
fn competing_names(device: &Device, entity: &str) -> Vec<String> {
	let entity = device.nib(&["--json", "show", entity]).ok().json();
	let name = entity["fields"]
		.as_array()
		.expect("fields")
		.iter()
		.find(|field| field["name"] == "org.nibkeep.name")
		.expect("a name");
	assert_eq!(name["competing"], true, "{name}");
	let mut names: Vec<String> = name["values"]
		.as_array()
		.expect("values")
		.iter()
		.map(|value| value["value"]["full"].as_str().expect("a name").to_owned())
		.collect();
	names.sort_unstable();
	names
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn two_computers_pair_sync_compete_and_part() {
	let test = EndToEnd::new("two devices");
	let (laptop, desktop) = paired(&test);

	// Each adds someone, and syncing brings both to both.
	let grace = laptop
		.nib(&["--json", "new", "person", "name=Grace Hopper"])
		.ok()
		.json()["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	desktop.nib(&["new", "person", "name=Alan Turing"]).ok();
	synced(&laptop, &desktop).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on laptop: sent 1 change, received 1 change.

"#]]);
	for device in [&laptop, &desktop] {
		device.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada           std.person
[SHORT_ID]  Alan Turing   std.person
[SHORT_ID]  Grace Hopper  std.person

"#]]);
	}

	// Renamed on both while apart, the two names compete once synced, and both show the same:
	// in the order of the changes' IDs, which are random.
	laptop
		.nib(&["edit", &grace, "name=Grace Brewster Hopper"])
		.ok();
	desktop.nib(&["edit", &grace, "name=Amazing Grace"]).ok();
	synced(&laptop, &desktop).ok();
	let shown = laptop.nib(&["show", &grace]).ok().stdout_eq(str![[r#"
[..]
  id                [ID]
  type              std.person
  org.nibkeep.name  [..]; [..]  (competing)

"#]]);
	assert_eq!(
		desktop.nib(&["show", &grace]).ok().stdout,
		shown.stdout,
		"the same on both"
	);
	assert_eq!(
		competing_names(&laptop, &grace),
		["Amazing Grace", "Grace Brewster Hopper"]
	);
	// One chosen, the competition ends, on both once synced.
	desktop
		.nib(&["edit", &grace, "name=Grace Brewster Hopper"])
		.ok();
	synced(&laptop, &desktop).ok();
	laptop.nib(&["show", &grace]).ok().stdout_eq(str![[r#"
Grace Brewster Hopper
  id                [ID]
  type              std.person
  org.nibkeep.name  Grace Brewster Hopper

"#]]);

	// Revoked by the laptop, the desktop syncs with it no more, and what it adds stays its own.
	laptop
		.nib(&["device", "revoke", "desktop", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Revoked, for good:
  [SHORT_ID]  nib on desktop
Your other devices stop syncing with it once they sync with this one, and the library has a new key that it does not get.

"#]]);
	desktop.nib(&["new", "person", "name=Linus"]).ok();
	synced(&laptop, &desktop)
		.fails()
		.stdout_eq(str![[r#"
Looking for your other devices…
nib on laptop: the other device ended the connection: it does not sync with this device, which it never paired with or revoked.

"#]])
		.stderr_eq(str![[r#"
nib: a device could not be synced with

"#]]);
	laptop.nib(&["list", "person"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Ada                    std.person
[SHORT_ID]  Alan Turing            std.person
[SHORT_ID]  Grace Brewster Hopper  std.person

"#]]);
	laptop.nib(&["device", "list"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  nib on laptop (this one)
[SHORT_ID]  nib on desktop (revoked)

"#]]);
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_mistyped_code_is_typed_again() {
	let test = EndToEnd::new("mistyped code");
	let laptop = test.device("laptop");
	let desktop = test.device("desktop");
	laptop
		.nib(&[
			"init",
			"--name",
			"Ada",
			"--device",
			"laptop",
			"--languages",
			"en",
		])
		.ok();
	let _alone = pairing_alone();
	let mut pairing = laptop.start(&["device", "pair"]);
	let code = pairing.wait_for(r"\d\d-\d{3}-\d{3}");
	let last = if code.ends_with('0') { '1' } else { '0' };
	let mistyped = format!("{}{last}", &code[..code.len() - 1]);
	desktop
		.nib(&["init", "--join", &mistyped, "--device", "desktop"])
		.fails()
		.stdout_eq(str![[r#"
Looking for the device that shows the code…

"#]])
		.stderr_eq(str![[r#"
nib: the pairing code did not match

"#]]);
	pairing.wait_for("A device typed a wrong code: 2 tries left.");
	desktop
		.nib(&["init", "--join", &code, "--device", "desktop"])
		.ok();
	pairing.finish().ok().stdout_eq(str![[r#"
On the new device, in the folder for the library, run:

    nib init --join [CODE]

The code works for 10 minutes; 3 wrong tries end it. Both devices must be on the same network. Waiting…
A device typed a wrong code: 2 tries left.
Paired nib on desktop.
Synced with it: sent 11 changes, received nothing.

"#]]);
}
