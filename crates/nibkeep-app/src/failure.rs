//! Why a request failed, as an app tells people: a message, and a kind that the app's
//! interface tells apart.

use std::fmt;

use nibkeep_node::{KeysError, NodeError};
use serde_json::{Value as Json, json};

use crate::keystore;

/// Why a request failed, as the app tells people.
#[derive(Debug)]
pub enum Failure {
	/// The engine failed, or a passphrase did not unlock.
	Node(NodeError),
	/// The request needs the library, which is locked.
	Locked,
	/// Another program has the library to itself, as while it makes a space's state file
	/// anew after a purge, longer than a call waits.
	Busy,
	/// The secrets are locked, as when another program protected them anew.
	SecretsLocked,
	/// The library holds no secrets yet: the first needs a passphrase of their own.
	NoSecrets,
	/// Another device keeps the library's secrets, which this one has not asked for.
	SecretsElsewhere,
	/// This device asked another for the secrets, which has not given them yet.
	SecretsAsked,
	/// The program that syncs this device did not sync in time when asked to.
	NotAnswered,
	/// A request that the engine does not take, such as an operation it does not know.
	Request(String),
	/// A failure of the app's own, of a kind that it tells apart, such as `clipboard`.
	App {
		/// The kind, as the app's interface tells it apart.
		kind: &'static str,
		/// What the person is told.
		message: String,
	},
}

impl From<NodeError> for Failure {
	fn from(error: NodeError) -> Self {
		Self::Node(error)
	}
}

impl Failure {
	/// A failure of the app's own, of the kind `kind`.
	pub fn app(kind: &'static str, message: impl fmt::Display) -> Self {
		Self::App {
			kind,
			message: message.to_string(),
		}
	}

	/// The failure's kind and message. Only a passphrase that does not unlock is of the kind
	/// `passphrase`; a keystore's prompt that the person turned down is `cancelled`, a key
	/// that the keystore no longer has, as when a phone's screen lock was removed,
	/// `key_lost`, and a library whose drive must be checked first, `removed`.
	#[must_use]
	pub fn kind_and_message(&self) -> (&'static str, String) {
		let busy = || {
			(
				"busy",
				"another program has the library to itself; try again in a moment".to_owned(),
			)
		};
		match self {
			Self::Node(NodeError::Keys(KeysError::WrongPassphrase)) => {
				("passphrase", "the passphrase is wrong".to_owned())
			}
			Self::Node(error) if keystore::cancelled(error) => {
				("cancelled", "nothing was confirmed".to_owned())
			}
			Self::Node(error) if keystore::lost(error) => {
				("key_lost", "the keystore no longer has the key".to_owned())
			}
			Self::Node(NodeError::UncleanRemoval) => {
				("removed", NodeError::UncleanRemoval.to_string())
			}
			Self::Node(NodeError::Exists) => ("exists", NodeError::Exists.to_string()),
			Self::Node(NodeError::NotInKeystore) => {
				("needs_keystore", NodeError::NotInKeystore.to_string())
			}
			Self::Node(NodeError::NoOtherReplicas) => (
				"no_other_devices",
				"the library is on no other device yet".to_owned(),
			),
			Self::Node(NodeError::RecoveryCutShort) => (
				"recovery_cut_short",
				"bringing the library back here was cut short; bring it back again with your recovery code".to_owned(),
			),
			// A change refused while another program has the library to itself.
			Self::Node(error) if error.in_use() => busy(),
			Self::Node(error) => ("engine", error.to_string()),
			Self::Locked => ("locked", "the library is locked".to_owned()),
			Self::Busy => busy(),
			Self::SecretsLocked => (
				"secrets_locked",
				"the secrets are locked; unlock them again".to_owned(),
			),
			Self::NoSecrets => (
				"no_secrets",
				"the library holds no secrets yet; the first needs a passphrase of their own"
					.to_owned(),
			),
			Self::SecretsElsewhere => {
				("secrets_elsewhere", NodeError::SecretsElsewhere.to_string())
			}
			Self::SecretsAsked => ("secrets_asked", NodeError::SecretsAsked.to_string()),
			Self::NotAnswered => (
				"not_answered",
				"the program that syncs this device did not sync in time".to_owned(),
			),
			Self::Request(why) => ("request", why.clone()),
			Self::App { kind, message } => (kind, message.clone()),
		}
	}

	/// The failure as an answer: `{"error": message, "kind": kind}`.
	#[must_use]
	pub fn answer(&self) -> Json {
		let (kind, message) = self.kind_and_message();
		json!({"error": message, "kind": kind})
	}
}

impl fmt::Display for Failure {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.kind_and_message().1)
	}
}

impl std::error::Error for Failure {}
