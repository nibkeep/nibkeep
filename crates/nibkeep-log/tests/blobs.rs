//! Blob keys: wrapped with the space key, and those of content in secret fields with the
//! secrets key inside it; opened on other devices, and purged.

mod common;

use nibkeep_model::storage::Infallibly;

use std::collections::BTreeSet;

use common::Device;
use nibkeep_base::cbor::{self, ToCbor, Value as Cbor};
use nibkeep_base::{Hash, SpaceId};
use nibkeep_crypto::Key;
use nibkeep_log::{LogError, Purges, SpaceKeys, WrappedBlobKey};
use nibkeep_testkit::{TestRng, millis_at};

fn start() -> u64 {
	millis_at("2026-10-12", 10, 0)
}

/// A space with a phone, a laptop, and a new member's device that has only the space's
/// second key so far.
struct Space {
	rng: TestRng,
	id: SpaceId,
	keys: SpaceKeys,
	newest_keys: SpaceKeys,
	phone: Device,
	laptop: Device,
	newcomer: Device,
}

fn space(seed: &str) -> Space {
	let mut rng = TestRng::new(seed);
	let space = rng.space_id();
	let (first, second) = (rng.key(), rng.key());
	let mut keys = SpaceKeys::new(0, first);
	keys.insert(1, second.clone());
	let newest_keys = SpaceKeys::new(1, second);
	let [phone, laptop, newcomer] = [(); 3].map(|()| Device::new(&mut rng, space));
	Space {
		rng,
		id: space,
		keys,
		newest_keys,
		phone,
		laptop,
		newcomer,
	}
}

/// What a device knows after sealing a blob: its content hash, its sealed hash and its key.
struct Blob {
	content: Hash,
	sealed: Hash,
	key: Key,
}

/// Adds the key of a blob to a device, as the device would after sealing the content. The
/// sealed hash stands in for the hash of a sealed blob.
fn add_blob(device: &mut Device, keys: &SpaceKeys, rng: &mut TestRng, content: &[u8]) -> Blob {
	let blob = Blob {
		content: Hash::of(content),
		sealed: Hash::of(&rng.bytes::<32>()),
		key: rng.key(),
	};
	device
		.log
		.add_blob_key(blob.content, blob.sealed, blob.key.clone(), keys, rng)
		.expect("adds the key");
	blob
}

/// The keys of a space with a secrets key of number 0, as a device that holds the secrets
/// has them while they are unlocked.
fn with_secrets(keys: &SpaceKeys, secrets: &Key) -> SpaceKeys {
	let mut keys = keys.clone();
	keys.insert_secrets(0, secrets.clone());
	keys
}

/// Adds the key of a blob of a secret field to a device, as the device would after sealing
/// the content for the secrets.
fn add_secret_blob(
	device: &mut Device,
	keys: &SpaceKeys,
	rng: &mut TestRng,
	content: &[u8],
	sealed: Option<Hash>,
) -> Blob {
	let blob = Blob {
		content: Hash::of(content),
		sealed: sealed.unwrap_or_else(|| Hash::of(&rng.bytes::<32>())),
		key: rng.key(),
	};
	device
		.log
		.add_secret_blob_key(blob.content, blob.sealed, blob.key.clone(), keys, rng)
		.expect("adds the key");
	blob
}

/// The sealed hashes and key bytes that a device has opened for some content.
fn opened(device: &Device, content: &Hash) -> Vec<(Hash, [u8; 32])> {
	device
		.log
		.blob_keys(content)
		.map(|(sealed, key)| (*sealed, *key.expose_bytes()))
		.collect()
}

/// Receives every wrapped blob key of `other`.
fn receive_blob_keys(device: &mut Device, other: &Device, keys: &SpaceKeys) {
	let wrapped: Vec<(Hash, WrappedBlobKey)> = other
		.log
		.wrapped_blob_keys()
		.map(|(sealed, wrapped)| (*sealed, wrapped.clone()))
		.collect();
	for (sealed, wrapped) in wrapped {
		device
			.log
			.receive_blob_key(sealed, wrapped, keys)
			.expect("takes the key");
	}
}

#[test]
fn blob_keys_travel_wrapped_and_open_with_the_space_key() {
	let Space {
		mut rng,
		keys,
		newest_keys,
		mut phone,
		mut laptop,
		mut newcomer,
		..
	} = space("travel");
	let blob = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	let expected = vec![(blob.sealed, *blob.key.expose_bytes())];
	assert_eq!(opened(&phone, &blob.content), expected);

	let wrapped = phone.log.wrapped_blob_keys_of(&blob.sealed).to_vec();
	assert_eq!(wrapped.len(), 1);
	assert_eq!(wrapped[0].epoch(), 1, "wrapped with the newest space key");
	let encoded = cbor::to_vec(&wrapped[0]);
	let decoded: WrappedBlobKey = cbor::from_slice(&encoded).expect("decodes");
	assert_eq!(decoded, wrapped[0]);
	let content = blob.content.to_multihash();
	assert!(
		!encoded
			.windows(content.len())
			.any(|window| window == content),
		"the content hash is encrypted"
	);

	receive_blob_keys(&mut laptop, &phone, &keys);
	assert_eq!(opened(&laptop, &blob.content), expected);

	// A wrapped key moved to another sealed hash does not open, and is dropped.
	let elsewhere = Hash::of(b"another sealed blob");
	laptop
		.log
		.receive_blob_key(elsewhere, wrapped[0].clone(), &keys)
		.expect("takes the key");
	assert_eq!(laptop.log.wrapped_blob_keys_of(&elsewhere), []);

	// A device that does not have the space key yet keeps the wrapped key, and opens it
	// once it has the key.
	let old_blob = {
		let first_key_only = SpaceKeys::new(0, keys.get(0).expect("the first key").clone());
		add_blob(&mut phone, &first_key_only, &mut rng, b"an old photo")
	};
	receive_blob_keys(&mut newcomer, &phone, &newest_keys);
	assert_eq!(newcomer.log.blob_keys(&blob.content).count(), 1);
	assert_eq!(newcomer.log.blob_keys(&old_blob.content).count(), 0);
	assert_eq!(newcomer.log.wrapped_blob_keys_of(&old_blob.sealed).len(), 1);
	newcomer.log.open_blob_keys(&keys);
	assert_eq!(
		opened(&newcomer, &old_blob.content),
		vec![(old_blob.sealed, *old_blob.key.expose_bytes())]
	);
}

#[test]
fn wrapped_keys_that_cannot_be_checked_never_crowd_out_a_good_one() {
	let Space {
		mut rng,
		id,
		keys,
		newest_keys,
		mut phone,
		mut newcomer,
		..
	} = space("crowding");
	let blob = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	// Wrapped keys for the same sealed hash under the space's first key, which the
	// newcomer does not have, so it cannot tell that they are junk.
	let first_key_only = SpaceKeys::new(0, keys.get(0).expect("the first key").clone());
	for content in [b"one", b"two", b"six", b"ten"] {
		let mut other = Device::new(&mut rng, id);
		let key = rng.key();
		other
			.log
			.add_blob_key(
				Hash::of(content),
				blob.sealed,
				key,
				&first_key_only,
				&mut rng,
			)
			.expect("adds the key");
		let junk = other.log.wrapped_blob_keys_of(&blob.sealed)[0].clone();
		assert_eq!(junk.epoch(), 0);
		newcomer
			.log
			.receive_blob_key(blob.sealed, junk, &newest_keys)
			.expect("takes the key");
	}
	assert_eq!(newcomer.log.wrapped_blob_keys_of(&blob.sealed).len(), 4);
	assert_eq!(newcomer.log.blob_keys(&blob.content).count(), 0);

	receive_blob_keys(&mut newcomer, &phone, &newest_keys);
	assert_eq!(
		opened(&newcomer, &blob.content),
		vec![(blob.sealed, *blob.key.expose_bytes())]
	);
	let kept = newcomer.log.wrapped_blob_keys_of(&blob.sealed);
	assert_eq!(kept.len(), 4);
	assert!(kept.contains(&phone.log.wrapped_blob_keys_of(&blob.sealed)[0]));
}

#[test]
fn several_keys_for_one_content_are_kept() {
	let Space {
		mut rng,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("several");
	// The same photo, imported on two devices before they synced.
	let on_phone = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	let on_laptop = add_blob(&mut laptop, &keys, &mut rng, b"a photo");
	receive_blob_keys(&mut laptop, &phone, &keys);
	let sealed: BTreeSet<Hash> = laptop
		.log
		.blob_keys(&on_phone.content)
		.map(|(sealed, _)| *sealed)
		.collect();
	assert_eq!(sealed, BTreeSet::from([on_phone.sealed, on_laptop.sealed]));
}

#[test]
fn purging_a_blob_destroys_its_keys_everywhere() {
	let Space {
		mut rng,
		keys,
		mut phone,
		mut laptop,
		mut newcomer,
		..
	} = space("purge");
	let photo = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	let copy = add_blob(&mut laptop, &keys, &mut rng, b"a photo");
	let other = add_blob(&mut phone, &keys, &mut rng, b"another photo");
	receive_blob_keys(&mut phone, &laptop, &keys);
	receive_blob_keys(&mut laptop, &phone, &keys);
	let photo_wrapped = phone.log.wrapped_blob_keys_of(&photo.sealed).to_vec();

	assert!(matches!(
		phone
			.log
			.purge_blob(&phone.key, &keys, &Hash::of(b"unknown"), start(), &mut rng),
		Err(LogError::UnknownBlob)
	));
	let purge = phone
		.log
		.purge_blob(&phone.key, &keys, &photo.content, start(), &mut rng)
		.expect("purges");
	assert_eq!(
		purge.change.purges().blobs,
		BTreeSet::from([photo.sealed, copy.sealed]),
		"every key of the content"
	);
	let payload = phone
		.log
		.payload(purge.id(), &keys)
		.expect("opens")
		.expect("accepted");
	nibkeep_testkit::assert_empty!(payload.operations);

	laptop
		.log
		.receive(purge.clone(), &keys, start())
		.expect("accepts the purge");
	for device in [&phone, &laptop] {
		assert_eq!(device.log.blob_keys(&photo.content).count(), 0);
		for sealed in [photo.sealed, copy.sealed] {
			assert!(device.log.is_blob_purged(&sealed).infallible());
			assert_eq!(device.log.wrapped_blob_keys_of(&sealed), []);
		}
		assert_eq!(device.log.blob_keys(&other.content).count(), 1);
		assert!(!device.log.is_blob_purged(&other.sealed).infallible());
	}

	// A device that gets the purge before the key ignores the key.
	newcomer
		.log
		.receive(purge, &keys, start())
		.expect("accepts the purge");
	for wrapped in photo_wrapped {
		newcomer
			.log
			.receive_blob_key(photo.sealed, wrapped, &keys)
			.expect("takes the key");
	}
	receive_blob_keys(&mut newcomer, &laptop, &keys);
	assert_eq!(newcomer.log.blob_keys(&photo.content).count(), 0);
	assert_eq!(newcomer.log.wrapped_blob_keys_of(&photo.sealed), []);
	assert_eq!(newcomer.log.blob_keys(&other.content).count(), 1);

	// Content that was purged can be added again, with a new key.
	let again = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	assert_eq!(
		opened(&phone, &photo.content),
		vec![(again.sealed, *again.key.expose_bytes())]
	);
}

#[test]
fn a_value_and_its_content_are_purged_by_one_change() {
	let Space {
		mut rng,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("purge with a value");
	let photo = add_blob(&mut phone, &keys, &mut rng, b"a photo");
	let file = rng.entity_id();
	let content = common::add(
		&mut rng,
		file,
		"std.content",
		nibkeep_model::Value::Blob(photo.content),
	);
	let statement = match &content {
		nibkeep_model::Operation::Assert { statement, .. } => statement.id,
		_ => unreachable!("an assertion"),
	};
	let added = phone.commit(&keys, vec![content], start(), &mut rng);
	receive_blob_keys(&mut laptop, &phone, &keys);
	laptop.receive_from(&phone, &keys, start());

	let purge = phone
		.log
		.purge_with_blobs(
			&phone.key,
			&keys,
			[statement],
			(&[photo.content, Hash::of(b"never sealed here")], false),
			start() + 1_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(purge.change.purges().changes, BTreeSet::from([added.id()]));
	assert_eq!(purge.change.purges().blobs, BTreeSet::from([photo.sealed]));
	laptop
		.log
		.receive(purge, &keys, start() + 1_000)
		.expect("accepts the purge");
	for device in [&phone, &laptop] {
		assert_eq!(device.log.state().statement(statement).infallible(), None);
		assert_eq!(device.log.blob_keys(&photo.content).count(), 0);
		assert!(device.log.is_blob_purged(&photo.sealed).infallible());
	}
}

#[test]
fn purges_list_blobs_in_order() {
	let (first, second) = {
		let (a, b) = (Hash::of(b"one"), Hash::of(b"two"));
		(a.min(b), a.max(b))
	};
	let purges = Purges {
		blobs: BTreeSet::from([first, second]),
		..Purges::default()
	};
	let decoded: Purges = cbor::from_slice(&cbor::to_vec(&purges)).expect("decodes");
	assert_eq!(decoded, purges);

	let empty = Cbor::Array(Vec::new());
	let unordered = Cbor::Array(vec![
		empty.clone(),
		empty.clone(),
		Cbor::Array(vec![second.to_cbor(), first.to_cbor()]),
	]);
	assert!(cbor::from_slice::<Purges>(&cbor::encode(&unordered)).is_err());
	let two_items = Cbor::Array(vec![empty.clone(), empty]);
	assert!(cbor::from_slice::<Purges>(&cbor::encode(&two_items)).is_err());
}

#[test]
fn secret_blob_keys_open_only_with_the_secrets_key() {
	let Space {
		mut rng,
		keys,
		newest_keys,
		mut phone,
		mut laptop,
		mut newcomer,
		..
	} = space("secret");
	let secrets_key = rng.key();
	let secrets = with_secrets(&keys, &secrets_key);
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	let expected = vec![(scan.sealed, *scan.key.expose_bytes())];
	assert_eq!(opened(&phone, &scan.content), expected);
	assert!(phone.log.is_secret_blob(&scan.sealed));

	let wrapped = phone.log.wrapped_blob_keys_of(&scan.sealed).to_vec();
	assert_eq!(wrapped.len(), 1);
	assert_eq!(
		wrapped[0].epoch(),
		1,
		"wrapped outside with the newest space key"
	);
	let encoded = cbor::to_vec(&wrapped[0]);
	let decoded: WrappedBlobKey = cbor::from_slice(&encoded).expect("decodes");
	assert_eq!(decoded, wrapped[0]);
	let content = scan.content.to_multihash();
	assert!(
		!encoded
			.windows(content.len())
			.any(|window| window == content),
		"the content hash is encrypted"
	);

	// The space key alone opens nothing: the laptop keeps the key, unchecked, and passes it
	// on.
	receive_blob_keys(&mut laptop, &phone, &keys);
	laptop.log.open_blob_keys(&keys);
	assert_eq!(laptop.log.blob_keys(&scan.content).count(), 0);
	assert_eq!(
		laptop.log.wrapped_blob_keys_of(&scan.sealed),
		wrapped.as_slice()
	);
	receive_blob_keys(
		&mut newcomer,
		&laptop,
		&with_secrets(&newest_keys, &secrets_key),
	);
	assert_eq!(opened(&newcomer, &scan.content), expected);
	assert!(newcomer.log.is_secret_blob(&scan.sealed));

	// Unlocking the secrets opens it.
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(opened(&laptop, &scan.content), expected);

	// The secrets key alone opens nothing either.
	let mut secrets_only = SpaceKeys::new(7, rng.key());
	secrets_only.insert_secrets(0, secrets_key);
	let mut stranger = Device::new(&mut rng, newcomer.log.space());
	receive_blob_keys(&mut stranger, &phone, &secrets_only);
	assert_eq!(stranger.log.blob_keys(&scan.content).count(), 0);
}

#[test]
fn a_secret_blob_key_needs_the_secrets() {
	let Space {
		mut rng,
		keys,
		mut phone,
		..
	} = space("locked");
	let sealed = Hash::of(b"a sealed scan");
	let added =
		phone
			.log
			.add_secret_blob_key(Hash::of(b"a scan"), sealed, rng.key(), &keys, &mut rng);
	assert!(matches!(added, Err(LogError::SecretsLocked)), "{added:?}");
	assert_eq!(phone.log.wrapped_blob_keys_of(&sealed), []);
	assert!(!phone.log.is_secret_blob(&sealed));
}

#[test]
fn locking_the_secrets_forgets_the_secret_blob_keys_alone() {
	let Space {
		mut rng,
		keys,
		mut phone,
		..
	} = space("locking");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	let photo = add_blob(&mut phone, &secrets, &mut rng, b"a photo");
	assert!(!phone.log.is_secret_blob(&photo.sealed));
	// The same content in an ordinary field too has an ordinary key of its own.
	let plain_scan = add_blob(&mut phone, &secrets, &mut rng, b"a scan");
	assert_eq!(phone.log.blob_keys(&scan.content).count(), 2);

	phone.log.lock_secrets();
	assert_eq!(
		opened(&phone, &scan.content),
		vec![(plain_scan.sealed, *plain_scan.key.expose_bytes())]
	);
	assert!(!phone.log.is_secret_blob(&scan.sealed));
	assert_eq!(phone.log.blob_keys(&photo.content).count(), 1);
	assert_eq!(phone.log.wrapped_blob_keys_of(&scan.sealed).len(), 1);

	phone.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(phone.log.blob_keys(&scan.content).count(), 2);
	assert!(phone.log.is_secret_blob(&scan.sealed));
}

#[test]
fn a_secret_blob_key_wrapped_with_another_secrets_key_is_dropped_once_it_can_be_checked() {
	let Space {
		mut rng,
		id,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("forged");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	// An infected replica, which holds the space keys but not the secrets, wraps a key of its
	// own for the same sealed blob with a secrets key it made up, of the same number.
	let mut infected = Device::new(&mut rng, id);
	let made_up = with_secrets(&keys, &rng.key());
	add_secret_blob(
		&mut infected,
		&made_up,
		&mut rng,
		b"something else",
		Some(scan.sealed),
	);
	let forged = infected.log.wrapped_blob_keys_of(&scan.sealed)[0].clone();

	// Without the secrets, the laptop cannot tell them apart, and keeps both.
	receive_blob_keys(&mut laptop, &infected, &keys);
	receive_blob_keys(&mut laptop, &phone, &keys);
	assert_eq!(laptop.log.wrapped_blob_keys_of(&scan.sealed).len(), 2);
	// With them, it opens the good one, and drops the other.
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(
		opened(&laptop, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
	assert_eq!(
		laptop.log.wrapped_blob_keys_of(&scan.sealed),
		phone.log.wrapped_blob_keys_of(&scan.sealed)
	);
	assert_eq!(
		laptop.log.blob_keys(&Hash::of(b"something else")).count(),
		0
	);

	// A device whose secrets are unlocked drops it as it arrives.
	phone
		.log
		.receive_blob_key(scan.sealed, forged, &secrets)
		.expect("takes the key");
	assert_eq!(phone.log.wrapped_blob_keys_of(&scan.sealed).len(), 1);
}

#[test]
fn forged_secret_blob_keys_never_crowd_out_a_good_one_where_the_secrets_are_unlocked() {
	let Space {
		mut rng,
		id,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("secret crowding");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	for content in [b"one", b"two", b"six", b"ten"] {
		let mut infected = Device::new(&mut rng, id);
		let made_up = with_secrets(&keys, &rng.key());
		add_secret_blob(
			&mut infected,
			&made_up,
			&mut rng,
			content,
			Some(scan.sealed),
		);
		receive_blob_keys(&mut laptop, &infected, &secrets);
	}
	assert_eq!(laptop.log.wrapped_blob_keys_of(&scan.sealed), []);
	receive_blob_keys(&mut laptop, &phone, &secrets);
	assert_eq!(
		opened(&laptop, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
}

#[test]
fn purging_secret_content_destroys_its_keys_where_the_secrets_are_locked() {
	let Space {
		mut rng,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("secret purge");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	receive_blob_keys(&mut laptop, &phone, &keys);
	assert_eq!(laptop.log.wrapped_blob_keys_of(&scan.sealed).len(), 1);

	let purge = phone
		.log
		.purge_blob(&phone.key, &secrets, &scan.content, start(), &mut rng)
		.expect("purges");
	assert_eq!(purge.change.purges().blobs, BTreeSet::from([scan.sealed]));
	laptop
		.log
		.receive(purge, &keys, start())
		.expect("accepts the purge");
	for device in [&phone, &laptop] {
		assert!(device.log.is_blob_purged(&scan.sealed).infallible());
		assert_eq!(device.log.wrapped_blob_keys_of(&scan.sealed), []);
		assert_eq!(device.log.blob_keys(&scan.content).count(), 0);
	}
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(laptop.log.blob_keys(&scan.content).count(), 0);
}

#[test]
fn forged_ordinary_keys_never_replace_a_secret_blob_key_where_the_secrets_are_locked() {
	let Space {
		mut rng,
		id,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("forged ordinary keys");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	// The laptop, its secrets locked, holds the good key, which waits for them.
	receive_blob_keys(&mut laptop, &phone, &keys);
	let good = phone.log.wrapped_blob_keys_of(&scan.sealed)[0].clone();
	// An infected replica wraps keys of its own for the same sealed blob as ordinary blob
	// keys, which open with the space key alone.
	for content in [b"one", b"two", b"six", b"ten"] {
		let mut infected = Device::new(&mut rng, id);
		let key = rng.key();
		infected
			.log
			.add_blob_key(Hash::of(content), scan.sealed, key, &keys, &mut rng)
			.expect("adds the key");
		receive_blob_keys(&mut laptop, &infected, &keys);
	}
	assert!(
		laptop
			.log
			.wrapped_blob_keys_of(&scan.sealed)
			.contains(&good),
		"the good key stays"
	);
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(
		opened(&laptop, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
	assert!(laptop.log.is_secret_blob(&scan.sealed));
}

#[test]
fn a_secret_blob_key_takes_the_place_of_a_forged_ordinary_one_once_the_secrets_unlock() {
	let Space {
		mut rng,
		id,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("forged ordinary key first");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	// A forged ordinary key arrives first, while the laptop's secrets are locked, and opens.
	let mut infected = Device::new(&mut rng, id);
	let forged = rng.key();
	infected
		.log
		.add_blob_key(Hash::of(b"other"), scan.sealed, forged, &keys, &mut rng)
		.expect("adds the key");
	receive_blob_keys(&mut laptop, &infected, &keys);
	receive_blob_keys(&mut laptop, &phone, &keys);
	assert_eq!(laptop.log.blob_keys(&Hash::of(b"other")).count(), 1);
	assert_eq!(laptop.log.blob_keys(&scan.content).count(), 0);

	// Unlocked, the secret blob key opens, in its place.
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	assert_eq!(
		opened(&laptop, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
	assert_eq!(laptop.log.blob_keys(&Hash::of(b"other")).count(), 0);

	// So it does where the secrets are unlocked as the forged key arrives.
	let mut desk = Device::new(&mut rng, id);
	receive_blob_keys(&mut desk, &phone, &secrets);
	receive_blob_keys(&mut desk, &infected, &secrets);
	assert_eq!(
		opened(&desk, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
}

#[test]
fn purging_content_purges_the_copies_of_its_own_kind() {
	let Space {
		mut rng,
		keys,
		mut phone,
		..
	} = space("purge by kind");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	let plain = add_blob(&mut phone, &secrets, &mut rng, b"a scan");
	// Purging the ordinary copy, with the secrets unlocked or not, leaves the secret one.
	let ordinary = phone
		.log
		.purge_with_blobs(
			&phone.key,
			&secrets,
			[],
			(&[plain.content], false),
			start(),
			&mut rng,
		)
		.expect("purges");
	assert_eq!(
		ordinary.change.purges().blobs,
		BTreeSet::from([plain.sealed])
	);
	assert_eq!(
		opened(&phone, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
	let secret = phone
		.log
		.purge_with_blobs(
			&phone.key,
			&secrets,
			[],
			(&[scan.content], true),
			start() + 1_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(secret.change.purges().blobs, BTreeSet::from([scan.sealed]));
	assert_eq!(phone.log.blob_keys(&scan.content).count(), 0);
}

#[test]
fn a_secret_blob_key_that_opens_drops_the_ordinary_keys_forged_for_its_blob() {
	let Space {
		mut rng,
		id,
		keys,
		mut phone,
		mut laptop,
		..
	} = space("forged keys first");
	let secrets = with_secrets(&keys, &rng.key());
	let scan = add_secret_blob(&mut phone, &secrets, &mut rng, b"a scan", None);
	let forge = |rng: &mut TestRng, content: &[u8]| {
		let mut infected = Device::new(rng, id);
		let key = rng.key();
		infected
			.log
			.add_blob_key(Hash::of(content), scan.sealed, key, &keys, rng)
			.expect("adds the key");
		infected
	};
	// Four ordinary keys forged for the blob fill the laptop's places, its secrets locked:
	// the good key, which arrives then, cannot take one yet.
	for content in [b"one", b"two", b"six", b"ten"] {
		let infected = forge(&mut rng, content);
		receive_blob_keys(&mut laptop, &infected, &keys);
	}
	receive_blob_keys(&mut laptop, &phone, &keys);
	assert_eq!(laptop.log.wrapped_blob_keys_of(&scan.sealed).len(), 4);
	// It arrives again, as it does at every sync, while the secrets are unlocked: it opens,
	// and the forged keys go.
	laptop.log.unlock_secrets(&secrets).expect("unlocks");
	receive_blob_keys(&mut laptop, &phone, &secrets);
	assert_eq!(
		laptop.log.wrapped_blob_keys_of(&scan.sealed),
		phone.log.wrapped_blob_keys_of(&scan.sealed)
	);
	assert_eq!(
		opened(&laptop, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
}

#[test]
fn a_secret_blob_key_of_a_space_key_received_later_takes_the_place_of_a_forged_one() {
	let Space {
		mut rng,
		id,
		keys,
		newest_keys,
		mut phone,
		mut newcomer,
		..
	} = space("secret key of an older space key");
	let secrets_key = rng.key();
	// The good key is wrapped with the space's first key, which the newcomer lacks.
	let first_key_only = SpaceKeys::new(0, keys.get(0).expect("the first key").clone());
	let scan = add_secret_blob(
		&mut phone,
		&with_secrets(&first_key_only, &secrets_key),
		&mut rng,
		b"a scan",
		None,
	);
	let mut infected = Device::new(&mut rng, id);
	let forged = rng.key();
	infected
		.log
		.add_blob_key(
			Hash::of(b"other"),
			scan.sealed,
			forged,
			&newest_keys,
			&mut rng,
		)
		.expect("adds the key");
	let newest = with_secrets(&newest_keys, &secrets_key);
	receive_blob_keys(&mut newcomer, &infected, &newest);
	receive_blob_keys(&mut newcomer, &phone, &newest);
	assert_eq!(newcomer.log.blob_keys(&Hash::of(b"other")).count(), 1);
	// Once the older space key arrives, the good key opens, in the forged one's place.
	newcomer
		.log
		.open_blob_keys(&with_secrets(&keys, &secrets_key));
	assert_eq!(
		opened(&newcomer, &scan.content),
		vec![(scan.sealed, *scan.key.expose_bytes())]
	);
	assert_eq!(newcomer.log.blob_keys(&Hash::of(b"other")).count(), 0);
	assert_eq!(
		newcomer.log.wrapped_blob_keys_of(&scan.sealed),
		phone.log.wrapped_blob_keys_of(&scan.sealed)
	);
}
