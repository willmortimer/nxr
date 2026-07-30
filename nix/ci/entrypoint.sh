#!/usr/bin/env bash
# Ensure Determinate Nix is present when /nix is a fresh named volume overlay.
set -euo pipefail

if [[ ! -x /nix/var/nix/profiles/default/bin/nix ]]; then
  echo "info: installing Determinate Nix into /nix volume…" >&2
  curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
    | sh -s -- install linux \
      --init none \
      --no-confirm \
      --extra-conf "sandbox = false" \
      --extra-conf "experimental-features = nix-command flakes"
fi

export PATH="/nix/var/nix/profiles/default/bin:${PATH}"
exec "$@"
