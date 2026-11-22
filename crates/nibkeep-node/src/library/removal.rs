//! Removing the copies that this replica no longer keeps, once copies on other disks have
//! proven, right then, that they stand for them ([technical design: the planner]): a pass
//! plans the removals and the challenges to put to replicas found on the network, the syncs
//! with those replicas put them, and the copies whose proofs came in time go.
//!
//! [technical design: the planner]: ../../../../docs/Technical-design.md#99-the-planner

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{BlobReader, VerifyingKey};
use nibkeep_model::storage::Statements;
use nibkeep_model::{Value, fields};
use nibkeep_std::{CONTENT, FILE};
use nibkeep_sync::Challenge;

use super::Library;
use super::accounts::{Checked, disk_id};
use super::behind::WaitingMove;
use super::checking::COUNTS_FOR_MILLIS;
use super::planner::Keeps;
use super::policies::{Kept, facts_of, own_choices};
use crate::role::Records;
use crate::{DeviceRole, Needs, NodeError, Replica};

/// How long the proofs of a replica stand after it was asked for them: 8 minutes, short of
/// the 10 for which it holds what it proved, so that it holds them still when the copies
/// here go, even if the clocks of the two run a little apart.
const PROOFS_STAND: Duration = Duration::from_secs(480);

/// How many copies a pass removes at most, so that a device removes a little at a time.
pub const REMOVALS_AT_ONCE: usize = 1024;

/// A copy that a pass is to remove: a sealed blob of a space that this replica holds and no
/// longer keeps.
struct Candidate {
	space: SpaceId,
	sealed: Hash,
	content: Hash,
	/// Its size in bytes.
	size: u64,
	/// For each disk that may stand for it, the replica there asked to prove its copy, with
	/// whether the disk is an archive node's.
	provers: Vec<(Prover, bool)>,
	/// How many of those are to prove their copies, how many of those archive nodes', and
	/// how many kept away from home.
	needed: usize,
	archived: usize,
	away: usize,
	/// How many copies this replica's own disk keeps besides: one where it holds an
	/// unchanged original of a content that its policy keeps in place.
	own: usize,
	/// The sealed copy of its content with a smaller sealed hash that this replica holds,
	/// if it is that copy's twin: one that goes without proofs, as the disk keeps the
	/// content in the other.
	twin_of: Option<Hash>,
}

/// The challenges put to a replica in a space, by the hash each names and whether it is
/// plain, each with the answer expected.
type ChallengesOf = BTreeMap<(Hash, bool), (Challenge, [u8; 32])>;

/// What a pass asks of one replica: the challenges to put to it in each space, each with
/// the answer that this replica's own copy gives; when they were put; and which it proved.
#[derive(Default)]
struct Asking {
	challenges: BTreeMap<SpaceId, ChallengesOf>,
	asked: Option<Instant>,
	proven: BTreeSet<(SpaceId, Hash, bool)>,
}

/// A pass of removals, as [`Library::plan_removals`] plans it: the copies to remove, and
/// what to ask the replicas that are to prove theirs, which a sync with each of
/// [`Removals::provers`] puts to them ([`crate::Syncing::proving`]).
#[derive(Default)]
pub struct Removals {
	candidates: Vec<Candidate>,
	asking: BTreeMap<[u8; 32], Asking>,
	provers: Vec<Replica>,
	/// How many copies that it no longer keeps stay for now: no disks found on the network
	/// can stand for them, their policy needs a copy away from home, or the pass had room for
	/// no more.
	waiting: usize,
	/// Whether more copies could be removed than the pass had room for.
	more: bool,
	/// This replica's moves whose leftovers wait for their content's copies in the new space
	/// to be proven.
	moves: Vec<MovePurge>,
	/// The backups plugged into this computer that may stand for copies, by the key that
	/// stands for each among the provers, with their folders: this replica reads them itself.
	backups: BTreeMap<[u8; 32], PathBuf>,
	/// Of those, the ones kept away from home.
	away: BTreeSet<[u8; 32]>,
}

/// A move of this replica's whose leftovers a pass purges once the copies in the new space
/// of the content they hold are proven: the old space and entity, and for each content, the
/// new space, the sealed copy there that this replica holds, and the replicas chosen to
/// prove theirs.
struct MovePurge {
	space: SpaceId,
	entity: EntityId,
	/// The space it moved into, as the pass planned.
	now_in: SpaceId,
	proofs: Vec<(SpaceId, Hash, Hash, Vec<Prover>)>,
}

/// A replica asked to prove a copy, by its key, and whether it proves an original, by the
/// content's own bytes, rather than a sealed copy.
type Prover = ([u8; 32], bool);

/// What the copies of a content of a space need, as `needs` gives the strictest policy of
/// the files that hold or held each content: where this replica keeps nothing of the space,
/// as it drops it, a content that no file holds, as a thumbnail's or a secret file's, needs
/// one copy elsewhere, as every replica keeps it; elsewhere, what a file needs that no rule
/// chooses for.
fn needs_of(needs: &BTreeMap<Hash, Needs>, content: &Hash, keeps: &Keeps) -> Needs {
	needs
		.get(content)
		.copied()
		.unwrap_or(if *keeps == Keeps::Nothing {
			Needs::ONE
		} else {
			Needs::FALLBACK
		})
}

impl Removals {
	/// The replicas to sync with, so that they prove their copies.
	#[must_use]
	pub fn provers(&self) -> &[Replica] {
		&self.provers
	}

	/// The challenges to put to `peer` in a space, noting when they were first put to it.
	pub(crate) fn challenges(&mut self, space: SpaceId, peer: &VerifyingKey) -> Vec<Challenge> {
		let Some(asking) = self.asking.get_mut(peer.as_bytes()) else {
			return Vec::new();
		};
		let Some(challenges) = asking.challenges.get(&space) else {
			return Vec::new();
		};
		asking.asked.get_or_insert_with(Instant::now);
		challenges
			.values()
			.map(|(challenge, _)| challenge.clone())
			.collect()
	}

	/// Takes `peer`'s answers to the challenges put to it in a space: those that its own
	/// copy also gives prove its copies.
	pub(crate) fn proved(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
		answers: &[Option<[u8; 32]>],
	) {
		let Some(asking) = self.asking.get_mut(peer.as_bytes()) else {
			return;
		};
		let Some(expected) = asking.challenges.get(&space) else {
			return;
		};
		for (challenge, answer) in challenges.iter().zip(answers) {
			if expected
				.get(&(challenge.sealed, challenge.plain))
				.is_some_and(|(put, answered)| put == challenge && *answer == Some(*answered))
			{
				asking
					.proven
					.insert((space, challenge.sealed, challenge.plain));
			}
		}
	}

	/// Whether enough of the replicas asked for a candidate proved their copies, while their
	/// proofs stand: as many as it needs, and as many of them archive nodes' as it needs;
	/// one that proved an original only where `in_place` says that an original counts.
	fn proven(&self, candidate: &Candidate, in_place: bool) -> bool {
		let proved: Vec<&(Prover, bool)> = candidate
			.provers
			.iter()
			.filter(|(prover, _)| in_place || !prover.1)
			.filter(|(prover, _)| {
				self.all_proved(
					candidate.space,
					(&candidate.sealed, &candidate.content),
					std::slice::from_ref(prover),
				)
			})
			.collect();
		proved.len() >= candidate.needed
			&& proved.iter().filter(|(_, archive)| *archive).count() >= candidate.archived
			&& proved
				.iter()
				.filter(|((key, _), _)| self.away.contains(key))
				.count()
				>= candidate.away
	}

	/// Whether each of `provers` proved its copy, of a sealed blob of a space or of its
	/// content as an original, while its proofs stand.
	fn all_proved(
		&self,
		space: SpaceId,
		(sealed, content): (&Hash, &Hash),
		provers: &[Prover],
	) -> bool {
		provers.iter().all(|(prover, plain)| {
			let of = if *plain { *content } else { *sealed };
			self.asking.get(prover).is_some_and(|asking| {
				asking
					.asked
					.is_some_and(|asked| asked.elapsed() < PROOFS_STAND)
					&& asking.proven.contains(&(space, of, *plain))
			})
		})
	}

	/// Notes a challenge to put to a replica of a sealed blob of a space, with the answer
	/// expected; one put to it already for that blob stands, and serves both. A backup
	/// plugged in here answers at once, as this replica reads its copy itself.
	fn ask(&mut self, prover: [u8; 32], space: SpaceId, challenge: Challenge, expected: [u8; 32]) {
		let folder = self.backups.get(&prover).cloned();
		let asking = self.asking.entry(prover).or_default();
		let key = (challenge.sealed, challenge.plain);
		if let Some(folder) = folder
			&& !challenge.plain
		{
			let answered = File::open(folder.join(crate::backup::blob_path(&challenge.sealed)))
				.ok()
				.and_then(|mut file| answer_from_file(&challenge, &mut file).ok());
			if answered == Some(expected) {
				asking.asked.get_or_insert_with(Instant::now);
				asking.proven.insert((space, challenge.sealed, false));
			}
		}
		asking
			.challenges
			.entry(space)
			.or_default()
			.entry(key)
			.or_insert((challenge, expected));
	}
}

/// What a pass of removals did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
	/// How many copies it removed.
	pub copies: usize,
	/// Their bytes.
	pub bytes: u64,
	/// How many copies that this replica no longer keeps stay for now: their copies
	/// elsewhere were not all found or proven, their policy needs a copy away from home,
	/// another replica holds them for its own removal, or the pass had room for no more.
	pub waiting: usize,
	/// Whether more could be removed than the pass had room for, so that another pass
	/// soon removes more.
	pub more: bool,
	/// How many of this replica's moves it purged what they left behind, once the copies
	/// in the new space of the content they held were proven.
	pub moves: usize,
	/// The copies of this device's checkouts that those purges deleted.
	pub deleted: Vec<std::path::PathBuf>,
	/// Why purges that it tried failed, which the next pass tries again.
	pub failed: Vec<String>,
}

/// The replicas found on each disk other than this replica's, in the order of their keys:
/// each with the sealed blobs, and the contents of unchanged originals, that its latest
/// account lists, and whether the disk is an archive node's.
type OnDisks = BTreeMap<Hash, Vec<OnDisk>>;

/// What may stand for copies elsewhere: the keys of the replicas found on the network, and
/// the backups plugged in here.
type Standing<'a> = (&'a BTreeSet<[u8; 32]>, &'a [BackupHere]);

/// A backup plugged into this computer, as a pass of removals reads it.
struct BackupHere {
	/// The key that stands for it among the provers.
	prover: [u8; 32],
	/// What names its disk.
	disk: Vec<u8>,
	/// The spaces whose items it holds here.
	spaces: Vec<SpaceId>,
	/// Whether it is kept away from home.
	away: bool,
	/// The sealed blobs it holds in files of their own, with when each was last checked.
	sealed: Checked,
	/// Its folder.
	folder: PathBuf,
}

/// The key that stands for a backup among the provers of a pass: no replica's, as it is
/// derived from the backup's ID.
fn backup_prover(backup: EntityId) -> [u8; 32] {
	let mut hasher = blake3::Hasher::new_derive_key("nibkeep 2026-11 backup prover");
	hasher.update(backup.as_bytes());
	*hasher.finalize().as_bytes()
}

/// A replica found on a disk, or a backup plugged in here, as [`OnDisks`] holds it.
struct OnDisk {
	key: [u8; 32],
	sealed: Checked,
	plain: Checked,
	archive: bool,
	/// Whether it is kept away from home: only a backup can be, as no replica away is on the
	/// home network until relays.
	away: bool,
}

/// The disks that may stand for a copy of a sealed blob of a content whose policy asks for
/// `needs`, if enough can, besides `own` copies on this replica's disk: on each, the first
/// replica found, or backup plugged in here, whose account lists, checked since `since`, an
/// unchanged original of the content, where the policy keeps it in place, or else the sealed
/// blob. At least one other disk unless `own` copies are enough. Returns each disk's prover,
/// with whether it is to prove an original and whether the disk is an archive node's, in
/// the order they are chosen in: those away from home that the policy asks for first, then
/// the archive nodes' that it asks for, then originals, then archive nodes', then any; and
/// how many of them are to prove their copies, how many of those archive nodes', and how
/// many away. `None` if too few disks can: copies away from home only backups plugged in
/// here stand for, as no replica away is on the home network until relays.
fn choose_disks(
	(sealed, content): (&Hash, &Hash),
	(needs, own): (Needs, usize),
	disks: &OnDisks,
	since: u64,
) -> Option<Chosen> {
	let in_place = needs.kept == Kept::InPlace;
	let counts = |checked: Option<&u64>| checked.is_some_and(|checked| *checked >= since);
	// For each disk, the prover there, whether the disk is an archive node's, and whether it
	// is away from home.
	let mut found: Vec<(Prover, bool, bool)> = Vec::new();
	for replicas in disks.values() {
		let original = replicas
			.iter()
			.find(|replica| in_place && counts(replica.plain.get(content)))
			.map(|replica| ((replica.key, true), replica.archive, replica.away));
		if let Some(prover) = original.or_else(|| {
			replicas
				.iter()
				.find(|replica| counts(replica.sealed.get(sealed)))
				.map(|replica| ((replica.key, false), replica.archive, replica.away))
		}) {
			found.push(prover);
		}
	}
	let away_needed = needs.away as usize;
	let archived_needed = needs.archived as usize;
	let total = (needs.copies as usize)
		.max(archived_needed)
		.max(away_needed)
		.max(1)
		.saturating_sub(own);
	let away_found = found.iter().filter(|(_, _, away)| *away).count();
	let archive_found = found.iter().filter(|(_, archive, _)| *archive).count();
	if away_found < away_needed || archive_found < archived_needed || found.len() < total {
		return None;
	}
	// Those away from home that the policy asks for first, archive nodes' first among them;
	// then the archive nodes' that it asks for, originals first among them; then originals,
	// where the policy keeps the content in place, as they are its copies; then archive
	// nodes'; then any.
	found.sort_by_key(|((_, original), archive, away)| (!*away, !*archive, !*original));
	let mut chosen: Vec<(Prover, bool, bool)> = found
		.iter()
		.filter(|(_, _, away)| *away)
		.take(away_needed)
		.copied()
		.collect();
	let archived_chosen = chosen.iter().filter(|(_, archive, _)| *archive).count();
	let mut by_archive = found.clone();
	by_archive.sort_by_key(|((_, original), archive, _)| (!*archive, !*original));
	chosen.extend(
		by_archive
			.into_iter()
			.filter(|candidate| candidate.1 && !chosen.contains(candidate))
			.take(archived_needed.saturating_sub(archived_chosen))
			.collect::<Vec<_>>(),
	);
	let mut rest: Vec<(Prover, bool, bool)> = found
		.into_iter()
		.filter(|candidate| !chosen.contains(candidate))
		.collect();
	rest.sort_by_key(|((_, original), archive, _)| (!*original, !*archive));
	// All are asked, the first `total` chosen, and any that prove make up for those that do
	// not, as an original edited since its account was issued.
	chosen.extend(rest);
	Some(Chosen {
		provers: chosen
			.into_iter()
			.map(|(prover, archive, _)| (prover, archive))
			.collect(),
		needed: total,
		archived: archived_needed,
		away: away_needed,
	})
}

/// The replicas that may stand for a copy, as [`choose_disks`] chooses them.
struct Chosen {
	provers: Vec<(Prover, bool)>,
	needed: usize,
	archived: usize,
	away: usize,
}

/// A challenge to put to a replica, by its key, with the answer expected.
type Put = ([u8; 32], Challenge, [u8; 32]);

/// The challenges to put to `provers` of a sealed blob of `size` bytes, each with the answer
/// that this replica's copy, `file`, gives, or, to a replica that proves an original, the
/// answer that the content gives, as `plain` reads it opened from that copy; `None` if that
/// copy cannot be read, which is no copy to check others against.
fn challenges_for(
	(sealed, size, file): (Hash, u64, &mut File),
	plain: Option<(Hash, &mut BlobReader<File>)>,
	provers: &[(Prover, bool)],
	rng: &mut impl CryptoRng,
) -> Option<Vec<Put>> {
	let mut put = Vec::new();
	let mut plain = plain;
	for ((prover, original), _) in provers {
		let (challenge, expected) = if *original {
			let (content, reader) = plain.as_mut()?;
			let len = reader.seek(SeekFrom::End(0)).ok()?;
			let challenge = Challenge::random_plain(*content, len, rng);
			let expected = challenge
				.answer(|offset, out| {
					reader.seek(SeekFrom::Start(offset))?;
					reader.read_exact(out)
				})
				.ok()?;
			(challenge, expected)
		} else {
			let challenge = Challenge::random(sealed, size, rng);
			let expected = answer_from_file(&challenge, file).ok()?;
			(challenge, expected)
		};
		put.push((*prover, challenge, expected));
	}
	Some(put)
}

/// The answer to a challenge from the bytes of a sealed blob's file.
pub(crate) fn answer_from_file(challenge: &Challenge, file: &mut File) -> io::Result<[u8; 32]> {
	challenge.answer(|offset, out| {
		file.seek(SeekFrom::Start(offset))?;
		file.read_exact(out)
	})
}

/// On a device that waits before purging, its sync records, which keep since when it no
/// longer keeps each copy, and its waits; `None` on one that never waited.
type DeviceWaits<'a> = Option<(&'a Records, &'a [(u64, u64)])>;

impl Library {
	/// Plans a pass of removals, as of `wall_millis`: the sealed blobs that this replica
	/// holds and no longer keeps, as the planner says, at most [`REMOVALS_AT_ONCE`] of them,
	/// and for each, other disks whose copies together meet the strictest policy of the files
	/// that hold or held its content without this replica's, each with a replica on it found
	/// on the network, whose `found` endpoints say, whose latest account lists a copy checked
	/// in the last 8 weeks, or a backup plugged into this computer, which this replica reads
	/// itself and proves at once; the challenges to put to those replicas, with the answers
	/// that this replica's copies give. A copy whose policy needs a copy away from home waits
	/// for relays, as no replica away is on the home network, unless a backup kept away is
	/// plugged in; so does one that another space of the replica keeps a key for, which that
	/// space may keep. So does one that this replica
	/// holds for another, as `records` say, which is about to remove its own: asking that one
	/// to prove its copy would hold it there in turn, and the two would keep each other from
	/// removing for as long as both try.
	///
	/// # Errors
	///
	/// Returns an error if the library or the records cannot be read.
	pub fn plan_removals(
		&self,
		records: &Records,
		found: &BTreeSet<[u8; 32]>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Removals, NodeError> {
		let plan = self.plan(wall_millis)?;
		let held = records.holds(wall_millis)?;
		let mut removals = Removals::default();
		let found_replicas = self.found_replicas(found)?;
		let found_keys: BTreeSet<[u8; 32]> = found_replicas.keys().copied().collect();
		// The backups plugged in here, which this replica reads itself: looked for only if
		// anything may go.
		let anything = plan.values().any(|keeps| *keeps != Keeps::All)
			|| !self.moves_waiting_for_copies()?.is_empty();
		let local = if anything {
			self.backups_here(records)?
		} else {
			Vec::new()
		};
		removals.backups = local
			.iter()
			.map(|here| (here.prover, here.folder.clone()))
			.collect();
		removals.away = local
			.iter()
			.filter(|here| here.away)
			.map(|here| here.prover)
			.collect();
		let mut eligible = Vec::new();
		let moving = self.contents_moving()?;
		// A device that waits before purging lets nothing go on another device's word before
		// its wait has passed since it stopped keeping it.
		let waits = self.space(self.info.personal)?.stored.waits()?;
		let waiting = waits.iter().any(|(_, wait)| *wait > 0);
		let mut unkept_now = BTreeSet::new();
		for (&space, keeps) in &plan {
			let twins = self.twins(space)?;
			if *keeps != Keeps::All {
				let waits = waiting.then_some((records, waits.as_slice()));
				eligible.extend(self.eligible_in(
					(space, keeps, &twins),
					((&found_keys, &local), &held, moving.get(&space)),
					(wall_millis, waits, &mut unkept_now),
					&mut removals.waiting,
				)?);
			}
			for &(sealed, content, smaller) in &twins {
				if held.contains(&sealed) {
					removals.waiting += 1;
					continue;
				}
				eligible.push(Candidate {
					space,
					sealed,
					content,
					size: 0,
					provers: Vec::new(),
					needed: 0,
					archived: 0,
					away: 0,
					own: 0,
					twin_of: Some(smaller),
				});
			}
		}
		records.forget_unkept_but(&unkept_now)?;
		// A window of the eligible from a random place, so that copies whose proofs keep
		// failing do not take every pass's room.
		if eligible.len() > REMOVALS_AT_ONCE {
			let mut bytes = [0; 8];
			rng.fill_bytes(&mut bytes);
			let start =
				usize::try_from(u64::from_le_bytes(bytes) % eligible.len() as u64).unwrap_or(0);
			eligible.rotate_left(start);
			removals.waiting += eligible.len() - REMOVALS_AT_ONCE;
			eligible.truncate(REMOVALS_AT_ONCE);
			removals.more = true;
		}
		for candidate in eligible {
			self.put_challenges(&mut removals, candidate, rng)?;
		}
		for waiting in self.moves_waiting_for_copies()? {
			let found = (&found_keys, local.as_slice());
			self.plan_move_purge(&mut removals, waiting, found, wall_millis, rng)?;
		}
		removals.provers = removals
			.asking
			.keys()
			.filter_map(|prover| found_replicas.get(prover).cloned())
			.collect();
		Ok(removals)
	}

	/// The other replicas, neither revoked nor deleted, whose endpoints were `found` on the
	/// network, by their keys.
	fn found_replicas(
		&self,
		found: &BTreeSet<[u8; 32]>,
	) -> Result<BTreeMap<[u8; 32], Replica>, NodeError> {
		let mut replicas = BTreeMap::new();
		for replica in self.replicas()?.into_iter().chain(self.peers()?) {
			if replica.this || replica.revoked || replica.deleted {
				continue;
			}
			if let (Some(key), Some(endpoint)) = (replica.signing, replica.endpoint)
				&& found.contains(&endpoint)
			{
				replicas.entry(*key.as_bytes()).or_insert(replica);
			}
		}
		Ok(replicas)
	}

	/// The copies of a space that this replica could remove now, those it no longer keeps
	/// as `keeps` says, each with the disks chosen to stand for it, as
	/// [`Library::plan_removals`] says; `waiting` counts those that cannot go yet.
	fn eligible_in(
		&self,
		(space, keeps, twins): (SpaceId, &Keeps, &[(Hash, Hash, Hash)]),
		((found, local), held, moving): (Standing<'_>, &BTreeSet<Hash>, Option<&BTreeSet<Hash>>),
		(wall_millis, waits, unkept_now): (u64, DeviceWaits<'_>, &mut BTreeSet<Hash>),
		waiting: &mut usize,
	) -> Result<Vec<Candidate>, NodeError> {
		let mut unkept = self.unkept(space, keeps, twins)?;
		// A copy that this replica's own move waits for stays until the move's leftovers go,
		// as the replica checks the others' proofs against it.
		if let Some(moving) = moving {
			unkept.retain(|(_, content)| !moving.contains(content));
		}
		// On a device that waits, a copy goes only once its wait has passed since the device
		// first found that it no longer keeps it, as when its policy took it out of the store,
		// or the device is no longer an archive node.
		if let Some((records, waits)) = waits {
			let sealed: Vec<Hash> = unkept.iter().map(|(sealed, _)| *sealed).collect();
			unkept_now.extend(sealed.iter().copied());
			let since = records.unkept_since(&sealed, wall_millis)?;
			unkept.retain(|(sealed, _)| {
				let at = since.get(sealed).copied().unwrap_or(wall_millis);
				let due = at.saturating_add(super::waiting::wait_at(waits, at)) <= wall_millis;
				if !due {
					*waiting += 1;
				}
				due
			});
		}
		if unkept.is_empty() {
			return Ok(Vec::new());
		}
		let needs = self.needs_of_contents(space)?;
		let since = wall_millis.saturating_sub(COUNTS_FOR_MILLIS);
		// Originals stand for copies of a content only where its policy keeps it in place:
		// read only if such a content is to go.
		let in_place = unkept.iter().any(|(_, content)| {
			needs
				.get(content)
				.is_some_and(|needs| needs.kept == Kept::InPlace)
		});
		let disks = self.disks_of(space, (found, local), in_place)?;
		// A device that drops the space counts none of its originals, which stay its person's
		// files, no longer counted, once the space is gone.
		let originals = if in_place && *keeps != Keeps::Nothing {
			self.own_originals(space, since)?
		} else {
			BTreeSet::new()
		};
		let mut eligible = Vec::new();
		for (sealed, content) in unkept {
			if held.contains(&sealed) {
				*waiting += 1;
				continue;
			}
			let needs = needs_of(&needs, &content, keeps);
			let own = usize::from(needs.kept == Kept::InPlace && originals.contains(&content));
			let Some(chosen) = choose_disks((&sealed, &content), (needs, own), &disks, since)
			else {
				*waiting += 1;
				continue;
			};
			eligible.push(Candidate {
				space,
				sealed,
				content,
				size: 0,
				provers: chosen.provers,
				needed: chosen.needed,
				archived: chosen.archived,
				away: chosen.away,
				own,
				twin_of: None,
			});
		}
		Ok(eligible)
	}

	/// Adds a copy to a pass, with a challenge for each replica that is to prove its own,
	/// and the answer that this replica's copy gives; one that cannot be read here waits.
	fn put_challenges(
		&self,
		removals: &mut Removals,
		mut candidate: Candidate,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let stored = &self.space(candidate.space)?.stored;
		let Some((size, mut file)) = stored.open_sealed_blob(&candidate.sealed)? else {
			// Gone meanwhile, as a check moved it aside: it is to come again first.
			removals.waiting += 1;
			return Ok(());
		};
		candidate.size = size;
		let mut reader = if candidate.provers.iter().any(|((_, original), _)| *original) {
			let Ok(reader) = stored.open_blob(&candidate.content) else {
				removals.waiting += 1;
				return Ok(());
			};
			Some(reader)
		} else {
			None
		};
		let plain = reader.as_mut().map(|reader| (candidate.content, reader));
		let Some(put) = challenges_for(
			(candidate.sealed, size, &mut file),
			plain,
			&candidate.provers,
			rng,
		) else {
			removals.waiting += 1;
			return Ok(());
		};
		for (prover, challenge, expected) in put {
			removals.ask(prover, candidate.space, challenge, expected);
		}
		removals.candidates.push(candidate);
		Ok(())
	}

	/// Removes the copies of a pass whose proofs came while they stand, as of `wall_millis`,
	/// those that this replica still does not keep and whose policy the proven copies still
	/// meet: marked first in `records` as being removed, so that this replica proves none of
	/// them to another meanwhile, and none that it holds for another, which it proved. Its
	/// account then says that it no longer holds them. Then it purges what this replica's
	/// moves left behind, of those whose content's copies in the new space were proven while
	/// they stand, and this replica holds still; and moves on the drops of spaces that this
	/// replica makes ([`Library::drop_space`]).
	///
	/// # Errors
	///
	/// Returns an error if the library cannot be read, the records cannot be written, or a
	/// file cannot be removed; those removed before stay removed.
	pub fn remove_proven(
		&mut self,
		removals: &Removals,
		records: &Records,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Removed, NodeError> {
		let mut removed = self.remove_copies(removals, records, wall_millis, rng)?;
		for purge in &removals.moves {
			let proved = purge
				.proofs
				.iter()
				.all(|(space, sealed, content, provers)| {
					self.spaces
						.get(space)
						.is_some_and(|open| open.stored.holds_sealed(sealed))
						&& removals.all_proved(*space, (sealed, content), provers)
				});
			// The move counts still, as the syncs of the pass may have brought it set aside, or
			// this replica revoked.
			let stands = || -> Result<bool, NodeError> {
				Ok(self.check_not_revoked(purge.space).is_ok()
					&& self
						.moved_into(purge.space, purge.entity)?
						.is_some_and(|(now_in, _)| now_in == purge.now_in))
			};
			if !proved || !stands()? {
				continue;
			}
			// A purge that fails waits for the next pass, and the pass goes on.
			match self.purge_left_behind(purge.space, purge.entity, wall_millis, rng) {
				Ok(Some(deleted)) => {
					removed.moves += 1;
					removed.deleted.extend(deleted);
				}
				Ok(None) => {}
				Err(error) => removed.failed.push(error.to_string()),
			}
		}
		// The drops whose last copies went now state their choices, to sync next.
		if let Err(error) = self.settle_drops(wall_millis, rng) {
			removed.failed.push(error.to_string());
		}
		Ok(removed)
	}

	/// Removes the copies of a pass whose proofs came while they stand, as
	/// [`Library::remove_proven`] says.
	fn remove_copies(
		&mut self,
		removals: &Removals,
		records: &Records,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<Removed, NodeError> {
		let mut removed = Removed {
			waiting: removals.waiting,
			more: removals.more,
			..Removed::default()
		};
		if removals.candidates.is_empty() {
			return Ok(removed);
		}
		let kept_back = self.still_due(removals, wall_millis, &mut removed.waiting)?;
		let all: Vec<Hash> = kept_back
			.iter()
			.map(|(candidate, _)| candidate.sealed)
			.collect();
		if all.is_empty() {
			return Ok(removed);
		}
		let marked = records.begin_removing(&all, wall_millis)?;
		// Whether the proofs stand is asked last, just before the copies go, so that the
		// provers hold theirs still.
		let mut by_space: BTreeMap<SpaceId, Vec<&Candidate>> = BTreeMap::new();
		for (candidate, in_place) in kept_back {
			if marked.contains(&candidate.sealed) && removals.proven(candidate, in_place) {
				by_space.entry(candidate.space).or_default().push(candidate);
			} else {
				removed.waiting += 1;
			}
		}
		let mut failed = None;
		let mut touched = Vec::new();
		for (space, candidates) in by_space {
			let picked: Vec<Hash> = candidates
				.iter()
				.map(|candidate| candidate.sealed)
				.collect();
			let gone = match self.spaces.get(&space) {
				Some(open) => open.stored.remove_sealed_blobs(&picked),
				None => Err(nibkeep_store::StoreError::Io(io::Error::from(
					io::ErrorKind::NotFound,
				))),
			};
			let gone: BTreeSet<Hash> = match gone {
				Ok(gone) => gone.into_iter().collect(),
				Err(error) => {
					failed.get_or_insert(error);
					BTreeSet::new()
				}
			};
			for candidate in candidates {
				if gone.contains(&candidate.sealed) {
					removed.copies += 1;
					removed.bytes = removed.bytes.saturating_add(candidate.size);
				} else {
					removed.waiting += 1;
				}
			}
			if !gone.is_empty() {
				touched.push(space);
			}
		}
		// The marks go whatever happened, so that no blob stays unproved for others.
		let ended = records.end_removing(&all);
		for space in touched {
			// Its account says so now, rather than at a sync a minute later.
			self.accounts_refreshed.borrow_mut().remove(&space);
			let _ = self.refresh_account(space, wall_millis, rng);
		}
		ended?;
		match failed {
			Some(error) => Err(error.into()),
			None => Ok(removed),
		}
	}

	/// The contents of this device's originals of a space, kept where they were found, that
	/// stand for copies where a policy keeps them in place: those of origins that recorded the
	/// content they found, unchanged by their size and modification time, and found holding
	/// it by a check since `since`; never a checkout, which NibKeep deletes and the person
	/// edits.
	fn own_originals(&self, space: SpaceId, since: u64) -> Result<BTreeSet<Hash>, NodeError> {
		let checks: BTreeMap<(String, Hash), (u64, bool)> = self
			.space(space)?
			.stored
			.place_checks()?
			.into_iter()
			.map(|check| ((check.path, check.content), (check.checked, check.held)))
			.collect();
		Ok(self
			.origin_places_here(space)?
			.into_iter()
			.filter(|place| place.untouched)
			.filter(|place| {
				checks
					.get(&(place.path.clone(), place.content))
					.is_some_and(|(checked, held)| *held && *checked >= since)
			})
			.map(|place| place.content)
			.collect())
	}

	/// The contents that this replica's own moves wait to purge what they left behind of, by
	/// the spaces they moved into, whose copies there it keeps meanwhile.
	fn contents_moving(&self) -> Result<BTreeMap<SpaceId, BTreeSet<Hash>>, NodeError> {
		let mut moving: BTreeMap<SpaceId, BTreeSet<Hash>> = BTreeMap::new();
		for waiting in self.moves_waiting_for_copies()? {
			moving
				.entry(waiting.now_in)
				.or_default()
				.extend(waiting.contents.iter().map(|(content, _)| *content));
		}
		Ok(moving)
	}

	/// The replicas found on each disk of a space other than this replica's, whose `found`
	/// keys say, with what their latest accounts list there.
	fn disks_of(
		&self,
		space: SpaceId,
		(found, local): (&BTreeSet<[u8; 32]>, &[BackupHere]),
		originals: bool,
	) -> Result<OnDisks, NodeError> {
		let archive = self.archive_disks(space)?;
		let own_disk = self.own_disk(space);
		let mut disks = OnDisks::new();
		for (key, listed) in self.listed_by(space, found, originals)? {
			if listed.disk != own_disk {
				disks.entry(listed.disk).or_default().push(OnDisk {
					key,
					sealed: listed.sealed,
					plain: listed.plain,
					archive: archive.get(&listed.disk).copied().unwrap_or(false),
					away: false,
				});
			}
		}
		// The backups plugged in here that hold the space, each keeping what an archive node
		// keeps.
		let own = self.replica.signing.verifying_key();
		for here in local {
			if !here.spaces.contains(&space) {
				continue;
			}
			let disk = disk_id(space, Some(&here.disk), &own);
			if disk != own_disk {
				disks.entry(disk).or_default().push(OnDisk {
					key: here.prover,
					sealed: here.sealed.clone(),
					plain: Checked::new(),
					archive: true,
					away: here.away,
				});
			}
		}
		// A disk is an archive node's, or away from home, only if all on it are, as counting
		// says.
		for on_disk in disks.values_mut() {
			let archive = on_disk.iter().all(|each| each.archive);
			let away = on_disk.iter().all(|each| each.away);
			for each in on_disk.iter_mut() {
				each.archive = archive;
				each.away = away;
			}
		}
		Ok(disks)
	}

	/// The backups plugged into this computer, found where `records` say they were last found
	/// or at their folders' paths on the drives mounted, each with what its folder says it
	/// holds in files of its own, with when each was last checked.
	fn backups_here(&self, records: &Records) -> Result<Vec<BackupHere>, NodeError> {
		let remembered = records
			.backup_folders()?
			.into_iter()
			.map(|(backup, found)| (backup, found.folder))
			.collect();
		let mut here = Vec::new();
		for (backup, folder) in self.find_backups(&remembered)? {
			// One being updated, or cut short while it was, stands for nothing until an update
			// has read it back; one that cannot be read now stands for nothing either, and the
			// pass goes on.
			let in_folder = crate::backup::InFileSystem::new(&folder);
			if crate::backup::marked_in_use(&in_folder) {
				continue;
			}
			let Ok(files) = crate::backup::blob_files_in(&in_folder) else {
				continue;
			};
			let record = crate::backup::read_record(&in_folder);
			let sealed: Checked = files
				.into_keys()
				.map(|sealed| (sealed, record.checks.get(&sealed).copied().unwrap_or(0)))
				.collect();
			here.push(BackupHere {
				prover: backup_prover(backup.id),
				disk: crate::backup::disk_name(&in_folder, backup.id),
				spaces: self.backup_spaces(&backup)?,
				away: backup.away,
				sealed,
				folder,
			});
		}
		Ok(here)
	}

	/// Adds to a pass a move of this replica's whose leftovers hold content: for each
	/// content, the sealed copy in the new space that this replica holds, and replicas on
	/// other disks found on the network whose accounts there list it, as many as make, with
	/// this replica's disk, the copies that the new entity's policy asks for, archive nodes'
	/// among them only once the new space has an archive node, and none away from home until
	/// relays; with challenges for them. A move with a content that this replica holds no
	/// sealed copy of in the new space, or that too few disks hold, waits.
	fn plan_move_purge(
		&self,
		removals: &mut Removals,
		waiting: WaitingMove,
		(found, local): (&BTreeSet<[u8; 32]>, &[BackupHere]),
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let new_space = waiting.now_in;
		let needs = self.needs_of_contents(new_space)?;
		let archive_counts = self.has_archive_node(new_space)?;
		let own_archive = self.device_marks(self.info.device)?.role == DeviceRole::Archive;
		let disks = self.disks_of(new_space, (found, local), false)?;
		let since = wall_millis.saturating_sub(COUNTS_FOR_MILLIS);
		let stored = &self.space(new_space)?.stored;
		let log = stored.log();
		let mut proofs = Vec::new();
		let mut put = Vec::new();
		for (content, secret) in waiting.contents {
			let Some(sealed) = log
				.blob_keys(&content)
				.map(|(copy, _)| *copy)
				.filter(|copy| log.is_secret_blob(copy) == secret && stored.holds_sealed(copy))
				.min()
			else {
				return Ok(());
			};
			let policy = needs.get(&content).copied().unwrap_or(Needs::FALLBACK);
			// This replica's copy is one of them; at least one other, so that another device
			// holds the new entity, as the purge always waited for.
			let others = Needs {
				copies: policy.copies.saturating_sub(1).max(1),
				away: 0,
				archived: if archive_counts {
					policy.archived.saturating_sub(u32::from(own_archive))
				} else {
					0
				},
				kept: policy.kept,
			};
			// Sealed copies alone, as an original kept in place holds no copy of the new space.
			let sealed_only = Needs {
				kept: Kept::Store,
				..others
			};
			let Some(chosen) = choose_disks((&sealed, &content), (sealed_only, 0), &disks, since)
			else {
				return Ok(());
			};
			let Some((size, mut file)) = stored.open_sealed_blob(&sealed)? else {
				return Ok(());
			};
			// Those chosen alone, as each must prove: the others make up for none.
			let provers: Vec<(Prover, bool)> =
				chosen.provers.into_iter().take(chosen.needed).collect();
			let Some(challenges) = challenges_for((sealed, size, &mut file), None, &provers, rng)
			else {
				return Ok(());
			};
			put.extend(challenges);
			proofs.push((
				new_space,
				sealed,
				content,
				provers.into_iter().map(|(prover, _)| prover).collect(),
			));
		}
		for (prover, challenge, expected) in put {
			removals.ask(prover, new_space, challenge, expected);
		}
		removals.moves.push(MovePurge {
			space: waiting.space,
			entity: waiting.entity,
			now_in: new_space,
			proofs,
		});
		Ok(())
	}

	/// The copies of a pass that are still to go, as of `wall_millis`: twins whose smaller
	/// copy is here still, and copies that this replica still does not keep and whose policy
	/// the disks chosen, with this replica's original where it counts, still meet, each with
	/// whether the policy keeps it in place, so that originals proved count; `waiting` counts
	/// the others but those kept now.
	fn still_due<'r>(
		&self,
		removals: &'r Removals,
		wall_millis: u64,
		waiting: &mut usize,
	) -> Result<Vec<(&'r Candidate, bool)>, NodeError> {
		let plan = self.plan(wall_millis)?;
		let since = wall_millis.saturating_sub(COUNTS_FOR_MILLIS);
		let mut needs: BTreeMap<SpaceId, BTreeMap<Hash, Needs>> = BTreeMap::new();
		let mut originals: BTreeMap<SpaceId, BTreeSet<Hash>> = BTreeMap::new();
		let mut kept_back = Vec::new();
		for candidate in &removals.candidates {
			// A twin goes while its smaller copy is here, whatever the replica keeps.
			if let Some(smaller) = candidate.twin_of {
				let open = self.space(candidate.space)?;
				if open.stored.holds_sealed(&smaller)
					&& !open.stored.log().wrapped_blob_keys_of(&smaller).is_empty()
				{
					kept_back.push((candidate, false));
				} else {
					*waiting += 1;
				}
				continue;
			}
			// One that the replica keeps now, as a sync brought a pin, stays and waits for
			// nothing.
			if plan
				.get(&candidate.space)
				.is_none_or(|keeps| keeps.keeps(&candidate.content))
			{
				continue;
			}
			if let std::collections::btree_map::Entry::Vacant(entry) = needs.entry(candidate.space)
			{
				entry.insert(self.needs_of_contents(candidate.space)?);
			}
			let keeps = plan.get(&candidate.space).unwrap_or(&Keeps::All);
			let now_needs = needs
				.get(&candidate.space)
				.map_or(Needs::FALLBACK, |needs| {
					needs_of(needs, &candidate.content, keeps)
				});
			let in_place = now_needs.kept == Kept::InPlace;
			// An original here counts while the policy keeps the content in place, and it is
			// unchanged and checked still.
			let own = if in_place && candidate.own > 0 {
				if let std::collections::btree_map::Entry::Vacant(entry) =
					originals.entry(candidate.space)
				{
					entry.insert(self.own_originals(candidate.space, since)?);
				}
				usize::from(
					originals
						.get(&candidate.space)
						.is_some_and(|here| here.contains(&candidate.content)),
				)
			} else {
				0
			};
			let met = now_needs.away as usize <= candidate.away
				&& candidate.needed + own >= now_needs.copies.max(1) as usize;
			if met {
				kept_back.push((candidate, in_place));
			} else {
				*waiting += 1;
			}
		}
		Ok(kept_back)
	}

	/// The sealed blobs of a space that this replica holds and does not keep, with their
	/// contents: ordinary ones, whose contents it can tell, or any where it keeps nothing of
	/// the space, and that no other space of the replica keeps a key for.
	fn unkept(
		&self,
		space: SpaceId,
		keeps: &Keeps,
		twins: &[(Hash, Hash, Hash)],
	) -> Result<Vec<(Hash, Hash)>, NodeError> {
		let stored = &self.space(space)?.stored;
		let log = stored.log();
		// A space that this device drops keeps no blob, a secret one neither: one whose
		// content cannot be read here, as while the secrets are locked, stands for its content
		// by its sealed hash, and needs one copy elsewhere, as every replica keeps it.
		let nothing = *keeps == Keeps::Nothing;
		let unkept: Vec<(Hash, Hash)> = stored
			.sealed_blobs()?
			.into_iter()
			.filter(|sealed| nothing || !log.is_secret_blob(sealed))
			.filter_map(|sealed| match log.content_of_sealed(&sealed) {
				Some(content) => (!keeps.keeps(&content)).then_some((sealed, content)),
				None => nothing.then_some((sealed, sealed)),
			})
			.collect();
		if unkept.is_empty() {
			return Ok(unkept);
		}
		let sealed: Vec<Hash> = unkept.iter().map(|(sealed, _)| *sealed).collect();
		let elsewhere = stored.keyed_elsewhere(&sealed);
		let twins: BTreeSet<Hash> = twins.iter().map(|(sealed, _, _)| *sealed).collect();
		Ok(unkept
			.into_iter()
			.filter(|(sealed, _)| !elsewhere.contains(sealed) && !twins.contains(sealed))
			.collect())
	}

	/// The sealed blobs of a space that this replica holds beside another ordinary sealed
	/// copy of the same content, of the space too, with a smaller sealed hash: each with its
	/// content and the smallest such copy. One that another space of the replica keeps a key
	/// for is left out, as that space may lack the smaller copy's key.
	fn twins(&self, space: SpaceId) -> Result<Vec<(Hash, Hash, Hash)>, NodeError> {
		let stored = &self.space(space)?.stored;
		let log = stored.log();
		let held: BTreeSet<Hash> = stored
			.sealed_blobs()?
			.into_iter()
			.filter(|sealed| !log.is_secret_blob(sealed))
			.collect();
		let mut twins = Vec::new();
		for sealed in &held {
			let Some(content) = log.content_of_sealed(sealed) else {
				continue;
			};
			if let Some(smaller) = log
				.blob_keys(&content)
				.map(|(copy, _)| *copy)
				.filter(|copy| copy < sealed && held.contains(copy))
				.min()
			{
				twins.push((*sealed, content, smaller));
			}
		}
		if twins.is_empty() {
			return Ok(twins);
		}
		let sealed: Vec<Hash> = twins.iter().map(|(sealed, _, _)| *sealed).collect();
		let elsewhere = stored.keyed_elsewhere(&sealed);
		Ok(twins
			.into_iter()
			.filter(|(sealed, _, _)| !elsewhere.contains(sealed))
			.collect())
	}

	/// How many of the sealed blobs of a space that this replica holds, and that no other
	/// space of it keeps a key for, other disks' accounts, as `copies` gives them, show
	/// fewer copies of than they need where this replica keeps nothing of the space, as
	/// [`Library::drop_space`] counts them: those a drop could never remove. A content that a
	/// policy keeps in place is left to the check of files, as originals stand for it.
	pub(crate) fn blobs_short_elsewhere(
		&self,
		space: SpaceId,
		copies: &super::accounts::Copies,
	) -> Result<usize, NodeError> {
		let unkept = self.unkept(space, &Keeps::Nothing, &[])?;
		if unkept.is_empty() {
			return Ok(0);
		}
		let needs = self.needs_of_contents(space)?;
		let mut short = 0;
		for (sealed, content) in unkept {
			let needs = needs_of(&needs, &content, &Keeps::Nothing);
			if needs.kept == Kept::InPlace {
				continue;
			}
			let (mut disks, _) = copies.holding(&[sealed], (&[], None));
			disks.remove(&super::accounts::Copies::OWN);
			let (away, archived) = copies.marked(&disks);
			short += usize::from(needs.lacks(disks.len(), away, archived).any());
		}
		Ok(short)
	}

	/// What the copies of each content that a file of a space holds or held need: the
	/// strictest of the policies chosen for those files.
	fn needs_of_contents(&self, space: SpaceId) -> Result<BTreeMap<Hash, Needs>, NodeError> {
		let policies = self.read_policies(space)?;
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let mut needs: BTreeMap<Hash, Needs> = BTreeMap::new();
		let mut seen = BTreeSet::new();
		for file in open
			.stored
			.find(&fields::TYPE, &Value::Text(FILE.to_string()))?
		{
			let file = state.canonical(file)?;
			if !seen.insert(file) {
				continue;
			}
			let chosen = policies
				.choose(&own_choices(state, file)?, &facts_of(state, file)?)
				.needs;
			for member in state.merged(file)? {
				for id in storage.of_field(member, &CONTENT)? {
					if let Some(recorded) = storage.get(id)?
						&& let Value::Blob(content) = recorded.statement.value
					{
						let entry = needs.entry(content).or_insert(chosen);
						*entry = entry.strictest(chosen);
					}
				}
			}
		}
		Ok(needs)
	}
}
