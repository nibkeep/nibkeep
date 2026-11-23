//! Pairing a new replica, the joiner, with one that holds the library, the sponsor: the code
//! that the person types, the key that SPAKE2 agrees on from it, the joiner's request and
//! the sponsor's grant ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#2-pairing)).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{
	CryptoError, DecryptionKey, EncryptionKey, Key, Pake, PakeSide, SealedMessage, VerifyingKey,
};

/// What a sponsor adds to what it announces on the network while it pairs, followed by the
/// first two digits of the code.
pub const PAIRING_PREFIX: &str = "nibkeep-pair-";

/// What an admin adds to what it announces on the network while it invites someone to a
/// space, followed by the first two digits of the code: apart from pairing's, so that a
/// joiner never reaches a sponsor, nor a new replica an admin.
pub const INVITATION_PREFIX: &str = "nibkeep-invite-";

/// The context of the space keys sealed to the joiner.
pub(crate) const SPACE_KEYS: &str = "nibkeep 2026-10 space keys";

/// How long the name of a device or an app in a request may be, in bytes.
pub const MAX_NAME_LEN: usize = 200;

/// A code that pairs two replicas once: eight decimal digits, the first two from 10 to 99,
/// which find the sponsor on the network, and six more that are secret. It is shown as
/// `37-482-177`, and never printed in full by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode([u8; 8]);

impl PairingCode {
	/// A new random code.
	pub fn generate(rng: &mut impl CryptoRng) -> Self {
		let mut digits = [0; 8];
		digits[0] = random_below(rng, 9) + 1;
		for digit in &mut digits[1..] {
			*digit = random_below(rng, 10);
		}
		Self(digits)
	}

	/// The code's first two digits, from 10 to 99, which find the sponsor.
	#[must_use]
	pub fn nameplate(&self) -> u8 {
		self.0[0] * 10 + self.0[1]
	}

	/// What the sponsor announces on the network while it pairs with this code.
	#[must_use]
	pub fn announcement(&self) -> String {
		format!("{PAIRING_PREFIX}{}", self.nameplate())
	}

	/// What an admin announces on the network while it invites someone with this code.
	#[must_use]
	pub fn invitation_announcement(&self) -> String {
		format!("{INVITATION_PREFIX}{}", self.nameplate())
	}

	/// SPAKE2's password: the eight digits in ASCII.
	fn password(&self) -> [u8; 8] {
		self.0.map(|digit| b'0' + digit)
	}
}

/// A random number below `bound`, which is at most 10, without bias.
fn random_below(rng: &mut impl CryptoRng, bound: u8) -> u8 {
	// The largest multiple of `bound` that a byte holds: bytes at or above it are drawn again.
	let limit = 256 - 256 % u16::from(bound);
	loop {
		let mut byte = [0];
		rng.fill_bytes(&mut byte);
		if u16::from(byte[0]) < limit {
			return byte[0] % bound;
		}
	}
}

impl fmt::Display for PairingCode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for (place, digit) in self.0.iter().enumerate() {
			if place == 2 || place == 5 {
				f.write_str("-")?;
			}
			write!(f, "{digit}")?;
		}
		Ok(())
	}
}

impl fmt::Debug for PairingCode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "PairingCode({}-…)", self.nameplate())
	}
}

/// Why text is not a pairing code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a pairing code is eight digits, such as 37-482-177")]
pub struct NotACode;

impl FromStr for PairingCode {
	type Err = NotACode;

	/// Reads a code as it is shown, or with spaces or no separators: eight digits, the first
	/// not 0.
	fn from_str(text: &str) -> Result<Self, NotACode> {
		let mut digits = Vec::new();
		for character in text.trim().chars() {
			match character {
				'0'..='9' => digits.push(character as u8 - b'0'),
				'-' | ' ' => {}
				_ => return Err(NotACode),
			}
		}
		let digits: [u8; 8] = digits.try_into().map_err(|_| NotACode)?;
		if digits[0] == 0 {
			return Err(NotACode);
		}
		Ok(Self(digits))
	}
}

/// Which side of pairing a replica takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
	/// The replica that holds the library, and shows the code.
	Sponsor,
	/// The new replica, on whose device the code is typed.
	Joiner,
}

impl Side {
	/// The side's name in tags.
	const fn name(self) -> &'static str {
		match self {
			Self::Sponsor => "sponsor",
			Self::Joiner => "joiner",
		}
	}
}

/// One side of SPAKE2 for pairing, between its own message and the other side's.
#[derive(Debug)]
pub struct Pairing(Pake);

impl Pairing {
	/// Starts pairing with `code` as `side`, and returns the message to send.
	pub fn start(side: Side, code: &PairingCode, rng: &mut impl CryptoRng) -> (Self, Vec<u8>) {
		let pake_side = match side {
			Side::Sponsor => PakeSide::A,
			Side::Joiner => PakeSide::B,
		};
		let (pake, message) = Pake::start(
			pake_side,
			&code.password(),
			b"nibkeep sponsor",
			b"nibkeep joiner",
			rng,
		);
		(Self(pake), message)
	}

	/// Finishes SPAKE2 with the other side's message, and derives the pairing key, bound to
	/// the connection by `binding`.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Pake`] if the message is not one of the other side's.
	pub fn finish(self, message: &[u8], binding: &[u8; 32]) -> Result<PairingKey, CryptoError> {
		self.finish_with("nibkeep 2026-10 pairing", message, binding)
	}

	/// Finishes SPAKE2 as [`Pairing::finish`] does, for an invitation to a space: the key is
	/// derived with a context of its own, so that a code shown to pair never works as an
	/// invitation, nor one shown to invite as a pairing.
	///
	/// # Errors
	///
	/// Returns [`CryptoError::Pake`] if the message is not one of the other side's.
	pub fn finish_invitation(
		self,
		message: &[u8],
		binding: &[u8; 32],
	) -> Result<PairingKey, CryptoError> {
		self.finish_with("nibkeep 2026-11 invitations", message, binding)
	}

	fn finish_with(
		self,
		context: &str,
		message: &[u8],
		binding: &[u8; 32],
	) -> Result<PairingKey, CryptoError> {
		let agreed = self.0.finish(message)?;
		let mut material = Vec::with_capacity(64);
		material.extend_from_slice(agreed.expose_bytes());
		material.extend_from_slice(binding);
		Ok(PairingKey(Key::from_bytes(blake3::derive_key(
			context, &material,
		))))
	}
}

/// The key that pairing agrees on, which tags what each side sends. Both sides have the same
/// key only if they typed the same code, over the same connection.
pub struct PairingKey(Key);

impl PairingKey {
	/// The tag of a message that `side` sends.
	#[must_use]
	pub fn tag(&self, side: Side, message: &Value) -> [u8; 32] {
		let tagged = Value::Array(vec![Value::Text(side.name().to_owned()), message.clone()]);
		self.0.tag(&cbor::encode(&tagged))
	}

	/// Whether `tag` is the tag of a message that `side` sent, compared in constant time.
	#[must_use]
	pub fn checks(&self, side: Side, message: &Value, tag: &[u8]) -> bool {
		let tagged = Value::Array(vec![Value::Text(side.name().to_owned()), message.clone()]);
		self.0.verify_tag(&cbor::encode(&tagged), tag)
	}
}

impl fmt::Debug for PairingKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("PairingKey(..)")
	}
}

/// What a joiner asks for: to be a replica with these keys, on a device and in an app with
/// these names, and, with a secrets encryption key, the secrets too. In CBOR,
/// `[signing key, encryption key, device, app]`, or
/// `[signing key, encryption key, device, app, secrets encryption key]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
	/// The new replica's verifying key.
	pub signing: VerifyingKey,
	/// The new replica's encryption key, which the space keys are sealed to.
	pub encryption: EncryptionKey,
	/// The name of its device.
	pub device: String,
	/// The name of its app.
	pub app: String,
	/// The new replica's secrets encryption key, which secrets keys are sealed to, if it
	/// asks for the secrets from the sponsor.
	pub secrets: Option<EncryptionKey>,
}

impl ToCbor for JoinRequest {
	fn to_cbor(&self) -> Value {
		let mut items = vec![
			self.signing.to_cbor(),
			self.encryption.to_cbor(),
			self.device.to_cbor(),
			self.app.to_cbor(),
		];
		items.extend(self.secrets.as_ref().map(ToCbor::to_cbor));
		Value::Array(items)
	}
}

impl FromCbor for JoinRequest {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a request: [signing key, encryption key, device, app, secrets encryption key?]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let (signing, encryption, device, app, secrets) = match items.as_slice() {
			[signing, encryption, device, app] => (signing, encryption, device, app, None),
			[signing, encryption, device, app, secrets] => {
				(signing, encryption, device, app, Some(secrets))
			}
			_ => return Err(cbor::Error::Expected(EXPECTED)),
		};
		let name = |value: &Value| -> Result<String, cbor::Error> {
			let name = String::from_cbor(value)?;
			if name.is_empty() || name.len() > MAX_NAME_LEN {
				return Err(cbor::Error::Expected(EXPECTED));
			}
			Ok(name)
		};
		Ok(Self {
			signing: VerifyingKey::from_cbor(signing)?,
			encryption: EncryptionKey::from_cbor(encryption)?,
			device: name(device)?,
			app: name(app)?,
			secrets: secrets.map(EncryptionKey::from_cbor).transpose()?,
		})
	}
}

/// The space keys of one space, each sealed to the joiner. In CBOR,
/// `[space, [[epoch, sealed key], …]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedSpaceKeys {
	/// The space.
	pub space: SpaceId,
	/// Its keys by epoch, each sealed.
	pub keys: Vec<(u64, SealedMessage)>,
}

/// What a sponsor grants a joiner: who it is now, and the space keys; for a replica of the
/// user's, whether it asks before it takes a space given to it later, as a phone does, or
/// takes it, as a computer does; and, for an AI agent's replica, that it is one, and the
/// other spaces chosen for it, in which it signs its listings as it joins, as it signs its
/// listing in the personal space. In CBOR, `[user, device, replica, space, sponsor, keys,
/// asks]` for a replica of the user's, where `asks` is a boolean, and `[user, device,
/// replica, space, sponsor, keys, [[space, member], …]]` for an AI agent's, with no space
/// chosen or some.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
	/// The user.
	pub user: EntityId,
	/// The joiner's device.
	pub device: EntityId,
	/// The joiner's replica.
	pub replica: EntityId,
	/// The user's personal space.
	pub space: SpaceId,
	/// The sponsor's verifying key, which the joiner trusts for its first sync.
	pub sponsor: VerifyingKey,
	/// The space keys, sorted by space.
	pub keys: Vec<SealedSpaceKeys>,
	/// Whether the joiner is an AI agent's replica, as its listings say.
	pub ai_agent: bool,
	/// For an AI agent's replica, the spaces besides the personal one chosen for it, each with
	/// the user's member record there, sorted by space: empty for a replica of the user's.
	pub chosen: Vec<(SpaceId, EntityId)>,
	/// For a replica of the user's, whether it asks before it takes a space given to it
	/// later, as the person chose by pairing it as a phone: it states so itself, after its
	/// first sync ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	/// `false` for an AI agent's.
	pub asks_first: bool,
}

impl ToCbor for Grant {
	fn to_cbor(&self) -> Value {
		let keys = self
			.keys
			.iter()
			.map(|space| {
				Value::Array(vec![
					space.space.to_cbor(),
					Value::Array(
						space
							.keys
							.iter()
							.map(|(epoch, sealed)| {
								Value::Array(vec![epoch.to_cbor(), sealed.to_cbor()])
							})
							.collect(),
					),
				])
			})
			.collect();
		let mut items = vec![
			self.user.to_cbor(),
			self.device.to_cbor(),
			self.replica.to_cbor(),
			self.space.to_cbor(),
			self.sponsor.to_cbor(),
			Value::Array(keys),
		];
		if self.ai_agent {
			items.push(Value::Array(
				self.chosen
					.iter()
					.map(|(space, member)| Value::Array(vec![space.to_cbor(), member.to_cbor()]))
					.collect(),
			));
		} else {
			items.push(Value::Bool(self.asks_first));
		}
		Value::Array(items)
	}
}

impl FromCbor for Grant {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str =
			"a grant: [user, device, replica, space, sponsor, keys, asks or chosen]";
		let error = || cbor::Error::Expected(EXPECTED);
		let Value::Array(items) = value else {
			return Err(error());
		};
		let [user, device, replica, space, sponsor, keys, last] = items.as_slice() else {
			return Err(error());
		};
		// A boolean for a replica of the user's, the spaces chosen for an AI agent's.
		let (asks_first, chosen) = match last {
			Value::Bool(asks) => (*asks, None),
			chosen => (false, Some(chosen)),
		};
		let mut chosen_spaces = Vec::new();
		if let Some(chosen) = chosen {
			let Value::Array(chosen) = chosen else {
				return Err(error());
			};
			for each in chosen {
				let Value::Array(each) = each else {
					return Err(error());
				};
				let [space, member] = each.as_slice() else {
					return Err(error());
				};
				chosen_spaces.push((SpaceId::from_cbor(space)?, EntityId::from_cbor(member)?));
			}
			if !chosen_spaces.windows(2).all(|pair| pair[0].0 < pair[1].0) {
				return Err(error());
			}
		}
		let Value::Array(spaces) = keys else {
			return Err(error());
		};
		let mut sealed = Vec::new();
		for entry in spaces {
			let Value::Array(entry) = entry else {
				return Err(error());
			};
			let [space, keys] = entry.as_slice() else {
				return Err(error());
			};
			let Value::Array(keys) = keys else {
				return Err(error());
			};
			let mut by_epoch = Vec::new();
			for key in keys {
				let Value::Array(key) = key else {
					return Err(error());
				};
				let [epoch, key] = key.as_slice() else {
					return Err(error());
				};
				by_epoch.push((u64::from_cbor(epoch)?, SealedMessage::from_cbor(key)?));
			}
			if by_epoch.is_empty() || !by_epoch.windows(2).all(|pair| pair[0].0 < pair[1].0) {
				return Err(error());
			}
			sealed.push(SealedSpaceKeys {
				space: SpaceId::from_cbor(space)?,
				keys: by_epoch,
			});
		}
		if !sealed
			.windows(2)
			.all(|pair| pair[0].space.as_bytes() < pair[1].space.as_bytes())
		{
			return Err(error());
		}
		Ok(Self {
			user: EntityId::from_cbor(user)?,
			device: EntityId::from_cbor(device)?,
			replica: EntityId::from_cbor(replica)?,
			space: SpaceId::from_cbor(space)?,
			sponsor: VerifyingKey::from_cbor(sponsor)?,
			keys: sealed,
			ai_agent: chosen.is_some(),
			chosen: chosen_spaces,
			asks_first,
		})
	}
}

/// The associated data of a sealed space key: `[space, epoch, replica]`.
pub(crate) fn space_key_data(space: SpaceId, epoch: u64, replica: EntityId) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![
		space.to_cbor(),
		epoch.to_cbor(),
		replica.to_cbor(),
	]))
}

/// Seals every key of every space to the joiner's encryption key, for its replica.
pub fn seal_space_keys(
	recipient: &EncryptionKey,
	replica: EntityId,
	spaces: &BTreeMap<SpaceId, BTreeMap<u64, Key>>,
	rng: &mut impl CryptoRng,
) -> Vec<SealedSpaceKeys> {
	let mut sealed: Vec<SealedSpaceKeys> = spaces
		.iter()
		.map(|(&space, keys)| SealedSpaceKeys {
			space,
			keys: keys
				.iter()
				.map(|(&epoch, key)| {
					let data = space_key_data(space, epoch, replica);
					(
						epoch,
						SealedMessage::seal_key(recipient, key, SPACE_KEYS, &data, rng),
					)
				})
				.collect(),
		})
		.collect();
	sealed.sort_by(|a, b| a.space.as_bytes().cmp(b.space.as_bytes()));
	sealed
}

/// Opens the space keys that a grant seals to this replica.
///
/// # Errors
///
/// Returns [`CryptoError::Decryption`] if a key does not open: it was sealed to another key,
/// for another replica, space or epoch, or changed.
pub fn open_space_keys(
	decryption: &DecryptionKey,
	replica: EntityId,
	sealed: &[SealedSpaceKeys],
) -> Result<BTreeMap<SpaceId, BTreeMap<u64, Key>>, CryptoError> {
	let mut spaces = BTreeMap::new();
	for space in sealed {
		let mut keys = BTreeMap::new();
		for (epoch, key) in &space.keys {
			let data = space_key_data(space.space, *epoch, replica);
			keys.insert(*epoch, key.open_key(decryption, SPACE_KEYS, &data)?);
		}
		spaces.insert(space.space, keys);
	}
	Ok(spaces)
}
