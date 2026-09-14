"""Check Discord installation without disclosing tokens, channel IDs or messages."""
import argparse
import json
import os
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--configure-intents", action="store_true")
    args = parser.parse_args()
    token = os.environ["DISCORD_BOT_TOKEN"].strip()
    config = tomllib.loads(os.environ["SIRUCORD_CONFIG"])

    def request(path, payload=None):
        req = urllib.request.Request(
            "https://discord.com/api/v10" + path,
            data=None if payload is None else json.dumps(payload).encode(),
            method="GET" if payload is None else "PATCH",
            headers={"Authorization": "Bot " + token,
                     "Content-Type": "application/json", "User-Agent": "Sirucord setup"},
        )
        try:
            with urllib.request.urlopen(req, timeout=20) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, None

    status, app = request("/applications/@me")
    if status != 200:
        print(f"Discord application authentication failed: HTTP {status}.")
        return 1
    application_id = str(app["id"])
    if not application_id.isdigit():
        raise ValueError("Invalid application ID")
    invite = "https://discord.com/oauth2/authorize?" + urllib.parse.urlencode({
        "client_id": application_id, "scope": "bot", "permissions": 1115136,
        "integration_type": 0,
    })
    # Application IDs and installation links are public; never print guild IDs.
    print("Bot installation link: " + invite)
    print("Bot settings: https://discord.com/developers/applications/" + application_id + "/bot")
    targets = config.get("servers", []) + config.get("streamers", [])
    message_content = any(t.get("announcement_channel_id") for t in targets)
    presence = any(t.get("use_activity") for t in targets)
    flags = int(app.get("flags", 0))
    required = 0
    if message_content and not flags & ((1 << 18) | (1 << 19)):
        required |= 1 << 19
    if presence and not flags & ((1 << 12) | (1 << 13)):
        required |= 1 << 13
    issues = 0
    if required and args.configure_intents:
        editable = (1 << 13) | (1 << 15) | (1 << 19)
        status, updated = request("/applications/@me", {"flags": (flags & editable) | required})
        if status == 200 and int(updated.get("flags", 0)) & required == required:
            print("Required limited Gateway intents enabled.")
            required = 0
        else:
            print(f"Automatic intent configuration failed: HTTP {status}. Enable required intents in the Bot settings.")
    if required:
        print("Required Gateway intents are not enabled. Check Message Content / Presence settings.")
        issues += 1
    else:
        print("Required Gateway intents: OK.")
    for number, target in enumerate(targets, 1):
        status, _ = request("/guilds/" + target["guild_id"])
        if status != 200:
            print(f"Target {number}: bot is not installed or cannot access the server (HTTP {status}).")
            issues += 1
            continue
        for channel in target["voice_channel_ids"]:
            status, data = request("/channels/" + channel)
            if status != 200 or data.get("guild_id") != target["guild_id"] or data.get("type") not in (2, 13):
                print(f"Target {number}: voice channel is missing, inaccessible or in another server (HTTP {status}).")
                issues += 1
        channel = target.get("announcement_channel_id")
        if channel:
            status, data = request("/channels/" + channel)
            if status != 200 or data.get("guild_id") != target["guild_id"]:
                print(f"Target {number}: announcement channel is missing, inaccessible or in another server (HTTP {status}).")
                issues += 1
            else:
                status, _ = request("/channels/" + channel + "/messages?limit=1")
                if status != 200:
                    print(f"Target {number}: cannot read announcement history (HTTP {status}).")
                    issues += 1
    print("Setup checks passed." if not issues else f"Setup needs attention: {issues} issue(s).")
    return 1 if issues else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        print("Setup check failed: network or configuration error (details redacted).")
        sys.exit(1)
