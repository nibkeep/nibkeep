//! An infected device of a member of a group ([technical design: groups]) plants no first
//! change of its own making in a space that the group gives another member's device: that
//! device takes as the space's first change only the one that the group's agreement and the
//! entry it took name, also once its library has closed and opened again, and also when
//! another program on the device took the space.
//!
//! The people: Mona, an admin of the household's group and of the family's space, which she
//! adds the household to; Sam and Fred, editors in the household; and Gran, a viewer of the
//! family alone. Fred's device is infected.
//!
//! [technical design: groups]: ../../../docs/Technical-design.md#138-groups

mod common;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::mpsc;

use common::in_process::Pipe;
use common::{invite_member, now, sync_in_memory, user_library};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_log::{Change, ChangeId, Draft, Kind, Parcel, Payload};
use nibkeep_model::{Name, Operation, Statement, TypeName, Value, fields};
use nibkeep_node::{Library, MemberRole, NodeError, Syncing, Unlocker};
use nibkeep_sync::{BlobBytes, Holder, Holding};
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

/// The household's group and the family's space, both made by Mona, with Sam and Fred in the
/// household as editors, each synced with Mona; and the household added to the family, which
/// no device but Mona's holds yet.
struct World {
	mona: Library,
	sam: Library,
	fred: Library,
	household: SpaceId,
	family: SpaceId,
	at: u64,
}

impl World {
	fn new(rng: &mut TestRng) -> Self {
		let mut mona = user_library("Mona", "laptop", rng);
		let mut sam = user_library("Sam", "phone", rng);
		let mut fred = user_library("Fred", "tablet", rng);
		let mut at = now() + 100_000;
		let household = mona.create_group("Household", at, rng).expect("makes it");
		let family = mona
			.create_space("Family", at + 100, rng)
			.expect("makes it");
		mona.create_entity(
			family,
			&[TypeName::from_static("std.person")],
			vec![(
				"org.nibkeep.name".parse().expect("a field"),
				Value::Name(Name::new("Wedding album")),
			)],
			at + 200,
			rng,
		)
		.expect("adds");
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut sam, at);
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut fred, at);
		at = sync_rounds(&mut mona, &mut fred, at + 1_000, 1);
		at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
		mona.add_group(family, household, MemberRole::Editor, at + 1_000, rng)
			.expect("adds the group");
		Self {
			mona,
			sam,
			fred,
			household,
			family,
			at: at + 2_000,
		}
	}

	/// Syncs Mona's device with Sam's, a round at a time, until Mona's gives his the family
	/// and his takes it: he has nothing of it yet.
	fn give_sam_the_family(&mut self) {
		for _ in 0..4 {
			self.at = sync_rounds(&mut self.mona, &mut self.sam, self.at, 1);
			if self.sam.spaces().contains(&self.family) {
				break;
			}
		}
		assert!(self.sam.spaces().contains(&self.family));
		assert_eq!(found(&self.sam, self.family, "album"), Vec::<String>::new());
	}

	/// A first change of `space` that Fred's device makes up, with a key of its own, signed
	/// with its replica's key: one that makes Fred an admin.
	fn forged_first_change(&self, space: SpaceId, rng: &mut TestRng) -> Parcel {
		let key_file =
			nibkeep_keys::KeyFile::read(&self.fred.directory().join("keys.cbor")).expect("reads");
		let replica = key_file
			.replica(&key_file.unlock_replica(&UNLOCKER).expect("unlocks"))
			.expect("opens");
		let mut draft = Draft::new(
			[],
			nibkeep_time::Timestamp::new(self.at, 0),
			Payload {
				operations: vec![Operation::assert(Statement::new(
					StatementId::random(rng),
					EntityId::random(rng),
					fields::ROLE,
					Value::Text("admin".to_owned()),
				))],
				..Payload::default()
			},
		);
		draft.kind = Kind::Governing;
		Change::create(
			space,
			&nibkeep_log::SpaceKeys::new(0, nibkeep_crypto::Key::generate(rng)),
			&replica.signing,
			&draft,
			rng,
		)
	}
}

/// A sync as an infected device runs it: with its library's keys, it syncs only `space`,
/// whether its library holds it or not, holds only `forged` there, sends it, and takes
/// nothing; `sent` tells whether the other side asked for it.
struct Infected<'a> {
	syncing: Syncing<'a>,
	space: SpaceId,
	forged: Parcel,
	sent: Cell<bool>,
}

impl Holder for Infected<'_> {
	type Error = NodeError;

	fn verifying_key(&self) -> nibkeep_crypto::VerifyingKey {
		self.syncing.verifying_key()
	}

	fn sign(&self, context: &'static str, message: &[u8]) -> nibkeep_crypto::Signature {
		self.syncing.sign(context, message)
	}

	fn spaces_with(&self, _: &nibkeep_crypto::VerifyingKey) -> Result<Vec<SpaceId>, NodeError> {
		Ok(vec![self.space])
	}

	fn holdings(&self, _: SpaceId) -> Result<Vec<Holding>, NodeError> {
		Ok(vec![Holding::Change {
			id: self.forged.id(),
			millis: self.forged.change.timestamp().millis(),
			depth: 0,
			complete: true,
		}])
	}

	fn sends(
		&self,
		_: SpaceId,
		_: &nibkeep_crypto::VerifyingKey,
		_: &Holding,
	) -> Result<bool, NodeError> {
		Ok(true)
	}

	fn wanted_blobs(
		&mut self,
		_: SpaceId,
		_: &nibkeep_crypto::VerifyingKey,
		_: &BTreeSet<Hash>,
		_: usize,
	) -> Result<Vec<Hash>, NodeError> {
		Ok(Vec::new())
	}

	fn parcel(&self, _: SpaceId, id: ChangeId) -> Result<Option<Parcel>, NodeError> {
		let sends = id == self.forged.id();
		self.sent.set(self.sent.get() || sends);
		Ok(sends.then(|| self.forged.clone()))
	}

	fn blob(&self, _: SpaceId, _: &Hash) -> Result<Option<(u64, BlobBytes<'_>)>, NodeError> {
		Ok(None)
	}

	fn receive_change(
		&mut self,
		_: SpaceId,
		_: &nibkeep_crypto::VerifyingKey,
		_: Parcel,
	) -> Result<bool, NodeError> {
		Ok(false)
	}

	fn receive_record_key(
		&mut self,
		_: SpaceId,
		_: StatementId,
		_: nibkeep_log::WrappedKey,
	) -> Result<bool, NodeError> {
		Ok(false)
	}

	fn receive_blob_key(
		&mut self,
		_: SpaceId,
		_: Hash,
		_: nibkeep_log::WrappedBlobKey,
	) -> Result<bool, NodeError> {
		Ok(false)
	}

	fn receive_blob(
		&mut self,
		_: SpaceId,
		_: Hash,
		_: u64,
		_: &mut dyn std::io::Read,
	) -> Result<bool, NodeError> {
		Ok(false)
	}

	fn finish_space(&mut self, _: SpaceId) -> Result<(), NodeError> {
		Ok(())
	}
}

/// Syncs an infected device's library with another's in memory, at `at`, as [`Infected`]
/// says, with the infected device connecting; the other settles what it took, as after any
/// sync. Checks that the other asked for the forged change, so that it had to refuse it.
fn sync_infected(
	infected: &mut Library,
	(space, forged): (SpaceId, Parcel),
	other: &mut Library,
	at: u64,
) {
	let (to_other, from_infected) = mpsc::channel();
	let (to_infected, from_other) = mpsc::channel();
	let (infected_endpoint, other_endpoint) = (infected.endpoint(), other.endpoint());
	let mut infected_end = Pipe {
		to: to_other,
		from: from_other,
		early: None,
		own: infected_endpoint,
		peer: other_endpoint,
	};
	let mut other_end = Pipe {
		to: to_infected,
		from: from_infected,
		early: None,
		own: other_endpoint,
		peer: infected_endpoint,
	};
	let now = move || at;
	let known = other.checkout_contents().expect("reads");
	let synced = std::thread::scope(|scope| {
		let initiated = scope.spawn(|| {
			let mut holder = Infected {
				syncing: Syncing::new(infected, &now),
				space,
				forged,
				sent: Cell::new(false),
			};
			let synced = nibkeep_sync::initiate(&mut infected_end, &mut holder);
			assert!(
				holder.sent.get(),
				"the other side asks for the forged change"
			);
			drop(holder);
			drop(infected_end);
			synced
		});
		let responded = nibkeep_sync::respond(&mut other_end, &mut Syncing::new(other, &now));
		drop(other_end);
		initiated
			.join()
			.expect("the infected device ran")
			.expect("the infected device syncs");
		responded.expect("the other device syncs")
	});
	let mut rng = TestRng::new(&format!("settling after an infected device at {at}"));
	other.settle_keys(at, &mut rng).expect("settles");
	other.settle_waiting(at).expect("settles");
	other
		.settle_checkouts(&known, at, &mut rng)
		.expect("settles");
	other
		.settle_moves(&synced, at, at, &mut rng)
		.expect("settles");
}

#[test]
fn a_space_taken_through_a_group_takes_only_its_own_first_change_after_the_library_opens_again() {
	let mut rng = TestRng::new("a first change of one's own making after a restart");
	let mut world = World::new(&mut rng);
	let family = world.family;
	world.give_sam_the_family();
	// Sam's library opens again, in a new program, and the old one closes.
	let directory = world.sam.library_directory().expect("a library").to_owned();
	world.sam = Library::open(&directory, &UNLOCKER).expect("opens");
	// Fred's device, infected, sends Sam's a first change of its own making for the family.
	let forged = world.forged_first_change(family, &mut rng);
	world.at += 1_000;
	sync_infected(&mut world.fred, (family, forged), &mut world.sam, world.at);
	// Sam's takes the family's own all the same, when it comes.
	world.at = sync_rounds(&mut world.mona, &mut world.sam, world.at, 3);
	assert_eq!(found(&world.sam, family, "album"), ["Wedding album"]);
}

#[test]
fn a_space_taken_through_a_group_by_another_program_takes_only_its_own_first_change() {
	let mut rng = TestRng::new("a first change of one's own making in another program");
	let mut world = World::new(&mut rng);
	let family = world.family;
	// Another program holds Sam's library open, as an app does, while this one takes the
	// family.
	let directory = world.sam.library_directory().expect("a library").to_owned();
	let mut other = Library::open(&directory, &UNLOCKER).expect("opens");
	world.give_sam_the_family();
	other.catch_up().expect("catches up");
	assert!(other.spaces().contains(&family));
	// Fred's device, infected, sends the other program a first change of its own making.
	let forged = world.forged_first_change(family, &mut rng);
	world.at += 1_000;
	sync_infected(&mut world.fred, (family, forged), &mut other, world.at);
	// The other program takes the family's own all the same, when it comes.
	world.at = sync_rounds(&mut world.mona, &mut other, world.at, 3);
	assert_eq!(found(&other, family, "album"), ["Wedding album"]);
}

#[test]
fn a_groups_log_taken_by_another_program_takes_only_its_own_first_change() {
	let mut rng = TestRng::new("a group's first change of one's own making in another program");
	let mut world = World::new(&mut rng);
	let (household, family) = (world.household, world.family);
	// Fred joins the family too, then Gran, in the family alone, while another program holds
	// her library open, as an app does.
	world.at += 1_000;
	invite_member(
		&mut world.mona,
		(family, MemberRole::Viewer),
		&mut world.fred,
		world.at,
	);
	world.at = sync_rounds(&mut world.mona, &mut world.fred, world.at + 1_000, 1);
	let mut gran = user_library("Gran", "tablet", &mut rng);
	let directory = gran.library_directory().expect("a library").to_owned();
	let mut other = Library::open(&directory, &UNLOCKER).expect("opens");
	world.at += 1_000;
	invite_member(
		&mut world.mona,
		(family, MemberRole::Viewer),
		&mut gran,
		world.at,
	);
	// Joining the family, Gran's device took the household's first key from it, and keeps
	// the household's log, with nothing in it yet.
	assert!(gran.spaces().contains(&household));
	assert!(!gran.is_group(household).expect("reads"));
	other.catch_up().expect("catches up");
	assert!(other.spaces().contains(&household));
	// Fred's device, infected, sends the other program a first change of its own making for
	// the household.
	let forged = world.forged_first_change(household, &mut rng);
	world.at += 1_000;
	sync_infected(&mut world.fred, (household, forged), &mut other, world.at);
	// The other program takes the household's own all the same, when it comes.
	world.at = sync_rounds(&mut world.mona, &mut other, world.at, 3);
	assert!(other.is_group(household).expect("reads"));
}
