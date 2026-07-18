# Dynamic nxr app completion for Zsh.
#
# Appended to the clap-generated script by `nxr completion zsh`. When discovery
# is slow or fails, `nxr __complete apps` returns no candidates and reserved
# command completion from clap still works.

_nxr_complete_apps() {
    local -a lines apps descriptions
    lines=("${(@f)$(command nxr __complete apps 2>/dev/null)}")
    for line in "${lines[@]}"; do
        if [[ "$line" == *$'\t'* ]]; then
            apps+=("${line%%$'\t'*}")
            descriptions+=("${line#*$'\t'}")
        else
            apps+=("$line")
            descriptions+=("$line")
        fi
    done
  (( ${#apps[@]} )) || return 1
    _describe -t apps 'app' apps descriptions
}

_nxr_dynamic_apps() {
    local -a reserved
    reserved=(list run plan select doctor completion inspect task watch graph)

    if (( CURRENT == 2 )) && [[ ${words[2]} != -* ]] && [[ ${reserved[(Ie)${words[2]}]} -eq 0 ]]; then
        _nxr_complete_apps
        return $?
    fi

    if (( CURRENT == 3 )) && [[ ${words[2]} == run ]]; then
        _nxr_complete_apps
        return $?
    fi

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
