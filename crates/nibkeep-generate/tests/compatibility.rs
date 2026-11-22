//! Compatibility fixtures: libraries written by earlier versions of NibKeep, which every
//! later version must read as they were written.
//!
//! Each fixture in `fixtures/libraries/` is a library and a description of what it holds.
//! The library is read from a copy, so that no test can change it, and its description
//! must match. The `draft` fixture is the one that the current code writes, file for file:
//! while the formats are drafts it may change, and `NIBKEEP_BLESS=1` writes it again. Once
//! data is stored for real, fixtures are only ever added, never changed or removed.
//!
//! The descriptions show what the engine shows. Statements' times and authors join them
//! with history.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_generate::PASSPHRASE;
use nibkeep_model::schema::{FIELD_DEFINITION, TYPE_DEFINITION};
use nibkeep_model::{FieldDef, Multiplicity, Value};
use nibkeep_node::{Drive, Library, Unlocker, declare_drive};
use nibkeep_time::Day;

const DRAFT: &str = "draft";

/// The library of a fixture, and its description.
const LIBRARY: &str = "library";
const DESCRIPTION: &str = "description.txt";

/// Files that a library writes as it runs, or derives from its log files, which a fixture
/// leaves out: a library opened from the fixture makes them again.
const TRANSIENT: [&str; 8] = [
	"lock",
	"gate",
	"engines.lock",
	"log.sqlite-wal",
	"log.sqlite-shm",
	"state.sqlite",
	"state.sqlite-wal",
	"state.sqlite-shm",
];

/// Files that a fixture holds, and that every later version must read, but that the code
/// need not write again byte for byte: the engines file records the engine's version, which
/// changes with each release.
const UNCOMPARED: [&str; 1] = ["engines.cbor"];

/// The time of the `minute`th change of the first round.
fn at(minute: u64) -> u64 {
	nibkeep_generate::at(0, minute)
}

/// The day the fixture is shown as of: the day it was written.
fn today() -> Day {
	"2026-10-12".parse().expect("a day")
}

/// Writes the draft fixture: a library of one round of the generator, which its engines file
/// says that `nibkeep-generate` made.
fn generate(directory: &Path) {
	nibkeep_node::name_program("nibkeep-generate");
	drop(nibkeep_generate::generate(directory, 1));
}

/// Describes an entity as the engine shows it today, and its dated fields as of an earlier
/// day.
fn describe_entity(library: &Library, space: SpaceId, entity: EntityId) -> Vec<String> {
	let mut lines = Vec::new();
	let view = library
		.entity(space, entity, today())
		.expect("a space")
		.expect("an entity");
	let types: Vec<String> = view.types.iter().map(ToString::to_string).collect();
	lines.push(format!(
		"  entity {} {:?}{}: {}",
		view.id,
		view.name.as_deref().unwrap_or("-"),
		if view.deleted { ", deleted" } else { "" },
		types.join(" "),
	));
	for field in &view.fields {
		for shown in &field.values {
			let valid_time = shown
				.valid_time
				.as_ref()
				.map_or_default(|valid_time| format!(" [{valid_time}]"));
			let language = shown
				.language
				.as_ref()
				.map_or_default(|language| format!(" @{language}"));
			let forms = match shown.other_forms {
				0 => String::new(),
				forms => format!(", {forms} other forms"),
			};
			lines.push(format!(
				"    {} {}{language}{valid_time}{}{forms} ({})",
				field.field,
				value_text(&shown.value),
				if field.competing { ", competing" } else { "" },
				shown.statement,
			));
		}
	}
	// The history in the world: the dated fields as of an earlier day.
	let earlier: Day = "2022-01-01".parse().expect("a day");
	let then = library
		.entity(space, entity, earlier)
		.expect("a space")
		.expect("an entity");
	for field in &then.fields {
		for shown in &field.values {
			if let Some(valid_time) = &shown.valid_time {
				lines.push(format!(
					"    on {earlier}: {} {} [{valid_time}] ({})",
					field.field,
					value_text(&shown.value),
					shown.statement,
				));
			}
		}
	}
	lines
}

/// Describes everything a library holds, as far as the engine shows it.
fn describe(library: &mut Library) -> String {
	let mut description = String::new();
	let status = library.status().expect("reads the status");
	let report = library.verify(at(59)).expect("verifies");
	let mut push = |text: String| {
		description.push_str(&text);
		description.push('\n');
	};
	for (role, who) in [
		("user", &status.user),
		("device", &status.device),
		("replica", &status.replica),
	] {
		push(format!(
			"{role} {} {}",
			who.id,
			who.name.as_deref().unwrap_or("-")
		));
	}
	for (space, checked) in status.spaces.iter().zip(&report.spaces) {
		push(format!(
			"space {} {}: {} entities, {} changes",
			space.id,
			space.name.as_deref().unwrap_or("-"),
			space.entities,
			space.changes,
		));
		push(format!(
			"  verified: {} changes, {} blobs, {} waiting, problems {:?}",
			checked.changes, checked.blobs, checked.waiting, checked.problems
		));
		let types = library.types(space.id).expect("a space");
		for definition in types.defined_by_space() {
			push(format!(
				"  type {} {}{}",
				definition.name(),
				definition.version(),
				labels_text(definition.labels())
			));
			for (name, field) in definition.fields() {
				push(format!("    {name} {}", field_text(field)));
			}
		}
		let definitions = [TYPE_DEFINITION, FIELD_DEFINITION]
			.iter()
			.map(|kind| library.entities(space.id, Some(kind)).expect("a space"))
			.map(|entities| entities.len().to_string())
			.collect::<Vec<_>>();
		push(format!(
			"  defined by {} types and {} fields",
			definitions[0], definitions[1]
		));
		let live = library.entities(space.id, None).expect("a space");
		let deleted = library.deleted(space.id).expect("a space");
		for listed in live.iter().chain(&deleted) {
			for line in describe_entity(library, space.id, listed.id) {
				push(line);
			}
		}
		for query in [
			"leviathan",
			"anarchy physics",
			"le guin",
			"dispo",
			"password",
			"library",
		] {
			let found: Vec<String> = library
				.search(space.id, query)
				.expect("a space")
				.iter()
				.map(|found| found.id.to_string())
				.collect();
			push(format!("  search {query:?}: [{}]", found.join(", ")));
		}
	}
	description
}

fn field_text(field: &FieldDef) -> String {
	let multiplicity = match field.multiplicity() {
		Multiplicity::One => "one".to_owned(),
		Multiplicity::Many => "many".to_owned(),
		Multiplicity::LastWriterWins => "last writer wins".to_owned(),
		other @ Multiplicity::Other(_) => format!("other {:?}", other.code()),
	};
	let mut text = format!("{:?}, {multiplicity}", field.kind());
	if field.is_secret() {
		text.push_str(", secret");
	}
	if field.is_role() {
		text.push_str(", role");
	}
	text.push_str(&labels_text(field.labels()));
	text
}

/// Labels, as `, labels en "Book", sv "Bok"`, or nothing without them.
fn labels_text(labels: &nibkeep_model::Labels) -> String {
	if labels.is_empty() {
		return String::new();
	}
	let labels: Vec<String> = labels
		.iter()
		.map(|(tag, text)| format!("{tag} {text:?}"))
		.collect();
	format!(", labels {}", labels.join(", "))
}

fn value_text(value: &Value) -> String {
	match value {
		Value::Text(text) => format!("{text:?}"),
		Value::Integer(integer) => integer.to_string(),
		Value::Boolean(boolean) => boolean.to_string(),
		Value::Date(date) => format!("date {date}"),
		Value::Reference(entity) => format!("-> {entity}"),
		Value::Blob(hash) => format!("blob {hash}"),
		Value::Decimal(decimal) => format!("decimal {decimal}"),
		Value::Quantity(quantity) => format!("quantity {quantity}"),
		Value::Name(name) => {
			let parts: Vec<String> = name
				.parts()
				.map(|(part, text)| format!("{part} {text:?}"))
				.collect();
			format!("name {{{}}}", parts.join(", "))
		}
		other @ Value::Unknown(_) => format!("unknown {other:?}"),
	}
}

/// Copies a directory, without the files a library writes as it runs.
fn copy(from: &Path, to: &Path) {
	fs::create_dir_all(to).expect("makes the directory");
	for entry in fs::read_dir(from).expect("reads the directory") {
		let entry = entry.expect("an entry");
		let path = entry.path();
		let name = entry.file_name();
		if TRANSIENT.iter().any(|transient| name == *transient) {
			continue;
		}
		if path.is_dir() {
			copy(&path, &to.join(&name));
		} else {
			fs::copy(&path, to.join(&name)).expect("copies the file");
		}
	}
}

/// What the files of a library hold, by their paths relative to it: the bytes of each
/// file, but of each log file the rows of its tables, since SQLite may lay out the same
/// rows differently from one version to the next. The files [`UNCOMPARED`] are left out.
fn contents(directory: &Path) -> BTreeMap<String, Vec<u8>> {
	let mut contents = BTreeMap::new();
	let mut directories = vec![directory.to_owned()];
	while let Some(current) = directories.pop() {
		for entry in fs::read_dir(&current).expect("reads the directory") {
			let path = entry.expect("an entry").path();
			let name = path
				.file_name()
				.expect("a name")
				.to_string_lossy()
				.into_owned();
			if path.is_dir() {
				directories.push(path);
			} else if !TRANSIENT.contains(&name.as_str()) && !UNCOMPARED.contains(&name.as_str()) {
				let relative = path
					.strip_prefix(directory)
					.expect("inside")
					.to_string_lossy()
					.replace('\\', "/");
				let content = if name == "log.sqlite" {
					rows(&path).into_bytes()
				} else {
					fs::read(&path).expect("reads the file")
				};
				contents.insert(relative, content);
			}
		}
	}
	contents
}

/// The schema of a SQLite database and the rows of its tables, as text.
fn rows(path: &Path) -> String {
	use rusqlite::types::ValueRef;

	// Immutable, so that reading a fixture writes nothing beside it, not even the files of
	// SQLite's write-ahead log.
	let uri = format!("file:{}?immutable=1", uri_path(path));
	let connection = rusqlite::Connection::open_with_flags(
		uri,
		rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
	)
	.expect("opens the database");
	let mut text = String::new();
	for pragma in ["application_id", "user_version"] {
		let value: i64 = connection
			.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
			.expect("reads the pragma");
		let _ = writeln!(text, "{pragma} {value}");
	}
	let mut tables = connection
		.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table' ORDER BY name")
		.expect("lists the tables");
	let tables: Vec<(String, String)> = tables
		.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
		.expect("lists the tables")
		.map(|table| table.expect("a table"))
		.collect();
	for (table, schema) in tables {
		let _ = writeln!(text, "{schema}");
		let mut query = connection
			.prepare(&format!("SELECT * FROM \"{table}\""))
			.expect("reads the table");
		let columns = query.column_count();
		let mut lines: Vec<String> = query
			.query_map([], |row| {
				let mut line = String::new();
				for column in 0..columns {
					let _ = match row.get_ref(column)? {
						ValueRef::Null => write!(line, " null"),
						ValueRef::Integer(integer) => write!(line, " {integer}"),
						ValueRef::Real(real) => write!(line, " {real}"),
						ValueRef::Text(bytes) => {
							write!(line, " {:?}", String::from_utf8_lossy(bytes))
						}
						ValueRef::Blob(bytes) => write!(line, " x{}", hex(bytes)),
					};
				}
				Ok(line)
			})
			.expect("reads the rows")
			.map(|line| line.expect("a row"))
			.collect();
		lines.sort();
		for line in lines {
			let _ = writeln!(text, " {line}");
		}
	}
	text
}

/// A path as SQLite's filename URIs write it: with forward slashes, a slash before a
/// Windows drive, and the characters that URIs reserve escaped.
fn uri_path(path: &Path) -> String {
	let mut text = path.to_string_lossy().replace('\\', "/");
	if !text.starts_with('/') {
		text.insert(0, '/');
	}
	text.chars().fold(String::new(), |mut escaped, character| {
		match character {
			'%' | '?' | '#' | ' ' => {
				let _ = write!(escaped, "%{:02X}", u32::from(character));
			}
			_ => escaped.push(character),
		}
		escaped
	})
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().fold(String::new(), |mut hex, byte| {
		let _ = write!(hex, "{byte:02x}");
		hex
	})
}

/// The fixtures, each with its name: the directories in `fixtures/libraries`.
fn fixtures() -> Vec<(String, PathBuf)> {
	let directory = nibkeep_testkit::fixtures_directory().join("libraries");
	let mut fixtures: Vec<(String, PathBuf)> = fs::read_dir(&directory)
		.expect("the fixtures")
		.map(|entry| entry.expect("an entry").path())
		.filter(|path| path.is_dir())
		.map(|path| {
			let name = path
				.file_name()
				.expect("a name")
				.to_string_lossy()
				.into_owned();
			(name, path)
		})
		.collect();
	fixtures.sort();
	fixtures
}

/// Places a library by its path alone, as on a drive that reports no serial, IDs of folders
/// or computer: a fixture is written on one machine and read on others, as a copy, which is
/// otherwise never opened (technical design: copies of a replica).
fn by_path(library: &Path) {
	declare_drive(library, Drive::default(), None);
}

/// Opens a copy of a library, and describes it.
fn describe_copy(library: &Path, name: &str) -> String {
	let copied = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	copy(library, &copied);
	by_path(&copied);
	let mut opened =
		Library::open(&copied, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	describe(&mut opened)
}

/// One test, so that writing the draft fixture again never races reading it: first the
/// draft fixture is checked against what the code writes, or written again, then every
/// fixture is read.
#[test]
fn fixtures_read_as_described_and_the_draft_is_what_the_code_writes() {
	let draft = nibkeep_testkit::fixtures_directory()
		.join("libraries")
		.join(DRAFT);
	let generated = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "generated");
	by_path(&generated);
	generate(&generated);
	let described = describe_copy(&generated, "generated, opened");
	if std::env::var_os("NIBKEEP_BLESS").is_some_and(|bless| bless == "1") {
		let _ = fs::remove_dir_all(&draft);
		copy(&generated, &draft.join(LIBRARY));
		fs::write(draft.join(DESCRIPTION), &described).expect("writes the description");
	} else {
		let advice = "If the formats were meant to change, and they are still drafts, write \
		              the draft fixture again with NIBKEEP_BLESS=1";
		let expected = fs::read_to_string(draft.join(DESCRIPTION)).unwrap_or_default();
		assert!(
			described == expected,
			"the code writes a library that reads differently from the draft fixture. \
			 {advice}.\n{}",
			difference(&expected, &described)
		);
		let written = contents(&generated);
		let committed = contents(&draft.join(LIBRARY));
		let differing: Vec<&String> = written
			.keys()
			.chain(committed.keys())
			.filter(|path| written.get(*path) != committed.get(*path))
			.collect::<BTreeSet<_>>()
			.into_iter()
			.collect();
		assert!(
			differing.is_empty(),
			"the code writes files that differ from those of the draft fixture: {differing:?}. \
			 {advice}."
		);
	}

	let fixtures = fixtures();
	assert!(
		fixtures.iter().any(|(name, _)| name == DRAFT),
		"the draft fixture exists"
	);
	for (name, fixture) in fixtures {
		let expected = fs::read_to_string(fixture.join(DESCRIPTION)).expect("the description");
		let described = describe_copy(&fixture.join(LIBRARY), &format!("fixture {name}"));
		assert!(
			described == expected,
			"fixture {name} reads differently:\n{}",
			difference(&expected, &described)
		);
	}
}

/// Where two descriptions part: the first line that differs, with the lines after it.
fn difference(expected: &str, actual: &str) -> String {
	let expected: Vec<&str> = expected.lines().collect();
	let actual: Vec<&str> = actual.lines().collect();
	let first = expected
		.iter()
		.zip(&actual)
		.position(|(expected, actual)| expected != actual)
		.unwrap_or_else(|| expected.len().min(actual.len()));
	let mut text = format!(
		"from line {}, of {} expected and {} read:\n",
		first + 1,
		expected.len(),
		actual.len()
	);
	for line in expected.iter().skip(first).take(5) {
		let _ = writeln!(text, "- {line}");
	}
	for line in actual.iter().skip(first).take(5) {
		let _ = writeln!(text, "+ {line}");
	}
	text
}
