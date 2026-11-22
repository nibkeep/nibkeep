//! An AI agent works on the person's library through `nib mcp`, on a replica of its own, as
//! its client starts it: over standard input and output, by the current revision of the
//! protocol. What it adds reaches the person's computer that waits, a few seconds later and
//! while it still works, named as the AI agent's; and when its client ends, `nib mcp` ends.

use std::time::{Duration, Instant};

use nibkeep_end_to_end::{EndToEnd, Running, str};
use serde_json::{Value, json};

use crate::common::pairing_alone;

/// The revision of the protocol that the client speaks.
const REVISION: &str = "2026-07-28";

/// What every request of the current revision tells about its client.
fn meta() -> Value {
	json!({
		"io.modelcontextprotocol/protocolVersion": REVISION,
		"io.modelcontextprotocol/clientCapabilities": {},
		"io.modelcontextprotocol/clientInfo": { "name": "a test", "version": "1" },
	})
}

/// Sends a request and waits for its answer, a line of JSON with its ID.
fn request(server: &mut Running, id: u64, method: &str, params: &Value) -> Value {
	let mut params = params.clone();
	params["_meta"] = meta();
	server.send(
		&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
	);
	server.wait_until(&format!("the answer to request {id}"), |printed| {
		printed
			.lines()
			.filter_map(|line| serde_json::from_str::<Value>(line).ok())
			.find(|answer| answer["id"] == id)
	})
}

/// The JSON that a tool's result holds, checking that its text says the same.
fn structured(answer: &Value) -> Value {
	let result = &answer["result"];
	assert_eq!(result["isError"], false, "{answer}");
	let text = result["content"][0]["text"].as_str().expect("text");
	let parsed: Value = serde_json::from_str(text).expect("the text is JSON");
	assert_eq!(parsed, result["structuredContent"]);
	parsed
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn an_ai_agents_changes_reach_the_person_while_it_works() {
	let test = EndToEnd::new("ai agent");
	let laptop = test.device("laptop");
	laptop
		.nib(&[
			"init",
			"--name",
			"Ada",
			"--device",
			"laptop",
			"--languages",
			"en",
		])
		.ok();
	let agent = test.device("agent");
	let alone = pairing_alone();
	let mut pairing = laptop.start(&["device", "pair", "--ai-agent", "Claude"]);
	let code = pairing.wait_for(r"\d\d-\d{3}-\d{3}");
	agent
		.nib(&["init", "--join", &code, "--device", "laptop"])
		.ok()
		.stdout_eq(str![[r#"
Looking for the device that shows the code…
Joined the library: sent nothing, received 11 changes.
Ada, on laptop, in [ROOT]/agent/library
Personal: 7 entities, 11 changes

"#]]);
	pairing.finish().ok().stdout_eq(str![[r#"
On the new device, in the folder for the library, run:

    nib init --join [CODE]

The code works for 10 minutes; 3 wrong tries end it. Both devices must be on the same network. Waiting…
Paired the AI agent Claude on laptop: serve it the library with nib mcp serve there.
Synced with it: sent 11 changes, received nothing.

"#]]);
	drop(alone);

	// The laptop waits, as the person's computer does, while the AI agent works.
	let serving = laptop.start(&["sync", "--serve"]);
	let mut server = agent.start_with_input(&["mcp", "serve"]);
	let discovered = request(&mut server, 1, "server/discover", &json!({}));
	assert_eq!(
		discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "nib",
		"{discovered}"
	);
	let added = structured(&request(
		&mut server,
		2,
		"tools/call",
		&json!({
			"name": "new",
			"arguments": { "type": "person", "values": ["name=Katherine Johnson"] },
		}),
	));
	let id = added["id"].as_str().expect("an ID").to_owned();

	// A few seconds later, with no other request, it reaches the laptop: well before the
	// minute after which a server that found nobody looks again.
	let started = Instant::now();
	loop {
		let people = laptop.nib(&["--json", "list", "person"]).ok().json();
		let found = people
			.as_array()
			.expect("a list")
			.iter()
			.any(|person| person["id"] == id.as_str());
		if found {
			break;
		}
		assert!(
			started.elapsed() < Duration::from_secs(30),
			"the AI agent's change did not reach the laptop: {people}"
		);
		std::thread::sleep(Duration::from_millis(500));
	}
	laptop
		.nib(&["history", &id, "org.nibkeep.name"])
		.ok()
		.stdout_eq(str![[r#"
Katherine Johnson
  org.nibkeep.name
    [TIME]  Ada, through Claude  Katherine Johnson

"#]]);

	// Its client ends, and so does it.
	server.close_input();
	server.finish().ok().stderr_eq("");
	let _ = serving.kill();
}
