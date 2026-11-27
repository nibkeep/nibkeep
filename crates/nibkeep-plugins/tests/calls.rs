//! A plugin answers, and calls the functions that the app gives it.

use nibkeep_plugins::State;
use nibkeep_plugins::wasmtime::StoreContextMut;

use super::{ADD, EXECUTIONS, LIMITS, call, compile, host, start};

/// A plugin that asks the host to double a number, twice.
const QUADRUPLE: &str = r#"(component
	(import "double" (func $double (param "x" u32) (result u32)))
	(core func $double_core (canon lower (func $double)))
	(core module $m
		(import "host" "double" (func $double (param i32) (result i32)))
		(func (export "quadruple") (param i32) (result i32)
			local.get 0
			call $double
			call $double))
	(core instance $host (export "double" (func $double_core)))
	(core instance $i (instantiate $m (with "host" (instance $host))))
	(func (export "quadruple") (param "x" u32) (result u32)
		(canon lift (core func $i "quadruple"))))"#;

#[test]
fn calls_a_plugin_answers() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let mut running = start(&host, ADD, LIMITS).expect("started");
		let (sum,): (u32,) = call(&mut running, "add", (2u32, 3u32)).expect("an answer");
		assert_eq!(sum, 5, "{execution:?}");
		let (sum,): (u32,) = call(&mut running, "add", (40u32, 2u32)).expect("an answer");
		assert_eq!(sum, 42, "{execution:?}");
	}
}

#[test]
fn calls_a_plugin_calls_the_functions_the_app_gives_it() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let plugin = compile(&host, QUADRUPLE);
		// The app's state: how often the plugin called it.
		let mut linker = host.linker::<u32>().expect("a linker");
		linker
			.root()
			.func_wrap(
				"double",
				|mut store: StoreContextMut<'_, State<u32>>, (x,): (u32,)| {
					*store.data_mut().data_mut() += 1;
					Ok((x * 2,))
				},
			)
			.expect("defined");
		let mut running = host
			.prepare(&linker, &plugin)
			.expect("prepared")
			.start(0, LIMITS)
			.expect("started");
		let (four_times,): (u32,) = call(&mut running, "quadruple", (5u32,)).expect("an answer");
		assert_eq!(four_times, 20, "{execution:?}");
		assert_eq!(*running.data(), 2, "{execution:?}");
	}
}

#[test]
fn calls_a_prepared_plugin_starts_many_times_each_with_its_own_state() {
	let host = host(nibkeep_plugins::Execution::for_this_platform());
	let plugin = compile(&host, ADD);
	let linker = host.linker::<&'static str>().expect("a linker");
	let prepared = host.prepare(&linker, &plugin).expect("prepared");
	let mut first = prepared.start("first", LIMITS).expect("started");
	let mut second = prepared.start("second", LIMITS).expect("started");
	let (a,): (u32,) = call(&mut first, "add", (1u32, 1u32)).expect("an answer");
	let (b,): (u32,) = call(&mut second, "add", (2u32, 2u32)).expect("an answer");
	assert_eq!((a, b), (2, 4));
	assert_eq!((*first.data(), *second.data()), ("first", "second"));
}
