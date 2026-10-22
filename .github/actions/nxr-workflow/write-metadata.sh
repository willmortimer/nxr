#!/bin/sh
# Write ci-run-metadata-v1. Arguments:
#   out provider external_job_id run_url idempotency_key operation_id exit_code
# An empty run_url is omitted. Values are data, not a shell command.
set -eu
out=$1
provider=$2
job=$3
url=$4
idem=$5
operation=$6
code=$7
python3 - "$out" "$provider" "$job" "$url" "$idem" "$operation" "$code" <<'PY'
import json
import sys

out, provider, job, url, idem, operation, code = sys.argv[1:8]
document = {
    "schema_version": "ci-run-metadata-v1",
    "provider": provider,
    "external_job_id": job,
    "idempotency_key": idem,
    "operation_id": operation,
    "exit_code": int(code),
}
if url:
    document["run_url"] = url
with open(out, "w", encoding="utf-8") as handle:
    json.dump(document, handle, indent=2)
    handle.write("\n")
PY
