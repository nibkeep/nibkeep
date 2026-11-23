//! The recovery file: the personal space's first key, sealed to each recovery code that
//! stands, which cannot come from the log, whose governing changes it wraps. Each replica
//! of the person keeps one in its folder, and each backup holds the same file, so that a
//! code and a backup together open the library ([technical design: recovery codes]).
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use std::fs;
use std::io;
use std::path::Path;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::{Key, RecoveryCode, Signature, key_hash};
use nibkeep_governance::{RecoveryEntry, SpaceEntry, recovery_record};
use nibkeep_log::ChangeId;

use crate::NodeError;

/// The recovery file's name, in a replica's folder and in a backup's.
pub const RECOVERY_FILE: &str = "recovery.cbor";

/// The suite of the recovery file in version 1 of the specification.
const SUITE: u8 = 1;

/// The recovery file. In CBOR, the array `[1, codes, check]`: for each code, sorted by its
/// record, `[record, entry, signature]`, the code's record in the personal space, the
/// personal space's entry for it, as the personal space gives a space to a replica, and the
/// code's signature of what the entry names, which the personal space states on the code's
/// record ([`RecoveryEntry`]); and the BLAKE3 hash of the canonical encoding of `codes`, so
/// that a file damaged on its drive reads as damaged, and is written again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFile {
	entries: Vec<(EntityId, SpaceEntry, Signature)>,
}

/// What a recovery file gives a code that opens it.
pub struct OpenedRecovery {
	/// The personal space.
	pub space: SpaceId,
	/// The member that the code is named for there: the user's own entity.
	pub member: EntityId,
	/// The personal space's first change: the only change that a replica takes as its first.
	pub root: ChangeId,
	/// The personal space's first key, whose hash the entry names.
	pub key: Key,
}

impl RecoveryFile {
	/// A file of these entries, each on its code's record, with the code's signature of what
	/// it names.
	pub(crate) fn new(mut entries: Vec<(EntityId, SpaceEntry, Signature)>) -> Self {
		entries.sort_by_key(|(record, _, _)| *record);
		Self { entries }
	}

	/// Reads the recovery file at `path`: `None` if there is none.
	///
	/// # Errors
	///
	/// Returns [`NodeError::Damaged`] if it does not read as one, and an error if it cannot
	/// be read.
	pub fn read(path: &Path) -> Result<Option<Self>, NodeError> {
		match fs::read(path) {
			Ok(bytes) => cbor::from_slice(&bytes)
				.map(Some)
				.map_err(|error| NodeError::Damaged(format!("{}: {error}", path.display()))),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
			Err(error) => Err(NodeError::file(path)(error)),
		}
	}

	/// The recovery file that `bytes` are, if they read as one.
	pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
		cbor::from_slice(bytes).ok()
	}

	/// Its bytes.
	pub(crate) fn to_bytes(&self) -> Vec<u8> {
		cbor::to_vec(self)
	}

	/// The records of the codes that it holds the first key for, in the personal space.
	pub fn records(&self) -> impl Iterator<Item = EntityId> + '_ {
		self.entries.iter().map(|(record, _, _)| *record)
	}

	/// Its entries, each on its code's record, with the code's signature.
	pub(crate) fn entries(&self) -> &[(EntityId, SpaceEntry, Signature)] {
		&self.entries
	}

	/// Opens the personal space's first key with `code`: from the entry on the code's
	/// record, which is derived from the space, the member and the code's recovery key, if
	/// the code signed what it names, and it opens to a key whose hash it names.
	///
	/// # Errors
	///
	/// Returns [`NodeError::RecoveryFileChanged`] if the file holds an entry on the code's
	/// record that the code did not sign, or that it signed and that does not open to the key
	/// it names, as a device that turned against the person could write; and
	/// [`NodeError::CodeOpensNothing`] if it holds no entry on the code's record.
	pub fn open(&self, code: &RecoveryCode) -> Result<OpenedRecovery, NodeError> {
		let key = code.signing_key().verifying_key();
		let decryption = code.decryption_key();
		let mut changed = false;
		for (record, entry, signature) in &self.entries {
			if recovery_record(entry.space, entry.member, &key) != *record {
				continue;
			}
			let signed = RecoveryEntry {
				space: entry.space,
				member: entry.member,
				root: entry.root,
				hash: entry.hash,
				key,
				signature: *signature,
			};
			// An entry that the code signed and that does not open was sealed to it anew by
			// someone else, as the file's check rules out a damaged drive.
			if signed.verifies()
				&& let Some(opened) = entry.open(&decryption, *record)
				&& key_hash(&opened) == entry.hash
			{
				return Ok(OpenedRecovery {
					space: entry.space,
					member: entry.member,
					root: ChangeId::from_hash(entry.root),
					key: opened,
				});
			}
			changed = true;
		}
		Err(if changed {
			NodeError::RecoveryFileChanged
		} else {
			NodeError::CodeOpensNothing
		})
	}
}

impl ToCbor for RecoveryFile {
	fn to_cbor(&self) -> Value {
		let codes = Value::Array(
			self.entries
				.iter()
				.map(|(record, entry, signature)| {
					Value::Array(vec![record.to_cbor(), entry.to_cbor(), signature.to_cbor()])
				})
				.collect(),
		);
		let check = Hash::of(&cbor::encode(&codes));
		Value::Array(vec![SUITE.to_cbor(), codes, check.to_cbor()])
	}
}

impl FromCbor for RecoveryFile {
	fn from_cbor(value: &Value) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a recovery file: [1, [[record, entry, signature], ...], check]";
		let Value::Array(items) = value else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let [suite, listed, check] = items.as_slice() else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		let Value::Array(codes) = listed else {
			return Err(cbor::Error::Expected(EXPECTED));
		};
		if u8::from_cbor(suite) != Ok(SUITE)
			|| Hash::from_cbor(check)? != Hash::of(&cbor::encode(listed))
		{
			return Err(cbor::Error::Expected(EXPECTED));
		}
		let entries = codes
			.iter()
			.map(|code| {
				let Value::Array(pair) = code else {
					return Err(cbor::Error::Expected(EXPECTED));
				};
				let [record, entry, signature] = pair.as_slice() else {
					return Err(cbor::Error::Expected(EXPECTED));
				};
				Ok((
					EntityId::from_cbor(record)?,
					SpaceEntry::from_cbor(entry)?,
					Signature::from_cbor(signature)?,
				))
			})
			.collect::<Result<Vec<_>, _>>()?;
		Ok(Self::new(entries))
	}
}

#[cfg(test)]
mod tests {
	use nibkeep_crypto::key_hash;
	use nibkeep_testkit::TestRng;

	use super::*;

	#[test]
	fn a_recovery_file_reads_back_and_opens_with_its_code_alone() {
		let mut rng = TestRng::new("a recovery file");
		let space = SpaceId::random(&mut rng);
		let member = EntityId::random(&mut rng);
		let first = Key::generate(&mut rng);
		let code = RecoveryCode::generate(&mut rng);
		let other = RecoveryCode::generate(&mut rng);
		let root = nibkeep_base::Hash::of(b"the first change");
		let (record, entry, signature) =
			signed_entry(&code, (space, member, root), &first, &mut rng);
		let file = RecoveryFile::new(vec![(record, entry.clone(), signature)]);
		let read: RecoveryFile = cbor::from_slice(&file.to_bytes()).expect("reads");
		assert_eq!(read, file);
		let opened = read.open(&code).expect("opens");
		assert_eq!((opened.space, opened.member), (space, member));
		assert_eq!(opened.root, ChangeId::from_hash(root));
		assert!(opened.key.same_as(&first));
		assert!(matches!(
			read.open(&other),
			Err(NodeError::CodeOpensNothing)
		));
		// An entry on another record than the code's opens nothing, even sealed to it.
		let moved = RecoveryFile::new(vec![(EntityId::random(&mut rng), entry, signature)]);
		assert!(matches!(
			moved.open(&code),
			Err(NodeError::CodeOpensNothing)
		));
		// A byte changed anywhere, as on a damaged drive, makes it read as damaged.
		let bytes = file.to_bytes();
		for at in [bytes.len() / 2, bytes.len() - 5] {
			let mut damaged = bytes.clone();
			damaged[at] ^= 1;
			assert!(cbor::from_slice::<RecoveryFile>(&damaged).is_err(), "{at}");
		}
	}

	/// An entry for `code` named for `member` in `space`, whose first change is `root` and
	/// first key `first`, on the code's record, with the code's signature.
	fn signed_entry(
		code: &RecoveryCode,
		(space, member, root): (SpaceId, EntityId, nibkeep_base::Hash),
		first: &Key,
		rng: &mut TestRng,
	) -> (EntityId, SpaceEntry, Signature) {
		let record = recovery_record(space, member, &code.signing_key().verifying_key());
		let entry = SpaceEntry::seal(
			&code.decryption_key().encryption_key(),
			(space, member, root),
			record,
			(first, key_hash(first)),
			rng,
		);
		let signed = RecoveryEntry::sign(code, (space, member, root), key_hash(first));
		(record, entry, signed.signature)
	}

	#[test]
	fn an_entry_that_the_code_did_not_sign_opens_nothing() {
		// A device that holds the first key, and writes the file as it updates a backup,
		// seals it to the code anew for what it chooses: the entry opens, but the code never
		// signed what it names.
		let mut rng = TestRng::new("a forged recovery file");
		let space = SpaceId::random(&mut rng);
		let member = EntityId::random(&mut rng);
		let first = Key::generate(&mut rng);
		let code = RecoveryCode::generate(&mut rng);
		let root = nibkeep_base::Hash::of(b"the first change");
		let (_, _, signature) = signed_entry(&code, (space, member, root), &first, &mut rng);
		let made_up = EntityId::random(&mut rng);
		let another_root = nibkeep_base::Hash::of(b"a space of the forger's making");
		let another_key = Key::generate(&mut rng);
		let forged = [
			// A member made up, whom the library would take for its person.
			signed_entry(&code, (space, made_up, root), &first, &mut rng),
			// A personal space of the forger's making, under the same ID.
			signed_entry(&code, (space, member, another_root), &another_key, &mut rng),
			signed_entry(&code, (space, member, another_root), &first, &mut rng),
		];
		for (record, entry, _) in forged {
			// With the genuine entry's signature, the best that it can do.
			let file = RecoveryFile::new(vec![(record, entry.clone(), signature)]);
			let read: RecoveryFile = cbor::from_slice(&file.to_bytes()).expect("reads");
			assert!(matches!(
				read.open(&code),
				Err(NodeError::RecoveryFileChanged)
			));
			// Nor does another code's signature of the same entry pass.
			let other = RecoveryCode::generate(&mut rng);
			let signed =
				RecoveryEntry::sign(&other, (entry.space, entry.member, entry.root), entry.hash);
			let file = RecoveryFile::new(vec![(record, entry, signed.signature)]);
			assert!(matches!(
				file.open(&code),
				Err(NodeError::RecoveryFileChanged)
			));
		}
		// Nor does one that keeps what the code signed, with another key sealed to it.
		let (record, _, signature) = signed_entry(&code, (space, member, root), &first, &mut rng);
		let (_, mut resealed, _) =
			signed_entry(&code, (space, member, root), &another_key, &mut rng);
		resealed.hash = key_hash(&first);
		let file = RecoveryFile::new(vec![(record, resealed, signature)]);
		assert!(matches!(
			file.open(&code),
			Err(NodeError::RecoveryFileChanged)
		));
		// The genuine entry beside a forged one still opens.
		let genuine = signed_entry(&code, (space, member, root), &first, &mut rng);
		let (record, entry, _) = signed_entry(&code, (space, made_up, root), &first, &mut rng);
		let file = RecoveryFile::new(vec![genuine, (record, entry, signature)]);
		assert_eq!(file.open(&code).expect("opens").member, member);
	}
}
