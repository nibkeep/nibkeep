//! The Git repositories in trees: recorded by their remotes, branches and pinned commits, as
//! `nib` reads them from Git, with no credential; listed to clone on another device, never
//! from an address that could make Git run anything but a clone over an encrypted
//! connection, nor one that an AI agent's replica recorded; and cloned only into folders
//! checked as a file's place is ([technical design: trees]).
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{library, now, sync_in_memory};
use nibkeep_base::EntityId;
use nibkeep_model::Value;
use nibkeep_node::{
	Change, Commit, Edit, GitRepository, Library, NodeError, PairingCode, RecordedRepository,
	TreeChange, TreePoint, resolved_path,
};
use nibkeep_testkit::TestRng;

/// A folder of its own, as the file system resolves it.
fn folder(name: &str) -> PathBuf {
	let made = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), name);
	resolved_path(&made).expect("resolves")
}

/// A new folder's place, where nothing is yet.
fn new_folder(name: &str) -> PathBuf {
	let made = folder(name);
	fs::remove_dir(&made).expect("removes");
	made
}

/// Writes a file and makes it look written an hour ago.
fn write(root: &Path, path: &str, content: &[u8]) {
	let path = root.join(path);
	fs::create_dir_all(path.parent().expect("a folder")).expect("makes folders");
	fs::write(&path, content).expect("writes");
	let then = SystemTime::now() - Duration::from_secs(3_600);
	fs::File::options()
		.write(true)
		.open(&path)
		.and_then(|file| file.set_modified(then))
		.expect("ages");
}

/// Makes `path` hold a Git repository, as far as a tree can tell: a `.git` folder, and a
/// file of its own.
fn repository_at(root: &Path, path: &str) {
	fs::create_dir_all(root.join(path).join(".git")).expect("makes a repository");
	write(root, &format!("{path}/code.rs"), b"fn main() {}\n");
}

/// A commit named by 40 times `digit`.
fn commit(digit: char) -> String {
	std::iter::repeat_n(digit, 40).collect()
}

/// What Git would say of a repository at `path` with `remotes`, on `main`, following
/// `origin`, at the commit of `digit`s.
fn described(path: &str, remotes: &[(&str, &str)], digit: char) -> GitRepository {
	GitRepository {
		path: path.to_owned(),
		remotes: remotes
			.iter()
			.map(|(name, address)| ((*name).to_owned(), (*address).to_owned()))
			.collect(),
		branch: Some("main".to_owned()),
		upstream: Some("origin".to_owned()),
		commit: Some(commit(digit)),
	}
}

const ORIGIN: (&str, &str) = ("origin", "https://example.com/me/proj.git");

/// A library with a tree whose folder holds `notes.md` and a repository at `proj`, recorded
/// as Git describes it.
struct One {
	library: Library,
	tree: EntityId,
	root: PathBuf,
	at: u64,
	rng: TestRng,
}

impl One {
	fn new(name: &str, remotes: &[(&str, &str)]) -> Self {
		let mut rng = TestRng::new(name);
		let (_, mut library) = library(&format!("{name}, laptop"), &mut rng);
		let root = folder(&format!("{name}, on laptop"));
		write(&root, "notes.md", b"notes\n");
		repository_at(&root, "proj");
		let personal = library.personal_space();
		let repositories = [described("proj", remotes, 'a')];
		let started = library
			.start_tree_with(
				personal,
				&root,
				&Commit::new(Some("Start")).with_repositories(&repositories),
				now() + 1_000,
				&mut rng,
			)
			.expect("starts");
		Self {
			library,
			tree: started.tree,
			root,
			at: now() + 2_000,
			rng,
		}
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn commit_with(&mut self, repositories: &[GitRepository]) -> Vec<TreeChange> {
		let at = self.tick();
		let personal = self.library.personal_space();
		self.library
			.commit_tree(
				personal,
				self.tree,
				&Commit::new(None).with_repositories(repositories),
				at,
				&mut self.rng,
			)
			.expect("commits")
			.repositories
	}

	fn recorded(&self) -> Vec<RecordedRepository> {
		let personal = self.library.personal_space();
		self.library
			.tree_repositories(personal, self.tree)
			.expect("lists")
	}
}

#[test]
fn a_commit_records_each_repository_by_its_remotes_with_no_credential() {
	let one = One::new(
		"tree git records",
		&[
			(
				"origin",
				"https://ada:secret-token@example.com/me/proj.git?private_token=1",
			),
			("backup", "ssh://git:password@backup.example/me/proj.git"),
		],
	);
	let recorded = one.recorded();
	assert_eq!(recorded.len(), 1);
	let proj = &recorded[0];
	assert_eq!(proj.path, "proj");
	assert_eq!(
		proj.remotes,
		[
			(
				"backup".to_owned(),
				"ssh://git@backup.example/me/proj.git".to_owned()
			),
			(
				"origin".to_owned(),
				"https://example.com/me/proj.git".to_owned()
			),
		]
	);
	assert_eq!(proj.branch.as_deref(), Some("main"));
	assert_eq!(proj.upstream.as_deref(), Some("origin"));
	assert_eq!(proj.pinned, None);
	assert!(proj.here);
	// The repository is a reference, never file by file.
	let personal = one.library.personal_space();
	let entries = one.library.tree_entries(personal, one.tree).expect("lists");
	assert!(entries.iter().all(|entry| !entry.path.starts_with("proj")));
	let status = one.library.tree_status(personal, one.tree).expect("status");
	assert_eq!(status.repositories, ["proj"]);
}

#[test]
fn a_repository_whose_remotes_or_branch_change_is_recorded_again() {
	let mut one = One::new("tree git changes", &[ORIGIN]);
	let mut edited = described(
		"proj",
		&[ORIGIN, ("fork", "git@example.com:me/fork.git")],
		'b',
	);
	edited.branch = Some("work".to_owned());
	let changes = one.commit_with(&[edited]);
	assert_eq!(
		changes,
		[TreeChange {
			path: "proj".to_owned(),
			change: Change::Edited
		}]
	);
	let recorded = one.recorded();
	assert_eq!(recorded[0].branch.as_deref(), Some("work"));
	assert_eq!(recorded[0].remotes.len(), 2);
	// The same again records nothing; an unpinned repository's commit is not recorded.
	let mut same = described(
		"proj",
		&[ORIGIN, ("fork", "git@example.com:me/fork.git")],
		'c',
	);
	same.branch = Some("work".to_owned());
	nibkeep_testkit::assert_empty!(one.commit_with(&[same]));
}

#[test]
fn a_repository_moved_keeps_its_record() {
	let mut one = One::new("tree git moves", &[ORIGIN]);
	let entity = one.recorded()[0].entity;
	fs::create_dir(one.root.join("tools")).expect("makes a folder");
	fs::rename(one.root.join("proj"), one.root.join("tools/proj")).expect("moves");
	let changes = one.commit_with(&[described("tools/proj", &[ORIGIN], 'a')]);
	assert_eq!(
		changes,
		[TreeChange {
			path: "tools/proj".to_owned(),
			change: Change::Moved {
				from: "proj".to_owned()
			}
		}]
	);
	let recorded = one.recorded();
	assert_eq!(recorded.len(), 1);
	assert_eq!(recorded[0].entity, entity);
	assert_eq!(recorded[0].path, "tools/proj");
}

#[test]
fn a_repository_whose_folder_is_gone_is_deleted_from_the_tree_only_when_git_was_read() {
	let mut one = One::new("tree git deletes", &[ORIGIN]);
	fs::remove_dir_all(one.root.join("proj")).expect("deletes");
	// A commit that was not told what Git says leaves repositories as they are.
	write(&one.root, "notes.md", b"notes, again\n");
	let at = one.tick();
	let personal = one.library.personal_space();
	one.library
		.commit_tree(personal, one.tree, &Commit::new(None), at, &mut one.rng)
		.expect("commits");
	assert_eq!(one.recorded().len(), 1);
	let changes = one.commit_with(&[]);
	assert_eq!(
		changes,
		[TreeChange {
			path: "proj".to_owned(),
			change: Change::Deleted
		}]
	);
	nibkeep_testkit::assert_empty!(one.recorded());
}

#[test]
fn a_pinned_repository_keeps_the_commit_that_git_says() {
	let mut one = One::new("tree git pins", &[ORIGIN]);
	let personal = one.library.personal_space();
	let at = one.tick();
	one.library
		.pin_repository(
			personal,
			one.tree,
			"proj",
			Some(&commit('a')),
			at,
			&mut one.rng,
		)
		.expect("pins");
	assert_eq!(one.recorded()[0].pinned, Some(commit('a')));
	one.commit_with(&[described("proj", &[ORIGIN], 'b')]);
	assert_eq!(one.recorded()[0].pinned, Some(commit('b')));
	let at = one.tick();
	one.library
		.pin_repository(personal, one.tree, "proj", None, at, &mut one.rng)
		.expect("unpins");
	assert_eq!(one.recorded()[0].pinned, None);
	one.commit_with(&[described("proj", &[ORIGIN], 'c')]);
	assert_eq!(one.recorded()[0].pinned, None);
	let at = one.tick();
	assert!(matches!(
		one.library
			.pin_repository(personal, one.tree, "proj", Some("HEAD"), at, &mut one.rng),
		Err(NodeError::NotACommit(_))
	));
	let at = one.tick();
	assert!(matches!(
		one.library.pin_repository(
			personal,
			one.tree,
			"nowhere",
			Some(&commit('d')),
			at,
			&mut one.rng
		),
		Err(NodeError::NoRepository(_))
	));
}

/// A desk that joined the library of `One`, with no copy of the tree yet.
fn desk_of(one: &mut One, name: &str) -> Library {
	let desk_directory = new_folder(&format!("{name}, desk"));
	let code = PairingCode::generate(&mut one.rng);
	let (paired, joined) = common::pair_with(
		&mut one.library,
		&desk_directory,
		&code,
		&code,
		&mut one.rng,
	);
	paired.expect("the laptop paired");
	joined.expect("the desk joined").library
}

#[test]
fn a_repository_in_the_folder_a_tree_is_written_into_is_left_as_it_is() {
	let name = "tree written over its repository";
	let mut one = One::new(name, &[("origin", "https://example.com/proj.git")]);
	let mut desk = desk_of(&mut one, name);
	let on_desk = folder(&format!("{name}, on desk"));
	repository_at(&on_desk, "proj");
	write(&on_desk, "proj/mine.rs", b"// mine\n");
	let personal = desk.personal_space();
	let written = desk
		.write_tree(
			personal,
			one.tree,
			&on_desk,
			TreePoint::Now,
			now() + 70_000,
			&mut one.rng,
		)
		.expect("writes beside the repository");
	assert_eq!(written.written, ["notes.md"]);
	// The repository is the tree's, and what is inside it is Git's.
	nibkeep_testkit::assert_empty!(written.others);
	nibkeep_testkit::assert_empty!(
		desk.repositories_to_clone(personal, one.tree)
			.expect("lists")
	);
	assert_eq!(
		fs::read_to_string(on_desk.join("proj/mine.rs")).expect("reads"),
		"// mine\n"
	);
	let status = desk.tree_status(personal, one.tree).expect("status");
	nibkeep_testkit::assert_empty!(status.changes);
}

/// The laptop's library of `One`, and a desk that joined it and wrote the tree.
struct Two {
	one: One,
	desk: Library,
	on_desk: PathBuf,
}

impl Two {
	fn new(name: &str, remotes: &[(&str, &str)]) -> Self {
		let mut one = One::new(name, remotes);
		let mut desk = desk_of(&mut one, name);
		let on_desk = new_folder(&format!("{name}, on desk"));
		let personal = desk.personal_space();
		desk.write_tree(
			personal,
			one.tree,
			&on_desk,
			TreePoint::Now,
			now() + 70_000,
			&mut one.rng,
		)
		.expect("the desk writes its copy");
		one.at = now() + 80_000;
		Self { one, desk, on_desk }
	}

	/// The desk clones `proj`, as Git then describes it.
	fn clone_proj(&mut self, cloned: &GitRepository) {
		let personal = self.desk.personal_space();
		let tree = self.one.tree;
		let listed = self
			.desk
			.repositories_to_clone(personal, tree)
			.expect("lists");
		let proj = listed
			.iter()
			.find(|repository| repository.path == "proj")
			.expect("listed");
		self.desk
			.prepare_clone(personal, tree, proj)
			.expect("prepares");
		repository_at(&self.on_desk, "proj");
		self.desk
			.repository_cloned(personal, tree, (proj, cloned))
			.expect("notes");
	}

	fn sync(&mut self) {
		let at = self.one.tick();
		let (first, second) = sync_in_memory(&mut self.one.library, &mut self.desk, at, (1, 2));
		first.expect("the laptop syncs");
		second.expect("the desk syncs");
	}
}

#[test]
fn another_device_lists_the_repositories_to_clone_and_where_they_come_from() {
	let mut two = Two::new(
		"tree git to clone",
		&[ORIGIN, ("backup", "ssh://git@backup.example/me/proj.git")],
	);
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	let to_clone = two
		.desk
		.repositories_to_clone(personal, tree)
		.expect("lists");
	assert_eq!(to_clone.len(), 1);
	let proj = &to_clone[0];
	assert_eq!(proj.path, "proj");
	assert_eq!((proj.remote.as_str(), proj.address.as_str()), ORIGIN);
	assert_eq!(
		proj.others,
		[(
			"backup".to_owned(),
			"ssh://git@backup.example/me/proj.git".to_owned()
		)]
	);
	assert_eq!(proj.branch.as_deref(), Some("main"));
	assert_eq!(proj.recorded_by, "laptop");
	assert_eq!(proj.refused, None);
	assert!(!two.desk.tree_repositories(personal, tree).expect("lists")[0].here);
	// Not cloned here is not deleted: a commit here leaves it in the tree.
	write(&two.on_desk, "notes.md", b"notes, from the desk\n");
	let at = two.one.tick();
	two.desk
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_repositories(&[]),
			at,
			&mut two.one.rng,
		)
		.expect("commits");
	assert_eq!(
		two.desk
			.tree_repositories(personal, tree)
			.expect("lists")
			.len(),
		1
	);
	// Its folder made, as Git then clones into it, and noted once cloned.
	let place = two
		.desk
		.prepare_clone(personal, tree, proj)
		.expect("prepares");
	assert_eq!(place.folder, two.on_desk.join("proj"));
	assert!(place.folder.is_dir());
	nibkeep_testkit::assert_empty!(place.made);
	repository_at(&two.on_desk, "proj");
	two.desk
		.repository_cloned(personal, tree, (proj, &described("proj", &[ORIGIN], 'a')))
		.expect("notes");
	nibkeep_testkit::assert_empty!(
		two.desk
			.repositories_to_clone(personal, tree)
			.expect("lists")
	);
	assert!(two.desk.tree_repositories(personal, tree).expect("lists")[0].here);
	two.sync();
}

#[test]
fn addresses_that_could_make_git_run_anything_but_a_clone_are_refused() {
	for (number, address) in [
		"http://example.com/me/proj.git",
		"git://example.com/me/proj.git",
		"ssh://-oProxyCommand=touch%20pwned/proj",
		"-oProxyCommand=touch:proj",
		"/home/ada/proj.git",
		"ext::sh -c touch% pwned",
	]
	.into_iter()
	.enumerate()
	{
		// Named by number: Windows takes no colon in a folder's name.
		let two = Two::new(
			&format!("tree git refuses {number}"),
			&[("origin", address)],
		);
		let personal = two.desk.personal_space();
		let to_clone = two
			.desk
			.repositories_to_clone(personal, two.one.tree)
			.expect("lists");
		assert!(to_clone[0].refused.is_some(), "{address}: {to_clone:?}");
	}
}

#[test]
fn a_branch_that_git_could_read_as_an_option_is_refused() {
	let mut one = One::new("tree git branch option", &[ORIGIN]);
	let mut odd = described("proj", &[ORIGIN], 'a');
	odd.branch = Some("--upload-pack=touch pwned".to_owned());
	one.commit_with(&[odd]);
	let personal = one.library.personal_space();
	fs::remove_dir_all(one.root.join("proj")).expect("deletes");
	let one = forget_index(one);
	let to_clone = one
		.library
		.repositories_to_clone(personal, one.tree)
		.expect("lists");
	assert!(to_clone[0].refused.is_some(), "{to_clone:?}");
}

/// The library of `one` with its index of copies lost, as a crash on a new disk leaves it.
fn forget_index(one: One) -> One {
	let One {
		library,
		tree,
		root,
		at,
		rng,
	} = one;
	let replica = library.close();
	fs::remove_file(replica.directory().join("trees.sqlite")).expect("loses the index");
	let library = replica.open().expect("opens");
	One {
		library,
		tree,
		root,
		at,
		rng,
	}
}

#[test]
fn an_address_that_an_ai_agent_s_replica_recorded_is_never_cloned() {
	let mut one = One::new("tree git agent", &[ORIGIN]);
	let agent_directory = new_folder("tree git agent, agent");
	let code = PairingCode::generate(&mut one.rng);
	let (paired, joined) = common::pair_ai_agent_named(
		&mut one.library,
		&agent_directory,
		("laptop", "Helper"),
		&code,
		&mut one.rng,
	);
	paired.expect("the laptop paired");
	let mut agent = joined.expect("the agent joined").library;
	let personal = agent.personal_space();
	// The agent's replica adds a repository to the tree.
	agent
		.create_entity(
			personal,
			&[nibkeep_std::GIT_REPOSITORY],
			vec![
				(nibkeep_std::IN_TREE, Value::Reference(one.tree)),
				(nibkeep_std::TREE_PATH, Value::Text("helper".to_owned())),
				(
					nibkeep_std::GIT_REMOTE,
					Value::Text("origin https://example.com/helper.git".to_owned()),
				),
			],
			now() + 90_000,
			&mut one.rng,
		)
		.expect("records");
	// Its replica replaces nothing of the person's repository, whose rest a clone would trust.
	let proj = one.recorded()[0].entity;
	let branch = Value::Text("helper-work".to_owned());
	let replacing = agent.edit_entity(
		personal,
		proj,
		vec![Edit::Set(nibkeep_std::GIT_BRANCH, vec![branch.clone()])],
		&common::today(),
		now() + 91_000,
		&mut one.rng,
	);
	assert!(
		matches!(replacing, Err(NodeError::AiAgentMayNot(_))),
		"{replacing:?}"
	);
	// But it gives the repository a branch and a commit of its choice beside the person's.
	agent
		.edit_entity(
			personal,
			proj,
			vec![
				Edit::Add(nibkeep_std::GIT_BRANCH, branch),
				Edit::Add(nibkeep_std::GIT_COMMIT, Value::Text(commit('f'))),
			],
			&common::today(),
			now() + 91_000,
			&mut one.rng,
		)
		.expect("edits");
	let at = one.tick() + 100_000;
	let (first, second) = sync_in_memory(&mut one.library, &mut agent, at, (1, 2));
	first.expect("the laptop syncs");
	second.expect("the agent syncs");
	fs::remove_dir_all(one.root.join("proj")).expect("deletes");
	let one = forget_index(one);
	let to_clone = one
		.library
		.repositories_to_clone(personal, one.tree)
		.expect("lists");
	assert_eq!(to_clone.len(), 2, "{to_clone:?}");
	for repository in &to_clone {
		let why = repository.refused.as_deref().unwrap_or_default();
		assert!(why.contains("AI agent"), "{repository:?}");
	}
}

#[test]
fn a_clone_is_never_made_through_a_link_nor_into_git_s_folders() {
	let mut one = One::new("tree git clone place", &[ORIGIN]);
	let personal = one.library.personal_space();
	// What another replica might record: a path into `.git`, or out of the tree.
	for path in [".git/hooks", "a/.GIT/x", "../outside"] {
		let created = one.library.create_entity(
			personal,
			&[nibkeep_std::GIT_REPOSITORY],
			vec![
				(nibkeep_std::IN_TREE, Value::Reference(one.tree)),
				(nibkeep_std::TREE_PATH, Value::Text(path.to_owned())),
				(
					nibkeep_std::GIT_REMOTE,
					Value::Text("origin https://example.com/x.git".to_owned()),
				),
			],
			now() + 5_000,
			&mut one.rng,
		);
		created.expect("records");
		let listed = one
			.library
			.repositories_to_clone(personal, one.tree)
			.expect("lists");
		let to = listed
			.iter()
			.find(|repository| repository.path == path)
			.expect("listed");
		assert!(
			one.library.prepare_clone(personal, one.tree, to).is_err(),
			"{path}"
		);
	}
	let to_clone = one
		.library
		.repositories_to_clone(personal, one.tree)
		.expect("lists");
	assert!(
		to_clone
			.iter()
			.all(|repository| repository.refused.is_some()),
		"{to_clone:?}"
	);
	// Through a link to another folder.
	#[cfg(unix)]
	{
		let elsewhere = folder("tree git clone place, elsewhere");
		std::os::unix::fs::symlink(&elsewhere, one.root.join("linked")).expect("links");
		let created = one.library.create_entity(
			personal,
			&[nibkeep_std::GIT_REPOSITORY],
			vec![
				(nibkeep_std::IN_TREE, Value::Reference(one.tree)),
				(
					nibkeep_std::TREE_PATH,
					Value::Text("linked/proj".to_owned()),
				),
				(
					nibkeep_std::GIT_REMOTE,
					Value::Text("origin https://example.com/x.git".to_owned()),
				),
			],
			now() + 6_000,
			&mut one.rng,
		);
		created.expect("records");
		let listed = one
			.library
			.repositories_to_clone(personal, one.tree)
			.expect("lists");
		let to = listed
			.iter()
			.find(|repository| repository.path == "linked/proj")
			.expect("listed");
		assert!(one.library.prepare_clone(personal, one.tree, to).is_err());
		assert!(!elsewhere.join("proj").exists());
	}
}

#[test]
fn a_repository_added_elsewhere_is_listed_after_an_update_and_one_deleted_elsewhere_stays() {
	let mut two = Two::new("tree git elsewhere", &[ORIGIN]);
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	// The desk clones proj.
	two.clone_proj(&described("proj", &[ORIGIN], 'a'));
	// The laptop adds another, and deletes proj.
	repository_at(&two.one.root, "lib");
	fs::remove_dir_all(two.one.root.join("proj")).expect("deletes");
	two.one.commit_with(&[described(
		"lib",
		&[("origin", "https://example.com/me/lib.git")],
		'e',
	)]);
	two.sync();
	two.desk
		.update_tree(personal, tree, false)
		.expect("updates");
	let to_clone = two
		.desk
		.repositories_to_clone(personal, tree)
		.expect("lists");
	assert_eq!(to_clone.len(), 1);
	assert_eq!(to_clone[0].path, "lib");
	// The desk's proj stays, with the work it may hold, and its commits leave it out.
	assert!(two.on_desk.join("proj/code.rs").exists());
	let at = two.one.tick();
	let committed = two
		.desk
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_repositories(&[described("proj", &[ORIGIN], 'a')]),
			at,
			&mut two.one.rng,
		)
		.expect("commits");
	nibkeep_testkit::assert_empty!(committed.repositories);
	let recorded = two.desk.tree_repositories(personal, tree).expect("lists");
	assert_eq!(recorded.len(), 1);
	assert_eq!(recorded[0].path, "lib");
}

#[test]
fn a_commit_of_some_paths_leaves_the_other_repositories_as_they_are() {
	let mut one = One::new("tree git some paths", &[ORIGIN]);
	write(&one.root, "notes.md", b"notes, again\n");
	let at = one.tick();
	let personal = one.library.personal_space();
	let committed = one
		.library
		.commit_tree(
			personal,
			one.tree,
			&Commit::new(None)
				.with_paths(vec!["notes.md".to_owned()])
				.with_repositories(&[]),
			at,
			&mut one.rng,
		)
		.expect("commits");
	nibkeep_testkit::assert_empty!(committed.repositories);
	assert_eq!(one.recorded().len(), 1);
}

#[test]
fn a_device_records_only_what_changed_of_a_repository_there() {
	let backup = ("backup", "/mnt/usb/proj.git");
	let mut two = Two::new("tree git each device", &[ORIGIN, backup]);
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	let to_clone = two
		.desk
		.repositories_to_clone(personal, tree)
		.expect("lists");
	// A clone takes only what it may: origin, not the folder on a drive.
	assert_eq!(to_clone[0].others, []);
	let cloned = described("proj", &[ORIGIN], 'a');
	two.clone_proj(&cloned);
	// The desk's commits leave the laptop's backup in the record.
	let at = two.one.tick();
	let committed = two
		.desk
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_repositories(&[cloned]),
			at,
			&mut two.one.rng,
		)
		.expect("commits");
	nibkeep_testkit::assert_empty!(committed.repositories);
	// A remote the desk adds is recorded, beside the laptop's.
	let mut added = described(
		"proj",
		&[ORIGIN, ("fork", "git@example.com:me/fork.git")],
		'a',
	);
	added.branch = Some("main".to_owned());
	let at = two.one.tick();
	two.desk
		.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_repositories(&[added]),
			at,
			&mut two.one.rng,
		)
		.expect("commits");
	let remotes: Vec<String> = two.desk.tree_repositories(personal, tree).expect("lists")[0]
		.remotes
		.iter()
		.map(|(name, _)| name.clone())
		.collect();
	assert_eq!(remotes, ["backup", "fork", "origin"]);
}

#[test]
fn a_clone_comes_from_a_remote_it_may_take_and_never_goes_inside_another_repository() {
	let mut one = One::new(
		"tree git choice",
		&[("backup", "/mnt/usb/proj.git"), ORIGIN],
	);
	let mut odd = described("proj", &[("backup", "/mnt/usb/proj.git"), ORIGIN], 'a');
	odd.upstream = Some("backup".to_owned());
	one.commit_with(&[odd]);
	let personal = one.library.personal_space();
	// Another repository, recorded inside proj.
	one.library
		.create_entity(
			personal,
			&[nibkeep_std::GIT_REPOSITORY],
			vec![
				(nibkeep_std::IN_TREE, Value::Reference(one.tree)),
				(
					nibkeep_std::TREE_PATH,
					Value::Text("proj/vendor/x".to_owned()),
				),
				(
					nibkeep_std::GIT_REMOTE,
					Value::Text("origin https://example.com/x.git".to_owned()),
				),
			],
			now() + 9_000,
			&mut one.rng,
		)
		.expect("records");
	let mut one = forget_index(one);
	let to_clone = one
		.library
		.repositories_to_clone(personal, one.tree)
		.expect("lists");
	let inside = to_clone
		.iter()
		.find(|repository| repository.path == "proj/vendor/x")
		.expect("listed");
	assert_eq!(
		inside.refused.as_deref(),
		Some("it is inside another Git repository")
	);
	fs::remove_dir_all(one.root.join("proj")).expect("deletes");
	let to_clone = one
		.library
		.repositories_to_clone(personal, one.tree)
		.expect("lists");
	let proj = to_clone
		.iter()
		.find(|repository| repository.path == "proj")
		.expect("listed");
	assert_eq!(
		(proj.remote.as_str(), proj.refused.as_deref()),
		("origin", None)
	);
	let _ = one.tick();
}

#[test]
fn a_copy_with_nothing_left_deletes_no_repository_without_force() {
	let mut rng = TestRng::new("tree git emptied");
	let (_, mut library) = library("tree git emptied, laptop", &mut rng);
	let root = folder("tree git emptied, on laptop");
	repository_at(&root, "proj");
	let personal = library.personal_space();
	let repositories = [described("proj", &[ORIGIN], 'a')];
	let tree = library
		.start_tree_with(
			personal,
			&root,
			&Commit::new(None).with_repositories(&repositories),
			now() + 1_000,
			&mut rng,
		)
		.expect("starts")
		.tree;
	fs::remove_dir_all(root.join("proj")).expect("empties");
	assert!(matches!(
		library.commit_tree(
			personal,
			tree,
			&Commit::new(None).with_repositories(&[]),
			now() + 2_000,
			&mut rng
		),
		Err(NodeError::TreeEmptied(_))
	));
}

#[cfg(unix)]
#[test]
fn a_repository_in_a_folder_that_cannot_be_read_is_not_taken_for_deleted() {
	use std::os::unix::fs::PermissionsExt as _;
	let mut rng = TestRng::new("tree git unreadable");
	let (_, mut library) = library("tree git unreadable, laptop", &mut rng);
	let root = folder("tree git unreadable, on laptop");
	write(&root, "notes.md", b"notes\n");
	repository_at(&root, "work/proj");
	let personal = library.personal_space();
	let repositories = [described("work/proj", &[ORIGIN], 'a')];
	let tree = library
		.start_tree_with(
			personal,
			&root,
			&Commit::new(None).with_repositories(&repositories),
			now() + 1_000,
			&mut rng,
		)
		.expect("starts")
		.tree;
	fs::set_permissions(root.join("work"), fs::Permissions::from_mode(0o000)).expect("locks");
	let committed = library.commit_tree(
		personal,
		tree,
		&Commit::new(None).with_repositories(&[]),
		now() + 2_000,
		&mut rng,
	);
	fs::set_permissions(root.join("work"), fs::Permissions::from_mode(0o755)).expect("unlocks");
	nibkeep_testkit::assert_empty!(committed.expect("commits").repositories);
	assert_eq!(
		library
			.tree_repositories(personal, tree)
			.expect("lists")
			.len(),
		1
	);
}

#[test]
fn a_move_counts_when_either_end_is_chosen() {
	let mut one = One::new("tree git move chosen", &[ORIGIN]);
	let entity = one.recorded()[0].entity;
	fs::create_dir(one.root.join("tools")).expect("makes a folder");
	fs::rename(one.root.join("proj"), one.root.join("tools/proj")).expect("moves");
	let at = one.tick();
	let personal = one.library.personal_space();
	let repositories = [described("tools/proj", &[ORIGIN], 'a')];
	let committed = one
		.library
		.commit_tree(
			personal,
			one.tree,
			&Commit::new(None)
				.with_paths(vec!["proj".to_owned()])
				.with_repositories(&repositories),
			at,
			&mut one.rng,
		)
		.expect("commits");
	assert_eq!(
		committed.repositories,
		[TreeChange {
			path: "tools/proj".to_owned(),
			change: Change::Moved {
				from: "proj".to_owned()
			}
		}]
	);
	assert_eq!(one.recorded()[0].entity, entity);
}

#[test]
fn a_folder_whose_git_is_gone_is_no_repository_any_longer() {
	let mut one = One::new("tree git no longer", &[ORIGIN]);
	fs::remove_dir_all(one.root.join("proj/.git")).expect("deletes");
	let changes = one.commit_with(&[]);
	assert_eq!(
		changes,
		[TreeChange {
			path: "proj".to_owned(),
			change: Change::Deleted
		}]
	);
	let personal = one.library.personal_space();
	let entries = one.library.tree_entries(personal, one.tree).expect("lists");
	assert!(entries.iter().any(|entry| entry.path == "proj/code.rs"));
}

#[test]
fn nothing_is_written_into_a_git_repository_s_folder() {
	let mut two = Two::new("tree git nothing inside", &[ORIGIN]);
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	two.clone_proj(&described("proj", &[ORIGIN], 'a'));
	// Another replica records a file inside proj.
	two.one
		.library
		.create_entity(
			personal,
			&[nibkeep_std::FILE],
			vec![
				(nibkeep_std::IN_TREE, Value::Reference(tree)),
				(
					nibkeep_std::TREE_PATH,
					Value::Text("proj/.envrc".to_owned()),
				),
			],
			now() + 85_000,
			&mut two.one.rng,
		)
		.expect("records");
	two.sync();
	two.desk
		.update_tree(personal, tree, false)
		.expect("updates");
	assert!(!two.on_desk.join("proj/.envrc").exists());
}

#[test]
fn a_commit_cut_short_records_no_repository() {
	let mut one = One::new("tree git cut short", &[ORIGIN]);
	write(&one.root, "a.txt", b"a\n");
	write(&one.root, "b.txt", b"b\n");
	repository_at(&one.root, "lib");
	let at = one.tick();
	let personal = one.library.personal_space();
	let repositories = [
		described("proj", &[ORIGIN], 'a'),
		described("lib", &[("origin", "https://example.com/me/lib.git")], 'b'),
	];
	one.library
		.commit_tree(
			personal,
			one.tree,
			&Commit::new(None)
				.cut_after(2)
				.with_repositories(&repositories),
			at,
			&mut one.rng,
		)
		.expect("commits a part");
	assert_eq!(one.recorded().len(), 1);
}

#[test]
fn files_of_the_tree_in_a_folder_that_became_a_repository_are_left_to_git() {
	let mut two = Two::new("tree git became repository", &[ORIGIN]);
	write(&two.one.root, "notes/todo.md", b"todo\n");
	write(&two.one.root, "notes/old.md", b"old\n");
	two.one.commit_with(&[]);
	two.sync();
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	two.desk
		.update_tree(personal, tree, false)
		.expect("updates");
	// The person makes notes a repository on the desk; the laptop edits and deletes.
	fs::create_dir(two.on_desk.join("notes/.git")).expect("makes a repository");
	write(&two.one.root, "notes/todo.md", b"todo, edited\n");
	fs::remove_file(two.one.root.join("notes/old.md")).expect("deletes");
	two.one.commit_with(&[]);
	two.sync();
	two.desk
		.update_tree(personal, tree, false)
		.expect("updates");
	assert_eq!(
		fs::read_to_string(two.on_desk.join("notes/todo.md")).expect("reads"),
		"todo\n"
	);
	assert!(two.on_desk.join("notes/old.md").exists());
}

#[test]
fn an_upstream_that_is_no_remote_s_name_is_not_recorded() {
	let mut one = One::new("tree git upstream address", &[ORIGIN]);
	let mut odd = described("proj", &[ORIGIN], 'a');
	odd.upstream = Some("https://ada:secret-token@example.com/fork.git".to_owned());
	odd.branch = Some("other".to_owned());
	one.commit_with(&[odd]);
	let recorded = one.recorded();
	assert_eq!(recorded[0].branch.as_deref(), Some("other"));
	assert_ne!(
		recorded[0].upstream.as_deref(),
		Some("https://ada:secret-token@example.com/fork.git")
	);
}

#[test]
fn a_bare_repository_is_neither_files_nor_a_reference() {
	let mut one = One::new("tree git bare", &[ORIGIN]);
	write(
		&one.root,
		"backups/notes.git/HEAD",
		b"ref: refs/heads/main\n",
	);
	write(
		&one.root,
		"backups/notes.git/hooks/post-receive",
		b"#!/bin/sh\n",
	);
	fs::create_dir_all(one.root.join("backups/notes.git/objects")).expect("makes a folder");
	fs::create_dir_all(one.root.join("backups/notes.git/refs")).expect("makes a folder");
	one.commit_with(&[]);
	let personal = one.library.personal_space();
	let entries = one.library.tree_entries(personal, one.tree).expect("lists");
	assert!(
		entries
			.iter()
			.all(|entry| !entry.path.starts_with("backups/"))
	);
	let status = one.library.tree_status(personal, one.tree).expect("status");
	assert_eq!(status.repositories, ["proj"]);
}

#[test]
fn a_clone_is_refused_once_what_the_person_agreed_to_changed() {
	let mut two = Two::new("tree git agreed", &[ORIGIN]);
	let personal = two.desk.personal_space();
	let tree = two.one.tree;
	let asked = two
		.desk
		.repositories_to_clone(personal, tree)
		.expect("lists")
		.remove(0);
	// Meanwhile the laptop moves it.
	fs::create_dir(two.one.root.join("tools")).expect("makes a folder");
	fs::rename(two.one.root.join("proj"), two.one.root.join("tools/proj")).expect("moves");
	two.one
		.commit_with(&[described("tools/proj", &[ORIGIN], 'a')]);
	two.sync();
	assert!(matches!(
		two.desk.prepare_clone(personal, tree, &asked),
		Err(NodeError::RepositoryChanged(_))
	));
	assert!(!two.on_desk.join("tools/proj").exists());
}
