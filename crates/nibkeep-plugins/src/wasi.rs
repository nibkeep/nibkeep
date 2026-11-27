//! What a plugin has of WASI: clocks, random numbers, an empty environment, and standard
//! streams that lead nowhere, which a plugin's standard library reaches for. Never the file
//! system, sockets or exiting: [`crate::Host::prepare`] makes each of those a trap.

use wasmtime::component::{HasData, Linker, ResourceTable};
use wasmtime_wasi::WasiView;
use wasmtime_wasi::cli::{WasiCli, WasiCliView};
use wasmtime_wasi::clocks::{WasiClocks, WasiClocksView};
use wasmtime_wasi::p2::bindings::{cli, clocks, random, sync};
use wasmtime_wasi::random::WasiRandom;

/// WASI's streams, errors and pollables, which live in the store's table of resources.
struct Io;

impl HasData for Io {
	type Data<'a> = &'a mut ResourceTable;
}

/// Adds to `linker` the interfaces of WASI that a plugin has.
pub(crate) fn add<T: WasiView>(linker: &mut Linker<T>) -> wasmtime::Result<()> {
	sync::io::error::add_to_linker::<T, Io>(linker, |t| t.ctx().table)?;
	sync::io::poll::add_to_linker::<T, Io>(linker, |t| t.ctx().table)?;
	sync::io::streams::add_to_linker::<T, Io>(linker, |t| t.ctx().table)?;
	clocks::wall_clock::add_to_linker::<T, WasiClocks>(linker, T::clocks)?;
	clocks::monotonic_clock::add_to_linker::<T, WasiClocks>(linker, T::clocks)?;
	random::random::add_to_linker::<T, WasiRandom>(linker, |t| t.ctx().ctx.random())?;
	random::insecure::add_to_linker::<T, WasiRandom>(linker, |t| t.ctx().ctx.random())?;
	random::insecure_seed::add_to_linker::<T, WasiRandom>(linker, |t| t.ctx().ctx.random())?;
	cli::environment::add_to_linker::<T, WasiCli>(linker, T::cli)?;
	cli::stdin::add_to_linker::<T, WasiCli>(linker, T::cli)?;
	cli::stdout::add_to_linker::<T, WasiCli>(linker, T::cli)?;
	cli::stderr::add_to_linker::<T, WasiCli>(linker, T::cli)?;
	Ok(())
}
