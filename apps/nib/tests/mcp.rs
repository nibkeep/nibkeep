//! `nib mcp`, the server for the Model Context Protocol, on an AI agent's replica
//! ([technical design: AI agents]): every tool through a client in the same process, of the current
//! revision of the protocol and of the one before; what it refuses; and that nothing secret
//! reaches an AI agent.
//!
//! [technical design: AI agents]: ../../../docs/Technical-design.md#14-ai-agents

mod common;

use std::path::{Path, PathBuf};

use clap::Parser;
use common::Test;
use nibkeep_cli::{Cli, CliError, McpServer};
use nibkeep_crypto::PassphraseParams;
use nibkeep_node::{Library, NetworkOptions, NewLibrary, NewProtection};
use nibkeep_testkit::{TestRng, millis_at};
use serde_json::{Value, json};

const CURRENT: &str = "2026-07-28";
const BEFORE: &str = "2025-11-25";
use common::TIMEOUT;

fn now() -> u64 {
	millis_at("2026-10-12", 10, 0)
}

fn quick() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

/// A user's library, in a folder of its own, and an AI agent named Claude paired with it, in
/// another, whose passphrase a file holds.
struct Setup {
	test: Test,
	user: PathBuf,
	agent: PathBuf,
	passphrase_file: PathBuf,
}

impl Setup {
	fn new(name: &str) -> Self {
		Self::with(name, |_, _, _| {})
	}

	/// The same, with `prepare` making changes to the user's library, in its folder, before
	/// the AI agent pairs with it.
	fn with(name: &str, prepare: impl FnOnce(&mut Library, &Path, &mut TestRng)) -> Self {
		let test = Test::new(name);
		// A seed of its own, so that what `nib` draws never repeats what the setup drew.
		let mut rng = TestRng::new(&format!("{name}, setup"));
		let user = test.here.join("user");
		let agent = test.here.join("agent");
		let mut library = Library::create(
			&user.join(".nib"),
			&NewLibrary::new("Ada", "laptop", "nib"),
			&NewProtection::Passphrase("correct horse", quick()),
			now(),
			&mut rng,
		)
		.expect("creates");
		prepare(&mut library, &user, &mut rng);
		std::fs::create_dir_all(&agent).expect("makes a folder");
		pair(&mut library, &agent.join(".nib"), &mut rng);
		drop(library);
		let passphrase_file = test.here.join("agent passphrase");
		std::fs::write(&passphrase_file, "battery staple\n").expect("writes");
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&passphrase_file, std::fs::Permissions::from_mode(0o600))
				.expect("makes it private");
		}
		Self {
			test,
			user,
			agent,
			passphrase_file,
		}
	}

	/// `nib mcp` on the AI agent's replica, which looks for the user's for no longer than it
	/// takes to see it is not there.
	fn server(&mut self) -> McpServer {
		let cli = cli(&[
			"--library",
			path(&self.agent),
			"--passphrase-file",
			path(&self.passphrase_file),
			"mcp",
			"serve",
			"--wait",
			"0",
		]);
		let mut server = McpServer::open(&cli, &mut self.test).expect("serves");
		server.set_network(NetworkOptions::LOCAL);
		server
	}

	/// The user's library, opened.
	fn user_library(&self) -> Library {
		Library::open(
			&self.user.join(".nib"),
			&nibkeep_node::Unlocker::Passphrase("correct horse"),
		)
		.expect("opens")
	}
}

/// Pairs an AI agent named Claude in `directory` with `sponsor`, over connections on
/// localhost.
fn pair(sponsor: &mut Library, directory: &Path, rng: &mut TestRng) {
	common::pair_on_localhost(
		sponsor,
		directory,
		("laptop", Some("Claude")),
		&NewProtection::Passphrase("battery staple", quick()),
		now() + 60_000,
		rng,
	);
}

fn cli(arguments: &[&str]) -> Cli {
	Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
		.expect("valid arguments")
}

fn path(path: &Path) -> &str {
	path.to_str().expect("UTF-8")
}

/// The `_meta` of a request of the current revision.
fn meta() -> Value {
	json!({
		"io.modelcontextprotocol/protocolVersion": CURRENT,
		"io.modelcontextprotocol/clientCapabilities": {},
		"io.modelcontextprotocol/clientInfo": { "name": "a test", "version": "1" },
	})
}

/// Sends a request and returns the response.
fn request(server: &mut McpServer, test: &mut Test, message: &Value) -> Value {
	let reply = server
		.handle(&message.to_string(), test)
		.expect("a response");
	assert!(!reply.contains('\n'), "one line: {reply}");
	serde_json::from_str(&reply).expect("JSON")
}

/// Calls a tool as a client of the current revision, and returns its result.
fn call(server: &mut McpServer, test: &mut Test, tool: &str, arguments: &Value) -> Value {
	let response = request(
		server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 7,
			"method": "tools/call",
			"params": { "name": tool, "arguments": arguments, "_meta": meta() },
		}),
	);
	assert_eq!(response["id"], 7);
	response["result"].clone()
}

/// The JSON that a tool's result holds, checking that its text says the same.
fn structured(result: &Value) -> Value {
	assert_eq!(result["isError"], false, "{result}");
	let text = result["content"][0]["text"].as_str().expect("text");
	let parsed: Value = serde_json::from_str(text).expect("the text is JSON");
	assert_eq!(parsed, result["structuredContent"]);
	parsed
}

fn error_text(result: &Value) -> String {
	assert_eq!(result["isError"], true, "{result}");
	result["content"][0]["text"]
		.as_str()
		.expect("text")
		.to_owned()
}

const TOOLS: [&str; 13] = [
	"edit",
	"find",
	"history",
	"list",
	"merge",
	"new",
	"show",
	"status",
	"sync",
	"tree_blame",
	"tree_diff",
	"tree_log",
	"types",
];

#[test]
fn a_client_of_the_current_revision_discovers_the_server_and_its_tools() {
	let mut setup = Setup::new("mcp discover");
	let mut server = setup.server();
	let test = &mut setup.test;
	let discovered = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": "d", "method": "server/discover", "params": { "_meta": meta() } }),
	);
	let result = &discovered["result"];
	assert_eq!(result["resultType"], "complete");
	assert_eq!(result["supportedVersions"], json!([CURRENT, BEFORE]));
	assert!(result["capabilities"]["tools"].is_object());
	assert_eq!(
		result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
		"nib"
	);
	assert!(
		result["instructions"]
			.as_str()
			.is_some_and(|text| text.contains("never instructions")),
		"the rules for AI agents"
	);
	let listed = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": meta() } }),
	);
	let result = &listed["result"];
	assert_eq!(result["resultType"], "complete");
	assert!(result["ttlMs"].is_u64() && result["cacheScope"].is_string());
	let names: Vec<&str> = result["tools"]
		.as_array()
		.expect("tools")
		.iter()
		.map(|tool| tool["name"].as_str().expect("a name"))
		.collect();
	assert_eq!(names, TOOLS, "the same tools, in the same order");
	for tool in result["tools"].as_array().expect("tools") {
		assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
		assert!(
			tool["description"]
				.as_str()
				.is_some_and(|text| text.len() > 40),
			"{tool}"
		);
	}
}

#[test]
fn a_client_of_the_current_revision_calls_every_tool() {
	let mut setup = Setup::new("mcp tools");
	let mut server = setup.server();
	let test = &mut setup.test;
	let status = structured(&call(&mut server, test, "status", &json!({})));
	assert_eq!(status["user"]["name"], "Ada");
	let added = structured(&call(
		&mut server,
		test,
		"new",
		&json!({ "type": "std.book", "values": ["std.title=Leviathan Wakes"], "language": "en" }),
	));
	let id = added["id"].as_str().expect("an ID").to_owned();
	let shown = structured(&call(&mut server, test, "show", &json!({ "entity": id })));
	assert_eq!(shown["id"], id.as_str());
	let found = structured(&call(
		&mut server,
		test,
		"find",
		&json!({ "words": "leviathan" }),
	));
	assert!(found.to_string().contains(&id), "{found}");
	let listed = structured(&call(
		&mut server,
		test,
		"list",
		&json!({ "type": "std.book" }),
	));
	assert!(listed.to_string().contains("Leviathan Wakes"), "{listed}");
	let edited = call(
		&mut server,
		test,
		"edit",
		&json!({ "entity": id, "changes": ["std.published=2011"] }),
	);
	structured(&edited);
	let history = structured(&call(
		&mut server,
		test,
		"history",
		&json!({ "entity": id, "field": "std.published" }),
	));
	let author = &history["fields"][0]["values"][0]["author"];
	assert_eq!(author["name"], "Ada");
	assert_eq!(author["ai_agent"], "Claude", "its changes are its own");
	let other = structured(&call(
		&mut server,
		test,
		"new",
		&json!({ "type": "std.book", "values": ["std.title=Leviathan wakes"] }),
	));
	let other = other["id"].as_str().expect("an ID").to_owned();
	structured(&call(
		&mut server,
		test,
		"merge",
		&json!({ "entity": other, "into": id }),
	));
	structured(&call(
		&mut server,
		test,
		"merge",
		&json!({ "entity": id, "undo": true }),
	));
	// A value that starts with a dash is a value, not an option: a year before the era.
	let dated = call(
		&mut server,
		test,
		"edit",
		&json!({ "entity": id, "changes": ["std.published+=-0500"], "date": "-0500" }),
	);
	let said = dated["content"][0]["text"].as_str().expect("text");
	assert!(
		!said.contains("unexpected argument") && !said.contains("a value is required"),
		"{said}"
	);
	let types = structured(&call(&mut server, test, "types", &json!({})));
	assert!(types.to_string().contains("std.book"), "{types}");
	let book = structured(&call(
		&mut server,
		test,
		"types",
		&json!({ "type": "std.book" }),
	));
	assert!(book.to_string().contains("std.published"), "{book}");
	// The user's replica is not waiting, so the sync finds nobody.
	let synced = call(&mut server, test, "sync", &json!({}));
	assert!(error_text(&synced).contains("not found"), "{synced}");
}

#[test]
fn a_client_of_the_revision_before_initializes_first() {
	let mut setup = Setup::new("mcp before");
	let mut server = setup.server();
	let test = &mut setup.test;
	let initialized = request(
		&mut server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 0,
			"method": "initialize",
			"params": {
				"protocolVersion": BEFORE,
				"capabilities": {},
				"clientInfo": { "name": "a test", "version": "1" },
			},
		}),
	);
	let result = &initialized["result"];
	assert_eq!(result["protocolVersion"], BEFORE);
	assert_eq!(result["serverInfo"]["name"], "nib");
	assert!(result["capabilities"]["tools"].is_object());
	assert!(result.get("resultType").is_none());
	let notified = server.handle(
		&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
		test,
	);
	assert_eq!(notified, None, "a notification has no answer");
	let pinged = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
	);
	assert_eq!(pinged["result"], json!({}));
	let listed = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
	);
	assert!(listed["result"].get("resultType").is_none());
	assert_eq!(
		listed["result"]["tools"].as_array().map(Vec::len),
		Some(TOOLS.len())
	);
	let called = request(
		&mut server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 3,
			"method": "tools/call",
			"params": { "name": "list", "arguments": {} },
		}),
	);
	let result = &called["result"];
	assert!(result.get("resultType").is_none());
	assert_eq!(result["isError"], false);
	assert!(
		result["structuredContent"].is_object(),
		"the revision before takes objects alone: {result}"
	);
	// An older revision's client is answered as this one.
	let older = request(
		&mut server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 4,
			"method": "initialize",
			"params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "old", "version": "1" } },
		}),
	);
	assert_eq!(older["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn requests_that_the_server_cannot_serve_get_errors() {
	let mut setup = Setup::new("mcp errors");
	let mut server = setup.server();
	let test = &mut setup.test;
	let code = |response: &Value| response["error"]["code"].as_i64();
	let garbled = server.handle("{not json", test).expect("a response");
	let garbled: Value = serde_json::from_str(&garbled).expect("JSON");
	assert_eq!(code(&garbled), Some(-32700));
	let unversioned = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
	);
	assert_eq!(code(&unversioned), Some(-32602));
	let future = request(
		&mut server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 2,
			"method": "tools/list",
			"params": { "_meta": {
				"io.modelcontextprotocol/protocolVersion": "2099-01-01",
				"io.modelcontextprotocol/clientCapabilities": {},
			} },
		}),
	);
	assert_eq!(code(&future), Some(-32022));
	assert_eq!(
		future["error"]["data"]["supported"],
		json!([CURRENT, BEFORE])
	);
	let unknown = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": { "_meta": meta() } }),
	);
	assert_eq!(code(&unknown), Some(-32601));
	let no_tool = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "purge", "arguments": {}, "_meta": meta() } }),
	);
	assert_eq!(code(&no_tool), Some(-32602), "purging is no tool");
	// A tool's own error is its result, for the AI agent to read.
	let missing = call(
		&mut server,
		test,
		"show",
		&json!({ "entity": "nobody at all" }),
	);
	assert_ne!(error_text(&missing), "");
	let wrong = call(&mut server, test, "show", &json!({}));
	assert!(error_text(&wrong).contains("entity"), "{wrong}");
	let forever = call(&mut server, test, "sync", &json!({ "wait": 100_000_000 }));
	assert!(error_text(&forever).contains("up to 60"), "{forever}");
	let flag = call(&mut server, test, "show", &json!({ "entity": "--json" }));
	assert!(
		!error_text(&flag).is_empty(),
		"an argument is never an option"
	);
}

/// A file that gives the user's passphrase, readable by its owner alone.
fn user_passphrase(setup: &Setup) -> PathBuf {
	let file = setup.test.here.join("user passphrase");
	std::fs::write(&file, "correct horse\n").expect("writes");
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
			.expect("makes it private");
	}
	file
}

#[test]
fn nib_mcp_refuses_a_library_that_does_not_allow_ai_agents_and_asks_nothing() {
	let mut setup = Setup::new("mcp refuses");
	let file = user_passphrase(&setup);
	let user = cli(&[
		"--library",
		path(&setup.user),
		"--passphrase-file",
		path(&file),
		"mcp",
		"serve",
	]);
	let refused = McpServer::open(&user, &mut setup.test);
	assert!(
		matches!(refused, Err(CliError::NotAnAiAgent)),
		"{:?}",
		refused.err()
	);
	assert_eq!(setup.test.asked, 0, "nothing is asked on a terminal");
	let agent = cli(&["--library", path(&setup.agent), "mcp", "serve"]);
	let locked = McpServer::open(&agent, &mut setup.test);
	let error = locked.err().expect("refused").to_string();
	assert!(error.contains("--passphrase-file"), "{error}");
	assert_eq!(setup.test.asked, 0);
}

#[test]
fn nothing_secret_reaches_an_ai_agent() {
	let mut setup = Setup::new("mcp secrets");
	{
		let mut library = setup.user_library();
		let mut rng = TestRng::new("secrets");
		library
			.set_up_secrets(
				&NewProtection::Passphrase("the secrets", quick()),
				now() + 500,
				&mut rng,
			)
			.expect("sets up secrets");
		drop(library);
	}
	// The user stores a password, which the AI agent's replica gets sealed.
	let user = path(&setup.user).to_owned();
	setup.test.answer(&["correct horse", "correct horse"]);
	setup
		.test
		.nib(&[
			"--library",
			&user,
			"type",
			"add",
			"local.login",
			"local.password:text:secret",
		])
		.expect("defines");
	setup
		.test
		.nib(&[
			"--library",
			&user,
			"new",
			"local.login",
			"org.nibkeep.name=Bank",
		])
		.expect("adds");
	setup
		.test
		.answer(&["correct horse", "the secrets", "hunter2", "hunter2"]);
	setup
		.test
		.nib(&[
			"--library",
			&user,
			"secret",
			"set",
			"Bank",
			"local.password",
		])
		.expect("sets the secret");
	sync_with_user(&mut setup);
	// The AI agent's replica holds every change, the sealed secret's too.
	let changes = |library: Library| library.status().expect("a status").spaces[0].changes;
	let agent = Library::open(
		&setup.agent.join(".nib"),
		&nibkeep_node::Unlocker::Passphrase("battery staple"),
	)
	.expect("opens");
	assert_eq!(changes(agent), changes(setup.user_library()));
	let mut server = setup.server();
	let test = &mut setup.test;
	let names: Vec<String> = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": meta() } }),
	)["result"]["tools"]
		.as_array()
		.expect("tools")
		.iter()
		.map(|tool| tool["name"].as_str().expect("a name").to_owned())
		.collect();
	assert!(!names.iter().any(|name| name.contains("secret")));
	for (tool, arguments) in [
		("show", json!({ "entity": "Bank" })),
		("history", json!({ "entity": "Bank" })),
		("find", json!({ "words": "hunter2" })),
		("list", json!({ "type": "local.login" })),
	] {
		let result = call(&mut server, test, tool, &arguments);
		assert!(!result.to_string().contains("hunter2"), "{tool}: {result}");
	}
	let shown = structured(&call(
		&mut server,
		test,
		"show",
		&json!({ "entity": "Bank" }),
	));
	assert!(shown.to_string().contains("Bank"), "{shown}");
}

/// The AI agent's replica syncs once with the user's, which answers.
fn sync_with_user(setup: &mut Setup) {
	let mut server = setup.server();
	let user = setup.user_library();
	let network = user.network(NetworkOptions::LOCAL).expect("starts");
	server
		.remember(network.endpoint(), &network.local_addresses())
		.expect("remembers");
	let closed = user.close();
	let test = &mut setup.test;
	std::thread::scope(|scope| {
		let answered = scope.spawn(|| {
			nibkeep_node::answer(
				&closed,
				&network,
				Some(TIMEOUT),
				&|| now() + 120_000,
				&mut TestRng::new("answering"),
			)
		});
		let synced = call(&mut server, test, "sync", &json!({ "wait": 30 }));
		structured(&synced);
		answered
			.join()
			.expect("the user's replica ran")
			.expect("answers")
			.expect("a sync came")
			.synced
			.expect("synced");
	});
}

#[test]
fn an_ai_agents_change_is_synced_once_it_is_quiet_for_a_few_seconds() {
	let mut setup = Setup::new("mcp quiet");
	let mut server = setup.server();
	// The first call syncs before it runs, and finds nobody: the user's replica is not there
	// yet.
	let added = structured(&call(
		&mut server,
		&mut setup.test,
		"new",
		&json!({ "type": "std.book", "values": ["std.title=Abaddon's Gate"] }),
	));
	let book: nibkeep_base::EntityId = added["id"]
		.as_str()
		.expect("an ID")
		.parse()
		.expect("an entity ID");
	let user = setup.user_library();
	let network = user.network(NetworkOptions::LOCAL).expect("starts");
	server
		.remember(network.endpoint(), &network.local_addresses())
		.expect("remembers");
	let closed = user.close();
	let answer_within = |patience| {
		nibkeep_node::answer(
			&closed,
			&network,
			Some(patience),
			&|| now() + 120_000,
			&mut TestRng::new("answering"),
		)
		.expect("answers")
	};
	// Right after the change, the server waits for more.
	std::thread::scope(|scope| {
		let answered = scope.spawn(|| answer_within(std::time::Duration::from_millis(500)));
		server.idle(&mut setup.test);
		assert!(
			answered.join().expect("the user's replica ran").is_none(),
			"no sync came yet"
		);
	});
	// Once the AI agent was quiet for a few seconds, it syncs, though the sync before the
	// change was less than a minute ago.
	setup.test.now += 6_000;
	std::thread::scope(|scope| {
		let answered = scope.spawn(|| answer_within(TIMEOUT));
		server.idle(&mut setup.test);
		answered
			.join()
			.expect("the user's replica ran")
			.expect("a sync came")
			.synced
			.expect("synced");
	});
	let user = setup.user_library();
	let personal = user.personal_space();
	assert!(
		user.entity(
			personal,
			book,
			nibkeep_time::Day::from_unix_millis(now(), 0).expect("a day")
		)
		.expect("reads")
		.is_some(),
		"the user's replica has the book"
	);
}

#[test]
fn an_ai_agents_changes_reach_the_user_named_as_its_own() {
	let mut setup = Setup::new("mcp reaches");
	{
		let mut server = setup.server();
		let added = structured(&call(
			&mut server,
			&mut setup.test,
			"new",
			&json!({ "type": "std.book", "values": ["std.title=Caliban's War"] }),
		));
		assert!(added["id"].is_string());
	}
	sync_with_user(&mut setup);
	let user = path(&setup.user).to_owned();
	setup.test.answer(&["correct horse"]);
	let history = setup
		.test
		.nib(&["--library", &user, "history", "Caliban's War", "std.title"])
		.expect("shows its history");
	assert!(history.contains("Ada, through Claude"), "{history}");
	setup.test.answer(&["correct horse"]);
	let devices = setup
		.test
		.nib(&["--library", &user, "device", "list"])
		.expect("lists");
	assert!(
		devices.contains("Claude on laptop (an AI agent)"),
		"{devices}"
	);
	// Nor does the AI agent's own copy pair devices, nor serve a terminal.
	let agent = path(&setup.agent).to_owned();
	let file = path(&setup.passphrase_file).to_owned();
	let paired = setup.test.nib(&[
		"--library",
		&agent,
		"--passphrase-file",
		&file,
		"device",
		"pair",
	]);
	assert!(
		matches!(
			paired,
			Err(CliError::Node(nibkeep_node::NodeError::AiAgentMayNot(_)))
		),
		"{paired:?}"
	);
}

#[test]
fn nib_device_pair_takes_an_ai_agents_name() {
	let cli = cli(&["device", "pair", "--ai-agent", "Claude"]);
	assert!(matches!(
		cli.command,
		nibkeep_cli::Command::Device {
			command: nibkeep_cli::DeviceCommand::Pair {
				ai_agent: Some(ref name),
				..
			}
		} if name == "Claude"
	));
}

#[test]
fn an_ai_agent_reads_a_tree_s_log_diffs_and_blame_and_writes_none_of_it() {
	let mut revisions = Vec::new();
	let mut setup = Setup::with("mcp trees", |library, user, rng| {
		let notes = user.join("notes");
		std::fs::create_dir_all(&notes).expect("makes a folder");
		std::fs::write(notes.join("plan.md"), "one\ntwo\n").expect("writes");
		let personal = library.personal_space();
		let started = library
			.start_tree(personal, &notes, Some("Start"), now() + 1_000, rng)
			.expect("starts");
		std::fs::write(notes.join("plan.md"), "one\nTWO\n").expect("writes");
		let second = library
			.commit_tree(
				personal,
				started.tree,
				&nibkeep_node::Commit::new(Some("Second")),
				now() + 2_000,
				rng,
			)
			.expect("commits");
		revisions.extend(started.committed.revision);
		revisions.extend(second.revision);
	});
	let mut server = setup.server();
	let test = &mut setup.test;
	let log = structured(&call(
		&mut server,
		test,
		"tree_log",
		&json!({ "tree": "notes" }),
	));
	let messages: Vec<&Value> = log
		.as_array()
		.expect("a list")
		.iter()
		.map(|entry| &entry["message"])
		.collect();
	assert_eq!(messages, [&json!("Second"), &json!("Start")]);
	let blame = structured(&call(
		&mut server,
		test,
		"tree_blame",
		&json!({ "tree": "notes", "path": "plan.md" }),
	));
	assert_eq!(blame[1]["text"], "TWO");
	assert_eq!(blame[1]["revision"], revisions[1].to_string());
	let diff = structured(&call(
		&mut server,
		test,
		"tree_diff",
		&json!({
			"tree": "notes",
			"from": revisions[0].to_string(),
			"to": revisions[1].to_string(),
		}),
	));
	let patch = diff[0]["patch"].as_str().expect("a patch");
	assert!(patch.contains("+TWO"), "{patch}");
	// The tree is named, as the AI agent's copy holds no folder of it.
	let unnamed = call(&mut server, test, "tree_log", &json!({}));
	assert_eq!(unnamed["isError"], true);
}

/// The arguments of the example in a tool's description: the JSON between the backticks
/// after "such as", written as the tool takes them.
fn example(tool: &Value) -> Value {
	let description = tool["description"].as_str().expect("a description");
	let example = description
		.split("such as `")
		.nth(1)
		.and_then(|rest| rest.split('`').next())
		.expect("an example");
	serde_json::from_str(example)
		.unwrap_or_else(|_| panic!("the example is the tool's arguments: {example}"))
}

/// The names of the entities that a tool listed.
fn listed_names(listed: &Value) -> Vec<String> {
	listed
		.as_array()
		.or_else(|| listed["entities"].as_array())
		.expect("entities")
		.iter()
		.map(|entity| entity["name"].as_str().expect("a name").to_owned())
		.collect()
}

#[test]
fn the_tools_are_described_by_their_own_parameters() {
	let mut setup = Setup::new("mcp described");
	let mut server = setup.server();
	let test = &mut setup.test;
	let listed = request(
		&mut server,
		test,
		&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": meta() } }),
	);
	let tools = listed["result"]["tools"].as_array().expect("tools").clone();
	let tool = |name: &str| {
		tools
			.iter()
			.find(|tool| tool["name"] == name)
			.expect("the tool")
			.clone()
	};
	for tool in &tools {
		let name = tool["name"].as_str().expect("a name");
		let description = tool["description"].as_str().expect("a description");
		assert!(
			!description.contains("`--"),
			"{name} names no option of nib's: {description}"
		);
		for (parameter, schema) in tool["inputSchema"]["properties"]
			.as_object()
			.expect("parameters")
		{
			let help = schema["description"].as_str().expect("a description");
			assert!(!help.is_empty(), "{name} describes {parameter}");
			assert!(
				!help.contains("`--") && !help.contains("two minutes"),
				"{name}, {parameter}: {help}"
			);
		}
	}
	let status = tool("status");
	assert!(
		!status["description"]
			.as_str()
			.is_some_and(|text| text.contains("tree's copy")),
		"the library's status, not a tree's: {status}"
	);
	let edit = tool("edit").to_string();
	assert!(
		edit.contains("`date`") && edit.contains("`form`"),
		"options named as parameters: {edit}"
	);
	let added = structured(&call(&mut server, test, "new", &example(&tool("new"))));
	let book = added["id"].as_str().expect("an ID").to_owned();
	structured(&call(
		&mut server,
		test,
		"new",
		&json!({ "type": "series", "values": ["name=The Expanse"] }),
	));
	structured(&call(
		&mut server,
		test,
		"edit",
		&json!({ "entity": book, "changes": ["in-series=The Expanse", "number-in-series=1"] }),
	));
	for (name, number) in [("Caliban's War", "2"), ("Drive", "0.1")] {
		structured(&call(
			&mut server,
			test,
			"new",
			&json!({
				"type": "book",
				"values": [
					format!("name={name}"),
					"in-series=The Expanse",
					format!("number-in-series={number}"),
				],
			}),
		));
	}
	structured(&call(
		&mut server,
		test,
		"new",
		&json!({ "type": "book", "values": ["name=Dune"] }),
	));
	let listed = structured(&call(&mut server, test, "list", &example(&tool("list"))));
	assert_eq!(
		listed_names(&listed),
		["Drive", "Leviathan Wakes", "Caliban's War"],
		"{listed}"
	);
}

#[test]
fn nib_mcp_works_on_the_persons_library_as_the_client_once_allowed() {
	let mut setup = Setup::new("mcp on the person's library");
	let file = user_passphrase(&setup);
	let (user, file) = (path(&setup.user).to_owned(), path(&file).to_owned());
	let printed = setup
		.test
		.nib(&[
			"--library",
			&user,
			"--passphrase-file",
			&file,
			"mcp",
			"enable",
			"--print",
		])
		.expect("enables");
	assert!(
		printed.contains("/opt/nib/bin/nib mcp serve --library") && printed.contains("mcpServers"),
		"{printed}"
	);
	let mut server = McpServer::open(
		&cli(&[
			"--library",
			&user,
			"--passphrase-file",
			&file,
			"mcp",
			"serve",
			"--wait",
			"0",
		]),
		&mut setup.test,
	)
	.expect("serves");
	let test = &mut setup.test;
	let added = structured(&call(
		&mut server,
		test,
		"new",
		&json!({ "type": "std.book", "values": ["name=Leviathan Wakes"] }),
	));
	let id = added["id"].as_str().expect("an ID").to_owned();
	let history = structured(&call(
		&mut server,
		test,
		"history",
		&json!({ "entity": id, "field": "name" }),
	));
	let author = &history["fields"][0]["values"][0]["author"];
	assert_eq!(author["name"], "Ada");
	assert_eq!(
		author["ai_agent"], "a test",
		"named as the client named itself"
	);
	let synced = structured(&call(&mut server, test, "sync", &json!({})));
	assert!(synced["synced_by"].is_string(), "{synced}");
	// Another name in a later request is refused.
	let renamed = request(
		&mut server,
		test,
		&json!({
			"jsonrpc": "2.0",
			"id": 9,
			"method": "tools/call",
			"params": {
				"name": "status",
				"arguments": {},
				"_meta": {
					"io.modelcontextprotocol/protocolVersion": CURRENT,
					"io.modelcontextprotocol/clientCapabilities": {},
					"io.modelcontextprotocol/clientInfo": { "name": "another", "version": "1" },
				},
			},
		}),
	);
	assert!(renamed["error"]["message"].is_string(), "{renamed}");
	// What an AI agent may not do stays refused.
	let types = call(
		&mut server,
		test,
		"edit",
		&json!({ "entity": id, "changes": ["org.nibkeep.endpoint=AAAA"] }),
	);
	assert_eq!(types["isError"], true, "{types}");
	drop(server);
	let library = setup.user_library();
	let replicas = library.replicas().expect("lists");
	let own = replicas.iter().find(|replica| replica.this).expect("own");
	let agents: Vec<_> = replicas.iter().filter(|replica| replica.ai_agent).collect();
	assert!(
		agents
			.iter()
			.any(|agent| agent.name.as_deref() == Some("a test") && agent.device == own.device),
		"the client's key, on the person's device: {replicas:?}"
	);
}

#[cfg(unix)]
#[test]
fn nib_mcp_enable_tells_the_ai_clients_and_disable_takes_the_server_out() {
	use std::os::unix::fs::PermissionsExt;
	let mut setup = Setup::new("mcp enable");
	let file = user_passphrase(&setup);
	let log = setup.test.here.join("clients.log");
	for program in ["claude", "codex"] {
		let script = setup.test.here.join(program);
		std::fs::write(
			&script,
			format!("#!/bin/sh\necho \"{program} $*\" >> '{}'\n", log.display()),
		)
		.expect("writes");
		std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
			.expect("makes it run");
		setup.test.ai_clients.push((program, script));
	}
	let (user, file) = (path(&setup.user).to_owned(), path(&file).to_owned());
	let library = [
		"--library",
		user.as_str(),
		"--passphrase-file",
		file.as_str(),
	];
	let nib = |test: &mut Test, words: &[&str]| {
		let mut arguments = library.to_vec();
		arguments.extend_from_slice(words);
		test.nib(&arguments)
	};
	let enabled = nib(&mut setup.test, &["mcp", "enable"]).expect("enables");
	assert!(
		enabled.contains("Told Claude Code of the server nibkeep-user")
			&& enabled.contains("Told Codex of the server nibkeep-user"),
		"{enabled}"
	);
	let told = std::fs::read_to_string(&log).expect("reads");
	let line = format!("/opt/nib/bin/nib mcp serve --library {user}");
	assert!(
		told.contains(&format!(
			"claude mcp add --scope user nibkeep-user -- {line}"
		)) && told.contains(&format!("codex mcp add nibkeep-user -- {line}")),
		"{told}"
	);
	let mut server = McpServer::open(
		&cli(&[
			"--library",
			&user,
			"--passphrase-file",
			&file,
			"mcp",
			"serve",
		]),
		&mut setup.test,
	)
	.expect("serves");
	structured(&call(
		&mut server,
		&mut setup.test,
		"new",
		&json!({ "type": "std.book", "values": ["name=Caliban's War"] }),
	));
	drop(server);
	std::fs::remove_file(&log).expect("removes");
	let disabled = nib(&mut setup.test, &["mcp", "disable"]).expect("disables");
	assert!(
		disabled.contains("Revoked the keys of a test")
			&& disabled.contains("Took the server nibkeep-user out of Claude Code"),
		"{disabled}"
	);
	let told = std::fs::read_to_string(&log).expect("reads");
	assert!(
		told.contains("claude mcp remove --scope user nibkeep-user")
			&& told.contains("codex mcp remove nibkeep-user")
			&& !told.contains(" add "),
		"{told}"
	);
	let refused = McpServer::open(
		&cli(&[
			"--library",
			&user,
			"--passphrase-file",
			&file,
			"mcp",
			"serve",
		]),
		&mut setup.test,
	);
	assert!(matches!(refused, Err(CliError::NotAnAiAgent)));
	let unknown = nib(&mut setup.test, &["mcp", "enable", "--client", "cursor"]);
	assert!(
		matches!(unknown, Err(CliError::UnknownAiClient(_))),
		"{unknown:?}"
	);
}
