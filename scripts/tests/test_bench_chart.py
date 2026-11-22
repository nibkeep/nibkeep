"""Tests of scripts/bench-chart.py, without Git.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import json
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "bench-chart.py"
spec = importlib.util.spec_from_file_location("bench_chart", SCRIPT)
chart = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chart)


def api(machine, at, commit, data=1, recipe=1, rust="1.99.0", **numbers):
	result = {
		"machine": machine, "at": at, "commit": commit, "kind": "api", "data": data,
		"recipe": recipe, "rust": rust, "entities": 1000, "repeat": 3, "open_seconds": 1.0,
	}
	result.update(numbers)
	return result


def nib(machine, at, commit, command, data=1, recipe=1, seconds=0.2):
	return {
		"machine": machine, "at": at, "commit": commit, "kind": "nib", "command": command,
		"data": data, "recipe": recipe, "rust": "1.99.0", "runs": 10,
		"total_without_unlock_median_seconds": seconds,
	}


class Charts(unittest.TestCase):
	def test_commits_are_ordered_by_when_they_were_first_measured(self):
		built = chart.build([
			api("windows-laptop", "2026-11-03T00:00:00Z", "c1"),
			api("linux-desktop", "2026-11-02T00:00:00Z", "c2"),
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1"),
		])
		self.assertEqual([each["commit"] for each in built["commits"]], ["c1", "c2"])

	def test_a_commit_written_short_and_long_is_one_place(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "6da467c"),
			api("windows-laptop", "2026-11-02T00:00:00Z", "6da467c1d2e3"),
		])
		self.assertEqual(built["commits"], [{"commit": "6da467c1d2e3", "at": "2026-11-01T00:00:00Z"}])

	def test_the_nib_commands_of_a_run_join_its_point(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1"),
			nib("linux-desktop", "2026-11-01T00:10:00Z", "c1", "find", seconds=0.1),
			nib("linux-desktop", "2026-11-01T00:11:00Z", "c1", "edit", seconds=0.3),
		])
		[line] = built["lines"]
		[point] = line["points"]
		self.assertEqual(point["values"], {
			"open_seconds": 1.0,
			"nib find: total_without_unlock_median_seconds": 0.1,
			"nib edit: total_without_unlock_median_seconds": 0.3,
		})
		titles = [measure["title"] for measure in built["measures"]]
		self.assertEqual(titles, ["Open", "nib find, without unlocking", "nib edit, without unlocking"])

	def test_measures_come_in_a_fixed_order_and_say_which_way_is_better(self):
		built = chart.build([api(
			"linux-desktop", "2026-11-01T00:00:00Z", "c1", disk_bytes_per_entity=3000.0,
			search_first_p95_ms=4.0, entities_written_per_second=700.0,
		)])
		self.assertEqual(
			[(measure["key"], measure["higher_is_better"]) for measure in built["measures"]],
			[
				("open_seconds", False), ("search_first_p95_ms", False),
				("entities_written_per_second", True), ("disk_bytes_per_entity", False),
			],
		)

	def test_descriptions_are_not_charted(self):
		built = chart.build([api(
			"linux-desktop", "2026-11-01T00:00:00Z", "c1",
			calibration={"core_ms": 90.0}, hardware={"cores": 16},
		)])
		self.assertEqual([measure["key"] for measure in built["measures"]], ["open_seconds"])

	def test_the_latest_run_of_a_commit_on_a_machine_is_its_point(self):
		built = chart.build([
			api("linux-desktop", "2026-11-02T00:00:00Z", "c1", open_seconds=3.0),
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1", open_seconds=2.0),
		])
		[line] = built["lines"]
		self.assertEqual([point["values"]["open_seconds"] for point in line["points"]], [3.0])
		self.assertEqual(line["points"][0]["runs"], 2)
		self.assertEqual(len(built["results"]), 2)

	def test_new_data_breaks_the_line_and_is_marked(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1"),
			api("linux-desktop", "2026-11-02T00:00:00Z", "c2", data=2),
			api("windows-laptop", "2026-11-02T01:00:00Z", "c2", data=2),
		])
		lines = sorted((line["machine"], line["series"]) for line in built["lines"])
		self.assertEqual(lines, [
			("linux-desktop", "data 1, recipe 1"), ("linux-desktop", "data 2, recipe 1"),
			("windows-laptop", "data 2, recipe 1"),
		])
		self.assertEqual(built["starts"], [{"slot": 1, "label": "data 2"}])
		self.assertEqual(built["series"], ["data 1, recipe 1", "data 2, recipe 1"])

	def test_a_new_recipe_is_marked_as_one(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1"),
			api("linux-desktop", "2026-11-02T00:00:00Z", "c2", recipe=2),
		])
		self.assertEqual(built["starts"], [{"slot": 1, "label": "recipe 2"}])

	def test_a_bridge_shows_the_commit_on_both_sides(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1", open_seconds=1.0),
			api("linux-desktop", "2026-11-02T00:00:00Z", "c2", open_seconds=1.1),
			api("linux-desktop", "2026-11-02T01:00:00Z", "c2", data=2, open_seconds=1.5),
			api("linux-desktop", "2026-11-03T00:00:00Z", "c3", data=2, open_seconds=1.4),
		])
		old, new = built["lines"]
		self.assertEqual([point["slot"] for point in old["points"]], [0, 1])
		self.assertEqual([point["slot"] for point in new["points"]], [1, 2])
		self.assertEqual(new["points"][0]["values"]["open_seconds"], 1.5)
		self.assertEqual(built["starts"], [{"slot": 1, "label": "data 2"}])

	def test_a_change_of_rust_is_marked_on_the_point(self):
		built = chart.build([
			api("linux-desktop", "2026-11-01T00:00:00Z", "c1", rust="1.99.0"),
			api("linux-desktop", "2026-11-02T00:00:00Z", "c2", rust="1.99.1"),
		])
		[line] = built["lines"]
		self.assertEqual([point["rust_from"] for point in line["points"]], [None, "1.99.0"])


class Machines(unittest.TestCase):
	def test_each_machine_with_its_description_and_latest_calibration(self):
		results = [
			api("windows-laptop", "2026-11-01T00:00:00Z", "c1", calibration={"core_ms": 120.0}),
			api("windows-laptop", "2026-11-02T00:00:00Z", "c2", calibration={"core_ms": 110.0}),
			api("android-m52", "2026-11-02T00:00:00Z", "c2"),
		]
		descriptions = {"windows-laptop": {"name": "ASUS ROG Zephyrus G14", "disk": "Samsung PM991a"}}
		self.assertEqual(chart.machines(results, descriptions), [
			{"label": "android-m52", "last": "2026-11-02T00:00:00Z", "calibration": {}},
			{
				"label": "windows-laptop", "last": "2026-11-02T00:00:00Z", "calibration": {"core_ms": 110.0},
				"name": "ASUS ROG Zephyrus G14", "disk": "Samsung PM991a",
			},
		])

	def test_a_description_is_read_from_toml(self):
		self.assertEqual(
			chart.description('name = "ASUS ROG Zephyrus G14"  # its model\nmemory = "16 GB"\n'),
			{"name": "ASUS ROG Zephyrus G14", "memory": "16 GB"},
		)

	def test_a_description_that_cannot_be_read_is_left_out(self):
		self.assertEqual(chart.description('name = "unclosed\n'), {})

	def test_only_texts_are_shown(self):
		self.assertEqual(chart.description('name = "Pi"\nports = 4\n'), {"name": "Pi"})


class Page(unittest.TestCase):
	def test_the_data_cannot_close_the_script_element(self):
		page = chart.page({"note": "</script><script>alert(1)</script>"})
		self.assertNotIn("</script><script>alert", page)

	def test_the_data_survives_the_page(self):
		data = {"note": "</script> & <b>"}
		page = chart.page(data)
		start = page.index('id="data">') + len('id="data">')
		end = page.index("</script>", start)
		self.assertEqual(json.loads(page[start:end]), data)


if __name__ == "__main__":
	unittest.main()
