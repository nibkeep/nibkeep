//! AI agents in the spaces their user chooses ([technical design: sharing]): an AI agent's replica
//! gets the personal space and the spaces chosen when it is paired, and no other; in a
//! shared space, its listing counts only where the admins allow AI agents, and every member
//! sees which AI agents read it.
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

mod common;

use common::{TIMEOUT, invite_member, now, protection, sync_in_memory, user_library};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_model::{FieldName, Name, TypeName, Value};
use nibkeep_node::{
	Joining, Library, MemberRole, NetworkOptions, NodeError, PairingCode, join, pair_ai_agent_in,
};
use nibkeep_testkit::TestRng;

/// Pairs a new replica of an AI agent named Claude with `sponsor`, listed in `spaces` too.
fn paired_ai_agent(
	sponsor: &mut Library,
	spaces: &[SpaceId],
	rng: &mut TestRng,
) -> Result<Library, NodeError> {
	let directory = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&common::unique_folder("an AI agent"),
	);
	std::fs::remove_dir(&directory).expect("removes");
	let code = PairingCode::generate(rng);
	let sponsor_network = sponsor.network(NetworkOptions::LOCAL).expect("starts");
	let joining = Joining::new("laptop", "nib", rng).expect("names");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		sponsor_network.endpoint(),
		&sponsor_network.local_addresses(),
		Some(&code.announcement()),
	);
	let mut sponsor_rng = TestRng::new("sponsor of an AI agent");
	std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			pair_ai_agent_in(
				sponsor,
				&sponsor_network,
				&code,
				("Claude", spaces),
				nibkeep_node::Showing::new(TIMEOUT),
				&|| now() + 60_000,
				&mut sponsor_rng,
			)
		});
		let joined = join(
			&directory,
			joining,
			&joiner_network,
			&code,
			&protection(),
			TIMEOUT,
			&|| now() + 60_000,
			rng,
		);
		paired.join().expect("the sponsor ran")?;
		Ok(joined?.library)
	})
}

/// Pairs an AI agent with `sponsor`, listed in `spaces` too, with nobody to join: what is
/// refused before the code is shown.
fn pair_alone(sponsor: &mut Library, spaces: &[SpaceId], rng: &mut TestRng) -> NodeError {
	let network = sponsor.network(NetworkOptions::LOCAL).expect("starts");
	match pair_ai_agent_in(
		sponsor,
		&network,
		&PairingCode::generate(rng),
		("Claude", spaces),
		nibkeep_node::Showing::new(std::time::Duration::from_secs(1)),
		&|| now() + 60_000,
		rng,
	) {
		Ok(_) => panic!("nobody joined"),
		Err(error) => error,
	}
}

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// Adds a person named `name` to `space`.
fn note(library: &mut Library, space: SpaceId, name: &str, at: u64, rng: &mut TestRng) -> EntityId {
	library
		.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(
				"org.nibkeep.name".parse::<FieldName>().expect("a field"),
				Value::Name(Name::new(name)),
			)],
			at,
			rng,
		)
		.expect("adds")
}

/// The names of what a search finds in a space.
fn found(library: &Library, space: SpaceId, words: &str) -> Vec<String> {
	let mut names: Vec<String> = library
		.search(space, words)
		.expect("searches")
		.into_iter()
		.filter_map(|listed| listed.name)
		.collect();
	names.sort();
	names
}

#[test]
fn an_ai_agent_gets_the_spaces_chosen_for_it_and_no_other() {
	let mut rng = TestRng::new("an AI agent's spaces");
	let mut laptop = user_library("Ada", "laptop", &mut rng);
	let mut at = now() + 100_000;
	let household = laptop
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let private = laptop
		.create_space("Private", at + 10, &mut rng)
		.expect("makes it");
	note(&mut laptop, household, "Boiler service", at + 20, &mut rng);
	note(&mut laptop, private, "Diary", at + 30, &mut rng);
	// Ada is the admin of her own space, which then allows AI agents.
	assert!(!laptop.ai_agents_allowed(household).expect("reads"));
	let mut agent = paired_ai_agent(&mut laptop, &[household], &mut rng).expect("pairs");
	assert!(laptop.ai_agents_allowed(household).expect("reads"));
	assert!(!laptop.ai_agents_allowed(private).expect("reads"));

	// The AI agent takes the space chosen through the personal space, and syncs it.
	at = sync_rounds(&mut laptop, &mut agent, now() + 200_000, 3);
	assert_eq!(agent.spaces(), vec![agent.personal_space(), household]);
	assert_eq!(found(&agent, household, "boiler"), ["Boiler service"]);
	// Every member sees that an AI agent reads the space.
	let ai_agents: Vec<bool> = laptop
		.members(household)
		.expect("lists")
		.into_iter()
		.flat_map(|member| member.replicas)
		.map(|replica| replica.ai_agent)
		.collect();
	assert_eq!(ai_agents.iter().filter(|ai_agent| **ai_agent).count(), 1);
	// What it writes there counts.
	note(&mut agent, household, "Window cleaner", at, &mut rng);
	at = sync_rounds(&mut agent, &mut laptop, at + 1_000, 1);
	assert_eq!(found(&laptop, household, "window"), ["Window cleaner"]);
	// Nothing gives it the other space, sync after sync.
	sync_rounds(&mut laptop, &mut agent, at, 2);
	assert!(!agent.spaces().contains(&private));
}

#[test]
fn in_a_shared_space_an_ai_agent_needs_the_admins_to_allow_ai_agents() {
	let mut rng = TestRng::new("AI agents allowed");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	let at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	invite_member(
		&mut ada,
		(household, MemberRole::Editor),
		&mut bea,
		at + 100,
	);
	// Bea, an editor, cannot bring her AI agent where the admins allow none.
	let refused = pair_alone(&mut bea, &[household], &mut rng);
	assert!(
		matches!(refused, NodeError::AiAgentsNotAllowed(_)),
		"{refused:?}"
	);
	// Nor allow them herself.
	let allowed = bea.allow_ai_agents(household, true, at + 200, &mut rng);
	assert!(
		matches!(allowed, Err(NodeError::AdminsOnly(_))),
		"{allowed:?}"
	);
	ada.allow_ai_agents(household, true, at + 300, &mut rng)
		.expect("allows");
	let at = sync_rounds(&mut ada, &mut bea, at + 1_000, 1);
	assert!(bea.ai_agents_allowed(household).expect("reads"));
	let mut agent = paired_ai_agent(&mut bea, &[household], &mut rng).expect("pairs");
	let at = sync_rounds(&mut bea, &mut agent, at + 70_000, 3);
	let at = sync_rounds(&mut bea, &mut ada, at, 1);
	// Ada sees Bea's AI agent among Bea's replicas there.
	let bea_member = ada
		.members(household)
		.expect("lists")
		.into_iter()
		.find(|member| member.name.as_deref() == Some("Bea"))
		.expect("Bea is a member");
	assert_eq!(bea_member.replicas.len(), 2);
	assert_eq!(
		bea_member
			.replicas
			.iter()
			.filter(|replica| replica.ai_agent)
			.count(),
		1
	);
	// The personal space is never a space to choose.
	let personal = bea.personal_space();
	let refused = pair_alone(&mut bea, &[personal], &mut rng);
	assert!(matches!(refused, NodeError::NeverShared), "{refused:?}");

	// Once Ada stops allowing them, the AI agent's replica there ends, as a removal ends one,
	// and the space gets a new key at once, which it does not get.
	let before = ada.space_key_numbers(household).expect("reads").1;
	ada.allow_ai_agents(household, false, at, &mut rng)
		.expect("stops them");
	let new_key = ada.space_key_numbers(household).expect("reads").1;
	assert_ne!(new_key, before);
	note(&mut ada, household, "Gutters", at + 100, &mut rng);
	let at = sync_rounds(&mut ada, &mut bea, at + 1_000, 2);
	sync_rounds(&mut bea, &mut agent, at, 2);
	assert!(agent.has_left(household).expect("reads"));
	assert!(
		!agent
			.space_key_numbers(household)
			.expect("reads")
			.0
			.contains(&new_key)
	);
	assert_eq!(found(&agent, household, "gutters"), Vec::<String>::new());
	assert_eq!(found(&bea, household, "gutters"), ["Gutters"]);
}
