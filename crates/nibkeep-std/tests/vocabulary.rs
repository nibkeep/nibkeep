//! The standard vocabulary: names under `std.`, links that follow the rules, and types that
//! a space can install as they are.

use nibkeep_model::{FieldName, Types, ValueKind, fields};
use nibkeep_testkit::TestRng;

/// The types that NibKeep defines in code, as a library knows them.
fn code_types() -> Types {
	let mut types = Types::new();
	for definition in nibkeep_governance::types() {
		types.add(definition);
	}
	types
}

#[test]
fn every_name_is_standard_but_the_name_of_entities() {
	for definition in nibkeep_std::types() {
		assert!(definition.name().as_str().starts_with("std."));
		for (name, field) in definition.fields() {
			assert!(
				name.as_str().starts_with("std.") || *name == fields::NAME,
				"{name}"
			);
			if let Some(reverse) = field.reverse() {
				assert!(reverse.as_str().starts_with("std."), "{reverse}");
			}
		}
	}
}

#[test]
fn only_links_have_reverse_names_or_stand_for_what_they_refer_to() {
	for definition in nibkeep_std::types() {
		for (name, field) in definition.fields() {
			if field.reverse().is_some()
				|| field.is_stands_for()
				|| field.is_search_through()
				|| field.is_role()
			{
				assert_eq!(field.kind(), ValueKind::Reference, "{name}");
			}
			// A link that is not a role names its other end; a role is shown with its
			// relationship instead.
			if field.kind() == ValueKind::Reference && !field.is_role() {
				assert!(field.reverse().is_some(), "{name} has no reverse name");
			}
		}
	}
	let standing: Vec<String> = nibkeep_std::types()
		.iter()
		.flat_map(nibkeep_model::TypeDef::fields)
		.filter(|(_, field)| field.is_stands_for())
		.map(|(name, _)| name.to_string())
		.collect();
	assert_eq!(standing, ["std.used-by"]);
	// A search for a person finds what they wrote or translated.
	let searched: Vec<String> = nibkeep_std::types()
		.iter()
		.flat_map(nibkeep_model::TypeDef::fields)
		.filter(|(_, field)| field.is_search_through())
		.map(|(name, _)| name.to_string())
		.collect::<std::collections::BTreeSet<_>>()
		.into_iter()
		.collect();
	assert_eq!(searched, ["std.author", "std.in-folder", "std.translator"]);
}

#[test]
fn a_space_installs_the_types_as_they_are() {
	let mut rng = TestRng::new("installs");
	let types = code_types();
	for definition in nibkeep_std::types() {
		assert_eq!(types.check(&definition), Ok(()), "{}", definition.name());
		let operations = types.define(&definition, &mut rng);
		assert!(operations.len() > 1, "{}", definition.name());
	}
	// A field that several types list is defined alike by each.
	let mut combined = code_types();
	for definition in nibkeep_std::types() {
		combined.add(definition);
	}
	for definition in nibkeep_std::types() {
		for (name, field) in definition.fields() {
			assert_eq!(combined.field(name).as_ref(), Some(field), "{name}");
		}
	}
}

#[test]
fn every_type_and_field_has_a_label_in_english() {
	let english = nibkeep_model::Tag::parse("en").expect("a tag");
	for definition in nibkeep_std::types() {
		assert!(
			definition.labels().get(Some(&english)).is_some(),
			"{} has no label",
			definition.name()
		);
		for (name, field) in definition.fields() {
			assert!(
				field.labels().get(Some(&english)).is_some(),
				"{name} has no label"
			);
		}
	}
	// The name of entities is NibKeep's own, and labelled by NibKeep.
	let name = code_types().field(&fields::NAME).expect("defined");
	assert_eq!(name.labels().get(Some(&english)), Some("Name"));
}

#[test]
fn a_login_keeps_what_signs_in_secret_and_what_finds_it_searchable() {
	let login = nibkeep_std::types()
		.into_iter()
		.find(|definition| *definition.name() == nibkeep_std::LOGIN)
		.expect("a login type");
	let field = |name| login.field(&name).cloned().expect("listed");
	for secret in [
		nibkeep_std::PASSWORD,
		nibkeep_std::ONE_TIME_CODE,
		nibkeep_std::RECOVERY_CODE,
		nibkeep_std::SECRET_NOTES,
	] {
		assert!(field(secret.clone()).is_secret(), "{secret}");
	}
	for searchable in [fields::NAME, nibkeep_std::WEBSITE, nibkeep_std::USERNAME] {
		assert!(!field(searchable.clone()).is_secret(), "{searchable}");
	}
	assert!(field(nibkeep_std::WEBSITE).is_code());
	assert_eq!(
		field(nibkeep_std::WEBSITE).multiplicity(),
		nibkeep_model::Multiplicity::Many
	);
	assert!(field(nibkeep_std::USERNAME).is_code());
	// A password has one value: two set at once compete until a person chooses.
	assert_eq!(
		field(nibkeep_std::PASSWORD).multiplicity(),
		nibkeep_model::Multiplicity::One
	);
	// Nothing else in the vocabulary is secret.
	let secret: Vec<String> = nibkeep_std::types()
		.iter()
		.flat_map(nibkeep_model::TypeDef::fields)
		.filter(|(_, field)| field.is_secret())
		.map(|(name, _)| name.to_string())
		.collect::<std::collections::BTreeSet<_>>()
		.into_iter()
		.collect();
	assert_eq!(
		secret,
		[
			"std.one-time-code",
			"std.password",
			"std.recovery-code",
			"std.secret-content",
			"std.secret-notes"
		]
	);
}

#[test]
fn a_secret_file_keeps_its_content_secret_and_what_finds_it_searchable() {
	let types = nibkeep_std::types();
	let secret_file = types
		.iter()
		.find(|definition| *definition.name() == nibkeep_std::SECRET_FILE)
		.expect("a type of secret files");
	let field = |name| secret_file.field(&name).cloned().expect("listed");
	let content = field(nibkeep_std::SECRET_CONTENT);
	assert!(content.is_secret());
	assert_eq!(content.kind(), ValueKind::Blob);
	assert_eq!(content.multiplicity(), nibkeep_model::Multiplicity::One);
	for searchable in [fields::NAME, nibkeep_std::MEDIA_TYPE, nibkeep_std::SIZE] {
		assert!(!field(searchable.clone()).is_secret(), "{searchable}");
	}
	let attached = field(nibkeep_std::ATTACHED_TO);
	assert_eq!(attached.kind(), ValueKind::Reference);
	assert_eq!(attached.multiplicity(), nibkeep_model::Multiplicity::Many);
	assert_eq!(
		attached.reverse().map(FieldName::as_str),
		Some("std.attachments")
	);
	// A search for a record does not find what is attached to it.
	assert!(!attached.is_search_through());
	// A secret file is no file: storage policies, origins and checkouts are not for it.
	assert!(secret_file.field(&nibkeep_std::CONTENT).is_none());
}

#[test]
fn a_secret_file_can_be_a_tree_s_with_its_path_in_the_open() {
	let types = nibkeep_std::types();
	let secret_file = types
		.iter()
		.find(|definition| *definition.name() == nibkeep_std::SECRET_FILE)
		.expect("a type of secret files");
	assert!(
		secret_file.version() >= 2,
		"the fields of trees came in version 2"
	);
	let field = |name| secret_file.field(&name).cloned().expect("listed");
	let in_tree = field(nibkeep_std::IN_TREE);
	assert_eq!(in_tree.kind(), ValueKind::Reference);
	assert_eq!(
		in_tree.reverse().map(FieldName::as_str),
		Some("std.tree-contents")
	);
	let path = field(nibkeep_std::TREE_PATH);
	assert!(path.is_code());
	assert_eq!(field(nibkeep_std::EXECUTABLE).kind(), ValueKind::Boolean);
	assert_eq!(
		field(nibkeep_std::COMMITTED_IN).kind(),
		ValueKind::Reference
	);
	// Where it is and when it changed are ordinary; only its content is secret.
	for open in [
		nibkeep_std::IN_TREE,
		nibkeep_std::TREE_PATH,
		nibkeep_std::EXECUTABLE,
		nibkeep_std::COMMITTED_IN,
	] {
		assert!(!field(open.clone()).is_secret(), "{open}");
	}
}

#[test]
fn folders_hold_entities_and_other_folders() {
	let types = nibkeep_std::types();
	let of = |name| {
		types
			.iter()
			.find(|definition| *definition.name() == name)
			.expect("defined")
	};
	for holder in [
		nibkeep_std::LOGIN,
		nibkeep_std::FOLDER,
		nibkeep_std::SECRET_FILE,
	] {
		let in_folder = of(holder.clone())
			.field(&nibkeep_std::IN_FOLDER)
			.expect("in folders");
		assert_eq!(in_folder.kind(), ValueKind::Reference);
		assert_eq!(in_folder.multiplicity(), nibkeep_model::Multiplicity::Many);
		// A search for a folder finds what is in it.
		assert!(in_folder.is_search_through());
	}
	let folder = of(nibkeep_std::FOLDER);
	assert!(folder.field(&fields::NAME).is_some());
	assert!(folder.field(&nibkeep_std::DESCRIPTION).is_some());
}

#[test]
fn a_tree_holds_files_and_links_by_their_paths_and_revisions_by_their_messages() {
	let types = nibkeep_std::types();
	let of = |name| {
		types
			.iter()
			.find(|definition| *definition.name() == name)
			.expect("defined")
	};
	assert!(of(nibkeep_std::TREE).field(&fields::NAME).is_some());
	// A file of a tree is a file, with its tree, its path and its executable bit, so that
	// it keeps its versions as any file does; a link has its target as text instead.
	let file = of(nibkeep_std::FILE);
	let link = of(nibkeep_std::SYMBOLIC_LINK);
	for holder in [file, link] {
		let in_tree = holder.field(&nibkeep_std::IN_TREE).expect("in a tree");
		assert_eq!(in_tree.kind(), ValueKind::Reference);
		assert_eq!(in_tree.multiplicity(), nibkeep_model::Multiplicity::One);
		assert_eq!(
			in_tree.reverse().map(FieldName::as_str),
			Some("std.tree-contents")
		);
		let path = holder.field(&nibkeep_std::TREE_PATH).expect("a path");
		assert!(path.is_code(), "a path is in no language");
		assert_eq!(path.multiplicity(), nibkeep_model::Multiplicity::One);
	}
	assert!(file.version() >= 2, "the fields of trees came in version 2");
	assert_eq!(
		file.field(&nibkeep_std::EXECUTABLE)
			.map(nibkeep_model::FieldDef::kind),
		Some(ValueKind::Boolean)
	);
	assert!(
		link.field(&nibkeep_std::LINK_TARGET)
			.is_some_and(nibkeep_model::FieldDef::is_code)
	);
	assert!(link.field(&nibkeep_std::CONTENT).is_none());
	// A root says where the tree's folder is on a device.
	let root = of(nibkeep_std::TREE_ROOT);
	for (field, reverse) in [
		(nibkeep_std::ROOT_OF, "std.roots"),
		(nibkeep_std::ON_DEVICE, "std.roots-here"),
	] {
		let link = root.field(&field).expect("a link");
		assert_eq!(link.reverse().map(FieldName::as_str), Some(reverse));
	}
	assert!(
		root.field(&nibkeep_std::PATH)
			.is_some_and(nibkeep_model::FieldDef::is_code)
	);
	// A revision's message is text, which a search finds; nothing of a tree is secret.
	let revision = of(nibkeep_std::REVISION);
	let message = revision.field(&nibkeep_std::MESSAGE).expect("a message");
	assert_eq!(message.kind(), ValueKind::Text);
	assert!(!message.is_code());
	assert_eq!(
		revision
			.field(&nibkeep_std::PARTS)
			.map(nibkeep_model::FieldDef::kind),
		Some(ValueKind::Integer)
	);
	// Each part of a commit names its revision on the entity it records.
	for recorded in [
		nibkeep_std::TREE,
		nibkeep_std::FILE,
		nibkeep_std::SYMBOLIC_LINK,
		nibkeep_std::GIT_REPOSITORY,
	] {
		let committed_in = of(recorded)
			.field(&nibkeep_std::COMMITTED_IN)
			.expect("the commit that recorded it");
		assert_eq!(committed_in.kind(), ValueKind::Reference);
		assert_eq!(
			committed_in.reverse().map(FieldName::as_str),
			Some("std.recorded")
		);
	}
	for tree_type in [
		nibkeep_std::TREE,
		nibkeep_std::TREE_ROOT,
		nibkeep_std::REVISION,
	] {
		assert!(of(tree_type).fields().all(|(_, field)| !field.is_secret()));
	}
}

/// People are the standard vocabulary's, whose type NibKeep names for each user.
#[test]
fn nibkeep_names_people_by_the_vocabularys_type() {
	assert_eq!(nibkeep_governance::PERSON, nibkeep_std::PERSON);
	assert!(
		nibkeep_std::types()
			.iter()
			.any(|definition| *definition.name() == nibkeep_std::PERSON)
	);
}
