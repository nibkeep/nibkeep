"""Fetches real data for the benchmarks from Wikidata and MusicBrainz (docs/Plan.md,
section 4).

    python3 scripts/fetch-real-data.py [--depth N] [--max-works N] [--output DIRECTORY]

The data starts from two media franchises, the Wizarding World of Harry Potter and The
Expanse: their books and series of books, films, television series with their seasons
and episodes, games and plays; the characters of their universes; the people credited in
them, and the characters they played; the groups credited, such as studios, and pen names
with the writers behind them; and the audiobooks of the books' authors, with their
narrators. Each step of `--depth`, 0 by default, goes one step further out: the other
films of the people found so far, and the people credited in those; `--max-works` stops the
steps at that many works.

To these come seven companies, with the games they made or published, and seven bands
whose members changed; who broadcast each work and who distributes it; and what changed
over time, with its dates: the members of groups, the founders, chief executives and chairs
of companies, and how many people they employ; and the spouses, partners, employers,
memberships and awards of people. The spouses and partners found join the data, with their
own dates of birth and death. Last comes a household of the library's user, simulated from
a fixed seed, as no source holds one: two children three years apart, measured as they
grow, and a dog, weighed every week by an app and now and then at the vet, from 2016 to
2025. With the first step, the data comes to about 138,000 entities, counting each credit as
one, in some fifty minutes.

Names come in English, Russian, Ukrainian and Swedish, as labels and aliases, and in the
form that Wikidata gives for every language (`mul`), which is how it often names people;
an audiobook's title is in the audiobook's language. Dates are in EDTF, as precise as
Wikidata has them.

It writes, into DIRECTORY, `real-data/depth-N` by default, which Git ignores:

- `entities.jsonl`, one entity per line: its `id` (a Wikidata item, `mb:` and a
  MusicBrainz ID, or `sim:` and a name for the household), its `kind`, its `names`, its
  `dates`, `numbers`, `texts` and `links` to other entities, each by name, and its `dated`
  values, each a `link`, a whole `number` or a `quantity` with its unit, such as `12.1 kg`,
  and the `valid` time when it held, in EDTF: a period, or the moment it happened;
- `credits.jsonl`, one credit per line: the `work`, the `person` or group, the `role`, and
  the `character` played, if any;
- `manifest.json`: the sources, the queries, when the first and the last of the answers
  it was made from were fetched, how many entities of each kind it has, and its hash,
  which names the data in the benchmarks' results.

Every line ends with a line feed, on Windows too, so that the same data is the same bytes
on every system and the hash in the manifest is that of the files.

Links and credits point only at entities in the data, and every entity has a name. Answers
are kept in real-data/cache, so that a run that was cut short goes on where it stopped,
and runs of several depths share what they fetched; remove the cache to fetch afresh.

Wikidata is in the public domain (CC0), and so is MusicBrainz' core data; MusicBrainz'
supplementary data, such as tags, has another license, and none of it is kept. Requests go
one at a time, as both services ask; MusicBrainz takes one a second. They name the script,
and the contact in NIBKEEP_CONTACT, if set, as both services would like.
"""

import argparse
import collections
import datetime
import hashlib
import http.client
import json
import math
import os
import pathlib
import re
import sys
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
DATA = ROOT / "real-data"

WIKIDATA = "https://query.wikidata.org/sparql"
MUSICBRAINZ = "https://musicbrainz.org/ws/2/"
ENTITY = "http://www.wikidata.org/entity/"
GREGORIAN = ENTITY + "Q1985727"
HUMAN = "Q5"
FICTIONAL_CHARACTER = "Q95074"

# The languages of the names, in the order they are written; and Wikidata's code for a
# name that is the same in every language.
LANGUAGES = ("en", "ru", "uk", "sv")
EVERY_LANGUAGE = "mul"

# Where the data starts: each franchise, the fictional universes of its stories, and its
# series of works, whose parts belong to it even when they do not name the franchise.
SEEDS = {
	"Wizarding World": {"franchise": "Q30739117", "universes": ["Q5410773"], "series": ["Q8337", "Q216930"]},
	"The Expanse": {"franchise": "Q131519400", "universes": [], "series": ["Q19610143", "Q18389644"]},
}

# Companies whose games join the data, with their founders and leaders, and bands whose
# members changed in the last thirty years.
COMPANIES = {
	"Microsoft": "Q2283", "Apple": "Q312", "Nvidia": "Q182477", "AMD": "Q128896",
	"Google": "Q95", "Epic Games": "Q739711", "Ubisoft": "Q188273",
}
BANDS = {
	"Metallica": "Q15920", "Linkin Park": "Q261", "Nightwish": "Q43510",
	"Iron Maiden": "Q42482", "AC/DC": "Q27593", "Slipknot": "Q254138",
	"Evanescence": "Q1777698",
}
VIDEO_GAME = "Q7889"

# The kinds of works, the narrowest first, each with the classes its items are instances
# of, or of a subclass of.
WORK_KINDS = (
	("episode", ("Q21191270",)),
	("season", ("Q3464665",)),
	("tv series", ("Q5398426",)),
	("series", ("Q1667921", "Q277759", "Q24856", "Q7058673")),
	("game", ("Q7889",)),
	("film", ("Q11424",)),
	("play", ("Q25379", "Q58483083")),
	("book", ("Q7725634",)),
)

# The classes of the films that further steps go to.
FILMS = ("Q11424", "Q24869", "Q202866", "Q506240", "Q24862", "Q93204")

# Who is credited in a work, by property, and as what.
CREDITS = {
	"P50": "author", "P57": "director", "P58": "screenwriter", "P86": "composer",
	"P162": "producer", "P161": "cast", "P725": "voice", "P178": "developer", "P123": "publisher",
}

# The statements fetched of each kind of entity, by property, and their names in the data.
WORK_PROPERTIES = {
	"P577": "published", "P179": "series", "P4908": "season", "P144": "based_on",
	"P136": "genre", "P400": "platform", "P2047": "duration", "P1104": "pages", **CREDITS,
}
PERSON_PROPERTIES = {"P569": "born", "P570": "died", "P1477": "birth_name"}
GROUP_PROPERTIES = {"P571": "founded", "P576": "dissolved"}
CHARACTER_PROPERTIES = {
	"P22": "father", "P25": "mother", "P26": "spouse", "P3373": "sibling", "P40": "child",
	"P1441": "appears_in",
}
# The statements fetched with when they held, from their start and end, or when they
# happened; those that say neither are fetched as the others are.
WORK_DATED = {"P449": "broadcaster", "P750": "distributor"}
GROUP_DATED = {
	"P527": "members", "P112": "founded_by", "P169": "chief_executive", "P488": "chairperson",
	"P1128": "employees",
}
PERSON_DATED = {
	"P26": "spouse", "P451": "partner", "P108": "employer", "P463": "member_of", "P166": "award",
}
# Links whose targets are fetched by name, as terms, rather than followed.
TERMS = {"genre", "platform", "award"}
# Measurements, which say nothing without when they were made, and are left out then.
MEASURES = {"employees"}
NAMES = {
	**WORK_PROPERTIES, **PERSON_PROPERTIES, **GROUP_PROPERTIES, **CHARACTER_PROPERTIES,
	**WORK_DATED, **GROUP_DATED, **PERSON_DATED,
}

# Units of durations, by item, in minutes; and the unit of plain numbers.
MINUTES = {"Q7727": 1, "Q25235": 60, "Q11574": 1 / 60}
ONE = "Q199"

# The order of kinds in entities.jsonl.
KIND_ORDER = (
	"series", "tv series", "season", "episode", "book", "film", "game", "play", "audiobook",
	"character", "person", "group", "animal", "term",
)

# ISO 639-3 codes as MusicBrainz gives them, and their two-letter tags where they have one.
TWO_LETTERS = {
	"ces": "cs", "dan": "da", "deu": "de", "ell": "el", "eng": "en", "spa": "es", "est": "et",
	"fin": "fi", "fra": "fr", "heb": "he", "hun": "hu", "ita": "it", "jpn": "ja", "kor": "ko",
	"lit": "lt", "lav": "lv", "nob": "nb", "nld": "nl", "nno": "nn", "nor": "no", "pol": "pl",
	"por": "pt", "ron": "ro", "rus": "ru", "slk": "sk", "slv": "sl", "swe": "sv", "tur": "tr",
	"ukr": "uk", "zho": "zh", "isl": "is", "cat": "ca", "bul": "bg", "hrv": "hr", "srp": "sr",
	"bel": "be", "kat": "ka", "ara": "ar", "hin": "hi",
}
# How MusicBrainz relates an artist to the names it writes under, seen from the artist:
# the legal name of a person who performs under other names, and the artist that another,
# such as Robert Galbraith, is named after.
PEN_NAMES = {("is person", "forward"), ("named after artist", "backward")}

# Codes that name no single language: several, none, or not known.
NO_LANGUAGE = {"mul", "zxx", "und", "mis"}

QUERIES = {
	"seed works": """
SELECT DISTINCT ?work ?kind WHERE {
  { ?work wdt:P8345 ?franchise . VALUES ?franchise { %(franchises)s } }
  UNION { ?work wdt:P1080 ?universe . VALUES ?universe { %(universes)s } }
  UNION { ?work wdt:P179 ?series . VALUES ?series { %(series)s } }
  UNION { VALUES ?work { %(series)s } }
  VALUES (?kind ?class) { %(kinds)s }
  ?work wdt:P31/wdt:P279* ?class .
}""",
	"seed characters": """
SELECT DISTINCT ?character WHERE {
  { ?character wdt:P8345 ?franchise . VALUES ?franchise { %(franchises)s } }
  UNION { ?character wdt:P1080 ?universe . VALUES ?universe { %(universes)s } }
  UNION { ?character wdt:P1441 ?work . VALUES ?work { %(works)s } }
  ?character wdt:P31/wdt:P279* wd:%(character)s .
}""",
	"statements": """
SELECT ?item ?property ?value ?time ?precision ?calendar ?amount ?unit ?ordinal ?character WHERE {
  VALUES ?item { %(items)s }
  VALUES ?property { %(properties)s }
  ?property wikibase:claim ?claim ; wikibase:statementProperty ?simple ;
    wikibase:statementValue ?full .
  ?item ?claim ?statement .
  ?statement ?simple ?value ; wikibase:rank ?rank .
  FILTER(?rank != wikibase:DeprecatedRank)
  OPTIONAL { ?statement ?full ?node . ?node wikibase:timeValue ?time ;
    wikibase:timePrecision ?precision ; wikibase:timeCalendarModel ?calendar . }
  OPTIONAL { ?statement ?full ?quantity . ?quantity wikibase:quantityAmount ?amount ;
    wikibase:quantityUnit ?unit . }
  OPTIONAL { ?statement pq:P1545 ?ordinal }
  OPTIONAL { ?statement pq:P453 ?character }
}""",
	"dated statements": """
SELECT ?item ?statement ?property ?value ?amount ?unit
  ?start ?startPrecision ?startCalendar ?end ?endPrecision ?endCalendar
  ?point ?pointPrecision ?pointCalendar WHERE {
  VALUES ?item { %(items)s }
  VALUES ?property { %(properties)s }
  ?property wikibase:claim ?claim ; wikibase:statementProperty ?simple ;
    wikibase:statementValue ?full .
  ?item ?claim ?statement .
  ?statement ?simple ?value ; wikibase:rank ?rank .
  FILTER(?rank != wikibase:DeprecatedRank)
  OPTIONAL { ?statement ?full ?quantity . ?quantity wikibase:quantityAmount ?amount ;
    wikibase:quantityUnit ?unit . }
  OPTIONAL { ?statement pqv:P580 ?startNode . ?startNode wikibase:timeValue ?start ;
    wikibase:timePrecision ?startPrecision ; wikibase:timeCalendarModel ?startCalendar . }
  OPTIONAL { ?statement pqv:P582 ?endNode . ?endNode wikibase:timeValue ?end ;
    wikibase:timePrecision ?endPrecision ; wikibase:timeCalendarModel ?endCalendar . }
  OPTIONAL { ?statement pqv:P585 ?pointNode . ?pointNode wikibase:timeValue ?point ;
    wikibase:timePrecision ?pointPrecision ; wikibase:timeCalendarModel ?pointCalendar . }
}""",
	"games of companies": """
SELECT DISTINCT ?work WHERE {
  VALUES ?company { %(companies)s }
  ?work wdt:P178|wdt:P123 ?company ; wdt:P31 wd:%(game)s .
}""",
	"names": """
SELECT ?item ?name ?alias WHERE {
  VALUES ?item { %(items)s }
  { ?item rdfs:label ?name . BIND(false AS ?alias) }
  UNION { ?item skos:altLabel ?name . BIND(true AS ?alias) }
  FILTER(LANG(?name) IN (%(languages)s))
}""",
	"any name": """
SELECT ?item (SAMPLE(?label) AS ?name) WHERE {
  VALUES ?item { %(items)s }
  ?item rdfs:label ?label .
} GROUP BY ?item""",
	"humans": """
SELECT ?item WHERE { VALUES ?item { %(items)s } ?item wdt:P31 wd:%(human)s . }""",
	"musicbrainz artists": """
SELECT ?item ?artist WHERE { VALUES ?item { %(items)s } ?item wdt:P434 ?artist . }""",
	"items of musicbrainz artists": """
SELECT ?item ?artist WHERE { VALUES ?artist { %(artists)s } ?item wdt:P434 ?artist . }""",
	"book titles": """
SELECT ?item ?name WHERE { VALUES ?item { %(items)s } ?item rdfs:label|skos:altLabel ?name . }""",
	"films of people": """
SELECT DISTINCT ?work WHERE {
  VALUES ?person { %(items)s }
  VALUES ?class { %(films)s }
  ?work wdt:P161|wdt:P57|wdt:P58|wdt:P86|wdt:P725 ?person ; wdt:P31 ?class .
}""",
	"pen names": "artist/{artist}?inc=artist-rels&fmt=json",
	"audiobooks": "release?query=arid:{artist} AND secondarytype:audiobook&fmt=json&limit=100&offset={offset}",
}


def qid(value):
	"""The item a URI names, such as `Q42`, or None for anything else."""
	if isinstance(value, dict):
		value = value.get("value") if value.get("type") == "uri" else None
	if value and value.startswith(ENTITY):
		item = value[len(ENTITY):]
		if re.fullmatch(r"[PQ][0-9]+", item):
			return item
	return None


def edtf(value, precision, calendar=GREGORIAN):
	"""A Wikidata time, as the query service gives it, as an EDTF date, as precise as
	Wikidata has it, or None when EDTF cannot say it simply: before the common era, or less
	precise than a decade."""
	match = re.fullmatch(r"\+?([0-9]{1,4})-([0-9]{2})-([0-9]{2})T.*", value)
	if not match or int(match[1]) == 0:
		return None
	year, month, day = int(match[1]), match[2], match[3]
	precision = int(precision)
	# The query service gives a day of another calendar, such as the Julian, converted to
	# the Gregorian, but not a month, which only its year says rightly.
	if calendar != GREGORIAN and precision == 10:
		precision = 9
	if precision >= 11 and day == "00":
		precision = 10
	if precision >= 10 and month == "00":
		precision = 9
	if precision >= 11:
		return f"{year:04}-{month}-{day}"
	if precision == 10:
		return f"{year:04}-{month}"
	if precision == 9:
		return f"{year:04}"
	if precision == 8:
		return f"{year // 10:03}X"
	return None


def bounds(date):
	"""The first and the last day that a date as edtf() writes it may be, as (year, month,
	day), with every month taken to have 31 days, which orders days all the same."""
	if date.endswith("X"):
		decade = int(date[:3]) * 10
		return (decade, 1, 1), (decade + 9, 12, 31)
	parts = [int(part) for part in date.split("-")]
	first = (*parts, *(1, 1)[len(parts) - 1:])
	last = (*parts, *(12, 31)[len(parts) - 1:])
	return first, last


def period(start, end):
	"""The valid time of a state from its start and end, as edtf() writes them: a period that
	still holds without an end, and that began when nobody knows without a start. None
	without either, or when it would end before it starts, which NibKeep refuses."""
	if start is None and end is None:
		return None
	if start and end and bounds(start)[0] > bounds(end)[1]:
		return None
	return f"{start or ''}/{end or '..'}"


def valid_time(row):
	"""When the statement of a `dated statements` row held: a period, from its start and end,
	or the point in time it happened. None if it says neither, or nothing NibKeep takes."""
	def date(name):
		if name not in row:
			return None
		return edtf(text_of(row[name]), text_of(row[name + "Precision"]), text_of(row.get(name + "Calendar")))

	start, end = date("start"), date("end")
	if start or end:
		return period(start, end)
	return date("point")


def first_rows(rows):
	"""The rows of a query, one for each statement where the query names them: of a statement
	with several dates, the row with the earliest, a date coming before none."""
	unnamed, named = [], {}
	for row in rows:
		if "statement" not in row:
			unnamed.append(row)
			continue
		times = tuple(
			(name not in row, text_of(row.get(name)) or "") for name in ("start", "end", "point")
		)
		statement = text_of(row["statement"])
		if statement not in named or times < named[statement][0]:
			named[statement] = (times, row)
	return unnamed + [row for _, row in named.values()]


def language_tag(code):
	"""A MusicBrainz language as a BCP 47 tag, or None when it names no single language."""
	if not code or code in NO_LANGUAGE:
		return None
	return TWO_LETTERS.get(code, code)


def normalize_title(title):
	"""A title as titles compare: in one case, with one kind of hyphen, and without
	apostrophes, which titles drop at times, or other punctuation."""
	title = unicodedata.normalize("NFKC", title).casefold()
	title = re.sub(r"['’‘`´]", "", title)
	title = re.sub(r"[‐‑‒–—]", "-", title)
	return " ".join(re.sub(r"[^\w-]+", " ", title).split())


def choose_kind(kinds):
	"""The narrowest of the kinds that a work is of."""
	return next(kind for kind, _ in WORK_KINDS if kind in kinds)


def read_cached(path):
	"""A cached answer, or None if there is none or it is broken, as a run stopped while
	writing it could leave it; a broken one is removed."""
	try:
		return json.loads(path.read_text(encoding="utf-8"))
	except FileNotFoundError:
		return None
	except (json.JSONDecodeError, UnicodeDecodeError):
		path.unlink()
		return None


def write_cached(path, answer):
	"""Caches an answer whole or not at all."""
	path.parent.mkdir(parents=True, exist_ok=True)
	partial = path.with_suffix(".partial")
	partial.write_text(json.dumps(answer), encoding="utf-8")
	os.replace(partial, path)


def text_of(binding):
	return binding["value"] if binding else None


class Data:
	"""The entities and credits fetched so far."""

	def __init__(self):
		self.entities = {}
		self.credits = set()
		# Names to give entities that the sources name nowhere else.
		self.fallback_names = {}

	def entity(self, id, kind):
		"""The entity, made with this kind if it is new; an entity keeps its first kind."""
		if id not in self.entities:
			self.entities[id] = {"id": id, "kind": kind, "names": []}
		return self.entities[id]

	def of_kind(self, *kinds):
		return sorted(id for id, entity in self.entities.items() if entity["kind"] in kinds)

	def add_statements(self, rows):
		"""Adds the statements of a `statements` or a `dated statements` query to the entities
		they are about: those with dates as dated values, and the others as plain ones."""
		for row in first_rows(rows):
			entity = self.entities.get(qid(row["item"]))
			prop = qid(row["property"])
			if entity is None or prop not in NAMES:
				continue
			name = NAMES[prop]
			value = row["value"]
			valid = valid_time(row)
			if prop in CREDITS:
				person = qid(value)
				if person:
					self.credits.add((entity["id"], person, name, qid(row.get("character"))))
			elif valid and qid(value):
				add_dated(entity, name, {"link": qid(value), "valid": valid})
			elif valid and "amount" in row:
				amount, unit = float(text_of(row["amount"])), qid(row.get("unit"))
				if unit == ONE and amount.is_integer():
					add_dated(entity, name, {"number": int(amount), "valid": valid})
			elif name in MEASURES:
				continue
			elif "time" in row:
				date = edtf(text_of(row["time"]), text_of(row["precision"]), text_of(row.get("calendar")))
				if date:
					add(entity, "dates", name, date)
			elif "amount" in row:
				amount, unit = float(text_of(row["amount"])), qid(row.get("unit"))
				if name == "duration" and unit in MINUTES:
					add(entity, "numbers", "duration_minutes", round(amount * MINUTES[unit]))
				elif name != "duration" and unit == ONE and amount.is_integer():
					add(entity, "numbers", name, int(amount))
			elif value.get("type") == "literal" and value.get("xml:lang"):
				add(entity, "texts", name, {"text": value["value"], "lang": value["xml:lang"]})
			elif qid(value):
				add(entity, "links", name, qid(value))
				if name == "series" and row.get("ordinal"):
					add(entity, "texts", "series_ordinal", {"text": text_of(row["ordinal"])})

	def add_names(self, rows):
		"""Adds the labels and aliases of a `names` query: labels before aliases, each in
		the order of the languages."""
		order = {language: index for index, language in enumerate((*LANGUAGES, EVERY_LANGUAGE))}
		found = collections.defaultdict(list)
		for row in rows:
			name = row["name"]
			alias = text_of(row.get("alias")) == "true"
			language = name.get("xml:lang")
			found[qid(row["item"])].append((alias, order.get(language, len(order)), name["value"], language))
		for item, names in found.items():
			entity = self.entities.get(item)
			if entity is None:
				continue
			for alias, _, text, language in sorted(set(names)):
				name = {"text": text, "lang": language} if language else {"text": text}
				if alias:
					name["alias"] = True
				if name not in entity["names"]:
					entity["names"].append(name)

	def rename(self, old, new):
		"""Makes `old` the item `new`, as when a MusicBrainz artist turns out to be a
		Wikidata item: its credits go to `new`, and its names, if Wikidata has none. The
		entity of `new` is made once Wikidata says what it is."""
		self.fallback_names[new] = self.entities.pop(old)["names"]
		self.credits = {
			tuple(new if part == old else part for part in credit) for credit in self.credits
		}

	def name_unnamed(self):
		"""Gives entities without a name the names that were put aside for them."""
		for id, names in self.fallback_names.items():
			if id in self.entities and not self.entities[id]["names"]:
				self.entities[id]["names"] = list(names)

	def prune(self):
		"""Drops entities without a name, and links and credits to what is not in the data."""
		self.entities = {id: entity for id, entity in self.entities.items() if entity["names"]}

		def keep(entity, group, kept):
			values = {name: [each for each in values if kept(each)] for name, values in entity.get(group, {}).items()}
			values = {name: values for name, values in values.items() if values}
			if values:
				entity[group] = values
			else:
				entity.pop(group, None)

		for entity in self.entities.values():
			keep(entity, "links", lambda target: target in self.entities)
			keep(entity, "dated", lambda value: "link" not in value or value["link"] in self.entities)
		self.credits = {
			(work, person, role, character if character in self.entities else None)
			for work, person, role, character in self.credits
			if work in self.entities and person in self.entities
		}

	def write(self, directory):
		"""Writes the entities and credits, sorted, and returns their hash."""
		directory.mkdir(parents=True, exist_ok=True)

		def order(id):
			number = re.fullmatch(r"Q([0-9]+)", id)
			return (0, int(number[1]), "") if number else (1, 0, id)

		entities = sorted(
			self.entities.values(),
			key=lambda entity: (KIND_ORDER.index(entity["kind"]), order(entity["id"])),
		)
		credits = sorted(
			self.credits,
			key=lambda credit: (order(credit[0]), order(credit[1]), credit[2], credit[3] or ""),
		)
		files = {
			"entities.jsonl": "".join(
				json.dumps(entity, ensure_ascii=False, sort_keys=True) + "\n" for entity in entities
			),
			"credits.jsonl": "".join(
				json.dumps(
					{
						key: value
						for key, value in zip(("work", "person", "role", "character"), credit)
						if value
					},
					ensure_ascii=False, sort_keys=True,
				) + "\n"
				for credit in credits
			),
		}
		for name, text in files.items():
			# Lines end with \n on every system, as the hash counts them, never \r\n.
			(directory / name).write_text(text, encoding="utf-8", newline="\n")
		return digest(directory)


def digest(directory):
	"""The hash of the data in a directory, as its manifest names it: of each file's name
	and the hash of its bytes, in order. scripts/bench-record.py checks data with it."""
	total = hashlib.sha256()
	for name in ("entities.jsonl", "credits.jsonl"):
		content = (directory / name).read_bytes()
		total.update(name.encode() + b"\0" + hashlib.sha256(content).digest())
	return total.hexdigest()


def add(entity, group, name, value):
	"""Adds a value to one of an entity's groups of values, once."""
	values = entity.setdefault(group, {}).setdefault(name, [])
	if value not in values:
		values.append(value)
		if not isinstance(value, dict):
			values.sort()


def add_dated(entity, name, value):
	"""Adds a dated value to an entity, once, in an order that its value fixes."""
	values = entity.setdefault("dated", {}).setdefault(name, [])
	if value not in values:
		values.append(value)
		values.sort(key=lambda each: json.dumps(each, sort_keys=True))


def targets(data, ids, names):
	"""The entities that the entities `ids` link to through the fields `names`, with dates
	or without."""
	found = set()
	for id in ids:
		entity = data.entities[id]
		for name in names:
			found.update(entity.get("links", {}).get(name, []))
			found.update(each["link"] for each in entity.get("dated", {}).get(name, []) if "link" in each)
	return found


# The household, from 2016 to 2025: two boys, each with his names, his birthday and how tall
# he is for his age, and a dog with its names and birthday.
HOUSEHOLD = (datetime.date(2016, 1, 1), datetime.date(2025, 12, 31))
CHILDREN = (
	("sim:leo", ("Leo Berg", "Лео Берг", "Лео Берг", "Leo Berg"), datetime.date(2013, 5, 14), 1.02),
	("sim:nils", ("Nils Berg", "Нильс Берг", "Нільс Берг", "Nils Berg"), datetime.date(2016, 8, 2), 0.99),
)
DOG = ("sim:bruno", ("Bruno", "Бруно", "Бруно", "Bruno"), datetime.date(2016, 3, 20))
# How tall boys are by age in months, in centimetres, close to the median of growth charts.
GROWTH = (
	(0, 49.9), (1, 54.7), (2, 58.4), (4, 63.9), (6, 67.6), (9, 72.0), (12, 75.7), (18, 82.3),
	(24, 87.1), (36, 96.1), (48, 103.3), (60, 110.0), (72, 116.0), (84, 121.7), (96, 127.3),
	(108, 132.6), (120, 137.8), (132, 143.1), (144, 149.1), (156, 156.2),
)
# The ages in months at which a paediatrician measures a child.
CHECKUPS = (1, 2, 4, 6, 9, 12, 18, 24, 36, 48, 60, 72, 96, 120, 144)


def jitter(*key):
	"""A number from -1 to 1 that the key fixes, for the household's readings."""
	digest = hashlib.sha256("/".join(map(str, key)).encode()).digest()
	return int.from_bytes(digest[:8], "big") / 2 ** 63 - 1


def months_after(day, months):
	month = day.month - 1 + months
	return datetime.date(day.year + month // 12, month % 12 + 1, min(day.day, 28))


def months_between(born, day):
	return (day - born).days / 30.4375


def height(born, factor, day):
	"""How tall a boy is on a day, in centimetres."""
	age = months_between(born, day)
	for (first, low), (last, high) in zip(GROWTH, GROWTH[1:]):
		if age <= last:
			return factor * (low + (high - low) * (max(age, first) - first) / (last - first))
	return factor * GROWTH[-1][1]


def summer_time(day):
	"""Whether a day is in central Europe's summer time, from the last Sunday of March to the
	last Sunday of October."""
	def last_sunday(month):
		last = datetime.date(day.year, month, 31)
		return last - datetime.timedelta(days=(last.weekday() + 1) % 7)

	return last_sunday(3) <= day < last_sunday(10)


def names_of(texts):
	return [{"text": text, "lang": language} for text, language in zip(texts, LANGUAGES)]


def child(id, texts, born, factor, sibling):
	"""A child measured from 2016 to 2025: by a paediatrician at check-ups, to the millimetre;
	by the parents against a door frame every second month from his first birthday, to half
	a centimetre; and by a grandparent in inches each summer and winter from his second, to a
	quarter. A day has one reading at most, the paediatrician's first."""
	first, last = HOUSEHOLD
	readings = {}

	def measure(day, text):
		if first <= day <= last and day not in readings:
			readings[day] = {"quantity": text, "valid": day.isoformat()}

	for months in CHECKUPS:
		day = months_after(born, months)
		measure(day, f"{height(born, factor, day) * (1 + 0.002 * jitter(id, day)):.1f} cm")
	for year in range(first.year, last.year + 1):
		for month, nominal in ((7, 12), (12, 27)):
			day = datetime.date(year, month, nominal + round(2 * jitter(id, "visit", year, month)))
			if months_between(born, day) >= 24:
				inches = height(born, factor, day) / 2.54 + 0.3 * jitter(id, "inches", day)
				measure(day, f"{round(inches * 4) / 4:g} [in_i]")
		for month in range(2, 13, 2):
			day = datetime.date(year, month, 3 + round(8.5 * (jitter(id, "home", year, month) + 1)))
			if months_between(born, day) >= 12:
				centimetres = height(born, factor, day) + 0.6 * jitter(id, "frame", day)
				measure(day, f"{round(centimetres * 2) / 2:.1f} cm")
	return {
		"id": id, "kind": "person", "names": names_of(texts), "dates": {"born": [born.isoformat()]},
		"links": {"sibling": [sibling]},
		"dated": {"height": [readings[day] for day in sorted(readings)]},
	}


def dog(id, texts, born):
	"""A dog of a middle size, brought home at eight weeks: weighed most Sunday mornings by an
	app, which records the moment, to 100 g, but not in late July, when the family is away;
	and at the vet, to 10 g, for its vaccinations and each spring and autumn, on weekdays."""
	first, last = HOUSEHOLD

	def weight(day):
		age = (day - born).days
		grown = 23 / (1 + math.exp(-(age - 120) / 45))
		winter = 0.6 * math.cos(2 * math.pi * (day.timetuple().tm_yday - 15) / 365.25)
		older = 0.15 * max(0.0, age / 365.25 - 5)
		return grown + (winter + older) * min(1.0, age / 365)

	readings = []
	day = born + datetime.timedelta(weeks=8)
	while day <= last:
		if jitter(id, "skip", day) > -0.8 and not (day.month == 7 and day.day > 15):
			minutes = 7 * 60 + round(45 * (jitter(id, "time", day) + 1))
			offset = 2 if summer_time(day) else 1
			kilograms = weight(day) + 0.3 * jitter(id, "scale", day)
			readings.append({
				"quantity": f"{kilograms:.1f} kg",
				"valid": f"{day.isoformat()}T{minutes // 60:02}:{minutes % 60:02}:00+0{offset}",
			})
		day += datetime.timedelta(weeks=1)
	visits = [born + datetime.timedelta(weeks=weeks, days=2) for weeks in (8, 12, 16)]
	for year in range(first.year + 1, last.year + 1):
		for month in (3, 10):
			visits.append(datetime.date(year, month, 10 + round(5 * (jitter(id, "vet", year, month) + 1))))
	for visit in visits:
		while visit.weekday() >= 5:
			visit += datetime.timedelta(days=1)
		kilograms = weight(visit) + 0.05 * jitter(id, "vet scale", visit)
		readings.append({"quantity": f"{kilograms:.2f} kg", "valid": visit.isoformat()})
	readings.sort(key=lambda reading: reading["valid"])
	return {
		"id": id, "kind": "animal", "names": names_of(texts), "dates": {"born": [born.isoformat()]},
		"dated": {"weight": readings},
	}


def household():
	"""The household of the library's user: the two children and the dog."""
	(leo, *leo_rest), (nils, *nils_rest) = CHILDREN
	return [child(leo, *leo_rest, nils), child(nils, *nils_rest, leo), dog(*DOG)]


def add_audiobooks(data, releases, authors, books):
	"""Adds the audiobooks among MusicBrainz releases, one for each release group, with
	their authors and narrators, and the books they read, by title. `authors` maps the
	MusicBrainz IDs of authors to their items, `books` normalized titles to their items.
	Returns the MusicBrainz IDs of the narrators."""
	groups = collections.defaultdict(list)
	for release in releases:
		groups[release["release-group"]["id"]].append(release)
	narrators = set()
	for group, members in groups.items():
		id = "mb:" + group
		audiobook = data.entity(id, "audiobook")
		languages = collections.Counter(
			language_tag(release.get("text-representation", {}).get("language")) for release in members
		)
		language = max(sorted(languages, key=str), key=lambda each: (each is not None, languages[each]))
		title = members[0]["release-group"]["title"]
		audiobook["names"] = [{"text": title, "lang": language} if language else {"text": title}]
		dates = [release["date"] for release in members if re.fullmatch(r"[0-9]{4}(-[0-9]{2}){0,2}", release.get("date", ""))]
		if dates:
			audiobook["dates"] = {"published": [min(dates)]}
		titles = {normalize_title(release["title"]) for release in members} | {normalize_title(title)}
		read = sorted({books[each] for each in titles if each in books})
		if read:
			audiobook["links"] = {"reads": read}
		for release in members:
			for credit in release.get("artist-credit", []):
				if not isinstance(credit, dict):
					continue
				artist = credit["artist"]
				if artist["id"] in authors:
					data.credits.add((id, authors[artist["id"]], "author", None))
				else:
					narrator = data.entity("mb:" + artist["id"], "person")
					if not narrator["names"]:
						narrator["names"].append({"text": artist["name"]})
					data.credits.add((id, narrator["id"], "narrator", None))
					narrators.add(artist["id"])
	return narrators


def pen_names(artist):
	"""The pen names of a MusicBrainz artist with its relations: the names it performs as,
	and the artists named after it, by their IDs, with their names."""
	return {
		relation["artist"]["id"]: relation["artist"]["name"]
		for relation in artist.get("relations", [])
		if (relation.get("type"), relation.get("direction")) in PEN_NAMES and "artist" in relation
	}


class TooSlow(Exception):
	"""A query that the service gave up on."""


class Client:
	"""Requests to Wikidata and MusicBrainz, one at a time, with their answers kept."""

	def __init__(self, cache):
		self.cache = cache
		self.requests = 0
		# When the answers used were fetched, as the times of their files.
		self.fetched = []
		self.last_musicbrainz = 0.0
		contact = os.environ.get("NIBKEEP_CONTACT")
		self.agent = "nibkeep-fetch-real-data/1 (benchmark data" + (f"; {contact})" if contact else ")")

	def fetch(self, url, body=None):
		key = hashlib.sha256(url.encode() + b"\0" + (body or b"")).hexdigest()
		cached = self.cache / key[:2] / f"{key}.json"
		answer = read_cached(cached)
		if answer is not None:
			self.fetched.append(cached.stat().st_mtime)
			return answer
		for attempt in range(8):
			if url.startswith(MUSICBRAINZ):
				time.sleep(max(0.0, self.last_musicbrainz + 1.1 - time.monotonic()))
				self.last_musicbrainz = time.monotonic()
			request = urllib.request.Request(url, data=body, headers={
				"User-Agent": self.agent,
				"Accept": "application/sparql-results+json" if body else "application/json",
			})
			self.requests += 1
			try:
				with urllib.request.urlopen(request, timeout=90) as response:
					answer = json.loads(response.read())
				break
			except json.JSONDecodeError as error:
				# The query service ends an answer it gave up on midway with an error.
				if body:
					raise TooSlow() from error
				time.sleep(5 * 2 ** attempt)
			except urllib.error.HTTPError as error:
				text = error.read().decode(errors="replace")
				if "TimeoutException" in text:
					raise TooSlow() from error
				if error.code == 404 and not body:
					# Such as an artist that MusicBrainz merged away, which Wikidata still names.
					print(f"{url} is not there, left out", file=sys.stderr)
					return None
				if error.code not in (429, 500, 502, 503, 504):
					sys.exit(f"{url} answered {error.code}: {text[:500]}")
				wait = error.headers.get("Retry-After")
				time.sleep(int(wait) if wait and wait.isdigit() else 5 * 2 ** attempt)
			except TimeoutError as error:
				if body:
					raise TooSlow() from error
				time.sleep(5 * 2 ** attempt)
			except (urllib.error.URLError, http.client.HTTPException, ConnectionError) as error:
				print(f"retrying after {getattr(error, 'reason', error)!r}", file=sys.stderr)
				time.sleep(5 * 2 ** attempt)
		else:
			sys.exit(f"{url} kept failing")
		write_cached(cached, answer)
		self.fetched.append(cached.stat().st_mtime)
		return answer

	def sparql(self, name, **values):
		query = QUERIES[name] % values
		body = urllib.parse.urlencode({"query": query}).encode()
		return self.fetch(WIKIDATA, body)["results"]["bindings"]

	def sparql_items(self, name, items, size, **values):
		"""Runs a query for items, `size` at a time, halving a batch the service gives up on."""
		items = sorted(items)
		rows = []
		for start in range(0, len(items), size):
			rows += self.sparql_batch(name, items[start:start + size], **values)
		return rows

	def sparql_batch(self, name, items, **values):
		try:
			return self.sparql(name, items=" ".join(f"wd:{item}" for item in items), **values)
		except TooSlow:
			if len(items) == 1:
				print(f"{name} of {items[0]} took too long, left out", file=sys.stderr)
				return []
			half = len(items) // 2
			return self.sparql_batch(name, items[:half], **values) + self.sparql_batch(name, items[half:], **values)

	def musicbrainz(self, path):
		return self.fetch(MUSICBRAINZ + path)


def moment(seconds):
	return datetime.datetime.fromtimestamp(seconds, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def wd(items):
	return " ".join(f"wd:{item}" for item in items) or "wd:Q0"


def properties(table):
	return " ".join(f"wd:{prop}" for prop in table)


def say(text):
	print(text, file=sys.stderr, flush=True)


def fetch_statements(client, data, ids, table):
	data.add_statements(client.sparql_items("statements", ids, 100, properties=properties(table)))


def fetch_dated(client, data, ids, table):
	data.add_statements(client.sparql_items("dated statements", ids, 100, properties=properties(table)))


def credited(data, works):
	works = set(works)
	return {person for work, person, _, _ in data.credits if work in works}


def sort_credited(client, data, people):
	"""Makes entities of the people and groups credited or linked to: humans are people, and
	anything else, such as a studio, a band or a pen name, a group."""
	new = sorted(person for person in people if person not in data.entities)
	humans = {qid(row["item"]) for row in client.sparql_items("humans", new, 200, human=HUMAN)}
	for person in new:
		data.entity(person, "person" if person in humans else "group")


def main():
	parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
	parser.add_argument("--depth", type=int, default=0, help="steps out from the franchises")
	parser.add_argument("--max-works", type=int, help="stop the steps at this many works")
	parser.add_argument("--output", type=pathlib.Path, help="the directory of the data")
	arguments = parser.parse_args()
	output = arguments.output or DATA / f"depth-{arguments.depth}"
	client = Client(DATA / "cache")
	data = Data()

	franchises = wd(seed["franchise"] for seed in SEEDS.values())
	universes = wd(universe for seed in SEEDS.values() for universe in seed["universes"])
	series = wd(each for seed in SEEDS.values() for each in seed["series"])
	kinds = " ".join(f'("{kind}" wd:{root})' for kind, roots in WORK_KINDS for root in roots)
	found = collections.defaultdict(set)
	for row in client.sparql("seed works", franchises=franchises, universes=universes, series=series, kinds=kinds):
		found[qid(row["work"])].add(text_of(row["kind"]))
	for work, its_kinds in found.items():
		data.entity(work, choose_kind(its_kinds))
	seed_works = sorted(found)
	for row in client.sparql(
		"seed characters", franchises=franchises, universes=universes,
		works=wd(seed_works), character=FICTIONAL_CHARACTER,
	):
		data.entity(qid(row["character"]), "character")
	for group in (*COMPANIES.values(), *BANDS.values()):
		data.entity(group, "group")
	say(f"the franchises: {len(seed_works)} works, {len(data.of_kind('character'))} characters")

	works, known_people = seed_works, set()
	for depth in range(arguments.depth + 1):
		fetch_statements(client, data, works, WORK_PROPERTIES)
		people = credited(data, works) - known_people
		sort_credited(client, data, people)
		known_people |= people
		say(f"depth {depth}: {len(works)} works, {len(people)} more people and groups")
		if depth == arguments.depth:
			break
		humans = people & set(data.of_kind("person"))
		rows = client.sparql_items("films of people", humans, 100, films=wd(FILMS))
		works = sorted({qid(row["work"]) for row in rows} - set(data.entities))
		if arguments.max_works is not None:
			room = arguments.max_works - len(data.of_kind(*(kind for kind, _ in WORK_KINDS)))
			works = sorted(works, key=lambda work: int(work[1:]))[:max(room, 0)]
		for work in works:
			data.entity(work, "film")

	# The games of the companies, with the people credited in them, who lead no further.
	rows = client.sparql("games of companies", companies=wd(COMPANIES.values()), game=VIDEO_GAME)
	games = sorted({qid(row["work"]) for row in rows} - set(data.entities))
	for game in games:
		data.entity(game, "game")
	fetch_statements(client, data, games, WORK_PROPERTIES)
	sort_credited(client, data, credited(data, games))
	say(f"the companies' games: {len(games)}")

	for _, _, _, character in list(data.credits):
		if character and character not in data.entities:
			data.entity(character, "character")

	# The audiobooks of the franchises' authors, and who narrated them.
	authors = {person for work, person, role, _ in data.credits if role == "author" and work in found}
	artists = {
		text_of(row["artist"]): qid(row["item"])
		for row in client.sparql_items("musicbrainz artists", authors, 200)
	}
	# Audiobooks credit authors under their pen names too, as Robert Galbraith: each with
	# its name and its writer.
	others = {}
	for artist in sorted(artists):
		answer = client.musicbrainz(QUERIES["pen names"].format(artist=artist)) or {}
		for each, name in pen_names(answer).items():
			others.setdefault(each, (name, artists[artist]))
	searched = sorted(artists)
	for row in client.sparql(
		"items of musicbrainz artists", artists=" ".join(f'"{each}"' for each in sorted(others)) or '""',
	):
		if qid(row["item"]):
			artists.setdefault(text_of(row["artist"]), qid(row["item"]))
	sort_credited(client, data, {artists[each] for each in others if each in artists})
	for each, (name, writer) in others.items():
		if each not in artists:
			data.entity("mb:" + each, "group")["names"].append({"text": name})
			artists[each] = "mb:" + each
		if artists[each] != writer:
			add(data.entities[artists[each]], "links", "members", writer)
	books = {}
	book_ids = [work for work in seed_works if data.entities[work]["kind"] == "book"]
	for row in client.sparql_items("book titles", book_ids, 200):
		books.setdefault(normalize_title(text_of(row["name"])), qid(row["item"]))
	releases = []
	for artist in searched:
		offset = 0
		while True:
			page = client.musicbrainz(QUERIES["audiobooks"].format(
				artist=artist, offset=offset).replace(" ", "%20"))
			if page is None:
				break
			releases += page["releases"]
			offset += len(page["releases"])
			if not page["releases"] or offset >= page["count"]:
				break
	narrators = add_audiobooks(data, releases, artists, books)
	known = client.sparql(
		"items of musicbrainz artists", artists=" ".join(f'"{each}"' for each in sorted(narrators)) or '""',
	)
	renamed = set()
	for row in known:
		item = qid(row["item"])
		if item and "mb:" + text_of(row["artist"]) in data.entities:
			data.rename("mb:" + text_of(row["artist"]), item)
			renamed.add(item)
	sort_credited(client, data, renamed)
	say(f"audiobooks: {len(data.of_kind('audiobook'))}, from {len(releases)} releases")

	# Who broadcast the works and who distributes them, and when.
	works = [id for id in data.of_kind(*(kind for kind, _ in WORK_KINDS)) if id.startswith("Q")]
	fetch_dated(client, data, works, WORK_DATED)
	sort_credited(client, data, targets(data, works, set(WORK_DATED.values())))

	# What is known of the characters, groups and people, the writers behind pen names, and
	# what changed for groups and people over time. Of the people found then, spouses and
	# partners join the data.
	fetch_statements(client, data, data.of_kind("character"), CHARACTER_PROPERTIES)
	groups = [id for id in data.of_kind("group") if id.startswith("Q")]
	fetch_statements(client, data, groups, GROUP_PROPERTIES)
	fetch_dated(client, data, groups, GROUP_DATED)
	leaders = {"members", "founded_by", "chief_executive", "chairperson"}
	sort_credited(client, data, targets(data, data.of_kind("group"), leaders))
	people = [id for id in data.of_kind("person") if id.startswith("Q")]
	fetch_dated(client, data, people, PERSON_DATED)
	sort_credited(client, data, targets(data, people, {"spouse", "partner"}))
	fetch_statements(client, data, [id for id in data.of_kind("person") if id.startswith("Q")], PERSON_PROPERTIES)
	for term in targets(data, list(data.entities), TERMS):
		data.entity(term, "term")
	dated = sum(len(values) for entity in data.entities.values() for values in entity.get("dated", {}).values())
	say(f"over time: {dated} dated values")

	items = [id for id in data.entities if id.startswith("Q")]
	languages = ", ".join(f'"{language}"' for language in (*LANGUAGES, EVERY_LANGUAGE))
	data.add_names(client.sparql_items("names", items, 200, languages=languages))
	data.name_unnamed()
	unnamed = [id for id in items if not data.entities[id]["names"]]
	data.add_names(client.sparql_items("any name", unnamed, 200))
	say(f"names: {len(unnamed)} entities had none in the languages")

	for entity in household():
		data.entities[entity["id"]] = entity
	data.prune()
	digest = data.write(output)
	counts = collections.Counter(entity["kind"] for entity in data.entities.values())
	manifest = {
		"data": digest[:12],
		"sha256": digest,
		"fetched": {"first": moment(min(client.fetched)), "last": moment(max(client.fetched))},
		"seeds": SEEDS,
		"companies": COMPANIES,
		"bands": BANDS,
		"household": [HOUSEHOLD[0].isoformat(), HOUSEHOLD[1].isoformat()],
		"depth": arguments.depth,
		"max_works": arguments.max_works,
		"languages": [*LANGUAGES, EVERY_LANGUAGE],
		"sources": {
			"Wikidata": f"{WIKIDATA} (CC0)",
			"MusicBrainz": f"{MUSICBRAINZ} (core data, CC0)",
		},
		"queries": QUERIES,
		"requests": client.requests,
		"counts": {**dict(sorted(counts.items())), "credits": len(data.credits)},
		"entities": len(data.entities) + len(data.credits),
	}
	(output / "manifest.json").write_text(
		json.dumps(manifest, indent="\t", ensure_ascii=False) + "\n", encoding="utf-8", newline="\n",
	)
	say(f"wrote {output}: {manifest['entities']} entities, counting credits, data {manifest['data']}")


if __name__ == "__main__":
	main()
