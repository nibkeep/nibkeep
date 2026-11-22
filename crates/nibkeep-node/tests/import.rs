//! Imports: an export brought into a library, adding what it lacks
//! (`docs/spec/export.md`, section 7).

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::{library, now, sum_again, today};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, ExportKind, Library, NodeError};
use nibkeep_std::{AUTHOR, BOOK, PEN_NAME, USED_BY};
use nibkeep_testkit::TestRng;
use serde_json::{Value as Json, json};

const NOTE: TypeName = TypeName::from_static("local.note");
const OWN_RECORDS: [&str; 4] = [
	"org.nibkeep.user",
	"org.nibkeep.device",
	"org.nibkeep.replica",
	"org.nibkeep.space",
];

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn name(text: &str) -> Value {
	Value::Name(Name::new(text))
}

/// A directory for an export, apart from any library's.
fn fresh(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, exported"))
		.join("export")
}

/// A library with a pen name, a book credited to it, a note with a title written twice and
/// a file, and a deleted note.
fn with_data(test: &str, rng: &mut TestRng) -> Library {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	library
		.define_type(
			personal,
			&TypeDef::new(NOTE, 1)
				.with_field(field("local.title"), FieldDef::new(ValueKind::Text))
				.with_field(field("local.scan"), FieldDef::new(ValueKind::Blob)),
			now(),
			rng,
		)
		.expect("defines");
	let corey = library
		.create_entity(
			personal,
			&[PEN_NAME],
			vec![(fields::NAME, name("James S. A. Corey"))],
			now(),
			rng,
		)
		.expect("creates");
	let abraham = library
		.create_entity(
			personal,
			&[nibkeep_governance::PERSON],
			vec![(fields::NAME, name("Daniel Abraham"))],
			now(),
			rng,
		)
		.expect("creates");
	library
		.edit_entity(
			personal,
			corey,
			vec![Edit::Add(USED_BY, Value::Reference(abraham))],
			&today(),
			now(),
			rng,
		)
		.expect("edits");
	library
		.create_entity(
			personal,
			&[BOOK],
			vec![
				(fields::NAME, name("Leviathan Wakes")),
				(AUTHOR, Value::Reference(corey)),
			],
			now(),
			rng,
		)
		.expect("creates");
	let hash = library
		.import_blob(personal, b"a scan of a list".as_slice(), now(), rng)
		.expect("imports");
	let note = library
		.create_entity(
			personal,
			&[NOTE],
			vec![
				(field("local.title"), Value::Text("Milk".to_owned())),
				(field("local.scan"), Value::Blob(hash)),
			],
			now(),
			rng,
		)
		.expect("creates");
	library
		.edit_entity(
			personal,
			note,
			vec![Edit::Set(
				field("local.title"),
				vec![Value::Text("Oat milk".to_owned())],
			)],
			&today(),
			now() + 60_000,
			rng,
		)
		.expect("edits");
	let gone = library
		.create_entity(
			personal,
			&[NOTE],
			vec![(field("local.title"), Value::Text("Bread".to_owned()))],
			now(),
			rng,
		)
		.expect("creates");
	library
		.edit_entity(personal, gone, vec![Edit::Delete], &today(), now(), rng)
		.expect("deletes");
	library
}

/// What the lines of an export hold, apart from when and by whom it was recorded: each
/// statement of each entity that is not one of NibKeep's own records, with whether it was
/// replaced.
fn held(directory: &Path) -> BTreeSet<(String, String, String, bool)> {
	let mut held = BTreeSet::new();
	for line in fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
	{
		let line: Json = serde_json::from_str(line).expect("JSON");
		let types = line["types"].as_array().expect("types");
		if OWN_RECORDS
			.iter()
			.any(|own| types.contains(&Json::from(*own)))
		{
			continue;
		}
		for field in line["fields"].as_array().expect("fields") {
			for value in field["values"].as_array().expect("values") {
				let mut kept = value.clone();
				let object = kept.as_object_mut().expect("an object");
				let replaced = object
					.remove("replaced")
					.is_some_and(|replaced| !replaced.is_null());
				for key in ["change", "recorded"] {
					object.remove(key);
				}
				held.insert((
					line["id"].as_str().expect("an ID").to_owned(),
					field["name"].as_str().expect("a name").to_owned(),
					kept.to_string(),
					replaced,
				));
			}
		}
	}
	held
}

/// The library's export, as [`held`] reads it.
fn exported(library: &Library, name: &str) -> BTreeSet<(String, String, String, bool)> {
	let directory = fresh(name);
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	held(&directory)
}

#[test]
fn an_export_imported_loses_nothing() {
	let mut rng = TestRng::new("import");
	let from = with_data("import, from", &mut rng);
	let directory = fresh("import");
	from.export(from.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let (_, mut into) = library("import, into", &mut rng);
	let personal = into.personal_space();
	let before = exported(&into, "import, into before");
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	assert_eq!(imported.files, 1);
	assert!(imported.added > 0);
	assert_eq!(imported.replaced, 1, "the first title");
	assert!(
		imported.skipped > 0,
		"the records of users, devices, replicas and spaces"
	);
	// The library holds everything the export held, and what it held before.
	let after = exported(&into, "import, into after");
	let wanted = held(&directory);
	assert!(
		wanted.is_subset(&after),
		"{:?}",
		wanted.difference(&after).collect::<Vec<_>>()
	);
	assert!(before.is_subset(&after));
	// It shows the imported entities, named and found as they were.
	let book = into
		.find_entity(personal, "Leviathan Wakes")
		.expect("the book");
	let links = into
		.links_to(
			personal,
			into.find_entity(personal, "James S. A. Corey")
				.expect("the pen name"),
			common::this_day(),
			10,
		)
		.expect("a space");
	assert_eq!(links[0].entities[0].id, book);
	assert!(into.find_entity(personal, "Bread").is_err(), "deleted");
	// Its own records are its own.
	let status = into.status().expect("a status");
	assert_eq!(status.spaces[0].name.as_deref(), Some("Personal"));
	assert_eq!(status.user.name.as_deref(), Some("Ada"));
	assert!(into.verify(now() + 3_600_000).expect("verifies").is_sound());
}

#[test]
fn an_export_that_tells_who_and_when_on_every_value_imports_the_same() {
	let mut rng = TestRng::new("import older");
	let from = with_data("import older, from", &mut rng);
	let directory = fresh("import older");
	from.export(from.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let wanted = held(&directory);
	// Each value as exports wrote it before there was a list of changes: when and by whom it
	// was recorded, and every key, empty or not.
	let user = from.status().expect("a status").user.id.to_string();
	let mut older = String::new();
	for line in fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
	{
		let mut line: Json = serde_json::from_str(line).expect("JSON");
		for field in line["fields"].as_array_mut().expect("fields") {
			for value in field["values"].as_array_mut().expect("values") {
				let object = value.as_object_mut().expect("an object");
				object.remove("change");
				object.insert("recorded".to_owned(), json!("2026-10-12T10:00:00.000Z"));
				object.insert("restated".to_owned(), Json::Null);
				object.insert("author".to_owned(), json!({ "id": user, "name": "Ada" }));
				object.entry("qualifiers").or_insert_with(|| json!({}));
				object.entry("replaced").or_insert(Json::Null);
			}
		}
		older.push_str(&line.to_string());
		older.push('\n');
	}
	fs::write(directory.join("entities.jsonl"), older).expect("writes");
	fs::remove_file(directory.join("changes.jsonl")).expect("removes");
	let sums: String = fs::read_to_string(directory.join("SHA256SUMS"))
		.expect("sums")
		.lines()
		.filter(|line| !line.ends_with("  changes.jsonl"))
		.flat_map(|line| [line, "\n"])
		.collect();
	fs::write(directory.join("SHA256SUMS"), sums).expect("writes");
	sum_again(&directory, "entities.jsonl");

	let (_, mut into) = library("import older, into", &mut rng);
	let personal = into.personal_space();
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	assert_eq!(imported.replaced, 1, "the first title");
	let after = exported(&into, "import older, into after");
	assert!(
		wanted.is_subset(&after),
		"{:?}",
		wanted.difference(&after).collect::<Vec<_>>()
	);
}

#[test]
fn importing_again_adds_nothing() {
	let mut rng = TestRng::new("import again");
	let from = with_data("import again, from", &mut rng);
	let directory = fresh("import again");
	from.export(from.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let (_, mut into) = library("import again, into", &mut rng);
	let personal = into.personal_space();
	into.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	let changes = into.status().expect("a status").spaces[0].changes;
	let again = into
		.import(personal, &directory, now() + 7_200_000, &mut rng)
		.expect("imports again");
	assert_eq!((again.added, again.replaced, again.files), (0, 0, 0));
	assert_eq!(into.status().expect("a status").spaces[0].changes, changes);
	// Nor into the library it came from.
	let mut from = from;
	let personal = from.personal_space();
	let back = from
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports back");
	assert_eq!((back.added, back.replaced, back.files), (0, 0, 0));
}

#[test]
fn an_export_of_current_values_imports_them() {
	let mut rng = TestRng::new("import current");
	let from = with_data("import current, from", &mut rng);
	let directory = fresh("import current");
	from.export(from.personal_space(), &directory, ExportKind::Current)
		.expect("exports");
	let (_, mut into) = library("import current, into", &mut rng);
	let personal = into.personal_space();
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	assert_eq!(imported.replaced, 0);
	let note = into.find_entity(personal, "Oat milk").expect("the note");
	let shown = into
		.entity(personal, note, common::this_day())
		.expect("a space")
		.expect("the note");
	assert!(
		shown
			.fields
			.iter()
			.any(|shown| shown.field == field("local.title"))
	);
	assert!(into.find_entity(personal, "Bread").is_err(), "not exported");
	assert_eq!(imported.person, None, "no user's records");
}

#[test]
fn a_damaged_export_is_refused_whole() {
	let mut rng = TestRng::new("import damaged");
	let from = with_data("import damaged, from", &mut rng);
	let directory = fresh("import damaged");
	from.export(from.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let lines = fs::read_to_string(directory.join("entities.jsonl")).expect("entities");
	fs::write(
		directory.join("entities.jsonl"),
		lines.replacen("Leviathan Wakes", "Leviathan Sleeps", 1),
	)
	.expect("writes");
	let (_, mut into) = library("import damaged, into", &mut rng);
	let personal = into.personal_space();
	let changes = into.status().expect("a status").spaces[0].changes;
	assert!(matches!(
		into.import(personal, &directory, now(), &mut rng),
		Err(NodeError::ExportDamaged(_))
	));
	assert_eq!(into.status().expect("a status").spaces[0].changes, changes);
}

#[test]
fn values_of_every_kind_survive_an_import() {
	use nibkeep_model::{Decimal, Quantity};
	let mut rng = TestRng::new("import kinds");
	let (_, mut from) = library("import kinds, from", &mut rng);
	let personal = from.personal_space();
	let kinds = [
		(
			"local.text",
			ValueKind::Text,
			Value::Text("Milk".to_owned()),
		),
		("local.small", ValueKind::Integer, Value::Integer(-3)),
		("local.big", ValueKind::Integer, Value::Integer(i64::MIN)),
		("local.yes", ValueKind::Boolean, Value::Boolean(true)),
		(
			"local.when",
			ValueKind::Date,
			Value::Date("196X/2001-03-04".parse().expect("a date")),
		),
		(
			"local.price",
			ValueKind::Decimal,
			Value::Decimal("-0.050".parse().expect("a decimal")),
		),
		(
			"local.many",
			ValueKind::Decimal,
			Value::Decimal(Decimal::new(15, 2).expect("a decimal")),
		),
		(
			"local.weight",
			ValueKind::Quantity,
			Value::Quantity(
				Quantity::new(Decimal::new(1215, -2).expect("a decimal"), "[lb_av]")
					.expect("a quantity"),
			),
		),
		(
			"local.person",
			ValueKind::Name,
			Value::Name(
				Name::new("Анна Иванова")
					.with_part("given", "Анна")
					.with_part("family", "Иванова"),
			),
		),
	];
	let mut note = TypeDef::new(NOTE, 1);
	for (name, kind, _) in &kinds {
		note = note.with_field(field(name), FieldDef::new(*kind));
	}
	from.define_type(personal, &note, now(), &mut rng)
		.expect("defines");
	let entity = from
		.create_entity(
			personal,
			&[NOTE],
			kinds
				.iter()
				.map(|(name, _, value)| (field(name), value.clone()))
				.collect(),
			now(),
			&mut rng,
		)
		.expect("creates");
	let directory = fresh("import kinds");
	from.export(personal, &directory, ExportKind::Full)
		.expect("exports");
	let (_, mut into) = library("import kinds, into", &mut rng);
	let into_personal = into.personal_space();
	into.import(into_personal, &directory, now(), &mut rng)
		.expect("imports");
	let view = into
		.entity(into_personal, entity, common::this_day())
		.expect("a space")
		.expect("the note");
	for (name, _, value) in &kinds {
		let shown = view
			.fields
			.iter()
			.find(|shown| shown.field == field(name))
			.unwrap_or_else(|| panic!("{name}"));
		assert_eq!(&shown.values[0].value, value, "{name}");
	}
}

#[test]
fn an_export_made_before_a_purge_of_its_space_is_refused() {
	let mut rng = TestRng::new("import after purge");
	let mut library = with_data("import after purge", &mut rng);
	let personal = library.personal_space();
	let directory = fresh("import after purge");
	library
		.export(personal, &directory, ExportKind::Full)
		.expect("exports");
	let note = library.find_entity(personal, "Oat milk").expect("the note");
	library
		.purge(
			personal,
			note,
			&field("local.title"),
			Some(&Value::Text("Milk".to_owned())),
			now() + 3_600_000,
			&mut rng,
		)
		.expect("purges");
	let error = library
		.import(personal, &directory, now() + 7_200_000, &mut rng)
		.expect_err("refused");
	assert!(error.to_string().contains("purged values since"), "{error}");
	// An export made after it imports, adding nothing.
	let after = fresh("import after purge, again");
	library
		.export(personal, &after, ExportKind::Full)
		.expect("exports");
	let back = library
		.import(personal, &after, now() + 7_200_000, &mut rng)
		.expect("imports");
	assert_eq!((back.added, back.replaced), (0, 0));
}

#[test]
fn exports_that_do_not_hold_together_are_refused() {
	let mut rng = TestRng::new("import refused");
	let from = with_data("import refused, from", &mut rng);
	let (_, mut into) = library("import refused, into", &mut rng);
	let personal = into.personal_space();
	let refused = |into: &mut Library, directory: &Path, rng: &mut TestRng| {
		let error = into
			.import(personal, directory, now(), rng)
			.expect_err("refused");
		assert!(matches!(error, NodeError::ExportDamaged(_)), "{error}");
		error.to_string()
	};
	// A file that no sum lists.
	let unlisted = fresh("import refused, unlisted");
	from.export(from.personal_space(), &unlisted, ExportKind::Full)
		.expect("exports");
	fs::write(unlisted.join("files").join("blake3").join("00"), "stray").expect("writes");
	assert!(refused(&mut into, &unlisted, &mut rng).contains("files/blake3/00"));
	// A name without its text, with the sums written to match.
	let empty = fresh("import refused, empty name");
	from.export(from.personal_space(), &empty, ExportKind::Full)
		.expect("exports");
	let lines = fs::read_to_string(empty.join("entities.jsonl")).expect("entities");
	let damaged = lines.replacen("{\"full\":\"Leviathan Wakes\"}", "{\"full\":\"\"}", 1);
	assert_ne!(damaged, lines);
	fs::write(empty.join("entities.jsonl"), &damaged).expect("writes");
	rewrite_sums(&empty);
	assert!(refused(&mut into, &empty, &mut rng).contains("line"));
	// A record of a user whose line lists no types.
	let hidden = fresh("import refused, hidden user");
	from.export(from.personal_space(), &hidden, ExportKind::Full)
		.expect("exports");
	let lines = fs::read_to_string(hidden.join("entities.jsonl")).expect("entities");
	let rewritten = lines.replace("\"types\":[\"org.nibkeep.user\"]", "\"types\":[]");
	assert_ne!(rewritten, lines);
	fs::write(hidden.join("entities.jsonl"), rewritten).expect("writes");
	rewrite_sums(&hidden);
	let imported = into
		.import(personal, &hidden, now(), &mut rng)
		.expect("imports");
	assert!(imported.skipped >= 4, "the user, device, replica and space");
	assert_eq!(
		into.status().expect("a status").user.name.as_deref(),
		Some("Ada")
	);
}

/// Writes the sums of an export again, as someone who changed it might.
fn rewrite_sums(directory: &Path) {
	use sha2::{Digest, Sha256};
	use std::fmt::Write as _;
	let mut sums = String::new();
	for line in fs::read_to_string(directory.join("SHA256SUMS"))
		.expect("sums")
		.lines()
	{
		let (_, path) = line.split_once("  ").expect("a sum and a path");
		let bytes = fs::read(directory.join(path)).expect("a file");
		for byte in Sha256::digest(&bytes) {
			let _ = write!(sums, "{byte:02x}");
		}
		let _ = writeln!(sums, "  {path}");
	}
	fs::write(directory.join("SHA256SUMS"), sums).expect("writes");
}

const DEFINITIONS: [&str; 2] = [
	"org.nibkeep.type-definition",
	"org.nibkeep.field-definition",
];

/// The library's export, in a directory of its own.
fn export_of(library: &Library, name: &str) -> PathBuf {
	let directory = fresh(name);
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	directory
}

/// The lines of an export that define types and fields.
fn definition_lines(directory: &Path) -> Vec<Json> {
	fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.map(|line| serde_json::from_str::<Json>(line).expect("JSON"))
		.filter(|line| {
			line["types"]
				.as_array()
				.expect("types")
				.iter()
				.any(|kind| DEFINITIONS.contains(&kind.as_str().unwrap_or_default()))
		})
		.collect()
}

/// How many entities of an export define types, and how many define fields.
fn definitions(directory: &Path) -> (usize, usize) {
	let lines = definition_lines(directory);
	let types = lines
		.iter()
		.filter(|line| line["types"][0] == DEFINITIONS[0])
		.count();
	(types, lines.len() - types)
}

/// Gives every entity and statement of an export's definitions a random ID, as programs did
/// before definitions took derived IDs.
fn with_random_definitions(directory: &Path, rng: &mut TestRng) {
	let mut new_ids = std::collections::BTreeMap::new();
	for line in definition_lines(directory) {
		new_ids.insert(
			line["id"].as_str().expect("an ID").to_owned(),
			rng.entity_id().to_string(),
		);
		for field in line["fields"].as_array().expect("fields") {
			for value in field["values"].as_array().expect("values") {
				new_ids.insert(
					value["statement"].as_str().expect("an ID").to_owned(),
					rng.statement_id().to_string(),
				);
			}
		}
	}
	let mut entities = fs::read_to_string(directory.join("entities.jsonl")).expect("entities");
	for (old, new) in &new_ids {
		entities = entities.replace(old.as_str(), new);
	}
	fs::write(directory.join("entities.jsonl"), entities).expect("writes");
	rewrite_sums(directory);
}

#[test]
fn a_library_made_anew_from_its_export_defines_its_types_once() {
	let mut rng = TestRng::new("import definitions");
	let from = with_data("import definitions, from", &mut rng);
	let directory = export_of(&from, "import definitions");
	let (_, mut into) = library("import definitions, into", &mut rng);
	let personal = into.personal_space();
	into.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	// The standard vocabulary that `nib init` installed is the export's, statement for
	// statement, so the library defines each type once, as the export does.
	let anew = export_of(&into, "import definitions, anew");
	assert_eq!(definitions(&anew), definitions(&directory));
	assert_eq!(
		definition_lines(&anew).len(),
		definition_lines(&directory).len()
	);
}

#[test]
fn definitions_that_took_random_ids_add_only_what_the_library_lacks() {
	let mut rng = TestRng::new("import random definitions");
	let mut from = with_data("import random definitions, from", &mut rng);
	let personal = from.personal_space();
	let swedish = nibkeep_model::Tag::parse("sv").expect("a tag");
	from.define_type(
		personal,
		&TypeDef::new(BOOK, 1).with_label(Some(&swedish), "Bok"),
		now(),
		&mut rng,
	)
	.expect("labels the book");
	let directory = export_of(&from, "import random definitions");
	with_random_definitions(&directory, &mut rng);
	let (_, mut into) = library("import random definitions, into", &mut rng);
	let personal = into.personal_space();
	let before = definitions(&export_of(&into, "import random definitions, before"));
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	nibkeep_testkit::assert_empty!(imported.types_left_out);
	// The note's definition, which the library lacked, comes in as it is; the standard types
	// add to the library's own the label it lacked, and nothing more.
	let after = definitions(&export_of(&into, "import random definitions, after"));
	assert_eq!(
		after,
		(before.0 + 1, before.1 + 2),
		"the note, its title and scan"
	);
	let types = into.types(personal).expect("types");
	assert_eq!(
		types
			.get(&BOOK)
			.expect("the book")
			.labels()
			.get(Some(&swedish)),
		Some("Bok")
	);
	assert!(types.get(&NOTE).is_some());
	// Importing it again adds nothing.
	let again = into
		.import(personal, &directory, now() + 7_200_000, &mut rng)
		.expect("imports again");
	assert_eq!((again.added, again.replaced), (0, 0));
}

#[test]
fn a_definition_the_library_cannot_take_is_left_out() {
	let mut rng = TestRng::new("import other definitions");
	let from = with_data("import other definitions, from", &mut rng);
	let directory = export_of(&from, "import other definitions");
	with_random_definitions(&directory, &mut rng);
	// The library defines the note's title as a number.
	let (_, mut into) = library("import other definitions, into", &mut rng);
	let personal = into.personal_space();
	into.define_type(
		personal,
		&TypeDef::new(NOTE, 1).with_field(field("local.title"), FieldDef::new(ValueKind::Integer)),
		now(),
		&mut rng,
	)
	.expect("defines");
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	assert_eq!(imported.types_left_out, [NOTE]);
	let types = into.types(personal).expect("types");
	let note = types.get(&NOTE).expect("the note");
	assert_eq!(
		note.field(&field("local.title")).map(FieldDef::kind),
		Some(ValueKind::Integer)
	);
	assert!(note.field(&field("local.scan")).is_none());
	assert!(
		imported.added > 0,
		"its values, which are kept whatever their kind"
	);
}

/// The library's user and the person linked to the user, the only one.
fn own_person(library: &Library) -> (EntityId, EntityId) {
	let user = library.status().expect("a status").user.id;
	let linked = common::values(library, user, &nibkeep_governance::LINKED_PERSON);
	let [Value::Reference(person)] = linked.as_slice() else {
		panic!("one person: {linked:?}");
	};
	(user, *person)
}

#[test]
fn a_library_made_anew_takes_the_person_of_the_exports_user() {
	let mut rng = TestRng::new("import person");
	let mut from = with_data("import person, from", &mut rng);
	let personal = from.personal_space();
	let (_, theirs) = own_person(&from);
	from.edit_entity(
		personal,
		theirs,
		vec![Edit::Set(fields::NAME, vec![name("Ada Lovelace")])],
		&today(),
		now(),
		&mut rng,
	)
	.expect("renames");
	let directory = export_of(&from, "import person");
	let (_, mut into) = library("import person, into", &mut rng);
	let personal = into.personal_space();
	let (user, made) = own_person(&into);
	let imported = into
		.import(personal, &directory, now() + 3_600_000, &mut rng)
		.expect("imports");
	// The user is the export's person now, and history names them so.
	let person = imported.person.expect("the export's person");
	assert_eq!(
		(person.id, person.name.as_deref()),
		(theirs, Some("Ada Lovelace"))
	);
	assert_eq!(own_person(&into), (user, theirs));
	assert_eq!(
		into.status().expect("a status").user.name.as_deref(),
		Some("Ada Lovelace")
	);
	// The person that the new library made is deleted, so the export's is the only one.
	let deleted = into.deleted(personal).expect("a space");
	assert!(deleted.iter().any(|listed| listed.id == made));
	assert_eq!(
		into.find_entity(personal, "Ada Lovelace")
			.expect("the person"),
		theirs
	);
	assert!(into.verify(now() + 3_600_000).expect("verifies").is_sound());
	// Importing again changes nothing.
	let changes = into.status().expect("a status").spaces[0].changes;
	let again = into
		.import(personal, &directory, now() + 7_200_000, &mut rng)
		.expect("imports again");
	assert_eq!(again.person, None);
	assert_eq!(into.status().expect("a status").spaces[0].changes, changes);
}

#[test]
fn a_person_that_the_user_described_or_refers_to_stays_theirs() {
	let mut rng = TestRng::new("import own person");
	let from = with_data("import own person, from", &mut rng);
	let (_, theirs) = own_person(&from);
	let directory = export_of(&from, "import own person");
	// A user who gave their person another name, and one whom a pen name refers to.
	let (_, mut named) = library("import own person, named", &mut rng);
	let (_, renamed) = own_person(&named);
	let personal = named.personal_space();
	named
		.edit_entity(
			personal,
			renamed,
			vec![Edit::Set(fields::NAME, vec![name("Ada King")])],
			&today(),
			now(),
			&mut rng,
		)
		.expect("renames");
	let (_, mut referred) = library("import own person, referred", &mut rng);
	let (_, used) = own_person(&referred);
	let personal = referred.personal_space();
	referred
		.create_entity(
			personal,
			&[PEN_NAME],
			vec![
				(fields::NAME, name("A. A. L.")),
				(USED_BY, Value::Reference(used)),
			],
			now(),
			&mut rng,
		)
		.expect("creates");
	for (library, mine) in [(&mut named, renamed), (&mut referred, used)] {
		let personal = library.personal_space();
		let imported = library
			.import(personal, &directory, now() + 3_600_000, &mut rng)
			.expect("imports");
		assert_eq!(imported.person, None);
		assert_eq!(own_person(library).1, mine);
		assert!(
			!library
				.deleted(personal)
				.expect("a space")
				.iter()
				.any(|listed| listed.id == mine)
		);
		// The export's person comes in as a person of its own.
		assert!(
			library
				.entity(personal, theirs, common::this_day())
				.expect("a space")
				.is_some()
		);
	}
}
