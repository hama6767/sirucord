import json
import unittest

from external_schedule import provision, schedule, sync, target

REPO = "example/sirucord"
TOKEN = "test-only-placeholder"


class ExternalTimerTests(unittest.TestCase):
    def test_unchanged_occupancy_does_not_consume_management_quota(self):
        for occupied, hint in ((True, "5"), (False, "30")):
            sync(occupied, hint, "123", REPO, "main", TOKEN,
                 lambda *args: self.fail("No API call expected"))

    def test_transition_updates_interval_body_and_auth_in_one_request(self):
        for occupied, old, wanted in ((True, "30", 5), (False, "5", 30)):
            calls = []
            sync(occupied, old, "123", REPO, "main", TOKEN, lambda *args: calls.append(args))
            self.assertEqual(len(calls), 1)
            method, path, payload = calls[0]
            self.assertEqual((method, path), ("PATCH", "/jobs/123"))
            job = payload["job"]
            self.assertEqual(job["schedule"], schedule(wanted))
            self.assertEqual(json.loads(job["extendedData"]["body"])["inputs"],
                             {"interval_minutes": str(wanted), "dry_run": False})
            self.assertEqual(job["extendedData"]["headers"]["Authorization"], "Bearer " + TOKEN)

    def test_bad_reports_and_hints_never_modify_timer(self):
        for occupied, hint, job_id in ((None, "5", "123"), ("false", "5", "123"),
                                       (True, "1", "123"), (True, "30", "../other")):
            with self.assertRaises(ValueError):
                sync(occupied, hint, job_id, REPO, "main", TOKEN,
                     lambda *args: self.fail("No API call expected"))

    def test_manual_check_reads_timer_and_rejects_unrelated_job(self):
        calls = []

        def request(method, path, payload=None):
            calls.append((method, path, payload))
            return {"jobDetails": {"title": "Other job", "url": "https://example.com"}}

        with self.assertRaises(ValueError):
            sync(True, "", "123", REPO, "main", TOKEN, request)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "GET")

    def test_failed_patch_is_not_retried_or_acknowledged(self):
        calls = []

        def request(*args):
            calls.append(args)
            raise RuntimeError("HTTP 429")

        with self.assertRaises(RuntimeError):
            sync(True, "30", "123", REPO, "main", TOKEN, request)
        self.assertEqual(len(calls), 1)

    def test_setup_reuses_existing_job_and_rotates_dispatch_token(self):
        calls = []

        def request(method, path, payload=None):
            calls.append((method, path, payload))
            return {"someFailed": False, "jobs": [{"title": f"Sirucord {REPO}",
                    "url": target(REPO), "jobId": 123, "schedule": schedule(30)}]}

        self.assertEqual(provision(request, REPO, "main", TOKEN, True), ("123", 30))
        self.assertEqual([c[0] for c in calls], ["GET", "PATCH"])
        self.assertFalse(calls[1][2]["job"]["saveResponses"])

    def test_setup_creates_one_named_post_job(self):
        calls = []

        def request(method, path, payload=None):
            calls.append((method, path, payload))
            return {"jobs": [], "someFailed": False} if method == "GET" else {"jobId": 456}

        self.assertEqual(provision(request, REPO, "main", TOKEN, True), ("456", 5))
        job = calls[-1][2]["job"]
        self.assertEqual(job["url"], target(REPO))
        self.assertEqual(job["requestMethod"], 1)
        self.assertTrue(job["enabled"])

    def test_incomplete_listing_never_creates_duplicate(self):
        calls = []

        def request(*args):
            calls.append(args)
            return {"jobs": [], "someFailed": True}

        with self.assertRaises(ValueError):
            provision(request, REPO, "main", TOKEN, True)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
