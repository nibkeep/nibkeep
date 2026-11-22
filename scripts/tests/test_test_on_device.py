"""Tests of scripts/test-on-device.py: what the iPhone's runner writes into a test's app, and
how it judges how the app ended. Runs anywhere, without a device.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import pathlib
import re
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
spec = importlib.util.spec_from_file_location("test_on_device", ROOT / "scripts" / "test-on-device.py")
test_on_device = importlib.util.module_from_spec(spec)
spec.loader.exec_module(test_on_device)
Ios = test_on_device.Ios


class AppInfo(unittest.TestCase):
	def test_it_names_the_bundle_and_the_executable(self):
		info = Ios.info("org.example.Tests", "store-0123456789abcdef")
		self.assertEqual(info["CFBundleIdentifier"], "org.example.Tests")
		self.assertEqual(info["CFBundleExecutable"], "store-0123456789abcdef")
		self.assertEqual(info["MinimumOSVersion"], Ios.DEPLOYMENT_TARGET)

	def test_it_declares_the_service_that_replicas_announce(self):
		# iOS lets an app browse only the Bonjour services that it declares.
		discovery = (ROOT / "crates" / "nibkeep-net" / "src" / "discovery.rs").read_text(encoding="utf-8")
		kind = re.search(r'const KIND: &str = "([^"]+)";', discovery).group(1)
		info = Ios.info("org.example.Tests", "store-0123456789abcdef")
		self.assertEqual(info["NSBonjourServices"], [kind])
		self.assertTrue(info["NSLocalNetworkUsageDescription"])


class Verdict(unittest.TestCase):
	def test_tests_that_pass_pass(self):
		self.assertEqual(Ios.verdict(0, 0, [True, True], []), (0, None))

	def test_a_failed_test_fails_whatever_devicectl_returns(self):
		# devicectl returns what the app returned since Xcode 27.2, and 0 before it.
		for launched in (0, 1):
			self.assertEqual(Ios.verdict(launched, 101, [True, False], []), (101, None))
		self.assertEqual(Ios.verdict(0, 0, [True, False], []), (101, None))

	def test_an_app_that_never_ended_with_a_code_was_not_launched_or_crashed(self):
		code, said = Ios.verdict(1, None, [], [])
		self.assertEqual(code, 1)
		self.assertIn("could not launch", said)
		code, said = Ios.verdict(0, None, [], [])
		self.assertEqual(code, 1)
		self.assertIn("crashed, or was ended", said)

	def test_tests_that_end_without_a_summary_crashed(self):
		code, said = Ios.verdict(0, 0, [], [])
		self.assertEqual(code, 1)
		self.assertIn("without a summary", said)

	def test_a_listing_has_no_summary(self):
		self.assertEqual(Ios.verdict(0, 0, [], ["--list"]), (0, None))
		self.assertEqual(Ios.verdict(1, 2, [], ["--help"]), (2, None))


if __name__ == "__main__":
	unittest.main()
