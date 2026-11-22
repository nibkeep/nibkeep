//! The password generator: random characters of the kinds a recipe asks for, at least one
//! of each, every character as likely as any other of its alphabet.

use rand_core::CryptoRng;
use zeroize::Zeroizing;

/// The shortest password generated: one of each kind of character fits.
pub const MIN_LENGTH: usize = 4;

/// The longest password generated.
pub const MAX_LENGTH: usize = 1024;

const LOWERCASE: &str = "abcdefghijklmnopqrstuvwxyz";
const UPPERCASE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &str = "0123456789";
/// ASCII's punctuation, without quotes, the backtick, the backslash and the bar, which some
/// sites refuse and shells read.
const SYMBOLS: &str = "!#$%&()*+,-./:;<=>?@[]^_{}~";
/// Characters that look like others in some fonts.
const AMBIGUOUS: &str = "0Oo1lI";

/// What a generated password is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "independent choices, as an app shows them: one switch each"
)]
pub struct Recipe {
	/// How many characters it has.
	pub length: usize,
	/// Whether it has lowercase letters.
	pub lowercase: bool,
	/// Whether it has uppercase letters.
	pub uppercase: bool,
	/// Whether it has digits.
	pub digits: bool,
	/// Whether it has symbols.
	pub symbols: bool,
	/// Whether it leaves out characters that look like others, such as `0` and `O`.
	pub unambiguous: bool,
}

impl Default for Recipe {
	/// Twenty characters of every kind: about 129 bits.
	fn default() -> Self {
		Self {
			length: 20,
			lowercase: true,
			uppercase: true,
			digits: true,
			symbols: true,
			unambiguous: false,
		}
	}
}

impl Recipe {
	/// The kinds of character asked for, each as the characters it may use.
	fn kinds(&self) -> Vec<Vec<char>> {
		[
			(self.lowercase, LOWERCASE),
			(self.uppercase, UPPERCASE),
			(self.digits, DIGITS),
			(self.symbols, SYMBOLS),
		]
		.into_iter()
		.filter(|(asked, _)| *asked)
		.map(|(_, characters)| {
			characters
				.chars()
				.filter(|c| !(self.unambiguous && AMBIGUOUS.contains(*c)))
				.collect()
		})
		.collect()
	}

	/// Every character that a password may have.
	#[must_use]
	pub fn characters(&self) -> Vec<char> {
		self.kinds().concat()
	}

	/// How strong its passwords are, in bits: the logarithm of how many passwords of its
	/// length its characters make, rounded down. Requiring one of each kind takes a little
	/// away, which this leaves out.
	#[must_use]
	pub fn bits(&self) -> u32 {
		let characters = self.characters().len();
		if characters < 2 {
			return 0;
		}
		// Lengths and alphabets are small, so the product is exact enough as a float.
		#[expect(clippy::cast_precision_loss, reason = "both are far below 2^52")]
		let bits = self.length as f64 * (characters as f64).log2();
		#[expect(
			clippy::cast_possible_truncation,
			clippy::cast_sign_loss,
			reason = "a positive number of bits below 2^32"
		)]
		let bits = bits.floor() as u32;
		bits
	}
}

/// Why a recipe cannot be met.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GeneratorError {
	/// No kind of character was asked for.
	#[error("a password needs some kind of character: letters, digits or symbols")]
	NoCharacters,
	/// The length is too short or too long.
	#[error("a password is from {MIN_LENGTH} to {MAX_LENGTH} characters long, not {length}")]
	Length {
		/// The length asked for.
		length: usize,
	},
}

/// Generates a password as `recipe` says: at least one character of each kind it asks for,
/// the rest from all of them, in random order.
///
/// # Errors
///
/// Returns an error if the recipe asks for no characters, or for a length outside
/// [`MIN_LENGTH`] to [`MAX_LENGTH`].
pub fn generate(
	recipe: &Recipe,
	rng: &mut impl CryptoRng,
) -> Result<Zeroizing<String>, GeneratorError> {
	if !(MIN_LENGTH..=MAX_LENGTH).contains(&recipe.length) {
		return Err(GeneratorError::Length {
			length: recipe.length,
		});
	}
	let kinds = recipe.kinds();
	if kinds.is_empty() {
		return Err(GeneratorError::NoCharacters);
	}
	let all: Vec<char> = kinds.concat();
	let mut chosen = Zeroizing::new(Vec::with_capacity(recipe.length));
	for kind in &kinds {
		chosen.push(kind[below(rng, kind.len())]);
	}
	while chosen.len() < recipe.length {
		chosen.push(all[below(rng, all.len())]);
	}
	// Fisher and Yates: the characters required of each kind could be anywhere.
	for at in (1..chosen.len()).rev() {
		let other = below(rng, at + 1);
		chosen.swap(at, other);
	}
	let mut password = Zeroizing::new(String::with_capacity(recipe.length));
	password.extend(chosen.iter());
	Ok(password)
}

/// A number below `bound`, each as likely: random numbers past the last whole multiple of
/// `bound` are drawn again, rather than folded onto the first.
fn below(rng: &mut impl CryptoRng, bound: usize) -> usize {
	let bound = u32::try_from(bound).expect("alphabets and passwords are short");
	let zone = u32::MAX - u32::MAX % bound;
	loop {
		let drawn = rng.next_u32();
		if drawn < zone {
			return usize::try_from(drawn % bound).expect("below a usize");
		}
	}
}
