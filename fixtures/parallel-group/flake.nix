{
  description = "nxr fixture: diamond task DAG with parallelizable siblings";

  inputs = {
    nxr.url = "path:../..";
    nixpkgs.follows = "nxr/nixpkgs";
    flake-parts.follows = "nxr/flake-parts";
  };

  outputs =
    inputs@{ flake-parts, nxr, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        nxr.flakeModules.default
      ];

      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        { ... }:
        {
          nxr.apps = {
            a = {
              description = "Diamond root";
              script = ''
                echo "a"
              '';
            };

            # Sleep so -j 2 can start both siblings before either exits.
            left = {
              description = "Left sibling";
              script = ''
                echo "left-start"
                sleep 1
                echo "left-done"
              '';
            };

            right = {
              description = "Right sibling";
              script = ''
                echo "right-start"
                sleep 1
                echo "right-done"
              '';
            };

            join = {
              description = "Diamond join";
              script = ''
                echo "join"
              '';
            };

            # Fail-fast / keep-going helpers (independent of diamond).
            ok = {
              description = "Always succeeds";
              script = ''
                echo "ok"
              '';
            };

            boom = {
              description = "Always fails";
              script = ''
                echo "boom"
                exit 1
              '';
            };

            unrelated = {
              description = "Independent of boom";
              script = ''
                echo "unrelated"
              '';
            };
          };

          # a → (left || right) → join
          nxr.tasks = {
            a = {
              description = "Diamond root";
              app = "a";
            };

            left = {
              description = "Left sibling";
              dependsOn = [ "a" ];
              app = "left";
            };

            right = {
              description = "Right sibling";
              dependsOn = [ "a" ];
              app = "right";
            };

            join = {
              description = "Diamond join / CI entry";
              dependsOn = [ "left" "right" ];
              app = "join";
              category = "validation";
            };

            # ok and unrelated are independent; boom fails.
            # Root `gate` depends on all three for keep-going vs fail-fast tests.
            ok = {
              description = "Succeeds";
              app = "ok";
            };

            boom = {
              description = "Fails";
              app = "boom";
            };

            unrelated = {
              description = "Independent success";
              app = "unrelated";
            };

            gate = {
              description = "Fan-in of ok, boom, unrelated";
              dependsOn = [ "ok" "boom" "unrelated" ];
              app = "join";
            };
          };
        };
    };
}
