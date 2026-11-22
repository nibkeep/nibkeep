//! Libraries of several rounds: what each round adds, that they are sound, and that a new
//! replica receives them whole.

use nibkeep_generate::{BOOK, PASSPHRASE, at, generate, passphrase_params};
use nibkeep_node::{Joining, Library, NewProtection, Unlocker};
use nibkeep_testkit::TestRng;

#[test]
fn every_round_adds_the_same_mix_and_the_library_stays_sound() {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "three rounds")
		.join("library");
	drop(generate(&directory, 3));
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	let personal = library.personal_space();
	let status = library.status().expect("reads the status");
	// The user, the person, the device, the replica and the space, per round an author,
	// four books that are not deleted, and a login, and in the first round a book whose
	// note was purged.
	assert_eq!(status.spaces[0].entities, 5 + 3 * 6 + 1);
	let books = library.entities(personal, Some(&BOOK)).expect("a space");
	assert_eq!(books.len(), 3 * 4 + 1);
	// Each round's book has two competing titles, which differ in case only.
	for title in ["Leviathan wakes", "Leviathan wakes 1", "Leviathan wakes 2"] {
		let found = library.search(personal, title).expect("searches");
		assert!(
			found.iter().any(|listed| listed
				.name
				.as_deref()
				.is_some_and(|name| name.eq_ignore_ascii_case(title))),
			"{title}"
		);
	}
	let report = library.verify(at(3, 0)).expect("verifies");
	assert!(report.spaces.iter().all(|space| space.problems.is_empty()));
}

#[test]
fn a_new_replica_receives_a_generated_library_whole_in_its_first_sync() {
	let root = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "received whole");
	let mut library = generate(&root.join("library"), 3);
	let mut rng = TestRng::new("the replica that receives");
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let protection = NewProtection::Passphrase(PASSPHRASE, passphrase_params());
	let directory = root.join("received");
	let received = library
		.pair_in_process(&directory, joining, &protection, &|| at(3, 1), &mut rng)
		.expect("pairs");
	drop(received);
	// What came in batches is on disk: a purge, a blob, secrets, types and languages.
	let mut received =
		Library::open(&directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the replica");
	let personal = library.personal_space();
	let listed = |library: &Library| {
		let mut listed = library.entities(personal, None).expect("a space");
		listed.sort_by_key(|listed| listed.id);
		listed
	};
	assert_eq!(listed(&received), listed(&library));
	for query in ["Leviathan wakes 2", "Grace", "Урсула"] {
		assert_eq!(
			received.search(personal, query).expect("searches"),
			library.search(personal, query).expect("searches"),
			"{query}"
		);
	}
	assert!(
		received
			.search(personal, "Grace")
			.expect("searches")
			.is_empty(),
		"the purged note"
	);
	let report = received.verify(at(3, 1)).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
	let kept = library.verify(at(3, 1)).expect("verifies");
	assert_eq!(report.spaces[0].blobs, kept.spaces[0].blobs);
}
