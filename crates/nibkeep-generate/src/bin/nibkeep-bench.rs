//! Benchmarks of the engine, through its Rust API (`docs/Plan.md`, section 4): makes a
//! library with the generator, then measures opening it, searching it, showing entities
//! and verifying it in a process of its own, so that its peak memory is theirs and not the
//! generator's, and last how long a new replica takes to receive all of it in its first
//! sync. Prints the numbers as JSON.
//!
//!     nibkeep-bench ROUNDS DIRECTORY
//!     nibkeep-bench --generate ROUNDS DIRECTORY
//!     nibkeep-bench --real DATA [--languages first|all] DIRECTORY
//!     nibkeep-bench --import DATA DIRECTORY
//!     nibkeep-bench --calibrate DIRECTORY
//!
//! With `--generate`, it only makes the library, for `scripts/bench-nib.py` to measure
//! `nib` with. With `--real`, it imports the real data in `DATA`, as
//! `scripts/fetch-real-data.py` fetched it, instead of generating: with one name for each
//! entity, or with `--languages all`, every name and text in every language; the results
//! name the data, the importer's recipe and the choice of languages. With `--import`, it
//! only imports the real data, with one name for each entity. With `--calibrate`, it
//! measures the machine rather than NibKeep, on the disk that holds `DIRECTORY`: how long
//! one core takes for a fixed piece of work, how long writing a page and syncing it take,
//! and how fast the disk takes a large file. The program calls itself with `--measure` to
//! measure in a process of its own, which is not for people to use.
//! `DIRECTORY` must not exist yet. A library is left behind for the caller to measure or
//! remove, with a record of the replica that received it, which is removed; what
//! `--calibrate` writes is removed. Each round writes seven
//! entities, one of which it deletes. The library is opened with
//! its files in the system's cache, as they are right after they were written.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};
use std::{env, fs, hint, process};

use nibkeep_generate::{
	ENTITIES_PER_ROUND, Languages, PASSPHRASE, RECIPE, RealData, at, generate, passphrase_params,
};
use nibkeep_node::{Joining, Library, NewProtection, Unlocker};
use nibkeep_testkit::TestRng;
use nibkeep_time::Day;

/// The words searched for in generated libraries: some found in every round, some in one.
const QUERIES: [&str; 8] = [
	"leviathan",
	"le guin",
	"earthsea",
	"dispossessed 42",
	"tehanu",
	"library example",
	"anarchy",
	"wizard of",
];

/// The words searched for in real data: some found often, some rarely, and one in a
/// language that comes first only where an entity has no name in English.
const REAL_QUERIES: [&str; 8] = [
	"harry potter",
	"potter",
	"expanse",
	"radcliffe",
	"smith",
	"james",
	"leviathan wakes",
	"гарри",
];

/// How many times each kind of measure is taken.
const SAMPLES: usize = 200;

/// How many entities a search lists in the measure of a first page, as `nib find` does.
const FIRST: usize = 50;

/// The argument that makes the program measure a library made already.
const MEASURE: &str = "--measure";

/// The argument that makes the program only make a library.
const GENERATE: &str = "--generate";

/// The argument that makes the program import real data.
const REAL: &str = "--real";

/// The argument that chooses the languages of real data to import.
const LANGUAGES: &str = "--languages";

/// The argument that makes the program only import real data.
const IMPORT: &str = "--import";

/// The argument that makes the program measure the machine.
const CALIBRATE: &str = "--calibrate";

/// How many numbers the fixed piece of work for one core sorts.
const CORE_NUMBERS: usize = 1 << 20;

/// How many times the work for one core is timed, of which the median counts.
const CORE_SAMPLES: usize = 5;

/// How many pages are written and synced one by one, of which the median counts.
const SYNCS: usize = 100;

/// The bytes of a page, as SQLite writes them.
const PAGE: usize = 4096;

/// The mebibytes of the large file, and how many times it is written, of which the median
/// counts.
const LARGE_FILE_MEBIBYTES: usize = 64;
const LARGE_FILES: usize = 3;

fn main() {
	let arguments: Vec<String> = env::args().skip(1).collect();
	match arguments.as_slice() {
		[flag, directory, now, queries] if flag == MEASURE => {
			let queries = if queries == REAL {
				&REAL_QUERIES
			} else {
				&QUERIES
			};
			let now = now.parse().expect("a time");
			println!("{}", measure_library(Path::new(directory), now, queries));
		}
		[flag, directory] if flag == CALIBRATE => calibrate(Path::new(directory)),
		[flag, data, directory] if flag == IMPORT => {
			let directory = Path::new(directory);
			fresh(directory);
			drop(read_real(Path::new(data), Languages::First).import(directory));
		}
		[flag, data, directory] if flag == REAL => {
			import_and_measure(Path::new(data), Languages::First, Path::new(directory));
		}
		[flag, data, choose, languages, directory] if flag == REAL && choose == LANGUAGES => {
			let languages = Languages::named(languages).unwrap_or_else(|| {
				eprintln!("the languages are first or all, not {languages}");
				process::exit(2);
			});
			import_and_measure(Path::new(data), languages, Path::new(directory));
		}
		[flag, rounds, directory] if flag == GENERATE => {
			let directory = Path::new(directory);
			fresh(directory);
			drop(generate(directory, rounds_from(rounds)));
		}
		[rounds, directory] => make_and_measure(rounds_from(rounds), Path::new(directory)),
		_ => {
			eprintln!("usage: nibkeep-bench [--generate] ROUNDS DIRECTORY");
			eprintln!("       nibkeep-bench --real DATA [--languages first|all] DIRECTORY");
			eprintln!("       nibkeep-bench --import DATA DIRECTORY");
			eprintln!("       nibkeep-bench --calibrate DIRECTORY");
			process::exit(2);
		}
	}
}

fn rounds_from(text: &str) -> u64 {
	text.parse().unwrap_or_else(|_| {
		eprintln!("ROUNDS is a whole number, not {text}");
		process::exit(2);
	})
}

/// Ends the program if the directory exists: libraries are made only in new ones.
fn fresh(directory: &Path) {
	if directory.exists() {
		eprintln!("{} exists already", directory.display());
		process::exit(2);
	}
}

/// Makes the library, measures it in a new process, and prints both parts' numbers.
fn make_and_measure(rounds: u64, directory: &Path) {
	fresh(directory);
	fresh(&receiving_beside(directory));
	let started = Instant::now();
	drop(generate(directory, rounds));
	let generated = started.elapsed();
	let written = rounds * ENTITIES_PER_ROUND;
	let measured = measure_apart(directory, at(rounds, 0), "generated");
	// After the size, which the record of the receiving replica would add to.
	let disk = size(directory);
	let (received, changes) = receive(directory, at(rounds, 1));
	let mut results = Json::new();
	results.number("rounds", rounds);
	results.number("entities_written", written);
	results.decimal("generate_seconds", generated.as_secs_f64());
	results.decimal(
		"entities_written_per_second",
		per_second(written, generated),
	);
	results.number("disk_bytes", disk);
	results.decimal("receive_seconds", received.as_secs_f64());
	results.decimal("changes_received_per_second", per_second(changes, received));
	println!("{}", results.join(&measured));
}

/// Where the replica that receives the library in `directory` is made: beside it.
fn receiving_beside(directory: &Path) -> std::path::PathBuf {
	let mut name = directory.file_name().expect("a name").to_owned();
	name.push("-received");
	directory.with_file_name(name)
}

/// Pairs a new replica with the library in `directory`, in this process, at `now`, which
/// brings the new one everything in its first sync, as a new device's first sync does.
/// Returns how long that took, pairing included, and how many changes the new replica
/// received. It is made beside the library, and removed after; the library keeps its record,
/// and is measured before.
fn receive(directory: &Path, now: u64) -> (Duration, u64) {
	let mut library =
		Library::open(directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	let receiving = receiving_beside(directory);
	let mut rng = TestRng::new("the replica that receives");
	let joining = Joining::new("phone", "nib", &mut rng).expect("names");
	let protection = NewProtection::Passphrase(PASSPHRASE, passphrase_params());
	let started = Instant::now();
	let received = library
		.pair_in_process(&receiving, joining, &protection, &|| now, &mut rng)
		.expect("pairs");
	let elapsed = started.elapsed();
	let changes = received.changes_kept().expect("counts the changes");
	drop((received, library));
	fs::remove_dir_all(&receiving).expect("removes the replica that received");
	(elapsed, u64::try_from(changes).expect("a count"))
}

/// Reads real data, or ends the program with why it cannot.
fn read_real(data: &Path, languages: Languages) -> RealData {
	RealData::read(data, languages).unwrap_or_else(|error| {
		eprintln!("{error}");
		process::exit(1);
	})
}

/// Imports real data, measures the library in a new process and a new replica's first
/// sync, and prints the numbers. Reading the data is not timed, only writing it into the
/// library.
fn import_and_measure(data: &Path, languages: Languages, directory: &Path) {
	fresh(directory);
	fresh(&receiving_beside(directory));
	let data = read_real(data, languages);
	let name = data.name().to_owned();
	let started = Instant::now();
	let (library, imported) = data.import(directory);
	drop(library);
	let elapsed = started.elapsed();
	let written = imported.entities + imported.credits;
	let measured = measure_apart(directory, imported.finished_at, REAL);
	// After the size, which the record of the receiving replica would add to.
	let disk = size(directory);
	let (received, changes) = receive(directory, imported.finished_at + 60_000);
	let mut results = Json::new();
	results.text("data", &name);
	results.number("recipe", u64::from(RECIPE));
	results.text("languages", languages.as_str());
	results.number("entities_written", written);
	results.decimal("import_seconds", elapsed.as_secs_f64());
	results.decimal("entities_written_per_second", per_second(written, elapsed));
	results.number("disk_bytes", disk);
	results.decimal("receive_seconds", received.as_secs_f64());
	results.decimal("changes_received_per_second", per_second(changes, received));
	println!("{}", results.join(&measured));
}

/// Measures the machine in a new directory on the disk to measure, prints the numbers, and
/// removes what it wrote. They tell machines apart where their numbers differ: one core's
/// speed, and the disk's.
fn calibrate(directory: &Path) {
	fresh(directory);
	fs::create_dir_all(directory).expect("makes the directory");
	let core = measure(CORE_SAMPLES, |_| core_work());
	let sync = sync_time(directory);
	let rate = large_file_rate(directory);
	fs::remove_dir_all(directory).expect("removes what it wrote");
	let mut results = Json::new();
	results.decimal("core_ms", millis(percentile(&core, 50)));
	results.decimal("sync_ms", millis(sync));
	results.decimal("write_mb_per_second", rate);
	println!("{}", results.finish());
}

/// Numbers that look random, the same on every machine.
fn numbers(count: usize) -> Vec<u64> {
	let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
	(0..count)
		.map(|_| {
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
			state
		})
		.collect()
}

/// A fixed piece of work for one core, of the kinds the engine does: sorting numbers, and
/// building a B-tree of some of them in no order and looking all of them up in it.
fn core_work() {
	let unsorted = numbers(CORE_NUMBERS);
	let mut sorted = unsorted.clone();
	sorted.sort_unstable();
	let tree: BTreeMap<u64, usize> = unsorted
		.iter()
		.take(CORE_NUMBERS / 4)
		.enumerate()
		.map(|(index, &number)| (number, index))
		.collect();
	let found = sorted
		.iter()
		.filter(|number| tree.contains_key(number))
		.count();
	hint::black_box(found);
}

/// Bytes that look random, which no file system can compress.
fn noise(bytes: usize) -> Vec<u8> {
	numbers(bytes.div_ceil(8))
		.iter()
		.flat_map(|number| number.to_le_bytes())
		.take(bytes)
		.collect()
}

/// The median time that writing a page at the end of a file and syncing it takes, as each
/// change a library saves ends.
fn sync_time(directory: &Path) -> Duration {
	let path = directory.join("pages");
	let mut file = File::create(&path).expect("makes a file");
	let page = noise(PAGE);
	let times = measure(SYNCS, |_| {
		file.write_all(&page).expect("writes");
		file.sync_data().expect("syncs");
	});
	drop(file);
	fs::remove_file(path).expect("removes the file");
	percentile(&times, 50)
}

/// How fast the disk takes a large file, written and synced, in megabytes a second: the
/// median of a few.
fn large_file_rate(directory: &Path) -> f64 {
	const MEBIBYTE: usize = 1 << 20;
	let chunk = noise(MEBIBYTE);
	let mut rates: Vec<f64> = (0..LARGE_FILES)
		.map(|round| {
			let path = directory.join(format!("large-{round}"));
			let started = Instant::now();
			let mut file = File::create(&path).expect("makes a file");
			for _ in 0..LARGE_FILE_MEBIBYTES {
				file.write_all(&chunk).expect("writes");
			}
			file.sync_all().expect("syncs");
			let elapsed = started.elapsed();
			drop(file);
			fs::remove_file(path).expect("removes the file");
			let bytes = u64::try_from(LARGE_FILE_MEBIBYTES * MEBIBYTE).expect("a size");
			per_second(bytes, elapsed) / 1e6
		})
		.collect();
	rates.sort_by(f64::total_cmp);
	rates[rates.len() / 2]
}

/// Measures a library in a process of its own, as of `now`, with the queries of real data
/// or generated data, and returns the numbers as JSON.
fn measure_apart(directory: &Path, now: u64, queries: &str) -> String {
	let program = env::current_exe().expect("the program's path");
	let output = process::Command::new(program)
		.arg(MEASURE)
		.arg(directory)
		.arg(now.to_string())
		.arg(queries)
		.output()
		.expect("runs the measures");
	if !output.status.success() {
		eprint!("{}", String::from_utf8_lossy(&output.stderr));
		process::exit(1);
	}
	String::from_utf8(output.stdout)
		.expect("JSON")
		.trim()
		.to_owned()
}

/// Opens, searches, shows and verifies a library last written at `now`, and returns the
/// numbers as JSON.
fn measure_library(directory: &Path, now: u64, queries: &[&str]) -> String {
	let started = Instant::now();
	let mut library =
		Library::open(directory, &Unlocker::Passphrase(PASSPHRASE)).expect("opens the library");
	let opened = started.elapsed();
	let personal = library.personal_space();
	let entities = library.status().expect("reads the status").spaces[0].entities;

	let searches = measure(SAMPLES, |sample| {
		library
			.search(personal, queries[sample % queries.len()])
			.expect("searches");
	});
	// The first page, as `nib find` lists it.
	let first_pages = measure(SAMPLES, |sample| {
		library
			.search_first(personal, queries[sample % queries.len()], FIRST, |_, _| {
				Ok(true)
			})
			.expect("searches");
	});
	let all = library.entities(personal, None).expect("a space");
	// Today is the day the library was last written on, when every value has started.
	let today = Day::from_unix_millis(now, 0).expect("a day");
	let past: Day = "2022-01-01".parse().expect("a day");
	let show = |day: Day| {
		measure(SAMPLES, |sample| {
			let listed = &all[sample * all.len() / SAMPLES];
			library
				.entity(personal, listed.id, day)
				.expect("a space")
				.expect("an entity");
		})
	};
	let shows_today = show(today);
	let shows_past = show(past);

	let started = Instant::now();
	let report = library.verify(now).expect("verifies");
	let verified = started.elapsed();
	assert!(
		report.spaces.iter().all(|space| space.problems.is_empty()),
		"the library is sound"
	);

	let mut results = Json::new();
	results.number("entities", u64::try_from(entities).expect("a count"));
	results.decimal("open_seconds", opened.as_secs_f64());
	results.decimal("search_median_ms", millis(percentile(&searches, 50)));
	results.decimal("search_p95_ms", millis(percentile(&searches, 95)));
	results.decimal(
		"search_first_median_ms",
		millis(percentile(&first_pages, 50)),
	);
	results.decimal("search_first_p95_ms", millis(percentile(&first_pages, 95)));
	results.decimal("show_median_ms", millis(percentile(&shows_today, 50)));
	results.decimal("show_p95_ms", millis(percentile(&shows_today, 95)));
	results.decimal("show_past_median_ms", millis(percentile(&shows_past, 50)));
	results.decimal("verify_seconds", verified.as_secs_f64());
	match peak_memory() {
		Some(bytes) => results.number("peak_memory_bytes", bytes),
		None => results.null("peak_memory_bytes"),
	}
	results.finish()
}

/// A JSON object of numbers, written by hand: the program needs nothing more.
struct Json(String);

impl Json {
	fn new() -> Self {
		Self(String::from("{"))
	}

	fn key(&mut self, key: &str) {
		if self.0.len() > 1 {
			self.0.push(',');
		}
		write!(self.0, "\"{key}\":").expect("writes to a string");
	}

	fn number(&mut self, key: &str, value: u64) {
		self.key(key);
		write!(self.0, "{value}").expect("writes to a string");
	}

	fn decimal(&mut self, key: &str, value: f64) {
		self.key(key);
		write!(self.0, "{value:.6}").expect("writes to a string");
	}

	/// A text, which must need no escaping, such as a name of data.
	fn text(&mut self, key: &str, value: &str) {
		assert!(
			value
				.chars()
				.all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
			"plain text"
		);
		self.key(key);
		write!(self.0, "\"{value}\"").expect("writes to a string");
	}

	fn null(&mut self, key: &str) {
		self.key(key);
		self.0.push_str("null");
	}

	fn finish(mut self) -> String {
		self.0.push('}');
		self.0
	}

	/// This object followed by the members of another, as one object.
	fn join(self, other: &str) -> String {
		let members = other.trim_start_matches('{');
		if members == "}" {
			return self.finish();
		}
		format!("{},{members}", self.0)
	}
}

/// Runs `work` `samples` times, and returns how long each run took, sorted.
fn measure(samples: usize, mut work: impl FnMut(usize)) -> Vec<Duration> {
	let mut times: Vec<Duration> = (0..samples)
		.map(|sample| {
			let started = Instant::now();
			work(sample);
			started.elapsed()
		})
		.collect();
	times.sort_unstable();
	times
}

/// The `percent`th percentile of sorted times.
fn percentile(sorted: &[Duration], percent: usize) -> Duration {
	let index = (sorted.len() * percent / 100).min(sorted.len().saturating_sub(1));
	sorted.get(index).copied().unwrap_or_default()
}

fn millis(duration: Duration) -> f64 {
	duration.as_secs_f64() * 1000.0
}

fn per_second(count: u64, duration: Duration) -> f64 {
	#[expect(clippy::cast_precision_loss, reason = "a rate for people to read")]
	let count = count as f64;
	count / duration.as_secs_f64().max(f64::MIN_POSITIVE)
}

/// The bytes of the files in a directory and the directories in it.
fn size(directory: &Path) -> u64 {
	fs::read_dir(directory)
		.into_iter()
		.flatten()
		.flatten()
		.map(|entry| match entry.metadata() {
			Ok(metadata) if metadata.is_dir() => size(&entry.path()),
			Ok(metadata) => metadata.len(),
			Err(_) => 0,
		})
		.sum()
}

/// The most memory the process has held, where the system tells: on Linux and Android, from
/// `/proc/self/status`.
#[cfg(not(windows))]
fn peak_memory() -> Option<u64> {
	let status = fs::read_to_string("/proc/self/status").ok()?;
	let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
	let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
	Some(kilobytes * 1024)
}

/// The most memory the process has held: on Windows, its peak working set, the memory it
/// held in RAM at most, as `VmHWM` is on Linux.
#[cfg(windows)]
fn peak_memory() -> Option<u64> {
	windows::peak_working_set()
}

/// The one call into Windows that measuring peak memory needs, which the standard library
/// does not make: the only `unsafe` code of this crate.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
	use std::ffi::c_void;

	/// `PROCESS_MEMORY_COUNTERS`, of which only the peak working set is read.
	#[repr(C)]
	#[derive(Default)]
	struct Counters {
		size: u32,
		page_faults: u32,
		peak_working_set: usize,
		others: [usize; 7],
	}

	#[link(name = "kernel32")]
	unsafe extern "system" {
		fn GetCurrentProcess() -> *mut c_void;
		fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, size: u32)
		-> i32;
	}

	/// The peak working set of this process, in bytes.
	pub(super) fn peak_working_set() -> Option<u64> {
		let size = u32::try_from(size_of::<Counters>()).ok()?;
		let mut counters = Counters {
			size,
			..Counters::default()
		};
		// SAFETY: GetCurrentProcess takes nothing and returns a pseudo handle that needs no
		// closing. K32GetProcessMemoryInfo writes at most `size` bytes into `counters`,
		// which is a `PROCESS_MEMORY_COUNTERS` of that size, alive for the call.
		let succeeded =
			unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &raw mut counters, size) };
		if succeeded == 0 {
			return None;
		}
		u64::try_from(counters.peak_working_set).ok()
	}
}
