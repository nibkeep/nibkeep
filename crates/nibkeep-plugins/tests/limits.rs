//! A plugin is stopped at its deadline and at its ceiling of memory, and answers no more.

use std::time::{Duration, Instant};

use nibkeep_plugins::{Error, Limits, Stop};

use super::{EXECUTIONS, LIMITS, call, host, start};

/// A plugin that never returns.
const SPIN: &str = r#"(component
	(core module $m
		(func (export "spin")
			(loop $forever
				br $forever)))
	(core instance $i (instantiate $m))
	(func (export "spin")
		(canon lift (core func $i "spin"))))"#;

/// A plugin that grows its memory, a page of 64 KiB at a time, as it is asked.
const GROW: &str = r#"(component
	(core module $m
		(memory 1)
		(func (export "grow") (param i32) (result i32)
			local.get 0
			memory.grow))
	(core instance $i (instantiate $m))
	(func (export "grow") (param "pages" u32) (result s32)
		(canon lift (core func $i "grow"))))"#;

/// A plugin whose memory starts at 6.25 MiB.
const BIG: &str = r#"(component
	(core module $m
		(memory 100)
		(func (export "nothing")))
	(core instance $i (instantiate $m))
	(func (export "nothing")
		(canon lift (core func $i "nothing"))))"#;

#[test]
fn limits_a_plugin_that_spins_is_stopped_at_its_deadline() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let deadline = Duration::from_millis(50);
		let mut running = start(&host, SPIN, Limits { deadline, ..LIMITS }).expect("started");
		let started = Instant::now();
		let spun: Result<(), Error> = call(&mut running, "spin", ());
		let took = started.elapsed();
		assert!(
			matches!(spun, Err(Error::Stopped(Stop::Deadline))),
			"{execution:?}: {spun:?}"
		);
		assert!(took >= deadline, "{execution:?}: stopped after {took:?}");
		// The clock of deadlines counts time, not its sleeps, which on Windows take longer
		// than asked; the margin is for a busy machine.
		assert!(
			took < deadline + Duration::from_secs(1),
			"{execution:?}: stopped after {took:?}"
		);
		// The deadline holds for each call anew, and a stopped plugin answers no more.
		let again: Result<(), Error> = call(&mut running, "spin", ());
		assert!(
			matches!(again, Err(Error::Stopped(Stop::Earlier))),
			"{execution:?}: {again:?}"
		);
	}
}

#[test]
fn limits_a_plugin_that_grows_past_its_ceiling_is_stopped() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let limits = Limits {
			memory: 1 << 20,
			..LIMITS
		};
		let mut running = start(&host, GROW, limits).expect("started");
		// From one page to nine, below the ceiling of sixteen.
		let (before,): (i32,) = call(&mut running, "grow", (8u32,)).expect("grown");
		assert_eq!(before, 1, "{execution:?}");
		let grown: Result<(i32,), Error> = call(&mut running, "grow", (64u32,));
		assert!(
			matches!(grown, Err(Error::Stopped(Stop::Memory))),
			"{execution:?}: {grown:?}"
		);
		let again: Result<(i32,), Error> = call(&mut running, "grow", (0u32,));
		assert!(
			matches!(again, Err(Error::Stopped(Stop::Earlier))),
			"{execution:?}: {again:?}"
		);
	}
}

/// A plugin of two modules, each with a memory of its own that it grows as it is asked.
const GROW_TWO: &str = r#"(component
	(core module $m
		(memory 1)
		(func (export "grow") (param i32) (result i32)
			local.get 0
			memory.grow))
	(core instance $first (instantiate $m))
	(core instance $second (instantiate $m))
	(func (export "grow-first") (param "pages" u32) (result s32)
		(canon lift (core func $first "grow")))
	(func (export "grow-second") (param "pages" u32) (result s32)
		(canon lift (core func $second "grow"))))"#;

#[test]
fn limits_the_ceiling_holds_for_all_of_a_plugins_memories_together() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let limits = Limits {
			memory: 1 << 20,
			..LIMITS
		};
		let mut running = start(&host, GROW_TWO, limits).expect("started");
		// Nine pages and two: eleven of sixteen.
		let _: (i32,) = call(&mut running, "grow-first", (8u32,)).expect("grown");
		let _: (i32,) = call(&mut running, "grow-second", (1u32,)).expect("grown");
		// Each would still fit alone, at fifteen pages, but not with the other's nine.
		let grown: Result<(i32,), Error> = call(&mut running, "grow-second", (13u32,));
		assert!(
			matches!(grown, Err(Error::Stopped(Stop::Memory))),
			"{execution:?}: {grown:?}"
		);
	}
}

#[test]
fn limits_a_plugin_too_big_for_its_ceiling_does_not_start() {
	for execution in EXECUTIONS {
		let host = host(execution);
		let limits = Limits {
			memory: 1 << 20,
			..LIMITS
		};
		let started = start(&host, BIG, limits);
		assert!(
			matches!(started, Err(Error::Stopped(Stop::Memory))),
			"{execution:?}: {:?}",
			started.err()
		);
		assert!(start(&host, BIG, LIMITS).is_ok(), "{execution:?}");
	}
}
