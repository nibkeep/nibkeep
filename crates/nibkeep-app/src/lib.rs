//! What the core of every NibKeep app needs, beside the engine. An app's core is a library
//! of its own, which the app's interface calls through one C function that takes a request
//! as JSON and returns the answer as JSON ([`call`]). This crate answers what every app
//! asks alike:
//!
//! - the library, held between calls, closed, and opened for each, so that `nib` and other
//!   apps use it in between ([`held`]);
//! - finding, making, unlocking and joining a library, and making a device from a copy of
//!   one ([`setup`]), with keys in the platform's keystore or protected by a passphrase
//!   ([`keystore`]);
//! - syncing with the person's other devices ([`syncing`]), adding a device ([`pairing`]),
//!   and joining a space that someone shares ([`joining`]);
//! - the spaces on this device ([`holding`]), making a space ([`spaces`]), recovery codes
//!   ([`recovery`]), and syncing from login ([`at_login`]).
//!
//! An app names itself with [`start`] as each call begins, or answers through [`answer`],
//! which does. An app that reads secrets says how with [`Secrets`]; one that does not never
//! asks for, gives or takes them here, whatever a request says.
//!
//! The C function itself is the app's, as are the app's own operations: this crate has no
//! `unsafe` code.

pub mod at_login;
mod failure;
pub mod held;
pub mod holding;
pub mod joining;
pub mod keystore;
pub mod pairing;
pub mod recovery;
pub mod setup;
pub mod spaces;
pub mod syncing;

use std::ffi::CString;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use keyring_core::CredentialStore;
use nibkeep_node::Library;
use serde_json::{Value as Json, json};
use zeroize::{Zeroize, Zeroizing};

pub use failure::Failure;

/// An app on the engine, as this crate serves it.
pub struct App {
	/// The app's name: of the replicas it makes, and of the app in the records of syncing.
	pub name: &'static str,
	/// The keystore that libraries use, made the first time one is needed, or `None` where
	/// there is none: on a computer, [`keystore::computer`]; on a phone, the app's own,
	/// which only the app's code reaches; in tests, one in memory.
	pub keystore: fn() -> Option<Arc<CredentialStore>>,
	/// How the app reads secrets, if it does.
	pub secrets: Option<Secrets>,
	/// What the app stops and forgets of the library as it goes, locked or replaced by
	/// another, besides what this crate stops: syncing, adding a device, joining a space.
	pub let_go: fn(),
}

/// How an app reads secrets: their key unlocked apart from the library's, by a passphrase
/// of their own or the keystore.
pub struct Secrets {
	/// Unlocks the secrets that the library kept, for an operation that reads or writes
	/// them, or says why it cannot.
	pub require: fn(&mut Library) -> Result<(), Failure>,
	/// Whether the secrets' key can go in the keystore here.
	pub keystore: fn() -> bool,
	/// The keystore service under which the secrets' key goes.
	pub service: fn() -> &'static str,
}

/// The app that named itself with [`start`].
static APP: OnceLock<&'static App> = OnceLock::new();

/// Names the app, as each of its calls begins: the engine records the name in the replicas
/// it opens, beside other programs. A process holds one app: the first named stays.
pub fn start(app: &'static App) {
	nibkeep_node::name_program(APP.get_or_init(|| app).name);
}

/// The app that named itself with [`start`].
///
/// # Panics
///
/// If no app did.
#[must_use]
pub fn app() -> &'static App {
	APP.get()
		.expect("an app names itself with nibkeep_app::start before it calls")
}

/// The app's way of reading secrets, if it named itself and reads them.
fn secrets() -> Option<&'static Secrets> {
	APP.get().and_then(|app| app.secrets.as_ref())
}

/// Answers a request with `dispatch`, given the request's operation, from its `op`, and the
/// rest of it: names the app first, wipes the request's text after, and gives a failure as
/// its answer, `{"error": message, "kind": kind}`.
pub fn answer(
	app: &'static App,
	mut request: Json,
	dispatch: impl FnOnce(&str, &mut Json) -> Result<Json, Failure>,
) -> Json {
	start(app);
	let op = take(&mut request, "op");
	let result = dispatch(&op, &mut request);
	wipe(&mut request);
	result.unwrap_or_else(|failure| failure.answer())
}

/// Answers a request, given as the bytes of its JSON, with `answer`, as the app's C
/// function does: a request that is no JSON is `null`, and a panic answers
/// `{"error": "the engine failed", "kind": "panic"}`. The answer comes as a C string,
/// written once into a buffer of its size, and the JSON it came from is wiped; free it with
/// [`free`], which wipes it too.
pub fn call(request: &[u8], answer: impl FnOnce(Json) -> Json) -> CString {
	let request: Json = serde_json::from_slice(request).unwrap_or(Json::Null);
	let mut answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| answer(request)))
		.unwrap_or_else(|_| json!({"error": "the engine failed", "kind": "panic"}));
	let text = to_c_string(&answered);
	wipe(&mut answered);
	text
}

/// Wipes an answer of [`call`] as it is freed.
pub fn free(answer: CString) {
	drop(Zeroizing::new(answer.into_bytes_with_nul()));
}

/// JSON as a C string, written once into a buffer of its size, so that no copy of a secret
/// in it is left behind by a buffer that grew.
#[must_use]
pub fn to_c_string(json: &Json) -> CString {
	/// Counts what is written to it.
	struct Counter(usize);
	impl io::Write for Counter {
		fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
			self.0 += bytes.len();
			Ok(bytes.len())
		}
		fn flush(&mut self) -> io::Result<()> {
			Ok(())
		}
	}
	let mut counter = Counter(0);
	let _ = serde_json::to_writer(&mut counter, json);
	let mut bytes = Vec::with_capacity(counter.0 + 1);
	let _ = serde_json::to_writer(&mut bytes, json);
	// JSON has no NUL bytes: serde_json writes them as \u0000.
	bytes.push(0);
	CString::from_vec_with_nul(bytes).unwrap_or_default()
}

/// Wipes the text in JSON, where secrets may be.
pub fn wipe(json: &mut Json) {
	match json {
		Json::String(text) => text.zeroize(),
		Json::Array(values) => values.iter_mut().for_each(wipe),
		Json::Object(map) => map.values_mut().for_each(wipe),
		Json::Null | Json::Bool(_) | Json::Number(_) => {}
	}
}

/// A request's text, taken out of it, so that it is wiped once used: empty if the request
/// has no text there.
pub fn take(request: &mut Json, name: &str) -> Zeroizing<String> {
	match request.get_mut(name).map(Json::take) {
		Some(Json::String(text)) => Zeroizing::new(text),
		_ => Zeroizing::new(String::new()),
	}
}

/// The wall clock, in milliseconds since 1970.
#[must_use]
pub fn wall_millis() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |since| {
			u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
		})
}

/// The wall clock, read anew each time, as the other programs on the replica read theirs:
/// the times of syncs that one records, another compares with its own, also after the
/// computer slept.
pub fn clock() -> impl Fn() -> u64 + Send + Sync + 'static {
	wall_millis
}

/// The device's offset from UTC now, in minutes.
#[must_use]
pub fn offset_minutes() -> i32 {
	jiff::tz::TimeZone::system()
		.to_offset(jiff::Timestamp::now())
		.seconds()
		/ 60
}

/// A mutex's lock, also after a thread panicked with it.
pub fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
