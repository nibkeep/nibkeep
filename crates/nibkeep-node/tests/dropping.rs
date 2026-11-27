//! Dropping a space ([technical design: devices that choose their spaces]): a device drops a
//! space only once its own changes there are on other disks, as many disks as the space's
//! Critical policy asks for hold all of the space's changes, the copies elsewhere of the
//! blobs it holds are proven, and another device that remains holds the space's secrets,
//! where it holds them. A drop that would leave the space below its policies, or on no other
//! device of the person's, is refused, and says what is missing; so is one while a tree's
//! copy, or an edited checkout, of the space is on the device. The device then states its
//! choice, and once a device of the person's that holds the space holds it, deletes the
//! space's files, its keys, and the blobs that no other space of the replica keeps. A space
//! that the person left is deleted at once.
//!
//! The people: Ada, with a NAS that is an archive node, a desk and a laptop, each on a disk
//! of its own, where the laptop made Project; and, for a space that she leaves, Bea.
//!
//! [technical design: devices that choose their spaces]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces

use crate::common;

use std::fs;
use std::path::PathBuf;

use crate::common::devices::{Devices, blob_files, bytes};
use crate::common::{invite_member, now, own_record, replica_folder, sync_rounds};
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{Name, TypeName, Value};
use nibkeep_node::{
	DropRefusal, DropStage, DropWait, Dropping, Library, MemberRole, NewProtection, NodeError,
	PolicyEdit, Taking, Unlocker,
};

/// How long a replica waits before it issues its account again while its state stays the
/// same, in milliseconds, and a little more.
const ACCOUNTS_AGAIN: u64 = 61_000;

const NAS: usize = 0;
const DESK: usize = 1;
const LAPTOP: usize = 2;

/// Ada's NAS, an archive node, her desk and her laptop, each on a disk of its own, which all
/// hold Project, which the laptop made with a file, `plan.bin`, that every device holds; and
/// their accounts say so.
struct Ada {
	devices: Devices,
	project: SpaceId,
	plan: (EntityId, Hash),
}

impl Ada {
	fn new(name: &str) -> Self {
		let mut devices = Devices::new(name, true);
		devices.pair("desk", None);
		devices.pair("laptop", None);
		devices.record_endpoints();
		let at = devices.tick();
		let project = devices.libraries[LAPTOP]
			.create_space("Project", at, &mut devices.rng)
			.expect("makes it");
		let plan = add_file(&mut devices, LAPTOP, project, "plan.bin", &bytes(3, 30_000));
		let mut ada = Self {
			devices,
			project,
			plan,
		};
		// The desk takes Project through the NAS, and then the plan.
		ada.sync_all();
		ada.sync_all();
		for device in [NAS, DESK, LAPTOP] {
			assert!(ada.holds(device), "device {device} holds Project");
			assert!(
				ada.holds_content(device, &ada.plan.1),
				"device {device} holds the plan"
			);
		}
		ada
	}

	/// The desk and the laptop each sync with the NAS, three times, the clock moving on
	/// between, so that their accounts say what each holds.
	fn sync_all(&mut self) {
		for _ in 0..3 {
			for device in [DESK, LAPTOP] {
				self.devices.sync(device, NAS);
			}
			self.devices.advance(ACCOUNTS_AGAIN);
		}
	}

	/// Asks of Project's Critical policy two copies, none away from home, which the NAS and
	/// the desk meet: as installed, it asks for three, one away.
	fn lower_critical(&mut self) {
		let at = self.devices.tick();
		let project = self.project;
		let laptop = &mut self.devices.libraries[LAPTOP];
		let critical = laptop.find_policy(project, "Critical").expect("finds it");
		laptop
			.edit_policy(
				project,
				critical,
				&PolicyEdit {
					copies: Some(2),
					away: Some(0),
					..PolicyEdit::default()
				},
				at,
				&mut self.devices.rng,
			)
			.expect("edits it");
		self.sync_all();
	}

	/// Adds a note named `name` to Project on `device`.
	fn note(&mut self, device: usize, name: &str) {
		let at = self.devices.tick();
		self.devices.libraries[device]
			.create_entity(
				self.project,
				&[TypeName::from_static("std.person")],
				vec![(
					"org.nibkeep.name".parse().expect("a field"),
					Value::Name(Name::new(name)),
				)],
				at,
				&mut self.devices.rng,
			)
			.expect("adds");
	}

	fn drop_on(&mut self, device: usize) -> Result<Dropping, NodeError> {
		let at = self.devices.tick();
		self.devices.libraries[device].drop_space(self.project, at, &mut self.devices.rng)
	}

	/// What a drop of Project on `device` waits for, if it drops it.
	fn dropping(&self, device: usize) -> Option<Dropping> {
		self.devices.libraries[device]
			.drop_progress(self.project)
			.expect("reads")
	}

	fn holds(&self, device: usize) -> bool {
		self.devices.libraries[device]
			.spaces()
			.contains(&self.project)
	}

	/// Whether `device` holds `content` sealed, in Project.
	fn holds_content(&self, device: usize, content: &Hash) -> bool {
		self.devices.libraries[device]
			.file_content(self.project, content, u64::MAX)
			.expect("reads")
			.is_some()
	}

	/// Project's folder on `device`.
	fn folder(&self, device: usize) -> PathBuf {
		replica_folder(&self.devices.folders[device])
			.join("spaces")
			.join(self.project.to_string())
	}

	/// The blob files of `device`.
	fn blobs(&self, device: usize) -> Vec<PathBuf> {
		blob_files(&replica_folder(&self.devices.folders[device]).join("blobs"))
	}
}

/// Adds a file of `bytes` to `space` on `device`; returns it and its content.
fn add_file(
	devices: &mut Devices,
	device: usize,
	space: SpaceId,
	name: &str,
	bytes: &[u8],
) -> (EntityId, Hash) {
	let folder = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		&common::unique_folder(&format!("dropping files {device} {name}")),
	);
	let path = folder.join(name);
	fs::write(&path, bytes).expect("writes");
	let at = devices.tick();
	let added = devices.libraries[device]
		.add_file(space, &path, at, &mut devices.rng)
		.expect("adds");
	(added.file, added.content)
}

/// Takes Project back on the laptop, which deleted it: once approved on the desk, it fills
/// again as it syncs.
fn take_back_after_deletion(ada: &mut Ada) {
	let project = ada.project;
	let at = ada.devices.tick();
	assert_eq!(
		ada.devices.libraries[LAPTOP]
			.take_space(project, at, &mut ada.devices.rng)
			.expect("asks"),
		Taking::Asked
	);
	ada.sync_all();
	let laptop = own_record(&ada.devices.libraries[LAPTOP]);
	let at = ada.devices.tick();
	ada.devices.libraries[DESK]
		.allow_space(project, laptop, at, &mut ada.devices.rng)
		.expect("allows");
	for _ in 0..2 {
		ada.devices.sync(LAPTOP, DESK);
		ada.sync_all();
	}
	assert!(ada.holds(LAPTOP));
	let found = ada.devices.libraries[LAPTOP]
		.search(project, "budget")
		.expect("searches");
	assert_eq!(found.len(), 1);
}

/// The refusals of a drop that is refused.
fn refusals(result: Result<Dropping, NodeError>) -> Vec<DropRefusal> {
	match result {
		Err(NodeError::DropRefused(refused)) => refused.0,
		other => panic!("refused, not {other:?}"),
	}
}

#[test]
fn a_drop_waits_for_the_changes_and_the_proven_copies_then_deletes_once_another_device_holds_it() {
	let mut ada = Ada::new("dropping, waits and deletes");
	ada.lower_critical();
	let project = ada.project;
	// A file that the personal space holds too, whose blob stays; and a checkout of the plan,
	// unedited, which goes with the space.
	let personal = ada.devices.personal();
	let shared = bytes(5, 20_000);
	let (_, shared_content) = add_file(&mut ada.devices, LAPTOP, personal, "copy.bin", &shared);
	add_file(&mut ada.devices, LAPTOP, project, "shared.bin", &shared);
	ada.sync_all();
	let place = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "dropping, checkout");
	let at = ada.devices.tick();
	let checkout = ada.devices.libraries[LAPTOP]
		.checkout(project, ada.plan.0, &place, at, &mut ada.devices.rng)
		.expect("checks it out")
		.path;
	ada.sync_all();

	// The laptop's newest change is its own: the drop waits for it to reach other disks,
	// and for the plan's copies elsewhere to be proven.
	ada.note(LAPTOP, "Budget");
	let dropping = ada.drop_on(LAPTOP).expect("drops");
	assert_eq!(dropping.stage, DropStage::Waiting);
	assert!(
		dropping
			.waits
			.iter()
			.any(|wait| matches!(wait, DropWait::OwnChanges { changes, .. } if *changes > 0)),
		"{dropping:?}"
	);
	assert!(
		dropping
			.waits
			.iter()
			.any(|wait| matches!(wait, DropWait::ChangeCopies { disks: 0, .. })),
		"{dropping:?}"
	);
	assert!(dropping.waits.contains(&DropWait::Files(1)), "{dropping:?}");
	assert!(ada.holds(LAPTOP));
	assert_eq!(ada.dropping(LAPTOP), Some(dropping));

	// Once the others hold its changes, the plan's blob alone waits: the shared one stays, as
	// the personal space keeps it.
	ada.sync_all();
	let dropping = ada.dropping(LAPTOP).expect("dropping");
	assert_eq!(dropping.waits, [DropWait::Files(1)], "{dropping:?}");
	assert_eq!(dropping.stage, DropStage::Waiting);

	// A pass of removals proves the others' copies and removes the laptop's; the choice is
	// then stated, and nothing is deleted until a device that holds the space holds it.
	assert_eq!(ada.blobs(LAPTOP).len(), 2, "{:?}", ada.blobs(LAPTOP));
	let removed = ada.devices.remove(LAPTOP);
	assert_eq!(removed.copies, 1, "{removed:?}");
	assert_eq!(ada.blobs(LAPTOP).len(), 1, "{:?}", ada.blobs(LAPTOP));
	let dropping = ada.dropping(LAPTOP).expect("dropping");
	assert_eq!(dropping.stage, DropStage::Dropped, "{dropping:?}");
	assert_eq!(dropping.waits, []);
	assert!(ada.holds(LAPTOP));
	assert!(ada.folder(LAPTOP).exists());
	// Asked again, it says so.
	assert_eq!(
		ada.drop_on(LAPTOP).expect("tells").stage,
		DropStage::Dropped
	);

	ada.sync_all();
	assert!(!ada.holds(LAPTOP));
	assert!(!ada.folder(LAPTOP).exists());
	assert_eq!(ada.dropping(LAPTOP), None);
	assert!(!checkout.exists());
	// The blob that the personal space keeps stays, and opens there.
	assert_eq!(
		ada.devices.libraries[LAPTOP]
			.file_content(personal, &shared_content, u64::MAX)
			.expect("reads"),
		Some(shared)
	);
	assert_eq!(ada.blobs(LAPTOP).len(), 1, "{:?}", ada.blobs(LAPTOP));
	// The laptop is ended there, and knows the space as dropped; the others hold all of it.
	let laptop_key = common::own_key(&ada.devices.libraries[LAPTOP]);
	let dropped = ada.devices.libraries[DESK]
		.members(project)
		.expect("lists")
		.into_iter()
		.flat_map(|member| member.replicas)
		.find(|replica| replica.signing == laptop_key)
		.map(|replica| replica.dropped);
	assert_eq!(dropped, Some(true));
	let known = ada.devices.libraries[LAPTOP]
		.known_spaces()
		.expect("lists")
		.into_iter()
		.find(|known| known.space == project)
		.expect("known");
	assert_eq!(known.here, nibkeep_node::Here::Declined);
	for device in [NAS, DESK] {
		assert!(ada.holds_content(device, &ada.plan.1));
	}
	let found = ada.devices.libraries[DESK]
		.search(project, "budget")
		.expect("searches");
	assert_eq!(found.len(), 1);

	take_back_after_deletion(&mut ada);
}

#[test]
fn a_drop_is_refused_while_no_other_device_of_the_persons_holds_the_space() {
	let mut ada = Ada::new("dropping, the only device");
	ada.lower_critical();
	// The NAS and the desk ask before they take a space made later; the laptop makes Notes,
	// which only it holds.
	for device in [NAS, DESK] {
		let at = ada.devices.tick();
		ada.devices.libraries[device]
			.set_takes_new_spaces(false, at, &mut ada.devices.rng)
			.expect("says so");
	}
	ada.sync_all();
	let at = ada.devices.tick();
	let notes = ada.devices.libraries[LAPTOP]
		.create_space("Notes", at, &mut ada.devices.rng)
		.expect("makes it");
	ada.sync_all();
	let at = ada.devices.tick();
	let refused =
		refusals(ada.devices.libraries[LAPTOP].drop_space(notes, at, &mut ada.devices.rng));
	assert!(refused.contains(&DropRefusal::OnlyDevice), "{refused:?}");
	assert!(ada.devices.libraries[LAPTOP].spaces().contains(&notes));
	assert_eq!(
		ada.devices.libraries[LAPTOP]
			.drop_progress(notes)
			.expect("reads"),
		None
	);
}

#[test]
fn a_drop_is_refused_below_the_policies_and_says_what_is_missing() {
	let mut ada = Ada::new("dropping, below the policies");
	// As installed, Critical asks for three disks, one away: the NAS and the desk are two,
	// at home.
	let refused = refusals(ada.drop_on(LAPTOP));
	assert!(
		refused.iter().any(|refusal| matches!(
			refusal,
			DropRefusal::ChangeCopies {
				disks: 2,
				away: 0,
				needs,
				..
			} if needs.copies == 3 && needs.away == 1
		)),
		"{refused:?}"
	);
	ada.lower_critical();
	// A file that only the laptop holds would lack its copies.
	let project = ada.project;
	let (late, _) = add_file(
		&mut ada.devices,
		LAPTOP,
		project,
		"late.bin",
		&bytes(9, 10_000),
	);
	let refused = refusals(ada.drop_on(LAPTOP));
	assert!(
		refused.iter().any(|refusal| matches!(
			refusal,
			DropRefusal::Files { files, .. } if files == &[late]
		)),
		"{refused:?}"
	);
	assert_eq!(ada.dropping(LAPTOP), None);
	// Once the others hold it, the drop goes on.
	ada.sync_all();
	assert!(ada.drop_on(LAPTOP).is_ok());
}

#[test]
fn a_drop_waits_while_this_device_alone_holds_the_spaces_secrets() {
	let mut ada = Ada::new("dropping, secrets");
	ada.lower_critical();
	let protection = |passphrase| {
		NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
	};
	// The laptop sets up the secrets, and saves the first secret of Project, whose secrets
	// it then holds alone.
	let project = ada.project;
	let at = ada.devices.tick();
	let laptop_library = &mut ada.devices.libraries[LAPTOP];
	laptop_library
		.set_up_secrets(&protection("laptop secrets"), at, &mut ada.devices.rng)
		.expect("sets up");
	laptop_library
		.define_type(
			project,
			&common::login_type(),
			at + 100,
			&mut ada.devices.rng,
		)
		.expect("defines");
	laptop_library
		.create_entity(
			project,
			&[common::LOGIN],
			vec![
				(
					"local.title".parse().expect("a field"),
					Value::Text("Signing".to_owned()),
				),
				(
					"local.password".parse().expect("a field"),
					Value::Text("first".to_owned()),
				),
			],
			at + 200,
			&mut ada.devices.rng,
		)
		.expect("creates");
	ada.sync_all();
	assert_ne!(
		ada.devices.libraries[LAPTOP]
			.secrets_key_numbers(project)
			.expect("reads")
			.0,
		Vec::<u64>::new()
	);
	let dropping = ada.drop_on(LAPTOP).expect("drops");
	assert!(dropping.waits.contains(&DropWait::Secrets), "{dropping:?}");

	// The desk asks for the secrets, and the laptop gives them.
	let laptop = own_record(&ada.devices.libraries[LAPTOP]);
	let desk = own_record(&ada.devices.libraries[DESK]);
	let at = ada.devices.tick();
	ada.devices.libraries[DESK]
		.ask_for_secrets(
			laptop,
			&protection("desk secrets"),
			at,
			&mut ada.devices.rng,
		)
		.expect("asks");
	ada.devices.sync(DESK, LAPTOP);
	let at = ada.devices.tick();
	ada.devices.libraries[LAPTOP]
		.give_secrets(desk, at, &mut ada.devices.rng)
		.expect("gives");
	for _ in 0..3 {
		ada.devices.sync(DESK, LAPTOP);
		ada.sync_all();
	}
	let dropping = ada.dropping(LAPTOP);
	assert!(
		dropping
			.as_ref()
			.is_none_or(|dropping| !dropping.waits.contains(&DropWait::Secrets)),
		"{dropping:?}"
	);
}

#[test]
fn a_drop_is_refused_while_a_tree_or_an_edited_checkout_of_the_space_is_here() {
	let mut ada = Ada::new("dropping, trees and checkouts");
	ada.lower_critical();
	let project = ada.project;
	let place = nibkeep_testkit::empty_directory(
		env!("CARGO_TARGET_TMPDIR"),
		"dropping, an edited checkout",
	);
	let at = ada.devices.tick();
	let checkout = ada.devices.libraries[LAPTOP]
		.checkout(project, ada.plan.0, &place, at, &mut ada.devices.rng)
		.expect("checks it out")
		.path;
	fs::write(&checkout, b"edited since").expect("edits");
	let refused = refusals(ada.drop_on(LAPTOP));
	assert!(
		refused.contains(&DropRefusal::EditedCheckout(checkout.clone())),
		"{refused:?}"
	);
	fs::remove_file(&checkout).expect("deletes");

	let folder = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "dropping, a tree");
	fs::write(folder.join("notes.txt"), b"a tree's file").expect("writes");
	let at = ada.devices.tick();
	ada.devices.libraries[LAPTOP]
		.start_tree(project, &folder, None, at, &mut ada.devices.rng)
		.expect("starts it");
	let refused = refusals(ada.drop_on(LAPTOP));
	assert!(
		refused.iter().any(
			|refusal| matches!(refusal, DropRefusal::Tree(path) if path.ends_with("dropping, a tree"))
		),
		"{refused:?}"
	);
	assert!(ada.holds(LAPTOP));
}

#[test]
fn taking_a_space_back_before_its_choice_ends_the_drop() {
	let mut ada = Ada::new("dropping, taken back");
	ada.lower_critical();
	let dropping = ada.drop_on(LAPTOP).expect("drops");
	assert_eq!(dropping.waits, [DropWait::Files(1)], "{dropping:?}");
	let at = ada.devices.tick();
	assert_eq!(
		ada.devices.libraries[LAPTOP]
			.take_space(ada.project, at, &mut ada.devices.rng)
			.expect("takes it back"),
		Taking::Took
	);
	assert_eq!(ada.dropping(LAPTOP), None);
	// A pass removes nothing now, and the plan stays.
	let removed = ada.devices.remove(LAPTOP);
	assert_eq!(removed.copies, 0, "{removed:?}");
	assert!(ada.holds_content(LAPTOP, &ada.plan.1));
	ada.sync_all();
	assert!(ada.holds(LAPTOP));
}

#[test]
fn a_space_the_person_left_is_deleted_at_once() {
	let mut rng = nibkeep_testkit::TestRng::new("dropping, left");
	let mut ada = common::user_library("Ada", "desktop", &mut rng);
	let at = now() + 1_000;
	let family = ada.create_space("Family", at, &mut rng).expect("makes it");
	let mut bea = common::user_library("Bea", "phone", &mut rng);
	invite_member(&mut ada, (family, MemberRole::Editor), &mut bea, at + 2_000);
	let at = sync_rounds(&mut bea, &mut ada, at + 3_000, 2);
	assert!(bea.spaces().contains(&family));
	bea.leave(family, at, &mut rng).expect("leaves");
	let at = sync_rounds(&mut bea, &mut ada, at + 1_000, 1);
	// Bea's phone is the only device of hers, and holds the space read only: it goes at once.
	let dropping = bea.drop_space(family, at, &mut rng).expect("drops");
	assert_eq!(dropping.stage, DropStage::Deleted, "{dropping:?}");
	assert!(!bea.spaces().contains(&family));
	let folder = replica_folder(bea.library_directory().expect("a folder"))
		.join("spaces")
		.join(family.to_string());
	assert!(!folder.exists());
	// Ada's desktop holds it still; Bea's phone, sharing no space with it now, syncs no more.
	assert!(ada.spaces().contains(&family));
}

#[test]
fn a_deletion_cut_short_after_the_spaces_key_went_is_finished_as_the_library_opens() {
	let mut ada = Ada::new("dropping, cut short");
	ada.lower_critical();
	let removed = ada.devices.remove(LAPTOP);
	assert_eq!(removed.copies, 0, "{removed:?}");
	ada.drop_on(LAPTOP).expect("drops");
	ada.devices.remove(LAPTOP);
	ada.sync_all();
	assert!(!ada.holds(LAPTOP));
	// As a power cut leaves it once the key file no longer holds the space's key: its folder,
	// with the mark of the drop and what the deletion had not reached yet.
	let folder = ada.folder(LAPTOP);
	fs::create_dir_all(&folder).expect("makes it");
	fs::write(folder.join("dropping"), b"").expect("marks it");
	fs::write(folder.join("log.sqlite"), b"what was left").expect("writes");
	let library_folder = ada.devices.folders[LAPTOP].clone();
	let closed = common::user_library("Nobody", "nothing", &mut ada.devices.rng);
	drop(std::mem::replace(
		&mut ada.devices.libraries[LAPTOP],
		closed,
	));
	let reopened =
		Library::open(&library_folder, &Unlocker::Passphrase("battery staple")).expect("opens");
	assert!(!folder.exists());
	assert!(!reopened.spaces().contains(&ada.project));
}

#[test]
fn a_device_that_turned_against_its_person_cannot_have_another_drop_a_space() {
	let mut ada = Ada::new("dropping, an infected desk");
	ada.lower_critical();
	// The desk, turned against Ada, states on the laptop's record a choice not to hold
	// Project, signed by its own key, as it cannot sign by the laptop's.
	let personal = ada.devices.personal();
	let laptop = own_record(&ada.devices.libraries[LAPTOP]);
	let unlocker = Unlocker::Passphrase("battery staple");
	let desk_folder = ada.devices.folders[DESK].clone();
	let (desk_keys, _) = common::replica_keys(&desk_folder, &unlocker, personal);
	let choice = nibkeep_governance::Choice::sign(&desk_keys.signing, ada.project, 99, false);
	let operation = nibkeep_model::Operation::assert(nibkeep_model::Statement::new(
		ada.devices.rng.statement_id(),
		laptop,
		nibkeep_model::fields::SPACE_CHOICE,
		Value::Text(choice.to_text()),
	));
	let closed = common::user_library("Nobody", "nothing", &mut ada.devices.rng);
	drop(std::mem::replace(&mut ada.devices.libraries[DESK], closed));
	let at = ada.devices.tick();
	common::forge(
		&desk_folder,
		&unlocker,
		(personal, vec![operation]),
		(None, at),
		&mut ada.devices.rng,
	);
	ada.devices.libraries[DESK] = Library::open(&desk_folder, &unlocker).expect("opens");
	ada.sync_all();
	// The laptop holds Project still, whole, and drops nothing in a pass.
	assert!(ada.holds(LAPTOP));
	assert!(ada.folder(LAPTOP).exists());
	assert_eq!(ada.dropping(LAPTOP), None);
	let removed = ada.devices.remove(LAPTOP);
	assert_eq!(removed.copies, 0, "{removed:?}");
	assert!(ada.holds_content(LAPTOP, &ada.plan.1));
}
