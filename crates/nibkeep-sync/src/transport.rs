//! What carries the messages between two replicas.

/// How long a message may be: 16 MiB.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

/// A connection between two replicas, which carries whole messages in order, and is
/// encrypted and bound to the two endpoints' keys ([Sync](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/sync.md#1-connections)).
pub trait Transport {
	/// Sends a message.
	///
	/// # Errors
	///
	/// Returns an error if the connection fails.
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError>;

	/// Waits for the next message.
	///
	/// # Errors
	///
	/// Returns an error if the connection fails or closes, or the message is longer than
	/// [`MAX_MESSAGE_LEN`].
	fn receive(&mut self) -> Result<Vec<u8>, TransportError>;

	/// Whether [`Transport::receive`] would return at once: the next message has arrived, or
	/// the connection failed. A transport that cannot tell says no, which costs only speed:
	/// a replica saves what it received before it waits ([`Holder::pause`](crate::Holder::pause)).
	fn has_message(&mut self) -> bool {
		false
	}

	/// The 32 bytes exported from the connection's encryption, the same on both sides of
	/// this connection and on no other.
	fn binding(&self) -> [u8; 32];

	/// This side's endpoint ID.
	fn own_endpoint(&self) -> [u8; 32];

	/// The other side's endpoint ID, as the connection proved it.
	fn peer_endpoint(&self) -> [u8; 32];
}

/// Why a connection failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);
