//! Claims to the admin role ([technical design: claims]): when the admins of a space are gone,
//! an editor claims the role; every other member's device shows it saw the claim as soon as
//! it holds it; the claim takes effect 30 days after another member's device first saw it,
//! and for 30 days after that the new admin can only undo, unless an admin returns and
//! rejects it in time.
//!
//! The people: Ada, the family space's admin, who goes away; Bea, an editor, who claims; and
//! Cal, another editor.
//!
//! [technical design: claims]: ../../../docs/Technical-design.md#139-claims-to-the-admin-role

mod common;

use common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::SpaceId;
use nibkeep_node::{Library, MemberRole, NodeError};
use nibkeep_testkit::TestRng;

const DAY: u64 = 86_400_000;

/// Syncs `a` and `b` back and forth once, at `at`; returns the time after.
fn sync_once(a: &mut Library, b: &mut Library, at: u64) -> u64 {
	let at = at + 1_000;
	let (first, second) = sync_in_memory(a, b, at, (0, 1));
	first.unwrap_or_else(|error| panic!("the first side syncs: {error}"));
	second.unwrap_or_else(|error| panic!("the second side syncs: {error}"));
	at
}

/// The role that a space's members, as a library lists them, give the member named `name`.
fn role_of(library: &Library, space: SpaceId, name: &str) -> Option<MemberRole> {
	library
		.members(space)
		.expect("lists")
		.into_iter()
		.find(|member| member.name.as_deref() == Some(name))
		.map(|member| member.role)
}

/// The family's space, made by Ada, with Bea and Cal as editors, each synced with Ada.
struct Family {
	ada: Library,
	bea: Library,
	cal: Library,
	space: SpaceId,
	at: u64,
}

impl Family {
	fn new(rng: &mut TestRng) -> Self {
		let mut ada = user_library("Ada", "laptop", rng);
		let mut bea = user_library("Bea", "phone", rng);
		let mut cal = user_library("Cal", "tablet", rng);
		let mut at = now() + 100_000;
		let space = ada.create_space("Family", at, rng).expect("makes it");
		at += 2_000;
		invite_member(&mut ada, (space, MemberRole::Editor), &mut bea, at);
		at += 2_000;
		invite_member(&mut ada, (space, MemberRole::Editor), &mut cal, at);
		at = sync_once(&mut ada, &mut bea, at + 1_000);
		at = sync_once(&mut ada, &mut cal, at + 1_000);
		at = sync_once(&mut ada, &mut bea, at + 1_000);
		Self {
			ada,
			bea,
			cal,
			space,
			at: at + 1_000,
		}
	}

	/// Bea and Cal sync, Ada being away.
	fn sync_bea_and_cal(&mut self) {
		self.at = sync_once(&mut self.bea, &mut self.cal, self.at);
	}

	/// Lets `days` days pass.
	fn wait(&mut self, days: u64) {
		self.at += days * DAY;
	}
}

#[test]
fn a_claim_takes_effect_thirty_days_after_another_members_device_saw_it_and_settles() {
	let mut rng = TestRng::new("a claim");
	let mut family = Family::new(&mut rng);
	let space = family.space;
	let cal = family.bea.find_member(space, "Cal").expect("a member");
	let at = family.at;
	family
		.bea
		.claim_admin(space, at, &mut rng)
		.expect("Bea claims");
	assert!(matches!(
		family
			.bea
			.give_role(space, cal, MemberRole::Viewer, at + 100, &mut rng),
		Err(NodeError::AdminsOnly(_))
	));
	assert_eq!(
		family.bea.own_role(space).expect("reads"),
		MemberRole::Editor
	);
	// Cal's device sees the claim as soon as it holds it, and shows it.
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	let claims = family.cal.claims(space).expect("lists the claims");
	assert_eq!(claims.len(), 1);
	assert_eq!(claims[0].name.as_deref(), Some("Bea"));
	assert!(!claims[0].rejected);
	let seen = claims[0].seen.expect("Cal's device saw it");
	assert_eq!(claims[0].effective, Some(seen + 30 * DAY));
	// Thirty-one days on, Bea is an admin who can only undo.
	family.wait(31);
	family.sync_bea_and_cal();
	assert_eq!(
		family.bea.own_role(space).expect("reads"),
		MemberRole::Admin
	);
	let at = family.at;
	family
		.bea
		.give_role(space, cal, MemberRole::Viewer, at, &mut rng)
		.expect("Bea lowers Cal");
	assert!(matches!(
		family
			.bea
			.give_role(space, cal, MemberRole::Admin, at + 100, &mut rng),
		Err(NodeError::AdminsOnly(_))
	));
	family.sync_bea_and_cal();
	assert_eq!(role_of(&family.cal, space, "Cal"), Some(MemberRole::Viewer));
	// Once settled, her device states her role, and she makes admins.
	family.wait(30);
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	assert_eq!(role_of(&family.cal, space, "Bea"), Some(MemberRole::Admin));
	let at = family.at;
	family
		.bea
		.give_role(space, cal, MemberRole::Admin, at, &mut rng)
		.expect("Bea makes Cal an admin");
	family.sync_bea_and_cal();
	assert_eq!(role_of(&family.cal, space, "Cal"), Some(MemberRole::Admin));
	// The claim removed nobody: Ada is an admin still.
	assert_eq!(role_of(&family.cal, space, "Ada"), Some(MemberRole::Admin));
}

#[test]
fn an_admin_who_returns_rejects_a_claim_and_what_the_claimant_did_as_admin() {
	let mut rng = TestRng::new("a rejection");
	let mut family = Family::new(&mut rng);
	let space = family.space;
	let cal = family.bea.find_member(space, "Cal").expect("a member");
	let at = family.at;
	family
		.bea
		.claim_admin(space, at, &mut rng)
		.expect("Bea claims");
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	family.wait(31);
	family.sync_bea_and_cal();
	let at = family.at;
	family
		.bea
		.give_role(space, cal, MemberRole::Viewer, at, &mut rng)
		.expect("Bea lowers Cal");
	family.sync_bea_and_cal();
	assert_eq!(role_of(&family.cal, space, "Cal"), Some(MemberRole::Viewer));
	// Ada comes back ten days later and rejects the claim; Cal's device sees it.
	family.wait(10);
	family.at = sync_once(&mut family.ada, &mut family.bea, family.at);
	let bea = family.ada.find_member(space, "Bea").expect("a member");
	let at = family.at;
	assert!(matches!(
		family.cal.reject_claim(space, bea, at, &mut rng),
		Err(NodeError::AdminsOnly(_))
	));
	family
		.ada
		.reject_claim(space, bea, at, &mut rng)
		.expect("Ada rejects the claim");
	family.at = sync_once(&mut family.ada, &mut family.cal, family.at);
	family.at = sync_once(&mut family.ada, &mut family.cal, family.at);
	family.at = sync_once(&mut family.ada, &mut family.bea, family.at);
	let claims = family.ada.claims(space).expect("lists the claims");
	assert!(claims[0].rejected);
	assert_eq!(role_of(&family.ada, space, "Cal"), Some(MemberRole::Editor));
	assert_eq!(role_of(&family.ada, space, "Bea"), Some(MemberRole::Editor));
}

#[test]
fn only_a_direct_editor_claims() {
	let mut rng = TestRng::new("who claims");
	let mut family = Family::new(&mut rng);
	let space = family.space;
	let cal = family.ada.find_member(space, "Cal").expect("a member");
	let at = family.at;
	family
		.ada
		.give_role(space, cal, MemberRole::Viewer, at, &mut rng)
		.expect("Ada makes Cal a viewer");
	family.at = sync_once(&mut family.ada, &mut family.cal, family.at);
	let at = family.at;
	assert!(matches!(
		family.cal.claim_admin(space, at, &mut rng),
		Err(NodeError::ClaimNeedsEditor)
	));
	assert!(matches!(
		family.ada.claim_admin(space, at, &mut rng),
		Err(NodeError::ClaimNeedsEditor)
	));
	assert!(matches!(
		family
			.ada
			.claim_admin(family.ada.personal_space(), at, &mut rng),
		Err(NodeError::NeverShared)
	));
}

#[test]
fn a_claimant_removed_before_the_claim_settles_takes_nothing() {
	let mut rng = TestRng::new("a removed claimant");
	let mut family = Family::new(&mut rng);
	let space = family.space;
	let at = family.at;
	family
		.bea
		.claim_admin(space, at, &mut rng)
		.expect("Bea claims");
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	// Ada comes back before the claim takes effect, and removes Bea instead of rejecting.
	family.wait(10);
	family.at = sync_once(&mut family.ada, &mut family.bea, family.at);
	let bea = family.ada.find_member(space, "Bea").expect("a member");
	let at = family.at;
	family
		.ada
		.remove_member(space, bea, at, &mut rng)
		.expect("removes Bea");
	family.at = sync_once(&mut family.ada, &mut family.bea, family.at);
	family.at = sync_once(&mut family.ada, &mut family.cal, family.at);
	family.wait(61);
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	assert_eq!(role_of(&family.cal, space, "Bea"), Some(MemberRole::None));
	assert_eq!(family.bea.own_role(space).expect("reads"), MemberRole::None);
}

#[test]
fn a_settled_claim_is_rejected_no_more() {
	let mut rng = TestRng::new("too late to reject");
	let mut family = Family::new(&mut rng);
	let space = family.space;
	let at = family.at;
	family
		.bea
		.claim_admin(space, at, &mut rng)
		.expect("Bea claims");
	family.sync_bea_and_cal();
	family.sync_bea_and_cal();
	family.wait(61);
	family.sync_bea_and_cal();
	family.at = sync_once(&mut family.ada, &mut family.bea, family.at);
	let bea = family.ada.find_member(space, "Bea").expect("a member");
	let at = family.at;
	assert!(matches!(
		family.ada.reject_claim(space, bea, at, &mut rng),
		Err(NodeError::NoClaim(_))
	));
}
