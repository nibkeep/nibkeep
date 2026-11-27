//! A compiled plugin, saved, loads back on a host with the same key and engine, and on no
//! other: compiled code from elsewhere is never run.

use nibkeep_plugins::{Error, Execution, Host, Options};

use super::{ADD, EXECUTIONS, LIMITS, call, compile};

fn keyed(execution: Execution, key: u8) -> Host {
	Host::new(Options {
		execution,
		cache_key: Some([key; 32]),
	})
	.expect("a host")
}

#[test]
fn cache_a_saved_plugin_loads_back_on_a_host_with_its_key() {
	for execution in EXECUTIONS {
		let host = keyed(execution, 7);
		let saved = host.save(&compile(&host, ADD)).expect("saved");
		// Also on a new host with the same key, as after the app starts again.
		for host in [host, keyed(execution, 7)] {
			let plugin = host.load(&saved).expect("loaded");
			let linker = host.linker().expect("a linker");
			let mut running = host
				.prepare(&linker, &plugin)
				.expect("prepared")
				.start((), LIMITS)
				.expect("started");
			let (sum,): (u32,) = call(&mut running, "add", (2u32, 3u32)).expect("an answer");
			assert_eq!(sum, 5, "{execution:?}");
		}
	}
}

#[test]
fn cache_a_saved_plugin_loads_nowhere_else() {
	for execution in EXECUTIONS {
		let host = keyed(execution, 7);
		let saved = host.save(&compile(&host, ADD)).expect("saved");

		let other_key = keyed(execution, 8).load(&saved);
		assert!(matches!(other_key, Err(Error::Foreign)), "{execution:?}");

		for at in [0, 31, 32, saved.len() / 2, saved.len() - 1] {
			let mut changed = saved.clone();
			changed[at] ^= 1;
			let loaded = host.load(&changed);
			assert!(
				matches!(loaded, Err(Error::Foreign)),
				"{execution:?}, byte {at}"
			);
		}
		assert!(matches!(host.load(&saved[..20]), Err(Error::Foreign)));

		// The same key on a host that runs plugins the other way: Wasmtime refuses it.
		let other = match execution {
			Execution::Compiled => Execution::Interpreted,
			Execution::Interpreted => Execution::Compiled,
		};
		let other_engine = keyed(other, 7).load(&saved);
		assert!(matches!(other_engine, Err(Error::Foreign)), "{execution:?}");
	}
}

#[test]
fn cache_without_a_key_a_host_loads_only_what_it_saved_itself() {
	let first = Host::new(Options::default()).expect("a host");
	let second = Host::new(Options::default()).expect("a host");
	let saved = first.save(&compile(&first, ADD)).expect("saved");
	assert!(first.load(&saved).is_ok());
	assert!(matches!(second.load(&saved), Err(Error::Foreign)));
}
