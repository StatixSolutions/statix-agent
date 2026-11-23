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
          agent = pkgs.callPackage ./nix/package.nix {};
          capabilities = pkgs.writeShellApplication {
            name = "statix-agent-capabilities";
            runtimeInputs = [ pkgs.bash pkgs.coreutils ];
            text = builtins.readFile ./installers/common/statix-agent-capabilities.sh;
          };
          runtimeDependencies = with pkgs; [
            cacert
            cloud-utils
            curl
            dnsmasq
            docker
            gawk
            iproute2
            iptables
            lxc
            nginx
            openssh
            pciutils
            procps
            qemu-utils
            qemu_kvm
            shadow
            sudo
            systemd
            util-linux
            wget
            wireguard-tools
            xz
          ];
        in
        {
          statix-agent = agent;
          statix-agent-capabilities = capabilities;
          statix-agent-runtime = pkgs.symlinkJoin {
            name = "statix-agent-runtime";
            paths = [ agent capabilities ] ++ runtimeDependencies;
          };
          default = self.packages.${system}.statix-agent-runtime;
        });

      nixosModules.statix-agent = import ./nix/module.nix;
      nixosModules.statix-guest = import ./nix/guest.nix;
      nixosModules.default = self.nixosModules.statix-agent;
    };
}
