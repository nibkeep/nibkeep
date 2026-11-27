//! Of WASI, a plugin has clocks and random numbers; it starts though it imports files or
//! the network, and stops if it calls them.

use nibkeep_plugins::{Error, Stop};

use super::{EXECUTIONS, LIMITS, call, host, start};

/// A plugin that reads WASI's random numbers and its monotonic clock.
const RANDOM_AND_CLOCK: &str = r#"(component
	(import "wasi:random/random@0.2.12" (instance $random
		(export "get-random-u64" (func (result u64)))))
	(import "wasi:clocks/monotonic-clock@0.2.12" (instance $clock
		(export "now" (func (result u64)))))
	(alias export $random "get-random-u64" (func $random_u64))
	(alias export $clock "now" (func $now))
	(core func $random_u64_core (canon lower (func $random_u64)))
	(core func $now_core (canon lower (func $now)))
	(core module $m
		(import "wasi" "random" (func $random (result i64)))
		(import "wasi" "now" (func $now (result i64)))
		(func (export "random") (result i64) call $random)
		(func (export "now") (result i64) call $now))
	(core instance $wasi
		(export "random" (func $random_u64_core))
		(export "now" (func $now_core)))
	(core instance $i (instantiate $m (with "wasi" (instance $wasi))))
	(func (export "random") (result u64) (canon lift (core func $i "random")))
	(func (export "now") (result u64) (canon lift (core func $i "now"))))"#;

/// A plugin that imports `interface` of WASI, with a function `name` that it calls.
///
/// The function's type is simpler than WASI's own: had the host given the plugin that
/// interface, the plugin would not even start, its type being another.
fn importing(interface: &str, name: &str) -> String {
	format!(
		r#"(component
	(import "{interface}" (instance $wasi
		(export "{name}" (func (result u32)))))
	(alias export $wasi "{name}" (func $imported))
	(core func $imported_core (canon lower (func $imported)))
	(core module $m
		(import "wasi" "imported" (func $imported (result i32)))
		(func (export "reach") (result i32) call $imported))
	(core instance $wasi_core (export "imported" (func $imported_core)))
	(core instance $i (instantiate $m (with "wasi" (instance $wasi_core))))
	(func (export "reach") (result u32) (canon lift (core func $i "reach"))))"#
	)
}

#[test]
fn imports_a_plugin_reads_clocks_and_random_numbers() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let mut running = start(&host, RANDOM_AND_CLOCK, LIMITS).expect("started");
		let (first,): (u64,) = call(&mut running, "random", ()).expect("a number");
		let (second,): (u64,) = call(&mut running, "random", ()).expect("a number");
		assert_ne!(first, second, "{execution:?}");
		let (earlier,): (u64,) = call(&mut running, "now", ()).expect("a time");
		let (later,): (u64,) = call(&mut running, "now", ()).expect("a time");
		assert!(later >= earlier, "{execution:?}");
	}
}

#[test]
fn imports_a_plugin_that_reaches_for_files_or_the_network_is_stopped() {
	let reaching = [
		("wasi:filesystem/preopens@0.2.12", "get-directories"),
		("wasi:filesystem/types@0.2.12", "filesystem-error-code"),
		("wasi:sockets/instance-network@0.2.12", "instance-network"),
		("wasi:sockets/tcp-create-socket@0.2.12", "create-tcp-socket"),
		("wasi:sockets/ip-name-lookup@0.2.12", "resolve-addresses"),
		("wasi:cli/exit@0.2.12", "exit"),
	];
	for execution in EXECUTIONS {
		let host = host(execution);
		for (interface, name) in reaching {
			let mut running = start(&host, &importing(interface, name), LIMITS).expect("started");
			let reached: Result<(u32,), Error> = call(&mut running, "reach", ());
			let Err(Error::Stopped(Stop::Trap(message))) = reached else {
				panic!("{execution:?}, {interface}: {reached:?}");
			};
			assert!(message.contains(interface), "{execution:?}: {message}");
		}
	}
}
