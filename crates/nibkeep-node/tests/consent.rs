//! Consent and taking ([technical design: devices that choose their spaces]): a space reaches
//! a device of the person's only as they chose: as they pair it, as a computer, which takes
//! every space and those given to it later, or as a phone, which takes the personal space
//! alone and asks before it takes a space given to it later; as they make or join a space;
//! and as they approve, on a device that holds a space, a device that asks for it. Each
//! device says itself whether it takes new spaces, so no other device can have a phone take
//! what it was given; a device that declined a space takes none it is given, and is never
//! listed there. Every device shows each space of the person's by name, those it does not
//! hold too.
//!
//! The people: Ada, with a desktop that made Project, a laptop paired as a computer and a
//! phone paired as a phone; and, for groups, Mona, an admin of the household's group and of
//! the family's space that the group is in, and Sam, in the household, with a laptop and a
//! phone.
//!
//! [technical design: devices that choose their spaces]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces

mod common;

use common::{
	invite_member, now, own_key, own_record, pair_device_as, pair_joining_as, sync_rounds,
	user_library,
};
use nibkeep_base::{SpaceId, StatementId};
use nibkeep_model::{Name, Operation, Statement, TypeName, Value};
use nibkeep_node::{
	Here, Joining, Library, MemberRole, NewDevice, NodeError, PairingCode, Taking, Unlocker,
};
use nibkeep_testkit::TestRng;

/// Adds an entity named `name` to `space`.
fn note(library: &mut Library, space: SpaceId, name: &str, at: u64, rng: &mut TestRng) {
	library
		.create_entity(
			space,
			&[TypeName::from_static("std.person")],
			vec![(
				"org.nibkeep.name".parse().expect("a field"),
				Value::Name(Name::new(name)),
			)],
			at,
			rng,
		)
		.expect("adds");
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

/// Whether a library holds a space.
fn holds(library: &Library, space: SpaceId) -> bool {
	library.spaces().contains(&space)
}

/// What a library knows of a space of the person's, held or not.
fn known(library: &Library, space: SpaceId) -> nibkeep_node::KnownSpace {
	library
		.known_spaces()
		.expect("lists")
		.into_iter()
		.find(|known| known.space == space)
		.expect("known")
}

/// The names of the devices that a library knows to hold a space.
fn holders(library: &Library, space: SpaceId) -> Vec<String> {
	let mut names: Vec<String> = known(library, space)
		.holders
		.into_iter()
		.filter_map(|holder| holder.device)
		.collect();
	names.sort();
	names
}

/// Ada's desktop, which made Project, with a note, before her laptop paired as a computer
/// and her phone as a phone, each synced with the desktop.
struct Ada {
	desktop: Library,
	laptop: Library,
	phone: Library,
	project: SpaceId,
	at: u64,
	rng: TestRng,
}

impl Ada {
	fn new(seed: &str) -> Self {
		let mut rng = TestRng::new(seed);
		let mut desktop = user_library("Ada", "desktop", &mut rng);
		let at = now() + 1_000;
		let project = desktop
			.create_space("Project", at, &mut rng)
			.expect("makes it");
		note(&mut desktop, project, "Plan", at + 100, &mut rng);
		let laptop = pair_device_as(
			&mut desktop,
			(seed, "laptop"),
			&NewDevice::computer(),
			&mut rng,
		);
		let phone = pair_device_as(&mut desktop, (seed, "phone"), &NewDevice::phone(), &mut rng);
		let mut ada = Self {
			desktop,
			laptop,
			phone,
			project,
			at: now() + 100_000,
			rng,
		};
		ada.sync_all(3);
		ada
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// The laptop and the phone each sync with the desktop, in turn, `rounds` times.
	fn sync_all(&mut self, rounds: u64) {
		for _ in 0..rounds {
			self.at = sync_rounds(&mut self.laptop, &mut self.desktop, self.at, 1);
			self.at = sync_rounds(&mut self.phone, &mut self.desktop, self.at, 1);
		}
	}
}

#[test]
fn a_phone_holds_the_personal_space_alone_and_a_computer_every_space() {
	let mut ada = Ada::new("a phone and a computer");
	let project = ada.project;
	// The laptop took Project as it paired, and fills as it syncs; the phone did not.
	assert!(holds(&ada.laptop, project));
	assert_eq!(found(&ada.laptop, project, "plan"), ["Plan"]);
	assert_eq!(
		ada.laptop.own_role(project).expect("reads"),
		MemberRole::Admin
	);
	assert_eq!(ada.phone.spaces(), [ada.phone.personal_space()]);
	// Each says so itself.
	assert!(ada.laptop.takes_new_spaces().expect("reads"));
	assert!(!ada.phone.takes_new_spaces().expect("reads"));
	// The phone knows Project by name, and which devices hold it.
	let shown = known(&ada.phone, project);
	assert_eq!(shown.name.as_deref(), Some("Project"));
	assert_eq!(shown.here, Here::Elsewhere);
	assert_eq!(holders(&ada.phone, project), ["desktop", "laptop"]);

	// A phone paired with Project chosen holds it; a computer paired with none chosen holds
	// the personal space alone, and takes new spaces.
	let tablet = pair_device_as(
		&mut ada.desktop,
		("a phone and a computer", "tablet"),
		&NewDevice::phone().with_spaces(vec![project]),
		&mut ada.rng,
	);
	assert!(holds(&tablet, project));
	assert!(!tablet.takes_new_spaces().expect("reads"));
	let server = pair_device_as(
		&mut ada.desktop,
		("a phone and a computer", "server"),
		&NewDevice::computer().with_spaces(Vec::new()),
		&mut ada.rng,
	);
	assert_eq!(server.spaces(), [server.personal_space()]);
	assert!(server.takes_new_spaces().expect("reads"));
}

#[test]
fn a_space_made_on_one_device_reaches_the_devices_that_take_new_spaces_and_no_other() {
	let mut ada = Ada::new("a space made later");
	let at = ada.tick();
	let notes = ada
		.desktop
		.create_space("Notes", at, &mut ada.rng)
		.expect("makes it");
	note(&mut ada.desktop, notes, "Groceries", at + 100, &mut ada.rng);
	ada.sync_all(3);
	assert!(holds(&ada.laptop, notes));
	assert_eq!(found(&ada.laptop, notes, "groceries"), ["Groceries"]);
	assert!(!holds(&ada.phone, notes));
	let shown = known(&ada.phone, notes);
	assert_eq!(shown.name.as_deref(), Some("Notes"));
	assert_eq!(shown.here, Here::Elsewhere);
	assert_eq!(holders(&ada.phone, notes), ["desktop", "laptop"]);

	// Made for the phone too, it waits there until the person takes it on the phone.
	let phone = own_record(&ada.phone);
	let at = ada.tick();
	let travel = ada
		.desktop
		.create_space_on("Travel", &[phone], at, &mut ada.rng)
		.expect("makes it");
	note(&mut ada.desktop, travel, "Tickets", at + 100, &mut ada.rng);
	ada.sync_all(2);
	assert!(holds(&ada.laptop, travel));
	assert!(!holds(&ada.phone, travel));
	assert_eq!(known(&ada.phone, travel).here, Here::Given);
	let at = ada.tick();
	assert_eq!(
		ada.phone
			.take_space(travel, at, &mut ada.rng)
			.expect("takes it"),
		Taking::Took
	);
	ada.sync_all(3);
	assert_eq!(found(&ada.phone, travel, "tickets"), ["Tickets"]);
	assert_eq!(
		holders(&ada.desktop, travel),
		["desktop", "laptop", "phone"]
	);
	// Taking it again finds it held.
	let at = ada.tick();
	assert!(matches!(
		ada.phone.take_space(travel, at, &mut ada.rng),
		Err(NodeError::HoldsSpace(_))
	));
}

#[test]
fn a_device_that_declines_a_space_takes_none_and_is_never_listed() {
	let mut ada = Ada::new("a declined space");
	// The laptop asks first for a while; the desktop makes Notes for it too.
	let at = ada.tick();
	ada.laptop
		.set_takes_new_spaces(false, at, &mut ada.rng)
		.expect("says so");
	ada.sync_all(1);
	let laptop = own_record(&ada.laptop);
	let at = ada.tick();
	let notes = ada
		.desktop
		.create_space_on("Notes", &[laptop], at, &mut ada.rng)
		.expect("makes it");
	ada.sync_all(1);
	assert_eq!(known(&ada.laptop, notes).here, Here::Given);
	// The person declines it there, and the laptop takes new spaces again.
	let at = ada.tick();
	ada.laptop
		.state_space_choice(notes, false, at, &mut ada.rng)
		.expect("declines");
	ada.laptop
		.set_takes_new_spaces(true, at + 100, &mut ada.rng)
		.expect("says so");
	ada.sync_all(3);
	assert!(!holds(&ada.laptop, notes));
	assert_eq!(known(&ada.laptop, notes).here, Here::Declined);
	assert_eq!(
		common::listed_there(&ada.desktop, notes, &own_key(&ada.laptop)),
		None
	);
	// Nothing to approve, and giving it changes nothing.
	assert_eq!(ada.desktop.space_requests().expect("lists"), Vec::new());
	let at = ada.tick();
	assert!(matches!(
		ada.desktop.allow_space(notes, laptop, at, &mut ada.rng),
		Err(NodeError::NoRequest)
	));
	ada.desktop
		.give_space(notes, &[laptop], at, &mut ada.rng)
		.expect("gives nothing");
	ada.sync_all(2);
	assert!(!holds(&ada.laptop, notes));

	// Once the person takes it there, the laptop holds it, as it was given it before.
	let at = ada.tick();
	assert_eq!(
		ada.laptop
			.take_space(notes, at, &mut ada.rng)
			.expect("takes it"),
		Taking::Took
	);
	ada.sync_all(3);
	assert_eq!(
		common::listed_there(&ada.desktop, notes, &own_key(&ada.laptop)),
		Some(false)
	);
}

#[test]
fn a_request_is_shown_only_on_a_device_that_holds_the_space_and_approving_it_gives_it() {
	let mut ada = Ada::new("a request");
	let project = ada.project;
	let at = ada.tick();
	assert_eq!(
		ada.phone
			.take_space(project, at, &mut ada.rng)
			.expect("asks"),
		Taking::Asked
	);
	assert_eq!(known(&ada.phone, project).here, Here::Asked);
	ada.sync_all(2);
	assert_eq!(ada.phone.space_requests().expect("lists"), Vec::new());
	let requests = ada.desktop.space_requests().expect("lists");
	assert_eq!(requests.len(), 1, "{requests:?}");
	let phone = own_record(&ada.phone);
	assert_eq!(
		(
			requests[0].space,
			requests[0].replica,
			requests[0].device.as_deref(),
			requests[0].back
		),
		(project, phone, Some("phone"), false)
	);
	// So on the laptop, which holds it too.
	assert_eq!(ada.laptop.space_requests().expect("lists").len(), 1);

	let at = ada.tick();
	ada.desktop
		.allow_space(project, phone, at, &mut ada.rng)
		.expect("allows");
	ada.sync_all(3);
	assert_eq!(found(&ada.phone, project, "plan"), ["Plan"]);
	assert_eq!(ada.desktop.space_requests().expect("lists"), Vec::new());
	let at = ada.tick();
	assert!(matches!(
		ada.desktop.allow_space(project, phone, at, &mut ada.rng),
		Err(NodeError::NoRequest)
	));
}

#[test]
fn a_space_just_taken_shows_by_the_name_kept_for_it_until_its_own_comes() {
	let mut ada = Ada::new("a name kept");
	let project = ada.project;
	let at = ada.tick();
	ada.phone
		.take_space(project, at, &mut ada.rng)
		.expect("asks");
	ada.sync_all(1);
	let at = ada.tick();
	ada.desktop
		.allow_space(project, own_record(&ada.phone), at, &mut ada.rng)
		.expect("allows");
	// One sync: the phone takes the space, empty, and asks to be listed there.
	ada.at = sync_rounds(&mut ada.phone, &mut ada.desktop, ada.at, 1);
	assert!(holds(&ada.phone, project));
	assert!(found(&ada.phone, project, "plan").is_empty(), "empty yet");
	assert_eq!(
		ada.phone.space_name(project).expect("reads").as_deref(),
		Some("Project")
	);
	let shown = known(&ada.phone, project);
	assert_eq!(
		(shown.here, shown.name.as_deref()),
		(Here::Held, Some("Project"))
	);
	ada.sync_all(2);
	assert_eq!(found(&ada.phone, project, "plan"), ["Plan"]);
}

#[test]
fn a_device_that_dropped_a_space_asks_for_it_again_and_is_listed_again_once_approved() {
	let mut ada = Ada::new("asking again");
	let project = ada.project;
	let at = ada.tick();
	ada.laptop
		.state_space_choice(project, false, at, &mut ada.rng)
		.expect("drops");
	ada.sync_all(2);
	let dropped = |library: &Library, key| {
		library
			.members(project)
			.expect("lists")
			.into_iter()
			.flat_map(|member| member.replicas)
			.find(|replica| replica.signing == key)
			.map(|replica| replica.dropped)
	};
	let key = own_key(&ada.laptop);
	assert_eq!(dropped(&ada.desktop, key), Some(true));
	assert_eq!(known(&ada.laptop, project).here, Here::Declined);

	let at = ada.tick();
	assert_eq!(
		ada.laptop
			.take_space(project, at, &mut ada.rng)
			.expect("asks"),
		Taking::Asked
	);
	ada.sync_all(1);
	let requests = ada.desktop.space_requests().expect("lists");
	assert_eq!(requests.len(), 1, "{requests:?}");
	assert!(requests[0].back);
	let at = ada.tick();
	ada.desktop
		.allow_space(project, own_record(&ada.laptop), at, &mut ada.rng)
		.expect("allows");
	ada.sync_all(2);
	assert_eq!(dropped(&ada.desktop, key), Some(false));
	let at = ada.tick();
	note(&mut ada.desktop, project, "Budget", at, &mut ada.rng);
	ada.sync_all(2);
	assert_eq!(found(&ada.laptop, project, "budget"), ["Budget"]);
}

#[test]
fn every_device_shows_each_space_by_name_as_it_is_renamed() {
	let mut ada = Ada::new("names of spaces");
	let project = ada.project;
	assert_eq!(
		ada.phone.find_known_space("project").expect("finds"),
		project
	);
	let at = ada.tick();
	ada.desktop
		.rename_space(project, "Novel", at, &mut ada.rng)
		.expect("renames");
	ada.sync_all(2);
	assert_eq!(known(&ada.phone, project).name.as_deref(), Some("Novel"));
	// The desktop and the laptop, which both hold it, wrote the names once each, and write
	// them no more.
	let kept = ada.desktop.changes_kept().expect("counts");
	ada.sync_all(2);
	assert_eq!(ada.desktop.changes_kept().expect("counts"), kept);
	assert_eq!(ada.phone.find_known_space("NOVEL").expect("finds"), project);
	assert!(matches!(
		ada.phone.find_known_space("Project"),
		Err(NodeError::NoSpaceNamed(_))
	));
	// The record of its names is no entity of the personal space's for a list or a search.
	let personal = ada.phone.personal_space();
	assert_eq!(found(&ada.phone, personal, "novel"), Vec::<String>::new());
}

#[test]
fn a_device_that_turned_against_its_person_cannot_have_a_phone_take_a_space() {
	let ada = Ada::new("an infected desktop and a phone");
	let personal = ada.desktop.personal_space();
	let phone = own_record(&ada.phone);
	// The desktop says, on the phone's record, that the phone takes new spaces.
	let directory = ada
		.desktop
		.library_directory()
		.expect("a folder")
		.to_owned();
	let Ada {
		desktop,
		laptop,
		phone: phone_library,
		project,
		at,
		mut rng,
	} = ada;
	drop(desktop.close());
	common::forge(
		&directory,
		&Unlocker::Passphrase("battery staple"),
		(
			personal,
			vec![Operation::assert(Statement::new(
				StatementId::random(&mut rng),
				phone,
				"org.nibkeep.new-spaces".parse().expect("a field"),
				Value::Boolean(true),
			))],
		),
		(None, at + 1_000),
		&mut rng,
	);
	let desktop =
		Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	let mut ada = Ada {
		desktop,
		laptop,
		phone: phone_library,
		project,
		at: at + 2_000,
		rng,
	};
	ada.sync_all(2);
	assert!(!ada.phone.takes_new_spaces().expect("reads"));
	// It makes a space, which reaches the phone only as an entry, and gives it Project too:
	// the phone takes neither.
	let at = ada.tick();
	let notes = ada
		.desktop
		.create_space("Notes", at, &mut ada.rng)
		.expect("makes it");
	ada.desktop
		.give_space(notes, &[phone], at + 100, &mut ada.rng)
		.expect("gives");
	ada.desktop
		.give_space(ada.project, &[phone], at + 200, &mut ada.rng)
		.expect("gives");
	ada.sync_all(3);
	assert!(!holds(&ada.phone, notes));
	assert!(!holds(&ada.phone, ada.project));
	assert_eq!(known(&ada.phone, notes).here, Here::Given);
}

#[test]
fn a_device_that_turned_against_its_person_cannot_list_one_that_never_asked() {
	let mut ada = Ada::new("an infected desktop lists the phone");
	let project = ada.project;
	let phone_key = own_key(&ada.phone);
	// It lists the phone's key in Project, with a listing that the phone never signed.
	let member = ada
		.desktop
		.members(project)
		.expect("lists")
		.into_iter()
		.find(|member| member.this_user)
		.expect("Ada")
		.record;
	let forger = nibkeep_crypto::SigningKey::generate(&mut ada.rng);
	let encryption = nibkeep_crypto::DecryptionKey::generate(&mut ada.rng).encryption_key();
	let mut listing =
		nibkeep_governance::Listing::sign(&forger, &encryption, project, (member, false));
	listing.key = phone_key;
	listing.record = nibkeep_governance::replica_record(project, &phone_key);
	let text = listing.to_text();
	let listed = nibkeep_governance::list_replica(
		&nibkeep_governance::SpaceReplica {
			record: listing.record,
			device_record: nibkeep_governance::device_record(project, &phone_key),
			member,
			signing: &phone_key,
			encryption: &encryption,
			listing: &text,
			endpoint: None,
			device: "phone",
			app: "nib",
			ai_agent: false,
		},
		&mut ada.rng,
	);
	let directory = ada
		.desktop
		.library_directory()
		.expect("a folder")
		.to_owned();
	let Ada {
		desktop,
		laptop,
		phone,
		project,
		at,
		mut rng,
	} = ada;
	drop(desktop.close());
	common::forge(
		&directory,
		&Unlocker::Passphrase("battery staple"),
		(project, listed.governing),
		(None, at + 1_000),
		&mut rng,
	);
	let desktop =
		Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	let mut ada = Ada {
		desktop,
		laptop,
		phone,
		project,
		at: at + 2_000,
		rng,
	};
	ada.sync_all(2);
	for library in [&ada.desktop, &ada.laptop] {
		assert_eq!(common::listed_there(library, project, &phone_key), None);
	}
	assert_eq!(ada.desktop.space_requests().expect("lists"), Vec::new());
}

/// Mona's household group and family space, the group in the space; Sam, in the household
/// and in the family, with his laptop, and his phone, paired as a phone with the household,
/// which is one of the family's devices through the household alone.
struct Families {
	mona: Library,
	sam: Library,
	phone: Library,
	family: SpaceId,
	at: u64,
	rng: TestRng,
}

impl Families {
	fn new(seed: &str) -> Self {
		Self::with(seed, true)
	}

	/// The same, with Sam in the family through the household alone, unless `direct`.
	fn with(seed: &str, direct: bool) -> Self {
		let mut rng = TestRng::new(seed);
		let mut mona = user_library("Mona", "laptop", &mut rng);
		let mut sam = user_library("Sam", "laptop", &mut rng);
		let mut at = now() + 100_000;
		let household = mona
			.create_group("Household", at, &mut rng)
			.expect("makes it");
		let family = mona
			.create_space("Family", at + 100, &mut rng)
			.expect("makes it");
		note(&mut mona, family, "Wedding album", at + 300, &mut rng);
		at += 2_000;
		invite_member(&mut mona, (household, MemberRole::Editor), &mut sam, at);
		at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
		// Sam is in the family directly too, so that his laptop may ask for its secrets and
		// approve his devices' requests.
		if direct {
			at += 2_000;
			invite_member(&mut mona, (family, MemberRole::Editor), &mut sam, at);
			at = sync_rounds(&mut mona, &mut sam, at + 1_000, 1);
		}
		mona.add_group(family, household, MemberRole::Editor, at, &mut rng)
			.expect("adds the group");
		at = sync_rounds(&mut mona, &mut sam, at + 1_000, 3);
		// Sam's phone, paired as a phone, with the household and the secrets.
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&common::unique_folder(&format!("{seed} phone")),
		);
		let code = PairingCode::generate(&mut rng);
		let joining = Joining::new("phone", "nib", &mut rng).expect("names");
		let (paired, joined) = pair_joining_as(
			&mut sam,
			&directory,
			(
				joining,
				false,
				&NewDevice::phone().with_spaces(vec![household]),
			),
			(&code, &code),
			&mut rng,
		);
		paired.expect("paired");
		let phone = joined.expect("joined").library;
		let mut families = Self {
			mona,
			sam,
			phone,
			family,
			at: at + 1_000,
			rng,
		};
		families.sync_all(3);
		families
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Mona and Sam's laptop sync, then Sam's laptop and phone, `rounds` times.
	fn sync_all(&mut self, rounds: u64) {
		for _ in 0..rounds {
			self.at = sync_rounds(&mut self.mona, &mut self.sam, self.at, 1);
			self.at = sync_rounds(&mut self.phone, &mut self.sam, self.at, 1);
		}
	}
}

#[test]
fn a_groups_space_reaches_a_phone_only_when_the_person_takes_it() {
	let mut families = Families::new("a group's space on a phone");
	let family = families.family;
	// The phone holds the household, which gives it the family, and does not take it.
	assert!(!holds(&families.phone, family));
	assert_eq!(known(&families.phone, family).here, Here::Given);
	assert!(holds(&families.sam, family));
	// Once the person takes it on the phone, it fills.
	let at = families.tick();
	assert_eq!(
		families
			.phone
			.take_space(family, at, &mut families.rng)
			.expect("takes it"),
		Taking::Took
	);
	families.sync_all(3);
	assert_eq!(found(&families.phone, family, "album"), ["Wedding album"]);
	assert_eq!(holders(&families.sam, family), ["laptop", "phone"]);
}

#[test]
fn a_groups_space_that_a_phone_declined_hears_of_it_and_takes_it_back_once_approved() {
	let mut families = Families::new("a group's space declined");
	let family = families.family;
	let at = families.tick();
	families
		.phone
		.state_space_choice(family, false, at, &mut families.rng)
		.expect("declines");
	families.sync_all(3);
	assert!(!holds(&families.phone, family));
	assert_eq!(known(&families.phone, family).here, Here::Declined);
	// Taking new spaces again, it still does not take it: the person declined it.
	let at = families.tick();
	families
		.phone
		.set_takes_new_spaces(true, at, &mut families.rng)
		.expect("says so");
	families.sync_all(2);
	assert!(!holds(&families.phone, family));
	// The decline went into the family through Sam's laptop, which holds it: taken on the
	// phone, the family sends it what its decline builds on, as to a device that dropped
	// it, and nothing written after, until the person approves on the laptop.
	let at = families.tick();
	note(
		&mut families.mona,
		family,
		"School photos",
		at,
		&mut families.rng,
	);
	let at = families.tick();
	assert_eq!(
		families
			.phone
			.take_space(family, at, &mut families.rng)
			.expect("takes it"),
		Taking::Took
	);
	families.sync_all(3);
	assert_eq!(found(&families.phone, family, "album"), ["Wedding album"]);
	assert_eq!(
		found(&families.phone, family, "photos"),
		Vec::<String>::new()
	);
	let requests = families.sam.space_requests().expect("lists");
	assert_eq!(requests.len(), 1, "{requests:?}");
	assert!(requests[0].back);
	let at = families.tick();
	families
		.sam
		.allow_space(family, own_record(&families.phone), at, &mut families.rng)
		.expect("allows");
	families.sync_all(3);
	assert_eq!(found(&families.phone, family, "photos"), ["School photos"]);
}

#[test]
fn a_groups_space_that_a_phone_declined_hears_of_it_from_a_member_through_the_group_alone() {
	let mut families = Families::with("a group's space declined, through the group", false);
	let family = families.family;
	assert!(holds(&families.sam, family));
	let at = families.tick();
	families
		.phone
		.state_space_choice(family, false, at, &mut families.rng)
		.expect("declines");
	families.sync_all(3);
	// Sam's laptop, in the family through the household alone, carried the decline there:
	// taken on the phone, the family sends it nothing written after.
	let at = families.tick();
	note(
		&mut families.mona,
		family,
		"School photos",
		at,
		&mut families.rng,
	);
	let at = families.tick();
	assert_eq!(
		families
			.phone
			.take_space(family, at, &mut families.rng)
			.expect("takes it"),
		Taking::Took
	);
	families.sync_all(3);
	assert_eq!(found(&families.sam, family, "photos"), ["School photos"]);
	assert_eq!(
		found(&families.phone, family, "photos"),
		Vec::<String>::new()
	);
}
