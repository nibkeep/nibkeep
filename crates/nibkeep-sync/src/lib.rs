//! How NibKeep replicas pair and sync their spaces: the messages they send, reconciling
//! what two replicas hold with negentropy, and the exchanges of pairing and syncing, which
//! run over any [`Transport`] and work on the replica's data through the traits
//! [`Holder`] and [`Sponsoring`]. The crate does no I/O of its own, so that the simulation
//! can run many replicas in one process.
//!
//! The protocol is specified in `docs/spec/sync.md`, and the reasons for it are in
//! `docs/Technical-design.md`, section 10.

mod error;
mod invite;
mod message;
mod pack;
mod pair;
mod proof;
mod reconcile;
mod session;
mod transport;

pub use error::{Reason, SyncError};
pub use invite::{Accepting, Invitation, Inviting, accept, invite};
pub use pack::{MOST_PACK_BLOB, PACK_SIZE, PackItem, PackReader, pack_header, pack_item};
pub use pair::{Joined, Sponsored, Sponsoring, join, joined, refuse, sponsor};
pub use proof::{
	CHALLENGES_AT_ONCE, Challenge, MOST_RANGE_LEN, MOST_RANGES, RANGE_LEN, RANGES, Range,
};
pub use reconcile::{Holding, item_id};
pub use session::{BlobBytes, Counts, Holder, SpaceSynced, Synced, initiate, respond};
pub use transport::{MAX_MESSAGE_LEN, Transport, TransportError};

/// How many listings by a recovery code an initiator's hello carries at most: one for each
/// space or group that lists it by a code ([Sync](../../../docs/spec/sync.md#3-syncing)).
pub const MOST_INTRODUCTIONS: usize = 1024;

/// What the fuzz targets reach: reading the messages that other devices send.
#[doc(hidden)]
pub mod fuzzing {
	use nibkeep_base::{Hash, SpaceId};

	use crate::message::{InviteMessage, Item, PairMessage, SyncMessage};
	use crate::reconcile::Reconciling;
	use crate::{Holding, Reason};

	/// Reads a message of pairing, and writes it out again, if it is one.
	#[must_use]
	pub fn pair_message(bytes: &[u8]) -> Option<Vec<u8>> {
		PairMessage::decode(bytes)
			.ok()
			.map(|message| message.encode())
	}

	/// Reads a message of an invitation, and writes it out again, if it is one.
	#[must_use]
	pub fn invite_message(bytes: &[u8]) -> Option<Vec<u8>> {
		InviteMessage::decode(bytes)
			.ok()
			.map(|message| message.encode())
	}

	/// Reads a message of syncing, and writes it out again, if it is one.
	#[must_use]
	pub fn sync_message(bytes: &[u8]) -> Option<Vec<u8>> {
		SyncMessage::decode(bytes)
			.ok()
			.map(|message| message.encode())
	}

	/// Reads a pack to its end, and checks that each item writes out again as it reads;
	/// `None` if it is no pack, or holds what a pack does not.
	#[must_use]
	pub fn pack(bytes: &[u8]) -> Option<()> {
		let mut reader = crate::PackReader::new(bytes).ok()?;
		let space = reader.space();
		while let Some((item, start, length)) = reader.next_item().ok()? {
			let written = crate::pack_item(space, &item);
			assert!(
				bytes.get(start..start + length) == Some(written.as_slice()),
				"an item of a pack writes out as it was read"
			);
			assert!(
				crate::PackReader::item_at(space, &written).ok().as_ref() == Some(&item),
				"an item reads alone as in its pack"
			);
		}
		Some(())
	}

	/// Answers a message of negentropy as each side, over a few items.
	pub fn negentropy(bytes: &[u8]) {
		let holdings = || {
			(0..40_u8)
				.map(|byte| Holding::Account {
					id: Hash::of(&[byte]),
					issued: u64::from(byte),
				})
				.collect()
		};
		if let Ok(mut responder) = Reconciling::new(holdings()) {
			let _ = responder.respond(bytes);
		}
		if let Ok(mut initiator) = Reconciling::new(holdings())
			&& initiator.initiate().is_ok()
		{
			let _ = initiator.answer(bytes, &mut Vec::new(), &mut Vec::new());
		}
	}

	/// The first message of negentropy over a few items, as a seed.
	///
	/// # Panics
	///
	/// Never: the items are few and well formed.
	#[must_use]
	pub fn negentropy_sample() -> Vec<u8> {
		let mut initiator = Reconciling::new(
			(0..3_u8)
				.map(|byte| Holding::Account {
					id: Hash::of(&[byte]),
					issued: u64::from(byte),
				})
				.collect(),
		)
		.expect("a set");
		initiator.initiate().expect("a message")
	}

	/// Messages, each with its name.
	pub type Named = Vec<(&'static str, Vec<u8>)>;

	/// Messages of an invitation, as seeds for the fuzz targets.
	#[must_use]
	pub fn invitation_samples() -> Named {
		vec![
			("end", InviteMessage::End(Reason::Declined).encode()),
			("pake", InviteMessage::Pake(vec![0x41; 33], None).encode()),
			(
				"pake-proven",
				InviteMessage::Pake(vec![0x41; 33], Some([3; 32])).encode(),
			),
			(
				"listing",
				InviteMessage::Listing("bGlzdGluZw".to_owned(), [3; 32]).encode(),
			),
			("joined", InviteMessage::Joined.encode()),
		]
	}

	/// Messages of each kind, as seeds for the fuzz targets: pairing's first, then
	/// syncing's.
	#[must_use]
	pub fn samples() -> (Named, Named) {
		let space = SpaceId::from_bytes([5; 16]);
		let pairing = vec![
			("end", PairMessage::End(Reason::WrongCode).encode()),
			("pake", PairMessage::Pake(vec![0x41; 33], None).encode()),
			(
				"pake-proven",
				PairMessage::Pake(vec![0x41; 33], Some([3; 32])).encode(),
			),
			("joined", PairMessage::Joined(Vec::new()).encode()),
			(
				"joined-listing",
				PairMessage::Joined(vec!["bGlzdGluZw".to_owned()]).encode(),
			),
		];
		let first = negentropy_sample();
		let signing = nibkeep_crypto::SigningKey::from_bytes(&[6; 32]);
		let code = nibkeep_crypto::SigningKey::from_bytes(&[8; 32]);
		let listing = nibkeep_governance::RecoveryListing {
			space,
			record: nibkeep_base::EntityId::from_bytes([1; 16]),
			owner: nibkeep_base::EntityId::from_bytes([2; 16]),
			encryption: Hash::of(b"encryption key"),
			key: signing.verifying_key(),
			recovery_key: code.verifying_key(),
			signature: code.sign("sample", b"listing"),
		};
		let hello = |listings| {
			SyncMessage::Hello(
				signing.verifying_key(),
				signing.sign("sample", b"hello"),
				listings,
			)
			.encode()
		};
		let syncing = vec![
			("end", SyncMessage::End(Reason::Busy).encode()),
			("hello", hello(Vec::new())),
			("hello-listings", hello(vec![listing])),
			("spaces", SyncMessage::Spaces(vec![space]).encode()),
			("reconcile", SyncMessage::Reconcile(space, first).encode()),
			(
				"blob",
				SyncMessage::Item(space, Item::Blob(Hash::of(b"sealed"), 7)).encode(),
			),
			("wanted", SyncMessage::Wanted(space, vec![[9; 32]]).encode()),
			("sent", SyncMessage::Sent(space).encode()),
			("bytes", SyncMessage::Bytes(b"sealed!".to_vec()).encode()),
			("done", SyncMessage::Done.encode()),
			(
				"asked",
				SyncMessage::Asked(space, vec![Hash::of(b"sealed")]).encode(),
			),
			("your-asks", SyncMessage::YourAsks(space).encode()),
			(
				"prove",
				SyncMessage::Prove(
					space,
					vec![crate::Challenge {
						sealed: Hash::of(b"sealed"),
						key: [3; 32],
						ranges: vec![crate::Range { offset: 0, len: 7 }],
						plain: true,
					}],
				)
				.encode(),
			),
			(
				"proofs",
				SyncMessage::Proofs(space, vec![Some([4; 32]), None]).encode(),
			),
		];
		(pairing, syncing)
	}
}
