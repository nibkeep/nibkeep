//! Syncing from login ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)):
//! the list of libraries that every program reads and writes alike; the newest copy of
//! `nibsync` named in it; and the service, which holds each listed library's sync role,
//! follows the list, waits for a locked keyring, and says what it cannot serve.

mod common;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{now, pair_with};
use nibkeep_base::EntityId;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{Name, Value};
use nibkeep_node::at_login::{
	AtLogin, ENGINE_VERSION, EntryOutcome, EntryState, LoginEntry, SERVICE_PROGRAM, Served,
	ServiceEnded, ServiceOptions, ServiceStatus, Serving, name_own_copy, newer, serve_at_login,
	service_running, turn_off, turn_on,
};
use nibkeep_node::{
	Background, BackgroundOptions, Closed, KEYSTORE_SERVICE, Library, NetworkOptions, NewLibrary,
	NewProtection, NodeError, PairingCode, PassphraseParams, Unlocker,
};
use nibkeep_testkit::TestRng;

/// How long a test waits for what the service does.
const PATIENCE: Duration = Duration::from_secs(30);

/// Waits until `done` says so, or fails after [`PATIENCE`].
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
	let started = Instant::now();
	while !done() {
		assert!(started.elapsed() < PATIENCE, "{what}: not in time");
		std::thread::sleep(Duration::from_millis(50));
	}
}

/// Uses the mock keystore, once for all tests of this file.
fn mock_keystore() {
	static ONCE: Once = Once::new();
	ONCE.call_once(|| {
		let store: Arc<keyring_core::CredentialStore> =
			keyring_core::mock::Store::new().expect("a mock store");
		keyring_core::set_default_store(store);
	});
}

fn directory(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name)
}

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

/// A new library in a directory of its own, its keys in the keystore.
fn keystore_library(name: &str, rng: &mut TestRng) -> (PathBuf, Library) {
	mock_keystore();
	let directory = directory(name);
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&NewProtection::Keystore {
			service: KEYSTORE_SERVICE,
		},
		now(),
		rng,
	)
	.expect("creates");
	(directory, library)
}

/// The ID of a library's own replica.
fn replica_id(library: &Library) -> EntityId {
	library
		.replicas()
		.expect("reads")
		.into_iter()
		.find(|replica| replica.this)
		.expect("itself")
		.id
}

/// A laptop's library, its keys in the keystore, listed to be served from login; and a desk
/// paired with it, closed.
fn laptop_and_desk(name: &str) -> (Served, Closed) {
	let mut rng = TestRng::new(name);
	let (laptop_directory, mut laptop) = keystore_library(&format!("{name} laptop"), &mut rng);
	let desk = directory(&format!("{name} desk"));
	std::fs::remove_dir(&desk).expect("removes");
	let code = PairingCode::generate(&mut rng);
	let (paired, joined) = pair_with(&mut laptop, &desk, &code, &code, &mut rng);
	paired.expect("paired");
	let served = Served {
		replica: replica_id(&laptop),
		directory: laptop_directory,
	};
	(served, joined.expect("joined").library.close())
}

fn open_served(served: &Served) -> Library {
	Library::open(&served.directory, &Unlocker::Keystore).expect("opens")
}

/// The program that holds the role of a closed library's replica, once it runs an endpoint.
fn holder(library: &Library) -> Option<String> {
	library
		.sync_records(TestRng::new("a reader"))
		.expect("records")
		.holder()
		.expect("reads")
		.filter(|holder| holder.port.is_some())
		.map(|holder| holder.program)
}

fn found(library: &Library, word: &str) -> usize {
	library
		.search(library.personal_space(), word)
		.expect("searches")
		.len()
}

fn add_person(library: &mut Library, name: &str, rng: &mut TestRng) {
	let personal = library.personal_space();
	library
		.create_entity(
			personal,
			&[PERSON],
			vec![(NAME, Value::Name(Name::new(name)))],
			now() + 180_000,
			rng,
		)
		.expect("adds");
}

fn network(port: u16, other: ([u8; 32], u16)) -> BackgroundOptions {
	let mut options = BackgroundOptions::new(
		SERVICE_PROGRAM,
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
	options
}

/// A service on a thread of its own, which a test that fails stops.
struct Service {
	stop: Arc<AtomicBool>,
	thread: Option<JoinHandle<Result<ServiceEnded, NodeError>>>,
}

impl Service {
	fn start(settings: &Path, options: ServiceOptions) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		let thread = {
			let (settings, stop) = (settings.to_owned(), Arc::clone(&stop));
			std::thread::spawn(move || {
				serve_at_login(
					&settings,
					&options,
					clock,
					|| TestRng::new("the service"),
					&stop,
				)
			})
		};
		Self {
			stop,
			thread: Some(thread),
		}
	}

	fn ended(&self) -> bool {
		self.thread.as_ref().is_none_or(JoinHandle::is_finished)
	}

	fn end(mut self) -> ServiceEnded {
		wait_until("the service ends", || self.ended());
		self.thread
			.take()
			.expect("runs")
			.join()
			.expect("did not panic")
			.expect("ran")
	}
}

impl Drop for Service {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

fn status_of(settings: &Path, directory: &Path) -> Option<Serving> {
	ServiceStatus::read(settings)
		.expect("reads")
		.and_then(|status| status.of(directory).cloned())
}

#[test]
fn the_list_keeps_the_libraries_and_the_copy_that_serves_them() {
	let settings = directory("the list");
	assert_eq!(
		AtLogin::read(&settings).expect("reads"),
		AtLogin::default(),
		"none yet"
	);
	let mut rng = TestRng::new("the list");
	let first = Served {
		replica: EntityId::random(&mut rng),
		directory: PathBuf::from("/home/ada/Books/.nib"),
	};
	let second = Served {
		replica: EntityId::random(&mut rng),
		directory: PathBuf::from("/home/ada/Passwords/.nib"),
	};
	AtLogin::edit(&settings, |list| {
		list.program = Some(PathBuf::from("/opt/nibkeep/nibsync"));
		list.add(first.clone());
		list.add(second.clone());
	})
	.expect("edits");
	let list = AtLogin::read(&settings).expect("reads");
	assert_eq!(
		list.program.as_deref(),
		Some(Path::new("/opt/nibkeep/nibsync"))
	);
	assert_eq!(list.libraries, [first.clone(), second.clone()]);
	// A library in the same folder replaces the one before, as one made anew there.
	let anew = Served {
		replica: EntityId::random(&mut rng),
		directory: first.directory.clone(),
	};
	AtLogin::edit(&settings, |list| list.add(anew.clone())).expect("edits");
	assert_eq!(
		AtLogin::read(&settings).expect("reads").libraries,
		[second.clone(), anew.clone()]
	);
	assert!(AtLogin::edit(&settings, |list| list.remove(&second.directory)).expect("edits"));
	assert!(!AtLogin::edit(&settings, |list| list.remove(&second.directory)).expect("edits"));
	assert_eq!(AtLogin::read(&settings).expect("reads").libraries, [anew]);
	// It is a file that a person can read.
	let written = std::fs::read_to_string(settings.join("sync-at-login")).expect("reads");
	assert!(written.contains("/home/ada/Books/.nib"), "{written}");
	std::fs::write(settings.join("sync-at-login"), "not a list").expect("writes");
	assert!(matches!(
		AtLogin::read(&settings),
		Err(NodeError::Damaged(_))
	));
	assert!(!service_running(&settings).expect("looks"));
	assert_eq!(ServiceStatus::read(&settings).expect("reads"), None);
}

#[test]
fn the_newest_copy_is_named_and_one_of_the_same_version_stays() {
	assert!(newer("0.2.0", "0.1.9"));
	assert!(newer("0.10.0", "0.9.0"));
	assert!(!newer("1.0.0", "1.0.0-dev"), "a suffix counts for nothing");
	assert!(!newer("0.1.0", "0.1.0"));
	assert!(newer("0.1.0", "unknown"));

	let settings = directory("the newest copy");
	let own = settings.join("passwords").join("nibsync");
	let editors = settings.join("editor").join("nibsync");
	for copy in [&own, &editors] {
		std::fs::create_dir_all(copy.parent().expect("a folder")).expect("makes");
		std::fs::write(copy, "").expect("writes");
	}
	let named = || AtLogin::read(&settings).expect("reads").program;
	// The versions that the copies tell: this one's own, and the other's as given.
	let telling = |editor: Option<&'static str>| {
		let own = own.clone();
		move |copy: &Path| {
			if copy == own {
				Some("0.5.0".to_owned())
			} else {
				editor.map(str::to_owned)
			}
		}
	};
	assert!(
		name_own_copy(&settings, &own, &telling(None), None).expect("names"),
		"none named"
	);
	assert_eq!(named(), Some(own.clone()));
	assert!(
		!name_own_copy(&settings, &own, &telling(Some("0.1.0")), None).expect("names"),
		"itself"
	);

	let name_editors = || {
		AtLogin::edit(&settings, |list| list.program = Some(editors.clone())).expect("edits");
	};
	name_editors();
	assert!(!name_own_copy(&settings, &own, &telling(Some("0.5.0")), None).expect("names"));
	assert!(!name_own_copy(&settings, &own, &telling(Some("0.9.0")), None).expect("names"));
	assert_eq!(
		named(),
		Some(editors.clone()),
		"a newer or equal copy stays"
	);
	assert!(
		!name_own_copy(&settings, &own, &telling(None), None).expect("names"),
		"one that does not tell its version in time stays"
	);
	assert!(name_own_copy(&settings, &own, &telling(Some("0.1.0")), None).expect("names"));
	assert_eq!(named(), Some(own.clone()), "an older copy gives way");

	name_editors();
	std::fs::remove_file(&editors).expect("removes");
	assert!(name_own_copy(&settings, &own, &telling(Some("0.9.0")), None).expect("names"));
	assert_eq!(named(), Some(own), "a copy that is gone gives way");
}

#[test]
fn turning_on_starts_the_listed_copy_or_one_just_named() {
	let mut rng = TestRng::new("turning on");
	let (library_directory, library) = keystore_library("turning on library", &mut rng);
	let served = Served {
		replica: replica_id(&library),
		directory: library_directory,
	};
	drop(library);
	let settings = directory("turning on settings");
	let own = settings.join("nibsync");
	std::fs::write(&own, "").expect("writes");
	let same = |_: &Path| Some(ENGINE_VERSION.to_owned());
	assert_eq!(
		turn_on(&settings, served.clone(), (&own, &same), None)
			.expect("turns on")
			.start,
		Some(own.clone()),
		"none runs"
	);
	// One runs, named already: nothing to start.
	let mut options =
		ServiceOptions::new(own.clone(), network(free_port(), ([0; 32], free_port())));
	options.look = Duration::from_millis(100);
	let _service = Service::start(&settings, options);
	wait_until("the service runs", || {
		service_running(&settings).expect("looks")
	});
	assert_eq!(
		turn_on(&settings, served.clone(), (&own, &same), None)
			.expect("turns on")
			.start,
		None
	);
	// A newer copy, just named, is started, to take over as the one that runs ends.
	let newer_copy = settings.join("newer").join("nibsync");
	std::fs::create_dir_all(newer_copy.parent().expect("a folder")).expect("makes");
	std::fs::write(&newer_copy, "").expect("writes");
	let newer_one = newer_copy.clone();
	let telling = move |copy: &Path| {
		Some(
			if copy == newer_one {
				"999.0.0"
			} else {
				ENGINE_VERSION
			}
			.to_owned(),
		)
	};
	assert_eq!(
		turn_on(&settings, served, (&newer_copy, &telling), None)
			.expect("turns on")
			.start,
		Some(newer_copy)
	);
}

#[test]
fn the_service_syncs_the_listed_libraries_and_follows_the_list() {
	let (served, desk) = laptop_and_desk("follows the list");
	let settings = directory("follows the list settings");
	let program = settings.join("nibsync");
	AtLogin::edit(&settings, |list| {
		list.program = Some(program.clone());
		list.add(served.clone());
	})
	.expect("lists");
	let ports = [free_port(), free_port()];
	let endpoints = [
		open_served(&served).endpoint(),
		desk.open().expect("opens").endpoint(),
	];
	let service = Service::start(
		&settings,
		ServiceOptions::new(program, network(ports[0], (endpoints[1], ports[1]))),
	);
	let laptop = open_served(&served);
	wait_until("the service holds the laptop's role", || {
		holder(&laptop).as_deref() == Some(SERVICE_PROGRAM)
	});
	assert!(service_running(&settings).expect("looks"));
	wait_until("it says that it syncs", || {
		status_of(&settings, &served.directory) == Some(Serving::Syncing)
	});
	drop(laptop);

	// A change on the desk reaches the laptop through the service, with no app open there.
	let desk_app = Background::start(
		desk.without_secrets(),
		network(ports[1], (endpoints[0], ports[0])),
		clock,
		|| TestRng::new("the desk's app"),
	);
	let mut rng = TestRng::new("follows the list");
	add_person(&mut desk.open().expect("opens"), "Ursula Kroeber", &mut rng);
	wait_until("the desk's person reaches the laptop", || {
		found(&open_served(&served), "Kroeber") == 1
	});
	desk_app.wait();

	// Turned off, the library's role is let go, and with no library left, the service ends.
	assert!(
		turn_off(&settings, (&served.directory, None), None)
			.expect("turns off")
			.removed
	);
	assert_eq!(service.end(), ServiceEnded::Empty);
	assert_eq!(holder(&open_served(&served)), None, "let go");
	assert!(!service_running(&settings).expect("looks"));
	assert_eq!(ServiceStatus::read(&settings).expect("reads"), None);
}

#[test]
fn the_service_waits_for_a_locked_keyring_and_says_what_it_cannot_serve() {
	let mut rng = TestRng::new("cannot serve");
	let (keystore_directory, keystore) = keystore_library("cannot serve keystore", &mut rng);
	let served = Served {
		replica: replica_id(&keystore),
		directory: keystore_directory,
	};
	drop(keystore);
	let (other_directory, other) = keystore_library("cannot serve another", &mut rng);
	drop(other);
	let passphrase_directory = directory("cannot serve passphrase");
	let protection = NewProtection::Passphrase(
		"correct horse",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	let passphrase = Library::create(
		&passphrase_directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates");
	let protected = Served {
		replica: replica_id(&passphrase),
		directory: passphrase_directory,
	};
	drop(passphrase);
	// Another library's directory, under this one's replica's ID: as from another computer.
	let elsewhere = Served {
		replica: served.replica,
		directory: other_directory,
	};
	let gone = Served {
		replica: EntityId::random(&mut rng),
		directory: directory("cannot serve gone").join("nowhere"),
	};

	let settings = directory("cannot serve settings");
	let program = settings.join("nibsync");
	AtLogin::edit(&settings, |list| {
		list.program = Some(program.clone());
		list.libraries = vec![
			served.clone(),
			protected.clone(),
			elsewhere.clone(),
			gone.clone(),
		];
	})
	.expect("lists");
	let keyring_locked = Arc::new(AtomicBool::new(true));
	let mut options = ServiceOptions::new(program, network(free_port(), ([0; 32], free_port())));
	options.look = Duration::from_millis(100);
	options.locked = {
		let keyring_locked = Arc::clone(&keyring_locked);
		Arc::new(move |service, _| {
			assert_eq!(service, KEYSTORE_SERVICE);
			keyring_locked.load(Ordering::SeqCst)
		})
	};
	let _service = Service::start(&settings, options);
	wait_until("it waits for the keyring", || {
		status_of(&settings, &served.directory) == Some(Serving::WaitingForKeyring)
	});
	// Each on a thread of its own, which says what it does once it opened its library.
	wait_until(
		"the other waits for the keyring, its key in the keystore too",
		|| status_of(&settings, &elsewhere.directory) == Some(Serving::WaitingForKeyring),
	);
	wait_until("a library that a passphrase protects is turned off", || {
		AtLogin::read(&settings)
			.expect("reads")
			.find(&protected.directory)
			.is_none()
	});
	wait_until("a library that is gone is not here", || {
		status_of(&settings, &gone.directory) == Some(Serving::NotHere)
	});
	assert_eq!(
		holder(&open_served(&served)),
		None,
		"nothing asked of a locked keyring"
	);

	keyring_locked.store(false, Ordering::SeqCst);
	wait_until("it serves the library once the keyring is unlocked", || {
		status_of(&settings, &served.directory) == Some(Serving::Syncing)
	});
	wait_until("it finds the other replica not here", || {
		status_of(&settings, &elsewhere.directory) == Some(Serving::NotHere)
	});
}

#[test]
fn a_copy_that_the_list_does_not_name_gives_way_to_the_one_it_names() {
	let mut rng = TestRng::new("gives way");
	let (directory_of, library) = keystore_library("gives way library", &mut rng);
	let served = Served {
		replica: replica_id(&library),
		directory: directory_of,
	};
	drop(library);
	let settings = directory("gives way settings");
	let (older, newer_copy) = (settings.join("editor"), settings.join("passwords"));
	AtLogin::edit(&settings, |list| {
		list.program = Some(older.clone());
		list.add(served.clone());
	})
	.expect("lists");
	let options = |program: &Path| {
		let mut options = ServiceOptions::new(
			program.to_owned(),
			network(free_port(), ([0; 32], free_port())),
		);
		options.look = Duration::from_millis(100);
		options
	};
	let first = Service::start(&settings, options(&older));
	wait_until("the first serves", || {
		holder(&open_served(&served)).as_deref() == Some(SERVICE_PROGRAM)
	});
	// A copy that the list does not name ends at once.
	assert_eq!(
		Service::start(&settings, options(&newer_copy)).end(),
		ServiceEnded::NamesAnother
	);
	assert!(!first.ended());

	// Once the list names the newer copy, it waits for the first, which ends.
	AtLogin::edit(&settings, |list| list.program = Some(newer_copy.clone())).expect("names");
	let second = Service::start(&settings, options(&newer_copy));
	assert_eq!(first.end(), ServiceEnded::NamesAnother);
	wait_until("the second takes over", || {
		status_of(&settings, &served.directory) == Some(Serving::Syncing)
			&& holder(&open_served(&served)).as_deref() == Some(SERVICE_PROGRAM)
	});
	assert!(!second.ended());
	assert!(service_running(&settings).expect("looks"));
}

#[test]
fn the_autostart_entry_starts_the_listed_copy_and_keeps_the_persons_off() {
	let folder = directory("the autostart entry");
	let file = folder.join("autostart").join("nibsync.desktop");
	let entry = LoginEntry::autostart(file.clone());
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);

	// A path with what the format reserves comes back as it was.
	let program = PathBuf::from("/home/ada/My Apps/$HOME \"quoted\" 100%/back\\slash/nibsync");
	assert_eq!(
		entry.make(&program).expect("makes"),
		EntryState::On(program.clone())
	);
	let text = std::fs::read_to_string(&file).expect("reads");
	assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
	assert!(text.contains("Name=NibKeep Sync\n"), "{text}");

	// The person turns it off, as a desktop's settings do; a newer copy keeps it off.
	std::fs::write(
		&file,
		format!("{text}Hidden=true\n[Other]\nExec=elsewhere\n"),
	)
	.expect("writes");
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::Off(program.clone())
	);
	let newer = PathBuf::from("/opt/nibkeep/nibsync");
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::Off(newer.clone())
	);
	let text = std::fs::read_to_string(&file).expect("reads");
	assert!(
		text.contains("[Other]\nExec=elsewhere\n"),
		"another group stays: {text}"
	);
	let gnome_off = text.replace("Hidden=true\n", "").replace(
		"X-GNOME-Autostart-enabled=true",
		"X-GNOME-Autostart-enabled=false",
	);
	std::fs::write(&file, &gnome_off).expect("writes");
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::Off(newer.clone())
	);

	// A file that does not read as an entry is made anew; a byte-order mark is no matter.
	std::fs::remove_file(&file).expect("removes");
	std::fs::write(&file, "\u{feff}").expect("writes");
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::On(newer.clone())
	);
	let text = std::fs::read_to_string(&file).expect("reads");
	std::fs::write(&file, format!("\u{feff}{text}")).expect("writes");
	assert_eq!(entry.state().expect("reads"), EntryState::On(newer));

	entry.remove().expect("removes");
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);
	entry.remove().expect("removes what is gone");
}

#[test]
fn an_autostart_entry_through_a_link_or_with_an_exec_that_does_not_read() {
	let folder = directory("the autostart entry's file");
	let file = folder.join("nibsync.desktop");
	let entry = LoginEntry::autostart(file.clone());
	let newer = PathBuf::from("/opt/nibkeep/nibsync");

	// A link that a program that manages dotfiles made stays a link, and its file changes.
	#[cfg(unix)]
	{
		let gnome_off = "[Desktop Entry]\nName=NibKeep Sync\nExec=/opt/nibkeep/nibsync\nX-GNOME-Autostart-enabled=false\n";
		let kept = folder.join("kept.desktop");
		std::fs::write(&kept, gnome_off).expect("writes");
		std::os::unix::fs::symlink(&kept, &file).expect("links");
		assert_eq!(
			entry.make(&newer).expect("makes"),
			EntryState::Off(newer.clone())
		);
		let elsewhere = PathBuf::from("/usr/lib/nibkeep/nibsync");
		assert_eq!(
			entry.point(&elsewhere).expect("points"),
			EntryState::Off(elsewhere.clone())
		);
		assert!(
			std::fs::symlink_metadata(&file)
				.expect("reads")
				.is_symlink()
		);
		assert!(
			std::fs::read_to_string(&kept)
				.expect("reads")
				.contains("/usr/lib/nibkeep")
		);
		entry.make(&newer).expect("makes");
	}

	// One whose Exec does not read keeps the person's off, and gets the program.
	std::fs::write(
		&file,
		"[Desktop Entry]\nName=NibKeep Sync\nHidden=true\nExec=\"broken\\\n",
	)
	.expect("writes");
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::Off(PathBuf::new())
	);
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::Off(newer.clone())
	);
	std::fs::write(&file, "[Desktop Entry]\nName=NibKeep Sync\nHidden=true\n").expect("writes");
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::Off(newer.clone())
	);
	assert!(
		std::fs::read_to_string(&file)
			.expect("reads")
			.starts_with("[Desktop Entry]\nExec=")
	);
}

/// Removes the tests' keys from the registry as it goes, even when a test fails.
#[cfg(windows)]
struct TestKeys;

#[cfg(windows)]
impl Drop for TestKeys {
	fn drop(&mut self) {
		let user = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
		let _ = user.delete_subkey_all(r"Software\NibKeep\Tests");
		// Only if nothing else of NibKeep's is there.
		let _ = user.delete_subkey(r"Software\NibKeep");
	}
}

#[cfg(windows)]
#[test]
fn the_run_key_starts_the_listed_copy_and_keeps_the_persons_off() {
	use winreg::RegKey;
	use winreg::enums::HKEY_CURRENT_USER;
	let _keys = TestKeys;
	let tests = format!(r"Software\NibKeep\Tests\{}", std::process::id());
	let (key, approved) = (format!(r"{tests}\Run"), format!(r"{tests}\Approved"));
	let entry = LoginEntry::run_key(&key, &approved, "NibKeep Sync 0123456789abcdef");
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);
	let program = PathBuf::from(r"C:\Program Files\NibKeep\nibsync.exe");
	assert_eq!(
		entry.make(&program).expect("makes"),
		EntryState::On(program.clone())
	);
	// Windows keeps the person's off apart, under the same name.
	let user = RegKey::predef(HKEY_CURRENT_USER);
	let (marks, _) = user.create_subkey(&approved).expect("makes");
	marks
		.set_raw_value(
			"NibKeep Sync 0123456789abcdef",
			&winreg::RegValue {
				vtype: winreg::enums::RegType::REG_BINARY,
				bytes: vec![3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8].into(),
			},
		)
		.expect("writes");
	assert_eq!(entry.state().expect("reads"), EntryState::Off(program));
	let newer = PathBuf::from(r"C:\Users\Ada\AppData\Local\NibKeep\nibsync.exe");
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::Off(newer.clone())
	);
	entry.remove().expect("removes");
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);
	assert_eq!(
		entry.make(&newer).expect("makes"),
		EntryState::On(newer.clone()),
		"made anew, on"
	);
	// Made anew where only Windows' record of an earlier one turned off was left.
	entry.remove().expect("removes");
	let (marks, _) = user.create_subkey(&approved).expect("makes");
	marks
		.set_raw_value(
			"NibKeep Sync 0123456789abcdef",
			&winreg::RegValue {
				vtype: winreg::enums::RegType::REG_BINARY,
				bytes: vec![3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8].into(),
			},
		)
		.expect("writes");
	assert_eq!(
		entry.point(&newer).expect("points"),
		EntryState::Missing,
		"never made"
	);
	assert_eq!(entry.make(&newer).expect("makes"), EntryState::On(newer));
	entry.remove().expect("removes");
}

#[test]
fn turning_on_makes_the_login_entry_and_turning_off_the_last_removes_it() {
	let mut rng = TestRng::new("the entry");
	let mut served = Vec::new();
	for name in ["the entry first", "the entry second"] {
		let (library_directory, library) = keystore_library(name, &mut rng);
		served.push(Served {
			replica: replica_id(&library),
			directory: library_directory,
		});
	}
	let settings = directory("the entry settings");
	let entry = LoginEntry::autostart(settings.join("nibsync.desktop"));
	let own = settings.join("nibsync");
	std::fs::write(&own, "").expect("writes");
	let same = |_: &Path| Some(ENGINE_VERSION.to_owned());
	for library in &served {
		let turned_on =
			turn_on(&settings, library.clone(), (&own, &same), Some(&entry)).expect("turns on");
		assert_eq!(turned_on.entry, EntryOutcome::On(own.clone()));
	}
	let off = |directory: &Path, replica| {
		turn_off(&settings, (directory, replica), Some(&entry)).expect("turns off")
	};
	assert!(off(&served[0].directory, None).removed);
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::On(own.clone()),
		"one is left"
	);
	// By its replica, as after its folder was moved.
	let moved = served[1].directory.with_file_name("elsewhere");
	assert!(off(&moved, Some(served[1].replica)).removed);
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::Missing,
		"none is left"
	);

	// An entry left by a turning off that failed to remove it goes with the next.
	entry.make(&own).expect("makes");
	let turned_off = off(&served[0].directory, None);
	assert_eq!((turned_off.removed, turned_off.entry_failed), (false, None));
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);

	// Naming a newer copy points the entry along, and never makes one that is gone.
	let newer = settings.join("newer").join("nibsync");
	std::fs::create_dir_all(newer.parent().expect("a folder")).expect("makes");
	std::fs::write(&newer, "").expect("writes");
	let newest = |copy: &Path| Some(if copy == newer { "999.0.0" } else { "0.0.1" }.to_owned());
	assert!(name_own_copy(&settings, &newer, &newest, Some(&entry)).expect("names"));
	assert_eq!(entry.state().expect("reads"), EntryState::Missing);
	entry.make(&newer).expect("makes");
	assert!(!name_own_copy(&settings, &own, &newest, Some(&entry)).expect("names"));
	assert_eq!(entry.state().expect("reads"), EntryState::On(newer.clone()));

	// One that cannot be made leaves the library on, and says why.
	let blocked = LoginEntry::autostart(own.join("cannot").join("nibsync.desktop"));
	let turned_on =
		turn_on(&settings, served[0].clone(), (&own, &same), Some(&blocked)).expect("turns on");
	assert!(
		matches!(turned_on.entry, EntryOutcome::Failed(_)),
		"{turned_on:?}"
	);
	assert!(
		AtLogin::read(&settings)
			.expect("reads")
			.find(&served[0].directory)
			.is_some()
	);
}

#[test]
fn the_service_takes_off_a_library_that_a_passphrase_protects_now() {
	let mut rng = TestRng::new("protected anew");
	let passphrase_directory = directory("protected anew library");
	let protection = NewProtection::Passphrase(
		"correct horse",
		PassphraseParams::new(64, 1, 1).expect("valid"),
	);
	let library = Library::create(
		&passphrase_directory,
		&NewLibrary::new("Ada", "laptop", "nib"),
		&protection,
		now(),
		&mut rng,
	)
	.expect("creates");
	let served = Served {
		replica: replica_id(&library),
		directory: passphrase_directory,
	};
	drop(library);
	let settings = directory("protected anew settings");
	let program = settings.join("nibsync");
	let entry = LoginEntry::autostart(settings.join("nibsync.desktop"));
	entry.make(&program).expect("makes");
	AtLogin::edit(&settings, |list| {
		list.program = Some(program.clone());
		list.add(served);
	})
	.expect("lists");
	let mut options = ServiceOptions::new(program, network(free_port(), ([0; 32], free_port())));
	options.look = Duration::from_millis(100);
	options.entry = Some(entry.clone());
	assert_eq!(
		Service::start(&settings, options).end(),
		ServiceEnded::Empty
	);
	assert_eq!(
		AtLogin::read(&settings).expect("reads").libraries,
		Vec::<Served>::new()
	);
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::Missing,
		"with the last library"
	);
}

#[test]
fn an_app_that_starts_names_its_copy_and_starts_the_one_that_should_run() {
	let mut rng = TestRng::new("an app starts");
	let (library_directory, library) = keystore_library("an app starts library", &mut rng);
	assert_eq!(
		library.close().library_directory(),
		Some(library_directory.as_path()),
		"the directory that opens it"
	);
	let library = open_served(&Served {
		replica: EntityId::random(&mut rng),
		directory: library_directory.clone(),
	});
	let served = Served {
		replica: replica_id(&library),
		directory: library_directory,
	};
	drop(library);
	let settings = directory("an app starts settings");
	let entry = LoginEntry::autostart(settings.join("nibsync.desktop"));
	let (older, own) = (settings.join("older"), settings.join("own"));
	for copy in [&older, &own] {
		std::fs::write(copy, "").expect("writes");
	}
	let versions = {
		let own = own.clone();
		move |copy: &Path| Some(if copy == own { "0.5.0" } else { "0.1.0" }.to_owned())
	};
	let started = |entry: Option<&LoginEntry>| {
		nibkeep_node::at_login::app_started(&settings, &own, &versions, entry).expect("starts")
	};
	assert_eq!(started(Some(&entry)), None, "no list, nothing to start");
	let same = |_: &Path| Some("0.1.0".to_owned());
	turn_on(&settings, served.clone(), (&older, &same), Some(&entry)).expect("turns on");
	assert_eq!(
		started(Some(&entry)),
		Some(own.clone()),
		"its newer copy, named"
	);
	assert_eq!(
		entry.state().expect("reads"),
		EntryState::On(own.clone()),
		"pointed along"
	);
	assert_eq!(
		started(Some(&entry)),
		Some(own.clone()),
		"none runs: the listed one"
	);
	assert_eq!(
		started(None),
		Some(own.clone()),
		"where there is no entry, as on macOS"
	);

	// The person turned the entry off, or removed it: an app starts nothing.
	let file = settings.join("nibsync.desktop");
	let text = std::fs::read_to_string(&file).expect("reads");
	std::fs::write(&file, format!("{text}Hidden=true\n")).expect("writes");
	assert_eq!(started(Some(&entry)), None);
	entry.remove().expect("removes");
	assert_eq!(started(Some(&entry)), None);

	// What a program shows.
	let shown = nibkeep_node::at_login::status(&settings, (&served.directory, None), Some(&entry));
	assert!(shown.on);
	assert_eq!(shown.serving, None, "none runs");
	assert_eq!(shown.entry, Some(Ok(EntryState::Missing)));
	assert_eq!(shown.listed, Some(own.clone()));
	entry.make(&older).expect("makes");
	let shown = nibkeep_node::at_login::status(&settings, (&served.directory, None), Some(&entry));
	assert!(
		shown.entry_stale(),
		"it starts another copy than the listed one"
	);
	let other =
		nibkeep_node::at_login::status(&settings, (&settings.join("elsewhere"), None), None);
	assert!(!other.on);
	assert_eq!(
		other.entry, None,
		"of a library that is off, nothing more is read"
	);
	// Named another way, as through a link, it is found by its replica.
	let linked = settings.join("linked").join(".nib");
	let found =
		nibkeep_node::at_login::status(&settings, (&linked, Some(served.replica)), Some(&entry));
	assert!(found.on);
	assert_eq!(found.entry_word(), Some("stale"));
	let file = settings.join("nibsync.desktop");
	let text = std::fs::read_to_string(&file).expect("reads");
	std::fs::write(&file, format!("{text}Hidden=true\n")).expect("writes");
	let found =
		nibkeep_node::at_login::status(&settings, (&linked, Some(served.replica)), Some(&entry));
	assert_eq!(
		found.entry_word(),
		Some("off"),
		"the person's off before a stale program"
	);
}
