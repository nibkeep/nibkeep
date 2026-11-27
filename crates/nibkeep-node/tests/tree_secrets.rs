//! Secrets in trees: a project's `.env` and keys kept in its tree as secret files, sealed on
//! every replica, and read or written only where the space's secrets are
//! ([technical design: secrets in trees]).
//!
//! [technical design: secrets in trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#151-secrets-in-trees

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::common::{library, now, this_day, today};
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	Change, Commit, Committed, Edit, InTree, Joining, Library, MemberRole, NewProtection,
	NodeError, TreePoint, TreeStarted, Unlocker, resolved_path,
};
use nibkeep_std::{CONTENT, FILE, IN_TREE, SECRET_CONTENT, SECRET_FILE, TREE_PATH};
use nibkeep_testkit::TestRng;

/// What a secret file holds, which must never be stored or written in plain where it
/// should not be.
const SENTINEL: &str = "TOKEN=s3ntinel-of-the-tree\n";

const SECRETS: Unlocker<'static> = Unlocker::Passphrase("tr0ub4dor");

fn secrets_passphrase() -> NewProtection<'static> {
	NewProtection::Passphrase("tr0ub4dor", PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// A folder of its own, as the file system resolves it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	resolved_path(&made).expect("resolves")
}

/// Writes a file, with its folders, and makes it look written an hour ago.
fn write(root: &Path, path: &str, content: &str) {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	let hour_ago = SystemTime::now() - Duration::from_secs(3_600);
	fs::File::options()
		.write(true)
		.open(&path)
		.and_then(|file| file.set_modified(hour_ago))
		.expect("ages");
}

fn read(root: &Path, path: &str) -> String {
	fs::read_to_string(root.join(path)).expect("reads")
}

/// The values of an entity's field, as the library shows them today.
fn values(library: &Library, entity: EntityId, field: &FieldName) -> Vec<Value> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity")
		.values(field)
		.iter()
		.map(|shown| shown.value.clone())
		.collect()
}

fn types(library: &Library, entity: EntityId) -> Vec<nibkeep_model::TypeName> {
	library
		.entity(library.personal_space(), entity, this_day())
		.expect("reads")
		.expect("the entity")
		.types
}

/// The entity at a path of a tree, as the library has it now.
fn at_path(library: &Library, tree: EntityId, path: &str) -> EntityId {
	library
		.tree_entries(library.personal_space(), tree)
		.expect("reads")
		.into_iter()
		.find(|entry| entry.path == path)
		.unwrap_or_else(|| panic!("nothing at {path}"))
		.entity
}

/// A library whose secrets are set up and unlocked, with a tree started in a folder of its
/// own that holds `files`.
fn with_tree(
	name: &str,
	files: &[(&str, &str)],
	rng: &mut TestRng,
) -> (PathBuf, Library, EntityId, PathBuf) {
	let (directory, mut library) = library(name, rng);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, rng)
		.expect("sets up the secrets");
	let root = folder(&format!("{name}, tree"));
	for (path, content) in files {
		write(&root, path, content);
	}
	let personal = library.personal_space();
	let started = library
		.start_tree(personal, &root, Some("Start"), now() + 1_000, rng)
		.expect("starts");
	(directory, library, started.tree, root)
}

/// A tree of `space`, started in a folder of its own, named `name`, that holds `files`.
fn tree_in(
	library: &mut Library,
	(space, name): (SpaceId, &str),
	files: &[(&str, &str)],
	at: u64,
	rng: &mut TestRng,
) -> (TreeStarted, PathBuf) {
	let root = folder(name);
	for (path, content) in files {
		write(&root, path, content);
	}
	let started = library
		.start_tree(space, &root, Some("Start"), at, rng)
		.expect("starts");
	(started, root)
}

/// The types of the entity at a path of a tree of `space`, as the library has it now.
fn types_at(
	library: &Library,
	(space, tree): (SpaceId, EntityId),
	path: &str,
) -> Vec<nibkeep_model::TypeName> {
	let entity = library
		.tree_entries(space, tree)
		.expect("reads")
		.into_iter()
		.find(|entry| entry.path == path)
		.unwrap_or_else(|| panic!("nothing at {path}"))
		.entity;
	library
		.entity(space, entity, this_day())
		.expect("reads")
		.expect("the entity")
		.types
}

fn commit(
	library: &mut Library,
	tree: EntityId,
	commit: &Commit<'_>,
	at: u64,
	rng: &mut TestRng,
) -> Committed {
	let personal = library.personal_space();
	library
		.commit_tree(personal, tree, commit, at, rng)
		.expect("commits")
}

/// A new replica of `laptop`'s library on a device named desk, given the secrets if
/// `secrets` says so.
fn desk_of(
	laptop: &mut Library,
	name: &str,
	secrets: bool,
	rng: &mut TestRng,
) -> (PathBuf, Library) {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	fs::remove_dir(&directory).expect("removes");
	let code = nibkeep_node::PairingCode::generate(rng);
	let mut joining = Joining::new("desk", "nib", rng).expect("names");
	if secrets {
		joining = joining.asking_for_secrets(
			NewProtection::Passphrase(
				"desk secrets",
				PassphraseParams::new(64, 1, 1).expect("valid"),
			),
			rng,
		);
	}
	let (paired, joined) =
		common::pair_joining(laptop, &directory, (joining, secrets), (&code, &code), rng);
	paired.expect("paired");
	(directory, joined.expect("joined").library)
}

fn synced(first: &mut Library, second: &mut Library, at: u64, sides: (usize, usize)) {
	let (initiated, responded) = common::sync_in_memory(first, second, at, sides);
	initiated.expect("synced");
	responded.expect("answered");
}

/// Whether the library's files hold the content's hash anywhere in plain.
fn holds_hash(directory: &Path, content: &Hash) -> bool {
	nibkeep_testkit::holds_anywhere(directory, &content.to_multihash())
		|| nibkeep_testkit::holds_anywhere(directory, content.to_string().as_bytes())
}

#[test]
fn a_file_with_a_built_in_secret_name_goes_in_as_a_secret_file() {
	let mut rng = TestRng::new("tree secret named");
	let (directory, library, tree, _root) = with_tree(
		"tree secret named",
		&[
			(".env", SENTINEL),
			(".env.example", "TOKEN=\n"),
			("deploy/id_ed25519", "-----BEGIN OPENSSH PRIVATE KEY-----\n"),
			("notes.md", "# Notes\n"),
		],
		&mut rng,
	);
	let env = at_path(&library, tree, ".env");
	assert_eq!(types(&library, env), [SECRET_FILE]);
	nibkeep_testkit::assert_empty!(values(&library, env, &CONTENT));
	assert_eq!(values(&library, env, &IN_TREE), [Value::Reference(tree)]);
	assert_eq!(
		values(&library, env, &TREE_PATH),
		[Value::Text(".env".to_owned())]
	);
	let content = Hash::of(SENTINEL.as_bytes());
	assert_eq!(
		values(&library, env, &SECRET_CONTENT),
		[Value::Blob(content)]
	);
	let key = at_path(&library, tree, "deploy/id_ed25519");
	assert_eq!(types(&library, key), [SECRET_FILE]);
	// An example of a `.env` holds no secrets.
	let example = at_path(&library, tree, ".env.example");
	assert_eq!(types(&library, example), [FILE]);
	assert!(!nibkeep_testkit::holds_anywhere(
		&directory,
		SENTINEL.as_bytes()
	));
	assert!(!holds_hash(&directory, &content));
}

#[test]
fn nibsecret_names_what_else_is_secret_and_a_commit_can_choose_otherwise() {
	let mut rng = TestRng::new("tree nibsecret");
	let (_, mut library, tree, root) = with_tree(
		"tree nibsecret",
		&[(".nibsecret", "*.tfvars\n"), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	write(&root, "infra/prod.tfvars", "password = \"hunter2\"\n");
	write(&root, "certs/public.pem", "-----BEGIN CERTIFICATE-----\n");
	write(&root, "keys/signing", "a key under a name no rule knows\n");
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None)
			.with_ordinary(vec!["certs/public.pem".to_owned()])
			.with_secret(vec!["keys/signing".to_owned()]),
		now() + 2_000,
		&mut rng,
	);
	assert_eq!(committed.secret, ["infra/prod.tfvars", "keys/signing"]);
	let of = |path| types(&library, at_path(&library, tree, path));
	assert_eq!(of("infra/prod.tfvars"), [SECRET_FILE]);
	assert_eq!(of("keys/signing"), [SECRET_FILE]);
	assert_eq!(of("certs/public.pem"), [FILE]);
	assert_eq!(of(".nibsecret"), [FILE]);
}

#[test]
fn a_commit_needs_the_secrets_for_a_secret_file_and_can_leave_it() {
	let mut rng = TestRng::new("tree secrets locked");
	let (_, mut library, tree, root) = with_tree(
		"tree secrets locked",
		&[("notes.md", "# Notes\n")],
		&mut rng,
	);
	library.lock_secrets();
	write(&root, ".env", SENTINEL);
	write(&root, "notes.md", "# Notes, more\n");
	let personal = library.personal_space();
	let refused = library.commit_tree(personal, tree, &Commit::new(None), now() + 2_000, &mut rng);
	assert!(
		matches!(&refused, Err(NodeError::SecretsNeeded(paths)) if paths == &[".env".to_owned()]),
		"{refused:?}"
	);
	let status = library.tree_status(personal, tree).expect("status");
	assert_eq!(status.secret, [".env"]);
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None).leaving_secrets(),
		now() + 3_000,
		&mut rng,
	);
	assert_eq!(committed.left, [".env"]);
	assert_eq!(committed.changes.len(), 1, "{committed:?}");
	library.unlock_secrets(&SECRETS).expect("unlocks");
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 4_000,
		&mut rng,
	);
	assert_eq!(committed.secret, [".env"]);
}

#[test]
fn a_tree_of_another_space_sets_up_the_space_s_secrets_with_its_first_secret_file() {
	let mut rng = TestRng::new("tree secrets in a space");
	let (directory, mut library) = library("tree secrets in a space", &mut rng);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 500, &mut rng)
		.expect("sets up the secrets");
	let rig = library
		.create_space("Rig", now() + 600, &mut rng)
		.expect("makes it");
	let (started, root) = tree_in(
		&mut library,
		(rig, "tree secrets in a space, rig"),
		&[
			(".nibsecret", "power-switch/wifi.env\n"),
			("notes.md", "# Notes\n"),
		],
		now() + 1_000,
		&mut rng,
	);
	assert!(!library.secrets_unlocked(rig), "set up for a secret file");
	write(&root, "power-switch/wifi.env", SENTINEL);
	let committed = library
		.commit_tree(
			rig,
			started.tree,
			&Commit::new(None),
			now() + 2_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.secret, ["power-switch/wifi.env"]);
	assert!(library.secrets_unlocked(rig));
	assert_eq!(
		types_at(&library, (rig, started.tree), "power-switch/wifi.env"),
		[SECRET_FILE]
	);
	// A tree started with one sets them up with its first commit.
	let work = library
		.create_space("Work", now() + 3_000, &mut rng)
		.expect("makes it");
	let (started, _) = tree_in(
		&mut library,
		(work, "tree secrets in a space, work"),
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		now() + 4_000,
		&mut rng,
	);
	assert_eq!(started.committed.secret, [".env"]);
	assert!(library.secrets_unlocked(work));
	assert!(!nibkeep_testkit::holds_anywhere(
		&directory,
		SENTINEL.as_bytes()
	));
	assert!(!holds_hash(&directory, &Hash::of(SENTINEL.as_bytes())));
}

#[test]
fn a_tree_of_another_space_takes_its_first_secret_file_once_the_secrets_are_set_up_or_unlocked() {
	let mut rng = TestRng::new("tree secrets in a space, asked");
	let (_, mut library) = library("tree secrets in a space, asked", &mut rng);
	let rig = library
		.create_space("Rig", now() + 600, &mut rng)
		.expect("makes it");
	let work = library
		.create_space("Work", now() + 700, &mut rng)
		.expect("makes it");
	let notes = [("notes.md", "# Notes\n")];
	let (in_rig, rig_root) = tree_in(
		&mut library,
		(rig, "tree secrets in a space, asked, rig"),
		&notes,
		now() + 1_000,
		&mut rng,
	);
	let (in_work, work_root) = tree_in(
		&mut library,
		(work, "tree secrets in a space, asked, work"),
		&notes,
		now() + 1_100,
		&mut rng,
	);
	write(&rig_root, ".env", SENTINEL);
	write(&work_root, ".env", SENTINEL);
	let needed = |refused: &Result<Committed, NodeError>| matches!(refused, Err(NodeError::SecretsNeeded(paths)) if paths == &[".env".to_owned()]);
	// Without secrets, the commit asks for them, as in the personal space, and sets up the
	// space's once they are.
	let refused = library.commit_tree(
		rig,
		in_rig.tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	);
	assert!(needed(&refused), "{refused:?}");
	assert_eq!(
		refused.expect_err("refused").to_string(),
		".env is a secret file, which a commit records only with the secrets unlocked; unlock \
		 them, or leave this change for a later commit"
	);
	library
		.set_up_secrets(&secrets_passphrase(), now() + 2_500, &mut rng)
		.expect("sets up the secrets");
	let committed = library
		.commit_tree(
			rig,
			in_rig.tree,
			&Commit::new(None),
			now() + 3_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.secret, [".env"]);
	// Locked, they are asked for in a space that has none yet, which gets them once they are
	// unlocked.
	library.lock_secrets();
	let refused = library.commit_tree(
		work,
		in_work.tree,
		&Commit::new(None),
		now() + 4_000,
		&mut rng,
	);
	assert!(needed(&refused), "{refused:?}");
	library.unlock_secrets(&SECRETS).expect("unlocks");
	let committed = library
		.commit_tree(
			work,
			in_work.tree,
			&Commit::new(None),
			now() + 5_000,
			&mut rng,
		)
		.expect("commits");
	assert_eq!(committed.secret, [".env"]);
	assert!(library.secrets_unlocked(work));
}

#[test]
fn only_an_admin_s_tree_sets_up_a_space_s_secrets_and_another_member_leaves_its_secret_files() {
	let mut rng = TestRng::new("tree secrets of an editor");
	let mut laptop = common::user_library("Ada", "laptop", &mut rng);
	let at = now() + 1_000;
	let space = laptop
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	let mut bea = common::user_library("Bea", "phone", &mut rng);
	bea.set_up_secrets(&secrets_passphrase(), at + 100, &mut rng)
		.expect("sets up the secrets");
	common::invite_member(
		&mut laptop,
		(space, MemberRole::Editor),
		&mut bea,
		at + 2_000,
	);
	let root = folder("tree secrets of an editor");
	write(&root, "notes.md", "# Notes\n");
	write(&root, ".env", SENTINEL);
	let refused = bea.start_tree(space, &root, Some("Start"), at + 3_000, &mut rng);
	assert!(
		matches!(refused, Err(NodeError::AdminsOnly(_))),
		"{refused:?}"
	);
	let started = bea
		.start_tree_with(
			space,
			&root,
			&Commit::new(Some("Start")).leaving_secrets(),
			at + 4_000,
			&mut rng,
		)
		.expect("starts");
	assert_eq!(started.committed.left, [".env"]);
	assert!(!bea.secrets_unlocked(space));
	assert_eq!(types_at(&bea, (space, started.tree), "notes.md"), [FILE]);
}

#[test]
fn a_file_once_secret_stays_secret_whatever_its_types_and_paths_say_later() {
	let mut rng = TestRng::new("tree secret for good");
	let (directory, mut library, tree, root) = with_tree(
		"tree secret for good",
		&[(".env", "TOKEN=1\n"), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let env = at_path(&library, tree, ".env");
	// As an AI agent's replica may, a change takes the type of secret files away.
	library
		.edit_entity(
			personal,
			env,
			vec![Edit::AddType(FILE), Edit::RemoveType(SECRET_FILE)],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("takes the type away");
	// Moved, as a commit finds a file gone from one path and found at another, it stays
	// secret, and so do its new versions there.
	fs::rename(root.join(".env"), root.join("settings.txt")).expect("renames");
	commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 3_000,
		&mut rng,
	);
	assert_eq!(at_path(&library, tree, "settings.txt"), env);
	write(&root, "settings.txt", SENTINEL);
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 3_500,
		&mut rng,
	);
	assert_eq!(committed.secret, ["settings.txt"]);
	let content = Hash::of(SENTINEL.as_bytes());
	assert_eq!(
		values(&library, env, &SECRET_CONTENT),
		[Value::Blob(content)]
	);
	nibkeep_testkit::assert_empty!(values(&library, env, &CONTENT));
	assert!(!nibkeep_testkit::holds_anywhere(
		&directory,
		SENTINEL.as_bytes()
	));
	// A new file where a secret file was is secret too.
	write(&root, ".env", "TOKEN=2\n");
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 4_000,
		&mut rng,
	);
	assert_eq!(committed.secret, [".env"]);
}

#[test]
fn a_new_ordinary_file_with_a_secret_file_s_content_is_named() {
	let mut rng = TestRng::new("tree secret copied");
	let (_, mut library, tree, root) =
		with_tree("tree secret copied", &[(".env", SENTINEL)], &mut rng);
	write(&root, "env.backup", SENTINEL);
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	);
	assert_eq!(committed.unprotected, ["env.backup"]);
}

#[test]
fn a_secret_file_taken_into_a_tree_by_another_change_is_never_written_as_the_tree_s() {
	let mut rng = TestRng::new("tree secret taken in");
	let (_, mut library, tree, root) = with_tree(
		"tree secret taken in",
		&[("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let scan = library
		.add_secret_file(
			personal,
			"passport.pdf",
			SENTINEL.as_bytes(),
			&[],
			now() + 2_000,
			&mut rng,
		)
		.expect("adds a secret file");
	// As an AI agent's replica may, a change puts a password record's file into the tree.
	library
		.edit_entity(
			personal,
			scan.file,
			vec![
				Edit::Set(IN_TREE, vec![Value::Reference(tree)]),
				Edit::Set(
					TREE_PATH,
					vec![Value::Text("public/passport.pdf".to_owned())],
				),
			],
			&today(),
			now() + 3_000,
			&mut rng,
		)
		.expect("takes it in");
	let entries = library.tree_entries(personal, tree).expect("reads");
	assert!(
		entries.iter().all(|entry| entry.entity != scan.file),
		"{entries:?}"
	);
	let updated = library
		.update_tree_agreeing(personal, tree, false)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.written);
	assert!(!root.join("public").exists());
}

#[test]
fn a_secret_file_moved_elsewhere_is_written_at_its_new_path_only_once_agreed() {
	let mut rng = TestRng::new("tree secret moved");
	let (_, mut library, tree, root) = with_tree(
		"tree secret moved",
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let env = at_path(&library, tree, ".env");
	// As another device may, a change moves it where a web server publishes what it finds.
	library
		.edit_entity(
			personal,
			env,
			vec![Edit::Set(
				TREE_PATH,
				vec![Value::Text("public/env.txt".to_owned())],
			)],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("moves it");
	let updated = library.update_tree(personal, tree, false).expect("updates");
	assert_eq!(updated.held, [".env -> public/env.txt"]);
	assert_eq!(read(&root, ".env"), SENTINEL);
	assert!(!root.join("public").exists());
	let updated = library
		.update_tree_agreeing(personal, tree, false)
		.expect("updates");
	assert_eq!(
		updated.moved,
		[(".env".to_owned(), "public/env.txt".to_owned())]
	);
	assert_eq!(read(&root, "public/env.txt"), SENTINEL);
}

#[test]
fn a_line_taken_out_of_nibsecret_elsewhere_is_written_only_once_agreed() {
	let mut rng = TestRng::new("tree nibsecret narrowed");
	let (_, mut library, tree, root) = with_tree(
		"tree nibsecret narrowed",
		&[
			(".nibsecret", "*.tfvars\n*.secret\n"),
			("narrow.txt", "*.secret\n"),
		],
		&mut rng,
	);
	let personal = library.personal_space();
	let nibsecret = at_path(&library, tree, ".nibsecret");
	let narrow = values(&library, at_path(&library, tree, "narrow.txt"), &CONTENT)[0].clone();
	// As another device may, a change takes `*.tfvars` out.
	library
		.edit_entity(
			personal,
			nibsecret,
			vec![Edit::Set(CONTENT, vec![narrow])],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("narrows it");
	let updated = library.update_tree(personal, tree, false).expect("updates");
	assert_eq!(updated.held, [".nibsecret"]);
	assert_eq!(read(&root, ".nibsecret"), "*.tfvars\n*.secret\n");
	let updated = library
		.update_tree_agreeing(personal, tree, false)
		.expect("updates");
	assert_eq!(updated.written, [".nibsecret"]);
	assert_eq!(read(&root, ".nibsecret"), "*.secret\n");
}

#[test]
fn a_diff_shows_that_a_secret_file_changed_and_its_lines_only_when_asked() {
	let mut rng = TestRng::new("tree secret diff");
	let (_, library, tree, root) =
		with_tree("tree secret diff", &[(".env", "TOKEN=1\n")], &mut rng);
	write(&root, ".env", SENTINEL);
	let personal = library.personal_space();
	let hidden = library.tree_diff(personal, tree, None).expect("diffs");
	assert_eq!(hidden.len(), 1);
	assert!(hidden[0].secret);
	assert_eq!(hidden[0].patch, None);
	let shown = library
		.tree_diff_with(personal, tree, None, true)
		.expect("diffs");
	assert!(
		shown[0]
			.patch
			.as_deref()
			.is_some_and(|patch| patch.contains("s3ntinel"))
	);
	let blamed = library.tree_blame(personal, tree, ".env");
	assert!(
		matches!(blamed, Err(NodeError::SecretLines(_))),
		"{blamed:?}"
	);
	let blamed = library
		.tree_blame_with(personal, tree, ".env", true)
		.expect("blames");
	assert_eq!(blamed[0].text, "TOKEN=1");
}

#[test]
fn a_device_without_the_secrets_has_the_rest_of_the_tree_and_never_their_content() {
	let mut rng = TestRng::new("tree secrets elsewhere");
	let (_, mut laptop, tree, _root) = with_tree(
		"tree secrets elsewhere, laptop",
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let (desk_directory, mut desk) =
		desk_of(&mut laptop, "tree secrets elsewhere, desk", false, &mut rng);
	let personal = laptop.personal_space();
	let copy = folder("tree secrets elsewhere, desk copy");
	let written = desk
		.write_tree(
			personal,
			tree,
			&copy,
			TreePoint::Now,
			now() + 2_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(written.written, ["notes.md"]);
	assert_eq!(written.sealed, [".env"]);
	assert!(!copy.join(".env").exists());
	let status = desk.tree_status(personal, tree).expect("status");
	assert_eq!(status.sealed, [".env"]);
	nibkeep_testkit::assert_empty!(status.changes);
	// It commits the rest, and never takes the missing copy for a deletion.
	write(&copy, "notes.md", "# Notes, from the desk\n");
	let committed = commit(&mut desk, tree, &Commit::new(None), now() + 3_000, &mut rng);
	assert_eq!(committed.changes.len(), 1, "{committed:?}");
	// Its own `.env`, where the tree has a secret file, waits for secrets it cannot have.
	write(&copy, ".env", "TOKEN=desk\n");
	let refused = desk.commit_tree(personal, tree, &Commit::new(None), now() + 4_000, &mut rng);
	assert!(
		matches!(refused, Err(NodeError::SecretsNeeded(_))),
		"{refused:?}"
	);
	let entries = desk.tree_entries(personal, tree).expect("reads");
	assert!(entries.iter().any(|entry| entry.path == ".env"));
	assert!(!nibkeep_testkit::holds_anywhere(
		&desk_directory,
		SENTINEL.as_bytes()
	));
	assert!(!holds_hash(&desk_directory, &Hash::of(SENTINEL.as_bytes())));
}

#[test]
fn a_secret_copy_stays_while_the_secrets_are_locked_and_goes_with_a_purge_of_its_content() {
	let mut rng = TestRng::new("tree secret purged");
	let (_, mut laptop, tree, _root) = with_tree(
		"tree secret purged, laptop",
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let (_, mut desk) = desk_of(&mut laptop, "tree secret purged, desk", true, &mut rng);
	let personal = laptop.personal_space();
	let copy = folder("tree secret purged, desk copy");
	let written = desk
		.write_tree(
			personal,
			tree,
			&copy,
			TreePoint::Now,
			now() + 2_000,
			&mut rng,
		)
		.expect("writes");
	assert_eq!(written.written, [".env", "notes.md"]);
	assert_eq!(read(&copy, ".env"), SENTINEL);
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = fs::metadata(copy.join(".env"))
			.expect("there")
			.permissions()
			.mode();
		assert_eq!(mode & 0o077, 0, "only its owner reads it");
	}
	desk.lock_secrets();
	let status = desk.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
	let updated = desk.update_tree(personal, tree, false).expect("updates");
	nibkeep_testkit::assert_empty!(updated.deleted);
	assert_eq!(read(&copy, ".env"), SENTINEL);
	// The laptop purges the leaked token; the desk, its secrets locked, deletes its copy.
	let env = at_path(&laptop, tree, ".env");
	laptop
		.purge(
			personal,
			env,
			&SECRET_CONTENT,
			None,
			now() + 3_000,
			&mut rng,
		)
		.expect("purges");
	synced(&mut desk, &mut laptop, now() + 4_000, (1, 2));
	assert!(!copy.join(".env").exists());
	assert_eq!(read(&copy, "notes.md"), "# Notes\n");
}

#[test]
fn making_a_committed_file_secret_purges_its_old_versions_but_what_other_files_hold() {
	let mut rng = TestRng::new("tree made secret");
	let (_, mut library, tree, root) = with_tree(
		"tree made secret",
		&[
			("config.ini", "token = example\n"),
			("config.example", "token = example\n"),
		],
		&mut rng,
	);
	let personal = library.personal_space();
	let old = at_path(&library, tree, "config.ini");
	write(&root, "config.ini", SENTINEL);
	commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	);
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None).with_secret(vec!["config.ini".to_owned()]),
		now() + 3_000,
		&mut rng,
	);
	assert_eq!(committed.secret, ["config.ini"]);
	assert_eq!(committed.made_secret, [old]);
	let new = at_path(&library, tree, "config.ini");
	assert_ne!(new, old);
	assert_eq!(types(&library, new), [SECRET_FILE]);
	let leaked = Hash::of(SENTINEL.as_bytes());
	let example = Hash::of(b"token = example\n");
	let plan = library
		.would_purge_made_secret(personal, old)
		.expect("plans");
	assert_eq!(plan.purged, [leaked]);
	assert_eq!(plan.kept, [example]);
	library
		.purge_made_secret(personal, old, now() + 4_000, &mut rng)
		.expect("purges");
	let example_file = at_path(&library, tree, "config.example");
	assert_eq!(
		values(&library, example_file, &CONTENT),
		[Value::Blob(example)]
	);
	assert_eq!(
		values(&library, new, &SECRET_CONTENT),
		[Value::Blob(leaked)]
	);
	assert_eq!(read(&root, "config.ini"), SENTINEL);
}

#[test]
fn tree_secrets_lists_the_secret_files_and_the_ordinary_ones_that_look_secret() {
	let mut rng = TestRng::new("tree secrets listed");
	let (_, mut library, tree, root) = with_tree(
		"tree secrets listed",
		&[
			(".env", SENTINEL),
			("notes.md", "# Notes\n"),
			("cert.pem", "public\n"),
		],
		&mut rng,
	);
	let personal = library.personal_space();
	// The certificate went in as ordinary, as the person chose, before it was listed.
	let cert = at_path(&library, tree, "cert.pem");
	assert_eq!(types(&library, cert), [SECRET_FILE]);
	write(&root, "public.pem", "public too\n");
	commit(
		&mut library,
		tree,
		&Commit::new(None).with_ordinary(vec!["public.pem".to_owned()]),
		now() + 2_000,
		&mut rng,
	);
	let listed = library.tree_secrets(personal, tree).expect("lists");
	assert_eq!(listed.secret, [".env", "cert.pem"]);
	assert_eq!(listed.looking_secret, ["public.pem"]);
	nibkeep_testkit::assert_empty!(listed.sealed);
	library.lock_secrets();
	let listed = library.tree_secrets(personal, tree).expect("lists");
	assert_eq!(listed.secret, [".env", "cert.pem"]);
}

#[test]
fn an_ordinary_file_that_a_change_made_secret_stays_in_its_tree_and_is_never_sealed_as_ordinary() {
	let mut rng = TestRng::new("tree typed secret later");
	let (directory, mut library, tree, root) = with_tree(
		"tree typed secret later",
		&[("notes.md", "# Notes\n"), ("plan.md", "Plan\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let notes = at_path(&library, tree, "notes.md");
	// As an AI agent's replica may, a change gives an ordinary file the type of secret files.
	library
		.edit_entity(
			personal,
			notes,
			vec![Edit::AddType(SECRET_FILE)],
			&today(),
			now() + 2_000,
			&mut rng,
		)
		.expect("adds the type");
	let updated = library.update_tree(personal, tree, false).expect("updates");
	nibkeep_testkit::assert_empty!(updated.deleted);
	assert_eq!(read(&root, "notes.md"), "# Notes\n");
	assert_eq!(at_path(&library, tree, "notes.md"), notes);
	// Its version, an ordinary one, is none of a secret file's, so an edit goes in on top
	// only with force, and sealed for the secrets.
	write(&root, "notes.md", SENTINEL);
	let forced = Commit::new(None).with_force();
	let committed = commit(&mut library, tree, &forced, now() + 3_000, &mut rng);
	assert_eq!(committed.secret, ["notes.md"]);
	assert!(!nibkeep_testkit::holds_anywhere(
		&directory,
		SENTINEL.as_bytes()
	));
}

#[test]
fn the_log_names_each_version_of_a_secret_file_and_a_locked_diff_leaves_it_out() {
	let mut rng = TestRng::new("tree secret log");
	let (_, mut library, tree, root) = with_tree(
		"tree secret log",
		&[(".env", "TOKEN=1\n"), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let first = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 1_500,
		&mut rng,
	);
	assert_eq!(first.revision, None, "nothing changed yet");
	write(&root, ".env", "TOKEN=2\n");
	let edited = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	)
	.revision
	.expect("a revision");
	let log = library
		.tree_log(personal, tree, Some(".env"))
		.expect("reads the log");
	assert_eq!(log.len(), 2, "{log:?}");
	assert_eq!(log[0].revision, Some(edited));
	library.lock_secrets();
	let diffs = library
		.tree_diff(
			personal,
			tree,
			Some((TreePoint::Revision(edited), TreePoint::Now)),
		)
		.expect("diffs");
	nibkeep_testkit::assert_empty!(diffs);
}

#[test]
fn a_secret_file_is_the_tree_s_whose_commit_made_it_at_its_path_there() {
	let mut rng = TestRng::new("tree of a secret file");
	let (_, mut library, tree, _) = with_tree(
		"tree of a secret file",
		&[("api/.env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let env = at_path(&library, tree, "api/.env");
	let in_tree =
		|library: &Library, file| library.secret_file_in_tree(personal, file).expect("reads");
	let at = |path: &str| {
		Some(InTree {
			tree,
			path: Some(path.to_owned()),
		})
	};
	assert_eq!(in_tree(&library, env), at("api/.env"));
	library
		.move_in_tree(
			personal,
			tree,
			("api/.env", "server/.env"),
			now() + 2_000,
			&mut rng,
		)
		.expect("moves");
	assert_eq!(in_tree(&library, env), at("server/.env"));
	// Its secrets locked, as on a device without them, it is in its tree all the same.
	library.lock_secrets();
	assert_eq!(in_tree(&library, env), at("server/.env"));
	library.unlock_secrets(&SECRETS).expect("unlocks");
	// An ordinary file of the tree is none.
	let notes = at_path(&library, tree, "notes.md");
	assert_eq!(in_tree(&library, notes), None);

	// As an AI agent's replica may, a change names another tree for it, and puts the
	// file of a password record into this tree, at a path a person would trust: neither counts.
	let scan = library
		.add_secret_file(
			personal,
			"passport.pdf",
			SENTINEL.as_bytes(),
			&[],
			now() + 3_000,
			&mut rng,
		)
		.expect("adds a secret file");
	for (entity, edits) in [
		(env, vec![Edit::Set(IN_TREE, vec![Value::Reference(notes)])]),
		(
			scan.file,
			vec![
				Edit::Set(IN_TREE, vec![Value::Reference(tree)]),
				Edit::Set(TREE_PATH, vec![Value::Text("config/.env".to_owned())]),
			],
		),
	] {
		library
			.edit_entity(personal, entity, edits, &today(), now() + 4_000, &mut rng)
			.expect("changes it");
	}
	assert_eq!(in_tree(&library, env), at("server/.env"));
	assert_eq!(in_tree(&library, scan.file), None);
}

#[test]
fn a_secret_file_deleted_from_its_tree_is_still_the_tree_s() {
	let mut rng = TestRng::new("tree of a deleted secret file");
	let (_, mut library, tree, root) = with_tree(
		"tree of a deleted secret file",
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	let env = at_path(&library, tree, ".env");
	fs::remove_file(root.join(".env")).expect("deletes");
	commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	);
	assert!(
		library
			.entity(personal, env, this_day())
			.expect("reads")
			.expect("the file")
			.deleted
	);
	let in_tree = |library: &Library| library.secret_file_in_tree(personal, env).expect("reads");
	assert_eq!(
		in_tree(&library),
		Some(InTree {
			tree,
			path: Some(".env".to_owned()),
		})
	);
	// A path no tree may hold, as any replica may give it, is none.
	library
		.edit_entity(
			personal,
			env,
			vec![Edit::Set(
				TREE_PATH,
				vec![Value::Text("../outside/.env".to_owned())],
			)],
			&today(),
			now() + 3_000,
			&mut rng,
		)
		.expect("changes its path");
	assert_eq!(in_tree(&library), Some(InTree { tree, path: None }));
}

/// What an update of the laptop's copy has to do when a crash cuts it short: the desk moved
/// the laptop's key to `deploy/keys/server.key`, then gave it a new version. Returns the
/// laptop and its folder, its tree and copy, and the key's new version.
fn key_moved_and_edited_on_desk(
	name: &str,
	rng: &mut TestRng,
) -> (Library, PathBuf, EntityId, PathBuf, String) {
	let key = format!("-----BEGIN PRIVATE KEY-----\n{SENTINEL}");
	let (directory, mut laptop, tree, root) = with_tree(
		name,
		&[("deploy/server.key", &key), ("notes.md", "# Notes\n")],
		rng,
	);
	let (_, mut desk) = desk_of(&mut laptop, &format!("{name}, desk"), true, rng);
	let personal = laptop.personal_space();
	let on_desk = folder(&format!("{name}, desk copy"));
	fs::remove_dir(&on_desk).expect("removes");
	desk.write_tree(personal, tree, &on_desk, TreePoint::Now, now() + 2_000, rng)
		.expect("the desk writes its copy");
	fs::create_dir(on_desk.join("deploy/keys")).expect("makes a folder");
	fs::rename(
		on_desk.join("deploy/server.key"),
		on_desk.join("deploy/keys/server.key"),
	)
	.expect("moves");
	commit(&mut desk, tree, &Commit::new(None), now() + 3_000, rng);
	let new_key = format!("{key}, renewed\n");
	write(&on_desk, "deploy/keys/server.key", &new_key);
	commit(&mut desk, tree, &Commit::new(None), now() + 4_000, rng);
	synced(&mut laptop, &mut desk, now() + 5_000, (1, 2));
	(laptop, directory, tree, root, new_key)
}

/// Checks that the laptop's copy, which an update cut short left with its key at the new
/// path, holds nothing for a commit to record, with the secrets locked, as the library opens
/// after the crash, or unlocked, and that an update then leaves the key's new version there,
/// writing it if `behind`.
fn nothing_to_commit_then_updated(
	(laptop, directory): (Library, &Path),
	(tree, root): (EntityId, &Path),
	(new_key, behind): (&str, bool),
	rng: &mut TestRng,
) {
	drop(laptop);
	let mut laptop = Library::open(directory, &Unlocker::Passphrase("correct horse"))
		.expect("opens after the crash");
	let laptop = &mut laptop;
	let personal = laptop.personal_space();
	let behind: &[&str] = if behind {
		&["deploy/keys/server.key"]
	} else {
		&[]
	};
	let locked = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(locked.changes);
	laptop.unlock_secrets(&SECRETS).expect("unlocks");
	let status = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	assert_eq!(status.behind, behind);
	let committed = commit(laptop, tree, &Commit::new(None), now() + 6_000, rng);
	assert_eq!(committed.revision, None, "{committed:?}");
	let updated = laptop
		.update_tree_agreeing(personal, tree, false)
		.expect("updates");
	assert_eq!(updated.written, behind, "{updated:?}");
	assert_eq!(read(root, "deploy/keys/server.key"), new_key);
	assert!(!root.join("deploy/server.key").exists());
	let entries = laptop.tree_entries(personal, tree).expect("reads");
	let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
	assert_eq!(paths, ["deploy/keys/server.key", "notes.md"]);
	let status = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
}

#[test]
fn an_update_cut_short_after_moving_a_secret_file_leaves_nothing_to_commit() {
	let mut rng = TestRng::new("tree secret move cut short");
	let (laptop, directory, tree, root, new_key) =
		key_moved_and_edited_on_desk("tree secret move cut short", &mut rng);
	// Cut short after its move, before it wrote the new version and recorded either: the
	// copy holds the version before at the new path.
	let personal = laptop.personal_space();
	let updated = laptop
		.update_tree_cut_after(personal, tree, 0)
		.expect("updates");
	nibkeep_testkit::assert_empty!(updated.written);
	let before = new_key.strip_suffix(", renewed\n").expect("the key before");
	assert_eq!(read(&root, "deploy/keys/server.key"), before);
	nothing_to_commit_then_updated(
		(laptop, &directory),
		(tree, &root),
		(&new_key, true),
		&mut rng,
	);
}

#[test]
fn an_update_cut_short_after_writing_a_secret_file_leaves_nothing_to_commit_even_locked() {
	let mut rng = TestRng::new("tree secret write cut short");
	let (laptop, directory, tree, root, new_key) =
		key_moved_and_edited_on_desk("tree secret write cut short", &mut rng);
	// Cut short after it moved the key and wrote its new version, before it recorded either:
	// with the secrets locked, only what the update noted first tells that it wrote it.
	let personal = laptop.personal_space();
	let updated = laptop
		.update_tree_cut_after(personal, tree, 1)
		.expect("updates");
	assert_eq!(updated.written, ["deploy/keys/server.key"]);
	assert_eq!(read(&root, "deploy/keys/server.key"), new_key);
	nothing_to_commit_then_updated(
		(laptop, &directory),
		(tree, &root),
		(&new_key, false),
		&mut rng,
	);
}

#[test]
fn an_update_cut_short_while_moving_a_secret_file_puts_it_back_before_a_commit() {
	let mut rng = TestRng::new("tree secret aside");
	let (laptop, _, tree, root, new_key) =
		key_moved_and_edited_on_desk("tree secret aside", &mut rng);
	// As an update cut short between its moves' two steps: the copy put aside under a
	// temporary name of its own, which the walk passes over.
	let mut hasher = blake3::Hasher::new();
	hasher.update(b"moved aside\0server.key");
	let aside = format!(".nib-tree-{}", &hasher.finalize().to_hex()[..16]);
	fs::rename(
		root.join("deploy/server.key"),
		root.join("deploy").join(&aside),
	)
	.expect("puts aside");
	let personal = laptop.personal_space();
	let status = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	assert!(root.join("deploy/server.key").exists());
	assert!(!root.join("deploy").join(&aside).exists());
	let updated = laptop
		.update_tree_agreeing(personal, tree, false)
		.expect("updates");
	assert_eq!(
		updated.moved,
		[(
			"deploy/server.key".to_owned(),
			"deploy/keys/server.key".to_owned()
		)]
	);
	assert_eq!(read(&root, "deploy/keys/server.key"), new_key);
	let status = laptop.tree_status(personal, tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
	nibkeep_testkit::assert_empty!(status.behind);
}

/// The files of a replica's index of its copies of trees, by name.
fn index_files(directory: &Path) -> Vec<(String, Vec<u8>)> {
	let replica = common::replica_folder(directory);
	fs::read_dir(&replica)
		.expect("lists the replica")
		.map(|entry| entry.expect("an entry"))
		.filter(|entry| {
			entry
				.file_name()
				.to_string_lossy()
				.starts_with("trees.sqlite")
		})
		.map(|entry| {
			let name = entry.file_name().to_string_lossy().into_owned();
			(name, fs::read(entry.path()).expect("reads"))
		})
		.collect()
}

#[test]
fn a_commit_cut_short_before_the_index_took_it_in_leaves_later_edits_to_commit() {
	let mut rng = TestRng::new("tree commit before index");
	let (directory, library, tree, root) = with_tree(
		"tree commit before index",
		&[(".env", SENTINEL), ("notes.md", "# Notes\n")],
		&mut rng,
	);
	let personal = library.personal_space();
	drop(library);
	let before = index_files(&directory);
	let reopen = || {
		let mut library =
			Library::open(&directory, &Unlocker::Passphrase("correct horse")).expect("opens");
		library.unlock_secrets(&SECRETS).expect("unlocks");
		library
	};
	let mut library = reopen();
	write(&root, ".env", "TOKEN=second\n");
	write(&root, "notes.md", "# Notes, second\n");
	write(&root, "deploy/server.key", "KEY second\n");
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 2_000,
		&mut rng,
	);
	assert_eq!(committed.changes.len(), 3, "{committed:?}");
	drop(library);
	// As a crash after the commit's parts were saved, before the index took them in.
	let replica = common::replica_folder(&directory);
	for (name, _) in index_files(&directory) {
		fs::remove_file(replica.join(name)).expect("removes");
	}
	for (name, bytes) in &before {
		fs::write(replica.join(name), bytes).expect("puts back");
	}
	let mut library = reopen();
	// The person goes on editing.
	write(&root, ".env", "TOKEN=third\n");
	write(&root, "notes.md", "# Notes, third\n");
	write(&root, "deploy/server.key", "KEY third\n");
	let status = library.tree_status(personal, tree).expect("status");
	let changes: Vec<(&str, &Change)> = status
		.changes
		.iter()
		.map(|change| (change.path.as_str(), &change.change))
		.collect();
	assert_eq!(
		changes,
		[
			(".env", &Change::Edited),
			("deploy/server.key", &Change::Edited),
			("notes.md", &Change::Edited)
		]
	);
	nibkeep_testkit::assert_empty!(status.behind);
	let committed = commit(
		&mut library,
		tree,
		&Commit::new(None),
		now() + 3_000,
		&mut rng,
	);
	assert_eq!(committed.changes.len(), 3, "{committed:?}");
	let entries = library.tree_entries(personal, tree).expect("reads");
	assert_eq!(entries.len(), 3, "one file at each path: {entries:?}");
}
