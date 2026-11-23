//! Invitations to a shared space ([technical design: sharing]): an admin invites with a one-time
//! code and confirms who asks; the new member's replica syncs the space with other users'
//! replicas, and their changes count by their role on every device; the member's other
//! replicas get the space through their personal space; and an invitation takes three tries,
//! expires, and adds nobody when either side refuses.
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use common::{TIMEOUT, now, pair_named, sync_in_memory};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{Name, TypeName, Value};
use nibkeep_node::{
	Edit, Invitation, Invited, Invitee, JoinedSpace, Library, MemberRole, NetworkOptions,
	NewLibrary, NewProtection, NodeError, PairingCode, Reason, Showing, SyncError, invite,
	invite_closed, join_space,
};
use nibkeep_testkit::TestRng;

/// A name for a folder of this file's tests, which run at once, that no other takes.
fn folder_name(name: &str) -> String {
	static NEXT: AtomicUsize = AtomicUsize::new(0);
	format!(
		"invitations {} {name}",
		NEXT.fetch_add(1, Ordering::Relaxed)
	)
}

/// A new library of a user named `user`, on a device named `device`.
fn user_library(user: &str, device: &str, rng: &mut TestRng) -> Library {
	let directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&folder_name(&format!("{user} {device}")),
	);
	Library::create(
		&directory,
		&NewLibrary::new(user, device, "nib"),
		&NewProtection::Passphrase(
			"correct horse",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		),
		now(),
		rng,
	)
	.expect("creates")
}

/// A new folder's place, where nothing is yet.
fn new_folder(name: &str) -> std::path::PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &folder_name(name));
	std::fs::remove_dir(&made).expect("removes");
	made
}

/// Invites the user of `joiner` to `space` of `admin`, as `role`, over connections on
/// localhost: the admin shows `shown`, the joiner types `typed`, and `confirm` answers who
/// asks.
fn invite_with(
	admin: &mut Library,
	(space, role): (SpaceId, MemberRole),
	newcomer: &mut Library,
	(shown, typed): (&PairingCode, &PairingCode),
	confirm: &mut (dyn FnMut(&Invitee) -> bool + Send),
	at: u64,
) -> (Result<Invited, NodeError>, Result<JoinedSpace, NodeError>) {
	let admin_network = admin.network(NetworkOptions::LOCAL).expect("starts");
	let joiner_network = newcomer.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		admin_network.endpoint(),
		&admin_network.local_addresses(),
		Some(&shown.invitation_announcement()),
	);
	let mut admin_rng = TestRng::new(&format!("admin at {at}"));
	let mut joiner_rng = TestRng::new(&format!("joiner at {at}"));
	std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			invite(
				admin,
				&admin_network,
				&Invitation {
					space,
					role,
					person: None,
				},
				shown,
				confirm,
				nibkeep_node::Showing::new(TIMEOUT),
				&|| at,
				&mut admin_rng,
			)
		});
		let joined = join_space(
			newcomer,
			&joiner_network,
			typed,
			TIMEOUT,
			&|| at,
			&mut joiner_rng,
		);
		(invited.join().expect("the admin ran"), joined)
	})
}

/// Invites someone to `space` of `library`, as `role`, with nobody to join.
fn invite_alone(
	library: &mut Library,
	(space, role): (SpaceId, MemberRole),
	at: u64,
	rng: &mut TestRng,
) -> Result<Invited, NodeError> {
	let network = library.network(NetworkOptions::LOCAL).expect("starts");
	invite(
		library,
		&network,
		&Invitation {
			space,
			role,
			person: None,
		},
		&PairingCode::generate(rng),
		&mut |_| true,
		nibkeep_node::Showing::new(Duration::from_secs(1)),
		&|| at,
		rng,
	)
}

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

/// Adds a note named `name` to `space`, and returns it.
fn note(library: &mut Library, space: SpaceId, name: &str, at: u64, rng: &mut TestRng) -> EntityId {
	library
		.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(
				"org.nibkeep.name".parse().expect("a field"),
				Value::Name(Name::new(name)),
			)],
			at,
			rng,
		)
		.expect("adds")
}

/// Renames an entity.
fn rename(
	library: &mut Library,
	(space, entity): (SpaceId, EntityId),
	name: &str,
	at: u64,
	rng: &mut TestRng,
) {
	library
		.edit_entity(
			space,
			entity,
			vec![Edit::Set(
				"org.nibkeep.name".parse().expect("a field"),
				vec![Value::Name(Name::new(name))],
			)],
			&common::today(),
			at,
			rng,
		)
		.expect("saves the change");
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

/// Checks that Bea's phone holds the space that Ada invited her to, with what Ada wrote; that
/// both list both members, and each syncs with the other's replica; and that the personal
/// spaces stay apart.
fn both_hold_it(ada: &Library, bea: &Library, space: SpaceId) {
	// Bea's phone holds the space, with what Ada wrote, and both list both members.
	assert_eq!(bea.spaces(), vec![bea.personal_space(), space]);
	assert_eq!(found(bea, space, "boiler"), ["Boiler service"]);
	let expected = vec![
		("Ada".to_owned(), MemberRole::Admin),
		("Bea".to_owned(), MemberRole::Editor),
	];
	assert_eq!(roles(ada, space), expected);
	assert_eq!(roles(bea, space), expected);
	let own = |library: &Library| -> Vec<String> {
		library
			.members(space)
			.expect("lists")
			.into_iter()
			.filter(|member| member.this_user)
			.filter_map(|member| member.name)
			.collect()
	};
	assert_eq!(own(ada), ["Ada"]);
	assert_eq!(own(bea), ["Bea"]);
	// Each syncs with the other's replica, where it connects from.
	let endpoints = |library: &Library| -> Vec<[u8; 32]> {
		library
			.peers()
			.expect("lists")
			.into_iter()
			.filter_map(|peer| peer.endpoint)
			.collect()
	};
	assert_eq!(endpoints(ada), [bea.endpoint()]);
	assert_eq!(endpoints(bea), [ada.endpoint()]);
	// The personal spaces stay apart.
	assert_eq!(
		found(bea, bea.personal_space(), "boiler"),
		Vec::<String>::new()
	);
}

/// Ada's household, with a note in it, and Bea's library, with Bea invited as `role`.
struct Household {
	ada: Library,
	bea: Library,
	space: SpaceId,
	bea_member: EntityId,
	at: u64,
}

impl Household {
	fn new(role: MemberRole, rng: &mut TestRng) -> Self {
		let mut ada = user_library("Ada", "laptop", rng);
		let mut bea = user_library("Bea", "phone", rng);
		let at = now() + 100_000;
		let space = ada.create_space("Household", at, rng).expect("makes it");
		note(&mut ada, space, "Boiler service", at + 500, rng);
		let code = PairingCode::generate(rng);
		let (invited, joined) = invite_with(
			&mut ada,
			(space, role),
			&mut bea,
			(&code, &code),
			&mut |_| true,
			at + 1_000,
		);
		let invited = invited.expect("Ada invited");
		let joined = joined.expect("Bea joined");
		assert_eq!(joined.space, space);
		assert_eq!(joined.member, invited.member);
		Self {
			ada,
			bea,
			space,
			bea_member: invited.member,
			at: at + 2_000,
		}
	}
}

#[test]
fn an_invited_member_syncs_the_space_and_their_changes_count_by_their_role() {
	let mut rng = TestRng::new("an invited member");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let mut at = now() + 100_000;
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let boiler = note(&mut ada, space, "Boiler service", at + 500, &mut rng);
	assert!(
		ada.suggests_second_admin(space).expect("reads"),
		"a shared space with one admin suggests a second"
	);
	assert!(
		!ada.suggests_second_admin(ada.personal_space())
			.expect("reads"),
		"a personal space never suggests one"
	);

	// Ada's laptop shows who asks, and Ada confirms.
	let code = PairingCode::generate(&mut rng);
	let mut asked = Vec::new();
	let (invited, joined) = invite_with(
		&mut ada,
		(space, MemberRole::Editor),
		&mut bea,
		(&code, &code),
		&mut |invitee| {
			asked.push(invitee.clone());
			true
		},
		at + 1_000,
	);
	let invited = invited.expect("Ada invited");
	let joined = joined.expect("Bea joined");
	assert_eq!(asked.len(), 1);
	assert_eq!(asked[0].user_name, "Bea");
	assert_eq!(asked[0].device, "phone");
	assert_eq!(asked[0].app, "nib");
	assert_eq!(asked[0].same_user, Vec::new());
	assert!(!asked[0].was_member);
	assert_eq!(invited.user_name, "Bea");
	assert_eq!(joined.space, space);
	assert_eq!(joined.name, "Household");
	assert_eq!(joined.role, MemberRole::Editor);
	assert_eq!(joined.member, invited.member);
	at += 2_000;

	both_hold_it(&ada, &bea, space);

	// As an editor, Bea renames Ada's note, and adds one; both reach Ada.
	rename(
		&mut bea,
		(space, boiler),
		"Boiler and radiators",
		at,
		&mut rng,
	);
	note(&mut bea, space, "Window cleaner", at + 100, &mut rng);
	at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	assert_eq!(found(&ada, space, "boiler"), ["Boiler and radiators"]);
	assert_eq!(found(&ada, space, "window"), ["Window cleaner"]);

	// Ada makes Bea a viewer: what Bea changes then, or adds, counts on neither device, even
	// the note Bea added, which is no record of Bea's own.
	ada.give_role(space, invited.member, MemberRole::Viewer, at, &mut rng)
		.expect("gives the role");
	at = sync_rounds(&mut ada, &mut bea, at + 1_000, 1);
	assert_eq!(
		roles(&bea, space)[1],
		("Bea".to_owned(), MemberRole::Viewer)
	);
	rename(&mut bea, (space, boiler), "Bea's boiler", at, &mut rng);
	let window = bea.find_entity(space, "Window cleaner").expect("finds");
	rename(
		&mut bea,
		(space, window),
		"Window cleaner, monthly",
		at + 100,
		&mut rng,
	);
	note(&mut bea, space, "Chimney sweep", at + 200, &mut rng);
	at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	for library in [&ada, &bea] {
		assert_eq!(found(library, space, "boiler"), ["Boiler and radiators"]);
		assert_eq!(found(library, space, "window"), ["Window cleaner"]);
		assert_eq!(found(library, space, "chimney"), Vec::<String>::new());
	}

	// Ada makes Bea an admin too, and the space suggests no second admin then.
	ada.give_role(space, invited.member, MemberRole::Admin, at, &mut rng)
		.expect("gives the role");
	assert!(!ada.suggests_second_admin(space).expect("reads"));
	sync_rounds(&mut ada, &mut bea, at + 1_000, 1);
	assert!(!bea.suggests_second_admin(space).expect("reads"));
}

#[test]
fn only_an_admin_invites_gives_roles_and_removes() {
	let mut rng = TestRng::new("only an admin invites");
	let Household {
		mut ada,
		mut bea,
		space,
		bea_member: _,
		at,
	} = Household::new(MemberRole::Editor, &mut rng);
	// Refused before any code is shown, so that nobody can join.
	let invited = invite_alone(&mut bea, (space, MemberRole::Viewer), at, &mut rng);
	assert!(
		matches!(invited, Err(NodeError::AdminsOnly(_))),
		"{invited:?}"
	);
	let ada_member = ada
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|member| member.this_user)
		.expect("Ada is a member")
		.record;
	let given = bea.give_role(space, ada_member, MemberRole::Viewer, at, &mut rng);
	assert!(matches!(given, Err(NodeError::AdminsOnly(_))), "{given:?}");
	// The space keeps an admin.
	let lowered = ada.give_role(space, ada_member, MemberRole::Editor, at, &mut rng);
	assert!(matches!(lowered, Err(NodeError::LastAdmin)), "{lowered:?}");
	// Only an admin removes; an invitation gives a role.
	let removed = bea.remove_member(space, ada_member, at, &mut rng);
	assert!(
		matches!(removed, Err(NodeError::AdminsOnly(_))),
		"{removed:?}"
	);
	let removed = bea.give_role(space, ada_member, MemberRole::None, at, &mut rng);
	assert!(
		matches!(removed, Err(NodeError::AdminsOnly(_))),
		"{removed:?}"
	);
	let invited = invite_alone(&mut ada, (space, MemberRole::None), at, &mut rng);
	assert!(matches!(invited, Err(NodeError::NoRole)), "{invited:?}");
	// The personal space has no members to invite.
	let personal = ada.personal_space();
	let invited = invite_alone(&mut ada, (personal, MemberRole::Editor), at, &mut rng);
	assert!(
		matches!(invited, Err(NodeError::NeverShared)),
		"{invited:?}"
	);
	assert!(matches!(ada.members(personal), Err(NodeError::NeverShared)));
	assert_eq!(roles(&ada, space), roles(&bea, space));
}

/// What answers who asks to join, from the admin's thread.
type Confirm<'a> = &'a mut (dyn FnMut(&Invitee) -> bool + Send);

/// What is told of each try that failed before the code matched, from the admin's thread.
type TellWrongTry<'a> = &'a (dyn Fn(&NodeError, usize) + Sync);

/// Invites the user of `newcomer` to `space` of `admin`, as an editor, over connections on
/// localhost: the admin shows `shown`, telling `wrong_try` of each try that fails before the
/// code matches, while the joiner types each of `typed` in turn, and `confirm` answers who
/// asks.
fn invite_tried(
	(admin, space): (&mut Library, SpaceId),
	newcomer: &mut Library,
	(shown, typed): (&PairingCode, &[&PairingCode]),
	(confirm, wrong_try): (Confirm<'_>, TellWrongTry<'_>),
	at: u64,
) -> (
	Result<Invited, NodeError>,
	Vec<Result<JoinedSpace, NodeError>>,
) {
	let admin_network = admin.network(NetworkOptions::LOCAL).expect("starts");
	let mut admin_rng = TestRng::new(&format!("admin tried at {at}"));
	let mut joiner_rng = TestRng::new(&format!("joiner tried at {at}"));
	std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			invite(
				admin,
				&admin_network,
				&Invitation {
					space,
					role: MemberRole::Editor,
					person: None,
				},
				shown,
				confirm,
				Showing {
					timeout: TIMEOUT,
					wrong_try,
				},
				&|| at,
				&mut admin_rng,
			)
		});
		let joined = typed
			.iter()
			.map(|typed| {
				let joiner_network = newcomer.network(NetworkOptions::LOCAL).expect("starts");
				joiner_network.remember(
					admin_network.endpoint(),
					&admin_network.local_addresses(),
					Some(&shown.invitation_announcement()),
				);
				join_space(
					newcomer,
					&joiner_network,
					typed,
					TIMEOUT,
					&|| at,
					&mut joiner_rng,
				)
			})
			.collect();
		(invited.join().expect("the admin ran"), joined)
	})
}

/// A code with the first two digits of `shown`, and the rest wrong.
fn mistyped(shown: &PairingCode) -> PairingCode {
	let digits = shown.to_string();
	let wrong = if digits.ends_with("000") {
		"111"
	} else {
		"000"
	};
	format!("{}{wrong}", &digits[..digits.len() - 3])
		.parse()
		.expect("a code")
}

#[test]
fn an_invitation_takes_three_tries_and_expires() {
	let mut rng = TestRng::new("an invitation takes three tries");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");

	// Three wrong codes spend the invitation, and add nobody.
	let shown = PairingCode::generate(&mut rng);
	let typed = mistyped(&shown);
	let mut asked = 0;
	let heard = Mutex::new(Vec::new());
	let (invited, joined) = invite_tried(
		(&mut ada, space),
		&mut bea,
		(&shown, &[&typed, &typed, &typed]),
		(
			&mut |_| {
				asked += 1;
				true
			},
			&|error: &NodeError, left| {
				let wrong = error.is_wrong_code();
				heard.lock().expect("not poisoned").push((wrong, left));
			},
		),
		at + 1_000,
	);
	assert!(
		matches!(invited, Err(NodeError::Sync(SyncError::WrongCode))),
		"{invited:?}"
	);
	for joined in joined {
		assert!(
			matches!(joined, Err(NodeError::Sync(SyncError::WrongCode))),
			"{joined:?}"
		);
	}
	assert_eq!(asked, 0, "nobody is asked about a wrong code");
	assert_eq!(
		heard.into_inner().expect("not poisoned"),
		[(true, 2), (true, 1)]
	);
	assert_eq!(roles(&ada, space).len(), 1);
	assert_eq!(bea.spaces(), vec![bea.personal_space()]);

	// The code is spent: the admin no longer announces it.
	let bea_network = bea.network(NetworkOptions::LOCAL).expect("starts");
	let again = join_space(
		&mut bea,
		&bea_network,
		&shown,
		Duration::from_secs(2),
		&|| at + 2_000,
		&mut rng,
	);
	assert!(matches!(again, Err(NodeError::NoSponsor)), "{again:?}");

	// An invitation that nobody takes expires.
	let expired = invite_alone(&mut ada, (space, MemberRole::Editor), at + 3_000, &mut rng);
	assert!(
		matches!(expired, Err(NodeError::InvitationExpired)),
		"{expired:?}"
	);
}

#[test]
fn a_mistyped_invitation_code_is_typed_again() {
	let mut rng = TestRng::new("an invitation typed again");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let shown = PairingCode::generate(&mut rng);
	let mut asked = 0;
	let heard = AtomicUsize::new(0);
	let (invited, mut joined) = invite_tried(
		(&mut ada, space),
		&mut bea,
		(&shown, &[&mistyped(&shown), &shown]),
		(
			&mut |_| {
				asked += 1;
				true
			},
			&|_, left| {
				assert_eq!(left, 2);
				heard.fetch_add(1, Ordering::Relaxed);
			},
		),
		at + 1_000,
	);
	let invited = invited.expect("invites");
	let joined_space = joined.pop().expect("tried twice").expect("joins");
	assert!(
		matches!(
			joined.pop(),
			Some(Err(NodeError::Sync(SyncError::WrongCode)))
		),
		"the first try had a wrong code"
	);
	assert_eq!(joined_space.member, invited.member);
	assert_eq!((asked, heard.into_inner()), (1, 1));
	assert_eq!(roles(&ada, space).len(), 2);
}

#[test]
fn a_refusal_on_either_side_adds_nobody() {
	let mut rng = TestRng::new("a refusal adds nobody");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");

	// Ada declines whoever asks.
	let code = PairingCode::generate(&mut rng);
	let (invited, joined) = invite_with(
		&mut ada,
		(space, MemberRole::Editor),
		&mut bea,
		(&code, &code),
		&mut |_| false,
		at + 1_000,
	);
	assert!(matches!(invited, Err(NodeError::Declined)), "{invited:?}");
	assert!(
		matches!(
			joined,
			Err(NodeError::Sync(SyncError::Ended(Reason::Declined)))
		),
		"{joined:?}"
	);
	assert_eq!(roles(&ada, space).len(), 1);
	assert_eq!(bea.spaces(), vec![bea.personal_space()]);

	// Ada's own desk holds the space already, and refuses to join it as another member.
	let mut desk = {
		let code = PairingCode::generate(&mut rng);
		let (paired, joined) = pair_named(
			&mut ada,
			&new_folder("Ada desk"),
			"desk",
			(&code, &code),
			&mut rng,
		);
		paired.expect("pairs");
		joined.expect("joins").library
	};
	sync_rounds(&mut ada, &mut desk, at + 70_000, 4);
	assert!(desk.spaces().contains(&space));
	let code = PairingCode::generate(&mut rng);
	let (invited, joined) = invite_with(
		&mut ada,
		(space, MemberRole::Viewer),
		&mut desk,
		(&code, &code),
		&mut |_| true,
		at + 80_000,
	);
	assert!(
		matches!(joined, Err(NodeError::HoldsSpace(held)) if held == space),
		"{joined:?}"
	);
	assert!(
		matches!(
			invited,
			Err(NodeError::Sync(SyncError::Ended(Reason::HoldsSpace)))
		),
		"{invited:?}"
	);
	assert_eq!(roles(&ada, space), [("Ada".to_owned(), MemberRole::Admin)]);
}

#[test]
fn the_members_other_replicas_get_the_space_through_their_personal_space() {
	let mut rng = TestRng::new("the member's other replicas");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let mut desk = {
		let code = PairingCode::generate(&mut rng);
		let (paired, joined) = pair_named(
			&mut bea,
			&new_folder("Bea desk"),
			"desk",
			(&code, &code),
			&mut rng,
		);
		paired.expect("pairs");
		joined.expect("joins").library
	};
	let mut at = sync_rounds(&mut bea, &mut desk, now() + 70_000, 1);
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	note(&mut ada, space, "Boiler service", at + 500, &mut rng);
	let code = PairingCode::generate(&mut rng);
	let (invited, joined) = invite_with(
		&mut ada,
		(space, MemberRole::Editor),
		&mut bea,
		(&code, &code),
		&mut |_| true,
		at + 1_000,
	);
	invited.expect("Ada invited");
	joined.expect("Bea joined");

	// Bea's desk learns of the space through Bea's personal space, and Bea's phone lists it.
	at = sync_rounds(&mut bea, &mut desk, at + 2_000, 4);
	assert_eq!(desk.spaces(), vec![desk.personal_space(), space]);
	assert_eq!(found(&desk, space, "boiler"), ["Boiler service"]);
	let replicas = |library: &Library| -> Vec<usize> {
		let mut counts: Vec<usize> = library
			.members(space)
			.expect("lists")
			.into_iter()
			.map(|member| member.replicas.len())
			.collect();
		counts.sort_unstable();
		counts
	};
	assert_eq!(replicas(&desk), [1, 2]);

	// Ada's laptop learns of the desk from Bea's phone, and then syncs with the desk itself:
	// what the desk adds counts there.
	at = sync_rounds(&mut bea, &mut ada, at, 1);
	assert_eq!(replicas(&ada), [1, 2]);
	assert!(
		ada.peers()
			.expect("lists")
			.iter()
			.any(|peer| peer.endpoint == Some(desk.endpoint()))
	);
	note(&mut desk, space, "Gutters", at, &mut rng);
	sync_rounds(&mut desk, &mut ada, at + 1_000, 1);
	assert_eq!(found(&ada, space, "gutters"), ["Gutters"]);
	// Bea's own replicas stay out of Ada's personal space.
	assert_eq!(ada.replicas().expect("lists").len(), 1);
}

#[test]
fn two_records_with_one_user_id_are_two_members_and_the_admin_is_told() {
	let mut rng = TestRng::new("one user, two members");
	let Household {
		mut ada,
		mut bea,
		space,
		bea_member,
		at,
	} = Household::new(MemberRole::Viewer, &mut rng);
	// Bea's desk, paired without the space, is invited on its own.
	let mut desk = {
		let code = PairingCode::generate(&mut rng);
		let joining = nibkeep_node::Joining::new("desk", "nib", &mut rng).expect("names");
		let (paired, joined) = common::pair_joining_as(
			&mut bea,
			&new_folder("Bea desk, again"),
			(
				joining,
				false,
				&nibkeep_node::NewDevice::computer().with_spaces(Vec::new()),
			),
			(&code, &code),
			&mut rng,
		);
		paired.expect("pairs");
		joined.expect("joins").library
	};
	assert_eq!(desk.spaces(), vec![desk.personal_space()]);
	let code = PairingCode::generate(&mut rng);
	let mut asked = Vec::new();
	let (invited, joined) = invite_with(
		&mut ada,
		(space, MemberRole::Editor),
		&mut desk,
		(&code, &code),
		&mut |invitee| {
			asked.push(invitee.clone());
			true
		},
		at + 70_000,
	);
	let invited = invited.expect("Ada invited");
	joined.expect("the desk joined");
	// Ada was told that a member is the same user, on Bea's phone.
	assert_eq!(asked.len(), 1);
	let same = &asked[0].same_user;
	assert_eq!(same.len(), 1, "{same:?}");
	assert_eq!(same[0].record, bea_member);
	assert_eq!(
		same[0]
			.replicas
			.iter()
			.filter_map(|replica| replica.device.as_deref())
			.collect::<Vec<_>>(),
		["phone"]
	);
	// Two members, each with a role of their own, both of Bea's user.
	assert_ne!(invited.member, bea_member);
	let members = ada.members(space).expect("lists");
	let bea_user = members
		.iter()
		.find(|member| member.record == bea_member)
		.and_then(|member| member.user);
	assert!(bea_user.is_some());
	let of_bea: Vec<MemberRole> = members
		.iter()
		.filter(|member| member.user == bea_user)
		.map(|member| member.role)
		.collect();
	assert_eq!(of_bea.len(), 2);
	assert!(of_bea.contains(&MemberRole::Viewer));
	assert!(of_bea.contains(&MemberRole::Editor));
}

/// A joiner that signs a listing other than the one offered, as for another member.
struct Forger {
	signing: nibkeep_crypto::SigningKey,
	decryption: nibkeep_crypto::DecryptionKey,
	owner: EntityId,
}

impl nibkeep_sync::Accepting for Forger {
	type Error = NodeError;

	fn listing(&mut self, offer: &nibkeep_governance::Offer) -> Result<Option<String>, NodeError> {
		Ok(Some(
			nibkeep_governance::Listing::sign(
				&self.signing,
				&self.decryption.encryption_key(),
				offer.space,
				(self.owner, false),
			)
			.to_text(),
		))
	}

	fn take<R: nibkeep_crypto::rand_core::CryptoRng>(
		&mut self,
		_: &nibkeep_governance::Offer,
		_: &nibkeep_governance::InvitationGrant,
		_: &mut R,
	) -> Result<(), NodeError> {
		Ok(())
	}
}

#[test]
fn a_joiner_whose_listing_is_not_the_one_offered_is_given_nothing() {
	let mut rng = TestRng::new("a forged listing");
	let Household {
		mut ada, space, at, ..
	} = Household::new(MemberRole::Viewer, &mut rng);
	let ada_member = ada
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|member| member.this_user)
		.expect("Ada is a member")
		.record;
	// A device that asks to join, and then signs its listing for Ada's member, to take on
	// her role.
	let signing = nibkeep_crypto::SigningKey::generate(&mut rng);
	let decryption = nibkeep_crypto::DecryptionKey::generate(&mut rng);
	let request = nibkeep_governance::InvitationRequest {
		user: EntityId::random(&mut rng),
		user_name: "Mallory".to_owned(),
		signing: signing.verifying_key(),
		encryption: decryption.encryption_key(),
		device: "laptop".to_owned(),
		app: "nib".to_owned(),
	};
	let mut forger = Forger {
		signing,
		decryption,
		owner: ada_member,
	};
	let code = PairingCode::generate(&mut rng);
	let ada_network = ada.network(NetworkOptions::LOCAL).expect("starts");
	let forger_network =
		nibkeep_node::Network::start(&[7; 32], NetworkOptions::LOCAL).expect("starts");
	forger_network.remember(
		ada_network.endpoint(),
		&ada_network.local_addresses(),
		Some(&code.invitation_announcement()),
	);
	let mut ada_rng = TestRng::new("Ada invites");
	let (invited, accepted) = std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			invite(
				&mut ada,
				&ada_network,
				&Invitation {
					space,
					role: MemberRole::Viewer,
					person: None,
				},
				&code,
				&mut |_| true,
				nibkeep_node::Showing::new(TIMEOUT),
				&|| at,
				&mut ada_rng,
			)
		});
		let mut connection = forger_network
			.connect(
				ada_network.endpoint(),
				nibkeep_net::Protocol::Invite,
				TIMEOUT,
			)
			.expect("connects");
		let accepted =
			nibkeep_sync::accept(&mut connection, &request, &mut forger, &code, &mut rng);
		connection.close();
		(invited.join().expect("Ada ran"), accepted)
	});
	assert!(invited.is_err(), "{invited:?}");
	assert!(
		matches!(accepted, Err(SyncError::Ended(Reason::Failed))),
		"{accepted:?}"
	);
	assert_eq!(
		roles(&ada, space),
		[
			("Ada".to_owned(), MemberRole::Admin),
			("Bea".to_owned(), MemberRole::Viewer)
		]
	);
}

/// A joiner that signs its listing as offered, takes nothing, and whose first sync breaks off
/// at once.
struct Breaking {
	signing: nibkeep_crypto::SigningKey,
	decryption: nibkeep_crypto::DecryptionKey,
	user: EntityId,
}

impl nibkeep_sync::Accepting for Breaking {
	type Error = NodeError;

	fn listing(&mut self, offer: &nibkeep_governance::Offer) -> Result<Option<String>, NodeError> {
		Ok(Some(
			nibkeep_governance::Listing::sign(
				&self.signing,
				&self.decryption.encryption_key(),
				offer.space,
				(offer.member, false),
			)
			.to_text(),
		))
	}

	fn take<R: nibkeep_crypto::rand_core::CryptoRng>(
		&mut self,
		_: &nibkeep_governance::Offer,
		_: &nibkeep_governance::InvitationGrant,
		_: &mut R,
	) -> Result<(), NodeError> {
		Ok(())
	}
}

impl Breaking {
	/// Joins `space` of `admin` as `role`, and breaks off the first sync.
	fn join(
		&mut self,
		admin: &mut Library,
		(space, role): (SpaceId, MemberRole),
		at: u64,
		rng: &mut TestRng,
	) -> (
		Result<Invited, NodeError>,
		Result<nibkeep_sync::Invitation, SyncError>,
	) {
		let request = nibkeep_governance::InvitationRequest {
			user: self.user,
			user_name: "Bea".to_owned(),
			signing: self.signing.verifying_key(),
			encryption: self.decryption.encryption_key(),
			device: "phone".to_owned(),
			app: "nib".to_owned(),
		};
		let code = PairingCode::generate(rng);
		let admin_network = admin.network(NetworkOptions::LOCAL).expect("starts");
		let network =
			nibkeep_node::Network::start(&[11; 32], NetworkOptions::LOCAL).expect("starts");
		network.remember(
			admin_network.endpoint(),
			&admin_network.local_addresses(),
			Some(&code.invitation_announcement()),
		);
		let mut admin_rng = TestRng::new(&format!("Ada invites at {at}"));
		std::thread::scope(|scope| {
			let invited = scope.spawn(|| {
				invite(
					admin,
					&admin_network,
					&Invitation {
						space,
						role,
						person: None,
					},
					&code,
					&mut |_| true,
					nibkeep_node::Showing::new(TIMEOUT),
					&|| at,
					&mut admin_rng,
				)
			});
			let mut connection = network
				.connect(
					admin_network.endpoint(),
					nibkeep_net::Protocol::Invite,
					TIMEOUT,
				)
				.expect("connects");
			let accepted = nibkeep_sync::accept(&mut connection, &request, self, &code, rng);
			connection.close();
			if accepted.is_ok() {
				network
					.connect(
						admin_network.endpoint(),
						nibkeep_net::Protocol::Sync,
						TIMEOUT,
					)
					.expect("connects")
					.close();
			}
			(invited.join().expect("Ada ran"), accepted)
		})
	}
}

#[test]
fn a_device_whose_first_sync_failed_comes_back_as_the_same_member() {
	let mut rng = TestRng::new("invited again");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let at = now() + 100_000;
	let space = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let mut bea = Breaking {
		signing: nibkeep_crypto::SigningKey::generate(&mut rng),
		decryption: nibkeep_crypto::DecryptionKey::generate(&mut rng),
		user: EntityId::random(&mut rng),
	};
	let (invited, accepted) = bea.join(&mut ada, (space, MemberRole::Viewer), at, &mut rng);
	assert!(
		matches!(invited, Err(NodeError::InvitationFailed(_))),
		"{invited:?}"
	);
	let first = accepted.expect("Bea took the space").offer.member;
	// The member stays, as the joiner may hold the space: both sides agree whichever saw the
	// sync fail.
	assert_eq!(
		roles(&ada, space),
		[
			("Ada".to_owned(), MemberRole::Admin),
			("Bea".to_owned(), MemberRole::Viewer)
		]
	);

	// Invited again, the device comes back as the same member, with the role given now.
	let (invited, accepted) = bea.join(&mut ada, (space, MemberRole::Editor), at + 1_000, &mut rng);
	assert!(
		matches!(invited, Err(NodeError::InvitationFailed(_))),
		"{invited:?}"
	);
	assert_eq!(accepted.expect("Bea took the space").offer.member, first);
	assert_eq!(
		roles(&ada, space),
		[
			("Ada".to_owned(), MemberRole::Admin),
			("Bea".to_owned(), MemberRole::Editor)
		]
	);
}

/// Who history says wrote the name of an entity last.
fn named_by(library: &Library, (space, entity): (SpaceId, EntityId)) -> Option<String> {
	let field: nibkeep_model::FieldName = "org.nibkeep.name".parse().expect("a field");
	let history = library
		.history(space, entity, Some(&field))
		.expect("reads")
		.expect("an entity");
	history.fields[0]
		.values
		.iter()
		.max_by_key(|value| value.recorded)
		.and_then(|value| value.author.as_ref())
		.and_then(|author| author.user.name.clone())
}

/// What a space says of a member's link to a person: the person it links them to, if the
/// admin's statement and the member's own agree, and each of the two.
fn link_of(library: &Library, space: SpaceId, member: EntityId) -> nibkeep_node::PersonLink {
	library
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|found| found.record == member)
		.expect("a member")
		.person
}

/// Ada invites Bea to `space` as an editor, naming her `person` there: Bea's member, and
/// what her library joined.
fn invite_as(
	ada: &mut Library,
	bea: &mut Library,
	(space, person): (SpaceId, EntityId),
	at: u64,
	rng: &mut TestRng,
) -> (EntityId, JoinedSpace) {
	let code = PairingCode::generate(rng);
	let admin_network = ada.network(NetworkOptions::LOCAL).expect("starts");
	let joiner_network = bea.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		admin_network.endpoint(),
		&admin_network.local_addresses(),
		Some(&code.invitation_announcement()),
	);
	let mut admin_rng = TestRng::new("Ada invites Olga");
	let (invited, joined) = std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			invite(
				ada,
				&admin_network,
				&Invitation {
					space,
					role: MemberRole::Editor,
					person: Some(person),
				},
				&code,
				&mut |_| true,
				nibkeep_node::Showing::new(TIMEOUT),
				&|| at,
				&mut admin_rng,
			)
		});
		let joined = join_space(bea, &joiner_network, &code, TIMEOUT, &|| at, rng);
		(invited.join().expect("Ada ran"), joined)
	});
	drop((admin_network, joiner_network));
	let member = invited.expect("Ada invited").member;
	let joined = joined.expect("Bea joined");
	(member, joined)
}

#[test]
fn a_member_is_linked_to_a_person_only_when_an_admin_and_they_agree() {
	let mut rng = TestRng::new("links to people");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let mut at = now() + 100_000;
	let space = ada.create_space("Family", at, &mut rng).expect("makes it");
	let olga = note(&mut ada, space, "Olga", at + 100, &mut rng);
	let grandma = note(&mut ada, space, "Grandma", at + 200, &mut rng);

	// Ada invites Bea as Olga, of the family tree; Bea is told, and agrees after joining.
	let (member, joined) = invite_as(&mut ada, &mut bea, (space, olga), at + 1_000, &mut rng);
	assert_eq!(joined.person, Some((olga, "Olga".to_owned())));
	at += 2_000;
	// Until Bea agrees, the link does not hold, and history names her as Bea.
	let link = link_of(&bea, space, member);
	assert_eq!(link.by_admin, Some(olga));
	assert_eq!(link.by_member, None);
	assert_eq!(link.holds, None);
	rename(
		&mut bea,
		(space, grandma),
		"Grandma Olga's mother",
		at,
		&mut rng,
	);
	assert_eq!(named_by(&bea, (space, grandma)).as_deref(), Some("Bea"));
	bea.link_member(space, member, Some(olga), at + 100, &mut rng)
		.expect("links");
	at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	for library in [&ada, &bea] {
		assert_eq!(link_of(library, space, member).holds, Some(olga));
		assert_eq!(named_by(library, (space, grandma)).as_deref(), Some("Olga"));
	}

	// Bea alone cannot be someone else in the data: the link holds no more until Ada agrees.
	bea.link_member(space, member, Some(grandma), at, &mut rng)
		.expect("links");
	at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	let link = link_of(&ada, space, member);
	assert_eq!(
		(link.holds, link.by_admin, link.by_member),
		(None, Some(olga), Some(grandma))
	);
	ada.link_member(space, member, Some(grandma), at, &mut rng)
		.expect("links");
	at = sync_rounds(&mut ada, &mut bea, at + 1_000, 1);
	assert_eq!(link_of(&bea, space, member).holds, Some(grandma));

	// Only an admin links another member, and an admin links themselves alone.
	let ada_member = ada
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|found| found.this_user)
		.expect("Ada is a member")
		.record;
	let refused = bea.link_member(space, ada_member, Some(olga), at, &mut rng);
	assert!(
		matches!(refused, Err(NodeError::AdminsOnly(_))),
		"{refused:?}"
	);
	let ada_person = note(&mut ada, space, "Ada Lovelace", at, &mut rng);
	ada.link_member(space, ada_member, Some(ada_person), at + 100, &mut rng)
		.expect("links");
	assert_eq!(link_of(&ada, space, ada_member).holds, Some(ada_person));
	// A member's record is no person, and a plain edit of the field would replace the other
	// side's statement too: both are refused.
	let refused = ada.link_member(space, member, Some(ada_member), at + 150, &mut rng);
	assert!(
		matches!(refused, Err(NodeError::NoEntity(_))),
		"{refused:?}"
	);
	let edited = bea.edit_entity(
		space,
		member,
		vec![Edit::Set(
			"org.nibkeep.linked-person".parse().expect("a field"),
			vec![Value::Reference(olga)],
		)],
		&common::today(),
		at + 150,
		&mut rng,
	);
	assert!(matches!(edited, Err(NodeError::OwnField(_))), "{edited:?}");
	// Unlinked, the member is named by their own name again.
	bea.link_member(space, member, None, at + 200, &mut rng)
		.expect("unlinks");
	assert_eq!(link_of(&bea, space, member).holds, None);
	assert_eq!(named_by(&bea, (space, grandma)).as_deref(), Some("Bea"));
}

#[test]
fn an_admin_invites_from_a_closed_library_opened_once_the_joiner_connects() {
	let mut rng = TestRng::new("inviting from a closed library");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 1_000;
	let space = ada.create_space("Family", at, &mut rng).expect("makes it");
	let admin_network = ada.network(NetworkOptions::LOCAL).expect("starts");
	let closed = ada.close();
	let invitation = Invitation {
		space,
		role: MemberRole::Editor,
		person: None,
	};
	// Stopped, it ends without anyone joining.
	let stop = AtomicBool::new(true);
	let code = PairingCode::generate(&mut rng);
	let stopped = invite_closed(
		&closed,
		&admin_network,
		&invitation,
		&code,
		&mut |_| true,
		(Showing::new(TIMEOUT), &stop),
		&|| at,
		&mut rng,
	);
	assert!(
		matches!(stopped, Err(NodeError::InvitationExpired)),
		"{stopped:?}"
	);
	stop.store(false, Ordering::SeqCst);
	let joiner_network = bea.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		admin_network.endpoint(),
		&admin_network.local_addresses(),
		Some(&code.invitation_announcement()),
	);
	let mut asked = String::new();
	let mut admin_rng = TestRng::new("the admin's");
	let (invited, joined) = std::thread::scope(|scope| {
		let invited = scope.spawn(|| {
			invite_closed(
				&closed,
				&admin_network,
				&invitation,
				&code,
				&mut |invitee| {
					asked.clone_from(&invitee.user_name);
					true
				},
				(Showing::new(TIMEOUT), &stop),
				&|| at + 1_000,
				&mut admin_rng,
			)
		});
		let joined = join_space(
			&mut bea,
			&joiner_network,
			&code,
			TIMEOUT,
			&|| at + 1_000,
			&mut rng,
		);
		(invited.join().expect("the admin ran"), joined)
	});
	assert_eq!(joined.expect("joins").role, MemberRole::Editor);
	assert_eq!(invited.expect("invites").user_name, "Bea");
	assert_eq!(asked, "Bea");
	let ada = closed.open().expect("opens");
	let names: Vec<Option<String>> = ada
		.members(space)
		.expect("lists")
		.into_iter()
		.map(|member| member.name)
		.collect();
	assert!(names.contains(&Some("Bea".to_owned())), "{names:?}");
	// Only an admin's closed library shows a code.
	let viewer = bea.close();
	let refused = invite_closed(
		&viewer,
		&joiner_network,
		&invitation,
		&code,
		&mut |_| true,
		(Showing::new(TIMEOUT), &stop),
		&|| at + 2_000,
		&mut TestRng::new("a refusal"),
	);
	assert!(
		matches!(refused, Err(NodeError::AdminsOnly(_))),
		"{refused:?}"
	);
}
