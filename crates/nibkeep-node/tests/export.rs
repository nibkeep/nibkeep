//! Exports: a space in ordinary files, JSON and SHA-256 sums (`docs/spec/export.md`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{library, now, sum_again, today};
use nibkeep_base::EntityId;
use nibkeep_model::{
	Decimal, FieldDef, FieldName, Name, Quantity, TypeDef, TypeName, Value, ValueKind, fields,
};
use nibkeep_node::{Edit, ExportKind, ExportProblem, Library, NodeError, Unlocker};
use nibkeep_testkit::TestRng;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

const NOTE: TypeName = TypeName::from_static("local.note");

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

/// A directory for an export that does not exist yet, apart from any library's.
fn fresh(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, exported"))
		.join("export")
}

/// A library with a note that has a value of every kind, a title written twice, and a file.
fn with_a_note(test: &str, rng: &mut TestRng) -> (Library, EntityId, Vec<u8>) {
	let (_, mut library) = library(test, rng);
	let personal = library.personal_space();
	let note = TypeDef::new(NOTE, 1)
		.with_field(field("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(field("local.tag"), FieldDef::new(ValueKind::Text).many())
		.with_field(field("local.count"), FieldDef::new(ValueKind::Integer))
		.with_field(field("local.big"), FieldDef::new(ValueKind::Integer))
		.with_field(field("local.done"), FieldDef::new(ValueKind::Boolean))
		.with_field(field("local.when"), FieldDef::new(ValueKind::Date))
		.with_field(field("local.price"), FieldDef::new(ValueKind::Decimal))
		.with_field(
			field("local.weight"),
			FieldDef::new(ValueKind::Quantity).with_unit("kg"),
		)
		.with_field(field("local.scan"), FieldDef::new(ValueKind::Blob))
		.with_field(field("local.about"), FieldDef::new(ValueKind::Reference))
		.with_field(field("local.person"), FieldDef::new(ValueKind::Name));
	library
		.define_type(personal, &note, now(), rng)
		.expect("defines");
	let content = b"a scan of a shopping list".to_vec();
	let hash = library
		.import_blob(personal, &mut content.as_slice(), now(), rng)
		.expect("imports");
	let user = library.status().expect("a status").user.id;
	let entity = library
		.create_entity(
			personal,
			&[NOTE],
			vec![
				(field("local.title"), Value::Text("Milk".to_owned())),
				(field("local.tag"), Value::Text("shop".to_owned())),
				(field("local.count"), Value::Integer(3)),
				(field("local.big"), Value::Integer(1 << 60)),
				(field("local.done"), Value::Boolean(false)),
				(
					field("local.when"),
					Value::Date("2026-10~".parse().expect("a date")),
				),
				(
					field("local.price"),
					Value::Decimal("12.50".parse().expect("a decimal")),
				),
				(
					field("local.weight"),
					Value::Quantity(
						Quantity::new(Decimal::new(723, -1).expect("a decimal"), "kg")
							.expect("a quantity"),
					),
				),
				(field("local.scan"), Value::Blob(hash)),
				(field("local.about"), Value::Reference(user)),
				(
					field("local.person"),
					Value::Name(Name::new("Ada Lovelace").with_part("family", "Lovelace")),
				),
			],
			now(),
			rng,
		)
		.expect("creates");
	library
		.edit_entity(
			personal,
			entity,
			vec![Edit::Set(
				field("local.title"),
				vec![Value::Text("Oat milk".to_owned())],
			)],
			&today(),
			now() + 60_000,
			rng,
		)
		.expect("edits");
	(library, entity, content)
}

/// The lines of the export's entities, parsed.
fn entities(directory: &Path) -> Vec<Json> {
	fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.map(|line| serde_json::from_str(line).expect("JSON"))
		.collect()
}

/// The lines of the export's changes, parsed.
fn changes(directory: &Path) -> Vec<Json> {
	fs::read_to_string(directory.join("changes.jsonl"))
		.expect("changes")
		.lines()
		.map(|line| serde_json::from_str(line).expect("JSON"))
		.collect()
}

/// The number of the change that a value names.
fn change_of(value: &Json) -> usize {
	value["change"]
		.as_u64()
		.and_then(|number| usize::try_from(number).ok())
		.expect("a change's number")
}

/// The values of a field of an entity's line.
fn values<'a>(line: &'a Json, name: &str) -> &'a [Json] {
	line["fields"]
		.as_array()
		.expect("fields")
		.iter()
		.find(|field| field["name"] == name)
		.and_then(|field| field["values"].as_array())
		.map_or(&[], Vec::as_slice)
}

/// Every file under a directory: its path relative to it, with `/`, and its path, ordered.
fn walk(directory: &Path) -> Vec<(String, PathBuf)> {
	let mut found = Vec::new();
	let mut to_visit = vec![directory.to_path_buf()];
	while let Some(next) = to_visit.pop() {
		for entry in fs::read_dir(&next).expect("a directory") {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				to_visit.push(path);
			} else {
				let relative = path
					.strip_prefix(directory)
					.expect("inside")
					.components()
					.map(|part| part.as_os_str().to_string_lossy().into_owned())
					.collect::<Vec<_>>()
					.join("/");
				found.push((relative, path));
			}
		}
	}
	found.sort();
	found
}

/// The path of every file under a directory, relative to it, with `/`, ordered. Nothing is
/// read, so the files of an open library can be listed: on Windows, the locks that it holds
/// on them keep them from being read through any other handle, even in its own process.
fn paths(directory: &Path) -> Vec<String> {
	walk(directory)
		.into_iter()
		.map(|(relative, _)| relative)
		.collect()
}

/// Every file under a directory, by its path relative to it, with `/`, ordered.
fn files(directory: &Path) -> Vec<(String, Vec<u8>)> {
	walk(directory)
		.into_iter()
		.map(|(relative, path)| (relative, fs::read(path).expect("a file")))
		.collect()
}

#[test]
fn a_full_export_holds_every_statement_with_its_history() {
	let mut rng = TestRng::new("full export");
	let (library, note, _) = with_a_note("full export", &mut rng);
	let directory = fresh("full export");
	let exported = library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	assert_eq!(exported.files, 1);
	nibkeep_testkit::assert_empty!(exported.missing_files);

	let manifest: Json =
		serde_json::from_slice(&fs::read(directory.join("manifest.json")).expect("a manifest"))
			.expect("JSON");
	assert_eq!(manifest["format"], "nibkeep-export");
	assert_eq!(manifest["version"], 1);
	assert_eq!(manifest["history"], true);
	assert_eq!(
		manifest["space"]["id"],
		library.personal_space().to_string()
	);
	assert_eq!(manifest["space"]["name"], "Personal");
	assert_eq!(manifest["as_of"], "2026-10-12T10:01:00.000Z");
	assert_eq!(manifest["files"], 1);
	assert_eq!(manifest["missing_files"], json!([]));
	assert_eq!(manifest["secrets"], "left out");

	let lines = entities(&directory);
	assert_eq!(manifest["entities"], lines.len());
	assert_eq!(exported.entities, lines.len());
	let ids: Vec<&str> = lines
		.iter()
		.map(|line| line["id"].as_str().expect("an ID"))
		.collect();
	let mut sorted = ids.clone();
	sorted.sort_unstable();
	assert_eq!(ids, sorted, "ordered by ID");
	// The types, and the records of who uses the library, are entities too.
	for kind in [
		"org.nibkeep.type-definition",
		"org.nibkeep.user",
		"org.nibkeep.replica",
	] {
		assert!(
			lines.iter().any(|line| line["types"]
				.as_array()
				.expect("types")
				.contains(&json!(kind))),
			"{kind}"
		);
	}

	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	assert_eq!(line["types"], json!(["local.note"]));
	assert_eq!(line["name"], "Oat milk");
	let names: Vec<&str> = line["fields"]
		.as_array()
		.expect("fields")
		.iter()
		.map(|field| field["name"].as_str().expect("a name"))
		.collect();
	let mut sorted_names = names.clone();
	sorted_names.sort_unstable();
	assert_eq!(names, sorted_names, "ordered by name");
	assert!(names.contains(&"org.nibkeep.type"));

	// The title's history: the first value replaced by the second, each recorded by a change
	// that the list of changes tells of.
	let changes = changes(&directory);
	let title = values(line, "local.title");
	assert_eq!(title.len(), 2);
	assert_eq!(title[0]["value"], "Milk");
	assert!(title[0]["statement"].is_string());
	let first = &changes[change_of(&title[0])];
	assert_eq!(first["recorded"], "2026-10-12T10:00:00.000Z");
	assert_eq!(title[0]["replaced"], "2026-10-12T10:01:00.000Z");
	for key in ["recorded", "restated", "author", "qualifiers"] {
		assert!(title[0].get(key).is_none(), "{key}");
	}
	assert_eq!(title[1]["value"], "Oat milk");
	assert!(title[1].get("replaced").is_none(), "current");
	assert_eq!(
		changes[change_of(&title[1])]["recorded"],
		"2026-10-12T10:01:00.000Z"
	);

	// Who made a change: this replica, its user, and the person linked to the user, by ID.
	let status = library.status().expect("a status");
	assert_eq!(first["replica"], status.replica.id.to_string());
	assert_eq!(first["user"], status.user.id.to_string());
	let user = lines
		.iter()
		.find(|line| line["id"] == status.user.id.to_string())
		.expect("the user's line");
	assert_eq!(
		first["person"],
		values(user, "org.nibkeep.linked-person")[0]["value"]
	);
	assert!(first.get("ai_agent").is_none());
	// Names are not repeated: the lines of the person and the user give them.
	let listed = fs::read_to_string(directory.join("changes.jsonl")).expect("changes");
	assert!(!listed.contains("Ada"), "{listed}");
}

#[test]
fn changes_are_numbered_as_the_entities_first_name_them() {
	let mut rng = TestRng::new("export changes");
	let (library, _, _) = with_a_note("export changes", &mut rng);
	let directory = fresh("export changes");
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let mut named = Vec::new();
	for line in entities(&directory) {
		for field in line["fields"].as_array().expect("fields") {
			for value in field["values"].as_array().expect("values") {
				let number = change_of(value);
				if !named.contains(&number) {
					named.push(number);
				}
				// Keys without anything to say are left out.
				for key in ["qualifiers", "replaced", "recorded"] {
					assert!(
						value.get(key).is_none_or(|held| !held.is_null()
							&& held.as_object().is_none_or(|object| !object.is_empty())),
						"{key} in {value}"
					);
				}
			}
		}
	}
	let listed = changes(&directory);
	assert_eq!(
		named,
		(0..listed.len()).collect::<Vec<_>>(),
		"every change named, in order"
	);
	for change in &listed {
		assert!(change["recorded"].is_string(), "{change}");
	}
}

#[test]
fn a_change_by_an_ai_agent_names_its_replica_and_the_user() {
	let mut rng = TestRng::new("export ai agent");
	let (directory, mut library) = library("export ai agent", &mut rng);
	let personal = library.personal_space();
	library
		.define_type(personal, &TypeDef::new(NOTE, 1), now(), &mut rng)
		.expect("defines");
	library.enable_ai_agents(&mut rng).expect("enables");
	let user = library.status().expect("a status").user.id;
	drop(library);
	let mut agent =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	agent
		.act_as_ai_agent("Claude Code", now() + 1_000, &mut rng)
		.expect("acts");
	let note = agent
		.create_entity(
			personal,
			&[NOTE],
			vec![(fields::NAME, Value::Name(Name::new("Shopping")))],
			now() + 2_000,
			&mut rng,
		)
		.expect("creates");
	let export = fresh("export ai agent");
	agent
		.export(personal, &export, ExportKind::Full)
		.expect("exports");
	let lines = entities(&export);
	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	let change = &changes(&export)[change_of(&values(line, "org.nibkeep.name")[0])];
	assert_eq!(change["ai_agent"], true);
	assert_eq!(change["user"], user.to_string());
	let replica = lines
		.iter()
		.find(|line| line["id"] == change["replica"])
		.expect("the AI agent's replica");
	assert_eq!(replica["name"], "Claude Code");
	assert_eq!(
		values(replica, "org.nibkeep.owner")[0]["value"],
		user.to_string()
	);
}

#[test]
fn a_value_that_a_purge_stated_again_keeps_when_it_was_first_recorded() {
	let mut rng = TestRng::new("export restated");
	let (mut library, note, _) = with_a_note("export restated", &mut rng);
	let personal = library.personal_space();
	library
		.purge(
			personal,
			note,
			&field("local.tag"),
			None,
			now() + 120_000,
			&mut rng,
		)
		.expect("purges");
	let directory = fresh("export restated");
	library
		.export(personal, &directory, ExportKind::Full)
		.expect("exports");
	let lines = entities(&directory);
	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	assert!(values(line, "local.tag").is_empty(), "purged");
	// The count came in the change that the purge took apart: the purge stated it again.
	let count = &values(line, "local.count")[0];
	assert_eq!(count["recorded"], "2026-10-12T10:00:00.000Z");
	let purge = &changes(&directory)[change_of(count)];
	assert_eq!(purge["recorded"], "2026-10-12T10:02:00.000Z");
	assert_eq!(
		purge["user"],
		library.status().expect("a status").user.id.to_string()
	);
	nibkeep_testkit::assert_empty!(library.verify_export(&directory).expect("checks").problems);
}

#[test]
fn an_export_of_a_space_that_has_not_changed_is_the_same() {
	let mut rng = TestRng::new("same export");
	let (library, _, _) = with_a_note("same export", &mut rng);
	let personal = library.personal_space();
	let first = fresh("same export, first");
	let second = fresh("same export, second");
	for directory in [&first, &second] {
		library
			.export(personal, directory, ExportKind::Full)
			.expect("exports");
	}
	assert_eq!(files(&first), files(&second));
}

#[test]
fn an_export_of_current_values_leaves_out_history_and_records() {
	let mut rng = TestRng::new("current export");
	let (mut library, note, _) = with_a_note("current export", &mut rng);
	let personal = library.personal_space();
	let gone = library
		.create_entity(
			personal,
			&[NOTE],
			vec![(field("local.title"), Value::Text("Bread".to_owned()))],
			now(),
			&mut rng,
		)
		.expect("creates");
	library
		.edit_entity(
			personal,
			gone,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	let directory = fresh("current export");
	library
		.export(personal, &directory, ExportKind::Current)
		.expect("exports");
	let manifest: Json =
		serde_json::from_slice(&fs::read(directory.join("manifest.json")).expect("a manifest"))
			.expect("JSON");
	assert_eq!(manifest["history"], false);
	let lines = entities(&directory);
	assert!(
		lines.iter().all(|line| line["id"] != gone.to_string()),
		"no deleted entity"
	);
	for own in [
		"org.nibkeep.user",
		"org.nibkeep.device",
		"org.nibkeep.replica",
		"org.nibkeep.space",
	] {
		assert!(
			lines.iter().all(|line| !line["types"]
				.as_array()
				.expect("types")
				.contains(&json!(own))),
			"{own}"
		);
	}
	assert!(lines.iter().any(|line| {
		line["types"]
			.as_array()
			.expect("types")
			.contains(&json!("org.nibkeep.type-definition"))
	}));
	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	let title = values(line, "local.title");
	assert_eq!(title.len(), 1);
	assert_eq!(title[0]["value"], "Oat milk");
	for key in ["change", "recorded", "restated", "replaced", "author"] {
		assert!(title[0].get(key).is_none(), "{key}");
	}
	assert!(title[0]["statement"].is_string());
	assert!(!directory.join("changes.jsonl").exists());
}

#[test]
fn an_export_writes_over_nothing() {
	let mut rng = TestRng::new("export over");
	let (library, _, _) = with_a_note("export over", &mut rng);
	let personal = library.personal_space();
	let directory = fresh("export over");
	fs::create_dir_all(&directory).expect("makes a directory");
	fs::write(directory.join("notes.txt"), "mine").expect("writes");
	assert!(matches!(
		library.export(personal, &directory, ExportKind::Full),
		Err(NodeError::ExportExists(_))
	));
	assert_eq!(
		files(&directory),
		[("notes.txt".to_owned(), b"mine".to_vec())]
	);
	// An empty directory takes an export.
	let empty = fresh("export into empty");
	fs::create_dir_all(&empty).expect("makes a directory");
	library
		.export(personal, &empty, ExportKind::Full)
		.expect("exports");
}

/// The kind and value of the only value of a field of an entity's line.
fn only_value(line: &Json, name: &str) -> (Json, Json) {
	let values = values(line, name);
	assert_eq!(values.len(), 1, "{name}");
	(values[0]["kind"].clone(), values[0]["value"].clone())
}

#[test]
fn values_of_every_kind_are_written_in_their_json_forms() {
	let mut rng = TestRng::new("value forms");
	let (library, note, _) = with_a_note("value forms", &mut rng);
	let directory = fresh("value forms");
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let lines = entities(&directory);
	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	let only = |name: &str| only_value(line, name);
	assert_eq!(only("local.tag"), (json!("text"), json!("shop")));
	assert_eq!(only("local.count"), (json!("integer"), json!(3)));
	assert_eq!(
		only("local.big"),
		(json!("integer"), json!((1_i64 << 60).to_string()))
	);
	assert_eq!(only("local.done"), (json!("boolean"), json!(false)));
	assert_eq!(only("local.when"), (json!("date"), json!("2026-10~")));
	assert_eq!(only("local.price"), (json!("decimal"), json!("12.50")));
	assert_eq!(
		only("local.weight"),
		(json!("quantity"), json!({ "number": "72.3", "unit": "kg" }))
	);
	let user = library.status().expect("a status").user.id;
	assert_eq!(
		only("local.about"),
		(json!("reference"), json!(user.to_string()))
	);
	assert_eq!(
		only("local.person"),
		(
			json!("name"),
			json!({ "full": "Ada Lovelace", "family": "Lovelace" })
		)
	);
	assert_eq!(only("local.scan").0, json!("blob"));
}

#[test]
fn an_export_holds_its_files_and_their_sums() {
	let mut rng = TestRng::new("files and sums");
	let (library, note, content) = with_a_note("files and sums", &mut rng);
	let directory = fresh("files and sums");
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	let lines = entities(&directory);
	let line = lines
		.iter()
		.find(|line| line["id"] == note.to_string())
		.expect("the note");
	let (_, hash) = only_value(line, "local.scan");
	let hash = hash.as_str().expect("a hash");
	let digest = hash.strip_prefix("blake3:").expect("a BLAKE3 hash");
	// The file, as it was added.
	assert_eq!(
		fs::read(directory.join("files").join("blake3").join(digest)).expect("the file"),
		content
	);

	// The sums of every other file, which `sha256sum -c` checks.
	let sums = fs::read_to_string(directory.join("SHA256SUMS")).expect("sums");
	let listed: Vec<(String, String)> = sums
		.lines()
		.map(|line| {
			let (sum, path) = line.split_once("  ").expect("a sum and a path");
			(sum.to_owned(), path.to_owned())
		})
		.collect();
	let expected: Vec<(String, String)> = files(&directory)
		.into_iter()
		.filter(|(path, _)| path != "SHA256SUMS")
		.map(|(path, bytes)| {
			let sum = Sha256::digest(&bytes)
				.iter()
				.fold(String::new(), |mut sum, byte| {
					use std::fmt::Write as _;
					let _ = write!(sum, "{byte:02x}");
					sum
				});
			(sum, path)
		})
		.collect();
	assert_eq!(
		listed
			.iter()
			.map(|(_, path)| path.clone())
			.collect::<Vec<_>>(),
		{
			let mut paths: Vec<String> = expected.iter().map(|(_, path)| path.clone()).collect();
			paths.sort();
			paths
		}
	);
	for (sum, path) in &expected {
		assert!(listed.contains(&(sum.clone(), path.clone())), "{path}");
	}
	assert!(sums.ends_with('\n'));
	assert!(
		fs::read_to_string(directory.join("README.txt"))
			.expect("a readme")
			.contains("sha256sum -c SHA256SUMS")
	);
}

#[test]
fn a_file_that_cannot_be_read_is_listed_as_missing() {
	let mut rng = TestRng::new("damaged file");
	let (directory, mut library) = library("damaged file", &mut rng);
	let personal = library.personal_space();
	let note =
		TypeDef::new(NOTE, 1).with_field(field("local.scan"), FieldDef::new(ValueKind::Blob));
	library
		.define_type(personal, &note, now(), &mut rng)
		.expect("defines");
	let content: Vec<u8> = (0..100_000_u32)
		.map(|index| u8::try_from(index % 251).expect("below 251"))
		.collect();
	let hash = library
		.import_blob(personal, content.as_slice(), now(), &mut rng)
		.expect("imports");
	library
		.create_entity(
			personal,
			&[NOTE],
			vec![(field("local.scan"), Value::Blob(hash))],
			now(),
			&mut rng,
		)
		.expect("creates");
	// The library is open, and holds locks on its other files.
	let blobs = nibkeep_store::blob_directory(&common::replica_folder(&directory));
	let sealed = paths(&blobs)
		.into_iter()
		.find(|path| path.contains("blake3"))
		.map(|path| blobs.join(path))
		.expect("the sealed file");
	let mut bytes = fs::read(&sealed).expect("reads");
	bytes[50_000] ^= 1;
	fs::write(&sealed, bytes).expect("damages");
	let export = fresh("damaged file");
	let exported = library
		.export(personal, &export, ExportKind::Full)
		.expect("exports the rest");
	assert_eq!(exported.files, 0);
	assert_eq!(exported.missing_files, [hash]);
	assert!(
		!export.join("files").join("blake3").exists() || files(&export.join("files")).is_empty()
	);
	// Nothing is left beside it.
	let beside: Vec<String> = fs::read_dir(export.parent().expect("a parent"))
		.expect("a directory")
		.map(|entry| {
			entry
				.expect("an entry")
				.file_name()
				.to_string_lossy()
				.into_owned()
		})
		.collect();
	assert_eq!(beside, ["export"]);
}

/// A library with a note, and its full export, checked against nothing yet.
fn exported(test: &str, rng: &mut TestRng) -> (Library, PathBuf) {
	let (library, _, _) = with_a_note(test, rng);
	let directory = fresh(test);
	library
		.export(library.personal_space(), &directory, ExportKind::Full)
		.expect("exports");
	(library, directory)
}

#[test]
fn an_export_matches_the_library_as_of_its_moment() {
	let mut rng = TestRng::new("export matches");
	let (mut library, directory) = exported("export matches", &mut rng);
	let personal = library.personal_space();
	let check = library.verify_export(&directory).expect("checks");
	nibkeep_testkit::assert_empty!(check.problems);
	assert_eq!(check.space, Some(personal));
	assert!(check.entities > 0);
	assert_eq!(check.files, 1);
	// Changes since leave it matching: it is compared with the library as it was then.
	library
		.create_entity(
			personal,
			&[NOTE],
			vec![(field("local.title"), Value::Text("Bread".to_owned()))],
			now() + 120_000,
			&mut rng,
		)
		.expect("creates");
	nibkeep_testkit::assert_empty!(library.verify_export(&directory).expect("checks").problems);
}

#[test]
fn a_changed_export_is_found_out() {
	let mut rng = TestRng::new("export changed");
	let (library, directory) = exported("export changed", &mut rng);
	let lines: Vec<String> = fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.map(str::to_owned)
		.collect();
	let first: Json = serde_json::from_str(&lines[0]).expect("JSON");
	let first_id: EntityId = first["id"].as_str().expect("an ID").parse().expect("an ID");
	let second: Json = serde_json::from_str(&lines[1]).expect("JSON");
	let second_id: EntityId = second["id"]
		.as_str()
		.expect("an ID")
		.parse()
		.expect("an ID");
	// A line changed, and another one gone.
	let changed = lines[0].replacen("\"fields\"", "\"fields \"", 1);
	let rest: String = lines[2..]
		.iter()
		.flat_map(|line| [line.as_str(), "\n"])
		.collect();
	fs::write(
		directory.join("entities.jsonl"),
		format!("{changed}\n{rest}"),
	)
	.expect("writes");
	let check = library.verify_export(&directory).expect("checks");
	assert!(
		check
			.problems
			.contains(&ExportProblem::WrongSum("entities.jsonl".to_owned()))
	);
	assert!(
		check
			.problems
			.contains(&ExportProblem::DifferentEntity(first_id))
	);
	assert!(
		check
			.problems
			.contains(&ExportProblem::MissingEntity(second_id))
	);
	// With its sum written again, the sums no longer tell, but the library does.
	sum_again(&directory, "entities.jsonl");
	let check = library.verify_export(&directory).expect("checks");
	assert_eq!(
		check.problems,
		[
			ExportProblem::DifferentEntity(first_id),
			ExportProblem::MissingEntity(second_id),
			ExportProblem::WrongManifest("entities".to_owned()),
		]
	);
}

#[test]
fn a_changed_list_of_changes_is_found_out() {
	let mut rng = TestRng::new("export changes changed");
	let (library, directory) = exported("export changes changed", &mut rng);
	let path = directory.join("changes.jsonl");
	let listed = fs::read_to_string(&path).expect("changes");
	fs::write(&path, listed.replacen("\"recorded\"", "\"recorded \"", 1)).expect("writes");
	sum_again(&directory, "changes.jsonl");
	assert_eq!(
		library.verify_export(&directory).expect("checks").problems,
		[ExportProblem::DifferentChanges]
	);
	fs::remove_file(&path).expect("removes");
	assert_eq!(
		library.verify_export(&directory).expect("checks").problems,
		[
			ExportProblem::WrongSum("changes.jsonl".to_owned()),
			ExportProblem::DifferentChanges,
		]
	);
}

#[test]
fn lines_that_name_the_same_changes_by_other_numbers_are_alike() {
	let mut rng = TestRng::new("export changes renumbered");
	let (library, directory) = exported("export changes renumbered", &mut rng);
	// Two changes swapped, in the list and in every line that names them, as a change that
	// one side lacks shifts the numbers after it.
	let path = directory.join("changes.jsonl");
	let mut listed: Vec<String> = fs::read_to_string(&path)
		.expect("changes")
		.lines()
		.map(str::to_owned)
		.collect();
	let other = listed
		.iter()
		.position(|change| *change != listed[0])
		.expect("two changes that differ");
	listed.swap(0, other);
	fs::write(&path, listed.join("\n") + "\n").expect("writes");
	let mut lines = String::new();
	for mut line in entities(&directory) {
		for field in line["fields"].as_array_mut().expect("fields") {
			for value in field["values"].as_array_mut().expect("values") {
				let number = change_of(value);
				let swapped = if number == 0 {
					other
				} else if number == other {
					0
				} else {
					number
				};
				value["change"] = json!(swapped);
			}
		}
		lines.push_str(&line.to_string());
		lines.push('\n');
	}
	fs::write(directory.join("entities.jsonl"), lines).expect("writes");
	sum_again(&directory, "changes.jsonl");
	sum_again(&directory, "entities.jsonl");
	assert_eq!(
		library.verify_export(&directory).expect("checks").problems,
		[ExportProblem::DifferentChanges],
		"only the list's order differs"
	);
}

#[test]
fn a_file_gone_from_an_export_is_found_out() {
	let mut rng = TestRng::new("export file gone");
	let (library, directory) = exported("export file gone", &mut rng);
	let (path, _) = files(&directory.join("files"))
		.into_iter()
		.next()
		.expect("a file");
	let hash: nibkeep_base::Hash = format!("blake3:{}", path.rsplit('/').next().expect("a name"))
		.parse()
		.expect("a hash");
	fs::remove_file(directory.join("files").join(&path)).expect("removes");
	fs::write(directory.join("stray.txt"), "mine").expect("writes");
	let check = library.verify_export(&directory).expect("checks");
	assert!(
		check
			.problems
			.contains(&ExportProblem::WrongSum(format!("files/{path}")))
	);
	assert!(check.problems.contains(&ExportProblem::MissingFile(hash)));
	assert!(
		check
			.problems
			.contains(&ExportProblem::Unlisted("stray.txt".to_owned()))
	);
}

#[test]
fn a_folder_that_is_no_export_is_said_so() {
	let mut rng = TestRng::new("no export");
	let (library, _, _) = with_a_note("no export", &mut rng);
	let directory = fresh("no export");
	fs::create_dir_all(&directory).expect("makes a directory");
	let check = library.verify_export(&directory).expect("checks");
	assert!(matches!(
		check.problems.as_slice(),
		[ExportProblem::NotAnExport(_)]
	));
}

#[test]
fn an_export_of_current_values_matches_after_deletions() {
	let mut rng = TestRng::new("current export matches");
	let (mut library, note, _) = with_a_note("current export matches", &mut rng);
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			note,
			vec![Edit::Delete],
			&today(),
			now() + 120_000,
			&mut rng,
		)
		.expect("deletes");
	let directory = fresh("current export matches");
	library
		.export(personal, &directory, ExportKind::Current)
		.expect("exports");
	nibkeep_testkit::assert_empty!(library.verify_export(&directory).expect("checks").problems);
	// An export of current values names no changes.
	fs::write(directory.join("changes.jsonl"), "").expect("writes");
	assert_eq!(
		library.verify_export(&directory).expect("checks").problems,
		[
			ExportProblem::Unlisted("changes.jsonl".to_owned()),
			ExportProblem::DifferentChanges,
		]
	);
}

#[test]
fn lines_and_files_that_do_not_belong_are_found_out() {
	let mut rng = TestRng::new("export strays");
	let (library, directory) = exported("export strays", &mut rng);
	let mut lines = fs::read_to_string(directory.join("entities.jsonl")).expect("entities");
	lines.push_str("{\"foo\":1}\n");
	fs::write(directory.join("entities.jsonl"), lines).expect("writes");
	sum_again(&directory, "entities.jsonl");
	let stray = "files/blake3/00";
	fs::write(directory.join(stray), "not referred to").expect("writes");
	let check = library.verify_export(&directory).expect("checks");
	let count = fs::read_to_string(directory.join("entities.jsonl"))
		.expect("entities")
		.lines()
		.count();
	assert!(
		check.problems.contains(&ExportProblem::DamagedLine(count)),
		"{:?}",
		check.problems
	);
	assert!(
		check
			.problems
			.contains(&ExportProblem::ExtraFile(stray.to_owned()))
	);
	assert!(
		check
			.problems
			.contains(&ExportProblem::Unlisted(stray.to_owned()))
	);
	assert_eq!(check.entities, count - 1);
}

#[test]
fn a_missing_entities_file_is_reported_and_sums_outside_are_refused() {
	let mut rng = TestRng::new("export entities gone");
	let (library, directory) = exported("export entities gone", &mut rng);
	fs::remove_file(directory.join("entities.jsonl")).expect("removes");
	let mut sums = fs::read_to_string(directory.join("SHA256SUMS")).expect("sums");
	sums.push_str("0000  ../outside\n");
	fs::write(directory.join("SHA256SUMS"), sums).expect("writes");
	let check = library.verify_export(&directory).expect("checks");
	assert!(
		check
			.problems
			.contains(&ExportProblem::WrongSum("entities.jsonl".to_owned()))
	);
	assert!(
		check
			.problems
			.contains(&ExportProblem::WrongSum("../outside".to_owned()))
	);
	assert!(
		check
			.problems
			.iter()
			.any(|problem| matches!(problem, ExportProblem::MissingEntity(_)))
	);
	assert_eq!(check.entities, 0);
}
