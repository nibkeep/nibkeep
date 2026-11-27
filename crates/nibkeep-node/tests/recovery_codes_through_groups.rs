//! Recovery codes in the spaces that a person reaches through a group alone: their devices,
//! listed there through the group, name their codes there too, on their member's record in
//! the group, so the space gives the codes its keys and takes a new key when one is revoked;
//! and the group's admins give the codes the space, as they give it to the devices
//! ([technical design: recovery codes]).
//!
//! The people: Mona, an admin of the household's group and of the family's space, which she
//! adds the household to; Sam and Fred, editors in the household alone; and Gran, a viewer
//! of the family alone.
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use crate::common;

use crate::common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::SpaceId;
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{Library, MemberRole, RecoveryKeys, Unlocker};
use nibkeep_testkit::TestRng;

/// The passphrase of the libraries that [`user_library`] makes.
const UNLOCKER: Unlocker<'static> = Unlocker::Passphrase("battery staple");

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

/// What a check of `code` on `library` finds in `space`, if the space names the code.
fn in_space(library: &Library, code: &RecoveryCode, space: SpaceId) -> Option<RecoveryKeys> {
	library
		.check_recovery_code(code)
		.expect("checks")
		.spaces
		.into_iter()
		.find(|keys| keys.space == space)
}

/// The key that a library writes a space with.
fn writing_key(library: &Library, space: SpaceId) -> u64 {
	library.space_key_numbers(space).expect("reads").1
}

/// Makes a code on a library: the person wrote it down and typed it back.
fn make_code(library: &mut Library, at: u64, rng: &mut TestRng) -> RecoveryCode {
	let code = RecoveryCode::generate(rng);
	library
		.add_recovery_code(&code, false, at, rng)
		.expect("names the code");
	code
}

/// The household's group and the family's space, both made by Mona, with Sam and Fred in the
/// household as editors and Gran in the family as a viewer, each synced with Mona; then the
/// household added to the family, and synced until Sam's device holds the family.
struct Families {
	mona: Library,
	sam: Library,
	fred: Library,
	gran: Library,
	household: SpaceId,
	family: SpaceId,
	at: u64,
}

impl Families {
	fn new(rng: &mut TestRng) -> Self {
		let mut mona = user_library("Mona", "laptop", rng);
		let mut sam = user_library("Sam", "phone", rng);
		let mut fred = user_library("Fred", "tablet", rng);
		let mut gran = user_library("Gran", "reader", rng);
		let mut at = now() + 100_000;
		let household = mona.create_group("Household", at, rng).expect("makes it");
		let family = mona
			.create_space("Family", at + 100, rng)
			.expect("makes it");
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut sam, at);
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut fred, at);
		at += 2_000;
		invite_member(&mut mona, (family, MemberRole::Viewer), &mut gran, at);
		at = sync_rounds(&mut mona, &mut fred, at + 1_000, 1);
		at = sync_rounds(&mut mona, &mut gran, at + 1_000, 1);
		at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
		mona.add_group(family, household, MemberRole::Editor, at + 1_000, rng)
			.expect("adds the group");
		let mut families = Self {
			mona,
			sam,
			fred,
			gran,
			household,
			family,
			at: at + 2_000,
		};
		families.sync_sam(4);
		assert!(families.sam.spaces().contains(&family));
		assert_eq!(
			families.sam.own_role(family).expect("reads"),
			MemberRole::Editor
		);
		families
	}

	/// Mona's device and Sam's sync `rounds` times.
	fn sync_sam(&mut self, rounds: u64) {
		self.at = sync_rounds(&mut self.mona, &mut self.sam, self.at, rounds);
	}

	/// Mona's device syncs with each of the others, in turn, `rounds` times.
	fn sync_all(&mut self, rounds: u64) {
		for _ in 0..rounds {
			self.at = sync_rounds(&mut self.mona, &mut self.sam, self.at, 1);
			self.at = sync_rounds(&mut self.mona, &mut self.fred, self.at, 1);
			self.at = sync_rounds(&mut self.mona, &mut self.gran, self.at, 1);
		}
	}

	/// Mona removes Gran from the family, which gives it a new key: returns its number.
	fn remove_gran(&mut self, rng: &mut TestRng) -> u64 {
		let gran = self
			.mona
			.find_member(self.family, "Gran")
			.expect("a member");
		self.mona
			.remove_member(self.family, gran, self.at, rng)
			.expect("removes");
		self.at += 1_000;
		writing_key(&self.mona, self.family)
	}
}

#[test]
fn a_code_reaches_a_space_that_its_person_is_in_through_a_group_alone() {
	let mut rng = TestRng::new("a code through a group");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let code = make_code(&mut families.sam, families.at, &mut rng);
	families.at += 1_000;
	families.sync_sam(3);
	// The family names Sam's code, on his device and on Mona's, as the household does; and
	// the household's admin gave the code the family, as she gave it his devices.
	for library in [&families.sam, &families.mona] {
		assert!(in_space(library, &code, household).is_some());
		assert!(in_space(library, &code, family).is_some());
	}
	let keys = in_space(&families.sam, &code, family).expect("the family names it");
	assert_eq!(keys.entry, Some(true));
	// A new key of the family is the code's too, whoever makes it.
	let number = families.remove_gran(&mut rng);
	families.sync_sam(2);
	let keys = in_space(&families.sam, &code, family).expect("the family names it");
	assert!(keys.opens.contains(&number), "{keys:?}");
	assert_eq!(keys.lacks, [] as [u64; 0]);
	// Syncing again adds nothing: the code is named and given the family once.
	let before = [household, family].map(|space| {
		(
			changes(&families.mona, space),
			changes(&families.sam, space),
		)
	});
	families.sync_sam(2);
	let after = [household, family].map(|space| {
		(
			changes(&families.mona, space),
			changes(&families.sam, space),
		)
	});
	assert_eq!(after, before);
}

#[test]
fn a_code_reaches_the_spaces_of_a_group_that_its_persons_group_is_inside() {
	let mut rng = TestRng::new("a code through a group inside a group");
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut sam = user_library("Sam", "phone", &mut rng);
	let mut at = now() + 100_000;
	let household = mona
		.create_group("Household", at, &mut rng)
		.expect("makes it");
	let kids = mona
		.create_group("Kids", at + 100, &mut rng)
		.expect("makes it");
	let family = mona
		.create_space("Family", at + 200, &mut rng)
		.expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (kids, MemberRole::Editor), &mut sam, at);
	at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
	mona.add_group(family, household, MemberRole::Editor, at, &mut rng)
		.expect("adds the household to the family");
	mona.add_group(household, kids, MemberRole::Editor, at + 100, &mut rng)
		.expect("adds the kids to the household");
	at = sync_rounds(&mut mona, &mut sam, at + 1_000, 5);
	assert!(sam.spaces().contains(&family));
	// Sam's code reaches the household through the kids, and the family through both.
	let code = make_code(&mut sam, at, &mut rng);
	sync_rounds(&mut mona, &mut sam, at + 1_000, 4);
	assert!(in_space(&sam, &code, kids).is_some());
	for space in [household, family] {
		let keys = in_space(&sam, &code, space).expect("the space names it");
		assert_eq!(keys.entry, Some(true), "{keys:?}");
		assert!(in_space(&mona, &code, space).is_some());
	}
}

#[test]
fn revoking_a_code_revokes_it_in_a_space_reached_through_a_group_with_a_new_key() {
	let mut rng = TestRng::new("a code revoked through a group");
	let mut families = Families::new(&mut rng);
	let family = families.family;
	let code = make_code(&mut families.sam, families.at, &mut rng);
	families.at += 1_000;
	families.sync_sam(3);
	let before = writing_key(&families.sam, family);
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	families
		.sam
		.revoke_recovery_code(&name, families.at, &mut rng)
		.expect("revokes");
	families.at += 1_000;
	// The family has a new key, which the code lacks, on his device and on Mona's.
	let number = writing_key(&families.sam, family);
	assert_ne!(number, before);
	families.sync_sam(2);
	assert_eq!(writing_key(&families.mona, family), number);
	for library in [&families.mona, &families.sam] {
		let keys = in_space(library, &code, family).expect("the family names it");
		assert!(keys.lacks.contains(&number), "{keys:?}");
		assert!(!keys.opens.contains(&number), "{keys:?}");
	}
	// Nor is it revoked twice.
	let before = (
		changes(&families.mona, family),
		changes(&families.sam, family),
	);
	families.sync_sam(2);
	assert_eq!(
		(
			changes(&families.mona, family),
			changes(&families.sam, family)
		),
		before
	);
}

#[test]
fn a_code_is_given_no_new_key_of_a_space_once_its_person_is_out_of_the_group() {
	let mut rng = TestRng::new("a code once its person is out of the group");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let code = make_code(&mut families.sam, families.at, &mut rng);
	families.at += 1_000;
	families.sync_sam(3);
	let before = writing_key(&families.mona, family);
	// Mona takes Sam out of the household: the family takes a new key, which his code lacks.
	let sam = families
		.mona
		.find_member(household, "Sam")
		.expect("a member");
	families
		.mona
		.remove_member(household, sam, families.at, &mut rng)
		.expect("removes");
	families.at += 1_000;
	families.at = sync_rounds(&mut families.mona, &mut families.gran, families.at, 2);
	let number = writing_key(&families.mona, family);
	assert_ne!(number, before);
	let keys = in_space(&families.mona, &code, family).expect("the family names it");
	assert!(keys.lacks.contains(&number), "{keys:?}");
	assert!(!keys.opens.contains(&number), "{keys:?}");
	// Nor does a key that Mona makes later reach it.
	let number = families.remove_gran(&mut rng);
	let keys = in_space(&families.mona, &code, family).expect("the family names it");
	assert!(keys.lacks.contains(&number), "{keys:?}");
}

#[test]
fn nobody_names_another_members_code_in_a_space_reached_through_a_group() {
	let mut rng = TestRng::new("another member's code through a group");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	families.sync_all(2);
	assert!(families.fred.spaces().contains(&family));
	// Fred's device, infected, names in the family a code on Sam's record in the household:
	// Sam's own devices alone may.
	let code = RecoveryCode::generate(&mut rng);
	let sam = families
		.mona
		.find_member(household, "Sam")
		.expect("a member");
	let stated = nibkeep_governance::RecoveryStatement::sign(&code, sam);
	let operation = nibkeep_governance::state_recovery_code(family, &stated, &mut rng);
	let directory = families
		.fred
		.library_directory()
		.expect("a library")
		.to_owned();
	drop(families.fred);
	common::forge(
		&directory,
		&UNLOCKER,
		(family, vec![operation]),
		(None, families.at),
		&mut rng,
	);
	families.fred = Library::open(&directory, &UNLOCKER).expect("opens");
	let before = changes(&families.mona, family);
	families.at = sync_rounds(&mut families.mona, &mut families.fred, families.at, 2);
	// Mona's device holds the change, and the family names no code.
	assert!(changes(&families.mona, family) > before);
	assert_eq!(in_space(&families.mona, &code, family), None);
}

#[test]
fn a_code_follows_its_member_made_direct_and_then_taken_out_of_the_group() {
	let mut rng = TestRng::new("a code of a member made direct");
	let mut families = Families::new(&mut rng);
	let (household, family) = (families.household, families.family);
	let code = make_code(&mut families.sam, families.at, &mut rng);
	families.at += 1_000;
	families.sync_sam(3);
	// Mona makes Sam a direct member of the family, and he confirms: the code stays on his
	// record in the household, which still gives him a role, and is named once.
	let sam = families
		.mona
		.find_group_member(family, "Sam")
		.expect("finds Sam through the household");
	families
		.mona
		.make_direct(family, sam, MemberRole::Editor, families.at, &mut rng)
		.expect("makes a record for Sam");
	families.at += 1_000;
	families.sync_sam(1);
	families
		.sam
		.confirm_direct(family, families.at, &mut rng)
		.expect("Sam confirms");
	families.at += 1_000;
	families.sync_sam(3);
	let before = changes(&families.sam, family);
	families.sync_sam(2);
	assert_eq!(changes(&families.sam, family), before);
	// Mona takes him out of the household: his devices name the code on his own record in
	// the family, which gives it the family's keys, a new one included.
	let in_household = families
		.mona
		.find_member(household, "Sam")
		.expect("a member");
	families
		.mona
		.remove_member(household, in_household, families.at, &mut rng)
		.expect("removes");
	families.at += 1_000;
	families.sync_sam(3);
	let number = families.remove_gran(&mut rng);
	families.sync_sam(2);
	let keys = in_space(&families.sam, &code, family).expect("the family names it");
	assert!(keys.opens.contains(&number), "{keys:?}");
	assert_eq!(keys.lacks, [] as [u64; 0]);
	// Revoking the code revokes it there, with a new key that it lacks.
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	families
		.sam
		.revoke_recovery_code(&name, families.at, &mut rng)
		.expect("revokes");
	families.at += 1_000;
	let number = writing_key(&families.sam, family);
	families.sync_sam(2);
	for library in [&families.mona, &families.sam] {
		let keys = in_space(library, &code, family).expect("the family names it");
		assert!(keys.lacks.contains(&number), "{keys:?}");
		assert!(!keys.opens.contains(&number), "{keys:?}");
	}
}
