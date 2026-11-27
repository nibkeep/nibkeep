//! Every file of the workspace's integration tests runs: each crate's tests are one program,
//! `tests/main.rs`, of which each file is a module, but for a file that is a program of its
//! own, a `[[test]]` in the crate's `Cargo.toml`. With `autotests = false`, cargo builds no
//! other file there, nor a folder's `main.rs`, so a file without its `mod` line would
//! silently never run.

use std::fs;
use std::path::Path;

/// The line of a module declaration with its attributes and visibility left out, as
/// `mod name;` of `#[cfg(unix)] pub(crate) mod name;`.
fn declaration(line: &str) -> String {
	let mut rest = line.trim();
	while let Some(attribute) = rest.strip_prefix("#[") {
		let Some(end) = attribute.find(']') else {
			break;
		};
		rest = attribute[end + 1..].trim_start();
	}
	for visibility in ["pub(crate) ", "pub(super) ", "pub "] {
		rest = rest.strip_prefix(visibility).unwrap_or(rest);
	}
	rest.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the manifest declares a test program at `path`, such as `tests/one.rs`.
fn declares_program(manifest: &str, path: &str) -> bool {
	let wanted = format!("path=\"{path}\"");
	manifest
		.lines()
		.any(|line| line.split_whitespace().collect::<String>() == wanted)
}

#[test]
fn every_file_of_tests_is_in_a_test_program() {
	let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	if !workspace.join("Cargo.toml").is_file() {
		// A phone or another device, to which the test went without the sources.
		eprintln!("the workspace's sources are not here, so its tests are not checked");
		return;
	}
	let mut problems = Vec::new();
	for group in ["apps", "crates"] {
		for entry in fs::read_dir(workspace.join(group)).expect("reads the crates") {
			let folder = entry.expect("reads a crate").path();
			let Ok(entries) = fs::read_dir(folder.join("tests")) else {
				continue;
			};
			let crate_name = folder.file_name().expect("a name").to_string_lossy();
			// The files of tests, and the folders with a `main.rs` that cargo would take for
			// a program each, as `tests/end_to_end/main.rs`; `tests/main.rs` itself is not one.
			let mut files: Vec<String> = Vec::new();
			let mut folders: Vec<String> = Vec::new();
			for path in entries.map(|entry| entry.expect("reads a test file").path()) {
				let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
					continue;
				};
				if path.is_dir() {
					if path.join("main.rs").is_file() {
						folders.push(name.to_owned());
					}
				} else if let Some(stem) = name.strip_suffix(".rs")
					&& stem != "main"
				{
					files.push(stem.to_owned());
				}
			}
			files.sort();
			folders.sort();
			let manifest =
				fs::read_to_string(folder.join("Cargo.toml")).expect("reads the crate's manifest");
			if !manifest
				.lines()
				.any(|line| line.split_whitespace().collect::<String>() == "autotests=false")
			{
				if files.len() + folders.len() > 1 {
					problems.push(format!(
						"{group}/{crate_name}: each file of tests is a program of its own; make them \
						 modules of tests/main.rs, with autotests = false"
					));
				}
				continue;
			}
			let main = fs::read_to_string(folder.join("tests/main.rs")).unwrap_or_default();
			for file in files {
				let module = format!("mod {file};");
				if !main.lines().any(|line| declaration(line) == module)
					&& !declares_program(&manifest, &format!("tests/{file}.rs"))
				{
					problems.push(format!(
						"{group}/{crate_name}/tests/{file}.rs runs in no test program: add `{module}` \
						 to tests/main.rs"
					));
				}
			}
			for name in folders {
				let path = format!("tests/{name}/main.rs");
				if !declares_program(&manifest, &path) {
					problems.push(format!(
						"{group}/{crate_name}/{path} runs in no test program: make its files modules \
						 of tests/main.rs, or declare it as a [[test]] in Cargo.toml"
					));
				}
			}
		}
	}
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn a_declaration_is_found_with_its_attributes_and_visibility() {
	assert_eq!(declaration("mod one;"), "mod one;");
	assert_eq!(declaration("  #[cfg(unix)] mod one;"), "mod one;");
	assert_eq!(
		declaration("#[cfg(unix)] #[allow(dead_code)] pub(crate) mod one;"),
		"mod one;"
	);
	assert_ne!(declaration("// mod one;"), "mod one;");
	assert!(declares_program("path=\"tests/one.rs\"", "tests/one.rs"));
	assert!(declares_program(
		"  path = \"tests/one.rs\"\n",
		"tests/one.rs"
	));
	assert!(!declares_program("path = \"tests/two.rs\"", "tests/one.rs"));
}
