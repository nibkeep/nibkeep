//! Two libraries on this computer, over connections on localhost: one pairs a new replica,
//! which gets everything the library holds, files too; a change made on either reaches the
//! other; a code takes three tries, and a wrong one adds no replica and makes no library; and
//! a new replica tries each device that announces the first two digits of its code; and a
//! replica whose record another device of the user's no longer types as a replica's states
//! its endpoint once, not at every sync.

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{TIMEOUT, library, now, pair_with, this_day};
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{
	Joining, Library, Network, NetworkOptions, NodeError, PairingCode, Showing, SyncError,
	Unlocker, Waiting, answer, join, pair, pair_closed, sync_with_others,
};
use nibkeep_testkit::TestRng;

/// A library with a type of books, a book, and a file.
fn with_books(name: &str, rng: &mut TestRng) -> (std::path::PathBuf, Library, FieldName) {
	let (directory, mut library) = library(name, rng);
	let personal = library.personal_space();
	let title: FieldName = "local.title".parse().expect("a field name");
	let book = TypeDef::new(TypeName::from_static("local.book"), 1)
		.with_field(title.clone(), FieldDef::new(ValueKind::Text));
	library
		.define_type(personal, &book, now(), rng)
		.expect("defines");
	for (count, name) in ["Lavinia", "The Left Hand of Darkness"].iter().enumerate() {
		library
			.create_entity(
				personal,
				&[book.name().clone()],
				vec![(title.clone(), Value::Text((*name).to_owned()))],
				now() + 1_000 * (count as u64 + 1),
				rng,
			)
			.expect("adds");
	}
	library
		.import_blob(personal, &vec![42_u8; 200_000][..], now() + 5_000, rng)
		.expect("imports a blob");
	(directory, library, title)
}

/// The titles of the books a library holds, sorted.
fn titles(library: &Library, title: &FieldName) -> Vec<String> {
	let personal = library.personal_space();
	let mut titles: Vec<String> = library
		.search(personal, "")
		.unwrap_or_default()
		.into_iter()
		.chain(
			["Lavinia", "Darkness", "Lathe"]
				.iter()
				.flat_map(|word| library.search(personal, word).unwrap_or_default()),
		)
		.filter_map(|found| {
			let view = library
				.entity(personal, found.id, this_day())
				.expect("reads")?;
			view.fields
				.iter()
				.find(|field| field.field == *title)
				.map(|field| {
					field.values[0]
						.value
						.as_text()
						.unwrap_or_default()
						.to_owned()
				})
		})
		.collect();
	titles.sort();
	titles.dedup();
	titles
}

#[test]
fn a_paired_replica_gets_the_library_and_their_changes_reach_each_other() {
	let mut rng = TestRng::new("paired");
	let (_, mut laptop, title) = with_books("paired laptop", &mut rng);
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "paired desk");
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	let paired = paired.expect("the laptop paired");
	assert_eq!(paired.device, "desk");
	assert_eq!(paired.app, "nib");
	let synced = paired.synced;
	assert!(synced.spaces[0].sent.changes > 3, "{synced:?}");
	assert_eq!(synced.spaces[0].sent.blobs, 1);
	let joined = joined.expect("the desk joined");
	let mut desk = joined.library;
	assert_eq!(desk.personal_space(), laptop.personal_space());
	assert_eq!(titles(&desk, &title), titles(&laptop, &title));
	assert_eq!(titles(&desk, &title).len(), 2);
	// The file arrived whole, and opens with its key.
	let report = desk.verify(now() + 70_000).expect("verifies");
	assert_eq!(report.spaces[0].blobs, 1, "{report:?}");
	assert!(report.spaces[0].problems.is_empty(), "{report:?}");
	// Both list both replicas, and know where the other connects from.
	for library in [&laptop, &desk] {
		let replicas = library.replicas().expect("lists");
		assert_eq!(replicas.len(), 2, "{replicas:?}");
		assert!(replicas[0].this);
		assert_eq!(library.peers().expect("lists").len(), 1);
	}
	assert_eq!(
		desk.replicas().expect("lists")[0].device_name.as_deref(),
		Some("desk")
	);

	// A book added on the desk reaches the laptop, which answers while it waits.
	let personal = desk.personal_space();
	let book = TypeName::from_static("local.book");
	desk.create_entity(
		personal,
		&[book],
		vec![(title.clone(), Value::Text("The Lathe of Heaven".to_owned()))],
		now() + 80_000,
		&mut rng,
	)
	.expect("adds");
	let laptop_network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let desk_network = desk.network(NetworkOptions::LOCAL).expect("starts");
	desk_network.remember(
		laptop_network.endpoint(),
		&laptop_network.local_addresses(),
		None,
	);
	let laptop_directory = laptop.close();
	let mut answering = TestRng::new("answering");
	let synced = std::thread::scope(|scope| {
		let answered = scope.spawn(|| {
			answer(
				&laptop_directory,
				&laptop_network,
				Some(TIMEOUT),
				&|| now() + 90_000,
				&mut answering,
			)
		});
		let synced = sync_with_others(
			&mut desk,
			&desk_network,
			TIMEOUT,
			&|| now() + 90_000,
			&mut rng,
		)
		.expect("syncs");
		let answered = answered
			.join()
			.expect("the laptop ran")
			.expect("answers")
			.expect("a sync came");
		assert_eq!(answered.endpoint, desk.endpoint());
		answered.synced.expect("the laptop synced");
		synced
	});
	assert_eq!(synced.len(), 1);
	let sent = synced[0].synced.as_ref().expect("synced").spaces[0].sent;
	assert_eq!(sent.changes, 1, "{synced:?}");
	let laptop = laptop_directory.open().expect("opens again");
	assert_eq!(titles(&laptop, &title).len(), 3);
	assert_eq!(titles(&laptop, &title), titles(&desk, &title));
	drop(laptop);

	// The desk's library opens again as any other.
	let directory = desk_directory.clone();
	drop(desk);
	let desk = Library::open(&directory, &Unlocker::Passphrase("battery staple")).expect("opens");
	assert_eq!(titles(&desk, &title).len(), 3);
}

/// A new replica on a device named `device`, and its network, which knows where each of
/// `sponsors` is and what it announces, as if it had found them on the network.
fn joiner(
	device: &str,
	sponsors: &[(&Network, &PairingCode)],
	rng: &mut TestRng,
) -> (Joining<'static>, Network) {
	let joining = Joining::new(device, "nib", rng).expect("names");
	let network = joining.network(NetworkOptions::LOCAL).expect("starts");
	for (sponsor, code) in sponsors {
		network.remember(
			sponsor.endpoint(),
			&sponsor.local_addresses(),
			Some(&code.announcement()),
		);
	}
	(joining, network)
}

/// Joins in `directory` as `joiner` with the code `typed`.
fn join_as(
	directory: &std::path::Path,
	(joining, network): (Joining<'_>, Network),
	typed: &PairingCode,
	rng: &mut TestRng,
) -> Result<nibkeep_node::Joined, NodeError> {
	join(
		directory,
		joining,
		&network,
		typed,
		&common::protection(),
		TIMEOUT,
		&|| now() + 60_000,
		rng,
	)
}

/// What a sponsor heard of the tries that failed before the code matched: whether each was a
/// wrong code, and how many tries were left.
type Heard = Mutex<Vec<(bool, usize)>>;

/// Records a try that failed in `heard`.
fn hear(heard: &Heard, error: &NodeError, left: usize) {
	heard
		.lock()
		.expect("not poisoned")
		.push((error.is_wrong_code(), left));
}

/// An empty folder for a new library, which does not exist yet.
fn new_folder(name: &str) -> std::path::PathBuf {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	std::fs::remove_dir(&directory).expect("removes");
	directory
}

#[test]
fn a_code_takes_three_tries_and_the_last_that_fails_spends_it() {
	let mut rng = TestRng::new("three tries");
	let (_, mut laptop, _) = with_books("three tries laptop", &mut rng);
	let shown: PairingCode = "37-482-177".parse().expect("a code");
	let typed: PairingCode = "37-482-178".parse().expect("a code");
	let sponsor_network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let desk_directory = new_folder("three tries desk");
	let heard = Heard::default();
	let wrong_try = |error: &NodeError, left| hear(&heard, error, left);
	let (paired, joined) = std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			pair(
				&mut laptop,
				&sponsor_network,
				&shown,
				Showing {
					timeout: TIMEOUT,
					wrong_try: &wrong_try,
				},
				&|| now() + 60_000,
				&mut TestRng::new("three tries sponsor"),
			)
		});
		let mut joined = Vec::new();
		let desk = joiner("desk", &[(&sponsor_network, &shown)], &mut rng);
		joined.push(join_as(&desk_directory, desk, &typed, &mut rng));
		// A try that breaks off before the code is checked counts too.
		let (_, network) = joiner("desk", &[(&sponsor_network, &shown)], &mut rng);
		let mut connection = network
			.connect(
				sponsor_network.endpoint(),
				nibkeep_net::Protocol::Pair,
				TIMEOUT,
			)
			.expect("connects");
		nibkeep_sync::Transport::send(&mut connection, b"not a message").expect("sends");
		connection.close();
		let desk = joiner("desk", &[(&sponsor_network, &shown)], &mut rng);
		joined.push(join_as(&desk_directory, desk, &typed, &mut rng));
		(paired.join().expect("the sponsor ran"), joined)
	});
	assert!(
		matches!(paired, Err(NodeError::Sync(SyncError::WrongCode))),
		"{paired:?}"
	);
	for joined in joined {
		assert!(
			matches!(joined, Err(NodeError::Sync(SyncError::WrongCode))),
			"{:?}",
			joined.err()
		);
	}
	assert_eq!(
		heard.into_inner().expect("not poisoned"),
		[(true, 2), (false, 1)]
	);
	assert!(!Library::exists(&desk_directory));
	assert_eq!(laptop.replicas().expect("lists").len(), 1);
}

#[test]
fn a_mistyped_code_is_typed_again_while_it_has_tries_left() {
	let mut rng = TestRng::new("typed again");
	let (_, library, title) = with_books("typed again laptop", &mut rng);
	let sponsor_network = library.network(NetworkOptions::LOCAL).expect("starts");
	let closed = library.close();
	let code: PairingCode = "52-104-993".parse().expect("a code");
	let mistyped: PairingCode = "52-104-939".parse().expect("a code");
	let phone_directory = new_folder("typed again phone");
	let heard = Heard::default();
	let wrong_try = |error: &NodeError, left| hear(&heard, error, left);
	let (paired, mistyped_joined, joined) = std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			pair_closed(
				&closed,
				&sponsor_network,
				&code,
				&Waiting {
					showing: Showing {
						timeout: TIMEOUT,
						wrong_try: &wrong_try,
					},
					stop: &AtomicBool::new(false),
					give_secrets: false,
					device: &nibkeep_node::NewDevice::computer(),
				},
				&|| now() + 60_000,
				&mut TestRng::new("typed again sponsor"),
			)
		});
		let phone = joiner("phone", &[(&sponsor_network, &code)], &mut rng);
		let mistyped_joined = join_as(&phone_directory, phone, &mistyped, &mut rng);
		let phone = joiner("phone", &[(&sponsor_network, &code)], &mut rng);
		let joined = join_as(&phone_directory, phone, &code, &mut rng);
		(
			paired.join().expect("the sponsor ran"),
			mistyped_joined,
			joined,
		)
	});
	assert!(
		matches!(mistyped_joined, Err(NodeError::Sync(SyncError::WrongCode))),
		"{:?}",
		mistyped_joined.err()
	);
	assert_eq!(paired.expect("pairs").device, "phone");
	let phone = joined.expect("joins").library;
	assert_eq!(titles(&phone, &title).len(), 2);
	assert_eq!(heard.into_inner().expect("not poisoned"), [(true, 2)]);
	assert_eq!(
		closed
			.open()
			.expect("opens")
			.replicas()
			.expect("lists")
			.len(),
		2
	);
}

#[test]
fn a_new_replica_tries_each_device_that_announces_the_digits_of_its_code() {
	let mut rng = TestRng::new("same digits");
	let (_, one) = library("same digits, one", &mut rng);
	let (_, other) = library("same digits, other", &mut rng);
	let one_network = one.network(NetworkOptions::LOCAL).expect("starts");
	let other_network = other.network(NetworkOptions::LOCAL).expect("starts");
	// The new replica tries the devices in the order of their endpoints: the one that shows
	// its code comes second.
	let mut sponsors = [(one, one_network), (other, other_network)];
	sponsors.sort_by_key(|(_, network)| network.endpoint());
	let [(mut first, first_network), (mut second, second_network)] = sponsors;
	let first_code: PairingCode = "37-111-111".parse().expect("a code");
	let second_code: PairingCode = "37-222-222".parse().expect("a code");
	assert_eq!(first_code.announcement(), second_code.announcement());
	let desk_directory = new_folder("same digits desk");
	let heard = Heard::default();
	let wrong_try = |error: &NodeError, left| hear(&heard, error, left);
	let (first_paired, second_paired, joined) = std::thread::scope(|scope| {
		let first_paired = scope.spawn(|| {
			pair(
				&mut first,
				&first_network,
				&first_code,
				Showing {
					timeout: Duration::from_secs(8),
					wrong_try: &wrong_try,
				},
				&|| now() + 60_000,
				&mut TestRng::new("same digits, first sponsor"),
			)
		});
		let second_paired = scope.spawn(|| {
			pair(
				&mut second,
				&second_network,
				&second_code,
				Showing::new(TIMEOUT),
				&|| now() + 60_000,
				&mut TestRng::new("same digits, second sponsor"),
			)
		});
		let desk = joiner(
			"desk",
			&[
				(&first_network, &first_code),
				(&second_network, &second_code),
			],
			&mut rng,
		);
		let joined = join_as(&desk_directory, desk, &second_code, &mut rng);
		(
			first_paired.join().expect("the first sponsor ran"),
			second_paired.join().expect("the second sponsor ran"),
			joined,
		)
	});
	assert_eq!(second_paired.expect("pairs").device, "desk");
	let desk = joined.expect("joins").library;
	assert_eq!(desk.personal_space(), second.personal_space());
	// The first sponsor heard one wrong try, and waited on with the tries left.
	assert!(
		matches!(first_paired, Err(NodeError::PairingExpired)),
		"{first_paired:?}"
	);
	assert_eq!(heard.into_inner().expect("not poisoned"), [(true, 2)]);
	assert_eq!(first.replicas().expect("lists").len(), 1);
}

#[test]
fn a_library_without_other_replicas_has_none_to_sync_with() {
	let mut rng = TestRng::new("alone");
	let (_, mut alone) = library("alone", &mut rng);
	let network: Network = alone.network(NetworkOptions::LOCAL).expect("starts");
	assert!(matches!(
		sync_with_others(
			&mut alone,
			&network,
			Duration::from_millis(100),
			&now,
			&mut rng
		),
		Err(NodeError::NoOtherReplicas)
	));
}

#[test]
fn a_closed_library_pairs_and_is_free_while_it_waits() {
	let mut rng = TestRng::new("paired closed");
	let (_, library, title) = with_books("paired closed laptop", &mut rng);
	let sponsor_network = library.network(NetworkOptions::LOCAL).expect("starts");
	let closed = library.close();
	let phone_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "paired closed phone");
	std::fs::remove_dir(&phone_directory).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let joining = Joining::new("phone", "Passwords", &mut rng).expect("names");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	joiner_network.remember(
		sponsor_network.endpoint(),
		&sponsor_network.local_addresses(),
		Some(&code.announcement()),
	);
	let mut sponsor_rng = TestRng::new("sponsor of the phone");
	// Another program has the library open, and adds a book, while the pairing waits, and
	// still when the phone connects: the pairing waits for it.
	let mut open = closed.open().expect("opens");
	let personal = open.personal_space();
	open.create_entity(
		personal,
		&[TypeName::from_static("local.book")],
		vec![(title.clone(), Value::Text("The Lathe of Heaven".to_owned()))],
		now() + 30_000,
		&mut rng,
	)
	.expect("adds while pairing waits");
	let (paired, joined) = std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			pair_closed(
				&closed,
				&sponsor_network,
				&code,
				&Waiting {
					showing: Showing {
						timeout: TIMEOUT,
						wrong_try: &|_, _| {},
					},
					stop: &AtomicBool::new(false),
					give_secrets: false,
					device: &nibkeep_node::NewDevice::computer(),
				},
				&|| now() + 60_000,
				&mut sponsor_rng,
			)
		});
		let joining_thread = scope.spawn(|| {
			let mut joiner_rng = TestRng::new("the phone");
			join(
				&phone_directory,
				joining,
				&joiner_network,
				&code,
				&common::protection(),
				TIMEOUT,
				&|| now() + 60_000,
				&mut joiner_rng,
			)
		});
		std::thread::sleep(Duration::from_millis(300));
		drop(open);
		(
			paired.join().expect("the sponsor ran"),
			joining_thread.join().expect("the phone ran"),
		)
	});
	let paired = paired.expect("the closed library paired");
	assert_eq!(
		(paired.device.as_str(), paired.app.as_str()),
		("phone", "Passwords")
	);
	let phone = joined.expect("the phone joined").library;
	assert_eq!(
		titles(&phone, &title).len(),
		3,
		"the book added while it waited came"
	);
	let laptop = closed.open().expect("free again");
	assert_eq!(laptop.replicas().expect("lists").len(), 2);
}

#[test]
fn pairing_with_a_closed_library_stops_waiting_when_told() {
	let mut rng = TestRng::new("pairing stopped");
	let (_, library) = library("pairing stopped", &mut rng);
	let network = library.network(NetworkOptions::LOCAL).expect("starts");
	let closed = library.close();
	let code = PairingCode::generate(&mut rng);
	let stop = AtomicBool::new(false);
	let started = std::time::Instant::now();
	let stopped = std::thread::scope(|scope| {
		let waiting = scope.spawn(|| {
			pair_closed(
				&closed,
				&network,
				&code,
				&Waiting {
					showing: Showing {
						timeout: TIMEOUT,
						wrong_try: &|_, _| {},
					},
					stop: &stop,
					give_secrets: false,
					device: &nibkeep_node::NewDevice::computer(),
				},
				&now,
				&mut TestRng::new("stopped sponsor"),
			)
		});
		std::thread::sleep(Duration::from_millis(300));
		stop.store(true, Ordering::Relaxed);
		waiting.join().expect("ran")
	});
	assert!(
		matches!(stopped, Err(NodeError::PairingExpired)),
		"{stopped:?}"
	);
	assert!(
		started.elapsed() < Duration::from_secs(5),
		"it stopped at once"
	);
}

#[test]
fn a_name_longer_than_pairing_takes_is_refused_before_the_code_is_spent() {
	let mut rng = TestRng::new("long name");
	let long = "d".repeat(201);
	assert!(matches!(
		Joining::new(&long, "nib", &mut rng),
		Err(NodeError::LongName(200))
	));
	assert!(Joining::new(&"d".repeat(200), "nib", &mut rng).is_ok());
}

/// Pairs a desk and a phone with the laptop, which has books, and has each add a book that
/// only it holds.
fn desk_and_phone(laptop: &mut Library, title: &FieldName, rng: &mut TestRng) -> [Library; 2] {
	let book = TypeName::from_static("local.book");
	["desk", "phone"].map(|device| {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("two syncs {device}"),
		);
		std::fs::remove_dir(&directory).expect("removes");
		let code = PairingCode::generate(rng);
		let (paired, joined) = common::pair_named(laptop, &directory, device, (&code, &code), rng);
		paired.expect("the laptop paired");
		let mut library = joined.expect("joined").library;
		let personal = library.personal_space();
		let name = if device == "desk" {
			"The Dispossessed"
		} else {
			"The Word for World Is Forest"
		};
		library
			.create_entity(
				personal,
				std::slice::from_ref(&book),
				vec![(title.clone(), Value::Text(name.to_owned()))],
				now() + 100_000,
				rng,
			)
			.expect("adds");
		library
	})
}

#[test]
fn a_replica_answers_one_replica_while_it_syncs_with_another() {
	let mut rng = TestRng::new("two syncs at once");
	let (_, mut laptop, title) = with_books("two syncs laptop", &mut rng);
	let [mut desk, phone] = desk_and_phone(&mut laptop, &title, &mut rng);
	// The laptop has one endpoint, as the program that holds its sync role runs it: on it,
	// it answers the desk while it syncs with the phone, each with a library of its own.
	let laptop_network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let desk_network = desk.network(NetworkOptions::LOCAL).expect("starts");
	let phone_network = phone.network(NetworkOptions::LOCAL).expect("starts");
	desk_network.remember(
		laptop_network.endpoint(),
		&laptop_network.local_addresses(),
		None,
	);
	laptop_network.remember(
		phone_network.endpoint(),
		&phone_network.local_addresses(),
		None,
	);
	let laptop = laptop.close();
	let phone = phone.close();
	let at = || now() + 110_000;
	std::thread::scope(|scope| {
		let answering = scope.spawn(|| {
			answer(
				&laptop,
				&laptop_network,
				Some(TIMEOUT),
				&at,
				&mut TestRng::new("the laptop answers"),
			)
		});
		let phone_answering = scope.spawn(|| {
			answer(
				&phone,
				&phone_network,
				Some(TIMEOUT),
				&at,
				&mut TestRng::new("the phone answers"),
			)
		});
		let connecting = scope.spawn(|| {
			let mut library = laptop.open().expect("opens a second time");
			let peers = library.peers().expect("lists");
			let found = [phone_network.endpoint()].into_iter().collect();
			nibkeep_node::sync_with_found(
				&mut library,
				&laptop_network,
				peers,
				&found,
				&at,
				&mut TestRng::new("the laptop connects"),
			)
		});
		let desk_synced =
			sync_with_others(&mut desk, &desk_network, TIMEOUT, &at, &mut rng).expect("syncs");
		assert!(desk_synced[0].synced.is_ok(), "{desk_synced:?}");
		let answered = answering.join().expect("ran").expect("answers");
		answered.expect("a sync came").synced.expect("answered");
		let synced = connecting.join().expect("ran");
		assert!(
			synced
				.iter()
				.any(|with| with.replica.device_name.as_deref() == Some("phone")
					&& with.synced.is_ok()),
			"{synced:?}"
		);
		let phone_answered = phone_answering.join().expect("ran").expect("answers");
		phone_answered
			.expect("a sync came")
			.synced
			.expect("answered");
	});
	// The laptop holds both books, and is sound.
	let mut laptop = laptop.open().expect("opens");
	let personal = laptop.personal_space();
	for word in ["Dispossessed", "Forest"] {
		let found = laptop.search(personal, word).expect("searches");
		assert_eq!(found.len(), 1, "{word}: {found:?}");
	}
	let report = laptop.verify(now() + 120_000).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
}

#[test]
fn a_replica_whose_first_sync_fails_after_it_joined_is_revoked_and_listed_nowhere() {
	let mut rng = TestRng::new("first sync fails");
	let (_, mut laptop) = library("first sync fails, laptop", &mut rng);
	let personal = laptop.personal_space();
	let (keys_before, _) = laptop.space_key_numbers(personal).expect("a space");
	let code = PairingCode::generate(&mut rng);
	let sponsor_network = laptop.network(NetworkOptions::LOCAL).expect("starts");
	let joining = Joining::new("desk", "nib", &mut rng).expect("names");
	let joiner_network = joining.network(NetworkOptions::LOCAL).expect("starts");
	let sponsor = sponsor_network.endpoint();
	joiner_network.remember(
		sponsor,
		&sponsor_network.local_addresses(),
		Some(&code.announcement()),
	);
	let paired = std::thread::scope(|scope| {
		let paired = scope.spawn(|| {
			nibkeep_node::pair(
				&mut laptop,
				&sponsor_network,
				&code,
				Showing::new(TIMEOUT),
				&|| now() + 60_000,
				&mut TestRng::new("first sync fails, sponsor"),
			)
		});
		// The new replica takes the grant, with the space's keys, and says that it made its
		// library, which the sponsor records; then its first sync breaks off, as when the
		// new device loses the network, or is turned off.
		let mut connection = joiner_network
			.connect(sponsor, nibkeep_net::Protocol::Pair, TIMEOUT)
			.expect("connects");
		let signing = nibkeep_crypto::SigningKey::generate(&mut rng);
		let decryption = nibkeep_crypto::DecryptionKey::generate(&mut rng);
		let request = nibkeep_governance::JoinRequest {
			signing: signing.verifying_key(),
			encryption: decryption.encryption_key(),
			..joining.request()
		};
		let grant =
			nibkeep_sync::join(&mut connection, &request, &code, &mut rng).expect("granted");
		let own = nibkeep_governance::Listing::sign(
			&signing,
			&decryption.encryption_key(),
			grant.space,
			(grant.user, grant.ai_agent),
		);
		nibkeep_sync::joined(&mut connection, grant, vec![own.to_text()]).expect("says it joined");
		connection.close();
		let mut connection = joiner_network
			.connect(sponsor, nibkeep_net::Protocol::Sync, TIMEOUT)
			.expect("connects");
		nibkeep_sync::Transport::send(&mut connection, b"not a message").expect("sends");
		connection.close();
		paired.join().expect("the sponsor ran")
	});
	assert!(
		matches!(paired, Err(NodeError::PairingFailed(_))),
		"{paired:?}"
	);
	// The replica that it recorded is revoked, so the space has a new key, which the replica
	// never gets; and its record is deleted, which the apps do not list.
	let (keys_after, writing) = laptop.space_key_numbers(personal).expect("a space");
	assert_eq!(keys_after.len(), keys_before.len() + 1, "{keys_after:?}");
	assert!(!keys_before.contains(&writing));
	let replicas = laptop.replicas().expect("lists");
	assert_eq!(replicas.len(), 2, "{replicas:?}");
	assert!(replicas[0].this);
	assert!(replicas[1].revoked && replicas[1].deleted, "{replicas:?}");
	let report = laptop.verify(now() + 120_000).expect("verifies");
	assert!(report.spaces[0].problems.is_empty(), "{report:?}");
	// The person pairs the device again, which works, with networks of the new pairing alone.
	drop((sponsor_network, joiner_network));
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "first sync fails, desk");
	std::fs::remove_dir(&directory).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut laptop, &directory, &code, &code, &mut rng);
	paired.expect("pairs");
	let desk = joined.expect("joins").library;
	assert_eq!(
		desk.space_key_numbers(personal).expect("a space").1,
		writing
	);
	let replicas = laptop.replicas().expect("lists");
	assert_eq!(
		replicas.iter().filter(|replica| !replica.deleted).count(),
		2,
		"{replicas:?}"
	);
}

#[test]
fn a_sponsor_lists_a_new_replica_only_by_its_own_listing_in_the_personal_space() {
	use nibkeep_governance::{JoinRequest, Listing, replica_record};
	use nibkeep_sync::Sponsoring;
	let mut rng = TestRng::new("own listing");
	let (_directory, mut library) = library("own listing", &mut rng);
	let joiner = nibkeep_crypto::SigningKey::generate(&mut rng);
	let decryption = nibkeep_crypto::DecryptionKey::generate(&mut rng);
	let request = JoinRequest {
		signing: joiner.verifying_key(),
		encryption: decryption.encryption_key(),
		device: "phone".to_owned(),
		app: "nib".to_owned(),
		secrets: None,
	};
	let other = nibkeep_crypto::SigningKey::generate(&mut rng);
	let other_encryption = nibkeep_crypto::DecryptionKey::generate(&mut rng).encryption_key();
	let stranger = rng.entity_id();
	let clock = now;
	let mut sponsor = nibkeep_node::Sponsor::new(&mut library, &clock);
	// The new replica's records are derived from the space and its key, and its grant says
	// whether it is an AI agent's, so that it signs its listing as one or not.
	let grant = sponsor.grant(&request, &[7; 32], &mut rng).expect("grants");
	assert_eq!(
		grant.replica,
		replica_record(grant.space, &joiner.verifying_key())
	);
	assert!(!grant.ai_agent);
	// A listing that another key signed, for another owner, marked as an AI agent's, or
	// with another encryption key, records nothing.
	let (space, user) = (grant.space, grant.user);
	for listing in [
		Listing::sign(&other, &decryption.encryption_key(), space, (user, false)),
		Listing::sign(
			&joiner,
			&decryption.encryption_key(),
			space,
			(stranger, false),
		),
		Listing::sign(&joiner, &decryption.encryption_key(), space, (user, true)),
		Listing::sign(&joiner, &other_encryption, space, (user, false)),
	] {
		sponsor.grant(&request, &[7; 32], &mut rng).expect("grants");
		assert!(sponsor.record(&[listing.to_text()], &mut rng).is_err());
	}
	assert!(
		!library
			.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.id == grant.replica)
	);
	// Its own listing records it.
	let mut sponsor = nibkeep_node::Sponsor::new(&mut library, &clock);
	sponsor.grant(&request, &[7; 32], &mut rng).expect("grants");
	let own = Listing::sign(&joiner, &decryption.encryption_key(), space, (user, false));
	sponsor.record(&[own.to_text()], &mut rng).expect("records");
	assert!(
		library
			.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.id == grant.replica)
	);
}

#[test]
fn a_joiner_refuses_a_grant_that_names_records_not_its_own() {
	use nibkeep_sync::Sponsoring;
	let mut rng = TestRng::new("records not its own");
	let (directory, mut library) = library("records not its own", &mut rng);
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let clock = now;
	let mut sponsor = nibkeep_node::Sponsor::new(&mut library, &clock);
	let grant = sponsor
		.grant(&joining.request(), &[7; 32], &mut rng)
		.expect("grants");
	let wrong = nibkeep_governance::Grant {
		replica: rng.entity_id(),
		..grant
	};
	let made = joining.make(
		&directory.join("phone"),
		&wrong,
		&common::protection(),
		&mut rng,
	);
	assert!(
		matches!(&made, Err(NodeError::Damaged(message)) if message.contains("not this replica's")),
		"{:?}",
		made.err()
	);
}

/// A device of the user's, infected, may take the replica type off another replica's
/// record. That replica still finds its endpoint there, and states it at no later sync, so
/// that its devices come to show the same.
#[test]
fn a_replica_whose_record_lost_its_type_states_its_endpoint_once() {
	let mut rng = TestRng::new("record without its type");
	let (_, mut laptop) = library("record without its type laptop", &mut rng);
	let directory = new_folder("record without its type desk");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut laptop, &directory, &code, &code, &mut rng);
	paired.expect("the laptop paired");
	let mut desk = joined.expect("joined").library;
	let personal = laptop.personal_space();
	let record = laptop
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.this)
		.expect("the laptop's record")
		.id;
	let mut at = now() + 100_000;
	let (first, second) = common::sync_in_memory(&mut laptop, &mut desk, at, (0, 1));
	first.expect("the laptop synced");
	second.expect("the desk synced");
	// The desk writes its own files while its program is closed.
	drop(desk);
	let unlocker = Unlocker::Passphrase("battery staple");
	let types = common::current_statements(
		&directory,
		&unlocker,
		personal,
		(record, &nibkeep_model::fields::TYPE),
	);
	assert!(!types.is_empty(), "the laptop's record has its type");
	at += 1_000;
	common::forge(
		&directory,
		&unlocker,
		(
			personal,
			vec![nibkeep_model::Operation::retract(record, types)],
		),
		(None, at),
		&mut rng,
	);
	let mut desk = Library::open(&directory, &unlocker).expect("opens");
	let mut changes = Vec::new();
	for _ in 0..3 {
		at += 1_000;
		let (first, second) = common::sync_in_memory(&mut laptop, &mut desk, at, (0, 1));
		first.expect("the laptop synced");
		second.expect("the desk synced");
		changes.push(laptop.status().expect("a status").spaces[0].changes);
	}
	let shown = laptop
		.entity(personal, record, this_day())
		.expect("reads")
		.expect("the laptop's record");
	assert!(
		shown.types.is_empty(),
		"the desk took the record's type away"
	);
	assert_eq!(
		changes[1], changes[2],
		"the laptop writes nothing more once both hold the same"
	);
}
