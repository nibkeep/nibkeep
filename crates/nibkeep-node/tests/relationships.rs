//! Relationships: links read from their other end, search through links and pen names,
//! relationships recorded twice, and scenario S12 against the standard vocabulary.

use crate::common::{library, now, this_day, today};
use nibkeep_base::EntityId;
use nibkeep_model::{FieldDef, FieldName, Name, TypeDef, TypeName, Value, ValueKind, fields};
use nibkeep_node::{Edit, Library, Links, More};
use nibkeep_std::{
	APPEARS_IN, AUTHOR, BASED_ON, BOOK, CHARACTER, PEN_NAME, PERFORMER, PORTRAYAL, PORTRAYED,
	PRODUCTION, USED_BY,
};
use nibkeep_testkit::TestRng;
use nibkeep_time::Day;

const PERSON: TypeName = nibkeep_governance::PERSON;
const SERIES: TypeName = TypeName::from_static("local.tv-series");

fn name(text: &str) -> Value {
	Value::Name(Name::new(text))
}

fn field(text: &str) -> FieldName {
	text.parse().expect("a field name")
}

/// Adds an entity of a type with these values, and returns it.
fn add(
	library: &mut Library,
	kind: TypeName,
	values: Vec<(FieldName, Value)>,
	rng: &mut TestRng,
) -> EntityId {
	let personal = library.personal_space();
	library
		.create_entity(personal, &[kind], values, now(), rng)
		.expect("creates")
}

/// The Expanse: two people, the pen name they share, and the first three novels.
struct Expanse {
	abraham: EntityId,
	franck: EntityId,
	corey: EntityId,
	books: [EntityId; 3],
}

fn expanse(library: &mut Library, rng: &mut TestRng) -> Expanse {
	let abraham = add(
		library,
		PERSON,
		vec![(fields::NAME, name("Daniel Abraham"))],
		rng,
	);
	let franck = add(
		library,
		PERSON,
		vec![(fields::NAME, name("Ty Franck"))],
		rng,
	);
	let corey = add(
		library,
		PEN_NAME,
		vec![
			(fields::NAME, name("James S. A. Corey")),
			(USED_BY, Value::Reference(abraham)),
			(USED_BY, Value::Reference(franck)),
		],
		rng,
	);
	let books = ["Leviathan Wakes", "Caliban's War", "Abaddon's Gate"].map(|title| {
		add(
			library,
			BOOK,
			vec![
				(fields::NAME, name(title)),
				(AUTHOR, Value::Reference(corey)),
			],
			rng,
		)
	});
	Expanse {
		abraham,
		franck,
		corey,
		books,
	}
}

/// The names of the entities in a group of links, in their order.
fn names(links: &Links) -> Vec<&str> {
	links
		.entities
		.iter()
		.map(|linking| linking.name.as_deref().unwrap_or("(no name)"))
		.collect()
}

/// The links to an entity on the day of [`now`], at most ten of each field.
fn links_to(library: &Library, entity: EntityId) -> Vec<Links> {
	library
		.links_to(library.personal_space(), entity, this_day(), 10)
		.expect("a space")
}

#[test]
fn a_new_library_has_the_standard_vocabulary() {
	let mut rng = TestRng::new("vocabulary");
	let (_, library) = library("vocabulary", &mut rng);
	let types = library.types(library.personal_space()).expect("a space");
	for definition in nibkeep_std::types() {
		assert_eq!(types.get(definition.name()), Some(&definition));
	}
	// The vocabulary is types, which no list of things shows: the user's own person aside.
	nibkeep_testkit::assert_empty!(
		library
			.entities(library.personal_space(), None)
			.expect("a space")
			.into_iter()
			.filter(|listed| listed
				.types
				.iter()
				.any(|kind| { kind.as_str().starts_with("std.") && *kind != nibkeep_std::PERSON }))
			.collect::<Vec<_>>()
	);
}

#[test]
fn links_are_read_from_their_other_end() {
	let mut rng = TestRng::new("other end");
	let (_, mut library) = library("other end", &mut rng);
	let expanse = expanse(&mut library, &mut rng);

	let to_corey = links_to(&library, expanse.corey);
	assert_eq!(to_corey.len(), 1);
	assert_eq!(to_corey[0].field, AUTHOR);
	assert_eq!(to_corey[0].reverse, Some(field("std.author-of")));
	assert!(!to_corey[0].role);
	assert_eq!(to_corey[0].count, 3);
	// Ordered by name, without regard to case.
	assert_eq!(
		names(&to_corey[0]),
		["Abaddon's Gate", "Caliban's War", "Leviathan Wakes"]
	);
	assert_eq!(to_corey[0].entities[2].id, expanse.books[0]);
	assert_eq!(to_corey[0].entities[2].types, [BOOK]);

	let to_abraham = links_to(&library, expanse.abraham);
	assert_eq!(to_abraham.len(), 1);
	assert_eq!(to_abraham[0].field, USED_BY);
	assert_eq!(to_abraham[0].reverse, Some(field("std.pen-names")));
	assert_eq!(names(&to_abraham[0]), ["James S. A. Corey"]);
	assert_eq!(
		names(&links_to(&library, expanse.franck)[0]),
		["James S. A. Corey"]
	);

	// The books link to nothing that links back.
	nibkeep_testkit::assert_empty!(links_to(&library, expanse.books[0]));

	// An entity that refers to itself is not listed as linking to itself.
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			expanse.books[0],
			vec![Edit::Add(BASED_ON, Value::Reference(expanse.books[0]))],
			&today(),
			now(),
			&mut rng,
		)
		.expect("adds");
	nibkeep_testkit::assert_empty!(links_to(&library, expanse.books[0]));
}

#[test]
fn many_links_are_counted_and_some_shown() {
	let mut rng = TestRng::new("many links");
	let (_, mut library) = library("many links", &mut rng);
	let expanse = expanse(&mut library, &mut rng);
	let personal = library.personal_space();
	let links = library
		.links_to(personal, expanse.corey, this_day(), 2)
		.expect("a space");
	assert_eq!(links[0].count, 3);
	assert_eq!(names(&links[0]), ["Abaddon's Gate", "Caliban's War"]);
}

#[test]
fn deleted_entities_link_to_nothing() {
	let mut rng = TestRng::new("deleted links");
	let (_, mut library) = library("deleted links", &mut rng);
	let expanse = expanse(&mut library, &mut rng);
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			expanse.books[1],
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	let links = links_to(&library, expanse.corey);
	assert_eq!(links[0].count, 2);
	assert_eq!(names(&links[0]), ["Abaddon's Gate", "Leviathan Wakes"]);
}

#[test]
fn a_relationship_is_shown_with_its_other_participants() {
	let mut rng = TestRng::new("participants");
	let (_, mut library) = library("participants", &mut rng);
	let personal = library.personal_space();
	library
		.define_type(
			personal,
			&TypeDef::new(SERIES, 1).with_field(fields::NAME, FieldDef::new(ValueKind::Name)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let amos = add(
		&mut library,
		CHARACTER,
		vec![(fields::NAME, name("Amos Burton"))],
		&mut rng,
	);
	let wes = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Wes Chatham"))],
		&mut rng,
	);
	let show = add(
		&mut library,
		SERIES,
		vec![(fields::NAME, name("The Expanse"))],
		&mut rng,
	);
	let portrayal = add(
		&mut library,
		PORTRAYAL,
		vec![
			(PORTRAYED, Value::Reference(amos)),
			(PERFORMER, Value::Reference(wes)),
			(PRODUCTION, Value::Reference(show)),
		],
		&mut rng,
	);
	let to_wes = links_to(&library, wes);
	assert_eq!(to_wes.len(), 1);
	assert_eq!(to_wes[0].field, PERFORMER);
	assert!(to_wes[0].role);
	assert_eq!(to_wes[0].reverse, None);
	let linking = &to_wes[0].entities[0];
	assert_eq!(linking.id, portrayal);
	assert_eq!(linking.types, [PORTRAYAL]);
	assert_eq!(
		linking.participants,
		[(PORTRAYED, vec![amos]), (PRODUCTION, vec![show])]
	);
}

#[test]
fn a_link_that_held_only_in_the_past_shows_on_its_days() {
	let mut rng = TestRng::new("past links");
	let (_, mut library) = library("past links", &mut rng);
	let personal = library.personal_space();
	let employer = field("local.employer");
	library
		.define_type(
			personal,
			&TypeDef::new(PERSON, 1)
				.with_field(employer.clone(), FieldDef::new(ValueKind::Reference)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let company_type = TypeName::from_static("local.company");
	library
		.define_type(
			personal,
			&TypeDef::new(company_type.clone(), 1)
				.with_field(fields::NAME, FieldDef::new(ValueKind::Name)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let company = |library: &mut Library, text: &str, rng: &mut TestRng| {
		add(
			library,
			company_type.clone(),
			vec![(fields::NAME, name(text))],
			rng,
		)
	};
	let acme = company(&mut library, "Acme", &mut rng);
	let globex = company(&mut library, "Globex", &mut rng);
	let anna = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Anna"))],
		&mut rng,
	);
	for (value, valid) in [(acme, "1990/2015"), (globex, "2019-03")] {
		library
			.edit_entity(
				personal,
				anna,
				vec![Edit::Add(employer.clone(), Value::Reference(value))],
				&today().with_valid_time(valid.parse().expect("a date")),
				now(),
				&mut rng,
			)
			.expect("adds");
	}
	nibkeep_testkit::assert_empty!(links_to(&library, acme));
	assert_eq!(names(&links_to(&library, globex)[0]), ["Anna"]);
	let in_2000: Day = "2000-01-01".parse().expect("a day");
	let then = library
		.links_to(personal, acme, in_2000, 10)
		.expect("a space");
	assert_eq!(names(&then[0]), ["Anna"]);
	nibkeep_testkit::assert_empty!(
		library
			.links_to(personal, globex, in_2000, 10)
			.expect("a space")
	);
	// On any day: a link that held once counts, as one that holds later does.
	for (company, day) in [(acme, this_day()), (globex, in_2000)] {
		let ever = library
			.links_to_on_any_day(personal, company, day, 10)
			.expect("a space");
		assert_eq!(names(&ever[0]), ["Anna"]);
		assert_eq!(ever[0].count, 1);
	}
	// A link removed holds on no day.
	library
		.edit_entity(
			personal,
			anna,
			vec![Edit::Remove(employer.clone(), Value::Reference(acme))],
			&nibkeep_node::Dating::on(in_2000),
			now(),
			&mut rng,
		)
		.expect("removes");
	nibkeep_testkit::assert_empty!(
		library
			.links_to_on_any_day(personal, acme, this_day(), 10)
			.expect("a space")
	);
}

/// The entities that a search finds, each with the entity it was found through.
fn search(library: &Library, query: &str) -> Vec<(EntityId, Option<EntityId>)> {
	library
		.search(library.personal_space(), query)
		.expect("a space")
		.iter()
		.map(|listed| (listed.id, listed.via))
		.collect()
}

#[test]
fn a_search_finds_works_through_their_authors_and_pen_names() {
	let mut rng = TestRng::new("search through");
	let (_, mut library) = library("search through", &mut rng);
	let expanse = expanse(&mut library, &mut rng);
	let [leviathan, caliban, abaddon] = expanse.books;
	// Daniel Abraham writes under his own name too.
	let dragon = add(
		&mut library,
		BOOK,
		vec![
			(fields::NAME, name("The Dragon's Path")),
			(AUTHOR, Value::Reference(expanse.abraham)),
		],
		&mut rng,
	);
	// A search for him finds him; through him, his book and the pen name that stands for
	// him; and through the pen name, the books credited to it.
	assert_eq!(
		search(&library, "daniel abraham"),
		[
			(expanse.abraham, None),
			(expanse.corey, Some(expanse.abraham)),
			(dragon, Some(expanse.abraham)),
			(abaddon, Some(expanse.corey)),
			(caliban, Some(expanse.corey)),
			(leviathan, Some(expanse.corey)),
		]
	);
	assert_eq!(
		search(&library, "ty franck"),
		[
			(expanse.franck, None),
			(expanse.corey, Some(expanse.franck)),
			(abaddon, Some(expanse.corey)),
			(caliban, Some(expanse.corey)),
			(leviathan, Some(expanse.corey)),
		]
	);
	// A search for the pen name finds the books credited to it.
	assert_eq!(
		search(&library, "corey"),
		[
			(expanse.corey, None),
			(abaddon, Some(expanse.corey)),
			(caliban, Some(expanse.corey)),
			(leviathan, Some(expanse.corey)),
		]
	);
	// Only marked links are followed, one step: a book leads nowhere.
	assert_eq!(search(&library, "leviathan"), [(leviathan, None)]);

	// Deleted entities are not found through links either.
	let personal = library.personal_space();
	library
		.edit_entity(
			personal,
			caliban,
			vec![Edit::Delete],
			&today(),
			now(),
			&mut rng,
		)
		.expect("deletes");
	assert_eq!(
		search(&library, "corey"),
		[
			(expanse.corey, None),
			(abaddon, Some(expanse.corey)),
			(leviathan, Some(expanse.corey)),
		]
	);
}

#[test]
fn a_search_lists_its_first_entities_and_counts_the_rest() {
	let mut rng = TestRng::new("search first");
	let (_, mut library) = library("search first", &mut rng);
	let expanse = expanse(&mut library, &mut rng);
	let dragon = add(
		&mut library,
		BOOK,
		vec![
			(fields::NAME, name("The Dragon's Path")),
			(AUTHOR, Value::Reference(expanse.abraham)),
		],
		&mut rng,
	);
	let personal = library.personal_space();
	let books = |kinds: &std::collections::BTreeSet<TypeName>| kinds.contains(&BOOK);
	// "s" is a word of the pen name and of three titles, and finds the fourth book through
	// the pen name.
	for query in ["daniel abraham", "s", "corey", "leviathan", "nothing"] {
		let everything = library.search(personal, query).expect("a space");
		for only_books in [false, true] {
			let kept: Vec<_> = everything
				.iter()
				.filter(|listed| !only_books || listed.types.contains(&BOOK))
				.cloned()
				.collect();
			for first in 0..=kept.len() + 1 {
				let page = library
					.search_first(personal, query, first, |_, kinds| {
						Ok(!only_books || books(kinds))
					})
					.expect("a space");
				let case = format!("{query:?}, first {first}, only books {only_books}");
				assert_eq!(page.listed, kept[..first.min(kept.len())], "{case}");
				let rest = kept.len() - page.listed.len();
				match page.more {
					More::Exactly(more) => assert_eq!(more, rest, "{case}"),
					More::AtLeast(more) => {
						assert!(more > 0 && more <= rest, "{case}: {more} of {rest}");
						assert_eq!(page.listed.len(), first, "{case}");
					}
				}
			}
		}
	}
	// More are found by their words than are listed: those found through links are left
	// uncounted.
	let page = library
		.search_first(personal, "s", 2, |_, _| Ok(true))
		.expect("a space");
	assert_eq!(
		page.more,
		More::AtLeast(2),
		"two of the four found by words"
	);
	let page = library
		.search_first(personal, "s", 4, |_, _| Ok(true))
		.expect("a space");
	assert_eq!(
		page.more,
		More::Exactly(1),
		"Leviathan Wakes, through the pen name"
	);
	assert!(
		page.listed
			.iter()
			.all(|listed| listed.id != expanse.books[0]),
		"{:?}",
		page.listed
	);
	assert!(page.listed.iter().any(|listed| listed.id == dragon));
}

#[test]
fn pen_names_lead_to_their_books_in_the_order_of_their_names() {
	let mut rng = TestRng::new("pen names in order");
	let (_, mut library) = library("pen names in order", &mut rng);
	let author = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Prolific Writer"))],
		&mut rng,
	);
	// Two pen names, the one whose name comes first with the larger ID.
	let pen_names = [0, 1].map(|_| {
		add(
			&mut library,
			PEN_NAME,
			vec![(USED_BY, Value::Reference(author))],
			&mut rng,
		)
	});
	let (later, earlier) = (
		pen_names[0].min(pen_names[1]),
		pen_names[0].max(pen_names[1]),
	);
	let personal = library.personal_space();
	for (pen_name, text) in [(earlier, "Aaron Early"), (later, "Zed Late")] {
		library
			.edit_entity(
				personal,
				pen_name,
				vec![Edit::Set(fields::NAME, vec![name(text)])],
				&today(),
				now(),
				&mut rng,
			)
			.expect("names");
	}
	let book = |library: &mut Library, rng: &mut TestRng, title: &str, by: EntityId| {
		add(
			library,
			BOOK,
			vec![(fields::NAME, name(title)), (AUTHOR, Value::Reference(by))],
			rng,
		)
	};
	let late_book = book(&mut library, &mut rng, "A Book by Zed", later);
	let early_book = book(&mut library, &mut rng, "Z Book by Aaron", earlier);
	let expected = [
		(author, None),
		(earlier, Some(author)),
		(later, Some(author)),
		(early_book, Some(earlier)),
		(late_book, Some(later)),
	];
	assert_eq!(search(&library, "prolific"), expected);
	// So do books alone, which leave out the pen names they come through.
	let page = library
		.search_first(personal, "prolific", 10, |_, kinds| {
			Ok(kinds.contains(&BOOK))
		})
		.expect("a space");
	let books: Vec<_> = page
		.listed
		.iter()
		.map(|listed| (listed.id, listed.via))
		.collect();
	assert_eq!(books, [expected[3], expected[4]]);
}

#[test]
fn an_entity_found_by_its_own_words_is_not_found_again() {
	let mut rng = TestRng::new("found once");
	let (_, mut library) = library("found once", &mut rng);
	let expanse = expanse(&mut library, &mut rng);
	// A book that holds the words itself is found by them, not through its author.
	let memoir = add(
		&mut library,
		BOOK,
		vec![
			(fields::NAME, name("Corey on Corey")),
			(AUTHOR, Value::Reference(expanse.corey)),
		],
		&mut rng,
	);
	let found = search(&library, "corey");
	assert_eq!(found.iter().filter(|(id, _)| *id == memoir).count(), 1);
	assert!(found.contains(&(memoir, None)));
}

#[test]
fn a_relationship_recorded_twice_is_found() {
	let mut rng = TestRng::new("twice");
	let (_, mut library) = library("twice", &mut rng);
	let personal = library.personal_space();
	let amos = add(
		&mut library,
		CHARACTER,
		vec![(fields::NAME, name("Amos Burton"))],
		&mut rng,
	);
	let wes = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Wes Chatham"))],
		&mut rng,
	);
	let participants = vec![
		(PORTRAYED, Value::Reference(amos)),
		(PERFORMER, Value::Reference(wes)),
	];
	nibkeep_testkit::assert_empty!(
		library
			.same_relationship(personal, &[PORTRAYAL], &participants)
			.expect("a space")
	);
	let first = add(&mut library, PORTRAYAL, participants.clone(), &mut rng);
	assert_eq!(
		library
			.same_relationship(personal, &[PORTRAYAL], &participants)
			.expect("a space"),
		[first]
	);
	// Other participants, or values that are no roles, make no relationship the same.
	let other = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Someone"))],
		&mut rng,
	);
	nibkeep_testkit::assert_empty!(
		library
			.same_relationship(
				personal,
				&[PORTRAYAL],
				&[
					(PORTRAYED, Value::Reference(amos)),
					(PERFORMER, Value::Reference(other)),
				]
			)
			.expect("a space")
	);
	nibkeep_testkit::assert_empty!(
		library
			.same_relationship(personal, &[PERSON], &[(fields::NAME, name("Wes Chatham"))])
			.expect("a space")
	);
}

/// S12, "Who was that again?": characters recorded with the book they appear in, listed
/// before the next book; a portrayal that joins a character, an actor and a series; and a
/// game character based on a book's.
#[test]
fn s12_who_was_that_again() {
	let mut rng = TestRng::new("s12");
	let (_, mut library) = library("s12", &mut rng);
	let personal = library.personal_space();
	let expanse = expanse(&mut library, &mut rng);
	let leviathan = expanse.books[0];
	let characters = ["Jim Holden", "Naomi Nagata", "Amos Burton"].map(|character| {
		add(
			&mut library,
			CHARACTER,
			vec![
				(fields::NAME, name(character)),
				(APPEARS_IN, Value::Reference(leviathan)),
			],
			&mut rng,
		)
	});

	// Before the next book, the book lists its characters.
	let to_book = links_to(&library, leviathan);
	assert_eq!(to_book[0].field, APPEARS_IN);
	assert_eq!(to_book[0].reverse, Some(field("std.characters")));
	assert_eq!(
		names(&to_book[0]),
		["Amos Burton", "Jim Holden", "Naomi Nagata"]
	);

	// Later, the actor who plays Amos in the series, and a game character based on him.
	library
		.define_type(
			personal,
			&TypeDef::new(SERIES, 1).with_field(fields::NAME, FieldDef::new(ValueKind::Name)),
			now(),
			&mut rng,
		)
		.expect("defines");
	let amos = characters[2];
	let wes = add(
		&mut library,
		PERSON,
		vec![(fields::NAME, name("Wes Chatham"))],
		&mut rng,
	);
	let series = add(
		&mut library,
		SERIES,
		vec![(fields::NAME, name("The Expanse"))],
		&mut rng,
	);
	let portrayal = add(
		&mut library,
		PORTRAYAL,
		vec![
			(PORTRAYED, Value::Reference(amos)),
			(PERFORMER, Value::Reference(wes)),
			(PRODUCTION, Value::Reference(series)),
		],
		&mut rng,
	);
	let game_amos = add(
		&mut library,
		CHARACTER,
		vec![
			(fields::NAME, name("Amos Burton (game)")),
			(BASED_ON, Value::Reference(amos)),
		],
		&mut rng,
	);
	let to_amos = links_to(&library, amos);
	let by_field: Vec<(FieldName, Vec<EntityId>)> = to_amos
		.iter()
		.map(|links| {
			(
				links.field.clone(),
				links.entities.iter().map(|linking| linking.id).collect(),
			)
		})
		.collect();
	assert_eq!(
		by_field,
		[(BASED_ON, vec![game_amos]), (PORTRAYED, vec![portrayal])]
	);
	assert_eq!(
		to_amos[1].entities[0].participants,
		[(PERFORMER, vec![wes]), (PRODUCTION, vec![series])]
	);

	// The books are credited to James S. A. Corey, and a search for Daniel Abraham finds
	// them too.
	let found = search(&library, "Daniel Abraham");
	for book in expanse.books {
		assert!(found.contains(&(book, Some(expanse.corey))));
	}
}

#[test]
fn of_a_field_whose_last_writer_wins_only_the_value_written_last_links_on_any_day() {
	let mut rng = TestRng::new("last writer links");
	let (_, mut library) = library("last writer links", &mut rng);
	let personal = library.personal_space();
	let mentor = field("local.mentor");
	library
		.define_type(
			personal,
			&TypeDef::new(PERSON, 1).with_field(
				mentor.clone(),
				FieldDef::new(ValueKind::Reference).last_writer_wins(),
			),
			now(),
			&mut rng,
		)
		.expect("defines");
	let person = |library: &mut Library, text: &str, rng: &mut TestRng| {
		add(library, PERSON, vec![(fields::NAME, name(text))], rng)
	};
	let anna = person(&mut library, "Anna", &mut rng);
	let bo = person(&mut library, "Bo", &mut rng);
	let cy = person(&mut library, "Cy", &mut rng);
	for one in [bo, cy] {
		library
			.edit_entity(
				personal,
				anna,
				vec![Edit::Add(mentor.clone(), Value::Reference(one))],
				&today(),
				now(),
				&mut rng,
			)
			.expect("adds");
	}
	let mentored = |one: EntityId| -> Vec<FieldName> {
		library
			.links_to_on_any_day(personal, one, this_day(), 10)
			.expect("a space")
			.into_iter()
			.map(|links| links.field)
			.collect()
	};
	assert!(!mentored(bo).contains(&mentor), "written over");
	assert!(mentored(cy).contains(&mentor));
}
