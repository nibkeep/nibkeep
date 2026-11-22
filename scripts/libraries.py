"""Where the benchmarks keep the libraries they make on this computer: in
target/bench-libraries, on the disk that holds the checkout, never in memory.

The system's directory of temporary files will not do: on Arch and other Linux systems it
is tmpfs, in memory, where nothing reaches a disk and synchronizing costs nothing, while on
Windows and macOS it is on a disk, so the numbers of different machines would not compare.
On Linux, a library that would be in memory, such as a checkout on tmpfs, is refused. A
run removes its library when it ends; one that is killed leaves it behind, until
target/bench-libraries is deleted, as `cargo clean` does.
Devices that scripts/bench-record.py measures over adb or SSH keep their libraries in
directories of their own, on a disk.
"""

import pathlib
import re
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASE = ROOT / "target" / "bench-libraries"
# File systems that keep files in memory.
MEMORY = frozenset({"tmpfs", "ramfs"})
MOUNTINFO = pathlib.Path("/proc/self/mountinfo")


def file_system(path, mountinfo):
	"""The type of the file system that holds `path`, an absolute POSIX path, as a Linux
	/proc/self/mountinfo says: that of the mount point closest to it, the one mounted last
	if several share it. None if no mount point holds it."""
	found, closest = None, -1
	for line in mountinfo.splitlines():
		before, separator, after = line.partition(" - ")
		fields = before.split()
		if not separator or len(fields) < 5 or not after.split():
			continue
		# Spaces and other characters in mount points are written as octal escapes.
		point = re.sub(r"\\([0-7]{3})", lambda escape: chr(int(escape.group(1), 8)), fields[4])
		inside = point == "/" or path == point or path.startswith(point.rstrip("/") + "/")
		if inside and len(point) >= closest:
			found, closest = after.split()[0], len(point)
	return found


def refuse_memory(path, mountinfo=None):
	"""Ends the script if `path` is in memory. Only Linux says where its files are: what it
	reads is /proc/self/mountinfo, unless `mountinfo` is given."""
	if mountinfo is None:
		if not MOUNTINFO.exists():
			return
		mountinfo = MOUNTINFO.read_text(encoding="utf-8")
	kind = file_system(str(path), mountinfo)
	if kind in MEMORY:
		sys.exit(
			f"{path} is on {kind}, in memory, where nothing reaches a disk: the benchmarks "
			"measure libraries on a disk, so move the checkout to one"
		)


def new_directory(prefix, base=BASE):
	"""A new, empty directory under `base` for a library, named with `prefix`, after
	checking that `base` is not in memory."""
	base.mkdir(parents=True, exist_ok=True)
	refuse_memory(base.resolve())
	return pathlib.Path(tempfile.mkdtemp(prefix=prefix, dir=base))
