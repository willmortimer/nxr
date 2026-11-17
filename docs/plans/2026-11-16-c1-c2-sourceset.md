# C1/C2 SourceSet contract

Frozen for NXR `d303cd63fc3a4df31b293bf10b63d38c794d647f` and NixPlane `58bb460012bd8f3bc5ac2253950668a9f477efff`. This document does not change runtime behavior. Both identity modules keep a private copy of the same preimage. Do not hash a new field inside `source_set_digest`.

## V1 digest

`source_set_digest` hashes, in order, length-prefixed fields:

1. `kind`
2. `commit`
3. submodule pins sorted by `path` (`path`, then `commit`)
4. the literal `submodules-end`
5. lockfiles sorted by `path` (`path`, then `digest`)
6. the literal `lockfiles-end`
7. `dirty_snapshot_id`, or an empty string

The golden fixture is `SourceSetV1::fixture("c1-golden")`: kind `fixture`, commit `c1-golden`, no pins, no lockfiles, no dirty id. Its digest is

`src_a47ed7bd8e42c2bc7a4b1911d23f66c805f9aae28fcbe4871df8cb23f9c4ce87`

A second fixture with the same fields in a different declaration order is the same digest. Adding a submodule, lockfile, or dirty id changes it.

## Source set v2

`SourceSetV2` uses `schema_version = "source-set-v2"`. It has `primary: String` and `members: Vec<SourceMemberV2>`. A member has `name`, `logical_path`, `kind`, `commit`, `submodules`, `lockfiles`, and optional `dirty_snapshot_id`.

Refuse a document when:

- `primary` is empty or does not name a member
- any member name is empty
- two members share a name
- `logical_path` is empty, absolute, contains `..`, or contains `:`

`source_set_digest_v2` sorts members by `name` and hashes:

1. the literal `source-set-v2`
2. `primary`
3. for each member: `name`, `logical_path`, `kind`, `commit`, submodule pins sorted by path, `submodules-end`, lockfiles sorted by path, `lockfiles-end`, dirty id or empty
4. the literal `members-end`

The prefix is `src2_`. Machine paths are not inputs. Reordering members does not change the digest. Changing any member commit or dirty id does.

The example primary `lattice` at logical path `lattice` commit `aaa`, plus member `stargate` at `deps/stargate` commit `bbb`, both kind `git` and otherwise empty, digests to

`src2_501f07df541ef192dad7831f68372b7ee9105a55fe1f0c2657913f618783ab7f`

Changing the stargate commit to `ccc` digests to

`src2_2386048beda35c5fc6ca878a72f1851a2450218b6ff41d2624bb5c609630b724`

`project_primary_v1` copies the primary member into a `SourceSetV1`. That V1 digest is not the v2 digest.

A remote start may send that V1 document to a v1 Hub only when every non-primary member has no dirty id and the leaf does not read those members. A dirty non-primary member, or a leaf that reads an undeclared sibling, is refused before `nixplane execution submit`.

## Dirty bytes

`dirty_snapshot_id` stays a digest. The bytes are a dirty manifest:

```json
{"files":[{"path":"a.txt","mode":"100644","kind":"file","bytes_hex":"6869"}]}
```

`digest_dirty_manifest` already hashes file payloads as the sha256 of the decoded `bytes_hex`. Deletions use `kind: "deleted"` and `payload`. Symlinks use `kind: "symlink"` and `payload` as the target.

Planning writes the manifest once under `{run_dir}/source/{source_set_id}/{member}.dirty.json`. Submission reads that file. It does not reread the working tree. A missing file, or a digest that differs from the member's `dirty_snapshot_id`, refuses the start.

When more than one member is dirty, the submitted document is

```json
{"schema":"dirty-bundle-v1","manifests":{"primary":{"files":[]}}}
```

A single-member manifest remains the bare `files` document. The worker applies those bytes. It does not replace a requested dirty snapshot with a clean checkout.

## Remote authority

`.nxr/execution-policy.toml` gains a separate table:

```toml
[[remote_operation]]
id = "workspace:task:marker"
```

`remote_allowed` is true only for ids in that table. `execution_allowed`, workspace trust, and `caller_role` do not grant `operations_start_remote`. An unauthorized call returns `remote execution is not authorized` and does not start `nixplane`.

NXR keeps the run id, idempotency key, and receipt. NixPlane keeps the attempt id and lease generation. The adapter spawns the existing `nixplane execution` client. There is no second HTTP protocol and no NixPlane crate dependency.

## Attempt journal

Before the submit RPC, NXR stores `idempotency_key`, `operation_id`, and `source_set_id` with phase `submitting`. The attempt id is stored before the caller is told the submit succeeded. A lost response submits the same idempotency key again and keeps the attempt Hub already admitted. Reconnect, replay, and cancel use that attempt id. Cancel is complete only when the cancel response says the attempt was cancelled. A result whose lease generation is older than the stored generation is rejected.

## Receipt

The remote receipt records `source_set_id`, `operation_id`, `run_id`, `attempt_id`, `lease_generation`, and `output_digest`. `source_disposition` is `fetched` or `reused`. `execution_disposition` is `fresh` or `existing_artifact`. A verified artifact is not a test qualification.

The live leaf is one `native_command` that writes a small file containing the admitted source digest. Bytes return through `nixplane execution fetch`. The worker is the enrolled nixdev generation on `v0.2.14`, with no restart and no generation switch. Hub 6420 and SYS-5 stay untouched. If that worker cannot claim without a fleet change, the live gate is `not_executed`.
