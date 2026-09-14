"""Read-only diagnosis of schedule delivery; never contact Discord or Mastodon."""
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request

WORKFLOWS = ("monitor.yml", "monitor-active.yml")


def diagnose(state, latest, *, expected, now):
    if not expected:
        return "INFO", "Not currently required (monitoring off or extra checks disabled)."
    if state != "active":
        return "ERROR", f"Expected workflow is not active: {state}."
    if latest is None:
        return "ERROR", "No retained schedule-triggered run exists. Manual runs do not verify scheduling."
    created = datetime.fromisoformat(latest["created_at"].replace("Z", "+00:00"))
    age = int((now - created).total_seconds() / 60)
    if age > 60:
        return "ERROR", f"No new scheduled run for {age} minutes; configured cadence is not being met."
    if latest["status"] != "completed":
        return "WARN", "Schedule delivered a run, but execution is still pending or in progress."
    if latest["conclusion"] != "success":
        return "ERROR", f"Schedule delivered a run, but its conclusion is {latest['conclusion']}. Inspect that run."
    return "OK", f"Scheduled execution succeeded; latest run was created {age} minutes ago. Timing is not guaranteed."


def report(request, enabled, now):
    repo = request("")
    lines = ["# Sirucord schedule diagnosis", "", f"Checked at: {now.isoformat()}",
             f"Default branch: `{repo['default_branch']}`; public: `{not repo['private']}`; "
             f"fork: `{repo['fork']}`; archived: `{repo['archived']}`.",
             f"SIRUCORD_ENABLED: `{enabled}`.", ""]
    unhealthy = False
    for name in WORKFLOWS:
        workflow = request(f"/actions/workflows/{name}")
        scheduled = request(f"/actions/workflows/{name}/runs?event=schedule&per_page=1")
        runs = scheduled["workflow_runs"]
        latest = runs[0] if runs else None
        # Disabled extra checks are normal while everyone is absent. The base
        # workflow must remain active whenever monitoring is enabled.
        expected = enabled == "true" and (name == "monitor.yml" or workflow["state"] == "active")
        level, detail = diagnose(workflow["state"], latest, expected=expected, now=now)
        unhealthy |= level == "ERROR"
        lines.extend([f"## {name}", "", f"Workflow ID: `{workflow['id']}`; state: `{workflow['state']}`.",
                      f"Retained scheduled runs: **{scheduled['total_count']}**.", f"**{level}**: {detail}"])
        if latest:
            lines.append(f"[Latest scheduled run]({latest['html_url']}) — "
                         f"`{latest['created_at']}`, `{latest['status']}`, `{latest['conclusion']}`.")
        manual = request(f"/actions/workflows/{name}/runs?event=workflow_dispatch&per_page=1")["workflow_runs"]
        if manual:
            run = manual[0]
            lines.append(f"[Latest manual run]({run['html_url']}) — "
                         f"`{run['created_at']}`, `{run['status']}`, `{run['conclusion']}`.")
        lines.append("")
    lines.extend(["## Interpretation", "",
                  "No scheduled run means diagnosis must start before application execution. "
                  "A queued or failed scheduled run instead points to execution or application logs.",
                  "An active setting or successful manual run alone does not prove that cron is working. "
                  "This snapshot cannot distinguish an internal GitHub registration problem from delay or dropped events.",
                  "This check only reads GitHub metadata. It does not post, change schedules, or use Discord/Mastodon tokens.",
                  "", "[GitHub scheduling limitations](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule)"])
    return "\n".join(lines) + "\n", unhealthy


def main():
    repository = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("Invalid repository")
    token = os.environ.get("GH_TOKEN") or os.environ["GITHUB_TOKEN"]

    def request(suffix):
        req = urllib.request.Request(f"https://api.github.com/repos/{repository}{suffix}", headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "Sirucord schedule diagnosis",
        })
        try:
            with urllib.request.urlopen(req, timeout=20) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"GitHub metadata read failed: HTTP {error.code}") from None

    text, unhealthy = report(request, os.environ.get("SIRUCORD_ENABLED", "unknown"), datetime.now(timezone.utc))
    print(text)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a", encoding="utf-8") as output:
            output.write(text)
    return int(unhealthy)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as error:
        print(str(error))
        sys.exit(1)
    except Exception:
        print("Schedule diagnosis could not complete; details redacted.")
        sys.exit(1)
