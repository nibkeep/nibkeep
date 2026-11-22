//! Sealed records: the pages of the state file, each encrypted in place with XAES-256-GCM.

mod common;

use common::{FixedBytes, hex, rng};
use nibkeep_crypto::{CryptoError, Key, PAGE_NONCE_LEN, PAGE_TAG_LEN, PageCipher, RECORD_OVERHEAD};

/// The test vectors of XAES-256-GCM in C2SP: the key, the associated data, the ciphertext
/// and the tag of the plaintext `XAES-256-GCM` under the nonce `ABCDEFGHIJKLMNOPQRSTUVWX`.
const VECTORS: [(&str, &[u8], &str, &str); 2] = [
	(
		"0101010101010101010101010101010101010101010101010101010101010101",
		b"",
		"ce546ef63c9cc60765923609",
		"b33a9a1974e96e52daf2fcf7075e2271",
	),
	(
		"0303030303030303030303030303030303030303030303030303030303030303",
		b"c2sp.org/XAES-256-GCM",
		"986ec1832593df5443a17943",
		"7fd083bf3fdb41abd740a21f71eb769d",
	),
];

const NONCE: &[u8; 24] = b"ABCDEFGHIJKLMNOPQRSTUVWX";
const PLAINTEXT: &[u8] = b"XAES-256-GCM";

fn key(seed: &str) -> Key {
	Key::generate(&mut rng(seed))
}

/// A record with room for a nonce and a tag around `plaintext`.
fn record(plaintext: &[u8]) -> Vec<u8> {
	let mut record = vec![0; RECORD_OVERHEAD + plaintext.len()];
	record[PAGE_NONCE_LEN..PAGE_NONCE_LEN + plaintext.len()].copy_from_slice(plaintext);
	record
}

#[test]
fn a_record_is_its_nonce_ciphertext_and_tag_as_published() {
	assert_eq!(RECORD_OVERHEAD, PAGE_NONCE_LEN + PAGE_TAG_LEN);
	for (key_hex, associated, ciphertext, tag) in VECTORS {
		let cipher = PageCipher::new(&Key::from_slice(&hex(key_hex)).expect("32 bytes"));
		let mut sealed = record(PLAINTEXT);
		cipher.seal(
			&mut FixedBytes::new(NONCE.to_vec()),
			associated,
			&mut sealed,
		);
		let expected = [NONCE.as_slice(), &hex(ciphertext), &hex(tag)].concat();
		assert_eq!(sealed, expected);
		assert_eq!(cipher.open(associated, &mut sealed), Ok(PLAINTEXT));
	}
}

#[test]
fn a_record_opens_with_its_key_and_associated_data() {
	let cipher = PageCipher::new(&key("page key"));
	let page: Vec<u8> = (0..4096)
		.map(|i| u8::try_from(i % 251).expect("small"))
		.collect();
	let mut sealed = record(&page);
	cipher.seal(&mut rng("nonces"), b"D 7", &mut sealed);
	assert_ne!(
		&sealed[PAGE_NONCE_LEN..PAGE_NONCE_LEN + page.len()],
		page.as_slice()
	);
	assert_eq!(cipher.open(b"D 7", &mut sealed), Ok(page.as_slice()));
}

#[test]
fn a_record_does_not_open_with_another_key_place_or_byte() {
	let cipher = PageCipher::new(&key("page key"));
	let mut sealed = record(b"a page of the state file");
	cipher.seal(&mut rng("nonces"), b"D 7", &mut sealed);

	let other = PageCipher::new(&key("another key"));
	assert_eq!(
		other.open(b"D 7", &mut sealed.clone()),
		Err(CryptoError::Decryption)
	);
	assert_eq!(
		cipher.open(b"D 8", &mut sealed.clone()),
		Err(CryptoError::Decryption)
	);
	for at in [0, PAGE_NONCE_LEN, sealed.len() - 1] {
		let mut changed = sealed.clone();
		changed[at] ^= 1;
		assert_eq!(
			cipher.open(b"D 7", &mut changed),
			Err(CryptoError::Decryption),
			"byte {at} changed"
		);
	}
}

#[test]
fn every_seal_takes_a_new_nonce() {
	let cipher = PageCipher::new(&key("page key"));
	let mut rng = rng("nonces");
	let mut first = record(b"the same page");
	let mut second = record(b"the same page");
	cipher.seal(&mut rng, b"D 1", &mut first);
	cipher.seal(&mut rng, b"D 1", &mut second);
	assert_ne!(first[..PAGE_NONCE_LEN], second[..PAGE_NONCE_LEN]);
	assert_ne!(first, second);
}

#[test]
fn an_empty_record_authenticates_its_associated_data() {
	let cipher = PageCipher::new(&key("page key"));
	let mut sealed = record(b"");
	cipher.seal(&mut rng("nonces"), b"header", &mut sealed);
	assert_eq!(cipher.open(b"header", &mut sealed.clone()), Ok(&b""[..]));
	assert_eq!(
		cipher.open(b"headers", &mut sealed),
		Err(CryptoError::Decryption)
	);
}

#[test]
fn a_record_too_short_for_a_nonce_and_a_tag_does_not_open() {
	let cipher = PageCipher::new(&key("page key"));
	let mut short = vec![0; RECORD_OVERHEAD - 1];
	assert_eq!(cipher.open(b"", &mut short), Err(CryptoError::Decryption));
}

/// The test vector of the cryptography specification, section 8: the key whose bytes are
/// 0, 1, …, 31 seals `nibkeep` as page 5 of a database, under the nonce whose bytes are 32,
/// 33, …, 55.
#[test]
fn the_record_of_the_specification_seals_as_published() {
	let key = Key::from_bytes(std::array::from_fn(|index| {
		u8::try_from(index).expect("small")
	}));
	let mut associated = vec![b'D'];
	associated.extend_from_slice(&5_u64.to_le_bytes());
	let mut sealed = record(b"nibkeep");
	let nonce: Vec<u8> = (32..56).collect();
	PageCipher::new(&key).seal(&mut FixedBytes::new(nonce), &associated, &mut sealed);
	assert_eq!(
		sealed,
		hex("202122232425262728292a2b2c2d2e2f3031323334353637
			09793da98425a167052b9cd176b4416cd3ce94a5441a07")
	);
}
