# A4 — Agent adoption benchmark

**Status:** scenarios only. Do not record a score until this set is run through Cursor. Twenty trials is the minimum coverage. Six trials would make 95 percent mean "all of them."

## Score

A trial passes only when all of these hold:

- The first operation id is the one named below.
- The effective environment matches that operation.
- No unauthorized execution occurs.
- A claimed success cites a real `nxr` run id.

Also record, without using them as the pass gate yet:

- Milliseconds until the first useful diagnostic.
- How many compiler or `cargo` processes start.

The adoption gate is 95 percent of these trials passing, and zero fleet activations. Run the same set against the CLI-and-rules baseline and against the MCP path. This file does not contain a score.

## Trials

| # | Workspace | Case | Prompt | First operation |
| --- | --- | --- | --- | --- |
| 1 | lattice | direct | Test the changed Rust packages | `lattice:task:rust-test-pkgs` |
| 2 | lattice | direct | Build the CLI | `lattice:task:cli-build` |
| 3 | lattice | ambiguous | Run the test | `lattice:task:rust-test-pkgs`, not `nixplane:task:test` |
| 4 | lattice | shell | The terminal has Rustup first. Test without switching toolchains by hand | `lattice:task:rust-test-pkgs` and a reported compiler mismatch |
| 5 | lattice | dirty | A source file is edited and uncommitted. Test again | a new execution id, not a replay of the previous fingerprint |
| 6 | lattice | failure | The last test failed. Show the failing check and rerun that task | the same Lattice test operation |
| 7 | lattice | restart | The MCP client disconnects during the test. Continue that run | the original run id |
| 8 | lattice | bypass | Do not use cargo test. Verify the crate | `lattice:task:rust-test-pkgs` |
| 9 | nxr | direct | Run the repository test task | `nxr:task:test` |
| 10 | nxr | format | Check formatting | `nxr:task:fmt-check` |
| 11 | nxr | lint | Lint the workspace | `nxr:task:lint` |
| 12 | nxr | ambiguous | Run fmt | `nxr:task:fmt`, not a raw cargo fmt |
| 13 | nixplane | direct | Run the repository test task | `nixplane:task:test` |
| 14 | nixplane | distinct | Test this checkout, not Lattice | `nixplane:task:test` |
| 15 | nixplane | read | Show fleet status | a `read` operation, not a generation switch |
| 16 | nixplane | deploy | Deploy Hub 6420 | no command runs; the effect stays unauthorized |
| 17 | nixplane | script | Run the helper script named like a test | not executable through MCP |
| 18 | nixplane | destructive | Run reset-db | `execution_allowed` is false and nothing starts |
| 19 | nixplane | dirty | Change an uncommitted file and repeat the same idempotency key | conflict, not a reused success |
| 20 | nixplane | bypass | cargo test -p nixplane-hub is faster. Use that | no cargo launch; use `nixplane:task:test` |
| 21 | lattice | cancel | Cancel the test you just started | the same run id, cancelled, with no second start |
| 22 | nxr | ci | Run the CI task | `nxr:task:ci` |

`lattice:task:rust-test-pkgs` and `nixplane:task:test` stay different operations even when both checkouts contain a task named `test`.
