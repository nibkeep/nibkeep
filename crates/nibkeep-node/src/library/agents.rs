//! AI agents on the user's own replica ([technical design: AI agents]): the person's
//! consent, which `nib mcp enable` gives, and a key for each AI client, kept in this
//! replica's folder sealed with its storage key, which signs the client's changes while the
//! library acts as it.
//!
//! The record of each key is like a paired AI agent's replica's, on this replica's device,
//! with no endpoint and no folder of its own: every replica treats its changes as a paired
//! AI agent's, and nothing in the log's rules is new.
//!
//! [technical design: AI agents]: ../../../docs/Technical-design.md#14-ai-agents

use std::fs::{self, File};
use std::io;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, Envelope, SigningKey, Zeroizing};
use nibkeep_governance::{Listing, NewAgentKey, add_agent_key, replica_record};

use super::Library;
use crate::NodeError;
use crate::info::write_file;

/// The file, in a replica's folder, of the AI agents that the person allows on it.
pub(crate) const AGENTS: &str = "agents.cbor";

/// The lock that programs take while they read and change [`AGENTS`], so that two sessions
/// of one client never make two keys.
const AGENTS_LOCK: &str = "agents.lock";

/// What the key that seals [`AGENTS`] is derived from the storage key with.
const AGENTS_CONTEXT: &str = "nibkeep 2026-11 ai agents";

/// What the file's envelope is bound to.
const AGENTS_AAD: &[u8] = b"nibkeep 2026-11 ai agents file";

/// The key that the library acts as, while an AI client works on it: its changes are signed
/// with it.
pub(crate) struct Acting {
	/// The AI client's name.
	pub(crate) name: String,
	/// Its signing key.
	pub(crate) signing: SigningKey,
}

/// An AI agent's key that this replica made.
struct AgentKey {
	name: String,
	signing: SigningKey,
	decryption: DecryptionKey,
}

/// What the file holds: whether the person allows AI agents here, and the keys made.
#[derive(Default)]
struct Agents {
	allowed: bool,
	keys: Vec<AgentKey>,
}

impl ToCbor for Agents {
	fn to_cbor(&self) -> Value {
		Value::Array(vec![
			1u8.to_cbor(),
			Value::Bool(self.allowed),
			Value::Array(
				self.keys
					.iter()
					.map(|key| {
						Value::Array(vec![
							key.name.to_cbor(),
							Value::Bytes(key.signing.expose_bytes().to_vec()),
							Value::Bytes(key.decryption.expose_seed().to_vec()),
						])
					})
					.collect(),
			),
		])
	}
}

impl FromCbor for Agents {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "AI agents: [1, allowed, [[name, signing, decryption], …]]";
		let wrong = || cbor::Error::Expected(EXPECTED);
		let Value::Array(items) = value else {
			return Err(wrong());
		};
		let [suite, allowed, keys] = items.as_slice() else {
			return Err(wrong());
		};
		if u8::from_cbor(suite) != Ok(1) {
			return Err(wrong());
		}
		let Value::Array(keys) = keys else {
			return Err(wrong());
		};
		let keys = keys
			.iter()
			.map(|key| {
				let Value::Array(parts) = key else {
					return Err(wrong());
				};
				let [name, Value::Bytes(signing), Value::Bytes(decryption)] = parts.as_slice()
				else {
					return Err(wrong());
				};
				let signing: [u8; 32] = signing.as_slice().try_into().map_err(|_| wrong())?;
				let decryption: [u8; 32] = decryption.as_slice().try_into().map_err(|_| wrong())?;
				Ok(AgentKey {
					name: String::from_cbor(name)?,
					signing: SigningKey::from_bytes(&signing),
					decryption: DecryptionKey::from_seed(&decryption),
				})
			})
			.collect::<Result<_, _>>()?;
		Ok(Self {
			allowed: bool::from_cbor(allowed)?,
			keys,
		})
	}
}

impl Library {
	/// Whether the person allowed AI agents on this replica, with `nib mcp enable`.
	///
	/// # Errors
	///
	/// Returns an error if the file of AI agents cannot be read.
	pub fn ai_agents_enabled(&self) -> Result<bool, NodeError> {
		Ok(self.read_agents()?.allowed)
	}

	/// Allows AI agents on this replica, as the person's consent: AI clients on this
	/// computer may then work on it, each with a key of its own, in the personal space.
	/// Keys of clients that were revoked since are forgotten, so that a client named as one
	/// of them gets a new key.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] if the library acts as an AI agent, and an error
	/// if the file cannot be written.
	pub fn enable_ai_agents(&mut self, rng: &mut impl CryptoRng) -> Result<(), NodeError> {
		self.check_not_ai_agent("allow AI agents")?;
		self.change_agents(rng, |library, agents| {
			agents.allowed = true;
			let personal = library.info.personal;
			let mut kept = Vec::new();
			for key in std::mem::take(&mut agents.keys) {
				if !library.key_revoked(personal, &key.signing)? {
					kept.push(key);
				}
			}
			agents.keys = kept;
			Ok(())
		})
	}

	/// Withdraws the person's consent to AI agents on this replica, and revokes every key
	/// that it made for them. Returns the names of the clients whose keys it revoked.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentMayNot`] if the library acts as an AI agent, and an error
	/// if a key cannot be revoked or the file written.
	pub fn disable_ai_agents(
		&mut self,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Vec<String>, NodeError> {
		self.check_not_ai_agent("withdraw AI agents")?;
		let personal = self.info.personal;
		// Held throughout, so that no key made meanwhile is forgotten unrevoked.
		let lock = self.lock_agents()?;
		let agents = self.read_agents()?;
		let mut revoking = Vec::new();
		let mut names = Vec::new();
		for key in &agents.keys {
			if !self.key_revoked(personal, &key.signing)? {
				revoking.push(replica_record(personal, &key.signing.verifying_key()));
				names.push(key.name.clone());
			}
		}
		if !revoking.is_empty() {
			self.revoke(&revoking, wall_millis, rng)?;
		}
		self.write_agents(&Agents::default(), rng)?;
		drop(lock);
		Ok(names)
	}

	/// Acts as the AI client named `name` from now on, for as long as the library is open:
	/// its key signs every change, the library refuses what an AI agent may not do, and it
	/// keeps only the personal space, so that nothing reads another. The client's key is made
	/// the first time, and listed by this replica.
	///
	/// # Errors
	///
	/// Returns [`NodeError::AiAgentsNotAllowedHere`] if the person did not allow AI agents here,
	/// [`NodeError::AiAgentRevoked`] if the client's key was revoked, and an error if the key
	/// cannot be made or listed.
	pub fn act_as_ai_agent(
		&mut self,
		name: &str,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		if name.trim().is_empty() {
			return Err(NodeError::AiAgentMayNot("go without a name"));
		}
		self.check_not_ai_agent("act as another AI agent")?;
		let personal = self.info.personal;
		let lock = self.lock_agents()?;
		let mut agents = self.read_agents()?;
		if !agents.allowed {
			return Err(NodeError::AiAgentsNotAllowedHere);
		}
		let found = agents.keys.iter().position(|key| key.name == name);
		if let Some(at) = found
			&& self.key_revoked(personal, &agents.keys[at].signing)?
		{
			return Err(NodeError::AiAgentRevoked(name.to_owned()));
		}
		let at = found.unwrap_or_else(|| {
			agents.keys.push(AgentKey {
				name: name.to_owned(),
				signing: SigningKey::generate(rng),
				decryption: DecryptionKey::generate(rng),
			});
			agents.keys.len() - 1
		});
		if found.is_none() {
			// Kept before it is listed, so that a listed key is never lost.
			self.write_agents(&agents, rng)?;
		}
		let key = &agents.keys[at];
		let signing = SigningKey::from_bytes(key.signing.expose_bytes());
		// Listed unless it is already: also a key kept by a call whose listing failed.
		if !self.key_listed(personal, &signing)? {
			let encryption = key.decryption.encryption_key();
			let listing =
				Listing::sign(&signing, &encryption, personal, (self.info.user, true)).to_text();
			let (_, mut operations, ordinary) = add_agent_key(
				&NewAgentKey {
					space: personal,
					user: self.info.user,
					signing: &signing.verifying_key(),
					encryption: &encryption,
					device: self.info.device,
					name,
					listing: &listing,
				},
				rng,
			);
			operations.extend(ordinary);
			self.commit(personal, operations, wall_millis, rng)?;
		}
		drop(lock);
		self.spaces.retain(|&space, _| space == personal);
		self.chosen = None;
		self.acting = Some(Acting {
			name: name.to_owned(),
			signing,
		});
		Ok(())
	}

	/// The name of the AI client that the library acts as, if it does.
	#[must_use]
	pub fn acting_as(&self) -> Option<&str> {
		self.acting.as_ref().map(|acting| acting.name.as_str())
	}

	/// The key that signs this library's changes: the AI client's that it acts as, or else
	/// this replica's own.
	pub(crate) fn author_key(&self) -> &SigningKey {
		self.acting
			.as_ref()
			.map_or(&self.replica.signing, |acting| &acting.signing)
	}

	/// Whether a counting change lists `key` in `space`.
	fn key_listed(&self, space: SpaceId, key: &SigningKey) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.is_listed(&key.verifying_key(), &open.keys)?)
	}

	/// Whether `key` was revoked in `space`.
	fn key_revoked(&self, space: SpaceId, key: &SigningKey) -> Result<bool, NodeError> {
		let open = self.space(space)?;
		Ok(open
			.stored
			.log()
			.is_revoked(&key.verifying_key(), &open.keys)?)
	}

	/// The record of the key of the AI client named `name`, if this replica made one.
	///
	/// # Errors
	///
	/// Returns an error if the file of AI agents cannot be read.
	pub fn ai_agent_record(&self, name: &str) -> Result<Option<EntityId>, NodeError> {
		let personal = self.info.personal;
		Ok(self
			.read_agents()?
			.keys
			.iter()
			.find(|key| key.name == name)
			.map(|key| replica_record(personal, &key.signing.verifying_key())))
	}

	/// Takes the lock on the file of AI agents, which closing the returned file lets go of.
	fn lock_agents(&self) -> Result<File, NodeError> {
		let path = self.directory.join(AGENTS_LOCK);
		let lock = File::options()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&path)
			.map_err(NodeError::file(&path))?;
		lock.lock().map_err(NodeError::file(&path))?;
		Ok(lock)
	}

	/// Changes the file of AI agents under its lock.
	fn change_agents(
		&mut self,
		rng: &mut impl CryptoRng,
		change: impl FnOnce(&mut Self, &mut Agents) -> Result<(), NodeError>,
	) -> Result<(), NodeError> {
		let lock = self.lock_agents()?;
		let mut agents = self.read_agents()?;
		change(self, &mut agents)?;
		self.write_agents(&agents, rng)?;
		drop(lock);
		Ok(())
	}

	/// The file of AI agents, or none allowed where there is no file.
	fn read_agents(&self) -> Result<Agents, NodeError> {
		let path = self.directory.join(AGENTS);
		let bytes = match fs::read(&path) {
			Ok(bytes) => bytes,
			Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Agents::default()),
			Err(error) => return Err(NodeError::file(&path)(error)),
		};
		let unreadable = || {
			NodeError::file(&path)(io::Error::new(
				io::ErrorKind::InvalidData,
				"the file of AI agents does not read",
			))
		};
		let envelope: Envelope = cbor::from_slice(&bytes).map_err(|_| unreadable())?;
		let plain: Zeroizing<Vec<u8>> = envelope
			.open(&self.derived_key(AGENTS_CONTEXT), AGENTS_AAD)
			.map_err(|_| unreadable())?;
		cbor::from_slice(&plain).map_err(|_| unreadable())
	}

	/// Writes the file of AI agents, sealed with a key derived from the storage key.
	fn write_agents(&self, agents: &Agents, rng: &mut impl CryptoRng) -> Result<(), NodeError> {
		let plain = Zeroizing::new(cbor::to_vec(agents));
		let envelope = Envelope::seal(&self.derived_key(AGENTS_CONTEXT), &plain, AGENTS_AAD, rng);
		write_file(&self.directory.join(AGENTS), &cbor::to_vec(&envelope))
	}
}
