//! Two endpoints on this computer: messages each way over one connection, bound to it.

use std::collections::BTreeSet;
use std::time::Duration;

use nibkeep_net::{NetError, Network, Options, Protocol};
use nibkeep_sync::Transport;

const TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn messages_go_both_ways_over_a_bound_connection() {
	let server = Network::start(&[1; 32], Options::LOCAL).expect("starts");
	let client = Network::start(&[2; 32], Options::LOCAL).expect("starts");
	assert_ne!(server.endpoint(), client.endpoint());
	let addresses = server.local_addresses();
	let server_id = server.endpoint();
	let client_id = client.endpoint();
	std::thread::scope(|scope| {
		let accepted = scope.spawn(|| {
			let mut connection = server
				.accept(Some(TIMEOUT))
				.expect("accepts")
				.expect("a connection");
			assert_eq!(connection.protocol(), Protocol::Sync);
			assert_eq!(connection.peer(), client_id);
			assert_eq!(connection.own_endpoint(), server_id);
			let first = connection.receive().expect("a message");
			assert_eq!(first, b"hello");
			let large = connection.receive().expect("a large message");
			assert_eq!(large.len(), 3 * 1024 * 1024);
			connection.send(b"goodbye").expect("sends");
			let binding = connection.binding();
			connection.close();
			binding
		});
		let mut connection = client
			.connect_at(server_id, &addresses, Protocol::Sync, TIMEOUT)
			.expect("connects");
		assert_eq!(connection.peer(), server_id);
		assert_eq!(connection.own_endpoint(), client_id);
		connection.send(b"hello").expect("sends");
		connection
			.send(&vec![7; 3 * 1024 * 1024])
			.expect("sends a large message");
		assert_eq!(connection.receive().expect("an answer"), b"goodbye");
		let binding = connection.binding();
		connection.close();
		let server_binding = accepted.join().expect("the server ran");
		assert_eq!(binding, server_binding, "both sides are bound alike");
	});
}

#[test]
fn another_connection_has_another_binding() {
	let server = Network::start(&[3; 32], Options::LOCAL).expect("starts");
	let client = Network::start(&[4; 32], Options::LOCAL).expect("starts");
	let addresses = server.local_addresses();
	let mut bindings = Vec::new();
	for _ in 0..2 {
		std::thread::scope(|scope| {
			let accepted = scope.spawn(|| {
				let mut connection = server
					.accept(Some(TIMEOUT))
					.expect("accepts")
					.expect("a connection");
				let _ = connection.receive();
				connection.close();
			});
			let mut connection = client
				.connect_at(server.endpoint(), &addresses, Protocol::Pair, TIMEOUT)
				.expect("connects");
			connection.send(b"x").expect("sends");
			bindings.push(connection.binding());
			connection.close();
			accepted.join().expect("ran");
		});
	}
	assert_ne!(bindings[0], bindings[1]);
}

#[test]
fn accepting_gives_up_after_its_time() {
	let server = Network::start(&[5; 32], Options::LOCAL).expect("starts");
	assert!(
		server
			.accept(Some(Duration::from_millis(200)))
			.expect("waits")
			.is_none()
	);
}

/// Discovery on the network by mDNS, which a sandbox or a CI runner may not allow: run with
/// `--ignored`.
#[test]
#[ignore = "needs multicast on the network"]
fn endpoints_find_each_other_by_what_they_announce() {
	let one = Network::start(
		&[6; 32],
		Options {
			discovery: true,
			port: None,
			accept: true,
		},
	)
	.expect("starts");
	let other = Network::start(
		&[7; 32],
		Options {
			discovery: true,
			port: None,
			accept: true,
		},
	)
	.expect("starts");
	one.announce(Some("nibkeep-pair-42")).expect("announces");
	let found = other
		.find_announcing("nibkeep-pair-42", &BTreeSet::new(), TIMEOUT)
		.expect("finds it");
	assert_eq!(found, one.endpoint());
	std::thread::scope(|scope| {
		let accepted = scope.spawn(|| {
			let mut connection = one.accept(Some(TIMEOUT)).expect("accepts").expect("one");
			assert_eq!(connection.receive().expect("a message"), b"found");
			connection.close();
		});
		let mut connection = other
			.connect(found, Protocol::Pair, TIMEOUT)
			.expect("connects through discovery");
		connection.send(b"found").expect("sends");
		connection.close();
		accepted.join().expect("ran");
	});
}

#[test]
fn an_endpoint_takes_its_port_or_the_next_free_one() {
	let port = 53_391;
	let first = Network::start(
		&[8; 32],
		Options {
			discovery: false,
			port: Some(port),
			accept: true,
		},
	)
	.expect("starts");
	assert!(
		first
			.local_addresses()
			.iter()
			.any(|address| address.port() == port),
		"{:?}",
		first.local_addresses()
	);
	let second = Network::start(
		&[9; 32],
		Options {
			discovery: false,
			port: Some(port),
			accept: true,
		},
	)
	.expect("starts all the same");
	assert!(
		second
			.local_addresses()
			.iter()
			.all(|address| address.port() != port),
		"{:?}",
		second.local_addresses()
	);
	assert_eq!(first.port(), Some(port));
	assert_eq!(first.port_refused(), None);
	// The next of the ten ports from the one asked for, so that one firewall rule for them
	// lets both through.
	assert!(
		second
			.port()
			.is_some_and(|taken| (port + 1..port + nibkeep_net::PORTS).contains(&taken)),
		"{:?}",
		second.port()
	);
	assert_eq!(second.port_refused().map(|(asked, _)| asked), Some(port));
}

#[test]
fn an_endpoint_that_only_connects_refuses_connections_at_once() {
	let connecting = Network::start(
		&[10; 32],
		Options {
			accept: false,
			..Options::LOCAL
		},
	)
	.expect("starts");
	let other = Network::start(&[11; 32], Options::LOCAL).expect("starts");
	let started = std::time::Instant::now();
	let refused = other.connect_at(
		connecting.endpoint(),
		&connecting.local_addresses(),
		Protocol::Sync,
		TIMEOUT,
	);
	assert!(
		matches!(refused, Err(NetError::Refused)),
		"{:?}",
		refused.err()
	);
	assert!(
		started.elapsed() < Duration::from_secs(10),
		"{:?}",
		started.elapsed()
	);
	assert!(connecting.accept(Some(Duration::from_millis(10))).is_err());
}

#[test]
fn a_connection_that_stalls_holds_up_no_other() {
	let server = Network::start(&[12; 32], Options::LOCAL).expect("starts");
	let addresses = server.local_addresses();
	// One client connects and never opens its stream; another connects after it.
	let stalled = Network::start(&[13; 32], Options::LOCAL).expect("starts");
	let _silent = stalled
		.connect_at(server.endpoint(), &addresses, Protocol::Sync, TIMEOUT)
		.expect("connects");
	let client = Network::start(&[14; 32], Options::LOCAL).expect("starts");
	std::thread::scope(|scope| {
		let accepted = scope.spawn(|| {
			let started = std::time::Instant::now();
			let mut connection = server
				.accept(Some(TIMEOUT))
				.expect("accepts")
				.expect("a connection");
			assert_eq!(connection.receive().expect("a message"), b"here");
			connection.close();
			started.elapsed()
		});
		let mut connection = client
			.connect_at(server.endpoint(), &addresses, Protocol::Sync, TIMEOUT)
			.expect("connects");
		connection.send(b"here").expect("sends");
		connection.close();
		let waited = accepted.join().expect("ran");
		assert!(waited < Duration::from_secs(5), "{waited:?}");
	});
}

#[test]
fn a_search_for_what_others_announce_passes_by_those_tried() {
	let options = Options {
		discovery: false,
		port: None,
		accept: false,
	};
	let looking = Network::start(&[11; 32], options).expect("starts");
	let mut found: Vec<[u8; 32]> = [12, 13]
		.into_iter()
		.map(|byte| {
			Network::start(&[byte; 32], options)
				.expect("starts")
				.endpoint()
		})
		.collect();
	found.sort_unstable();
	for endpoint in &found {
		looking.remember(*endpoint, &[], Some("nibkeep-pair-42"));
	}
	let mut tried = BTreeSet::new();
	for endpoint in &found {
		let next = looking
			.find_announcing("nibkeep-pair-42", &tried, TIMEOUT)
			.expect("finds one");
		assert_eq!(next, *endpoint, "the lowest ID first");
		tried.insert(next);
	}
	assert!(matches!(
		looking.find_announcing("nibkeep-pair-42", &tried, Duration::from_millis(200)),
		Err(NetError::Timeout)
	));
}
