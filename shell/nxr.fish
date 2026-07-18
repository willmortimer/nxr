# Dynamic nxr app completion for Fish.
#
# Appended to the clap-generated script by `nxr completion fish`. When discovery
# is slow or fails, `nxr __complete apps` returns no candidates and reserved
# command completion from clap still works.

function __nxr_complete_apps
    command nxr __complete apps 2>/dev/null
end

function __nxr_should_complete_apps
    set -l tokens (commandline -opc)
    set -e tokens[1]
    if test (count $tokens) -eq 0
        return 0
    end
    if test (count $tokens) -eq 1; and contains -- $tokens[1] run
        return 0
    end
    return 1
end

complete -c nxr -n __nxr_should_complete_apps -a "(__nxr_complete_apps)"
