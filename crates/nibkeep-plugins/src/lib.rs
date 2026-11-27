//! The plugin host. A plugin is a WebAssembly component, whose world the app that runs it
//! writes in WIT. The host runs it with Wasmtime:
//!
//! - compiled to machine code by Cranelift where the system lets an app generate code, and
//!   interpreted by Pulley where it does not, as on iOS ([`Execution`]); Cranelift makes
//!   Pulley's bytecode too, so a host compiles every plugin it runs itself;
//! - with a deadline on every call and a ceiling on its memory ([`Limits`]), which stop it
//!   ([`Stop`]), after which it answers no more;
//! - with clocks and random numbers of WASI, and neither files, nor the network, nor
//!   anything else that the app does not give it.
//!
//! An app compiles a plugin once, as it is installed ([`Host::compile`]), and keeps what was
//! compiled ([`Host::save`]), which only a host with the same key loads back ([`Host::load`]).
//! It takes a [`Linker`] from the host ([`Host::linker`]), adds the functions of its world,
//! as Wasmtime's `bindgen!` generates them, prepares the plugin once ([`Host::prepare`]), and
//! starts it for use ([`Prepared::start`]), each start with a state of its own ([`State`]).
//!
//! The app gives Wasmtime's types to `bindgen!` through [`wasmtime`], this crate's own.

mod wasi;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasmtime::component::{Component, Instance, InstancePre, Linker, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

pub use wasmtime;

/// How often the host's clock for deadlines ticks.
const TICK: Duration = Duration::from_millis(1);

/// The most elements that a plugin's tables may hold: a table of functions holds a few
/// hundred, and its elements are not memory that [`Limits::memory`] counts.
const TABLE_ELEMENTS: usize = 100_000;

/// The size of the keyed hash before what a host saves of a compiled plugin.
const SEAL: usize = blake3::OUT_LEN;

/// How a host runs plugins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Execution {
	/// Compiled to machine code, about twenty times faster than interpreted.
	Compiled,
	/// Interpreted by Pulley, where the system lets no app generate machine code.
	Interpreted,
}

impl Execution {
	/// How plugins run on this system: interpreted on Apple's phones, tablets, watches,
	/// televisions and headsets, where no app may generate machine code; compiled elsewhere.
	#[must_use]
	pub const fn for_this_platform() -> Self {
		if cfg!(any(
			target_os = "ios",
			target_os = "tvos",
			target_os = "watchos",
			target_os = "visionos"
		)) {
			Execution::Interpreted
		} else {
			Execution::Compiled
		}
	}
}

/// What a host is made with.
#[derive(Clone, Copy)]
pub struct Options {
	/// How the host runs plugins.
	pub execution: Execution,
	/// The key of what the host saves of compiled plugins, which only a host with the same
	/// key loads back: a secret of this device's, never shared with another. Without one,
	/// the host makes a key of its own, and loads only what it saved itself.
	pub cache_key: Option<[u8; 32]>,
}

impl Default for Options {
	fn default() -> Self {
		Options {
			execution: Execution::for_this_platform(),
			cache_key: None,
		}
	}
}

impl fmt::Debug for Options {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Options")
			.field("execution", &self.execution)
			.field("cache_key", &self.cache_key.map(|_| "…"))
			.finish()
	}
}

/// What a running plugin may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
	/// How long each call may run, and starting the plugin. It counts in milliseconds, so a
	/// plugin may run a millisecond or two past it.
	pub deadline: Duration,
	/// How many bytes of memory the plugin may have, all its memories together.
	pub memory: usize,
}

impl Default for Limits {
	/// A second for each call, and 64 MiB.
	fn default() -> Self {
		Limits {
			deadline: Duration::from_secs(1),
			memory: 64 << 20,
		}
	}
}

/// Why a plugin stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
	/// It ran past its deadline.
	Deadline,
	/// It asked for more memory than its ceiling.
	Memory,
	/// It failed in another way, or reached for what it does not have: Wasmtime's message.
	Trap(String),
	/// It stopped at an earlier call, and answers no more.
	Earlier,
}

impl fmt::Display for Stop {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Stop::Deadline => f.write_str("it ran past its deadline"),
			Stop::Memory => f.write_str("it asked for more memory than it may have"),
			Stop::Trap(message) => write!(f, "it failed: {message}"),
			Stop::Earlier => f.write_str("it stopped earlier, and answers no more"),
		}
	}
}

/// What fails in the host.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	/// The system would not give the host what it needs, such as its thread.
	#[error("the plugin host could not start: {0}")]
	Host(String),
	/// The plugin is no valid component, or does not compile.
	#[error("the plugin does not compile: {0}")]
	Compile(String),
	/// What was to be loaded as a compiled plugin was not saved by this host, or by one with
	/// its key and engine.
	#[error("the compiled plugin was not made here")]
	Foreign,
	/// The plugin imports a function of the app's with another type than the app's.
	#[error("the plugin does not fit the app: {0}")]
	Link(String),
	/// The plugin stopped.
	#[error("the plugin stopped: {0}")]
	Stopped(Stop),
}

/// The plugin host: an engine, its key, and the clock of its deadlines.
pub struct Host {
	engine: Engine,
	execution: Execution,
	key: [u8; 32],
	ticker: Arc<Ticker>,
}

impl Host {
	/// A host made with `options`, whose clock for deadlines ticks on a thread of its own
	/// until the host and every plugin it started are gone.
	///
	/// # Errors
	///
	/// Fails if Wasmtime refuses the settings, or the system gives no thread or randomness.
	pub fn new(options: Options) -> Result<Host, Error> {
		let mut config = Config::new();
		config.wasm_component_model(true);
		config.epoch_interruption(true);
		if options.execution == Execution::Interpreted {
			let pulley = match (
				cfg!(target_pointer_width = "64"),
				cfg!(target_endian = "big"),
			) {
				(true, false) => "pulley64",
				(true, true) => "pulley64be",
				(false, false) => "pulley32",
				(false, true) => "pulley32be",
			};
			config
				.target(pulley)
				.map_err(|error| Error::Host(error.to_string()))?;
			// Pulley checks every access, so reserving address space buys nothing, and
			// Apple's phones refuse Wasmtime's usual reservation of 4 GiB a memory.
			config.memory_reservation(0);
			config.memory_guard_size(0);
			config.memory_reservation_for_growth(1 << 20);
		}
		let engine = Engine::new(&config).map_err(|error| Error::Host(error.to_string()))?;
		let key = if let Some(key) = options.cache_key {
			key
		} else {
			let mut key = [0; 32];
			getrandom::fill(&mut key).map_err(|error| Error::Host(error.to_string()))?;
			key
		};
		let ticker = Ticker::start(engine.clone())?;
		Ok(Host {
			engine,
			execution: options.execution,
			key,
			ticker: Arc::new(ticker),
		})
	}

	/// How the host runs plugins.
	#[must_use]
	pub fn execution(&self) -> Execution {
		self.execution
	}

	/// Compiles a plugin, given as a component in WebAssembly's binary format.
	///
	/// # Errors
	///
	/// Fails if `wasm` is no valid component.
	pub fn compile(&self, wasm: &[u8]) -> Result<Plugin, Error> {
		Component::from_binary(&self.engine, wasm)
			.map(|component| Plugin { component })
			.map_err(|error| Error::Compile(format!("{error:#}")))
	}

	/// What the app keeps of a compiled plugin, so as not to compile it again: Wasmtime's
	/// own bytes, after a keyed hash of them.
	///
	/// # Errors
	///
	/// Fails if Wasmtime cannot write the plugin out.
	pub fn save(&self, plugin: &Plugin) -> Result<Vec<u8>, Error> {
		let compiled = plugin
			.component
			.serialize()
			.map_err(|error| Error::Compile(format!("{error:#}")))?;
		let mut saved = Vec::with_capacity(SEAL + compiled.len());
		saved.extend_from_slice(blake3::keyed_hash(&self.key, &compiled).as_bytes());
		saved.extend_from_slice(&compiled);
		Ok(saved)
	}

	/// Loads back what [`Host::save`] made, on this host or one with its key and engine.
	///
	/// # Errors
	///
	/// Fails with [`Error::Foreign`] if anything else made `saved`, or changed it.
	pub fn load(&self, saved: &[u8]) -> Result<Plugin, Error> {
		if saved.len() < SEAL {
			return Err(Error::Foreign);
		}
		let (seal, compiled) = saved.split_at(SEAL);
		let seal: [u8; SEAL] = seal.try_into().map_err(|_| Error::Foreign)?;
		// `Hash`'s comparison takes the same time wherever the two differ.
		if blake3::keyed_hash(&self.key, compiled) != blake3::Hash::from_bytes(seal) {
			return Err(Error::Foreign);
		}
		// SAFETY: Wasmtime runs the machine code it loads as it is, so it may load only what
		// it wrote itself. The keyed hash, which only a host with this key makes, shows that
		// these bytes are what `Component::serialize` wrote on such a host, unchanged; and
		// Wasmtime refuses bytes that another version, or an engine with other settings,
		// wrote.
		#[allow(unsafe_code)]
		let component = unsafe { Component::deserialize(&self.engine, compiled) };
		component
			.map(|component| Plugin { component })
			.map_err(|_| Error::Foreign)
	}

	/// A linker that has what a plugin has of WASI, to which the app adds its world's
	/// functions, which reach the app's own state through [`State::data_mut`].
	///
	/// # Errors
	///
	/// Fails only if Wasmtime refuses WASI's functions.
	pub fn linker<T: Send + 'static>(&self) -> Result<Linker<State<T>>, Error> {
		let mut linker = Linker::new(&self.engine);
		wasi::add(&mut linker).map_err(|error| Error::Link(format!("{error:#}")))?;
		Ok(linker)
	}

	/// Prepares `plugin` to start with `linker`'s functions, once for any number of starts.
	/// What the plugin imports and the linker lacks, such as WASI's files and sockets,
	/// becomes a trap, which stops the plugin if it calls it.
	///
	/// # Errors
	///
	/// Fails with [`Error::Link`] if the plugin imports a function of the linker's with
	/// another type.
	pub fn prepare<T: Send + 'static>(
		&self,
		linker: &Linker<State<T>>,
		plugin: &Plugin,
	) -> Result<Prepared<T>, Error> {
		let mut linker = linker.clone();
		linker
			.define_unknown_imports_as_traps(&plugin.component)
			.map_err(|error| Error::Link(format!("{error:#}")))?;
		let pre = linker
			.instantiate_pre(&plugin.component)
			.map_err(|error| Error::Link(format!("{error:#}")))?;
		Ok(Prepared {
			pre,
			ticker: Arc::clone(&self.ticker),
		})
	}
}

impl fmt::Debug for Host {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Host")
			.field("execution", &self.execution)
			.finish_non_exhaustive()
	}
}

/// A compiled plugin, which only the host that compiled or loaded it runs.
#[derive(Clone)]
pub struct Plugin {
	component: Component,
}

impl fmt::Debug for Plugin {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Plugin").finish_non_exhaustive()
	}
}

/// A plugin prepared with an app's functions, ready to start.
pub struct Prepared<T: 'static> {
	pre: InstancePre<State<T>>,
	ticker: Arc<Ticker>,
}

impl<T: 'static> Clone for Prepared<T> {
	fn clone(&self) -> Self {
		Prepared {
			pre: self.pre.clone(),
			ticker: Arc::clone(&self.ticker),
		}
	}
}

impl<T: Send + 'static> Prepared<T> {
	/// Starts the plugin, with `data`, the app's state for it, and `limits`.
	///
	/// # Errors
	///
	/// Fails with [`Error::Stopped`] if starting it ran past the deadline, past the ceiling
	/// of memory, or into a trap.
	pub fn start(&self, data: T, limits: Limits) -> Result<Running<T>, Error> {
		let state = State {
			data,
			wasi: WasiCtx::builder().build(),
			table: ResourceTable::new(),
			ceiling: Ceiling {
				memory: limits.memory,
				used: 0,
			},
		};
		let mut store = Store::new(self.pre.engine(), state);
		store.limiter(|state| &mut state.ceiling);
		let ticks = ticks(limits.deadline);
		store.set_epoch_deadline(self.ticker.deadline(ticks));
		let instance = self
			.pre
			.instantiate(&mut store)
			.map_err(|error| Error::Stopped(stop(&error)))?;
		Ok(Running {
			store,
			instance,
			ticks,
			stopped: false,
			ticker: Arc::clone(&self.ticker),
		})
	}
}

impl<T: 'static> fmt::Debug for Prepared<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Prepared").finish_non_exhaustive()
	}
}

/// A started plugin, with its own memory and state.
pub struct Running<T: 'static> {
	store: Store<State<T>>,
	instance: Instance,
	ticks: u64,
	stopped: bool,
	ticker: Arc<Ticker>,
}

impl<T: Send + 'static> Running<T> {
	/// Calls into the plugin through `call`, which gets the plugin's store and instance, as
	/// the functions that `bindgen!` generates take them. The deadline starts anew.
	///
	/// An error stops the plugin for good: a trap, the deadline or the ceiling of memory, and
	/// also an error of the app's own functions, which unwinds the plugin as a trap does.
	///
	/// # Errors
	///
	/// Fails with [`Error::Stopped`] if the plugin stops, or stopped at an earlier call.
	pub fn call<R>(
		&mut self,
		call: impl FnOnce(&mut Store<State<T>>, Instance) -> wasmtime::Result<R>,
	) -> Result<R, Error> {
		if self.stopped {
			return Err(Error::Stopped(Stop::Earlier));
		}
		self.store
			.set_epoch_deadline(self.ticker.deadline(self.ticks));
		// Marked stopped until the call returns, so that one whose closure panics counts as
		// stopped too.
		self.stopped = true;
		let answer =
			call(&mut self.store, self.instance).map_err(|error| Error::Stopped(stop(&error)))?;
		self.stopped = false;
		Ok(answer)
	}

	/// The app's state for this plugin.
	#[must_use]
	pub fn data(&self) -> &T {
		&self.store.data().data
	}

	/// The app's state for this plugin, to change.
	pub fn data_mut(&mut self) -> &mut T {
		&mut self.store.data_mut().data
	}

	/// Whether the plugin stopped, and answers no more.
	#[must_use]
	pub fn stopped(&self) -> bool {
		self.stopped
	}
}

impl<T: 'static> fmt::Debug for Running<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Running")
			.field("stopped", &self.stopped)
			.finish_non_exhaustive()
	}
}

/// What a running plugin's store holds: the app's state for it, and the host's.
pub struct State<T> {
	data: T,
	wasi: WasiCtx,
	table: ResourceTable,
	ceiling: Ceiling,
}

impl<T> State<T> {
	/// The app's state for this plugin.
	#[must_use]
	pub fn data(&self) -> &T {
		&self.data
	}

	/// The app's state for this plugin, to change: what the app's functions reach, as
	/// `bindgen!`'s `add_to_linker` takes it.
	pub fn data_mut(&mut self) -> &mut T {
		&mut self.data
	}
}

impl<T: Send> WasiView for State<T> {
	fn ctx(&mut self) -> WasiCtxView<'_> {
		WasiCtxView {
			ctx: &mut self.wasi,
			table: &mut self.table,
		}
	}
}

/// The ceiling of a plugin's memory.
struct Ceiling {
	memory: usize,
	/// The bytes that the plugin's memories hold, all together: a component may have many.
	used: usize,
}

/// A plugin asked for more memory, or more table, than it may have.
#[derive(Debug, thiserror::Error)]
#[error("over the plugin's ceiling")]
struct OverCeiling;

impl ResourceLimiter for Ceiling {
	fn memory_growing(
		&mut self,
		current: usize,
		desired: usize,
		maximum: Option<usize>,
	) -> wasmtime::Result<bool> {
		let used = (self.used - current.min(self.used)).saturating_add(desired);
		if used > self.memory {
			return Err(wasmtime::Error::new(OverCeiling));
		}
		let allowed = maximum.is_none_or(|maximum| desired <= maximum);
		if allowed {
			// Counted before the memory grows: one that then fails to grow still counts,
			// which errs on the side of the ceiling.
			self.used = used;
		}
		Ok(allowed)
	}

	fn table_growing(
		&mut self,
		_current: usize,
		desired: usize,
		maximum: Option<usize>,
	) -> wasmtime::Result<bool> {
		if desired > TABLE_ELEMENTS {
			return Err(wasmtime::Error::new(OverCeiling));
		}
		Ok(maximum.is_none_or(|maximum| desired <= maximum))
	}
}

/// Why `error` stopped a plugin.
fn stop(error: &wasmtime::Error) -> Stop {
	if error.downcast_ref::<OverCeiling>().is_some() {
		Stop::Memory
	} else if matches!(error.downcast_ref::<Trap>(), Some(Trap::Interrupt)) {
		Stop::Deadline
	} else {
		Stop::Trap(format!("{error:#}"))
	}
}

/// How many ticks of the host's clock a plugin may run for `deadline`: one more than the
/// deadline holds, as a call starts between two ticks, and at most some fifty days.
fn ticks(deadline: Duration) -> u64 {
	let tick = TICK.as_micros();
	let ticks = deadline.as_micros().div_ceil(tick).saturating_add(1);
	u64::try_from(ticks).map_or(u64::from(u32::MAX), |ticks| ticks.min(u64::from(u32::MAX)))
}

/// The clock of a host's deadlines: a thread that ticks Wasmtime's epoch once for every
/// millisecond gone, until it is dropped with the host and the last plugin it started.
struct Ticker {
	clock: Arc<Clock>,
	thread: Option<JoinHandle<()>>,
}

/// When a ticker started, how many ticks it gave, and whether it is to stop.
struct Clock {
	started: Instant,
	ticked: AtomicU64,
	stop: AtomicBool,
}

impl Clock {
	/// How many ticks are due by now.
	fn due(&self) -> u64 {
		let due = self.started.elapsed().as_micros() / TICK.as_micros();
		u64::try_from(due).unwrap_or(u64::MAX)
	}
}

impl Ticker {
	fn start(engine: Engine) -> Result<Ticker, Error> {
		let clock = Arc::new(Clock {
			started: Instant::now(),
			ticked: AtomicU64::new(0),
			stop: AtomicBool::new(false),
		});
		let ticking = Arc::clone(&clock);
		let thread = thread::Builder::new()
			.name("plugin deadlines".to_owned())
			.spawn(move || {
				while !ticking.stop.load(Ordering::Relaxed) {
					thread::sleep(TICK);
					// A system that sleeps longer than asked, as Windows does, gets the ticks it
					// missed at once, so that the epoch counts time rather than sleeps.
					let due = ticking.due();
					while ticking.ticked.load(Ordering::Relaxed) < due {
						engine.increment_epoch();
						ticking.ticked.fetch_add(1, Ordering::Relaxed);
					}
				}
			})
			.map_err(|error| Error::Host(error.to_string()))?;
		Ok(Ticker {
			clock,
			thread: Some(thread),
		})
	}

	/// The epoch's delta for a deadline of `ticks` from now: the ticks that are due and not
	/// yet given come at once as the ticker wakes, so they are added.
	fn deadline(&self, ticks: u64) -> u64 {
		let owed = self
			.clock
			.due()
			.saturating_sub(self.clock.ticked.load(Ordering::Relaxed));
		ticks.saturating_add(owed)
	}
}

impl Drop for Ticker {
	fn drop(&mut self) {
		self.clock.stop.store(true, Ordering::Relaxed);
		if let Some(thread) = self.thread.take() {
			// The thread only sleeps and ticks, so it ends within a tick, and cannot panic.
			let _ = thread.join();
		}
	}
}
