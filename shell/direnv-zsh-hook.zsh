# Session-local nxr completion for interactive zsh + direnv.
#
# direnv can only export environment variables, not shell functions. This hook
# is sourced from the interactive shell when NXR_COMPLETION_HOOK points here
# (see .envrc). It is idempotent and safe to source repeatedly.

command -v nxr >/dev/null 2>&1 || return 0

# Reload when the hook path or nxr binary identity changes (flake rebuild).
_nxr_comp_key="${NXR_COMPLETION_HOOK:-}:${commands[nxr]}"
if [[ ${NXR_COMPLETION_LOADED:-} == "$_nxr_comp_key" ]] && (( $+functions[_nxr] )); then
  return 0
fi

# Prefer a materialized script from the direnv layout (fast, offline).
if [[ -n ${NXR_COMPLETION_DIR:-} && -r ${NXR_COMPLETION_DIR}/zsh/site-functions/_nxr ]]; then
  # shellcheck disable=SC1090
  source "${NXR_COMPLETION_DIR}/zsh/site-functions/_nxr"
else
  source <(nxr completion zsh)
fi

NXR_COMPLETION_LOADED="$_nxr_comp_key"
unset _nxr_comp_key
