//! AI agents on the user's own replica ([technical design: AI agents]): with the person's
//! consent, each AI client works on the user's replica with a key of its own, kept there,
//! listed on the user's own device, named in history as the client, unable to do what an AI
//! agent may not, reading only the personal space, and revoked on its own.
//!
//! [technical design: AI agents]: ../../../docs/Technical-design.md#14-ai-agents

mod common;

use std::path::{Path, PathBuf};

use common::{library, now, pair_named, replica_keys, sync_in_memory};
use nibkeep_base::cbor::{self, Value as Cbor};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{Envelope, SigningKey};
use nibkeep_model::{Name, TypeDef, TypeName, Value, fields};
use nibkeep_node::{Library, NodeError, PairingCode, Unlocker};
use nibkeep_testkit::TestRng;

const UNLOCKER: Unlocker<'static> = Unlocker::Passphrase("correct horse");

/// A laptop with the type of books, and a desk paired with it.
struct Setup {
	rng: TestRng,
	directory: PathBuf,
	laptop: Library,
	desk: Library,
	book: TypeName,
	at: u64,
}

impl Setup {
	fn new(seed: &str) -> Self {
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
		let desk_directory =
			nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{seed} desk"));
		std::fs::remove_dir(&desk_directory).expect("removes");
		let code = PairingCode::generate(&mut rng);
		let (paired, joined) = pair_named(
			&mut laptop,
			&desk_directory,
			"desk",
			(&code, &code),
			&mut rng,
		);
		paired.expect("paired");
		let desk = joined.expect("joined").library;
		Self {
			rng,
			directory,
			laptop,
			desk,
			book,
			at: now() + 100_000,
		}
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn personal(&self) -> SpaceId {
		self.laptop.personal_space()
	}

	/// The laptop's library opened again, as `nib mcp` opens it for an AI client.
	fn opened(&self) -> Library {
		Library::open(&self.directory, &UNLOCKER).expect("opens")
	}

	/// The laptop's library, opened again and acting as the AI client `name`.
	fn acting(&mut self, name: &str) -> Result<Library, NodeError> {
		let mut library = self.opened();
		let at = self.tick();
		library.act_as_ai_agent(name, at, &mut self.rng)?;
		Ok(library)
	}

	fn add(&mut self, library: &mut Library, name: &str) -> Result<EntityId, NodeError> {
		let at = self.tick();
		library.create_entity(
			library.personal_space(),
			std::slice::from_ref(&self.book),
			vec![(fields::NAME, Value::Name(Name::new(name)))],
			at,
			&mut self.rng,
		)
	}

	fn synced(&mut self) {
		let at = self.tick();
		self.laptop.catch_up().expect("catches up");
		let (laptop, desk) = sync_in_memory(&mut self.laptop, &mut self.desk, at, (0, 1));
		laptop.expect("the laptop synced");
		desk.expect("the desk synced");
	}
}

/// The key of the AI client `name` that the replica in `directory` keeps, read from its
/// files as an infected device could.
fn agent_key(directory: &Path, personal: SpaceId, name: &str) -> SigningKey {
	let (replica, _) = replica_keys(directory, &UNLOCKER, personal);
	let path = common::replica_folder(directory).join("agents.cbor");
	let envelope: Envelope = cbor::from_slice(&std::fs::read(path).expect("reads")).expect("CBOR");
	let plain = envelope
		.open(
			&replica.storage.derive("nibkeep 2026-11 ai agents"),
			b"nibkeep 2026-11 ai agents file",
		)
		.expect("opens");
	let Cbor::Array(items) = cbor::from_slice::<Cbor>(&plain).expect("CBOR") else {
		panic!("an array");
	};
	let Cbor::Array(keys) = &items[2] else {
		panic!("the keys");
	};
	keys.iter()
		.find_map(|key| match key {
			Cbor::Array(parts) => match parts.as_slice() {
				[Cbor::Text(named), Cbor::Bytes(signing), _] if named == name => Some(
					SigningKey::from_bytes(&signing.as_slice().try_into().expect("32 bytes")),
				),
				_ => None,
			},
			_ => None,
		})
		.expect("the client's key")
}

fn refused<T: std::fmt::Debug>(result: &Result<T, NodeError>) -> bool {
	matches!(result, Err(NodeError::AiAgentMayNot(_)))
}

#[test]
fn an_ai_client_works_only_with_the_persons_consent() {
	let mut setup = Setup::new("ai keys consent");
	assert!(!setup.laptop.ai_agents_enabled().expect("reads"));
	assert!(matches!(
		setup.acting("Claude Code"),
		Err(NodeError::AiAgentsNotAllowedHere)
	));
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables");
	assert!(setup.opened().ai_agents_enabled().expect("reads"));
	let agent = setup.acting("Claude Code").expect("acts");
	assert_eq!(agent.acting_as(), Some("Claude Code"));
	assert!(agent.is_ai_agent().expect("knows"));
	assert!(
		!setup.laptop.is_ai_agent().expect("knows"),
		"the person's own"
	);
}

#[test]
fn an_ai_clients_changes_are_its_own_on_the_persons_device() {
	let mut setup = Setup::new("ai keys own");
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables");
	let mut agent = setup.acting("Claude Code").expect("acts");
	let book = setup.add(&mut agent, "Leviathan Wakes").expect("adds");
	drop(agent);
	// The same client again takes the same key.
	let mut again = setup.acting("Claude Code").expect("acts again");
	setup.add(&mut again, "Caliban's War").expect("adds");
	drop(again);
	setup.synced();
	let personal = setup.personal();
	for library in [&setup.laptop, &setup.desk] {
		let history = library
			.history(personal, book, Some(&fields::NAME))
			.expect("reads")
			.expect("a history");
		let author = history.fields[0].values[0]
			.author
			.clone()
			.expect("an author");
		assert_eq!(author.user.name.as_deref(), Some("Ada"));
		assert_eq!(author.ai_agent.as_deref(), Some("Claude Code"));
	}
	let replicas = setup.laptop.replicas().expect("lists");
	let agents: Vec<_> = replicas.iter().filter(|replica| replica.ai_agent).collect();
	assert_eq!(agents.len(), 1, "one key for one client: {replicas:?}");
	let own = replicas
		.iter()
		.find(|replica| replica.this)
		.expect("this replica");
	assert_eq!(agents[0].name.as_deref(), Some("Claude Code"));
	assert_eq!(agents[0].device, own.device, "on the person's own device");
	assert_eq!(agents[0].endpoint, None, "nothing connects to it");
	let devices: std::collections::BTreeSet<_> = replicas
		.iter()
		.filter_map(|replica| replica.device)
		.collect();
	assert_eq!(devices.len(), 2, "the laptop and the desk alone");
	assert_eq!(
		setup
			.desk
			.entities(personal, Some(&setup.book))
			.expect("lists")
			.len(),
		2
	);
}

#[test]
fn an_ai_client_may_not_do_what_an_ai_agent_may_not_and_reads_only_the_personal_space() {
	let mut setup = Setup::new("ai keys refused");
	let at = setup.tick();
	let shared = setup
		.laptop
		.create_space("Family", at, &mut setup.rng)
		.expect("makes a space");
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables");
	let mut agent = setup.acting("Claude Code").expect("acts");
	let personal = setup.personal();
	assert_eq!(agent.spaces(), [personal], "not {shared}");
	let at = setup.tick();
	let desk = setup
		.laptop
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| !replica.this && !replica.ai_agent)
		.expect("the desk");
	assert!(refused(&agent.define_type(
		personal,
		&TypeDef::new(TypeName::from_static("local.film"), 1),
		at,
		&mut setup.rng
	)));
	assert!(refused(&agent.revoke(&[desk.id], at, &mut setup.rng)));
	assert!(refused(&agent.enable_ai_agents(&mut setup.rng)));
	assert!(refused(&agent.disable_ai_agents(at, &mut setup.rng)));
	assert!(refused(&agent.create_space("Work", at, &mut setup.rng)));
}

#[test]
fn what_an_ai_clients_key_may_not_write_counts_nowhere() {
	let mut setup = Setup::new("ai keys forged");
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables");
	drop(setup.acting("Claude Code").expect("acts"));
	setup.synced();
	let personal = setup.personal();
	let key = agent_key(&setup.directory, personal, "Claude Code");
	let desk = setup
		.desk
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.expect("the desk");
	let statement = setup.rng.statement_id();
	let operation = nibkeep_model::Operation::assert(nibkeep_model::Statement::new(
		statement,
		desk.id,
		fields::ENDPOINT,
		Value::Text("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()),
	));
	// What a program that holds the key writes to the person's replica with it.
	let at = setup.tick();
	common::forge(
		&setup.directory,
		&UNLOCKER,
		(personal, vec![operation]),
		(Some(&key), at),
		&mut setup.rng,
	);
	setup.synced();
	assert!(
		!setup.desk.set_aside().expect("lists").is_empty(),
		"the desk sets it aside"
	);
	let shown = setup
		.desk
		.entity(personal, desk.id, common::this_day())
		.expect("reads")
		.expect("the desk's record");
	assert!(
		shown
			.values(&fields::ENDPOINT)
			.iter()
			.all(|shown| shown.statement != statement)
	);
}

#[test]
fn a_revoked_client_is_refused_until_the_person_allows_ai_agents_again() {
	let mut setup = Setup::new("ai keys revoked");
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables");
	drop(setup.acting("Claude Code").expect("acts"));
	drop(setup.acting("Codex").expect("acts"));
	setup.synced();
	// The desk revokes one client by its name.
	let named = setup.desk.to_revoke("Codex").expect("names it");
	assert_eq!(named.len(), 1);
	assert!(named[0].ai_agent);
	let at = setup.tick();
	let ids: Vec<EntityId> = named.iter().map(|replica| replica.id).collect();
	setup
		.desk
		.revoke(&ids, at, &mut setup.rng)
		.expect("revokes");
	setup.synced();
	assert!(matches!(
		setup.acting("Codex"),
		Err(NodeError::AiAgentRevoked(name)) if name == "Codex"
	));
	let mut claude = setup.acting("Claude Code").expect("the other still works");
	setup.add(&mut claude, "Abaddon's Gate").expect("adds");
	drop(claude);
	// Withdrawing the consent revokes every key left.
	let at = setup.tick();
	let revoked = setup
		.laptop
		.disable_ai_agents(at, &mut setup.rng)
		.expect("disables");
	assert_eq!(revoked, ["Claude Code"]);
	assert!(matches!(
		setup.acting("Claude Code"),
		Err(NodeError::AiAgentsNotAllowedHere)
	));
	setup
		.laptop
		.enable_ai_agents(&mut setup.rng)
		.expect("enables again");
	let mut codex = setup.acting("Codex").expect("a new key");
	setup.add(&mut codex, "Cibola Burn").expect("adds");
	drop(codex);
	setup.laptop.catch_up().expect("catches up");
	let replicas = setup.laptop.replicas().expect("lists");
	let codex: Vec<_> = replicas
		.iter()
		.filter(|replica| replica.ai_agent && replica.name.as_deref() == Some("Codex"))
		.collect();
	assert_eq!(codex.len(), 2, "the revoked key and the new one");
	assert_eq!(codex.iter().filter(|replica| replica.revoked).count(), 1);
}
