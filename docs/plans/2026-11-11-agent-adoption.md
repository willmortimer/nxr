# Agent adoption

**Status:** note. Do not treat it as a score. The A4.2 counts stay blank until the Cursor trials in [2026-11-11-a4.2-runbook.md](2026-11-11-a4.2-runbook.md) are run.

The question after those two scores is whether MCP changes how often the agent picks the right operation without help. The baseline and the MCP path share the prompts, the starting trees, and the safety bar. They differ in whether the agent can call the operation tools.

If the MCP count is higher, the operation catalog is doing the work the hooks and rules were asking the agent to remember. If the counts match, the next work is discovery, environment resolution, or tool selection. It is not more MCP surface.

The shell hook and the policy digest stay cooperative controls. The hook denies literal trust commands. The digest rejects an unapproved policy file on the normal NXR path. Neither isolates the trust database from other code running as the same user.

B2.0 and B3.0 start only after `v3.9.1` is tagged on `db8961a`. SourceSet and one remote NixPlane attempt follow those slices. v0.2.14 and a distributed DAG stay out of this milestone.
