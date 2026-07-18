# Telemetry

## Decision

**V1 ships with no telemetry.** The `nxr` binary does not phone home, collect usage metrics, or emit analytics events.

## Rationale

- `nxr` runs in developer and CI environments that may process proprietary code.
- The runner already surfaces diagnostics locally (`--verbose`, `plan`, `doctor`).
- Opt-in observability belongs in explicit, versioned event schemas (V2/V3), not silent collection.

## Future work

OpenTelemetry mapping for run events is deferred to V3.5 (ADR-0403 in [adr/README.md](adr/README.md)). Any future telemetry must be:

- opt-in;
- documented in the CLI contract;
- covered by an ADR before implementation.
