//! Which changes count in a space whose first change governs it: those of the replicas it
//! lists, and of a revoked replica only those that every replica which revoked it had
//! ([technical design: revocation]).
//!
//! [technical design: revocation]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#11-revocation-and-new-keys

mod common;

use std::collections::BTreeSet;

use common::{DRIFT, Device, add, pair, send, text, texts};
use nibkeep_base::EntityId;
use nibkeep_log::{
	Change, ChangeId, Draft, Kind, Log, LogError, Parcel, Payload, SpaceKeys, Status,
};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::{Clock, Timestamp};
use proptest::prelude::*;

fn start() -> u64 {
	millis_at("2026-10-26", 9, 0)
}

const NOTE: &str = "org.example.note";

/// A governed space: its keys, and replicas that are devices of one user, each with a
/// record derived from the space and its key, as in every space of NibKeep. The first
/// founds the space.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	devices: Vec<Device>,
	replicas: Vec<EntityId>,
	/// The thing that the tests write notes on.
	thing: EntityId,
	now: u64,
	id: nibkeep_base::SpaceId,
	/// The user, or in a space with members the member, whose replicas every device's are.
	owner: EntityId,
	/// Each device's encryption key.
	encryptions: Vec<nibkeep_crypto::EncryptionKey>,
}

impl Space {
	/// A space founded by the first of `count` devices, which lists only itself.
	fn new(seed: &str, count: usize) -> Self {
		Self::founded(seed, count, None)
	}

	/// A space founded by the first of `count` devices, which lists only itself, and gives
	/// its member `role`, if any, as the first change of a space with members does.
	fn founded(seed: &str, count: usize, role: Option<&str>) -> Self {
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let mut devices: Vec<Device> = (0..count).map(|_| Device::new(&mut rng, id)).collect();
		// As in every space of NibKeep, the first change must govern the space.
		for device in &mut devices {
			device.log.expect_governed();
		}
		let replicas = devices
			.iter()
			.map(|device| nibkeep_governance::replica_record(id, &device.key.verifying_key()))
			.collect();
		let thing = rng.entity_id();
		let owner = rng.entity_id();
		let encryptions = (0..count)
			.map(|_| nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key())
			.collect();
		let mut space = Self {
			rng,
			keys,
			devices,
			replicas,
			thing,
			now: start(),
			id,
			owner,
			encryptions,
		};
		let mut founding = space.signed_listing(0, false);
		if let Some(role) = role {
			founding.push(governing(
				&mut space.rng,
				owner,
				fields::ROLE,
				Value::Text(role.to_owned()),
			));
		}
		space.commit(0, founding);
		space
	}

	/// The governing operations that list device `listed`'s key on its record, owned by the
	/// space's owner, with the listing that its key signed.
	fn signed_listing(&mut self, listed: usize, ai_agent: bool) -> Vec<Operation> {
		let signed = nibkeep_governance::Listing::sign(
			&self.devices[listed].key,
			&self.encryptions[listed],
			self.id,
			(self.owner, ai_agent),
		);
		let mut operations = self.unsigned_listing(listed, signed.record, ai_agent);
		operations.push(governing(
			&mut self.rng,
			signed.record,
			fields::LISTING,
			Value::Text(signed.to_text()),
		));
		operations
	}

	/// The governing operations that list device `listed`'s key on `record`, owned by the
	/// space's owner, without a listing that its key signed.
	fn unsigned_listing(
		&mut self,
		listed: usize,
		record: EntityId,
		ai_agent: bool,
	) -> Vec<Operation> {
		let key = self.devices[listed].key.verifying_key();
		let encryption = nibkeep_governance::encryption_key_text(&self.encryptions[listed]);
		let rng = &mut self.rng;
		let mut operations = vec![
			governing(rng, record, fields::SIGNING_KEY, Value::Text(key.to_text())),
			governing(rng, record, fields::OWNER, Value::Reference(self.owner)),
			governing(rng, record, fields::ENCRYPTION_KEY, Value::Text(encryption)),
		];
		if ai_agent {
			operations.push(governing(
				rng,
				record,
				fields::AI_AGENT,
				Value::Boolean(true),
			));
		}
		operations
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	/// The governing operations that list device `listed`'s key for its replica.
	fn listing(&mut self, listed: usize) -> Vec<Operation> {
		self.signed_listing(listed, false)
	}

	/// The governing operations that revoke device `revoked`'s replica, by its key.
	fn revoking(&mut self, revoked: usize) -> Vec<Operation> {
		let key = self.devices[revoked].key.verifying_key();
		vec![governing(
			&mut self.rng,
			self.replicas[revoked],
			fields::REVOKED,
			Value::Text(key.to_text()),
		)]
	}

	/// The governing operations that accept a change of device `author`'s.
	fn accepting(&mut self, author: usize, change: ChangeId) -> Vec<Operation> {
		vec![governing(
			&mut self.rng,
			self.replicas[author],
			fields::ACCEPTED,
			Value::Text(change.to_string()),
		)]
	}

	/// The governing operations that make `field` secret for good, on device `by`'s record.
	fn making_secret(&mut self, by: usize, field: &str) -> Vec<Operation> {
		vec![governing(
			&mut self.rng,
			self.replicas[by],
			fields::SECRET_FIELD,
			Value::Text(field.to_owned()),
		)]
	}

	/// A note on the thing.
	fn note(&mut self, note: &str) -> Vec<Operation> {
		vec![add(&mut self.rng, self.thing, NOTE, text(note))]
	}

	fn commit(&mut self, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.keys.clone();
		self.devices[device].commit(&keys, operations, wall, &mut self.rng)
	}

	/// Device `to` receives every change of device `from`, with its payload and keys, also
	/// those it has without them, parents first, and every wrapped record key, as a sync
	/// sends them. Changes of keys that it knows of no listing of are refused, as a sync
	/// drops them.
	fn sync(&mut self, from: usize, to: usize) {
		let wall = self.tick();
		let (from, to) = pair(&mut self.devices, from, to);
		send(&from.log, &mut to.log, &self.keys, wall);
		to.receive_keys_from(from, &self.keys);
	}

	/// Devices `a` and `b` receive what the other has.
	fn sync_both(&mut self, a: usize, b: usize) {
		self.sync(a, b);
		self.sync(b, a);
	}

	/// The notes that a device shows, sorted.
	fn notes(&self, device: usize) -> Vec<String> {
		let mut notes = texts(&self.devices[device].log, self.thing, NOTE);
		notes.sort();
		notes
	}

	fn set_aside(&self, device: usize, change: ChangeId) -> bool {
		self.devices[device].log.is_set_aside(change).infallible()
	}
}

fn governing(
	rng: &mut TestRng,
	entity: EntityId,
	field: nibkeep_model::FieldName,
	value: Value,
) -> Operation {
	Operation::assert(Statement::new(rng.statement_id(), entity, field, value))
}

#[test]
fn changes_of_a_key_that_nobody_listed_are_refused_until_it_is_listed() {
	let mut space = Space::new("unlisted", 2);
	// The laptop has the space, but nobody listed its key yet.
	space.sync(0, 1);
	let note = space.note("from nobody");
	let note = space.commit(1, note);
	space.sync(1, 0);
	assert!(!space.devices[0].log.contains(note.id()).infallible());
	assert_eq!(space.notes(0), Vec::<String>::new());
	// Once the phone lists it, what it wrote is taken, and counts.
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(1, 0);
	assert!(!space.set_aside(0, note.id()));
	assert_eq!(space.notes(0), ["from nobody"]);
}

#[test]
fn a_key_cannot_list_itself() {
	let mut space = Space::new("listing itself", 2);
	space.sync(0, 1);
	let listing = space.listing(1);
	let own = space.commit(1, listing);
	let note = space.note("self-made");
	let note = space.commit(1, note);
	space.sync(1, 0);
	for change in [own.id(), note.id()] {
		assert!(!space.devices[0].log.contains(change).infallible());
	}
	assert_eq!(space.notes(0), Vec::<String>::new());
}

#[test]
fn a_revoked_replicas_changes_that_the_revoker_lacked_are_set_aside() {
	let mut space = Space::new("revoked", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	let note = space.note("seen");
	let seen = space.commit(1, note);
	space.sync(1, 0);
	// The phone writes once more, and only the tablet gets it.
	let note = space.note("unseen");
	let unseen = space.commit(1, note);
	space.sync(1, 2);
	assert_eq!(space.notes(2), ["seen", "unseen"]);
	// The desktop revokes the phone, without knowing of its last change.
	let revoking = space.revoking(1);
	let revocation = space.commit(0, revoking);
	space.sync(1, 0);
	space.sync(0, 2);
	for device in [0, 2] {
		assert!(!space.set_aside(device, seen.id()), "on {device}");
		assert!(space.set_aside(device, unseen.id()), "on {device}");
		assert_eq!(space.notes(device), ["seen"], "on {device}");
		assert_eq!(
			space.devices[device]
				.log
				.revocations(&space.keys)
				.expect("reads"),
			vec![revocation.id()]
		);
	}
	// What the phone writes later is set aside too.
	space.sync(0, 1);
	let note = space.note("later");
	let later = space.commit(1, note);
	space.sync(1, 2);
	assert!(space.set_aside(2, later.id()));
	assert_eq!(space.notes(2), ["seen"]);
}

#[test]
fn of_two_revocations_only_what_both_revokers_had_counts() {
	let mut space = Space::new("two revokers", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	let note = space.note("both had it");
	let both = space.commit(1, note);
	space.sync(1, 0);
	space.sync(1, 2);
	let note = space.note("only the tablet had it");
	let one = space.commit(1, note);
	space.sync(1, 2);
	for revoker in [0, 2] {
		let revoking = space.revoking(1);
		space.commit(revoker, revoking);
	}
	space.sync_both(0, 2);
	for device in [0, 2] {
		assert!(!space.set_aside(device, both.id()));
		assert!(space.set_aside(device, one.id()));
		assert_eq!(space.notes(device), ["both had it"]);
	}
}

#[test]
fn a_revoked_replica_cannot_list_replicas_of_its_own() {
	let mut space = Space::new("revoked lists", 3);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	space.sync(0, 2);
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	// The phone, not knowing, lists the tablet, which writes.
	let listing = space.listing(2);
	let listed = space.commit(1, listing);
	space.sync(1, 2);
	let note = space.note("the phone's friend");
	let note = space.commit(2, note);
	space.sync(1, 0);
	space.sync(2, 0);
	assert!(space.set_aside(0, listed.id()));
	assert!(space.set_aside(0, note.id()));
	assert_eq!(space.notes(0), Vec::<String>::new());
}

#[test]
fn a_purge_by_a_revoked_replica_is_undone_and_its_value_comes_back() {
	let mut space = Space::new("purge undone", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	let note = space.note("keep me");
	let Operation::Assert { statement, .. } = &note[0] else {
		unreachable!("an assertion");
	};
	let statement = statement.id;
	let kept = space.commit(0, note);
	space.sync(0, 1);
	space.sync(0, 2);
	// The phone purges the note, and only the tablet gets the purge.
	let wall = space.tick();
	let keys = space.keys.clone();
	let phone = &mut space.devices[1];
	let purge = phone
		.log
		.purge(&phone.key, &keys, [statement], wall, &mut space.rng)
		.expect("purges");
	space.sync(1, 2);
	assert_eq!(space.notes(2), Vec::<String>::new());
	assert!(
		space.devices[2]
			.log
			.is_change_purged(kept.id())
			.infallible()
	);
	// The desktop revokes the phone, and the tablet hears of it.
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	space.sync(0, 2);
	assert!(space.set_aside(2, purge.id()));
	assert!(
		!space.devices[2]
			.log
			.is_change_purged(kept.id())
			.infallible()
	);
	// The tablet lost the note's payload, and gets it back from the desktop.
	space.sync(0, 2);
	assert_eq!(space.notes(2), ["keep me"]);
	assert_eq!(space.notes(0), ["keep me"]);
	space.sync(1, 0);
	assert!(space.set_aside(0, purge.id()));
	assert_eq!(space.notes(0), ["keep me"]);
}

#[test]
fn of_two_replicas_that_revoke_each_other_the_one_added_first_stays() {
	let mut space = Space::new("each other", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// The phone was added before the tablet. Each revokes the other, offline.
	let revoking = space.revoking(2);
	let by_phone = space.commit(1, revoking);
	let revoking = space.revoking(1);
	let by_tablet = space.commit(2, revoking);
	let note = space.note("the tablet's");
	let tablets = space.commit(2, note);
	let note = space.note("the phone's");
	let phones = space.commit(1, note);
	space.sync(1, 0);
	space.sync(2, 0);
	space.sync_both(1, 2);
	for device in 0..3 {
		assert!(!space.set_aside(device, by_phone.id()), "on {device}");
		assert!(space.set_aside(device, by_tablet.id()), "on {device}");
		assert!(!space.set_aside(device, phones.id()), "on {device}");
		assert!(space.set_aside(device, tablets.id()), "on {device}");
	}
}

#[test]
fn a_revoked_replica_may_revoke_back_only_one_added_after_it() {
	let mut space = Space::new("revoking back", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	let [phone, tablet] = [1, 2].map(|device| space.devices[device].key.verifying_key());
	let may_revoke_back = |space: &Space, device: usize, key| {
		space.devices[device]
			.log
			.may_revoke_back(key, &space.keys)
			.expect("reads")
	};
	// The tablet, added after the phone, revokes it: the phone would stay if it revoked
	// the tablet back. A replica that nothing revoked has nothing to revoke back.
	let revoking = space.revoking(1);
	space.commit(2, revoking);
	assert!(may_revoke_back(&space, 2, &phone));
	assert!(!may_revoke_back(&space, 2, &tablet));
	// The phone, which revokes the tablet, added after it, gives it nothing to revoke back.
	let revoking = space.revoking(2);
	space.commit(1, revoking);
	assert!(!may_revoke_back(&space, 1, &tablet));
	// Once the first device, added before the phone, revokes it too, it may not.
	space.sync(2, 0);
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	assert!(!may_revoke_back(&space, 0, &phone));
}

#[test]
fn an_accepted_change_counts_after_all() {
	let mut space = Space::new("accepted", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	let note = space.note("worth keeping");
	let note = space.commit(1, note);
	space.sync(1, 0);
	assert!(space.set_aside(0, note.id()));
	assert_eq!(
		space.devices[0].log.set_aside_changes().infallible(),
		[note.id()]
	);
	let accepting = space.accepting(1, note.id());
	space.commit(0, accepting);
	assert!(!space.set_aside(0, note.id()));
	assert_eq!(space.notes(0), ["worth keeping"]);
	assert_eq!(
		space.devices[0].log.set_aside_changes().infallible(),
		Vec::<ChangeId>::new()
	);
}

#[test]
fn a_governed_space_has_one_root() {
	let mut space = Space::new("one root", 2);
	let mut rng = TestRng::new("another root");
	let keys = space.keys.clone();
	let wall = space.tick();
	let mut other = Device::new(&mut rng, space.devices[0].log.space());
	other.key = space.devices[0].key.clone();
	let listing = space.listing(0);
	let root = other.commit(&keys, listing, wall, &mut rng);
	assert_eq!(
		space.devices[0].log.receive(root, &keys, wall),
		Err(LogError::SecondRoot)
	);
}

#[test]
fn a_set_aside_change_has_its_own_status() {
	let mut space = Space::new("status", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	let note = space.note("revoked");
	let note = space.commit(1, note);
	space.sync(1, 0);
	let history = space.devices[0].log.history();
	let accepted = nibkeep_log::History::get(history, note.id())
		.infallible()
		.expect("accepted");
	assert_eq!(accepted.status, Status::SetAside);
}

/// What one run of a story does, as each device ends up: the notes it shows, and which of
/// the story's changes it sets aside.
type Outcome = (Vec<String>, Vec<bool>);

/// A story of four devices with a revocation, a purge, a listing by the revoked device, a
/// revocation of each other and an acceptance; then each device gets every change, in an
/// order that `order` shuffles. Every order must end the same.
fn story(order: &[usize]) -> Vec<Outcome> {
	let mut space = Space::new("story", 4);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	let note = space.note("first");
	let first = space.commit(0, note);
	space.sync(0, 1);
	space.sync(0, 2);
	let note = space.note("phone, seen");
	let seen = space.commit(1, note);
	space.sync(1, 0);
	let note = space.note("phone, unseen");
	let unseen = space.commit(1, note);
	let listing = space.listing(3);
	let phones_listing = space.commit(1, listing);
	space.sync(1, 3);
	let note = space.note("the phone's friend");
	let friend = space.commit(3, note);
	let revoking = space.revoking(1);
	let revocation = space.commit(0, revoking);
	let revoking = space.revoking(0);
	let back = space.commit(1, revoking);
	// The desktop gets what the phone did, and accepts the change it had not seen.
	space.sync(1, 0);
	let accepting = space.accepting(1, unseen.id());
	let accepted = space.commit(0, accepting);
	let watched = [
		first.id(),
		seen.id(),
		unseen.id(),
		phones_listing.id(),
		friend.id(),
		revocation.id(),
		back.id(),
		accepted.id(),
	];
	// Each device gets the others' changes, in the order given, twice over.
	let count = space.devices.len();
	for round in 0..2 {
		for &step in order {
			let from = step % count;
			let to = (step / count + round) % count;
			if from != to {
				space.sync(from, to);
			}
		}
	}
	for from in 0..count {
		for to in 0..count {
			if from != to {
				space.sync(from, to);
			}
		}
	}
	(0..count)
		.map(|device| {
			let notes = space.notes(device);
			let aside = watched
				.iter()
				.map(|&change| space.set_aside(device, change))
				.collect();
			(notes, aside)
		})
		.collect()
}

#[test]
fn the_story_ends_as_the_rules_say() {
	let outcomes = story(&[]);
	let (notes, aside) = &outcomes[0];
	assert_eq!(notes, &["first", "phone, seen", "phone, unseen"]);
	assert_eq!(
		aside,
		&[false, false, false, true, true, false, true, false],
		"the phone's listing and its friend's note are set aside, and so is the phone's \
		 revocation of the desktop, which was added first"
	);
	assert!(outcomes.iter().all(|outcome| outcome == &outcomes[0]));
}

proptest! {
	#![proptest_config(ProptestConfig::with_cases(24))]

	#[test]
	fn every_order_of_arrival_ends_the_same(order in proptest::collection::vec(0usize..16, 0..24)) {
		let outcomes = story(&order);
		let expected = story(&[]);
		prop_assert_eq!(outcomes, expected);
	}
}

#[test]
fn a_revoked_replica_cannot_have_its_revoker_revoked() {
	for listed_first in [false, true] {
		let mut space = Space::new("revoker kept", 2);
		let listing = space.listing(1);
		space.commit(0, listing);
		space.sync(0, 1);
		// The phone claims the laptop's key for its own record, before its revocation
		// reaches the laptop or after.
		let laptop_key = space.devices[0].key.verifying_key();
		let claim = vec![governing(
			&mut space.rng,
			space.replicas[1],
			fields::SIGNING_KEY,
			Value::Text(laptop_key.to_text()),
		)];
		if listed_first {
			space.commit(1, claim.clone());
			space.sync(1, 0);
		}
		let revoking = space.revoking(1);
		space.commit(0, revoking);
		if !listed_first {
			space.commit(1, claim);
			space.sync(1, 0);
		}
		let laptop = &space.devices[0].log;
		assert!(
			!laptop.is_revoked(&laptop_key, &space.keys).expect("reads"),
			"listed first: {listed_first}"
		);
		let note = space.note("still mine");
		let note = space.commit(0, note);
		assert!(!space.set_aside(0, note.id()));
		assert_eq!(space.notes(0), ["still mine"]);
	}
}

#[test]
fn a_key_that_lists_itself_is_not_older_for_it() {
	let mut space = Space::new("not older", 3);
	// The tablet is listed before the phone.
	for listed in [2, 1] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// The phone lists itself, in a change dated just after the space's first, on it alone.
	let root = space.devices[0]
		.log
		.root()
		.infallible()
		.expect("a governed space");
	let first = space.devices[0]
		.log
		.change(root)
		.infallible()
		.expect("the first change");
	let listing = space.listing(1);
	let mut draft = Draft::new(
		[root],
		Timestamp::new(first.timestamp().millis() + 1, 0),
		Payload::from(listing),
	);
	draft.kind = Kind::Governing;
	let keys = space.keys.clone();
	let backdated = Change::create(
		space.devices[1].log.space(),
		&keys,
		&space.devices[1].key,
		&draft,
		&mut space.rng,
	);
	let wall = space.tick();
	space.devices[1]
		.log
		.receive(backdated, &keys, wall)
		.expect("takes the change");
	// They revoke each other, offline.
	let revoking = space.revoking(2);
	let by_phone = space.commit(1, revoking);
	let revoking = space.revoking(1);
	let by_tablet = space.commit(2, revoking);
	space.sync_both(1, 2);
	for device in [1, 2] {
		assert!(space.set_aside(device, by_phone.id()), "on {device}");
		assert!(!space.set_aside(device, by_tablet.id()), "on {device}");
	}
}

#[test]
fn a_governing_change_set_aside_is_never_accepted() {
	let mut space = Space::new("not accepted", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	// They revoke each other; the laptop, added first, stays.
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	let revoking = space.revoking(0);
	let back = space.commit(1, revoking);
	space.sync(1, 0);
	assert!(space.set_aside(0, back.id()));
	let accepting = space.accepting(1, back.id());
	space.commit(0, accepting);
	assert!(space.set_aside(0, back.id()));
	let note = space.note("the laptop's");
	let note = space.commit(0, note);
	assert!(!space.set_aside(0, note.id()));
}

#[test]
fn a_space_whose_first_change_must_govern_it_refuses_an_ordinary_one_in_any_order() {
	let mut space = Space::new("ordinary roots", 1);
	let id = space.devices[0].log.space();
	let keys = space.keys.clone();
	let root = space.devices[0]
		.log
		.parcels()
		.infallible()
		.into_iter()
		.find(|parcel| parcel.change.parents().is_empty())
		.expect("the first change");
	let mut stranger = Device::new(&mut space.rng, id);
	let note = space.note("first?");
	let wall = space.tick();
	let ordinary = stranger.commit(&keys, note, wall, &mut space.rng);
	for governed_first in [false, true] {
		let mut log = Log::new(id, Clock::new(DRIFT));
		log.expect_governed();
		if governed_first {
			log.receive(root.clone(), &keys, wall).expect("accepts");
		}
		assert_eq!(
			log.receive(ordinary.clone(), &keys, wall),
			Err(LogError::UngovernedRoot)
		);
		if !governed_first {
			log.receive(root.clone(), &keys, wall).expect("accepts");
		}
		assert!(log.is_governed(&keys).expect("reads"));
	}
}

#[test]
fn a_purge_of_a_secret_that_a_set_aside_change_holds_is_taken_alike() {
	let mut space = Space::new("secret set aside", 3);
	space.keys.insert_secrets(0, space.rng.key());
	let password: nibkeep_model::FieldName = "org.example.password".parse().expect("a field");
	for device in &mut space.devices {
		device.log.set_secret_fields([password.clone()]);
	}
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// The phone seals a secret, which the desk gets and purges; the laptop revokes the
	// phone without having it.
	let secret = add(
		&mut space.rng,
		space.thing,
		password.as_str(),
		text("hunter2"),
	);
	let Operation::Assert { statement, .. } = &secret else {
		unreachable!("an assertion");
	};
	let secret_id = statement.id;
	let sealing = space.commit(1, vec![secret]);
	space.sync(1, 2);
	let wall = space.tick();
	let keys = space.keys.clone();
	let desk = &mut space.devices[2];
	let purge = desk
		.log
		.purge(&desk.key, &keys, [secret_id], wall, &mut space.rng)
		.expect("purges");
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	// The laptop gets the sealing change, which it sets aside, then the purge, which it
	// takes as the desk did.
	space.sync(1, 0);
	assert!(space.set_aside(0, sealing.id()));
	let wall = space.tick();
	space.devices[0]
		.log
		.receive(purge.clone(), &keys, wall)
		.expect("takes the purge");
	assert!(space.devices[0].log.is_purged(secret_id).infallible());
	let note = space.note("after the purge");
	let after = space.commit(2, note);
	space.sync(2, 0);
	assert!(space.devices[0].log.contains(after.id()).infallible());
}

#[test]
fn a_replica_cannot_accept_what_it_keeps_back() {
	let mut space = Space::new("kept back", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	// The phone makes a change, keeps it back, and accepts it in a change it sends.
	let note = space.note("kept back");
	let kept = space.commit(1, note);
	let accepting = space.accepting(1, kept.id());
	let wall = space.tick();
	let keys = space.keys.clone();
	// On what the laptop has, so that the laptop takes it without the change it accepts.
	let heads: Vec<_> = space.devices[0]
		.log
		.heads()
		.infallible()
		.into_iter()
		.collect();
	let mut draft = Draft::new(heads, Timestamp::new(wall, 0), Payload::from(accepting));
	draft.kind = Kind::Governing;
	let phone = &space.devices[1];
	let acceptance = Change::create(phone.log.space(), &keys, &phone.key, &draft, &mut space.rng);
	space.devices[0]
		.log
		.receive(acceptance, &keys, wall)
		.expect("takes the acceptance");
	// The laptop revokes the phone, building on the acceptance, which counts.
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	space.sync(1, 0);
	assert!(space.set_aside(0, kept.id()));
	assert_eq!(space.notes(0), Vec::<String>::new());
}

#[test]
fn a_set_aside_change_that_is_purged_stays_set_aside() {
	let mut space = Space::new("purged aside", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let revoking = space.revoking(1);
	space.commit(0, revoking);
	let note = space.note("unseen");
	let unseen = space.commit(1, note);
	space.sync(1, 0);
	assert!(space.set_aside(0, unseen.id()));
	// A purge of the change, which builds on it, leaves it set aside.
	let heads: Vec<_> = space.devices[0]
		.log
		.heads()
		.infallible()
		.into_iter()
		.collect();
	let wall = space.tick();
	let mut draft = Draft::new(heads, Timestamp::new(wall, 0), Payload::default());
	draft.purges = nibkeep_log::Purges {
		changes: [unseen.id()].into(),
		..nibkeep_log::Purges::default()
	};
	let keys = space.keys.clone();
	let laptop = &mut space.devices[0];
	let purge = Change::create(
		laptop.log.space(),
		&keys,
		&laptop.key,
		&draft,
		&mut space.rng,
	);
	laptop
		.log
		.receive(purge, &keys, wall)
		.expect("takes the purge");
	assert!(space.set_aside(0, unseen.id()));
	assert_eq!(
		space.devices[0].log.set_aside_changes().infallible(),
		[unseen.id()]
	);
}

/// The ID of the statement that an operation asserts.
fn asserted(operation: &Operation) -> nibkeep_base::StatementId {
	match operation {
		Operation::Assert { statement, .. } => statement.id,
		other => panic!("an assertion, not {other:?}"),
	}
}

impl Space {
	/// The governing operations that list device `listed`'s key for its replica, which they
	/// mark as an AI agent's, as its listing does.
	fn ai_listing(&mut self, listed: usize) -> Vec<Operation> {
		self.signed_listing(listed, true)
	}

	/// Whether device `device` takes device `of`'s key for an AI agent's.
	fn is_ai_agent(&self, device: usize, of: usize) -> bool {
		self.devices[device]
			.log
			.is_ai_agent(&self.devices[of].key.verifying_key(), &self.keys)
			.expect("reads")
	}

	/// A space founded by the first of `count` devices, which lists the second as an AI
	/// agent's, and has every device get that.
	fn with_ai_agent(seed: &str, count: usize) -> Self {
		let mut space = Self::new(seed, count);
		let listing = space.ai_listing(1);
		space.commit(0, listing);
		for device in 1..count {
			space.sync(0, device);
		}
		space
	}
}

#[test]
fn the_first_listing_of_a_key_says_whether_it_is_an_ai_agents() {
	let mut space = Space::new("first listing", 3);
	let listing = space.ai_listing(1);
	space.commit(0, listing);
	// Listed again, without the mark: it stays an AI agent's.
	let listing = space.listing(1);
	space.commit(0, listing);
	// Marked only later: it stays a user's.
	let listing = space.listing(2);
	space.commit(0, listing);
	let listing = space.ai_listing(2);
	space.commit(0, listing);
	space.sync(0, 1);
	for device in [0, 1] {
		assert!(space.is_ai_agent(device, 1));
		assert!(!space.is_ai_agent(device, 2));
		assert!(!space.is_ai_agent(device, 0));
	}
}

#[test]
fn an_ai_agents_notes_count_and_its_governing_changes_do_not() {
	let mut space = Space::with_ai_agent("ai agent governs", 3);
	let note = space.note("from the AI agent");
	let note = space.commit(1, note);
	// It lists a replica of its own, and revokes the user's.
	let listing = space.listing(2);
	let listed = space.commit(1, listing);
	let revoking = space.revoking(0);
	let revoking = space.commit(1, revoking);
	space.sync(1, 2);
	let friends = space.note("from its friend");
	let friends = space.commit(2, friends);
	space.sync(1, 0);
	space.sync(2, 0);
	assert!(!space.set_aside(0, note.id()));
	assert!(space.set_aside(0, listed.id()));
	assert!(space.set_aside(0, revoking.id()));
	assert!(
		!space.devices[0].log.contains(friends.id()).infallible(),
		"a key that only an AI agent lists is nobody's"
	);
	assert_eq!(space.notes(0), ["from the AI agent"]);
	// Its own replica decides the same.
	assert!(space.set_aside(1, listed.id()));
	assert!(space.set_aside(1, revoking.id()));
	// The user's replica still writes, and the AI agent takes it.
	let note = space.note("the user's");
	let mine = space.commit(0, note);
	space.sync(0, 1);
	assert!(!space.set_aside(1, mine.id()));
	assert_eq!(space.notes(1), ["from the AI agent", "the user's"]);
}

#[test]
fn an_ai_agents_purge_is_set_aside_and_never_accepted() {
	let mut space = Space::with_ai_agent("ai agent purges", 2);
	let note = space.note("keep me");
	let statement = asserted(&note[0]);
	let kept = space.commit(0, note);
	space.sync(0, 1);
	let wall = space.tick();
	let keys = space.keys.clone();
	let agent = &mut space.devices[1];
	let purge = agent
		.log
		.purge(&agent.key, &keys, [statement], wall, &mut space.rng)
		.expect("purges");
	assert!(
		space.set_aside(1, purge.id()),
		"its own replica sets it aside too"
	);
	assert_eq!(space.notes(1), ["keep me"]);
	space.sync(1, 0);
	assert!(space.set_aside(0, purge.id()));
	assert!(
		!space.devices[0]
			.log
			.is_change_purged(kept.id())
			.infallible()
	);
	assert_eq!(space.notes(0), ["keep me"]);
	let accepting = space.accepting(1, purge.id());
	space.commit(0, accepting);
	assert!(
		space.set_aside(0, purge.id()),
		"a purge by an AI agent is never accepted"
	);
	assert_eq!(space.notes(0), ["keep me"]);
}

#[test]
fn an_ai_agents_secret_is_set_aside() {
	let mut space = Space::with_ai_agent("ai agent seals", 2);
	space.keys.insert_secrets(0, space.rng.key());
	let password: nibkeep_model::FieldName = "org.example.password".parse().expect("a field");
	for device in &mut space.devices {
		device.log.set_secret_fields([password.clone()]);
	}
	let secret = add(
		&mut space.rng,
		space.thing,
		password.as_str(),
		text("hunter2"),
	);
	let sealing = space.commit(1, vec![secret]);
	space.sync(1, 0);
	assert!(space.set_aside(0, sealing.id()));
	assert!(space.set_aside(1, sealing.id()));
	// Nor does it take away a secret value, which it cannot read.
	let human = add(
		&mut space.rng,
		space.thing,
		password.as_str(),
		text("correct horse"),
	);
	let human_id = asserted(&human);
	space.commit(0, vec![human]);
	space.sync(0, 1);
	let taking = space.commit(1, vec![Operation::retract_secret([human_id])]);
	space.sync(1, 0);
	assert!(space.set_aside(0, taking.id()));
	assert!(space.set_aside(1, taking.id()));
	assert!(
		space.devices[0]
			.log
			.state()
			.is_current(human_id)
			.infallible()
	);
}

#[test]
fn an_ai_agent_writes_the_data_and_not_the_librarys_records() {
	let mut space = Space::with_ai_agent("ai agent records", 2);
	let thing = space.thing;
	let cases: [(&str, Value, bool); 13] = [
		(NOTE, text("a note"), true),
		(
			"org.nibkeep.name",
			Value::Name(nibkeep_model::Name::new("A thing")),
			true,
		),
		("org.nibkeep.type", text("org.example.book"), true),
		("org.nibkeep.type", text("std.person"), true),
		("org.nibkeep.type", text("org.nibkeep.membership"), true),
		("org.nibkeep.position", text("a0"), true),
		("org.nibkeep.endpoint", text("AAAA"), false),
		("org.nibkeep.field-secret", Value::Boolean(true), false),
		("org.nibkeep.languages", text("en"), false),
		("org.nibkeep.policy-copies", Value::Integer(1), false),
		("org.nibkeep.something-new", text("?"), false),
		("org.nibkeep.type", text("org.nibkeep.replica"), false),
		(
			"org.nibkeep.type",
			text("org.nibkeep.field-definition"),
			false,
		),
	];
	for (field, value, counts) in cases {
		let operation = add(&mut space.rng, thing, field, value.clone());
		let change = space.commit(1, vec![operation]);
		space.sync(1, 0);
		for device in [0, 1] {
			assert_eq!(
				!space.set_aside(device, change.id()),
				counts,
				"{field} = {value:?}, on device {device}"
			);
		}
	}
	// What it may write it may also delete.
	let deleting = add(
		&mut space.rng,
		thing,
		"org.nibkeep.deleted",
		Value::Boolean(true),
	);
	let deleting = space.commit(1, vec![deleting]);
	space.sync(1, 0);
	assert!(!space.set_aside(0, deleting.id()));
	// A move's note, which history shows as who recorded a value where it came from, only a
	// move writes, and a migration's only a migration; a date and a language it may give.
	for (qualifier, value, counts) in [
		(
			fields::CARRIED_BY,
			text("a key it would put words in the mouth of"),
			false,
		),
		(fields::CARRIED_AT, Value::Integer(1_000), false),
		(
			fields::MIGRATED_FROM,
			text("org.example.split-names 0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10"),
			false,
		),
		(fields::LANGUAGE, text("en"), true),
	] {
		let noted = Operation::assert(
			Statement::new(
				space.rng.statement_id(),
				thing,
				NOTE.parse().expect("a field name"),
				text("noted"),
			)
			.with_qualifiers(nibkeep_model::Qualifiers::new().with(qualifier.clone(), value)),
		);
		let change = space.commit(1, vec![noted]);
		space.sync(1, 0);
		assert_eq!(!space.set_aside(0, change.id()), counts, "{qualifier}");
	}
}

#[test]
fn an_ai_agent_may_take_away_what_it_may_not_set() {
	let mut space = Space::with_ai_agent("ai agent retracts", 2);
	let thing = space.thing;
	let mark = add(
		&mut space.rng,
		thing,
		"org.nibkeep.field-secret",
		Value::Boolean(true),
	);
	let mark_id = asserted(&mark);
	space.commit(0, vec![mark]);
	space.sync(0, 1);
	// It may take the statement away, as it may delete any entity, but not replace it with
	// one it may not write.
	let replacement = Statement::new(
		space.rng.statement_id(),
		thing,
		common::field("org.nibkeep.field-secret"),
		Value::Boolean(false),
	);
	let replacing = space.commit(1, vec![Operation::replace(replacement, [mark_id])]);
	let retracting = space.commit(1, vec![Operation::retract(thing, [mark_id])]);
	space.sync(1, 0);
	for device in [0, 1] {
		assert!(space.set_aside(device, replacing.id()), "{device}");
		assert!(!space.set_aside(device, retracting.id()), "{device}");
		assert!(
			!space.devices[device]
				.log
				.state()
				.is_current(mark_id)
				.infallible(),
			"{device}"
		);
	}
}

const PASSWORD: &str = "org.example.password";

#[test]
fn a_value_written_plainly_counts_only_where_its_fields_mark_builds_on_it() {
	let mut space = Space::new("written plainly", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	// Neither device seals the password, as one that turned against its user would not:
	// written before the field is secret, which the mark builds on, a value counts; written
	// meanwhile by a device that did not know, or after, by one that knew, it does not.
	let password = |space: &mut Space, value: &str| {
		vec![add(&mut space.rng, space.thing, PASSWORD, text(value))]
	};
	let before = password(&mut space, "before");
	let before = space.commit(1, before);
	space.sync(1, 0);
	let marking = space.making_secret(0, PASSWORD);
	space.commit(0, marking);
	let meanwhile = password(&mut space, "meanwhile");
	let meanwhile = space.commit(1, meanwhile);
	space.sync(0, 1);
	let after = password(&mut space, "after");
	let after = space.commit(1, after);
	space.sync_both(0, 1);
	for device in [0, 1] {
		assert!(!space.set_aside(device, before.id()));
		assert!(space.set_aside(device, meanwhile.id()));
		assert!(space.set_aside(device, after.id()));
		assert_eq!(
			texts(&space.devices[device].log, space.thing, PASSWORD),
			["before"]
		);
	}
}

#[test]
fn a_replica_cannot_replace_a_secret_by_leaving_the_mark_out_of_its_parents() {
	let mut space = Space::new("mark left out", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let before = vec![add(&mut space.rng, space.thing, PASSWORD, text("hunter2"))];
	let before_id = asserted(&before[0]);
	let before = space.commit(0, before);
	let marking = space.making_secret(0, PASSWORD);
	space.commit(0, marking);
	space.sync(0, 1);
	// The phone, which turned against its user, replaces the password with one of its own,
	// building only on what came before the field was secret: superseding needs no ancestry.
	let planted = Statement::new(
		space.rng.statement_id(),
		space.thing,
		common::field(PASSWORD),
		text("planted"),
	);
	let wall = space.tick();
	let keys = space.keys.clone();
	let forged = space.devices[1].forge(
		&keys,
		(
			Kind::Ordinary,
			std::collections::BTreeSet::from([before.id()]),
		),
		vec![Operation::replace(planted, [before_id])],
		wall,
		&mut space.rng,
	);
	space.sync_both(0, 1);
	for device in [0, 1] {
		assert!(space.set_aside(device, forged.id()));
		assert_eq!(
			texts(&space.devices[device].log, space.thing, PASSWORD),
			["hunter2"]
		);
	}
}

#[test]
fn a_value_written_before_its_field_was_secret_counts_when_a_purge_states_it_again() {
	let mut space = Space::new("restated plainly", 1);
	let mut written = vec![add(&mut space.rng, space.thing, PASSWORD, text("before"))];
	let note = space.note("purge me");
	let note_id = asserted(&note[0]);
	written.extend(note);
	space.commit(0, written);
	let marking = space.making_secret(0, PASSWORD);
	space.commit(0, marking);
	// The purge builds on the mark, but the password it states again comes from a change
	// that the mark builds on.
	let wall = space.tick();
	let keys = space.keys.clone();
	let device = &mut space.devices[0];
	let purge = device
		.log
		.purge(&device.key, &keys, [note_id], wall, &mut space.rng)
		.expect("purges the note");
	assert!(!space.set_aside(0, purge.id()));
	assert_eq!(
		texts(&space.devices[0].log, space.thing, PASSWORD),
		["before"]
	);
	assert_eq!(space.notes(0), Vec::<String>::new());
}

/// A field that a later version of NibKeep may govern with, which this one does not know.
const LATER: &str = "org.nibkeep.from-a-later-version";

#[test]
fn a_governing_statement_that_this_version_does_not_know_is_kept_with_no_effect() {
	let mut space = Space::new("a later version's statement", 2);
	// A later version lists the phone, and states something of its own in the same change.
	let mut operations = space.listing(1);
	let later = governing(
		&mut space.rng,
		space.replicas[0],
		common::field(LATER),
		text("for a later version"),
	);
	let later_id = asserted(&later);
	operations.push(later);
	let wall = space.tick();
	let keys = space.keys.clone();
	let change =
		space.devices[0].forge_on_heads(&keys, Kind::Governing, operations, wall, &mut space.rng);
	space.sync(0, 1);
	// The listing counts, as the phone's changes then do, and the statement is kept.
	let note = space.note("from the phone");
	let noted = space.commit(1, note);
	space.sync(1, 0);
	for device in [0, 1] {
		assert!(!space.set_aside(device, change.id()), "{device}");
		assert!(!space.set_aside(device, noted.id()), "{device}");
		assert!(
			space.devices[device]
				.log
				.state()
				.is_current(later_id)
				.infallible(),
			"{device}"
		);
	}
	assert_eq!(space.notes(0), ["from the phone"]);
}

#[test]
fn a_governing_change_that_holds_other_statements_still_adds_nothing() {
	// Data, of NibKeep's own known fields or of others, never goes into a change that is
	// never purged.
	for field in ["org.nibkeep.name", NOTE] {
		let mut space = Space::new(&format!("{field} in a governing change"), 2);
		let mut operations = space.listing(1);
		let data = governing(
			&mut space.rng,
			space.thing,
			common::field(field),
			text("data"),
		);
		let data_id = asserted(&data);
		operations.push(data);
		let wall = space.tick();
		let keys = space.keys.clone();
		space.devices[0].forge_on_heads(&keys, Kind::Governing, operations, wall, &mut space.rng);
		let state = space.devices[0].log.state();
		assert_eq!(state.statement(data_id).infallible(), None, "{field}");
	}
}

#[test]
fn a_value_written_plainly_is_set_aside_once_its_field_is_known_to_be_secret() {
	let mut space = Space::new("made secret late", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// The field is made secret; the second device writes plainly after it, and the third
	// gets the change that made it secret without its payload, for now.
	let marking = space.making_secret(0, PASSWORD);
	let marked = space.commit(0, marking);
	space.sync(0, 1);
	let plainly = vec![add(&mut space.rng, space.thing, PASSWORD, text("planted"))];
	let written = space.commit(1, plainly);
	let wall = space.tick();
	let keys = space.keys.clone();
	let mut header = space.devices[0]
		.log
		.parcel(marked.id())
		.infallible()
		.expect("accepted");
	let payload = header.payload.take().expect("a payload");
	let desk = &mut space.devices[2];
	desk.log
		.receive(header, &keys, wall)
		.expect("takes the header");
	let (phone, desk) = pair(&mut space.devices, 1, 2);
	desk.receive_from(phone, &keys, wall);
	assert!(!desk.log.is_complete(marked.id()).infallible());
	assert_eq!(texts(&desk.log, space.thing, PASSWORD), ["planted"]);
	// Once it reads that the field is secret, it decides as the others do.
	desk.log
		.receive_payload(marked.id(), payload, &keys)
		.expect("takes the payload");
	assert!(desk.log.is_complete(marked.id()).infallible());
	assert!(space.set_aside(2, written.id()));
	assert!(space.set_aside(1, written.id()));
	assert_eq!(
		texts(&space.devices[2].log, space.thing, PASSWORD),
		Vec::<String>::new()
	);
}

#[test]
fn a_replica_wraps_every_governing_change_with_the_first_key() {
	let mut space = Space::new("first key", 2);
	let mut newer = space.keys.clone();
	newer.insert(7, space.rng.key());
	newer.write_with(Some(7));
	let wall = space.tick();
	let listing = space.listing(1);
	let governing = space.devices[0].commit(&newer, listing, wall, &mut space.rng);
	assert_eq!(governing.change.epoch(), 0);
	let wall = space.tick();
	let note = space.note("with the newest key");
	let ordinary = space.devices[0].commit(&newer, note, wall, &mut space.rng);
	assert_eq!(ordinary.change.epoch(), 7);
}

#[test]
fn a_governing_change_wrapped_with_another_key_than_the_first_is_refused() {
	let mut space = Space::new("another key", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// A replica that turned against its user makes a key that it gives the desktop alone,
	// and wraps with it a revocation of the phone, which the phone could never read.
	let mut its_own = space.keys.clone();
	its_own.insert(7, space.rng.key());
	let revoking = space.revoking(1);
	let heads = space.devices[2].log.heads().infallible();
	let latest = heads
		.iter()
		.filter_map(|&parent| space.devices[2].log.change(parent).infallible())
		.map(|change| change.timestamp().millis())
		.max()
		.expect("a head");
	let mut draft = Draft::new(
		heads,
		Timestamp::new(latest + 1, 0),
		Payload::from(revoking),
	);
	draft.kind = Kind::Governing;
	let forged = Change::create(
		space.id,
		&its_own,
		&space.devices[2].key,
		&draft,
		&mut space.rng,
	);
	assert_eq!(forged.change.epoch(), 7);
	let wall = space.tick();
	for device in [0, 1] {
		assert!(matches!(
			space.devices[device]
				.log
				.receive(forged.clone(), &its_own, wall),
			Err(LogError::GoverningEpoch)
		));
		assert!(!space.devices[device].log.contains(forged.id()).infallible());
		assert_eq!(
			space.devices[device]
				.log
				.revocations(&its_own)
				.expect("reads"),
			[]
		);
	}
	// What builds on it is rejected with it, and waits for nothing.
	let note = space.note("on the forged one");
	let mut child = Draft::new(
		[forged.id()],
		Timestamp::new(latest + 2, 0),
		Payload::from(note),
	);
	child.kind = Kind::Ordinary;
	let child = Change::create(
		space.id,
		&its_own,
		&space.devices[2].key,
		&child,
		&mut space.rng,
	);
	assert!(matches!(
		space.devices[0].log.receive(child, &its_own, wall),
		Err(LogError::RejectedParent)
	));
	// One that a replica kept from a draft of the formats, which wrapped governing changes
	// with newer keys, still opens.
	space.devices[2]
		.log
		.restore(forged, &its_own, false)
		.expect("restores it");
}

#[test]
fn a_revocation_builds_on_no_governing_change_whose_payload_is_missing() {
	let mut space = Space::new("unread", 3);
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let note = space.note("before");
	let before = space.commit(0, note);
	// The phone gets a governing change without its payload, and what builds on it whole.
	let listing = space.listing(2);
	let unread = space.commit(0, listing);
	let note = space.note("after");
	let after = space.commit(0, note);
	let wall = space.tick();
	let keys = space.keys.clone();
	let phone = &mut space.devices[1].log;
	phone
		.receive(before.clone(), &keys, wall)
		.expect("takes it");
	let mut without = unread.clone();
	without.payload = None;
	phone.receive(without, &keys, wall).expect("takes it");
	phone.receive(after.clone(), &keys, wall).expect("takes it");
	assert!(!phone.is_complete(unread.id()).infallible());
	assert_eq!(phone.heads().infallible(), BTreeSet::from([after.id()]));
	let parents = |parcel: &Parcel| -> BTreeSet<ChangeId> {
		parcel.change.parents().iter().copied().collect()
	};
	// Its revocation builds on what it reads, and on nothing that builds on what it does
	// not; its other changes, governing ones too, build on every change it holds.
	let revoking = space.revoking(0);
	let revocation = space.commit(1, revoking);
	assert_eq!(parents(&revocation), BTreeSet::from([before.id()]));
	let marking = space.making_secret(1, PASSWORD);
	let marked = space.commit(1, marking);
	assert_eq!(
		parents(&marked),
		BTreeSet::from([after.id(), revocation.id()])
	);
	let note = space.note("the phone's");
	let ordinary = space.commit(1, note);
	assert_eq!(parents(&ordinary), BTreeSet::from([marked.id()]));
	// Once it reads it, its revocations build on all again.
	space.sync(0, 1);
	assert!(space.devices[1].log.is_complete(unread.id()).infallible());
	let revoking = space.revoking(2);
	let revocation = space.commit(1, revoking);
	assert_eq!(parents(&revocation), BTreeSet::from([ordinary.id()]));
}

#[test]
fn an_older_replica_sent_its_revocation_without_the_payload_stays_by_revoking_back() {
	let mut space = Space::new("withheld", 3);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	space.sync(0, 1);
	space.sync(0, 2);
	// The tablet, added after the phone, turned against its user: it revokes the phone,
	// sends the desktop that whole, and the phone only its header.
	let revoking = space.revoking(1);
	let revocation = space.commit(2, revoking);
	space.sync(2, 0);
	let wall = space.tick();
	let keys = space.keys.clone();
	let mut without = revocation.clone();
	without.payload = None;
	space.devices[1]
		.log
		.receive(without, &keys, wall)
		.expect("takes it");
	assert!(
		space.devices[0]
			.log
			.is_revoked(&space.devices[1].key.verifying_key(), &keys)
			.expect("reads")
	);
	// The person sees on the desktop that the phone was revoked, and revokes the tablet
	// from the phone, which builds on none of what it cannot read.
	let revoking = space.revoking(2);
	let back = space.commit(1, revoking);
	assert!(!back.change.parents().contains(&revocation.id()));
	space.sync_both(0, 1);
	space.sync_both(0, 2);
	space.sync_both(1, 2);
	for device in 0..3 {
		assert!(space.set_aside(device, revocation.id()), "on {device}");
		assert!(!space.set_aside(device, back.id()), "on {device}");
	}
}

#[test]
fn what_a_change_set_aside_for_writing_a_secret_plainly_purged_comes_back() {
	let mut space = Space::new("plain purge", 4);
	for listed in [1, 2] {
		let listing = space.listing(listed);
		space.commit(0, listing);
	}
	let marking = space.making_secret(0, PASSWORD);
	space.commit(0, marking);
	let note = space.note("keep me");
	let kept = space.commit(0, note);
	space.sync(0, 1);
	space.sync(0, 2);
	// The phone purges the note in a change that also writes a password plainly.
	let phone = &space.devices[1];
	let parents = phone.log.heads().infallible().clone();
	let latest = parents
		.iter()
		.filter_map(|&parent| phone.log.change(parent).infallible())
		.map(|change| change.timestamp())
		.max()
		.expect("a head");
	let mut payload = Payload::default();
	payload
		.operations
		.push(add(&mut space.rng, space.thing, PASSWORD, text("planted")));
	let mut draft = Draft::new(parents, Timestamp::new(latest.millis() + 1, 0), payload);
	draft.purges.changes = std::collections::BTreeSet::from([kept.id()]);
	let forged = Change::create(
		phone.log.space(),
		&space.keys,
		&phone.key,
		&draft,
		&mut space.rng,
	);
	let wall = space.tick();
	let keys = space.keys.clone();
	space.devices[2]
		.log
		.receive(forged.clone(), &keys, wall)
		.expect("takes it");
	let desk = &space.devices[2].log;
	assert!(desk.is_set_aside(forged.id()).infallible());
	assert!(!desk.is_change_purged(kept.id()).infallible());
	// What it destroyed comes back from another replica that has it.
	space.sync(0, 2);
	assert_eq!(texts(&space.devices[2].log, space.thing, NOTE), ["keep me"]);
	// Nor does the purge come back as the space is governed anew, which decides again
	// whether the changes set aside count.
	let listing = space.listing(3);
	space.commit(0, listing);
	space.sync(0, 2);
	let desk = &space.devices[2].log;
	assert!(desk.is_set_aside(forged.id()).infallible());
	assert!(!desk.is_change_purged(kept.id()).infallible());
	assert_eq!(texts(desk, space.thing, NOTE), ["keep me"]);
}

#[test]
fn nibkeep_own_fields_made_secret_are_still_written_plainly() {
	let mut space = Space::new("own fields secret", 2);
	let listing = space.listing(1);
	space.commit(0, listing);
	let marking = space.making_secret(0, "org.nibkeep.name");
	space.commit(0, marking);
	space.sync(0, 1);
	let naming = vec![add(
		&mut space.rng,
		space.thing,
		"org.nibkeep.name",
		text("Ada"),
	)];
	let named = space.commit(1, naming);
	space.sync(1, 0);
	assert!(!space.set_aside(0, named.id()));
}

/// A space with members, as a space other than the personal one is
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing)): its first change gives
/// the founder's member a role, and each replica's record is derived from the space and its
/// key.
struct Members {
	space: Space,
	id: nibkeep_base::SpaceId,
	/// The member whose replicas every device's are.
	member: EntityId,
	/// Each device's encryption key.
	encryptions: Vec<nibkeep_crypto::EncryptionKey>,
}

impl Members {
	fn new(seed: &str, count: usize) -> Self {
		let space = Space::founded(seed, count, Some("admin"));
		let (id, member, encryptions) = (space.id, space.owner, space.encryptions.clone());
		Self {
			space,
			id,
			member,
			encryptions,
		}
	}

	/// The governing operations that list device `listed`'s key on its record, owned by the
	/// member, with the listing that its key signed.
	fn signed_listing(&mut self, listed: usize, ai_agent: bool) -> Vec<Operation> {
		self.space.signed_listing(listed, ai_agent)
	}

	/// The governing operations that list device `listed`'s key on `record`, owned by the
	/// member, without a listing that its key signed.
	fn unsigned_listing(
		&mut self,
		listed: usize,
		record: EntityId,
		ai_agent: bool,
	) -> Vec<Operation> {
		self.space.unsigned_listing(listed, record, ai_agent)
	}
}

#[test]
fn in_a_space_with_members_a_listing_counts_only_with_the_keys_own_signature() {
	let mut members = Members::new("signed listings", 2);
	members.space.sync(0, 1);
	// Listed without its own signature, the phone's key is nobody's: what it writes is
	// refused.
	let record = members.space.replicas[1];
	let unsigned = members.unsigned_listing(1, record, false);
	members.space.commit(0, unsigned);
	members.space.sync(0, 1);
	let note = members.space.note("unsigned");
	let note = members.space.commit(1, note);
	members.space.sync(1, 0);
	assert!(
		!members.space.devices[0]
			.log
			.contains(note.id())
			.infallible()
	);
	// Listed with it, its changes are taken; the one it wrote before it knew of its listing
	// has no member, and does not count, and those it writes once it knows do.
	let signed = members.signed_listing(1, false);
	members.space.commit(0, signed);
	members.space.sync(1, 0);
	assert!(members.space.set_aside(0, note.id()));
	members.space.sync(0, 1);
	let note = members.space.note("signed");
	let note = members.space.commit(1, note);
	members.space.sync(1, 0);
	assert!(!members.space.set_aside(0, note.id()));
	assert_eq!(members.space.notes(0), ["signed"]);
}

#[test]
fn in_a_space_with_members_nobody_marks_or_takes_another_replicas_key() {
	// The laptop founds the space and lists the tablet; the tablet lists the phone's key on
	// a record of its own choosing, as an AI agent's, before the phone is listed itself.
	let mut members = Members::new("taken keys", 4);
	let tablet = members.signed_listing(1, false);
	members.space.commit(0, tablet);
	members.space.sync(0, 1);
	let elsewhere = members.space.rng.entity_id();
	let mut taking = members.unsigned_listing(2, elsewhere, true);
	// The tablet's own listing, put on the record, verifies for nobody else's key.
	let borrowed = nibkeep_governance::Listing::sign(
		&members.space.devices[1].key,
		&members.encryptions[1],
		members.id,
		(members.member, true),
	);
	taking.push(governing(
		&mut members.space.rng,
		elsewhere,
		fields::LISTING,
		Value::Text(borrowed.to_text()),
	));
	members.space.commit(1, taking);
	members.space.sync(1, 0);
	// The laptop lists the phone properly, and the phone lists the desk: a governing change,
	// which counts, as the phone's key is no AI agent's.
	let phone = members.signed_listing(2, false);
	members.space.commit(0, phone);
	members.space.sync(0, 2);
	let desk = members.signed_listing(3, false);
	let desk = members.space.commit(2, desk);
	members.space.sync(2, 0);
	assert!(!members.space.set_aside(0, desk.id()), "the phone governs");
	members.space.sync(0, 3);
	let note = members.space.note("from the desk");
	let note = members.space.commit(3, note);
	members.space.sync(3, 0);
	assert!(!members.space.set_aside(0, note.id()));
	assert_eq!(members.space.notes(0), ["from the desk"]);
}

#[test]
fn in_the_personal_space_too_nobody_marks_or_takes_another_replicas_key() {
	// The tablet lists the phone's key on a record of its own choosing, as an AI agent's,
	// before the phone is listed itself: without the phone's own listing, that lists nothing.
	let mut space = Space::new("marked keys", 4);
	let tablet = space.listing(1);
	space.commit(0, tablet);
	space.sync(0, 1);
	let elsewhere = space.rng.entity_id();
	let marking = space.unsigned_listing(2, elsewhere, true);
	space.commit(1, marking);
	space.sync(1, 0);
	let phone = space.listing(2);
	space.commit(0, phone);
	space.sync(0, 2);
	let desk = space.listing(3);
	let desk = space.commit(2, desk);
	space.sync(2, 0);
	assert!(!space.set_aside(0, desk.id()), "the phone governs");
	assert!(!space.is_ai_agent(0, 2));
}

#[test]
fn in_the_personal_space_too_a_listing_counts_only_with_the_keys_own_signature() {
	let mut space = Space::new("signed personal listings", 2);
	space.sync(0, 1);
	// Listed without its own signature, or on a record not derived from its key, the
	// phone's key is nobody's: what it writes is refused.
	let elsewhere = space.rng.entity_id();
	let record = space.replicas[1];
	for listing in [
		space.unsigned_listing(1, record, false),
		space.unsigned_listing(1, elsewhere, false),
	] {
		space.commit(0, listing);
	}
	space.sync(0, 1);
	let note = space.note("unsigned");
	let note = space.commit(1, note);
	space.sync(1, 0);
	assert!(!space.devices[0].log.contains(note.id()).infallible());
	// With its own listing, it is listed, and what was refused is taken as it comes again.
	let listing = space.listing(1);
	space.commit(0, listing);
	space.sync(0, 1);
	let note = space.note("signed");
	let note = space.commit(1, note);
	space.sync(1, 0);
	assert!(!space.set_aside(0, note.id()));
	assert_eq!(space.notes(0), ["signed", "unsigned"]);
}

#[test]
fn no_replica_states_an_encryption_key_for_another() {
	// The tablet, which turned against its user, has the phone's listing put again, dated
	// before the phone's own, with an encryption key of its own beside it, so that new keys
	// would be sealed to it.
	let mut space = Space::new("planted encryption key", 3);
	let tablet = space.listing(1);
	let tablet = space.commit(0, tablet);
	space.sync(0, 1);
	let phone = space.listing(2);
	space.commit(0, phone);
	let thief = nibkeep_crypto::DecryptionKey::generate(&mut space.rng).encryption_key();
	let record = space.replicas[2];
	let copied = nibkeep_governance::Listing::sign(
		&space.devices[2].key,
		&space.encryptions[2],
		space.id,
		(space.owner, false),
	);
	let key = space.devices[2].key.verifying_key();
	let rng = &mut space.rng;
	let planting = vec![
		governing(rng, record, fields::SIGNING_KEY, Value::Text(key.to_text())),
		governing(rng, record, fields::OWNER, Value::Reference(space.owner)),
		governing(
			rng,
			record,
			fields::ENCRYPTION_KEY,
			Value::Text(nibkeep_governance::encryption_key_text(&thief)),
		),
		governing(rng, record, fields::LISTING, Value::Text(copied.to_text())),
	];
	let wall = space.tick();
	let keys = space.keys.clone();
	space.devices[1].forge(
		&keys,
		(
			Kind::Governing,
			std::collections::BTreeSet::from([tablet.id()]),
		),
		planting,
		wall,
		&mut space.rng,
	);
	space.sync(1, 0);
	let statements = space.devices[0]
		.log
		.governing_statements(&space.keys)
		.expect("reads");
	let roster = nibkeep_governance::Roster::new(space.id, &statements);
	assert_eq!(roster.encryption_of(record), Some(&space.encryptions[2]));
	assert_eq!(
		roster.own_key_of(record),
		Some(&space.devices[2].key.verifying_key())
	);
}

#[test]
fn a_replica_told_which_change_founded_a_space_takes_no_other() {
	let real = Space::new("the real root", 1);
	let keys = real.keys.clone();
	let real_root = real.devices[0]
		.log
		.root()
		.infallible()
		.expect("a first change");
	let parcel = real.devices[0]
		.log
		.parcel(real_root)
		.infallible()
		.expect("the first change");
	let mut rng = TestRng::new("forging");
	let space = parcel.change.space();
	// A first change of the same space that another device made up, as one that turned
	// bad could.
	let mut maker = common::Device::new(&mut rng, space);
	maker.log.expect_governed();
	let key = maker.key.verifying_key();
	let entity = rng.entity_id();
	let forged_root = maker.commit(
		&keys,
		vec![governing(
			&mut rng,
			entity,
			fields::SIGNING_KEY,
			Value::Text(key.to_text()),
		)],
		start(),
		&mut rng,
	);
	let mut device = common::Device::new(&mut rng, space);
	device.log.expect_governed();
	device.log.expect_roots([real_root]);
	// The forged first change is refused, without being rejected for good.
	assert_eq!(
		device
			.log
			.receive(forged_root.clone(), &keys, start() + 60_000)
			.err(),
		Some(LogError::UnexpectedRoot)
	);
	// The real one is taken.
	assert!(device.log.receive(parcel, &keys, start() + 60_000).is_ok());
	assert!(device.log.contains(real_root).infallible());
	// Once it holds its first change, another is rejected as before.
	assert_eq!(
		device
			.log
			.receive(forged_root.clone(), &keys, start() + 60_000)
			.err(),
		Some(LogError::SecondRoot)
	);
	// A replica told of no first change, as when a source it trusts cannot be read, takes
	// none, not even the real one, until it is told of one.
	let parcel = real.devices[0]
		.log
		.parcel(real_root)
		.infallible()
		.expect("the first change");
	let mut device = common::Device::new(&mut rng, space);
	device.log.expect_governed();
	device.log.expect_roots_too([]);
	assert_eq!(
		device
			.log
			.receive(parcel.clone(), &keys, start() + 60_000)
			.err(),
		Some(LogError::UnexpectedRoot)
	);
	// Told of another first change by one source, and then of the real one by another, it
	// keeps both: it takes the real one, and still refuses one that no source named.
	device
		.log
		.expect_roots_too([ChangeId::from_hash(nibkeep_base::Hash::of(b"another"))]);
	device.log.expect_roots_too([real_root]);
	assert_eq!(
		device
			.log
			.receive(forged_root, &keys, start() + 60_000)
			.err(),
		Some(LogError::UnexpectedRoot)
	);
	assert!(device.log.receive(parcel, &keys, start() + 60_000).is_ok());
	assert!(device.log.contains(real_root).infallible());
}
