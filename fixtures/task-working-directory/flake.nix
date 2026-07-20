{
  description = "nxr fixture: task workingDirectory tokens and relative paths";

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
            pwd = {
              description = "Print the invocation working directory";
              script = ''
                pwd
              '';
            };
          };

          nxr.tasks = {
            invocation-pwd = {
              description = "Run pwd in the invocation directory";
              app = "pwd";
              workingDirectory = "invocation";
            };

            flake-root-pwd = {
              description = "Run pwd at the flake root";
              app = "pwd";
              workingDirectory = "flake-root";
            };

            subdir-pwd = {
              description = "Run pwd in a flake-root-relative subdirectory";
              app = "pwd";
              workingDirectory = "deep/down/here";
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
            };
          };
        };
    };
}
