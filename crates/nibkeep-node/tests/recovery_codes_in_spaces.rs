//! Recovery codes in the spaces besides the personal one: a person's devices carry their
//! codes into every space they are listed in, give the codes those spaces, and carry the
//! revocations too ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: ../../../docs/Technical-design.md#111-recovery-codes

mod common;

use common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::SpaceId;
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{Library, MemberRole, RecoveryKeys};
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

/// Ada's household, which Ada made, and Bea, whom she invited as an editor, synced once.
fn household(rng: &mut TestRng) -> (Library, Library, SpaceId, u64) {
	let mut ada = user_library("Ada", "laptop", rng);
	let mut bea = user_library("Bea", "phone", rng);
	let at = now() + 100_000;
	let space = ada.create_space("Household", at, rng).expect("makes it");
	invite_member(&mut ada, (space, MemberRole::Editor), &mut bea, at + 2_000);
	let at = sync_rounds(&mut ada, &mut bea, at + 3_000, 1);
	(ada, bea, space, at)
}

#[test]
fn a_code_is_given_each_space_of_its_persons_and_named_there() {
	let mut rng = TestRng::new("a code and a space of one's own");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let at = now() + 100_000;
	let space = ada.create_space("Household", at, &mut rng).expect("makes");
	let code = make_code(&mut ada, at + 1_000, &mut rng);
	let check = ada.check_recovery_code(&code).expect("checks");
	assert!(check.code.is_some());
	let personal = ada.personal_space();
	let spaces: Vec<SpaceId> = check.spaces.iter().map(|keys| keys.space).collect();
	assert_eq!(spaces.len(), 2);
	assert!(spaces.contains(&personal) && spaces.contains(&space));
	for keys in &check.spaces {
		// The personal space's first key reaches a code another way.
		assert_eq!(keys.entry, (keys.space == space).then_some(true));
		assert_eq!(keys.lacks, [] as [u64; 0]);
	}
	// A space made after the code is given to it too, and names it, once the keys settle.
	let later = ada
		.create_space("Allotment", at + 2_000, &mut rng)
		.expect("makes");
	ada.settle_keys(at + 3_000, &mut rng).expect("settles");
	let keys = in_space(&ada, &code, later).expect("the later space names it");
	assert_eq!(keys.entry, Some(true));
}

#[test]
fn a_members_devices_carry_their_code_into_a_shared_space() {
	let mut rng = TestRng::new("a code in a shared space");
	let (mut ada, mut bea, space, at) = household(&mut rng);
	let code = make_code(&mut bea, at + 1_000, &mut rng);
	// Her own device names it in the space at once, and the personal space gives the code
	// the space.
	let keys = in_space(&bea, &code, space).expect("the space names it");
	assert_eq!(keys.entry, Some(true));
	// Ada's device learns of Bea's code by syncing: the space names it, and it is none of
	// Ada's own.
	let at = sync_rounds(&mut ada, &mut bea, at + 2_000, 2);
	let check = ada.check_recovery_code(&code).expect("checks");
	assert_eq!(check.code, None);
	let [keys] = check.spaces.as_slice() else {
		panic!("the space alone: {:?}", check.spaces);
	};
	assert_eq!((keys.space, keys.entry), (space, None));
	// Syncing again adds nothing: the code is named once.
	let before = (changes(&ada, space), changes(&bea, space));
	sync_rounds(&mut ada, &mut bea, at, 2);
	assert_eq!((changes(&ada, space), changes(&bea, space)), before);
	// Ada's device carries none of Bea's codes, and has none of her own.
	assert_eq!(ada.recovery_codes().expect("lists"), []);
}

#[test]
fn a_code_made_before_its_person_joined_a_space_is_carried_there_after() {
	let mut rng = TestRng::new("a code made before joining");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let code = make_code(&mut bea, at, &mut rng);
	let space = ada
		.create_space("Household", at + 1_000, &mut rng)
		.expect("makes it");
	invite_member(&mut ada, (space, MemberRole::Viewer), &mut bea, at + 2_000);
	// A viewer's device names her code there too: it is her own to name.
	sync_rounds(&mut ada, &mut bea, at + 3_000, 2);
	let keys = in_space(&bea, &code, space).expect("the space names it");
	assert_eq!(keys.entry, Some(true));
	assert!(in_space(&ada, &code, space).is_some());
}

#[test]
fn a_new_key_of_a_shared_space_is_the_codes_too_whoever_makes_it() {
	let mut rng = TestRng::new("a shared space's new key");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let mut cy = user_library("Cy", "tablet", &mut rng);
	let at = now() + 100_000;
	let space = ada.create_space("Household", at, &mut rng).expect("makes");
	invite_member(&mut ada, (space, MemberRole::Editor), &mut bea, at + 2_000);
	invite_member(&mut ada, (space, MemberRole::Viewer), &mut cy, at + 4_000);
	let at = sync_rounds(&mut ada, &mut bea, at + 5_000, 1);
	let code = make_code(&mut bea, at + 1_000, &mut rng);
	let at = sync_rounds(&mut ada, &mut bea, at + 2_000, 2);
	// Ada removes Cy: the new key is sealed to Bea's code by Ada's device.
	let cy_member = ada.find_member(space, "Cy").expect("a member");
	ada.remove_member(space, cy_member, at, &mut rng)
		.expect("removes");
	let number = writing_key(&ada, space);
	sync_rounds(&mut ada, &mut bea, at + 1_000, 2);
	let keys = in_space(&bea, &code, space).expect("the space names it");
	assert!(keys.opens.contains(&number), "{keys:?}");
	assert_eq!(keys.lacks, [] as [u64; 0]);
}

#[test]
fn revoking_a_code_revokes_it_in_every_space_with_a_new_key_there() {
	let mut rng = TestRng::new("a code revoked in every space");
	let (mut ada, mut bea, space, at) = household(&mut rng);
	let code = make_code(&mut bea, at + 1_000, &mut rng);
	let at = sync_rounds(&mut ada, &mut bea, at + 2_000, 2);
	let before = writing_key(&bea, space);
	let name = RecoveryCode::name_of(&code.signing_key().verifying_key());
	bea.revoke_recovery_code(&name, at, &mut rng)
		.expect("revokes");
	// The space has a new key, which the code lacks, on her device and on Ada's.
	let number = writing_key(&bea, space);
	assert_ne!(number, before);
	let at = sync_rounds(&mut ada, &mut bea, at + 1_000, 2);
	assert_eq!(writing_key(&ada, space), number);
	for library in [&ada, &bea] {
		let keys = in_space(library, &code, space).expect("the space names it");
		assert!(keys.lacks.contains(&number), "{keys:?}");
		assert!(!keys.opens.contains(&number), "{keys:?}");
	}
	// Nor is it revoked twice: syncing again adds nothing.
	let changes_before = (changes(&ada, space), changes(&bea, space));
	sync_rounds(&mut ada, &mut bea, at, 2);
	assert_eq!((changes(&ada, space), changes(&bea, space)), changes_before);
}
