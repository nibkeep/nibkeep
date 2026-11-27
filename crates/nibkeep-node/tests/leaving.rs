//! Leaving a shared space and removing members ([technical design: sharing]): a member leaves, or an
//! admin removes them, which ends every replica of theirs there; a removed member's replicas
//! get their removal and nothing added after; what they did without knowing is set aside;
//! the others give the space a new key, which the removed never get; the only admin leaves
//! only once another is one, and the last member's leaving closes the space; and a key that
//! a member made while being removed is still given out, so that what was written with it
//! stays readable to those who join later; and an admin reviews what was set aside, and
//! accepts only what the member's role allowed.
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

use crate::common;

use crate::common::{invite_member, now, pair_named, sync_in_memory, user_library};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{Name, TypeName, Value};
use nibkeep_node::{Library, MemberRole, NodeError, PairingCode};
use nibkeep_testkit::TestRng;

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// Adds an entity named `name` to `space`.
fn note(
	library: &mut Library,
	space: SpaceId,
	name: &str,
	at: u64,
	rng: &mut TestRng,
) -> Result<EntityId, NodeError> {
	library.create_entity(
		space,
		&[TypeName::from_static("std.person")],
		vec![(
			"org.nibkeep.name".parse().expect("a field"),
			Value::Name(Name::new(name)),
		)],
		at,
		rng,
	)
}

/// The names of what a search finds in a space.
fn found(library: &Library, space: SpaceId, words: &str) -> Vec<String> {
	let mut names: Vec<String> = library
		.search(space, words)
		.expect("searches")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

/// Each member's name and role, by name.
fn roles(library: &Library, space: SpaceId) -> Vec<(String, MemberRole)> {
	let mut roles: Vec<(String, MemberRole)> = library
		.members(space)
		.expect("lists the members")
		.into_iter()
		.map(|member| (member.name.unwrap_or_default(), member.role))
		.collect();
	roles.sort();
	roles
}

/// How many changes a space's log holds.
fn changes(library: &Library, space: SpaceId) -> usize {
	library
		.status()
		.expect("reads")
		.spaces
		.into_iter()
		.find(|status| status.id == space)
		.expect("the space")
		.changes
}

/// The number of the key that a library writes a space with, and the numbers it holds.
fn writing_key(library: &Library, space: SpaceId) -> u64 {
	library.space_key_numbers(space).expect("reads").1
}

fn holds_key(library: &Library, space: SpaceId, number: u64) -> bool {
	library
		.space_key_numbers(space)
		.expect("reads")
		.0
		.contains(&number)
}

/// A member record by its name.
fn member(library: &Library, space: SpaceId, name: &str) -> EntityId {
	library.find_member(space, name).expect("a member")
}

/// Ada's household, where Ada is the admin, with a note, and each of `others` invited as
/// the role given, every one synced with Ada.
struct Household {
	ada: Library,
	others: Vec<Library>,
	space: SpaceId,
	at: u64,
}

impl Household {
	fn new(others: &[(&str, MemberRole)], rng: &mut TestRng) -> Self {
		let mut ada = user_library("Ada", "laptop", rng);
		let mut at = now() + 100_000;
		let space = ada.create_space("Household", at, rng).expect("makes it");
		note(&mut ada, space, "Boiler service", at + 500, rng).expect("adds");
		let mut joined = Vec::new();
		for (name, role) in others {
			let mut library = user_library(name, "phone", rng);
			at += 2_000;
			invite_member(&mut ada, (space, *role), &mut library, at);
			joined.push(library);
		}
		for library in &mut joined {
			at = sync_rounds(&mut ada, library, at + 1_000, 1);
		}
		Self {
			ada,
			others: joined,
			space,
			at: at + 1_000,
		}
	}
}

#[test]
fn a_removed_member_gets_their_removal_and_nothing_added_after() {
	let mut rng = TestRng::new("a removed member");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(&[("Bea", MemberRole::Editor)], &mut rng);
	let bea = &mut others[0];
	let before = changes(bea, space);
	let bea_member = member(&ada, space, "Bea");
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	note(&mut ada, space, "Gutters", at + 100, &mut rng).expect("adds");
	let file = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&common::unique_folder("removed files"),
	)
	.join("invoice.txt");
	std::fs::write(&file, b"The boiler's invoice").expect("writes");
	ada.add_file(space, &file, at + 200, &mut rng)
		.expect("adds the file");
	// Of what came after, nothing is sent: no change, no key and no file.
	at += 1_000;
	let (sent, received) = sync_in_memory(&mut ada, bea, at, (0, 1));
	received.expect("Bea syncs");
	let sent = sent.expect("Ada syncs");
	let household = sent
		.spaces
		.iter()
		.find(|synced| synced.space == space)
		.expect("syncs the space");
	assert_eq!(
		(
			household.sent.changes,
			household.sent.keys,
			household.sent.blobs
		),
		(1, 0, 0),
		"only the removal"
	);
	at = sync_rounds(&mut ada, bea, at + 1_000, 2);

	// Bea knows she is no member any more, and keeps what she had, read only.
	for library in [&ada, &*bea] {
		assert_eq!(
			roles(library, space),
			[
				("Ada".to_owned(), MemberRole::Admin),
				("Bea".to_owned(), MemberRole::None)
			]
		);
	}
	assert!(bea.has_left(space).expect("reads"));
	assert!(!ada.has_left(space).expect("reads"));
	assert_eq!(found(bea, space, "boiler"), ["Boiler service"]);
	// She got her removal, and nothing added after: not the new key, nor the note.
	assert_eq!(changes(bea, space), before + 1);
	assert_eq!(found(bea, space, "gutters"), Vec::<String>::new());
	assert!(!holds_key(bea, space, writing_key(&ada, space)));
	assert_eq!(found(&ada, space, "gutters"), ["Gutters"]);
	// What she would write is refused.
	let written = note(bea, space, "Chimney", at, &mut rng);
	assert!(matches!(written, Err(NodeError::Removed)), "{written:?}");
}

#[test]
fn what_a_removed_member_did_without_knowing_is_set_aside_on_every_device() {
	let mut rng = TestRng::new("unknowing");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(
		&[("Bea", MemberRole::Editor), ("Cy", MemberRole::Editor)],
		&mut rng,
	);
	let bea_member = member(&ada, space, "Bea");
	// Ada removes Bea while Bea adds a note, offline.
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	let (bea, cy) = others.split_at_mut(1);
	let (bea, cy) = (&mut bea[0], &mut cy[0]);
	note(bea, space, "Bea's shelf", at + 100, &mut rng).expect("adds, not knowing");
	assert_eq!(found(bea, space, "shelf"), ["Bea's shelf"]);
	at = sync_rounds(bea, &mut ada, at + 1_000, 2);
	at = sync_rounds(&mut ada, cy, at, 2);
	for library in [&ada, &*bea, &*cy] {
		assert_eq!(found(library, space, "shelf"), Vec::<String>::new());
	}
	// Cy writes on, with a key that Bea does not get.
	note(cy, space, "Cy's lamp", at, &mut rng).expect("adds");
	sync_rounds(cy, &mut ada, at + 1_000, 1);
	assert_eq!(found(&ada, space, "lamp"), ["Cy's lamp"]);
	assert!(!holds_key(bea, space, writing_key(cy, space)));
}

#[test]
fn a_member_who_leaves_reads_nothing_added_later_and_the_others_make_a_new_key() {
	let mut rng = TestRng::new("leaving");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(
		&[("Bea", MemberRole::Editor), ("Cy", MemberRole::Viewer)],
		&mut rng,
	);
	let (bea, cy) = others.split_at_mut(1);
	let (bea, cy) = (&mut bea[0], &mut cy[0]);
	let old_key = writing_key(&ada, space);
	assert_eq!(bea.own_role(space).expect("reads"), MemberRole::Editor);
	assert_eq!(cy.own_role(space).expect("reads"), MemberRole::Viewer);
	assert!(
		!bea.leave(space, at, &mut rng).expect("leaves"),
		"others stay"
	);
	assert!(bea.has_left(space).expect("reads"));
	assert_eq!(bea.own_role(space).expect("reads"), MemberRole::None);
	let written = note(bea, space, "After leaving", at + 100, &mut rng);
	assert!(matches!(written, Err(NodeError::Removed)), "{written:?}");
	// Ada learns of it, and gives the space a new key, which Cy gets and Bea does not.
	at = sync_rounds(bea, &mut ada, at + 1_000, 1);
	assert_ne!(writing_key(&ada, space), old_key);
	note(&mut ada, space, "Gutters", at, &mut rng).expect("adds");
	at = sync_rounds(&mut ada, cy, at + 1_000, 2);
	assert_eq!(found(cy, space, "gutters"), ["Gutters"]);
	assert_eq!(writing_key(cy, space), writing_key(&ada, space));
	sync_rounds(&mut ada, bea, at, 2);
	assert_eq!(found(bea, space, "gutters"), Vec::<String>::new());
	assert!(!holds_key(bea, space, writing_key(&ada, space)));
	assert_eq!(found(bea, space, "boiler"), ["Boiler service"]);
}

#[test]
fn the_only_admin_leaves_once_another_is_one_and_the_last_member_closes_the_space() {
	let mut rng = TestRng::new("the last member");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(&[("Bea", MemberRole::Editor)], &mut rng);
	let bea = &mut others[0];
	let left = ada.leave(space, at, &mut rng);
	assert!(matches!(left, Err(NodeError::LastAdmin)), "{left:?}");
	let bea_member = member(&ada, space, "Bea");
	ada.give_role(space, bea_member, MemberRole::Admin, at, &mut rng)
		.expect("gives the role");
	assert!(!ada.leave(space, at + 100, &mut rng).expect("leaves"));
	at = sync_rounds(&mut ada, bea, at + 1_000, 1);
	assert!(bea.leave(space, at, &mut rng).expect("leaves"), "closes it");
	sync_rounds(&mut ada, bea, at + 1_000, 1);
	assert_eq!(
		roles(bea, space),
		[
			("Ada".to_owned(), MemberRole::None),
			("Bea".to_owned(), MemberRole::None)
		]
	);
	// Ada left first, and gets nothing after her leaving, Bea's neither.
	assert_eq!(
		roles(&ada, space),
		[
			("Ada".to_owned(), MemberRole::None),
			("Bea".to_owned(), MemberRole::Admin)
		]
	);
	for library in [&ada, &*bea] {
		assert!(library.has_left(space).expect("reads"));
		assert_eq!(found(library, space, "boiler"), ["Boiler service"]);
	}
	// A space of one closes as its member leaves.
	let mine = ada.create_space("Mine", at, &mut rng).expect("makes it");
	assert!(ada.leave(mine, at + 100, &mut rng).expect("leaves"));
	let written = note(&mut ada, mine, "After", at + 200, &mut rng);
	assert!(matches!(written, Err(NodeError::Removed)), "{written:?}");
	// The personal space is never left.
	let personal = ada.personal_space();
	let left = ada.leave(personal, at + 300, &mut rng);
	assert!(matches!(left, Err(NodeError::NeverShared)), "{left:?}");
}

#[test]
fn removing_a_member_ends_every_replica_of_theirs() {
	let mut rng = TestRng::new("every replica");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(&[("Bea", MemberRole::Editor)], &mut rng);
	let bea = &mut others[0];
	// Bea's desk joins her library, and gets the space through her personal space.
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&common::unique_folder("Bea desk"),
	);
	std::fs::remove_dir(&folder).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_named(bea, &folder, "desk", (&code, &code), &mut rng);
	paired.expect("pairs");
	let mut desk = joined.expect("joins").library;
	at = sync_rounds(bea, &mut desk, at.max(now() + 70_000), 4);
	at = sync_rounds(bea, &mut ada, at, 1);
	let replicas: usize = ada
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|found| found.name.as_deref() == Some("Bea"))
		.expect("Bea")
		.replicas
		.len();
	assert_eq!(replicas, 2);
	// Ada removes Bea; the desk, not knowing, adds a note.
	let bea_member = member(&ada, space, "Bea");
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	note(&mut desk, space, "Desk lamp", at + 100, &mut rng).expect("adds, not knowing");
	at = sync_rounds(&mut desk, &mut ada, at + 1_000, 2);
	at = sync_rounds(bea, &mut ada, at, 1);
	assert_eq!(found(&ada, space, "lamp"), Vec::<String>::new());
	for library in [&*bea, &desk] {
		assert!(library.has_left(space).expect("reads"));
		assert!(!holds_key(library, space, writing_key(&ada, space)));
	}
	// Ada writes on, and syncs with neither of Bea's replicas anything they would read.
	note(&mut ada, space, "Gutters", at, &mut rng).expect("adds");
	sync_rounds(&mut ada, &mut desk, at + 1_000, 1);
	assert_eq!(found(&desk, space, "gutters"), Vec::<String>::new());
}

#[test]
fn two_admins_who_remove_each_other_settle_it_as_each_receives_its_removal() {
	let mut rng = TestRng::new("two admins");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(&[("Bea", MemberRole::Admin)], &mut rng);
	let bea = &mut others[0];
	let (ada_member, bea_member) = (member(&ada, space, "Ada"), member(&ada, space, "Bea"));
	// Offline, each removes the other.
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	bea.remove_member(space, ada_member, at + 100, &mut rng)
		.expect("removes");
	at = sync_rounds(&mut ada, bea, at + 1_000, 3);
	// Ada became an admin first, and stays; both agree.
	for library in [&ada, &*bea] {
		assert_eq!(
			roles(library, space),
			[
				("Ada".to_owned(), MemberRole::Admin),
				("Bea".to_owned(), MemberRole::None)
			]
		);
	}
	assert!(bea.has_left(space).expect("reads"));
	assert!(!ada.has_left(space).expect("reads"));
	note(&mut ada, space, "Gutters", at, &mut rng).expect("adds");
	sync_rounds(&mut ada, bea, at + 1_000, 1);
	assert_eq!(found(bea, space, "gutters"), Vec::<String>::new());
}

#[test]
fn a_key_that_a_member_made_while_being_removed_is_given_out_and_never_written_with() {
	let mut rng = TestRng::new("a key made while being removed");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(
		&[("Bea", MemberRole::Editor), ("Cy", MemberRole::Editor)],
		&mut rng,
	);
	let (bea, cy) = others.split_at_mut(1);
	let (bea, cy) = (&mut bea[0], &mut cy[0]);
	// Cy leaves, and Bea, who hears of it first, gives the space a new key; meanwhile, Ada
	// removes Bea, not knowing of either.
	cy.leave(space, at, &mut rng).expect("leaves");
	at = sync_rounds(cy, bea, at + 1_000, 1);
	let beas_key = writing_key(bea, space);
	assert!(holds_key(bea, space, beas_key));
	let bea_member = member(&ada, space, "Bea");
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	assert!(!holds_key(&ada, space, beas_key));
	// Bea, not knowing, writes with her key.
	note(bea, space, "Bea's shelf", at + 100, &mut rng).expect("adds, not knowing");
	// Bea's key reaches Ada, which never writes with it, as it was made without knowing of
	// Bea's removal; Ada's next key knows of both departures.
	at = sync_rounds(bea, &mut ada, at + 1_000, 2);
	assert!(holds_key(&ada, space, beas_key));
	assert_ne!(writing_key(&ada, space), beas_key);
	// So Ada reads what Bea wrote with it, which waits for her review, and accepts it.
	let set_aside: Vec<_> = ada
		.set_aside_in(space)
		.expect("lists")
		.into_iter()
		.filter(|aside| !aside.governs)
		.collect();
	assert_eq!(set_aside.len(), 1, "{set_aside:?}");
	assert_eq!(set_aside[0].member, Some(bea_member));
	assert!(set_aside[0].acceptable);
	assert_eq!(
		set_aside[0]
			.entities
			.iter()
			.filter_map(|(_, name)| name.clone())
			.collect::<Vec<_>>(),
		["Bea's shelf"]
	);
	let accepted = ada
		.accept_set_aside_in(space, &[set_aside[0].change], at, &mut rng)
		.expect("accepts");
	assert_eq!(accepted, 1);
	assert_eq!(found(&ada, space, "shelf"), ["Bea's shelf"]);
	// Dee joins later, gets the key too, and reads it.
	let mut dee = user_library("Dee", "phone", &mut rng);
	invite_member(&mut ada, (space, MemberRole::Viewer), &mut dee, at + 1_000);
	sync_rounds(&mut ada, &mut dee, at + 2_000, 2);
	assert!(holds_key(&dee, space, beas_key));
	assert_eq!(found(&dee, space, "shelf"), ["Bea's shelf"]);
	assert!(!holds_key(bea, space, writing_key(&ada, space)));
	assert!(!holds_key(cy, space, writing_key(&ada, space)));
}

#[test]
fn a_removed_device_invited_again_comes_back_as_a_new_member() {
	let mut rng = TestRng::new("invited again");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(&[("Bea", MemberRole::Editor)], &mut rng);
	let bea = &mut others[0];
	let old_member = member(&ada, space, "Bea");
	ada.remove_member(space, old_member, at, &mut rng)
		.expect("removes");
	note(&mut ada, space, "Gutters", at + 100, &mut rng).expect("adds");
	at = sync_rounds(&mut ada, bea, at + 1_000, 2);
	assert!(bea.has_left(space).expect("reads"));
	// Invited again from the same device, she is a new member, and reads what was added.
	let new_member = invite_member(&mut ada, (space, MemberRole::Viewer), bea, at);
	assert_ne!(new_member, old_member);
	let at = sync_rounds(&mut ada, bea, at + 1_000, 2);
	assert!(!bea.has_left(space).expect("reads"));
	assert_eq!(found(bea, space, "gutters"), ["Gutters"]);
	let mut members: Vec<(MemberRole, EntityId)> = bea
		.members(space)
		.expect("lists")
		.into_iter()
		.filter(|found| found.name.as_deref() == Some("Bea"))
		.map(|found| (found.role, found.record))
		.collect();
	members.sort();
	assert_eq!(
		members,
		[
			(MemberRole::None, old_member),
			(MemberRole::Viewer, new_member)
		]
	);
	// By name, the member she is now.
	for library in [&ada, &*bea] {
		assert_eq!(member(library, space, "Bea"), new_member);
		assert_eq!(library.find_members(space, "Bea").expect("finds").len(), 2);
	}
	// Her device is the new member's: as that member, she may leave again.
	let you: Vec<EntityId> = bea
		.members(space)
		.expect("lists")
		.into_iter()
		.filter(|found| found.this_user)
		.map(|found| found.record)
		.collect();
	assert_eq!(you, [new_member]);
	assert!(!bea.leave(space, at, &mut rng).expect("leaves again"));
	assert!(bea.has_left(space).expect("reads"));
}

#[test]
fn a_device_that_does_not_know_of_a_leaving_cannot_invite_the_member_back_as_they_were() {
	let mut rng = TestRng::new("invited back too soon");
	let Household {
		mut ada,
		mut others,
		space,
		at,
	} = Household::new(&[("Bea", MemberRole::Editor)], &mut rng);
	let bea = &mut others[0];
	bea.leave(space, at, &mut rng).expect("leaves");
	// Ada's laptop has not heard of it, and offers Bea the member she was: Bea's phone
	// refuses, rather than staying removed.
	let joined = invitation(&mut ada, (space, MemberRole::Editor), bea, at + 1_000);
	assert!(
		matches!(joined, Err(NodeError::InviterBehind)),
		"{joined:?}"
	);
	assert!(bea.has_left(space).expect("reads"));
	// Once they synced, the invitation makes her a new member.
	let at = sync_rounds(bea, &mut ada, at + 2_000, 1);
	let joined = invitation(&mut ada, (space, MemberRole::Editor), bea, at);
	assert!(joined.is_ok(), "{joined:?}");
	assert!(!bea.has_left(space).expect("reads"));
}

/// Invites the user of `newcomer` to `space` of `admin`, as `role`, over connections on
/// localhost, and returns what the joiner's side gave.
fn invitation(
	admin: &mut Library,
	(space, role): (SpaceId, MemberRole),
	newcomer: &mut Library,
	at: u64,
) -> Result<nibkeep_node::JoinedSpace, NodeError> {
	let mut rng = TestRng::new(&format!("inviting at {at}"));
	let code = PairingCode::generate(&mut rng);
	let admin_network = admin
		.network(nibkeep_node::NetworkOptions::LOCAL)
		.expect("starts");
	let joiner_network = newcomer
		.network(nibkeep_node::NetworkOptions::LOCAL)
		.expect("starts");
	joiner_network.remember(
		admin_network.endpoint(),
		&admin_network.local_addresses(),
		Some(&code.invitation_announcement()),
	);
	let mut admin_rng = TestRng::new(&format!("admin inviting at {at}"));
	std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			nibkeep_node::invite(
				admin,
				&admin_network,
				&nibkeep_node::Invitation {
					space,
					role,
					person: None,
				},
				&code,
				&mut |_| true,
				nibkeep_node::Showing::new(std::time::Duration::from_secs(10)),
				&|| at,
				&mut admin_rng,
			)
		});
		let joined = nibkeep_node::join_space(
			newcomer,
			&joiner_network,
			&code,
			std::time::Duration::from_secs(10),
			&|| at,
			&mut rng,
		);
		let _ = invited.join().expect("the admin ran");
		joined
	})
}

#[test]
fn an_admin_reviews_and_accepts_what_a_removed_member_did_without_knowing() {
	let mut rng = TestRng::new("review");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(
		&[("Bea", MemberRole::Editor), ("Cy", MemberRole::Editor)],
		&mut rng,
	);
	let bea_member = member(&ada, space, "Bea");
	ada.remove_member(space, bea_member, at, &mut rng)
		.expect("removes");
	let (bea, cy) = others.split_at_mut(1);
	let (bea, cy) = (&mut bea[0], &mut cy[0]);
	note(bea, space, "Bea's shelf", at + 100, &mut rng).expect("adds, not knowing");
	at = sync_rounds(bea, &mut ada, at + 1_000, 2);
	at = sync_rounds(&mut ada, cy, at, 1);
	let set_aside = ada.set_aside_in(space).expect("lists");
	assert_eq!(set_aside.len(), 1, "{set_aside:?}");
	let aside = &set_aside[0];
	assert_eq!(aside.member, Some(bea_member));
	assert!(aside.acceptable && !aside.governs);
	assert!(aside.operations.is_some_and(|count| count > 0));
	assert!(
		aside
			.entities
			.iter()
			.any(|(_, name)| name.as_deref() == Some("Bea's shelf"))
	);
	// Only an admin accepts.
	let by_editor = cy.accept_set_aside_in(space, &[aside.change], at, &mut rng);
	assert!(
		matches!(by_editor, Err(NodeError::AdminsOnly(_))),
		"{by_editor:?}"
	);
	assert_eq!(
		ada.accept_set_aside_in(space, &[aside.change], at, &mut rng)
			.expect("accepts"),
		1
	);
	assert_eq!(found(&ada, space, "shelf"), ["Bea's shelf"]);
	assert_eq!(ada.set_aside_in(space).expect("lists"), []);
	sync_rounds(&mut ada, cy, at + 1_000, 1);
	assert_eq!(found(cy, space, "shelf"), ["Bea's shelf"]);
}

#[test]
fn acceptance_never_lifts_what_a_role_did_not_allow() {
	let mut rng = TestRng::new("not allowed");
	let Household {
		mut ada,
		mut others,
		space,
		at,
	} = Household::new(&[("Bea", MemberRole::Viewer)], &mut rng);
	let bea = &mut others[0];
	// A viewer adds what only an editor may.
	note(bea, space, "Bea's shelf", at, &mut rng).expect("writes it all the same");
	let at = sync_rounds(bea, &mut ada, at + 1_000, 1);
	let set_aside = ada.set_aside_in(space).expect("lists");
	assert_eq!(set_aside.len(), 1, "{set_aside:?}");
	assert!(!set_aside[0].acceptable);
	assert_eq!(
		ada.accept_set_aside_in(space, &[set_aside[0].change], at, &mut rng)
			.expect("accepts nothing"),
		0
	);
	assert_eq!(found(&ada, space, "shelf"), Vec::<String>::new());
}

#[test]
fn a_removed_members_recovery_code_is_given_no_key_made_after() {
	let mut rng = TestRng::new("a removed member's code");
	let Household {
		mut ada,
		mut others,
		space,
		mut at,
	} = Household::new(
		&[("Bea", MemberRole::Editor), ("Cy", MemberRole::Viewer)],
		&mut rng,
	);
	let bea_member = member(&ada, space, "Bea");
	let cy_member = member(&ada, space, "Cy");
	// Bea's device names a code of hers in the space, as a later version does by itself.
	let code = nibkeep_crypto::RecoveryCode::generate(&mut rng);
	let stated = nibkeep_governance::RecoveryStatement::sign(&code, bea_member);
	let operation = nibkeep_governance::state_recovery_code(space, &stated, &mut rng);
	let bea = others.remove(0);
	let folder = bea.directory().to_owned();
	let directory = folder.ancestors().nth(2).expect("a library").to_owned();
	drop(bea);
	let nibkeep_node::NewProtection::Passphrase(passphrase, _) = common::protection() else {
		unreachable!("a passphrase");
	};
	let unlocker = nibkeep_node::Unlocker::Passphrase(passphrase);
	at += 1_000;
	common::forge(
		&directory,
		&unlocker,
		(space, vec![operation]),
		(None, at),
		&mut rng,
	);
	let mut bea = Library::open(&directory, &unlocker).expect("opens");
	at = sync_rounds(&mut ada, &mut bea, at, 2);
	let keys_of = |ada: &Library| {
		let check = ada.check_recovery_code(&code).expect("checks");
		// The code is Bea's, and none of Ada's own.
		assert_eq!(check.code, None);
		let [keys] = check.spaces.as_slice() else {
			panic!("the space names it: {:?}", check.spaces);
		};
		assert_eq!(keys.space, space);
		(keys.opens.clone(), keys.lacks.clone())
	};
	// While Bea is a member, a new key is her code's too: Ada removes Cy.
	ada.remove_member(space, cy_member, at, &mut rng)
		.expect("removes Cy");
	let (opens, lacks) = keys_of(&ada);
	assert_eq!((opens.len(), lacks.len()), (1, 0), "{opens:?} {lacks:?}");
	// Once Bea is removed, the key that her removal makes is not.
	ada.remove_member(space, bea_member, at + 1_000, &mut rng)
		.expect("removes Bea");
	let (still, lacks) = keys_of(&ada);
	assert_eq!(still, opens);
	assert_eq!(lacks.len(), 1, "{lacks:?}");
}
