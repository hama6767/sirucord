import unittest
from pathlib import Path
import re
from sync_schedule import sync


class ScheduleTests(unittest.TestCase):
    def test_schedules_combine_into_five_minutes_without_overlap(self):
        root = Path(__file__).resolve().parents[1] / ".github" / "workflows"
        def minutes(name):
            cron = re.search(r"cron: '([^']+)'", (root / name).read_text()).group(1)
            return set(map(int, cron.split()[0].split(",")))
        base = minutes("monitor.yml")
        extra = minutes("monitor-active.yml")
        self.assertEqual(base, {17, 47})
        self.assertFalse(base & extra)
        self.assertEqual(base | extra, set(range(2, 60, 5)))

    def run_sync(self, occupied, state):
        calls = []

        def request(method, suffix):
            calls.append((method, suffix))
            return {"state": state}

        sync(occupied, request)
        return calls

    def test_occupied_enables_extra_checks(self):
        self.assertEqual(self.run_sync(True, "disabled_manually"), [("GET", ""), ("PUT", "/enable")])

    def test_empty_disables_extra_checks(self):
        self.assertEqual(self.run_sync(False, "active"), [("GET", ""), ("PUT", "/disable")])

    def test_unchanged_state_does_not_write(self):
        self.assertEqual(self.run_sync(True, "active"), [("GET", "")])
        self.assertEqual(self.run_sync(False, "disabled_manually"), [("GET", "")])

    def test_inactivity_can_be_recovered(self):
        self.assertEqual(self.run_sync(True, "disabled_inactivity")[-1], ("PUT", "/enable"))

    def test_invalid_reports_never_change_schedule(self):
        for occupied in (None, "false", 0, [], {}):
            with self.assertRaises(ValueError):
                sync(occupied, lambda *_: self.fail("Invalid report must not access GitHub"))

    def test_unknown_state_and_read_errors_never_write(self):
        with self.assertRaises(ValueError):
            self.run_sync(False, "deleted")
        calls = []

        def fail(method, suffix):
            calls.append((method, suffix))
            raise RuntimeError("network")

        with self.assertRaises(RuntimeError):
            sync(False, fail)
        self.assertEqual(calls, [("GET", "")])


if __name__ == "__main__":
    unittest.main()
