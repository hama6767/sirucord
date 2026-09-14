from datetime import datetime, timezone
import unittest

from check_schedule import diagnose

NOW = datetime(2026, 9, 14, 21, 30, tzinfo=timezone.utc)


class DiagnosisTests(unittest.TestCase):
    def result(self, latest=None, state="active", expected=True):
        return diagnose(state, latest, expected=expected, now=NOW)

    def test_active_without_scheduled_runs_is_not_healthy(self):
        self.assertEqual(self.result()[0], "ERROR")

    def test_disabled_extra_checks_are_normal_while_empty(self):
        self.assertEqual(self.result(state="disabled_manually", expected=False)[0], "INFO")
        self.assertEqual(self.result(state="disabled_manually")[0], "ERROR")

    def test_successful_but_stale_run_is_unhealthy(self):
        run = {"created_at": "2026-09-14T20:00:00Z", "status": "completed", "conclusion": "success"}
        self.assertEqual(self.result(run)[0], "ERROR")

    def test_distinguishes_delivery_from_execution(self):
        run = {"created_at": "2026-09-14T21:25:00Z", "status": "queued", "conclusion": None}
        self.assertEqual(self.result(run)[0], "WARN")
        run.update(status="completed", conclusion="failure")
        self.assertEqual(self.result(run)[0], "ERROR")
        run["conclusion"] = "success"
        self.assertEqual(self.result(run)[0], "OK")


if __name__ == "__main__":
    unittest.main()
