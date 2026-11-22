"""Tests of scripts/libraries.py: where the benchmarks keep their libraries.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import pathlib
import shutil
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
spec = importlib.util.spec_from_file_location("libraries", ROOT / "scripts" / "libraries.py")
libraries = importlib.util.module_from_spec(spec)
spec.loader.exec_module(libraries)

# A Linux /proc/self/mountinfo, as Arch has it: /tmp in memory, a home on another disk,
# and a mount point with a space, which mountinfo writes as \040.
MOUNTINFO = """\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
30 22 0:27 / /tmp rw,nosuid,nodev shared:13 - tmpfs tmpfs rw,size=8G
31 22 259:3 / /home rw,relatime shared:14 - xfs /dev/nvme0n1p3 rw
32 31 0:40 / /home/ada/ram rw,relatime shared:15 - ramfs ramfs rw
33 22 259:4 / /mnt/my\\040disk rw,relatime shared:16 - btrfs /dev/sda1 rw
"""


class FileSystem(unittest.TestCase):
	def test_the_closest_mount_holds_a_path(self):
		self.assertEqual(libraries.file_system("/tmp/nibkeep-bench-x", MOUNTINFO), "tmpfs")
		self.assertEqual(libraries.file_system("/home/ada/nibkeep/target", MOUNTINFO), "xfs")
		self.assertEqual(libraries.file_system("/home/ada/ram/library", MOUNTINFO), "ramfs")
		self.assertEqual(libraries.file_system("/var/lib", MOUNTINFO), "ext4")

	def test_a_mount_holds_only_what_is_inside_it(self):
		self.assertEqual(libraries.file_system("/tmpfoo/library", MOUNTINFO), "ext4")
		self.assertEqual(libraries.file_system("/tmp", MOUNTINFO), "tmpfs")

	def test_escaped_mount_points_are_read(self):
		self.assertEqual(libraries.file_system("/mnt/my disk/nibkeep", MOUNTINFO), "btrfs")

	def test_a_later_mount_on_the_same_point_hides_the_earlier(self):
		stacked = MOUNTINFO + "40 22 0:50 / /tmp rw - ext4 /dev/sdb1 rw\n"
		self.assertEqual(libraries.file_system("/tmp/library", stacked), "ext4")


class Refusing(unittest.TestCase):
	def test_a_library_in_memory_is_refused(self):
		for path in ("/tmp/library", "/home/ada/ram/library"):
			with self.assertRaises(SystemExit) as refused:
				libraries.refuse_memory(path, MOUNTINFO)
			self.assertIn("memory", str(refused.exception))

	def test_a_library_on_a_disk_is_not(self):
		libraries.refuse_memory("/home/ada/nibkeep/target/bench-libraries", MOUNTINFO)
		libraries.refuse_memory("/var/lib", MOUNTINFO)


class NewDirectory(unittest.TestCase):
	def test_each_library_gets_a_new_directory_under_the_base(self):
		base = ROOT / "target" / "test-bench-libraries"
		try:
			first = libraries.new_directory("nibkeep-bench-", base)
			second = libraries.new_directory("nibkeep-bench-", base)
			self.assertNotEqual(first, second)
			for made in (first, second):
				self.assertTrue(made.is_dir())
				self.assertEqual(made.parent, base)
				self.assertTrue(made.name.startswith("nibkeep-bench-"))
		finally:
			shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
	unittest.main()
