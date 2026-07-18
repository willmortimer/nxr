# Dynamic nxr app completion for Bash.
#
# Appended to the clap-generated script by `nxr completion bash`. When discovery
# is slow or fails, `nxr __complete apps` returns no candidates and reserved
# command completion from clap still works.

_nxr_complete_apps() {
    command nxr __complete apps 2>/dev/null
}

_nxr_dynamic_apps() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local first="${COMP_WORDS[1]:-}"

    case "$first" in
        completion|__complete|__manpage|help|--help|-h)
            return 1
            ;;
        list|plan|select|doctor|inspect|task|watch|graph)
            return 1
            ;;
        run)
            if ((COMP_CWORD == 2)); then
                COMPREPLY=( $(compgen -W "$(_nxr_complete_apps)" -- "$cur") )
                return 0
            fi
            return 1
            ;;
        ""|-*|--*)
            if ((COMP_CWORD == 1)); then
                COMPREPLY=( $(compgen -W "$(_nxr_complete_apps)" -- "$cur") )
                return 0
            fi
            return 1
            ;;
        *)
            return 1
            ;;
    esac
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
