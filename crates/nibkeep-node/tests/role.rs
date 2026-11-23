//! The sync role ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#105-the-sync-role)): one program
//! on a replica holds it at a time; the holder answers and connects, syncs what comes, and
//! records what it does, which other programs read and ask through; and pairing borrows its
//! network.

mod common;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{library, now, pair_with};
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{Name, Value};
use nibkeep_node::{
	Background, BackgroundOptions, Closed, Holding, Library, NetworkOptions, PairingCode, Records,
	Role,
};
use nibkeep_testkit::TestRng;

/// How long a test waits for what holders do.
const PATIENCE: Duration = Duration::from_secs(30);

/// Waits until `done` says so, or fails after [`PATIENCE`].
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
	let started = Instant::now();
	while !done() {
		assert!(started.elapsed() < PATIENCE, "{what}: not in time");
		std::thread::sleep(Duration::from_millis(50));
	}
}

/// A library on a laptop, and one on a desk paired with it, both closed.
fn laptop_and_desk(name: &str) -> (Closed, Closed) {
	let mut rng = TestRng::new(name);
	let (_, mut laptop) = library(&format!("{name} laptop"), &mut rng);
	let desk =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name} desk"));
	std::fs::remove_dir(&desk).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut laptop, &desk, &code, &code, &mut rng);
	paired.expect("paired");
	(laptop.close(), joined.expect("joined").library.close())
}

/// How many entities a search for `word` finds in a closed library, opened for it.
fn found(closed: &Closed, word: &str) -> usize {
	let library = closed.open().expect("opens");
	library
		.search(library.personal_space(), word)
		.expect("searches")
		.len()
}

fn add_person(closed: &Closed, name: &str, rng: &mut TestRng) {
	let mut library = closed.open().expect("opens");
	let personal = library.personal_space();
	library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new(name)))],
			now() + 120_000,
			rng,
		)
		.expect("adds");
}

fn records(closed: &Closed) -> Records {
	closed
		.open()
		.expect("opens")
		.sync_records(TestRng::new("a reader"))
		.expect("records")
}

fn endpoint(closed: &Closed) -> [u8; 32] {
	closed.open().expect("opens").endpoint()
}

/// A UDP port that is free now, on this computer.
fn free_port() -> u16 {
	UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
		.and_then(|socket| socket.local_addr())
		.expect("binds")
		.port()
}

/// The clock of the holders.
fn clock() -> u64 {
	now() + 120_000
}

/// Sets a flag as it goes, so that a test that fails stops the holders it started.
struct Stops<'a>(&'a AtomicBool);

impl Drop for Stops<'_> {
	fn drop(&mut self) {
		self.0.store(true, Ordering::SeqCst);
	}
}

/// Holds the role of `closed` until `stop` is set, on `port`, with the other replica at
/// `other`, if it says where.
fn run_holder(closed: &Closed, port: u16, other: Option<([u8; 32], u16)>, stop: &AtomicBool) {
	run_holder_checking(closed, (port, other), nibkeep_node::Pace::DEFAULT, stop);
}

/// Holds the role of `closed` as [`run_holder`] does, checking its copies at the pace
/// `check`.
fn run_holder_checking(
	closed: &Closed,
	(port, other): (u16, Option<([u8; 32], u16)>),
	check: nibkeep_node::Pace,
	stop: &AtomicBool,
) {
	let role = Role::take(closed).expect("takes").expect("free");
	let holding = Holding {
		program: "test",
		network: NetworkOptions {
			discovery: false,
			port: Some(port),
			accept: true,
		},
		known: other
			.into_iter()
			.map(|(endpoint, port)| {
				(
					endpoint,
					vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))],
				)
			})
			.collect(),
		quiet: Duration::from_millis(200),
		appear_wait: Duration::from_millis(200),
		check,
		stop,
		now: &clock,
	};
	nibkeep_node::hold(
		closed,
		role,
		&holding,
		&|| TestRng::new("a holder's thread"),
		&mut |_| {},
	)
	.expect("holds");
}

#[test]
fn a_holder_checks_the_copies_and_brings_a_damaged_one_again() {
	let (laptop, desk) = laptop_and_desk("holders check");
	let mut rng = TestRng::new("holders check");
	// A film on the laptop, which reaches the desk.
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "holders check film");
	let film = folder.join("film.mp4");
	std::fs::write(&film, vec![4; 60_000]).expect("writes");
	{
		let mut laptops = laptop.open().expect("opens");
		let mut desks = desk.open().expect("opens");
		let space = laptops.personal_space();
		laptops
			.add_file(space, &film, now() + 60_000, &mut rng)
			.expect("adds");
		let (first, second) =
			common::sync_in_memory(&mut desks, &mut laptops, now() + 70_000, (1, 0));
		first.expect("syncs");
		second.expect("syncs");
	}
	// The desk's copy is damaged. A round began a round's time ago there, so that the whole
	// of it is due when the holder looks.
	let replica = common::replica_folder(
		&nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR")).join("holders check desk"),
	);
	let (sealed, _) = (0..=255_u8)
		.flat_map(|byte| {
			nibkeep_store::blobs_starting_with(&replica, &nibkeep_base::Hash::of(b""), byte)
				.expect("lists")
		})
		.next()
		.expect("the desk holds the film");
	let digest = sealed.digest().iter().fold(String::new(), |mut hex, byte| {
		let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
		hex
	});
	let path = nibkeep_store::blob_directory(&replica)
		.join(sealed.algorithm().name())
		.join(&digest[..2])
		.join(&digest);
	let mut bytes = std::fs::read(&path).expect("reads");
	bytes[100] ^= 1;
	std::fs::write(&path, &bytes).expect("writes");
	{
		let mut desks = desk.open().expect("opens");
		let desk_records = desks
			.sync_records(TestRng::new("a reader"))
			.expect("records");
		desks
			.check_copies(
				&desk_records,
				&|| clock() - nibkeep_node::ROUND_MILLIS,
				nibkeep_node::Pace::DEFAULT,
				&|| false,
			)
			.expect("begins a round");
	}
	let quick = nibkeep_node::Pace {
		wait: Duration::ZERO,
		rest: 0,
		run_for: Duration::from_secs(10),
	};
	let stop = AtomicBool::new(false);
	let ports = [free_port(), free_port()];
	let endpoints = [endpoint(&laptop), endpoint(&desk)];
	std::thread::scope(|scope| {
		scope.spawn(|| run_holder(&laptop, ports[0], Some((endpoints[1], ports[1])), &stop));
		scope.spawn(|| {
			run_holder_checking(
				&desk,
				(ports[1], Some((endpoints[0], ports[0]))),
				quick,
				&stop,
			);
		});
		let _stops = Stops(&stop);
		// The desk's holder finds the damage, moves the copy aside, and syncs, which brings
		// an intact copy from the laptop.
		wait_until("the desk's copy is found damaged and replaced", || {
			let desks = desk.open().expect("opens");
			let status = desks
				.check_status(
					&desks
						.sync_records(TestRng::new("a reader"))
						.expect("records"),
					clock(),
				)
				.expect("reads");
			status.damaged.iter().any(|copy| {
				copy.copy
					== nibkeep_node::Damaged::Sealed {
						sealed,
						replaced: true,
					}
			})
		});
		assert_eq!(
			blake3::hash(&std::fs::read(&path).expect("reads")).as_bytes(),
			sealed.digest()
		);
		stop.store(true, Ordering::SeqCst);
	});
}

#[test]
fn one_program_holds_the_role_at_a_time() {
	let mut rng = TestRng::new("one holder");
	let (_, library) = library("one holder", &mut rng);
	let closed = library.close();
	let held = Role::take(&closed).expect("takes").expect("free");
	assert!(Role::take(&closed).expect("takes").is_none(), "held");
	drop(held);
	let again = Role::take(&closed).expect("takes");
	assert!(again.is_some(), "free again");
	// One that waits takes it once the holder lets go.
	let stop = AtomicBool::new(false);
	std::thread::scope(|scope| {
		let waiting = scope.spawn(|| Role::wait(&closed, &stop).expect("waits"));
		std::thread::sleep(Duration::from_millis(400));
		drop(again);
		assert!(waiting.join().expect("waited").is_some());
	});
}

#[test]
fn holders_answer_and_connect_sync_what_comes_and_record_it() {
	let (laptop, desk) = laptop_and_desk("holders");
	let mut rng = TestRng::new("holders");
	add_person(&desk, "Ursula Kroeber", &mut rng);
	let stop = AtomicBool::new(false);
	let ports = [free_port(), free_port()];
	let endpoints = [endpoint(&laptop), endpoint(&desk)];
	std::thread::scope(|scope| {
		scope.spawn(|| run_holder(&laptop, ports[0], Some((endpoints[1], ports[1])), &stop));
		scope.spawn(|| run_holder(&desk, ports[1], Some((endpoints[0], ports[0])), &stop));
		let _stops = Stops(&stop);
		// As they take the role, they sync with each other.
		wait_until("the desk's person reaches the laptop", || {
			found(&laptop, "Kroeber") == 1
		});
		// A change that another program makes on the laptop travels on a moment later.
		add_person(&laptop, "Octavia Butler", &mut rng);
		wait_until("the laptop's change reaches the desk", || {
			found(&desk, "Butler") == 1
		});
		// The laptop's holder recorded itself, the desk, and their syncs.
		let laptop_records = records(&laptop);
		let holder = laptop_records.holder().expect("reads").expect("a holder");
		assert_eq!(holder.program, "test");
		assert_eq!(holder.port, Some(ports[0]));
		wait_until("the desk is recorded as synced", || {
			laptop_records
				.replicas()
				.expect("reads")
				.values()
				.any(|replica| {
					replica.present && replica.synced.is_some() && replica.error.is_none()
				})
		});
		// Another program asks the desk's holder to sync with all it finds, and it does.
		let desk_records = records(&desk);
		let ask = desk_records.ask(None, clock()).expect("asks");
		wait_until("the ask is answered", || {
			desk_records.answered(ask).expect("reads").is_some()
		});
		stop.store(true, Ordering::SeqCst);
	});
	assert_eq!(records(&laptop).holder().expect("reads"), None, "let go");
	for closed in [&laptop, &desk] {
		let mut library = closed.open().expect("opens");
		let report = library.verify(clock()).expect("verifies");
		assert!(report.is_sound(), "{report:?}");
	}
}

#[test]
fn a_holder_refused_by_a_device_that_only_connects_records_no_failed_sync() {
	let (laptop, desk) = laptop_and_desk("refused");
	let mut rng = TestRng::new("refused");
	add_person(&desk, "Ursula Kroeber", &mut rng);
	let stop = AtomicBool::new(false);
	let ports = [free_port(), free_port()];
	let endpoints = [endpoint(&laptop), endpoint(&desk)];
	let desk_id = closed_id(&desk);
	// The desk syncs once, as `nib sync` does: its endpoint only connects.
	let mut desk_library = desk.open().expect("opens");
	let network = desk_library
		.network(NetworkOptions {
			discovery: false,
			port: Some(ports[1]),
			accept: false,
		})
		.expect("starts");
	network.remember(
		endpoints[0],
		&[SocketAddr::from((Ipv4Addr::LOCALHOST, ports[0]))],
		None,
	);
	std::thread::scope(|scope| {
		scope.spawn(|| run_holder(&laptop, ports[0], Some((endpoints[1], ports[1])), &stop));
		let _stops = Stops(&stop);
		let laptop_records = records(&laptop);
		let desk_record = || {
			laptop_records
				.replicas()
				.expect("reads")
				.get(&desk_id)
				.cloned()
		};
		// The laptop's holder finds the desk, connects to it at once, and is refused: no
		// sync failed, as the desk syncs by itself.
		wait_until("the desk is seen", || {
			desk_record().is_some_and(|record| record.present)
		});
		std::thread::sleep(Duration::from_secs(1));
		let record = desk_record().expect("a record");
		assert_eq!(
			(record.synced, record.error.as_deref()),
			(None, None),
			"{record:?}"
		);
		// The desk's own sync reaches the laptop's holder, which records it.
		let synced =
			nibkeep_node::sync_with_others(&mut desk_library, &network, PATIENCE, &clock, &mut rng)
				.expect("syncs");
		assert!(synced.iter().all(|with| with.synced.is_ok()), "{synced:?}");
		wait_until("the desk's person reaches the laptop", || {
			found(&laptop, "Kroeber") == 1
		});
		wait_until("the desk is recorded as synced", || {
			desk_record().is_some_and(|record| record.synced.is_some())
		});
		std::thread::sleep(Duration::from_secs(1));
		let record = desk_record().expect("a record");
		assert_eq!(record.error, None, "{record:?}");
		stop.store(true, Ordering::SeqCst);
	});
}

#[test]
fn pairing_borrows_the_network_from_the_holder() {
	let (laptop, _desk) = laptop_and_desk("borrowed");
	let stop = AtomicBool::new(false);
	let port = free_port();
	std::thread::scope(|scope| {
		scope.spawn(|| run_holder(&laptop, port, None, &stop));
		let _stops = Stops(&stop);
		let laptop_records = records(&laptop);
		let port_of = || {
			laptop_records
				.holder()
				.expect("reads")
				.and_then(|holder| holder.port)
		};
		wait_until("the holder's endpoint runs", || port_of() == Some(port));
		let library = laptop.open().expect("opens");
		let borrowed = nibkeep_node::borrow_network(
			&library,
			&AtomicBool::new(false),
			TestRng::new("borrows"),
		)
		.expect("borrows");
		// The holder runs no endpoint meanwhile, so that pairing has the port.
		assert_eq!(port_of(), None);
		assert!(
			std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok(),
			"the port is free"
		);
		drop(borrowed);
		wait_until("the endpoint runs again", || port_of() == Some(port));
		stop.store(true, Ordering::SeqCst);
	});
	// Without a holder, borrowing takes the role.
	let library = laptop.open().expect("opens");
	let borrowed =
		nibkeep_node::borrow_network(&library, &AtomicBool::new(false), TestRng::new("borrows"))
			.expect("borrows");
	assert!(
		Role::take(&laptop).expect("takes").is_none(),
		"held while borrowed"
	);
	drop(borrowed);
	assert!(Role::take(&laptop).expect("takes").is_some());
	let _ = Library::exists;
}

/// Holds the role of `closed` in the background as `program` names itself, on `port`, with
/// the other replica at `other`.
fn background(closed: &Closed, program: &str, port: u16, other: ([u8; 32], u16)) -> Background {
	let mut options = BackgroundOptions::new(
		program,
		NetworkOptions {
			discovery: false,
			port: Some(port),
			accept: true,
		},
	);
	options.known = vec![(
		other.0,
		vec![SocketAddr::from((Ipv4Addr::LOCALHOST, other.1))],
	)];
	options.quiet = Duration::from_millis(200);
	options.appear_wait = Duration::from_millis(200);
	Background::start(closed.without_secrets(), options, clock, || {
		TestRng::new("a background thread")
	})
}

#[test]
fn apps_hold_the_role_in_the_background_in_turn_and_ask_it_to_sync() {
	let (laptop, desk) = laptop_and_desk("background");
	let mut rng = TestRng::new("background");
	let ports = [free_port(), free_port()];
	let endpoints = [endpoint(&laptop), endpoint(&desk)];
	// Two apps on the laptop's replica, and one on the desk's.
	let first = background(&laptop, "first", ports[0], (endpoints[1], ports[1]));
	let laptop_records = records(&laptop);
	let holder = || {
		laptop_records
			.holder()
			.expect("reads")
			.filter(|holder| holder.port.is_some())
			.map(|holder| holder.program)
	};
	wait_until("the first holds the role", || {
		holder().as_deref() == Some("first")
	});
	let second = background(&laptop, "second", ports[0], (endpoints[1], ports[1]));
	let desk_app = background(&desk, "desk", ports[1], (endpoints[0], ports[0]));
	assert!(first.state().holding);
	assert!(!second.state().holding, "waits");
	// A change made on the desk travels to the laptop.
	add_person(&desk, "Ursula Kroeber", &mut rng);
	wait_until("the desk's person reaches the laptop", || {
		found(&laptop, "Kroeber") == 1
	});
	// Another program asks the laptop's holder to sync, and reads what came of it.
	let asked = nibkeep_node::ask_to_sync(&laptop_records, None, &clock, PATIENCE)
		.expect("asks")
		.expect("answered");
	let desk_id = closed_id(&desk);
	let record = laptop_records.replicas().expect("reads")[&desk_id].clone();
	assert!(record.synced >= Some(asked), "{record:?}");
	assert_eq!(record.error, None);
	// The first lets go, and the second takes over.
	first.wait();
	wait_until("the second takes over", || {
		holder().as_deref() == Some("second")
	});
	assert!(second.state().holding);
	add_person(&laptop, "Octavia Butler", &mut rng);
	wait_until("the laptop's change reaches the desk", || {
		found(&desk, "Butler") == 1
	});
	second.wait();
	desk_app.wait();
	assert_eq!(records(&laptop).holder().expect("reads"), None, "let go");
}

/// The ID of a closed library's replica.
fn closed_id(closed: &Closed) -> nibkeep_base::EntityId {
	let library = closed.open().expect("opens");
	library
		.replicas()
		.expect("reads")
		.iter()
		.find(|replica| replica.this)
		.expect("itself")
		.id
}

#[test]
#[cfg_attr(
	target_os = "ios",
	ignore = "iOS lists no mounts to look for a backup on: backups from a phone are on Android alone"
)]
fn a_holder_updates_a_backup_that_it_finds() {
	let mut rng = TestRng::new("holder backs up");
	let (_, mut library) = library("holder backs up", &mut rng);
	let files =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "holder backs up files");
	std::fs::write(files.join("recipe.txt"), b"three apples").expect("writes");
	let space = library.personal_space();
	library
		.add_file(space, &files.join("recipe.txt"), now() + 60_000, &mut rng)
		.expect("adds");
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "holder backs up drive")
			.join("Backup");
	let backup = library
		.add_backup(
			&folder,
			&nibkeep_node::NewBackup {
				name: "Blue stick".to_owned(),
				..nibkeep_node::NewBackup::default()
			},
			now() + 61_000,
			&mut rng,
		)
		.expect("adds")
		.backup;
	let closed = library.close();
	let stop = AtomicBool::new(false);
	let mut updated = None;
	std::thread::scope(|scope| {
		let _stops = Stops(&stop);
		let role = Role::take(&closed).expect("takes").expect("free");
		let holding = Holding {
			program: "test",
			network: NetworkOptions {
				discovery: false,
				port: Some(free_port()),
				accept: true,
			},
			known: Vec::new(),
			quiet: Duration::from_millis(200),
			appear_wait: Duration::from_millis(200),
			check: nibkeep_node::Pace::DEFAULT,
			stop: &stop,
			now: &clock,
		};
		let started = Instant::now();
		let stopping = &stop;
		let watchdog = scope.spawn(move || {
			while !stopping.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(60) {
				std::thread::sleep(Duration::from_millis(50));
			}
			stopping.store(true, Ordering::SeqCst);
		});
		nibkeep_node::hold(
			&closed,
			role,
			&holding,
			&|| TestRng::new("a holder's thread"),
			&mut |held| {
				if let nibkeep_node::Held::BackedUp(done) = held {
					updated = Some(done);
					stop.store(true, Ordering::SeqCst);
				}
			},
		)
		.expect("holds");
		watchdog.join().expect("joins");
	});
	let updated = updated.expect("the holder updated the backup");
	assert_eq!(updated.backup.id, backup.id);
	let done = updated.updated.expect("updated");
	assert_eq!(done.written.blobs, 1, "{done:?}");
	// It remembers where it found it.
	let library = closed.open().expect("opens");
	let records = library
		.sync_records(TestRng::new("a reader"))
		.expect("records");
	let known = records.backup_folders().expect("reads");
	assert!(
		known
			.get(&backup.id)
			.is_some_and(|found| found.updated.is_some())
	);
}
