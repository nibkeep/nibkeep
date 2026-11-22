//! What the stories share: two computers of the person's, paired, which sync.

use std::sync::{Mutex, MutexGuard, PoisonError};

use nibkeep_end_to_end::{Device, EndToEnd, Ran, str};

/// Held while a story pairs, so that the stories, which run side by side, pair one at a time.
/// A sponsor announces only the first two digits of its code, so a new device tries each that
/// announces them; of two pairings at once whose codes share them, 1 in 90, one sponsor can
/// show the other's try as a wrong one, which no story's transcript expects.
static PAIRING: Mutex<()> = Mutex::new(());

/// Waits until no other story pairs, and holds that until the guard goes.
pub(crate) fn pairing_alone() -> MutexGuard<'static, ()> {
	PAIRING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The laptop with a new library, and the desktop that joins it with the code that the laptop
/// shows.
pub(crate) fn paired(test: &EndToEnd) -> (Device, Device) {
	let laptop = test.device("laptop");
	let desktop = test.device("desktop");
	laptop
		.nib(&[
			"init",
			"--name",
			"Ada",
			"--device",
			"laptop",
			"--languages",
			"en",
		])
		.ok();
	let alone = pairing_alone();
	let mut pairing = laptop.start(&["device", "pair"]);
	let code = pairing.wait_for(r"\d\d-\d{3}-\d{3}");
	desktop
		.nib(&["init", "--join", &code, "--device", "desktop"])
		.ok()
		.stdout_eq(str![[r#"
Looking for the device that shows the code…
Joined the library: sent nothing, received 11 changes.
Ada, on desktop, in [ROOT]/desktop/library
Personal: 7 entities, 11 changes

"#]]);
	pairing.finish().ok().stdout_eq(str![[r#"
On the new device, in the folder for the library, run:

    nib init --join [CODE]

The code works for 10 minutes; 3 wrong tries end it. Both devices must be on the same network. Waiting…
Paired nib on desktop.
Synced with it: sent 11 changes, received nothing.

"#]]);
	drop(alone);
	(laptop, desktop)
}

/// The desktop syncs with the laptop, which serves meanwhile, as `nib sync --serve` does.
pub(crate) fn synced(laptop: &Device, desktop: &Device) -> Ran {
	let serving = laptop.start(&["sync", "--serve"]);
	let synced = desktop.nib(&["sync"]);
	let _ = serving.kill();
	synced
}

/// A device named `name` that joins the library of `sponsor`, with the code it shows.
pub(crate) fn joined(test: &EndToEnd, sponsor: &Device, name: &str) -> Device {
	let device = test.device(name);
	let _alone = pairing_alone();
	let mut pairing = sponsor.start(&["device", "pair"]);
	let code = pairing.wait_for(r"\d\d-\d{3}-\d{3}");
	device
		.nib(&["init", "--join", &code, "--device", name])
		.ok();
	pairing.finish().ok();
	device
}

/// The lines that a command printed, sorted: a sync reaches the devices at once, and says
/// what became of each in no set order.
pub(crate) fn sorted(ran: &Ran) -> Vec<&str> {
	let mut lines: Vec<&str> = ran.stdout.lines().collect();
	lines.sort_unstable();
	lines
}

/// The devices that a device lists, by their names and notes: itself first, as it lists it,
/// then the others, sorted, as it lists them in the order of their IDs, which are random.
pub(crate) fn devices(device: &Device) -> Vec<String> {
	let listed = device.nib(&["device", "list"]).ok();
	let mut names: Vec<String> = listed
		.stdout
		.lines()
		.filter_map(|line| line.split_once("  ").map(|(_, rest)| rest.to_owned()))
		.collect();
	assert!(
		names
			.first()
			.is_some_and(|first| first.ends_with("(this one)")),
		"{listed}"
	);
	names[1..].sort_unstable();
	names
}
