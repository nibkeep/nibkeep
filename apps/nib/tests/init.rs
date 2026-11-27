//! `nib init` and `nib status`: a library for a user, this device, a replica on it, and a
//! personal space, in a folder of its own, which commands find as Git finds a repository.

use std::path::Path;

use crate::common::Test;
use nibkeep_cli::CliError;

/// What `nib status` says of a new library in this folder, which it names as the file
/// system resolves it.
fn status_in(folder: &Path) -> String {
	let folder = nibkeep_node::resolved_path(folder).expect("resolves");
	format!(
		"Ada, on laptop, in {}\nPersonal: 5 entities, 7 changes\n",
		folder.display()
	)
}

#[test]
fn init_starts_a_library_that_status_describes() {
	let mut test = Test::new("init and status");
	test.answer(&["correct horse", "correct horse"]);
	assert_eq!(
		test.nib(&["init", "--name", "Ada"]).expect("starts"),
		status_in(&test.here)
	);
	assert_eq!(test.asked, 2, "the passphrase, and again to confirm it");
	test.answer(&["correct horse"]);
	assert_eq!(
		test.nib(&["status"]).expect("describes"),
		status_in(&test.here)
	);

	test.answer(&["correct horse"]);
	let status = test.json(&["status"]);
	let resolved = nibkeep_node::resolved_path(&test.here).expect("resolves");
	assert_eq!(status["directory"], resolved.to_str().expect("UTF-8"));
	assert_eq!(status["user"]["name"], "Ada");
	assert_eq!(status["device"]["name"], "laptop");
	assert_eq!(status["replica"]["name"], "nib");
	assert_eq!(status["spaces"][0]["name"], "Personal");
	assert_eq!(status["spaces"][0]["entities"], 5);
	assert_eq!(status["spaces"][0]["changes"], 7);
	for id in [
		&status["user"]["id"],
		&status["device"]["id"],
		&status["replica"]["id"],
		&status["spaces"][0]["id"],
	] {
		let id = id.as_str().expect("an ID as text");
		assert!(id.parse::<nibkeep_base::EntityId>().is_ok(), "{id}");
	}
}

#[test]
fn without_a_name_the_user_is_named_after_the_account() {
	// The account's name, as whoami prints it, for the user, and the full name for the
	// person the user is, whose name the user goes by.
	let mut test = Test::with_keystore("init without a name");
	let started = test.nib(&["init"]).expect("starts");
	assert!(
		started.starts_with("Ada Lovelace, on laptop, in "),
		"{started}"
	);
	assert_eq!(test.json(&["status"])["user"]["name"], "Ada Lovelace");
	let users = test.nib(&["list", "user"]).expect("lists");
	assert!(users.contains("  ada  "), "{users}");
	let people = test.nib(&["list", "person"]).expect("lists");
	assert!(people.contains("  Ada Lovelace  "), "{people}");
	// Without a full name, or with the account's, the person takes the account's name; and
	// an argument that only a new library takes makes one, here where --library says.
	for (full_name, folder) in [(None, "none"), (Some(" ada "), "same")] {
		test.account.full_name = full_name.map(str::to_owned);
		let library = test.here.join(folder);
		let library = library.to_str().expect("UTF-8");
		test.nib(&["--library", library, "init", "--device", "phone"])
			.expect("starts");
		let people = test
			.nib(&["--library", library, "list", "person"])
			.expect("lists");
		assert!(people.contains("  ada  "), "{people}");
	}
	// Init alone makes a library where --library names a folder that holds none.
	let library = test.here.join("named");
	let named = library.to_str().expect("UTF-8");
	let started = test.nib(&["--library", named, "init"]).expect("starts");
	assert!(started.contains(named), "{started}");
	assert!(nibkeep_node::Library::replica_folder(&library.join(".nib")).is_ok());
	// A system that names no account leaves the name to be given.
	test.account = nibkeep_cli::Account::default();
	let library = test.here.join("nameless");
	let library = library.to_str().expect("UTF-8");
	let refused = test.nib(&["--library", library, "init"]);
	assert!(
		matches!(refused, Err(CliError::NoAccountName)),
		"{refused:?}"
	);
	assert!(!test.here.join("nameless").join(".nib").exists());
	test.nib(&["--library", library, "init", "--name", "Ada"])
		.expect("starts with the name given");
}

#[test]
fn with_a_keystore_no_passphrase_is_asked_for() {
	let mut test = Test::with_keystore("init with a keystore");
	let started = test
		.nib(&["init", "--name", "Ada", "--device", "phone"])
		.expect("starts");
	assert_eq!(
		started,
		status_in(&test.here).replace("on laptop", "on phone")
	);
	assert_eq!(test.nib(&["status"]).expect("describes"), started);
	assert_eq!(test.asked, 0);
}

#[test]
fn passphrase_overrides_the_keystore() {
	let mut test = Test::with_keystore("init with a passphrase");
	test.answer(&["correct horse", "correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada", "--passphrase"])
		.expect("starts");
	test.nib(&["status"]).expect("describes");
	assert_eq!(test.asked, 3);
}

#[test]
fn passphrases_that_differ_start_nothing() {
	let mut test = Test::new("init with different passphrases");
	test.answer(&["correct horse", "correct hose"]);
	let error = test.nib(&["init", "--name", "Ada"]).expect_err("differ");
	assert_eq!(error.to_string(), "the passphrases differ");
	assert!(!test.here.join(".nib").exists());
}

#[test]
fn init_makes_the_current_folder_a_library_and_keeps_its_files() {
	let mut test = Test::with_keystore("init in a folder");
	std::fs::write(test.here.join("notes.txt"), "Milk.\n").expect("writes");
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	// What the library keeps is in `.nib`, and the rest of the folder is as it was.
	let mut entries: Vec<String> = std::fs::read_dir(&test.here)
		.expect("reads the folder")
		.map(|entry| {
			let entry = entry.expect("an entry");
			entry.file_name().to_string_lossy().into_owned()
		})
		.collect();
	entries.sort();
	assert_eq!(entries, [".nib", "notes.txt"]);
	let data = nibkeep_node::Library::replica_folder(&test.here.join(".nib")).expect("a replica");
	for kept in ["place.cbor", "keys.cbor", "library.cbor", "spaces"] {
		assert!(data.join(kept).exists(), "{kept}");
	}
	assert_eq!(
		std::fs::read_to_string(test.here.join("notes.txt")).expect("reads"),
		"Milk.\n"
	);
}

#[test]
fn commands_find_the_library_from_the_folders_in_it() {
	let mut test = Test::with_keystore("the folders of a library");
	let folder = test.here.clone();
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	for inside in [
		folder.join("Covers").join("Books"),
		folder.join(".nib").join("spaces"),
	] {
		std::fs::create_dir_all(&inside).expect("makes the folder");
		test.here = inside;
		assert_eq!(
			test.nib(&["status"]).expect("finds the library"),
			status_in(&folder)
		);
	}
	// A `.nib` that holds no library, such as an empty one, is passed by.
	let empty = folder.join("Covers").join(".nib");
	std::fs::create_dir_all(&empty).expect("makes the folder");
	test.here = folder.join("Covers");
	assert_eq!(
		test.nib(&["status"]).expect("passes it by"),
		status_in(&folder)
	);
	// Relative to the current folder, `..` and all.
	assert_eq!(
		test.nib(&["--library", "..", "status"]).expect("opens"),
		status_in(&folder)
	);
	std::fs::remove_dir(&empty).expect("removes it");
	// A library in a folder of another library's is a library of its own.
	let inner = folder.join("Covers");
	test.here.clone_from(&inner);
	test.nib(&["init", "--name", "Ada"])
		.expect("starts another");
	test.here = inner.join("Books");
	assert_eq!(test.nib(&["status"]).expect("finds it"), status_in(&inner));
}

#[test]
fn outside_a_library_commands_say_so() {
	let mut test = Test::with_keystore("outside a library");
	assert_eq!(
		test.nib(&["status"]).expect_err("in none").to_string(),
		"this folder is not in a library; if it is a tree's folder that lost its .nib file, run nib init FOLDER in your library's folder; otherwise make a library with nib init, or give one with --library or NIBKEEP_LIBRARY"
	);
	// --library names the library's folder, wherever the current folder is, relative to it
	// or not.
	let elsewhere = test.here.join("elsewhere");
	let named = elsewhere.to_str().expect("UTF-8");
	assert_eq!(
		test.nib(&["--library", named, "status"])
			.expect_err("none there")
			.to_string(),
		format!("there is no library in {named}")
	);
	test.nib(&["--library", named, "init", "--name", "Ada"])
		.expect("starts");
	assert!(nibkeep_node::Library::replica_folder(&elsewhere.join(".nib")).is_ok());
	for given in [named, "elsewhere"] {
		assert_eq!(
			test.nib(&["--library", given, "status"]).expect("opens"),
			status_in(&elsewhere)
		);
	}
	assert!(matches!(test.nib(&["status"]), Err(CliError::NotInLibrary)));
}

#[test]
fn nibkeep_library_counts_only_outside_every_library_s_folder() {
	let mut test = Test::with_keystore("a default library");
	let outside = test.here.clone();
	let default = outside.join("default");
	test.nib(&[
		"--library",
		default.to_str().expect("UTF-8"),
		"init",
		"--name",
		"Ada",
	])
	.expect("starts the default library");
	test.default_library = Some(default.clone());
	assert_eq!(
		test.nib(&["status"]).expect("opens it"),
		status_in(&default)
	);
	// nib init starts a library in the current folder all the same, and commands in that
	// folder work in it.
	let other = outside.join("other");
	std::fs::create_dir_all(&other).expect("makes the folder");
	test.here.clone_from(&other);
	test.nib(&["init", "--name", "Ada"]).expect("starts here");
	assert_eq!(test.nib(&["status"]).expect("opens it"), status_in(&other));
	// A default that holds no library is named.
	test.here = outside;
	test.default_library = Some(other.join("missing"));
	assert!(matches!(
		test.nib(&["status"]),
		Err(CliError::NoDefaultLibrary(folder)) if folder.ends_with("missing")
	));
	// Relative to every folder in turn, it would name another library in each.
	test.default_library = Some("default".into());
	assert_eq!(
		test.nib(&["status"]).expect_err("relative").to_string(),
		"NIBKEEP_LIBRARY names default, which is not a full path"
	);
}

#[test]
fn no_library_is_started_in_another_s_own_folder() {
	let mut test = Test::with_keystore("a library in a library's data");
	let folder = test.here.clone();
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	let data = folder.join(".nib");
	test.here = data.join("spaces");
	let error = test
		.nib(&["init", "--name", "Ada"])
		.expect_err("in .nib")
		.to_string();
	assert_eq!(
		error,
		format!(
			"{} is in a library's own folder .nib; start the library elsewhere",
			data.join("spaces").display()
		)
	);
	test.here.clone_from(&folder);
	assert!(matches!(
		test.nib(&[
			"--library",
			data.to_str().expect("UTF-8"),
			"init",
			"--name",
			"Ada"
		]),
		Err(CliError::InLibraryData(_))
	));
	assert!(!data.join(".nib").exists());
	// Nor through a link into it, which makes nothing there.
	#[cfg(unix)]
	{
		std::os::unix::fs::symlink(&data, folder.join("link")).expect("links");
		let through = folder.join("link").join("new").join("deeper");
		let through = through.to_str().expect("UTF-8");
		assert!(matches!(
			test.nib(&["--library", through, "init", "--name", "Ada"]),
			Err(CliError::InLibraryData(_))
		));
		assert!(!data.join("new").exists());
	}
}

#[test]
fn a_failed_init_makes_no_folder() {
	let mut test = Test::new("a failed init");
	let folder = test.here.join("new").join("library");
	let missing = test.here.join("missing passphrase");
	let (folder, missing) = (
		folder.to_str().expect("UTF-8"),
		missing.to_str().expect("UTF-8"),
	);
	let arguments = ["--passphrase-file", missing, "--library", folder];
	assert!(
		test.nib(&[&arguments[..], &["init", "--name", "Ada"]].concat())
			.is_err()
	);
	assert!(!test.here.join("new").exists());
}

#[test]
fn errors_say_what_went_wrong() {
	let mut test = Test::new("errors");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	test.answer(&["correct hose"]);
	assert_eq!(
		test.nib(&["status"])
			.expect_err("a wrong passphrase")
			.to_string(),
		"the passphrase is wrong, or the key file was changed"
	);
	assert_eq!(
		test.nib(&["init", "--name", "Ada"])
			.expect_err("a second library")
			.to_string(),
		"there is a library here already"
	);
}

#[test]
fn a_missing_keystore_is_named() {
	let mut test = Test::with_keystore("a missing keystore");
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	test.keystore = false;
	assert_eq!(
		test.nib(&["status"]).expect_err("no keystore").to_string(),
		"the library's keys are in the platform's keystore, which is not available"
	);
}

#[test]
fn an_unfinished_library_asks_for_nothing() {
	let mut test = Test::new("an unfinished library");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	let replica =
		nibkeep_node::Library::replica_folder(&test.here.join(".nib")).expect("a replica");
	std::fs::remove_file(replica.join("library.cbor")).expect("removes the library file");
	let asked = test.asked;
	// It names the replica's folder, not the `.nib`, which may hold copies of others, nor
	// the folder that holds the person's own files too.
	// As nib shows it, resolved, but on Windows without the `\\?\` of a resolved path.
	let shown = std::fs::canonicalize(&replica)
		.expect("resolves")
		.display()
		.to_string();
	let unfinished = format!(
		"a library was started in {}, but not finished; remove that folder to start again",
		shown.strip_prefix(r"\\?\").unwrap_or(&shown)
	);
	for arguments in [&["status"][..], &["init", "--name", "Ada"]] {
		assert_eq!(
			test.nib(arguments).expect_err("unfinished").to_string(),
			unfinished
		);
	}
	assert_eq!(test.asked, asked, "no passphrase was asked for");
}

/// A file that holds a passphrase, readable by its owner alone.
fn passphrase_file(test: &Test, name: &str, contents: &str) -> std::path::PathBuf {
	let folder = test.here.with_extension("passphrases");
	std::fs::create_dir_all(&folder).expect("makes a folder");
	let path = folder.join(name);
	std::fs::write(&path, contents).expect("writes the file");
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
			.expect("makes it private");
	}
	path
}

#[test]
fn a_file_can_give_the_passphrase() {
	let mut test = Test::with_keystore("a passphrase file");
	let file = passphrase_file(&test, "passphrase", "correct horse\n");
	let file = file.to_str().expect("UTF-8");
	assert_eq!(
		test.nib(&["--passphrase-file", file, "init", "--name", "Ada"])
			.expect("starts"),
		status_in(&test.here)
	);
	assert_eq!(test.asked, 0, "nothing is asked for");
	assert_eq!(
		test.nib(&["status", "--passphrase-file", file])
			.expect("opens"),
		status_in(&test.here)
	);
	assert_eq!(test.asked, 0);
	// The file gave a passphrase, rather than the keystore protecting the keys.
	test.answer(&["correct horse"]);
	test.nib(&["status"])
		.expect("opens with the passphrase typed");
	assert_eq!(test.asked, 1);

	let wrong = passphrase_file(&test, "wrong", "correct hose");
	assert_eq!(
		test.nib(&[
			"--passphrase-file",
			wrong.to_str().expect("UTF-8"),
			"status"
		])
		.expect_err("a wrong passphrase")
		.to_string(),
		"the passphrase is wrong, or the key file was changed"
	);
}

#[test]
fn a_passphrase_file_must_be_private_and_hold_one() {
	let mut test = Test::new("a bad passphrase file");
	let empty = passphrase_file(&test, "empty", "\nsecond line");
	assert_eq!(
		test.nib(&[
			"--passphrase-file",
			empty.to_str().expect("UTF-8"),
			"init",
			"--name",
			"Ada"
		])
		.expect_err("empty")
		.to_string(),
		"the passphrase is empty"
	);
	let missing = empty.with_file_name("missing");
	let error = test
		.nib(&[
			"--passphrase-file",
			missing.to_str().expect("UTF-8"),
			"init",
			"--name",
			"Ada",
		])
		.expect_err("missing")
		.to_string();
	assert!(
		error.starts_with(&format!(
			"the passphrase file {} cannot be read: ",
			missing.display()
		)),
		"{error}"
	);
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let open = passphrase_file(&test, "open", "correct horse");
		std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o640))
			.expect("lets the group read it");
		assert_eq!(
			test.nib(&[
				"--passphrase-file",
				open.to_str().expect("UTF-8"),
				"init",
				"--name",
				"Ada"
			])
			.expect_err("readable by others")
			.to_string(),
			format!(
				"others can read the passphrase file {}; make it yours alone, such as with chmod 600",
				open.display()
			)
		);
	}
	assert!(!test.here.join(".nib").exists(), "nothing was started");
	assert_eq!(test.asked, 0);
}

/// Declares that `folder` is a drive of its own, with the serial `serial`, as a backup's or
/// a program's that syncs folders would be.
fn drive(folder: &std::path::Path, serial: &str) {
	std::fs::create_dir_all(folder).expect("makes the folder");
	nibkeep_node::declare_drive(
		folder,
		nibkeep_node::Drive {
			serial: Some(serial.as_bytes().to_vec()),
			folder_ids: true,
			..nibkeep_node::Drive::default()
		},
		Some(b"desktop".to_vec()),
	);
}

#[test]
fn a_copy_says_how_to_make_this_computer_a_device_and_starts_no_library() {
	let mut test = Test::new("a copy");
	let (original, restored) = (test.here.join("original"), test.here.join("restored"));
	drive(&original, "old disk");
	drive(&restored, "new disk");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&[
		"--library",
		original.to_str().expect("text"),
		"init",
		"--name",
		"Ada",
	])
	.expect("starts");
	nibkeep_testkit::copy_folder(&original, &restored);
	let given = restored.to_str().expect("text");
	let said = test
		.nib(&["--library", given, "status"])
		.expect_err("a copy")
		.to_string();
	assert!(said.contains("is a copy"), "{said}");
	assert!(said.contains("nib init --join"), "{said}");
	let refused = test
		.nib(&["--library", given, "init", "--name", "Ada"])
		.expect_err("among copies")
		.to_string();
	assert!(refused.contains("holds a copy of a library"), "{refused}");
	// Commands below the copy's folder find it, rather than a library above it.
	test.here = restored.join("notes");
	std::fs::create_dir_all(&test.here).expect("makes a folder");
	assert!(
		test.nib(&["status"])
			.expect_err("a copy")
			.to_string()
			.contains("is a copy")
	);
}

#[test]
fn a_stick_without_a_journal_holds_a_library_only_with_allow_fat() {
	let mut test = Test::new("on a stick");
	let stick = test.here.join("stick");
	std::fs::create_dir_all(&stick).expect("makes the folder");
	nibkeep_node::declare_drive(
		&stick,
		nibkeep_node::Drive {
			serial: Some(b"EBE7-F414".to_vec()),
			unjournaled: true,
			..nibkeep_node::Drive::default()
		},
		Some(b"desktop".to_vec()),
	);
	let given = stick.to_str().expect("text");
	let refused = test
		.nib(&["--library", given, "init", "--name", "Ada"])
		.expect_err("no journal")
		.to_string();
	assert!(refused.contains("keeps no journal"), "{refused}");
	assert!(refused.contains("--allow-fat"), "{refused}");
	assert_eq!(test.asked, 0, "refused before a passphrase is asked for");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["--library", given, "init", "--name", "Ada", "--allow-fat"])
		.expect("starts");
	test.answer(&["correct horse"]);
	test.nib(&["--library", given, "status"]).expect("opens");
	// As a stick pulled out while the library was open leaves it.
	let replica = nibkeep_node::Library::replica_folder(&stick.join(".nib")).expect("a replica");
	std::fs::write(replica.join("in-use"), b"").expect("writes");
	let said = test
		.nib(&["--library", given, "status"])
		.expect_err("removed while open")
		.to_string();
	assert!(said.contains("check the drive for errors"), "{said}");
	assert!(said.contains("nib verify --drive-checked"), "{said}");
	test.answer(&["correct horse"]);
	test.nib(&["--library", given, "verify", "--drive-checked"])
		.expect("checked and verified");
	test.answer(&["correct horse"]);
	test.nib(&["--library", given, "status"])
		.expect("opens again");
}

#[cfg(unix)]
#[test]
fn a_folder_that_nib_may_not_read_is_named_so_and_not_as_holding_no_library() {
	use std::os::unix::fs::PermissionsExt as _;
	let mut test = Test::new("no access");
	let folder = test.here.join("closed");
	test.answer(&["correct horse", "correct horse"]);
	let given = folder.to_str().expect("text").to_owned();
	test.nib(&["--library", &given, "init", "--name", "Ada"])
		.expect("starts");
	// As macOS keeps a removable drive from a terminal until the person allows it.
	std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o000)).expect("closes");
	let said = test
		.nib(&["--library", &given, "status"])
		.expect_err("no access");
	std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).expect("opens");
	let said = said.to_string();
	assert!(said.contains("nib may not read"), "{said}");
	assert!(said.contains("Files and Folders"), "{said}");
}

#[test]
fn a_program_that_syncs_the_librarys_folder_is_told_once() {
	let mut test = Test::new("synced by a program");
	let folder = test.here.join("synced");
	drive(&folder, "disk");
	// Syncthing marks the folders it syncs with a folder of its own.
	std::fs::create_dir_all(folder.join(".stfolder")).expect("makes the mark");
	test.answer(&["correct horse", "correct horse"]);
	let given = folder.to_str().expect("text");
	test.nib(&["--library", given, "init", "--name", "Ada"])
		.expect("starts");
	test.answer(&["correct horse"]);
	test.nib(&["--library", given, "status"]).expect("opens");
	assert_eq!(test.notes.len(), 1, "{:?}", test.notes);
	assert!(test.notes[0].contains("syncs this library's folder"));
	test.answer(&["correct horse"]);
	test.nib(&["--library", given, "status"]).expect("opens");
	assert_eq!(test.notes.len(), 1, "told once");
}

#[test]
fn from_copy_makes_this_computer_a_device_of_its_own_with_the_copys_keys() {
	let mut test = Test::new("from a copy");
	let (original, restored) = (test.here.join("original"), test.here.join("restored"));
	drive(&original, "old disk");
	drive(&restored, "new disk");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&[
		"--library",
		original.to_str().expect("text"),
		"init",
		"--name",
		"Ada",
	])
	.expect("starts");
	nibkeep_testkit::copy_folder(&original, &restored);
	let given = restored.to_str().expect("text");
	// Its own passphrase, then a new one for this device's, twice.
	test.answer(&["correct horse", "battery staple", "battery staple"]);
	let said = test
		.nib_printing(&["--library", given, "recovery", "from-copy", "--passphrase"])
		.0;
	assert!(said.contains("a device of its own from the copy"), "{said}");
	test.answer(&["battery staple"]);
	test.nib(&["--library", given, "status"])
		.expect("opens as a device of its own");
	// A copy no longer here: nothing to make a device from.
	let empty = test.here.join("empty");
	drive(&empty, "third disk");
	assert!(
		test.nib(&[
			"--library",
			empty.to_str().expect("text"),
			"recovery",
			"from-copy"
		])
		.expect_err("no copy")
		.to_string()
		.contains("no copy")
	);
}

#[test]
fn the_secrets_of_a_copy_are_taken_later_with_secret_from_copy() {
	let mut test = Test::with_keystore("secrets later");
	let (original, restored) = (test.here.join("original"), test.here.join("restored"));
	drive(&original, "old disk");
	drive(&restored, "new disk");
	let (old, new) = (
		original.to_str().expect("text").to_owned(),
		restored.to_str().expect("text").to_owned(),
	);
	test.nib(&["--library", &old, "init", "--name", "Ada"])
		.expect("starts");
	test.nib(&[
		"--library",
		&old,
		"type",
		"add",
		"login",
		"title:text",
		"password:text:secret",
	])
	.expect("adds the type");
	test.nib(&["--library", &old, "new", "login", "title=Bank"])
		.expect("adds the login");
	test.answer(&["tr0ub4dor", "tr0ub4dor", "hunter2", "hunter2"]);
	test.nib(&["--library", &old, "secret", "set", "Bank", "password"])
		.expect("sets the password");
	nibkeep_testkit::copy_folder(&original, &restored);
	// The copy's keys are in this computer's keystore; its secrets are left for now.
	test.answer(&[""]);
	test.nib(&["--library", &new, "recovery", "from-copy"])
		.expect("adds this computer");
	// A passphrase for the secrets here, twice, then the copy's secrets'.
	test.answer(&["s3cret here", "s3cret here", "tr0ub4dor"]);
	let (said, taken) = test.nib_printing(&["--library", &new, "secret", "from-copy"]);
	taken.expect("takes the secrets");
	assert!(said.contains("holds the secrets"), "{said}");
	test.answer(&["s3cret here"]);
	let (shown, result) =
		test.nib_printing(&["--library", &new, "secret", "show", "Bank", "password"]);
	result.expect("shows");
	assert!(shown.contains("hunter2"), "{shown}");
}

/// The entries of the engines file of the library in `directory`, as `[name, reads, engine,
/// opened]`, after the replica's version.
fn engines(directory: &Path) -> (u64, Vec<Vec<nibkeep_base::cbor::Value>>) {
	use nibkeep_base::cbor::Value;
	let folder = nibkeep_node::Library::replica_folder(directory).expect("a replica");
	let bytes = std::fs::read(folder.join("engines.cbor")).expect("reads");
	let Value::Array(items) = nibkeep_base::cbor::decode(&bytes).expect("decodes") else {
		panic!("an array");
	};
	let (Value::Integer(version), Value::Array(programs)) = (&items[0], &items[1]) else {
		panic!("a version and the programs");
	};
	let programs = programs
		.iter()
		.map(|program| match program {
			Value::Array(items) => items.clone(),
			other => panic!("an entry: {other:?}"),
		})
		.collect();
	(u64::try_from(*version).expect("a version"), programs)
}

#[test]
fn nib_records_itself_in_the_replica_and_asks_to_be_updated_for_a_later_one() {
	use nibkeep_base::cbor::{Integer, Value};
	let mut test = Test::new("init records nib");
	test.answer(&["correct horse", "correct horse"]);
	test.nib(&["init", "--name", "Ada"]).expect("starts");
	let directory = test.here.join(".nib");
	let (version, programs) = engines(&directory);
	assert_eq!(version, nibkeep_node::REPLICA_VERSION);
	assert_eq!(programs.len(), 1, "{programs:?}");
	assert_eq!(
		programs[0][..3],
		[
			Value::from("nib"),
			Value::Integer(Integer::from(nibkeep_node::REPLICA_VERSION)),
			Value::from(nibkeep_node::ENGINE_VERSION),
		]
	);
	// A later version of NibKeep opened the library since.
	let folder = nibkeep_node::Library::replica_folder(&directory).expect("a replica");
	std::fs::write(
		folder.join("engines.cbor"),
		nibkeep_base::cbor::encode(&Value::Array(vec![
			Value::Integer(Integer::from(nibkeep_node::REPLICA_VERSION + 1)),
			Value::Array(vec![]),
		])),
	)
	.expect("writes");
	let error = test.nib(&["status"]).expect_err("refuses");
	assert!(error.to_string().contains("update"), "{error}");
	assert_eq!(test.asked, 2, "no passphrase was asked for in vain");
}
