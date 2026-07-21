# Dynamic nxr completion for Zsh.
#
# Appended to the clap-generated script by `nxr completion zsh`. When discovery
# is slow or fails, `nxr __complete <target>` returns no candidates and reserved
# command completion from clap still works.

_nxr_complete_target() {
    local target="$1"
    local -a lines values descriptions
    lines=("${(@f)$(command nxr __complete "$target" 2>/dev/null)}")
    for line in "${lines[@]}"; do
        if [[ "$line" == *$'\t'* ]]; then
            values+=("${line%%$'\t'*}")
            descriptions+=("${line#*$'\t'}")
        else
            values+=("$line")
            descriptions+=("$line")
        fi
    done
    (( ${#values[@]} )) || return 1
    _describe -t "$target" "$target" values descriptions
}

_nxr_complete_apps() {
    _nxr_complete_target apps
}

_nxr_dynamic_apps() {
    local -a reserved
    reserved=(list run plan select doctor completion inspect task watch graph build check shell cache __complete __manpage)

    if (( CURRENT == 2 )) && [[ ${words[2]} != -* ]] && [[ ${reserved[(Ie)${words[2]}]} -eq 0 ]]; then
        _nxr_complete_target apps
        return $?
    fi

    if (( CURRENT == 3 )); then
        case ${words[2]} in
            run)
                _nxr_complete_target apps
                return $?
                ;;
            task|graph|watch)
                _nxr_complete_target tasks
                return $?
                ;;
            build)
                _nxr_complete_target packages
                return $?
                ;;
            check)
                _nxr_complete_target checks
                return $?
                ;;
            shell)
                _nxr_complete_target shells
                return $?
                ;;
        esac
    fi

    # list/inspect --namespace / --category <TAB>
    local i
    for ((i = 2; i < CURRENT; i++)); do
        case ${words[i]} in
            --namespace)
                if (( CURRENT == i + 1 )); then
                    _nxr_complete_target namespaces
                    return $?
                fi
                ;;
            --category)
                if (( CURRENT == i + 1 )); then
                    _nxr_complete_target categories
                    return $?
                fi
                ;;
        esac
    done

    return 1
}

if (( $+functions[_nxr] )); then
    functions[_nxr_clap]=$functions[_nxr]
    _nxr() {
        _nxr_clap "$@"
        local status=$?
        _nxr_dynamic_apps && return 0
        return $status
    }
fi
