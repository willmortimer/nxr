{
  description = "Example: nxr mkApp helper and flake-parts apps module";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
    nixpkgsIntelDarwin.follows = "nxr/nixpkgsIntelDarwin";
    flake-parts.follows = "nxr/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nxr, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake.schemas = { };

      imports = [
        nxr.flakeModules.default
      ];

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];

      perSystem =
        { pkgs, ... }:
        {
          nxr.apps = {
            hello = {
              description = "Print a greeting via nxr.apps";
              script = ''
                echo "hello from examples/mk-app"
              '';
            };

            echo-args = {
              description = "Echo forwarded arguments";
              script = ''
                printf '%s\n' "$@"
              '';
            };

            # File-backed app (ADR-0170): store wrapper + optional live fast path.
            from-file = {
              description = "Run scripts/hello.sh via file-backed nxr.apps";
              file = "scripts/hello.sh";
              fastPath.enable = true;
            };
          };

          apps.greet = nxr.lib.mkApp {
            inherit pkgs;
            name = "example-greet";
            description = "Greet via nxr.lib.mkApp";
            text = ''
              echo "greet via lib.mkApp"
            '';
          };

          apps.hello-pkg = nxr.lib.mkPackageApp {
            inherit pkgs;
            package = pkgs.hello;
            bin = "hello";
            description = "Run hello from a nixpkgs package";
          };

          apps.script-alias = nxr.lib.mkScriptApp {
            inherit pkgs;
            name = "example-script-alias";
            description = "Same as mkApp via mkScriptApp alias";
            text = ''
              echo "greet via lib.mkScriptApp"
            '';
          };
        };
    };
}
