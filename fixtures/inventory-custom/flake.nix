{
  description = "nxr fixture: custom inventory role";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { ... }:
    {
      customWorkflow = {
        plan = {
          type = "unknown";
          description = "CI plan workflow";
        };
      };
    };
}
