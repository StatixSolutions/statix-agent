{
  description = "Statix agent";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          statix-agent = pkgs.callPackage ./nix/package.nix {};
          statix-agent-capabilities = pkgs.writeShellApplication {
            name = "statix-agent-capabilities";
            runtimeInputs = [ pkgs.bash pkgs.coreutils ];
            text = builtins.readFile ./installers/common/statix-agent-capabilities.sh;
          };
          default = self.packages.${system}.statix-agent;
        });

      nixosModules.statix-agent = import ./nix/module.nix;
      nixosModules.default = self.nixosModules.statix-agent;
    };
}
