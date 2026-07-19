# Regenerable terminal demo for the README.
# From repo root:  ./docs/demo/record.sh
#
# Requires: vhs (https://github.com/charmbracelet/vhs), ffmpeg, ttyd
# Output: docs/demo/nxr.gif (committed for GitHub README rendering)

Output docs/demo/nxr.gif

Require nix

Set Shell "bash"
Set FontSize 16
Set FontFamily "JetBrains Mono, Menlo, monospace"
Set Width 980
Set Height 520
Set Padding 16
Set Theme "nord"
Set WindowBar Colorful
Set WindowBarSize 36
Set BorderRadius 8
Set TypingSpeed 28ms
Set Framerate 24
Set PlaybackSpeed 1.15

Hide
Type `export PATH="./target/debug:$PATH"`
Enter
Type "clear"
Enter
Show

Type "nxr --flake fixtures/basic-apps list"
Enter
Sleep 2s

Type "nxr --flake fixtures/basic-apps hello"
Enter
Sleep 1.2s

Type "nxr --flake fixtures/task-dag graph ci --format mermaid"
Enter
Sleep 2s

Type "nxr --flake fixtures/task-dag task ci --dry-run"
Enter
Sleep 2.5s

Sleep 1.5s
