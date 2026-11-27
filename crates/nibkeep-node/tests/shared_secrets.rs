//! Secrets in a shared space ([technical design: sharing]): an admin's device sets them up with the
//! first secret value written there; another member's device asks a device that holds them,
//! whose person gives them; a member's own other devices take them from each other, linked
//! with a key of their personal space; an editor replaces a secret value; and removing a
//! member who held them gives the space a new secrets key, which the removed never get.
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

use crate::common;

use crate::common::{LOGIN, invite_member, login_type, now, pair_joining, sync_in_memory, today};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{FieldName, Value};
use nibkeep_node::{
	Edit, Joining, Library, MemberRole, NewProtection, NodeError, PairingCode, Unlocker,
};
use nibkeep_testkit::TestRng;

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn protected(passphrase: &str) -> NewProtection<'_> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

/// Ada's household: Ada's laptop, whose secrets are set up, is the admin of a space with a
/// login whose password is secret; her desk, paired with the secrets, and Bea's phone, with
/// secrets of her own, invited as `role`; each synced with the laptop.
struct Household {
	laptop: Library,
	desk: Library,
	bea: Library,
	space: SpaceId,
	login: EntityId,
	at: u64,
}

impl Household {
	fn new(seed: &str, role: MemberRole) -> Self {
		let mut rng = TestRng::new(seed);
		let mut laptop = common::user_library("Ada", "laptop", &mut rng);
		laptop
			.set_up_secrets(&protected("ada secrets"), now() + 500, &mut rng)
			.expect("sets up");
		let desk_directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&common::unique_folder(&format!("{seed} desk")),
		);
		std::fs::remove_dir(&desk_directory).expect("removes");
		let code = PairingCode::generate(&mut rng);
		let joining = Joining::new("desk", "nib", &mut rng)
			.expect("names")
			.asking_for_secrets(protected("desk secrets"), &mut rng);
		let (paired, joined) = pair_joining(
			&mut laptop,
			&desk_directory,
			(joining, true),
			(&code, &code),
			&mut rng,
		);
		paired.expect("paired");
		let desk = joined.expect("joined").library;
		let mut at = now() + 100_000;
		let space = laptop
			.create_space("Household", at, &mut rng)
			.expect("makes it");
		laptop
			.define_type(space, &login_type(), at + 100, &mut rng)
			.expect("defines");
		let login = laptop
			.create_entity(
				space,
				&[LOGIN],
				vec![(field("local.title"), Value::Text("Bank".to_owned()))],
				at + 200,
				&mut rng,
			)
			.expect("creates");
		let mut bea = common::user_library("Bea", "phone", &mut rng);
		bea.set_up_secrets(&protected("bea secrets"), at + 300, &mut rng)
			.expect("sets up");
		at += 2_000;
		invite_member(&mut laptop, (space, role), &mut bea, at);
		let mut household = Self {
			laptop,
			desk,
			bea,
			space,
			login,
			at,
		};
		// The desk is listed in the space, and both hear of it.
		for _ in 0..3 {
			household.sync(Device::Desk, Device::Laptop);
		}
		household.sync(Device::Bea, Device::Laptop);
		household
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn library(&mut self, device: Device) -> &mut Library {
		match device {
			Device::Laptop => &mut self.laptop,
			Device::Desk => &mut self.desk,
			Device::Bea => &mut self.bea,
		}
	}

	/// Syncs a device with the laptop, which both must manage.
	fn sync(&mut self, device: Device, with: Device) {
		assert_eq!(with, Device::Laptop, "every device syncs with the laptop");
		let at = self.tick();
		let other = match device {
			Device::Desk => &mut self.desk,
			Device::Bea => &mut self.bea,
			Device::Laptop => unreachable!("not with itself"),
		};
		let (first, second) = sync_in_memory(other, &mut self.laptop, at, (1, 0));
		first.expect("the device syncs");
		second.expect("the laptop syncs");
	}

	/// Sets the login's password on a device.
	fn set_password(&mut self, device: Device, password: &str) -> Result<(), NodeError> {
		let at = self.tick();
		let (space, login) = (self.space, self.login);
		let mut rng = TestRng::new(&format!("setting {password}"));
		self.library(device).edit_entity(
			space,
			login,
			vec![Edit::Set(
				field("local.password"),
				vec![Value::Text(password.to_owned())],
			)],
			&today(),
			at,
			&mut rng,
		)
	}

	/// The passwords that a device shows.
	fn passwords(&mut self, device: Device) -> Vec<String> {
		let (space, login) = (self.space, self.login);
		let password = field("local.password");
		self.library(device)
			.entity(space, login, common::this_day())
			.expect("reads")
			.expect("the login")
			.values(&password)
			.iter()
			.filter_map(|shown| shown.value.as_text().map(str::to_owned))
			.collect()
	}

	/// The numbers of the space's secrets keys that a device holds, and the one it seals
	/// with.
	fn secrets_keys(&mut self, device: Device) -> (Vec<u64>, Option<u64>) {
		let space = self.space;
		self.library(device)
			.secrets_key_numbers(space)
			.expect("a space")
	}

	/// A device's record in the space, as the laptop lists the members.
	fn record(&self, device: &str) -> EntityId {
		self.laptop
			.members(self.space)
			.expect("lists")
			.into_iter()
			.flat_map(|member| member.replicas)
			.find(|replica| replica.device.as_deref() == Some(device))
			.expect("listed")
			.record
	}

	/// Whether the laptop lists a device as holding the space's secrets.
	fn holds_secrets(&self, device: &str) -> bool {
		self.laptop
			.members(self.space)
			.expect("lists")
			.into_iter()
			.flat_map(|member| member.replicas)
			.find(|replica| replica.device.as_deref() == Some(device))
			.expect("listed")
			.holds_secrets
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Device {
	Laptop,
	Desk,
	Bea,
}

#[test]
fn an_admin_sets_up_a_spaces_secrets_and_another_member_asks_for_them() {
	let mut household = Household::new("shared secrets", MemberRole::Editor);
	// The first secret value written in the space sets its secrets up, on the admin's laptop.
	household
		.set_password(Device::Laptop, "hunter2")
		.expect("sets");
	assert_eq!(household.secrets_keys(Device::Laptop).0.len(), 1);
	assert!(household.holds_secrets("laptop"));
	household.sync(Device::Bea, Device::Laptop);
	assert_eq!(household.passwords(Device::Bea), Vec::<String>::new());

	// Bea asks the laptop; Ada gives them; Bea reads the password.
	let (space, laptop) = (household.space, household.record("laptop"));
	let at = household.tick();
	household
		.bea
		.ask_for_space_secrets(space, laptop, at, &mut TestRng::new("asking"))
		.expect("asks");
	household.sync(Device::Bea, Device::Laptop);
	let bea = household.record("phone");
	let at = household.tick();
	let given = household
		.laptop
		.give_space_secrets(space, bea, at, &mut TestRng::new("giving"))
		.expect("gives");
	assert_eq!(given, 1);
	household.sync(Device::Bea, Device::Laptop);
	assert_eq!(household.passwords(Device::Bea), ["hunter2"]);
	assert!(household.holds_secrets("phone"));

	// As an editor, she replaces it, and the laptop shows the new one alone.
	household
		.set_password(Device::Bea, "correct horse")
		.expect("sets");
	household.sync(Device::Bea, Device::Laptop);
	assert_eq!(household.passwords(Device::Laptop), ["correct horse"]);
}

#[test]
fn a_members_other_devices_take_a_spaces_secrets_by_themselves() {
	let mut household = Household::new("own devices' shared secrets", MemberRole::Editor);
	household
		.set_password(Device::Laptop, "hunter2")
		.expect("sets");
	// The desk hears of them while the laptop's secrets are locked: it holds none, and
	// writes none, as another device holds them.
	household.laptop.lock_secrets();
	household.sync(Device::Desk, Device::Laptop);
	assert_eq!(household.passwords(Device::Desk), Vec::<String>::new());
	let written = household.set_password(Device::Desk, "staple");
	assert!(
		matches!(written, Err(NodeError::SpaceSecretsElsewhere)),
		"{written:?}"
	);
	// Once the laptop unlocks them, the desk takes them as they sync, the gift reaching it
	// the next time, as it holds the personal space's secrets.
	household
		.laptop
		.unlock_secrets(&Unlocker::Passphrase("ada secrets"))
		.expect("unlocks");
	household.sync(Device::Desk, Device::Laptop);
	household.sync(Device::Desk, Device::Laptop);
	assert_eq!(household.passwords(Device::Desk), ["hunter2"]);
	assert_eq!(
		household.secrets_keys(Device::Desk),
		household.secrets_keys(Device::Laptop)
	);
	// Bea, who asked for nothing, is given nothing.
	household.sync(Device::Bea, Device::Laptop);
	assert_eq!(household.passwords(Device::Bea), Vec::<String>::new());
	assert!(!household.holds_secrets("phone"));
}

#[test]
fn only_an_admin_sets_up_a_spaces_secrets() {
	let mut household = Household::new("no editor's shared secrets", MemberRole::Editor);
	let written = household.set_password(Device::Bea, "hunter2");
	assert!(
		matches!(written, Err(NodeError::AdminsOnly(_))),
		"{written:?}"
	);
	assert_eq!(household.secrets_keys(Device::Bea).0.len(), 0);
}

#[test]
fn removing_a_member_who_held_a_spaces_secrets_gives_it_a_new_secrets_key() {
	let mut household = Household::new("shared secrets after a removal", MemberRole::Editor);
	household
		.set_password(Device::Laptop, "hunter2")
		.expect("sets");
	household.sync(Device::Bea, Device::Laptop);
	let (space, laptop) = (household.space, household.record("laptop"));
	let at = household.tick();
	household
		.bea
		.ask_for_space_secrets(space, laptop, at, &mut TestRng::new("asking"))
		.expect("asks");
	household.sync(Device::Bea, Device::Laptop);
	let bea = household.record("phone");
	let at = household.tick();
	household
		.laptop
		.give_space_secrets(space, bea, at, &mut TestRng::new("giving"))
		.expect("gives");
	household.sync(Device::Bea, Device::Laptop);
	household.sync(Device::Desk, Device::Laptop);
	let (before, _) = household.secrets_keys(Device::Bea);
	assert_eq!(before.len(), 1);

	// Ada removes Bea, her secrets unlocked: the laptop makes a new secrets key at once.
	let member = household
		.laptop
		.find_member(space, "Bea")
		.expect("a member");
	let at = household.tick();
	household
		.laptop
		.remove_member(space, member, at, &mut TestRng::new("removing"))
		.expect("removes");
	let (after, writing) = household.secrets_keys(Device::Laptop);
	assert_eq!(after.len(), 2, "{after:?}");
	let writing = writing.expect("seals with one");
	assert!(!before.contains(&writing));
	household
		.set_password(Device::Laptop, "correct horse")
		.expect("sets");
	// The desk takes it; Bea, removed, gets her removal and no key.
	household.sync(Device::Desk, Device::Laptop);
	assert_eq!(household.secrets_keys(Device::Desk), (after, Some(writing)));
	assert_eq!(household.passwords(Device::Desk), ["correct horse"]);
	household.sync(Device::Bea, Device::Laptop);
	assert_eq!(household.secrets_keys(Device::Bea).0, before);
	assert_eq!(household.passwords(Device::Bea), ["hunter2"]);
}

#[test]
fn a_code_that_holds_its_persons_secrets_is_given_a_shared_spaces_too() {
	let mut household = Household::new("a code and a space's secrets", MemberRole::Editor);
	household
		.set_password(Device::Laptop, "hunter2")
		.expect("sets");
	let space = household.space;
	let (held, _) = household.secrets_keys(Device::Laptop);
	assert_eq!(held.len(), 1);
	// Ada makes a code with the secrets: her laptop, which holds the space's, gives it
	// those too, as to any device of hers that holds the personal space's.
	let mut rng = TestRng::new("a code and a space's secrets, the code");
	let code = nibkeep_crypto::RecoveryCode::generate(&mut rng);
	let at = household.tick();
	household
		.laptop
		.add_recovery_code(&code, true, at, &mut rng)
		.expect("names the code");
	let in_space = |library: &Library, code: &nibkeep_crypto::RecoveryCode| {
		library
			.check_recovery_code(code)
			.expect("checks")
			.spaces
			.into_iter()
			.find(|keys| keys.space == space)
			.expect("the space names it")
			.secrets
	};
	assert_eq!(
		in_space(&household.laptop, &code),
		Some((held.clone(), vec![]))
	);
	// A code made without the secrets is given none of the space's either.
	let plain = nibkeep_crypto::RecoveryCode::generate(&mut rng);
	let at = household.tick();
	household
		.laptop
		.add_recovery_code(&plain, false, at, &mut rng)
		.expect("names the code");
	assert_eq!(in_space(&household.laptop, &plain), Some((vec![], held)));
}
