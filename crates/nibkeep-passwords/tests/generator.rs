//! The password generator: what a recipe asks for, and nothing else.

use std::collections::BTreeMap;

use nibkeep_passwords::{GeneratorError, Recipe, generate};
use nibkeep_testkit::TestRng;

fn rng(seed: &str) -> TestRng {
	TestRng::new(seed)
}

#[test]
fn a_password_has_the_length_and_every_kind_of_character_asked_for() {
	let recipe = Recipe::default();
	assert_eq!(recipe.length, 20);
	let mut rng = rng("every kind");
	for _ in 0..200 {
		let password = generate(&recipe, &mut rng).expect("made");
		assert_eq!(password.chars().count(), 20);
		assert!(
			password.chars().any(|c| c.is_ascii_lowercase()),
			"{}",
			*password
		);
		assert!(
			password.chars().any(|c| c.is_ascii_uppercase()),
			"{}",
			*password
		);
		assert!(
			password.chars().any(|c| c.is_ascii_digit()),
			"{}",
			*password
		);
		assert!(
			password.chars().any(|c| c.is_ascii_punctuation()),
			"{}",
			*password
		);
	}
}

#[test]
fn a_password_has_only_the_characters_asked_for() {
	let digits = Recipe {
		length: 6,
		lowercase: false,
		uppercase: false,
		digits: true,
		symbols: false,
		unambiguous: false,
	};
	let mut rng = rng("only digits");
	for _ in 0..100 {
		let pin = generate(&digits, &mut rng).expect("made");
		assert_eq!(pin.len(), 6);
		assert!(pin.chars().all(|c| c.is_ascii_digit()), "{}", *pin);
	}
	let letters = Recipe {
		symbols: false,
		digits: false,
		..Recipe::default()
	};
	let password = generate(&letters, &mut rng).expect("made");
	assert!(
		password.chars().all(|c| c.is_ascii_alphabetic()),
		"{}",
		*password
	);
}

#[test]
fn symbols_are_those_that_sites_take_and_shells_leave_alone() {
	let symbols = Recipe {
		length: 64,
		lowercase: false,
		uppercase: false,
		digits: false,
		symbols: true,
		unambiguous: false,
	};
	let mut rng = rng("symbols");
	for _ in 0..50 {
		let password = generate(&symbols, &mut rng).expect("made");
		for c in password.chars() {
			assert!(c.is_ascii_punctuation(), "{c}");
			assert!(!"\"'`\\| ".contains(c), "{c}");
		}
	}
}

#[test]
fn unambiguous_passwords_leave_out_what_looks_alike() {
	let recipe = Recipe {
		length: 128,
		unambiguous: true,
		..Recipe::default()
	};
	let mut rng = rng("unambiguous");
	for _ in 0..50 {
		let password = generate(&recipe, &mut rng).expect("made");
		for c in password.chars() {
			assert!(!"0O1lI".contains(c), "{c}");
		}
	}
}

#[test]
fn every_character_is_about_as_likely() {
	let digits = Recipe {
		length: 100,
		lowercase: false,
		uppercase: false,
		digits: true,
		symbols: false,
		unambiguous: false,
	};
	let mut rng = rng("uniform");
	let mut counts: BTreeMap<char, usize> = BTreeMap::new();
	for _ in 0..1000 {
		for c in generate(&digits, &mut rng).expect("made").chars() {
			*counts.entry(c).or_default() += 1;
		}
	}
	// 100,000 digits: 10,000 of each expected, and a bias of a few percent would show.
	assert_eq!(counts.len(), 10);
	for (digit, count) in counts {
		assert!((9_600..=10_400).contains(&count), "{digit}: {count}");
	}
}

#[test]
fn the_same_randomness_gives_the_same_password() {
	let recipe = Recipe::default();
	let first = generate(&recipe, &mut rng("same")).expect("made");
	let again = generate(&recipe, &mut rng("same")).expect("made");
	let other = generate(&recipe, &mut rng("other")).expect("made");
	assert_eq!(first, again);
	assert_ne!(first, other);
}

#[test]
fn recipes_that_cannot_be_met_are_refused() {
	let nothing = Recipe {
		lowercase: false,
		uppercase: false,
		digits: false,
		symbols: false,
		..Recipe::default()
	};
	assert_eq!(
		generate(&nothing, &mut rng("x")),
		Err(GeneratorError::NoCharacters)
	);
	let short = Recipe {
		length: 3,
		..Recipe::default()
	};
	assert_eq!(
		generate(&short, &mut rng("x")),
		Err(GeneratorError::Length { length: 3 })
	);
	let long = Recipe {
		length: 1025,
		..Recipe::default()
	};
	assert_eq!(
		generate(&long, &mut rng("x")),
		Err(GeneratorError::Length { length: 1025 })
	);
	// Four kinds of character fit in four.
	let four = Recipe {
		length: 4,
		..Recipe::default()
	};
	assert!(generate(&four, &mut rng("x")).is_ok());
}

#[test]
fn a_recipe_says_how_strong_its_passwords_are() {
	let recipe = Recipe::default();
	// 20 of 89 characters: about 129 bits.
	assert_eq!(recipe.characters().len(), 89);
	assert_eq!(recipe.bits(), 129);
	let pin = Recipe {
		length: 4,
		lowercase: false,
		uppercase: false,
		digits: true,
		symbols: false,
		unambiguous: false,
	};
	assert_eq!(pin.bits(), 13);
}
