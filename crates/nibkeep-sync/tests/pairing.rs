//! Pairing over a connection: the right code grants the joiner the space keys, and a wrong
//! one fails on both sides and grants nothing. The device that shows the code proves it
//! first, so a joiner tells nothing of itself to one that does not know it.

mod common;

use std::collections::BTreeMap;
use std::convert::Infallible;

use nibkeep_base::cbor::{self, FromCbor, ToCbor, Value};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_crypto::{DecryptionKey, Key, SigningKey};
use nibkeep_governance::{
	Grant, InvitationGrant, InvitationRequest, JoinRequest, Offer, Pairing, PairingCode, Side,
	open_space_keys, seal_space_keys,
};
use nibkeep_sync::{Accepting, Reason, Sponsoring, SyncError, Transport};
use nibkeep_testkit::TestRng;

/// A sponsor in memory, which records whom it added.
struct Library {
	key: SigningKey,
	spaces: BTreeMap<SpaceId, BTreeMap<u64, Key>>,
	granted: Vec<(JoinRequest, [u8; 32])>,
	added: Vec<(JoinRequest, [u8; 32])>,
}

impl Sponsoring for Library {
	type Error = Infallible;

	fn grant<R: CryptoRng>(
		&mut self,
		request: &JoinRequest,
		endpoint: &[u8; 32],
		rng: &mut R,
	) -> Result<Grant, Infallible> {
		self.granted.push((request.clone(), *endpoint));
		let replica = EntityId::random(rng);
		Ok(Grant {
			user: EntityId::random(rng),
			device: EntityId::random(rng),
			replica,
			space: *self.spaces.keys().next().expect("a space"),
			sponsor: self.key.verifying_key(),
			keys: seal_space_keys(&request.encryption, replica, &self.spaces, rng),
			ai_agent: false,
			chosen: Vec::new(),
			asks_first: false,
		})
	}

	fn record<R: CryptoRng>(&mut self, _: &[String], _: &mut R) -> Result<(), Infallible> {
		self.added.extend(self.granted.pop());
		Ok(())
	}
}

/// A sponsor in memory that holds one space, with one key.
fn library(rng: &mut TestRng) -> Library {
	Library {
		key: SigningKey::generate(rng),
		spaces: BTreeMap::from([(
			SpaceId::random(rng),
			BTreeMap::from([(0, Key::generate(rng))]),
		)]),
		granted: Vec::new(),
		added: Vec::new(),
	}
}

/// A joiner's request, which names its device and app.
fn request(rng: &mut TestRng) -> JoinRequest {
	JoinRequest {
		signing: SigningKey::generate(rng).verifying_key(),
		encryption: DecryptionKey::generate(rng).encryption_key(),
		device: "Hidden laptop".to_owned(),
		app: "Hidden app".to_owned(),
		secrets: None,
	}
}

/// Pairs a joiner that typed `typed` with a sponsor that showed `shown`; the joiner sends
/// its listing in the personal space once it made its library.
fn pair(
	shown: &str,
	typed: &str,
) -> (
	Library,
	DecryptionKey,
	Result<nibkeep_sync::Sponsored, SyncError>,
	Result<Grant, SyncError>,
) {
	pair_listed(shown, typed, vec!["its listing".to_owned()])
}

/// Pairs as [`pair`] does, the joiner sending `listings` once it made its library.
fn pair_listed(
	shown: &str,
	typed: &str,
	listings: Vec<String>,
) -> (
	Library,
	DecryptionKey,
	Result<nibkeep_sync::Sponsored, SyncError>,
	Result<Grant, SyncError>,
) {
	let mut rng = TestRng::new(&format!("{shown} {typed}"));
	let mut library = Library {
		key: SigningKey::generate(&mut rng),
		spaces: BTreeMap::from([(
			SpaceId::random(&mut rng),
			BTreeMap::from([(0, Key::generate(&mut rng)), (1, Key::generate(&mut rng))]),
		)]),
		granted: Vec::new(),
		added: Vec::new(),
	};
	let decryption = DecryptionKey::generate(&mut rng);
	let request = JoinRequest {
		signing: SigningKey::generate(&mut rng).verifying_key(),
		encryption: decryption.encryption_key(),
		device: "laptop".to_owned(),
		app: "nib".to_owned(),
		secrets: None,
	};
	let (shown, typed): (PairingCode, PairingCode) = (
		shown.parse().expect("a code"),
		typed.parse().expect("a code"),
	);
	let (mut sponsor_end, mut joiner_end) = common::pipes([1; 32], [2; 32], [9; 32], None);
	let mut joiner_rng = TestRng::new("joiner");
	let (sponsored, joined) = std::thread::scope(|scope| {
		let joiner = scope.spawn(|| {
			let granted = nibkeep_sync::join(&mut joiner_end, &request, &typed, &mut joiner_rng);
			let granted = granted.and_then(|grant| {
				nibkeep_sync::joined(&mut joiner_end, grant, listings).map(|joined| joined.grant)
			});
			drop(joiner_end);
			granted
		});
		let sponsored = nibkeep_sync::sponsor(&mut sponsor_end, &mut library, &shown, &mut rng);
		drop(sponsor_end);
		(sponsored, joiner.join().expect("the joiner ran"))
	});
	(library, decryption, sponsored, joined)
}

#[test]
fn the_right_code_grants_the_space_keys() {
	let (library, decryption, sponsored, joined) = pair("37-482-177", "37482177");
	let sponsored = sponsored.expect("the sponsor paired");
	let grant = joined.expect("the joiner paired");
	assert_eq!(sponsored.grant, grant);
	assert_eq!(library.added.len(), 1);
	assert_eq!(library.added[0].1, [2; 32], "the joiner's endpoint");
	assert_eq!(grant.sponsor, library.key.verifying_key());
	let keys = open_space_keys(&decryption, grant.replica, &grant.keys).expect("opens");
	assert_eq!(keys.len(), 1);
	for (space, epochs) in &library.spaces {
		for (epoch, key) in epochs {
			assert!(keys[space][epoch].same_as(key));
		}
	}
}

#[test]
fn a_joiner_that_sends_no_listing_of_its_own_is_not_recorded() {
	let (library, _, sponsored, _) = pair_listed("37-482-177", "37482177", Vec::new());
	assert!(matches!(sponsored, Err(SyncError::Protocol(_))));
	nibkeep_testkit::assert_empty!(library.added);
}

#[test]
fn a_wrong_code_fails_on_both_sides_and_adds_nothing() {
	let (library, _, sponsored, joined) = pair("37-482-177", "37-482-178");
	assert!(
		matches!(sponsored, Err(SyncError::WrongCode)),
		"{sponsored:?}"
	);
	assert!(matches!(joined, Err(SyncError::WrongCode)), "{joined:?}");
	assert_eq!(library.granted, []);
	assert_eq!(library.added, []);
}

#[test]
fn a_joiner_that_does_not_join_is_not_recorded() {
	let mut rng = TestRng::new("does not join");
	let mut library = library(&mut rng);
	let request = request(&mut rng);
	let code: PairingCode = "37-482-177".parse().expect("a code");
	let (mut sponsor_end, mut joiner_end) = common::pipes([1; 32], [2; 32], [9; 32], None);
	let mut joiner_rng = TestRng::new("joiner");
	let sponsored = std::thread::scope(|scope| {
		let joiner = scope.spawn(|| {
			let granted = nibkeep_sync::join(&mut joiner_end, &request, &code, &mut joiner_rng);
			// The joiner fails to make its library, and says so.
			let _ = nibkeep_sync::refuse(&mut joiner_end, Reason::Failed);
			drop(joiner_end);
			granted
		});
		let sponsored = nibkeep_sync::sponsor(&mut sponsor_end, &mut library, &code, &mut rng);
		drop(sponsor_end);
		joiner.join().expect("the joiner ran").expect("granted");
		sponsored
	});
	assert!(
		matches!(sponsored, Err(SyncError::Ended(Reason::Failed))),
		"{sponsored:?}"
	);
	assert_eq!(library.granted.len(), 1);
	assert_eq!(library.added, []);
}

#[test]
fn reasons_name_what_went_wrong() {
	assert_eq!(
		SyncError::Ended(Reason::Busy).to_string(),
		"the other device ended the connection: it is syncing with another replica"
	);
}

/// How a device that announces a code's first two digits, but does not know the code,
/// answers a joiner's message of SPAKE2.
#[derive(Clone, Copy, Debug)]
enum Answer {
	/// With its own message for the code it guessed, and the tag that the key it got gives.
	Guessed,
	/// With its own message for the code it guessed, and no tag.
	Untagged,
}

/// Runs `joiner` against a device that does not know the code, which plays the other side
/// with `guess` for it, pairing or, if `invitation`, inviting, and answers as `answer` says.
/// Returns what the joiner returned and every message it sent after the device answered,
/// until it hung up.
fn against_impostor<T: Send>(
	guess: &PairingCode,
	invitation: bool,
	answer: Answer,
	joiner: impl FnOnce(&mut common::Pipe) -> Result<T, SyncError> + Send,
) -> (Result<T, SyncError>, Vec<Vec<u8>>) {
	let (mut end, mut joiner_end) = common::pipes([1; 32], [2; 32], [9; 32], None);
	std::thread::scope(|scope| {
		let joiner = scope.spawn(move || {
			let result = joiner(&mut joiner_end);
			drop(joiner_end);
			result
		});
		let mut rng = TestRng::new("impostor");
		let first =
			cbor::decode(&end.receive().expect("the joiner's first message")).expect("CBOR");
		let Value::Array(items) = first else {
			panic!("an array");
		};
		// Only its message of SPAKE2, which tells nothing of the joiner.
		let [kind, Value::Bytes(theirs)] = items.as_slice() else {
			panic!("[1, message] from the joiner");
		};
		assert_eq!(kind, &1_u8.to_cbor());
		let (pairing, ours) = Pairing::start(Side::Sponsor, guess, &mut rng);
		let binding = end.binding();
		let key = if invitation {
			pairing.finish_invitation(theirs, &binding)
		} else {
			pairing.finish(theirs, &binding)
		}
		.expect("the joiner's message");
		let mut message = vec![1_u8.to_cbor(), Value::Bytes(ours.clone())];
		if let Answer::Guessed = answer {
			let tag = key.tag(Side::Sponsor, &Value::Bytes(ours));
			message.push(Value::Bytes(tag.to_vec()));
		}
		end.send(&cbor::encode(&Value::Array(message)))
			.expect("sent");
		let sent = std::iter::from_fn(|| end.receive().ok()).collect();
		(joiner.join().expect("the joiner ran"), sent)
	})
}

/// The number of the reason in `message`, if it is `[0, reason]`, which ends the
/// connection: 1 for a code that did not match, 4 for a failure on the side that ended.
fn ending(message: &[u8]) -> Option<u64> {
	let Ok(Value::Array(items)) = cbor::decode(message) else {
		return None;
	};
	match items.as_slice() {
		[kind, reason] if kind == &0_u8.to_cbor() => u64::from_cbor(reason).ok(),
		_ => None,
	}
}

/// Checks that a joiner that met a device answering as `answer` failed as it should, and
/// sent nothing but the end of the connection, which holds none of `secrets`.
fn ended_unproven<T: std::fmt::Debug>(
	answer: Answer,
	result: &Result<T, SyncError>,
	sent: &[Vec<u8>],
	secrets: &[&[u8]],
) {
	let (failed, reason) = match answer {
		// A wrong code: the joiner says so, and the device counts it as a wrong try.
		Answer::Guessed => (matches!(result, Err(SyncError::WrongCode)), 1),
		// No proof at all breaks the protocol.
		Answer::Untagged => (matches!(result, Err(SyncError::Protocol(_))), 4),
	};
	assert!(failed, "{answer:?}: {result:?}");
	assert_eq!(
		sent.iter().map(|m| ending(m)).collect::<Vec<_>>(),
		[Some(reason)],
		"{answer:?}"
	);
	for secret in secrets {
		assert!(
			!sent.iter().any(|message| message
				.windows(secret.len())
				.any(|window| window == *secret)),
			"{answer:?}"
		);
	}
}

#[test]
fn a_joiner_tells_nothing_of_itself_to_a_device_that_does_not_prove_the_code() {
	let typed: PairingCode = "37-482-177".parse().expect("a code");
	let guess: PairingCode = "37-482-178".parse().expect("a code");
	for answer in [Answer::Guessed, Answer::Untagged] {
		let mut rng = TestRng::new("joiner");
		let request = request(&mut rng);
		let (joined, sent) = against_impostor(&guess, false, answer, |end| {
			nibkeep_sync::join(end, &request, &typed, &mut rng)
		});
		let encryption = request.encryption.to_bytes();
		let secrets: [&[u8]; 4] = [
			b"Hidden laptop",
			b"Hidden app",
			request.signing.as_bytes(),
			&encryption,
		];
		ended_unproven(answer, &joined, &sent, &secrets);
	}
}

/// The joiner's side of an invitation that never gets as far as an offer.
struct NeverOffered;

impl Accepting for NeverOffered {
	type Error = Infallible;

	fn listing(&mut self, _: &Offer) -> Result<Option<String>, Infallible> {
		panic!("no offer comes from a device that does not prove the code");
	}

	fn take<R: CryptoRng>(
		&mut self,
		_: &Offer,
		_: &InvitationGrant,
		_: &mut R,
	) -> Result<(), Infallible> {
		panic!("no grant comes from a device that does not prove the code");
	}
}

#[test]
fn an_invited_user_tells_nothing_of_themselves_to_a_device_that_does_not_prove_the_code() {
	let typed: PairingCode = "37-482-177".parse().expect("a code");
	let guess: PairingCode = "37-482-178".parse().expect("a code");
	for answer in [Answer::Guessed, Answer::Untagged] {
		let mut rng = TestRng::new("invited");
		let pairing = request(&mut rng);
		let request = InvitationRequest {
			user: EntityId::random(&mut rng),
			user_name: "Hidden name".to_owned(),
			signing: pairing.signing,
			encryption: pairing.encryption,
			device: pairing.device,
			app: pairing.app,
		};
		let (accepted, sent) = against_impostor(&guess, true, answer, |end| {
			nibkeep_sync::accept(end, &request, &mut NeverOffered, &typed, &mut rng)
		});
		let encryption = request.encryption.to_bytes();
		let secrets: [&[u8]; 6] = [
			b"Hidden name",
			b"Hidden laptop",
			b"Hidden app",
			request.user.as_bytes(),
			request.signing.as_bytes(),
			&encryption,
		];
		ended_unproven(answer, &accepted, &sent, &secrets);
	}
}

#[test]
fn a_sponsor_takes_no_proof_from_a_joiner() {
	let mut rng = TestRng::new("proving joiner");
	let mut library = library(&mut rng);
	let code: PairingCode = "37-482-177".parse().expect("a code");
	let (mut sponsor_end, mut joiner_end) = common::pipes([1; 32], [2; 32], [9; 32], None);
	let (_, ours) = Pairing::start(Side::Joiner, &code, &mut TestRng::new("joiner"));
	// The joiner's message of SPAKE2 with a tag, which only the sponsor's answer carries.
	let message = vec![
		1_u8.to_cbor(),
		Value::Bytes(ours),
		Value::Bytes(vec![3; 32]),
	];
	joiner_end
		.send(&cbor::encode(&Value::Array(message)))
		.expect("sent");
	let (sponsored, answer) = std::thread::scope(|scope| {
		let sponsor = scope.spawn(|| {
			let sponsored = nibkeep_sync::sponsor(&mut sponsor_end, &mut library, &code, &mut rng);
			drop(sponsor_end);
			sponsored
		});
		let answer = joiner_end.receive().expect("the sponsor's answer");
		// So that a sponsor that answered otherwise stops waiting.
		drop(joiner_end);
		(sponsor.join().expect("the sponsor ran"), answer)
	});
	assert!(
		matches!(sponsored, Err(SyncError::Protocol(_))),
		"{sponsored:?}"
	);
	assert_eq!(ending(&answer), Some(4));
	assert_eq!(library.granted, []);
}
