# ADR-0145: Capability-cache config layer hashes config file identity

- **Status:** Accepted
- **Date:** 2026-07-27
- **Target release:** 2.7.1
- **Related ADRs:** ADR-0133
- **Supersedes:** Warm-hit claim that env-var digests alone prove config freshness

## Context

Layer-2 warm hits previously hashed `NIX_CONFIG` / `NIX_USER_CONF_FILES` /
`NIX_CONF_DIR` string values only. Editing a file at an unchanged path left the
digest matching while effective Nix configuration changed.

## Decision

Environment-layer digest input includes, for each known config path:

- absolute path string;
- size, mtime, and ctime when available;
- content hash (BLAKE3) when the file is readable.

Paths come from `NIX_USER_CONF_FILES` (split), `NIX_CONF_DIR` defaults
(`nix.conf`), and XDG/user Nix conf locations when present. `NIX_CONFIG` inline
text remains hashed. Periodic effective-config revalidation via
`nix config show --json` remains a backstop on layer miss / forced refresh.

Bump capability-cache schema when on-disk format changes.

## Validation

- Test: same env vars, mutated conf file contents → env-layer miss / config reprobe.
- Warm unchanged path: still 0 version/help/config probes.
