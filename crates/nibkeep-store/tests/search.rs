//! Search and indexes: built from the log in memory, following its changes, and never
//! holding a secret value.

use std::collections::BTreeSet;
use std::path::PathBuf;

use nibkeep_base::{EntityId, SpaceId, StatementId};
use nibkeep_crypto::SigningKey;
use nibkeep_log::{Parcel, SpaceKeys, WrappedKey};
use nibkeep_model::{FieldName, Operation, Statement, Value, fields};
use nibkeep_store::{Found, StoredLog, log_path};
use nibkeep_testkit::{TestRng, millis_at};

const DRIFT: u64 = 3_600_000;

const LOGIN: &str = "org.nibkeep.login";
const TITLE: &str = "org.nibkeep.title";
const WEBSITE: &str = "org.nibkeep.website";
const USERNAME: &str = "org.nibkeep.username";
const PASSWORD: &str = "org.nibkeep.password";
const SEED: &str = "org.nibkeep.one-time-code-seed";

/// An empty directory for one test.
fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

/// A device with a space's keys, its secrets key among them, and the space on its disk.
struct Device {
	rng: TestRng,
	keys: SpaceKeys,
	author: SigningKey,
	space: SpaceId,
	path: PathBuf,
	wall: u64,
}

impl Device {
	fn new(name: &str) -> Self {
		let mut rng = TestRng::new(name);
		let space = rng.space_id();
		let mut keys = SpaceKeys::new(0, rng.key());
		keys.insert_secrets(0, rng.key());
		Self::of(name, space, keys, rng)
	}

	/// Another device of the same space.
	fn another(&self, name: &str) -> Self {
		Self::of(name, self.space, self.keys.clone(), TestRng::new(name))
	}

	fn of(name: &str, space: SpaceId, keys: SpaceKeys, mut rng: TestRng) -> Self {
		Self {
			author: rng.signing_key(),
			rng,
			keys,
			space,
			path: log_path(&directory(name), space),
			wall: millis_at("2026-10-12", 10, 0),
		}
	}

	fn open(&self) -> StoredLog {
		StoredLog::open(
			&self.path,
			self.space,
			&self.keys,
			&nibkeep_testkit::storage_key(),
			DRIFT,
			[field(PASSWORD)],
		)
		.expect("opens")
	}

	/// Commits operations, a minute after the previous commit.
	fn commit(&mut self, stored: &mut StoredLog, operations: Vec<Operation>) {
		self.wall += 60_000;
		stored
			.commit(
				&self.author,
				&self.keys,
				operations,
				self.wall,
				&mut self.rng,
			)
			.expect("commits");
	}

	/// A statement of a field of an entity, with its ID.
	fn statement(&mut self, entity: EntityId, field: FieldName, value: Value) -> Statement {
		Statement::new(self.rng.statement_id(), entity, field, value)
	}

	/// Commits a login, and returns it with the IDs of its title and website statements.
	fn login(
		&mut self,
		stored: &mut StoredLog,
		title: &str,
		website: &str,
		username: &str,
		password: &str,
	) -> (EntityId, StatementId, StatementId) {
		let login = self.rng.entity_id();
		let statements = [
			(fields::TYPE, LOGIN),
			(field(TITLE), title),
			(field(WEBSITE), website),
			(field(USERNAME), username),
			(field(PASSWORD), password),
		]
		.map(|(field, value)| self.statement(login, field, Value::Text(value.into())));
		let (title, website) = (statements[1].id, statements[2].id);
		self.commit(
			stored,
			statements.into_iter().map(Operation::assert).collect(),
		);
		(login, title, website)
	}
}

/// The entities a search finds, in order.
fn entities(stored: &StoredLog, query: &str) -> Vec<EntityId> {
	stored
		.search(query)
		.expect("searches")
		.into_iter()
		.map(|found| found.entity)
		.collect()
}

/// Every change of `from` that `to` does not have, received in the order of their IDs.
fn sync(from: &StoredLog, to: &mut StoredLog, keys: &SpaceKeys, wall: u64) {
	let changes: Vec<Parcel> = from
		.log()
		.parcels()
		.expect("reads")
		.into_iter()
		.filter(|parcel| !to.log().contains(parcel.id()).expect("reads"))
		.collect();
	for change in changes {
		to.receive(&change, keys, wall).expect("accepts");
	}
}

#[test]
fn entities_are_found_by_their_words() {
	let mut device = Device::new("found by their words");
	let mut stored = device.open();
	let (netflix, ..) = device.login(&mut stored, "Netflix", "netflix.com", "ada", "pw-one");
	let (cafe, ..) = device.login(&mut stored, "Café Bleu", "cafe-bleu.fr", "zoë", "pw-two");
	let (spotify, ..) = device.login(&mut stored, "Spotify", "spotify.com", "ada", "pw-three");

	assert_eq!(entities(&stored, "netflix"), [netflix]);
	assert_eq!(
		stored.search("netflix").expect("searches"),
		[Found {
			entity: netflix,
			matching: 2,
		}],
		"both values with the word, its title and its website"
	);
	assert_eq!(
		entities(&stored, "NETF"),
		[netflix],
		"any case, and the start of a word"
	);
	assert_eq!(entities(&stored, "cafe"), [cafe], "without accents");
	assert_eq!(entities(&stored, "Zoe"), [cafe]);
	assert_eq!(
		entities(&stored, "ada")
			.into_iter()
			.collect::<BTreeSet<_>>(),
		BTreeSet::from([netflix, spotify])
	);
	assert_eq!(entities(&stored, "ada spot"), [spotify], "every word");
	assert_eq!(entities(&stored, "netflix.com ada"), [netflix]);
	nibkeep_testkit::assert_empty!(entities(&stored, "ada hulu"));
	nibkeep_testkit::assert_empty!(entities(&stored, ""));
	nibkeep_testkit::assert_empty!(entities(&stored, "  !! "));
}

#[test]
fn secret_values_are_never_indexed() {
	let mut device = Device::new("secrets are never indexed");
	let mut stored = device.open();
	let (login, ..) = device.login(&mut stored, "Netflix", "netflix.com", "ada", "hunter2");
	assert_eq!(entities(&stored, "netflix"), [login]);
	nibkeep_testkit::assert_empty!(entities(&stored, "hunter2"));
	nibkeep_testkit::assert_empty!(
		stored
			.find(&field(PASSWORD), &Value::Text("hunter2".into()))
			.expect("searches")
	);
	stored.unlock_secrets(&device.keys).expect("unlocks");
	nibkeep_testkit::assert_empty!(entities(&stored, "hunter2"), "not even when unlocked");
	drop(stored);
	nibkeep_testkit::assert_empty!(entities(&device.open(), "hunter2"));
}

#[test]
fn values_find_entities() {
	let mut device = Device::new("values find entities");
	let mut stored = device.open();
	let (netflix, ..) = device.login(&mut stored, "Netflix", "netflix.com", "ada", "pw-one");
	let (spotify, ..) = device.login(&mut stored, "Spotify", "spotify.com", "ada", "pw-two");
	let logins = stored
		.find(&fields::TYPE, &Value::Text(LOGIN.into()))
		.expect("searches");
	assert_eq!(
		logins.into_iter().collect::<BTreeSet<_>>(),
		BTreeSet::from([netflix, spotify])
	);
	assert_eq!(
		stored
			.find(&field(WEBSITE), &Value::Text("netflix.com".into()))
			.expect("searches"),
		[netflix]
	);
	nibkeep_testkit::assert_empty!(
		stored
			.find(&field(WEBSITE), &Value::Text("Netflix.com".into()))
			.expect("searches"),
		"exact values"
	);
}

#[test]
fn the_index_follows_changes_on_every_device() {
	let mut phone = Device::new("the index follows changes");
	let laptop = phone.another("the index follows changes on the laptop");
	let mut on_phone = phone.open();
	let (login, title, website) = phone.login(&mut on_phone, "Netflix", "netflix.com", "ada", "pw");

	// A new title replaces the old one, and the website goes.
	let family = phone.statement(login, field(TITLE), Value::Text("Hulu family".into()));
	phone.commit(
		&mut on_phone,
		vec![
			Operation::replace(family, [title]),
			Operation::retract(login, [website]),
		],
	);
	nibkeep_testkit::assert_empty!(entities(&on_phone, "netflix"));
	assert_eq!(entities(&on_phone, "hulu"), [login]);

	// Two records of the same person, merged into one.
	let (ada, augusta) = {
		let (first, second) = (phone.rng.entity_id(), phone.rng.entity_id());
		(first.min(second), first.max(second))
	};
	let name = field("org.nibkeep.name");
	let names = [
		phone.statement(ada, name.clone(), Value::Text("Ada Lovelace".into())),
		phone.statement(augusta, name, Value::Text("Augusta Ada King".into())),
	];
	phone.commit(
		&mut on_phone,
		names.into_iter().map(Operation::assert).collect(),
	);
	assert_eq!(
		entities(&on_phone, "ada").len(),
		3,
		"the login, and two records"
	);
	let merge = phone.statement(augusta, fields::MERGED_INTO, Value::Reference(ada));
	phone.commit(&mut on_phone, vec![Operation::assert(merge)]);
	let people: Vec<Found> = on_phone
		.search("ada")
		.expect("searches")
		.into_iter()
		.filter(|found| found.entity != login)
		.collect();
	assert_eq!(people.len(), 1, "one person");
	assert_eq!(people[0].entity, ada);
	assert_eq!(people[0].matching, 2);

	// A deleted login is not found.
	let deleted = phone.statement(login, fields::DELETED, Value::Boolean(true));
	phone.commit(&mut on_phone, vec![Operation::assert(deleted)]);
	nibkeep_testkit::assert_empty!(entities(&on_phone, "hulu"));
	nibkeep_testkit::assert_empty!(
		on_phone
			.find(&fields::TYPE, &Value::Text(LOGIN.into()))
			.expect("searches")
	);

	// The laptop gets the same index from the same changes.
	let mut on_laptop = laptop.open();
	sync(&on_phone, &mut on_laptop, &laptop.keys, phone.wall);
	for query in ["ada", "hulu", "netflix", "augusta king", "lovelace"] {
		assert_eq!(
			on_laptop.search(query).expect("searches"),
			on_phone.search(query).expect("searches"),
			"{query}"
		);
	}
}

#[test]
fn the_index_is_built_again_when_the_space_opens() {
	let mut device = Device::new("built again");
	let queries = ["netflix", "ada", "spot", "cafe bleu", "fr"];
	let results: Vec<Vec<Found>> = {
		let mut stored = device.open();
		device.login(&mut stored, "Netflix", "netflix.com", "ada", "pw-one");
		device.login(&mut stored, "Café Bleu", "cafe-bleu.fr", "zoë", "pw-two");
		device.login(&mut stored, "Spotify", "spotify.com", "ada", "pw-three");
		queries
			.iter()
			.map(|query| stored.search(query).expect("searches"))
			.collect()
	};
	let stored = device.open();
	for (query, expected) in queries.iter().zip(results) {
		assert!(!expected.is_empty(), "{query}");
		assert_eq!(stored.search(query).expect("searches"), expected, "{query}");
	}
}

#[test]
fn statements_that_arrived_sealed_are_never_indexed() {
	// The writer's types mark one-time code seeds as secret, and the reader's do not yet.
	let mut writer = Device::new("sealed by the writer");
	let reader = writer.another("sealed for the reader");
	let mut on_writer = StoredLog::open(
		&writer.path,
		writer.space,
		&writer.keys,
		&nibkeep_testkit::storage_key(),
		DRIFT,
		[field(PASSWORD), field(SEED)],
	)
	.expect("opens");
	let login = writer.rng.entity_id();
	let title = writer.statement(login, field(TITLE), Value::Text("Netflix".into()));
	let seed = writer.statement(login, field(SEED), Value::Text("JBSWY3DPEHPK3PXP".into()));
	let seed_id = seed.id;
	writer.commit(
		&mut on_writer,
		vec![Operation::assert(title), Operation::assert(seed)],
	);
	assert!(on_writer.log().is_sealed(seed_id).expect("reads"));

	let mut on_reader = reader.open();
	sync(&on_writer, &mut on_reader, &reader.keys, writer.wall);
	let wrapped: Vec<(StatementId, WrappedKey)> = on_writer
		.log()
		.wrapped_keys()
		.map(|(statement, wrapped)| (statement, wrapped.clone()))
		.collect();
	for (statement, wrapped) in wrapped {
		on_reader
			.receive_wrapped_key(statement, wrapped, &reader.keys)
			.expect("keeps the key");
	}
	on_reader.unlock_secrets(&reader.keys).expect("unlocks");
	assert!(
		on_reader.log().state().is_current(seed_id).expect("reads"),
		"the reader opened the seed"
	);
	assert_eq!(entities(&on_reader, "netflix"), [login]);
	nibkeep_testkit::assert_empty!(entities(&on_reader, "jbswy3dpehpk3pxp"));
	drop(on_reader);
	let reopened = reader.open();
	assert!(reopened.log().state().is_current(seed_id).expect("reads"));
	nibkeep_testkit::assert_empty!(entities(&reopened, "jbswy3dpehpk3pxp"));
}

#[test]
fn types_are_found_by_value_but_not_by_their_words() {
	let mut device = Device::new("types are not words");
	let mut stored = device.open();
	let (login, ..) = device.login(&mut stored, "Netflix", "netflix.com", "ada", "pw");
	nibkeep_testkit::assert_empty!(
		entities(&stored, "login"),
		"the type's name is no word of it"
	);
	nibkeep_testkit::assert_empty!(entities(&stored, "nibkeep"));
	assert_eq!(
		stored
			.find(&fields::TYPE, &Value::Text(LOGIN.into()))
			.expect("searches"),
		[login]
	);
}
