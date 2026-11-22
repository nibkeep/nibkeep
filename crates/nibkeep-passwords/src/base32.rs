//! Base32 as RFC 4648 defines it, as sites show the secrets of one-time codes: in either
//! case, with or without padding, and with spaces or hyphens between groups.

use zeroize::{Zeroize, Zeroizing};

/// The bytes that base32 text stands for, or `None` if it is not base32. Spaces, hyphens and
/// the padding at the end are skipped.
pub(crate) fn decode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
	let mut bytes = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8));
	let mut buffer: u64 = 0;
	let mut bits = 0;
	let mut padding = false;
	for c in text.chars() {
		let value = match c.to_ascii_uppercase() {
			' ' | '-' => continue,
			'=' => {
				padding = true;
				continue;
			}
			// Padding comes only at the end.
			_ if padding => return None,
			upper @ 'A'..='Z' => u64::from(upper) - u64::from('A'),
			digit @ '2'..='7' => u64::from(digit) - u64::from('2') + 26,
			_ => return None,
		};
		buffer = (buffer << 5) | value;
		bits += 5;
		if bits >= 8 {
			bits -= 8;
			bytes.push(u8::try_from((buffer >> bits) & 0xff).expect("a byte"));
		}
	}
	// What is left of the secret in the buffer.
	buffer.zeroize();
	Some(bytes)
}

#[cfg(test)]
mod tests {
	use super::decode;

	#[test]
	fn decodes_the_vectors_of_rfc_4648() {
		let vectors = [
			("", ""),
			("MY======", "f"),
			("MZXQ====", "fo"),
			("MZXW6===", "foo"),
			("MZXW6YQ=", "foob"),
			("MZXW6YTB", "fooba"),
			("MZXW6YTBOI======", "foobar"),
		];
		for (text, bytes) in vectors {
			assert_eq!(
				decode(text).expect("base32").as_slice(),
				bytes.as_bytes(),
				"{text}"
			);
			let bare = text.trim_end_matches('=').to_ascii_lowercase();
			assert_eq!(
				decode(&bare).expect("base32").as_slice(),
				bytes.as_bytes(),
				"{bare}"
			);
		}
	}

	#[test]
	fn refuses_what_is_not_base32() {
		for text in ["MZ1W", "MZ8W", "MZ=W", "MZÄW"] {
			assert!(decode(text).is_none(), "{text}");
		}
	}
}
