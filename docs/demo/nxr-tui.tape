# Regenerable TUI watch demo for the README / wiki.
# From repo root:  ./docs/demo/record.sh tui
#
# Requires: vhs, ffmpeg, ttyd, nix
# Output: docs/demo/nxr-tui.gif

Output docs/demo/nxr-tui.gif

Require nix

Set Shell "bash"
Set FontSize 15
Set FontFamily "JetBrains Mono, Menlo, monospace"
Set Width 980
Set Height 560
Set Padding 14
Set Theme "nord"
Set WindowBar Colorful
Set WindowBarSize 36
Set BorderRadius 8
Set TypingSpeed 18ms
Set Framerate 24
Set PlaybackSpeed 1.15

Hide
Type `export PATH="./target/debug:$PATH"`
Enter
Type `export NIX_CONFIG="warn-dirty = false"`
Enter
Type `nxr --flake fixtures/task-dag task check --dry-run >/dev/null 2>&1 || true`
Enter
Sleep 2s
Type "clear"
Enter
Show

Sleep 500ms
Type "nxr --flake fixtures/task-dag --output tui task check -j 2"
Enter
Sleep 8s
Type "q"
Sleep 1.5s
