//! Secret files: content that only the replicas holding the secrets read, sealed in the
//! store, with no record of where it was found or of its plain copies (technical design: secret files).

use crate::common;

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::common::{library, now, this_day, today, values};
use nibkeep_base::{EntityId, Hash};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::Value;
use nibkeep_node::{Edit, Library, NewProtection, NodeError, Unlocker};
use nibkeep_std::{ATTACHED_TO, MEDIA_TYPE, SECRET_CONTENT, SECRET_FILE, SIZE};
use nibkeep_testkit::TestRng;

/// A scan, as a PDF's first bytes tell it, with a sentinel that no file may hold in plain.
fn scan() -> Vec<u8> {
	let mut scan = b"%PDF-1.7\n".to_vec();
	scan.extend(b"a scan of a passport, s3ntinel-scan ".repeat(3_000));
	scan
}

const SENTINEL: &[u8] = b"s3ntinel-scan";

fn secrets_passphrase() -> NewProtection<'static> {
	NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"))
}

const SECRETS: Unlocker<'static> = Unlocker::Passphrase("tr0ub4dor");

/// A library whose secrets are set up and unlocked, with a login to attach files to.
fn with_secrets(test: &str, rng: &mut TestRng) -> (PathBuf, Library, EntityId) {
	let (directory, mut library) = library(test, rng);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, rng)
		.expect("sets up");
	let personal = library.personal_space();
	library
		.install_standard(personal, &[nibkeep_std::LOGIN], now() + 600, rng)
		.expect("installs");
	let login = library
		.create_entity(
			personal,
			&[nibkeep_std::LOGIN],
			vec![(
				nibkeep_model::fields::NAME,
				Value::Name(nibkeep_model::Name::new("Passport office")),
			)],
			now() + 700,
			rng,
		)
		.expect("creates");
	(directory, library, login)
}

fn add_scan(
	library: &mut Library,
	attached_to: &[EntityId],
	at: u64,
	rng: &mut TestRng,
) -> nibkeep_node::AddedSecretFile {
	let personal = library.personal_space();
	library
		.add_secret_file(
			personal,
			"passport.pdf",
			scan().as_slice(),
			attached_to,
			at,
			rng,
		)
		.expect("adds")
}

fn read(library: &Library, file: EntityId) -> Result<Vec<u8>, NodeError> {
	let mut reader = library.open_secret_file(library.personal_space(), file)?;
	let mut content = Vec::new();
	reader.read_to_end(&mut content).expect("reads");
	Ok(content)
}

/// How many sealed blobs a library holds on its disk.
fn sealed_blobs(directory: &Path) -> usize {
	let mut count = 0;
	let mut to_visit = vec![directory.join(".nib")];
	if !to_visit[0].exists() {
		to_visit = vec![directory.to_path_buf()];
	}
	while let Some(next) = to_visit.pop() {
		for entry in std::fs::read_dir(&next).expect("lists") {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				to_visit.push(path);
			} else if path
				.parent()
				.and_then(Path::parent)
				.and_then(Path::file_name)
				.is_some_and(|name| name == "blake3")
			{
				count += 1;
			}
		}
	}
	count
}

/// How many sealed files of its personal space a library reads, and how many wait for keys
/// it cannot open, as `nib verify` counts them.
fn readable_and_waiting(library: &mut Library) -> (usize, usize) {
	let personal = library.personal_space();
	let report = library.verify(now() + 100_000).expect("verifies");
	let space = report
		.spaces
		.iter()
		.find(|space| space.id == personal)
		.expect("the personal space");
	(space.blobs, space.waiting)
}

/// Whether the library's files hold the content's hash anywhere in plain, as bytes or as
/// text.
fn holds_hash(directory: &Path, content: &Hash) -> bool {
	nibkeep_testkit::holds_anywhere(directory, &content.to_multihash())
		|| nibkeep_testkit::holds_anywhere(directory, content.to_string().as_bytes())
}

#[test]
fn a_secret_file_is_read_only_while_the_secrets_are_unlocked() {
	let mut rng = TestRng::new("secret file");
	let (directory, mut library, login) = with_secrets("secret file", &mut rng);
	let added = add_scan(&mut library, &[login], now() + 1_000, &mut rng);
	assert_eq!(added.media_type.as_deref(), Some("application/pdf"));
	assert_eq!(added.size, scan().len() as u64);
	assert_eq!(added.content, Hash::of(&scan()));
	assert!(!added.unprotected_copy);
	assert_eq!(read(&library, added.file).expect("reads"), scan());

	let personal = library.personal_space();
	let view = library
		.entity(personal, added.file, this_day())
		.expect("reads")
		.expect("the file");
	assert_eq!(view.types, [SECRET_FILE]);
	assert_eq!(view.name.as_deref(), Some("passport.pdf"));
	assert_eq!(
		values(&library, added.file, &MEDIA_TYPE),
		[Value::Text("application/pdf".to_owned())]
	);
	assert_eq!(
		values(&library, added.file, &ATTACHED_TO),
		[Value::Reference(login)]
	);
	assert_eq!(
		values(&library, added.file, &SECRET_CONTENT),
		[Value::Blob(added.content)]
	);
	// Its name is ordinary, so that a search finds it without the secrets.
	let found = library.search(personal, "passport").expect("searches");
	assert!(
		found.iter().any(|listed| listed.id == added.file),
		"{found:?}"
	);

	library.lock_secrets();
	assert!(matches!(
		read(&library, added.file),
		Err(NodeError::SecretsLocked)
	));
	nibkeep_testkit::assert_empty!(values(&library, added.file, &SECRET_CONTENT));
	assert_eq!(
		values(&library, added.file, &SIZE),
		[Value::Integer(i64::try_from(scan().len()).expect("small"))]
	);
	assert!(
		library
			.search(personal, "passport")
			.expect("searches")
			.iter()
			.any(|listed| listed.id == added.file)
	);
	drop(library);

	// Nothing on the disk holds the content or its hash in plain.
	assert!(!nibkeep_testkit::holds_anywhere(&directory, SENTINEL));
	assert!(!holds_hash(&directory, &added.content));
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	assert!(matches!(
		read(&library, added.file),
		Err(NodeError::SecretsLocked)
	));
	library.unlock_secrets(&SECRETS).expect("unlocks");
	assert_eq!(read(&library, added.file).expect("reads"), scan());
}

#[test]
fn a_secret_file_needs_the_secrets() {
	let mut rng = TestRng::new("secret file needs secrets");
	let (directory, mut library) = library("secret file needs secrets", &mut rng);
	let personal = library.personal_space();
	let before = sealed_blobs(&directory);
	let added = library.add_secret_file(
		personal,
		"scan.pdf",
		scan().as_slice(),
		&[],
		now(),
		&mut rng,
	);
	assert!(matches!(added, Err(NodeError::SecretsLocked)), "{added:?}");
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up");
	library.lock_secrets();
	let added = library.add_secret_file(
		personal,
		"scan.pdf",
		scan().as_slice(),
		&[],
		now() + 1_000,
		&mut rng,
	);
	assert!(matches!(added, Err(NodeError::SecretsLocked)), "{added:?}");
	assert_eq!(sealed_blobs(&directory), before, "nothing is sealed");
	nibkeep_testkit::assert_empty!(
		library
			.entities(personal, Some(&SECRET_FILE))
			.expect("lists")
	);
}

#[test]
fn a_secret_file_is_attached_only_to_entities_of_the_space() {
	let mut rng = TestRng::new("secret file attached to nothing");
	let (_, mut library, _) = with_secrets("secret file attached to nothing", &mut rng);
	let personal = library.personal_space();
	let nowhere = EntityId::random(&mut rng);
	let added = library.add_secret_file(
		personal,
		"scan.pdf",
		scan().as_slice(),
		&[nowhere],
		now() + 1_000,
		&mut rng,
	);
	assert!(matches!(added, Err(NodeError::NoEntity(_))), "{added:?}");
	// A file attached to nothing is a record of its own.
	let alone = add_scan(&mut library, &[], now() + 2_000, &mut rng);
	nibkeep_testkit::assert_empty!(values(&library, alone.file, &ATTACHED_TO));
}

#[test]
fn content_that_an_ordinary_file_holds_too_is_reported() {
	let mut rng = TestRng::new("secret file held plainly too");
	let (directory, mut library, _) = with_secrets("secret file held plainly too", &mut rng);
	let original = directory.join("passport.pdf");
	std::fs::write(&original, scan()).expect("writes");
	let personal = library.personal_space();
	library
		.add_file(personal, &original, now() + 1_000, &mut rng)
		.expect("adds");
	let added = add_scan(&mut library, &[], now() + 2_000, &mut rng);
	assert!(added.unprotected_copy);
	assert_eq!(read(&library, added.file).expect("reads"), scan());
}

#[test]
fn content_in_a_secret_field_must_be_sealed_for_the_secrets() {
	let mut rng = TestRng::new("secret content sealed plainly");
	let (_, mut library, _) = with_secrets("secret content sealed plainly", &mut rng);
	let personal = library.personal_space();
	// Content sealed as any blob is, which every replica of the space reads.
	let plain = library
		.import_blob(
			personal,
			&b"%PDF-1.7 sealed for everyone"[..],
			now() + 1_000,
			&mut rng,
		)
		.expect("imports");
	let created = library.create_entity(
		personal,
		&[SECRET_FILE],
		vec![(SECRET_CONTENT, Value::Blob(plain))],
		now() + 2_000,
		&mut rng,
	);
	assert!(
		matches!(created, Err(NodeError::NotSealedForSecrets(ref field)) if *field == SECRET_CONTENT),
		"{created:?}"
	);
	let added = add_scan(&mut library, &[], now() + 3_000, &mut rng);
	let edited = library.edit_entity(
		personal,
		added.file,
		vec![Edit::Set(SECRET_CONTENT, vec![Value::Blob(plain)])],
		&today(),
		now() + 4_000,
		&mut rng,
	);
	assert!(
		matches!(edited, Err(NodeError::NotSealedForSecrets(_))),
		"{edited:?}"
	);
	assert_eq!(read(&library, added.file).expect("reads"), scan());
}

#[test]
fn a_secret_file_has_no_checkouts_and_its_saved_copies_are_not_recorded() {
	let mut rng = TestRng::new("secret file copies");
	let (directory, mut library, login) = with_secrets("secret file copies", &mut rng);
	let added = add_scan(&mut library, &[login], now() + 1_000, &mut rng);
	let personal = library.personal_space();
	let folder = directory.join("saved");
	std::fs::create_dir(&folder).expect("makes a folder");

	let checkout = library.checkout(personal, added.file, &folder, now() + 2_000, &mut rng);
	assert!(
		matches!(checkout, Err(NodeError::SecretFile(_))),
		"{checkout:?}"
	);
	nibkeep_testkit::assert_empty!(library.checkouts(personal).expect("lists"));

	let saved = library
		.save_secret_file(personal, added.file, &folder)
		.expect("saves");
	// Both resolved, as Windows writes a canonical path with `\\?\`, which the saved one has not.
	assert_eq!(
		std::fs::canonicalize(&saved).expect("resolves"),
		std::fs::canonicalize(&folder)
			.expect("resolves")
			.join("passport.pdf")
	);
	assert_eq!(std::fs::read(&saved).expect("reads"), scan());
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = std::fs::metadata(&saved)
			.expect("reads")
			.permissions()
			.mode();
		assert_eq!(mode & 0o777, 0o600, "only its owner reads it");
	}
	let leftovers: Vec<_> = std::fs::read_dir(&folder)
		.expect("lists")
		.map(|entry| entry.expect("an entry").file_name())
		.collect();
	assert_eq!(leftovers, ["passport.pdf"]);
	nibkeep_testkit::assert_empty!(library.checkouts(personal).expect("lists"));
	nibkeep_testkit::assert_empty!(
		library
			.entities(personal, Some(&nibkeep_std::CHECKOUT))
			.expect("lists")
	);

	// A copy replaces nothing.
	let again = library.save_secret_file(personal, added.file, &folder);
	assert!(matches!(again, Err(NodeError::PlaceTaken(_))), "{again:?}");
	let elsewhere = folder.join("copy.pdf");
	library
		.save_secret_file(personal, added.file, &elsewhere)
		.expect("saves as a new file");
	assert_eq!(std::fs::read(&elsewhere).expect("reads"), scan());

	// A deleted secret file is restored first.
	library
		.edit_entity(
			personal,
			added.file,
			vec![Edit::Delete],
			&today(),
			now() + 3_000,
			&mut rng,
		)
		.expect("deletes");
	assert!(matches!(
		library.save_secret_file(personal, added.file, &folder.join("deleted.pdf")),
		Err(NodeError::Deleted(_))
	));
	library
		.edit_entity(
			personal,
			added.file,
			vec![Edit::Restore],
			&today(),
			now() + 4_000,
			&mut rng,
		)
		.expect("restores");

	// A login is no secret file, and nothing is saved while the secrets are locked.
	assert!(matches!(
		library.save_secret_file(personal, login, &folder.join("login")),
		Err(NodeError::NotSecretFile(_))
	));
	library.lock_secrets();
	assert!(matches!(
		library.save_secret_file(personal, added.file, &folder.join("locked.pdf")),
		Err(NodeError::SecretsLocked)
	));
	assert!(!folder.join("locked.pdf").exists());
}

#[test]
fn a_device_without_the_secrets_holds_a_secret_file_sealed_until_it_is_given_them() {
	let mut rng = TestRng::new("secret file elsewhere");
	let (_, mut laptop, login) = with_secrets("secret file elsewhere laptop", &mut rng);
	let added = add_scan(&mut laptop, &[login], now() + 1_000, &mut rng);
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secret file elsewhere desk");
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	let desk_replica = paired.expect("paired").replica;
	let laptop_replica = laptop.replicas().expect("lists")[0].id;
	let mut desk = joined.expect("joined").library;

	// The desk sees the file, its name and what it is attached to, and holds it sealed.
	assert_eq!(
		values(&desk, added.file, &ATTACHED_TO),
		[Value::Reference(login)]
	);
	nibkeep_testkit::assert_empty!(values(&desk, added.file, &SECRET_CONTENT));
	assert!(matches!(
		read(&desk, added.file),
		Err(NodeError::SecretsLocked)
	));
	assert_eq!(sealed_blobs(&desk_directory), 1);
	assert_eq!(
		readable_and_waiting(&mut desk),
		(0, 1),
		"its key waits for the secrets"
	);
	assert!(!nibkeep_testkit::holds_anywhere(&desk_directory, SENTINEL));
	assert!(!holds_hash(&desk_directory, &added.content));

	// Once the laptop gives it the secrets, it reads the file.
	desk.ask_for_secrets(
		laptop_replica,
		&NewProtection::Passphrase(
			"desk secrets",
			PassphraseParams::new(64, 1, 1).expect("valid"),
		),
		now() + 3_000,
		&mut rng,
	)
	.expect("asks");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	laptop
		.give_secrets(desk_replica, now() + 5_000, &mut rng)
		.expect("gives");
	synced(&mut desk, &mut laptop, now() + 6_000, (3, 4));
	assert_eq!(read(&desk, added.file).expect("reads"), scan());
	assert_eq!(readable_and_waiting(&mut desk), (1, 0));
	desk.lock_secrets();
	assert_eq!(readable_and_waiting(&mut desk), (0, 1));
}

#[test]
fn an_ai_agent_sees_a_secret_file_but_never_its_content() {
	let mut rng = TestRng::new("secret file for an agent");
	let (_, mut laptop, login) = with_secrets("secret file for an agent laptop", &mut rng);
	let added = add_scan(&mut laptop, &[login], now() + 1_000, &mut rng);
	let agent_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secret file for an agent");
	std::fs::remove_dir(&agent_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_ai_agent_named(
		&mut laptop,
		&agent_directory,
		("laptop", "Claude"),
		&code,
		&mut rng,
	);
	paired.expect("paired");
	let mut agent = joined.expect("joined").library;
	let personal = agent.personal_space();
	assert_eq!(
		agent
			.entity(personal, added.file, this_day())
			.expect("reads")
			.expect("the file")
			.name
			.as_deref(),
		Some("passport.pdf")
	);
	assert!(matches!(
		read(&agent, added.file),
		Err(NodeError::SecretsLocked)
	));
	assert_eq!(
		readable_and_waiting(&mut agent),
		(0, 1),
		"its key waits for the secrets"
	);
	let added_by_agent = agent.add_secret_file(
		personal,
		"mine.pdf",
		scan().as_slice(),
		&[],
		now() + 2_000,
		&mut rng,
	);
	assert!(
		matches!(added_by_agent, Err(NodeError::SecretsLocked)),
		"{added_by_agent:?}"
	);
	assert!(!nibkeep_testkit::holds_anywhere(&agent_directory, SENTINEL));
	assert!(!holds_hash(&agent_directory, &added.content));
}

#[test]
fn a_purged_secret_file_is_gone_everywhere() {
	let mut rng = TestRng::new("secret file purged");
	let (laptop_directory, mut laptop, login) = with_secrets("secret file purged laptop", &mut rng);
	let added = add_scan(&mut laptop, &[login], now() + 1_000, &mut rng);
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secret file purged desk");
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	assert_eq!(sealed_blobs(&desk_directory), 1, "the desk holds it sealed");

	let personal = laptop.personal_space();
	laptop.lock_secrets();
	let locked = laptop.purge(
		personal,
		added.file,
		&SECRET_CONTENT,
		None,
		now() + 2_000,
		&mut rng,
	);
	assert!(
		matches!(locked, Err(NodeError::SecretsLocked)),
		"{locked:?}"
	);
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	let purge = laptop
		.purge(
			personal,
			added.file,
			&SECRET_CONTENT,
			None,
			now() + 3_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(purge.contents, [added.content]);
	assert!(matches!(
		read(&laptop, added.file),
		Err(NodeError::NoContent(_))
	));
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	assert_eq!(sealed_blobs(&laptop_directory), 0);
	assert_eq!(
		sealed_blobs(&desk_directory),
		0,
		"the sealed copy is deleted"
	);
}

#[test]
fn an_infected_device_cannot_replace_a_secret_files_content() {
	let mut rng = TestRng::new("secret file forged");
	let (_, mut laptop, login) = with_secrets("secret file forged laptop", &mut rng);
	let added = add_scan(&mut laptop, &[login], now() + 1_000, &mut rng);
	let desk_directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "secret file forged desk");
	std::fs::remove_dir(&desk_directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(&mut rng);
	let (paired, joined) = common::pair_with(&mut laptop, &desk_directory, &code, &code, &mut rng);
	paired.expect("paired");
	let mut desk = joined.expect("joined").library;
	// The infected desk seals content of its own making as any blob, and writes its hash
	// plainly into the secret field, with nothing that the engine checks checked.
	let personal = desk.personal_space();
	let forged = desk
		.import_blob(
			personal,
			&b"%PDF-1.7 a forged scan"[..],
			now() + 2_000,
			&mut rng,
		)
		.expect("imports");
	drop(desk);
	let NewProtection::Passphrase(passphrase, _) = common::protection() else {
		unreachable!("a passphrase");
	};
	let unlocker = Unlocker::Passphrase(passphrase);
	common::forge(
		&desk_directory,
		&unlocker,
		(
			personal,
			vec![nibkeep_model::Operation::assert(
				nibkeep_model::Statement::new(
					nibkeep_base::StatementId::random(&mut rng),
					added.file,
					SECRET_CONTENT,
					Value::Blob(forged),
				),
			)],
		),
		(None, now() + 3_000),
		&mut rng,
	);
	let mut desk = Library::open(&desk_directory, &unlocker).expect("opens");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	assert_eq!(
		values(&laptop, added.file, &SECRET_CONTENT),
		[Value::Blob(added.content)],
		"the plain value does not count"
	);
	assert_eq!(read(&laptop, added.file).expect("reads"), scan());
}

#[test]
fn an_export_leaves_a_secret_file_out_but_its_ordinary_values() {
	let mut rng = TestRng::new("secret file exported");
	let (_, mut library, login) = with_secrets("secret file exported", &mut rng);
	add_scan(&mut library, &[login], now() + 1_000, &mut rng);
	let export = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"secret file exported, export",
	)
	.join("export");
	let personal = library.personal_space();
	library
		.export(personal, &export, nibkeep_node::ExportKind::Full)
		.expect("exports");
	assert!(!nibkeep_testkit::holds_anywhere(&export, SENTINEL));
	assert!(!holds_hash(&export, &Hash::of(&scan())));
	assert!(
		nibkeep_testkit::holds_anywhere(&export, b"passport.pdf"),
		"its name is ordinary"
	);
}

/// Deletes the sealed copies that a library holds, as if this device had their keys but not
/// the copies yet.
fn without_sealed_copies(directory: &Path) {
	let mut to_visit = vec![directory.to_path_buf()];
	while let Some(next) = to_visit.pop() {
		for entry in std::fs::read_dir(&next).expect("lists") {
			let path = entry.expect("an entry").path();
			if path.is_dir() {
				to_visit.push(path);
			} else if path
				.parent()
				.and_then(Path::parent)
				.and_then(Path::file_name)
				.is_some_and(|name| name == "blake3")
			{
				std::fs::remove_file(&path).expect("deletes");
			}
		}
	}
}

#[test]
fn purging_a_file_leaves_a_secret_file_of_the_same_content_and_the_other_way_round() {
	let mut rng = TestRng::new("purged by kind");
	let (directory, mut library, _) = with_secrets("purged by kind", &mut rng);
	let original = directory.join("passport.pdf");
	std::fs::write(&original, scan()).expect("writes");
	let personal = library.personal_space();
	let file = library
		.add_file(personal, &original, now() + 1_000, &mut rng)
		.expect("adds")
		.file;
	let secret = add_scan(&mut library, &[], now() + 2_000, &mut rng);
	assert_eq!(sealed_blobs(&directory), 2);

	// The ordinary file's content goes, with the secrets unlocked as when they are locked.
	let purge = library
		.purge(
			personal,
			file,
			&nibkeep_std::CONTENT,
			None,
			now() + 3_000,
			&mut rng,
		)
		.expect("purges");
	assert!(!purge.of_secrets);
	assert_eq!(purge.blobs, 1);
	assert_eq!(sealed_blobs(&directory), 1);
	assert_eq!(read(&library, secret.file).expect("reads"), scan());

	let purge = library
		.purge(
			personal,
			secret.file,
			&SECRET_CONTENT,
			None,
			now() + 4_000,
			&mut rng,
		)
		.expect("purges");
	assert!(purge.of_secrets);
	assert_eq!(sealed_blobs(&directory), 0);
}

#[test]
fn purging_a_secret_file_says_nothing_of_the_checkouts_of_a_file_of_the_same_content() {
	let mut rng = TestRng::new("purged with checkouts");
	let (directory, mut library, _) = with_secrets("purged with checkouts", &mut rng);
	let original = directory.join("passport.pdf");
	std::fs::write(&original, scan()).expect("writes");
	let personal = library.personal_space();
	let file = library
		.add_file(personal, &original, now() + 1_000, &mut rng)
		.expect("adds")
		.file;
	let folder = directory.join("checked out");
	std::fs::create_dir(&folder).expect("makes a folder");
	library
		.checkout(personal, file, &folder, now() + 2_000, &mut rng)
		.expect("checks out");
	let secret = add_scan(&mut library, &[], now() + 3_000, &mut rng);
	let purge = library
		.purgeable(personal, secret.file, &SECRET_CONTENT, None)
		.expect("tells");
	nibkeep_testkit::assert_empty!(purge.checkouts);
	nibkeep_testkit::assert_empty!(purge.originals);
	assert_eq!(purge.blobs, 1);
	library
		.purge(
			personal,
			secret.file,
			&SECRET_CONTENT,
			None,
			now() + 4_000,
			&mut rng,
		)
		.expect("purges");
	assert_eq!(
		std::fs::read(folder.join("passport.pdf")).expect("stays"),
		scan()
	);
	assert_eq!(library.checkouts(personal).expect("lists").len(), 1);
}

#[test]
fn an_ordinary_file_of_the_same_content_is_reported_though_its_copy_is_elsewhere() {
	let mut rng = TestRng::new("held elsewhere");
	let (directory, mut library, _) = with_secrets("held elsewhere", &mut rng);
	let original = directory.join("passport.pdf");
	std::fs::write(&original, scan()).expect("writes");
	let personal = library.personal_space();
	library
		.add_file(personal, &original, now() + 1_000, &mut rng)
		.expect("adds");
	// This device knows the file, but holds no copy of its content.
	drop(library);
	without_sealed_copies(&directory);
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	library.unlock_secrets(&SECRETS).expect("unlocks");
	let added = add_scan(&mut library, &[], now() + 2_000, &mut rng);
	assert!(added.unprotected_copy);
}

#[test]
fn content_sealed_for_the_secrets_is_taken_again_before_its_copy_is_here() {
	let mut rng = TestRng::new("keys before the copy");
	let (directory, mut library, _) = with_secrets("keys before the copy", &mut rng);
	let added = add_scan(&mut library, &[], now() + 1_000, &mut rng);
	drop(library);
	without_sealed_copies(&directory);
	let mut library =
		Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
	library.unlock_secrets(&SECRETS).expect("unlocks");
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			added.file,
			vec![Edit::Set(SECRET_CONTENT, vec![Value::Blob(added.content)])],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("chooses the content, sealed for the secrets");
	assert!(matches!(
		read(&library, added.file),
		Err(NodeError::NoCopyHere(_))
	));
}

/// Syncs two libraries in memory, as `nib sync` does, and checks that both sides synced.
fn synced(first: &mut Library, second: &mut Library, at: u64, sides: (usize, usize)) {
	let (initiated, responded) = common::sync_in_memory(first, second, at, sides);
	initiated.expect("synced");
	responded.expect("answered");
}

#[test]
fn a_secret_files_content_is_never_read_as_a_files() {
	let mut rng = TestRng::new("secret content as a file");
	let (_, mut library, login) = with_secrets("secret content as a file", &mut rng);
	let added = add_scan(&mut library, &[login], now() + 1_000, &mut rng);
	assert_eq!(added.content, Hash::of(&scan()));
	let personal = library.personal_space();
	assert_eq!(
		library
			.file_content(personal, &added.content, 1 << 20)
			.expect("reads"),
		None
	);
}
