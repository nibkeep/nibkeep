//! `nib protect`: a new passphrase for the library or its secrets, or the keystore instead.

use crate::common;

use crate::common::library;
use nibkeep_cli::CliError;
use nibkeep_node::{KeysError, NodeError};

/// The secrets' passphrase in these tests.
const SECRETS: &str = "tr0ub4dor";

#[test]
fn the_library_gets_a_passphrase_and_goes_back_to_the_keystore() {
	let mut test = library("nib protect");
	test.answer(&["new horse", "new horse"]);
	let printed = test.nib(&["protect", "--passphrase"]).expect("protects");
	assert_eq!(
		printed,
		"The library's keys are protected by a passphrase now.\n"
	);
	// The next command asks for it.
	test.answer(&["wrong horse"]);
	assert!(matches!(
		test.nib(&["status"]),
		Err(CliError::Node(NodeError::Keys(KeysError::WrongPassphrase)))
	));
	test.answer(&["new horse"]);
	test.nib(&["status"]).expect("opens");

	test.answer(&["new horse"]);
	let json = test.json(&["protect"]);
	assert_eq!(json["protected"], "library");
	assert_eq!(json["by"], "keystore");
	let asked = test.asked;
	test.nib(&["status"]).expect("opens");
	assert_eq!(test.asked, asked, "the keystore opens it");
}

#[test]
fn the_secrets_get_a_new_passphrase() {
	let mut test = library("nib protect --secrets");
	test.nib(&["new", "std.login", "name=Bank"]).expect("adds");
	test.answer(&[SECRETS, SECRETS, "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");

	test.answer(&[SECRETS, "s3cond", "s3cond"]);
	let printed = test
		.nib(&["protect", "--secrets", "--passphrase"])
		.expect("protects");
	assert_eq!(
		printed,
		"The library's secrets are protected by a passphrase now.\n"
	);
	test.answer(&[SECRETS]);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::Node(NodeError::Keys(KeysError::WrongPassphrase)))
	));
	test.answer(&["s3cond"]);
	assert_eq!(
		test.nib(&["secret", "show", "Bank", "password"])
			.expect("shows"),
		"hunter2\n"
	);
}

#[test]
fn the_library_and_its_secrets_keep_passphrases_of_their_own() {
	let mut test = library("nib protect apart");
	test.nib(&["new", "std.login", "name=Bank"]).expect("adds");
	test.answer(&[SECRETS, SECRETS, "hunter2", "hunter2"]);
	test.nib(&["secret", "set", "Bank", "password"])
		.expect("sets");
	test.answer(&[SECRETS, SECRETS]);
	assert!(matches!(
		test.nib(&["protect", "--passphrase"]),
		Err(CliError::Node(NodeError::LibraryPassphrase))
	));
	test.answer(&["new horse", "new horse"]);
	test.nib(&["protect", "--passphrase"]).expect("protects");
	test.answer(&["new horse", SECRETS, "new horse", "new horse"]);
	assert!(matches!(
		test.nib(&["protect", "--secrets", "--passphrase"]),
		Err(CliError::Node(NodeError::LibraryPassphrase))
	));
	// The secrets' passphrase is what it was.
	test.answer(&["new horse", SECRETS]);
	test.nib(&["secret", "show", "Bank", "password"])
		.expect("shows");
}

#[test]
fn a_library_without_secrets_has_none_to_protect() {
	let mut test = library("nib protect no secrets");
	assert!(matches!(
		test.nib(&["protect", "--secrets"]),
		Err(CliError::NoSecrets)
	));
	assert_eq!(test.asked, 0);
}

#[test]
fn without_a_keystore_only_a_passphrase_protects() {
	let mut test = common::Test::new("nib protect no keystore");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada", "--passphrase"])
		.expect("starts");
	for arguments in [&["protect"][..], &["protect", "--secrets"]] {
		// Asked only for the library's passphrase, which opens it.
		test.answer(&["correct horse"]);
		let asked = test.asked;
		assert!(matches!(
			test.nib(arguments),
			Err(CliError::NoKeystoreToProtect)
		));
		assert_eq!(test.asked, asked + 1);
	}
}
