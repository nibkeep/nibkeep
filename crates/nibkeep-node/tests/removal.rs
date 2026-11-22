//! An everyday device removes the copies of files that it no longer keeps, only once other
//! disks whose copies together meet each file's policy have proven, right then, that they
//! hold theirs; a device that proved a copy holds it for 10 minutes, and two devices that
//! remove one file at once each hold it for the other ([technical design: the planner]).
//!
//! [technical design: the planner]: ../../../docs/Technical-design.md#99-the-planner

mod common;

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::devices::{Devices, blob_files, bytes};
use common::replica_folder;
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_node::{Behind, DeviceRole, MoveHistory, MoveSecrets, NewProtection};

/// A day, in milliseconds.
const DAY: u64 = 86_400_000;

/// How long a device holds a copy that it proved, in milliseconds: 10 minutes.
const HOLD_FOR: u64 = 600_000;

/// How often the holder of the sync role runs a pass of removals, in milliseconds: every
/// 5 minutes.
const PASSES_EVERY: u64 = 300_000;

/// How long a replica waits before it issues its account again while its state stays the
/// same, in milliseconds, and a little more.
const ACCOUNTS_AGAIN: u64 = 61_000;

/// A NAS that is an archive node, a desk and a laptop, each on a disk of its own, which all
/// hold an older and a newer file of 30,000 bytes that the NAS added, so that the others hold
/// them sealed alone, and know of each other's copies. Returns the devices, the desk, the laptop, the older file with its content,
/// and the newer's content.
fn three(name: &str) -> (Devices, usize, usize, (EntityId, Hash), Hash) {
	let mut devices = Devices::new(name, true);
	let desk = devices.pair("desk", None);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let (file, older) = devices.add(0, "older.bin", &bytes(1, 30_000));
	let (_, newer) = devices.add(0, "newer.bin", &bytes(2, 30_000));
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	// Their accounts list what came, and travel through the NAS.
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(desk, 0);
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	for device in [0, desk, laptop] {
		assert!(devices.holds(device, &older));
		assert!(devices.holds(device, &newer));
	}
	(devices, desk, laptop, (file, older), newer)
}

/// Gives a device room for one of the files, the newer.
fn keeps_one(devices: &mut Devices, device: usize) {
	devices.mark(device, DeviceRole::Everyday, Some(35_000));
}

#[test]
fn a_device_removes_a_file_it_no_longer_keeps_once_other_disks_prove_theirs() {
	let (mut devices, desk, laptop, (_, older), newer) = three("removal, removes");
	keeps_one(&mut devices, laptop);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 1, "{removed:?}");
	assert!(removed.bytes > 30_000);
	assert_eq!(removed.waiting, 0);
	assert!(!devices.holds(laptop, &older));
	assert!(devices.holds(laptop, &newer));
	// The NAS, as Standard asks an archive node, and the desk keep theirs, and hold them for
	// 10 minutes, as they proved them.
	assert!(devices.holds(0, &older));
	assert!(devices.holds(desk, &older));
	assert_eq!(devices.held_for_others(0).len(), 1);
	assert_eq!(devices.held_for_others(desk).len(), 1);
	devices.advance(HOLD_FOR);
	assert!(devices.held_for_others(0).is_empty());
	// A sync does not bring it back, and a pass after finds nothing more to remove.
	devices.sync(laptop, 0);
	assert!(!devices.holds(laptop, &older));
	assert_eq!(devices.remove(laptop).copies, 0);
}

#[test]
fn a_device_that_waits_removes_nothing_before_its_wait_has_passed() {
	let (mut devices, _, laptop, (_, older), _) = three("removal, waits");
	// The laptop was an archive node, which waits 30 days: no longer one, it still waits
	// that long before it lets a copy go on the other devices' proofs.
	devices.mark(laptop, DeviceRole::Archive, None);
	keeps_one(&mut devices, laptop);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 0, "{removed:?}");
	assert!(removed.waiting >= 1, "{removed:?}");
	assert!(devices.holds(laptop, &older));
	devices.advance(29 * DAY);
	assert_eq!(devices.remove(laptop).copies, 0);
	// Once 30 days have passed since it found that it no longer keeps the file, it goes.
	devices.advance(2 * DAY);
	devices.sync(laptop, 0);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 1, "{removed:?}");
	assert!(!devices.holds(laptop, &older));
}

#[test]
fn an_account_that_lists_a_copy_is_not_enough() {
	let (mut devices, desk, laptop, (_, older), _) = three("removal, accounts are claims");
	// The desk no longer keeps the older file, and loses its copy, while its account, as the
	// laptop has it, still lists it.
	keeps_one(&mut devices, desk);
	let folder = replica_folder(&devices.folders[desk]);
	for file in blob_files(&folder.join("blobs")) {
		fs::remove_file(file).expect("removes");
	}
	keeps_one(&mut devices, laptop);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 0);
	assert_eq!(removed.waiting, 1);
	assert!(devices.holds(laptop, &older));
	assert!(!devices.holds(desk, &older));
	// The NAS proved its copy, and holds it all the same.
	assert_eq!(devices.held_for_others(0).len(), 1);
}

#[test]
fn a_device_that_lost_a_copy_it_keeps_gets_it_back_from_the_remover_before_it_proves() {
	let (mut devices, desk, laptop, (_, older), _) = three("removal, repaired first");
	// The desk loses its copies, which it keeps: the remover's sync gives them back first,
	// checked, so the desk proves what it holds then, and keeps it.
	let folder = replica_folder(&devices.folders[desk]);
	for file in blob_files(&folder.join("blobs")) {
		fs::remove_file(file).expect("removes");
	}
	keeps_one(&mut devices, laptop);
	assert_eq!(devices.remove(laptop).copies, 1);
	assert!(!devices.holds(laptop, &older));
	assert!(devices.holds(desk, &older));
	assert!(devices.holds(0, &older));
}

#[test]
fn two_devices_that_remove_one_file_at_once_hold_it_for_each_other() {
	let (mut devices, desk, laptop, (_, older), _) = three("removal, at once");
	keeps_one(&mut devices, laptop);
	keeps_one(&mut devices, desk);
	let mut laptops = devices.plan_removals(laptop);
	let mut desks = devices.plan_removals(desk);
	devices.prove(laptop, &mut laptops);
	devices.prove(desk, &mut desks);
	// Each proved the other's copy, so holds its own: neither removes.
	assert_eq!(devices.remove_proven(laptop, &laptops).copies, 0);
	assert_eq!(devices.remove_proven(desk, &desks).copies, 0);
	assert!(devices.holds(laptop, &older));
	assert!(devices.holds(desk, &older));
	// Five minutes on, the passes run again: each holds the file for the other still, so
	// neither asks the other, which would hold it there anew, and the holds can end.
	devices.advance(PASSES_EVERY);
	assert_eq!(devices.remove(laptop).copies, 0);
	assert_eq!(devices.remove(desk).copies, 0);
	// Once the holds end, one removes first; the other then holds the file for it, and
	// after that finds its copy unproven.
	devices.advance(HOLD_FOR - PASSES_EVERY);
	assert_eq!(devices.remove(laptop).copies, 1);
	assert_eq!(devices.remove(desk).copies, 0);
	devices.advance(PASSES_EVERY);
	assert_eq!(devices.remove(desk).copies, 0);
	devices.advance(PASSES_EVERY);
	assert_eq!(devices.remove(desk).copies, 0);
	assert!(devices.holds(desk, &older));
	assert!(devices.holds(0, &older));
	assert!(!devices.holds(laptop, &older));
}

#[test]
fn a_file_whose_policy_needs_a_copy_away_from_home_stays_until_relays() {
	let (mut devices, _, laptop, (file, older), _) = three("removal, away");
	let space = devices.personal();
	let critical = devices.libraries[laptop]
		.policies(space)
		.expect("lists")
		.into_iter()
		.find(|policy| policy.name.as_deref() == Some("Critical"))
		.expect("a Critical policy")
		.id;
	let at = devices.tick();
	devices.libraries[laptop]
		.set_policy(space, file, Some(critical), at, &mut devices.rng)
		.expect("sets");
	keeps_one(&mut devices, laptop);
	let removed = devices.remove(laptop);
	assert_eq!(removed.copies, 0);
	assert_eq!(removed.waiting, 1);
	assert!(devices.holds(laptop, &older));
}

#[test]
fn nothing_is_removed_while_no_device_is_an_archive_node() {
	let mut devices = Devices::new("removal, no archive node", false);
	let laptop = devices.pair("laptop", Some(35_000));
	devices.record_endpoints();
	let (_, older) = devices.add(laptop, "older.bin", &bytes(3, 30_000));
	devices.add(laptop, "newer.bin", &bytes(4, 30_000));
	devices.sync(laptop, 0);
	let removed = devices.remove(laptop);
	assert_eq!(removed, nibkeep_node::Removed::default());
	assert!(devices.holds(laptop, &older));
}

/// Holds the role of `closed` until `stop` is set, on `port`, with the other replicas at
/// `others`, checking and removing at the pace `check`, with the wall clock `now`.
fn run_holder(
	closed: &nibkeep_node::Closed,
	(port, others): (u16, Vec<([u8; 32], u16)>),
	check: nibkeep_node::Pace,
	(stop, now): (&AtomicBool, &(dyn Fn() -> u64 + Sync)),
) {
	let role = nibkeep_node::Role::take(closed)
		.expect("takes")
		.expect("free");
	let holding = nibkeep_node::Holding {
		program: "test",
		network: nibkeep_node::NetworkOptions {
			discovery: false,
			port: Some(port),
			accept: true,
		},
		known: others
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
		now,
	};
	nibkeep_node::hold(
		closed,
		role,
		&holding,
		&|| nibkeep_testkit::TestRng::new("a holder's thread"),
		&mut |_| {},
	)
	.expect("holds");
}

/// A UDP port that is free now, on this computer.
fn free_port() -> u16 {
	UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
		.and_then(|socket| socket.local_addr())
		.expect("binds")
		.port()
}

#[test]
fn the_holder_of_the_sync_role_removes_over_the_network() {
	let (mut devices, _, laptop, (_, older), newer) = three("removal, holder");
	keeps_one(&mut devices, laptop);
	let space = devices.personal();
	let at = devices.tick();
	let clock = move || at;
	let endpoints: Vec<[u8; 32]> = devices
		.libraries
		.iter()
		.map(nibkeep_node::Library::endpoint)
		.collect();
	let closed: Vec<nibkeep_node::Closed> = devices
		.libraries
		.drain(..)
		.map(nibkeep_node::Library::close)
		.collect();
	let ports: Vec<u16> = closed.iter().map(|_| free_port()).collect();
	let others = |device: usize| -> Vec<([u8; 32], u16)> {
		(0..closed.len())
			.filter(|other| *other != device)
			.map(|other| (endpoints[other], ports[other]))
			.collect()
	};
	// The laptop removes at once; the others only answer.
	let quick = nibkeep_node::Pace {
		wait: Duration::ZERO,
		rest: 0,
		run_for: Duration::from_secs(10),
	};
	let holds = |device: usize, content: &Hash| {
		closed[device]
			.open()
			.expect("opens")
			.file_content(space, content, u64::MAX)
			.expect("reads")
			.is_some()
	};
	let stop = AtomicBool::new(false);
	std::thread::scope(|scope| {
		for device in 0..closed.len() {
			let pace = if device == laptop {
				quick
			} else {
				nibkeep_node::Pace::DEFAULT
			};
			let (closed, port, others) = (&closed[device], ports[device], others(device));
			let (stop, clock) = (&stop, &clock);
			scope.spawn(move || run_holder(closed, (port, others), pace, (stop, clock)));
		}
		let started = Instant::now();
		while holds(laptop, &older) {
			assert!(
				started.elapsed() < Duration::from_secs(60),
				"not removed in time"
			);
			std::thread::sleep(Duration::from_millis(100));
		}
		stop.store(true, Ordering::SeqCst);
	});
	assert!(holds(laptop, &newer));
	assert!(holds(0, &older));
	assert!(holds(1, &older));
}

#[test]
fn a_damaged_copy_of_a_file_no_longer_kept_comes_again_and_then_goes() {
	let (mut devices, _, laptop, (_, older), newer) = three("removal, damaged first");
	keeps_one(&mut devices, laptop);
	// The laptop's copies are damaged; a check moves them aside.
	let folder = replica_folder(&devices.folders[laptop]);
	for file in blob_files(&folder.join("blobs")) {
		let mut data = fs::read(&file).expect("reads");
		data[100] ^= 1;
		fs::write(&file, data).expect("writes");
	}
	let at = devices.tick();
	devices.libraries[laptop].verify(at).expect("verifies");
	assert!(!devices.holds(laptop, &older));
	// A sync brings back even the one it no longer keeps, so that the damaged copy goes.
	devices.sync(laptop, 0);
	assert!(devices.holds(laptop, &older));
	assert!(devices.holds(laptop, &newer));
	assert_eq!(
		blob_files(&folder.join("damaged")),
		Vec::<std::path::PathBuf>::new()
	);
	// Then it goes the planner's way.
	assert_eq!(devices.remove(laptop).copies, 1);
	assert!(!devices.holds(laptop, &older));
}

#[test]
fn every_device_settles_on_the_smaller_of_two_sealed_copies_of_a_content() {
	// No archive node: every device keeps every file, and still lets a twin go.
	let mut devices = Devices::new("removal, twins", false);
	let laptop = devices.pair("laptop", None);
	let desk = devices.pair("desk", None);
	devices.record_endpoints();
	// Both add the same content before they sync: two sealed copies of it.
	devices.add(laptop, "same.txt", &bytes(7, 3_000));
	devices.add(desk, "same again.txt", &bytes(7, 3_000));
	let blobs = |devices: &Devices, device: usize| -> Vec<std::ffi::OsString> {
		blob_files(&replica_folder(&devices.folders[device]).join("blobs"))
			.into_iter()
			.filter_map(|file| file.file_name().map(std::ffi::OsStr::to_owned))
			.collect()
	};
	let (laptops, desks) = (blobs(&devices, laptop), blobs(&devices, desk));
	assert_eq!(laptops.len(), 1);
	assert_ne!(laptops, desks);
	let smaller = laptops.clone().min(desks);
	for _ in 0..2 {
		devices.sync(laptop, 0);
		devices.sync(desk, 0);
	}
	for device in [0, laptop, desk] {
		devices.remove(device);
	}
	for device in [0, laptop, desk] {
		assert_eq!(blobs(&devices, device), smaller, "device {device}");
	}
	// The twin does not come back.
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	for device in [0, laptop, desk] {
		assert_eq!(blobs(&devices, device), smaller, "device {device}");
	}
}

/// A NAS and a laptop that holds the secrets, and a space of the user's own besides the
/// personal one, "Archive", into which the laptop moves a secret file it adds, which no
/// origin holds; the move asks for a purge of what it leaves behind. Returns the devices, the
/// laptop, the archive, and the file in each space.
fn moved_secret_file(name: &str) -> (Devices, usize, SpaceId, (EntityId, EntityId)) {
	// No archive node: Standard asks for two copies, and no archive node's yet.
	let mut devices = Devices::new(name, false);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let personal = devices.personal();
	let at = devices.tick();
	devices.libraries[laptop]
		.set_up_secrets(
			&NewProtection::Passphrase(
				"tr0ub4dor",
				nibkeep_node::PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			at,
			&mut devices.rng,
		)
		.expect("sets up");
	let at = devices.tick();
	let archive = devices.libraries[laptop]
		.create_space("Archive", at, &mut devices.rng)
		.expect("makes the space");
	let at = devices.tick();
	let added = devices.libraries[laptop]
		.add_secret_file(
			personal,
			"passport.pdf",
			bytes(5, 20_000).as_slice(),
			&[],
			at,
			&mut devices.rng,
		)
		.expect("adds");
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let at = devices.tick();
	let moved = devices.libraries[laptop]
		.move_entity(
			personal,
			added.file,
			archive,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Bring,
			at,
			&mut devices.rng,
		)
		.expect("moves");
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	(devices, laptop, archive, (added.file, moved))
}

/// Whether the laptop opens a secret file of a space.
fn opens(devices: &Devices, laptop: usize, (space, file): (SpaceId, EntityId)) -> bool {
	devices.libraries[laptop]
		.open_secret_file(space, file)
		.is_ok_and(|mut reader| {
			let mut bytes = Vec::new();
			std::io::Read::read_to_end(&mut reader, &mut bytes).is_ok() && !bytes.is_empty()
		})
}

#[test]
fn what_a_move_leaves_behind_is_purged_once_its_copies_where_it_went_are_proven() {
	let (mut devices, laptop, archive, (left, moved)) = moved_secret_file("removal, moves");
	let personal = devices.personal();
	// The NAS holds the new entity, but the purge waits for its copy there to be proven, so
	// what was left behind is still there.
	assert!(opens(&devices, laptop, (personal, left)));
	assert!(opens(&devices, laptop, (archive, moved)));
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(laptop, 0);
	let removed = devices.remove(laptop);
	assert_eq!(removed.moves, 1, "{removed:?}");
	assert!(!opens(&devices, laptop, (personal, left)));
	assert!(opens(&devices, laptop, (archive, moved)));
	assert_eq!(devices.remove(laptop).moves, 0);
}

#[test]
fn what_a_move_leaves_behind_stays_while_too_few_copies_where_it_went_are_proven() {
	let (mut devices, laptop, archive, (left, moved)) =
		moved_secret_file("removal, moves unproven");
	let personal = devices.personal();
	// The NAS's copies are damaged, every byte, as its account still lists them intact: it
	// holds them, so asks for none, and proves none.
	let folder = replica_folder(&devices.folders[0]);
	for file in blob_files(&folder.join("blobs")) {
		let data: Vec<u8> = fs::read(&file)
			.expect("reads")
			.iter()
			.map(|byte| byte ^ 0x5a)
			.collect();
		fs::write(&file, data).expect("writes");
	}
	devices.advance(ACCOUNTS_AGAIN);
	assert_eq!(devices.remove(laptop).moves, 0);
	assert!(opens(&devices, laptop, (personal, left)));
	assert!(opens(&devices, laptop, (archive, moved)));
}

/// A NAS that is an archive node, a desk and a laptop, which all hold sealed a film that the
/// laptop added, whose original stays where it was found on the laptop, and whose policy then
/// turns to Replaceable: one copy, kept in place. Returns the devices, the desk, the laptop,
/// and the film's original.
fn film_kept_in_place(name: &str) -> (Devices, usize, usize, std::path::PathBuf) {
	let mut devices = Devices::new(name, true);
	let desk = devices.pair("desk", None);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let (file, _) = devices.add(laptop, "film.mkv", &bytes(9, 40_000));
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	let space = devices.personal();
	let replaceable = devices.libraries[laptop]
		.policies(space)
		.expect("lists")
		.into_iter()
		.find(|policy| policy.name.as_deref() == Some("Replaceable"))
		.expect("a Replaceable policy")
		.id;
	let at = devices.tick();
	devices.libraries[laptop]
		.set_policy(space, file, Some(replaceable), at, &mut devices.rng)
		.expect("sets");
	devices.advance(ACCOUNTS_AGAIN);
	for _ in 0..2 {
		devices.sync(laptop, 0);
		devices.sync(desk, 0);
	}
	let original = nibkeep_testkit::test_directory(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("{name}, files {laptop} film.mkv"))
		.join("film.mkv");
	assert!(original.is_file());
	(devices, desk, laptop, original)
}

/// Has the NAS, an archive node that waits 30 days, try a pass, which removes nothing yet,
/// and then wait out its wait, after which the others sync with it again.
fn nas_waits_out(devices: &mut Devices, desk: usize, laptop: usize) {
	let early = devices.remove(0);
	assert_eq!(early.copies, 0, "{early:?}");
	assert!(early.waiting >= 1, "{early:?}");
	devices.advance(30 * DAY);
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
}

/// How many sealed blobs a device holds.
fn sealed_here(devices: &Devices, device: usize) -> usize {
	blob_files(&replica_folder(&devices.folders[device]).join("blobs")).len()
}

#[test]
fn a_file_whose_policy_turns_to_in_place_leaves_the_store_once_its_original_is_proven() {
	let (mut devices, desk, laptop, original) = film_kept_in_place("removal, leaves the store");
	for device in [0, desk, laptop] {
		assert_eq!(sealed_here(&devices, device), 1, "device {device}");
	}
	// The NAS, an archive node, which waits, lets nothing leave the store on another
	// device's proof before its wait has passed.
	nas_waits_out(&mut devices, desk, laptop);
	// The NAS and the desk let theirs go once the laptop proves its original; the laptop on
	// its own original, unchanged. Each pass asks every disk that may prove, which then
	// holds its copy for a while, so the passes are apart.
	for device in [0, desk, laptop] {
		let removed = devices.remove(device);
		assert_eq!(removed.copies, 1, "device {device}: {removed:?}");
		devices.advance(HOLD_FOR);
	}
	for device in [0, desk, laptop] {
		assert_eq!(sealed_here(&devices, device), 0, "device {device}");
	}
	// A sync brings none back, and the original is there as it was.
	devices.sync(laptop, 0);
	devices.sync(desk, 0);
	for device in [0, desk, laptop] {
		assert_eq!(sealed_here(&devices, device), 0, "device {device}");
	}
	assert_eq!(fs::read(&original).expect("reads"), bytes(9, 40_000));
}

#[test]
fn the_last_sealed_copy_of_a_file_kept_in_place_stays_while_its_original_is_changed() {
	let (mut devices, desk, laptop, original) = film_kept_in_place("removal, original changed");
	// The NAS, which waits, has found that it no longer keeps the film, a wait ago.
	nas_waits_out(&mut devices, desk, laptop);
	// The original is edited: the laptop no longer holds the content there, and proves
	// nothing, as its account, which the others have, still says it does.
	fs::write(&original, bytes(10, 41_000)).expect("writes");
	// The NAS lets its copy go on the desk's, which proves; the desk's is then the last,
	// which neither the laptop's original nor the NAS's copy, gone, stands for.
	assert_eq!(devices.remove(0).copies, 1);
	devices.advance(HOLD_FOR);
	assert_eq!(devices.remove(desk).copies, 0);
	assert_eq!(sealed_here(&devices, desk), 1);
}

/// How many origins a space holds that are neither deleted nor moved, as a device lists them:
/// records of where files were found.
fn origins_in(devices: &Devices, device: usize, space: SpaceId) -> usize {
	devices.libraries[device]
		.entities(space, Some(&nibkeep_std::ORIGIN))
		.expect("lists")
		.into_iter()
		.filter(|listed| {
			devices.libraries[device]
				.moved_into(space, listed.id)
				.expect("reads")
				.is_none()
		})
		.count()
}

#[test]
fn a_moved_file_takes_its_original_along_and_its_content_leaves_the_old_space() {
	let mut devices = Devices::new("removal, origin moves", false);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let personal = devices.personal();
	let at = devices.tick();
	let archive = devices.libraries[laptop]
		.create_space("Archive", at, &mut devices.rng)
		.expect("makes the space");
	// The laptop adds a letter: its original stays where it was found there.
	let (file, content) = devices.add(laptop, "letter.pdf", &bytes(11, 20_000));
	for _ in 0..2 {
		devices.sync(laptop, 0);
	}
	assert_eq!(origins_in(&devices, laptop, personal), 1);
	let at = devices.tick();
	let moved = devices.libraries[laptop]
		.move_entity(
			personal,
			file,
			archive,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Leave,
			at,
			&mut devices.rng,
		)
		.expect("moves");
	// The laptop holds the new space, so its original is told of there, and nowhere else.
	assert_eq!(origins_in(&devices, laptop, archive), 1);
	assert_eq!(origins_in(&devices, laptop, personal), 0);
	devices.sync(laptop, 0);
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let removed = devices.remove(laptop);
	assert!(removed.moves >= 1, "{removed:?}");
	devices.sync(laptop, 0);
	// The old space no longer reads the content, nothing there holding it; the new one does.
	let nas_reads = |devices: &Devices, space| {
		devices.libraries[0]
			.file_content(space, &content, u64::MAX)
			.expect("reads")
			.is_some()
	};
	assert!(!nas_reads(&devices, personal));
	assert!(nas_reads(&devices, archive));
	assert_eq!(origins_in(&devices, laptop, archive), 1);
	assert_eq!(origins_in(&devices, 0, archive), 1);
	assert!(
		devices.libraries[laptop]
			.moved_into(personal, file)
			.expect("reads")
			.is_some_and(|(space, entity)| space == archive && entity == moved)
	);
}

#[test]
fn an_original_on_another_device_stays_out_of_the_new_space_and_is_purged() {
	let mut devices = Devices::new("removal, origin stays out", false);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let personal = devices.personal();
	let at = devices.tick();
	let archive = devices.libraries[laptop]
		.create_space("Archive", at, &mut devices.rng)
		.expect("makes the space");
	// The laptop adds a letter, whose origin then says it was found on another member's
	// device, which no replica of the user's is on.
	let (file, content) = devices.add(laptop, "letter.pdf", &bytes(12, 20_000));
	let origin = devices.libraries[laptop]
		.entities(personal, Some(&nibkeep_std::ORIGIN))
		.expect("lists")
		.first()
		.expect("an origin")
		.id;
	let at = devices.tick();
	devices.libraries[laptop]
		.edit_entity(
			personal,
			origin,
			vec![nibkeep_node::Edit::Set(
				nibkeep_std::FOUND_ON,
				vec![nibkeep_model::Value::Reference(EntityId::from_bytes(
					[9; 16],
				))],
			)],
			&common::today(),
			at,
			&mut devices.rng,
		)
		.expect("edits");
	for _ in 0..2 {
		devices.sync(laptop, 0);
	}
	let at = devices.tick();
	devices.libraries[laptop]
		.move_entity(
			personal,
			file,
			archive,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Leave,
			at,
			&mut devices.rng,
		)
		.expect("moves");
	// It stays out of the new space.
	assert_eq!(origins_in(&devices, laptop, archive), 0);
	devices.sync(laptop, 0);
	devices.advance(ACCOUNTS_AGAIN);
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let removed = devices.remove(laptop);
	assert!(removed.moves >= 1, "{removed:?}");
	devices.sync(laptop, 0);
	// It was purged with what stayed behind, so nothing there holds the content any more.
	assert!(
		devices.libraries[0]
			.file_content(personal, &content, u64::MAX)
			.expect("reads")
			.is_none()
	);
	assert!(
		devices.libraries[0]
			.file_content(archive, &content, u64::MAX)
			.expect("reads")
			.is_some()
	);
	assert_eq!(origins_in(&devices, 0, archive), 0);
}

#[test]
fn a_move_without_history_leaves_an_origin_of_an_older_version_behind() {
	let mut devices = Devices::new("removal, origin of an older version", false);
	let laptop = devices.pair("laptop", None);
	devices.record_endpoints();
	let personal = devices.personal();
	let at = devices.tick();
	let archive = devices.libraries[laptop]
		.create_space("Archive", at, &mut devices.rng)
		.expect("makes the space");
	// The laptop adds a letter, whose origin records the first version, then edits it
	// through a checkout into a second.
	let (file, first) = devices.add(laptop, "letter.txt", &bytes(13, 20_000));
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"removal, origin of an older version, checkouts",
	);
	let at = devices.tick();
	let checked_out = devices.libraries[laptop]
		.checkout(personal, file, &folder, at, &mut devices.rng)
		.expect("checks out");
	fs::write(&checked_out.path, bytes(14, 21_000)).expect("writes");
	let at = devices.tick();
	let edits = devices.libraries[laptop]
		.record_edits(personal, checked_out.checkout, false, at, &mut devices.rng)
		.expect("records");
	let second = edits.version.expect("a new version");
	let at = devices.tick();
	devices.libraries[laptop]
		.end_checkout(personal, checked_out.checkout, at, &mut devices.rng)
		.expect("ends");
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let at = devices.tick();
	devices.libraries[laptop]
		.move_entity(
			personal,
			file,
			archive,
			MoveHistory::Leave,
			Behind::Purge,
			MoveSecrets::Leave,
			at,
			&mut devices.rng,
		)
		.expect("moves");
	// The origin names the first version, which the move leaves: it stays, and the new
	// space never sees that version.
	assert_eq!(origins_in(&devices, laptop, archive), 0);
	devices.sync(laptop, 0);
	devices.sync(laptop, 0);
	let reads = |devices: &Devices, space, content: &Hash| {
		devices.libraries[0]
			.file_content(space, content, u64::MAX)
			.expect("reads")
			.is_some()
	};
	assert!(!reads(&devices, archive, &first));
	assert!(reads(&devices, archive, &second));
}
