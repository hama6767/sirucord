"""Provision and adapt a cron-job.org timer. Never print API responses or tokens."""
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone

WORKFLOW = "monitor-external.yml"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def api(base, token):
    if not token.strip():
        raise ValueError("Missing API credential")
    opener = urllib.request.build_opener(NoRedirect)

    def request(method, path, payload=None):
        req = urllib.request.Request(base + path, method=method,
                                     data=None if payload is None else json.dumps(payload).encode(), headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json", "Accept": "application/json",
            "User-Agent": "Sirucord external scheduler", "X-GitHub-Api-Version": "2022-11-28",
        })
        try:
            with opener.open(req, timeout=20) as response:
                data = response.read(2 * 1024 * 1024)
                return json.loads(data) if data else {}
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"Scheduler API request failed: HTTP {error.code}. Existing interval is preserved on failure.") from None
    return request


def schedule(interval):
    if interval not in (5, 30):
        raise ValueError("Invalid polling interval")
    return {"timezone": "UTC", "expiresAt": 0, "hours": [-1], "mdays": [-1],
            "months": [-1], "wdays": [-1],
            "minutes": list(range(4, 60, 5)) if interval == 5 else [14, 44]}


def body(branch, interval):
    return json.dumps({"ref": branch, "inputs": {"interval_minutes": str(interval), "dry_run": False}})


def extended_data(branch, interval, token):
    if not token.strip():
        raise ValueError("Missing dispatch credential")
    return {"headers": {"Authorization": "Bearer " + token,
                        "Accept": "application/vnd.github+json",
                        "Content-Type": "application/json",
                        "X-GitHub-Api-Version": "2022-11-28"},
            "body": body(branch, interval)}


def interval_of(job):
    actual = job.get("schedule", {})
    for interval in (5, 30):
        if actual == schedule(interval):
            return interval
    return None


def target(repository):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("Invalid repository")
    return f"https://api.github.com/repos/{repository}/actions/workflows/{WORKFLOW}/dispatches"


def provision(request, repository, branch, dispatch_token, enabled):
    url = target(repository)
    title = f"Sirucord {repository}"
    listing = request("GET", "/jobs")
    if listing.get("someFailed", False):
        raise ValueError("Incomplete job listing; refusing to create a duplicate")
    jobs = [job for job in listing["jobs"] if job.get("title") == title]
    if len(jobs) > 1 or (jobs and jobs[0].get("url") != url):
        raise ValueError("Ambiguous existing job; configuration preserved")
    interval = (interval_of(jobs[0]) if jobs else None) or 5
    job = {"title": title, "url": url, "enabled": enabled, "saveResponses": False,
           "requestMethod": 1, "requestTimeout": 15, "redirectSuccess": False,
           "schedule": schedule(interval),
           "notification": {"onFailure": True, "onFailureCount": 3, "onDisable": True},
           "extendedData": extended_data(branch, interval, dispatch_token)}
    if jobs:
        job_id = valid_id(jobs[0]["jobId"])
        request("PATCH", f"/jobs/{job_id}", {"job": job})
    else:
        job_id = valid_id(request("PUT", "/jobs", {"job": job})["jobId"])
    return job_id, interval


def valid_id(value):
    if not re.fullmatch(r"[1-9][0-9]*", str(value)):
        raise ValueError("Invalid cron job ID")
    return str(value)


def sync(occupied, dispatched_interval, job_id, repository, branch, dispatch_token, request):
    if type(occupied) is not bool:
        raise ValueError("Incomplete occupancy report; preserving the timer")
    job_id = valid_id(job_id)
    target(repository)
    desired = 5 if occupied else 30
    if dispatched_interval:
        if dispatched_interval not in ("5", "30"):
            raise ValueError("Invalid dispatched interval")
        current = int(dispatched_interval)
    else:
        # A manual check has no timer snapshot. Scheduled checks need no GET:
        # the provider sends its configured interval in the request body.
        job = request("GET", f"/jobs/{job_id}")["jobDetails"]
        if job.get("url") != target(repository) or job.get("title") != f"Sirucord {repository}":
            raise ValueError("Unexpected cron job; refusing to modify it")
        current = interval_of(job)
    if current == desired:
        print(f"External timer interval: {desired} minutes; no management API request needed for timer-triggered checks.")
        return
    # Update schedule, request body and headers together so a partial nested
    # update cannot accidentally remove authentication or leave a stale hint.
    request("PATCH", f"/jobs/{job_id}", {"job": {
        "schedule": schedule(desired), "extendedData": extended_data(branch, desired, dispatch_token),
    }})
    print(f"External timer changed to {desired} minutes.")


def main():
    request = api("https://api.cron-job.org", os.environ["CRONJOB_API_KEY"])
    repository = os.environ["GITHUB_REPOSITORY"]
    branch = os.environ.get("DEFAULT_BRANCH", "main")
    if sys.argv[1] == "setup":
        dispatch_token = os.environ["SIRUCORD_DISPATCH_TOKEN"]
        github = api("https://api.github.com", dispatch_token)
        target(repository)
        # Prove Actions:write before storing a timer. This test cannot publish.
        github("POST", f"/repos/{repository}/actions/workflows/{WORKFLOW}/dispatches",
               {"ref": branch, "inputs": {"dry_run": True, "interval_minutes": "5"}})
        job_id, interval = provision(request, repository, branch, dispatch_token,
                                     os.environ.get("ACTIVATE", "false") == "true")
        print(f"CRONJOB_JOB_ID={job_id}")
        print(f"Configured interval: {interval} minutes. Connection test dispatched without posting.")
        if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
            with Path(summary).open("a", encoding="utf-8") as output:
                output.write(f"Set repository variable `SIRUCORD_CRONJOB_ID` to `{job_id}` and "
                             "`SIRUCORD_SCHEDULER` to `cronjob` after the connection test succeeds.\n")
    elif sys.argv[1] == "sync":
        report = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
        sync(report.get("occupied"), os.environ.get("EXTERNAL_INTERVAL", ""),
             os.environ["SIRUCORD_CRONJOB_ID"], repository, branch,
             os.environ["SIRUCORD_DISPATCH_TOKEN"], request)
    elif sys.argv[1] == "inspect":
        job_id = valid_id(os.environ["SIRUCORD_CRONJOB_ID"])
        job = request("GET", f"/jobs/{job_id}")["jobDetails"]
        if job.get("url") != target(repository):
            raise ValueError("Unexpected timer target")
        # Deliberately exclude headers, tokens, bodies and other account jobs.
        summary = {key: job.get(key) for key in ("enabled", "lastStatus", "lastExecution", "nextExecution")}
        summary["interval_minutes"] = interval_of(job)
        print("External timer: " + json.dumps(summary))
        for execution in request("GET", f"/jobs/{job_id}/history")["history"][:5]:
            print("Timer execution: " + json.dumps({
                "planned_utc": datetime.fromtimestamp(execution["datePlanned"], timezone.utc).isoformat(),
                "actual_utc": datetime.fromtimestamp(execution["date"], timezone.utc).isoformat(),
                "status": execution["status"], "http_status": execution["httpStatus"],
            }))
        print("A successful HTTP dispatch only confirms GitHub accepted the request. Check the Actions run for completion.")
    else:
        raise ValueError("Unknown command")


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(str(error))
        sys.exit(1)
    except Exception:
        print("External scheduler operation failed; details redacted. Check credentials, job ID, and API quota.")
        sys.exit(1)
