{
  description = "nxr fixture: task workingDirectory tokens and relative paths";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
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

      nxrDoc = {
        schema_version = 1;
        tasks = {
          invocation-pwd = {
            description = "Run pwd in the invocation directory";
            app = "pwd";
            workingDirectory = "invocation";
            dependsOn = [ ];
            hidden = false;
          };
          flake-root-pwd = {
            description = "Run pwd at the flake root";
            app = "pwd";
            workingDirectory = "flake-root";
            dependsOn = [ ];
            hidden = false;
          };
          subdir-pwd = {
            description = "Run pwd in a flake-root-relative subdirectory";
            app = "pwd";
            workingDirectory = "deep/down/here";
            dependsOn = [ ];
            hidden = false;
          };
          chain = {
            description = "Exercise dependency nodes with different working directories";
            dependsOn = [
              "invocation-pwd"
              "flake-root-pwd"
              "subdir-pwd"
            ];
            app = "pwd";
            workingDirectory = "invocation";
            hidden = false;
          };
        };
      };
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          pwd = mkApp pkgs "fixture-pwd" "Print the invocation working directory" ''
            pwd
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
