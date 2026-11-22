//! Helpers shared by the tests.

// Each test file uses only some of the helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::convert::Infallible;

use nibkeep_crypto::rand_core::{TryCryptoRng, TryRng};

/// Decodes hexadecimal text, ignoring whitespace.
pub(crate) fn hex(text: &str) -> Vec<u8> {
	let digits: Vec<u8> = text
		.bytes()
		.filter(|byte| !byte.is_ascii_whitespace())
		.collect();
	assert!(digits.len().is_multiple_of(2), "odd number of hex digits");
	digits
		.chunks(2)
		.map(|pair| {
			let pair = std::str::from_utf8(pair).expect("ASCII");
			u8::from_str_radix(pair, 16).expect("hex digits")
		})
		.collect()
}

/// A random number generator with a fixed seed, so that every run of a test sees the same
/// "random" bytes. It is BLAKE3's extendable output, which is as good as random for tests.
pub(crate) struct TestRng(blake3::OutputReader);

/// Returns a [`TestRng`] seeded with `seed`.
pub(crate) fn rng(seed: &str) -> TestRng {
	TestRng(blake3::Hasher::new().update(seed.as_bytes()).finalize_xof())
}

impl TryRng for TestRng {
	type Error = Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Infallible> {
		let mut bytes = [0; 4];
		self.0.fill(&mut bytes);
		Ok(u32::from_le_bytes(bytes))
	}

	fn try_next_u64(&mut self) -> Result<u64, Infallible> {
		let mut bytes = [0; 8];
		self.0.fill(&mut bytes);
		Ok(u64::from_le_bytes(bytes))
	}

	fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
		self.0.fill(destination);
		Ok(())
	}
}

impl TryCryptoRng for TestRng {}

/// A "random number generator" that hands out the given bytes, in order, to reproduce
/// published test vectors. It panics when it runs out.
pub(crate) struct FixedBytes {
	bytes: Vec<u8>,
	position: usize,
}

impl FixedBytes {
	pub(crate) fn new(bytes: Vec<u8>) -> Self {
		Self { bytes, position: 0 }
	}
}

impl TryRng for FixedBytes {
	type Error = Infallible;

	fn try_next_u32(&mut self) -> Result<u32, Infallible> {
		let mut bytes = [0; 4];
		self.try_fill_bytes(&mut bytes)?;
		Ok(u32::from_le_bytes(bytes))
	}

	fn try_next_u64(&mut self) -> Result<u64, Infallible> {
		let mut bytes = [0; 8];
		self.try_fill_bytes(&mut bytes)?;
		Ok(u64::from_le_bytes(bytes))
	}

	fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
		let end = self.position + destination.len();
		destination.copy_from_slice(
			self.bytes
				.get(self.position..end)
				.expect("the test supplied enough bytes"),
		);
		self.position = end;
		Ok(())
	}
}

impl TryCryptoRng for FixedBytes {}

/// Reads a file of test vectors: `name:` lines, each followed by lines of hexadecimal.
/// Lines starting with `#` are comments.
pub(crate) fn vectors(text: &str) -> BTreeMap<&str, Vec<u8>> {
	let mut vectors = BTreeMap::new();
	let mut name = None;
	let mut value = String::new();
	for line in text.lines().filter(|line| !line.starts_with('#')) {
		if let Some(next) = line.strip_suffix(':') {
			if let Some(name) = name.replace(next) {
				vectors.insert(name, hex(&value));
			}
			value.clear();
		} else {
			value.push_str(line);
		}
	}
	if let Some(name) = name {
		vectors.insert(name, hex(&value));
	}
	vectors
}
