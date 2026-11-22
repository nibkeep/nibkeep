//! Accounts of copies: what each replica of a space holds, which it tells the others in
//! pages that it signs and seals with the space key, and that they pass on
//! ([Log](../../../docs/spec/log.md#11-accounts-of-copies)).

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{Envelope, Key, Signature, SigningKey, VerifyingKey};

use crate::{LogError, SpaceKeys};

/// The context of the key, derived from a space key, that seals the pages of accounts.
const ACCOUNTS_CONTEXT: &str = "nibkeep 2026-11 accounts";

/// The context of a page's signature.
const SIGNATURE_CONTEXT: &str = "nibkeep account";

/// The version of the format of pages.
const VERSION: u64 = 1;

/// How long a page may be, 4 MiB: a page of blobs of a space of ten million blobs takes about
/// half of it, and longer pages are refused, so that no replica fills another's disk with
/// them.
pub const MAX_PAGE_LEN: usize = 4 * 1024 * 1024;

/// Which page of an account a page is: the one with the disk, the marks and the heads, or
/// one of the pages of the sealed blobs or of the plain copies, by the first byte of the
/// sealed hashes' digests or of the files' IDs that it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageKind {
	/// The disk, the device's marks, and the heads.
	Head,
	/// The sealed blobs whose sealed hash's digest starts with this byte.
	Blobs(u8),
	/// The plain copies of the files whose ID starts with this byte.
	Copies(u8),
}

impl PageKind {
	/// Its number: 0 for the head, 256 plus the byte for blobs, 512 plus the byte for plain
	/// copies.
	#[must_use]
	pub fn number(self) -> u16 {
		match self {
			Self::Head => 0,
			Self::Blobs(byte) => 0x100 + u16::from(byte),
			Self::Copies(byte) => 0x200 + u16::from(byte),
		}
	}

	/// The kind of page that a number names, if any.
	#[must_use]
	pub fn from_number(number: u64) -> Option<Self> {
		match number {
			0 => Some(Self::Head),
			0x100..=0x1FF => u8::try_from(number - 0x100).ok().map(Self::Blobs),
			0x200..=0x2FF => u8::try_from(number - 0x200).ok().map(Self::Copies),
			_ => None,
		}
	}
}

/// What a device does with the files of its spaces, as its user marked it
/// ([technical design](../../../docs/Technical-design.md#96-where-the-copies-are)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceRole {
	/// An everyday device, such as a phone or a laptop: every device that is not marked.
	#[default]
	Everyday,
	/// An archive node, such as a NAS or a desktop, which keeps what policies assign it.
	Archive,
}

impl DeviceRole {
	/// Its name, as text: `everyday` or `archive`.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Everyday => "everyday",
			Self::Archive => "archive",
		}
	}

	/// The role that a name names, if any.
	#[must_use]
	pub fn from_name(name: &str) -> Option<Self> {
		match name {
			"everyday" => Some(Self::Everyday),
			"archive" => Some(Self::Archive),
			_ => None,
		}
	}
}

/// The head of an account: the disk, its device's marks, the space's changes it holds, and
/// what the replica's checks found damaged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
	/// The disk's ID in the space.
	pub disk: Hash,
	/// Its device's role.
	pub role: DeviceRole,
	/// Whether its device is kept away from home.
	pub away: bool,
	/// The replica's heads: the space's changes it holds are these and what they build on.
	pub heads: Vec<ChangeId>,
	/// How many of its copies of the space's files its checks found damaged in the last 4
	/// weeks.
	pub damaged: u64,
	/// How many of its copies that checks found damaged are damaged still: sealed blobs that
	/// wait for an intact copy, and originals that do not hold their content.
	pub unrepaired: u64,
	/// How many days its device waits before it destroys what a purge destroys, as the wait
	/// in force when the page was issued: 0 for none
	/// ([technical design](../../../docs/Technical-design.md#911-copies-that-wait)).
	pub waits: u64,
}

/// The head of the account of a backup, which a replica that updated it issues: the
/// backup's disk, its mark, the space's changes it holds, what checks found damaged, and the
/// number of the update that the page tells of
/// ([technical design](../../../docs/Technical-design.md#910-backups)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupHead {
	/// The backup's disk's ID in the space.
	pub disk: Hash,
	/// Whether it is kept away from home.
	pub away: bool,
	/// The heads of the space's changes it holds.
	pub heads: Vec<ChangeId>,
	/// How many of its copies checks found damaged in the 4 weeks before the update.
	pub damaged: u64,
	/// How many of those it holds no intact copy of yet.
	pub unrepaired: u64,
	/// The number of the update.
	pub update: u64,
	/// How many days it waits before it destroys what a purge destroys, after the update that
	/// first brings the purge: 0 for none.
	pub waits: u64,
}

/// What a page tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageBody {
	/// The head of the account.
	Head(Head),
	/// The head of the account of a backup.
	BackupHead(BackupHead),
	/// The sealed blobs whose sealed hash's digest starts with the byte, each with when the
	/// replica last read it in full and found it intact, in milliseconds since 1970, or 0
	/// if it never did; sorted by sealed hash.
	Blobs(u8, Vec<(Hash, u64)>),
	/// The plain copies, on the replica's device and unchanged, of the files whose ID starts
	/// with the byte: each file, the content the copy holds, and when the replica last read a
	/// copy of it in full and found the content, in milliseconds since 1970, or 0 if it never
	/// did; sorted by file and content.
	Copies(u8, Vec<(EntityId, Hash, u64)>),
}

impl PageBody {
	/// The kind of page it is.
	#[must_use]
	pub fn kind(&self) -> PageKind {
		match self {
			Self::Head(_) | Self::BackupHead(_) => PageKind::Head,
			Self::Blobs(byte, _) => PageKind::Blobs(*byte),
			Self::Copies(byte, _) => PageKind::Copies(*byte),
		}
	}

	fn to_cbor(&self) -> Value {
		match self {
			Self::Head(head) => Value::Array(vec![
				head.disk.to_cbor(),
				head.role.as_str().to_cbor(),
				head.away.to_cbor(),
				head.heads.to_cbor(),
				head.damaged.to_cbor(),
				head.unrepaired.to_cbor(),
				head.waits.to_cbor(),
			]),
			Self::BackupHead(head) => Value::Array(vec![
				head.disk.to_cbor(),
				head.away.to_cbor(),
				head.heads.to_cbor(),
				head.damaged.to_cbor(),
				head.unrepaired.to_cbor(),
				head.update.to_cbor(),
				head.waits.to_cbor(),
			]),
			Self::Blobs(_, blobs) => Value::Array(
				blobs
					.iter()
					.map(|(sealed, checked)| {
						Value::Array(vec![sealed.to_cbor(), checked.to_cbor()])
					})
					.collect(),
			),
			Self::Copies(_, copies) => Value::Array(
				copies
					.iter()
					.map(|(file, content, checked)| {
						Value::Array(vec![file.to_cbor(), content.to_cbor(), checked.to_cbor()])
					})
					.collect(),
			),
		}
	}

	/// Reads the body of a page of the kind given, of a backup's account if `backup` says
	/// so, checking that what it lists is sorted, once each, and on the page that its first
	/// byte names.
	fn from_cbor(kind: PageKind, backup: bool, value: &Value) -> Result<Self, LogError> {
		let bad = |what: &'static str| LogError::Account(what);
		let Value::Array(items) = value else {
			return Err(bad("a page's body is no array"));
		};
		match kind {
			PageKind::Head if backup => read_backup_head(items),
			PageKind::Copies(_) if backup => Err(bad("a backup's account lists no plain copies")),
			PageKind::Head => read_head(items),
			PageKind::Blobs(byte) => {
				let mut blobs = Vec::with_capacity(items.len());
				for item in items {
					let Value::Array(pair) = item else {
						return Err(bad("a blob is [sealed hash, checked]"));
					};
					let [sealed, checked] = pair.as_slice() else {
						return Err(bad("a blob is [sealed hash, checked]"));
					};
					let sealed = Hash::from_cbor(sealed).map_err(LogError::Operations)?;
					if sealed.digest().first() != Some(&byte) {
						return Err(bad("a blob on another page"));
					}
					blobs.push((
						sealed,
						u64::from_cbor(checked).map_err(LogError::Operations)?,
					));
				}
				if !blobs.windows(2).all(|pair| pair[0].0 < pair[1].0) {
					return Err(bad("blobs that are not sorted once each"));
				}
				Ok(Self::Blobs(byte, blobs))
			}
			PageKind::Copies(byte) => {
				let mut copies = Vec::with_capacity(items.len());
				for item in items {
					let Value::Array(triple) = item else {
						return Err(bad("a plain copy is [file, content, checked]"));
					};
					let [file, content, checked] = triple.as_slice() else {
						return Err(bad("a plain copy is [file, content, checked]"));
					};
					let file = EntityId::from_cbor(file).map_err(LogError::Operations)?;
					if file.as_bytes()[0] != byte {
						return Err(bad("a plain copy on another page"));
					}
					copies.push((
						file,
						Hash::from_cbor(content).map_err(LogError::Operations)?,
						u64::from_cbor(checked).map_err(LogError::Operations)?,
					));
				}
				if !copies
					.windows(2)
					.all(|pair| (pair[0].0, pair[0].1) < (pair[1].0, pair[1].1))
				{
					return Err(bad("plain copies that are not sorted once each"));
				}
				Ok(Self::Copies(byte, copies))
			}
		}
	}
}

/// One page of a replica's account of what it holds in a space, signed by the replica and
/// sealed with a space key.
///
/// In CBOR, a page is `[1, header, body, signature]`, where the header is `[space, epoch,
/// author, page, issued]` and the body an envelope ([`Envelope`]) sealed with the key derived
/// from the space key of `epoch`, with the header's encoding as associated data; the
/// signature signs the encoding of `[header, body]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPage {
	space: SpaceId,
	epoch: u64,
	author: VerifyingKey,
	/// The backup whose account it is a page of, if it is not the author's own.
	about: Option<EntityId>,
	kind: PageKind,
	issued: u64,
	body: Envelope,
	signature: Signature,
}

impl AccountPage {
	/// Issues a page of the account of `signing`'s replica in `space`, telling `body`,
	/// sealed with the space key that new changes are wrapped with, and issued at `issued`,
	/// in milliseconds since 1970.
	#[must_use]
	pub fn issue(
		space: SpaceId,
		keys: &SpaceKeys,
		signing: &SigningKey,
		(issued, body): (u64, &PageBody),
		rng: &mut impl CryptoRng,
	) -> Self {
		Self::issue_of(space, keys, signing, None, (issued, body), rng)
	}

	/// Issues a page of the account of a backup in `space`, which `signing`'s replica
	/// updated, as [`AccountPage::issue`] issues one of its own.
	#[must_use]
	pub fn issue_about(
		space: SpaceId,
		keys: &SpaceKeys,
		signing: &SigningKey,
		backup: EntityId,
		(issued, body): (u64, &PageBody),
		rng: &mut impl CryptoRng,
	) -> Self {
		Self::issue_of(space, keys, signing, Some(backup), (issued, body), rng)
	}

	fn issue_of(
		space: SpaceId,
		keys: &SpaceKeys,
		signing: &SigningKey,
		about: Option<EntityId>,
		(issued, body): (u64, &PageBody),
		rng: &mut impl CryptoRng,
	) -> Self {
		let (epoch, space_key) = keys.newest();
		let author = signing.verifying_key();
		let kind = body.kind();
		let header = header(space, epoch, &author, (about, kind), issued);
		let sealed = Envelope::seal(
			&accounts_key(space_key),
			&cbor::encode(&body.to_cbor()),
			&cbor::encode(&header),
			rng,
		);
		let signature = signing.sign(SIGNATURE_CONTEXT, &signed(&header, &sealed));
		Self {
			space,
			epoch,
			author,
			about,
			kind,
			issued,
			body: sealed,
			signature,
		}
	}

	/// Reads a page from its encoding, and checks its signature: one that does not read, is
	/// longer than [`MAX_PAGE_LEN`], or whose author did not sign it, is refused.
	///
	/// # Errors
	///
	/// Returns [`LogError::Account`] or [`LogError::Operations`] if the bytes are not a
	/// page, and [`LogError::InvalidSignature`] if its signature does not check.
	pub fn from_bytes(bytes: &[u8]) -> Result<Self, LogError> {
		let bad = |what: &'static str| LogError::Account(what);
		if bytes.len() > MAX_PAGE_LEN {
			return Err(bad("a page longer than 4 MiB"));
		}
		let value = cbor::decode(bytes)
			.map_err(|error| LogError::Operations(cbor::Error::Decode(error)))?;
		let Value::Array(items) = &value else {
			return Err(bad("a page is [1, header, body, signature]"));
		};
		let [version, header, body, signature] = items.as_slice() else {
			return Err(bad("a page is [1, header, body, signature]"));
		};
		if u64::from_cbor(version).ok() != Some(VERSION) {
			return Err(bad("a page of another version"));
		}
		let Value::Array(fields) = header else {
			return Err(bad("a header is [space, epoch, author, page, issued]"));
		};
		let [space, epoch, author, kind, issued] = fields.as_slice() else {
			return Err(bad("a header is [space, epoch, author, page, issued]"));
		};
		let operations = LogError::Operations;
		// A page of the author's own account names it by its number, and one of a backup's
		// by the backup and its number.
		let (about, number) = match kind {
			Value::Array(pair) => {
				let [backup, number] = pair.as_slice() else {
					return Err(bad("a backup's page is [backup, number]"));
				};
				(
					Some(EntityId::from_cbor(backup).map_err(operations)?),
					number,
				)
			}
			number => (None, number),
		};
		let kind = PageKind::from_number(u64::from_cbor(number).map_err(operations)?)
			.ok_or(bad("a page that is none of an account's"))?;
		if about.is_some() && matches!(kind, PageKind::Copies(_)) {
			return Err(bad("a backup's account lists no plain copies"));
		}
		let page = Self {
			space: SpaceId::from_cbor(space).map_err(operations)?,
			epoch: u64::from_cbor(epoch).map_err(operations)?,
			author: VerifyingKey::from_cbor(author).map_err(operations)?,
			about,
			kind,
			issued: u64::from_cbor(issued).map_err(operations)?,
			body: Envelope::from_cbor(body).map_err(operations)?,
			signature: Signature::from_cbor(signature).map_err(operations)?,
		};
		page.author
			.verify(
				SIGNATURE_CONTEXT,
				&signed(header, &page.body),
				&page.signature,
			)
			.map_err(|_| LogError::InvalidSignature)?;
		Ok(page)
	}

	/// Its encoding, as it is stored and sent.
	#[must_use]
	pub fn to_bytes(&self) -> Vec<u8> {
		cbor::encode(&Value::Array(vec![
			VERSION.to_cbor(),
			self.header(),
			self.body.to_cbor(),
			self.signature.to_cbor(),
		]))
	}

	/// The BLAKE3 hash of its encoding, by which reconciliation knows it.
	#[must_use]
	pub fn id(&self) -> Hash {
		Hash::of(&self.to_bytes())
	}

	/// Opens what the page tells, with the space key it was sealed with.
	///
	/// # Errors
	///
	/// Returns [`LogError::UnknownEpoch`] if `keys` lack that key, [`LogError::Decryption`]
	/// if the body does not open, and [`LogError::Account`] or [`LogError::Operations`] if
	/// what it holds does not read.
	pub fn open(&self, keys: &SpaceKeys) -> Result<PageBody, LogError> {
		let space_key = keys
			.get(self.epoch)
			.ok_or(LogError::UnknownEpoch(self.epoch))?;
		let plaintext = self
			.body
			.open(&accounts_key(space_key), &cbor::encode(&self.header()))
			.map_err(|_| LogError::Decryption)?;
		let value = cbor::decode(&plaintext)
			.map_err(|error| LogError::Operations(cbor::Error::Decode(error)))?;
		PageBody::from_cbor(self.kind, self.about.is_some(), &value)
	}

	/// The space whose account it is a page of.
	#[must_use]
	pub const fn space(&self) -> SpaceId {
		self.space
	}

	/// The verifying key of the replica that issued it.
	#[must_use]
	pub const fn author(&self) -> &VerifyingKey {
		&self.author
	}

	/// Which page it is.
	#[must_use]
	pub const fn kind(&self) -> PageKind {
		self.kind
	}

	/// The backup whose account it is a page of, if it is not its author's own.
	#[must_use]
	pub const fn about(&self) -> Option<EntityId> {
		self.about
	}

	/// When it was issued, in milliseconds since 1970.
	#[must_use]
	pub const fn issued(&self) -> u64 {
		self.issued
	}

	/// The number of the space key that seals it.
	#[must_use]
	pub const fn epoch(&self) -> u64 {
		self.epoch
	}

	fn header(&self) -> Value {
		header(
			self.space,
			self.epoch,
			&self.author,
			(self.about, self.kind),
			self.issued,
		)
	}
}

fn header(
	space: SpaceId,
	epoch: u64,
	author: &VerifyingKey,
	(about, kind): (Option<EntityId>, PageKind),
	issued: u64,
) -> Value {
	let page = match about {
		None => kind.number().to_cbor(),
		Some(backup) => Value::Array(vec![backup.to_cbor(), kind.number().to_cbor()]),
	};
	Value::Array(vec![
		space.to_cbor(),
		epoch.to_cbor(),
		author.to_cbor(),
		page,
		issued.to_cbor(),
	])
}

/// What a page's signature signs: the encoding of `[header, body]`.
fn signed(header: &Value, body: &Envelope) -> Vec<u8> {
	cbor::encode(&Value::Array(vec![header.clone(), body.to_cbor()]))
}

/// The key that seals pages, derived from a space key.
fn accounts_key(space_key: &Key) -> Key {
	space_key.derive(ACCOUNTS_CONTEXT)
}

/// Reads a replica's head page.
fn read_head(items: &[Value]) -> Result<PageBody, LogError> {
	let bad = |what: &'static str| LogError::Account(what);
	let [disk, role, away, heads, damaged, unrepaired, waits] = items else {
		return Err(bad(
			"a head page is [disk, role, away, heads, damaged, unrepaired, waits]",
		));
	};
	let role = DeviceRole::from_name(&String::from_cbor(role).map_err(LogError::Operations)?)
		.ok_or(bad("a role that is neither everyday nor archive"))?;
	let heads = Vec::<ChangeId>::from_cbor(heads).map_err(LogError::Operations)?;
	if !heads.windows(2).all(|pair| pair[0] < pair[1]) {
		return Err(bad("heads that are not sorted once each"));
	}
	Ok(PageBody::Head(Head {
		disk: Hash::from_cbor(disk).map_err(LogError::Operations)?,
		role,
		away: bool::from_cbor(away).map_err(LogError::Operations)?,
		heads,
		damaged: u64::from_cbor(damaged).map_err(LogError::Operations)?,
		unrepaired: u64::from_cbor(unrepaired).map_err(LogError::Operations)?,
		waits: u64::from_cbor(waits).map_err(LogError::Operations)?,
	}))
}

/// Reads a backup's head page.
fn read_backup_head(items: &[Value]) -> Result<PageBody, LogError> {
	let bad = |what: &'static str| LogError::Account(what);
	let [disk, away, heads, damaged, unrepaired, update, waits] = items else {
		return Err(bad(
			"a backup's head page is [disk, away, heads, damaged, unrepaired, update, waits]",
		));
	};
	let heads = Vec::<ChangeId>::from_cbor(heads).map_err(LogError::Operations)?;
	if !heads.windows(2).all(|pair| pair[0] < pair[1]) {
		return Err(bad("heads that are not sorted once each"));
	}
	Ok(PageBody::BackupHead(BackupHead {
		disk: Hash::from_cbor(disk).map_err(LogError::Operations)?,
		away: bool::from_cbor(away).map_err(LogError::Operations)?,
		heads,
		damaged: u64::from_cbor(damaged).map_err(LogError::Operations)?,
		unrepaired: u64::from_cbor(unrepaired).map_err(LogError::Operations)?,
		update: u64::from_cbor(update).map_err(LogError::Operations)?,
		waits: u64::from_cbor(waits).map_err(LogError::Operations)?,
	}))
}

#[cfg(test)]
mod tests {
	use nibkeep_testkit::TestRng;

	use super::*;

	fn keys(rng: &mut TestRng) -> SpaceKeys {
		SpaceKeys::new(0, Key::generate(rng))
	}

	fn head() -> PageBody {
		let mut heads = vec![
			ChangeId::from_hash(Hash::of(b"a")),
			ChangeId::from_hash(Hash::of(b"b")),
		];
		heads.sort();
		PageBody::Head(Head {
			disk: Hash::of(b"disk"),
			role: DeviceRole::Archive,
			away: true,
			heads,
			damaged: 2,
			unrepaired: 1,
			waits: 30,
		})
	}

	#[test]
	fn a_page_reads_as_it_was_issued_and_opens_with_the_space_key() {
		let mut rng = TestRng::new("account pages");
		let keys = keys(&mut rng);
		let signing = SigningKey::generate(&mut rng);
		let space = SpaceId::random(&mut rng);
		let body = head();
		let page = AccountPage::issue(space, &keys, &signing, (1_000, &body), &mut rng);
		let read = AccountPage::from_bytes(&page.to_bytes()).expect("reads");
		assert_eq!(read, page);
		assert_eq!(read.to_bytes(), page.to_bytes());
		assert_eq!(read.author(), &signing.verifying_key());
		assert_eq!(
			(read.kind(), read.issued(), read.space()),
			(PageKind::Head, 1_000, space)
		);
		assert_eq!(read.open(&keys).expect("opens"), body);
		// Without the space key, nothing of it opens.
		assert!(matches!(
			read.open(&SpaceKeys::new(0, Key::generate(&mut rng))),
			Err(LogError::Decryption)
		));
	}

	#[test]
	fn a_head_page_without_its_wait_does_not_read() {
		let items = [
			Hash::of(b"disk").to_cbor(),
			"archive".to_owned().to_cbor(),
			true.to_cbor(),
			Vec::<ChangeId>::new().to_cbor(),
			2_u64.to_cbor(),
			1_u64.to_cbor(),
			30_u64.to_cbor(),
		];
		assert!(read_head(&items).is_ok());
		assert!(matches!(read_head(&items[..6]), Err(LogError::Account(_))));
		let backup = [
			Hash::of(b"disk").to_cbor(),
			true.to_cbor(),
			Vec::<ChangeId>::new().to_cbor(),
			2_u64.to_cbor(),
			1_u64.to_cbor(),
			4_u64.to_cbor(),
			30_u64.to_cbor(),
		];
		assert!(read_backup_head(&backup).is_ok());
		assert!(matches!(
			read_backup_head(&backup[..6]),
			Err(LogError::Account(_))
		));
	}

	#[test]
	fn a_page_that_another_key_signed_or_that_was_changed_is_refused() {
		let mut rng = TestRng::new("forged account pages");
		let keys = keys(&mut rng);
		let signing = SigningKey::generate(&mut rng);
		let space = SpaceId::random(&mut rng);
		let body = PageBody::Blobs(7, Vec::new());
		let page = AccountPage::issue(space, &keys, &signing, (5, &body), &mut rng);
		// Another author named, or another time, breaks the signature.
		let mut forged = page.clone();
		forged.author = SigningKey::generate(&mut rng).verifying_key();
		assert!(matches!(
			AccountPage::from_bytes(&forged.to_bytes()),
			Err(LogError::InvalidSignature)
		));
		let mut later = page;
		later.issued = 6;
		assert!(matches!(
			AccountPage::from_bytes(&later.to_bytes()),
			Err(LogError::InvalidSignature)
		));
	}

	#[test]
	fn entries_on_the_wrong_page_or_out_of_order_do_not_open() {
		let mut rng = TestRng::new("account entries");
		let keys = keys(&mut rng);
		let signing = SigningKey::generate(&mut rng);
		let space = SpaceId::random(&mut rng);
		let sealed = Hash::of(b"a blob");
		let byte = sealed.digest()[0];
		let fine = PageBody::Blobs(byte, vec![(sealed, 3)]);
		let page = AccountPage::issue(space, &keys, &signing, (1, &fine), &mut rng);
		assert_eq!(page.open(&keys).expect("opens"), fine);
		let elsewhere = PageBody::Blobs(byte.wrapping_add(1), vec![(sealed, 3)]);
		let page = AccountPage::issue(space, &keys, &signing, (1, &elsewhere), &mut rng);
		assert!(matches!(page.open(&keys), Err(LogError::Account(_))));
		let twice = PageBody::Blobs(byte, vec![(sealed, 3), (sealed, 4)]);
		let page = AccountPage::issue(space, &keys, &signing, (1, &twice), &mut rng);
		assert!(matches!(page.open(&keys), Err(LogError::Account(_))));
	}

	#[test]
	fn plain_copies_tell_when_each_was_checked_and_are_listed_once() {
		let mut rng = TestRng::new("account copies");
		let keys = keys(&mut rng);
		let signing = SigningKey::generate(&mut rng);
		let space = SpaceId::random(&mut rng);
		let file = EntityId::from_bytes([3; 16]);
		let mut contents = [Hash::of(b"a film"), Hash::of(b"its new version")];
		contents.sort();
		let listed = PageBody::Copies(3, vec![(file, contents[0], 7), (file, contents[1], 0)]);
		let page = AccountPage::issue(space, &keys, &signing, (1, &listed), &mut rng);
		let read = AccountPage::from_bytes(&page.to_bytes()).expect("reads");
		assert_eq!(read.open(&keys).expect("opens"), listed);
		// One file and content listed twice, with two times, is refused.
		let twice = PageBody::Copies(3, vec![(file, contents[0], 7), (file, contents[0], 9)]);
		let page = AccountPage::issue(space, &keys, &signing, (1, &twice), &mut rng);
		assert!(matches!(page.open(&keys), Err(LogError::Account(_))));
	}

	#[test]
	fn a_page_about_a_backup_reads_with_its_backup_and_update() {
		let mut rng = TestRng::new("backup pages");
		let keys = keys(&mut rng);
		let signing = SigningKey::generate(&mut rng);
		let space = SpaceId::random(&mut rng);
		let backup = EntityId::from_bytes([9; 16]);
		let body = PageBody::BackupHead(BackupHead {
			disk: Hash::of(b"stick"),
			away: true,
			heads: vec![ChangeId::from_hash(Hash::of(b"a"))],
			damaged: 1,
			unrepaired: 0,
			update: 12,
			waits: 14,
		});
		let page = AccountPage::issue_about(space, &keys, &signing, backup, (7, &body), &mut rng);
		let read = AccountPage::from_bytes(&page.to_bytes()).expect("reads");
		assert_eq!(read, page);
		assert_eq!((read.about(), read.kind()), (Some(backup), PageKind::Head));
		assert_eq!(read.open(&keys).expect("opens"), body);
		// The replica's own head, put on a backup's page, does not open, nor the other way.
		let own = AccountPage::issue_about(space, &keys, &signing, backup, (8, &head()), &mut rng);
		assert!(own.open(&keys).is_err());
		let swapped = AccountPage::issue(space, &keys, &signing, (9, &body), &mut rng);
		assert!(swapped.open(&keys).is_err());
	}

	#[test]
	fn page_numbers_name_each_page_once() {
		for kind in [
			PageKind::Head,
			PageKind::Blobs(0),
			PageKind::Blobs(255),
			PageKind::Copies(9),
		] {
			assert_eq!(PageKind::from_number(u64::from(kind.number())), Some(kind));
		}
		assert_eq!(PageKind::from_number(1), None);
		assert_eq!(PageKind::from_number(0x300), None);
	}
}
