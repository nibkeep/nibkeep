//! S7, a lost phone, with libraries on this computer standing in for the devices: from
//! another device, the user revokes it. It can no longer sync, nor read what is added
//! later; what it changed that the revoking device lacked is set aside for review; and
//! nothing is lost ([technical design: revocation]).
//!
//! [technical design: revocation]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#11-revocation-and-new-keys

mod common;

use std::path::PathBuf;

use common::{
	LOGIN, library, login_type, now, pair_joining, pair_named, passwords, sync_in_memory, this_day,
	today,
};
use nibkeep_base::EntityId;
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{FieldName, Name, TypeDef, TypeName, Value, fields};
use nibkeep_node::{
	Edit, Joining, Library, NewProtection, NodeError, PairingCode, SyncError, Synced,
};
use nibkeep_std::CONTENT;
use nibkeep_testkit::TestRng;

/// Devices of one user: a laptop that started the library, and others paired with it.
struct Devices {
	seed: String,
	rng: TestRng,
	libraries: Vec<Library>,
	directories: Vec<PathBuf>,
	book: TypeName,
	at: u64,
}

const LAPTOP: usize = 0;
const DESK: usize = 1;
const PHONE: usize = 2;
const TABLET: usize = 3;

impl Devices {
	/// A laptop with a type of books, and the devices `names` paired with it, each synced
	/// with it once.
	fn new(seed: &str, names: &[&str]) -> Self {
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
			seed: seed.to_owned(),
			rng,
			libraries: vec![laptop],
			directories: vec![directory],
			book,
			at: now() + 100_000,
		};
		for name in names {
			devices.pair(name);
		}
		devices
	}

	/// Pairs a new device with the laptop.
	fn pair(&mut self, name: &str) -> usize {
		self.pair_with(LAPTOP, name)
	}

	/// Pairs a new device with `sponsor`.
	fn pair_with(&mut self, sponsor: usize, name: &str) -> usize {
		let directory = self.new_directory(name);
		let code = PairingCode::generate(&mut self.rng);
		let (paired, joined) = pair_named(
			&mut self.libraries[sponsor],
			&directory,
			name,
			(&code, &code),
			&mut self.rng,
		);
		self.joined(directory, paired, joined)
	}

	/// Pairs a new device with the laptop, which gives it the secrets that it asks for, kept
	/// under a passphrase of its own.
	fn pair_with_secrets(&mut self, name: &str) -> usize {
		let directory = self.new_directory(name);
		let code = PairingCode::generate(&mut self.rng);
		let joining = Joining::new(name, "nib", &mut self.rng)
			.expect("names")
			.asking_for_secrets(
				NewProtection::Passphrase(
					"secrets here",
					PassphraseParams::new(64, 1, 1).expect("valid"),
				),
				&mut self.rng,
			);
		let (paired, joined) = pair_joining(
			&mut self.libraries[LAPTOP],
			&directory,
			(joining, true),
			(&code, &code),
			&mut self.rng,
		);
		self.joined(directory, paired, joined)
	}

	/// A folder for a new device's library, which pairing makes.
	fn new_directory(&self, name: &str) -> PathBuf {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("{} {name} {}", self.seed, self.directories.len()),
		);
		std::fs::remove_dir(&directory).expect("removes");
		directory
	}

	/// Keeps the device that joined.
	fn joined(
		&mut self,
		directory: PathBuf,
		paired: Result<nibkeep_node::Paired, NodeError>,
		joined: Result<nibkeep_node::Joined, NodeError>,
	) -> usize {
		let joined = joined.expect("joined");
		paired.expect("paired");
		self.libraries.push(joined.library);
		self.directories.push(directory);
		self.libraries.len() - 1
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	/// Adds a book with a name on a device.
	fn add(&mut self, device: usize, name: &str) {
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
			.expect("adds");
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

	/// Syncs two devices in memory, as `nib sync` does over the network: `a` connects to
	/// `b`; then each settles its keys. Returns what each side's sync gave.
	fn sync(
		&mut self,
		a: usize,
		b: usize,
	) -> (Result<Synced, NodeError>, Result<Synced, NodeError>) {
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

	/// Syncs two devices, which both must manage.
	fn synced(&mut self, a: usize, b: usize) {
		let (first, second) = self.sync(a, b);
		first.expect("the first synced");
		second.expect("the second synced");
	}

	/// Revokes a device from another, by its name.
	fn revoke(&mut self, from: usize, name: &str) {
		let at = self.tick();
		let library = &mut self.libraries[from];
		let replicas: Vec<_> = library
			.to_revoke(name)
			.expect("names a device")
			.into_iter()
			.map(|replica| replica.id)
			.collect();
		assert_eq!(replicas.len(), 1);
		library
			.revoke(&replicas, at, &mut self.rng)
			.expect("revokes");
	}

	/// The names of the devices that a device counts as revoked, sorted.
	fn revoked(&self, device: usize) -> Vec<String> {
		let mut names: Vec<String> = self.libraries[device]
			.replicas()
			.expect("lists")
			.into_iter()
			.filter(|replica| replica.revoked)
			.filter_map(|replica| replica.device_name)
			.collect();
		names.sort();
		names
	}

	/// The numbers of the personal space's keys that a device holds, and the one it writes
	/// with.
	fn keys(&self, device: usize) -> (Vec<u64>, u64) {
		let library = &self.libraries[device];
		library
			.space_key_numbers(library.personal_space())
			.expect("a space")
	}
}

#[test]
fn s7_a_lost_phone_is_revoked_and_nothing_is_lost() {
	let mut devices = Devices::new("s7", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	devices.add(PHONE, "Seen by all");
	devices.synced(PHONE, LAPTOP);
	devices.synced(DESK, LAPTOP);
	// The phone adds a book that only the desk gets, and is lost.
	devices.add(PHONE, "Seen by the desk alone");
	devices.synced(PHONE, DESK);
	assert_eq!(
		devices.books(DESK),
		["Seen by all", "Seen by the desk alone"]
	);
	let (before, _) = devices.keys(LAPTOP);

	// The laptop revokes it, and has a new key, which it writes with.
	devices.revoke(LAPTOP, "phone");
	let (after, writing) = devices.keys(LAPTOP);
	assert_eq!(after.len(), before.len() + 1, "{after:?}");
	assert!(!before.contains(&writing));
	let listed = devices.libraries[LAPTOP].replicas().expect("lists");
	assert!(
		listed
			.iter()
			.any(|replica| replica.revoked && replica.device_name.as_deref() == Some("phone"))
	);
	assert_eq!(devices.libraries[LAPTOP].peers().expect("lists").len(), 1);

	// The desk hears of it from the laptop: it takes the new key, and sets aside what the
	// laptop did not have.
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.keys(DESK).1, writing);
	assert_eq!(devices.books(DESK), ["Seen by all"]);
	assert_eq!(devices.books(LAPTOP), ["Seen by all"]);
	for device in [LAPTOP, DESK] {
		let set_aside = devices.libraries[device].set_aside().expect("lists");
		assert_eq!(set_aside.len(), 1, "on {device}: {set_aside:?}");
		assert!(set_aside[0].operations.is_some(), "{set_aside:?}");
	}

	// Neither syncs with the phone any more, whoever connects.
	for device in [LAPTOP, DESK] {
		let (phone, other) = devices.sync(PHONE, device);
		assert!(
			matches!(other, Err(NodeError::Sync(SyncError::NotAReplica))),
			"{other:?}"
		);
		assert!(phone.is_err(), "{phone:?}");
		let (other, phone) = devices.sync(device, PHONE);
		assert!(other.is_err() || phone.is_err());
	}
	// The phone never got the new key, and what is added now is wrapped with it.
	assert!(!devices.keys(PHONE).0.contains(&writing));
	devices.add(DESK, "After the phone was lost");
	devices.synced(DESK, LAPTOP);
	assert_eq!(
		devices.books(LAPTOP),
		["After the phone was lost", "Seen by all"]
	);

	// The user reviews what was set aside, and accepts it: nothing was lost.
	let accepted: Vec<_> = devices.libraries[LAPTOP]
		.set_aside()
		.expect("lists")
		.into_iter()
		.map(|aside| aside.change)
		.collect();
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	assert_eq!(
		laptop
			.accept_set_aside(&accepted, at, &mut TestRng::new("accept"))
			.expect("accepts"),
		1
	);
	devices.synced(DESK, LAPTOP);
	for device in [LAPTOP, DESK] {
		assert_eq!(
			devices.books(device),
			[
				"After the phone was lost",
				"Seen by all",
				"Seen by the desk alone"
			],
			"on {device}"
		);
		assert_eq!(
			devices.libraries[device].set_aside().expect("lists"),
			Vec::new()
		);
	}

	// It stays so once the libraries are opened again.
	let directory = devices.directories[DESK].clone();
	let desk = devices.libraries.remove(DESK);
	drop(desk);
	let desk = Library::open(
		&directory,
		&nibkeep_node::Unlocker::Passphrase("battery staple"),
	)
	.expect("opens");
	devices.libraries.insert(DESK, desk);
	assert_eq!(devices.keys(DESK).1, writing);
	assert_eq!(devices.books(DESK).len(), 3);
}

#[test]
fn a_revoked_phones_purge_is_undone_where_it_arrived_first() {
	let mut devices = Devices::new("undone purge", &["desk", "phone"]);
	devices.add(LAPTOP, "Keep me");
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// The phone purges the book's name, and only the desk gets the purge.
	let at = devices.tick();
	let phone = &mut devices.libraries[PHONE];
	let personal = phone.personal_space();
	let book = phone
		.entities(personal, Some(&devices.book))
		.expect("lists")[0]
		.id;
	phone
		.purge(personal, book, &fields::NAME, None, at, &mut devices.rng)
		.expect("purges");
	devices.synced(PHONE, DESK);
	assert_eq!(devices.books(DESK), Vec::<String>::new());
	// The laptop revokes the phone without having the purge; the desk hears of it, and
	// gets the name back from the laptop.
	devices.revoke(LAPTOP, "phone");
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.books(DESK), ["Keep me"]);
	assert_eq!(devices.books(LAPTOP), ["Keep me"]);
	let report = devices.libraries[DESK]
		.verify(devices.at + 1_000)
		.expect("verifies");
	assert!(report.spaces[0].problems.is_empty(), "{report:?}");
}

/// The content of a file, as a device reads it from its own copy, if it has one.
fn content_on(devices: &Devices, device: usize, file: EntityId) -> Option<Vec<u8>> {
	let library = &devices.libraries[device];
	let personal = library.personal_space();
	let view = library
		.entity(personal, file, this_day())
		.expect("reads")
		.expect("the file");
	let Value::Blob(content) = view.values(&CONTENT).first()?.value else {
		panic!("a blob");
	};
	library
		.file_content(personal, &content, 1 << 20)
		.expect("reads")
}

#[test]
fn a_revoked_phones_purge_of_a_files_content_is_undone_where_it_arrived_first() {
	let mut devices = Devices::new("undone content purge", &["desk", "phone"]);
	let files =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "undone content purge files");
	let notes = files.join("notes.txt");
	std::fs::write(&notes, "Milk, eggs, bread.\n").expect("writes");
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	let personal = laptop.personal_space();
	let file = laptop
		.add_file(personal, &notes, at, &mut devices.rng)
		.expect("adds")
		.file;
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// Only the laptop has the original, which the others never read.
	std::fs::remove_file(&notes).expect("removes");
	for device in [LAPTOP, DESK, PHONE] {
		assert_eq!(
			content_on(&devices, device, file).as_deref(),
			Some(&b"Milk, eggs, bread.\n"[..]),
			"on {device}"
		);
	}
	// The phone purges the file's content, and only the desk gets the purge.
	let at = devices.tick();
	let phone = &mut devices.libraries[PHONE];
	phone
		.purge(personal, file, &CONTENT, None, at, &mut devices.rng)
		.expect("purges");
	devices.synced(PHONE, DESK);
	assert_eq!(content_on(&devices, DESK, file), None);
	// The laptop revokes the phone without having the purge; the desk hears of it, and
	// gets the content back from the laptop.
	devices.revoke(LAPTOP, "phone");
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	for device in [LAPTOP, DESK] {
		assert_eq!(
			content_on(&devices, device, file).as_deref(),
			Some(&b"Milk, eggs, bread.\n"[..]),
			"on {device}"
		);
		let report = devices.libraries[device]
			.verify(devices.at + 1_000)
			.expect("verifies");
		assert!(report.spaces[0].problems.is_empty(), "{report:?}");
		assert_eq!(report.spaces[0].blobs, 1, "{report:?}");
	}
}

/// On the laptop, logins whose passwords are secret, its secrets set up, and the login to a
/// bank with its password; returns the login and the field of passwords.
fn with_a_login(devices: &mut Devices) -> (EntityId, FieldName) {
	let password: FieldName = "local.password".parse().expect("a field name");
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	let personal = laptop.personal_space();
	laptop
		.define_type(personal, &login_type(), at, &mut devices.rng)
		.expect("defines");
	laptop
		.set_up_secrets(
			&NewProtection::Passphrase(
				"tr0ub4dor",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			at,
			&mut devices.rng,
		)
		.expect("sets up");
	let bank = laptop
		.create_entity(
			personal,
			&[LOGIN],
			vec![
				(
					"local.title".parse().expect("a field name"),
					Value::Text("Bank".to_owned()),
				),
				(password.clone(), Value::Text("hunter2".to_owned())),
			],
			at,
			&mut devices.rng,
		)
		.expect("adds");
	(bank, password)
}

#[test]
fn a_revoked_phones_purge_of_a_secret_value_is_undone_where_it_arrived_first() {
	let mut devices = Devices::new("undone secret purge", &[]);
	let (bank, password) = with_a_login(&mut devices);
	let personal = devices.libraries[LAPTOP].personal_space();
	assert_eq!(devices.pair_with_secrets("desk"), DESK);
	assert_eq!(devices.pair_with_secrets("phone"), PHONE);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	let passwords = |devices: &Devices, device: usize| passwords(&devices.libraries[device], bank);
	for device in [LAPTOP, DESK, PHONE] {
		assert_eq!(
			passwords(&devices, device),
			[Value::Text("hunter2".to_owned())],
			"on {device}"
		);
	}
	// The phone purges the password, and only the desk gets the purge.
	let at = devices.tick();
	let phone = &mut devices.libraries[PHONE];
	let purge = phone
		.purge(personal, bank, &password, None, at, &mut devices.rng)
		.expect("purges");
	assert_eq!(purge.secret, 1, "{purge:?}");
	devices.synced(PHONE, DESK);
	assert_eq!(passwords(&devices, DESK), []);
	// The laptop revokes the phone without having the purge; the desk hears of it, and
	// gets the password back from the laptop.
	devices.revoke(LAPTOP, "phone");
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	for device in [LAPTOP, DESK] {
		assert_eq!(
			passwords(&devices, device),
			[Value::Text("hunter2".to_owned())],
			"on {device}"
		);
		let report = devices.libraries[device]
			.verify(devices.at + 1_000)
			.expect("verifies");
		assert!(report.spaces[0].problems.is_empty(), "{report:?}");
	}
	// And a new password, set on the desk, reaches the laptop.
	let at = devices.tick();
	let desk = &mut devices.libraries[DESK];
	desk.edit_entity(
		personal,
		bank,
		vec![Edit::Set(
			password.clone(),
			vec![Value::Text("correct horse".to_owned())],
		)],
		&today(),
		at,
		&mut devices.rng,
	)
	.expect("sets");
	devices.synced(DESK, LAPTOP);
	assert_eq!(
		passwords(&devices, LAPTOP),
		[Value::Text("correct horse".to_owned())]
	);
}

#[test]
fn of_two_devices_that_revoke_each_other_the_first_stays_also_where_the_other_came_first() {
	let mut devices = Devices::new("revoked back", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// The desk revokes the phone, and the phone, infected, revokes the desk, each before it
	// heard of the other's; the laptop hears the phone's first.
	devices.revoke(DESK, "phone");
	devices.revoke(PHONE, "desk");
	devices.synced(PHONE, LAPTOP);
	assert_eq!(devices.revoked(LAPTOP), ["desk"]);
	// The desk, added before the phone, would stay if it revoked the phone back, which it
	// did: the laptop takes what the desk made, its revocation among it, and the two meet.
	devices.add(DESK, "From the desk");
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.revoked(LAPTOP), ["phone"]);
	assert_eq!(devices.books(LAPTOP), ["From the desk"]);
	// The phone is refused there from then on.
	let (phone, laptop) = devices.sync(PHONE, LAPTOP);
	assert!(
		matches!(laptop, Err(NodeError::Sync(SyncError::NotAReplica))),
		"{laptop:?}"
	);
	assert!(phone.is_err(), "{phone:?}");
	// And the laptop and the desk end up writing with a key that the phone never got.
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	let writing = devices.keys(LAPTOP).1;
	assert_eq!(devices.keys(DESK).1, writing);
	assert!(!devices.keys(PHONE).0.contains(&writing));
}

#[test]
fn a_revoked_device_that_could_revoke_back_is_heard_and_sent_nothing() {
	let mut devices = Devices::new("heard", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// The phone, infected, revokes the desk, added before it, and the laptop hears of it.
	devices.revoke(PHONE, "desk");
	devices.synced(PHONE, LAPTOP);
	devices.add(LAPTOP, "After the revocation");
	// The desk, which knows nothing of it, connects: the laptop takes what it made, set
	// aside, and sends it nothing.
	devices.add(DESK, "From the desk");
	let (desk, laptop) = devices.sync(DESK, LAPTOP);
	let (desk, laptop) = (desk.expect("synced"), laptop.expect("synced"));
	assert!(
		desk.spaces.iter().all(|space| space.received.changes == 0
			&& space.received.keys == 0
			&& space.received.blobs == 0),
		"{desk:?}"
	);
	assert!(
		laptop.spaces.iter().any(|space| space.received.changes > 0),
		"{laptop:?}"
	);
	assert_eq!(devices.books(DESK), ["From the desk"]);
	assert_eq!(devices.books(LAPTOP), ["After the revocation"]);
	assert_eq!(
		devices.libraries[LAPTOP].set_aside().expect("lists").len(),
		1
	);
	// Once the desk revokes the phone back, the two revocations meet, and the desk stays,
	// with what it made; what is set aside is the phone's revocation alone.
	devices.revoke(DESK, "phone");
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.revoked(LAPTOP), ["phone"]);
	for device in [LAPTOP, DESK] {
		assert_eq!(
			devices.books(device),
			["After the revocation", "From the desk"],
			"on {device}"
		);
		let set_aside = devices.libraries[device].set_aside().expect("lists");
		assert!(
			set_aside.len() == 1 && set_aside[0].governs,
			"on {device}: {set_aside:?}"
		);
	}
}

#[test]
fn a_revoked_device_added_after_the_one_that_revoked_it_stays_refused() {
	let mut devices = Devices::new("revoked for good", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// The desk revokes the phone, added after it, and the laptop hears of it; the phone's
	// revocation of the desk, which would not stay, never gets there.
	devices.revoke(DESK, "phone");
	devices.synced(DESK, LAPTOP);
	devices.revoke(PHONE, "desk");
	let (phone, laptop) = devices.sync(PHONE, LAPTOP);
	assert!(
		matches!(laptop, Err(NodeError::Sync(SyncError::NotAReplica))),
		"{laptop:?}"
	);
	assert!(phone.is_err(), "{phone:?}");
	assert_eq!(devices.revoked(LAPTOP), ["phone"]);
}

#[test]
fn two_revocations_at_once_end_in_one_safe_key() {
	let mut devices = Devices::new("two at once", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	// The laptop and the desk each revoke the phone, offline.
	devices.revoke(LAPTOP, "phone");
	devices.revoke(DESK, "phone");
	assert_ne!(devices.keys(LAPTOP).1, devices.keys(DESK).1);
	// Neither key is safe, as each was given to whom the other revoked; so each makes a
	// new one, and the newest of those is safe for both.
	devices.synced(DESK, LAPTOP);
	devices.synced(DESK, LAPTOP);
	let (laptop, writing) = devices.keys(LAPTOP);
	assert_eq!(devices.keys(DESK).1, writing);
	assert_eq!(laptop.len(), 5, "{laptop:?}");
	// And it stays: no more keys are made.
	devices.synced(DESK, LAPTOP);
	assert_eq!(devices.keys(LAPTOP).0.len(), 5);
}

#[test]
fn a_device_paired_while_a_revocation_was_on_its_way_gets_the_new_key() {
	let mut devices = Devices::new("paired meanwhile", &["desk", "phone"]);
	for device in [DESK, PHONE] {
		devices.synced(device, LAPTOP);
	}
	devices.revoke(LAPTOP, "phone");
	// The desk, which has not heard of it, pairs a tablet.
	let tablet = devices.pair_with(DESK, "tablet");
	assert_eq!(tablet, TABLET);
	devices.synced(DESK, LAPTOP);
	devices.synced(TABLET, DESK);
	devices.synced(DESK, LAPTOP);
	devices.synced(TABLET, LAPTOP);
	let writing = devices.keys(LAPTOP).1;
	for device in [DESK, TABLET] {
		assert_eq!(devices.keys(device).1, writing, "on {device}");
	}
	devices.add(TABLET, "From the tablet");
	devices.synced(TABLET, LAPTOP);
	assert_eq!(devices.books(LAPTOP), ["From the tablet"]);
}

#[test]
fn a_device_to_revoke_is_named_clearly() {
	let mut devices = Devices::new("named", &["phone", "phone"]);
	let laptop = &devices.libraries[LAPTOP];
	for (name, why) in [
		("laptop", "this device cannot revoke itself"),
		("nothing", "no device or replica of yours is nothing"),
		("phone", "phone names several devices"),
	] {
		match laptop.to_revoke(name) {
			Err(NodeError::WhichDevice(message)) => {
				assert!(message.starts_with(why), "{message}");
			}
			other => panic!("{name}: {other:?}"),
		}
	}
	// A device's ID, or its first eight characters, names it.
	let phone = laptop.replicas().expect("lists")[1].clone();
	let device = phone.device.expect("a device").to_string();
	for given in [&device[..8], device.as_str()] {
		let named = laptop.to_revoke(given).expect("names one");
		assert_eq!(named.len(), 1);
		assert_eq!(named[0].id, phone.id);
	}
	devices.revoke(LAPTOP, &device);
	// Revoked already: nothing more to revoke.
	assert_eq!(
		devices.libraries[LAPTOP]
			.to_revoke(&device)
			.expect("names it"),
		Vec::new()
	);
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	assert!(matches!(
		laptop.revoke(
			&[laptop.replicas().expect("lists")[0].id],
			at,
			&mut devices.rng
		),
		Err(NodeError::WhichDevice(_))
	));
}
