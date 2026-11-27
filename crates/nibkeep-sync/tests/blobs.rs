//! Sealed blobs go only to a replica that asks for them: they are not in the set that a sync
//! reconciles, every replica receives every blob key, and each side asks for the blobs it
//! keeps and lacks ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#3-syncing)).

use crate::common::{Replica, sync};
use nibkeep_base::Hash;
use nibkeep_log::SpaceKeys;
use nibkeep_sync::Synced;
use nibkeep_testkit::TestRng;

/// Two replicas of one space that sync with each other.
fn two(name: &str) -> (TestRng, Replica, Replica) {
	let mut rng = TestRng::new(name);
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut a = Replica::new(&mut rng, space, &keys);
	let mut b = Replica::new(&mut rng, space, &keys);
	a.peers.insert(*b.key.verifying_key().as_bytes());
	b.peers.insert(*a.key.verifying_key().as_bytes());
	(rng, a, b)
}

/// Syncs `a` with `b`, `a` connecting, and returns what each side's sync did.
fn synced(rng: &mut TestRng, a: &mut Replica, b: &mut Replica) -> (Synced, Synced) {
	let (initiated, responded) = sync(a, b, rng.bytes(), None);
	let responded = responded.expect("the responder synced");
	(initiated.expect("the initiator synced"), responded)
}

/// How many blobs a side received in its sync, in all.
fn blobs_received(synced: &Synced) -> usize {
	synced.spaces.iter().map(|space| space.received.blobs).sum()
}

fn keys_of(replica: &Replica) -> Vec<Hash> {
	replica
		.log
		.wrapped_blob_keys()
		.map(|(sealed, _)| *sealed)
		.collect()
}

#[test]
fn a_replica_receives_only_the_blobs_it_asks_for() {
	let (mut rng, mut a, mut b) = two("only asked");
	let kept = a.add_blob(&mut rng, 1_000);
	let declined = a.add_blob(&mut rng, 2_000);
	b.declines.insert(declined);
	// The responder asks.
	let (_, responded) = synced(&mut rng, &mut a, &mut b);
	assert_eq!(blobs_received(&responded), 1);
	assert!(b.blobs.contains_key(&kept));
	assert!(!b.blobs.contains_key(&declined));
	// Every blob key comes, whatever blobs a replica keeps.
	assert_eq!(keys_of(&b), keys_of(&a));
	// The initiator asks too, and is sent nothing it declines; a blob that one side lacks
	// is no difference that a sync goes over again.
	let (initiated, responded) = synced(&mut rng, &mut b, &mut a);
	assert!(
		initiated
			.spaces
			.iter()
			.chain(&responded.spaces)
			.all(|space| space.sent.is_empty() && space.received.is_empty()),
		"{initiated:?} {responded:?}"
	);
	assert!(!b.blobs.contains_key(&declined));
}

#[test]
fn each_side_asks_for_what_it_lacks() {
	let (mut rng, mut a, mut b) = two("both ask");
	let from_a = a.add_blob(&mut rng, 1_500_000);
	let from_b = b.add_blob(&mut rng, 3_000);
	let (initiated, responded) = synced(&mut rng, &mut a, &mut b);
	assert_eq!(blobs_received(&initiated), 1);
	assert_eq!(blobs_received(&responded), 1);
	for replica in [&a, &b] {
		assert!(replica.blobs.contains_key(&from_a));
		assert!(replica.blobs.contains_key(&from_b));
	}
	assert_eq!(Hash::of(&b.blobs[&from_a]), from_a);
}

#[test]
fn a_blob_that_neither_side_holds_is_asked_for_and_not_sent() {
	let (mut rng, mut a, mut b) = two("held by neither");
	let sealed = a.add_blob(&mut rng, 1_000);
	a.blobs.remove(&sealed);
	let (initiated, responded) = synced(&mut rng, &mut a, &mut b);
	assert_eq!(blobs_received(&initiated) + blobs_received(&responded), 0);
	assert_eq!(keys_of(&b), [sealed]);
	assert!(b.asks.contains(&1), "{:?}", b.asks);
}

#[test]
fn a_replica_that_sends_no_blobs_to_a_peer_sends_it_none_when_asked() {
	let (mut rng, mut a, mut b) = two("no blobs to");
	let sealed = a.add_blob(&mut rng, 1_000);
	a.no_blobs_to.insert(*b.key.verifying_key().as_bytes());
	for _ in 0..2 {
		let (initiated, responded) = synced(&mut rng, &mut a, &mut b);
		assert_eq!(blobs_received(&initiated) + blobs_received(&responded), 0);
		let (initiated, responded) = synced(&mut rng, &mut b, &mut a);
		assert_eq!(blobs_received(&initiated) + blobs_received(&responded), 0);
	}
	assert!(!b.blobs.contains_key(&sealed));
	assert_eq!(keys_of(&b), [sealed]);
}
