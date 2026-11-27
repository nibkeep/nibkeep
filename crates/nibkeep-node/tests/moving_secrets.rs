//! Moves of secret values between spaces ([technical design: sharing]): a move brings the secret
//! values of what it moves, past ones too, sealed with the other space's secrets key, only
//! with the secrets of both spaces unlocked, or leaves them behind; a field secret in the old
//! space stays secret in the new one; secret files move with their content; and what a move
//! leaves behind loses only the secret values that moved, once the mover's device sees them.
//!
//! [technical design: sharing]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#13-sharing

use crate::common;

use std::io::Read as _;

use crate::common::{
	LOGIN, invite_member, login_type, now, pair_named, sync_in_memory, this_day, user_library,
};
use nibkeep_base::{EntityId, SpaceId};
use nibkeep_crypto::PassphraseParams;
use nibkeep_model::{FieldDef, FieldName, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{
	Behind, Library, MemberRole, MoveHistory, MoveSecrets, NewProtection, NodeError, PairingCode,
	Unlocker,
};
use nibkeep_testkit::TestRng;

const SECRETS: &str = "tr0ub4dor";
const NOTE: TypeName = TypeName::from_static("local.note");
const SAFE_NOTE: TypeName = TypeName::from_static("local.safe-note");

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

fn text(text: &str) -> Value {
	Value::Text(text.to_owned())
}

fn title() -> FieldName {
	field("local.title")
}

fn password() -> FieldName {
	field("local.password")
}

fn body() -> FieldName {
	field("local.body")
}

fn protected(passphrase: &str) -> NewProtection<'_> {
	NewProtection::Passphrase(passphrase, PassphraseParams::new(64, 1, 1).expect("valid"))
}

fn unlock(library: &mut Library) {
	library
		.unlock_secrets(&Unlocker::Passphrase(SECRETS))
		.expect("unlocks");
}

/// The values that an entity of a space shows today in a field.
fn shown(library: &Library, (space, entity): (SpaceId, EntityId), field: &FieldName) -> Vec<Value> {
	library
		.entity(space, entity, this_day())
		.expect("reads")
		.map_or_default(|entity| {
			entity
				.values(field)
				.iter()
				.map(|shown| shown.value.clone())
				.collect()
		})
}

/// The values that an entity of a space has had in a field, current or past, as its history
/// shows them, sorted.
fn ever(library: &Library, (space, entity): (SpaceId, EntityId), field: &FieldName) -> Vec<Value> {
	let Some(history) = library.history(space, entity, Some(field)).expect("reads") else {
		return Vec::new();
	};
	let mut values: Vec<Value> = history
		.fields
		.iter()
		.flat_map(|field| &field.values)
		.map(|value| value.value.clone())
		.collect();
	values.sort_by_key(|value| format!("{value:?}"));
	values
}

/// Whether an entity of a space was ever marked as moved, by a move that came or not.
fn marked(library: &Library, (space, entity): (SpaceId, EntityId)) -> bool {
	!ever(library, (space, entity), &fields::MOVED_INTO).is_empty()
}

/// Moves an entity, with its history, keeping what it leaves behind.
fn move_keeping(
	library: &mut Library,
	(from, entity, to): (SpaceId, EntityId, SpaceId),
	secrets: MoveSecrets,
	at: u64,
	rng: &mut TestRng,
) -> Result<EntityId, NodeError> {
	library.move_entity(
		from,
		entity,
		to,
		MoveHistory::Bring,
		Behind::Keep,
		secrets,
		at,
		rng,
	)
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

/// Adds a login titled `name` to a space whose types have logins, with the passwords given,
/// each replacing the one before.
fn login(
	library: &mut Library,
	space: SpaceId,
	name: &str,
	passwords: &[&str],
	at: u64,
	rng: &mut TestRng,
) -> EntityId {
	let login = library
		.create_entity(space, &[LOGIN], vec![(title(), text(name))], at, rng)
		.expect("adds");
	for (minute, value) in (1..).zip(passwords) {
		library
			.edit_entity(
				space,
				login,
				vec![nibkeep_node::Edit::Set(password(), vec![text(value)])],
				&common::today(),
				at + minute * 60_000,
				rng,
			)
			.expect("sets the password");
	}
	login
}

/// Defines, in a space, notes whose body is plain, and adds one titled "Safe" whose body is
/// "1234"; then defines safe notes, whose body is secret, which makes the body secret for
/// good there, and leaves the note's plain body as it was written.
fn note_made_secret(library: &mut Library, space: SpaceId, at: u64, rng: &mut TestRng) -> EntityId {
	let note = TypeDef::new(NOTE, 1)
		.with_field(title(), FieldDef::new(ValueKind::Text))
		.with_field(body(), FieldDef::new(ValueKind::Text));
	library.define_type(space, &note, at, rng).expect("defines");
	let safe = library
		.create_entity(
			space,
			&[NOTE],
			vec![(title(), text("Safe")), (body(), text("1234"))],
			at + 100,
			rng,
		)
		.expect("adds");
	let safe_note =
		TypeDef::new(SAFE_NOTE, 1).with_field(body(), FieldDef::new(ValueKind::Text).secret());
	library
		.define_type(space, &safe_note, at + 200, rng)
		.expect("defines");
	assert!(
		library
			.secret_fields(space)
			.expect("a space")
			.contains(&body())
	);
	safe
}

/// Ada's laptop, its secrets set up and unlocked, with a household space and, in the personal
/// space, a login titled "Bank" whose password was "hunter2" and is "correct horse".
fn laptop_with_login(rng: &mut TestRng) -> (Library, SpaceId, SpaceId, EntityId) {
	let mut laptop = user_library("Ada", "laptop", rng);
	laptop
		.set_up_secrets(&protected(SECRETS), now() + 500, rng)
		.expect("sets up");
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, rng)
		.expect("makes the space");
	laptop
		.define_type(personal, &login_type(), now() + 2_000, rng)
		.expect("defines");
	let bank = login(
		&mut laptop,
		personal,
		"Bank",
		&["hunter2", "correct horse"],
		now() + 3_000,
		rng,
	);
	(laptop, personal, household, bank)
}

#[test]
fn a_move_brings_secret_values_past_ones_too_only_with_the_secrets_unlocked() {
	let mut rng = TestRng::new("moving secrets");
	let (mut laptop, personal, household, bank) = laptop_with_login(&mut rng);
	let at = now() + 600_000;
	// Locked, which entity the sealed values are of is unknown, so the login does not move.
	laptop.lock_secrets();
	assert!(
		laptop
			.move_preview(personal, bank, household)
			.expect("previews")
			.secrets
	);
	let refused = move_keeping(
		&mut laptop,
		(personal, bank, household),
		MoveSecrets::Bring,
		at,
		&mut rng,
	);
	assert!(
		matches!(refused, Err(NodeError::MoveNeedsSecrets)),
		"{refused:?}"
	);
	assert!(!marked(&laptop, (personal, bank)));

	unlock(&mut laptop);
	let moved = move_keeping(
		&mut laptop,
		(personal, bank, household),
		MoveSecrets::Bring,
		at + 1_000,
		&mut rng,
	)
	.expect("moves");
	assert_eq!(
		shown(&laptop, (household, moved), &password()),
		[text("correct horse")]
	);
	assert_eq!(
		ever(&laptop, (household, moved), &password()),
		[text("correct horse"), text("hunter2")]
	);
	// Sealed with the household's secrets key, which the move set up: locked, they show no
	// more, and the title still does.
	assert_eq!(
		laptop
			.secrets_key_numbers(household)
			.expect("a space")
			.0
			.len(),
		1
	);
	laptop.lock_secrets();
	assert_eq!(
		ever(&laptop, (household, moved), &password()),
		Vec::<Value>::new()
	);
	assert_eq!(shown(&laptop, (household, moved), &title()), [text("Bank")]);
	unlock(&mut laptop);
	assert_eq!(
		shown(&laptop, (household, moved), &password()),
		[text("correct horse")]
	);
	// The old login keeps its secret values, as the move kept what it left behind.
	assert_eq!(
		ever(&laptop, (personal, bank), &password()),
		[text("correct horse"), text("hunter2")]
	);
}

#[test]
fn a_field_secret_in_the_old_space_is_made_secret_in_the_new_one_by_its_admin() {
	let mut rng = TestRng::new("secret for good, moved");
	let mut laptop = user_library("Ada", "laptop", &mut rng);
	laptop
		.set_up_secrets(&protected(SECRETS), now() + 500, &mut rng)
		.expect("sets up");
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let safe = note_made_secret(&mut laptop, personal, now() + 2_000, &mut rng);
	// Notes, as the move defines them in the household, leave the body plain there.
	assert!(
		!laptop
			.secret_fields(household)
			.expect("a space")
			.contains(&body())
	);
	let moved = move_keeping(
		&mut laptop,
		(personal, safe, household),
		MoveSecrets::Bring,
		now() + 600_000,
		&mut rng,
	)
	.expect("moves");
	assert!(
		laptop
			.secret_fields(household)
			.expect("a space")
			.contains(&body())
	);
	assert_eq!(shown(&laptop, (household, moved), &body()), [text("1234")]);
	laptop.lock_secrets();
	assert_eq!(
		ever(&laptop, (household, moved), &body()),
		Vec::<Value>::new()
	);
	assert_eq!(shown(&laptop, (household, moved), &title()), [text("Safe")]);
}

#[test]
fn an_editor_leaves_secret_values_behind_where_their_field_is_not_secret() {
	let mut rng = TestRng::new("secret values left behind");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	let mut bea = user_library("Bea", "phone", &mut rng);
	bea.set_up_secrets(&protected(SECRETS), now() + 500, &mut rng)
		.expect("sets up");
	let mut at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	invite_member(
		&mut ada,
		(household, MemberRole::Editor),
		&mut bea,
		at + 100,
	);
	at = sync_rounds(&mut bea, &mut ada, at + 200, 2);
	let personal = bea.personal_space();
	let safe = note_made_secret(&mut bea, personal, at, &mut rng);

	// The body would be plain in the household, where only an admin makes it secret.
	let refused = move_keeping(
		&mut bea,
		(personal, safe, household),
		MoveSecrets::Bring,
		at + 1_000,
		&mut rng,
	);
	assert!(
		matches!(&refused, Err(NodeError::NotSecretThere(secret)) if *secret == body()),
		"{refused:?}"
	);
	// Refused before anything is written: no mark, and no type in the household.
	assert!(!marked(&bea, (personal, safe)));
	assert!(bea.types(household).expect("a space").get(&NOTE).is_none());

	// Left behind, the body stays with the old note, also once the rest of it is purged.
	let moved = bea
		.move_entity(
			personal,
			safe,
			household,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Leave,
			at + 2_000,
			&mut rng,
		)
		.expect("moves");
	assert_eq!(shown(&bea, (household, moved), &title()), [text("Safe")]);
	assert_eq!(
		shown(&bea, (household, moved), &body()),
		Vec::<Value>::new()
	);
	sync_rounds(&mut bea, &mut ada, at + 3_000, 2);
	assert_eq!(
		ever(&bea, (personal, safe), &title()),
		Vec::<Value>::new(),
		"the rest is purged"
	);
	assert_eq!(ever(&bea, (personal, safe), &body()), [text("1234")]);
}

#[test]
fn secret_values_move_into_a_space_only_where_this_device_may_seal_them() {
	let mut rng = TestRng::new("moving into others' secrets");
	let mut ada = user_library("Ada", "laptop", &mut rng);
	ada.set_up_secrets(&protected("ada secrets"), now() + 500, &mut rng)
		.expect("sets up");
	let mut bea = user_library("Bea", "phone", &mut rng);
	bea.set_up_secrets(&protected(SECRETS), now() + 500, &mut rng)
		.expect("sets up");
	let mut at = now() + 100_000;
	let household = ada
		.create_space("Household", at, &mut rng)
		.expect("makes it");
	ada.define_type(household, &login_type(), at + 50, &mut rng)
		.expect("defines");
	invite_member(
		&mut ada,
		(household, MemberRole::Editor),
		&mut bea,
		at + 100,
	);
	at = sync_rounds(&mut bea, &mut ada, at + 200, 2);
	let personal = bea.personal_space();
	bea.define_type(personal, &login_type(), at, &mut rng)
		.expect("defines");
	let bank = login(&mut bea, personal, "Bank", &["hunter2"], at + 100, &mut rng);

	// An editor sets up no secrets of the household.
	let refused = move_keeping(
		&mut bea,
		(personal, bank, household),
		MoveSecrets::Bring,
		at + 200_000,
		&mut rng,
	);
	assert!(
		matches!(refused, Err(NodeError::AdminsOnly(_))),
		"{refused:?}"
	);
	assert!(!marked(&bea, (personal, bank)));

	// Once Ada set them up, they are hers to give, and Bea, who holds none, moves none.
	login(
		&mut ada,
		household,
		"Gas",
		&["staple"],
		at + 300_000,
		&mut rng,
	);
	at = sync_rounds(&mut bea, &mut ada, at + 400_000, 2);
	let refused = move_keeping(
		&mut bea,
		(personal, bank, household),
		MoveSecrets::Bring,
		at,
		&mut rng,
	);
	assert!(
		matches!(refused, Err(NodeError::SpaceSecretsElsewhere)),
		"{refused:?}"
	);
	assert!(!marked(&bea, (personal, bank)));
}

#[test]
fn a_secret_file_moves_with_its_content() {
	let mut rng = TestRng::new("moving a secret file");
	let (mut laptop, personal, household, _) = laptop_with_login(&mut rng);
	let scan = b"%PDF-1.7 the passport's scan".to_vec();
	let added = laptop
		.add_secret_file(
			personal,
			"passport.pdf",
			scan.as_slice(),
			&[],
			now() + 600_000,
			&mut rng,
		)
		.expect("adds");
	let moved = move_keeping(
		&mut laptop,
		(personal, added.file, household),
		MoveSecrets::Bring,
		now() + 700_000,
		&mut rng,
	)
	.expect("moves");
	let mut content = Vec::new();
	laptop
		.open_secret_file(household, moved)
		.expect("opens")
		.read_to_end(&mut content)
		.expect("reads");
	assert_eq!(content, scan);
	laptop.lock_secrets();
	assert!(matches!(
		laptop.open_secret_file(household, moved),
		Err(NodeError::SecretsLocked)
	));
}

#[test]
fn a_record_moves_with_the_files_attached_to_it_alone() {
	let mut rng = TestRng::new("moving attachments");
	let (mut laptop, personal, household, bank) = laptop_with_login(&mut rng);
	let shop = login(
		&mut laptop,
		personal,
		"Shop",
		&["popcorn"],
		now() + 4_000,
		&mut rng,
	);
	let codes = laptop
		.add_secret_file(
			personal,
			"codes.txt",
			b"4f2a".as_slice(),
			&[bank],
			now() + 600_000,
			&mut rng,
		)
		.expect("adds")
		.file;
	let both = laptop
		.add_secret_file(
			personal,
			"both.txt",
			b"shared".as_slice(),
			&[bank, shop],
			now() + 610_000,
			&mut rng,
		)
		.expect("adds")
		.file;
	let preview = laptop
		.move_preview(personal, bank, household)
		.expect("previews");
	assert_eq!(preview.entities, [bank, codes]);
	let moved = move_keeping(
		&mut laptop,
		(personal, bank, household),
		MoveSecrets::Bring,
		now() + 700_000,
		&mut rng,
	)
	.expect("moves");
	// The file attached to it alone went with it, attached to it there.
	let (space, file) = laptop
		.moved_into(personal, codes)
		.expect("reads")
		.expect("moved");
	assert_eq!(space, household);
	assert_eq!(
		shown(&laptop, (household, file), &nibkeep_std::ATTACHED_TO),
		[Value::Reference(moved)]
	);
	let mut content = Vec::new();
	laptop
		.open_secret_file(household, file)
		.expect("opens")
		.read_to_end(&mut content)
		.expect("reads");
	assert_eq!(content, b"4f2a");
	// The one attached to another record too stays.
	assert!(laptop.moved_into(personal, both).expect("reads").is_none());
}

#[test]
fn what_a_move_left_behind_loses_its_secret_values_once_the_mover_sees_them() {
	let mut rng = TestRng::new("secret values purged behind");
	let (mut laptop, personal, household, bank) = laptop_with_login(&mut rng);
	let mut desk = {
		let code = PairingCode::generate(&mut rng);
		let directory = nibkeep_testkit::empty_directory(
			env!("CARGO_TARGET_TMPDIR"),
			&common::unique_folder("secret values purged behind, desk"),
		);
		std::fs::remove_dir(&directory).expect("removes");
		let (paired, joined) =
			pair_named(&mut laptop, &directory, "desk", (&code, &code), &mut rng);
		paired.expect("pairs");
		joined.expect("joins").library
	};
	let mut at = sync_rounds(&mut laptop, &mut desk, now() + 600_000, 3);
	let gas = login(
		&mut laptop,
		personal,
		"Gas",
		&["staple"],
		at + 1_000,
		&mut rng,
	);
	let moved = laptop
		.move_entity(
			personal,
			bank,
			household,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Bring,
			at + 200_000,
			&mut rng,
		)
		.expect("moves");
	// Another login leaves its password behind, which no purge takes.
	laptop
		.move_entity(
			personal,
			gas,
			household,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Leave,
			at + 210_000,
			&mut rng,
		)
		.expect("moves");

	// Synced while the secrets are locked, the rest is purged, and the sealed values stay,
	// as which login they are of is unknown.
	laptop.lock_secrets();
	at = sync_rounds(&mut laptop, &mut desk, at + 300_000, 3);
	unlock(&mut laptop);
	assert_eq!(
		ever(&laptop, (personal, bank), &title()),
		Vec::<Value>::new()
	);
	assert_eq!(
		ever(&laptop, (personal, bank), &password()),
		[text("correct horse"), text("hunter2")]
	);
	// Synced while they are unlocked, the secret values that moved are purged too.
	sync_rounds(&mut laptop, &mut desk, at, 1);
	assert_eq!(
		ever(&laptop, (personal, bank), &password()),
		Vec::<Value>::new()
	);
	assert_eq!(
		ever(&laptop, (personal, gas), &password()),
		[text("staple")]
	);
	assert_eq!(
		shown(&laptop, (household, moved), &password()),
		[text("correct horse")]
	);
}

#[test]
fn a_purge_of_what_stays_behind_takes_its_secret_values_with_the_rest_once_unlocked() {
	let mut rng = TestRng::new("secret values purged at once");
	let (mut laptop, personal, household, bank) = laptop_with_login(&mut rng);
	let moved = laptop
		.move_entity(
			personal,
			bank,
			household,
			MoveHistory::Bring,
			Behind::Purge,
			MoveSecrets::Bring,
			now() + 600_000,
			&mut rng,
		)
		.expect("moves");
	// The mover purges it now, the secrets unlocked: every value but the marks.
	let purged = laptop
		.purge_behind(personal, bank, now() + 700_000, &mut rng)
		.expect("purges");
	assert!(purged >= 3, "{purged}");
	assert_eq!(
		ever(&laptop, (personal, bank), &title()),
		Vec::<Value>::new()
	);
	assert_eq!(
		ever(&laptop, (personal, bank), &password()),
		Vec::<Value>::new()
	);
	assert!(marked(&laptop, (personal, bank)), "the mark stays");
	assert_eq!(
		ever(&laptop, (household, moved), &password()),
		[text("correct horse"), text("hunter2")]
	);
}

#[test]
fn a_plain_value_of_a_field_secret_in_the_new_space_is_sealed_there() {
	let mut rng = TestRng::new("sealed on arrival");
	let mut laptop = user_library("Ada", "laptop", &mut rng);
	laptop
		.set_up_secrets(&protected(SECRETS), now() + 500, &mut rng)
		.expect("sets up");
	let personal = laptop.personal_space();
	let household = laptop
		.create_space("Household", now() + 1_000, &mut rng)
		.expect("makes the space");
	let note = TypeDef::new(NOTE, 1)
		.with_field(title(), FieldDef::new(ValueKind::Text))
		.with_field(body(), FieldDef::new(ValueKind::Text));
	laptop
		.define_type(personal, &note, now() + 2_000, &mut rng)
		.expect("defines");
	let safe = laptop
		.create_entity(
			personal,
			&[NOTE],
			vec![(title(), text("Safe")), (body(), text("1234"))],
			now() + 3_000,
			&mut rng,
		)
		.expect("adds");
	let safe_note =
		TypeDef::new(SAFE_NOTE, 1).with_field(body(), FieldDef::new(ValueKind::Text).secret());
	laptop
		.define_type(household, &safe_note, now() + 4_000, &mut rng)
		.expect("defines");
	// Plain here, the body would be sealed there, which needs the secrets.
	laptop.lock_secrets();
	assert!(
		laptop
			.move_preview(personal, safe, household)
			.expect("previews")
			.secrets
	);
	let refused = move_keeping(
		&mut laptop,
		(personal, safe, household),
		MoveSecrets::Bring,
		now() + 600_000,
		&mut rng,
	);
	assert!(
		matches!(refused, Err(NodeError::SecretsLocked)),
		"{refused:?}"
	);
	assert!(!marked(&laptop, (personal, safe)));
	unlock(&mut laptop);
	let moved = move_keeping(
		&mut laptop,
		(personal, safe, household),
		MoveSecrets::Bring,
		now() + 700_000,
		&mut rng,
	)
	.expect("moves");
	laptop.lock_secrets();
	assert_eq!(
		ever(&laptop, (household, moved), &body()),
		Vec::<Value>::new()
	);
	unlock(&mut laptop);
	assert_eq!(shown(&laptop, (household, moved), &body()), [text("1234")]);
}
