# ADR-0146: Secret refs distinguish provider from logical reference

- **Status:** Accepted
- **Date:** 2026-07-27
- **Target release:** 2.7.1 (schema + env provider), 3.0 (bindings)
- **Related ADRs:** ADR-0123, ADR-0405

## Context

Docs described logical refs such as `openseat/prod/token` resolved via user
bindings, while the runtime called `std::env::var(ref)` — path-like refs cannot
work.

## Decision

Each context secret declares:

```text
provider = "env" | "file" | "sops" | "sops-nix" | …   # default "env" until 3.0
ref      = "<provider-specific reference>"
delivery = "env" | "file" | "stdin"
```

For `provider = "env"`, `ref` is an environment variable name in the caller
environment. For other providers, `ref` is a logical binding key resolved only
through user/HM `secretBindings` (3.0+). Slot name is the child env/file key
used at delivery.

Until non-env providers ship, non-`env` providers hard-error at plan/run.
Nix module + JSON schema gain optional `provider` (default `env`).

## Validation

- Env provider: `ref = "CLOUDFLARE_API_TOKEN"` works.
- Path-like ref with default env provider fails with guidance to set
  `provider` / bindings (3.0), not silent empty.
