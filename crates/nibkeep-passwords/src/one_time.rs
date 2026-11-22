//! One-time codes that change with time (TOTP, RFC 6238), from the secret that a site shows
//! when a person turns them on: as base32 text, or as an `otpauth://` link in a QR code, in
//! the format that Google Authenticator set ("Key Uri Format").

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use zeroize::{Zeroize, Zeroizing};

use crate::base32;

/// The hash function of a one-time code's HMAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
	/// HMAC-SHA-1, which nearly every site uses.
	Sha1,
	/// HMAC-SHA-256.
	Sha256,
	/// HMAC-SHA-512.
	Sha512,
}

/// Why a secret or a link cannot make one-time codes. No message shows the secret.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OneTimeError {
	/// There is no secret.
	#[error("there is no secret for one-time codes")]
	NoSecret,
	/// The secret is not base32.
	#[error("the secret for one-time codes is not base32, as sites show it")]
	NotBase32,
	/// An `otpauth://` link of a kind that is not known.
	#[error("the link is not one for one-time codes")]
	NotALink,
	/// Codes counted by use rather than by time (HOTP).
	#[error("one-time codes counted by use, rather than by time, are not supported")]
	CounterBased,
	/// A hash function other than SHA-1, SHA-256 and SHA-512.
	#[error("one-time codes use SHA1, SHA256 or SHA512")]
	Algorithm,
	/// Codes with fewer than 6 digits or more than 10.
	#[error("one-time codes have from 6 to 10 digits")]
	Digits,
	/// A period of no seconds.
	#[error("one-time codes last a second or more")]
	Period,
}

/// What makes one-time codes: a secret, and how codes are made from it.
#[derive(Clone)]
pub struct OneTimeCode {
	secret: Zeroizing<Vec<u8>>,
	algorithm: Algorithm,
	digits: u32,
	period: u64,
	issuer: Option<String>,
	account: Option<String>,
}

impl fmt::Debug for OneTimeCode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("OneTimeCode")
			.field("algorithm", &self.algorithm)
			.field("digits", &self.digits)
			.field("period", &self.period)
			.field("issuer", &self.issuer)
			.field("account", &self.account)
			.finish_non_exhaustive()
	}
}

impl OneTimeCode {
	/// Codes from `secret`, of `digits` digits, each lasting `period` seconds.
	///
	/// # Errors
	///
	/// Returns an error if the secret is empty, the digits are fewer than 6 or more than
	/// 10, or the period is 0.
	pub fn new(
		secret: Vec<u8>,
		algorithm: Algorithm,
		digits: u32,
		period: u64,
	) -> Result<Self, OneTimeError> {
		let secret = Zeroizing::new(secret);
		if secret.is_empty() {
			return Err(OneTimeError::NoSecret);
		}
		if !(6..=10).contains(&digits) {
			return Err(OneTimeError::Digits);
		}
		if period == 0 {
			return Err(OneTimeError::Period);
		}
		Ok(Self {
			secret,
			algorithm,
			digits,
			period,
			issuer: None,
			account: None,
		})
	}

	/// Reads the secret as a site shows it: base32 text, which makes codes of 6 digits for 30
	/// seconds with SHA-1, or an `otpauth://totp/` link, which may say otherwise and name the
	/// site and the account.
	///
	/// # Errors
	///
	/// Returns an error if the text is neither, or the link asks for codes that cannot be
	/// made. No error shows the secret.
	pub fn parse(text: &str) -> Result<Self, OneTimeError> {
		let text = text.trim();
		let Some(rest) = strip_prefix_ignoring_case(text, "otpauth://") else {
			return Self::new(secret(text)?.to_vec(), Algorithm::Sha1, 6, 30);
		};
		let (kind, rest) = rest.split_once('/').unwrap_or((rest, ""));
		match kind.to_ascii_lowercase().as_str() {
			"totp" => {}
			"hotp" => return Err(OneTimeError::CounterBased),
			_ => return Err(OneTimeError::NotALink),
		}
		let (label, query) = rest.split_once('?').unwrap_or((rest, ""));
		let label = percent_decoded(label);
		let (label_issuer, account) = match label.split_once(':') {
			Some((issuer, account)) => (Some(issuer.trim()), account.trim()),
			None => (None, label.trim()),
		};
		let mut found = None;
		let mut algorithm = Algorithm::Sha1;
		let mut digits = 6;
		let mut period = 30;
		let mut issuer = label_issuer.map(str::to_owned);
		for pair in query.split('&') {
			let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
			let value = percent_decoded(value);
			match name.to_ascii_lowercase().as_str() {
				"secret" => found = Some(secret(&value)?),
				"issuer" if !value.is_empty() => issuer = Some(value.to_string()),
				"algorithm" => {
					algorithm = match value.to_ascii_uppercase().as_str() {
						"SHA1" => Algorithm::Sha1,
						"SHA256" => Algorithm::Sha256,
						"SHA512" => Algorithm::Sha512,
						_ => return Err(OneTimeError::Algorithm),
					};
				}
				"digits" => digits = value.parse().map_err(|_| OneTimeError::Digits)?,
				"period" => period = value.parse().map_err(|_| OneTimeError::Period)?,
				_ => {}
			}
		}
		let found = found.ok_or(OneTimeError::NoSecret)?;
		let mut code = Self::new(found.to_vec(), algorithm, digits, period)?;
		code.issuer = issuer.filter(|issuer| !issuer.is_empty());
		code.account = Some(account.to_owned()).filter(|account| !account.is_empty());
		Ok(code)
	}

	/// Checks text that a person gives as the secret of one-time codes, before it is kept:
	/// an `otpauth://` link, of any kind, as a site's QR code holds it, or a key in base32.
	/// Codes are made only from links of the kind `totp` and from keys.
	///
	/// # Errors
	///
	/// Returns an error if the text is neither. No error shows the secret.
	pub fn check(text: &str) -> Result<(), OneTimeError> {
		let text = text.trim();
		if strip_prefix_ignoring_case(text, "otpauth://").is_some() {
			return Ok(());
		}
		secret(text).map(|_| ())
	}

	/// The code at a moment, in seconds since 1970 (UTC).
	#[must_use]
	pub fn code_at(&self, unix_seconds: u64) -> Zeroizing<String> {
		hotp(
			&self.secret,
			self.algorithm,
			unix_seconds / self.period,
			self.digits,
		)
	}

	/// How many seconds the code at a moment still lasts, from 1 to the period.
	#[must_use]
	pub fn seconds_left(&self, unix_seconds: u64) -> u64 {
		self.period - unix_seconds % self.period
	}

	/// The hash function of its HMAC.
	#[must_use]
	pub fn algorithm(&self) -> Algorithm {
		self.algorithm
	}

	/// How many digits its codes have.
	#[must_use]
	pub fn digits(&self) -> u32 {
		self.digits
	}

	/// How many seconds each code lasts.
	#[must_use]
	pub fn period(&self) -> u64 {
		self.period
	}

	/// The site or company that the link names, if it does.
	#[must_use]
	pub fn issuer(&self) -> Option<&str> {
		self.issuer.as_deref()
	}

	/// The account that the link names, if it does.
	#[must_use]
	pub fn account(&self) -> Option<&str> {
		self.account.as_deref()
	}
}

/// The code for a counter (HOTP, RFC 4226), of `digits` digits, which may start with 0.
///
/// # Panics
///
/// Panics if `digits` is more than 10, which [`OneTimeCode`] never asks for.
#[must_use]
pub fn hotp(secret: &[u8], algorithm: Algorithm, counter: u64, digits: u32) -> Zeroizing<String> {
	assert!(digits <= 10, "one-time codes have 10 digits at most");
	let counter = counter.to_be_bytes();
	let hash: Zeroizing<Vec<u8>> = Zeroizing::new(match algorithm {
		Algorithm::Sha1 => mac::<Hmac<sha1::Sha1>>(secret, &counter),
		Algorithm::Sha256 => mac::<Hmac<sha2::Sha256>>(secret, &counter),
		Algorithm::Sha512 => mac::<Hmac<sha2::Sha512>>(secret, &counter),
	});
	// Dynamic truncation: four bytes from where the last byte's low bits say.
	let at = usize::from(hash[hash.len() - 1] & 0x0f);
	let mut number = u32::from_be_bytes([hash[at], hash[at + 1], hash[at + 2], hash[at + 3]]);
	number &= 0x7fff_ffff;
	let modulus = 10_u64.pow(digits);
	let code = u64::from(number) % modulus;
	let width = usize::try_from(digits).expect("ten at most");
	Zeroizing::new(format!("{code:0width$}"))
}

/// The HMAC of a message; its state, which holds what is derived from the key, is wiped as
/// it is dropped, and so is its output once copied.
fn mac<M: Mac + KeyInit>(key: &[u8], message: &[u8]) -> Vec<u8> {
	let mut mac = <M as KeyInit>::new_from_slice(key).expect("HMAC takes keys of any length");
	mac.update(message);
	let mut output = mac.finalize().into_bytes();
	let copy = output.to_vec();
	output.as_mut_slice().zeroize();
	copy
}

/// A secret as base32 text.
fn secret(text: &str) -> Result<Zeroizing<Vec<u8>>, OneTimeError> {
	let bytes = base32::decode(text).ok_or(OneTimeError::NotBase32)?;
	if bytes.is_empty() {
		return Err(OneTimeError::NoSecret);
	}
	Ok(bytes)
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
	let head = text.get(..prefix.len())?;
	head.eq_ignore_ascii_case(prefix)
		.then(|| &text[prefix.len()..])
}

/// Text with its percent escapes decoded, as UTF-8: `%` and two hexadecimal digits; anything
/// else, `+` too, stays as it is.
fn percent_decoded(text: &str) -> Zeroizing<String> {
	let bytes = text.as_bytes();
	let mut decoded = Zeroizing::new(Vec::with_capacity(bytes.len()));
	let mut at = 0;
	while at < bytes.len() {
		let escaped = (bytes[at] == b'%')
			.then(|| bytes.get(at + 1..at + 3))
			.flatten()
			.filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
			.and_then(|hex| std::str::from_utf8(hex).ok())
			.and_then(|hex| u8::from_str_radix(hex, 16).ok());
		if let Some(byte) = escaped {
			decoded.push(byte);
			at += 3;
		} else {
			decoded.push(bytes[at]);
			at += 1;
		}
	}
	Zeroizing::new(String::from_utf8_lossy(&decoded).into_owned())
}
