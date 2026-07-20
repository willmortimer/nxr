# Architecture Decision Record Index

## 1. Purpose

This index lists decisions that should be captured as individual Architecture Decision Records as implementation proceeds.

V3-era ADRs (0201–0412) are **Deferred** and parked with [ideas/FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md). They are not active roadmap commitments. Active delivery follows [ROADMAP.md](../ROADMAP.md) (2.1–2.3).

An ADR is required when a decision:

- changes a public CLI or schema contract;
- creates an irreversible compatibility burden;
- defines a security or trust boundary;
- chooses between native Nix behavior and runner-owned behavior;
- affects local/CI equivalence;
- introduces persistent state, a daemon, a cache, or a network protocol (V3 ideas only until explicitly scheduled);

Statuses used here:

```text
Accepted   already established by the design package
Proposed   expected direction, requires implementation validation
Deferred   intentionally postponed until the named phase
Superseded replaced by a later ADR
Rejected   considered and explicitly not chosen
```

## 2. Foundational ADRs

| ADR | Title | Status | Target |
|---|---|---:|---:|
| ADR-0001 | Standard flake apps are the canonical leaf operation | Accepted | V1 |
| ADR-0002 | No mandatory second project task manifest | Accepted | V1 |
| ADR-0003 | Nix owns toolchain and runtime pinning | Accepted | V1 |
| ADR-0004 | Use the Nix CLI as the initial integration boundary | Proposed | V1 |
| ADR-0005 | Discover the flake root upward while preserving invocation CWD | Accepted | V1 |
| ADR-0006 | Define exact CLI/app argument parsing boundaries | Accepted | V1 |
| ADR-0007 | App closures own executable dependencies | Accepted | V1 |
| ADR-0008 | Inherit caller environment by default and provide explicit clean mode | Accepted | V1 |
| ADR-0009 | Keep apps, checks, and development shells as distinct sibling outputs | Accepted | V1 |
| ADR-0010 | Preserve direct `nix run` compatibility as an escape hatch | Accepted | V1 |
| ADR-0011 | Separate runner diagnostics from child stdout | Proposed | V1 |
| ADR-0012 | Preserve child exit status, signals, and terminal semantics | Proposed | V1 |
| ADR-0013 | Version all machine-readable schemas | Accepted | V1 |
| ADR-0014 | Sanitize untrusted flake metadata before terminal rendering | Proposed | V1 |
| ADR-0015 | Cache only discovery metadata in V1 | Accepted | V1 |
| ADR-0016 | Nix helper libraries must emit ordinary standard apps | Accepted | V1 |
| ADR-0017 | Support human, plain, JSON, and event-compatible output architecture | Proposed | V1 |
| ADR-0018 | Define minimum Nix versions through capability detection | Proposed | V1 |

## 3. V2 workflow ADRs

| ADR | Title | Status | Target |
|---|---|---:|---:|
| ADR-0101 | Tasks coordinate apps rather than replace them | Accepted | V2 |
| ADR-0102 | Store optional workflow metadata in a versioned flake output | Proposed | V2 |
| ADR-0103 | Define task/app name resolution and explicit conflict syntax | Proposed | V2 |
| ADR-0104 | Freeze DAG dependency and failure semantics | Proposed | V2 |
| ADR-0105 | Execute each logical task node at most once per plan | Proposed | V2 |
| ADR-0106 | Separate planning from execution | Accepted | V2 |
| ADR-0107 | Use a typed internal execution event bus | Proposed | V2 |
| ADR-0108 | Supervise parallel children through owned process groups | Proposed | V2 |
| ADR-0109 | Define stdin ownership for parallel and interactive tasks | Proposed | V2 |
| ADR-0110 | Define graceful shutdown and escalation deadlines | Proposed | V2 |
| ADR-0111 | Use native filesystem notifications for watch mode | Proposed | V2 |
| ADR-0112 | Define watch generation replacement and debounce semantics | Proposed | V2 |
| ADR-0113 | Make development-shell execution explicit, not automatic | Accepted | V2 |
| ADR-0114 | Mark integrated development shells for reliable nesting detection | Proposed | V2 |
| ADR-0115 | Activate shell completion through the dev shell without editing dotfiles | Accepted | V2 |
| ADR-0116 | Define task argument forwarding for single and multi-node plans | Proposed | V2 |
| ADR-0117 | Do not introduce general task-result caching in V2 | Accepted | V2 |
| ADR-0118 | Keep output rendering replaceable and independent from scheduling | Proposed | V2 |
| ADR-0119 | Make dangerous-operation metadata a guardrail, not a security boundary | Accepted | V2 |
| ADR-0120 | Define schema migration and unknown-field compatibility policy | Proposed | V2 |

## 4. V3 monorepo and action ADRs (deferred)

Parked with [ideas/FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md). Not scheduled for the active 2.x roadmap.

| ADR | Title | Status | Target |
|---|---|---:|---:|
| ADR-0201 | Introduce projects as first-class workspace identities | Deferred | V3.0 |
| ADR-0202 | Define canonical project/app/action addressing | Deferred | V3.0 |
| ADR-0203 | Build project graphs through explicit metadata plus ecosystem adapters | Deferred | V3.0 |
| ADR-0204 | Define confidence and override rules for inferred dependencies | Deferred | V3.0 |
| ADR-0205 | Combine Git, project, task, and Nix-input changes for affected analysis | Deferred | V3.0 |
| ADR-0206 | Define the workspace query language and stable selectors | Deferred | V3.0 |
| ADR-0207 | Introduce action contracts separately from ordinary tasks | Deferred | V3.1 |
| ADR-0208 | Define action identity and complete hash inputs | Deferred | V3.1 |
| ADR-0209 | Separate Nix store artifacts from workspace/report artifacts | Deferred | V3.1 |
| ADR-0210 | Introduce a CAS only for artifacts the Nix store cannot naturally own | Deferred | V3.1 |
| ADR-0211 | Define mutable, hermetic, and remote execution tiers | Deferred | V3.1 |
| ADR-0212 | Define artifact replay, retention, and garbage collection | Deferred | V3.1 |
| ADR-0213 | Define cache trust, signing, and poisoning defenses | Deferred | V3.1 |
| ADR-0214 | Keep CI plans provider-independent | Deferred | V3.2 |
| ADR-0215 | Use one logical workflow graph for local and CI execution | Deferred | V3.2 |
| ADR-0216 | Define historical timing and deterministic sharding policy | Deferred | V3.2 |
| ADR-0217 | Define test identity, retries, and flakiness records | Deferred | V3.2 |
| ADR-0218 | Define the public run event protocol | Deferred | V3.2 |
| ADR-0219 | Decide whether the run event protocol uses JSON, Protobuf, or both | Deferred | V3.2 |
| ADR-0220 | Define provenance records and reproducible-run capsules | Deferred | V3.2 |

## 5. V3 worker and development-fabric ADRs (deferred)

Parked with [ideas/FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md). Not scheduled for the active 2.x roadmap.

| ADR | Title | Status | Target |
|---|---|---:|---:|
| ADR-0301 | Keep the daemon optional for basic foreground execution | Deferred | V3.3 |
| ADR-0302 | Split local workspace daemon and remote worker responsibilities | Deferred | V3.3 |
| ADR-0303 | Define worker capability advertisement and matching | Deferred | V3.3 |
| ADR-0304 | Integrate native Nix remote builders before duplicating them | Accepted | V3.3 |
| ADR-0305 | Choose or adapt a remote action execution protocol | Deferred | V3.3 |
| ADR-0306 | Define source transfer and workspace materialization | Deferred | V3.3 |
| ADR-0307 | Define trusted, untrusted, and isolated execution pools | Deferred | V3.3 |
| ADR-0308 | Define burst execution across local and remote workers | Deferred | V3.3 |
| ADR-0309 | Introduce services as long-running apps with lifecycle metadata | Deferred | V3.4 |
| ADR-0310 | Define readiness, liveness, and dependency-state semantics | Deferred | V3.4 |
| ADR-0311 | Define automatic port allocation and collision policy | Deferred | V3.4 |
| ADR-0312 | Define local DNS and TLS namespace behavior | Deferred | V3.4 |
| ADR-0313 | Define persistent state ownership and cleanup | Deferred | V3.4 |
| ADR-0314 | Define Git worktree and branch workspace namespaces | Deferred | V3.4 |
| ADR-0315 | Define DevPod, devcontainer, and DevCell backend interfaces | Deferred | V3.4 |
| ADR-0316 | Define remote development environment lifecycle | Deferred | V3.4 |

## 6. V3 platform and governance ADRs (deferred)

Parked with [ideas/FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md). Not scheduled for the active 2.x roadmap.

| ADR | Title | Status | Target |
|---|---|---:|---:|
| ADR-0401 | Define stable IDE and agent-facing APIs | Deferred | V3.5 |
| ADR-0402 | Define graph and run subscription protocols | Deferred | V3.5 |
| ADR-0403 | Map run events to OpenTelemetry traces and logs | Deferred | V3.5 |
| ADR-0404 | Define capability-based execution policy | Deferred | V3.5 |
| ADR-0405 | Represent secrets as references, never serialized plan values | Accepted | V3.5 |
| ADR-0406 | Define approvals and non-interactive authorization | Deferred | V3.5 |
| ADR-0407 | Define organization policy layering and repository overrides | Deferred | V3.5 |
| ADR-0408 | Keep deployment orchestration separate from infrastructure reconciliation | Accepted | V3.5 |
| ADR-0409 | Define release artifact promotion and provenance | Deferred | V3.5 |
| ADR-0410 | Decide plugin boundaries and extension compatibility guarantees | Deferred | V3.5 |
| ADR-0411 | Define local-first and self-hostable guarantees | Accepted | V3.5 |
| ADR-0412 | Define product boundaries that prevent platform sprawl | Accepted | V3.5 |

## 7. ADR creation order

The first implementation ADRs should be written in this order:

1. ADR-0001 through ADR-0013 before the CLI contract is considered stable.
2. ADR-0101 through ADR-0110 before V2 task execution begins.
3. ADR-0111 through ADR-0120 before V2 schema freeze.
4. V3 ADRs (0201–0412) only if a future control-plane effort is explicitly scheduled — see [ideas/FUTURE_CONTROL_PLANE.md](../ideas/FUTURE_CONTROL_PLANE.md).

## 8. Repository location

Recommended layout:

```text
docs/
  adr/
    README.md
    0001-standard-flake-apps.md
    0002-no-second-manifest.md
    ...
```

The filename is stable even if the ADR title later changes.

Rejected and superseded ADRs remain in the repository.
