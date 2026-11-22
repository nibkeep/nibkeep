"""Building for a 64-bit ARM Linux computer reached over SSH, such as a Raspberry Pi: with
clang and lld, against the device's own C library and headers, copied once into a sysroot
under target/sysroots. Delete it there to copy it again, for example after installing more
libraries on the device; a new version of the C library is copied anew by itself.

Zig's way of linking against an older GNU C library (cargo-zigbuild) made programs that
crashed on a Raspberry Pi, with a bad pointer after a call into the C library, while the
same code linked against the device's own library did not. So the device's library it is.

Needs clang, and lld: `ld.lld`, or Zig, which carries it.
"""

import os
import pathlib
import re
import shlex
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
TARGET = "aarch64-unknown-linux-gnu"
SSH = ["ssh", "-o", "BatchMode=yes"]
# What the sysroot needs from the device: headers, the C library, and the runtime of the
# device's compiler that clang links with.
SYSROOT_PARTS = ["usr/include", "usr/lib/aarch64-linux-gnu", "usr/lib/gcc/aarch64-linux-gnu"]
# Copied too where the device has them: the kernel's headers, which newer Debian keeps
# here and links to from usr/include.
OPTIONAL_PARTS = ["usr/lib/linux"]


def output(host, command, options=SSH):
	"""What a command prints on the device, or exits with what went wrong."""
	done = subprocess.run([*options, host, command], capture_output=True, text=True, check=False)
	if done.returncode != 0:
		advice = ""
		if "Host key verification failed" in done.stderr:
			advice = f" Log in once with `ssh {host}` to accept its host key."
		sys.exit(f"cannot run a command on {host}: {done.stderr.strip()}.{advice}")
	return done.stdout.strip()


def check_device(host, options=SSH):
	"""Exits unless the device is 64-bit ARM Linux with the GNU C library and a compiler's
	runtime, and returns the library's version."""
	machine = output(host, "uname -m", options)
	if machine != "aarch64":
		sys.exit(f"{host} is {machine}; the programs are built for 64-bit ARM, aarch64")
	version = output(host, "getconf GNU_LIBC_VERSION", options).split()
	if len(version) != 2 or version[0] != "glibc":
		sys.exit(f"cannot tell which GNU C library {host} has")
	missing = output(
		host,
		" ; ".join(f"test -d /{part} || echo /{part}" for part in SYSROOT_PARTS),
		options,
	)
	if missing:
		sys.exit(f"{host} lacks {missing.split()}: install gcc and libc6-dev there")
	return version[1]


def relink(directory):
	"""Points the absolute symbolic links of a copied sysroot inside it."""
	for path in directory.rglob("*"):
		if path.is_symlink():
			target = os.readlink(path)
			if target.startswith("/"):
				inside = directory / target.lstrip("/")
				path.unlink()
				path.symlink_to(os.path.relpath(inside, path.parent))


def sysroot(host, version, options=SSH):
	"""The device's headers and libraries, copied once for each version of its C library."""
	name = re.sub(r"[^A-Za-z0-9.-]", "_", host)
	directory = ROOT / "target" / "sysroots" / f"{name}-glibc-{version}"
	if (directory / "complete").exists():
		return directory
	if directory.exists():
		shutil.rmtree(directory)
	directory.mkdir(parents=True)
	# Without a merged /usr, the C library itself is in /lib.
	merged = output(host, "test -L /lib && echo merged || echo separate", options) == "merged"
	present = output(
		host, " ; ".join(f"test -e /{part} && echo {part}" for part in OPTIONAL_PARTS) + " ; true", options
	).split()
	parts = [*SYSROOT_PARTS, *present]
	if not merged:
		parts.append("lib/aarch64-linux-gnu")
	print(f"copying the C library and headers of {host} into {directory}", file=sys.stderr)
	# Symbolic links stay links; files that cannot be read are left out, not fatal.
	reading = subprocess.Popen(
		[*options, host, "tar -C / --ignore-failed-read -cf - " + " ".join(parts)],
		stdout=subprocess.PIPE,
	)
	unpacked = subprocess.run(["tar", "-C", str(directory), "-xf", "-"], stdin=reading.stdout, check=False)
	# So that the device's tar ends if this one stops reading.
	reading.stdout.close()
	if reading.wait() not in (0, 1) or unpacked.returncode != 0:
		sys.exit(f"cannot copy the sysroot from {host}")
	relink(directory)
	if merged:
		(directory / "lib").symlink_to("usr/lib")
	loader = directory / "lib" / "ld-linux-aarch64.so.1"
	if not loader.exists():
		loader.symlink_to("aarch64-linux-gnu/ld-linux-aarch64.so.1")
	(directory / "complete").write_text(version, encoding="utf-8")
	return directory


def lld():
	"""lld: ld.lld, or Zig's."""
	found = shutil.which("ld.lld")
	if found:
		return [found]
	zig = shutil.which("zig")
	if not zig:
		sys.exit("needs lld: install it, or Zig, which carries it")
	return [zig, "ld.lld"]


def wrapper(path, command, last=()):
	"""A script that runs a command with arguments given in full, quoted, so that no path
	is ever split at its spaces. `last` come after the caller's arguments, and a target
	that the caller gives is dropped, since clang keeps the first of several."""
	text = (
		"#!/bin/sh\n"
		"for argument do\n"
		"\tshift\n"
		'\tcase $argument in --target=*) ;; *) set -- "$@" "$argument" ;; esac\n'
		"done\n"
		f'exec {shlex.join(command)} "$@" {shlex.join(last)}\n'
	)
	path.write_text(text, encoding="utf-8")
	path.chmod(0o755)
	return str(path)


def build_environment(host, options=SSH):
	"""The environment in which cargo builds for the device, with a target directory of
	its own."""
	clang = shutil.which("clang")
	if not clang:
		sys.exit("needs clang")
	root = sysroot(host, check_device(host, options), options)
	# Debian's spelling of the target, under which clang finds its multiarch headers, in
	# place of the one that build scripts give.
	flags = ["--target=aarch64-linux-gnu", f"--sysroot={root}"]
	tools = root.parent / f"{root.name}-tools"
	tools.mkdir(exist_ok=True)
	linker = wrapper(tools / "ld.lld", lld())
	cc = wrapper(tools / "cc", [clang], flags)
	# Named after clang, so that rustc gives it the flags a C compiler takes.
	link = wrapper(tools / "link-clang", [clang], [*flags, f"--ld-path={linker}"])
	variables = dict(os.environ)
	variables["CARGO_TARGET_DIR"] = str(ROOT / "target" / "device")
	variables["CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"] = link
	variables["CC_aarch64_unknown_linux_gnu"] = cc
	variables["AR_aarch64_unknown_linux_gnu"] = shutil.which("llvm-ar") or "ar"
	return variables
