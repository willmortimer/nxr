#!/usr/bin/env python3
"""Classify a shell command before Cursor runs it."""

import json
import re
import sys


def decide(command: str) -> dict:
    text = command.strip()
    if not text or text.startswith("nxr ") or text == "nxr":
        return {"permission": "allow"}
    if re.search(r"(^|[;&|`(]\s*)rustup\b", text) or re.search(
        r"PATH=[^;&|]*(\.cargo/bin|/\.rustup)", text
    ):
        return {
            "permission": "deny",
            "user_message": "This command puts rustup ahead of the flake toolchain.",
            "agent_message": "Do not prepend ~/.cargo/bin or ~/.rustup. Discover the task with nxr list.",
        }
    fleet = "switch-to-configuration" in text or (
        re.search(r"\bnixos-rebuild\b|\bdeploy-rs\b|\bdeploy\b", text)
        and re.search(r"hub|sys-?5|nixdev|gmkdev", text, re.IGNORECASE)
    )
    if fleet or re.search(r"\bsys-?5\b", text, re.IGNORECASE):
        return {
            "permission": "deny",
            "user_message": "This command activates fleet infrastructure.",
            "agent_message": "Do not activate SYS-5, staging, Hub 6420, nixdev, or gmkdev from an automatic task.",
        }
    if re.search(r"(^|[;&|`(]\s*)cargo\b", text) or re.search(
        r"(^|[;&|`(]\s*)nix\s+(build|develop)\b", text
    ):
        return {
            "permission": "ask",
            "user_message": "This bypasses NXR. Confirm it is one already-failing command.",
            "agent_message": "Prefer nxr list and nxr task. cargo, nix build, and nix develop need confirmation.",
        }
    return {"permission": "allow"}


def main() -> int:
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        payload = {}
    command = payload.get("command") or ""
    json.dump(decide(command), sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
