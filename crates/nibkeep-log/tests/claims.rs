//! Claims to the admin role ([technical design: claims]): when the admins are gone, a member
//! who can edit claims the role. The claim takes effect 30 days after a replica of another
//! member first shows that it saw it, by the claimant's timestamps, unless an admin rejects
//! it, and a rejection counts if it is seen within 30 days after that. For those 30 days the
//! new admin can only undo: no purge, acceptance, rejection of claims, new admin, or lowering
//! an admin from before.
//!
//! The people: Olga, the space's first admin, who goes away; Ivan, an editor, who claims;
//! Vera, another editor; and Nina, a viewer.
//!
//! [technical design: claims]: ../../../docs/Technical-design.md#139-claims-to-the-admin-role

mod common;

use common::{DRIFT, counts_in, pair, send, text, texts};
use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::{DecryptionKey, EncryptionKey, SigningKey};
use nibkeep_governance::replica_record;
use nibkeep_log::{Log, Parcel, SpaceKeys};
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Clock;

const CAPTION: &str = "org.example.caption";
const DAY: u64 = 86_400_000;

const OLGA: usize = 0;
const IVAN: usize = 1;
const VERA: usize = 2;
const NINA: usize = 3;
const PEOPLE: usize = 4;

/// A device of one person's, with its keys and its copy of the space's log.
struct Device {
	key: SigningKey,
	encryption: EncryptionKey,
	person: usize,
	log: Log,
}

/// The family's space and the devices that hold it. Device 0, Olga's, founds it.
struct Family {
	rng: TestRng,
	now: u64,
	id: SpaceId,
	keys: SpaceKeys,
	members: Vec<EntityId>,
	devices: Vec<Device>,
	photo: EntityId,
}

impl Family {
	/// Devices of the people that `people` names, one for each; the first, Olga's, founds
	/// the space, as its admin.
	fn new(seed: &str, people: &[usize]) -> Self {
		assert_eq!(people[0], OLGA);
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let members = (0..PEOPLE).map(|_| rng.entity_id()).collect();
		let devices = people
			.iter()
			.map(|&person| {
				let mut log = Log::new(id, Clock::new(DRIFT));
				log.expect_governed();
				Device {
					key: rng.signing_key(),
					encryption: DecryptionKey::generate(&mut rng).encryption_key(),
					person,
					log,
				}
			})
			.collect();
		let photo = rng.entity_id();
		let mut family = Self {
			rng,
			now: millis_at("2026-11-07", 9, 0),
			id,
			keys,
			members,
			devices,
			photo,
		};
		let mut founding = family.making_member(OLGA, "admin");
		founding.extend(family.listing(0, false));
		family.commit(0, founding);
		family
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	/// Lets `days` days pass.
	fn wait(&mut self, days: u64) {
		self.now += days * DAY;
	}

	fn assert(&mut self, entity: EntityId, field: FieldName, value: Value) -> Operation {
		common::assert(&mut self.rng, entity, field, value)
	}

	fn making_member(&mut self, person: usize, role: &str) -> Vec<Operation> {
		common::making_member(&mut self.rng, self.members[person], role)
	}

	fn listing(&mut self, device: usize, ai_agent: bool) -> Vec<Operation> {
		let device = &self.devices[device];
		let member = (self.members[device.person], ai_agent);
		common::listing(
			&mut self.rng,
			self.id,
			(&device.key, &device.encryption),
			member,
		)
	}

	fn commit(&mut self, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.keys.clone();
		let device = &mut self.devices[device];
		let key = device.key.clone();
		device
			.log
			.commit(&key, &keys, operations, wall, &mut self.rng)
			.expect("commits")
	}

	/// Device `by` invites device `device`'s person with `role`.
	fn invite(&mut self, by: usize, device: usize, role: &str) -> Parcel {
		let mut operations = self.making_member(self.devices[device].person, role);
		operations.extend(self.listing(device, false));
		let parcel = self.commit(by, operations);
		self.sync(by, device);
		parcel
	}

	/// Device `by` lists device `device`, a replica of a person who has a record already.
	fn list(&mut self, by: usize, device: usize, ai_agent: bool) -> Parcel {
		let operations = self.listing(device, ai_agent);
		self.commit(by, operations)
	}

	/// Device `by` gives a person a role.
	fn give_role(&mut self, by: usize, person: usize, role: &str) -> Parcel {
		let record = self.members[person];
		let operation = self.assert(record, fields::ROLE, Value::Text(role.to_owned()));
		self.commit(by, vec![operation])
	}

	/// Device `by` claims the admin role for its person.
	fn claim(&mut self, by: usize) -> Parcel {
		let record = self.members[self.devices[by].person];
		let operation = self.assert(record, fields::CLAIM, Value::Boolean(true));
		self.commit(by, vec![operation])
	}

	/// Device `by` shows that it saw a change, a claim or a rejection.
	fn see(&mut self, by: usize, change: &Parcel) -> Parcel {
		let record = replica_record(self.id, &self.devices[by].key.verifying_key());
		let operation = self.assert(record, fields::SEEN, Value::Text(change.id().to_string()));
		self.commit(by, vec![operation])
	}

	/// Device `by` rejects a claim.
	fn reject(&mut self, by: usize, claim: &Parcel) -> Parcel {
		let record = self.members[IVAN];
		let operation = self.assert(
			record,
			fields::REJECTED,
			Value::Text(claim.id().to_string()),
		);
		self.commit(by, vec![operation])
	}

	/// Device `by` captions the photo.
	fn caption(&mut self, by: usize, caption: &str) -> Parcel {
		let operation = Operation::assert(Statement::new(
			self.rng.statement_id(),
			self.photo,
			FieldName::from_static(CAPTION),
			text(caption),
		));
		self.commit(by, vec![operation])
	}

	/// Device `by` purges a statement.
	fn purge(&mut self, by: usize, statement: StatementId) -> Parcel {
		let (wall, keys) = (self.tick(), self.keys.clone());
		let device = &mut self.devices[by];
		let key = device.key.clone();
		device
			.log
			.purge(&key, &keys, [statement], wall, &mut self.rng)
			.expect("makes the purge")
	}

	/// The statement that a change of one operation asserts.
	fn statement_of(&self, change: &Parcel) -> StatementId {
		let payload = self.devices[0]
			.log
			.payload(change.id(), &self.keys)
			.expect("reads")
			.expect("holds it");
		let Operation::Assert { statement, .. } = &payload.operations[0] else {
			unreachable!("an assertion");
		};
		statement.id
	}

	/// Device `to` receives every change of device `from`, parents first.
	fn sync(&mut self, from: usize, to: usize) {
		let wall = self.tick();
		let (sender, receiver) = pair(&mut self.devices, from, to);
		send(&sender.log, &mut receiver.log, &self.keys, wall);
	}

	/// The devices in `among` receive what each other has.
	fn sync_among(&mut self, among: &[usize]) {
		for _ in 0..2 {
			for &from in among {
				for &to in among {
					if from != to {
						self.sync(from, to);
					}
				}
			}
		}
	}

	/// Every device receives what every other has.
	fn sync_all(&mut self) {
		let all: Vec<usize> = (0..self.devices.len()).collect();
		self.sync_among(&all);
	}

	/// Whether a change counts on every device that holds it, which must agree.
	fn counts(&self, change: &Parcel) -> bool {
		counts_in(self.devices.iter().map(|device| &device.log), change)
	}

	/// The captions that a device shows, sorted.
	fn captions(&self, of: usize) -> Vec<String> {
		let mut captions = texts(&self.devices[of].log, self.photo, CAPTION);
		captions.sort();
		captions
	}

	/// The role that a person has by every governing change, as device 0 decides it.
	fn role_of(&self, person: usize) -> String {
		self.devices[0]
			.log
			.member_roles(&self.keys)
			.expect("reads")
			.get(&self.members[person])
			.map_or_else(|| "nothing".to_owned(), |role| role.as_text().to_owned())
	}
}

/// Olga's space, with Ivan and Vera as editors and Nina as a viewer, if they have devices.
fn family(seed: &str, people: &[usize]) -> Family {
	let mut family = Family::new(seed, people);
	for (device, &person) in people.iter().enumerate().skip(1) {
		let role = if person == NINA { "viewer" } else { "editor" };
		family.invite(0, device, role);
	}
	family.sync_all();
	family
}

#[test]
fn a_claim_takes_effect_thirty_days_after_another_members_device_saw_it() {
	let mut family = family("taking effect", &[OLGA, IVAN, VERA]);
	// Olga goes away; Ivan claims, and acts as an admin at once: that does not count.
	let claim = family.claim(1);
	let at_once = family.give_role(1, VERA, "viewer");
	family.wait(10);
	// Vera's device sees the claim ten days later.
	family.sync(1, 2);
	let seen = family.see(2, &claim);
	family.sync(2, 1);
	family.wait(25);
	let too_soon = family.give_role(1, VERA, "viewer");
	family.wait(6);
	let in_time = family.give_role(1, VERA, "viewer");
	family.sync_among(&[1, 2]);
	assert!(family.counts(&claim));
	assert!(family.counts(&seen));
	assert!(!family.counts(&at_once));
	assert!(!family.counts(&too_soon));
	assert!(family.counts(&in_time), "31 days after it was seen");
	// It adds an admin and removes nobody: Olga is one still.
	family.sync_all();
	assert_eq!(family.role_of(OLGA), "admin");
}

#[test]
fn for_thirty_more_days_the_new_admin_can_only_undo() {
	let mut family = family("only undoing", &[OLGA, IVAN, VERA]);
	let veras = family.caption(2, "Vera's");
	family.sync_all();
	let claim = family.claim(1);
	family.sync(1, 2);
	family.see(2, &claim);
	family.sync(2, 1);
	family.wait(31);
	// No new admin, no lowering of Olga, no purge of another's statement.
	let new_admin = family.give_role(1, VERA, "admin");
	let lowering_olga = family.give_role(1, OLGA, "editor");
	let statement = family.statement_of(&veras);
	let purging = family.purge(1, statement);
	// Lowering Vera, an editor, is undoing, which counts.
	let lowering_vera = family.give_role(1, VERA, "viewer");
	family.sync_among(&[1, 2]);
	assert!(!family.counts(&new_admin));
	assert!(!family.counts(&lowering_olga));
	assert!(!family.counts(&purging));
	assert!(family.counts(&lowering_vera));
	// Once the 30 days are over, the claim is settled.
	family.wait(30);
	let later_admin = family.give_role(1, VERA, "admin");
	family.sync_among(&[1, 2]);
	assert!(family.counts(&later_admin));
}

#[test]
fn a_claim_that_nobody_else_saw_never_takes_effect() {
	// Ivan's tablet, the third device, sees his claim, but it is his own.
	let mut family = family("unseen", &[OLGA, IVAN, IVAN]);
	family.list(1, 2, false);
	family.sync_all();
	let claim = family.claim(1);
	family.sync(1, 2);
	family.see(2, &claim);
	family.sync(2, 1);
	// Seventy days on, his claim would be settled, had anybody else seen it.
	family.wait(70);
	let caption = family.caption(1, "an editor's");
	let as_admin = family.give_role(1, OLGA, "editor");
	family.sync_among(&[1, 2]);
	assert!(family.counts(&caption));
	assert!(!family.counts(&as_admin));
	assert_eq!(family.captions(1), ["an editor's"]);
}

#[test]
fn a_claim_dated_back_takes_effect_no_earlier() {
	let mut family = family("dated back", &[OLGA, IVAN, VERA]);
	// Ivan claims on a device that nobody syncs with for 40 days.
	let claim = family.claim(1);
	family.wait(40);
	family.sync(1, 2);
	family.see(2, &claim);
	family.sync(2, 1);
	family.wait(10);
	// Fifty days after the claim, but ten after it was seen.
	let early = family.give_role(1, VERA, "viewer");
	family.wait(21);
	let late = family.give_role(1, VERA, "viewer");
	family.sync_among(&[1, 2]);
	assert!(!family.counts(&early));
	assert!(family.counts(&late));
}

#[test]
fn an_admin_who_returns_in_time_rejects_the_claim_and_what_the_claimant_did_as_admin() {
	let mut family = family("rejected", &[OLGA, IVAN, VERA]);
	let claim = family.claim(1);
	family.sync(1, 2);
	family.see(2, &claim);
	family.sync(2, 1);
	family.wait(31);
	let as_admin = family.give_role(1, VERA, "viewer");
	let as_editor = family.caption(1, "Ivan's");
	family.sync_among(&[1, 2]);
	assert!(family.counts(&as_admin));
	// Olga returns 20 days into the new admin's first 30, and rejects the claim; Vera sees
	// it at once.
	family.wait(20);
	family.sync(1, 0);
	let rejection = family.reject(0, &claim);
	family.sync(0, 2);
	family.see(2, &rejection);
	family.sync_all();
	assert!(family.counts(&rejection));
	assert!(!family.counts(&as_admin));
	assert!(family.counts(&as_editor));
	assert_eq!(family.role_of(VERA), "editor");
}

#[test]
fn a_rejection_counts_if_it_is_seen_within_thirty_days_after_the_claim_took_effect() {
	for (days, counts) in [(29, true), (31, false)] {
		let mut family = family(&format!("late rejection {days}"), &[OLGA, IVAN, VERA]);
		let claim = family.claim(1);
		family.sync(1, 2);
		family.see(2, &claim);
		family.sync(2, 1);
		family.wait(31);
		let as_admin = family.give_role(1, VERA, "viewer");
		family.sync_among(&[1, 2]);
		// Olga's rejection is dated early, as her device was offline, but is seen late.
		family.sync(1, 0);
		let rejection = family.reject(0, &claim);
		family.wait(days - 1);
		family.sync(0, 2);
		family.see(2, &rejection);
		family.sync_all();
		assert_eq!(
			family.counts(&as_admin),
			!counts,
			"a rejection seen {days} days after the claim took effect"
		);
	}
}

#[test]
fn concurrent_claims_each_take_effect() {
	let mut family = family("both claim", &[OLGA, IVAN, VERA, NINA]);
	let ivans = family.claim(1);
	let veras = family.claim(2);
	family.sync_among(&[1, 2, 3]);
	family.see(3, &ivans);
	family.see(3, &veras);
	family.sync_among(&[1, 2, 3]);
	family.wait(31);
	let by_ivan = family.give_role(1, NINA, "editor");
	family.sync_among(&[1, 2, 3]);
	let by_vera = family.give_role(2, NINA, "viewer");
	family.sync_among(&[1, 2, 3]);
	assert!(family.counts(&by_ivan));
	assert!(family.counts(&by_vera));
}

#[test]
fn viewers_and_ai_agents_cannot_claim() {
	// Ivan's AI agent is the fourth device.
	let mut family = family("who claims", &[OLGA, IVAN, VERA, NINA, IVAN]);
	let olga = family.members[OLGA];
	let allowing = family.assert(olga, fields::AI_AGENTS, Value::Boolean(true));
	family.commit(0, vec![allowing]);
	family.list(1, 4, true);
	family.sync_all();
	let by_nina = family.claim(3);
	let by_agent = family.claim(4);
	family.sync_all();
	assert!(!family.counts(&by_nina));
	assert!(!family.counts(&by_agent));
}

#[test]
fn every_device_decides_claims_alike_after_random_changes_and_syncs() {
	for seed in 0..6 {
		let mut family = family(&format!("random {seed}"), &[OLGA, IVAN, VERA, NINA]);
		let mut changes = Vec::new();
		let mut claims: Vec<Parcel> = Vec::new();
		for step in 0..30 {
			let pick = |family: &mut Family, count: usize| {
				usize::from(family.rng.space_id().as_bytes()[0]) % count
			};
			let device = pick(&mut family, 4);
			match pick(&mut family, 8) {
				0 => claims.push(family.claim(device)),
				1 => {
					if let Some(claim) = claims.get(pick(&mut family, claims.len().max(1))) {
						let claim = claim.clone();
						changes.push(family.see(device, &claim));
					}
				}
				2 => {
					if let Some(claim) = claims.first() {
						let claim = claim.clone();
						changes.push(family.reject(device, &claim));
					}
				}
				3 => {
					let person = [IVAN, VERA, NINA][pick(&mut family, 3)];
					let role = ["viewer", "editor", "admin"][pick(&mut family, 3)];
					changes.push(family.give_role(device, person, role));
				}
				4 => changes.push(family.caption(device, &format!("step {step}"))),
				5 => {
					let days = 1 + 10 * u64::try_from(pick(&mut family, 4)).expect("small");
					family.wait(days);
				}
				_ => {
					let other = (device + 1 + pick(&mut family, 3)) % 4;
					family.sync(device, other);
				}
			}
		}
		family.sync_all();
		for change in changes.iter().chain(&claims) {
			family.counts(change);
		}
		let shown = family.captions(0);
		for device in 1..4 {
			assert_eq!(family.captions(device), shown, "seed {seed}");
		}
	}
}

#[test]
fn a_claimant_removed_or_lowered_below_an_editor_gets_nothing_from_the_claim() {
	for role in ["none", "viewer"] {
		let mut family = family(&format!("claimant {role}"), &[OLGA, IVAN, VERA]);
		let veras = family.caption(2, "Vera's");
		family.sync_all();
		let claim = family.claim(1);
		family.sync_all();
		family.see(2, &claim);
		// Olga, back before the claim takes effect, lowers Ivan instead of rejecting it.
		family.give_role(0, IVAN, role);
		family.sync_all();
		family.wait(61);
		let as_admin = family.give_role(1, VERA, "viewer");
		let statement = family.statement_of(&veras);
		let purging = family.purge(1, statement);
		family.sync_all();
		assert!(!family.counts(&as_admin), "Ivan as {role}");
		assert!(!family.counts(&purging), "Ivan as {role}");
		assert_eq!(family.role_of(IVAN), role);
	}
}

#[test]
fn a_claim_whose_claimant_withdraws_it_gives_nothing() {
	let mut family = family("withdrawn", &[OLGA, IVAN, VERA]);
	let claim = family.claim(1);
	family.sync(1, 2);
	family.see(2, &claim);
	family.sync(2, 1);
	// Ivan changes his mind, and takes his claim back.
	let statement = family.statement_of_on(1, &claim);
	let record = family.members[IVAN];
	let withdrawing = Operation::retract(record, [statement]);
	family.commit(1, vec![withdrawing]);
	family.wait(31);
	let as_admin = family.give_role(1, VERA, "viewer");
	family.sync_among(&[1, 2]);
	assert!(!family.counts(&as_admin));
}

impl Family {
	/// The statement that a change of one operation asserts, as device `on` reads it.
	fn statement_of_on(&self, on: usize, change: &Parcel) -> StatementId {
		let payload = self.devices[on]
			.log
			.payload(change.id(), &self.keys)
			.expect("reads")
			.expect("holds it");
		let Operation::Assert { statement, .. } = &payload.operations[0] else {
			unreachable!("an assertion");
		};
		statement.id
	}
}
