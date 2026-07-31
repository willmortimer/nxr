# Regenerable nxr ui browser demo.
# From repo root:  ./docs/demo/record.sh ui
#
# Requires: vhs, ffmpeg, ttyd, nix
# Output: docs/demo/nxr-ui.gif

Output docs/demo/nxr-ui.gif

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
Type `nxr --flake fixtures/workspace-scripts list >/dev/null 2>&1 || true`
Enter
Type "clear"
Enter
Show

Type "nxr --flake fixtures/workspace-scripts ui"
Enter
Sleep 2.5s
Type "j"
Sleep 500ms
Type "j"
Sleep 500ms
Tab
Sleep 1.2s
Type "j"
Sleep 600ms
Tab
Sleep 1.2s
Tab
Sleep 800ms
Type "q"
Sleep 700ms
