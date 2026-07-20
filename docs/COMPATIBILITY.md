# Compatibility

## Supported platforms (V1)

Initial release matrix:

| System | Architecture | CI | Notes |
|---|---|---|---|
| `aarch64-darwin` | Apple Silicon macOS | `macos-latest` | Primary developer platform |
| `x86_64-darwin` | Intel macOS | — | Flake outputs; CI when available |
| `aarch64-linux` | Linux (arm64) | — | Flake outputs; CI when available |
| `x86_64-linux` | Linux (amd64) | `ubuntu-latest` | Primary CI platform |

The root flake evaluates all four systems. CI currently exercises `aarch64-darwin`
and `x86_64-linux`; other Unix targets may build from source but are not part of
the V1 compatibility guarantee until listed with CI coverage.

## Nix

`nxr` delegates execution to the Nix CLI. A working `nix` on `PATH` is required.

### Tested support floor

| Requirement | Floor |
|---|---|
| Nix CLI | **2.18+** (exercised in development and CI) |
| Experimental features | `nix-command` and `flakes` must be enabled |

Capability negotiation still runs on older Nix releases (roughly 2.4+): `nxr`
detects version and feature flags once per adapter construction and chooses a
compatible argv rather than hard-coding a single global flag set. Missing
optional capabilities (for example `--offline`, `--no-write-lock-file`,
`--accept-flake-config`, `--log-format json`) are omitted; flakes being
disabled is a hard capability error.

Inspect negotiated capabilities with:

```bash
nxr doctor --json
```

The JSON envelope includes a `capabilities` object:

```json
{
  "schema_version": 1,
  "capabilities": {
    "version": "2.34.7",
    "flakes_enabled": true,
    "supports_json_log_format": true,
    "supports_no_write_lock_file": true,
    "supports_offline": true,
    "supports_accept_flake_config": true
  },
  "findings": []
}
```

See [ARCHITECTURE.md](ARCHITECTURE.md) §9 and ADR-0018 in [adr/README.md](adr/README.md).

### CI Nix matrix

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) exercises two Nix versions on
`ubuntu-latest` and `macos-latest`:

| Matrix label | Install source |
|---|---|
| `latest` | Default Determinate Nix from `nix-installer-action` |
| `2.18` | Upstream Nix **2.18.9** via `nix-package-url` (support floor) |

Fixture smoke tests run the packaged `nix build .#nxr` binary, not `nix run` on the
project flake apps.

## Escape hatch

Every V1 operation has an equivalent native form:

```bash
nix run .#<app> -- [args...]
```

Projects remain operable without `nxr` installed. See [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md).

## Machine-readable schemas (V2.0 freeze)

Orchestration metadata and plan envelopes are **frozen at schema version 1**
for the V2.0 release line:

| Schema | File | Stability |
|---|---|---|
| Task document | [`schemas/task-v1.schema.json`](../schemas/task-v1.schema.json) | **Frozen** — `schema_version: 1` on flake output `nxr.<system>` |
| Execution plan | [`schemas/execution-plan-v1.schema.json`](../schemas/execution-plan-v1.schema.json) | **Frozen** — internal envelope for `nxr plan` task fallback and scheduling |
| Execution events | [`schemas/events-v1.schema.json`](../schemas/events-v1.schema.json) | **Frozen** — matches the Rust [`Event`](../crates/nxr-task/src/events.rs) enum (`type`-tagged JSON) |

Policy for V2.x:

- **Additive** optional fields on existing envelopes are allowed when older
  consumers can ignore them (for example `argument_forwarding` on execution
  plans).
- **Breaking** shape or semantics changes require a new major `schema_version`
  and a new schema file; unsupported majors are rejected at load time.
- The `plan-v1` and `list-v1` CLI output schemas follow the same additive-only
  rule within major version 1.

See [TASKS.md](TASKS.md) for author-facing task fields and V2 argument/stdin
freeze.

## Extension points (V2.x bridge)

V3 may grow adapters and negotiated Nix capabilities without creating a second
operation authority:

- **Metadata adapters** — optional readers for adjacent project metadata (for
  example devenv or mission-control) may suggest or project task graphs, but
  flake apps (`apps.<system>.<name>`) remain the only canonical leaf
  operations.
- **Capability-negotiated Nix** — the Nix adapter detects available CLI
  features at runtime rather than hard-coding a floor; missing optional
  capabilities degrade gracefully (see [ARCHITECTURE.md](ARCHITECTURE.md) §4.3
  and §9).
- **Event / schema surfaces** — versioned JSON schemas (`task-v1`,
  `execution-plan-v1`, `events-v1`) are the stable machine-readable contracts;
  consumers must ignore additive optional fields within a major.

Adapters must not replace flake apps, introduce a second toolchain resolver, or
make standard flake outputs subordinate to an opaque runner database. Direct
`nix run` remains the escape hatch.

## Reporting gaps

Open an issue with:

- `uname -m` and OS version;
- `nix --version`;
- the `nxr` command and flake reference;
- `nxr plan <app> --json` when execution fails.
