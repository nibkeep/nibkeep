//! Checking and repair: the holder of the sync role reads every copy that the replica holds
//! once in a round of 4 weeks, a share at a time; a sealed blob found damaged is moved aside,
//! so that a sync brings it again, and an original found damaged is reported, never written
//! over ([technical design: checking and repair]).
//!
//! [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_store::{PlaceCheck, SealedCheck, blobs_starting_with, hash_file};

use super::Library;
use crate::{NodeError, Records};

/// A day, in milliseconds.
const DAY: u64 = 86_400_000;

/// How long a round of checks takes, in which every copy is read once: 4 weeks.
pub const ROUND_MILLIS: u64 = 28 * DAY;

/// How long a copy counts after its last check: 8 weeks, two rounds.
pub const COUNTS_FOR_MILLIS: u64 = 56 * DAY;

/// How long a check rests at most before it asks again whether to stop.
const REST_STEP: Duration = Duration::from_millis(100);

/// How the holder of the sync role paces its checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
	/// How long it waits after it takes the role, and after a run that caught up with the
	/// round, before it looks again whether checks are due.
	pub wait: Duration,
	/// How many times as long as reading a file took a check rests after it, so that checks
	/// take at most a share of the disk's time: 4, a fifth.
	pub rest: u32,
	/// How long one run goes on at most, rests included, before it saves where it stands and
	/// ends, once the file it reads is read; another starts once the rest it owes is over, if
	/// more is due.
	pub run_for: Duration,
}

impl Pace {
	/// The pace the apps and `nib sync --serve` keep: they look every 5 minutes, rest four
	/// times as long as they read, and run for a minute at a time.
	pub const DEFAULT: Self = Self {
		wait: Duration::from_mins(5),
		rest: 4,
		run_for: Duration::from_secs(60),
	};
}

impl Default for Pace {
	fn default() -> Self {
		Self::DEFAULT
	}
}

/// Where a round of checks stands, as the sync records keep it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Round {
	/// When it began, in milliseconds since 1970.
	pub began: u64,
	/// The sealed blobs, in the order of their sealed hashes.
	pub blobs: Part,
	/// The plain copies on this device, in the order of their files' IDs.
	pub plain: Part,
}

/// Where one kind of copies stands in a round.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Part {
	/// How many bytes they took when the round began.
	pub total: u64,
	/// How many bytes of them were read since.
	pub read: u64,
	/// The last one read: a sealed hash as a multihash, or a file's ID; none before the
	/// first.
	pub after: Option<Vec<u8>>,
	/// Whether every one was read.
	pub done: bool,
}

impl Part {
	/// Whether more of them are due, `elapsed` milliseconds into the round: until as many
	/// bytes are read as that share of the round's, or every one is read, once the round's
	/// time is up.
	fn due(&self, elapsed: u64) -> bool {
		if self.done {
			return false;
		}
		if elapsed >= ROUND_MILLIS {
			return true;
		}
		let share = u128::from(self.total) * u128::from(elapsed) / u128::from(ROUND_MILLIS);
		u128::from(self.read) < share
	}
}

/// What one run of the regular check did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckRun {
	/// How many sealed blobs it read in full.
	pub blobs: usize,
	/// How many plain copies it read in full.
	pub plain: usize,
	/// How many copies it found damaged: sealed blobs, which it moved aside so that a sync
	/// brings them again, and originals, which it reports.
	pub damaged: usize,
	/// Whether more is due already, as the run ended at its time.
	pub more: bool,
	/// How much of its rest after the last file it read the run still owes, as its time
	/// ended first: the next run waits that long.
	pub rest: Duration,
}

/// What checks found damaged in a space, for the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DamagedCopy {
	/// The space.
	pub space: SpaceId,
	/// The files whose content it holds, if this replica knows them.
	pub files: Vec<EntityId>,
	/// When a check last found it damaged, in milliseconds since 1970.
	pub found: u64,
	/// What it is, and where it stands.
	pub copy: Damaged,
}

/// A copy that checks found damaged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damaged {
	/// A sealed blob, moved aside.
	Sealed {
		/// Its sealed hash.
		sealed: Hash,
		/// Whether an intact copy is in place again: one that a sync brought, or the copy
		/// aside, which a later check found intact after all.
		replaced: bool,
	},
	/// A plain copy on this device that holds other content than it should, though its size
	/// and modification time are those recorded: damaged, or edited within the second its
	/// time records. NibKeep never writes over it.
	Plain {
		/// Its full path.
		path: String,
	},
}

/// Where this replica's checks stand, for the person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckStatus {
	/// Whether the replica holds any copy to check: a sealed blob, or a plain copy on this
	/// device.
	pub holds: bool,
	/// The round under way, if one began.
	pub round: Option<Round>,
	/// When a check last ran, in milliseconds since 1970, if one did.
	pub last: Option<u64>,
	/// What checks found damaged in the last round, and what is damaged still.
	pub damaged: Vec<DamagedCopy>,
}

/// A plain copy of a file on this device: an original kept where it was found, or the copy of
/// a checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlainPlace {
	/// The file, as the origin or checkout names it.
	pub(crate) file: EntityId,
	/// The copy's full path.
	pub(crate) path: String,
	/// The content it should hold.
	pub(crate) content: Hash,
	/// Whether it is there with the size and modification time recorded, as an unchanged
	/// copy is.
	pub(crate) untouched: bool,
	/// Its size in bytes, as recorded.
	pub(crate) size: u64,
	/// The size and modification time found when it was listed, if it was untouched: a check
	/// reads it only while they stay, so that an edit is not taken for damage.
	pub(crate) seen: Option<(u64, SystemTime)>,
}

/// The size and modification time of the file at `path`, if it is one.
pub(crate) fn seen_at(path: &str) -> Option<(u64, SystemTime)> {
	let metadata = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
	Some((metadata.len(), metadata.modified().ok()?))
}

/// This device's plain copies of a space's files, with their space's latest checks of them.
pub(crate) struct PlainState {
	/// The copies.
	pub(crate) places: Vec<PlainPlace>,
	/// The latest check of each copy, by its path and the content it should hold: when, and
	/// whether it held the content.
	checks: BTreeMap<(String, Hash), (u64, bool)>,
}

impl PlainState {
	/// The unchanged plain copies that the last check did not find damaged, by file and
	/// content, with when a check last found one of them holding the content, or 0 if none
	/// did.
	pub(crate) fn copies(&self) -> BTreeMap<(EntityId, Hash), u64> {
		let mut copies: BTreeMap<(EntityId, Hash), u64> = BTreeMap::new();
		for place in self.places.iter().filter(|place| place.untouched) {
			let checked = match self.checks.get(&(place.path.clone(), place.content)) {
				Some((_, false)) => continue,
				Some((checked, true)) => *checked,
				None => 0,
			};
			let latest = copies.entry((place.file, place.content)).or_insert(0);
			*latest = (*latest).max(checked);
		}
		copies
	}

	/// The unchanged plain copies that the last check found damaged, once each, with when.
	fn damaged(&self) -> Vec<(&PlainPlace, u64)> {
		let mut seen = BTreeSet::new();
		self.places
			.iter()
			.filter(|place| place.untouched)
			.filter_map(
				|place| match self.checks.get(&(place.path.clone(), place.content)) {
					Some((checked, false)) if seen.insert((place.path.clone(), place.content)) => {
						Some((place, *checked))
					}
					_ => None,
				},
			)
			.collect()
	}
}

/// The sealed blobs of a replica's blob directory in the order of their sealed hashes, read
/// a directory at a time; a directory that cannot be read is passed over, and read again in
/// the next round.
struct SealedWalk<'a> {
	replica: &'a Path,
	/// A hash of the algorithm that names the blobs, for its directory.
	like: Hash,
	/// The blobs read from the current directory and not yet taken.
	pending: VecDeque<(Hash, u64)>,
	/// The first byte of the next directory to read, if any is left.
	next: Option<u8>,
	/// The blob after which to start.
	after: Option<Hash>,
}

impl<'a> SealedWalk<'a> {
	fn new(replica: &'a Path, after: Option<Hash>) -> Self {
		Self {
			replica,
			like: Hash::of(b""),
			pending: VecDeque::new(),
			next: Some(after.map_or(0, |after| after.digest()[0])),
			after,
		}
	}

	/// The next sealed blob, with its size in bytes, if any is left.
	fn next(&mut self) -> Option<(Hash, u64)> {
		loop {
			if let Some(found) = self.pending.pop_front() {
				return Some(found);
			}
			let byte = self.next?;
			self.next = byte.checked_add(1);
			let after = self.after;
			self.pending = blobs_starting_with(self.replica, &self.like, byte)
				.unwrap_or_default()
				.into_iter()
				.filter(|(sealed, _)| after.is_none_or(|after| *sealed > after))
				.collect();
		}
	}
}

/// What a run found intact, to save as it ends, for each space.
#[derive(Default)]
struct Found {
	intact: Vec<(Hash, u64)>,
	places: Vec<PlaceCheck>,
}

/// Rests until `until`, or until `deadline` or `stop` comes first. Returns how much of the
/// rest is left: none, unless `deadline` came first.
fn rest_until(until: Instant, deadline: Instant, stop: &dyn Fn() -> bool) -> Duration {
	let end = until.min(deadline);
	while !stop() {
		let left = end.saturating_duration_since(Instant::now());
		if left.is_zero() {
			break;
		}
		std::thread::sleep(left.min(REST_STEP));
	}
	if stop() {
		return Duration::ZERO;
	}
	until.saturating_duration_since(Instant::now())
}

/// Checks the plain copies of one file at `now`, those whose size and modification time
/// are those recorded and stay so while they are read, until `stop` says to stop: notes each
/// check in `checks` and counts it in `run`. A copy whose hash `known` holds already is not
/// read again. Returns how many bytes it checked. One that cannot be read now, or changed as
/// it was read, is read again in the next round.
fn check_plain(
	places: &[PlainPlace],
	(now, stop, known): (u64, &dyn Fn() -> bool, &BTreeMap<String, Hash>),
	checks: &mut Vec<PlaceCheck>,
	run: &mut CheckRun,
) -> u64 {
	let mut size = 0_u64;
	for place in places.iter().filter(|place| place.untouched) {
		let Some(seen) = place.seen else {
			continue;
		};
		if seen_at(&place.path) != Some(seen) {
			continue;
		}
		let hash = match known.get(&place.path) {
			Some(hash) => *hash,
			None => match hash_file(Path::new(&place.path), stop) {
				Ok(Some((hash, _))) => hash,
				Ok(None) => break,
				Err(_) => continue,
			},
		};
		if seen_at(&place.path) != Some(seen) {
			continue;
		}
		let held = hash == place.content;
		run.plain += 1;
		run.damaged += usize::from(!held);
		size = size.saturating_add(place.size);
		checks.push(PlaceCheck {
			path: place.path.clone(),
			content: place.content,
			checked: now,
			held,
		});
	}
	size
}

/// How a run goes: the wall clock, its pace, when to stop, and when its time ends.
struct Running<'a> {
	wall: &'a dyn Fn() -> u64,
	pace: Pace,
	stop: &'a dyn Fn() -> bool,
	deadline: Instant,
}

impl Library {
	/// Runs the regular check of the replica's copies, as the holder of the sync role does
	/// every few minutes: reads what the round under way says is due, sealed blobs in the
	/// order of their sealed hashes against their sealed hashes, and this device's unchanged
	/// plain copies in the order of their files' IDs against their content, each from where
	/// the last run stopped, resting after each file as `pace` says, for `pace.run_for` at
	/// most, or until `stop` says to stop. A sealed blob found damaged is moved aside, so
	/// that the replica no longer holds it, and recorded at once; the run then asks the
	/// holder to sync with every replica it finds, which brings it again. A plain copy found
	/// damaged is recorded, and left as it is. As a round begins, the copies kept aside are
	/// read again, and one found intact after all goes back in place. Where the round
	/// stands, and what the run found, are saved as it ends, after a failure too
	/// ([technical design: checking and repair]).
	///
	/// [technical design: checking and repair]: ../../../../docs/Technical-design.md#97-checking-and-repair
	///
	/// # Errors
	///
	/// Returns an error if the records or a space cannot be read, or what the run found
	/// cannot be saved. A copy that cannot be read is no error: it is read again in the next
	/// round.
	pub fn check_copies(
		&mut self,
		records: &Records,
		wall: &dyn Fn() -> u64,
		pace: Pace,
		stop: &dyn Fn() -> bool,
	) -> Result<CheckRun, NodeError> {
		let running = Running {
			wall,
			pace,
			stop,
			deadline: Instant::now() + pace.run_for,
		};
		let mut round = match records.round()? {
			Some(round) => round,
			None => self.new_round(wall())?,
		};
		self.note_aside(wall());
		let mut found: BTreeMap<SpaceId, Found> = BTreeMap::new();
		let mut run = CheckRun::default();
		let checked = self.check_due(&mut round, &running, &mut found, &mut run);
		let mut saved = Ok(());
		for (space, found) in found {
			if let Some(open) = self.spaces.get_mut(&space) {
				let recorded = open
					.stored
					.record_checks(found.intact, Vec::new(), found.places);
				saved = saved.and(recorded.map_err(NodeError::from));
			}
		}
		let now = wall();
		records.save_round(&round, now)?;
		if run.damaged > 0 {
			records.ask(None, now)?;
		}
		checked.and(saved)?;
		Ok(run)
	}

	/// Reads what the round says is due, as [`Library::check_copies`] says, noting what it
	/// finds intact in `found` and counting it in `run`.
	fn check_due(
		&mut self,
		round: &mut Round,
		running: &Running<'_>,
		found: &mut BTreeMap<SpaceId, Found>,
		run: &mut CheckRun,
	) -> Result<(), NodeError> {
		let directory = self.directory().to_path_buf();
		let mut sealed_walk = SealedWalk::new(
			&directory,
			round
				.blobs
				.after
				.as_deref()
				.and_then(|after| Hash::from_multihash(after).ok()),
		);
		let mut plain: Option<VecDeque<(EntityId, SpaceId, Vec<PlainPlace>)>> = None;
		let stop = running.stop;
		loop {
			if stop() {
				return Ok(());
			}
			if !run.rest.is_zero() || Instant::now() >= running.deadline {
				run.more = true;
				return Ok(());
			}
			let now = (running.wall)();
			let elapsed = now.saturating_sub(round.began);
			let began = Instant::now();
			if round.blobs.due(elapsed) {
				let Some((sealed, size)) = sealed_walk.next() else {
					round.blobs.done = true;
					continue;
				};
				match self.check_one_blob(&sealed, now, stop, found)? {
					Some(damaged) => {
						run.blobs += 1;
						run.damaged += usize::from(damaged);
					}
					None if stop() => return Ok(()),
					None => {}
				}
				round.blobs.after = Some(sealed.to_multihash());
				round.blobs.read = round.blobs.read.saturating_add(size);
			} else if round.plain.due(elapsed) {
				if plain.is_none() {
					let after = round
						.plain
						.after
						.as_deref()
						.and_then(|after| <[u8; 16]>::try_from(after).ok())
						.map(EntityId::from_bytes);
					plain = Some(
						self.plain_by_file()?
							.into_iter()
							.filter(|(file, _, _)| after.is_none_or(|after| *file > after))
							.collect(),
					);
				}
				let Some((file, space, places)) = plain.as_mut().and_then(VecDeque::pop_front)
				else {
					round.plain.done = true;
					continue;
				};
				let checks = &mut found.entry(space).or_default().places;
				let size = check_plain(&places, (now, stop, &BTreeMap::new()), checks, run);
				if stop() {
					return Ok(());
				}
				round.plain.after = Some(file.as_bytes().to_vec());
				round.plain.read = round.plain.read.saturating_add(size);
			} else {
				if round.blobs.done && round.plain.done {
					self.recheck_aside(now, stop, found);
					*round = self.new_round(now)?;
				}
				return Ok(());
			}
			let owed = began.elapsed().saturating_mul(running.pace.rest);
			run.rest = rest_until(Instant::now() + owed, running.deadline, stop);
		}
	}

	/// Checks one sealed blob of the replica's blob directory against its sealed hash, for
	/// each space that keeps a key for it. One found intact is noted in `found`; one found
	/// damaged is moved aside, and recorded at once in each of those spaces, so that nothing
	/// that fails later in the run loses the record. Returns whether it was damaged, or
	/// `None` if it was not read or moved: no space keeps a key for it, it went meanwhile, it
	/// could not be read or moved, or `stop` said to stop.
	fn check_one_blob(
		&mut self,
		sealed: &Hash,
		now: u64,
		stop: &dyn Fn() -> bool,
		found: &mut BTreeMap<SpaceId, Found>,
	) -> Result<Option<bool>, NodeError> {
		let keyed = self.keeping(sealed);
		let Some(first) = keyed.first().copied() else {
			return Ok(None);
		};
		let stored = &self.space(first)?.stored;
		match stored.check_sealed_blob(sealed, stop) {
			Ok(SealedCheck::Intact(_)) => {
				for space in keyed {
					found.entry(space).or_default().intact.push((*sealed, now));
				}
				Ok(Some(false))
			}
			Ok(SealedCheck::Damaged) => {
				if !stored.set_aside_blob(sealed).unwrap_or(false) {
					return Ok(None);
				}
				// One not recorded now is recorded at the next run (Library::note_aside).
				for space in keyed {
					if let Some(open) = self.spaces.get_mut(&space) {
						let _ =
							open.stored
								.record_checks(Vec::new(), vec![(*sealed, now)], Vec::new());
					}
				}
				Ok(Some(true))
			}
			Ok(SealedCheck::Gone | SealedCheck::Stopped) | Err(_) => Ok(None),
		}
	}

	/// The spaces of the library that keep a key for a sealed blob.
	fn keeping(&self, sealed: &Hash) -> Vec<SpaceId> {
		self.spaces
			.iter()
			.filter(|(_, open)| !open.stored.log().wrapped_blob_keys_of(sealed).is_empty())
			.map(|(id, _)| *id)
			.collect()
	}

	/// Records, at `now`, the copies kept aside that no space recorded as damaged, in each
	/// space that keeps a key for them, as when a crash or a failure came between moving one
	/// and recording it. One that cannot be recorded now is recorded next time.
	pub(crate) fn note_aside(&mut self, now: u64) {
		for open in self.spaces.values_mut() {
			let Ok(recorded) = open.stored.damaged_blobs() else {
				continue;
			};
			let missing: Vec<(Hash, u64)> = open
				.stored
				.aside_blobs()
				.into_iter()
				.filter(|sealed| !recorded.contains_key(sealed))
				.map(|sealed| (sealed, now))
				.collect();
			let _ = open.stored.record_checks(Vec::new(), missing, Vec::new());
		}
	}

	/// Reads the copies kept aside again, and puts each that is intact after all back in
	/// place, as when the damage was a bad read, noting it in `found` as checked at `now`.
	fn recheck_aside(
		&self,
		now: u64,
		stop: &dyn Fn() -> bool,
		found: &mut BTreeMap<SpaceId, Found>,
	) {
		let mut aside = BTreeSet::new();
		for open in self.spaces.values() {
			aside.extend(open.stored.aside_blobs());
		}
		for sealed in aside {
			let keyed = self.keeping(&sealed);
			let Some(open) = keyed.first().and_then(|first| self.spaces.get(first)) else {
				continue;
			};
			if let Ok(SealedCheck::Intact(_)) = open.stored.recheck_aside_blob(&sealed, stop) {
				for space in keyed {
					found.entry(space).or_default().intact.push((sealed, now));
				}
			}
		}
	}

	/// A new round of checks, beginning at `now`, with how many bytes each kind of copies
	/// takes now.
	fn new_round(&self, now: u64) -> Result<Round, NodeError> {
		let mut blobs = 0_u64;
		let mut walk = SealedWalk::new(self.directory(), None);
		while let Some((_, size)) = walk.next() {
			blobs = blobs.saturating_add(size);
		}
		let plain = self
			.plain_by_file()?
			.iter()
			.flat_map(|(_, _, places)| places.iter().filter(|place| place.untouched))
			.fold(0_u64, |total, place| total.saturating_add(place.size));
		Ok(Round {
			began: now,
			blobs: Part {
				total: blobs,
				..Part::default()
			},
			plain: Part {
				total: plain,
				..Part::default()
			},
		})
	}

	/// Records that the plain copy at `path` held `content` at `wall_millis`, as it was just
	/// read whole, where it was found or written: its first check. One that cannot be
	/// recorded waits for the regular check.
	pub(crate) fn record_first_check(
		&mut self,
		space: SpaceId,
		(path, content): (String, Hash),
		wall_millis: u64,
	) {
		if let Some(open) = self.spaces.get_mut(&space) {
			let _ = open.stored.record_checks(
				Vec::new(),
				Vec::new(),
				vec![PlaceCheck {
					path,
					content,
					checked: wall_millis,
					held: true,
				}],
			);
		}
	}

	/// Checks the unchanged plain copies of a space on this device at `wall_millis`, as
	/// `nib verify` does: each is read in full, but those whose hashes `known` holds already,
	/// and the checks are recorded.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read, or the checks cannot be saved.
	pub(crate) fn check_plain_now(
		&mut self,
		space: SpaceId,
		known: &BTreeMap<String, Hash>,
		wall_millis: u64,
	) -> Result<(), NodeError> {
		let places = self.plain_places_here(space)?;
		let mut checks = Vec::new();
		let mut run = CheckRun::default();
		check_plain(
			&places,
			(wall_millis, &|| false, known),
			&mut checks,
			&mut run,
		);
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		Ok(open.stored.record_checks(Vec::new(), Vec::new(), checks)?)
	}

	/// This device's plain copies of every space's files, by file, in the order of the
	/// files' IDs.
	fn plain_by_file(&self) -> Result<Vec<(EntityId, SpaceId, Vec<PlainPlace>)>, NodeError> {
		let mut by_file: BTreeMap<(EntityId, SpaceId), Vec<PlainPlace>> = BTreeMap::new();
		for &space in self.spaces.keys() {
			for place in self.plain_places_here(space)? {
				by_file.entry((place.file, space)).or_default().push(place);
			}
		}
		Ok(by_file
			.into_iter()
			.map(|((file, space), places)| (file, space, places))
			.collect())
	}

	/// This device's plain copies of a space's files: the originals kept where they were
	/// found, and the copies of checkouts, each with the content it should hold.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn plain_places_here(&self, space: SpaceId) -> Result<Vec<PlainPlace>, NodeError> {
		let mut places = self.origin_places_here(space)?;
		places.extend(self.checkout_places_here(space)?);
		Ok(places)
	}

	/// This device's plain copies of a space's files, with the space's latest checks of them.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn plain_state(&self, space: SpaceId) -> Result<PlainState, NodeError> {
		let checks = self
			.space(space)?
			.stored
			.place_checks()?
			.into_iter()
			.map(|check| ((check.path, check.content), (check.checked, check.held)))
			.collect();
		Ok(PlainState {
			places: self.plain_places_here(space)?,
			checks,
		})
	}

	/// How many of this replica's copies of a space's files its checks found damaged in the
	/// 4 weeks before `wall_millis`, those damaged still included, and how many are damaged
	/// still, as its account says; `plain` is its plain copies there.
	///
	/// # Errors
	///
	/// Returns an error if the space cannot be read.
	pub(crate) fn own_damage(
		&self,
		space: SpaceId,
		plain: &PlainState,
		wall_millis: u64,
	) -> Result<(u64, u64), NodeError> {
		let since = wall_millis.saturating_sub(ROUND_MILLIS);
		let mut recent = 0_u64;
		let mut still = 0_u64;
		for copy in self.damaged_in(space, plain, (wall_millis, false))? {
			let unrepaired = match copy.copy {
				Damaged::Sealed { replaced, .. } => !replaced,
				Damaged::Plain { .. } => true,
			};
			recent += u64::from(copy.found >= since || unrepaired);
			still += u64::from(unrepaired);
		}
		Ok((recent, still))
	}

	/// What checks found damaged in a space and is still of interest at `wall`: sealed blobs
	/// whose key the space keeps, found damaged in the 4 weeks before or not yet replaced,
	/// and plain copies on this device, of `plain`, unchanged, that do not hold their
	/// content; with the files each holds the content of, where `named` asks for them.
	fn damaged_in(
		&self,
		space: SpaceId,
		plain: &PlainState,
		(wall, named): (u64, bool),
	) -> Result<Vec<DamagedCopy>, NodeError> {
		let open = self.space(space)?;
		let log = open.stored.log();
		let mut damaged = Vec::new();
		for (sealed, found) in open.stored.damaged_blobs()? {
			if log.wrapped_blob_keys_of(&sealed).is_empty() {
				continue;
			}
			let replaced = open.stored.holds_sealed(&sealed);
			if replaced && found < wall.saturating_sub(ROUND_MILLIS) {
				continue;
			}
			let files = match log.content_of_sealed(&sealed).filter(|_| named) {
				Some(content) => self.files_with(space, &content)?,
				None => Vec::new(),
			};
			damaged.push(DamagedCopy {
				space,
				files,
				found,
				copy: Damaged::Sealed { sealed, replaced },
			});
		}
		for (place, checked) in plain.damaged() {
			damaged.push(DamagedCopy {
				space,
				files: vec![place.file],
				found: checked,
				copy: Damaged::Plain {
					path: place.path.clone(),
				},
			});
		}
		Ok(damaged)
	}

	/// The files of a space whose current content is `content`, deleted ones aside.
	fn files_with(&self, space: SpaceId, content: &Hash) -> Result<Vec<EntityId>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut files = Vec::new();
		for entity in open
			.stored
			.find(&nibkeep_std::CONTENT, &nibkeep_model::Value::Blob(*content))?
		{
			let entity = state.canonical(entity)?;
			if !state.is_deleted(entity)?
				&& state.types(entity)?.contains(&nibkeep_std::FILE)
				&& !files.contains(&entity)
			{
				files.push(entity);
			}
		}
		Ok(files)
	}

	/// Where this replica's checks stand at `wall_millis`: the round under way, as `records`
	/// keep it, and what checks found damaged in each space, the personal space first: what
	/// is damaged still, and what came again in the 4 weeks before.
	///
	/// # Errors
	///
	/// Returns an error if the records or a space cannot be read.
	pub fn check_status(
		&self,
		records: &Records,
		wall_millis: u64,
	) -> Result<CheckStatus, NodeError> {
		let (round, last) = match records.round_with_last()? {
			Some((round, last)) => (Some(round), last),
			None => (None, None),
		};
		let mut ids: Vec<SpaceId> = self.spaces.keys().copied().collect();
		ids.sort_by_key(|&id| (id != self.info.personal, id));
		let mut damaged = Vec::new();
		let mut holds = SealedWalk::new(self.directory(), None).next().is_some();
		for id in ids {
			let plain = self.plain_state(id)?;
			holds |= !plain.places.is_empty();
			damaged.extend(self.damaged_in(id, &plain, (wall_millis, true))?);
		}
		Ok(CheckStatus {
			holds,
			round,
			last,
			damaged,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_plain_copy_that_changed_since_it_was_listed_is_not_read_as_damaged() {
		let directory = nibkeep_testkit::empty_directory(
			std::env::temp_dir(),
			&format!("nibkeep-checking-edited-{}", std::process::id()),
		);
		let path = directory.join("letter.txt");
		fs::write(&path, b"Dear Ada").expect("writes");
		let text = path.to_string_lossy().into_owned();
		let listed = PlainPlace {
			file: EntityId::from_bytes([1; 16]),
			path: text.clone(),
			content: Hash::of(b"Dear Ada"),
			untouched: true,
			size: 8,
			seen: seen_at(&text),
		};
		// Edited after it was listed: another size, so not read.
		fs::write(&path, b"Dear Ada,\n").expect("edits");
		let mut checks = Vec::new();
		let mut run = CheckRun::default();
		check_plain(
			std::slice::from_ref(&listed),
			(5, &|| false, &BTreeMap::new()),
			&mut checks,
			&mut run,
		);
		assert_eq!(checks, Vec::<PlaceCheck>::new());
		assert_eq!(run, CheckRun::default());
		// Listed as it is now, with other content than recorded: damage.
		let relisted = PlainPlace {
			seen: seen_at(&text),
			..listed.clone()
		};
		check_plain(
			std::slice::from_ref(&relisted),
			(5, &|| false, &BTreeMap::new()),
			&mut checks,
			&mut run,
		);
		assert_eq!((run.plain, run.damaged), (1, 1));
		assert!(!checks[0].held);
		let _ = fs::remove_dir_all(directory);
	}
}
