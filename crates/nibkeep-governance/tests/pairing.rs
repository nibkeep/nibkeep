//! Pairing a new replica: the code, the key agreed on from it, the request and the grant,
//! and the records of the new replica.

use std::collections::BTreeMap;

use nibkeep_base::cbor::{self, ToCbor};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{DecryptionKey, Key, SigningKey};
use nibkeep_governance::{
	DEVICE, ENDPOINT, Grant, JoinRequest, NewReplica, Pairing, PairingCode, REPLICA, Side,
	endpoint_secret, endpoint_text, open_space_keys, replica, seal_space_keys,
};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{State, Value};
use nibkeep_testkit::TestRng;
use nibkeep_time::Timestamp;

#[test]
fn codes_have_eight_digits_and_read_back_as_shown() {
	let mut rng = TestRng::new("codes");
	for _ in 0..200 {
		let code = PairingCode::generate(&mut rng);
		let shown = code.to_string();
		assert_eq!(shown.len(), 10, "{shown}");
		assert!((10..=99).contains(&code.nameplate()));
		assert_eq!(shown.parse::<PairingCode>(), Ok(code.clone()));
		assert_eq!(
			shown.replace('-', "").parse::<PairingCode>(),
			Ok(code.clone())
		);
		assert_eq!(
			shown.replace('-', " ").parse::<PairingCode>(),
			Ok(code.clone())
		);
		assert_eq!(code.announcement(), format!("nibkeep-pair-{}", &shown[..2]));
		assert!(
			!format!("{code:?}").contains(&shown[3..]),
			"Debug hides the secret"
		);
	}
	for text in [
		"",
		"1234567",
		"123456789",
		"02-345-678",
		"12-345-67x",
		"12_345_678",
	] {
		assert!(text.parse::<PairingCode>().is_err(), "{text:?}");
	}
}

#[test]
fn codes_are_spread_over_every_digit() {
	let mut rng = TestRng::new("spread");
	let mut seen = [[0_u32; 10]; 8];
	for _ in 0..2_000 {
		let code = PairingCode::generate(&mut rng).to_string().replace('-', "");
		for (place, digit) in code.bytes().enumerate() {
			seen[place][usize::from(digit - b'0')] += 1;
		}
	}
	assert_eq!(seen[0][0], 0, "the first digit is never 0");
	for (place, counts) in seen.iter().enumerate() {
		let digits = if place == 0 {
			&counts[1..]
		} else {
			&counts[..]
		};
		assert!(
			digits.iter().all(|&count| count > 100),
			"{place}: {counts:?}"
		);
	}
}

/// Runs SPAKE2 between a sponsor and a joiner, each with its code and its binding.
fn pair(
	sponsor_code: &str,
	joiner_code: &str,
	sponsor_binding: [u8; 32],
	joiner_binding: [u8; 32],
) -> (
	nibkeep_governance::PairingKey,
	nibkeep_governance::PairingKey,
) {
	let mut rng = TestRng::new(&format!("{sponsor_code} {joiner_code}"));
	let (sponsor, to_joiner) = Pairing::start(
		Side::Sponsor,
		&sponsor_code.parse().expect("a code"),
		&mut rng,
	);
	let (joiner, to_sponsor) = Pairing::start(
		Side::Joiner,
		&joiner_code.parse().expect("a code"),
		&mut rng,
	);
	(
		sponsor
			.finish(&to_sponsor, &sponsor_binding)
			.expect("the joiner's message"),
		joiner
			.finish(&to_joiner, &joiner_binding)
			.expect("the sponsor's message"),
	)
}

#[test]
fn the_same_code_on_one_connection_makes_tags_that_check() {
	let (sponsor, joiner) = pair("37-482-177", "37482177", [1; 32], [1; 32]);
	let message = "a request".to_cbor();
	let tag = joiner.tag(Side::Joiner, &message);
	assert!(sponsor.checks(Side::Joiner, &message, &tag));
	assert!(
		!sponsor.checks(Side::Sponsor, &message, &tag),
		"a tag is for one role"
	);
	assert!(!sponsor.checks(Side::Joiner, &"another".to_cbor(), &tag));
}

#[test]
fn another_code_or_another_connection_makes_tags_that_fail() {
	let message = "a request".to_cbor();
	for (sponsor, joiner) in [
		pair("37-482-177", "37-482-178", [1; 32], [1; 32]),
		pair("37-482-177", "37-482-177", [1; 32], [2; 32]),
	] {
		let tag = joiner.tag(Side::Joiner, &message);
		assert!(!sponsor.checks(Side::Joiner, &message, &tag));
	}
}

#[test]
fn space_keys_open_only_for_the_replica_they_are_sealed_for() {
	let mut rng = TestRng::new("space keys");
	let decryption = DecryptionKey::generate(&mut rng);
	let replica = EntityId::random(&mut rng);
	let mut spaces: BTreeMap<SpaceId, BTreeMap<u64, Key>> = BTreeMap::new();
	for count in 0..3 {
		let keys = (0..=count)
			.map(|epoch| (epoch, Key::generate(&mut rng)))
			.collect();
		spaces.insert(SpaceId::random(&mut rng), keys);
	}
	let sealed = seal_space_keys(&decryption.encryption_key(), replica, &spaces, &mut rng);
	let opened = open_space_keys(&decryption, replica, &sealed).expect("opens");
	assert_eq!(opened.len(), 3);
	for (space, keys) in &spaces {
		for (epoch, key) in keys {
			assert!(opened[space][epoch].same_as(key));
		}
	}
	let other = EntityId::random(&mut rng);
	assert!(open_space_keys(&decryption, other, &sealed).is_err());
	let wrong = DecryptionKey::generate(&mut rng);
	assert!(open_space_keys(&wrong, replica, &sealed).is_err());
}

#[test]
fn requests_and_grants_read_back_as_written() {
	let mut rng = TestRng::new("messages");
	let decryption = DecryptionKey::generate(&mut rng);
	let request = JoinRequest {
		signing: SigningKey::generate(&mut rng).verifying_key(),
		encryption: decryption.encryption_key(),
		device: "laptop".to_owned(),
		app: "nib".to_owned(),
		secrets: None,
	};
	assert_eq!(
		cbor::from_slice::<JoinRequest>(&cbor::to_vec(&request)),
		Ok(request.clone())
	);
	// A request that asks for the secrets too.
	let asking = JoinRequest {
		secrets: Some(DecryptionKey::generate(&mut rng).encryption_key()),
		..request.clone()
	};
	assert_eq!(
		cbor::from_slice::<JoinRequest>(&cbor::to_vec(&asking)),
		Ok(asking.clone())
	);
	let empty = JoinRequest {
		device: String::new(),
		..request
	};
	assert!(cbor::from_slice::<JoinRequest>(&cbor::to_vec(&empty)).is_err());

	let replica = EntityId::random(&mut rng);
	let space = SpaceId::random(&mut rng);
	let keys = BTreeMap::from([(space, BTreeMap::from([(0, Key::generate(&mut rng))]))]);
	let grant = Grant {
		user: EntityId::random(&mut rng),
		device: EntityId::random(&mut rng),
		replica,
		space,
		sponsor: SigningKey::generate(&mut rng).verifying_key(),
		keys: seal_space_keys(&decryption.encryption_key(), replica, &keys, &mut rng),
		ai_agent: false,
		chosen: Vec::new(),
		asks_first: false,
	};
	assert_eq!(
		cbor::from_slice::<Grant>(&cbor::to_vec(&grant)),
		Ok(grant.clone())
	);
	// A phone's grant says that it asks before it takes a space given to it later.
	let phone = Grant {
		asks_first: true,
		..grant.clone()
	};
	assert_eq!(
		cbor::from_slice::<Grant>(&cbor::to_vec(&phone)),
		Ok(phone.clone())
	);
	assert_ne!(cbor::to_vec(&phone), cbor::to_vec(&grant));
	// An AI agent's grant says that it is one, and names the other spaces chosen for it,
	// with the user's member in each, if any.
	let ai_agent = Grant {
		ai_agent: true,
		..grant.clone()
	};
	assert_eq!(
		cbor::from_slice::<Grant>(&cbor::to_vec(&ai_agent)),
		Ok(ai_agent.clone())
	);
	assert_ne!(cbor::to_vec(&ai_agent), cbor::to_vec(&grant));
	let chosen = Grant {
		chosen: vec![(SpaceId::random(&mut rng), EntityId::random(&mut rng))],
		..ai_agent
	};
	assert_eq!(
		cbor::from_slice::<Grant>(&cbor::to_vec(&chosen)),
		Ok(chosen)
	);
}

#[test]
fn an_added_replica_has_its_keys_owner_device_and_endpoint() {
	let mut rng = TestRng::new("added replica");
	let signing = SigningKey::generate(&mut rng);
	let decryption = DecryptionKey::generate(&mut rng);
	let user = EntityId::random(&mut rng);
	let endpoint = [7; 32];
	let space = SpaceId::random(&mut rng);
	let new_replica = nibkeep_governance::replica_record(space, &signing.verifying_key());
	let space_key = Key::generate(&mut rng);
	let own = nibkeep_governance::Listing::sign(
		&signing,
		&decryption.encryption_key(),
		space,
		(user, false),
	)
	.to_text();
	let hash = nibkeep_base::Hash::of(b"the key's hash");
	let granted = [nibkeep_governance::SealedSpaceKey::seal(
		&decryption.encryption_key(),
		space,
		new_replica,
		(0, &space_key, hash),
		&mut rng,
	)];
	let added = nibkeep_governance::add_replica(
		&NewReplica {
			space,
			user,
			signing: &signing.verifying_key(),
			encryption: &decryption.encryption_key(),
			endpoint: &endpoint,
			device: "laptop",
			app: "nib",
			keys: &granted,
			ai_agent: false,
			given: &[],
			listing: Some(&own),
		},
		&mut rng,
	);
	// The keys it is granted are stated on its record, and open for it alone.
	let stated: Vec<nibkeep_governance::SealedSpaceKey> = added
		.governing
		.iter()
		.filter_map(|operation| match operation {
			nibkeep_model::Operation::Assert { statement, .. }
				if statement.field == nibkeep_governance::SPACE_KEY =>
			{
				assert_eq!(statement.entity, new_replica);
				nibkeep_governance::SealedSpaceKey::from_text(statement.value.as_text()?)
			}
			_ => None,
		})
		.collect();
	assert_eq!(stated, granted);
	assert_eq!(
		stated[0]
			.open(&decryption, space, new_replica)
			.map(|key| *key.expose_bytes()),
		Some(*space_key.expose_bytes())
	);
	assert!(
		stated[0]
			.open(&decryption, space, EntityId::random(&mut rng))
			.is_none()
	);
	assert_eq!(added.replica, new_replica);
	assert!(
		added.governing.iter().all(|operation| matches!(
			operation,
			nibkeep_model::Operation::Assert { statement, .. }
				if nibkeep_model::fields::is_governing(&statement.field)
		)),
		"the first change holds only what governs"
	);
	let mut state = State::new();
	for operation in added.governing.iter().chain(&added.operations) {
		state.apply(operation, Timestamp::new(1, 0));
	}
	let record = replica(&state, added.replica)
		.infallible()
		.expect("a replica");
	assert_eq!(record.signing, Some(signing.verifying_key()));
	assert!(record.has_encryption_key);
	assert_eq!(record.endpoint, Some(endpoint));
	assert_eq!(record.owner, Some(user));
	assert_eq!(record.device, Some(added.device));
	assert!(!record.deleted);
	assert!(state.types(added.replica).infallible().contains(&REPLICA));
	assert!(state.types(added.device).infallible().contains(&DEVICE));
	assert_eq!(
		state.current(added.replica, &ENDPOINT).infallible()[0].value,
		Value::Text(endpoint_text(&endpoint))
	);
	assert_eq!(replica(&state, added.device).infallible(), None);
}

#[test]
fn endpoint_keys_are_derived_from_signing_keys() {
	let mut rng = TestRng::new("endpoint");
	let one = SigningKey::generate(&mut rng);
	let other = SigningKey::generate(&mut rng);
	assert!(endpoint_secret(&one).same_as(&endpoint_secret(&one)));
	assert!(!endpoint_secret(&one).same_as(&endpoint_secret(&other)));
	assert_ne!(endpoint_secret(&one).expose_bytes(), one.expose_bytes());
}

#[test]
fn an_added_replica_is_listed_by_its_own_listing_on_records_derived_from_its_key() {
	let mut rng = TestRng::new("added replica's listing");
	let signing = SigningKey::generate(&mut rng);
	let decryption = DecryptionKey::generate(&mut rng);
	let (user, space) = (EntityId::random(&mut rng), SpaceId::random(&mut rng));
	let own = nibkeep_governance::Listing::sign(
		&signing,
		&decryption.encryption_key(),
		space,
		(user, false),
	)
	.to_text();
	let added = nibkeep_governance::add_replica(
		&NewReplica {
			space,
			user,
			signing: &signing.verifying_key(),
			encryption: &decryption.encryption_key(),
			endpoint: &[7; 32],
			device: "laptop",
			app: "nib",
			keys: &[],
			ai_agent: false,
			given: &[],
			listing: Some(&own),
		},
		&mut rng,
	);
	assert_eq!(
		added.replica,
		nibkeep_governance::replica_record(space, &signing.verifying_key())
	);
	assert_eq!(
		added.device,
		nibkeep_governance::device_record(space, &signing.verifying_key())
	);
	assert!(added.governing.iter().any(|operation| matches!(
		operation,
		nibkeep_model::Operation::Assert { statement, .. }
			if statement.field == nibkeep_model::fields::LISTING
				&& statement.entity == added.replica
				&& statement.value.as_text() == Some(own.as_str())
	)));
}
