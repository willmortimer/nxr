{
  description = "nxr fixture: diamond task DAG with parallelizable siblings";

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
          a = {
            description = "Diamond root";
            app = "a";
            dependsOn = [ ];
            hidden = false;
          };
          left = {
            description = "Left sibling";
            dependsOn = [ "a" ];
            app = "left";
            hidden = false;
          };
          right = {
            description = "Right sibling";
            dependsOn = [ "a" ];
            app = "right";
            hidden = false;
          };
          join = {
            description = "Diamond join / CI entry";
            dependsOn = [
              "left"
              "right"
            ];
            app = "join";
            category = "validation";
            hidden = false;
          };
          ok = {
            description = "Succeeds";
            app = "ok";
            dependsOn = [ ];
            hidden = false;
          };
          boom = {
            description = "Fails";
            app = "boom";
            dependsOn = [ ];
            hidden = false;
          };
          unrelated = {
            description = "Independent success";
            app = "unrelated";
            dependsOn = [ ];
            hidden = false;
          };
          gate = {
            description = "Fan-in of ok, boom, unrelated";
            dependsOn = [
              "ok"
              "boom"
              "unrelated"
            ];
            app = "join";
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
          a = mkApp pkgs "fixture-a" "Diamond root" ''
            echo "a"
          '';
          left = mkApp pkgs "fixture-left" "Left sibling" ''
            echo "left-start"
            sleep 1
            echo "left-done"
          '';
          right = mkApp pkgs "fixture-right" "Right sibling" ''
            echo "right-start"
            sleep 1
            echo "right-done"
          '';
          join = mkApp pkgs "fixture-join" "Diamond join" ''
            echo "join"
          '';
          ok = mkApp pkgs "fixture-ok" "Always succeeds" ''
            echo "ok"
          '';
          boom = mkApp pkgs "fixture-boom" "Always fails" ''
            echo "boom"
            exit 1
          '';
          unrelated = mkApp pkgs "fixture-unrelated" "Independent of boom" ''
            echo "unrelated"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
