//! Two people, each with a library of their own on their own computer: one makes a space and
//! invites the other, who joins it with the code shown; both change it and sync, and the
//! admin gives the other another role, and later removes them, and accepts what they did
//! meanwhile ([technical design: sharing]).
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

use nibkeep_end_to_end::{Device, EndToEnd, str};

use crate::common::pairing_alone;

/// Ada's laptop and Bea's phone, each with a library of their own, and Ada's space
/// Household, with a note and the person Bea Martin in it.
fn two_people(test: &EndToEnd) -> (Device, Device) {
	let ada = test.device("ada");
	let bea = test.device("bea");
	for (device, name, computer) in [(&ada, "Ada", "laptop"), (&bea, "Bea", "phone")] {
		device
			.nib(&[
				"init",
				"--name",
				name,
				"--device",
				computer,
				"--languages",
				"en",
			])
			.ok();
	}
	ada.nib(&["space", "new", "Household"]).ok();
	ada.nib(&[
		"--space",
		"Household",
		"new",
		"person",
		"name=Boiler service",
	])
	.ok();
	ada.nib(&["--space", "Household", "new", "person", "name=Bea Martin"])
		.ok();
	(ada, bea)
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn one_invites_another_to_a_space_and_both_change_it() {
	let test = EndToEnd::new("shared space");
	let (ada, bea) = two_people(&test);

	// Ada invites Bea as an editor, who is Bea Martin in the space, and is told to make a
	// second admin; Bea joins with the code, agrees, and both list both members.
	let alone = pairing_alone();
	let mut inviting = ada.start(&[
		"space",
		"invite",
		"Household",
		"--as",
		"Bea Martin",
		"--yes",
	]);
	let code = inviting.wait_for(r"\d\d-\d{3}-\d{3}");
	bea.nib(&["space", "join", &code, "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Looking for the device that shows the code…
Joined Household as an editor; your other devices that take new spaces get it when they sync.
Synced with it: sent nothing, received 12 changes.
You are Bea Martin there now.

"#]]);
	inviting.finish().ok().stdout_eq(str![[r#"
On their device, in their library, run:

    nib space join [CODE]

They join Household as an editor. The code works for 10 minutes; 3 wrong tries end it. Both devices must be on the same network. Waiting…
Household has one admin. So that it is never stuck without one, consider making them an admin too, with --role admin.
Added Bea to Household as an editor, on phone.
Synced with it: sent 12 changes, received nothing.

"#]]);
	drop(alone);
	ada.nib(&["space", "show", "Household"])
		.ok()
		.stdout_eq(str![[r#"
Members:
  Ada (you)  admin  [ID]
      nib on laptop
  Bea  editor  [ID]  named Bea Martin by an admin, which they have not agreed to
      nib on phone

"#]]);
	bea.nib(&["space", "show", "Household"])
		.ok()
		.stdout_eq(str![[r#"
Members:
  Ada  admin  [ID]
      nib on laptop
  Bea (you)  editor  [ID]  as Bea Martin
      nib on phone

"#]]);
	bea.nib(&["--space", "Household", "list", "person"])
		.ok()
		.stdout_eq(str![[r#"
[SHORT_ID]  Bea Martin      std.person
[SHORT_ID]  Boiler service  std.person

"#]]);

	// What Bea adds reaches Ada, whose computer serves meanwhile.
	bea.nib(&[
		"--space",
		"Household",
		"new",
		"person",
		"name=Window cleaner",
	])
	.ok();
	let serving = ada.start(&["sync", "--serve"]);
	bea.nib(&["sync"]).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with Ada's nib on laptop: sent 2 changes, received nothing.

"#]]);
	let _ = serving.kill();
	ada.nib(&["--space", "Household", "list", "person"])
		.ok()
		.stdout_eq(str![[r#"
[SHORT_ID]  Bea Martin      std.person
[SHORT_ID]  Boiler service  std.person
[SHORT_ID]  Window cleaner  std.person

"#]]);

	// Bea's agreement reached Ada with it: she is Bea Martin there for both now.
	ada.nib(&["space", "show", "Household"])
		.ok()
		.stdout_eq(str![[r#"
Members:
  Ada (you)  admin  [ID]
      nib on laptop
  Bea  editor  [ID]  as Bea Martin
      nib on phone

"#]]);

	// Only an admin gives roles: Ada makes Bea a viewer, and Bea cannot make Ada one.
	ada.nib(&["space", "role", "Household", "Bea", "viewer"])
		.ok()
		.stdout_eq(str![[r#"
Bea is a viewer now; their devices learn of it when they sync.

"#]]);
	bea.nib(&["space", "role", "Household", "Ada", "viewer"])
		.fails()
		.stderr_eq(str![[r#"
nib: only an admin of the space may give roles

"#]]);
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn an_admin_removes_a_member_and_accepts_what_they_did_meanwhile() {
	let test = EndToEnd::new("removed member");
	let (ada, bea) = two_people(&test);
	let alone = pairing_alone();
	let mut inviting = ada.start(&["space", "invite", "Household", "--yes"]);
	let code = inviting.wait_for(r"\d\d-\d{3}-\d{3}");
	bea.nib(&["space", "join", &code]).ok();
	inviting.finish().ok();
	drop(alone);

	// Ada removes Bea, who adds something meanwhile, not knowing.
	ada.nib(&["space", "remove", "Household", "Bea"])
		.fails()
		.stdout_eq(str![[r#"
Removing Bea ends every device of theirs in Household: they keep what they have, read only, and get nothing added after, and the space gets a new key. What they change before they learn of it waits for an admin's review.

"#]]);
	ada.nib(&["space", "remove", "Household", "Bea", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Bea is no longer a member of Household, which has a new key; their devices learn of it when they sync.

"#]]);
	bea.nib(&["--space", "Household", "new", "person", "name=Shelf"])
		.ok();
	let serving = ada.start(&["sync", "--serve"]);
	bea.nib(&["sync"]).ok();
	let _ = serving.kill();

	// Bea learned of it, and keeps what she had, read only; what she added meanwhile is set
	// aside on her device too.
	bea.nib(&["space", "show", "Household"])
		.ok()
		.stdout_eq(str![[r#"
Members:
  Ada  admin  [ID]
      nib on laptop
  Bea (you)  no longer a member  [ID]
      nib on phone

Changes set aside for review:
  [SHORT_ID]  by Bea, on phone, at [..], of [..], on Shelf[..]
  Accept a member's changes with nib space accept SPACE MEMBER --yes.

"#]]);
	bea.nib(&["--space", "Household", "new", "person", "name=Lamp"])
		.fails()
		.stderr_eq(str![[r#"
nib: you are no longer a member of this space: you keep what you had there, read only, and come back only if an admin invites you again

"#]]);

	// What Bea added waits for Ada's review, and counts once Ada accepts it.
	ada.nib(&["--space", "Household", "list", "person"])
		.ok()
		.stdout_eq(str![[r#"
[SHORT_ID]  Bea Martin      std.person
[SHORT_ID]  Boiler service  std.person

"#]]);
	ada.nib(&["space", "show", "Household"])
		.ok()
		.stdout_eq(str![[r#"
Members:
...
Changes set aside for review:
  [SHORT_ID]  by Bea, on phone, at [..], of [..], on Shelf
  Accept a member's changes with nib space accept SPACE MEMBER --yes.

"#]]);
	ada.nib(&["space", "accept", "Household", "Bea", "--yes"])
		.ok()
		.stdout_eq(str![[r#"
Accepted 1 change.

"#]]);
	ada.nib(&["--space", "Household", "list", "person"])
		.ok()
		.stdout_eq(str![[r#"
[SHORT_ID]  Bea Martin      std.person
[SHORT_ID]  Boiler service  std.person
[SHORT_ID]  Shelf           std.person

"#]]);
}
