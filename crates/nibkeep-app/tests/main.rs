//! The crate's integration tests, one binary of them: each file is a module.

mod calls;
mod devices;
mod joining;
mod library;
mod recovery;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use keyring_core::CredentialStore;
use nibkeep_app::App;
use serde_json::{Value as Json, json};

/// The app of the tests: it reads no secrets.
static APP: App = App {
	name: "Example",
	keystore: mock,
	secrets: None,
	let_go,
};

/// Set, the tests' app locks the library the next time it lets go of it.
static LOCK_AS_IT_LETS_GO: AtomicBool = AtomicBool::new(false);

/// What the tests' app does as it lets go of the library: it locks it, once, if a test
/// asked, as the person may lock it at any moment, here at a moment the test knows.
fn let_go() {
	if LOCK_AS_IT_LETS_GO.swap(false, Ordering::SeqCst) {
		nibkeep_app::held::lock();
	}
}

/// Has the tests' app lock the library the next time it lets go of it: within a call that
/// makes, joins or brings back a library, once that library is there.
fn lock_as_it_lets_go() {
	LOCK_AS_IT_LETS_GO.store(true, Ordering::SeqCst);
}

/// A keystore in memory, never the computer's.
#[expect(clippy::unnecessary_wraps, reason = "as App::keystore asks")]
fn mock() -> Option<Arc<CredentialStore>> {
	Some(keyring_core::mock::Store::new().expect("a mock store"))
}

/// The library held is the process's, so the tests that use it take turns.
static TURNS: Mutex<()> = Mutex::new(());

/// This test's turn, with the app named, and never the person's syncing from login.
fn turn() -> MutexGuard<'static, ()> {
	let turn = TURNS.lock().unwrap_or_else(PoisonError::into_inner);
	LOCK_AS_IT_LETS_GO.store(false, Ordering::SeqCst);
	nibkeep_app::start(&APP);
	nibkeep_app::at_login::set_place(&mut json!({}));
	turn
}

/// A folder of the test's own, empty.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(std::env::temp_dir().join("nibkeep-app"), name)
}

/// Makes a library in a folder of its own, and holds it unlocked: the folder.
fn made(name: &str) -> String {
	let folder = folder(name).to_str().expect("UTF-8").to_owned();
	let made = ask(&json!({
		"op": "create", "folder": folder, "user": "Alex", "device": "laptop",
		"passphrase": "correct horse",
	}));
	assert_eq!(made, json!({}), "made");
	folder
}

/// Answers a request as an app's C function does, through JSON in bytes.
fn ask(request: &Json) -> Json {
	let text = nibkeep_app::call(request.to_string().as_bytes(), |request| {
		nibkeep_app::answer(&APP, request, dispatch)
	});
	let answer = serde_json::from_slice(text.as_bytes()).expect("JSON");
	nibkeep_app::free(text);
	answer
}

/// The operations of the tests' app: the crate's, by the names the apps give them.
fn dispatch(op: &str, request: &mut Json) -> Result<Json, nibkeep_app::Failure> {
	use nibkeep_app::{held, holding, joining, pairing, recovery, setup, spaces};
	match op {
		"library_at" => setup::library_at(&nibkeep_app::take(request, "folder")),
		"create" => setup::create(request),
		"join" => setup::join(request),
		"from_copy" => setup::from_copy(request),
		"unlock" => setup::unlock(request),
		"create_and_join" => joining::create_and_join(request),
		"join_space" => joining::join_space(request),
		op if recovery::CLOSED.contains(&op) => recovery::answer(op, request),
		"add_device" => pairing::add_device(request),
		"pairing" => Ok(pairing::pairing()),
		"stop_adding" => {
			pairing::stop_adding();
			Ok(json!({}))
		}
		"lock" => {
			held::lock();
			Ok(json!({}))
		}
		"panic" => panic!("as asked"),
		op => held::with_open(|library| match op {
			"spaces_here" => holding::spaces_here(library),
			"take_new_spaces" => holding::take_new_spaces(library, request),
			"create_space" => spaces::create(library, request),
			"new_recovery_code" => Ok(recovery::new_code()),
			"confirm_recovery_code" => recovery::confirm(library, request),
			"check_recovery_code" => recovery::check(library, request),
			_ => Err(nibkeep_app::Failure::Request(format!(
				"there is no operation {op}"
			))),
		}),
	}
}
