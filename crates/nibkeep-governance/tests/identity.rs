//! A user, the person linked to the user, a device, a replica on it, and a personal space.

use nibkeep_base::EntityId;
use nibkeep_crypto::SigningKey;
use nibkeep_governance::{
	DEVICE, LANGUAGES, LINKED_PERSON, NAME, ON_DEVICE, OWNER, PERSON, REPLICA, SIGNING_KEY, SPACE,
	USER, found, name,
};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{FieldName, Languages, State, Types, Value};
use nibkeep_testkit::TestRng;
use nibkeep_time::Timestamp;

/// A replica's signing key and encryption key, and the ID of the personal space it founds.
fn keys(
	rng: &mut TestRng,
) -> (
	SigningKey,
	nibkeep_crypto::EncryptionKey,
	nibkeep_base::SpaceId,
) {
	(
		SigningKey::generate(rng),
		nibkeep_crypto::DecryptionKey::generate(rng).encryption_key(),
		rng.space_id(),
	)
}

/// The entity that a field of an entity refers to.
fn reference(state: &State, entity: EntityId, field: &FieldName) -> Option<EntityId> {
	let current = state.current(entity, field).infallible();
	assert!(current.len() <= 1, "{field} has one value");
	current
		.first()
		.and_then(|statement| statement.value.as_reference())
}

#[test]
fn founding_a_personal_space_records_who_and_where() {
	let mut rng = TestRng::new("founding");
	let (signing, encryption, space) = keys(&mut rng);
	let key = signing.verifying_key();
	let founding = found(
		space,
		("ada", "Ada Lovelace"),
		("laptop", "nib"),
		&Languages::default(),
		(&signing, &encryption),
		&mut rng,
	);
	let mut state = State::new();
	for operation in founding.governing.iter().chain(&founding.operations) {
		state.apply(operation, Timestamp::new(1, 0));
	}
	for (entity, of_type, named) in [
		// The user and the person each with a name of their own.
		(founding.user, USER, "ada"),
		(founding.person, PERSON, "Ada Lovelace"),
		(founding.device, DEVICE, "laptop"),
		(founding.replica, REPLICA, "nib"),
		(founding.space, SPACE, "Personal"),
	] {
		assert_eq!(
			state
				.types(entity)
				.infallible()
				.into_iter()
				.collect::<Vec<_>>(),
			std::slice::from_ref(&of_type),
			"{of_type}"
		);
		assert_eq!(
			name(&state, entity).infallible().as_deref(),
			Some(named),
			"{of_type}"
		);
	}
	assert_eq!(
		state.entities().infallible().len(),
		5,
		"nothing else is made"
	);
	// The user is the person; the replica belongs to the user, and is on the device.
	assert_eq!(
		reference(&state, founding.user, &LINKED_PERSON),
		Some(founding.person)
	);
	assert_eq!(
		reference(&state, founding.replica, &OWNER),
		Some(founding.user)
	);
	assert_eq!(
		reference(&state, founding.replica, &ON_DEVICE),
		Some(founding.device)
	);
	// The replica holds the key, and nothing else does.
	let signing = state.current(founding.replica, &SIGNING_KEY).infallible();
	assert_eq!(signing.len(), 1);
	assert_eq!(signing[0].value, Value::Text(key.to_text()));
	for entity in [
		founding.user,
		founding.person,
		founding.device,
		founding.space,
	] {
		nibkeep_testkit::assert_empty!(state.current(entity, &SIGNING_KEY).infallible());
		nibkeep_testkit::assert_empty!(state.current(entity, &OWNER).infallible());
	}
	assert_eq!(
		key.to_text().len(),
		43,
		"32 bytes in base64url without padding"
	);
	assert!(
		name(&state, EntityId::from_bytes([1; 16]))
			.infallible()
			.is_none()
	);
}

#[test]
fn the_types_of_identity_fit_what_founding_writes() {
	let mut rng = TestRng::new("types");
	let (signing, encryption, space) = keys(&mut rng);
	let founding = found(
		space,
		("Ada", "Ada"),
		("laptop", "nib"),
		&Languages::default(),
		(&signing, &encryption),
		&mut rng,
	);
	let mut types = Types::new();
	for definition in nibkeep_governance::types() {
		types.add(definition);
	}
	for of_type in [USER, DEVICE, REPLICA, SPACE] {
		assert!(types.get(&of_type).is_some(), "{of_type}");
	}
	// People are the standard vocabulary's, which every space installs.
	assert!(types.get(&PERSON).is_none());
	assert!(PERSON.as_str().starts_with("std."));
	// Every entity has one name at a time, of the kind Name.
	let name_field = types.field(&NAME).expect("names are defined");
	assert_eq!(name_field.multiplicity(), nibkeep_model::Multiplicity::One);
	assert_eq!(name_field.kind(), nibkeep_model::ValueKind::Name);
	assert!(types.field(&OWNER).is_some_and(|owner| owner.is_role()));
	for field in [LINKED_PERSON, ON_DEVICE] {
		let defined = types.field(&field).expect("defined");
		assert_eq!(defined.kind(), nibkeep_model::ValueKind::Reference);
		assert_eq!(defined.multiplicity(), nibkeep_model::Multiplicity::One);
	}
	assert!(types.secret_fields().is_empty());
	for operation in founding.governing.iter().chain(&founding.operations) {
		let nibkeep_model::Operation::Assert { statement, .. } = operation else {
			panic!("founding only asserts");
		};
		if statement.field != nibkeep_model::fields::TYPE {
			let field = types.field(&statement.field).expect("a defined field");
			assert_eq!(field.kind(), statement.value.kind(), "{}", statement.field);
		}
	}
}

#[test]
fn the_first_change_governs_the_space_and_holds_nothing_else() {
	let mut rng = TestRng::new("governing");
	let (signing, encryption, space) = keys(&mut rng);
	let key = signing.verifying_key();
	let founding = found(
		space,
		("Ada", "Ada"),
		("laptop", "nib"),
		&Languages::default(),
		(&signing, &encryption),
		&mut rng,
	);
	let fields = |operations: &[nibkeep_model::Operation]| -> Vec<(EntityId, bool)> {
		operations
			.iter()
			.map(|operation| match operation {
				nibkeep_model::Operation::Assert { statement, .. } => (
					statement.entity,
					nibkeep_model::fields::is_governing(&statement.field),
				),
				_ => panic!("founding only asserts"),
			})
			.collect()
	};
	// The replica's key, encryption key, owner and listing, on its record derived from the
	// space and its key, as its device's record is: the listing is the key's own.
	assert_eq!(fields(&founding.governing), [(founding.replica, true); 4]);
	assert_eq!(
		founding.replica,
		nibkeep_governance::replica_record(space, &key)
	);
	assert_eq!(
		founding.device,
		nibkeep_governance::device_record(space, &key)
	);
	let listing = founding
		.governing
		.iter()
		.find_map(|operation| match operation {
			nibkeep_model::Operation::Assert { statement, .. }
				if statement.field == nibkeep_model::fields::LISTING =>
			{
				statement
					.value
					.as_text()
					.and_then(nibkeep_governance::Listing::from_text)
			}
			_ => None,
		})
		.expect("a listing");
	assert!(listing.verifies());
	assert_eq!(
		(listing.space, listing.owner, listing.ai_agent),
		(space, founding.user, false)
	);
	assert_eq!(
		listing.encryption,
		nibkeep_governance::encryption_hash(&encryption.to_bytes())
	);
	assert!(
		fields(&founding.operations)
			.iter()
			.all(|(_, governing)| !governing)
	);
}

#[test]
fn founding_records_the_languages_the_user_reads_if_any() {
	let mut rng = TestRng::new("languages");
	let (signing, encryption, space) = keys(&mut rng);
	let ranked: Languages = "en, *-Latn, ru".parse().expect("ranges");
	for (languages, recorded) in [
		(ranked, Some("en, *-Latn, ru")),
		(Languages::default(), None),
	] {
		let founding = found(
			space,
			("Ada", "Ada"),
			("laptop", "nib"),
			&languages,
			(&signing, &encryption),
			&mut rng,
		);
		let mut state = State::new();
		for operation in founding.governing.iter().chain(&founding.operations) {
			state.apply(operation, Timestamp::new(1, 0));
		}
		let current = state.current(founding.user, &LANGUAGES).infallible();
		assert_eq!(
			current
				.first()
				.and_then(|statement| statement.value.as_text()),
			recorded
		);
	}
}

#[test]
fn a_link_proves_that_the_giver_held_the_key_it_names_for_that_gift_alone() {
	use nibkeep_crypto::{DecryptionKey, Key};
	use nibkeep_governance::SealedSecretsKey;
	let mut rng = TestRng::new("links");
	let space = nibkeep_base::SpaceId::random(&mut rng);
	let replica = EntityId::random(&mut rng);
	let recipient = DecryptionKey::generate(&mut rng).encryption_key();
	let (held, new) = (Key::generate(&mut rng), Key::generate(&mut rng));
	let linked = SealedSecretsKey::seal(
		&recipient,
		space,
		replica,
		(9, &new, nibkeep_base::Hash::of(b"the new key's hash")),
		(space, 3, &held),
		&mut rng,
	);
	assert_eq!((linked.link.space, linked.link.epoch), (space, 3));
	assert!(linked.link_holds(space, replica, &held));
	// Not with another key, nor for another replica or space.
	assert!(!linked.link_holds(space, replica, &new));
	assert!(!linked.link_holds(space, EntityId::random(&mut rng), &held));
	assert!(!linked.link_holds(nibkeep_base::SpaceId::random(&mut rng), replica, &held));
	// Nor for another key's number.
	let mut renumbered = linked.clone();
	renumbered.epoch = 10;
	assert!(!renumbered.link_holds(space, replica, &held));
	// Its text keeps it, and a gift without one does not read.
	let read = SealedSecretsKey::from_text(&linked.to_text()).expect("reads");
	assert_eq!(read, linked);
	let nibkeep_base::cbor::Value::Array(mut items) = nibkeep_base::cbor::ToCbor::to_cbor(&linked)
	else {
		panic!("a gift is an array");
	};
	items.pop();
	let unlinked = nibkeep_base::cbor::encode(&nibkeep_base::cbor::Value::Array(items));
	let text = <base64ct::Base64UrlUnpadded as base64ct::Encoding>::encode_string(&unlinked);
	assert_eq!(SealedSecretsKey::from_text(&text), None);
}
