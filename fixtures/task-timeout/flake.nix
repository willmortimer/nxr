{
  description = "nxr fixture: per-task timeouts and parallel timeout peers";

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
          slow_a = {
            description = "Long-running peer A";
            app = "slow";
            dependsOn = [ ];
            hidden = false;
            timeout = "200ms";
            terminationGracePeriod = "100ms";
          };
          slow_b = {
            description = "Long-running peer B";
            app = "slow";
            dependsOn = [ ];
            hidden = false;
            timeout = "200ms";
            terminationGracePeriod = "100ms";
          };
          both = {
            description = "Join of timed-out peers";
            dependsOn = [
              "slow_a"
              "slow_b"
            ];
            app = "join";
            hidden = false;
          };
          hang = {
            description = "Single node that times out";
            app = "slow";
            dependsOn = [ ];
            hidden = false;
            timeout = "200ms";
            terminationGracePeriod = "100ms";
          };
          after_hang = {
            description = "Depends on hang (skipped/cancelled)";
            dependsOn = [ "hang" ];
            app = "join";
            hidden = false;
          };
          ok = {
            description = "Succeeds quickly";
            app = "ok";
            dependsOn = [ ];
            hidden = false;
          };
          gate = {
            description = "Fan-in of hang and ok";
            dependsOn = [
              "hang"
              "ok"
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
          slow = mkApp pkgs "fixture-slow" "Sleeps longer than fixture timeouts" ''
            echo "slow-start"
            sleep 30
            echo "slow-done"
          '';
          join = mkApp pkgs "fixture-join" "Join marker" ''
            echo "join"
          '';
          ok = mkApp pkgs "fixture-ok" "Succeeds immediately" ''
            echo "ok"
          '';
        }
      );

      nxr = forAllSystems (_: nxrDoc);
    };
}
