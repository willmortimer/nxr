# Fail if crate `include_str!` / `include_bytes!` paths resolve outside the
# filtered workspace source (the packaging footgun that broke v2.6–v3.1.0).
{
  pkgs,
  src,
}:
pkgs.runCommand "workspace-src-includes"
  {
    nativeBuildInputs = [ pkgs.python3 ];
    inherit src;
  }
  ''
    python3 - <<'PY'
    import pathlib
    import re
    import sys

    src = pathlib.Path("${src}").resolve()
    crates = src / "crates"
    # Simple form only; concat! includes are rare and still covered by nix build.
    simple = re.compile(r"""include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)""")

    missing = []
    scanned = 0
    for path in crates.rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        for match in simple.finditer(text):
            scanned += 1
            rel = match.group(1)
            target = (path.parent / rel).resolve()
            try:
                target.relative_to(src)
            except ValueError:
                missing.append(f"{path.relative_to(src)}: {rel} escapes filtered src")
                continue
            if not target.is_file():
                missing.append(
                    f"{path.relative_to(src)}: {rel} -> missing in filtered src "
                    f"(add the directory to nix/lib/workspace-src.nix)"
                )

    if missing:
        print("workspace-src include_str/bytes check failed:", file=sys.stderr)
        for item in missing:
            print(f"  - {item}", file=sys.stderr)
        sys.exit(1)

    print(f"ok: {scanned} include_str/bytes paths present under filtered src")
    PY
    touch "$out"
  ''
