//! The shape of nib's commands: summaries that fit a line, the overview that groups the
//! commands in `nib --help`, and the commands that went into others.

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use nibkeep_cli::{Cli, Command};

/// The width of a terminal, which every line of a list of commands fits in.
const WIDTH: usize = 80;

/// What these arguments come to: the command, or the kind of error clap gives.
fn parsed(arguments: &[&str]) -> Result<Command, ErrorKind> {
	Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
		.map(|cli| cli.command)
		.map_err(|error| error.kind())
}

/// The lines of the list of commands in this help: those after `Commands:`, up to the next
/// empty line.
fn listed(help: &str) -> Vec<String> {
	help.lines()
		.skip_while(|line| *line != "Commands:")
		.skip(1)
		.take_while(|line| !line.is_empty())
		.map(str::to_owned)
		.collect()
}

/// Every command that has commands of its own, but `nib` itself, with the names that lead
/// to it.
fn groups(command: &clap::Command, path: &[&str], found: &mut Vec<(String, clap::Command)>) {
	for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
		if sub.has_subcommands() {
			let mut names = path.to_vec();
			names.push(sub.get_name());
			found.push((names.join(" "), sub.clone()));
			groups(sub, &names, found);
		}
	}
}

#[test]
fn every_summary_fits_a_line_of_its_list() {
	let mut found = Vec::new();
	groups(&Cli::command(), &[], &mut found);
	assert!(found.len() > 5, "{}", found.len());
	for (path, mut group) in found {
		let help = group.render_help().to_string();
		let lines = listed(&help);
		assert!(!lines.is_empty(), "nib {path}: {help}");
		for line in lines {
			assert!(
				line.chars().count() <= WIDTH,
				"nib {path}: {} characters: {line}",
				line.chars().count()
			);
		}
	}
}

/// The names of the commands that `nib --help` lists, from its overview: the first word of
/// each line indented by two spaces.
fn overview(help: &str) -> Vec<String> {
	help.lines()
		.take_while(|line| !line.starts_with("Options:"))
		.filter_map(|line| line.strip_prefix("  "))
		.filter(|line| !line.starts_with(' '))
		.filter_map(|line| line.split_whitespace().next())
		.map(str::to_owned)
		.collect()
}

#[test]
fn the_overview_lists_every_command_once_by_what_it_works_on() {
	let help = nibkeep_cli::command().render_help().to_string();
	let mut shown = overview(&help);
	let mut visible: Vec<String> = Cli::command()
		.get_subcommands()
		.filter(|sub| !sub.is_hide_set())
		.map(|sub| sub.get_name().to_owned())
		.collect();
	let count = shown.len();
	shown.sort();
	shown.dedup();
	assert_eq!(shown.len(), count, "a command listed twice: {help}");
	visible.sort();
	assert_eq!(shown, visible, "{help}");
	// By what they work on, under headings.
	for heading in ["Library:", "Entities:", "Trees:", "Safekeeping:"] {
		assert!(
			help.lines().any(|line| line == heading),
			"{heading}: {help}"
		);
	}
	for line in help
		.lines()
		.take_while(|line| !line.starts_with("Options:"))
	{
		assert!(line.chars().count() <= WIDTH, "{line}");
	}
	// The other names that commands answer to, for Git's, Mercurial's and Subversion's.
	for (alias, name) in [
		("st", "status"),
		("ci", "commit"),
		("up", "update"),
		("push", "sync"),
		("annotate", "blame"),
		("backout", "revert"),
	] {
		assert!(
			help.contains(&format!("{alias} for {name}")),
			"{alias}: {help}"
		);
	}
}

#[test]
fn the_overview_is_what_nib_help_prints() {
	let error = nibkeep_cli::command()
		.try_get_matches_from(["nib", "--help"])
		.expect_err("prints the help");
	assert_eq!(error.kind(), ErrorKind::DisplayHelp);
	let help = error.render().to_string();
	assert!(help.contains("\nEntities:\n"), "{help}");
	assert!(!help.contains("\nCommands:\n"), "{help}");
}

/// What only trees have went into the commands that do the like: nib tree is gone.
#[test]
fn trees_have_no_commands_of_their_own() {
	assert_eq!(
		parsed(&["tree", "secrets"]).expect_err("gone"),
		ErrorKind::InvalidSubcommand
	);
	assert!(matches!(
		parsed(&["status", "--secrets"]),
		Ok(Command::Status { secrets: true, .. })
	));
	assert_eq!(
		parsed(&["status", "--secrets", "--library-wide"]).expect_err("conflicts"),
		ErrorKind::ArgumentConflict
	);
	assert!(parsed(&["commit", "--pin", "vendor/lib"]).is_ok());
	assert!(parsed(&["commit", "--unpin", "vendor/lib"]).is_ok());
	assert_eq!(
		parsed(&["commit", "--pin", "a", "--unpin", "b"]).expect_err("conflicts"),
		ErrorKind::ArgumentConflict
	);
	assert_eq!(
		parsed(&["commit", "--pin", "a", "b"]).expect_err("conflicts"),
		ErrorKind::ArgumentConflict
	);
}

/// nib push syncs, as nib sync does, under another name.
#[test]
fn push_is_another_name_for_sync() {
	assert!(matches!(
		parsed(&["push", "--wait", "3"]),
		Ok(Command::Sync {
			wait: 3,
			serve: false,
			..
		})
	));
}

/// --timings is for those who measure nib, and --port is only where nib takes connections.
#[test]
fn the_options_of_every_command_are_few() {
	let cli = Cli::command();
	let global: Vec<&str> = cli
		.get_arguments()
		.filter(|argument| argument.is_global_set() && !argument.is_hide_set())
		.map(|argument| argument.get_id().as_str())
		.collect();
	assert_eq!(global, ["library", "space", "json", "passphrase_file"]);
	assert!(parsed(&["--timings", "list"]).is_ok());
	assert_eq!(
		parsed(&["--port", "53400", "list"]).expect_err("no such option there"),
		ErrorKind::UnknownArgument
	);
	// The commands that take connections, which a firewall lets through at that port.
	for arguments in [
		&["sync", "--serve", "--port", "53400"][..],
		&["device", "pair", "--port", "53400"],
		&["space", "invite", "Family", "--port", "53400"],
	] {
		parsed(arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error:?}"));
	}
}

/// nib mcp alone serves nothing, which on a terminal would only wait: it shows its help.
#[test]
fn the_server_of_ai_agents_is_nib_mcp_serve() {
	assert_eq!(
		parsed(&["mcp"]).expect_err("shows the help"),
		ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
	);
	assert!(parsed(&["mcp", "serve", "--agent", "Claude", "--wait", "1"]).is_ok());
	assert!(parsed(&["mcp", "enable", "--print"]).is_ok());
}

/// nib file thumbnail, for scripts and tests, works without being listed beside nib file
/// thumbnails.
#[test]
fn the_thumbnail_of_one_file_is_not_listed() {
	let cli = Cli::command();
	let file = cli.find_subcommand("file").expect("nib file");
	let thumbnail = file
		.find_subcommand("thumbnail")
		.expect("nib file thumbnail");
	assert!(thumbnail.is_hide_set());
	assert!(parsed(&["file", "thumbnail", "photo", "out.jpg"]).is_ok());
}

/// What a space holds besides its entities is shown at once by nib space show, and what
/// moves left behind is listed by the command that purges it.
#[test]
fn a_space_is_shown_whole() {
	assert!(parsed(&["space", "show", "Family"]).is_ok());
	for gone in [
		"members", "groups", "requests", "claims", "review", "purges",
	] {
		assert_eq!(
			parsed(&["space", gone, "Family"]).expect_err("gone"),
			ErrorKind::InvalidSubcommand,
			"{gone}"
		);
	}
	assert!(matches!(
		parsed(&["purge", "--behind"]),
		Ok(Command::Purge {
			entity: None,
			behind: true,
			..
		})
	));
	assert_eq!(
		parsed(&["purge"]).expect_err("an entity, or --behind"),
		ErrorKind::MissingRequiredArgument
	);
}

/// Bringing a library back goes with the recovery codes, and nib init only makes or joins.
#[test]
fn bringing_a_library_back_is_a_command_of_recovery() {
	assert!(parsed(&["recovery", "restore"]).is_ok());
	assert!(
		parsed(&[
			"recovery",
			"restore",
			"/media/stick/backup",
			"--keep",
			"phone",
			"--device",
			"desktop",
			"--passphrase",
		])
		.is_ok()
	);
	assert!(parsed(&["recovery", "from-copy", "--device", "desktop"]).is_ok());
	for gone in [&["init", "--recover"][..], &["init", "--from-copy"]] {
		assert_eq!(
			parsed(gone).expect_err("gone"),
			ErrorKind::UnknownArgument,
			"{gone:?}"
		);
	}
}

/// `new` makes something, and `add` brings in what exists, such as files.
#[test]
fn new_makes_and_add_brings_in() {
	for arguments in [
		&["backup", "new", "/media/stick"][..],
		&["policy", "new", "Precious", "--copies", "3"],
		&["policy", "rule", "new", "Precious", "--type", "std.photo"],
		&["space", "new", "Family"],
		&["recovery", "new"],
		&["file", "add", "photo.jpg"],
	] {
		parsed(arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error:?}"));
	}
	for gone in [
		&["backup", "add", "/media/stick"][..],
		&["policy", "add", "Precious"],
		&["policy", "rule", "add", "Precious"],
	] {
		assert_eq!(
			parsed(gone).expect_err("gone"),
			ErrorKind::InvalidSubcommand,
			"{gone:?}"
		);
	}
}

/// One command imports an export of a library and an archive of Google Takeout alike.
#[test]
fn one_command_imports() {
	assert!(parsed(&["import", "export-folder"]).is_ok());
	assert!(
		parsed(&[
			"import",
			"takeout-001.zip",
			"takeout-002.zip",
			"--from",
			"Google Photos (Mom)"
		])
		.is_ok()
	);
	assert_eq!(
		parsed(&["file", "takeout", "takeout-001.zip"]).expect_err("gone"),
		ErrorKind::InvalidSubcommand
	);
}
