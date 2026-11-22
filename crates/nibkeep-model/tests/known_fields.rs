//! Every field of NibKeep's own that the programs define is one that this version knows, so
//! that a governing change that states it is not taken for one that a later version made
//! (`docs/spec/log.md`, section 1).

use std::path::{Path, PathBuf};

use nibkeep_model::{FieldName, fields};

/// The source files of the crates and the apps' cores.
fn sources(folder: &Path, found: &mut Vec<PathBuf>) {
	let Ok(entries) = std::fs::read_dir(folder) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let name = entry.file_name();
		if name == "target" || name == "tests" || name == "build" {
			continue;
		}
		if path.is_dir() {
			sources(&path, found);
		} else if path.extension().is_some_and(|extension| extension == "rs") {
			found.push(path);
		}
	}
}

/// The names of the fields of NibKeep's own that a source file defines, as
/// `FieldName::from_static("org.nibkeep.…")`.
fn defined(source: &str) -> Vec<String> {
	let mut names = Vec::new();
	for (at, _) in source.match_indices("FieldName::from_static(") {
		let rest = source[at + "FieldName::from_static(".len()..].trim_start();
		if let Some(quoted) = rest.strip_prefix('"')
			&& let Some(end) = quoted.find('"')
			&& quoted[..end].starts_with("org.nibkeep.")
		{
			names.push(quoted[..end].to_owned());
		}
	}
	names
}

#[test]
fn every_field_of_nibkeep_own_is_known() {
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	let mut files = Vec::new();
	sources(&workspace.join("crates"), &mut files);
	sources(&workspace.join("apps"), &mut files);
	let mut count = 0;
	for file in files {
		let source = std::fs::read_to_string(&file).expect("reads the source");
		for name in defined(&source) {
			let field: FieldName = name.parse().expect("a field name");
			assert!(fields::is_known(&field), "{name}, in {}", file.display());
			count += 1;
		}
	}
	// The fields module alone defines dozens.
	assert!(count > 60, "found {count}");
}

#[test]
fn a_field_that_nibkeep_does_not_define_is_not_known() {
	for name in [
		"org.nibkeep.from-a-later-version",
		"org.example.note",
		"std.password",
	] {
		let field: FieldName = name.parse().expect("a field name");
		assert!(!fields::is_known(&field), "{name}");
	}
	for known in [fields::NAME, fields::SIGNING_KEY, fields::MIGRATED_FROM] {
		assert!(fields::is_known(&known), "{known}");
	}
}
