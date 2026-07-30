#!/usr/bin/env bash
# Prepare (and optionally create) a SemVer release tag for nxr.
#
# Default is dry-run: verify gates inputs and print the exact git commands.
# Does not clear the operator's git signing config (needed for `git tag -s`).
#
# Usage:
#   nix run .#release
#   nix run .#release -- --execute          # requires host+linux gate stamps
#   nix run .#release -- --execute --yes
#   nix run .#release -- --execute --skip-gates   # break-glass only
#   nxr task release                        # runs host `ci` graph, then dry-run
#   nxr task release -- --execute
set -euo pipefail

execute=0
assume_yes=0
skip_remote_check=0
skip_gates=0

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
gates_sh="$script_dir/release-gates.sh"

usage() {
  cat <<'EOF'
Usage: release [--execute] [--yes] [--skip-remote-check] [--skip-gates]

  (default)     Check repo state and print git tag/push commands.
  --execute     Create annotated/signed tag and push to origin.
                Requires .nxr/release-gates/{host,linux}.<HEAD> stamps from:
                  nix run .#ci-gate
                  nix run .#ci-gate-linux
  --yes         With --execute, skip the interactive confirmation.
  --skip-remote-check
                Do not require origin/main containment / remote tag probe.
  --skip-gates  Break-glass: allow --execute without gate stamps (loud warning).

Environment:
  NXR_RELEASE_REMOTE   Remote name (default: origin)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --execute)
      execute=1
      shift
      ;;
    --yes | -y)
      assume_yes=1
      shift
      ;;
    --skip-remote-check)
      skip_remote_check=1
      shift
      ;;
    --skip-gates)
      skip_gates=1
      shift
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

remote="${NXR_RELEASE_REMOTE:-origin}"

if [[ ! -f Cargo.toml || ! -f flake.nix ]]; then
  echo "error: run from the nxr repository root" >&2
  exit 1
fi

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is required" >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: not a git repository" >&2
  exit 1
fi

# Workspace package version (Cargo.toml [workspace.package] or root package).
version="$(
  awk '
    $0 == "[workspace.package]" { in_ws=1; next }
    $0 ~ /^\[/ { in_ws=0 }
    in_ws && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' Cargo.toml
)"
if [[ -z "$version" ]]; then
  version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
fi
if [[ -z "$version" ]]; then
  echo "error: could not read version from Cargo.toml" >&2
  exit 1
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-].*)?$ ]]; then
  echo "error: version '$version' is not SemVer-like" >&2
  exit 1
fi

tag="v${version}"

echo "info: release candidate ${tag} (Cargo.toml version=${version})"

# --- cleanliness ---
if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is dirty; commit or stash before releasing" >&2
  git status --short >&2
  exit 1
fi
echo "ok: working tree clean"

branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" != "main" ]]; then
  echo "warning: current branch is '$branch' (expected main)" >&2
fi

# --- version sync ---
if command -v nix >/dev/null 2>&1; then
  system="$(nix eval --raw --impure --expr 'builtins.currentSystem' 2>/dev/null || true)"
  if [[ -n "$system" ]]; then
    pkg_version="$(nix eval --raw ".#packages.${system}.nxr.version" 2>/dev/null || true)"
    if [[ -n "$pkg_version" && "$pkg_version" != "$version" ]]; then
      echo "error: flake package version ($pkg_version) != Cargo.toml ($version)" >&2
      exit 1
    fi
    if [[ -n "$pkg_version" ]]; then
      echo "ok: flake package version matches ($pkg_version)"
    else
      echo "warning: could not eval flake package version; skipping that check" >&2
    fi
  fi
else
  echo "warning: nix not on PATH; skipping flake package version check" >&2
fi

if [[ -f CHANGELOG.md ]]; then
  if grep -qE "^## \[${version}\]" CHANGELOG.md; then
    echo "ok: CHANGELOG.md has ## [${version}]"
  elif grep -qE '^## \[Unreleased\]' CHANGELOG.md; then
    echo "warning: CHANGELOG.md still has [Unreleased] and no ## [${version}] section" >&2
    echo "         cut the release notes before tagging" >&2
  else
    echo "warning: CHANGELOG.md has neither [Unreleased] nor ## [${version}]" >&2
  fi
fi

# --- tag collision ---
if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null 2>&1; then
  echo "error: local tag ${tag} already exists" >&2
  exit 1
fi
echo "ok: local tag ${tag} does not exist"

if [[ "$skip_remote_check" -eq 0 ]]; then
  if git remote get-url "$remote" >/dev/null 2>&1; then
    if git ls-remote --tags "$remote" "refs/tags/${tag}" 2>/dev/null | grep -q .; then
      echo "error: remote ${remote} already has tag ${tag}" >&2
      exit 1
    fi
    echo "ok: remote ${remote} does not have ${tag}"

    # HEAD should be contained in origin/main when releasing from main.
    if git rev-parse -q --verify "${remote}/main" >/dev/null 2>&1; then
      if git merge-base --is-ancestor HEAD "${remote}/main" 2>/dev/null; then
        echo "ok: HEAD is contained in ${remote}/main"
      else
        echo "warning: HEAD is not an ancestor of ${remote}/main (push main first?)" >&2
      fi
    else
      echo "warning: ${remote}/main not found locally; fetch before releasing" >&2
    fi
  else
    echo "warning: remote '${remote}' not configured; skipping remote checks" >&2
  fi
fi

# --- dogfood gate stamps ---
echo
echo "Local dogfood gate stamps (HEAD=$(git rev-parse --short HEAD)):"
"$gates_sh" status
echo

tag_cmd=(git tag -s "$tag" -m "nxr ${version}")
push_cmd=(git push "$remote" "refs/tags/${tag}")

echo "Planned commands:"
printf '  %q' "${tag_cmd[@]}"
printf '\n'
printf '  %q' "${push_cmd[@]}"
printf '\n'

if [[ "$execute" -eq 0 ]]; then
  echo
  echo "Dry-run only. Re-run with --execute after:"
  echo "  nix run .#ci-gate"
  echo "  nix run .#ci-gate-linux"
  exit 0
fi

if [[ "$skip_gates" -eq 1 ]]; then
  echo "WARNING: --skip-gates set; tagging WITHOUT host+linux dogfood stamps" >&2
else
  "$gates_sh" require
fi

if [[ "$assume_yes" -eq 0 ]]; then
  if [[ ! -t 0 ]]; then
    echo "error: --execute needs a TTY for confirmation (or pass --yes)" >&2
    exit 1
  fi
  read -r -p "Create and push signed tag ${tag}? [y/N] " answer
  case "$answer" in
    y | Y | yes | YES) ;;
    *)
      echo "aborted"
      exit 1
      ;;
  esac
fi

# Prefer signed tags; fall back to annotated if signing is unavailable.
if ! "${tag_cmd[@]}"; then
  echo "warning: git tag -s failed; falling back to annotated unsigned tag" >&2
  git tag -a "$tag" -m "nxr ${version}"
fi

"${push_cmd[@]}"
echo "ok: pushed ${tag} → ${remote} (release.yml + compat.yml should start)"
