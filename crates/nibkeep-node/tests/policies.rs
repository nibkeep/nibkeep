//! Storage policies: the defaults a space gets, how a file's policy is chosen, files kept
//! where they are, and the question about large files ([technical design: storage policies]).
//!
//! [technical design: storage policies]: ../../../docs/Technical-design.md#93-storage-policies

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{library, now, today};
use nibkeep_base::EntityId;
use nibkeep_model::{Value, fields};
use nibkeep_node::{
	ChosenBy, Edit, ExportKind, Kept, Library, Needs, NewRule, NodeError, POLICY, Place,
	PolicyEdit, STORAGE_POLICY, resolved_path,
};
use nibkeep_std::CAMERA_MAKE;
use nibkeep_testkit::TestRng;
use nibkeep_testkit::media::Photo;

/// A folder of files to add, apart from the library's.
fn folder(name: &str) -> PathBuf {
	nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, files"))
}

fn write(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
	let path = folder.join(name);
	fs::write(&path, bytes).expect("writes");
	path
}

/// The policies of the personal space, by name.
fn names(library: &Library) -> Vec<String> {
	library
		.policies(library.personal_space())
		.expect("reads")
		.into_iter()
		.filter_map(|policy| policy.name)
		.collect()
}

fn policy(library: &Library, name: &str) -> nibkeep_base::EntityId {
	library
		.find_policy(library.personal_space(), name)
		.expect("a policy")
}

fn photo() -> Vec<u8> {
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken: "2019:12:24 18:30:00",
		offset: Some("+01:00"),
		size: (4032, 3024),
		orientation: None,
		place: None,
	}
	.jpeg()
}

#[test]
fn a_space_gets_the_default_policies_once() {
	let mut rng = TestRng::new("defaults");
	let (_, mut library) = library("policies defaults", &mut rng);
	let personal = library.personal_space();
	assert_eq!(library.policies(personal).expect("reads"), []);
	let downloads = folder("policies defaults downloads");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs");
	assert_eq!(names(&library), ["Critical", "Replaceable", "Standard"]);
	let standard = policy(&library, "Standard");
	let replaceable = policy(&library, "Replaceable");
	let critical = library
		.policies(personal)
		.expect("reads")
		.into_iter()
		.find(|policy| policy.name.as_deref() == Some("Critical"))
		.expect("Critical");
	assert_eq!(
		critical.needs,
		Needs {
			copies: 3,
			away: 1,
			archived: 0,
			kept: Kept::Store
		}
	);
	let rules = library.rules(personal).expect("reads");
	assert_eq!(rules.len(), 3, "{rules:?}");
	assert_eq!(rules[0].conditions.has, Some(CAMERA_MAKE));
	assert_eq!(rules[0].policy, Some(standard));
	let folder_rule = rules[1].conditions.folder.as_ref().expect("a folder");
	assert_eq!(
		Path::new(&folder_rule.path),
		resolved_path(&downloads).expect("resolves")
	);
	assert_eq!(rules[1].policy, Some(replaceable));
	assert!(rules[2].conditions.is_empty());
	assert_eq!(rules[2].asks_above, Some(1 << 30));

	// Deleted, they are not installed again.
	let changes = library.status().expect("status").spaces[0].changes;
	library
		.remove_rule(personal, rules[1].id, now(), &mut rng)
		.expect("removes");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs nothing");
	assert_eq!(library.rules(personal).expect("reads").len(), 2);
	assert_eq!(
		library.status().expect("status").spaces[0].changes,
		changes + 1,
		"only the removal was committed"
	);
}

#[test]
fn rules_choose_in_order_and_a_file_of_its_own_wins() {
	let mut rng = TestRng::new("choosing");
	let (_, mut library) = library("policies choosing", &mut rng);
	let personal = library.personal_space();
	let files = folder("policies choosing");
	let photo = write(&files, "christmas.jpg", &photo());
	let note = write(&files, "note.txt", b"Milk, eggs, bread.\n");
	let photo = library
		.add_file(personal, &photo, now(), &mut rng)
		.expect("adds");
	let note = library
		.add_file(personal, &note, now(), &mut rng)
		.expect("adds");
	let rules = library.rules(personal).expect("reads");
	// The rules without a downloads folder: from a camera, and any other file.
	assert_eq!(rules.len(), 2);
	let choice = library.policy_of(personal, photo.file).expect("chooses");
	assert_eq!(choice.by, ChosenBy::Rule(rules[0].id));
	assert_eq!(choice.policy, Some(policy(&library, "Standard")));
	let choice = library.policy_of(personal, note.file).expect("chooses");
	assert_eq!(choice.by, ChosenBy::Rule(rules[1].id));

	// A rule put first wins over the others.
	let replaceable = policy(&library, "Replaceable");
	let text = NewRule {
		media_type: Some("text/".to_owned()),
		..NewRule::new(replaceable)
	};
	let notes = NewRule {
		of_type: Some(nibkeep_std::FILE),
		larger_than: Some(10),
		..NewRule::new(replaceable)
	};
	let rule = library
		.add_rule(personal, &notes, Place::First, now(), &mut rng)
		.expect("adds");
	assert_eq!(library.rules(personal).expect("reads")[0].id, rule);
	let choice = library.policy_of(personal, note.file).expect("chooses");
	assert_eq!(choice.by, ChosenBy::Rule(rule));
	assert_eq!(choice.needs.kept, Kept::InPlace);
	// The photo is larger than 10 bytes, and a file too.
	assert_eq!(
		library.policy_of(personal, photo.file).expect("chooses").by,
		ChosenBy::Rule(rule)
	);
	// A media type the note lacks: infer does not tell plain text.
	let text_rule = library
		.add_rule(personal, &text, Place::First, now(), &mut rng)
		.expect("adds");
	assert_ne!(
		library.policy_of(personal, note.file).expect("chooses").by,
		ChosenBy::Rule(text_rule)
	);

	// Moved last, the rule for files larger than 10 bytes chooses nothing before the others.
	library
		.move_rule(personal, rule, Place::Last, now(), &mut rng)
		.expect("moves");
	assert_eq!(
		library.policy_of(personal, photo.file).expect("chooses").by,
		ChosenBy::Rule(rules[0].id)
	);

	// A file's own policy wins, until it is cleared.
	let critical = policy(&library, "Critical");
	library
		.set_policy(personal, note.file, Some(critical), now(), &mut rng)
		.expect("sets");
	let choice = library.policy_of(personal, note.file).expect("chooses");
	assert_eq!((choice.by, choice.policy), (ChosenBy::Own, Some(critical)));
	library
		.set_policy(personal, note.file, None, now(), &mut rng)
		.expect("clears");
	assert_eq!(
		library.policy_of(personal, note.file).expect("chooses").by,
		ChosenBy::Rule(rules[1].id)
	);

	// Without rules, a file gets what Standard asks, in the store.
	for rule in library.rules(personal).expect("reads") {
		library
			.remove_rule(personal, rule.id, now(), &mut rng)
			.expect("removes");
	}
	let choice = library.policy_of(personal, note.file).expect("chooses");
	assert_eq!(
		(choice.by, choice.policy, choice.needs),
		(ChosenBy::Fallback, None, Needs::FALLBACK)
	);
}

#[test]
fn a_folder_rule_needs_every_place_a_file_was_found() {
	let mut rng = TestRng::new("folders");
	let (_, mut library) = library("policies folders", &mut rng);
	let personal = library.personal_space();
	let downloads = folder("policies folders downloads");
	let elsewhere = folder("policies folders elsewhere");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs");
	let film = write(&downloads, "film.mkv", b"a film, said to be one");
	let added = library
		.add_file(personal, &film, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.kept, Kept::InPlace, "a download is kept where it is");
	let choice = library.policy_of(personal, added.file).expect("chooses");
	assert_eq!(choice.policy, Some(policy(&library, "Replaceable")));
	// Found elsewhere too, it is no longer only a download, and goes into the store.
	let copy = write(&elsewhere, "film.mkv", b"a film, said to be one");
	let again = library
		.add_file(personal, &copy, now(), &mut rng)
		.expect("adds");
	assert_eq!(again.file, added.file);
	assert_eq!(again.kept, Kept::Store);
	let choice = library.policy_of(personal, added.file).expect("chooses");
	assert_eq!(choice.policy, Some(policy(&library, "Standard")));
}

#[test]
fn a_file_kept_where_it_is_counts_while_it_is_unchanged() {
	let mut rng = TestRng::new("in place");
	let (directory, mut library) = library("policies in place", &mut rng);
	let personal = library.personal_space();
	let downloads = folder("policies in place downloads");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs");
	let film = write(&downloads, "film.mkv", b"a film, said to be one");
	let added = library
		.add_file(personal, &film, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.kept, Kept::InPlace);
	// Nothing was sealed into the store.
	let blobs = nibkeep_store::blob_directory(&common::replica_folder(&directory));
	let sealed = walk(&blobs)
		.into_iter()
		.filter(|path| path.components().any(|part| part.as_os_str() == "blake3"))
		.count();
	assert_eq!(sealed, 0, "no sealed blob");

	let check = library.check_policies(personal, now()).expect("checks");
	let replaceable = check
		.policies
		.iter()
		.find(|count| count.name.as_deref() == Some("Replaceable"))
		.expect("Replaceable");
	assert_eq!((replaceable.files, replaceable.short), (1, 0));
	assert_eq!(replaceable.none_here, Vec::<EntityId>::new());
	// The file itself, for an app to list: its policy, size, disks and what it lacks.
	let checked = |check: &nibkeep_node::PolicyCheck| {
		let [one] = check.files.as_slice() else {
			panic!("one file: {:?}", check.files);
		};
		one.clone()
	};
	let counted = checked(&check);
	assert_eq!(
		(counted.file, counted.policy, counted.size, counted.disks),
		(
			added.file,
			replaceable.policy,
			Some(b"a film, said to be one".len() as u64),
			1
		)
	);
	assert!(!counted.lacks.any() && !counted.unchecked, "{counted:?}");
	let report = library.verify(now()).expect("verifies");
	assert!(report.is_sound(), "{report:?}");
	assert_eq!(report.spaces[0].in_place, 1);

	// An export copies it from where it is.
	let export = folder("policies in place export").join("export");
	let exported = library
		.export(personal, &export, ExportKind::Current)
		.expect("exports");
	assert_eq!(exported.missing_files, [], "{exported:?}");

	// Changed by another app, it no longer counts, nor goes into an export.
	fs::write(&film, b"another film altogether, longer").expect("writes");
	let check = library.check_policies(personal, now()).expect("checks");
	let replaceable = check
		.policies
		.iter()
		.find(|count| count.name.as_deref() == Some("Replaceable"))
		.expect("Replaceable");
	assert_eq!(replaceable.none_here, [added.file]);
	assert_eq!(replaceable.short, 1);
	let counted = checked(&check);
	assert_eq!(counted.disks, 0);
	assert!(counted.lacks.copies, "{counted:?}");
	let report = library.verify(now()).expect("verifies");
	assert!(
		report.is_sound(),
		"a changed original is not the library's fault"
	);
	assert_eq!(report.spaces[0].changed.len(), 1);
	let export = folder("policies in place changed export").join("export");
	let exported = library
		.export(personal, &export, ExportKind::Current)
		.expect("exports");
	assert_eq!(exported.missing_files, [added.content]);
	// NibKeep never touches the original.
	assert_eq!(
		fs::read(&film).expect("reads"),
		b"another film altogether, longer"
	);
}

/// Every file below a folder.
fn walk(folder: &Path) -> Vec<PathBuf> {
	let mut found = Vec::new();
	let Ok(entries) = fs::read_dir(folder) else {
		return found;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			found.extend(walk(&path));
		} else {
			found.push(path);
		}
	}
	found
}

#[test]
fn a_file_kept_where_it_is_goes_into_the_store_when_its_policy_wants() {
	let mut rng = TestRng::new("into the store");
	let (_, mut library) = library("policies into the store", &mut rng);
	let personal = library.personal_space();
	let downloads = folder("policies into the store downloads");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs");
	let film = write(&downloads, "film.mkv", b"a film, said to be one");
	let added = library
		.add_file(personal, &film, now(), &mut rng)
		.expect("adds");
	assert_eq!(added.kept, Kept::InPlace);
	let critical = policy(&library, "Critical");
	library
		.set_policy(personal, added.file, Some(critical), now(), &mut rng)
		.expect("sets");
	let check = library.check_policies(personal, now()).expect("checks");
	let counted = check
		.policies
		.iter()
		.find(|count| count.policy == Some(critical))
		.expect("Critical");
	assert_eq!(counted.not_stored, [added.file]);
	// Added again, unchanged, it is read into the store.
	let again = library
		.add_file(personal, &film, now(), &mut rng)
		.expect("adds");
	assert_eq!((again.file, again.kept), (added.file, Kept::Store));
	let check = library.check_policies(personal, now()).expect("checks");
	let counted = check
		.policies
		.iter()
		.find(|count| count.policy == Some(critical))
		.expect("Critical");
	assert_eq!(counted.not_stored, Vec::<EntityId>::new());
}

#[test]
fn a_large_file_asks_once_for_its_folder() {
	let mut rng = TestRng::new("asks");
	let (_, mut library) = library("policies asks", &mut rng);
	let personal = library.personal_space();
	library
		.install_policies(personal, None, now(), &mut rng)
		.expect("installs");
	// A rule that asks above 10 bytes, first.
	let standard = policy(&library, "Standard");
	let asking = NewRule {
		asks_above: Some(10),
		..NewRule::new(standard)
	};
	library
		.add_rule(personal, &asking, Place::First, now(), &mut rng)
		.expect("adds");
	let films = folder("policies asks films");
	let small = write(&films, "small.txt", b"tiny");
	let large = write(&films, "large.mkv", b"a film, said to be one");
	assert_eq!(library.question_for(personal, &small).expect("looks"), None);
	let question = library
		.question_for(personal, &large)
		.expect("looks")
		.expect("a question");
	assert_eq!(question.folder, resolved_path(&films).expect("resolves"));
	// As people write it, which on Windows is not the form the file system resolves to.
	assert!(
		!question.folder.to_string_lossy().starts_with(r"\\?\"),
		"{}",
		question.folder.display()
	);
	assert_eq!(question.size, 22);

	// Unanswered, the file goes into the store, and says so.
	let added = library
		.add_file(personal, &large, now(), &mut rng)
		.expect("adds");
	assert!(added.unasked);
	assert_eq!(added.kept, Kept::Store);

	// Answered, the folder gets a rule just before the one that asked, which asks nothing.
	library
		.answer(personal, &question, true, now(), &mut rng)
		.expect("answers");
	let rules = library.rules(personal).expect("reads");
	assert!(rules[0].conditions.folder.is_some());
	assert_eq!(rules[1].id, question.rule);
	let other = write(&films, "other.mkv", b"another film, also long");
	assert_eq!(library.question_for(personal, &other).expect("looks"), None);
	let added = library
		.add_file(personal, &other, now(), &mut rng)
		.expect("adds");
	assert!(!added.unasked);
	assert_eq!(added.kept, Kept::InPlace);
	let choice = library.policy_of(personal, added.file).expect("chooses");
	assert_eq!(choice.policy, Some(policy(&library, "Replaceable")));
}

#[test]
fn policies_can_be_added_and_changed() {
	let mut rng = TestRng::new("changed");
	let (_, mut library) = library("policies changed", &mut rng);
	let personal = library.personal_space();
	let films = library
		.add_policy(
			personal,
			"Films",
			Needs {
				copies: 0,
				away: 0,
				archived: 0,
				kept: Kept::InPlace,
			},
			now(),
			&mut rng,
		)
		.expect("adds");
	assert_eq!(
		policy(&library, "films"),
		films,
		"found without regard to case"
	);
	let found = |library: &Library| {
		library
			.policies(personal)
			.expect("reads")
			.into_iter()
			.find(|policy| policy.id == films)
			.expect("the policy")
	};
	assert_eq!(found(&library).needs.copies, 1, "at least one copy");
	library
		.edit_policy(
			personal,
			films,
			&PolicyEdit {
				name: Some("Films and series".to_owned()),
				copies: Some(2),
				kept: Some(Kept::Store),
				..PolicyEdit::default()
			},
			now(),
			&mut rng,
		)
		.expect("changes");
	let changed = found(&library);
	assert_eq!(changed.name.as_deref(), Some("Films and series"));
	assert_eq!(changed.needs.copies, 2);
	assert_eq!(changed.needs.kept, Kept::Store);
	assert!(matches!(
		library.add_policy(personal, " ", Needs::FALLBACK, now(), &mut rng),
		Err(NodeError::EmptyName)
	));
	assert!(matches!(
		library.find_policy(personal, "Nothing"),
		Err(NodeError::NoEntity(_))
	));
}

#[test]
fn only_the_policy_functions_write_policies() {
	let mut rng = TestRng::new("own");
	let (_, mut library) = library("policies own", &mut rng);
	let personal = library.personal_space();
	library
		.install_policies(personal, None, now(), &mut rng)
		.expect("installs");
	assert!(matches!(
		library.create_entity(personal, &[STORAGE_POLICY], Vec::new(), now(), &mut rng),
		Err(NodeError::OwnType(_))
	));
	let standard = policy(&library, "Standard");
	let note = library
		.create_entity(
			personal,
			&[],
			vec![(fields::NAME, Value::Text("A note".to_owned()))],
			now(),
			&mut rng,
		)
		.expect("adds");
	assert!(matches!(
		library.edit_entity(
			personal,
			note,
			vec![Edit::Set(POLICY, vec![Value::Reference(standard)])],
			&today(),
			now(),
			&mut rng,
		),
		Err(NodeError::OwnField(field)) if field == POLICY
	));
}

#[test]
fn a_space_with_a_policy_gets_no_defaults() {
	let mut rng = TestRng::new("imported");
	let (_, mut library) = library("policies imported", &mut rng);
	let personal = library.personal_space();
	// As an import of a space's policies would bring them.
	library
		.add_policy(personal, "Films", Needs::FALLBACK, now(), &mut rng)
		.expect("adds");
	library
		.install_policies(personal, None, now(), &mut rng)
		.expect("installs nothing");
	assert_eq!(names(&library), ["Films"]);
}

#[test]
fn policies_and_rules_are_not_edited_as_entities() {
	let mut rng = TestRng::new("not edited");
	let (_, mut library) = library("policies not edited", &mut rng);
	let personal = library.personal_space();
	library
		.install_policies(personal, None, now(), &mut rng)
		.expect("installs");
	let critical = policy(&library, "Critical");
	assert!(matches!(
		library.edit_entity(
			personal,
			critical,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		),
		Err(NodeError::OwnType(name)) if name == STORAGE_POLICY
	));
	library
		.remove_policy(personal, critical, now(), &mut rng)
		.expect("removes");
	assert_eq!(names(&library), ["Replaceable", "Standard"]);
}

#[cfg(unix)]
#[test]
fn an_unchanged_file_is_not_opened_again() {
	use std::os::unix::fs::PermissionsExt;

	let mut rng = TestRng::new("not opened");
	let (_, mut library) = library("policies not opened", &mut rng);
	let personal = library.personal_space();
	let files = folder("policies not opened");
	let note = write(&files, "note.txt", b"Milk, eggs, bread.\n");
	let added = library
		.add_file(personal, &note, now(), &mut rng)
		.expect("adds");
	fs::set_permissions(&note, fs::Permissions::from_mode(0o000)).expect("locks");
	// Only its size and time are looked at.
	let again = library.add_file(personal, &note, now(), &mut rng);
	fs::set_permissions(&note, fs::Permissions::from_mode(0o644)).expect("unlocks");
	let again = again.expect("known");
	assert_eq!(
		(again.file, again.known, again.origin),
		(added.file, true, None)
	);
}

#[test]
fn an_unchanged_file_is_not_asked_about_again() {
	let mut rng = TestRng::new("not asked again");
	let (_, mut library) = library("policies not asked again", &mut rng);
	let personal = library.personal_space();
	library
		.install_policies(personal, None, now(), &mut rng)
		.expect("installs");
	let asking = NewRule {
		asks_above: Some(10),
		..NewRule::new(policy(&library, "Standard"))
	};
	library
		.add_rule(personal, &asking, Place::First, now(), &mut rng)
		.expect("adds");
	let films = folder("policies not asked again");
	let film = write(&films, "film.mkv", b"a film, said to be one");
	assert!(
		library
			.question_for(personal, &film)
			.expect("looks")
			.is_some()
	);
	library
		.add_file(personal, &film, now(), &mut rng)
		.expect("adds");
	assert_eq!(library.question_for(personal, &film).expect("looks"), None);
}

#[test]
fn a_file_found_twice_unchanged_is_checked_once() {
	let mut rng = TestRng::new("checked once");
	let (_, mut library) = library("policies checked once", &mut rng);
	let personal = library.personal_space();
	let downloads = folder("policies checked once downloads");
	library
		.install_policies(personal, Some(&downloads), now(), &mut rng)
		.expect("installs");
	let inner = downloads.join("inner");
	fs::create_dir(&inner).expect("makes a folder");
	let first = write(&downloads, "film.mkv", b"a film, said to be one");
	let second = write(&inner, "film.mkv", b"a film, said to be one");
	library
		.add_file(personal, &first, now(), &mut rng)
		.expect("adds");
	library
		.add_file(personal, &second, now(), &mut rng)
		.expect("adds");
	let report = library.verify(now()).expect("verifies");
	assert_eq!(report.spaces[0].in_place, 1, "one file, in two places");
}
