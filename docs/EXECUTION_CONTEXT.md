# Execution context and ecosystem expansion

**Status:** design contract for post-2.5 work. Not yet implemented.
**Companion:** [ROADMAP.md](ROADMAP.md) (scheduling), [CONTRACT_SUMMARY.md](CONTRACT_SUMMARY.md) (invariants), [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md) (inheritance rules).

## Product identity

`nxr` should become the **execution-context and orchestration layer** for standard flake outputs—not a replacement for development shells, direnv, devenv, Home Manager, or secret stores.

The stronger product statement:

> A Nix-native command, workflow, and execution-context runner for standard flake outputs.

It understands:

| Concern | nxr role |
|---|---|
| What to run | apps and tasks |
| Where to run | working directory (and eventually remote context) |
| What tools apply | packages and named development shells |
| What may enter | environment and secret policy |
| What came before | DAG dependencies and structured task outputs |
| When it is ready | process health and dependency states |
| What Nix produced | packages, checks, VMs, containers, configurations |

It still does **not** own:

- language / toolchain installation;
- development-shell construction;
- secret storage or encryption;
- system configuration / activation;
- container or VM runtimes;
- deployment state reconciliation.

That expansion is meaningful, not bloat. The unifying feature is **execution context**: nxr resolves the app, shell, environment, secret requirements, dependencies, and supervision policy, then delegates primitives to Nix and the appropriate provider.

## Layer ownership

| Layer | Owns |
|---|---|
| **Nix flakes** | Packages, apps, checks, development shells, configurations, artifacts |
| **direnv / nix-direnv** | Automatic shell activation and cached shell environments |
| **devenv / numtide/devshell** | Optional richer development-environment authoring |
| **SOPS / sops-nix / SecretSpec** | Secret encryption, storage, and provisioning |
| **Home Manager** | User-level installation, global configuration, shell hooks, trust policy |
| **nxr** | Target discovery, execution contexts, DAGs, environment policy, runtime secret delivery, process supervision |

This remains consistent with current invariants:

- standard flake apps stay valid leaf operations;
- development shells remain ordinary flake outputs;
- Nix owns builds, stores, and remote builders;
- secret **values** must never appear in public metadata or plans.

---

## 1. Development-shell execution (keep shells optional)

### Current foundation (shipped)

```bash
nxr shell backend
nxr --shell backend test
nxr --shell backend task integration
```

Smart nesting detects `NXR_DEV_SHELL` and skips re-entering the same shell unless
`--shell-mode always` is set.

Apps should normally remain independently runnable:

```bash
nix run .#test
```

Shell wrapping is for workflows that need SDK variables, native library paths, or
other shell-provided configuration—not a hidden prerequisite for every app.

### Ergonomic shorthand: `nxr in`

The prefix flag is precise but awkward for frequent use. Add:

```bash
nxr in backend test
nxr in backend task integration
nxr in release deploy
```

`in` is preferred over cryptic syntax such as `@backend` because it is
discoverable, completion-friendly, and unambiguous.

Keep both forms:

```bash
nxr --shell backend test    # stable low-level form
nxr in backend test         # ergonomic form
```

**Do not allow** `nxr test --shell backend`. After the app name, arguments belong
to the app; moving runner options there would weaken the parsing boundary
([CLI_CONTRACT.md](CLI_CONTRACT.md)).

Treat `nxr in <shell> <target>` as an anonymous **shell-only** execution context.

### Task-level shell association (schema v2)

Ordinary apps stay shell-free by default. Tasks may declare a shell:

```nix
perSystem.nxr.tasks.integration = {
  app = "test-integration";
  shell = "backend";
};
```

Then `nxr task integration` uses `devShells.backend` automatically.

**Precedence:**

```text
CLI --shell / nxr in
  overrides task / CLI context
    overrides task.shell
      otherwise no shell wrapper
```

An explicitly active matching shell still causes smart wrapping to be skipped.

### Named execution contexts

A shell alone becomes insufficient for release workflows. Contexts bundle:

- named development shell;
- environment policy (inherit / clean / set / keep);
- non-secret environment values;
- secret requirements (references only);
- confirmation policy;
- working-directory policy (where needed).

```nix
perSystem.nxr.contexts = {
  backend = {
    shell = "backend";
    environment = {
      mode = "inherit";
      set.RUST_LOG = "debug";
    };
  };

  release = {
    shell = "release";
    environment = {
      mode = "clean";
      keep = [ "HOME" "SSH_AUTH_SOCK" ];
      set.RELEASE_CHANNEL = "stable";
    };
    confirm = true;
  };
};

perSystem.nxr.tasks.deploy = {
  app = "deploy";
  context = "release";
};
```

CLI:

```bash
nxr context release deploy
nxr context backend task integration
nxr in backend test          # anonymous shell-only context
```

Machine-specific defaults belong in **Home Manager**, not in the repository’s
public flake metadata. Prefer explicit `nxr context work …` / `nxr context ci …`
over implicit hostname/username profile selection.

### One-shell DAG optimization

Today the selected shell and run-wide environment policy are prepared per node.
When **every** node in a plan resolves to the same context, prefer entering that
shell once and running the scheduler inside it:

```text
# Conceptual today (per-node wrap)
nxr
 ├─ nix develop .#backend -c nix run .#fmt
 ├─ nix develop .#backend -c nix run .#test
 └─ nix develop .#backend -c nix run .#integration

# Preferred (same context)
nix develop .#backend
 └─ nxr internal scheduler
     ├─ nix run .#fmt
     ├─ nix run .#test
     └─ nix run .#integration
```

Mixed-context DAGs continue wrapping nodes independently until a robust
shell-environment cache exists.

**Do not** eagerly parse `nix print-dev-env --json` and reconstruct every
development shell inside nxr. That interface is still experimental, and
faithfully reproducing shell hooks / semantics becomes environment-manager
territory (direnv / devenv / Home Manager).

---

## 2. Better direnv integration (do not replace direnv)

### What nix-direnv already owns

- cached `use flake`;
- `nix print-dev-env` integration;
- GC roots for shell dependencies;
- automatic tracking of `.envrc`, `flake.nix`, and `flake.lock`;
- fallback to the previous working shell when a new shell fails evaluation.

nxr must not absorb that responsibility.

### `nxr envrc` generator

Provide a generator, not a proprietary activation mechanism:

```bash
nxr envrc
nxr envrc --shell backend
nxr envrc --write
```

Output:

```bash
use flake
```

or:

```bash
use flake .#backend
```

`--write` must be explicit and **refuse** to overwrite an existing `.envrc`
without a force flag. Generated content may include completion-hook setup where
applicable.

### `nxr doctor env`

Expand diagnostics:

```bash
nxr doctor env
```

Inspect at least:

- direnv installed;
- nix-direnv loaded;
- `.envrc` present;
- active development shell;
- requested development shell;
- `NIX_DIRENV_DID_FALLBACK` set;
- `flake.nix` / `flake.lock` tracked;
- nxr shell integration loaded;
- secret provider availability.

Especially useful:

```text
warning: nix-direnv loaded its previous working environment
         because the current devShell failed evaluation
```

Explain friction without becoming the activation system.

### Secrets must not ride on `.envrc`

Entering an arbitrary repository must not automatically grant every process in
that shell access to important credentials.

Direnv’s approval model protects `.envrc` execution, but once a secret is
exported into the shell, editors, language servers, subprocesses, and all DAG
nodes inherit it unless further restricted.

**Policy:** use direnv for ordinary development-shell activation; use nxr
execution contexts for secret-bearing process launches.

### What to take from numtide/devshell

Useful ideas:

- optional shell-entry command menu;
- descriptions and categories for named shells;
- concise “available project commands” display;
- shell lifecycle metadata;
- clearer inheritance between package build inputs and developer tools.

Opt-in greeting:

```nix
perSystem.nxr.shellIntegration = {
  enable = true;
  devShells = [ "default" "backend" ];
  showMenu = true;
};
```

On entry:

```text
backend development shell
apps:
  test              Run backend tests
  db-migrate        Apply local migrations
tasks:
  ci                Run backend validation
  dev               Start backend services
```

**Do not** copy devshell’s environment or service-definition system. The command
catalog is already nxr’s strength; shell authoring remains someone else’s job.

### What to take from devenv

Highest-value borrowings (see also [ECOSYSTEM_SYNTHESIS.md](ECOSYSTEM_SYNTHESIS.md)):

#### Structured task inputs and outputs

```nix
nxr.tasks.build = {
  app = "build";
  outputs = {
    manifest = { type = "json"; };
    bundle = { type = "path"; };
  };
};

nxr.tasks.publish = {
  app = "publish";
  dependsOn = [ "build" ];
  inputs = {
    manifest.from = "build.manifest";
    bundle.from = "build.bundle";
  };
};
```

Runtime contract (illustrative):

```text
NXR_TASK_INPUT_FILE
NXR_TASK_OUTPUT_FILE
NXR_TASK_ARTIFACT_DIR
```

Distinguish **artifacts** from **environment exports**. A child cannot export an
environment variable back into nxr; a structured output channel is required.

#### Dependency states

```nix
dependsOn = [
  "database@ready"
  "generate@succeeded"
  "cleanup@completed"
];
```

Borrow devenv’s distinction among started / ready / succeeded / completed once
process nodes exist.

#### Status and freshness checks

```nix
nxr.tasks.codegen = {
  app = "codegen";
  freshness = {
    inputs = [ "schemas/**" ];
    outputs = [ "src/generated/**" ];
  };
};
# or
statusApp = "codegen-current";
```

Extends affected analysis into local incremental execution.

#### Explicit contexts, not host profiles

Devenv profiles can vary environments by hostname or username. nxr should use
**named contexts** selected explicitly (`nxr context work …`). Machine-specific
defaults live in Home Manager, not public flake metadata.

---

## 3. First-class secret delivery

### Problem

```bash
sops exec-env ... -- nxr task deploy
```

is a bad **user-facing** workflow for important operations, but a good
**underlying** security mechanism. SOPS `exec-env` / `exec-file` scopes
plaintext to a child process; `exec-file` can use a FIFO and clean up after
execution.

The improvement is `nxr task deploy` with nxr resolving and delivering required
secrets internally—without becoming a secret store.

### Separate declarations from provider bindings

**Project declares what it needs** (references only):

```nix
perSystem.nxr.contexts.release = {
  shell = "release";
  environment = {
    mode = "clean";
    keep = [ "HOME" "SSH_AUTH_SOCK" ];
    set.DEPLOY_ENVIRONMENT = "production";
  };
  secrets = {
    CLOUDFLARE_API_TOKEN = {
      ref = "openseat/prod/cloudflare-token";
      delivery = "env";
    };
    KUBECONFIG = {
      ref = "openseat/prod/kubeconfig";
      delivery = "file";
    };
  };
  confirm = true;
};

perSystem.nxr.tasks.deploy = {
  app = "deploy";
  context = "release";
};
```

**Developer / CI declares how references are provisioned** (Home Manager or
user config—never flake evaluation):

```nix
programs.nxr.secretBindings = {
  "openseat/prod/cloudflare-token" = {
    provider = "sops-nix";
    path = config.sops.secrets.openseat-cloudflare-token.path;
  };
  "openseat/prod/kubeconfig" = {
    provider = "sops";
    file = "/Users/will/.config/openseat/prod.sops.yaml";
    key = "kubeconfig";
  };
};
```

No secret **values** enter flake evaluation.

### Built-in providers (start)

| Provider | Role |
|---|---|
| `env` | Read an existing caller variable |
| `file` | Read or pass an existing runtime file |
| `sops` | Decrypt a named key from an encrypted SOPS file |
| `sops-nix` | Pass an activation-provisioned runtime path |

Optional later: a SecretSpec-compatible adapter. SecretSpec’s separation
(project declares logical secrets; each environment chooses a provider) is the
right model; nxr should be structurally compatible without mandating SecretSpec
as a dependency. Prefer loading secrets only into the process that needs them,
not the whole development shell.

### Delivery modes

| Mode | Behavior |
|---|---|
| `env` | Inject value into the selected child process only |
| `file` | Pass a protected file path (`NAME_FILE` or `NAME`) |
| `stdin` | Deliver over stdin when the app explicitly supports it |
| `fd` | Possible later on Unix |

Prefer **file** delivery for important credentials when the target supports it
(`AWS_WEB_IDENTITY_TOKEN_FILE`, `KUBECONFIG`, `DOCKER_CONFIG`,
`GOOGLE_APPLICATION_CREDENTIALS`). Environment delivery is sometimes unavoidable
but must not be the default merely because it is convenient.

### Runtime rules

Secret-bearing contexts must obey:

1. Resolve secrets immediately before spawning the node.
2. Never resolve during `list`, `graph`, `plan`, completion, or dry-run.
3. Never serialize values into JSON plans or events.
4. Never include values in command-line arguments.
5. Never cache decrypted values.
6. Inject only into nodes that explicitly declare them.
7. Clean up temporary files on success, failure, timeout, and signals.
8. Prefer clean environment by default for high-sensitivity contexts.
9. Redact known values if they accidentally appear in runner diagnostics.
10. Do not make dependency nodes inherit root secrets automatically.

Plans may show:

```json
{
  "context": "release",
  "secrets": [
    {
      "name": "CLOUDFLARE_API_TOKEN",
      "ref": "openseat/prod/cloudflare-token",
      "delivery": "env",
      "value": "<runtime>"
    }
  ]
}
```

Never the actual value.

### Project trust boundary (mandatory)

Without authorization, a malicious repository could declare:

```nix
secrets.GITHUB_TOKEN.ref = "github/personal-token";
```

and exfiltrate a binding.

Before a project receives a secret binding, require approval similar in spirit to
direnv:

```text
Project github.com/willmortimer/openseat requests:
  openseat/prod/cloudflare-token
  openseat/prod/kubeconfig
Allow: once / always / deny
```

Approvals scoped by:

- canonical repository identity;
- Git remote where available;
- secret reference;
- context;
- optionally task name.

Home Manager should support declarative pre-authorization for owned repositories
and machines.

### Schema versioning (critical)

Do **not** silently add execution-affecting fields to task schema v1.

Current schema v1 tolerates unknown task fields; the Rust deserializer ignores
them. An older nxr could therefore read:

```nix
nxr.tasks.deploy = {
  app = "deploy";
  context = "release";
};
```

ignore `context`, and run **without** the intended security policy.

That is unacceptable for security and execution metadata.

Introduce **task document schema v2** for:

- contexts;
- secret requirements;
- task inputs / outputs;
- dependency states;
- confirmation requirements;
- process / readiness semantics.

Unknown **listing** metadata may remain additive. Unknown **security or
execution** metadata must cause the runner to **reject** the document, not
silently ignore it.

---

## 4. Home Manager integration (major feature)

`nxr` is a user-level development CLI. Home Manager is the correct place for
permanent installation, shell integration, configuration files,
environment-independent defaults, and machine-specific secret bindings.

Export:

```text
homeManagerModules.default
```

Illustrative module surface:

```nix
programs.nxr = {
  enable = true;
  package = inputs.nxr.packages.${pkgs.system}.default;

  settings = {
    defaultJobs = 8;
    shellMode = "smart";
    output = "live";
    color = "auto";
  };

  shellIntegration = {
    bash = true;
    zsh = true;
    fish = true;
  };

  direnvIntegration = {
    enable = true;
    nixDirenv = true;
  };

  trustedProjects = {
    "github.com/willmortimer/openseat" = {
      allowedSecrets = [
        "openseat/dev/database-url"
        "openseat/prod/cloudflare-token"
      ];
    };
  };

  secretBindings = {
    "openseat/dev/database-url" = {
      provider = "sops-nix";
      path = config.sops.secrets.openseat-dev-database-url.path;
    };
  };
};
```

The module should:

- add nxr to `home.packages`;
- install Bash / Zsh / Fish completion;
- install global shell hooks;
- write `$XDG_CONFIG_HOME/nxr/config.toml`;
- optionally enable `programs.direnv` and `programs.direnv.nix-direnv`;
- store user defaults, project trust decisions, and provider paths;
- **never** store secret values.

### Home Manager + sops-nix

sops-nix decrypts during system or user **activation**, not flake evaluation.
System paths under `/run/secrets` (or Home Manager user-runtime paths) map cleanly:

```text
sops-nix activation  →  protected runtime file
Home Manager nxr     →  logical ref → runtime path
nxr task             →  path only to the required node
```

On macOS, nix-darwin + sops-nix is the stronger system-level route. Direct SOPS
remains useful for repositories and machines not managed through nix-darwin.

### What Home Manager must not do

- Do **not** export complete `homeConfigurations` from the nxr repository.
  Export the reusable module; users import it into their own configuration.
- Do **not** make the module required. `nix run github:…/nxr` and project-local
  development-shell installation remain valid.

---

## 5. Other Nix ecosystem features worth supporting

### `nxr fmt`

Thin wrapper around `nix fmt` / the flake `formatter` output:

```bash
nxr fmt
nxr fmt path/to/file.nix
```

Symmetric with existing packages / checks / shells commands.

### Arbitrary installables for build and inspection

Escape hatch beyond package leaf names:

```bash
nxr build .#packages.aarch64-darwin.desktop
nxr build .#nixosConfigurations.dev.config.system.build.vm
nxr build .#darwinConfigurations.work.system
nxr build .#homeConfigurations.will.activationPackage

nxr build --attr nixosConfigurations.dev.config.system.build.vm
```

### Configuration adapters: inspect and build, not manage

Read-only catalog:

```bash
nxr list configurations
nxr inspect configuration devcell
nxr build configuration devcell
```

Recognize conventional outputs:

- `nixosConfigurations`
- `darwinConfigurations`
- `homeConfigurations`

**Do not** make nxr responsible for `nixos-rebuild switch`, `darwin-rebuild
switch`, or `home-manager switch`. Those have host privilege, activation,
rollback, and lifecycle semantics. A project may expose them as an explicit
flake app when desired.

### Containers and VMs as artifacts

Prefer packages:

```nix
packages.container = pkgs.dockerTools.buildLayeredImage { … };
packages.vm = nixosConfigurations.dev.config.system.build.vm;
packages.iso = nixosConfigurations.appliance.config.system.build.isoImage;
```

```bash
nxr build container
nxr build vm
nxr build iso
```

Optional presentation metadata:

```nix
nxr.artifacts.dev-vm = {
  installable = "packages.${system}.vm";
  kind = "vm";
  description = "Development NixOS VM";
};
```

nxr must not become a container builder, container runtime, VM manager, or NixOS
image framework.

### Process nodes and readiness (after task I/O)

```nix
nxr.processes = {
  database = {
    app = "postgres-dev";
    readiness.tcp.port = 5432;
    restart = "on-failure";
  };
  api = {
    app = "api-dev";
    dependsOn = [ "database@ready" ];
    readiness.http = {
      url = "http://127.0.0.1:8080/health";
    };
  };
};
```

```bash
nxr up
nxr up api
nxr status
nxr logs api
```

This remains a runner responsibility: supervise apps, readiness, logs, shutdown,
and dependency states. **Do not** add fifty built-in service modules (Postgres,
Redis, …). A service remains a flake app or is supplied by devenv.

### treefmt and git-hooks via standard outputs

Do not create another formatter or hook engine.

- recognize the flake formatter;
- recognize checks from treefmt-nix / git-hooks.nix;
- explain them in `inspect` and `doctor`;
- expose via `nxr fmt` and `nxr check`.

### Cache and remote-builder diagnostics

Nix owns substituters, binary caches, remote stores, and remote builders. nxr
must not proxy them.

```bash
nxr doctor cache
nxr doctor builders
```

Report configured substituters, trusted public keys, flake `nixConfig`
acceptance, remote builder availability, missing cache signatures, and whether a
build will be local or delegated where determinable.

---

## Scheduling summary

See [ROADMAP.md](ROADMAP.md) for the ordered release plan:

| Release | Theme |
|---|---|
| **2.5** | Affected execution (already planned) |
| **2.6** | Ecosystem ergonomics (HM module, `fmt`, `in`, `envrc`, doctor env/cache, installables, adapters, shell menu) |
| **3.0** | Execution-context schema v2 (contexts, secrets, I/O, dependency states, strict rejection) |
| **3.1** | Process workflows (`up` / `status` / `logs`, readiness) |
| **Later** | Artifact restoration, task result caching, remote workspace, daemon/control plane |

Speculative control-plane ideas beyond that remain in
[ideas/FUTURE_CONTROL_PLANE.md](ideas/FUTURE_CONTROL_PLANE.md).
