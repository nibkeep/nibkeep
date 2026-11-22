//! Replays the inputs in `seeds/` through the fuzz targets, with stable Rust, so that every
//! input that once failed keeps passing.

use std::fs;
use std::path::Path;

/// A fuzz target, by its name.
type Target = (&'static str, fn(&[u8]));

#[test]
fn every_seed_passes() {
	let targets: [Target; 8] = [
		("cbor", nibkeep_fuzz::cbor),
		("log", nibkeep_fuzz::log),
		("model", nibkeep_fuzz::model),
		("keys", nibkeep_fuzz::keys),
		("crypto", nibkeep_fuzz::crypto),
		("text", nibkeep_fuzz::text),
		("encrypted", nibkeep_fuzz::encrypted),
		("sync", nibkeep_fuzz::sync),
	];
	let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds");
	for (name, target) in targets {
		let entries = fs::read_dir(seeds.join(name)).expect("every target has seeds");
		let mut replayed = 0;
		for entry in entries {
			let path = entry.expect("an entry").path();
			target(&fs::read(&path).expect("reads the seed"));
			replayed += 1;
		}
		assert!(replayed > 0, "the {name} target has seeds");
	}
}

/// Writes the seeds that the fixtures cannot give: values of the data model, which travel
/// sealed inside changes, and encrypted data: `cargo test -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_seeds() {
	use nibkeep_base::cbor;
	use nibkeep_base::{EntityId, Hash, StatementId};
	use nibkeep_model::{FieldDef, Operation, Statement, TypeDef, TypeName, Value, ValueKind};

	let field = |name: &str| name.parse().expect("a field name");
	let entity = EntityId::from_bytes([3; 16]);
	let statement = Statement::new(
		StatementId::from_bytes([4; 16]),
		entity,
		field("local.title"),
		Value::Text("Leviathan Wakes".to_owned()),
	);
	let book = TypeDef::new(TypeName::from_static("local.book"), 2)
		.with_field(field("local.title"), FieldDef::new(ValueKind::Text))
		.with_field(
			field("local.tag"),
			FieldDef::new(ValueKind::Text).many().secret(),
		);
	let seeds: Vec<(u8, &str, Vec<u8>)> = vec![
		(0, "text", cbor::to_vec(&Value::Text("amber".to_owned()))),
		(0, "integer", cbor::to_vec(&Value::Integer(-387))),
		(
			0,
			"date",
			cbor::to_vec(&Value::Date("2011-06~".parse().expect("a date"))),
		),
		(0, "reference", cbor::to_vec(&Value::Reference(entity))),
		(0, "blob", cbor::to_vec(&Value::Blob(Hash::of(b"a scan")))),
		(1, "statement", cbor::to_vec(&statement)),
		(
			2,
			"assert",
			cbor::to_vec(&Operation::replace(
				statement.clone(),
				vec![StatementId::from_bytes([5; 16])],
			)),
		),
		(
			2,
			"retract",
			cbor::to_vec(&Operation::retract(
				entity,
				vec![StatementId::from_bytes([4; 16])],
			)),
		),
		(
			2,
			"assert-earlier",
			cbor::to_vec(&Operation::Assert {
				statement: statement.clone(),
				supersedes: [StatementId::from_bytes([4; 16])].into(),
				any_entity: true,
			}),
		),
		(
			2,
			"retract-earlier",
			cbor::to_vec(&Operation::Retract {
				entity: None,
				statements: [StatementId::from_bytes([4; 16])].into(),
			}),
		),
		(
			2,
			"retract-secret",
			cbor::to_vec(&Operation::retract_secret(vec![StatementId::from_bytes(
				[6; 16],
			)])),
		),
		(4, "type", cbor::to_vec(&book)),
		(
			5,
			"field",
			cbor::to_vec(&FieldDef::new(ValueKind::Reference).many().role()),
		),
		(
			3,
			"qualifiers",
			cbor::to_vec(&nibkeep_model::Qualifiers::new()),
		),
	];
	let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/model");
	fs::create_dir_all(&directory).expect("makes the directory");
	for (choice, name, bytes) in seeds {
		let mut seed = vec![choice];
		seed.extend(bytes);
		fs::write(directory.join(name), seed).expect("writes the seed");
	}
	write_crypto_seeds();
	write_account_seeds();
}

/// Pages of accounts of copies, of each kind, sealed with the space key that the log target
/// opens them with, so that mutations reach what they tell.
fn write_account_seeds() {
	use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId};
	use nibkeep_crypto::SigningKey;
	use nibkeep_log::{AccountPage, DeviceRole, Head, PageBody};
	use nibkeep_testkit::TestRng;

	let mut rng = TestRng::new("fuzz seeds, accounts");
	let signing = SigningKey::from_bytes(&[8; 32]);
	let space = SpaceId::from_bytes([2; 16]);
	let sealed = Hash::of(b"a sealed blob");
	let file = EntityId::from_bytes([3; 16]);
	let bodies = [
		(
			"account-head",
			PageBody::Head(Head {
				disk: Hash::of(b"a disk"),
				role: DeviceRole::Archive,
				away: true,
				heads: vec![ChangeId::from_hash(Hash::of(b"a change"))],
				damaged: 2,
				unrepaired: 1,
				waits: 30,
			}),
		),
		(
			"account-blobs",
			PageBody::Blobs(sealed.digest()[0], vec![(sealed, 1_762_000_000_000)]),
		),
		(
			"account-copies",
			PageBody::Copies(3, vec![(file, Hash::of(b"a film"), 1_762_000_000_000)]),
		),
	];
	let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/log");
	for (name, body) in bodies {
		let page = AccountPage::issue(
			space,
			&nibkeep_fuzz::space_keys(),
			&signing,
			(1_762_000_000_000, &body),
			&mut rng,
		);
		let mut seed = vec![6];
		seed.extend(page.to_bytes());
		fs::write(directory.join(name), seed).expect("writes the seed");
	}
}

/// Envelopes and sealed messages sealed with other keys than the crypto target opens them
/// with, and a sealed blob of two chunks and more, sealed with the key it reads them with.
fn write_crypto_seeds() {
	use nibkeep_base::cbor;
	use nibkeep_crypto::{DecryptionKey, Envelope, Key, SealedMessage, seal_blob};
	use nibkeep_testkit::TestRng;

	let mut rng = TestRng::new("fuzz seeds");
	let other = Key::from_slice(&[9; 32]).expect("32 bytes");
	let envelope = Envelope::seal(&other, b"a key, perhaps", b"", &mut rng);
	let recipient = DecryptionKey::from_seed(&[9; 32]).encryption_key();
	let message = SealedMessage::seal(
		&recipient,
		b"a space key",
		nibkeep_fuzz::CONTEXT,
		b"",
		&mut rng,
	);
	let mut blob = Vec::new();
	let key = Key::from_slice(&[7; 32]).expect("32 bytes");
	seal_blob(&key, nibkeep_fuzz::blob_content(), &mut blob, &mut rng).expect("seals");
	let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/crypto");
	fs::create_dir_all(&directory).expect("makes the directory");
	for (choice, name, bytes) in [
		(0_u8, "envelope", cbor::to_vec(&envelope)),
		(1, "sealed-message", cbor::to_vec(&message)),
		(2, "blob", blob),
	] {
		let mut seed = vec![choice];
		seed.extend(bytes);
		fs::write(directory.join(name), seed).expect("writes the seed");
	}
}

/// Writes the seed of the text target for a space key sealed to a replica, as a statement
/// holds it: `cargo test -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_identity_seeds() {
	use nibkeep_base::{EntityId, Hash, SpaceId};
	use nibkeep_crypto::{DecryptionKey, Key};
	use nibkeep_testkit::TestRng;

	let mut rng = TestRng::new("identity seeds");
	let recipient = DecryptionKey::from_seed(&[9; 32]).encryption_key();
	let key = Key::from_slice(&[7; 32]).expect("32 bytes");
	let sealed = nibkeep_governance::SealedSpaceKey::seal(
		&recipient,
		SpaceId::from_bytes([1; 16]),
		EntityId::from_bytes([2; 16]),
		(4_294_967_296, &key, Hash::of(b"the key's hash")),
		&mut rng,
	);
	let mut seed = vec![9];
	seed.extend(sealed.to_text().into_bytes());
	let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/text");
	fs::write(directory.join("sealed-space-key"), seed).expect("writes the seed");
}

/// Writes the seeds of the log target: changes of every kind, alone, with their payloads
/// and keys, and their parts, made with the keys the target reads them with:
/// `cargo test -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_log_seeds() {
	use nibkeep_base::cbor;

	let (parcels, record_key, blob_key) = log_seed_parcels();
	let keys = nibkeep_fuzz::space_keys();
	let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/log");
	for entry in fs::read_dir(&directory).expect("reads the seeds") {
		let path = entry.expect("an entry").path();
		if path
			.file_name()
			.is_some_and(|name| name.to_string_lossy().starts_with("change-"))
		{
			fs::remove_file(path).expect("deletes an old seed");
		}
	}
	let write = |choice: u8, name: String, bytes: Vec<u8>| {
		let mut seed = vec![choice];
		seed.extend(bytes);
		fs::write(directory.join(name), seed).expect("writes the seed");
	};
	for (index, parcel) in parcels.into_iter().enumerate() {
		let number = index + 1;
		write(
			0,
			format!("change-{number:02}"),
			cbor::to_vec(&parcel.change),
		);
		write(
			4,
			format!("change-{number:02}-parcel"),
			nibkeep_fuzz::write_parcel(&parcel),
		);
		write(
			5,
			format!("change-{number:02}-key"),
			cbor::to_vec(&parcel.keys[0]),
		);
		if let (Some(payload), Some(key)) = (&parcel.payload, parcel.keys.first())
			&& let Ok(payload) = parcel.change.open(payload, key, &keys)
		{
			write(
				1,
				format!("change-{number:02}-payload"),
				cbor::to_vec(&payload),
			);
		}
	}
	write(2, "record-key-0".to_owned(), cbor::to_vec(&record_key));
	write(3, "blob-key-0".to_owned(), cbor::to_vec(&blob_key));
}

/// The wall clock time of the changes in the log target's seeds.
const LOG_SEEDS_WALL: u64 = 1_800_000_000_000;

/// The changes of the log target's seeds, with a wrapped record key and a wrapped blob key.
/// Each change but the last two is made in a log of its own, so that it has no parents
/// and a log that receives it applies it.
fn log_seed_parcels() -> (
	Vec<nibkeep_log::Parcel>,
	nibkeep_log::WrappedKey,
	nibkeep_log::WrappedBlobKey,
) {
	use nibkeep_base::{EntityId, Hash, StatementId};
	use nibkeep_crypto::Key;
	use nibkeep_log::{Change, Draft, Kind, Log, Payload, Purges};
	use nibkeep_model::{Operation, Statement, Value};
	use nibkeep_testkit::TestRng;
	use nibkeep_time::{Clock, Timestamp};

	let mut rng = TestRng::new("log seeds");
	let keys = nibkeep_fuzz::space_keys();
	let space = rng.space_id();
	let author = rng.signing_key();
	let wall = LOG_SEEDS_WALL;
	let statement = |rng: &mut TestRng, field: &str, value: &str| {
		Operation::assert(Statement::new(
			StatementId::random(rng),
			EntityId::from_bytes([3; 16]),
			field.parse().expect("a field name"),
			Value::Text(value.to_owned()),
		))
	};
	let fresh = || Log::new(space, Clock::new(3_600_000));
	let mut parcels = Vec::new();
	let title = statement(&mut rng, "local.title", "Leviathan Wakes");
	let tag = statement(&mut rng, "local.tag", "space");
	let Operation::Assert {
		statement: tagged, ..
	} = &tag
	else {
		unreachable!("an assertion");
	};
	let tagged = tagged.id;
	let mut edited = fresh();
	parcels.push(edited.commit(&author, &keys, vec![title, tag], wall, &mut rng));
	// A purge that states the title again.
	parcels.push(edited.purge(&author, &keys, [tagged], wall + 1_000, &mut rng));
	let signing = statement(&mut rng, "org.nibkeep.signing-key", "a key");
	parcels.push(fresh().commit(&author, &keys, vec![signing], wall, &mut rng));
	let mut secrets = fresh();
	secrets.set_secret_fields(["local.password".parse().expect("a field name")]);
	let password = statement(&mut rng, "local.password", "hunter2");
	let sealed = secrets
		.commit(&author, &keys, vec![password], wall, &mut rng)
		.expect("commits");
	let record_key = secrets
		.wrapped_keys()
		.map(|(_, key)| key.clone())
		.next()
		.expect("a record key");
	parcels.push(Ok(sealed.clone()));
	let purged = *sealed
		.change
		.sealed()
		.keys()
		.next()
		.expect("a sealed statement");
	parcels.push(secrets.purge(&author, &keys, [purged], wall + 1_000, &mut rng));
	let mut blobs = fresh();
	let content = Hash::of(b"a photo");
	blobs
		.add_blob_key(
			content,
			Hash::of(b"sealed"),
			Key::generate(&mut rng),
			&keys,
			&mut rng,
		)
		.expect("adds the key");
	let blob_key = blobs
		.wrapped_blob_keys()
		.map(|(_, key)| key.clone())
		.next()
		.expect("a blob key");
	parcels.push(blobs.purge_blob(&author, &keys, &content, wall, &mut rng));
	// A change that purges another, and one that governs and purges, which is refused.
	let mut draft = Draft::new(
		[sealed.id()],
		Timestamp::new(wall + 2_000, 0),
		Payload::default(),
	);
	draft.purges = Purges {
		changes: [sealed.id()].into(),
		..Purges::default()
	};
	parcels.push(Ok(Change::create(space, &keys, &author, &draft, &mut rng)));
	draft.kind = Kind::Governing;
	parcels.push(Ok(Change::create(space, &keys, &author, &draft, &mut rng)));
	let parcels = parcels
		.into_iter()
		.map(|parcel| parcel.expect("commits"))
		.collect();
	(parcels, record_key, blob_key)
}

/// Writes the seeds of encrypted databases: one as the store leaves it on disk, with pages
/// in its write-ahead log, and the pages of a database that stock SQLite wrote, for the
/// target to seal: `cargo test -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_encrypted_seeds() {
	use nibkeep_crypto::Key;
	use nibkeep_store::EncryptedDatabase;
	use nibkeep_testkit::TestRng;

	let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/encrypted");
	fs::create_dir_all(&seeds).expect("makes the directory");
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "encrypted seeds");

	// As the store leaves it: the file, and its write-ahead log apart where the seed says.
	let path = directory.join("state.sqlite");
	let database = EncryptedDatabase::open(
		&path,
		&Key::from_slice(&[9; 32]).expect("32 bytes"),
		TestRng::new("encrypted seed"),
	)
	.expect("creates");
	database
		.connection()
		.execute_batch(
			"CREATE TABLE rows (n INTEGER PRIMARY KEY, value TEXT NOT NULL);
			 INSERT INTO rows (value) VALUES ('one'), ('two');
			 PRAGMA wal_checkpoint(TRUNCATE);
			 CREATE INDEX values_of_rows ON rows (value);
			 INSERT INTO rows (value) VALUES ('three');",
		)
		.expect("writes");
	let file = fs::read(&path).expect("reads the file");
	let wal = fs::read(path.with_file_name("state.sqlite-wal")).expect("reads the log");
	drop(database);
	let split = u16::try_from(file.len()).expect("small").to_le_bytes();
	fs::write(
		seeds.join("on-disk"),
		[&[0][..], &split, &file, &wal].concat(),
	)
	.expect("writes");

	// The pages of a database that stock SQLite wrote, with pages of 4,096 bytes.
	let plain = directory.join("plain.sqlite");
	let connection = rusqlite::Connection::open(&plain).expect("creates");
	connection
		.execute_batch(
			"PRAGMA page_size = 4096;
			 PRAGMA journal_mode = WAL;
			 CREATE TABLE rows (n INTEGER PRIMARY KEY, value TEXT NOT NULL);
			 CREATE INDEX values_of_rows ON rows (value);
			 INSERT INTO rows (value) VALUES ('one'), ('two'), ('three');
			 PRAGMA wal_checkpoint(TRUNCATE);",
		)
		.expect("writes");
	drop(connection);
	let pages = fs::read(&plain).expect("reads");
	fs::write(seeds.join("pages"), [&[1][..], &pages].concat()).expect("writes");
}

/// The target seals pages as the store's VFS does: a database that stock SQLite wrote,
/// sealed by `encrypted_format`, opens through the VFS with all its rows.
#[test]
fn sealed_pages_open_through_the_store() {
	use nibkeep_fuzz::encrypted_format;
	use nibkeep_store::EncryptedDatabase;
	use nibkeep_testkit::TestRng;

	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "sealed pages");
	let plain = directory.join("plain.sqlite");
	let connection = rusqlite::Connection::open(&plain).expect("creates");
	connection
		.execute_batch(
			"PRAGMA page_size = 4096;
			 PRAGMA journal_mode = WAL;
			 CREATE TABLE rows (n INTEGER PRIMARY KEY, value TEXT NOT NULL);
			 INSERT INTO rows (value) VALUES ('one'), ('two'), ('three');
			 PRAGMA wal_checkpoint(TRUNCATE);",
		)
		.expect("writes");
	drop(connection);
	let (key, own) = (TestRng::new("key").key(), TestRng::new("own").key());
	let mut rng = TestRng::new("nonces");
	let mut file = encrypted_format::header(&key, &own, &mut rng);
	for (index, page) in fs::read(&plain)
		.expect("reads")
		.chunks(encrypted_format::PAGE)
		.enumerate()
	{
		let index = u64::try_from(index).expect("small");
		file.extend(encrypted_format::page(&own, &mut rng, index, page));
	}
	let sealed = directory.join("sealed.sqlite");
	fs::write(&sealed, file).expect("writes");
	let database = EncryptedDatabase::open(&sealed, &key, TestRng::new("database")).expect("opens");
	let rows: i64 = database
		.connection()
		.query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
		.expect("counts");
	assert_eq!(rows, 3);
}

/// Writes the seeds of messages from other devices: `cargo test -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_sync_seeds() {
	let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/sync");
	fs::create_dir_all(&directory).expect("makes the folder");
	let (pairing, syncing) = nibkeep_sync::fuzzing::samples();
	for (prefix, messages) in [(0_u8, pairing), (1, syncing)] {
		let side = if prefix == 0 { "pair" } else { "sync" };
		for (name, message) in messages {
			let mut seed = vec![prefix];
			seed.extend(&message);
			fs::write(directory.join(format!("{side}-{name}")), seed).expect("writes");
		}
	}
	for (name, message) in nibkeep_sync::fuzzing::invitation_samples() {
		let mut seed = vec![3];
		seed.extend(&message);
		fs::write(directory.join(format!("invite-{name}")), seed).expect("writes");
	}
	let mut seed = vec![2];
	seed.extend(nibkeep_sync::fuzzing::negentropy_sample());
	fs::write(directory.join("negentropy"), seed).expect("writes");
}

/// Writes the seed of a pack, as a backup holds it: `cargo test write_pack_seed --
/// --ignored`.
#[test]
#[ignore = "writes a seed once"]
fn write_pack_seed() {
	use nibkeep_base::{Hash, SpaceId};
	use nibkeep_sync::{PackItem, pack_header, pack_item};

	let space = SpaceId::from_bytes([5; 16]);
	let blob = b"sealed bytes of a thumbnail".to_vec();
	let mut pack = vec![4];
	pack.extend(pack_header(space, 2));
	pack.extend(pack_item(space, &PackItem::Blob(Hash::of(&blob), blob)));
	let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("seeds")
		.join("sync");
	fs::write(directory.join("pack-blob"), pack).expect("writes");
}

/// Writes the seed of a page about a backup, as the replica that updated it issues it:
/// `cargo test write_backup_page_seed -- --ignored`.
#[test]
#[ignore = "writes a seed once"]
fn write_backup_page_seed() {
	use nibkeep_base::{ChangeId, EntityId, Hash, SpaceId};
	use nibkeep_crypto::SigningKey;
	use nibkeep_log::{AccountPage, BackupHead, PageBody};
	use nibkeep_testkit::TestRng;

	let mut rng = TestRng::new("fuzz seeds, backup accounts");
	let body = PageBody::BackupHead(BackupHead {
		disk: Hash::of(b"a stick"),
		away: true,
		heads: vec![ChangeId::from_hash(Hash::of(b"a change"))],
		damaged: 1,
		unrepaired: 0,
		update: 7,
		waits: 30,
	});
	let page = AccountPage::issue_about(
		SpaceId::from_bytes([2; 16]),
		&nibkeep_fuzz::space_keys(),
		&SigningKey::from_bytes(&[8; 32]),
		EntityId::from_bytes([4; 16]),
		(1_762_000_000_000, &body),
		&mut rng,
	);
	let mut seed = vec![6];
	seed.extend(page.to_bytes());
	let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/log");
	fs::write(directory.join("account-backup-head"), seed).expect("writes the seed");
}

/// Writes the seeds of the log target for a change that restores what a purge destroyed,
/// alone and with its parts: `cargo test write_restore_seeds -- --ignored`.
#[test]
#[ignore = "writes seeds once"]
fn write_restore_seeds() {
	use nibkeep_base::{EntityId, StatementId, cbor};
	use nibkeep_log::{Log, Restores};
	use nibkeep_model::{Operation, Statement, Value};
	use nibkeep_testkit::TestRng;
	use nibkeep_time::Clock;

	let mut rng = TestRng::new("restore seeds");
	let keys = nibkeep_fuzz::space_keys();
	let space = rng.space_id();
	let author = rng.signing_key();
	let mut log = Log::new(space, Clock::new(3_600_000));
	log.keep_destroyed(true);
	let title = Operation::assert(Statement::new(
		StatementId::random(&mut rng),
		EntityId::from_bytes([3; 16]),
		"local.title".parse().expect("a field name"),
		Value::Text("Leviathan Wakes".to_owned()),
	));
	let Operation::Assert { statement, .. } = &title else {
		unreachable!("an assertion");
	};
	let id = statement.id;
	let first = log
		.commit(&author, &keys, vec![title], LOG_SEEDS_WALL, &mut rng)
		.expect("commits");
	log.purge(&author, &keys, [id], LOG_SEEDS_WALL + 1_000, &mut rng)
		.expect("purges");
	let restore = log
		.restore_purged(
			&author,
			&keys,
			Restores {
				changes: std::collections::BTreeSet::from([first.id()]),
				..Restores::default()
			},
			LOG_SEEDS_WALL + 2_000,
			&mut rng,
		)
		.expect("restores");
	let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/log");
	let mut alone = vec![0];
	alone.extend(cbor::to_vec(&restore.change));
	fs::write(directory.join("restore"), alone).expect("writes the seed");
	let mut parcel = vec![4];
	parcel.extend(nibkeep_fuzz::write_parcel(&restore));
	fs::write(directory.join("restore-parcel"), parcel).expect("writes the seed");
}
