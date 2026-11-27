//! Scenarios from the design guidelines, as far as phase 1 reaches.

use crate::common::{Device, add, field, set, text};
use nibkeep_base::{EntityId, Hash};
use nibkeep_log::{SpaceKeys, list};
use nibkeep_model::storage::Infallibly;
use nibkeep_model::{
	FieldDef, Operation, Qualifiers, Resolution, Statement, Value, ValueKind, fields,
};
use nibkeep_testkit::{TestRng, millis_at};
use nibkeep_time::{Day, Presence, holds_on};

fn day(text: &str) -> Day {
	text.parse().expect("a day")
}

/// A claim with its source: a statement that adds to a field without replacing anything.
fn claim(
	rng: &mut TestRng,
	entity: EntityId,
	name: &str,
	value: Value,
	source: EntityId,
) -> Operation {
	Operation::assert(
		Statement::new(rng.statement_id(), entity, field(name), value)
			.with_qualifiers(Qualifiers::new().with(fields::SOURCE, Value::Reference(source))),
	)
}

/// The family of S2: three devices that share a space.
struct Family {
	rng: TestRng,
	key: SpaceKeys,
	scanner: Device,
	aunt: Device,
	cousin: Device,
}

impl Family {
	fn new() -> Self {
		let mut rng = TestRng::new("s02");
		let space = rng.space_id();
		let key = SpaceKeys::new(0, rng.key());
		let scanner = Device::new(&mut rng, space);
		let aunt = Device::new(&mut rng, space);
		let cousin = Device::new(&mut rng, space);
		Self {
			rng,
			key,
			scanner,
			aunt,
			cousin,
		}
	}

	/// Everyone receives everything: the aunt from the others, then the others from her.
	fn sync(&mut self, wall: u64) {
		self.aunt.receive_from(&self.scanner, &self.key, wall);
		self.aunt.receive_from(&self.cousin, &self.key, wall);
		self.scanner.receive_from(&self.aunt, &self.key, wall);
		self.cousin.receive_from(&self.aunt, &self.key, wall);
	}
}

/// A print is scanned, front and back. It was taken in the summer of 1975, perhaps.
fn scan_print(family: &mut Family, wall: u64) -> EntityId {
	let rng = &mut family.rng;
	let photo = rng.entity_id();
	let date = Value::Date("1975-22?".parse().expect("valid"));
	let scan = vec![
		add(rng, photo, "org.nibkeep.type", text("org.nibkeep.photo")),
		add(
			rng,
			photo,
			"org.nibkeep.front",
			Value::Blob(Hash::of(b"front")),
		),
		add(
			rng,
			photo,
			"org.nibkeep.back",
			Value::Blob(Hash::of(b"back")),
		),
		add(rng, photo, "org.nibkeep.date-taken", date),
	];
	family.scanner.commit(&family.key, scan, wall, rng);
	family.sync(wall);
	photo
}

/// Without being online at the same time, the aunt recognizes Grandma and the cousin an
/// uncle. Returns them.
fn identify_people(family: &mut Family, photo: EntityId, wall: u64) -> (EntityId, EntityId) {
	let rng = &mut family.rng;
	let grandma = rng.entity_id();
	let uncle = rng.entity_id();
	let operations = vec![
		add(rng, grandma, "org.nibkeep.name", text("Grandma")),
		add(rng, photo, "org.nibkeep.depicts", Value::Reference(grandma)),
	];
	family.aunt.commit(&family.key, operations, wall, rng);
	let operations = vec![
		add(rng, uncle, "org.nibkeep.name", text("Uncle Bo")),
		add(rng, photo, "org.nibkeep.depicts", Value::Reference(uncle)),
	];
	family.cousin.commit(&family.key, operations, wall, rng);
	(grandma, uncle)
}

/// They disagree about the wedding photo: Grandma says 1952, a letter says 1953. Returns
/// the photo and the letter.
fn dispute_a_date(family: &mut Family, grandma: EntityId, wall: u64) -> (EntityId, EntityId) {
	let rng = &mut family.rng;
	let wedding = rng.entity_id();
	let letter = rng.entity_id();
	let date = |year: &str| Value::Date(year.parse().expect("valid"));
	let says_grandma = claim(
		rng,
		wedding,
		"org.nibkeep.date-taken",
		date("1952"),
		grandma,
	);
	family
		.aunt
		.commit(&family.key, vec![says_grandma], wall, rng);
	let says_letter = claim(rng, wedding, "org.nibkeep.date-taken", date("1953"), letter);
	family
		.cousin
		.commit(&family.key, vec![says_letter], wall, rng);
	(wedding, letter)
}

/// S2, the shoebox of prints: old prints are scanned, front and back, with fuzzy dates.
/// Over months, family members identify the people in them, and sometimes disagree.
#[test]
fn s02_the_shoebox_of_prints() {
	let mut family = Family::new();
	let photo = scan_print(&mut family, millis_at("2026-10-10", 10, 0));
	let later = millis_at("2026-11-02", 20, 0);
	let (grandma, uncle) = identify_people(&mut family, photo, later);
	let (wedding, letter) = dispute_a_date(&mut family, grandma, later + 60_000);

	// Once everyone has synced, everyone sees the same.
	family.sync(later + 120_000);
	assert_eq!(family.scanner.log.state(), family.aunt.log.state());
	assert_eq!(family.aunt.log.state(), family.cousin.log.state());
	let state = family.scanner.log.state();

	// Both people are in the photo.
	let depicted: Vec<Option<EntityId>> = state
		.current(photo, &field("org.nibkeep.depicts"))
		.infallible()
		.iter()
		.map(|statement| statement.value.as_reference())
		.collect();
	assert_eq!(depicted.len(), 2);
	assert!(depicted.contains(&Some(grandma)) && depicted.contains(&Some(uncle)));

	// Both dates are kept as competing claims, each with its source and its author.
	let one = FieldDef::new(ValueKind::Date);
	let Resolution::Competing(claims) = state
		.resolve(wedding, &field("org.nibkeep.date-taken"), Some(&one))
		.infallible()
	else {
		panic!("the claims compete");
	};
	for claim in claims {
		let grandma_says = claim.value == Value::Date("1952".parse().expect("valid"));
		let (source, author) = if grandma_says {
			(grandma, &family.aunt.key)
		} else {
			(letter, &family.cousin.key)
		};
		assert_eq!(claim.qualifiers.source(), Some(&Value::Reference(source)));
		assert_eq!(
			family.scanner.log.author_of(claim.id).infallible(),
			Some(author.verifying_key())
		);
	}

	// Travelling in time, the print was perhaps taken in July 1975, and certainly not in
	// 1980. The question is about the date the value names, not about when the statement
	// was true.
	let taken = state
		.current(photo, &field("org.nibkeep.date-taken"))
		.infallible();
	let date_taken = taken[0].value.as_date();
	assert_eq!(holds_on(date_taken, day("1975-07-01")), Presence::Uncertain);
	assert_eq!(holds_on(date_taken, day("1980-01-01")), Presence::No);
}

/// S5, a recommendation: a friend recommends a movie, two taps add it to "To watch" with
/// "recommended by" filled in, and the list is reordered on two phones while both are
/// offline. Asking for a rating after watching needs automations, which come later.
#[test]
fn s05_a_recommendation() {
	let mut rng = TestRng::new("s05");
	let space = rng.space_id();
	let key = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);
	let mut partner = Device::new(&mut rng, space);
	let friday = millis_at("2026-10-09", 18, 0);

	let to_watch = rng.entity_id();
	let mut operations = vec![
		add(
			&mut rng,
			to_watch,
			"org.nibkeep.type",
			text("org.nibkeep.list"),
		),
		add(&mut rng, to_watch, "org.nibkeep.name", text("To watch")),
	];
	phone.commit(&key, operations.split_off(0), friday, &mut rng);
	let movies: Vec<EntityId> = (0..4).map(|_| rng.entity_id()).collect();
	for (index, &movie) in movies.iter().enumerate() {
		let (_, operations) =
			list::insert(phone.log.state(), to_watch, movie, index, &mut rng).infallible();
		phone.commit(&key, operations, friday, &mut rng);
	}

	// Two taps: the recommended movie goes to the top, with who recommended it.
	let friend = rng.entity_id();
	let paterson = rng.entity_id();
	let (membership, mut operations) =
		list::insert(phone.log.state(), to_watch, paterson, 0, &mut rng).infallible();
	operations.push(add(
		&mut rng,
		membership,
		"org.nibkeep.recommended-by",
		Value::Reference(friend),
	));
	phone.commit(&key, operations, friday + 60_000, &mut rng);
	partner.receive_from(&phone, &key, friday + 60_000);

	// Offline, both reorder: the phone moves the third movie to the top, and the partner,
	// a little later, moves the same movie to the end and the first one to second place.
	let [alien, arrival, heat, dune] = movies[..] else {
		panic!("four movies");
	};
	let saturday = millis_at("2026-10-10", 20, 0);
	let move_on =
		|device: &mut Device, rng: &mut TestRng, item: EntityId, index: usize, wall: u64| {
			let membership = list::members(device.log.state(), to_watch)
				.infallible()
				.iter()
				.find(|member| member.item == Some(item))
				.expect("in the list")
				.membership;
			let operations =
				list::move_to(device.log.state(), to_watch, membership, index, rng).infallible();
			device.commit(&key, operations, wall, rng);
		};
	move_on(&mut phone, &mut rng, heat, 0, saturday);
	move_on(&mut partner, &mut rng, heat, 4, saturday + 60_000);
	move_on(&mut partner, &mut rng, alien, 1, saturday + 120_000);

	// They meet again.
	phone.receive_from(&partner, &key, saturday + 180_000);
	partner.receive_from(&phone, &key, saturday + 180_000);
	let order = |device: &Device| -> Vec<EntityId> {
		list::members(device.log.state(), to_watch)
			.infallible()
			.iter()
			.filter_map(|member| member.item)
			.collect()
	};
	assert_eq!(order(&phone), order(&partner));
	// Every movie is there once; the later move of the same movie counts.
	assert_eq!(order(&phone), [paterson, alien, arrival, dune, heat]);
	assert_eq!(
		phone
			.log
			.state()
			.current(membership, &field("org.nibkeep.recommended-by"))
			.infallible()[0]
			.value,
		Value::Reference(friend)
	);
}

/// S9, shopping history: "Which chocolate did I buy last weekend?" The shopping list as
/// it was last Saturday, including items since checked off or deleted.
#[test]
fn s09_shopping_history() {
	let mut rng = TestRng::new("s09");
	let space = rng.space_id();
	let key = SpaceKeys::new(0, rng.key());
	let mut phone = Device::new(&mut rng, space);

	let shopping = rng.entity_id();
	let friday = millis_at("2026-10-09", 18, 0);
	let products = [
		("Milk", "org.example.shop.dairy"),
		("Bread", "org.example.shop.bakery"),
		("Marabou milk chocolate", "org.example.shop.chocolate"),
	];
	let mut items = Vec::new();
	for (index, (name, kind)) in products.into_iter().enumerate() {
		let product = rng.entity_id();
		let (_, mut operations) =
			list::insert(phone.log.state(), shopping, product, index, &mut rng).infallible();
		operations.push(add(&mut rng, product, "org.nibkeep.name", text(name)));
		operations.push(add(&mut rng, product, "org.nibkeep.type", text(kind)));
		phone.commit(&key, operations, friday, &mut rng);
		items.push(product);
	}
	let [milk, bread, chocolate] = items[..] else {
		panic!("three products");
	};
	let membership = |phone: &Device, item: EntityId| {
		list::members(phone.log.state(), shopping)
			.infallible()
			.iter()
			.find(|member| member.item == Some(item))
			.expect("on the list")
			.membership
	};

	// Saturday, in the shop: chocolate and milk go into the basket.
	let saturday = millis_at("2026-10-10", 11, 0);
	for item in [chocolate, milk] {
		let membership = membership(&phone, item);
		let done = set(
			&mut rng,
			&phone.log,
			membership,
			"org.nibkeep.done",
			Value::Boolean(true),
		);
		phone.commit(&key, vec![done], saturday, &mut rng);
	}
	// Sunday: the list is cleared of what was bought.
	let sunday = millis_at("2026-10-11", 9, 0);
	for item in [chocolate, milk] {
		let membership = membership(&phone, item);
		let delete = add(
			&mut rng,
			membership,
			"org.nibkeep.deleted",
			Value::Boolean(true),
		);
		phone.commit(&key, vec![delete], sunday, &mut rng);
	}

	// Today, only the bread is on the list.
	let now: Vec<Option<EntityId>> = list::members(phone.log.state(), shopping)
		.infallible()
		.iter()
		.map(|member| member.item)
		.collect();
	assert_eq!(now, [Some(bread)]);

	// As of Saturday evening, the list still holds everything, and shows what was bought.
	let as_of_saturday = phone.log.state_as_of(millis_at("2026-10-10", 20, 0));
	let members = list::members(&as_of_saturday, shopping).infallible();
	assert_eq!(
		members.iter().map(|member| member.item).collect::<Vec<_>>(),
		[Some(milk), Some(bread), Some(chocolate)]
	);
	let bought: Vec<EntityId> = members
		.iter()
		.filter(|member| {
			as_of_saturday
				.current(member.membership, &field("org.nibkeep.done"))
				.infallible()
				.iter()
				.any(|statement| statement.value == Value::Boolean(true))
		})
		.filter_map(|member| member.item)
		.collect();
	let chocolates: Vec<String> = bought
		.into_iter()
		.filter(|&item| {
			as_of_saturday
				.types(item)
				.infallible()
				.iter()
				.any(|kind| kind.as_str() == "org.example.shop.chocolate")
		})
		.flat_map(|item| {
			as_of_saturday
				.current(item, &field("org.nibkeep.name"))
				.infallible()
		})
		.filter_map(|statement| statement.value.as_text().map(str::to_owned))
		.collect();
	assert_eq!(chocolates, ["Marabou milk chocolate"]);
}
