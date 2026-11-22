//! A simulation of a person using a library: random operations, from a seed, checked after
//! every one against a model of what the library should show, with restarts in between.
//!
//! Each run reports its seed when it fails. `NIBKEEP_SIMULATION_SEED=<seed>` repeats one
//! run. `NIBKEEP_SIMULATION_RUNS=<count>` runs more seeds than the usual few, from
//! `NIBKEEP_SIMULATION_FIRST_SEED`, or 0. The long tests give `NIBKEEP_SIMULATION_RUN`, the
//! number of their run, instead, so that each run starts where the one before ended.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use nibkeep_base::{EntityId, Hash};
use nibkeep_crypto::PassphraseParams;
use nibkeep_crypto::rand_core::TryRng;
use nibkeep_model::{FieldDef, FieldName, Multiplicity, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{
	Dating, Edit, EntityView, Library, NewLibrary, NewProtection, NodeError, Ticks, Unlocker,
};
use nibkeep_testkit::{ManualClock, TestRng, millis_at};
use nibkeep_time::Day;

const PASSPHRASE: &str = "correct horse";

/// The runs of an ordinary test, and the operations of each.
const RUNS: u64 = 6;
const STEPS: usize = 150;

/// The words of text values. None starts another, so that a word found as the start of a
/// longer one is never a surprise.
const WORDS: [&str; 20] = [
	"amber", "birch", "cedar", "delta", "ember", "fjord", "grove", "heron", "inlet", "karst",
	"lunar", "maple", "north", "ocean", "pearl", "quill", "raven", "slate", "tidal", "umber",
];

const NOTE: TypeName = TypeName::from_static("local.note");
const TASK: TypeName = TypeName::from_static("local.task");

/// The day the library is shown as of. The simulation writes no valid times, so every
/// value shows on every day.
fn today() -> Day {
	"2026-10-12".parse().expect("a day")
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

/// The fields of the simulation, with their kinds and multiplicities, and the types that
/// have them from the start.
fn fields() -> Vec<(FieldName, ValueKind, Multiplicity, Vec<TypeName>)> {
	vec![
		(
			field("local.title"),
			ValueKind::Text,
			Multiplicity::One,
			vec![NOTE, TASK],
		),
		(
			field("local.tag"),
			ValueKind::Text,
			Multiplicity::Many,
			vec![NOTE],
		),
		(
			field("local.status"),
			ValueKind::Text,
			Multiplicity::LastWriterWins,
			vec![TASK],
		),
		(
			field("local.count"),
			ValueKind::Integer,
			Multiplicity::One,
			vec![NOTE],
		),
		(
			field("local.done"),
			ValueKind::Boolean,
			Multiplicity::One,
			vec![TASK],
		),
		(
			field("local.link"),
			ValueKind::Reference,
			Multiplicity::Many,
			vec![NOTE, TASK],
		),
		(
			field("local.due"),
			ValueKind::Date,
			Multiplicity::One,
			vec![TASK],
		),
	]
}

/// A field that a type gains during a run.
fn grown_field() -> (FieldName, ValueKind, Multiplicity) {
	(field("local.place"), ValueKind::Text, Multiplicity::Many)
}

fn definition(kind: ValueKind, multiplicity: Multiplicity) -> FieldDef {
	FieldDef::new(kind).with_multiplicity(multiplicity)
}

/// What the library should show.
#[derive(Clone, Default)]
struct Model {
	/// The fields that types have, with their kinds and multiplicities, and the types.
	fields: BTreeMap<FieldName, (ValueKind, Multiplicity, BTreeSet<TypeName>)>,
	entities: BTreeMap<EntityId, Entity>,
	/// The entities in the order they were created.
	created: Vec<EntityId>,
	/// The contents of the blobs imported.
	blobs: BTreeSet<Hash>,
	/// Every statement gets the next number: later statements win where the last writer
	/// wins.
	written: u64,
	/// How many changes the space's log holds.
	changes: usize,
}

#[derive(Clone, Default, PartialEq)]
struct Entity {
	types: BTreeSet<TypeName>,
	deleted: bool,
	/// The current values of each field, each with the number it was written as.
	current: BTreeMap<FieldName, Vec<(u64, Value)>>,
}

/// What an edit did to the model, or why the engine must refuse it.
#[derive(Debug, PartialEq)]
enum Outcome {
	/// Applied, and whether it makes operations to commit.
	Applied(bool),
	/// Refused: several values for a field with one.
	OneValue(FieldName),
	/// Refused: a value to remove that the field does not show.
	NoValue(FieldName),
	/// Refused: a type to take away that the entity does not have.
	NotOfType(TypeName),
	/// Refused: the entity's only type, to take away.
	OnlyType(TypeName),
}

impl Model {
	fn multiplicity(&self, name: &FieldName) -> Multiplicity {
		self.fields
			.get(name)
			.map_or(Multiplicity::Many, |(_, multiplicity, _)| *multiplicity)
	}

	/// The fields that an entity of these types can have.
	fn fields_of(&self, types: &BTreeSet<TypeName>) -> Vec<FieldName> {
		self.fields
			.iter()
			.filter(|(_, (_, _, of))| !of.is_disjoint(types))
			.map(|(name, _)| name.clone())
			.collect()
	}

	/// The values the library shows for a field, ordered, and whether they compete.
	fn shown(&self, entity: &Entity, name: &FieldName) -> (Vec<Value>, bool) {
		let current = entity.current.get(name).map_or(&[][..], Vec::as_slice);
		let mut values: Vec<Value> = match self.multiplicity(name) {
			Multiplicity::LastWriterWins => current
				.iter()
				.max_by_key(|(written, _)| *written)
				.map(|(_, value)| value.clone())
				.into_iter()
				.collect(),
			_ => current.iter().map(|(_, value)| value.clone()).collect(),
		};
		values.sort_by_key(|value| format!("{value:?}"));
		// The same value written twice shows once.
		values.dedup();
		let competing = self.multiplicity(name) == Multiplicity::One && values.len() > 1;
		(values, competing)
	}

	/// Applies an edit to an entity as the engine does, numbering the statements it writes
	/// after `written`.
	fn edit(&self, written: &mut u64, entity: &mut Entity, edit: &Edit) -> Outcome {
		match edit {
			Edit::Set(name, values) => {
				let one = matches!(
					self.multiplicity(name),
					Multiplicity::One | Multiplicity::LastWriterWins
				);
				if values.len() > 1 && one {
					return Outcome::OneValue(name.clone());
				}
				let last_writer_wins = self.multiplicity(name) == Multiplicity::LastWriterWins;
				let current = entity.current.entry(name.clone()).or_default();
				// Setting a field to what it shows changes nothing: the value written last, in a
				// field whose last writer wins, and otherwise each value once.
				let mut held: Vec<String> = if last_writer_wins {
					current
						.iter()
						.max_by_key(|(written, _)| *written)
						.map(|(_, value)| format!("{value:?}"))
						.into_iter()
						.collect()
				} else {
					current
						.iter()
						.map(|(_, value)| format!("{value:?}"))
						.collect()
				};
				let mut new: Vec<String> =
					values.iter().map(|value| format!("{value:?}")).collect();
				held.sort();
				if !last_writer_wins {
					held.dedup();
				}
				new.sort();
				if held == new {
					return Outcome::Applied(false);
				}
				current.clear();
				for value in values {
					*written += 1;
					current.push((*written, value.clone()));
				}
				Outcome::Applied(true)
			}
			Edit::Add(name, value) => {
				*written += 1;
				entity
					.current
					.entry(name.clone())
					.or_default()
					.push((*written, value.clone()));
				Outcome::Applied(true)
			}
			Edit::Remove(name, value) => {
				if !self.shown(entity, name).0.contains(value) {
					return Outcome::NoValue(name.clone());
				}
				let last_writer_wins = self.multiplicity(name) == Multiplicity::LastWriterWins;
				if let Some(current) = entity.current.get_mut(name) {
					current.retain(|(_, held)| !last_writer_wins && held != value);
				}
				Outcome::Applied(true)
			}
			// The simulation writes no languages, and so no forms.
			Edit::AddForm { .. } | Edit::RemoveForm(..) | Edit::RemoveFormIn { .. } => {
				unreachable!("the simulation writes no forms")
			}
			Edit::Delete => {
				let changes = !entity.deleted;
				entity.deleted = true;
				Outcome::Applied(changes)
			}
			Edit::Restore => {
				let changes = entity.deleted;
				entity.deleted = false;
				Outcome::Applied(changes)
			}
			Edit::AddType(name) => Outcome::Applied(entity.types.insert(name.clone())),
			Edit::RemoveType(name) => {
				if !entity.types.contains(name) {
					return Outcome::NotOfType(name.clone());
				}
				if entity.types.len() == 1 {
					return Outcome::OnlyType(name.clone());
				}
				entity.types.remove(name);
				Outcome::Applied(true)
			}
		}
	}

	/// The entities that a search for these words finds: those not deleted with a value
	/// shown of an ordinary text field that holds every word, the last one perhaps as the
	/// start of a longer word. The older values of a field whose last writer wins are not
	/// shown, and not found.
	fn search(&self, words: &[&str]) -> BTreeSet<EntityId> {
		self.entities
			.iter()
			.filter(|(_, entity)| !entity.deleted)
			.filter(|(_, entity)| {
				let shown: Vec<Value> = entity
					.current
					.keys()
					.flat_map(|name| self.shown(entity, name).0)
					.collect();
				let held: BTreeSet<&str> = shown
					.iter()
					.filter_map(Value::as_text)
					.flat_map(str::split_whitespace)
					.collect();
				words.iter().enumerate().all(|(index, word)| {
					if index + 1 == words.len() {
						held.iter().any(|held| held.starts_with(word))
					} else {
						held.contains(word)
					}
				})
			})
			.map(|(id, _)| *id)
			.collect()
	}
}

/// One run of the simulation.
struct Run {
	seed: u64,
	choices: TestRng,
	rng: TestRng,
	directory: PathBuf,
	library: Option<Library>,
	model: Model,
	/// The time of the last operation.
	now: u64,
	/// What the run did, for the report of a failure.
	log: Vec<String>,
	/// How often each situation worth testing came up.
	seen: BTreeMap<&'static str, usize>,
	/// The clock that tells bursts of changes apart, which the run moves as its seed says,
	/// so that a seed forms the same bursts on every machine.
	clock: ManualClock,
	/// The choices of how much time goes by, apart from those of operations, so that the
	/// operations of a seed are as they were before time went by.
	time: TestRng,
}

/// The situations that the runs of an ordinary test must come to, so that a change to the
/// simulation cannot quietly stop it from testing them.
const SITUATIONS: [&str; 20] = [
	"an entity got another type",
	"a type was taken away",
	"refused: a type the entity lacks",
	"refused: the only type",
	"a blob of several chunks",
	"a field took several values",
	"an edit changed nothing",
	"an entity was deleted",
	"an entity was restored",
	"competing values were settled",
	"refused: a value not shown",
	"refused: several values for one",
	"the library restarted",
	"a reference to an entity not there",
	"a search found something",
	"a search by the start of a word",
	"a type grew",
	"the shown value of a field with older ones was removed",
	"operations came close together",
	"operations came a second apart",
];

macro_rules! check {
	($run:expr, $condition:expr, $($message:tt)+) => {
		if !$condition {
			$run.fail(&format!($($message)+));
		}
	};
}

impl Run {
	fn new(seed: u64) -> Self {
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&format!("simulation {seed}"),
		);
		let mut rng = TestRng::new(&format!("simulation {seed}"));
		let now = millis_at("2026-10-12", 8, 0);
		let protection =
			NewProtection::Passphrase(PASSPHRASE, PassphraseParams::new(64, 1, 1).expect("valid"));
		let library = Library::create(
			&directory,
			&NewLibrary::new("Ada", "laptop", "nib"),
			&protection,
			now,
			&mut rng,
		)
		.expect("creates");
		let changes = library.status().expect("reads the status").spaces[0].changes;
		let clock = ManualClock::new();
		let mut library = library;
		library.set_ticks(&ticks(&clock));
		let mut run = Self {
			seed,
			choices: TestRng::new(&format!("simulation choices {seed}")),
			rng,
			directory,
			library: Some(library),
			model: Model {
				changes,
				..Model::default()
			},
			now,
			log: Vec::new(),
			seen: BTreeMap::new(),
			clock,
			time: TestRng::new(&format!("simulation time {seed}")),
		};
		run.define_types();
		run
	}

	fn library(&mut self) -> &mut Library {
		self.library.as_mut().expect("open")
	}

	fn fail(&self, message: &str) -> ! {
		let mut report = format!(
			"simulation seed {} failed: {message}\nlast operations:\n",
			self.seed
		);
		for line in self.log.iter().rev().take(12).rev() {
			let _ = writeln!(report, "  {line}");
		}
		let _ = write!(
			report,
			"repeat it with NIBKEEP_SIMULATION_SEED={}",
			self.seed
		);
		panic!("{report}");
	}

	fn saw(&mut self, situation: &'static str) {
		*self.seen.entry(situation).or_default() += 1;
	}

	fn below(&mut self, count: usize) -> usize {
		let count = u64::try_from(count).expect("small");
		let chosen = self.choices.try_next_u64().expect("infallible") % count;
		usize::try_from(chosen).expect("small")
	}

	fn chance(&mut self, percent: usize) -> bool {
		self.below(100) < percent
	}

	fn tick(&mut self) -> u64 {
		self.now += 1_000;
		self.now
	}

	fn define_types(&mut self) {
		for kind in [NOTE, TASK] {
			let mut definition = TypeDef::new(kind.clone(), 1);
			for (name, value_kind, multiplicity, of) in fields() {
				if of.contains(&kind) {
					definition = definition
						.with_field(name.clone(), self::definition(value_kind, multiplicity));
					self.model
						.fields
						.entry(name)
						.or_insert((value_kind, multiplicity, BTreeSet::new()))
						.2
						.insert(kind.clone());
				}
			}
			let now = self.tick();
			let personal = self.library().personal_space();
			let library = self.library.as_mut().expect("open");
			library
				.define_type(personal, &definition, now, &mut self.rng)
				.expect("defines");
			self.model.changes += 1;
		}
	}

	/// A type gains a field.
	fn grow(&mut self) {
		let (name, kind, multiplicity) = grown_field();
		if self.model.fields.contains_key(&name) {
			return;
		}
		self.log.push(format!("grow {NOTE} with {name}"));
		self.saw("a type grew");
		let definition =
			TypeDef::new(NOTE, 2).with_field(name.clone(), definition(kind, multiplicity));
		let now = self.tick();
		let personal = self.library().personal_space();
		let library = self.library.as_mut().expect("open");
		library
			.define_type(personal, &definition, now, &mut self.rng)
			.expect("grows");
		self.model
			.fields
			.insert(name, (kind, multiplicity, BTreeSet::from([NOTE])));
		self.model.changes += 1;
	}

	fn text(&mut self) -> String {
		let count = 1 + self.below(3);
		(0..count)
			.map(|_| WORDS[self.below(WORDS.len())])
			.collect::<Vec<_>>()
			.join(" ")
	}

	fn value(&mut self, name: &FieldName) -> Value {
		let kind = self.model.fields[name].0;
		match kind {
			ValueKind::Integer => Value::Integer(i64::try_from(self.below(5)).expect("small")),
			ValueKind::Boolean => Value::Boolean(self.chance(50)),
			ValueKind::Date => {
				let dates = ["2026", "2026-10", "2026-10-12", "2027~"];
				Value::Date(dates[self.below(dates.len())].parse().expect("a date"))
			}
			ValueKind::Reference if self.model.entities.is_empty() || self.chance(10) => {
				// A reference to an entity this space does not hold, which it may later.
				self.saw("a reference to an entity not there");
				Value::Reference(self.rng.entity_id())
			}
			ValueKind::Reference => {
				let index = self.below(self.model.entities.len());
				Value::Reference(*self.model.entities.keys().nth(index).expect("an entity"))
			}
			_ => Value::Text(self.text()),
		}
	}

	fn create(&mut self) {
		let types: BTreeSet<TypeName> = match self.below(4) {
			0 => [TASK].into(),
			1 => [NOTE, TASK].into(),
			_ => [NOTE].into(),
		};
		let mut entity = Entity {
			types: types.clone(),
			..Entity::default()
		};
		let mut values = Vec::new();
		for name in self.model.fields_of(&types) {
			let most = match self.model.multiplicity(&name) {
				Multiplicity::Many => 2,
				_ => 1,
			};
			for _ in 0..self.below(most + 1) {
				let value = self.value(&name);
				self.model.written += 1;
				entity
					.current
					.entry(name.clone())
					.or_default()
					.push((self.model.written, value.clone()));
				values.push((name.clone(), value));
			}
		}
		let types: Vec<TypeName> = types.into_iter().collect();
		self.log.push(format!("create {types:?} with {values:?}"));
		let now = self.tick();
		let personal = self.library().personal_space();
		let library = self.library.as_mut().expect("open");
		let created = library.create_entity(personal, &types, values, now, &mut self.rng);
		let id = match created {
			Ok(id) => id,
			Err(error) => self.fail(&format!("creating failed: {error}")),
		};
		self.model.entities.insert(id, entity);
		self.model.created.push(id);
		self.model.changes += 1;
	}

	/// An edit of an entity, mostly one that can be made.
	fn random_edit(&mut self, entity: &Entity, touched: &mut BTreeSet<FieldName>) -> Edit {
		match self.below(20) {
			0 => return Edit::Delete,
			1 => return Edit::Restore,
			2 => return Edit::AddType([NOTE, TASK][self.below(2)].clone()),
			3 => return Edit::RemoveType([NOTE, TASK][self.below(2)].clone()),
			_ => {}
		}
		// Mostly a field with values already, so that they pile up.
		let held: Vec<FieldName> = entity
			.current
			.iter()
			.filter(|(_, values)| !values.is_empty())
			.map(|(name, _)| name.clone())
			.collect();
		let name = if !held.is_empty() && self.chance(60) {
			held[self.below(held.len())].clone()
		} else {
			let fields = self.model.fields_of(&entity.types);
			fields[self.below(fields.len())].clone()
		};
		// Two writes to a field whose last value wins, in one change, are a tie that only the
		// statements' random IDs settle.
		let last_writer_wins = self.model.multiplicity(&name) == Multiplicity::LastWriterWins;
		if last_writer_wins && !touched.insert(name.clone()) {
			return Edit::Delete;
		}
		match self.below(4) {
			0 => {
				let count = match self.below(10) {
					0 => 0,
					1 => 2,
					_ => 1,
				};
				let values = (0..count).map(|_| self.value(&name)).collect();
				Edit::Set(name, values)
			}
			1 => {
				let shown = self.model.shown(entity, &name).0;
				let value = if !shown.is_empty() && self.chance(85) {
					shown[self.below(shown.len())].clone()
				} else {
					self.value(&name)
				};
				Edit::Remove(name, value)
			}
			_ => {
				let value = self.value(&name);
				Edit::Add(name, value)
			}
		}
	}

	fn edit(&mut self) {
		if self.model.entities.is_empty() {
			return self.create();
		}
		// Mostly one of the entities created last, so that values pile up on the same
		// fields: some compete, some are written over, some removed.
		let count = self.model.created.len();
		let index = if self.chance(70) {
			count - 1 - self.below(count.min(3))
		} else {
			self.below(count)
		};
		let id = self.model.created[index];
		let mut working = self.model.entities[&id].clone();
		let mut written = self.model.written;
		let mut edits = Vec::new();
		let mut touched = BTreeSet::new();
		// What the engine must do: commit, if any edit makes operations, or refuse.
		let mut expected = Outcome::Applied(false);
		// The situations that the edits come to, which count once the engine agrees.
		let mut situations = Vec::new();
		for _ in 0..=self.below(3) {
			let edit = self.random_edit(&working, &mut touched);
			let held = |name: &FieldName| working.current.get(name).map_or(0, Vec::len);
			let multiplicity = |name: &FieldName| self.model.multiplicity(name);
			situations.extend(match &edit {
				Edit::Remove(name, _)
					if multiplicity(name) == Multiplicity::LastWriterWins && held(name) > 1 =>
				{
					Some("the shown value of a field with older ones was removed")
				}
				Edit::Set(name, values)
					if multiplicity(name) == Multiplicity::One
						&& held(name) > 1
						&& values.len() == 1 =>
				{
					Some("competing values were settled")
				}
				Edit::Delete if !working.deleted => Some("an entity was deleted"),
				Edit::Restore if working.deleted => Some("an entity was restored"),
				Edit::AddType(name) if !working.types.contains(name) => {
					Some("an entity got another type")
				}
				Edit::RemoveType(name)
					if working.types.contains(name) && working.types.len() > 1 =>
				{
					Some("a type was taken away")
				}
				_ => None,
			});
			let outcome = self.model.edit(&mut written, &mut working, &edit);
			edits.push(edit);
			match outcome {
				Outcome::Applied(changes) => {
					if working.current.values().any(|values| values.len() > 1) {
						situations.push("a field took several values");
					}
					expected = Outcome::Applied(expected == Outcome::Applied(true) || changes);
				}
				refused => {
					situations = vec![match refused {
						Outcome::OneValue(_) => "refused: several values for one",
						Outcome::NotOfType(_) => "refused: a type the entity lacks",
						Outcome::OnlyType(_) => "refused: the only type",
						_ => "refused: a value not shown",
					}];
					expected = refused;
					break;
				}
			}
		}
		self.log.push(format!("edit {id} with {edits:?}"));
		let now = self.tick();
		let personal = self.library().personal_space();
		let library = self.library.as_mut().expect("open");
		let dating = Dating::today(now, 0).expect("a moment");
		let edited = library.edit_entity(personal, id, edits, &dating, now, &mut self.rng);
		match (&expected, &edited) {
			(Outcome::Applied(changes), Ok(())) => {
				if !changes {
					situations.push("an edit changed nothing");
				}
				self.model.entities.insert(id, working);
				self.model.written = written;
				self.model.changes += usize::from(*changes);
			}
			(Outcome::OneValue(expected), Err(NodeError::OneValue(refused)))
			| (Outcome::NoValue(expected), Err(NodeError::NoValue(refused)))
				if expected == refused => {}
			(Outcome::NotOfType(expected), Err(NodeError::NotOfType(refused)))
			| (Outcome::OnlyType(expected), Err(NodeError::OnlyType(refused)))
				if expected == refused => {}
			_ => self.fail(&format!(
				"the edit should give {expected:?}, and gave {edited:?}"
			)),
		}
		for situation in situations {
			self.saw(situation);
		}
	}

	fn import(&mut self) {
		let size = if self.chance(20) {
			70_000 + self.below(70_000)
		} else {
			self.below(2_000)
		};
		let mut content = vec![0; size];
		self.choices
			.try_fill_bytes(&mut content)
			.expect("infallible");
		self.log.push(format!("import a blob of {size} bytes"));
		if size > nibkeep_crypto::BLOB_CHUNK_LEN {
			self.saw("a blob of several chunks");
		}
		let now = self.tick();
		let personal = self.library().personal_space();
		let library = self.library.as_mut().expect("open");
		let hash = library
			.import_blob(personal, content.as_slice(), now, &mut self.rng)
			.expect("imports");
		check!(
			self,
			hash == Hash::of(&content),
			"the blob's hash is its content's"
		);
		self.model.blobs.insert(hash);
	}

	fn search(&mut self) {
		let count = 1 + self.below(2);
		let mut words: Vec<&str> = (0..count).map(|_| WORDS[self.below(WORDS.len())]).collect();
		if self.chance(30) {
			// The start of a word.
			let last = words.pop().expect("a word");
			words.push(&last[..3]);
			self.saw("a search by the start of a word");
		}
		let query = words.join(" ");
		self.log.push(format!("search {query:?}"));
		let personal = self.library().personal_space();
		let everything = self.library().search(personal, &query).expect("a space");
		// The first of them, as a page of results lists them, are the same. How many are
		// listed is not drawn, which would change the runs.
		let first = self.log.len() % (everything.len() + 2);
		let page = self
			.library()
			.search_first(personal, &query, first, |_, _| Ok(true))
			.expect("a space");
		let listed = page.listed.len();
		check!(
			self,
			page.listed == everything[..first.min(everything.len())]
				&& listed + page.more.count() <= everything.len()
				&& (listed + page.more.count() == everything.len()
					|| matches!(page.more, nibkeep_node::More::AtLeast(_))),
			"the first {first} of search {query:?} were {page:?}, of {everything:?}"
		);
		let found: BTreeSet<EntityId> = everything.into_iter().map(|found| found.id).collect();
		let expected = self.model.search(&words);
		if !found.is_empty() {
			self.saw("a search found something");
		}
		check!(
			self,
			found == expected,
			"search {query:?} found {found:?}, not {expected:?}"
		);
	}

	/// What the library shows: every entity, and what a search for every word finds.
	fn snapshot(&mut self) -> (Vec<Option<EntityView>>, Vec<Vec<EntityId>>) {
		let ids: Vec<EntityId> = self.model.entities.keys().copied().collect();
		let library = self.library.as_ref().expect("open");
		let personal = library.personal_space();
		let views = ids
			.iter()
			.map(|id| library.entity(personal, *id, today()).expect("a space"))
			.collect();
		let found = WORDS
			.iter()
			.map(|word| {
				library
					.search(personal, word)
					.expect("a space")
					.into_iter()
					.map(|found| found.id)
					.collect()
			})
			.collect();
		(views, found)
	}

	/// Closes the library and opens it again: it must show the same, its index built anew
	/// from the log the same as the one kept up to date change by change.
	fn restart(&mut self) {
		self.log.push("restart".to_owned());
		self.saw("the library restarted");
		let before = self.snapshot();
		self.library = None;
		let library = Library::open(&self.directory, &Unlocker::Passphrase(PASSPHRASE));
		match library {
			Ok(mut library) => {
				library.set_ticks(&ticks(&self.clock));
				self.library = Some(library);
			}
			Err(error) => self.fail(&format!("opening again failed: {error}")),
		}
		let after = self.snapshot();
		check!(
			self,
			before.0 == after.0,
			"the entities read differently after a restart"
		);
		check!(
			self,
			before.1 == after.1,
			"search finds differently after a restart"
		);
	}

	/// Compares everything the library shows with the model.
	fn compare(&mut self) {
		let library = self.library.as_ref().expect("open");
		let personal = library.personal_space();
		let changes = library.status().expect("reads the status").spaces[0].changes;
		check!(
			self,
			changes == self.model.changes,
			"the log holds {changes} changes, not {}",
			self.model.changes
		);
		for (id, entity) in &self.model.entities {
			let Some(view) = library.entity(personal, *id, today()).expect("a space") else {
				self.fail(&format!("entity {id} is gone"));
			};
			let types: BTreeSet<TypeName> = view.types.iter().cloned().collect();
			check!(
				self,
				types == entity.types,
				"entity {id} has types {types:?}"
			);
			check!(
				self,
				view.deleted == entity.deleted,
				"entity {id} deleted: {}",
				view.deleted
			);
			let mut shown = BTreeMap::new();
			for field in &view.fields {
				let mut values: Vec<Value> = field
					.values
					.iter()
					.map(|shown| shown.value.clone())
					.collect();
				values.sort_by_key(|value| format!("{value:?}"));
				shown.insert(field.field.clone(), (values, field.competing));
			}
			let expected: BTreeMap<FieldName, (Vec<Value>, bool)> = entity
				.current
				.keys()
				.map(|name| (name.clone(), self.model.shown(entity, name)))
				.filter(|(_, (values, _))| !values.is_empty())
				.collect();
			if shown != expected {
				let differing: Vec<String> = shown
					.keys()
					.chain(expected.keys())
					.collect::<BTreeSet<_>>()
					.into_iter()
					.filter(|name| shown.get(*name) != expected.get(*name))
					.map(|name| {
						format!(
							"{name} shows {}, not {}",
							values_text(shown.get(name)),
							values_text(expected.get(name))
						)
					})
					.collect();
				self.fail(&format!("entity {id}: {}", differing.join("; ")));
			}
		}
		let deleted: BTreeSet<EntityId> = library
			.deleted(personal)
			.expect("a space")
			.into_iter()
			.map(|listed| listed.id)
			.collect();
		let expected: BTreeSet<EntityId> = self
			.model
			.entities
			.iter()
			.filter(|(_, entity)| entity.deleted)
			.map(|(id, _)| *id)
			.collect();
		check!(
			self,
			deleted == expected,
			"the deleted entities are {deleted:?}"
		);
	}

	fn verify(&mut self) {
		let now = self.tick();
		let report = self.library().verify(now).expect("verifies");
		check!(self, report.is_sound(), "verify found problems: {report:?}");
		check!(
			self,
			report.spaces[0].blobs == self.model.blobs.len(),
			"verify read {} blobs, not {}",
			report.spaces[0].blobs,
			self.model.blobs.len()
		);
	}

	fn step(&mut self) {
		// Time goes by between operations: most come less than a tenth of a second apart, so
		// that their changes form bursts, and some end one, after a pause or a second.
		let millis = match self.time.try_next_u64().expect("infallible") % 10 {
			0..5 => {
				self.saw("operations came close together");
				10
			}
			5..7 => 90,
			7..9 => 150,
			_ => {
				self.saw("operations came a second apart");
				1_200
			}
		};
		self.clock.advance(Duration::from_millis(millis));
		match self.below(100) {
			0..8 => self.create(),
			8..65 => self.edit(),
			65..80 => self.search(),
			80..85 => self.import(),
			85..92 => self.restart(),
			92..94 => self.grow(),
			_ => self.compare(),
		}
	}
}

/// The times of bursts, read from the run's clock.
fn ticks(clock: &ManualClock) -> Ticks {
	let clock = clock.clone();
	Ticks::from_fn(move || clock.now())
}

/// Values in short, for reports.
fn values_text(values: Option<&(Vec<Value>, bool)>) -> String {
	let Some((values, competing)) = values else {
		return "nothing".to_owned();
	};
	let texts: Vec<String> = values
		.iter()
		.map(|value| match value {
			Value::Text(text) => format!("{text:?}"),
			Value::Integer(integer) => integer.to_string(),
			Value::Boolean(boolean) => boolean.to_string(),
			Value::Date(date) => date.to_string(),
			Value::Reference(entity) => format!("-> {entity}"),
			other => format!("{other:?}"),
		})
		.collect();
	format!(
		"[{}]{}",
		texts.join(", "),
		if *competing { ", competing" } else { "" }
	)
}

fn simulate(seed: u64, steps: usize) -> BTreeMap<&'static str, usize> {
	let mut run = Run::new(seed);
	for step in 0..steps {
		run.step();
		if step % 10 == 9 {
			run.compare();
		}
	}
	run.compare();
	run.restart();
	run.compare();
	run.verify();
	let directory = run.directory.clone();
	let situations = std::mem::take(&mut run.seen);
	drop(run);
	let _ = std::fs::remove_dir_all(directory);
	situations
}

#[test]
fn a_library_shows_what_its_model_says_through_random_use() {
	let variable = |name: &str| {
		std::env::var(name)
			.ok()
			.map(|value| value.parse::<u64>().expect("a number"))
	};
	if let Some(seed) = variable("NIBKEEP_SIMULATION_SEED") {
		simulate(seed, STEPS);
		return;
	}
	let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
	let runs = variable("NIBKEEP_SIMULATION_RUNS").unwrap_or(RUNS);
	let first = variable("NIBKEEP_SIMULATION_FIRST_SEED")
		.or_else(|| variable("NIBKEEP_SIMULATION_RUN").map(|run| run * runs))
		.unwrap_or(0);
	for seed in first..first + runs {
		for (situation, count) in simulate(seed, STEPS) {
			*seen.entry(situation).or_default() += count;
		}
	}
	println!("situations: {seen:#?}");
	let missed: Vec<&str> = SITUATIONS
		.into_iter()
		.filter(|situation| !seen.contains_key(situation))
		.collect();
	assert!(missed.is_empty(), "the runs never came to: {missed:?}");
}
