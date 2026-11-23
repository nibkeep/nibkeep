//! AI agents as replicas of their user ([technical design: AI agents]): paired with a key of their
//! own and no secrets key, marked as AI agents' in the list of replicas, named in history
//! as such, unable to purge, govern or change the library's own records, and revoked on
//! their own.
//!
//! [technical design: AI agents]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#14-ai-agents

mod common;

use std::path::PathBuf;

use common::{library, now, pair_ai_agent_named, pair_named, sync_in_memory};
use nibkeep_base::EntityId;
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, Library, Needs, NewProtection, NodeError, PairingCode};
use nibkeep_testkit::TestRng;

const LAPTOP: usize = 0;
const AGENT: usize = 1;
const DESK: usize = 2;

/// A laptop with a book, an AI agent named Claude paired with it on the same computer,
/// and, if asked, a desk paired with it too.
struct Devices {
	rng: TestRng,
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	book: TypeName,
	at: u64,
}

impl Devices {
	fn new(seed: &str, with_desk: bool) -> Self {
		let mut rng = TestRng::new(seed);
		let (directory, mut laptop) = library(&format!("{seed} laptop"), &mut rng);
		let book = TypeName::from_static("local.book");
		laptop
			.define_type(
				laptop.personal_space(),
				&TypeDef::new(book.clone(), 1),
				now(),
				&mut rng,
			)
			.expect("defines");
		let mut devices = Self {
			rng,
			libraries: vec![laptop],
			directories: vec![directory],
			book,
			at: now() + 100_000,
		};
		devices.add(LAPTOP, "Leviathan Wakes");
		let directory = Self::new_directory(&format!("{seed} agent"));
		let code = PairingCode::generate(&mut devices.rng);
		let (paired, joined) = pair_ai_agent_named(
			&mut devices.libraries[LAPTOP],
			&directory,
			("laptop", "Claude"),
			&code,
			&mut devices.rng,
		);
		paired.expect("paired");
		devices.libraries.push(joined.expect("joined").library);
		devices.directories.push(directory);
		if with_desk {
			let directory = Self::new_directory(&format!("{seed} desk"));
			let code = PairingCode::generate(&mut devices.rng);
			let (paired, joined) = pair_named(
				&mut devices.libraries[LAPTOP],
				&directory,
				"desk",
				(&code, &code),
				&mut devices.rng,
			);
			paired.expect("paired");
			devices.libraries.push(joined.expect("joined").library);
			devices.directories.push(directory);
			devices.synced(LAPTOP, AGENT);
		}
		devices
	}

	fn new_directory(name: &str) -> PathBuf {
		let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
		std::fs::remove_dir(&directory).expect("removes");
		directory
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Adds a book with a name on a device, and returns it.
	fn add(&mut self, device: usize, name: &str) -> EntityId {
		let at = self.tick();
		let library = &mut self.libraries[device];
		library
			.create_entity(
				library.personal_space(),
				std::slice::from_ref(&self.book),
				vec![(fields::NAME, Value::Name(Name::new(name)))],
				at,
				&mut self.rng,
			)
			.expect("adds")
	}

	/// The names of the books a device shows, sorted.
	fn books(&self, device: usize) -> Vec<String> {
		let library = &self.libraries[device];
		let mut books: Vec<String> = library
			.entities(library.personal_space(), Some(&self.book))
			.expect("lists")
			.into_iter()
			.filter_map(|listed| listed.name)
			.collect();
		books.sort();
		books
	}

	fn sync(
		&mut self,
		a: usize,
		b: usize,
	) -> (
		Result<nibkeep_node::Synced, NodeError>,
		Result<nibkeep_node::Synced, NodeError>,
	) {
		let at = self.tick();
		let (first, second) = if a < b {
			let (left, right) = self.libraries.split_at_mut(b);
			(&mut left[a], &mut right[0])
		} else {
			let (left, right) = self.libraries.split_at_mut(a);
			(&mut right[0], &mut left[b])
		};
		sync_in_memory(first, second, at, (a, b))
	}

	fn synced(&mut self, a: usize, b: usize) {
		let (first, second) = self.sync(a, b);
		first.expect("the first synced");
		second.expect("the second synced");
	}

	/// How many changes a device's personal space holds.
	fn changes(&self, device: usize) -> usize {
		self.libraries[device].status().expect("a status").spaces[0].changes
	}

	fn personal(&self) -> nibkeep_base::SpaceId {
		self.libraries[LAPTOP].personal_space()
	}
}

fn refused<T: std::fmt::Debug>(result: &Result<T, NodeError>) -> bool {
	matches!(result, Err(NodeError::AiAgentMayNot(_)))
}

#[test]
fn an_ai_agent_is_paired_as_a_replica_of_its_own() {
	let mut devices = Devices::new("ai agent paired", false);
	let protection = NewProtection::Passphrase(
		"secrets of the laptop",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	devices.libraries[LAPTOP]
		.set_up_secrets(&protection, now() + 500, &mut devices.rng)
		.expect("sets up secrets");
	devices.synced(AGENT, LAPTOP);
	let replicas = devices.libraries[LAPTOP].replicas().expect("lists");
	let agent = replicas
		.iter()
		.find(|replica| replica.ai_agent)
		.expect("an AI agent's replica");
	assert_eq!(agent.name.as_deref(), Some("Claude"));
	assert_eq!(
		replicas.iter().filter(|replica| replica.ai_agent).count(),
		1
	);
	assert!(devices.libraries[AGENT].is_ai_agent().expect("knows"));
	assert!(!devices.libraries[LAPTOP].is_ai_agent().expect("knows"));
	assert!(
		devices.libraries[AGENT]
			.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.this && replica.ai_agent)
	);
	assert_eq!(devices.books(AGENT), ["Leviathan Wakes"]);
	assert!(
		devices.libraries[AGENT]
			.secrets_protection()
			.expect("reads")
			.is_none(),
		"an AI agent gets no secrets key"
	);
}

#[test]
fn an_ai_agent_may_not_purge_govern_or_change_the_librarys_records() {
	let mut devices = Devices::new("ai agent refused", false);
	let personal = devices.personal();
	let at = devices.tick();
	let book = devices.libraries[AGENT]
		.entities(personal, Some(&devices.book))
		.expect("lists")[0]
		.id;
	let laptop = devices.libraries[LAPTOP].replicas().expect("lists")[0].clone();
	let agent = &mut devices.libraries[AGENT];
	let rng = &mut devices.rng;
	assert!(refused(&agent.purge(
		personal,
		book,
		&fields::NAME,
		None,
		at,
		rng
	)));
	assert!(refused(&agent.define_type(
		personal,
		&TypeDef::new(TypeName::from_static("local.film"), 1),
		at,
		rng
	)));
	// Nor deletes or restores a type, whose definitions are the library's records.
	assert!(refused(&agent.delete_type(
		personal,
		&TypeName::from_static("local.book"),
		at,
		rng
	)));
	assert!(refused(&agent.restore_type(
		personal,
		&TypeName::from_static("local.book"),
		at,
		rng
	)));
	assert!(refused(&agent.revoke(&[laptop.id], at, rng)));
	let dating = common::today();
	let endpoint = Edit::Set(
		fields::ENDPOINT,
		vec![Value::Text(
			"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
		)],
	);
	assert!(refused(&agent.edit_entity(
		personal,
		laptop.id,
		vec![endpoint],
		&dating,
		at,
		rng
	)));
	// Records keep the types that NibKeep gave them, for every replica.
	let replica_type = Edit::AddType(TypeName::from_static("org.nibkeep.replica"));
	assert!(
		agent
			.edit_entity(personal, book, vec![replica_type], &dating, at, rng)
			.is_err()
	);
	// The data it may change, and settling keys after a sync governs nothing.
	let before = devices.changes(AGENT);
	let agent = &mut devices.libraries[AGENT];
	agent
		.edit_entity(
			personal,
			book,
			vec![Edit::Set(
				fields::NAME,
				vec![Value::Name(Name::new("Caliban's War"))],
			)],
			&dating,
			at,
			&mut devices.rng,
		)
		.expect("edits the book");
	agent
		.settle_keys(at, &mut devices.rng)
		.expect("settles its keys");
	assert_eq!(devices.changes(AGENT), before + 1);
	devices.synced(AGENT, LAPTOP);
	assert_eq!(devices.books(LAPTOP), ["Caliban's War"]);
}

#[test]
fn an_ai_agent_may_not_take_away_or_delete_the_librarys_records() {
	let mut devices = Devices::new("ai agent takes away", false);
	let personal = devices.personal();
	let at = devices.tick();
	let films = devices.libraries[LAPTOP]
		.add_policy(personal, "Films", Needs::FALLBACK, at, &mut devices.rng)
		.expect("adds a policy");
	// A record that the laptop deleted, which the AI agent may not bring back.
	let laptop_record = devices.libraries[LAPTOP].replicas().expect("lists")[0].clone();
	let device = laptop_record.device.expect("a device record");
	let dating = common::today();
	devices.libraries[LAPTOP]
		.edit_entity(
			personal,
			device,
			vec![Edit::Delete],
			&dating,
			at,
			&mut devices.rng,
		)
		.expect("deletes its device's record");
	devices.synced(LAPTOP, AGENT);
	let at = devices.tick();
	let laptop = laptop_record.id;
	let before = devices.changes(AGENT);
	let agent = &mut devices.libraries[AGENT];
	let book = agent
		.entities(personal, Some(&devices.book))
		.expect("lists")[0]
		.id;
	let rng = &mut devices.rng;
	// Its user's records stay as they are: a replica's record and its endpoint, a deleted
	// device's record, which every replica would take as an AI agent's change, and a
	// storage policy, which no book is merged into.
	let edits = [
		(laptop, Edit::Set(fields::ENDPOINT, Vec::new())),
		(laptop, Edit::Delete),
		(device, Edit::Restore),
	];
	for (entity, edit) in edits {
		let result = agent.edit_entity(personal, entity, vec![edit.clone()], &dating, at, rng);
		assert!(refused(&result), "{edit:?}: {result:?}");
	}
	let removal = agent.remove_policy(personal, films, at, rng);
	assert!(refused(&removal), "{removal:?}");
	let merge = agent.merge_entities(personal, book, films, at, rng);
	assert!(refused(&merge), "{merge:?}");
	assert_eq!(devices.changes(AGENT), before);
	let agent = &mut devices.libraries[AGENT];
	let rng = &mut devices.rng;
	// Its user's data it may still delete, and restore.
	agent
		.edit_entity(personal, book, vec![Edit::Delete], &dating, at, rng)
		.expect("deletes the book");
	devices.synced(AGENT, LAPTOP);
	nibkeep_testkit::assert_empty!(devices.books(LAPTOP));
	let at = devices.tick();
	devices.libraries[AGENT]
		.edit_entity(
			personal,
			book,
			vec![Edit::Restore],
			&dating,
			at,
			&mut devices.rng,
		)
		.expect("restores the book");
	devices.synced(AGENT, LAPTOP);
	assert_eq!(devices.books(LAPTOP), ["Leviathan Wakes"]);
}

#[test]
fn history_names_an_ai_agents_changes_as_its_own() {
	let mut devices = Devices::new("ai agent named", false);
	let book = devices.add(AGENT, "Abaddon's Gate");
	devices.synced(AGENT, LAPTOP);
	let personal = devices.personal();
	for device in [LAPTOP, AGENT] {
		let history = devices.libraries[device]
			.history(personal, book, Some(&fields::NAME))
			.expect("reads")
			.expect("a history");
		let author = history.fields[0].values[0]
			.author
			.clone()
			.expect("an author");
		assert_eq!(author.user.name.as_deref(), Some("Ada"));
		assert_eq!(author.ai_agent.as_deref(), Some("Claude"), "{device}");
	}
	let first = devices.libraries[LAPTOP]
		.entities(personal, Some(&devices.book))
		.expect("lists")
		.into_iter()
		.find(|listed| listed.name.as_deref() == Some("Leviathan Wakes"))
		.expect("the first book")
		.id;
	let history = devices.libraries[LAPTOP]
		.history(personal, first, Some(&fields::NAME))
		.expect("reads")
		.expect("a history");
	let author = history.fields[0].values[0]
		.author
		.clone()
		.expect("an author");
	assert_eq!(author.ai_agent, None, "the user's own change");
}

#[test]
fn a_revoked_ai_agent_reads_nothing_added_later_and_the_rest_keep_working() {
	let mut devices = Devices::new("ai agent revoked", true);
	// The AI agent's device has the laptop's name, which still names the laptop alone.
	let by_device = devices.libraries[DESK]
		.to_revoke("laptop")
		.expect("names one");
	assert_eq!(by_device.len(), 1);
	assert!(!by_device[0].ai_agent);
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	let named = laptop.to_revoke("Claude").expect("names the AI agent");
	assert_eq!(named.len(), 1);
	assert!(named[0].ai_agent);
	let ids: Vec<EntityId> = named.iter().map(|replica| replica.id).collect();
	laptop.revoke(&ids, at, &mut devices.rng).expect("revokes");
	devices.add(LAPTOP, "Cibola Burn");
	devices.synced(LAPTOP, DESK);
	assert_eq!(devices.books(DESK), ["Cibola Burn", "Leviathan Wakes"]);
	let (agent, laptop) = devices.sync(AGENT, LAPTOP);
	assert!(agent.is_err() && laptop.is_err(), "the laptop refuses it");
	assert_eq!(devices.books(AGENT), ["Leviathan Wakes"]);
	// The desk, on whose device name the AI agent was too, keeps syncing.
	devices.add(DESK, "Nemesis Games");
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.books(LAPTOP).len(), 3);
}

#[test]
fn a_secret_field_is_marked_for_good_and_an_ai_agent_cannot_unmark_it() {
	let mut devices = Devices::new("ai agent secrets", false);
	let personal = devices.personal();
	let pin: FieldName = "local.pin".parse().expect("a field");
	let login = TypeDef::new(TypeName::from_static("local.login"), 1)
		.with_field(pin.clone(), FieldDef::new(ValueKind::Text).secret());
	let at = devices.tick();
	devices.libraries[LAPTOP]
		.define_type(personal, &login, at, &mut devices.rng)
		.expect("defines");
	devices.synced(LAPTOP, AGENT);
	let agent = &devices.libraries[AGENT];
	assert!(
		agent
			.secret_fields(personal)
			.expect("a space")
			.contains(&pin)
	);
	// Which the laptop's record says, in a statement that governs the space.
	let laptop = devices.libraries[LAPTOP].replicas().expect("lists")[0].id;
	let history = agent
		.history(personal, laptop, Some(&fields::SECRET_FIELD))
		.expect("reads")
		.expect("a history");
	let marks: Vec<&Value> = history.fields[0]
		.values
		.iter()
		.map(|value| &value.value)
		.collect();
	// Beside the vocabulary's, which the library marked when it was founded, each once.
	let mut expected: Vec<Value> = nibkeep_std::secret_fields()
		.iter()
		.map(|field| Value::Text(field.to_string()))
		.collect();
	expected.push(Value::Text("local.pin".to_owned()));
	let mut marks: Vec<Value> = marks.into_iter().cloned().collect();
	marks.sort_by_key(|mark| mark.as_text().map(str::to_owned));
	expected.sort_by_key(|mark| mark.as_text().map(str::to_owned));
	assert_eq!(marks, expected);
	// The AI agent may not take it back.
	let at = devices.tick();
	let dating = common::today();
	let agent = &mut devices.libraries[AGENT];
	let unmarking = agent.edit_entity(
		personal,
		laptop,
		vec![Edit::Set(fields::SECRET_FIELD, Vec::new())],
		&dating,
		at,
		&mut devices.rng,
	);
	assert!(
		matches!(unmarking, Err(NodeError::AiAgentMayNot(_))),
		"{unmarking:?}"
	);
}

/// An AI agent's replica that writes, by other means than NibKeep's programs, what an AI agent
/// may not, such as a replica's endpoint or an unknown field of NibKeep's own, has that change
/// set aside by the devices it syncs with.
#[test]
fn what_an_ai_agent_may_not_write_counts_nowhere_it_arrives() {
	let mut devices = Devices::new("ai agent writes what it may not", false);
	let personal = devices.personal();
	let laptop = devices.libraries[LAPTOP].replicas().expect("lists")[0].clone();
	let unlocker = nibkeep_node::Unlocker::Passphrase("battery staple");
	let cases = [
		(
			fields::ENDPOINT,
			Value::Text("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()),
		),
		(
			"org.nibkeep.something-new".parse().expect("a field name"),
			Value::Text("?".to_owned()),
		),
	];
	for (count, (field, value)) in cases.into_iter().enumerate() {
		// It writes its own files while its program is closed.
		let directory = devices.directories[AGENT].clone();
		drop(devices.libraries.remove(AGENT));
		let at = devices.tick();
		let statement = devices.rng.statement_id();
		let operation = nibkeep_model::Operation::assert(nibkeep_model::Statement::new(
			statement,
			laptop.id,
			field.clone(),
			value,
		));
		common::forge(
			&directory,
			&unlocker,
			(personal, vec![operation]),
			(None, at),
			&mut devices.rng,
		);
		devices
			.libraries
			.insert(AGENT, Library::open(&directory, &unlocker).expect("opens"));
		devices.synced(AGENT, LAPTOP);
		assert_eq!(
			devices.libraries[LAPTOP].set_aside().expect("lists").len(),
			count + 1,
			"the laptop sets aside what it wrote to {field}"
		);
		let shown = devices.libraries[LAPTOP]
			.entity(personal, laptop.id, common::this_day())
			.expect("reads")
			.expect("the laptop's record");
		// The laptop states its own endpoint there as it syncs.
		assert!(
			shown
				.values(&field)
				.iter()
				.all(|shown| shown.statement != statement),
			"what it wrote to {field} does not show"
		);
	}
}
