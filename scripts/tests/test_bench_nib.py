"""Tests of scripts/bench-nib.py, without nib.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "bench-nib.py"
spec = importlib.util.spec_from_file_location("bench_nib", SCRIPT)
bench_nib = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench_nib)


class OwnSettings(unittest.TestCase):
	def test_a_persons_settings_of_nib_stay_out_of_the_benchmark(self):
		environment = {
			"NIBKEEP_LIBRARY": "/home/ada/Libraries/to-read",
			"NIBKEEP_PASSPHRASE_FILE": "/home/ada/.config/nibkeep/to-read-passphrase",
			"NIBKEEP_PORT": "53301",
			"PATH": "/usr/bin",
			"LANG": "en_US.UTF-8",
		}
		bench_nib.forget_own_settings(environment)
		self.assertEqual(environment, {"PATH": "/usr/bin", "LANG": "en_US.UTF-8"})


class Commands(unittest.TestCase):
	def test_real_data_measures_a_search_and_an_edit(self):
		self.assertEqual(list(bench_nib.real_commands(0)), ["find", "edit"])

	def test_no_two_runs_make_the_same_edit(self):
		self.assertNotEqual(bench_nib.real_commands(0)["edit"], bench_nib.real_commands(1)["edit"])
		self.assertNotEqual(
			bench_nib.commands(1667, 0)["edit"], bench_nib.commands(1667, 1)["edit"],
		)


class Totals(unittest.TestCase):
	def test_the_whole_time_its_start_and_the_time_without_unlocking(self):
		timings = {"unlock_seconds": 0.3, "open_seconds": 0.05, "work_seconds": 0.04, "waiting_seconds": 0.5}
		self.assertEqual(
			{key: round(value, 6) for key, value in bench_nib.totals(1.0, timings).items()},
			{
				"unlock_seconds": 0.3, "open_seconds": 0.05, "work_seconds": 0.04,
				"total_seconds": 0.5, "start_seconds": 0.11, "total_without_unlock_seconds": 0.2,
			},
		)


if __name__ == "__main__":
	unittest.main()
