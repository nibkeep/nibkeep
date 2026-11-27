//! Revoking a lost device in the spaces that its person shares with others: the devices that
//! it added there and that the person's other devices never heard of are revoked with it, as
//! nothing tells them from a device that a thief paired on it; those that a device still at
//! hand added stay ([technical design: recovery codes]).
//!
//! The people: Leo, who loses his laptop, and Mona, who shares a space with him.
//!
//! [technical design: recovery codes]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes

use crate::common::{
	invite_member, listed_there, now, own_key, own_record, pair_device, sync_rounds, user_library,
};
use nibkeep_node::{MemberRole, Taking};

#[test]
fn a_device_that_the_lost_one_added_and_no_other_heard_of_is_revoked_with_it() {
	let name = "revoked with the laptop";
	let mut rng = nibkeep_testkit::TestRng::new(name);
	let mut laptop = user_library("Leo", "laptop", &mut rng);
	let mut mona = user_library("Mona", "laptop", &mut rng);
	let mut at = now() + 100_000;
	let family = mona.create_space("Family", at, &mut rng).expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (family, MemberRole::Editor), &mut laptop, at);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	let mut desk = pair_device(&mut laptop, name, "desk", &mut rng);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 3);
	// The desk adds a tablet, which it lists in the family; the laptop learns of it.
	let mut tablet = pair_device(&mut desk, name, "tablet", &mut rng);
	at = sync_rounds(&mut desk, &mut tablet, at + 1_000, 3);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 2);
	// Then the laptop adds a phone, which it lists in the family. The desk learns of the
	// phone from Mona alone, so its personal space does not list it.
	let mut phone = pair_device(&mut laptop, name, "phone", &mut rng);
	at = sync_rounds(&mut laptop, &mut phone, at + 1_000, 3);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	at = sync_rounds(&mut mona, &mut desk, at + 1_000, 1);
	let (laptop_key, phone_key, tablet_key) = (own_key(&laptop), own_key(&phone), own_key(&tablet));
	assert_eq!(listed_there(&desk, family, &phone_key), Some(false));
	assert!(
		desk.replicas()
			.expect("lists")
			.iter()
			.all(|replica| replica.signing != Some(phone_key))
	);
	// The laptop is stolen, and the phone with it. Revoked on the desk, the laptop takes the
	// phone with it, in the family, which the desk could not name; the tablet, which the
	// desk added, stays.
	let laptop_replica = desk
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.signing == Some(laptop_key))
		.expect("the laptop")
		.id;
	let elsewhere = desk
		.revoke(&[laptop_replica], at + 1_000, &mut rng)
		.expect("revokes");
	assert_eq!(listed_there(&desk, family, &laptop_key), Some(true));
	// The tablet, which remains, may have heard of the phone: the phone waits for it.
	assert_eq!(elsewhere, []);
	assert_eq!(listed_there(&desk, family, &phone_key), Some(false));
	// Once the tablet tells the desk all it held after it learned of the revocation, the
	// phone is revoked in the family; the tablet stays.
	at = sync_rounds(&mut desk, &mut tablet, at + 2_000, 2);
	assert_eq!(listed_there(&desk, family, &phone_key), Some(true));
	assert_eq!(listed_there(&desk, family, &tablet_key), Some(false));
	// Mona learns of it, and the new key reaches neither the laptop nor the phone.
	sync_rounds(&mut mona, &mut desk, at + 1_000, 1);
	assert_eq!(listed_there(&mona, family, &phone_key), Some(true));
	drop(phone);
}

#[test]
fn a_device_at_hand_that_the_lost_one_added_to_a_space_alone_stays() {
	let name = "kept from the laptop";
	let mut rng = nibkeep_testkit::TestRng::new(name);
	let mut laptop = user_library("Leo", "laptop", &mut rng);
	let mut mona = user_library("Mona", "laptop", &mut rng);
	// A tablet, paired before Leo joins the family, which then stays away from it.
	let mut tablet = pair_device(&mut laptop, name, "tablet", &mut rng);
	let mut desk = pair_device(&mut laptop, name, "desk", &mut rng);
	let mut at = now() + 100_000;
	at = sync_rounds(&mut laptop, &mut tablet, at, 2);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 2);
	let family = mona.create_space("Family", at, &mut rng).expect("makes it");
	at += 2_000;
	invite_member(&mut mona, (family, MemberRole::Editor), &mut laptop, at);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	at = sync_rounds(&mut laptop, &mut desk, at + 1_000, 3);
	assert_eq!(listed_there(&laptop, family, &own_key(&desk)), Some(false));
	// The tablet pairs a phone, and tells the laptop of it. The phone asks for the family,
	// which the tablet does not hold, and the laptop gives it, and lists it there; the desk
	// learns of it from Mona alone.
	let mut phone = pair_device(&mut tablet, name, "phone", &mut rng);
	at = sync_rounds(&mut tablet, &mut laptop, at + 1_000, 1);
	assert_eq!(
		phone.take_space(family, at, &mut rng).expect("asks"),
		Taking::Asked
	);
	at = sync_rounds(&mut laptop, &mut phone, at + 1_000, 1);
	laptop
		.allow_space(family, own_record(&phone), at, &mut rng)
		.expect("allows");
	at = sync_rounds(&mut laptop, &mut phone, at + 1_000, 3);
	let phone_key = own_key(&phone);
	assert_eq!(listed_there(&laptop, family, &phone_key), Some(false));
	assert_eq!(listed_there(&laptop, family, &own_key(&tablet)), None);
	at = sync_rounds(&mut mona, &mut laptop, at + 1_000, 1);
	at = sync_rounds(&mut mona, &mut desk, at + 1_000, 1);
	assert_eq!(listed_there(&desk, family, &phone_key), Some(false));
	// The laptop alone is stolen. The desk, which has not heard of the phone, revokes it:
	// the phone waits for the tablet, which tells of it, so it stays.
	let laptop_key = own_key(&laptop);
	let laptop_replica = desk
		.replicas()
		.expect("lists")
		.into_iter()
		.find(|replica| replica.signing == Some(laptop_key))
		.expect("the laptop")
		.id;
	let elsewhere = desk
		.revoke(&[laptop_replica], at + 1_000, &mut rng)
		.expect("revokes");
	assert_eq!(elsewhere, []);
	sync_rounds(&mut desk, &mut tablet, at + 2_000, 2);
	assert_eq!(listed_there(&desk, family, &laptop_key), Some(true));
	assert_eq!(listed_there(&desk, family, &phone_key), Some(false));
	assert!(
		desk.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.signing == Some(phone_key) && !replica.revoked)
	);
}
