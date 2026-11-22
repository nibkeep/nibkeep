//! Recovery codes: their text, the characters that check it, and the keys derived from them.

mod common;

use nibkeep_crypto::{NotARecoveryCode, RecoveryCode};

/// The 32 symbols of a code's text, in order.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The bytes 0, 1, …, 31, of the code that the specification's test vector has.
const COUNTING: [u8; 32] = [
	0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
	26, 27, 28, 29, 30, 31,
];

fn counting() -> RecoveryCode {
	RecoveryCode::from_bytes(COUNTING)
}

#[test]
fn a_code_is_thirteen_groups_of_five_characters() {
	let code = RecoveryCode::generate(&mut common::rng("groups"));
	let text = code.to_text();
	let groups: Vec<&str> = text.split('-').collect();
	assert_eq!(groups.len(), 13);
	for group in groups {
		assert_eq!(group.len(), 5);
		assert!(group.bytes().all(|symbol| ALPHABET.contains(&symbol)));
	}
}

#[test]
fn a_code_round_trips_through_its_text() {
	let mut rng = common::rng("round trip");
	for _ in 0..64 {
		let code = RecoveryCode::generate(&mut rng);
		let read = RecoveryCode::from_text(&code.to_text()).unwrap();
		assert!(read.same_as(&code));
	}
}

#[test]
fn generated_codes_differ() {
	let mut rng = common::rng("generated codes");
	let first = RecoveryCode::generate(&mut rng);
	assert!(!first.same_as(&RecoveryCode::generate(&mut rng)));
}

#[test]
fn the_text_is_the_specifications() {
	let code = counting();
	assert_eq!(
		code.to_text().as_str(),
		"20016-081G9-8186P-0W40F-J2GB1-1G6GZ-W3RG6-2491B-650N4-2RBHZ-G68T7-3CE19-T7GZX"
	);
}

#[test]
fn the_derived_keys_are_the_specifications() {
	let code = counting();
	let recovery_key = code.signing_key().verifying_key();
	assert_eq!(
		recovery_key.as_bytes().as_slice(),
		common::hex("c4cfbe45012b4efca3dc335cd6ea00bf65100fac27da14cb6cc55f7f6008645d")
	);
	// The BLAKE3 hashes of the two encryption keys' 1,216 bytes.
	let encryption = code.decryption_key().encryption_key().to_bytes();
	assert_eq!(
		blake3::hash(&encryption).to_hex().as_str(),
		"51649c5f35c36a6f7ce73774ea1292ee7b8ade6e4360c8bea0716773e4844d5f"
	);
	let secrets_encryption = code.secrets_decryption_key().encryption_key().to_bytes();
	assert_eq!(
		blake3::hash(&secrets_encryption).to_hex().as_str(),
		"27b6f92cda6dea741b9c714c8fca01b16ad07eeeef4f817e6e141c270b1285df"
	);
	assert_eq!(RecoveryCode::name_of(&recovery_key), "NSWN");
}

#[test]
fn each_key_is_derived_for_its_purpose_alone() {
	let code = counting();
	assert_eq!(
		code.signing_key().expose_bytes(),
		&blake3::derive_key("nibkeep 2026-11 recovery signing key", &COUNTING)
	);
	assert_eq!(
		*code.decryption_key().expose_seed(),
		blake3::derive_key("nibkeep 2026-11 recovery decryption key", &COUNTING)
	);
	assert_eq!(
		*code.secrets_decryption_key().expose_seed(),
		blake3::derive_key("nibkeep 2026-11 recovery secrets decryption key", &COUNTING)
	);
}

#[test]
fn reading_passes_over_case_spaces_and_hyphens() {
	let code = RecoveryCode::generate(&mut common::rng("typed"));
	let text = code.to_text();
	let typed = text.to_lowercase().replace('-', "  ");
	assert!(RecoveryCode::from_text(&typed).unwrap().same_as(&code));
	let run_together = text.replace('-', "");
	assert!(
		RecoveryCode::from_text(&run_together)
			.unwrap()
			.same_as(&code)
	);
	let spaced = format!("  {}\n", text.replace('-', " - "));
	assert!(RecoveryCode::from_text(&spaced).unwrap().same_as(&code));
}

#[test]
fn letters_that_look_like_digits_read_as_them() {
	// The counting code has zeros and ones to mistake.
	let code = counting();
	let text = code.to_text();
	assert!(text.contains('0') && text.contains('1'));
	for (digit, letters) in [('0', "Oo"), ('1', "IiLl")] {
		for letter in letters.chars() {
			let typed = text.replace(digit, &letter.to_string());
			assert!(RecoveryCode::from_text(&typed).unwrap().same_as(&code));
		}
	}
}

#[test]
fn a_mistyped_character_names_its_group() {
	let code = RecoveryCode::generate(&mut common::rng("mistyped"));
	let text = code.to_text();
	for (place, symbol) in text.bytes().enumerate() {
		if symbol == b'-' {
			continue;
		}
		let group = place / 6 + 1;
		for &other in ALPHABET.iter().filter(|&&other| other != symbol) {
			let mut typed = text.as_bytes().to_vec();
			typed[place] = other;
			let typed = String::from_utf8(typed).unwrap();
			match RecoveryCode::from_text(&typed) {
				Err(NotARecoveryCode::Group(named)) => assert_eq!(named, group),
				// The first character also carries the format's number.
				Err(NotARecoveryCode::Format) => assert_eq!(group, 1),
				other => panic!("a mistyped character in group {group} read as {other:?}"),
			}
		}
	}
}

#[test]
fn two_characters_swapped_within_a_group_are_told() {
	let code = RecoveryCode::generate(&mut common::rng("swapped"));
	let text = code.to_text();
	for group in 0..13 {
		for first in 0..5 {
			for second in first + 1..5 {
				let mut typed = text.as_bytes().to_vec();
				let (first, second) = (group * 6 + first, group * 6 + second);
				if typed[first] == typed[second] {
					continue;
				}
				typed.swap(first, second);
				let typed = String::from_utf8(typed).unwrap();
				assert_eq!(
					RecoveryCode::from_text(&typed).err(),
					Some(NotARecoveryCode::Group(group + 1))
				);
			}
		}
	}
}

#[test]
fn a_group_in_another_groups_place_is_told() {
	let code = RecoveryCode::generate(&mut common::rng("groups swapped"));
	let text = code.to_text();
	let groups: Vec<&str> = text.split('-').collect();
	for first in 0..13 {
		for second in first + 1..13 {
			let mut typed = groups.clone();
			typed.swap(first, second);
			let error = RecoveryCode::from_text(&typed.join("-")).unwrap_err();
			// The earlier of the two is the one named.
			assert!(
				error == NotARecoveryCode::Group(first + 1) || error == NotARecoveryCode::Format,
				"{error:?}"
			);
		}
	}
}

#[test]
fn a_code_of_another_length_says_how_many_characters_it_has() {
	let code = RecoveryCode::generate(&mut common::rng("length"));
	let text = code.to_text();
	assert_eq!(
		RecoveryCode::from_text(&text[..text.len() - 6]).err(),
		Some(NotARecoveryCode::Length(60))
	);
	assert_eq!(
		RecoveryCode::from_text(&format!("{}-00000", text.as_str())).err(),
		Some(NotARecoveryCode::Length(70))
	);
	assert_eq!(
		RecoveryCode::from_text("").err(),
		Some(NotARecoveryCode::Length(0))
	);
}

#[test]
fn a_character_outside_the_alphabet_names_its_group() {
	let code = RecoveryCode::generate(&mut common::rng("alphabet"));
	let text = code.to_text();
	for strange in ['U', 'u', '!', 'é', '_'] {
		let mut typed: Vec<char> = text.chars().collect();
		typed[2 * 6 + 3] = strange;
		let typed: String = typed.into_iter().collect();
		assert_eq!(
			RecoveryCode::from_text(&typed).err(),
			Some(NotARecoveryCode::Character(3))
		);
	}
}

#[test]
fn a_code_of_another_format_is_refused() {
	// The first four bits name the format, 1: a code that starts with another number is of
	// a format this version does not read, even where its groups check.
	let text = counting().to_text();
	assert!(text.starts_with('2'));
	let mut refused = 0;
	for &first in ALPHABET {
		for &check in ALPHABET {
			let mut typed = text.as_bytes().to_vec();
			typed[0] = first;
			typed[4] = check;
			let typed = String::from_utf8(typed).unwrap();
			match RecoveryCode::from_text(&typed) {
				Ok(code) => assert!(first == b'2' || first == b'3', "{code:?}"),
				Err(NotARecoveryCode::Format) => refused += 1,
				Err(error) => assert_eq!(error, NotARecoveryCode::Group(1)),
			}
		}
	}
	// One check character fits each first character, and 30 of the 32 name another format.
	assert_eq!(refused, 30);
}

#[test]
fn a_code_is_never_printed() {
	let code = counting();
	let printed = format!("{code:?}");
	assert!(!printed.contains("20016"));
	assert_eq!(printed, "RecoveryCode(..)");
}

#[test]
fn a_name_is_four_characters_of_the_alphabet() {
	let mut rng = common::rng("names");
	let first = RecoveryCode::generate(&mut rng)
		.signing_key()
		.verifying_key();
	let second = RecoveryCode::generate(&mut rng)
		.signing_key()
		.verifying_key();
	let name = RecoveryCode::name_of(&first);
	assert_eq!(name.len(), 4);
	assert!(name.bytes().all(|symbol| ALPHABET.contains(&symbol)));
	assert_eq!(name, RecoveryCode::name_of(&first));
	assert_ne!(name, RecoveryCode::name_of(&second));
}
