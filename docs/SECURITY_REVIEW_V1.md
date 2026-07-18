# V1 security review

**Date:** 2026-07-18  
**Scope:** V1 flake app runner only (discover, list, run, plan, select, doctor, completion). V2 task/watch/graph surfaces are stubs and out of scope.  
**Method:** Architecture §8 checklist against the implementation, plus a Security Review pass on packaging/man-page changes (no medium-or-higher findings).

This is an engineering review for release readiness, not a third-party audit.

## Requirement → evidence → status

| ARCHITECTURE §8 requirement | Evidence | Status |
|---|---|---|
| Display remote flake references clearly | Flake ref preserved in plan JSON/human output (`nxr-core` `Plan`, `commands/plan.rs`) | Pass |
| Never interpolate app names into a shell | Argv builders in [`crates/nxr-nix/src/command.rs`](../crates/nxr-nix/src/command.rs); spawn via [`nxr_process::run_in`](../crates/nxr-process/src/foreground.rs) (`Command::new` + args, no `sh -c`); test `no_shell_evaluation_of_args` | Pass (ADR-0006) |
| Do not silently load arbitrary project config outside the flake | V1 has no `nxr.toml`; discovery is `flake.nix` upward walk + `nix flake show` | Pass |
| Version custom metadata schemas | V1 list/plan JSON use `schema_version`; V2 task schemas deferred | Pass for V1 |
| Expose exact command with `--dry-run` / `plan` | Shared `prepare_app_plan` → `Plan.command`; dry-run prints plan without spawn (`commands/run.rs`) | Pass |
| Allow restricted environment execution | `nxr doctor --clean-env` diagnostics; apps inherit caller env by default (documented) | Pass |
| Preserve Nix trust / substituters | Runner invokes user `nix` with argv only; no custom substituter injection | Pass |
| Avoid evaluating unrelated outputs when possible | Discovery uses `nix flake show --json` for apps; run uses `nix run …#app` | Pass |
| Do not auto-execute shell hooks merely for discovery | Discovery does not enter `nix develop` / shellHook | Pass |
| Sanitize descriptions / metadata for the terminal | [`sanitize_terminal_text`](../crates/nxr-core/src/sanitize.rs); used in human list/error paths (ADR-0014) | Pass |
| No hidden telemetry | [`docs/TELEMETRY.md`](TELEMETRY.md) | Pass |
| Completion protocol stays quiet | `__complete` writes candidates to stdout only; failures → empty list (`commands/complete.rs`); timeout 500 ms | Pass |

## Packaging note (`__manpage`)

Hidden `nxr __manpage` renders clap help via `clap_mangen` with **no** Nix evaluation or shell. Used only for `installManPage` / `cargo run -p xtask -- man`. Security review found **no blockers**.

## Non-blocking follow-ups

- Optional CLI test that `__manpage` keeps stderr clean on success (parity with `__complete`).
- Align bash dynamic reserved-name list with `__manpage` for consistency.
- Promote ADR-0014 from Proposed → Accepted when convenient.

## Sign-off

V1 runner meets ARCHITECTURE §8 for a **1.0.0** taggable release given the trust model: the user trusts the flake they run, and `nxr` must not add shell evaluation, silent config, or hidden network telemetry on top of Nix.
