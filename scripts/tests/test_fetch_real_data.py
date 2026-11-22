"""Tests of scripts/fetch-real-data.py, without the network.

    python3 -m unittest discover -s scripts/tests
"""

import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "fetch-real-data.py"
spec = importlib.util.spec_from_file_location("fetch_real_data", SCRIPT)
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)

ENTITY = "http://www.wikidata.org/entity/"


def uri(item):
	return {"type": "uri", "value": ENTITY + item}


def literal(value, lang=None):
	binding = {"type": "literal", "value": str(value)}
	if lang:
		binding["xml:lang"] = lang
	return binding


def statement(item, prop, value, **more):
	row = {"item": uri(item), "property": uri(prop), "value": value}
	row.update(more)
	return row


def time_row(item, prop, time, precision, calendar=fetch.GREGORIAN):
	return statement(
		item, prop, literal(time),
		time=literal(time), precision=literal(precision), calendar={"type": "uri", "value": calendar},
	)


def dated_row(item, prop, value, statement_id, **more):
	"""A row of a `dated statements` query. `more` maps start, end or point to a time and its
	precision, and anything else, such as an amount, to its binding."""
	row = statement(item, prop, value, statement=literal(statement_id))
	for name, binding in more.items():
		if name in ("start", "end", "point"):
			time, precision = binding
			row[name] = literal(time)
			row[name + "Precision"] = literal(precision)
			row[name + "Calendar"] = {"type": "uri", "value": fetch.GREGORIAN}
		else:
			row[name] = binding
	return row


class Dates(unittest.TestCase):
	def test_each_precision_becomes_edtf(self):
		self.assertEqual(fetch.edtf("1997-06-26T00:00:00Z", 11), "1997-06-26")
		self.assertEqual(fetch.edtf("1997-06-01T00:00:00Z", 10), "1997-06")
		self.assertEqual(fetch.edtf("1997-01-01T00:00:00Z", 9), "1997")
		self.assertEqual(fetch.edtf("1960-01-01T00:00:00Z", 8), "196X")

	def test_a_sign_before_the_year_is_allowed(self):
		self.assertEqual(fetch.edtf("+1997-06-26T00:00:00Z", 11), "1997-06-26")

	def test_what_edtf_cannot_say_simply_is_left_out(self):
		self.assertIsNone(fetch.edtf("1900-01-01T00:00:00Z", 7), "a century")
		self.assertIsNone(fetch.edtf("-0044-03-15T00:00:00Z", 11), "before the common era")
		self.assertIsNone(fetch.edtf("0000-01-01T00:00:00Z", 9), "the year zero")
		self.assertIsNone(fetch.edtf("not a time", 11))

	def test_a_day_of_another_calendar_comes_converted_but_a_month_keeps_only_its_year(self):
		julian = "http://www.wikidata.org/entity/Q1985786"
		self.assertEqual(fetch.edtf("1890-02-22T00:00:00Z", 11, julian), "1890-02-22")
		self.assertEqual(fetch.edtf("1890-02-01T00:00:00Z", 10, julian), "1890")

	def test_a_day_that_is_not_known_keeps_what_is(self):
		self.assertEqual(fetch.edtf("1997-06-00T00:00:00Z", 11), "1997-06")
		self.assertEqual(fetch.edtf("1997-00-00T00:00:00Z", 11), "1997")


class Languages(unittest.TestCase):
	def test_three_letters_become_two_where_there_are_two(self):
		self.assertEqual(fetch.language_tag("swe"), "sv")
		self.assertEqual(fetch.language_tag("deu"), "de")
		self.assertEqual(fetch.language_tag("ukr"), "uk")

	def test_a_language_without_two_letters_keeps_three(self):
		self.assertEqual(fetch.language_tag("nds"), "nds")

	def test_no_language_or_several_is_none(self):
		self.assertIsNone(fetch.language_tag(None))
		self.assertIsNone(fetch.language_tag("mul"))
		self.assertIsNone(fetch.language_tag("zxx"))


class Titles(unittest.TestCase):
	def test_titles_match_whatever_their_case_quotes_and_punctuation(self):
		self.assertEqual(
			fetch.normalize_title("Harry Potter and the Sorcerer’s Stone"),
			fetch.normalize_title("harry potter and the sorcerer's stone"),
		)
		self.assertEqual(
			fetch.normalize_title("Harry Potter och De Vises Sten"),
			fetch.normalize_title("Harry Potter och de vises sten"),
		)
		self.assertEqual(
			fetch.normalize_title("Babylons Ashes"), fetch.normalize_title("Babylon’s Ashes"),
		)
		self.assertEqual(
			fetch.normalize_title("Harry Potter and the Half‐Blood Prince"),
			fetch.normalize_title("Harry Potter and the Half-Blood Prince"),
		)


class Kinds(unittest.TestCase):
	def test_the_narrowest_kind_of_a_work_wins(self):
		self.assertEqual(fetch.choose_kind({"film", "episode"}), "episode")
		self.assertEqual(fetch.choose_kind({"book", "series"}), "series")
		self.assertEqual(fetch.choose_kind({"book", "game"}), "game")


class Statements(unittest.TestCase):
	def test_statements_become_dates_links_numbers_texts_and_credits(self):
		data = fetch.Data()
		data.entity("Q102438", "film")
		data.add_statements([
			time_row("Q102438", "P577", "2001-11-04T00:00:00Z", 11),
			time_row("Q102438", "P577", "2001-11-16T00:00:00Z", 11),
			statement("Q102438", "P179", uri("Q216930"), ordinal=literal("1")),
			statement("Q102438", "P2047", literal("152"), amount=literal("152"), unit=uri("Q7727")),
			statement("Q102438", "P161", uri("Q38119"), character=uri("Q3244512")),
			statement("Q102438", "P57", uri("Q295964")),
			statement("Q102438", "P161", {"type": "bnode", "value": "t1"}),
		])
		film = data.entities["Q102438"]
		self.assertEqual(film["dates"], {"published": ["2001-11-04", "2001-11-16"]})
		self.assertEqual(film["links"], {"series": ["Q216930"]})
		self.assertEqual(film["texts"], {"series_ordinal": [{"text": "1"}]})
		self.assertEqual(film["numbers"], {"duration_minutes": [152]})
		self.assertEqual(data.credits, {
			("Q102438", "Q38119", "cast", "Q3244512"),
			("Q102438", "Q295964", "director", None),
		})

	def test_hours_become_minutes_and_odd_units_are_left_out(self):
		data = fetch.Data()
		data.entity("Q1", "film")
		data.add_statements([
			statement("Q1", "P2047", literal("2.5"), amount=literal("2.5"), unit=uri("Q25235")),
			statement("Q1", "P2047", literal("3"), amount=literal("3"), unit=uri("Q573")),
		])
		self.assertEqual(data.entities["Q1"]["numbers"], {"duration_minutes": [150]})

	def test_a_birth_name_keeps_its_language(self):
		data = fetch.Data()
		data.entity("Q38119", "person")
		data.add_statements([
			statement("Q38119", "P1477", literal("Daniel Jacob Radcliffe", "en")),
			time_row("Q38119", "P569", "1989-07-23T00:00:00Z", 11),
		])
		person = data.entities["Q38119"]
		self.assertEqual(person["texts"], {"birth_name": [{"text": "Daniel Jacob Radcliffe", "lang": "en"}]})
		self.assertEqual(person["dates"], {"born": ["1989-07-23"]})

	def test_a_pen_name_has_its_writers_and_an_episode_its_season(self):
		data = fetch.Data()
		data.entity("Q6142591", "group")
		data.entity("Q61849523", "episode")
		data.add_statements([
			statement("Q6142591", "P527", uri("Q1159871")),
			statement("Q6142591", "P527", uri("Q18608460")),
			statement("Q61849523", "P4908", uri("Q25399422")),
		])
		self.assertEqual(data.entities["Q6142591"]["links"], {"members": ["Q1159871", "Q18608460"]})
		self.assertEqual(data.entities["Q61849523"]["links"], {"season": ["Q25399422"]})

	def test_the_same_value_twice_counts_once(self):
		data = fetch.Data()
		data.entity("Q1", "character")
		row = statement("Q1", "P22", uri("Q2"))
		data.add_statements([row, row])
		self.assertEqual(data.entities["Q1"]["links"], {"father": ["Q2"]})


class ValidTimes(unittest.TestCase):
	def test_a_start_and_an_end_make_a_period(self):
		self.assertEqual(fetch.period("1975-04", "2000-01"), "1975-04/2000-01")

	def test_a_period_without_an_end_still_holds_and_one_without_a_start_began_when_unknown(self):
		self.assertEqual(fetch.period("2014-02-04", None), "2014-02-04/..")
		self.assertEqual(fetch.period(None, "2003"), "/2003")

	def test_a_period_that_ends_before_it_starts_has_no_dates(self):
		self.assertIsNone(fetch.period("2005", "2001"))
		self.assertIsNone(fetch.period("2001-06", "2001-05-31"))
		self.assertEqual(fetch.period("2001-06", "2001"), "2001-06/2001", "within the year")
		self.assertEqual(fetch.period("196X", "1965-03"), "196X/1965-03", "within the decade")

	def test_neither_end_is_no_period(self):
		self.assertIsNone(fetch.period(None, None))


class DatedStatements(unittest.TestCase):
	def test_dated_statements_become_periods_and_events(self):
		data = fetch.Data()
		data.entity("Q2283", "group")
		data.entity("Q15920", "group")
		data.add_statements([
			dated_row(
				"Q2283", "P169", uri("Q5284"), "s1",
				start=("1975-04-01T00:00:00Z", 10), end=("2000-01-01T00:00:00Z", 10),
			),
			dated_row("Q2283", "P169", uri("Q19837"), "s2", start=("2014-02-04T00:00:00Z", 11)),
			dated_row("Q15920", "P527", uri("Q484030"), "s3", end=("2001-01-17T00:00:00Z", 11)),
			dated_row("Q15920", "P527", uri("Q106193"), "s4"),
			dated_row("Q15920", "P166", uri("Q179191"), "s5", point=("2009-01-01T00:00:00Z", 9)),
		])
		self.assertEqual(data.entities["Q2283"]["dated"], {"chief_executive": [
			{"link": "Q19837", "valid": "2014-02-04/.."},
			{"link": "Q5284", "valid": "1975-04/2000-01"},
		]})
		self.assertEqual(data.entities["Q15920"]["dated"], {
			"members": [{"link": "Q484030", "valid": "/2001-01-17"}],
			"award": [{"link": "Q179191", "valid": "2009"}],
		})
		self.assertEqual(data.entities["Q15920"]["links"], {"members": ["Q106193"]}, "no dates")

	def test_a_period_that_ends_before_it_starts_is_kept_without_dates(self):
		data = fetch.Data()
		data.entity("Q1", "person")
		data.add_statements([dated_row(
			"Q1", "P26", uri("Q2"), "s1", start=("2005-01-01T00:00:00Z", 9), end=("2001-01-01T00:00:00Z", 9),
		)])
		self.assertEqual(data.entities["Q1"]["links"], {"spouse": ["Q2"]})
		self.assertNotIn("dated", data.entities["Q1"])

	def test_a_statement_with_several_dates_counts_once_with_the_earliest(self):
		data = fetch.Data()
		data.entity("Q1", "person")
		data.add_statements([
			dated_row("Q1", "P26", uri("Q2"), "s1", start=("1999-01-01T00:00:00Z", 9)),
			dated_row("Q1", "P26", uri("Q2"), "s1", start=("1998-01-01T00:00:00Z", 9)),
			dated_row("Q1", "P26", uri("Q3"), "s2"),
			dated_row("Q1", "P26", uri("Q3"), "s2", start=("2005-01-01T00:00:00Z", 9)),
		])
		self.assertEqual(data.entities["Q1"]["dated"], {"spouse": [
			{"link": "Q2", "valid": "1998/.."}, {"link": "Q3", "valid": "2005/.."},
		]})

	def test_a_count_of_employees_is_kept_only_with_its_date(self):
		data = fetch.Data()
		data.entity("Q2283", "group")
		one = uri("Q199")
		data.add_statements([
			dated_row(
				"Q2283", "P1128", literal("221000"), "s1", point=("2022-01-01T00:00:00Z", 9),
				amount=literal("221000"), unit=one,
			),
			dated_row(
				"Q2283", "P1128", literal("118000"), "s2", point=("2015-06-30T00:00:00Z", 11),
				amount=literal("118000"), unit=one,
			),
			dated_row("Q2283", "P1128", literal("5"), "s3", amount=literal("5"), unit=one),
		])
		self.assertEqual(data.entities["Q2283"]["dated"], {"employees": [
			{"number": 118000, "valid": "2015-06-30"},
			{"number": 221000, "valid": "2022"},
		]})
		self.assertNotIn("numbers", data.entities["Q2283"])

	def test_the_targets_of_links_with_dates_and_without(self):
		data = fetch.Data()
		data.entity("Q1", "group")["links"] = {"members": ["Q2"], "genre": ["Q9"]}
		data.entities["Q1"]["dated"] = {"members": [{"link": "Q3", "valid": "1990/.."}]}
		data.entity("Q4", "group")["dated"] = {"employees": [{"number": 5, "valid": "2001"}]}
		self.assertEqual(fetch.targets(data, ["Q1", "Q4"], {"members"}), {"Q2", "Q3"})


class Names(unittest.TestCase):
	def test_labels_come_before_aliases_in_the_order_of_the_languages(self):
		data = fetch.Data()
		data.entity("Q3244512", "character")
		data.add_names([
			{"item": uri("Q3244512"), "name": literal("Гаррі Поттер", "uk"), "alias": literal("false")},
			{"item": uri("Q3244512"), "name": literal("The Boy Who Lived", "en"), "alias": literal("true")},
			{"item": uri("Q3244512"), "name": literal("Harry Potter", "en"), "alias": literal("false")},
			{"item": uri("Q3244512"), "name": literal("Harry Potter", "mul"), "alias": literal("false")},
			{"item": uri("Q3244512"), "name": literal("Harry Potter", "en"), "alias": literal("false")},
		])
		self.assertEqual(data.entities["Q3244512"]["names"], [
			{"text": "Harry Potter", "lang": "en"},
			{"text": "Гаррі Поттер", "lang": "uk"},
			{"text": "Harry Potter", "lang": "mul"},
			{"text": "The Boy Who Lived", "lang": "en", "alias": True},
		])


class Audiobooks(unittest.TestCase):
	def release(self, group, title, language, date, *artists):
		return {
			"title": title,
			"date": date,
			"text-representation": {"language": language} if language else {},
			"release-group": {"id": group, "title": title},
			"artist-credit": [{"name": name, "artist": {"id": mbid, "name": name}} for mbid, name in artists],
			"tags": [{"name": "kept nowhere", "count": 1}],
		}

	def test_releases_of_one_group_make_one_audiobook_read_by_its_narrators(self):
		data = fetch.Data()
		data.entity("Q43361", "book")
		books = {fetch.normalize_title("Harry Potter och De vises sten"): "Q43361"}
		rowling = ("a-rowling", "J. K. Rowling")
		henriksson = ("a-henriksson", "Krister Henriksson")
		narrators = fetch.add_audiobooks(data, [
			self.release("g-sv", "Harry Potter och De Vises Sten", "swe", "2004-04-01", rowling, henriksson),
			self.release("g-sv", "Harry Potter och De Vises Sten", "swe", "2004", rowling, henriksson),
			self.release("g-x", "Something Else", None, "", rowling),
		], {"a-rowling": "Q34660"}, books)
		self.assertEqual(narrators, {"a-henriksson"})
		audiobook = data.entities["mb:g-sv"]
		self.assertEqual(audiobook["kind"], "audiobook")
		self.assertEqual(audiobook["names"], [{"text": "Harry Potter och De Vises Sten", "lang": "sv"}])
		self.assertEqual(audiobook["dates"], {"published": ["2004"]})
		self.assertEqual(audiobook["links"], {"reads": ["Q43361"]})
		self.assertEqual(data.entities["mb:a-henriksson"]["names"], [{"text": "Krister Henriksson"}])
		self.assertIn(("mb:g-sv", "Q34660", "author", None), data.credits)
		self.assertIn(("mb:g-sv", "mb:a-henriksson", "narrator", None), data.credits)
		self.assertNotIn("links", data.entities["mb:g-x"])
		self.assertNotIn("tags", json.dumps(data.entities))

	def test_an_authors_pen_names_are_artists_named_after_them_or_who_are_them(self):
		artist = {"relations": [
			{"type": "named after artist", "direction": "backward", "artist": {"id": "a-galbraith", "name": "Robert Galbraith"}},
			{"type": "is person", "direction": "forward", "artist": {"id": "a-other", "name": "Another Name"}},
			{"type": "is person", "direction": "backward", "artist": {"id": "a-legal", "name": "A Legal Name"}},
			{"type": "named after artist", "direction": "forward", "artist": {"id": "a-namesake", "name": "Whom They Are Named After"}},
			{"type": "collaboration", "direction": "forward", "artist": {"id": "a-band", "name": "A Band"}},
			{"type": "named after artist", "direction": "backward", "url": {"resource": "https://example.org"}},
		]}
		self.assertEqual(fetch.pen_names(artist), {"a-galbraith": "Robert Galbraith", "a-other": "Another Name"})

	def test_a_narrator_whom_wikidata_knows_becomes_that_person(self):
		data = fetch.Data()
		data.entity("mb:g", "audiobook")
		data.entity("mb:a", "person")["names"].append({"text": "Stephen Fry"})
		data.credits.add(("mb:g", "mb:a", "narrator", None))
		data.rename("mb:a", "Q192912")
		self.assertNotIn("mb:a", data.entities)
		self.assertNotIn("Q192912", data.entities, "made once Wikidata says what it is")
		self.assertEqual(data.credits, {("mb:g", "Q192912", "narrator", None)})
		data.entity("Q192912", "person")
		data.name_unnamed()
		self.assertEqual(data.entities["Q192912"]["names"], [{"text": "Stephen Fry"}], "MusicBrainz' name, if Wikidata has none")


class Pruning(unittest.TestCase):
	def test_what_points_outside_the_data_or_has_no_name_goes(self):
		data = fetch.Data()
		for item, kind in (("Q1", "film"), ("Q2", "person"), ("Q3", "character"), ("Q4", "person")):
			data.entity(item, kind)["names"].append({"text": item, "lang": "en"})
		data.entity("Q5", "person")
		data.entities["Q1"]["links"] = {"series": ["Q9"], "based_on": ["Q3"]}
		data.credits |= {
			("Q1", "Q2", "cast", "Q8"),
			("Q1", "Q5", "cast", None),
			("Q1", "Q4", "cast", "Q3"),
			("Q9", "Q2", "director", None),
		}
		data.prune()
		self.assertNotIn("Q5", data.entities, "an entity without a name")
		self.assertEqual(data.entities["Q1"]["links"], {"based_on": ["Q3"]})
		self.assertEqual(data.credits, {("Q1", "Q2", "cast", None), ("Q1", "Q4", "cast", "Q3")})

	def test_dated_links_outside_the_data_go_and_dated_numbers_stay(self):
		data = fetch.Data()
		for item in ("Q1", "Q2"):
			data.entity(item, "person")["names"].append({"text": item})
		data.entities["Q1"]["dated"] = {
			"employer": [{"link": "Q9", "valid": "2001/2003"}],
			"spouse": [{"link": "Q2", "valid": "1990/.."}, {"link": "Q8", "valid": "1980/1985"}],
		}
		data.entity("Q3", "group")["names"].append({"text": "Q3"})
		data.entities["Q3"]["dated"] = {"employees": [{"number": 5, "valid": "2001"}]}
		data.prune()
		self.assertEqual(data.entities["Q1"]["dated"], {"spouse": [{"link": "Q2", "valid": "1990/.."}]})
		self.assertEqual(data.entities["Q3"]["dated"], {"employees": [{"number": 5, "valid": "2001"}]})


class Household(unittest.TestCase):
	def setUp(self):
		self.household = {entity["id"]: entity for entity in fetch.household()}

	def readings(self, id, field):
		return self.household[id]["dated"][field]

	def test_two_children_three_years_apart_and_a_dog(self):
		self.assertEqual(sorted(self.household), ["sim:bruno", "sim:leo", "sim:nils"])
		leo, nils, bruno = (self.household[id] for id in ("sim:leo", "sim:nils", "sim:bruno"))
		self.assertEqual((leo["kind"], nils["kind"], bruno["kind"]), ("person", "person", "animal"))
		self.assertEqual(int(nils["dates"]["born"][0][:4]) - int(leo["dates"]["born"][0][:4]), 3)
		self.assertEqual(leo["links"], {"sibling": ["sim:nils"]})
		self.assertEqual(nils["links"], {"sibling": ["sim:leo"]})
		for entity in self.household.values():
			self.assertTrue(entity["names"])

	def test_the_same_household_every_time(self):
		self.assertEqual(fetch.household(), fetch.household())

	def test_readings_are_quantities_within_the_ten_years(self):
		for id, field in (("sim:leo", "height"), ("sim:nils", "height"), ("sim:bruno", "weight")):
			readings = self.readings(id, field)
			self.assertGreater(len(readings), 60, id)
			for reading in readings:
				number, unit = reading["quantity"].split(" ")
				float(number)
				self.assertIn(unit, ("cm", "[in_i]", "kg"))
				self.assertTrue("2016" <= reading["valid"][:4] <= "2025", reading)
			self.assertEqual(len({reading["valid"][:10] for reading in readings}), len(readings), "one a day")
		frame = [
			reading["valid"][:7] for reading in self.readings("sim:leo", "height")
			if reading["quantity"].endswith(".0 cm") or reading["quantity"].endswith(".5 cm")
		]
		self.assertIn("2018-12", frame)
		self.assertIn("2019-02", frame)
		inches = [reading["quantity"] for reading in self.readings("sim:leo", "height") if "[in_i]" in reading["quantity"]]
		self.assertFalse(any(text.split(" ")[0].endswith("0") for text in inches), "no digits beyond what was read")

	def test_the_dog_is_weighed_weekly_by_an_app_and_now_and_then_at_the_vet(self):
		readings = self.readings("sim:bruno", "weight")
		at_home = [reading for reading in readings if "T" in reading["valid"]]
		at_the_vet = [reading for reading in readings if "T" not in reading["valid"]]
		self.assertGreater(len(at_home), 400)
		self.assertGreater(len(at_the_vet), 15)
		self.assertRegex(at_home[0]["valid"], r"^2016-[0-9]{2}-[0-9]{2}T0[0-9]:[0-9]{2}:00\+0[12]$")
		weights = [float(reading["quantity"].split(" ")[0]) for reading in readings]
		self.assertLess(weights[0], 8, "a puppy")
		self.assertTrue(all(18 < weight < 30 for weight in weights[-100:]), "a grown dog")

	def test_children_grow(self):
		for id in ("sim:leo", "sim:nils"):
			centimetres = [
				float(reading["quantity"].split(" ")[0]) * (2.54 if reading["quantity"].endswith("[in_i]") else 1)
				for reading in sorted(self.readings(id, "height"), key=lambda reading: reading["valid"])
			]
			self.assertGreater(centimetres[-1] - centimetres[0], 40, id)
			self.assertTrue(all(later > earlier - 3 for earlier, later in zip(centimetres, centimetres[1:])), id)
		units = {reading["quantity"].split(" ")[1] for reading in self.readings("sim:leo", "height")}
		self.assertEqual(units, {"cm", "[in_i]"}, "a grandparent measures in inches")


class Cache(unittest.TestCase):
	def test_a_broken_answer_is_removed_to_be_fetched_again(self):
		with tempfile.TemporaryDirectory() as directory:
			path = pathlib.Path(directory) / "ab" / "answer.json"
			self.assertIsNone(fetch.read_cached(path))
			fetch.write_cached(path, {"results": [1]})
			self.assertEqual(fetch.read_cached(path), {"results": [1]})
			self.assertEqual([each.name for each in path.parent.iterdir()], ["answer.json"])
			path.write_text('{"results": [1')
			self.assertIsNone(fetch.read_cached(path))
			self.assertFalse(path.exists())


class Writing(unittest.TestCase):
	@staticmethod
	def make():
		data = fetch.Data()
		data.entity("Q2", "person")["names"].append({"text": "Two", "lang": "en"})
		data.entity("Q10", "film")["names"].append({"text": "Ten", "lang": "en"})
		data.credits.add(("Q10", "Q2", "cast", None))
		return data

	def test_the_same_data_writes_the_same_files_and_hash(self):
		with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
			one = self.make().write(pathlib.Path(first))
			two = self.make().write(pathlib.Path(second))
			self.assertEqual(one, two)
			lines = (pathlib.Path(first) / "entities.jsonl").read_text().splitlines()
			self.assertEqual([json.loads(line)["id"] for line in lines], ["Q10", "Q2"], "films first")
			self.assertEqual(
				json.loads((pathlib.Path(first) / "credits.jsonl").read_text()),
				{"work": "Q10", "person": "Q2", "role": "cast"},
			)

	def test_the_hash_is_of_the_bytes_written_which_end_lines_with_a_line_feed_on_every_system(self):
		# Windows would end each line with \r\n, which the hash does not count.
		with tempfile.TemporaryDirectory() as directory:
			directory = pathlib.Path(directory)
			written = self.make().write(directory)
			on_disk = hashlib.sha256()
			for name in ("entities.jsonl", "credits.jsonl"):
				content = (directory / name).read_bytes()
				self.assertNotIn(b"\r", content, name)
				on_disk.update(name.encode() + b"\0" + hashlib.sha256(content).digest())
			self.assertEqual(on_disk.hexdigest(), written)


if __name__ == "__main__":
	unittest.main()
