"""Runs tests on another device, through cargo test: an Android phone, an iPhone, or a Linux
computer reached over SSH, such as a Raspberry Pi.

    python3 scripts/test-on-device.py android [cargo test arguments]
    python3 scripts/test-on-device.py ios [cargo test arguments]
    python3 scripts/test-on-device.py ssh USER@HOST [cargo test arguments]

It runs `cargo test` for the device, with this script as the runner that cargo hands each
test executable to. The runner copies the executable to the device, runs it there in a
directory of its own, and removes both afterwards. Each run works in a directory of its
own on the device, to which the compatibility fixtures go once, and which goes when the
run ends. Every argument goes to cargo test, so `-- blobs` runs only the tests whose names
contain `blobs`, and `--release` builds optimized tests.

Android: without `-p` or `--workspace`, the tests are those of the crates that continuous
integration checks for phones. Needs Linux or macOS, the target (rustup target add
aarch64-linux-android), the NDK in ANDROID_NDK_HOME, and adb with one device connected,
or the one ANDROID_SERIAL names.

iOS: runs on a Mac with Xcode, to which the iPhone is connected and paired, in Developer
Mode; without `-p` or `--workspace`, the tests are those of the phone crates. Each test
executable becomes an app, with the fixtures inside, signed with an Apple Development
certificate and a provisioning profile for the bundle ID in NIBKEEP_IOS_BUNDLE_ID,
org.nibkeep.Dinghy by default, which Xcode makes when it runs an empty app of that ID on
the iPhone once. The free profile of a personal team lasts a week. The app is installed
and run with Xcode's devicectl, which needs no administrator, under LLDB: iOS ends an
app that has not finished launching after 20 seconds, which a test executable never does,
except under a debugger (scripts/ios_lldb_wait.py). Over SSH, codesign cannot use
the login keychain: scripts/ios-signing-keychain.sh makes one that the run unlocks. Needs
the target (rustup target add aarch64-apple-ios); IOS_DEVICE picks one of several iPhones.
One run at a time on an iPhone: every run installs the same app, and its tests share the
app's temporary directory.

SSH: a 64-bit ARM Linux computer that the user logs into with a key; without `-p` or
`--workspace`, the tests are those of the whole workspace. They are built with clang
against the device's own C library and headers, which scripts/devices.py copies once. One
connection serves the whole run, and a test stops when the run is interrupted. Needs the
target (rustup target add aarch64-unknown-linux-gnu), clang, and lld or Zig.
"""

import datetime
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time

import devices

ROOT = pathlib.Path(__file__).resolve().parent.parent
# The argument with which cargo calls this script as the runner.
ON_DEVICE = "--on-device"


def phone_crates():
	"""The crates that continuous integration checks for phones."""
	workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
	found = re.search(r"PHONE_CRATES: (.*)", workflow)
	if not found:
		sys.exit("no PHONE_CRATES in .github/workflows/ci.yml")
	return found.group(1).split()


def quote(path):
	return shlex.quote(path)


class Remote:
	"""A device to which a test executable is copied and run with a shell: what Android and
	SSH share. Each needs base(), shell(), push() and close()."""

	def prepare(self):
		"""Makes the run's directory on the device, with the fixtures in it."""
		if self.shell(f"mkdir -p {quote(self.directory)}") != 0:
			return 1
		return self.push(ROOT / "fixtures", f"{self.directory}/fixtures")

	def finish(self):
		"""Removes the run's directory from the device."""
		self.shell(f"rm -rf {quote(self.directory)}")
		self.close()

	def run(self, executable, arguments):
		"""Copies a test executable to the device, runs it there with the fixtures that the
		run copied, and removes it."""
		name = pathlib.Path(executable).name
		remote = f"{self.directory}/{name}"
		files = f"{remote}.files"
		fixtures = f"{self.directory}/fixtures"
		try:
			if self.shell(f"mkdir -p {quote(files)}") != 0:
				return 1
			if self.push(executable, remote) != 0:
				return 1
			test_arguments = " ".join(quote(argument) for argument in arguments)
			variables = f"NIBKEEP_TEST_DIR={quote(files)} NIBKEEP_FIXTURES={quote(fixtures)}"
			return self.shell(f"cd {quote(files)} && {variables} {quote(remote)} {test_arguments}")
		finally:
			self.shell(f"rm -rf {quote(remote)} {quote(files)}")


class Android(Remote):
	"""An Android phone, reached with adb."""

	TARGET = "aarch64-linux-android"
	# The oldest Android the tests are built for: 7.0.
	API = 24
	# The NDK's directory of tools for each system it runs on.
	HOSTS = {"Linux": "linux-x86_64", "Darwin": "darwin-x86_64"}

	def __init__(self, directory=None):
		self.directory = directory

	def spec(self):
		return ["android", self.directory]

	def default_crates(self):
		return phone_crates()

	def base(self):
		"""The directory under which runs work."""
		return "/data/local/tmp/nibkeep"

	def cargo(self):
		"""The command that runs cargo test for the device, and its environment."""
		host = self.HOSTS.get(platform.system())
		if host is None:
			sys.exit(f"this script runs on Linux and macOS, not on {platform.system()}")
		ndk = os.environ.get("ANDROID_NDK_HOME")
		if not ndk:
			sys.exit("set ANDROID_NDK_HOME to the Android NDK, such as ~/Android/android-ndk-r30")
		tools = pathlib.Path(ndk).expanduser() / "toolchains" / "llvm" / "prebuilt" / host / "bin"
		compiler = tools / f"{self.TARGET}{self.API}-clang"
		if not compiler.exists():
			sys.exit(f"{compiler} does not exist: is ANDROID_NDK_HOME an NDK for {host}?")
		variables = dict(os.environ)
		variables["CC_aarch64_linux_android"] = str(compiler)
		variables["AR_aarch64_linux_android"] = str(tools / "llvm-ar")
		variables["CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"] = str(compiler)
		return ["cargo", "test", "--target", self.TARGET], self.TARGET, variables

	def shell(self, command):
		return subprocess.run(["adb", "shell", command], check=False).returncode

	def capture(self, command):
		"""Runs a command on the device, and returns its status and what it printed."""
		done = subprocess.run(["adb", "shell", command], capture_output=True, text=True, check=False)
		return done.returncode, done.stdout

	def push(self, local, remote):
		return subprocess.run(["adb", "push", "-q", str(local), remote], check=False).returncode

	def close(self):
		pass


class Ios:
	"""An iPhone connected to this Mac, reached with Xcode's devicectl."""

	TARGET = "aarch64-apple-ios"
	# The oldest iOS the tests are built for, for Rust and the C code alike.
	DEPLOYMENT_TARGET = "15.0"
	KEYCHAIN = pathlib.Path.home() / "Library" / "Keychains" / "nibkeep-signing.keychain-db"
	KEYCHAIN_PASSWORD = pathlib.Path.home() / ".config" / "nibkeep" / "signing-keychain-password"
	PROFILES = [
		pathlib.Path.home() / "Library" / "Developer" / "Xcode" / "UserData" / "Provisioning Profiles",
		pathlib.Path.home() / "Library" / "MobileDevice" / "Provisioning Profiles",
	]

	def __init__(self, device=None, directory=None):
		self.device = device
		self.directory = directory

	def spec(self):
		return ["ios", self.device, self.directory]

	def default_crates(self):
		return phone_crates()

	def base(self):
		"""The directory on this Mac under which runs keep their apps."""
		return str(ROOT / "target" / "ios-apps")

	@staticmethod
	def environment():
		"""This environment, with Xcode's tools rather than the Command Line Tools alone."""
		variables = dict(os.environ)
		xcode = pathlib.Path("/Applications/Xcode.app/Contents/Developer")
		if "DEVELOPER_DIR" not in variables and xcode.exists():
			variables["DEVELOPER_DIR"] = str(xcode)
		variables["IPHONEOS_DEPLOYMENT_TARGET"] = Ios.DEPLOYMENT_TARGET
		# macOS's own python3 sets SDKROOT to the Mac's SDK, which the linker and the C
		# compiler would then take for the iPhone too. Without it, each finds the SDK for
		# what it builds, the iPhone's or the Mac's.
		variables.pop("SDKROOT", None)
		return variables

	@staticmethod
	def devicectl(*arguments):
		"""What devicectl reports as JSON, or exits with what went wrong."""
		with tempfile.TemporaryDirectory() as scratch:
			report = pathlib.Path(scratch) / "report.json"
			done = subprocess.run(
				["xcrun", "devicectl", *arguments, "--quiet", "--json-output", str(report)],
				env=Ios.environment(), capture_output=True, text=True, check=False,
			)
			if done.returncode != 0:
				sys.exit(f"devicectl {' '.join(arguments[:3])} failed: {done.stderr.strip()}")
			return json.loads(report.read_text(encoding="utf-8"))["result"]

	def find_device(self):
		"""The iPhone's identifier: IOS_DEVICE, or the only iPhone that is connected."""
		wanted = os.environ.get("IOS_DEVICE")
		phones = [
			found
			for found in self.devicectl("list", "devices")["devices"]
			if found["hardwareProperties"].get("platform") == "iOS"
			and found["hardwareProperties"].get("reality") == "physical"
			and found["connectionProperties"].get("pairingState") == "paired"
			# Phones paired once but not here now are listed too, as unavailable.
			and found["connectionProperties"].get("tunnelState") != "unavailable"
		]
		if wanted:
			phones = [found for found in phones if wanted in (found["hardwareProperties"]["udid"], found["deviceProperties"]["name"])]
		if len(phones) != 1:
			names = [found["deviceProperties"]["name"] for found in phones]
			sys.exit(f"needs one paired iPhone connected, or IOS_DEVICE to pick one; found {names}")
		return phones[0]["hardwareProperties"]["udid"]

	def cargo(self):
		"""The command that runs cargo test for the iPhone, which also readies the signing."""
		if platform.system() != "Darwin":
			sys.exit("the tests run on an iPhone from a Mac with Xcode")
		self.device = self.device or self.find_device()
		if self.KEYCHAIN.exists() and self.KEYCHAIN_PASSWORD.exists():
			# The password goes in through standard input, where no other process sees it.
			with self.KEYCHAIN_PASSWORD.open("rb") as password:
				subprocess.run(
					["security", "unlock-keychain", str(self.KEYCHAIN)],
					stdin=password, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True,
				)
		return ["cargo", "test", "--target", self.TARGET], self.TARGET, self.environment()

	def prepare(self):
		"""Makes the run's directory on this Mac, and finds what signs the apps once for the
		whole run: the runner reads it from there for each test executable."""
		directory = pathlib.Path(self.directory)
		directory.mkdir(parents=True)
		bundle_id = os.environ.get("NIBKEEP_IOS_BUNDLE_ID", "org.nibkeep.Dinghy")
		profile, entitlements, identity = self.signing(bundle_id)
		(directory / "entitlements.plist").write_bytes(plistlib.dumps(entitlements))
		# LLDB names the iPhone by its CoreDevice identifier, not by its UDID.
		debugger_device = next(
			(
				found["identifier"]
				for found in self.devicectl("list", "devices")["devices"]
				if found["hardwareProperties"].get("udid") == self.device
			),
			self.device,
		)
		signing = {
			"bundle_id": bundle_id, "profile": str(profile), "identity": identity,
			"debugger_device": debugger_device,
		}
		(directory / "signing.json").write_text(json.dumps(signing), encoding="utf-8")
		return 0

	def finish(self):
		shutil.rmtree(self.directory, ignore_errors=True)

	def signing(self, bundle_id):
		"""The provisioning profile for the bundle ID and this iPhone, its entitlements, and
		the certificate to sign with, as its SHA-1 hash, which must be in a keychain."""
		identities = subprocess.run(
			["security", "find-identity", "-v", "-p", "codesigning"], capture_output=True, text=True, check=False
		).stdout
		hashes = set(re.findall(r"\b([0-9A-F]{40})\b", identities))
		now = datetime.datetime.now(datetime.timezone.utc)
		expired = False
		for folder in self.PROFILES:
			for path in sorted(folder.glob("*.mobileprovision")) if folder.exists() else []:
				decoded = subprocess.run(
					["security", "cms", "-D", "-i", str(path)], capture_output=True, check=False
				).stdout
				if not decoded:
					continue
				profile = plistlib.loads(decoded)
				entitlements = profile.get("Entitlements", {})
				if not entitlements.get("application-identifier", "").endswith(f".{bundle_id}"):
					continue
				if self.device not in profile.get("ProvisionedDevices", []):
					continue
				if profile["ExpirationDate"].replace(tzinfo=datetime.timezone.utc) <= now:
					expired = True
					continue
				for certificate in profile.get("DeveloperCertificates", []):
					digest = hashlib.sha1(certificate).hexdigest().upper()
					if digest in hashes:
						return path, entitlements, digest
		if expired:
			sys.exit(f"the provisioning profile for {bundle_id} has expired: run the empty app of that ID on the iPhone from Xcode again")
		sys.exit(f"no provisioning profile for {bundle_id} and this iPhone with a certificate in a keychain: run an empty app of that ID on the iPhone from Xcode once")

	@staticmethod
	def info(bundle_id, name):
		"""The Info.plist of the app of the test executable `name`. It declares the Bonjour
		service that replicas announce (nibkeep-net's discovery), as iOS lets an app browse
		only the services it declares, for the tests that find devices on the local network;
		iOS asks once whether apps of the bundle ID may reach it."""
		return {
			"CFBundleIdentifier": bundle_id,
			"CFBundleExecutable": name,
			"CFBundleName": "Dinghy",
			"CFBundlePackageType": "APPL",
			"CFBundleVersion": "1",
			"CFBundleShortVersionString": "1.0",
			"CFBundleSupportedPlatforms": ["iPhoneOS"],
			"MinimumOSVersion": Ios.DEPLOYMENT_TARGET,
			"UIDeviceFamily": [1, 2],
			"LSRequiresIPhoneOS": True,
			"NSBonjourServices": ["_nibkeep._udp"],
			"NSLocalNetworkUsageDescription": "The tests find the devices that they start on the local network.",
		}

	def bundle(self, executable, app):
		"""Makes a signed app of a test executable, with the fixtures inside, and returns
		its bundle ID."""
		directory = pathlib.Path(self.directory)
		signing = json.loads((directory / "signing.json").read_text(encoding="utf-8"))
		bundle_id, profile, identity = signing["bundle_id"], signing["profile"], signing["identity"]
		name = pathlib.Path(executable).name
		app.mkdir(parents=True)
		shutil.copy2(executable, app / name)
		shutil.copytree(ROOT / "fixtures", app / "fixtures")
		shutil.copy2(profile, app / "embedded.mobileprovision")
		info = self.info(bundle_id, name)
		(app / "Info.plist").write_bytes(plistlib.dumps(info, fmt=plistlib.FMT_BINARY))
		# The signing keychain, when there is one: the login keychain holds the same
		# certificate, but over SSH its key cannot be used.
		keychain = ["--keychain", str(self.KEYCHAIN)] if self.KEYCHAIN.exists() else []
		signed = subprocess.run(
			["codesign", "--force", "--timestamp=none", "--sign", identity, *keychain,
				"--entitlements", str(directory / "entitlements.plist"), str(app)],
			capture_output=True, text=True, check=False,
		)
		if signed.returncode != 0:
			advice = ""
			if "errSecInternalComponent" in signed.stderr:
				advice = " Over SSH, make the signing keychain with scripts/ios-signing-keychain.sh."
			sys.exit(f"cannot sign {app}: {signed.stderr.strip()}.{advice}")
		return bundle_id

	def processes(self, name):
		"""The IDs of the processes on the iPhone whose executable is the app's `name`; none
		when devicectl cannot list them just now."""
		with tempfile.TemporaryDirectory() as scratch:
			report = pathlib.Path(scratch) / "report.json"
			done = subprocess.run(
				["xcrun", "devicectl", "device", "info", "processes", "--device", self.device,
					"--quiet", "--json-output", str(report)],
				env=Ios.environment(), capture_output=True, text=True, check=False,
			)
			if done.returncode != 0 or not report.exists():
				return set()
			running = json.loads(report.read_text(encoding="utf-8"))["result"]["runningProcesses"]
		return {
			each["processIdentifier"]
			for each in running
			if each.get("executable", "").endswith(f"/Dinghy.app/{name}")
		}

	def attach(self, name, launch, earlier, log, variables):
		"""Attaches LLDB to the suspended app whose executable is `name`, a process not
		among `earlier`, lets it run, and returns LLDB, which stays until the app ends and
		writes to `log`; or None if `launch` ends first, or the app is not there within a
		minute."""
		signing = json.loads((pathlib.Path(self.directory) / "signing.json").read_text(encoding="utf-8"))
		waiting = pathlib.Path(__file__).resolve().parent / "ios_lldb_wait.py"
		deadline = time.monotonic() + 60
		while time.monotonic() < deadline and launch.poll() is None:
			started = self.processes(name) - earlier
			if started:
				return subprocess.Popen(
					["xcrun", "lldb", "--batch",
						"-o", f"command script import {waiting}",
						"-o", f"device select {signing['debugger_device']}",
						"-o", f"device process attach --continue --pid {max(started)}",
						"-o", "wait_for_exit"],
					env=variables, stdout=log, stderr=subprocess.STDOUT, text=True,
				)
			time.sleep(1)
		return None

	def run(self, executable, arguments):
		"""Runs a test executable on the iPhone as an app, with its output here, and returns
		its exit code. The app is deleted here once it has run, as the iPhone keeps its own
		copy, so that a run keeps one test executable's app at a time, not all of them."""
		app = pathlib.Path(self.directory) / pathlib.Path(executable).name / "Dinghy.app"
		try:
			return self.run_app(executable, arguments, app)
		finally:
			shutil.rmtree(app.parent, ignore_errors=True)

	@staticmethod
	def verdict(launched, status, summaries, arguments):
		"""The exit code of a test executable's run, and what to say of it, if anything: from
		what devicectl returned (`launched`), the exit code it reported for the app (`status`,
		None for none), whether each summary of the test harness said ok, and the arguments.
		devicectl returns 0 when the app returns 101, as when a test fails, before Xcode 27.2,
		and what the app returns since: so the summaries decide, and the app's code only when
		there are none to read, as with --list."""
		if status is None:
			if launched != 0:
				return 1, "devicectl could not launch the app: is the iPhone unlocked, in Developer Mode?"
			return 1, "the app ended without an exit code: it crashed, or was ended"
		if "--list" in arguments or "--help" in arguments:
			return status, None
		if not summaries:
			return 1, "the tests ended without a summary: they crashed, or panicked"
		return status or (0 if all(summaries) else 101), None

	def run_app(self, executable, arguments, app):
		"""Makes `app` of a test executable, installs it, runs it, and returns its exit code."""
		bundle_id = self.bundle(executable, app)
		variables = self.environment()
		installed = subprocess.run(
			["xcrun", "devicectl", "device", "install", "app", "--quiet", "--device", self.device, str(app)],
			env=variables, capture_output=True, text=True, check=False,
		)
		if installed.returncode != 0:
			print(installed.stdout + installed.stderr, file=sys.stderr)
			return 1
		# devicectl reports the app's exit code in a line of its own (see verdict). iOS ends
		# an app that has not finished launching after 20 seconds of wall clock time, which a
		# test executable, the whole app, never does; but not under a debugger attached from
		# the start. So the app starts suspended, and LLDB attaches, lets it run, and stays
		# until it ends.
		name = pathlib.Path(executable).name
		earlier = self.processes(name)
		process = subprocess.Popen(
			["xcrun", "devicectl", "device", "process", "launch", "--console", "--start-stopped",
				"--terminate-existing", "--device", self.device, bundle_id, *arguments],
			env=variables, stdout=subprocess.PIPE, text=True,
		)
		log = tempfile.TemporaryFile(mode="w+")
		debugger = self.attach(name, process, earlier, log, variables)
		if debugger is None:
			# The app may wait on the iPhone; the next launch ends it.
			process.kill()
			process.wait()
			print("the test app did not start on the iPhone: is it unlocked, in Developer Mode?", file=sys.stderr)
			return 1
		# LLDB ends with the app. One that ends while the app runs on failed to attach, or to
		# let it go, and the app would wait for ever: so the launch is ended.
		abandoned = []

		def watch():
			debugger.wait()
			try:
				process.wait(timeout=30)
			except subprocess.TimeoutExpired:
				abandoned.append(True)
				process.kill()

		threading.Thread(target=watch, daemon=True).start()
		status = None
		summaries = []
		for line in process.stdout:
			found = re.match(r"The app terminated with the exit code (-?\d+)", line)
			if found:
				status = int(found.group(1))
				continue
			if line.startswith("test result: "):
				summaries.append(line.startswith("test result: ok."))
			if not line.startswith(("Launched application with", "Waiting for the application to terminate")):
				print(line, end="", flush=True)
		launched = process.wait()
		try:
			debugger.wait(timeout=60)
		except subprocess.TimeoutExpired:
			debugger.kill()
		log.seek(0)
		said = log.read()
		for line in said.splitlines():
			if line.startswith("the test app stopped"):
				print(line, file=sys.stderr)
		if abandoned or status is None:
			print(f"what LLDB said:\n{said}", file=sys.stderr)
		if abandoned:
			print("LLDB ended before the test app, which was ended", file=sys.stderr)
			return 1
		code, verdict = self.verdict(launched, status, summaries, arguments)
		if verdict:
			print(verdict, file=sys.stderr)
		return code


class Ssh(Remote):
	"""A 64-bit ARM Linux computer, reached with SSH, over one connection for the run."""

	TARGET = devices.TARGET

	def __init__(self, host, directory=None):
		self.host = host
		self.directory = directory
		# One connection for every command of the run: logging in costs a small device a
		# second or so, and a run makes several commands for every test executable.
		control = pathlib.Path.home() / ".ssh" / "nibkeep-test-%C"
		self.options = [
			"-o", "BatchMode=yes",
			"-o", "ControlMaster=auto",
			"-o", f"ControlPath={control}",
			"-o", "ControlPersist=60",
		]

	def spec(self):
		return ["ssh", self.host, self.directory]

	def default_crates(self):
		return ["--workspace"]

	def output(self, command):
		"""What a command prints on the device, or exits with what went wrong."""
		return devices.output(self.host, command, ["ssh", *self.options])

	def base(self):
		"""The directory under which runs work."""
		home = self.output("echo $HOME")
		if not home.startswith("/"):
			sys.exit(f"{self.host} has no home directory to work in")
		return f"{home}/nibkeep-tests"

	def cargo(self):
		"""The command that runs cargo test for the device, linked against its C library."""
		variables = devices.build_environment(self.host, ["ssh", *self.options])
		return ["cargo", "test", "--target", self.TARGET], self.TARGET, variables

	def shell(self, command):
		"""Runs a command on the device. Its input stays open while it runs, and when the
		run is interrupted and the connection goes, the device ends the command."""
		return self.watched(command, capture=False)[0]

	def capture(self, command):
		"""Runs a command on the device as shell() does, and returns its status and what
		it printed."""
		return self.watched(command, capture=True)

	def watched(self, command, capture):
		"""Runs a command on the device, ended there if the connection goes, and returns its
		status and, if captured, what it printed."""
		# Without job control, a command in the background reads from /dev/null, so the
		# watcher reads the connection through a copy of it. The command runs in a group
		# of its own, so that ending it ends what it started.
		watched = (
			"exec 3<&0; "
			f"setsid sh -c {quote(command)} < /dev/null & child=$!; "
			"( cat <&3; kill -TERM -- -$child ) > /dev/null 2>&1 & watcher=$!; "
			"wait $child; status=$?; kill $watcher 2> /dev/null; exit $status"
		)
		process = subprocess.Popen(
			["ssh", *self.options, self.host, watched],
			stdin=subprocess.PIPE,
			stdout=subprocess.PIPE if capture else None,
			text=True,
		)
		try:
			if capture:
				# Only stdout is read; input stays open until the command ends.
				output = process.stdout.read()
				return process.wait(), output
			return process.wait(), None
		except KeyboardInterrupt:
			process.kill()
			raise

	def push(self, local, remote):
		return subprocess.run(
			["scp", "-q", "-r", *self.options, str(local), f"{self.host}:{remote}"],
			check=False,
		).returncode

	def close(self):
		subprocess.run(
			["ssh", *self.options, "-O", "exit", self.host],
			capture_output=True,
			check=False,
		)


def device(spec):
	"""The device that arguments name, and the arguments after them."""
	if spec[:1] == ["android"]:
		return Android(), spec[1:]
	if spec[:1] == ["ios"]:
		return Ios(), spec[1:]
	if spec[:1] == ["ssh"] and len(spec) > 1:
		return Ssh(spec[1]), spec[2:]
	sys.exit(__doc__)


def runner_device(spec):
	"""The device that the runner's arguments name, with the run's directory on it, and the
	arguments after them."""
	if spec[:1] == ["android"] and len(spec) > 1:
		return Android(spec[1]), spec[2:]
	if spec[:1] == ["ios"] and len(spec) > 2:
		return Ios(spec[1], spec[2]), spec[3:]
	if spec[:1] == ["ssh"] and len(spec) > 2:
		return Ssh(spec[1], spec[2]), spec[3:]
	sys.exit("the runner was called without its device")


def test(target_device, arguments):
	"""Runs cargo test for the device, with this script as the runner."""
	options = arguments[: arguments.index("--")] if "--" in arguments else arguments
	selects = any(
		option in ("-p", "--package", "--workspace") or option.startswith("--package=")
		for option in options
	)
	# A directory of its own for the run, so that runs on one device keep apart.
	target_device.directory = f"{target_device.base()}/run-{os.getpid()}-{int(time.time())}"
	command, triple, variables = target_device.cargo()
	runner = json.dumps(
		[sys.executable, str(pathlib.Path(__file__).resolve()), ON_DEVICE, *target_device.spec()]
	)
	command += ["--locked", "--config", f"target.{triple}.runner = {runner}"]
	command += arguments if selects else target_device.default_crates() + arguments
	try:
		if target_device.prepare() != 0:
			return 1
		return subprocess.run(command, cwd=ROOT, env=variables, check=False).returncode
	finally:
		target_device.finish()


def main():
	arguments = sys.argv[1:]
	if arguments[:1] == [ON_DEVICE]:
		target_device, rest = runner_device(arguments[1:])
		if not rest:
			sys.exit("no test executable to run")
		sys.exit(target_device.run(rest[0], rest[1:]))
	target_device, rest = device(arguments)
	sys.exit(test(target_device, rest))


if __name__ == "__main__":
	main()
