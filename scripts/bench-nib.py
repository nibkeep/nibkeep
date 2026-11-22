"""Benchmarks of everyday `nib` commands (docs/Plan.md, section 4): each command runs as a
process of its own, which starts, unlocks the keys, opens the spaces and builds their
indexes, does its work and prints, as a script would run it.

    python3 scripts/bench-nib.py [ROUNDS ...] [--runs N]
    python3 scripts/bench-nib.py --library FOLDER (ROUNDS | --real) [--runs N]

For each number of rounds, 1,667 and 16,667 by default, which make 10,007 and 100,007
entities, it makes a library with `nibkeep-bench --generate`, on a disk, as
scripts/libraries.py says, in the folder `.nib` of a library's folder, as nib keeps one,
runs each command `--runs` times, 10 by default, and prints a line of JSON per command:
the medians of the time the process took from start to end, of that time without
unlocking, and of the parts that `nib --timings` reports. The start is what the process
took beyond those parts: loading the program and reading its arguments. With `--library`,
it measures the commands alone on a library made already, without `nib mcp`: of generated
data with that number of rounds, or of real data with `--real`, as scripts/bench-record.py
does, where the commands are `nib find` and `nib edit`.

The libraries are unlocked with a passphrase, which the script types into a terminal of
the process's own, as a person would: every machine then unlocks the same way. On Windows,
the terminal is a pseudoconsole. They use the generator's quick passphrase parameters, so
unlocking costs less than in a library of one's own; it is reported apart, and the budgets
leave it out.

Then it measures the same work through `nib mcp`, as an AI agent does it: it pairs an AI
agent's copy of the library on the same computer, starts the server once on it, and
reports the medians of each tool call, from the request to the answer, and how long a
burst of a hundred new entities takes, as an AI agent importing a hundred films would.
The server opens the library once, so a call costs the work alone. Pairing reaches the
library over this computer's network, as `nib device pair` and `nib init --join` do. With
`--library`, these are left out: each run would pair one more copy into that library.

Runs on Linux, macOS and Windows. Builds both programs with cargo, optimized, first. The
settings of nib in the environment, such as NIBKEEP_LIBRARY and NIBKEEP_PASSPHRASE_FILE,
are a person's own, for their own library, and are left out.
"""

import argparse
import importlib.util
import json
import os
import pathlib
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

if sys.platform == "win32":
	import ctypes
	import msvcrt
	import threading
	from ctypes import wintypes
else:
	import fcntl
	import select
	import termios

ROOT = pathlib.Path(__file__).resolve().parent.parent
# What the names of programs end with.
EXE = ".exe" if sys.platform == "win32" else ""

_libraries = importlib.util.spec_from_file_location("libraries", ROOT / "scripts" / "libraries.py")
libraries = importlib.util.module_from_spec(_libraries)
_libraries.loader.exec_module(libraries)

# The generator's passphrase, which protects no one's data.
PASSPHRASE = "correct horse battery staple"
# How long a command may take before the run gives up on it, in seconds.
TIMEOUT = 600


def commands(rounds, run):
	"""The commands measured, as nib's arguments, for the `run`th run: each edit and each
	new entity is a different one, so that no run repeats what another did."""
	round = rounds // 2
	return {
		"show": ["show", f"Lavinia {round}"],
		"find": ["find", "leviathan"],
		"new": ["new", "book", f"title=Bench {run}"],
		"edit": ["edit", f"A Wizard of Earthsea {round}", f"shelf=attic {run}"],
	}


def real_commands(run):
	"""The commands measured on real data, as `commands` gives them: a search that finds
	many, and an edit of one person, each run a different one."""
	return {
		"find": ["find", "potter"],
		"edit": ["edit", "Daniel Radcliffe", f"birth-name=Bench {run}"],
	}


def forget_own_settings(environment):
	"""Removes from the environment the settings of nib that a person keeps for their own
	library, such as its folder and the file of its passphrase, which would reach the
	benchmark's nib."""
	for name in [name for name in environment if name.startswith("NIBKEEP_")]:
		del environment[name]


def build():
	"""Builds nib and nibkeep-bench, optimized, and returns their paths."""
	subprocess.run(
		["cargo", "build", "--release", "--quiet", "-p", "nibkeep-cli", "-p", "nibkeep-generate"],
		cwd=ROOT,
		check=True,
	)
	release = ROOT / "target" / "release"
	return release / f"nib{EXE}", release / f"nibkeep-bench{EXE}"


def nib(program, library, arguments):
	"""Runs nib with a terminal of its own, types the passphrase when it asks, and returns
	the seconds the process took and the timings it reported."""
	command = [program, "--library", library, "--json", "--timings", *arguments]
	run = nib_windows if sys.platform == "win32" else nib_posix
	took, status, errors = run([str(each) for each in command])
	if status != 0:
		sys.exit(f"nib {' '.join(arguments)} failed: {errors.decode(errors='replace')}")
	# With --json, the last line of standard error is the timings, as JSON.
	report = errors.decode().strip().splitlines()[-1]
	return took, json.loads(report)


def nib_posix(command):
	"""Runs a command with a pseudoterminal of its own, types the passphrase there when it
	asks, and returns the seconds it took, its status and what it wrote to standard error."""
	primary, secondary = os.openpty()

	def take_terminal():
		# The new session's controlling terminal, which nib asks for the passphrase on.
		fcntl.ioctl(secondary, termios.TIOCSCTTY, 0)

	started = time.perf_counter()
	process = subprocess.Popen(
		command,
		stdin=subprocess.DEVNULL,
		stdout=subprocess.DEVNULL,
		stderr=subprocess.PIPE,
		start_new_session=True,
		pass_fds=[secondary],
		preexec_fn=take_terminal,
	)
	os.close(secondary)
	try:
		if asks_for_passphrase(primary):
			os.write(primary, PASSPHRASE.encode() + b"\n")
		_, errors = process.communicate(timeout=TIMEOUT)
	except BaseException:
		# A command that hangs, or a run that is interrupted, leaves no nib behind.
		process.kill()
		process.wait()
		raise
	finally:
		os.close(primary)
	return time.perf_counter() - started, process.returncode, errors


def asks_for_passphrase(terminal):
	"""Reads what nib writes to its terminal until it asks for the passphrase, and tells
	whether it did: it may end first, as when it fails."""
	asked = b""
	while b"Passphrase:" not in asked:
		ready, _, _ = select.select([terminal], [], [], TIMEOUT)
		if not ready:
			raise TimeoutError("nib neither asked for the passphrase nor ended")
		try:
			read = os.read(terminal, 1024)
		except OSError:
			# Linux: the terminal is closed, because nib ended.
			return False
		if not read:
			# macOS: the same.
			return False
		asked += read
	return True


if sys.platform == "win32":
	EXTENDED_STARTUPINFO_PRESENT = 0x00080000
	STARTF_USESTDHANDLES = 0x00000100
	PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = 0x00020016
	WAIT_OBJECT_0 = 0

	class COORD(ctypes.Structure):
		_fields_ = [("X", wintypes.SHORT), ("Y", wintypes.SHORT)]

	class STARTUPINFOW(ctypes.Structure):
		_fields_ = [
			("cb", wintypes.DWORD),
			("lpReserved", wintypes.LPWSTR),
			("lpDesktop", wintypes.LPWSTR),
			("lpTitle", wintypes.LPWSTR),
			("dwX", wintypes.DWORD),
			("dwY", wintypes.DWORD),
			("dwXSize", wintypes.DWORD),
			("dwYSize", wintypes.DWORD),
			("dwXCountChars", wintypes.DWORD),
			("dwYCountChars", wintypes.DWORD),
			("dwFillAttribute", wintypes.DWORD),
			("dwFlags", wintypes.DWORD),
			("wShowWindow", wintypes.WORD),
			("cbReserved2", wintypes.WORD),
			("lpReserved2", ctypes.c_void_p),
			("hStdInput", wintypes.HANDLE),
			("hStdOutput", wintypes.HANDLE),
			("hStdError", wintypes.HANDLE),
		]

	class STARTUPINFOEXW(ctypes.Structure):
		_fields_ = [("StartupInfo", STARTUPINFOW), ("lpAttributeList", ctypes.c_void_p)]

	class PROCESS_INFORMATION(ctypes.Structure):
		_fields_ = [
			("hProcess", wintypes.HANDLE),
			("hThread", wintypes.HANDLE),
			("dwProcessId", wintypes.DWORD),
			("dwThreadId", wintypes.DWORD),
		]

	kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
	kernel32.CreatePseudoConsole.argtypes = [
		COORD, wintypes.HANDLE, wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p),
	]
	kernel32.CreatePseudoConsole.restype = ctypes.c_long
	kernel32.ClosePseudoConsole.argtypes = [ctypes.c_void_p]
	kernel32.ClosePseudoConsole.restype = None
	kernel32.InitializeProcThreadAttributeList.argtypes = [
		ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(ctypes.c_size_t),
	]
	kernel32.UpdateProcThreadAttribute.argtypes = [
		ctypes.c_void_p, wintypes.DWORD, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t,
		ctypes.c_void_p, ctypes.c_void_p,
	]
	kernel32.DeleteProcThreadAttributeList.argtypes = [ctypes.c_void_p]
	kernel32.DeleteProcThreadAttributeList.restype = None
	kernel32.CreateProcessW.argtypes = [
		wintypes.LPCWSTR, wintypes.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, wintypes.BOOL,
		wintypes.DWORD, ctypes.c_void_p, wintypes.LPCWSTR, ctypes.POINTER(STARTUPINFOEXW),
		ctypes.POINTER(PROCESS_INFORMATION),
	]
	kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
	kernel32.WaitForSingleObject.restype = wintypes.DWORD
	kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
	kernel32.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
	kernel32.CloseHandle.argtypes = [wintypes.HANDLE]


def check(succeeded, what):
	"""Raises the last Windows error unless the call succeeded."""
	if not succeeded:
		raise ctypes.WinError(ctypes.get_last_error(), f"{what} failed")


def nib_windows(command):
	"""Runs a command with a pseudoconsole of its own, types the passphrase there when it
	asks, and returns the seconds it took, its status and what it wrote to standard error.
	nib asks on the console itself, so its standard output and error stay pipes apart."""
	# The console reads what is typed from one pipe and writes what it shows to another.
	console_reads, typing = os.pipe()
	showing, console_writes = os.pipe()
	errors_read, errors_written = os.pipe()
	nothing = os.open(os.devnull, os.O_RDWR)
	console = ctypes.c_void_p()
	attributes = None
	process = None
	readers = []
	try:
		check(
			kernel32.CreatePseudoConsole(
				COORD(80, 25), msvcrt.get_osfhandle(console_reads),
				msvcrt.get_osfhandle(console_writes), 0, ctypes.byref(console),
			) == 0,
			"CreatePseudoConsole",
		)
		# The console holds its own ends of the pipes now.
		os.close(console_reads)
		os.close(console_writes)
		console_reads = console_writes = None

		size = ctypes.c_size_t()
		kernel32.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
		attributes = ctypes.create_string_buffer(size.value)
		check(
			kernel32.InitializeProcThreadAttributeList(attributes, 1, 0, ctypes.byref(size)),
			"InitializeProcThreadAttributeList",
		)
		check(
			kernel32.UpdateProcThreadAttribute(
				attributes, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, console,
				ctypes.sizeof(console), None, None,
			),
			"UpdateProcThreadAttribute",
		)
		startup = STARTUPINFOEXW()
		startup.StartupInfo.cb = ctypes.sizeof(startup)
		startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES
		startup.StartupInfo.hStdInput = msvcrt.get_osfhandle(nothing)
		startup.StartupInfo.hStdOutput = msvcrt.get_osfhandle(nothing)
		startup.StartupInfo.hStdError = msvcrt.get_osfhandle(errors_written)
		startup.lpAttributeList = ctypes.addressof(attributes)
		os.set_handle_inheritable(startup.StartupInfo.hStdInput, True)
		os.set_handle_inheritable(startup.StartupInfo.hStdError, True)

		# What the console shows must be read, or it stops; so must standard error.
		shown = bytearray()
		errors = bytearray()
		asked = threading.Event()

		def read(descriptor, into, until=None):
			while True:
				try:
					read = os.read(descriptor, 4096)
				except OSError:
					return
				if not read:
					return
				into += read
				if until and b"Passphrase:" in into:
					until.set()

		readers = [
			threading.Thread(target=read, args=(showing, shown, asked), daemon=True),
			threading.Thread(target=read, args=(errors_read, errors), daemon=True),
		]
		for reader in readers:
			reader.start()

		information = PROCESS_INFORMATION()
		line = ctypes.create_unicode_buffer(subprocess.list2cmdline(command))
		started = time.perf_counter()
		check(
			kernel32.CreateProcessW(
				None, line, None, None, True, EXTENDED_STARTUPINFO_PRESENT, None, None,
				ctypes.byref(startup), ctypes.byref(information),
			),
			"CreateProcessW",
		)
		process = information.hProcess
		kernel32.CloseHandle(information.hThread)
		# Only nib holds the write end of standard error now, so it ends when nib does.
		os.close(errors_written)
		errors_written = None

		# nib may end without asking, as when it fails.
		while not asked.wait(0.001):
			if kernel32.WaitForSingleObject(process, 0) == WAIT_OBJECT_0:
				break
			if time.perf_counter() - started > TIMEOUT:
				raise TimeoutError("nib neither asked for the passphrase nor ended")
		if asked.is_set():
			os.write(typing, PASSPHRASE.encode() + b"\r")
		if kernel32.WaitForSingleObject(process, TIMEOUT * 1000) != WAIT_OBJECT_0:
			raise TimeoutError(f"nib took longer than {TIMEOUT} seconds")
		took = time.perf_counter() - started
		status = wintypes.DWORD()
		check(kernel32.GetExitCodeProcess(process, ctypes.byref(status)), "GetExitCodeProcess")
		readers[1].join()
		return took, status.value, bytes(errors)
	except BaseException:
		# A command that hangs, or a run that is interrupted, leaves no nib behind.
		if process:
			kernel32.TerminateProcess(process, 1)
			kernel32.WaitForSingleObject(process, TIMEOUT * 1000)
		raise
	finally:
		if process:
			kernel32.CloseHandle(process)
		# Closing the console closes the pipe it shows on, which ends its reader.
		if console:
			kernel32.ClosePseudoConsole(console)
		if attributes:
			kernel32.DeleteProcThreadAttributeList(attributes)
		for reader in readers:
			reader.join()
		for descriptor in (console_reads, console_writes, typing, showing, errors_read, errors_written, nothing):
			if descriptor is not None:
				os.close(descriptor)


def totals(took, timings):
	"""The parts that nib reported, with the whole time the process took, what it took
	beyond those parts, and the whole time without unlocking."""
	timings = dict(timings)
	parts = sum(timings.values())
	# The time spent typing the passphrase is the script's, not nib's.
	timings["total_seconds"] = took - timings.pop("waiting_seconds", 0.0)
	timings["start_seconds"] = max(0.0, took - parts)
	timings["total_without_unlock_seconds"] = timings["total_seconds"] - timings.get("unlock_seconds", 0.0)
	return timings


def measure(program, library, rounds, runs):
	"""Runs each command `runs` times, on generated data of `rounds` rounds or, if `rounds`
	is None, on real data, and returns a result for each."""
	chosen = (lambda run: commands(rounds, run)) if rounds else real_commands
	results = []
	for name in chosen(0):
		samples = []
		for run in range(runs):
			took, timings = nib(program, library, chosen(run)[name])
			samples.append(totals(took, timings))
		result = {"command": name, "runs": runs} if rounds is None else {"command": name, "rounds": rounds, "runs": runs}
		for key in samples[0]:
			result[f"{key.removesuffix('_seconds')}_median_seconds"] = round(
				statistics.median(sample[key] for sample in samples), 6
			)
		results.append(result)
	return results


def tool_calls(rounds, run):
	"""The tool calls measured, as `commands` gives the commands: each with its tool and its
	arguments."""
	round = rounds // 2
	return {
		"show": ("show", {"entity": f"Lavinia {round}"}),
		"find": ("find", {"words": "leviathan"}),
		"new": ("new", {"type": "book", "values": [f"title=Bench MCP {run}"]}),
		"edit": (
			"edit",
			{"entity": f"A Wizard of Earthsea {round}", "changes": [f"shelf=cellar {run}"]},
		),
	}


# How many new entities the burst adds, as an AI agent importing a hundred films.
BURST = 100


def passphrase_file(directory):
	"""A file that holds the generator's passphrase, which its owner alone can read."""
	file = directory / "passphrase"
	file.write_text(PASSPHRASE + "\n")
	if sys.platform != "win32":
		file.chmod(0o600)
	return file


def read_json(stream):
	"""Reads lines from a stream until they make one JSON value, and returns it."""
	text = ""
	while True:
		line = stream.readline()
		if not line:
			sys.exit(f"the stream ended before a JSON value: {text}")
		text += line
		try:
			return json.loads(text)
		except json.JSONDecodeError:
			continue


def pair_ai_agent(program, library, agent, passphrase):
	"""Pairs an AI agent's copy of `library` in `agent`, over this computer's network."""
	keys = ["--passphrase-file", passphrase]
	pairing = subprocess.Popen(
		[program, "--library", library, *keys, "--json", "device", "pair", "--ai-agent", "Bench", "--port", "0"],
		stdout=subprocess.PIPE,
		text=True,
	)
	try:
		code = read_json(pairing.stdout)["code"]
		agent.mkdir()
		subprocess.run(
			[program, "--library", agent, *keys, "init", "--passphrase", "--join", code],
			check=True,
			stdout=subprocess.DEVNULL,
			timeout=TIMEOUT,
		)
		check(pairing.wait(timeout=TIMEOUT) == 0, "pairing the AI agent")
	finally:
		if pairing.poll() is None:
			pairing.kill()


class Server:
	"""`nib mcp serve` on an AI agent's copy, with a client of the protocol's current revision."""

	def __init__(self, program, agent, passphrase):
		self.process = subprocess.Popen(
			[program, "--library", agent, "--passphrase-file", passphrase, "mcp", "serve", "--wait", "0"],
			stdin=subprocess.PIPE,
			stdout=subprocess.PIPE,
			text=True,
		)
		self.next = 0

	def request(self, method, params):
		"""Sends a request, and returns its result and the seconds the answer took."""
		self.next += 1
		params = dict(params)
		params["_meta"] = {
			"io.modelcontextprotocol/protocolVersion": "2026-07-28",
			"io.modelcontextprotocol/clientCapabilities": {},
		}
		message = {"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}
		started = time.perf_counter()
		self.process.stdin.write(json.dumps(message) + "\n")
		self.process.stdin.flush()
		answer = json.loads(self.process.stdout.readline())
		took = time.perf_counter() - started
		check("result" in answer, f"{method}: {answer}")
		return answer["result"], took

	def call(self, tool, arguments):
		"""Calls a tool, and returns the seconds it took; a failed call ends the run."""
		result, took = self.request("tools/call", {"name": tool, "arguments": arguments})
		check(not result["isError"], f"{tool}: {result['content'][0]['text']}")
		return took

	def close(self):
		self.process.stdin.close()
		self.process.wait(timeout=TIMEOUT)


def measure_mcp(program, library, rounds, runs):
	"""Measures each tool call `runs` times through `nib mcp`, and a burst of new entities,
	on an AI agent's copy of `library`, and returns a result for each."""
	directory = pathlib.Path(tempfile.mkdtemp(prefix="nibkeep-bench-mcp-"))
	try:
		passphrase = passphrase_file(directory)
		agent = directory / "agent"
		pair_ai_agent(program, library, agent, passphrase)
		server = Server(program, agent, passphrase)
		try:
			# The first call also looks for the user's other devices, once.
			server.request("server/discover", {})
			server.call("status", {})
			results = []
			for name in tool_calls(rounds, 0):
				samples = [server.call(*tool_calls(rounds, run)[name]) for run in range(runs)]
				results.append({
					"command": f"mcp {name}",
					"rounds": rounds,
					"runs": runs,
					"total_median_seconds": round(statistics.median(samples), 6),
				})
			started = time.perf_counter()
			for film in range(BURST):
				server.call("new", {"type": "book", "values": [f"title=Bench film {film}"]})
			results.append({
				"command": f"mcp new, {BURST} at once",
				"rounds": rounds,
				"runs": 1,
				"total_seconds": round(time.perf_counter() - started, 6),
			})
			return results
		finally:
			server.close()
	finally:
		shutil.rmtree(directory)


def main():
	parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
	parser.add_argument("rounds", nargs="*", type=int, help="1,667 and 16,667 by default")
	parser.add_argument("--runs", type=int, default=10)
	parser.add_argument(
		"--library",
		type=pathlib.Path,
		help="the folder of a library that nibkeep-bench made already in its .nib, of the one "
		"number of rounds given, or of real data; the commands change it",
	)
	parser.add_argument("--real", action="store_true", help="the library holds real data")
	arguments = parser.parse_args()
	if arguments.real and not arguments.library:
		sys.exit("--real measures a library of real data, which --library names")
	# Every nib started from here, on Windows too, takes this script's environment.
	forget_own_settings(os.environ)
	program, generator = build()
	if arguments.library:
		if arguments.real == bool(arguments.rounds) or len(arguments.rounds or []) > 1:
			sys.exit("--library goes with the one number of rounds it was made with, or with --real")
		rounds = None if arguments.real else arguments.rounds[0]
		for result in measure(program, arguments.library, rounds, arguments.runs):
			print(json.dumps(result), flush=True)
		return
	for rounds in arguments.rounds or [1667, 16667]:
		directory = libraries.new_directory("nibkeep-bench-nib-")
		try:
			library = directory / "library"
			# What a library keeps is in its folder's .nib, where nib looks for it.
			subprocess.run([generator, "--generate", str(rounds), library / ".nib"], check=True)
			for result in measure(program, library, rounds, arguments.runs):
				print(json.dumps(result), flush=True)
			for result in measure_mcp(program, library, rounds, arguments.runs):
				print(json.dumps(result), flush=True)
		finally:
			shutil.rmtree(directory)


if __name__ == "__main__":
	main()
