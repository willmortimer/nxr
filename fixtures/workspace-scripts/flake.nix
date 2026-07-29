{
  description = "nxr fixture: workspace scripts and file-backed app fast-path metadata";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      lib = nixpkgs.lib;
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      mkApp =
        pkgs: name: description: text:
        let
          drv = pkgs.writeShellApplication {
            inherit name text;
          };
        in
        {
          type = "app";
          program = "${drv}/bin/${name}";
          meta.description = description;
        };
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          app = mkApp pkgs;
          greetBody = builtins.readFile ./scripts/greet.sh;
          greetFile = pkgs.writeScript "greet-file-workspace-script" greetBody;
        in
        {
          hello = app "fixture-ws-hello-app" "Print a greeting from the flake app" ''
            echo "hello from workspace-scripts app"
          '';

          # Store-backed copy of scripts/greet.sh (nix run escape hatch).
          greet-file = app "fixture-ws-greet-file" "File-backed greeting" ''
            exec ${greetFile} "$@"
          '';
        }
      );

      # Listing metadata for ADR-0170 live fast path (no flake-parts / path:../..).
      nxr = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          schema_version = 1;
          tasks = { };
          apps = {
            greet-file = {
              workspace_path = "scripts/greet.sh";
              runtime_path = lib.makeBinPath [ pkgs.hello ];
              fastPath = {
                enable = true;
              };
            };
          };
          discoveryInputs = [ "scripts/greet.sh" ];
        }
      );
    };
}
