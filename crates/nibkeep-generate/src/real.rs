//! Real data, as `scripts/fetch-real-data.py` fetches it from Wikidata and MusicBrainz,
//! imported into a library for the benchmarks (`docs/Plan.md`, section 4).
//!
//! Each entity of the data becomes an entity of the standard type of its kind, such as
//! `std.film` or `std.person`, an audiobook an edition and an audiobook, and one of a kind
//! that the standard vocabulary lacks, such as terms, of a type named after it, such as
//! `local.term`. The data's fields become the standard vocabulary's where it has them,
//! such as `std.genre` or `std.born`, and fields named after them otherwise, such as
//! `local.members`, which the import adds to the entities' types. Dates, numbers and texts
//! become values of their kinds, durations quantities, and links references. A credit
//! becomes a field of the work, such as `std.director`, or with the character played, a
//! portrayal (`std.portrayal`), which its type's title names. Entities
//! are imported first, in the order of the data, each in a change of its own; then their
//! links, which can go round, as between a father and his child; then their dated values,
//! each in a change of its own with when it held, as a person records what happened or an
//! app a reading; then the credits, each in a change of its own. The changes are saved in batches ([technical design: saving in batches]).
//!
//! A field of dated numbers or quantities is a measurement, such as a dog's weight: it holds
//! one value at a time, each until the next starts ([values that change]), and has no values without dates.
//! Quantities keep the unit they were written in, and the field takes the unit of its
//! first. Every other field holds many values.
//!
//! The data is read and checked in full before anything is written, so that data the
//! importer cannot take leaves no library behind, and so that the benchmarks can time the
//! writing alone.
//!
//! [technical design: saving in batches]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#77-saving-in-batches
//! [values that change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#54-values-that-change-over-time

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use std::{fmt, fs, io, mem};

use nibkeep_base::EntityId;
use nibkeep_governance::NAME;
use nibkeep_model::{
	Decimal, FieldDef, FieldName, Multiplicity, Name, Quantity, Tag, TypeDef, TypeName, Value,
	ValueKind,
};
use nibkeep_node::{Dating, Edit, Library, NewValue};
use nibkeep_std as vocabulary;
use nibkeep_testkit::TestRng;
use nibkeep_time::Edtf;
use serde_json::{Map, Value as Json};

use crate::create_library;

/// How many changes a batch of the import holds, after which it ends with the item it is at,
/// and the longest that it lasts: the space's other writers wait for a batch to end.
const BATCH_CHANGES: usize = 1_000;
const BATCH_TIME: Duration = Duration::from_secs(1);

/// The importer's recipe: what an import with one name for each entity holds, numbered.
/// The benchmarks' results name it, and a new one starts a new series of them, which is
/// compared with no earlier one; so it is raised when what the importer makes of the data
/// changes, and only then. A test fails when the import of a sample changes without it
/// (`tests/real.rs`).
pub const RECIPE: u32 = 2;

/// The standard types of the data's kinds; a kind that has none is a type named after it.
fn types_of(kind: &str) -> Vec<TypeName> {
	match kind {
		"person" => vec![vocabulary::PERSON],
		"film" => vec![vocabulary::FILM],
		"series" | "tv series" => vec![vocabulary::SERIES],
		"season" => vec![vocabulary::SEASON],
		"episode" => vec![vocabulary::EPISODE],
		"game" => vec![vocabulary::GAME],
		"play" => vec![vocabulary::PLAY],
		"book" => vec![vocabulary::BOOK],
		"audiobook" => vec![vocabulary::EDITION, vocabulary::AUDIOBOOK],
		"character" => vec![vocabulary::CHARACTER],
		"group" => vec![vocabulary::ORGANIZATION],
		_ => Vec::new(),
	}
}

/// The standard vocabulary's field for a field of the data, if it has one.
fn standard_field(name: &str) -> Option<FieldName> {
	Some(match name {
		"published" => vocabulary::PUBLISHED,
		"born" => vocabulary::BORN,
		"died" => vocabulary::DIED,
		"founded" => vocabulary::FOUNDED,
		"dissolved" => vocabulary::DISSOLVED,
		"birth_name" => vocabulary::BIRTH_NAME,
		"series_ordinal" => vocabulary::NUMBER_IN_SERIES,
		"duration_minutes" => vocabulary::DURATION,
		"pages" => vocabulary::PAGES,
		"genre" => vocabulary::GENRE,
		"platform" => vocabulary::PLATFORM,
		"distributor" => vocabulary::DISTRIBUTOR,
		"broadcaster" => vocabulary::BROADCASTER,
		"based_on" => vocabulary::BASED_ON,
		"series" => vocabulary::IN_SERIES,
		"season" => vocabulary::IN_SEASON,
		"reads" => vocabulary::EDITION_OF,
		"appears_in" => vocabulary::APPEARS_IN,
		"spouse" => vocabulary::SPOUSE,
		"partner" => vocabulary::PARTNER,
		"father" => vocabulary::FATHER,
		"mother" => vocabulary::MOTHER,
		"child" => vocabulary::CHILD,
		"sibling" => vocabulary::SIBLING,
		"award" => vocabulary::AWARD,
		"member_of" => vocabulary::MEMBER_OF,
		"employer" => vocabulary::EMPLOYER,
		_ => return None,
	})
}

/// The field of a work that a credit of the data in a role becomes, without a character:
/// the standard vocabulary's, or one named after the role.
fn credit_field(role: &str) -> Option<FieldName> {
	Some(match role {
		"author" => vocabulary::AUTHOR,
		"publisher" => vocabulary::PUBLISHER,
		"developer" => vocabulary::DEVELOPER,
		"director" => vocabulary::DIRECTOR,
		"producer" => vocabulary::PRODUCER,
		"screenwriter" => vocabulary::SCREENWRITER,
		"composer" => vocabulary::COMPOSER,
		"narrator" => vocabulary::NARRATOR,
		"cast" => vocabulary::CAST,
		"voice" => vocabulary::VOICE_CAST,
		_ => {
			return format!("local.{}", role.replace([' ', '_'], "-"))
				.parse()
				.ok();
		}
	})
}

/// The fields that the standard vocabulary gives one value.
static ONE_VALUE: LazyLock<HashSet<FieldName>> = LazyLock::new(|| {
	vocabulary::types()
		.iter()
		.flat_map(|definition| {
			definition
				.fields()
				.filter(|(_, field)| field.multiplicity() == Multiplicity::One)
				.map(|(name, _)| name.clone())
				.collect::<Vec<_>>()
		})
		.collect()
});

/// The kind of a portrayal in a role, as text, where it says more than that it was played.
fn portrayal_kind(role: &str) -> Option<&'static str> {
	(role == "voice").then_some("voice")
}

/// The other names of an entity, besides its labels, when every language is imported.
const ALIAS: FieldName = FieldName::from_static("local.alias");

/// The languages of the names, in the order the fetching script writes them first: English,
/// Russian, Ukrainian, Swedish, and the form for every language.
const PREFERRED: [&str; 5] = ["en", "ru", "uk", "sv", "mul"];

const ENTITIES: &str = "entities.jsonl";
const CREDITS: &str = "credits.jsonl";
const MANIFEST: &str = "manifest.json";

/// The names and the texts in several languages that are imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Languages {
	/// One name for each entity, the first the data gives: in English, Russian, Ukrainian
	/// or Swedish, in that order, or in the form for every language, or else in any. Of the
	/// texts of a field, those in the first of these languages that they are in, or else in
	/// the language that comes first, and those in no language. Nothing is tagged with its
	/// language, so that the data is the same as before languages were.
	First,
	/// Every name and text of the data, each tagged with its language: an entity's labels
	/// as forms of its name, the first of them in the order of the data first, and its
	/// aliases as values of `local.alias`; the texts of a field as forms of one value when
	/// the data gives at most one in each language, and as values of their own otherwise.
	All,
}

impl Languages {
	/// The name of the choice, as the benchmarks' results record it.
	#[must_use]
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::First => "first",
			Self::All => "all",
		}
	}

	/// The choice that a name, as [`Languages::as_str`] gives it, stands for.
	#[must_use]
	pub fn named(name: &str) -> Option<Self> {
		[Self::First, Self::All]
			.into_iter()
			.find(|choice| choice.as_str() == name)
	}
}

/// What an import made.
#[derive(Debug)]
pub struct Imported {
	/// How many entities of the data it imported, credits aside.
	pub entities: u64,
	/// How many credits it imported.
	pub credits: u64,
	/// The library's entity of each entity of the data, by the data's ID.
	pub ids: HashMap<String, EntityId>,
	/// The time of the last change, in milliseconds since 1970.
	pub finished_at: u64,
}

/// Why real data could not be read.
#[derive(Debug)]
pub enum ImportError {
	/// A file of the data could not be read.
	Read {
		/// The file.
		file: String,
		/// Why.
		error: io::Error,
	},
	/// A line of the data is not what the fetching script writes.
	Data {
		/// The file.
		file: String,
		/// The line, from 1.
		line: usize,
		/// What is wrong with it.
		reason: String,
	},
}

impl fmt::Display for ImportError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Read { file, error } => write!(f, "cannot read {file}: {error}"),
			Self::Data { file, line, reason } => write!(f, "{file}:{line}: {reason}"),
		}
	}
}

impl std::error::Error for ImportError {}

/// Real data, read and checked, ready to import.
pub struct RealData {
	name: String,
	entities: Vec<Entity>,
	credits: Vec<Credit>,
}

impl RealData {
	/// Reads the real data in `data`, with the names and texts in the languages chosen, and
	/// checks it.
	///
	/// # Errors
	///
	/// Returns an error if a file of the data cannot be read, or a line is not what the
	/// fetching script writes: an entity without a name, an ID twice, a field with values
	/// of different kinds, a link or credit to an entity that the data does not hold, or a
	/// manifest without the data's name.
	pub fn read(data: &Path, languages: Languages) -> Result<Self, ImportError> {
		let manifest = read(data, MANIFEST)?;
		let name = serde_json::from_str::<Json>(&manifest)
			.ok()
			.and_then(|manifest| manifest.get("data")?.as_str().map(str::to_owned))
			.filter(|name| !name.is_empty())
			.ok_or_else(|| wrong(MANIFEST, 1, "no name of the data"))?;
		let entities = lines(data, ENTITIES)?
			.into_iter()
			.map(|(line, object)| read_entity(&object, line, languages))
			.collect::<Result<Vec<_>, _>>()?;
		let credits = read_credits(data)?;
		check(&entities, &credits)?;
		Ok(Self {
			name,
			entities,
			credits,
		})
	}

	/// The data's name, from its manifest.
	#[must_use]
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Makes a library in `directory` from the data, and returns it with what was imported.
	/// The library has the passphrase of every generated library.
	///
	/// # Panics
	///
	/// Panics if the library cannot be made or written to, such as when the directory
	/// holds one already.
	#[must_use]
	pub fn import(self, directory: &Path) -> (Library, Imported) {
		let Self {
			mut entities,
			mut credits,
			..
		} = self;
		let mut rng = TestRng::new("the real data");
		let mut library = create_library(directory, &mut rng);
		let now = crate::at(0, 0);
		let mut importer = Importer {
			rng,
			now,
			ids: HashMap::with_capacity(entities.len()),
		};
		importer.define_types(&mut library, &entities);
		in_batches(&mut library, &mut entities, |library, entity| {
			importer.create(library, entity);
			1
		});
		in_batches(&mut library, &mut entities, |library, entity| {
			importer.link(library, entity)
		});
		in_batches(&mut library, &mut entities, |library, entity| {
			importer.date(library, entity)
		});
		in_batches(&mut library, &mut credits, |library, credit| {
			importer.credit(library, credit);
			1
		});
		(
			library,
			Imported {
				entities: count(entities.len()),
				credits: count(credits.len()),
				ids: importer.ids,
				finished_at: importer.now,
			},
		)
	}
}

fn count(length: usize) -> u64 {
	u64::try_from(length).expect("a count")
}

/// An entity of the data, read.
struct Entity {
	id: String,
	line: usize,
	types: Vec<TypeName>,
	/// The type that has its fields.
	own_type: TypeName,
	values: Vec<NewValue>,
	links: Vec<(FieldName, Vec<String>)>,
	dated: Vec<Dated>,
}

/// A value of an entity of the data, with when it held.
struct Dated {
	field: FieldName,
	value: Held,
	valid: Edtf,
}

/// What a dated value holds.
enum Held {
	/// A link to an entity of the data, by its ID.
	Link(String),
	/// A value of its own, a number or a quantity.
	Value(Value),
}

/// A credit of the data, read.
struct Credit {
	line: usize,
	work: String,
	person: String,
	role: String,
	character: Option<String>,
}

fn read(data: &Path, file: &str) -> Result<String, ImportError> {
	fs::read_to_string(data.join(file)).map_err(|error| ImportError::Read {
		file: file.to_owned(),
		error,
	})
}

/// A line of a file of JSON lines: its number, from 1, and its object.
type Line = (usize, Map<String, Json>);

/// The lines of a file of JSON lines, each as an object, with its number.
fn lines(data: &Path, file: &'static str) -> Result<Vec<Line>, ImportError> {
	read(data, file)?
		.lines()
		.enumerate()
		.filter(|(_, text)| !text.trim().is_empty())
		.map(|(index, text)| {
			let line = index + 1;
			match serde_json::from_str(text) {
				Ok(Json::Object(object)) => Ok((line, object)),
				Ok(_) => Err(wrong(file, line, "not an object")),
				Err(error) => Err(wrong(file, line, error)),
			}
		})
		.collect()
}

fn wrong(file: &str, line: usize, reason: impl fmt::Display) -> ImportError {
	ImportError::Data {
		file: file.to_owned(),
		line,
		reason: reason.to_string(),
	}
}

/// A text member of an object.
fn text<'a>(object: &'a Map<String, Json>, key: &str) -> Option<&'a str> {
	object.get(key).and_then(Json::as_str)
}

/// The groups of values of an entity, such as its dates: each value of each field.
fn group<'a>(object: &'a Map<String, Json>, key: &str) -> Vec<(&'a str, &'a [Json])> {
	object
		.get(key)
		.and_then(Json::as_object)
		.map_or_default(|fields| {
			fields
				.iter()
				.map(|(name, values)| {
					(
						name.as_str(),
						values.as_array().map_or(&[][..], Vec::as_slice),
					)
				})
				.collect()
		})
}

/// The field for one of the data's: the standard vocabulary's, such as `std.birth-name`
/// for `birth_name`, or else one named after it, such as `local.members`.
fn field_named(name: &str, line: usize) -> Result<FieldName, ImportError> {
	if let Some(field) = standard_field(name) {
		return Ok(field);
	}
	format!("local.{}", name.replace('_', "-"))
		.parse()
		.map_err(|_| {
			wrong(
				ENTITIES,
				line,
				format!("no field can be named after {name}"),
			)
		})
}

/// How early a language comes in [`PREFERRED`]: a tag that is there exactly comes before
/// one whose language alone is, such as `en-gb`; others come after both.
fn rank(tag: &str) -> usize {
	let tag = tag.to_ascii_lowercase();
	let language = tag.split('-').next().unwrap_or_default();
	PREFERRED
		.iter()
		.position(|each| *each == tag)
		.map(|index| index * 2)
		.or_else(|| {
			PREFERRED
				.iter()
				.position(|each| *each == language)
				.map(|index| index * 2 + 1)
		})
		.unwrap_or(PREFERRED.len() * 2)
}

/// The texts of a field to import: of those in a language, the ones in the language that
/// comes first, by [`rank`] and then by the order of the data, for they are forms of the
/// same values; and those in no language.
fn texts_in_first_language<'a>(texts: &[(&'a str, Option<&str>)]) -> Vec<&'a str> {
	let first = texts
		.iter()
		.filter_map(|(_, language)| *language)
		.enumerate()
		.min_by_key(|(order, language)| (rank(language), *order))
		.map(|(_, language)| language.to_ascii_lowercase());
	texts
		.iter()
		.filter(|(_, language)| {
			language.is_none_or(|language| Some(language.to_ascii_lowercase()) == first)
		})
		.map(|(text, _)| *text)
		.collect()
}

/// The texts of a field, each in its language: forms of one value when there is at most
/// one in each language, the first in the order of the data first, and values of their
/// own otherwise.
fn texts_in_every_language(field: &FieldName, texts: &[(&str, Option<&str>)]) -> Vec<NewValue> {
	let value = |(text, language): &(&str, Option<&str>)| {
		let mut value = NewValue::new(field.clone(), Value::Text((*text).to_owned()));
		value.language = language.and_then(Tag::parse);
		value
	};
	let mut languages = HashSet::new();
	let forms = texts.len() > 1
		&& texts
			.iter()
			.all(|(_, language)| language.is_some_and(|language| languages.insert(language)));
	if !forms {
		return texts.iter().map(value).collect();
	}
	let mut first = value(&texts[0]);
	for (text, language) in &texts[1..] {
		first = first.with_form(
			Value::Text((*text).to_owned()),
			language.and_then(Tag::parse),
		);
	}
	vec![first]
}

/// The name of an entity, and its aliases when every language is imported, or `None` if
/// the data gives no name.
fn read_names(object: &Map<String, Json>, languages: Languages) -> Option<Vec<NewValue>> {
	let mut values = Vec::new();
	// The script writes names in the order of [`PREFERRED`], labels before aliases.
	let names: Vec<(&str, Option<Tag>, bool)> = object
		.get("names")
		.and_then(Json::as_array)
		.map_or_default(|names| {
			names
				.iter()
				.filter_map(|name| {
					let text = name.get("text").and_then(Json::as_str)?;
					let language = name.get("lang").and_then(Json::as_str).and_then(Tag::parse);
					let alias = name.get("alias").and_then(Json::as_bool) == Some(true);
					(!text.is_empty()).then_some((text, language, alias))
				})
				.collect()
		});
	let (first, _, _) = names.first()?;
	match languages {
		Languages::First => values.push(NewValue::new(NAME, Value::Name(Name::new(*first)))),
		Languages::All => {
			// The labels are forms of one name; an entity with aliases alone is named by
			// the first.
			let (labels, aliases): (Vec<_>, Vec<_>) =
				names.iter().partition(|(_, _, alias)| !alias);
			let (labels, aliases) = if labels.is_empty() {
				(aliases[..1].to_vec(), aliases[1..].to_vec())
			} else {
				(labels, aliases)
			};
			let (text, language, _) = labels[0];
			let mut name = NewValue::new(NAME, Value::Name(Name::new(*text)));
			name.language.clone_from(language);
			for (text, language, _) in &labels[1..] {
				name = name.with_form(Value::Name(Name::new(*text)), language.clone());
			}
			values.push(name);
			// An alias once, in the first language that gives it, and none that a form of
			// the name says already: Wikidata repeats aliases in many languages.
			let mut said: HashSet<&str> = labels.iter().map(|(text, _, _)| *text).collect();
			for (text, language, _) in aliases {
				if !said.insert(*text) {
					continue;
				}
				let mut alias = NewValue::new(ALIAS, Value::Text((*text).to_owned()));
				alias.language.clone_from(language);
				values.push(alias);
			}
		}
	}
	Some(values)
}

/// The whole numbers of an entity, durations as quantities in the minutes that the data
/// counts.
fn read_numbers(
	object: &Map<String, Json>,
	id: &str,
	line: usize,
) -> Result<Vec<NewValue>, ImportError> {
	let bad = |reason: String| wrong(ENTITIES, line, reason);
	let mut values = Vec::new();
	for (name, numbers) in group(object, "numbers") {
		let field = field_named(name, line)?;
		for number in numbers {
			let number = number
				.as_i64()
				.ok_or_else(|| bad(format!("{id} has a {name} that is not a whole number")))?;
			let value = if field == vocabulary::DURATION {
				Decimal::new(i128::from(number), 0)
					.and_then(|minutes| Quantity::new(minutes, "min"))
					.map(Value::Quantity)
					.ok_or_else(|| bad(format!("{id} has a {name} out of range")))?
			} else {
				Value::Integer(number)
			};
			values.push(NewValue::new(field.clone(), value));
		}
	}
	Ok(values)
}

fn read_entity(
	object: &Map<String, Json>,
	line: usize,
	languages: Languages,
) -> Result<Entity, ImportError> {
	let bad = |reason: String| wrong(ENTITIES, line, reason);
	let id = text(object, "id").ok_or_else(|| bad("no id".to_owned()))?;
	let kind = text(object, "kind").ok_or_else(|| bad("no kind".to_owned()))?;
	let mut types = types_of(kind);
	if types.is_empty() {
		types.push(
			format!("local.{}", kind.replace(' ', "-"))
				.parse()
				.map_err(|_| bad(format!("no type can be named after the kind {kind}")))?,
		);
	}
	// The type that the fields the standard vocabulary lacks are added to.
	let own_type = types[types.len() - 1].clone();

	let mut values = Vec::new();
	values.extend(read_names(object, languages).ok_or_else(|| bad(format!("{id} has no name")))?);
	for (name, dates) in group(object, "dates") {
		let field = field_named(name, line)?;
		for date in dates {
			let date = date
				.as_str()
				.and_then(|date| date.parse().ok())
				.ok_or_else(|| bad(format!("{id} has a {name} that is not a date: {date}")))?;
			values.push(NewValue::new(field.clone(), Value::Date(date)));
		}
	}
	values.extend(read_numbers(object, id, line)?);
	for (name, texts) in group(object, "texts") {
		let field = field_named(name, line)?;
		let texts = texts
			.iter()
			.map(|each| {
				let content = each
					.get("text")
					.and_then(Json::as_str)
					.ok_or_else(|| bad(format!("{id} has a {name} without text")))?;
				Ok((content, each.get("lang").and_then(Json::as_str)))
			})
			.collect::<Result<Vec<_>, ImportError>>()?;
		// A code, such as a number in a series, is in no language: one value, as the first
		// language writes it. A field of one value, such as a birth name, takes the first
		// text in each language, as forms of that value.
		let code = field == vocabulary::NUMBER_IN_SERIES;
		let texts = if ONE_VALUE.contains(&field) {
			let mut languages = HashSet::new();
			texts
				.into_iter()
				.filter(|(_, language)| languages.insert(language.map(str::to_ascii_lowercase)))
				.collect()
		} else {
			texts
		};
		match languages {
			Languages::First => {
				for content in texts_in_first_language(&texts) {
					values.push(NewValue::new(
						field.clone(),
						Value::Text(content.to_owned()),
					));
				}
			}
			Languages::All if code => values.extend(
				texts_in_first_language(&texts)
					.into_iter()
					.map(|content| NewValue::new(field.clone(), Value::Text(content.to_owned()))),
			),
			Languages::All => values.extend(texts_in_every_language(&field, &texts)),
		}
	}
	let mut links = Vec::new();
	for (name, targets) in group(object, "links") {
		let targets = targets
			.iter()
			.map(|target| {
				target
					.as_str()
					.map(str::to_owned)
					.ok_or_else(|| bad(format!("{id} has a {name} that is not an ID")))
			})
			.collect::<Result<_, _>>()?;
		links.push((field_named(name, line)?, targets));
	}
	// A field of the standard vocabulary that holds one value keeps the data's first: more
	// would compete until a person chose.
	let mut held = HashSet::new();
	values.retain(|value| {
		value.field == NAME || !ONE_VALUE.contains(&value.field) || held.insert(value.field.clone())
	});
	Ok(Entity {
		id: id.to_owned(),
		line,
		types,
		own_type,
		values,
		links,
		dated: read_dated(object, id, line)?,
	})
}

/// The dated values of an entity: each a link, a whole number or a quantity, with the time
/// when it held.
fn read_dated(
	object: &Map<String, Json>,
	id: &str,
	line: usize,
) -> Result<Vec<Dated>, ImportError> {
	let bad = |reason: String| wrong(ENTITIES, line, reason);
	let mut dated = Vec::new();
	for (name, values) in group(object, "dated") {
		let field = field_named(name, line)?;
		for each in values {
			let valid = each
				.get("valid")
				.and_then(Json::as_str)
				.and_then(|valid| valid.parse().ok())
				.ok_or_else(|| bad(format!("{id} has a {name} without a valid time")))?;
			let kinds = ["link", "number", "quantity"];
			if kinds
				.iter()
				.filter(|kind| each.get(**kind).is_some())
				.count()
				!= 1
			{
				return Err(bad(format!(
					"{id} has a {name} that is not one link, whole number or quantity"
				)));
			}
			let value = if let Some(target) = each.get("link").and_then(Json::as_str) {
				Held::Link(target.to_owned())
			} else if let Some(number) = each.get("number").and_then(Json::as_i64) {
				Held::Value(Value::Integer(number))
			} else if let Some(quantity) = each
				.get("quantity")
				.and_then(Json::as_str)
				.and_then(|quantity| quantity.parse::<Quantity>().ok())
			{
				Held::Value(Value::Quantity(quantity))
			} else {
				return Err(bad(format!(
					"{id} has a {name} that is not a link, a whole number or a quantity"
				)));
			};
			dated.push(Dated {
				field: field.clone(),
				value,
				valid,
			});
		}
	}
	Ok(dated)
}

fn read_credits(data: &Path) -> Result<Vec<Credit>, ImportError> {
	lines(data, CREDITS)?
		.into_iter()
		.map(|(line, object)| {
			let member = |key: &str| {
				text(&object, key)
					.map(str::to_owned)
					.ok_or_else(|| wrong(CREDITS, line, format!("no {key}")))
			};
			let role = member("role")?;
			let character = text(&object, "character").map(str::to_owned);
			// A character is played: only a role in the cast or a voice has one.
			if character.is_some() && !matches!(role.as_str(), "cast" | "voice") {
				return Err(wrong(
					CREDITS,
					line,
					format!("a character for the role {role}, which plays none"),
				));
			}
			if credit_field(&role).is_none() {
				return Err(wrong(
					CREDITS,
					line,
					format!("no field can be named after the role {role}"),
				));
			}
			Ok(Credit {
				line,
				work: member("work")?,
				person: member("person")?,
				role,
				character,
			})
		})
		.collect()
}

/// The kind of a value that the data gives.
fn kind_of(value: &Value) -> ValueKind {
	match value {
		Value::Date(_) => ValueKind::Date,
		Value::Integer(_) => ValueKind::Integer,
		Value::Name(_) => ValueKind::Name,
		Value::Quantity(_) => ValueKind::Quantity,
		_ => ValueKind::Text,
	}
}

/// The kind of a dated value.
fn kind_held(held: &Held) -> ValueKind {
	match held {
		Held::Link(_) => ValueKind::Reference,
		Held::Value(value) => kind_of(value),
	}
}

/// The kinds of the standard vocabulary's fields.
fn standard_kinds() -> HashMap<FieldName, ValueKind> {
	vocabulary::types()
		.iter()
		.flat_map(|definition| {
			definition
				.fields()
				.map(|(name, field)| (name.clone(), field.kind()))
				.collect::<Vec<_>>()
		})
		.collect()
}

/// Checks what the library would refuse, or what would make it differ from the data: that
/// each ID is there once; that each field has values of one kind, in every type, since a
/// field is the same field in every type, and the standard vocabulary's fields have the
/// kinds it gives them; that a measurement has no values without dates; and that every link
/// and credit refers to an entity of the data.
fn check(entities: &[Entity], credits: &[Credit]) -> Result<(), ImportError> {
	let mut ids = HashSet::with_capacity(entities.len());
	let mut kinds = standard_kinds();
	for entity in entities {
		if !ids.insert(entity.id.as_str()) {
			return Err(wrong(
				ENTITIES,
				entity.line,
				format!("{} is in the data twice", entity.id),
			));
		}
		let fields = entity
			.values
			.iter()
			.map(|value| (&value.field, kind_of(&value.value)))
			.chain(
				entity
					.links
					.iter()
					.map(|(field, _)| (field, ValueKind::Reference)),
			)
			.chain(
				entity
					.dated
					.iter()
					.map(|dated| (&dated.field, kind_held(&dated.value))),
			);
		for (field, kind) in fields {
			match kinds.get(field) {
				None => {
					kinds.insert(field.clone(), kind);
				}
				Some(known) if *known != kind => {
					return Err(wrong(
						ENTITIES,
						entity.line,
						format!("{field} has values of two kinds in the data"),
					));
				}
				Some(_) => {}
			}
		}
	}
	let measurements: HashSet<&FieldName> = entities
		.iter()
		.flat_map(|entity| &entity.dated)
		.filter(|dated| matches!(dated.value, Held::Value(_)))
		.map(|dated| &dated.field)
		.collect();
	for entity in entities {
		let undated = entity
			.values
			.iter()
			.map(|value| &value.field)
			.chain(entity.links.iter().map(|(field, _)| field));
		if let Some(field) = undated
			.into_iter()
			.find(|field| measurements.contains(field))
		{
			return Err(wrong(
				ENTITIES,
				entity.line,
				format!("{field} is a measurement, with dates, and has a value without one"),
			));
		}
	}
	let missing = |id: &str, file: &str, line: usize| {
		if ids.contains(id) {
			Ok(())
		} else {
			Err(wrong(file, line, format!("{id} is not in the data")))
		}
	};
	for entity in entities {
		let dated = entity.dated.iter().filter_map(|dated| match &dated.value {
			Held::Link(target) => Some(target),
			Held::Value(_) => None,
		});
		for target in entity
			.links
			.iter()
			.flat_map(|(_, targets)| targets)
			.chain(dated)
		{
			missing(target, ENTITIES, entity.line)?;
		}
	}
	for credit in credits {
		for id in [&credit.work, &credit.person]
			.into_iter()
			.chain(&credit.character)
		{
			missing(id, CREDITS, credit.line)?;
		}
	}
	Ok(())
}

/// Runs `each`, which makes changes of the personal space and returns how many, on every
/// item, in batches.
fn in_batches<T>(
	library: &mut Library,
	items: &mut [T],
	mut each: impl FnMut(&mut Library, &mut T) -> usize,
) {
	let personal = library.personal_space();
	let mut rest = items;
	while !rest.is_empty() {
		let started = Instant::now();
		let done = library
			.batch(personal, |library| {
				let (mut done, mut changes) = (0, 0);
				for item in rest.iter_mut() {
					changes += each(library, item);
					done += 1;
					if changes >= BATCH_CHANGES || started.elapsed() >= BATCH_TIME {
						break;
					}
				}
				Ok(done)
			})
			.expect("saves a batch");
		rest = &mut mem::take(&mut rest)[done..];
	}
}

/// An import under way, into a library.
struct Importer {
	rng: TestRng,
	/// The time of the last change.
	now: u64,
	ids: HashMap<String, EntityId>,
}

impl Importer {
	/// The time of the next change: a millisecond after the last.
	fn tick(&mut self) -> u64 {
		self.now += 1;
		self.now
	}

	/// Defines a type for each kind, with the fields its entities have, and credits.
	fn define_types(&mut self, library: &mut Library, entities: &[Entity]) {
		let mut types: BTreeMap<&TypeName, BTreeMap<&FieldName, FieldDef>> = BTreeMap::new();
		for entity in entities {
			let fields = types.entry(&entity.own_type).or_default();
			// The standard vocabulary defines its own fields.
			let own = |field: &FieldName| field.as_str().starts_with("local.");
			for value in entity.values.iter().filter(|value| own(&value.field)) {
				fields.insert(&value.field, FieldDef::new(kind_of(&value.value)).many());
			}
			for (field, _) in entity.links.iter().filter(|(field, _)| own(field)) {
				fields.insert(field, FieldDef::new(ValueKind::Reference).many());
			}
			for dated in entity.dated.iter().filter(|dated| own(&dated.field)) {
				fields
					.entry(&dated.field)
					.or_insert_with(|| match &dated.value {
						Held::Link(_) => FieldDef::new(ValueKind::Reference).many(),
						Held::Value(Value::Quantity(quantity)) => {
							FieldDef::new(ValueKind::Quantity).with_unit(quantity.unit())
						}
						Held::Value(value) => FieldDef::new(kind_of(value)),
					});
			}
		}
		// A kind of its own, or a standard type with fields that the vocabulary lacks.
		let definitions = types
			.into_iter()
			.filter(|(name, fields)| name.as_str().starts_with("local.") || !fields.is_empty())
			.map(|(name, fields)| {
				fields.into_iter().fold(
					TypeDef::new(name.clone(), 1),
					|definition, (field, field_definition)| {
						definition.with_field(field.clone(), field_definition)
					},
				)
			});
		let personal = library.personal_space();
		for definition in definitions {
			let now = self.tick();
			library
				.define_type(personal, &definition, now, &mut self.rng)
				.expect("defines a type");
		}
	}

	/// Adds an entity with its values, which it takes, for nothing needs them after.
	fn create(&mut self, library: &mut Library, entity: &mut Entity) {
		let personal = library.personal_space();
		let now = self.tick();
		let values = mem::take(&mut entity.values);
		let id = library
			.create_entity_with(personal, &entity.types, values, now, &mut self.rng)
			.expect("adds an entity");
		self.ids.insert(entity.id.clone(), id);
	}

	/// A reference to an entity of the data, which [`check`] made sure is there.
	fn target(&self, id: &str) -> Value {
		Value::Reference(self.ids[id])
	}

	/// Links an entity, in one change, and returns how many changes it made.
	fn link(&mut self, library: &mut Library, entity: &Entity) -> usize {
		if entity.links.is_empty() {
			return 0;
		}
		let edits = entity
			.links
			.iter()
			.map(|(field, targets)| {
				Edit::Set(
					field.clone(),
					targets.iter().map(|target| self.target(target)).collect(),
				)
			})
			.collect();
		let personal = library.personal_space();
		let now = self.tick();
		let dating = Dating::today(now, 0).expect("a moment");
		library
			.edit_entity(
				personal,
				self.ids[&entity.id],
				edits,
				&dating,
				now,
				&mut self.rng,
			)
			.expect("links an entity");
		1
	}

	/// Adds the dated values of an entity, which it takes, each in a change of its own with
	/// its valid time, and returns how many changes it made.
	fn date(&mut self, library: &mut Library, entity: &mut Entity) -> usize {
		let personal = library.personal_space();
		let dated = mem::take(&mut entity.dated);
		let changes = dated.len();
		for Dated {
			field,
			value,
			valid,
		} in dated
		{
			let value = match value {
				Held::Link(target) => self.target(&target),
				Held::Value(value) => value,
			};
			let now = self.tick();
			let dating = Dating::today(now, 0)
				.expect("a moment")
				.with_valid_time(valid);
			library
				.edit_entity(
					personal,
					self.ids[&entity.id],
					vec![Edit::Add(field, value)],
					&dating,
					now,
					&mut self.rng,
				)
				.expect("adds a dated value");
		}
		changes
	}

	/// Adds a credit: with the character played, a portrayal, and otherwise a value of the
	/// work's field for the role, each in a change of its own.
	fn credit(&mut self, library: &mut Library, credit: &Credit) {
		let personal = library.personal_space();
		let now = self.tick();
		let performer = self.target(&credit.person);
		let work = self.target(&credit.work);
		match (&credit.character, credit.role.as_str()) {
			(Some(character), "cast" | "voice") => {
				let mut values = vec![
					(vocabulary::PERFORMER, performer),
					(vocabulary::PORTRAYED, self.target(character)),
					(vocabulary::PRODUCTION, work),
				];
				if let Some(kind) = portrayal_kind(&credit.role) {
					values.push((vocabulary::PORTRAYAL_KIND, Value::Text(kind.to_owned())));
				}
				library
					.create_entity(
						personal,
						&[vocabulary::PORTRAYAL],
						values,
						now,
						&mut self.rng,
					)
					.expect("adds a portrayal");
			}
			(_, role) => {
				let field = credit_field(role).expect("a role that names a field, checked");
				let dating = Dating::today(now, 0).expect("a moment");
				library
					.edit_entity(
						personal,
						self.ids[&credit.work],
						vec![Edit::Add(field, performer)],
						&dating,
						now,
						&mut self.rng,
					)
					.expect("credits a work");
			}
		}
	}
}
