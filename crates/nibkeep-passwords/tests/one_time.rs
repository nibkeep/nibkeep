//! One-time codes: the test vectors of RFC 4226 and RFC 6238, and the links that sites show
//! as QR codes.

use nibkeep_passwords::{Algorithm, OneTimeCode, OneTimeError, hotp};

/// The secrets of RFC 6238's test vectors, one for each algorithm.
const SHA1_SEED: &[u8] = b"12345678901234567890";
const SHA256_SEED: &[u8] = b"12345678901234567890123456789012";
const SHA512_SEED: &[u8] = b"1234567890123456789012345678901234567890123456789012345678901234";

#[test]
fn hotp_gives_the_codes_of_rfc_4226() {
	let codes = [
		"755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583", "399871",
		"520489",
	];
	for (counter, code) in (0..).zip(codes) {
		assert_eq!(hotp(SHA1_SEED, Algorithm::Sha1, counter, 6).as_str(), code);
	}
}

#[test]
fn totp_gives_the_codes_of_rfc_6238() {
	let vectors: [(u64, [&str; 3]); 6] = [
		(59, ["94287082", "46119246", "90693936"]),
		(1_111_111_109, ["07081804", "68084774", "25091201"]),
		(1_111_111_111, ["14050471", "67062674", "99943326"]),
		(1_234_567_890, ["89005924", "91819424", "93441116"]),
		(2_000_000_000, ["69279037", "90698825", "38618901"]),
		(20_000_000_000, ["65353130", "77737706", "47863826"]),
	];
	let generators = [
		OneTimeCode::new(SHA1_SEED.to_vec(), Algorithm::Sha1, 8, 30).expect("valid"),
		OneTimeCode::new(SHA256_SEED.to_vec(), Algorithm::Sha256, 8, 30).expect("valid"),
		OneTimeCode::new(SHA512_SEED.to_vec(), Algorithm::Sha512, 8, 30).expect("valid"),
	];
	for (seconds, codes) in vectors {
		for (generator, code) in generators.iter().zip(codes) {
			assert_eq!(generator.code_at(seconds).as_str(), code, "at {seconds}");
		}
	}
}

#[test]
fn a_code_lasts_until_its_period_ends() {
	let generator = OneTimeCode::new(SHA1_SEED.to_vec(), Algorithm::Sha1, 6, 30).expect("valid");
	assert_eq!(generator.seconds_left(0), 30);
	assert_eq!(generator.seconds_left(59), 1);
	assert_eq!(generator.seconds_left(60), 30);
	assert_eq!(generator.code_at(30), generator.code_at(59));
	assert_ne!(generator.code_at(59), generator.code_at(60));
}

#[test]
fn a_secret_is_read_in_base32_as_sites_show_it() {
	// "12345678901234567890" in base32, as a site shows it: in groups, in lower case.
	let shown = "gezd gnbv gy3t qojq gezd gnbv gy3t qojq";
	let generator = OneTimeCode::parse(shown).expect("a secret");
	assert_eq!(generator.algorithm(), Algorithm::Sha1);
	assert_eq!(generator.digits(), 6);
	assert_eq!(generator.period(), 30);
	assert_eq!(generator.code_at(59).as_str(), "287082");
	// Padding is allowed, and so are hyphens between groups.
	let padded = OneTimeCode::parse("MZXW6===").expect("a secret");
	let unpadded = OneTimeCode::parse("MZXW-6").expect("a secret");
	assert_eq!(padded.code_at(0), unpadded.code_at(0));
}

#[test]
fn a_link_gives_its_secret_settings_and_names() {
	let link = "otpauth://totp/Example%20Co:alice%40example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Example%20Co&algorithm=SHA256&digits=8&period=60";
	let generator = OneTimeCode::parse(link).expect("a link");
	assert_eq!(generator.algorithm(), Algorithm::Sha256);
	assert_eq!(generator.digits(), 8);
	assert_eq!(generator.period(), 60);
	assert_eq!(generator.issuer(), Some("Example Co"));
	assert_eq!(generator.account(), Some("alice@example.com"));
	let same = OneTimeCode::new(SHA1_SEED.to_vec(), Algorithm::Sha256, 8, 60).expect("valid");
	assert_eq!(
		generator.code_at(1_234_567_890),
		same.code_at(1_234_567_890)
	);

	// Without an issuer in the query, the label's prefix names it; without one, none.
	let labelled = OneTimeCode::parse("otpauth://totp/Mail:bob?secret=MZXW6").expect("a link");
	assert_eq!(labelled.issuer(), Some("Mail"));
	assert_eq!(labelled.account(), Some("bob"));
	let plain = OneTimeCode::parse("otpauth://totp/bob?secret=MZXW6").expect("a link");
	assert_eq!(plain.issuer(), None);
	assert_eq!(plain.account(), Some("bob"));
	// Scheme and type in any case, as some sites write them.
	assert!(OneTimeCode::parse("OTPAUTH://TOTP/bob?secret=MZXW6").is_ok());
}

#[test]
fn what_cannot_make_codes_is_refused_without_showing_the_secret() {
	let refused = [
		("", OneTimeError::NoSecret),
		("   ", OneTimeError::NoSecret),
		("not base32 at all!", OneTimeError::NotBase32),
		("MZXW1", OneTimeError::NotBase32),
		("otpauth://totp/bob", OneTimeError::NoSecret),
		(
			"otpauth://totp/bob?secret=MZXW6&algorithm=MD5",
			OneTimeError::Algorithm,
		),
		(
			"otpauth://totp/bob?secret=MZXW6&digits=4",
			OneTimeError::Digits,
		),
		(
			"otpauth://totp/bob?secret=MZXW6&digits=eleven",
			OneTimeError::Digits,
		),
		(
			"otpauth://totp/bob?secret=MZXW6&period=0",
			OneTimeError::Period,
		),
		(
			"otpauth://hotp/bob?secret=MZXW6&counter=1",
			OneTimeError::CounterBased,
		),
		("otpauth://other/bob?secret=MZXW6", OneTimeError::NotALink),
		("https://example.com/?secret=MZXW6", OneTimeError::NotBase32),
	];
	for (text, error) in refused {
		let found = OneTimeCode::parse(text).expect_err(text);
		assert_eq!(found, error, "{text}");
		assert!(!found.to_string().contains("MZXW6"), "{found}");
	}
	assert!(OneTimeCode::new(Vec::new(), Algorithm::Sha1, 6, 30).is_err());
	assert!(OneTimeCode::new(SHA1_SEED.to_vec(), Algorithm::Sha1, 11, 30).is_err());
}

#[test]
fn a_generator_never_shows_its_secret() {
	let generator = OneTimeCode::parse("otpauth://totp/bob?secret=MZXW6YTBOI").expect("a link");
	let shown = format!("{generator:?}");
	assert!(!shown.contains("MZXW6YTBOI"), "{shown}");
	assert!(!shown.contains("foobar"), "{shown}");
}

#[test]
fn a_label_keeps_its_plus_signs_and_escapes_that_are_not_ones() {
	let generator =
		OneTimeCode::parse("otpauth://totp/Mail:alice+work%40example.com%+5?secret=MZXW6")
			.expect("a link");
	assert_eq!(generator.account(), Some("alice+work@example.com%+5"));
}

#[test]
fn what_a_person_keeps_as_the_secret_is_a_link_of_any_kind_or_a_key() {
	for kept in [
		"MZXW6YTBOI",
		"gezd gnbv gy3t qojq",
		"otpauth://totp/bob?secret=MZXW6",
		"otpauth://hotp/bob?secret=MZXW6&counter=1",
		"otpauth://steam/bob?secret=MZXW6",
	] {
		assert_eq!(OneTimeCode::check(kept), Ok(()), "{kept}");
	}
	assert_eq!(
		OneTimeCode::check("not a secret!"),
		Err(OneTimeError::NotBase32)
	);
	assert_eq!(OneTimeCode::check(""), Err(OneTimeError::NoSecret));
}
