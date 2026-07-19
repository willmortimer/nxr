# Regenerable terminal demo for the README.
# From repo root:  ./docs/demo/record.sh
#
# Covers: list/run, inline flake#app, inspect, graph (mermaid + dot),
# task alias + dry-run, parallel -j with labeled output, --shell, watch.
#
# Requires: vhs (https://github.com/charmbracelet/vhs), ffmpeg, ttyd, nix
# Output: docs/demo/nxr.gif (committed for GitHub README rendering)

Output docs/demo/nxr.gif

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
Set TypingSpeed 22ms
Set Framerate 24
Set PlaybackSpeed 1.3

Hide
Type `export PATH="./target/debug:$PATH"`
Enter
Type `export NIX_CONFIG="warn-dirty = false"`
Enter
# Warm Nix eval/cache so parallel -j stays quiet mid-demo.
Type `for a in a left right join; do nix run ./fixtures/parallel-group#$a >/dev/null 2>&1; done`
Enter
Type `nix develop ./fixtures/named-dev-shells --command true >/dev/null 2>&1 || true`
Enter
Type "clear"
Enter
Show

# --- Discover & run ---
Type "nxr --flake fixtures/basic-apps list"
Enter
Sleep 1.5s

Type "nxr fixtures/basic-apps#hello"
Enter
Sleep 1.2s

Type "clear"
Enter
Sleep 350ms

# --- Inspect & graph ---
Type "nxr --flake fixtures/task-dag inspect"
Enter
Sleep 1.6s

Type "nxr --flake fixtures/task-dag graph ci --format mermaid"
Enter
Sleep 1.5s

Type "nxr --flake fixtures/task-dag graph ci --format dot"
Enter
Sleep 1.4s

Type "clear"
Enter
Sleep 350ms

# --- Task alias + dry-run ---
Type "nxr --flake fixtures/task-dag task check --dry-run"
Enter
Sleep 2s

Type "clear"
Enter
Sleep 350ms

# --- Parallel tasks ---
Type "nxr --flake fixtures/parallel-group --output live task join -j 2"
Enter
Sleep 5s

Type "clear"
Enter
Sleep 350ms

# --- Named shell ---
Type "nxr --flake fixtures/named-dev-shells --shell default shell-marker"
Enter
Sleep 4s

Type "clear"
Enter
Sleep 350ms

# --- Watch (start + interrupt) ---
Type "nxr --flake fixtures/basic-apps watch hello --debounce 300"
Enter
Sleep 3s
Ctrl+C
Sleep 1.5s

Sleep 700ms
