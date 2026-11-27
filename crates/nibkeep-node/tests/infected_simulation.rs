//! A simulation of a person's devices that sync in pairs, one of which turned against its
//! user: a device of theirs, or an AI agent's replica, which writes whatever its own key can
//! sign and sends it whenever it syncs, from a seed. The honest devices keep the guarantees
//! of the testing strategy (`docs/Plan.md`, section 4), checked after every step: they sync
//! with each other and then show the same; each opens again showing the same; each takes
//! new spaces as it said itself, whatever the infected device says on its record; an AI
//! agent's replica never gets the secrets, and none of its governing changes or purges
//! counts; and
//! once an honest device revokes the infected one, every change of the infected device that
//! the revoker lacked is set aside wherever the revocation is, the infected device gets
//! nothing more from any of them, and every honest device that the revoker still counts
//! comes back to show what it shows, also one that took the infected device's revocation of
//! the revoker first, as the two revocations meet there.
//!
//! Each run reports its seed when it fails. `NIBKEEP_SIMULATION_SEED=<seed>` repeats one
//! run. `NIBKEEP_SIMULATION_RUNS=<count>` runs more seeds than the usual few, from
//! `NIBKEEP_SIMULATION_FIRST_SEED`, or 0. The long tests give `NIBKEEP_SIMULATION_RUN`, the
//! number of their run, instead, so that each run starts where the one before ended. Even
//! seeds infect a device of the person's, odd ones an AI agent's replica.
//! `NIBKEEP_SIMULATION_DEBUG` reports every step of a run that fails, not only the last.

use crate::common;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read;

use crate::common::in_process::{pipes, settle};
use crate::common::{now, own_key, pair_ai_agent_named, pair_joining, replica_keys, this_day};
use nibkeep_base::{EntityId, Hash, SpaceId, StatementId};
use nibkeep_crypto::rand_core::TryRng;
use nibkeep_crypto::{Key, PassphraseParams, Signature, VerifyingKey};
use nibkeep_governance::RecoveryListing;
use nibkeep_log::{
	Change, ChangeId, Draft, Kind, Parcel, Payload, Purges, SpaceKeys, WrappedBlobKey, WrappedKey,
};
use nibkeep_model::{FieldDef, FieldName, Operation, Statement, TypeDef, TypeName, Value, fields};
use nibkeep_node::{
	Dating, Edit, Joining, Library, NewProtection, NodeError, PairingCode, Syncing, Unlocker,
};
use nibkeep_sync::{BlobBytes, Challenge, Holder, Holding};
use nibkeep_testkit::TestRng;
use nibkeep_time::Timestamp;

/// The runs of an ordinary test, and the steps of each.
const RUNS: u64 = 6;
const STEPS: usize = 120;

/// How far ahead of its wall clock a device takes a change: ten minutes, as the engine
/// does.
const DRIFT: u64 = 600_000;

const WORDS: [&str; 8] = [
	"amber", "birch", "cedar", "delta", "ember", "fjord", "grove", "heron",
];

const NOTE: TypeName = TypeName::from_static("local.note");
const LOGIN: TypeName = TypeName::from_static("local.login");

/// The word that every password holds, which no search may find, and the one that the
/// infected device writes, plainly, to the field of passwords.
const SECRET_WORD: &str = "sesame";
const PLAIN_WORD: &str = "opensesame";

/// The passphrase of the secrets.
const SECRETS: &str = "secrets of the simulation";

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn title() -> FieldName {
	field("local.title")
}

fn tag() -> FieldName {
	field("local.tag")
}

fn password() -> FieldName {
	field("local.password")
}

/// Who the infected device is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Infection {
	/// A device of the person's, which may do anything in the personal space until it is
	/// revoked.
	OwnDevice,
	/// An AI agent's replica, paired with the person's laptop, which may write the person's
	/// data, and never their secrets or who the replicas are.
	AiAgent,
}

/// What a change that the infected device made up does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
	Governing,
	Purge,
	Ordinary,
}

/// A change that the infected device made up.
struct Forged {
	parcel: Parcel,
	/// Its depth in the graph of changes, as a sync sends parents first.
	depth: u64,
	shape: Shape,
	/// Whether an honest device took it.
	taken: bool,
	/// Whether it can count where it is taken: sent with its payload and a key of the space,
	/// and with a payload that fits its kind. Else it adds nothing, neither counting nor set
	/// aside, while it waits for its payload or key, or for good once the payload is read.
	whole: bool,
	/// Whether it restores, or writes what an AI agent may not, so that it never counts from
	/// an AI agent's replica.
	beyond_an_agent: bool,
	/// The number of the space key it is sealed with: a device that lacks it, such as one
	/// that the infected device made as it revoked that device, cannot open it yet.
	epoch: u64,
}

impl Forged {
	fn whole(&self) -> bool {
		self.whole
	}
}

/// A revocation of the infected device by an honest one.
struct Revocation {
	/// The honest device that revoked it.
	by: usize,
	/// The changes that the revoker held then: of the infected device's, every other is set
	/// aside wherever the revocation is.
	held: BTreeSet<ChangeId>,
	/// Whether a revocation of the revoker by the infected device may stand: the revoker
	/// held one, which its revocation builds on where it read it, or, for the infected
	/// device's revocation of itself, the infected device had revoked it before.
	knowing: bool,
}

/// An honest device: its library, and how it opens again.
struct Device {
	library: Library,
	name: &'static str,
	passphrase: &'static str,
}

/// One run of the simulation.
struct Run {
	seed: u64,
	infection: Infection,
	choices: TestRng,
	/// What the infected device says, on a replica's record, of the spaces given to it later,
	/// drawn apart, so that the other draws come as they did before it could
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	marks: TestRng,
	/// Whether the infected device sends each of its revocations of an honest device to
	/// that device without its payload, drawn apart for the same reason.
	withholds: TestRng,
	rng: TestRng,
	/// The laptop, which made the library, and the phone, which it paired.
	honest: Vec<Device>,
	infected: Library,
	/// The keys of the laptop's, the phone's and the infected device's replicas, and their
	/// records, as they were paired: what the infected device writes may change how the
	/// records show.
	keys: Vec<VerifyingKey>,
	records: Vec<EntityId>,
	forged: Vec<Forged>,
	/// The notes the honest devices made.
	notes: Vec<EntityId>,
	/// The logins the laptop made, with their passwords.
	logins: Vec<EntityId>,
	at: u64,
	revocation: Option<Revocation>,
	/// Whether each honest device holds the revocation.
	knows: Vec<bool>,
	/// The infected device's changes that may revoke each honest device.
	revoking: Vec<BTreeSet<ChangeId>>,
	/// Its revocations of each honest device that it sends that device without their
	/// payloads, which the devices that read them never send a device they revoke.
	withheld: Vec<BTreeSet<ChangeId>>,
	/// The notes made after the revocation, which the infected device cannot name where the
	/// revocation is, with every title that honest devices gave them.
	after: BTreeMap<EntityId, BTreeSet<String>>,
	log: Vec<String>,
	seen: BTreeMap<&'static str, usize>,
	/// What came of each forged change, as counted in `seen` once.
	fates: BTreeSet<(ChangeId, &'static str)>,
}

/// The situations that the runs of an ordinary test must come to.
const SITUATIONS: [&str; 19] = [
	"a forged change counted",
	"a forged change was set aside",
	"a forged change was not taken",
	"a forged governing change counted",
	"a forged governing change was set aside",
	"a forged purge counted",
	"a forged purge was set aside",
	"a forged change had no parents",
	"a forged change built on one nobody holds",
	"a forged change was dated ahead",
	"a forged change came without its payload",
	"the infected device was revoked",
	"the infected device revoked an honest device by the engine",
	"the infected device withheld its revocation's payload",
	"a revoked honest device was heard",
	"two revocations met",
	"an honest device refused the infected one",
	"honest devices synced",
	"an honest device opened again",
];

macro_rules! check {
	($run:expr, $condition:expr, $($message:tt)+) => {
		if !$condition {
			$run.fail(&format!($($message)+));
		}
	};
}

impl Run {
	fn new(seed: u64) -> Self {
		let infection = if seed.is_multiple_of(2) {
			Infection::OwnDevice
		} else {
			Infection::AiAgent
		};
		let name = format!("infected simulation {seed}");
		let mut rng = TestRng::new(&name);
		let mut laptop = laptop(&name, &mut rng);
		let phone = paired(&mut laptop, &format!("{name}, phone"), "phone", &mut rng);
		let infected = match infection {
			Infection::OwnDevice => paired(&mut laptop, &format!("{name}, desk"), "desk", &mut rng),
			Infection::AiAgent => ai_agent(&mut laptop, &format!("{name}, agent"), &mut rng),
		};
		let keys: Vec<VerifyingKey> = [&laptop, &phone, &infected]
			.into_iter()
			.map(own_key)
			.collect();
		let replicas = laptop.replicas().expect("lists");
		let records = keys
			.iter()
			.map(|key| {
				replicas
					.iter()
					.find(|replica| replica.signing == Some(*key))
					.expect("a replica the laptop paired")
					.id
			})
			.collect();
		let mut run = Self {
			seed,
			infection,
			choices: TestRng::new(&format!("{name}, choices")),
			marks: TestRng::new(&format!("{name}, marks")),
			withholds: TestRng::new(&format!("{name}, withholds")),
			rng,
			honest: vec![
				Device {
					library: laptop,
					name: "laptop",
					passphrase: "correct horse",
				},
				Device {
					library: phone,
					name: "phone",
					passphrase: "battery staple",
				},
			],
			infected,
			keys,
			records,
			forged: Vec::new(),
			notes: Vec::new(),
			logins: Vec::new(),
			at: now() + 120_000,
			revocation: None,
			knows: vec![false, false],
			revoking: vec![BTreeSet::new(), BTreeSet::new()],
			withheld: vec![BTreeSet::new(), BTreeSet::new()],
			after: BTreeMap::new(),
			log: vec![format!("the infected device is {infection:?}")],
			seen: BTreeMap::new(),
			fates: BTreeSet::new(),
		};
		// Everyone starts with what the laptop holds.
		run.honest_sync(0, 1);
		run.infected_sync(0, true);
		run
	}

	fn fail(&self, message: &str) -> ! {
		let mut report = format!(
			"infected simulation seed {} failed: {message}\nlast steps:\n",
			self.seed
		);
		let shown = if std::env::var("NIBKEEP_SIMULATION_DEBUG").is_ok() {
			self.log.len()
		} else {
			16
		};
		for line in self.log.iter().rev().take(shown).rev() {
			let _ = writeln!(report, "  {line}");
		}
		let _ = write!(
			report,
			"repeat it with NIBKEEP_SIMULATION_SEED={}",
			self.seed
		);
		panic!("{report}");
	}

	fn saw(&mut self, situation: &'static str) {
		*self.seen.entry(situation).or_default() += 1;
	}

	/// Counts what came of a forged change, once.
	fn fate(&mut self, id: ChangeId, situation: &'static str) {
		if self.fates.insert((id, situation)) {
			self.saw(situation);
		}
	}

	fn below(&mut self, count: usize) -> usize {
		let count = u64::try_from(count).expect("small");
		let chosen = self.choices.try_next_u64().expect("infallible") % count;
		usize::try_from(chosen).expect("small")
	}

	fn chance(&mut self, percent: usize) -> bool {
		self.below(100) < percent
	}

	fn tick(&mut self) -> u64 {
		self.at += 1_000;
		self.at
	}

	fn word(&mut self) -> String {
		WORDS[self.below(WORDS.len())].to_owned()
	}

	fn personal(&self) -> SpaceId {
		self.infected.personal_space()
	}

	/// Whether an honest device knows that it was revoked.
	fn revoked_itself(&self, device: usize) -> bool {
		self.honest[device]
			.library
			.is_revoked()
			.expect("reads the personal space")
	}

	/// Whether an honest device holds the revocation of the infected device and counts it:
	/// one that holds it may lack the key that it was written with, as when the infected
	/// device revoked that device, and wrote with a new key, until another gives it the key.
	fn applies_revocation(&self, device: usize) -> bool {
		self.knows[device] && self.counts_infected_revoked(device)
	}

	/// Whether two honest devices count each other: neither knows that it, or the other,
	/// was revoked.
	fn stand(&self, a: usize, b: usize) -> bool {
		let counts = |from: usize, of: usize| !self.counts_revoked(from, self.records[of]);
		!self.revoked_itself(a) && !self.revoked_itself(b) && counts(a, b) && counts(b, a)
	}

	// The honest devices' steps.

	fn create(&mut self) {
		let device = self.below(self.honest.len());
		let text = format!("{} {}", self.word(), self.word());
		let now = self.tick();
		let personal = self.personal();
		let created = self.honest[device].library.create_entity(
			personal,
			&[NOTE],
			vec![(title(), Value::Text(text.clone()))],
			now,
			&mut self.rng,
		);
		self.log.push(format!(
			"the {} makes a note {text:?}: {created:?}",
			self.honest[device].name
		));
		match created {
			Ok(note) => {
				self.notes.push(note);
				if self.revocation.is_some() {
					self.after.insert(note, [text].into());
				}
			}
			Err(NodeError::Revoked) if self.revoked_itself(device) => {}
			Err(error) => self.fail(&format!("making a note failed: {error}")),
		}
	}

	/// The laptop, which holds the secrets unlocked, makes a login with a password.
	fn create_login(&mut self) {
		let secret = format!("{SECRET_WORD} {}", self.word());
		let name = self.word();
		let now = self.tick();
		let personal = self.personal();
		let created = self.honest[0].library.create_entity(
			personal,
			&[LOGIN],
			vec![
				(title(), Value::Text(name)),
				(password(), Value::Text(secret)),
			],
			now,
			&mut self.rng,
		);
		self.log
			.push(format!("the laptop makes a login: {created:?}"));
		match created {
			Ok(login) => self.logins.push(login),
			Err(NodeError::Revoked) if self.revoked_itself(0) => {}
			Err(error) => self.fail(&format!("making a login failed: {error}")),
		}
	}

	fn edit(&mut self) {
		if self.notes.is_empty() {
			return self.create();
		}
		let device = self.below(self.honest.len());
		let index = self.below(self.notes.len());
		let note = self.notes[index];
		let personal = self.personal();
		let shown = self.honest[device]
			.library
			.entity(personal, note, this_day())
			.expect("reads");
		let Some(shown) = shown else {
			return;
		};
		let edit = match self.below(6) {
			0 if !shown.deleted => Edit::Delete,
			0 => Edit::Restore,
			1 | 2 => {
				let word = self.word();
				if let Some(titles) = self.after.get_mut(&note) {
					titles.insert(word.clone());
				}
				Edit::Set(title(), vec![Value::Text(word)])
			}
			_ => Edit::Add(tag(), Value::Text(self.word())),
		};
		let now = self.tick();
		let dating = Dating::today(now, 0).expect("a moment");
		self.log.push(format!(
			"the {} edits {note} with {edit:?}",
			self.honest[device].name
		));
		let edited = self.honest[device].library.edit_entity(
			personal,
			note,
			vec![edit],
			&dating,
			now,
			&mut self.rng,
		);
		match edited {
			// Values that the infected device wrote may have made the edit one the type
			// refuses, such as a second title.
			Ok(()) | Err(NodeError::OneValue(_) | NodeError::NoValue(_)) => {}
			Err(NodeError::Revoked) if self.revoked_itself(device) => {}
			Err(error) => self.fail(&format!("editing failed: {error}")),
		}
	}

	/// Two honest devices that count each other sync twice, the second bringing what each
	/// wrote as it settled after the first, and then show the same; or after a third, which
	/// brings back what a purge of the infected device's, set aside by the second, had
	/// destroyed on one of them (Known limits, Trust between devices).
	fn honest_sync(&mut self, a: usize, b: usize) {
		if !self.stand(a, b) && !self.heard_sync(a, b) {
			return;
		}
		self.sync_once(a, b);
		if !self.stand(a, b) {
			return;
		}
		self.sync_once(a, b);
		let (mut left, mut right) = (view(&self.honest[a].library), view(&self.honest[b].library));
		let purged = self
			.forged
			.iter()
			.any(|forged| forged.shape == Shape::Purge);
		if left != right && self.revocation.is_some() && purged {
			self.saw("honest devices needed a third sync");
			self.sync_once(a, b);
			(left, right) = (view(&self.honest[a].library), view(&self.honest[b].library));
		}
		if left != right {
			let differing: Vec<String> = left
				.iter()
				.zip(&right)
				.filter(|(left, right)| left != right)
				.take(3)
				.map(|(left, right)| {
					let (left, right) = where_they_differ(left, right);
					format!("{left}\n    against {right}")
				})
				.collect();
			self.fail(&format!(
				"after their syncs, the {} and the {} show differently: {}{}",
				self.honest[a].name,
				self.honest[b].name,
				differing.join("; "),
				if left.len() == right.len() {
					String::new()
				} else {
					format!(" ({} lines against {})", left.len(), right.len())
				}
			));
		}
		self.check_guarantees();
	}

	/// Two honest devices that do not count each other, by the infected device's revocations.
	/// One that the other counts revoked, and that knows nothing of it, connects, and is
	/// heard, as it was added before the infected device and would stay if it revoked it
	/// back: it gets nothing, and if it holds a revocation of the infected device, the two
	/// revocations meet, and the two count each other again. One that knows that it was
	/// revoked syncs all the same, and a revocation of the infected device that it gets may
	/// set that aside. Returns whether they count each other then.
	fn heard_sync(&mut self, a: usize, b: usize) -> bool {
		let (heard, hearing) = match (
			self.counts_revoked(a, self.records[b]),
			self.counts_revoked(b, self.records[a]),
		) {
			(true, false) => (b, a),
			// The second counts the first revoked, or neither the other, though they do not
			// stand together.
			(false, _) => (a, b),
			// Each counts the other revoked, as the infected device revoked each where the
			// other was: neither sends the other anything.
			(true, true) => return false,
		};
		let uneven = self.counts_revoked(hearing, self.records[heard]);
		let before = self.held(heard);
		let at = self.tick();
		self.log.push(if uneven {
			format!(
				"the {}, which the {} counts revoked, syncs with it",
				self.honest[heard].name, self.honest[hearing].name
			)
		} else {
			format!(
				"the {} syncs with the {}, though one knows that it was revoked",
				self.honest[heard].name, self.honest[hearing].name
			)
		});
		let (first, second) = if heard < hearing {
			let (left, right) = self.honest.split_at_mut(hearing);
			(&mut left[heard].library, &mut right[0].library)
		} else {
			let (left, right) = self.honest.split_at_mut(heard);
			(&mut right[0].library, &mut left[hearing].library)
		};
		let (first, second) = common::sync_in_memory(first, second, at, (heard, hearing));
		for (device, synced) in [(heard, first), (hearing, second)] {
			match synced {
				Ok(_) => {}
				// One that knows that it was revoked syncs all the same, and writes nothing.
				Err(NodeError::Revoked) if self.revoked_itself(device) => {}
				Err(error) => self.fail(&format!(
					"the {} failed to sync: {error}",
					self.honest[device].name
				)),
			}
		}
		if uneven {
			self.saw("a revoked honest device was heard");
		}
		// What the heard device made reaches the hearing one first, such as its revocation
		// of the device that revoked it: once that sets the revocation aside there, the two
		// revocations met, and the hearing device sends it what it lacks.
		if uneven && self.counts_revoked(hearing, self.records[heard]) {
			let after = self.held(heard);
			check!(
				self,
				after == before,
				"the {} got changes from the {}, which counts it revoked",
				self.honest[heard].name,
				self.honest[hearing].name
			);
			return false;
		}
		if !self.stand(a, b) {
			return false;
		}
		self.saw(if uneven {
			"two revocations met"
		} else {
			"a device that knew it was revoked learned otherwise"
		});
		let knows = self.knows[a] || self.knows[b];
		self.knows[a] = knows;
		self.knows[b] = knows;
		true
	}

	/// Two honest devices sync once, which neither may fail.
	fn sync_once(&mut self, a: usize, b: usize) {
		let at = self.tick();
		self.log.push(format!(
			"the {} syncs with the {}",
			self.honest[a].name, self.honest[b].name
		));
		let (first, second) = if a < b {
			let (left, right) = self.honest.split_at_mut(b);
			(&mut left[a].library, &mut right[0].library)
		} else {
			let (left, right) = self.honest.split_at_mut(a);
			(&mut right[0].library, &mut left[b].library)
		};
		let (first, second) = common::sync_in_memory(first, second, at, (a, b));
		if let Err(error) = first {
			self.fail(&format!(
				"the {} failed to sync: {error}",
				self.honest[a].name
			));
		}
		if let Err(error) = second {
			self.fail(&format!(
				"the {} failed to sync: {error}",
				self.honest[b].name
			));
		}
		self.saw("honest devices synced");
		let knows = self.knows[a] || self.knows[b];
		self.knows[a] = knows;
		self.knows[b] = knows;
	}

	/// An honest device closes its library and opens it again: it shows the same.
	fn restart(&mut self) {
		let device = self.below(self.honest.len());
		self.log
			.push(format!("the {} opens again", self.honest[device].name));
		let before = view(&self.honest[device].library);
		let Device {
			library,
			name,
			passphrase,
		} = self.honest.remove(device);
		let directory = library.library_directory().expect("a library").to_owned();
		drop(library);
		let mut library = match Library::open(&directory, &Unlocker::Passphrase(passphrase)) {
			Ok(library) => library,
			Err(error) => self.fail(&format!("the {name} failed to open again: {error}")),
		};
		// The person unlocks the secrets on the laptop again, where they write passwords.
		if device == 0
			&& let Err(error) = library.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		{
			self.fail(&format!("the laptop failed to unlock its secrets: {error}"));
		}
		self.honest.insert(
			device,
			Device {
				library,
				name,
				passphrase,
			},
		);
		self.saw("an honest device opened again");
		let after = view(&self.honest[device].library);
		check!(
			self,
			before == after,
			"the {name} shows differently after it opened again"
		);
	}

	/// An honest device revokes the infected one, if it still can.
	fn revoke(&mut self, device: usize) {
		if self.revocation.is_some() || self.revoked_itself(device) {
			return;
		}
		let replicas = self.honest[device].library.replicas().expect("lists");
		let Some(infected) = replicas
			.iter()
			.find(|replica| replica.id == self.records[2])
		else {
			return;
		};
		let held = self.held(device);
		if infected.revoked {
			// The infected device revoked itself, and this device holds that: what it made
			// that this device does not hold now builds on none of it, and is set aside.
			self.log.push(format!(
				"the {} holds the infected device's revocation of itself",
				self.honest[device].name
			));
			self.saw("the infected device revoked itself");
			let knowing = !self.revoking[device].is_empty();
			self.revocation = Some(Revocation {
				by: device,
				held,
				knowing,
			});
			self.knows[device] = true;
			self.check_guarantees();
			return;
		}
		let infected = infected.id;
		let now = self.tick();
		self.log.push(format!(
			"the {} revokes the infected device",
			self.honest[device].name
		));
		match self.honest[device]
			.library
			.revoke(&[infected], now, &mut self.rng)
		{
			Ok(_) => {}
			Err(error) => self.fail(&format!("revoking failed: {error}")),
		}
		self.saw("the infected device was revoked");
		let knowing = self.revoking[device].iter().any(|id| held.contains(id));
		self.revocation = Some(Revocation {
			by: device,
			held,
			knowing,
		});
		self.knows[device] = true;
		self.check_guarantees();
	}

	/// The infected device revokes an honest one, as the engine revokes a device, which an
	/// AI agent's replica may not, nor one that counts itself revoked.
	fn infected_revoke(&mut self) {
		let device = self.below(self.honest.len());
		let now = self.tick();
		self.log.push(format!(
			"the infected device revokes the {}",
			self.honest[device].name
		));
		let before: BTreeSet<ChangeId> = changes_of(&mut self.infected)
			.into_iter()
			.map(|(id, _, _)| id)
			.collect();
		if self
			.infected
			.revoke(&[self.records[device]], now, &mut self.rng)
			.is_ok()
		{
			self.saw("the infected device revoked an honest device by the engine");
			let made: Vec<ChangeId> = changes_of(&mut self.infected)
				.into_iter()
				.map(|(id, _, _)| id)
				.filter(|id| !before.contains(id))
				.collect();
			if self
				.withholds
				.try_next_u64()
				.expect("infallible")
				.is_multiple_of(2)
			{
				self.log.push(format!(
					"the infected device will send the {} its revocation without the payload",
					self.honest[device].name
				));
				self.saw("the infected device withheld its revocation's payload");
				self.withheld[device].extend(made.iter().copied());
			}
			self.revoking[device].extend(made);
		}
	}

	/// The changes that an honest device holds.
	fn held(&mut self, device: usize) -> BTreeSet<ChangeId> {
		changes_of(&mut self.honest[device].library)
			.into_iter()
			.map(|(id, _, _)| id)
			.collect()
	}

	/// The changes that an honest device sets aside.
	fn set_aside(&self, device: usize) -> BTreeSet<ChangeId> {
		self.honest[device]
			.library
			.set_aside()
			.expect("lists")
			.into_iter()
			.map(|set_aside| set_aside.change)
			.collect()
	}

	/// What must hold on every honest device whatever the infected one sent.
	fn check_guarantees(&mut self) {
		for device in 0..self.honest.len() {
			self.check_device(device);
		}
		self.check_secrets();
		self.check_after();
	}

	/// What comes of each forged change on an honest device: an AI agent's governing
	/// changes and purges never count, nor does it hold the secrets; and once the device holds
	/// the revocation, no change of the infected device that the revoker lacked counts.
	fn check_device(&mut self, device: usize) {
		let held = self.held(device);
		let personal = self.personal();
		let (opens, _) = self.honest[device]
			.library
			.space_key_numbers(personal)
			.expect("reads");
		let set_aside = self.set_aside(device);
		let mut counted = Vec::new();
		let mut fates = Vec::new();
		for forged in self.forged.iter().filter(|forged| forged.whole()) {
			let id = forged.parcel.id();
			// One that waits for a key that the device lacks, such as one that the infected
			// device made as it revoked that device, adds nothing yet.
			if !held.contains(&id) || !opens.contains(&forged.epoch) {
				continue;
			}
			let aside = set_aside.contains(&id);
			fates.push((
				id,
				match (aside, forged.shape) {
					(true, Shape::Governing) => "a forged governing change was set aside",
					(true, Shape::Purge) => "a forged purge was set aside",
					(true, Shape::Ordinary) => "a forged change was set aside",
					(false, Shape::Governing) => "a forged governing change counted",
					(false, Shape::Purge) => "a forged purge counted",
					(false, Shape::Ordinary) => "a forged change counted",
				},
			));
			if !aside {
				counted.push((id, forged.shape, forged.beyond_an_agent));
			}
		}
		for (id, fate) in fates {
			self.fate(id, fate);
		}
		let name = self.honest[device].name;
		// Whatever the infected device says on its record, an honest device takes new spaces
		// as it said itself: it said nothing, so it takes them
		// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
		let takes = self.honest[device]
			.library
			.takes_new_spaces()
			.expect("reads");
		check!(
			self,
			takes,
			"the {name} asks before it takes new spaces, as another device said"
		);
		if self.infection == Infection::AiAgent {
			if let Some((id, shape, _)) = counted
				.iter()
				.find(|(_, shape, beyond)| *shape != Shape::Ordinary || *beyond)
			{
				self.fail(&format!(
					"the {name} counts an AI agent's change that {}: {id}",
					match shape {
						Shape::Governing => "governs",
						Shape::Purge => "purges",
						Shape::Ordinary => "restores, or writes what an AI agent may not",
					}
				));
			}
			let holds = self.honest[device]
				.library
				.replicas()
				.expect("lists")
				.iter()
				.any(|replica| replica.id == self.records[2] && replica.holds_secrets);
			check!(
				self,
				!holds,
				"the {name} says that the AI agent's replica holds the secrets"
			);
		}
		if let Some(revocation) = &self.revocation
			&& self.applies_revocation(device)
		{
			let lacked: Vec<&ChangeId> = counted
				.iter()
				.map(|(id, _, _)| id)
				.filter(|id| !revocation.held.contains(id))
				.collect();
			check!(
				self,
				lacked.is_empty(),
				"the {name} counts changes of the revoked device that the {} lacked: {lacked:?}",
				self.honest[revocation.by].name
			);
		}
	}

	/// No search finds a password, nor what the infected device wrote plainly to the field of
	/// passwords; and an AI agent's replica reads no password.
	fn check_secrets(&self) {
		let personal = self.personal();
		for device in &self.honest {
			for word in [SECRET_WORD, PLAIN_WORD] {
				let found = device.library.search(personal, word).expect("searches");
				check!(
					self,
					found.is_empty(),
					"a search for {word:?} on the {} finds {found:?}",
					device.name
				);
			}
		}
		if self.infection == Infection::AiAgent {
			for login in &self.logins {
				let read = self
					.infected
					.entity(personal, *login, this_day())
					.expect("reads")
					.map(|shown| shown.values(&password()).len());
				check!(
					self,
					read.unwrap_or(0) == 0,
					"the AI agent's replica reads {read:?} passwords of {login}"
				);
			}
		}
	}

	/// What the honest devices made after the revocation shows only the titles they gave it,
	/// on each device that holds the revocation.
	fn check_after(&self) {
		let personal = self.personal();
		for (device, honest) in self.honest.iter().enumerate() {
			if !self.applies_revocation(device) || self.revoked_itself(device) {
				continue;
			}
			for (note, given) in &self.after {
				let Some(shown) = honest
					.library
					.entity(personal, *note, this_day())
					.expect("reads")
				else {
					continue;
				};
				let titles: Vec<&Value> = shown
					.values(&title())
					.iter()
					.map(|shown| &shown.value)
					.collect();
				check!(
					self,
					titles
						.iter()
						.all(|title| title.as_text().is_some_and(|title| given.contains(title))),
					"the {} shows a note made after the revocation with the titles {titles:?}",
					honest.name
				);
			}
		}
	}

	// The infected device's steps.

	/// The changes that the infected device knows of, with their times and depths: those its
	/// library holds, and those it made up that an honest device took, or all of them if
	/// `all` says so.
	fn known(&mut self, all: bool) -> Vec<(ChangeId, u64, u64)> {
		let mut known = changes_of(&mut self.infected);
		known.extend(
			self.forged
				.iter()
				.filter(|forged| all || forged.taken)
				.map(|forged| {
					(
						forged.parcel.id(),
						forged.parcel.change.timestamp().millis(),
						forged.depth,
					)
				}),
		);
		known.sort_by_key(|(id, _, depth)| (*depth, *id));
		known
	}

	/// The text of a signing key that the infected device may name: an honest device's, its
	/// own, or one nobody has.
	fn some_key(&mut self) -> String {
		match self.below(4) {
			0 => self.keys[2].to_text(),
			1 => self.rng.signing_key().verifying_key().to_text(),
			device => self.keys[device - 2].to_text(),
		}
	}

	/// The text of a choice of a space that the infected device may state: signed by its own
	/// key, mostly, or by one nobody has; of the personal space or of one nobody holds; with
	/// any number, holding or not
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	fn some_choice(&mut self) -> String {
		let signing = if self.chance(75) {
			replica_keys(
				self.infected.library_directory().expect("a library"),
				&Unlocker::Passphrase("battery staple"),
				self.personal(),
			)
			.0
			.signing
		} else {
			self.rng.signing_key()
		};
		let space = if self.chance(50) {
			self.personal()
		} else {
			self.rng.space_id()
		};
		let number = u64::try_from(self.below(4)).expect("small");
		let holds = self.chance(50);
		nibkeep_governance::Choice::sign(&signing, space, number, holds).to_text()
	}

	/// An operation that the infected device writes.
	fn forged_operation(&mut self, governs: bool, known: &[(ChangeId, u64, u64)]) -> Operation {
		let personal = self.personal();
		// What it writes on: a note, a login, a replica's record, or something new.
		let replicas = self.infected.replicas().expect("lists");
		let entity = match self.below(3) {
			0 if !self.notes.is_empty() => {
				let index = self.below(self.notes.len());
				self.notes[index]
			}
			2 if !self.logins.is_empty() && self.chance(50) => {
				let index = self.below(self.logins.len());
				self.logins[index]
			}
			1 if !replicas.is_empty() => replicas[self.below(replicas.len())].id,
			_ => self.rng.entity_id(),
		};
		let governing = [
			fields::REVOKED,
			fields::SIGNING_KEY,
			fields::ENCRYPTION_KEY,
			fields::OWNER,
			fields::LISTING,
			fields::ROLE,
			fields::AI_AGENT,
			fields::SPACE_KEY,
			fields::ACCEPTED,
			fields::SPACE_CHOICE,
			field("org.nibkeep.later"),
		];
		let ordinary = [
			title(),
			tag(),
			password(),
			fields::NAME,
			fields::DELETED,
			fields::ACCEPTED,
			fields::REVOKED,
			field("local.other"),
		];
		let name = if governs || self.chance(10) {
			governing[self.below(governing.len())].clone()
		} else {
			ordinary[self.below(ordinary.len())].clone()
		};
		let value = if name == fields::REVOKED || name == fields::SIGNING_KEY {
			Value::Text(self.some_key())
		} else if name == fields::OWNER {
			Value::Reference(replicas.first().map_or(entity, |replica| replica.id))
		} else if name == fields::AI_AGENT || name == fields::DELETED {
			Value::Boolean(self.chance(50))
		} else if name == password() {
			Value::Text(format!("{PLAIN_WORD} {}", self.word()))
		} else if name == fields::ACCEPTED && !known.is_empty() {
			let (id, _, _) = known[self.below(known.len())];
			Value::Text(id.to_string())
		} else if name == fields::SPACE_CHOICE {
			Value::Text(self.some_choice())
		} else {
			Value::Text(self.word())
		};
		// Half the choices it would state become what a replica says of new spaces, on any
		// record, drawn from a stream of their own.
		let marks = self.marks.try_next_u64().expect("infallible");
		let (name, value) = if name == fields::SPACE_CHOICE && marks.is_multiple_of(2) {
			(fields::NEW_SPACES, Value::Boolean(marks.is_multiple_of(4)))
		} else {
			(name, value)
		};
		let current: Vec<StatementId> = self
			.infected
			.entity(personal, entity, this_day())
			.ok()
			.flatten()
			.map_or_else(Vec::new, |shown| {
				shown
					.values(&name)
					.iter()
					.map(|shown| shown.statement)
					.collect()
			});
		let statement = Statement::new(self.rng.statement_id(), entity, name, value);
		match self.below(10) {
			0 if !current.is_empty() => Operation::retract(entity, current),
			1 | 2 if !current.is_empty() => Operation::replace(statement, current),
			_ => Operation::assert(statement),
		}
	}

	/// What a forged change builds on: mostly the latest it knows, sometimes nothing, or a
	/// change that nobody holds. With its depth, and the time of its latest parent.
	fn forged_parents(&mut self, known: &[(ChangeId, u64, u64)]) -> (BTreeSet<ChangeId>, u64, u64) {
		let mut parents = BTreeSet::new();
		match self.below(20) {
			0 => {
				self.saw("a forged change had no parents");
				(parents, 0, self.at)
			}
			1 => {
				self.saw("a forged change built on one nobody holds");
				parents.insert(ChangeId::from_hash(Hash::of(&self.rng.bytes::<32>())));
				(parents, 1_000, self.at)
			}
			_ => {
				let (mut depth, mut latest) = (0, 0);
				for _ in 0..=self.below(2) {
					let index = if self.chance(60) {
						known.len() - 1 - self.below(known.len().min(3))
					} else {
						self.below(known.len())
					};
					let (id, millis, parent_depth) = known[index];
					parents.insert(id);
					depth = depth.max(parent_depth + 1);
					latest = latest.max(millis);
				}
				(parents, depth, latest)
			}
		}
	}

	/// When a forged change says it was made: mostly after its parents, sometimes not after
	/// them, or further ahead than a device takes.
	fn forged_time(&mut self, latest: u64) -> u64 {
		match self.below(16) {
			0 => latest,
			1 => {
				self.saw("a forged change was dated ahead");
				self.at + 2 * DRIFT
			}
			2 => latest.saturating_sub(10_000),
			3 => (latest + 1).max(self.at + DRIFT / 2),
			_ => (latest + 1).max(self.at.saturating_sub(5_000)),
		}
	}

	/// What a forged change purges, now and then: one of its parents, or any ordinary change
	/// that the infected device knows of.
	fn forged_purges(
		&mut self,
		parents: &BTreeSet<ChangeId>,
		known: &[(ChangeId, u64, u64)],
	) -> Purges {
		let mut purges = Purges::default();
		if !self.chance(25) {
			return purges;
		}
		let ids: BTreeSet<ChangeId> = known.iter().map(|(id, _, _)| *id).collect();
		let mut ordinary = ordinary_of(&mut self.infected, &ids);
		ordinary.extend(
			self.forged
				.iter()
				.filter(|forged| forged.shape != Shape::Governing)
				.map(|forged| forged.parcel.id()),
		);
		if ordinary.is_empty() {
			return purges;
		}
		let of_parents: Vec<ChangeId> = parents.intersection(&ordinary).copied().collect();
		let target = if !of_parents.is_empty() && self.chance(70) {
			of_parents[self.below(of_parents.len())]
		} else {
			*ordinary
				.iter()
				.nth(self.below(ordinary.len()))
				.expect("a change")
		};
		purges.changes.insert(target);
		purges
	}

	/// Signs a draft with the infected device's key, wrapped with its newest key of the space,
	/// mostly, or an old one, or one that is no key of the space, and sent whole, or now and
	/// then without its payload or key. Says whether a key of the space wraps it.
	fn seal(&mut self, draft: &Draft) -> (Parcel, bool, u64) {
		let (signing, mut keys) = replica_keys(
			self.infected.library_directory().expect("a library"),
			&Unlocker::Passphrase("battery staple"),
			self.personal(),
		);
		let mut readable = true;
		match self.below(10) {
			0 => {
				let (first, key) = keys.space_keys().next().expect("a key");
				keys = SpaceKeys::new(first, key.clone());
			}
			1 => {
				let next = keys.newest().0 + 1;
				keys = SpaceKeys::new(next, Key::generate(&mut self.rng));
				readable = false;
			}
			_ => {}
		}
		let personal = self.personal();
		let mut parcel = Change::create(personal, &keys, &signing.signing, draft, &mut self.rng);
		match self.below(25) {
			0 => {
				self.saw("a forged change came without its payload");
				parcel.payload = None;
			}
			1 => parcel.keys.clear(),
			_ => {}
		}
		(parcel, readable, keys.newest().0)
	}

	/// The honest devices whose keys the operations name as revoked.
	fn revoked_in(&self, operations: &[Operation]) -> Vec<usize> {
		(0..self.honest.len())
			.filter(|&device| {
				let key = Value::Text(self.keys[device].to_text());
				operations.iter().any(|operation| {
					matches!(operation, Operation::Assert { statement, .. }
						if statement.field == fields::REVOKED && statement.value == key)
				})
			})
			.collect()
	}

	/// The infected device makes up a change, signed with its own key.
	fn forge(&mut self) {
		// Mostly on what an honest device took, so that what it makes is not refused for
		// what it builds on alone.
		let all = self.chance(10);
		let known = self.known(all);
		let (parents, depth, latest) = self.forged_parents(&known);
		let millis = self.forged_time(latest);
		let governs = self.chance(35);
		// A purge holds nothing else, mostly, as an honest one holds only what it states
		// again.
		let purges = if governs {
			Purges::default()
		} else {
			self.forged_purges(&parents, &known)
		};
		let operations: Vec<Operation> = if purges.is_empty() || self.chance(20) {
			(0..=self.below(2))
				.map(|_| self.forged_operation(governs, &known))
				.collect()
		} else {
			Vec::new()
		};
		let revokes = self.revoked_in(&operations);
		let mut draft = Draft::new(
			parents.iter().copied(),
			Timestamp::new(millis, 0),
			Payload::from(operations),
		);
		if governs {
			draft.kind = Kind::Governing;
		}
		draft.purges = purges;
		if self.chance(4) && !known.is_empty() {
			draft
				.restores
				.changes
				.insert(known[self.below(known.len())].0);
		}
		let (parcel, readable, epoch) = self.seal(&draft);
		let shape = if governs {
			Shape::Governing
		} else if draft.purges.is_empty() {
			Shape::Ordinary
		} else {
			Shape::Purge
		};
		let written: Vec<String> = draft
			.payload
			.operations
			.iter()
			.map(|operation| match operation {
				Operation::Assert { statement, .. } => {
					format!("{} {:?}", statement.field, statement.value)
				}
				other => format!("{other:?}"),
			})
			.collect();
		let parcel_whole = parcel.payload.is_some() && !parcel.keys.is_empty();
		let sent = match (&parcel.payload, parcel.keys.is_empty(), readable) {
			(None, ..) => ", without its payload",
			(_, true, _) => ", without its key",
			(_, false, false) => ", with a key of its own",
			_ => "",
		};
		self.log.push(format!(
			"the infected device makes up {} {shape:?}, on {} parents, at {millis}{sent}: {}",
			parcel.id(),
			parents.len(),
			written.join("; ")
		));
		let beyond_an_agent = !draft.restores.is_empty()
			|| draft.payload.operations.iter().any(|operation| {
				matches!(operation, Operation::Assert { statement, .. }
					if !fields::ai_agent_may_write(statement))
			});
		// As the log reads a payload: a change that restores holds nothing else, an ordinary
		// one no statement that governs, and a governing one nothing else.
		let operations = &draft.payload.operations;
		let governing = |field: &FieldName| fields::is_governing(field);
		let fits = if governs {
			operations.iter().all(|operation| match operation {
				Operation::Assert { statement, .. } => {
					governing(&statement.field) || fields::is_later_governing(&statement.field)
				}
				Operation::Retract { .. } => true,
				_ => false,
			})
		} else if draft.restores.is_empty() {
			!operations.iter().any(|operation| {
				matches!(operation, Operation::Assert { statement, .. } if governing(&statement.field))
			})
		} else {
			operations.is_empty()
		};
		for device in revokes {
			self.revoking[device].insert(parcel.id());
		}
		self.forged.push(Forged {
			parcel,
			depth,
			shape,
			taken: false,
			whole: fits && readable && parcel_whole,
			epoch,
			beyond_an_agent,
		});
	}

	/// The infected device syncs with an honest one, sending what it made up.
	fn infected_sync(&mut self, device: usize, initiates: bool) {
		let at = self.tick();
		self.log.push(format!(
			"the infected device syncs with the {}",
			self.honest[device].name
		));
		let before = self.held(device);
		let knew = self.applies_revocation(device);
		let counted_revoked = self.counts_infected_revoked(device);
		// The infected device refuses, or hears, one that it revoked.
		let may_refuse =
			counted_revoked || self.revoked_itself(device) || !self.lists_infected(device);
		let space = self.personal();
		let Self {
			infected,
			forged,
			honest,
			withheld,
			..
		} = self;
		let honest = &mut honest[device].library;
		let (mut infected_end, mut honest_end) = pipes(infected.endpoint(), honest.endpoint());
		let now = move || at;
		let known = honest.checkout_contents().expect("reads");
		let infected_known = infected.checkout_contents().expect("reads");
		let (by_infected, by_honest) = std::thread::scope(|scope| {
			let by_infected = scope.spawn(|| {
				let mut holder = Infected {
					syncing: Syncing::new(infected, &now),
					space,
					forged,
					withholding: &withheld[device],
				};
				let synced = if initiates {
					nibkeep_sync::initiate(&mut infected_end, &mut holder)
				} else {
					nibkeep_sync::respond(&mut infected_end, &mut holder)
				};
				drop(holder);
				drop(infected_end);
				synced
			});
			let mut syncing = Syncing::new(honest, &now);
			let synced = if initiates {
				nibkeep_sync::respond(&mut honest_end, &mut syncing)
			} else {
				nibkeep_sync::initiate(&mut honest_end, &mut syncing)
			};
			drop(syncing);
			drop(honest_end);
			(by_infected.join().expect("the infected device ran"), synced)
		});
		let settled = by_honest
			.as_ref()
			.map(|synced| settle(honest, synced, &known, (at, device)));
		if let Ok(synced) = by_infected {
			let _ = settle(infected, &synced, &infected_known, (at, 2));
		}
		// A device hears one it counts as revoked that would stay by revoking back those that
		// revoked it, such as the infected device's key revoked by another that it listed for
		// itself: it sends it nothing, unless that one's revocation by the infected device's
		// key came, and it counts it no longer.
		let sent_nothing = by_honest.as_ref().is_ok_and(|synced| {
			synced.spaces.iter().all(|space| {
				space.sent.changes == 0 && space.sent.keys == 0 && space.sent.blobs == 0
			})
		});
		let name = self.honest[device].name;
		match settled {
			Ok(Ok(()))
				if counted_revoked && !sent_nothing && self.counts_infected_revoked(device) =>
			{
				self.fail(&format!(
					"the {name} sent the infected device, which it counts as revoked, what it lacked"
				));
			}
			Ok(Ok(())) if counted_revoked => {
				self.saw("an honest device heard the revoked infected one");
			}
			Ok(Ok(())) => {}
			// A device of the person's may revoke another, which then writes nothing more.
			Ok(Err(NodeError::Revoked)) if self.revoked_itself(device) => {
				self.saw("the infected device revoked an honest one");
			}
			Ok(Err(error)) => {
				self.fail(&format!(
					"settling after the infected device failed: {error}"
				));
			}
			// A device refuses one it counts as revoked, also once this sync made it so.
			// The infected device may end a sync, or break the protocol, as it likes.
			Err(
				nibkeep_sync::SyncError::Ended(_)
				| nibkeep_sync::SyncError::Connection(_)
				| nibkeep_sync::SyncError::Protocol(_),
			) => self.saw("the infected device ended a sync"),
			Err(_)
				if may_refuse
					|| self.counts_infected_revoked(device)
					|| self.revoked_itself(device)
					|| !self.lists_infected(device) =>
			{
				self.saw("an honest device refused the infected one");
			}
			Err(error) => self.fail(&format!(
				"the {name} failed to sync with the infected device: {error}"
			)),
		}
		self.after_infected_sync(device, (&before, knew), at);
	}

	/// What comes of a sync with the infected device: no device that held the revocation
	/// took anything from it, and the infected library takes what was taken of its own.
	fn after_infected_sync(
		&mut self,
		device: usize,
		(before, knew): (&BTreeSet<ChangeId>, bool),
		at: u64,
	) {
		let space = self.personal();
		let held = self.held(device);
		let mut untaken = Vec::new();
		let mut newly_taken = Vec::new();
		for forged in &mut self.forged {
			let id = forged.parcel.id();
			if held.contains(&id) {
				if !forged.taken {
					newly_taken.push(forged.parcel.clone());
				}
				forged.taken = true;
				if knew && !before.contains(&id) {
					untaken.push(id);
				}
			} else {
				untaken.push(id);
			}
		}
		// The infected library takes what an honest device took of what it made up, so that
		// it takes what honest devices build on it, as a device that turned against its user
		// keeps its own changes.
		let honest_key = self.keys[device];
		let now = || at;
		let mut syncing = Syncing::new(&mut self.infected, &now);
		for parcel in newly_taken {
			let _ = syncing.receive_change(space, &honest_key, parcel);
		}
		drop(syncing);
		for id in untaken {
			if held.contains(&id) {
				self.fail(&format!(
					"the {} took {id} from the infected device after it held its revocation",
					self.honest[device].name
				));
			}
			self.fate(id, "a forged change was not taken");
		}
		self.check_guarantees();
	}

	/// Whether an honest device counts the infected one as revoked.
	fn counts_infected_revoked(&self, device: usize) -> bool {
		self.counts_revoked(device, self.records[2])
	}

	/// Whether an honest device lists the infected one with the key it was paired with: the
	/// infected device may have taken it off its record, so that it is no replica of the
	/// library any more, and devices refuse it.
	fn lists_infected(&self, device: usize) -> bool {
		self.honest[device]
			.library
			.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.id == self.records[2] && replica.signing == Some(self.keys[2]))
	}

	/// Whether an honest device counts the replica of a record as revoked.
	fn counts_revoked(&self, device: usize, record: EntityId) -> bool {
		self.honest[device]
			.library
			.replicas()
			.expect("lists")
			.iter()
			.any(|replica| replica.id == record && replica.revoked)
	}

	fn step(&mut self, step: usize) {
		match self.below(100) {
			0..4 => self.create_login(),
			4..15 => self.create(),
			15..35 => self.edit(),
			35..52 => self.forge(),
			52..55 => self.infected_revoke(),
			55..70 => {
				let device = self.below(self.honest.len());
				let initiates = self.chance(50);
				self.infected_sync(device, initiates);
			}
			70..88 => self.honest_sync(0, 1),
			88..94 => self.restart(),
			_ if step > STEPS / 3 => {
				let device = self.below(self.honest.len());
				self.revoke(device);
			}
			_ => self.forge(),
		}
	}

	/// The end of a run: an honest device revokes the infected one, if none did, and the
	/// honest devices sync; every one that the revoker counts must then count it too, and
	/// every honest library verify as sound.
	fn finish(&mut self) {
		for device in 0..self.honest.len() {
			self.revoke(device);
		}
		for _ in 0..3 {
			self.honest_sync(0, 1);
			self.honest_sync(1, 0);
		}
		if let Some((by, knowing)) = self
			.revocation
			.as_ref()
			.map(|revocation| (revocation.by, revocation.knowing))
		{
			for device in 0..self.honest.len() {
				let counted = !self.counts_revoked(by, self.records[device]);
				if device == by || !counted || self.stand(device, by) {
					continue;
				}
				// The infected device's revocation of the revoker stands where the revocation
				// builds on it, or is the infected device's revocation of itself, made after it:
				// the revoker is revoked, and may not know it, as when that revocation came
				// without its payload (Known limits, Trust between devices).
				if knowing && self.counts_revoked(device, self.records[by]) {
					self.saw("a revocation built on one of the revoker");
					continue;
				}
				self.fail(&format!(
					"the {} counts the {}, which does not count it: their revocations did not meet",
					self.honest[by].name, self.honest[device].name
				));
			}
		}
		// Else the infected device revoked both honest ones before either revoked it, as a
		// device of the person's may.
		for device in 0..self.honest.len() {
			let now = self.tick();
			let report = self.honest[device].library.verify(now);
			match report {
				Ok(report) => check!(
					self,
					report.is_sound(),
					"verify found problems on the {}: {report:?}",
					self.honest[device].name
				),
				Err(error) => self.fail(&format!(
					"the {} failed to verify: {error}",
					self.honest[device].name
				)),
			}
		}
	}

	/// The folders of the run's libraries.
	fn folders(&self) -> Vec<std::path::PathBuf> {
		self.honest
			.iter()
			.map(|device| &device.library)
			.chain([&self.infected])
			.map(|library| library.library_directory().expect("a library").to_owned())
			.collect()
	}
}

/// The laptop, which makes the library, with its types of notes and logins, and its
/// secrets set up and unlocked.
fn laptop(name: &str, rng: &mut TestRng) -> Library {
	let (_, mut laptop) = common::library(&format!("{name}, laptop"), rng);
	let personal = laptop.personal_space();
	let text = || FieldDef::new(nibkeep_model::ValueKind::Text);
	let note = TypeDef::new(NOTE, 1)
		.with_field(title(), text())
		.with_field(
			tag(),
			text().with_multiplicity(nibkeep_model::Multiplicity::Many),
		);
	let login = TypeDef::new(LOGIN, 1)
		.with_field(title(), text())
		.with_field(password(), text().secret());
	for (at, definition) in [(now(), note), (now() + 100, login)] {
		laptop
			.define_type(personal, &definition, at, rng)
			.expect("defines");
	}
	laptop
		.set_up_secrets(
			&NewProtection::Passphrase(SECRETS, PassphraseParams::new(64, 1, 1).expect("valid")),
			now() + 500,
			rng,
		)
		.expect("sets up the secrets");
	laptop
}

/// An AI agent's replica, paired with the laptop.
fn ai_agent(laptop: &mut Library, folder: &str, rng: &mut TestRng) -> Library {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), folder);
	let code = PairingCode::generate(rng);
	let (paired, joined) = pair_ai_agent_named(laptop, &directory, ("desk", "Agent"), &code, rng);
	paired.expect("paired");
	joined.expect("joined").library
}

/// A device of the person's, paired with the laptop and given the secrets.
fn paired(laptop: &mut Library, folder: &str, device: &str, rng: &mut TestRng) -> Library {
	let directory = nibkeep_testkit::empty_directory(env!("CARGO_TARGET_TMPDIR"), folder);
	let code = PairingCode::generate(rng);
	let joining = Joining::new(device, "nib", rng).expect("names");
	let (paired, joined) = pair_joining(laptop, &directory, (joining, true), (&code, &code), rng);
	paired.expect("paired");
	joined.expect("joined").library
}

/// The changes a library holds in its personal space, with their times and depths.
fn changes_of(library: &mut Library) -> Vec<(ChangeId, u64, u64)> {
	let personal = library.personal_space();
	let now = || 0;
	Syncing::new(library, &now)
		.holdings(personal)
		.expect("lists")
		.into_iter()
		.filter_map(|holding| match holding {
			Holding::Change {
				id, millis, depth, ..
			} => Some((id, millis, depth)),
			_ => None,
		})
		.collect()
}

/// The ordinary changes, those that a purge may name, of those a library holds.
fn ordinary_of(library: &mut Library, ids: &BTreeSet<ChangeId>) -> BTreeSet<ChangeId> {
	let personal = library.personal_space();
	let now = || 0;
	let syncing = Syncing::new(library, &now);
	ids.iter()
		.copied()
		.filter(|id| {
			syncing
				.parcel(personal, *id)
				.expect("reads")
				.is_some_and(|parcel| parcel.change.kind() == Kind::Ordinary)
		})
		.collect()
}

/// What an honest library shows, a line for each thing, to compare: every entity of the
/// personal space and the deleted ones, but their passwords, the changes set aside, and the
/// replicas, sorted.
fn view(library: &Library) -> Vec<String> {
	let personal = library.personal_space();
	let mut lines = Vec::new();
	let listed = library.entities(personal, None).expect("lists");
	let deleted = library.deleted(personal).expect("lists");
	for entity in listed.iter().chain(&deleted) {
		let mut shown = library
			.entity(personal, entity.id, this_day())
			.expect("reads");
		// Passwords show only where the secrets are unlocked.
		if let Some(shown) = &mut shown {
			shown.fields.retain(|shown| shown.field != password());
		}
		lines.push(format!("{shown:?}"));
	}
	for set_aside in library.set_aside().expect("lists") {
		lines.push(format!(
			"set aside: {} by {:?}, acceptable: {}",
			set_aside.change, set_aside.replica, set_aside.acceptable
		));
	}
	for replica in library.replicas().expect("lists") {
		lines.push(format!(
			"replica {} {:?}: revoked {}, deleted {}, AI agent {}, secrets {}, set aside {}",
			replica.id,
			replica.signing.map(|key| key.to_text()),
			replica.revoked,
			replica.deleted,
			replica.ai_agent,
			replica.holds_secrets,
			replica.set_aside
		));
	}
	// Lists may order alike things differently.
	lines.sort();
	lines
}

/// Two lines of a view cut to where they first differ, with what comes before it, as views
/// hold keys thousands of characters long.
fn where_they_differ(left: &str, right: &str) -> (String, String) {
	let same = left
		.char_indices()
		.zip(right.chars())
		.find(|((_, a), b)| a != b)
		.map_or_else(|| left.len().min(right.len()), |((at, _), _)| at);
	let cut = |line: &str| {
		let start = line.floor_char_boundary(same.saturating_sub(160));
		let end = line.floor_char_boundary((same + 240).min(line.len()));
		format!("…{}…", &line[start..end])
	};
	(cut(left), cut(right))
}

/// A sync as the infected device runs it: with its library's keys, holding what its library
/// holds and what it made up, and sending all of it.
struct Infected<'a> {
	syncing: Syncing<'a>,
	space: SpaceId,
	forged: &'a [Forged],
	/// The changes it sends without their payloads.
	withholding: &'a BTreeSet<ChangeId>,
}

impl Holder for Infected<'_> {
	type Error = NodeError;

	fn verifying_key(&self) -> VerifyingKey {
		self.syncing.verifying_key()
	}

	fn sign(&self, context: &'static str, message: &[u8]) -> Signature {
		self.syncing.sign(context, message)
	}

	fn spaces_with(&self, peer: &VerifyingKey) -> Result<Vec<SpaceId>, NodeError> {
		self.syncing.spaces_with(peer)
	}

	fn introductions(&self, peer: &[u8; 32]) -> Result<Vec<RecoveryListing>, NodeError> {
		self.syncing.introductions(peer)
	}

	fn introduced(
		&mut self,
		peer: &VerifyingKey,
		listings: &[RecoveryListing],
	) -> Result<(), NodeError> {
		self.syncing.introduced(peer, listings)
	}

	fn prepare(&mut self, space: SpaceId) -> Result<(), NodeError> {
		self.syncing.prepare(space)
	}

	fn holdings(&self, space: SpaceId) -> Result<Vec<Holding>, NodeError> {
		let mut holdings = self.syncing.holdings(space)?;
		if space == self.space {
			let held: BTreeSet<ChangeId> = holdings
				.iter()
				.filter_map(|holding| match holding {
					Holding::Change { id, .. } => Some(*id),
					_ => None,
				})
				.collect();
			let forged = self
				.forged
				.iter()
				.filter(|forged| !held.contains(&forged.parcel.id()));
			holdings.extend(forged.map(|forged| Holding::Change {
				id: forged.parcel.id(),
				millis: forged.parcel.change.timestamp().millis(),
				depth: forged.depth,
				complete: forged.parcel.payload.is_some() && !forged.parcel.keys.is_empty(),
			}));
		}
		Ok(holdings)
	}

	fn sends(
		&self,
		space: SpaceId,
		peer: &VerifyingKey,
		holding: &Holding,
	) -> Result<bool, NodeError> {
		if let Holding::Change { id, .. } = holding
			&& self.forged.iter().any(|forged| forged.parcel.id() == *id)
		{
			return Ok(true);
		}
		self.syncing.sends(space, peer, holding)
	}

	fn wanted_blobs(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		asked: &BTreeSet<Hash>,
		most: usize,
	) -> Result<Vec<Hash>, NodeError> {
		self.syncing.wanted_blobs(space, peer, asked, most)
	}

	fn sends_blobs(&self, space: SpaceId, peer: &VerifyingKey) -> Result<bool, NodeError> {
		self.syncing.sends_blobs(space, peer)
	}

	fn challenges(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
	) -> Result<Vec<Challenge>, NodeError> {
		self.syncing.challenges(space, peer)
	}

	fn proved(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
		answers: &[Option<[u8; 32]>],
	) -> Result<(), NodeError> {
		self.syncing.proved(space, peer, challenges, answers)
	}

	fn prove(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		challenges: &[Challenge],
	) -> Result<Vec<Option<[u8; 32]>>, NodeError> {
		self.syncing.prove(space, peer, challenges)
	}

	fn parcel(&self, space: SpaceId, id: ChangeId) -> Result<Option<Parcel>, NodeError> {
		if let Some(forged) = self.forged.iter().find(|forged| forged.parcel.id() == id) {
			return Ok(Some(forged.parcel.clone()));
		}
		let mut parcel = self.syncing.parcel(space, id)?;
		if self.withholding.contains(&id)
			&& let Some(parcel) = &mut parcel
		{
			parcel.payload = None;
		}
		Ok(parcel)
	}

	fn blob(
		&self,
		space: SpaceId,
		sealed: &Hash,
	) -> Result<Option<(u64, BlobBytes<'_>)>, NodeError> {
		self.syncing.blob(space, sealed)
	}

	fn receive_change(
		&mut self,
		space: SpaceId,
		peer: &VerifyingKey,
		parcel: Parcel,
	) -> Result<bool, NodeError> {
		self.syncing.receive_change(space, peer, parcel)
	}

	fn receive_record_key(
		&mut self,
		space: SpaceId,
		statement: StatementId,
		key: WrappedKey,
	) -> Result<bool, NodeError> {
		self.syncing.receive_record_key(space, statement, key)
	}

	fn receive_blob_key(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		key: WrappedBlobKey,
	) -> Result<bool, NodeError> {
		self.syncing.receive_blob_key(space, sealed, key)
	}

	fn receive_blob(
		&mut self,
		space: SpaceId,
		sealed: Hash,
		size: u64,
		bytes: &mut dyn Read,
	) -> Result<bool, NodeError> {
		self.syncing.receive_blob(space, sealed, size, bytes)
	}

	fn account(&self, space: SpaceId, id: &Hash) -> Result<Option<Vec<u8>>, NodeError> {
		self.syncing.account(space, id)
	}

	fn receive_account(&mut self, space: SpaceId, page: Vec<u8>) -> Result<bool, NodeError> {
		self.syncing.receive_account(space, page)
	}

	fn finish_space(&mut self, space: SpaceId) -> Result<(), NodeError> {
		self.syncing.finish_space(space)
	}

	fn pause(&mut self) -> Result<(), NodeError> {
		self.syncing.pause()
	}
}

fn simulate(seed: u64, steps: usize) -> BTreeMap<&'static str, usize> {
	// A panic that does not go through the run's report, such as the engine's, still names
	// the seed.
	eprintln!("infected simulation seed {seed}");
	let mut run = Run::new(seed);
	for step in 0..steps {
		run.step(step);
	}
	run.finish();
	let folders = run.folders();
	let situations = std::mem::take(&mut run.seen);
	drop(run);
	for folder in folders {
		let _ = std::fs::remove_dir_all(folder);
	}
	situations
}

#[test]
fn honest_devices_keep_their_guarantees_whatever_an_infected_one_sends() {
	let variable = |name: &str| {
		std::env::var(name)
			.ok()
			.map(|value| value.parse::<u64>().expect("a number"))
	};
	if let Some(seed) = variable("NIBKEEP_SIMULATION_SEED") {
		simulate(seed, STEPS);
		return;
	}
	let mut situations: BTreeMap<&str, usize> = BTreeMap::new();
	let runs = variable("NIBKEEP_SIMULATION_RUNS").unwrap_or(RUNS);
	let first = variable("NIBKEEP_SIMULATION_FIRST_SEED")
		.or_else(|| variable("NIBKEEP_SIMULATION_RUN").map(|run| run * runs))
		.unwrap_or(0);
	// The runs go side by side, a few at a time, each in folders of its own.
	let next = std::sync::atomic::AtomicU64::new(first);
	let workers = std::thread::available_parallelism()
		.map_or(1, usize::from)
		.min(8);
	let reports: Vec<Vec<BTreeMap<&str, usize>>> = std::thread::scope(|scope| {
		let handles: Vec<_> = (0..workers)
			.map(|_| {
				scope.spawn(|| {
					let mut reports = Vec::new();
					loop {
						let seed = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						if seed >= first + runs {
							return reports;
						}
						reports.push(simulate(seed, STEPS));
					}
				})
			})
			.collect();
		handles
			.into_iter()
			.map(|handle| {
				handle
					.join()
					.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
			})
			.collect()
	});
	for (situation, count) in reports.into_iter().flatten().flatten() {
		*situations.entry(situation).or_default() += count;
	}
	println!("situations: {situations:#?}");
	let missed: Vec<&str> = SITUATIONS
		.into_iter()
		.filter(|situation| !situations.contains_key(situation))
		.collect();
	assert!(missed.is_empty(), "the runs never came to: {missed:?}");
}
