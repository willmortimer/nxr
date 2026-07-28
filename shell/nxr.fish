# Dynamic nxr completion for Fish.
#
# Appended to the clap-generated script by `nxr completion fish`. When discovery
# is slow or fails, `nxr __complete <target>` returns no candidates and reserved
# command completion from clap still works.

function __nxr_flake_root
    set -l dir $PWD
    while test -n "$dir" -a "$dir" != "/"
        if test -f "$dir/flake.nix"
            if type -q realpath
                realpath "$dir"
            else
                printf '%s\n' "$dir"
            end
            return 0
        end
        set dir (dirname "$dir")
    end
    return 1
end

function __nxr_daemon_socket
    if test -n "$NXR_DAEMON_SOCKET"
        printf '%s\n' "$NXR_DAEMON_SOCKET"
        return 0
    end
    switch "$NXR_DAEMON"
        case off 0 false no OFF FALSE NO
            return 1
    end
    if test -n "$XDG_RUNTIME_DIR"; and test -S "$XDG_RUNTIME_DIR/nxr/nxrd.sock"
        printf '%s\n' "$XDG_RUNTIME_DIR/nxr/nxrd.sock"
        return 0
    end
    set -l tmp (test -n "$TMPDIR"; and echo "$TMPDIR"; or echo "/tmp")
    set -l user (test -n "$USER"; and echo "$USER"; or echo "user")
    if test -S "$tmp/nxr-$user/nxrd.sock"
        printf '%s\n' "$tmp/nxr-$user/nxrd.sock"
        return 0
    end
    return 1
end

function __nxr_invoke
    set -l socket (__nxr_daemon_socket 2>/dev/null)
    if test -n "$socket"
        env NXR_DAEMON_SOCKET="$socket" command nxr $argv
    else
        command nxr $argv
    end
end

function __nxr_complete_apps
    __nxr_invoke __complete apps 2>/dev/null
end

function __nxr_complete_tasks
    __nxr_invoke __complete tasks 2>/dev/null
end

function __nxr_complete_packages
    __nxr_invoke __complete packages 2>/dev/null
end

function __nxr_complete_checks
    __nxr_invoke __complete checks 2>/dev/null
end

function __nxr_complete_shells
    __nxr_invoke __complete shells 2>/dev/null
end

function __nxr_complete_namespaces
    __nxr_invoke __complete namespaces 2>/dev/null
end

function __nxr_complete_categories
    __nxr_invoke __complete categories 2>/dev/null
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
