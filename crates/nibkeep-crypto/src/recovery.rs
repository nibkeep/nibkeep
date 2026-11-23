//! Recovery codes: a long random key that a person keeps on paper, and the keys derived from
//! it ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).

use std::fmt;

use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::{DecryptionKey, Key, SigningKey, VerifyingKey};

/// The context of a code's signing key, the *recovery key*.
const SIGNING_CONTEXT: &str = "nibkeep 2026-11 recovery signing key";

/// The context of the seed of a code's decryption key, which space keys are sealed to.
const DECRYPTION_CONTEXT: &str = "nibkeep 2026-11 recovery decryption key";

/// The context of the seed of a code's secrets decryption key, which secrets keys are sealed
/// to.
const SECRETS_DECRYPTION_CONTEXT: &str = "nibkeep 2026-11 recovery secrets decryption key";

/// The context of the short name of a recovery key.
const NAME_CONTEXT: &str = "nibkeep 2026-11 recovery names";

/// The symbols of a code's text, each of five bits: Crockford's base 32, which leaves out
/// the letters I, L, O and U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The number of the format, which the first four bits of a code's text hold.
const FORMAT: u8 = 1;

/// How many groups a code's text has, and how many symbols of the code each holds before
/// the symbol that checks it.
const GROUPS: usize = 13;
const GROUP_DATA: usize = 4;

/// How many symbols a code's text has: 13 groups of five.
const SYMBOLS: usize = GROUPS * (GROUP_DATA + 1);

/// Why a text is not a recovery code. None of these says anything of the code itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NotARecoveryCode {
	/// The text has this many letters and digits, and a code has 65.
	#[error("a recovery code has 65 letters and digits, in 13 groups of 5, and this has {0}")]
	Length(usize),
	/// This group, counted from 1, holds a character that no code holds.
	#[error("group {0} of the recovery code holds a character that no code has")]
	Character(usize),
	/// This group, counted from 1, does not check: a character of it is mistyped, or the
	/// group is in another's place.
	#[error("group {0} of the recovery code is mistyped")]
	Group(usize),
	/// Every group checks, but the code is of a format that this version does not read.
	#[error("the recovery code is of a format that this version does not read")]
	Format,
}

/// A recovery code: 32 random bytes, from which the keys that stand for its user are
/// derived. Whoever holds it can use it, and nothing guesses it.
///
/// The code is wiped from memory when it is dropped, and it is never printed.
#[derive(Clone)]
pub struct RecoveryCode(Key);

impl RecoveryCode {
	/// Returns a new random code.
	#[must_use]
	pub fn generate(rng: &mut (impl CryptoRng + ?Sized)) -> Self {
		Self(Key::generate(rng))
	}

	/// Returns the code with these bytes. The caller should wipe its own copy of them.
	#[must_use]
	pub const fn from_bytes(bytes: [u8; 32]) -> Self {
		Self(Key::from_bytes(bytes))
	}

	/// Whether two codes are the same, compared in constant time.
	#[must_use]
	pub fn same_as(&self, other: &Self) -> bool {
		self.0.same_as(&other.0)
	}

	/// The code's signing key, the *recovery key*, which signs the code's own encryption
	/// keys once, and then the listings of the devices that the code brings back.
	#[must_use]
	pub fn signing_key(&self) -> SigningKey {
		SigningKey::from_bytes(self.0.derive(SIGNING_CONTEXT).expose_bytes())
	}

	/// The code's decryption key, which opens the space keys sealed to the code.
	#[must_use]
	pub fn decryption_key(&self) -> DecryptionKey {
		DecryptionKey::from_seed(self.0.derive(DECRYPTION_CONTEXT).expose_bytes())
	}

	/// The code's secrets decryption key, which opens the secrets keys sealed to the code.
	#[must_use]
	pub fn secrets_decryption_key(&self) -> DecryptionKey {
		DecryptionKey::from_seed(self.0.derive(SECRETS_DECRYPTION_CONTEXT).expose_bytes())
	}

	/// The short name of a recovery key, four symbols of a code's alphabet, printed beside
	/// the code so that one sheet of paper is told from another. It tells nothing of the
	/// code.
	#[must_use]
	pub fn name_of(recovery_key: &VerifyingKey) -> String {
		let hash = blake3::derive_key(NAME_CONTEXT, recovery_key.as_bytes());
		let bits = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]);
		(0..4)
			.map(|place| char::from(ALPHABET[((bits >> (27 - 5 * place)) & 0x1f) as usize]))
			.collect()
	}

	/// The code as a person writes it down: 13 groups of five letters and digits, with
	/// hyphens between them. The first four bits name the format, the next 256 are the
	/// code, and the last symbol of each group checks the group and its place.
	#[must_use]
	pub fn to_text(&self) -> Zeroizing<String> {
		let symbols = self.symbols();
		let mut text = Zeroizing::new(String::with_capacity(SYMBOLS + GROUPS));
		for (group, data) in symbols.as_chunks::<GROUP_DATA>().0.iter().enumerate() {
			if group > 0 {
				text.push('-');
			}
			for &symbol in data.iter().chain([&check(group, data)]) {
				text.push(char::from(ALPHABET[usize::from(symbol)]));
			}
		}
		text
	}

	/// The code that a person typed: letters in either case, with or without hyphens and
	/// spaces, where `I` and `L` read as 1 and `O` as 0, which the alphabet leaves out as they
	/// look alike.
	///
	/// # Errors
	///
	/// Fails if the text is not a code, and says which group to look at.
	pub fn from_text(text: &str) -> Result<Self, NotARecoveryCode> {
		let mut symbols = Zeroizing::new([0_u8; SYMBOLS]);
		let mut count = 0;
		for character in text.chars() {
			if character == '-' || character.is_whitespace() {
				continue;
			}
			if count < SYMBOLS {
				symbols[count] = symbol_of(character)
					.ok_or(NotARecoveryCode::Character(count / (GROUP_DATA + 1) + 1))?;
			}
			count += 1;
		}
		if count != SYMBOLS {
			return Err(NotARecoveryCode::Length(count));
		}
		// The 260 bits of the format and the code, in 33 bytes whose last four bits stay 0.
		let mut bits = Zeroizing::new([0_u8; 33]);
		for (group, typed) in symbols
			.as_chunks::<{ GROUP_DATA + 1 }>()
			.0
			.iter()
			.enumerate()
		{
			let (data, checked) = typed.split_at(GROUP_DATA);
			if check(group, data) != checked[0] {
				return Err(NotARecoveryCode::Group(group + 1));
			}
			for (place, &symbol) in data.iter().enumerate() {
				let at = (group * GROUP_DATA + place) * 5;
				// A symbol's five bits fall within two bytes.
				let shifted = u16::from(symbol) << (11 - at % 8);
				bits[at / 8] |= shifted.to_be_bytes()[0];
				bits[at / 8 + 1] |= shifted.to_be_bytes()[1];
			}
		}
		if bits[0] >> 4 != FORMAT {
			return Err(NotARecoveryCode::Format);
		}
		let Ok(key) = Key::fill_with(|key| {
			for (index, byte) in key.iter_mut().enumerate() {
				*byte = (bits[index] << 4) | (bits[index + 1] >> 4);
			}
			Ok::<(), std::convert::Infallible>(())
		});
		Ok(Self(key))
	}

	/// The 52 symbols of the format and the code, five bits each.
	fn symbols(&self) -> Zeroizing<[u8; GROUPS * GROUP_DATA]> {
		let code = self.0.expose_bytes();
		// The format's four bits, then the code's 256, then four that no symbol takes.
		let mut bits = Zeroizing::new([0_u8; 33]);
		bits[0] = (FORMAT << 4) | (code[0] >> 4);
		for index in 1..32 {
			bits[index] = (code[index - 1] << 4) | (code[index] >> 4);
		}
		bits[32] = code[31] << 4;
		let mut symbols = Zeroizing::new([0_u8; GROUPS * GROUP_DATA]);
		for (index, symbol) in symbols.iter_mut().enumerate() {
			let at = index * 5;
			let window = u16::from_be_bytes([bits[at / 8], bits[at / 8 + 1]]);
			*symbol = ((window >> (11 - at % 8)) & 0x1f) as u8;
		}
		symbols
	}
}

impl fmt::Debug for RecoveryCode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("RecoveryCode(..)")
	}
}

/// The symbol that a character of a typed code stands for.
fn symbol_of(character: char) -> Option<u8> {
	let character = match character.to_ascii_uppercase() {
		'I' | 'L' => '1',
		'O' => '0',
		other => other,
	};
	let place = ALPHABET
		.iter()
		.position(|&symbol| char::from(symbol) == character)?;
	u8::try_from(place).ok()
}

/// The symbol that checks a group: with the group's four symbols `d1` to `d4` as elements
/// of the field of 32 elements, whose element `a` is a root of `x⁵ + x² + 1`, it is
/// `d1·a⁴ + d2·a³ + d3·a² + d4·a + g`, where `g` is the group's number, from 1. The five
/// symbols have five different weights, so one mistyped symbol and two swapped symbols both
/// show, and so does a group in another group's place.
fn check(group: usize, data: &[u8]) -> u8 {
	let number = u8::try_from(group + 1).unwrap_or(0);
	data.iter().fold(0, |sum, &symbol| times_a(sum ^ symbol)) ^ number
}

/// An element of the field of 32 elements times `a`, without a branch on its bits.
const fn times_a(element: u8) -> u8 {
	// a⁵ = a² + 1.
	((element << 1) & 0x1f) ^ (0x05 & 0_u8.wrapping_sub((element >> 4) & 1))
}
