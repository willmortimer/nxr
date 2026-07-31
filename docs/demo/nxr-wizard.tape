# Regenerable deploy-wizard fixture demo.
# From repo root:  ./docs/demo/record.sh wizard
#
# Requires: vhs, ffmpeg, ttyd, nix
# Output: docs/demo/nxr-wizard.gif

Output docs/demo/nxr-wizard.gif

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
Set PlaybackSpeed 1.2

Hide
Type `export PATH="./target/debug:$PATH"`
Enter
Type `export NIX_CONFIG="warn-dirty = false"`
Enter
Type "clear"
Enter
Show

Type "NXR_FIXTURE_WIZARD_ENV=staging nxr --flake fixtures/deploy-wizard deploy-wizard"
Enter
Sleep 2s

Type "nxr --flake fixtures/deploy-wizard task deploy-staging --set version=1.2.3"
Enter
Sleep 3s

Type "clear"
Enter
Sleep 300ms

Type `NXR_FIXTURE_WIZARD_ENV=production NXR_FIXTURE_WIZARD_REASON=cutover-demo nxr --flake fixtures/deploy-wizard deploy-wizard`
Enter
Sleep 2s

Type `nxr --flake fixtures/deploy-wizard task deploy-prod --set reason=cutover-demo --set version=1.2.3`
Enter
Sleep 3s
Sleep 500ms
