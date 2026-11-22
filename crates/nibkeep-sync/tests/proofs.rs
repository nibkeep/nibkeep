//! A replica proves that it holds a sealed blob now, as another is about to remove its own
//! copy: it answers a fresh challenge from the blob's bytes, after it has recorded that it
//! holds the blob for a while, and proves nothing it is removing itself
//! ([Sync](../../../docs/spec/sync.md#3-syncing)).

mod common;

use common::{Replica, sync};
use nibkeep_base::Hash;
use nibkeep_log::SpaceKeys;
use nibkeep_sync::{CHALLENGES_AT_ONCE, Challenge};
use nibkeep_testkit::TestRng;

/// Two replicas of one space that sync with each other, `a` holding a blob of 50,000 bytes
/// and `b` a copy of it, after one sync.
fn two(name: &str) -> (TestRng, Replica, Replica, Hash) {
	let mut rng = TestRng::new(name);
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut a = Replica::new(&mut rng, space, &keys);
	let mut b = Replica::new(&mut rng, space, &keys);
	a.peers.insert(*b.key.verifying_key().as_bytes());
	b.peers.insert(*a.key.verifying_key().as_bytes());
	let sealed = a.add_blob(&mut rng, 50_000);
	synced(&mut rng, &mut a, &mut b);
	assert!(b.blobs.contains_key(&sealed));
	(rng, a, b, sealed)
}

fn synced(rng: &mut TestRng, a: &mut Replica, b: &mut Replica) {
	let (initiated, responded) = sync(a, b, rng.bytes(), None);
	initiated.expect("the initiator synced");
	responded.expect("the responder synced");
}

/// Puts these challenges to `b` in `a`'s next sync with it.
fn challenge(a: &mut Replica, b: &Replica, challenges: Vec<Challenge>) {
	a.challenges
		.insert(*b.key.verifying_key().as_bytes(), challenges);
}

/// A challenge of a blob that `a` holds.
fn of(rng: &mut TestRng, a: &Replica, sealed: Hash) -> Challenge {
	Challenge::random(sealed, a.blobs[&sealed].len() as u64, rng)
}

#[test]
fn a_replica_proves_a_blob_it_holds_and_then_holds_it() {
	let (mut rng, mut a, mut b, sealed) = two("proves");
	let lacked = a.add_blob(&mut rng, 3_000);
	b.declines.insert(lacked);
	let asked = vec![of(&mut rng, &a, sealed), of(&mut rng, &a, lacked)];
	challenge(&mut a, &b, asked.clone());
	synced(&mut rng, &mut a, &mut b);
	assert_eq!(a.proofs.len(), 2);
	// The answer is the one that the remover's own copy gives.
	let expected = asked[0].answer_from(&a.blobs[&sealed]);
	assert!(expected.is_some());
	assert_eq!(a.proofs[0], (asked[0].clone(), expected));
	// A blob that it never received, it cannot prove.
	assert_eq!(a.proofs[1], (asked[1].clone(), None));
	// It holds what it proved, and only that.
	assert_eq!(b.held, [sealed].into());
}

#[test]
fn a_replica_proves_nothing_that_it_is_removing_itself() {
	let (mut rng, mut a, mut b, sealed) = two("removing");
	b.removing.insert(sealed);
	let asked = vec![of(&mut rng, &a, sealed)];
	challenge(&mut a, &b, asked);
	synced(&mut rng, &mut a, &mut b);
	assert_eq!(a.proofs.len(), 1);
	assert_eq!(a.proofs[0].1, None);
	assert!(b.held.is_empty());
}

#[test]
fn a_damaged_copy_gives_another_answer() {
	let (mut rng, mut a, mut b, sealed) = two("damaged");
	// Every byte of the copy changed, so every range reads otherwise.
	for byte in b.blobs.get_mut(&sealed).expect("held").iter_mut() {
		*byte ^= 0x5a;
	}
	let asked = vec![of(&mut rng, &a, sealed)];
	challenge(&mut a, &b, asked.clone());
	synced(&mut rng, &mut a, &mut b);
	let expected = asked[0].answer_from(&a.blobs[&sealed]);
	assert_eq!(a.proofs.len(), 1);
	assert_ne!(a.proofs[0].1, expected);
}

#[test]
fn a_sync_puts_at_most_one_message_of_challenges_in_a_space() {
	let (mut rng, mut a, mut b, _) = two("many");
	let blobs: Vec<Hash> = (0..CHALLENGES_AT_ONCE + 3)
		.map(|_| a.add_blob(&mut rng, 40))
		.collect();
	synced(&mut rng, &mut a, &mut b);
	let asked: Vec<Challenge> = blobs
		.iter()
		.map(|sealed| of(&mut rng, &a, *sealed))
		.collect();
	challenge(&mut a, &b, asked.clone());
	synced(&mut rng, &mut a, &mut b);
	// The rest wait for another sync.
	assert_eq!(a.proofs.len(), CHALLENGES_AT_ONCE);
	for (challenge, answer) in &a.proofs {
		assert_eq!(*answer, challenge.answer_from(&a.blobs[&challenge.sealed]));
	}
	assert_eq!(b.held.len(), CHALLENGES_AT_ONCE);
}
