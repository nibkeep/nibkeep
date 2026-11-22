"""Tests of scripts/bench-record.py, with Git repositories of their own and no devices.

    python3 -m unittest discover -s scripts/tests
"""

import contextlib
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

SCRIPTS = pathlib.Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("bench_record", SCRIPTS / "bench-record.py")
record = importlib.util.module_from_spec(spec)
spec.loader.exec_module(record)

# Commits in the tests' repositories, by a test identity rather than the person's.
IDENTITY = {
	"GIT_AUTHOR_NAME": "Test", "GIT_AUTHOR_EMAIL": "test@example.com",
	"GIT_COMMITTER_NAME": "Test", "GIT_COMMITTER_EMAIL": "test@example.com",
}


def run(*arguments, cwd):
	return subprocess.run(arguments, cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def quietly(call, *arguments):
	"""Calls with what it prints kept from the test's output."""
	with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
		return call(*arguments)


def write_data(directory, entities):
	"""Writes real data as scripts/fetch-real-data.py does, with its manifest."""
	directory.mkdir(parents=True)
	(directory / "entities.jsonl").write_bytes(entities)
	(directory / "credits.jsonl").write_bytes(b'{"person": "Q2", "role": "cast", "work": "Q1"}\n')
	digest = record.fetch.digest(directory)
	(directory / "manifest.json").write_text(json.dumps({"data": digest[:12], "sha256": digest}))
	return digest


class Repositories(unittest.TestCase):
	"""A temporary directory, and the identity that commits in it."""

	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.root = pathlib.Path(self.directory.name)
		patcher = mock.patch.dict(os.environ, IDENTITY)
		patcher.start()
		self.addCleanup(patcher.stop)

	def results(self):
		return quietly(record.open_results, self.root / "results")

	def commit_file(self, repository, name, text, message):
		(repository / name).parent.mkdir(parents=True, exist_ok=True)
		(repository / name).write_text(text, encoding="utf-8")
		run("git", "add", name, cwd=repository)
		run("git", "commit", "--quiet", "-m", message, cwd=repository)


class OpenResults(Repositories):
	def test_a_new_repository_starts_on_main_with_its_line_endings(self):
		results = self.results()
		self.assertEqual(run("git", "branch", "--show-current", cwd=results), "main")
		attributes = (results / ".gitattributes").read_text()
		self.assertIn("eol=lf", attributes)
		self.assertIn("data/** -text", attributes)
		self.assertEqual(run("git", "status", "--porcelain", cwd=results), "")

	def test_an_existing_repository_is_kept(self):
		results = self.results()
		self.commit_file(results, "results/linux-desktop.jsonl", "{}\n", "Results")
		self.assertEqual(record.open_results(results), results)
		self.assertTrue((results / "results" / "linux-desktop.jsonl").exists())

	def test_a_folder_that_is_no_repository_is_refused(self):
		(self.root / "results").mkdir()
		(self.root / "results" / "notes.txt").write_text("mine")
		with self.assertRaises(SystemExit):
			quietly(record.open_results, self.root / "results")

	def test_the_variable_names_the_folder_and_an_option_overrides_it(self):
		with mock.patch.dict(os.environ, {record.RESULTS_VARIABLE: "/elsewhere"}):
			self.assertEqual(record.results_directory(None), pathlib.Path("/elsewhere"))
			self.assertEqual(record.results_directory(pathlib.Path("/given")), pathlib.Path("/given"))
		with mock.patch.dict(os.environ, {}, clear=True):
			self.assertEqual(record.results_directory(None), record.ROOT / "bench-results")


class Branches(Repositories):
	def code(self):
		"""A code repository with two commits on main, as the tests' ROOT."""
		code = self.root / "code"
		code.mkdir()
		run("git", "init", "--quiet", "--initial-branch", "main", cwd=code)
		self.commit_file(code, "a.txt", "a", "First")
		self.commit_file(code, "b.txt", "b", "Second")
		patcher = mock.patch.object(record, "ROOT", code)
		patcher.start()
		self.addCleanup(patcher.stop)
		return code

	def test_the_branch_checked_out_names_the_results_branch(self):
		code = self.code()
		run("git", "switch", "--quiet", "--create", "faster-search", cwd=code)
		self.assertEqual(record.code_branch(None), "faster-search")

	def test_a_detached_checkout_that_main_contains_is_main(self):
		code = self.code()
		run("git", "switch", "--quiet", "--detach", "main~1", cwd=code)
		self.assertEqual(record.code_branch(None), "main")

	def test_a_detached_checkout_elsewhere_needs_its_branch_named(self):
		code = self.code()
		run("git", "switch", "--quiet", "--create", "elsewhere", cwd=code)
		self.commit_file(code, "c.txt", "c", "Third")
		run("git", "switch", "--quiet", "--detach", "elsewhere", cwd=code)
		with self.assertRaises(SystemExit):
			quietly(record.code_branch, None)
		self.assertEqual(record.code_branch("elsewhere"), "elsewhere")

	def test_a_new_results_branch_starts_from_main_with_its_results(self):
		results = self.results()
		self.commit_file(results, "results/linux-desktop.jsonl", '{"commit": "c1"}\n', "Results of c1")
		record.switch(results, "faster-search")
		self.assertEqual(run("git", "branch", "--show-current", cwd=results), "faster-search")
		self.assertIn("c1", (results / "results" / "linux-desktop.jsonl").read_text())

	def test_an_existing_results_branch_keeps_its_own_results(self):
		results = self.results()
		record.switch(results, "faster-search")
		self.commit_file(results, "results/linux-desktop.jsonl", '{"commit": "f1"}\n', "Results of f1")
		record.switch(results, "main")
		self.assertFalse((results / "results" / "linux-desktop.jsonl").exists())
		record.switch(results, "faster-search")
		self.assertIn("f1", (results / "results" / "linux-desktop.jsonl").read_text())

	def test_an_origin_that_cannot_be_reached_leaves_the_results_here(self):
		results = self.results()
		run("git", "remote", "add", "origin", str(self.root / "nowhere"), cwd=results)
		quietly(record.switch, results, "faster-search")
		self.assertEqual(run("git", "branch", "--show-current", cwd=results), "faster-search")
		quietly(record.record, results, "linux-desktop", [result("t1", commit="a")])
		self.assertEqual(run("git", "log", "-1", "--format=%s", cwd=results), "Results of a on linux-desktop")

	def test_changes_to_results_by_hand_stop_the_run(self):
		results = self.results()
		self.commit_file(results, "results/linux-desktop.jsonl", "{}\n", "Results")
		(results / "results" / "linux-desktop.jsonl").write_text("changed\n")
		with self.assertRaises(SystemExit):
			quietly(record.clean_results, results, ())

	def test_a_machines_description_may_wait_to_be_committed(self):
		results = self.results()
		(results / "machines").mkdir()
		(results / "machines" / "linux-desktop.toml").write_text('name = "Desktop"\n')
		record.clean_results(results, ("machines/linux-desktop.toml",))


class DataRevisions(Repositories):
	def test_revisions_are_numbered_in_the_order_they_are_added(self):
		results = self.results()
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		write_data(self.root / "two", b'{"id": "Q1"}\n{"id": "Q2"}\n')
		self.assertEqual(quietly(record.add_data, results, self.root / "one"), 1)
		self.assertEqual(quietly(record.add_data, results, self.root / "two"), 2)
		self.assertEqual(record.revisions(results), [1, 2])
		self.assertEqual(run("git", "status", "--porcelain", cwd=results), "")

	def test_the_same_data_is_not_added_twice(self):
		results = self.results()
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		quietly(record.add_data, results, self.root / "one")
		with self.assertRaises(SystemExit):
			quietly(record.add_data, results, self.root / "one")

	def test_data_that_does_not_match_its_manifest_is_refused(self):
		results = self.results()
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		(self.root / "one" / "entities.jsonl").write_bytes(b'{"id": "Q9"}\n')
		with self.assertRaises(SystemExit):
			quietly(record.add_data, results, self.root / "one")

	def test_a_revision_is_taken_out_whole_and_checked(self):
		results = self.results()
		entities = b'{"id": "Q1", "names": [{"text": "\xc3\x84"}]}\n'
		digest = write_data(self.root / "one", entities)
		quietly(record.add_data, results, self.root / "one")
		cache = self.root / "cache"
		taken = record.extract(results, 1, cache)
		self.assertEqual((taken / "entities.jsonl").read_bytes(), entities)
		self.assertEqual(record.fetch.digest(taken), digest)
		# Taken again from where it was taken out to.
		self.assertEqual(record.extract(results, 1, cache), taken)

	def test_a_revision_changed_where_it_was_taken_out_is_taken_out_again(self):
		results = self.results()
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		quietly(record.add_data, results, self.root / "one")
		cache = self.root / "cache"
		taken = record.extract(results, 1, cache)
		(taken / "entities.jsonl").write_bytes(b'{"id": "Q9"}\n')
		taken = record.extract(results, 1, cache)
		self.assertEqual((taken / "entities.jsonl").read_bytes(), b'{"id": "Q1"}\n')

	def test_a_revision_taken_out_in_part_is_taken_out_again(self):
		results = self.results()
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		quietly(record.add_data, results, self.root / "one")
		cache = self.root / "cache"
		taken = record.extract(results, 1, cache)
		# As if the run stopped before the manifest, which goes last.
		(taken / "manifest.json").unlink()
		taken = record.extract(results, 1, cache)
		self.assertEqual(json.loads((taken / "manifest.json").read_text())["data"], record.fetch.digest(taken)[:12])

	def test_a_revision_that_does_not_exist_is_refused(self):
		results = self.results()
		with self.assertRaises(SystemExit):
			quietly(record.extract, results, 3, self.root / "cache")

	def test_revisions_are_read_from_main_whatever_is_checked_out(self):
		results = self.results()
		record.switch(results, "faster-search")
		write_data(self.root / "one", b'{"id": "Q1"}\n')
		quietly(record.add_data, results, self.root / "one")
		record.switch(results, "faster-search")
		self.assertEqual(record.revisions(results), [1])
		self.assertTrue(record.extract(results, 1, self.root / "cache").exists())


HARDWARE = {
	"family": "windows", "model": "ROG Zephyrus G15 GA503QR",
	"processor": "AMD Ryzen 9 5900HS with Radeon Graphics", "cores": 16,
	"memory_bytes": 42_312_536_064, "disk": "HFM001TD3JX013N (NVMe)",
	"file_system": "NTFS", "system": "Windows 11, build 26300", "on_battery": False,
}
LAPTOP = "rog-zephyrus-g15-ga503qr-windows"


class Labels(unittest.TestCase):
	def test_a_model_written_twice_is_written_once(self):
		self.assertEqual(record.clean_model("ROG Zephyrus G15 GA503QR_GA503QR"), "ROG Zephyrus G15 GA503QR")
		self.assertEqual(record.clean_model("Raspberry Pi 3 Model B Rev 1.2\x00"), "Raspberry Pi 3 Model B Rev 1.2")

	def test_a_model_that_names_nothing_is_none(self):
		for junk in ("System Product Name", "To Be Filled By O.E.M.", "Default string", "  "):
			self.assertIsNone(record.clean_model(junk), junk)

	def test_a_pcs_model_from_its_firmware(self):
		self.assertEqual(
			record.model_from_dmi("ASUSTeK COMPUTER INC.", "ROG Zephyrus G15 GA503QR_GA503QR", "1.0"),
			"ROG Zephyrus G15 GA503QR",
		)
		self.assertEqual(
			record.model_from_dmi("LENOVO\n", "20XW0055GE\n", "ThinkPad X1 Carbon Gen 9\n"), "ThinkPad X1 Carbon Gen 9",
		)
		self.assertIsNone(record.model_from_dmi("ASUS", "System Product Name", "System Version"))

	def test_a_processor_without_its_trimmings(self):
		self.assertEqual(record.short_processor("AMD Ryzen 9 7950X 16-Core Processor"), "AMD Ryzen 9 7950X")
		self.assertEqual(record.short_processor("Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz"), "Intel Core i7-8700K")
		self.assertEqual(record.short_processor("AMD Ryzen 9 5900HS with Radeon Graphics"), "AMD Ryzen 9 5900HS")

	def test_memory_counts_by_the_gigabyte_rounded_up_as_it_was_sold(self):
		self.assertEqual(record.gigabytes(16_542_502_912), 16)
		self.assertEqual(record.gigabytes(7_800_000_000), 8)
		self.assertEqual(record.gigabytes(970_000_000), 1)

	def test_a_label_from_the_model_and_the_system(self):
		self.assertEqual(record.new_label(HARDWARE, set()), LAPTOP)

	def test_a_machine_without_a_model_is_named_by_its_processor(self):
		desktop = {"family": "linux", "processor": "AMD Ryzen 9 7950X 16-Core Processor"}
		self.assertEqual(record.new_label(desktop, set()), "amd-ryzen-9-7950x-linux")
		self.assertEqual(record.new_label({"family": "linux"}, set()), "machine-linux")

	def test_a_label_taken_gets_a_number(self):
		self.assertEqual(record.new_label(HARDWARE, {LAPTOP}), f"{LAPTOP}-2")
		self.assertEqual(record.new_label(HARDWARE, {LAPTOP, f"{LAPTOP}-2"}), f"{LAPTOP}-3")


class Machines(Repositories):
	def identify(self, results, hardware, given=None):
		return quietly(record.identify, results, hardware, given)

	def file(self, results, label):
		return results / "machines" / f"{label}.toml"

	def test_a_new_machine_gets_a_description_on_main_and_the_run_goes_on(self):
		results = self.results()
		quietly(record.switch, results, "faster-search")
		self.assertEqual(self.identify(results, HARDWARE), LAPTOP)
		self.assertEqual(run("git", "branch", "--show-current", cwd=results), "main")
		self.assertEqual(run("git", "status", "--porcelain", cwd=results), "")
		self.assertEqual(run("git", "log", "-1", "--format=%s", cwd=results), f"Add machine {LAPTOP}")
		described = record.tomllib.loads(self.file(results, LAPTOP).read_text(encoding="utf-8"))
		self.assertEqual(described["name"], "ROG Zephyrus G15 GA503QR")
		self.assertEqual(described["processor"], "AMD Ryzen 9 5900HS with Radeon Graphics, 16 threads")
		self.assertEqual(described["memory"], "40 GB")
		self.assertEqual(described["detected"], {
			"system": "windows", "model": "ROG Zephyrus G15 GA503QR",
			"processor": "AMD Ryzen 9 5900HS with Radeon Graphics", "threads": 16, "memory_gb": 40,
			"disk": "HFM001TD3JX013N (NVMe)",
		})

	def test_the_same_machine_is_found_again(self):
		results = self.results()
		self.identify(results, HARDWARE)
		commits = run("git", "rev-list", "--count", "HEAD", cwd=results)
		updated = dict(HARDWARE, system="Windows 11, build 26400", on_battery=True, memory_bytes=42_000_000_000)
		self.assertEqual(self.identify(results, updated), LAPTOP)
		self.assertEqual(run("git", "rev-list", "--count", "HEAD", cwd=results), commits)

	def test_a_new_disk_more_memory_or_another_system_is_another_machine(self):
		results = self.results()
		self.identify(results, HARDWARE)
		self.assertEqual(self.identify(results, dict(HARDWARE, disk="WD Black SN850X (NVMe)")), f"{LAPTOP}-2")
		self.assertEqual(self.identify(results, dict(HARDWARE, memory_bytes=68_000_000_000)), f"{LAPTOP}-3")
		linux = dict(HARDWARE, family="linux", system="Arch Linux, Linux 6.17")
		self.assertEqual(self.identify(results, linux), "rog-zephyrus-g15-ga503qr-linux")
		self.assertEqual(self.identify(results, HARDWARE), LAPTOP)

	def test_what_the_system_did_not_tell_counts_as_no_difference(self):
		results = self.results()
		self.identify(results, HARDWARE)
		without_disk = {key: value for key, value in HARDWARE.items() if key != "disk"}
		self.assertEqual(self.identify(results, without_disk), LAPTOP)

	def test_too_little_told_matches_nothing(self):
		results = self.results()
		self.identify(results, HARDWARE)
		self.assertNotEqual(self.identify(results, {"family": "windows", "cores": 16}), LAPTOP)

	def test_two_machines_that_fit_stop_the_run(self):
		results = self.results()
		self.identify(results, HARDWARE)
		self.identify(results, dict(HARDWARE, disk="WD Black SN850X (NVMe)"))
		without_disk = {key: value for key, value in HARDWARE.items() if key != "disk"}
		with self.assertRaises(SystemExit):
			self.identify(results, without_disk)

	def test_what_a_person_added_is_kept_and_committed(self):
		results = self.results()
		self.identify(results, HARDWARE)
		path = self.file(results, LAPTOP)
		path.write_text(
			path.read_text(encoding="utf-8").replace('memory = "40 GB"', 'memory = "40 GB DDR4-3200"'),
			encoding="utf-8",
		)
		self.assertEqual(self.identify(results, HARDWARE), LAPTOP)
		self.assertEqual(run("git", "log", "-1", "--format=%s", cwd=results), f"Describe {LAPTOP}")
		self.assertIn("DDR4-3200", path.read_text(encoding="utf-8"))

	def test_what_a_person_wrote_of_another_machine_is_committed_too(self):
		results = self.results()
		self.identify(results, dict(HARDWARE, family="linux"))
		other = self.file(results, "rog-zephyrus-g15-ga503qr-linux")
		other.write_text(other.read_text(encoding="utf-8").replace('memory = "40 GB"', 'memory = "40 GB DDR4"'), encoding="utf-8")
		self.identify(results, HARDWARE)
		self.assertEqual(run("git", "status", "--porcelain", cwd=results), "")
		self.assertEqual(
			run("git", "log", "-1", "--format=%s", cwd=results),
			f"Describe rog-zephyrus-g15-ga503qr-linux, {LAPTOP}",
		)

	def test_a_label_with_results_but_no_description_is_not_taken_again(self):
		results = self.results()
		self.commit_file(results, f"results/{LAPTOP}.jsonl", "{}\n", "Results")
		self.assertEqual(self.identify(results, HARDWARE), f"{LAPTOP}-2")

	def test_a_label_given_is_used_for_a_new_machine(self):
		results = self.results()
		self.assertEqual(self.identify(results, HARDWARE, "pi-twin-2"), "pi-twin-2")
		self.assertTrue(self.file(results, "pi-twin-2").exists())

	def test_a_label_given_for_other_hardware_stops_the_run(self):
		results = self.results()
		self.identify(results, HARDWARE)
		with self.assertRaises(SystemExit):
			self.identify(results, dict(HARDWARE, disk="WD Black SN850X (NVMe)"), LAPTOP)

	def test_a_description_written_by_hand_gets_what_was_detected(self):
		results = self.results()
		(results / "machines").mkdir()
		self.file(results, "windows-laptop").write_text('name = "ASUS ROG Zephyrus G15 (2021)"\n', encoding="utf-8")
		self.assertEqual(self.identify(results, HARDWARE, "windows-laptop"), "windows-laptop")
		described = record.tomllib.loads(self.file(results, "windows-laptop").read_text(encoding="utf-8"))
		self.assertEqual(described["name"], "ASUS ROG Zephyrus G15 (2021)")
		self.assertEqual(described["detected"]["threads"], 16)
		self.assertEqual(self.identify(results, HARDWARE), "windows-laptop")

	def test_a_description_that_cannot_be_read_stops_the_run(self):
		results = self.results()
		(results / "machines").mkdir()
		self.file(results, LAPTOP).write_text('name = "unclosed\n', encoding="utf-8")
		with self.assertRaises(SystemExit):
			self.identify(results, HARDWARE)


CPUINFO_X86 = """processor\t: 0
vendor_id\t: AuthenticAMD
model name\t: AMD Ryzen 9 7950X 16-Core Processor
processor\t: 1
model name\t: AMD Ryzen 9 7950X 16-Core Processor
"""

CPUINFO_PI = """processor\t: 0
BogoMIPS\t: 38.40
Features\t: fp asimd evtstrm crc32 cpuid
CPU part\t: 0xd03

Hardware\t: BCM2835
Model\t\t: Raspberry Pi 3 Model B Rev 1.2
"""

MOUNTS = """/dev/nvme0n1p2 / btrfs rw,relatime 0 0
tmpfs /tmp tmpfs rw 0 0
/dev/nvme0n1p2 /home btrfs rw,relatime,subvol=/@home 0 0
/dev/block/dm-48 /data f2fs rw,lazytime 0 0
"""


class Hardware(unittest.TestCase):
	def test_the_processors_name_on_a_pc_and_a_raspberry_pi(self):
		self.assertEqual(record.processor_from_cpuinfo(CPUINFO_X86), "AMD Ryzen 9 7950X 16-Core Processor")
		self.assertEqual(record.processor_from_cpuinfo(CPUINFO_PI), "BCM2835")
		self.assertIsNone(record.processor_from_cpuinfo("processor\t: 0\n"))

	def test_the_processors_present_also_those_offline(self):
		self.assertEqual(record.cpus_from_list("0-7\n"), 8)
		self.assertEqual(record.cpus_from_list("0-3,6"), 5)
		self.assertEqual(record.cpus_from_list("0"), 1)
		self.assertIsNone(record.cpus_from_list(""))
		self.assertIsNone(record.cpus_from_list("none"))

	def test_memory_from_meminfo(self):
		self.assertEqual(record.memory_from_meminfo("MemTotal:       32768000 kB\nMemFree: 1 kB\n"), 32768000 * 1024)
		self.assertIsNone(record.memory_from_meminfo("MemFree: 1 kB\n"))

	def test_the_mount_that_holds_a_folder(self):
		self.assertEqual(record.mount_of(MOUNTS, "/home/ada/nibkeep/target"), ("/dev/nvme0n1p2", "btrfs"))
		self.assertEqual(record.mount_of(MOUNTS, "/tmp/x"), ("tmpfs", "tmpfs"))
		self.assertEqual(record.mount_of(MOUNTS, "/srv"), ("/dev/nvme0n1p2", "btrfs"))
		self.assertEqual(record.mount_of(MOUNTS, "/data/local/tmp/run"), ("/dev/block/dm-48", "f2fs"))
		self.assertEqual(record.mount_of(MOUNTS, "/database"), ("/dev/nvme0n1p2", "btrfs"))

	def test_a_phone_on_a_cable_is_not_on_battery(self):
		self.assertFalse(record.battery_from_dumpsys("  AC powered: false\n  USB powered: true\n  level: 80\n"))
		self.assertTrue(record.battery_from_dumpsys("  AC powered: false\n  USB powered: false\n"))
		self.assertIsNone(record.battery_from_dumpsys(""))

	def test_a_linux_computer_on_battery(self):
		self.assertTrue(record.battery_from_supplies([("Mains", "0"), ("Battery", None)]))
		self.assertFalse(record.battery_from_supplies([("Mains", "1"), ("Battery", None)]))
		self.assertIsNone(record.battery_from_supplies([]))
		self.assertIsNone(record.battery_from_supplies([("USB", "0")]))

	def test_a_summary_for_people(self):
		self.assertEqual(
			record.summary(HARDWARE),
			"AMD Ryzen 9 5900HS with Radeon Graphics, 16 threads, 40 GB of memory, "
			"HFM001TD3JX013N (NVMe), NTFS, Windows 11, build 26300",
		)


class Numbers(unittest.TestCase):
	SAMPLE = {
		"data": "51859ce3a5ea", "recipe": 1, "languages": "first", "entities": 1000,
		"entities_written": 990, "import_seconds": 10.0, "entities_written_per_second": 99.0,
		"disk_bytes": 500_000, "receive_seconds": 2.0, "changes_received_per_second": 500.0,
		"open_seconds": 0.03, "search_median_ms": 2.0, "search_p95_ms": 9.0,
		"search_first_median_ms": 1.0, "search_first_p95_ms": 4.0, "show_median_ms": 0.1,
		"show_p95_ms": 0.2, "show_past_median_ms": 0.1, "verify_seconds": 3.0,
		"peak_memory_bytes": 90_000_000,
	}

	def test_a_run_keeps_the_numbers_that_matter(self):
		self.assertEqual(record.kept_api(self.SAMPLE), {
			"recipe": 1, "entities": 1000, "open_seconds": 0.03, "search_first_p95_ms": 4.0,
			"entities_written_per_second": 99.0, "changes_received_per_second": 500.0,
			"peak_memory_bytes": 90_000_000, "disk_bytes_per_entity": 500.0,
		})

	def test_a_number_the_system_does_not_tell_is_left_out(self):
		kept = record.kept_api(dict(self.SAMPLE, peak_memory_bytes=None))
		self.assertNotIn("peak_memory_bytes", kept)

	def test_the_median_of_repeated_runs(self):
		samples = [dict(self.SAMPLE, open_seconds=value) for value in (0.05, 0.01, 0.03)]
		self.assertEqual(record.medians(samples)["open_seconds"], 0.03)

	def test_a_nib_command_keeps_its_time_without_unlocking(self):
		self.assertEqual(
			record.kept_nib({
				"command": "find", "runs": 10, "total_median_seconds": 0.4, "unlock_median_seconds": 0.3,
				"total_without_unlock_median_seconds": 0.1, "open_median_seconds": 0.02,
			}),
			{"kind": "nib", "command": "find", "runs": 10, "total_without_unlock_median_seconds": 0.1},
		)


def result(at, data=1, recipe=1, kind="api", open_seconds=1.0, **others):
	return {"at": at, "commit": "c", "rust": "1.99.0", "data": data, "recipe": recipe, "kind": kind,
		"open_seconds": open_seconds, **others}


class Compare(unittest.TestCase):
	def test_the_first_of_a_series(self):
		self.assertEqual(record.compare([], result("t1")), ["first of its series"])

	def test_new_data_or_a_new_recipe_starts_a_series(self):
		earlier = [result("t1", open_seconds=1.0)]
		self.assertEqual(record.compare(earlier, result("t2", data=2, open_seconds=9.0)), ["first of its series"])
		self.assertEqual(record.compare(earlier, result("t2", recipe=2, open_seconds=9.0)), ["first of its series"])

	def test_a_slower_number_is_flagged_against_the_median_of_the_last_three(self):
		earlier = [result(f"t{n}", open_seconds=value) for n, value in enumerate((9.0, 1.0, 1.1, 0.9))]
		notes = record.compare(earlier, result("t9", open_seconds=1.5))
		self.assertTrue(notes[0].startswith("SLOWER open_seconds"), notes)

	def test_descriptions_are_not_compared(self):
		earlier = [result("t1", entities=10, runs=10)]
		notes = record.compare(earlier, result("t2", entities=99, runs=3, data=1))
		self.assertEqual(notes, ["within 20% of the last 1"])

	def test_each_nib_command_is_a_series(self):
		earlier = [result("t1", kind="nib", command="find", open_seconds=1.0)]
		self.assertEqual(
			record.compare(earlier, result("t2", kind="nib", command="edit", open_seconds=5.0)),
			["first of its series"],
		)

	def test_a_label_for_people(self):
		self.assertEqual(record.label(result("t1")), "api, data 1, recipe 1")
		self.assertEqual(record.label(result("t1", kind="nib", command="edit")), "nib edit, data 1, recipe 1")


class Recording(Repositories):
	def test_results_are_appended_and_committed_on_their_branch(self):
		results = self.results()
		new = [result("2026-11-14T10:00:00Z", commit="abc", target="x86_64-pc-windows-msvc")]
		record.switch(results, "faster-search")
		flagged = quietly(record.record, results, "windows-laptop", new)
		self.assertFalse(flagged)
		lines = (results / "results" / "windows-laptop.jsonl").read_bytes()
		self.assertTrue(lines.endswith(b"}\n") and b"\r" not in lines)
		self.assertEqual(json.loads(lines)["commit"], "abc")
		self.assertEqual(
			run("git", "log", "-1", "--format=%s", cwd=results), "Results of abc on windows-laptop",
		)
		self.assertEqual(run("git", "status", "--porcelain", cwd=results), "")

	def test_a_machines_results_name_one_target(self):
		results = self.results()
		quietly(record.record, results, "android-m52", [result("t1", commit="a", target="aarch64-linux-android")])
		with self.assertRaises(SystemExit):
			quietly(record.record, results, "android-m52", [result("t2", commit="b", target="x86_64-unknown-linux-gnu")])

	def test_a_slower_result_is_recorded_and_flagged(self):
		results = self.results()
		quietly(record.record, results, "linux-desktop", [result("t1", commit="a", open_seconds=1.0)])
		self.assertTrue(quietly(record.record, results, "linux-desktop", [result("t2", commit="b", open_seconds=2.0)]))
		self.assertEqual(len((results / "results" / "linux-desktop.jsonl").read_text().splitlines()), 2)


if __name__ == "__main__":
	unittest.main()
