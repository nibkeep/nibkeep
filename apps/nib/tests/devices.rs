//! `nib device` and `nib sync` in one library, without another device to reach.

use crate::common;

use crate::common::library;
use clap::Parser;
use nibkeep_cli::{Cli, CliError};
use nibkeep_node::NodeError;

#[test]
fn a_new_library_lists_this_device_alone() {
	let mut test = library("devices listed");
	let listed = test.nib(&["device", "list"]).expect("lists");
	let lines: Vec<&str> = listed.lines().collect();
	assert_eq!(lines.len(), 1, "{listed}");
	assert!(lines[0].ends_with("  nib on laptop (this one)"), "{listed}");
	let json = test.json(&["device", "list"]);
	assert_eq!(json[0]["name"], "nib");
	assert_eq!(json[0]["device_name"], "laptop");
	assert_eq!(json[0]["this"], true);
}

#[test]
fn a_device_is_marked_as_an_archive_node_and_away_from_home() {
	let mut test = library("devices marked");
	assert_eq!(
		test.nib(&["device", "mark", "laptop"]).expect("says"),
		"laptop: an everyday device, at home\n"
	);
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--archive", "--away"])
			.expect("marks"),
		"laptop: an archive node, away from home, waits 30 days before purging\n"
	);
	let listed = test.nib(&["device", "list"]).expect("lists");
	assert!(
		listed.ends_with("  nib on laptop (this one; an archive node; away from home)\n"),
		"{listed}"
	);
	let json = test.json(&["device", "list"]);
	assert_eq!(
		(&json[0]["role"], &json[0]["away"]),
		(&"archive".into(), &true.into())
	);
	// Marks stay until changed, one at a time.
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--home"])
			.expect("marks"),
		"laptop: an archive node, at home, waits 30 days before purging\n"
	);
	let json = test.json(&["device", "mark", "laptop", "--everyday"]);
	assert_eq!(
		(&json["role"], &json["away"]),
		(&"everyday".into(), &false.into())
	);
	assert!(
		Cli::try_parse_from(["nib", "device", "mark", "laptop", "--archive", "--everyday"])
			.is_err()
	);
	assert!(matches!(
		test.nib(&["device", "mark", "toaster", "--archive"]),
		Err(CliError::Node(NodeError::WhichDevice(_)))
	));
}

#[test]
fn a_device_keeps_up_to_the_size_it_is_given() {
	let mut test = library("devices keep");
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--keeps", "20GB"])
			.expect("marks"),
		"laptop: an everyday device, at home, keeps up to 20.0 GB of files\n"
	);
	let json = test.json(&["device", "mark", "laptop"]);
	assert_eq!(json["keeps"], 20_000_000_000_u64);
	// An archive node keeps every file, whatever its size.
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--archive"])
			.expect("marks"),
		"laptop: an archive node, at home, waits 30 days before purging\n"
	);
	assert_eq!(
		test.nib(&[
			"device",
			"mark",
			"laptop",
			"--everyday",
			"--keeps",
			"default"
		])
		.expect("marks"),
		"laptop: an everyday device, at home\n"
	);
	assert_eq!(
		test.json(&["device", "mark", "laptop"])["keeps"],
		serde_json::Value::Null
	);
	assert!(Cli::try_parse_from(["nib", "device", "mark", "laptop", "--keeps", "lots"]).is_err());
}

#[test]
fn sync_without_another_device_says_so() {
	let mut test = library("sync alone");
	let synced = test.nib(&["sync", "--wait", "0"]);
	assert!(
		matches!(synced, Err(CliError::Node(NodeError::NoOtherReplicas))),
		"{synced:?}"
	);
	assert_eq!(
		synced.err().map(|error| error.to_string()).as_deref(),
		Some("this library has no other device to sync with")
	);
}

#[test]
fn a_pairing_code_is_eight_digits() {
	for code in ["12-345-678", "12345678", "12 345 678"] {
		assert!(
			Cli::try_parse_from(["nib", "init", "--join", code]).is_ok(),
			"{code}"
		);
	}
	for code in ["1234567", "02-345-678", "12-345-67a"] {
		let refused = Cli::try_parse_from(["nib", "init", "--join", code]);
		assert!(refused.is_err(), "{code}");
	}
	// Joining makes a copy of a library, which has its user already.
	assert!(Cli::try_parse_from(["nib", "init", "--join", "12345678", "--name", "Ada"]).is_err());
	// Without a name or a code, init makes a tree in a library, as the tests of trees show.
	assert!(Cli::try_parse_from(["nib", "init"]).is_ok());
}

#[test]
fn joining_where_a_library_is_refuses_before_looking() {
	let mut test = library("join existing");
	let joined = test.nib(&["init", "--join", "12-345-678"]);
	assert!(
		matches!(joined, Err(CliError::Node(NodeError::Exists))),
		"{joined:?}"
	);
}

#[test]
fn this_device_cannot_be_revoked_from_itself() {
	let mut test = library("revoke itself");
	let revoked = test.nib(&["device", "revoke", "laptop", "--yes"]);
	assert_eq!(
		revoked.err().map(|error| error.to_string()).as_deref(),
		Some("this device cannot revoke itself; revoke it from another of yours")
	);
	let unknown = test.nib(&["device", "revoke", "phone"]);
	assert_eq!(
		unknown.err().map(|error| error.to_string()).as_deref(),
		Some("no device or replica of yours is phone")
	);
	// Nothing changed: the device is listed as before.
	let listed = test.nib(&["device", "list"]).expect("lists");
	assert!(listed.ends_with("  nib on laptop (this one)\n"), "{listed}");
	assert_eq!(test.json(&["device", "list"])[0]["revoked"], false);
}

#[test]
fn nothing_is_set_aside_in_a_new_library() {
	let mut test = library("nothing set aside");
	assert_eq!(
		test.nib(&["device", "review"]).expect("reviews"),
		"No changes are set aside.\n"
	);
	assert_eq!(test.json(&["device", "review"]), serde_json::json!([]));
	assert_eq!(
		test.nib(&["device", "accept", "laptop", "--yes"])
			.expect("accepts"),
		"None of its changes are set aside.\n"
	);
}

#[test]
fn the_secrets_go_with_a_device_paired_only_as_asked() {
	let parses = |arguments: &[&str]| {
		nibkeep_cli::Cli::try_parse_from(std::iter::once("nib").chain(arguments.iter().copied()))
			.is_ok()
	};
	assert!(parses(&["device", "pair", "--give-secrets"]));
	assert!(parses(&["init", "--join", "37-482-177", "--secrets"]));
	// An AI agent never gets them, and asking needs a device to ask.
	assert!(!parses(&[
		"device",
		"pair",
		"--give-secrets",
		"--ai-agent",
		"Claude"
	]));
	assert!(!parses(&["init", "--name", "Ada", "--secrets"]));
}

/// Holds the sync role of the library in `directory`, whose keys the mock keystore holds,
/// on `port`, with the other replica at `other`, until `stop` is set.
fn hold(
	directory: &std::path::Path,
	port: u16,
	other: ([u8; 32], u16),
	stop: &std::sync::atomic::AtomicBool,
	now: u64,
) {
	use std::net::{Ipv4Addr, SocketAddr};
	let closed = nibkeep_node::Library::open(directory, &nibkeep_node::Unlocker::Keystore)
		.expect("opens")
		.close();
	let role = nibkeep_node::Role::take(&closed)
		.expect("takes")
		.expect("free");
	let clock = move || now + 60_000;
	let holding = nibkeep_node::Holding {
		program: "a test",
		network: nibkeep_node::NetworkOptions {
			discovery: false,
			port: Some(port),
			accept: true,
		},
		known: vec![(
			other.0,
			vec![SocketAddr::from((Ipv4Addr::LOCALHOST, other.1))],
		)],
		quiet: std::time::Duration::from_millis(200),
		appear_wait: std::time::Duration::from_millis(200),
		check: nibkeep_node::Pace::DEFAULT,
		stop,
		now: &clock,
	};
	nibkeep_node::hold(
		&closed,
		role,
		&holding,
		&|| nibkeep_testkit::TestRng::new("a holder"),
		&mut |_| {},
	)
	.expect("holds");
}

/// A UDP port that is free now.
fn free_port() -> u16 {
	std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
		.and_then(|socket| socket.local_addr())
		.expect("binds")
		.port()
}

#[test]
fn nib_sync_asks_the_program_that_syncs_this_device() {
	use std::sync::atomic::{AtomicBool, Ordering};
	let mut test = library("asks the holder");
	let laptop = test.here.join(".nib");
	let desk = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "asked desk");
	std::fs::remove_dir(&desk).expect("removes");
	let mut sponsor =
		nibkeep_node::Library::open(&laptop, &nibkeep_node::Unlocker::Keystore).expect("opens");
	common::pair_on_localhost(
		&mut sponsor,
		&desk,
		("desk", None),
		&nibkeep_node::NewProtection::Keystore {
			service: nibkeep_node::KEYSTORE_SERVICE,
		},
		test.now,
		&mut test.rng,
	);
	let endpoints = [
		sponsor.endpoint(),
		nibkeep_node::Library::open(&desk, &nibkeep_node::Unlocker::Keystore)
			.expect("opens")
			.endpoint(),
	];
	drop(sponsor);
	let ports = [free_port(), free_port()];
	let stop = AtomicBool::new(false);
	let now = test.now;
	std::thread::scope(|scope| {
		scope.spawn(|| hold(&laptop, ports[0], (endpoints[1], ports[1]), &stop, now));
		scope.spawn(|| hold(&desk, ports[1], (endpoints[0], ports[0]), &stop, now));
		// Asked only once the holder holds the role: before, nib sync would sync itself.
		let records = nibkeep_node::Library::open(&laptop, &nibkeep_node::Unlocker::Keystore)
			.expect("opens")
			.sync_records(nibkeep_testkit::TestRng::new("a reader"))
			.expect("records");
		let waited = std::time::Instant::now();
		while records.holder().expect("reads").is_none() {
			assert!(
				waited.elapsed() < std::time::Duration::from_secs(30),
				"held"
			);
			std::thread::sleep(std::time::Duration::from_millis(50));
		}
		let synced = test.nib(&["sync", "--wait", "5"]);
		let listed = test.nib(&["device", "list"]);
		stop.store(true, Ordering::SeqCst);
		let synced = synced.expect("syncs");
		assert!(synced.contains("nib on desk: synced"), "{synced}");
		let listed = listed.expect("lists");
		assert!(
			listed.contains("on the network; synced 2026-10-12"),
			"{listed}"
		);
	});
}

#[test]
fn an_archive_node_waits_the_days_it_is_given_before_purging() {
	let mut test = library("devices wait");
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--archive", "--waits", "45"])
			.expect("marks"),
		"laptop: an archive node, at home, waits 45 days before purging\n"
	);
	let json = test.json(&["device", "mark", "laptop", "--waits", "none"]);
	assert_eq!(
		(&json["waits"], &json["waits_days"]),
		(&0.into(), &0.into())
	);
	assert_eq!(
		test.nib(&["device", "mark", "laptop", "--waits", "default"])
			.expect("marks"),
		"laptop: an archive node, at home, waits 30 days before purging\n"
	);
	// An everyday device never waits, whatever it was given.
	let json = test.json(&["device", "mark", "laptop", "--everyday", "--waits", "10d"]);
	assert_eq!(
		(&json["waits"], &json["waits_days"]),
		(&10.into(), &0.into())
	);
	for wrong in ["366", "soon", "-1"] {
		assert!(
			Cli::try_parse_from(["nib", "device", "mark", "laptop", "--waits", wrong]).is_err(),
			"{wrong}"
		);
	}
}
