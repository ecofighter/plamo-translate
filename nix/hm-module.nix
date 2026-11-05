{ self }:

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.plamo-translate;
  tomlFormat = pkgs.formats.toml { };
  inherit (config.home) homeDirectory;

  dataDir = "${homeDirectory}/Library/Application Support/plamo-translate";
  logDir = "${homeDirectory}/Library/Logs/plamo-translate";

  # plist paths cannot use `~`, so the daemon, CLI and plist must agree on this
  # absolute path regardless of what the user put in settings.
  settings = cfg.settings // {
    socket_path = "${dataDir}/daemon.sock";
  };
  configFile = "plamo-translate/config.toml";
  # sun_path limit; the CLI's `daemon install` enforces the same bound.
  sunPathMax = 104;
in
{
  options.services.plamo-translate = {
    enable = lib.mkEnableOption "the plamo-translate daemon (socket-activated LaunchAgent)";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "plamo-translate.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The plamo-translate package; it must provide libexec/bin/plamo-translated.";
    };

    settings = lib.mkOption {
      inherit (tomlFormat) type;
      default = { };
      example = lib.literalExpression ''
        {
          idle_timeout_secs = 600;
          default_target = "auto";
        }
      '';
      description = ''
        Contents of {file}`$XDG_CONFIG_HOME/plamo-translate/config.toml`.
        `socket_path` is always set to the socket the LaunchAgent listens on.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        # tomlFormat accepts any TOML value; the daemon would crash-loop on a bad one.
        assertion =
          !(cfg.settings ? idle_timeout_secs)
          || (builtins.isInt cfg.settings.idle_timeout_secs && cfg.settings.idle_timeout_secs >= 0);
        message = "services.plamo-translate.settings.idle_timeout_secs must be a non-negative integer.";
      }
      {
        assertion = builtins.stringLength settings.socket_path < sunPathMax;
        message = "services.plamo-translate: socket path ${settings.socket_path} must be shorter than ${toString sunPathMax} bytes; use a shorter home directory.";
      }
    ];

    home.packages = [ cfg.package ];

    xdg.configFile.${configFile}.source = tomlFormat.generate "config.toml" settings;

    launchd.agents.plamo-translate = {
      enable = true;
      config = import ./launchd.nix { inherit lib; } {
        inherit homeDirectory settings;
        inherit (cfg) package;
        xdgConfigHome = config.xdg.configHome;
      };
      restartTriggers = [ config.xdg.configFile.${configFile}.source ];
    };

    # launchd creates the socket when the agent is bootstrapped, so the parent
    # directory must exist first; after writeBoundary so dry runs write nothing.
    home.activation.plamoTranslateDirs =
      lib.hm.dag.entryBetween [ "setupLaunchAgents" ] [ "writeBoundary" ]
        ''
          run mkdir -p -m 700 ${lib.escapeShellArg dataDir} ${lib.escapeShellArg logDir}
        '';
  };
}
