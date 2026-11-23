//! What a replica keeps of its spaces' sealed blobs, and so asks for in each sync: by its
//! device's role, size and pins, and when its files were last opened on the device
//! ([technical design: the planner]).
//!
//! [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner

use std::collections::{BTreeMap, BTreeSet};

use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::{DEVICE, PINNED};
use nibkeep_log::DeviceRole;
use nibkeep_model::storage::Statements;
use nibkeep_model::{Operation, State, Statement, Value, fields};
use nibkeep_std::{CONTENT, FILE, IN_TREE, ON_DEVICE, ROOT_OF, TAKEN, TREE_ROOT};
use nibkeep_time::{Day, Start};

use super::Library;
use super::policies::{Kept, facts_of, own_choices, size_of};
use crate::{NodeError, place};

/// What an everyday computer keeps without a size set: this share of its disk's size, a
/// tenth.
const SHARE_OF_DISK: u64 = 10;

/// How long a file opened on this device is kept whatever the size: 10 minutes, so that
/// opening it fetches it.
pub(crate) const OPENED_FOR: u64 = 600_000;

/// A moment, as files are ranked by it: a day, and a second of it, in UTC as recorded, or as
/// the camera's clock said for when a photo was taken.
pub(super) type Moment = (Day, u32);

/// The moment of a time in milliseconds since 1970.
pub(super) fn moment(millis: u64) -> Option<Moment> {
	Day::from_unix_millis(millis, 0).map(|day| (day, Day::second_of_day(millis, 0)))
}

/// What a replica keeps of a space's sealed blobs, by the contents they seal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Keeps {
	/// Every blob: on an everyday device while no device of the space is an archive node.
	All,
	/// Every blob but those of these contents: on an archive node, the contents that only
	/// files kept in place hold.
	AllBut(BTreeSet<Hash>),
	/// On an everyday device: the blobs of contents that no file of the space holds or held,
	/// which every replica keeps, and those of the files' contents that it keeps.
	Some {
		/// Every content that a file of the space holds or held.
		files: BTreeSet<Hash>,
		/// The files' contents that it keeps.
		kept: BTreeSet<Hash>,
	},
	/// No blob: on a device that drops the space, whose copies go as the copies elsewhere
	/// are proven
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	Nothing,
}

impl Keeps {
	/// Whether a blob of this content is kept.
	pub(crate) fn keeps(&self, content: &Hash) -> bool {
		match self {
			Self::All => true,
			Self::AllBut(not) => !not.contains(content),
			Self::Some { files, kept } => !files.contains(content) || kept.contains(content),
			Self::Nothing => false,
		}
	}
}

/// What a space's files tell the plan.
#[derive(Default)]
struct Files {
	/// Every content that a file of the space holds or held.
	all: BTreeSet<Hash>,
	/// The current content of each file that is neither deleted nor moved away, with its
	/// size and the newest moment of the files that hold it.
	current: BTreeMap<Hash, (Moment, u64)>,
	/// The current contents kept whatever the size: those of files pinned to this device,
	/// of trees with a root on it, and opened on it lately.
	always: BTreeSet<Hash>,
	/// The current contents that only files kept in place hold, whose originals stand for
	/// them: no device keeps them sealed, but an everyday device those it keeps whatever the
	/// size.
	in_place: BTreeSet<Hash>,
}

/// A file's current content, as an everyday device ranks it against the others of every
/// space.
struct Ranked {
	newest: Moment,
	space: SpaceId,
	content: Hash,
	size: u64,
}

impl Library {
	/// How many bytes of files this device keeps as an everyday device: as the person set it,
	/// else a tenth of the size of the disk its replica is on, on a computer, and none on a
	/// phone, which keeps thumbnails alone ([technical design: the planner]).
	///
	/// [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn keeps_up_to(&self) -> Result<u64, NodeError> {
		Ok(self
			.device_marks(self.info.device)?
			.keeps
			.unwrap_or_else(|| self.default_size()))
	}

	/// What this device keeps without a size set.
	fn default_size(&self) -> u64 {
		if place::whereabouts(&self.directory).phone {
			return 0;
		}
		fs4::total_space(&self.directory).map_or(0, |total| total / SHARE_OF_DISK)
	}

	/// The files pinned to one of the person's devices, in any space, by their IDs.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn pinned_files(&self, device: EntityId) -> Result<BTreeSet<EntityId>, NodeError> {
		let state = self.space(self.info.personal)?.stored.log().state();
		let device = state.canonical(device)?;
		Ok(state
			.current(device, &PINNED)?
			.iter()
			.filter_map(|statement| statement.value.as_reference())
			.collect())
	}

	/// Pins a file of a space to one of the person's devices that holds the space, which then
	/// keeps it whatever its size, or unpins it: in the personal space, where other members
	/// never see it. The device keeps it from its next sync.
	///
	/// # Errors
	///
	/// Returns [`NodeError::NotAFile`] if `file` is no file of the space,
	/// [`NodeError::WhichDevice`] if `device` is no device of the person's,
	/// [`NodeError::DeviceLacksSpace`] if it does not hold the space, as its choices say, and
	/// another error if the change cannot be saved.
	pub fn pin_file(
		&mut self,
		(space, file): (SpaceId, EntityId),
		device: EntityId,
		pinned: bool,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		// The file, and those merged into it, any of which a pin made before may name.
		let (file, members) = {
			let state = self.space(space)?.stored.log().state();
			let file = state.canonical(file)?;
			if pinned && (state.is_deleted(file)? || !state.types(file)?.contains(&FILE)) {
				return Err(NodeError::NotAFile(file.to_string()));
			}
			(file, state.merged(file)?)
		};
		let personal = self.info.personal;
		let state = self.space(personal)?.stored.log().state();
		let device = state.canonical(device)?;
		if state.is_deleted(device)? || !state.types(device)?.contains(&DEVICE) {
			return Err(NodeError::WhichDevice(format!(
				"{device} is no device of yours"
			)));
		}
		if pinned && !self.holding_devices(space)?.contains(&device) {
			return Err(NodeError::DeviceLacksSpace);
		}
		let current: Vec<StatementId> = state
			.current(device, &PINNED)?
			.iter()
			.filter(|statement| {
				statement
					.value
					.as_reference()
					.is_some_and(|pinned| pinned == file || members.contains(&pinned))
			})
			.map(|statement| statement.id)
			.collect();
		let operation = match (pinned, current.is_empty()) {
			(true, true) => Operation::assert(Statement::new(
				StatementId::random(rng),
				device,
				PINNED,
				Value::Reference(file),
			)),
			(false, false) => Operation::retract(device, current),
			_ => return Ok(()),
		};
		self.commit(personal, vec![operation], wall_millis, rng)
	}

	/// Records that a file of a space was opened on this device at `wall_millis`, outside the
	/// history: an everyday device then keeps it whatever its size for a while, which makes
	/// a sync fetch it, and ranks it by when it was last opened. It is never synced.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or the record cannot be saved.
	pub fn record_opened(
		&mut self,
		space: SpaceId,
		file: EntityId,
		wall_millis: u64,
	) -> Result<(), NodeError> {
		let open = self
			.spaces
			.get_mut(&space)
			.ok_or(NodeError::NoSpace(space))?;
		let file = open.stored.log().state().canonical(file)?;
		open.stored.record_opened(file, wall_millis)?;
		Ok(())
	}

	/// The other replicas whose latest accounts list a sealed copy of a file's current
	/// content, which a sync with one of them would bring: those to look for when the file is
	/// opened on this device and it lacks the content.
	///
	/// # Errors
	///
	/// Returns an error if the library has no such space, or it cannot be read.
	pub fn holders_of(
		&self,
		space: SpaceId,
		file: EntityId,
	) -> Result<Vec<crate::Replica>, NodeError> {
		let content = {
			let state = self.space(space)?.stored.log().state();
			let file = state.canonical(file)?;
			state
				.current(file, &CONTENT)?
				.iter()
				.find_map(|statement| match statement.value {
					Value::Blob(content) => Some(content),
					_ => None,
				})
		};
		let sealed: BTreeSet<Hash> = self.ordinary_sealed(space, content)?.into_iter().collect();
		let mut holders = Vec::new();
		for replica in self.replicas()?.into_iter().chain(self.peers()?) {
			let Some(key) = replica.signing.filter(|_| !replica.this) else {
				continue;
			};
			if holders
				.iter()
				.any(|holder: &crate::Replica| holder.signing == Some(key))
			{
				continue;
			}
			if self
				.listed_blobs(space, &key)?
				.is_some_and(|listed| !listed.is_disjoint(&sealed))
			{
				holders.push(replica);
			}
		}
		Ok(holders)
	}

	/// What this replica keeps of each of its spaces' sealed blobs, as of `wall_millis`
	/// ([technical design: the planner]).
	///
	/// [technical design: the planner]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#99-the-planner
	pub(crate) fn plan(&self, wall_millis: u64) -> Result<BTreeMap<SpaceId, Keeps>, NodeError> {
		let marks = self.device_marks(self.info.device)?;
		let archive = marks.role == DeviceRole::Archive;
		let pinned = self.pinned_files(self.info.device)?;
		let mut plan = BTreeMap::new();
		let mut ranked = Vec::new();
		let mut used: u64 = 0;
		for &space in self.spaces.keys() {
			if self.is_dropping(space) {
				plan.insert(space, Keeps::Nothing);
				continue;
			}
			if !archive && !self.has_archive_node(space)? {
				plan.insert(space, Keeps::All);
				continue;
			}
			let files = self.planned_files(space, (&pinned, archive), wall_millis)?;
			if archive {
				plan.insert(space, Keeps::AllBut(files.in_place));
				continue;
			}
			for content in &files.always {
				used = used.saturating_add(files.current.get(content).map_or(0, |(_, size)| *size));
			}
			ranked.extend(
				files
					.current
					.iter()
					.filter(|(content, _)| !files.always.contains(*content))
					.map(|(content, (newest, size))| Ranked {
						newest: *newest,
						space,
						content: *content,
						size: *size,
					}),
			);
			plan.insert(
				space,
				Keeps::Some {
					files: files.all,
					kept: files.always,
				},
			);
		}
		if !ranked.is_empty() {
			let size = marks.keeps.unwrap_or_else(|| self.default_size());
			// Newest first, each that still fits: one too large for what is left is passed
			// over, so that one large file leaves room for the smaller ones after it.
			ranked.sort_by(|a, b| {
				b.newest
					.cmp(&a.newest)
					.then(a.space.cmp(&b.space))
					.then(a.content.cmp(&b.content))
			});
			for each in ranked {
				let Some(after) = used.checked_add(each.size).filter(|after| *after <= size) else {
					continue;
				};
				used = after;
				if let Some(Keeps::Some { kept, .. }) = plan.get_mut(&each.space) {
					kept.insert(each.content);
				}
			}
		}
		Ok(plan)
	}

	/// What a space's files tell the plan: which contents only files kept in place hold; and
	/// on an everyday device, each other current content with when its newest file was taken
	/// or recorded, or last opened here, and which are kept whatever the size.
	fn planned_files(
		&self,
		space: SpaceId,
		(pinned, archive): (&BTreeSet<EntityId>, bool),
		wall_millis: u64,
	) -> Result<Files, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let storage = state.storage();
		let opened = open.stored.opened()?;
		let trees_here = self.trees_here(space)?;
		let policies = self.read_policies(space)?;
		let mut files = Files::default();
		let (mut in_store, mut in_place) = (BTreeSet::new(), BTreeSet::new());
		let mut seen = BTreeSet::new();
		for file in open
			.stored
			.find(&fields::TYPE, &Value::Text(FILE.to_string()))?
		{
			let file = state.canonical(file)?;
			if !seen.insert(file) {
				continue;
			}
			let members = state.merged(file)?;
			// Every content that it, or a file merged into it, holds or held.
			for &member in &members {
				for id in storage.of_field(member, &CONTENT)? {
					if let Some(recorded) = storage.get(id)?
						&& let Value::Blob(content) = recorded.statement.value
					{
						files.all.insert(content);
					}
				}
			}
			if gone(state, file)? || !state.types(file)?.contains(&FILE) {
				continue;
			}
			let Some((content, statement)) =
				state
					.current(file, &CONTENT)?
					.into_iter()
					.find_map(|statement| match statement.value {
						Value::Blob(content) => Some((content, statement.id)),
						_ => None,
					})
			else {
				continue;
			};
			let choice = policies.choose(&own_choices(state, file)?, &facts_of(state, file)?);
			if choice.needs.kept == Kept::Store {
				in_store.insert(content);
			} else {
				in_place.insert(content);
			}
			if archive {
				continue;
			}
			let opened_at = members
				.iter()
				.filter_map(|member| opened.get(member))
				.max()
				.copied();
			let taken = taken_of(state, file)?;
			let recorded = storage
				.get(statement)?
				.and_then(|recorded| moment(recorded.at.millis()));
			let newest = [taken.or(recorded), opened_at.and_then(moment)]
				.into_iter()
				.flatten()
				.max()
				.unwrap_or((
					Day::from_days_since_epoch(0).unwrap_or_else(|| unreachable!()),
					0,
				));
			let size = size_of(state, file)?.unwrap_or(0);
			let mut in_tree_here = false;
			for tree in state
				.current(file, &IN_TREE)?
				.iter()
				.filter_map(|statement| statement.value.as_reference())
			{
				in_tree_here |= trees_here.contains(&state.canonical(tree)?);
			}
			let lately = opened_at.is_some_and(|at| wall_millis.saturating_sub(at) < OPENED_FOR);
			if in_tree_here || lately || members.iter().any(|member| pinned.contains(member)) {
				files.always.insert(content);
			}
			let entry = files.current.entry(content).or_insert((newest, size));
			entry.0 = entry.0.max(newest);
		}
		files.in_place = in_place.difference(&in_store).copied().collect();
		// Nor does an everyday device keep the contents that only files kept in place hold,
		// whose originals stand for them, but those it keeps whatever the size.
		let (in_place, always) = (&files.in_place, &files.always);
		files
			.current
			.retain(|content, _| !in_place.contains(content) || always.contains(content));
		Ok(files)
	}

	/// The trees of a space with a root on this device, not deleted.
	fn trees_here(&self, space: SpaceId) -> Result<BTreeSet<EntityId>, NodeError> {
		let open = self.space(space)?;
		let state = open.stored.log().state();
		let mut trees = BTreeSet::new();
		for root in open
			.stored
			.find(&fields::TYPE, &Value::Text(TREE_ROOT.to_string()))?
		{
			let root = state.canonical(root)?;
			if state.is_deleted(root)?
				|| !state
					.current(root, &ON_DEVICE)?
					.iter()
					.any(|statement| statement.value.as_reference() == Some(self.info.device))
			{
				continue;
			}
			for statement in state.current(root, &ROOT_OF)? {
				if let Some(tree) = statement.value.as_reference() {
					trees.insert(state.canonical(tree)?);
				}
			}
		}
		Ok(trees)
	}
}

/// When a photo or a video was taken, as its `std.taken` starts.
pub(super) fn taken_of<S: Statements>(
	state: &State<S>,
	file: EntityId,
) -> Result<Option<Moment>, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state
		.current(file, &TAKEN)?
		.first()
		.and_then(|statement| match &statement.value {
			Value::Date(taken) => match taken.start() {
				Start::At { day, second } => Some((day, second.unwrap_or(0))),
				Start::BeginningOfTime => None,
			},
			_ => None,
		}))
}

/// Whether an entity is gone from its space: deleted, or moved to another.
pub(super) fn gone<S: Statements>(state: &State<S>, entity: EntityId) -> Result<bool, NodeError>
where
	NodeError: From<S::Error>,
{
	Ok(state.is_deleted(entity)? || !state.current(entity, &fields::MOVED_INTO)?.is_empty())
}
