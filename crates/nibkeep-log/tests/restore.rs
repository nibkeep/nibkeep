//! Restoring what purges destroyed: a replica that waits keeps it apart, and a change that
//! restores it brings it back, as it was, on every replica ([technical design: copies that
//! wait]).
//!
//! [technical design: copies that wait]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait

use std::collections::BTreeSet;

use crate::common::{Device, add, field, items, set, text, texts};
use nibkeep_base::cbor::{self, Value as Cbor};
use nibkeep_base::{EntityId, Hash, StatementId};
use nibkeep_log::{
	Change, ChangeId, Draft, Kept, LogError, Parcel, Payload, Purges, Received, Restores, SpaceKeys,
};
use nibkeep_model::Operation;
use nibkeep_model::storage::{Infallibly, Statements};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Timestamp;

fn start() -> u64 {
	millis_at("2026-11-11", 9, 0)
}

const NOTE: &str = "org.example.note";
const ADDRESS: &str = "org.example.address";
const PASSWORD: &str = "org.nibkeep.password";

/// A space with a phone, on which purges are made, and a desktop that waits before purging,
/// so keeps what they destroy.
struct Space {
	rng: TestRng,
	keys: SpaceKeys,
	phone: Device,
	desktop: Device,
	ada: EntityId,
}

fn space(seed: &str) -> Space {
	let mut rng = TestRng::new(seed);
	let id = rng.space_id();
	let mut keys = SpaceKeys::new(0, rng.key());
	keys.insert_secrets(0, rng.key());
	let mut phone = Device::new(&mut rng, id);
	let mut desktop = Device::new(&mut rng, id);
	for device in [&mut phone, &mut desktop] {
		device.log.set_secret_fields([field(PASSWORD)]);
	}
	desktop.log.keep_destroyed(true);
	let ada = rng.entity_id();
	Space {
		rng,
		keys,
		phone,
		desktop,
		ada,
	}
}

fn asserted(operation: &Operation) -> StatementId {
	match operation {
		Operation::Assert { statement, .. } => statement.id,
		_ => panic!("an assertion"),
	}
}

/// An address and a note in one change, and the address moved in a second one, on the
/// phone, which both devices hold.
struct Edited {
	first: Parcel,
	old_address: StatementId,
}

fn edited(space: &mut Space) -> Edited {
	let ada = space.ada;
	let address = add(&mut space.rng, ada, ADDRESS, text("1 Old Road"));
	let note = add(&mut space.rng, ada, NOTE, text("met at the library"));
	let old_address = asserted(&address);
	let first = space
		.phone
		.commit(&space.keys, vec![address, note], start(), &mut space.rng);
	let moved = set(
		&mut space.rng,
		&space.phone.log,
		ada,
		ADDRESS,
		text("2 New Street"),
	);
	space
		.phone
		.commit(&space.keys, vec![moved], start() + 60_000, &mut space.rng);
	space
		.desktop
		.receive_from(&space.phone, &space.keys, start() + 90_000);
	Edited { first, old_address }
}

/// What the desktop kept of the purges it took in since this was last asked.
fn kept(device: &mut Device) -> Vec<(ChangeId, Kept)> {
	device.log.take_destroyed().kept
}

/// Gives back to a log what it kept, as a replica that waits does once it is restored.
fn give_back(device: &mut Device, kept: &[(ChangeId, Kept)], keys: &SpaceKeys, wall: u64) {
	for (_, part) in kept {
		match part {
			Kept::Change(parcel) => {
				device
					.log
					.receive((**parcel).clone(), keys, wall)
					.expect("takes the change back");
			}
			Kept::RecordKeys(statement, wrapped) => {
				for wrapped in wrapped {
					device
						.log
						.receive_wrapped_key(*statement, wrapped.clone(), keys)
						.expect("takes the key back");
				}
			}
			Kept::BlobKeys(sealed, wrapped) => {
				for wrapped in wrapped {
					device
						.log
						.receive_blob_key(*sealed, wrapped.clone(), keys)
						.expect("takes the key back");
				}
			}
		}
	}
}

#[test]
fn a_purged_value_comes_back_as_it_was_once_restored() {
	let mut space = space("restored as it was");
	let edited = edited(&mut space);
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges the old address");
	space
		.desktop
		.log
		.receive(purge.clone(), &space.keys, start() + 120_000)
		.expect("takes the purge");
	// The desktop applied the purge, and kept what it destroyed apart.
	assert!(
		space
			.desktop
			.log
			.is_change_purged(edited.first.id())
			.infallible()
	);
	assert_eq!(
		space
			.desktop
			.log
			.state()
			.statement(edited.old_address)
			.infallible(),
		None
	);
	let kept = kept(&mut space.desktop);
	assert_eq!(kept.len(), 1);
	assert_eq!(kept[0].0, purge.id());
	assert_eq!(kept[0].1, Kept::Change(Box::new(edited.first.clone())));
	// A phone that does not wait keeps nothing.
	nibkeep_testkit::assert_empty!(space.phone.log.take_destroyed().kept);

	let restores = Restores {
		changes: BTreeSet::from([edited.first.id()]),
		..Restores::default()
	};
	let restore = space
		.desktop
		.log
		.restore_purged(
			&space.desktop.key,
			&space.keys,
			restores.clone(),
			start() + 180_000,
			&mut space.rng,
		)
		.expect("restores");
	assert_eq!(restore.change.restores(), &restores);
	assert!(restore.change.purges().is_empty());
	let came_back = space.desktop.log.take_destroyed().restored;
	assert_eq!(came_back, restores);
	assert!(
		!space
			.desktop
			.log
			.is_change_purged(edited.first.id())
			.infallible()
	);
	give_back(&mut space.desktop, &kept, &space.keys, start() + 180_000);

	// The phone takes the restore, and the change's payload and key again from the desktop.
	space
		.phone
		.log
		.receive(restore, &space.keys, start() + 200_000)
		.expect("takes the restore");
	assert!(
		!space
			.phone
			.log
			.is_change_purged(edited.first.id())
			.infallible()
	);
	assert_eq!(
		space
			.phone
			.log
			.receive(edited.first.clone(), &space.keys, start() + 200_000),
		Ok(Received::Completed)
	);
	for device in [&space.phone, &space.desktop] {
		shows_as_it_was(&space, device, &edited);
	}
	assert_eq!(space.phone.log.state(), space.desktop.log.state());
}

/// Checks that a device shows the old address again, as an old value, from its own change
/// and time, and by its author, beside what the purge left.
fn shows_as_it_was(space: &Space, device: &Device, edited: &Edited) {
	let log = &device.log;
	let old = log
		.state()
		.storage()
		.get(edited.old_address)
		.infallible()
		.expect("the old address is back");
	assert_eq!(old.at, edited.first.change.timestamp());
	assert_eq!(texts(log, space.ada, ADDRESS), ["2 New Street"]);
	assert_eq!(texts(log, space.ada, NOTE), ["met at the library"]);
	assert_eq!(
		log.author_of(edited.old_address).infallible(),
		Some(space.phone.key.verifying_key())
	);
}

#[test]
fn a_purge_after_a_restore_purges_again() {
	let mut space = space("purged again");
	let edited = edited(&mut space);
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges");
	space
		.desktop
		.log
		.receive(purge, &space.keys, start() + 120_000)
		.expect("takes the purge");
	let kept = kept(&mut space.desktop);
	space
		.desktop
		.log
		.restore_purged(
			&space.desktop.key,
			&space.keys,
			Restores {
				changes: BTreeSet::from([edited.first.id()]),
				..Restores::default()
			},
			start() + 180_000,
			&mut space.rng,
		)
		.expect("restores");
	give_back(&mut space.desktop, &kept, &space.keys, start() + 180_000);
	space
		.phone
		.receive_from(&space.desktop, &space.keys, start() + 200_000);
	space
		.phone
		.log
		.receive(edited.first.clone(), &space.keys, start() + 200_000)
		.expect("completes");
	assert!(
		space
			.phone
			.log
			.state()
			.statement(edited.old_address)
			.infallible()
			.is_some()
	);

	// A purge that builds on the restore purges it again, everywhere.
	let again = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 240_000,
			&mut space.rng,
		)
		.expect("purges again");
	space
		.desktop
		.log
		.receive(again, &space.keys, start() + 240_000)
		.expect("takes the purge");
	for device in [&space.phone, &space.desktop] {
		assert!(device.log.is_change_purged(edited.first.id()).infallible());
		assert_eq!(
			device
				.log
				.state()
				.statement(edited.old_address)
				.infallible(),
			None
		);
	}
	assert_eq!(space.phone.log.state(), space.desktop.log.state());
}

#[test]
fn a_restore_does_not_undo_a_purge_it_does_not_build_on() {
	let mut space = space("concurrent purge");
	let edited = edited(&mut space);
	let purge = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[edited.old_address],
			start() + 120_000,
			&mut space.rng,
		)
		.expect("purges");
	space
		.desktop
		.log
		.receive(purge, &space.keys, start() + 120_000)
		.expect("takes the purge");
	let kept = kept(&mut space.desktop);
	// Meanwhile, a third device that had not seen the purge purges the same change.
	let mut tablet = Device::new(&mut space.rng, space.phone.log.space());
	let history = space.phone.log.parcels().infallible();
	for parcel in history
		.into_iter()
		.filter(|parcel| parcel.change.purges().is_empty())
		.chain([edited.first.clone()])
	{
		tablet
			.log
			.receive(parcel, &space.keys, start() + 130_000)
			.expect("takes the change");
	}
	let other = tablet
		.log
		.purge(
			&tablet.key,
			&space.keys,
			[edited.old_address],
			start() + 130_000,
			&mut space.rng,
		)
		.expect("purges too");
	space
		.desktop
		.log
		.restore_purged(
			&space.desktop.key,
			&space.keys,
			Restores {
				changes: BTreeSet::from([edited.first.id()]),
				..Restores::default()
			},
			start() + 180_000,
			&mut space.rng,
		)
		.expect("restores");
	give_back(&mut space.desktop, &kept, &space.keys, start() + 180_000);
	assert!(
		!space
			.desktop
			.log
			.is_change_purged(edited.first.id())
			.infallible()
	);
	// The tablet's purge, which the restore does not build on, purges it again.
	space
		.desktop
		.log
		.receive(other, &space.keys, start() + 200_000)
		.expect("takes the other purge");
	assert!(
		space
			.desktop
			.log
			.is_change_purged(edited.first.id())
			.infallible()
	);
	assert_eq!(
		space
			.desktop
			.log
			.state()
			.statement(edited.old_address)
			.infallible(),
		None
	);
}

#[test]
fn a_purged_secret_and_a_purged_blob_key_come_back_once_restored() {
	let mut space = space("secret restored");
	let ada = space.ada;
	let password = add(&mut space.rng, ada, PASSWORD, text("hunter2"));
	let password_id = asserted(&password);
	space
		.phone
		.commit(&space.keys, vec![password], start(), &mut space.rng);
	let content = Hash::of(b"a photo");
	let sealed = Hash::of(b"the photo, sealed");
	let blob_key = space.rng.key();
	space
		.phone
		.log
		.add_blob_key(content, sealed, blob_key, &space.keys, &mut space.rng)
		.expect("adds the blob key");
	space
		.desktop
		.receive_from(&space.phone, &space.keys, start() + 30_000);
	for (sealed, wrapped) in space.phone.log.wrapped_blob_keys() {
		space
			.desktop
			.log
			.receive_blob_key(*sealed, wrapped.clone(), &space.keys)
			.expect("takes the blob key");
	}
	assert_eq!(texts(&space.desktop.log, ada, PASSWORD), ["hunter2"]);
	let secret = space
		.phone
		.log
		.purge(
			&space.phone.key,
			&space.keys,
			[password_id],
			start() + 60_000,
			&mut space.rng,
		)
		.expect("purges the password");
	let blob = space
		.phone
		.log
		.purge_blob(
			&space.phone.key,
			&space.keys,
			&content,
			start() + 61_000,
			&mut space.rng,
		)
		.expect("purges the blob");
	// Each call says what it destroyed: the caller keeps it at once.
	let mut kept_apart = Vec::new();
	for purge in [secret, blob] {
		space
			.desktop
			.log
			.receive(purge, &space.keys, start() + 90_000)
			.expect("takes the purge");
		kept_apart.extend(kept(&mut space.desktop));
	}
	nibkeep_testkit::assert_empty!(texts(&space.desktop.log, ada, PASSWORD));
	assert_eq!(space.desktop.log.blob_keys(&content).count(), 0);
	let kept = kept_apart;
	assert!(kept.iter().any(
		|(_, part)| matches!(part, Kept::RecordKeys(statement, _) if *statement == password_id)
	));
	assert!(
		kept.iter()
			.any(|(_, part)| matches!(part, Kept::BlobKeys(blob, _) if *blob == sealed))
	);

	space
		.desktop
		.log
		.restore_purged(
			&space.desktop.key,
			&space.keys,
			Restores {
				statements: BTreeSet::from([password_id]),
				blobs: BTreeSet::from([sealed]),
				..Restores::default()
			},
			start() + 120_000,
			&mut space.rng,
		)
		.expect("restores");
	give_back(&mut space.desktop, &kept, &space.keys, start() + 120_000);
	assert_eq!(texts(&space.desktop.log, ada, PASSWORD), ["hunter2"]);
	assert_eq!(space.desktop.log.blob_keys(&content).count(), 1);

	// The phone takes the restore, and the keys again from the desktop.
	space
		.phone
		.receive_from(&space.desktop, &space.keys, start() + 150_000);
	for (sealed, wrapped) in space.desktop.log.wrapped_blob_keys() {
		space
			.phone
			.log
			.receive_blob_key(*sealed, wrapped.clone(), &space.keys)
			.expect("takes the blob key");
	}
	assert_eq!(texts(&space.phone.log, ada, PASSWORD), ["hunter2"]);
	assert_eq!(space.phone.log.blob_keys(&content).count(), 1);
}

#[test]
fn a_restore_names_only_what_a_purge_may_and_purges_nothing() {
	let mut space = space("what a restore names");
	let edited = edited(&mut space);
	let heads = space.phone.log.heads().infallible();
	let restoring = |restores: Restores, purges: Purges, rng: &mut TestRng| {
		let mut draft = Draft::new(
			heads.clone(),
			Timestamp::new(start() + 100_000, 0),
			Payload::default(),
		);
		draft.restores = restores;
		draft.purges = purges;
		Change::create(
			space.phone.log.space(),
			&space.keys,
			&space.phone.key,
			&draft,
			rng,
		)
	};
	let unknown = Restores {
		changes: BTreeSet::from([ChangeId::from_hash(Hash::of(b"elsewhere"))]),
		..Restores::default()
	};
	let named = Restores {
		changes: BTreeSet::from([edited.first.id()]),
		..Restores::default()
	};
	let and_purges = restoring(named.clone(), named.clone(), &mut space.rng);
	let not_an_ancestor = restoring(unknown, Purges::default(), &mut space.rng);
	for refused in [and_purges, not_an_ancestor] {
		assert_eq!(
			space
				.desktop
				.log
				.receive(refused, &space.keys, start() + 100_000),
			Err(LogError::InvalidPurge)
		);
	}
	assert_eq!(
		space.desktop.log.restore_purged(
			&space.desktop.key,
			&space.keys,
			Restores::default(),
			start() + 100_000,
			&mut space.rng
		),
		Err(LogError::InvalidPurge)
	);

	// The header names what it restores after what it purges, only if it restores anything;
	// a change that does not keeps the form changes had.
	let restore = restoring(named, Purges::default(), &mut space.rng);
	let encoded = cbor::to_vec(&restore.change);
	let decoded: Change = cbor::from_slice(&encoded).expect("decodes");
	assert_eq!(decoded, restore.change);
	let header = |change: &Change| match &items(change)[1] {
		Cbor::Array(header) => header.clone(),
		_ => panic!("a header"),
	};
	assert_eq!(header(&restore.change).len(), 11);
	assert_eq!(header(&edited.first.change).len(), 10);
	let mut empty = header(&restore.change);
	*empty.last_mut().expect("restores") = Cbor::Array(vec![
		Cbor::Array(vec![]),
		Cbor::Array(vec![]),
		Cbor::Array(vec![]),
	]);
	let mut change = items(&restore.change);
	change[1] = Cbor::Array(empty);
	assert!(cbor::from_slice::<Change>(&cbor::encode(&Cbor::Array(change))).is_err());
}
