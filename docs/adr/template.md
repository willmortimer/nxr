# ADR-NNNN: Decision Title

- **Status:** Proposed
- **Date:** YYYY-MM-DD
- **Owners:** 
- **Target release:** 
- **Related ADRs:** 
- **Supersedes:** 
- **Superseded by:** 

## Context

Describe the problem, constraints, current behavior, and why a durable decision is required.

Include:

- user-facing behavior;
- Nix compatibility concerns;
- local and CI implications;
- platform constraints;
- security and trust boundaries;
- existing ecosystem conventions;
- implementation pressure that makes postponement risky.

## Decision drivers

- Driver one
- Driver two
- Driver three

## Considered options

### Option A

Description.

Advantages:

- 

Disadvantages:

- 

### Option B

Description.

Advantages:

- 

Disadvantages:

- 

## Decision

State the chosen behavior precisely.

Avoid aspirational wording. Include enough detail to write conformance tests.

## Public contract

Document any resulting:

- CLI syntax;
- output or event schema;
- flake output shape;
- environment variables;
- filesystem paths;
- process behavior;
- compatibility guarantees.

## Consequences

### Positive

- 

### Negative

- 

### Neutral or accepted tradeoffs

- 

## Compatibility and migration

Explain:

- behavior for existing standard flake apps;
- fallback behavior without `nxr` metadata;
- schema migration;
- version gating;
- deprecation period;
- direct `nix run` behavior.

## Security and trust

Explain:

- untrusted input handling;
- secrets behavior;
- remote execution implications;
- cache trust;
- terminal-output sanitization;
- permissions required.

## Operational impact

Explain:

- persistent state;
- cache invalidation;
- logging and observability;
- cleanup;
- failure recovery;
- cross-platform differences.

## Validation plan

List:

- unit tests;
- integration fixtures;
- compatibility tests;
- performance tests;
- failure-injection tests;
- manual UX validation.

## Rollout

Describe staged implementation and rollback.

## Unresolved questions

- 

## References

List relevant project documents, issue discussions, and external design references.
