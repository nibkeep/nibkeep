//! A program that depends on `nibkeep` alone works with a library: the engine is at the
//! crate's root, and the types that its API takes and returns are in the crate's modules.

use std::convert::identity;

use nibkeep::{Library, NewLibrary, NewProtection, PassphraseParams, Unlocker};
use nibkeep_testkit::TestRng;

/// A moment in January 2027, in milliseconds since 1970.
const NOW: u64 = 1_800_000_000_000;

const PASSPHRASE: &str = "correct horse";

#[test]
fn a_program_that_names_nibkeep_alone_makes_a_library_and_opens_it_again() {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "one name");
	let mut rng = TestRng::new("one name");
	let protection =
		NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
	let library = Library::create(
		&directory,
		&NewLibrary::new("Ada", "laptop", "an example"),
		&protection,
		NOW,
		&mut rng,
	)
	.expect("creates");
	let personal: nibkeep::base::SpaceId = library.personal_space();
	// What the engine returns is named through the modules, the standard vocabulary too.
	let types: nibkeep::model::Types = library.types(personal).expect("a space");
	assert!(types.get(&nibkeep::vocabulary::LOGIN).is_some());
	drop(library);

	let again = Library::open(&directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens");
	assert_eq!(again.personal_space(), personal);
}

/// The modules are the crates below the engine themselves, so a value that one program got
/// through `nibkeep` is the value that another got from the crate.
#[test]
fn the_root_is_the_engine_and_the_modules_are_the_crates_below_it() {
	let _: fn(nibkeep_node::Status) -> nibkeep::Status = identity;
	let _: fn(nibkeep_base::EntityId) -> nibkeep::base::EntityId = identity;
	let _: fn(nibkeep_model::Value) -> nibkeep::model::Value = identity;
	let _: fn(nibkeep_time::Day) -> nibkeep::time::Day = identity;
	let _: fn(nibkeep_crypto::PassphraseParams) -> nibkeep::crypto::PassphraseParams = identity;
	assert_eq!(nibkeep::vocabulary::LOGIN, nibkeep_std::LOGIN);
}
