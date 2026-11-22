"""Cuts the power under a library on a real device, round after round, and checks that
nothing committed is lost.

    python3 scripts/power-cut.py USER@HOST [--rounds N] [--switch auto|URL] [--library PATH]
        [--fsck COMMAND] [--tree]

The device is a 64-bit ARM Linux computer reached over SSH, such as a Raspberry Pi. The
script builds the power-cut example of nibkeep-node for it, copies it there, and in every
round:

1. runs `power-cut write` on the device, which writes to a library and reports each
   operation that returned; the script records every report on this computer's disk;
2. cuts the power, or waits for it to be cut by hand, and notices when the device is
   gone. With `--switch`, the power switch of rig/power-switch cuts it, a random 2 to 60
   seconds after the writer gets going, and restores it a few seconds later; `auto` finds
   the switch on the local network;
3. waits for the device to come back, and runs `power-cut check`;
4. passes the round if the library is sound and holds every change and blob reported.
   It may hold more: those done just before the cut, whose reports never arrived.

The library is in ~/nibkeep-power/library on the device, or where `--library` says, such as
on a USB stick that the device mounts as it starts, which loses its power with the device;
it grows from round to round.

With `--tree`, the device writes to a tree with secret files instead: a laptop's library and
copy of the tree, and a desk's, all in ~/nibkeep-power/tree, or where `--library` says. The
desk edits, moves, deletes and adds secret files and commits, the laptop receives the commit,
updates its copy and commits its own files; the device reports the laptop's commits and the
number of the key its copy holds. A round passes if both libraries are sound, the tree keeps
every commit reported, and the copy holds whole versions, a key at least as new as the one
reported, and nothing that a commit would take for a change (`power-cut check-tree`, the
checks of the power-cut tests with LazyFS). The first round that fails ends the run, as the
next would start from a tree already found wrong.

On a drive without a journal, as FAT and exFAT, a cut leaves the library not closed, and
the drive is checked before it opens again (technical design: drives without a journal). There, `--fsck` gives the
command that checks the drive on the device, as
`sudo -n umount /mnt/stick && sudo -n fsck.exfat -y /dev/sda1; sudo -n mount /mnt/stick`;
after each cut, the round expects the library not closed, runs the command, and checks the
library with `power-cut check --drive-checked`, or with `--tree`, both libraries with
`power-cut check-tree --drive-checked`. A round where a library was closed fails: the
guard missed the cut.
"""

import argparse
import concurrent.futures
import http.client
import ipaddress
import json
import pathlib
import queue
import random
import shlex
import subprocess
import sys
import threading
import time
import urllib.request

import devices

ROOT = pathlib.Path(__file__).resolve().parent.parent
REMOTE = "nibkeep-power"
# Noticing quickly that the device is gone: a keep-alive every 2 seconds, three missed.
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=2", "-o", "ServerAliveCountMax=3"]


def say(text):
	print(f"{time.strftime('%H:%M:%S')} {text}", flush=True)


def ask(text):
	"""Says what the person at the device is to do, also as a desktop notification that
	stays until it is dismissed, where the desktop has notify-send."""
	say(text)
	subprocess.run(
		["notify-send", "--app-name=NibKeep", "--urgency=critical", "NibKeep power cuts", text],
		check=False, stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
	)


def ssh(host, command, **options):
	return subprocess.run([*SSH, "-o", "ConnectTimeout=5", host, command], check=False, **options)


def build(host):
	"""Builds the example against the device's own C library, and returns its path."""
	built = subprocess.run(
		[
			"cargo", "build", "--locked", "--profile", "checked",
			"--example", "power-cut", "-p", "nibkeep-node",
			"--target", devices.TARGET, "--message-format=json-render-diagnostics",
		],
		cwd=ROOT, stdout=subprocess.PIPE, text=True, check=False,
		env=devices.build_environment(host),
	)
	if built.returncode != 0:
		sys.exit("the example did not build")
	for line in built.stdout.splitlines():
		message = json.loads(line)
		if message.get("reason") == "compiler-artifact" and message.get("executable"):
			return message["executable"]
	sys.exit("the build gave no program")


class Switch:
	"""The power switch of rig/power-switch, reached over HTTP."""

	MARK = "nibkeep power switch:"

	def __init__(self, url):
		# The board prints its address as a URL; an address alone will do too.
		self.url = (url if "://" in url else f"http://{url}").rstrip("/")

	@classmethod
	def find(cls):
		"""Looks for the switch on the local IPv4 networks of this computer, with `ip`."""
		try:
			listed = subprocess.run(
				["ip", "-4", "-o", "addr", "show", "scope", "global"], capture_output=True, text=True, check=False
			).stdout.split()
		except FileNotFoundError:
			sys.exit("cannot list this computer's networks without `ip`: give the switch's address instead")
		hosts = []
		for word in listed:
			if "/" in word:
				network = ipaddress.ip_interface(word).network
				if network.num_addresses <= 1024:
					hosts.extend(str(host) for host in network.hosts())
		pool = concurrent.futures.ThreadPoolExecutor(64)
		probes = {pool.submit(cls(host).state, 1): host for host in hosts}
		try:
			for probe in concurrent.futures.as_completed(probes):
				if probe.result() is not None:
					return cls(probes[probe])
		finally:
			# Not waiting for the probes still queued, once the switch is found.
			pool.shutdown(wait=False, cancel_futures=True)
		sys.exit("found no power switch on the local network")

	def request(self, method, path, timeout):
		request = urllib.request.Request(f"{self.url}{path}", method=method)
		with urllib.request.urlopen(request, timeout=timeout) as response:
			return response.read().decode("utf-8", "replace")

	def state(self, timeout=3):
		"""'on' or 'off', or None if the switch does not answer, or what answers is no
		switch, such as a printer's web page."""
		try:
			reply = self.request("GET", "/status", timeout)
		except (OSError, ValueError, http.client.HTTPException):
			return None
		return reply.removeprefix(self.MARK).strip() if reply.startswith(self.MARK) else None

	def set(self, on):
		"""Switches the power, and checks that it is so, trying for half a minute."""
		wanted = "on" if on else "off"
		for _ in range(30):
			try:
				self.request("POST", f"/{wanted}", 3)
			except (OSError, ValueError, http.client.HTTPException):
				pass
			if self.state() == wanted:
				return
			time.sleep(1)
		sys.exit(f"the power switch at {self.url} did not switch {wanted}")


class Kind:
	"""What the device writes to, a library or a tree with secret files, and how the example
	writes and checks it."""

	def __init__(self, tree):
		self.tree = tree
		self.write = "write-tree" if tree else "write"

	def described(self, numbers):
		"""Two numbers, as reported or kept, said in words."""
		if self.tree:
			return f"{numbers[0]} commits and key {numbers[1]}"
		return f"{numbers[0]} changes and {numbers[1]} blobs"

	def check(self, library, round_number, done, drive_checked=False):
		"""The example's check after a cut, against the last report; with `drive_checked`,
		once the drive without a journal was checked."""
		flag = "--drive-checked " if drive_checked else ""
		if self.tree:
			return f"check-tree {flag}{shlex.quote(library)} {round_number} {done[0]} {done[1]}"
		return f"check {flag}{shlex.quote(library)}"


def write_until_cut(host, library, kind, switch=None):
	"""Runs the writer on the library until the device is gone, and returns the last
	report. With a switch, cuts the power a random while after the writer gets going."""
	program = f"{REMOTE}/power-cut"
	process = subprocess.Popen(
		[*SSH, host, f"{program} {kind.write} {shlex.quote(library)}"],
		stdout=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True,
	)
	# The writer's lines come through a queue, so that the cut and the check after it go by
	# the clock even while the writer is silent; None marks the end of its output.
	lines = queue.Queue()

	def read():
		for line in process.stdout:
			lines.put(line)
		lines.put(None)

	threading.Thread(target=read, daemon=True).start()
	done = None
	cut_at = None
	cut_since = None
	while True:
		try:
			line = lines.get(timeout=1)
		except queue.Empty:
			line = ""
		if line is None:
			break
		parts = line.split()
		if len(parts) == 3 and parts[0] == "done":
			if done is None:
				if switch is None:
					ask("The device is writing: cut its power whenever you like.")
				else:
					cut_at = time.monotonic() + random.uniform(2, 60)
			done = (int(parts[1]), int(parts[2]))
		if cut_at is not None and time.monotonic() >= cut_at:
			say(f"cutting the power, after {kind.described(done)} were reported")
			switch.set(False)
			cut_at = None
			cut_since = time.monotonic()
		# Still connected a minute after the cut: the switch is not wired to the device.
		# The power goes back on in main.
		if cut_since is not None and time.monotonic() - cut_since > 60:
			process.kill()
			sys.exit("the switch is off, but the device still runs: is the circuit connected?")
	# Only a lost connection is the device going away; anything else is the writer failing.
	status = process.wait()
	if status != 255:
		sys.exit(f"the writer ended with {status}, not with a power cut; see its errors above")
	if done is None:
		sys.exit("the device went away before the writer reported anything")
	return done


def wait_for(host, minutes=10):
	deadline = time.monotonic() + minutes * 60
	while time.monotonic() < deadline:
		if ssh(host, "true", capture_output=True).returncode == 0:
			return
		time.sleep(5)
	sys.exit(f"{host} did not come back within {minutes} minutes")


def check(host, command):
	"""What the library kept, and whether it is sound, from the example's `command`, such as
	`check DIR`. If the device goes away during the check, it waits for it, and checks
	again."""
	for _ in range(3):
		done = ssh(host, f"{REMOTE}/power-cut {command}", capture_output=True, text=True)
		# ssh fails with 255 when the device went away, which says nothing of the library,
		# and when the program there died of a signal, as by a crash, which three times in a
		# row on a device that answers does.
		if done.returncode != 255:
			break
		say("the device went away during the check, or the check crashed; waiting for it")
		wait_for(host)
	else:
		return None, f"the check crashed or never ran: {done.stdout}{done.stderr}"
	lines = done.stdout.splitlines()
	kept = next((line.split()[1:] for line in lines if line.startswith("kept ")), None)
	if kept is None:
		return None, done.stdout + done.stderr
	return (int(kept[0]), int(kept[1])), "\n".join(lines[1:]) or done.stderr


def main():
	parser = argparse.ArgumentParser(description=" ".join(__doc__.split("\n\n")[0].split()))
	parser.add_argument("host")
	parser.add_argument("--rounds", type=int, default=3)
	parser.add_argument("--switch", help="the power switch: its URL, or auto to find it")
	parser.add_argument(
		"--library",
		help="the library on the device, such as one on a USB stick that it mounts as it starts; "
		f"by default {REMOTE}/library, or {REMOTE}/tree with --tree",
	)
	parser.add_argument(
		"--fsck",
		metavar="COMMAND",
		help="on a drive without a journal, the command that checks the drive on the device after a cut",
	)
	parser.add_argument(
		"--tree", action="store_true", help="write to a tree with secret files, not to a library alone"
	)
	options = parser.parse_args()
	library = options.library or f"{REMOTE}/{'tree' if options.tree else 'library'}"
	host = options.host
	switch = None
	if options.switch:
		switch = Switch.find() if options.switch == "auto" else Switch(options.switch)
		say(f"the power switch is at {switch.url}")
		switch.set(True)
	program = build(host)
	if ssh(host, f"mkdir -p {REMOTE}").returncode != 0:
		sys.exit(f"cannot reach {host}")
	subprocess.run(["scp", "-q", "-o", "BatchMode=yes", program, f"{host}:{REMOTE}/power-cut"], check=True)
	# On the device's disk before the first cut, which can come seconds later: a program cut
	# short there crashes as it starts, which looks like anything but itself.
	if ssh(host, "sync").returncode != 0:
		sys.exit(f"cannot reach {host}")
	try:
		run, failures = rounds(host, library, Kind(options.tree), options.rounds, switch, options.fsck)
	finally:
		# Whatever happens, the device is left powered.
		if switch is not None:
			switch.set(True)
	stopped = f", and the run stopped after round {run}" if run < options.rounds else ""
	ask(f"Done: {run - failures} of {run} rounds passed{stopped}. Leave the device's power on.")
	sys.exit(1 if failures else 0)


def check_drive(host, library, kind, round_number, done, fsck):
	"""After a cut on a drive without a journal: whether the library, or both of a tree's,
	was left not closed, as it must be, and after the drive's check with `fsck`, what was
	kept, and the check's report."""
	said = check(host, kind.check(library, round_number, done))[1].strip()
	if said != "not closed":
		return None, f"a library was closed, so the guard missed the cut: {said}"
	checked = ssh(host, fsck, capture_output=True, text=True)
	said = (checked.stdout + checked.stderr).strip().splitlines()
	say(f"the drive was checked, with exit code {checked.returncode}: {' / '.join(said[-3:])}")
	return check(host, kind.check(library, round_number, done, drive_checked=True))


def rounds(host, library, kind, count, switch, fsck):
	"""Runs the rounds, and returns how many ran and how many failed. A tree's first failure
	ends them."""
	failures = 0
	for round_number in range(1, count + 1):
		say(f"round {round_number}: writing")
		done = write_until_cut(host, library, kind, switch)
		if switch is None:
			ask(f"The device is gone, after reporting {kind.described(done)}: "
				"restore its power, and wait for the next round before cutting it again.")
		else:
			say(f"the device is gone, after reporting {kind.described(done)}")
			time.sleep(random.uniform(1, 5))
			switch.set(True)
		wait_for(host)
		say("the device is back; checking the library")
		if fsck:
			kept, report = check_drive(host, library, kind, round_number, done, fsck)
		else:
			kept, report = check(host, kind.check(library, round_number, done))
		if kept is None:
			ask(f"round {round_number} FAILED: the check found something wrong, or the library did not open:\n{report}")
			failures += 1
			if kind.tree:
				return round_number, failures
			continue
		changes_ok = kept[0] >= done[0]
		blobs_ok = kept[1] >= done[1]
		sound = report.strip() == "sound"
		verdict = "passed" if changes_ok and blobs_ok and sound else "FAILED"
		if verdict == "FAILED":
			failures += 1
		message = (f"round {round_number} {verdict}: reported {kind.described(done)}, "
			f"kept {kind.described(kept)}; {report.strip()}")
		if verdict == "FAILED":
			ask(message)
			if kind.tree:
				return round_number, failures
		else:
			say(message)
	return count, failures


if __name__ == "__main__":
	main()
