# Session-local nxr dev-shell integration for Zsh.
#
# Sourced from the flake-parts shellIntegration shellHook. Requires
# NXR_SHELL_INTEGRATION=1 (set by the hook) and an interactive shell.

[[ -n ${NXR_SHELL_INTEGRATION:-} ]] || return 0
[[ -o interactive ]] || return 0
command -v nxr >/dev/null 2>&1 || return 0

if (( $+functions[_nxr] )); then
  return 0
fi

if [[ -n ${NXR_PACKAGE:-} && -r ${NXR_PACKAGE}/share/zsh/site-functions/_nxr ]]; then
  # shellcheck disable=SC1090
  source "${NXR_PACKAGE}/share/zsh/site-functions/_nxr"
else
  # shellcheck disable=SC1090
  source <(command nxr completion zsh 2>/dev/null)
fi
