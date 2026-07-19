# Session-local nxr dev-shell integration for Bash.
#
# Sourced from the flake-parts shellIntegration shellHook. Requires
# NXR_SHELL_INTEGRATION=1 (set by the hook) and an interactive shell.

[[ -n ${NXR_SHELL_INTEGRATION:-} ]] || return 0
[[ $- == *i* ]] || return 0
command -v nxr >/dev/null 2>&1 || return 0

if declare -F _nxr >/dev/null 2>&1; then
  return 0
fi

if [[ -n ${NXR_PACKAGE:-} && -r ${NXR_PACKAGE}/share/bash-completion/completions/nxr ]]; then
  # shellcheck disable=SC1090
  source "${NXR_PACKAGE}/share/bash-completion/completions/nxr"
else
  # shellcheck disable=SC1090
  source <(command nxr completion bash 2>/dev/null)
fi
