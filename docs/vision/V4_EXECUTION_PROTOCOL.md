# V4+ execution protocol (ideas)

This document captures the **post–workspace-scripting** product thesis. It is
**not** the active delivery plan.

**Near-term (scheduled):** workspace scripts, file-backed apps, and materialized
process environments — [ROADMAP.md](../ROADMAP.md) §3.3–3.4 and ADR-0169–0171.

**Earlier speculative V3 control-plane prose** remains in
[FUTURE_CONTROL_PLANE.md](FUTURE_CONTROL_PLANE.md). Mine that file for
requirements; prefer **this** framing for anything ordered after 3.4.

---

## Thesis

| Layer | Owns |
|---|---|
| **Nix** | packages, development environments, derivations, store realizations, binary caches, native remote builders |
| **NXR** | operation graph, workspace scripts, process lifecycle, inputs/approvals, run events, logs/artifacts, UI / IDE / CI / agent interfaces |
| **Task implementations** | Bash, Nushell, Python, TypeScript, Rust, external tools, eventually WASI components |

Invariant through every version:

> Nix owns reproducible environments and artifacts. NXR owns operational intent,
> execution lifecycle, interaction, and observability. Scripts remain the simple
> imperative layer between them.

Version framing:

```text
v3   ergonomic runner and orchestrator around flake apps
     (+ 3.3/3.4 workspace scripting and process-env acceleration)
v4   local-first execution protocol for apps, scripts, processes, CI, IDEs, agents
v5   distributed execution fabric
```

V4 should be a **protocol major**, not merely a feature collection. Three
boundaries are approaching their next major:

1. Task schema still requires an `app` leaf.
2. Event schema covers node lifecycle and byte streams, not rich interaction.
3. Daemon protocol v1 is a cache, not an execution authority.

---

## V4.0 — General operation IR and public run protocol

Introduce an internal tagged operation representation produced by desugaring
existing surfaces (flake apps, tasks, workspace scripts, processes, later
adapters/WASI)—not a hand-authored mega-manifest:

```text
Operation::FlakeApp { name }
Operation::WorkspaceScript { path, interpreter, environment }
Operation::NixBuild { installable }
Operation::Process { app, readiness, restart }
Operation::WasiComponent { component, world, capabilities }
Operation::Adapter { kind, configuration }
```

Legacy `tasks.test.app = "test"` desugars to `FlakeApp("test")`. No forced
project migration.

### Event protocol v2

Envelope every event:

```text
EventEnvelope { protocol, run_id, sequence, timestamp, source, event }
```

Expand beyond plan/node/stdout/exit to include step/progress, prompts,
diagnostics, artifacts, service readiness, debug endpoints, approvals,
external-run links, cache decisions, deployment guards.

TUI, IDE, and agent APIs should **consume** this protocol—not invent parallel
ones. Precedes those UIs.

Related deferred ADRs to revisit: ADR-0218, ADR-0219, ADR-0401–0403.

---

## V4.1 — Durable runs and interactive automation

Optional second `nxrd` role: **run-coordinator** (cache remains primary).
Direct mode must remain available; daemon never mandatory for `nxr test`
(ADR-0301 spirit / ADR-0157).

### Durable run model

```text
nxr runs
nxr watch-run <id>
nxr attach <id>
nxr cancel <id>
nxr rerun <id>
nxr rerun <id> --failed
```

Store: SQLite metadata/state; chunked stdout/stderr files; indexed diagnostics
and artifacts; monotonic event sequences for reconnect; redacted inputs and
secret **references** only.

### Typed inputs and prompts

Declare choice/boolean/confirm inputs on tasks; render as terminal questions,
TUI, VS Code Quick Pick, CI inputs, agent JSON, or noninteractive values.

Scripts communicate via a dedicated control FD/socket (`nxr step`, `nxr
progress`, `nxr diagnostic`, `nxr artifact`, `nxr ask`)—never by polluting
stdout with magic sequences.

---

## V4.2 — One-screen TUI, IDE, and debugger integration

Once runs/events are protocol objects:

- `nxr task ci --ui dashboard` / `nxr attach <id> --ui dashboard` — DAG visible,
  collapse completed nodes, recent output for selection, detach without cancel,
  reconnect, compact scrollback summary. Builds on existing EventSink /
  live|grouped|failures|summary|raw.
- VS Code reference extension: discovery, run views, Quick Pick, Problems
  diagnostics, artifacts, start/cancel/rerun, pre-launch bridge.
- DAP broker: NXR orchestrates build → dependents → readiness → launch existing
  adapters (LLDB/GDB/Delve/debugpy/Node); editor speaks normal DAP; NXR owns
  environment and lifetime.

---

## V4.3 — Agent and CI operational interface

Prefer structured operations over “run arbitrary shell”:

```text
operations.list / describe
runs.start / inspect / cancel / answer_prompt
runs.get_diagnostics / get_failed_logs / get_artifacts / rerun_failed
graph.affected
services.ensure_ready
```

Same protocol powers local execution, GitHub Actions annotations, workflow
watching, job summaries, external-run links, artifact ingestion, retry/cancel.
A workflow can remain `nxr ci run pull-request` while NXR owns the graph.

Related: ADR-0139, ADR-0214, ADR-0215.

---

## V4.4 — Nix-native release, deployment, and fleet operations

Narrow Nix-focused slice—not a universal Ansible clone.

Fleet nodes reference NixOS configurations; deploy/rollback adapters around
`nixos-rebuild`, deploy-rs, Colmena, nixos-anywhere, native closure transfer.
NXR provides selection, structured progress, build/copy/activate stages, health
checks, rollback guards, summaries, agent-readable results.

Does **not** implement mutable replacements for NixOS modules (packages, users,
firewall, systemd units as desired state).

Typed workflow transitions (not host reconciliation): `ssh.command`,
`file.upload`, `http.wait`, `tcp.wait`, `systemd.wait`, `github.dispatch`,
`github.wait`, `oci.pull`, `database.migrate`, `secret.materialize`,
`deployment.confirm`.

Related: ADR-0408.

---

## V4.5 — `builtins.wasm`, WASI, WIT, KernelFS

Two distinct layers:

1. **`builtins.wasm` (evaluation-time)** — pure computation: parse inventories,
   validate schemas, expand matrices, normalize metadata, analyze graphs,
   policy decisions. Results are ordinary versioned NXR/Nix metadata; Nix/Lix
   retain a fallback path.
2. **WASI/WIT operation runtime** — `world nxr-task` importing context, events,
   prompts, process, artifacts, secrets, workspace; KernelFS views
   (`/project`, `/work`, `/output`, `/secrets`). Portable across macOS/Linux/CI/
   Cell/microVM/agent/worker.

Shell, Nushell, Python, and native executables remain first-class. WASI is an
additional tier, not the price of entry.

---

## V5 — Distributed execution fabric

Reserve the genuinely distributed control plane for v5:

- Remote workspace CAS; worker registration/leases; capability advertisement
  (CPU/memory/GPU/KVM/Xcode/platform/isolation); source transfer; remote
  workspace actions; event/artifact streaming; cancellation/draining; trusted
  vs untrusted pools; microVM/Cell isolation; self-hosted control plane.

**Critical rule:** Nix remote builders for derivations; NXR workers for mutable
workspace actions, interactive operations, platform-specific automation, and
tasks that are not naturally derivations. Visualize and route native Nix
builder work—do not recreate it.

Related deferred ADRs: ADR-0301–0308, ADR-0140.

---

## Explicitly out of the 3.3/3.4 scripting milestone

Do not bundle into workspace scripting:

- Full-screen TUI; typed interactive prompt schemas; VS Code extension; DAP;
  remote workers; fleet deployment; WIT brokerage; WASI sandbox execution;
  general plugin SDK; new YAML/TOML project manifest.

That milestone answers one question:

> Can a developer check in a five-line script and run it with mise-like
> immediacy while inheriting a Nix-defined environment?

---

## References

- ADR-0169 / 0170 / 0171
- [FUTURE_CONTROL_PLANE.md](FUTURE_CONTROL_PLANE.md)
- [EXECUTION_CONTEXT.md](../EXECUTION_CONTEXT.md)
- [ROADMAP.md](../ROADMAP.md)
- Deferred ADR index §4–6 in [adr/README.md](../adr/README.md)
