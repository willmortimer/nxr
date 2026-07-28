# Dynamic nxr completion for Bash.
#
# Appended to the clap-generated script by `nxr completion bash`. When discovery
# is slow or fails, `nxr __complete <target>` returns no candidates and reserved
# command completion from clap still works.

# Resolve the nearest flake root without spawning nxr.
_nxr_flake_root() {
    local dir="$PWD"
    while [[ -n "$dir" && "$dir" != "/" ]]; do
        if [[ -f "$dir/flake.nix" ]]; then
            if command -v realpath >/dev/null 2>&1; then
                realpath "$dir"
            else
                printf '%s\n' "$dir"
            fi
            return 0
        fi
        dir="${dir%/*}"
    done
    return 1
}

# Resolve a running nxrd socket when daemon connect is enabled.
_nxr_daemon_socket() {
    if [[ -n "${NXR_DAEMON_SOCKET:-}" ]]; then
        printf '%s\n' "$NXR_DAEMON_SOCKET"
        return 0
    fi
    case "${NXR_DAEMON:-}" in
        off|0|false|no|OFF|FALSE|NO) return 1 ;;
    esac
    if [[ -n "${XDG_RUNTIME_DIR:-}" && -S "${XDG_RUNTIME_DIR}/nxr/nxrd.sock" ]]; then
        printf '%s\n' "${XDG_RUNTIME_DIR}/nxr/nxrd.sock"
        return 0
    fi
    local tmp="${TMPDIR:-/tmp}"
    local user="${USER:-${UID:-user}}"
    if [[ -S "${tmp}/nxr-${user}/nxrd.sock" ]]; then
        printf '%s\n' "${tmp}/nxr-${user}/nxrd.sock"
        return 0
    fi
    return 1
}

# Invoke nxr with optional daemon socket forwarding (execution still via nxr).
_nxr_invoke() {
    local socket
    if socket="$(_nxr_daemon_socket)"; then
        NXR_DAEMON_SOCKET="$socket" command nxr "$@"
    else
        command nxr "$@"
    fi
}

_nxr_complete_target() {
    _nxr_invoke __complete "$1" 2>/dev/null
}

_nxr_complete_apps() {
    _nxr_complete_target apps
}

# Resolve which __complete target applies at the current cursor position.
# Prints the target name, or nothing when clap should handle completion alone.
_nxr_dynamic_target() {
    local first="${COMP_WORDS[1]:-}"
    local second="${COMP_WORDS[2]:-}"

    case "$first" in
        completion|__complete|__manpage|help|--help|-h)
            return 1
            ;;
        run)
            if ((COMP_CWORD == 2)); then
                echo apps
                return 0
            fi
            return 1
            ;;
        task|graph|watch)
            if ((COMP_CWORD == 2)); then
                echo tasks
                return 0
            fi
            return 1
            ;;
        build)
            if ((COMP_CWORD == 2)); then
                echo packages
                return 0
            fi
            return 1
            ;;
        check)
            if ((COMP_CWORD == 2)); then
                echo checks
                return 0
            fi
            return 1
            ;;
        shell)
            if ((COMP_CWORD == 2)); then
                echo shells
                return 0
            fi
            return 1
            ;;
        list|inspect)
            if ((COMP_CWORD >= 2)); then
                case "$second" in
                    --namespace)
                        if ((COMP_CWORD == 3)); then
                            echo namespaces
                            return 0
                        fi
                        ;;
                    --category)
                        if ((COMP_CWORD == 3)); then
                            echo categories
                            return 0
                        fi
                        ;;
                esac
                # Also handle `list --namespace <TAB>` when flag is not second word.
                local i
                for ((i = 1; i < COMP_CWORD; i++)); do
                    case "${COMP_WORDS[i]}" in
                        --namespace)
                            if ((COMP_CWORD == i + 1)); then
                                echo namespaces
                                return 0
                            fi
                            ;;
                        --category)
                            if ((COMP_CWORD == i + 1)); then
                                echo categories
                                return 0
                            fi
                            ;;
                    esac
                done
            fi
            return 1
            ;;
        plan|select|doctor|cache|fmt|envrc|in)
            return 1
            ;;
        ""|-*|--*)
            if ((COMP_CWORD == 1)); then
                echo apps
                return 0
            fi
            return 1
            ;;
        *)
            return 1
            ;;
    esac
}

_nxr_dynamic_apps() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local target
    target="$(_nxr_dynamic_target)" || return 1
    [[ -n "$target" ]] || return 1
    COMPREPLY=( $(compgen -W "$(_nxr_complete_target "$target")" -- "$cur") )
    return 0
}

if declare -F _nxr >/dev/null; then
    eval "$(declare -f _nxr | sed 's/^function _nxr/function __nxr_clap/; s/^_nxr/__nxr_clap/')"
    _nxr() {
        __nxr_clap "$@"
        local status=$?
        _nxr_dynamic_apps && return 0
        return "$status"
    }
fi
