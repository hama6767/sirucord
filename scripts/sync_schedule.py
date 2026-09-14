"""Enable extra five-minute checks only while a complete check reports occupants."""
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request

WORKFLOW = "monitor-active.yml"


def sync(occupied, request):
    if type(occupied) is not bool:
        raise ValueError("Missing or invalid occupancy; schedule preserved")
    state = request("GET", "")["state"]
    if state not in ("active", "disabled_manually", "disabled_inactivity"):
        raise ValueError("Unexpected workflow state; schedule preserved")
    if occupied and state != "active":
        request("PUT", "/enable")
    elif not occupied and state == "active":
        request("PUT", "/disable")
    print("Configured polling interval: 5 minutes (occupied)." if occupied else "Configured polling interval: 30 minutes (empty).")
    print("This confirms configuration only; GitHub schedule delivery may be delayed or dropped.")


def main():
    # No report is produced after an incomplete/failed application run. Never
    # interpret a missing report or unreadable state as an empty Discord server.
    report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    repository = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("Invalid repository")
    token = os.environ["GITHUB_TOKEN"]
    endpoint = f"https://api.github.com/repos/{repository}/actions/workflows/{WORKFLOW}"

    def request(method, suffix):
        req = urllib.request.Request(endpoint + suffix, method=method, headers={
            "Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
            "User-Agent": "Sirucord schedule", "X-GitHub-Api-Version": "2022-11-28",
        })
        try:
            with urllib.request.urlopen(req, timeout=20) as response:
                if response.status == 204:
                    return None
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"GitHub schedule API failed: HTTP {error.code}") from None

    sync(report.get("occupied"), request)


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(str(error))
        sys.exit(1)
    except Exception:
        print("Schedule synchronization failed; details redacted. Retry on the next base check.")
        sys.exit(1)
