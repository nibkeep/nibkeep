//! What daily use of passwords needs from `nib secret`: copying a secret, marked as
//! sensitive and cleared afterwards; generating passwords; and one-time codes.

use std::time::Duration;

use crate::common::{Test, library};
use nibkeep_cli::CliError;
use nibkeep_passwords::{GeneratorError, OneTimeCode, OneTimeError};

/// The secrets' passphrase in these tests.
const SECRETS: &str = "tr0ub4dor";

/// A library with a login whose password is set.
fn with_password(name: &str) -> Test {
	let mut test = library(name);
	test.nib(&["new", "std.login", "name=Bank", "std.username=ada"])
		.expect("adds the login");
	test.answer(&[
		SECRETS,
		SECRETS,
		"a sentinel password",
		"a sentinel password",
	]);
	test.nib(&["secret", "set", "Bank", "std.password"])
		.expect("sets");
	test
}

#[test]
fn a_copied_secret_is_marked_sensitive_and_cleared_after_a_while() {
	let mut test = with_password("nib secret copy");
	test.answer(&[SECRETS]);
	let printed = test
		.nib(&["secret", "copy", "Bank", "std.password"])
		.expect("copies");
	assert!(!printed.contains("a sentinel password"), "{printed}");
	assert_eq!(
		printed,
		"Copied std.password of Bank. The clipboard clears in 30 s; Ctrl+C clears it now.\n\
		Cleared the clipboard.\n"
	);
	assert_eq!(test.waited, [Duration::from_secs(30)]);
	// The clipboard held the password, marked, while nib waited, and is cleared since.
	assert_eq!(test.held, [(Some("a sentinel password".to_owned()), true)]);
	assert_eq!(test.clip().text, None);
	// Ctrl+C was taken before the copy, so that it never leaves the secret behind.
	assert_eq!(test.clip().copied_interruptible, [true]);

	test.answer(&[SECRETS]);
	test.interrupts = true;
	let json = test.json(&[
		"secret",
		"copy",
		"Bank",
		"std.password",
		"--clear-after",
		"5",
	]);
	assert_eq!(json["field"], "std.password");
	assert_eq!(json["values"], 1);
	assert_eq!(json["cleared"], true);
	assert_eq!(json["interrupted"], true);
	assert_eq!(test.waited[1], Duration::from_secs(5));
	assert!(!json.to_string().contains("a sentinel password"));
}

#[test]
fn what_the_person_copies_meanwhile_stays() {
	let mut test = with_password("nib secret copy meanwhile");
	test.copied_meanwhile = Some("a link");
	test.answer(&[SECRETS]);
	let printed = test
		.nib(&["secret", "copy", "Bank", "password"])
		.expect("copies");
	assert!(
		printed.ends_with("The clipboard holds something else now, which stays.\n"),
		"{printed}"
	);
	assert_eq!(test.clip().text.as_deref(), Some("a link"));
}

#[test]
fn a_clipboard_that_fails_says_so() {
	let mut test = with_password("nib secret copy failing");
	test.clipboard = None;
	// Refused before the passphrase is asked for.
	let asked = test.asked;
	let refused = test.nib(&["secret", "copy", "Bank", "password"]);
	assert_eq!(test.asked, asked);
	assert!(
		matches!(&refused, Err(CliError::Clipboard(error)) if error.to_string() == "the clipboard failed: no display"),
		"{refused:?}"
	);
	assert_eq!(test.waited, Vec::<Duration>::new());
}

#[test]
fn only_secret_fields_with_values_are_copied() {
	let mut test = with_password("nib secret copy refused");
	assert!(matches!(
		test.nib(&["secret", "copy", "Bank", "username"]),
		Err(CliError::NotSecret(_))
	));
	test.answer(&[SECRETS]);
	assert!(matches!(
		test.nib(&["secret", "copy", "Bank", "one-time-code"]),
		Err(CliError::NoSecretValue { .. })
	));
	assert_eq!(test.waited, Vec::<Duration>::new());
	assert_eq!(test.clip().text, None);
}

#[test]
fn passwords_are_generated_as_asked_without_a_library() {
	let mut test = Test::new("nib secret generate");
	let password = test.nib(&["secret", "generate"]).expect("generates");
	assert_eq!(password.trim_end().chars().count(), 20, "{password}");
	let short = test
		.nib(&[
			"secret",
			"generate",
			"--length",
			"32",
			"--no-symbols",
			"--unambiguous",
		])
		.expect("generates");
	let short = short.trim_end();
	assert_eq!(short.len(), 32);
	assert!(short.chars().all(|c| c.is_ascii_alphanumeric()), "{short}");
	assert!(!short.contains(['0', 'O', 'o', '1', 'l', 'I']), "{short}");
	let json = test.json(&["secret", "generate", "--length", "16"]);
	assert_eq!(json["password"].as_str().expect("a password").len(), 16);
	assert_eq!(json["bits"], 103);
	assert!(matches!(
		test.nib(&["secret", "generate", "--length", "2"]),
		Err(CliError::Generator(GeneratorError::Length { length: 2 }))
	));
}

#[test]
fn a_generated_password_is_set_without_being_shown() {
	let mut test = with_password("nib secret set --generate");
	test.answer(&[SECRETS]);
	let printed = test
		.nib(&[
			"secret",
			"set",
			"Bank",
			"password",
			"--generate",
			"--length",
			"24",
		])
		.expect("sets");
	// Asked only for the secrets' passphrase.
	assert_eq!(test.asked, 5);
	test.answer(&[SECRETS]);
	let password = test
		.nib(&["secret", "show", "Bank", "password"])
		.expect("shows");
	let password = password.trim_end();
	assert_eq!(password.chars().count(), 24);
	assert!(!printed.contains(password), "{printed}");
	// What makes passwords is chosen only for a generated one.
	assert!(matches!(
		test.nib(&["secret", "set", "Bank", "password", "--length", "24"]),
		Err(CliError::RecipeWithoutGenerate)
	));
}

#[test]
fn one_time_codes_come_from_the_secret_that_a_site_shows() {
	let mut test = with_password("nib secret code");
	let link = "otpauth://totp/Bank:ada?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Bank";
	test.answer(&[SECRETS, link, link]);
	test.nib(&["secret", "set", "Bank", "one-time-code"])
		.expect("sets");
	let expected = OneTimeCode::parse(link).expect("a link");
	let seconds = test.now / 1000;
	test.answer(&[SECRETS]);
	let printed = test.nib(&["secret", "code", "Bank"]).expect("shows");
	assert_eq!(
		printed,
		format!(
			"{} ({} s left)\n",
			expected.code_at(seconds).as_str(),
			expected.seconds_left(seconds)
		)
	);
	test.answer(&[SECRETS]);
	let json = test.json(&["secret", "code", "Bank", "std.one-time-code"]);
	assert_eq!(json["code"], expected.code_at(seconds).as_str());
	assert_eq!(json["seconds_left"], expected.seconds_left(seconds));
	assert_eq!(json["issuer"], "Bank");
	assert_eq!(json["account"], "ada");
	assert!(!json.to_string().contains("GEZDGNBV"));

	// A secret that makes no codes is refused when it is set, and never shown.
	test.answer(&[SECRETS, "not a secret!", "not a secret!"]);
	let refused = test.nib(&["secret", "set", "Bank", "one-time-code"]);
	assert!(
		matches!(&refused, Err(CliError::OneTime(OneTimeError::NotBase32))),
		"{refused:?}"
	);
	assert!(!format!("{refused:?}").contains("not a secret!"));
}

#[test]
fn a_deleted_login_still_gives_its_secrets() {
	let mut test = with_password("nib secret copy deleted");
	test.nib(&["edit", "Bank", "--delete"]).expect("deletes");
	test.answer(&[SECRETS]);
	test.nib(&["secret", "copy", "Bank", "password"])
		.expect("copies");
	assert_eq!(test.held[0].0.as_deref(), Some("a sentinel password"));
}

#[test]
fn a_password_that_cannot_be_generated_changes_nothing() {
	let mut test = library("nib secret set --generate refused");
	test.nib(&["new", "std.login", "name=Bank"])
		.expect("adds the login");
	// Refused before the first secrets would be set up, and nothing asked for.
	assert!(matches!(
		test.nib(&[
			"secret",
			"set",
			"Bank",
			"password",
			"--generate",
			"--length",
			"2"
		]),
		Err(CliError::Generator(GeneratorError::Length { length: 2 }))
	));
	assert!(matches!(
		test.nib(&[
			"secret",
			"set",
			"Bank",
			"password",
			"--generate",
			"--no-lowercase",
			"--no-uppercase",
			"--no-digits",
			"--no-symbols",
		]),
		Err(CliError::Generator(GeneratorError::NoCharacters))
	));
	assert!(matches!(
		test.nib(&["secret", "set", "Bank", "one-time-code", "--generate"]),
		Err(CliError::GeneratedOneTimeCode(_))
	));
	// The default length given is still an option of --generate.
	assert!(matches!(
		test.nib(&["secret", "set", "Bank", "password", "--length", "20"]),
		Err(CliError::RecipeWithoutGenerate)
	));
	assert_eq!(test.asked, 0);
	assert!(matches!(
		test.nib(&["secret", "show", "Bank", "password"]),
		Err(CliError::NoSecrets)
	));
}

#[test]
fn a_secret_of_codes_counted_by_use_is_kept_but_makes_no_codes() {
	let mut test = with_password("nib secret code hotp");
	let link = "otpauth://hotp/Bank:ada?secret=MZXW6YTBOI&counter=3";
	test.answer(&[SECRETS, link, link]);
	test.nib(&["secret", "set", "Bank", "one-time-code"])
		.expect("keeps it");
	test.answer(&[SECRETS]);
	assert!(matches!(
		test.nib(&["secret", "code", "Bank"]),
		Err(CliError::OneTime(OneTimeError::CounterBased))
	));
}
