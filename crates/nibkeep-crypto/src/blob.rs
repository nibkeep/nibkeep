//! Sealed blobs: content encrypted in chunks with a key of its own, so that a reader can
//! start anywhere, and purging can destroy the key.

use std::convert::Infallible;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce, Tag};
use nibkeep_base::{Hash, Hasher};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::{CryptoError, Key};

/// The suite of sealed blobs in version 1 of the specification.
const SUITE: u8 = 1;

/// The context of the key, derived from a blob key and a sealed blob's nonce, that
/// encrypts the chunks.
const CHUNKS_CONTEXT: &str = "nibkeep 2026-10 blob chunks";

/// How many bytes of content a chunk holds. The last chunk may hold fewer.
pub const BLOB_CHUNK_LEN: usize = 65_536;

/// [`BLOB_CHUNK_LEN`], for positions in a blob.
const CHUNK_LEN: u64 = BLOB_CHUNK_LEN as u64;

/// The length of the random nonce that follows the suite.
const NONCE_LEN: usize = 16;

/// The length of the suite and the nonce.
const HEADER_LEN: u64 = 1 + NONCE_LEN as u64;

/// The length of a chunk's tag.
const TAG_BYTES: usize = 16;

/// [`TAG_BYTES`], for positions in a blob.
const TAG_LEN: u64 = TAG_BYTES as u64;

/// The length of an encrypted chunk that is not the last: a whole chunk and its tag.
const SEALED_CHUNK_LEN: u64 = CHUNK_LEN + TAG_LEN;

/// What sealing tells about a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedBlob {
	/// The hash of the content.
	pub content: Hash,
	/// The hash of the sealed blob.
	pub sealed: Hash,
	/// The length of the content, in bytes.
	pub len: u64,
}

/// Seals the content that `content` yields with a blob key, and writes the sealed blob to
/// `sealed`. Every sealed blob gets a random nonce, so no two are alike, even with the same
/// key and content. `docs/spec/crypto.md` describes the format.
///
/// # Errors
///
/// Returns the errors of reading the content and writing the sealed blob.
pub fn seal_blob(
	key: &Key,
	mut content: impl Read,
	mut sealed: impl Write,
	rng: &mut impl CryptoRng,
) -> io::Result<SealedBlob> {
	let mut nonce = [0; NONCE_LEN];
	rng.fill_bytes(&mut nonce);
	let cipher = cipher(key, &nonce);
	let mut content_hash = Hasher::new();
	let mut sealed_hash = Hasher::new();
	let mut write = |bytes: &[u8]| {
		sealed_hash.update(bytes);
		sealed.write_all(bytes)
	};
	write(&[SUITE])?;
	write(&nonce)?;
	let mut current = Zeroizing::new(vec![0; BLOB_CHUNK_LEN]);
	let mut next = Zeroizing::new(vec![0; BLOB_CHUNK_LEN]);
	let mut current_len = fill(&mut content, &mut current)?;
	let mut len = 0;
	for index in 0.. {
		// A chunk is the last one when nothing follows it, so that the last chunk is empty
		// only when the whole content is.
		let next_len = if current_len == BLOB_CHUNK_LEN {
			fill(&mut content, &mut next)?
		} else {
			0
		};
		let last = next_len == 0;
		let chunk = &mut current[..current_len];
		content_hash.update(chunk);
		let tag = cipher
			.encrypt_inout_detached(&chunk_nonce(index, last), &[], (&mut *chunk).into())
			.map_err(|_| io::Error::other("a chunk is too long to encrypt"))?;
		write(chunk)?;
		write(&tag)?;
		len += current_len as u64;
		if last {
			break;
		}
		std::mem::swap(&mut current, &mut next);
		current_len = next_len;
	}
	Ok(SealedBlob {
		content: content_hash.finalize(),
		sealed: sealed_hash.finalize(),
		len,
	})
}

/// Reads the content of a sealed blob, from any position.
///
/// A reader that reads the whole content in order, possibly reading some parts again,
/// checks the content hash as it reaches the end, and the read that reaches it fails if
/// the hash differs. A reader that reads only a part relies on the tags of the chunks it
/// reads. Damage, and a wrong key or content hash, are errors of the kind
/// [`io::ErrorKind::InvalidData`].
pub struct BlobReader<R> {
	sealed: R,
	cipher: ChaCha20Poly1305,
	content: Hash,
	/// The length of the content.
	len: u64,
	/// The length of the sealed blob.
	sealed_len: u64,
	/// The number of chunks.
	chunks: u64,
	position: u64,
	/// Room for one encrypted chunk.
	buffer: Zeroizing<Vec<u8>>,
	/// The chunk decrypted last: its index, and the length of its content, which is at the
	/// start of `buffer`.
	chunk: Option<(u64, usize)>,
	/// The hash of the content read in order from the start, and how much of it that is.
	hasher: Hasher,
	hashed: u64,
	/// Whether the content hash was checked.
	checked: bool,
}

impl<R: Read + Seek> BlobReader<R> {
	/// Prepares to read a sealed blob with its blob key and content hash. Checks the suite
	/// and the length of the sealed blob, but no chunk yet.
	///
	/// # Errors
	///
	/// Returns an error if the sealed blob cannot be read, or its suite or length is not
	/// that of a sealed blob.
	pub fn new(key: &Key, content: Hash, mut sealed: R) -> io::Result<Self> {
		let sealed_len = sealed.seek(SeekFrom::End(0))?;
		// The header, then whole chunks, then a last chunk that holds at least its tag,
		// and more unless it is the only one.
		let body = sealed_len.checked_sub(HEADER_LEN).ok_or_else(damaged)?;
		let chunks = body.div_ceil(SEALED_CHUNK_LEN);
		let last = body - chunks.saturating_sub(1) * SEALED_CHUNK_LEN;
		if chunks == 0 || last < TAG_LEN || (chunks > 1 && last == TAG_LEN) {
			return Err(damaged());
		}
		let mut suite = [0];
		let mut nonce = [0; NONCE_LEN];
		sealed.seek(SeekFrom::Start(0))?;
		sealed.read_exact(&mut suite)?;
		sealed.read_exact(&mut nonce)?;
		if suite != [SUITE] {
			return Err(damaged());
		}
		let room = usize::try_from(body.min(SEALED_CHUNK_LEN)).map_err(|_| damaged())?;
		Ok(Self {
			sealed,
			cipher: cipher(key, &nonce),
			content,
			len: body - chunks * TAG_LEN,
			sealed_len,
			chunks,
			position: 0,
			buffer: Zeroizing::new(vec![0; room]),
			chunk: None,
			hasher: Hasher::new(),
			hashed: 0,
			checked: false,
		})
	}

	/// The length of the content, in bytes.
	#[must_use]
	pub const fn len(&self) -> u64 {
		self.len
	}

	/// Whether the content is empty.
	#[must_use]
	pub const fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Decrypts a chunk into the buffer, unless it is there already.
	fn decrypt(&mut self, index: u64) -> io::Result<()> {
		if self.chunk.is_some_and(|(decrypted, _)| decrypted == index) {
			return Ok(());
		}
		self.chunk = None;
		let start = HEADER_LEN + index * SEALED_CHUNK_LEN;
		let last = index + 1 == self.chunks;
		let len = if last {
			self.sealed_len - start
		} else {
			SEALED_CHUNK_LEN
		};
		let encrypted = &mut self.buffer[..usize::try_from(len).map_err(|_| damaged())?];
		self.sealed.seek(SeekFrom::Start(start))?;
		self.sealed.read_exact(encrypted).map_err(|error| {
			if error.kind() == io::ErrorKind::UnexpectedEof {
				damaged()
			} else {
				error
			}
		})?;
		let (chunk, tag) = encrypted.split_at_mut(encrypted.len() - TAG_BYTES);
		let tag = Tag::try_from(&*tag).map_err(|_| damaged())?;
		self.cipher
			.decrypt_inout_detached(&chunk_nonce(index, last), &[], (&mut *chunk).into(), &tag)
			.map_err(|_| damaged())?;
		self.chunk = Some((index, chunk.len()));
		Ok(())
	}

	/// Checks the content hash, once all of the content was read in order.
	fn check(&mut self) -> io::Result<()> {
		if self.checked {
			return Ok(());
		}
		if self.len == 0 {
			// No chunk was read, so the empty chunk's tag is checked here.
			self.decrypt(0)?;
		}
		if self.hasher.finalize() != self.content {
			return Err(damaged());
		}
		self.checked = true;
		Ok(())
	}
}

impl<R: Read + Seek> Read for BlobReader<R> {
	fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		if self.position >= self.len {
			if self.hashed == self.len {
				self.check()?;
			}
			return Ok(0);
		}
		let start = self.position;
		self.decrypt(start / CHUNK_LEN)?;
		let Some((_, chunk_len)) = self.chunk else {
			unreachable!("the chunk was just decrypted");
		};
		let offset = usize::try_from(start % CHUNK_LEN).expect("below the chunk length");
		let read = buffer.len().min(chunk_len - offset);
		buffer[..read].copy_from_slice(&self.buffer[offset..offset + read]);
		self.position += read as u64;
		// The hash covers the content read in order from the start, so it takes the part of
		// this read that lies beyond what it covers, if this read reaches that far.
		if start <= self.hashed && self.hashed < self.position {
			let new = usize::try_from(self.hashed - start).expect("below the chunk length");
			self.hasher.update(&buffer[new..read]);
			self.hashed = self.position;
			if self.hashed == self.len {
				self.check()?;
			}
		}
		Ok(read)
	}
}

impl<R: Read + Seek> Seek for BlobReader<R> {
	fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
		let target = match position {
			SeekFrom::Start(offset) => Some(offset),
			SeekFrom::End(offset) => self.len.checked_add_signed(offset),
			SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
		};
		self.position = target.ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::InvalidInput,
				"cannot seek before the start of a blob",
			)
		})?;
		Ok(self.position)
	}
}

impl<R> fmt::Debug for BlobReader<R> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("BlobReader")
			.field("content", &self.content)
			.field("len", &self.len)
			.field("position", &self.position)
			.finish_non_exhaustive()
	}
}

/// The cipher of the chunks: ChaCha20-Poly1305, under the key derived from the blob key
/// followed by the nonce.
fn cipher(key: &Key, nonce: &[u8; NONCE_LEN]) -> ChaCha20Poly1305 {
	let Ok(chunks_key) = Key::fill_with(|bytes| {
		blake3::Hasher::new_derive_key(CHUNKS_CONTEXT)
			.update(key.expose_bytes())
			.update(nonce)
			.finalize_xof()
			.fill(bytes);
		Ok::<(), Infallible>(())
	});
	ChaCha20Poly1305::new(chunks_key.expose_bytes().into())
}

/// The nonce of a chunk: its index as an 11-byte big-endian number, then whether it is the
/// last chunk.
fn chunk_nonce(index: u64, last: bool) -> Nonce {
	let mut nonce = [0; 12];
	nonce[3..11].copy_from_slice(&index.to_be_bytes());
	nonce[11] = u8::from(last);
	Nonce::from(nonce)
}

/// Reads until `buffer` is full or the content ends. Returns how much was read.
fn fill(content: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
	let mut filled = 0;
	while filled < buffer.len() {
		match content.read(&mut buffer[filled..]) {
			Ok(0) => break,
			Ok(read) => filled += read,
			Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
			Err(error) => return Err(error),
		}
	}
	Ok(filled)
}

fn damaged() -> io::Error {
	io::Error::new(io::ErrorKind::InvalidData, CryptoError::Decryption)
}
