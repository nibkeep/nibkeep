//! `nib space` and `--space`: spaces besides the personal one, where new entities go, and
//! where entities are found ([technical design: sharing]).
//!
//! [technical design: sharing]: ../../../docs/Technical-design.md#13-sharing

mod common;

use clap::Parser;
use common::{ids_hidden, library};
use nibkeep_cli::{Cli, CliError};
use nibkeep_node::NodeError;

#[test]
fn a_space_is_made_listed_and_renamed() {
	let mut test = library("spaces made");
	let made = test.nib(&["space", "new", "Household"]).expect("makes");
	assert!(made.starts_with("Made the space Household ("), "{made}");
	let listed = test.json(&["space", "list"]);
	let spaces = listed["spaces"].as_array().expect("spaces");
	assert_eq!(spaces.len(), 2);
	assert_eq!(spaces[0]["name"], "Personal");
	assert_eq!(spaces[0]["personal"], true);
	assert_eq!(spaces[1]["name"], "Household");
	assert_eq!(spaces[1]["personal"], false);

	test.nib(&["space", "rename", "household", "Home"])
		.expect("renames");
	let listed = test.json(&["space", "list"]);
	assert_eq!(listed["spaces"][1]["name"], "Home");
	// The personal space keeps its name, and a space that is not there is said so.
	assert!(test.nib(&["space", "rename", "Personal", "Mine"]).is_err());
	assert!(test.nib(&["space", "rename", "Garden", "Yard"]).is_err());
	assert!(test.nib(&["--space", "Garden", "list"]).is_err());
	// The device and the user are named as before, though each space has records of them.
	let shown = test.nib(&["show", "laptop"]).expect("shows the device");
	assert!(shown.contains("laptop"), "{shown}");
}

#[test]
fn new_entities_go_to_the_space_chosen_and_are_found_in_any_space() {
	let mut test = library("spaces with entities");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["new", "person", "name=Ada's diary"])
		.expect("adds to the personal space");
	test.nib(&[
		"--space",
		"Household",
		"new",
		"person",
		"name=Boiler service",
	])
	.expect("adds to the household");

	// Listing and finding cover every space, each under its name.
	let listed = test.nib(&["list", "person"]).expect("lists");
	let expected = "\
Personal:
<id>  Ada          std.person
<id>  Ada's diary  std.person

Household:
<id>  Boiler service  std.person
";
	assert_eq!(ids_hidden(&listed), expected, "{listed}");
	let found = &test.json(&["find", "boiler"])["entities"];
	assert_eq!(found[0]["name"], "Boiler service");
	assert_eq!(found[0]["space"], "Household");

	// An entity is shown, and changed, in whichever space holds it.
	let shown = test.nib(&["show", "Boiler service"]).expect("shows");
	assert!(shown.contains("Boiler service"), "{shown}");
	test.nib(&["edit", "Boiler service", "name=Boiler check"])
		.expect("edits");
	let found = &test.json(&["--space", "Household", "find", "boiler"])["entities"];
	assert_eq!(found[0]["name"], "Boiler check");

	// With --space, only that space is looked in.
	assert!(
		test.nib(&["--space", "Personal", "show", "Boiler check"])
			.is_err()
	);
	let only = test.json(&["--space", "Personal", "list", "person"]);
	let names: Vec<_> = only
		.as_array()
		.expect("entities")
		.iter()
		.map(|entity| entity["name"].clone())
		.collect();
	assert_eq!(names, ["Ada", "Ada's diary"]);
}

#[test]
fn an_entity_is_moved_to_another_space() {
	let mut test = library("spaces, moving");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["new", "person", "name=Boiler service"])
		.expect("adds");
	let moved = test
		.nib(&["move", "Boiler service", "--to", "Household"])
		.expect("moves");
	assert!(moved.starts_with("Moved to Household as "), "{moved}");
	// It lists in the household alone, and is found there.
	let found = &test.json(&["find", "boiler"])["entities"];
	assert_eq!(found.as_array().expect("found").len(), 1, "{found}");
	assert_eq!(found[0]["space"], "Household");
	test.nib(&["edit", "Boiler service", "name=Boiler check"])
		.expect("edits it where it is now");
	assert!(
		test.nib(&["move", "Boiler check", "--to", "Household"])
			.is_err()
	);
}

#[test]
fn a_purge_says_which_other_space_keeps_the_same_content() {
	let mut test = library("spaces, a purge of shared content");
	let files = nibkeep_node::resolved_path(&nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"spaces, a purge of shared content, files",
	))
	.expect("resolves");
	let manual = files.join("manual.txt");
	std::fs::write(&manual, "Bleed the radiators in October.\n").expect("writes");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["file", "add", manual.to_str().expect("UTF-8")])
		.expect("adds");
	test.nib(&["move", "manual.txt", "--to", "Household"])
		.expect("moves");

	// The entity left in the personal space still holds the content, which the household
	// keeps.
	let (said, _) =
		test.nib_printing(&["--space", "Personal", "purge", "manual.txt", "std.content="]);
	assert!(
		said.contains("also in   Household, a space that holds the same content, which it keeps\n"),
		"{said}"
	);
	let purged = test.json(&[
		"--space",
		"Personal",
		"purge",
		"manual.txt",
		"std.content=",
		"--yes",
	]);
	assert_eq!(
		purged["elsewhere"].as_array().map(Vec::len),
		Some(1),
		"{purged}"
	);
	std::fs::remove_file(&manual).expect("removes the original");
	let out = files.join("out");
	std::fs::create_dir(&out).expect("makes a folder");
	test.nib(&[
		"file",
		"checkout",
		"manual.txt",
		out.to_str().expect("UTF-8"),
	])
	.expect("checks out the household's copy");
	assert_eq!(
		std::fs::read_to_string(out.join("manual.txt")).expect("reads"),
		"Bleed the radiators in October.\n"
	);
}

#[test]
fn a_folder_moves_with_what_it_holds_once_confirmed() {
	let mut test = library("spaces, a folder moves");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["new", "folder", "name=House"]).expect("adds");
	test.nib(&["new", "folder", "name=Garden", "in-folder=House"])
		.expect("adds a folder in it");
	let (said, refused) = test.nib_printing(&["move", "House", "--to", "Household"]);
	assert!(refused.is_err(), "{said}");
	assert_eq!(
		ids_hidden(&said),
		"\
Moving it takes 2 entities to Household:
  <id>  House
  <id>  Garden
",
		"{said}"
	);
	let moved = test
		.nib(&["move", "House", "--to", "Household", "--yes"])
		.expect("moves");
	assert!(
		moved.starts_with("Moved to Household as ")
			&& moved.contains(", with 1 entity it held\n")
			&& moved.ends_with(
				"What stays in Personal is purged once another device holds what moved.\n"
			),
		"{moved}"
	);
	let found = &test.json(&["find", "garden"])["entities"];
	assert_eq!(found[0]["space"], "Household", "{found}");
	let shown = test.nib(&["show", "Garden"]).expect("shows");
	assert!(shown.contains("House"), "{shown}");

	// An entity that leaves a folder that stays is listed first too.
	test.nib(&["new", "folder", "name=Errands"]).expect("adds");
	test.nib(&["new", "folder", "name=Shops", "in-folder=Errands"])
		.expect("adds a folder in it");
	let (said, refused) = test.nib_printing(&["move", "Shops", "--to", "Household"]);
	assert!(refused.is_err(), "{said}");
	assert_eq!(
		ids_hidden(&said),
		"\
Moving it takes 1 entity to Household:
  <id>  Shops
Some are also in these, which stay in Personal and find them where they go:
  <id>  Errands
",
		"{said}"
	);
}

#[test]
fn a_new_space_has_its_maker_as_its_one_admin_and_the_personal_space_no_members() {
	let mut test = library("space members");
	test.nib(&["space", "new", "Household"]).expect("makes");
	let members = test.json(&["space", "show", "household"]);
	let members = members["members"].as_array().expect("members");
	assert_eq!(members.len(), 1);
	assert_eq!(members[0]["name"], "Ada");
	assert_eq!(members[0]["role"], "admin");
	assert_eq!(members[0]["you"], true);
	assert_eq!(members[0]["devices"][0]["device_name"], "laptop");
	let shown = test.nib(&["space", "show", "Household"]).expect("lists");
	assert!(
		shown.starts_with("Members:\n  Ada (you)  admin  "),
		"{shown}"
	);

	// The space keeps its admin, and the personal space is never shared: both are refused
	// before any code is shown.
	let lowered = test.nib(&["space", "role", "Household", "Ada", "editor"]);
	assert!(
		matches!(lowered, Err(CliError::Node(NodeError::LastAdmin))),
		"{lowered:?}"
	);
	let unasked = test.nib(&["space", "invite", "Household"]);
	assert!(
		matches!(unasked, Err(CliError::UnconfirmedInvite)),
		"{unasked:?}"
	);
	let invited = test.nib(&["space", "invite", "Personal", "--yes"]);
	assert!(
		matches!(invited, Err(CliError::Node(NodeError::NeverShared))),
		"{invited:?}"
	);
	assert!(matches!(
		test.nib(&["space", "show", "Personal"]),
		Err(CliError::Node(NodeError::NeverShared))
	));
	let unknown = test.nib(&["space", "role", "Household", "Grace", "viewer"]);
	assert!(
		matches!(unknown, Err(CliError::Node(NodeError::NoMember(_)))),
		"{unknown:?}"
	);
}

#[test]
fn an_invitation_gives_a_role_and_a_join_takes_a_code() {
	let parses = |arguments: &[&str]| {
		let mut full = vec!["nib"];
		full.extend(arguments);
		Cli::try_parse_from(full).is_ok()
	};
	assert!(parses(&["space", "invite", "Household"]));
	assert!(parses(&[
		"space",
		"invite",
		"Household",
		"--role",
		"viewer",
		"--yes"
	]));
	assert!(parses(&["space", "invite", "Household", "--role", "admin"]));
	// Leaving and removing come in a later version.
	assert!(!parses(&["space", "invite", "Household", "--role", "none"]));
	assert!(!parses(&["space", "role", "Household", "Bea", "none"]));
	assert!(parses(&["space", "join", "37-482-177"]));
	assert!(!parses(&["space", "join", "37-482"]));
}

#[test]
fn an_admin_links_themselves_to_a_person_and_unlinks() {
	let mut test = library("space links");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["--space", "Household", "new", "person", "name=Ada Lovelace"])
		.expect("adds");
	let linked = test
		.nib(&["space", "link", "Household", "Ada", "Ada Lovelace"])
		.expect("links");
	assert_eq!(linked, "Ada is Ada Lovelace now.\n");
	let members = test.json(&["space", "show", "Household"]);
	assert_eq!(members["members"][0]["person"]["name"], "Ada Lovelace");
	let shown = test.nib(&["space", "show", "Household"]).expect("lists");
	assert!(shown.contains("  as Ada Lovelace\n"), "{shown}");
	let unlinked = test
		.nib(&["space", "link", "Household", "Ada"])
		.expect("unlinks");
	assert_eq!(unlinked, "Ada is linked to nobody now, on this side.\n");
	let members = test.json(&["space", "show", "Household"]);
	assert_eq!(members["members"][0]["person"], serde_json::Value::Null);
	// A person the space does not hold is refused.
	assert!(
		test.nib(&["space", "link", "Household", "Ada", "Grace"])
			.is_err()
	);
}

#[test]
fn what_a_move_leaves_behind_is_purged_once_confirmed() {
	let mut test = library("purged behind");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["new", "person", "name=Boiler service"])
		.expect("adds");
	let moved = test
		.nib(&["move", "Boiler service", "--to", "Household"])
		.expect("moves");
	assert!(
		moved.contains("What stays in Personal is purged once another device holds what moved."),
		"{moved}"
	);
	// With no other device, it waits; the person may purge it now.
	let unconfirmed = test.nib(&["purge", "Boiler service", "--behind"]);
	assert!(
		matches!(unconfirmed, Err(CliError::Unconfirmed)),
		"{unconfirmed:?}"
	);
	let purged = test
		.nib(&["purge", "Boiler service", "--behind", "--yes"])
		.expect("purges");
	assert!(purged.starts_with("Purged "), "{purged}");
	// Its name is purged with the rest: nothing is left there to find by it.
	let again = test.nib(&["purge", "Boiler service", "--behind", "--yes"]);
	assert!(
		matches!(again, Err(CliError::Node(NodeError::NoPurgeAsked(_)))),
		"{again:?}"
	);
	// What moved is untouched.
	let shown = test.nib(&["show", "Boiler service"]).expect("shows");
	assert!(shown.contains("Boiler service"), "{shown}");
	// A move that keeps what it leaves asks for nothing.
	test.nib(&["new", "person", "name=Diary"]).expect("adds");
	let kept = test
		.nib(&["move", "Diary", "--to", "Household", "--keep-behind"])
		.expect("moves");
	assert!(!kept.contains("purged"), "{kept}");
	let none = test.nib(&["purge", "Diary", "--behind", "--yes"]);
	assert!(
		matches!(none, Err(CliError::Node(NodeError::NoPurgeAsked(_)))),
		"{none:?}"
	);
	assert_eq!(
		test.json(&["purge", "--behind"])["requests"],
		serde_json::json!([])
	);
	assert_eq!(
		test.nib(&["purge", "--behind"]).expect("lists"),
		"Nothing that a move left behind waits for you to purge.\n"
	);
}

#[test]
fn an_admin_says_whether_a_space_allows_ai_agents() {
	let mut test = library("AI agents allowed");
	test.nib(&["space", "new", "Household"]).expect("makes");
	assert_eq!(
		test.nib(&["space", "ai-agents", "Household"])
			.expect("says"),
		"AI agents are not allowed there.\n"
	);
	assert_eq!(
		test.nib(&["space", "ai-agents", "Household", "on"])
			.expect("allows"),
		"AI agents are allowed there.\n"
	);
	assert_eq!(
		test.json(&["space", "ai-agents", "Household"])["ai_agents"],
		true
	);
	assert!(matches!(
		test.nib(&["space", "ai-agents", "Personal", "on"]),
		Err(CliError::Node(NodeError::NeverShared))
	));
	let parses = |arguments: &[&str]| {
		let mut full = vec!["nib"];
		full.extend(arguments);
		Cli::try_parse_from(full).is_ok()
	};
	assert!(parses(&[
		"device",
		"pair",
		"--ai-agent",
		"Claude",
		"--in",
		"Household",
		"--in",
		"Garden"
	]));
	// And for a device of one's own, a phone too, never both.
	assert!(parses(&["device", "pair", "--in", "Household"]));
	assert!(parses(&["device", "pair", "--phone", "--in", "Household"]));
	assert!(!parses(&[
		"device",
		"pair",
		"--phone",
		"--ai-agent",
		"Claude"
	]));
}

#[test]
fn the_last_member_leaves_a_space_once_confirmed_which_closes_it() {
	let mut test = library("leaving");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["new", "person", "name=Boiler", "--space", "Household"])
		.expect("adds");
	let (said, unconfirmed) = test.nib_printing(&["space", "leave", "Household"]);
	assert!(
		matches!(unconfirmed, Err(CliError::UnconfirmedLeave)),
		"{unconfirmed:?}"
	);
	assert!(
		said.starts_with("You are the last member of Household: leaving closes it for good."),
		"{said}"
	);
	assert_eq!(
		test.nib(&["space", "leave", "Household", "--yes"])
			.expect("leaves"),
		"You left Household, its last member, which closes it: every device keeps what it has, read only.\n"
	);
	// What it holds stays, read only.
	let listed = test.nib(&["space", "list"]).expect("lists");
	assert!(
		listed
			.lines()
			.any(|line| line.starts_with("Household  ") && line.ends_with("  left: read only")),
		"{listed}"
	);
	assert_eq!(test.json(&["space", "list"])["spaces"][1]["left"], true);
	assert!(test.nib(&["show", "Boiler"]).is_ok());
	let added = test.nib(&["new", "person", "name=Gutters", "--space", "Household"]);
	assert!(
		matches!(added, Err(CliError::Node(NodeError::Removed))),
		"{added:?}"
	);
	let shown = test.nib(&["space", "show", "Household"]).expect("lists");
	assert!(
		shown.starts_with("Members:\n  Ada (you)  no longer a member  "),
		"{shown}"
	);
	// Nobody is left to remove, and the personal space is never left: both are refused
	// before anything is said of them.
	let (said, unconfirmed) = test.nib_printing(&["space", "leave", "Personal"]);
	assert!(
		matches!(unconfirmed, Err(CliError::Node(NodeError::NeverShared))),
		"{unconfirmed:?}"
	);
	assert_eq!(said, "");
	let removed = test.nib(&["space", "remove", "Household", "Ada", "--yes"]);
	assert!(
		matches!(removed, Err(CliError::Node(NodeError::Removed))),
		"{removed:?}"
	);
	let left = test.nib(&["space", "leave", "Personal", "--yes"]);
	assert!(
		matches!(left, Err(CliError::Node(NodeError::NeverShared))),
		"{left:?}"
	);
}

#[test]
fn a_shared_spaces_secrets_are_set_up_by_its_admin_with_the_first_secret_saved_there() {
	let mut test = library("space secrets");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&[
		"--space",
		"Household",
		"type",
		"add",
		"login",
		"title:text",
		"password:text:secret",
	])
	.expect("adds the type");
	test.nib(&["--space", "Household", "new", "login", "title=Bank"])
		.expect("adds the login");
	// The first secret sets up this device's secrets, then the space's own.
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.answer(&["tr0ub4dor"]);
	assert_eq!(
		test.nib(&["secret", "show", "Bank", "password"])
			.expect("shows"),
		"hunter2\n"
	);
	let members = test.json(&["space", "show", "Household"]);
	assert_eq!(
		members["members"][0]["devices"][0]["holds_secrets"], true,
		"{members}"
	);
	assert!(
		test.nib(&["space", "show", "Household"])
			.expect("lists")
			.contains("holds the secrets")
	);
	// In a space of one's own, the library's own secrets are asked for and given, and no
	// device of the user's is the phone.
	for command in ["ask", "give"] {
		let named = test.nib(&["--space", "Household", "secret", command, "phone"]);
		assert!(
			matches!(named, Err(CliError::Node(NodeError::WhichDevice(_)))),
			"{named:?}"
		);
	}
}

#[test]
fn a_login_moves_with_its_password_once_the_secrets_unlock_or_leaves_it_behind() {
	let mut test = library("moving secrets");
	test.nib(&["space", "new", "Household"]).expect("makes");
	test.nib(&["type", "add", "login", "title:text", "password:text:secret"])
		.expect("adds the type");
	for title in ["Bank", "Gas"] {
		test.nib(&["new", "login", &format!("title={title}")])
			.expect("adds the login");
	}
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.answer(&["tr0ub4dor", "staple", "staple"]);
	test.nib(&["secret", "set", "Gas", "password"])
		.expect("sets");

	// The move asks for the secrets' passphrase, and takes the password along.
	test.answer(&["tr0ub4dor"]);
	test.nib(&["move", "Bank", "--to", "Household"])
		.expect("moves");
	test.answer(&["tr0ub4dor"]);
	assert_eq!(
		test.nib(&["--space", "Household", "secret", "show", "Bank", "password"])
			.expect("shows"),
		"hunter2\n"
	);
	// Purging what it left behind asks for the passphrase too, as only then does it see
	// which secret values moved: the title, the type and both forms of the password.
	test.answer(&["tr0ub4dor"]);
	let purged = test
		.nib(&["purge", "Bank", "--behind", "--yes"])
		.expect("purges");
	assert!(purged.starts_with("Purged 3 "), "{purged}");
	// What holds no secret value moves, and is purged behind, without asking.
	test.nib(&["new", "person", "name=Plumber"]).expect("adds");
	test.nib(&["move", "Plumber", "--to", "Household"])
		.expect("moves without asking");
	test.nib(&["purge", "Plumber", "--behind", "--yes"])
		.expect("purges without asking");
	// Left behind, the password needs no passphrase, and does not move.
	test.nib(&[
		"move",
		"Gas",
		"--to",
		"Household",
		"--keep-behind",
		"--leave-secrets",
	])
	.expect("moves without asking");
	test.answer(&["tr0ub4dor"]);
	let shown = test.nib(&["--space", "Household", "secret", "show", "Gas", "password"]);
	assert!(
		matches!(shown, Err(CliError::NoSecretValue { .. })),
		"{shown:?}"
	);
}

#[test]
fn a_group_is_made_and_added_to_a_space_as_an_editor_or_a_viewer() {
	let mut test = library("groups");
	test.nib(&["space", "new", "--group", "Household"])
		.expect("makes the group");
	test.nib(&["space", "new", "Family"])
		.expect("makes the space");
	// A group holds no entities of its own, and never joins as an admin.
	assert!(matches!(
		test.nib(&["--space", "Household", "new", "person", "name=Sam"]),
		Err(CliError::Node(NodeError::GroupHoldsNoData))
	));
	assert!(matches!(
		test.nib(&[
			"space",
			"add-group",
			"Family",
			"Household",
			"--role",
			"admin"
		]),
		Err(CliError::Node(NodeError::GroupAsAdmin))
	));
	// Ada is an admin of both, so the group joins at once.
	let added = test
		.nib(&[
			"space",
			"add-group",
			"family",
			"household",
			"--role",
			"viewer",
		])
		.expect("adds the group");
	assert!(
		added.starts_with("Household: viewer\n  Ada: admin"),
		"{added}"
	);
	let groups = test.json(&["space", "show", "Family"])["groups"].clone();
	assert_eq!(groups[0]["name"], "Household");
	assert_eq!(groups[0]["role"], "viewer");
	assert_eq!(groups[0]["agreed"], true);
	let requests = test.nib(&["space", "show", "Household"]).expect("shows");
	assert!(
		requests.contains("\nSpaces that ask for this group:\n  Family ("),
		"{requests}"
	);
	assert!(requests.contains("agreed"), "{requests}");
	// The group is no member of the space's own.
	let members = test.json(&["space", "show", "Family"]);
	assert_eq!(members["members"].as_array().expect("members").len(), 1);
	// Withdrawing takes it out.
	let withdrawn = test
		.nib(&["space", "withdraw", "Household", "family"])
		.expect("withdraws");
	assert!(withdrawn.contains("withdrawn"), "{withdrawn}");
}

#[test]
fn a_group_is_added_to_another_group_and_shown_inside_it() {
	let mut test = library("groups inside groups");
	for group in ["Household", "Kids"] {
		test.nib(&["space", "new", "--group", group])
			.expect("makes the group");
	}
	test.nib(&["space", "new", "Family"])
		.expect("makes the space");
	test.nib(&["space", "add-group", "Household", "Kids"])
		.expect("adds the kids to the household");
	let shown = test
		.nib(&["space", "add-group", "Family", "Household"])
		.expect("adds the household to the family");
	assert!(
		shown.starts_with("Household: editor\n  Ada: admin\n  Kids: editor\n    Ada: admin"),
		"{shown}"
	);
	let groups = test.json(&["space", "show", "Family"])["groups"].clone();
	assert_eq!(groups[0]["name"], "Household");
	assert_eq!(groups[0]["groups"][0]["name"], "Kids");
	assert_eq!(groups[0]["groups"][0]["agreed"], true);
	// No group holds a group that holds it.
	assert!(matches!(
		test.nib(&["space", "add-group", "Kids", "Household"]),
		Err(CliError::Node(NodeError::GroupCycle))
	));
}

#[test]
fn members_through_groups_are_made_direct_kept_out_and_let_back_in_by_name() {
	let parses = |arguments: &[&str]| {
		let mut full = vec!["nib"];
		full.extend(arguments);
		Cli::try_parse_from(full).is_ok()
	};
	assert!(parses(&["space", "make-direct", "Family", "Sam"]));
	assert!(parses(&[
		"space",
		"make-direct",
		"Family",
		"Sam",
		"--role",
		"admin"
	]));
	assert!(!parses(&[
		"space",
		"make-direct",
		"Family",
		"Sam",
		"--role",
		"none"
	]));
	assert!(parses(&["space", "confirm", "Family"]));
	assert!(parses(&["space", "let-in", "Family", "Sam"]));
	let mut test = library("direct members");
	test.nib(&["space", "new", "--group", "Household"])
		.expect("makes the group");
	test.nib(&["space", "new", "Family"])
		.expect("makes the space");
	test.nib(&["space", "add-group", "Family", "Household"])
		.expect("adds the group");
	// Nobody made Ada a direct member, and no group in the family holds a Sam.
	assert!(matches!(
		test.nib(&["space", "confirm", "Family"]),
		Err(CliError::Node(NodeError::NoDirectRecord))
	));
	for arguments in [
		["space", "make-direct", "Family", "Sam"].as_slice(),
		&["space", "let-in", "Family", "Sam"],
		&["space", "remove", "Family", "Sam", "--yes"],
	] {
		let found = test.nib(arguments);
		assert!(
			matches!(found, Err(CliError::Node(NodeError::NoMember(_)))),
			"{arguments:?}: {found:?}"
		);
	}
}

#[test]
fn a_claim_to_the_admin_role_is_checked_first_and_listed() {
	let parses = |arguments: &[&str]| {
		let mut full = vec!["nib"];
		full.extend(arguments);
		Cli::try_parse_from(full).is_ok()
	};
	let mut test = library("claims");
	test.nib(&["space", "new", "Family"])
		.expect("makes the space");
	assert!(parses(&["space", "claim", "Family"]));
	assert!(parses(&["space", "reject", "Family", "Bea"]));
	// Ada is the admin already, so she has nothing to claim, which she is told before what
	// claiming does; and nobody claims.
	assert!(matches!(
		test.nib(&["space", "claim", "Family"]),
		Err(CliError::Node(NodeError::ClaimNeedsEditor))
	));
	assert!(matches!(
		test.nib(&["space", "claim", "Family", "--yes"]),
		Err(CliError::Node(NodeError::ClaimNeedsEditor))
	));
	let shown = test.nib(&["space", "show", "Family"]).expect("shows");
	assert!(!shown.contains("Claims"), "{shown}");
	assert_eq!(
		test.json(&["space", "show", "Family"])["claims"],
		serde_json::json!([])
	);
	assert!(matches!(
		test.nib(&["space", "reject", "Family", "Ada"]),
		Err(CliError::Node(NodeError::NoClaim(_)))
	));
}

/// A time after the tests' changes, for syncs.
fn later() -> u64 {
	nibkeep_testkit::millis_at("2026-10-12", 11, 0)
}

/// A test's library, opened as nib opens it, with the mock keystore.
fn open(test: &common::Test) -> nibkeep_node::Library {
	nibkeep_node::Library::open(&test.here.join(".nib"), &nibkeep_node::Unlocker::Keystore)
		.expect("opens")
}

#[test]
fn a_phone_asks_for_a_space_that_another_device_gives_it() {
	let mut laptop = library("consent laptop");
	laptop.nib(&["space", "new", "Household"]).expect("makes");
	laptop
		.nib(&[
			"--space",
			"Household",
			"new",
			"person",
			"name=Boiler service",
		])
		.expect("adds");
	let household = laptop.json(&["space", "list"])["spaces"][1]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let mut phone = common::Test::with_keystore("consent phone");
	common::pair_device_on_localhost(
		&mut open(&laptop),
		&phone.here.join(".nib"),
		("phone", None, &nibkeep_node::NewDevice::phone()),
		&nibkeep_node::NewProtection::Keystore {
			service: nibkeep_node::KEYSTORE_SERVICE,
		},
		later(),
		&mut nibkeep_testkit::TestRng::new("pairing the phone"),
	);
	// The phone holds the personal space alone, knows Household by name, and says so as it
	// lists and searches.
	assert_eq!(
		ids_hidden(&phone.nib(&["space", "list"]).expect("lists")),
		"Personal  <id>  7 entities\nHousehold  <id>  not on this device: nib space take <id>  on: laptop\n"
	);
	assert_eq!(
		phone.nib(&["find", "boiler"]).expect("finds"),
		"Not looked in, as not on this device: Household.\n"
	);
	assert_eq!(
		phone.json(&["find", "boiler"])["not_looked_in"],
		serde_json::json!(["Household"])
	);
	// Only the phone says whether it takes new spaces.
	assert_eq!(
		phone
			.nib(&["device", "mark", "phone", "--new-spaces", "take"])
			.expect("marks"),
		"phone: an everyday device, at home, takes new spaces\n"
	);
	assert_eq!(
		phone
			.nib(&["device", "mark", "phone", "--new-spaces", "ask"])
			.expect("marks"),
		"phone: an everyday device, at home, asks before taking new spaces\n"
	);
	assert!(matches!(
		phone.nib(&["device", "mark", "laptop", "--new-spaces", "ask"]),
		Err(CliError::NewSpacesElsewhere(_))
	));

	// It asks for Household, and the laptop gives it.
	assert_eq!(
		phone.nib(&["space", "take", "household"]).expect("asks"),
		format!(
			"Asked for the space Household: on a device of yours that holds it, run nib space allow {household} phone.\n"
		)
	);
	common::sync_on_localhost(&phone, &laptop, later);
	let listed = laptop.nib(&["space", "list"]).expect("lists");
	assert!(
		listed.contains(&format!(
			"\n  phone asks for it: nib space allow {household} phone\n"
		)),
		"{listed}"
	);
	assert_eq!(
		laptop
			.nib(&["space", "allow", "Household", "phone"])
			.expect("allows"),
		"Gave the space Household to phone: it takes it as it syncs.\n"
	);
	assert!(matches!(
		laptop.nib(&["space", "allow", "Household", "phone"]),
		Err(CliError::Node(NodeError::NoRequest))
	));
	for _ in 0..3 {
		common::sync_on_localhost(&phone, &laptop, later);
	}
	let found = phone.nib(&["find", "boiler"]).expect("finds");
	assert!(found.contains("Boiler service"), "{found}");
	assert!(!found.contains("Not looked in"), "{found}");
	let listed = phone.json(&["space", "list"]);
	assert_eq!(listed["spaces"][1]["held"], true);
	assert_eq!(
		listed["spaces"][1]["holders"],
		serde_json::json!(["this device", "laptop"])
	);
}

/// Declares that `folder` is on a drive of its own, with the serial `serial`.
fn on_drive(folder: &std::path::Path, serial: &str) {
	std::fs::create_dir_all(folder).expect("makes the folder");
	nibkeep_node::declare_drive(
		folder,
		nibkeep_node::Drive {
			serial: Some(serial.as_bytes().to_vec()),
			folder_ids: true,
			..nibkeep_node::Drive::default()
		},
		None,
	);
}

#[test]
fn a_space_is_dropped_from_one_device_once_another_holds_it() {
	let mut laptop = common::Test::with_keystore("drop laptop");
	on_drive(&laptop.here, "drop laptop disk");
	laptop.nib(&["init", "--name", "Ada"]).expect("starts");
	laptop.nib(&["space", "new", "Household"]).expect("makes");
	laptop
		.nib(&[
			"--space",
			"Household",
			"new",
			"person",
			"name=Boiler service",
		])
		.expect("adds");
	// Alone on one disk, it is refused, and says why.
	let refused = laptop.nib(&["space", "drop", "household"]);
	let Err(CliError::Node(NodeError::DropRefused(refused))) = refused else {
		panic!("refused, not {refused:?}");
	};
	assert_eq!(
		refused.to_string(),
		"no other device of yours holds it; its changes would be on 0 other disks, where its \
		 Critical policy asks for 3, 1 away from home"
	);
	// Its Critical policy asks for one copy, which a desk on a disk of its own meets.
	laptop
		.nib(&[
			"--space",
			"Household",
			"policy",
			"edit",
			"Critical",
			"--copies",
			"1",
			"--away",
			"0",
		])
		.expect("edits");
	let mut desk = common::Test::with_keystore("drop desk");
	on_drive(&desk.here, "drop desk disk");
	common::pair_device_on_localhost(
		&mut open(&laptop),
		&desk.here.join(".nib"),
		("desk", None, &nibkeep_node::NewDevice::computer()),
		&nibkeep_node::NewProtection::Keystore {
			service: nibkeep_node::KEYSTORE_SERVICE,
		},
		later(),
		&mut nibkeep_testkit::TestRng::new("pairing the desk"),
	);
	for _ in 0..3 {
		common::sync_on_localhost(&desk, &laptop, later);
	}
	// The desk drops it, and deletes it once the laptop has learned so.
	assert_eq!(
		desk.nib(&["space", "drop", "Household"]).expect("drops"),
		"Dropped the space Household: it is deleted from this device once another device of \
		 yours that holds it has learned so, after their next syncs.\n"
	);
	assert_eq!(
		ids_hidden(&desk.nib(&["space", "list"]).expect("lists")),
		"Personal  <id>  7 entities\nHousehold  <id>  12 entities  on: laptop  dropped here\n"
	);
	for _ in 0..3 {
		common::sync_on_localhost(&desk, &laptop, later);
	}
	assert_eq!(
		ids_hidden(&desk.nib(&["space", "list"]).expect("lists")),
		"Personal  <id>  7 entities\nHousehold  <id>  declined here  on: laptop\n"
	);
	// Its folder is gone.
	let household = desk.json(&["space", "list"])["spaces"][1]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let replica =
		nibkeep_node::Library::replica_folder(&desk.here.join(".nib")).expect("a replica");
	assert!(replica.join("spaces").exists());
	assert!(!replica.join("spaces").join(&household).exists());
	let found = laptop.nib(&["find", "boiler"]).expect("finds");
	assert!(found.contains("Boiler service"), "{found}");
}

#[test]
fn a_revocation_names_the_spaces_that_this_device_does_not_hold() {
	let mut laptop = library("revoking laptop");
	laptop.nib(&["space", "new", "Household"]).expect("makes");
	let household = laptop.json(&["space", "list"])["spaces"][1]["id"]
		.as_str()
		.expect("an ID")
		.to_owned();
	let keystore = nibkeep_node::NewProtection::Keystore {
		service: nibkeep_node::KEYSTORE_SERVICE,
	};
	let desk = common::Test::with_keystore("revoking desk");
	common::pair_device_on_localhost(
		&mut open(&laptop),
		&desk.here.join(".nib"),
		("desk", None, &nibkeep_node::NewDevice::computer()),
		&keystore,
		later(),
		&mut nibkeep_testkit::TestRng::new("pairing the desk"),
	);
	let mut phone = common::Test::with_keystore("revoking phone");
	common::pair_device_on_localhost(
		&mut open(&laptop),
		&phone.here.join(".nib"),
		("phone", None, &nibkeep_node::NewDevice::phone()),
		&keystore,
		later(),
		&mut nibkeep_testkit::TestRng::new("pairing the phone"),
	);
	for _ in 0..2 {
		common::sync_on_localhost(&desk, &laptop, later);
		common::sync_on_localhost(&phone, &laptop, later);
	}
	// The phone does not hold Household: the laptop revokes the desk there.
	let (printed, told) = phone.nib_printing(&["--json", "device", "revoke", "desk"]);
	assert!(matches!(told, Err(CliError::UnconfirmedRevoke)), "{told:?}");
	let listed: serde_json::Value = serde_json::from_str(&printed).expect("JSON");
	assert_eq!(listed["out_of_reach"][0]["space"], household.as_str());
	assert_eq!(
		listed["out_of_reach"][0]["holders"],
		serde_json::json!(["laptop"])
	);
	let revoked = phone
		.nib(&["device", "revoke", "desk", "--yes"])
		.expect("revokes");
	assert!(
		revoked.contains(
			"Not on this device, so it is revoked there once a device of yours that holds it \
			 syncs with this one: Household (on laptop).\n"
		),
		"{revoked}"
	);
}
