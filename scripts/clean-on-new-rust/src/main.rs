//! Removes a build folder when the Rust toolchain that builds into it changes, so that the
//! builds of old toolchains do not fill the disk of a runner that keeps its builds.
//!
//! Usage: clean-on-new-rust <toolchain> <folder>
//!
//! It writes the toolchain's version into `<folder>/rust-version`, and removes the folder
//! when the version there is another.

use std::path::Path;
use std::process::{Command, ExitCode};
use std::{env, fs};

fn main() -> ExitCode {
	let arguments: Vec<String> = env::args().skip(1).collect();
	let [toolchain, folder] = arguments.as_slice() else {
		eprintln!("usage: clean-on-new-rust <toolchain> <folder>");
		return ExitCode::FAILURE;
	};
	let output = match Command::new("rustc")
		.arg(format!("+{toolchain}"))
		.arg("--version")
		.output()
	{
		Ok(output) if output.status.success() => output,
		_ => {
			eprintln!("cannot ask rustc +{toolchain} for its version");
			return ExitCode::FAILURE;
		}
	};
	let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
	let folder = Path::new(folder);
	let stamp = folder.join("rust-version");
	let built_with = fs::read_to_string(&stamp).unwrap_or_default();
	if built_with.trim() != version {
		if folder.exists() {
			println!(
				"{} was built with {built_with:?}; removing it for {version}",
				folder.display()
			);
			if let Err(error) = fs::remove_dir_all(folder) {
				eprintln!("cannot remove {}: {error}", folder.display());
				return ExitCode::FAILURE;
			}
		}
		if let Err(error) = fs::create_dir_all(folder).and_then(|()| fs::write(&stamp, &version)) {
			eprintln!("cannot write {}: {error}", stamp.display());
			return ExitCode::FAILURE;
		}
	}
	ExitCode::SUCCESS
}
