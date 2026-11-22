# Dynamic planning v1

Frozen for Pleroma Wave 2. `v3.9.2` documents this contract. `v3.9.3` implements it.

Planning order is fixed:

1. Resolve known inputs.
2. Run an admitted read-only discovery operation.
3. Return dependent questions through the existing interaction session.
4. Validate typed answers.
5. Compile the selected conditional DAG.
6. Freeze source identity, input identities, discovery digest, and selected branches.
7. Request domain authorization from the owning system.
8. Execute only through the existing scheduler.

A changed source revision, dirty tree, discovery digest, or selected branch invalidates the frozen plan. Completed answers do not start a destructive effect. NXR does not invent observed facts and does not grant disk, network, or enrollment authority.

## Conditions

Conditions are JSON objects. The only forms are `all`, `any`, `equals`, `in`, and `compare`.

```json
{
  "all": [
    { "equals": { "input": "network_mode", "value": "wifi" } },
    { "equals": { "fact": "wifi_supported", "value": true } }
  ]
}
```

`in` tests membership of an input or fact against `values`. `compare` uses `eq`, `ne`, `lt`, `lte`, `gt`, or `gte` on numbers. There is no shell, no JavaScript, and no general expression language. A condition that references an unknown input, fact, or operator is a plan error.
