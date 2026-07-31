#!/usr/bin/env bash
# Publish wiki/*.md to the GitHub Wiki for willmortimer/nxr.
#
# Prerequisites:
#   - `gh` authenticated with repo scope
#   - Wiki enabled on the repo
#   - First-time bootstrap: if `*.wiki.git` does not exist yet, open
#     https://github.com/willmortimer/nxr/wiki/_new once, create a stub Home,
#     then re-run this script.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/wiki"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/nxr-wiki.XXXXXX")"
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT

owner_repo="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
wiki_url="https://github.com/${owner_repo}.wiki.git"

echo "Publishing $src → $wiki_url"

if ! GIT_TERMINAL_PROMPT=0 gh auth setup-git >/dev/null 2>&1; then
  :
fi

if ! git -C "$tmp" clone --depth 1 "$wiki_url" repo 2>"$tmp/clone.err"; then
  echo "Could not clone wiki remote:" >&2
  cat "$tmp/clone.err" >&2
  echo >&2
  echo "If the wiki was never initialized, create the first page in the browser:" >&2
  echo "  https://github.com/${owner_repo}/wiki/_new" >&2
  echo "Then re-run: $0" >&2
  exit 1
fi

cd "$tmp/repo"
# Drop remote-only pages we manage from wiki/ (keep .git)
find . -maxdepth 1 -type f -name '*.md' -delete
cp -f "$src"/*.md .

git add -A
if git diff --cached --quiet; then
  echo "Wiki already up to date."
  exit 0
fi

git -c user.name="nxr-wiki-publish" -c user.email="wiki-publish@users.noreply.github.com" \
  commit -m "Sync wiki from repository wiki/ directory"
git push origin HEAD
echo "Published."
