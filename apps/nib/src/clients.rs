//! The AI clients that `nib mcp enable` tells of a library's server, and `nib mcp disable`
//! takes it from ([technical design: AI agents]): each through its own command, so that
//! `nib` never writes another program's configuration. A client that none of them is still
//! works from the line that `nib mcp enable --print` prints.
//!
//! [technical design: AI agents]: ../../../docs/Technical-design.md#14-ai-agents

use std::path::Path;

/// An AI client that has a command of its own for adding and removing MCP servers.
pub(crate) struct AiClient {
	/// Its name for people, and for `--client`.
	pub(crate) name: &'static str,
	/// The program whose commands add and remove servers.
	pub(crate) program: &'static str,
	/// The arguments that add a server of this name that runs this command line.
	add: fn(&str, &[String]) -> Vec<String>,
	/// The arguments that remove the server of this name.
	remove: fn(&str) -> Vec<String>,
}

impl AiClient {
	/// The arguments of the client's program that add the server `name`, which runs `line`.
	pub(crate) fn add(&self, name: &str, line: &[String]) -> Vec<String> {
		(self.add)(name, line)
	}

	/// The arguments of the client's program that remove the server `name`.
	pub(crate) fn remove(&self, name: &str) -> Vec<String> {
		(self.remove)(name)
	}
}

/// The AI clients that `nib mcp enable` knows, by name.
pub(crate) const AI_CLIENTS: [AiClient; 2] = [
	AiClient {
		name: "Claude Code",
		program: "claude",
		// For the person, in every folder, rather than for one project.
		add: |name, line| {
			let mut arguments = words(&["mcp", "add", "--scope", "user", name, "--"]);
			arguments.extend_from_slice(line);
			arguments
		},
		remove: |name| words(&["mcp", "remove", "--scope", "user", name]),
	},
	AiClient {
		name: "Codex",
		program: "codex",
		add: |name, line| {
			let mut arguments = words(&["mcp", "add", name, "--"]);
			arguments.extend_from_slice(line);
			arguments
		},
		remove: |name| words(&["mcp", "remove", name]),
	},
];

/// The client named `name`, ignoring case, if `nib` knows it.
pub(crate) fn ai_client(name: &str) -> Option<&'static AiClient> {
	AI_CLIENTS
		.iter()
		.find(|client| client.name.eq_ignore_ascii_case(name) || client.program == name)
}

fn words(words: &[&str]) -> Vec<String> {
	words.iter().map(|&word| word.to_owned()).collect()
}

/// The name of a library's server among a client's servers: from its folder's name, in
/// lowercase letters, digits and dashes, such as `nibkeep-to-read`, so that each library
/// has its own.
pub(crate) fn server_name(folder: &Path) -> String {
	let named = folder
		.file_name()
		.map_or_default(|name| name.to_string_lossy().to_lowercase());
	let mut name = String::from("nibkeep");
	let mut dash = true;
	for character in named.chars() {
		if character.is_ascii_alphanumeric() {
			if dash {
				name.push('-');
			}
			name.push(character);
			dash = false;
		} else {
			dash = true;
		}
	}
	name
}

/// The command line that a client runs to serve the library in `folder`: the same for
/// every client, which names itself when it connects.
pub(crate) fn server_line(nib: &Path, folder: &Path) -> Vec<String> {
	vec![
		nib.to_string_lossy().into_owned(),
		"mcp".to_owned(),
		"serve".to_owned(),
		"--library".to_owned(),
		folder.to_string_lossy().into_owned(),
	]
}

/// The entry for the server in the `mcpServers` object of JSON that most clients take,
/// for the person to paste where no command of a client adds it.
pub(crate) fn mcp_servers_entry(name: &str, line: &[String]) -> serde_json::Value {
	let (command, arguments) = line.split_first().expect("a program");
	serde_json::json!({ "mcpServers": { name: { "command": command, "args": arguments } } })
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::{ai_client, mcp_servers_entry, server_line, server_name};

	#[test]
	fn a_librarys_server_is_named_after_its_folder() {
		assert_eq!(
			server_name(Path::new("/home/ada/Libraries/to-read")),
			"nibkeep-to-read"
		);
		assert_eq!(
			server_name(Path::new("/x/My Books (2026)")),
			"nibkeep-my-books-2026"
		);
		assert_eq!(server_name(Path::new("/")), "nibkeep");
	}

	#[test]
	fn every_client_takes_one_line() {
		let line = server_line(Path::new("/bin/nib"), Path::new("/home/ada/to-read"));
		let claude = ai_client("claude code").expect("known");
		assert_eq!(
			claude.add("nibkeep-to-read", &line).join(" "),
			"mcp add --scope user nibkeep-to-read -- /bin/nib mcp serve --library /home/ada/to-read"
		);
		assert_eq!(
			ai_client("codex").expect("known").remove("nibkeep-to-read"),
			["mcp", "remove", "nibkeep-to-read"]
		);
		assert_eq!(
			mcp_servers_entry("nibkeep-to-read", &line)["mcpServers"]["nibkeep-to-read"]["args"],
			serde_json::json!(["mcp", "serve", "--library", "/home/ada/to-read"])
		);
		assert!(ai_client("an unknown client").is_none());
	}
}
