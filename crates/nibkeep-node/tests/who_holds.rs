//! What reads who holds a space ([technical design: devices that choose their spaces]): a
//! revocation names the spaces of the person's that the revoking device does not hold, and the
//! devices that do, which revoke there after their next sync; a recovery names the spaces
//! that only the devices it revokes held; a file's origins move with it only where their
//! device holds the new space; a pin names a file in a space that its device holds; and a
//! backup's update names the spaces of the backup that the device does not hold, and the
//! devices that do.
//!
//! The people: Ada, with a desktop that made Project, a laptop paired as a computer and a
//! phone paired as a phone; Leo, whose laptop and desk are lost or kept in a recovery; and,
//! for files, Bea, with a NAS, a laptop that made Archive, and a desk that takes no new
//! spaces.
//!
//! [technical design: devices that choose their spaces]: ../../../docs/Technical-design.md#1310-devices-that-choose-their-spaces

mod common;

use std::path::{Path, PathBuf};

use common::devices::{Devices, bytes, on_disk};
use common::{
	listed_there, now, own_key, own_record, pair_device_as, protection, sync_rounds, user_library,
};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::RecoveryCode;
use nibkeep_node::{
	Behind, HeldElsewhere, Joining, Library, MoveHistory, MoveSecrets, NewBackup, NewDevice,
	NodeError, Taking,
};
use nibkeep_testkit::TestRng;

/// The spaces of a list of spaces held elsewhere, each with the names of the devices that
/// hold it, sorted.
fn named(elsewhere: &[HeldElsewhere]) -> Vec<(SpaceId, Vec<String>)> {
	elsewhere
		.iter()
		.map(|each| {
			let mut names: Vec<String> = each
				.holders
				.iter()
				.filter_map(|holder| holder.device.clone())
				.collect();
			names.sort();
			(each.space, names)
		})
		.collect()
}

/// Ada's desktop, which made Project, before her laptop paired as a computer and her phone
/// as a phone; then the desktop took no new spaces, and the laptop made Taxes, which it alone
/// holds.
struct Ada {
	desktop: Library,
	laptop: Library,
	phone: Library,
	project: SpaceId,
	taxes: SpaceId,
	at: u64,
	rng: TestRng,
}

impl Ada {
	fn new(seed: &str) -> Self {
		let mut rng = TestRng::new(seed);
		let mut desktop = user_library("Ada", "desktop", &mut rng);
		let at = now() + 1_000;
		let project = desktop
			.create_space("Project", at, &mut rng)
			.expect("makes it");
		let mut laptop = pair_device_as(
			&mut desktop,
			(seed, "laptop"),
			&NewDevice::computer(),
			&mut rng,
		);
		let mut phone =
			pair_device_as(&mut desktop, (seed, "phone"), &NewDevice::phone(), &mut rng);
		let mut at = now() + 100_000;
		desktop
			.set_takes_new_spaces(false, at, &mut rng)
			.expect("says so");
		at = sync_rounds(&mut laptop, &mut desktop, at, 2);
		let taxes = laptop
			.create_space("Taxes", at + 1_000, &mut rng)
			.expect("makes it");
		for _ in 0..3 {
			at = sync_rounds(&mut laptop, &mut desktop, at + 1_000, 1);
			at = sync_rounds(&mut phone, &mut desktop, at + 1_000, 1);
		}
		assert!(!desktop.spaces().contains(&taxes));
		Self {
			desktop,
			laptop,
			phone,
			project,
			taxes,
			at,
			rng,
		}
	}
}

#[test]
fn a_revocation_names_the_spaces_it_cannot_reach_and_the_devices_that_hold_them() {
	let mut ada = Ada::new("a revocation out of reach");
	let (project, taxes) = (ada.project, ada.taxes);
	let laptop = own_record(&ada.laptop);
	let laptop_key = own_key(&ada.laptop);
	// The phone holds neither: the desktop holds Project, and nothing else holds Taxes.
	let out = ada.phone.out_of_reach(&[laptop]).expect("reads");
	assert_eq!(
		named(&out),
		[(project, vec!["desktop".to_owned()]), (taxes, Vec::new())]
	);
	assert_eq!(out[0].name.as_deref(), Some("Project"));
	assert_eq!(out[1].name.as_deref(), Some("Taxes"));
	// The desktop reaches Project itself, and names Taxes alone.
	let out = ada.desktop.out_of_reach(&[laptop]).expect("reads");
	assert_eq!(named(&out), [(taxes, Vec::new())]);

	// The phone revokes the lost laptop; the desktop revokes it in Project after its next
	// sync with the phone.
	let at = ada.at + 1_000;
	ada.phone
		.revoke(&[laptop], at, &mut ada.rng)
		.expect("revokes");
	// Still named after the revocation, the laptop no longer among the holders.
	assert_eq!(
		named(&ada.phone.out_of_reach(&[laptop]).expect("reads")),
		[(project, vec!["desktop".to_owned()]), (taxes, Vec::new())]
	);
	assert_eq!(
		listed_there(&ada.desktop, project, &laptop_key),
		Some(false)
	);
	sync_rounds(&mut ada.phone, &mut ada.desktop, at + 1_000, 2);
	assert_eq!(listed_there(&ada.desktop, project, &laptop_key), Some(true));
}

#[test]
fn a_revocation_of_a_device_that_holds_only_what_this_one_holds_names_nothing() {
	let ada = Ada::new("a revocation in reach");
	let phone = own_record(&ada.phone);
	assert_eq!(ada.desktop.out_of_reach(&[phone]).expect("reads"), []);
}

/// Makes a code on a library: the person wrote it down and typed it back.
fn make_code(library: &mut Library, at: u64, rng: &mut TestRng) -> RecoveryCode {
	let code = RecoveryCode::generate(rng);
	library
		.add_recovery_code(&code, false, at, rng)
		.expect("names the code");
	code
}

/// A backup of the personal space alone, made and updated on `library`.
fn backup_of_personal(library: &mut Library, name: &str, at: u64, rng: &mut TestRng) -> PathBuf {
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, backup"))
			.join("Backup");
	let personal = library.personal_space();
	let backup = library
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				spaces: vec![personal],
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

/// Recovers Leo's library on a new computer from the backup in `folder`, keeping the devices
/// named `kept`.
fn recover_keeping(
	name: &str,
	folder: &Path,
	code: &RecoveryCode,
	kept: &[&str],
	(at, rng): (u64, &mut TestRng),
) -> nibkeep_node::Recovered {
	let directory =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), &format!("{name}, new"));
	on_disk(&directory, &format!("{name} new"));
	let joining = Joining::new("new laptop", "nib", rng).expect("names");
	let mut recovering = Library::start_recovery(
		&directory,
		(folder, code),
		joining,
		&protection(),
		&|| at,
		rng,
	)
	.expect("starts");
	let keep: Vec<EntityId> = recovering
		.others()
		.expect("lists")
		.into_iter()
		.filter(|replica| {
			replica
				.device_name
				.as_deref()
				.is_some_and(|device| kept.contains(&device))
		})
		.map(|replica| replica.id)
		.collect();
	recovering.keep(&keep).expect("keeps");
	recovering.finish(None, &|| at, rng).expect("recovers").1
}

#[test]
fn a_recovery_names_the_spaces_that_only_the_devices_it_revokes_held() {
	let name = "a recovery out of reach";
	let mut rng = TestRng::new(name);
	let mut laptop = user_library("Leo", "laptop", &mut rng);
	let mut desk = pair_device_as(&mut laptop, (name, "desk"), &NewDevice::phone(), &mut rng);
	let mut at = now() + 100_000;
	// The laptop alone holds Notes; the desk takes Project too.
	let notes = laptop
		.create_space("Notes", at, &mut rng)
		.expect("makes it");
	let project = laptop
		.create_space_on("Project", &[own_record(&desk)], at + 1_000, &mut rng)
		.expect("makes it");
	at = sync_rounds(&mut laptop, &mut desk, at + 2_000, 2);
	assert_eq!(
		desk.take_space(project, at, &mut rng).expect("takes it"),
		Taking::Took
	);
	let code = make_code(&mut laptop, at + 1_000, &mut rng);
	at = sync_rounds(&mut laptop, &mut desk, at + 2_000, 3);
	let folder = backup_of_personal(&mut laptop, name, at + 1_000, &mut rng);
	let kept_copy = folder.with_file_name("Backup, kept");
	nibkeep_testkit::copy_folder(&folder, &kept_copy);

	// Both lost: neither space comes back, and nothing else holds them.
	let recovered = recover_keeping(name, &folder, &code, &[], (at + 10_000, &mut rng));
	assert_eq!(
		named(&recovered.out_of_reach),
		[(notes, Vec::new()), (project, Vec::new())]
	);
	// The desk kept: it holds Project, and revokes the laptop there after its next sync.
	let recovered = recover_keeping(
		&format!("{name}, kept"),
		&kept_copy,
		&code,
		&["desk"],
		(at + 20_000, &mut rng),
	);
	assert_eq!(
		named(&recovered.out_of_reach),
		[(notes, Vec::new()), (project, vec!["desk".to_owned()])]
	);
}

const NAS: usize = 0;
const LAPTOP: usize = 1;
const DESK: usize = 2;

/// Bea's NAS, laptop and desk, where the desk takes no new spaces, and the laptop made
/// Archive, which the NAS took and the desk did not.
fn bea(name: &str) -> (Devices, SpaceId) {
	let mut devices = Devices::new(name, false);
	devices.pair("laptop", None);
	devices.pair("desk", None);
	devices.record_endpoints();
	let at = devices.tick();
	devices.libraries[DESK]
		.set_takes_new_spaces(false, at, &mut devices.rng)
		.expect("says so");
	let at = devices.tick();
	let archive = devices.libraries[LAPTOP]
		.create_space("Archive", at, &mut devices.rng)
		.expect("makes it");
	for _ in 0..3 {
		devices.sync(LAPTOP, NAS);
		devices.sync(DESK, NAS);
	}
	assert!(devices.libraries[NAS].spaces().contains(&archive));
	assert!(!devices.libraries[DESK].spaces().contains(&archive));
	(devices, archive)
}

/// How many origins a space holds that are neither deleted nor moved, as a device lists them.
fn origins_in(devices: &Devices, device: usize, space: SpaceId) -> usize {
	devices.libraries[device]
		.entities(space, Some(&nibkeep_std::ORIGIN))
		.expect("lists")
		.into_iter()
		.filter(|listed| {
			devices.libraries[device]
				.moved_into(space, listed.id)
				.expect("reads")
				.is_none()
		})
		.count()
}

/// Moves a file of the personal space into `to` on the laptop, purging what stays behind.
fn move_on_laptop(devices: &mut Devices, file: EntityId, to: SpaceId) -> EntityId {
	let at = devices.tick();
	let personal = devices.personal();
	devices.libraries[LAPTOP]
		.move_entity(
			personal,
			file,
			to,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Leave,
			at,
			&mut devices.rng,
		)
		.expect("moves")
}

#[test]
fn an_origin_moves_with_its_file_only_where_its_device_holds_the_new_space() {
	let (mut devices, archive) = bea("who holds, origins");
	let (on_nas, _) = devices.add(NAS, "deed.pdf", &bytes(21, 20_000));
	let (on_desk, _) = devices.add(DESK, "letter.pdf", &bytes(22, 20_000));
	for _ in 0..2 {
		devices.sync(LAPTOP, NAS);
		devices.sync(DESK, NAS);
	}
	let personal = devices.personal();
	assert_eq!(origins_in(&devices, LAPTOP, personal), 2);
	// The NAS holds Archive: its original is told of there.
	move_on_laptop(&mut devices, on_nas, archive);
	assert_eq!(origins_in(&devices, LAPTOP, archive), 1);
	// The desk does not: its original stays out, and goes with what stays behind.
	move_on_laptop(&mut devices, on_desk, archive);
	assert_eq!(origins_in(&devices, LAPTOP, archive), 1);
}

#[test]
fn a_file_is_pinned_only_to_a_device_that_holds_its_space() {
	let (mut devices, archive) = bea("who holds, pins");
	let (file, _) = devices.add(LAPTOP, "deed.pdf", &bytes(23, 20_000));
	let file = move_on_laptop(&mut devices, file, archive);
	let (nas, desk) = (devices.device_of(NAS), devices.device_of(DESK));
	let at = devices.tick();
	let laptop = &mut devices.libraries[LAPTOP];
	assert!(matches!(
		laptop.pin_file((archive, file), desk, true, at, &mut devices.rng),
		Err(NodeError::DeviceLacksSpace)
	));
	laptop
		.pin_file((archive, file), nas, true, at + 1, &mut devices.rng)
		.expect("pins it to the NAS");
	assert!(laptop.pinned_files(nas).expect("reads").contains(&file));
	assert!(laptop.pinned_files(desk).expect("reads").is_empty());
	// Unpinning needs nothing held.
	laptop
		.pin_file((archive, file), desk, false, at + 2, &mut devices.rng)
		.expect("unpins");
}

#[test]
fn a_backups_update_names_the_spaces_that_this_device_does_not_hold() {
	let (mut devices, archive) = bea("who holds, backups");
	let personal = devices.personal();
	let folder =
		nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), "who holds, backups, backup")
			.join("Backup");
	let at = devices.tick();
	let backup = devices.libraries[LAPTOP]
		.add_backup(
			&folder,
			&NewBackup {
				name: "Blue stick".to_owned(),
				spaces: vec![personal, archive],
				..NewBackup::default()
			},
			at,
			&mut devices.rng,
		)
		.expect("adds")
		.backup;
	let update = |devices: &mut Devices, device: usize| {
		let at = devices.tick();
		devices.libraries[device]
			.update_backup(&backup, &folder, &|| at, &|| false)
			.expect("updates")
			.expect("not held by another")
	};
	// The laptop holds both.
	assert_eq!(update(&mut devices, LAPTOP).elsewhere, []);
	devices.sync(LAPTOP, NAS);
	devices.sync(DESK, NAS);
	// The desk does not hold Archive, which the NAS and the laptop update.
	let updated = update(&mut devices, DESK);
	assert_eq!(
		named(&updated.elsewhere),
		[(archive, vec!["laptop".to_owned(), "nas".to_owned()])]
	);
	assert_eq!(updated.elsewhere[0].name.as_deref(), Some("Archive"));
}
