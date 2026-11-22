"""Draws the benchmark results of a branch of the results repository as charts, in one
HTML page.

    python3 scripts/bench-chart.py [--branch NAME] [--results FOLDER] [--output FILE] [--open]

It reads every `results/LABEL.jsonl` of the branch as Git has it, without checking it out,
in the results repository that scripts/bench-record.py records in: by default the branch
named after the one checked out here, which holds main's results from before it started
too, or else main. A local branch is read rather than origin's, with a word when origin has
commits it lacks; `--branch origin/NAME` reads origin's. The machines' descriptions,
`machines/LABEL.toml`, are read from main.

The page shows the machines, each with its description and the latest of its own measures:
one core's speed, and the disk's. Then a chart for each number that bench-record.py keeps,
with a line for each machine, and every run as a table. Along the horizontal axis are the
commits measured, in the order they were first measured, so that a commit measured on
several machines is one place on the axis. A machine measured more than once at a commit
shows its latest run, and the `nib` commands of a run join its point.

A new data revision or a new recipe of the importer starts a series, which is never
compared with the one before: a line starts afresh, after a dashed mark that names what
changed. A commit measured on both revisions, a bridge, shows on both lines, so that the
step between them is what the data alone changed. A triangle marks a run with another
version of Rust than the run before it. Since the machines differ by tens of times, a chart
whose numbers span more than eight times has a logarithmic scale, and the page can show
each line instead as its change since its first run.

The page loads Chart.js from cdnjs, and so needs the network to show its charts; its
tables need nothing. It is written to target/bench-chart.html unless `--output` says
otherwise.
"""

import argparse
import html
import importlib.util
import json
import pathlib
import subprocess
import sys
import tomllib
import webbrowser

ROOT = pathlib.Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("bench_record", ROOT / "scripts" / "bench-record.py")
record = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(record)

OUTPUT = ROOT / "target" / "bench-chart.html"
# Commits are written with 12 hexadecimal digits by bench-record.py and 7 by hand: the first
# 7 name one.
SHORT = 7
# The numbers in the order the page shows them, with their titles and units. The nib
# commands follow, each without unlocking.
MEASURES = {
	"open_seconds": ("Open", "s"),
	"search_first_p95_ms": ("Search, first 50, 95th percentile", "ms"),
	"entities_written_per_second": ("Import rate", "/s"),
	"changes_received_per_second": ("First sync, receive rate", "/s"),
	"peak_memory_bytes": ("Peak memory", "bytes"),
	"disk_bytes_per_entity": ("Disk per entity", "bytes"),
}


def git(results, *arguments):
	done = subprocess.run(["git", *arguments], cwd=results, capture_output=True, text=True, encoding="utf-8")
	return done.stdout if done.returncode == 0 else None


def exists(results, ref):
	return git(results, "rev-parse", "--verify", "--quiet", ref) is not None


def default_branch(results):
	"""The branch named after the one checked out here, if the results have it, or main."""
	name = subprocess.run(
		["git", "symbolic-ref", "--quiet", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True,
	).stdout.strip()
	if name and any(exists(results, f"{prefix}{name}") for prefix in ("refs/heads/", "refs/remotes/origin/")):
		return name
	return "main"


def resolve(results, branch):
	"""What to read of a branch: the local one, or origin's without it. Warns when origin's
	has runs that the local one lacks."""
	local, remote = f"refs/heads/{branch}", f"refs/remotes/origin/{branch}"
	if exists(results, local):
		missing = git(results, "rev-list", "--count", f"{local}..{remote}") if exists(results, remote) else None
		if missing and int(missing) > 0:
			print(
				f"origin/{branch} has {int(missing)} commits that {branch} lacks; they are left out. "
				f"bench-record.py brings {branch} up to date, or --branch origin/{branch} reads the other.",
				file=sys.stderr,
			)
		return local
	if exists(results, remote):
		return remote
	if exists(results, branch):
		return branch
	sys.exit(f"the results repository has no branch {branch}, here or on origin")


def read(results, ref):
	"""Every result on the branch, each with its machine, named by its file."""
	names = git(results, "ls-tree", "--name-only", ref, "results/")
	if names is None:
		sys.exit(f"cannot read {ref}")
	found = []
	for name in names.splitlines():
		if not name.endswith(".jsonl"):
			continue
		machine = pathlib.PurePosixPath(name).stem
		text = git(results, "show", f"{ref}:{name}")
		if text is None:
			sys.exit(f"cannot read {name} on {ref}")
		for line in text.splitlines():
			if line.strip():
				found.append({**json.loads(line), "machine": machine})
	return found


def description(text):
	"""The texts of a machine's description, in TOML; nothing if it cannot be read."""
	try:
		described = tomllib.loads(text)
	except tomllib.TOMLDecodeError:
		return {}
	return {key: value for key, value in described.items() if isinstance(value, str)}


def descriptions(results):
	"""Each machine's description on main, by its label: origin's, which holds those pushed
	from other machines, and then this computer's, which may hold one not pushed yet."""
	local, remote = "refs/heads/main", "refs/remotes/origin/main"
	refs = [ref for ref in (remote, local) if exists(results, ref)]
	if refs == [remote, local] and (git(results, "rev-list", "--count", f"{remote}..{local}") or "").strip() == "0":
		# This computer's main holds nothing that origin's lacks.
		refs = [remote]
	described = {}
	for ref in refs:
		for name in (git(results, "ls-tree", "--name-only", ref, "machines/") or "").splitlines():
			if name.endswith(".toml"):
				described[pathlib.PurePosixPath(name).stem] = description(git(results, "show", f"{ref}:{name}") or "")
	return described


def machines(results, described):
	"""The machines measured, in a fixed order, so that each keeps its colour, with their
	descriptions and the latest of their own measures."""
	listed = []
	for label in sorted({result["machine"] for result in results}):
		runs = sorted((result for result in results if result["machine"] == label), key=lambda result: result["at"])
		calibrated = [result["calibration"] for result in runs if result.get("calibration")]
		listed.append({
			**described.get(label, {}), "label": label, "last": runs[-1]["at"],
			"calibration": calibrated[-1] if calibrated else {},
		})
	return listed


def chart_key(result, key):
	"""What a number of a result is charted as: a nib command's under the command's name."""
	return key if result["kind"] == "api" else f"{result['kind']} {result.get('command')}: {key}"


def values(result):
	"""The numbers a result measured, by what they are charted as."""
	return {
		chart_key(result, key): value for key, value in result.items()
		if key not in record.DESCRIPTIVE and isinstance(value, (int, float)) and not isinstance(value, bool)
	}


def series(result):
	return f"data {result.get('data')}, recipe {result.get('recipe')}"


def measure(key, order):
	"""A chart: its title, its unit, which way is better, and where it goes."""
	command, _, number = key.rpartition(": ")
	if command:
		title, unit = f"{command}, without unlocking", "s"
	else:
		title, unit = MEASURES.get(key, (key.replace("_", " "), ""))
	if not unit:
		suffixes = (("_seconds", "s"), ("_ms", "ms"), ("_bytes", "bytes"))
		unit = next((unit for suffix, unit in suffixes if key.endswith(suffix)), "")
	known = list(MEASURES)
	return {
		"key": key, "title": title, "unit": unit,
		"higher_is_better": number in record.MORE_IS_BETTER or number.endswith("_per_second"),
		"order": (known.index(key), 0) if key in known else (len(known), order),
	}


def change(before, after):
	"""What starts a new series, in words: a new data revision, a new recipe, or both."""
	changed = [f"{name} {after.get(name)}" for name in ("data", "recipe") if before.get(name) != after.get(name)]
	return ", ".join(changed) or "new series"


def build(results):
	"""The charts' data: the commits along the axis, a line for each machine and series, the
	marks where series start, the charts, and every run."""
	runs = sorted(results, key=lambda result: result["at"])
	slots, commits = {}, []
	for result in runs:
		short = result["commit"][:SHORT]
		if short not in slots:
			slots[short] = len(commits)
			commits.append({"commit": result["commit"], "at": result["at"]})
		elif len(result["commit"]) > len(commits[slots[short]]["commit"]):
			commits[slots[short]]["commit"] = result["commit"]

	# A point for each machine, series and commit, of its runs' numbers, the latest winning.
	points, firsts, keys = {}, {}, {}
	for result in runs:
		slot = slots[result["commit"][:SHORT]]
		firsts.setdefault(series(result), (slot, result))
		point = points.setdefault(
			(result["machine"], series(result), slot), {"slot": slot, "first": result["at"], "runs": 0, "values": {}},
		)
		numbers = values(result)
		for key in numbers:
			keys.setdefault(key, len(keys))
		point["values"].update(numbers)
		point.update(at=result["at"], rust=result.get("rust"), note=result.get("note"))
		if result["kind"] == "api":
			point["runs"] += 1

	# A line for each machine, which starts afresh with each new series; a bridge's commit
	# ends the old line and starts the new one.
	lines, current = [], {}
	for (machine, name, _), point in sorted(points.items(), key=lambda item: (item[1]["slot"], item[1]["first"])):
		line = current.get(machine)
		if line is None or line["series"] != name:
			line = current[machine] = {"machine": machine, "series": name, "points": []}
			lines.append(line)
		previous = line["points"][-1]["rust"] if line["points"] else None
		line["points"].append({
			"slot": point["slot"], "at": point["at"], "rust": point["rust"],
			"rust_from": previous if previous not in (None, point["rust"]) else None,
			"runs": max(point["runs"], 1), "note": point["note"], "values": point["values"],
		})

	ordered = sorted(firsts.items(), key=lambda item: (item[1][0], item[1][1]["at"]))
	starts = [
		{"slot": slot, "label": change(ordered[index - 1][1][1], first)}
		for index, (_, (slot, first)) in enumerate(ordered) if index > 0
	]
	measures = sorted((measure(key, order) for key, order in keys.items()), key=lambda each: each["order"])
	return {
		"commits": commits,
		"series": [name for name, _ in ordered],
		"starts": starts,
		"measures": [{key: value for key, value in each.items() if key != "order"} for each in measures],
		"lines": lines,
		"results": [
			{
				"at": result["at"], "machine": result["machine"], "branch": result.get("branch"),
				"commit": result["commit"], "rust": result.get("rust"), "series": series(result),
				"note": result.get("note"), "values": values(result),
			}
			for result in runs
		],
	}


def page(data):
	"""The page, with the data in it as JSON that no string in it can end early."""
	embedded = json.dumps(data, ensure_ascii=False).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
	return PAGE.replace("@TITLE@", html.escape(data.get("title", "Benchmarks"))).replace("@DATA@", embedded)


def main():
	parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
	parser.add_argument("--branch", help="the branch to read, named after the one checked out by default")
	parser.add_argument("--results", type=pathlib.Path, help=f"the results repository's folder, or {record.RESULTS_VARIABLE}")
	parser.add_argument("--output", type=pathlib.Path, default=OUTPUT)
	parser.add_argument("--open", action="store_true", help="open the page in a browser")
	arguments = parser.parse_args()
	results = record.results_directory(arguments.results)
	if not (results / ".git").exists():
		sys.exit(f"there is no results repository in {results}: scripts/bench-record.py starts one")
	branch = arguments.branch or default_branch(results)
	ref = resolve(results, branch)
	found = read(results, ref)
	if not found:
		sys.exit(f"{branch} holds no results")
	commit = git(results, "rev-parse", "--short=12", ref).strip()
	data = {
		"title": "NibKeep benchmarks",
		"source": f"{branch} at {commit}",
		"machines": machines(found, descriptions(results)),
		**build(found),
	}
	arguments.output.parent.mkdir(parents=True, exist_ok=True)
	arguments.output.write_text(page(data), encoding="utf-8")
	print(f"{len(found)} results of {len(data['machines'])} machines from {branch} at {commit}: {arguments.output}")
	if arguments.open:
		webbrowser.open(arguments.output.resolve().as_uri())


PAGE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>@TITLE@</title>
<script src="https://cdnjs.cloudflare.com/ajax/libs/Chart.js/4.4.1/chart.umd.min.js"></script>
<style>
:root {
	color-scheme: light;
	--surface: #fcfcfb;
	--card: #ffffff;
	--text: #0b0b0b;
	--text-secondary: #52514e;
	--muted: #898781;
	--grid: #e1e0d9;
	--border: #e1e0d9;
	--series-1: #2a78d6; --series-2: #eb6834; --series-3: #1baf7a; --series-4: #eda100;
	--series-5: #e87ba4; --series-6: #008300; --series-7: #6250d6; --series-8: #e34948;
	--other: #898781;
}
@media (prefers-color-scheme: dark) {
	:root:not([data-theme="light"]) {
		color-scheme: dark;
		--surface: #1a1a19; --card: #222220; --text: #f0efec; --text-secondary: #c3c2b7;
		--muted: #898781; --grid: #2c2c2a; --border: #353532;
		--series-1: #3987e5; --series-2: #d95926; --series-3: #199e70; --series-4: #c98500;
		--series-5: #d55181; --series-6: #008300; --series-7: #9085e9; --series-8: #e66767;
	}
}
:root[data-theme="dark"] {
	color-scheme: dark;
	--surface: #1a1a19; --card: #222220; --text: #f0efec; --text-secondary: #c3c2b7;
	--muted: #898781; --grid: #2c2c2a; --border: #353532;
	--series-1: #3987e5; --series-2: #d95926; --series-3: #199e70; --series-4: #c98500;
	--series-5: #d55181; --series-6: #008300; --series-7: #9085e9; --series-8: #e66767;
}
* { box-sizing: border-box; }
body {
	margin: 0; padding: 24px 16px 48px; background: var(--surface); color: var(--text);
	font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif;
}
main { max-width: 1280px; margin: 0 auto; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 17px; margin: 32px 0 4px; }
.secondary { color: var(--text-secondary); }
.controls { display: flex; flex-wrap: wrap; gap: 16px; align-items: center; margin: 16px 0 8px; }
.toggle { display: inline-flex; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
.toggle button {
	border: 0; background: transparent; color: var(--text-secondary); padding: 6px 12px; font: inherit; cursor: pointer;
}
.toggle button[aria-pressed="true"] { background: var(--text); color: var(--surface); }
.legend { display: flex; flex-wrap: wrap; gap: 14px; }
.legend span { display: inline-flex; align-items: center; gap: 6px; color: var(--text-secondary); }
.legend i, .swatch { display: inline-block; width: 16px; height: 2px; border-radius: 1px; vertical-align: middle; margin-right: 6px; }
.key { font-size: 12px; color: var(--muted); margin: 0 0 8px; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 380px), 1fr)); gap: 12px; margin-top: 12px; }
.card { background: var(--card); border: 1px solid var(--border); border-radius: 10px; padding: 12px 12px 8px; }
.card h3 { font-size: 13px; font-weight: 600; margin: 0; }
.card .better { font-size: 12px; color: var(--muted); margin: 0 0 6px; }
.card .plot { position: relative; height: 220px; }
details { margin-top: 12px; }
summary { cursor: pointer; color: var(--text-secondary); }
.table { overflow-x: auto; margin-top: 8px; }
table { border-collapse: collapse; font-size: 12px; font-variant-numeric: tabular-nums; }
th, td { padding: 4px 10px 4px 0; text-align: right; white-space: nowrap; border-bottom: 1px solid var(--grid); }
th { color: var(--text-secondary); font-weight: 600; }
.runs th:nth-child(-n+6), .runs td:nth-child(-n+6) { text-align: left; }
.machines th:nth-child(-n+6), .machines td:nth-child(-n+6) { text-align: left; }
.offline { display: none; color: var(--text-secondary); }
</style>
</head>
<body>
<main>
<h1 id="title"></h1>
<p class="secondary" id="source"></p>
<p class="offline" id="offline">Chart.js did not load, so only the tables show. The charts need the network.</p>
<h2>Machines</h2>
<p class="key">One core: how long a core takes for a fixed piece of work, sorting numbers and building a B-tree of them. Sync: how long writing a page and syncing it take. Large file: how fast the disk takes 64 MB, written and synced. Each from the machine's latest run.</p>
<div class="table machines" id="machines"></div>
<h2>Numbers</h2>
<p class="secondary" id="summary"></p>
<div class="controls">
	<div class="toggle" role="group" aria-label="What the charts show">
		<button type="button" data-mode="values" aria-pressed="true">Values</button>
		<button type="button" data-mode="change" aria-pressed="false">Change since first run</button>
	</div>
	<div class="legend" id="legend"></div>
</div>
<p class="key">The horizontal axis is the commits measured, in the order first measured. ▲ marks a run on a new version of Rust; a dashed line marks a new series, named by what changed: a new data revision, or a new recipe of the importer, whose numbers do not compare with the ones before. A commit measured on two data revisions shows on both lines. A chart whose numbers span more than eight times has a logarithmic scale.</p>
<div id="charts"></div>
<details id="runs">
<summary>Every run, as a table</summary>
</details>
</main>
<script type="application/json" id="data">@DATA@</script>
<script>
"use strict";
const data = JSON.parse(document.getElementById("data").textContent);
const labels = data.machines.map((machine) => machine.label);
// Room beside each chart for the machines' labels at the ends of their lines, as long as
// the longest, such as rog-zephyrus-g15-ga503qr-windows.
const labelRoom = (() => {
	const context = document.createElement("canvas").getContext("2d");
	context.font = "11px system-ui, sans-serif";
	return Math.ceil(Math.max(88, ...labels.map((label) => context.measureText(label).width)) + 12);
})();
const charts = [];
let mode = "values";

function css(name) {
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}
function colour(machine) {
	const slot = labels.indexOf(machine);
	return slot >= 0 && slot < 8 ? css(`--series-${slot + 1}`) : css("--other");
}
const number = new Intl.NumberFormat("en", { maximumSignificantDigits: 3 });
function format(value, unit) {
	if (value === null || value === undefined) return "";
	if (unit === "s") return value < 1 ? `${number.format(value * 1000)} ms` : `${number.format(value)} s`;
	if (unit === "ms") return value < 1 ? `${number.format(value * 1000)} µs` : `${number.format(value)} ms`;
	if (unit === "bytes") {
		const steps = ["B", "KB", "MB", "GB"];
		let step = 0;
		while (value >= 1000 && step < steps.length - 1) { value /= 1000; step++; }
		return `${number.format(value)} ${steps[step]}`;
	}
	if (unit === "/s") return `${number.format(value)}/s`;
	return number.format(value);
}
function percent(value) {
	return `${value > 0 ? "+" : ""}${number.format(value)}%`;
}
function when(at) {
	return at.replace("T", " ").replace(/:\d\dZ$/, " UTC");
}
function element(tag, attributes = {}, text) {
	const node = document.createElement(tag);
	for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
	if (text !== undefined) node.textContent = text;
	return node;
}

// A dashed vertical line where a new series starts, named by what changed.
const seriesMarks = {
	id: "seriesMarks",
	afterDatasetsDraw(chart, _, options) {
		const { ctx, chartArea, scales } = chart;
		ctx.save();
		ctx.strokeStyle = css("--muted");
		ctx.fillStyle = css("--muted");
		ctx.setLineDash([4, 4]);
		ctx.lineWidth = 1;
		ctx.font = "11px system-ui, sans-serif";
		for (const start of options.starts || []) {
			const x = start.slot > 0
				? (scales.x.getPixelForValue(start.slot - 1) + scales.x.getPixelForValue(start.slot)) / 2
				: chartArea.left;
			ctx.beginPath();
			ctx.moveTo(x, chartArea.top);
			ctx.lineTo(x, chartArea.bottom);
			ctx.stroke();
			ctx.save();
			ctx.translate(x - 3, chartArea.top);
			ctx.rotate(-Math.PI / 2);
			ctx.textAlign = "right";
			ctx.fillText(start.label, 0, 0);
			ctx.restore();
		}
		ctx.restore();
	},
};

// The machine's name at the end of its line, nudged apart so that names do not overlap.
const endLabels = {
	id: "endLabels",
	afterDatasetsDraw(chart) {
		const { ctx, chartArea } = chart;
		const ends = new Map();
		chart.data.datasets.forEach((dataset, index) => {
			if (!chart.isDatasetVisible(index)) return;
			const meta = chart.getDatasetMeta(index);
			for (let point = dataset.data.length - 1; point >= 0; point--) {
				if (dataset.data[point] !== null) {
					const element = meta.data[point];
					const end = ends.get(dataset.machine);
					if (!end || element.x > end.x) ends.set(dataset.machine, { x: element.x, y: element.y, machine: dataset.machine });
					break;
				}
			}
		});
		const placed = [...ends.values()].sort((a, b) => a.y - b.y);
		for (let index = 1; index < placed.length; index++) {
			placed[index].y = Math.max(placed[index].y, placed[index - 1].y + 13);
		}
		ctx.save();
		ctx.fillStyle = css("--text-secondary");
		ctx.font = "11px system-ui, sans-serif";
		ctx.textBaseline = "middle";
		for (const label of placed) {
			ctx.fillText(label.machine, Math.min(label.x + 8, chartArea.right + 8), label.y);
		}
		ctx.restore();
	},
};

function series(measure) {
	return data.lines.map((line) => {
		const values = new Array(data.commits.length).fill(null);
		const points = new Array(data.commits.length).fill(null);
		// The change is counted from the line's first number that is not zero; a line with
		// none has no change to show.
		const base = line.points.map((point) => point.values[measure.key]).find((value) => value);
		for (const point of line.points) {
			const value = point.values[measure.key];
			if (value === undefined) continue;
			if (mode === "change" && !base) continue;
			values[point.slot] = mode === "change" ? (value / base - 1) * 100 : value;
			points[point.slot] = point;
		}
		const stroke = colour(line.machine);
		return {
			label: line.machine,
			machine: line.machine,
			series: line.series,
			data: values,
			points,
			borderColor: stroke,
			backgroundColor: stroke,
			borderWidth: 2,
			pointRadius: points.map((point) => (point ? 4 : 0)),
			pointHoverRadius: points.map((point) => (point ? 6 : 0)),
			pointStyle: points.map((point) => (point && point.rust_from ? "triangle" : "circle")),
			pointBorderColor: css("--card"),
			pointBorderWidth: 1,
			spanGaps: true,
			tension: 0,
		};
	}).filter((dataset) => dataset.data.some((value) => value !== null));
}

function scale(datasets) {
	if (mode === "change") return "linear";
	const values = datasets.flatMap((dataset) => dataset.data).filter((value) => value !== null && value > 0);
	return values.length && Math.max(...values) / Math.min(...values) > 8 ? "logarithmic" : "linear";
}

function yAxis(datasets, measure) {
	const type = scale(datasets);
	return {
		type,
		beginAtZero: type === "linear" && mode === "values",
		grace: type === "linear" ? "8%" : 0,
		// Headroom on a logarithmic scale, so that no point sits on the edge.
		afterDataLimits: (axis) => {
			if (type !== "logarithmic") return;
			axis.min /= 1.4;
			axis.max *= 1.4;
		},
		grid: {
			color: (context) => (mode === "change" && context.tick && context.tick.value === 0 ? css("--muted") : css("--grid")),
		},
		border: { display: false },
		ticks: {
			color: css("--muted"),
			font: { size: 10 },
			maxTicksLimit: 6,
			callback: (value) => (mode === "change" ? percent(value) : format(value, measure.unit)),
		},
	};
}

function draw(canvas, measure) {
	const datasets = series(measure);
	return new Chart(canvas, {
		type: "line",
		data: { labels: data.commits.map((commit) => commit.commit.slice(0, 7)), datasets },
		options: {
			maintainAspectRatio: false,
			animation: false,
			layout: { padding: { right: labelRoom } },
			interaction: { mode: "index", intersect: false },
			scales: {
				x: {
					offset: true,
					grid: { display: false },
					border: { color: css("--grid") },
					ticks: { color: css("--muted"), font: { size: 10 }, maxRotation: 0, autoSkip: true, autoSkipPadding: 12 },
				},
				y: yAxis(datasets, measure),
			},
			plugins: {
				legend: { display: false },
				seriesMarks: { starts: data.starts },
				tooltip: {
					backgroundColor: css("--card"),
					borderColor: css("--border"),
					borderWidth: 1,
					titleColor: css("--text"),
					bodyColor: css("--text-secondary"),
					padding: 10,
					usePointStyle: true,
					callbacks: {
						title: (items) => {
							const commit = data.commits[items[0].dataIndex];
							return `${commit.commit} · first measured ${when(commit.at)}`;
						},
						label: (item) => {
							const point = item.dataset.points[item.dataIndex];
							if (!point) return null;
							const value = point.values[measure.key];
							let text = mode === "change"
								? `${percent(item.parsed.y)} (${format(value, measure.unit)})  ${item.dataset.machine}`
								: `${format(value, measure.unit)}  ${item.dataset.machine}`;
							if (data.series.length > 1) text += `  · ${item.dataset.series}`;
							if (point.rust_from) text += `  · Rust ${point.rust_from} → ${point.rust}`;
							if (point.runs > 1) text += `  · latest of ${point.runs} runs`;
							return text;
						},
						afterBody: (items) => items
							.map((item) => item.dataset.points[item.dataIndex])
							.filter((point) => point && point.note)
							.map((point) => `Note: ${point.note}`),
						labelColor: (item) => ({ borderColor: item.dataset.borderColor, backgroundColor: item.dataset.borderColor }),
						labelPointStyle: () => ({ pointStyle: "line", rotation: 0 }),
					},
				},
			},
		},
		plugins: [seriesMarks, endLabels],
	});
}

function table(className, titles, rows) {
	const grid = element("table", { class: className });
	const head = element("tr");
	for (const title of titles) head.append(element("th", { scope: "col" }, title));
	const thead = element("thead");
	thead.append(head);
	grid.append(thead);
	const body = element("tbody");
	for (const row of rows) body.append(row);
	grid.append(body);
	return grid;
}

function machinesTable() {
	const rows = data.machines.map((machine) => {
		const row = element("tr");
		const first = element("td");
		const swatch = element("i", { class: "swatch" });
		swatch.style.background = colour(machine.label);
		first.append(swatch, document.createTextNode(machine.label));
		row.append(first);
		for (const key of ["name", "processor", "memory", "disk", "system"]) row.append(element("td", {}, machine[key] || ""));
		const calibration = machine.calibration || {};
		row.append(element("td", {}, format(calibration.core_ms, "ms")));
		row.append(element("td", {}, format(calibration.sync_ms, "ms")));
		row.append(element("td", {}, calibration.write_mb_per_second === undefined ? "" : `${number.format(calibration.write_mb_per_second)} MB/s`));
		row.append(element("td", {}, when(machine.last)));
		return row;
	});
	document.getElementById("machines").replaceChildren(table("machines", [
		"Machine", "Name", "Processor", "Memory", "Disk", "System", "One core", "Sync", "Large file", "Last run",
	], rows));
}

function runsTable() {
	const rows = data.results.map((result) => {
		const row = element("tr");
		for (const text of [when(result.at), result.machine, result.branch || "", result.commit, result.rust || "", result.series]) {
			row.append(element("td", {}, text));
		}
		for (const measure of data.measures) row.append(element("td", {}, format(result.values[measure.key], measure.unit)));
		if (result.note) row.title = result.note;
		return row;
	});
	const wrapper = element("div", { class: "table" });
	wrapper.append(table("runs", ["When", "Machine", "Branch", "Commit", "Rust", "Series", ...data.measures.map((measure) => measure.title)], rows));
	document.getElementById("runs").append(wrapper);
}

function legend() {
	const root = document.getElementById("legend");
	root.replaceChildren();
	for (const machine of data.machines) {
		const item = element("span", machine.name ? { title: machine.name } : {});
		const swatch = element("i");
		swatch.style.background = colour(machine.label);
		item.append(swatch, document.createTextNode(machine.label));
		root.append(item);
	}
}

// Builds the page once; switching what the charts show or the colour scheme only redraws
// the charts, so that the table someone opened stays open.
function build() {
	const online = typeof Chart !== "undefined";
	document.getElementById("offline").style.display = online ? "none" : "block";
	document.getElementById("summary").textContent =
		`${data.results.length} results of ${data.commits.length} commits, `
		+ `in ${data.series.length === 1 ? "one series" : `${data.series.length} series`}: ${data.series.join(" → ")}`;
	machinesTable();
	if (online) {
		const grid = element("div", { class: "grid" });
		for (const measure of data.measures) {
			const card = element("div", { class: "card" });
			card.append(element("h3", {}, measure.title));
			card.append(element("p", { class: "better" }, measure.higher_is_better ? "higher is better" : "lower is better"));
			const plot = element("div", { class: "plot" });
			const canvas = element("canvas", { role: "img", "aria-label": measure.title });
			plot.append(canvas);
			card.append(plot);
			grid.append(card);
			charts.push({ canvas, measure, chart: draw(canvas, measure) });
		}
		document.getElementById("charts").append(grid);
	} else {
		document.getElementById("runs").open = true;
	}
	runsTable();
}

function refresh() {
	for (const entry of charts) {
		const datasets = series(entry.measure);
		entry.chart.data.datasets = datasets;
		entry.chart.options.scales.y = yAxis(datasets, entry.measure);
		entry.chart.update();
	}
}

function recolour() {
	legend();
	machinesTable();
	for (const entry of charts) {
		entry.chart.destroy();
		entry.chart = draw(entry.canvas, entry.measure);
	}
}

document.getElementById("title").textContent = data.title;
document.getElementById("source").textContent = `From ${data.source}.`;
for (const button of document.querySelectorAll(".toggle button")) {
	button.addEventListener("click", () => {
		mode = button.dataset.mode;
		for (const other of document.querySelectorAll(".toggle button")) other.setAttribute("aria-pressed", String(other === button));
		refresh();
	});
}
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", recolour);
legend();
build();
</script>
</body>
</html>
"""


if __name__ == "__main__":
	main()
