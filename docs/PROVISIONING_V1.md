# Server provisioning boundary v1

Frozen for Pleroma Wave 2. This is a NixPlane and tailscale-policy boundary. `v3.9.2` does not issue Tailscale keys.

The first issuer may mint only `tag:pleroma-server`. Its OAuth client has the `auth_keys` capability and an authorized tag set of exactly that tag. It does not receive `devices:core`, policy write, user management, or any other Pleroma tag. `tag:personal` is not assigned to provisioned servers. `tagOwners` is not widened. The Windows pilot device token is not reused.

The broker operations are `ProvisionPlan`, `ProvisionAuthorize`, `ProvisionIssueKey`, `ProvisionObserveNode`, `ProvisionComplete`, and `ProvisionRevokeUnusedKey`. Each call carries an idempotency key and the intended machine identity.

The OAuth client secret stays in a SOPS runtime file. It never enters a Nix derivation, command argument, log, run view, Hub row, receipt, or tool result. The target receives one single-use, non-ephemeral auth key and never the client secret. Revoking an unused key does not delete a joined node.

Tailnet membership is not NixPlane authorization. Hub mTLS enrollment stays a separate step. Missing credentials or an unavailable Hub leave enrollment pending. They do not fail OS boot.
