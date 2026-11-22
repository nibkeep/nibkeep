"""Records benchmark results in the results repository, and compares them with earlier
runs (docs/Plan.md, section 4).

    python3 scripts/bench-record.py [--on DEVICE] [--data N] [--branch NAME] [--repeat N] [--machine LABEL] [--push]
    python3 scripts/bench-record.py --new-data DATA [--push]

The results repository is a Git repository of its own, apart from this one, in the folder
that `--results` or the variable NIBKEEP_BENCH_RESULTS names, or else in bench-results in
this checkout, which Git ignores here. The project keeps one, private, which the machines
that keep a series push to: cloned into that folder, it is the one recorded in. Where
there is none, the script starts one that only this computer has, which serves to compare
a change with the code before it. It holds:

- `data/N/`: revision N of the real data, as scripts/fetch-real-data.py fetched it, added
  with `--new-data`, which checks it against the hash in its manifest. Every machine
  measures the same revision, rather than data fetched on its own: Wikidata changes daily.
- `machines/LABEL.toml`: what each machine is. Under `[detected]`, what the system tells of
  its hardware, by which a run finds the machine: its system, model, processor, threads,
  memory and the disk measured on. Above, the same for people, which a person may correct
  or complete, such as with the memory's speed, and the script keeps.
- `results/LABEL.jsonl`: each machine's results, one per line.

A machine that no description fits gets one, and its label, such as
`rog-zephyrus-g15-ga503qr-windows`: its model, or its processor for one built from parts,
and its system, numbered when another machine has it. That is a machine not measured
before, or one with a new disk, more memory or another system, whose numbers start a series
of their own; a system's updates do not. What the system fails to tell once counts as no
difference. Two machines that look alike, such as two of the same Raspberry Pi with the same
card, are told apart only by `--machine`, which names the label.

Its branches are named after this repository's: the results of the commit checked out go
to the branch of the same name, which starts from the results' `main` the first time, so
that a branch's results are compared with main's on the same machine. A detached checkout
is `main` if main holds its commit, and needs `--branch` otherwise. A branch's results are
never merged into main: main's own runs measure its code once it is merged. The data and
the machines' descriptions are read from main.

It measures the commit checked out, and refuses a working tree with changes, whose numbers
would be recorded under a commit they do not belong to; for the same reason, it records
nothing if another commit is checked out before it is done. It measures the latest data
revision, or the one `--data` names: one commit measured on two revisions bridges them,
showing what the new data alone changes. `nibkeep-bench` imports the data `--repeat` times,
3 by default, each into a new library, and measures it, and the medians of the numbers
that matter are kept: opening, the 95th percentile of searches for a first page, the
import rate, a new replica's first sync, peak memory, and disk space per entity. On this
computer, it then imports the data once more, into a library that no run paired a replica
with, and measures `nib find` and `nib edit` on it with scripts/bench-nib.py, each without
unlocking. With `--on android` or `--on ssh:HOST`, it measures on another device, as
scripts/test-on-device.py reaches it, and so needs Linux or macOS: an Android phone over
adb, with the NDK in ANDROID_NDK_HOME, or a 64-bit ARM Linux computer over SSH, such as a
Raspberry Pi. It builds nibkeep-bench for the device, copies it into a directory of the
run's own there with the data, runs it there, and removes both; an interrupted run ends
there too. Devices measure no `nib`.

Each result names the commit and the branch measured, the Rust version, the target built
for, the data revision, the importer's recipe, the time of the run, what the system tells
of the hardware, and the machine's own measures by `nibkeep-bench --calibrate`: one core's
speed, and the disk's. A laptop on battery is warned of. A machine's results all name one
target, so that a phone's numbers cannot join a desktop's by a wrong label.

Each result is compared with the median of the last three results of its series, the same
machine, kind, data revision and recipe, by the time they were measured. A number more than
20% worse is flagged, unless the difference is too small to tell from noise, and the script
then ends with an error; a number more than 20% better is noted, and so is a change of Rust
version, which can explain a difference by itself. The results are committed; `--push`
pushes them, and main too, with the machines' descriptions, rebasing on what other machines
pushed first. Where origin cannot be reached, the run goes on with what this computer has,
and records here.

The libraries are made on a disk, never in memory, as scripts/libraries.py says: on this
computer in target/bench-libraries, and on a device in the run's directory there.
"""

import argparse
import datetime
import importlib.util
import json
import math
import os
import pathlib
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import time
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
# The variable that names the folder of the results repository.
RESULTS_VARIABLE = "NIBKEEP_BENCH_RESULTS"
# Where data revisions are taken out to, for nibkeep-bench to read.
DATA_CACHE = ROOT / "target" / "bench-data"
# What a new results repository starts with: lines that end the same on every system, and
# data kept byte for byte, as its hash counts it.
ATTRIBUTES = "* text=auto eol=lf\ndata/** -text\n"
# The files of a data revision.
DATA_FILES = ("entities.jsonl", "credits.jsonl", "manifest.json")
# How much worse a number may get before it is flagged.
TOLERANCE = 0.20
# How many earlier results of a series a result is compared with.
BASELINE = 3
# Differences smaller than these are noise, whatever their share.
FLOORS = {"_seconds": 0.01, "_ms": 1.0}
# The numbers kept of a run through the Rust API, in the order the charts show them.
API_NUMBERS = (
	"open_seconds", "search_first_p95_ms", "entities_written_per_second",
	"changes_received_per_second", "peak_memory_bytes", "disk_bytes_per_entity",
)
# The number kept of each nib command: its whole time without unlocking, which a passphrase
# makes cost a fixed fraction of a second by design.
NIB_NUMBER = "total_without_unlock_median_seconds"
# What describes a run rather than measuring it.
DESCRIPTIVE = {
	"kind", "command", "commit", "branch", "machine", "rust", "data", "recipe", "repeat",
	"runs", "at", "note", "entities", "target", "hardware", "calibration",
}
# What makes a series: results compare only within one.
SERIES = ("kind", "command", "data", "recipe")
# What a machine's label looks like, such as linux-desktop.
LABEL = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
# Numbers for which more is better; for every other number, less is.
MORE_IS_BETTER = {"entities_written_per_second", "changes_received_per_second"}
# What tells a machine from another, as the system tells it, and as a machine's description
# keeps it under [detected]: when one changes, it is another machine.
IDENTITY = ("system", "model", "processor", "threads", "memory_gb", "disk")
# How many of those, the system aside, must have been told both times to tell a machine.
TOLD_ENOUGH = 2
# What computers built from parts give for their model, which names none.
NO_MODEL = {
	"system product name", "to be filled by o.e.m.", "default string", "not applicable", "none",
	"o.e.m.", "unknown", "standard pc",
}
# How many times each nib command runs.
NIB_RUNS = 10
# What the names of programs end with.
EXE = ".exe" if sys.platform == "win32" else ""


def load(name, file):
	"""Another script of this folder, as a module."""
	spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / file)
	module = importlib.util.module_from_spec(spec)
	spec.loader.exec_module(module)
	return module


libraries = load("libraries", "libraries.py")
fetch = load("fetch_real_data", "fetch-real-data.py")


def git(*arguments, cwd=None):
	"""Runs git, in this checkout unless `cwd` says otherwise, and ends the script with git's
	own message if it fails."""
	done = subprocess.run(
		["git", *arguments], cwd=cwd or ROOT, capture_output=True, text=True, encoding="utf-8",
	)
	if done.returncode != 0:
		sys.exit(f"git {' '.join(arguments)} failed:\n{done.stderr.strip()}")
	return done.stdout.strip()


def git_succeeds(*arguments, cwd=None):
	return subprocess.run(["git", *arguments], cwd=cwd or ROOT, capture_output=True).returncode == 0


def git_bytes(*arguments, cwd):
	"""What git prints, as bytes, unchanged."""
	done = subprocess.run(["git", *arguments], cwd=cwd, capture_output=True)
	if done.returncode != 0:
		sys.exit(f"git {' '.join(arguments)} failed:\n{done.stderr.decode(errors='replace').strip()}")
	return done.stdout


# The results repository.


def results_directory(given):
	"""The folder of the results repository: as given, as the variable names it, or else in
	this checkout."""
	if given:
		return given
	named = os.environ.get(RESULTS_VARIABLE)
	return pathlib.Path(named) if named else ROOT / "bench-results"


def open_results(directory):
	"""The results repository in `directory`, started there if the folder does not exist or
	is empty, and given its first commit, on main, if it has none."""
	if not (directory / ".git").exists():
		if directory.exists() and any(directory.iterdir()):
			sys.exit(f"{directory} is not a Git repository: clone the results repository there, or name another folder")
		directory.mkdir(parents=True, exist_ok=True)
		git("init", "--quiet", "--initial-branch", "main", cwd=directory)
		print(f"Started a results repository in {directory}, which only this computer has")
	if not git_succeeds("rev-parse", "--verify", "--quiet", "HEAD", cwd=directory):
		git("symbolic-ref", "HEAD", "refs/heads/main", cwd=directory)
		(directory / ".gitattributes").write_text(ATTRIBUTES, encoding="utf-8", newline="\n")
		git("add", ".gitattributes", cwd=directory)
		git("commit", "--quiet", "-m", "Start the benchmark results", cwd=directory)
	return directory


def has_origin(results):
	return git_succeeds("remote", "get-url", "origin", cwd=results)


def remote_has(results, branch):
	"""Whether origin has the branch. Where origin cannot be reached, the run goes on with
	what this computer has, and records here: measuring needs no network."""
	if not has_origin(results):
		return False
	done = subprocess.run(
		["git", "ls-remote", "--heads", "origin", f"refs/heads/{branch}"],
		cwd=results, capture_output=True, text=True, encoding="utf-8",
	)
	if done.returncode != 0:
		print(f"origin cannot be reached, so {branch} is not brought up to date: {done.stderr.strip()}", file=sys.stderr)
		return False
	return bool(done.stdout.strip())


def code_branch(given):
	"""The branch of the commit measured: as given, the branch checked out, or main for a
	detached checkout that main holds."""
	if given:
		return given
	done = subprocess.run(
		["git", "symbolic-ref", "--quiet", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True,
	)
	if done.returncode == 0:
		return done.stdout.strip()
	for main in ("refs/heads/main", "refs/remotes/origin/main"):
		if git_succeeds("merge-base", "--is-ancestor", "HEAD", main):
			return "main"
	sys.exit("the checkout is detached at a commit that main does not hold: --branch names its branch")


def clean_results(results, allowed):
	"""Ends the script if files of the results repository were changed by hand, other than
	those `allowed`, or in the folders among them, which end with /."""
	changed = [
		path for path in git("diff", "--name-only", "HEAD", cwd=results).splitlines()
		if path and not any(path == each or (each.endswith("/") and path.startswith(each)) for each in allowed)
	]
	if changed:
		sys.exit(f"{results} has changes, which a run would mix with its results: {', '.join(changed)}")


def update(results, branch):
	"""Brings the branch checked out up to date with origin's, if origin has it and can be
	reached."""
	if not remote_has(results, branch):
		return
	if not git_succeeds("fetch", "--quiet", "origin", f"refs/heads/{branch}:refs/remotes/origin/{branch}", cwd=results):
		print(f"origin cannot be reached, so {branch} is not brought up to date", file=sys.stderr)
		return
	git("rebase", "--quiet", "--autostash", f"refs/remotes/origin/{branch}", cwd=results)


def switch(results, branch):
	"""Checks out the results branch named after the code's, up to date with origin's, made
	from main the first time."""
	if git_succeeds("rev-parse", "--verify", "--quiet", f"refs/heads/{branch}", cwd=results):
		git("switch", "--quiet", branch, cwd=results)
	elif remote_has(results, branch):
		git("fetch", "--quiet", "origin", f"refs/heads/{branch}:refs/remotes/origin/{branch}", cwd=results)
		git("switch", "--quiet", "--create", branch, "--track", f"origin/{branch}", cwd=results)
	else:
		git("switch", "--quiet", "--create", branch, "refs/heads/main", cwd=results)
	update(results, branch)


def push(results, branch):
	"""Pushes the branch, which is checked out; if another machine pushed first, rebases on
	it and tries again. Each machine writes its own files, so rebasing never conflicts. The
	results stay committed here when it fails."""
	for _ in range(3):
		done = subprocess.run(
			["git", "push", "--quiet", "--set-upstream", "origin", branch],
			cwd=results, capture_output=True, text=True,
		)
		if done.returncode == 0:
			return
		update(results, branch)
	sys.exit(f"git push failed, and the results wait in {results}:\n{done.stderr.strip()}")


# Data revisions.


def revisions(results):
	"""The data revisions on main, in order."""
	names = git("ls-tree", "--name-only", "refs/heads/main", "data/", cwd=results)
	return sorted(
		int(name.split("/")[1]) for name in names.splitlines() if re.fullmatch(r"data/[1-9][0-9]*", name)
	)


def manifest_of(results, revision):
	return json.loads(git_bytes("show", f"refs/heads/main:data/{revision}/manifest.json", cwd=results))


def intact(directory, manifest):
	"""Whether the data in `directory` is what its manifest names."""
	try:
		return fetch.digest(directory) == manifest.get("sha256")
	except OSError:
		return False


def same_manifest(directory, manifest):
	try:
		return json.loads((directory / "manifest.json").read_text(encoding="utf-8")) == manifest
	except (OSError, ValueError):
		return False


def add_data(results, directory):
	"""Adds the real data in `directory` to main as the next revision, and returns its
	number."""
	path = directory / "manifest.json"
	if not path.exists():
		sys.exit(f"{directory} has no manifest.json: fetch it with scripts/fetch-real-data.py")
	manifest = json.loads(path.read_text(encoding="utf-8"))
	if not intact(directory, manifest):
		sys.exit(f"{directory} does not match the hash in its manifest")
	clean_results(results, ())
	switch(results, "main")
	known = revisions(results)
	for revision in known:
		if manifest_of(results, revision).get("sha256") == manifest["sha256"]:
			sys.exit(f"{directory} is data revision {revision} already")
	revision = max(known, default=0) + 1
	target = results / "data" / str(revision)
	target.mkdir(parents=True)
	for name in DATA_FILES:
		shutil.copyfile(directory / name, target / name)
	git("add", f"data/{revision}", cwd=results)
	git("commit", "--quiet", "-m", f"Add data revision {revision}, {manifest.get('data')}", cwd=results)
	print(f"{directory} is data revision {revision}")
	return revision


def extract(results, revision, cache=None):
	"""A folder that holds data revision `revision`, taken out of main and checked against
	its manifest; one taken out before is used again while it is intact."""
	if revision not in revisions(results):
		sys.exit(f"the results repository has no data revision {revision}")
	manifest = manifest_of(results, revision)
	directory = (cache or DATA_CACHE) / str(revision)
	# The manifest is taken out last, so a folder that holds it was taken out whole.
	if intact(directory, manifest) and same_manifest(directory, manifest):
		return directory
	shutil.rmtree(directory, ignore_errors=True)
	directory.mkdir(parents=True)
	for name in DATA_FILES:
		(directory / name).write_bytes(git_bytes("show", f"refs/heads/main:data/{revision}/{name}", cwd=results))
	if not intact(directory, manifest):
		sys.exit(f"data revision {revision} does not match the hash in its manifest")
	return directory


# Machines.


def gigabytes(memory):
	"""Memory in whole gigabytes, rounded up to what was sold: the system keeps some."""
	return math.ceil(memory / (1 << 30))


def summary(hardware):
	"""What the system tells of the hardware, for people."""
	parts = [
		hardware.get("processor"),
		f"{hardware['cores']} threads" if hardware.get("cores") else None,
		f"{gigabytes(hardware['memory_bytes'])} GB of memory" if hardware.get("memory_bytes") else None,
		hardware.get("disk"), hardware.get("file_system"), hardware.get("system"),
	]
	return ", ".join(part for part in parts if part)


def clean_model(text):
	"""A model as the system tells it, without what repeats or pads it, such as `GA503QR_GA503QR`;
	None for a model that names nothing, as computers built from parts report."""
	model = " ".join(re.sub(r"(\S+)_\1\b", r"\1", (text or "").replace("\x00", "")).split())
	return None if not model or model.lower() in NO_MODEL else model


def short_processor(text):
	"""A processor's name without its trimmings, such as `(R)`, `CPU @ 3.70GHz` or `with
	Radeon Graphics`."""
	name = re.sub(r"\((?:R|TM)\)", "", text, flags=re.IGNORECASE)
	name = re.sub(r"\s(?:with|@)\s.*$", "", name)
	name = re.sub(r"\s(?:CPU|Processor|[0-9]+-Core)\b", "", name, flags=re.IGNORECASE)
	return " ".join(name.split())


def model_from_dmi(vendor, product, version):
	"""A PC's model from its firmware: the product's name, or for Lenovo, whose product names
	are type numbers, its version, such as `ThinkPad X1 Carbon Gen 9`."""
	if vendor.strip().upper() == "LENOVO" and clean_model(version):
		return clean_model(version)
	return clean_model(product)


def new_label(hardware, taken):
	"""A label for a new machine: its model, or its processor for one built from parts, and
	its system, numbered when another machine has it, such as one before a new disk."""
	named = hardware.get("model") or short_processor(hardware.get("processor") or "")
	base = re.sub(r"[^a-z0-9]+", "-", named.lower()).strip("-")[:40].strip("-") or "machine"
	label = f"{base}-{hardware.get('family', 'unknown')}"
	number = 1
	while (f"{label}-{number}" if number > 1 else label) in taken:
		number += 1
	return f"{label}-{number}" if number > 1 else label


def identity(hardware):
	"""What tells this machine from others, as a description keeps it under [detected]."""
	found = {
		"system": hardware.get("family"), "model": hardware.get("model"),
		"processor": hardware.get("processor"), "threads": hardware.get("cores"),
		"memory_gb": gigabytes(hardware["memory_bytes"]) if hardware.get("memory_bytes") else None,
		"disk": hardware.get("disk"),
	}
	return {key: value for key, value in found.items() if value not in (None, "")}


def differences(known, found):
	"""What differs between hardware detected before and now, in words. What the system did
	not tell, either time, counts as no difference."""
	return [
		f"{key} {known[key]} then, {found[key]} now"
		for key in IDENTITY if key in known and key in found and known[key] != found[key]
	]


def fits(known, found):
	"""Whether hardware detected before is this machine's: the same system, nothing told both
	times that differs, and enough told both times to tell."""
	if found.get("system") is None or known.get("system") != found.get("system"):
		return False
	told = [key for key in IDENTITY[1:] if key in known and key in found]
	return len(told) >= TOLD_ENOUGH and not differences(known, found)


def detected_table(found):
	# A string or a number of JSON is one of TOML too.
	return "[detected]\n" + "".join(
		f"{key} = {json.dumps(found[key], ensure_ascii=False)}\n" for key in IDENTITY if key in found
	)


def description_text(label, hardware):
	"""A new machine's description: what the system tells, for people, then [detected]."""
	processor = ", ".join(
		part for part in (hardware.get("processor"), f"{hardware['cores']} threads" if hardware.get("cores") else None)
		if part
	)
	memory = f"{gigabytes(hardware['memory_bytes'])} GB" if hardware.get("memory_bytes") else ""
	name = hardware.get("model") or short_processor(hardware.get("processor") or "") or label

	def text(value):
		return json.dumps(value or "", ensure_ascii=False)

	return "\n".join([
		f"# {label}, for the benchmarks.",
		"# The lines above [detected] are for people: correct or complete them by hand with what",
		"# the system cannot tell, such as the memory's speed and the disk's interface, and the",
		"# script keeps them. [detected] is what the system told, by which the script finds this",
		"# machine again: hardware that no longer fits it is another machine.",
		f"name = {text(name)}",
		f"processor = {text(processor)}",
		f"memory = {text(memory)}",
		f"disk = {text(hardware.get('disk'))}",
		f"system = {text(hardware.get('system'))}",
		"",
		detected_table(identity(hardware)),
	])


def read_description(path):
	try:
		return tomllib.loads(path.read_text(encoding="utf-8"))
	except tomllib.TOMLDecodeError as error:
		sys.exit(f"{path}: {error}")


def identify(results, hardware, given=None):
	"""The machine's label, from the descriptions on main: the one whose [detected] fits the
	hardware, or the one given, for two machines that look alike. A machine that none fits, a
	new one or one with a new disk, more memory or another system, gets a new description,
	and the run goes on. A new or changed description is committed on main, which the caller
	brought up to date."""
	git("switch", "--quiet", "main", cwd=results)
	found = identity(hardware)
	folder = results / "machines"
	described = {path.stem: read_description(path) for path in sorted(folder.glob("*.toml"))}
	if given:
		label = given
		known = described.get(label, {}).get("detected")
		if label not in described:
			folder.mkdir(exist_ok=True)
			(folder / f"{label}.toml").write_text(description_text(label, hardware), encoding="utf-8", newline="\n")
		elif known is not None and not isinstance(known, dict):
			sys.exit(f"{folder / f'{label}.toml'} has a key detected, which the script keeps for a table")
		elif known is None:
			# Written by hand: what the system tells goes below it.
			with (folder / f"{label}.toml").open("a", encoding="utf-8", newline="\n") as file:
				file.write("\n" + detected_table(found))
		elif not fits(known, found):
			sys.exit(
				f"{label} was other hardware: {'; '.join(differences(known, found)) or 'too little was told'}. "
				"Another machine takes a label of its own."
			)
	else:
		fitting = [
			label for label, description in described.items()
			if isinstance(description.get("detected"), dict) and fits(description["detected"], found)
		]
		if len(fitting) > 1:
			sys.exit(f"this machine fits several descriptions, {', '.join(fitting)}: --machine names it")
		if fitting:
			label = fitting[0]
		else:
			# Nor a label that has results without a description.
			label = new_label(hardware, set(described) | {path.stem for path in (results / "results").glob("*.jsonl")})
			folder.mkdir(exist_ok=True)
			(folder / f"{label}.toml").write_text(description_text(label, hardware), encoding="utf-8", newline="\n")
			print(f"A machine not measured before, {label}: described in {folder / f'{label}.toml'}")
	# Every description new or changed, this machine's and what a person wrote of others, so
	# that no change of theirs stops a switch of branches later.
	changed = git("diff", "--name-only", "HEAD", "--", "machines/", cwd=results).splitlines()
	untracked = git("ls-files", "--others", "--exclude-standard", "--", "machines/", cwd=results).splitlines()
	if changed or untracked:
		names = sorted({pathlib.PurePosixPath(path).stem for path in changed + untracked})
		git("add", "machines/", cwd=results)
		new = names == [label] and f"machines/{label}.toml" in untracked
		git("commit", "--quiet", "-m", f"Add machine {label}" if new else f"Describe {', '.join(names)}", cwd=results)
	return label


def processor_from_cpuinfo(text):
	"""The processor's name in /proc/cpuinfo: its model name on a PC, or the hardware's name
	on an ARM computer that gives one."""
	fields = {}
	for line in text.splitlines():
		key, _, value = line.partition(":")
		fields.setdefault(key.strip(), value.strip())
	return next((fields[key] for key in ("model name", "Hardware") if fields.get(key)), None)


def cpus_from_list(text):
	"""How many processors a list such as `0-7` or `0-3,6` names, as
	/sys/devices/system/cpu/present gives it: those present, also while a phone takes some
	offline, unlike /proc/cpuinfo."""
	count = 0
	for part in text.strip().split(","):
		first, _, last = part.partition("-")
		try:
			count += int(last or first) - int(first) + 1
		except ValueError:
			return None
	return count or None


def memory_from_meminfo(text):
	"""The memory in bytes, from /proc/meminfo."""
	found = re.search(r"^MemTotal:\s+([0-9]+) kB", text, re.MULTILINE)
	return int(found[1]) * 1024 if found else None


def mount_of(mounts, path):
	"""The device and file system of the mount that holds `path`, from /proc/mounts."""
	best = None
	for line in mounts.splitlines():
		fields = line.split()
		if len(fields) < 3:
			continue
		# Spaces and the like are written as octal escapes.
		device, point, kind = (re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), field) for field in fields[:3])
		inside = point == "/" or path == point or path.startswith(point.rstrip("/") + "/")
		if inside and (best is None or len(point) >= len(best[0])):
			best = (point, device, kind)
	return (best[1], best[2]) if best else None


def battery_from_dumpsys(text):
	"""Whether an Android phone runs on its battery, from `dumpsys battery`: plugged into
	anything, a computer's USB port too, it does not."""
	powered = re.findall(r"^\s*(?:AC|USB|Wireless|Dock) powered: (true|false)", text, re.MULTILINE)
	return None if not powered else "true" not in powered


def battery_from_supplies(supplies):
	"""Whether a Linux computer runs on its battery, from the type and the `online` of each
	of its power supplies; nothing for a computer without a battery."""
	if not any(kind == "Battery" for kind, _ in supplies):
		return None
	return not any(online == "1" for kind, online in supplies if kind != "Battery")


def read_text(path):
	try:
		return pathlib.Path(path).read_text(encoding="utf-8", errors="replace")
	except OSError:
		return ""


def output_of(command):
	"""What a command prints, or nothing if it cannot run or fails."""
	try:
		done = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
	except (OSError, subprocess.TimeoutExpired):
		return ""
	return done.stdout.strip() if done.returncode == 0 else ""


def linux_disk(source):
	"""The model of the disk under a device, such as a partition of an encrypted disk, with
	how it is attached."""
	if not source.startswith("/dev/"):
		return None
	try:
		devices = json.loads(output_of(["lsblk", "--json", "--inverse", "--output", "NAME,TYPE,MODEL,TRAN", source]) or "{}")
	except json.JSONDecodeError:
		return None

	def disks(listed):
		for device in listed:
			if device.get("type") == "disk":
				yield device
			yield from disks(device.get("children") or [])

	disk = next(disks(devices.get("blockdevices") or []), None)
	if not disk or not (disk.get("model") or "").strip():
		return None
	model = disk["model"].strip()
	return f"{model} ({disk['tran']})" if disk.get("tran") else model


def linux_hardware(directory):
	mount = mount_of(read_text("/proc/mounts"), str(directory.resolve()))
	try:
		release = platform.freedesktop_os_release().get("PRETTY_NAME", "Linux")
	except OSError:
		release = "Linux"
	supplies = []
	for supply in pathlib.Path("/sys/class/power_supply").glob("*"):
		online = read_text(supply / "online").strip() or None
		supplies.append((read_text(supply / "type").strip(), online))
	dmi = pathlib.Path("/sys/class/dmi/id")
	return {
		"family": "linux",
		"model": clean_model(read_text("/proc/device-tree/model")) or model_from_dmi(
			read_text(dmi / "sys_vendor"), read_text(dmi / "product_name"), read_text(dmi / "product_version"),
		),
		"processor": processor_from_cpuinfo(read_text("/proc/cpuinfo")),
		"cores": cpus_from_list(read_text("/sys/devices/system/cpu/present")),
		"memory_bytes": memory_from_meminfo(read_text("/proc/meminfo")),
		"disk": linux_disk(mount[0]) if mount else None,
		"file_system": mount[1] if mount else None,
		"system": f"{release}, Linux {platform.release()}",
		"on_battery": battery_from_supplies(supplies),
	}


def windows_hardware(directory):
	import ctypes
	import winreg
	from ctypes import wintypes

	def registry(path, name):
		try:
			with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path) as key:
				return str(winreg.QueryValueEx(key, name)[0]).strip()
		except OSError:
			return ""

	processor = registry(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString") or None
	bios = r"HARDWARE\DESCRIPTION\System\BIOS"
	model = model_from_dmi(
		registry(bios, "SystemManufacturer"), registry(bios, "SystemProductName"), registry(bios, "SystemVersion"),
	)

	class Memory(ctypes.Structure):
		_fields_ = [
			("length", wintypes.DWORD), ("load", wintypes.DWORD), ("total", ctypes.c_ulonglong),
			("available", ctypes.c_ulonglong), ("others", ctypes.c_ulonglong * 5),
		]

	memory = Memory(length=ctypes.sizeof(Memory))
	memory_bytes = memory.total if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(memory)) else None

	drive = os.path.splitdrive(str(directory.resolve()))[0]
	name = ctypes.create_unicode_buffer(64)
	told = ctypes.windll.kernel32.GetVolumeInformationW(f"{drive}\\", None, 0, None, None, None, name, len(name))
	disk = output_of([
		"powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
		f"Get-Partition -DriveLetter {drive.rstrip(':')} | Get-Disk | "
		"ForEach-Object { $_.FriendlyName + ' (' + $_.BusType + ')' }",
	])

	class Power(ctypes.Structure):
		_fields_ = [
			("line", ctypes.c_ubyte), ("flags", ctypes.c_ubyte), ("percent", ctypes.c_ubyte),
			("saver", ctypes.c_ubyte), ("left", wintypes.DWORD), ("full", wintypes.DWORD),
		]

	power = Power()
	on_battery = None
	# A flag of 128 means no battery; a line of 255, that it is not known.
	if ctypes.windll.kernel32.GetSystemPowerStatus(ctypes.byref(power)) and power.flags != 128 and power.line != 255:
		on_battery = power.line == 0
	# Windows 11 still calls itself 10, but for its builds.
	build = sys.getwindowsversion().build
	return {
		"family": "windows", "model": model,
		"processor": processor, "memory_bytes": memory_bytes, "disk": disk or None,
		"file_system": name.value if told else None,
		"system": f"Windows {11 if build >= 22000 else platform.release()}, build {build}", "on_battery": on_battery,
	}


def mac_hardware(directory):
	lines = output_of(["df", "-P", str(directory.resolve())]).splitlines()
	point = lines[-1].split()[-1] if len(lines) > 1 else "/"
	info = dict(
		(key.strip(), value.strip())
		for key, _, value in (line.partition(":") for line in output_of(["diskutil", "info", point]).splitlines())
	)
	disk = info.get("Device / Media Name")
	if disk and info.get("Protocol"):
		disk = f"{disk} ({info['Protocol']})"
	battery = output_of(["pmset", "-g", "batt"])
	memory = output_of(["sysctl", "-n", "hw.memsize"])
	return {
		"family": "macos",
		"model": clean_model(output_of(["sysctl", "-n", "hw.model"])),
		"processor": output_of(["sysctl", "-n", "machdep.cpu.brand_string"]) or None,
		"memory_bytes": int(memory) if memory.isdigit() else None,
		"disk": disk,
		"file_system": info.get("Type (Bundle)"),
		"system": f"macOS {platform.mac_ver()[0]}",
		"on_battery": ("Battery Power" in battery) if "drawing from" in battery else None,
	}


def hardware_here(directory):
	"""What this computer's system tells of its hardware, and of the disk that holds
	`directory`."""
	if sys.platform == "win32":
		found = windows_hardware(directory)
	elif sys.platform == "darwin":
		found = mac_hardware(directory)
	else:
		found = linux_hardware(directory)
	found["cores"] = found.get("cores") or os.cpu_count()
	return {key: value for key, value in found.items() if value not in (None, "")}


# The runs.


def check_label(machine):
	"""Ends the script if the machine's label is not lowercase words joined by -."""
	if not LABEL.fullmatch(machine):
		sys.exit("the machine's label is lowercase words joined by -, such as linux-desktop")


def clean():
	"""Ends the script if the working tree has changes to tracked files."""
	changed = git("status", "--porcelain", "--untracked-files=no")
	if changed:
		sys.exit(f"the working tree has changes, which the commit does not have:\n{changed}")


def context():
	"""What every result of this run shares, taken before building, as the commit built."""
	return {
		"commit": git("rev-parse", "--short=12", "HEAD"),
		"rust": subprocess.run(
			["rustc", "--version"], check=True, capture_output=True, text=True
		).stdout.split()[1],
		"at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
	}


class Here:
	"""Runs nibkeep-bench on this computer."""

	program = ROOT / "target" / "release" / f"nibkeep-bench{EXE}"

	def prepare(self, data):
		"""Builds the programs, and returns the path of the data to run with."""
		subprocess.run(
			["cargo", "build", "--release", "--quiet", "--locked", "-p", "nibkeep-generate", "-p", "nibkeep-cli"],
			cwd=ROOT, check=True,
		)
		return str(data)

	@property
	def target(self):
		"""The target that the programs are built for: this computer's."""
		version = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
		return next(line.split()[1] for line in version.splitlines() if line.startswith("host:"))

	def hardware(self):
		return hardware_here(libraries.BASE)

	def calibrate(self):
		"""The machine's own measures, on the disk the libraries are made on."""
		directory = libraries.new_directory("nibkeep-bench-calibrate-")
		try:
			return json.loads(self.output([self.program, "--calibrate", directory / "calibration"]))
		finally:
			shutil.rmtree(directory)

	@staticmethod
	def output(command):
		# What it writes to standard error says why it failed.
		return subprocess.run(command, check=True, stdout=subprocess.PIPE, text=True).stdout

	def run(self, arguments, keep=None):
		"""Runs nibkeep-bench with these arguments and a new library, returns what it
		printed, and moves the library to `keep` if given, or removes it."""
		directory = libraries.new_directory("nibkeep-bench-")
		library = directory / "library"
		try:
			output = self.output([self.program, *arguments, library])
			if keep:
				library.rename(keep)
			return output
		finally:
			shutil.rmtree(directory)

	def finish(self):
		pass


def test_on_device():
	"""scripts/test-on-device.py, whose devices this script uses."""
	# It imports scripts/devices.py, which is beside it.
	if str(ROOT / "scripts") not in sys.path:
		sys.path.insert(0, str(ROOT / "scripts"))
	return load("test_on_device", "test-on-device.py")


class There:
	"""Runs nibkeep-bench on another device, as scripts/test-on-device.py reaches it."""

	def __init__(self, spec):
		devices = test_on_device()
		if spec == "android":
			self.device = devices.Android()
		elif spec.startswith("ssh:") and len(spec) > 4:
			self.device = devices.Ssh(spec[4:])
		else:
			sys.exit("--on takes android or ssh:HOST")
		self.android = spec == "android"
		self.directory = None
		self.target = None

	def capture(self, command):
		"""What a command prints on the device; the script ends if the command fails."""
		status, output = self.device.capture(command)
		if status != 0:
			sys.exit(f"on the device, {command} failed with status {status}")
		return output

	def told(self, command):
		"""What a command prints on the device, or nothing if it fails."""
		status, output = self.device.capture(command)
		return output.strip() if status == 0 else ""

	def prepare(self, data):
		"""Builds nibkeep-bench for the device, and copies it into a directory of the run's
		own there, with the data. Returns the data's path there."""
		_, self.target, variables = self.device.cargo()
		subprocess.run(
			[
				"cargo", "build", "--release", "--quiet", "--locked", "--target", self.target,
				"-p", "nibkeep-generate", "--bin", "nibkeep-bench",
			],
			cwd=ROOT, env=variables, check=True,
		)
		built = pathlib.Path(variables.get("CARGO_TARGET_DIR", ROOT / "target"))
		program = built / self.target / "release" / "nibkeep-bench"
		# A directory of the run's own, as test-on-device.py makes, so runs keep apart.
		self.directory = f"{self.device.base()}-bench/run-{os.getpid()}-{int(time.time())}"
		here = shlex.quote(self.directory)
		self.capture(f"mkdir -p {here}")
		if self.device.push(program, f"{self.directory}/nibkeep-bench") != 0:
			sys.exit("could not copy nibkeep-bench to the device")
		self.capture(f"chmod 755 {here}/nibkeep-bench")
		if self.device.push(data, f"{self.directory}/data") != 0:
			sys.exit("could not copy the data to the device")
		return "data"

	def hardware(self):
		"""What the device's system tells of its hardware, and of the disk of the run's
		directory."""
		cpuinfo = self.told("cat /proc/cpuinfo")
		mount = mount_of(self.told("cat /proc/mounts"), self.directory)
		# The disk under the mount, where its device names it, or else the device's main one.
		named = re.match(r"/dev/(mmcblk[0-9]+|nvme[0-9]+n[0-9]+|sd[a-z]+)", mount[0]) if mount else None
		disk = None
		for block in [named[1]] if named else ["sda", "mmcblk0", "nvme0n1"]:
			disk = self.told(f"cat /sys/block/{block}/device/model || cat /sys/block/{block}/device/name")
			if disk:
				break
		found = {
			"processor": processor_from_cpuinfo(cpuinfo),
			"cores": cpus_from_list(self.told("cat /sys/devices/system/cpu/present"))
			or sum(1 for line in cpuinfo.splitlines() if re.match(r"processor\s*:", line)) or None,
			"memory_bytes": memory_from_meminfo(self.told("cat /proc/meminfo")),
			"disk": disk,
			"file_system": mount[1] if mount else None,
		}
		if self.android:
			found["family"] = "android"
			found["model"] = clean_model(
				" ".join(self.told(f"getprop ro.product.{part}") for part in ("manufacturer", "model"))
			)
			soc = " ".join(self.told(f"getprop ro.soc.{part}") for part in ("manufacturer", "model")).strip()
			found["processor"] = soc or found["processor"]
			found["system"] = f"Android {self.told('getprop ro.build.version.release')}"
			found["on_battery"] = battery_from_dumpsys(self.told("dumpsys battery"))
		else:
			found["family"] = "linux"
			found["model"] = clean_model(self.told("cat /proc/device-tree/model")) or model_from_dmi(
				*(self.told(f"cat /sys/class/dmi/id/{name}") for name in ("sys_vendor", "product_name", "product_version"))
			)
			found["system"] = f"Linux {self.told('uname -r')}"
			supplies = self.told(
				"for supply in /sys/class/power_supply/*; do "
				'echo "$(cat $supply/type) $(cat $supply/online 2>/dev/null)"; done'
			)
			found["on_battery"] = battery_from_supplies([
				(line.split()[0], line.split()[1] if len(line.split()) > 1 else None)
				for line in supplies.splitlines() if line.strip()
			])
		return {key: value for key, value in found.items() if value not in (None, "")}

	def calibrate(self):
		here = shlex.quote(self.directory)
		return json.loads(self.capture(f"cd {here} && ./nibkeep-bench --calibrate calibration"))

	def run(self, arguments, keep=None):
		"""Runs nibkeep-bench on the device with these arguments and a new library, which
		it then removes, and returns what it printed."""
		assert keep is None, "libraries stay on this computer only"
		here = shlex.quote(self.directory)
		program = " ".join(shlex.quote(argument) for argument in ["./nibkeep-bench", *arguments])
		return self.capture(
			f"cd {here} && rm -rf library && {program} library; status=$?; rm -rf library; exit $status"
		)

	def finish(self):
		"""Removes the run's directory, if it can: a device that cannot be reached keeps it,
		and the error that ended the run stays the one shown."""
		try:
			if self.directory:
				status, _ = self.device.capture(f"rm -rf {shlex.quote(self.directory)}")
				if status != 0:
					print(f"could not remove {self.directory} from the device", file=sys.stderr)
		finally:
			self.device.close()


def medians(samples):
	"""The median of each number of several runs; what describes them, from the first."""
	result = {}
	for key, value in samples[0].items():
		values = [sample.get(key) for sample in samples]
		if key in DESCRIPTIVE or not all(isinstance(each, (int, float)) and not isinstance(each, bool) for each in values):
			result[key] = value
		else:
			result[key] = round(statistics.median(values), 6)
	return result


def kept_api(sample):
	"""The numbers kept of a run through the Rust API, with the recipe and the size of the
	library; a number the system does not tell is left out."""
	numbers = dict(sample)
	if numbers.get("disk_bytes") and numbers.get("entities"):
		numbers["disk_bytes_per_entity"] = round(numbers["disk_bytes"] / numbers["entities"], 1)
	kept = {"recipe": sample["recipe"], "entities": sample["entities"]}
	kept.update((key, numbers[key]) for key in API_NUMBERS if numbers.get(key) is not None)
	return kept


def kept_nib(result):
	"""The number kept of a nib command."""
	return {"kind": "nib", "command": result["command"], "runs": result["runs"], NIB_NUMBER: result[NIB_NUMBER]}


def measure_api(runner, data, repeat):
	"""Runs nibkeep-bench `repeat` times on the data where the runner has it, and returns
	the medians of the numbers kept."""
	samples = [json.loads(runner.run(["--real", data])) for _ in range(repeat)]
	return {"kind": "api", "repeat": repeat, **kept_api(medians(samples))}


def measure_nib(library, runs):
	"""Runs scripts/bench-nib.py on a library of real data made already, and returns the
	results of its commands."""
	output = subprocess.run(
		[
			sys.executable, ROOT / "scripts" / "bench-nib.py", "--real",
			"--runs", str(runs), "--library", library,
		],
		# What it writes to standard error says why it failed.
		check=True, stdout=subprocess.PIPE, text=True,
	).stdout
	return [kept_nib(json.loads(line)) for line in output.splitlines() if line]


def series(result):
	return tuple(result.get(key) for key in SERIES)


def worse_by(key, old, new):
	"""How much worse `new` is than `old`, as a share; negative when it is better. Zero
	when the difference is too small to tell from noise."""
	floor = next((value for suffix, value in FLOORS.items() if key.endswith(suffix)), 0)
	if abs(new - old) < floor:
		return 0.0
	change = (new - old) / old
	return -change if key in MORE_IS_BETTER else change


def compare(earlier_results, result):
	"""What changed against the median of the last results of the same series, in words."""
	earlier = sorted(
		(each for each in earlier_results if series(each) == series(result)),
		key=lambda each: each["at"],
	)[-BASELINE:]
	if not earlier:
		return ["first of its series"]
	notes = []
	if earlier[-1].get("rust") != result.get("rust"):
		notes.append(f"Rust {earlier[-1].get('rust')} -> {result.get('rust')}")
	for key, value in result.items():
		if key in DESCRIPTIVE or not isinstance(value, (int, float)) or isinstance(value, bool):
			continue
		olds = [each[key] for each in earlier if isinstance(each.get(key), (int, float))]
		if not olds or statistics.median(olds) == 0:
			continue
		old = statistics.median(olds)
		worse = worse_by(key, old, value)
		if worse > TOLERANCE:
			notes.append(f"SLOWER {key}: {old:g} -> {value:g} ({worse:.0%} worse)")
		elif worse < -TOLERANCE:
			notes.append(f"better {key}: {old:g} -> {value:g} ({-worse:.0%} better)")
	if not any(note.startswith(("SLOWER", "better")) for note in notes):
		notes.append(f"within {TOLERANCE:.0%} of the last {len(earlier)}")
	return notes


def label(result):
	kind = " ".join(part for part in (result["kind"], result.get("command")) if part)
	return f"{kind}, data {result.get('data')}, recipe {result.get('recipe')}"


def read_results(path):
	if not path.exists():
		return []
	return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def record(results, machine, new):
	"""Compares the new results with the machine's earlier ones on the branch checked out,
	says how each went, appends them, commits them, and tells whether one was flagged."""
	path = results / "results" / f"{machine}.jsonl"
	path.parent.mkdir(exist_ok=True)
	earlier = read_results(path)
	targets = {result["target"] for result in earlier + new if "target" in result}
	if len(targets) > 1:
		sys.exit(f"{machine}'s results would name several targets: {', '.join(sorted(targets))}")
	flagged = False
	for result in new:
		notes = compare(earlier, result)
		flagged |= any(note.startswith("SLOWER") for note in notes)
		print(f"{label(result)} at {result['commit']}: {'; '.join(notes)}")
		earlier.append(result)
	# Lines end the same on every machine, Windows too.
	with path.open("a", encoding="utf-8", newline="\n") as file:
		for result in new:
			file.write(json.dumps(result, sort_keys=True, ensure_ascii=False) + "\n")
	git("add", path.relative_to(results).as_posix(), cwd=results)
	commits = sorted({result["commit"] for result in new})
	git("commit", "--quiet", "-m", f"Results of {', '.join(commits)} on {machine}", cwd=results)
	return flagged


def measure(arguments, results, branch, revision, data):
	"""Measures the commit checked out, and returns the machine's label and the results."""
	shared = context()
	runner = There(arguments.on) if arguments.on else Here()
	try:
		where = runner.prepare(data)
		hardware = runner.hardware()
		label = identify(results, hardware, arguments.machine)
		print(f"{label}: {summary(hardware)}")
		if hardware.get("on_battery"):
			print("On battery, which slows a laptop down: the numbers may not compare.", file=sys.stderr)
		shared.update(
			branch=branch, target=runner.target, data=revision, hardware=hardware, calibration=runner.calibrate(),
		)
		measured = measure_api(runner, where, arguments.repeat)
		new = [{**shared, **measured}]
		if isinstance(runner, Here):
			directory = libraries.new_directory("nibkeep-bench-nib-")
			try:
				# nib finds what a library keeps in the folder .nib of its folder. Not a run's
				# library, to which the run paired a replica.
				library = directory / "library"
				library.mkdir()
				runner.run(["--import", where], keep=library / ".nib")
				new.extend({**shared, "recipe": measured["recipe"], **each} for each in measure_nib(library, NIB_RUNS))
			finally:
				shutil.rmtree(directory)
	finally:
		runner.finish()
	# Another commit may have been built in part, as bench-nib.py builds nib again.
	now = git("rev-parse", "--short=12", "HEAD")
	if now != shared["commit"]:
		sys.exit(f"{now} was checked out during the run of {shared['commit']}: nothing is recorded")
	return label, new


def main():
	parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
	parser.add_argument("--machine", help="the label of one of two machines that look alike, found otherwise")
	parser.add_argument("--on", help="measure on another device: android, or ssh:HOST")
	parser.add_argument("--data", type=int, help="the data revision to measure, the latest by default")
	parser.add_argument("--branch", help="the branch of a detached checkout, which main does not hold")
	parser.add_argument("--repeat", type=int, default=3)
	parser.add_argument("--results", type=pathlib.Path, help=f"the results repository's folder, or {RESULTS_VARIABLE}")
	parser.add_argument("--new-data", type=pathlib.Path, help="add real data as the next revision")
	parser.add_argument("--push", action="store_true")
	arguments = parser.parse_args()
	if arguments.repeat < 1:
		parser.error("--repeat takes a number from 1")
	results = open_results(results_directory(arguments.results))

	if arguments.new_data:
		if arguments.machine or arguments.on or arguments.data is not None:
			sys.exit("--new-data adds data, and measures nothing")
		add_data(results, arguments.new_data)
		if arguments.push:
			push(results, "main")
		return
	if arguments.machine:
		check_label(arguments.machine)
	if arguments.on and sys.platform == "win32":
		sys.exit("--on builds for the device with scripts/test-on-device.py, which needs Linux or macOS")
	clean()
	branch = code_branch(arguments.branch)
	# What a person wrote in the machines' descriptions is committed with them.
	clean_results(results, ("machines/",))
	switch(results, "main")
	known = revisions(results)
	if not known:
		sys.exit(
			"the results repository holds no data: fetch it with scripts/fetch-real-data.py, "
			"then add it with --new-data"
		)
	revision = known[-1] if arguments.data is None else arguments.data
	label, new = measure(arguments, results, branch, revision, extract(results, revision))

	switch(results, branch)
	flagged = record(results, label, new)
	if arguments.push:
		if branch != "main":
			# Main holds the machines' descriptions, which a run may have added to.
			switch(results, "main")
			push(results, "main")
			switch(results, branch)
		push(results, branch)
	elif has_origin(results):
		print(f"Nothing is pushed. To push the results: git -C {results} push origin main {branch}")
	sys.exit(1 if flagged else 0)


if __name__ == "__main__":
	main()
