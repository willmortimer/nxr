# Session-local nxr dev-shell integration for Fish.
#
# Intended for interactive Fish sessions inside an nxr-enabled dev shell.
# The bash shellHook exports NXR_PACKAGE / XDG_DATA_DIRS; Fish loads vendor
# completions from those paths. This script applies dynamic app hooks when
# the base completion is present but not yet augmented.

status is-interactive; or exit 0
set -q NXR_SHELL_INTEGRATION; or exit 0
command -q nxr; or exit 0

if functions -q __nxr_complete_apps
    exit 0
end

if set -q NXR_PACKAGE
    and test -r "$NXR_PACKAGE/share/nxr/shell/nxr.fish"
    source "$NXR_PACKAGE/share/nxr/shell/nxr.fish"
end
