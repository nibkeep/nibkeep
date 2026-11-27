//! What a sync receives is saved in batches ([technical design: saving in batches]): a new
//! replica gets everything in its first sync, and a sync that is cut short keeps what came
//! before the cut, on disk.
//!
//! [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches

use crate::common;

use std::path::Path;
use std::sync::mpsc;

use crate::common::in_process::Pipe;
use crate::common::{now, protection};
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{Name, Value};
use nibkeep_node::{Joining, Library, Syncing, Unlocker};
use nibkeep_sync::{Transport, TransportError};
use nibkeep_testkit::TestRng;

/// One end of a connection in memory that sends only so many messages, and then closes.
struct Cut {
	pipe: Pipe,
	budget: usize,
}

impl Transport for Cut {
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
		if self.budget == 0 {
			// The other end sees the connection close.
			let (closed, _) = mpsc::channel();
			self.pipe.to = closed;
			return Err(TransportError("cut".to_owned()));
		}
		self.budget -= 1;
		self.pipe.send(message)
	}

	fn receive(&mut self) -> Result<Vec<u8>, TransportError> {
		self.pipe.receive()
	}

	fn has_message(&mut self) -> bool {
		self.pipe.has_message()
	}

	fn binding(&self) -> [u8; 32] {
		self.pipe.binding()
	}

	fn own_endpoint(&self) -> [u8; 32] {
		self.pipe.own_endpoint()
	}

	fn peer_endpoint(&self) -> [u8; 32] {
		self.pipe.peer_endpoint()
	}
}

/// Adds `count` people to the personal space, in batches.
fn add_people(library: &mut Library, count: usize, rng: &mut TestRng) {
	let personal = library.personal_space();
	for start in (0..count).step_by(100) {
		library
			.batch(personal, |library| {
				for number in start..count.min(start + 100) {
					let name = Value::Name(Name::new(format!("person {number}")));
					library.create_entity(personal, &[PERSON], vec![(NAME, name)], now(), rng)?;
				}
				Ok(())
			})
			.expect("adds people");
	}
}

/// How many entities the library's personal space shows.
fn entities(library: &Library) -> usize {
	library.status().expect("reads the status").spaces[0].entities
}

fn reopen(directory: &Path) -> Library {
	Library::open(directory, &Unlocker::Passphrase("battery staple")).expect("opens again")
}

#[test]
fn a_sync_cut_short_keeps_on_disk_what_came_before_the_cut() {
	let mut rng = TestRng::new("receiving cut short");
	let (directory, mut laptop) = common::library("receiving cut short", &mut rng);
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let phone_directory = directory.join("phone");
	let mut phone = laptop
		.pair_in_process(&phone_directory, joining, &protection(), &now, &mut rng)
		.expect("pairs");
	let before = entities(&phone);
	add_people(&mut laptop, 300, &mut rng);

	// The phone connects; the laptop's end closes after some of the people it sends.
	let (to_laptop, from_phone) = mpsc::channel();
	let (to_phone, from_laptop) = mpsc::channel();
	let mut phone_end = Pipe {
		to: to_laptop,
		from: from_laptop,
		early: None,
		own: phone.endpoint(),
		peer: laptop.endpoint(),
	};
	let mut laptop_end = Cut {
		pipe: Pipe {
			to: to_phone,
			from: from_phone,
			early: None,
			own: laptop.endpoint(),
			peer: phone.endpoint(),
		},
		budget: 150,
	};
	let (initiated, responded) = std::thread::scope(|scope| {
		let responded = scope.spawn(|| {
			let synced =
				nibkeep_sync::respond(&mut laptop_end, &mut Syncing::new(&mut laptop, &now));
			drop(laptop_end);
			synced
		});
		let initiated = nibkeep_sync::initiate(&mut phone_end, &mut Syncing::new(&mut phone, &now));
		drop(phone_end);
		(initiated, responded.join().expect("the laptop ran"))
	});
	assert!(initiated.is_err() && responded.is_err(), "the sync was cut");
	let kept = entities(&phone);
	assert!(
		before < kept && kept < before + 300,
		"{kept} entities, {before} before the sync"
	);
	drop(phone);
	let mut phone = reopen(&phone_directory);
	assert_eq!(entities(&phone), kept, "what came is on disk");
	let report = phone.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
}

#[test]
fn a_new_replica_gets_every_change_in_its_first_sync() {
	let mut rng = TestRng::new("receiving everything");
	let (directory, mut laptop) = common::library("receiving everything", &mut rng);
	// More than one batch holds.
	add_people(&mut laptop, 1_500, &mut rng);
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let phone_directory = directory.join("phone");
	let phone = laptop
		.pair_in_process(&phone_directory, joining, &protection(), &now, &mut rng)
		.expect("pairs");
	let personal = laptop.personal_space();
	let listed = |library: &Library| {
		let mut listed = library.entities(personal, None).expect("a space");
		listed.sort_by_key(|listed| listed.id);
		listed
	};
	assert_eq!(listed(&phone), listed(&laptop));
	drop(phone);
	let mut phone = reopen(&phone_directory);
	assert_eq!(listed(&phone), listed(&laptop));
	let report = phone.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
}
