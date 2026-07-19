# App Authoring Guide

## 1. Preferred app pattern

Use `writeShellApplication` for shell-backed operations:

```nix
let
  testApp = pkgs.writeShellApplication {
    name = "project-test";

    runtimeInputs = [
      pkgs.cargo
      pkgs.cargo-nextest
    ];

    text = ''
      exec cargo nextest run "$@"
    '';
  };
in {
  apps.test = {
    type = "app";
    program = "${testApp}/bin/project-test";
    meta.description = "Run the test suite";
  };
}
```

Benefits:

- exact executable dependencies;
- standard shell safety checks;
- ordinary flake app output;
- direct `nix run` compatibility;
- correct argument forwarding.

## 2. Use `exec`

The final command should normally use `exec`:

```sh
exec cargo nextest run "$@"
```

This gives the application the expected signal and exit behavior.

Do not omit `"$@"` when the operation should accept user arguments.

## 3. Avoid accidental PATH dependencies

Fragile:

```nix
pkgs.writeShellScriptBin "lint" ''
  eslint .
'';
```

This may work only because `eslint` exists in a development shell.

Preferred:

```nix
pkgs.writeShellApplication {
  name = "lint";
  runtimeInputs = [ pkgs.nodePackages.eslint ];
  text = ''
    exec eslint . "$@"
  '';
};
```

## 4. Environment variables

Required runtime variables should fail clearly:

```sh
: "${DATABASE_URL:?DATABASE_URL must be set}"
```

Secrets should generally come from the caller, not be embedded in the Nix store.

Remember that Nix store content is not a secrets boundary.

## 5. Source tree access

An app launched with `nix run` executes against the caller's working directory.

For source-oriented operations:

```sh
exec cargo test "$@"
```

is usually appropriate.

Do not copy the source into the Nix store merely to run an incremental developer command.

Use checks for sandboxed validation.

## 6. Pair apps with checks

Recommended:

```text
apps.test
checks.test
```

The app optimizes developer iteration.

The check optimizes hermetic validation and CI caching.

They may share command definitions but need not be implemented identically.

## 7. Descriptions

Every user-facing app should include a short imperative description:

```nix
meta.description = "Run the complete test suite";
```

Good:

- Run the test suite
- Start local development services
- Apply database migrations
- Deploy to staging

Weak:

- Tests
- Script
- Development
- Does linting and a bunch of other stuff

## 8. Naming

Use lowercase kebab case:

```text
test
test-unit
test-integration
fmt
fmt-check
lint
dev
db-migrate
db-reset
deploy-staging
```

Avoid:

- spaces;
- uppercase;
- shell punctuation;
- deeply encoded hierarchies;
- names that differ only by obscure abbreviations.

## 9. Default app

A project may define:

```nix
apps.default = apps.test;
```

`nxr` should identify it visually.

The default app should represent the most natural project action, not a destructive command.

## 10. Long-running apps

Development servers should handle signals correctly:

```nix
pkgs.writeShellApplication {
  name = "api-dev";
  runtimeInputs = [
    pkgs.cargo
    pkgs.cargo-watch
  ];

  text = ''
    exec cargo watch -x 'run -p api' "$@"
  '';
};
```

V2 may move watch behavior into runner metadata, but ordinary app-native watch tools remain valid.

## 11. Parallel development groups

V1 can expose one composition app:

```nix
pkgs.writeShellApplication {
  name = "dev";
  runtimeInputs = [ pkgs.process-compose ];
  text = ''
    exec process-compose "$@"
  '';
};
```

V2 can model the services as separate apps and coordinate them in a parallel task group.

## 12. Destructive apps

A destructive app should:

- require explicit flags;
- print its target;
- avoid surprising defaults;
- support non-interactive CI behavior deliberately;
- document required credentials.

Runner metadata may add confirmation, but the app must remain safe enough when invoked directly through `nix run`.

## 13. Shared tool definitions

Avoid copying tool lists manually:

```nix
let
  rustTools = with pkgs; [
    cargo
    cargo-nextest
    clippy
    rustfmt
    rust-analyzer
  ];

  testRuntime = with pkgs; [
    cargo
    cargo-nextest
  ];
in {
  devShells.default = pkgs.mkShell {
    packages = rustTools;
  };

  # test app uses testRuntime
}
```

Share Nix values, but do not make the app enter the development shell.

## 14. Optional nxr helper API

`nxr.lib.mkApp` (alias `mkScriptApp`) wraps `writeShellApplication` into a standard flake app:

```nix
nxr.lib.mkApp {
  inherit pkgs;
  name = "test";
  description = "Run the test suite";
  runtimeInputs = [ pkgs.cargo pkgs.cargo-nextest ];
  text = ''
    exec cargo nextest run "$@"
  '';
}
```

`nxr.lib.mkPackageApp` wraps an existing package binary:

```nix
nxr.lib.mkPackageApp {
  inherit pkgs;
  package = pkgs.cargo;
  bin = "cargo";
  description = "Run Cargo from nixpkgs";
}
```

Both helpers emit the same shape:

```nix
{
  type = "app";
  program = "/nix/store/.../bin/<name>";
  meta.description = "...";
}
```

The helpers are transparent and easy to replace with native Nix.

See [examples/mk-app/](../examples/mk-app/) for a runnable flake using `mkApp`, `mkPackageApp`, and the flake-parts module.

## 15. flake-parts module

Import `nxr.flakeModules.default` and declare apps under `perSystem.nxr.apps`:

```nix
imports = [ nxr.flakeModules.default ];

perSystem = { pkgs, ... }: {
  nxr.apps.test = {
    description = "Run the test suite";
    runtimeInputs = [ pkgs.cargo pkgs.cargo-nextest ];
    script = ''
      exec cargo nextest run "$@"
    '';
  };
};
```

The module emits:

```text
apps.<system>.test
```

and optionally augments:

```text
devShells.<system>.default
```

with `nxr` and shell integration.

No custom runner is required to execute the emitted app.

For optional task orchestration metadata (`perSystem.nxr.tasks` → evaluable
`nxr.<system>`), see [TASKS.md](TASKS.md).
