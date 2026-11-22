"""Checks the links between the project's documents.

Every relative link in the Markdown files must reach a file, and every anchor a heading
or an `<a id>` in it, as GitHub renders them. If the book is built, in target/book, every
relative link in its pages must reach a file, and every anchor an element of that page.
A book older than the documents is refused: build it again with `mdbook build`.

Run it from anywhere: python3 scripts/check-links.py
"""

import functools
import html.parser
import pathlib
import re
import sys
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOOK = ROOT / "target" / "book"
# Folders that are not this repository's: builds, and the benchmarks' data and results.
SKIPPED = {"target", ".git", "node_modules", "real-data", "bench-results"}

FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
INLINE_LINK = re.compile(r"!?\[([^\]]*)\]\((?:<[^>]*>|[^)\s]*)(?:\s+\"[^\"]*\")?\)")
LINK_TARGET = re.compile(r"\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+\"[^\"]*\")?\s*\)")
REFERENCE = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)", re.M)
EXTERNAL = re.compile(r"^[a-z][a-z0-9+.-]*:", re.I)


def slug(heading):
	"""The anchor GitHub gives a heading: made from its text as rendered, lowercase, with
	spaces as hyphens and all punctuation but hyphens and underscores removed."""
	text = re.sub(r"\s+#+\s*$", "", heading.strip())
	text = INLINE_LINK.sub(r"\1", text)
	text = re.sub(r"<[^>]+>", "", text)
	text = text.replace("`", "")
	# Emphasis: markers at the edges of words, not underscores inside them.
	text = re.sub(r"(?<!\w)[*_]+|[*_]+(?!\w)", "", text)
	text = re.sub(r"[^\w\- ]", "", text.lower())
	return text.replace(" ", "-")


def outside_code(text):
	"""The lines of Markdown that are not in fenced code blocks."""
	fence = None
	for line in text.splitlines():
		found = FENCE.match(line)
		if fence is None and found:
			fence = found.group(1)
			continue
		if fence is not None:
			if found and found.group(1)[0] == fence[0] and len(found.group(1)) >= len(fence):
				fence = None
			continue
		yield line


@functools.cache
def markdown_anchors(path):
	headings, ids = set(), set()
	for line in outside_code(path.read_text(encoding="utf-8")):
		heading = re.match(r"^\s{0,3}(#{1,6})\s+(.*)$", line)
		if heading:
			base = slug(heading.group(2))
			anchor, number = base, 1
			while anchor in headings:
				anchor = f"{base}-{number}"
				number += 1
			headings.add(anchor)
		ids.update(re.findall(r'<a id="([^"]+)"></a>', line))
	return headings | ids


def targets(text):
	"""The targets of the links in Markdown, outside code."""
	lines = "\n".join(re.sub(r"`[^`\n]*`", "", line) for line in outside_code(text))
	for found in LINK_TARGET.findall(lines) + REFERENCE.findall(lines):
		yield found[1:-1] if found.startswith("<") else found


def resolve(base, root, name):
	"""Where a link leads: from the root it is relative to if it starts with a slash."""
	name = urllib.parse.unquote(name)
	return (root / name.lstrip("/") if name.startswith("/") else base / name).resolve()


def check_markdown():
	files = [
		path
		for path in ROOT.rglob("*.md")
		if not SKIPPED.intersection(path.relative_to(ROOT).parts)
	]
	problems, checked = [], 0
	for path in files:
		for target in targets(path.read_text(encoding="utf-8")):
			if EXTERNAL.match(target):
				continue
			checked += 1
			name, _, anchor = target.partition("#")
			destination = resolve(path.parent, ROOT, name) if name else path
			if not destination.exists():
				problems.append(f"{path.relative_to(ROOT)}: no file {target}")
			elif anchor and destination.suffix == ".md":
				if urllib.parse.unquote(anchor) not in markdown_anchors(destination):
					problems.append(f"{path.relative_to(ROOT)}: no anchor {target}")
	return checked, len(files), problems


class Page(html.parser.HTMLParser):
	"""The links of an HTML page, and the IDs of its elements."""

	def __init__(self):
		super().__init__()
		self.links, self.ids = [], set()

	def handle_starttag(self, tag, attributes):
		attributes = dict(attributes)
		if attributes.get("id"):
			self.ids.add(attributes["id"])
		if tag == "a" and attributes.get("href"):
			self.links.append(attributes["href"])


@functools.cache
def page(path):
	parsed = Page()
	parsed.feed(path.read_text(encoding="utf-8"))
	return parsed


def check_book():
	paths = sorted(BOOK.rglob("*.html"))
	problems, checked = [], 0
	for path in paths:
		# The page shown for a missing address uses links relative to the site's root.
		if path.name == "404.html":
			continue
		for link in page(path).links:
			if EXTERNAL.match(link):
				continue
			checked += 1
			name, _, anchor = link.partition("#")
			destination = resolve(path.parent, BOOK, name) if name else path
			if destination.is_dir():
				destination = destination / "index.html"
			if not destination.exists():
				problems.append(f"book {path.relative_to(BOOK)}: no page {link}")
			elif anchor and destination.suffix == ".html":
				if urllib.parse.unquote(anchor) not in page(destination).ids:
					problems.append(f"book {path.relative_to(BOOK)}: no anchor {link}")
	return checked, len(paths), problems


def book_is_current():
	"""Whether the book was built after every document and its configuration changed."""
	built = (BOOK / "index.html").stat().st_mtime
	sources = [ROOT / "book.toml", *(ROOT / "docs").rglob("*.md")]
	return all(source.stat().st_mtime <= built for source in sources)


def main():
	checked, files, problems = check_markdown()
	print(f"checked {checked} links in {files} Markdown files")
	if not (BOOK / "index.html").exists():
		print("the book is not built; run mdbook build to check it too")
	elif not book_is_current():
		problems.append("the book is older than the documents; run mdbook build")
	else:
		book_checked, pages, book_problems = check_book()
		print(f"checked {book_checked} links in {pages} pages of the book")
		problems += book_problems
	for problem in problems:
		print(problem)
	if problems:
		print(f"{len(problems)} problems")
		sys.exit(1)


if __name__ == "__main__":
	main()
