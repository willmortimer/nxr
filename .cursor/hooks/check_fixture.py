#!/usr/bin/env python3
"""Feed fixture commands to the shell hook and require the expected permission."""

import json
import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parent
hook = root / "before_shell.py"
cases = json.loads((root / "fixtures.json").read_text())
failed = 0
for case in cases:
    result = subprocess.run(
        [sys.executable, str(hook)],
        input=json.dumps({"command": case["command"]}),
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        print(f"hook failed for {case['command']}: {result.stderr}")
        failed += 1
        continue
    got = json.loads(result.stdout)["permission"]
    if got != case["permission"]:
        print(f"{case['command']}: expected {case['permission']}, got {got}")
        failed += 1
malformed = subprocess.run(
    [sys.executable, str(hook)],
    input="{not json",
    text=True,
    capture_output=True,
    check=False,
)
if malformed.returncode != 0 or json.loads(malformed.stdout)["permission"] != "deny":
    print("malformed hook input was not denied")
    failed += 1
config = json.loads((root.parent / "hooks.json").read_text())
hook_def = config["hooks"]["beforeShellExecution"][0]
if hook_def.get("failClosed") is not True:
    print("beforeShellExecution is not failClosed")
    failed += 1
if failed:
    raise SystemExit(1)
print(f"{len(cases)} shell decisions matched")
