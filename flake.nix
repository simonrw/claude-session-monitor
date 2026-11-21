{
  description = "claude-session-monitor server: package, NixOS module and VM check";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    bun2nix = {
      url = "github:nix-community/bun2nix/2.1.2";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      bun2nix,
    }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system}.default = pkgs.callPackage ./nix/package.nix {
        bun2nix = bun2nix.packages.${system}.default;
      };

      nixosModules.default = import ./nix/module.nix self.packages.${system}.default;

      checks.${system}.default = pkgs.testers.runNixOSTest (
        import ./nix/test.nix self.nixosModules.default
      );
    };
}
