# Security

`nxr` executes code from Nix flakes. It must not imply otherwise.

Threat model, trust boundaries, and runner security requirements are documented in:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — §8 Security model
- [docs/CONTRACT_SUMMARY.md](docs/CONTRACT_SUMMARY.md)
- [docs/adr/README.md](docs/adr/README.md) — security-related ADRs (e.g. metadata sanitization, secrets as references)

## Reporting

Until a dedicated process is published, report security issues privately to the repository maintainers (do not open a public issue for exploitable flaws).
