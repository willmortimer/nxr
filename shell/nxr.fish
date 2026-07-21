# Dynamic nxr completion for Fish.
#
# Appended to the clap-generated script by `nxr completion fish`. When discovery
# is slow or fails, `nxr __complete <target>` returns no candidates and reserved
# command completion from clap still works.

function __nxr_complete_apps
    command nxr __complete apps 2>/dev/null
end

function __nxr_complete_tasks
    command nxr __complete tasks 2>/dev/null
end

function __nxr_complete_packages
    command nxr __complete packages 2>/dev/null
end

function __nxr_complete_checks
    command nxr __complete checks 2>/dev/null
end

function __nxr_complete_shells
    command nxr __complete shells 2>/dev/null
end

function __nxr_complete_namespaces
    command nxr __complete namespaces 2>/dev/null
end

function __nxr_complete_categories
    command nxr __complete categories 2>/dev/null
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

function __nxr_should_complete_tasks
    set -l tokens (commandline -opc)
    set -e tokens[1]
    if test (count $tokens) -eq 1; and contains -- $tokens[1] task graph watch
        return 0
    end
    return 1
end

function __nxr_should_complete_packages
    set -l tokens (commandline -opc)
    set -e tokens[1]
    if test (count $tokens) -eq 1; and contains -- $tokens[1] build
        return 0
    end
    return 1
end

function __nxr_should_complete_checks
    set -l tokens (commandline -opc)
    set -e tokens[1]
    if test (count $tokens) -eq 1; and contains -- $tokens[1] check
        return 0
    end
    return 1
end

function __nxr_should_complete_shells
    set -l tokens (commandline -opc)
    set -e tokens[1]
    if test (count $tokens) -eq 1; and contains -- $tokens[1] shell
        return 0
    end
    return 1
end

function __nxr_should_complete_namespaces
    set -l tokens (commandline -opc)
    set -e tokens[1]
    contains -- --namespace $tokens; or return 1
    set -l last $tokens[-1]
    test "$last" = --namespace
end

function __nxr_should_complete_categories
    set -l tokens (commandline -opc)
    set -e tokens[1]
    contains -- --category $tokens; or return 1
    set -l last $tokens[-1]
    test "$last" = --category
end

complete -c nxr -n __nxr_should_complete_apps -a "(__nxr_complete_apps)"
complete -c nxr -n __nxr_should_complete_tasks -a "(__nxr_complete_tasks)"
complete -c nxr -n __nxr_should_complete_packages -a "(__nxr_complete_packages)"
complete -c nxr -n __nxr_should_complete_checks -a "(__nxr_complete_checks)"
complete -c nxr -n __nxr_should_complete_shells -a "(__nxr_complete_shells)"
complete -c nxr -n __nxr_should_complete_namespaces -a "(__nxr_complete_namespaces)"
complete -c nxr -n __nxr_should_complete_categories -a "(__nxr_complete_categories)"
