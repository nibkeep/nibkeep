//! Libraries made from a seed, of any size: the compatibility fixtures, and the data of the
//! benchmarks (`docs/Plan.md`, section 4).
//!
//! A library is made in rounds. The first round writes something of everything that the
//! engine can store: types that grow, secret fields, every kind of value, references, a
//! blob of several chunks, values that compete, edits, deleted and restored entities, and
//! values that change over time. Every later round adds the same mix of entities again,
//! with numbered titles and names and later times, and a blob every hundredth round, so
//! that a library of any size holds what a library of one round holds. Secret values come
//! with the keys that seal them, in a later step.
//!
//! Real data, fetched from Wikidata and MusicBrainz by `scripts/fetch-real-data.py`, is
//! imported by [`RealData`].

use std::path::Path;

use nibkeep_base::EntityId;
use nibkeep_crypto::PassphraseParams;
use nibkeep_governance::{NAME, PERSON};
use nibkeep_model::{FieldDef, FieldName, Name, Tag, TypeDef, TypeName, Value, ValueKind};
use nibkeep_node::{Dating, Edit, Library, NewLibrary, NewProtection};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::Edtf;

mod real;

pub use real::{ImportError, Imported, Languages, RECIPE, RealData};

/// The passphrase of every generated library. They hold no secrets of anyone's.
pub const PASSPHRASE: &str = "correct horse battery staple";

/// The parameters of the passphrase: quick, since the libraries hold nothing to protect.
///
/// # Panics
///
/// Never: the parameters are valid.
#[must_use]
pub fn passphrase_params() -> PassphraseParams {
	PassphraseParams::new(64, 1, 1).expect("valid")
}

/// The type of the books of every round.
pub const BOOK: TypeName = TypeName::from_static("local.book");

/// The type of the login of every round.
pub const LOGIN: TypeName = TypeName::from_static("local.login");

/// How many minutes each round takes.
const MINUTES_PER_ROUND: u64 = 30;

/// How many entities each round adds: an author, five books and a login.
pub const ENTITIES_PER_ROUND: u64 = 7;

/// Every how many rounds a blob is imported.
const BLOB_EVERY: u64 = 100;

/// Makes a library in `directory` in `rounds` rounds, from a fixed seed, and returns it.
///
/// # Panics
///
/// Panics if the library cannot be made, such as when the directory holds one already.
#[must_use]
pub fn generate(directory: &Path, rounds: u64) -> Library {
	let mut rng = TestRng::new("the compatibility fixture");
	let mut library = create_library(directory, &mut rng);
	define_types(&mut library, &mut rng);
	for round in 0..rounds {
		let mut generator = Generator {
			library: &mut library,
			rng: &mut rng,
			round,
		};
		let author = generator.add_first_entities();
		generator.edit_and_delete();
		generator.change_over_time();
		generator.purge();
		generator.in_languages(author);
	}
	library
}

/// Makes the library that generated and imported data go into: Ada's, on her laptop, who
/// reads English, then anything in the Latin script, then Russian, with the passphrase of
/// every generated library, at the start of the first round.
fn create_library(directory: &Path, rng: &mut TestRng) -> Library {
	let protection = NewProtection::Passphrase(PASSPHRASE, passphrase_params());
	Library::create(
		directory,
		&NewLibrary::new("Ada", "laptop", "nib")
			.with_languages("en, *-Latn, ru".parse().expect("ranges")),
		&protection,
		at(0, 0),
		rng,
	)
	.expect("creates")
}

/// The time of the `minute`th change of round `round`, from 09:00 UTC on 2026-10-12.
#[must_use]
pub fn at(round: u64, minute: u64) -> u64 {
	millis_at("2026-10-12", 9, 0) + (round * MINUTES_PER_ROUND + minute) * 60_000
}

/// Editing at minute `minute` of round `round`, in UTC.
fn dating(round: u64, minute: u64) -> Dating {
	Dating::today(at(round, minute), 0).expect("a moment")
}

fn field(name: &str) -> FieldName {
	name.parse().expect("a field name")
}

fn edtf(date: &str) -> Edtf {
	date.parse().expect("a date")
}

fn date(date: &str) -> Value {
	Value::Date(edtf(date))
}

/// Defines the types of every round: one that grows, with labels in languages, and secret
/// fields.
fn define_types(library: &mut Library, rng: &mut TestRng) {
	let personal = library.personal_space();
	let tag = |text: &str| Tag::parse(text).expect("a language tag");
	let book = TypeDef::new(BOOK, 1)
		.with_label(Some(&tag("en")), "Book")
		.with_label(Some(&tag("sv")), "Bok")
		.with_field(
			field("local.title"),
			FieldDef::new(ValueKind::Text)
				.with_label(Some(&tag("en")), "Title")
				.with_label(None, "Titel?"),
		)
		.with_field(
			field("local.author"),
			FieldDef::new(ValueKind::Reference).many().role(),
		)
		.with_field(field("local.tag"), FieldDef::new(ValueKind::Text).many())
		.with_field(field("local.year"), FieldDef::new(ValueKind::Date))
		.with_field(field("local.pages"), FieldDef::new(ValueKind::Integer))
		.with_field(field("local.read"), FieldDef::new(ValueKind::Boolean))
		.with_field(field("local.scan"), FieldDef::new(ValueKind::Blob));
	library
		.define_type(personal, &book, at(0, 1), rng)
		.expect("defines books");
	let grown = TypeDef::new(BOOK, 2)
		.with_label(Some(&tag("en")), "Volume")
		.with_label(Some(&tag("ru")), "Книга")
		.with_field(
			field("local.status"),
			FieldDef::new(ValueKind::Text)
				.last_writer_wins()
				.with_label(Some(&tag("en")), "Status"),
		)
		.with_field(
			field("local.notes"),
			FieldDef::new(ValueKind::Text).secret(),
		);
	library
		.define_type(personal, &grown, at(0, 2), rng)
		.expect("grows books");
	let login = TypeDef::new(LOGIN, 1)
		.with_field(field("local.site"), FieldDef::new(ValueKind::Text))
		.with_field(
			field("local.password"),
			FieldDef::new(ValueKind::Text).secret(),
		);
	library
		.define_type(personal, &login, at(0, 3), rng)
		.expect("defines logins");
}

/// The generator, in one of its rounds.
struct Generator<'a> {
	library: &'a mut Library,
	rng: &'a mut TestRng,
	round: u64,
}

impl Generator<'_> {
	/// Text of this round: as it is in the first round, numbered in later ones.
	fn text(&self, text: &str) -> String {
		if self.round == 0 {
			text.to_owned()
		} else {
			format!("{text} {}", self.round)
		}
	}

	fn value(&self, text: &str) -> Value {
		Value::Text(self.text(text))
	}

	fn at(&self, minute: u64) -> u64 {
		at(self.round, minute)
	}

	fn create(
		&mut self,
		types: &[TypeName],
		values: Vec<(FieldName, Value)>,
		minute: u64,
	) -> EntityId {
		let personal = self.library.personal_space();
		self.library
			.create_entity(personal, types, values, self.at(minute), self.rng)
			.expect("adds an entity")
	}

	/// Edits an entity at minute `minute`, as of that moment.
	fn edit(&mut self, entity: EntityId, edits: Vec<Edit>, minute: u64) {
		let when = dating(self.round, minute);
		self.edit_on(entity, edits, &when, minute);
	}

	/// Edits an entity at minute `minute`, with another dating.
	fn edit_on(&mut self, entity: EntityId, edits: Vec<Edit>, when: &Dating, minute: u64) {
		let personal = self.library.personal_space();
		self.library
			.edit_entity(personal, entity, edits, when, self.at(minute), self.rng)
			.expect("edits");
	}

	/// Adds a person, named in English, and a book with a value of every kind, and a blob in
	/// the first round and every hundredth. Returns the person.
	fn add_first_entities(&mut self) -> EntityId {
		let personal = self.library.personal_space();
		let round = self.round;
		let scan = round.is_multiple_of(BLOB_EVERY).then(|| {
			let seed = u32::try_from(round / BLOB_EVERY).expect("few blobs");
			let scan: Vec<u8> = (0..150_000_u32)
				.map(|index| {
					u8::try_from(index.wrapping_add(seed).wrapping_mul(2_654_435_761) >> 24)
						.expect("a byte")
				})
				.collect();
			self.library
				.import_blob(personal, scan.as_slice(), self.at(4), self.rng)
				.expect("imports the scan")
		});
		let name = Name::new(self.text("Ursula K. Le Guin"))
			.with_part("given", "Ursula")
			.with_part("additional", "K.")
			.with_part("family", "Le Guin");
		let english = Tag::parse("en").expect("a tag");
		let author = self
			.library
			.create_entity_with_language(
				personal,
				&[PERSON],
				vec![(NAME, Value::Name(name))],
				&english,
				self.at(5),
				self.rng,
			)
			.expect("adds the author");
		let mut values = vec![
			(field("local.title"), self.value("The Dispossessed")),
			(field("local.author"), Value::Reference(author)),
			(field("local.tag"), Value::Text("anarchy".to_owned())),
			(field("local.tag"), Value::Text("physics".to_owned())),
			(field("local.year"), date("1974")),
			(field("local.pages"), Value::Integer(387)),
			(field("local.read"), Value::Boolean(true)),
		];
		if let Some(scan) = scan {
			values.push((field("local.scan"), Value::Blob(scan)));
		}
		self.create(&[BOOK], values, 6);
		author
	}

	/// Adds books that are edited, deleted and restored, and a login.
	fn edit_and_delete(&mut self) {
		let leviathan = self.create(
			&[BOOK],
			vec![
				(field("local.title"), self.value("Leviathan Wakes")),
				(field("local.year"), date("2011-06~")),
				(field("local.tag"), Value::Text("space".to_owned())),
				(field("local.tag"), Value::Text("noir".to_owned())),
			],
			7,
		);
		for (minute, edits) in [
			(
				8,
				vec![Edit::Add(
					field("local.title"),
					self.value("Leviathan wakes"),
				)],
			),
			(
				9,
				vec![Edit::Add(
					field("local.status"),
					Value::Text("reading".to_owned()),
				)],
			),
			(
				10,
				vec![Edit::Add(
					field("local.status"),
					Value::Text("read".to_owned()),
				)],
			),
			(
				11,
				vec![
					Edit::Remove(field("local.tag"), Value::Text("noir".to_owned())),
					Edit::Set(field("local.read"), vec![Value::Boolean(false)]),
				],
			),
		] {
			self.edit(leviathan, edits, minute);
		}

		let tehanu = self.create(
			&[BOOK],
			vec![(field("local.title"), self.value("Tehanu"))],
			12,
		);
		self.edit(tehanu, vec![Edit::Delete], 13);
		let lavinia = self.create(
			&[BOOK],
			vec![(field("local.title"), self.value("Lavinia"))],
			14,
		);
		for (minute, edit) in [(15, Edit::Delete), (16, Edit::Restore)] {
			self.edit(lavinia, vec![edit], minute);
		}
		self.create(
			&[LOGIN],
			vec![(field("local.site"), self.value("library.example"))],
			17,
		);
	}

	/// In the first round, adds a field with one value to books, and decimals and
	/// quantities; in every round, moves a book from shelf to shelf: a change with a period,
	/// a change since a day, a correction in the past, and a value added without a date,
	/// which starts when it is written.
	fn change_over_time(&mut self) {
		let place = || field("local.shelf");
		if self.round == 0 {
			let grown = TypeDef::new(BOOK, 3)
				.with_field(place(), FieldDef::new(ValueKind::Text))
				.with_field(field("local.price"), FieldDef::new(ValueKind::Decimal))
				.with_field(
					field("local.weight"),
					FieldDef::new(ValueKind::Quantity).with_unit("g"),
				);
			let personal = self.library.personal_space();
			self.library
				.define_type(personal, &grown, self.at(18), self.rng)
				.expect("grows books");
		}
		let earthsea = self.create(
			&[BOOK],
			vec![
				(field("local.title"), self.value("A Wizard of Earthsea")),
				(
					field("local.price"),
					Value::Decimal("12.50".parse().expect("a decimal")),
				),
				(
					field("local.weight"),
					Value::Quantity("0.31 kg".parse().expect("a quantity")),
				),
			],
			19,
		);
		for (minute, edit, when) in [
			(
				20,
				Edit::Add(place(), Value::Text("living room".to_owned())),
				dating(self.round, 20).with_valid_time(edtf("2020/2023")),
			),
			(
				21,
				Edit::Add(place(), Value::Text("study".to_owned())),
				dating(self.round, 21).with_valid_time(edtf("2024-05")),
			),
			(
				22,
				Edit::Set(place(), vec![Value::Text("the living room".to_owned())]),
				Dating::on("2021-01-01".parse().expect("a day")),
			),
			(
				23,
				Edit::Add(place(), Value::Text("hall".to_owned())),
				dating(self.round, 23),
			),
		] {
			self.edit_on(earthsea, vec![edit], &when, minute);
		}
	}

	/// In the first round, adds a book with a note that is then purged, together with its
	/// past: the purge states the title again. Only once, as a purge writes the state file
	/// anew.
	fn purge(&mut self) {
		if self.round != 0 {
			return;
		}
		let note = || field("local.note");
		let home = self.create(
			&[BOOK],
			vec![
				(field("local.title"), self.value("Always Coming Home")),
				(note(), Value::Text("lent to Grace, 555 0100".to_owned())),
			],
			24,
		);
		let personal = self.library.personal_space();
		self.library
			.purge(personal, home, &note(), None, self.at(25), self.rng)
			.expect("purges the note");
	}

	/// Adds a form of the author's name in Russian, as another form of the English one.
	fn in_languages(&mut self, author: EntityId) {
		let russian = Name::new(self.text("Урсула Ле Гуин"));
		let when = dating(self.round, 26).with_language(Tag::parse("ru").expect("a tag"));
		self.edit_on(
			author,
			vec![Edit::AddForm {
				field: NAME,
				of: None,
				value: Value::Name(russian),
			}],
			&when,
			26,
		);
	}
}
