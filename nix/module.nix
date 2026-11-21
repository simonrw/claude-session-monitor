package:
{ config, lib, ... }:
let
  cfg = config.services.claude-session-monitor;
  name = "claude-session-monitor";
in
{
  options.services.claude-session-monitor = {
    enable = lib.mkEnableOption "the claude-session-monitor server";

    port = lib.mkOption {
      type = lib.types.port;
      default = 7685;
      readOnly = true;
      description = "Localhost port the server listens on.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${name} = {
      isSystemUser = true;
      group = name;
    };
    users.groups.${name} = { };

    systemd.services.${name} = {
      description = "claude-session-monitor server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${lib.getExe package} --host 127.0.0.1 --port ${toString cfg.port} --db %S/${name}/${name}.db";
        User = name;
        Group = name;
        StateDirectory = name;
        Restart = "on-failure";
      };
    };
  };
}
