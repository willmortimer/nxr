#!/usr/bin/env python3
"""Classify a shell command before Cursor runs it.

This hook is not NixPlane's authorization boundary. Admission and activation
policy stay on the Hub.
"""

import json
import re
import sys

LOCAL_TASKS = ("fmt-check", "test", "fmt", "lint", "ci", "deny")
DISCOVERY = {
    "nxr",
    "nxr list",
    "nxr --help",
    "nxr -h",
    "nxr --version",
    "nxr -V",
    "nxr task",
    "nxr task --help",
    "nxr task -h",
}


def deny(agent_message: str) -> dict:
    return {
        "permission": "deny",
        "user_message": "This command was blocked by the project shell hook.",
        "agent_message": agent_message,
    }


def ask(agent_message: str) -> dict:
    return {
        "permission": "ask",
        "user_message": "Confirm this command before it runs.",
        "agent_message": agent_message,
    }


def fleet_activation(text: str) -> bool:
    if re.search(r"hub-update|fabric-sync|sys-?5", text, re.IGNORECASE):
        return True
    return bool(
        re.search(r"\bnixos-rebuild\b|\bdeploy-rs\b|\bdeploy\b", text)
        and re.search(r"hub|sys-?5|nixdev|gmkdev", text, re.IGNORECASE)
    )


def generation_switch(text: str) -> bool:
    return "switch-to-configuration" in text


def trust_mutation(text: str) -> bool:
    """An agent must not approve or rewrite its own project trust."""
    if "trusted-projects.json" in text:
        return True
    return bool(
        re.search(
            r"(^|[;&|`(]\s*)nxr\s+trust\s+(approve-policy|add|revoke)\b",
            text,
        )
    )


def is_compound(text: str) -> bool:
    """A second command must not inherit the first command's permission."""
    if any(token in text for token in (";", "&&", "||", "`", "$(")):
        return True
    without_or = text.replace("||", "")
    return "|" in without_or


def local_nxr_task(text: str) -> bool:
    if is_compound(text):
        return False
    match = re.fullmatch(r"nxr task (\S+)(?:\s+.*)?", text)
    return bool(match and match.group(1) in LOCAL_TASKS)


def decide(command: str) -> dict:
    text = command.strip()
    if re.search(r"(^|[;&|`(]\s*)rustup\b", text) or re.search(
        r"PATH=[^;&|]*(\.cargo/bin|/\.rustup)", text
    ):
        return deny("Do not prepend ~/.cargo/bin or ~/.rustup. Discover the task with nxr list.")
    if fleet_activation(text):
        return deny(
            "Do not activate SYS-5, staging, Hub 6420, nixdev, or gmkdev from an automatic task."
        )
    if trust_mutation(text):
        return deny(
            "Do not change project trust or approve an execution policy from an automatic task."
        )
    if generation_switch(text):
        return ask(
            "This switches a NixOS generation. Confirm the prepared closure before it activates."
        )
    if text in DISCOVERY or local_nxr_task(text):
        return {"permission": "allow"}
    if text == "nxr" or text.startswith("nxr "):
        return ask(
            "This nxr command is not a known local check. Confirm it before it changes local or remote state."
        )
    if re.search(r"(^|[;&|`(]\s*)cargo\b", text) or re.search(
        r"(^|[;&|`(]\s*)nix\s+(build|develop)\b", text
    ):
        return ask(
            "Prefer nxr list and nxr task. cargo, nix build, and nix develop need confirmation."
        )
    return {"permission": "allow"}


def main() -> int:
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        json.dump(
            deny("The shell hook could not read its input, so the command was not run."),
            sys.stdout,
        )
        sys.stdout.write("\n")
        return 0
    if not isinstance(payload, dict):
        json.dump(deny("The shell hook received an unexpected payload."), sys.stdout)
        sys.stdout.write("\n")
        return 0
    command = payload.get("command") or ""
    json.dump(decide(command), sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
