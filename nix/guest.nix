{ config, lib, pkgs, ... }:

let
  cfg = config.services.statix-guest;
in
{
  options.services.statix-guest = {
    enable = lib.mkEnableOption "Statix lightweight SSH guest";

    user = lib.mkOption {
      type = lib.types.str;
      default = "statix";
      description = "User used for control-plane console sessions.";
    };

    authorizedKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Ephemeral SSH public keys authorized for console access.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.openssh = {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "no";
        X11Forwarding = false;
        AllowTcpForwarding = "no";
      };
    };

    users.users.${cfg.user} = {
      isNormalUser = true;
      shell = pkgs.bashInteractive;
      openssh.authorizedKeys.keys = cfg.authorizedKeys;
    };

    networking.firewall.allowedTCPPorts = [ 22 ];
    environment.systemPackages = [ pkgs.bashInteractive pkgs.coreutils pkgs.procps ];
  };
}
