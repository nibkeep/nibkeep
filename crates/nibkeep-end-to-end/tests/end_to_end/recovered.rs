//! A computer lost with its library, brought back with a recovery code and a backup drive: the
//! person types the code at a terminal, where alone nib reads it. The new computer has the
//! library and its passwords again, under a new passphrase for them; it keeps the device that
//! the person still has, as the person answers, and revokes the lost one, which the kept device
//! learns as it syncs.

use std::fs;
use std::path::PathBuf;

use nibkeep_end_to_end::{Device, EndToEnd, str};

use crate::common::{devices, paired, synced};

/// The passphrase of the secrets on the laptop.
const SECRETS: &str = "The secrets of the laptop in recovered";
/// The passphrase of the secrets on the computer that brings the library back.
const NEW_SECRETS: &str = "The secrets of the restored computer in recovered";
/// The password that the library keeps.
const PASSWORD: &str = "A password that only the secrets hold";

/// The recovery code that `nib recovery new` printed so far, on the lines that hold its groups
/// of five letters and digits, once it has printed them all.
fn printed_code(printed: &str) -> Option<String> {
	let groups: Vec<&str> = printed
		.lines()
		.map(str::trim)
		.filter(|line| {
			!line.is_empty()
				&& line.split('-').all(|group| {
					group.len() == 5
						&& group
							.bytes()
							.all(|symbol| symbol.is_ascii_digit() || symbol.is_ascii_uppercase())
				})
		})
		.collect();
	let code = groups.join("-");
	// Thirteen groups, with the hyphens between them.
	(code.len() == 13 * 6 - 1).then_some(code)
}

/// The laptop keeps a password, and makes a backup and a recovery code that brings the
/// secrets back too, which the backup carries: the backup's folder, and the code.
fn backed_up_with_a_code(test: &EndToEnd, laptop: &Device) -> (PathBuf, String) {
	// The laptop keeps a password, which sets up the library's secrets, with a passphrase of
	// their own; each is typed twice, unseen.
	laptop
		.nib(&["type", "add", "login", "title:text", "password:text:secret"])
		.ok();
	laptop.nib(&["new", "login", "title=Bank"]).ok();
	let mut setting = laptop.start_at_terminal(&["secret", "set", "Bank", "password"]);
	setting.answer_unseen(
		"The library's first secret. A passphrase for its secrets, not the library's: ",
		SECRETS,
	);
	setting.answer_unseen("The passphrase again: ", SECRETS);
	setting.answer_unseen("local.password: ", PASSWORD);
	setting.answer_unseen("local.password again: ", PASSWORD);
	setting
		.finish()
		.ok()
		.stdout_eq(str![[r#"
Bank
  id           [ID]
  type         local.login
  secret       local.password
  local.title  Bank

"#]])
		.stderr_eq(str![[r#"
The library's first secret. A passphrase for its secrets, not the library's: 
The passphrase again: 
local.password: 
local.password again: 

"#]]);

	// It makes a backup on a USB drive, and a recovery code that brings the secrets back too,
	// which the person writes down and types back; the backup then carries the code.
	let drive = test.folder().join("USB drive");
	fs::create_dir(&drive).expect("makes the drive");
	let backup = drive.join("NibKeep backup");
	let folder = backup.to_str().expect("a path in UTF-8");
	laptop
		.nib(&["backup", "new", folder, "--name", "Blue stick"])
		.ok()
		.stdout_eq(str![[r#"
Made the backup Blue stick in [ROOT]/USB drive/NibKeep backup.
Wrote 17 changes, 1 key; took in nothing.
It holds 0 files (0 bytes).
Your computers update it whenever they find it, while nib sync --serve, syncing from login or an app runs; nib backup update does it by hand.

"#]]);
	let mut making = laptop.start_at_terminal(&["recovery", "new", "--secrets"]);
	making.answer_unseen("Passphrase for secrets: ", SECRETS);
	let code = making.wait_until("a recovery code", printed_code);
	test.secret(&code);
	making.answer_unseen("Type the code back, or press Enter to do it later: ", &code);
	making.finish().ok().stdout_eq(str![[r#"
A new recovery code, [CODE_NAME]:

  [RECOVERY_CODE]
  [RECOVERY_CODE]
  [RECOVERY_CODE]

Write it down or print it, with its name, and keep it as safely as a passport:
whoever has it and a backup of this library reads everything in it.
The code holds keys and no data: it brings the library back from a backup, or
from a library on this computer that no longer unlocks.
The recovery code [CODE_NAME] now stands, and opens the secrets too.
It brings the library back from a backup once nib backup update has updated it: Blue stick.

"#]]);
	laptop
		.nib(&["backup", "update", "Blue stick"])
		.ok()
		.stdout_eq(str![[r#"
Updated the backup Blue stick in [ROOT]/USB drive/NibKeep backup.
Wrote 3 changes; took in nothing.
It holds 0 files (0 bytes).

"#]]);
	(backup, code)
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_lost_computer_is_brought_back_with_a_recovery_code_and_a_backup() {
	let test = EndToEnd::new("recovered");
	for secret in [SECRETS, NEW_SECRETS, PASSWORD] {
		test.secret(secret);
	}
	let (laptop, desktop) = paired(&test);

	let (backup, code) = backed_up_with_a_code(&test, &laptop);
	let backup = backup.to_str().expect("a path in UTF-8");
	// The desktop learns of the code, by which it takes the new computer's word later.
	synced(&laptop, &desktop).ok();

	// The laptop is lost, with its library.
	fs::remove_dir_all(laptop.library()).expect("the laptop is lost");

	// A new computer brings the library back from the backup, with the code typed at its
	// terminal. It keeps the desktop, which the person still has, and not the laptop; and
	// asks twice for a new passphrase of the secrets.
	let restored = test.device("restored");
	let mut recovering =
		restored.start_at_terminal(&["recovery", "restore", backup, "--device", "restored"]);
	recovering.answer_unseen("Your recovery code: ", &code);
	recovering.answer("Keep desktop (nib)? ", "y");
	recovering.answer("Keep laptop (nib)? ", "n");
	recovering.answer_unseen(
		"Your recovery code holds your secrets. A passphrase for them on this device, not the library's: ",
		NEW_SECRETS,
	);
	recovering.answer_unseen("The passphrase again: ", NEW_SECRETS);
	recovering
		.finish()
		.ok()
		.stdout_eq(str![[r#"
Brought your library back from the backup Blue stick, and updated it. Your secrets came back too, under the passphrase you gave them here. Revoked your other devices, which you lost: laptop. Kept desktop, which you still have: it learns of this device, and of the revocations, the first time this device syncs with it: run nib sync here while it serves, as nib sync --serve, syncing from login or an app does; or run nib backup update "Blue stick" there. Your recovery code still works, so keep it safe, or replace it: nib recovery new.
Ada, on restored, in [ROOT]/restored/library
Personal: 12 entities, 26 changes

"#]])
		.stderr_eq(str![[r#"
Your recovery code: 
Keep desktop (nib)? Only if you still have it, and its library still unlocks there; otherwise it is revoked as lost. [y/N] y
Keep laptop (nib)? Only if you still have it, and its library still unlocks there; otherwise it is revoked as lost. [y/N] n
Your recovery code holds your secrets. A passphrase for them on this device, not the library's: 
The passphrase again: 

"#]]);

	// The password is back, under the new passphrase of the secrets.
	let mut showing = restored.start_at_terminal(&["secret", "show", "Bank", "password"]);
	showing.answer_unseen("Passphrase for secrets: ", NEW_SECRETS);
	assert_eq!(showing.finish().ok().stdout.trim(), PASSWORD);

	// The person puts the drive away, which the desktop, as it serves, would otherwise update
	// and take some of the new computer's changes from, before the new computer sends them.
	let drive = test.folder().join("USB drive");
	fs::rename(&drive, test.folder().join("USB drive, put away")).expect("puts the drive away");

	// The new computer syncs with the desktop, which serves, and introduces itself; the desktop
	// learns that the laptop is revoked.
	synced(&desktop, &restored).ok().stdout_eq(str![[r#"
Looking for your other devices…
Synced with nib on desktop: sent 6 changes, received nothing.

"#]]);
	assert_eq!(
		devices(&desktop),
		[
			"nib on desktop (this one)",
			"nib on laptop (revoked; held the secrets)",
			"nib on restored (holds the secrets)",
		]
	);
}
