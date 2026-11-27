//! Devices' choices of a space ([technical design: devices that choose their spaces]): a
//! device drops a space by a choice signed by its key, stated in the personal space and in
//! the space, which ends it there as a removal does; the space's other devices give it a new
//! key, and a new secrets key, and the dropping device makes neither; a sync sends it only
//! what ended it; and only a newer choice of its own to hold the space brings it back.
//!
//! The people: Ada, with a desktop that made the space Project and a laptop, and Bea, an
//! editor there.
//!
//! [technical design: devices that choose their spaces]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces

use crate::common;

use crate::common::{
	LOGIN, invite_member, login_type, now, own_key, pair_joining, sync_rounds, today,
};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::{PassphraseParams, VerifyingKey};
use nibkeep_model::{FieldName, Name, TypeName, Value};
use nibkeep_node::Unlocker;
use nibkeep_node::{Edit, Joining, Library, MemberRole, NewProtection, NodeError, PairingCode};
use nibkeep_testkit::TestRng;

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn protected(passphrase: &str) -> NewProtection<'_> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// Ada's desktop, whose secrets are set up, made Project, with a login whose password is
/// secret; her laptop, paired with the secrets; and Bea's phone, an editor there.
struct Project {
	desktop: Library,
	laptop: Library,
	bea: Library,
	space: SpaceId,
	login: EntityId,
	at: u64,
	rng: TestRng,
}

impl Project {
	fn new(seed: &str) -> Self {
		let mut rng = TestRng::new(seed);
		let mut desktop = common::user_library("Ada", "desktop", &mut rng);
		desktop
			.set_up_secrets(&protected("desktop secrets"), now() + 500, &mut rng)
			.expect("sets up");
		let laptop_directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&common::unique_folder(&format!("{seed} laptop")),
		);
		std::fs::remove_dir(&laptop_directory).expect("removes");
		let code = PairingCode::generate(&mut rng);
		let joining = Joining::new("laptop", "nib", &mut rng)
			.expect("names")
			.asking_for_secrets(protected("laptop secrets"), &mut rng);
		let (paired, joined) = pair_joining(
			&mut desktop,
			&laptop_directory,
			(joining, true),
			(&code, &code),
			&mut rng,
		);
		paired.expect("paired");
		let mut laptop = joined.expect("joined").library;
		let mut at = now() + 100_000;
		let space = desktop
			.create_space("Project", at, &mut rng)
			.expect("makes it");
		desktop
			.define_type(space, &login_type(), at + 100, &mut rng)
			.expect("defines");
		let login = desktop
			.create_entity(
				space,
				&[LOGIN],
				vec![(field("local.title"), Value::Text("Signing".to_owned()))],
				at + 200,
				&mut rng,
			)
			.expect("creates");
		let mut bea = common::user_library("Bea", "phone", &mut rng);
		at += 2_000;
		invite_member(&mut desktop, (space, MemberRole::Editor), &mut bea, at);
		at = sync_rounds(&mut laptop, &mut desktop, at + 1_000, 3);
		at = sync_rounds(&mut bea, &mut desktop, at + 1_000, 1);
		let mut project = Self {
			desktop,
			laptop,
			bea,
			space,
			login,
			at,
			rng,
		};
		project.set_password("first");
		project
			.laptop
			.unlock_secrets(&Unlocker::Passphrase("laptop secrets"))
			.expect("unlocks");
		project.sync_laptop(2);
		project.sync_bea(1);
		project
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn sync_laptop(&mut self, rounds: u64) {
		let at = self.tick();
		self.at = sync_rounds(&mut self.laptop, &mut self.desktop, at, rounds);
	}

	fn sync_bea(&mut self, rounds: u64) {
		let at = self.tick();
		self.at = sync_rounds(&mut self.bea, &mut self.desktop, at, rounds);
	}

	fn set_password(&mut self, password: &str) {
		let at = self.tick();
		let (space, login) = (self.space, self.login);
		self.desktop
			.edit_entity(
				space,
				login,
				vec![Edit::Set(
					field("local.password"),
					vec![Value::Text(password.to_owned())],
				)],
				&today(),
				at,
				&mut self.rng,
			)
			.expect("sets the password");
	}

	/// Adds a note named `name` to Project on `library`.
	fn note(
		library: &mut Library,
		space: SpaceId,
		name: &str,
		at: u64,
		rng: &mut TestRng,
	) -> Result<EntityId, NodeError> {
		library.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(field("org.nibkeep.name"), Value::Name(Name::new(name)))],
			at,
			rng,
		)
	}

	/// The record in the personal space, and the folder, of the desktop or the laptop.
	fn whose(&self, laptop: bool) -> (EntityId, std::path::PathBuf) {
		let library = if laptop { &self.laptop } else { &self.desktop };
		let record = library
			.replicas()
			.expect("lists")
			.into_iter()
			.find(|replica| replica.this)
			.expect("itself")
			.id;
		let folder = library.library_directory().expect("a folder").to_owned();
		(record, folder)
	}

	/// Writes into the personal space of the desktop, or of the laptop, while its program is
	/// closed, the choices of Project of the desktop, or of the laptop, signed by its key,
	/// on its record there: as a device of the person's states them while it holds nothing
	/// of Project, and its program has not synced since.
	fn forge_choices(&mut self, into_laptop: bool, of_laptop: bool, choices: &[(u64, bool)]) {
		let unlocker = Unlocker::Passphrase("battery staple");
		let personal = self.laptop.default_space();
		let (record, chooser) = self.whose(of_laptop);
		let (_, folder) = self.whose(into_laptop);
		let (replica, _) = common::replica_keys(&chooser, &unlocker, personal);
		let operations = choices
			.iter()
			.map(|(number, holds)| {
				let choice =
					nibkeep_governance::Choice::sign(&replica.signing, self.space, *number, *holds);
				nibkeep_model::Operation::assert(nibkeep_model::Statement::new(
					self.rng.statement_id(),
					record,
					nibkeep_model::fields::SPACE_CHOICE,
					Value::Text(choice.to_text()),
				))
			})
			.collect();
		let closed = common::user_library("Nobody", "nothing", &mut self.rng);
		let library = if into_laptop {
			&mut self.laptop
		} else {
			&mut self.desktop
		};
		drop(std::mem::replace(library, closed));
		let at = self.tick();
		common::forge(
			&folder,
			&unlocker,
			(personal, operations),
			(None, at),
			&mut self.rng,
		);
		let library = if into_laptop {
			&mut self.laptop
		} else {
			&mut self.desktop
		};
		*library = Library::open(&folder, &unlocker).expect("opens");
	}

	/// The laptop's choice of Project: it states it, as a device does.
	fn laptop_chooses(&mut self, holds: bool) -> Result<(), NodeError> {
		let at = self.tick();
		self.laptop
			.state_space_choice(self.space, holds, at, &mut self.rng)
			.map(|_| ())
	}
}

/// Whether `library` knows the replica whose key is `key` as one that dropped `space`.
fn dropped(library: &Library, space: SpaceId, key: &VerifyingKey) -> bool {
	library
		.members(space)
		.expect("lists")
		.iter()
		.flat_map(|member| &member.replicas)
		.find(|replica| replica.signing == *key)
		.expect("the replica")
		.dropped
}

fn writing_key(library: &Library, space: SpaceId) -> u64 {
	library.space_key_numbers(space).expect("reads").1
}

fn holds_key(library: &Library, space: SpaceId, number: u64) -> bool {
	library
		.space_key_numbers(space)
		.expect("reads")
		.0
		.contains(&number)
}

/// How many changes a space's log holds.
fn changes(library: &Library, space: SpaceId) -> usize {
	library
		.status()
		.expect("reads")
		.spaces
		.into_iter()
		.find(|status| status.id == space)
		.expect("the space")
		.changes
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
fn a_device_that_drops_a_space_is_ended_there() {
	let mut project = Project::new("a drop");
	let laptop_key = own_key(&project.laptop);
	let before = writing_key(&project.desktop, project.space);
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	project.sync_bea(1);
	for library in [&project.desktop, &project.laptop, &project.bea] {
		assert!(dropped(library, project.space, &laptop_key));
	}
	// The others give the space a new key, which the laptop never gets.
	let after = writing_key(&project.desktop, project.space);
	assert_ne!(after, before);
	assert!(holds_key(&project.bea, project.space, after));
	project.sync_laptop(2);
	assert!(!holds_key(&project.laptop, project.space, after));
}

#[test]
fn the_others_make_a_new_key_and_secrets_key_and_the_dropping_device_makes_none() {
	let mut project = Project::new("new keys after a drop");
	let space = project.space;
	let (laptop_keys, _) = project.laptop.space_key_numbers(space).expect("reads");
	let (_, sealing) = project.desktop.secrets_key_numbers(space).expect("reads");
	let sealing = sealing.expect("seals with one");
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	project.set_password("second");
	project.sync_laptop(2);
	// The desktop seals with a new secrets key, which the laptop does not hold.
	let (_, newer) = project.desktop.secrets_key_numbers(space).expect("reads");
	let newer = newer.expect("seals with one");
	assert_ne!(newer, sealing);
	let (laptop_secrets, _) = project.laptop.secrets_key_numbers(space).expect("reads");
	assert!(!laptop_secrets.contains(&newer));
	// The laptop made no space key: those it holds are those it held, and none newer.
	let (held, _) = project.laptop.space_key_numbers(space).expect("reads");
	assert_eq!(held, laptop_keys);
}

#[test]
fn a_drop_stated_in_the_personal_space_is_carried_into_the_space() {
	let mut project = Project::new("a carried drop");
	let laptop_key = own_key(&project.laptop);
	let space = project.space;
	// The laptop's drop reaches the personal space alone, as one stated while the laptop
	// held nothing of Project would: the desktop carries it into Project.
	project.forge_choices(true, true, &[(1, false)]);
	let at = project.tick();
	let (first, second) =
		common::sync_in_memory(&mut project.laptop, &mut project.desktop, at, (0, 1));
	first.expect("the laptop syncs");
	second.expect("the desktop syncs");
	assert!(dropped(&project.desktop, space, &laptop_key));
	project.sync_bea(1);
	assert!(dropped(&project.bea, space, &laptop_key));
}

#[test]
fn a_drop_that_a_newer_choice_to_hold_replaced_is_not_carried() {
	let mut project = Project::new("a replaced drop");
	let laptop_key = own_key(&project.laptop);
	let space = project.space;
	project.forge_choices(true, true, &[(1, false), (2, true)]);
	project.sync_laptop(2);
	assert!(!dropped(&project.desktop, space, &laptop_key));
	assert!(!dropped(&project.laptop, space, &laptop_key));
}

#[test]
fn listing_again_follows_the_newest_choice() {
	let mut project = Project::new("the newest choice");
	let space = project.space;
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	// The desktop holds the laptop's choice to hold Project, and a newer one to drop it.
	project.forge_choices(false, true, &[(2, true), (3, false)]);
	let (laptop, _) = project.whose(true);
	let at = project.tick();
	assert!(matches!(
		project
			.desktop
			.list_again(space, laptop, at, &mut project.rng),
		Err(NodeError::NoNewerChoice)
	));
}

#[test]
fn a_device_that_dropped_a_space_carries_nothing_into_it() {
	let mut project = Project::new("carrying nothing");
	let space = project.space;
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	let before = changes(&project.laptop, space);
	// The laptop holds a drop of the desktop's that Project lacks, which it does not carry
	// there, as it changes nothing in Project any more.
	project.forge_choices(true, false, &[(1, false)]);
	// It settles its spaces after a sync with Bea, which sends it nothing of Project.
	let at = project.tick();
	let (first, second) = common::sync_in_memory(&mut project.laptop, &mut project.bea, at, (0, 1));
	first.expect("the laptop syncs");
	second.expect("Bea syncs");
	assert_eq!(changes(&project.laptop, space), before);
}

#[test]
fn a_dropped_device_is_sent_only_what_ended_it() {
	let mut project = Project::new("only what ended it");
	let space = project.space;
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	let at = project.tick();
	Project::note(
		&mut project.desktop,
		space,
		"Release plan",
		at,
		&mut project.rng,
	)
	.expect("adds");
	// Whether the laptop's program refuses it or not, nothing it writes there counts.
	let at = project.tick();
	let _ = Project::note(
		&mut project.laptop,
		space,
		"Written after the drop",
		at,
		&mut project.rng,
	);
	project.sync_laptop(2);
	assert_eq!(
		found(&project.laptop, space, "release"),
		Vec::<String>::new()
	);
	assert_eq!(
		found(&project.desktop, space, "written"),
		Vec::<String>::new()
	);
}

#[test]
fn listing_again_needs_a_newer_choice_to_hold() {
	let mut project = Project::new("listing again");
	let space = project.space;
	let laptop_key = own_key(&project.laptop);
	let laptop = project
		.desktop
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.signing == Some(laptop_key))
		.expect("the laptop")
		.id;
	project.laptop_chooses(false).expect("drops");
	project.sync_laptop(2);
	let at = project.tick();
	assert!(matches!(
		project
			.desktop
			.list_again(space, laptop, at, &mut project.rng),
		Err(NodeError::NoNewerChoice)
	));
	project.laptop_chooses(true).expect("asks for it again");
	project.sync_laptop(1);
	let at = project.tick();
	project
		.desktop
		.list_again(space, laptop, at, &mut project.rng)
		.expect("lists it again");
	let writing = writing_key(&project.desktop, space);
	project.sync_laptop(2);
	assert!(!dropped(&project.desktop, space, &laptop_key));
	assert!(!dropped(&project.laptop, space, &laptop_key));
	assert!(holds_key(&project.laptop, space, writing));
}

#[test]
fn a_choice_of_the_personal_space_is_refused() {
	let mut project = Project::new("the personal space");
	let personal = project.laptop.default_space();
	let at = project.tick();
	assert!(matches!(
		project
			.laptop
			.state_space_choice(personal, false, at, &mut project.rng),
		Err(NodeError::PersonalSpaceHeld)
	));
}
