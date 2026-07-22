# Security

`nxr` executes code from Nix flakes. It must not imply otherwise.

Threat model, trust boundaries, and runner security requirements are documented in:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — §8 Security model
- [docs/CONTRACT_SUMMARY.md](docs/CONTRACT_SUMMARY.md)
- [docs/EXECUTION_CONTEXT.md](docs/EXECUTION_CONTEXT.md) — planned secret delivery, trust approvals, schema v2
- [docs/adr/README.md](docs/adr/README.md) — security-related ADRs (e.g. metadata sanitization, secrets as references)

## V1 security checklist

Before tagging a release or adopting `nxr` in CI, confirm:

1. **Trust the flake** — remote references and substituters follow your organization's Nix trust policy.
2. **Inspect before run** — use `nxr plan <app>` or `--dry-run` to see the exact `nix run` command.
3. **No shell interpolation** — app names and arguments are passed to Nix without shell evaluation (ADR-0006).
4. **Untrusted metadata** — app descriptions and flake metadata are sanitized before terminal rendering (ADR-0014).
5. **Environment policy** — use `nxr doctor --clean-env` when validating apps that must not depend on a dev-shell `PATH`.
6. **No hidden telemetry** — V1 collects no usage data ([docs/TELEMETRY.md](docs/TELEMETRY.md)).

V1.0 engineering review: [docs/SECURITY_REVIEW_V1.md](docs/SECURITY_REVIEW_V1.md).

Full requirements: [ARCHITECTURE.md §8](docs/ARCHITECTURE.md#8-security-model).

## Reporting

Until a dedicated process is published, report security issues privately to the repository maintainers (do not open a public issue for exploitable flaws).
