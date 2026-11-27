//! Recovering a person's other spaces and groups with their recovery code and a backup: the
//! spaces of their own and those shared with them come back with their roles, the new device
//! lists itself in each by the code, or through a group for a space that the person reaches
//! through a group alone, and the lost devices are revoked there too, those that the backup's
//! personal space does not list as well; a space that the backup does not hold is named
//! ([technical design: recovery codes]).
//!
//! The people: Leo, whose library comes back, and Mona, who shares a space or a group with
//! him.
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use std::path::{Path, PathBuf};

use crate::common::devices::on_disk;
use crate::common::{
	invite_member, listed_there, now, own_key, pair_device, protection, sync_in_memory, this_day,
	user_library,
};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::RecoveryCode;
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{Joining, Library, MemberRole, NewBackup, Recovered};
use nibkeep_testkit::TestRng;

const NOTE: &str = "local.note";

fn field() -> FieldName {
	NOTE.parse().expect("a field name")
}

/// Syncs `a` and `b` back and forth `rounds` times, from `at` on; returns the time after.
fn sync_rounds(a: &mut Library, b: &mut Library, at: u64, rounds: u64) -> u64 {
	let mut at = at;
	for round in 0..rounds {
		at += 1_000;
		let (first, second) = sync_in_memory(a, b, at, (0, 1));
		first.unwrap_or_else(|error| panic!("round {round}: the first side syncs: {error}"));
		second.unwrap_or_else(|error| panic!("round {round}: the second side syncs: {error}"));
	}
	at
}

/// A note in `space`, written on `library`.
fn note(library: &mut Library, space: SpaceId, text: &str, at: u64, rng: &mut TestRng) -> EntityId {
	library
		.create_entity(
			space,
			&[],
			vec![(field(), Value::Text(text.to_owned()))],
			at,
			rng,
		)
		.expect("writes")
}

fn notes(library: &Library, space: SpaceId, entity: EntityId) -> Vec<Value> {
	library
		.entity(space, entity, this_day())
		.expect("reads")
		.map_or_default(|entity| {
			entity
				.values(&field())
				.iter()
				.map(|shown| shown.value.clone())
				.collect()
		})
}

/// Makes a code on a library: the person wrote it down and typed it back.
fn make_code(library: &mut Library, at: u64, rng: &mut TestRng) -> RecoveryCode {
	let code = RecoveryCode::generate(rng);
	library
		.add_recovery_code(&code, false, at, rng)
		.expect("names the code");
	code
}

/// A backup on `library` of the spaces `spaces`, every space of its own if none, updated
/// there.
fn backup(
	library: &mut Library,
	name: &str,
	spaces: Vec<SpaceId>,
	at: u64,
	rng: &mut TestRng,
) -> PathBuf {
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, backup"))
			.join("Backup");
	let backup = library
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				spaces,
				..NewBackup::default()
			},
			at,
			rng,
		)
		.expect("adds")
		.backup;
	library
		.update_backup(&backup, &folder, &|| at + 1, &|| false)
		.expect("updates")
		.expect("not held by another");
	folder
}

/// Recovers a library on a new computer from the backup in `folder` with `code`.
fn recover(
	name: &str,
	folder: &Path,
	code: &RecoveryCode,
	at: u64,
	rng: &mut TestRng,
) -> (Library, Recovered) {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, new"));
	on_disk(&directory, &format!("{name} new"));
	let joining = Joining::new("new laptop", "nib", rng).expect("names");
	Library::recover(
		directory.as_path(),
		(folder, code),
		joining,
		(&protection(), None),
		&|| at,
		rng,
	)
	.expect("recovers")
}

/// Whether, in `space`, this replica is listed for its member, and every other replica of
/// that member is revoked.
fn only_this_device_stands(library: &Library, space: SpaceId) -> bool {
	let members = library.members(space).expect("lists");
	let member = members
		.iter()
		.find(|member| member.this_user)
		.expect("this replica's member");
	member.replicas.iter().any(|replica| replica.this)
		&& member
			.replicas
			.iter()
			.all(|replica| replica.revoked != replica.this)
}

#[test]
fn a_library_comes_back_with_its_own_and_its_shared_spaces() {
	let mut rng = TestRng::new("own and shared spaces");
	let mut leo = user_library("Leo", "laptop", &mut rng);
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut at = now() + 100_000;
	let notes_space = leo.create_space("Notes", at, &mut rng).expect("makes it");
	let family = mona.create_space("Family", at, &mut rng).expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (family, MemberRole::Editor), &mut leo, at);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 1);
	let his = note(&mut leo, notes_space, "the boiler's code", at, &mut rng);
	let hers = note(&mut mona, family, "Gran's birthday", at, &mut rng);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 1);
	let code = make_code(&mut leo, at, &mut rng);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 2);
	let folder = backup(&mut leo, "own and shared spaces", Vec::new(), at, &mut rng);
	// Leo's laptop is lost.
	let (library, recovered) = recover(
		"own and shared spaces",
		&folder,
		&code,
		at + 10_000,
		&mut rng,
	);
	assert_eq!(recovered.missing, [] as [SpaceId; 0]);
	for space in [notes_space, family] {
		assert!(recovered.spaces.contains(&space), "{recovered:?}");
		assert!(only_this_device_stands(&library, space));
	}
	assert_eq!(
		library.own_role(notes_space).expect("reads"),
		MemberRole::Admin
	);
	assert_eq!(library.own_role(family).expect("reads"), MemberRole::Editor);
	assert_eq!(
		notes(&library, notes_space, his),
		[Value::Text("the boiler's code".to_owned())]
	);
	assert_eq!(
		notes(&library, family, hers),
		[Value::Text("Gran's birthday".to_owned())]
	);
	// Leo's new device introduces itself to Mona's in their first sync: she takes it into
	// the family, which is all they share, and each reads what the other writes there.
	let mut library = library;
	at += 20_000;
	let his_new = note(
		&mut library,
		family,
		"written on Leo's new laptop",
		at,
		&mut rng,
	);
	let (first, second) = sync_in_memory(&mut library, &mut mona, at + 1_000, (0, 1));
	first.expect("Leo's new device syncs");
	second.expect("Mona's syncs");
	assert_eq!(
		notes(&mona, family, his_new),
		[Value::Text("written on Leo's new laptop".to_owned())]
	);
	assert!(!mona.spaces().contains(&notes_space));
	let her_new = note(
		&mut mona,
		family,
		"written by Mona after",
		at + 2_000,
		&mut rng,
	);
	sync_rounds(&mut mona, &mut library, at + 3_000, 1);
	assert_eq!(
		notes(&library, family, her_new),
		[Value::Text("written by Mona after".to_owned())]
	);
}

#[test]
fn a_member_through_a_group_alone_comes_back_to_the_groups_spaces() {
	let mut rng = TestRng::new("through a group");
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut leo = user_library("Leo", "phone", &mut rng);
	let mut at = now() + 100_000;
	let household = mona
		.create_group("Household", at, &mut rng)
		.expect("makes it");
	let family = mona
		.create_space("Family", at + 100, &mut rng)
		.expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (household, MemberRole::Editor), &mut leo, at);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 1);
	mona.add_group(family, household, MemberRole::Editor, at + 1_000, &mut rng)
		.expect("adds the group");
	at = sync_rounds(&mut mona, &mut leo, at + 2_000, 4);
	assert!(leo.spaces().contains(&family));
	let hers = note(&mut mona, family, "Gran's birthday", at, &mut rng);
	let code = make_code(&mut leo, at, &mut rng);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 3);
	let folder = backup(&mut leo, "through a group", Vec::new(), at, &mut rng);
	let before = leo.space_key_numbers(family).expect("reads").1;
	let (library, recovered) = recover("through a group", &folder, &code, at + 10_000, &mut rng);
	assert_eq!(recovered.missing, [] as [SpaceId; 0]);
	assert!(recovered.spaces.contains(&household), "{recovered:?}");
	assert!(recovered.spaces.contains(&family), "{recovered:?}");
	// He is listed in the household by his code, and in the family through the household.
	assert!(only_this_device_stands(&library, household));
	assert_eq!(
		library.own_role(household).expect("reads"),
		MemberRole::Editor
	);
	assert_eq!(library.own_role(family).expect("reads"), MemberRole::Editor);
	// The family learns, by that anchor, that the household revoked his lost phone, and
	// writes with a key the phone never held.
	assert_ne!(library.space_key_numbers(family).expect("reads").1, before);
	assert_eq!(
		notes(&library, family, hers),
		[Value::Text("Gran's birthday".to_owned())]
	);
	// He introduces himself to Mona's device in the household, which listed him by his code;
	// the family, which reaches him through it, follows at the next sync.
	let mut library = library;
	at += 20_000;
	let his_new = note(
		&mut library,
		family,
		"written on Leo's new phone",
		at,
		&mut rng,
	);
	let (first, second) = sync_in_memory(&mut library, &mut mona, at + 1_000, (0, 1));
	first.expect("Leo's new device syncs");
	second.expect("Mona's syncs");
	sync_rounds(&mut library, &mut mona, at + 2_000, 2);
	assert_eq!(
		notes(&mona, family, his_new),
		[Value::Text("written on Leo's new phone".to_owned())]
	);
}

#[test]
fn a_member_removed_since_the_backup_does_not_come_back_by_his_code() {
	let mut rng = TestRng::new("removed since");
	let mut leo = user_library("Leo", "laptop", &mut rng);
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut at = now() + 100_000;
	let family = mona.create_space("Family", at, &mut rng).expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (family, MemberRole::Editor), &mut leo, at);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 1);
	let code = make_code(&mut leo, at, &mut rng);
	at = sync_rounds(&mut mona, &mut leo, at + 1_000, 2);
	let folder = backup(&mut leo, "removed since", Vec::new(), at, &mut rng);
	// Mona removes Leo from the family; then his laptop is lost.
	let his = mona
		.members(family)
		.expect("lists")
		.into_iter()
		.find(|member| !member.this_user)
		.expect("Leo")
		.record;
	at += 1_000;
	mona.remove_member(family, his, at, &mut rng)
		.expect("removes him");
	let (mut library, recovered) = recover("removed since", &folder, &code, at + 10_000, &mut rng);
	assert!(recovered.spaces.contains(&family), "{recovered:?}");
	// His code stands, but he has no role there any more: his new device introduces itself
	// to nobody.
	let (first, second) = sync_in_memory(&mut library, &mut mona, at + 20_000, (0, 1));
	assert!(
		matches!(
			second,
			Err(nibkeep_node::NodeError::Sync(
				nibkeep_sync::SyncError::NotAReplica
			))
		),
		"{second:?}"
	);
	assert!(first.is_err());
}

#[test]
fn a_space_that_the_backup_does_not_hold_is_named_and_left_out() {
	let mut rng = TestRng::new("a space not backed up");
	let mut leo = user_library("Leo", "laptop", &mut rng);
	let at = now() + 100_000;
	let notes_space = leo.create_space("Notes", at, &mut rng).expect("makes it");
	let personal = leo.personal_space();
	let code = make_code(&mut leo, at + 1_000, &mut rng);
	let folder = backup(
		&mut leo,
		"a space not backed up",
		vec![personal],
		at + 2_000,
		&mut rng,
	);
	let (library, recovered) = recover(
		"a space not backed up",
		&folder,
		&code,
		at + 10_000,
		&mut rng,
	);
	assert_eq!(recovered.missing, [notes_space]);
	assert_eq!(recovered.spaces, [] as [SpaceId; 0]);
	assert_eq!(library.spaces(), [personal]);
}

#[test]
fn a_device_that_only_a_shared_space_lists_is_revoked_there_unless_a_kept_device_listed_it() {
	let name = "listed in a space alone";
	let mut rng = TestRng::new(name);
	let mut laptop = user_library("Leo", "laptop", &mut rng);
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut at = now() + 100_000;
	let family = mona.create_space("Family", at, &mut rng).expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (family, MemberRole::Editor), &mut laptop, at);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	// Leo's desk, which the laptop lists in the family too, and his code.
	let mut desk = pair_device(&mut laptop, name, "desk", &mut rng);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 3);
	let (laptop_key, desk_key) = (own_key(&laptop), own_key(&desk));
	let code = make_code(&mut laptop, at, &mut rng);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 2);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	assert_eq!(listed_there(&laptop, family, &desk_key), Some(false));
	// Then a phone, which the laptop lists in the family. Mona learns of it from the laptop,
	// and the desk from Mona alone: its personal space does not list the phone.
	let mut phone = pair_device(&mut laptop, name, "phone", &mut rng);
	at = sync_rounds(&mut laptop, &mut phone, at + 1_000, 3);
	let phone_key = own_key(&phone);
	assert_eq!(listed_there(&laptop, family, &phone_key), Some(false));
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	at = sync_rounds(&mut mona, &mut desk, at + 1_000, 1);
	assert!(
		desk.replicas()
			.expect("lists")
			.iter()
			.all(|replica| replica.device_name.as_deref() != Some("phone"))
	);
	// The desk updates the backup, which then lists the phone in the family alone.
	let folder = backup(&mut desk, name, Vec::new(), at, &mut rng);
	let copy = folder.with_file_name("Backup, kept");
	nibkeep_testkit::copy_folder(&folder, &copy);
	// Every device is lost. The recovery revokes the phone in the family too, which nothing
	// else would: no device of Leo's is left to tell it of the phone.
	let (library, recovered) = recover(name, &folder, &code, at + 10_000, &mut rng);
	assert!(only_this_device_stands(&library, family));
	assert_eq!(listed_there(&library, family, &phone_key), Some(true));
	assert_eq!(recovered.revoked_elsewhere, [family]);
	// Had Leo kept the laptop, which listed the phone, the phone would stay too.
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, kept"));
	on_disk(&directory, &format!("{name} kept"));
	let joining = Joining::new("new laptop", "nib", &mut rng).expect("names");
	let at = at + 20_000;
	let mut recovering = Library::start_recovery(
		&directory,
		(&copy, &code),
		joining,
		&protection(),
		&|| at,
		&mut rng,
	)
	.expect("starts");
	let kept: Vec<EntityId> = recovering
		.others()
		.expect("lists")
		.into_iter()
		.filter(|replica| replica.device_name.as_deref() == Some("laptop"))
		.map(|replica| replica.id)
		.collect();
	assert_eq!(kept.len(), 1);
	recovering.keep(&kept).expect("keeps");
	let (library, recovered) = recovering.finish(None, &|| at, &mut rng).expect("recovers");
	assert_eq!(listed_there(&library, family, &laptop_key), Some(false));
	assert_eq!(listed_there(&library, family, &phone_key), Some(false));
	assert_eq!(listed_there(&library, family, &desk_key), Some(true));
	assert_eq!(recovered.revoked_elsewhere, [] as [SpaceId; 0]);
	drop(phone);
}
