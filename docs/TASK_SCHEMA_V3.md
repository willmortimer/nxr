# Task schema v3: typed input values

Task schema v3 keeps the task, app, context, process, cache, and dependency
contracts of v2 and adds richer parameter declarations. V1 remains tolerant of
unknown task fields; v2 and v3 reject unknown execution fields. V1/v2 do not
accept v3 parameter types or constraints. Existing declarations continue to
emit v1/v2 unless the consumer explicitly requests v3.

The flake-parts module automatically emits v3 when a parameter uses a richer
type or a `constraints` attribute. Forcing `nxr.schemaVersion = 1` or `2` in
that case fails evaluation. This does not upgrade the NXR pin in another
repository; an old runner must reject the new major.

## Declarations and normalization

Each parameter has `type`, optional `default`, optional `values`, and optional
`constraints`. Values are resolved independently for each DAG node.

| Type | Value and normalized child delivery |
| --- | --- |
| `string` | String, with optional character-count bounds and regex pattern |
| `boolean` | JSON Boolean; text CLI/environment aliases normalize to `true` or `false` |
| `choice` | String contained in `values` |
| `integer` | JSON integer; exact integer bounds and step |
| `number` | Finite JSON number; numeric bounds and optional positive step |
| `multichoice` | Unique allowed strings, sorted before canonical serialization |
| `path` | Nonempty local path string, relative to the caller invocation directory |
| `duration` | Unit-bearing `ms`/`s`/`m`/`h` string or integer milliseconds; canonical unsigned milliseconds |
| `object` | Strict object with declared `properties` and optional `required` names |
| `artifact_ref` | `{ "digest": "sha256:…", "producer": "…", "reference": "…" }` |
| `secret_ref` | Existing secret provider reference: `{ "ref": "…", "provider": "env|file|sops|sops-nix", "delivery": "env|file|stdin" }` |
| `observed_fact` | `{ "authority": "…", "reference": "…", "digest": "sha256:…" }` |
| `authorization_ref` | `{ "authority": "…", "reference": "…", "digest": "sha256:…" }` |

The reference schemas reject unknown fields, including a caller-supplied
`verified` flag. Secret, observed-fact, and authorization references cannot
have implicit defaults. Provider authorization, artifact availability and
integrity, evidence freshness, and domain grants still require the system
that owns those decisions. These envelopes do not attest those properties.

Strings are delivered directly through `NXR_PARAM_<NAME>`. Other values use
canonical JSON; object keys and multichoice subsets are sorted. Bare duration
text from CLI/environment is milliseconds, matching canonical environment
values and preventing repeated resolution from changing their meaning.

Constraints use these exact fields:

- String/choice/path: `minLength`, `maxLength`, `pattern`.
- Integer/number: `minimum`, `maximum`, `step`; step is measured from minimum
  when supplied, otherwise zero.
- Multichoice: `minItems`, `maxItems`.
- Path: `path = { kind = "any|file|directory"; exists = true; absolute = false; }`.
  Kind applies when the path exists; `exists` independently requires it.
- Object: `properties`, mapping names to recursive parameter declarations,
  and `required`, an array of declared property names. Undeclared object
  properties are always rejected.

Unknown or incompatible constraint fields fail validation. Path filesystem
checks occur during local input resolution, with the absolute caller invocation
directory provided explicitly. Remote paths are not inspected or silently
assumed to exist.

## Input values document

The values format contains neither task definitions nor authorization tokens:

```json
{
  "schema_version": "nxr-input-values-v1",
  "inputs": {
    "prepare": { "count": 4, "timeout": "2s" },
    "verify": { "count": 8 }
  }
}
```

Precedence is scoped CLI, unambiguous unqualified CLI, per-node input document,
authorized context binding, environment, declared default, interactive answer,
then a missing-input request. An invalid supplied value is an error; resolution
does not fall back to a lower-priority source. CLI and environment text must be
parsed according to the declared type before joining the typed layers.

An unqualified override must have compatible declarations across the selected
nodes. Node-local defaults remain independent even when names coincide. Each
child receives only its own canonical bindings. Persisted provenance records
contain parameter names and origin classes, not the input values. Secret
references retain the existing secret-dependent cache restrictions.

See [task-v3.schema.json](../schemas/task-v3.schema.json) and
[input-values-v1.schema.json](../schemas/input-values-v1.schema.json).
