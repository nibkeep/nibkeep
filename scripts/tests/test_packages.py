"""Tests of the packages that are published: each carries the licenses, as the workspace
states them, and what crates.io asks of a package.

    python3 -m unittest discover -s scripts/tests
"""

import pathlib
import re
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
# The licenses' texts, which a package must carry itself: Cargo packs only what is in a
# package's own folder.
LICENSES = ("LICENSE-MIT", "LICENSE-APACHE")
# Tools of this repository's own tests and benchmarks, which are not published, and the app
# kit's crate and the plugin host, while their APIs still change freely.
UNPUBLISHED = {"nibkeep-testkit", "nibkeep-generate", "nibkeep-end-to-end", "nibkeep-app", "nibkeep-plugins"}
# The target of a Markdown link, inline or by reference.
LINK = re.compile(r"\]\(\s*<?([^)\s>]+)|^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?$", re.M)


def manifests():
	"""The manifest of each member of the workspace, by its folder."""
	found = {}
	for manifest in [*sorted((ROOT / "crates").glob("*/Cargo.toml")), ROOT / "apps" / "nib" / "Cargo.toml"]:
		with open(manifest, "rb") as file:
			found[manifest.parent] = tomllib.load(file)
	return found


def workspace():
	with open(ROOT / "Cargo.toml", "rb") as file:
		return tomllib.load(file)["workspace"]


def published(manifest):
	"""Whether a package is published: `publish` is false only where it is not."""
	publish = manifest["package"].get("publish", True)
	if isinstance(publish, dict):
		publish = workspace()["package"].get("publish", True)
	return publish is not False


class Packages(unittest.TestCase):
	def test_only_the_tools_of_tests_and_benchmarks_and_the_app_kit_stay_unpublished(self):
		unpublished = {
			manifest["package"]["name"] for manifest in manifests().values() if not published(manifest)
		}
		self.assertEqual(unpublished, UNPUBLISHED)

	def test_every_published_package_carries_the_licenses_as_the_workspace_has_them(self):
		for folder, manifest in manifests().items():
			if not published(manifest):
				continue
			for name in LICENSES:
				with self.subTest(package=manifest["package"]["name"], license=name):
					copy = folder / name
					self.assertTrue(copy.is_file(), f"{copy.relative_to(ROOT)} is missing")
					self.assertEqual(copy.read_bytes(), (ROOT / name).read_bytes())

	def test_every_published_package_states_what_crates_io_shows(self):
		for manifest in manifests().values():
			if not published(manifest):
				continue
			package = manifest["package"]
			for field in ("description", "license", "repository", "readme", "keywords"):
				with self.subTest(package=package["name"], field=field):
					self.assertIn(field, package)

	def test_a_published_package_depends_on_the_others_by_version(self):
		# Cargo drops a dependency for tests that has only a path as it packs, and refuses
		# any other without a version.
		internal = workspace()["dependencies"]
		for name in UNPUBLISHED & internal.keys():
			self.assertNotIn("version", internal[name], name)
		for name, dependency in internal.items():
			if isinstance(dependency, dict) and "path" in dependency and name not in UNPUBLISHED:
				with self.subTest(dependency=name):
					self.assertEqual(dependency.get("version"), workspace()["package"]["version"])
		for manifest in manifests().values():
			if not published(manifest):
				continue
			used = set(manifest.get("dependencies", {}))
			for table in manifest.get("target", {}).values():
				used |= set(table.get("dependencies", {}))
			self.assertFalse(used & UNPUBLISHED, manifest["package"]["name"])

	def test_the_readme_of_every_published_package_links_by_full_addresses(self):
		# crates.io takes a relative link in a package's readme as relative to the package's
		# own folder in the repository, where the documents it names are not.
		for folder, manifest in manifests().items():
			if not published(manifest):
				continue
			readme = manifest["package"]["readme"]
			path = ROOT / workspace()["package"]["readme"] if isinstance(readme, dict) else folder / readme
			text = re.sub(r"^```.*?^```", "", path.read_text(encoding="utf-8"), flags=re.M | re.S)
			text = re.sub(r"`[^`\n]*`", "", text)
			for inline, reference in LINK.findall(text):
				with self.subTest(package=manifest["package"]["name"], link=inline or reference):
					self.assertRegex(inline or reference, r"^(https?://|#)")


if __name__ == "__main__":
	unittest.main()
