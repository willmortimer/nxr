#!/bin/sh
# Write ci-run-metadata-v2. Arguments:
#   out provider run_id job run_attempt run_url operation_id exit_code
# An empty operation_id is JSON null. An empty run_url is omitted.
set -eu
out=$1
provider=$2
run_id=$3
job=$4
attempt=$5
url=$6
operation=$7
code=$8
python3 - "$out" "$provider" "$run_id" "$job" "$attempt" "$url" "$operation" "$code" <<'PY'
import json
import sys

out, provider, run_id, job, attempt, url, operation, code = sys.argv[1:9]
attempt_number = int(attempt)
document = {
    "schema_version": "ci-run-metadata-v2",
    "provider": provider,
    "external_job_id": f"{run_id}/{job}/{attempt_number}",
    "run_id": run_id,
    "job": job,
    "run_attempt": attempt_number,
    "idempotency_key": f"{provider}:{run_id}/{job}/{attempt_number}",
    "operation_id": operation or None,
    "exit_code": int(code),
}
if url:
    document["run_url"] = url
with open(out, "w", encoding="utf-8") as handle:
    json.dump(document, handle, indent=2)
    handle.write("\n")
PY
