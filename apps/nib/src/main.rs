//! `nib`, the generic command-line tool for NibKeep.

use std::io;
use std::process::ExitCode;

use clap::FromArgMatches;
use nibkeep_cli::{Cli, System};

/// The stack that `nib` runs on, as on Linux: clap's code for its many commands needs more
/// than the 1 MB that Windows gives a program's main thread, in a debug build.
const STACK: usize = 8 << 20;

fn main() -> ExitCode {
	match std::thread::Builder::new()
		.name("nib".to_owned())
		.stack_size(STACK)
		.spawn(nib)
	{
		Ok(thread) => thread
			.join()
			.unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
		// Where no thread can start, the main thread's stack may do.
		Err(_) => nib(),
	}
}

/// Runs the command that the arguments name.
fn nib() -> ExitCode {
	// With nib --help listing the commands by what they work on.
	let matches = nibkeep_cli::command().get_matches();
	let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
	let mut system = System::new();
	// A command's work goes on when nothing reads what it prints, as when `head` has read
	// enough.
	let output = &mut nibkeep_cli::UntilUnread::new(io::stdout().lock());
	let ran = if cli.timings {
		nibkeep_cli::run_timed(&cli, &mut system, output).map(|timings| {
			// With --json, the line is JSON alone, for scripts to read.
			if cli.json {
				eprintln!("{}", timings.describe(true));
			} else {
				eprintln!("nib: {}", timings.describe(false));
			}
		})
	} else {
		nibkeep_cli::run(&cli, &mut system, output)
	};
	match ran {
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("nib: {}", nibkeep_cli::printable(&error.to_string()));
			ExitCode::FAILURE
		}
	}
}
