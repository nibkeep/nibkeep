//! Proving that a replica holds a sealed blob now, before another removes its own copy: the
//! remover sends a fresh random key and a few random ranges of the sealed bytes, and the
//! prover answers with the BLAKE3 hash of those ranges keyed with that key, which the remover
//! checks against its own copy ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#3-syncing)). A plain
//! challenge asks the same of the content itself, which the device that holds an original
//! where it was found reads from that file, and the remover from its sealed copy, opened.

use std::io;

use nibkeep_base::Hash;
use nibkeep_crypto::rand_core::CryptoRng;

/// How many ranges a challenge reads of a blob larger than they take together.
pub const RANGES: usize = 8;

/// How many bytes each range of a challenge takes, but in a blob smaller than all of them.
pub const RANGE_LEN: u32 = 1024;

/// How many ranges a challenge may name at most, as the prover reads them: those that a
/// remover draws.
pub const MOST_RANGES: usize = RANGES;

/// How many bytes a range may take at most: the whole of a blob that a remover reads in one
/// range, so that a prover reads at most 64 KiB for each challenge.
pub const MOST_RANGE_LEN: u32 = 8 * RANGE_LEN;

const _: () = assert!(MOST_RANGE_LEN as usize == RANGES * RANGE_LEN as usize);

/// How many challenges one message carries at most.
pub const CHALLENGES_AT_ONCE: usize = 1024;

/// The context of a proof's hash, which goes into it before the ranges.
const CONTEXT: &[u8] = b"nibkeep 2026-11 proof of a sealed blob";

/// The context of a plain proof's hash: of a content's own bytes.
const PLAIN_CONTEXT: &[u8] = b"nibkeep 2026-11 proof of a plain copy";

/// A range of a sealed blob's bytes: where it starts, and how many bytes it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
	/// Its first byte's offset.
	pub offset: u64,
	/// How many bytes it takes, at least 1.
	pub len: u32,
}

/// What a remover asks a prover to answer of a sealed blob, or of a content's own bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
	/// The sealed blob's sealed hash, or for a plain challenge the content's hash.
	pub sealed: Hash,
	/// The key of the hash, fresh for each challenge.
	pub key: [u8; 32],
	/// The ranges of the bytes to hash, in order.
	pub ranges: Vec<Range>,
	/// Whether it asks for the content's own bytes, as an original holds them, rather than
	/// a sealed blob's.
	pub plain: bool,
}

impl Challenge {
	/// A challenge of a sealed blob of `size` bytes: a fresh key, and [`RANGES`] random
	/// ranges of [`RANGE_LEN`] bytes, in order of their offsets, or the whole blob if it is
	/// no larger than those take.
	#[must_use]
	pub fn random(sealed: Hash, size: u64, rng: &mut (impl CryptoRng + ?Sized)) -> Self {
		Self::drawn(sealed, size, false, rng)
	}

	/// A plain challenge of a content of `len` bytes, as [`Challenge::random`] draws one.
	#[must_use]
	pub fn random_plain(content: Hash, len: u64, rng: &mut (impl CryptoRng + ?Sized)) -> Self {
		Self::drawn(content, len, true, rng)
	}

	fn drawn(sealed: Hash, size: u64, plain: bool, rng: &mut (impl CryptoRng + ?Sized)) -> Self {
		let mut key = [0; 32];
		rng.fill_bytes(&mut key);
		let all = RANGES as u64 * u64::from(RANGE_LEN);
		let ranges = if size == 0 && plain {
			// An empty content has no bytes to read, and its hash alone is asked for.
			Vec::new()
		} else if size <= all {
			// A sealed blob is never empty: its header comes first.
			vec![Range {
				offset: 0,
				len: u32::try_from(size.max(1)).unwrap_or(RANGE_LEN),
			}]
		} else {
			let room = size - u64::from(RANGE_LEN) + 1;
			let mut offsets: Vec<u64> = (0..RANGES)
				.map(|_| {
					let mut bytes = [0; 8];
					rng.fill_bytes(&mut bytes);
					u64::from_le_bytes(bytes) % room
				})
				.collect();
			offsets.sort_unstable();
			offsets
				.into_iter()
				.map(|offset| Range {
					offset,
					len: RANGE_LEN,
				})
				.collect()
		};
		Self {
			sealed,
			key,
			ranges,
			plain,
		}
	}

	/// Whether a prover reads it: no more than [`MOST_RANGES`] ranges, each of 1 to
	/// [`MOST_RANGE_LEN`] bytes, none ending past the largest offset; none only for a plain
	/// challenge, of an empty content.
	#[must_use]
	pub fn is_fair(&self) -> bool {
		(self.plain || !self.ranges.is_empty())
			&& self.ranges.len() <= MOST_RANGES
			&& self.ranges.iter().all(|range| {
				range.len >= 1
					&& range.len <= MOST_RANGE_LEN
					&& range.offset.checked_add(u64::from(range.len)).is_some()
			})
	}

	/// The answer to the challenge, from the bytes as `read` reads each range, into a buffer
	/// of the range's length: the BLAKE3 hash, keyed with the challenge's key, of the context,
	/// which a plain challenge's differs from, the hash it names, and each range's offset,
	/// length and bytes.
	///
	/// # Errors
	///
	/// Returns what `read` returns, as when the blob ends before a range does.
	pub fn answer(
		&self,
		mut read: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
	) -> io::Result<[u8; 32]> {
		let mut hasher = blake3::Hasher::new_keyed(&self.key);
		hasher.update(if self.plain { PLAIN_CONTEXT } else { CONTEXT });
		hasher.update(&self.sealed.to_multihash());
		let mut buffer = Vec::new();
		for range in &self.ranges {
			buffer.resize(range.len as usize, 0);
			read(range.offset, &mut buffer)?;
			hasher.update(&range.offset.to_be_bytes());
			hasher.update(&range.len.to_be_bytes());
			hasher.update(&buffer);
		}
		Ok(*hasher.finalize().as_bytes())
	}

	/// The answer from the blob's bytes in memory, or `None` if a range ends past them.
	#[must_use]
	pub fn answer_from(&self, bytes: &[u8]) -> Option<[u8; 32]> {
		self.answer(|offset, out| {
			let start = usize::try_from(offset).map_err(io::Error::other)?;
			let found = start
				.checked_add(out.len())
				.and_then(|end| bytes.get(start..end))
				.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
			out.copy_from_slice(found);
			Ok(())
		})
		.ok()
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn a_large_blob_is_read_in_a_few_ranges_and_a_small_one_whole() {
		let mut rng = TestRng::new("ranges");
		let sealed = Hash::of(b"sealed");
		let large = Challenge::random(sealed, 10_000_000, &mut rng);
		assert_eq!(large.ranges.len(), RANGES);
		assert!(large.is_fair());
		assert!(
			large
				.ranges
				.windows(2)
				.all(|pair| pair[0].offset <= pair[1].offset)
		);
		assert!(
			large
				.ranges
				.iter()
				.all(|range| range.offset + u64::from(range.len) <= 10_000_000)
		);
		let small = Challenge::random(sealed, 5_000, &mut rng);
		assert_eq!(
			small.ranges,
			vec![Range {
				offset: 0,
				len: 5_000
			}]
		);
	}

	#[test]
	fn only_the_same_bytes_answer_a_challenge() {
		let mut rng = TestRng::new("answers");
		let bytes: Vec<u8> = (0..100_000_u32).map(|index| (index % 251) as u8).collect();
		let challenge = Challenge::random(Hash::of(&bytes), bytes.len() as u64, &mut rng);
		let answer = challenge.answer_from(&bytes).expect("answers");
		assert_eq!(challenge.answer_from(&bytes), Some(answer));
		// Another key gives another answer, so an answer cannot be kept for later.
		let other = Challenge::random(Hash::of(&bytes), bytes.len() as u64, &mut rng);
		assert_ne!(other.answer_from(&bytes), Some(answer));
		// A byte changed in a range changes it; a blob that is cut short answers nothing.
		let mut changed = bytes.clone();
		let at = usize::try_from(challenge.ranges[3].offset).expect("small") + 10;
		changed[at] ^= 1;
		assert_ne!(challenge.answer_from(&changed), Some(answer));
		let last = challenge.ranges.last().expect("ranges");
		let cut = usize::try_from(last.offset).expect("small");
		assert_eq!(challenge.answer_from(&bytes[..cut]), None);
	}

	#[test]
	fn a_plain_answer_is_not_a_sealed_one() {
		let mut rng = TestRng::new("plain");
		let bytes: Vec<u8> = (0..30_000_u32).map(|index| (index % 249) as u8).collect();
		let plain = Challenge::random_plain(Hash::of(&bytes), bytes.len() as u64, &mut rng);
		assert!(plain.plain);
		let sealed = Challenge {
			plain: false,
			..plain.clone()
		};
		assert_ne!(plain.answer_from(&bytes), sealed.answer_from(&bytes));
		// An empty content is asked for by its hash alone.
		let empty = Challenge::random_plain(Hash::of(b""), 0, &mut rng);
		assert!(empty.ranges.is_empty() && empty.is_fair());
		assert!(empty.answer_from(&[]).is_some());
	}
}
