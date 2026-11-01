# NXR 3.9.0 → Lattice/NixPlane deployment integration

Status: architecture/operational guide, not a completed deployment. 2026-11-01.

NXR is the **command and durable workflow plane** over flake apps, not the trusted deployment authority. Keep individual leaf apps as canonical executable operations and compose them through `perSystem.nxr.tasks`. Ordinary `nxr task` execution and `nxr session` have different durability semantics: journal-first `nxrd` sessions require a running supervisor; distributed workers are experimental.

## Release graph to implement
- `fabric-source-verify`: exact Lattice + ProjectStargate + KernelFS + ecosystem SourceSet, lock files, worker fetchability and clean source.
- `fabric-build`: choose NXB worker on nixdev/qualified builder, produce content-addressed Nix closure or OCI digest, keep attempt/job IDs.
- `fabric-test`: unit, runtime, web integration, package/health and deployment preflight with zero production mutations.
- `fabric-publish`: immutable digest plus provenance and test evidence, not just a mutable tag.
- `sys5-stage`: copy closure or fetch image, verify and dry-activate; no production profile switch.
- `sys5-approve`: explicit operator decision bound to target, artifact, policy and rollback candidate.
- `sys5-activate`: execute an already-staged immutable artifact only; NixPlane Hub/NDX is the authorized deployment owner when enabled.
- `sys5-verify`: native process + PostgreSQL + Stargate gateway/host + web + external Cloudflare Access-protected route checks; record verdict and rollback on failure.

## Existing state and specific blockers
Lattice `stargate-cutover` and ecosystem use NXR v3.9.0. `fabric-doctor` and `fabric-source-smoke` passed; desktop build failed because Lattice's pinned Stargate `453358de` does not include `scripts/wasm-bundle.py`, which its `prepare-stargate.mjs` currently requires. Do not bypass pin checks with `STARGATE_TRUST_LOCAL=1` in a release.
The SYS-5 shell script has a deliberate `exit 0` inside `activate_staged()`, so the older source currently does not implement a new activation path. Do not expose that leaf as safe automatic deploy.

## DAG principles
- Explicit `paths`, `timeout`, `dependsOn`, `resources.exclusive` and secret contexts.
- `deploy` task is never a dependency of read-only build/test CI.
- Keep status of producer execution separate from artifact identity; bind artifacts to source+toolchain+run/attempt/job.
- Resume and cancellation must not duplicate publication or deployment effects.
- Do not give CI a production activation token merely to run tests.
- Report `not_run` and `blocked` rather than promoting absent evidence to green.

See NXR `docs/TASKS.md`, `docs/ROADMAP.md`, and NixPlane `docs/integrations/LATTICE_SYS5_RELEASE_CONTRACT.md` for system boundaries.
