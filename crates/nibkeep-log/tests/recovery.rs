//! A device that lists itself by a recovery code, after its user lost their other devices:
//! its listing counts where a counting change it builds on names the code, and no counting
//! revocation of the code is unknown to it; it counts as added then, and loses to a device in
//! hand ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use crate::common::{Device, add, assert as asserting, pair, send, text, texts};
use nibkeep_base::EntityId;
use nibkeep_crypto::{DecryptionKey, EncryptionKey, RecoveryCode};
use nibkeep_governance::{
	RecoveryListing, RecoveryStatement, encryption_key_text, list_by_recovery_code,
	recovery_record, revoke_recovery_code, state_recovery_code,
};
use nibkeep_log::{ChangeId, Parcel, SpaceKeys};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{Operation, Value, fields};
use nibkeep_testkit::{TestRng, millis_at};

const NOTE: &str = "org.example.note";

/// A personal space of one user, founded by the first device, which lists only itself, and
/// a recovery code of the user's.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	devices: Vec<Device>,
	encryption: Vec<EncryptionKey>,
	replicas: Vec<EntityId>,
	user: EntityId,
	code: RecoveryCode,
	thing: EntityId,
	now: u64,
}

impl Space {
	fn new(seed: &str, count: usize) -> Self {
		let mut rng = TestRng::new(seed);
		let id = rng.space_id();
		let keys = SpaceKeys::new(0, rng.key());
		let mut devices: Vec<Device> = (0..count).map(|_| Device::new(&mut rng, id)).collect();
		for device in &mut devices {
			device.log.expect_governed();
		}
		let encryption = (0..count)
			.map(|_| DecryptionKey::generate(&mut rng).encryption_key())
			.collect();
		let replicas = devices
			.iter()
			.map(|device| nibkeep_governance::replica_record(id, &device.key.verifying_key()))
			.collect();
		let user = rng.entity_id();
		let code = RecoveryCode::generate(&mut rng);
		let thing = rng.entity_id();
		let mut space = Self {
			rng,
			keys,
			devices,
			encryption,
			replicas,
			user,
			code,
			thing,
			now: millis_at("2026-11-13", 9, 0),
		};
		let listing = space.listing(0);
		space.commit(0, listing);
		space
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	/// The governing operations that list device `listed`'s key for its replica, owned by
	/// the user, with its encryption key and the listing that its key signed.
	fn listing(&mut self, listed: usize) -> Vec<Operation> {
		let record = self.replicas[listed];
		let key = self.devices[listed].key.verifying_key().to_text();
		let encryption = encryption_key_text(&self.encryption[listed]);
		let signed = nibkeep_governance::Listing::sign(
			&self.devices[listed].key,
			&self.encryption[listed],
			self.devices[listed].log.space(),
			(self.user, false),
		);
		vec![
			asserting(&mut self.rng, record, fields::SIGNING_KEY, Value::Text(key)),
			asserting(
				&mut self.rng,
				record,
				fields::ENCRYPTION_KEY,
				Value::Text(encryption),
			),
			asserting(
				&mut self.rng,
				record,
				fields::OWNER,
				Value::Reference(self.user),
			),
			asserting(
				&mut self.rng,
				record,
				fields::LISTING,
				Value::Text(signed.to_text()),
			),
		]
	}

	/// The governing operations by which device `listed` lists itself by `code`, as the
	/// listing that the code signs says.
	fn by_code(&mut self, listed: usize, code: &RecoveryCode) -> Vec<Operation> {
		let signed = self.signed_by(listed, code);
		self.listing_with(listed, &signed)
	}

	/// The listing of device `listed` that `code` signs, for the user, on its record.
	fn signed_by(&self, listed: usize, code: &RecoveryCode) -> RecoveryListing {
		RecoveryListing::sign(
			code,
			(
				self.devices[listed].log.space(),
				self.replicas[listed],
				self.user,
			),
			&self.devices[listed].key.verifying_key(),
			&self.encryption[listed],
		)
	}

	/// The governing operations that list device `listed` with the listing `signed`.
	fn listing_with(&mut self, listed: usize, signed: &RecoveryListing) -> Vec<Operation> {
		let mut operations = self.listing(listed);
		operations.push(list_by_recovery_code(signed, &mut self.rng));
		operations
	}

	/// The governing operations that name `code` for the user.
	fn naming(&mut self, code: &RecoveryCode) -> Vec<Operation> {
		let space = self.devices[0].log.space();
		let statement = RecoveryStatement::sign(code, self.user);
		vec![state_recovery_code(space, &statement, &mut self.rng)]
	}

	/// The governing operations that revoke `code` on its record.
	fn revoking_code(&mut self, code: &RecoveryCode) -> Vec<Operation> {
		let key = code.signing_key().verifying_key();
		let record = recovery_record(self.devices[0].log.space(), self.user, &key);
		vec![revoke_recovery_code(record, &key, &mut self.rng)]
	}

	/// The governing operations that revoke device `revoked`'s replica, by its key.
	fn revoking(&mut self, revoked: usize) -> Vec<Operation> {
		let key = self.devices[revoked].key.verifying_key();
		vec![asserting(
			&mut self.rng,
			self.replicas[revoked],
			fields::REVOKED,
			Value::Text(key.to_text()),
		)]
	}

	fn note(&mut self, note: &str) -> Vec<Operation> {
		vec![add(&mut self.rng, self.thing, NOTE, text(note))]
	}

	fn commit(&mut self, device: usize, operations: Vec<Operation>) -> Parcel {
		let wall = self.tick();
		let keys = self.keys.clone();
		self.devices[device].commit(&keys, operations, wall, &mut self.rng)
	}

	/// Device `to` receives every change of device `from` that it takes, parents first, as a
	/// sync sends them, and every wrapped record key.
	fn sync(&mut self, from: usize, to: usize) {
		let wall = self.tick();
		let (from, to) = pair(&mut self.devices, from, to);
		send(&from.log, &mut to.log, &self.keys, wall);
		to.receive_keys_from(from, &self.keys);
	}

	fn sync_both(&mut self, a: usize, b: usize) {
		self.sync(a, b);
		self.sync(b, a);
	}

	fn holds(&self, device: usize, change: &Parcel) -> bool {
		self.devices[device].log.contains(change.id()).infallible()
	}

	fn counts(&self, device: usize, change: &Parcel) -> bool {
		self.holds(device, change)
			&& !self.devices[device]
				.log
				.is_set_aside(change.id())
				.infallible()
	}

	fn notes(&self, device: usize) -> Vec<String> {
		let mut notes = texts(&self.devices[device].log, self.thing, NOTE);
		notes.sort();
		notes
	}

	/// The changes that device `device` holds.
	fn ids(&self, device: usize) -> Vec<ChangeId> {
		self.devices[device]
			.log
			.summaries()
			.infallible()
			.into_iter()
			.map(|summary| summary.id)
			.collect()
	}
}

#[test]
fn a_device_lists_itself_by_a_code_that_the_space_names() {
	let mut space = Space::new("listed by a code", 2);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	let note = space.note("from the old laptop");
	space.commit(0, note);
	// The new device takes what a backup holds, and lists itself by the code.
	space.sync(0, 1);
	let listing = space.by_code(1, &code);
	let listed = space.commit(1, listing);
	let note = space.note("from the new laptop");
	let written = space.commit(1, note);
	let revoking = space.revoking(0);
	let revocation = space.commit(1, revoking);
	for change in [&listed, &written, &revocation] {
		assert!(space.counts(1, change));
	}
	assert_eq!(
		space.notes(1),
		["from the new laptop", "from the old laptop"]
	);
	// The old device takes the new one's listing, which a stranger's would not be, and with
	// it the rest: it sees itself revoked.
	space.sync(1, 0);
	for change in [&listed, &written, &revocation] {
		assert!(space.counts(0, change), "on the old device");
	}
	let late = space.note("written after the revocation");
	let late = space.commit(0, late);
	assert!(!space.counts(0, &late));
	space.sync(0, 1);
	assert!(!space.counts(1, &late));
}

#[test]
fn a_listing_by_a_code_that_nothing_names_lists_nothing() {
	let mut space = Space::new("an unnamed code", 2);
	let code = space.code.clone();
	space.sync(0, 1);
	let listing = space.by_code(1, &code);
	let listed = space.commit(1, listing);
	let note = space.note("from a stranger");
	let written = space.commit(1, note);
	assert!(!space.counts(1, &listed));
	assert!(!space.counts(1, &written));
	// The device that holds the space takes nothing of it: strangers fill nobody's log.
	space.sync(1, 0);
	assert!(!space.holds(0, &listed));
	assert!(!space.holds(0, &written));
}

#[test]
fn a_code_named_where_the_listing_does_not_build_on_it_lists_nothing() {
	let mut space = Space::new("named elsewhere", 2);
	let code = space.code.clone();
	space.sync(0, 1);
	// The code is named while the new device lists itself, apart.
	let naming = space.naming(&code);
	space.commit(0, naming);
	let listing = space.by_code(1, &code);
	let listed = space.commit(1, listing);
	space.sync_both(0, 1);
	assert!(
		space.holds(0, &listed),
		"taken, as the old device names the code"
	);
	assert!(!space.counts(0, &listed));
	assert!(!space.counts(1, &listed));
}

#[test]
fn a_codes_signature_for_another_space_record_key_or_owner_lists_nothing() {
	let mut space = Space::new("other signatures", 3);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	space.sync(0, 1);
	space.sync(0, 2);
	let other_space = space.rng.space_id();
	let other_entity = space.rng.entity_id();
	let other_key = space.devices[2].key.verifying_key();
	let changed: [&dyn Fn(&mut RecoveryListing); 4] = [
		&|listing| listing.space = other_space,
		&|listing| listing.record = other_entity,
		&|listing| listing.key = other_key,
		&|listing| listing.owner = other_entity,
	];
	for change in changed {
		// Signed by the code for what it was changed to.
		let mut forged = space.signed_by(1, &code);
		change(&mut forged);
		let forged = RecoveryListing::sign(
			&code,
			(forged.space, forged.record, forged.owner),
			&forged.key,
			&space.encryption[1],
		);
		let listing = space.listing_with(1, &forged);
		let listed = space.commit(1, listing);
		assert!(!space.counts(1, &listed));
	}
	// Another code's signature, of a code that nothing names.
	let other = RecoveryCode::generate(&mut space.rng);
	let listing = space.by_code(1, &other);
	let listed = space.commit(1, listing);
	assert!(!space.counts(1, &listed));
	// Nor does a listing that states another encryption key, or marks an AI agent's replica.
	let mut listing = space.by_code(1, &code);
	let record = space.replicas[1];
	let planted = encryption_key_text(&space.encryption[2]);
	listing.push(asserting(
		&mut space.rng,
		record,
		fields::ENCRYPTION_KEY,
		Value::Text(planted),
	));
	let listed = space.commit(1, listing);
	assert!(!space.counts(1, &listed));
	let mut listing = space.by_code(1, &code);
	listing.push(asserting(
		&mut space.rng,
		record,
		fields::AI_AGENT,
		Value::Boolean(true),
	));
	let listed = space.commit(1, listing);
	assert!(!space.counts(1, &listed));
	// The listing that the code signed for this device counts.
	let listing = space.by_code(1, &code);
	let listed = space.commit(1, listing);
	assert!(space.counts(1, &listed));
}

#[test]
fn a_listing_made_while_its_code_was_revoked_is_set_aside() {
	let mut space = Space::new("revoked at once", 3);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	space.sync(0, 1);
	space.sync(0, 2);
	// The old device revokes the code, while the new one lists itself by it.
	let revoking = space.revoking_code(&code);
	space.commit(0, revoking);
	let listing = space.by_code(1, &code);
	let listed = space.commit(1, listing);
	space.sync_both(0, 1);
	assert!(!space.counts(0, &listed));
	assert!(!space.counts(1, &listed));
	// A revocation of the code that builds on a listing leaves that listing standing.
	let listing = space.by_code(2, &code);
	let before = space.commit(2, listing);
	let mut fresh = Space::new("revoked after", 2);
	let code = fresh.code.clone();
	let naming = fresh.naming(&code);
	fresh.commit(0, naming);
	fresh.sync(0, 1);
	let listing = fresh.by_code(1, &code);
	let listed = fresh.commit(1, listing);
	let revoking = fresh.revoking_code(&code);
	fresh.commit(1, revoking);
	fresh.sync_both(0, 1);
	assert!(fresh.counts(0, &listed));
	assert!(fresh.counts(1, &listed));
	space.sync(0, 2);
	assert!(
		!space.counts(2, &before),
		"its code was revoked as it listed"
	);
}

#[test]
fn a_device_in_hand_beats_one_listed_by_a_code() {
	// The old laptop founded the space and made the code.
	let mut space = Space::new("in hand", 4);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	space.sync(0, 1);
	// Someone who found the paper lists a device by the code, and has it list another.
	let listing = space.by_code(1, &code);
	let found = space.commit(1, listing);
	let listing = space.listing(3);
	let listed = space.commit(1, listing);
	space.sync(1, 3);
	// The user, unaware, adds a phone: it is newer than both.
	let listing = space.listing(2);
	space.commit(0, listing);
	space.sync(0, 2);
	// The phone, in hand, sees them, and revokes both, while they revoke the phone.
	space.sync(1, 2);
	let revoking = space.revoking(2);
	let by_found = space.commit(1, revoking);
	let revoking = space.revoking(2);
	let by_listed = space.commit(3, revoking);
	let mut revoking = space.revoking(1);
	revoking.extend(space.revoking(3));
	let by_phone = space.commit(2, revoking);
	for (a, b) in [(1, 2), (3, 2), (2, 0), (1, 0), (3, 0), (0, 1)] {
		space.sync_both(a, b);
	}
	let mut all = space.ids(0);
	all.sort();
	for device in 0..4 {
		for change in [&found, &listed, &by_phone] {
			assert!(space.counts(device, change), "on {device}");
		}
		assert!(!space.counts(device, &by_found), "on {device}");
		assert!(!space.counts(device, &by_listed), "on {device}");
		let mut held = space.ids(device);
		held.sort();
		assert_eq!(held, all, "every device holds the same");
	}
}

#[test]
fn of_two_devices_listed_by_codes_the_first_listed_stays() {
	let mut space = Space::new("two codes", 3);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	space.sync(0, 1);
	space.sync(0, 2);
	let listing = space.by_code(1, &code);
	let first = space.commit(1, listing);
	let listing = space.by_code(2, &code);
	let second = space.commit(2, listing);
	// Each sees the other's listing, then they revoke each other.
	space.sync_both(1, 2);
	let revoking = space.revoking(2);
	let by_first = space.commit(1, revoking);
	let revoking = space.revoking(1);
	let by_second = space.commit(2, revoking);
	space.sync_both(1, 2);
	space.sync_both(0, 1);
	space.sync_both(0, 2);
	for device in 0..3 {
		for change in [&first, &second, &by_first] {
			assert!(space.counts(device, change), "on {device}");
		}
		assert!(!space.counts(device, &by_second), "on {device}");
	}
}

#[test]
fn a_key_listed_with_a_codes_listing_is_listed_through_the_code_too() {
	let mut space = Space::new("listed alongside", 4);
	let code = space.code.clone();
	let naming = space.naming(&code);
	space.commit(0, naming);
	space.sync(0, 1);
	// The found code's device lists another key in the very change that lists itself.
	let mut listing = space.by_code(1, &code);
	listing.extend(space.listing(3));
	let found = space.commit(1, listing);
	space.sync(1, 3);
	// The phone, added after, sees it, and the key listed alongside and the phone revoke
	// each other.
	let listing = space.listing(2);
	space.commit(0, listing);
	space.sync(0, 2);
	space.sync(1, 2);
	let revoking = space.revoking(2);
	let by_listed = space.commit(3, revoking);
	let revoking = space.revoking(3);
	let by_phone = space.commit(2, revoking);
	for (a, b) in [(3, 2), (2, 0), (3, 0), (1, 0)] {
		space.sync_both(a, b);
	}
	for device in [0, 2, 3] {
		assert!(space.counts(device, &found), "on {device}");
		assert!(space.counts(device, &by_phone), "on {device}");
		assert!(!space.counts(device, &by_listed), "on {device}");
	}
}
