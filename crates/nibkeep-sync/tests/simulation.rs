//! Replicas of one space that make changes, seal secrets and add blobs, and sync in pairs
//! now and then, over connections that may break off: once every pair has synced, they
//! hold the same, and a sync after that sends nothing.

use crate::common;

use std::collections::BTreeMap;

use crate::common::{Replica, sync};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_log::SpaceKeys;
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{FieldName, Operation, Statement, Value};
use nibkeep_testkit::TestRng;

/// The fields the replicas write: two plain ones, and a secret one.
const FIELDS: [&str; 3] = [
	"org.example.note",
	"org.example.title",
	"org.example.password",
];

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

/// A random number below `bound`.
fn below(rng: &mut TestRng, bound: usize) -> usize {
	let [byte_a, byte_b] = rng.bytes::<2>();
	usize::from(u16::from_le_bytes([byte_a, byte_b])) % bound
}

/// What a replica holds, in a form that compares.
#[derive(Debug, PartialEq, Eq)]
struct Held {
	heads: Vec<String>,
	holdings: Vec<[u8; 32]>,
	values: BTreeMap<(EntityId, String), Vec<String>>,
}

fn held(replica: &Replica, entities: &[EntityId]) -> Held {
	let mut holdings: Vec<[u8; 32]> = nibkeep_sync::Holder::holdings(replica, replica.space)
		.unwrap_or_else(|never| match never {})
		.iter()
		.map(nibkeep_sync::item_id)
		.collect();
	holdings.sort_unstable();
	let mut values = BTreeMap::new();
	for &entity in entities {
		for name in FIELDS {
			let current: Vec<String> = replica
				.log
				.state()
				.current(entity, &field(name))
				.infallible()
				.iter()
				.map(|statement| format!("{:?}", statement.value))
				.collect();
			values.insert((entity, name.to_owned()), current);
		}
	}
	Held {
		heads: replica
			.log
			.heads()
			.infallible()
			.iter()
			.map(ToString::to_string)
			.collect(),
		holdings,
		values,
	}
}

/// A replica writes a value: titles replace what is there; notes and passwords add to it.
fn write(rng: &mut TestRng, replica: &mut Replica, entities: &[EntityId], wall: u64) {
	let entity = entities[below(rng, entities.len())];
	let name = FIELDS[below(rng, FIELDS.len())];
	let value = Value::Text(format!("{name} {wall}"));
	let statement = Statement::new(StatementId::random(rng), entity, field(name), value);
	let operation = if name == FIELDS[1] {
		let current: Vec<StatementId> = replica
			.log
			.state()
			.current(entity, &field(name))
			.infallible()
			.iter()
			.map(|statement| statement.id)
			.collect();
		Operation::replace(statement, current)
	} else {
		Operation::assert(statement)
	};
	let key = replica.key.clone();
	replica
		.log
		.commit(&key, &replica.keys, vec![operation], wall, rng)
		.expect("commits");
}

/// A replica seals a blob of up to 1.25 MiB, so that some take two messages.
fn add_blob(rng: &mut TestRng, replica: &mut Replica) {
	let len = below(rng, 1_300_000) + 1;
	replica.add_blob(rng, len);
}

/// One run of the simulation: `count` replicas, `steps` random steps, then syncs until
/// every pair has synced.
fn run(seed: &str, count: usize, steps: usize) -> (usize, usize) {
	let mut rng = TestRng::new(seed);
	let space = rng.space_id();
	let mut keys = SpaceKeys::new(0, rng.key());
	keys.insert_secrets(0, rng.key());
	let mut replicas: Vec<Replica> = (0..count)
		.map(|_| Replica::new(&mut rng, space, &keys))
		.collect();
	let all: Vec<[u8; 32]> = replicas
		.iter()
		.map(|replica| *replica.key.verifying_key().as_bytes())
		.collect();
	for replica in &mut replicas {
		replica.peers = all.iter().copied().collect();
		replica.log.set_secret_fields([field(FIELDS[2])]);
	}
	let entities: Vec<EntityId> = (0..3).map(|_| rng.entity_id()).collect();
	let mut wall = 1_800_000_000_000_u64;
	let (mut broken, mut blobs) = (0, 0);
	for _ in 0..steps {
		wall += 1_000 + u64::try_from(below(&mut rng, 5_000)).expect("small");
		for replica in &mut replicas {
			replica.wall = wall;
		}
		let who = below(&mut rng, count);
		match below(&mut rng, 10) {
			0..=4 => write(&mut rng, &mut replicas[who], &entities, wall),
			5 => {
				add_blob(&mut rng, &mut replicas[who]);
				blobs += 1;
			}
			_ => {
				let other = (who + 1 + below(&mut rng, count - 1)) % count;
				// Some connections break off after a few messages.
				let budget = (below(&mut rng, 3) == 0).then(|| below(&mut rng, 12));
				let binding = rng.bytes();
				let (a, b) = pair(&mut replicas, who, other);
				let (initiated, responded) = sync(a, b, binding, budget);
				if budget.is_some() && (initiated.is_err() || responded.is_err()) {
					broken += 1;
				} else {
					initiated.expect("the initiator synced");
					responded.expect("the responder synced");
				}
			}
		}
	}
	// Every replica syncs with the next one, around twice: then each holds what all hold.
	for round in 0..2 * count {
		let (who, other) = (round % count, (round + 1) % count);
		let binding = rng.bytes();
		let (a, b) = pair(&mut replicas, who, other);
		let (initiated, responded) = sync(a, b, binding, None);
		initiated.expect("the initiator synced");
		responded.expect("the responder synced");
	}
	let first = held(&replicas[0], &entities);
	for replica in &replicas[1..] {
		assert_eq!(held(replica, &entities), first, "{seed}");
	}
	// Nothing is left to send.
	let binding = rng.bytes();
	let (a, b) = pair(&mut replicas, 0, count - 1);
	let (initiated, _) = sync(a, b, binding, None);
	let synced = initiated.expect("synced");
	assert!(
		synced
			.spaces
			.iter()
			.all(|space| space.sent.is_empty() && space.received.is_empty()),
		"{seed}: {synced:?}"
	);
	// Every blob arrived whole.
	for replica in &replicas {
		for (sealed, bytes) in &replica.blobs {
			assert_eq!(Hash::of(bytes), *sealed);
		}
		assert!(
			replica
				.log
				.wrapped_blob_keys()
				.all(|(sealed, _)| replica.blobs.contains_key(sealed))
		);
	}
	(broken, blobs)
}

/// Two different replicas of the list, mutably.
fn pair(replicas: &mut [Replica], a: usize, b: usize) -> (&mut Replica, &mut Replica) {
	assert_ne!(a, b);
	if a < b {
		let (left, right) = replicas.split_at_mut(b);
		(&mut left[a], &mut right[0])
	} else {
		let (left, right) = replicas.split_at_mut(a);
		(&mut right[0], &mut left[b])
	}
}

#[test]
fn replicas_that_sync_in_pairs_converge() {
	let (mut broken, mut blobs) = (0, 0);
	for seed in 0..12 {
		let count = 2 + seed % 4;
		let (cut, added) = run(&format!("simulation {seed}"), count, 60);
		broken += cut;
		blobs += added;
	}
	// The situations worth testing came up.
	assert!(broken > 5, "connections broke off {broken} times");
	assert!(blobs > 5, "{blobs} blobs");
}

#[test]
fn a_stranger_is_refused_and_learns_nothing() {
	let mut rng = TestRng::new("stranger");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut ours = Replica::new(&mut rng, space, &keys);
	let mut stranger = Replica::new(&mut rng, space, &keys);
	stranger.peers.insert(*ours.key.verifying_key().as_bytes());
	let (initiated, responded) = sync(&mut stranger, &mut ours, rng.bytes(), None);
	assert!(matches!(
		responded,
		Err(nibkeep_sync::SyncError::NotAReplica)
	));
	assert!(
		matches!(
			initiated,
			Err(nibkeep_sync::SyncError::Ended(
				nibkeep_sync::Reason::Refused
			))
		),
		"{initiated:?}"
	);
}

#[test]
fn a_replica_listed_by_a_code_introduces_itself_where_the_code_stands() {
	let mut rng = TestRng::new("introduced");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut ours = Replica::new(&mut rng, space, &keys);
	let mut new = Replica::new(&mut rng, space, &keys);
	new.peers.insert(*ours.key.verifying_key().as_bytes());
	let code = nibkeep_crypto::RecoveryCode::generate(&mut rng);
	let other = nibkeep_crypto::RecoveryCode::generate(&mut rng);
	let encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
	let (record, owner) = (rng.entity_id(), rng.entity_id());
	let listing = |code: &nibkeep_crypto::RecoveryCode, key: &nibkeep_crypto::VerifyingKey| {
		nibkeep_governance::RecoveryListing::sign(code, (space, record, owner), key, &encryption)
	};
	ours.codes
		.insert(*code.signing_key().verifying_key().as_bytes());
	let new_key = new.key.verifying_key();
	// A listing by a code that does not stand here, or of another key, introduces nobody.
	let stranger_key = rng.signing_key().verifying_key();
	for listings in [
		vec![],
		vec![listing(&other, &new_key)],
		vec![listing(&code, &stranger_key)],
	] {
		new.introductions = listings;
		let (initiated, responded) = sync(&mut new, &mut ours, rng.bytes(), None);
		assert!(
			matches!(responded, Err(nibkeep_sync::SyncError::NotAReplica)),
			"{responded:?}"
		);
		assert!(initiated.is_err());
	}
	// Its own listing, by the code, does: the two sync the space.
	new.introductions = vec![listing(&other, &new_key), listing(&code, &new_key)];
	let (initiated, responded) = sync(&mut new, &mut ours, rng.bytes(), None);
	let synced = responded.expect("syncs");
	assert_eq!(synced.peer, new_key);
	assert_eq!(
		synced
			.spaces
			.iter()
			.map(|synced| synced.space)
			.collect::<Vec<_>>(),
		[space]
	);
	initiated.expect("syncs");
	// Only for that sync: without its listings, the next is refused.
	new.introductions = Vec::new();
	let (_, responded) = sync(&mut new, &mut ours, rng.bytes(), None);
	assert!(
		matches!(responded, Err(nibkeep_sync::SyncError::NotAReplica)),
		"{responded:?}"
	);
}

#[test]
fn a_hello_for_another_connection_does_not_check() {
	let mut rng = TestRng::new("binding");
	let space = rng.space_id();
	let keys = SpaceKeys::new(0, rng.key());
	let mut a = Replica::new(&mut rng, space, &keys);
	let mut b = Replica::new(&mut rng, space, &keys);
	a.peers.insert(*b.key.verifying_key().as_bytes());
	b.peers.insert(*a.key.verifying_key().as_bytes());
	// The initiator signs for one connection; the responder checks against another.
	let (a_end, b_end) = common::pipes_bound(a.endpoint, b.endpoint, [1; 32], [2; 32]);
	let (initiated, responded) = common::sync_over(&mut a, &mut b, a_end, b_end);
	assert!(
		matches!(responded, Err(nibkeep_sync::SyncError::Protocol(_))),
		"{responded:?}"
	);
	assert!(initiated.is_err());
	let holdings = nibkeep_sync::Holder::holdings(&b, space).unwrap_or_else(|never| match never {});
	assert_eq!(holdings, []);
}
