{
  description = "Starter flake for nxr consumers";

  inputs = {
    nxr.url = "github:willmortimer/nxr";
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
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      perSystem =
        { ... }:
        {
          nxr.apps = {
            hello = {
              description = "Print a greeting";
              script = ''
                echo "hello from nxr template"
              '';
            };
          };
        };
    };
}
