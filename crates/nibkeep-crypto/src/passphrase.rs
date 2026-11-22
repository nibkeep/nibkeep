//! Keys derived from passphrases with Argon2id.

use argon2::{Algorithm, Argon2, Params, Version};
use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::format;
use crate::{CryptoError, Key};

const MIN_SALT_LEN: usize = 8;
const MAX_MEMORY_KIB: u32 = 4 * 1024 * 1024;
const MAX_ITERATIONS: u32 = 16;
const MAX_LANES: u32 = 16;

/// How much work deriving a key from a passphrase takes: the memory, iterations and lanes
/// of Argon2id.
///
/// In CBOR, the parameters are the array `[1, memory, iterations, lanes]`, with the memory
/// in KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassphraseParams {
	memory_kib: u32,
	iterations: u32,
	lanes: u32,
}

impl PassphraseParams {
	/// RFC 9106's first recommendation, for devices with enough memory: 2 GiB, one
	/// iteration and four lanes.
	pub const RECOMMENDED: Self = Self {
		memory_kib: 2 * 1024 * 1024,
		iterations: 1,
		lanes: 4,
	};

	/// RFC 9106's second recommendation, for devices with little memory: 64 MiB, three
	/// iterations and four lanes.
	pub const LOW_MEMORY: Self = Self {
		memory_kib: 64 * 1024,
		iterations: 3,
		lanes: 4,
	};

	/// Returns the parameters, with the memory in KiB.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Passphrase`] unless there are 1 to 16 iterations and 1 to 16
	/// lanes, and the memory is at least 8 KiB per lane, as Argon2 requires, and at most
	/// 4 GiB. The upper limits keep a damaged or hostile record from making a device
	/// allocate unlimited memory or work for ever.
	pub fn new(memory_kib: u32, iterations: u32, lanes: u32) -> Result<Self, CryptoError> {
		if !(1..=MAX_ITERATIONS).contains(&iterations) {
			return Err(CryptoError::Passphrase("there must be 1 to 16 iterations"));
		}
		if !(1..=MAX_LANES).contains(&lanes) {
			return Err(CryptoError::Passphrase("there must be 1 to 16 lanes"));
		}
		if memory_kib < 8 * lanes || memory_kib > MAX_MEMORY_KIB {
			return Err(CryptoError::Passphrase(
				"the memory must be at least 8 KiB per lane and at most 4 GiB",
			));
		}
		Ok(Self {
			memory_kib,
			iterations,
			lanes,
		})
	}

	/// The memory, in KiB.
	#[must_use]
	pub const fn memory_kib(self) -> u32 {
		self.memory_kib
	}

	/// The number of iterations.
	#[must_use]
	pub const fn iterations(self) -> u32 {
		self.iterations
	}

	/// The number of lanes.
	#[must_use]
	pub const fn lanes(self) -> u32 {
		self.lanes
	}
}

impl Key {
	/// Derives a key from a passphrase with Argon2id, version 1.3. The passphrase is first
	/// brought into Unicode Normalization Form C, so that it gives the same key however it
	/// was typed. NibKeep uses 16 random bytes of salt.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Passphrase`] if the salt is shorter than 8 bytes, or if the
	/// passphrase or the salt is longer than Argon2 allows, 4 GiB, and
	/// [`CryptoError::OutOfMemory`] if the device cannot provide the memory that the
	/// parameters ask for.
	pub fn from_passphrase(
		passphrase: &str,
		salt: &[u8],
		params: &PassphraseParams,
	) -> Result<Self, CryptoError> {
		if salt.len() < MIN_SALT_LEN {
			return Err(CryptoError::Passphrase(
				"the salt must have at least 8 bytes",
			));
		}
		// Reserve enough room up front, so that no unwiped copy is left behind when the
		// string grows. Normalization makes UTF-8 text at most three times longer.
		let capacity = passphrase
			.len()
			.checked_mul(3)
			.ok_or(CryptoError::Passphrase("the passphrase is too long"))?;
		let mut normalized = Zeroizing::new(String::with_capacity(capacity));
		normalized.extend(passphrase.nfc());
		let params = Params::new(params.memory_kib, params.iterations, params.lanes, Some(32))
			.map_err(|_| CryptoError::Passphrase("the parameters are not valid"))?;
		let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
		Self::fill_with(|bytes| {
			argon2
				.hash_password_into(normalized.as_bytes(), salt, bytes)
				.map_err(|error| match error {
					argon2::Error::OutOfMemory => CryptoError::OutOfMemory,
					_ => CryptoError::Passphrase("the passphrase or the salt is too long"),
				})
		})
	}
}

impl ToCbor for PassphraseParams {
	fn to_cbor(&self) -> Value {
		format::write([
			self.memory_kib.to_cbor(),
			self.iterations.to_cbor(),
			self.lanes.to_cbor(),
		])
	}
}

impl FromCbor for PassphraseParams {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "passphrase parameters: [1, memory, iterations, lanes]";
		let error = || cbor::Error::Expected(EXPECTED);
		let [memory, iterations, lanes] = format::read(value, EXPECTED)?;
		let number = |item: &Value| u32::from_cbor(item).map_err(|_| error());
		Self::new(number(memory)?, number(iterations)?, number(lanes)?).map_err(|_| error())
	}
}
