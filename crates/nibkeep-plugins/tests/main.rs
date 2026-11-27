//! The crate's integration tests, one binary of them: each file is a module. Their plugins
//! are components written in WebAssembly's text format, each run compiled and interpreted.

mod cache;
mod calls;
mod imports;
mod limits;

use std::time::Duration;

use nibkeep_plugins::wasmtime::component::{ComponentNamedList, Lift, Lower};
use nibkeep_plugins::{Error, Execution, Host, Limits, Options, Plugin, Running};

/// Both ways of running a plugin, which every test tries.
const EXECUTIONS: [Execution; 2] = [Execution::Compiled, Execution::Interpreted];

/// Limits that no test's plugin reaches unless it means to.
const LIMITS: Limits = Limits {
	deadline: Duration::from_secs(10),
	memory: 16 << 20,
};

/// A plugin that adds two numbers.
const ADD: &str = r#"(component
	(core module $m
		(func (export "add") (param i32 i32) (result i32)
			local.get 0
			local.get 1
			i32.add))
	(core instance $i (instantiate $m))
	(func (export "add") (param "a" u32) (param "b" u32) (result u32)
		(canon lift (core func $i "add"))))"#;

/// A host that runs plugins as `execution` says, with a cache key of its own.
fn host(execution: Execution) -> Host {
	Host::new(Options {
		execution,
		..Options::default()
	})
	.expect("a host")
}

/// The plugin written in `text`, compiled by `host`.
fn compile(host: &Host, text: &str) -> Plugin {
	host.compile(&wat::parse_str(text).expect("a component"))
		.expect("compiled")
}

/// The plugin written in `text`, started on `host` with no functions of the host's own.
fn start(host: &Host, text: &str, limits: Limits) -> Result<Running<()>, Error> {
	let plugin = compile(host, text);
	let linker = host.linker().expect("a linker");
	host.prepare(&linker, &plugin)?.start((), limits)
}

/// Calls the plugin's function `name` with `params`.
fn call<T, P, R>(running: &mut Running<T>, name: &str, params: P) -> Result<R, Error>
where
	T: Send + 'static,
	P: ComponentNamedList + Lower + Send + Sync,
	R: ComponentNamedList + Lift + Send + Sync,
{
	running.call(|store, instance| {
		let function = instance.get_typed_func::<P, R>(&mut *store, name)?;
		function.call(&mut *store, params)
	})
}
