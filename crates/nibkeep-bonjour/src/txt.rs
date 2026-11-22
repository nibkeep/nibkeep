//! TXT records as DNS-SD writes them ([RFC 6763, section 6]): strings of up to 255 bytes,
//! each after a byte that holds its length, each `key=value` or a key alone. A record with
//! no attribute holds one empty string, since DNS-SD requires at least one.
//!
//! [RFC 6763, section 6]: https://www.rfc-editor.org/rfc/rfc6763.html#section-6

/// The TXT record of these attributes, leaving out any too long for one string.
#[cfg_attr(not(any(target_os = "macos", target_os = "ios")), allow(dead_code))]
pub(crate) fn encode(attributes: &[(&str, &str)]) -> Vec<u8> {
	let mut record = Vec::new();
	for (key, value) in attributes {
		let entry = format!("{key}={value}");
		if let Ok(length) = u8::try_from(entry.len()) {
			record.push(length);
			record.extend_from_slice(entry.as_bytes());
		}
	}
	if record.is_empty() {
		record.push(0);
	}
	record
}

/// The attributes of a TXT record: those that are not UTF-8 or have an empty key are left
/// out, and of a key given twice, the first counts, as DNS-SD says.
#[cfg_attr(not(any(target_os = "macos", target_os = "ios")), allow(dead_code))]
pub(crate) fn decode(record: &[u8]) -> Vec<(String, Option<String>)> {
	let mut attributes: Vec<(String, Option<String>)> = Vec::new();
	let mut rest = record;
	while let Some((&length, after)) = rest.split_first() {
		let Some(entry) = after.get(..usize::from(length)) else {
			break;
		};
		rest = &after[usize::from(length)..];
		let Ok(entry) = std::str::from_utf8(entry) else {
			continue;
		};
		let (key, value) = match entry.split_once('=') {
			Some((key, value)) => (key, Some(value.to_owned())),
			None => (entry, None),
		};
		if key.is_empty() || attributes.iter().any(|(found, _)| found == key) {
			continue;
		}
		attributes.push((key.to_owned(), value));
	}
	attributes
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn attributes_go_there_and_back() {
		let record = encode(&[("announced", "nibkeep-pair-37"), ("v", "1")]);
		assert_eq!(&record[..20], b"\x19announced=nibkeep-p");
		assert_eq!(
			decode(&record),
			vec![
				("announced".to_owned(), Some("nibkeep-pair-37".to_owned())),
				("v".to_owned(), Some("1".to_owned())),
			]
		);
	}

	#[test]
	fn a_record_without_attributes_holds_one_empty_string() {
		assert_eq!(encode(&[]), vec![0]);
		assert_eq!(decode(&[0]), Vec::new());
	}

	#[test]
	fn the_first_of_a_key_counts_and_broken_entries_are_left_out() {
		let mut record = encode(&[("a", "1"), ("a", "2")]);
		record.extend_from_slice(b"\x04flag\x02\xff\xfe\x02=x");
		// A length that runs past the end ends the record.
		record.extend_from_slice(b"\x09short");
		assert_eq!(
			decode(&record),
			vec![
				("a".to_owned(), Some("1".to_owned())),
				("flag".to_owned(), None),
			]
		);
	}

	#[test]
	fn an_attribute_too_long_for_one_string_is_left_out() {
		let long = "x".repeat(300);
		assert_eq!(encode(&[("big", &long)]), vec![0]);
	}
}
